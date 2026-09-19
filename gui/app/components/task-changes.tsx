import { EmptyState } from "~/components/ui/misc";

/**
 * タスク画面の「変更」タブ（ADR-0044 D5 の 3 番目のタブ / ADR-0043 D5: 差分・PR・3 ボタン）。
 *
 * **いまは差し込み口だけ**。ADR-0043 A2 の API（`GET /tasks/{id}/changes` ほか）が入ったら、
 * このファイルの中身を本物に差し替えるだけでタブが動く（タスク画面側は 1 行の差し込みしか持たない）。
 */
export default function TaskChanges({ taskId }: { taskId: string }) {
  return (
    <EmptyState icon="gitBranch" title="変更はまだ見られません" data-testid="task-changes-placeholder">
      差分・PR・取り込み（merge / PR / discard）は ADR-0043 A1/A2 で入ります。 それまでは手元の worktree
      で確認してください（タスク {taskId}）。
    </EmptyState>
  );
}
