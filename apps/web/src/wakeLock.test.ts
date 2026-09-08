// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { RecordingWakeLock } from "./wakeLock";

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
function sentinel() {
  const value = new EventTarget() as WakeLockSentinel;
  Object.assign(value, { release: vi.fn(async () => { value.dispatchEvent(new Event("release")); }) });
  return value;
}
describe("recording screen lifetime", () => {
  it("deduplicates requests and releases exactly when recording ends", async () => {
    const lock = sentinel(); const request = vi.fn(async () => lock);
    vi.stubGlobal("navigator", { wakeLock: { request } });
    const controller = new RecordingWakeLock(vi.fn());
    controller.setActive(true); controller.setActive(true);
    await Promise.resolve();
    expect(request).toHaveBeenCalledTimes(1);
    controller.setActive(false);
    expect(lock.release).toHaveBeenCalledTimes(1);
  });
  it("releases a delayed grant after stop, never leaving the screen pinned", async () => {
    const lock = sentinel();
    let grant!: (value: WakeLockSentinel) => void;
    vi.stubGlobal("navigator", { wakeLock: { request: () => new Promise<WakeLockSentinel>((resolve) => { grant = resolve; }) } });
    const controller = new RecordingWakeLock(vi.fn());
    controller.setActive(true); controller.setActive(false); grant(lock);
    await Promise.resolve();
    expect(lock.release).toHaveBeenCalledTimes(1);
  });
  it("reacquires after an OS release only while still recording", async () => {
    vi.useFakeTimers();
    const first = sentinel(); const next = sentinel();
    const request = vi.fn().mockResolvedValueOnce(first).mockResolvedValue(next);
    vi.stubGlobal("navigator", { wakeLock: { request } });
    const controller = new RecordingWakeLock(vi.fn());
    controller.setActive(true); await Promise.resolve();
    first.dispatchEvent(new Event("release"));
    await vi.advanceTimersByTimeAsync(1000);
    expect(request).toHaveBeenCalledTimes(2);
    controller.setActive(false);
    await vi.advanceTimersByTimeAsync(2000);
    expect(request).toHaveBeenCalledTimes(2);
  });
});
