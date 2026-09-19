import type {
  AccountCheckResponse,
  AccountLoginResult,
  AccountLoginStart,
  AccountView,
  Action,
  ApprovalDecideResult,
  ClusterConnectResult,
  ClusterConnectStart,
  MessageAccepted,
  Milestone,
  MilestoneDecided,
  NotifyTestResult,
  OrgNode,
  Project,
  ProjectPlanAccepted,
  ProviderCheckResponse,
  ProviderConfigView1,
  ReleasePromoteAccepted,
  ReloadResult,
  ReplayReport,
  ReportsNotifiedResult,
  ReportsReadResult,
  RetryResult,
  SecretPutResult,
  StandingRule,
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

/**
 * 失敗した仕事をやり直す（Phase 31。実機の事故、2026-09-18。`POST /tasks/{id}/retry`、
 * docs/taskd-api-v1.md §3.63）。`taskId` は**元の**タスク（対象が固定できるように）、
 * `result.task_id` が**新しく作られた**タスク（成功したら画面はそちらへ遷移する）。
 */
export type RetryOutcome =
  | { ok: true; taskId: string; result: RetryResult }
  | { ok: false; taskId: string; error: ActionError };

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
/**
 * API キー（秘密）の管理（ADR-0030 D3/D4）: `PUT/DELETE /secrets/{id}` の結果。値は一切載せない
 * （taskd の応答自体に値が無い。ADR-0030 D3）。
 */
export type SecretOpOutcome =
  | { ok: true; op: "put"; id: string; secret: SecretPutResult }
  | { ok: true; op: "delete"; id: string }
  | { ok: false; op: "put" | "delete"; id: string; error: ActionError };

/** `/accounts` の API キー節の action が返すデータ。`reload` は 2xx のときだけ入る（ADR-0030 D4）。 */
export interface SecretActionResult {
  op: SecretOpOutcome;
  reload?: ReloadOutcome;
}

/**
 * クラスタへの接続の中継（ADR-0032 D5/D6、docs/taskd-api-v1.md §3.39〜3.41）:
 * `POST /clusters/{id}/connect` / `POST /clusters/{id}/connect/code` / `DELETE /clusters/{id}/connect` の結果。
 * taskd のエラーは例外にせず `{ok:false, error}` にする（401 `unauthorized` を含む）。**`POST /reload` は呼ばない**
 * （接続を張っても `taskd.toml` の設定は変わらないので不要。プロバイダ・秘密の管理とはここが違う。ADR-0032）。
 * コード自体（`code` フォーム値）はここにも `fetcher.data` にも載せない（結果の可否と `detail` だけ。ADR-0032 D6）。
 */
export type ClusterConnectOutcome =
  | { ok: true; op: "connect_start"; id: string; start: ClusterConnectStart }
  | { ok: true; op: "connect_code"; id: string; result: ClusterConnectResult }
  | { ok: true; op: "connect_cancel"; id: string }
  | {
      ok: false;
      op: "connect_start" | "connect_code" | "connect_cancel";
      id: string;
      error: ActionError;
    };

/**
 * 組織の木の編集（ADR-0033 D1、docs/taskd-api-v1.md §3.43〜3.45。**管理系**、`token_file` 未設定でも 401）:
 * `POST/PATCH/DELETE /org...` の結果。taskd のエラーは例外にせず `{ok:false, error}` にする
 * （409 `org_node_exists` / `org_node_in_use`、422 `validation`、401 `unauthorized` を含む）。
 * 組織は設定ではなく DB が正（ADR-0033 D1）なので、プロバイダ・秘密とは違い `POST /reload` は呼ばない。
 */
export type OrgOpOutcome =
  | { ok: true; op: "create" | "patch"; id: string; node: OrgNode }
  | { ok: true; op: "delete"; id: string }
  | { ok: false; op: "create" | "patch" | "delete"; id: string; error: ActionError };

/**
 * 案件・途中目標の状態変更（ADR-0033 D2、docs/taskd-api-v1.md §3.46〜3.49）: `PATCH /projects/{id}` /
 * `POST /projects/{id}/milestones` / `PATCH /milestones/{id}` の結果。読み取り・案件操作は通常の要求
 * （管理系ではない。3.42〜3.49 の前書き）。taskd のエラーは例外にせず `{ok:false, error}` にする。
 */
export type ProjectOpOutcome =
  // `project_workspace` は作業場所の保存・消去（`PATCH /projects/{id}` の `workspace`。ADR-0039 D1、
  // Phase G13k）。`status` の変更と同じ `Project` を返す形なのでまとめる。
  | { ok: true; op: "project_status" | "project_workspace"; project: Project }
  | { ok: true; op: "milestone_create" | "milestone_status"; milestone: Milestone }
  // 「この方針で進める」（`POST /projects/{id}/plan`。**管理系**、202。docs/taskd-api-v1.md §3.61、Phase 29）。
  | { ok: true; op: "project_plan"; accepted: ProjectPlanAccepted }
  // 途中目標の判定（`POST /milestones/{id}/decide`。**管理系**、202。ADR-0038 D2、docs/taskd-api-v1.md §3.63、
  // Phase 41 / G13j）。`ok` / `discuss` / `ng` のどれでも同じ形（`decided.decision` を見て画面が出し分ける）。
  | { ok: true; op: "milestone_decide"; decided: MilestoneDecided }
  | {
      ok: false;
      op:
        | "project_status"
        | "project_workspace"
        | "milestone_create"
        | "milestone_status"
        | "project_plan"
        | "milestone_decide";
      error: ActionError;
    };

/**
 * 「報告」画面（`/reports`）の既読・通知（ADR-0033 D3、docs/taskd-api-v1.md §3.52〜3.53。**管理系**）:
 * `POST /reports/read` / `POST /reports/notified` の結果。taskd のエラーは例外にせず `{ok:false, error}` にする
 * （401 `unauthorized` を含む）。`reports_notified` は `NotificationsWatcher`（`app/components`）が
 * ブラウザ通知を出した直後にも呼ぶ（画面を開いていなくてもよい resource 呼び出し）。
 */
export type ReportOpOutcome =
  | { ok: true; op: "reports_read"; ids: string[]; result: ReportsReadResult }
  | { ok: true; op: "reports_notified"; result: ReportsNotifiedResult }
  | { ok: false; op: "reports_read" | "reports_notified"; error: ActionError };

/**
 * Discord へのテスト送信（ADR-0037 D4、docs/taskd-api-v1.md §3.65。**管理系**、`token_file` 未設定でも 401）:
 * `POST /notify/test` の結果。taskd のエラーは例外にせず `{ok:false, error}` にする（401 `unauthorized` /
 * 409 `notify_unavailable`＝秘密が未登録を含む）。`ok: true` でも `result.ok` が `false`（送り先が 404 を
 * 返した等）はありうるので、画面はどちらの `ok` も見て表示を分ける。
 */
export type NotifyTestOutcome =
  | { ok: true; op: "notify_test"; result: NotifyTestResult }
  | { ok: false; op: "notify_test"; error: ActionError };

/**
 * 対話（ADR-0033 D4、docs/taskd-api-v1.md §3.55。`POST /org/{id}/messages` は**管理系**）:
 * 話しかけた結果（202 `{message_id, task_id}` をそのまま載せる）と、「新しい案件として」送ったときの
 * `POST /projects`（201）の結果。返事は同期では返らないので、画面は `message_id` を手がかりに
 * `GET /org/{id}/messages` を引き直して待つ（`~/lib/conversation.ts` の `replyArrived`）。
 */
export type ConversationOpOutcome =
  | { ok: true; op: "send"; accepted: MessageAccepted }
  | { ok: true; op: "new_project"; project: Project }
  | { ok: false; op: "send" | "new_project"; error: ActionError };

/**
 * 認可の決定（ADR-0033 D5、docs/taskd-api-v1.md §3.57。**管理系**、`token_file` 未設定でも 401）:
 * `POST /approvals/{id}/decide` の結果。`once`/`standing`/`denied` のいずれでも同じ形（`result.approval.decision`
 * を見て画面に出す）。taskd のエラーは例外にせず `{ok:false, error}` にする。
 */
export type ApprovalOpOutcome =
  | { ok: true; op: "decide"; id: string; result: ApprovalDecideResult }
  | { ok: false; op: "decide"; id: string; error: ActionError };

/**
 * 永続の認可の追加・削除（ADR-0033 D5、docs/taskd-api-v1.md §3.59〜3.60。**管理系**）:
 * `POST /standing-rules` / `DELETE /standing-rules/{id}` の結果。taskd のエラーは例外にせず
 * `{ok:false, error}` にする（401 `unauthorized` を含む）。
 */
export type StandingRuleOpOutcome =
  | { ok: true; op: "create"; id: string; rule: StandingRule }
  | { ok: true; op: "delete"; id: string }
  | { ok: false; op: "create" | "delete"; id: string; error: ActionError };

/**
 * リリースの昇格（ADR-0040 D6、docs/taskd-api-v1.md §3.67。**管理系**、`token_file` 未設定でも 401）:
 * `POST /releases/{sha12}/promote` の結果。taskd のエラーは例外にせず `{ok:false, error}` にする
 * （404 `release_not_found` / 409 `release_not_promotable`＝未検証・既に current・既に昇格中 /
 * 401 `unauthorized` を含む）。202 は「`promote.sh` を起こした」だけで、**昇格の完了ではない**
 * （進行は `GET /releases` の `instances` を読み直して見る）。
 */
export type ReleasePromoteOutcome =
  | { ok: true; op: "release_promote"; sha12: string; accepted: ReleasePromoteAccepted }
  | { ok: false; op: "release_promote"; sha12: string; error: ActionError };

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
