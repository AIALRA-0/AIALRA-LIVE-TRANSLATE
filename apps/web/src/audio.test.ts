import { describe, expect, it } from "vitest";
import { StreamingResampler, assessMicrophoneLevels, encodeFrame, isDurableAudioAck, mediaInputError, nextFramesToSend, recoverNextSequence, resample } from "./audio";

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

  it("keeps the resampling phase and duration across AudioWorklet blocks", () => {
    const input = Float32Array.from({ length: 48_000 }, (_, index) => index / 48_000);
    const streaming = new StreamingResampler(48_000);
    const outputParts: Float32Array[] = [];
    for (let offset = 0; offset < input.length; offset += 128) {
      outputParts.push(streaming.push(input.subarray(offset, Math.min(offset + 128, input.length))));
    }
    outputParts.push(streaming.flush());
    const output = new Float32Array(outputParts.reduce((total, part) => total + part.length, 0));
    let offset = 0;
    outputParts.forEach((part) => { output.set(part, offset); offset += part.length; });
    expect(output).toHaveLength(16_000);
    expect(output[0]).toBeCloseTo(input[0]);
    expect(output[15_999]).toBeCloseTo(input[47_997]);
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

describe("microphone level assessment", () => {
  it("accepts ordinary speech relative to a quiet room even below the old fixed peak threshold", () => {
    const result = assessMicrophoneLevels(
      [-62, -60, -61, -59, -60],
      [-48, -46, -45, -47, -49, -46, -44],
      0,
    );
    expect(result.passed).toBe(true);
    expect(result.voicedRatio).toBeGreaterThanOrEqual(0.25);
    expect(result.speechMedianDbfs).toBeGreaterThan(result.noiseFloorDbfs);
  });

  it("rejects silence and a quiet signal without enough speech frames", () => {
    const result = assessMicrophoneLevels(
      [-70, -69, -71, -70],
      [-69, -70, -68, -71],
      0,
    );
    expect(result.passed).toBe(false);
    expect(result.voicedRatio).toBe(0);
    expect(result.message).toContain("持续语音");
  });

  it("rejects clipped input independently of speech presence", () => {
    const result = assessMicrophoneLevels(
      [-65, -64, -66],
      [-35, -34, -36, -35],
      0.02,
    );
    expect(result.passed).toBe(false);
    expect(result.message).toContain("音量过高");
  });
});
