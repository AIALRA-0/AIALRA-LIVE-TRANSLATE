import { FormEvent, useCallback, useEffect, useReducer, useRef, useState } from "react";
import { api, subscribeEvents, subscribeProject, subscribeWorkspace, type RuntimeHealth } from "./api";
import { BrowserCapture, listAudioInputs, testMicrophone, type CaptureMode, type CapturePhase, type MicrophoneTestProgress, type MicrophoneTestResult } from "./audio";
import { applySessionStateEvent } from "./sessionState";
import { appendEvent } from "./timeline";
import type { EventEnvelope, LanguageView, Project, ReadWeavePreview, ReadWeaveStatus, RecordingLease, RecordingProjectStatus, Session, TimelineItem, WorkspaceFolder, WorkspaceSnapshot, WorkspaceTrashItem } from "./types";
import { canDropWorkspaceTarget, formatAudioInputLabel, formatLocalTimestamp, isFolderDescendant, isRecordingResumable, resumeSessionLabel, type WorkspaceDragTarget, type WorkspaceDropTarget } from "./uiState";

const LEASE_STORAGE_KEY = "aialra-active-recording-lease";
const TIMELINE_PAGE_SIZE = 160;
const SOURCE_LANGUAGE_OPTIONS = [
  ["auto", "自动识别"], ["zh", "中文"], ["en", "英文"], ["ja", "日文"],
  ["ko", "韩文"], ["es", "西班牙文"], ["fr", "法文"], ["de", "德文"],
] as const;
const TARGET_LANGUAGE_OPTIONS = [
  ["zh-CN", "简体中文"], ["en", "英文"], ["ja", "日文"], ["ko", "韩文"],
  ["es", "西班牙文"], ["fr", "法文"], ["de", "德文"],
] as const;

function languageLabel(value: string): string {
  return [...SOURCE_LANGUAGE_OPTIONS, ...TARGET_LANGUAGE_OPTIONS].find(([code]) => code === value)?.[1] ?? value;
}

function audioInputStorageKey(projectId: string): string {
  return `aialra-selected-audio-input:${projectId}`;
}

interface TimelineState { events: EventEnvelope[]; items: TimelineItem[] }
type TimelineAction = { type: "append"; event: EventEnvelope } | { type: "reset" };

function timelineReducer(state: TimelineState, action: TimelineAction): TimelineState {
  return action.type === "reset" ? { events: [], items: [] } : appendEvent(state, action.event);
}

function recorderDeviceId(): string {
  const key = "aialra-recorder-tab-id";
  const existing = window.sessionStorage.getItem(key);
  if (existing) return existing;
  const legacy = window.sessionStorage.getItem(LEASE_STORAGE_KEY)
    ? window.localStorage.getItem("aialra-recorder-device-id")
    : null;
  const created = legacy ?? `browser-${crypto.randomUUID()}`;
  window.sessionStorage.setItem(key, created);
  return created;
}

function workspaceDeviceId(): string {
  const key = "aialra-workspace-device-id";
  const existing = window.localStorage.getItem(key);
  if (existing) return existing;
  const legacy = window.localStorage.getItem("aialra-recorder-device-id");
  const created = legacy ?? `browser-${crypto.randomUUID()}`;
  window.localStorage.setItem(key, created);
  return created;
}

function saveLocalLease(lease: RecordingLease | null): void {
  if (lease) sessionStorage.setItem(LEASE_STORAGE_KEY, JSON.stringify(lease));
  else sessionStorage.removeItem(LEASE_STORAGE_KEY);
}

function restoredLocalLease(projectId: string, sessionId: string): RecordingLease | null {
  try {
    const lease = JSON.parse(sessionStorage.getItem(LEASE_STORAGE_KEY) ?? "null") as RecordingLease | null;
    return lease?.project_id === projectId && lease.session_id === sessionId ? lease : null;
  } catch {
    sessionStorage.removeItem(LEASE_STORAGE_KEY);
    return null;
  }
}

function currentLocalLease(): RecordingLease | null {
  try {
    const lease = JSON.parse(sessionStorage.getItem(LEASE_STORAGE_KEY) ?? "null") as RecordingLease | null;
    if (lease && new Date(lease.expires_at).getTime() <= Date.now()) {
      sessionStorage.removeItem(LEASE_STORAGE_KEY);
      return null;
    }
    return lease;
  } catch {
    sessionStorage.removeItem(LEASE_STORAGE_KEY);
    return null;
  }
}

function stateLabel(state: string): string {
  return ({ ready: "已就绪", recording: "录音中", degraded: "降级录音中", stopping: "正在停止", processing: "模型处理中", completed: "已完成", failed: "失败", archived: "已归档" } as Record<string, string>)[state] ?? state;
}

function stateTone(state: string): "green" | "yellow" | "red" | "gray" {
  if (state === "recording" || state === "degraded" || state === "failed") return "red";
  if (state === "stopping" || state === "processing") return "yellow";
  if (state === "ready" || state === "completed") return "green";
  return "gray";
}

const SYSTEM_NOTE_LABELS: Record<string, string> = {
  overview: "课程概览",
  transcript: "逐段转写与翻译",
  explanations: "补充讲解与术语",
  assets: "课件与证据",
  "user-notes": "我的笔记",
};

function capturePhaseLabel(phase: CapturePhase, sessionState: string, hasLease: boolean, mode: CaptureMode = "microphone"): string {
  const inputName = mode === "screen" ? "共享音频" : "麦克风";
  if (phase === "requesting-permission") return `正在申请${inputName}权限`;
  if (phase === "acquiring-lease") return "正在获取录音权限";
  if (phase === "connecting") return "正在连接服务器";
  if (phase === "recording") return "正在收音";
  if (phase === "blocked") return "项目录音已被其他设备占用";
  if (phase === "recoverable") return "本次课程可恢复";
  if (phase === "stopping") return "正在停止并保存尾音";
  if (phase === "processing") return "模型处理中";
  if (phase === "error") return "录音需要处理";
  if (sessionState === "recording" && !hasLease) return "需要重新连接本次收音";
  if (sessionState === "processing") return "模型处理中";
  if (sessionState === "completed") return "已完成";
  if (sessionState === "failed") return "处理失败";
  if (sessionState === "ready") return "可以开始录音";
  return "尚未录音";
}

function capturePhaseTone(phase: CapturePhase, sessionState: string, hasLease: boolean): "green" | "yellow" | "red" | "gray" {
  if (phase === "recording" || phase === "error" || (sessionState === "recording" && !hasLease && !["recoverable", "processing"].includes(phase))) return "red";
  if (phase === "blocked") return "yellow";
  if (phase === "recoverable") return "green";
  if (["requesting-permission", "acquiring-lease", "connecting", "stopping", "processing"].includes(phase) || sessionState === "processing") return "yellow";
  if (sessionState === "ready" || sessionState === "completed") return "green";
  if (sessionState === "failed") return "red";
  return "gray";
}

function captureActionLabel(phase: CapturePhase, hasLease: boolean, mode: CaptureMode = "microphone"): string {
  const inputName = mode === "screen" ? "共享音频" : "麦克风";
  if (phase === "requesting-permission") return `正在申请${inputName}权限`;
  if (phase === "acquiring-lease") return "正在获取录音权限";
  if (phase === "connecting") return "正在连接服务器";
  if (phase === "stopping") return "正在保存尾音";
  if (phase === "processing") return "模型处理中";
  if (phase === "blocked") return "重新检查录音状态";
  if (phase === "recoverable") return "确认后继续本次课程";
  return hasLease ? "继续连接收音" : "开始录音";
}

function routeSelection(): { projectId: string | null; sessionId: string | null; section: string | null } {
  const match = window.location.pathname.match(/^\/app\/projects\/([^/]+)(?:\/sessions\/([^/]+))?(?:\/notes\/([^/]+))?\/?$/);
  return { projectId: match?.[1] ?? null, sessionId: match?.[2] ?? null, section: match?.[3] ?? null };
}

function routePath(projectId: string | null, sessionId: string | null, section?: string | null): string {
  return projectId
    ? `/app/projects/${projectId}${sessionId ? `/sessions/${sessionId}${section ? `/notes/${section}` : ""}` : ""}`
    : "/app";
}

function navigate(projectId: string | null, sessionId: string | null, section?: string | null): void {
  const path = routePath(projectId, sessionId, section);
  window.history.pushState({}, "", path);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

function StatusBadge({ tone, children }: { tone: "green" | "yellow" | "red" | "gray"; children: React.ReactNode }) {
  return <span className={`status-badge ${tone}`}><i aria-hidden="true" />{children}</span>;
}

type ThemeMode = "light" | "dark";
type WorkspaceDialogState =
  | { action: "create-folder" | "create-project" }
  | { action: "rename-folder" | "move-folder"; folder: WorkspaceFolder }
  | { action: "rename-project" | "move-project" | "project-language"; project: Project }
  | { action: "rename-session"; project: Project; session: Session };
type WorkspaceTarget = WorkspaceDragTarget;
type WorkspaceContextTarget = WorkspaceTarget | { entityType: "root" };
type ContextMenuState =
  | { kind: "workspace"; x: number; y: number; target: WorkspaceContextTarget }
  | { kind: "trash"; x: number; y: number; item: WorkspaceTrashItem };

function WorkspaceSidebar({ snapshot, activeProjectId, activeSessionId, theme, onToggleTheme, onSelectProject, onSelectSession, onCreateFolder, onCreateProject, onUpdateFolder, onPlaceProject, onUpdateProject, onUpdateSession, onMoveWorkspace, onTrash, onRestoreTrash, onPurgeTrash, onOpenSettings }: {
  snapshot: WorkspaceSnapshot;
  activeProjectId: string | null;
  activeSessionId: string | null;
  theme: ThemeMode;
  onToggleTheme: () => void;
  onSelectProject: (project: Project) => void;
  onSelectSession: (project: Project, session: Session) => void;
  onCreateFolder: (title: string, parentId: string | null) => Promise<void>;
  onCreateProject: (title: string, folderId: string | null) => Promise<void>;
  onUpdateFolder: (folder: WorkspaceFolder, title: string, parentId: string | null, archived: boolean, sortOrder?: number) => Promise<void>;
  onPlaceProject: (project: Project, folderId: string | null, archived: boolean, sortOrder?: number) => Promise<void>;
  onUpdateProject: (project: Project, input: { title?: string; source_language?: string; target_language?: string }) => Promise<void>;
  onUpdateSession: (project: Project, session: Session, archived: boolean, sortOrder?: number, title?: string) => Promise<void>;
  onMoveWorkspace: (input: { entity_type: "folder" | "project" | "session"; entity_id: string; intent: "before" | "inside" | "after" | "root"; target_type?: "folder" | "project" | "session"; target_id?: string }) => Promise<void>;
  onTrash: (entityType: "folder" | "project" | "session", entityId: string) => Promise<void>;
  onRestoreTrash: (item: WorkspaceTrashItem) => Promise<void>;
  onPurgeTrash: (item: WorkspaceTrashItem) => Promise<void>;
  onOpenSettings: () => void;
}) {
  const [mobileOpen, setMobileOpen] = useState(false);
  const [selectedFolderId, setSelectedFolderId] = useState<string | null>(null);
  const [expandedFolderIds, setExpandedFolderIds] = useState<Set<string>>(
    () => new Set(snapshot.folders.filter((folder) => folder.parent_id === null).map((folder) => folder.id)),
  );
  const [dialog, setDialog] = useState<WorkspaceDialogState | null>(null);
  const [dialogTitle, setDialogTitle] = useState("");
  const [dialogParentId, setDialogParentId] = useState<string>("");
  const [dialogSourceLanguage, setDialogSourceLanguage] = useState("en");
  const [dialogTargetLanguage, setDialogTargetLanguage] = useState("zh-CN");
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const [dragging, setDragging] = useState<WorkspaceTarget | null>(null);
  const [dropTarget, setDropTarget] = useState<WorkspaceDropTarget | null>(null);
  const folderParents = Object.fromEntries(snapshot.folders.map((folder) => [folder.id, folder.parent_id]));
  const placements = new Map(snapshot.project_placements.map((item) => [item.project_id, item]));
  const sessionMetadata = new Map(snapshot.session_metadata.map((item) => [item.session_id, item]));
  const trashItems = snapshot.trash ?? [];
  const projectSessions = (projectId: string) => snapshot.sessions
    .filter((session) => snapshot.session_projects[session.id] === projectId && !sessionMetadata.get(session.id)?.archived_at)
    .sort((left, right) => {
      const leftMeta = sessionMetadata.get(left.id);
      const rightMeta = sessionMetadata.get(right.id);
      if (Boolean(leftMeta?.pinned) !== Boolean(rightMeta?.pinned)) return leftMeta?.pinned ? -1 : 1;
      return (leftMeta?.sort_order ?? 0) - (rightMeta?.sort_order ?? 0)
        || right.created_at.localeCompare(left.created_at);
    });
  const projectsInFolder = (folderId: string | null) => snapshot.projects.filter((project) => {
    const placement = placements.get(project.id);
    return !placement?.archived_at && (placement?.folder_id ?? null) === folderId;
  }).sort((left, right) => {
    const leftPlacement = placements.get(left.id);
    const rightPlacement = placements.get(right.id);
    return (leftPlacement?.sort_order ?? 0) - (rightPlacement?.sort_order ?? 0)
      || left.title.localeCompare(right.title, "zh-CN")
      || left.id.localeCompare(right.id);
  });

  useEffect(() => {
    const close = () => setContextMenu(null);
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    window.addEventListener("click", close);
    window.addEventListener("keydown", closeOnEscape);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", closeOnEscape);
    };
  }, []);

  function targetTitle(target: WorkspaceTarget): string {
    if (target.entityType === "folder") return snapshot.folders.find((folder) => folder.id === target.entityId)?.title ?? "文件夹";
    if (target.entityType === "project") return snapshot.projects.find((project) => project.id === target.entityId)?.title ?? "项目";
    return snapshot.sessions.find((session) => session.id === target.entityId)?.title ?? "课程会话";
  }

  function dropTargetTitle(target: WorkspaceDropTarget): string {
    if (target.entityType === "root") return "工作区根目录";
    const title = targetTitle(target);
    if (target.intent === "before") return `“${title}”前面`;
    if (target.intent === "after") return `“${title}”后面`;
    return `文件夹“${title}”里面`;
  }

  function currentDropIntent(target: WorkspaceDragTarget | { entityType: "root" }): WorkspaceDropTarget["intent"] | null {
    if (!dropTarget || dropTarget.entityType !== target.entityType) return null;
    if (dropTarget.entityType === "root" && target.entityType === "root") return "root";
    if (dropTarget.entityType !== "root" && target.entityType !== "root" && dropTarget.entityId === target.entityId) return dropTarget.intent;
    return null;
  }

  function dropHint(intent: WorkspaceDropTarget["intent"]): string {
    if (intent === "before") return "放到前面";
    if (intent === "after") return "放到后面";
    if (intent === "inside") return "放入文件夹";
    return "放到根目录";
  }

  function trashTitle(item: WorkspaceTrashItem): string {
    return targetTitle({ entityType: item.entity_type, entityId: item.entity_id, projectId: item.original_project_id ?? undefined });
  }

  function trashBlockReason(target: WorkspaceTarget): string | null {
    const blockedStates = new Set(["recording", "degraded", "stopping", "processing"]);
    let sessionIds: string[] = [];
    if (target.entityType === "session") {
      sessionIds = [target.entityId];
    } else if (target.entityType === "project") {
      sessionIds = snapshot.sessions.filter((session) => snapshot.session_projects[session.id] === target.entityId).map((session) => session.id);
    } else {
      const folders = new Set<string>([target.entityId]);
      let changed = true;
      while (changed) {
        changed = false;
        snapshot.folders.forEach((folder) => {
          if (folder.parent_id && folders.has(folder.parent_id) && !folders.has(folder.id)) {
            folders.add(folder.id);
            changed = true;
          }
        });
      }
      const projectIds = [...placements.values()]
        .filter((placement) => placement.folder_id && folders.has(placement.folder_id))
        .map((placement) => placement.project_id);
      sessionIds = snapshot.sessions.filter((session) => projectIds.includes(snapshot.session_projects[session.id])).map((session) => session.id);
    }
    const blocked = snapshot.sessions.find((session) => sessionIds.includes(session.id) && blockedStates.has(session.state));
    if (!blocked) return null;
    return blocked.state === "processing" || blocked.state === "stopping"
      ? "请先停止录音并等待处理完成"
      : "请先停止当前录音";
  }

  function isNestedTrashItem(item: WorkspaceTrashItem): boolean {
    if (item.entity_type === "folder") return Boolean(item.original_parent_id && trashItems.some((parent) => parent.entity_type === "folder" && parent.entity_id === item.original_parent_id));
    if (item.entity_type === "project") return Boolean(item.original_parent_id && trashItems.some((parent) => parent.entity_type === "folder" && parent.entity_id === item.original_parent_id));
    return Boolean(item.original_project_id && trashItems.some((parent) => parent.entity_type === "project" && parent.entity_id === item.original_project_id));
  }

  function showContextMenu(event: React.MouseEvent, target: WorkspaceContextTarget): void {
    event.preventDefault();
    event.stopPropagation();
    setContextMenu({ kind: "workspace", x: event.clientX, y: event.clientY, target });
  }

  function showTrashMenu(event: React.MouseEvent, item: WorkspaceTrashItem): void {
    event.preventDefault();
    event.stopPropagation();
    setContextMenu({ kind: "trash", x: event.clientX, y: event.clientY, item });
  }

  function toggleFolder(folderId: string): void {
    setSelectedFolderId(folderId);
    setExpandedFolderIds((current) => {
      const next = new Set(current);
      if (next.has(folderId)) next.delete(folderId);
      else next.add(folderId);
      return next;
    });
  }

  function selectFolder(folderId: string): void {
    setSelectedFolderId(folderId);
  }

  function openTarget(target: WorkspaceTarget): void {
    setContextMenu(null);
    if (target.entityType === "folder") {
      selectFolder(target.entityId);
      setExpandedFolderIds((current) => new Set(current).add(target.entityId));
      return;
    }
    if (target.entityType === "project") {
      const project = snapshot.projects.find((item) => item.id === target.entityId);
      if (project) onSelectProject(project);
      return;
    }
    const session = snapshot.sessions.find((item) => item.id === target.entityId);
    const projectId = target.projectId ?? snapshot.session_projects[target.entityId];
    const project = snapshot.projects.find((item) => item.id === projectId);
    if (project && session) onSelectSession(project, session);
  }

  function openDialog(next: WorkspaceDialogState, parentOverride?: string | null): void {
    setContextMenu(null);
    setDialog(next);
    setDialogTitle(next.action === "rename-session" ? next.session.title : "folder" in next ? next.folder.title : "project" in next ? next.project.title : "");
    setDialogParentId(
      "folder" in next ? next.folder.parent_id ?? ""
        : "project" in next ? placements.get(next.project.id)?.folder_id ?? ""
          : parentOverride !== undefined ? parentOverride ?? "" : selectedFolderId ?? "",
    );
    if ("project" in next) {
      setDialogSourceLanguage(next.project.source_language);
      setDialogTargetLanguage(next.project.target_language);
    }
  }

  async function submitDialog(event: FormEvent): Promise<void> {
    event.preventDefault();
    if (!dialog) return;
    const title = dialogTitle.trim();
    if (dialog.action === "create-folder" && title) await onCreateFolder(title, dialogParentId || null);
    if (dialog.action === "create-project" && title) await onCreateProject(title, dialogParentId || null);
    if (dialog.action === "rename-folder" && title) await onUpdateFolder(dialog.folder, title, dialog.folder.parent_id, false);
    if (dialog.action === "move-folder") await onUpdateFolder(dialog.folder, dialog.folder.title, dialogParentId || null, false);
    if (dialog.action === "rename-project" && title) await onUpdateProject(dialog.project, { title });
    if (dialog.action === "project-language") await onUpdateProject(dialog.project, { source_language: dialogSourceLanguage, target_language: dialogTargetLanguage });
    if (dialog.action === "move-project") await onPlaceProject(dialog.project, dialogParentId || null, false);
    if (dialog.action === "rename-session" && title) await onUpdateSession(dialog.project, dialog.session, false, sessionMetadata.get(dialog.session.id)?.sort_order ?? 0, title);
    setDialog(null);
  }

  function moveTargetToTrash(target: WorkspaceTarget): void {
    setContextMenu(null);
    if (window.confirm(`将“${targetTitle(target)}”移入回收站？其中的课程内容仍可恢复。`)) void onTrash(target.entityType, target.entityId);
  }

  function restoreTrash(item: WorkspaceTrashItem): void {
    setContextMenu(null);
    void onRestoreTrash(item);
  }

  function purgeTrash(item: WorkspaceTrashItem): void {
    setContextMenu(null);
    if (window.confirm(`永久删除“${trashTitle(item)}”及其关联内容？此操作不可恢复。`)) void onPurgeTrash(item);
  }

  function parseDrag(event: React.DragEvent): WorkspaceTarget | null {
    if (dragging) return dragging;
    try {
      const candidate = JSON.parse(event.dataTransfer.getData("application/x-aialra-workspace")) as Partial<WorkspaceTarget>;
      if (!candidate || typeof candidate.entityId !== "string" || !["folder", "project", "session"].includes(candidate.entityType ?? "")) return null;
      return {
        entityType: candidate.entityType as WorkspaceTarget["entityType"],
        entityId: candidate.entityId,
        ...(typeof candidate.projectId === "string" ? { projectId: candidate.projectId } : {}),
      };
    } catch {
      return null;
    }
  }

  function beginDrag(event: React.DragEvent, target: WorkspaceTarget): void {
    setDragging(target);
    setDropTarget(null);
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("application/x-aialra-workspace", JSON.stringify(target));
  }

  function endDrag(): void {
    setDragging(null);
    setDropTarget(null);
  }

  function deriveDropTarget(event: React.DragEvent, target: WorkspaceDragTarget | { entityType: "root" }, explicitIntent?: WorkspaceDropTarget["intent"]): WorkspaceDropTarget {
    if (target.entityType === "root") return { entityType: "root", intent: "root" };
    if (explicitIntent && explicitIntent !== "root") return { ...target, intent: explicitIntent };
    const bounds = event.currentTarget.getBoundingClientRect();
    const ratio = bounds.height > 0 ? (event.clientY - bounds.top) / bounds.height : 0.5;
    const intent = target.entityType === "folder"
      ? ratio < 0.28 ? "before" : ratio > 0.72 ? "after" : "inside"
      : ratio < 0.5 ? "before" : "after";
    return { ...target, intent };
  }

  function allowDrop(event: React.DragEvent, baseTarget: WorkspaceDragTarget | { entityType: "root" }, explicitIntent?: WorkspaceDropTarget["intent"]): void {
    const source = parseDrag(event);
    const target = deriveDropTarget(event, baseTarget, explicitIntent);
    if (!source || !canDropWorkspaceTarget(source, target, folderParents)) {
      event.dataTransfer.dropEffect = "none";
      setDropTarget(null);
      return;
    }
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
    setDropTarget(target);
  }

  function dropWorkspace(event: React.DragEvent, baseTarget: WorkspaceDragTarget | { entityType: "root" }, explicitIntent?: WorkspaceDropTarget["intent"]): void {
    event.preventDefault();
    event.stopPropagation();
    const source = parseDrag(event);
    const target = dropTarget ?? deriveDropTarget(event, baseTarget, explicitIntent);
    endDrag();
    if (!source || !canDropWorkspaceTarget(source, target, folderParents)) return;
    void onMoveWorkspace({
      entity_type: source.entityType,
      entity_id: source.entityId,
      intent: target.intent,
      ...(target.entityType === "root" ? {} : { target_type: target.entityType, target_id: target.entityId }),
    });
  }

  function renderDropZone(target: WorkspaceDragTarget | { entityType: "root" }, intent: WorkspaceDropTarget["intent"]): React.ReactNode {
    if (!dragging) return null;
    const candidate = target.entityType === "root" ? { entityType: "root", intent: "root" as const } : { ...target, intent };
    const active = currentDropIntent(target) === candidate.intent;
    const label = candidate.entityType === "root" ? "放到工作区根目录" : dropHint(candidate.intent);
    return <div
      className={"workspace-drop-zone" + (active ? " active" : "") + (candidate.entityType === "root" ? " root-drop-zone" : "")}
      role="button"
      aria-label={label}
      onDragOver={(event) => { event.stopPropagation(); allowDrop(event, target, intent); }}
      onDrop={(event) => dropWorkspace(event, target, intent)}
    >
      <span>{label}</span>
    </div>;
  }

  function isDescendant(folderId: string, ancestorId: string): boolean {
    return isFolderDescendant(folderId, ancestorId, folderParents);
  }

  const dialogNeedsTitle = dialog?.action === "create-folder" || dialog?.action === "create-project" || dialog?.action === "rename-folder" || dialog?.action === "rename-project" || dialog?.action === "rename-session";
  const dialogNeedsParent = dialog?.action === "create-folder" || dialog?.action === "create-project" || dialog?.action === "move-folder" || dialog?.action === "move-project";
  const dialogTitleText = dialog ? ({
    "create-folder": "新建课程文件夹", "create-project": "新建课程项目", "rename-folder": "重命名文件夹", "rename-project": "重命名项目", "rename-session": "重命名课程会话",
    "move-folder": "移动文件夹", "move-project": "移动项目", "project-language": "课程语言默认值",
  } as Record<WorkspaceDialogState["action"], string>)[dialog.action] : "";

  const renderProject = (project: Project) => {
    const projectTarget = { entityType: "project", entityId: project.id } as const;
    const projectDropIntent = currentDropIntent(projectTarget);
    return (
    <li key={project.id} className={`tree-project ${dragging?.entityId === project.id ? "dragging" : ""}`}>
      <div
        className={`tree-item-row ${activeProjectId === project.id && !activeSessionId ? "selected" : ""} ${projectDropIntent ? `drop-target drop-${projectDropIntent}` : ""}`}
        onDragEnd={endDrag}
        onContextMenu={(event) => showContextMenu(event, { entityType: "project", entityId: project.id })}
      >
        <button className="tree-item-button" onClick={() => onSelectProject(project)}><span aria-hidden="true">▣</span><span>{project.title}</span></button>
        <button className="tree-drag-handle" draggable aria-label={"拖动项目 " + project.title} title="拖动项目" onClick={(event) => event.stopPropagation()} onDragStart={(event) => beginDrag(event, { entityType: "project", entityId: project.id })}>⠿</button>
        {renderDropZone(projectTarget, "before")}
        {renderDropZone(projectTarget, "after")}
        <button className="tree-context-hint" aria-label={`管理项目 ${project.title}`} onClick={(event) => showContextMenu(event, { entityType: "project", entityId: project.id })} onContextMenu={(event) => showContextMenu(event, { entityType: "project", entityId: project.id })}>⋯</button>
      </div>
      {activeProjectId === project.id && (
        <ul className="tree-sessions">
          {projectSessions(project.id).map((session) => {
            const sessionTarget = { entityType: "session", entityId: session.id, projectId: project.id } as const;
            const sessionDropIntent = currentDropIntent(sessionTarget);
            return <li key={session.id} className={dragging?.entityId === session.id ? "dragging" : ""}>
              <div
                className={`tree-item-row ${activeSessionId === session.id ? "selected" : ""} ${sessionDropIntent ? `drop-target drop-${sessionDropIntent}` : ""}`}
                onDragEnd={endDrag}
                onContextMenu={(event) => showContextMenu(event, { entityType: "session", entityId: session.id, projectId: project.id })}
              >
                <button className="tree-item-button" onClick={() => onSelectSession(project, session)}><span aria-hidden="true">◫</span><span>{session.title}</span><i className={`tiny-dot ${stateTone(session.state)}`} aria-label={stateLabel(session.state)} /></button>
                <button className="tree-drag-handle" draggable aria-label={"拖动课程 " + session.title} title="拖动课程" onClick={(event) => event.stopPropagation()} onDragStart={(event) => beginDrag(event, { entityType: "session", entityId: session.id, projectId: project.id })}>⠿</button>
                {renderDropZone(sessionTarget, "before")}
                {renderDropZone(sessionTarget, "after")}
                <button className="tree-context-hint" aria-label={`管理课程 ${session.title}`} onClick={(event) => showContextMenu(event, { entityType: "session", entityId: session.id, projectId: project.id })} onContextMenu={(event) => showContextMenu(event, { entityType: "session", entityId: session.id, projectId: project.id })}>⋯</button>
              </div>
              {activeSessionId === session.id && (
                <ul className="system-notes tree-note-category" aria-label="课程笔记分类">
                  {Object.entries(SYSTEM_NOTE_LABELS).map(([section, title]) => (
                    <li key={section}><button title={section === "user-notes" ? "只由你编辑，AIALRA 不会覆盖正文" : "AIALRA 自动整理并同步到 ReadWeave"} onClick={() => navigate(project.id, session.id, section)}>{title}</button></li>
                  ))}
                </ul>
              )}
            </li>;
          })}
        </ul>
      )}
    </li>
    );
  };

  const renderFolder = (folder: WorkspaceFolder, depth: number): React.ReactNode => {
    const folderTarget = { entityType: "folder", entityId: folder.id } as const;
    const folderDropIntent = currentDropIntent(folderTarget);
    return (
    <li key={folder.id} className={`tree-folder ${dragging?.entityId === folder.id ? "dragging" : ""}`} style={{ "--tree-depth": depth } as React.CSSProperties}>
      <div
        className={`folder-label ${selectedFolderId === folder.id ? "selected" : ""} ${folderDropIntent ? `drop-target drop-${folderDropIntent}` : ""}`}
        onDragEnd={endDrag}
        onContextMenu={(event) => showContextMenu(event, { entityType: "folder", entityId: folder.id })}
      >
        <button className="folder-disclosure" aria-label={`${expandedFolderIds.has(folder.id) ? "折叠" : "展开"}${folder.title}`} aria-expanded={expandedFolderIds.has(folder.id)} onClick={() => toggleFolder(folder.id)}><span aria-hidden="true">{expandedFolderIds.has(folder.id) ? "▾" : "▸"}</span></button>
        <button className="folder-name" onClick={() => selectFolder(folder.id)}>{folder.title}</button>
        <button className="tree-drag-handle" draggable aria-label={"拖动文件夹 " + folder.title} title="拖动文件夹" onClick={(event) => event.stopPropagation()} onDragStart={(event) => beginDrag(event, { entityType: "folder", entityId: folder.id })}>⠿</button>
        {renderDropZone(folderTarget, "before")}
        {renderDropZone(folderTarget, "inside")}
        {renderDropZone(folderTarget, "after")}
        <button className="tree-context-hint" aria-label={`管理文件夹 ${folder.title}`} onClick={(event) => showContextMenu(event, { entityType: "folder", entityId: folder.id })} onContextMenu={(event) => showContextMenu(event, { entityType: "folder", entityId: folder.id })}>⋯</button>
      </div>
      {expandedFolderIds.has(folder.id) && <ul>
        {snapshot.folders.filter((item) => !item.archived_at && item.parent_id === folder.id).map((child) => renderFolder(child, depth + 1))}
        {projectsInFolder(folder.id).map(renderProject)}
      </ul>}
    </li>
    );
  };

  const renderWorkspaceContextMenu = (menu: Extract<ContextMenuState, { kind: "workspace" }>) => {
    const target = menu.target;
    if (target.entityType === "root") {
      return <div className="workspace-context-menu" role="menu" style={{ left: menu.x, top: menu.y }} onClick={(event) => event.stopPropagation()}>
        <strong className="context-menu-heading">工作区根目录</strong>
        <button role="menuitem" onClick={() => openDialog({ action: "create-folder" }, null)}>新建文件夹</button>
        <button role="menuitem" onClick={() => openDialog({ action: "create-project" }, null)}>新建项目</button>
        <button role="menuitem" onClick={() => { setContextMenu(null); onOpenSettings(); }}>设置与运行状态</button>
      </div>;
    }
    const project = target.entityType === "project" ? snapshot.projects.find((item) => item.id === target.entityId) : null;
    const session = target.entityType === "session" ? snapshot.sessions.find((item) => item.id === target.entityId) : null;
    const sessionProject = session ? snapshot.projects.find((item) => item.id === (target.projectId ?? snapshot.session_projects[session.id])) : null;
    const folder = target.entityType === "folder" ? snapshot.folders.find((item) => item.id === target.entityId) : null;
    const blockedReason = trashBlockReason(target);
    return <div className="workspace-context-menu" role="menu" style={{ left: menu.x, top: menu.y }} onClick={(event) => event.stopPropagation()}>
      <button role="menuitem" onClick={() => openTarget(target)}>打开</button>
      {folder && <><button role="menuitem" onClick={() => { setSelectedFolderId(folder.id); openDialog({ action: "create-folder" }, folder.id); }}>在此新建子文件夹</button><button role="menuitem" onClick={() => { setSelectedFolderId(folder.id); openDialog({ action: "create-project" }, folder.id); }}>在此新建项目</button><button role="menuitem" onClick={() => openDialog({ action: "rename-folder", folder })}>重命名</button><button role="menuitem" onClick={() => openDialog({ action: "move-folder", folder })}>移动</button></>}
      {project && <><button role="menuitem" onClick={() => openDialog({ action: "rename-project", project })}>重命名</button><button role="menuitem" onClick={() => openDialog({ action: "project-language", project })}>语言默认值</button><button role="menuitem" onClick={() => openDialog({ action: "move-project", project })}>移动</button></>}
      {session && sessionProject && <button role="menuitem" onClick={() => openDialog({ action: "rename-session", project: sessionProject, session })}>重命名</button>}
      <button className="danger-menu-item" role="menuitem" disabled={Boolean(blockedReason)} title={blockedReason ?? "移入回收站"} onClick={() => moveTargetToTrash(target)}>移入回收站</button>
      {blockedReason && <span className="context-menu-reason" role="note">{blockedReason}</span>}
    </div>;
  };

  return (
    <aside className={`workspace-sidebar ${mobileOpen ? "mobile-open" : ""}`} aria-label="课程工作区">
      <div className="workspace-brand"><span>A</span><div><strong>AIALRA</strong><small>课程工作区</small></div><button className="theme-toggle" aria-label={`切换到${theme === "light" ? "黑色" : "白色"}模式`} onClick={onToggleTheme}>{theme === "light" ? "◐ 黑色" : "◑ 白色"}</button><button className="mobile-tree-toggle" aria-expanded={mobileOpen} onClick={() => setMobileOpen((current) => !current)}>{mobileOpen ? "关闭课程树" : "打开课程树"}</button></div>
      <nav className="workspace-tree">
        <div className="tree-heading"><span>我的课程</span><div className="tree-heading-actions"><button aria-label="打开设置和运行状态" onClick={onOpenSettings}>设置</button></div></div>
        {dragging && <div className="drag-status" role="status" aria-live="polite"><strong>正在移动：{targetTitle(dragging)}</strong><span>{dropTarget ? `松开放入“${dropTargetTitle(dropTarget)}”` : "将光标移到高亮位置，再松开鼠标"}</span></div>}
        <ul className={currentDropIntent({ entityType: "root" }) ? "workspace-root-drop drop-target drop-root" : "workspace-root-drop"} onContextMenu={(event) => showContextMenu(event, { entityType: "root" })}>
          {snapshot.folders.filter((folder) => !folder.archived_at && folder.parent_id === null).map((folder) => renderFolder(folder, 0))}
          {projectsInFolder(null).map(renderProject)}
          {dragging && dragging.entityType !== "session" && <li>{renderDropZone({ entityType: "root" }, "root")}</li>}
        </ul>
      </nav>
      <section className="workspace-trash" aria-label="回收站">
        <div className="trash-heading"><span>回收站</span><small>{trashItems.length ? `${trashItems.length} 项 · 右键管理` : "暂无内容"}</small></div>
        {trashItems.filter((item) => !isNestedTrashItem(item)).map((item) => <button key={`${item.entity_type}:${item.entity_id}`} className="trash-item" onClick={() => restoreTrash(item)} onContextMenu={(event) => showTrashMenu(event, item)}><span aria-hidden="true">⌫</span><span>{trashTitle(item)}</span><small>{item.entity_type === "folder" ? "文件夹" : item.entity_type === "project" ? "项目" : "课程"}</small></button>)}
      </section>
      {dialog && <div className="workspace-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setDialog(null); }}><dialog className="workspace-dialog" open aria-modal="true" aria-labelledby="workspace-dialog-title" onCancel={() => setDialog(null)}>
        <form method="dialog" onSubmit={(event) => void submitDialog(event)}>
          <header><div><p>工作区操作</p><h2 id="workspace-dialog-title">{dialogTitleText}</h2></div><button type="button" aria-label="关闭" onClick={() => setDialog(null)}>×</button></header>
          {dialogNeedsTitle && <label>名称<input autoFocus value={dialogTitle} placeholder="请输入清晰的名称" onChange={(event) => setDialogTitle(event.target.value)} required /></label>}
          {dialogNeedsParent && <label>位置<select value={dialogParentId} onChange={(event) => setDialogParentId(event.target.value)}><option value="">工作区根目录</option>{snapshot.folders.filter((item) => !item.archived_at && !("folder" in dialog && (item.id === dialog.folder.id || isDescendant(item.id, dialog.folder.id)))).map((item) => <option key={item.id} value={item.id}>{item.title}</option>)}</select></label>}
          {dialog.action === "project-language" && <div className="language-settings-grid"><label>默认讲授语言<select value={dialogSourceLanguage} onChange={(event) => setDialogSourceLanguage(event.target.value)}>{SOURCE_LANGUAGE_OPTIONS.map(([code, label]) => <option key={code} value={code}>{label}</option>)}</select></label><label>默认翻译语言<select value={dialogTargetLanguage} onChange={(event) => setDialogTargetLanguage(event.target.value)}>{TARGET_LANGUAGE_OPTIONS.map(([code, label]) => <option key={code} value={code}>{label}</option>)}</select></label><p>只影响以后新建的课程，已有课程保持原语言。</p></div>}
          <footer><button type="button" className="secondary-button" onClick={() => setDialog(null)}>取消</button><button type="submit" className="primary-button" disabled={Boolean(dialogNeedsTitle && !dialogTitle.trim())}>保存</button></footer>
        </form>
      </dialog></div>}
      {contextMenu?.kind === "workspace" && renderWorkspaceContextMenu(contextMenu)}
      {contextMenu?.kind === "trash" && <div className="workspace-context-menu" role="menu" style={{ left: contextMenu.x, top: contextMenu.y }} onClick={(event) => event.stopPropagation()}><button role="menuitem" onClick={() => restoreTrash(contextMenu.item)}>恢复</button><button className="danger-menu-item" role="menuitem" onClick={() => purgeTrash(contextMenu.item)}>永久删除</button></div>}
    </aside>
  );
}

function ProjectOverview({ project, sessions, onCreated }: { project: Project; sessions: Session[]; onCreated: (session: Session) => void }) {
  const [title, setTitle] = useState("今天的课程");
  const [consent, setConsent] = useState(false);
  const [sourceLanguage, setSourceLanguage] = useState(project.source_language);
  const [targetLanguage, setTargetLanguage] = useState(project.target_language);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const resumableSession = [...sessions]
    .filter((session) => isRecordingResumable(session.state))
    .sort((left, right) => right.updated_at.localeCompare(left.updated_at))[0];
  async function create(event: FormEvent): Promise<void> {
    event.preventDefault(); setBusy(true); setError("");
    try { onCreated(await api.createProjectSession(project.id, { title, consent_confirmed: consent, device_id: recorderDeviceId(), source_language: sourceLanguage, target_language: targetLanguage })); }
    catch (caught) { setError(caught instanceof Error ? caught.message : "创建课程会话失败"); }
    finally { setBusy(false); }
  }
  return (
    <main className="project-overview">
      <header><p className="eyebrow">课程项目</p><h1>{project.title}</h1><p>会话、材料、转写与 ReadWeave 笔记都保存在这个项目中</p></header>
      <div className="project-overview-grid">
        <section className="overview-card">
          <h2>最近课程</h2>
          {resumableSession && <div className="resume-session-card">
            <div><p>继续已有课程会话</p><strong>{resumableSession.title}</strong><small>最近活动：{formatLocalTimestamp(resumableSession.updated_at)} · 已确认历史会按时间戳继续保留</small></div>
            <button className="primary-button" onClick={() => navigate(project.id, resumableSession.id)}>{resumeSessionLabel(resumableSession.state)}</button>
          </div>}
          {sessions.length ? sessions.map((session) => <button className="recent-session-row" key={session.id} onClick={() => navigate(project.id, session.id)}><span><strong>{session.title}</strong><small>最近活动：{formatLocalTimestamp(session.updated_at)}</small></span><StatusBadge tone={stateTone(session.state)}>{stateLabel(session.state)}</StatusBadge></button>) : <p>还没有课程会话</p>}
        </section>
        <form className="overview-card new-session" onSubmit={create}>
          <h2>新建独立课程会话</h2>
          <label>课程名称<input value={title} placeholder="例如：机器学习导论" onChange={(event) => setTitle(event.target.value)} required /></label>
          <div className="language-pair language-pair-selectors">
            <label>讲授语言<select value={sourceLanguage} onChange={(event) => setSourceLanguage(event.target.value)}>{SOURCE_LANGUAGE_OPTIONS.map(([code, label]) => <option key={code} value={code}>{label}</option>)}</select></label>
            <span aria-hidden="true">→</span>
            <label>翻译语言<select value={targetLanguage} onChange={(event) => setTargetLanguage(event.target.value)}>{TARGET_LANGUAGE_OPTIONS.map(([code, label]) => <option key={code} value={code}>{label}</option>)}</select></label>
          </div>
          <p className="form-help">本节课程：{languageLabel(sourceLanguage)}讲授，翻译为{languageLabel(targetLanguage)}。录音开始后不可修改。</p>
          <label className="check-row"><input type="checkbox" checked={consent} onChange={(event) => setConsent(event.target.checked)} /><span>我已获得课程录音许可</span></label>
          {error && <p className="error-message" role="alert">{error}</p>}
          <button className="primary-button" disabled={busy || !consent} aria-describedby="create-session-help">{busy ? "正在创建课程会话" : "创建独立课程并进入录音台"}</button>
          <p id="create-session-help" className="form-help">重复进入已有课程请使用左侧历史或上方“继续本次收音”；新建按钮只用于另开一节独立课程。</p>
        </form>
      </div>
    </main>
  );
}

function DocumentItem({ item, languageView }: { item: TimelineItem; languageView: LanguageView }) {
  const time = new Date(item.occurredAt).toLocaleTimeString("zh-CN", { hour12: false });
  if (item.kind === "paragraph") {
    return (
      <article className="course-paragraph" data-testid="course-paragraph">
        <header><time>{time}</time><span>{item.sourceProvider || "等待 Provider"}</span></header>
        {languageView !== "translation" && <p className="source-text">{item.original}</p>}
        {languageView !== "source" && <p className="translation-text">{item.translation || "等待真实模型翻译"}</p>}
        {item.translationProvider && <small>{item.translationProvider}</small>}
      </article>
    );
  }
  return (
    <aside className={`insight-block ${item.kind}`} data-testid={`insight-${item.kind}`}>
      <header><strong>{item.title}</strong><time>{time}</time></header>
      {item.imageUrl && <img src={item.imageUrl} alt={item.title} />}
      {item.sections?.length ? <div className="insight-sections">{item.sections.map((section, index) => <section key={`${section.label}:${index}`} className={section.tone ?? "neutral"}><strong>{section.label}</strong><p>{section.text}</p></section>)}</div> : <p>{item.body || "正在解析内容"}</p>}
      {item.provider && <small>{item.provider}</small>}
      {item.evidenceIds.length > 0 && <footer>{item.evidenceIds.slice(0, 6).map((id) => <code key={id} title={id}>证据 · {id.slice(-6)}</code>)}</footer>}
    </aside>
  );
}

function GpuPanel({ runtime }: { runtime: RuntimeHealth | null }) {
  const metadata = runtime?.worker?.model_metadata ?? {};
  const gpu = metadata.gpu && typeof metadata.gpu === "object" ? metadata.gpu as Record<string, unknown> : {};
  const online = runtime?.worker?.online === true;
  const queued = runtime?.model_queue?.queued ?? 0;
  const tone = !online ? "red" : queued > 0 ? "yellow" : "green";
  const number = (key: string) => typeof gpu[key] === "number" ? gpu[key] as number : null;
  const utilization = number("utilization_percent");
  const memoryUsed = number("memory_used_mib");
  const memoryTotal = number("memory_total_mib");
  return (
    <section className="side-card gpu-panel">
      <div className="card-heading"><h3>本机 GPU</h3><StatusBadge tone={tone}>{online ? queued > 0 ? "处理中" : "可用" : "离线"}</StatusBadge></div>
      <strong>{String(gpu.name ?? metadata.gpu_family ?? "等待 GPU 遥测")}</strong>
      <div className="metric-grid">
        <span><b>{utilization === null ? "--" : `${utilization}%`}</b>占用</span>
        <span><b>{memoryUsed === null || memoryTotal === null ? "--" : `${Math.round(memoryUsed / 1024 * 10) / 10}/${Math.round(memoryTotal / 1024 * 10) / 10} GB`}</b>显存</span>
        <span><b>{number("power_w") === null ? "--" : `${number("power_w")} W`}</b>功耗</span>
        <span><b>{number("temperature_c") === null ? "--" : `${number("temperature_c")}°C`}</b>温度</span>
      </div>
      <p>{String(metadata.llm_provider ?? "模型等待连接")} · 队列 {queued}</p>
    </section>
  );
}

function RuntimeSettingsDialog({ runtime, readWeave, onClose }: { runtime: RuntimeHealth | null; readWeave: ReadWeaveStatus | null; onClose: () => void }) {
  const worker = runtime?.worker;
  const queue = runtime?.model_queue;
  return <div className="workspace-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}><dialog className="workspace-dialog runtime-settings-dialog" open aria-modal="true" aria-labelledby="runtime-settings-title" onCancel={onClose}>
    <header><div><p>只读状态</p><h2 id="runtime-settings-title">设置与运行状态</h2></div><button type="button" aria-label="关闭" onClick={onClose}>×</button></header>
    <div className="runtime-settings-body">
      <p className="dialog-description">这里仅显示当前连接和运行信息；API 令牌、设备凭证和其他秘密配置不会在网页中编辑或展示。</p>
      <section><h3>账号与服务</h3><dl className="runtime-settings-grid"><div><dt>当前登录</dt><dd>{runtime ? "已由服务端验证" : "正在读取"}</dd></div><div><dt>服务地址</dt><dd>{window.location.origin}</dd></div><div><dt>Core</dt><dd>{runtime ? `${runtime.status} · ${runtime.deployment_mode}` : "未读取"}</dd></div><div><dt>构建 SHA</dt><dd className="monospace">{runtime?.build_id ?? "未读取"}</dd></div></dl></section>
      <section><h3>本机模型</h3><dl className="runtime-settings-grid"><div><dt>GPU Worker</dt><dd>{worker ? `${worker.online ? "在线" : "离线"} · ${worker.capabilities.join("、") || "无能力声明"}` : "未连接"}</dd></div><div><dt>模型队列</dt><dd>{queue ? `排队 ${queue.queued} · 租约中 ${queue.leased} · 失败 ${queue.failed}` : "未读取"}</dd></div></dl></section>
      <section><h3>ReadWeave 同步</h3><dl className="runtime-settings-grid"><div><dt>连接目标</dt><dd>{readWeave?.connection?.public_url ?? (readWeave?.configured ? "已配置（地址受保护）" : "未配置")}</dd></div><div><dt>同步范围</dt><dd>{readWeave?.connection?.policy ?? "仅同步稳定内容"}</dd></div></dl></section>
    </div>
    <footer><button type="button" className="primary-button" onClick={onClose}>完成</button></footer>
  </dialog></div>;
}

function SessionConsole({ project, initial, languageView, onLanguageView }: { project: Project; initial: Session; languageView: LanguageView; onLanguageView: (view: LanguageView) => void }) {
  const [session, setSession] = useState(initial);
  const [timeline, dispatch] = useReducer(timelineReducer, { events: [], items: [] });
  const [streamConnected, setStreamConnected] = useState(false);
  const [lastActivityAt, setLastActivityAt] = useState(initial.updated_at);
  const [captureStatus, setCaptureStatus] = useState("尚未连接麦克风");
  const [capturePhase, setCapturePhase] = useState<CapturePhase>("idle");
  const [captureNotice, setCaptureNotice] = useState("");
  const [recordingStatus, setRecordingStatus] = useState<RecordingProjectStatus | null>(null);
  const [statusClock, setStatusClock] = useState(() => Date.now());
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [lease, setLease] = useState<RecordingLease | null>(null);
  const [captureActive, setCaptureActive] = useState(false);
  const [captureMode, setCaptureMode] = useState<CaptureMode>("microphone");
  const [audioInputs, setAudioInputs] = useState<MediaDeviceInfo[]>([]);
  const [selectedAudioInput, setSelectedAudioInput] = useState(() => window.localStorage.getItem(audioInputStorageKey(project.id)) ?? "");
  const [audioInputsReady, setAudioInputsReady] = useState(false);
  const [audioDeviceNotice, setAudioDeviceNotice] = useState("");
  const [micProgress, setMicProgress] = useState<MicrophoneTestProgress | null>(null);
  const [micResult, setMicResult] = useState<MicrophoneTestResult | null>(null);
  const [micTesting, setMicTesting] = useState(false);
  const [runtime, setRuntime] = useState<RuntimeHealth | null>(null);
  const [readWeave, setReadWeave] = useState<ReadWeaveStatus | null>(null);
  const [readWeavePreview, setReadWeavePreview] = useState<ReadWeavePreview | null>(null);
  const [readWeaveConfirmUrl, setReadWeaveConfirmUrl] = useState<string | null>(null);
  const [readWeaveReconciling, setReadWeaveReconciling] = useState(false);
  const [visibleItemLimit, setVisibleItemLimit] = useState(TIMELINE_PAGE_SIZE);
  const [pendingUpload, setPendingUpload] = useState<File | null>(null);
  const [uploadDropActive, setUploadDropActive] = useState(false);
  const capture = useRef<BrowserCapture | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);

  const refreshRecordingStatus = useCallback(async (): Promise<RecordingProjectStatus | null> => {
    try {
      const next = await api.recordingStatus(project.id, recorderDeviceId());
      setRecordingStatus(next);
      setStatusClock(new Date(next.server_time).getTime());
      if (next.lease?.holder === "other") {
        capture.current?.revoke();
        capture.current = null;
        setLease(null);
        saveLocalLease(null);
        setCaptureActive(false);
        setCapturePhase("blocked");
        setCaptureNotice("这个项目当前由其他设备录音；租约释放或到期后可以重新尝试。");
      } else if (!next.lease) {
        const currentStatus = next.sessions?.find((item) => item.session_id === initial.id);
        if (currentStatus?.recoverable) {
          setCapturePhase("recoverable");
          setCaptureNotice("本次课程没有活动租约或排队任务；确认后会接续已有历史，并按时间戳追加新的内容。");
        } else if (currentStatus?.reason === "processing") {
          setCapturePhase("processing");
          setCaptureNotice("本次课程仍有后台任务处理中；队列排空后会恢复“确认后继续本次课程”。");
        } else {
          setCapturePhase((current) => {
            if (!["blocked", "recoverable", "processing"].includes(current)) return current;
            setCaptureNotice("");
            return "idle";
          });
        }
      }
      return next;
    } catch {
      return null;
    }
  }, [project.id, initial.id]);

  const refreshAudioInputs = useCallback(async (requestPermission = false): Promise<MediaDeviceInfo[]> => {
    try {
      const devices = await listAudioInputs(requestPermission);
      const hasNamedInputs = devices.some((device) => device.label.trim().length > 0);
      setAudioInputs(devices);
      setAudioInputsReady(hasNamedInputs);
      // Before permission, browsers may expose only a generic `default`
      // device. Preserve a previously selected concrete device until the
      // permissioned enumeration can confirm whether it is still present.
      if (requestPermission || hasNamedInputs) {
        setSelectedAudioInput((current) => current && devices.some((device) => device.deviceId === current && device.deviceId !== "default" && device.deviceId !== "communications") ? current : "");
      }
      if (requestPermission) {
        setAudioDeviceNotice(hasNamedInputs
          ? "已取得麦克风权限，设备名称已刷新；切换选择会在下一次录音连接时生效"
          : "权限已取得，但浏览器没有返回设备名称，请检查系统麦克风权限后重试");
      }
      return devices;
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : "无法读取麦克风设备，请检查浏览器和系统权限";
      setAudioDeviceNotice(message);
      throw caught;
    }
  }, []);

  function reportCaptureStatus(message: string): void {
    setCaptureStatus(message);
    if (message.includes("已连接")) {
      setCapturePhase("recording");
      setCaptureNotice("");
    } else if (message.includes("连接失败") || message.includes("连接超时") || message.includes("网络不可用")) {
      setCapturePhase("error");
      setCaptureNotice(message);
    } else if (message.includes("连接恢复中")) {
      setCapturePhase("connecting");
      setCaptureNotice(message);
    }
  }

  useEffect(() => {
    return subscribeEvents(initial.id, (event) => {
      dispatch({ type: "append", event });
      setSession((current) => applySessionStateEvent(current, event.event_type));
      const eventTime = event.ingested_at || event.captured_at_wall;
      if (eventTime) setLastActivityAt((current) => new Date(eventTime).getTime() >= new Date(current).getTime() ? eventTime : current);
    }, setStreamConnected);
  }, [initial.id]);

  useEffect(() => subscribeProject(project.id, (update) => {
    if (update.session_id === initial.id && ["recording.lease.acquired", "recording.lease.renewed"].includes(update.update_type)) {
      setLastActivityAt((current) => new Date(update.created_at).getTime() >= new Date(current).getTime() ? update.created_at : current);
    }
    if (update.update_type === "recording.lease.acquired" && update.payload.holder_device_id !== recorderDeviceId()) {
      capture.current?.revoke(); capture.current = null; setLease(null); saveLocalLease(null); setCaptureActive(false); setCapturePhase("blocked"); setCaptureNotice("这个项目已由其他设备接管，本标签页已停止收音；租约释放后会自动恢复重试入口。");
    }
    if (update.update_type.startsWith("recording.lease.")) void refreshRecordingStatus();
    if (update.update_type.startsWith("readweave.")) void api.readWeaveStatus(project.id).then(setReadWeave).catch(() => setReadWeave(null));
  }, setStreamConnected), [project.id, initial.id, refreshRecordingStatus]);

  useEffect(() => {
    let active = true;
    const refresh = () => { if (active) void refreshRecordingStatus(); };
    refresh();
    const timer = window.setInterval(refresh, 10_000);
    window.addEventListener("focus", refresh);
    return () => { active = false; window.clearInterval(timer); window.removeEventListener("focus", refresh); };
  }, [refreshRecordingStatus]);

  useEffect(() => {
    if (recordingStatus?.lease?.holder !== "other") return;
    const timer = window.setInterval(() => setStatusClock((current) => current + 1_000), 1_000);
    return () => window.clearInterval(timer);
  }, [recordingStatus?.lease?.holder]);

  useEffect(() => {
    void api.readWeaveStatus(project.id).then(setReadWeave).catch(() => setReadWeave(null));
    void api.readWeavePreview(project.id).then(setReadWeavePreview).catch(() => setReadWeavePreview(null));
    const initialDeviceRefresh = window.setTimeout(() => void refreshAudioInputs(false).catch(() => undefined), 0);
    const onDeviceChange = () => void refreshAudioInputs(false).catch(() => undefined);
    navigator.mediaDevices?.addEventListener("devicechange", onDeviceChange);
    const restored = restoredLocalLease(project.id, initial.id);
    if (restored) void api.renewRecording(project.id, initial.id, recorderDeviceId(), restored.lease_token).then(() => {
      setLease(restored);
      setCapturePhase("connecting");
      setCaptureStatus("录音租约已恢复，可继续收音");
    }).catch(() => saveLocalLease(null));
    return () => {
      window.clearTimeout(initialDeviceRefresh);
      navigator.mediaDevices?.removeEventListener("devicechange", onDeviceChange);
    };
  }, [project.id, initial.id, refreshAudioInputs]);

  useEffect(() => {
    const key = audioInputStorageKey(project.id);
    if (selectedAudioInput) window.localStorage.setItem(key, selectedAudioInput);
    else window.localStorage.removeItem(key);
  }, [project.id, selectedAudioInput]);

  useEffect(() => {
    let active = true;
    const refresh = () => void api.health().then((value) => { if (active) setRuntime(value); }).catch(() => { if (active) setRuntime(null); });
    refresh(); const timer = window.setInterval(refresh, 5_000);
    return () => { active = false; window.clearInterval(timer); };
  }, []);

  useEffect(() => {
    const visibility = () => { if (document.hidden && captureActive) setCaptureNotice("页面已进入后台，iOS 浏览器可能暂停收音，请尽快返回前台"); };
    document.addEventListener("visibilitychange", visibility);
    return () => document.removeEventListener("visibilitychange", visibility);
  }, [captureActive]);

  useEffect(() => {
    if (!captureActive) return;
    const warnBeforeUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", warnBeforeUnload);
    return () => window.removeEventListener("beforeunload", warnBeforeUnload);
  }, [captureActive]);

  useEffect(() => () => capture.current?.dispose(), []);

  async function runMicTest(): Promise<void> {
    setMicTesting(true); setMicResult(null); setCaptureNotice("");
    try {
      const devices = await refreshAudioInputs(true);
      const requestedDeviceId = selectedAudioInput && devices.some((device) => device.deviceId === selectedAudioInput && device.deviceId !== "default" && device.deviceId !== "communications") ? selectedAudioInput : undefined;
      if (!requestedDeviceId && selectedAudioInput) setSelectedAudioInput("");
      setMicResult(await testMicrophone(requestedDeviceId, setMicProgress));
    }
    catch (caught) { setCaptureNotice(caught instanceof Error ? caught.message : "麦克风测试失败"); }
    finally { setMicTesting(false); setMicProgress(null); }
  }

  async function startCapture(acquired: RecordingLease, preparedCapture?: BrowserCapture): Promise<void> {
    const next = preparedCapture ?? capture.current ?? new BrowserCapture(
      project.id,
      session.id,
      recorderDeviceId(),
      (message) => reportCaptureStatus(message),
      captureMode,
      selectedAudioInput || undefined,
      (message) => {
        setCaptureActive(false);
        setCapturePhase("error");
        setCaptureNotice(message ?? "录音权限已由另一台设备接管，未确认音频仍保留在本机");
        void refreshRecordingStatus();
      },
    );
    capture.current = next;
    setCapturePhase("connecting");
    try {
      await next.activate(acquired.lease_token, acquired.generation);
      setCaptureActive(true);
    } catch (error) {
      next.dispose();
      if (capture.current === next) capture.current = null;
      setCaptureActive(false);
      setCapturePhase("error");
      setCaptureNotice(error instanceof Error ? error.message : "浏览器无法连接录音服务");
      throw error;
    }
  }

  async function continueCapture(): Promise<void> {
    if (!lease) return;
    setBusy(true);
    setCaptureNotice("");
    try { await startCapture(lease); }
    catch { /* startCapture has already placed the actionable error in the capture card */ }
    finally { setBusy(false); }
  }

  async function begin(): Promise<void> {
    setBusy(true);
    setNotice("");
    setCaptureNotice("");
    setCapturePhase("requesting-permission");
    let acquired: RecordingLease | null = null;
    let preparedCapture: BrowserCapture | null = null;
    try {
      const status = await refreshRecordingStatus();
      if (status?.lease?.holder === "other") {
        setCapturePhase("blocked");
        setCaptureNotice("这个项目当前由其他设备录音；租约释放或到期后可以重新尝试。");
        return;
      }
      const currentStatus = status?.sessions?.find((item) => item.session_id === initial.id);
      if (currentStatus?.reason === "processing") {
        setCapturePhase("processing");
        setCaptureNotice("本次课程仍有后台任务处理中，请等待队列排空后再继续收音。");
        return;
      }
      if (currentStatus?.recoverable && !window.confirm("本次课程已有历史内容。确认后将接续原课程，新的录音按时间戳追加，不会覆盖历史。")) {
        setCapturePhase("recoverable");
        setCaptureNotice("已保留本次课程历史；确认后才会重新获取录音租约。");
        return;
      }
      if (status && !status.lease && !status.admission.allowed && !["recording", "degraded"].includes(session.state)) {
        setCapturePhase("idle");
        setCaptureNotice("GPU 正在处理已有课程，新项目暂时不能开始录音；当前录音、停止和确认不会受影响。");
        return;
      }
      const availableInputs = captureMode === "microphone" ? await refreshAudioInputs(true) : [];
      const requestedDeviceId = selectedAudioInput && availableInputs.some((device) => device.deviceId === selectedAudioInput && device.deviceId !== "default" && device.deviceId !== "communications")
        ? selectedAudioInput
        : undefined;
      if (selectedAudioInput && !requestedDeviceId) setSelectedAudioInput("");
      // Request the browser input while the click still carries user intent.
      // The server lease is created only after this local step succeeds.
      preparedCapture = new BrowserCapture(
        project.id,
        session.id,
        recorderDeviceId(),
        (message) => reportCaptureStatus(message),
        captureMode,
        requestedDeviceId,
        (message) => {
          setCaptureActive(false);
          setCapturePhase("error");
          setCaptureNotice(message ?? "录音权限已由另一台设备接管，未确认音频仍保留在本机");
          void refreshRecordingStatus();
        },
      );
      capture.current = preparedCapture;
      await preparedCapture.prepare();
      if (captureMode === "microphone") await refreshAudioInputs(false).catch(() => undefined);
      setCapturePhase("acquiring-lease");
      acquired = await api.acquireRecording(project.id, session.id, recorderDeviceId());
      setLease(acquired);
      saveLocalLease(acquired);
      setLastActivityAt(new Date().toISOString());
      setSession((current) => ({ ...current, state: "recording" }));
      void refreshRecordingStatus();
      await startCapture(acquired, preparedCapture);
    } catch (caught) {
      const code = (caught as Error & { code?: string })?.code;
      if (!acquired) {
        preparedCapture?.dispose();
        if (capture.current === preparedCapture) capture.current = null;
        setLease(null);
        saveLocalLease(null);
        setCaptureActive(false);
        setCaptureNotice(caught instanceof Error ? caught.message : "浏览器录音启动失败，请检查麦克风和录音权限");
      } else {
        // Keep a valid lease after a transient connection failure so the user
        // can press “继续连接收音” instead of losing the whole session.
        setCaptureNotice((caught instanceof Error ? caught.message : "浏览器无法连接录音服务") + "；录音权限仍保留，可点击“继续连接收音”重试");
      }
      setCapturePhase(code === "recording_lease_conflict" ? "blocked" : code === "recording_session_processing" ? "processing" : "error");
      if (code === "recording_lease_conflict" || code === "recording_lease_expired") void refreshRecordingStatus();
      if (!acquired) setCaptureStatus("尚未连接麦克风");
    } finally { setBusy(false); }
  }

  async function stop(): Promise<void> {
    setBusy(true);
    setCaptureNotice("");
    setCapturePhase("stopping");
    try {
      try { await capture.current?.stop(); } catch (caught) {
        setCapturePhase("error");
        setCaptureNotice(caught instanceof Error ? caught.message : "浏览器音频尚未全部确认，请保持页面在线后重试");
        return;
      }
      capture.current = null; setCaptureActive(false);
      if (!lease) throw new Error("本机不是当前录音设备");
      const next = await api.stopRecording(project.id, session.id, recorderDeviceId(), lease.lease_token);
      setSession(next); setLease(null); saveLocalLease(null);
      setCapturePhase(next.state === "processing" ? "processing" : "idle");
      void refreshRecordingStatus();
    } catch (caught) {
      setCapturePhase("error");
      setCaptureNotice(caught instanceof Error ? caught.message : "停止录音失败");
      const code = (caught as Error & { code?: string })?.code;
      if (code === "recording_lease_conflict" || code === "recording_lease_expired") void refreshRecordingStatus();
    }
    finally { setBusy(false); }
  }

  function chooseUpload(file: File): void {
    if (file.size <= 0) {
      setNotice("这个文件为空，请选择有内容的材料");
      return;
    }
    if (file.size > 50 * 1024 * 1024) {
      setNotice("材料不能超过 50 MiB，请压缩后再上传");
      return;
    }
    setNotice("");
    setPendingUpload(file);
  }

  async function confirmUpload(): Promise<void> {
    const file = pendingUpload;
    if (!file) return;
    setBusy(true);
    setNotice(`正在保存已确认材料 ${file.name}`);
    try {
      const result = await api.uploadAsset(session.id, file, true);
      setNotice(result.explain_job_id
        ? "已确认上传；材料解析任务和等待讲解任务已排队，材料解析完成并出现稳定段落后会自动执行讲解。"
        : "已确认上传，材料解析任务已排队");
      setPendingUpload(null);
    } catch (caught) {
      setNotice(caught instanceof Error ? caught.message : "材料上传失败");
    } finally {
      setBusy(false);
      if (fileInput.current) fileInput.current.value = "";
    }
  }

  const isRecording = ["recording", "degraded"].includes(session.state);
  const visibleCaptureStatus = session.state === "processing"
    ? "录音已停止，真实模型仍在处理"
    : session.state === "completed" ? "录音和模型处理均已完成" : captureStatus;
  const captureTone = capturePhaseTone(capturePhase, session.state, Boolean(lease));
  const captureLabel = capturePhaseLabel(capturePhase, session.state, Boolean(lease), captureMode);
  const currentRecordingStatus = recordingStatus?.sessions?.find((item) => item.session_id === initial.id);
  const conflictingLeaseSeconds = recordingStatus?.lease?.holder === "other"
    ? Math.max(0, Math.ceil((new Date(recordingStatus.lease.expires_at).getTime() - statusClock) / 1_000))
    : null;
  const capacityBlocked = Boolean(recordingStatus && !recordingStatus.lease && !recordingStatus.admission.allowed && !["recording", "degraded"].includes(session.state));
  const recordingWaitsForQueue = Boolean(
    !lease
      && ["recording", "degraded"].includes(session.state)
      && currentRecordingStatus
      && !currentRecordingStatus.recoverable,
  );
  const defaultAudioInput = audioInputs.find((device) => device.deviceId === "default");
  const selectedAudioDevice = audioInputs.find((device) => device.deviceId === selectedAudioInput);
  const defaultAudioLabel = defaultAudioInput?.label.trim()
    ? `系统默认 · ${formatAudioInputLabel(defaultAudioInput)}`
    : "系统默认麦克风（允许权限后显示具体型号）";
  const selectedAudioLabel = selectedAudioDevice ? formatAudioInputLabel(selectedAudioDevice) : defaultAudioLabel;
  const sessionContinuity = session.state === "recording" || session.state === "degraded"
    ? "这是同一课程会话；刷新或再次进入会沿用已确认历史，本设备保留租约后可继续收音，新的片段会按时间戳追加。"
    : session.state === "ready"
      ? "这是同一课程会话；开始后再次进入会回到这里，不会覆盖已有历史。"
      : "录音已进入收尾或完成阶段；历史内容按时间戳保留，不能重新打开并覆盖本次会话。";
  const readWeaveTone = !readWeave?.configured ? "gray" : readWeave.conflicts > 0 ? "red" : readWeave.syncing > 0 || readWeave.queued > 0 ? "yellow" : "green";
  const latestSummaryEvent = [...timeline.events].reverse().find((event) => event.event_type === "session.summary.created" || event.event_type === "session.summary.failed");
  const summaryRetryable = latestSummaryEvent?.event_type === "session.summary.failed";
  const modelQueueDepth = runtime?.model_queue?.queued ?? 0;
  const modelStatusTone = summaryRetryable ? "red" : modelQueueDepth > 0 ? "yellow" : "green";
  const section = routeSelection().section;
  const readWeaveNodeType = section === "user-notes" ? "user_notes" : section;
  const readWeaveUrl = readWeave?.targets?.find((target) => target.local_id === `${session.id}:${section === "user-notes" ? "user" : section}` || (!section && target.node_type === "session" && target.local_id === session.id))?.note_url ?? readWeave?.note_url;
  const visibleItems = timeline.items.filter((item) => {
    if (!section || section === "transcript") return section ? item.kind === "paragraph" : true;
    if (section === "overview") return item.kind === "session-summary";
    if (section === "explanations") return item.kind === "insight";
    if (section === "assets") return item.kind === "asset";
    return true;
  });
  const renderedItems = visibleItems.slice(-visibleItemLimit);
  const hiddenItemCount = visibleItems.length - renderedItems.length;

  return (
    <div className="session-workspace">
      <header className="session-header">
        <div><p className="eyebrow">{project.title}</p><h1>{session.title}</h1><div className="session-meta"><span>建立：{formatLocalTimestamp(session.created_at)}</span><span>最近活动：{formatLocalTimestamp(lastActivityAt)}</span></div></div>
        <div className="header-status"><StatusBadge tone={streamConnected ? "green" : "yellow"}>{streamConnected ? "多端已同步" : "正在恢复同步"}</StatusBadge><StatusBadge tone={stateTone(session.state)}>{stateLabel(session.state)}</StatusBadge></div>
      </header>
      <main className="session-layout">
        <section className="document-panel">
          <div className="document-toolbar">
            <div>
              <span>课程文档</span>
              <small>{visibleItems.filter((item) => item.kind === "paragraph").length} 个稳定段落</small>
              <small className="document-explainer">自动整理的内容会同步到 ReadWeave；“我的笔记”不会被系统覆盖</small>
            </div>
            <div className="view-switch" role="group" aria-label="语言显示模式">{(["bilingual", "source", "translation"] as LanguageView[]).map((view) => <button key={view} aria-pressed={languageView === view} className={languageView === view ? "active" : ""} onClick={() => onLanguageView(view)}>{view === "bilingual" ? "双语" : view === "source" ? "原文" : "译文"}</button>)}</div>
          </div>
          <div className="course-document" aria-live="polite">
            {hiddenItemCount > 0 && (
              <button
                className="load-earlier-button"
                onClick={() => setVisibleItemLimit((current) => current + TIMELINE_PAGE_SIZE)}
              >
                加载更早内容 · 还有 {hiddenItemCount} 项
              </button>
            )}
            {renderedItems.length ? renderedItems.map((item) => <DocumentItem key={item.id} item={item} languageView={languageView} />) : <div className="document-empty"><div className="waveform">{[10, 25, 17, 38, 22, 31, 12].map((height, index) => <span key={index} style={{ height }} />)}</div><h2>课程内容会在这里连续展开</h2><p>字幕、译文、讲解和课件证据来自真实模型，不显示占位结果</p></div>}
          </div>
          <section
            className={"material-composer" + (uploadDropActive ? " drop-active" : "")}
            aria-label="讲解与材料"
            onDragEnter={(event) => { event.preventDefault(); setUploadDropActive(true); }}
            onDragOver={(event) => { event.preventDefault(); event.dataTransfer.dropEffect = "copy"; }}
            onDragLeave={(event) => { if (event.currentTarget === event.target || !event.currentTarget.contains(event.relatedTarget as Node)) setUploadDropActive(false); }}
            onDrop={(event) => { event.preventDefault(); setUploadDropActive(false); const file = event.dataTransfer.files[0]; if (file) chooseUpload(file); }}
          >
            <div className="material-composer-heading"><div><h3>讲解与材料</h3><p>上传后先保存材料；只有你确认后才会排队，并自动加入下一次讲解。</p></div><StatusBadge tone={modelStatusTone}>{summaryRetryable ? "总结可重试" : modelQueueDepth > 0 ? "队列处理中" : "可用"}</StatusBadge></div>
            <div className="material-drop-copy"><strong>拖动材料到这里</strong><span>或选择 PPT、PDF、图片、文档和文本文件</span></div>
            <input ref={fileInput} className="visually-hidden" type="file" accept=".pptx,.pdf,.docx,.png,.jpg,.jpeg,.webp,.txt,.md,.csv" onChange={(event) => { const file = event.target.files?.[0]; if (file) chooseUpload(file); }} />
            <button className="secondary-button" disabled={busy} onClick={() => fileInput.current?.click()}>选择材料</button>
            {summaryRetryable && <button className="secondary-button" disabled={busy || isRecording} onClick={() => void api.summarize(project.id, session.id).then(() => setNotice("课程总结已重新排队，完成后会在当前页面出现")).catch((caught) => setNotice(caught instanceof Error ? caught.message : "课程总结重试失败"))}>重试课程总结</button>}
            <p className="material-queue-help">确认窗口会列出文件名、类型、大小和目标课程；取消不会创建任何任务。确认后材料解析与等待讲解任务会立即进入队列，讲解会等待材料解析和稳定段落完成。</p>
            {pendingUpload && <div className="material-confirm" role="dialog" aria-modal="false" aria-label="确认上传材料">
              <div><strong>确认上传材料</strong><span>{pendingUpload.name}</span><small>{pendingUpload.type || "未知类型"} · {(pendingUpload.size / 1024 / 1024).toFixed(2)} MiB · 目标课程：{session.title}</small></div>
              <p>确认后将保存材料，并自动加入下一次讲解；不会覆盖已有字幕、译文或人工笔记。</p>
              <div className="material-confirm-actions"><button className="secondary-button" type="button" disabled={busy} onClick={() => { setPendingUpload(null); if (fileInput.current) fileInput.current.value = ""; }}>取消</button><button className="primary-button" type="button" disabled={busy} onClick={() => void confirmUpload()}>{busy ? "正在确认…" : "确认上传并排队"}</button></div>
            </div>}
          </section>
        </section>
        <aside className="session-sidebar">
           <section className="side-card capture-card">
             <div className="card-heading"><h3>录音</h3><StatusBadge tone={captureTone}>{captureLabel}</StatusBadge></div>
             <div className="session-continuity"><strong>{isRecordingResumable(session.state) ? "可继续本次课程" : "本次课程历史"}</strong><span>{sessionContinuity}</span></div>
             <label>音频来源<select value={captureMode} onChange={(event) => setCaptureMode(event.target.value as CaptureMode)} disabled={isRecording}><option value="microphone">麦克风</option><option value="screen">浏览器标签或共享音频</option></select></label>
             {captureMode === "microphone" && <>
               <label>输入设备<select value={selectedAudioInput} onChange={(event) => { setSelectedAudioInput(event.target.value); setMicResult(null); }} disabled={isRecording}><option value="">{defaultAudioLabel}</option>{audioInputs.filter((device) => device.deviceId !== "default" && device.deviceId !== "communications").map((device) => <option key={device.deviceId} value={device.deviceId}>{formatAudioInputLabel(device)}</option>)}</select></label>
               <div className="audio-device-row"><span><strong>当前设备</strong><small>{selectedAudioLabel}</small></span><button className="text-link-button" type="button" disabled={isRecording || micTesting} onClick={() => void refreshAudioInputs(true).catch(() => undefined)}>{audioInputsReady ? "刷新设备" : "允许权限并刷新设备"}</button></div>
               {audioDeviceNotice && <small className="audio-device-notice" role="status">{audioDeviceNotice}</small>}
             </>}
            <div className="mic-test">
              <div className="mic-meter"><span style={{ width: `${Math.max(2, Math.min(100, ((micProgress?.levelDbfs ?? -96) + 96) / 0.96))}%` }} /></div>
              <StatusBadge tone={micTesting ? "yellow" : micResult?.passed ? "green" : micResult ? "red" : "yellow"}>{micTesting ? micProgress?.phase === "quiet" ? "请保持安静" : "请朗读一句话" : micResult?.message ?? "麦克风尚未测试"}</StatusBadge>
              {micResult && <small>噪声 {micResult.noiseFloorDbfs.toFixed(1)} dBFS · 语音 {micResult.speechP95Dbfs.toFixed(1)} dBFS · 峰值 {micResult.peakDbfs.toFixed(1)} dBFS</small>}
              <button className="secondary-button" disabled={micTesting || isRecording || captureMode !== "microphone"} onClick={() => void runMicTest()}>{micTesting ? "测试中 4 秒" : "测试麦克风"}</button>
            </div>
            <p className="capture-help">音频在确认写入后才会从本机发送队列中移除；浏览器端不需要额外配对设备。</p>
            {conflictingLeaseSeconds !== null && <div className="recording-lease-status" role="status"><strong>其他设备正在录制本项目</strong><span>{recordingStatus?.lease?.session_title ? `课程“${recordingStatus.lease.session_title}”` : "当前课程会话"} · 租约约 {conflictingLeaseSeconds} 秒后到期</span></div>}
            {capacityBlocked && <div className="recording-capacity-status" role="status"><strong>暂不接纳新的项目录音</strong><span>原因：{recordingStatus?.admission.reason === "asr_backlog" ? "ASR 队列积压" : recordingStatus?.admission.reason === "asr_degraded" ? "ASR/CUDA 状态降级" : "ASR Worker 暂时离线"} · 约 {recordingStatus?.admission.retry_after_seconds ?? 5} 秒后自动复查</span></div>}
            {currentRecordingStatus?.recoverable && !lease && <div className="recording-recovery-status" role="status"><strong>发现可继续的历史收音</strong><span>服务端确认没有活动租约或排队任务；点击下方按钮并确认后，会在原课程中按时间戳追加。</span></div>}
            {recordingWaitsForQueue && <div className="recording-capacity-status" role="status"><strong>本次课程正在收尾</strong><span>还有 {currentRecordingStatus?.active_model_jobs ?? 0} 个后台任务；完成后会自动恢复继续入口。</span></div>}
            {captureNotice && <div className="capture-inline-alert" role="alert">{captureNotice}</div>}
            <p className="capture-copy" aria-live="polite"><strong>{captureLabel}</strong> · {visibleCaptureStatus}</p>
            {!isRecording ? (
              <button className="primary-button" disabled={busy || capacityBlocked || capturePhase === "processing" || session.state === "processing"} onClick={() => void begin()}>{captureActionLabel(capturePhase, Boolean(lease), captureMode)}</button>
            ) : lease && captureActive ? (
              <button className="stop-button" disabled={busy} onClick={() => void stop()}>停止并完成处理</button>
            ) : lease ? (
              <button className="primary-button" disabled={busy} onClick={() => void continueCapture()}>继续连接收音</button>
            ) : currentRecordingStatus?.recoverable ? (
              <button className="primary-button" disabled={busy} onClick={() => void begin()}>确认并继续本次课程</button>
            ) : (
              <button className="primary-button" disabled>等待后台处理完成</button>
            )}
          </section>
          <GpuPanel runtime={runtime} />
          <section className="side-card readweave-card">
            <div className="card-heading"><h3>ReadWeave</h3><StatusBadge tone={readWeaveTone}>{!readWeave?.configured ? "未配置" : readWeave.conflicts > 0 ? "存在冲突" : readWeave.syncing > 0 || readWeave.queued > 0 ? "同步中" : "已同步"}</StatusBadge></div>
            <p>{readWeavePreview?.sessions.find((item) => item.session_id === session.id)?.latest_entries[0]?.translation ?? "稳定字幕和讲解会自动进入对应笔记"}</p>
            <dl className="readweave-connection">
              <div><dt>连接目标</dt><dd>{readWeave?.connection?.public_url ?? (readWeave?.configured ? "已连接（地址受保护）" : "未配置")}</dd></div>
              <div><dt>同步范围</dt><dd>{readWeave?.connection?.policy ?? "仅同步稳定内容，不同步原始音频或秘密配置"}</dd></div>
              <div><dt>最近同步</dt><dd>{readWeave?.updated_at ? new Date(readWeave.updated_at).toLocaleString("zh-CN") : "尚无同步记录"}</dd></div>
            </dl>
            <button className="secondary-button" disabled={!readWeave?.configured || readWeaveReconciling || Boolean(readWeave?.queued || readWeave?.syncing)} onClick={() => {
              setReadWeaveReconciling(true);
              void api.reconcileReadWeave(project.id)
                .then(() => setNotice("ReadWeave 校对已排队，同一项目不会重复创建后台任务"))
                .catch((caught) => setNotice(caught instanceof Error ? caught.message : "ReadWeave 校对失败"))
                .finally(() => setReadWeaveReconciling(false));
            }}>{readWeaveReconciling ? "正在排队…" : "立即校对"}</button>
            {readWeaveUrl && <button className="text-link-button" onClick={() => setReadWeaveConfirmUrl(readWeaveUrl)}>打开{readWeaveNodeType ? "对应" : "项目"}笔记 →</button>}
            {readWeaveConfirmUrl && <div className="inline-confirm" role="dialog" aria-label="确认打开 ReadWeave"><p>即将打开 ReadWeave 对应目标：</p><code>{readWeaveConfirmUrl}</code><div><button className="secondary-button" onClick={() => setReadWeaveConfirmUrl(null)}>取消</button><button className="primary-button" onClick={() => { const url = readWeaveConfirmUrl; setReadWeaveConfirmUrl(null); window.location.assign(url); }}>确认打开</button></div></div>}
          </section>
          {notice && <div className="notice-box" role="status">{notice}</div>}
        </aside>
      </main>
    </div>
  );
}

export default function App() {
  const [deviceId] = useState(workspaceDeviceId);
  const [snapshot, setSnapshot] = useState<WorkspaceSnapshot | null>(null);
  const [route, setRoute] = useState(routeSelection);
  const [error, setError] = useState("");
  const [theme, setTheme] = useState<ThemeMode>(() => window.localStorage.getItem("aialra-theme") === "dark" ? "dark" : "light");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsRuntime, setSettingsRuntime] = useState<RuntimeHealth | null>(null);
  const [settingsReadWeave, setSettingsReadWeave] = useState<ReadWeaveStatus | null>(null);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.documentElement.style.colorScheme = theme;
    window.localStorage.setItem("aialra-theme", theme);
  }, [theme]);

  const refresh = useCallback(async (): Promise<WorkspaceSnapshot> => {
    const next = await api.workspace(deviceId); setSnapshot(next); return next;
  }, [deviceId]);

  useEffect(() => {
    if (window.location.pathname === "/" || !window.location.pathname.startsWith("/app")) window.history.replaceState({}, "", "/app");
    const updateRoute = () => setRoute(routeSelection());
    window.addEventListener("popstate", updateRoute);
    void api.workspace(deviceId).then((next) => {
      setSnapshot(next);
      const selected = routeSelection();
      if (!selected.projectId && next.preference?.active_project_id) navigate(next.preference.active_project_id, next.preference.active_session_id);
      else if (!selected.projectId && next.projects[0]) navigate(next.projects[0].id, null);
    }).catch((caught) => setError(caught instanceof Error ? caught.message : "工作区加载失败"));
    const unsubscribe = subscribeWorkspace(() => void refresh(), () => undefined);
    return () => { window.removeEventListener("popstate", updateRoute); unsubscribe(); };
  }, [deviceId, refresh]);

  const activeProject = snapshot?.projects.find((project) => {
    if (project.id !== route.projectId) return false;
    return !snapshot.project_placements.find((placement) => placement.project_id === project.id)?.archived_at;
  }) ?? null;
  const activeSession = snapshot?.sessions.find((session) => {
    if (session.id !== route.sessionId) return false;
    return !snapshot.session_metadata.find((metadata) => metadata.session_id === session.id)?.archived_at;
  }) ?? null;
  const languageView = snapshot?.preference?.language_view ?? "bilingual";
  const activeProjectIdForSettings = activeProject?.id ?? null;

  useEffect(() => {
    if (!settingsOpen) return;
    let active = true;
    void api.health().then((value) => { if (active) setSettingsRuntime(value); }).catch(() => { if (active) setSettingsRuntime(null); });
    if (activeProjectIdForSettings) void api.readWeaveStatus(activeProjectIdForSettings).then((value) => { if (active) setSettingsReadWeave(value); }).catch(() => { if (active) setSettingsReadWeave(null); });
    return () => { active = false; };
  }, [settingsOpen, activeProjectIdForSettings]);

  async function persistSelection(projectId: string | null, sessionId: string | null, view = languageView): Promise<void> {
    await api.updatePreference(deviceId, { active_project_id: projectId, active_session_id: sessionId, language_view: view, sidebar_collapsed: false });
    await refresh();
  }

  async function runWorkspaceAction(action: () => Promise<void>): Promise<void> {
    setError("");
    try {
      await action();
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : "工作区操作失败";
      setError(message);
    }
  }

  function selectWithoutAbandoningRecording(projectId: string, sessionId: string | null): void {
    const localLease = currentLocalLease();
    if (localLease && localLease.project_id !== projectId) {
      if (window.confirm("当前标签页正在录制另一个项目。为避免中断收音，将目标项目打开到新标签页。继续吗？")) {
        window.open(routePath(projectId, sessionId), "_blank", "noopener,noreferrer");
      }
      return;
    }
    navigate(projectId, sessionId);
    void persistSelection(projectId, sessionId);
  }

  if (!snapshot) return <main className="workspace-loading"><div className="brand-mark">A</div><p>{error || "正在加载课程工作区"}</p></main>;

  return (
    <div className="app-workspace">
      {error && <div className="workspace-error" role="alert"><span>{error}</span><button aria-label="关闭错误提示" onClick={() => setError("")}>×</button></div>}
      <WorkspaceSidebar
        snapshot={snapshot}
        activeProjectId={activeProject?.id ?? null}
        activeSessionId={activeSession?.id ?? null}
        theme={theme}
        onToggleTheme={() => setTheme((current) => current === "light" ? "dark" : "light")}
        onOpenSettings={() => { setSettingsRuntime(null); setSettingsReadWeave(null); setSettingsOpen(true); }}
        onSelectProject={(project) => selectWithoutAbandoningRecording(project.id, null)}
        onSelectSession={(project, session) => selectWithoutAbandoningRecording(project.id, session.id)}
        onCreateFolder={(title, parentId) => runWorkspaceAction(async () => { await api.createFolder({ title, parent_id: parentId }); await refresh(); })}
        onCreateProject={(title, folderId) => runWorkspaceAction(async () => { const project = await api.createProject(title); if (folderId) await api.placeProject(project.id, { folder_id: folderId, sort_order: 0, archived: false }); await refresh(); navigate(project.id, null); await persistSelection(project.id, null); })}
        onUpdateFolder={(folder, title, parentId, archived, sortOrder) => runWorkspaceAction(async () => { await api.updateFolder(folder.id, { title, parent_id: parentId, sort_order: sortOrder ?? folder.sort_order, archived }); await refresh(); })}
        onPlaceProject={(project, folderId, archived, sortOrder) => runWorkspaceAction(async () => { const placement = snapshot.project_placements.find((item) => item.project_id === project.id); await api.placeProject(project.id, { folder_id: folderId, sort_order: sortOrder ?? placement?.sort_order ?? 0, archived }); await refresh(); if (archived && activeProject?.id === project.id) navigate(null, null); })}
        onUpdateProject={(project, input) => runWorkspaceAction(async () => { await api.updateProject(project.id, input); await refresh(); })}
        onMoveWorkspace={(input) => runWorkspaceAction(async () => { await api.moveWorkspaceEntity(input); await refresh(); })}
        onUpdateSession={(project, session, archived, sortOrder, title) => runWorkspaceAction(async () => { const metadata = snapshot.session_metadata.find((item) => item.session_id === session.id); await api.updateSession(project.id, session.id, { title, pinned: metadata?.pinned ?? false, sort_order: sortOrder ?? metadata?.sort_order ?? 0, archived }); await refresh(); if (archived && activeSession?.id === session.id) navigate(project.id, null); })}
        onTrash={(entityType, entityId) => runWorkspaceAction(async () => { await api.trash(entityType, entityId); await refresh(); if (entityType === "folder" || (entityType === "project" && activeProject?.id === entityId) || (entityType === "session" && activeSession?.id === entityId)) navigate(null, null); })}
        onRestoreTrash={(item) => runWorkspaceAction(async () => { await api.restoreTrash(item.entity_type, item.entity_id); await refresh(); })}
        onPurgeTrash={(item) => runWorkspaceAction(async () => {
          await api.purgeTrash(item.entity_type, item.entity_id);
          await refresh();
          const removesActive = item.entity_type === "folder"
            || (item.entity_type === "project" && activeProject?.id === item.entity_id)
            || (item.entity_type === "session" && activeSession?.id === item.entity_id);
          if (removesActive) navigate(null, null);
        })}
      />
      {settingsOpen && <RuntimeSettingsDialog runtime={settingsRuntime} readWeave={settingsReadWeave} onClose={() => setSettingsOpen(false)} />}
      {activeProject && activeSession ? (
        <SessionConsole key={activeSession.id} project={activeProject} initial={activeSession} languageView={languageView} onLanguageView={(view) => void persistSelection(activeProject.id, activeSession.id, view)} />
      ) : activeProject ? (
        <ProjectOverview project={activeProject} sessions={snapshot.sessions.filter((session) => snapshot.session_projects[session.id] === activeProject.id && !snapshot.session_metadata.find((metadata) => metadata.session_id === session.id)?.archived_at)} onCreated={(session) => { void refresh(); navigate(activeProject.id, session.id); void persistSelection(activeProject.id, session.id); }} />
      ) : (
        <main className="workspace-empty"><div className="brand-mark">A</div><h1>课程工作区已准备好</h1><p>在左侧创建课程项目，之后每次登录都会直接回到这里</p></main>
      )}
    </div>
  );
}
