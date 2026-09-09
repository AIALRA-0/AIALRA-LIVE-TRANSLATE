// Actual Edge native audio graph + the shipped TS resampler, synthetic tones only.
import { chromium } from "@playwright/test";
import assert from "node:assert/strict";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import ts from "typescript";

const root = path.resolve(import.meta.dirname, "..");
const audio = await readFile(path.join(root, "src/audio.ts"), "utf8");
const source = ts.createSourceFile("audio.ts", audio, ts.ScriptTarget.Latest, true);
const selected = source.statements.filter(node =>
  ((ts.isClassDeclaration(node) || ts.isFunctionDeclaration(node)) && ["StreamingResampler", "resample"].includes(node.name?.text)) ||
  (ts.isVariableStatement(node) && node.declarationList.declarations.some(item => item.name.getText(source) === "TARGET_SAMPLE_RATE")));
assert.equal(selected.length, 3);
const code = selected.map(node => node.getText(source)).join("\n") + "\n" + await readFile(path.join(root, "src/captureBandlimit.ts"), "utf8");
const javascript = ts.transpileModule(code, {compilerOptions: {target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext}}).outputText;
const browser = await chromium.launch({channel: "msedge", headless: true});
try {
  const page = await browser.newPage();
  const results = await page.evaluate(async (moduleSource) => {
    const url = URL.createObjectURL(new Blob([moduleSource], {type:"text/javascript"}));
    const {resample, connectCaptureBandlimit} = await import(url);
    URL.revokeObjectURL(url);
    const rows = [];
    const rms = samples => Math.sqrt(samples.slice(1000).reduce((sum, value) => sum + value * value, 0) / (samples.length - 1000));
    for (const rate of [44100, 48000]) {
      for (const frequency of [1000, 4000, 10000, 12000]) {
        const context = new OfflineAudioContext(1, rate, rate);
        const buffer = context.createBuffer(1, rate, rate);
        const input = buffer.getChannelData(0);
        for (let i = 0; i < input.length; i++) input[i] = Math.sin(2 * Math.PI * frequency * i / rate);
        const node = context.createBufferSource(); node.buffer = buffer;
        const filters = connectCaptureBandlimit(context, node, context.destination);
        node.start();
        const started = performance.now();
        const rendered = await context.startRendering();
        const filtered = resample(rendered.getChannelData(0), rate);
        const old = resample(input, rate);
        rows.push({rate, frequency, output_samples:filtered.length, input_samples:rate, filter_count:filters.length,
          gain_db:20*Math.log10(rms(filtered)/Math.SQRT1_2), versus_unfiltered_db:20*Math.log10(rms(filtered)/rms(old)),
          render_ms:performance.now()-started});
      }
    }
    return rows;
  }, javascript);
  for (const row of results) {
    assert.equal(row.output_samples, 16000, "filtering preserves sample duration");
    assert.equal(row.filter_count, 4);
    if (row.frequency <= 4000) assert.ok(Math.abs(row.gain_db) < 0.6, "speech passband remains flat");
    else assert.ok(row.versus_unfiltered_db < -35, "out-of-band folding attenuated by at least 35 dB");
  }
  if (process.env.AIALRA_EVIDENCE_DIR) {
    await mkdir(process.env.AIALRA_EVIDENCE_DIR, {recursive:true});
    await writeFile(path.join(process.env.AIALRA_EVIDENCE_DIR,"audio-bandlimit.json"), JSON.stringify({passed:true,results},null,2));
  }
  console.log(JSON.stringify({passed:true,results}));
} finally { await browser.close(); }
