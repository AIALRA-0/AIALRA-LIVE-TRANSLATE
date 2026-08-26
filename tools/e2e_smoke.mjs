// This smoke runner exercises the compiled local services with a synthetic, non-private lecture fixture.
import { readFile } from "node:fs/promises";

const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const FIXTURE = process.argv[2] || "data/test-fixtures/pipeline-lecture.pcm";

// JSON helpers surface the service error body and preserve one readable failure boundary.
async function checked(responsePromise) {
  const response = await responsePromise;
  if (!response.ok) {
    throw new Error(`${response.status} ${await response.text()}`);
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
async function sendPcm(sessionId, pcm) {
  const wsBase = API.replace(/^http/, "ws").replace(/\/api\/v1$/, "");
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
      `${wsBase}/api/v1/sessions/${sessionId}/sources/smoke/audio`,
    );
    const acknowledgements = new Set();
    const timer = setTimeout(() => reject(new Error("audio ACK timeout")), 60_000);
    socket.onopen = () => chunks.forEach(({ frame }) => socket.send(frame));
    socket.onerror = () => reject(new Error("audio WebSocket failed"));
    socket.onmessage = (message) => {
      const response = JSON.parse(String(message.data));
      if (response.type === "audio.error") reject(new Error(response.message));
      if (response.type !== "audio.ack") return;
      acknowledgements.add(response.sequence);
      if (acknowledgements.size === chunks.length) {
        clearTimeout(timer);
        socket.close();
        resolve(acknowledgements.size);
      }
    };
  });
}

// One real session covers consent, audio durability, ASR, translation, asset parsing, explanation, and stop.
const startedAt = Date.now();
const session = await checked(
  fetch(`${API}/sessions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      title: "端到端合成课程验证",
      source_language: "en",
      target_language: "zh-CN",
      consent_confirmed: true,
      demo_mode: false,
    }),
  }),
);
await checked(fetch(`${API}/sessions/${session.id}/start`, { method: "POST" }));
const capabilities = await checked(fetch(`${API}/sessions/${session.id}/dingtalk/capabilities`));
const pcm = await readFile(FIXTURE);
const expectedWindows = Math.floor(pcm.length / 128_000);
const acknowledgements = await sendPcm(session.id, pcm);
let events = await waitForEvents(
  session.id,
  (items) =>
    items.some((item) => item.event_type === "segment.finalized") &&
    items.some((item) => item.event_type === "translation.finalized"),
);

// A text page proves that newly supplied material becomes eligible for the next explanation.
const material = new FormData();
material.append(
  "file",
  new Blob(["Pipeline forwarding reduces some read-after-write stalls."], { type: "text/plain" }),
  "pipeline-notes.txt",
);
await checked(fetch(`${API}/sessions/${session.id}/assets`, { method: "POST", body: material }));
await checked(fetch(`${API}/sessions/${session.id}/explain`, { method: "POST" }));
await checked(fetch(`${API}/sessions/${session.id}/stop`, { method: "POST" }));
events = await waitForEvents(
  session.id,
  (items) =>
    items.some((item) => item.event_type === "session.completed") &&
    items.filter((item) => item.event_type === "translation.finalized").length >= expectedWindows,
);

// Machine-readable output is stored by the caller and can be compared across model changes.
const count = (eventType) => events.filter((item) => item.event_type === eventType).length;
process.stdout.write(
  `${JSON.stringify(
    {
      status: "PASS",
      session_id: session.id,
      elapsed_ms: Date.now() - startedAt,
      audio_acknowledgements: acknowledgements,
      audio_chunks: count("audio.chunk.received"),
      stable_segments: count("segment.finalized"),
      stable_translations: count("translation.finalized"),
      extracted_pages: count("asset.page.extracted"),
      explanation_cards: count("explanation.card.created"),
      dingtalk_configured: capabilities.configured,
      dingtalk_live_pcm_verified: capabilities.incremental_pcm_verified,
    },
    null,
    2,
  )}\n`,
);
