// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { CloudTextPolicy } from "./CloudTextPolicy";

const fetchMock = vi.fn();

beforeEach(() => {
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it("loads the project policy and defaults the control off when cloud text is not authorized", async () => {
  fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ cloud_enabled: false, allowed_modalities: [], route_available: true }), { status: 200 }));
  render(<CloudTextPolicy projectId="project-1" />);
  await act(async () => undefined);

  expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/project-1/ai-policy");
  expect(screen.getByRole("checkbox", { name: /允许本项目使用云端文本讲解/ })).not.toBeChecked();
  expect(screen.getByText(/字幕和资料文本/)).toBeInTheDocument();
  expect(screen.getByText(/不发送原始音频或图片/)).toBeInTheDocument();
});

it("sends an explicit text-only project authorization", async () => {
  fetchMock
    .mockResolvedValueOnce(new Response(JSON.stringify({ cloud_enabled: false, allowed_modalities: [], route_available: true }), { status: 200 }))
    .mockResolvedValueOnce(new Response(JSON.stringify({ cloud_enabled: true, allowed_modalities: ["text"], route_available: true }), { status: 200 }));
  render(<CloudTextPolicy projectId="project-2" />);
  await act(async () => undefined);

  await act(async () => {
    fireEvent.click(screen.getByRole("checkbox", { name: /允许本项目使用云端文本讲解/ }));
  });

  expect(fetchMock).toHaveBeenLastCalledWith("/api/v1/projects/project-2/ai-policy", expect.objectContaining({
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ cloud_enabled: true, allowed_modalities: ["text"] }),
  }));
  expect(screen.getByRole("checkbox", { name: /允许本项目使用云端文本讲解/ })).toBeChecked();
});

it("keeps the setting disabled when policy loading fails", async () => {
  fetchMock.mockResolvedValueOnce(new Response("{}", { status: 503 }));
  render(<CloudTextPolicy projectId="project-3" />);
  await act(async () => undefined);

  expect(screen.getByRole("checkbox", { name: /允许本项目使用云端文本讲解/ })).toBeDisabled();
  expect(screen.getByRole("alert")).toHaveTextContent("项目授权状态读取失败");
  expect(fetchMock).toHaveBeenCalledTimes(1);
});

it("does not offer authorization when the server text route is closed", async () => {
  fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ cloud_enabled: false, allowed_modalities: [], route_available: false }), { status: 200 }));
  render(<CloudTextPolicy projectId="project-4" />);
  await act(async () => undefined);

  expect(screen.getByRole("checkbox", { name: /允许本项目使用云端文本讲解/ })).toBeDisabled();
  expect(screen.getByText("服务器未开放")).toBeInTheDocument();
  expect(fetchMock).toHaveBeenCalledTimes(1);
});
