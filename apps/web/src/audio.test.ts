import { describe, expect, it } from "vitest";
import { encodeFrame, resample } from "./audio";

describe("audio transport", () => {
  it("encodes sequence and capture time as big-endian unsigned integers", () => {
    const frame = encodeFrame(42, 1_234, new Float32Array([1, -1]));
    const view = new DataView(frame);
    expect(view.getBigUint64(0, false)).toBe(42n);
    expect(view.getBigUint64(8, false)).toBe(1_234n);
    expect(view.getInt16(16, true)).toBe(32_767);
    expect(view.getInt16(18, true)).toBe(-32_768);
  });

  it("resamples a 48 kHz window to the 16 kHz service contract", () => {
    const input = new Float32Array(48_000);
    const output = resample(input, 48_000);
    expect(output).toHaveLength(16_000);
  });
});
