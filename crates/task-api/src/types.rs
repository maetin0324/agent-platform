//! task-api の要求・応答の型（`docs/gui/api.md` §6.2）。task-core / task-ops の型はそのまま使う。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{
    ArtifactRef, EventRow, Milestone, MilestoneId, MilestoneStatus, OrgKind, OrgNode, Project, ProjectStatus,
    Status, TaskId, Tier,
};
use task_ops::daemon::{CooldownView, DaemonSnapshot};
use task_ops::view::RunSummary;

/// `GET /health`（無認証）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Health {
    /// 常に `"1"`。
    pub api_version: String,
    /// `schema_migrations` の最大版数。
    pub schema_version: u32,
    pub taskd_version: String,
    pub instance_id: String,
    pub started_at: String,
    pub now: String,
    pub db: DbInfo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DbInfo {
    /// `PRAGMA journal_mode` の実測値（`"wal"` でなければ設定不備）。
    pub journal_mode: String,
    pub busy_timeout_ms: u64,
}

/// RFC 9457 の problem details（`application/problem+json`）。`extra` は `code` ごとの付加フィールド。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Problem {
    /// `urn:taskd:problem:<code>`。
    pub r#type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    pub code: String,
    /// `urn:taskd:request:<X-Request-Id>`。
    pub instance: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// 422 `validation` の `errors[]`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ValidationError {
    /// 文言から対象が分かるときだけ（`acceptance` / `depends_on` / `goal` / `answer`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub message: String,
}

/// `POST /tasks/{id}/approve`、`POST /tasks/{id}/reject` の本文。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionBody {
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub expected_status: Option<Status>,
}

/// `POST /tasks/{id}/answer` の本文。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerBody {
    pub answer: String,
    #[serde(default)]
    pub expected_status: Option<Status>,
}

/// `POST /tasks/{id}/cancel` の本文。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelBody {
    #[serde(default)]
    pub expected_status: Option<Status>,
}

/// `GET /tasks/{id}/events`、`GET /events`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventsPage {
    pub items: Vec<EventRow>,
    pub has_more: bool,
}

/// `GET /tasks/{id}/runs`。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RunList {
    pub runs: Vec<RunSummary>,
}

/// `GET /tasks/{id}/artifacts`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactList {
    pub items: Vec<ArtifactView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactView {
    /// `ArtifactProduced` の出現順（0 始まり）。`GET /tasks/{id}/artifacts/{idx}` の添字。
    pub idx: usize,
    pub run_id: String,
    pub ts: String,
    pub artifact: ArtifactRef,
    pub exists: bool,
    /// パス検査に落ちた（ワークスペース外・symlink 越え・絶対パス等）。本体の取得は 403。
    pub forbidden: bool,
    pub size: Option<u64>,
    /// 64 MiB 以下のときだけ計算する。
    pub sha256_current: Option<String>,
    /// 記録値との一致。`exists = false` または未計算なら `null`。
    pub sha256_matches: Option<bool>,
}

/// `GET /providers`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Providers {
    pub items: Vec<ProviderView>,
}

/// `POST /api/v1/reload` の応答（ADR-0017 D1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReloadResult {
    pub reloaded: bool,
}

/// `POST /api/v1/providers/{id}/check` の応答（ADR-0017 D2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderCheckResponse {
    pub result: crate::admin::ProviderCheckResult,
    pub checked_at: String,
    /// ADR-0022 M1: 人が読むための一行の手がかり（ワーカーの返答、失敗の理由）。無ければ `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderView {
    pub id: String,
    pub adapter: String,
    pub tiers: Vec<Tier>,
    pub concurrency: usize,
    pub model: Option<String>,
    /// `env` のキー名だけ（値は出さない）。
    pub env_keys: Vec<String>,
    /// スナップショットが無ければ `null`。
    pub in_use: Option<u32>,
    /// スナップショットが無い、または cooldown 中でなければ `null`。
    pub cooldown: Option<CooldownView>,
    /// ADR-0022 D2: 直近の `POST /providers/{id}/check` の結果（`{at, result}`）。まだ確認していない、
    /// または taskd を再起動した後は `null`（メモリだけに持つ観測値）。
    pub last_check: Option<task_ops::daemon::ProviderCheckView>,
    pub stats: ProviderStats,
    /// ADR-0024 D2: `[accounts]` のプールから選ぶか（既定 `false`）。
    #[serde(default)]
    pub account_pool: bool,
}

/// プロバイダ別の run 集計（task-api のメモリ内の観測値。再起動で再計算）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderStats {
    /// `WorkerStarted` の数（実行中を含む）。
    pub runs: u64,
    pub done: u64,
    pub question: u64,
    pub error: u64,
    pub requeue: u64,
    pub lease_expired: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `WorkerFinished.ts` の UTC 日付で直近 30 日（昇順。run の無い日は現れない）。
    pub by_day: Vec<DailyUsage>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DailyUsage {
    /// `YYYY-MM-DD`（UTC）。
    pub day: String,
    /// その日に終わった run の数。
    pub runs: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// `GET /daemon`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonView {
    pub now: String,
    /// 最初の tick より前は `null`。
    pub snapshot: Option<DaemonSnapshot>,
}

/// `GET /config`: `taskd.toml` の要約。env の値・トークンは含めない。taskd が起動時に作る。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigView {
    pub config_path: String,
    /// DB の絶対パス。
    pub db: String,
    pub workspace_root: String,
    pub tick_ms: u64,
    pub max_concurrency: usize,
    pub lease_grace_secs: u64,
    pub idle_timeout_secs: u64,
    pub kill_grace_secs: u64,
    pub review_timeout_secs: u64,
    pub error_cooldown_secs: u64,
    pub retry_backoff_base_secs: u64,
    pub retry_backoff_max_secs: u64,
    pub max_requeues: u32,
    pub plan_auto_accept: bool,
    pub reviewer: ReviewerConfigView,
    pub providers: Vec<ProviderConfigView>,
    /// ADR-0018: `[[clusters]]` の要約（`env` はキー名だけ、`setup` は有無だけ）。
    #[serde(default)]
    pub clusters: Vec<ClusterConfigView>,
    /// ADR-0016 D1: `[[roles]]` の要約（指示文の本文は出さない）。
    #[serde(default)]
    pub roles: Vec<RoleConfigView>,
    /// ADR-0027 D1: `[[genres]]` の要約。
    #[serde(default)]
    pub genres: Vec<GenreConfigView>,
    /// ADR-0016 D2: `[delegation]` の上限。
    #[serde(default)]
    pub delegation: task_core::DelegationLimits,
    pub api: ApiConfigView,
}

/// `[[roles]]` 1 行の要約（ADR-0016 D1）。`instructions` は**本文を出さない**（プロンプトの中身は設定ファイルにだけ置く）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RoleConfigView {
    pub id: String,
    pub tier: Option<Tier>,
    pub adapter: Option<String>,
    pub max_turns: Option<u32>,
    pub max_wall_secs: Option<u64>,
    /// 指示文が 1 文字以上あるか（中身は出さない）。
    pub has_instructions: bool,
}

/// `[[genres]]` 1 行の要約（ADR-0027 D1, ADR-0028 D1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GenreConfigView {
    pub id: String,
    pub description: String,
    /// ADR-0028 D1: この分野で「できること」の自由記述。空なら省略される。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// ADR-0028 D1: この分野に渡すもの（目安）。空なら省略される。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_artifacts: Vec<String>,
    /// ADR-0028 D1: この分野から返るもの（目安）。空なら省略される。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_artifacts: Vec<String>,
    pub default_role: Option<String>,
    /// この分野に属する役割 id の一覧。
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewerConfigView {
    pub adapter: Option<String>,
    pub tier: Tier,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderConfigView {
    pub id: String,
    pub adapter: String,
    pub tiers: Vec<Tier>,
    pub concurrency: usize,
    /// 実効モデル（空なら `null`）。
    pub model: Option<String>,
    /// `[[providers]].env` のキー名だけ。
    pub env_keys: Vec<String>,
    /// ADR-0024 D2: `[accounts]` のプールから選ぶか（既定 `false`）。
    #[serde(default)]
    pub account_pool: bool,
}

/// `[[clusters]]` 1 行の要約（ADR-0018 D7: `env` の値は出さない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClusterConfigView {
    pub id: String,
    /// `~/.ssh/config` の `Host` 名。
    pub host: String,
    pub concurrency: usize,
    /// `"rsync"` | `"none"`。
    pub sync: String,
    pub delete_on_push: bool,
    /// `setup` が 1 行以上あるか（中身は出さない）。
    pub has_setup: bool,
    /// `env` のキー名だけ（昇順）。
    pub env_keys: Vec<String>,
    pub rsync_excludes: Vec<String>,
    /// ADR-0032 D1: `"manual"` | `"publickey"` | `"totp"`。
    #[serde(default = "default_cluster_auth")]
    pub auth: String,
}

fn default_cluster_auth() -> String {
    "manual".to_string()
}

/// `GET /clusters`（ADR-0018 受け入れ条件8）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Clusters {
    pub items: Vec<ClusterView>,
}

/// 設定（`[[clusters]]`）とスナップショット（`ClusterLive`）を結合したもの。`env` の値は出さない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClusterView {
    pub id: String,
    /// `~/.ssh/config` の `Host` 名。
    pub host: String,
    pub concurrency: usize,
    /// `"rsync"` | `"none"`。
    pub sync: String,
    pub delete_on_push: bool,
    /// `setup` が 1 行以上あるか（中身は出さない）。
    pub has_setup: bool,
    /// `env` のキー名だけ（昇順）。
    pub env_keys: Vec<String>,
    pub rsync_excludes: Vec<String>,
    /// スナップショットが無ければ `null`。
    pub in_use: Option<u32>,
    /// この tick で `ssh -O check` が成功したか。スナップショットが無ければ `null`。
    pub connected: Option<bool>,
    /// cooldown 中ならその終わり（RFC 3339）。
    pub cooldown_until: Option<String>,
    /// `cooldown_until − now`（秒）。過ぎていれば両方 `null`。
    pub cooldown_remaining_secs: Option<u64>,
    /// ADR-0032 D1: `"manual"` | `"publickey"` | `"totp"`（設定の `[[clusters]].auth` から）。
    #[serde(default = "default_cluster_auth")]
    pub auth: String,
    /// ADR-0032 D5: GUI 発の接続（`POST /clusters/{id}/connect`）が進行中か。スナップショットが無ければ `false`。
    #[serde(default)]
    pub connect_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApiConfigView {
    pub bind: String,
    pub auth_required: bool,
    pub allowed_hosts: Vec<String>,
}

/// SSE `event: hello`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StreamHello {
    /// 送信開始位置（この id より後の `task.event` を送る）。
    pub cursor: u64,
    pub now: String,
    pub daemon: Option<DaemonSnapshot>,
}

/// SSE `event: heartbeat`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StreamHeartbeat {
    pub now: String,
}

/// SSE `event: reset`。クライアントは全体を再取得する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StreamReset {
    /// `"cursor_too_old"` | `"cursor_ahead"`。
    pub reason: String,
    /// 以後の送信開始位置（最新の id）。
    pub cursor: u64,
}

// ---- Phase 13（ADR-0024）: Claude アカウントのプール ----

/// `GET /accounts`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountList {
    /// `[accounts] claude_dir` の絶対パス。`[accounts]` が無ければ `null`。ADR-0025 D6: `roots["claude-code"]`
    /// の別名として残す（後方互換）。
    pub root: Option<String>,
    /// ADR-0025 D6: `"claude-code"` / `"codex"` → 設定されていればその絶対パス、無ければ `null`。
    #[serde(default)]
    pub roots: std::collections::HashMap<String, Option<String>>,
    pub max_runs_per_account: usize,
    /// `adapter` → `id` の順。
    pub items: Vec<AccountView>,
}

/// 1 アカウント（`GET /accounts` の要素、`POST /accounts` の応答）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountView {
    /// ADR-0025 D1: `"claude-code"` | `"codex"`。
    #[serde(default = "default_account_adapter")]
    pub adapter: String,
    pub id: String,
    /// アカウントディレクトリの絶対パス（ログイン手順に要る。秘密の中身は含まない）。
    pub dir: String,
    /// ログイン済みを示すファイル（claude-code は `.credentials.json`、codex は `auth.json`）の有無
    /// （中身は読まない）。
    pub logged_in: bool,
    /// 実行中の run の数。最初の tick 前は `0`。
    pub in_use: u32,
    pub usage: Option<AccountUsageView>,
    /// ADR-0024 D3 のスコア。除外なら `null`。
    pub score: Option<f64>,
    /// `"not_logged_in" | "at_capacity" | "cooldown" | "five_hour_exhausted" | "seven_day_exhausted" | "rejected"`。
    pub excluded_reason: Option<String>,
    pub cooldown: Option<AccountCooldownView>,
    pub last_check: Option<task_ops::daemon::ProviderCheckView>,
    /// ADR-0024 D7: 進行中のログイン中継があるか。
    pub login_pending: bool,
    pub stats: AccountStats,
}

/// `RateLimitObservation` を RFC 3339 に直したもの。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountUsageView {
    pub five_hour: Option<RateWindowView>,
    pub seven_day: Option<RateWindowView>,
    pub status: Option<String>,
    pub observed_at: String,
    /// `"run" | "check"`。
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RateWindowView {
    pub utilization: f64,
    pub resets_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountCooldownView {
    pub until: String,
    /// `"auth_failed" | "throttled" | "exhausted"`。
    pub reason: String,
}

/// `WorkerStarted.account` / `WorkerFinished` から集計（task-api のメモリ内の観測値。再起動で再計算）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountStats {
    pub runs: u64,
    pub done: u64,
    pub error: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

fn default_account_adapter() -> String {
    "claude-code".to_string()
}

/// `POST /accounts` の要求本文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountCreateBody {
    pub id: String,
    /// ADR-0025 D6: `"claude-code"`（既定）| `"codex"`。
    #[serde(default = "default_account_adapter")]
    pub adapter: String,
}

/// `POST /accounts/{id}/check` の応答（ADR-0024 D6）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountCheckResponse {
    pub result: crate::admin::ProviderCheckResult,
    pub checked_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AccountUsageView>,
}

/// `POST /accounts/{id}/login` の応答（ADR-0024 D7、ADR-0025 D5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountLoginStart {
    /// `"paste_code"`（claude-code: URL を開いて認可し、表示されたコードを `login/code` に貼る）|
    /// `"device_code"`（codex: URL を開いて `user_code` を入力する。GUI には貼り戻さない）。
    #[serde(default = "default_login_kind")]
    pub kind: String,
    pub url: String,
    /// codex のみ。`login/code` には使わない（GUI が画面に出すだけ）。ログには出さない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    /// claude-code は 10 分後、codex は 15 分後（RFC 3339）。
    pub expires_at: String,
}

fn default_login_kind() -> String {
    "paste_code".to_string()
}

/// `POST /accounts/{id}/login/code` の要求本文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountLoginCodeBody {
    pub code: String,
}

/// `POST /accounts/{id}/login/code` の応答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountLoginResult {
    /// `"ok" | "failed"`。
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ---- Phase 20（ADR-0030）: GUI から預かる秘密（API キー等） ----

/// `GET /secrets`。`[secrets]` が未設定なら 409 `secrets_unavailable`（`dir: None` の応答は返さない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SecretList {
    /// `[secrets] dir` の絶対パス。
    pub dir: Option<String>,
    /// ファイルがある id を id 昇順、続けて未設定（`env_from_secrets` が参照しているだけ）の id を id 昇順。
    pub items: Vec<SecretView>,
}

/// 1 秘密（値は決して含まない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SecretView {
    pub id: String,
    /// ファイルの mtime（RFC 3339）。**まだ値が入っていない**（設定が参照しているだけ）なら `null`。
    pub updated_at: Option<String>,
    /// 値の sha256 の先頭 8 桁（値そのものは復元できない）。値が無ければ `null`。
    pub fingerprint: Option<String>,
    /// 設定（`env_from_secrets`）から導いた、この秘密を使っている場所。
    pub used_by: Vec<SecretUse>,
}

/// `SecretView.used_by` の 1 要素。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SecretUse {
    /// `"adapter" | "provider"`。
    pub scope: String,
    /// `scope = "adapter"` ならアダプタ種別（`"claude-code"` 等）、`"provider"` ならプロバイダ id。
    pub name: String,
    /// 流し込む環境変数名。
    pub env: String,
}

/// `PUT /secrets/{id}` の要求本文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretPutBody {
    /// 空白だけは 422。
    pub value: String,
}

/// `PUT /secrets/{id}` の応答。値は含まない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SecretPutResult {
    pub id: String,
    pub updated_at: String,
    pub fingerprint: String,
}

// ---- ADR-0032 D5: クラスタへの接続を GUI から張る ----

/// `POST /clusters/{id}/connect` の応答。`kind = "connected"` はコード不要で張れた場合。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClusterConnectStart {
    /// `"connected"` | `"needs_code"`。
    pub kind: String,
    /// `kind = "needs_code"` のときだけ。ssh が出したプロンプト文字列（ユーザ名・ホスト名を含みうるので
    /// ログには出さない。`GET /clusters` にも出さない。応答にだけ載る）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// `kind = "needs_code"` のときだけ（RFC 3339）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// `POST /clusters/{id}/connect/code` の要求本文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClusterConnectCodeBody {
    pub code: String,
}

/// `POST /clusters/{id}/connect/code` の応答。コード・URL は含まない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClusterConnectResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ---- ADR-0033 D1/D2（Phase 23）: 組織・案件・途中目標 ----

/// `GET /org` の応答。木は GUI が `parent_id` で組む（順序は `position`、同値なら `id` の昇順）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OrgList {
    pub items: Vec<OrgNode>,
}

/// `POST /org` の要求本文（管理系）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgCreateBody {
    pub id: String,
    pub name: String,
    pub kind: OrgKind,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub brief: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
}

/// `PATCH /org/{id}` の要求本文（管理系）。書いた項目だけを変える。
/// `genre` は `null` を書けば「分野なし」にできる（書かなければ今の値のまま）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgPatchBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<OrgKind>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub genre: Option<Option<String>>,
    #[serde(default)]
    pub brief: Option<String>,
    #[serde(default)]
    pub position: Option<i64>,
}

/// 「書かなかった」と「`null` を書いた」を区別するための小道具（`Option<Option<T>>`）。
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// `GET /projects` の応答（`created_at` の降順）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectList {
    pub items: Vec<Project>,
}

/// `POST /projects` の要求本文。作られた案件は `status = "proposed"`（秘書の返事待ち）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectCreateBody {
    pub title: String,
    pub request: String,
}

/// `PATCH /projects/{id}` の要求本文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectPatchBody {
    pub status: ProjectStatus,
}

/// `GET /projects/{id}` の応答。案件 + 途中目標 + その案件のタスクの要約（GUI の「仕事の木」用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectDetail {
    pub project: Project,
    pub milestones: Vec<Milestone>,
    /// 仕事の木を描くのに必要な最小限だけ（詳細は `GET /tasks/{id}`）。
    pub tasks: Vec<ProjectTaskView>,
}

/// 仕事の木の 1 ノード（ADR-0033 D2: DAG は既存の `parent_id` / `depends_on` がそのまま）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectTaskView {
    pub id: TaskId,
    pub title: String,
    pub status: Status,
    pub parent_id: Option<TaskId>,
    pub depends_on: Vec<TaskId>,
    pub assignee: Option<String>,
    pub milestone_id: Option<MilestoneId>,
    /// 対話用タスク（人への返事のための run）か。GUI は仕事の木から隠せる（GUI-R3）。
    pub conversation: bool,
}

/// `POST /projects/{id}/milestones` の要求本文。`seq` はストアが採番する。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MilestoneCreateBody {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 省略時は `proposed`（秘書が提案し、人が承認する。SPEC §7）。
    #[serde(default)]
    pub status: Option<MilestoneStatus>,
}

/// `PATCH /milestones/{id}` の要求本文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MilestonePatchBody {
    pub status: MilestoneStatus,
}
