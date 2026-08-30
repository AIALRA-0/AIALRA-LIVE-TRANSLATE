import { describe, expect, it } from "vitest";
import { applyLeaseAcquired, applySessionStateEvent, shouldRefreshProjectSessions, shouldRefreshReadWeave } from "./sessionState";
import type { Session } from "./types";

const session = (state: Session["state"]): Session => ({
  id: "session_synthetic",
  title: "Synthetic course",
  state,
  source_language: "en",
  target_language: "zh-CN",
  privacy_mode: "local_only",
  consent_confirmed: true,
  demo_mode: false,
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
});

describe("project lease replay", () => {
  it("moves a ready observer into recording", () => {
    expect(applyLeaseAcquired(session("ready")).state).toBe("recording");
  });

  it("does not regress a completed session", () => {
    expect(applyLeaseAcquired(session("completed")).state).toBe("completed");
  });
});

describe("session event replay", () => {
  it("does not regress a completed snapshot while old events replay", () => {
    expect(applySessionStateEvent(session("completed"), "session.recording.started").state).toBe("completed");
    expect(applySessionStateEvent(session("completed"), "session.processing").state).toBe("completed");
  });

  it("advances a live session through the durable state machine", () => {
    expect(applySessionStateEvent(session("ready"), "session.recording.started").state).toBe("recording");
    expect(applySessionStateEvent(session("recording"), "session.processing").state).toBe("processing");
    expect(applySessionStateEvent(session("processing"), "session.completed").state).toBe("completed");
  });
});

describe("ReadWeave project update filtering", () => {
  const update = (eventType: string) => ({
    cursor: 1,
    project_id: "project_test",
    session_id: "session_test",
    update_type: "session.event",
    payload: { event_type: eventType },
    created_at: "2026-08-28T00:00:00Z",
  });

  it("does not refetch note previews for every audio event", () => {
    expect(shouldRefreshReadWeave(update("audio.chunk.received"))).toBe(false);
    expect(shouldRefreshProjectSessions(update("audio.chunk.received"))).toBe(false);
  });

  it("refreshes after stable text and connector status changes", () => {
    expect(shouldRefreshReadWeave(update("segment.finalized"))).toBe(true);
    expect(shouldRefreshReadWeave({ ...update("ignored"), update_type: "readweave.synced" })).toBe(true);
    expect(shouldRefreshProjectSessions(update("session.completed"))).toBe(true);
  });
});
