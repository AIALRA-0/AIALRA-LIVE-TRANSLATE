"""A course summary preserves lecture groups without promoting unused materials."""

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
        [item["text"] for item in segments[2:]],
    )
    assert any(body["phase"] == "course" for body in calls)
    assert any("Synthetic paragraph 49" in note for note in result["key_points"])
    assert any("Synthetic paragraph 99" in note for note in result["key_points"])
    assert result["evidence_segment_ids"] == [f"p{i}" for i in range(100)]
    assert result["asset_page_ids"] == []
    assert result["provider"] == "ollama:test@cuda"


@pytest.mark.asyncio
async def test_only_material_cited_by_a_verified_group_reaches_course_summary() -> None:
    calls: list[dict[str, Any]] = []

    async def synthesis(body: dict[str, Any]) -> dict[str, Any]:
        calls.append(body)
        return {"prose": "课程要点", "provider": "kuafushe:synthetic@cloud"}

    result = await compile_course({
        "segments": [{"id": "p1", "text": "Lecture paragraph"}],
        "asset_pages": [
            {"id": "used", "text": "Relevant material"},
            {"id": "unused", "text": "Unrelated appendix"},
        ],
        "complete_groups": [{"coverage_contract": "all_sources_v1", "result": {
            "paragraph_summary": "Verified teaching note",
            "terms": [], "evidence_segment_ids": ["p1"], "asset_page_ids": ["used"],
        }}],
        "target_language": "zh-CN",
    }, synthesis)
    assert [body["phase"] for body in calls] == ["course"]
    assert calls[0]["text"] == "Verified teaching note"
    assert result["asset_page_ids"] == ["used"]


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


@pytest.mark.asyncio
async def test_long_course_reduces_utf8_bytes_without_dropping_chapters() -> None:
    phases: list[str] = []

    async def synthesis(body: dict[str, Any]) -> dict[str, Any]:
        phases.append(body["phase"])
        text = "概念关系" * (200 if body["phase"] != "course_reduce" else 75)
        return {"prose": text, "provider": "ollama:test@cuda"}

    segments = [
        {"id": f"p{index}", "text": f"Concept {index}: " + "connected facts. " * 18}
        for index in range(283)
    ]
    result = await compile_course({
        "segments": segments, "target_language": "zh-CN",
    }, synthesis)
    assert "course_reduce" in phases
    assert result["evidence_segment_ids"] == [item["id"] for item in segments]
    assert len(result["key_points"]) > 1
    assert result["overview"]


@pytest.mark.asyncio
async def test_verified_group_chapters_are_preserved_without_cloud_rewriting() -> None:
    phases: list[str] = []

    async def synthesis(body: dict[str, Any]) -> dict[str, Any]:
        phases.append(body["phase"])
        return {
            "prose": "承上启下：\n\n主要内容：有效事实。\n\n内容讲解：有效事实。\n\n易错点：无",
            "provider": "kuafushe:test@cloud",
        }

    notes = [f"电路故障模型与仿真边界（第 {index + 1} 组）。" * 24 for index in range(12)]
    groups = [{"coverage_contract": "all_sources_v1", "result": {
        "paragraph_summary": notes[index],
        "terms": [], "evidence_segment_ids": [f"p{index}"], "asset_page_ids": [],
    }} for index in range(12)]
    result = await compile_course({
        "segments": [{"id": f"p{index}", "text": f"source {index}"} for index in range(12)],
        "complete_groups": groups, "target_language": "zh-CN",
    }, synthesis)
    assert result["key_points"] == notes
    assert "course_reduce" in phases
    assert phases[-1] == "course"
    assert len(phases) < len(notes)
    assert result["evidence_segment_ids"] == [f"p{index}" for index in range(12)]


@pytest.mark.asyncio
async def test_cloud_contract_retry_repeats_only_the_rejected_part() -> None:
    phases: list[str] = []

    async def synthesis(body: dict[str, Any]) -> dict[str, Any]:
        phases.append(body["phase"])
        if body["phase"] == "group" and phases.count("group") <= 2:
            raise ValueError("cloud_teaching_contract_invalid")
        return {"prose": "合格的讲解内容", "provider": "kuafushe:test@cloud"}

    result = await compile_course({
        "segments": [{"id": "p1", "text": "Synthetic lesson paragraph"}],
        "target_language": "zh-CN",
    }, synthesis)
    assert phases == ["group", "group", "group", "course"]
    assert result["evidence_segment_ids"] == ["p1"]


@pytest.mark.asyncio
async def test_cloud_contract_retry_is_bounded_and_other_errors_fail_fast() -> None:
    attempts = 0

    async def rejected(_body: dict[str, Any]) -> dict[str, Any]:
        nonlocal attempts
        attempts += 1
        raise ValueError("cloud_teaching_contract_invalid")

    request = {"segments": [{"id": "p1", "text": "Synthetic lesson paragraph"}],
               "target_language": "zh-CN"}
    with pytest.raises(ValueError, match="cloud_teaching_contract_invalid"):
        await compile_course(request, rejected)
    assert attempts == 4

    attempts = 0

    async def other_error(_body: dict[str, Any]) -> dict[str, Any]:
        nonlocal attempts
        attempts += 1
        raise ValueError("course_synthesis_capacity_exceeded")

    with pytest.raises(ValueError, match="course_synthesis_capacity_exceeded"):
        await compile_course(request, other_error)
    assert attempts == 1
