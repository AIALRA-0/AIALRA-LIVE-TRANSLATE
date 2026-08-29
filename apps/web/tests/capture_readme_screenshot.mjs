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
    await auditPage.goto(baseUrl);
    await auditPage.locator(".recent-button").first().click();
    await auditPage.getByText("课程会话已安全结束，模型队列已排空").waitFor({ timeout: 30_000 });
    const overflow = await auditPage.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    if (overflow > 0) throw new Error(`${viewport.width}px ${colorScheme} overflowed by ${overflow}px`);
    if (consoleProblems.length) throw new Error(`${viewport.width}px ${colorScheme} console problems: ${consoleProblems.join(" | ")}`);
  } finally {
    await auditContext.close();
  }
}

try {
  await page.goto(baseUrl);
  await page.locator(".recent-button").first().click();
  await page.getByText("课程会话已安全结束，模型队列已排空").waitFor({ timeout: 30_000 });
  await page.locator(".readweave-card").getByText("已同步", { exact: true }).waitFor({ timeout: 120_000 });
  await page.locator(".readweave-preview > div p").first().waitFor({ timeout: 30_000 });
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
