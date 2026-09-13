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
