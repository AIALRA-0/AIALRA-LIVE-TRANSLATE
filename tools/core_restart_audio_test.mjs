import { readFile } from "node:fs/promises";
import WebSocket from "ws";

const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const SUBJECT = process.env.AIALRA_TEST_SUBJECT || "core-restart-owner";
const nativeFetch = globalThis.fetch;
globalThis.fetch = (input, init = {}) => nativeFetch(input, {
  ...init, headers: { ...(init.headers || {}), "X-authentik-uid": SUBJECT },
});

async function checked(responsePromise) {
  const response = await responsePromise;
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return response.json();
}

function frame(sequence, pcm) {
  const value = Buffer.alloc(16 + pcm.length);
  value.writeBigUInt64BE(BigInt(sequence), 0);
  value.writeBigUInt64BE(BigInt(Date.now()), 8);
  pcm.copy(value, 16);
  return value;
}

async function sendOne(sessionId, token, sequence, pcm) {
  const wsBase = API.replace(/^http/, "ws").replace(/\/api\/v1$/, "");
  await new Promise((resolve, reject) => {
    const socket = new WebSocket(
      `${wsBase}/api/v1/sessions/${sessionId}/sources/restart-g1/audio`,
      ["aialra.audio.v1", `lease.${token}`],
      { headers: { "X-authentik-uid": SUBJECT } },
    );
    const timer = setTimeout(() => reject(new Error(`ACK ${sequence} timed out`)), 30_000);
    socket.onopen = () => socket.send(frame(sequence, pcm));
    socket.onerror = reject;
    socket.onmessage = (message) => {
      const response = JSON.parse(String(message.data));
      if (response.type === "audio.error") return reject(new Error(response.message));
      if (response.type === "audio.ack" && response.sequence === sequence) {
        clearTimeout(timer); socket.close(); resolve();
      }
    };
  });
}

const fixture = await readFile(process.argv[2] || "data/test-fixtures/pipeline-lecture.pcm");
if (fixture.length < 64_000) throw new Error("restart fixture needs two seconds of PCM16 audio");
const project = await checked(fetch(`${API}/projects`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "Core 重启尾音恢复验证", source_language: "en", target_language: "zh-CN" }),
}));
const deviceId = "core-restart-device-0001";
const session = await checked(fetch(`${API}/projects/${project.id}/sessions`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "未满窗口重启", consent_confirmed: true, device_id: deviceId }),
}));
const lease = await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceId }),
}));
await sendOne(session.id, lease.lease_token, 1, fixture.subarray(0, 32_000));
process.stdout.write("ACK_BEFORE_RESTART\n");

let observedDown = false;
const deadline = Date.now() + 120_000;
while (Date.now() < deadline) {
  try {
    const response = await fetch(`${API}/health`, { signal: AbortSignal.timeout(2_000) });
    if (observedDown && response.ok) break;
  } catch {
    observedDown = true;
  }
  await new Promise((resolve) => setTimeout(resolve, 500));
}
if (!observedDown) throw new Error("Core restart was not observed");
await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/renew`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: deviceId, lease_token: lease.lease_token }),
}));
await sendOne(session.id, lease.lease_token, 2, fixture.subarray(32_000, 64_000));
await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/stop`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: deviceId, lease_token: lease.lease_token }),
}));

let events = [];
const completion = Date.now() + 300_000;
while (Date.now() < completion) {
  events = await checked(fetch(`${API}/sessions/${session.id}/events`));
  if (events.some((event) => event.event_type === "session.completed")) break;
  if (events.some((event) => event.event_type === "session.failed")) throw new Error("recovered session failed");
  await new Promise((resolve) => setTimeout(resolve, 2_000));
}
const chunks = events.filter((event) => event.event_type === "audio.chunk.received");
const segments = events.filter((event) => event.event_type === "segment.finalized");
const ids = segments.map((event) => event.payload.segment_id);
if (chunks.length !== 2 || segments.length === 0 || ids.length !== new Set(ids).size) {
  throw new Error(`restart recovery gate failed: chunks=${chunks.length}, segments=${segments.length}`);
}
process.stdout.write(`${JSON.stringify({ status: "PASS", acknowledged_chunks: chunks.length, stable_segments: segments.length, duplicate_segments: 0, core_restart_observed: true }, null, 2)}\n`);
