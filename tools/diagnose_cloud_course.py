"""Run one opt-in cloud course summary and emit metadata only."""

from __future__ import annotations

import asyncio
import json
import os
import re
import sys
import time
from typing import Any

import httpx

from workers.gpu_agent.course_summary import compile_course
from workers.gpu_agent.kuafushe import KuafuTextClient

_HTTP_FAILURE = re.compile(r"http_(\d{3})\Z")
_KNOWN_FAILURES = {"contract_rejected", "transport_error", "invalid_json"}
_PHASES = {"group", "course", "course_reduce"}


def _status_categories(failures: Any) -> list[str]:
    categories: list[str] = []
    if not isinstance(failures, list):
        return categories
    for failure in failures:
        if isinstance(failure, str) and failure in _KNOWN_FAILURES:
            category = failure
        elif isinstance(failure, str) and (match := _HTTP_FAILURE.fullmatch(failure)):
            status = int(match.group(1))
            category = "http_4xx" if 400 <= status < 500 else (
                "http_5xx" if 500 <= status < 600 else "http_other"
            )
        else:
            category = "other"
        if category not in categories:
            categories.append(category)
    return categories


def _emit(record: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(record, separators=(",", ":")) + "\n")


async def diagnose(model_input: dict[str, Any]) -> None:
    async with httpx.AsyncClient() as http:
        setup_started = time.perf_counter()
        cloud = KuafuTextClient(http)
        if not cloud.available:
            _emit({
                "phase_index": 0,
                "phase_type": "setup",
                "input_bytes": 0,
                "elapsed_ms": round((time.perf_counter() - setup_started) * 1000),
                "route_status_categories": ["not_configured"],
            })
            raise RuntimeError

        phase_index = 0

        async def call(body: dict[str, Any]) -> dict[str, Any]:
            nonlocal phase_index
            phase_index += 1
            phase = body.get("phase")
            phase_type = phase if isinstance(phase, str) and phase in _PHASES else "other"
            source = body.get("text")
            input_bytes = len(source.encode("utf-8")) if isinstance(source, str) else 0
            started = time.perf_counter()
            succeeded = False
            try:
                result = await cloud.teaching_part(body)
                succeeded = True
                return result
            finally:
                categories = _status_categories(cloud.last_failures)
                outcome = "accepted" if succeeded else "call_failed"
                if outcome not in categories:
                    categories.append(outcome)
                _emit({
                    "phase_index": phase_index,
                    "phase_type": phase_type,
                    "input_bytes": input_bytes,
                    "elapsed_ms": round((time.perf_counter() - started) * 1000),
                    "route_status_categories": categories,
                })

        result = await compile_course(model_input, call)
        segments = model_input["segments"]
        asset_pages = model_input.get("asset_pages", [])
        _emit({
            "record_type": "final",
            "summary_characters": len(result["overview"]),
            "chapter_count": len(result["key_points"]),
            "terminology_count": len(result["terminology"]),
            "covered_segment_count": len(result["evidence_segment_ids"]),
            "input_segment_count": len(segments),
            "covered_asset_page_count": len(result["asset_page_ids"]),
            "input_asset_page_count": len(asset_pages),
        })


def main() -> int:
    if os.getenv("AIALRA_RUN_LIVE_PROVIDER_TEST") != "1":
        return 2
    try:
        model_input = json.load(sys.stdin)
        if not isinstance(model_input, dict):
            return 2
        asyncio.run(diagnose(model_input))
    except Exception:
        # Exceptions may contain provider content, request details, or secrets.
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
