// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";
import { clearStopIntent, readStopIntent, saveStopIntent, type RecordingStopIntent } from "./recordingStop";

const intent: RecordingStopIntent = {
  lease: { project_id: "project-test", session_id: "session-test", holder_device_id: "test-tab", generation: 3, expires_at: "2000-01-01T00:00:00Z", lease_token: "synthetic-not-a-credential" },
  mode: "screen", audioAcknowledged: false,
};

describe("explicit recording stop intent", () => {
  beforeEach(() => { sessionStorage.clear(); localStorage.clear(); });
  it("survives lease expiry and a reread without granting permission to capture", () => {
    saveStopIntent(intent);
    expect(readStopIntent("project-test", "session-test")).toEqual(intent);
    expect(localStorage.length).toBe(0);
  });
  it("isolates courses and preserves the original source generation and mode", () => {
    saveStopIntent(intent);
    expect(readStopIntent("project-other", "session-test")).toBeNull();
    expect(readStopIntent("project-test", "session-other")).toBeNull();
    expect(readStopIntent("project-test", "session-test")?.mode).toBe("screen");
  });
  it("keeps acknowledged audio distinct from an acknowledged server stop", () => {
    saveStopIntent({ ...intent, audioAcknowledged: true });
    expect(readStopIntent("project-test", "session-test")?.audioAcknowledged).toBe(true);
    clearStopIntent("project-test", "session-test");
    expect(readStopIntent("project-test", "session-test")).toBeNull();
  });
  it("rejects damaged or mismatched stored state", () => {
    sessionStorage.setItem("aialra-recording-stop:project-test:session-test", "{");
    expect(readStopIntent("project-test", "session-test")).toBeNull();
    sessionStorage.setItem("aialra-recording-stop:project-test:session-test", JSON.stringify({ ...intent, lease: { ...intent.lease, session_id: "wrong" } }));
    expect(readStopIntent("project-test", "session-test")).toBeNull();
  });
});
