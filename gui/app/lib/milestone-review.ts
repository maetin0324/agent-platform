import { isSupportTask } from "~/lib/work-tree";
import type { MilestoneDecideBody, MilestoneId, ProjectTaskView } from "~/taskd/types";

/**
 * 途中目標のレビューの対話（ADR-0038、Phase 41 / G13j）の純粋な判定・補助。
 * taskd 側の決定的な判定（`crates/taskd/src/milestone_review.rs::ready_milestones`）と
 * 同じ条件（裏方を除く、その途中目標のタスクだけを見る）を GUI 側でも使う。
 * ここには HTTP も React も持ち込まない（`~/lib/conversation.ts` と同じ方針）。
 */

/** taskd が「動いている」とみなす状態（ADR-0037 D5）。 */
const ACTIVE_STATUSES: ReadonlySet<ProjectTaskView["status"]> = new Set(["ready", "running", "reviewing", "blocked"]);

/** その途中目標に属する「仕事」だけ（裏方の対話・レビュー run 等は除く）。 */
export function milestoneWorkTasks(tasks: readonly ProjectTaskView[], milestoneId: MilestoneId): ProjectTaskView[] {
  return tasks.filter((t) => t.milestone_id === milestoneId && !isSupportTask(t));
}

/**
 * 途中目標が「止まっている」（taskd の `milestone_ready` と同じ条件: 動いているものが無く、
 * done が 1 件以上）か。秘書のレビューの返事（`review`）がまだ無いときに
 * 「秘書が結果をまとめています」を出すかどうかの判定に使う。
 */
export function milestoneIsStalled(tasks: readonly ProjectTaskView[], milestoneId: MilestoneId): boolean {
  const own = milestoneWorkTasks(tasks, milestoneId);
  if (own.length === 0) return false;
  if (own.some((t) => ACTIVE_STATUSES.has(t.status))) return false;
  return own.some((t) => t.status === "done");
}

/** `discuss` / `ng` は自由記述が必須（ADR-0038 D2。空なら taskd が 422 にする前に GUI 側で赤くする）。 */
export function milestoneDecisionNoteRequired(decision: MilestoneDecideBody["decision"]): boolean {
  return decision === "discuss" || decision === "ng";
}

/** フォームの送信直前に見る値から、送ってよいかを判定する（`note` の trim 後が空でないか）。 */
export function milestoneDecisionValid(decision: MilestoneDecideBody["decision"], note: string): boolean {
  return !milestoneDecisionNoteRequired(decision) || note.trim().length > 0;
}
