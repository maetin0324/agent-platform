import type { Approval, DaemonView, OrgNode, Project, StandingRule } from "~/taskd/types";

/**
 * 「認可」（SPEC §3.6・§4 の 5、ADR-0033 D5、docs/taskd-api-v1.md §3.56〜3.60）の純粋関数。
 * `/approvals` の loader / コンポーネントから使う（`~/lib/reports.ts` と同じ作り: 判断・計算はここに集めて
 * 純粋関数としてテストする。DOM を描画する unit テストはこのリポジトリに無い。G10-U1）。
 */

/**
 * 未決の要求（上）と決めたものの履歴（下）に分ける（`Approval.decision` が無い = 未決。§3.56「decision は
 * once/standing/denied（未決定は無い）」）。**`GET /approvals?pending=false` には頼らない**:
 * 実機で `pending=false` がフィルタせず全件を返す（`pending=true` は正しく未決だけに絞れる）ことを確認したため、
 * `GET /approvals`（フィルタ無し）を 1 回だけ呼び、ここで `decision` の有無だけを見て分ける
 * （`decision` は応答のドキュメント化されたフィールドなので、taskd に無い判断を GUI に足すことにはならない。
 * `docs/taskd-requests.md` R5 に記録済み）。
 */
export function splitApprovals(items: Approval[]): { pending: Approval[]; decided: Approval[] } {
  const pending = items.filter((a) => a.decision == null);
  const decided = items.filter((a) => a.decision != null);
  return { pending, decided };
}

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
