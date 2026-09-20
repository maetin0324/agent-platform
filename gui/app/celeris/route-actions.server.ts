import { data } from "react-router";
import type { CreateFailure, ReplayOutcome, RetryOutcome, TransitionOutcome } from "./action-types";
import { applyRetry, applyTransition, readTransitionForm, toActionError } from "./actions.server";
import type { CelerisClient } from "./client.server";
import type { NewPlanSpec, NewTaskSpec, ReplayReport, Task } from "./types";

/**
 * 各ルートの `action` 本体（docs/DESIGN.md §6.3 の 2、§6.6、docs/adr/0005 D2 / D4）。
 * ルートファイル（`app/routes/*.tsx`）ではなくここに置くのは、React Router が server-only モジュール
 * （`*.server.ts`）への参照を消せるのが `loader` / `action` / `middleware` 等の route export だけで、
 * テスト用に export した補助関数がこれらを参照するとクライアントバンドルの生成が失敗するため
 * （`react-router:dot-server` プラグイン。`pnpm build` で発覚）。ルートは `action` の中からだけこれを呼ぶ。
 * GUI 側で判断ロジックは書かない: どの操作が可能かは celeris（`TaskDetail.actions` / 受信箱の区画）が決め、
 * ここはフォームの値を対応する `POST` に写すだけ。409 / 422 は celeris の文言をそのまま返す。
 */

/** `/tasks/:id` の状態変更（approve / reject / answer / cancel）。 */
export async function runTaskAction(
  client: CelerisClient,
  taskId: string,
  form: FormData,
  signal?: AbortSignal,
): Promise<TransitionOutcome> {
  const input = readTransitionForm(form);
  return applyTransition(client, taskId, input, signal);
}

/**
 * `/tasks/:id` の「やり直す」（Phase 31。`intent` は `TransitionInput`（approve 等）とは別語彙なので、
 * ルートの `action` はここへの分岐を `intent === "retry"` で先に判定する）。
 */
export async function runRetryAction(
  client: CelerisClient,
  taskId: string,
  form: FormData,
  signal?: AbortSignal,
): Promise<RetryOutcome> {
  return applyRetry(client, taskId, form, signal);
}

/**
 * 受信箱の状態変更。1 ページに複数タスクがあるので hidden `task_id` を 1 つ以上受け取り、
 * **直列に** `applyTransition` を呼ぶ。途中で失敗しても残りを続ける（原子性は無い。docs/DESIGN.md §4.1）。
 */
export async function runInboxAction(
  client: CelerisClient,
  form: FormData,
  signal?: AbortSignal,
): Promise<TransitionOutcome[]> {
  const taskIds = form.getAll("task_id").filter((v): v is string => typeof v === "string" && v !== "");
  if (taskIds.length === 0) {
    throw data({ error: "task_id is required" }, { status: 400 });
  }
  const input = readTransitionForm(form);
  const outcomes: TransitionOutcome[] = [];
  for (const taskId of taskIds) {
    outcomes.push(await applyTransition(client, taskId, input, signal));
  }
  return outcomes;
}

/** `POST /tasks`。celeris のエラーは例外にせず `CreateFailure` として返す。 */
export async function createTask(
  client: CelerisClient,
  spec: NewTaskSpec,
  signal?: AbortSignal,
): Promise<{ ok: true; task: Task } | CreateFailure> {
  try {
    const task = await client.post<Task>("/tasks", spec, { signal });
    return { ok: true, task };
  } catch (e) {
    return { ok: false, error: toActionError(e) };
  }
}

/** `POST /plans`。celeris のエラーは例外にせず `CreateFailure` として返す。 */
export async function createPlan(
  client: CelerisClient,
  spec: NewPlanSpec,
  signal?: AbortSignal,
): Promise<{ ok: true; task: Task } | CreateFailure> {
  try {
    const task = await client.post<Task>("/plans", spec, { signal });
    return { ok: true, task };
  } catch (e) {
    return { ok: false, error: toActionError(e) };
  }
}

/** `POST /replay`（docs/celeris-api-v1.md §3.15）。本文は空。DB は変更しない。503 `replay_in_progress` も `{ok:false}`。 */
export async function runReplay(client: CelerisClient, signal?: AbortSignal): Promise<ReplayOutcome> {
  try {
    const report = await client.post<ReplayReport>("/replay", {}, { signal });
    return { ok: true, report };
  } catch (e) {
    return { ok: false, error: toActionError(e) };
  }
}
