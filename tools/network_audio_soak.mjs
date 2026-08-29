// This runner downloads a controlled PCM fixture over HTTPS and drives the real GPU pipeline in wall-clock time.
import WebSocket from "ws";
const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const FIXTURE_URL = process.env.AIALRA_AUDIO_FIXTURE_URL;
const FIXTURE_PASSWORD = process.env.AIALRA_AUDIO_FIXTURE_PASSWORD;
const FIXTURE_USERNAME = process.env.AIALRA_AUDIO_FIXTURE_USERNAME || "soak";
const DURATION_MINUTES = Number(process.env.AIALRA_SOAK_MINUTES || "30");
const TEST_SUBJECT = process.env.AIALRA_TEST_SUBJECT || "";
const nativeFetch = globalThis.fetch;
globalThis.fetch = (input, init = {}) => {
  const url = String(input);
  if (!TEST_SUBJECT || !url.startsWith(API)) return nativeFetch(input, init);
  return nativeFetch(input, { ...init, headers: { ...(init.headers || {}), "X-authentik-uid": TEST_SUBJECT } });
};
const OUTAGES = (process.env.AIALRA_SOAK_OUTAGES || "120:5,600:15,1200:60")
  .split(",")
  .map((item) => item.split(":").map(Number))
  .filter(([at, duration]) => Number.isFinite(at) && Number.isFinite(duration));

if (!FIXTURE_URL?.startsWith("https://")) {
  throw new Error("AIALRA_AUDIO_FIXTURE_URL must be a controlled HTTPS audio fixture");
}
if (!FIXTURE_PASSWORD) throw new Error("AIALRA_AUDIO_FIXTURE_PASSWORD is required");
if (!/^[a-z0-9_-]{1,32}$/i.test(FIXTURE_USERNAME)) throw new Error("invalid controlled fixture username");
if (!Number.isFinite(DURATION_MINUTES) || DURATION_MINUTES <= 0 || DURATION_MINUTES > 120) {
  throw new Error("AIALRA_SOAK_MINUTES must be between 0 and 120");
}

async function checked(responsePromise) {
  const response = await responsePromise;
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return await response.json();
}

function pcmFromWave(bytes) {
  if (bytes.subarray(0, 4).toString("ascii") !== "RIFF") return bytes;
  let offset = 12;
  let format;
  let data;
  while (offset + 8 <= bytes.length) {
    const id = bytes.subarray(offset, offset + 4).toString("ascii");
    const size = bytes.readUInt32LE(offset + 4);
    if (id === "fmt ") format = {
      encoding: bytes.readUInt16LE(offset + 8), channels: bytes.readUInt16LE(offset + 10),
      sampleRate: bytes.readUInt32LE(offset + 12), bits: bytes.readUInt16LE(offset + 22),
    };
    if (id === "data") data = bytes.subarray(offset + 8, offset + 8 + size);
    offset += 8 + size + (size % 2);
  }
  if (!data) throw new Error("network WAV fixture has no data chunk");
  if (!format || format.encoding !== 1 || format.channels !== 1 || format.sampleRate !== 16_000 || format.bits !== 16) {
    throw new Error("network WAV fixture must be PCM16 mono at 16 kHz");
  }
  return data;
}

function frame(sequence, pcm) {
  const value = Buffer.alloc(16 + pcm.length);
  value.writeBigUInt64BE(BigInt(sequence), 0);
  value.writeBigUInt64BE(BigInt(Date.now()), 8);
  pcm.copy(value, 16);
  return value;
}

const fixtureResponse = await fetch(FIXTURE_URL, {
  cache: "no-store",
  headers: { Authorization: `Basic ${Buffer.from(`${FIXTURE_USERNAME}:${FIXTURE_PASSWORD}`).toString("base64")}` },
});
if (!fixtureResponse.ok) throw new Error(`fixture download failed: ${fixtureResponse.status}`);
const fixture = pcmFromWave(Buffer.from(await fixtureResponse.arrayBuffer()));
if (fixture.length < 32_000) throw new Error("network fixture must contain at least one second of PCM16 audio");

const project = await checked(fetch(`${API}/projects`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: `${DURATION_MINUTES} 分钟网络音频稳定性验证`, source_language: "en", target_language: "zh-CN" }),
}));
const deviceId = "network-soak-device-0001";
const session = await checked(fetch(`${API}/projects/${project.id}/sessions`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "受控网络课程音频", consent_confirmed: true, device_id: deviceId }),
}));
const lease = await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceId }),
}));

// A second device must be rejected while the first lease remains healthy.
const contention = await fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: "network-soak-observer-0002" }),
});
if (contention.status !== 409) throw new Error(`second recorder was not rejected: ${contention.status}`);

const wsBase = API.replace(/^http/, "ws").replace(/\/api\/v1$/, "");
const pending = new Map();
const sentAt = new Map();
const acknowledged = new Set();
const acknowledgementLatencyMs = [];
let socket;
let reconnectAllowed = true;
let reconnectTimer;
let outageUntil = 0;
let socketFailure;
let renewFailure;

function connect() {
  if (!reconnectAllowed || Date.now() < outageUntil) return;
  socket = new WebSocket(`${wsBase}/api/v1/sessions/${session.id}/sources/network-soak/audio`, ["aialra.audio.v1", `lease.${lease.lease_token}`], TEST_SUBJECT ? { headers: { "X-authentik-uid": TEST_SUBJECT } } : undefined);
  socket.onopen = () => pending.forEach((value) => socket.send(value));
  socket.onmessage = (message) => {
    const value = JSON.parse(String(message.data));
    if (value.type === "audio.error") socketFailure = new Error(value.message);
    if (value.type === "audio.ack") {
      acknowledged.add(value.sequence);
      const started = sentAt.get(value.sequence);
      if (started) acknowledgementLatencyMs.push(Date.now() - started);
      pending.delete(value.sequence);
      sentAt.delete(value.sequence);
    }
  };
  socket.onclose = () => {
    if (!reconnectAllowed) return;
    reconnectTimer = setTimeout(connect, Math.max(1_000, outageUntil - Date.now()));
  };
}

connect();
const renewTimer = setInterval(async () => {
  await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/renew`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ device_id: deviceId, lease_token: lease.lease_token }),
  })).catch((error) => { renewFailure = error; });
}, 10_000);

const startedAt = Date.now();
const deadline = startedAt + DURATION_MINUTES * 60_000;
let sequence = 1;
let fixtureOffset = 0;
const firedOutages = new Set();
while (Date.now() < deadline) {
  if (socketFailure) throw socketFailure;
  if (renewFailure) throw renewFailure;
  const elapsedSeconds = Math.floor((Date.now() - startedAt) / 1_000);
  for (const [at, duration] of OUTAGES) {
    if (elapsedSeconds >= at && !firedOutages.has(at)) {
      firedOutages.add(at);
      outageUntil = Date.now() + duration * 1_000;
      socket?.close(1012, "injected network outage");
    }
  }
  if (Date.now() >= outageUntil && (!socket || socket.readyState === WebSocket.CLOSED)) connect();
  if (fixtureOffset + 32_000 > fixture.length) fixtureOffset = 0;
  const value = frame(sequence, fixture.subarray(fixtureOffset, fixtureOffset + 32_000));
  fixtureOffset += 32_000;
  pending.set(sequence, value);
  sentAt.set(sequence, Date.now());
  if (socket?.readyState === WebSocket.OPEN) socket.send(value);
  sequence += 1;
  await new Promise((resolve) => setTimeout(resolve, 1_000));
}

const ackDeadline = Date.now() + 180_000;
while (pending.size > 0 && Date.now() < ackDeadline) {
  if (!socket || socket.readyState === WebSocket.CLOSED) connect();
  if (socket?.readyState === WebSocket.OPEN) pending.forEach((value) => socket.send(value));
  await new Promise((resolve) => setTimeout(resolve, 1_000));
}
clearInterval(renewTimer);
clearTimeout(reconnectTimer);
reconnectAllowed = false;
socket?.close();
if (pending.size > 0) throw new Error(`${pending.size} acknowledged audio chunks are still pending`);
if (acknowledged.size !== sequence - 1) throw new Error(`ACK mismatch: sent ${sequence - 1}, acknowledged ${acknowledged.size}`);

await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/stop`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: deviceId, lease_token: lease.lease_token }),
}));

const completionDeadline = Date.now() + 600_000;
let events = [];
while (Date.now() < completionDeadline) {
  events = await checked(fetch(`${API}/sessions/${session.id}/events`));
  if (events.some((event) => ["session.completed", "session.failed"].includes(event.event_type))) break;
  await new Promise((resolve) => setTimeout(resolve, 5_000));
}
const segments = events.filter((event) => event.event_type === "segment.finalized");
const segmentIds = segments.map((event) => event.payload.segment_id);
const duplicateSegments = segmentIds.length - new Set(segmentIds).size;
const failedJobs = events.filter((event) => event.event_type === "model.job.failed");
const oomEvents = failedJobs.filter((event) => String(event.payload.error_kind || "").toLowerCase().includes("oom"));
const finalState = events.findLast((event) => ["session.completed", "session.failed"].includes(event.event_type))?.event_type || "timeout";
const translations = events.filter((event) => event.event_type === "translation.finalized");
const explanations = events.filter((event) => event.event_type === "explanation.card.created");
const percentile = (values, quantile) => {
  if (!values.length) return null;
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * quantile) - 1)];
};
const stageLatency = (eventType) => events.filter((event) => event.event_type === eventType)
  .map((event) => Number(event.payload.elapsed_ms)).filter(Number.isFinite);
const asrP95 = percentile(stageLatency("segment.finalized"), 0.95);
const translationP95 = percentile(stageLatency("translation.finalized"), 0.95);
const explanationP95 = percentile(stageLatency("explanation.card.created"), 0.95);
if (duplicateSegments !== 0 || oomEvents.length !== 0 || finalState !== "session.completed") throw new Error(`long-run result gate failed: final=${finalState}, duplicates=${duplicateSegments}, oom=${oomEvents.length}`);
if (segments.length === 0 || translations.length === 0 || explanations.length === 0) throw new Error("real model pipeline produced an empty final stage");
if ((asrP95 ?? Infinity) > 3_000 || (translationP95 ?? Infinity) > 8_000 || (explanationP95 ?? Infinity) > 20_000) {
  throw new Error(`latency gate failed: asr=${asrP95}, translation=${translationP95}, explanation=${explanationP95}`);
}

process.stdout.write(`${JSON.stringify({
  status: "PASS",
  duration_minutes: DURATION_MINUTES,
  fixture_url_sha256: await crypto.subtle.digest("SHA-256", new TextEncoder().encode(FIXTURE_URL)).then((value) => Buffer.from(value).toString("hex")),
  sent_chunks: sequence - 1,
  acknowledged_chunks: acknowledged.size,
  pending_chunks: pending.size,
  audio_ack_p95_ms: percentile(acknowledgementLatencyMs, 0.95),
  injected_outages: [...firedOutages],
  stable_segments: segments.length,
  stable_translations: translations.length,
  explanation_cards: explanations.length,
  asr_p95_ms: asrP95,
  translation_p95_ms: translationP95,
  explanation_p95_ms: explanationP95,
  duplicate_segments: duplicateSegments,
  gpu_oom_events: oomEvents.length,
  final_state: finalState,
}, null, 2)}\n`);
