import type { Graph, OrgNode, ProjectTaskView, TaskSummary } from "~/taskd/types";

/**
 * 案件の「仕事の木」（SPEC §3.3、ADR-0033 D2）を、既存の DAG 描画部品（`~/lib/graph-layout.ts` の
 * `layoutGraph`）に渡せる `Graph` に写す純粋関数。DAG は既存どおり `parent_id` / `depends_on`
 * （`GET /projects/{id}` の `tasks` は `GET /graph` と同じ辺の作り方。docs/gui/api.md §3.47）。
 * `role` フィールドには `assignee` の**組織ノードの名前**を入れる（taskd の役割ではなく表示用の流用。
 * `layoutGraph` は `role` をラベルの最終行にそのまま出すだけで、意味の解釈はしない）。
 *
 * 木から外すもの（どちらも「裏方」で、人が方針の異常を見るための木には要らない）:
 * - `conversation` が `true` のタスク（対話用。人への返事のための run。GUI-R3 Phase 27）。
 * - **まとめのタスク**（`role = "report-compressor"`、`title` が「報告のまとめ: …」。ADR-0033 D3 の報告の圧縮）。
 *   `ProjectTaskView` に `role` が無いので、`GET /tasks` の `TaskSummary.role` から id の集合を作って渡す
 *   （{@link supportTaskIds}）。taskd 側 Phase 29 の `support` の印が来たら、そちらに切り替える。
 *
 * 外したタスクを指す `parent_id` / `depends_on` は、既存の「存在しないタスクを指す辺・親は `layoutGraph`
 * 側が捨てる／親なしのグループとして描く」という前提にそのまま乗せる（新しい判断ロジックを足さない）。
 */

/** まとめの run の役割（taskd の `task_core::report::COMPACTION_ROLE`）。 */
export const COMPACTION_ROLE = "report-compressor";

/** `GET /tasks` の要約から、木に出さない裏方のタスク（まとめの run）の id を集める。 */
export function supportTaskIds(summaries: readonly Pick<TaskSummary, "id" | "role">[]): Set<string> {
  const ids = new Set<string>();
  for (const t of summaries) {
    if (t.role === COMPACTION_ROLE) ids.add(t.id);
  }
  return ids;
}

/** 人が見る「仕事」だけに絞る（対話用とまとめのタスクを外す）。件数表示・一覧もこの結果に揃える。 */
export function visibleWorkTasks(
  tasks: readonly ProjectTaskView[],
  hiddenIds: ReadonlySet<string> = new Set(),
): ProjectTaskView[] {
  return tasks.filter((t) => !t.conversation && !hiddenIds.has(t.id));
}

export function projectTasksToGraph(
  tasks: readonly ProjectTaskView[],
  orgById: Map<string, OrgNode>,
  hiddenIds: ReadonlySet<string> = new Set(),
): Graph {
  const workTasks = visibleWorkTasks(tasks, hiddenIds);
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
