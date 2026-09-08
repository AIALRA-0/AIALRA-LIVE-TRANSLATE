// Real Chromium + compiled Core, synthetic identity/audio only. Never production data.
import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes, createHash } from "node:crypto";
import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "../../..");
const data = await mkdtemp(path.join(tmpdir(), "aialra-reliability-"));
const evidence = process.env.AIALRA_EVIDENCE_DIR || path.join(data, "evidence");
await mkdir(evidence, { recursive: true });
const base = "http://127.0.0.1:18790";
const token = randomBytes(32).toString("hex");
const headers = {"content-type":"application/json", "X-aialra-auth-proxy":"1", "X-authentik-uid":"reliability-test"};
const core = spawn(path.join(root, "target/debug/aialra-core-server.exe"), [], {
  cwd: root, windowsHide: true, stdio: "ignore", env: {...process.env,
    AIALRA_BIND:"127.0.0.1:18790", AIALRA_DATA_DIR:data, AIALRA_DEPLOYMENT_MODE:"server",
    AIALRA_ALLOWED_ORIGINS:base, AIALRA_WORKER_TOKEN_SHA256:createHash("sha256").update(token).digest("hex"),
  },
});
let browser;
let heartbeat;
const checks = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function request(url, method="GET", body, customHeaders=headers) {
  const response = await fetch(`${base}${url}`, {method, headers:customHeaders, ...(body ? {body:JSON.stringify(body)} : {})});
  assert.ok(response.ok, `request ${method} failed with ${response.status}`);
  return response.json();
}
try {
  let ready = false;
  for (let i=0;i<40;i++) { try { await request("/api/v1/health"); ready=true; break; } catch { await sleep(250); } }
  assert.ok(ready, "candidate core started");
  const beat = () => request("/internal/v1/workers/heartbeat", "POST", {
    worker_id:"reliability-worker", capabilities:["asr"], active_job_id:null,
    model_metadata:{status:"ok",asr_available:true,asr_provider:"test-only@cuda"},
  }, {...headers, authorization:`Bearer ${token}`});
  await beat(); heartbeat = setInterval(() => void beat(), 10000);
  const project = await request("/api/v1/projects", "POST", {title:"Browser reliability test",source_language:"auto",target_language:"zh-CN"});
  const create = () => request(`/api/v1/projects/${project.id}/sessions`, "POST", {title:"Synthetic course",consent_confirmed:true,device_id:"reliability-browser"});
  const historical = await create();
  const previous = await request(`/api/v1/projects/${project.id}/sessions/${historical.id}/recording/acquire`,"POST",{device_id:"old-device-test"});
  await request(`/api/v1/projects/${project.id}/sessions/${historical.id}/recording/stop`,"POST",{device_id:"old-device-test",lease_token:previous.lease_token});
  const session = await create();
  browser = await chromium.launch({headless:true, channel:process.env.AIALRA_BROWSER_CHANNEL || "msedge", args:["--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream"]});
  const context = await browser.newContext({extraHTTPHeaders:headers, viewport:{width:1360,height:960}, permissions:["microphone"]});
  await context.addInitScript(() => {
    window.__captureProbe = {calls:0, tracks:[], wakes:0, releases:0};
    const original = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia = async (...args) => {
      window.__captureProbe.calls++;
      const stream = await original(...args); window.__captureProbe.tracks.push(...stream.getTracks()); return stream;
    };
    Object.defineProperty(navigator, "wakeLock", {configurable:true,value:{request:async () => {
      window.__captureProbe.wakes++;
      const sentinel = new EventTarget(); sentinel.release = async () => {window.__captureProbe.releases++; sentinel.dispatchEvent(new Event("release"));}; return sentinel;
    }}});
  });
  const page = await context.newPage();
  const pageErrors=[]; page.on("pageerror", (error) => pageErrors.push(error.name));
  const wasm=[]; page.on("response", (response) => { if(response.url().includes(".wasm")) wasm.push(response.status()); });
  const acks=[]; page.on("websocket", (ws) => ws.on("framereceived", ({payload}) => {
    try { const event=JSON.parse(String(payload)); if(event.type==="audio.ack") acks.push(Boolean(event.commit_id)); } catch { /* binary */ }
  }));
  const course = `${base}/app/projects/${project.id}/sessions/${session.id}`;
  await page.goto(course);
  await page.getByRole("button", {name:"开始录音", exact:true}).waitFor();
  await sleep(800);
  assert.equal(await page.evaluate(() => window.__captureProbe.calls),0);
  assert.equal(await page.getByText("其他设备正在录制本项目",{exact:true}).count(),0);
  checks.push("opening requests no microphone and historical lease replay causes no conflict");
  assert.equal(await page.getByRole("combobox",{name:/^降噪/}).inputValue(),"gtcrn");
  assert.ok(await page.evaluate(() => {
    const panel=document.querySelector(".paragraph-insight-panel");
    const gpu=[...document.querySelectorAll(".session-sidebar .side-card")].find(el=>el.querySelector("h3")?.textContent==="本机 GPU");
    return panel && gpu && Boolean(panel.compareDocumentPosition(gpu)&Node.DOCUMENT_POSITION_FOLLOWING);
  }));
  assert.equal(await page.getByText("当前内容组的总结正在生成。",{exact:true}).count(),0);
  checks.push("content group precedes GPU; accumulation is not falsely presented as generation; GTCRN default");
  await page.getByRole("button",{name:"新建项目",exact:true}).click();
  assert.equal(await page.getByRole("menuitem",{name:"新建文件夹",exact:true}).count(),0);
  await page.getByRole("button",{name:"取消",exact:true}).click();
  checks.push("new button is project-only");
  await page.getByRole("button",{name:"测试麦克风",exact:true}).click();
  await page.getByRole("button",{name:"测试中 4 秒",exact:true}).waitFor();
  assert.ok(await page.getByRole("button",{name:"开始录音",exact:true}).isDisabled());
  await page.getByRole("button",{name:"测试麦克风",exact:true}).waitFor({timeout:15000});
  assert.ok(await page.evaluate(() => window.__captureProbe.tracks.every((t) => t.readyState==="ended")));
  checks.push("microphone test blocks overlapping start and releases every track");
  await page.getByRole("button",{name:"开始录音",exact:true}).click();
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).waitFor({timeout:20000});
  await sleep(2500);
  assert.ok(wasm.length>0 && wasm.every((s)=>s===200),"GTCRN WASM loaded locally");
  assert.ok(acks.length>=1 && acks.every(Boolean),"durable ACKs");
  assert.ok(await page.evaluate(() => window.__captureProbe.wakes>=1));
  const stoppedResponse=page.waitForResponse((response)=>response.url().endsWith("/recording/stop") && response.request().method()==="POST");
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).click();
  assert.ok((await stoppedResponse).ok(),"server stop succeeds");
  await page.waitForFunction(() => window.__captureProbe.tracks.every((t)=>t.readyState==="ended") && window.__captureProbe.releases>=1);
  const status=await request(`/api/v1/projects/${project.id}/recording/status?device_id=reliability-browser`);
  assert.equal(status.lease,null);
  checks.push("real browser GTCRN -> PCM -> durable ACK -> stop -> lease and tracks released; wake API lifecycle");
  await page.goto(`${course}/notes/user-notes`);
  const editor=page.getByRole("textbox",{name:"课程笔记"}); await editor.waitFor();
  await editor.fill("Synthetic personal note.");
  await page.getByText("已保存 · 历史版本保留",{exact:true}).waitFor();
  const note=await request(`/api/v1/sessions/${session.id}/notes`); assert.equal(note.text,"Synthetic personal note.");
  const conflict=await fetch(`${base}/api/v1/sessions/${session.id}/notes`,{method:"PUT",headers,body:JSON.stringify({text:"stale edit",base_revision:0})}); assert.equal(conflict.status,409);
  const forbidden=await fetch(`${base}/api/v1/sessions/${session.id}/notes`,{headers:{...headers,"X-authentik-uid":"other-test-owner"}}); assert.equal(forbidden.status,404);
  checks.push("notes autosave, stale revision rejected, other owner denied");
  for (const section of ["overview","transcript","explanations","assets","user-notes"]) {
    await page.goto(`${course}/notes/${section}`); await page.locator(".course-document").waitFor();
  }
  await page.setViewportSize({width:390,height:844});
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth<=window.innerWidth+1));
  await page.screenshot({path:path.join(evidence,"narrow-notes.png"),fullPage:true});
  await page.setViewportSize({width:1360,height:960});
  await page.screenshot({path:path.join(evidence,"desktop-notes.png"),fullPage:true});
  assert.deepEqual(pageErrors,[]);
  checks.push("five modules load; desktop and 390px no horizontal overflow or uncaught page errors");
  await writeFile(path.join(evidence,"browser-validation.json"),JSON.stringify({passed:true,checks,ack_count:acks.length,wasm_responses:wasm,physical_microphone_test:false,real_gpu_inference:false},null,2));
  console.log(JSON.stringify({passed:true,checks,ack_count:acks.length}));
} finally {
  clearInterval(heartbeat); await browser?.close(); core.kill();
}
