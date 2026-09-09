"""A course summary preserves all source groups, including its middle."""

from typing import Any

import pytest

from workers.gpu_agent.course_summary import COMPILED_PROVIDER, compile_course


@pytest.mark.asyncio
async def test_complete_summary_reuses_groups_and_backfills_only_uncovered_sources() -> None:
    calls: list[str] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        calls.append(body["text"])
        return {"prose": body["text"], "original_terms": [], "provider": "ollama:test@cuda"}

    segments = [{"id": f"p{i}", "text": f"Synthetic paragraph {i}"} for i in range(100)]
    group = {"coverage_contract": "all_sources_v1", "result": {
        "paragraph_summary": "Verified first group", "terms": [],
        "evidence_segment_ids": ["p0", "p1"], "asset_page_ids": [],
    }}
    result = await compile_course({"segments": segments, "asset_pages": [
        {"id": "page", "text": "Synthetic material"},
    ], "complete_groups": [group], "target_language": "zh-CN"}, call)
    assert 1 < len(calls) < 99
    assert "\n\n".join(calls) == "\n\n".join(
        [item["text"] for item in segments[2:]] + ["Synthetic material"],
    )
    assert "Synthetic paragraph 49" in result["overview"]
    assert "Synthetic paragraph 99" in result["overview"]
    assert result["evidence_segment_ids"] == [f"p{i}" for i in range(100)]
    assert result["asset_page_ids"] == ["page"]
    assert result["provider"] == COMPILED_PROVIDER


@pytest.mark.asyncio
async def test_summary_rejects_foreign_and_noncontiguous_reused_groups() -> None:
    calls: list[str] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        calls.append(body["text"])
        return {"prose": "Known source", "original_terms": [], "provider": "ollama:test@cuda"}

    groups = [{"coverage_contract": "all_sources_v1", "result": {
        "paragraph_summary": "Must not be reused", "terms": [], "asset_page_ids": [],
        "evidence_segment_ids": ids,
    }} for ids in [["foreign"], ["p0", "p2"], ["p2", "p1"]]]
    result = await compile_course({
        "segments": [{"id": f"p{i}", "text": f"source {i}"} for i in range(3)],
        "complete_groups": groups, "target_language": "zh-CN",
    }, call)
    assert calls == ["source 0\n\nsource 1\n\nsource 2"]
    assert "Must not" not in result["overview"]


@pytest.mark.asyncio
async def test_summary_preserves_reviewed_background_reference_without_model_call() -> None:
    async def unused(body: dict[str, Any]) -> dict[str, Any]:
        raise AssertionError("complete group must be reused")

    reference = "https://www.rfc-editor.org/rfc/rfc3385"
    result = await compile_course({
        "segments": [{"id": "p1", "text": "Synthetic data check"}],
        "target_language": "zh-CN", "complete_groups": [{
            "coverage_contract": "all_sources_v1", "result": {
                "paragraph_summary": "Complete group", "evidence_segment_ids": ["p1"],
                "asset_page_ids": [], "terms": [{"term": "校验", "explanation": "背景说明",
                    "background_reference": reference,
                    "evidence_segment_ids": ["p1"], "asset_page_ids": []}],
            },
        }],
    }, unused)
    assert result["terminology"][0]["background_reference"] == reference
