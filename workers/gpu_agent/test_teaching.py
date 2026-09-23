"""Synthetic teaching coverage, citation, and bounded scheduling contracts."""

from typing import Any

import pytest

from workers.gpu_agent.teaching import (
    assemble_explanation,
    contains_term,
    parse_teaching_sections,
    person_reference,
    redundant_bilingual_name,
    source_pieces,
    teaching_chunks,
    valid_definition,
    valid_provider,
    valid_summary,
    valid_teaching_sections,
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
    assert not valid_summary("这段内容解释算法怎样优化分区设计并节省成本；" * 18, 500, "zh-CN")
    assert valid_definition(COMPLETE_DEFINITION, "zh-CN")
    assert not valid_definition("只有一句很短的定义", "zh-CN")
    assert not valid_definition("这是定义；" + "用于说明技术对象" * 40 + "；这里保留边界", "zh-CN")


def test_structured_sections_parse_and_legacy_paragraph_remains_usable() -> None:
    structured = parse_teaching_sections(
        "承上启下：前文建立的条件决定本节的问题\n"
        "主要内容：\n- 结论一\n- 结论二\n"
        "**内容讲解：**\n先解释对象之间的关系，再说明结果如何产生。\n"
        "易错点：\n**错误理解：** 把两个阶段当作同一步骤\n\n"
        "**错因：** 忽略了它们的输入不同\n\n"
        "**正确判断：** 分别按各自条件核对\n\n"
        "**核对方法：** 检查每一步的输入和输出",
        [{"term": "示例术语"}],
    )
    assert structured["chapter_bridge"].startswith("前文建立")
    assert structured["main_content"] == "- 结论一\n- 结论二"
    assert structured["professional_terms"] == [{"term": "示例术语"}]
    assert structured["content_explanation"].startswith("先解释对象")
    assert len(structured["misconceptions"]) == 1
    assert not structured["legacy_input"]
    assert valid_teaching_sections(structured, 80, "zh-CN")
    bullet_roles = parse_teaching_sections(
        "主要内容：\n- 一个主要结论\n"
        "内容讲解：\n解释这个结论的因果依据与适用边界。\n"
        "易错点：\n- 错误理解：把条件当结论\n"
        "- 错因：遗漏输入限制\n"
        "- 正确判断：先核对输入限制\n"
        "- 核对方法：比较条件和结论的来源"
    )
    assert len(bullet_roles["misconceptions"]) == 1
    assert valid_teaching_sections(bullet_roles, 80, "zh-CN")
    bold_label_roles = parse_teaching_sections(
        "主要内容：\n- 一项结论\n内容讲解：\n这是结论的依据。\n易错点：\n"
        "- **错误理解**：忽略限制\n- **错因**：只看结果\n"
        "- **正确判断**：检查条件\n- **核对方法**：回查输入"
    )
    assert valid_teaching_sections(bold_label_roles, 80, "zh-CN")
    unsupported_mistake = parse_teaching_sections(
        "主要内容：\n- 一项结论\n内容讲解：\n这是结论的依据。\n"
        "易错点：\n（原文未提供错误理解、错因、正确判断和核对方法）"
    )
    assert unsupported_mistake["misconceptions"] == []
    assert valid_teaching_sections(unsupported_mistake, 80, "zh-CN")
    no_supported_mistake = parse_teaching_sections(
        "主要内容：\n- 一项结论\n内容讲解：\n这是结论的依据。\n易错点：无"
    )
    assert no_supported_mistake["misconceptions"] == []
    assert valid_teaching_sections(no_supported_mistake, 80, "zh-CN")
    incomplete = {**structured, "misconceptions": ["**错误理解：** 忽略适用条件"]}
    assert not valid_teaching_sections(incomplete, 80, "zh-CN")

    legacy = parse_teaching_sections("第一句说明输入。第二句说明结果。第三句保留条件。")
    assert legacy["legacy_input"]
    assert legacy["content_explanation"] == "第一句说明输入。第二句说明结果。第三句保留条件。"
    assert legacy["main_content"] == "- 第一句说明输入。\n- 第二句说明结果。\n- 第三句保留条件。"
    assert valid_teaching_sections(legacy, 80, "zh-CN")
    legacy_with_old_heading = parse_teaching_sections(
        "主要内容：旧版标题仍属于正文。旧版内容继续保留。"
    )
    assert legacy_with_old_heading["legacy_input"]
    assert legacy_with_old_heading["content_explanation"].startswith("主要内容：")


def test_provider_validation_supports_both_local_and_cloud_lanes() -> None:
    assert valid_provider("ollama:local-model@cuda")
    assert valid_provider("kuafushe:deepseek-chat@cloud")
    assert not valid_provider("kuafushe:deepseek-chat@cuda")
    assert not valid_provider("other:deepseek-chat@cloud")


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
            return {"provider": provider, "prose": body["text"],
                    "original_terms": ["latch"], "used_material_indices": [0]}
        return {"provider": provider, "term": "锁存器",
                "definition": COMPLETE_DEFINITION}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "A latch stores a bit."},
                     {"id": "b", "text": "The latch is level sensitive."}],
        "asset_pages": [{"id": "page", "text": "A latch diagram."}], "target_language": "zh-CN",
    }, call)
    assert len(calls) == 2
    assert [body["phase"] for body in calls] == ["prose", "definition"]
    assert calls[0]["text"] == "A latch stores a bit.\n\nThe latch is level sensitive."
    assert calls[0]["context"] == []
    assert calls[0]["material_references"] == ["A latch diagram."]
    assert result["evidence_segment_ids"] == ["a", "b"]
    assert result["asset_page_ids"] == ["page"]
    assert result["terms"][0]["evidence_segment_ids"] == ["a", "b"]
    assert result["terms"][0]["asset_page_ids"] == []
    assert result["paragraph_summary"] == calls[0]["text"]


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
    assert result["teaching_sections"]["legacy_input"]
    assert result["teaching_sections"]["content_explanation"] == result["paragraph_summary"]
    assert result["teaching_sections"]["main_content"].startswith("- ")


@pytest.mark.asyncio
async def test_cloud_provider_is_accepted_without_an_extra_generation_call() -> None:
    phases: list[str] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        phases.append(body["phase"])
        return {"provider": "kuafushe:deepseek-chat@cloud",
                "prose": "完整解释一个观察到的关系", "original_terms": []}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "A relationship is observed."}],
        "target_language": "zh-CN",
    }, call)
    assert phases == ["prose"]
    assert result["provider"] == "kuafushe:deepseek-chat@cloud"
    assert result["paragraph_summary"] == result["teaching_sections"]["content_explanation"]


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
async def test_page_supplies_context_without_extra_prose_call() -> None:
    phases: list[str] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        phases.append(body["phase"])
        if body["phase"] == "group":
            return {"provider": "ollama:synthetic@cuda", "prose": "组合后的完整说明"}
        return {"provider": "ollama:synthetic@cuda", "prose": "分块说明",
                "original_terms": [], "used_material_indices": [0]}

    result = await assemble_explanation({
        "segments": [{"id": "a", "text": "First premise."}],
        "asset_pages": [
            {"id": "p", "text": "Supporting page."},
            {"id": "unrelated", "text": "Unrelated note."},
        ],
        "target_language": "zh-CN",
    }, call)
    assert phases == ["prose"]
    assert result["paragraph_summary"] == "分块说明"
    assert result["asset_page_ids"] == ["p"]


@pytest.mark.asyncio
async def test_optional_cloud_glossary_failure_keeps_verified_teaching() -> None:
    phases: list[str] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        phases.append(body["phase"])
        if body["phase"] in {"definition", "definitions"}:
            raise ValueError("cloud_teaching_contract_invalid")
        if body["phase"] == "group":
            return {
                "provider": "kuafushe:synthetic@cloud",
                "prose": (
                    "主要内容\n- 线网连接单元。\n\n内容讲解\n"
                    "线网连接单元，材料说明总线也是连接路径。\n\n易错点"
                ),
            }
        return {
            "provider": "kuafushe:synthetic@cloud",
            "prose": "主要内容\n- 说明连接关系。\n\n内容讲解\n材料说明了连接路径。\n\n易错点",
            "original_terms": ["net"] if "net" in body["text"] else ["bus"],
            "used_material_indices": [0],
        }

    result = await assemble_explanation({
        "segments": [{"id": "segment", "text": "The net connects cells."}],
        "asset_pages": [{"id": "page", "text": "The bus connects cells."}],
        "target_language": "zh-CN",
    }, call)
    assert phases == ["prose", "definition"]
    assert result["terms"] == []
    assert result["evidence_segment_ids"] == ["segment"]
    assert result["asset_page_ids"] == ["page"]


@pytest.mark.asyncio
async def test_cloud_glossary_caps_optional_batches_without_per_term_retry() -> None:
    phases: list[str] = []

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        phases.append(body["phase"])
        if body["phase"] == "definitions":
            raise ValueError("cloud_teaching_contract_invalid")
        assert body["phase"] == "prose"
        return {
            "provider": "kuafushe:synthetic@cloud",
            "prose": "主要内容\n- 解释连接。\n\n内容讲解\n这些名称指向不同连接。\n\n易错点：无",
            "original_terms": ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"],
            "used_material_indices": [],
        }

    result = await assemble_explanation({
        "segments": [{"id": "segment", "text": "alpha beta gamma delta epsilon zeta"}],
        "target_language": "zh-CN",
    }, call)
    assert phases == ["prose", "definitions", "definitions"]
    assert result["terms"] == []
    assert result["evidence_segment_ids"] == ["segment"]


@pytest.mark.asyncio
async def test_two_complete_drafts_above_old_byte_limit_are_still_synthesized() -> None:
    phases: list[str] = []
    synthesis_bytes = 0

    async def call(body: dict[str, Any]) -> dict[str, Any]:
        nonlocal synthesis_bytes
        phases.append(body["phase"])
        if body["phase"] == "group":
            synthesis_bytes = len(body["text"].encode())
            return {
                "provider": "ollama:synthetic@cuda",
                "prose": (
                    "先解释问题与必要条件，再连接两部分机制和例子；"
                    "第一部分给出了对象成立的前提，第二部分说明过程怎样产生结果；"
                    "这两部分按照时间顺序连接，读者可以分别核对条件、过程和结论；"
                    "最后保留结论成立所依赖的边界。"
                ),
            }
        return {
            "provider": "ollama:synthetic@cuda",
            "prose": (
                "这一部分完整保留来源中的对象、机制、例子、数量关系、"
                "限制条件和不确定性。" * 17
            ),
            "original_terms": [],
        }

    result = await assemble_explanation({
        "segments": [
            {"id": "a", "text": "First premise and its boundary. " * 60},
            {"id": "b", "text": "Second mechanism and its conclusion. " * 60},
        ],
        "target_language": "zh-CN",
    }, call)

    assert phases == ["prose", "prose", "group"]
    assert 3500 < synthesis_bytes <= 6200
    assert result["paragraph_summary"].startswith("先解释问题")


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
