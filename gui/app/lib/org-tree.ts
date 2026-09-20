import type { OrgNode, Status, TaskSummary } from "~/celeris/types";
import { isSupportTask } from "~/lib/work-tree";

/**
 * 組織の木（SPEC §3.2、ADR-0033 D1）を `parent_id` から組む純粋関数。API（`GET /org`）は木にしない
 * （docs/gui/api.md §3.42「木は GUI が parent_id で組む」）ので、ここで組み立てる。
 * - 根の判定: `kind === "secretary"`、または `parent_id` が無いノード。
 * - 孤児（`parent_id` が指しているノードが無い）は根の下に出す（画面を壊さない。木を組めないことにはしない）。
 */

export interface OrgTreeNode {
  node: OrgNode;
  children: OrgTreeNode[];
}

export interface OrgTreeResult {
  roots: OrgTreeNode[];
  /** 根の下に付け替えた孤児の id（存在しない parent_id を指していたノード）。 */
  orphanIds: string[];
}

function orgSort(a: OrgNode, b: OrgNode): number {
  const pa = a.position ?? 0;
  const pb = b.position ?? 0;
  if (pa !== pb) return pa - pb;
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

export function buildOrgTree(items: OrgNode[]): OrgTreeResult {
  const byId = new Map(items.map((n) => [n.id, n]));
  const childrenOf = new Map<string, OrgNode[]>();
  const rootCandidates: OrgNode[] = [];
  const orphanIds: string[] = [];

  for (const node of items) {
    if (node.kind === "secretary" || node.parent_id == null) {
      rootCandidates.push(node);
      continue;
    }
    if (!byId.has(node.parent_id)) {
      orphanIds.push(node.id);
      continue;
    }
    const list = childrenOf.get(node.parent_id) ?? [];
    list.push(node);
    childrenOf.set(node.parent_id, list);
  }

  function build(node: OrgNode): OrgTreeNode {
    const kids = (childrenOf.get(node.id) ?? []).sort(orgSort).map(build);
    return { node, children: kids };
  }

  const roots = rootCandidates.sort(orgSort).map(build);

  if (roots.length > 0 && orphanIds.length > 0) {
    const target = roots.find((r) => r.node.kind === "secretary") ?? roots[0];
    for (const id of orphanIds) {
      const node = byId.get(id);
      if (node) target.children.push(build(node));
    }
    target.children.sort((a, b) => orgSort(a.node, b.node));
  }

  return { roots, orphanIds };
}

const OPEN_STATUSES = new Set<Status>(["draft", "ready", "running", "blocked", "reviewing"]);

export interface Workload {
  /** 終端（done/failed/cancelled）でないタスクの数（「抱えている」仕事）。 */
  open: number;
  /** 割り当てられた全タスクの数（終端を含む）。 */
  total: number;
}

/**
 * 割り当て（`assignee`）ごとのタスク件数を数える。`assignee` が無いタスクと、**裏方のタスク**
 * （`support`: 対話・報告のまとめ・承認待ち・レビュー。Phase 29）は数えない（SPEC の「タスクは裏方」＝
 * 人が見る「抱えている仕事」には入らない。ADR-0033 D8）。
 *
 * `GET /tasks` の `TaskSummary.assignee`（Phase 27 で追加）を直接数える。G13a では `assignee` が
 * `TaskSummary` に無かったため `GET /projects/{id}` を案件数ぶん束ねて代替していたが、
 * その N+1 呼び出しはやめた（celeris-requests.md R3 が解決済み）。
 */
export function countWorkload(tasks: TaskSummary[]): Map<string, Workload> {
  const counts = new Map<string, Workload>();
  for (const t of tasks) {
    if (!t.assignee || isSupportTask(t)) continue;
    const cur = counts.get(t.assignee) ?? { open: 0, total: 0 };
    cur.total += 1;
    if (OPEN_STATUSES.has(t.status)) cur.open += 1;
    counts.set(t.assignee, cur);
  }
  return counts;
}

/**
 * 組織の木の担当詳細に出す「抱えている仕事」一覧用に、`assignee` ごとにグループ化する
 * （裏方のタスクは除く。理由は `countWorkload` と同じ）。`TaskSummary` には `project_id` が無いため、
 * 案件名は添えられない（G13a の代替実装にあった `project_title` は、N+1 をやめた代わりに落とした）。
 */
export function tasksByAssignee(tasks: TaskSummary[]): Map<string, TaskSummary[]> {
  const byAssignee = new Map<string, TaskSummary[]>();
  for (const t of tasks) {
    if (!t.assignee || isSupportTask(t)) continue;
    const list = byAssignee.get(t.assignee) ?? [];
    list.push(t);
    byAssignee.set(t.assignee, list);
  }
  return byAssignee;
}
