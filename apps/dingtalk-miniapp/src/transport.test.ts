import { describe, expect, it } from "vitest";
import { frameWithHeader } from "./transport";

describe("DingTalk foreground frame transport", () => {
  it("matches the core big-endian header contract", () => {
    const frame = frameWithHeader(42, 1_234, new Uint8Array([1, 2]).buffer);
    const view = new DataView(frame);
    expect(view.getUint32(0, false)).toBe(0);
    expect(view.getUint32(4, false)).toBe(42);
    expect(view.getUint32(12, false)).toBe(1_234);
    expect(new Uint8Array(frame).slice(16)).toEqual(new Uint8Array([1, 2]));
  });
});
