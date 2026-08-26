"""Command-line entrypoint for an operator-approved DingTalk A1 probe."""

from __future__ import annotations

import argparse
import asyncio
import json
import os
from typing import Any

from .client import DingtalkA1Client, DingtalkConfigurationError


def parse_arguments() -> argparse.Namespace:
    """A JSON payload preserves new DingTalk fields without storing credentials in files."""

    parser = argparse.ArgumentParser(description="Probe DingTalk A1 OpenAPI capabilities")
    parser.add_argument(
        "operation",
        choices=[
            "control-recording",
            "list-audio",
            "download-audio",
            "get-minutes",
            "get-transcription",
            "create-transcription",
            "query-asr",
            "ai-summary",
        ],
    )
    parser.add_argument("--payload", default="{}", help="JSON object sent as body or query")
    parser.add_argument("--execute", action="store_true", help="Send the request to DingTalk")
    return parser.parse_args()


def parse_payload(raw: str) -> dict[str, Any]:
    """The probe accepts only a JSON object so operators cannot accidentally send ambiguous data."""

    parsed = json.loads(raw)
    if not isinstance(parsed, dict):
        raise ValueError("payload must be a JSON object")
    return parsed


async def run() -> int:
    """Dry-run is the default and execution requires an explicit switch plus environment token."""

    arguments = parse_arguments()
    payload = parse_payload(arguments.payload)
    token = os.getenv("DINGTALK_ACCESS_TOKEN", "dry-run-token")
    api_base = os.getenv("DINGTALK_API_BASE", "https://api.dingtalk.com")
    client = DingtalkA1Client(token, api_base)
    if not arguments.execute:
        request = client.describe(arguments.operation, payload)
        print(
            json.dumps(
                {
                    "dry_run": True,
                    "method": request.method,
                    "url": request.url,
                    "payload": request.body,
                },
                ensure_ascii=False,
                indent=2,
            )
        )
        return 0
    if token == "dry-run-token":
        raise DingtalkConfigurationError("DINGTALK_ACCESS_TOKEN is required with --execute")
    result = await client.execute(arguments.operation, payload)
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    # asyncio owns one short-lived HTTP client and exits after the selected probe completes.
    raise SystemExit(asyncio.run(run()))
