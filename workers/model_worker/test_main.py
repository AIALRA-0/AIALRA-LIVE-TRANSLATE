"""Deterministic tests cover local fallbacks and page extraction without model downloads."""

from __future__ import annotations

import asyncio
import io

import httpx
import numpy as np
import pytest
from pptx import Presentation

import workers.model_worker.main as model_worker
from workers.model_worker.main import (
    ASR_CPU_THREADS,
    EvidencePage,
    EvidenceSegment,
    ExplanationRequest,
    ExplanationResponse,
    SummaryRequest,
    _audio_has_speech,
    _clean_translation_output,
    _dedupe_text_items,
    _has_explanation_shape,
    _has_nonempty_list,
    _has_nonempty_string,
    _ollama_model_uses_gpu,
    _parse_asset_sync,
    _parse_model_json,
    _restore_realtime_translation_model,
    _translation_contract_ok,
    _translation_text_contract_ok,
    _uses_requested_explanation_language,
)


def test_asr_cpu_threads_stays_within_safe_host_bounds() -> None:
    assert 0 <= ASR_CPU_THREADS <= 32


def test_ollama_gpu_residency_requires_configured_model_and_near_full_vram() -> None:
    configured = {"name": model_worker.OLLAMA_MODEL, "size": 2_000, "size_vram": 1_900}
    assert _ollama_model_uses_gpu({"models": [configured]})
    assert not _ollama_model_uses_gpu({"models": [{**configured, "size_vram": 1_000}]})
    assert not _ollama_model_uses_gpu({"models": [{**configured, "name": "other"}]})


def _test_only_explanation(request: ExplanationRequest) -> ExplanationResponse:
    """Build deterministic evidence for unit tests without registering a runtime provider."""

    segment_ids = [segment.id for segment in request.segments]
    page_ids = [page.id for page in request.asset_pages]
    return ExplanationResponse(
        paragraph_summary=request.segments[-1].text,
        terms=[],
        evidence_segment_ids=segment_ids,
        asset_page_ids=page_ids,
        provider="test_only",
    )


def test_explanation_fallback_keeps_all_supplied_evidence_ids() -> None:
    """A failed LLM call still links the card to the stable segment and uploaded page."""

    request = ExplanationRequest(
        segments=[EvidenceSegment(id="seg_1", text="Forwarding reduces stalls.")],
        asset_pages=[EvidencePage(id="page_1", title="Pipeline hazards", text="RAW hazard")],
        target_language="zh-CN",
    )
    result = _test_only_explanation(request)
    assert result.evidence_segment_ids == ["seg_1"]
    assert result.asset_page_ids == ["page_1"]


def test_pptx_parser_emits_stable_page_order_and_text() -> None:
    """A generated fixture verifies page order without private course files."""

    presentation = Presentation()
    first = presentation.slides.add_slide(presentation.slide_layouts[1])
    first.shapes.title.text = "Pipeline"
    first.placeholders[1].text = "Forwarding"
    second = presentation.slides.add_slide(presentation.slide_layouts[1])
    second.shapes.title.text = "Cache"
    second.placeholders[1].text = "Locality"
    buffer = io.BytesIO()
    presentation.save(buffer)

    result = _parse_asset_sync(".pptx", buffer.getvalue())
    assert [page.page_number for page in result.pages] == [1, 2]
    assert result.pages[0].title == "Pipeline"
    assert "Locality" in result.pages[1].text


def test_asyncio_is_available_for_worker_runtime() -> None:
    """The selected Python runtime can create an event loop for FastAPI inference calls."""

    assert asyncio.run(asyncio.sleep(0, result=True)) is True


def test_translation_shape_requires_clean_source_and_target_text() -> None:
    valid = {"source_text": "Attention uses context.", "translation": "注意力使用上下文"}
    assert _has_nonempty_string(valid, "source_text")
    assert _has_nonempty_string(valid, "translation")
    assert not _has_nonempty_string({"translation": "翻译结果"}, "source_text")
    assert not _has_nonempty_string({"source_text": "  "}, "source_text")


def test_translation_contract_rejects_source_language_drift_and_same_language_copy() -> None:
    assert _translation_contract_ok(
        {"source_text": "Attention uses context.", "translation": "注意力使用上下文"},
        "en",
        "zh-CN",
    )
    assert not _translation_contract_ok(
        {"source_text": "注意力使用上下文", "translation": "注意力使用上下文"},
        "en",
        "zh-CN",
    )
    assert not _translation_contract_ok(
        {"source_text": "Attention uses context.", "translation": "Attention uses context."},
        "en",
        "zh-CN",
    )


def test_translation_contract_allows_configured_same_language_output() -> None:
    assert _translation_contract_ok(
        {"source_text": "Attention uses context.", "translation": "Attention uses context."},
        "en",
        "en",
    )


def test_dedicated_translation_path_returns_plain_provider_result(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(model_worker, "TRANSLATION_PROVIDER", "hy-mt")
    monkeypatch.setattr(model_worker, "HYMT_MODEL", "test/hy-mt")
    monkeypatch.setattr(model_worker, "HYMT_DEVICE", "cuda")
    monkeypatch.setattr(model_worker, "_configured_translation_importable", lambda: True)
    monkeypatch.setattr(model_worker, "_translate_hymt_sync", lambda _request: "注意力使用上下文。")

    result = asyncio.run(
        model_worker.translate(
            model_worker.TranslationRequest(
                text="Attention uses context.",
                source_language="en",
                target_language="zh-CN",
            )
        )
    )

    assert result.source_text == "Attention uses context."
    assert result.text == "注意力使用上下文。"
    assert result.provider == "hy-mt:test/hy-mt@cuda"


def test_same_language_translation_is_an_identity_result_without_model_call(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        model_worker,
        "_translate_hymt_sync",
        lambda _request: pytest.fail("same-language input must not call a translator"),
    )
    result = asyncio.run(
        model_worker.translate(
            model_worker.TranslationRequest(
                text="Attention uses context.",
                source_language="en",
                target_language="en-US",
            )
        )
    )
    assert result.text == result.source_text
    assert result.provider == "identity:en@cpu"


def test_qwen_asr_path_uses_the_configured_language_and_provider(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class Result:
        text = "Attention uses context."
        language = "English"

    class Model:
        def transcribe(self, **kwargs: object) -> list[Result]:
            assert kwargs["language"] == "English"
            assert kwargs["context"] == ""
            return [Result()]

    monkeypatch.setattr(model_worker, "ASR_PROVIDER", "qwen3-asr")
    monkeypatch.setattr(model_worker, "ASR_MODEL_NAME", "Qwen/Qwen3-ASR-1.7B")
    monkeypatch.setattr(model_worker, "ASR_DEVICE", "cuda")
    monkeypatch.setattr(model_worker, "_get_qwen_asr_model", lambda: Model())
    request = model_worker.AsrRequest(
        pcm_s16le_base64="AA==",
        sample_rate=16_000,
        language="en-US",
    )
    audio = np.zeros(1, dtype=np.float32)

    result = model_worker._transcribe_sync(audio, request)

    assert result.provider == "qwen3-asr:Qwen/Qwen3-ASR-1.7B@cuda"
    assert result.text == "Attention uses context."


def test_silent_audio_is_rejected_before_provider_inference() -> None:
    assert not _audio_has_speech(np.zeros(16_000, dtype=np.float32), 16_000)


def test_quiet_speech_like_audio_is_not_rejected_as_silence() -> None:
    samples = np.arange(16_000, dtype=np.float32)
    quiet_voice = (0.01 * np.sin(2 * np.pi * 180 * samples / 16_000)).astype(np.float32)
    envelope = np.zeros(16_000, dtype=np.float32)
    envelope[2_000:7_000] = np.linspace(0.2, 1.0, 5_000, dtype=np.float32)
    envelope[9_000:14_000] = np.linspace(1.0, 0.2, 5_000, dtype=np.float32)
    assert _audio_has_speech(quiet_voice * envelope, 16_000)


def test_short_provider_probe_remains_compatible_with_unit_inputs() -> None:
    assert _audio_has_speech(np.zeros(1, dtype=np.float32), 16_000)


def test_translation_contract_normalizes_region_codes_and_unicode_latin() -> None:
    assert _translation_contract_ok(
        {"source_text": "Café déjà vu.", "translation": "咖啡似曾相识。"},
        "en-US",
        "zh-CN",
    )
    assert not _translation_contract_ok(
        {"source_text": "中文内容", "translation": "中文内容"},
        "en-US",
        "zh-CN",
    )


def test_auto_source_still_requires_the_requested_target_language() -> None:
    assert _translation_text_contract_ok("注意力使用上下文。", "auto", "zh-CN")
    assert not _translation_text_contract_ok("Attention uses context.", "auto", "zh-CN")


def test_translation_prompt_keeps_source_and_translation_fields_separate(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, object] = {}
    monkeypatch.setattr(model_worker, "TRANSLATION_PROVIDER", "ollama")

    async def ollama_json(
        system: str,
        user: str,
        _schema: dict[str, object],
        **kwargs: object,
    ) -> dict[str, str]:
        captured["system"] = system
        captured["user"] = user
        captured.update(kwargs)
        return {"source_text": "Attention uses context.", "translation": "注意力使用上下文。"}

    monkeypatch.setattr(model_worker, "_ollama_json", ollama_json)
    request = model_worker.TranslationRequest(
        text="Attention uses context.",
        source_language="en",
        target_language="zh-CN",
    )
    result = asyncio.run(model_worker.translate(request))

    assert result.source_text == request.text
    assert result.text == "注意力使用上下文。"
    assert "source_text field is a cleaned copy" in str(captured["system"])
    assert "translate only the translation field" in str(captured["repair_instruction"])


def test_explanation_shape_requires_summary_and_valid_terms() -> None:
    valid = {
        "paragraph_summary": "简短总结",
        "terms": [{"term": "GPU", "explanation": "图形处理器。"}],
    }
    assert _has_explanation_shape(valid)
    assert _has_explanation_shape({"summary": "兼容旧结果"})
    assert not _has_explanation_shape({**valid, "terms": "none"})
    assert not _has_explanation_shape({**valid, "terms": [{"term": "GPU"}]})


def test_model_json_accepts_unescaped_newline_inside_string() -> None:
    parsed = _parse_model_json('{"text":"first line\nsecond line"}')
    assert parsed == {"text": "first line\nsecond line"}


def test_model_json_rejects_non_json_or_non_object_output() -> None:
    assert _parse_model_json("translation without JSON") is None
    assert _parse_model_json('["translation"]') is None


def test_translation_contract_rejects_language_metadata_leaking_into_display() -> None:
    leaked = "源语言：en\n目标语言：zh-CN\n术语：\n这是译文。"
    assert _clean_translation_output(leaked) == "这是译文。"
    assert _translation_text_contract_ok(leaked, "en", "zh-CN")
    assert _translation_contract_ok(
        {"source_text": "Source language: en\nAttention uses context.", "translation": leaked},
        "en",
        "zh-CN",
    )


def test_translation_cleaner_removes_multilingual_labels_without_losing_same_line_text() -> None:
    leaked = "之前的术语背景仅用于说明：是的。\n翻译后的文本：这是译文。"
    assert _clean_translation_output(leaked) == "这是译文。"
    assert not model_worker._contains_translation_metadata(_clean_translation_output(leaked))


def test_chinese_explanation_requires_chinese_summary() -> None:
    assert _uses_requested_explanation_language({"summary": "中文总结"}, "zh-CN")
    assert not _uses_requested_explanation_language({"summary": "English summary"}, "zh-CN")
    assert _uses_requested_explanation_language({"summary": "English summary"}, "en")


def test_background_model_restores_translation_before_releasing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[str] = []
    monkeypatch.setattr(model_worker, "TRANSLATION_PROVIDER", "ollama")

    async def unload(model: str) -> None:
        calls.append(f"unload:{model}")

    class Response:
        def raise_for_status(self) -> None:
            calls.append(f"loaded:{model_worker.TRANSLATION_MODEL}")

    class Client:
        async def __aenter__(self) -> Client:
            return self

        async def __aexit__(self, *_args: object) -> None:
            return None

        async def post(self, _url: str, *, json: dict[str, object]) -> Response:
            assert json["model"] == model_worker.TRANSLATION_MODEL
            return Response()

    async def resident(model: str) -> bool:
        calls.append(f"resident:{model}")
        return True

    monkeypatch.setattr(model_worker, "_unload_ollama_model", unload)
    monkeypatch.setattr(httpx, "AsyncClient", lambda **_kwargs: Client())
    monkeypatch.setattr(model_worker, "_ollama_gpu_resident", resident)

    background_model = "qwen2.5:14b-instruct"
    asyncio.run(_restore_realtime_translation_model(background_model))

    assert calls == [
        f"unload:{background_model}",
        f"loaded:{model_worker.TRANSLATION_MODEL}",
        f"resident:{model_worker.TRANSLATION_MODEL}",
    ]


def test_summary_contract_accepts_the_core_rolling_summary_limit() -> None:
    request = SummaryRequest(
        segments=[EvidenceSegment(id="segment-1", text="Forwarding reduces stalls.")],
        rolling_summaries=[f"rolling-{index}" for index in range(24)],
        target_language="zh-CN",
    )
    assert len(request.rolling_summaries) == 24


def test_summary_required_lists_cannot_be_empty() -> None:
    assert _has_nonempty_list({"key_points": ["Pipeline hazards reduce throughput."]}, "key_points")
    assert not _has_nonempty_list({"key_points": []}, "key_points")
    assert not _has_nonempty_list({}, "key_points")


def test_summary_sections_drop_repeated_points_without_reordering() -> None:
    assert _dedupe_text_items(["  Pipeline stalls  ", "pipeline stalls", "Cache locality"]) == [
        "Pipeline stalls",
        "Cache locality",
    ]
