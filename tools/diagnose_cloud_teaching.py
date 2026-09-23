"""Opt-in, metadata-only diagnosis of one authorized teaching job from stdin.

Never print source text, model prose, credentials, object IDs, or request bodies.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
from collections.abc import Callable
from typing import Any

import httpx

from workers.gpu_agent.kuafushe import KuafuTextClient
from workers.gpu_agent.teaching import (
    _complete_misconception_roles,
    assemble_explanation,
    parse_teaching_sections,
    valid_summary,
    valid_teaching_sections,
)
from workers.model_worker.teaching_format import generated_prose


class DiagnosticClient(KuafuTextClient):
    async def infer_json(
        self, system: str, user: str, schema: dict[str, Any], **kwargs: Any,
    ) -> dict[str, Any] | None:
        original_accept: Callable[[dict[str, Any]], bool] = kwargs.pop("accept")
        source = json.loads(user)
        source_characters = len(str(source.get("source", "")))
        language = str(source.get("target_language", ""))

        def checked(value: dict[str, Any]) -> bool:
            accepted = original_accept(value)
            prose = value.get("prose") if isinstance(value, dict) else None
            if not accepted and isinstance(prose, str):
                normalized = (
                    generated_prose(prose) if language.casefold().startswith("zh") else prose
                )
                sections = parse_teaching_sections(normalized)
                print(json.dumps({
                    "status": "rejected_shape",
                    "prose_characters": len(normalized),
                    "full_summary_valid": valid_summary(normalized, source_characters, language),
                    "sections_valid": valid_teaching_sections(
                        sections, source_characters, language,
                    ),
                    "legacy_input": sections["legacy_input"],
                    "main_lines": len(sections["main_content"].splitlines()),
                    "bridge_characters": len(sections["chapter_bridge"]),
                    "explanation_characters": len(sections["content_explanation"]),
                    "misconception_roles_complete": [
                        _complete_misconception_roles(item, language)
                        for item in sections["misconceptions"]
                    ],
                }))
            return accepted

        return await super().infer_json(system, user, schema, accept=checked, **kwargs)


async def diagnose(model_input: dict[str, Any]) -> None:
    async with httpx.AsyncClient() as http:
        cloud = DiagnosticClient(http)
        if not cloud.available:
            raise SystemExit("two cloud routes are not configured")

        async def call(body: dict[str, Any]) -> dict[str, Any]:
            phase = str(body.get("phase", "unknown"))
            try:
                result = await cloud.teaching_part(body)
            except ValueError as error:
                print(json.dumps({
                    "status": "part_failed",
                    "phase": phase,
                    "source_characters": len(str(body.get("text", ""))),
                    "category": str(error),
                    "routes": cloud.last_failures,
                    "shapes": cloud.last_shapes,
                }), file=sys.stderr)
                raise
            print(json.dumps({
                "status": "part_accepted",
                "phase": phase,
                "source_characters": len(str(body.get("text", ""))),
                "output_characters": len(str(result.get("prose", ""))),
            }))
            return result

        try:
            result = await assemble_explanation(model_input, call)
        except ValueError as error:
            print(
                json.dumps({"status": "assembly_failed", "category": str(error)}),
                file=sys.stderr,
            )
            raise SystemExit(1) from None
        print(json.dumps({
            "status": "accepted",
            "summary_characters": len(result["paragraph_summary"]),
            "term_count": len(result["terms"]),
            "cited_material_count": len(result["asset_page_ids"]),
        }))


if __name__ == "__main__":
    if os.getenv("AIALRA_RUN_LIVE_PROVIDER_TEST") != "1":
        raise SystemExit("explicit live-provider opt-in required")
    payload = json.load(sys.stdin)
    if not isinstance(payload, dict):
        raise SystemExit("one job input object is required")
    asyncio.run(diagnose(payload))
