import { describe, expect, it } from "vitest";
import { appendEvent, buildCourseDocument, isRenderableDocumentItem } from "./timeline";
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
  it("keeps reviewed background separate from course evidence and rejects unsafe links", () => {
    const reference = "https://www.rfc-editor.org/rfc/rfc3385";
    const [item] = buildCourseDocument([event("explanation.card.created", { result: {
      paragraph_summary: "合成说明", evidence_segment_ids: ["p1"], asset_page_ids: [],
      terms: [reference, "javascript:alert(1)", "https://127.0.0.1/", "https://secret@www.rfc-editor.org/"]
        .map((url) => ({ term: "校验", explanation: "背景定义", background_reference: url })),
    } })]);
    expect(item.evidenceIds).toEqual(["p1"]);
    expect(item.sections?.slice(1).map((section) => section.backgroundReference))
      .toEqual([reference, undefined, undefined, undefined]);
  });
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

  it("pairs translations without allowing their echoed source to rewrite recognition", () => {
    const source = event("segment.finalized", { segment_id: "seg-1", text: "attention", provider: "asr" });
    const translation = { ...event("translation.finalized", { segment_id: "seg-1", source_text: "Attention uses context.", text: "注意力使用上下文", provider: "llm" }), event_id: "evt-translation" };
    const [paragraph] = buildCourseDocument([source, translation]);
    expect(paragraph.kind).toBe("paragraph");
    expect(paragraph.original).toBe("attention");
    expect(paragraph.translation).toBe("注意力使用上下文");
  });

  it("removes provider language headers from an older translation event", () => {
    const source = event("paragraph.finalized", { paragraph_id: "para-1", text: "Attention uses context." });
    const translation = event("translation.finalized", {
      paragraph_id: "para-1",
      source_text: "源语言：en\n目标语言：zh-CN\nAttention uses context.",
      text: "源语言：en\n目标语言：zh-CN\n注意力使用上下文。",
    });
    const [paragraph] = buildCourseDocument([source, translation]);
    expect(paragraph.original).toBe("Attention uses context.");
    expect(paragraph.translation).toBe("注意力使用上下文。");
  });

  it("removes explanation labels without dropping translated text on the same line", () => {
    const source = event("paragraph.finalized", { paragraph_id: "para-2", text: "Attention uses context." });
    const translation = event("translation.finalized", {
      paragraph_id: "para-2",
      source_text: "Attention uses context.",
      text: "翻译后的文本：注意力使用上下文。",
    });
    const [paragraph] = buildCourseDocument([source, translation]);
    expect(paragraph.translation).toBe("注意力使用上下文。");
  });

  it("keeps raw acoustic fragments internal and shows one coherent paragraph", () => {
    const fragment = event("segment.finalized", { segment_id: "seg-1", text: "attention", display_mode: "internal_fragment" });
    const paragraph = { ...event("paragraph.finalized", { paragraph_id: "para-1", segment_ids: ["seg-1"], text: "Attention uses context.", provider: "asr" }), event_id: "evt-paragraph" };
    const items = buildCourseDocument([fragment, paragraph]);
    expect(items).toHaveLength(1);
    expect(items[0]?.original).toBe("Attention uses context.");
  });

  it("previews only unconsumed recognition and removes it when the paragraph arrives", () => {
    const fragment = event("segment.finalized", { segment_id: "s1", text: "The value", display_mode: "internal_fragment" });
    const next = { ...fragment, event_id: "second", payload: { ...fragment.payload, segment_id: "s2", text: "is ready." } };
    const diagnostic = event("model.job.stage", { job_id: "j1", stage: "inferring", text: "internal diagnostic" });
    const preview = buildCourseDocument([fragment, next, fragment, diagnostic]).filter(isRenderableDocumentItem);
    expect(preview).toHaveLength(1);
    expect(preview[0].kind).toBe("preview");
    expect(preview[0].original).toBe("The value is ready.");
    expect(preview[0].translation).toBeUndefined();
    const paragraph = event("paragraph.finalized", { paragraph_id: "p1", segment_ids: ["s1", "s2"], text: "The value is ready." });
    const completed = buildCourseDocument([fragment, next, paragraph]).filter(isRenderableDocumentItem);
    expect(completed).toHaveLength(1);
    expect(completed[0].kind).toBe("paragraph");
    const later = { ...fragment, event_id: "later", payload: { ...fragment.payload, segment_id: "s3", text: "A new statement" } };
    expect(buildCourseDocument([fragment, next, paragraph, later]).at(-1)?.original).toBe("A new statement");
  });

  it("does not strip literal language-label speech from source or previews", () => {
    const literal = "源语言：我们正在讨论翻译格式";
    const paragraph = event("paragraph.finalized", { paragraph_id: "p1", text: literal });
    expect(buildCourseDocument([paragraph])[0].original).toBe(literal);
    const fragment = event("segment.finalized", { segment_id: "s1", text: literal, display_mode: "internal_fragment" });
    expect(buildCourseDocument([fragment])[0].original).toBe(literal);
  });

  it("keeps one teaching block instead of bursting into many cards", () => {
    const [item] = buildCourseDocument([event("explanation.card.created", {
      card_id: "card-2",
      result: { paragraph_summary: "要点", terms: [{ term: "token", explanation: "词元" }], evidence_segment_ids: ["para-1"] },
    })]);
    expect(item.kind).toBe("insight");
    expect(item.sections).toHaveLength(2);
    expect(item.sections?.map((section) => section.label)).toEqual(["当前内容组总结", "知识补充 · token"]);
  });

  it("labels capacity continuation without displaying internal grouping events", () => {
    const events = [
      event("content.group.created", {
        paragraph_ids: ["para-1", "para-2"], reason: "capacity_continuation",
      }),
      event("topic.window.checked", { paragraph_ids: ["para-1", "para-2"] }),
      event("explanation.card.created", {
        card_id: "card-topic",
        result: { paragraph_summary: "同主题内容", evidence_segment_ids: ["para-1", "para-2"] },
      }),
    ];
    const items = buildCourseDocument(events);
    expect(items).toHaveLength(1);
    expect(items[0].groupReason).toBe("capacity_continuation");
    expect(items[0].evidenceIds).toEqual(["para-1", "para-2"]);
  });

  it("renders anonymous course labels and retains uncertainty without guessing names", () => {
    const labels = [{ status: "assigned", index: 2 }, { status: "unconfirmed", index: null },
      { status: "assigned", index: 999 }, null];
    const items = buildCourseDocument(labels.map((speaker, i) => event("paragraph.finalized", {
      paragraph_id: `para-speaker-${i}`, text: "Synthetic speech", speaker,
    })));
    expect(items.map((item) => item.speakerLabel)).toEqual([
      "说话人 2", "说话人待确认", "说话人待确认", undefined,
    ]);
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

  it("shows that realtime facts are complete while the summary remains pending", () => {
    const [item] = buildCourseDocument([event("session.completed", { summary_pending: true })]);
    expect(item.title).toBe("实时结果已完成，课程总结生成中");
    expect(item.body).toContain("字幕和译文已经保存");
  });

  it("shows a material parse failure without pretending explanation ran", () => {
    const [item] = buildCourseDocument([event("model.job.failed", {
      job_type: "asset_parse",
      error_kind: "material_parse_failed",
    })]);
    expect(item.kind).toBe("status");
    expect(item.title).toBe("材料解析失败，讲解未执行");
    expect(item.body).toContain("请重新上传后再确认排队");
  });

  it("keeps processing statuses out of the course document", () => {
    const [status] = buildCourseDocument([event("model.job.stage", { job_id: "job-1", stage: "inferring" })]);
    const [paragraph] = buildCourseDocument([event("segment.finalized", { segment_id: "seg-1", text: "hello" })]);
    expect(status.kind).toBe("status");
    expect(isRenderableDocumentItem(status)).toBe(false);
    expect(paragraph.kind).toBe("paragraph");
    expect(isRenderableDocumentItem(paragraph)).toBe(true);
  });

  it("shows only the latest active model stage and removes it after completion", () => {
    const loading = event("model.job.stage", { job_id: "job-1", stage: "model_loading" });
    const inferring = { ...event("model.job.stage", { job_id: "job-1", stage: "inferring", elapsed_ms: 12_000 }), event_id: "evt-inferring" };
    const active = buildCourseDocument([loading, inferring]);
    expect(active).toHaveLength(1);
    expect(active[0]?.title).toBe("模型正在推理");
    expect(active[0]?.body).toContain("12 秒");
    const completed = { ...event("model.job.completed", { job_id: "job-1" }), event_id: "evt-completed" };
    expect(buildCourseDocument([loading, inferring, completed])).toHaveLength(0);
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

  it("does not expose legacy review questions in the course summary", () => {
    const [item] = buildCourseDocument([event("session.summary.created", {
      summary_id: "summary-2",
      result: {
        overview: "课程概览",
        key_points: [],
        terminology: [],
        open_questions: ["不应出现在用户页面"],
      },
    })]);
    expect(item.body).not.toContain("不应出现在用户页面");
  });

  it("deduplicates a replayed event by event ID", () => {
    const input = event("segment.finalized", { segment_id: "seg-1", text: "hello" });
    const once = appendEvent({ events: [], items: [] }, input);
    const twice = appendEvent(once, input);
    expect(twice.events).toHaveLength(1);
    expect(twice.items).toHaveLength(1);
  });

  it("joins preview punctuation and Chinese fragments without inserting stray spaces", () => {
    const fragments = ["Context", ".", "下一个", "概念"].map((value, index) => event("segment.finalized", {
      segment_id: `preview-punctuation-${index}`, display_mode: "internal_fragment", text: value,
    }));
    const [preview] = buildCourseDocument(fragments);
    expect(preview.body).toBe("Context.下一个概念");
  });

  it("preserves same-language content rather than treating literal labels as translator headers", () => {
    const original = "源语言：中文；今天讨论信号";
    const [item] = buildCourseDocument([
      event("paragraph.finalized", {paragraph_id:"same-language",text:original}),
      event("translation.finalized", {paragraph_id:"same-language",text:original,translation_mode:"same_language"}),
    ]);
    expect(item.original).toBe(original);
    expect(item.translation).toBe(original);
    expect(item.translationMode).toBe("same_language");
  });
});
