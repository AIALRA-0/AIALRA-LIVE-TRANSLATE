import { useMemo } from "react";
import { buildCourseChapters } from "./courseChapters";
import type { TimelineItem } from "./types";

export function CourseOutline({ items, onSeek }: { items: TimelineItem[]; onSeek: (capturedAtMs: number) => void }) {
  const chapters = useMemo(() => buildCourseChapters(items), [items]);
  return <section className="course-outline" aria-label="课程章节与观点树">
    <header><h2>课程章节</h2><span>{chapters.length} 个主题章节</span></header>
    {chapters.length ? chapters.map((chapter) => <details key={chapter.firstParagraphId}>
      <summary><span>{chapter.title}</span><small>{chapter.groups.length} 个内容组</small></summary>
      <div className="course-outline-detail">
        <p>{chapter.summary || "该章节已与原文绑定，讲解仍在生成"}</p>
        {chapter.audioStartMs && <button type="button" onClick={() => onSeek(chapter.audioStartMs!)}>从本章开始回听</button>}
        <h3>观点与术语</h3>
        <ul>{chapter.groups.flatMap((group, groupIndex) => {
          const summary = group.sections?.find((section) => section.label === "当前内容组总结");
          const terms = group.sections?.filter((section) => section.label.startsWith("知识补充")) ?? [];
          return [<li key={`${group.id}:summary`}><strong>内容组 {groupIndex + 1}</strong><span>{summary?.text ?? "正在整理"}</span></li>,
            ...terms.map((term, termIndex) => <li key={`${group.id}:term:${termIndex}`}><strong>{term.label.replace("知识补充 · ", "")}</strong><span>{term.text}</span></li>)];
        })}</ul>
      </div>
    </details>) : <p>录音会先保存原文和译文；形成完整主题并完成讲解后，章节会在这里出现。</p>}
  </section>;
}
