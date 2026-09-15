import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { TaskdClient } from "~/taskd/client.server";
import { runInboxAction } from "~/taskd/route-actions.server";
import type { TransitionResult } from "~/taskd/types";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

function form(entries: Array<[string, string]>): FormData {
  const f = new FormData();
  for (const [k, v] of entries) f.append(k, v);
  return f;
}

describe("runInboxAction", () => {
  it("approves 1 task with a note (POST /tasks/{id}/approve, expected_status: ready)", async () => {
    const result: TransitionResult = { id: "A1", from: "ready", to: "done", reason: "approved" };
    mock.on("POST", "/api/v1/tasks/A1/approve", (_req, res) => {
      sendJson(res, 200, result);
    });

    const outcomes = await runInboxAction(
      client,
      form([
        ["task_id", "A1"],
        ["intent", "approve"],
        ["expected_status", "ready"],
        ["note", "ok"],
      ]),
    );

    expect(outcomes).toHaveLength(1);
    expect(outcomes[0]).toEqual({ ok: true, intent: "approve", taskId: "A1", result });
    const req = mock.requests.find((r) => r.method === "POST" && r.url === "/api/v1/tasks/A1/approve");
    expect(req).toBeDefined();
    expect(JSON.parse(req?.body ?? "{}")).toEqual({ note: "ok", expected_status: "ready" });
  });

  it("answers a question (POST /tasks/{id}/answer, expected_status: blocked)", async () => {
    const result: TransitionResult = { id: "Q1", from: "blocked", to: "ready", reason: "answered" };
    mock.on("POST", "/api/v1/tasks/Q1/answer", (_req, res) => {
      sendJson(res, 200, result);
    });

    const outcomes = await runInboxAction(
      client,
      form([
        ["task_id", "Q1"],
        ["intent", "answer"],
        ["expected_status", "blocked"],
        ["answer", "staging"],
      ]),
    );

    expect(outcomes).toHaveLength(1);
    expect(outcomes[0]).toEqual({ ok: true, intent: "answer", taskId: "Q1", result });
    const req = mock.requests.find((r) => r.method === "POST" && r.url === "/api/v1/tasks/Q1/answer");
    expect(req).toBeDefined();
    expect(JSON.parse(req?.body ?? "{}")).toEqual({ answer: "staging", expected_status: "blocked" });
  });

  it("posts task_id entries in order, continues after a 409 on the first, and returns all outcomes (no atomicity)", async () => {
    mock.on("POST", "/api/v1/tasks/D1/approve", (_req, res) => {
      sendProblem(res, {
        status: 409,
        code: "invalid_transition",
        detail: "task D1 (kind=execute, status=ready) cannot be approved",
      });
    });
    const okResult: TransitionResult = { id: "D2", from: "draft", to: "ready", reason: "approved" };
    mock.on("POST", "/api/v1/tasks/D2/approve", (_req, res) => {
      sendJson(res, 200, okResult);
    });

    const outcomes = await runInboxAction(
      client,
      form([
        ["task_id", "D1"],
        ["task_id", "D2"],
        ["intent", "approve"],
        ["expected_status", "draft"],
      ]),
    );

    expect(outcomes).toHaveLength(2);
    expect(outcomes[0].ok).toBe(false);
    if (!outcomes[0].ok) {
      expect(outcomes[0].error.conflict).toBe(true);
      expect(outcomes[0].error.code).toBe("invalid_transition");
    }
    expect(outcomes[1]).toEqual({ ok: true, intent: "approve", taskId: "D2", result: okResult });

    const posts = mock.requests.filter((r) => r.method === "POST" && r.url.startsWith("/api/v1/tasks/"));
    expect(posts.map((r) => r.url)).toEqual(["/api/v1/tasks/D1/approve", "/api/v1/tasks/D2/approve"]);
  });

  it("throws react-router's data() with status 400 when task_id is missing", async () => {
    // `data({...}, {status:400})`（react-router）は `DataWithResponseInit`（シリアライズ前の中間表現）であり
    // `instanceof Response` にはならない（test/unit/tasks.detail.action.test.ts の readIntent のテストと同じ）。
    let thrown: unknown;
    try {
      await runInboxAction(client, form([["intent", "approve"]]));
    } catch (e) {
      thrown = e;
    }

    expect(thrown).not.toBeInstanceOf(Response);
    expect(thrown).toMatchObject({
      type: "DataWithResponseInit",
      init: { status: 400 },
      data: { error: "task_id is required" },
    });
  });
});
