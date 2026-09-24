import { insightForParagraph } from "./documentLayout";
import type { TimelineItem } from "./types";

export function visualTranslationBlocks(text: string): string[] {
  if (!text) return [];
  const sentences: string[] = [];
  let start = 0;
  for (let index = 0; index < text.length; index += 1) {
    if (!/[。！？!?\n]/.test(text[index])) continue;
    if (index + 1 < text.length && /[。！？!?]/.test(text[index + 1])) continue;
    sentences.push(text.slice(start, index + 1));
    start = index + 1;
  }
  if (start < text.length) sentences.push(text.slice(start));
  const blocks: string[] = [];
  for (let index = 0; index < sentences.length; index += 2) {
    blocks.push(sentences.slice(index, index + 2).join(""));
  }
  return blocks;
}

export function liveTeachingSections(items: TimelineItem[], paragraphId: string): NonNullable<TimelineItem["sections"]> {
  const insight = insightForParagraph(items, paragraphId);
  if (!insight) return [];
  const paragraphIds = new Set(items.filter((item) => item.kind === "paragraph").map((item) => item.id));
  if (insight.evidenceIds.filter((id) => paragraphIds.has(id)).at(-1) !== paragraphId) return [];
  return (insight.sections ?? []).filter((section) =>
    Boolean(section.text.trim()) && !/^(?:无|暂无)[。.]?$/.test(section.text.trim()));
}
