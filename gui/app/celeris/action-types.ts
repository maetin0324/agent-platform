import type {
  AccountCheckResponse,
  AccountLoginResult,
  AccountLoginStart,
  AccountView,
  Action,
  ApprovalDecideResult,
  ClusterConnectResult,
  ClusterConnectStart,
  CommentResult,
  ConsoleInstructAccepted,
  DocPageResult,
  DocsInitResult,
  EditResult,
  IntegrateResult,
  KnowledgePageResult,
  KnowledgeRejectResult,
  MessageAccepted,
  Milestone,
  MilestoneDecided,
  MilestoneLifecycle,
  NotifyTestResult,
  OrgNode,
  Project,
  ProjectLifecycle,
  ProjectPlanAccepted,
  ProjectRepo,
  ProviderCheckResponse,
  ProviderConfigView1,
  ReleasePromoteAccepted,
  ReloadResult,
  ReplayReport,
  ReportsNotifiedResult,
  ReportsReadResult,
  RetryResult,
  SecretPutResult,
  SkillPutResult,
  StandingRule,
  Task,
  TransitionResult,
} from "./types";

/**
 * action（状態変更）の結果をコンポーネントに渡す型（docs/DESIGN.md §6.3 の 2、docs/adr/0005 D2）。
 * サーバ専用 API を使わないのでクライアントからも import できる。実体は `app/celeris/actions.server.ts` が作る。
 */

/** celeris のエラー（`application/problem+json`）または接続不可を、画面表示用に整理したもの。 */
export interface ActionError {
  /** HTTP status（接続不可は 503） */
  status: number;
  /** Problem の `code`（接続不可は `unavailable`） */
  code: string;
  /** Problem の `detail`（celeris の文言そのまま） */
  detail: string;
  /** 409 `conflict` / `invalid_transition`: 「状態が変わりました」として表示し、loader の再検証で最新にする */
  conflict: boolean;
  /** 422 `validation` の `errors[]` のうち `field` があるもの（field → message[]）。文言は celeris のもの */
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
 * docs/celeris-api-v1.md §3.63）。`taskId` は**元の**タスク（対象が固定できるように）、
 * `result.task_id` が**新しく作られた**タスク（成功したら画面はそちらへ遷移する）。
 */
export type RetryOutcome =
  | { ok: true; taskId: string; result: RetryResult }
  | { ok: false; taskId: string; error: ActionError };

/**
 * タスクの編集（ADR-0044 D1、Phase 53。`PATCH /tasks/{id}`。**管理系**）。
 * `result.fields` は**実際に変わった項目**（何も変わらなければ空）なので、画面はこれをそのまま出す
 * （GUI 側で差分を計算し直さない）。409（終端のタスク・`expected_status` 不一致）も例外にせず
 * `{ok:false, error}` にする。
 */
export type TaskEditOutcome =
  | { ok: true; op: "edit"; taskId: string; result: EditResult }
  | { ok: false; op: "edit"; taskId: string; error: ActionError };

/**
 * タスクへのコメント（ADR-0044 D2、Phase 53。`POST /tasks/{id}/comments`。**管理系**）。
 * `result.effect` が「何が起きたか」（`stored` / `interrupted` / `answered` / `terminal`）で、
 * 状態が動いたときだけ `result.transition` が付く。判断は celeris がした結果なので GUI は writes せず出すだけ。
 */
export type TaskCommentOutcome =
  | { ok: true; op: "comment"; taskId: string; result: CommentResult }
  | { ok: false; op: "comment"; taskId: string; error: ActionError };

/**
 * 終端のタスクの再開（ADR-0044 D2、Phase 53。`POST /tasks/{id}/reopen`。**管理系**）。
 * `done`/`failed` → `ready`。`cancelled` は 409（worktree が無いので「やり直す」= `retry` を使う）。
 */
export type TaskReopenOutcome =
  | { ok: true; op: "reopen"; taskId: string; result: TransitionResult }
  | { ok: false; op: "reopen"; taskId: string; error: ActionError };

/** 作成（`POST /tasks` / `POST /plans`）の失敗。成功は詳細へ redirect するので data にならない。 */
export interface CreateFailure {
  ok: false;
  error: ActionError;
}

/** `POST /replay` の結果。celeris のエラー（503 `replay_in_progress` を含む）は例外にせず `{ok:false, error}` にする。 */
export type ReplayOutcome = { ok: true; report: ReplayReport } | { ok: false; error: ActionError };

/**
 * プロバイダ管理（ADR-GUI-0012 D2）: `POST/PATCH/DELETE /providers...` の結果。
 * celeris のエラーは例外にせず `{ok:false, error}` にする（401 `unauthorized` を含む。`ErrorFlash` が案内文を足す）。
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
 * 全体はこの 2 つ（celeris 側の `AccountAdapter`）。
 */
export type AccountAdapter = "claude-code" | "codex";

/**
 * アカウントのプール管理（ADR-GUI-0012 D3、ADR-0025 D5/D6）: `/accounts...` の結果。celeris のエラーは例外にせず
 * `{ok:false, error}` にする（401 `unauthorized` を含む）。`adapter` は呼び出しに使ったアダプタ（`?adapter=`）で、
 * `AccountCard` が自分宛ての結果かどうかを id と一緒に判定するのに使う。
 */
/**
 * API キー（秘密）の管理（ADR-0030 D3/D4）: `PUT/DELETE /secrets/{id}` の結果。値は一切載せない
 * （celeris の応答自体に値が無い。ADR-0030 D3）。
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
 * クラスタへの接続の中継（ADR-0032 D5/D6、docs/celeris-api-v1.md §3.39〜3.41）:
 * `POST /clusters/{id}/connect` / `POST /clusters/{id}/connect/code` / `DELETE /clusters/{id}/connect` の結果。
 * celeris のエラーは例外にせず `{ok:false, error}` にする（401 `unauthorized` を含む）。**`POST /reload` は呼ばない**
 * （接続を張っても `config.toml` の設定は変わらないので不要。プロバイダ・秘密の管理とはここが違う。ADR-0032）。
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
 * 組織の木の編集（ADR-0033 D1、docs/celeris-api-v1.md §3.43〜3.45。**管理系**、`token_file` 未設定でも 401）:
 * `POST/PATCH/DELETE /org...` の結果。celeris のエラーは例外にせず `{ok:false, error}` にする
 * （409 `org_node_exists` / `org_node_in_use`、422 `validation`、401 `unauthorized` を含む）。
 * 組織は設定ではなく DB が正（ADR-0033 D1）なので、プロバイダ・秘密とは違い `POST /reload` は呼ばない。
 */
export type OrgOpOutcome =
  | { ok: true; op: "create" | "patch"; id: string; node: OrgNode }
  | { ok: true; op: "delete"; id: string }
  | { ok: false; op: "create" | "patch" | "delete"; id: string; error: ActionError };

/**
 * 案件・途中目標の状態変更（ADR-0033 D2、docs/celeris-api-v1.md §3.46〜3.49）: `PATCH /projects/{id}` /
 * `POST /projects/{id}/milestones` / `PATCH /milestones/{id}` の結果。**Phase 55（ADR-0044 D6）から
 * この 3 つも管理系**（`token_file` 未設定でも 401。読み取りの `GET /projects` だけが通常の要求）。
 * celeris のエラーは例外にせず `{ok:false, error}` にする。
 */
export type ProjectOpOutcome =
  // `project_workspace` は作業場所の保存・消去（`PATCH /projects/{id}` の `workspace`。ADR-0039 D1、
  // Phase G13k）。`status` の変更と同じ `Project` を返す形なのでまとめる。
  | { ok: true; op: "project_status" | "project_workspace"; project: Project }
  | { ok: true; op: "milestone_create" | "milestone_status"; milestone: Milestone }
  // 「この方針で進める」（`POST /projects/{id}/plan`。**管理系**、202。docs/celeris-api-v1.md §3.61、Phase 29）。
  | { ok: true; op: "project_plan"; accepted: ProjectPlanAccepted }
  // 途中目標の判定（`POST /milestones/{id}/decide`。**管理系**、202。ADR-0038 D2、docs/celeris-api-v1.md §3.63、
  // Phase 41 / G13j）。`ok` / `discuss` / `ng` のどれでも同じ形（`decided.decision` を見て画面が出し分ける）。
  | { ok: true; op: "milestone_decide"; decided: MilestoneDecided }
  // 案件のリポジトリ（ADR-0043 D1、docs/celeris-api-v1.md §3.68〜3.71。Phase 52 / G16）。
  // 変更系は**管理系**（`token_file` 未設定でも 401）。`repo_primary` は `PATCH /repos/{id}` の
  // `is_primary: true` で、他の行の付け替えは celeris が行う（GUI は再計算しない）。
  | { ok: true; op: "repo_create" | "repo_patch" | "repo_primary"; repo: ProjectRepo }
  | { ok: true; op: "repo_delete"; repoId: string }
  // ADR-0044 D1（Phase 53）: 案件・途中目標から人がタスクを足す（`POST /tasks`。**管理系**、201）。
  // 人が作ったタスクは `ready`（人は Go を出す側なので draft を挟まない）。
  | { ok: true; op: "task_create"; task: Task }
  // 中止・一時停止・アーカイブ（ADR-0044 D6、docs/celeris-api-v1.md §3.84〜3.91。Phase 55 / G19。
  // **管理系**、200）。`lifecycle.cancelled_tasks` / `cancelled_milestones` は**`cancel` のときだけ**
  // 中身が入る（他は空配列）ので、画面はそれをそのまま件数として出す（連鎖を GUI で計算し直さない）。
  | { ok: true; op: ProjectLifecycleOp; lifecycle: ProjectLifecycle }
  | { ok: true; op: MilestoneLifecycleOp; lifecycle: MilestoneLifecycle }
  | {
      ok: false;
      op:
        | "project_status"
        | "project_workspace"
        | "milestone_create"
        | "milestone_status"
        | "project_plan"
        | "milestone_decide"
        | "repo_create"
        | "repo_patch"
        | "repo_primary"
        | "repo_delete"
        | "task_create"
        | ProjectLifecycleOp
        | MilestoneLifecycleOp;
      error: ActionError;
    };

/** 案件の中止・一時停止・アーカイブ（`POST /projects/{id}/{cancel|pause|resume|archive|unarchive}`）。 */
export type ProjectLifecycleOp =
  | "project_cancel"
  | "project_pause"
  | "project_resume"
  | "project_archive"
  | "project_unarchive";

/** 途中目標の中止・一時停止（`POST /milestones/{id}/{cancel|pause|resume}`）。 */
export type MilestoneLifecycleOp = "milestone_cancel" | "milestone_pause" | "milestone_resume";

/**
 * 「報告」画面（`/reports`）の既読・通知（ADR-0033 D3、docs/celeris-api-v1.md §3.52〜3.53。**管理系**）:
 * `POST /reports/read` / `POST /reports/notified` の結果。celeris のエラーは例外にせず `{ok:false, error}` にする
 * （401 `unauthorized` を含む）。`reports_notified` は `NotificationsWatcher`（`app/components`）が
 * ブラウザ通知を出した直後にも呼ぶ（画面を開いていなくてもよい resource 呼び出し）。
 */
export type ReportOpOutcome =
  | { ok: true; op: "reports_read"; ids: string[]; result: ReportsReadResult }
  | { ok: true; op: "reports_notified"; result: ReportsNotifiedResult }
  | { ok: false; op: "reports_read" | "reports_notified"; error: ActionError };

/**
 * Discord へのテスト送信（ADR-0037 D4、docs/celeris-api-v1.md §3.65。**管理系**、`token_file` 未設定でも 401）:
 * `POST /notify/test` の結果。celeris のエラーは例外にせず `{ok:false, error}` にする（401 `unauthorized` /
 * 409 `notify_unavailable`＝秘密が未登録を含む）。`ok: true` でも `result.ok` が `false`（送り先が 404 を
 * 返した等）はありうるので、画面はどちらの `ok` も見て表示を分ける。
 */
export type NotifyTestOutcome =
  | { ok: true; op: "notify_test"; result: NotifyTestResult }
  | { ok: false; op: "notify_test"; error: ActionError };

/**
 * 対話（ADR-0033 D4、docs/celeris-api-v1.md §3.55。`POST /org/{id}/messages` は**管理系**）:
 * 話しかけた結果（202 `{message_id, task_id}` をそのまま載せる）と、「新しい案件として」送ったときの
 * `POST /projects`（201）の結果。返事は同期では返らないので、画面は `message_id` を手がかりに
 * `GET /org/{id}/messages` を引き直して待つ（`~/lib/conversation.ts` の `replyArrived`）。
 */
export type ConversationOpOutcome =
  | { ok: true; op: "send"; accepted: MessageAccepted }
  | { ok: true; op: "new_project"; project: Project }
  | { ok: false; op: "send" | "new_project"; error: ActionError };

/**
 * 認可の決定（ADR-0033 D5、docs/celeris-api-v1.md §3.57。**管理系**、`token_file` 未設定でも 401）:
 * `POST /approvals/{id}/decide` の結果。`once`/`standing`/`denied` のいずれでも同じ形（`result.approval.decision`
 * を見て画面に出す）。celeris のエラーは例外にせず `{ok:false, error}` にする。
 */
export type ApprovalOpOutcome =
  | { ok: true; op: "decide"; id: string; result: ApprovalDecideResult }
  | { ok: false; op: "decide"; id: string; error: ActionError };

/**
 * 永続の認可の追加・削除（ADR-0033 D5、docs/celeris-api-v1.md §3.59〜3.60。**管理系**）:
 * `POST /standing-rules` / `DELETE /standing-rules/{id}` の結果。celeris のエラーは例外にせず
 * `{ok:false, error}` にする（401 `unauthorized` を含む）。
 */
export type StandingRuleOpOutcome =
  | { ok: true; op: "create"; id: string; rule: StandingRule }
  | { ok: true; op: "delete"; id: string }
  | { ok: false; op: "create" | "delete"; id: string; error: ActionError };

/**
 * リリースの昇格（ADR-0040 D6、docs/celeris-api-v1.md §3.67。**管理系**、`token_file` 未設定でも 401）:
 * `POST /releases/{sha12}/promote` の結果。celeris のエラーは例外にせず `{ok:false, error}` にする
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

/**
 * 変更の取り込み（ADR-0043 D5、celeris Phase 54 / G18。**管理系。人だけ**）:
 * `POST /tasks/{id}/changes/{repo}/integrate`（`merge` / `pr` / `discard`）と
 * `POST /tasks/{id}/changes/{repo}/pr/merge` の結果。celeris のエラーは例外にせず `{ok:false, error}` にする
 * （409 `default_branch_busy`＝「main が編集中」/ 409 `pr_unavailable` / 422 `validation`（`confirm`）/
 * 401 `unauthorized` を含む）。**衝突と git の失敗は 200** で返るので `ok: true` のままで、
 * `result.integration.state`（`conflict` / `failed`）と `result.child_task_id`（「衝突の解消: …」タスク）を
 * 画面が見る（GUI では判定しない）。
 */
export type IntegrateOutcome =
  | { ok: true; op: "integrate" | "pr_merge"; repo: string; result: IntegrateResult }
  | { ok: false; op: "integrate" | "pr_merge"; repo: string; error: ActionError };

/**
 * 文書（ADR-0044 D7、celeris Phase 57 / G20。**管理系。正本は git**）:
 * `POST /projects/{id}/docs/init`、`PUT`/`DELETE /projects/{id}/docs/page`、
 * `POST /tasks/{id}/artifacts/promote` の結果。celeris のエラーは例外にせず `{ok:false, error}` にする
 * （409 `etag_mismatch`＝「読んでから誰かが直した」/ 409 `default_branch_busy`＝「main が編集中」/
 * 409 `page_exists` / 409 `docs_unavailable` / 403 `path_forbidden` / 422 `validation` /
 * 401 `unauthorized` を含む）。衝突の判定は celeris 側にあるので GUI では作り直さない。
 */
export type DocsOpOutcome =
  | { ok: true; op: "docs_init"; result: DocsInitResult }
  | { ok: true; op: "docs_put" | "docs_delete" | "docs_promote"; result: DocPageResult }
  | {
      ok: false;
      op: "docs_init" | "docs_put" | "docs_delete" | "docs_promote";
      error: ActionError;
    };

/**
 * 知識ベース（ADR-0047 D5、celeris Phase 61 / G21。**管理系。正本は `[knowledge] root` の Markdown**）:
 * `PUT /knowledge/page`、`POST /knowledge/inbox/{id}/accept`、`POST /knowledge/inbox/{id}/reject` の結果。
 * celeris のエラーは例外にせず `{ok:false, error}` にする（409 `etag_mismatch`＝「読んでから誰かが直した」/
 * 409 `page_exists`＝「宛先に既にある」/ 409 `knowledge_unavailable`＝「`[knowledge] root` が無い」/
 * 404 `candidate_not_found` / 403 `path_forbidden` / 422 `validation` / 401 `unauthorized` を含む）。
 * 衝突の判定は celeris 側にあるので GUI では作り直さない。
 */
export type KnowledgeOpOutcome =
  | { ok: true; op: "knowledge_put"; result: KnowledgePageResult }
  | { ok: true; op: "knowledge_accept"; id: string; result: KnowledgePageResult }
  | { ok: true; op: "knowledge_reject"; id: string; result: KnowledgeRejectResult }
  | { ok: false; op: "knowledge_put"; error: ActionError }
  | { ok: false; op: "knowledge_accept" | "knowledge_reject"; id: string; error: ActionError };

/**
 * Console の入力欄（ADR-0048 D3/D4、celeris Phase 60b / GUI Phase G22）: `POST /console/instruct` の結果
 * （202 `ConsoleInstructAccepted`）。返事は同期では返らない（`GET /console` / `GET /console/stream` で拾う）ので、
 * 画面はこの `message_id` / `task_id` / `node_id` を「送った」表示にだけ使う。celeris のエラーは例外にせず
 * `{ok:false, error}` にする（404 `org_node_not_found` / 422 `validation`（空白のみ） / 400 `bad_request`
 * （`scope` の形） / 401 `unauthorized` を含む）。
 */
export type ConsoleInstructOutcome =
  | { ok: true; op: "instruct"; accepted: ConsoleInstructAccepted }
  | { ok: false; op: "instruct"; error: ActionError };

/** ADR-0054 D1/D3（Phase 67/68）: `POST /console/new-conversation`（「新しい会話」ボタン）の結果。 */
export type ConsoleNewConversationOutcome =
  | { ok: true; op: "new_conversation" }
  | { ok: false; op: "new_conversation"; error: ActionError };

/**
 * skills（ADR-0056 D3 続き、docs/celeris-api-v1.md §3.114〜3.115。**管理系**。Phase 82 / G35）:
 * `PUT /skills/{name}` / `DELETE /skills/{name}` の結果。celeris のエラーは例外にせず `{ok:false, error}`
 * にする（404 `skill_not_found` / 409 `skill_mounted`＝「mount されている間は消せない」/ 422 `validation`
 * ＝「frontmatter の `name`/`description` が無い・URL と食い違う」/ 401 `unauthorized` を含む）。
 */
export type SkillOpOutcome =
  | { ok: true; op: "skill_put"; name: string; result: SkillPutResult }
  | { ok: true; op: "skill_delete"; name: string }
  | { ok: false; op: "skill_put" | "skill_delete"; name: string; error: ActionError };

/**
 * skill の mount / unmount（ADR-0056 D3 続き、docs/celeris-api-v1.md §3.116〜3.117。**管理系**。
 * Phase 82 / G35）: `POST /org/{id}/skills` / `DELETE /org/{id}/skills/{skill}` の結果。応答は
 * どちらも更新後の `OrgNode`（`OrgOpOutcome` の `patch` と同じ形だが、celeris-mcp の
 * `org_mount_skill`/`org_unmount_skill` と同じ関数を通ることを画面のコードからも辿れるよう、
 * `op` を分けて別の型にした）。celeris のエラーは例外にせず `{ok:false, error}` にする（404
 * `org_node_not_found` / 422 `validation`＝「skill 名の綴り」/ 401 `unauthorized` を含む）。
 */
export type OrgSkillMountOutcome =
  | { ok: true; op: "skill_mount" | "skill_unmount"; id: string; skill: string; node: OrgNode }
  | { ok: false; op: "skill_mount" | "skill_unmount"; id: string; skill: string; error: ActionError };
