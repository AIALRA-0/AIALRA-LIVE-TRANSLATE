// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { TeachingSectionsView } from "./App";

afterEach(cleanup);

it("groups glossary entries under one heading while preserving their explanations and section order", () => {
  const { container } = render(<TeachingSectionsView sections={[
    { label: "承上启下", text: "沿用上一段的稳定证据。" },
    { label: "专业术语 · FPGA", text: "可重复配置的逻辑器件。" },
    { label: "内容讲解", text: "说明本段的设计取舍。" },
    { label: "专业术语 · LUT", text: "用于实现组合逻辑的查找表。", backgroundReference: "https://example.test/lut" },
  ]} />);

  const glossary = screen.getByRole("region", { name: "专业术语" });
  expect(within(glossary).getByRole("heading", { name: "专业术语", level: 4 })).toBeInTheDocument();
  const fpgaSummary = within(glossary).getByText("FPGA", { selector: "summary" });
  expect(fpgaSummary).toBeInTheDocument();
  expect(within(glossary).getByText("LUT", { selector: "summary" })).toBeInTheDocument();
  expect(fpgaSummary.closest("details")).not.toHaveAttribute("open");
  fireEvent.click(fpgaSummary);
  expect(fpgaSummary.closest("details")).toHaveAttribute("open");
  expect(within(glossary).queryByText(/专业术语\s*·/)).not.toBeInTheDocument();
  expect(within(glossary).getByText("可重复配置的逻辑器件。")).toBeInTheDocument();
  expect(within(glossary).getByRole("link", { name: "查看背景资料 ↗" })).toHaveAttribute("href", "https://example.test/lut");
  expect([...container.querySelectorAll(".teaching-sections-view > section > strong, .teaching-sections-view > section > h4")].map((node) => node.textContent)).toEqual(["承上启下", "专业术语", "内容讲解"]);
});
