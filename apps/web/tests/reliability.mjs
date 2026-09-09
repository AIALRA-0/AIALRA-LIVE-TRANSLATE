// Real Chromium + compiled Core, synthetic identity/audio only. Never production data.
import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
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
const core = spawn(process.env.AIALRA_CORE_BINARY || path.join(root, "target/debug/aialra-core-server.exe"), [], {
  cwd: root, windowsHide: true, stdio: "ignore", env: {...process.env,
    AIALRA_BIND:"127.0.0.1:18790", AIALRA_DATA_DIR:data, AIALRA_DEPLOYMENT_MODE:"server",
    AIALRA_ALLOWED_ORIGINS:base, AIALRA_WORKER_TOKEN_SHA256:createHash("sha256").update(token).digest("hex"),
    // No inference consumer in this browser-only fixture. Capacity rejection
    // has separate Rust tests; outage waits must not exhaust this fixture.
    AIALRA_RECORDING_ADMISSION_MAX_ASR_BACKLOG_SECONDS:"3600",
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
function expireTestLease(sessionId) {
  const result = spawnSync(path.join(root,".venv/Scripts/python.exe"),["-c",
    "import sqlite3,sys; d=sqlite3.connect(sys.argv[1]); d.execute('update recording_leases set expires_at=? where session_id=?',('2000-01-01T00:00:00+00:00',sys.argv[2])); d.commit()",
    path.join(data,"aialra.sqlite"),sessionId],{windowsHide:true,stdio:"ignore"});
  assert.equal(result.status,0);
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
    window.__captureProbe = {calls:0, tracks:[], wakes:0, releases:0, constraints:[]};
    const original = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia = async (...args) => {
      window.__captureProbe.calls++;
      window.__captureProbe.constraints.push(args[0]);
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
  assert.equal(await page.getByRole("combobox",{name:/^降噪/}).inputValue(),"off");
  assert.ok(await page.evaluate(() => {
    const panel=document.querySelector(".paragraph-insight-panel");
    const gpu=[...document.querySelectorAll(".session-sidebar .side-card")].find(el=>el.querySelector("h3")?.textContent==="本机 GPU");
    return panel && gpu && Boolean(panel.compareDocumentPosition(gpu)&Node.DOCUMENT_POSITION_FOLLOWING);
  }));
  assert.equal(await page.getByText("当前内容组的总结正在生成。",{exact:true}).count(),0);
  checks.push("content group precedes GPU; accumulation is not falsely presented as generation; raw input default");
  for (const width of [1360,390]) {
    await page.setViewportSize({width,height:960});
    assert.ok(await page.evaluate(() => {
      const text=document.querySelector(".paragraph-summary-section p");
      if(!text || getComputedStyle(text).whiteSpace!=="pre-wrap") return false;
      const saved=text.textContent;
      text.textContent="First.";
      const single=text.getBoundingClientRect().height;
      text.textContent="First.\n\nSecond.";
      const multiple=text.getBoundingClientRect().height;
      text.textContent=saved;
      return multiple>single*2;
    }));
  }
  await page.setViewportSize({width:1360,height:960});
  checks.push("content-group prose preserves paragraph breaks at desktop and 390px widths");
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
  assert.ok(await page.evaluate(() => window.__captureProbe.constraints.every(({audio}) =>
    audio.echoCancellation === false && audio.noiseSuppression === false && audio.autoGainControl === false)));
  await page.getByRole("combobox",{name:/^降噪/}).selectOption("gtcrn");
  await page.getByRole("button",{name:"测试麦克风",exact:true}).click();
  await page.getByRole("button",{name:"测试麦克风",exact:true}).waitFor({timeout:20000});
  await page.waitForFunction(() => window.__captureProbe.tracks.every((t)=>t.readyState==="ended"));
  checks.push("raw meter does not enable hidden DSP; opt-in GTCRN meter completes and releases input");
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
  // Resume an actually completed course (not an edited database state).
  // Separate device context: the local HTTP/1 fixture otherwise exhausts
  // Chromium's six per-origin connections with both tabs' three SSE streams.
  const observerContext = await browser.newContext({extraHTTPHeaders:headers});
  await observerContext.addInitScript(() => {
    window.__captureProbe = {calls:0};
    const getInput = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia = async (...args) => {
      window.__captureProbe.calls++;
      return getInput(...args);
    };
  });
  const observer = await observerContext.newPage();
  await observer.goto(`${base}/app/projects/${project.id}/sessions/${historical.id}`);
  await observer.getByRole("button",{name:"续录本次课程",exact:true}).waitFor();
  await page.goto(`${base}/app/projects/${project.id}/sessions/${historical.id}`);
  await page.bringToFront();
  await page.getByRole("button",{name:"续录本次课程",exact:true}).waitFor();
  page.once("dialog", dialog => dialog.dismiss());
  await page.getByRole("button",{name:"续录本次课程",exact:true}).click();
  assert.equal(await page.evaluate(() => window.__captureProbe.calls),0);
  assert.equal((await request(`/api/v1/projects/${project.id}/recording/status?device_id=reliability-browser`)).lease,null);
  const resumedResponse = page.waitForResponse(r => r.url().endsWith(`/sessions/${historical.id}/recording/acquire`) && r.request().method()==="POST");
  page.once("dialog", dialog => dialog.accept());
  await page.getByRole("button",{name:"续录本次课程",exact:true}).click();
  const resumedLease = await (await resumedResponse).json();
  assert.equal(resumedLease.session_id,historical.id);
  assert.ok(resumedLease.generation > previous.generation);
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).waitFor();
  await observer.locator(".session-header").getByText("录音中",{exact:true}).waitFor({timeout:15000});
  assert.equal(await observer.evaluate(() => window.__captureProbe.calls),0);
  await observerContext.close();
  const resumedAckCount = acks.length;
  await page.waitForFunction(() => window.__captureProbe.tracks.some(t => t.readyState === "live"));
  for (let i=0;i<40 && acks.length===resumedAckCount;i++) await sleep(250);
  assert.ok(acks.length>resumedAckCount && acks.every(Boolean));
  const oldStop = await fetch(`${base}/api/v1/projects/${project.id}/sessions/${historical.id}/recording/stop`,{method:"POST",headers,body:JSON.stringify({device_id:"old-device-test",lease_token:previous.lease_token})});
  assert.equal(oldStop.status,409);
  await page.screenshot({path:path.join(evidence,"resumed-completed-course.png"),fullPage:true});
  const resumedStop = page.waitForResponse(r => r.url().endsWith(`/sessions/${historical.id}/recording/stop`) && r.request().method()==="POST");
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).click();
  assert.ok((await resumedStop).ok());
  assert.equal((await request(`/api/v1/projects/${project.id}/recording/status?device_id=reliability-browser`)).lease,null);
  checks.push("completed course resumes with confirmation in the same course and fresh generation; cancel never opens mic; PCM ACKs; stale stop cannot end new run");
  const stopRetry = await create();
  const stopRetryUrl = `${base}/app/projects/${project.id}/sessions/${stopRetry.id}`;
  const stopEndpoint = `**/sessions/${stopRetry.id}/recording/stop`;
  let stopOffline = true;
  await page.route(stopEndpoint, route => stopOffline ? route.abort("internetdisconnected") : route.continue());
  await page.goto(stopRetryUrl);
  await page.getByRole("button",{name:"开始录音",exact:true}).click();
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).waitFor();
  await sleep(1500);
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).click();
  await page.getByRole("button",{name:"重试完成停止",exact:true}).waitFor({timeout:40000});
  assert.ok(await page.evaluate(() => window.__captureProbe.tracks.every(t=>t.readyState==="ended")));
  await page.reload();
  expireTestLease(stopRetry.id);
  await page.getByRole("button",{name:"重试完成停止",exact:true}).waitFor();
  assert.equal(await page.evaluate(() => window.__captureProbe.calls),0);
  assert.equal(await page.getByRole("button",{name:"继续连接收音",exact:true}).count(),0);
  assert.equal(await page.getByText("可继续本次课程",{exact:true}).count(),0);
  await page.locator(".header-status").getByText("本机已停麦，待完成停止",{exact:true}).waitFor();
  await page.screenshot({path:path.join(evidence,"stop-pending-desktop.png"),fullPage:true});
  await page.setViewportSize({width:390,height:844});
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth<=window.innerWidth+1));
  await page.screenshot({path:path.join(evidence,"stop-pending-narrow.png"),fullPage:true});
  await page.setViewportSize({width:1360,height:960});
  stopOffline = false;
  await page.getByRole("button",{name:"重试完成停止",exact:true}).click();
  await page.locator(".capture-card .stop-button").waitFor({state:"detached",timeout:40000});
  assert.equal((await request(`/api/v1/projects/${project.id}/recording/status?device_id=reliability-browser`)).lease,null);
  assert.equal(await page.evaluate(() => window.__captureProbe.calls),0);
  checks.push("failed stop POST survives refresh and lease expiry; retry releases lease without reopening microphone");

  const drainRetry = await create();
  let audioOffline = true;
  await page.routeWebSocket(`**/sessions/${drainRetry.id}/sources/*/audio`, ws => {
    if (audioOffline) ws.close(); else ws.connectToServer();
  });
  await page.goto(`${base}/app/projects/${project.id}/sessions/${drainRetry.id}`);
  await page.getByRole("button",{name:"开始录音",exact:true}).click();
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).waitFor();
  await sleep(2100);
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).click();
  await page.waitForFunction(() => window.__captureProbe.tracks.every(t=>t.readyState==="ended"));
  await page.getByRole("button",{name:"重试完成停止",exact:true}).waitFor({timeout:40000});
  const outboxCount = () => page.evaluate(async () => {
    const db = await new Promise((resolve,reject) => { const req=indexedDB.open("aialra-audio-outbox"); req.onsuccess=()=>resolve(req.result); req.onerror=()=>reject(req.error); });
    const count = await new Promise((resolve,reject) => {const req=db.transaction("frames").objectStore("frames").count();req.onsuccess=()=>resolve(req.result);req.onerror=()=>reject(req.error);});
    db.close(); return count;
  });
  const cached = await outboxCount(); assert.ok(cached>=2,"unacknowledged audio retained");
  await sleep(1200);
  assert.equal(await outboxCount(),cached,"no additional audio after stop");
  await page.reload();
  expireTestLease(drainRetry.id);
  await page.getByRole("button",{name:"重试完成停止",exact:true}).waitFor();
  assert.equal(await page.evaluate(() => window.__captureProbe.calls),0);
  audioOffline = false;
  await page.getByRole("button",{name:"重试完成停止",exact:true}).click();
  await page.locator(".capture-card .stop-button").waitFor({state:"detached",timeout:40000});
  assert.equal(await outboxCount(),0,"all persisted audio drained and ACK deletion committed");
  assert.equal(await page.evaluate(() => window.__captureProbe.calls),0);
  assert.equal((await request(`/api/v1/projects/${project.id}/recording/status?device_id=reliability-browser`)).lease,null);
  checks.push("audio outage: physical input stops immediately; cache stops growing; refresh drains original generation without mic; all ACKs and lease released");
  const lostStop = await create();
  const lostStopEndpoint = `**/sessions/${lostStop.id}/recording/stop`;
  let loseResponse = true;
  await page.route(lostStopEndpoint, async route => {
    if (!loseResponse) return route.continue();
    const response = await route.fetch(); assert.ok(response.ok());
    await route.abort("connectionreset");
  });
  await page.goto(`${base}/app/projects/${project.id}/sessions/${lostStop.id}`);
  await page.getByRole("button",{name:"开始录音",exact:true}).click();
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).waitFor();
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).click();
  await page.getByRole("button",{name:"重试完成停止",exact:true}).waitFor({timeout:40000});
  assert.equal((await request(`/api/v1/projects/${project.id}/recording/status?device_id=reliability-browser`)).lease,null);
  await page.reload();
  await page.getByRole("button",{name:"重试完成停止",exact:true}).waitFor();
  loseResponse = false;
  await page.getByRole("button",{name:"重试完成停止",exact:true}).click();
  await page.locator(".capture-card .stop-button").waitFor({state:"detached",timeout:40000});
  assert.equal(await page.evaluate(() => window.__captureProbe.calls),0);
  checks.push("lost successful stop response survives refresh and retries idempotently without microphone or lease reacquisition");

  const takeover = await create();
  const takeoverEndpoint = `**/sessions/${takeover.id}/recording/stop`;
  await page.route(takeoverEndpoint, route => route.abort("internetdisconnected"));
  await page.goto(`${base}/app/projects/${project.id}/sessions/${takeover.id}`);
  await page.getByRole("button",{name:"开始录音",exact:true}).click();
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).waitFor();
  await page.getByRole("button",{name:"停止并完成处理",exact:true}).click();
  await page.getByRole("button",{name:"重试完成停止",exact:true}).waitFor({timeout:40000});
  expireTestLease(takeover.id);
  const replacement = await request(`/api/v1/projects/${project.id}/sessions/${takeover.id}/recording/acquire`,"POST",{device_id:"replacement-test"});
  await page.reload();
  await page.unroute(takeoverEndpoint);
  await page.getByRole("button",{name:"重试完成停止",exact:true}).waitFor();
  const rejectedStop = page.waitForResponse(r=>r.url().endsWith(`/sessions/${takeover.id}/recording/stop`) && r.request().method()==="POST");
  await page.getByRole("button",{name:"重试完成停止",exact:true}).click();
  assert.equal((await rejectedStop).status(),409);
  await page.getByRole("button",{name:"重试完成停止",exact:true}).waitFor();
  assert.equal(await page.evaluate(() => window.__captureProbe.calls),0);
  const held = await request(`/api/v1/projects/${project.id}/recording/status?device_id=replacement-test`);
  assert.equal(held.lease.holder,"self");
  assert.equal(held.lease.generation,replacement.generation);
  await request(`/api/v1/projects/${project.id}/sessions/${takeover.id}/recording/stop`,"POST",{device_id:"replacement-test",lease_token:replacement.lease_token});
  checks.push("stop retry cannot steal or stop a replacement device lease; 409 keeps explicit stop intent and never opens microphone");
  const interrupted = await create();
  await request(`/api/v1/projects/${project.id}/sessions/${interrupted.id}/recording/acquire`,"POST",{device_id:"interrupted-test"});
  const liveSnapshot = await request("/api/v1/workspace");
  assert.equal(liveSnapshot.sessions.find((s)=>s.id===interrupted.id).recording_active,true);
  // Expire a lease only in the newly created local test database, never in a
  // production or user database. Exercise the no-stop-event failure path.
  expireTestLease(interrupted.id);
  const idleSnapshot = await request("/api/v1/workspace");
  const idle = idleSnapshot.sessions.find((s)=>s.id===interrupted.id);
  assert.equal(idle.state,"recording","history remains untouched");
  assert.equal(idle.recording_active,false,"expired lease is not a recording");
  for (const key of ["lease_token","holder_device_id","lease_token_hash"]) assert.equal(key in idle,false);
  const otherSnapshot = await request("/api/v1/workspace","GET",undefined,{...headers,"X-authentik-uid":"other-test-owner"});
  assert.equal(otherSnapshot.sessions.some((s)=>s.id===interrupted.id),false);
  await page.goto(`${base}/app/projects/${project.id}/sessions/${interrupted.id}`);
  await page.locator(".header-status").getByText("录音已中断，可恢复",{exact:true}).waitFor();
  await page.getByRole("button",{name:"确认并继续本次课程",exact:true}).waitFor();
  await page.screenshot({path:path.join(evidence,"interrupted-recording.png"),fullPage:true});
  checks.push("expired lease projects as interrupted without rewriting history or exposing another owner; header and resume agree");
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
  // Synthetic SSE projection checks complement the real Core recording tests.
  // They do not claim model inference or physical speaker identification.
  const syntheticEvents = [
    ["paragraph.finalized", {paragraph_id:"fixture-first",text:"A latch holds a value.",speaker:{status:"assigned",index:1}}],
    ["paragraph.finalized", {paragraph_id:"fixture-second",text:"The same topic continues.",speaker:{status:"unconfirmed",index:null}}],
    ["content.group.created", {paragraph_ids:["fixture-first","fixture-second"],reason:"capacity_continuation"}],
    ["explanation.card.created", {card_id:"fixture-card",result:{paragraph_summary:"先解释概念\n\n再说明条件",evidence_segment_ids:["fixture-first","fixture-second"],asset_page_ids:[],terms:[{term:"锁存器",explanation:"在使能期间响应输入，其他时间保持数据的存储元件",background_reference:"https://docs.amd.com/r/en-US/ug574-ultrascale-clb/Storage-Elements",evidence_segment_ids:["fixture-first"],asset_page_ids:[]}]}}],
    ["topic.window.checked", {paragraph_ids:["fixture-first","fixture-second"]}],
    ["model.job.stage", {job_id:"fixture-diagnostic",stage:"inferring",internal_only:"must-not-render-diagnostic"}],
  ].map(([event_type,payload],index)=>({schema_version:"1.0.0",event_id:`fixture-${index}`,session_id:session.id,source_id:"browser-fixture",sequence:index+1,event_type,captured_at_monotonic_ns:index+1,captured_at_wall:"2026-09-09T12:00:00Z",ingested_at:"2026-09-09T12:00:00Z",correlation_id:"fixture-correlation",causation_id:null,payload,content_hash:`sha256:${"0".repeat(64)}`}));
  await page.route(`**/sessions/${session.id}/stream`,route=>route.fulfill({status:200,contentType:"text/event-stream",body:syntheticEvents.map(event=>`event: message\ndata: ${JSON.stringify(event)}\n\n`).join("")}));
  await page.addInitScript(() => {
    const NativeEventSource = window.EventSource;
    window.__courseStreams = [];
    window.EventSource = class extends NativeEventSource {
      constructor(...args) {
        super(...args);
        if (String(args[0]).includes("/sessions/")) window.__courseStreams.push(this);
      }
    };
  });
  await page.goto(`${course}/notes/transcript`);
  await page.locator(".speaker-label").filter({hasText:"说话人 1"}).waitFor();
  await page.locator(".speaker-label").filter({hasText:"说话人待确认"}).waitFor();
  const insightPanel = page.getByTestId("paragraph-insight-panel");
  await insightPanel.getByText(/^同主题续接/).waitFor();
  assert.equal(await page.getByText("must-not-render-diagnostic").count(),0);
  await insightPanel.locator(".paragraph-terms-section summary").click();
  await insightPanel.getByText("在使能期间响应输入，其他时间保持数据的存储元件",{exact:true}).waitFor();
  await insightPanel.getByText(/不是老师原话/).waitFor();
  const reference = insightPanel.getByRole("link",{name:"查看背景资料 ↗"});
  assert.equal(await reference.getAttribute("href"),"https://docs.amd.com/r/en-US/ug574-ultrascale-clb/Storage-Elements");
  assert.equal(await reference.getAttribute("rel"),"noopener noreferrer");
  for (const width of [1360,390]) {
    await page.setViewportSize({width,height:960});
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth+1));
    await page.screenshot({path:path.join(evidence,`topic-speaker-${width}.png`),fullPage:true});
  }
  assert.deepEqual(pageErrors,[]);
  checks.push("synthetic SSE: anonymous and uncertain labels, same-topic continuation, readable explanation, reviewed background reference distinct from lecturer evidence; no internal diagnostics at desktop or 390px");
  let liveSequence = 100;
  const deliver = async (event_type, payload) => {
    const sequence = liveSequence++;
    const event = {...syntheticEvents[0], event_type, payload, sequence, event_id:`live-fixture-${sequence}`};
    await page.evaluate(event => {
      const stream = window.__courseStreams.at(-1);
      if (!stream) throw new Error("course event source missing");
      stream.dispatchEvent(new MessageEvent("message", {data:JSON.stringify(event)}));
    }, event);
  };
  await deliver("segment.finalized", {segment_id:"live-preview-a",display_mode:"internal_fragment",text:"The source becomes visible"});
  const preview = page.getByTestId("source-preview");
  await preview.waitFor();
  assert.equal(await preview.getByRole("button").count(),0);
  await deliver("segment.finalized", {segment_id:"live-preview-b",display_mode:"internal_fragment",text:"before sentence assembly."});
  await preview.getByText("The source becomes visible before sentence assembly.",{exact:true}).waitFor();
  assert.equal(await preview.count(),1);
  await deliver("paragraph.finalized", {paragraph_id:"live-final",segment_ids:["live-preview-a","live-preview-b"],text:"The source becomes visible before sentence assembly."});
  await preview.waitFor({state:"detached"});
  assert.equal(await page.locator(".course-document").getByText("The source becomes visible before sentence assembly.",{exact:true}).count(),1);
  await deliver("segment.finalized", {segment_id:"live-preview-c",display_mode:"internal_fragment",text:"Next source preview."});
  await preview.waitFor();
  await page.getByRole("group",{name:"语言显示模式"}).getByRole("button",{name:"译文",exact:true}).click();
  await preview.waitFor({state:"detached"});
  await page.getByRole("group",{name:"语言显示模式"}).getByRole("button",{name:"双语",exact:true}).click();
  await preview.waitFor();
  for (const width of [1360,390]) {
    await page.setViewportSize({width,height:960});
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth+1));
    await page.screenshot({path:path.join(evidence,`source-preview-${width}.png`),fullPage:true});
  }
  checks.push("live synthetic SSE: saved source appears before paragraph assembly, grows without duplication, is consumed by its paragraph and hidden in translation-only view; desktop and 390px layouts");
  await page.setViewportSize({width:1360,height:960});
  for (let i=0;i<12;i++) await deliver("paragraph.finalized", {
    paragraph_id:`scroll-paragraph-${i}`,text:`Synthetic paragraph ${i}. ` + "Only synthetic content for scrolling. ".repeat(8),
  });
  const documentPane = page.locator(".course-document");
  // Explicitly return to the bottom after populating the scroll fixture.
  await documentPane.evaluate(element => {element.scrollTop=element.scrollHeight;});
  await sleep(250);
  await deliver("segment.finalized", {segment_id:"live-preview-growth",display_mode:"internal_fragment",text:"A growing preview line. ".repeat(50)});
  await page.waitForFunction(() => {const e=document.querySelector(".course-document");return e.scrollHeight-e.scrollTop-e.clientHeight<80;});
  await documentPane.evaluate(element => {element.scrollTop=0;});
  await sleep(250);
  await deliver("segment.finalized", {segment_id:"live-preview-upscroll",display_mode:"internal_fragment",text:"A later preview must not interrupt reading."});
  await page.getByRole("button",{name:"有新内容，回到底部"}).waitFor();
  assert.ok(await documentPane.evaluate(element=>element.scrollTop<80));
  await page.getByRole("button",{name:"有新内容，回到底部"}).click();
  await page.waitForFunction(() => {const e=document.querySelector(".course-document");return e.scrollHeight-e.scrollTop-e.clientHeight<80;});
  const delayedTranslation = "迟到的译文应保持底部跟随，不改变原文。".repeat(60);
  await deliver("translation.finalized", {paragraph_id:"scroll-paragraph-11",text:delayedTranslation});
  await documentPane.getByText(delayedTranslation,{exact:true}).waitFor();
  await page.waitForFunction(() => {const e=document.querySelector(".course-document");return e.scrollHeight-e.scrollTop-e.clientHeight<80;});
  await documentPane.evaluate(element => {element.scrollTop=0;});
  await sleep(250);
  await deliver("translation.finalized", {paragraph_id:"scroll-paragraph-10",text:"上滑阅读时，另一段迟到的译文不应打断阅读。".repeat(30)});
  await page.getByRole("button",{name:"有新内容，回到底部"}).waitFor();
  assert.ok(await documentPane.evaluate(element=>element.scrollTop<80));
  checks.push("live preview growth follows the bottom without a new item; manual upscroll is preserved and the new-content control restores following");
  checks.push("delayed translation above the preview follows the bottom but never interrupts manual upscroll");
  const sameLanguageSource = "源语言：中文；无需再次翻译，但必须保留实际内容";
  await deliver("paragraph.finalized", {paragraph_id:"same-language-visible",text:sameLanguageSource});
  await deliver("translation.finalized", {paragraph_id:"same-language-visible",text:sameLanguageSource,translation_mode:"same_language"});
  await page.getByRole("group",{name:"语言显示模式"}).getByRole("button",{name:"译文",exact:true}).click();
  await documentPane.getByText(sameLanguageSource,{exact:true}).waitFor();
  await documentPane.getByText("原文，无需翻译",{exact:true}).waitFor();
  checks.push("translation-only view retains original content for same-language speech instead of showing only a status label");
  assert.deepEqual(pageErrors,[]);
  await writeFile(path.join(evidence,"browser-validation.json"),JSON.stringify({passed:true,checks,ack_count:acks.length,wasm_responses:wasm,physical_microphone_test:false,real_gpu_inference:false},null,2));
  console.log(JSON.stringify({passed:true,checks,ack_count:acks.length}));
} finally {
  clearInterval(heartbeat); await browser?.close(); core.kill();
}
