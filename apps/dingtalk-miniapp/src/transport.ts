const HEADER_BYTES = 16;

// Manual 64-bit serialization avoids BigInt requirements in older DingTalk WebViews.
export function writeUnsigned64(view: DataView, offset: number, value: number): void {
  const safe = Math.max(0, Math.floor(value));
  const high = Math.floor(safe / 0x1_0000_0000);
  const low = safe >>> 0;
  view.setUint32(offset, high, false);
  view.setUint32(offset + 4, low, false);
}

// RecorderManager PCM frames receive the same reliable header as Android and browser frames.
export function frameWithHeader(
  sequence: number,
  capturedAtMs: number,
  pcm: ArrayBuffer,
): ArrayBuffer {
  const output = new ArrayBuffer(HEADER_BYTES + pcm.byteLength);
  const view = new DataView(output);
  writeUnsigned64(view, 0, sequence);
  writeUnsigned64(view, 8, capturedAtMs);
  new Uint8Array(output, HEADER_BYTES).set(new Uint8Array(pcm));
  return output;
}
