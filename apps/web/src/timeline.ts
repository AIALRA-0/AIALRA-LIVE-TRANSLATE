import type { EventEnvelope, TimelineItem } from "./types";

// String helpers reject non-string model output before it reaches the visible timeline.
function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function stringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

// Each event becomes a view item whose evidence IDs remain clickable and exportable.
export function eventToTimelineItem(event: EventEnvelope): TimelineItem | null {
  const payload = event.payload;
  if (event.event_type === "segment.finalized") {
    return {
      id: text(payload.segment_id) || event.event_id,
      kind: "segment",
      title: "原文",
      body: text(payload.text),
      evidenceIds: [],
      occurredAt: event.captured_at_wall,
      provider: text(payload.provider),
    };
  }
  if (event.event_type === "translation.finalized") {
    return {
      id: text(payload.translation_id) || event.event_id,
      kind: "translation",
      title: "稳定译文",
      body: text(payload.text),
      evidenceIds: [text(payload.segment_id)].filter(Boolean),
      occurredAt: event.captured_at_wall,
      provider: text(payload.provider),
    };
  }
  if (event.event_type === "explanation.card.created") {
    const result =
      payload.result && typeof payload.result === "object"
        ? (payload.result as Record<string, unknown>)
        : payload;
    return {
      id: text(payload.card_id) || event.event_id,
      kind: "explanation",
      title: "补充讲解",
      body: text(result.summary),
      evidenceIds: [
        ...stringList(result.evidence_segment_ids),
        ...stringList(result.asset_page_ids),
      ],
      occurredAt: event.captured_at_wall,
      provider: text(result.provider),
    };
  }
  if (event.event_type === "asset.page.extracted") {
    return {
      id: text(payload.page_id) || event.event_id,
      kind: "asset",
      title: `材料第 ${String(payload.page_number ?? "?")} 页 · ${text(payload.title)}`,
      body: text(payload.text),
      evidenceIds: [],
      occurredAt: event.captured_at_wall,
      provider: text(payload.parser),
      imageUrl: text(payload.preview_url) || undefined,
    };
  }
  if (event.event_type === "model.job.failed") {
    return {
      id: event.event_id,
      kind: "status",
      title: "模型任务暂时不可用",
      body: "音频已安全保存，可在模型恢复后重新处理。",
      evidenceIds: [],
      occurredAt: event.captured_at_wall,
    };
  }
  if (event.event_type === "model.job.retry_scheduled") {
    return {
      id: event.event_id,
      kind: "status",
      title: "真实模型等待恢复",
      body: "输入已保存，任务会在本机 GPU 恢复后自动重试，不会生成占位结果",
      evidenceIds: [],
      occurredAt: event.captured_at_wall,
    };
  }
  return null;
}

// Replay can contain the same event as the live stream, so the reducer deduplicates by event ID.
export function appendEvent(
  state: { events: EventEnvelope[]; items: TimelineItem[] },
  event: EventEnvelope,
): { events: EventEnvelope[]; items: TimelineItem[] } {
  if (state.events.some((candidate) => candidate.event_id === event.event_id)) {
    return state;
  }
  const item = eventToTimelineItem(event);
  return {
    events: [...state.events, event],
    items: item ? [...state.items, item] : state.items,
  };
}
