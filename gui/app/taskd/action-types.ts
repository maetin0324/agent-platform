import type {
  AccountCheckResponse,
  AccountLoginResult,
  AccountLoginStart,
  AccountView,
  Action,
  ProviderCheckResponse,
  ProviderConfigView1,
  ReloadResult,
  ReplayReport,
  TransitionResult,
} from "./types";

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

/**
 * プロバイダ管理（ADR-GUI-0012 D2）: `POST/PATCH/DELETE /providers...` の結果。
 * taskd のエラーは例外にせず `{ok:false, error}` にする（401 `unauthorized` を含む。`ErrorFlash` が案内文を足す）。
 */
export type ProviderOpOutcome =
  | { ok: true; op: "create" | "patch"; id: string; provider: ProviderConfigView1 }
  | { ok: true; op: "delete"; id: string }
  | { ok: false; op: "create" | "patch" | "delete"; id: string; error: ActionError };

export type ProviderCheckOutcome =
  | { ok: true; op: "check"; id: string; result: ProviderCheckResponse }
  | { ok: false; op: "check"; id: string; error: ActionError };

/** `POST /reload`。プロバイダの追加・変更・削除が 2xx のときだけ続けて呼ぶ（ADR-GUI-0012 D2）。 */
export type ReloadOutcome = { ok: true; result: ReloadResult } | { ok: false; error: ActionError };

/** `/providers` の action が返すデータ。`reload` は `op` が create/patch/delete で成功したときだけ入る。 */
export interface ProviderActionResult {
  op: ProviderOpOutcome | ProviderCheckOutcome;
  reload?: ReloadOutcome;
}

/**
 * アカウントのアダプタ（ADR-0025 D1）。`(adapter, id)` でアカウントを識別する。GUI から見た「使えるアダプタ」の
 * 全体はこの 2 つ（taskd 側の `AccountAdapter`）。
 */
export type AccountAdapter = "claude-code" | "codex";

/**
 * アカウントのプール管理（ADR-GUI-0012 D3、ADR-0025 D5/D6）: `/accounts...` の結果。taskd のエラーは例外にせず
 * `{ok:false, error}` にする（401 `unauthorized` を含む）。`adapter` は呼び出しに使ったアダプタ（`?adapter=`）で、
 * `AccountCard` が自分宛ての結果かどうかを id と一緒に判定するのに使う。
 */
export type AccountOpOutcome =
  | { ok: true; op: "create"; id: string; adapter: AccountAdapter; account: AccountView }
  | { ok: true; op: "delete"; id: string; adapter: AccountAdapter }
  | { ok: true; op: "check"; id: string; adapter: AccountAdapter; result: AccountCheckResponse }
  | { ok: true; op: "login_start"; id: string; adapter: AccountAdapter; login: AccountLoginStart }
  | { ok: true; op: "login_code"; id: string; adapter: AccountAdapter; result: AccountLoginResult }
  | { ok: true; op: "login_cancel"; id: string; adapter: AccountAdapter }
  | {
      ok: false;
      op: "create" | "delete" | "check" | "login_start" | "login_code" | "login_cancel";
      id: string;
      adapter: AccountAdapter;
      error: ActionError;
    };
