import { expect, it } from "vitest";
import { courseMarkdown } from "./courseExport";
import type { TimelineItem } from "./types";

it("exports saved source and translation without leaking processing notices into course notes", () => {
  const item = (kind: TimelineItem["kind"], body: string): TimelineItem => ({
    id: kind, kind, title: kind, body, evidenceIds: [], occurredAt: "2026-09-13T00:00:00Z",
  });
  const markdown = courseMarkdown("Synthetic", [
    { ...item("paragraph", "source"), original: "source", translation: "译文", audioStartMs: 1000 },
    item("status", "internal retry notice"), item("session-summary", "课程总结"),
  ]);
  expect(markdown).toContain("source");
  expect(markdown).toContain("译文");
  expect(markdown).toContain("课程总结");
  expect(markdown).not.toContain("internal retry notice");
});

it("exports a corrected source without publishing its stale translation", () => {
  const markdown = courseMarkdown("Synthetic", [{
    id: "p1", kind: "paragraph", title: "段落", body: "corrected", original: "corrected",
    recognizedOriginal: "wrong", translationStale: true, evidenceIds: [],
    occurredAt: "2026-09-13T00:00:00Z",
  }]);
  expect(markdown).toContain("corrected");
  expect(markdown).toContain("原译文待校对");
  expect(markdown).not.toContain("wrong");
});
