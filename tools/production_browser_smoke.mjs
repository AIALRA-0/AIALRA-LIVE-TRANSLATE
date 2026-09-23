import { createRequire } from "node:module";

const requireFromWeb = createRequire(new URL("../apps/web/package.json", import.meta.url));
const { chromium, request } = requireFromWeb("@playwright/test");

const TOTAL_TIMEOUT_MS = 45_000;
const OPERATION_TIMEOUT_MS = 7_000;
const SEEK_TOLERANCE_SECONDS = 0.75;
const ALLOWED_HOSTS = new Set(["127.0.0.1", "localhost", "::1", "[::1]"]);

const failureCodes = {
  configuration: 1,
  tunnel: 2,
  core: 3,
  course: 4,
  audioIndex: 5,
  browser: 6,
  seek: 7,
  range: 8,
  layout: 9,
  timeout: 10,
};

let stage = failureCodes.configuration;
let browser;
let context;
let apiContext;
let blockedWriteRequests = 0;
let watchdog;

const metrics = {
  audio_duration_s: 0,
  seek_points: 0,
  seek_max_error_s: 0,
  seek_max_drift_s: 0,
  range_status: 0,
  content_range_present: 0,
  layout_widths_checked: 0,
  max_overflow_px: 0,
  blocked_write_requests: 0,
  blocked_cross_origin_requests: 0,
};

function requiredEnvironment(name) {
  const value = process.env[name]?.trim();
  if (!value) throw new Error("configuration");
  return value;
}

function safeIdentifier(value) {
  return /^[A-Za-z0-9_-]{1,128}$/.test(value);
}

function assert(condition, code) {
  if (!condition) throw new Error(String(code));
}

async function readJson(response) {
  assert(response.ok(), stage);
  return response.json();
}

async function cleanup() {
  clearTimeout(watchdog);
  const settleWithin = async (operation) => {
    if (!operation) return;
    let timer;
    await Promise.race([
      Promise.resolve(operation).catch(() => undefined),
      new Promise((resolve) => { timer = setTimeout(resolve, 2_000); }),
    ]);
    clearTimeout(timer);
  };
  await Promise.all([
    settleWithin(context?.close()),
    settleWithin(browser?.close()),
    settleWithin(apiContext?.dispose()),
  ]);
}

async function run() {
  const baseUrlValue = requiredEnvironment("AIALRA_BROWSER_BASE_URL");
  const subject = requiredEnvironment("AIALRA_TEST_SUBJECT");
  const proxyMarker = requiredEnvironment("AIALRA_TEST_PROXY_MARKER");
  const projectId = requiredEnvironment("AIALRA_TEST_PROJECT_ID");
  const sessionId = requiredEnvironment("AIALRA_TEST_SESSION_ID");

  assert(proxyMarker === "true", failureCodes.configuration);
  assert(safeIdentifier(projectId) && safeIdentifier(sessionId), failureCodes.configuration);

  let baseUrl;
  try {
    baseUrl = new URL(baseUrlValue);
  } catch {
    throw new Error(String(failureCodes.tunnel));
  }
  assert(["http:", "https:"].includes(baseUrl.protocol), failureCodes.tunnel);
  assert(ALLOWED_HOSTS.has(baseUrl.hostname), failureCodes.tunnel);
  assert(baseUrl.pathname === "/" && !baseUrl.search && !baseUrl.hash && !baseUrl.username && !baseUrl.password, failureCodes.tunnel);
  const origin = baseUrl.origin;

  const headers = {
    "X-authentik-uid": subject,
    "X-aialra-auth-proxy": "1",
  };
  apiContext = await request.newContext({
    extraHTTPHeaders: headers,
    timeout: OPERATION_TIMEOUT_MS,
    maxRedirects: 0,
  });

  stage = failureCodes.core;
  const health = await readJson(await apiContext.get(`${origin}/api/v1/health`));
  assert(health.status === "ok" && health.deployment_mode === "server", failureCodes.core);

  stage = failureCodes.course;
  const sessions = await readJson(await apiContext.get(`${origin}/api/v1/projects/${projectId}/sessions`));
  const selectedSession = Array.isArray(sessions)
    ? sessions.find((item) => item && item.id === sessionId)
    : undefined;
  assert(selectedSession?.state === "completed", failureCodes.course);

  stage = failureCodes.audioIndex;
  const audioIndex = await readJson(await apiContext.get(`${origin}/api/v1/sessions/${sessionId}/audio/index`));
  const durationSeconds = Number(audioIndex?.duration_ms) / 1000;
  assert(Number.isFinite(durationSeconds) && durationSeconds > 30, failureCodes.audioIndex);
  metrics.audio_duration_s = Math.round(durationSeconds * 100) / 100;

  stage = failureCodes.range;
  const rangeResponse = await apiContext.get(
    `${origin}/api/v1/sessions/${sessionId}/audio/segment?start_ms=0&duration_ms=45000`,
    { headers: { ...headers, Range: "bytes=0-1" }, maxRedirects: 0 },
  );
  const contentRange = rangeResponse.headers()["content-range"] ?? "";
  assert(rangeResponse.status() === 206, failureCodes.range);
  assert(/^bytes 0-1\/[1-9]\d*$/.test(contentRange), failureCodes.range);
  metrics.range_status = rangeResponse.status();
  metrics.content_range_present = 1;

  stage = failureCodes.browser;
  browser = await chromium.launch({ headless: true, timeout: OPERATION_TIMEOUT_MS });
  context = await browser.newContext({
    extraHTTPHeaders: headers,
    viewport: { width: 1440, height: 870 },
  });
  context.setDefaultTimeout(OPERATION_TIMEOUT_MS);
  context.setDefaultNavigationTimeout(OPERATION_TIMEOUT_MS);
  await context.route("**/*", async (route) => {
    let requestOrigin;
    try {
      requestOrigin = new URL(route.request().url()).origin;
    } catch {
      metrics.blocked_cross_origin_requests += 1;
      await route.abort("blockedbyclient");
      return;
    }
    if (requestOrigin !== origin) {
      metrics.blocked_cross_origin_requests += 1;
      await route.abort("blockedbyclient");
      return;
    }
    const method = route.request().method().toUpperCase();
    if (method === "GET" || method === "HEAD") {
      await route.continue();
      return;
    }
    blockedWriteRequests += 1;
    await route.abort("blockedbyclient");
  });

  const page = await context.newPage();
  await page.goto(
    `${origin}/app/projects/${projectId}/sessions/${sessionId}/notes/transcript`,
    { waitUntil: "domcontentloaded" },
  );
  const slider = page.getByRole("slider", { name: "回放位置" });
  const audio = page.locator('audio[aria-label="课程录音"]');
  await slider.waitFor({ state: "visible" });
  await audio.waitFor({ state: "attached" });
  await page.waitForFunction(() => {
    const media = document.querySelector('audio[aria-label="课程录音"]');
    return media instanceof HTMLAudioElement
      && media.readyState >= HTMLMediaElement.HAVE_METADATA
      && Number.isFinite(media.duration)
      && Number.isFinite(media.currentTime);
  }, undefined, { timeout: OPERATION_TIMEOUT_MS });

  const sliderMaximum = Number(await slider.getAttribute("max"));
  assert(Number.isFinite(sliderMaximum) && Math.abs(sliderMaximum - durationSeconds) <= 0.2, failureCodes.audioIndex);

  stage = failureCodes.seek;
  const seekTo = async (targetSeconds) => {
    await slider.evaluate((element, target) => {
      const input = element;
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      if (!setter) throw new Error("slider");
      setter.call(input, String(target));
      input.dispatchEvent(new Event("input", { bubbles: true }));
      input.dispatchEvent(new Event("change", { bubbles: true }));
    }, targetSeconds);

    await page.waitForFunction(({ target, tolerance }) => {
      const range = document.querySelector('input[aria-label="回放位置"]');
      const media = document.querySelector('audio[aria-label="课程录音"]');
      return range instanceof HTMLInputElement
        && media instanceof HTMLAudioElement
        && Math.abs(Number(range.value) - target) <= tolerance
        && Math.abs(media.currentTime - target) <= tolerance;
    }, { target: targetSeconds, tolerance: SEEK_TOLERANCE_SECONDS }, { timeout: OPERATION_TIMEOUT_MS });

    const samples = [];
    for (let sampleIndex = 0; sampleIndex < 4; sampleIndex += 1) {
      const sample = await page.evaluate(() => {
        const range = document.querySelector('input[aria-label="回放位置"]');
        const media = document.querySelector('audio[aria-label="课程录音"]');
        return {
          slider: range instanceof HTMLInputElement ? Number(range.value) : Number.NaN,
          currentTime: media instanceof HTMLAudioElement ? media.currentTime : Number.NaN,
          paused: media instanceof HTMLAudioElement && media.paused,
        };
      });
      assert(Number.isFinite(sample.slider) && Number.isFinite(sample.currentTime) && sample.paused, failureCodes.seek);
      samples.push(sample);
      if (sampleIndex < 3) await page.waitForTimeout(100);
    }
    for (const sample of samples) {
      const error = Math.max(Math.abs(sample.slider - targetSeconds), Math.abs(sample.currentTime - targetSeconds));
      metrics.seek_max_error_s = Math.max(metrics.seek_max_error_s, error);
      assert(error <= SEEK_TOLERANCE_SECONDS, failureCodes.seek);
    }
    const mediaTimes = samples.map((sample) => sample.currentTime);
    const drift = Math.max(...mediaTimes) - Math.min(...mediaTimes);
    metrics.seek_max_drift_s = Math.max(metrics.seek_max_drift_s, drift);
    assert(drift <= SEEK_TOLERANCE_SECONDS, failureCodes.seek);
    metrics.seek_points += 1;
  };

  for (const target of [0, 15, 30]) await seekTo(target);

  stage = failureCodes.layout;
  for (const width of [1440, 870, 390]) {
    const height = width === 390 ? 844 : 870;
    await page.setViewportSize({ width, height });
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => resolve())));
    const layout = await page.evaluate(() => {
      const root = document.documentElement;
      const body = document.body;
      const visible = (selector) => {
        const element = document.querySelector(selector);
        if (!(element instanceof HTMLElement)) return false;
        const style = getComputedStyle(element);
        const rect = element.getBoundingClientRect();
        return style.display !== "none"
          && style.visibility !== "hidden"
          && rect.width > 0
          && rect.height > 0
          && rect.left >= -1
          && rect.right <= window.innerWidth + 1;
      };
      return {
        width: window.innerWidth,
        overflow: Math.max(0, root.scrollWidth - root.clientWidth, body.scrollWidth - root.clientWidth),
        visible: visible(".session-player")
          && visible(".document-panel")
          && visible(".course-document"),
      };
    });
    assert(layout.width === width && layout.visible && layout.overflow <= 1, failureCodes.layout);
    metrics.layout_widths_checked += 1;
    metrics.max_overflow_px = Math.max(metrics.max_overflow_px, layout.overflow);
  }
  assert(metrics.blocked_cross_origin_requests === 0, failureCodes.browser);

  metrics.seek_max_error_s = Math.round(metrics.seek_max_error_s * 1000) / 1000;
  metrics.seek_max_drift_s = Math.round(metrics.seek_max_drift_s * 1000) / 1000;
  metrics.max_overflow_px = Math.round(metrics.max_overflow_px * 100) / 100;
  metrics.blocked_write_requests = blockedWriteRequests;
}

try {
  const totalTimeout = new Promise((_, reject) => {
    watchdog = setTimeout(() => reject(new Error(String(failureCodes.timeout))), TOTAL_TIMEOUT_MS);
  });
  await Promise.race([run(), totalTimeout]);
  process.stdout.write(`${JSON.stringify({ status: "PASS", ...metrics })}\n`);
} catch (error) {
  const requestedCode = Number(error?.message);
  const failureCode = Number.isInteger(requestedCode) && requestedCode > 0
    ? requestedCode
    : stage;
  process.stdout.write(`${JSON.stringify({ status: "FAIL", failure_code: failureCode })}\n`);
  process.exitCode = 1;
} finally {
  await cleanup();
}
