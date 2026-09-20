import type { TaskCommentOutcome, TaskEditOutcome, TaskReopenOutcome } from "./action-types";
import { toActionError } from "./actions.server";
import type { CelerisClient } from "./client.server";
import { formString } from "./forms";
import type {
  CommentBody,
  CommentResult,
  CriterionSpec,
  EditResult,
  NewTaskSpec,
  PriorityInput,
  ReopenBody,
  Status,
  TaskCategory,
  TaskEdit,
  Tier,
  TransitionResult,
} from "./types";

/**
 * タスクの編集・コメント・再開（ADR-0044 D1 / D2。**管理系**、`token_file` 未設定でも 401）:
 * `PATCH /tasks/{id}` / `POST /tasks/{id}/comments` / `POST /tasks/{id}/reopen`。
 *
 * `org-admin.server.ts` と同じ作り: **GUI 側では検証しない**（終端のタスクが 409、ラベルの形や
 * 依存の循環が 422 になるのは celeris の判断で、その文言をそのまま画面に出す）。
 * 組織と同じく DB が正なので `POST /reload` は呼ばない。
 */

/** `TaskEdit` のうち、`null` を送ると「消す」になる項目（Rust の `Option<Option<T>>`）。 */
const NULLABLE_FIELDS = ["assignee", "role", "adapter", "milestone_id"] as const;

/** 数値の項目（空欄・非数値は送らない = 変えない）。 */
const NUMBER_FIELDS = ["max_turns", "max_wall_secs", "max_retries"] as const;

/** 文字列をそのまま送る項目（省略 = 変えない）。 */
const STRING_FIELDS = ["title", "objective", "priority", "category", "tier"] as const;

/** 差し替えになる配列の項目。フォームは空の番兵（`value=""`）を 1 つ置き、`form.has` で「送る意思」を示す。 */
const ARRAY_FIELDS = ["labels", "depends_on"] as const;

/**
 * 編集フォーム（`/tasks/:id` の「概要」タブ）とボードの行内編集の両方から `TaskEdit` を組み立てる
 * （ADR-0044 D1）。**フォームに現れた項目だけ**を本文に入れる（`TaskEdit` は「省略 = 変えない」）ので、
 * ボードのように 1 項目だけ送るフォームもそのまま使える。
 *
 * - `assignee` / `role` / `adapter` / `milestone_id` は空文字を `null`（= 外す）として送る
 * - `labels` / `depends_on` は差し替え。空の値は落とすので、番兵だけの状態は `[]`（全部外す）になる
 * - `priority` は `"P1"` のラベルのまま送る（celeris が `PriorityInput` として受ける。ADR-0044 D3）
 */
export function buildTaskEdit(form: FormData): TaskEdit {
  const edit: TaskEdit = {};
  for (const name of STRING_FIELDS) {
    if (!form.has(name)) continue;
    const v = formString(form, name);
    if (v !== null) (edit as Record<string, unknown>)[name] = v;
  }
  for (const name of NULLABLE_FIELDS) {
    if (!form.has(name)) continue;
    (edit as Record<string, unknown>)[name] = formString(form, name);
  }
  for (const name of NUMBER_FIELDS) {
    if (!form.has(name)) continue;
    const v = formString(form, name);
    if (v === null) continue;
    const n = Number(v);
    if (!Number.isNaN(n)) (edit as Record<string, unknown>)[name] = n;
  }
  for (const name of ARRAY_FIELDS) {
    if (!form.has(name)) continue;
    (edit as Record<string, unknown>)[name] = form
      .getAll(name)
      .map((v) => String(v).trim())
      .filter((v) => v !== "");
  }
  const expected = formString(form, "expected_status");
  if (expected !== null) edit.expected_status = expected as Status;
  return edit;
}

/**
 * 案件・途中目標の「タスクを追加」フォーム → `POST /tasks` の本文（ADR-0044 D1）。
 * **`status` は送らない**: `POST /tasks` の既定が `ready` になった（人は Go を出す側なので draft を挟まない）。
 * 受け入れ条件は 1 行の自由記述を `human` の条件 1 件として送る（celeris は 1 件以上を要求する）。
 * 空欄は本文に入れない（celeris の既定に任せる。`tasks.new.tsx` の `buildNewTaskSpec` と同じ考え方）。
 */
export function buildProjectTaskSpec(form: FormData, projectId: string): NewTaskSpec {
  const acceptanceText = formString(form, "acceptance");
  const acceptance: CriterionSpec[] = acceptanceText ? [{ type: "human", text: acceptanceText }] : [];
  const spec: NewTaskSpec = {
    title: (form.get("title") as string | null) ?? "",
    objective: (form.get("objective") as string | null) ?? "",
    acceptance,
    project_id: projectId,
  };
  const milestoneId = formString(form, "milestone_id");
  if (milestoneId) spec.milestone_id = milestoneId;
  const assignee = formString(form, "assignee");
  if (assignee) spec.assignee = assignee;
  const tier = formString(form, "tier");
  if (tier) spec.tier = tier as Tier;
  // `"P1"` のラベルをそのまま送る（celeris が `PriorityInput` として受ける。ADR-0044 D3）。
  const priority = formString(form, "priority");
  if (priority) spec.priority = priority as PriorityInput;
  const category = formString(form, "category");
  if (category) spec.category = category as TaskCategory;
  const labels = form
    .getAll("labels")
    .map((v) => String(v).trim())
    .filter((v) => v !== "");
  if (labels.length > 0) spec.labels = labels;
  const parent = formString(form, "parent");
  if (parent) spec.parent = parent;
  return spec;
}

/** `PATCH /tasks/{id}`（ADR-0044 D1）。終端のタスクは 409、`running`/`reviewing` は次の run から効く。 */
export async function editTask(
  client: CelerisClient,
  taskId: string,
  edit: TaskEdit,
  signal?: AbortSignal,
): Promise<TaskEditOutcome> {
  try {
    const result = await client.patch<EditResult>(`/tasks/${encodeURIComponent(taskId)}`, edit, { signal });
    return { ok: true, op: "edit", taskId, result };
  } catch (e) {
    return { ok: false, op: "edit", taskId, error: toActionError(e) };
  }
}

/**
 * `POST /tasks/{id}/comments`（ADR-0044 D2）。**人のコメントは担当をすぐ起こす**ので、
 * 応答の `effect`（`stored` / `interrupted` / `answered` / `terminal`）を必ず画面に出す。
 */
export async function commentOnTask(
  client: CelerisClient,
  taskId: string,
  form: FormData,
  signal?: AbortSignal,
): Promise<TaskCommentOutcome> {
  const body: CommentBody = { body: (form.get("body") as string | null) ?? "" };
  try {
    const result = await client.post<CommentResult>(`/tasks/${encodeURIComponent(taskId)}/comments`, body, { signal });
    return { ok: true, op: "comment", taskId, result };
  } catch (e) {
    return { ok: false, op: "comment", taskId, error: toActionError(e) };
  }
}

/**
 * `POST /tasks/{id}/reopen`（ADR-0044 D2）。`done`/`failed` → `ready`（attempts は 0 に戻る）。
 * `cancelled` は worktree を消してあるので 409（画面は「やり直す」= `retry` を使う）。
 */
export async function reopenTask(
  client: CelerisClient,
  taskId: string,
  form: FormData,
  signal?: AbortSignal,
): Promise<TaskReopenOutcome> {
  const expected = formString(form, "expected_status");
  const body: ReopenBody = expected === null ? {} : { expected_status: expected as Status };
  try {
    const result = await client.post<TransitionResult>(`/tasks/${encodeURIComponent(taskId)}/reopen`, body, { signal });
    return { ok: true, op: "reopen", taskId, result };
  } catch (e) {
    return { ok: false, op: "reopen", taskId, error: toActionError(e) };
  }
}
