// Verify owner isolation across every legacy session-scoped route and the audio upgrade.
import WebSocket from "ws";

const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const OWNER = process.env.AIALRA_TEST_SUBJECT || "security-owner";
const ATTACKER = `${OWNER}-attacker`;
const PROXY_MARKER = process.env.AIALRA_TEST_PROXY_MARKER === "true";

async function request(path, init = {}, subject = OWNER, origin, includeProxyMarker = PROXY_MARKER) {
  const headers = { ...(init.headers || {}), "X-authentik-uid": subject };
  if (includeProxyMarker) headers["X-aialra-Auth-Proxy"] = "1";
  if (origin) headers.Origin = origin;
  return fetch(`${API}${path}`, { ...init, headers });
}

async function json(path, init, subject = OWNER) {
  const response = await request(path, init, subject);
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return response.json();
}

const project = await json("/projects", {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "安全隔离验证", source_language: "en", target_language: "zh-CN" }),
});
const session = await json(`/projects/${project.id}/sessions`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "跨项目访问验证", consent_confirmed: true, device_id: "security-device-0001" }),
});
const lease = await json(`/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: "security-device-0001" }),
});

const checks = [];
for (const path of [
  `/projects/${project.id}`,
  `/projects/${project.id}/sessions`,
  `/sessions/${session.id}`,
  `/sessions/${session.id}/events`,
  `/sessions/${session.id}/dingtalk/capabilities`,
]) {
  const response = await request(path, {}, ATTACKER);
  if (response.status !== 404) throw new Error(`cross-user route ${path} returned ${response.status}`);
  checks.push(path);
}

const oldStart = await request(`/sessions/${session.id}/start`, { method: "POST" }, OWNER);
if (oldStart.status !== 409) throw new Error(`legacy start bypass returned ${oldStart.status}`);

const wsBase = API.replace(/^http/, "ws").replace(/\/api\/v1$/, "");
await new Promise((resolve, reject) => {
  const socket = new WebSocket(
    `${wsBase}/api/v1/sessions/${session.id}/sources/security/audio`,
    ["aialra.audio.v1", `lease.${lease.lease_token}`],
    { headers: { "X-authentik-uid": ATTACKER, ...(PROXY_MARKER ? { "X-aialra-Auth-Proxy": "1" } : {}) } },
  );
  const timer = setTimeout(() => reject(new Error("cross-user WebSocket remained open")), 10_000);
  socket.onopen = () => {
    socket.close();
    reject(new Error("cross-user WebSocket accepted an owner lease"));
  };
  socket.onerror = () => { clearTimeout(timer); resolve(); };
  socket.onclose = () => { clearTimeout(timer); resolve(); };
});

if (process.env.AIALRA_EXPECT_PROXY_MARKER_REJECTION === "true") {
  const missingMarker = await request("/projects", {}, OWNER, undefined, false);
  if (missingMarker.status !== 401) {
    throw new Error(`direct request without proxy marker returned ${missingMarker.status}`);
  }
}

const spoofedOrigin = await request("/projects", {}, OWNER, "https://attacker.invalid");
if (process.env.AIALRA_EXPECT_ORIGIN_REJECTION === "true" && spoofedOrigin.status !== 403) {
  throw new Error(`cross-site origin returned ${spoofedOrigin.status}`);
}

await json(`/projects/${project.id}/sessions/${session.id}/recording/stop`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: "security-device-0001", lease_token: lease.lease_token }),
});
process.stdout.write(`${JSON.stringify({
  status: "PASS",
  isolated_routes: checks.length,
  legacy_start_blocked: true,
  cross_user_websocket_blocked: true,
  proxy_marker_enforced: process.env.AIALRA_EXPECT_PROXY_MARKER_REJECTION === "true",
  origin_rejection_checked: process.env.AIALRA_EXPECT_ORIGIN_REJECTION === "true",
}, null, 2)}\n`);
