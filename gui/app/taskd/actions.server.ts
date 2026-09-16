import { data } from "react-router";
import type { ActionError, TransitionOutcome } from "./action-types";
import type { TaskdClient } from "./client.server";
import { isTaskdUnavailable, TaskdError } from "./errors";
import { formString } from "./forms";
import type { Action, AnswerBody, CancelBody, DecisionBody, Status, TransitionResult } from "./types";

/**
 * 状態変更 action の共通処理（docs/DESIGN.md §6.3 の 2、§6.6、docs/adr/0005 D2）。
 * - フォーム（`intent` / `expected_status` / `note` / `answer`）を対応する `POST /tasks/{id}/{intent}` の本文に写す。
 *   GUI は判断ロジックを持たない: どの操作が可能かは `TaskDetail.actions` / 受信箱の区画（taskd 側）で決まり、
 *   ここは送るだけ。拒否（409 / 422）は taskd の文言をそのまま画面に返す
 * - `expected_status` は常に付ける（docs/taskd-api-v1.md §1.2 楽観的検査）。409 は `conflict: true`
 * - taskd のエラーは例外にせず `TransitionOutcome` / `ActionError` として返す（`data(..., {status})` で包む）
 */

export { formString };

export const ACTIONS: readonly Action[] = ["approve", "reject", "answer", "cancel"];
const STATUSES: readonly Status[] = [
  "draft",
  "ready",
  "running",
  "blocked",
  "reviewing",
  "done",
  "failed",
  "cancelled",
];

export function isAction(v: unknown): v is Action {
  return typeof v === "string" && (ACTIONS as readonly string[]).includes(v);
}

export function isStatus(v: unknown): v is Status {
  return typeof v === "string" && (STATUSES as readonly string[]).includes(v);
}

/** フォームの `intent` を `Action` として読む。無効なら 400 の `Response` を投げる。 */
export function readIntent(form: FormData): Action {
  const intent = form.get("intent");
  if (!isAction(intent)) throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  return intent;
}

/** フォームの `expected_status`（無ければ `undefined`）。不正な値は 400。 */
export function readExpectedStatus(form: FormData): Status | undefined {
  const v = formString(form, "expected_status");
  if (v === null) return undefined;
  if (!isStatus(v)) throw data({ error: `invalid expected_status: ${v}` }, { status: 400 });
  return v;
}

/** `TaskdError` / `TaskdUnavailable` を `ActionError` にする。それ以外は re-throw（本当に予期しないエラー）。 */
export function toActionError(e: unknown): ActionError {
  if (isTaskdUnavailable(e)) {
    return {
      status: 503,
      code: "unavailable",
      detail: `taskd に接続できません（${e.baseUrl}）`,
      conflict: false,
      fields: {},
      messages: [],
    };
  }
  if (e instanceof TaskdError) {
    const fields: Record<string, string[]> = {};
    const messages: string[] = [];
    const errors = e.extra.errors;
    if (Array.isArray(errors)) {
      for (const item of errors) {
        if (!item || typeof item !== "object") continue;
        const message = (item as { message?: unknown }).message;
        if (typeof message !== "string") continue;
        messages.push(message);
        const field = (item as { field?: unknown }).field;
        if (typeof field === "string" && field) {
          fields[field] ??= [];
          fields[field].push(message);
        }
      }
    }
    return {
      status: e.status,
      code: e.code,
      detail: e.detail,
      conflict: e.status === 409,
      fields,
      messages,
    };
  }
  throw e;
}

export interface TransitionInput {
  intent: Action;
  expectedStatus?: Status | undefined;
  note?: string | null;
  answer?: string | null;
}

/** 状態変更 1 件を taskd に送る。結果は `TransitionOutcome`（taskd のエラーは例外にしない）。 */
export async function applyTransition(
  client: TaskdClient,
  taskId: string,
  input: TransitionInput,
  signal?: AbortSignal,
): Promise<TransitionOutcome> {
  let body: DecisionBody | AnswerBody | CancelBody;
  switch (input.intent) {
    case "approve":
    case "reject":
      body = { note: input.note ?? null, expected_status: input.expectedStatus ?? null } satisfies DecisionBody;
      break;
    case "answer":
      body = { answer: input.answer ?? "", expected_status: input.expectedStatus ?? null } satisfies AnswerBody;
      break;
    case "cancel":
      body = { expected_status: input.expectedStatus ?? null } satisfies CancelBody;
      break;
  }
  try {
    const result = await client.post<TransitionResult>(`/tasks/${encodeURIComponent(taskId)}/${input.intent}`, body, {
      signal,
    });
    return { ok: true, intent: input.intent, taskId, result };
  } catch (e) {
    return { ok: false, intent: input.intent, taskId, error: toActionError(e) };
  }
}

/** フォーム（`intent` / `expected_status` / `note` / `answer`）から `TransitionInput` を読む。 */
export function readTransitionForm(form: FormData): TransitionInput {
  return {
    intent: readIntent(form),
    expectedStatus: readExpectedStatus(form),
    note: formString(form, "note"),
    answer: formString(form, "answer"),
  };
}

/** `TransitionOutcome` を action の戻り値にする（失敗時は taskd の status をそのまま応答の status にする）。 */
export function transitionData(outcome: TransitionOutcome) {
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}
