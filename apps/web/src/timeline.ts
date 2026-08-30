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
    if (event.event_type === "translation.finalized") translations.set(text(event.payload.segment_id), event);
  }

  const items: TimelineItem[] = [];
  for (const event of events) {
    const payload = event.payload;
    if (event.event_type === "segment.finalized") {
      const segmentId = text(payload.segment_id) || event.event_id;
      const translation = translations.get(segmentId);
      items.push({
        id: segmentId, kind: "paragraph", title: "课程段落", body: text(payload.text),
        original: text(payload.text), translation: translation ? text(translation.payload.text) : undefined,
        sourceProvider: text(payload.provider), translationProvider: translation ? text(translation.payload.provider) : undefined,
        evidenceIds: [segmentId], occurredAt: event.captured_at_wall,
      });
      continue;
    }
    if (event.event_type === "translation.finalized") continue;

    if (event.event_type === "explanation.card.created") {
      const result = object(payload.result);
      const cardId = text(payload.card_id) || event.event_id;
      const sharedEvidence = [...evidence(result.evidence_segment_ids), ...evidence(result.asset_page_ids)];
      const provider = text(result.provider);
      const summary = text(result.summary);
      if (summary) items.push({ id: `${cardId}:summary`, kind: "summary", title: "本段总结", body: summary, evidenceIds: sharedEvidence, occurredAt: event.captured_at_wall, provider });
      const contexts = Array.isArray(result.missing_context) ? result.missing_context : [];
      contexts.forEach((entry, index) => {
        const value = object(entry); const body = text(value.text);
        if (body) items.push({ id: `${cardId}:context:${index}`, kind: "context", title: "背景补充", body, evidenceIds: evidence(value.evidence_segment_ids), occurredAt: event.captured_at_wall, provider });
      });
      const terms = Array.isArray(result.rare_terms) ? result.rare_terms : [];
      terms.forEach((entry, index) => {
        const value = object(entry); const term = text(value.term); const oneLine = text(value.one_line);
        if (term || oneLine) items.push({ id: `${cardId}:term:${index}`, kind: "term", title: `术语解释${term ? ` · ${term}` : ""}`, body: oneLine, evidenceIds: [...evidence(value.evidence_segment_ids), ...evidence(value.asset_page_ids)], occurredAt: event.captured_at_wall, provider });
      });
      strings(result.possible_asr_errors).forEach((body, index) => items.push({ id: `${cardId}:asr:${index}`, kind: "asr-warning", title: "疑似听写", body, evidenceIds: sharedEvidence, occurredAt: event.captured_at_wall, provider }));
      strings(result.review_questions).forEach((body, index) => items.push({ id: `${cardId}:review:${index}`, kind: "review", title: "复习问题", body, evidenceIds: sharedEvidence, occurredAt: event.captured_at_wall, provider }));
      continue;
    }

    if (event.event_type === "session.summary.created") {
      const result = object(payload.result);
      const body = [text(result.overview), ...strings(result.key_points).map((item) => `• ${item}`), ...strings(result.open_questions).map((item) => `待复习：${item}`)].filter(Boolean).join("\n");
      items.push({ id: text(payload.summary_id) || event.event_id, kind: "session-summary", title: "课程总结", body, evidenceIds: [...evidence(result.evidence_segment_ids), ...evidence(result.asset_page_ids)], occurredAt: event.captured_at_wall, provider: text(result.provider) });
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
