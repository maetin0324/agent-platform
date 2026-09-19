import type { ActionError, IntegrateOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { TaskdClient } from "./client.server";
import { formString } from "./forms";
import type { ChangeDiffView, ChangesView, IntegrateBody, IntegrateResult } from "./types";

/**
 * 変更の取り込み（ADR-0043 D5、taskd Phase 54 / G17）の中継。
 * 読み取り（`GET /tasks/{id}/changes`、`GET /tasks/{id}/changes/{repo}/diff`）はトークン不要、
 * 取り込み（`POST .../integrate`、`POST .../pr/merge`）は**管理系（人だけ）**で、
 * どちらも同じ `TaskdClient`（`getTaskdClient()`）を通す。`Authorization` を付けるのはクライアント側で、
 * 401 の案内文は `~/components/Flash.tsx` に集約されている（`~/taskd/repos-admin.server.ts` と同じ）。
 *
 * GUI では判定も回避もしない（ADR-0043 D5 はすべて taskd 側の判断）:
 * - 409 `default_branch_busy`（人のチェックアウトが default_branch で未コミット＝「main が編集中」）
 * - 409 `pr_unavailable`（`origin` が無い・`gh` が使えない・開いている PR が無い）
 * - 422 `validation`（`discard` に `confirm: true` が無い）
 * - **衝突は 200**（`integration.state === "conflict"` + `child_task_id` に「衝突の解消: …」タスク）、
 *   git の失敗も 200（`state === "failed"` + `detail`）。エラーにせずそのまま画面に出す。
 *
 * 一覧（`GET /tasks/{id}/changes`）が落ちたらページ自体が出せないので例外のまま投げ、選んだファイルの
 * 差分（`GET .../diff`）の失敗だけは `diffError` として画面に載せる（`~/taskd/task-files.server.ts` の
 * `fileError` と同じ。1 件の差分が取れなくても一覧は見えていてほしい）。
 */
export interface TaskChangesQuery {
  /** 差分を見るリポジトリ（`file` と対で使う。タスクの中での名前 = `repos/<name>/`）。 */
  repo?: string | null;
  /** 差分を見るファイル（リポジトリの根からの相対パス）。無ければ差分は取りに行かない。 */
  file?: string | null;
}

export interface TaskChangesData {
  taskId: string;
  changes: ChangesView;
  /** 選んだファイルの unified diff（`truncated` なら 200 KiB で切られている）。 */
  diff: ChangeDiffView | null;
  /** 差分の取得に失敗したときの taskd の文言（400 / 403 / 404 など）。 */
  diffError: ActionError | null;
  /** いま選んでいるリポジトリとファイル（失敗していても画面に出すため別に持つ）。 */
  diffRepo: string | null;
  diffPath: string | null;
}

export async function loadTaskChanges(
  client: TaskdClient,
  taskId: string,
  query: TaskChangesQuery = {},
  signal?: AbortSignal,
): Promise<TaskChangesData> {
  const diffRepo = query.repo || null;
  const diffPath = query.file || null;
  const changes = await client.get<ChangesView>(`/tasks/${encodeURIComponent(taskId)}/changes`, { signal });
  // `repo` と `file` は対。片方だけのリンクは差分を持たない（400 `path` 必須を踏みに行かない）。
  if (!diffRepo || !diffPath) {
    return { taskId, changes, diff: null, diffError: null, diffRepo, diffPath };
  }
  try {
    const diff = await client.get<ChangeDiffView>(
      `/tasks/${encodeURIComponent(taskId)}/changes/${encodeURIComponent(diffRepo)}/diff`,
      { query: { path: diffPath }, signal },
    );
    return { taskId, changes, diff, diffError: null, diffRepo, diffPath };
  } catch (e) {
    return { taskId, changes, diff: null, diffError: toActionError(e), diffRepo, diffPath };
  }
}

/** `Request` の URL（`?repo=&file=`）から問い合わせを組む。 */
export function readTaskChangesQuery(request: Request): TaskChangesQuery {
  const url = new URL(request.url);
  return { repo: url.searchParams.get("repo"), file: url.searchParams.get("file") };
}

/**
 * `POST /tasks/{id}/changes/{repo}/integrate`（管理系、200 `IntegrateResult`）。
 * 衝突・失敗も 200 で返るので `ok: true` のまま（`integration.state` を画面が見る）。
 */
export async function integrateChange(
  client: TaskdClient,
  taskId: string,
  repo: string,
  body: IntegrateBody,
  signal?: AbortSignal,
): Promise<IntegrateOutcome> {
  const path = `/tasks/${encodeURIComponent(taskId)}/changes/${encodeURIComponent(repo)}/integrate`;
  try {
    const result = await client.post<IntegrateResult>(path, body, { signal });
    return { ok: true, op: "integrate", repo, result };
  } catch (e) {
    return { ok: false, op: "integrate", repo, error: toActionError(e) };
  }
}

/**
 * `POST /tasks/{id}/changes/{repo}/pr/merge`（管理系、本文は空の JSON）。
 * 方法（`--merge` / `--squash` / …）は taskd の `[github] merge_method` が決める（`ChangesView.merge_method`）。
 */
export async function mergePullRequest(
  client: TaskdClient,
  taskId: string,
  repo: string,
  signal?: AbortSignal,
): Promise<IntegrateOutcome> {
  const path = `/tasks/${encodeURIComponent(taskId)}/changes/${encodeURIComponent(repo)}/pr/merge`;
  try {
    const result = await client.post<IntegrateResult>(path, {}, { signal });
    return { ok: true, op: "pr_merge", repo, result };
  } catch (e) {
    return { ok: false, op: "pr_merge", repo, error: toActionError(e) };
  }
}

/**
 * 取り込みのフォーム（`method` / `note` / `confirm`）から `IntegrateBody` を組む。
 * `confirm` は `discard` の確認欄が出ているときだけ送られてくる（**空欄はキーごと送らない**。
 * `~/taskd/repos-admin.server.ts` と同じ規律で、GUI 側では検証しない: `confirm` を落としたら
 * taskd が 422 `validation` の `errors[0].field = "confirm"` を返す）。
 */
export function readIntegrateBody(form: FormData): IntegrateBody {
  const body: IntegrateBody = { method: (formString(form, "method") ?? "merge") as IntegrateBody["method"] };
  const note = formString(form, "note");
  if (note) body.note = note;
  if (formString(form, "confirm") === "true") body.confirm = true;
  return body;
}
