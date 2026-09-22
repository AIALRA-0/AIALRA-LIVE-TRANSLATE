"""Synthetic direct-provider tests; no live tokens or course data enter snapshots."""

import json
import os
import shlex
import subprocess

import httpx
import pytest

from workers.gpu_agent.course_summary import compile_course
from workers.gpu_agent.kuafushe import KuafuTextClient, Route
from workers.gpu_agent.teaching import (
    _complete_misconception_roles,
    assemble_explanation,
    parse_teaching_sections,
    valid_summary,
    valid_teaching_sections,
)
from workers.model_worker.teaching import repetition_collapse


def fixture_routes() -> tuple[Route, Route]:
    return (
        Route("synthetic-one", "https://api.kuafushe.cc/v1", "test-ds"),
        Route("synthetic-two", "https://api.kuafushe.cc/v1", "test-ds"),
    )


@pytest.mark.asyncio
async def test_both_ds_routes_can_complete_independently() -> None:
    seen: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        assert request.headers["authorization"].startswith("Bearer synthetic-")
        seen.append(request.headers["authorization"])
        assert str(request.url) == "https://api.kuafushe.cc/v1/responses"
        return httpx.Response(200, json={"output_text": json.dumps({"answer": "valid"})})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        for preferred in (0, 1):
            client = KuafuTextClient(http, fixture_routes())
            client.preferred = preferred
            result = await client.infer_json(
                "instruction",
                "synthetic source",
                {},
                max_tokens=100,
                accept=lambda item: item == {"answer": "valid"},
            )
            assert result == {"answer": "valid"}
    assert seen == ["Bearer synthetic-one", "Bearer synthetic-two"]


@pytest.mark.asyncio
async def test_unavailable_route_fails_over_in_either_direction() -> None:
    for failed in ("synthetic-one", "synthetic-two"):
        seen: list[str] = []

        def handler(
            request: httpx.Request,
            *,
            _seen: list[str] = seen,
            _failed: str = failed,
        ) -> httpx.Response:
            token = request.headers["authorization"].split()[-1]
            _seen.append(token)
            if token == _failed:
                return httpx.Response(503, json={"error": "synthetic outage"})
            if str(request.url).endswith("/responses"):
                return httpx.Response(
                    200,
                    json={
                        "output": [
                            {
                                "type": "message",
                                "content": [
                                    {"type": "output_text", "text": '{"answer":"valid"}'},
                                ],
                            }
                        ]
                    },
                )
            return httpx.Response(
                200,
                json={
                    "choices": [
                        {
                            "message": {
                                "content": '{"answer":"valid"}',
                            }
                        }
                    ]
                },
            )

        async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
            client = KuafuTextClient(http, fixture_routes())
            client.preferred = 0 if failed == "synthetic-one" else 1
            result = await client.infer_json(
                "instruction",
                "synthetic source",
                {},
                max_tokens=100,
                accept=lambda item: item == {"answer": "valid"},
            )
            assert result == {"answer": "valid"}
            assert client.preferred == (1 if failed == "synthetic-one" else 0)
        assert seen == [failed, "synthetic-two" if failed == "synthetic-one" else "synthetic-one"]


@pytest.mark.asyncio
async def test_invalid_contract_uses_backup_without_publishing_bad_result() -> None:
    seen: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        token = request.headers["authorization"].split()[-1]
        seen.append(token)
        answer = "bad" if token == "synthetic-one" else "valid"
        if str(request.url).endswith("/responses"):
            return httpx.Response(200, json={"output_text": json.dumps({"answer": answer})})
        return httpx.Response(
            200,
            json={
                "choices": [
                    {
                        "message": {
                            "content": json.dumps({"answer": answer}),
                        }
                    }
                ]
            },
        )

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        client = KuafuTextClient(http, fixture_routes())
        result = await client.infer_json(
            "instruction",
            "synthetic source",
            {},
            max_tokens=100,
            accept=lambda item: item == {"answer": "valid"},
        )
    assert result == {"answer": "valid"}
    assert seen == ["synthetic-one", "synthetic-two"]


@pytest.mark.asyncio
async def test_incomplete_teaching_sections_switch_to_backup() -> None:
    seen: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        token = request.headers["authorization"].split()[-1]
        seen.append(token)
        end = "注意不要误解" if token == "synthetic-one" else "无"
        prose = (
            "承上启下：\n主要内容：\n- 故障模型限定测试对象\n"
            "内容讲解：\n故障模型是测试电路的一种抽象；黏着故障假设信号固定，"
            "测试向量仅能检查所覆盖的故障，不能证明所有物理缺陷均不存在。\n"
            f"易错点：{end}"
        )
        return httpx.Response(
            200,
            json={
                "output_text": json.dumps(
                    {
                        "prose": prose,
                        "original_terms": [],
                    },
                    ensure_ascii=False,
                )
            },
        )

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as http:
        cloud = KuafuTextClient(http, fixture_routes())
        result = await cloud.teaching_part(
            {
                "phase": "prose",
                "text": "A stuck-at fault model limits what tests can prove.",
                "target_language": "zh-CN",
            }
        )
    assert "故障模型" in result["prose"]
    assert seen == ["synthetic-one", "synthetic-two"]


@pytest.mark.asyncio
async def test_live_ds_routes_independently_when_explicitly_requested() -> None:
    if os.getenv("AIALRA_RUN_LIVE_PROVIDER_TEST") != "1":
        pytest.skip("live provider probe requires explicit opt-in")
    async with httpx.AsyncClient() as http:
        for preferred in (0, 1):
            client = KuafuTextClient(http)
            assert client.available
            client.preferred = preferred
            result = await client.infer_json(
                "Return only a JSON object with ok set to true.",
                "Synthetic connectivity probe; no course content.",
                {"type": "object", "properties": {"ok": {"type": "boolean"}}},
                max_tokens=80,
                accept=lambda value: value.get("ok") is True,
            )
            assert result == {"ok": True}
            assert client.preferred == preferred, (
                "route required backup instead of independent success"
            )


@pytest.mark.asyncio
async def test_live_failover_from_each_disabled_credential_when_requested() -> None:
    if os.getenv("AIALRA_RUN_LIVE_PROVIDER_TEST") != "1":
        pytest.skip("live provider probe requires explicit opt-in")
    async with httpx.AsyncClient() as http:
        configured = KuafuTextClient(http)
        assert configured.available
        for working in configured.routes:
            disabled = Route(
                "synthetic-disabled-credential", working.base_url, working.model, working.transport
            )
            client = KuafuTextClient(http, (disabled, working))
            result = await client.infer_json(
                "Return only a JSON object with ok set to true.",
                "Synthetic failover probe; no course content.",
                {"type": "object", "properties": {"ok": {"type": "boolean"}}},
                max_tokens=80,
                accept=lambda value: value.get("ok") is True,
            )
            assert result == {"ok": True}
            assert client.preferred == 1


@pytest.mark.asyncio
async def test_live_synthetic_teaching_flow_when_requested() -> None:
    if os.getenv("AIALRA_RUN_LIVE_PROVIDER_TEST") != "1":
        pytest.skip("live provider probe requires explicit opt-in")
    lesson = (
        "A fault model is an abstraction used to test a digital circuit. "
        "The stuck-at model assumes one circuit signal stays at zero or one. "
        "A test vector applies an input pattern; the expected output is compared "
        "with the observed output. A mismatch can reveal a fault, but a passing "
        "test does not prove that every physical defect is absent. The model "
        "helps choose tests, while coverage depends on which faults and vectors "
        "were considered."
    )
    async with httpx.AsyncClient() as http:
        cloud = KuafuTextClient(http)
        assert cloud.available
        parts: list[tuple[str, str]] = []

        async def call(body: dict[str, object]) -> dict[str, object]:
            value = await cloud.teaching_part(body)
            prose = value.get("prose")
            parts.append((str(body.get("phase")), prose if isinstance(prose, str) else ""))
            return value

        try:
            result = await assemble_explanation(
                {
                    "segments": [{"id": "synthetic-p1", "text": lesson}],
                    "asset_pages": [],
                    "target_language": "zh-CN",
                },
                call,
            )
        except ValueError as error:
            detail = next(
                (text for phase, text in reversed(parts) if phase in {"prose", "group"}), ""
            )
            sections = parse_teaching_sections(detail)
            misconception_roles = [
                _complete_misconception_roles(item, "zh-CN") for item in sections["misconceptions"]
            ]
            section_lengths = {
                key: len(value) if isinstance(value, (str, list)) else -1
                for key, value in sections.items()
            }
            raise AssertionError(
                f"teaching failure category: {cloud.last_failures}, shape: {cloud.last_shapes}, "
                f"parts: {[(phase, len(text)) for phase, text in parts]}, "
                f"summary_valid: {valid_summary(detail, len(lesson), 'zh-CN')}, "
                f"section_valid: {valid_teaching_sections(sections, len(lesson), 'zh-CN')}, "
                f"main_lines: {len(sections['main_content'].splitlines())}, "
                f"misconception_roles: {misconception_roles}, "
                f"misconceptions: {sections['misconceptions']!r}, "
                f"section_lengths: {section_lengths}"
            ) from error
    assert result["provider"].startswith("kuafushe:")
    assert result["evidence_segment_ids"] == ["synthetic-p1"]
    assert valid_teaching_sections(result["teaching_sections"], len(lesson), "zh-CN")


@pytest.mark.asyncio
async def test_live_synthetic_course_summary_when_requested() -> None:
    if os.getenv("AIALRA_RUN_LIVE_PROVIDER_TEST") != "1":
        pytest.skip("live provider probe requires explicit opt-in")
    async with httpx.AsyncClient() as http:
        cloud = KuafuTextClient(http)
        assert cloud.available
        result = await compile_course(
            {
                "segments": [
                    {
                        "id": "synthetic-p1",
                        "text": (
                            "A digital circuit fault model describes a limited set of possible "
                            "failures. A stuck-at fault assumes one signal stays at zero or one. "
                            "Test vectors expose some modeled faults, but a passing test does not "
                            "prove the absence of every physical defect."
                        ),
                    }
                ],
                "target_language": "zh-CN",
            },
            cloud.teaching_part,
        )
    assert result["provider"].startswith("kuafushe:")
    assert result["evidence_segment_ids"] == ["synthetic-p1"]
    assert result["overview"].strip()


@pytest.mark.asyncio
async def test_live_retained_course_teaching_without_logging_source() -> None:
    if (
        os.getenv("AIALRA_RUN_LEGACY_COURSE_TEST") != "1"
        or os.getenv("AIALRA_LEGACY_CLOUD_AUTHORIZED") != "1"
    ):
        pytest.skip("retained-course cloud probe requires explicit project authorization")
    session_id = os.environ["AIALRA_LEGACY_SESSION_ID"]
    database = os.environ["AIALRA_LEGACY_SQLITE_PATH"]
    session_chars = ",".join(str(ord(char)) for char in session_id)
    policy_sql = (
        "SELECT p.cloud_enabled,p.allowed_modalities_json FROM project_ai_policies p "
        "JOIN project_sessions ps ON ps.project_id=p.project_id WHERE "
        f"ps.session_id=char({session_chars});"
    )
    policy = subprocess.run(
        [
            "ssh",
            "-o",
            "BatchMode=yes",
            "contabo-vps-aialra",
            f"sqlite3 {shlex.quote(database)} {shlex.quote(policy_sql)}",
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if policy != '1|["text"]':
        pytest.skip("server policy does not authorize cloud text for this course")
    type_chars = ",".join(str(ord(char)) for char in "paragraph.finalized")
    sql = (
        "SELECT payload_json FROM events WHERE "
        f"session_id=char({session_chars}) AND event_type=char({type_chars}) "
        "ORDER BY ingested_at LIMIT 4 OFFSET 90;"
    )
    rows = subprocess.run(
        [
            "ssh",
            "-o",
            "BatchMode=yes",
            "contabo-vps-aialra",
            f"sqlite3 {shlex.quote(database)} {shlex.quote(sql)}",
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.splitlines()
    segments = []
    for raw in rows:
        payload = json.loads(raw)
        segments.append({"id": payload["paragraph_id"], "text": payload["text"]})
    assert len(segments) == 4
    async with httpx.AsyncClient() as http:
        cloud = KuafuTextClient(http)
        assert cloud.available
        try:
            result = await assemble_explanation(
                {
                    "segments": segments,
                    "asset_pages": [],
                    "target_language": "zh-CN",
                },
                cloud.teaching_part,
            )
        except Exception as error:
            # Never serialize course excerpts or provider bodies into CI output.
            raise AssertionError(
                f"retained-course teaching failed: {type(error).__name__}; "
                f"route categories={cloud.last_failures}"
            ) from None
    assert result["evidence_segment_ids"] == [item["id"] for item in segments]
    assert valid_teaching_sections(
        result["teaching_sections"], sum(len(item["text"]) for item in segments), "zh-CN"
    )
    assert not repetition_collapse(result["paragraph_summary"])
