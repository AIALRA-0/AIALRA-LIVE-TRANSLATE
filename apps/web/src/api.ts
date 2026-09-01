import type { EventEnvelope, LanguageView, Project, ProjectUpdate, ReadWeavePreview, ReadWeaveStatus, RecordingLease, Session, WorkspaceFolder, WorkspaceProjectPlacement, WorkspaceSnapshot, WorkspaceUpdate } from "./types";

export interface DingtalkCapabilities {
  configured: boolean;
  a1_recording_control: boolean;
  post_recording_import: boolean;
  incremental_pcm_verified: boolean;
  incremental_transcript_verified: boolean;
  foreground_miniapp_probe: boolean;
}

export interface RuntimeHealth {
  status: string;
  service: string;
  version: string;
  deployment_mode: "local" | "server";
  processing_location: string;
  worker: {
    id: string;
    online: boolean;
    capabilities: string[];
    model_metadata: Record<string, unknown>;
    active_job_id: string | null;
    last_seen_at: string;
  } | null;
  model_queue: {
    queued: number;
    leased: number;
    completed: number;
    failed: number;
  } | null;
}

// API errors retain the server message so consent and state failures stay actionable.
async function checked<T>(responsePromise: Promise<Response> | Response): Promise<T> {
  const response = await responsePromise;
  if (!response.ok) {
    const body = (await response.json().catch(() => ({ error: response.statusText }))) as {
      error?: string;
    };
    throw new Error(body.error || `请求失败：${response.status}`);
  }
  return (await response.json()) as T;
}

// All calls use relative URLs so browser, Tauri, and the Rust static host share one client.
export const api = {
  health: () => checked<RuntimeHealth>(fetch("/api/v1/health")),
  workspace: (deviceId: string) => checked<WorkspaceSnapshot>(fetch(`/api/v1/workspace?device_id=${encodeURIComponent(deviceId)}`)),
  createFolder: (input: { title: string; parent_id: string | null; sort_order?: number }) => checked<WorkspaceFolder>(fetch("/api/v1/workspace/folders", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(input),
  })),
  updateFolder: (folderId: string, input: { title: string; parent_id: string | null; sort_order: number; archived: boolean }) => checked<WorkspaceFolder>(fetch(`/api/v1/workspace/folders/${folderId}`, {
    method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify(input),
  })),
  updatePreference: (deviceId: string, input: { active_project_id: string | null; active_session_id: string | null; language_view: LanguageView; sidebar_collapsed: boolean }) => checked(fetch(`/api/v1/workspace/preferences/${deviceId}`, {
    method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify(input),
  })),
  listSessions: () => checked<Session[]>(fetch("/api/v1/sessions")),
  listProjects: () => checked<Project[]>(fetch("/api/v1/projects")),
  createProject: (title: string) => checked<Project>(fetch("/api/v1/projects", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ title, source_language: "en", target_language: "zh-CN" }),
  })),
  updateProject: (projectId: string, title: string) => checked<Project>(fetch(`/api/v1/projects/${projectId}`, {
    method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify({ title }),
  })),
  placeProject: (projectId: string, input: { folder_id: string | null; sort_order: number; archived: boolean }) => checked<WorkspaceProjectPlacement>(fetch(`/api/v1/projects/${projectId}/placement`, {
    method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify(input),
  })),
  listProjectSessions: (projectId: string) => checked<Session[]>(fetch(`/api/v1/projects/${projectId}/sessions`)),
  updateSession: (projectId: string, sessionId: string, input: { title?: string; pinned: boolean; sort_order: number; archived: boolean }) => checked(fetch(`/api/v1/projects/${projectId}/sessions/${sessionId}`, {
    method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify(input),
  })),
  createProjectSession: (projectId: string, input: { title: string; consent_confirmed: boolean; device_id: string }) =>
    checked<Session>(fetch(`/api/v1/projects/${projectId}/sessions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(input),
    })),
  acquireRecording: (projectId: string, sessionId: string, deviceId: string) =>
    checked<RecordingLease>(fetch(`/api/v1/projects/${projectId}/sessions/${sessionId}/recording/acquire`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceId }),
    })),
  renewRecording: (projectId: string, sessionId: string, deviceId: string, leaseToken: string) =>
    checked<Record<string, unknown>>(fetch(`/api/v1/projects/${projectId}/sessions/${sessionId}/recording/renew`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceId, lease_token: leaseToken }),
    })),
  stopRecording: (projectId: string, sessionId: string, deviceId: string, leaseToken: string) =>
    checked<Session>(fetch(`/api/v1/projects/${projectId}/sessions/${sessionId}/recording/stop`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceId, lease_token: leaseToken }),
    })),
  createDevicePairing: (projectId: string, sessionId: string) =>
    checked<{ code: string; expires_at: string }>(fetch(`/api/v1/projects/${projectId}/sessions/${sessionId}/device-pairing`, { method: "POST" })),
  readWeaveStatus: (projectId: string) => checked<ReadWeaveStatus>(fetch(`/api/v1/projects/${projectId}/readweave`)),
  readWeavePreview: (projectId: string) => checked<ReadWeavePreview>(fetch(`/api/v1/projects/${projectId}/readweave/preview`)),
  reconcileReadWeave: (projectId: string) => checked<{ queued: boolean }>(fetch(`/api/v1/projects/${projectId}/readweave/reconcile`, { method: "POST" })),
  createSession: (input: {
    title: string;
    source_language: string;
    target_language: string;
    consent_confirmed: boolean;
    demo_mode: boolean;
  }) =>
    checked<Session>(
      fetch("/api/v1/sessions", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(input),
      }),
    ),
  startSession: (sessionId: string) =>
    checked<Session>(fetch(`/api/v1/sessions/${sessionId}/start`, { method: "POST" })),
  stopSession: (sessionId: string) =>
    checked<Session>(fetch(`/api/v1/sessions/${sessionId}/stop`, { method: "POST" })),
  explain: (sessionId: string) =>
    checked<{ job_id: string; status: string }>(
      fetch(`/api/v1/sessions/${sessionId}/explain`, { method: "POST" }),
    ),
  summarize: (projectId: string, sessionId: string) =>
    checked<{ job_id: string; status: string }>(
      fetch(`/api/v1/projects/${projectId}/sessions/${sessionId}/summary`, { method: "POST" }),
    ),
  dingtalkCapabilities: (sessionId: string) =>
    checked<DingtalkCapabilities>(
      fetch(`/api/v1/sessions/${sessionId}/dingtalk/capabilities`),
    ),
  startDingtalk: (projectId: string, sessionId: string, deviceId: string, leaseToken: string) =>
    checked<{ event: EventEnvelope }>(
      fetch(`/api/v1/sessions/${sessionId}/dingtalk/start`, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ project_id: projectId, device_id: deviceId, lease_token: leaseToken }),
      }),
    ),
  stopDingtalk: (projectId: string, sessionId: string, deviceId: string, leaseToken: string) =>
    checked<{ event: EventEnvelope }>(
      fetch(`/api/v1/sessions/${sessionId}/dingtalk/stop`, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ project_id: projectId, device_id: deviceId, lease_token: leaseToken }),
      }),
    ),
  uploadAsset: (sessionId: string, file: File) => {
    const form = new FormData();
    form.append("file", file);
    return checked<{ asset_id: string; job_id: string; page_ids: string[] }>(
      fetch(`/api/v1/sessions/${sessionId}/assets`, { method: "POST", body: form }),
    );
  },
};

export function subscribeWorkspace(onUpdate: (update: WorkspaceUpdate) => void, onConnection: (connected: boolean) => void): () => void {
  const source = new EventSource("/api/v1/workspace/stream");
  source.onopen = () => onConnection(true);
  source.onerror = () => onConnection(false);
  ["workspace.folder.created", "workspace.folder.updated", "workspace.folder.moved", "workspace.folder.archived", "workspace.project.created", "workspace.project.updated", "workspace.project.placed", "workspace.session.created", "workspace.session.updated"].forEach((eventName) => {
    source.addEventListener(eventName, (message) => onUpdate(JSON.parse((message as MessageEvent).data) as WorkspaceUpdate));
  });
  return () => source.close();
}

export function subscribeProject(
  projectId: string,
  onUpdate: (update: ProjectUpdate) => void,
  onConnection: (connected: boolean) => void,
): () => void {
  const source = new EventSource(`/api/v1/projects/${projectId}/stream`);
  source.onopen = () => onConnection(true);
  source.onerror = () => onConnection(false);
  source.addEventListener("session.event", (message) => onUpdate(JSON.parse((message as MessageEvent).data) as ProjectUpdate));
  ["project.created", "project.updated", "recording.lease.acquired", "recording.lease.renewed", "recording.lease.released", "readweave.synced", "readweave.conflict", "readweave.reconcile.queued"].forEach((eventName) => {
    source.addEventListener(eventName, (message) => onUpdate(JSON.parse((message as MessageEvent).data) as ProjectUpdate));
  });
  return () => source.close();
}

// EventSource reconnects automatically and the service replays durable history after reconnect.
export function subscribeEvents(
  sessionId: string,
  onEvent: (event: EventEnvelope) => void,
  onConnection: (connected: boolean) => void,
): () => void {
  const source = new EventSource(`/api/v1/sessions/${sessionId}/stream`);
  source.onopen = () => onConnection(true);
  source.onerror = () => onConnection(false);
  source.onmessage = (message) => onEvent(JSON.parse(message.data) as EventEnvelope);
  return () => source.close();
}
