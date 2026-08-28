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
from dataclasses import dataclass
from typing import Any, cast

import httpx

GATEWAY_URL = os.getenv("AIALRA_GPU_GATEWAY_URL", "http://127.0.0.1:8787").rstrip("/")
MODEL_WORKER_URL = os.getenv("AIALRA_MODEL_WORKER_URL", "http://127.0.0.1:8790").rstrip("/")
WORKER_TOKEN = os.getenv("AIALRA_WORKER_TOKEN", "")
WORKER_ID = os.getenv("AIALRA_GPU_WORKER_ID", "rtx4080")
HEARTBEAT_SECONDS = 10
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
    Lane("llm", ("translate", "explain", "asset_parse")),
)


class RetryableJobError(RuntimeError):
    """A local provider or private-network interruption should return the job to the queue."""


def sanitize_worker_id(value: str) -> str:
    allowed = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-"
    return "".join(character for character in value if character in allowed)[:64] or "rtx-worker"


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


async def verify_model_worker(client: httpx.AsyncClient) -> dict[str, Any]:
    response = await client.get(f"{MODEL_WORKER_URL}/health", timeout=10)
    response.raise_for_status()
    health = cast(dict[str, Any], response.json())
    if not health.get("asr_available") or not health.get("ollama_available"):
        raise RuntimeError("local ASR and Ollama providers must both be ready")
    for key in ("asr_provider", "llm_provider"):
        if not str(health.get(key, "")).endswith("@cuda"):
            raise RuntimeError(f"{key} did not prove CUDA execution")
    return health


async def heartbeat_loop(
    gateway: httpx.AsyncClient,
    lane: Lane,
    metadata: dict[str, Any],
    active: dict[str, str | None],
) -> None:
    while True:
        try:
            await gateway.post(
                f"{GATEWAY_URL}/internal/v1/workers/heartbeat",
                json={
                    "worker_id": lane.worker_id,
                    "capabilities": list(lane.capabilities),
                    "model_metadata": metadata,
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
        response = await model.post(
            f"{MODEL_WORKER_URL}/v1/asr/transcribe", json=model_input, timeout=180
        )
    elif job_type == "translate":
        response = await model.post(
            f"{MODEL_WORKER_URL}/v1/translate", json=model_input, timeout=180
        )
    elif job_type == "explain":
        response = await model.post(f"{MODEL_WORKER_URL}/v1/explain", json=model_input, timeout=300)
    elif job_type == "asset_parse":
        binary = await gateway.get(f"{GATEWAY_URL}/internal/v1/jobs/{job['id']}/input", timeout=60)
        binary.raise_for_status()
        response = await model.post(
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
    else:
        raise RuntimeError("unsupported model job type")
    if response.status_code >= 500:
        raise RetryableJobError(f"provider unavailable for {job_type}")
    response.raise_for_status()
    result = cast(dict[str, Any], response.json())
    provider = str(result.get("provider", ""))
    if job_type in {"asr", "translate", "explain"} and not provider.endswith("@cuda"):
        raise RetryableJobError("model result did not prove CUDA execution")
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
                result = await execute_job(gateway, model, job)
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
        tasks = []
        for lane in LANES:
            tasks.append(asyncio.create_task(heartbeat_loop(gateway, lane, metadata, active)))
            tasks.append(asyncio.create_task(lane_loop(gateway, model, lane, active)))
        await asyncio.gather(*tasks)


if __name__ == "__main__":
    asyncio.run(run())
