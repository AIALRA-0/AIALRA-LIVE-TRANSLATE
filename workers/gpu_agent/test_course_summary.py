"""A course summary preserves all source groups, including its middle."""

from typing import Any

import pytest

from workers.gpu_agent.course_summary import compile_course


@pytest.mark.asyncio
async def test_complete_summary_reuses_groups_and_backfills_only_uncovered_sources() -> None:
    calls: list[dict[str, Any]] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        calls.append(body)
        return {"prose": body["text"], "original_terms": [], "provider": "ollama:test@cuda"}

    segments = [{"id": f"p{i}", "text": f"Synthetic paragraph {i}"} for i in range(100)]
    group = {"coverage_contract": "all_sources_v1", "result": {
        "paragraph_summary": "Verified first group", "terms": [],
        "evidence_segment_ids": ["p0", "p1"], "asset_page_ids": [],
    }}
    result = await compile_course({"segments": segments, "asset_pages": [
        {"id": "page", "text": "Synthetic material"},
    ], "complete_groups": [group], "target_language": "zh-CN"}, call)
    source_calls = [body["text"] for body in calls if body["phase"] == "group"]
    assert 1 < len(source_calls) < 99
    assert "\n\n".join(source_calls) == "\n\n".join(
        [item["text"] for item in segments[2:]] + ["Synthetic material"],
    )
    assert any(body["phase"] == "course" for body in calls)
    assert any("Synthetic paragraph 49" in note for note in result["key_points"])
    assert any("Synthetic paragraph 99" in note for note in result["key_points"])
    assert result["evidence_segment_ids"] == [f"p{i}" for i in range(100)]
    assert result["asset_page_ids"] == ["page"]
    assert result["provider"] == "ollama:test@cuda"


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
    assert calls[0] == "source 0\n\nsource 1\n\nsource 2"
    assert "Must not" not in result["overview"]


@pytest.mark.asyncio
async def test_summary_preserves_reviewed_background_reference_with_one_synthesis_call() -> None:
    async def synthesis(body: dict[str, Any]) -> dict[str, Any]:
        assert body["phase"] == "course"
        assert body["text"] == "Complete group"
        return {
            "prose": "Complete group overview", "original_terms": [],
            "provider": "ollama:test@cuda",
        }

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
    }, synthesis)
    assert result["terminology"][0]["background_reference"] == reference


@pytest.mark.asyncio
async def test_summary_accepts_cloud_provider_for_all_synthesis_parts() -> None:
    async def synthesis(body: dict[str, Any]) -> dict[str, Any]:
        return {
            "prose": f"{body['phase']}: synthetic explanation",
            "original_terms": [],
            "provider": "kuafushe:deepseek-v4.1-flash@cloud",
        }

    result = await compile_course({
        "segments": [{"id": "p1", "text": "Synthetic lesson paragraph"}],
        "target_language": "zh-CN",
    }, synthesis)
    assert result["provider"] == "kuafushe:deepseek-v4.1-flash@cloud"
    assert result["evidence_segment_ids"] == ["p1"]


@pytest.mark.asyncio
async def test_summary_rejects_provider_switch_midcourse() -> None:
    calls = 0

    async def synthesis(body: dict[str, Any]) -> dict[str, Any]:
        nonlocal calls
        calls += 1
        provider = "kuafushe:deepseek-v4.1-flash@cloud" if calls == 1 else "ollama:test@cuda"
        return {"prose": "Synthetic explanation", "provider": provider}

    with pytest.raises(ValueError, match="course_synthesis_invalid"):
        await compile_course({
            "segments": [{"id": "p1", "text": "Synthetic lesson paragraph"}],
            "target_language": "zh-CN",
        }, synthesis)
