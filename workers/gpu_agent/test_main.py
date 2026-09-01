"""Unit tests cover identifiers, lanes, and latency-sensitive GPU scheduling."""

import asyncio

import httpx

from workers.gpu_agent.main import (
    LANES,
    GpuScheduler,
    model_worker_remains_available,
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
    assert capabilities["explain"] == ("explain", "summarize", "asset_parse")


def test_provider_gate_allows_cpu_asr_but_requires_cuda_llm() -> None:
    assert provider_proves_local_execution("asr", "faster-whisper:small@cpu")
    assert provider_proves_local_execution("asr", "faster-whisper:small@cuda")
    assert provider_proves_local_execution("translate", "ollama:qwen2.5:3b-instruct@cuda")
    assert not provider_proves_local_execution("translate", "ollama:qwen2.5:3b-instruct@cpu")
    assert provider_proves_local_execution("summarize", "ollama:qwen2.5:14b-instruct@cuda")
    assert not provider_proves_local_execution("asr", "deterministic@cpu")


def test_background_model_swap_keeps_worker_health_available() -> None:
    assert model_worker_remains_available(
        {
            "status": "ok",
            "asr_available": True,
            "ollama_available": True,
            "ollama_gpu_resident": False,
        }
    )
    assert not model_worker_remains_available(
        {"status": "ok", "asr_available": True, "ollama_available": False}
    )


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


def test_cuda_asr_overlaps_an_active_gpu_llm() -> None:
    async def scenario() -> list[str]:
        scheduler = GpuScheduler(asr_uses_gpu=True)
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


def test_exclusive_model_waits_for_cuda_asr_and_blocks_new_asr() -> None:
    async def scenario() -> list[str]:
        scheduler = GpuScheduler(asr_uses_gpu=True)
        order: list[str] = []
        asr_started = asyncio.Event()
        release_asr = asyncio.Event()
        exclusive_started = asyncio.Event()

        async def asr_request() -> httpx.Response:
            order.append("asr-start")
            asr_started.set()
            await release_asr.wait()
            order.append("asr-end")
            return httpx.Response(201)

        async def exclusive_request() -> httpx.Response:
            order.append("exclusive")
            exclusive_started.set()
            return httpx.Response(200)

        first = asyncio.create_task(scheduler.run_asr(asr_request))
        await asyncio.wait_for(asr_started.wait(), timeout=1)
        second = asyncio.create_task(scheduler.run_exclusive(exclusive_request))
        await asyncio.sleep(0.01)
        assert not exclusive_started.is_set()
        release_asr.set()
        await asyncio.wait_for(asyncio.gather(first, second), timeout=1)
        return order

    assert asyncio.run(scenario()) == ["asr-start", "asr-end", "exclusive"]


def test_exclusive_model_waits_for_asr_overlapping_a_realtime_llm() -> None:
    async def scenario() -> list[str]:
        scheduler = GpuScheduler(asr_uses_gpu=True)
        order: list[str] = []
        llm_started = asyncio.Event()
        release_llm = asyncio.Event()
        asr_started = asyncio.Event()
        release_asr = asyncio.Event()
        exclusive_started = asyncio.Event()

        async def llm_request() -> httpx.Response:
            order.append("llm-start")
            llm_started.set()
            await release_llm.wait()
            order.append("llm-end")
            return httpx.Response(200)

        async def asr_request() -> httpx.Response:
            order.append("asr-start")
            asr_started.set()
            await release_asr.wait()
            order.append("asr-end")
            return httpx.Response(201)

        async def exclusive_request() -> httpx.Response:
            order.append("exclusive")
            exclusive_started.set()
            return httpx.Response(202)

        llm = asyncio.create_task(scheduler.run_llm(llm_request))
        await asyncio.wait_for(llm_started.wait(), timeout=1)
        asr = asyncio.create_task(scheduler.run_asr(asr_request))
        await asyncio.wait_for(asr_started.wait(), timeout=1)
        release_llm.set()
        await asyncio.wait_for(llm, timeout=1)
        exclusive = asyncio.create_task(scheduler.run_exclusive(exclusive_request))
        await asyncio.sleep(0.01)
        assert not exclusive_started.is_set()
        release_asr.set()
        await asyncio.wait_for(asyncio.gather(asr, exclusive), timeout=1)
        return order

    assert asyncio.run(scenario()) == [
        "llm-start",
        "asr-start",
        "llm-end",
        "asr-end",
        "exclusive",
    ]


def test_waiting_translation_runs_before_the_next_background_model_job() -> None:
    async def scenario() -> list[str]:
        scheduler = GpuScheduler(asr_uses_gpu=True)
        order: list[str] = []
        first_started = asyncio.Event()
        release_first = asyncio.Event()

        async def first_background() -> httpx.Response:
            order.append("background-1-start")
            first_started.set()
            await release_first.wait()
            order.append("background-1-end")
            return httpx.Response(200)

        async def second_background() -> httpx.Response:
            order.append("background-2")
            return httpx.Response(200)

        async def translation() -> httpx.Response:
            order.append("translation")
            return httpx.Response(200)

        first = asyncio.create_task(scheduler.run_llm(first_background))
        await asyncio.wait_for(first_started.wait(), timeout=1)
        second = asyncio.create_task(scheduler.run_llm(second_background))
        await asyncio.sleep(0)
        translated = asyncio.create_task(scheduler.run_translation(translation))
        await asyncio.sleep(0)
        release_first.set()
        await asyncio.wait_for(asyncio.gather(first, second, translated), timeout=1)
        return order

    assert asyncio.run(scenario()) == [
        "background-1-start",
        "background-1-end",
        "translation",
        "background-2",
    ]


def test_translation_gets_a_bounded_slot_when_asr_queue_is_continuously_ready() -> None:
    async def scenario() -> list[str]:
        scheduler = GpuScheduler(asr_uses_gpu=True)
        order: list[str] = []
        release_burst = asyncio.Event()
        release_first = asyncio.Event()
        release_second = asyncio.Event()

        async def burst_asr() -> httpx.Response:
            order.append("asr")
            await release_burst.wait()
            return httpx.Response(200)

        async def translation() -> httpx.Response:
            order.append("translation")
            return httpx.Response(200)

        # Complete a bounded burst before creating a continuously ready queue.
        for _ in range(8):
            release_burst.set()
            await asyncio.wait_for(scheduler.run_asr(burst_asr), timeout=1)
            release_burst.clear()

        async def first_asr() -> httpx.Response:
            order.append("asr")
            await release_first.wait()
            return httpx.Response(200)

        async def second_asr() -> httpx.Response:
            order.append("asr")
            await release_second.wait()
            return httpx.Response(200)

        first = asyncio.create_task(scheduler.run_asr(first_asr))
        await asyncio.sleep(0)
        translated = asyncio.create_task(scheduler.run_translation(translation))
        await asyncio.sleep(0)
        waiting_asr = asyncio.create_task(scheduler.run_asr(second_asr))
        await asyncio.sleep(0)
        release_first.set()
        await asyncio.wait_for(translated, timeout=1)
        release_second.set()
        await asyncio.wait_for(first, timeout=1)
        await asyncio.wait_for(waiting_asr, timeout=1)
        return order

    result = asyncio.run(scenario())
    assert result[-2:] == ["translation", "asr"]
