"""Compile complete, evidence-bound topic explanations without sampling a course."""

from __future__ import annotations

from typing import Any

from workers.gpu_agent.teaching import PartCaller, source_pieces, source_records, valid_provider

COMPILED_PROVIDER = "compiled:content-groups-v1@cpu"  # Historical result compatibility only.


def note_batches(notes: list[str], capacity: int = 3500) -> list[str]:
    batches: list[str] = []
    pending: list[str] = []
    used = 0
    for note in notes:
        for piece in source_pieces(note, capacity):
            size = len(piece.encode()) + (2 if pending else 0)
            if pending and used + size > capacity:
                batches.append("\n\n".join(pending))
                pending, used = [], 0
            pending.append(piece)
            used += len(piece.encode()) + (2 if len(pending) > 1 else 0)
    if pending:
        batches.append("\n\n".join(pending))
    return batches


async def compile_course(model_input: dict[str, Any], call: PartCaller) -> dict[str, Any]:
    segments = source_records(model_input.get("segments"), required=True)
    pages = source_records(model_input.get("asset_pages", []))
    positions = {source["id"]: index for index, source in enumerate(segments)}
    page_ids = {source["id"] for source in pages}
    provider = ""

    async def synthesize(text: str, phase: str = "course") -> str:
        nonlocal provider
        body = {"phase": phase, "text": text,
                "target_language": model_input["target_language"]}
        # Long courses contain many independent cloud parts. A rejected DS
        # response should retry this bounded part, not discard earlier parts
        # and start the entire course again. Each call already tries both DS
        # routes; other failures remain visible to the normal job retry path.
        for attempt in range(4):
            try:
                result = await call(body)
                break
            except ValueError as error:
                if str(error) != "cloud_teaching_contract_invalid" or attempt == 3:
                    raise
        observed, prose = result.get("provider"), result.get("prose")
        if (not valid_provider(observed) or (provider and provider != observed)
                or not isinstance(prose, str) or not prose.strip()):
            raise ValueError("course_synthesis_invalid")
        provider = observed
        return prose.strip()

    reusable: dict[int, dict[str, Any]] = {}
    for group in model_input.get("complete_groups", []):
        if not isinstance(group, dict) or group.get("coverage_contract") != "all_sources_v1":
            continue
        result = group.get("result")
        if not isinstance(result, dict) or not isinstance(result.get("paragraph_summary"), str):
            continue
        ids = result.get("evidence_segment_ids", [])
        refs = result.get("asset_page_ids", [])
        if not isinstance(ids, list) or not ids or any(ref not in positions for ref in ids):
            continue
        indexes = [positions[ref] for ref in ids]
        if indexes != list(range(indexes[0], indexes[0] + len(indexes))):
            continue
        if not isinstance(refs, list) or any(ref not in page_ids for ref in refs):
            continue
        if not result["paragraph_summary"].strip():
            continue
        if any(not set(term.get("evidence_segment_ids", [])) <= set(ids)
               or not set(term.get("asset_page_ids", [])) <= set(refs)
               for term in result.get("terms", [])):
            continue
        reusable[indexes[0]] = result

    groups: list[dict[str, Any]] = []
    cursor = 0
    while cursor < len(segments):
        if cursor in reusable:
            group = reusable[cursor]
            cursor += len(group["evidence_segment_ids"])
        else:
            end = cursor + 1
            while end < len(segments) and end not in reusable and end - cursor < 20:
                end += 1
            missing = segments[cursor:end]
            group = {
                "paragraph_summary": "\n\n".join([
                    await synthesize(batch, "group")
                    for batch in note_batches([source["text"] for source in missing])
                ]),
                "terms": [], "evidence_segment_ids": [source["id"] for source in missing],
                "asset_page_ids": [],
            }
            cursor = end
        groups.append(group)

    # The material library supplements a cited teaching group. Unused pages
    # are not a second lecture and must not become independent course chapters.
    covered_pages = {ref for group in groups for ref in group["asset_page_ids"]}
    expected_ids = [source["id"] for source in segments]
    if [ref for group in groups for ref in group["evidence_segment_ids"]] != expected_ids:
        raise ValueError("summary_source_coverage_invalid")
    terms: list[dict[str, Any]] = []
    indexed_terms: dict[tuple[str, str, str], dict[str, Any]] = {}
    for group in groups:
        for term in group["terms"]:
            key = (
                term["term"].casefold(), term["explanation"], term.get("background_reference", ""),
            )
            if key not in indexed_terms:
                record = {"term": term["term"], "one_line": term["explanation"],
                          "evidence_segment_ids": [], "asset_page_ids": []}
                if term.get("background_reference"):
                    record["background_reference"] = term["background_reference"]
                indexed_terms[key] = record
                terms.append(record)
            for field in ["evidence_segment_ids", "asset_page_ids"]:
                for ref in term[field]:
                    if ref not in indexed_terms[key][field]:
                        indexed_terms[key][field].append(ref)
    notes = [group["paragraph_summary"].strip() for group in groups]
    # A complete content-group note is already a source-checked chapter. Keep
    # its full explanation and evidence instead of asking the cloud to rewrite
    # every chapter into a shorter text that cannot preserve all its details.
    # Only the cross-chapter overview needs a second synthesis pass.
    chapters = notes if len(notes) > 1 else []
    if len("\n\n".join(notes).encode()) <= 3500:
        overview = await synthesize("\n\n".join(notes))
    else:
        remaining = notes
        # Chinese character limits do not imply a shrinking UTF-8 byte budget.
        # Keep reducing the overview until it fits; each accepted pass must
        # strictly reduce bytes, so the bound derives from the initial chapter count.
        for _ in range(len(chapters) + 1):
            if len("\n\n".join(remaining).encode()) <= 3500:
                break
            next_level = [
                await synthesize(batch, "course_reduce")
                for batch in note_batches(remaining)
            ]
            if len("\n\n".join(next_level).encode()) >= len("\n\n".join(remaining).encode()):
                raise ValueError("course_synthesis_capacity_exceeded")
            remaining = next_level
        else:
            raise ValueError("course_synthesis_capacity_exceeded")
        overview = await synthesize("\n\n".join(remaining))
    return {
        "overview": overview,
        "key_points": chapters, "terminology": terms, "open_questions": [],
        "evidence_segment_ids": expected_ids,
        "asset_page_ids": [source["id"] for source in pages if source["id"] in covered_pages],
        "provider": provider,
    }
