"""Lease persistent jobs from the VPS and execute them on the local RTX GPU."""

from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import os
import platform
import random
import subprocess
import time
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import Any, cast

import httpx

GATEWAY_URL = os.getenv("AIALRA_GPU_GATEWAY_URL", "http://127.0.0.1:8787").rstrip("/")
MODEL_WORKER_URL = os.getenv("AIALRA_MODEL_WORKER_URL", "http://127.0.0.1:8790").rstrip("/")
WORKER_TOKEN = os.getenv("AIALRA_WORKER_TOKEN", "")
WORKER_ID = os.getenv("AIALRA_GPU_WORKER_ID", "rtx4080")
HEARTBEAT_SECONDS = 5
RENEW_SECONDS = 20
BACKOFF_SECONDS = (1, 2, 4, 8, 16, 30)


@dataclass(frozen=True)
class Lane:
    """One independent scheduler lane keeps ASR ahead of longer language-model work."""

    suffix: str
    capabilities: tuple[str, ...]

    @property
    def worker_id(self) -> str:
        return sanitize_worker_id(f"{WORKER_ID}-{self.suffix}")


LANES = (
    Lane("asr", ("asr",)),
    # Translation must never wait behind a long explanation request.  Separate
    # leases also make the server-side pickup metric reflect actual worker
    # availability instead of the duration of the previous LLM generation.
    Lane("translate", ("translate",)),
    Lane("explain", ("explain", "summarize", "asset_parse")),
)


class RetryableJobError(RuntimeError):
    """A local provider or private-network interruption should return the job to the queue."""


class GpuScheduler:
    """Keep ASR responsive while serializing the longer Ollama requests."""

    def __init__(self, *, asr_uses_gpu: bool = True) -> None:
        self._asr_uses_gpu = asr_uses_gpu
        self._llm_lock = asyncio.Lock()
        self._condition = asyncio.Condition()
        self._active_kind: str | None = None
        self._asr_waiters = 0
        self._translation_waiters = 0
        self._last_asr_completed: float | None = None
        self._llm_start_window_seconds = 2.0

    async def run_asr(
        self, request: Callable[[], Awaitable[httpx.Response]]
    ) -> httpx.Response:
        if not self._asr_uses_gpu:
            return await request()
        concurrent_with_llm = False
        async with self._condition:
            self._asr_waiters += 1
            try:
                if self._active_kind == "llm":
                    # Ollama generation cannot be preempted.  The measured RTX 4080
                    # memory envelope leaves room for small ASR, so run it alongside
                    # the active LLM instead of adding up to nine seconds of queueing.
                    concurrent_with_llm = True
                else:
                    await self._condition.wait_for(lambda: self._active_kind is None)
                    self._active_kind = "asr"
            finally:
                self._asr_waiters -= 1
        try:
            return await request()
        finally:
            async with self._condition:
                if not concurrent_with_llm:
                    self._active_kind = None
                self._last_asr_completed = time.monotonic()
                self._condition.notify_all()

    async def run_llm(
        self, request: Callable[[], Awaitable[httpx.Response]]
    ) -> httpx.Response:
        return await self._run_llm(request, translation_priority=False)

    async def run_translation(
        self, request: Callable[[], Awaitable[httpx.Response]]
    ) -> httpx.Response:
        return await self._run_llm(request, translation_priority=True)

    async def _run_llm(
        self,
        request: Callable[[], Awaitable[httpx.Response]],
        *,
        translation_priority: bool,
    ) -> httpx.Response:
        if not self._asr_uses_gpu:
            async with self._llm_lock:
                return await request()
        async with self._condition:
            if translation_priority:
                self._translation_waiters += 1
            if self._last_asr_completed is None:
                try:
                    await asyncio.wait_for(self._condition.wait(), timeout=0.05)
                except TimeoutError:
                    pass
            try:
                while True:
                    no_higher_priority_work = self._asr_waiters == 0 and (
                        translation_priority or self._translation_waiters == 0
                    )
                    if self._active_kind is None and no_higher_priority_work:
                        break
                    await self._condition.wait()
                self._active_kind = "llm"
            finally:
                if translation_priority:
                    self._translation_waiters -= 1
        try:
            return await request()
        finally:
            async with self._condition:
                self._active_kind = None
                self._condition.notify_all()


def sanitize_worker_id(value: str) -> str:
    allowed = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-"
    return "".join(character for character in value if character in allowed)[:64] or "rtx-worker"


def provider_proves_local_execution(job_type: str, provider: str) -> bool:
    """ASR may use the local CPU while every language-model result must prove CUDA."""

    if job_type == "asr":
        return provider.startswith("faster-whisper:") and provider.endswith(("@cpu", "@cuda"))
    if job_type in {"translate", "explain", "summarize"}:
        return provider.startswith("ollama:") and provider.endswith("@cuda")
    return True


def authorization_headers() -> dict[str, str]:
    if not WORKER_TOKEN:
        raise RuntimeError("AIALRA_WORKER_TOKEN is required")
    return {"Authorization": f"Bearer {WORKER_TOKEN}"}


def cuda_metadata() -> dict[str, Any]:
    """Query bounded GPU metadata without recording local paths or device identifiers."""

    command = [
        "nvidia-smi",
        "--query-gpu=name,memory.total,driver_version",
        "--format=csv,noheader,nounits",
    ]
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=5, check=True)
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError("CUDA device probe failed") from error
    first = result.stdout.strip().splitlines()[0].split(",")
    if len(first) < 3 or "RTX" not in first[0]:
        raise RuntimeError("an NVIDIA RTX CUDA device is required")
    return {
        "gpu_family": first[0].strip(),
        "memory_mib": int(first[1].strip()),
        "driver": first[2].strip(),
        "platform": platform.system(),
    }


def cuda_telemetry() -> dict[str, Any]:
    """Return one privacy-safe live sample without process IDs, UUIDs, or host names."""

    command = [
        "nvidia-smi",
        "--query-gpu=name,utilization.gpu,utilization.memory,memory.used,memory.total,power.draw,power.limit,temperature.gpu",
        "--format=csv,noheader,nounits",
    ]
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=5, check=True)
        values = [value.strip() for value in result.stdout.strip().splitlines()[0].split(",")]
        if len(values) != 8:
            return {}
        return {
            "name": values[0],
            "utilization_percent": float(values[1]),
            "memory_utilization_percent": float(values[2]),
            "memory_used_mib": float(values[3]),
            "memory_total_mib": float(values[4]),
            "power_w": round(float(values[5]), 1),
            "power_limit_w": round(float(values[6]), 1),
            "temperature_c": float(values[7]),
            "sampled_at_unix_ms": int(time.time() * 1_000),
        }
    except (OSError, subprocess.SubprocessError, ValueError, IndexError):
        return {}


async def verify_model_worker(client: httpx.AsyncClient) -> dict[str, Any]:
    response = await client.get(f"{MODEL_WORKER_URL}/health", timeout=10)
    response.raise_for_status()
    health = cast(dict[str, Any], response.json())
    if not health.get("asr_available") or not health.get("ollama_available"):
        raise RuntimeError("local ASR and Ollama providers must both be ready")
    if not health.get("ollama_gpu_resident"):
        raise RuntimeError("configured Ollama model is not resident on the local GPU")
    asr_provider = str(health.get("asr_provider", ""))
    if not asr_provider.endswith(("@cpu", "@cuda")):
        raise RuntimeError("asr_provider did not prove local execution")
    if not str(health.get("llm_provider", "")).endswith("@cuda"):
        raise RuntimeError("llm_provider did not prove CUDA execution")
    return health


def model_worker_remains_available(health: dict[str, Any]) -> bool:
    """A one-shot background model may replace the resident translation model temporarily."""

    return bool(
        health.get("status") == "ok"
        and health.get("asr_available")
        and health.get("ollama_available")
    )


async def model_health_loop(client: httpx.AsyncClient) -> None:
    """Exit only when the worker process is unreachable, not while a local model is busy."""

    failures = 0
    while True:
        await asyncio.sleep(10)
        try:
            response = await client.get(f"{MODEL_WORKER_URL}/health", timeout=10)
            response.raise_for_status()
            health = response.json()
            if not model_worker_remains_available(health):
                raise RuntimeError("local model worker health status is not ok")
            failures = 0
        except (httpx.HTTPError, RuntimeError, KeyError, ValueError) as error:
            failures += 1
            if failures >= 3:
                raise RuntimeError(
                    "local model worker remained unavailable for 30 seconds"
                ) from error


async def heartbeat_loop(
    gateway: httpx.AsyncClient,
    lane: Lane,
    metadata: dict[str, Any],
    active: dict[str, str | None],
) -> None:
    while True:
        try:
            live_metadata = {
                **metadata,
                "gpu": cuda_telemetry(),
                "active_job_type": lane.suffix if active.get(lane.suffix) else None,
            }
            await gateway.post(
                f"{GATEWAY_URL}/internal/v1/workers/heartbeat",
                json={
                    "worker_id": lane.worker_id,
                    "capabilities": list(lane.capabilities),
                    "model_metadata": live_metadata,
                    "active_job_id": active.get(lane.suffix),
                },
                timeout=10,
            )
        except httpx.HTTPError:
            pass
        await asyncio.sleep(HEARTBEAT_SECONDS)


async def renew_loop(gateway: httpx.AsyncClient, lane: Lane, job_id: str) -> None:
    while True:
        await asyncio.sleep(RENEW_SECONDS)
        response = await gateway.post(
            f"{GATEWAY_URL}/internal/v1/jobs/{job_id}/renew",
            json={"worker_id": lane.worker_id},
            timeout=10,
        )
        if response.status_code == 409:
            raise RetryableJobError("job lease was lost")
        response.raise_for_status()


async def execute_job(
    gateway: httpx.AsyncClient,
    model: httpx.AsyncClient,
    job: dict[str, Any],
    scheduler: GpuScheduler,
) -> dict[str, Any]:
    job_type = str(job["job_type"])
    model_input = dict(job["input"])
    if job_type == "asr":
        binary = await gateway.get(f"{GATEWAY_URL}/internal/v1/jobs/{job['id']}/input", timeout=30)
        binary.raise_for_status()
        expected = binary.headers.get("x-aialra-content-sha256", "")
        if expected and hashlib.sha256(binary.content).hexdigest() != expected:
            raise RetryableJobError("input integrity check failed")
        model_input["pcm_s16le_base64"] = base64.b64encode(binary.content).decode("ascii")
        response = await scheduler.run_asr(
            lambda: model.post(
                f"{MODEL_WORKER_URL}/v1/asr/transcribe", json=model_input, timeout=180
            )
        )
    elif job_type == "translate":
        response = await scheduler.run_translation(
            lambda: model.post(
                f"{MODEL_WORKER_URL}/v1/translate", json=model_input, timeout=180
            )
        )
    elif job_type == "explain":
        response = await scheduler.run_llm(
            lambda: model.post(
                f"{MODEL_WORKER_URL}/v1/explain", json=model_input, timeout=300
            )
        )
    elif job_type == "summarize":
        response = await scheduler.run_llm(
            lambda: model.post(
                f"{MODEL_WORKER_URL}/v1/summarize", json=model_input, timeout=600
            )
        )
    elif job_type == "asset_parse":
        binary = await gateway.get(f"{GATEWAY_URL}/internal/v1/jobs/{job['id']}/input", timeout=60)
        binary.raise_for_status()

        async def request_asset_parse() -> httpx.Response:
            return await model.post(
                f"{MODEL_WORKER_URL}/v1/assets/parse",
                files={
                    "file": (
                        model_input.get("file_name", "asset.bin"),
                        binary.content,
                        model_input.get("media_type", "application/octet-stream"),
                    )
                },
                timeout=300,
            )

        if str(model_input.get("media_type", "")).startswith("image/"):
            response = await scheduler.run_llm(request_asset_parse)
        else:
            response = await request_asset_parse()
    else:
        raise RuntimeError("unsupported model job type")
    if response.status_code >= 500:
        raise RetryableJobError(f"provider unavailable for {job_type}")
    response.raise_for_status()
    result = cast(dict[str, Any], response.json())
    provider = str(result.get("provider") or result.get("parser") or "")
    is_image_parse = job_type == "asset_parse" and str(
        model_input.get("media_type", "")
    ).startswith("image/")
    if is_image_parse and not (
        provider.startswith("ollama:") and provider.endswith("@cuda")
    ):
        raise RetryableJobError("image parser did not prove local CUDA execution")
    if not provider_proves_local_execution(job_type, provider):
        raise RetryableJobError("model result did not prove allowed local execution")
    return result


async def complete_job(
    gateway: httpx.AsyncClient,
    lane: Lane,
    job_id: str,
    result: dict[str, Any],
    elapsed_ms: int,
) -> None:
    response = await gateway.post(
        f"{GATEWAY_URL}/internal/v1/jobs/{job_id}/complete",
        json={"worker_id": lane.worker_id, "result": result, "elapsed_ms": elapsed_ms},
        timeout=30,
    )
    response.raise_for_status()


async def fail_job(
    gateway: httpx.AsyncClient,
    lane: Lane,
    job_id: str,
    error_kind: str,
    retryable: bool,
) -> None:
    response = await gateway.post(
        f"{GATEWAY_URL}/internal/v1/jobs/{job_id}/fail",
        json={
            "worker_id": lane.worker_id,
            "error_kind": error_kind,
            "retryable": retryable,
            "retry_after_seconds": 4 if retryable else 0,
        },
        timeout=30,
    )
    if response.status_code not in {200, 409}:
        response.raise_for_status()


async def lane_loop(
    gateway: httpx.AsyncClient,
    model: httpx.AsyncClient,
    lane: Lane,
    active: dict[str, str | None],
    scheduler: GpuScheduler,
) -> None:
    failures = 0
    while True:
        try:
            response = await gateway.post(
                f"{GATEWAY_URL}/internal/v1/jobs/lease",
                json={"worker_id": lane.worker_id, "capabilities": list(lane.capabilities)},
                timeout=30,
            )
            if response.status_code == 204:
                failures = 0
                continue
            response.raise_for_status()
            job = response.json()["job"]
            job_id = str(job["id"])
            active[lane.suffix] = job_id
            started = time.monotonic()
            renew = asyncio.create_task(renew_loop(gateway, lane, job_id))
            try:
                result = await execute_job(gateway, model, job, scheduler)
                await complete_job(
                    gateway, lane, job_id, result, int((time.monotonic() - started) * 1_000)
                )
            except RetryableJobError:
                await fail_job(gateway, lane, job_id, "provider_unavailable", True)
            except (httpx.HTTPError, KeyError, ValueError, json.JSONDecodeError):
                await fail_job(gateway, lane, job_id, "provider_response_invalid", True)
            finally:
                renew.cancel()
                await asyncio.gather(renew, return_exceptions=True)
                active[lane.suffix] = None
            failures = 0
        except (httpx.HTTPError, RuntimeError):
            delay = BACKOFF_SECONDS[min(failures, len(BACKOFF_SECONDS) - 1)]
            failures += 1
            await asyncio.sleep(delay + random.random() * 0.25)


async def run() -> None:
    if not WORKER_TOKEN:
        raise RuntimeError("AIALRA_WORKER_TOKEN is required")
    headers = authorization_headers()
    async with httpx.AsyncClient(headers=headers) as gateway, httpx.AsyncClient() as model:
        health = await verify_model_worker(model)
        metadata = {**cuda_metadata(), **health}
        active: dict[str, str | None] = {lane.suffix: None for lane in LANES}
        scheduler = GpuScheduler(
            asr_uses_gpu=str(health.get("asr_provider", "")).endswith("@cuda")
        )
        tasks = [asyncio.create_task(model_health_loop(model))]
        for lane in LANES:
            tasks.append(asyncio.create_task(heartbeat_loop(gateway, lane, metadata, active)))
            tasks.append(
                asyncio.create_task(lane_loop(gateway, model, lane, active, scheduler))
            )
        await asyncio.gather(*tasks)


if __name__ == "__main__":
    asyncio.run(run())
