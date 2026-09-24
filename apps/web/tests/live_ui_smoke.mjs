import { chromium } from "@playwright/test";
import path from "node:path";
import { tmpdir } from "node:os";

const baseUrl = process.env.AIALRA_UI_SMOKE_URL || "http://127.0.0.1:5173";
const now = "2026-09-22T00:00:00Z";
const project = { id: "synthetic-live-project", owner_subject: "synthetic", title: "合成课程",
  source_language: "en", target_language: "zh-CN", version: 1, created_at: now, updated_at: now };
const session = { id: "synthetic-live-session", title: "故障模型与测试",
  source_language: "en", target_language: "zh-CN", privacy_mode: "local_only",
  consent_confirmed: true, demo_mode: false, state: "ready", created_at: now, updated_at: now };
const snapshot = { folders: [], projects: [project], project_placements: [], sessions: [session],
  session_projects: { [session.id]: project.id }, session_metadata: [], trash: [], preference: null };
const event = (type, sequence, payload) => ({
  schema_version: "1", event_id: `synthetic-${sequence}`, session_id: session.id,
  source_id: "synthetic", sequence, event_type: type, captured_at_monotonic_ns: sequence,
  captured_at_wall: now, ingested_at: now, correlation_id: `synthetic-${sequence}`,
  causation_id: null, payload, content_hash: `synthetic-${sequence}`,
});
const events = [
  event("paragraph.finalized", 1, { paragraph_id: "p1", text: "A test checks whether a circuit fails." }),
  event("translation.finalized", 2, { paragraph_id: "p1", source_text: "A test checks whether a circuit fails.", text: "测试检查电路是否发生故障。", provider: "synthetic" }),
  event("paragraph.finalized", 3, { paragraph_id: "p2", text: "The second step needs context." }),
  event("translation.finalized", 4, { paragraph_id: "p2", source_text: "The second step needs context.", text: "第二步需要上下文。", provider: "synthetic" }),
  event("transcript.corrected", 5, { paragraph_id: "p2", text: "The second step needs more context." }),
  event("paragraph.finalized", 6, { paragraph_id: "p3", text: "A fault model defines which failures matter." }),
  event("translation.finalized", 7, { paragraph_id: "p3", source_text: "A fault model defines which failures matter.", text: "故障模型定义哪些故障需要关注。", provider: "synthetic" }),
  event("explanation.card.created", 8, { card_id: "card1", result: {
    evidence_segment_ids: ["p3"], teaching_sections: {
      content_explanation: "它约束测试需要覆盖的故障范围。",
    }, provider: "synthetic",
  } }),
];

const browser = await chromium.launch({ channel: "msedge", headless: true });
try {
  for (const width of [1280, 390]) {
    const page = await browser.newPage({ viewport: { width, height: 900 } });
    let activeEvents = events;
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.route("**/api/v1/**", async (route) => {
      const p = new URL(route.request().url()).pathname;
      if (p.includes("/events") || p.includes("/updates")) {
        await route.fulfill({ status: 200, contentType: "text/event-stream", body: "" });
        return;
      }
      let body = {};
      if (p === "/api/v1/workspace") body = snapshot;
      else if (p.endsWith("/document-snapshot")) body = { events: activeEvents, cursor: null };
      else if (p.endsWith("/audio/index")) body = { duration_ms: 45000, positions: [] };
      else if (p.endsWith("/health")) body = { status: "ok", service: "synthetic", version: "test",
        build_id: "synthetic", deployment_mode: "local", processing_location: "local", worker: null,
        model_queue: { queued: 0, leased: 0, completed: 0, failed: 0 } };
      else if (p.endsWith("/recording/status")) body = { project_id: project.id, lease: null,
        admission: { allowed: true }, sessions: [] };
      else if (p.endsWith("/ai-policy")) body = { cloud_enabled: false, allowed_modalities: [], route_available: false };
      else if (p.endsWith("/readweave/status")) body = { configured: false, conflicts: 0, syncing: 0, queued: 0 };
      await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
    });
    await page.goto(`${baseUrl}/app/projects/${project.id}/sessions/${session.id}`, { waitUntil: "domcontentloaded" });
    const live = page.getByTestId("live-reading");
    await live.waitFor({ timeout: 10000 });
    const cards = live.getByTestId("live-caption");
    await cards.first().waitFor();
    await cards.first().getByText("故障模型定义哪些故障需要关注。").waitFor();
    const firstText = await cards.first().textContent();
    const chineseFirst = firstText.indexOf("故障模型定义") < firstText.indexOf("A fault model");
    const teaching = await live.locator(".live-teaching").count();
    const stale = await live.locator(".live-uncertainty").count();
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
    const screenshot = path.join(tmpdir(), `aialra-live-phase2p-${width}.png`);
    await page.screenshot({ path: screenshot });
    if ((await cards.count()) !== 3 || !chineseFirst || teaching !== 1 || stale !== 1 || overflow || errors.length) {
      throw new Error(JSON.stringify({ width, cards: await cards.count(), firstText, chineseFirst, teaching, stale, overflow, errors }));
    }
    await live.getByRole("button", { name: /待核实/ }).click();
    await live.locator(".live-uncertainty").waitFor();
    await live.getByRole("button", { name: "返回最新" }).click();
    await live.locator(".live-teaching summary").click();
    if (!(await live.getByText("它约束测试需要覆盖的故障范围。").isVisible())) throw new Error("teaching did not open");
    await page.goto(`${baseUrl}/app/projects/${project.id}/sessions/${session.id}/notes/transcript`, { waitUntil: "domcontentloaded" });
    await page.locator(".document-panel").waitFor();
    await page.getByRole("button", { name: "返回听课" }).click();
    await page.getByTestId("live-reading").waitFor();
    activeEvents = events.filter((item) => item.event_type !== "explanation.card.created");
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.getByTestId("live-reading").waitFor();
    if (await page.locator(".live-teaching").count()) throw new Error("zero teaching leaves a visible panel");
    session.state = "completed";
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.locator(".document-panel").waitFor();
    await page.getByRole("button", { name: "返回听课" }).click();
    await page.getByRole("button", { name: "进入复习" }).click();
    await page.locator(".document-panel").waitFor();
    session.state = "ready";
    console.log(JSON.stringify({ width, cards: 3, chineseFirst, teaching, stale, overflow, errors: errors.length, screenshot }));
    await page.close();
  }
} finally {
  await browser.close();
}
