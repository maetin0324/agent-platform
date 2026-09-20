import type { Approval, DaemonView, OrgNode, Project, StandingRule } from "~/celeris/types";

/**
 * 「認可」（SPEC §3.6・§4 の 5、ADR-0033 D5、docs/celeris-api-v1.md §3.56〜3.60）の純粋関数。
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

/** 同じ文面の未決の要求をまとめた 1 枚（監査 8）。`approvals` は新しい順のまま（元の並びを保つ）。 */
export interface ApprovalGroup {
  /** 代表（いちばん新しい 1 件。時刻・案件・担当の表示に使う） */
  head: Approval;
  /** この文面で未決の要求すべて（答えるときは全部にまとめて同じ答えを送る） */
  approvals: Approval[];
}

/**
 * 未決の要求を**同じ文面**でまとめる（監査 8「同一文面の未決要求はまとめて 1 枚にし『N 件』と出す」）。
 * 文面の同一性は前後の空白を落とした完全一致だけで判断する（言い換えの解釈はしない。GUI は判断を作らない）。
 * 並びは celeris が返した順（新しい順）を保ち、同じ文面の最初の 1 件の位置にまとめる。
 */
export function groupApprovals(items: readonly Approval[]): ApprovalGroup[] {
  const groups: ApprovalGroup[] = [];
  const byQuestion = new Map<string, ApprovalGroup>();
  for (const approval of items) {
    const key = approval.question.trim();
    const found = byQuestion.get(key);
    if (found) {
      found.approvals.push(approval);
      continue;
    }
    const group: ApprovalGroup = { head: approval, approvals: [approval] };
    byQuestion.set(key, group);
    groups.push(group);
  }
  return groups;
}

/** まとめた 1 枚に出す担当の名前（重複を除く。順は元のまま）。 */
export function approvalGroupNodeNames(group: ApprovalGroup, org: OrgNode[]): string[] {
  const names: string[] = [];
  for (const a of group.approvals) {
    const name = approvalNodeName(a, org);
    if (!names.includes(name)) names.push(name);
  }
  return names;
}

/**
 * ナビの「認可」バッジの件数（`DaemonSnapshot.approvals_pending`。古いスナップショットには無いので既定は 0。
 * §3.20 の追加、`~/lib/reports.ts` の `reportsBadgeTone` と同じ理由で純粋関数にしてある）。
 */
export function approvalsPendingCount(daemon: DaemonView | null | undefined): number {
  return daemon?.snapshot?.approvals_pending ?? 0;
}
