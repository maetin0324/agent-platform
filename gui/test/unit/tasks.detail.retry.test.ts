import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { TaskdClient } from "~/taskd/client.server";
import { runRetryAction, runTaskAction } from "~/taskd/route-actions.server";
import type { RetryResult } from "~/taskd/types";
import { type MockTaskd, sendJson, sendProblem, startMockTaskd } from "../mock-taskd/server";

/**
 * `POST /tasks/{id}/retry`（Phase 31。実機の事故、2026-09-18。docs/taskd-api-v1.md §3.63）。
 * `runRetryAction` / `runTaskAction` の意図の振り分けは `app/routes/tasks.$id.tsx` の `action` が行う
 * （`intent === "retry"` を先に見る）ので、ここは `route-actions.server.ts` の 2 つの関数を直接確かめる。
 */

let mock: MockTaskd;
let client: TaskdClient;

beforeEach(async () => {
  mock = await startMockTaskd();
  client = new TaskdClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

function form(entries: Record<string, string>): FormData {
  const f = new FormData();
  for (const [k, v] of Object.entries(entries)) f.append(k, v);
  return f;
}

describe("runRetryAction", () => {
  it("posts accept:false by default and returns the new task id + rewired", async () => {
    const result: RetryResult = { task_id: "T2", rewired: ["T3", "T4"] };
    mock.on("POST", "/api/v1/tasks/T1/retry", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ accept: false });
      sendJson(res, 201, result);
    });

    const outcome = await runRetryAction(client, "T1", form({ intent: "retry" }));

    expect(outcome).toEqual({ ok: true, taskId: "T1", result });
  });

  it("posts accept:true when the accept checkbox is checked", async () => {
    const result: RetryResult = { task_id: "T2", rewired: [] };
    mock.on("POST", "/api/v1/tasks/T1/retry", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ accept: true });
      sendJson(res, 201, result);
    });

    const outcome = await runRetryAction(client, "T1", form({ intent: "retry", accept: "true" }));

    expect(outcome).toEqual({ ok: true, taskId: "T1", result });
  });

  it("returns a 404 ActionError for a missing task (not thrown)", async () => {
    mock.on("POST", "/api/v1/tasks/T1/retry", (_req, res) => {
      sendProblem(res, { status: 404, code: "task_not_found", detail: "task not found: T1" });
    });

    const outcome = await runRetryAction(client, "T1", form({ intent: "retry" }));

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected failure");
    expect(outcome.taskId).toBe("T1");
    expect(outcome.error.status).toBe(404);
    expect(outcome.error.code).toBe("task_not_found");
    expect(outcome.error.conflict).toBe(false);
  });

  it("returns a conflict ActionError for 409 invalid_transition (task is not failed/cancelled)", async () => {
    mock.on("POST", "/api/v1/tasks/T1/retry", (_req, res) => {
      sendProblem(res, {
        status: 409,
        code: "invalid_transition",
        detail: "task T1 (status=Draft) cannot be retried",
        extra: { task_status: "draft", trigger: "retry" },
      });
    });

    const outcome = await runRetryAction(client, "T1", form({ intent: "retry" }));

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected failure");
    expect(outcome.error.status).toBe(409);
    expect(outcome.error.conflict).toBe(true);
    expect(outcome.error.detail).toContain("cannot be retried");
  });

  it("returns a 401 ActionError when taskd requires a token (propagated, not thrown)", async () => {
    mock.on("POST", "/api/v1/tasks/T1/retry", (_req, res) => {
      sendProblem(res, { status: 401, code: "unauthorized", detail: "a valid bearer token is required" });
    });

    const outcome = await runRetryAction(client, "T1", form({ intent: "retry" }));

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected failure");
    expect(outcome.error.status).toBe(401);
    expect(outcome.error.code).toBe("unauthorized");
  });
});

describe('the tasks.$id action dispatch (intent === "retry" vs. the usual approve/reject/answer/cancel)', () => {
  it("runTaskAction still rejects `retry` as an unknown intent (the route must branch before calling it)", async () => {
    let error: unknown;
    try {
      await runTaskAction(client, "T1", form({ intent: "retry" }));
    } catch (e) {
      error = e;
    }
    expect(error).toMatchObject({ type: "DataWithResponseInit", init: { status: 400 } });
  });
});
