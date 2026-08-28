// Validate that audio remains durable while the private GPU agent is offline, then verify recovery.
import { readFile } from "node:fs/promises";

const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";

async function checked(responsePromise) {
  const response = await responsePromise;
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return await response.json();
}

async function events(sessionId) {
  return await checked(fetch(`${API}/sessions/${sessionId}/events`));
}

async function waitFor(sessionId, predicate, timeoutMs = 300_000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const current = await events(sessionId);
    if (predicate(current)) return current;
    await new Promise((resolve) => setTimeout(resolve, 2_000));
  }
  throw new Error("offline recovery timed out");
}

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
    const socket = new WebSocket(`${wsBase}/api/v1/sessions/${sessionId}/sources/offline/audio`);
    const acknowledgements = new Set();
    const timer = setTimeout(() => reject(new Error("offline audio ACK timeout")), 60_000);
    socket.onopen = () => chunks.forEach(({ frame }) => socket.send(frame));
    socket.onerror = () => reject(new Error("offline audio WebSocket failed"));
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

const mode = process.argv[2];
if (mode === "enqueue") {
  const pcm = await readFile(process.argv[3] || "data/test-fixtures/pipeline-lecture.pcm");
  const session = await checked(fetch(`${API}/sessions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      title: "GPU 离线恢复验证",
      source_language: "en",
      target_language: "zh-CN",
      consent_confirmed: true,
      demo_mode: false,
    }),
  }));
  await checked(fetch(`${API}/sessions/${session.id}/start`, { method: "POST" }));
  const acknowledgements = await sendPcm(session.id, pcm);
  await checked(fetch(`${API}/sessions/${session.id}/stop`, { method: "POST" }));
  await new Promise((resolve) => setTimeout(resolve, 5_000));
  const health = await checked(fetch(`${API}/health`));
  const current = await events(session.id);
  const segments = current.filter((item) => item.event_type === "segment.finalized").length;
  if (segments !== 0 || health.model_queue.queued < 1) {
    throw new Error("offline gate produced model output or did not queue work");
  }
  process.stdout.write(`${JSON.stringify({ status: "QUEUED", session_id: session.id, audio_acknowledgements: acknowledgements, stable_segments: segments, queued_jobs: health.model_queue.queued })}\n`);
} else if (mode === "wait") {
  const sessionId = process.argv[3];
  if (!sessionId) throw new Error("session id is required for wait mode");
  const recovered = await waitFor(sessionId, (items) =>
    items.some((item) => item.event_type === "session.completed") &&
    items.some((item) => item.event_type === "translation.finalized"),
  );
  const count = (type) => recovered.filter((item) => item.event_type === type).length;
  const segmentIds = recovered.filter((item) => item.event_type === "segment.finalized").map((item) => item.payload.segment_id);
  if (new Set(segmentIds).size !== segmentIds.length) throw new Error("recovery produced duplicate stable segments");
  process.stdout.write(`${JSON.stringify({ status: "RECOVERED", session_id: sessionId, stable_segments: count("segment.finalized"), stable_translations: count("translation.finalized"), duplicate_segments: 0 })}\n`);
} else {
  throw new Error("use enqueue or wait mode");
}
