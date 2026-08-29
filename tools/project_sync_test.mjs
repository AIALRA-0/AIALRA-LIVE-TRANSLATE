import WebSocket from "ws";

const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const SUBJECT = process.env.AIALRA_TEST_SUBJECT || "project-sync-owner";

async function request(path, init = {}, subject = SUBJECT) {
  const response = await fetch(`${API}${path}`, {
    ...init,
    headers: { ...(init.headers || {}), "X-authentik-uid": subject },
  });
  return response;
}

async function json(path, init, subject) {
  const response = await request(path, init, subject);
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return response.json();
}

async function expectOldLeaseRejected(sessionId, token) {
  const wsBase = API.replace(/^http/, "ws").replace(/\/api\/v1$/, "");
  await new Promise((resolve, reject) => {
    const socket = new WebSocket(
      `${wsBase}/api/v1/sessions/${sessionId}/sources/expired-lease/audio`,
      ["aialra.audio.v1", `lease.${token}`],
      { headers: { "X-authentik-uid": SUBJECT } },
    );
    const timer = setTimeout(() => reject(new Error("expired lease WebSocket was not rejected")), 10_000);
    socket.onopen = () => reject(new Error("expired lease opened an audio WebSocket"));
    socket.onerror = () => { clearTimeout(timer); resolve(); };
    socket.onclose = () => { clearTimeout(timer); resolve(); };
  });
}

const project = await json("/projects", {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "多设备同步与接管验证", source_language: "en", target_language: "zh-CN" }),
});
const deviceA = "sync-browser-a-0001";
const deviceB = "sync-browser-b-0002";
const session = await json(`/projects/${project.id}/sessions`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "双设备租约验证", consent_confirmed: true, device_id: deviceA }),
});
const leaseA = await json(`/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceA }),
});
const conflict = await request(`/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceB }),
});
if (conflict.status !== 409) throw new Error(`second device conflict returned ${conflict.status}`);

const ownerViewA = await json(`/projects/${project.id}/sessions`);
const ownerViewB = await json(`/projects/${project.id}/sessions`);
if (JSON.stringify(ownerViewA) !== JSON.stringify(ownerViewB)) throw new Error("same-user devices observed different project sessions");
const otherUser = await request(`/projects/${project.id}`, {}, "project-sync-other-user");
if (otherUser.status !== 404) throw new Error(`cross-user project read returned ${otherUser.status}`);

// The real 45-second expiry verifies that no hidden shorter test TTL changes production behavior.
await new Promise((resolve) => setTimeout(resolve, 46_000));
const leaseB = await json(`/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ device_id: deviceB }),
});
if (leaseB.generation !== leaseA.generation + 1) throw new Error("takeover did not increment lease generation");
await expectOldLeaseRejected(session.id, leaseA.lease_token);

await json(`/projects/${project.id}/sessions/${session.id}/recording/stop`, {
  method: "POST", headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: deviceB, lease_token: leaseB.lease_token }),
});

const stream = await request(`/projects/${project.id}/stream`, { headers: { "Last-Event-ID": "0" } });
if (!stream.ok || !stream.body) throw new Error("project SSE replay did not open");
const reader = stream.body.getReader();
const first = await Promise.race([
  reader.read(),
  new Promise((_, reject) => setTimeout(() => reject(new Error("project SSE replay timed out")), 10_000)),
]);
await reader.cancel();
if (first.done || !new TextDecoder().decode(first.value).includes("id:")) throw new Error("project SSE replay had no durable cursor");

process.stdout.write(`${JSON.stringify({
  status: "PASS", project_id: project.id, session_id: session.id,
  same_user_views_equal: true, cross_user_status: otherUser.status,
  second_recorder_status: conflict.status, first_generation: leaseA.generation,
  takeover_generation: leaseB.generation, old_lease_rejected: true, sse_cursor_replayed: true,
}, null, 2)}\n`);
