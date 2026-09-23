"""Teaching uses bounded source-only names and rejects missing evidence."""

import json
from typing import Any, Literal

import pytest
from pydantic import ValidationError

from workers.model_worker.teaching import (
    TeachingPartRequest,
    bound_inventory,
    generate_part,
    readable_synthesis,
    repetition_collapse,
    source_surface,
    valid_part,
)
from workers.model_worker.terminology import matching_technical_terms


def test_synthesis_rejects_transcript_narration_and_thin_long_source_summary() -> None:
    assert not readable_synthesis("当我在讲解这个问题时，你会看到我所说的内容", "材料")
    assert not readable_synthesis("很短的总结", "source " * 80)
    assert not readable_synthesis("长" * 1201, "source " * 80)
    collapsed = "这段内容解释算法怎样优化分区设计并节省成本；" * 18
    assert repetition_collapse(collapsed)
    assert not readable_synthesis(collapsed, "source " * 80)
    assert readable_synthesis(
        "这部分先定义故障模型，再说明测试向量怎样激励电路并暴露响应中的异常；"
        "随后比较不同故障对输出的影响，解释覆盖率反映哪些目标已经被测试；"
        "最后区分检测到故障与定位故障的边界，并保留材料没有给出的实现条件",
        "source " * 80,
    )


def test_fm_naming_requires_partitioning_context() -> None:
    assert matching_technical_terms(["FM radio uses frequency modulation"], "zh-CN") == []
    assert ("FM", "FM 电路划分算法（Fiduccia–Mattheyses）") in matching_technical_terms(
        ["FM improves the solution", "This is hypergraph partitioning"], "zh-CN",
    )


def test_technical_inventory_does_not_extract_acronyms_from_inside_words() -> None:
    assert source_surface("RAM", "This program loads values.") is None
    assert source_surface("net", "The internet is available.") is None
    assert source_surface("RAM", "The RAM, not the register, stores it.") == "RAM"
    assert source_surface("load-use", "A load-use dependency.") == "load-use"
    assert source_surface("锁存器", "这个锁存器保存数据") == "锁存器"


def test_definition_requires_exact_source_and_bounded_context() -> None:
    with pytest.raises(ValidationError):
        TeachingPartRequest(phase="definition", text="A latch", original_term="clock",
                            target_language="zh-CN")
    with pytest.raises(ValidationError):
        TeachingPartRequest(phase="prose", text="语" * 3000, target_language="zh-CN")
    with pytest.raises(ValidationError):
        TeachingPartRequest(
            phase="prose", text="A latch.", material_references=["x" * 6500],
            target_language="zh-CN",
        )
    request = TeachingPartRequest(phase="prose", text="A latch.", target_language="zh-CN")
    assert valid_part({"prose": "锁存器", "original_terms": ["latch"]}, request)
    assert not valid_part({"prose": "锁存器", "original_terms": ["clock"]}, request)
    assert not valid_part({"prose": "Only English", "original_terms": []}, request)
    assert not valid_part({"prose": "锁存器", "original_terms": ["latch", "latch"]}, request)
    assert valid_part({"prose": "锁存器", "original_terms": ["Latch"]}, request)
    raw = {"prose": "锁存器", "original_terms": ["Latch", "latch", "memory bank"]}
    bound = bound_inventory(raw, request)
    assert bound == {"prose": "锁存器", "original_terms": ["latch"]}
    assert raw["original_terms"] == ["Latch", "latch", "memory bank"]
    with_material = TeachingPartRequest(
        phase="prose", text="A latch.", material_references=["Supplementary note"],
        target_language="zh-CN",
    )
    assert not valid_part({"prose": "锁存器", "original_terms": []}, with_material)
    assert not valid_part({"prose": "锁存器", "original_terms": [],
                           "used_material_indices": [1]}, with_material)
    assert valid_part({"prose": "锁存器", "original_terms": [],
                       "used_material_indices": []}, with_material)


def test_course_reduction_requires_a_real_byte_shrink() -> None:
    request = TeachingPartRequest(
        phase="course_reduce", text="Synthetic chapter notes", target_language="zh-CN",
    )
    assert valid_part({
        "prose": "故障模型限定可检查的对象；测试向量激励电路并观察输出；"
                 "通过测试不等于排除全部物理缺陷。",
        "original_terms": [],
    }, request)
    assert not valid_part({"prose": "概念关系" * 130, "original_terms": []}, request)


@pytest.mark.asyncio
async def test_course_reduction_repair_matches_the_validated_limits() -> None:
    async def infer(
        system: str, user: str, schema: dict[str, Any], **options: Any,
    ) -> dict[str, Any]:
        repair = options["repair_instruction"]
        assert "500 Unicode characters" in repair
        assert "1,600 UTF-8 bytes" in repair
        assert "1,200" not in repair
        return {
            "prose": "故障模型说明电路需要检查的错误条件；测试向量用于观察响应。",
            "original_terms": [],
        }

    result = await generate_part(TeachingPartRequest(
        phase="course_reduce", text="Synthetic chapter notes", target_language="zh-CN",
    ), infer, "synthetic-model", "cuda")
    assert result is not None


def test_batched_definitions_preserve_source_order_and_full_contract() -> None:
    request = TeachingPartRequest(
        phase="definitions",
        text="A latch is controlled by a clock.",
        original_terms=["latch", "clock"],
        target_language="zh-CN",
    )
    complete = (
        "这是一个技术概念；它用于解释合成测试中的作用；"
        "具体机制按来源上下文确定；使用时必须保留适用条件、限制和与相近概念之间的区别"
    )
    assert valid_part({"definitions": [
        {"original_term": "latch", "term": "锁存器（latch）", "definition": complete},
        {"original_term": "clock", "term": "时钟（clock）", "definition": complete},
    ]}, request)
    assert not valid_part({"definitions": [
        {"original_term": "clock", "term": "时钟（clock）", "definition": complete},
        {"original_term": "latch", "term": "锁存器（latch）", "definition": complete},
    ]}, request)


def test_definition_rejects_glossary_essay_over_240_characters() -> None:
    request = TeachingPartRequest(
        phase="definition", text="A latch stores data.", original_term="latch",
        target_language="zh-CN",
    )
    too_long = (
        "这是一个用于保存数据状态的技术概念；"
        + "它说明用途、机制和边界" * 30
        + "；使用时需要结合当前上下文"
    )
    assert len(too_long) > 240
    assert not valid_part({"term": "锁存器（Latch）", "definition": too_long}, request)


@pytest.mark.asyncio
async def test_batched_definition_generation_uses_one_structured_call() -> None:
    calls = 0
    complete = (
        "这是一个技术概念；它用于解释同步电路中的作用；"
        "具体机制由输入上下文确定；使用时必须保留适用条件、限制和相近概念的区别"
    )

    async def infer(
        system: str, user: str, schema: dict[str, Any], **options: Any,
    ) -> dict[str, Any]:
        nonlocal calls
        calls += 1
        assert json.loads(user)["original_terms"] == ["latch", "clock"]
        assert schema["properties"]["definitions"]["minItems"] == 2
        assert options["max_tokens"] == 840
        return {"definitions": [
            {"original_term": "latch", "term": "锁存器（latch）", "definition": complete},
            {"original_term": "clock", "term": "时钟（clock）", "definition": complete},
        ]}

    result = await generate_part(TeachingPartRequest(
        phase="definitions",
        text="A latch is controlled by a clock.",
        original_terms=["latch", "clock"],
        target_language="zh-CN",
    ), infer, "test", "cuda")
    assert calls == 1
    assert result is not None and len(result.definitions) == 2


def test_source_uncertainty_is_not_rejected_to_make_prose_sound_certain() -> None:
    request = TeachingPartRequest(
        phase="prose", text="The comparison is unclear. We have no final measurement.",
        target_language="zh-CN",
    )
    payload = {"prose": "当前比较尚不明确，还没有最终测量值", "original_terms": []}
    assert valid_part(payload, request)


@pytest.mark.asyncio
async def test_course_synthesis_uses_bounded_notes_without_term_inventory() -> None:
    async def infer(
        system: str, user: str, schema: dict[str, Any], **options: Any,
    ) -> dict[str, Any]:
        assert "whole course" in system
        assert "direction of the relationship" in system
        assert "physical chip speed" in system
        assert "chapter bridge" in system
        assert "main content" in system
        assert "content explanation" in system
        assert "misconceptions" in system
        assert json.loads(user)["source"] == "First, gates are modeled. Then their delays matter."
        assert options["max_tokens"] == 1400
        assert options["timeout_seconds"] == 75 and options["attempts"] == 2
        assert list(schema["properties"]) == ["prose"]
        assert schema["properties"]["prose"]["maxLength"] == 1200
        return {"prose": "先建立门电路模型，再考虑门延迟对结果的影响"}

    result = await generate_part(TeachingPartRequest(
        phase="course", text="First, gates are modeled. Then their delays matter.",
        target_language="zh-CN",
    ), infer, "test", "cuda")
    assert result is not None and result.provider == "ollama:test@cuda"
    assert result.original_terms == []
    assert not valid_part({"prose": "有结论", "original_terms": ["gates"]}, TeachingPartRequest(
        phase="course", text="gates", target_language="zh-CN",
    ))


@pytest.mark.asyncio
async def test_part_generation_is_bounded_and_retains_source() -> None:
    async def infer(
        system: str, user: str, schema: dict[str, Any], **options: Any,
    ) -> dict[str, Any]:
        assert "A latch." in user
        assert options["max_tokens"] == 1000 and options["num_ctx"] == 8192
        assert options["thinking"] is False
        assert "original_terms" in schema["required"]
        assert "source" in system
        assert "chapter bridge" in system
        assert "content explanation" in system
        assert "错误理解" in system
        assert "Do not hide that uncertainty" in system
        return {"prose": "锁存器保存数据", "original_terms": ["latch"]}

    result = await generate_part(TeachingPartRequest(
        phase="prose", text="A latch.", target_language="zh-CN",
    ), infer, "test", "cuda")
    assert result is not None and result.provider == "ollama:test@cuda"


@pytest.mark.asyncio
async def test_prose_inventory_keeps_only_the_four_prioritized_source_terms() -> None:
    source = "A latch, flip-flop, register, clock, result and write back appear here."

    async def infer(
        system: str, user: str, schema: dict[str, Any], **options: Any,
    ) -> dict[str, Any]:
        assert schema["properties"]["original_terms"]["maxItems"] == 4
        assert "dictionary translation of an ordinary word" in system
        return {"prose": "锁存器和触发器按时钟保持数据", "original_terms": [
            "latch", "flip-flop", "register", "clock", "result", "write back",
        ]}

    result = await generate_part(TeachingPartRequest(
        phase="prose", text=source, target_language="zh-CN",
    ), infer, "test", "cuda")
    assert result is not None
    assert result.original_terms == ["latch", "flip-flop", "register", "clock"]


@pytest.mark.asyncio
@pytest.mark.parametrize("phase", ["prose", "definition"])
async def test_adjacent_source_context_reaches_model_without_becoming_inventory(
    phase: Literal["prose", "definition"],
) -> None:
    context = ["We are discussing circuit partitioning, not radio modulation."]

    async def infer(
        system: str, user: str, schema: dict[str, Any], **options: Any,
    ) -> dict[str, Any]:
        supplied = json.loads(user)
        assert supplied["source"] == "FM is the algorithm used here."
        assert supplied["context_reference"] == context
        assert "context_reference" in system
        if phase == "prose":
            return {"prose": "这里使用 FM 算法", "original_terms": ["FM", "radio"]}
        return {"term": "FM 算法", "definition": (
            "这是一种用于电路划分的启发式算法；它通过移动单元来减少跨区连接；"
            "它适合在规模约束下改进已有划分，并不保证找到全局最优结果"
        )}

    result = await generate_part(TeachingPartRequest(
        phase=phase, text="FM is the algorithm used here.", context=context,
        original_term="FM" if phase == "definition" else None, target_language="zh-CN",
    ), infer, "test", "cuda")
    assert result is not None
    if phase == "prose":
        assert result.original_terms == ["FM"]


@pytest.mark.asyncio
async def test_material_reference_is_separate_from_lecture_source() -> None:
    async def infer(
        system: str, user: str, schema: dict[str, Any], **options: Any,
    ) -> dict[str, Any]:
        supplied = json.loads(user)
        assert supplied["source"] == "The net connects cells."
        assert supplied["material_references"] == ["A bus may connect several cells."]
        assert "not lecturer speech" in system
        assert "used_material_indices" in schema["required"]
        return {"prose": "线网连接单元", "original_terms": ["net", "bus"],
                "used_material_indices": [0]}

    result = await generate_part(TeachingPartRequest(
        phase="prose", text="The net connects cells.",
        material_references=["A bus may connect several cells."],
        target_language="zh-CN",
    ), infer, "test", "cuda")
    assert result is not None
    assert result.original_terms == ["net"]
    assert result.used_material_indices == [0]


@pytest.mark.asyncio
async def test_generated_definition_style_does_not_change_source_or_raw_result() -> None:
    raw = {"term": "Register (寄存器)", "definition": (
        "寄存器是用于保存数字状态的电路单元；它在控制时刻接收并保持输入值；"
        "这里保留 1.25 V，但这个示例并不表示所有输入都适用。"
    )}

    async def infer(
        system: str, user: str, schema: dict[str, Any], **options: Any,
    ) -> dict[str, Any]:
        assert json.loads(user)["source"] == "The register preserves a value."
        return raw

    result = await generate_part(TeachingPartRequest(
        phase="definition", text="The register preserves a value.", original_term="register",
        target_language="zh-CN",
    ), infer, "test", "cuda")
    assert result is not None
    assert result.term == "寄存器（Register）"
    assert result.definition == (
        "寄存器是用于保存数字状态的电路单元；它在控制时刻接收并保持输入值；"
        "这里保留 1.25 V，但这个示例并不表示所有输入都适用"
    )
    assert raw["term"] == "Register (寄存器)"
    assert raw["definition"].endswith("。")
