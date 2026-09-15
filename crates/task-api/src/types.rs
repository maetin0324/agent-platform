//! task-api の要求・応答の型（`docs/gui/api.md` §6.2）。task-core / task-ops の型はそのまま使う。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{ArtifactRef, EventRow, Status, Tier};
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
    pub stats: ProviderStats,
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
