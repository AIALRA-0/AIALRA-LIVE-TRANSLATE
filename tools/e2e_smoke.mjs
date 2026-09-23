// This runner covers the local services with a synthetic fixture or an authorized retained recording.
// `--retained-recording` is an explicitly authorized, in-memory replay path for local E2E runs.
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import WebSocket from "ws";

const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const WS_BASE = process.env.AIALRA_WS_BASE || API.replace(/^http/, "ws").replace(/\/api\/v1$/, "");
const CLI_ARGS = process.argv.slice(2);
const RETAINED_RECORDING = CLI_ARGS.includes("--retained-recording");
const PREPARE_ONLY = CLI_ARGS.includes("--prepare-only");
const POSITIONAL_ARGS = CLI_ARGS.filter((argument) => !argument.startsWith("--"));
const UNKNOWN_OPTIONS = CLI_ARGS.filter((argument) => argument.startsWith("--") &&
  !["--retained-recording", "--prepare-only", "--help"].includes(argument));
if (CLI_ARGS.includes("--help")) {
  process.stdout.write(
    "Usage: node tools/e2e_smoke.mjs [fixture.pcm] | --retained-recording [--prepare-only]\n" +
    "--retained-recording loads a consented, completed local-only recording window in memory\n" +
    "--prepare-only validates and describes the selected audio without calling services\n" +
    "The E2E replay requires AIALRA_TEST_CLOUD_TEXT=true; the server policy is restricted to text\n",
  );
  process.exit(0);
}
if (UNKNOWN_OPTIONS.length > 0 || POSITIONAL_ARGS.length > 1 ||
    (RETAINED_RECORDING && POSITIONAL_ARGS.length > 0) || (PREPARE_ONLY && !RETAINED_RECORDING)) {
  throw new Error("invalid E2E smoke runner arguments; use --help for usage");
}
if (RETAINED_RECORDING && !PREPARE_ONLY && process.env.AIALRA_TEST_CLOUD_TEXT !== "true") {
  throw new Error("retained recording E2E requires explicit server-side cloud text opt-in");
}
const FIXTURE = POSITIONAL_ARGS[0] || "data/test-fixtures/pipeline-lecture.pcm";
const TEST_SUBJECT = process.env.AIALRA_TEST_SUBJECT || "";
const PROXY_MARKER = process.env.AIALRA_TEST_PROXY_MARKER === "true";
const TEST_CLOUD_TEXT = process.env.AIALRA_TEST_CLOUD_TEXT === "true";
const REPLAY_REALTIME = RETAINED_RECORDING || process.env.AIALRA_REPLAY_REALTIME === "true";
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
      code = typeof body?.code === "string" ? body.code.replace(/[^a-z0-9_-]/gi, "") : "";
    } catch {
      // A non-JSON response is still represented by its status only.
    }
    throw new Error(`HTTP ${response.status}${code ? ` (${code})` : ""}`);
  }
  return await response.json();
}

// Read a short real recording window without exporting it to a file or logging
// any session identifiers, object hashes, event payloads, transcripts, or keys.
async function loadRetainedRecording() {
  let DatabaseSync;
  try {
    ({ DatabaseSync } = await import("node:sqlite"));
  } catch {
    throw new Error("retained recording mode requires Node.js with built-in SQLite support");
  }

  const dataRoot = "data";
  const database = new DatabaseSync(join(dataRoot, "aialra.sqlite"), { readOnly: true });
  let sourceChunks;
  let selectedWindowStart;
  let stableEventCount;
  try {
    database.exec("PRAGMA query_only = ON");
    const candidates = database.prepare(`
      SELECT s.id, COUNT(*) AS chunk_count
      FROM sessions s
      JOIN audio_chunks c ON c.session_id = s.id
      WHERE s.consent_confirmed = 1
        AND s.demo_mode = 0
        AND s.privacy_mode = 'local_only'
        AND s.state = 'completed'
      GROUP BY s.id
      HAVING COUNT(*) >= 20
      ORDER BY chunk_count DESC
    `).all();

    for (const candidate of candidates) {
      const chunks = database.prepare(`
        SELECT sequence, captured_at_ms, sample_rate, channels, encoding,
               duration_ms, object_hash, size_bytes
        FROM audio_chunks
        WHERE session_id = ?
        ORDER BY sequence
      `).all(candidate.id);
      if (chunks.length !== candidate.chunk_count) continue;
      const validSequence = chunks.every((chunk, index) =>
        chunk.sequence === chunks[0].sequence + index &&
        chunk.sample_rate === 16_000 &&
        chunk.channels === 1 &&
        chunk.encoding === "pcm_s16le" &&
        chunk.duration_ms === 1_000 &&
        chunk.size_bytes === 32_000,
      );
      if (!validSequence) continue;

      const stableTimes = database.prepare(`
        SELECT captured_at_wall
        FROM events
        WHERE session_id = ? AND event_type = 'segment.finalized'
        ORDER BY rowid
      `).all(candidate.id)
        .map((event) => Date.parse(event.captured_at_wall))
        .filter(Number.isFinite);
      if (stableTimes.length === 0) continue;

      const nearestEvents = stableTimes.map((capturedAt) => {
        let low = 0;
        let high = chunks.length;
        while (low < high) {
          const mid = (low + high) >>> 1;
          if (chunks[mid].captured_at_ms < capturedAt) low = mid + 1;
          else high = mid;
        }
        const right = Math.min(low, chunks.length - 1);
        const left = Math.max(0, right - 1);
        const center = Math.abs(chunks[left].captured_at_ms - capturedAt) <=
          Math.abs(chunks[right].captured_at_ms - capturedAt) ? left : right;
        return {
          center,
          deltaMs: Math.abs(chunks[center].captured_at_ms - capturedAt),
        };
      }).sort((left, right) => left.deltaMs - right.deltaMs);

      sourceChunks = chunks;
      stableEventCount = stableTimes.length;
      const testedStarts = new Set();
      for (const event of nearestEvents) {
        const start = Math.max(0, Math.min(chunks.length - 20, event.center - 10));
        if (testedStarts.has(start)) continue;
        testedStarts.add(start);
        selectedWindowStart = start;
        break;
      }
      if (selectedWindowStart !== undefined) break;
    }
  } finally {
    database.close();
  }

  if (!sourceChunks || selectedWindowStart === undefined) {
    throw new Error("no eligible completed, consented local-only recording with stable ASR timing was found");
  }

  // The closest stable ASR timestamp is centered in a 20-second window.
  const windowCandidates = sourceChunks.slice(selectedWindowStart, selectedWindowStart + 20);
  if (windowCandidates.length !== 20) throw new Error("retained recording window is not contiguous");

  const buffers = [];
  let hasSignal = false;
  for (const chunk of windowCandidates) {
    const match = /^sha256:([a-f0-9]{64})$/i.exec(chunk.object_hash);
    if (!match) throw new Error("retained recording object reference is invalid");
    const hash = match[1].toLowerCase();
    let bytes;
    try {
      bytes = await readFile(join(dataRoot, "objects", hash.slice(0, 2), hash));
    } catch {
      throw new Error("retained recording object is unavailable");
    }
    if (bytes.length !== chunk.size_bytes ||
        `sha256:${createHash("sha256").update(bytes).digest("hex")}` !== chunk.object_hash.toLowerCase()) {
      throw new Error("retained recording object integrity check failed");
    }
    for (let offset = 0; offset < bytes.length; offset += 2) {
      if (bytes.readInt16LE(offset) !== 0) {
        hasSignal = true;
        break;
      }
    }
    buffers.push(bytes);
  }
  if (!hasSignal) throw new Error("selected retained recording window contains no audio signal");

  return {
    pcm: Buffer.concat(buffers),
    metadata: {
      source: "retained_recording_memory",
      duration_seconds: 20,
      sample_rate_hz: 16_000,
      channels: 1,
      encoding: "pcm_s16le",
      signal_detected: true,
      near_stable_asr_event: true,
      stable_asr_event_count: stableEventCount,
    },
  };
}

// Polling waits for asynchronous GPU work without assuming a model-specific latency.
async function waitForEvents(sessionId, predicate, timeoutMs = 300_000) {
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    const events = await checked(fetch(`${API}/sessions/${sessionId}/events`));
    if (predicate(events)) return events;
    const failed = events.find((item) => item.event_type === "model.job.failed");
    if (failed) {
      const kind = String(failed.payload?.error_kind || "unknown").replace(/[^a-z0-9_-]/gi, "");
      throw new Error(`terminal model job failed (${kind})`);
    }
    await new Promise((resolve) => setTimeout(resolve, 2_000));
  }
  throw new Error(`model events did not satisfy the condition within ${timeoutMs} ms`);
}

// The WebSocket sender uses one-second frames and waits until every sequence has an ACK.
async function sendPcm(sessionId, leaseToken, pcm, onAcknowledgement) {
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
      onAcknowledgement?.(acknowledgements.size, chunks.length);
      if (acknowledgements.size >= chunks.length) {
        clearTimeout(timer);
        socket.close();
        const persistedSequences = chunks.filter(({ sequence }) =>
          acknowledgements.has(sequence) && acknowledgementCommitIds.has(sequence),
        ).length;
        resolve({
          count: persistedSequences,
          commitIdsValid: persistedSequences === chunks.length,
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

// One isolated session covers consent, audio durability, ASR, translation, explanation, and stop.
async function runE2e(pcmInput) {
const startedAt = Date.now();
let project;
let result;
let archived = false;
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
  body: JSON.stringify({
    title: RETAINED_RECORDING ? "保留录音回放验证" : "端到端合成课程验证",
    consent_confirmed: true,
    device_id: deviceId,
  }),
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
if (RETAINED_RECORDING) process.stderr.write("已获许可，开始实时回放保留录音\n");
const acknowledgements = await sendPcm(
  session.id,
  lease.lease_token,
  pcmInput.pcm,
  RETAINED_RECORDING
    ? (acknowledged, total) => process.stderr.write(
        `\r录音回放中：${acknowledged}/${total} 秒已持久化确认`,
      )
    : undefined,
);
if (RETAINED_RECORDING) process.stderr.write("\n");
if (!acknowledgements.commitIdsValid) throw new Error("one or more durable ACKs lacked commit_id");
const stopRecording = async () => {
  await stopLeaseRenewal();
  await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/stop`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceId, lease_token: lease.lease_token }),
  }));
};
// A short retained sample can have interim text but no sealed segment until
// stop flushes the ASR window. Do not wait for a finalized segment first.
if (RETAINED_RECORDING) await stopRecording();
let events = await waitForEvents(
  session.id,
  (items) =>
    items.some((item) => item.event_type === "segment.finalized") &&
    items.some((item) => item.event_type === "translation.finalized"),
);

if (!RETAINED_RECORDING) {
  // This synthetic material is unrelated to the retained course, so only add it to fixture runs.
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
  await stopRecording();
}
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
result = {
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
      ...(pcmInput.metadata ? { audio_input: pcmInput.metadata } : {}),
    };
} finally {
  if (project?.id) {
    const archiveRequest = checked(fetch(`${API}/projects/${project.id}/placement`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ folder_id: null, sort_order: 9999, archived: true }),
    }));
    if (RETAINED_RECORDING) {
      const archivedPlacement = await archiveRequest;
      if (!archivedPlacement.archived_at) {
        throw new Error("isolated retained-recording project archive was not confirmed");
      }
      archived = true;
    } else {
      await archiveRequest.catch(() => undefined);
    }
  }
}
if (RETAINED_RECORDING) result.isolated_test_project_archived = archived;
process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

const pcmInput = RETAINED_RECORDING
  ? await loadRetainedRecording()
  : { pcm: await readFile(FIXTURE), metadata: null };
if (PREPARE_ONLY) {
  process.stdout.write(`${JSON.stringify({ status: "PREPARED", audio_input: pcmInput.metadata }, null, 2)}\n`);
} else {
  await runE2e(pcmInput);
}
