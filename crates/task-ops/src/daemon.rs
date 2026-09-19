//! デーモンのメモリ上のスナップショット（ADR-0013 D4, `docs/gui/api.md` §3.20 / §6.2）。
//!
//! ディスパッチャ（task-dispatch）が tick の最後に作って `tokio::sync::watch` に送り、API（task-api）が読む。両者が依存する
//! この crate に型を置く（task-api は task-dispatch に依存しない）。真実ではなく観測値で、DB には書かず `replay` の対象外。
//! 時刻は RFC 3339 の文字列（`Instant` は作る側で壁時計に直す）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{TaskId, Tier};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonSnapshot {
    /// 起動ごとの ULID（`GET /health` の `instance_id` と同じ）。
    pub instance_id: String,
    pub pid: u32,
    pub hostname: String,
    pub started_at: String,
    pub last_tick_at: String,
    pub ticks: u64,
    pub tick_ms: u64,
    pub in_flight: Vec<InFlight>,
    pub cooldowns: Vec<CooldownView>,
    /// 人間の承認待ちでレビューを延期している reviewing タスク。
    pub awaiting_human: Vec<TaskId>,
    /// ADR-0023 D3: 委譲した子が終わるのを待っている親（`reviewing` のまま。id 昇順）。
    /// 「自分の判定待ち」と区別するための観測値。古いスナップショットには無いので既定は空。
    #[serde(default)]
    pub awaiting_children: Vec<TaskId>,
    /// 設定に合うプロバイダが無い ready タスク（この tick の判定）。
    pub unroutable: Vec<TaskId>,
    /// ADR-0033 D3（Phase 25）: 秘書レベルの未読の報告と通知の判定。**API が応答を組むときに埋める**
    /// 唯一のフィールド（`last_notified_at` は `POST /reports/notified` が進める API プロセスの観測値で、
    /// ディスパッチャは知らない）。ディスパッチャが送るスナップショットでは常に `None`。
    #[serde(default)]
    pub reports: Option<task_core::ReportsLive>,
    /// ADR-0033 D5（Phase 26）: 未決定の認可（`approvals.decision IS NULL`）の件数。`reports` と同じ理由で
    /// **API が応答を組むときに埋める**（ディスパッチャが送るスナップショットでは常に 0）。
    #[serde(default)]
    pub approvals_pending: u32,
    pub providers: Vec<ProviderLive>,
    /// ADR-0018: `[[clusters]]` の稼働状況（`id` 昇順）。第 2 段階で追加したので、古いスナップショットには無い。
    #[serde(default)]
    pub clusters: Vec<ClusterLive>,
    /// ADR-0024 D1/D5: `[accounts] claude_dir` の絶対パス（`[accounts]` が無ければ `None`）。
    /// ADR-0025 D6: claude-code の根の別名として残す（`accounts_roots["claude-code"]` と同じ値）。
    #[serde(default)]
    pub accounts_root: Option<String>,
    /// ADR-0024 D1: `[accounts] max_runs_per_account`（`[accounts]` が無ければ `None`）。
    #[serde(default)]
    pub max_runs_per_account: Option<usize>,
    /// ADR-0025 D1/D6: アダプタごとの根ディレクトリ（`"claude-code"` / `"codex"` → 絶対パス。設定されていない
    /// アダプタはキーごと無い）。古いスナップショットには無いので既定は空。
    #[serde(default)]
    pub accounts_roots: std::collections::HashMap<String, String>,
    /// ADR-0024/0025: プールのアカウント（`adapter` → `id` の順、id 昇順）。`[accounts]` が無ければ空。
    #[serde(default)]
    pub accounts: Vec<AccountLive>,
    /// ADR-0043 D3（Phase 56）: `[containers]` の設定と、起動時に調べたコンテナ runtime。
    /// 古いスナップショットには無いので既定は `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub containers: Option<ContainersLive>,
}

/// ADR-0043 D3（Phase 56）: コンテナ実行の設定と起動時の検出（**観測値**。DB には書かない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContainersLive {
    /// `[containers] runtime`（`"auto"` | `"podman"` | `"docker"`）。
    pub preference: String,
    /// 実際に使う runtime（`"podman"` | `"docker"`）。どれも使えなければ `None`
    /// （`run = container` のタスクは dispatch されず `blocked` になる）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// 試した runtime ごとの `<runtime> info` の結果（試した順）。
    #[serde(default)]
    pub probes: Vec<ContainerProbeView>,
    /// `[containers] image_default`（`[container] image` も `dockerfile` も無いときのイメージ）。
    pub image_default: String,
    /// `[containers] build_dir`（Dockerfile からビルドしたイメージの作業場所。絶対パス）。
    pub build_dir: String,
}

/// `<runtime> info` の結果 1 件（`detail` は成功なら `"ok"`、失敗なら理由の 1 行）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContainerProbeView {
    pub runtime: String,
    pub detail: String,
}

/// ADR-0024/0025: プールの 1 アカウントの稼働状況（観測値。DB には書かない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountLive {
    /// ADR-0025 D1: `"claude-code"` | `"codex"`。古いスナップショットには無いので既定は `"claude-code"`。
    #[serde(default = "default_account_adapter")]
    pub adapter: String,
    pub id: String,
    /// ログイン済みを示すファイル（claude-code は `.credentials.json`、codex は `auth.json`）の有無。
    pub logged_in: bool,
    /// 実行中の run（ワーカー run + このアカウントを使う Reviewer run）の数。
    pub in_use: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AccountUsageLive>,
    /// ADR-0024 D3 のスコア。除外されていれば `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// `"not_logged_in" | "at_capacity" | "cooldown" | "five_hour_exhausted" | "seven_day_exhausted" | "rejected"`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<AccountCooldownLive>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<ProviderCheckView>,
    /// 進行中のログイン中継（ADR-0024 D7）があるか。
    #[serde(default)]
    pub login_pending: bool,
}

/// `RateLimitObservation` の観測値部分（Unix 秒のまま。壁時計の文字列化は task-api が行う）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountUsageLive {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<task_core::RateWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<task_core::RateWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub observed_at: i64,
    /// `"run" | "check"`。
    pub source: String,
}

fn default_account_adapter() -> String {
    "claude-code".to_string()
}

/// アカウントの cooldown（Unix 秒）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AccountCooldownLive {
    pub until: i64,
    /// `"auth_failed" | "throttled" | "exhausted"`。
    pub reason: String,
}

/// 実行中の run（ワーカー run、またはプロバイダを使う Reviewer run）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InFlight {
    pub task_id: TaskId,
    pub run_id: String,
    pub provider: String,
    pub kind: InFlightKind,
    pub since: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InFlightKind {
    Worker,
    Reviewer,
}

/// `task_dispatch::policy::Cooldown`（`Instant`）を壁時計に直したもの。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CooldownView {
    pub provider: String,
    pub until: String,
    /// `"throttled" | "auth_failed" | "exhausted"`。
    pub reason: String,
}

/// プロバイダ（`[[providers]]` の行 = アカウント）の稼働状況。`env` の値は含めない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderLive {
    pub id: String,
    pub adapter: String,
    pub tiers: Vec<Tier>,
    pub concurrency: usize,
    /// 実効モデル（空なら `None`）。
    pub model: Option<String>,
    /// `env` のキー名だけ（値は出さない）。古いスナップショットには無いので既定は空（ADR-0017 M4）。
    #[serde(default)]
    pub env_keys: Vec<String>,
    /// 実行中の run と Reviewer run の合計。
    pub in_use: u32,
    /// ADR-0022 D2: 直近の疎通確認（`POST /providers/{id}/check`）の結果。**メモリだけに持つ観測値**で、
    /// taskd を再起動すると消える（イベントにも DB にも残さない）。一度も確認していなければ `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<ProviderCheckView>,
    /// ADR-0024 D2: `[accounts]` のプールから選ぶか。古いスナップショットには無いので既定 `false`。
    #[serde(default)]
    pub account_pool: bool,
}

/// ADR-0022 D2: 1 回の疎通確認の記録。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderCheckView {
    /// 確認した時刻（RFC 3339）。
    pub at: String,
    /// `ok` / `auth_failed` / `throttled` / `spawn_failed`（`task_api::ProviderCheckResult` の serde 名）。
    pub result: String,
    /// 人が読むための一行の手がかり（ワーカーの返答や失敗の理由。ADR-0022 M1）。無ければ `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// クラスタ（`[[clusters]]` の行）の稼働状況（ADR-0018 D2 / D5）。`env` の値・`setup` の中身は含めない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClusterLive {
    pub id: String,
    /// `~/.ssh/config` の `Host` 名。
    pub host: String,
    pub concurrency: usize,
    /// このクラスタで走っている run（ワーカー run + 判定）の数。
    pub in_use: u32,
    /// この tick で `ssh -O check` が成功した（人が張った多重接続がある）。
    pub connected: bool,
    /// 多重接続が無くて cooldown 中なら、その終わり（RFC 3339）。
    pub cooldown_until: Option<String>,
    /// ADR-0032 D1: `"manual"` / `"publickey"` / `"totp"`。古いスナップショットには無いので既定は `"manual"`
    /// （`taskd::config::ClusterConfig.auth` と同じ既定）。
    #[serde(default = "default_cluster_live_auth")]
    pub auth: String,
    /// ADR-0032 D4: GUI 発の接続（`POST /clusters/{id}/connect`）が進行中か。古いスナップショットには無いので既定は `false`。
    #[serde(default)]
    pub connect_pending: bool,
}

fn default_cluster_live_auth() -> String {
    "manual".to_string()
}
