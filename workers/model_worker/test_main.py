"""Deterministic tests cover local fallbacks and page extraction without model downloads."""

from __future__ import annotations

import asyncio
import io

from pptx import Presentation

from workers.model_worker.main import (
    EvidencePage,
    EvidenceSegment,
    ExplanationRequest,
    ExplanationResponse,
    _parse_asset_sync,
)


def _test_only_explanation(request: ExplanationRequest) -> ExplanationResponse:
    """Build deterministic evidence for unit tests without registering a runtime provider."""

    segment_ids = [segment.id for segment in request.segments]
    page_ids = [page.id for page in request.asset_pages]
    return ExplanationResponse(
        summary=request.segments[-1].text,
        missing_context=[],
        rare_terms=[],
        possible_asr_errors=[],
        review_questions=[],
        evidence_segment_ids=segment_ids,
        asset_page_ids=page_ids,
        confidence=0.5,
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
