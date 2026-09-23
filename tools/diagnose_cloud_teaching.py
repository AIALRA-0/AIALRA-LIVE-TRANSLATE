"""Opt-in, metadata-only diagnosis of one authorized teaching job from stdin.

Never print source text, model prose, credentials, object IDs, or request bodies.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
from typing import Any

import httpx

from workers.gpu_agent.kuafushe import KuafuTextClient
from workers.gpu_agent.teaching import assemble_explanation


async def diagnose(model_input: dict[str, Any]) -> None:
    async with httpx.AsyncClient() as http:
        cloud = KuafuTextClient(http)
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
        }))


if __name__ == "__main__":
    if os.getenv("AIALRA_RUN_LIVE_PROVIDER_TEST") != "1":
        raise SystemExit("explicit live-provider opt-in required")
    payload = json.load(sys.stdin)
    if not isinstance(payload, dict):
        raise SystemExit("one job input object is required")
    asyncio.run(diagnose(payload))
