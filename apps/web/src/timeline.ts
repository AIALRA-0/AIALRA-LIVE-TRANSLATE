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

function cleanTranslationDisplay(value: unknown): string {
  if (typeof value !== "string") return "";
  const labels = [
    "previous context for terminology only:", "translated text:", "text to translate:",
    "source language:", "source_language:", "target language:", "target_language:",
    "terminology:", "glossary:", "translation:", "源语言：", "源语言:", "目标语言：",
    "目标语言:", "术语背景：", "术语背景:", "之前的术语背景仅用于说明：", "之前的术语背景仅用于说明:",
    "术语：", "术语:", "翻译后的文本：", "翻译后的文本:", "翻译后文本：", "翻译后文本:",
    "译文：", "译文:",
  ].sort((left, right) => right.length - left.length);
  const contentLabels = new Set([
    "translated text:", "translation:", "翻译后的文本：", "翻译后的文本:",
    "翻译后文本：", "翻译后文本:", "译文：", "译文:",
  ]);
  const lines = value.trim().split(/\r?\n/);
  const cleaned: string[] = [];
  let leading = true;
  for (const line of lines) {
    if (leading) {
      const normalized = line.trim().toLocaleLowerCase();
      const label = labels.find((candidate) => normalized.startsWith(candidate));
      if (label) {
        const remainder = contentLabels.has(label) ? line.trim().slice(label.length).trimStart() : "";
        if (remainder) cleaned.push(remainder);
        continue;
      }
      if (!line.trim()) continue;
      leading = false;
    }
    cleaned.push(line);
  }
  return cleaned.join("\n").trim();
}

// A course document pairs stable source segments with translations and expands structured teaching output.
export function buildCourseDocument(events: EventEnvelope[]): TimelineItem[] {
  const translations = new Map<string, EventEnvelope>();
  for (const event of events) {
    if (event.event_type === "translation.finalized") translations.set(text(event.payload.paragraph_id) || text(event.payload.segment_id), event);
  }
  const hasParagraphs = events.some((event) => event.event_type === "paragraph.finalized");
  const usesInternalFragments = events.some((event) => event.event_type === "segment.finalized" && event.payload.display_mode === "internal_fragment");
  const activeStages = new Map<string, EventEnvelope>();

  const items: TimelineItem[] = [];
  for (const event of events) {
    const payload = event.payload;
    if (event.event_type === "model.job.stage") {
      const jobId = text(payload.job_id);
      const stage = text(payload.stage);
      if (jobId && ["model_loading", "inferring", "retrying", "committing"].includes(stage)) activeStages.set(jobId, event);
      continue;
    }
    if (event.event_type === "model.job.completed" || event.event_type === "model.job.failed") {
      const jobId = text(payload.job_id);
      if (jobId) activeStages.delete(jobId);
      if (event.event_type === "model.job.completed") continue;
    }
    if (event.event_type === "model.job.retry_scheduled") {
      const jobId = text(payload.job_id);
      if (jobId) activeStages.delete(jobId);
    }
    if (event.event_type === "paragraph.finalized" || (event.event_type === "segment.finalized" && !hasParagraphs && !usesInternalFragments)) {
      const segmentId = text(payload.paragraph_id) || text(payload.segment_id) || event.event_id;
      const translation = translations.get(segmentId);
      const original = cleanTranslationDisplay(translation?.payload.source_text) || cleanTranslationDisplay(payload.text);
      items.push({
        id: segmentId, kind: "paragraph", title: "课程段落", body: original,
        original, translation: translation ? cleanTranslationDisplay(translation.payload.text) : undefined,
        translationMode: translation?.payload.translation_mode === "same_language" ? "same_language" : undefined,
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
      const summary = text(result.paragraph_summary) || text(result.summary);
      if (summary) sections.push({ label: "当前内容组总结", text: summary });
      const terms = Array.isArray(result.terms) ? result.terms : Array.isArray(result.rare_terms) ? result.rare_terms : [];
      terms.forEach((entry) => {
        const value = object(entry); const term = text(value.term); const explanation = text(value.explanation) || text(value.one_line);
        if (term || explanation) sections.push({ label: term ? `知识补充 · ${term}` : "知识补充", text: explanation });
      });
      if (sections.length) items.push({ id: cardId, kind: "insight", title: "知识补充", body: "", sections, evidenceIds: sharedEvidence, occurredAt: event.captured_at_wall, provider });
      continue;
    }

    if (event.event_type === "session.completed" && payload.summary_pending === true) {
      items.push({
        id: event.event_id,
        kind: "status",
        title: "实时结果已完成，课程总结生成中",
        body: "录音、字幕和译文已经保存；总结完成后会自动出现在这里。",
        evidenceIds: [],
        occurredAt: event.captured_at_wall,
      });
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
      const body = [text(result.overview), ...strings(result.key_points).map((item) => `• ${item}`), ...terminology].filter(Boolean).join("\n");
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
      const errorKind = text(payload.error_kind);
      const materialFailure = errorKind === "material_parse_failed";
      items.push({
        id: event.event_id,
        kind: "status",
        title: materialFailure ? "材料解析失败，讲解未执行" : text(payload.job_type) === "translate" ? "翻译暂时未跟上" : event.event_type === "model.job.failed" ? "模型任务暂时不可用" : "真实模型等待恢复",
        body: materialFailure ? "材料已经保存，但解析没有完成；请重新上传后再确认排队。" : text(payload.job_type) === "translate" ? "原文和音频已经保存；这一段翻译会在模型恢复后重试，不影响后续录音。" : "输入已经安全保存，任务会在本机模型恢复后继续处理",
        evidenceIds: [],
        occurredAt: event.captured_at_wall,
      });
    }
  }
  const stageTitles: Record<string, string> = {
    model_loading: "模型准备中",
    inferring: "模型正在推理",
    retrying: "模型连接中断，正在重试",
    committing: "正在保存模型结果",
  };
  for (const event of activeStages.values()) {
    const payload = event.payload;
    const stage = text(payload.stage);
    if (!stageTitles[stage]) continue;
    const elapsed = typeof payload.elapsed_ms === "number" && Number.isFinite(payload.elapsed_ms) && payload.elapsed_ms >= 0
      ? `已持续约 ${Math.ceil(payload.elapsed_ms / 1_000)} 秒。`
      : "结果完成后会自动显示在课程文档中。";
    items.push({
      id: `model-stage-${text(payload.job_id)}`,
      kind: "status",
      statusTone: stage === "retrying" ? "warning" : "neutral",
      title: stageTitles[stage],
      body: `${elapsed} 音频保存不受模型处理影响。`,
      evidenceIds: [],
      occurredAt: event.captured_at_wall,
    });
  }
  return items;
}

// Processing and retry states belong to the control/status surfaces, never to the
// transcript document. Keeping this boundary explicit prevents mobile layouts
// from rendering backend progress events as if they were recognized speech.
export function isRenderableDocumentItem(item: TimelineItem): boolean {
  return item.kind !== "status";
}

// Replay and live delivery can overlap, so event IDs remain the deduplication boundary.
export function appendEvent(state: { events: EventEnvelope[]; items: TimelineItem[] }, event: EventEnvelope): { events: EventEnvelope[]; items: TimelineItem[] } {
  if (state.events.some((candidate) => candidate.event_id === event.event_id)) return state;
  const events = [...state.events, event];
  return { events, items: buildCourseDocument(events) };
}
