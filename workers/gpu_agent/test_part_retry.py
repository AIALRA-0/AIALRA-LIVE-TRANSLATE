"""A transient local model refusal must not restart a completed teaching job."""

import asyncio
import json

import httpx

from workers.gpu_agent.main import (
    GpuScheduler,
    execute_job,
    retryable_teaching_part_response,
    teaching_part_failure_kind,
)


def test_one_local_503_part_response_is_retryable_without_exposing_its_body() -> None:
    assert retryable_teaching_part_response(httpx.Response(
        503, json={"detail": "model_worker_busy"},
    ))
    assert retryable_teaching_part_response(httpx.Response(
        503, json={"detail": "teaching_part_contract_invalid"},
    ))
    assert retryable_teaching_part_response(httpx.Response(
        503, json={"detail": "model_execution_failed"},
    ))
    assert retryable_teaching_part_response(httpx.Response(
        503, content=b"unclassified transient response",
    ))
    assert not retryable_teaching_part_response(httpx.Response(
        429, json={"detail": "model_worker_busy"},
    ))
    assert teaching_part_failure_kind(httpx.Response(
        503, json={"detail": "teaching_part_contract_invalid"},
    )) == "teaching_part_contract_invalid"
    assert teaching_part_failure_kind(httpx.Response(
        503, json={"detail": "private model output"},
    )) == "model_http_error"


def test_explanation_retries_one_busy_part_in_the_same_job() -> None:
    async def scenario() -> tuple[dict[str, object], list[str]]:
        phases: list[str] = []

        def infer(request: httpx.Request) -> httpx.Response:
            phase = json.loads(request.content)["phase"]
            phases.append(phase)
            if len(phases) == 1:
                return httpx.Response(503, json={"detail": "model_worker_busy"})
            return httpx.Response(200, json={
                "prose": "锁存器保持数据。", "original_terms": [],
                "provider": "ollama:synthetic@cuda",
            })

        async with httpx.AsyncClient(transport=httpx.MockTransport(infer)) as model, \
                   httpx.AsyncClient() as gateway:
            result = await execute_job(gateway, model, {
                "id": "synthetic-part-retry", "idempotency_key": "synthetic-part-retry",
                "job_type": "explain", "input": {
                    "segments": [{"id": "first", "text": "A latch retains a value."}],
                    "target_language": "zh-CN", "asset_pages": [],
                },
            }, GpuScheduler(asr_uses_gpu=False), "worker")
        return result, phases

    result, phases = asyncio.run(scenario())
    assert phases == ["prose", "prose"]
    assert result["evidence_segment_ids"] == ["first"]
