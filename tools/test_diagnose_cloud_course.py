"""Synthetic tests for the cloud course diagnostic's metadata-only output."""

from __future__ import annotations

import io
import json
from typing import Any

from tools import diagnose_cloud_course
from workers.gpu_agent.kuafushe import KuafuTextClient


def test_diagnostic_outputs_metadata_only_with_synthetic_client(
    monkeypatch: Any,
    capsys: Any,
) -> None:
    monkeypatch.setenv("AIALRA_RUN_LIVE_PROVIDER_TEST", "1")
    monkeypatch.setenv("AIALRA_KUAFUSHE_DS_PRIMARY_KEY", "CREDENTIAL_SENTINEL")
    monkeypatch.setenv("AIALRA_KUAFUSHE_DS_BACKUP_KEY", "SECOND_CREDENTIAL_SENTINEL")
    segments = [
        {
            "id": "SOURCE_ID_SENTINEL" if index == 0 else f"synthetic-{index}",
            "text": ("SOURCE_TEXT_SENTINEL " if index == 0 else "")
            + f"Synthetic passage {index}: " + "connected facts. " * 18,
        }
        for index in range(283)
    ]
    monkeypatch.setattr(
        "sys.stdin",
        io.StringIO(json.dumps({
            "segments": segments,
            "target_language": "en",
        })),
    )
    observed_input_bytes: list[int] = []

    async def synthetic_teaching_part(
        client: Any,
        body: dict[str, Any],
    ) -> dict[str, Any]:
        observed_input_bytes.append(len(body["text"].encode("utf-8")))
        client.last_failures = ["http_503", "ROUTE_SECRET_SENTINEL"]
        return {
            "prose": "概念关系" * (75 if body["phase"] == "course_reduce" else 200)
            + ("MODEL_PROSE_SENTINEL" if body["phase"] == "course" else ""),
            "provider": "kuafushe:synthetic@cloud",
        }

    monkeypatch.setattr(
        KuafuTextClient,
        "teaching_part",
        synthetic_teaching_part,
    )

    assert diagnose_cloud_course.main() == 0
    captured = capsys.readouterr()
    records = [json.loads(line) for line in captured.out.splitlines()]
    phases = [record["phase_type"] for record in records[:-1]]

    assert "group" in phases
    assert "course" in phases
    assert "course_reduce" in phases
    assert [record["phase_index"] for record in records[:-1]] == list(
        range(1, len(observed_input_bytes) + 1),
    )
    assert all(set(record) == {
        "phase_index", "phase_type", "input_bytes", "elapsed_ms",
        "route_status_categories",
    } for record in records[:-1])
    assert all(record["elapsed_ms"] >= 0 for record in records[:-1])
    assert [record["input_bytes"] for record in records[:-1]] == observed_input_bytes
    assert records[0]["route_status_categories"] == ["http_5xx", "other", "accepted"]
    final = records[-1]
    assert set(final) == {
        "record_type", "summary_characters", "chapter_count", "terminology_count",
        "covered_segment_count", "input_segment_count", "covered_asset_page_count",
        "input_asset_page_count",
    }
    assert final["record_type"] == "final"
    assert final["summary_characters"] == len("概念关系" * 200 + "MODEL_PROSE_SENTINEL")
    assert final["chapter_count"] > 1
    assert final["terminology_count"] == 0
    assert final["covered_segment_count"] == len(segments)
    assert final["input_segment_count"] == len(segments)
    assert final["covered_asset_page_count"] == 0
    assert final["input_asset_page_count"] == 0
    assert captured.err == ""
    for sentinel in (
        "SOURCE_ID_SENTINEL",
        "SOURCE_TEXT_SENTINEL",
        "MODEL_PROSE_SENTINEL",
        "CREDENTIAL_SENTINEL",
        "SECOND_CREDENTIAL_SENTINEL",
        "ROUTE_SECRET_SENTINEL",
    ):
        assert sentinel not in captured.out


def test_diagnostic_requires_live_opt_in_before_reading_input(
    monkeypatch: Any,
    capsys: Any,
) -> None:
    monkeypatch.delenv("AIALRA_RUN_LIVE_PROVIDER_TEST", raising=False)

    class UnreadableInput:
        def read(self) -> str:
            raise AssertionError("stdin must not be read without opt-in")

    monkeypatch.setattr("sys.stdin", UnreadableInput())
    assert diagnose_cloud_course.main() == 2
    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == ""
