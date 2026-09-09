import type { CaptureMode } from "./audio";
import type { RecordingLease } from "./types";

// Keep credentials in the same tab-scoped storage as the existing lease, never
// localStorage. A refresh must not turn an explicit stop into permission to record.
export interface RecordingStopIntent {
  lease: RecordingLease;
  mode: CaptureMode;
  audioAcknowledged: boolean;
}

function key(projectId: string, sessionId: string): string {
  return `aialra-recording-stop:${projectId}:${sessionId}`;
}

export function readStopIntent(projectId: string, sessionId: string): RecordingStopIntent | null {
  try {
    const intent = JSON.parse(sessionStorage.getItem(key(projectId, sessionId)) ?? "null") as RecordingStopIntent | null;
    return intent?.lease?.project_id === projectId && intent.lease.session_id === sessionId
      && typeof intent.lease.lease_token === "string" && intent.lease.lease_token.length > 0
      && Number.isInteger(intent.lease.generation)
      && (intent.mode === "microphone" || intent.mode === "screen")
      && typeof intent.audioAcknowledged === "boolean" ? intent : null;
  } catch { return null; }
}

export function saveStopIntent(intent: RecordingStopIntent): void {
  sessionStorage.setItem(key(intent.lease.project_id, intent.lease.session_id), JSON.stringify(intent));
}

export function clearStopIntent(projectId: string, sessionId: string): void {
  sessionStorage.removeItem(key(projectId, sessionId));
}
