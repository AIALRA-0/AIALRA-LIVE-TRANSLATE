// Low-load runtime soak: health and telemetry only, without recording or model jobs.
import { writeFile } from "node:fs/promises";

const API = (process.env.AIALRA_API_URL || "http://127.0.0.1:8787/api/v1").replace(/\/$/, "");
const SUBJECT = process.env.AIALRA_TEST_SUBJECT || "runtime-low-load-soak";
const PROXY_MARKER = process.env.AIALRA_TEST_PROXY_MARKER === "true";
const HOURS = Number(process.env.AIALRA_RUNTIME_SOAK_HOURS || "24");
const INTERVAL_SECONDS = Number(process.env.AIALRA_RUNTIME_SOAK_INTERVAL_SECONDS || "30");
const RESULT_PATH = process.env.AIALRA_RUNTIME_SOAK_RESULT_PATH;
const EXPECT_WORKER = process.env.AIALRA_RUNTIME_SOAK_EXPECT_WORKER !== "false";

if (!Number.isFinite(HOURS) || HOURS <= 0 || HOURS > 168) {
  throw new Error("AIALRA_RUNTIME_SOAK_HOURS must be between 0 and 168");
}
if (!Number.isFinite(INTERVAL_SECONDS) || INTERVAL_SECONDS < 5 || INTERVAL_SECONDS > 600) {
  throw new Error("AIALRA_RUNTIME_SOAK_INTERVAL_SECONDS must be between 5 and 600");
}

const headers = {
  "X-authentik-uid": SUBJECT,
  ...(PROXY_MARKER ? { "X-Aialra-Auth-Proxy": "1" } : {}),
};
const samples = [];
const failures = [];
const startedAt = Date.now();
const deadline = startedAt + HOURS * 3_600_000;
let consecutiveFailures = 0;

async function sample() {
  const capturedAt = new Date().toISOString();
  try {
    const response = await fetch(`${API}/runtime/status`, { headers, cache: "no-store" });
    const body = await response.json().catch(() => null);
    if (!response.ok || !body || body.status !== "ok") {
      throw new Error(`runtime status ${response.status}`);
    }
    const worker = body.worker || null;
    const telemetry = worker?.model_metadata?.gpu || null;
    const workerOnline = worker?.online === true;
    if (EXPECT_WORKER && !workerOnline) throw new Error("GPU worker is offline");
    const sampleValue = {
      captured_at: capturedAt,
      ok: true,
      queue: body.model_queue || null,
      worker_online: workerOnline,
      worker_id: worker?.id || null,
      gpu: telemetry
        ? {
            utilization_percent: telemetry.utilization_percent,
            memory_used_mib: telemetry.memory_used_mib,
            memory_total_mib: telemetry.memory_total_mib,
            power_w: telemetry.power_w,
            temperature_c: telemetry.temperature_c,
            sampled_at_unix_ms: telemetry.sampled_at_unix_ms,
          }
        : null,
    };
    samples.push(sampleValue);
    consecutiveFailures = 0;
  } catch (error) {
    consecutiveFailures += 1;
    const message = error instanceof Error ? error.message : String(error);
    failures.push({ captured_at: capturedAt, message });
    samples.push({ captured_at: capturedAt, ok: false });
  }
}

while (Date.now() < deadline) {
  await sample();
  await new Promise((resolve) => setTimeout(resolve, Math.min(INTERVAL_SECONDS * 1_000, Math.max(0, deadline - Date.now()))));
}
await sample();

const result = {
  status: consecutiveFailures === 0 && failures.length === 0 ? "PASS" : "FAIL",
  duration_hours: HOURS,
  interval_seconds: INTERVAL_SECONDS,
  samples: samples.length,
  failed_samples: failures.length,
  failures,
  first_sample_at: samples[0]?.captured_at || null,
  last_sample_at: samples.at(-1)?.captured_at || null,
};
const resultText = `${JSON.stringify(result, null, 2)}\n`;
if (RESULT_PATH) await writeFile(RESULT_PATH, resultText, "utf8");
process.stdout.write(resultText);
if (result.status !== "PASS") process.exitCode = 1;
