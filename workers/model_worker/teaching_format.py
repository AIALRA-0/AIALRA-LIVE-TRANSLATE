"""Presentation-only normalization of generated teaching, never source events.

Follow the user's human-readable writing contract. Preserve quotations, code,
math, names and numerical values; this is not a factual correctness validator.
"""

from __future__ import annotations

import re

_PROTECTED = re.compile(
    r'```[\s\S]*?```|`[^`\n]*`|\$\$[\s\S]*?\$\$|\$[^$\n]+\$'
    r'|“[^”]*”|「[^」]*」|『[^』]*』|"[^"\n]*"'
)


def generated_prose(text: str) -> str:
    """Keep protected spans verbatim and normalize only generated punctuation."""
    parts: list[str] = []
    offset = 0
    for match in _PROTECTED.finditer(text):
        parts.append(_punctuation(text[offset:match.start()], final=False))
        parts.append(match.group())
        offset = match.end()
    parts.append(_punctuation(text[offset:]))
    return "".join(parts).strip()


def _punctuation(text: str, *, final: bool = True) -> str:
    end = r"(?:\n|$)" if final else r"\n"
    return re.sub(r"[。；]+(?=[ \t]*" + end + ")", "", text).replace("。", "；")


def bilingual_term(text: str) -> str:
    """Reorder existing bilingual names only; never invent a name or expansion."""
    value = text.strip()
    matched = re.fullmatch(r"([^()（）]+?)\s*[（(]([^()（）]+)[）)]", value)
    if not matched:
        return value
    outer, inner = (part.strip() for part in matched.groups())
    outer_cjk = any("\u4e00" <= char <= "\u9fff" for char in outer)
    inner_cjk = any("\u4e00" <= char <= "\u9fff" for char in inner)
    if not outer_cjk and inner_cjk:
        return f"{inner}（{outer}）"
    if outer_cjk and not inner_cjk:
        return f"{outer}（{inner}）"
    return value
