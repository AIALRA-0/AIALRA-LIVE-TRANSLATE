"""Assemble a complete teaching card while yielding between bounded model calls."""

from __future__ import annotations

import re
from collections.abc import Awaitable, Callable
from typing import Any

from workers.gpu_agent.reviewed_definitions import reviewed_definition

PartCaller = Callable[[dict[str, Any]], Awaitable[dict[str, Any]]]


def contains_term(term: str, source: str) -> bool:
    """A term citation needs a lexical occurrence, not RAM inside program."""
    if not term.strip():
        return False
    left = r"(?<![A-Za-z0-9_])" if term[0].isascii() and term[0].isalnum() else ""
    right = r"(?![A-Za-z0-9_])" if term[-1].isascii() and term[-1].isalnum() else ""
    return re.search(left + re.escape(term) + right, source) is not None


def source_pieces(text: str, capacity: int = 1400) -> list[str]:
    """Split by byte capacity without dropping text; prefer an existing boundary."""
    pieces: list[str] = []
    remaining = text
    while len(remaining.encode()) > capacity:
        end = 0
        used = 0
        for char in remaining:
            size = len(char.encode())
            if used + size > capacity:
                break
            used += size
            end += 1
        boundary = max(remaining.rfind(char, 0, end) for char in [" ", "\n", "。", "，", ". "])
        if boundary >= end // 2:
            end = boundary + 1
        pieces.append(remaining[:end])
        remaining = remaining[end:]
    if remaining:
        pieces.append(remaining)
    assert "".join(pieces) == text
    return pieces


def source_records(value: Any, *, required: bool = False) -> list[dict[str, str]]:
    if not isinstance(value, list) or (required and not value):
        raise ValueError("teaching_sources_required")
    result: list[dict[str, str]] = []
    for entry in value:
        if not isinstance(entry, dict) or not isinstance(entry.get("id"), str):
            raise ValueError("teaching_source_invalid")
        text = entry.get("text")
        if not isinstance(text, str) or not text.strip():
            raise ValueError("teaching_source_empty")
        result.append({"id": entry["id"], "text": text})
    if len({entry["id"] for entry in result}) != len(result):
        raise ValueError("teaching_source_duplicate")
    return result


def teaching_chunks(records: list[dict[str, str]]) -> list[list[dict[str, str]]]:
    """Keep adjacent statements together, bounded by inference capacity, not count."""
    chunks: list[list[dict[str, str]]] = []
    pending: list[dict[str, str]] = []
    used = 0
    for source in records:
        for piece in source_pieces(source["text"]):
            size = len(piece.encode()) + (2 if pending else 0)
            if pending and used + size > 1400:
                chunks.append(pending)
                pending, used = [], 0
            used += len(piece.encode()) + (2 if pending else 0)
            pending.append({"id": source["id"], "text": piece})
    if pending:
        chunks.append(pending)
    return chunks


async def assemble_explanation(model_input: dict[str, Any], call: PartCaller) -> dict[str, Any]:
    segments = source_records(model_input.get("segments"), required=True)
    pages = source_records(model_input.get("asset_pages", []))
    target = model_input.get("target_language")
    if not isinstance(target, str) or not target:
        raise ValueError("teaching_language_required")
    provider = ""

    async def generate(body: dict[str, Any]) -> dict[str, Any]:
        nonlocal provider
        result = await call(body)
        observed = result.get("provider")
        if not isinstance(observed, str) or not observed.startswith("ollama:"):
            raise ValueError("teaching_provider_unverified")
        if not observed.endswith("@cuda") or (provider and observed != provider):
            raise ValueError("teaching_provider_changed")
        provider = observed
        return result

    prose: list[str] = []
    term_sources: dict[str, dict[str, Any]] = {}
    chunks = [(kind, chunk) for kind, records in [("segment", segments), ("page", pages)]
              for chunk in teaching_chunks(records)]
    texts = ["\n\n".join(item["text"] for item in chunk) for _, chunk in chunks]
    # All source fragments finish before a card is published. A failed call
    # leaves the single persistent job retryable, never a fake partial card.
    for index, (kind, sources) in enumerate(chunks):
        piece = texts[index]
        # The topic is already sealed. Both neighbours are available and help
        # disambiguate abbreviations introduced before their full explanation.
        context = texts[max(0, index - 1):index] + texts[index + 1:index + 2]
        body = {"phase": "prose", "text": piece, "context": context,
                "target_language": target}
        result = await generate(body)
        paragraph = result.get("prose")
        terms = result.get("original_terms")
        if (not isinstance(paragraph, str) or not paragraph.strip()
                or not isinstance(terms, list)):
            raise ValueError("teaching_part_invalid")
        prose.append(paragraph.strip())
        for term in terms:
            if not isinstance(term, str) or not contains_term(term, piece):
                raise ValueError("teaching_term_source_invalid")
            matching = [source for source in sources if contains_term(term, source["text"])]
            if not matching:
                raise ValueError("teaching_term_source_invalid")
            key = " ".join(term.split()).casefold()
            record = term_sources.setdefault(key, {
                "original_term": term, "text": piece, "context": context,
                "segment_ids": [], "page_ids": [],
            })
            references = record[f"{kind}_ids"]
            for source in matching:
                if source["id"] not in references:
                    references.append(source["id"])
    definitions: list[dict[str, Any]] = []
    for term_source in term_sources.values():
        reviewed = reviewed_definition(
            term_source["original_term"], term_source["text"], target, term_source["context"],
        )
        if reviewed is not None:
            # The card provider describes its generated prose. This reviewed
            # glossary entry uses no GPU call and is not attributed to inference.
            result = {"term": reviewed.term, "definition": reviewed.explanation}
        else:
            result = await generate({
                "phase": "definition", "text": term_source["text"],
                "context": term_source["context"],
                "original_term": term_source["original_term"], "target_language": target,
            })
        term, definition = result.get("term"), result.get("definition")
        if not isinstance(term, str) or not term.strip():
            raise ValueError("teaching_term_missing")
        if not isinstance(definition, str) or not definition.strip():
            raise ValueError("teaching_definition_missing")
        entry = {
            "term": term.strip(), "explanation": definition.strip(),
            "evidence_segment_ids": term_source["segment_ids"],
            "asset_page_ids": term_source["page_ids"],
        }
        if reviewed is not None:
            entry["background_reference"] = reviewed.reference
        # Equivalent source spellings can resolve to the same reviewed concept.
        # Merge only identical definitions, never homonyms with different meaning.
        existing = next((
            item for item in definitions
            if item["term"].casefold() == entry["term"].casefold()
            and item["explanation"] == entry["explanation"]
            and item.get("background_reference") == entry.get("background_reference")
        ), None)
        if existing is None:
            definitions.append(entry)
        else:
            for field in ("evidence_segment_ids", "asset_page_ids"):
                existing[field] = list(dict.fromkeys([*existing[field], *entry[field]]))
    return {
        "paragraph_summary": "\n\n".join(prose), "terms": definitions,
        "evidence_segment_ids": [item["id"] for item in segments],
        "asset_page_ids": [item["id"] for item in pages], "provider": provider,
    }
