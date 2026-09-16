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
    pub providers: Vec<ProviderLive>,
    /// ADR-0018: `[[clusters]]` の稼働状況（`id` 昇順）。第 2 段階で追加したので、古いスナップショットには無い。
    #[serde(default)]
    pub clusters: Vec<ClusterLive>,
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
}
