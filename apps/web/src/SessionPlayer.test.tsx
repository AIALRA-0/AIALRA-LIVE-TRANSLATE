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
  vi.spyOn(HTMLMediaElement.prototype, "load").mockImplementation(() => undefined);
  vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue(undefined);
});

afterEach(cleanup);

it("does not preload long recordings, keeps playback available, and reports readiness after media loads", async () => {
  const onReady = vi.fn();
  const { container } = render(<SessionPlayer sessionId="session-test" sessionState="completed" seekRequest={null} onReady={onReady} />);
  await act(async () => undefined);

  const audio = screen.getByLabelText("课程录音") as HTMLAudioElement;
  expect(audio).not.toHaveAttribute("controls");
  expect(audio).toHaveAttribute("preload", "metadata");
  expect(screen.getByRole("button", { name: "播放课程" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "后退 15 秒" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "前进 15 秒" })).toBeInTheDocument();
  expect(screen.getByRole("slider", { name: "回放位置" })).toBeInTheDocument();
  expect(screen.getByRole("combobox", { name: "回放速度" })).toHaveValue("1");

  fireEvent.loadedMetadata(audio);
  expect(onReady).toHaveBeenLastCalledWith(true);
  expect(container.querySelectorAll("audio")).toHaveLength(1);
});

it("keeps a skip target when a stale media time update arrives", async () => {
  render(<SessionPlayer sessionId="session-test" sessionState="completed" seekRequest={null} onReady={vi.fn()} />);
  await act(async () => undefined);
  const audio = screen.getByLabelText("课程录音") as HTMLAudioElement;
  Object.defineProperty(audio, "readyState", { configurable: true, value: 4 });
  fireEvent.loadedMetadata(audio);

  fireEvent.click(screen.getByRole("button", { name: "前进 15 秒" }));
  expect(screen.getByRole("slider", { name: "回放位置" })).toHaveValue("15");
  audio.currentTime = 0;
  fireEvent.timeUpdate(audio);
  expect(screen.getByRole("slider", { name: "回放位置" })).toHaveValue("15");

  audio.currentTime = 15;
  fireEvent.timeUpdate(audio);
  fireEvent.click(screen.getByRole("button", { name: "前进 15 秒" }));
  expect(screen.getByRole("slider", { name: "回放位置" })).toHaveValue("30");
});

it("preserves the selected segment when session state refreshes audio and resets for a new session", async () => {
  sessionAudioIndex
    .mockResolvedValueOnce({
      duration_ms: 180_000,
      positions: [{ captured_at_ms: 1_000, duration_ms: 180_000, playback_start_ms: 0, playback_end_ms: 180_000 }],
    })
    .mockResolvedValueOnce({
      duration_ms: 240_000,
      positions: [{ captured_at_ms: 1_000, duration_ms: 240_000, playback_start_ms: 0, playback_end_ms: 240_000 }],
    });
  const onReady = vi.fn();
  const props = { seekRequest: null, onReady };
  const { rerender } = render(<SessionPlayer sessionId="session-test" sessionState="recording" {...props} />);
  await act(async () => undefined);

  const audio = screen.getByLabelText("课程录音") as HTMLAudioElement;
  const slider = screen.getByRole("slider", { name: "回放位置" });
  fireEvent.change(slider, { target: { value: "60" } });
  expect(slider).toHaveValue("60");
  expect(audio.src).toContain("/sessions/session-test/audio/segment?start_ms=45000");

  rerender(<SessionPlayer sessionId="session-test" sessionState="completed" {...props} />);
  await act(async () => undefined);

  expect(sessionAudioIndex).toHaveBeenCalledTimes(2);
  expect(slider).toHaveValue("60");
  expect(slider).toHaveAttribute("max", "240");
  expect(audio.src).toContain("/sessions/session-test/audio/segment?start_ms=45000");

  rerender(<SessionPlayer sessionId="session-next" sessionState="recording" {...props} />);
  await act(async () => undefined);

  expect(screen.getByRole("slider", { name: "回放位置" })).toHaveValue("0");
  expect((screen.getByLabelText("课程录音") as HTMLAudioElement).src).toContain("/sessions/session-next/audio/segment?start_ms=0");
});

it("loads the current segment before the first skip instead of resetting it", async () => {
  render(<SessionPlayer sessionId="session-test" sessionState="completed" seekRequest={null} onReady={vi.fn()} />);
  await act(async () => undefined);
  const audio = screen.getByLabelText("课程录音") as HTMLAudioElement;
  Object.defineProperty(audio, "readyState", { configurable: true, value: 0 });
  fireEvent.click(screen.getByRole("button", { name: "前进 15 秒" }));
  expect(audio.load).toHaveBeenCalledOnce();
  expect(screen.getByRole("slider", { name: "回放位置" })).toHaveValue("15");
});

it("shows buffering without replacing the playback controls", async () => {
  render(<SessionPlayer sessionId="session-test" sessionState="completed" seekRequest={null} onReady={vi.fn()} />);
  await act(async () => undefined);
  const audio = screen.getByLabelText("课程录音");
  fireEvent.loadedMetadata(audio);
  fireEvent.waiting(audio);
  expect(screen.getByRole("status")).toHaveTextContent("正在加载当前录音");
  expect(screen.getByRole("slider", { name: "回放位置" })).toBeInTheDocument();
});

it("seeks to the recording endpoint inside the final segment", async () => {
  render(<SessionPlayer sessionId="session-test" sessionState="completed" seekRequest={null} onReady={vi.fn()} />);
  await act(async () => undefined);

  const audio = screen.getByLabelText("课程录音") as HTMLAudioElement;
  fireEvent.change(screen.getByRole("slider", { name: "回放位置" }), { target: { value: "180" } });

  expect(screen.getByRole("slider", { name: "回放位置" })).toHaveValue("180");
  expect(audio.src).toContain("start_ms=135000");
});
