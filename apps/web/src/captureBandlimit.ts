// 48 kHz -> 16 kHz needs an anti-alias filter before selecting output samples.
// Linear interpolation alone is not a low-pass filter: at the integer ratio of
// three it just selects every third sample and folds high frequencies into speech.
// Use the browser's native biquads, not another denoiser or a custom DSP runtime.
// Web Audio specifies lowpass Q in dB, unlike the usual linear quality factor:
// https://www.w3.org/TR/webaudio/#dom-biquadfilternode-q
export function connectCaptureBandlimit(
  context: BaseAudioContext, input: AudioNode, output: AudioNode,
): BiquadFilterNode[] {
  if (context.sampleRate <= 16_000) { input.connect(output); return []; }
  const filters: BiquadFilterNode[] = [];
  let previous = input;
  // Eighth-order Butterworth: flat speech passband, no added gain. The transition
  // band must lie below the 8 kHz Nyquist limit of the durable PCM format.
  for (let section = 0; section < 4; section += 1) {
    const filter = context.createBiquadFilter();
    filter.type = "lowpass";
    filter.frequency.value = 6400;
    const quality = 1 / (2 * Math.cos((2 * section + 1) * Math.PI / 16));
    filter.Q.value = 20 * Math.log10(quality);
    previous.connect(filter);
    filters.push(filter);
    previous = filter;
  }
  previous.connect(output);
  return filters;
}
