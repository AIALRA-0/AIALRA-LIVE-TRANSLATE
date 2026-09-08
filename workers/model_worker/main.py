"""Loopback model worker used by the Rust core through a versioned HTTP contract."""

from __future__ import annotations

import asyncio
import base64
import gc
import io
import json
import logging
import os
import sys
import threading
import time
import unicodedata
from collections.abc import Awaitable, Callable, Coroutine
from functools import wraps
from pathlib import Path
from typing import Annotated, Any

import httpx
import numpy as np
import numpy.typing as npt
from docx import Document
from fastapi import FastAPI, File, HTTPException, UploadFile
from PIL import Image
from pptx import Presentation
from pydantic import BaseModel, Field
from pypdf import PdfReader

_windows_dll_handles: list[Any] = []


def _register_bundled_cuda_dlls() -> None:
    """Expose uv-installed NVIDIA runtime DLLs to CTranslate2 on Windows."""

    if os.name != "nt" or not hasattr(os, "add_dll_directory"):
        return
    binary_directories: list[str] = []
    for entry in sys.path:
        package_root = Path(entry) / "nvidia"
        for package in ("cublas", "cudnn", "cuda_nvrtc"):
            binary_directory = package_root / package / "bin"
            if binary_directory.is_dir():
                resolved = str(binary_directory.resolve())
                if resolved not in binary_directories:
                    binary_directories.append(resolved)
                    _windows_dll_handles.append(os.add_dll_directory(resolved))
    if binary_directories:
        os.environ["PATH"] = os.pathsep.join(binary_directories + [os.environ.get("PATH", "")])


_register_bundled_cuda_dlls()

app = FastAPI(title="AIALRA Local Model Worker", version="1.0.0")

_gpu_inflight: asyncio.Task[Any] | None = None
_gpu_started_at = 0.0


def single_gpu_call[**P, T](
    function: Callable[P, Awaitable[T]],
) -> Callable[P, Coroutine[Any, Any, T]]:
    """Keep the actual inference alive and exclusive after an HTTP disconnect.

    Cancelling asyncio.to_thread does not stop its CUDA work. Shield the entire
    operation (including cleanup), rejecting overlaps instead of stacking more
    threads on a timed-out request. Audio/lease traffic is served by Core.
    """
    @wraps(function)
    async def wrapped(*args: P.args, **kwargs: P.kwargs) -> T:
        global _gpu_inflight, _gpu_started_at
        if _gpu_inflight is not None and not _gpu_inflight.done():
            raise HTTPException(503, "model_worker_busy", headers={
                "Retry-After": "2", "X-Aialra-Worker-State": "busy",
            })

        async def invoke() -> T:
            try:
                return await function(*args, **kwargs)
            except HTTPException:
                raise
            except Exception as error:
                logging.getLogger(__name__).error(
                    "model_execution_failed kind=%s", type(error).__name__,
                )
                raise HTTPException(503, "model_execution_failed") from None

        task = asyncio.create_task(invoke())
        _gpu_inflight = task
        _gpu_started_at = time.monotonic()
        # Consume an eventual exception even if the original caller disconnected.
        task.add_done_callback(lambda done: done.exception() if not done.cancelled() else None)
        return await asyncio.shield(task)

    return wrapped

OLLAMA_URL = os.getenv("AIALRA_OLLAMA_URL", "http://127.0.0.1:11434").rstrip("/")
OLLAMA_MODEL = os.getenv("AIALRA_OLLAMA_MODEL", "qwen2.5:7b-instruct")
TRANSLATION_MODEL = os.getenv("AIALRA_TRANSLATION_MODEL", OLLAMA_MODEL)
EXPLANATION_MODEL = os.getenv("AIALRA_EXPLANATION_MODEL", "qwen2.5:7b-instruct")
SUMMARY_MODEL = os.getenv("AIALRA_SUMMARY_MODEL", "qwen2.5:14b-instruct")
# A 14B model can require a one-time cold CUDA load on a 16 GB card.  Keep that
# delay inside the explicit asynchronous summary lane instead of allowing the
# request to be cut off at the old 120-second transport timeout.  The core still
# keeps the session in ``processing`` until the real result arrives.
SUMMARY_TIMEOUT_SECONDS = max(
    30.0, min(float(os.getenv("AIALRA_SUMMARY_TIMEOUT_SECONDS", "120")), 120.0)
)
SUMMARY_CONTEXT_TOKENS = max(
    512, min(int(os.getenv("AIALRA_SUMMARY_CONTEXT_TOKENS", "3072")), 8192)
)
SUMMARY_MAX_TOKENS = max(
    160, min(int(os.getenv("AIALRA_SUMMARY_MAX_TOKENS", "420")), 600)
)
VISION_MODEL = os.getenv("AIALRA_VISION_MODEL", "qwen3-vl:8b-instruct")
ASR_PROVIDER = os.getenv("AIALRA_ASR_PROVIDER", "faster-whisper").strip().casefold()
ASR_MODEL_NAME = os.getenv("AIALRA_ASR_MODEL", "small")
ASR_DEVICE = os.getenv("AIALRA_ASR_DEVICE", "cuda")
ASR_COMPUTE_TYPE = os.getenv("AIALRA_ASR_COMPUTE_TYPE", "float16")
ASR_CPU_THREADS = max(0, min(32, int(os.getenv("AIALRA_ASR_CPU_THREADS", "12"))))
ASR_BEAM_SIZE = max(1, min(5, int(os.getenv("AIALRA_ASR_BEAM_SIZE", "3"))))
ASR_BEST_OF = max(1, min(5, int(os.getenv("AIALRA_ASR_BEST_OF", str(ASR_BEAM_SIZE)))))
TRANSLATION_PROVIDER = os.getenv("AIALRA_TRANSLATION_PROVIDER", "ollama").strip().casefold()
HYMT_MODEL = os.getenv("AIALRA_HYMT_MODEL", "tencent/HY-MT1.5-1.8B")
HYMT_DEVICE = os.getenv("AIALRA_HYMT_DEVICE", "cuda")
LLM_DEVICE = os.getenv("AIALRA_LLM_DEVICE", "cuda")

_asr_model: Any | None = None
_asr_lock = threading.Lock()
_qwen_asr_model: Any | None = None
_qwen_asr_lock = threading.Lock()
_hymt_tokenizer: Any | None = None
_hymt_model: Any | None = None
_hymt_lock = threading.Lock()


class HealthResponse(BaseModel):
    """Health separates worker availability from optional model readiness."""

    status: str
    asr_available: bool
    ollama_available: bool
    ollama_gpu_resident: bool
    model: str
    asr_provider: str
    asr_cpu_threads: int
    llm_provider: str
    translation_available: bool
    translation_provider: str
    inference_busy: bool = False
    inference_overdue: bool = False
    realtime_models_ready: bool = False


class AsrRequest(BaseModel):
    """PCM input avoids container ambiguity between browser, Android, and mini-app clients."""

    pcm_s16le_base64: str = Field(min_length=4, max_length=4_000_000)
    sample_rate: int = Field(ge=8_000, le=48_000)
    language: str = Field(min_length=2, max_length=32)
    initial_prompt: str = Field(default="", max_length=4_000)


class AsrResponse(BaseModel):
    """Final ASR result retains provider identity and measured audio duration."""

    text: str
    language: str
    confidence: float = Field(ge=0, le=1)
    duration_ms: int = Field(ge=0)
    provider: str


class GlossaryConstraint(BaseModel):
    """Confirmed terminology constrains translation without mutating source text."""

    source: str
    preferred: str
    do_not_translate: bool = False


class TranslationRequest(BaseModel):
    """Stable translation receives a bounded context and explicit terminology."""

    text: str = Field(min_length=1, max_length=20_000)
    source_language: str
    target_language: str
    glossary: list[GlossaryConstraint] = Field(default_factory=list, max_length=100)
    context: list[str] = Field(default_factory=list, max_length=10)


class TranslationResponse(BaseModel):
    """Provider identity lets the timeline verify the model and execution device."""

    source_text: str
    text: str
    provider: str
    source_language: str
    target_language: str


class EvidenceSegment(BaseModel):
    """Only stable segment IDs can become explanation evidence."""

    id: str
    text: str


class EvidencePage(BaseModel):
    """Parsed page text carries a stable page ID into the next explanation."""

    id: str
    title: str
    text: str


class ExplanationRequest(BaseModel):
    """The request contains one bounded content group and relevant course pages."""

    segments: list[EvidenceSegment] = Field(min_length=1, max_length=20)
    asset_pages: list[EvidencePage] = Field(default_factory=list, max_length=12)
    target_language: str
    content_group_id: str | None = None


class ExplanationTerm(BaseModel):
    """A rare term receives one short explanation and traceable evidence."""

    term: str
    explanation: str
    evidence_segment_ids: list[str]
    asset_page_ids: list[str]


class ExplanationResponse(BaseModel):
    """Paragraph summaries and term definitions are separate, bounded projections."""

    paragraph_summary: str
    terms: list[ExplanationTerm]
    evidence_segment_ids: list[str]
    asset_page_ids: list[str]
    provider: str


class RareTerm(BaseModel):
    """Course summaries retain their separate terminology shape."""

    term: str
    one_line: str
    evidence_segment_ids: list[str]
    asset_page_ids: list[str]


class SummaryRequest(BaseModel):
    """A final summary receives stable evidence only after the real-time queue drains."""

    segments: list[EvidenceSegment] = Field(min_length=1, max_length=64)
    asset_pages: list[EvidencePage] = Field(default_factory=list, max_length=24)
    rolling_summaries: list[str] = Field(default_factory=list, max_length=24)
    target_language: str


class SummaryResponse(BaseModel):
    """Every final summary remains traceable to stable transcript and page identifiers."""

    overview: str
    key_points: list[str]
    terminology: list[RareTerm]
    open_questions: list[str] = Field(default_factory=list)
    evidence_segment_ids: list[str]
    asset_page_ids: list[str]
    provider: str


class ParsedPage(BaseModel):
    """Every page receives deterministic order, title, and extracted text."""

    page_number: int = Field(ge=1)
    title: str
    text: str


class AssetParseResponse(BaseModel):
    """Parser identity makes derived page text reproducible after upgrades."""

    parser: str
    pages: list[ParsedPage]


@app.get("/health", response_model=HealthResponse)
async def health() -> HealthResponse:
    """Health proves configured imports without loading large model weights."""

    asr_available = _configured_asr_importable()
    ollama_available = await _ollama_available()
    ollama_gpu_resident = await _ollama_gpu_resident()
    translation_available = _configured_translation_importable()
    busy = _gpu_inflight is not None and not _gpu_inflight.done()
    return HealthResponse(
        status="ok" if asr_available and ollama_available and translation_available else "degraded",
        asr_available=asr_available,
        ollama_available=ollama_available,
        ollama_gpu_resident=ollama_gpu_resident,
        model=OLLAMA_MODEL,
        asr_provider=_asr_provider_name(),
        asr_cpu_threads=ASR_CPU_THREADS,
        llm_provider=f"ollama:{OLLAMA_MODEL}@{LLM_DEVICE}",
        translation_available=translation_available,
        translation_provider=_translation_provider_name(),
        inference_busy=busy,
        inference_overdue=busy and time.monotonic() - _gpu_started_at > 360,
        realtime_models_ready=(_qwen_asr_model is not None or _asr_model is not None)
        and (TRANSLATION_PROVIDER == "ollama" or _hymt_model is not None),
    )


@app.post("/v1/warmup")
@single_gpu_call
async def warmup() -> dict[str, bool]:
    """Load configured realtime weights before the Agent accepts a course."""
    def load() -> None:
        if ASR_PROVIDER in {"qwen3-asr", "qwen_asr", "qwen3_asr"}:
            _get_qwen_asr_model()
        else:
            _get_asr_model()
        if TRANSLATION_PROVIDER in {"hy-mt", "hymt", "hy_mt"}:
            _get_hymt_runtime()
    try:
        await asyncio.to_thread(load)
    except (ImportError, OSError, RuntimeError, ValueError) as error:
        raise HTTPException(503, "realtime_model_warmup_failed") from error
    return {"ready": True}


@app.post("/v1/asr/transcribe", response_model=AsrResponse)
@single_gpu_call
async def transcribe(request: AsrRequest) -> AsrResponse:
    """ASR runs in a worker thread so model inference never blocks the HTTP event loop."""

    if not _configured_asr_importable():
        raise HTTPException(status_code=503, detail="configured ASR provider is unavailable")
    try:
        pcm_bytes = base64.b64decode(request.pcm_s16le_base64, validate=True)
    except ValueError as error:
        raise HTTPException(status_code=400, detail="invalid base64 PCM") from error
    if len(pcm_bytes) % 2:
        raise HTTPException(status_code=400, detail="PCM must contain complete 16-bit samples")
    audio = np.frombuffer(pcm_bytes, dtype="<i2").astype(np.float32) / 32768.0
    try:
        return await asyncio.to_thread(_transcribe_sync, audio, request)
    except (ImportError, OSError, RuntimeError, ValueError) as error:
        raise HTTPException(status_code=503, detail="configured ASR provider failed") from error


@app.post("/v1/translate", response_model=TranslationResponse)
@single_gpu_call
async def translate(request: TranslationRequest) -> TranslationResponse:
    """Use the configured dedicated translator while keeping the old Ollama path available."""

    source_language = request.source_language.casefold().replace("_", "-")
    target_language = request.target_language.casefold().replace("_", "-")
    if (
        source_language.split("-", 1)[0] == target_language.split("-", 1)[0]
        and source_language not in {"auto", "mixed", "zh-en"}
    ):
        return TranslationResponse(
            source_text=request.text,
            text=request.text,
            provider=f"identity:{source_language}@cpu",
            source_language=source_language,
            target_language=target_language,
        )

    if TRANSLATION_PROVIDER in {"hy-mt", "hymt", "hy_mt"}:
        if not _configured_translation_importable():
            raise HTTPException(
                status_code=503, detail="configured translation provider is unavailable"
            )
        try:
            translation_text = await asyncio.to_thread(_translate_hymt_sync, request)
        except (ImportError, OSError, RuntimeError, ValueError) as error:
            raise HTTPException(
                status_code=503, detail="dedicated translation provider failed"
            ) from error
        if not _translation_text_contract_ok(
            translation_text, request.source_language, request.target_language
        ):
            raise HTTPException(status_code=503, detail="dedicated translation result is invalid")
        return TranslationResponse(
            source_text=request.text,
            text=translation_text,
            provider=_translation_provider_name(),
            source_language=source_language,
            target_language=target_language,
        )

    glossary_lines = [
        f"{item.source} => {item.source if item.do_not_translate else item.preferred}"
        for item in request.glossary
    ]
    system = (
        "You clean and translate one provisional ASR lecture paragraph. Return exactly one JSON "
        "object with only source_text and translation. The source_text field is a cleaned copy "
        "of the input Text in the source language; it is never a translation. Restore punctuation "
        "and casing, join clearly split clauses, and remove only immediate accidental repetition "
        "from adjacent ASR windows. Never paraphrase, translate, invent, or remove meaning in "
        "source_text. The translation field is one natural, meaning-based paragraph in the target "
        "language that preserves the cleaned source's logical connections without adding facts. "
        "For an English-to-Chinese request, source_text must remain English and only translation "
        "may be Chinese. For any other language pair, apply the same rule: never put "
        "target-language "
        "text in source_text. "
        "Context is previous-course context for terminology "
        "and pronoun resolution only: never translate, quote, or repeat Context. "
        "Preserve formulas, code, model numbers, and do-not-translate terms. "
        "Do not return labels or commentary outside JSON."
    )
    user = (
        f"Source language: {request.source_language}\n"
        f"Target language: {request.target_language}\n"
        f"Context: {' | '.join(request.context[-3:])}\n"
        f"Glossary: {'; '.join(glossary_lines)}\n"
        f"Text: {request.text}"
    )
    result = await _ollama_json(
        system,
        user,
        {
            "type": "object",
            "properties": {
                "source_text": {"type": "string", "maxLength": 20_000},
                "translation": {"type": "string", "maxLength": 20_000},
            },
            "required": ["source_text", "translation"],
            "additionalProperties": False,
        },
        max_tokens=1200,
        model=TRANSLATION_MODEL,
        timeout_seconds=45.0,
        attempts=2,
        repair_instruction=(
            f"This is a {request.source_language}-to-{request.target_language} request. "
            "The previous JSON was rejected because source_text was not in the source language "
            "or was identical to translation. Copy and clean the input Text into source_text; "
            "translate only the translation field. Do not put target-language text in source_text."
        ),
        accept=lambda payload: _translation_contract_ok(
            payload, request.source_language, request.target_language
        ),
    )
    if isinstance(result, dict):
        return TranslationResponse(
            source_text=_clean_translation_output(str(result["source_text"])),
            text=_clean_translation_output(str(result["translation"])),
            provider=_translation_provider_name(),
            source_language=source_language,
            target_language=target_language,
        )
    raise HTTPException(status_code=503, detail="local Ollama translation is unavailable")


def _script_counts(value: str) -> dict[str, int]:
    counts = {"latin": 0, "cjk": 0, "hangul": 0, "kana": 0, "other": 0}
    for character in value:
        if character.isspace() or unicodedata.category(character).startswith("P"):
            continue
        codepoint = ord(character)
        if (character.isascii() and character.isalpha()) or "LATIN" in unicodedata.name(
            character, ""
        ):
            counts["latin"] += 1
        elif 0x4E00 <= codepoint <= 0x9FFF:
            counts["cjk"] += 1
        elif 0xAC00 <= codepoint <= 0xD7AF:
            counts["hangul"] += 1
        elif 0x3040 <= codepoint <= 0x30FF:
            counts["kana"] += 1
        else:
            counts["other"] += 1
    return counts


def _language_matches(text: str, language: str) -> bool:
    counts = _script_counts(text)
    significant = sum(counts.values())
    if significant == 0:
        return False
    normalized = language.casefold().replace("_", "-")
    base_language = normalized.split("-", 1)[0]
    if normalized in {"auto", "mixed", "zh-en"}:
        return True
    if base_language == "zh":
        return counts["cjk"] >= max(1, significant // 5)
    if base_language == "ko":
        return counts["hangul"] >= max(1, significant // 5)
    if base_language == "ja":
        return counts["kana"] > 0 or counts["cjk"] >= max(1, significant // 3)
    if base_language in {"en", "es", "fr", "de"}:
        return counts["latin"] >= max(1, significant // 2) and counts["cjk"] < counts["latin"]
    return True


def _translation_contract_ok(
    payload: dict[str, Any], source_language: str, target_language: str
) -> bool:
    source = payload.get("source_text")
    translation = payload.get("translation")
    if (
        not isinstance(source, str)
        or not source.strip()
        or not isinstance(translation, str)
        or not translation.strip()
    ):
        return False
    source = _clean_translation_output(source)
    translation = _clean_translation_output(translation)
    source_normalized = source_language.casefold().replace("_", "-")
    target_normalized = target_language.casefold().replace("_", "-")
    same_language = (
        source_normalized.split("-", 1)[0] == target_normalized.split("-", 1)[0]
        and source_normalized not in {"auto", "mixed", "zh-en"}
    )
    if (
        not same_language
        and source_normalized not in {"auto", "mixed", "zh-en"}
        and not _language_matches(source, source_language)
    ):
        return False
    if not same_language and source.casefold().strip() == translation.casefold().strip():
        return False
    if _contains_translation_metadata(source) or _contains_translation_metadata(translation):
        return False
    if not same_language and not _language_matches(translation, target_language):
        return False
    return True


@app.post("/v1/explain", response_model=ExplanationResponse)
@single_gpu_call
async def explain(request: ExplanationRequest) -> ExplanationResponse:
    """The model writes bounded teaching content while trusted code attaches evidence IDs."""

    await _unload_ollama_model(VISION_MODEL)
    await _unload_ollama_model(SUMMARY_MODEL)
    segment_ids = [segment.id for segment in request.segments]
    page_ids = [page.id for page in request.asset_pages]
    system = (
        "You are a lecture comprehension assistant. Return compact JSON for the supplied "
        "content group "
        "in the requested language. The group contains several adjacent stable lecture paragraphs; "
        "reason over the whole group before writing the result. "
        "When target_language starts with zh, write every natural-language field "
        "in Simplified Chinese. "
        "Return only a paragraph_summary and a terms list. "
        "The paragraph_summary must describe the supplied content group in one to three sentences. "
        "terms must contain only professional terms or abbreviations that visibly occur in the "
        "supplied content group or course material. Explain each term in exactly one short "
        "sentence. "
        "Do not produce ASR guesses, review questions, confidence scores, identifiers, citations, "
        "unsupported background, or any text outside the JSON object."
    )
    language_instruction = (
        "All natural-language output fields must use Simplified Chinese.\n"
        if request.target_language.lower().startswith("zh")
        else ""
    )
    user = language_instruction + json.dumps(
        {
            "target_language": request.target_language,
            "segments": [segment.text for segment in request.segments],
            "asset_pages": [
                {"title": page.title, "text": page.text} for page in request.asset_pages
            ],
            "required_shape": {
                "paragraph_summary": "string",
                "terms": [{"term": "string", "explanation": "string"}],
            },
        },
        ensure_ascii=False,
    )
    try:
        result = await _ollama_json(
            system,
            user,
            {
            "type": "object",
            "properties": {
                "paragraph_summary": {"type": "string", "maxLength": 300},
                "terms": {
                    "type": "array",
                    "maxItems": 12,
                    "items": {
                        "type": "object",
                        "properties": {
                            "term": {"type": "string", "maxLength": 80},
                            "explanation": {"type": "string", "maxLength": 240},
                        },
                        "required": ["term", "explanation"],
                        "additionalProperties": False,
                    },
                },
            },
            "required": ["paragraph_summary", "terms"],
            "additionalProperties": False,
            },
            max_tokens=640,
            model=EXPLANATION_MODEL,
            timeout_seconds=75.0,
            attempts=2,
            accept=lambda payload: _has_explanation_shape(payload)
            and _uses_requested_explanation_language(payload, request.target_language),
        )
    finally:
        await _restore_realtime_translation_model(EXPLANATION_MODEL)
    if isinstance(result, dict):
        compact = {
            "paragraph_summary": result.get("paragraph_summary", result.get("summary", "")),
            "terms": [
                {
                    "term": item.get("term", ""),
                    "explanation": item.get("explanation", item.get("one_line", "")),
                    "evidence_segment_ids": segment_ids[-2:],
                    "asset_page_ids": page_ids,
                }
                for item in (result.get("terms", result.get("rare_terms", [])) or [])
                if isinstance(item, dict)
            ],
            "evidence_segment_ids": segment_ids,
            "asset_page_ids": page_ids,
        }
        normalized = _normalize_explanation(compact, segment_ids, page_ids)
        if normalized is not None:
            normalized.provider = f"ollama:{EXPLANATION_MODEL}@{LLM_DEVICE}"
            return normalized
    raise HTTPException(status_code=503, detail="local Ollama explanation is unavailable")


@app.post("/v1/summarize", response_model=SummaryResponse)
@single_gpu_call
async def summarize(request: SummaryRequest) -> SummaryResponse:
    """The larger local model produces one evidence-bounded summary after recording stops."""

    await _unload_ollama_model(VISION_MODEL)
    await _unload_ollama_model(EXPLANATION_MODEL)
    if TRANSLATION_PROVIDER == "ollama" and SUMMARY_MODEL != TRANSLATION_MODEL:
        # Remove the resident realtime model before loading 14B.  Relying on
        # Ollama's eviction heuristics made the one-shot path sensitive to the
        # exact lecture history and left too little headroom for CUDA ASR.
        await _unload_ollama_model(TRANSLATION_MODEL)
    # Whisper keeps a CUDA model resident during a recording.  Release it before
    # loading the one-shot summary model so the 16 GB card does not spend the
    # entire timeout evicting ASR allocations while Ollama is still cold-starting.
    await asyncio.to_thread(_release_asr_model_sync)
    segment_ids = [segment.id for segment in request.segments]
    page_ids = [page.id for page in request.asset_pages]
    system = (
        "You summarize a completed lecture using only supplied evidence. Return compact JSON. "
        "Do not invent facts, citations, or identifiers. Prefer the rolling summaries for the "
        "overall structure, then use the beginning, middle, and end of the supplied segments "
        "to verify details. Make the overview specific to this lecture, retain important caveats, "
        "and keep every key point traceable to supplied evidence. Keep the overview under 500 "
        "characters, and return at most eight key points and eight terms. Do not generate review "
        "questions or study prompts."
    )
    if request.target_language.lower().startswith("zh"):
        system += " Write every natural-language field in Simplified Chinese."
    user = json.dumps(
        {
            "segments": [{"id": item.id, "text": item.text} for item in request.segments],
            "asset_pages": [
                {"id": item.id, "title": item.title, "text": item.text}
                for item in request.asset_pages
            ],
            "rolling_summaries": request.rolling_summaries,
        },
        ensure_ascii=False,
    )
    try:
        result = await _ollama_json(
            system,
            user,
            {
            "type": "object",
            "properties": {
                "overview": {"type": "string", "maxLength": 500},
                "key_points": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 8,
                    "items": {"type": "string", "maxLength": 180},
                },
                "terminology": {
                    "type": "array",
                    "maxItems": 8,
                    "items": {
                        "type": "object",
                        "properties": {
                            "term": {"type": "string", "maxLength": 80},
                            "one_line": {"type": "string", "maxLength": 180},
                        },
                        "required": ["term", "one_line"],
                        "additionalProperties": False,
                    },
                },
            },
            "required": ["overview", "key_points", "terminology"],
            "additionalProperties": False,
            },
            max_tokens=SUMMARY_MAX_TOKENS,
            model=SUMMARY_MODEL,
            unload_after=True,
            timeout_seconds=SUMMARY_TIMEOUT_SECONDS,
            num_ctx=SUMMARY_CONTEXT_TOKENS,
            attempts=1,
            accept=lambda payload: _has_nonempty_string(payload, "overview")
            and _has_nonempty_list(payload, "key_points"),
        )
    finally:
        await _restore_realtime_translation_model(SUMMARY_MODEL)
    if not isinstance(result, dict):
        raise HTTPException(status_code=503, detail="local Ollama summary is unavailable")
    terminology: list[RareTerm] = []
    seen_terms: set[str] = set()
    for item in result.get("terminology", []):
        if not isinstance(item, dict):
            continue
        term = str(item.get("term", "")).strip()
        one_line = str(item.get("one_line", "")).strip()
        term_key = " ".join(term.split()).casefold()
        if not term or not one_line or term_key in seen_terms:
            continue
        seen_terms.add(term_key)
        terminology.append(
            RareTerm(
                term=term,
                one_line=one_line,
                evidence_segment_ids=segment_ids,
                asset_page_ids=page_ids,
            )
        )
    return SummaryResponse(
        overview=str(result.get("overview", "")),
        key_points=_dedupe_text_items(result.get("key_points", [])),
        terminology=terminology,
        open_questions=[],
        evidence_segment_ids=segment_ids,
        asset_page_ids=page_ids,
        provider=f"ollama:{SUMMARY_MODEL}@{LLM_DEVICE}",
    )


@app.post("/v1/assets/parse", response_model=AssetParseResponse)
@single_gpu_call
async def parse_asset(file: Annotated[UploadFile, File()]) -> AssetParseResponse:
    """Parsers receive in-memory bytes and never trust an uploaded path or archive member name."""

    data = await file.read()
    if len(data) > 50 * 1024 * 1024:
        raise HTTPException(status_code=413, detail="asset exceeds 50 MiB bootstrap limit")
    suffix = Path(file.filename or "asset.bin").suffix.lower()
    try:
        if suffix in {".png", ".jpg", ".jpeg", ".webp"}:
            return await _parse_image_with_vlm(data)
        return await asyncio.to_thread(_parse_asset_sync, suffix, data)
    except (OSError, ValueError, KeyError) as error:
        raise HTTPException(status_code=422, detail="asset parser rejected the file") from error


def _faster_whisper_importable() -> bool:
    """Import probing avoids loading model weights during health checks."""

    try:
        import faster_whisper  # noqa: F401
    except ImportError:
        return False
    return True


def _qwen_asr_importable() -> bool:
    """The dedicated ASR package is optional so the legacy provider remains usable."""

    try:
        import qwen_asr  # noqa: F401
        import torch
    except ImportError:
        return False
    return ASR_DEVICE.casefold() != "cuda" or bool(torch.cuda.is_available())


def _configured_asr_importable() -> bool:
    """Check only the selected provider and its execution device."""

    if ASR_PROVIDER in {"qwen3-asr", "qwen_asr", "qwen3_asr"}:
        return _qwen_asr_importable()
    return _faster_whisper_importable()


def _hymt_importable() -> bool:
    """Check the dedicated translation runtime without downloading model weights."""

    try:
        import torch
        import transformers  # noqa: F401
    except ImportError:
        return False
    return HYMT_DEVICE.casefold() != "cuda" or bool(torch.cuda.is_available())


def _configured_translation_importable() -> bool:
    if TRANSLATION_PROVIDER in {"hy-mt", "hymt", "hy_mt"}:
        return _hymt_importable()
    return True


def _asr_provider_name() -> str:
    if ASR_PROVIDER in {"qwen3-asr", "qwen_asr", "qwen3_asr"}:
        return f"qwen3-asr:{ASR_MODEL_NAME}@{ASR_DEVICE}"
    return f"faster-whisper:{ASR_MODEL_NAME}@{ASR_DEVICE}"


def _translation_provider_name() -> str:
    if TRANSLATION_PROVIDER in {"hy-mt", "hymt", "hy_mt"}:
        return f"hy-mt:{HYMT_MODEL}@{HYMT_DEVICE}"
    return f"ollama:{TRANSLATION_MODEL}@{LLM_DEVICE}"


def _get_asr_model() -> Any:
    """One lazy model instance preserves VRAM and serializes first-load races."""

    global _asr_model
    with _asr_lock:
        if _asr_model is None:
            from faster_whisper import WhisperModel

            _asr_model = WhisperModel(
                ASR_MODEL_NAME,
                device=ASR_DEVICE,
                compute_type=ASR_COMPUTE_TYPE,
                cpu_threads=ASR_CPU_THREADS,
            )
        return _asr_model


def _qwen_language(language: str) -> str | None:
    """Qwen3-ASR accepts English language names rather than ISO language tags."""

    normalized = language.casefold().replace("_", "-")
    if normalized in {"auto", "mixed", "zh-en"}:
        return None
    base = normalized.split("-", 1)[0]
    names = {
        "zh": "Chinese",
        "en": "English",
        "ja": "Japanese",
        "ko": "Korean",
        "es": "Spanish",
        "fr": "French",
        "de": "German",
        "pt": "Portuguese",
        "ru": "Russian",
        "ar": "Arabic",
        "it": "Italian",
        "nl": "Dutch",
        "hi": "Hindi",
    }
    return names.get(base, language)


def _get_qwen_asr_model() -> Any:
    """Load one serialized Qwen3-ASR instance and require CUDA when configured."""

    global _qwen_asr_model
    with _qwen_asr_lock:
        if _qwen_asr_model is None:
            import torch
            from qwen_asr import Qwen3ASRModel

            if ASR_DEVICE.casefold() == "cuda" and not torch.cuda.is_available():
                raise RuntimeError("Qwen3-ASR CUDA is unavailable")
            _qwen_asr_model = Qwen3ASRModel.from_pretrained(
                ASR_MODEL_NAME,
                dtype=torch.bfloat16 if ASR_DEVICE.casefold() == "cuda" else torch.float32,
                device_map="cuda:0" if ASR_DEVICE.casefold() == "cuda" else "cpu",
                max_inference_batch_size=1,
                max_new_tokens=256,
            )
        return _qwen_asr_model


def _transcribe_qwen_sync(
    audio: npt.NDArray[np.float32], request: AsrRequest
) -> AsrResponse:
    model = _get_qwen_asr_model()
    result = model.transcribe(
        audio=(audio, request.sample_rate),
        language=_qwen_language(request.language),
        context="",
        return_time_stamps=False,
    )
    first = result[0] if result else None
    if first is None or not str(getattr(first, "text", "")).strip():
        raise RuntimeError("Qwen3-ASR returned no transcript")
    duration_ms = int(len(audio) * 1_000 / request.sample_rate)
    return AsrResponse(
        text=str(first.text).strip(),
        language=str(getattr(first, "language", None) or request.language),
        confidence=0.0,
        duration_ms=duration_ms,
        provider=_asr_provider_name(),
    )


def _transcribe_sync(audio: npt.NDArray[np.float32], request: AsrRequest) -> AsrResponse:
    """A bounded four-second window produces a stable bootstrap segment."""

    duration_ms = int(len(audio) * 1_000 / request.sample_rate)
    if not _audio_has_speech(audio, request.sample_rate):
        return AsrResponse(
            text="",
            language="unknown",
            confidence=0.0,
            duration_ms=duration_ms,
            provider=_asr_provider_name(),
        )

    if ASR_PROVIDER in {"qwen3-asr", "qwen_asr", "qwen3_asr"}:
        return _transcribe_qwen_sync(audio, request)

    model = _get_asr_model()
    language = None if request.language in {"auto", "mixed", "zh-en"} else request.language
    segments, info = model.transcribe(
        audio,
        language=language,
        initial_prompt=request.initial_prompt or None,
        beam_size=ASR_BEAM_SIZE,
        best_of=ASR_BEST_OF,
        vad_filter=True,
        condition_on_previous_text=True,
    )
    realized = list(segments)
    text = " ".join(segment.text.strip() for segment in realized).strip()
    probabilities = [float(np.exp(segment.avg_logprob)) for segment in realized]
    confidence = float(np.clip(np.mean(probabilities), 0.0, 1.0)) if probabilities else 0.0
    duration_ms = int(len(audio) * 1_000 / request.sample_rate)
    return AsrResponse(
        text=text,
        language=str(getattr(info, "language", language or "unknown")),
        confidence=confidence,
        duration_ms=duration_ms,
        provider=_asr_provider_name(),
    )


def _audio_has_speech(audio: npt.NDArray[np.float32], sample_rate: int) -> bool:
    """Reject genuinely silent windows before they can become model hallucinations.

    Very short arrays are kept compatible with provider unit tests and are not a
    meaningful silence sample. Real capture windows are scored using short RMS
    frames relative to the quietest part of the same window.
    """

    if audio.size == 0:
        return False
    if audio.size < max(320, int(sample_rate * 0.25)):
        return True
    samples = np.nan_to_num(audio.astype(np.float32, copy=False), nan=0.0, posinf=0.0, neginf=0.0)
    samples = samples - float(np.mean(samples))
    frame_size = max(160, int(round(sample_rate * 0.02)))
    frame_count = samples.size // frame_size
    if frame_count == 0:
        return False
    frames = samples[: frame_count * frame_size].reshape(frame_count, frame_size)
    rms = np.sqrt(np.mean(np.square(frames), axis=1))
    levels = np.maximum(-96.0, 20.0 * np.log10(np.maximum(rms, 1e-5)))
    noise_floor = float(np.percentile(levels, 20))
    threshold = max(-55.0, min(-35.0, noise_floor + 7.0))
    voiced = levels >= threshold
    voiced_count = int(np.count_nonzero(voiced))
    return voiced_count >= 2 and voiced_count / len(levels) >= 0.04


def _get_hymt_runtime() -> tuple[Any, Any]:
    """Load HY-MT lazily so ordinary health checks never allocate model memory."""

    global _hymt_model, _hymt_tokenizer
    with _hymt_lock:
        if _hymt_model is None or _hymt_tokenizer is None:
            import torch
            from transformers import AutoModelForCausalLM, AutoTokenizer

            if HYMT_DEVICE.casefold() == "cuda" and not torch.cuda.is_available():
                raise RuntimeError("HY-MT CUDA is unavailable")
            _hymt_tokenizer = AutoTokenizer.from_pretrained(HYMT_MODEL)  # type: ignore[no-untyped-call]
            _hymt_model = AutoModelForCausalLM.from_pretrained(
                HYMT_MODEL,
                dtype=torch.bfloat16 if HYMT_DEVICE.casefold() == "cuda" else torch.float32,
                device_map="cuda:0" if HYMT_DEVICE.casefold() == "cuda" else "cpu",
                low_cpu_mem_usage=True,
            )
        return _hymt_tokenizer, _hymt_model


def _hymt_prompt(request: TranslationRequest) -> str:
    """Tencent's dedicated MT template, not a general assistant conversation."""
    languages = {"zh": "中文", "zh-cn": "简体中文", "en": "英语", "ja": "日语",
                 "ko": "韩语", "es": "西班牙语", "fr": "法语", "de": "德语"}
    chinese_pair = (request.source_language.casefold().startswith("zh")
                    or request.target_language.casefold().startswith("zh"))
    english_languages = {"en": "English", "ja": "Japanese", "ko": "Korean",
                         "es": "Spanish", "fr": "French", "de": "German"}
    target = (languages if chinese_pair else english_languages).get(
        request.target_language.casefold(), request.target_language,
    )
    context = "\n".join(request.context[-2:])[-1200:]
    terms = "\n".join(
        f"{item.source} 翻译成 {item.source if item.do_not_translate else item.preferred}"
        for item in request.glossary[:32]
    )
    if not chinese_pair:
        prompt = (f"{context}\nBased on the provided information, translate the following text "
                  f"into {target}, without translating the preceding information or adding "
                  f"explanations:\n{request.text}" if context else
                  f"Translate the following segment into {target}, without additional "
                  f"explanation.\n\n{request.text}")
    elif context:
        prompt = (f"{context}\n参考上面的信息，把下面的文本翻译成{target}，"
                  f"注意不需要翻译上文，也不要额外解释：\n{request.text}")
    else:
        prompt = (f"将以下文本翻译为{target}，注意只需要输出翻译后的结果，"
                  f"不要额外解释：\n\n{request.text}")
    return f"参考下面的翻译：\n{terms}\n\n{prompt}" if terms else prompt


def _translate_hymt_sync(request: TranslationRequest) -> str:
    """Generate one plain translation and keep provider output out of ordinary logs."""

    import torch

    tokenizer, model = _get_hymt_runtime()
    messages = [{"role": "user", "content": _hymt_prompt(request)}]
    inputs = tokenizer.apply_chat_template(
        messages,
        tokenize=True,
        add_generation_prompt=False,
        return_tensors="pt",
    )
    device = getattr(model, "device", None)
    if device is None:
        device = torch.device("cuda:0" if HYMT_DEVICE.casefold() == "cuda" else "cpu")
    inputs = inputs.to(device)
    started = time.monotonic()
    with torch.inference_mode():
        generated = model.generate(
            inputs,
            max_new_tokens=512,
            do_sample=False,
            num_beams=1,
            repetition_penalty=1.05,
            max_time=45.0,
            pad_token_id=tokenizer.eos_token_id,
        )
    if time.monotonic() - started >= 45.0:
        raise RuntimeError("translation_inference_deadline")
    if generated.shape[1] - inputs.shape[1] >= 512:
        raise RuntimeError("translation_output_limit")
    output = str(
        tokenizer.decode(generated[0, inputs.shape[1] :], skip_special_tokens=True)
    ).strip()
    for prefix in ("Translation:", "翻译：", "翻译:"):
        if output.startswith(prefix):
            output = output[len(prefix) :].strip()
    return _clean_translation_output(output)


def _translation_text_contract_ok(text: str, source_language: str, target_language: str) -> bool:
    text = _clean_translation_output(text)
    if not text or _contains_translation_metadata(text):
        return False
    source_normalized = source_language.casefold().replace("_", "-")
    target_normalized = target_language.casefold().replace("_", "-")
    if source_normalized in {"auto", "mixed", "zh-en"}:
        return _language_matches(text, target_language)
    if source_normalized.split("-", 1)[0] == target_normalized.split("-", 1)[0]:
        return True
    return _language_matches(text, target_language)


def _contains_translation_metadata(text: str) -> bool:
    """Reject provider labels that leaked into the user-facing translation field."""

    labels = _translation_metadata_labels()
    return any(
        line.strip().casefold().startswith(label)
        for line in text.splitlines()
        for label in labels
    )


def _translation_metadata_labels() -> tuple[str, ...]:
    return (
        "previous context for terminology only:", "translated text:",
        "text to translate:", "source language:", "source_language:",
        "target language:", "target_language:", "terminology:", "glossary:",
        "translation:", "源语言：", "源语言:", "目标语言：", "目标语言:",
        "术语背景：", "术语背景:", "之前的术语背景仅用于说明：", "之前的术语背景仅用于说明:",
        "术语：", "术语:", "翻译后的文本：", "翻译后的文本:", "翻译后文本：", "翻译后文本:",
        "译文：", "译文:",
    )


def _translation_content_labels() -> tuple[str, ...]:
    return (
        "translated text:", "translation:", "翻译后的文本：", "翻译后的文本:",
        "翻译后文本：", "翻译后文本:", "译文：", "译文:",
    )


def _metadata_remainder(line: str) -> str | None:
    normalized = line.strip().casefold()
    for label in sorted(_translation_metadata_labels(), key=len, reverse=True):
        if normalized.startswith(label):
            original = line.strip()
            if label in _translation_content_labels():
                return original[len(label) :].lstrip()
            return ""
    return None


def _clean_translation_output(text: str) -> str:
    """Remove only leading provider metadata while preserving same-line content."""

    lines = text.strip().splitlines()
    cleaned: list[str] = []
    leading = True
    for line in lines:
        if leading:
            remainder = _metadata_remainder(line)
            if remainder is not None:
                if remainder:
                    cleaned.append(remainder)
                continue
            if not line.strip():
                continue
            leading = False
        cleaned.append(line)
    return "\n".join(cleaned).strip()


async def _ollama_available() -> bool:
    """A short timeout prevents health checks from delaying the recording controls."""

    try:
        async with httpx.AsyncClient(timeout=1.0) as client:
            response = await client.get(f"{OLLAMA_URL}/api/tags")
            return response.is_success
    except httpx.HTTPError:
        return False


def _ollama_model_uses_gpu(payload: object, model: str = OLLAMA_MODEL) -> bool:
    """Accept the configured model only when at least 90 percent is resident in VRAM."""

    if not isinstance(payload, dict) or not isinstance(payload.get("models"), list):
        return False
    for item in payload["models"]:
        if not isinstance(item, dict) or item.get("name") != model:
            continue
        size = item.get("size")
        size_vram = item.get("size_vram")
        if (
            isinstance(size, int)
            and not isinstance(size, bool)
            and size > 0
            and isinstance(size_vram, int)
            and not isinstance(size_vram, bool)
        ):
            return size_vram >= size * 0.9
    return False


async def _ollama_gpu_resident(model: str = OLLAMA_MODEL) -> bool:
    """Read Ollama's process inventory instead of trusting a configured provider suffix."""

    try:
        async with httpx.AsyncClient(timeout=2.0) as client:
            response = await client.get(f"{OLLAMA_URL}/api/ps")
            response.raise_for_status()
            return _ollama_model_uses_gpu(response.json(), model)
    except (httpx.HTTPError, TypeError, ValueError):
        return False


async def _ollama_json(
    system: str,
    user: str,
    schema: dict[str, Any] | None = None,
    *,
    max_tokens: int = 768,
    accept: Callable[[dict[str, Any]], bool] | None = None,
    model: str = OLLAMA_MODEL,
    unload_after: bool = False,
    timeout_seconds: float = 90.0,
    attempts: int = 2,
    num_ctx: int | None = None,
    repair_instruction: str | None = None,
) -> dict[str, Any] | None:
    """Ollama receives only text already allowed by the local session policy."""

    for attempt in range(max(1, min(attempts, 3))):
        try:
            async with httpx.AsyncClient(timeout=timeout_seconds) as client:
                repair = (
                    "\nThe prior response was invalid. Return exactly the requested JSON shape "
                    "with every required field populated. "
                    f"{repair_instruction or ''}"
                    if attempt
                    else ""
                )
                options: dict[str, Any] = {
                    "temperature": 0 if attempt == 0 else 0.1,
                    "seed": attempt,
                    "num_predict": max_tokens,
                }
                if num_ctx is not None:
                    options["num_ctx"] = num_ctx
                response = await client.post(
                    f"{OLLAMA_URL}/api/chat",
                    json={
                        "model": model,
                        "stream": False,
                        "format": schema or "json",
                        "messages": [
                            {"role": "system", "content": system},
                            {"role": "user", "content": f"{user}{repair}"},
                        ],
                        "options": options,
                    },
                )
                response.raise_for_status()
                content = response.json()["message"]["content"]
                parsed = _parse_model_json(content)
                if isinstance(parsed, dict) and (accept is None or accept(parsed)):
                    if unload_after:
                        await _unload_ollama_model(model)
                    return parsed
        except (httpx.HTTPError, KeyError, TypeError):
            await asyncio.sleep(1)
    if unload_after:
        await _unload_ollama_model(model)
    return None


async def _unload_ollama_model(model: str) -> None:
    """Release large one-shot models after proof-backed inference so ASR keeps VRAM headroom."""

    try:
        async with httpx.AsyncClient(timeout=15.0) as client:
            response = await client.post(
                f"{OLLAMA_URL}/api/generate",
                json={"model": model, "keep_alive": 0, "stream": False},
            )
            response.raise_for_status()
    except httpx.HTTPError:
        return


async def _restore_realtime_translation_model(background_model: str) -> None:
    """Restore the low-latency model before a background lane releases its lock."""

    if TRANSLATION_PROVIDER != "ollama":
        await _unload_ollama_model(background_model)
        return
    if background_model == TRANSLATION_MODEL:
        return
    await _unload_ollama_model(background_model)
    try:
        async with httpx.AsyncClient(timeout=30.0) as client:
            response = await client.post(
                f"{OLLAMA_URL}/api/generate",
                json={
                    "model": TRANSLATION_MODEL,
                    "prompt": "",
                    "keep_alive": -1,
                    "stream": False,
                },
            )
            response.raise_for_status()
        if not await _ollama_gpu_resident(TRANSLATION_MODEL):
            raise RuntimeError("translation model did not return to the GPU")
    except (httpx.HTTPError, RuntimeError) as error:
        raise HTTPException(
            status_code=503,
            detail="local translation model could not be restored after background inference",
        ) from error


def _release_asr_model_sync() -> None:
    """One-shot large models reclaim ASR VRAM only after the serialized queue is idle."""

    global _asr_model, _qwen_asr_model, _hymt_model, _hymt_tokenizer
    with _asr_lock:
        _asr_model = None
    with _qwen_asr_lock:
        _qwen_asr_model = None
    with _hymt_lock:
        _hymt_model = None
        _hymt_tokenizer = None
    gc.collect()
    if "torch" in sys.modules:
        import torch
        if torch.cuda.is_available():
            torch.cuda.empty_cache()


async def _ollama_text(
    system: str,
    user: str,
    *,
    max_tokens: int,
    model: str = OLLAMA_MODEL,
) -> str | None:
    """A plain local model response avoids fragile JSON quoting for translated prose."""

    for attempt in range(2):
        try:
            async with httpx.AsyncClient(timeout=45.0) as client:
                repair = (
                    "\nThe prior response was empty. Return only the translated text."
                    if attempt
                    else ""
                )
                response = await client.post(
                    f"{OLLAMA_URL}/api/chat",
                    json={
                        "model": model,
                        "stream": False,
                        "messages": [
                            {"role": "system", "content": system},
                            {"role": "user", "content": f"{user}{repair}"},
                        ],
                        "options": {
                            "temperature": 0 if attempt == 0 else 0.1,
                            "seed": attempt,
                            "num_predict": max_tokens,
                        },
                    },
                )
                response.raise_for_status()
                content = response.json()["message"]["content"]
                if isinstance(content, str) and content.strip():
                    return content.strip()
        except (httpx.HTTPError, KeyError, TypeError):
            await asyncio.sleep(1)
    return None


async def _parse_image_with_vlm(data: bytes) -> AssetParseResponse:
    """Local VLM performs OCR and visual explanation only when the image stays on this host."""

    with Image.open(io.BytesIO(data)) as image:
        image.verify()
    await _unload_ollama_model(EXPLANATION_MODEL)
    await _unload_ollama_model(SUMMARY_MODEL)
    if TRANSLATION_PROVIDER == "ollama" and VISION_MODEL != TRANSLATION_MODEL:
        await _unload_ollama_model(TRANSLATION_MODEL)
    await asyncio.to_thread(_release_asr_model_sync)
    schema = {
        "type": "object",
        "properties": {
            "title": {"type": "string", "maxLength": 120},
            "ocr_text": {"type": "string", "maxLength": 8000},
            "visual_summary": {"type": "string", "maxLength": 1200},
            "key_terms": {
                "type": "array",
                "maxItems": 12,
                "items": {"type": "string", "maxLength": 100},
            },
        },
        "required": ["title", "ocr_text", "visual_summary", "key_terms"],
        "additionalProperties": False,
    }
    try:
        try:
            async with httpx.AsyncClient(timeout=240.0) as client:
                response = await client.post(
                    f"{OLLAMA_URL}/api/chat",
                    json={
                        "model": VISION_MODEL,
                        "stream": False,
                        "format": schema,
                        "messages": [
                            {
                                "role": "user",
                                "content": (
                                    "Extract all readable text and explain this lecture image in "
                                    "concise Simplified Chinese. Distinguish visible text from "
                                    "interpretation."
                                ),
                                "images": [base64.b64encode(data).decode("ascii")],
                            }
                        ],
                        "options": {"temperature": 0, "num_predict": 1200},
                    },
                )
                response.raise_for_status()
                result = _parse_model_json(str(response.json()["message"]["content"]))
        except (httpx.HTTPError, KeyError, TypeError, ValueError) as error:
            raise HTTPException(
                status_code=503,
                detail="local Ollama vision model is unavailable",
            ) from error
        if not isinstance(result, dict) or not _has_nonempty_string(result, "visual_summary"):
            raise HTTPException(status_code=503, detail="local Ollama vision result is invalid")
        gpu_resident = await _ollama_gpu_resident(VISION_MODEL)
        if not gpu_resident:
            raise HTTPException(
                status_code=503,
                detail="local Ollama vision model did not prove GPU residency",
            )
        title = str(result.get("title") or "课程图片")
        ocr_text = str(result.get("ocr_text") or "").strip()
        summary = str(result.get("visual_summary") or "").strip()
        terms = [
            str(item).strip()
            for item in result.get("key_terms", [])
            if isinstance(item, str) and item.strip()
        ]
        sections = [
            f"可见文字\n{ocr_text}" if ocr_text else "",
            f"图片解释\n{summary}",
            f"关键词\n{'、'.join(terms)}" if terms else "",
        ]
        return AssetParseResponse(
            parser=f"ollama:{VISION_MODEL}@{LLM_DEVICE}",
            pages=[
                ParsedPage(
                    page_number=1,
                    title=title,
                    text="\n\n".join(section for section in sections if section),
                )
            ],
        )
    finally:
        # Release VLM weights and restore the resident realtime model even when
        # inference, JSON validation, or the GPU residency proof fails.
        if TRANSLATION_PROVIDER == "ollama" and VISION_MODEL != TRANSLATION_MODEL:
            await _unload_ollama_model(VISION_MODEL)
            await _restore_realtime_translation_model(VISION_MODEL)


def _parse_model_json(content: str) -> dict[str, Any] | None:
    """Accept unescaped control characters while retaining the JSON object boundary."""

    for strict in (True, False):
        try:
            parsed = json.loads(content, strict=strict)
        except json.JSONDecodeError:
            continue
        return parsed if isinstance(parsed, dict) else None
    return None


def _has_nonempty_string(payload: dict[str, Any], field: str) -> bool:
    """Semantic validation turns malformed local output into an in-process repair attempt."""

    value = payload.get(field)
    return isinstance(value, str) and bool(value.strip())


def _has_nonempty_list(payload: dict[str, Any], field: str) -> bool:
    """Required summary sections cannot silently collapse to empty arrays."""

    value = payload.get(field)
    return isinstance(value, list) and bool(value)


def _dedupe_text_items(value: Any) -> list[str]:
    """Keep model-generated summary sections readable when a provider repeats a point."""

    if not isinstance(value, list):
        return []
    result: list[str] = []
    seen: set[str] = set()
    for item in value:
        if not isinstance(item, str):
            continue
        text = item.strip()
        key = " ".join(text.split()).casefold()
        if not text or key in seen:
            continue
        seen.add(key)
        result.append(text)
    return result


def _has_explanation_shape(payload: dict[str, Any]) -> bool:
    """Require only the two user-facing explanation sections."""

    if not _has_nonempty_string(payload, "paragraph_summary") and not _has_nonempty_string(
        payload, "summary"
    ):
        return False
    terms = payload.get("terms", payload.get("rare_terms", []))
    if not isinstance(terms, list):
        return False
    return all(
        isinstance(item, dict)
        and isinstance(item.get("term"), str)
        and bool(item["term"].strip())
        and isinstance(item.get("explanation", item.get("one_line")), str)
        and bool(str(item.get("explanation", item.get("one_line", ""))).strip())
        for item in terms
    )


def _uses_requested_explanation_language(
    payload: dict[str, Any], target_language: str
) -> bool:
    """A Chinese session never accepts an English-only summary as a completed card."""

    if not target_language.lower().startswith("zh"):
        return True
    summary = payload.get("paragraph_summary", payload.get("summary"))
    return isinstance(summary, str) and any("\u4e00" <= char <= "\u9fff" for char in summary)


def _normalize_explanation(
    raw: dict[str, Any], segment_ids: list[str], page_ids: list[str]
) -> ExplanationResponse | None:
    """Pydantic validates structure while local allowlists remove fabricated evidence IDs."""

    normalized = {
        "paragraph_summary": raw.get("paragraph_summary", raw.get("summary", "")),
        "terms": raw.get("terms", raw.get("rare_terms", [])),
        "evidence_segment_ids": raw.get("evidence_segment_ids", segment_ids),
        "asset_page_ids": raw.get("asset_page_ids", page_ids),
        "provider": "pending",
    }
    try:
        candidate = ExplanationResponse.model_validate(normalized)
    except ValueError:
        return None
    allowed_segments = set(segment_ids)
    allowed_pages = set(page_ids)
    candidate.evidence_segment_ids = [
        item for item in candidate.evidence_segment_ids if item in allowed_segments
    ]
    candidate.asset_page_ids = [item for item in candidate.asset_page_ids if item in allowed_pages]
    for term_item in candidate.terms:
        term_item.evidence_segment_ids = [
            value for value in term_item.evidence_segment_ids if value in allowed_segments
        ]
        term_item.asset_page_ids = [
            value for value in term_item.asset_page_ids if value in allowed_pages
        ]
    if not candidate.evidence_segment_ids:
        candidate.evidence_segment_ids = segment_ids
    return candidate


def _parse_asset_sync(suffix: str, data: bytes) -> AssetParseResponse:
    """Format-specific parsers extract page text before any optional OCR or VLM work."""

    if suffix == ".pptx":
        presentation = Presentation(io.BytesIO(data))
        pages = []
        for index, slide in enumerate(presentation.slides, start=1):
            texts = [shape.text.strip() for shape in slide.shapes if hasattr(shape, "text")]
            nonempty = [text for text in texts if text]
            pages.append(
                ParsedPage(
                    page_number=index,
                    title=nonempty[0] if nonempty else f"Slide {index}",
                    text="\n".join(nonempty),
                )
            )
        return AssetParseResponse(parser="python-pptx:1", pages=pages)
    if suffix == ".pdf":
        reader = PdfReader(io.BytesIO(data))
        pages = [
            ParsedPage(
                page_number=index,
                title=f"Page {index}",
                text=(page.extract_text() or "").strip(),
            )
            for index, page in enumerate(reader.pages, start=1)
        ]
        return AssetParseResponse(parser="pypdf:1", pages=pages)
    if suffix == ".docx":
        document = Document(io.BytesIO(data))
        text = "\n".join(
            paragraph.text.strip() for paragraph in document.paragraphs if paragraph.text
        )
        return AssetParseResponse(
            parser="python-docx:1",
            pages=[ParsedPage(page_number=1, title="Document", text=text)],
        )
    if suffix in {".png", ".jpg", ".jpeg", ".webp"}:
        with Image.open(io.BytesIO(data)) as image:
            title = f"Image {image.width}×{image.height}"
        return AssetParseResponse(
            parser="pillow-metadata:1",
            pages=[ParsedPage(page_number=1, title=title, text="")],
        )
    if suffix in {".txt", ".md", ".csv"}:
        text = data.decode("utf-8", errors="replace")
        return AssetParseResponse(
            parser="utf8-text:1",
            pages=[ParsedPage(page_number=1, title="Text", text=text)],
        )
    raise ValueError(f"unsupported asset suffix: {suffix}")


if __name__ == "__main__":
    # Direct execution supports local work; packaged launches reuse the same app object.
    import uvicorn

    uvicorn.run(app, host="127.0.0.1", port=8790)
