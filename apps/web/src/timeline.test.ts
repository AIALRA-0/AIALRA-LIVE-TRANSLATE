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
    const translation = { ...event("translation.finalized", { segment_id: "seg-1", source_text: "Attention uses context.", text: "注意力使用上下文", provider: "llm" }), event_id: "evt-translation" };
    const [paragraph] = buildCourseDocument([source, translation]);
    expect(paragraph.kind).toBe("paragraph");
    expect(paragraph.original).toBe("Attention uses context.");
    expect(paragraph.translation).toBe("注意力使用上下文");
  });

  it("keeps raw acoustic fragments internal and shows one coherent paragraph", () => {
    const fragment = event("segment.finalized", { segment_id: "seg-1", text: "attention", display_mode: "internal_fragment" });
    const paragraph = { ...event("paragraph.finalized", { paragraph_id: "para-1", segment_ids: ["seg-1"], text: "Attention uses context.", provider: "asr" }), event_id: "evt-paragraph" };
    const items = buildCourseDocument([fragment, paragraph]);
    expect(items).toHaveLength(1);
    expect(items[0]?.original).toBe("Attention uses context.");
  });

  it("keeps one teaching block instead of bursting into many cards", () => {
    const [item] = buildCourseDocument([event("explanation.card.created", {
      card_id: "card-2",
      result: { summary: "要点", missing_context: [{ text: "背景" }], rare_terms: [{ term: "token", one_line: "词元" }], review_questions: ["为什么"], evidence_segment_ids: ["para-1"] },
    })]);
    expect(item.kind).toBe("insight");
    expect(item.sections).toHaveLength(4);
  });

  it("shows a retryable summary failure without inventing summary text", () => {
    const [item] = buildCourseDocument([event("session.summary.failed", {
      job_id: "job-summary",
      error_kind: "provider_unavailable",
      manual_retry_available: true,
    })]);
    expect(item.kind).toBe("status");
    expect(item.title).toBe("课程总结等待重试");
    expect(item.body).toContain("最终总结暂未完成");
  });

  it("keeps summary terminology visible as a separate readable line", () => {
    const [item] = buildCourseDocument([event("session.summary.created", {
      summary_id: "summary-1",
      result: {
        overview: "课程概览",
        key_points: ["关键知识点"],
        terminology: [{ term: "attention", one_line: "根据上下文分配权重" }],
        open_questions: [],
        provider: "ollama:qwen2.5:14b-instruct@cuda",
      },
    })]);
    expect(item.kind).toBe("session-summary");
    expect(item.body).toContain("术语：attention — 根据上下文分配权重");
  });

  it("deduplicates a replayed event by event ID", () => {
    const input = event("segment.finalized", { segment_id: "seg-1", text: "hello" });
    const once = appendEvent({ events: [], items: [] }, input);
    const twice = appendEvent(once, input);
    expect(twice.events).toHaveLength(1);
    expect(twice.items).toHaveLength(1);
  });
});
