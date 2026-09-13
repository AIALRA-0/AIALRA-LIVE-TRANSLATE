import { expect, it } from "vitest";
import { playbackTimeForCapture } from "./sessionPlayback";

it("seeks to the second recording run without playing the wall-clock pause", () => {
  const index = { duration_ms: 200, positions: [
    { captured_at_ms: 1000, duration_ms: 100, playback_start_ms: 0, playback_end_ms: 100 },
    { captured_at_ms: 20_000, duration_ms: 100, playback_start_ms: 100, playback_end_ms: 200 },
  ] };
  expect(playbackTimeForCapture(index, 20_050)).toBe(0.15);
  expect(playbackTimeForCapture(index, 15_000)).toBe(0.1);
});
