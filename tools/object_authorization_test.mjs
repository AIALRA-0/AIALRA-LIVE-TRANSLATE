// Internal object reads are bound to the worker that currently owns the job lease.
const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const INTERNAL = API.replace(/\/api\/v1$/, "") + "/internal/v1";
const TOKEN = process.env.AIALRA_WORKER_TOKEN;
const SUBJECT = process.env.AIALRA_TEST_SUBJECT || "object-auth-owner";
const WORKER_ID = process.env.AIALRA_TEST_WORKER_ID || "object-auth-worker";
const PROXY_MARKER = process.env.AIALRA_TEST_PROXY_MARKER === "true";

if (!TOKEN) throw new Error("AIALRA_WORKER_TOKEN is required");

async function request(path, init = {}) {
  return fetch(`${API}${path}`, {
    ...init,
    headers: {
      ...(init.headers || {}),
      "X-authentik-uid": SUBJECT,
      ...(PROXY_MARKER ? { "X-aialra-Auth-Proxy": "1" } : {}),
    },
  });
}

async function checked(responsePromise) {
  const response = await responsePromise;
  if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
  return response.json();
}

const project = await checked(request("/projects", {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "对象授权验证", source_language: "en", target_language: "zh-CN" }),
}));
const session = await checked(request(`/projects/${project.id}/sessions`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ title: "内部对象读取", consent_confirmed: true, device_id: "object-auth-device" }),
}));
const material = new FormData();
material.append("file", new Blob(["private object authorization fixture"], { type: "text/plain" }), "object-auth.txt");
const uploaded = await checked(request(`/sessions/${session.id}/assets`, { method: "POST", body: material }));

const leaseResponse = await fetch(`${INTERNAL}/jobs/lease`, {
  method: "POST",
  headers: { Authorization: `Bearer ${TOKEN}`, "content-type": "application/json" },
  body: JSON.stringify({ worker_id: WORKER_ID, capabilities: ["asset_parse"], job_id: uploaded.job_id }),
});
if (!leaseResponse.ok) throw new Error(`${leaseResponse.status} ${await leaseResponse.text()}`);
const leased = await leaseResponse.json();
if (leased.job?.id !== uploaded.job_id) throw new Error("unexpected job was leased during object authorization test");

const noWorker = await fetch(`${INTERNAL}/jobs/${uploaded.job_id}/input`, {
  headers: { Authorization: `Bearer ${TOKEN}` },
});
if (noWorker.status !== 401) throw new Error(`missing worker identity returned ${noWorker.status}`);
const wrongWorker = await fetch(`${INTERNAL}/jobs/${uploaded.job_id}/input`, {
  headers: { Authorization: `Bearer ${TOKEN}`, "X-Aialra-Worker-ID": "different-worker" },
});
if (wrongWorker.status !== 409) throw new Error(`wrong worker identity returned ${wrongWorker.status}`);
const correctWorker = await fetch(`${INTERNAL}/jobs/${uploaded.job_id}/input`, {
  headers: { Authorization: `Bearer ${TOKEN}`, "X-Aialra-Worker-ID": WORKER_ID },
});
if (!correctWorker.ok || !(await correctWorker.arrayBuffer()).byteLength) throw new Error("leased worker could not read its object input");

process.stdout.write(`${JSON.stringify({
  status: "PASS",
  job_id: uploaded.job_id,
  missing_worker_status: noWorker.status,
  wrong_worker_status: wrongWorker.status,
  owner_worker_read: true,
}, null, 2)}\n`);
