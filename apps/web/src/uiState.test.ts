import { describe, expect, it } from "vitest";
import { canDropWorkspaceTarget, formatAudioInputLabel, isFolderDescendant, isRecordingResumable, resumeSessionLabel, workspaceTargetKey } from "./uiState";

describe("workspace drag targets", () => {
  const parents = { root_folder: null, child_folder: "root_folder" };

  it("rejects self and descendant folder drops while allowing a safe parent move", () => {
    expect(isFolderDescendant("child_folder", "root_folder", parents)).toBe(true);
    expect(canDropWorkspaceTarget({ entityType: "folder", entityId: "root_folder" }, { entityType: "folder", entityId: "root_folder" }, parents)).toBe(false);
    expect(canDropWorkspaceTarget({ entityType: "folder", entityId: "root_folder" }, { entityType: "folder", entityId: "child_folder" }, parents)).toBe(false);
    expect(canDropWorkspaceTarget({ entityType: "folder", entityId: "child_folder" }, { entityType: "root" }, parents)).toBe(true);
  });

  it("only allows same-project session reordering", () => {
    expect(canDropWorkspaceTarget({ entityType: "session", entityId: "one", projectId: "project_a" }, { entityType: "session", entityId: "two", projectId: "project_a" })).toBe(true);
    expect(canDropWorkspaceTarget({ entityType: "session", entityId: "one", projectId: "project_a" }, { entityType: "session", entityId: "two", projectId: "project_b" })).toBe(false);
    expect(workspaceTargetKey({ entityType: "root" })).toBe("root");
  });
});

describe("recording continuity and input labels", () => {
  it("marks only an unfinished session as resumable", () => {
    expect(isRecordingResumable("recording")).toBe(true);
    expect(isRecordingResumable("ready")).toBe(true);
    expect(isRecordingResumable("processing")).toBe(false);
    expect(resumeSessionLabel("recording")).toBe("继续本次收音");
  });

  it("uses the browser-provided concrete microphone name and a safe fallback", () => {
    expect(formatAudioInputLabel({ deviceId: "device", label: "MacBook Pro 麦克风" })).toBe("MacBook Pro 麦克风");
    expect(formatAudioInputLabel({ deviceId: "abcdef123456", label: "" })).toBe("麦克风（设备 abcdef12）");
  });
});
