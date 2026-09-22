import { afterEach, expect, it, vi } from "vitest";
import { api } from "./api";

afterEach(() => vi.unstubAllGlobals());

it("keeps course questions compatible and supports optional card and parent context", async () => {
  const fetchMock = vi.fn().mockImplementation(() => Promise.resolve(new Response(JSON.stringify({ job_id: "job-test", status: "queued" }), { status: 200 })));
  vi.stubGlobal("fetch", fetchMock);

  await api.askCourseQuestion("session-test", "问题");
  await api.askCourseQuestion("session-test", "追问", { card_id: "card-test", parent_job_id: "job-parent" });

  expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ question: "问题" });
  expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ question: "追问", card_id: "card-test", parent_job_id: "job-parent" });
});
