import type { TimelineItem } from "./types";

export interface CourseChapter {
  title: string;
  summary: string;
  audioStartMs?: number;
  firstParagraphId: string;
  groups: TimelineItem[];
}

export function buildCourseChapters(items: TimelineItem[]): CourseChapter[] {
  const paragraphs = items.filter((item) => item.kind === "paragraph");
  const positions = new Map(paragraphs.map((item, index) => [item.id, index]));
  const byId = new Map(paragraphs.map((item) => [item.id, item]));
  const groups = items.filter((item) => item.kind === "insight" && item.evidenceIds.some((id) => positions.has(id)))
    .sort((left, right) => Math.min(...left.evidenceIds.map((id) => positions.get(id) ?? Infinity))
      - Math.min(...right.evidenceIds.map((id) => positions.get(id) ?? Infinity)));
  const chapters: CourseChapter[] = [];
  let previous: TimelineItem | undefined;
  for (const group of groups) {
    const firstId = group.evidenceIds.find((id) => positions.has(id));
    if (!firstId) continue;
    const summary = group.sections?.find((section) => section.label === "当前内容组总结")?.text ?? "";
    if (!chapters.length || previous?.groupReason === "topic_change") {
      const heading = summary.split(/[。！？.!?\n]/)[0].trim();
      chapters.push({
        title: heading ? `${chapters.length + 1} · ${heading.slice(0, 52)}` : `第 ${chapters.length + 1} 章`,
        summary, audioStartMs: byId.get(firstId)?.audioStartMs,
        firstParagraphId: firstId, groups: [],
      });
    }
    chapters.at(-1)?.groups.push(group);
    previous = group;
  }
  return chapters;
}
