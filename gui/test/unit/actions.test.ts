import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  applyTransition,
  readExpectedStatus,
  readIntent,
  readTransitionForm,
  toActionError,
  transitionData,
} from "~/celeris/actions.server";
import { CelerisClient } from "~/celeris/client.server";
import { CelerisError, CelerisUnavailable } from "~/celeris/errors";
import type { TransitionResult } from "~/celeris/types";
import { type MockCeleris, sendJson, sendProblem, startMockCeleris } from "../mock-celeris/server";

// docs/adr/0005 D2 / D3: action の共通処理。フォーム → celeris の本文の写し、celeris のエラー → ActionError。

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

function lastBody(): unknown {
  const req = mock.requests.at(-1);
  return req ? JSON.parse(req.body) : undefined;
}

const ok: TransitionResult = { id: "T1", from: "ready", to: "done", reason: "approve", cascaded: [] };

describe("readIntent / readExpectedStatus / readTransitionForm", () => {
  it("accepts the four actions and rejects anything else with a 400 Response", () => {
    for (const intent of ["approve", "reject", "answer", "cancel"]) {
      expect(readIntent(form({ intent }))).toBe(intent);
    }
    let thrown: unknown;
    try {
      readIntent(form({ intent: "explode" }));
    } catch (e) {
      thrown = e;
    }
    // react-router の data() は Response ではなく DataWithResponseInit（status は init に入る）
    expect(thrown).not.toBeInstanceOf(Response);
    expect(thrown).toMatchObject({ type: "DataWithResponseInit", init: { status: 400 } });
  });

  it("reads expected_status only when it is a known Status", () => {
    expect(readExpectedStatus(form({}))).toBeUndefined();
    expect(readExpectedStatus(form({ expected_status: "" }))).toBeUndefined();
    expect(readExpectedStatus(form({ expected_status: "blocked" }))).toBe("blocked");
    expect(() => readExpectedStatus(form({ expected_status: "weird" }))).toThrow();
  });

  it("maps note/answer, turning empty strings into null", () => {
    expect(readTransitionForm(form({ intent: "approve", expected_status: "ready", note: "", answer: "" }))).toEqual({
      intent: "approve",
      expectedStatus: "ready",
      note: null,
      answer: null,
    });
  });
});

describe("applyTransition", () => {
  it("approve → POST /tasks/{id}/approve with DecisionBody{note, expected_status}", async () => {
    mock.on("POST", "/api/v1/tasks/T1/approve", (_req, res) => sendJson(res, 200, ok));
    const outcome = await applyTransition(client, "T1", { intent: "approve", expectedStatus: "ready", note: "lgtm" });
    expect(outcome).toEqual({ ok: true, intent: "approve", taskId: "T1", result: ok });
    expect(lastBody()).toEqual({ note: "lgtm", expected_status: "ready" });
    expect(mock.requests.at(-1)?.headers["content-type"]).toMatch(/application\/json/);
  });

  it("reject → POST /reject with note null when omitted", async () => {
    mock.on("POST", "/api/v1/tasks/T1/reject", (_req, res) => sendJson(res, 200, { ...ok, to: "failed" }));
    const outcome = await applyTransition(client, "T1", { intent: "reject", expectedStatus: "ready" });
    expect(outcome.ok).toBe(true);
    expect(lastBody()).toEqual({ note: null, expected_status: "ready" });
  });

  it("answer → POST /answer with AnswerBody{answer, expected_status}", async () => {
    mock.on("POST", "/api/v1/tasks/T1/answer", (_req, res) =>
      sendJson(res, 200, { ...ok, from: "blocked", to: "ready" }),
    );
    const outcome = await applyTransition(client, "T1", {
      intent: "answer",
      expectedStatus: "blocked",
      answer: "staging",
    });
    expect(outcome.ok).toBe(true);
    expect(lastBody()).toEqual({ answer: "staging", expected_status: "blocked" });
  });

  it("cancel → POST /cancel with CancelBody{expected_status} and passes `cascaded` through untouched", async () => {
    const cascaded = [{ id: "T2", title: "downstream", kind: "execute", status: "cancelled" }];
    mock.on("POST", "/api/v1/tasks/T1/cancel", (_req, res) =>
      sendJson(res, 200, { id: "T1", from: "ready", to: "cancelled", reason: "cancel", cascaded }),
    );
    const outcome = await applyTransition(client, "T1", { intent: "cancel", expectedStatus: "ready" });
    expect(lastBody()).toEqual({ expected_status: "ready" });
    expect(outcome.ok && outcome.result.cascaded).toEqual(cascaded);
  });

  it("409 conflict → ok:false with conflict:true and celeris's detail verbatim", async () => {
    mock.on("POST", "/api/v1/tasks/T1/approve", (_req, res) =>
      sendProblem(res, {
        status: 409,
        code: "conflict",
        detail: "expected status Ready but task has status Done",
        extra: { expected: "ready", actual: "done" },
      }),
    );
    const outcome = await applyTransition(client, "T1", { intent: "approve", expectedStatus: "ready" });
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error).toMatchObject({
      status: 409,
      code: "conflict",
      conflict: true,
      detail: "expected status Ready but task has status Done",
    });
  });

  it("409 invalid_transition is also a conflict", async () => {
    mock.on("POST", "/api/v1/tasks/T1/cancel", (_req, res) =>
      sendProblem(res, {
        status: 409,
        code: "invalid_transition",
        detail: "task T1 (kind=execute, status=done) cannot be cancelled",
        extra: { task_status: "done", kind: "execute", trigger: "cancel" },
      }),
    );
    const outcome = await applyTransition(client, "T1", { intent: "cancel" });
    expect(!outcome.ok && outcome.error.conflict).toBe(true);
    expect(!outcome.ok && outcome.error.code).toBe("invalid_transition");
  });

  it("422 validation → fields keyed by `field`, messages verbatim", async () => {
    mock.on("POST", "/api/v1/tasks/T1/answer", (_req, res) =>
      sendProblem(res, {
        status: 422,
        code: "validation",
        detail: "validation failed",
        extra: { errors: [{ field: "answer", message: "answer must not be blank" }, { message: "unfielded" }] },
      }),
    );
    const outcome = await applyTransition(client, "T1", { intent: "answer", answer: "  " });
    expect(outcome.ok).toBe(false);
    if (outcome.ok) return;
    expect(outcome.error.conflict).toBe(false);
    expect(outcome.error.fields).toEqual({ answer: ["answer must not be blank"] });
    expect(outcome.error.messages).toEqual(["answer must not be blank", "unfielded"]);
  });

  it("celeris down → ok:false with code unavailable / status 503", async () => {
    await mock.close();
    const outcome = await applyTransition(client, "T1", { intent: "cancel" });
    expect(!outcome.ok && outcome.error).toMatchObject({ status: 503, code: "unavailable", conflict: false });
    mock = await startMockCeleris(); // afterEach が close できるように
  });
});

describe("toActionError / transitionData", () => {
  it("re-throws errors that are not celeris errors", () => {
    expect(() => toActionError(new Error("bug"))).toThrow("bug");
  });

  it("CelerisUnavailable mentions the base URL", () => {
    const err = toActionError(new CelerisUnavailable("http://127.0.0.1:1"));
    expect(err.detail).toContain("http://127.0.0.1:1");
  });

  it("CelerisError without errors[] yields empty fields", () => {
    const err = toActionError(new CelerisError({ status: 404, code: "task_not_found", detail: "task X not found" }));
    expect(err).toMatchObject({ status: 404, code: "task_not_found", fields: {}, messages: [] });
  });

  it("transitionData uses 200 on success and the celeris status on failure", () => {
    const success = transitionData({ ok: true, intent: "approve", taskId: "T1", result: ok });
    const failure = transitionData({
      ok: false,
      intent: "approve",
      taskId: "T1",
      error: { status: 409, code: "conflict", detail: "", conflict: true, fields: {}, messages: [] },
    });
    expect(success.init?.status ?? 200).toBe(200);
    expect(failure.init?.status).toBe(409);
  });
});
