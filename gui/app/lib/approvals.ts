import type { Approval, DaemonView, OrgNode, Project, StandingRule } from "~/taskd/types";

/**
 * 「認可」（SPEC §3.6・§4 の 5、ADR-0033 D5、docs/taskd-api-v1.md §3.56〜3.60）の純粋関数。
 * `/approvals` の loader / コンポーネントから使う（`~/lib/reports.ts` と同じ作り: 判断・計算はここに集めて
 * 純粋関数としてテストする。DOM を描画する unit テストはこのリポジトリに無い。G10-U1）。
 */

/** 案件名の解決（`~/lib/reports.ts` の `reportProjectName` と同じ作り）。`project_id` が無ければ「案件なし」。 */
export function approvalProjectName(approval: Pick<Approval, "project_id">, projects: Project[]): string {
  if (!approval.project_id) return "案件なし";
  return projects.find((p) => p.id === approval.project_id)?.title ?? approval.project_id;
}

/** 聞いてきたノード名の解決（`node_id` → `GET /org` の `name`）。見つからなければ id をそのまま出す。 */
export function approvalNodeName(approval: Pick<Approval, "node_id">, org: OrgNode[]): string {
  return org.find((n) => n.id === approval.node_id)?.name ?? approval.node_id;
}

/** 永続の認可の宛先の表示（`node_id` が無ければ「全員」）。 */
export function standingRuleTargetName(rule: Pick<StandingRule, "node_id">, org: OrgNode[]): string {
  if (!rule.node_id) return "全員";
  return org.find((n) => n.id === rule.node_id)?.name ?? rule.node_id;
}

/**
 * ナビの「認可」バッジの件数（`DaemonSnapshot.approvals_pending`。古いスナップショットには無いので既定は 0。
 * §3.20 の追加、`~/lib/reports.ts` の `reportsBadgeTone` と同じ理由で純粋関数にしてある）。
 */
export function approvalsPendingCount(daemon: DaemonView | null | undefined): number {
  return daemon?.snapshot?.approvals_pending ?? 0;
}
