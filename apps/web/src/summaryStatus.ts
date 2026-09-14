import type { EventEnvelope } from "./types";

export type CourseSummaryStatus = {
  phase: "idle" | "queued" | "failed" | "completed";
  eventId: string | null;
  summaryId: string | null;
  errorKind: string | null;
};

export function courseSummaryStatus(events: EventEnvelope[]): CourseSummaryStatus {
  const lastStartIndex = events.reduce((index, event, current) =>
    event.event_type === "session.recording.started" ? current : index, -1);
  const lastStart = events[lastStartIndex];
  const currentRun = events.slice(Math.max(0, lastStartIndex));
  for (const event of [...currentRun].reverse()) {
    if (!["session.summary.created", "session.summary.failed", "session.summary.queued"].includes(event.event_type)) continue;
    const run = event.payload.recording_run;
    if (typeof run === "string" ? run !== (lastStart?.event_id ?? "legacy") : lastStart?.payload.resumed === true) continue;
    return {
      phase: event.event_type === "session.summary.created" ? "completed"
        : event.event_type === "session.summary.failed" ? "failed" : "queued",
      eventId: event.event_id,
      summaryId: event.event_type === "session.summary.created" && typeof event.payload.summary_id === "string"
        ? event.payload.summary_id : null,
      errorKind: event.event_type === "session.summary.failed" && typeof event.payload.error_kind === "string"
        ? event.payload.error_kind : null,
    };
  }
  const completed = [...currentRun].reverse().find((event) => event.event_type === "session.completed");
  return {
    phase: completed?.payload.summary_pending === true ? "queued" : "idle",
    eventId: completed?.event_id ?? null,
    summaryId: null,
    errorKind: null,
  };
}
