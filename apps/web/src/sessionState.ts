import type { ProjectUpdate, Session } from "./types";

const TERMINAL_OR_DRAINING = new Set(["stopping", "processing", "completed", "failed"]);

const SESSION_STATE_BY_EVENT: Record<string, Session["state"]> = {
  "session.ready": "ready",
  "session.recording.started": "recording",
  "session.stopping": "stopping",
  "session.processing": "processing",
  "session.completed": "completed",
  "session.failed": "failed",
};

const SESSION_STATE_RANK: Record<string, number> = {
  ready: 0,
  recording: 1,
  degraded: 1,
  stopping: 2,
  processing: 3,
  completed: 4,
  failed: 4,
  archived: 5,
};

// Project SSE replay can arrive after the session event stream, so an old lease event cannot regress a final state.
export function applyLeaseAcquired(session: Session): Session {
  return TERMINAL_OR_DRAINING.has(session.state) ? session : { ...session, state: "recording" };
}

// A fresh EventSource replays durable history, so state events may arrive after a newer session snapshot.
export function applySessionStateEvent(session: Session, eventType: string): Session {
  const nextState = SESSION_STATE_BY_EVENT[eventType];
  if (!nextState) return session;
  const currentRank = SESSION_STATE_RANK[session.state] ?? 0;
  const nextRank = SESSION_STATE_RANK[nextState] ?? 0;
  return nextRank > currentRank ? { ...session, state: nextState } : session;
}

const READWEAVE_CONTENT_EVENTS = new Set([
  "segment.finalized",
  "paragraph.finalized",
  "translation.finalized",
  "explanation.card.created",
  "asset.page.extracted",
  "session.processing",
  "session.completed",
  "session.failed",
]);

export function shouldRefreshReadWeave(update: ProjectUpdate): boolean {
  if (update.update_type.startsWith("readweave.")) return true;
  return update.update_type === "session.event"
    && typeof update.payload.event_type === "string"
    && READWEAVE_CONTENT_EVENTS.has(update.payload.event_type);
}

const SESSION_STATE_EVENTS = new Set([
  "session.created",
  "session.ready",
  "session.recording.started",
  "session.stopping",
  "session.processing",
  "session.completed",
  "session.failed",
]);

export function shouldRefreshProjectSessions(update: ProjectUpdate): boolean {
  if (update.update_type.startsWith("recording.lease.")) return true;
  if (update.update_type === "project.created" || update.update_type === "project.updated") return true;
  return update.update_type === "session.event"
    && typeof update.payload.event_type === "string"
    && SESSION_STATE_EVENTS.has(update.payload.event_type);
}
