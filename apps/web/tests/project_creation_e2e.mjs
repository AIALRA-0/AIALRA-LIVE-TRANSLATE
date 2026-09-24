import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";

const root = path.resolve(import.meta.dirname, "../../..");
const data = await mkdtemp(path.join(tmpdir(), "aialra-project-create-"));
const base = "http://127.0.0.1:18792";
const headers = { "X-aialra-auth-proxy": "1", "X-authentik-uid": "phase2f-project-smoke" };
const core = spawn(path.join(root, "target/debug/aialra-core-server.exe"), [], {
  cwd: root, windowsHide: true, stdio: "ignore",
  env: { ...process.env, AIALRA_BIND: "127.0.0.1:18792", AIALRA_DATA_DIR: data, AIALRA_DEPLOYMENT_MODE: "server", AIALRA_ALLOWED_ORIGINS: base },
});
let browser;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

try {
  let ready = false;
  for (let attempt = 0; attempt < 50; attempt += 1) {
    try { ready = (await fetch(`${base}/api/v1/health`, { headers })).ok; if (ready) break; } catch { /* wait for isolated Core */ }
    await sleep(200);
  }
  assert.ok(ready, "isolated Core started");
  browser = await chromium.launch({ channel: "msedge", headless: true });
  const page = await browser.newPage({ extraHTTPHeaders: headers });
  const errors = [];
  let posted = 0;
  page.on("pageerror", (error) => errors.push(error.message));
  await page.route("**/api/v1/projects", async (route) => {
    if (route.request().method() !== "POST") { await route.continue(); return; }
    posted += 1;
    const response = await route.fetch();
    await sleep(450);
    await route.fulfill({ response });
  });
  await page.goto(`${base}/app`);
  await page.getByRole("button", { name: "新建项目" }).click();
  await page.getByRole("textbox", { name: "名称" }).fill("Phase 2F project smoke");
  await page.getByRole("button", { name: "保存" }).click();
  await page.getByRole("button", { name: "创建中…" }).waitFor();
  assert.equal(await page.getByRole("button", { name: "创建中…" }).isDisabled(), true);
  await page.waitForURL(/\/app\/projects\/project_[a-z0-9]+/);
  assert.equal(posted, 1, "one user action creates one POST");
  const workspace = await (await fetch(`${base}/api/v1/workspace`, { headers })).json();
  assert.equal(workspace.projects.filter((item) => item.title === "Phase 2F project smoke").length, 1);

  const key = randomUUID();
  const request = async () => {
    const response = await fetch(`${base}/api/v1/projects`, { method: "POST", headers: { ...headers, "content-type": "application/json", "Idempotency-Key": key }, body: JSON.stringify({ title: "Duplicate request", source_language: "auto", target_language: "zh-CN" }) });
    assert.equal(response.status, 200);
    return response.json();
  };
  const [first, replay] = await Promise.all([request(), request()]);
  assert.equal(first.id, replay.id, "concurrent retries return the original project");
  const finalWorkspace = await (await fetch(`${base}/api/v1/workspace`, { headers })).json();
  assert.equal(finalWorkspace.projects.filter((item) => item.title === "Duplicate request").length, 1);
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ result: "PASS", browserPosts: posted, projectsFromSameKey: 1, pageErrors: errors.length }));
} finally {
  if (browser) await browser.close();
  core.kill();
}
