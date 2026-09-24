import { data } from "react-router";
import type { RetryOutcome, TransitionOutcome } from "./action-types";
import type { CelerisClient } from "./client.server";
import { toActionError } from "./errors";
import { formString } from "./forms";
import type {
  Action,
  AnswerBody,
  CancelBody,
  DecisionBody,
  RetryBody,
  RetryResult,
  Status,
  TransitionResult,
} from "./types";

/**
 * 状態変更 action の共通処理（docs/DESIGN.md §6.3 の 2、§6.6、docs/adr/0005 D2）。
 * - フォーム（`intent` / `expected_status` / `note` / `answer`）を対応する `POST /tasks/{id}/{intent}` の本文に写す。
 *   GUI は判断ロジックを持たない: どの操作が可能かは `TaskDetail.actions` / 受信箱の区画（celeris 側）で決まり、
 *   ここは送るだけ。拒否（409 / 422）は celeris の文言をそのまま画面に返す
 * - `expected_status` は常に付ける（docs/celeris-api-v1.md §1.2 楽観的検査）。409 は `conflict: true`
 * - celeris のエラーは例外にせず `TransitionOutcome` / `ActionError` として返す（`data(..., {status})` で包む）
 */

export { formString };

/**
 * `applyTransition`（`readTransitionForm` 経由）が扱う `intent`。Phase 31 で `Action` に加わった `retry`
 * は本文・応答の形が違う別経路（`applyRetry`）なので、ここでは意図して除く（`readIntent` は `retry` を
 * 400 として拒む。ルート側は `intent === "retry"` を先に見て `runRetryAction` に分ける）。
 * Phase 53（ADR-0044 D1/D2）で加わった `edit`（`PATCH /tasks/{id}`）と `reopen`
 * （`POST /tasks/{id}/reopen`）も同じ理由で除く（`~/celeris/tasks-admin.server.ts` が受け持つ）。
 * ADR-0070 D2（Phase 116）で加わった `rereview`（`POST /tasks/{id}/rereview`）も同じ理由で除く
 * （`~/celeris/tasks-admin.server.ts::rereviewTask`。ルート側は `intent === "rereview"` を先に見る）。
 */
export type GateAction = Exclude<Action, "retry" | "edit" | "reopen" | "rereview">;

export const ACTIONS: readonly GateAction[] = ["approve", "reject", "answer", "cancel"];
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

export function isAction(v: unknown): v is GateAction {
  return typeof v === "string" && (ACTIONS as readonly string[]).includes(v);
}

export function isStatus(v: unknown): v is Status {
  return typeof v === "string" && (STATUSES as readonly string[]).includes(v);
}

/** フォームの `intent` を `GateAction` として読む。無効なら 400 の `Response` を投げる。 */
export function readIntent(form: FormData): GateAction {
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

/**
 * `CelerisError` / `CelerisUnavailable` → `ActionError`。**実体は `./errors` に移した**
 * （Phase 52 + 53 のマージ。ルートの `loadX` ヘルパ〈クライアントの束にも入る〉から呼ぶので、
 * `*.server` の印が付いた場所には置けない）。ここからの再輸出は従来の import を壊さないため。
 */
export { toActionError };

export interface TransitionInput {
  intent: GateAction;
  expectedStatus?: Status | undefined;
  note?: string | null;
  answer?: string | null;
}

/** 状態変更 1 件を celeris に送る。結果は `TransitionOutcome`（celeris のエラーは例外にしない）。 */
export async function applyTransition(
  client: CelerisClient,
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

/**
 * `POST /tasks/{id}/retry`（Phase 31。実機の事故、2026-09-18。docs/celeris-api-v1.md §3.63）。
 * `failed`/`cancelled` のタスクを 1 件、複製してやり直す。`accept` チェックボックス（`"true"`）を付けると
 * 新しいタスクは `draft` を経ず `ready` で始まる。
 */
export async function applyRetry(
  client: CelerisClient,
  taskId: string,
  form: FormData,
  signal?: AbortSignal,
): Promise<RetryOutcome> {
  // ADR-0070 D2 追記（Phase 116。本番で確認: 既定で `accept` を送らないと `draft` のまま止まり、
  // 「やり直したのに動かない」状態になった）: 既定は `ready`（`accept: true`）。`draft` のまま
  // 始めたいときだけフォームが明示で `draft=true` を送る（celeris 側の既定と揃える）。
  const body: RetryBody = { accept: form.get("draft") !== "true" };
  try {
    const result = await client.post<RetryResult>(`/tasks/${encodeURIComponent(taskId)}/retry`, body, { signal });
    return { ok: true, taskId, result };
  } catch (e) {
    return { ok: false, taskId, error: toActionError(e) };
  }
}

/** `TransitionOutcome` を action の戻り値にする（失敗時は celeris の status をそのまま応答の status にする）。 */
export function transitionData(outcome: TransitionOutcome) {
  return data(outcome, { status: outcome.ok ? 200 : outcome.error.status });
}

/** `RetryOutcome`（Phase 31）を action の戻り値にする。成功は 201（celeris と同じ）。 */
export function retryData(outcome: RetryOutcome) {
  return data(outcome, { status: outcome.ok ? 201 : outcome.error.status });
}
