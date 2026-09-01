import type { EventEnvelope, TimelineItem } from "./types";

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function object(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? value as Record<string, unknown> : {};
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function evidence(value: unknown): string[] {
  return strings(value).filter(Boolean);
}

// A course document pairs stable source segments with translations and expands structured teaching output.
export function buildCourseDocument(events: EventEnvelope[]): TimelineItem[] {
  const translations = new Map<string, EventEnvelope>();
  for (const event of events) {
    if (event.event_type === "translation.finalized") translations.set(text(event.payload.paragraph_id) || text(event.payload.segment_id), event);
  }
  const hasParagraphs = events.some((event) => event.event_type === "paragraph.finalized");
  const usesInternalFragments = events.some((event) => event.event_type === "segment.finalized" && event.payload.display_mode === "internal_fragment");

  const items: TimelineItem[] = [];
  for (const event of events) {
    const payload = event.payload;
    if (event.event_type === "paragraph.finalized" || (event.event_type === "segment.finalized" && !hasParagraphs && !usesInternalFragments)) {
      const segmentId = text(payload.paragraph_id) || text(payload.segment_id) || event.event_id;
      const translation = translations.get(segmentId);
      const original = text(translation?.payload.source_text) || text(payload.text);
      items.push({
        id: segmentId, kind: "paragraph", title: "课程段落", body: original,
        original, translation: translation ? text(translation.payload.text) : undefined,
        sourceProvider: text(payload.provider), translationProvider: translation ? text(translation.payload.provider) : undefined,
        evidenceIds: [segmentId], occurredAt: event.captured_at_wall,
      });
      continue;
    }
    if (event.event_type === "segment.finalized") continue;
    if (event.event_type === "translation.finalized") continue;

    if (event.event_type === "explanation.card.created") {
      const result = object(payload.result);
      const cardId = text(payload.card_id) || event.event_id;
      const sharedEvidence = [...evidence(result.evidence_segment_ids), ...evidence(result.asset_page_ids)];
      const provider = text(result.provider);
      const sections: NonNullable<TimelineItem["sections"]> = [];
      const summary = text(result.summary);
      if (summary) sections.push({ label: "本段要点", text: summary });
      const contexts = Array.isArray(result.missing_context) ? result.missing_context : [];
      contexts.forEach((entry) => {
        const value = object(entry); const body = text(value.text);
        if (body) sections.push({ label: "背景补充", text: body });
      });
      const terms = Array.isArray(result.rare_terms) ? result.rare_terms : [];
      terms.forEach((entry) => {
        const value = object(entry); const term = text(value.term); const oneLine = text(value.one_line);
        if (term || oneLine) sections.push({ label: term ? `术语 · ${term}` : "术语解释", text: oneLine });
      });
      strings(result.possible_asr_errors).forEach((body) => sections.push({ label: "疑似听写", text: body, tone: "warning" }));
      strings(result.review_questions).forEach((body) => sections.push({ label: "复习问题", text: body, tone: "question" }));
      if (sections.length) items.push({ id: cardId, kind: "insight", title: "知识补充", body: "", sections, evidenceIds: sharedEvidence, occurredAt: event.captured_at_wall, provider });
      continue;
    }

    if (event.event_type === "session.summary.created") {
      const result = object(payload.result);
      const terminology = Array.isArray(result.terminology)
        ? result.terminology.map((entry) => {
          const value = object(entry); const term = text(value.term); const oneLine = text(value.one_line);
          if (!term && !oneLine) return "";
          return `术语：${term}${oneLine ? ` — ${oneLine}` : ""}`;
        }).filter(Boolean)
        : [];
      const body = [text(result.overview), ...strings(result.key_points).map((item) => `• ${item}`), ...terminology, ...strings(result.open_questions).map((item) => `待复习：${item}`)].filter(Boolean).join("\n");
      items.push({ id: text(payload.summary_id) || event.event_id, kind: "session-summary", title: "课程总结", body, evidenceIds: [...evidence(result.evidence_segment_ids), ...evidence(result.asset_page_ids)], occurredAt: event.captured_at_wall, provider: text(result.provider) });
      continue;
    }

    if (event.event_type === "session.summary.failed") {
      items.push({
        id: event.event_id,
        kind: "status",
        title: "课程总结等待重试",
        body: "实时字幕和译文已经保存，最终总结暂未完成，可在右侧重新生成",
        evidenceIds: [],
        occurredAt: event.captured_at_wall,
      });
      continue;
    }

    if (event.event_type === "asset.page.extracted") {
      items.push({ id: text(payload.page_id) || event.event_id, kind: "asset", title: `课件证据 · 第 ${String(payload.page_number ?? "?")} 页`, body: text(payload.text), evidenceIds: [], occurredAt: event.captured_at_wall, provider: text(payload.parser), imageUrl: text(payload.preview_url) || undefined });
      continue;
    }

    if (event.event_type === "model.job.failed" || event.event_type === "model.job.retry_scheduled") {
      items.push({ id: event.event_id, kind: "status", title: event.event_type === "model.job.failed" ? "模型任务暂时不可用" : "真实模型等待恢复", body: "输入已经安全保存，任务会在本机模型恢复后继续处理", evidenceIds: [], occurredAt: event.captured_at_wall });
    }
  }
  return items;
}

// Replay and live delivery can overlap, so event IDs remain the deduplication boundary.
export function appendEvent(state: { events: EventEnvelope[]; items: TimelineItem[] }, event: EventEnvelope): { events: EventEnvelope[]; items: TimelineItem[] } {
  if (state.events.some((candidate) => candidate.event_id === event.event_id)) return state;
  const events = [...state.events, event];
  return { events, items: buildCourseDocument(events) };
}
