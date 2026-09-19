import { EmptyState } from "~/components/ui/misc";

/**
 * タスク画面の「ファイル」タブ（ADR-0044 D5 の 4 番目のタブ / ADR-0043 D6: worktree のファイル閲覧）。
 *
 * **いまは差し込み口だけ**。ADR-0043 A1 の API（worktree のツリーとファイル本体）が入ったら、
 * このファイルの中身を本物に差し替えるだけでタブが動く（タスク画面側は 1 行の差し込みしか持たない）。
 */
export default function TaskFiles({ taskId }: { taskId: string }) {
  return (
    <EmptyState icon="folder" title="ファイルはまだ見られません" data-testid="task-files-placeholder">
      worktree のファイル閲覧は ADR-0043 A1/A2 で入ります。
      それまでは下の「概要」の作業ディレクトリを手元で開いてください（タスク {taskId}）。
    </EmptyState>
  );
}
