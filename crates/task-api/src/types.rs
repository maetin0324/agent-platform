//! task-api の要求・応答の型（`docs/gui/api.md` §6.2）。task-core / task-ops の型はそのまま使う。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{
    ArtifactRef, EventRow, Milestone, MilestoneId, MilestoneStatus, OrgKind, OrgNode, Project,
    ProjectStatus, Status, TaskId, Tier,
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
    pub celeris_version: String,
    pub instance_id: String,
    pub started_at: String,
    pub now: String,
    pub db: DbInfo,
    /// ADR-0040 D4（Phase 47）: このプロセスのリリース（`--release <sha12>` / `CELERIS_RELEASE` / `"dev"`）。
    pub release: String,
    /// ADR-0040 D3: `normal` または `verify`（`--mode`）。
    pub mode: String,
    /// ADR-0040 D4: `active` / `standby` / `draining` / `verify`。
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DbInfo {
    /// `PRAGMA journal_mode` の実測値（`"wal"` でなければ設定不備）。
    pub journal_mode: String,
    pub busy_timeout_ms: u64,
    /// ADR-0064 D1: `/proc/self/mountinfo` から引けたファイルシステム種別（`"ext4"` 等）。
    /// `GET /health` は無認証（`docs/gui/api.md` §1.1 / auth_and_guards.rs のテスト）なので、DB の
    /// **絶対パス自体はここに出さない**（それは認証済みの `GET /api/v1/config` の `config.db` が
    /// 既に返している）。判定できなければ `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<String>,
    /// 同上のマウントソース（`/dev/loop0` のような loop デバイスならネットワーク越しの可能性がある。
    /// ADR-0064 D1）。判定できなければ `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

/// RFC 9457 の problem details（`application/problem+json`）。`extra` は `code` ごとの付加フィールド。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Problem {
    /// `urn:celeris:problem:<code>`。
    pub r#type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    pub code: String,
    /// `urn:celeris:request:<X-Request-Id>`。
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

// ========== ADR-0044 D2/D5（Phase 53）: コメント・再開・タイムライン。ここから ==========
// このブロックは ADR-0044 B1 が足した型だけを持つ（ADR-0043 A1 の型は別のブロックに足す）。

/// `POST /tasks/{id}/comments` の本文（人のコメント。ADR-0044 D2）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentBody {
    pub body: String,
}

/// `GET /tasks/{id}/comments` の応答（古い順）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommentList {
    pub items: Vec<task_core::TaskComment>,
}

/// `POST /tasks/{id}/reopen` の本文。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReopenBody {
    #[serde(default)]
    pub expected_status: Option<Status>,
}

/// `GET /tasks/{id}/timeline` の応答（ADR-0044 D5）。**時刻の昇順で 1 本**。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Timeline {
    pub task_id: TaskId,
    pub items: Vec<TimelineItem>,
}

/// タイムラインの 1 件（ADR-0044 D5）。`at` は RFC 3339。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimelineItem {
    /// 状態遷移・run・質問・回答・編集・割り込み（`events` の 1 行）。
    Event {
        at: String,
        seq: u64,
        event: task_core::Event,
    },
    /// コメント（人・組織の「人」・celeris）。
    Comment {
        at: String,
        comment: task_core::TaskComment,
    },
    /// 認可（ADR-0033 D5）。`decided_at` があれば決まった時刻、無ければ聞いた時刻。
    Approval {
        at: String,
        approval: task_core::Approval,
    },
    /// 報告（ADR-0034）。
    Report {
        at: String,
        report: task_core::Report,
    },
    /// 委譲（`Event::Delegated`。作られた子のタスク）。
    Delegation {
        at: String,
        run_id: String,
        tasks: Vec<task_ops::view::TaskRef>,
    },
    /// リリース（ADR-0044 D5）: このタスクのブランチのコミットが入ったリリース。
    Release {
        at: String,
        sha12: String,
        /// そのリリースに入った、このタスクのコミット（完全な sha）。
        commits: Vec<String>,
    },
    /// ADR-0043 D5 / A2（取り込み: merge / PR / discard）。`task_integrations` の 1 行を
    /// `action`（方法）と `detail`（`<リポジトリ>: <行方>` + PR の番号と URL + 理由）に写したもの。
    Integration {
        at: String,
        action: String,
        detail: String,
    },
    /// ADR-0044 D7（Phase 57）: 逆リンク。front matter の `tasks:` にこのタスクを持つ文書のページ。
    /// `at` はそのページの最後のコミットの時刻（読めなければ空）。
    Doc {
        at: String,
        project_id: task_core::ProjectId,
        /// 案件のリポジトリからの相対パス（`docs/research/xxx.md`）。
        path: String,
        title: String,
    },
    /// ADR-0047 D4/D5（Phase 62）: このタスクの終端から起きた知識整理 run。`at` は適用済みなら
    /// `applied_at`、まだなら `created_at`（run を起こした時刻）。
    Knowledge {
        at: String,
        run_task_id: TaskId,
        /// `scheduled`（起こしたが未適用）| `applied` | `failed`。
        state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ingested: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inbox: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        discarded: Option<u32>,
        /// ADR-0052 D2（Phase 64）: 抽出した経路。`"langmem"`（Qwen）か `"fallback:<adapter>"`
        /// （Qwen に届かず tier cheap の汎用ハーネスで抽出した）。分からなければ `null`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<String>,
    },
}

// ========== ADR-0044 D2/D5（Phase 53）: ここまで ==========

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

/// `POST /tasks/{id}/retry`（Phase 31）の本文。`accept: true` なら新しいタスクは `draft` を経ず `ready` で始まる。
/// ADR-0062 Phase 108 追記: `workspace`（省略可）を与えると、複製先の作業場所をそれに差し替える
/// （検証は `PATCH /tasks/{id}` の `workspace` と同じ）。省略時は従来どおり元のタスクの `workspace` を複製する。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryBody {
    #[serde(default)]
    pub accept: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<task_core::WorkspaceSpec>,
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
    #[serde(default)]
    pub credential_refs: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub tier_models: task_core::model_routing::TierModels,
    #[serde(default)]
    pub account_id: Option<String>,
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
    /// または celeris を再起動した後は `null`（メモリだけに持つ観測値）。
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

/// `GET /config`: `config.toml` の要約。env の値・トークンは含めない。celeris が起動時に作る。
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
    #[serde(default)]
    pub credential_refs: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub tier_models: task_core::model_routing::TierModels,
    #[serde(default)]
    pub account_id: Option<String>,
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
    /// ADR-0059 D6: 設定ファイルの `[[clusters]] work_dir`（DB の上書きは含まない。`GET /clusters` の
    /// `ClusterView.work_dir`/`work_dir_source` が実効値を持つ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
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
    /// ADR-0053 D3（Phase 66）: `[[clusters.forwards]]`。空なら Qwen トンネル等の管理対象ではない。
    #[serde(default)]
    pub forwards: Vec<ClusterForwardView>,
}

fn default_cluster_auth() -> String {
    "manual".to_string()
}

/// ADR-0053 D3（Phase 66）: `[[clusters.forwards]]` 1 本の要約と生存（`GET /clusters` にそのまま出す）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClusterForwardView {
    pub listen: String,
    pub target: String,
    /// forward 越しに `GET <listen>/v1/models` が届くか（`listener && target_healthy`）。
    /// スナップショットが無ければ `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up: Option<bool>,
    /// ADR-0053 Phase 85: 手元のリスナー（`-O forward`/`ssh -N -L`）が有るか。スナップショットが
    /// 無ければ `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listener: Option<bool>,
    /// ADR-0053 Phase 85: listener 越しに target（`/v1/models`）が健全か。スナップショットが無ければ
    /// `null`（`listener == false` のときは意味を持たない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_healthy: Option<bool>,
    /// ADR-0053 Phase 85: 直近の失敗理由（無ければ `null`）。GUI が「転送あり・先方応答なし」等の
    /// 理由を出すのに使う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// `GET /clusters`（ADR-0018 受け入れ条件8）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Clusters {
    pub items: Vec<ClusterView>,
}

/// `PUT /clusters/{id}/settings`（ADR-0059 D6）の要求本文。絶対パスか `~`/`~/…` だけ許す
/// （それ以外・空文字は 422 `validation`）。`null`（省略）で DB の上書きを消す。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClusterSettingsPutBody {
    #[serde(default)]
    pub work_dir: Option<String>,
}

/// `PUT /clusters/{id}/settings` の応答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClusterSettingsView {
    pub cluster_id: String,
    /// 書いた後の DB 上書きの値（`null` なら上書きを消した = 設定ファイルの値に戻る）。
    pub work_dir: Option<String>,
    pub updated_at: String,
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
    /// ADR-0053 D3（Phase 66）: `[[clusters.forwards]]` の設定と生存を結合したもの。
    #[serde(default)]
    pub tunnel_forwards: Vec<ClusterForwardView>,
    /// ADR-0053 D3: 鍵認証を試しても ssh master が繋がらず、人の TOTP 入力が要る状態か。
    /// スナップショットが無ければ `false`。
    #[serde(default)]
    pub tunnel_login_needed: bool,
    /// ADR-0059 D6: 実効の作業ディレクトリ（DB の上書き `cluster_settings` があればそれ、無ければ
    /// 設定ファイルの `work_dir`）。どちらも無ければ `null`（`WorkspaceSpec::Remote.path` が相対・
    /// 省略のタスクはこのクラスタでは失敗する）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
    /// ADR-0059 D6: `work_dir` の出どころ。`"settings"`（DB の上書き）/ `"config"`（設定ファイル）。
    /// `work_dir` が `null` なら `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir_source: Option<String>,
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

// ---- ADR-0046（Phase 59）: 組織 = Agent Profile の継承木。ここから ----

/// `GET /org` の応答。木は GUI が `parent_id` で組む（順序は `position`、同値なら `id` の昇順）。
///
/// ADR-0046 D1（Phase 59）: 各ノードの `profile` は `items[]` にそのまま載る（空なら省略）。
/// **継いだ後の実効 profile** は `effective_profiles[]` に、`node_id` で引ける形で並べて返す
/// （`items` と同じ並び。GUI は「どこから継いだか」を `chain` で出す）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OrgList {
    pub items: Vec<OrgNode>,
    /// ADR-0046 D1: `items` と同じ並びの実効 profile（`EffectiveProfile.node_id` で対応づく）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effective_profiles: Vec<task_core::EffectiveProfile>,
    /// ADR-0054 D1/D3（Phase 67/68）: 部門長（`OrgKind::Department`）の継続セッション（`kind = lead`）が
    /// あるノードだけ、`node_id` で対応づけて渡す（無いノードは含めない。CoS の対話セッションは
    /// 組織画面ではなく Console のチャット欄自身が見せるので、ここには乗せない）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lead_sessions: Vec<NodeSessionSummary>,
}

/// `OrgList.lead_sessions[]` の 1 件（ADR-0054 D3。Phase 68）: 「継続中のセッション: turns / tokens /
/// 最終使用」を組織画面に出すための最小限の読み取り。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NodeSessionSummary {
    pub node_id: String,
    pub turns: i64,
    pub approx_tokens: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub last_used_at: time::OffsetDateTime,
}

// ---- ADR-0046（Phase 59）: ここまで ----

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
    /// ADR-0046 D1（Phase 59）: このノードの profile（省略時は空）。
    #[serde(default)]
    pub profile: Option<task_core::Profile>,
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
    /// ADR-0046 D1（Phase 59）: profile の**丸ごと差し替え**（部分更新はしない。書かなければ今のまま）。
    #[serde(default)]
    pub profile: Option<task_core::Profile>,
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
    /// ADR-0039 D1: この案件の作業場所（任意）。`{"kind":"local","path":"~/workspace/rust/pluvio-poc"}` か
    /// `{"kind":"remote","cluster":"pegasus","path":"/work/.../benchfs"}`。`~` は celeris の `$HOME` で
    /// 展開して保存する（`Local` のみ）。知らない `cluster` は 422。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<task_core::WorkspaceSpec>,
}

/// `PATCH /projects/{id}` の要求本文。`status` / `workspace` はどちらも任意（書いたものだけ変える）。
/// `"workspace": null` を明示すると作業場所を消す（案件を「作業場所なし」に戻す）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectPatchBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ProjectStatus>,
    /// ADR-0039 D1: 省略（`None`）なら変えない、`null`（`Some(None)`）なら消す、値なら差し替える。
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace: Option<Option<task_core::WorkspaceSpec>>,
}

/// `GET /projects/{id}` の応答。案件 + 途中目標 + その案件のタスクの要約（GUI の「仕事の木」用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectDetail {
    pub project: Project,
    /// ADR-0043 D1（Phase 52）: この案件のリポジトリ（primary が先頭）。
    /// `project.workspace` は primary の `location` の写し（GUI の後方互換）。
    #[serde(default)]
    pub repos: Vec<task_core::ProjectRepo>,
    /// ADR-0038 D1 / D4（Phase 41）: 途中目標そのもの（`Milestone` の各フィールドはそのまま）に、
    /// 秘書のレビューの返事と提案された次の途中目標を添えたもの。
    pub milestones: Vec<MilestoneView>,
    /// 仕事の木を描くのに必要な最小限だけ（詳細は `GET /tasks/{id}`）。
    pub tasks: Vec<ProjectTaskView>,
}

/// 途中目標 1 件のビュー（ADR-0038 D1 / D4。Phase 41）。`Milestone` のフィールドは**平らに**出るので、
/// 既存の GUI（`id` / `title` / `status` …）はそのまま読める。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MilestoneView {
    #[serde(flatten)]
    pub milestone: Milestone,
    /// 秘書のレビューの返事（まだ無ければ省略。run 中は `tasks[]` の
    /// `support = "milestone_review"` が動いている）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<MilestoneReviewView>,
    /// その返事が提案した次の途中目標（`proposed` の最新。無ければ省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Milestone>,
}

/// 秘書のレビューの返事（`messages` の 1 行。ADR-0038 D1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MilestoneReviewView {
    pub message_id: String,
    /// 返事の本文（Markdown。GUI がカードに出す）。
    pub text: String,
    /// RFC 3339。
    pub at: String,
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
    /// GUI 監査 H4（Phase 29）: 裏方タスクの印（`TaskSummary.support` と同じ規則）。
    pub support: Option<String>,
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

// ---- ADR-0040 D6（Phase 48）: リリース（自己改善のデプロイ）----

/// `GET /releases` の応答（読み取り。トークンは要らない）。
///
/// 中身は `[selfdeploy] releases_dir` の下の `manifest.json` / `gate.json` / `verify.json` と
/// `current` / `previous` の symlink、`daemon_instances` の行を**読むだけ**で作る。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Releases {
    /// `<releases_dir>/../current` が指す sha12（無ければ `null`）。
    pub current: Option<String>,
    /// `<releases_dir>/../previous` が指す sha12（無ければ `null`）。
    pub previous: Option<String>,
    /// いまこの要求に答えているプロセス自身（`GET /health` の `release` / `role` と同じ値）。
    pub running: ReleaseRunning,
    /// ADR-0040 D4 の `daemon_instances`（引き継ぎの進行が見える）。`started_at` 昇順。
    pub instances: Vec<task_core::DaemonInstance>,
    /// リリース一覧。`built_at` の新しい順。
    pub items: Vec<ReleaseItem>,
}

/// `GET /releases` の `running`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseRunning {
    /// `--release <sha12>` / `CELERIS_RELEASE` / `"dev"`。
    pub release: String,
    /// `active` / `standby` / `draining` / `verify`。
    pub role: String,
    pub instance_id: String,
}

/// `GET /releases` の `items[]` の 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseItem {
    /// ディレクトリ名（`git rev-parse --short=12`）。
    pub sha12: String,
    /// `manifest.json` の `ref`（`release.sh` に渡した git ref）。読めなければ `null`。
    pub r#ref: Option<String>,
    /// `manifest.json` の `built_at`（RFC 3339）。読めなければ `null`（並びは最後）。
    pub built_at: Option<String>,
    /// `manifest.json` の `schema_version`。
    pub schema_version: Option<u32>,
    /// `gate.json` の `ok`（`release.sh` の gate が全段 exit 0 だったか）。読めなければ `false`。
    pub gate_ok: bool,
    /// ADR-0058: `gate.json` の内訳（`ok`/`failed_step`/`steps[]`）。`gate.json` が読めない・壊れて
    /// いるときは `null`（そのときも `gate_ok` は `false` のまま出る。既存の挙動を変えない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<ReleaseGate>,
    /// `verify.json`。無ければ `null`（＝未検証。昇格できない）。
    pub verify: Option<ReleaseVerify>,
    /// ADR-0041 D3: `promoted.json` の `promoted_at`（`promote.sh` が昇格に成功したときだけ書く）。
    /// 一度も昇格していないリリースは `null`。
    pub promoted_at: Option<String>,
    /// ADR-0041 D3: この sha が `[selfdeploy] repo` の `main` の**祖先**か
    /// （`git merge-base --is-ancestor <sha> main`）。`false` なら本番のコードが `main` に
    /// 戻っていない。リポジトリが無い・git が動かない・その sha を知らないときは `null`。
    pub on_main: Option<bool>,
    /// ADR-0041 D4: いま動いている版からこのリリースへ**何が変わるか**（`changes.json`）。
    /// Phase 48 以前に作られたリリースには無いので `null`。
    pub changes: Option<ReleaseChanges>,
    pub is_current: bool,
    pub is_previous: bool,
    /// `promote.lock` に書かれた pid がまだ生きている（昇格が走っている最中）。
    pub promoting: bool,
    /// 直近の昇格の試みが失敗した記録（`<release>/promote_failed.json`）。次の昇格の試みが
    /// 始まると消える（celeris の `start_promote` が書き直す前に消す）ので、`null` なら
    /// 「まだ一度も失敗していない」か「その後もう一度試している」のどちらか。
    /// 昇格が成功すると `promoted_at` が新しくなる一方でこれは残らない（`promote.sh` は
    /// 成功時にこのファイルを書かない）。GUI はこれが非 `null` かつ `promoting` が偽のときだけ
    /// 赤いバナーで出す。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promote_failed: Option<ReleasePromoteFailure>,
    /// Phase 105（本番の観測、2026-09-22 21:55 UTC）: `promote.lock` の pid が死んでいるのに
    /// `promoted.json` が無く、`promote.log` も `promote.sh` の成功時の一行まで進んでいない
    /// （＝旧デーモンの drain が `promote.sh` を cgroup ごと巻き添えにした等で途中で止まった）。
    /// GUI 表示は次の GUI Phase（この Phase では契約だけ）。
    #[serde(default)]
    pub promote_stale: bool,
    /// `promote.log` の最後の（空でない）行。無ければ `null`。`promote_stale` の手がかり。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promote_last_line: Option<String>,
    /// `manifest.json` / `gate.json` が読めなかったときの一行（GUI が「壊れている」と出す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// `<release>/promote_failed.json` の中身（`promote.sh` が非 0 で終わったときだけ書く）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReleasePromoteFailure {
    /// RFC 3339。
    pub failed_at: String,
    /// `promote.log` の末尾（最大 20 行）。原因を画面で分かる範囲だけ見せる（全文は `promote.log`）。
    pub error: String,
}

/// `verify.json` の要約（ADR-0040 D3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseVerify {
    /// 検査 1〜4・4b・6 が全部真。`promote.sh` はこれが真でなければ拒否する
    /// （ADR-0041 追記「検査 4b」）。
    pub ok: bool,
    /// N-1 互換（旧バイナリが新スキーマを読める）。偽なら昇格は停止 → 起動になる。
    pub live_ok: bool,
    /// RFC 3339。
    pub at: Option<String>,
    /// ADR-0058: `verify.json` の `checks[]` をそのまま運んだもの（検査ごとの合否・詳細）。
    /// `verify.json` にこのキーが無い（この Phase 以前に作られたリリース）ときは空配列。
    #[serde(default)]
    pub checks: Vec<ReleaseVerifyCheck>,
}

/// `verify.json` の `checks[]` の 1 件（ADR-0058）。`scripts/selfdeploy/verify.sh` の
/// `record <id> <name> <ok> <detail> [task_id] [elapsed_s]` がそのまま書いたもの。
/// `task_id`（検査 6 の煙試験タスク id）は運ばない — GUI に使い道が無い（ADR-0058 D1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseVerifyCheck {
    /// `"1"`〜`"6"`、`"4b"`。文字列（数値専用にできない。ADR-0041 追記）。
    pub id: String,
    pub name: String,
    pub ok: bool,
    pub detail: String,
    /// 検査 6（煙試験）だけが埋める。他の検査は `0.0` のまま record されるので、値としては
    /// 常に `Some`（`verify.json` が `elapsed_s` を省略しない）。
    #[serde(default)]
    pub elapsed_s: Option<f64>,
}

/// `gate.json` の内訳（ADR-0058）。`release.sh` の `write_gate_json` がそのまま書いたもの。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseGate {
    /// gate の全段が exit 0 だったか（`ReleaseItem.gate_ok` と同じ値）。
    pub ok: bool,
    /// 最初に非 0 で終わった段の名前。全段成功なら `null`。
    pub failed_step: Option<String>,
    /// `gate.json` の `steps[]`。`run_step` が呼ばれた順（`GATE_OK` が偽になった後の段は
    /// 走らないので、`failed_step` 以降は含まれない）。
    #[serde(default)]
    pub steps: Vec<ReleaseGateStep>,
}

/// `gate.json` の `steps[]` の 1 件。ログのファイル名（`log`）は本番ホストのローカルパスで
/// GUI から読めないため運ばない（ADR-0058 D2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseGateStep {
    pub step: String,
    pub exit: i32,
    pub secs: f64,
}

/// `changes.json` の要約（ADR-0041 D4）。`release.sh` が**ビルド時の `current`**（`base`）から
/// そのリリースまでの差分を書いたもの。GUI は昇格の前にこれを人へ見せる。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseChanges {
    /// 差分の起点（ビルド時の `current` の sha12）。`current` が無いときに作られたリリースは `null`。
    pub base: Option<String>,
    /// `base` がいまの `current` と違う（＝この一覧は「いま昇格したら何が変わるか」ではない）。
    pub stale: bool,
    /// `base..<sha>` のコミット数（`commits` は最大 50 件までなので、こちらも 50 で頭打ち）。
    pub commit_count: usize,
    /// 変わったファイルの数。
    pub file_count: usize,
    /// **安全に関わる変更**（`scripts/selfdeploy/` などのパスに前方一致したもの。
    /// 一覧の定義は `scripts/selfdeploy/lib.sh` の `SD_SENSITIVE_PATTERNS` 1 か所）。
    pub sensitive: Vec<String>,
    /// 新しい順、最大 50 件。
    pub commits: Vec<ReleaseCommit>,
}

/// `changes.json` の `commits[]` の 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReleaseCommit {
    /// 完全な sha（GUI は先頭 7 桁を出す）。
    pub sha: String,
    pub subject: String,
}

/// `POST /releases/{sha12}/promote` → 202 の応答。**昇格そのものはこの API の外**
/// （`promote.sh` を detached で起こすだけ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReleasePromoteAccepted {
    pub sha12: String,
    /// `promote.sh` の出力を流し込んでいるファイルの絶対パス（中身は API では出さない）。
    pub log: String,
    /// RFC 3339。
    pub started_at: String,
    /// ADR-0041 D4: どちらの `promote.sh` を起こしたか。`"current"` = いま動いている版に同梱の
    /// スクリプト（既定。新しいコードの昇格スクリプトは、それ自身が昇格された後の次の昇格から使われる）、
    /// `"target"` = 昇格先に同梱のスクリプト（`current` に `scripts/` が無い Phase 48 以前のときだけ）。
    pub script_from: String,
}

// ============================================================================
// ADR-0043（Phase 52）: 案件のリポジトリ（D1）とタスクのファイル閲覧（D6）
// ここから下がこの Phase で足した型。既存の型には触っていない。
// ============================================================================

/// `GET /projects/{id}/repos` の応答（primary が先頭、あとは作った順）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RepoList {
    pub items: Vec<task_core::ProjectRepo>,
}

/// `POST /projects/{id}/repos` の要求本文（管理系）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoCreateBody {
    /// 案件の中で一意の slug。省略すると `location` のディレクトリ名から作る。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 省略すると `location` から決める（`<path>/.git` があれば `git`、無ければ `dir`。
    /// リモートは `git`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<task_core::RepoKind>,
    /// `{"kind":"local","path":"~/workspace/benchfs"}` か
    /// `{"kind":"remote","cluster":"pegasus","path":"/work/.../benchfs"}`。
    /// `Local` の `~` は celeris の `$HOME` で展開して保存する。知らない `cluster` は 422。
    pub location: task_core::WorkspaceSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    /// remote のみ。省略は既定の `worktree`（ADR-0019 の (a)）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<task_core::RepoSync>,
    /// 省略は `auto`（`workspace.toml` に従う。無ければ host）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<task_core::RepoRun>,
    /// 案件の「主なリポジトリ」にする。案件の最初の 1 件は自動的に primary。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_primary: bool,
}

/// `PATCH /repos/{id}` の要求本文（管理系）。書いたものだけ変える。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoPatchBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<task_core::RepoKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<task_core::WorkspaceSpec>,
    /// 省略なら変えない、`null` なら消す。
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_branch: Option<Option<String>>,
    /// 省略なら変えない、`null` なら消す（＝既定の `worktree`）。
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub sync: Option<Option<task_core::RepoSync>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<task_core::RepoRun>,
    /// `true` にするとこの行が案件の primary になる（他は落ちる）。`false` は何もしない
    /// （primary を空にはできない。別の行を primary にする）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_primary: Option<bool>,
}

/// `GET /tasks/{id}/tree` の応答（ADR-0043 D6。読み取り。トークンは要らない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreeView {
    /// 見ているリポジトリの名前。
    pub repo: String,
    /// そのリポジトリの作業ツリーからの相対パス（根は `""`）。
    pub path: String,
    /// このタスクが使っているリポジトリの一覧（GUI のタブ）。
    pub repos: Vec<TreeRepoView>,
    /// `path` の直下（ディレクトリが先、あとは名前順）。
    pub entries: Vec<TreeEntry>,
}

/// `TreeView.repos[]` の 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreeRepoView {
    pub name: String,
    /// `git`（worktree）か `dir`（シンボリックリンク）。
    pub kind: String,
    /// タスクの中での絶対パス。
    pub dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// `TreeView.entries[]` の 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreeEntry {
    pub name: String,
    /// リポジトリの作業ツリーからの相対パス。
    pub path: String,
    /// `dir` / `file` / `other`（シンボリックリンクは指す先で `dir` / `file`）。
    pub kind: String,
    /// ファイルのときだけ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// `GET /tasks/{id}/tree/file` の応答（ADR-0043 D6）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreeFileView {
    pub repo: String,
    pub path: String,
    pub size: u64,
    /// テキストとして読めなかった（NUL を含む・UTF-8 でない）。このときは `text` を返さない。
    pub binary: bool,
    /// 512 KiB を超えたので `text` を返していない。
    pub too_large: bool,
    /// 本文（テキストで 512 KiB 以下のときだけ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

// ============================================================================
// ADR-0043 D5（Phase 54）: 変更の取り込み（差分・merge・PR・衝突タスク）
// ここから下が Phase 54 で足した型。上の節（Phase 52）にも既存の型にも触っていない。
// ============================================================================

/// `GET /tasks/{id}/changes` の応答（ADR-0043 D5。読み取り。トークンは要らない）。
///
/// git のリポジトリだけを並べる（`dir` のリポジトリは対象外）。PR の状態の同期（`gh pr view`）は
/// **この API を呼んだときだけ**行う（ADR-0043 D5: 常時同期はしない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChangesView {
    /// 上司の取り込み判定とリリース準備（ADR-0051）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<task_core::Delivery>,
    pub task_id: String,
    /// git のリポジトリごとの差分（順番はタスクの `repos` の順）。
    pub repos: Vec<RepoChangesView>,
    /// `gh` が PATH にあって認証済みか（GUI が「PR を作る」を出すかどうか）。
    pub gh: bool,
    /// `[github] merge_method`（「Celeris で merge」が使う方法）。
    pub merge_method: String,
}

/// `ChangesView.repos[]` の 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RepoChangesView {
    pub repo: String,
    /// タスクのブランチ（`celeris/<task_id>`）。
    pub branch: String,
    /// 取り込む先（`project_repos.default_branch`、無ければ検出）。
    pub default_branch: String,
    /// 分岐した地点の sha。
    pub base: String,
    /// いまのブランチの先端の sha。
    pub head: String,
    /// `base..head` のコミットの数（コミットが無ければ 0）。
    pub ahead: u64,
    pub files: Vec<task_ops::changes::ChangedFile>,
    pub stat: task_ops::changes::DiffStat,
    /// 未コミットの変更がある。
    pub dirty: bool,
    /// worktree もブランチも無い（取り込み済み・中止済み）。
    pub missing: bool,
    /// `origin` リモートがある（PR を作れる前提の 1 つ）。
    pub origin: bool,
    /// このリポジトリの最新の取り込みの記録（無ければ `null`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration: Option<task_core::TaskIntegration>,
}

/// `GET /tasks/{id}/changes/{repo}/diff?path=` の応答（ADR-0043 D5。200 KiB で切る）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeDiffView {
    pub repo: String,
    pub path: String,
    /// unified diff（差分が無ければ空文字列）。
    pub diff: String,
    /// 200 KiB を超えたので途中で切った。
    pub truncated: bool,
}

/// `POST /tasks/{id}/changes/{repo}/integrate` の要求本文（**管理系。人だけ**。ADR-0043 D5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntegrateBody {
    /// `merge` / `pr` / `discard`。
    pub method: task_core::IntegrationMethod,
    /// 人のひとこと（記録の `detail` の先頭に入る。PR の本文には入れない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `discard` のときだけ必須（取り返しがつかないので確認を取る）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confirm: bool,
}

/// 取り込みの結果（`integrate` と `pr/merge` の応答）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrateResult {
    pub integration: task_core::TaskIntegration,
    /// 衝突したときに作った「衝突の解消」タスク（ADR-0043 D5）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_task_id: Option<String>,
}

/// `GET /projects/{id}/integrations` の応答（案件画面の「PR と取り込み」。ADR-0043 D5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectIntegrations {
    /// タスク × リポジトリごとに最新の 1 件（新しい順）。
    pub items: Vec<ProjectIntegrationItem>,
}

/// `ProjectIntegrations.items[]` の 1 件（記録 + 人が読むためのタスクの題名）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectIntegrationItem {
    pub integration: task_core::TaskIntegration,
    pub task_title: String,
    pub task_status: Status,
}

// ========== ADR-0048 D1（Phase 60a）: Console（一本の流れ）==========
//
// `GET /console` と `GET /console/stream` が返す**正規化したブロック**。ストアを引くのは
// `crate::console`、決定的な写像（束ね方・1 行の作り方・カーソル）は `task_ops::console` にある。
// ここにあるのは HTTP に出る形だけで、判断は無い。

/// `GET /console` の応答（ADR-0048 D1）。`items` は**時刻の昇順**（新しいものが最後）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConsolePage {
    pub items: Vec<ConsoleBlock>,
    /// 次に読む位置。`GET /console?since=` にそのまま渡す（中身は不透明な文字列）。
    /// 1 件も無ければ渡された `since` をそのまま返す（それも無ければ `null`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Console の 1 ブロック（ADR-0048 D1 の 8 種 + 予約の `knowledge`）。
/// `at` は RFC 3339、`cursor` はそのブロックの位置（`since` にそのまま渡せる）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConsoleBlock {
    /// 人の発言（`messages` の `role = user`）。
    Human {
        at: String,
        cursor: String,
        message_id: String,
        /// 話しかけた相手（組織のノード id）。
        node_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<task_core::ProjectId>,
        /// この 1 往復を起こした対話用タスク。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<TaskId>,
        text: String,
        /// ADR-0056 D2（Phase 78）: 発した外部 MCP クライアント（`mcp:<client_id>`）。人の発言なら
        /// `None`（GUI はこれがあれば「外部（<name>）」の帯を出す。名前の解決は GUI 側）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author: Option<String>,
    },
    /// CoS または部署ノードの返事（`messages` の `role = node`。本文は Markdown）。
    Reply {
        at: String,
        cursor: String,
        message_id: String,
        node_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<task_core::ProjectId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<TaskId>,
        /// 返事を作った run。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        text: String,
        /// ADR-0048 D3（Phase 60b）: CoS の返事が `actions` を宣言していれば、taskd が実行した結果
        /// （実行できた / できなかった）。GUI は「→ タスクを作りました: …」をここから出す。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actions_result: Option<task_core::MessageMetadata>,
        /// ADR-0054 D2（Phase 68）: `streaming`（run 中。`text` はここまでの積み上げ）か
        /// `done`（`messages` に確定した返事）。無ければ `done`（過去のブロック・このフィールドを
        /// 知らないクライアントとの後方互換）。
        #[serde(default)]
        state: task_ops::console::ConsoleReplyState,
        /// 育つ返事の「考え中…」の最新の 1 行（`state = streaming` のときだけ意味がある。置き換え式）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        /// 育つ返事の中の tool_use/tool_result（`state = streaming` のときだけ意味がある）。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        steps: Vec<task_ops::console::ConsoleReplyStep>,
    },
    /// タスクの開始・終了・失敗・中止・割り込み（`Event::Transitioned` の 1 行）。
    Task {
        at: String,
        cursor: String,
        task: task_ops::console::ConsoleTaskLine,
    },
    /// ワーカーの進行（ADR-0048 D2 の正規化を run ごとに束ねたもの。**既定は折り畳み**）。
    Progress {
        at: String,
        cursor: String,
        progress: task_ops::console::ConsoleProgress,
        /// 折り畳みの見出しに出す、そのタスクの題名。
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assignee: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        harness: Option<String>,
        tier: Tier,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<task_core::ProjectId>,
    },
    /// ディスパッチャが人に出した質問（`Event::QuestionRaised`）。同じ質問が認可（`approvals`）にも
    /// あるときは**認可の側だけ**出す（同じことを 2 回出さない）。
    Question {
        at: String,
        cursor: String,
        task_id: TaskId,
        run_id: String,
        /// 聞いてきたノード（`task.assignee`。無ければ `null`）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<task_core::ProjectId>,
        text: String,
        /// 人が答えたか。
        answered: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answer: Option<String>,
    },
    /// 認可（ADR-0033 D5）。状態（`decision` / `answer` / `decided_at`）ごと渡す。
    Approval {
        at: String,
        cursor: String,
        approval: task_core::Approval,
    },
    /// 途中目標の提案（ADR-0038）。秘書のレビューの返事が付いていれば一緒に渡す。
    Milestone {
        at: String,
        cursor: String,
        milestone: task_core::Milestone,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        review: Option<MilestoneReviewView>,
    },
    /// 報告（ADR-0034）。見出しと本文を渡す（GUI は見出しだけ出して開かせる）。
    Report {
        at: String,
        cursor: String,
        report: task_core::Report,
    },
    /// 知識整理 run の結果（ADR-0047 D4 / D5。Phase 62）。「この仕事から知識 N 件: 取り込み a /
    /// 候補 b / 破棄 c」の 1 行。`task_id` の終端から知識整理 run（`run_task_id`）が起き、
    /// `apply_candidates` の集計が付いたときに 1 件出る（`state = applied`。適用前は出さない）。
    Knowledge {
        at: String,
        cursor: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<task_core::ProjectId>,
        /// 知識整理 run の元になったタスク。
        task_id: TaskId,
        task_title: String,
        /// 知識整理 run（裏方の支援タスク）自身の id。
        run_task_id: TaskId,
        /// `applied`（適用済み）| `failed`（run が失敗し候補が無い）。
        state: String,
        /// 直接 KB にコミットした件数。
        ingested: u32,
        /// `_inbox/` へ送った件数（人の確認待ち）。
        inbox: u32,
        /// 検査で落とした件数。
        discarded: u32,
        /// ADR-0052 D2（Phase 64）: 抽出した経路。`"langmem"`（Qwen）か `"fallback:<adapter>"`
        /// （Qwen に届かず tier cheap の汎用ハーネスで抽出した）。分からなければ `null`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<String>,
    },
}
// ========== ADR-0048 D1（Phase 60a）: ここまで ==========

// ========== ADR-0053 D1/D4（Phase 65。GUI 表示は Phase 66）: `GET /llm/sources` ==========

/// `GET /llm/sources` の 1 アカウント（`llm-proxy` の `claude-oauth`/`codex-oauth` のプール）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LlmSourceAccountView {
    pub id: String,
    pub logged_in: bool,
    /// 0.0〜1.0（測れないときは `null`。値を捏造しない。ADR-0024 D3 と同じ規律）。短期・長期のうち
    /// **厳しい方**（残りが少ない方）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
    /// ADR-0053 D4（Phase 66）: 短期枠（Claude の 5 時間 / Codex の週内相当）だけの残り。測れないときは `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_short: Option<f64>,
    /// ADR-0053 D4: 長期枠（7 日）だけの残り。測れないときは `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_long: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<String>,
}

/// `GET /llm/sources` の 1 供給元。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LlmSourceView {
    /// `claude-oauth` / `codex-oauth` / `openai-compatible:<id>`。
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    /// `openai-compatible` だけ probe した結果。oauth のプールは `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachable: Option<bool>,
    pub accounts: Vec<LlmSourceAccountView>,
    pub last_hour_requests: u64,
    pub last_hour_prompt_tokens: u64,
    pub last_hour_completion_tokens: u64,
}

/// ADR-0053 D4（Phase 66）: `celeris/<tier>` が今どこに解決するか。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LlmCelerisTierView {
    /// `"frontier"` / `"standard"` / `"cheap"`。
    pub tier: String,
    /// 解決先の供給元 id（`sources[].id` と同じ形）。今選べる候補が無ければ `null`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolves_to: Option<String>,
}

/// `GET /llm/sources`（ADR-0053 D4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LlmSourcesView {
    pub sources: Vec<LlmSourceView>,
    /// 古いスナップショットには無いので既定は空。
    #[serde(default)]
    pub celeris_tiers: Vec<LlmCelerisTierView>,
}
// ========== ADR-0053（Phase 65）: ここまで ==========
