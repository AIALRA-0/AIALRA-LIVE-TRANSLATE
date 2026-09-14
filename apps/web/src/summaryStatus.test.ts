import { describe, expect, it } from "vitest";
import { courseSummaryStatus } from "./summaryStatus";
import type { EventEnvelope } from "./types";

const event = (id: string, kind: string, payload: Record<string, unknown> = {}): EventEnvelope => ({
  schema_version: "1", event_id: id, session_id: "synthetic_session", source_id: "test",
  sequence: 0, event_type: kind, captured_at_monotonic_ns: 0,
  captured_at_wall: "2026-01-01T00:00:00Z", ingested_at: "2026-01-01T00:00:00Z",
  correlation_id: id, causation_id: null, payload, content_hash: id,
});

describe("course summary status", () => {
  it("clears an old failure as soon as its retry is queued", () => {
    const history = [
      event("run", "session.recording.started"),
      event("failed", "session.summary.failed", { recording_run: "run", error_kind: "model_http_error" }),
    ];
    expect(courseSummaryStatus(history).phase).toBe("failed");
    expect(courseSummaryStatus([...history, event("queued", "session.summary.queued", { recording_run: "run" })]).phase).toBe("queued");
    expect(courseSummaryStatus([...history, event("queued", "session.summary.queued", { recording_run: "run" }),
      event("created", "session.summary.created", { recording_run: "run", summary_id: "summary" })])).toMatchObject({ phase: "completed", summaryId: "summary" });
  });

  it("does not show an earlier recording run's summary during a resumed run", () => {
    expect(courseSummaryStatus([
      event("first", "session.recording.started"),
      event("old", "session.summary.created", { recording_run: "first", summary_id: "old-summary" }),
      event("second", "session.recording.started", { resumed: true }),
      event("done", "session.completed", { summary_pending: true }),
    ])).toMatchObject({ phase: "queued", summaryId: null });
  });
});
