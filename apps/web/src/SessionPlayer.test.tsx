// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SessionPlayer } from "./SessionPlayer";

const { sessionAudioIndex } = vi.hoisted(() => ({ sessionAudioIndex: vi.fn() }));
vi.mock("./api", () => ({ api: { sessionAudioIndex } }));

beforeEach(() => {
  sessionAudioIndex.mockReset();
  sessionAudioIndex.mockResolvedValue({
    duration_ms: 180_000,
    positions: [{ captured_at_ms: 1_000, duration_ms: 180_000, playback_start_ms: 0, playback_end_ms: 180_000 }],
  });
});

afterEach(cleanup);

it("renders one unified accessible player and reports readiness only after media loads", async () => {
  const onReady = vi.fn();
  const { container } = render(<SessionPlayer sessionId="session-test" sessionState="completed" seekRequest={null} onReady={onReady} />);
  await act(async () => undefined);

  const audio = screen.getByLabelText("课程录音") as HTMLAudioElement;
  expect(audio).not.toHaveAttribute("controls");
  expect(screen.getByRole("button", { name: "播放课程" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "后退 15 秒" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "前进 15 秒" })).toBeInTheDocument();
  expect(screen.getByRole("slider", { name: "回放位置" })).toBeInTheDocument();
  expect(screen.getByRole("combobox", { name: "回放速度" })).toHaveValue("1");

  fireEvent.loadedMetadata(audio);
  expect(screen.getByRole("button", { name: "播放课程" })).toBeEnabled();
  expect(onReady).toHaveBeenLastCalledWith(true);
  expect(container.querySelectorAll("audio")).toHaveLength(1);
});

it("shows buffering without replacing the playback controls", async () => {
  render(<SessionPlayer sessionId="session-test" sessionState="completed" seekRequest={null} onReady={vi.fn()} />);
  await act(async () => undefined);
  const audio = screen.getByLabelText("课程录音");
  fireEvent.loadedMetadata(audio);
  fireEvent.waiting(audio);
  expect(screen.getByRole("status")).toHaveTextContent("正在加载下一段录音");
  expect(screen.getByRole("slider", { name: "回放位置" })).toBeInTheDocument();
});
