"""Small, reviewed technical glossary; never rewrite source text or guess acronyms.

Memory naming: AMD UG1079 (AI Engine memory banks).
Timing/activity naming: AMD UG835 set_switching_activity and UG440.
Only unambiguous full phrases select entries; an isolated 'bank' never selects
a memory term. The translator still applies a term only to the matching concept.
"""

import re

_TERMS = (
    (r"\bmemory banks?\b", "memory bank", "存储体"),
    (r"\bbank conflicts?\b", "bank conflict", "存储体冲突"),
    (r"\bsetup time\b", "setup time", "建立时间"),
    (r"\bhold time\b", "hold time", "保持时间"),
    (r"\bswitching activity factor\b", "switching activity factor", "翻转活动因子"),
    (r"\bdynamic switching power\b", "dynamic switching power", "动态开关功耗"),
    (r"\bclock gating\b", "clock gating", "时钟门控"),
)


def matching_technical_terms(texts: list[str], target_language: str) -> list[tuple[str, str]]:
    """Select known names from current/preceding source, never from prior translations."""
    if target_language.casefold().split("-", 1)[0] != "zh":
        return []
    result = [
        (source, preferred)
        for pattern, source, preferred in _TERMS
        if any(re.search(pattern, text, re.IGNORECASE) for text in texts)
    ]
    # FM is ambiguous outside circuit partitioning. These are naming hints,
    # not an instruction to alter source text or invent an acronym expansion.
    if any(re.search(r"\b(partitioning|hypergraph|cutsize|cut size)\b", text, re.I)
           for text in texts):
        for pattern, source, preferred in [
            (r"\bFM\b", "FM", "FM 电路划分算法（Fiduccia–Mattheyses）"),
            (r"\bcut ?size\b", "cut size", "割大小"),
        ]:
            if any(re.search(pattern, text, re.I) for text in texts):
                result.append((source, preferred))
    # A mechanical door latch is not a storage element. Select timing names
    # only when the source establishes the digital-circuit context (AMD UG574).
    if any(re.search(r"\b(clock|flip.flop|logic level|timing)\b", text, re.I) for text in texts):
        for pattern, source, preferred in [
            (r"\blatches?\b|\blatch\b", "latch", "锁存器"),
            (r"\bflip.flops?\b", "flip-flop", "触发器"),
        ]:
            if any(re.search(pattern, text, re.I) for text in texts):
                result.append((source, preferred))
    return result
