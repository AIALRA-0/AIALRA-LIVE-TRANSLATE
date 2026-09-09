"""Compile complete, evidence-bound topic explanations without sampling a course."""

from __future__ import annotations

from typing import Any

from workers.gpu_agent.teaching import PartCaller, assemble_explanation, source_records

COMPILED_PROVIDER = "compiled:content-groups-v1@cpu"


async def compile_course(model_input: dict[str, Any], call: PartCaller) -> dict[str, Any]:
    segments = source_records(model_input.get("segments"), required=True)
    pages = source_records(model_input.get("asset_pages", []))
    positions = {source["id"]: index for index, source in enumerate(segments)}
    page_ids = {source["id"] for source in pages}
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
            group = await assemble_explanation({
                "segments": segments[cursor:end], "asset_pages": [],
                "target_language": model_input["target_language"],
            }, call)
            cursor = end
        groups.append(group)

    covered_pages = {ref for group in groups for ref in group["asset_page_ids"]}
    for page in pages:
        if page["id"] in covered_pages:
            continue
        # The same bounded prose/definition contract works for a material page;
        # restore its page reference before composing the public result.
        group = await assemble_explanation({
            "segments": [page], "asset_pages": [],
            "target_language": model_input["target_language"],
        }, call)
        group["asset_page_ids"], group["evidence_segment_ids"] = [page["id"]], []
        for term in group["terms"]:
            term["asset_page_ids"], term["evidence_segment_ids"] = [page["id"]], []
        groups.append(group)
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
    return {
        "overview": "\n\n".join(group["paragraph_summary"] for group in groups),
        "key_points": [], "terminology": terms, "open_questions": [],
        "evidence_segment_ids": expected_ids,
        "asset_page_ids": [source["id"] for source in pages],
        "provider": COMPILED_PROVIDER,
    }
