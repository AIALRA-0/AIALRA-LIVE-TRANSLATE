import { describe, expect, it } from "vitest";
import { appendEvent, buildCourseDocument } from "./timeline";
import type { EventEnvelope } from "./types";

// Fixture creation keeps protocol metadata stable across reducer tests.
function event(eventType: string, payload: Record<string, unknown>): EventEnvelope {
  return {
    schema_version: "1.0.0",
    event_id: `evt-${eventType}`,
    session_id: "session-1",
    source_id: "test",
    sequence: 1,
    event_type: eventType,
    captured_at_monotonic_ns: 1,
    captured_at_wall: "2026-08-24T12:00:00Z",
    ingested_at: "2026-08-24T12:00:00Z",
    correlation_id: "corr-1",
    causation_id: null,
    payload,
    content_hash: `sha256:${"0".repeat(64)}`,
  };
}

describe("timeline mapping", () => {
  it("keeps segment and page evidence on explanation cards", () => {
    const items = buildCourseDocument([
      event("explanation.card.created", {
        card_id: "card-1",
        result: {
          summary: "数据前递减少等待。",
          evidence_segment_ids: ["seg-1"],
          asset_page_ids: ["page-2"],
          provider: "local",
        },
      }),
    ]);
    expect(items[0]?.evidenceIds).toEqual(["seg-1", "page-2"]);
  });

  it("pairs translations with their source paragraph", () => {
    const source = event("segment.finalized", { segment_id: "seg-1", text: "attention", provider: "asr" });
    const translation = { ...event("translation.finalized", { segment_id: "seg-1", text: "注意力", provider: "llm" }), event_id: "evt-translation" };
    const [paragraph] = buildCourseDocument([source, translation]);
    expect(paragraph.kind).toBe("paragraph");
    expect(paragraph.original).toBe("attention");
    expect(paragraph.translation).toBe("注意力");
  });

  it("deduplicates a replayed event by event ID", () => {
    const input = event("segment.finalized", { segment_id: "seg-1", text: "hello" });
    const once = appendEvent({ events: [], items: [] }, input);
    const twice = appendEvent(once, input);
    expect(twice.events).toHaveLength(1);
    expect(twice.items).toHaveLength(1);
  });
});
