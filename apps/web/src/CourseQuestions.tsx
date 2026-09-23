import { useMemo, useRef, useState } from "react";
import { api } from "./api";
import type { EventEnvelope } from "./types";

function string(value: unknown): string { return typeof value === "string" ? value : ""; }

function AnswerBody({ answer }: { answer: string }) {
  const paragraphs = answer.split(/\n\s*\n|\n(?=(?:直接回答|依据|适用边界|Direct answer|Evidence|Limits)[：:])/i)
    .map((part) => part.trim()).filter(Boolean);
  return <div className="course-question-answer">{paragraphs.map((part, index) => {
    const labelled = part.match(/^(直接回答|依据|适用边界|Direct answer|Evidence|Limits)[：:]\s*([\s\S]*)$/i);
    return <p key={index}>{labelled ? <><strong>{labelled[1]}</strong>{labelled[2]}</> : part}</p>;
  })}</div>;
}

export function CourseQuestions({ sessionId, sessionState, events, hasStableEvidence, initialCardId, onEvidence }: {
  sessionId: string;
  sessionState: string;
  events: EventEnvelope[];
  hasStableEvidence: boolean;
  initialCardId?: string | null;
  onEvidence: (paragraphId: string) => void;
}) {
  const [question, setQuestion] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState("");
  const [parentJobId, setParentJobId] = useState<string | null>(null);
  const [cardId, setCardId] = useState<string | null>(initialCardId ?? null);
  const questionInput = useRef<HTMLTextAreaElement | null>(null);
  const { asked, answers, questionsByJob, failures } = useMemo(() => {
    const questionEvents = events.filter((event) => event.event_type === "course.question.asked").slice(-10);
    const answerEvents = events.filter((event) => event.event_type === "course.question.answered");
    const answerByJob = new Map(answerEvents.map((event) => [string(event.payload.job_id), event]));
    const questionByJob = new Map(questionEvents.map((event) => [string(event.payload.job_id), event]));
    const failedJobs = new Set<string>();
    for (const event of events) {
      const jobId = string(event.payload.job_id);
      if (!jobId) continue;
      if (event.event_type === "model.job.failed" && event.payload.job_type === "course_qa") failedJobs.add(jobId);
      if (event.event_type === "model.job.retry_scheduled" && event.payload.job_type === "course_qa") failedJobs.delete(jobId);
      if (event.event_type === "course.question.answered") failedJobs.delete(jobId);
    }
    return { asked: questionEvents, answers: answerByJob, questionsByJob: questionByJob, failures: failedJobs };
  }, [events]);
  const canAsk = hasStableEvidence && sessionState !== "archived";

  async function submit(value: string): Promise<void> {
    if (!canAsk || !value.trim()) return;
    setSubmitting(true); setError("");
    try {
      await api.askCourseQuestion(sessionId, value, {
        ...(cardId ? { card_id: cardId } : {}),
        ...(parentJobId ? { parent_job_id: parentJobId } : {}),
      });
      setQuestion("");
      setParentJobId(null);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : "问题未能排队，请重试");
    } finally {
      setSubmitting(false);
    }
  }

  return <section className="side-card course-questions" aria-label="课程问答">
    <h3>课程问答</h3>
    <p>根据本节稳定字幕回答，录音进行中也可提问；回答会标出依据段落，证据不足时会明确说明。</p>
    <div className="course-questions-history" aria-live="polite">
      {asked.map((event) => {
        const jobId = string(event.payload.job_id);
        const answer = answers.get(jobId);
        const evidence = Array.isArray(answer?.payload.evidence_segment_ids)
          ? answer.payload.evidence_segment_ids.filter((value): value is string => typeof value === "string") : [];
        return <article key={event.event_id}>
          <strong>{string(event.payload.question)}</strong>
          {answer ? <AnswerBody answer={string(answer.payload.answer)} /> : <p>{failures.has(jobId) ? "这次回答失败，原课程内容没有变化" : "正在查找课程内容并组织回答…"}</p>}
          {evidence.length > 0 && <div className="course-questions-evidence">{evidence.map((id, index) => <button key={id} type="button" onClick={() => onEvidence(id)}>依据 {index + 1}</button>)}</div>}
          {answer && <button type="button" className="follow-up-button" onClick={() => {
            const askedEvent = questionsByJob.get(jobId);
            setParentJobId(jobId);
            setCardId(string(askedEvent?.payload.card_id) || cardId);
            setQuestion("");
            setError("");
            questionInput.current?.focus();
          }}>继续追问</button>}
          {failures.has(jobId) && !answer && <button type="button" disabled={submitting || !canAsk} onClick={() => void submit(string(event.payload.question))}>重试回答</button>}
        </article>;
      })}
    </div>
    <form onSubmit={(event) => { event.preventDefault(); if (canAsk && question.trim()) void submit(question.trim()); }}>
      {parentJobId && <p className="course-question-context" role="status">正在追问上方回答；新问题会带上父回答作为上下文</p>}
      {cardId && <p className="course-question-context" role="status">问题将围绕当前讲解展开</p>}
      <label>向这节课提问<textarea ref={questionInput} disabled={!canAsk || submitting} value={question} maxLength={2000} onChange={(event) => setQuestion(event.target.value)} placeholder={parentJobId ? "继续追问这条回答…" : "例如：为什么要先做故障模型验证？"} /></label>
      <button type="submit" disabled={!canAsk || submitting || !question.trim()}>{submitting ? "正在提交" : parentJobId ? "提交追问" : "提问"}</button>
    </form>
    {!hasStableEvidence && <small>出现已保存的稳定字幕后即可提问；尚未定稿的实时识别不会作为回答依据。</small>}
    {sessionState === "archived" && <small>已归档课程暂不能继续提问。</small>}
    {error && <p role="alert">{error}</p>}
  </section>;
}
