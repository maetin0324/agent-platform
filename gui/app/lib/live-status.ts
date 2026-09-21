/**
 * `role="status"`（ライブリージョン）をどの画面の状態バッジに付けるかを決める純粋関数（Phase 76 の
 * 未解決事項 U-G32-2 の解消、Phase 84）。
 *
 * root の SSE（`~/hooks/useCelerisStream.ts`）は `task.event`/`daemon`/`reset` を受けるたびに「今開いて
 * いる画面の loader」を再検証するだけで、celeris は「何が変わったか」を送ってこない（差分検出はできない）。
 * それでもライブリージョンの実際の挙動は「DOM のテキストが実際に変わったときだけ読み上げる」ので、対象を
 * 絞る意味は「SSE の再検証で書き換わらない・書き換わっても『見続けている画面』ではない場所にまで付けて回る
 * 事故を防ぐ」ことに限られる（Phase 76 が「全バッジに付けると騒がしくなる恐れ」として保留にした懸念への
 * 回答）。Console（Console の task ブロック）は Phase 76 で個別に対応済みなのでここには含めない。
 *
 * 各画面はここに固定の `screen` 値を渡すだけで、「付ける／付けない」の判断が 1 か所に集まる。
 */
export type StatusBadgeScreen =
  /** `/board`（ボードのカード）。 */
  | "board"
  /** `/tasks`（一覧の行）。 */
  | "task-list"
  /** `/tasks/:id`（詳細ヘッダ）。 */
  | "task-detail"
  /** `/tasks/new`（depends_on の候補一覧。作るときに一度読むだけで、開いたまま流れを追う画面ではない）。 */
  | "task-new-candidates"
  /** 案件詳細の連携節（`~/components/ProjectIntegrations.tsx`）。 */
  | "project-integrations"
  /** `/help` の説明用の例（実データではない）。 */
  | "help-example";

const LIVE_STATUS_SCREENS: ReadonlySet<StatusBadgeScreen> = new Set(["board", "task-list", "task-detail"]);

/** その画面の状態バッジに `role="status"` を付けるべきか。 */
export function isLiveStatusScreen(screen: StatusBadgeScreen): boolean {
  return LIVE_STATUS_SCREENS.has(screen);
}
