import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { CelerisClient } from "~/celeris/client.server";
import { runTaskAction } from "~/celeris/route-actions.server";
import type { TransitionResult } from "~/celeris/types";
import { type MockCeleris, sendJson, sendProblem, startMockCeleris } from "../mock-celeris/server";

let mock: MockCeleris;
let client: CelerisClient;

beforeEach(async () => {
  mock = await startMockCeleris();
  client = new CelerisClient({ baseUrl: mock.baseUrl });
});

afterEach(async () => {
  await mock.close();
});

function form(entries: Record<string, string>): FormData {
  const f = new FormData();
  for (const [k, v] of Object.entries(entries)) f.append(k, v);
  return f;
}

describe("runTaskAction", () => {
  it("posts approve with note and expected_status to /tasks/{id}/approve", async () => {
    const result: TransitionResult = { id: "T1", from: "ready", to: "reviewing", reason: "approved" };
    mock.on("POST", "/api/v1/tasks/T1/approve", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ note: "looks good", expected_status: "ready" });
      sendJson(res, 200, result);
    });

    const outcome = await runTaskAction(
      client,
      "T1",
      form({ intent: "approve", expected_status: "ready", note: "looks good" }),
    );

    expect(outcome).toEqual({ ok: true, intent: "approve", taskId: "T1", result });
  });

  it("posts answer with answer and expected_status to /tasks/{id}/answer", async () => {
    const result: TransitionResult = { id: "T1", from: "blocked", to: "ready", reason: "answered" };
    mock.on("POST", "/api/v1/tasks/T1/answer", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ answer: "staging", expected_status: "blocked" });
      sendJson(res, 200, result);
    });

    const outcome = await runTaskAction(
      client,
      "T1",
      form({ intent: "answer", answer: "staging", expected_status: "blocked" }),
    );

    expect(outcome).toEqual({ ok: true, intent: "answer", taskId: "T1", result });
  });

  it("posts cancel with expected_status to /tasks/{id}/cancel and returns cascaded as-is", async () => {
    const result: TransitionResult = {
      id: "T1",
      from: "ready",
      to: "cancelled",
      reason: "cancelled",
      cascaded: [{ id: "T2", kind: "execute", status: "cancelled", title: "child", actions: [] }],
    };
    mock.on("POST", "/api/v1/tasks/T1/cancel", (_req, res, body) => {
      expect(JSON.parse(body)).toEqual({ expected_status: "ready" });
      sendJson(res, 200, result);
    });

    const outcome = await runTaskAction(client, "T1", form({ intent: "cancel", expected_status: "ready" }));

    expect(outcome).toEqual({ ok: true, intent: "cancel", taskId: "T1", result });
  });

  it("returns a conflict ActionError for 409 conflict", async () => {
    mock.on("POST", "/api/v1/tasks/T1/approve", (_req, res) => {
      sendProblem(res, {
        status: 409,
        code: "conflict",
        detail: "expected status Ready but task has status Done",
        extra: { expected: "ready", actual: "done" },
      });
    });

    const outcome = await runTaskAction(client, "T1", form({ intent: "approve", expected_status: "ready", note: "" }));

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected failure");
    expect(outcome.error.conflict).toBe(true);
    expect(outcome.error.status).toBe(409);
    expect(outcome.error.detail).toBe("expected status Ready but task has status Done");
  });

  it("returns a conflict ActionError for 409 invalid_transition", async () => {
    mock.on("POST", "/api/v1/tasks/T1/cancel", (_req, res) => {
      sendProblem(res, {
        status: 409,
        code: "invalid_transition",
        detail: "cannot cancel a done task",
      });
    });

    const outcome = await runTaskAction(client, "T1", form({ intent: "cancel", expected_status: "ready" }));

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected failure");
    expect(outcome.error.conflict).toBe(true);
    expect(outcome.error.status).toBe(409);
  });

  it("returns field errors for 422 validation on answer", async () => {
    mock.on("POST", "/api/v1/tasks/T1/answer", (_req, res) => {
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "validation failed",
        extra: { errors: [{ field: "answer", message: "answer must not be blank" }] },
      });
    });

    const outcome = await runTaskAction(
      client,
      "T1",
      form({ intent: "answer", answer: "   ", expected_status: "blocked" }),
    );

    expect(outcome.ok).toBe(false);
    if (outcome.ok) throw new Error("expected failure");
    expect(outcome.error.status).toBe(422);
    expect(outcome.error.conflict).toBe(false);
    expect(outcome.error.fields.answer).toEqual(["answer must not be blank"]);
  });

  it("throws react-router's data() with status 400 for an unknown intent", async () => {
    // readIntent は `data({...}, {status:400})`（react-router）を投げる。この戻り値は
    // `DataWithResponseInit` であり実際には `instanceof Response` にはならない
    // （react-router のシリアライズ前の中間表現。node_modules/react-router で確認済み）。
    let error: unknown;
    try {
      await runTaskAction(client, "T1", form({ intent: "explode", expected_status: "ready" }));
    } catch (e) {
      error = e;
    }

    expect(error).not.toBeInstanceOf(Response);
    expect(error).toMatchObject({ type: "DataWithResponseInit", init: { status: 400 } });
  });
});
