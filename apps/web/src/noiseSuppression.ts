import rnnoiseWorkletUrl from "@sapphi-red/web-noise-suppressor/rnnoiseWorklet.js?url";
import rnnoiseUrl from "@sapphi-red/web-noise-suppressor/rnnoise.wasm?url";
import rnnoiseSimdUrl from "@sapphi-red/web-noise-suppressor/rnnoise_simd.wasm?url";
import gtcrnWorkletUrl from "@sapphi-red/web-noise-suppressor/gtcrnWorklet.js?url";
import gtcrnUrl from "@sapphi-red/web-noise-suppressor/gtcrn.wasm?url";

export type NoiseSuppressionMode = "gtcrn" | "rnnoise" | "browser" | "off";

// All assets are bundled on our origin. No audio or device data leaves the browser.
export async function createDenoiser(context: AudioContext, mode: "gtcrn" | "rnnoise" = "gtcrn"): Promise<AudioWorkletNode & { destroy(): void }> {
  const { GtcrnWorkletNode, loadGtcrn, RnnoiseWorkletNode, loadRnnoise } = await import("@sapphi-red/web-noise-suppressor");
  if (mode === "gtcrn") {
    const wasmBinary = await loadGtcrn({ url: gtcrnUrl }, { signal: AbortSignal.timeout(8000) });
    await context.audioWorklet.addModule(gtcrnWorkletUrl);
    return new GtcrnWorkletNode(context, { wasmBinary, maxChannels: 1 });
  }
  const wasmBinary = await loadRnnoise({ url: rnnoiseUrl, simdUrl: rnnoiseSimdUrl }, { signal: AbortSignal.timeout(8000) });
  await context.audioWorklet.addModule(rnnoiseWorkletUrl);
  return new RnnoiseWorkletNode(context, { wasmBinary, maxChannels: 1 });
}
