// Headless Chrome or Edge uses a network-downloaded WAV as its fake microphone while the app uses its real capture path.
import { chromium } from "@playwright/test";
import { createHash, randomUUID } from "node:crypto";
import { mkdir, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

const baseUrl = process.env.AIALRA_BROWSER_BASE_URL || "http://127.0.0.1:18787";
const apiUrl = `${baseUrl}/api/v1`;
const identity = process.env.AIALRA_TEST_SUBJECT || "browser-dual-device-test";
const fixtureUrl = process.env.AIALRA_AUDIO_FIXTURE_URL;
const fixturePassword = process.env.AIALRA_AUDIO_FIXTURE_PASSWORD;
const fixtureUsername = process.env.AIALRA_AUDIO_FIXTURE_USERNAME || "soak";
const browserChannel = process.env.AIALRA_BROWSER_CHANNEL || "chromium";
const captureSeconds = Number(process.env.AIALRA_BROWSER_CAPTURE_SECONDS || "35");
const offlineSeconds = Number(process.env.AIALRA_BROWSER_OFFLINE_SECONDS || "5");
const screenshotPath = process.env.AIALRA_BROWSER_SCREENSHOT_PATH;

if (!fixtureUrl?.startsWith("https://") || !fixturePassword) {
  throw new Error("controlled HTTPS fixture URL and password are required");
}
if (!/^[a-z0-9_-]{1,32}$/i.test(fixtureUsername)) throw new Error("invalid controlled fixture username");

const fixtureResponse = await fetch(fixtureUrl, {
  headers: { Authorization: `Basic ${Buffer.from(`${fixtureUsername}:${fixturePassword}`).toString("base64")}` },
});
if (!fixtureResponse.ok) throw new Error(`fixture download failed: ${fixtureResponse.status}`);
const fixture = Buffer.from(await fixtureResponse.arrayBuffer());

function repeatedPcmWav(source, seconds) {
  if (source.toString("ascii", 0, 4) !== "RIFF" || source.toString("ascii", 8, 12) !== "WAVE") {
    throw new Error("controlled fixture is not a RIFF/WAVE file");
  }
  let offset = 12;
  let dataHeader = -1;
  let dataStart = -1;
  let dataLength = 0;
  let byteRate = 0;
  let blockAlign = 0;
  while (offset + 8 <= source.length) {
    const chunkId = source.toString("ascii", offset, offset + 4);
    const chunkLength = source.readUInt32LE(offset + 4);
    if (chunkId === "fmt " && chunkLength >= 16) {
      if (source.readUInt16LE(offset + 8) !== 1) throw new Error("controlled fixture must use PCM WAV");
      byteRate = source.readUInt32LE(offset + 16);
      blockAlign = source.readUInt16LE(offset + 20);
    }
    if (chunkId === "data") {
      dataHeader = offset;
      dataStart = offset + 8;
      dataLength = Math.min(chunkLength, source.length - dataStart);
      break;
    }
    offset += 8 + chunkLength + (chunkLength % 2);
  }
  if (dataStart < 0 || !byteRate || !blockAlign || dataLength < blockAlign) {
    throw new Error("controlled fixture has no usable PCM data chunk");
  }
  const targetLength = Math.floor((byteRate * seconds) / blockAlign) * blockAlign;
  const output = Buffer.allocUnsafe(dataStart + targetLength);
  source.copy(output, 0, 0, dataStart);
  output.writeUInt32LE(output.length - 8, 4);
  output.writeUInt32LE(targetLength, dataHeader + 4);
  let written = 0;
  while (written < targetLength) {
    const count = Math.min(dataLength, targetLength - written);
    source.copy(output, dataStart + written, dataStart, dataStart + count);
    written += count;
  }
  return output;
}

const privateFixturePath = path.join(tmpdir(), `aialra-browser-${randomUUID()}.wav`);
await writeFile(privateFixturePath, repeatedPcmWav(fixture, captureSeconds + 120));

const apiHeaders = {
  "content-type": "application/json",
  "X-authentik-uid": identity,
  ...(process.env.AIALRA_TEST_PROXY_MARKER === "true" ? { "X-aialra-Auth-Proxy": "1" } : {}),
};
async function checked(responsePromise) {
  const response = await responsePromise;
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return response.json();
}

async function audioProgress(page) {
  return page.evaluate(async () => new Promise((resolve, reject) => {
    const request = indexedDB.open("aialra-audio-outbox", 2);
    request.onerror = () => reject(request.error ?? new Error("audio outbox unavailable"));
    request.onsuccess = () => {
      const database = request.result;
      const transaction = database.transaction(["metadata", "frames"], "readonly");
      const metadata = transaction.objectStore("metadata").getAll();
      const frames = transaction.objectStore("frames").getAll();
      const complete = () => {
        if (metadata.readyState !== "done" || frames.readyState !== "done") return;
        const nextSequence = metadata.result.reduce(
          (current, item) => Math.max(current, Number(item?.nextSequence ?? 1)),
          1,
        );
        const pendingSequences = frames.result.map((item) => Number(item?.sequence ?? 0));
        const pending = pendingSequences.length;
        const firstPending = pending ? Math.min(...pendingSequences) : nextSequence;
        database.close();
        resolve({ captured: nextSequence - 1, acknowledged: firstPending - 1, pending });
      };
      metadata.onerror = () => reject(metadata.error ?? new Error("audio metadata unavailable"));
      frames.onerror = () => reject(frames.error ?? new Error("audio outbox unavailable"));
      metadata.onsuccess = complete;
      frames.onsuccess = complete;
    };
  }));
}

const project = await checked(fetch(`${apiUrl}/projects`, {
  method: "POST",
  headers: apiHeaders,
  body: JSON.stringify({ title: `${browserChannel} 双设备验证`, source_language: "en", target_language: "zh-CN" }),
}));
const session = await checked(fetch(`${apiUrl}/projects/${project.id}/sessions`, {
  method: "POST",
  headers: apiHeaders,
  body: JSON.stringify({ title: `${browserChannel} 网络音频课程`, consent_confirmed: true, device_id: "browser-test-bootstrap" }),
}));

const launchOptions = {
  headless: true,
  args: [
    "--use-fake-ui-for-media-stream",
    "--use-fake-device-for-media-stream",
    `--use-file-for-fake-audio-capture=${privateFixturePath}`,
  ],
};
if (browserChannel !== "chromium") launchOptions.channel = browserChannel;
const browser = await chromium.launch(launchOptions);
const browserHeaders = {
  "X-authentik-uid": identity,
  ...(process.env.AIALRA_TEST_PROXY_MARKER === "true" ? { "X-Aialra-Auth-Proxy": "1" } : {}),
};
const recorderContext = await browser.newContext({ extraHTTPHeaders: browserHeaders });
const observerContext = await browser.newContext({ extraHTTPHeaders: browserHeaders });
const recorder = await recorderContext.newPage();
const observer = await observerContext.newPage();

async function openSession(page) {
  // Stable deep links are the contract for refreshes and multi-device observers
  // so the test never depends on a transient marketing or setup screen.
  await page.goto(`${baseUrl}/app/projects/${project.id}/sessions/${session.id}`, { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: session.title, exact: true }).waitFor({ timeout: 30_000 });
}

try {
  await Promise.all([openSession(recorder), openSession(observer)]);
  await recorder.getByText("默认使用当前设备的麦克风；只有本机没有麦克风时才需要安卓手机", { exact: true }).waitFor({ timeout: 10_000 });
  if (await recorder.locator("details.device-pairing[open]").count()) throw new Error("Android fallback must be collapsed by default");
  await recorder.getByRole("button", { name: "开始录音", exact: true }).click();
  await recorder.getByText("收音正常，服务器已确认全部音频块").waitFor({ timeout: 30_000 });
  await observer.getByText("另一台设备正在录音，请在录音设备停止").waitFor({ timeout: 30_000 });
  await new Promise((resolve) => setTimeout(resolve, 8_000));
  await recorderContext.setOffline(true);
  await new Promise((resolve) => setTimeout(resolve, offlineSeconds * 1_000));
  await recorderContext.setOffline(false);
  await recorder.reload({ waitUntil: "domcontentloaded" });
  await recorder.getByText("录音租约已恢复，可继续收音").waitFor({ timeout: 20_000 });
  await recorder.getByRole("button", { name: "继续连接收音", exact: true }).click();
  await recorder.getByText("收音正常，服务器已确认全部音频块").waitFor({ timeout: 30_000 });
  await new Promise((resolve) => setTimeout(resolve, 3_000));
  await recorder.reload({ waitUntil: "domcontentloaded" });
  await recorder.getByText("录音租约已恢复，可继续收音").waitFor({ timeout: 20_000 });
  await recorder.getByRole("button", { name: "继续连接收音", exact: true }).click();
  await recorder.getByText("收音正常，服务器已确认全部音频块").waitFor({ timeout: 30_000 });
  const captureDeadline = Date.now() + Math.max(10, captureSeconds - 13) * 1_000;
  let previousCaptured = 0;
  let previousAcknowledged = 0;
  let captureStalls = 0;
  let acknowledgementStalls = 0;
  while (Date.now() < captureDeadline) {
    await new Promise((resolve) => setTimeout(resolve, Math.min(30_000, captureDeadline - Date.now())));
    const progress = await audioProgress(recorder);
    captureStalls = progress.captured > previousCaptured ? 0 : captureStalls + 1;
    acknowledgementStalls = progress.acknowledged > previousAcknowledged ? 0 : acknowledgementStalls + 1;
    previousCaptured = progress.captured;
    previousAcknowledged = progress.acknowledged;
    if (captureStalls >= 3) throw new Error("browser audio stream stopped progressing for 90 seconds");
    if (acknowledgementStalls >= 3) {
      throw new Error(`server audio acknowledgements stopped progressing for 90 seconds with ${progress.pending} cached frames`);
    }
  }
  await recorder.getByRole("button", { name: "停止并完成处理", exact: true }).click();
  await recorder.getByText("录音和模型处理均已完成").waitFor({ timeout: 10 * 60_000 });
  await observer.getByText("已完成", { exact: true }).last().waitFor({ timeout: 60_000 });
  await observer.locator(".readweave-card > p").first().waitFor({ timeout: 30_000 });
  const events = await checked(fetch(`${apiUrl}/sessions/${session.id}/events`, { headers: apiHeaders }));
  const segments = events.filter((event) => event.event_type === "segment.finalized");
  const translations = events.filter((event) => event.event_type === "translation.finalized");
  if (!segments.length || !translations.length) throw new Error("browser capture produced no real final transcript or translation");
  if (screenshotPath) {
    await recorder.getByText("已同步", { exact: true }).waitFor({ timeout: 120_000 });
    const resolvedScreenshotPath = path.resolve(screenshotPath);
    await mkdir(path.dirname(resolvedScreenshotPath), { recursive: true });
    await recorder.screenshot({ path: resolvedScreenshotPath, fullPage: true });
  }
  process.stdout.write(`${JSON.stringify({
    status: "PASS",
    browser: browserChannel,
    dual_device_observer: true,
    readweave_in_page_preview: true,
    refresh_recovery: true,
    fully_acknowledged_refresh_recovery: true,
    offline_seconds: offlineSeconds,
    fixture_sha256: createHash("sha256").update(fixture).digest("hex"),
    stable_segments: segments.length,
    stable_translations: translations.length,
  }, null, 2)}\n`);
} finally {
  await recorderContext.close();
  await observerContext.close();
  await browser.close();
  await unlink(privateFixturePath).catch(() => {});
}
