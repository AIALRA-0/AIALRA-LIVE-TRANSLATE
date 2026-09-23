// Browser-only synthetic fixture. It never opens a real course or captures audio.
import { chromium } from "@playwright/test";
import path from "node:path";
import { tmpdir } from "node:os";

const baseUrl = process.env.AIALRA_UI_SMOKE_URL || "http://127.0.0.1:5173";
const now = "2026-09-22T00:00:00Z";
const project = {
  id: "synthetic-project", owner_subject: "synthetic", title: "合成电路课程",
  source_language: "en", target_language: "zh-CN", version: 1,
  created_at: now, updated_at: now,
};
const session = {
  id: "synthetic-session", title: "故障模型与测试", source_language: "en",
  target_language: "zh-CN", privacy_mode: "local_only", consent_confirmed: true,
  demo_mode: false, state: "completed", created_at: now, updated_at: now,
};
const snapshot = {
  folders: [], projects: [project], project_placements: [], sessions: [session],
  session_projects: { [session.id]: project.id }, session_metadata: [], trash: [],
  preference: null,
};
const paragraph = {
  schema_version: "1", event_id: "synthetic-event", session_id: session.id,
  source_id: "synthetic", sequence: 1, event_type: "paragraph.finalized",
  captured_at_monotonic_ns: 1, captured_at_wall: now, ingested_at: now,
  correlation_id: "synthetic-event", causation_id: null,
  payload: { paragraph_id: "synthetic-paragraph", text: "A fault model limits which failures a test can reveal." },
  content_hash: "synthetic-event",
};

const browser = await chromium.launch({ headless: true });
try {
  for (const width of [1280, 390]) {
    const page = await browser.newPage({ viewport: { width, height: 900 } });
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.route("**/api/v1/**", async (route) => {
      const url = new URL(route.request().url());
      const p = url.pathname;
      if (p.includes("/events") || p.includes("/updates")) {
        await route.fulfill({ status: 200, contentType: "text/event-stream", body: "" });
        return;
      }
      let body = {};
      if (p === "/api/v1/workspace") body = snapshot;
      else if (p.endsWith("/document-snapshot")) body = { events: [paragraph], cursor: null };
      else if (p.endsWith("/audio/index")) body = { duration_ms: 0, positions: [] };
      else if (p.endsWith("/health")) body = {
        status: "ok", service: "synthetic", version: "test", build_id: "synthetic",
        deployment_mode: "local", processing_location: "local", worker: null,
        model_queue: { queued: 0, leased: 0, completed: 0, failed: 0 },
      };
      else if (p.endsWith("/recording/status")) body = { project_id: project.id, lease: null, admission: { allowed: true }, sessions: [] };
      else if (p.endsWith("/ai-policy")) body = { cloud_enabled: false, allowed_modalities: [], route_available: false };
      else if (p.endsWith("/readweave/status")) body = { configured: false, conflicts: 0, syncing: 0, queued: 0 };
      await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
    });
    await page.goto(`${baseUrl}/app/projects/${project.id}/sessions/${session.id}`, {
      waitUntil: "domcontentloaded",
    });
    await page.getByRole("button", { name: "课程问答" }).waitFor({ timeout: 10000 });
    await page.getByRole("button", { name: "课程问答" }).click();
    const materialPicker = page.locator(".material-composer input[type=file]");
    if (!(await materialPicker.evaluate((input) => input.multiple))) {
      throw new Error(`material picker does not accept a batch at ${width}`);
    }
    await materialPicker.setInputFiles([
      { name: "synthetic-a.txt", mimeType: "text/plain", buffer: Buffer.from("Synthetic A") },
      { name: "synthetic-b.txt", mimeType: "text/plain", buffer: Buffer.from("Synthetic B") },
    ]);
    await page.getByText("确认上传 2 份材料").waitFor();
    await page.getByRole("button", { name: "确认加入资料库" }).click();
    await page.getByText(/已保存 2 份材料/).waitFor();
    const result = await page.evaluate(() => ({
      content: document.body.innerText.length,
      overflow: document.documentElement.scrollWidth > innerWidth,
      errorOverlay: !!document.querySelector("vite-error-overlay,.vite-error-overlay"),
      question: !!document.querySelector(".course-questions textarea"),
    }));
    const screenshot = path.join(tmpdir(), `aialra-ui-smoke-${width}.png`);
    await page.screenshot({ path: screenshot, fullPage: true });
    if (result.content < 100 || result.overflow || result.errorOverlay || !result.question || errors.length) {
      throw new Error(`browser smoke failed at ${width}: ${JSON.stringify({ ...result, errors })}`);
    }
    console.log(JSON.stringify({ width, ...result, errors: errors.length, screenshot }));
    await page.goto(`${baseUrl}/app/projects/${project.id}`, { waitUntil: "domcontentloaded" });
    const cloudToggle = page.getByRole("checkbox", { name: /允许本项目使用云端文本讲解/ });
    await cloudToggle.waitFor({ timeout: 10000 });
    if (!(await cloudToggle.isDisabled())) {
      throw new Error(`closed server route incorrectly permits cloud authorization at ${width}`);
    }
    await page.close();
  }
} finally {
  await browser.close();
}
