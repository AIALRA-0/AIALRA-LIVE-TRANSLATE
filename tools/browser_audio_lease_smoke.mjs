// Verify that a browser-style WebSocket can use its issued recording lease
// without sending an Authorization header or a proxy marker on the socket.
const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const SUBJECT = process.env.AIALRA_TEST_SUBJECT || "browser-audio-lease-smoke";
const nativeFetch = globalThis.fetch;

async function checked(input, init = {}) {
  const response = await nativeFetch(input, {
    ...init,
    headers: {
      ...(init.headers || {}),
      "X-aialra-auth-proxy": "1",
      "X-authentik-uid": SUBJECT,
    },
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(`${response.status} ${body.error || "request failed"}`);
  return body;
}

function openBrowserStyleSocket(sessionId, leaseToken) {
  const wsBase = process.env.AIALRA_WS_BASE || API.replace(/^http/, "ws").replace(/\/api\/v1$/, "");
  const socket = new WebSocket(
    `${wsBase}/api/v1/sessions/${sessionId}/sources/browser-lease-smoke/audio`,
    ["aialra.audio.v1", `lease.${leaseToken}`],
  );
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      socket.close();
      reject(new Error("browser-style audio WebSocket timed out"));
    }, 15_000);
    socket.addEventListener("open", () => {
      const frame = new Uint8Array(16 + 32_000);
      const view = new DataView(frame.buffer);
      view.setBigUint64(0, 1n, false);
      view.setBigUint64(8, BigInt(Date.now()), false);
      socket.send(frame);
    });
    socket.addEventListener("message", (event) => {
      let message;
      try {
        message = JSON.parse(String(event.data));
      } catch {
        return;
      }
      if (message.type === "audio.error") {
        clearTimeout(timer);
        socket.close();
        reject(new Error(message.message || "audio WebSocket rejected"));
      } else if (message.type === "audio.ack") {
        clearTimeout(timer);
        socket.close();
        resolve({
          type: message.type,
          sequence: message.sequence,
          commit_id_valid: typeof message.commit_id === "string" && message.commit_id.length > 0,
          duplicate: message.duplicate,
        });
      }
    });
    socket.addEventListener("error", () => {
      clearTimeout(timer);
      reject(new Error("browser-style audio WebSocket failed"));
    });
  });
}

const project = await checked(`${API}/projects`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "浏览器音频租约连接验证", source_language: "en", target_language: "zh-CN" }),
});
const deviceId = `browser-lease-smoke-${Date.now()}`;
const session = await checked(`${API}/projects/${project.id}/sessions`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "浏览器 WebSocket 验证", consent_confirmed: true, device_id: deviceId }),
});
const lease = await checked(`${API}/projects/${project.id}/sessions/${session.id}/recording/acquire`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: deviceId }),
});
const websocket = await openBrowserStyleSocket(session.id, lease.lease_token);
const stopped = await checked(`${API}/projects/${project.id}/sessions/${session.id}/recording/stop`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ device_id: deviceId, lease_token: lease.lease_token }),
});
const archived = await checked(`${API}/projects/${project.id}/placement`, {
  method: "PATCH",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ folder_id: null, sort_order: 9999, archived: true }),
});
process.stdout.write(`${JSON.stringify({
  status: "PASS",
  acquire_status: 200,
  websocket,
  stop_state: stopped.state || null,
  archived: typeof archived.archived_at === "string" && archived.archived_at.length > 0,
}, null, 2)}\n`);
