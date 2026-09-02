import { describe, expect, it } from "vitest";
import { encodeFrame, isDurableAudioAck, mediaInputError, nextFramesToSend, recoverNextSequence, resample } from "./audio";

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

describe("durable browser audio sequence", () => {
  it("continues after a fully acknowledged page refresh", () => {
    expect(recoverNextSequence(42, [])).toBe(42);
  });

  it("continues after the newest pending IndexedDB frame", () => {
    expect(recoverNextSequence(12, [12, 13, 14])).toBe(15);
  });

  it("starts a new lease generation at sequence one", () => {
    expect(recoverNextSequence(null, [])).toBe(1);
  });
});

describe("bounded audio recovery", () => {
  it("sends the oldest eight cached frames instead of flooding a recovered socket", () => {
    expect(nextFramesToSend([12, 4, 9, 3, 8, 7, 6, 5, 11, 10], [])).toEqual([
      3, 4, 5, 6, 7, 8, 9, 10,
    ]);
  });

  it("fills only the remaining acknowledgement window", () => {
    expect(nextFramesToSend([1, 2, 3, 4, 5], [1, 2, 3], 4)).toEqual([4]);
  });
});

describe("durable acknowledgement contract", () => {
  it("accepts only an ACK carrying a non-empty commit id", () => {
    expect(isDurableAudioAck({ type: "audio.ack", sequence: 3, commit_id: "commit-3" })).toBe(true);
    expect(isDurableAudioAck({ type: "audio.ack", sequence: 3 })).toBe(false);
    expect(isDurableAudioAck({ type: "audio.ack", sequence: 0, commit_id: "commit-0" })).toBe(false);
  });
});

describe("microphone device errors", () => {
  it("keeps permission and device failures actionable without exposing browser internals", () => {
    expect(mediaInputError({ name: "NotAllowedError" })).toContain("麦克风权限被拒绝");
    expect(mediaInputError({ name: "OverconstrainedError" })).toContain("所选输入设备当前不可用");
  });
});
