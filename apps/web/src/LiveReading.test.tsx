// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { LiveReading } from "./LiveReading";
import type { TimelineItem } from "./types";

afterEach(() => { cleanup(); vi.restoreAllMocks(); window.localStorage.clear(); });

function paragraph(id: string, index: number): TimelineItem {
  return { id, kind: "paragraph", title: "", body: "", evidenceIds: [], occurredAt: `2026-09-24T12:00:0${index}Z`, original: `English ${id}`, translation: `中文 ${id}` };
}

it("appends paragraphs chronologically and preserves a reader's upward scroll until return to latest", () => {
  Object.defineProperty(HTMLElement.prototype, "scrollHeight", { configurable: true, get: () => 1000 });
  Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 300 });
  const scrollTo = vi.fn(function (this: HTMLElement, options: ScrollToOptions) { this.scrollTop = Number(options.top); });
  Object.defineProperty(HTMLElement.prototype, "scrollTo", { configurable: true, value: scrollTo });
  const props = { sessionId: "session", wholeAudioReady: false, onSeek: vi.fn(), finished: false, onReview: vi.fn() };
  const { rerender, container } = render(<LiveReading {...props} items={[paragraph("p1", 1), paragraph("p2", 2)]} />);
  const captions = () => [...container.querySelectorAll(".live-caption")];
  expect(captions().map((node) => node.textContent)).toEqual([expect.stringContaining("中文 p1"), expect.stringContaining("中文 p2")]);
  expect(scrollTo).toHaveBeenCalled();
  const scroller = container.querySelector(".live-reading-scroll") as HTMLElement;
  scrollTo.mockClear();
  scroller.scrollTop = 100;
  fireEvent.scroll(scroller);
  rerender(<LiveReading {...props} items={[paragraph("p1", 1), paragraph("p2", 2), paragraph("p3", 3)]} />);
  expect(captions().at(-1)?.textContent).toContain("中文 p3");
  expect(scrollTo).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: /有新内容/ })).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: /有新内容/ }));
  expect(scrollTo).toHaveBeenCalled();
  expect(scroller.scrollTop).toBe(1000);
});
