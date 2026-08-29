// A controlled private PCM fixture verifies durable ACK, out-of-order assembly, and exact retransmission semantics.
import { readFile } from "node:fs/promises";
import WebSocket from "ws";

const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const SUBJECT = process.env.AIALRA_TEST_SUBJECT || "audio-reordering-owner";
const nativeFetch = globalThis.fetch;
globalThis.fetch = (input, init = {}) => nativeFetch(input, {
  ...init,
  headers: { ...(init.headers || {}), "X-authentik-uid": SUBJECT },
});

async function checked(responsePromise) {
  const response = await responsePromise;
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return response.json();
}

function frame(sequence, capturedAt, pcm) {
  const value = Buffer.alloc(16 + pcm.length);
  value.writeBigUInt64BE(BigInt(sequence), 0);
  value.writeBigUInt64BE(BigInt(capturedAt), 8);
  pcm.copy(value, 16);
  return value;
}

const fixture = await readFile(process.argv[2] || "data/test-fixtures/pipeline-lecture.pcm");
if (fixture.length < 64_000) throw new Error("reordering fixture needs two seconds of PCM16 audio");
const project = await checked(fetch(`${API}/projects`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "音频乱序与重传验证", source_language: "en", target_language: "zh-CN" }),
}));
const deviceId = "audio-reordering-device-0001";
const session = await checked(fetch(`${API}/projects/${project.id}/sessions`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "乱序补传", consent_confirmed: true, device_id: deviceId }),
}));
const lease = await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: deviceId }),
}));

const capturedAt = Date.now();
const first = frame(1, capturedAt, fixture.subarray(0, 32_000));
const second = frame(2, capturedAt + 1_000, fixture.subarray(32_000, 64_000));
const wsBase = API.replace(/^http/, "ws").replace(/\/api\/v1$/, "");
const socket = new WebSocket(
  `${wsBase}/api/v1/sessions/${session.id}/sources/reordering-g1/audio`,
  ["aialra.audio.v1", `lease.${lease.lease_token}`],
  { headers: { "X-authentik-uid": SUBJECT } },
);
const replies = [];
socket.onmessage = (message) => replies.push(JSON.parse(String(message.data)));
await new Promise((resolve, reject) => {
  const timer = setTimeout(() => reject(new Error("audio WebSocket did not open")), 20_000);
  socket.onopen = () => { clearTimeout(timer); resolve(); };
  socket.onerror = reject;
});

async function sendAndWait(value, predicate) {
  const start = replies.length;
  socket.send(value);
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const reply = replies.slice(start).find(predicate);
    if (reply) return reply;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("audio response timed out");
}

const ackTwo = await sendAndWait(second, (reply) => reply.type === "audio.ack" && reply.sequence === 2);
const ackOne = await sendAndWait(first, (reply) => reply.type === "audio.ack" && reply.sequence === 1);
const duplicate = await sendAndWait(first, (reply) => reply.type === "audio.ack" && reply.sequence === 1 && reply.duplicate === true);
if (ackTwo.duplicate || ackOne.duplicate || !duplicate.duplicate) throw new Error("unexpected duplicate ACK semantics");
socket.close();

await checked(fetch(`${API}/projects/${project.id}/sessions/${session.id}/recording/stop`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: deviceId, lease_token: lease.lease_token }),
}));

let events = [];
const completion = Date.now() + 300_000;
while (Date.now() < completion) {
  events = await checked(fetch(`${API}/sessions/${session.id}/events`));
  if (events.some((event) => event.event_type === "session.completed")) break;
  if (events.some((event) => event.event_type === "session.failed")) throw new Error("reordering session failed");
  await new Promise((resolve) => setTimeout(resolve, 2_000));
}
const chunks = events.filter((event) => event.event_type === "audio.chunk.received");
const segments = events.filter((event) => event.event_type === "segment.finalized");
const segmentIds = segments.map((event) => event.payload.segment_id);
if (chunks.length !== 2 || segments.length === 0 || segmentIds.length !== new Set(segmentIds).size) {
  throw new Error(`reordering gate failed: chunks=${chunks.length}, segments=${segments.length}`);
}
process.stdout.write(`${JSON.stringify({
  status: "PASS",
  acknowledged_out_of_order: [2, 1],
  exact_retransmission_duplicate: true,
  committed_chunks: chunks.length,
  stable_segments: segments.length,
  duplicate_segments: 0,
}, null, 2)}\n`);
