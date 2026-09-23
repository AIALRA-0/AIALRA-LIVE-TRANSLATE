"""Assemble a complete teaching card while yielding between bounded model calls."""

from __future__ import annotations

import re
from collections.abc import Awaitable, Callable
from typing import Any, TypeGuard

from workers.gpu_agent.reviewed_definitions import reviewed_definition
from workers.model_worker.teaching import repetition_collapse
from workers.model_worker.terminology import matching_technical_terms

PartCaller = Callable[[dict[str, Any]], Awaitable[dict[str, Any]]]
MAX_GROUP_TERMS = 8
MAX_DEFINITION_BATCH = 2
MAX_SOURCE_CHUNK_BYTES = 2800
MAX_SYNTHESIS_INPUT_BYTES = 6200


def optional_definition_failure(error: Exception) -> bool:
    """An invalid glossary completion must not discard verified teaching prose."""
    return isinstance(error, RuntimeError) or (
        isinstance(error, ValueError)
        and str(error) in {"cloud_teaching_contract_invalid", "teaching_part_invalid"}
    )


_BANNED_NARRATION = (
    "当我在讲解", "你会看到我所说", "老师说", "讲者提到",
    "本段话讲了", "本段内容讲了", "让我们来看",
)

_SECTION_NAMES = {
    "chapter_bridge": ("承上启下", "chapter bridge"),
    "main_content": ("主要内容", "main content"),
    "professional_terms": ("专业术语", "professional terms", "terms"),
    "content_explanation": ("内容讲解", "完整讲解", "content explanation", "full explanation"),
    "misconceptions": ("易错点", "misconceptions", "common mistakes"),
}
_SECTION_LOOKUP = {
    alias.casefold(): key for key, aliases in _SECTION_NAMES.items() for alias in aliases
}
_SECTION_LINE = re.compile(
    r"^\s*(?:#{1,6}\s*)?(?:\*\*)?"
    r"(承上启下|主要内容|专业术语|内容讲解|完整讲解|易错点|"
    r"chapter bridge|main content|professional terms|terms|content explanation|"
    r"full explanation|misconceptions|common mistakes)"
    r"(?:\*\*)?\s*(?:[：:]\s*(?:\*\*)?\s*(.*))?$",
    re.IGNORECASE,
)


def valid_provider(value: Any) -> TypeGuard[str]:
    """Accept only the two configured inference lanes and their device suffixes."""
    return isinstance(value, str) and bool(
        re.fullmatch(r"ollama:[A-Za-z0-9_.:/+-]+@cuda", value)
        or re.fullmatch(r"kuafushe:[A-Za-z0-9_.:/+-]+@cloud", value)
    )


def parse_teaching_sections(
    value: str, terms: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """Parse labelled output; map older unlabelled prose without rewriting it."""
    sections: dict[str, list[str]] = {key: [] for key in _SECTION_NAMES}
    active: str | None = None
    saw_explanation = False
    for line in value.splitlines():
        match = _SECTION_LINE.fullmatch(line)
        if match:
            active = _SECTION_LOOKUP[match.group(1).casefold()]
            saw_explanation = saw_explanation or active == "content_explanation"
            if match.group(2):
                sections[active].append(match.group(2).strip())
            continue
        if active is not None:
            sections[active].append(line)

    clean = {key: "\n".join(lines).strip() for key, lines in sections.items()}
    explanation = clean["content_explanation"]
    if not saw_explanation:
        # Legacy paragraph_summary values remain the complete explanation. The
        # short summary is a verbatim excerpt, never an additional inference.
        explanation = value.strip()
        sentences = [part.strip() for part in re.split(r"(?<=[。！？.!?])\s*|\r?\n+", explanation)
                    if part.strip()]
        clean["main_content"] = "\n".join(f"- {part}" for part in sentences[:5])
        clean["chapter_bridge"] = ""
        clean["misconceptions"] = ""

    misconception_text = clean["misconceptions"].strip()
    if misconception_text.strip("。.!！ ") in {"无", "暂无", "没有", "未提及"}:
        misconception_text = ""
    if (misconception_text.lstrip("（(").startswith(
            ("原文未提供", "原文没有提供", "材料未提供", "本段未提供"))
            and all(label in misconception_text for label in
                    ("错误理解", "错因", "正确判断", "核对方法"))):
        # An explicit absence note is not a misconception. The source cannot
        # support four roles, so this optional section stays empty.
        misconception_text = ""
    # Compatible providers often put each of the four required roles in its
    # own bullet. Those bullets form one misconception, not four incomplete
    # misconceptions. Canonicalize labels only; never fill in missing roles.
    role_pattern = re.compile(
        r"^\s*(?:[-*+]\s+)?(?:\*\*)?(错误理解|错因|正确判断|核对方法)"
        r"(?:\*\*)?\s*[：:]\s*(?:\*\*)?\s*(.*)$"
    )
    roles: list[tuple[str, str]] = []
    for line in misconception_text.splitlines():
        if match := role_pattern.match(line):
            roles.append((match.group(1), match.group(2).strip()))
        elif line.strip() and roles:
            name, body = roles[-1]
            roles[-1] = (name, f"{body}\n{line.strip()}")
    expected_roles = ("错误理解", "错因", "正确判断", "核对方法")
    if (roles and len(roles) % 4 == 0
            and [name for name, _ in roles] == list(expected_roles) * (len(roles) // 4)):
        misconceptions = ["\n\n".join(
            f"**{name}：** {body}" for name, body in roles[index:index + 4]
        ) for index in range(0, len(roles), 4)]
    elif re.search(r"(?m)^\s*[-*+]\s+", misconception_text):
        misconceptions = [part.strip() for part in re.split(
            r"(?m)(?=^\s*[-*+]\s+)", misconception_text
        ) if part.strip()]
    else:
        misconceptions = [misconception_text] if misconception_text else []
    return {
        "version": 1,
        "chapter_bridge": clean["chapter_bridge"],
        "main_content": clean["main_content"],
        "professional_terms": terms or [],
        "content_explanation": explanation,
        "misconceptions": misconceptions,
        "legacy_input": not saw_explanation,
    }


def valid_teaching_sections(
    sections: dict[str, Any], source_characters: int, language: str,
) -> bool:
    explanation = sections.get("content_explanation")
    main = sections.get("main_content")
    bridge = sections.get("chapter_bridge")
    misconceptions = sections.get("misconceptions")
    terms = sections.get("professional_terms")
    main_items = (
        [line for line in main.splitlines() if line.strip()]
        if isinstance(main, str) else []
    )
    return bool(
        isinstance(explanation, str)
        and valid_summary(explanation, source_characters, language)
        and isinstance(main, str) and bool(main.strip())
        and len(main) <= 900 and 1 <= len(main_items) <= 5
        and isinstance(bridge, str) and len(bridge) <= 500
        and isinstance(misconceptions, list) and len(misconceptions) <= 5
        and all(
            isinstance(item, str) and len(item) <= 700
            and _complete_misconception_roles(item, language)
            for item in misconceptions
        )
        and isinstance(terms, list)
    )


def _complete_misconception_roles(value: str, language: str) -> bool:
    roles = (
        ("错误理解", "错因", "正确判断", "核对方法")
        if language.casefold().startswith("zh")
        else ("misconception", "cause", "correction", "check")
    )
    labels = re.findall(
        r"\*\*(错误理解|错因|正确判断|核对方法)[：:]\*\*|"
        r"\*\*(misconception|cause|correction|check):\*\*",
        value,
        re.IGNORECASE,
    )
    found = [next(label for label in pair if label).casefold() for pair in labels]
    return tuple(found) == tuple(role.casefold() for role in roles)


def valid_summary(value: str, source_characters: int, language: str) -> bool:
    del language  # Core's final gate is content-based; language is checked by the Worker.
    text = value.strip()
    return bool(
        text
        and not any(phrase in text for phrase in _BANNED_NARRATION)
        and not repetition_collapse(text)
        and len(text) <= 1200
        and (source_characters < 240 or len(text) >= 80)
    )


def valid_definition(value: str, language: str) -> bool:
    text = value.strip()
    return bool(
        text
        and (not language.casefold().startswith("zh")
             or (50 <= len(text) <= 240 and text.count("；") >= 2))
    )


def contains_term(term: str, source: str) -> bool:
    """A term citation needs a lexical occurrence, not RAM inside program."""
    if not term.strip():
        return False
    left = r"(?<![A-Za-z0-9_])" if term[0].isascii() and term[0].isalnum() else ""
    right = r"(?![A-Za-z0-9_])" if term[-1].isascii() and term[-1].isalnum() else ""
    return re.search(left + re.escape(term) + right, source) is not None


def redundant_bilingual_name(value: str) -> bool:
    """Reject labels such as ``Alice (Alice)`` before publication."""
    matched = re.fullmatch(r"\s*([^()（）]+?)\s*[（(]\s*([^()（）]+?)\s*[）)]\s*", value)
    return bool(matched and matched.group(1).casefold() == matched.group(2).casefold())


def person_reference(term: str, source: str) -> bool:
    """A self-introduced speaker name is evidence about a person, not a concept."""
    candidate = re.escape(term.strip())
    if not candidate:
        return False
    patterns = [
        rf"\b(?:my name is|i am|i'm|call me|professor)\s+{candidate}\b",
        rf"\b{candidate}\s+(?:is my name|is the (?:speaker|professor|instructor))\b",
    ]
    return any(re.search(pattern, source, re.IGNORECASE) for pattern in patterns)


def source_pieces(text: str, capacity: int = MAX_SOURCE_CHUNK_BYTES) -> list[str]:
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
            if pending and used + size > MAX_SOURCE_CHUNK_BYTES:
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
        if not valid_provider(observed):
            raise ValueError("teaching_provider_unverified")
        if provider and observed != provider:
            raise ValueError("teaching_provider_changed")
        provider = observed
        return result

    prose: list[str] = []
    term_sources: dict[str, dict[str, Any]] = {}
    # Material pages are reference data, not separate lecture passages. A short
    # page must not have to satisfy the full teaching-card writing contract.
    chunks = [("segment", chunk) for chunk in teaching_chunks(segments)]
    texts = ["\n\n".join(item["text"] for item in chunk) for _, chunk in chunks]
    cited_page_ids: list[str] = []
    # All source fragments finish before a card is published. A failed call
    # leaves the single persistent job retryable, never a fake partial card.
    for index, (kind, sources) in enumerate(chunks):
        piece = texts[index]
        # The topic is already sealed. Both neighbours are available and help
        # disambiguate abbreviations introduced before their full explanation.
        candidate_context = texts[max(0, index - 1):index] + texts[index + 1:index + 2]
        capacity = 6500 - len(piece.encode())
        context: list[str] = []
        for neighbour in candidate_context:
            size = len(neighbour.encode())
            if size <= capacity:
                context.append(neighbour)
                capacity -= size
        material_references: list[str] = []
        material_page_ids: list[str] = []
        for page in pages:
            size = len(page["text"].encode())
            if size <= capacity and len(material_references) < 4:
                material_references.append(page["text"])
                material_page_ids.append(page["id"])
                capacity -= size
        body = {"phase": "prose", "text": piece, "context": context,
                "material_references": material_references,
                "target_language": target}
        result = await generate(body)
        paragraph = result.get("prose")
        terms = result.get("original_terms")
        used_material = result.get("used_material_indices", [])
        if (not isinstance(paragraph, str) or not paragraph.strip()
                or not isinstance(terms, list)):
            raise ValueError("teaching_part_invalid")
        if (not isinstance(used_material, list)
                or any(type(item) is not int or not 0 <= item < len(material_page_ids)
                       for item in used_material)
                or len(used_material) != len(set(used_material))):
            raise ValueError("teaching_material_reference_invalid")
        for material_index in used_material:
            page_id = material_page_ids[material_index]
            if page_id not in cited_page_ids:
                cited_page_ids.append(page_id)
        prose.append(paragraph.strip())
        for term in terms:
            if not isinstance(term, str) or not contains_term(term, piece):
                continue
            if person_reference(term, piece):
                continue
            matching = [source for source in sources if contains_term(term, source["text"])]
            if not matching:
                continue
            key = " ".join(term.split()).casefold()
            record = term_sources.setdefault(key, {
                "original_term": term, "text": piece, "context": context,
                "segment_ids": [], "page_ids": [],
            })
            references = record[f"{kind}_ids"]
            for source in matching:
                if source["id"] not in references:
                    references.append(source["id"])
    # A verified glossary entry should not disappear merely because a model
    # omitted it from the optional inventory. Both source presence and the
    # reviewed domain-specific definition remain mandatory.
    for source_term, _preferred in matching_technical_terms(
        [source["text"] for source in segments], target,
    ):
        for index, (kind, sources) in enumerate(chunks):
            if kind != "segment" or not contains_term(source_term, texts[index]):
                continue
            piece = texts[index]
            context = texts[max(0, index - 1):index] + texts[index + 1:index + 2]
            if reviewed_definition(source_term, piece, target, context) is None:
                continue
            key = " ".join(source_term.split()).casefold()
            record = term_sources.setdefault(key, {
                "original_term": source_term, "text": piece, "context": context,
                "segment_ids": [], "page_ids": [],
            })
            for source in sources:
                if (contains_term(source_term, source["text"])
                        and source["id"] not in record["segment_ids"]):
                    record["segment_ids"].append(source["id"])
    definitions: list[dict[str, Any]] = []

    def append_definition(
        term_source: dict[str, Any], term: Any, definition: Any, reference: str | None = None,
    ) -> None:
        # Glossary entries are useful but optional. One malformed entry must
        # never discard complete, evidence-bound teaching prose.
        if (
            not isinstance(term, str)
            or not term.strip()
            or not isinstance(definition, str)
            or not valid_definition(definition, target)
            or redundant_bilingual_name(term)
        ):
            return
        entry = {
            "term": term.strip(), "explanation": definition.strip(),
            "evidence_segment_ids": term_source["segment_ids"],
            "asset_page_ids": term_source["page_ids"],
        }
        if reference is not None:
            entry["background_reference"] = reference
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

    unresolved: list[dict[str, Any]] = []
    for term_source in list(term_sources.values())[:MAX_GROUP_TERMS]:
        reviewed = reviewed_definition(
            term_source["original_term"], term_source["text"], target, term_source["context"],
        )
        if reviewed is not None:
            # The card provider describes its generated prose. This reviewed
            # glossary entry uses no GPU call and is not attributed to inference.
            append_definition(
                term_source, reviewed.term, reviewed.explanation, reviewed.reference,
            )
        else:
            unresolved.append(term_source)

    while unresolved:
        first = unresolved.pop(0)
        batch = [first]
        for candidate in list(unresolved):
            if len(batch) == MAX_DEFINITION_BATCH:
                break
            if candidate["text"] == first["text"] and candidate["context"] == first["context"]:
                batch.append(candidate)
                unresolved.remove(candidate)
        if len(batch) == 1:
            try:
                result = await generate({
                    "phase": "definition", "text": first["text"],
                    "context": first["context"],
                    "original_term": first["original_term"], "target_language": target,
                })
            except (RuntimeError, ValueError) as error:
                if not optional_definition_failure(error):
                    raise
                continue
            append_definition(first, result.get("term"), result.get("definition"))
        else:
            try:
                result = await generate({
                    "phase": "definitions", "text": first["text"],
                    "context": first["context"],
                    "original_terms": [item["original_term"] for item in batch],
                    "target_language": target,
                })
            except (RuntimeError, ValueError) as error:
                if not optional_definition_failure(error):
                    raise
                for term_source in batch:
                    try:
                        fallback = await generate({
                            "phase": "definition", "text": term_source["text"],
                            "context": term_source["context"],
                            "original_term": term_source["original_term"],
                            "target_language": target,
                        })
                    except (RuntimeError, ValueError) as fallback_error:
                        if not optional_definition_failure(fallback_error):
                            raise
                        continue
                    append_definition(
                        term_source, fallback.get("term"), fallback.get("definition"),
                    )
                continue
            generated = result.get("definitions")
            if not isinstance(generated, list) or len(generated) != len(batch):
                continue
            for term_source, item in zip(batch, generated, strict=True):
                if (
                    not isinstance(item, dict)
                    or item.get("original_term") != term_source["original_term"]
                ):
                    continue
                append_definition(term_source, item.get("term"), item.get("definition"))
    detailed_prose = "\n\n".join(prose)
    # The model endpoint accepts up to 6,500 source/context bytes.  The old
    # 3,500-byte guard skipped synthesis for two ordinary complete drafts,
    # then rejected their concatenation at the final 1,200-character gate.
    # Keep a small allowance for request structure while using the capacity
    # that the endpoint actually validates.
    if len(chunks) > 1 and len(detailed_prose.encode()) <= MAX_SYNTHESIS_INPUT_BYTES:
        try:
            guide = await generate({"phase": "group", "text": detailed_prose,
                                    "target_language": target})
        except RuntimeError:
            guide = None
        if guide is None:
            # Piece drafts are an internal completeness scaffold, not reader-facing prose.
            # Publishing them side-by-side recreates a transcript-like wall of text and
            # makes a failed synthesis look successful.
            raise ValueError("teaching_group_synthesis_invalid")
        heading = guide.get("prose")
        if not isinstance(heading, str) or not heading.strip():
            raise ValueError("teaching_group_synthesis_invalid")
        # The group synthesis already covers all bounded notes. Publishing the
        # intermediate per-piece drafts beneath it repeats ideas and often
        # reads like stitched transcript fragments in the narrow learning rail.
        detailed_prose = heading.strip()
    source_characters = sum(len(item["text"]) for item in segments)
    teaching_sections = parse_teaching_sections(detailed_prose, definitions)
    if (not valid_summary(detailed_prose, source_characters, target)
            or not valid_teaching_sections(teaching_sections, source_characters, target)):
        raise ValueError("teaching_summary_quality_invalid")
    return {
        "paragraph_summary": detailed_prose, "terms": definitions,
        "teaching_sections": teaching_sections,
        "evidence_segment_ids": [item["id"] for item in segments],
        "asset_page_ids": cited_page_ids, "provider": provider,
    }
