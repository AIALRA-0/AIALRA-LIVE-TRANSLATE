import type { Session } from "./types";

export type WorkspaceEntityType = "folder" | "project" | "session";

export interface WorkspaceDragTarget {
  entityType: WorkspaceEntityType;
  entityId: string;
  projectId?: string;
}

export type WorkspaceDropTarget = WorkspaceDragTarget | { entityType: "root" };

export function workspaceTargetKey(target: WorkspaceDropTarget): string {
  return target.entityType === "root" ? "root" : `${target.entityType}:${target.entityId}`;
}

export function isFolderDescendant(
  folderId: string,
  ancestorId: string,
  parentByFolderId: Readonly<Record<string, string | null>>,
): boolean {
  let current = parentByFolderId[folderId] ?? null;
  const visited = new Set<string>();
  while (current && !visited.has(current)) {
    if (current === ancestorId) return true;
    visited.add(current);
    current = parentByFolderId[current] ?? null;
  }
  return false;
}

export function canDropWorkspaceTarget(
  source: WorkspaceDragTarget,
  target: WorkspaceDropTarget,
  parentByFolderId: Readonly<Record<string, string | null>> = {},
): boolean {
  if (target.entityType === "root") return source.entityType === "folder" || source.entityType === "project";
  if (source.entityType === "folder" && target.entityType === "folder") {
    return source.entityId !== target.entityId && !isFolderDescendant(target.entityId, source.entityId, parentByFolderId);
  }
  if (source.entityType === "project" && target.entityType === "project") return source.entityId !== target.entityId;
  if (source.entityType === "session" && target.entityType === "session") {
    return source.entityId !== target.entityId && source.projectId === target.projectId;
  }
  return (source.entityType === "folder" || source.entityType === "project") && target.entityType === "folder";
}

export function formatAudioInputLabel(device: Pick<MediaDeviceInfo, "deviceId" | "label">): string {
  const label = device.label.trim();
  if (label) return label;
  return "麦克风（浏览器未提供设备名称）";
}

export function isRecordingResumable(state: Session["state"] | string): boolean {
  return state === "ready" || state === "recording" || state === "degraded";
}

export function resumeSessionLabel(state: Session["state"] | string): string {
  if (state === "recording" || state === "degraded") return "继续本次收音";
  if (state === "ready") return "进入本次录音台";
  return "查看本次课程";
}

export function formatLocalTimestamp(value: string | null | undefined): string {
  if (!value) return "时间未知";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "时间未知";
  return date.toLocaleString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}
