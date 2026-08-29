"""Unit tests cover identifiers, lanes, and latency-sensitive GPU scheduling."""

import asyncio

import httpx

from workers.gpu_agent.main import (
    LANES,
    GpuScheduler,
    provider_proves_local_execution,
    sanitize_worker_id,
)


def test_worker_identifier_removes_network_and_path_punctuation() -> None:
    assert sanitize_worker_id("rtx host:C:\\private/path") == "rtxhostCprivatepath"


def test_worker_identifier_is_bounded() -> None:
    assert len(sanitize_worker_id("x" * 100)) == 64


def test_latency_sensitive_model_jobs_have_independent_lanes() -> None:
    capabilities = {lane.suffix: lane.capabilities for lane in LANES}
    assert capabilities["asr"] == ("asr",)
    assert capabilities["translate"] == ("translate",)
    assert capabilities["explain"] == ("explain", "asset_parse")


def test_provider_gate_allows_cpu_asr_but_requires_cuda_llm() -> None:
    assert provider_proves_local_execution("asr", "faster-whisper:small@cpu")
    assert provider_proves_local_execution("asr", "faster-whisper:small@cuda")
    assert provider_proves_local_execution("translate", "ollama:qwen2.5:3b-instruct@cuda")
    assert not provider_proves_local_execution("translate", "ollama:qwen2.5:3b-instruct@cpu")
    assert not provider_proves_local_execution("asr", "deterministic@cpu")


def test_asr_opens_a_serial_llm_start_window() -> None:
    async def scenario() -> tuple[list[str], int, int]:
        scheduler = GpuScheduler()
        order: list[str] = []

        async def llm_request() -> httpx.Response:
            order.append("llm")
            return httpx.Response(200)

        async def asr_request() -> httpx.Response:
            order.append("asr")
            return httpx.Response(201)

        llm = asyncio.create_task(scheduler.run_llm(llm_request))
        await asyncio.sleep(0.01)
        asr = await scheduler.run_asr(asr_request)
        llm_response = await asyncio.wait_for(llm, timeout=1)
        return order, asr.status_code, llm_response.status_code

    assert asyncio.run(scenario()) == (["asr", "llm"], 201, 200)


def test_cpu_asr_does_not_wait_for_an_active_gpu_llm() -> None:
    async def scenario() -> list[str]:
        scheduler = GpuScheduler(asr_uses_gpu=False)
        order: list[str] = []
        llm_started = asyncio.Event()
        release_llm = asyncio.Event()

        async def llm_request() -> httpx.Response:
            order.append("llm-start")
            llm_started.set()
            await release_llm.wait()
            order.append("llm-end")
            return httpx.Response(200)

        async def asr_request() -> httpx.Response:
            order.append("asr")
            return httpx.Response(201)

        llm = asyncio.create_task(scheduler.run_llm(llm_request))
        await asyncio.wait_for(llm_started.wait(), timeout=1)
        response = await asyncio.wait_for(scheduler.run_asr(asr_request), timeout=1)
        assert response.status_code == 201
        release_llm.set()
        await asyncio.wait_for(llm, timeout=1)
        return order

    assert asyncio.run(scenario()) == ["llm-start", "asr", "llm-end"]
