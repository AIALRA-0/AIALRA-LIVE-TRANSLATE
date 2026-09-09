"""Synthetic teaching coverage, citation, and bounded scheduling contracts."""

from typing import Any

import pytest

from workers.gpu_agent.teaching import (
    assemble_explanation,
    contains_term,
    source_pieces,
    teaching_chunks,
)


def test_term_citations_use_words_not_unrelated_substrings() -> None:
    assert contains_term("RAM", "This RAM stores data")
    assert not contains_term("RAM", "The PROGRAM runs")
    assert not contains_term("net", "The internet is available")
    assert contains_term("网表", "这里的网表描述连接")
    assert not contains_term("RAM", "Use ram")  # Worker returns the source spelling


@pytest.mark.asyncio
async def test_term_reference_does_not_include_a_substring_only_paragraph() -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        if body["phase"] == "prose":
            return {"provider": "ollama:synthetic@cuda", "prose": "合成说明",
                    "original_terms": ["net"]}
        return {"provider": "ollama:synthetic@cuda", "term": "线网", "definition": "连接关系"}

    result = await assemble_explanation({"segments": [
        {"id": "a", "text": "The net connects cells."},
        {"id": "b", "text": "Read the internet documentation."},
    ], "target_language": "zh-CN"}, call)
    assert result["terms"][0]["evidence_segment_ids"] == ["a"]


@pytest.mark.parametrize("text", ["word " * 1500, "术语，条件。" * 900, "x" * 5000])
def test_source_pieces_preserve_every_character(text: str) -> None:
    parts = source_pieces(text)
    assert "".join(parts) == text
    assert all(len(part.encode()) <= 1400 for part in parts)


@pytest.mark.asyncio
async def test_card_covers_all_sources_and_defines_repeated_term_only_once() -> None:
    calls: list[dict[str, Any]] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        calls.append(body)
        assert "id" not in body and "session_id" not in body
        provider = "ollama:synthetic@cuda"
        if body["phase"] == "prose":
            return {"provider": provider, "prose": body["text"], "original_terms": ["latch"]}
        return {"provider": provider, "term": "锁存器", "definition": "受使能控制的存储元件"}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "A latch stores a bit."},
                     {"id": "b", "text": "The latch is level sensitive."}],
        "asset_pages": [{"id": "page", "text": "A latch diagram."}], "target_language": "zh-CN",
    }, call)
    assert len(calls) == 3
    assert [body["phase"] for body in calls] == ["prose", "prose", "definition"]
    assert calls[0]["text"] == "A latch stores a bit.\n\nThe latch is level sensitive."
    assert calls[0]["context"] == ["A latch diagram."]
    assert result["evidence_segment_ids"] == ["a", "b"]
    assert result["asset_page_ids"] == ["page"]
    assert result["terms"][0]["evidence_segment_ids"] == ["a", "b"]
    assert result["terms"][0]["asset_page_ids"] == ["page"]
    assert len(result["paragraph_summary"].split("\n\n")) == 3


def test_group_chunking_retains_all_text_and_source_ownership() -> None:
    records = [{"id": str(i), "text": ("内容，条件，例子。" * 90) + str(i)} for i in range(5)]
    chunks = teaching_chunks(records)
    assert all(len("\n\n".join(item["text"] for item in chunk).encode()) <= 1400
               for chunk in chunks)
    for source in records:
        assert "".join(item["text"] for chunk in chunks for item in chunk
                       if item["id"] == source["id"]) == source["text"]


@pytest.mark.asyncio
async def test_group_inventory_cites_only_sources_containing_the_term() -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        if body["phase"] == "prose":
            return {"provider": "ollama:synthetic@cuda", "prose": "连贯解释",
                    "original_terms": ["latch"]}
        return {"provider": "ollama:synthetic@cuda", "term": "锁存器", "definition": "存储元件"}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "A latch stores a bit."},
                     {"id": "b", "text": "It responds while enabled."}],
        "target_language": "zh-CN",
    }, call)
    assert result["evidence_segment_ids"] == ["a", "b"]
    assert result["terms"][0]["evidence_segment_ids"] == ["a"]


@pytest.mark.asyncio
async def test_equivalent_reviewed_names_merge_references_not_meanings() -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        assert body["phase"] == "prose"
        return {"provider": "ollama:synthetic@cuda", "prose": "完整解释",
                "original_terms": ["frequency", "clock frequency"]}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "The circuit frequency is 1 MHz."},
                     {"id": "b", "text": "The clock frequency changes."}],
        "target_language": "zh-CN",
    }, call)
    assert len(result["terms"]) == 1
    assert result["terms"][0]["evidence_segment_ids"] == ["a", "b"]
    assert result["terms"][0]["background_reference"].startswith("https://www.nist.gov/")


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", ["foreign_term", "missing_definition", "changed_provider"])
async def test_bad_part_never_returns_a_partial_card(failure: str) -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        if body["phase"] == "prose":
            return {"provider": "ollama:synthetic@cuda", "prose": "完整解释",
                    "original_terms": ["unknown" if failure == "foreign_term" else "latch"]}
        return {"provider": "ollama:other@cuda" if failure == "changed_provider"
                else "ollama:synthetic@cuda", "term": "锁存器",
                "definition": "" if failure == "missing_definition" else "存储元件"}

    with pytest.raises(ValueError):
        await assemble_explanation({"segments": [{"id": "a", "text": "A latch."}],
                                    "target_language": "zh-CN"}, call)
