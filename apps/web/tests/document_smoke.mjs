// Real-browser regression with synthetic course facts only; no private audio or account data.
import { chromium } from "@playwright/test";

const base = process.env.AIALRA_BROWSER_BASE_URL || "http://127.0.0.1:5173";
const screenshotDir = process.env.AIALRA_BROWSER_SCREENSHOT_DIR;
const now = "2026-09-13T00:00:00Z";
const project = { id: "project_synthetic", owner_subject: "local-user", title: "Synthetic course", source_language: "en", target_language: "zh-CN", version: 1, created_at: now, updated_at: now };
const session = { id: "session_synthetic", title: "Synthetic lecture", source_language: "en", target_language: "zh-CN", privacy_mode: "local_only", consent_confirmed: true, demo_mode: false, state: "completed", created_at: now, updated_at: now };
const second = { ...session, id: "session_second", title: "Synthetic second lecture" };
let archived = false;
const snapshot = () => ({ folders: [], projects: [project], project_placements: [{ project_id: project.id, folder_id: null, sort_order: 0, archived_at: null, updated_at: now }],
  sessions: [session, second], session_projects: { [session.id]: project.id, [second.id]: project.id },
  session_metadata: [session, second].map((item) => ({ session_id: item.id, pinned: false, sort_order: 0, archived_at: item.id === session.id && archived ? now : null, updated_at: now })),
  trash: [], preference: null });
let serial = 0;
const fact = (event_type, payload) => ({ schema_version: "1.0.0", event_id: `synthetic-${++serial}`, session_id: session.id, source_id: "fixture", sequence: serial,
  event_type, captured_at_monotonic_ns: serial, captured_at_wall: now, ingested_at: now, correlation_id: "fixture", causation_id: null, payload, content_hash: `sha256:${"0".repeat(64)}` });
const events = [
  fact("segment.finalized", { segment_id: "s1", text: "Fault models explain test strategies.", audio_start_ms: 1000, audio_end_ms: 2000, display_mode: "internal_fragment" }),
  fact("segment.finalized", { segment_id: "s2", text: "Coverage describes detected faults.", audio_start_ms: 20_000, audio_end_ms: 21_000, display_mode: "internal_fragment" }),
  fact("paragraph.finalized", { paragraph_id: "p1", segment_ids: ["s1"], text: "Fault models explain test strategies." }),
  fact("paragraph.finalized", { paragraph_id: "p2", segment_ids: ["s2"], text: "Coverage describes detected faults." }),
  fact("translation.finalized", { paragraph_id: "p1", text: "故障模型解释测试策略。" }),
  fact("translation.finalized", { paragraph_id: "p2", text: "覆盖率描述检测到的故障。" }),
  fact("content.group.created", { paragraph_ids: ["p1"], reason: "topic_change" }),
  fact("content.group.created", { paragraph_ids: ["p2"], reason: "recording_stopped" }),
  fact("explanation.card.created", { card_id: "c1", result: { paragraph_summary: "故障模型帮助确定测试目标。", evidence_segment_ids: ["p1"], terms: [{ term: "故障模型", explanation: "对电路失效方式的抽象描述。" }] } }),
  fact("explanation.card.created", { card_id: "c2", result: { paragraph_summary: "覆盖率衡量已检测到的故障。", evidence_segment_ids: ["p2"], terms: [] } }),
  fact("session.summary.created", { summary_id: "summary", result: { overview: "本课程讨论故障模型与覆盖率。", key_points: ["测试目标与覆盖率"], terminology: [] } }),
];
const stressEvents = Array.from({ length: 207 }, (_, index) => [
  fact("segment.finalized", { segment_id: `long-s${index}`, text: `Synthetic source ${index}`, audio_start_ms: 1000 + index * 1000, audio_end_ms: 1900 + index * 1000, display_mode: "internal_fragment" }),
  fact("paragraph.finalized", { paragraph_id: `long-p${index}`, segment_ids: [`long-s${index}`], text: `Synthetic source ${index}` }),
  fact("translation.finalized", { paragraph_id: `long-p${index}`, text: `合成译文 ${index}` }),
]).flat();
const stressPositions = Array.from({ length: 5551 }, (_, index) => ({
  captured_at_ms: 1000 + index * 1000, duration_ms: 1000,
  playback_start_ms: index * 1000, playback_end_ms: (index + 1) * 1000,
}));

const pcm = Buffer.alloc(64_000);
const wav = Buffer.alloc(44 + pcm.length);
wav.write("RIFF", 0); wav.writeUInt32LE(wav.length - 8, 4); wav.write("WAVEfmt ", 8);
wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
wav.writeUInt32LE(16_000, 24); wav.writeUInt32LE(32_000, 28);
wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34); wav.write("data", 36);
wav.writeUInt32LE(pcm.length, 40); pcm.copy(wav, 44);

const browser = await chromium.launch({ channel: process.env.AIALRA_BROWSER_CHANNEL || "msedge", headless: true });
try {
  for (const [width, stress] of [[1440, false], [390, false], [1440, true]]) {
    archived = false;
    const page = await browser.newPage({ viewport: { width, height: 900 }, acceptDownloads: true });
    page.on("dialog", (dialog) => void dialog.accept());
    await page.route("**/api/v1/**", async (route) => {
      const url = new URL(route.request().url());
      const path = url.pathname;
      if (path.endsWith("/stream")) return route.fulfill({ status: 200, contentType: "text/event-stream", body: "" });
      if (path.endsWith("/workspace")) return route.fulfill({ json: snapshot() });
      if (path.endsWith("/document-snapshot")) return route.fulfill({ json: { events: stress ? stressEvents : events, cursor: events.at(-1).event_id } });
      if (path.endsWith("/audio/index")) return route.fulfill({ json: stress ? { duration_ms: 5551000, positions: stressPositions } : { duration_ms: 2000, positions: [
        { captured_at_ms: 1000, duration_ms: 1000, playback_start_ms: 0, playback_end_ms: 1000 },
        { captured_at_ms: 20_000, duration_ms: 1000, playback_start_ms: 1000, playback_end_ms: 2000 },
      ] } });
      if (path.endsWith("/audio")) {
        const range = route.request().headers().range;
        const start = range ? Number(range.match(/bytes=(\d+)/)?.[1] ?? 0) : 0;
        const end = Math.min(wav.length - 1, start + 1024 * 1024 - 1);
        return route.fulfill({ status: 206, headers: { "content-type": "audio/wav", "accept-ranges": "bytes", "content-range": `bytes ${start}-${end}/${wav.length}` }, body: wav.subarray(start, end + 1) });
      }
      if (path.endsWith("/recording/status")) return route.fulfill({ json: { project_id: project.id, server_time: now, lease: null, admission: { allowed: true, reason: "ok", retry_after_seconds: 0, max_asr_backlog_seconds: 15 }, sessions: [] } });
      if (path.endsWith("/readweave/preview")) return route.fulfill({ json: { sessions: [] } });
      if (path.endsWith("/readweave")) return route.fulfill({ json: { configured: false, queued: 0, syncing: 0, completed: 0, conflicts: 0, updated_at: null, note_url: null } });
      if (path.endsWith("/health")) return route.fulfill({ json: { status: "ok", service: "test", version: "synthetic", build_id: "synthetic", deployment_mode: "local", processing_location: "local", worker: null, model_queue: { queued: 0, leased: 0, completed: 0, failed: 0 } } });
      if (path.includes("/workspace/trash/") && route.request().method() === "POST") { archived = true; return route.fulfill({ json: { accepted: true } }); }
      if (route.request().method() !== "GET") return route.fulfill({ json: { accepted: true } });
      return route.fulfill({ status: 404, json: { code: "not_found" } });
    });
    const started = Date.now();
    await page.goto(`${base}/app/projects/${project.id}/sessions/${session.id}/notes/transcript`);
    await page.getByRole("heading", { name: session.title }).waitFor();
    await page.getByRole("region", { name: "整节课程录音回放" }).waitFor();
    await page.getByTestId("course-paragraph").first().waitFor();
    if (stress) {
      const elapsed = Date.now() - started;
      if (elapsed > 5000) throw new Error(`long course took ${elapsed} ms to become readable`);
      console.log(`browser long-course synthetic 621 events / 5551 audio positions: readable in ${elapsed} ms`);
      await page.close();
      continue;
    }
    await page.getByRole("searchbox", { name: "搜索课程原文和译文" }).fill("覆盖率");
    await page.getByText("找到 1 条匹配内容").waitFor();
    await page.getByRole("searchbox", { name: "搜索课程原文和译文" }).fill("");
    await page.getByRole("button", { name: "定位回听" }).first().click();
    const downloadPromise = page.waitForEvent("download");
    await page.getByRole("button", { name: "导出课程笔记" }).click();
    const download = await downloadPromise;
    if (!download.suggestedFilename().endsWith(".md")) throw new Error("Markdown export is missing");
    if (width <= 390) await page.getByRole("button", { name: "打开课程树" }).click();
    await page.getByRole("button", { name: "课程概览" }).click();
    await page.getByRole("region", { name: "课程章节与观点树" }).waitFor();
    if (await page.locator(".course-outline > details").count() !== 2) throw new Error("topic chapters were not separated");
    if (screenshotDir) await page.screenshot({ path: `${screenshotDir}/synthetic-course-${width}.png`, fullPage: true });
    await page.getByRole("button", { name: "管理课程 Synthetic lecture" }).click();
    await page.getByRole("menuitem", { name: "移入回收站" }).click();
    await page.waitForURL(`${base}/app/projects/${project.id}`);
    if (await page.getByRole("button", { name: "折叠Synthetic course" }).getAttribute("aria-expanded") !== "true") {
      throw new Error("trashing a course collapsed its project tree");
    }
    await page.close();
    console.log(`browser ${width}px: player, search, export, chapters, trash tree state passed`);
  }
} finally {
  await browser.close();
}
