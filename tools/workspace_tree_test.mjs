// Exercise the durable workspace tree without touching transcript or asset data.
const API = process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1";
const SUBJECT = process.env.AIALRA_TEST_SUBJECT || "workspace-tree-owner";
const PROXY_MARKER = process.env.AIALRA_TEST_PROXY_MARKER === "true";

function headers() {
  return {
    "content-type": "application/json",
    "X-authentik-uid": SUBJECT,
    ...(PROXY_MARKER ? { "X-aialra-Auth-Proxy": "1" } : {}),
  };
}

async function request(path, init = {}) {
  return fetch(`${API}${path}`, {
    ...init,
    headers: { ...headers(), ...(init.headers || {}) },
  });
}

async function json(path, init = {}) {
  const response = await request(path, init);
  const body = await response.text();
  if (!response.ok) throw new Error(`${response.status} ${body}`);
  return JSON.parse(body);
}

async function expectStatus(path, status, init = {}) {
  const response = await request(path, init);
  const body = await response.text();
  if (response.status !== status) throw new Error(`${path} returned ${response.status}, expected ${status}: ${body}`);
  return body;
}

const root = await json("/workspace/folders", {
  method: "POST",
  body: JSON.stringify({ title: "工作区根测试", parent_id: null }),
});
const child = await json("/workspace/folders", {
  method: "POST",
  body: JSON.stringify({ title: "课程子文件夹", parent_id: root.id }),
});
const project = await json("/projects", {
  method: "POST",
  body: JSON.stringify({ title: "工作区树测试项目", source_language: "en", target_language: "zh-CN" }),
});
await json(`/projects/${project.id}/placement`, {
  method: "PATCH",
  body: JSON.stringify({ folder_id: child.id, sort_order: 0, archived: false }),
});

await expectStatus(`/workspace/folders/${root.id}`, 400, {
  method: "PATCH",
  body: JSON.stringify({ title: root.title, parent_id: child.id, sort_order: 0, archived: false }),
});
await expectStatus(`/workspace/folders/${root.id}`, 409, {
  method: "PATCH",
  body: JSON.stringify({ title: root.title, parent_id: null, sort_order: 0, archived: true }),
});
await json(`/projects/${project.id}/placement`, {
  method: "PATCH",
  body: JSON.stringify({ folder_id: null, sort_order: 0, archived: false }),
});
const renamed = await json(`/workspace/folders/${root.id}`, {
  method: "PATCH",
  body: JSON.stringify({ title: "已重命名根文件夹", parent_id: null, sort_order: 1, archived: false }),
});
await json(`/workspace/folders/${child.id}/archive`, { method: "POST" });
const archived = await json(`/workspace/folders/${root.id}/archive`, { method: "POST" });
const restored = await json(`/workspace/folders/${root.id}`, {
  method: "PATCH",
  body: JSON.stringify({ title: archived.title, parent_id: null, sort_order: archived.sort_order, archived: false }),
});
await json(`/workspace/folders/${child.id}`, {
  method: "PATCH",
  body: JSON.stringify({ title: child.title, parent_id: root.id, sort_order: child.sort_order, archived: false }),
});

const depthFolders = [root.id];
for (let depth = 2; depth <= 5; depth += 1) {
  const folder = await json("/workspace/folders", {
    method: "POST",
    body: JSON.stringify({ title: `深度 ${depth}`, parent_id: depthFolders.at(-1) }),
  });
  depthFolders.push(folder.id);
}
await expectStatus("/workspace/folders", 400, {
  method: "POST",
  body: JSON.stringify({ title: "超出深度", parent_id: depthFolders.at(-1) }),
});

const snapshot = await json("/workspace");
const rootInSnapshot = snapshot.folders.find((folder) => folder.id === root.id);
if (!rootInSnapshot || rootInSnapshot.archived_at || rootInSnapshot.title !== renamed.title) {
  throw new Error("workspace folder rename or restore was not durable");
}
process.stdout.write(`${JSON.stringify({
  status: "PASS",
  cycle_rejected: true,
  non_empty_archive_rejected: true,
  rename_and_restore: restored.title === renamed.title,
  maximum_depth: depthFolders.length,
  depth_overflow_rejected: true,
  project_moved_before_archive: true,
}, null, 2)}\n`);
