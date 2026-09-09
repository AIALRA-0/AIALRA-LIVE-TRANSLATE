import { describe, expect, it } from "vitest";
import { canDropWorkspaceTarget, formatAudioInputLabel, isFolderDescendant, isRecordingResumable, recordingDisplayState, resumeSessionLabel, workspaceTargetKey } from "./uiState";

describe("workspace drag targets", () => {
  const parents = { root_folder: null, child_folder: "root_folder" };

  it("rejects self and descendant folder drops while allowing a safe parent move", () => {
    expect(isFolderDescendant("child_folder", "root_folder", parents)).toBe(true);
    expect(canDropWorkspaceTarget({ entityType: "folder", entityId: "root_folder" }, { entityType: "folder", entityId: "root_folder", intent: "inside" }, parents)).toBe(false);
    expect(canDropWorkspaceTarget({ entityType: "folder", entityId: "root_folder" }, { entityType: "folder", entityId: "child_folder", intent: "inside" }, parents)).toBe(false);
    expect(canDropWorkspaceTarget({ entityType: "folder", entityId: "child_folder" }, { entityType: "root", intent: "root" }, parents)).toBe(true);
    expect(canDropWorkspaceTarget({ entityType: "project", entityId: "project_a" }, { entityType: "folder", entityId: "root_folder", intent: "inside" }, parents)).toBe(true);
  });

  it("only allows same-project session reordering", () => {
    expect(canDropWorkspaceTarget({ entityType: "session", entityId: "one", projectId: "project_a" }, { entityType: "session", entityId: "two", projectId: "project_a", intent: "before" })).toBe(true);
    expect(canDropWorkspaceTarget({ entityType: "session", entityId: "one", projectId: "project_a" }, { entityType: "session", entityId: "two", projectId: "project_b", intent: "after" })).toBe(false);
    expect(workspaceTargetKey({ entityType: "root", intent: "root" })).toBe("root:root");
  });
});

describe("recording continuity and input labels", () => {
  it("never treats durable recording history as proof of live microphone activity", () => {
    expect(recordingDisplayState("recording", false)).toBe("recording_interrupted");
    expect(recordingDisplayState("degraded", false)).toBe("recording_interrupted");
    expect(recordingDisplayState("recording")).toBe("recording_checking");
    expect(recordingDisplayState("recording", true)).toBe("recording");
    expect(recordingDisplayState("completed", true)).toBe("completed");
    expect(recordingDisplayState("processing", false)).toBe("processing");
  });
  it("allows further recording after completion without reopening trash or a sealing tail", () => {
    expect(isRecordingResumable("recording")).toBe(true);
    expect(isRecordingResumable("ready")).toBe(true);
    expect(isRecordingResumable("processing")).toBe(false);
    expect(isRecordingResumable("completed")).toBe(true);
    expect(isRecordingResumable("failed")).toBe(true);
    expect(isRecordingResumable("archived")).toBe(false);
    expect(resumeSessionLabel("completed")).toBe("续录本次课程");
    expect(resumeSessionLabel("recording")).toBe("继续本次收音");
  });

  it("uses the browser-provided concrete microphone name and a safe fallback", () => {
    expect(formatAudioInputLabel({ deviceId: "device", label: "MacBook Pro 麦克风" })).toBe("MacBook Pro 麦克风");
    expect(formatAudioInputLabel({ deviceId: "abcdef123456", label: "" })).toBe("麦克风（浏览器未提供设备名称）");
  });
});
