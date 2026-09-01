import { FormEvent, useCallback, useEffect, useReducer, useRef, useState } from "react";
import { api, subscribeEvents, subscribeProject, subscribeWorkspace, type DingtalkCapabilities, type RuntimeHealth } from "./api";
import { BrowserCapture, testMicrophone, type CaptureMode, type CapturePhase, type MicrophoneTestProgress, type MicrophoneTestResult } from "./audio";
import { applySessionStateEvent } from "./sessionState";
import { appendEvent } from "./timeline";
import type { EventEnvelope, LanguageView, Project, ReadWeavePreview, ReadWeaveStatus, RecordingLease, Session, TimelineItem, WorkspaceFolder, WorkspaceSnapshot } from "./types";

const LEASE_STORAGE_KEY = "aialra-active-recording-lease";
const TIMELINE_PAGE_SIZE = 160;

interface TimelineState { events: EventEnvelope[]; items: TimelineItem[] }
type TimelineAction = { type: "append"; event: EventEnvelope } | { type: "reset" };

function timelineReducer(state: TimelineState, action: TimelineAction): TimelineState {
  return action.type === "reset" ? { events: [], items: [] } : appendEvent(state, action.event);
}

function recorderDeviceId(): string {
  const key = "aialra-recorder-device-id";
  const existing = window.localStorage.getItem(key);
  if (existing) return existing;
  const created = `browser-${crypto.randomUUID()}`;
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
  if (phase === "stopping") return "正在停止并保存尾音";
  if (phase === "processing") return "模型处理中";
  if (phase === "error") return "录音需要处理";
  if (sessionState === "recording" && !hasLease) return "其他设备正在收音";
  if (sessionState === "processing") return "模型处理中";
  if (sessionState === "completed") return "已完成";
  if (sessionState === "failed") return "处理失败";
  if (sessionState === "ready") return "可以开始录音";
  return "尚未录音";
}

function capturePhaseTone(phase: CapturePhase, sessionState: string, hasLease: boolean): "green" | "yellow" | "red" | "gray" {
  if (phase === "recording" || phase === "error" || (sessionState === "recording" && !hasLease)) return "red";
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
  return hasLease ? "继续连接收音" : "开始录音";
}

function routeSelection(): { projectId: string | null; sessionId: string | null; section: string | null } {
  const match = window.location.pathname.match(/^\/app\/projects\/([^/]+)(?:\/sessions\/([^/]+))?(?:\/notes\/([^/]+))?\/?$/);
  return { projectId: match?.[1] ?? null, sessionId: match?.[2] ?? null, section: match?.[3] ?? null };
}

function navigate(projectId: string | null, sessionId: string | null, section?: string | null): void {
  const path = projectId
    ? `/app/projects/${projectId}${sessionId ? `/sessions/${sessionId}${section ? `/notes/${section}` : ""}` : ""}`
    : "/app";
  window.history.pushState({}, "", path);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

function StatusBadge({ tone, children }: { tone: "green" | "yellow" | "red" | "gray"; children: React.ReactNode }) {
  return <span className={`status-badge ${tone}`}><i aria-hidden="true" />{children}</span>;
}

type ThemeMode = "light" | "dark";
type WorkspaceDialogState =
  | { action: "create-folder" | "create-project" }
  | { action: "rename-folder" | "move-folder" | "archive-folder"; folder: WorkspaceFolder }
  | { action: "rename-project" | "move-project" | "archive-project"; project: Project };

function WorkspaceSidebar({ snapshot, activeProjectId, activeSessionId, theme, onToggleTheme, onSelectProject, onSelectSession, onCreateFolder, onCreateProject, onUpdateFolder, onPlaceProject, onUpdateProject, onUpdateSession }: {
  snapshot: WorkspaceSnapshot;
  activeProjectId: string | null;
  activeSessionId: string | null;
  theme: ThemeMode;
  onToggleTheme: () => void;
  onSelectProject: (project: Project) => void;
  onSelectSession: (project: Project, session: Session) => void;
  onCreateFolder: (title: string, parentId: string | null) => Promise<void>;
  onCreateProject: (title: string, folderId: string | null) => Promise<void>;
  onUpdateFolder: (folder: WorkspaceFolder, title: string, parentId: string | null, archived: boolean) => Promise<void>;
  onPlaceProject: (project: Project, folderId: string | null, archived: boolean) => Promise<void>;
  onUpdateProject: (project: Project, title: string) => Promise<void>;
  onUpdateSession: (project: Project, session: Session, archived: boolean) => Promise<void>;
}) {
  const [mobileOpen, setMobileOpen] = useState(false);
  const [selectedFolderId, setSelectedFolderId] = useState<string | null>(null);
  const [expandedFolderIds, setExpandedFolderIds] = useState<Set<string>>(
    () => new Set(snapshot.folders.filter((folder) => folder.parent_id === null).map((folder) => folder.id)),
  );
  const [dialog, setDialog] = useState<WorkspaceDialogState | null>(null);
  const [dialogTitle, setDialogTitle] = useState("");
  const [dialogParentId, setDialogParentId] = useState<string>("");
  const placements = new Map(snapshot.project_placements.map((item) => [item.project_id, item]));
  const sessionMetadata = new Map(snapshot.session_metadata.map((item) => [item.session_id, item]));
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

  function openDialog(next: WorkspaceDialogState): void {
    setDialog(next);
    setDialogTitle("folder" in next ? next.folder.title : "project" in next ? next.project.title : "");
    setDialogParentId(
      "folder" in next ? next.folder.parent_id ?? ""
        : "project" in next ? placements.get(next.project.id)?.folder_id ?? ""
          : selectedFolderId ?? "",
    );
  }

  async function submitDialog(event: FormEvent): Promise<void> {
    event.preventDefault();
    if (!dialog) return;
    const title = dialogTitle.trim();
    if (dialog.action === "create-folder" && title) await onCreateFolder(title, dialogParentId || null);
    if (dialog.action === "create-project" && title) await onCreateProject(title, dialogParentId || null);
    if (dialog.action === "rename-folder" && title) await onUpdateFolder(dialog.folder, title, dialog.folder.parent_id, false);
    if (dialog.action === "move-folder") await onUpdateFolder(dialog.folder, dialog.folder.title, dialogParentId || null, false);
    if (dialog.action === "archive-folder") await onUpdateFolder(dialog.folder, dialog.folder.title, dialog.folder.parent_id, true);
    if (dialog.action === "rename-project" && title) await onUpdateProject(dialog.project, title);
    if (dialog.action === "move-project") await onPlaceProject(dialog.project, dialogParentId || null, false);
    if (dialog.action === "archive-project") await onPlaceProject(dialog.project, placements.get(dialog.project.id)?.folder_id ?? null, true);
    setDialog(null);
  }

  const dialogNeedsTitle = dialog?.action === "create-folder" || dialog?.action === "create-project" || dialog?.action === "rename-folder" || dialog?.action === "rename-project";
  const dialogNeedsParent = dialog?.action === "create-folder" || dialog?.action === "create-project" || dialog?.action === "move-folder" || dialog?.action === "move-project";
  const dialogTitleText = dialog ? ({
    "create-folder": "新建课程文件夹", "create-project": "新建课程项目", "rename-folder": "重命名文件夹", "rename-project": "重命名项目",
    "move-folder": "移动文件夹", "move-project": "移动项目", "archive-folder": "归档文件夹", "archive-project": "归档项目",
  } as Record<WorkspaceDialogState["action"], string>)[dialog.action] : "";

  const renderProject = (project: Project) => (
    <li key={project.id} className="tree-project">
      <div className={`tree-item-row ${activeProjectId === project.id && !activeSessionId ? "selected" : ""}`}>
        <button onClick={() => onSelectProject(project)}><span aria-hidden="true">▣</span><span>{project.title}</span></button>
        <details className="tree-actions">
          <summary aria-label={`管理项目 ${project.title}`}>•••</summary>
          <div>
            <button onClick={() => openDialog({ action: "rename-project", project })}>重命名</button>
            <button onClick={() => openDialog({ action: "move-project", project })}>移动</button>
            <button onClick={() => openDialog({ action: "archive-project", project })}>归档</button>
          </div>
        </details>
      </div>
      {activeProjectId === project.id && (
        <ul className="tree-sessions">
          {projectSessions(project.id).map((session) => (
            <li key={session.id}>
              <div className={`tree-item-row ${activeSessionId === session.id ? "selected" : ""}`}><button onClick={() => onSelectSession(project, session)}><span aria-hidden="true">◫</span><span>{session.title}</span><i className={`tiny-dot ${stateTone(session.state)}`} aria-label={stateLabel(session.state)} /></button><button aria-label={`归档 ${session.title}`} disabled={session.state === "recording"} onClick={() => void onUpdateSession(project, session, true)}>⌫</button></div>
              {activeSessionId === session.id && (
                <>
                  <div className="system-notes-caption">课程文档 · 自动整理</div>
                  <ul className="system-notes">
                    {Object.entries(SYSTEM_NOTE_LABELS).map(([section, title]) => (
                      <li key={section}><button title={section === "user-notes" ? "只由你编辑，AIALRA 不会覆盖正文" : "AIALRA 自动整理并同步到 ReadWeave"} onClick={() => navigate(project.id, session.id, section)}>{title}</button></li>
                    ))}
                  </ul>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
    </li>
  );

  const renderFolder = (folder: WorkspaceFolder, depth: number): React.ReactNode => (
    <li key={folder.id} className="tree-folder" style={{ "--tree-depth": depth } as React.CSSProperties}>
      <div className={`folder-label ${selectedFolderId === folder.id ? "selected" : ""}`}>
        <button
          aria-expanded={expandedFolderIds.has(folder.id)}
          onClick={() => {
            setSelectedFolderId(folder.id);
            setExpandedFolderIds((current) => {
              const next = new Set(current);
              if (next.has(folder.id)) next.delete(folder.id);
              else next.add(folder.id);
              return next;
            });
          }}
        >
          <span aria-hidden="true">{expandedFolderIds.has(folder.id) ? "▾" : "▸"}</span>{folder.title}
        </button>
        <details className="tree-actions">
          <summary aria-label={`管理文件夹 ${folder.title}`}>•••</summary>
          <div>
            <button onClick={() => openDialog({ action: "rename-folder", folder })}>重命名</button>
            <button onClick={() => openDialog({ action: "move-folder", folder })}>移动</button>
            <button onClick={() => openDialog({ action: "archive-folder", folder })}>归档</button>
          </div>
        </details>
      </div>
      {expandedFolderIds.has(folder.id) && <ul>
        {snapshot.folders.filter((item) => !item.archived_at && item.parent_id === folder.id).map((child) => renderFolder(child, depth + 1))}
        {projectsInFolder(folder.id).map(renderProject)}
      </ul>}
    </li>
  );

  return (
    <aside className={`workspace-sidebar ${mobileOpen ? "mobile-open" : ""}`} aria-label="课程工作区">
      <div className="workspace-brand"><span>A</span><div><strong>AIALRA</strong><small>课程工作区</small></div><button className="theme-toggle" aria-label={`切换到${theme === "light" ? "黑色" : "白色"}模式`} onClick={onToggleTheme}>{theme === "light" ? "◐ 黑色" : "◑ 白色"}</button><button className="mobile-tree-toggle" aria-expanded={mobileOpen} onClick={() => setMobileOpen((current) => !current)}>{mobileOpen ? "关闭课程树" : "打开课程树"}</button></div>
      <nav className="workspace-tree">
        <div className="tree-heading"><span>我的课程</span><div className="tree-heading-actions"><button aria-label="新建文件夹" onClick={() => openDialog({ action: "create-folder" })}>＋文件夹</button><button aria-label="新建项目" onClick={() => openDialog({ action: "create-project" })}>＋项目</button></div></div>
        <ul>
          {snapshot.folders.filter((folder) => !folder.archived_at && folder.parent_id === null).map((folder) => renderFolder(folder, 0))}
          {projectsInFolder(null).map(renderProject)}
        </ul>
      </nav>
      <div className="workspace-create">
        {(snapshot.folders.some((folder) => folder.archived_at) || snapshot.project_placements.some((placement) => placement.archived_at)) && <details className="archive-list"><summary>已归档</summary>{snapshot.folders.filter((folder) => folder.archived_at).map((folder) => <button key={folder.id} onClick={() => void onUpdateFolder(folder, folder.title, folder.parent_id, false)}>恢复文件夹 {folder.title}</button>)}{snapshot.projects.filter((project) => placements.get(project.id)?.archived_at).map((project) => <button key={project.id} onClick={() => void onPlaceProject(project, placements.get(project.id)?.folder_id ?? null, false)}>恢复项目 {project.title}</button>)}</details>}
      </div>
      {dialog && <div className="workspace-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setDialog(null); }}><dialog className="workspace-dialog" open aria-modal="true" aria-labelledby="workspace-dialog-title" onCancel={() => setDialog(null)}>
        <form method="dialog" onSubmit={(event) => void submitDialog(event)}>
          <header><div><p>工作区操作</p><h2 id="workspace-dialog-title">{dialogTitleText}</h2></div><button type="button" aria-label="关闭" onClick={() => setDialog(null)}>×</button></header>
          {dialogNeedsTitle && <label>名称<input autoFocus value={dialogTitle} placeholder="请输入清晰的名称" onChange={(event) => setDialogTitle(event.target.value)} required /></label>}
          {dialogNeedsParent && <label>位置<select value={dialogParentId} onChange={(event) => setDialogParentId(event.target.value)}><option value="">工作区根目录</option>{snapshot.folders.filter((folder) => !folder.archived_at && !("folder" in dialog && folder.id === dialog.folder.id)).map((folder) => <option key={folder.id} value={folder.id}>{folder.title}</option>)}</select></label>}
          {(dialog.action === "archive-folder" || dialog.action === "archive-project") && <p className="dialog-warning">归档后不会删除录音、字幕或笔记，可以随时从“已归档”恢复</p>}
          <footer><button type="button" className="secondary-button" onClick={() => setDialog(null)}>取消</button><button type="submit" className={dialog.action.startsWith("archive") ? "stop-button" : "primary-button"} disabled={dialogNeedsTitle && !dialogTitle.trim()}>{dialog.action.startsWith("archive") ? "确认归档" : "保存"}</button></footer>
        </form>
      </dialog></div>}
    </aside>
  );
}

function ProjectOverview({ project, sessions, onCreated }: { project: Project; sessions: Session[]; onCreated: (session: Session) => void }) {
  const [title, setTitle] = useState("今天的课程");
  const [consent, setConsent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function create(event: FormEvent): Promise<void> {
    event.preventDefault(); setBusy(true); setError("");
    try { onCreated(await api.createProjectSession(project.id, { title, consent_confirmed: consent, device_id: recorderDeviceId() })); }
    catch (caught) { setError(caught instanceof Error ? caught.message : "创建课程会话失败"); }
    finally { setBusy(false); }
  }
  return (
    <main className="project-overview">
      <header><p className="eyebrow">课程项目</p><h1>{project.title}</h1><p>会话、材料、转写与 ReadWeave 笔记都保存在这个项目中</p></header>
      <div className="project-overview-grid">
        <section className="overview-card"><h2>最近课程</h2>{sessions.length ? sessions.map((session) => <button key={session.id} onClick={() => navigate(project.id, session.id)}><span>{session.title}</span><StatusBadge tone={stateTone(session.state)}>{stateLabel(session.state)}</StatusBadge></button>) : <p>还没有课程会话</p>}</section>
        <form className="overview-card new-session" onSubmit={create}>
          <h2>新建课程会话</h2>
          <label>课程名称<input value={title} placeholder="例如：机器学习导论" onChange={(event) => setTitle(event.target.value)} required /></label>
          <div className="language-pair"><span>英文讲授</span><span>→</span><span>简体中文</span></div>
          <label className="check-row"><input type="checkbox" checked={consent} onChange={(event) => setConsent(event.target.checked)} /><span>我已获得课程录音许可</span></label>
          {error && <p className="error-message" role="alert">{error}</p>}
          <button className="primary-button" disabled={busy || !consent} aria-describedby="create-session-help">{busy ? "正在创建课程会话" : "创建课程并进入录音台"}</button>
          <p id="create-session-help" className="form-help">创建后会直接进入当前项目的录音工作台</p>
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

function SessionConsole({ project, initial, languageView, onLanguageView }: { project: Project; initial: Session; languageView: LanguageView; onLanguageView: (view: LanguageView) => void }) {
  const [session, setSession] = useState(initial);
  const [timeline, dispatch] = useReducer(timelineReducer, { events: [], items: [] });
  const [streamConnected, setStreamConnected] = useState(false);
  const [captureStatus, setCaptureStatus] = useState("尚未连接麦克风");
  const [capturePhase, setCapturePhase] = useState<CapturePhase>("idle");
  const [captureNotice, setCaptureNotice] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [lease, setLease] = useState<RecordingLease | null>(null);
  const [captureActive, setCaptureActive] = useState(false);
  const [captureMode, setCaptureMode] = useState<CaptureMode>("microphone");
  const [audioInputs, setAudioInputs] = useState<MediaDeviceInfo[]>([]);
  const [selectedAudioInput, setSelectedAudioInput] = useState("");
  const [micProgress, setMicProgress] = useState<MicrophoneTestProgress | null>(null);
  const [micResult, setMicResult] = useState<MicrophoneTestResult | null>(null);
  const [micTesting, setMicTesting] = useState(false);
  const [runtime, setRuntime] = useState<RuntimeHealth | null>(null);
  const [dingtalk, setDingtalk] = useState<DingtalkCapabilities | null>(null);
  const [dingtalkRecording, setDingtalkRecording] = useState(false);
  const [readWeave, setReadWeave] = useState<ReadWeaveStatus | null>(null);
  const [readWeavePreview, setReadWeavePreview] = useState<ReadWeavePreview | null>(null);
  const [pairing, setPairing] = useState<{ code: string; expires_at: string } | null>(null);
  const [visibleItemLimit, setVisibleItemLimit] = useState(TIMELINE_PAGE_SIZE);
  const capture = useRef<BrowserCapture | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);

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
    }, setStreamConnected);
  }, [initial.id]);

  useEffect(() => subscribeProject(project.id, (update) => {
    if (update.session_id === initial.id && update.update_type === "recording.lease.acquired" && update.payload.holder_device_id !== recorderDeviceId()) {
      capture.current?.revoke(); capture.current = null; setLease(null); saveLocalLease(null); setCaptureActive(false); setCapturePhase("error"); setCaptureNotice("另一台设备已经接管录音，本机已停止收音");
    }
    if (update.update_type.startsWith("readweave.")) void api.readWeaveStatus(project.id).then(setReadWeave).catch(() => setReadWeave(null));
  }, setStreamConnected), [project.id, initial.id]);

  useEffect(() => {
    void api.dingtalkCapabilities(initial.id).then(setDingtalk).catch(() => setDingtalk(null));
    void api.readWeaveStatus(project.id).then(setReadWeave).catch(() => setReadWeave(null));
    void api.readWeavePreview(project.id).then(setReadWeavePreview).catch(() => setReadWeavePreview(null));
    if (navigator.mediaDevices?.enumerateDevices) void navigator.mediaDevices.enumerateDevices().then((devices) => setAudioInputs(devices.filter((device) => device.kind === "audioinput")));
    const restored = restoredLocalLease(project.id, initial.id);
    if (restored) void api.renewRecording(project.id, initial.id, recorderDeviceId(), restored.lease_token).then(() => {
      setLease(restored);
      setCapturePhase("connecting");
      setCaptureStatus("录音租约已恢复，可继续收音");
    }).catch(() => saveLocalLease(null));
  }, [project.id, initial.id]);

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

  useEffect(() => () => capture.current?.dispose(), []);

  async function runMicTest(): Promise<void> {
    setMicTesting(true); setMicResult(null); setCaptureNotice("");
    try { setMicResult(await testMicrophone(selectedAudioInput || undefined, setMicProgress)); }
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
      () => {
        setCaptureActive(false);
        setCapturePhase("error");
        setCaptureNotice("录音权限已由另一台设备接管，未确认音频仍保留在本机");
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
    let dingtalkStarted = false;
    try {
      // Request the browser input while the click still carries user intent.
      // The server lease is created only after this local step succeeds.
      preparedCapture = new BrowserCapture(
        project.id,
        session.id,
        recorderDeviceId(),
        (message) => reportCaptureStatus(message),
        captureMode,
        selectedAudioInput || undefined,
        () => {
          setCaptureActive(false);
          setCapturePhase("error");
          setCaptureNotice("录音权限已由另一台设备接管，未确认音频仍保留在本机");
        },
      );
      capture.current = preparedCapture;
      await preparedCapture.prepare();
      setCapturePhase("acquiring-lease");
      acquired = await api.acquireRecording(project.id, session.id, recorderDeviceId());
      setLease(acquired);
      saveLocalLease(acquired);
      setSession((current) => ({ ...current, state: "recording" }));
      await startCapture(acquired, preparedCapture);
      if (dingtalk?.configured) {
        try {
          await api.startDingtalk(project.id, session.id, recorderDeviceId(), acquired.lease_token);
          dingtalkStarted = true;
          setDingtalkRecording(true);
        } catch {
          setCaptureNotice("DingTalk A1 未启动，但浏览器录音已经开始");
        }
      }
    } catch (caught) {
      if (dingtalkStarted && acquired) {
        await api.stopDingtalk(project.id, session.id, recorderDeviceId(), acquired.lease_token).catch(() => undefined);
        setDingtalkRecording(false);
      }
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
      setCapturePhase("error");
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
      let dingtalkStopFailed = false;
      if (dingtalkRecording && lease) {
        // DingTalk A1 is a backup path.  A transient control failure must not
        // leave the primary browser lease recording forever or block its durable
        // stop and tail flush.
        await api.stopDingtalk(project.id, session.id, recorderDeviceId(), lease.lease_token)
          .catch(() => { dingtalkStopFailed = true; });
        setDingtalkRecording(false);
      }
      if (!lease) throw new Error("本机不是当前录音设备");
      const next = await api.stopRecording(project.id, session.id, recorderDeviceId(), lease.lease_token);
      setSession(next); setLease(null); saveLocalLease(null);
      setCapturePhase(next.state === "processing" ? "processing" : "idle");
      if (dingtalkStopFailed) {
        setNotice("浏览器采集已结束，DingTalk A1 停止请求失败，请稍后在设备侧核对；服务器已安全封存音频并开始处理");
      }
    } catch (caught) {
      setCapturePhase("error");
      setCaptureNotice(caught instanceof Error ? caught.message : "停止录音失败");
    }
    finally { setBusy(false); }
  }

  async function upload(file: File): Promise<void> {
    setBusy(true); setNotice(`正在保存并解析 ${file.name}`);
    try { const result = await api.uploadAsset(session.id, file); setNotice(`真实解析任务 ${result.job_id.slice(0, 12)} 已排队`); }
    catch (caught) { setNotice(caught instanceof Error ? caught.message : "材料解析失败"); }
    finally { setBusy(false); if (fileInput.current) fileInput.current.value = ""; }
  }

  async function createDevicePairing(): Promise<void> {
    setBusy(true); setCaptureNotice("");
    try { setPairing(await api.createDevicePairing(project.id, session.id)); }
    catch (caught) { setCaptureNotice(caught instanceof Error ? caught.message : "手机配对失败"); }
    finally { setBusy(false); }
  }

  const isRecording = ["recording", "degraded"].includes(session.state);
  const visibleCaptureStatus = session.state === "processing"
    ? "录音已停止，真实模型仍在处理"
    : session.state === "completed" ? "录音和模型处理均已完成" : captureStatus;
  const captureTone = capturePhaseTone(capturePhase, session.state, Boolean(lease));
  const captureLabel = capturePhaseLabel(capturePhase, session.state, Boolean(lease), captureMode);
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
        <div><p className="eyebrow">{project.title}</p><h1>{session.title}</h1></div>
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
        </section>
        <aside className="session-sidebar">
          <section className="side-card capture-card">
            <div className="card-heading"><h3>录音</h3><StatusBadge tone={captureTone}>{captureLabel}</StatusBadge></div>
            <label>音频来源<select value={captureMode} onChange={(event) => setCaptureMode(event.target.value as CaptureMode)} disabled={isRecording}><option value="microphone">麦克风</option><option value="screen">浏览器标签或共享音频</option></select></label>
            {captureMode === "microphone" && <label>输入设备<select value={selectedAudioInput} onChange={(event) => { setSelectedAudioInput(event.target.value); setMicResult(null); }} disabled={isRecording}><option value="">系统默认麦克风</option>{audioInputs.map((device) => <option key={device.deviceId} value={device.deviceId}>{device.label || `麦克风 ${device.deviceId.slice(0, 6)}`}</option>)}</select></label>}
            <div className="mic-test">
              <div className="mic-meter"><span style={{ width: `${Math.max(2, Math.min(100, ((micProgress?.levelDbfs ?? -96) + 96) / 0.96))}%` }} /></div>
              <StatusBadge tone={micTesting ? "yellow" : micResult?.passed ? "green" : micResult ? "red" : "yellow"}>{micTesting ? micProgress?.phase === "quiet" ? "请保持安静" : "请朗读一句话" : micResult?.message ?? "麦克风尚未测试"}</StatusBadge>
              {micResult && <small>噪声 {micResult.noiseFloorDbfs.toFixed(1)} dBFS · 语音 {micResult.speechP95Dbfs.toFixed(1)} dBFS · 峰值 {micResult.peakDbfs.toFixed(1)} dBFS</small>}
              <button className="secondary-button" disabled={micTesting || isRecording || captureMode !== "microphone"} onClick={() => void runMicTest()}>{micTesting ? "测试中 4 秒" : "测试麦克风"}</button>
            </div>
            <p className="capture-help">默认使用当前设备的麦克风；只有本机没有麦克风时才需要安卓手机</p>
            {captureNotice && <div className="capture-inline-alert" role="alert">{captureNotice}</div>}
            <p className="capture-copy" aria-live="polite"><strong>{captureLabel}</strong> · {visibleCaptureStatus}</p>
            {!isRecording ? (
              <button className="primary-button" disabled={busy || capturePhase === "processing" || session.state === "processing"} onClick={() => void begin()}>{captureActionLabel(capturePhase, Boolean(lease), captureMode)}</button>
            ) : lease && captureActive ? (
              <button className="stop-button" disabled={busy} onClick={() => void stop()}>停止并完成处理</button>
            ) : lease ? (
              <button className="primary-button" disabled={busy} onClick={() => void continueCapture()}>继续连接收音</button>
            ) : (
              <p className="other-recorder-notice">另一台设备正在录音，请在录音设备停止</p>
            )}
            <details className="device-pairing">
              <summary>没有本机麦克风？使用安卓手机作为备用收音</summary>
              <p>有本机麦克风时无需 Android。配对只用于没有可用麦克风的设备，失败也不会影响浏览器录音</p>
              <button className="secondary-button" disabled={busy || isRecording} onClick={() => void createDevicePairing()}>生成安卓备用连接码</button>
              {pairing && <div className="pairing-code"><strong>{pairing.code}</strong><small>5 分钟内有效，只能使用一次</small><a href={`aialra://pair?server=${encodeURIComponent(window.location.origin)}&code=${encodeURIComponent(pairing.code)}`}>在安卓手机打开配对</a></div>}
            </details>
          </section>
          <GpuPanel runtime={runtime} />
          <section className="side-card">
            <div className="card-heading"><h3>讲解与材料</h3><StatusBadge tone={modelStatusTone}>{summaryRetryable ? "总结可重试" : modelQueueDepth > 0 ? "队列处理中" : "可用"}</StatusBadge></div>
            <button className="secondary-button" disabled={busy || !timeline.items.some((item) => item.kind === "paragraph")} onClick={() => void api.explain(session.id).then(() => setNotice("补充讲解已交给本机 GPU")).catch((caught) => setNotice(caught instanceof Error ? caught.message : "讲解失败"))}>根据最近内容讲解</button>
            {summaryRetryable && <button className="secondary-button" disabled={busy || isRecording} onClick={() => void api.summarize(project.id, session.id).then(() => setNotice("课程总结已重新排队，完成后会在当前页面出现")).catch((caught) => setNotice(caught instanceof Error ? caught.message : "课程总结重试失败"))}>重试课程总结</button>}
            <input ref={fileInput} className="visually-hidden" type="file" accept=".pptx,.pdf,.docx,.png,.jpg,.jpeg,.webp,.txt,.md,.csv" onChange={(event) => { const file = event.target.files?.[0]; if (file) void upload(file); }} />
            <button className="secondary-button" disabled={busy} onClick={() => fileInput.current?.click()}>上传 PPT、PDF 或图片</button>
            <p>7B 负责连贯段落翻译和知识补充，14B 在录音停止后生成最终课程总结</p>
          </section>
          <section className="side-card readweave-card">
            <div className="card-heading"><h3>ReadWeave</h3><StatusBadge tone={readWeaveTone}>{!readWeave?.configured ? "未配置" : readWeave.conflicts > 0 ? "存在冲突" : readWeave.syncing > 0 || readWeave.queued > 0 ? "同步中" : "已同步"}</StatusBadge></div>
            <p>{readWeavePreview?.sessions.find((item) => item.session_id === session.id)?.latest_entries[0]?.translation ?? "稳定字幕和讲解会自动进入对应笔记"}</p>
            <button className="secondary-button" disabled={!readWeave?.configured} onClick={() => void api.reconcileReadWeave(project.id).then(() => setNotice("ReadWeave 全量校对已排队"))}>立即校对</button>
            {readWeaveUrl && <button className="text-link-button" onClick={() => window.location.assign(readWeaveUrl)}>在当前标签打开{readWeaveNodeType ? "对应" : "项目"}笔记 →</button>}
          </section>
          {notice && <div className="notice-box" role="status">{notice}</div>}
        </aside>
      </main>
    </div>
  );
}

export default function App() {
  const [deviceId] = useState(recorderDeviceId);
  const [snapshot, setSnapshot] = useState<WorkspaceSnapshot | null>(null);
  const [route, setRoute] = useState(routeSelection);
  const [error, setError] = useState("");
  const [theme, setTheme] = useState<ThemeMode>(() => window.localStorage.getItem("aialra-theme") === "dark" ? "dark" : "light");

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

  const activeProject = snapshot?.projects.find((project) => project.id === route.projectId) ?? null;
  const activeSession = snapshot?.sessions.find((session) => session.id === route.sessionId) ?? null;
  const languageView = snapshot?.preference?.language_view ?? "bilingual";

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
        onSelectProject={(project) => { navigate(project.id, null); void persistSelection(project.id, null); }}
        onSelectSession={(project, session) => { navigate(project.id, session.id); void persistSelection(project.id, session.id); }}
        onCreateFolder={(title, parentId) => runWorkspaceAction(async () => { await api.createFolder({ title, parent_id: parentId }); await refresh(); })}
        onCreateProject={(title, folderId) => runWorkspaceAction(async () => { const project = await api.createProject(title); if (folderId) await api.placeProject(project.id, { folder_id: folderId, sort_order: 0, archived: false }); await refresh(); navigate(project.id, null); await persistSelection(project.id, null); })}
        onUpdateFolder={(folder, title, parentId, archived) => runWorkspaceAction(async () => { await api.updateFolder(folder.id, { title, parent_id: parentId, sort_order: folder.sort_order, archived }); await refresh(); })}
        onPlaceProject={(project, folderId, archived) => runWorkspaceAction(async () => { const placement = snapshot.project_placements.find((item) => item.project_id === project.id); await api.placeProject(project.id, { folder_id: folderId, sort_order: placement?.sort_order ?? 0, archived }); await refresh(); if (archived && activeProject?.id === project.id) navigate(null, null); })}
        onUpdateProject={(project, title) => runWorkspaceAction(async () => { await api.updateProject(project.id, title.trim()); await refresh(); })}
        onUpdateSession={(project, session, archived) => runWorkspaceAction(async () => { const metadata = snapshot.session_metadata.find((item) => item.session_id === session.id); await api.updateSession(project.id, session.id, { pinned: metadata?.pinned ?? false, sort_order: metadata?.sort_order ?? 0, archived }); await refresh(); if (archived && activeSession?.id === session.id) navigate(project.id, null); })}
      />
      {activeProject && activeSession ? (
        <SessionConsole key={activeSession.id} project={activeProject} initial={activeSession} languageView={languageView} onLanguageView={(view) => void persistSelection(activeProject.id, activeSession.id, view)} />
      ) : activeProject ? (
        <ProjectOverview project={activeProject} sessions={snapshot.sessions.filter((session) => snapshot.session_projects[session.id] === activeProject.id)} onCreated={(session) => { void refresh(); navigate(activeProject.id, session.id); void persistSelection(activeProject.id, session.id); }} />
      ) : (
        <main className="workspace-empty"><div className="brand-mark">A</div><h1>课程工作区已准备好</h1><p>在左侧创建课程项目，之后每次登录都会直接回到这里</p></main>
      )}
    </div>
  );
}
