import type { Graph, OrgNode, ProjectTaskView, TaskSummary } from "~/taskd/types";

/**
 * 案件の「仕事の木」（SPEC §3.3、ADR-0033 D2）を、既存の DAG 描画部品（`~/lib/graph-layout.ts` の
 * `layoutGraph`）に渡せる `Graph` に写す純粋関数。DAG は既存どおり `parent_id` / `depends_on`
 * （`GET /projects/{id}` の `tasks` は `GET /graph` と同じ辺の作り方。docs/gui/api.md §3.47）。
 * `role` フィールドには `assignee` の**組織ノードの名前**を入れる（taskd の役割ではなく表示用の流用。
 * `layoutGraph` は `role` をラベルの最終行にそのまま出すだけで、意味の解釈はしない）。
 *
 * **裏方のタスクは木から完全に外す**（SPEC「タスクは裏方」/ ADR-0033 D8）。判定は taskd の
 * `support`（`"conversation"` | `"compaction"` | `"approval"` | `"review"` | `null`。Phase 29 で追加、
 * `task_core::support_kind` の決定的な値）をそのまま使う。GUI 側で `role` や `title` から推測しない
 * （G13f-1 では `role = "report-compressor"` で代用していた。Phase 29 でこの印に置き換えた）。
 * `support` を持たない古い taskd の応答にも効くよう、`conversation` の真偽値も併せて見る。
 *
 * 外したタスクを指す `parent_id` / `depends_on` は、既存の「存在しないタスクを指す辺・親は `layoutGraph`
 * 側が捨てる／親なしのグループとして描く」という前提にそのまま乗せる（新しい判断ロジックを足さない）。
 */

/** 裏方のタスクか（`support` が付いていれば裏方。`conversation` は古い応答のための保険）。 */
export function isSupportTask(task: Pick<TaskSummary, "support" | "conversation">): boolean {
  return (task.support ?? null) !== null || task.conversation === true;
}

/** 人が見る「仕事」だけに絞る（裏方を外す）。件数表示・一覧もこの結果に揃える。 */
export function visibleWorkTasks(tasks: readonly ProjectTaskView[]): ProjectTaskView[] {
  return tasks.filter((t) => !isSupportTask(t));
}

export function projectTasksToGraph(tasks: readonly ProjectTaskView[], orgById: Map<string, OrgNode>): Graph {
  const workTasks = visibleWorkTasks(tasks);
  const nodes = workTasks.map((t) => ({
    id: t.id,
    kind: "execute" as const,
    parent_id: t.parent_id ?? null,
    role: t.assignee ? (orgById.get(t.assignee)?.name ?? t.assignee) : null,
    status: t.status,
    title: t.title,
  }));
  const edges = workTasks.flatMap((t) => t.depends_on.map((from) => ({ from, kind: "depends_on", to: t.id })));
  return { nodes, edges };
}
