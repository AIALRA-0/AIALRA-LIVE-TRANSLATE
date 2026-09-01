// Capture a privacy-reviewed README image from an existing synthetic real-model session.
import { chromium } from "@playwright/test";

const baseUrl = process.env.AIALRA_BROWSER_BASE_URL || "http://127.0.0.1:18787";
const identity = process.env.AIALRA_TEST_SUBJECT;
const output = process.env.AIALRA_SCREENSHOT_OUTPUT;
if (!identity || !output) throw new Error("synthetic identity and screenshot output are required");

const browser = await chromium.launch({ channel: "chrome", headless: true });
const context = await browser.newContext({
  viewport: { width: 1440, height: 1000 },
  deviceScaleFactor: 1,
  colorScheme: "light",
  extraHTTPHeaders: { "X-authentik-uid": identity },
});
const page = await context.newPage();

async function verifyInputContrast(targetPage) {
  const results = await targetPage.locator("input:not([type='hidden']):not([type='file'])").evaluateAll((inputs) => {
    const rgb = (value) => (value.match(/[\d.]+/g) ?? []).slice(0, 3).map(Number);
    const luminance = (value) => {
      const channels = rgb(value).map((channel) => {
        const normalized = channel / 255;
        return normalized <= 0.04045 ? normalized / 12.92 : ((normalized + 0.055) / 1.055) ** 2.4;
      });
      return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
    };
    return inputs.filter((input) => input.getClientRects().length > 0).map((input) => {
      const background = getComputedStyle(input).backgroundColor;
      const foreground = getComputedStyle(input, "::placeholder").color;
      const lighter = Math.max(luminance(background), luminance(foreground));
      const darker = Math.min(luminance(background), luminance(foreground));
      return { label: input.getAttribute("aria-label") ?? input.getAttribute("placeholder"), ratio: (lighter + 0.05) / (darker + 0.05) };
    });
  });
  const failure = results.find((result) => result.ratio < 4.5);
  if (failure) throw new Error(`${failure.label ?? "input"} placeholder contrast is ${failure.ratio.toFixed(2)}:1`);
}

async function verifyLayout(viewport, colorScheme) {
  const auditContext = await browser.newContext({
    viewport,
    deviceScaleFactor: 1,
    colorScheme,
    extraHTTPHeaders: { "X-authentik-uid": identity },
  });
  const auditPage = await auditContext.newPage();
  const consoleProblems = [];
  auditPage.on("console", (message) => {
    if (["error", "warning"].includes(message.type())) consoleProblems.push(`${message.type()}:${message.text()}`);
  });
  try {
    await auditPage.goto(`${baseUrl}/app`);
    await verifyInputContrast(auditPage);
    await auditPage.locator(".overview-card button").first().click();
    await auditPage.getByText("已完成", { exact: true }).last().waitFor({ timeout: 30_000 });
    await auditPage.locator(".course-paragraph").first().waitFor({ timeout: 30_000 });
    const overflow = await auditPage.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    if (overflow > 0) throw new Error(`${viewport.width}px ${colorScheme} overflowed by ${overflow}px`);
    if (consoleProblems.length) throw new Error(`${viewport.width}px ${colorScheme} console problems: ${consoleProblems.join(" | ")}`);
  } finally {
    await auditContext.close();
  }
}

try {
  await page.goto(`${baseUrl}/app`);
  await page.locator(".overview-card button").first().click();
  await page.getByText("已完成", { exact: true }).last().waitFor({ timeout: 30_000 });
  await page.locator(".course-paragraph").first().waitFor({ timeout: 30_000 });
  await page.locator(".insight-block").first().waitFor({ timeout: 30_000 });
  await page.getByText("NVIDIA GeForce RTX 4080", { exact: true }).waitFor({ timeout: 30_000 });
  await page.locator(".readweave-card").getByText("已同步", { exact: true }).waitFor({ timeout: 120_000 });
  await page.locator(".readweave-card > p").first().waitFor({ timeout: 30_000 });
  if (await page.getByText("录音中", { exact: true }).count()) throw new Error("completed session regressed to recording during event replay");
  await page.addStyleTag({ content: ".android-pairing, time, .timeline-card footer code { display: none !important; }" });
  await page.screenshot({ path: output, fullPage: false });
  await verifyLayout({ width: 1440, height: 1000 }, "light");
  await verifyLayout({ width: 1440, height: 1000 }, "dark");
  await verifyLayout({ width: 390, height: 844 }, "light");
  process.stdout.write("README_SCREENSHOT_AND_LAYOUTS_VERIFIED\n");
} finally {
  await context.close();
  await browser.close();
}
