import type { LanguageView, TimelineItem } from "./types";

// The reading column contains only speech. Teaching, summaries, and materials
// have their own surfaces and must never be interleaved with the transcript.
export function mainDocumentItems(items: TimelineItem[], section: string | null, languageView: LanguageView, search: string): TimelineItem[] {
  if (section === "assets") return items.filter((item) => item.kind === "asset");
  if (section === "user-notes") return [];
  if (section && !["transcript", "overview", "explanations"].includes(section)) return [];
  const query = section === "transcript" ? search.trim().toLocaleLowerCase() : "";
  return items.filter((item) => {
    if (item.kind !== "paragraph" && item.kind !== "preview") return false;
    if (item.kind === "preview" && languageView === "translation") return false;
    return !query || [item.body, item.translation, item.speakerLabel].some((value) => value?.toLocaleLowerCase().includes(query));
  });
}

export function insightForParagraph(items: TimelineItem[], paragraphId: string | null): TimelineItem | undefined {
  if (!paragraphId) return undefined;
  return items
    .filter((item) => item.kind === "insight" && item.evidenceIds.includes(paragraphId))
    .reduce<TimelineItem | undefined>((latest, item) => {
      if (!latest) return item;
      const itemTime = Date.parse(item.occurredAt);
      const latestTime = Date.parse(latest.occurredAt);
      if (itemTime > latestTime) return item;
      if (itemTime === latestTime) return item;
      return latest;
    }, undefined);
}

export function focusedParagraphId(positions: Array<{ id: string; bottom: number }>, anchor: number): string | null {
  let focused: string | null = null;
  for (const position of positions) {
    focused = position.id;
    if (position.bottom >= anchor) break;
  }
  return focused;
}
