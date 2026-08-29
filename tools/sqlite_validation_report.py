"""Print aggregate soak-test evidence without exposing transcript or project content."""

import argparse
import json
import sqlite3
from datetime import datetime


def percentile(values: list[float], quantile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, int(len(ordered) * quantile + 0.999999) - 1))
    return round(ordered[index], 2)


def milliseconds(start: str, end: str) -> float:
    return (datetime.fromisoformat(end) - datetime.fromisoformat(start)).total_seconds() * 1000


parser = argparse.ArgumentParser()
parser.add_argument("database")
parser.add_argument("owner_subject")
parser.add_argument("--enforce-long-run", action="store_true")
args = parser.parse_args()

connection = sqlite3.connect(f"file:{args.database}?mode=ro", uri=True)
connection.row_factory = sqlite3.Row
session = connection.execute(
    """
    SELECT s.id, s.state
    FROM projects p
    JOIN project_sessions ps ON ps.project_id = p.id
    JOIN sessions s ON s.id = ps.session_id
    WHERE p.owner_subject = ?
    ORDER BY s.created_at DESC
    LIMIT 1
    """,
    (args.owner_subject,),
).fetchone()
if session is None:
    raise SystemExit("no session found for validation owner")

session_id = session["id"]
audio_sources = connection.execute(
    """
    SELECT source_id, COUNT(*) AS stored, COUNT(DISTINCT sequence) AS unique_chunks,
           MIN(sequence) AS first_sequence, MAX(sequence) AS last_sequence
    FROM audio_chunks WHERE session_id = ?
    GROUP BY source_id
    """,
    (session_id,),
).fetchall()
stored_chunks = sum(row["stored"] for row in audio_sources)
unique_chunks = sum(row["unique_chunks"] for row in audio_sources)
expected_chunks = sum(row["last_sequence"] for row in audio_sources)

events = connection.execute(
    """
    SELECT event_type, payload_json
    FROM events WHERE session_id = ?
    ORDER BY ingested_at, event_id
    """,
    (session_id,),
).fetchall()
payloads = [(row["event_type"], json.loads(row["payload_json"])) for row in events]

def stage_latency(event_type: str) -> list[float]:
    return [
        float(payload["elapsed_ms"])
        for current_type, payload in payloads
        if current_type == event_type and isinstance(payload.get("elapsed_ms"), (int, float))
    ]


segment_ids = [
    payload.get("segment_id")
    for event_type, payload in payloads
    if event_type == "segment.finalized"
]
providers: dict[str, list[str]] = {"asr": [], "translation": [], "explanation": []}
for event_type, payload in payloads:
    if event_type == "segment.finalized" and isinstance(payload.get("provider"), str):
        providers["asr"].append(payload["provider"])
    elif event_type == "translation.finalized" and isinstance(payload.get("provider"), str):
        providers["translation"].append(payload["provider"])
    elif event_type == "explanation.card.created":
        result = payload.get("result")
        if isinstance(result, dict) and isinstance(result.get("provider"), str):
            providers["explanation"].append(result["provider"])
invalid_providers = [
    provider
    for stage, values in providers.items()
    for provider in values
    if (
        stage == "asr"
        and (
            not provider.startswith("faster-whisper:")
            or not provider.endswith(("@cpu", "@cuda"))
        )
    )
    or (
        stage != "asr"
        and (not provider.startswith("ollama:") or not provider.endswith("@cuda"))
    )
]
try:
    pickup_rows = connection.execute(
        """
        SELECT j.created_at, m.first_leased_at
        FROM model_jobs j
        JOIN model_job_metrics m ON m.job_id = j.id
        WHERE j.session_id = ?
        """,
        (session_id,),
    ).fetchall()
except sqlite3.OperationalError:
    pickup_rows = []
pickup_ms = [milliseconds(row["created_at"], row["first_leased_at"]) for row in pickup_rows]
failed_jobs = connection.execute(
    "SELECT last_error_kind FROM model_jobs WHERE session_id = ? AND status = 'failed'",
    (session_id,),
).fetchall()
completed_jobs = connection.execute(
    """
    SELECT job_type, created_at, completed_at
    FROM model_jobs
    WHERE session_id = ? AND completed_at IS NOT NULL
    """,
    (session_id,),
).fetchall()
job_status_rows = connection.execute(
    "SELECT status, COUNT(1) AS count FROM model_jobs WHERE session_id = ? GROUP BY status",
    (session_id,),
).fetchall()


def end_to_end_latency(job_type: str) -> list[float]:
    return [
        milliseconds(row["created_at"], row["completed_at"])
        for row in completed_jobs
        if row["job_type"] == job_type
    ]


def recent_end_to_end_latency(job_type: str, limit: int = 20) -> list[float]:
    recent = sorted(
        (row for row in completed_jobs if row["job_type"] == job_type),
        key=lambda row: row["completed_at"],
        reverse=True,
    )[:limit]
    return [milliseconds(row["created_at"], row["completed_at"]) for row in recent]

report = {
    "state": session["state"],
    "audio": {
        "sources": len(audio_sources),
        "stored_chunks": stored_chunks,
        "duplicate_chunks": stored_chunks - unique_chunks,
        "sequence_gaps": expected_chunks - unique_chunks,
    },
    "results": {
        "stable_segments": len(segment_ids),
        "duplicate_segments": len(segment_ids) - len(set(segment_ids)),
        "translations": sum(
            1 for event_type, _ in payloads if event_type == "translation.finalized"
        ),
        "explanations": sum(
            1 for event_type, _ in payloads if event_type == "explanation.card.created"
        ),
        "providers": {stage: sorted(set(values)) for stage, values in providers.items()},
        "invalid_provider_results": len(invalid_providers),
    },
    "latency_p95_ms": {
        "worker_pickup": percentile(pickup_ms, 0.95),
        "asr_provider": percentile(stage_latency("segment.finalized"), 0.95),
        "translation_provider": percentile(stage_latency("translation.finalized"), 0.95),
        "explanation_provider": percentile(stage_latency("explanation.card.created"), 0.95),
        "asr_window_to_result": percentile(end_to_end_latency("asr"), 0.95),
        "translation_trigger_to_result": percentile(end_to_end_latency("translate"), 0.95),
        "explanation_trigger_to_result": percentile(end_to_end_latency("explain"), 0.95),
    },
    "recent_20_latency_p95_ms": {
        "asr_provider": percentile(stage_latency("segment.finalized")[-20:], 0.95),
        "translation_provider": percentile(stage_latency("translation.finalized")[-20:], 0.95),
        "explanation_provider": percentile(stage_latency("explanation.card.created")[-20:], 0.95),
        "asr_window_to_result": percentile(recent_end_to_end_latency("asr"), 0.95),
        "translation_trigger_to_result": percentile(recent_end_to_end_latency("translate"), 0.95),
        "explanation_trigger_to_result": percentile(recent_end_to_end_latency("explain"), 0.95),
    },
    "model_jobs": {row["status"]: row["count"] for row in job_status_rows},
    "failed_jobs": len(failed_jobs),
    "gpu_oom_events": sum("oom" in str(row["last_error_kind"]).lower() for row in failed_jobs),
}
failures: list[str] = []
if args.enforce_long_run:
    if report["state"] != "completed":
        failures.append("session did not complete")
    if report["audio"]["duplicate_chunks"] != 0 or report["audio"]["sequence_gaps"] != 0:
        failures.append("audio sequence integrity failed")
    if report["results"]["duplicate_segments"] != 0:
        failures.append("stable transcript duplication failed")
    if report["results"]["invalid_provider_results"] != 0:
        failures.append("real local provider gate failed")
    if report["failed_jobs"] != 0 or report["gpu_oom_events"] != 0:
        failures.append("model job failure gate failed")
    thresholds = {
        "worker_pickup": 2_000,
        "asr_window_to_result": 3_000,
        "translation_trigger_to_result": 8_000,
        "explanation_trigger_to_result": 20_000,
    }
    for metric, threshold in thresholds.items():
        value = report["latency_p95_ms"][metric]
        if value is None or value > threshold:
            failures.append(f"{metric} p95 exceeded {threshold} ms")
report["status"] = "PASS" if not failures else "FAIL"
report["gate_failures"] = failures
print(json.dumps(report, indent=2, ensure_ascii=False))
if failures:
    raise SystemExit(1)
