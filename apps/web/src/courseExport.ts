import type { TimelineItem } from "./types";

export function courseMarkdown(title: string, items: TimelineItem[]): string {
  const lines = [`# ${title}`, ""];
  const summary = items.find((item) => item.kind === "session-summary");
  if (summary) lines.push("## 课程总结", "", summary.body, "");
  lines.push("## 逐段转写与翻译", "");
  for (const item of items) {
    if (item.kind !== "paragraph") continue;
    const timestamp = Number.isFinite(item.audioStartMs) ? new Date(item.audioStartMs!).toLocaleString("zh-CN") : item.occurredAt;
    lines.push(`### ${timestamp}${item.speakerLabel ? ` · ${item.speakerLabel}` : ""}`, "", item.original || item.body, "");
    if (item.translation && item.translation !== item.original) lines.push("译文：", "", item.translation, "");
  }
  const insights = items.filter((item) => item.kind === "insight");
  if (insights.length) lines.push("## 内容组总结与知识补充", "");
  for (const [index, insight] of insights.entries()) {
    lines.push(`### 内容组 ${index + 1}`, "");
    for (const section of insight.sections ?? []) lines.push(`**${section.label}**`, "", section.text, "");
  }
  return lines.join("\n").trimEnd() + "\n";
}

export function downloadCourseMarkdown(title: string, items: TimelineItem[]): void {
  const data = new Blob([courseMarkdown(title, items)], { type: "text/markdown;charset=utf-8" });
  const url = URL.createObjectURL(data);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = `${title.replace(/[\\/:*?"<>|]/g, "-").slice(0, 80) || "课程笔记"}.md`;
  anchor.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 10_000);
}
