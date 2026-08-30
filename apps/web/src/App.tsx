import { FormEvent, useCallback, useEffect, useReducer, useRef, useState } from "react";
import { api, subscribeEvents, subscribeProject, subscribeWorkspace, type DingtalkCapabilities, type RuntimeHealth } from "./api";
import { BrowserCapture, testMicrophone, type CaptureMode, type MicrophoneTestProgress, type MicrophoneTestResult } from "./audio";
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

function WorkspaceSidebar({ snapshot, activeProjectId, activeSessionId, onSelectProject, onSelectSession, onCreateFolder, onCreateProject, onUpdateFolder, onPlaceProject, onUpdateProject, onUpdateSession }: {
  snapshot: WorkspaceSnapshot;
  activeProjectId: string | null;
  activeSessionId: string | null;
  onSelectProject: (project: Project) => void;
  onSelectSession: (project: Project, session: Session) => void;
  onCreateFolder: (title: string, parentId: string | null) => Promise<void>;
  onCreateProject: (title: string) => Promise<void>;
  onUpdateFolder: (folder: WorkspaceFolder, title: string, parentId: string | null, archived: boolean) => Promise<void>;
  onPlaceProject: (project: Project, folderId: string | null, archived: boolean) => Promise<void>;
  onUpdateProject: (project: Project, title: string) => Promise<void>;
  onUpdateSession: (project: Project, session: Session, archived: boolean) => Promise<void>;
}) {
  const [mobileOpen, setMobileOpen] = useState(false);
  const [folderTitle, setFolderTitle] = useState("");
  const [projectTitle, setProjectTitle] = useState("");
  const [selectedFolderId, setSelectedFolderId] = useState<string | null>(null);
  const placements = new Map(snapshot.project_placements.map((item) => [item.project_id, item]));
  const sessionMetadata = new Map(snapshot.session_metadata.map((item) => [item.session_id, item]));
  const projectSessions = (projectId: string) => snapshot.sessions.filter((session) => snapshot.session_projects[session.id] === projectId && !sessionMetadata.get(session.id)?.archived_at);
  const projectsInFolder = (folderId: string | null) => snapshot.projects.filter((project) => {
    const placement = placements.get(project.id);
    return !placement?.archived_at && (placement?.folder_id ?? null) === folderId;
  });

  const renderProject = (project: Project) => (
    <li key={project.id} className="tree-project">
      <div className={`tree-item-row ${activeProjectId === project.id && !activeSessionId ? "selected" : ""}`}>
        <button onClick={() => onSelectProject(project)}><span aria-hidden="true">▣</span><span>{project.title}</span></button>
        <details className="tree-actions">
          <summary aria-label={`管理项目 ${project.title}`}>•••</summary>
          <div>
            <button onClick={() => { const title = window.prompt("新的项目名称", project.title); if (title?.trim()) void onUpdateProject(project, title); }}>重命名</button>
            <label>移动到<select value={placements.get(project.id)?.folder_id ?? ""} onChange={(event) => void onPlaceProject(project, event.target.value || null, false)}><option value="">根目录</option>{snapshot.folders.filter((folder) => !folder.archived_at).map((folder) => <option key={folder.id} value={folder.id}>{folder.title}</option>)}</select></label>
            <button onClick={() => { if (window.confirm(`归档项目「${project.title}」吗`)) void onPlaceProject(project, placements.get(project.id)?.folder_id ?? null, true); }}>归档</button>
          </div>
        </details>
      </div>
      {activeProjectId === project.id && (
        <ul className="tree-sessions">
          {projectSessions(project.id).map((session) => (
            <li key={session.id}>
              <div className={`tree-item-row ${activeSessionId === session.id ? "selected" : ""}`}><button onClick={() => onSelectSession(project, session)}><span aria-hidden="true">◫</span><span>{session.title}</span><i className={`tiny-dot ${stateTone(session.state)}`} aria-label={stateLabel(session.state)} /></button><button aria-label={`归档 ${session.title}`} disabled={session.state === "recording"} onClick={() => void onUpdateSession(project, session, true)}>⌫</button></div>
              {activeSessionId === session.id && (
                <ul className="system-notes">
                  {[["overview", "00 课程概览"], ["transcript", "01 实时转写与翻译"], ["explanations", "02 生僻词与补充解释"], ["assets", "03 课件与证据索引"], ["user-notes", "99 我的笔记"]].map(([section, title]) => (
                    <li key={section}><button onClick={() => navigate(project.id, session.id, section)}>{title}</button></li>
                  ))}
                </ul>
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
        <button onClick={() => setSelectedFolderId((current) => current === folder.id ? null : folder.id)}><span aria-hidden="true">▾</span>{folder.title}</button>
        <details className="tree-actions">
          <summary aria-label={`管理文件夹 ${folder.title}`}>•••</summary>
          <div>
            <button onClick={() => { const title = window.prompt("新的文件夹名称", folder.title); if (title?.trim()) void onUpdateFolder(folder, title, folder.parent_id, false); }}>重命名</button>
            <label>移动到<select value={folder.parent_id ?? ""} onChange={(event) => void onUpdateFolder(folder, folder.title, event.target.value || null, false)}><option value="">根目录</option>{snapshot.folders.filter((candidate) => !candidate.archived_at && candidate.id !== folder.id).map((candidate) => <option key={candidate.id} value={candidate.id}>{candidate.title}</option>)}</select></label>
            <button onClick={() => void onUpdateFolder(folder, folder.title, folder.parent_id, true)}>归档</button>
          </div>
        </details>
      </div>
      <ul>
        {snapshot.folders.filter((item) => !item.archived_at && item.parent_id === folder.id).map((child) => renderFolder(child, depth + 1))}
        {projectsInFolder(folder.id).map(renderProject)}
      </ul>
    </li>
  );

  return (
    <aside className={`workspace-sidebar ${mobileOpen ? "mobile-open" : ""}`} aria-label="课程工作区">
      <div className="workspace-brand"><span>A</span><div><strong>AIALRA</strong><small>课程工作区</small></div><button className="mobile-tree-toggle" aria-expanded={mobileOpen} onClick={() => setMobileOpen((current) => !current)}>{mobileOpen ? "关闭课程树" : "打开课程树"}</button></div>
      <nav className="workspace-tree">
        <div className="tree-heading"><span>我的课程</span><StatusBadge tone="green">已同步</StatusBadge></div>
        <ul>
          {snapshot.folders.filter((folder) => !folder.archived_at && folder.parent_id === null).map((folder) => renderFolder(folder, 0))}
          {projectsInFolder(null).map(renderProject)}
        </ul>
      </nav>
      <div className="workspace-create">
        <form onSubmit={(event) => { event.preventDefault(); if (folderTitle.trim()) void onCreateFolder(folderTitle, selectedFolderId).then(() => setFolderTitle("")); }}>
          <input aria-label="新文件夹名称" placeholder={selectedFolderId ? "新建子文件夹" : "新文件夹"} value={folderTitle} onChange={(event) => setFolderTitle(event.target.value)} />
          <button type="submit" disabled={!folderTitle.trim()}>＋文件夹</button>
        </form>
        <form onSubmit={(event) => { event.preventDefault(); if (projectTitle.trim()) void onCreateProject(projectTitle).then(() => setProjectTitle("")); }}>
          <input aria-label="新项目名称" placeholder="新课程项目" value={projectTitle} onChange={(event) => setProjectTitle(event.target.value)} />
          <button type="submit" disabled={!projectTitle.trim()}>＋项目</button>
        </form>
        {(snapshot.folders.some((folder) => folder.archived_at) || snapshot.project_placements.some((placement) => placement.archived_at)) && <details className="archive-list"><summary>已归档</summary>{snapshot.folders.filter((folder) => folder.archived_at).map((folder) => <button key={folder.id} onClick={() => void onUpdateFolder(folder, folder.title, folder.parent_id, false)}>恢复文件夹 {folder.title}</button>)}{snapshot.projects.filter((project) => placements.get(project.id)?.archived_at).map((project) => <button key={project.id} onClick={() => void onPlaceProject(project, placements.get(project.id)?.folder_id ?? null, false)}>恢复项目 {project.title}</button>)}</details>}
      </div>
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
          <label>课程名称<input value={title} onChange={(event) => setTitle(event.target.value)} required /></label>
          <div className="language-pair"><span>英文讲授</span><span>→</span><span>简体中文</span></div>
          <label className="check-row"><input type="checkbox" checked={consent} onChange={(event) => setConsent(event.target.checked)} /><span>我已获得课程录音许可</span></label>
          {error && <p className="error-message" role="alert">{error}</p>}
          <button className="primary-button" disabled={busy || !consent}>{busy ? "正在创建" : "创建并打开"}</button>
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
      <p>{item.body || "正在解析内容"}</p>
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

  useEffect(() => {
    return subscribeEvents(initial.id, (event) => {
      dispatch({ type: "append", event });
      setSession((current) => applySessionStateEvent(current, event.event_type));
    }, setStreamConnected);
  }, [initial.id]);

  useEffect(() => subscribeProject(project.id, (update) => {
    if (update.session_id === initial.id && update.update_type === "recording.lease.acquired" && update.payload.holder_device_id !== recorderDeviceId()) {
      capture.current?.revoke(); capture.current = null; setLease(null); saveLocalLease(null); setCaptureActive(false);
    }
    if (update.update_type.startsWith("readweave.")) void api.readWeaveStatus(project.id).then(setReadWeave).catch(() => setReadWeave(null));
  }, setStreamConnected), [project.id, initial.id]);

  useEffect(() => {
    void api.dingtalkCapabilities(initial.id).then(setDingtalk).catch(() => setDingtalk(null));
    void api.readWeaveStatus(project.id).then(setReadWeave).catch(() => setReadWeave(null));
    void api.readWeavePreview(project.id).then(setReadWeavePreview).catch(() => setReadWeavePreview(null));
    if (navigator.mediaDevices?.enumerateDevices) void navigator.mediaDevices.enumerateDevices().then((devices) => setAudioInputs(devices.filter((device) => device.kind === "audioinput")));
    const restored = restoredLocalLease(project.id, initial.id);
    if (restored) void api.renewRecording(project.id, initial.id, recorderDeviceId(), restored.lease_token).then(() => { setLease(restored); setCaptureStatus("录音租约已恢复，可继续收音"); }).catch(() => saveLocalLease(null));
  }, [project.id, initial.id]);

  useEffect(() => {
    let active = true;
    const refresh = () => void api.health().then((value) => { if (active) setRuntime(value); }).catch(() => { if (active) setRuntime(null); });
    refresh(); const timer = window.setInterval(refresh, 5_000);
    return () => { active = false; window.clearInterval(timer); };
  }, []);

  useEffect(() => {
    const visibility = () => { if (document.hidden && captureActive) setNotice("页面已进入后台，iOS 浏览器可能暂停收音，请尽快返回前台"); };
    document.addEventListener("visibilitychange", visibility);
    return () => document.removeEventListener("visibilitychange", visibility);
  }, [captureActive]);

  useEffect(() => () => capture.current?.revoke(), []);

  async function runMicTest(): Promise<void> {
    setMicTesting(true); setMicResult(null); setNotice("");
    try { setMicResult(await testMicrophone(selectedAudioInput || undefined, setMicProgress)); }
    catch (caught) { setNotice(caught instanceof Error ? caught.message : "麦克风测试失败"); }
    finally { setMicTesting(false); setMicProgress(null); }
  }

  async function startCapture(acquired: RecordingLease): Promise<void> {
    const next = new BrowserCapture(project.id, session.id, recorderDeviceId(), acquired.lease_token, acquired.generation, setCaptureStatus, captureMode, selectedAudioInput || undefined, () => setCaptureActive(false));
    capture.current = next;
    try { await next.start(); setCaptureActive(true); }
    catch (error) { next.revoke(); throw error; }
  }

  async function begin(): Promise<void> {
    setBusy(true); setNotice(""); let acquired: RecordingLease | null = null;
    try {
      acquired = await api.acquireRecording(project.id, session.id, recorderDeviceId()); setLease(acquired); saveLocalLease(acquired); setSession((current) => ({ ...current, state: "recording" }));
      if (dingtalk?.configured) try { await api.startDingtalk(session.id); setDingtalkRecording(true); } catch { setNotice("DingTalk A1 未启动，浏览器真实链路继续工作"); }
      await startCapture(acquired);
    } catch (caught) {
      if (acquired) await api.stopRecording(project.id, session.id, recorderDeviceId(), acquired.lease_token).catch(() => undefined);
      setLease(null); saveLocalLease(null); setNotice(caught instanceof Error ? caught.message : "启动失败");
    } finally { setBusy(false); }
  }

  async function stop(): Promise<void> {
    setBusy(true);
    try {
      await capture.current?.stop(); capture.current = null; setCaptureActive(false);
      if (dingtalkRecording) { await api.stopDingtalk(session.id); setDingtalkRecording(false); }
      if (!lease) throw new Error("本机不是当前录音设备");
      const next = await api.stopRecording(project.id, session.id, recorderDeviceId(), lease.lease_token); setSession(next); setLease(null); saveLocalLease(null);
    } catch (caught) { setNotice(caught instanceof Error ? caught.message : "停止失败"); }
    finally { setBusy(false); }
  }

  async function upload(file: File): Promise<void> {
    setBusy(true); setNotice(`正在保存并解析 ${file.name}`);
    try { const result = await api.uploadAsset(session.id, file); setNotice(`真实解析任务 ${result.job_id.slice(0, 12)} 已排队`); }
    catch (caught) { setNotice(caught instanceof Error ? caught.message : "材料解析失败"); }
    finally { setBusy(false); if (fileInput.current) fileInput.current.value = ""; }
  }

  async function createDevicePairing(): Promise<void> {
    setBusy(true); setNotice("");
    try { setPairing(await api.createDevicePairing(project.id, session.id)); }
    catch (caught) { setNotice(caught instanceof Error ? caught.message : "手机配对失败"); }
    finally { setBusy(false); }
  }

  const isRecording = ["recording", "degraded"].includes(session.state);
  const visibleCaptureStatus = session.state === "processing"
    ? "录音已停止，真实模型仍在处理"
    : session.state === "completed" ? "录音和模型处理均已完成" : captureStatus;
  const readWeaveTone = !readWeave?.configured ? "gray" : readWeave.conflicts > 0 ? "red" : readWeave.syncing > 0 || readWeave.queued > 0 ? "yellow" : "green";
  const section = routeSelection().section;
  const readWeaveNodeType = section === "user-notes" ? "user_notes" : section;
  const readWeaveUrl = readWeave?.targets?.find((target) => target.local_id === `${session.id}:${section === "user-notes" ? "user" : section}` || (!section && target.node_type === "session" && target.local_id === session.id))?.note_url ?? readWeave?.note_url;
  const visibleItems = timeline.items.filter((item) => {
    if (!section || section === "transcript") return section ? item.kind === "paragraph" : true;
    if (section === "overview") return item.kind === "session-summary" || item.kind === "summary";
    if (section === "explanations") return ["summary", "context", "term", "asr-warning", "review"].includes(item.kind);
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
            <div><span>课程文档</span><small>{visibleItems.filter((item) => item.kind === "paragraph").length} 个稳定段落</small></div>
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
            <div className="card-heading"><h3>录音</h3><StatusBadge tone={isRecording ? "red" : "gray"}>{isRecording ? "正在收音" : "未录音"}</StatusBadge></div>
            <label>音频来源<select value={captureMode} onChange={(event) => setCaptureMode(event.target.value as CaptureMode)} disabled={isRecording}><option value="microphone">麦克风</option><option value="screen">浏览器标签或共享音频</option></select></label>
            {captureMode === "microphone" && <label>输入设备<select value={selectedAudioInput} onChange={(event) => { setSelectedAudioInput(event.target.value); setMicResult(null); }} disabled={isRecording}><option value="">系统默认麦克风</option>{audioInputs.map((device) => <option key={device.deviceId} value={device.deviceId}>{device.label || `麦克风 ${device.deviceId.slice(0, 6)}`}</option>)}</select></label>}
            <div className="mic-test">
              <div className="mic-meter"><span style={{ width: `${Math.max(2, Math.min(100, ((micProgress?.levelDbfs ?? -96) + 96) / 0.96))}%` }} /></div>
              <StatusBadge tone={micTesting ? "yellow" : micResult?.passed ? "green" : micResult ? "red" : "yellow"}>{micTesting ? micProgress?.phase === "quiet" ? "请保持安静" : "请朗读一句话" : micResult?.message ?? "麦克风尚未测试"}</StatusBadge>
              {micResult && <small>噪声 {micResult.noiseFloorDbfs.toFixed(1)} dBFS · 语音 {micResult.speechP95Dbfs.toFixed(1)} dBFS · 峰值 {micResult.peakDbfs.toFixed(1)} dBFS</small>}
              <button className="secondary-button" disabled={micTesting || isRecording || captureMode !== "microphone"} onClick={() => void runMicTest()}>{micTesting ? "测试中 4 秒" : "测试麦克风"}</button>
            </div>
            <p className="capture-copy">{visibleCaptureStatus}</p>
            {!isRecording ? (
              <button className="primary-button" disabled={busy} onClick={() => void begin()}>开始录音</button>
            ) : lease && captureActive ? (
              <button className="stop-button" disabled={busy} onClick={() => void stop()}>停止并完成处理</button>
            ) : lease ? (
              <button className="primary-button" disabled={busy} onClick={() => void startCapture(lease)}>继续收音</button>
            ) : (
              <p className="other-recorder-notice">另一台设备正在录音，请在录音设备停止</p>
            )}
            <div className="device-pairing">
              <button className="secondary-button" disabled={busy || isRecording} onClick={() => void createDevicePairing()}>连接安卓收音</button>
              {pairing && <div className="pairing-code"><strong>{pairing.code}</strong><small>5 分钟内有效，只能使用一次</small><a href={`aialra://pair?server=${encodeURIComponent(window.location.origin)}&code=${encodeURIComponent(pairing.code)}`}>在安卓手机打开</a></div>}
            </div>
          </section>
          <GpuPanel runtime={runtime} />
          <section className="side-card">
            <div className="card-heading"><h3>讲解与材料</h3><StatusBadge tone={(runtime?.model_queue?.queued ?? 0) > 0 ? "yellow" : "green"}>{(runtime?.model_queue?.queued ?? 0) > 0 ? "队列处理中" : "可用"}</StatusBadge></div>
            <button className="secondary-button" disabled={busy || !timeline.items.some((item) => item.kind === "paragraph")} onClick={() => void api.explain(session.id).then(() => setNotice("补充讲解已交给本机 GPU")).catch((caught) => setNotice(caught instanceof Error ? caught.message : "讲解失败"))}>根据最近内容讲解</button>
            <input ref={fileInput} className="visually-hidden" type="file" accept=".pptx,.pdf,.docx,.png,.jpg,.jpeg,.webp,.txt,.md,.csv" onChange={(event) => { const file = event.target.files?.[0]; if (file) void upload(file); }} />
            <button className="secondary-button" disabled={busy} onClick={() => fileInput.current?.click()}>上传 PPT、PDF 或图片</button>
            <p>3B 负责实时翻译，复杂讲解和材料按 GPU 队列分层处理</p>
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
        onSelectProject={(project) => { navigate(project.id, null); void persistSelection(project.id, null); }}
        onSelectSession={(project, session) => { navigate(project.id, session.id); void persistSelection(project.id, session.id); }}
        onCreateFolder={(title, parentId) => runWorkspaceAction(async () => { await api.createFolder({ title, parent_id: parentId }); await refresh(); })}
        onCreateProject={(title) => runWorkspaceAction(async () => { const project = await api.createProject(title); await refresh(); navigate(project.id, null); await persistSelection(project.id, null); })}
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
