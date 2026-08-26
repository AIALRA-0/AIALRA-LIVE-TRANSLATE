"""The A1 probe tests verify routes, headers, payloads, and the HTTPS boundary."""

from __future__ import annotations

import json

import httpx
import pytest

from tools.a1_probe import DingtalkA1Client, DingtalkConfigurationError


@pytest.mark.asyncio
async def test_control_recording_uses_official_dvi_route_and_token_header() -> None:
    """A mocked request validates the contract without contacting a real organization or device."""

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/v1.0/dvi/devices/recording/control"
        assert request.headers["x-acs-dingtalk-access-token"] == "test-token"
        assert json.loads(request.content)["action"] == "start"
        return httpx.Response(200, json={"requestId": "req-1"})

    client = DingtalkA1Client(
        "test-token",
        api_base="http://test.local",
        transport=httpx.MockTransport(handler),
    )
    result = await client.execute("control-recording", {"action": "start"})
    assert result == {"requestId": "req-1"}


@pytest.mark.asyncio
async def test_get_minutes_places_identifier_in_query_string() -> None:
    """The minutes lookup is a GET request whose identifier stays out of the URL path."""

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/v1.0/dvi/audios/minutes"
        assert request.url.params["minutesId"] == "minutes-1"
        return httpx.Response(200, json={"minutesId": "minutes-1"})

    client = DingtalkA1Client(
        "test-token",
        api_base="http://test.local",
        transport=httpx.MockTransport(handler),
    )
    result = await client.execute("get-minutes", {"minutesId": "minutes-1"})
    assert result["minutesId"] == "minutes-1"


def test_real_api_base_requires_https() -> None:
    """Credential-bearing requests refuse plaintext transport outside isolated tests."""

    with pytest.raises(DingtalkConfigurationError):
        DingtalkA1Client("secret", "http://api.dingtalk.com")
