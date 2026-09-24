import { describe, expect, it } from "vitest";
import { liveTeachingSections, visualTranslationBlocks } from "./liveReadingModel";
import type { TimelineItem } from "./types";

const paragraph = (id: string): TimelineItem => ({
  id, kind: "paragraph", title: "", body: "", evidenceIds: [], occurredAt: "2026-01-01T00:00:00Z", original: "Synthetic source.",
});

describe("Live reading content", () => {
  it("changes visual blocks without changing translation text", () => {
    const text = "第一句。第二句！第三句？\n最后一句。";
    const blocks = visualTranslationBlocks(text);
    expect(blocks.length).toBeGreaterThan(1);
    expect(blocks.join("")).toBe(text);
    expect(visualTranslationBlocks("")).toEqual([]);
  });

  it("shows teaching once at the final evidence paragraph and omits empty sections", () => {
    const insight: TimelineItem = {
      id: "insight", kind: "insight", title: "", body: "", evidenceIds: ["one", "two", "asset-page"],
      occurredAt: "2026-01-01T00:01:00Z",
      sections: [{ label: "内容讲解", text: "Synthetic explanation." }, { label: "专业术语", text: "无" }],
    };
    const items = [paragraph("one"), paragraph("two"), insight];
    expect(liveTeachingSections(items, "one")).toEqual([]);
    expect(liveTeachingSections(items, "two")).toEqual([{ label: "内容讲解", text: "Synthetic explanation." }]);
    expect(liveTeachingSections([paragraph("one")], "one")).toEqual([]);
  });
});
