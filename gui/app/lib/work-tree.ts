import type { Graph, OrgNode, ProjectTaskView } from "~/taskd/types";

/**
 * 案件の「仕事の木」（SPEC §3.3、ADR-0033 D2）を、既存の DAG 描画部品（`~/lib/graph-layout.ts` の
 * `layoutGraph`）に渡せる `Graph` に写す純粋関数。DAG は既存どおり `parent_id` / `depends_on`
 * （`GET /projects/{id}` の `tasks` は `GET /graph` と同じ辺の作り方。docs/gui/api.md §3.47）。
 * `role` フィールドには `assignee` の**組織ノードの名前**を入れる（taskd の役割ではなく表示用の流用。
 * `layoutGraph` は `role` をラベルの 2 行目にそのまま出すだけで、意味の解釈はしない）。
 */
export function projectTasksToGraph(tasks: ProjectTaskView[], orgById: Map<string, OrgNode>): Graph {
  const nodes = tasks.map((t) => ({
    id: t.id,
    kind: "execute" as const,
    parent_id: t.parent_id ?? null,
    role: t.assignee ? (orgById.get(t.assignee)?.name ?? t.assignee) : null,
    status: t.status,
    title: t.title,
  }));
  const edges = tasks.flatMap((t) => t.depends_on.map((from) => ({ from, kind: "depends_on", to: t.id })));
  return { nodes, edges };
}
