import type { Milestone, ProjectDetail, TaskId } from "~/celeris/types";

/**
 * 裏方のタスク（`/tasks`、`/tasks/:id`）から**案件・担当・途中目標**へ戻れるようにするための索引
 * （Phase G13f-1、監査 M2「裏方から戻れる」）。
 *
 * `TaskSummary` には `project_id` / `milestone_id` が無い（`Task` にはある）ため、一覧では
 * `GET /projects` + 各案件の `GET /projects/{id}` の `tasks[]` から「どのタスクがどの案件・どの途中目標か」を
 * 引ける表を作る。判断はしていない（celeris が返した対応をそのまま写すだけ）。
 * celeris 側に `TaskSummary.project_id` が入ったら、この索引は不要になる（`docs/celeris-requests.md` R3 のメモ）。
 */

export interface TaskPlacement {
  projectId: string;
  projectTitle: string;
  milestoneId: string | null;
  milestoneTitle: string | null;
}

export function milestoneTitle(
  milestones: readonly Milestone[],
  milestoneId: string | null | undefined,
): string | null {
  if (!milestoneId) return null;
  const found = milestones.find((m) => m.id === milestoneId);
  return found ? `#${found.seq} ${found.title}` : milestoneId;
}

/** 案件の詳細（`GET /projects/{id}` の応答）の配列から、タスク id → 置かれている場所の表を作る。 */
export function buildTaskPlacements(details: readonly ProjectDetail[]): Record<TaskId, TaskPlacement> {
  const placements: Record<TaskId, TaskPlacement> = {};
  for (const detail of details) {
    for (const task of detail.tasks) {
      placements[task.id] = {
        projectId: detail.project.id,
        projectTitle: detail.project.title,
        milestoneId: task.milestone_id ?? null,
        milestoneTitle: milestoneTitle(detail.milestones, task.milestone_id),
      };
    }
  }
  return placements;
}
