// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { WorkspaceSidebar } from "./App";
import type { WorkspaceSnapshot } from "./types";

const snapshot: WorkspaceSnapshot = {
  folders: [],
  projects: [],
  project_placements: [],
  sessions: [],
  session_projects: {},
  session_metadata: [],
  trash: [],
  preference: null,
};

function renderSidebar(onCreateProject: (title: string, folderId: string | null, key: string, submittedAt: number) => Promise<void>) {
  return render(<WorkspaceSidebar
    snapshot={snapshot}
    activeProjectId={null}
    activeSessionId={null}
    theme="light"
    onToggleTheme={vi.fn()}
    onSelectProject={vi.fn()}
    onSelectSession={vi.fn()}
    onCreateFolder={vi.fn(async () => {})}
    onCreateProject={onCreateProject}
    onUpdateFolder={vi.fn(async () => {})}
    onPlaceProject={vi.fn(async () => {})}
    onUpdateProject={vi.fn(async () => {})}
    onUpdateSession={vi.fn(async () => {})}
    onMoveWorkspace={vi.fn(async () => {})}
    onTrash={vi.fn(async () => {})}
    onRestoreTrash={vi.fn(async () => {})}
    onPurgeTrash={vi.fn(async () => {})}
    onOpenSettings={vi.fn()}
  />);
}

function beginProjectCreation() {
  fireEvent.click(screen.getByRole("button", { name: "新建项目" }));
  fireEvent.change(screen.getByRole("textbox", { name: "名称" }), { target: { value: "测试项目" } });
  return screen.getByRole("dialog").querySelector("form")!;
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("blocks synchronous duplicate submits and shows pending state immediately", async () => {
  vi.stubGlobal("crypto", { randomUUID: () => "project-intent-1" });
  let resolveCreation!: () => void;
  const onCreateProject = vi.fn(() => new Promise<void>((resolve) => { resolveCreation = resolve; }));
  renderSidebar(onCreateProject);

  const form = beginProjectCreation();
  fireEvent.submit(form);
  fireEvent.submit(form);

  expect(onCreateProject).toHaveBeenCalledTimes(1);
  expect(onCreateProject).toHaveBeenCalledWith("测试项目", null, "project-intent-1", expect.any(Number));
  expect(screen.getByRole("status")).toHaveTextContent("正在创建项目…");
  expect(screen.getByRole("button", { name: "创建中…" })).toBeDisabled();

  await act(async () => { resolveCreation(); });
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
});

it("reuses the same idempotency key when retrying an unchanged failed intent", async () => {
  vi.stubGlobal("crypto", { randomUUID: () => "project-intent-retry" });
  const onCreateProject = vi.fn()
    .mockRejectedValueOnce(new Error("temporary network failure"))
    .mockResolvedValueOnce(undefined);
  renderSidebar(onCreateProject);

  const form = beginProjectCreation();
  fireEvent.submit(form);
  await waitFor(() => expect(onCreateProject).toHaveBeenCalledTimes(1));
  await waitFor(() => expect(screen.getByRole("button", { name: "保存" })).toBeEnabled());
  fireEvent.submit(form);
  await waitFor(() => expect(onCreateProject).toHaveBeenCalledTimes(2));

  expect(onCreateProject.mock.calls.map((call) => call[2])).toEqual(["project-intent-retry", "project-intent-retry"]);
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
});
