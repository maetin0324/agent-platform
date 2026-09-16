import type { Action, ReplayReport, TransitionResult } from "./types";

/**
 * action（状態変更）の結果をコンポーネントに渡す型（docs/DESIGN.md §6.3 の 2、docs/adr/0005 D2）。
 * サーバ専用 API を使わないのでクライアントからも import できる。実体は `app/taskd/actions.server.ts` が作る。
 */

/** taskd のエラー（`application/problem+json`）または接続不可を、画面表示用に整理したもの。 */
export interface ActionError {
  /** HTTP status（接続不可は 503） */
  status: number;
  /** Problem の `code`（接続不可は `unavailable`） */
  code: string;
  /** Problem の `detail`（taskd の文言そのまま） */
  detail: string;
  /** 409 `conflict` / `invalid_transition`: 「状態が変わりました」として表示し、loader の再検証で最新にする */
  conflict: boolean;
  /** 422 `validation` の `errors[]` のうち `field` があるもの（field → message[]）。文言は taskd のもの */
  fields: Record<string, string[]>;
  /** 422 `validation` の `errors[]` のうち `field` が無いもの、および `fields` にも入れた全文言 */
  messages: string[];
}

/** 状態変更（approve / reject / answer / cancel）の結果。`ok: false` でも例外にせず data として返す。 */
export type TransitionOutcome =
  | { ok: true; intent: Action; taskId: string; result: TransitionResult }
  | { ok: false; intent: Action; taskId: string; error: ActionError };

/** 作成（`POST /tasks` / `POST /plans`）の失敗。成功は詳細へ redirect するので data にならない。 */
export interface CreateFailure {
  ok: false;
  error: ActionError;
}

/** `POST /replay` の結果。taskd のエラー（503 `replay_in_progress` を含む）は例外にせず `{ok:false, error}` にする。 */
export type ReplayOutcome = { ok: true; report: ReplayReport } | { ok: false; error: ActionError };
