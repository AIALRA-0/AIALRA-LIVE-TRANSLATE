"""Teaching uses bounded source-only names and rejects missing evidence."""

import json
from typing import Any, Literal

import pytest
from pydantic import ValidationError

from workers.model_worker.teaching import (
    TeachingPartRequest,
    bound_inventory,
    generate_part,
    source_surface,
    valid_part,
)
from workers.model_worker.terminology import matching_technical_terms


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


def test_source_uncertainty_is_not_rejected_to_make_prose_sound_certain() -> None:
    request = TeachingPartRequest(
        phase="prose", text="The comparison is unclear. We have no final measurement.",
        target_language="zh-CN",
    )
    payload = {"prose": "当前比较尚不明确，还没有最终测量值", "original_terms": []}
    assert valid_part(payload, request)


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
        assert "Do not hide that uncertainty" in system
        return {"prose": "锁存器保存数据", "original_terms": ["latch"]}

    result = await generate_part(TeachingPartRequest(
        phase="prose", text="A latch.", target_language="zh-CN",
    ), infer, "test", "cuda")
    assert result is not None and result.provider == "ollama:test@cuda"


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
        return {"term": "FM 算法", "definition": "电路划分算法"}

    result = await generate_part(TeachingPartRequest(
        phase=phase, text="FM is the algorithm used here.", context=context,
        original_term="FM" if phase == "definition" else None, target_language="zh-CN",
    ), infer, "test", "cuda")
    assert result is not None
    if phase == "prose":
        assert result.original_terms == ["FM"]


@pytest.mark.asyncio
async def test_generated_definition_style_does_not_change_source_or_raw_result() -> None:
    raw = {"term": "Register (寄存器)", "definition": "保留 1.25 V。不是所有输入都适用。"}

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
    assert result.definition == "保留 1.25 V；不是所有输入都适用"
    assert raw["term"] == "Register (寄存器)"
    assert raw["definition"].endswith("。")
