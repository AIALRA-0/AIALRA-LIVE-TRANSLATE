"""Deterministic tests cover local fallbacks and page extraction without model downloads."""

from __future__ import annotations

import asyncio
import base64
import io
import json
import threading
import time
from typing import Any

import httpx
import numpy as np
import pytest
from fastapi import HTTPException
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
from workers.model_worker.speakers import SpeakerObservation


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


@pytest.mark.asyncio
@pytest.mark.parametrize("first", [
    {"done": True, "done_reason": "length", "message": {"content": '{"overview":"partial"}'}},
    {"done": False, "done_reason": "stop", "message": {"content": '{"overview":"partial"}'}},
    {"done": True, "message": {"content": '{"overview":"partial"}'}},
    {"done": True, "done_reason": "stop", "message": None},
    "invalid-json-body",
])
async def test_ollama_json_retries_incomplete_or_invalid_envelopes(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture, first: object,
) -> None:
    replies = [first, {
        "done": True, "done_reason": "stop", "message": {"content": '{"overview":"complete"}'},
    }]
    calls: list[httpx.Request] = []
    original_client = httpx.AsyncClient

    def respond(request: httpx.Request) -> httpx.Response:
        calls.append(request)
        reply = replies.pop(0)
        return httpx.Response(200, content=(
            reply if isinstance(reply, str) else json.dumps(reply)
        ))

    monkeypatch.setattr(httpx, "AsyncClient", lambda **kwargs: original_client(
        **kwargs, transport=httpx.MockTransport(respond),
    ))
    result = await model_worker._ollama_json("instruction", "synthetic input", attempts=2)
    assert result == {"overview": "complete"}
    assert len(calls) == 2
    assert "model_response_rejected" in caplog.text
    assert "partial" not in caplog.text
    assert "synthetic input" not in caplog.text


@pytest.mark.asyncio
async def test_ollama_json_does_not_accept_parseable_truncated_summary(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture,
) -> None:
    original_client = httpx.AsyncClient
    unloaded: list[str] = []

    def respond(_request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={
            "done": True, "done_reason": "length",
            "message": {"content": '{"overview":"private synthetic incomplete content"}'},
        })

    async def unload(model: str) -> None:
        unloaded.append(model)

    monkeypatch.setattr(httpx, "AsyncClient", lambda **kwargs: original_client(
        **kwargs, transport=httpx.MockTransport(respond),
    ))
    monkeypatch.setattr(model_worker, "_unload_ollama_model", unload)
    result = await model_worker._ollama_json(
        "instruction", "synthetic input", attempts=1, model="synthetic-model", unload_after=True,
    )
    assert result is None
    assert unloaded == ["synthetic-model"]
    assert "output_truncated" in caplog.text
    assert "private synthetic" not in caplog.text


@pytest.mark.asyncio
@pytest.mark.parametrize("timeout", [True, False])
async def test_ollama_failure_logs_only_category_and_status(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture, timeout: bool,
) -> None:
    original_client = httpx.AsyncClient

    def respond(request: httpx.Request) -> httpx.Response:
        if timeout:
            raise httpx.ReadTimeout("synthetic-secret-path", request=request)
        return httpx.Response(502, text="synthetic-private-upstream-body")

    monkeypatch.setattr(httpx, "AsyncClient", lambda **kwargs: original_client(
        **kwargs, transport=httpx.MockTransport(respond),
    ))
    assert await model_worker._ollama_json("system", "synthetic-private-source", attempts=1) is None
    assert ("request_timeout" if timeout else "status=502") in caplog.text
    assert "synthetic-secret" not in caplog.text
    assert "synthetic-private" not in caplog.text


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
        initial_prompt="Previous recognition may contain errors and must not be repeated.",
    )
    samples = np.arange(1600, dtype=np.float32)
    audio = (0.01 * np.sin(2 * np.pi * 180 * samples / 16_000)).astype(np.float32)

    result = model_worker._transcribe_sync(audio, request)

    assert result.provider == "qwen3-asr:Qwen/Qwen3-ASR-1.7B@cuda"
    assert result.text == "Attention uses context."


def test_nominal_same_language_still_translates_a_sustained_foreign_script(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(model_worker, "TRANSLATION_PROVIDER", "hy-mt")
    monkeypatch.setattr(model_worker, "_configured_translation_importable", lambda: True)
    monkeypatch.setattr(
        model_worker,
        "_translate_hymt_sync",
        lambda _request: "这里说明流水线冒险以及如何通过转发解决。",
    )
    result = asyncio.run(model_worker.translate(model_worker.TranslationRequest(
        text="ここではパイプラインハザードとフォワーディングによる解決方法を説明します。",
        source_language="zh",
        target_language="zh-CN",
    )))
    assert result.text == "这里说明流水线冒险以及如何通过转发解决。"
    assert result.provider.startswith("hy-mt:")


def test_same_language_keeps_short_embedded_technical_terms_without_translation() -> None:
    assert not model_worker._text_requires_translation("这个 GPU 使用 CUDA kernel。", "zh-CN")
    assert model_worker._text_requires_translation(
        "The complete lecture passage is delivered in English rather than Chinese.", "zh-CN"
    )


def test_silent_audio_is_rejected_before_provider_inference() -> None:
    assert not _audio_has_speech(np.zeros(16_000, dtype=np.float32), 16_000)


def test_quiet_speech_like_audio_is_not_rejected_as_silence() -> None:
    samples = np.arange(16_000, dtype=np.float32)
    quiet_voice = (0.01 * np.sin(2 * np.pi * 180 * samples / 16_000)).astype(np.float32)
    envelope = np.zeros(16_000, dtype=np.float32)
    envelope[2_000:7_000] = np.linspace(0.2, 1.0, 5_000, dtype=np.float32)
    envelope[9_000:14_000] = np.linspace(1.0, 0.2, 5_000, dtype=np.float32)
    assert _audio_has_speech(quiet_voice * envelope, 16_000)


@pytest.mark.parametrize("length", [1, 160, 320, 3999])
def test_short_silent_stop_tail_does_not_reach_provider(
    length: int, monkeypatch: pytest.MonkeyPatch,
) -> None:
    def unexpected_model_load() -> None:
        pytest.fail("silent tail reached ASR")
    monkeypatch.setattr(model_worker, "_get_qwen_asr_model", unexpected_model_load)
    monkeypatch.setattr(model_worker, "_get_asr_model", unexpected_model_load)
    request = model_worker.AsrRequest(pcm_s16le_base64="AAAA", sample_rate=16000, language="en")
    for audio in [np.zeros(length, dtype=np.float32), np.full(length, 0.02, dtype=np.float32)]:
        result = model_worker._transcribe_sync(audio, request)
        assert result.text == "" and result.duration_ms == length * 1000 // 16000


def test_short_weak_non_silent_tail_is_not_discarded() -> None:
    for length in [160, 320, 1600, 3999]:
        samples = np.arange(length, dtype=np.float32)
        voice = (0.004 * np.sin(2 * np.pi * 250 * samples / 16000)).astype(np.float32)
        assert _audio_has_speech(voice, 16000)
    assert not _audio_has_speech(np.array([np.nan, np.inf, -np.inf], dtype=np.float32), 16000)


@pytest.mark.asyncio
async def test_stop_tail_http_contract_returns_empty_success_without_model_load(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(model_worker, "_configured_asr_importable", lambda: True)
    def unexpected_model_load() -> None:
        pytest.fail("loaded for silence")
    monkeypatch.setattr(model_worker, "_get_qwen_asr_model", unexpected_model_load)
    monkeypatch.setattr(model_worker, "_get_asr_model", unexpected_model_load)
    monkeypatch.setattr(
        model_worker, "observe_speaker", lambda *_: SpeakerObservation(status="unavailable"),
    )
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=model_worker.app), base_url="http://candidate.invalid",
    ) as client:
        response = await client.post("/v1/asr/transcribe", json={
            "pcm_s16le_base64": base64.b64encode(bytes(320)).decode(),
            "sample_rate": 16000, "language": "en",
        })
    assert response.status_code == 200
    assert response.json()["text"] == ""
    assert response.json()["duration_ms"] == 10


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


def test_translation_prompt_binds_immutable_source_and_separate_context(
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
        # Even an unexpected model field cannot change the trusted source.
        return {"source_text": "Invented source.", "translation": "注意力使用上下文。"}

    monkeypatch.setattr(model_worker, "_ollama_json", ollama_json)
    request = model_worker.TranslationRequest(
        text="Attention uses context.",
        source_language="en",
        target_language="zh-CN",
        context=["Prior paragraph."],
    )
    result = asyncio.run(model_worker.translate(request))

    assert result.source_text == request.text
    assert result.text == "注意力使用上下文。"
    assert "source is immutable" in str(captured["system"])
    assert "Return only the translation field" in str(captured["repair_instruction"])
    payload = json.loads(str(captured["user"]))
    assert payload["source_text"] == request.text
    assert payload["context_for_reference_only"] == ["Prior paragraph."]
    assert captured["num_ctx"] == 4096
    assert captured["presence_penalty"] == 0.0
    assert captured["thinking"] == model_worker.TRANSLATION_THINK


@pytest.mark.asyncio
async def test_model_generation_options_are_explicit_only_when_requested(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    original_client = httpx.AsyncClient
    payloads: list[dict[str, object]] = []

    def respond(request: httpx.Request) -> httpx.Response:
        payloads.append(json.loads(request.content))
        return httpx.Response(200, json={
            "done": True, "done_reason": "stop", "message": {"content": '{"ok":true}'},
        })

    monkeypatch.setattr(httpx, "AsyncClient", lambda **kwargs: original_client(
        **kwargs, transport=httpx.MockTransport(respond),
    ))
    await model_worker._ollama_json("instruction", "synthetic input", attempts=1)
    await model_worker._ollama_json(
        "instruction", "synthetic input", attempts=1,
        thinking=False, presence_penalty=0.0, num_ctx=4096, max_tokens=600,
    )
    assert "think" not in payloads[0]
    assert payloads[1]["think"] is False
    assert payloads[1]["options"] == {
        "temperature": 0, "seed": 0, "num_predict": 600,
        "presence_penalty": 0.0, "num_ctx": 4096,
    }


def test_translation_candidate_acceptance_uses_original_source(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(model_worker, "TRANSLATION_PROVIDER", "ollama")

    async def infer(
        _system: str, _user: str, schema: dict[str, object], **kwargs: object,
    ) -> dict[str, str]:
        assert schema["required"] == ["translation"]
        accept = kwargs["accept"]
        assert callable(accept)
        assert accept({"translation": "不要重用之前的电压值"})
        assert not accept({"translation": "Do not reuse the previous voltage."})
        assert not accept({"translation": ""})
        return {"translation": "不要重用之前的电压值"}

    monkeypatch.setattr(model_worker, "_ollama_json", infer)
    source = "Do not reuse the previous voltage."
    result = asyncio.run(model_worker.translate(model_worker.TranslationRequest(
        text=source, source_language="en", target_language="zh-CN",
    )))
    assert result.source_text == source


def test_explanation_shape_requires_summary_and_valid_terms() -> None:
    valid = {
        "paragraph_summary": "简短总结",
        "terms": [{"term": "GPU", "explanation": "图形处理器。"}],
    }
    assert _has_explanation_shape(valid)
    assert _has_explanation_shape({"summary": "兼容旧结果"})
    assert not _has_explanation_shape({**valid, "terms": "none"})
    assert not _has_explanation_shape({**valid, "terms": [{"term": "GPU"}]})


def test_term_evidence_uses_quoted_source_not_last_two_paragraphs() -> None:
    request = ExplanationRequest(segments=[
        EvidenceSegment(id="first", text="A cache stores reusable data."),
        EvidenceSegment(id="second", text="A clock synchronizes operations."),
        EvidenceSegment(id="third", text="The lecture now ends."),
    ], asset_pages=[EvidencePage(id="page", title="Memory", text="SRAM is static memory.")],
        target_language="zh-CN")
    raw = {"sections": [{"source_indexes": [0, 1, 2],
                         "explanation": "缓存保存可重复使用的数据；时钟协调操作。"}], "terms": [
        {"term": "缓存", "explanation": "用于复用数据的存储，避免重复获取。",
         "evidence": [{"kind": "segment", "index": 0, "quote": "A cache stores reusable data."}]},
        {"term": "SRAM", "explanation": "静态随机存取存储器。",
         "evidence": [{"kind": "page", "index": 0, "quote": "SRAM is static memory."}]},
    ]}
    bound = model_worker._bind_explanation_sources(raw, request)
    assert bound is not None
    assert bound["terms"][0]["evidence_segment_ids"] == ["first"]
    assert bound["terms"][0]["asset_page_ids"] == []
    assert bound["terms"][1]["evidence_segment_ids"] == []
    assert bound["terms"][1]["asset_page_ids"] == ["page"]


@pytest.mark.parametrize("evidence", [
    [], [{"kind": "segment", "index": -1, "quote": "cache"}],
    [{"kind": "segment", "index": True, "quote": "cache"}],
    [{"kind": "segment", "index": 9, "quote": "cache"}],
    [{"kind": "segment", "index": 0, "quote": "unspoken fact"}],
    [{"kind": "segment", "index": 0, "quote": "  "}],
    [{"kind": "other-course", "index": 0, "quote": "cache"}],
])
def test_term_evidence_rejects_invalid_sources_without_silently_dropping_terms(
    evidence: object,
) -> None:
    request = ExplanationRequest(
        segments=[EvidenceSegment(id="first", text="A cache stores reusable data.")],
        target_language="zh-CN",
    )
    assert model_worker._bind_explanation_sources({
        "sections": [{"source_indexes": [0], "explanation": "缓存复用数据。"}],
        "terms": [{"term": "缓存", "explanation": "复用数据的存储。", "evidence": evidence}],
    }, request) is None


def test_explanation_can_keep_complete_summary_after_term_evidence_repair_fails() -> None:
    request = ExplanationRequest(
        segments=[EvidenceSegment(id="first", text="A cache stores reusable data.")],
        target_language="zh-CN",
    )
    bound = model_worker._bind_explanation_sources({
        "sections": [{"source_indexes": [0], "explanation": "缓存用于保存可复用的数据。"}],
        "terms": [{
            "term": "缓存", "explanation": "保存数据的存储层。",
            "evidence": [{"kind": "segment", "index": 0, "quote": "paraphrased quote"}],
        }],
    }, request, drop_invalid_terms=True)
    assert bound is not None
    assert bound["paragraph_summary"] == "缓存用于保存可复用的数据。"
    assert bound["terms"] == []


@pytest.mark.parametrize("indexes", [[0], [0, 0], [1, 0], [0, 2], [0, True]])
def test_explanation_rejects_missing_duplicated_or_reordered_source_coverage(
    indexes: list[object],
) -> None:
    request = ExplanationRequest(segments=[
        EvidenceSegment(id="one", text="Setup time is before sampling."),
        EvidenceSegment(id="two", text="Hold time is after sampling."),
    ], target_language="zh-CN")
    assert model_worker._bind_explanation_sources({
        "sections": [{"source_indexes": indexes, "explanation": "建立与保持时间。"}],
        "terms": [],
    }, request) is None


def test_explanation_preserves_multiple_complete_paragraphs() -> None:
    request = ExplanationRequest(segments=[
        EvidenceSegment(id="one", text="Setup time is before sampling."),
        EvidenceSegment(id="two", text="Hold time is after sampling."),
    ], target_language="zh-CN")
    bound = model_worker._bind_explanation_sources({
        "sections": [
            {"source_indexes": [0], "explanation": "建立时间在采样之前。"},
            {"source_indexes": [1], "explanation": "保持时间在采样之后。"},
        ], "terms": [],
    }, request)
    assert bound is not None
    assert bound["paragraph_summary"] == "建立时间在采样之前。\n\n保持时间在采样之后。"


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
@pytest.mark.asyncio
async def test_cancelled_http_request_keeps_gpu_exclusive_until_actual_completion() -> None:
    entered = asyncio.Event()
    finish = asyncio.Event()
    model_worker._gpu_inflight = None

    @model_worker.single_gpu_call
    async def inference() -> int:
        entered.set()
        await finish.wait()
        return 7

    caller = asyncio.create_task(inference())
    await entered.wait()
    caller.cancel()
    with pytest.raises(asyncio.CancelledError):
        await caller
    with pytest.raises(HTTPException) as busy:
        await inference()
    assert busy.value.status_code == 503
    finish.set()
    assert model_worker._gpu_inflight is not None
    await model_worker._gpu_inflight
    assert await inference() == 7


def test_hymt_keeps_bounded_history_separate_from_the_current_translation() -> None:
    request = model_worker.TranslationRequest(
        text="The voltage is not 5 V.", source_language="en", target_language="zh-CN",
        context=["prior context " * 300], glossary=[],
    )
    prompt = model_worker._hymt_prompt(request)
    assert prompt.endswith(request.text)
    assert "prior context" in prompt
    assert "不得翻译、复述或带入上文事实" in prompt
    assert len(prompt) < 1800
    assert "Source language:" not in prompt
    assert "Text to translate:" not in prompt
    request.glossary = [model_worker.GlossaryConstraint(source="voltage", preferred="电压")]
    assert "voltage 翻译成 电压" in model_worker._hymt_prompt(request)


@pytest.mark.asyncio
async def test_shared_resident_gate_bounds_asr_and_llm_and_keeps_cancelled_work_alive(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(model_worker, "_shared_resident_models", lambda: True)
    monkeypatch.setattr(model_worker, "_active_gpu_calls", {})
    asr_entered, llm_entered = asyncio.Event(), asyncio.Event()
    asr_finish, llm_finish = asyncio.Event(), asyncio.Event()

    @model_worker.single_gpu_call
    async def transcribe() -> None:
        asr_entered.set()
        await asr_finish.wait()

    @model_worker.single_gpu_call
    async def explain() -> None:
        llm_entered.set()
        await llm_finish.wait()

    @model_worker.single_gpu_call
    async def warmup() -> None:
        pass

    background = asyncio.create_task(explain())
    await llm_entered.wait()
    recognition = asyncio.create_task(transcribe())
    await asyncio.wait_for(asr_entered.wait(), timeout=1)
    try:
        for rejected in [transcribe, explain, warmup]:
            with pytest.raises(HTTPException) as busy:
                await rejected()
            assert busy.value.headers is not None
            assert busy.value.headers["X-Aialra-Worker-State"] == "busy"
        recognition.cancel()
        with pytest.raises(asyncio.CancelledError):
            await recognition
        llm_finish.set()
        await background
        with pytest.raises(HTTPException):
            await warmup()
    finally:
        asr_finish.set()
        llm_finish.set()
        await asyncio.gather(*model_worker._active_gpu_calls.values())
    await warmup()


def test_shared_resident_gate_requires_the_qualified_model_layout(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AIALRA_SHARED_RESIDENT_MODELS", "true")
    monkeypatch.setattr(model_worker, "ASR_PROVIDER", "qwen3-asr")
    monkeypatch.setattr(model_worker, "ASR_DEVICE", "cuda")
    monkeypatch.setattr(model_worker, "TRANSLATION_PROVIDER", "ollama")
    for key in ["TRANSLATION_MODEL", "TOPIC_MODEL", "EXPLANATION_MODEL", "SUMMARY_MODEL"]:
        monkeypatch.setattr(model_worker, key, "qualified-shared-model")
    assert model_worker._shared_resident_models()
    monkeypatch.setattr(model_worker, "SUMMARY_MODEL", "different-large-model")
    assert not model_worker._shared_resident_models()
    assert model_worker._inference_lane("transcribe") == "exclusive"


@pytest.mark.asyncio
async def test_health_does_not_hide_overdue_llm_when_asr_starts(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def available(*_args: object) -> bool:
        return True

    async def pending() -> None:
        await asyncio.Event().wait()

    tasks = {lane: asyncio.create_task(pending()) for lane in ["llm", "asr"]}
    monkeypatch.setattr(model_worker, "_configured_asr_importable", lambda: True)
    monkeypatch.setattr(model_worker, "_configured_translation_importable", lambda: True)
    monkeypatch.setattr(model_worker, "_ollama_available", available)
    monkeypatch.setattr(model_worker, "_ollama_gpu_resident", available)
    monkeypatch.setattr(model_worker, "_active_gpu_calls", tasks)
    monkeypatch.setattr(model_worker, "_active_gpu_started", {
        "llm": time.monotonic() - 361, "asr": time.monotonic(),
    })
    try:
        state = await model_worker.health()
        assert state.inference_busy and state.inference_overdue
    finally:
        for task in tasks.values():
            task.cancel()
        await asyncio.gather(*tasks.values(), return_exceptions=True)


@pytest.mark.asyncio
async def test_optional_speaker_work_runs_with_asr_and_cannot_discard_transcript(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    entered = threading.Barrier(2, timeout=2)

    def recognize(*_args: Any) -> model_worker.AsrResponse:
        entered.wait()
        return model_worker.AsrResponse(text="Synthetic speech", language="en", confidence=1,
                                       duration_ms=2000, provider="qwen3-asr:test@cuda")

    def identify(*_args: Any) -> SpeakerObservation:
        entered.wait()
        raise RuntimeError("synthetic-private-model-path")

    monkeypatch.setattr(model_worker, "_configured_asr_importable", lambda: True)
    monkeypatch.setattr(model_worker, "_transcribe_sync", recognize)
    monkeypatch.setattr(model_worker, "observe_speaker", identify)
    result = await model_worker.transcribe(model_worker.AsrRequest(
        pcm_s16le_base64=base64.b64encode(bytes(64000)).decode(), sample_rate=16000, language="en",
    ))
    assert result.text == "Synthetic speech"
    assert result.speaker_observation == SpeakerObservation(status="unavailable")


@pytest.mark.asyncio
async def test_interim_asr_skips_optional_speaker_work(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def recognize(*_args: Any) -> model_worker.AsrResponse:
        return model_worker.AsrResponse(text="Synthetic preview", language="en", confidence=1,
                                       duration_ms=2000, provider="qwen3-asr:test@cuda")

    def identify(*_args: Any) -> SpeakerObservation:
        raise AssertionError("interim ASR must not run speaker observation")

    monkeypatch.setattr(model_worker, "_configured_asr_importable", lambda: True)
    monkeypatch.setattr(model_worker, "_transcribe_sync", recognize)
    monkeypatch.setattr(model_worker, "observe_speaker", identify)
    result = await model_worker.transcribe(model_worker.AsrRequest(
        pcm_s16le_base64=base64.b64encode(bytes(64000)).decode(), sample_rate=16000,
        language="en", result_mode="interim",
    ))
    assert result.text == "Synthetic preview"
    assert result.speaker_observation is None


@pytest.mark.asyncio
async def test_shared_llm_response_cannot_claim_cuda_without_residency_proof(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture,
) -> None:
    original_client = httpx.AsyncClient
    proven = False

    def respond(_request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"done": True, "done_reason": "stop",
                                       "message": {"content": '{"text":"synthetic content"}'}})

    async def resident(model: str) -> bool:
        assert model == "synthetic-model"
        return proven

    monkeypatch.setattr(model_worker, "_shared_resident_models", lambda: True)
    monkeypatch.setattr(model_worker, "_ollama_gpu_resident", resident)
    monkeypatch.setattr(httpx, "AsyncClient", lambda **kwargs: original_client(
        **kwargs, transport=httpx.MockTransport(respond),
    ))
    assert await model_worker._ollama_json("system", "source", model="synthetic-model",
                                          attempts=1) is None
    assert "cuda_residency_unproven" in caplog.text
    assert "synthetic content" not in caplog.text
    proven = True
    assert await model_worker._ollama_json("system", "source", model="synthetic-model",
                                          attempts=1) == {"text": "synthetic content"}


def test_reviewed_glossary_requires_a_full_concept_and_keeps_explicit_choice() -> None:
    request = model_worker.TranslationRequest(
        text="Requests to the same bank may wait.", source_language="en", target_language="zh-CN",
        context=["The controller uses multiple memory banks."],
    )
    terms = model_worker._translation_glossary(request)
    assert [(term.source, term.preferred) for term in terms] == [
        ("memory bank", "存储体"),
    ]
    assert request.glossary == []
    request.glossary = [model_worker.GlossaryConstraint(source="memory bank", preferred="存储分区")]
    assert len(model_worker._translation_glossary(request)) == 1
    assert model_worker._translation_glossary(request)[0].preferred == "存储分区"
    request.glossary = []
    request.context = ["A customer deposits money at a bank."]
    assert model_worker._translation_glossary(request) == []
    request.context = ["The controller uses memory banks."]
    request.target_language = "ja"
    assert model_worker._translation_glossary(request) == []


@pytest.mark.parametrize("cuts,count,valid", [
    ([], 1, True), ([], 12, True), ([4, 8], 12, True),
    ([1], 12, False), ([11], 12, False), ([4, 5], 12, False),
    ([4, 4], 12, False), ([8, 4], 12, False), ([True], 12, False),
    ([2.0], 12, False), ("4", 12, False),
])
def test_topic_boundaries_require_order_and_supporting_context(
    cuts: object, count: int, valid: bool,
) -> None:
    assert model_worker._topic_boundaries_valid({"boundaries": cuts}, count) is valid


@pytest.mark.asyncio
async def test_topic_analysis_does_not_send_source_ids_or_accept_truncated_input(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[dict[str, Any]] = []

    async def infer(
        _system: str, user: str, _schema: dict[str, object], **kwargs: object,
    ) -> dict[str, Any]:
        calls.append(json.loads(user))
        assert kwargs["thinking"] is False
        assert kwargs["num_ctx"] == 8192
        if len(calls) == 2:
            return {"decisions": [{"index": 2, "preceding_subject": "first subject",
                                   "following_subject": "second subject", "distinct_topic": True}]}
        return {"boundaries": [2]}

    monkeypatch.setattr(model_worker, "_ollama_json", infer)
    request = model_worker.TopicRequest(segments=[
        EvidenceSegment(id=f"synthetic-private-id-{i}", text=f"Complete paragraph {i}")
        for i in range(4)
    ])
    result = await model_worker.topics(request)
    assert result.boundaries == [2]
    assert len(calls) == 2
    assert all(set(item) == {"index", "text"} for item in calls[0]["paragraphs"])
    assert calls[1]["proposed_indices"] == [2]
    request.segments[0].text = "长" * 5000
    with pytest.raises(HTTPException) as rejected:
        await model_worker.topics(request)
    assert rejected.value.status_code == 413
    assert len(calls) == 2


@pytest.mark.asyncio
async def test_topic_verification_can_veto_a_language_switch(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    responses: list[dict[str, Any]] = [
        {"boundaries": [2]},
        {"decisions": [{"index": 2, "preceding_subject": "feedback stability",
                        "following_subject": "feedback stability", "distinct_topic": False}]},
    ]
    outputs = iter(responses)

    async def infer(*_args: Any, **kwargs: Any) -> dict[str, Any]:
        value = next(outputs)
        assert kwargs["accept"](value)
        return value

    monkeypatch.setattr(model_worker, "_ollama_json", infer)
    result = await model_worker.topics(model_worker.TopicRequest(segments=[
        EvidenceSegment(id=str(i), text="The same synthetic subject") for i in range(4)
    ]))
    assert result.boundaries == []
    invalid_responses: list[dict[str, Any]] = [
        {}, {"decisions": []}, {"decisions": [{"index": 2}]}, {
        "decisions": [{"index": 2, "preceding_subject": "a", "following_subject": "b",
                       "distinct_topic": "false"}],
    }]
    for invalid in invalid_responses:
        assert not model_worker._topic_decisions_valid(invalid, [2])


@pytest.mark.asyncio
async def test_explanation_repairs_term_evidence_before_preserving_valid_summary(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    payload = {
        "sections": [{"source_indexes": [0], "explanation": "缓存用于保存可复用的数据。"}],
        "terms": [{
            "term": "缓存", "explanation": "保存数据的存储层。",
            "evidence": [{"kind": "segment", "index": 0, "quote": "paraphrased quote"}],
        }],
    }

    async def infer(*_args: object, **kwargs: object) -> dict[str, object]:
        accept = kwargs["accept"]
        assert callable(accept)
        assert not accept(payload)
        assert accept(payload)
        return payload

    async def no_op(*_args: object, **_kwargs: object) -> None:
        return None

    monkeypatch.setattr(model_worker, "_shared_resident_models", lambda: False)
    monkeypatch.setattr(model_worker, "_unload_ollama_model", no_op)
    monkeypatch.setattr(model_worker, "_release_asr_model_sync", lambda: None)
    monkeypatch.setattr(model_worker, "_restore_realtime_translation_model", no_op)
    monkeypatch.setattr(model_worker, "_ollama_json", infer)
    result = await model_worker.explain(ExplanationRequest(
        segments=[EvidenceSegment(id="first", text="A cache stores reusable data.")],
        target_language="zh-CN",
    ))
    assert result.paragraph_summary == "缓存用于保存可复用的数据。"
    assert result.terms == []


@pytest.mark.asyncio
async def test_explanation_releases_realtime_weights_before_loading_background_model(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[str] = []

    async def unload(_model: str) -> None:
        pass

    def release() -> None:
        calls.append("release_realtime")

    async def infer(*_args: object, **_kwargs: object) -> dict[str, object]:
        calls.append("infer_explanation")
        return {"sections": [{"source_indexes": [0],
                              "explanation": "转发减少流水线停顿。"}], "terms": []}

    async def restore(_model: str) -> None:
        calls.append("release_background")

    monkeypatch.setattr(model_worker, "_unload_ollama_model", unload)
    monkeypatch.setattr(model_worker, "_release_asr_model_sync", release)
    monkeypatch.setattr(model_worker, "_ollama_json", infer)
    monkeypatch.setattr(model_worker, "_restore_realtime_translation_model", restore)
    model_worker._gpu_inflight = None
    await model_worker.explain(ExplanationRequest(
        segments=[EvidenceSegment(id="synthetic-segment", text="Forwarding reduces stalls.")],
        asset_pages=[], target_language="zh-CN",
    ))
    assert calls == ["release_realtime", "infer_explanation", "release_background"]
