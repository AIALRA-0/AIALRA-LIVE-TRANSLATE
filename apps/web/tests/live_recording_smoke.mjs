// Real browser capture against an isolated Core, with synthetic microphone input.
import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { mkdtemp, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "../../..");
const data = await mkdtemp(path.join(tmpdir(), "aialra-live-capture-"));
const base = "http://127.0.0.1:18791";
const captureWav = process.env.AIALRA_CAPTURE_WAV;
const token = randomBytes(32).toString("hex");
const headers = { "content-type": "application/json", "X-aialra-auth-proxy": "1", "X-authentik-uid": "live-capture-smoke" };
const core = spawn(process.env.AIALRA_CORE_BINARY || path.join(root, "target/debug/aialra-core-server.exe"), [], {
  cwd: root, windowsHide: true, stdio: "ignore", env: {
    ...process.env, AIALRA_BIND: "127.0.0.1:18791", AIALRA_DATA_DIR: data,
    AIALRA_DEPLOYMENT_MODE: "server", AIALRA_ALLOWED_ORIGINS: base,
    AIALRA_WORKER_TOKEN_SHA256: createHash("sha256").update(token).digest("hex"),
    AIALRA_RECORDING_ADMISSION_MAX_ASR_BACKLOG_SECONDS: "3600",
  },
});
let browser;
let heartbeat;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function request(route, method = "GET", body, requestHeaders = headers) {
  const response = await fetch(`${base}${route}`, {
    method, headers: requestHeaders, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  assert.ok(response.ok, `${method} ${route}: ${response.status}`);
  return response.json();
}

try {
  let ready = false;
  for (let attempt = 0; attempt < 50; attempt += 1) {
    try { await request("/api/v1/health"); ready = true; break; } catch { await sleep(200); }
  }
  assert.ok(ready, "isolated Core started");
  const beat = () => request("/internal/v1/workers/heartbeat", "POST", {
    worker_id: "live-capture-smoke", capabilities: ["asr"], active_job_id: null,
    model_metadata: { status: "ok", asr_available: true, asr_provider: "synthetic@cuda" },
  }, { ...headers, authorization: `Bearer ${token}` });
  await beat();
  heartbeat = setInterval(() => void beat().catch(() => {}), 10000);
  const project = await request("/api/v1/projects", "POST", { title: "Synthetic Live UI", source_language: "auto", target_language: "zh-CN" });
  const session = await request(`/api/v1/projects/${project.id}/sessions`, "POST", {
    title: "Synthetic capture", consent_confirmed: true, device_id: "live-capture-browser",
  });
  if (captureWav) assert.ok((await stat(captureWav)).size > 44, "sealed capture WAV exists");
  browser = await chromium.launch({ headless: true, channel: "msedge", args: [
    "--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream",
    ...(captureWav ? [`--use-file-for-fake-audio-capture=${captureWav}`] : []),
  ] });
  const context = await browser.newContext({ extraHTTPHeaders: headers, permissions: ["microphone"], viewport: { width: 390, height: 844 } });
  const page = await context.newPage();
  const errors = [];
  const ackCommitIds = [];
  const mediaResponses = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("response", (response) => {
    if (response.url().includes("/audio/segment")) mediaResponses.push({ status: response.status(), type: response.headers()["content-type"], length: response.headers()["content-length"], range: response.headers()["content-range"] });
  });
  page.on("websocket", (ws) => ws.on("framereceived", ({ payload }) => {
    try { const frame = JSON.parse(String(payload)); if (frame.type === "audio.ack") ackCommitIds.push(frame.commit_id); } catch { /* binary */ }
  }));
  await page.goto(`${base}/app/projects/${project.id}/sessions/${session.id}`);
  await page.getByTestId("live-reading").waitFor();
  await page.getByRole("button", { name: "开始录音", exact: true }).click();
  await page.getByRole("button", { name: "停止并完成处理", exact: true }).waitFor({ timeout: 20000 });
  await page.getByTestId("live-reading").waitFor();
  await sleep(captureWav ? 8000 : 2700);
  assert.ok(ackCommitIds.length >= 1 && ackCommitIds.every(Boolean), "browser PCM received durable ACK");
  const stopped = page.waitForResponse((response) => response.url().endsWith("/recording/stop") && response.request().method() === "POST");
  await page.getByRole("button", { name: "停止并完成处理", exact: true }).click();
  assert.ok((await stopped).ok(), "stop completed");
  await page.getByRole("button", { name: "进入复习" }).waitFor({ timeout: 20000 });
  const player = page.locator(".live-player");
  await player.getByRole("button", { name: "后退 30 秒" }).waitFor({ timeout: 20000 });
  await player.locator("audio").evaluate((audio) => { window.__mediaEvents = []; for (const type of ["loadstart", "loadedmetadata", "loadeddata", "canplay", "seeking", "seeked", "play", "playing", "waiting", "stalled", "error"]) audio.addEventListener(type, () => window.__mediaEvents.push(type)); });
  await player.getByRole("slider", { name: "回放位置" }).fill("6");
  await player.getByRole("button", { name: "后退 30 秒" }).click();
  assert.ok(Number(await player.getByRole("slider", { name: "回放位置" }).inputValue()) < 1, "back 30 returns to start");
  await player.getByRole("button", { name: "播放课程" }).click();
  try {
    await player.getByRole("button", { name: "暂停回放" }).waitFor({ timeout: 8000 });
  } catch {
    throw new Error(JSON.stringify({ playback: "did-not-start", playerText: await player.innerText(), mediaResponses,
      media: await player.locator("audio").evaluate((audio) => ({ readyState: audio.readyState, networkState: audio.networkState, paused: audio.paused, seeking: audio.seeking, errorCode: audio.error?.code ?? null, currentTime: audio.currentTime, duration: audio.duration, buffered: audio.buffered.length ? [audio.buffered.start(0), audio.buffered.end(0)] : [], events: window.__mediaEvents })) }));
  }
  await player.getByRole("button", { name: "暂停回放" }).click();
  assert.equal((await request(`/api/v1/projects/${project.id}/recording/status?device_id=live-capture-browser`)).lease, null);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.getByRole("button", { name: "进入复习" }).click();
  await page.locator(".document-panel").waitFor();
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ result: "PASS", input: captureWav ? "sealed-course-audio" : "synthetic-microphone", width: 390, durableAcks: ackCommitIds.length, stopped: true, back30Playback: true, reviewEntry: true, pageErrors: errors.length }));
} finally {
  if (heartbeat) clearInterval(heartbeat);
  if (browser) await browser.close();
  core.kill();
}
