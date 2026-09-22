// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { CourseQuestions } from "./CourseQuestions";
import type { EventEnvelope } from "./types";

const { askCourseQuestion } = vi.hoisted(() => ({ askCourseQuestion: vi.fn() }));
vi.mock("./api", () => ({ api: { askCourseQuestion } }));

afterEach(() => {
  cleanup();
  askCourseQuestion.mockReset();
});

const event = (eventId: string, eventType: string, payload: Record<string, unknown>): EventEnvelope => ({
  schema_version: "1", event_id: eventId, session_id: "session-test", source_id: "test", sequence: 1,
  event_type: eventType, captured_at_monotonic_ns: 1, captured_at_wall: "2026-09-22T00:00:00Z",
  ingested_at: "2026-09-22T00:00:00Z", correlation_id: eventId, causation_id: null, payload, content_hash: eventId,
});

it("allows questions during recording once a stable paragraph exists and sends the teaching card context", async () => {
  askCourseQuestion.mockResolvedValue({ job_id: "job-new", status: "queued" });
  render(<CourseQuestions sessionId="session-test" sessionState="recording" events={[]} hasStableEvidence initialCardId="card-current" onEvidence={vi.fn()} />);

  fireEvent.change(screen.getByRole("textbox", { name: "向这节课提问" }), { target: { value: "这组内容的结论是什么？" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "提问" })); });

  expect(askCourseQuestion).toHaveBeenCalledWith("session-test", "这组内容的结论是什么？", { card_id: "card-current" });
});

it("keeps asking disabled before any stable paragraph exists", () => {
  render(<CourseQuestions sessionId="session-test" sessionState="recording" events={[]} hasStableEvidence={false} onEvidence={vi.fn()} />);

  expect(screen.getByRole("textbox", { name: "向这节课提问" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "提问" })).toBeDisabled();
  expect(screen.getByText(/尚未定稿的实时识别不会作为回答依据/)).toBeInTheDocument();
});

it("continues an answered question with its parent job and card context", async () => {
  askCourseQuestion.mockResolvedValue({ job_id: "job-followup", status: "queued" });
  const events = [
    event("asked-1", "course.question.asked", { job_id: "job-parent", question: "为什么？", card_id: "card-parent" }),
    event("answered-1", "course.question.answered", { job_id: "job-parent", answer: "因为有对应证据", evidence_segment_ids: ["paragraph-1"] }),
  ];
  render(<CourseQuestions sessionId="session-test" sessionState="recording" events={events} hasStableEvidence initialCardId="card-current" onEvidence={vi.fn()} />);

  fireEvent.click(screen.getByRole("button", { name: "继续追问" }));
  fireEvent.change(screen.getByRole("textbox", { name: "向这节课提问" }), { target: { value: "能再解释这个原因吗？" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "提交追问" })); });

  expect(askCourseQuestion).toHaveBeenCalledWith("session-test", "能再解释这个原因吗？", { card_id: "card-parent", parent_job_id: "job-parent" });
});

it("renders an evidence-bound answer as readable labelled parts", () => {
  const events = [
    event("asked-1", "course.question.asked", { job_id: "job-parent", question: "故障模型是什么？" }),
    event("answered-1", "course.question.answered", {
      job_id: "job-parent", evidence_segment_ids: ["paragraph-1"],
      answer: "直接回答：故障模型描述待检查的故障。\n依据：本课以黏着故障为例。\n适用边界：通过测试不等于排除全部物理缺陷。",
    }),
  ];
  render(<CourseQuestions sessionId="session-test" sessionState="completed" events={events} hasStableEvidence onEvidence={vi.fn()} />);
  expect(screen.getByText("直接回答")).toBeInTheDocument();
  expect(screen.getByText("依据")).toBeInTheDocument();
  expect(screen.getByText("适用边界")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "依据 1" })).toBeInTheDocument();
});
