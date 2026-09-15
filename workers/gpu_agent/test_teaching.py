"""Synthetic teaching coverage, citation, and bounded scheduling contracts."""

from typing import Any

import pytest

from workers.gpu_agent.teaching import (
    assemble_explanation,
    contains_term,
    person_reference,
    redundant_bilingual_name,
    source_pieces,
    teaching_chunks,
    valid_definition,
    valid_summary,
)

COMPLETE_DEFINITION = (
    "这是一个经过核对的定义对象；它用于说明当前来源中的专业概念和实际用途；"
    "这里补充它的工作方式、适用条件以及与相近概念之间容易混淆的边界"
)


def test_term_citations_use_words_not_unrelated_substrings() -> None:
    assert contains_term("RAM", "This RAM stores data")
    assert not contains_term("RAM", "The PROGRAM runs")
    assert not contains_term("net", "The internet is available")
    assert contains_term("网表", "这里的网表描述连接")
    assert not contains_term("RAM", "Use ram")  # Worker returns the source spelling


def test_redundant_bilingual_name_is_not_a_technical_term() -> None:
    assert redundant_bilingual_name("Alice (Alice)")
    assert redundant_bilingual_name(" Alice（alice） ")
    assert not redundant_bilingual_name("台积电（TSMC）")


def test_agent_quality_gate_matches_core_contract() -> None:
    assert valid_summary("完整说明", 100, "zh-CN")
    assert not valid_summary("当我在讲解一个主题", 100, "zh-CN")
    assert not valid_summary("过短", 240, "zh-CN")
    assert not valid_summary("过长" * 601, 240, "zh-CN")
    assert valid_definition(COMPLETE_DEFINITION, "zh-CN")
    assert not valid_definition("只有一句很短的定义", "zh-CN")
    assert not valid_definition("这是定义；" + "用于说明技术对象" * 40 + "；这里保留边界", "zh-CN")


def test_self_introduced_person_is_not_a_technical_term() -> None:
    assert person_reference("Alice", "My name is Alice and I teach this course.")
    assert person_reference("Alice Smith", "Professor Alice Smith explains testing.")
    assert not person_reference("latch", "A latch stores one bit.")


@pytest.mark.asyncio
async def test_group_caps_model_generated_glossary_to_key_concepts() -> None:
    definition_calls = 0
    largest_definition_batch = 0

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        nonlocal definition_calls, largest_definition_batch
        provider = "ollama:synthetic@cuda"
        if body["phase"] == "prose":
            return {
                "provider": provider,
                "prose": "完整解释",
                "original_terms": [f"term{index}" for index in range(12)],
            }
        if body["phase"] == "definition":
            definition_calls += 1
            return {"provider": provider, "term": body["original_term"],
                    "definition": COMPLETE_DEFINITION}
        if body["phase"] == "definitions":
            definition_calls += 1
            largest_definition_batch = max(
                largest_definition_batch, len(body["original_terms"]),
            )
            return {
                "provider": provider,
                "definitions": [{
                    "original_term": term,
                    "term": term,
                    "definition": COMPLETE_DEFINITION,
                } for term in body["original_terms"]],
            }
        return {"provider": provider, "prose": body["text"]}

    result = await assemble_explanation({
        "segments": [{
            "id": "a",
            "text": " ".join(f"term{index}" for index in range(12)),
        }],
        "target_language": "zh-CN",
    }, call)
    assert definition_calls == 4
    assert largest_definition_batch == 2
    assert len(result["terms"]) == 8


@pytest.mark.asyncio
async def test_term_reference_does_not_include_a_substring_only_paragraph() -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        if body["phase"] == "group":
            return {"provider": "ollama:synthetic@cuda", "prose": "连贯的组合说明"}
        if body["phase"] == "prose":
            return {"provider": "ollama:synthetic@cuda", "prose": "合成说明",
                    "original_terms": ["net"]}
        return {"provider": "ollama:synthetic@cuda", "term": "线网",
                "definition": COMPLETE_DEFINITION}

    result = await assemble_explanation({"segments": [
        {"id": "a", "text": "The net connects cells."},
        {"id": "b", "text": "Read the internet documentation."},
    ], "target_language": "zh-CN"}, call)
    assert result["terms"][0]["evidence_segment_ids"] == ["a"]


@pytest.mark.asyncio
async def test_reviewed_source_term_survives_an_empty_model_inventory() -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        assert body["phase"] == "prose"
        return {"provider": "ollama:synthetic@cuda", "prose": "说明分区间的连接代价。",
                "original_terms": []}

    result = await assemble_explanation({
        "segments": [{"id": "first", "text": "Circuit partitioning reduces cut size."}],
        "target_language": "zh-CN",
    }, call)
    assert result["terms"][0]["evidence_segment_ids"] == ["first"]
    assert result["terms"][0]["background_reference"].startswith("https://")


@pytest.mark.parametrize("text", ["word " * 1500, "术语，条件。" * 900, "x" * 5000])
def test_source_pieces_preserve_every_character(text: str) -> None:
    parts = source_pieces(text)
    assert "".join(parts) == text
    assert all(len(part.encode()) <= 2800 for part in parts)


@pytest.mark.asyncio
async def test_card_covers_all_sources_and_defines_repeated_term_only_once() -> None:
    calls: list[dict[str, Any]] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        calls.append(body)
        assert "id" not in body and "session_id" not in body
        provider = "ollama:synthetic@cuda"
        if body["phase"] == "group":
            return {"provider": provider, "prose": body["text"]}
        if body["phase"] == "prose":
            return {"provider": provider, "prose": body["text"], "original_terms": ["latch"]}
        return {"provider": provider, "term": "锁存器",
                "definition": COMPLETE_DEFINITION}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "A latch stores a bit."},
                     {"id": "b", "text": "The latch is level sensitive."}],
        "asset_pages": [{"id": "page", "text": "A latch diagram."}], "target_language": "zh-CN",
    }, call)
    assert len(calls) == 4
    assert [body["phase"] for body in calls] == [
        "prose", "prose", "definition", "group",
    ]
    assert calls[0]["text"] == "A latch stores a bit.\n\nThe latch is level sensitive."
    assert calls[0]["context"] == ["A latch diagram."]
    assert result["evidence_segment_ids"] == ["a", "b"]
    assert result["asset_page_ids"] == ["page"]
    assert result["terms"][0]["evidence_segment_ids"] == ["a", "b"]
    assert result["terms"][0]["asset_page_ids"] == ["page"]
    assert len(result["paragraph_summary"].split("\n\n")) == 3


@pytest.mark.asyncio
async def test_multiple_short_paragraphs_share_one_coherent_model_call() -> None:
    phases: list[str] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        phases.append(body["phase"])
        return {
            "provider": "ollama:synthetic@cuda",
            "prose": (
                "先说明电路测试要解决的问题，再解释故障模型如何缩小验证范围；"
                "随后说明测试向量怎样激励电路并观察响应；"
                "最后保留模型无法覆盖全部物理缺陷这一适用边界"
            ),
            "original_terms": [],
        }

    result = await assemble_explanation({
        "segments": [
            {"id": "a", "text": "Fault models define the target."},
            {"id": "b", "text": "Test vectors expose the modeled response."},
        ],
        "target_language": "zh-CN",
    }, call)
    assert phases == ["prose"]
    assert result["paragraph_summary"].startswith("先说明电路测试")


def test_group_chunking_retains_all_text_and_source_ownership() -> None:
    records = [{"id": str(i), "text": ("内容，条件，例子。" * 90) + str(i)} for i in range(5)]
    chunks = teaching_chunks(records)
    assert all(len("\n\n".join(item["text"] for item in chunk).encode()) <= 2800
               for chunk in chunks)
    for source in records:
        assert "".join(item["text"] for chunk in chunks for item in chunk
                       if item["id"] == source["id"]) == source["text"]


@pytest.mark.asyncio
async def test_group_inventory_cites_only_sources_containing_the_term() -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        if body["phase"] == "group":
            return {"provider": "ollama:synthetic@cuda", "prose": "完整组合说明"}
        if body["phase"] == "prose":
            return {"provider": "ollama:synthetic@cuda", "prose": "连贯解释",
                    "original_terms": ["latch"]}
        return {"provider": "ollama:synthetic@cuda", "term": "锁存器",
                "definition": COMPLETE_DEFINITION}

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
        if body["phase"] == "group":
            return {"provider": "ollama:synthetic@cuda", "prose": body["text"]}
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
async def test_failed_definition_batch_falls_back_without_losing_the_card() -> None:
    phases: list[str] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        phases.append(body["phase"])
        provider = "ollama:synthetic@cuda"
        if body["phase"] == "prose":
            return {"provider": provider, "prose": "完整解释",
                    "original_terms": ["alpha", "beta"]}
        if body["phase"] == "definitions":
            raise RuntimeError("model_http_error")
        if body["original_term"] == "beta":
            raise RuntimeError("model_http_error")
        return {"provider": provider, "term": "Alpha", "definition": COMPLETE_DEFINITION}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "alpha and beta are compared."}],
        "target_language": "zh-CN",
    }, call)
    assert phases == ["prose", "definitions", "definition", "definition"]
    assert result["paragraph_summary"] == "完整解释"
    assert [term["term"] for term in result["terms"]] == ["Alpha"]


@pytest.mark.asyncio
async def test_invalid_optional_definition_is_dropped_without_losing_prose() -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        if body["phase"] == "prose":
            return {"provider": "ollama:synthetic@cuda", "prose": "完整解释",
                    "original_terms": ["alpha"]}
        return {"provider": "ollama:synthetic@cuda", "term": "Alpha", "definition": "过短"}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "alpha is compared."}],
        "target_language": "zh-CN",
    }, call)
    assert result["paragraph_summary"] == "完整解释"
    assert result["terms"] == []


@pytest.mark.asyncio
async def test_malformed_optional_definition_batch_does_not_discard_prose() -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        if body["phase"] == "prose":
            return {"provider": "ollama:synthetic@cuda", "prose": "完整解释",
                    "original_terms": ["alpha", "beta"]}
        return {"provider": "ollama:synthetic@cuda", "definitions": []}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "alpha and beta are compared."}],
        "target_language": "zh-CN",
    }, call)
    assert result["paragraph_summary"] == "完整解释"
    assert result["terms"] == []


@pytest.mark.asyncio
async def test_segment_and_page_receive_one_group_synthesis() -> None:
    phases: list[str] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        phases.append(body["phase"])
        if body["phase"] == "group":
            return {"provider": "ollama:synthetic@cuda", "prose": "组合后的完整说明"}
        return {"provider": "ollama:synthetic@cuda", "prose": "分块说明",
                "original_terms": []}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "First premise."}],
        "asset_pages": [{"id": "p", "text": "Supporting page."}],
        "target_language": "zh-CN",
    }, call)
    assert phases == ["prose", "prose", "group"]
    assert result["paragraph_summary"] == "组合后的完整说明"


@pytest.mark.asyncio
async def test_failed_group_synthesis_never_publishes_piece_drafts() -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        if body["phase"] == "group":
            raise RuntimeError("model_http_error")
        return {"provider": "ollama:synthetic@cuda", "prose": "完整逐段说明",
                "original_terms": []}

    with pytest.raises(ValueError, match="teaching_group_synthesis_invalid"):
        await assemble_explanation({
            "segments": [
                {"id": "a", "text": "First premise. " * 140},
                {"id": "b", "text": "Second conclusion. " * 120},
            ],
            "target_language": "zh-CN",
        }, call)


@pytest.mark.asyncio
async def test_changed_provider_never_returns_a_partial_card() -> None:
    async def call(body: dict[str, Any]) -> dict[str, Any]:
        if body["phase"] == "prose":
            return {"provider": "ollama:synthetic@cuda", "prose": "完整解释",
                    "original_terms": ["latch"]}
        return {"provider": "ollama:other@cuda", "term": "锁存器",
                "definition": COMPLETE_DEFINITION}

    with pytest.raises(ValueError):
        await assemble_explanation({"segments": [{"id": "a", "text": "A latch."}],
                                    "target_language": "zh-CN"}, call)


@pytest.mark.asyncio
async def test_foreign_optional_term_is_dropped_without_losing_complete_prose() -> None:
    async def call(_body: dict[str, Any]) -> dict[str, Any]:
        return {"provider": "ollama:synthetic@cuda", "prose": "完整解释",
                "original_terms": ["unknown"]}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "A latch."}], "target_language": "zh-CN",
    }, call)
    assert result["paragraph_summary"] == "完整解释"
    assert result["terms"] == []
