import type { SessionAudioIndex } from "./api";

// Recording runs may have wall-clock gaps; the player joins recorded PCM only.
export function playbackTimeForCapture(index: SessionAudioIndex, capturedAtMs: number): number | null {
  if (!Number.isFinite(capturedAtMs) || index.positions.length === 0) return null;
  const positions = index.positions;
  let low = 0;
  let high = positions.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (positions[middle].captured_at_ms <= capturedAtMs) low = middle + 1;
    else high = middle;
  }
  const entry = positions[Math.max(0, low - 1)];
  const elapsed = Math.max(0, Math.min(entry.duration_ms, capturedAtMs - entry.captured_at_ms));
  return Math.min(index.duration_ms, entry.playback_start_ms + elapsed) / 1000;
}
