import rnnoiseWorkletUrl from "@sapphi-red/web-noise-suppressor/rnnoiseWorklet.js?url";
import rnnoiseUrl from "@sapphi-red/web-noise-suppressor/rnnoise.wasm?url";
import rnnoiseSimdUrl from "@sapphi-red/web-noise-suppressor/rnnoise_simd.wasm?url";

export type NoiseSuppressionMode = "rnnoise" | "browser" | "off";

// All assets are bundled on our origin. No audio or device data leaves the browser.
export async function createDenoiser(context: AudioContext): Promise<AudioWorkletNode & { destroy(): void }> {
  const { RnnoiseWorkletNode, loadRnnoise } = await import("@sapphi-red/web-noise-suppressor");
  const wasmBinary = await loadRnnoise({ url: rnnoiseUrl, simdUrl: rnnoiseSimdUrl }, { signal: AbortSignal.timeout(8000) });
  await context.audioWorklet.addModule(rnnoiseWorkletUrl);
  return new RnnoiseWorkletNode(context, { wasmBinary, maxChannels: 1 });
}
