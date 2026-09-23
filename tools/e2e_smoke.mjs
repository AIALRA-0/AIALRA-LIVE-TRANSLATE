// This smoke runner exercises the compiled local services with a synthetic, non-private lecture fixture.
import { readFile } from "node:fs/promises";
import WebSocket from "ws";

const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const WS_BASE = process.env.AIALRA_WS_BASE || API.replace(/^http/, "ws").replace(/\/api\/v1$/, "");
const FIXTURE = process.argv[2] || "data/test-fixtures/pipeline-lecture.pcm";
const TEST_SUBJECT = process.env.AIALRA_TEST_SUBJECT || "";
const PROXY_MARKER = process.env.AIALRA_TEST_PROXY_MARKER === "true";
const TEST_CLOUD_TEXT = process.env.AIALRA_TEST_CLOUD_TEXT === "true";
const REPLAY_REALTIME = process.env.AIALRA_REPLAY_REALTIME === "true";
const nativeFetch = globalThis.fetch;
globalThis.fetch = (input, init = {}) => {
  const url = String(input);
  if (!TEST_SUBJECT || !url.startsWith(API)) return nativeFetch(input, init);
  return nativeFetch(input, {
    ...init,
    headers: {
      ...(init.headers || {}),
      "X-authentik-uid": TEST_SUBJECT,
      ...(PROXY_MARKER ? { "X-aialra-auth-proxy": "1" } : {}),
    },
  });
};

// JSON helpers keep failures readable without copying service error bodies into smoke logs.
async function checked(responsePromise) {
  const response = await responsePromise;
  if (!response.ok) {
    let code = "";
    try {
      const body = await response.json();
      code = typeof body?.code === "string" ? body.code : "";
    } catch {
      // A non-JSON response is still represented by its status only.
    }
    throw new Error(`HTTP ${response.status}${code ? ` (${code})` : ""}`);
  }
  return await response.json();
}

// Polling waits for asynchronous GPU work without assuming a model-specific latency.
async function waitForEvents(sessionId, predicate, timeoutMs = 300_000) {
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    const events = await checked(fetch(`${API}/sessions/${sessionId}/events`));
    if (predicate(events)) return events;
    await new Promise((resolve) => setTimeout(resolve, 2_000));
  }
  throw new Error(`model events did not satisfy the condition within ${timeoutMs} ms`);
}

// The WebSocket sender uses one-second frames and waits until every sequence has an ACK.
async function sendPcm(sessionId, leaseToken, pcm) {
  const chunks = [];
  for (let offset = 0, sequence = 1; offset < pcm.length; offset += 32_000, sequence += 1) {
    const payload = pcm.subarray(offset, Math.min(offset + 32_000, pcm.length));
    const frame = Buffer.alloc(16 + payload.length);
    frame.writeBigUInt64BE(BigInt(sequence), 0);
    frame.writeBigUInt64BE(BigInt(Date.now()), 8);
    payload.copy(frame, 16);
    chunks.push({ sequence, frame });
  }
  return await new Promise((resolve, reject) => {
    const socket = new WebSocket(
      `${WS_BASE}/api/v1/sessions/${sessionId}/sources/smoke/audio`,
      ["aialra.audio.v1", `lease.${leaseToken}`],
      TEST_SUBJECT
        ? {
            headers: {
              "X-authentik-uid": TEST_SUBJECT,
              ...(PROXY_MARKER ? { "X-aialra-auth-proxy": "1" } : {}),
            },
          }
        : undefined,
    );
    const acknowledgements = new Set();
    const acknowledgementCommitIds = new Set();
    const timer = setTimeout(() => reject(new Error("audio ACK timeout")), 60_000);
    socket.onopen = () => {
      void (async () => {
        for (const { frame } of chunks) {
          if (socket.readyState !== WebSocket.OPEN) break;
          socket.send(frame);
          if (REPLAY_REALTIME) await new Promise((resume) => setTimeout(resume, 1_000));
        }
      })();
    };
    socket.onerror = () => reject(new Error("audio WebSocket failed"));
    socket.onmessage = (message) => {
      const response = JSON.parse(String(message.data));
      if (response.type === "audio.error") reject(new Error("audio endpoint rejected frame"));
      if (response.type !== "audio.ack") return;
      acknowledgements.add(response.sequence);
      if (typeof response.commit_id === "string" && response.commit_id.length > 0) {
        acknowledgementCommitIds.add(response.sequence);
      }
      if (acknowledgements.size === chunks.length) {
        clearTimeout(timer);
        socket.close();
        resolve({
          count: acknowledgements.size,
          commitIdsValid: acknowledgementCommitIds.size === chunks.length,
        });
      }
    };
  });
}

async function waitForReadWeave(projectId, sessionId, timeoutMs = 120_000) {
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    const status = await checked(fetch(`${API}/projects/${projectId}/readweave`));
    const preview = await checked(fetch(`${API}/projects/${projectId}/readweave/preview`));
    const readable = preview.sessions?.find((item) => item.session_id === sessionId);
    if (
      status.configured &&
      status.queued === 0 &&
      status.syncing === 0 &&
      status.conflicts === 0 &&
      readable?.latest_entries?.some((entry) => entry.original && entry.translation)
    ) {
      return { status, preview: readable };
    }
    await new Promise((resolve) => setTimeout(resolve, 2_000));
  }
  throw new Error(`ReadWeave did not become readable within ${timeoutMs} ms`);
}

// Summary/explanation work is intentionally asynchronous after session completion.
// Wait for the queue to settle instead of treating a short-lived leased summary
// as a failed recording or a failed deployment.
async function waitForQueueDrain(timeoutMs = 180_000) {
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    const health = await checked(fetch(`${API}/health`));
    if (health.model_queue?.queued === 0 && health.model_queue?.leased === 0) return health;
    await new Promise((resolve) => setTimeout(resolve, 2_000));
  }
  throw new Error(`model queue did not drain within ${timeoutMs} ms`);
}

// The browser renews its 45-second recording lease while asynchronous model
// work is running.  Keep the production smoke equivalent so a slow but valid
// explanation cannot turn the final stop into a false lease-expired failure.
function startLeaseRenewal(projectId, sessionId, deviceId, leaseToken) {
  let stopped = false;
  let pending = Promise.resolve();
  let renewalError = null;
  const renew = async () => {
    if (stopped) return;
    pending = checked(fetch(`${API}/projects/${projectId}/sessions/${sessionId}/recording/renew`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ device_id: deviceId, lease_token: leaseToken }),
    })).catch((error) => {
      renewalError = error;
    });
    await pending;
  };
  const timer = setInterval(() => void renew(), 10_000);
  return async () => {
    stopped = true;
    clearInterval(timer);
    await pending;
    if (renewalError) throw renewalError;
  };
}

// One real session covers consent, audio durability, ASR, translation, asset parsing, explanation, and stop.
const startedAt = Date.now();
let project;
try {
project = await checked(
  fetch(`${API}/projects`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ title: "端到端验证项目", source_language: "en", target_language: "zh-CN" }),
  }),
);
const deviceId = "smoke-device-0001";
const session = await checked(fetch(`${API}/projects/${project.id}/sessions`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "端到端合成课程验证", consent_confirmed: true, device_id: deviceId }),
}));
if (TEST_CLOUD_TEXT) {
  const policy = await checked(fetch(`${API}/projects/${project.id}/ai-policy`, {
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ cloud_enabled: true, allowed_modalities: ["text"] }),
  }));
  if (!policy.cloud_enabled || !policy.route_available) throw new Error("cloud text policy did not become active");
}
const lease = await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceId }),
}));
const stopLeaseRenewal = startLeaseRenewal(project.id, session.id, deviceId, lease.lease_token);
const contention = await fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: "smoke-observer-0002" }),
});
if (contention.status !== 409) throw new Error(`second recorder was not rejected: ${contention.status}`);
const capabilities = await checked(fetch(`${API}/sessions/${session.id}/dingtalk/capabilities`));
const pcm = await readFile(FIXTURE);
const acknowledgements = await sendPcm(session.id, lease.lease_token, pcm);
if (!acknowledgements.commitIdsValid) throw new Error("one or more durable ACKs lacked commit_id");
let events = await waitForEvents(
  session.id,
  (items) =>
    items.some((item) => item.event_type === "segment.finalized") &&
    items.some((item) => item.event_type === "translation.finalized"),
);

// A text page enters the lightweight material library without triggering an explanation.
const material = new FormData();
material.append(
  "file",
  new Blob(["Pipeline forwarding reduces some read-after-write stalls."], { type: "text/plain" }),
  "pipeline-notes.txt",
);
const uploadedMaterial = await checked(fetch(`${API}/sessions/${session.id}/assets`, { method: "POST", body: material }));
if (uploadedMaterial.explain_job_id) throw new Error("material upload unexpectedly queued an explanation");
await waitForEvents(
  session.id,
  (items) => items.some((item) => item.event_type === "asset.page.extracted"),
);
await stopLeaseRenewal();
await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/stop`, {
  method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceId, lease_token: lease.lease_token }),
}));
events = await waitForEvents(
  session.id,
  (items) =>
    items.some((item) => item.event_type === "session.completed") &&
    items.some((item) => item.event_type === "translation.finalized") &&
    items.some((item) => item.event_type === "explanation.card.created") &&
    items.some((item) => item.event_type === "session.summary.created"),
  600_000,
);
const card = events.find((item) => item.event_type === "explanation.card.created");
const teaching = card?.payload?.result;
if (!teaching?.teaching_sections || teaching.teaching_sections.version !== 1) {
  throw new Error("course explanation lacks the structured teaching contract");
}
if (TEST_CLOUD_TEXT && !String(teaching.provider).startsWith("kuafushe:")) {
  throw new Error("authorized teaching did not use the direct cloud provider");
}
const question = await checked(fetch(`${API}/sessions/${session.id}/questions`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ question: "这段课程的核心概念是什么？", card_id: card.payload.card_id }),
}));
events = await waitForEvents(session.id,
  (items) => items.some((item) => item.event_type === "course.question.answered" && item.payload.job_id === question.job_id),
  300_000);
function assertStructuredAnswer(jobId) {
  const result = events.find((item) => item.event_type === "course.question.answered" && item.payload.job_id === jobId);
  const answer = String(result?.payload?.answer || "");
  const sufficient = result?.payload?.sufficient_evidence === true;
  if (!answer || (sufficient && !["直接回答", "依据", "适用边界"].every(
    (label) => new RegExp(`(?:^|\\n)\\s*(?:\\*\\*)?${label}(?:\\*\\*)?[：:](?:\\*\\*)?`).test(answer),
  ))) throw new Error("course answer lacks the three-part evidence structure");
}
assertStructuredAnswer(question.job_id);
const followUp = await checked(fetch(`${API}/sessions/${session.id}/questions`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ question: "它适用于什么条件？", card_id: card.payload.card_id, parent_job_id: question.job_id }),
}));
events = await waitForEvents(session.id,
  (items) => items.some((item) => item.event_type === "course.question.answered" && item.payload.job_id === followUp.job_id),
  300_000);
assertStructuredAnswer(followUp.job_id);
if (events.some((item) => item.event_type === "model.job.failed")) {
  throw new Error("session contains a final model.job.failed event");
}
const readWeave = await waitForReadWeave(project.id, session.id);
const health = await waitForQueueDrain();

// Machine-readable output is stored by the caller and can be compared across model changes.
const count = (eventType) => events.filter((item) => item.event_type === eventType).length;
process.stdout.write(
  `${JSON.stringify(
    {
      status: "PASS",
      elapsed_ms: Date.now() - startedAt,
      audio_acknowledgements: acknowledgements.count,
      acknowledgement_commit_ids_valid: acknowledgements.commitIdsValid,
      audio_chunks: count("audio.chunk.received"),
      stable_segments: count("segment.finalized"),
      stable_translations: count("translation.finalized"),
      extracted_pages: count("asset.page.extracted"),
      explanation_cards: count("explanation.card.created"),
      course_summaries: count("session.summary.created"),
      answered_questions: count("course.question.answered"),
      second_device_status: contention.status,
      readweave_configured: readWeave.status.configured,
      readweave_readable_entries: readWeave.preview.latest_entries.length,
      build_id: health.build_id,
      dingtalk_configured: capabilities.configured,
      dingtalk_live_pcm_verified: capabilities.incremental_pcm_verified,
    },
    null,
    2,
  )}\n`,
);
} finally {
  if (project?.id) {
    await checked(fetch(`${API}/projects/${project.id}/placement`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ folder_id: null, sort_order: 9999, archived: true }),
    })).catch(() => undefined);
  }
}
