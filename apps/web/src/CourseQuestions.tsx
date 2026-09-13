import { useState } from "react";
import { api } from "./api";
import type { EventEnvelope } from "./types";

function string(value: unknown): string { return typeof value === "string" ? value : ""; }

export function CourseQuestions({ sessionId, sessionState, events, onEvidence }: {
  sessionId: string;
  sessionState: string;
  events: EventEnvelope[];
  onEvidence: (paragraphId: string) => void;
}) {
  const [question, setQuestion] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState("");
  const asked = events.filter((event) => event.event_type === "course.question.asked").slice(-10);
  const answers = new Map(events.filter((event) => event.event_type === "course.question.answered")
    .map((event) => [string(event.payload.job_id), event]));
  const failures = new Set<string>();
  for (const event of events) {
    const jobId = string(event.payload.job_id);
    if (!jobId) continue;
    if (event.event_type === "model.job.failed" && event.payload.job_type === "course_qa") failures.add(jobId);
    if (event.event_type === "model.job.retry_scheduled" && event.payload.job_type === "course_qa") failures.delete(jobId);
    if (event.event_type === "course.question.answered") failures.delete(jobId);
  }
  const ready = sessionState === "completed" || sessionState === "failed";

  async function submit(value: string): Promise<void> {
    setSubmitting(true); setError("");
    try {
      await api.askCourseQuestion(sessionId, value);
      setQuestion("");
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : "问题未能排队，请重试");
    } finally {
      setSubmitting(false);
    }
  }

  return <section className="side-card course-questions" aria-label="课程问答">
    <h3>课程问答</h3>
    <p>根据本节已保存的转写回答，回答会标出依据段落。证据不足时会明确说明。</p>
    <div className="course-questions-history" aria-live="polite">
      {asked.map((event) => {
        const jobId = string(event.payload.job_id);
        const answer = answers.get(jobId);
        const evidence = Array.isArray(answer?.payload.evidence_segment_ids)
          ? answer.payload.evidence_segment_ids.filter((value): value is string => typeof value === "string") : [];
        return <article key={event.event_id}>
          <strong>{string(event.payload.question)}</strong>
          <p>{answer ? string(answer.payload.answer) : failures.has(jobId) ? "这次回答失败，原课程内容没有变化" : "正在查找课程内容并组织回答…"}</p>
          {evidence.length > 0 && <div className="course-questions-evidence">{evidence.map((id, index) => <button key={id} type="button" onClick={() => onEvidence(id)}>依据 {index + 1}</button>)}</div>}
          {failures.has(jobId) && !answer && <button type="button" disabled={submitting || !ready} onClick={() => void submit(string(event.payload.question))}>重试回答</button>}
        </article>;
      })}
    </div>
    <form onSubmit={(event) => { event.preventDefault(); if (ready && question.trim()) void submit(question.trim()); }}>
      <label>向这节课提问<textarea value={question} maxLength={2000} onChange={(event) => setQuestion(event.target.value)} placeholder="例如：为什么要先做故障模型验证？" /></label>
      <button type="submit" disabled={!ready || submitting || !question.trim()}>{submitting ? "正在提交" : "提问"}</button>
    </form>
    {!ready && <small>录音和后台整理完成后可提问，不影响当前收音。</small>}
    {error && <p role="alert">{error}</p>}
  </section>;
}
