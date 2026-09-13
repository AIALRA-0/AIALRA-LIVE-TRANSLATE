import { expect, it } from "vitest";
import { buildCourseChapters } from "./courseChapters";
import type { TimelineItem } from "./types";

it("keeps capacity continuations inside one chapter and starts a new chapter at a topic change", () => {
  const paragraph = (id: string, time: number): TimelineItem => ({ id, kind: "paragraph", title: "课程段落", body: id, evidenceIds: [id], occurredAt: "2026-09-13T00:00:00Z", audioStartMs: time });
  const group = (id: string, evidenceId: string, reason: TimelineItem["groupReason"]): TimelineItem => ({
    id, kind: "insight", title: "知识补充", body: "", evidenceIds: [evidenceId], occurredAt: "2026-09-13T00:00:00Z",
    groupReason: reason, sections: [{ label: "当前内容组总结", text: `${id} covers one topic` }],
  });
  const chapters = buildCourseChapters([
    paragraph("p1", 1000), paragraph("p2", 2000), paragraph("p3", 3000),
    group("g1", "p1", "capacity_continuation"), group("g2", "p2", "topic_change"),
    group("g3", "p3", "recording_stopped"),
  ]);
  expect(chapters.map((chapter) => chapter.groups.length)).toEqual([2, 1]);
  expect(chapters.map((chapter) => chapter.audioStartMs)).toEqual([1000, 3000]);
});
