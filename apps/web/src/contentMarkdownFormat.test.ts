// @vitest-environment jsdom
import { expect, it } from "vitest";
import { normalizeContentMarkdown, safeContentHtml, safeSourceUrl } from "./contentMarkdownFormat";

it("renders teaching markdown without leaving syntax visible", () => {
  const html = safeContentHtml("## 核心\n\n**SPS** *处理器*\n\n- 一\n- 二\n\n> 需核实\n\n`code`\n\n```text\na < b\n```\n\n[参考](https://example.test/a)");
  const root = document.createElement("div");
  root.innerHTML = html;
  expect(root.querySelector("h2")?.textContent).toBe("核心");
  expect(root.querySelector("strong")?.textContent).toBe("SPS");
  expect(root.querySelector("em")?.textContent).toBe("处理器");
  expect(root.querySelectorAll("li")).toHaveLength(2);
  expect(root.querySelector("blockquote")?.textContent).toContain("需核实");
  expect(root.querySelector("pre code")?.textContent).toContain("a < b");
  expect(root.querySelector("a")?.getAttribute("href")).toBe("https://example.test/a");
});

it("does not turn list entries into headings and blocks unsafe markup", () => {
  expect(normalizeContentMarkdown("1. 概念\n\n这里解释内容。") ).toContain("## 1. 概念");
  expect(normalizeContentMarkdown("1. 一\n2. 二")).toBe("1. 一\n2. 二");
  const html = safeContentHtml("<script>alert(1)</script><img src=x onerror=alert(2)> [点我](javascript:alert(3))");
  const root = document.createElement("div");
  root.innerHTML = html;
  expect(root.querySelector("script, img, [onerror], a[href^='javascript:']")).toBeNull();
  expect(safeSourceUrl("javascript:alert(1)")).toBeNull();
  expect(safeSourceUrl("https://example.test/source")).toBe("https://example.test/source");
});
