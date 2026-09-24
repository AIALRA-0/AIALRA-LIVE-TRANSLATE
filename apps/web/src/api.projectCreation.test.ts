import { afterEach, expect, it, vi } from "vitest";
import { api } from "./api";

afterEach(() => vi.unstubAllGlobals());

it("sends the creation intent as the project idempotency header", async () => {
  const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ id: "project-test" }), { status: 201 }));
  vi.stubGlobal("fetch", fetchMock);

  await api.createProject("测试项目", "project-intent-test");

  expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects", {
    method: "POST",
    headers: { "content-type": "application/json", "Idempotency-Key": "project-intent-test" },
    body: JSON.stringify({ title: "测试项目", source_language: "en", target_language: "zh-CN" }),
  });
});
