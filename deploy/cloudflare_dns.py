#!/usr/bin/env python3
"""Create or update one proxied Cloudflare DNS record without printing credentials."""

from __future__ import annotations

import argparse
import json
import re
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


API = "https://api.cloudflare.com/client/v4"


def read_token(path: Path) -> str:
    text = path.read_text(encoding="utf-8")
    match = re.search(r"^\s*dns_cloudflare_api_token\s*=\s*(.+?)\s*$", text, re.MULTILINE)
    if not match:
        raise RuntimeError("Cloudflare API token was not found in the credential file")
    return match.group(1).strip()


def request(token: str, method: str, path: str, body: dict[str, Any] | None = None) -> Any:
    payload = json.dumps(body).encode() if body is not None else None
    call = urllib.request.Request(
        f"{API}{path}",
        data=payload,
        method=method,
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(call, timeout=20) as response:
        result = json.load(response)
    if not result.get("success"):
        raise RuntimeError("Cloudflare API request failed")
    return result["result"]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--zone", required=True)
    parser.add_argument("--host", required=True)
    parser.add_argument("--origin-record", required=True)
    parser.add_argument("--credentials", required=True, type=Path)
    args = parser.parse_args()

    token = read_token(args.credentials)
    zones = request(token, "GET", "/zones?" + urllib.parse.urlencode({"name": args.zone}))
    if len(zones) != 1:
        raise RuntimeError("Expected exactly one matching Cloudflare zone")
    zone_id = zones[0]["id"]

    def records(name: str) -> list[dict[str, Any]]:
        query = urllib.parse.urlencode({"type": "A", "name": name})
        return request(token, "GET", f"/zones/{zone_id}/dns_records?{query}")

    origins = records(args.origin_record)
    if len(origins) != 1:
        raise RuntimeError("Expected exactly one reference A record")

    body = {
        "type": "A",
        "name": args.host,
        "content": origins[0]["content"],
        "ttl": 1,
        "proxied": True,
        "comment": "Managed by AIALRA-LIVE-TRANSLATE deployment",
    }
    existing = records(args.host)
    if existing:
        request(token, "PUT", f"/zones/{zone_id}/dns_records/{existing[0]['id']}", body)
        action = "updated"
    else:
        request(token, "POST", f"/zones/{zone_id}/dns_records", body)
        action = "created"
    print(f"CLOUDFLARE_DNS_{action.upper()}")


if __name__ == "__main__":
    main()
