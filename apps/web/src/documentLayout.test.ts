import { describe, expect, it } from "vitest";
import { focusedParagraphId, insightForParagraph, mainDocumentItems } from "./documentLayout";
import type { TimelineItem } from "./types";

const item = (kind: TimelineItem["kind"], id: string, evidenceIds: string[] = [], occurredAt = "2026-09-13T00:00:00Z"): TimelineItem => ({
  kind, id, title: id, body: id, evidenceIds, occurredAt,
});

describe("course reading columns", () => {
  const items = [item("paragraph", "first"), item("insight", "teaching", ["first"]), item("asset", "slides"), item("session-summary", "summary"), item("status", "retry"), item("preview", "live")];

  it("keeps extra content out of the speech column on every reading route", () => {
    for (const section of [null, "transcript", "overview", "explanations"]) {
      expect(mainDocumentItems(items, section, "bilingual", "").map(({ id }) => id)).toEqual(["first", "live"]);
    }
    expect(mainDocumentItems(items, "assets", "bilingual", "").map(({ id }) => id)).toEqual(["slides"]);
    expect(mainDocumentItems(items, "user-notes", "bilingual", "")).toEqual([]);
  });

  it("respects translation-only preview hiding and transcript search", () => {
    expect(mainDocumentItems(items, "transcript", "translation", "first").map(({ id }) => id)).toEqual(["first"]);
    expect(mainDocumentItems(items, null, "translation", "").map(({ id }) => id)).toEqual(["first"]);
  });

  it("shows teaching only for its cited paragraph, never a neighboring one", () => {
    expect(insightForParagraph(items, "first")?.id).toBe("teaching");
    expect(insightForParagraph(items, "other")).toBeUndefined();
    const twoGroups = [item("insight", "first-group", ["first"]), item("insight", "second-group", ["second"]), item("insight", "first-revision", ["first"])];
    expect(insightForParagraph(twoGroups, "first")?.id).toBe("first-revision");
    expect(insightForParagraph(twoGroups, "second")?.id).toBe("second-group");
    const newestFirst = [
      item("insight", "quality-repair", ["first", "second", "third", "fourth"], "2026-09-15T00:00:00Z"),
      item("insight", "legacy-tail", ["first", "second"], "2026-09-13T00:00:00Z"),
    ];
    expect(insightForParagraph(newestFirst, "first")?.id).toBe("quality-repair");
  });

  it("tracks the paragraph crossing the reading position while scrolling", () => {
    const positions = [{ id: "first", bottom: 100 }, { id: "second", bottom: 300 }, { id: "third", bottom: 500 }];
    expect(focusedParagraphId(positions, 50)).toBe("first");
    expect(focusedParagraphId(positions, 150)).toBe("second");
    expect(focusedParagraphId(positions, 800)).toBe("third");
    expect(focusedParagraphId([], 150)).toBeNull();
  });
});
