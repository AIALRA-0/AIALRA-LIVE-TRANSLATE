"""Minimal DingTalk DVI client built from the current official OpenAPI contracts."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import httpx


class DingtalkConfigurationError(ValueError):
    """Configuration errors fail before a request can disclose data to an unintended host."""


@dataclass(frozen=True)
class ProbeRequest:
    """Dry-run output exposes method and path while redacting the access token."""

    method: str
    url: str
    body: dict[str, Any] | None


class DingtalkA1Client:
    """Cover A1 control and post-recording compensation without claiming live PCM access."""

    def __init__(
        self,
        access_token: str,
        api_base: str = "https://api.dingtalk.com",
        transport: httpx.AsyncBaseTransport | None = None,
    ) -> None:
        if not access_token.strip():
            raise DingtalkConfigurationError("DINGTALK_ACCESS_TOKEN is required")
        normalized_base = api_base.rstrip("/")
        if not normalized_base.startswith("https://") and not transport:
            raise DingtalkConfigurationError("DingTalk API base must use HTTPS")
        self._base = normalized_base
        self._headers = {"x-acs-dingtalk-access-token": access_token}
        self._transport = transport

    def describe(self, operation: str, payload: dict[str, Any] | None) -> ProbeRequest:
        """Let operators inspect a deterministic request before using real credentials."""

        method, path = self._operation(operation)
        return ProbeRequest(method=method, url=f"{self._base}{path}", body=payload)

    async def execute(
        self, operation: str, payload: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        """Only the selected official path receives the explicit probe payload."""

        method, path = self._operation(operation)
        async with httpx.AsyncClient(
            base_url=self._base,
            headers=self._headers,
            timeout=30,
            transport=self._transport,
        ) as client:
            if method == "GET":
                response = await client.get(path, params=payload or {})
            else:
                response = await client.post(path, json=payload or {})
            response.raise_for_status()
            data = response.json()
            if not isinstance(data, dict):
                raise ValueError("DingTalk returned a non-object response")
            return data

    @staticmethod
    def _operation(operation: str) -> tuple[str, str]:
        """An allowlist prevents arbitrary requests from becoming a credential-bearing proxy."""

        operations = {
            "control-recording": ("POST", "/v1.0/dvi/devices/recording/control"),
            "list-audio": ("POST", "/v1.0/dvi/device/audio/list"),
            "download-audio": ("POST", "/v1.0/dvi/device/audio/download"),
            "get-minutes": ("GET", "/v1.0/dvi/audios/minutes"),
            "get-transcription": ("GET", "/v1.0/dvi/asr/transcriptions"),
            "create-transcription": ("POST", "/v1.0/dvi/asr/transcriptions"),
            "query-asr": ("GET", "/v1.0/dvi/asr/query"),
            "ai-summary": ("POST", "/v1.0/minutes/smartdevice/aisummary"),
        }
        try:
            return operations[operation]
        except KeyError as error:
            raise DingtalkConfigurationError(f"unsupported operation: {operation}") from error
