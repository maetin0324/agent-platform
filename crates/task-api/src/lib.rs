//! `task-api`: taskd の HTTP API v1（`docs/gui/api.md`、ADR-0013 D2〜D4 / D8 / D11）。
//!
//! - `/api/v1` 配下の 26 エンドポイント。JSON で応答し、エラーは `application/problem+json`、通知は SSE。
//! - ハンドラは協調判断をしない。読み取りはストアのクエリと `task-ops` のビュー、状態変更は `task-ops` 経由だけ。
//!   LLM 呼び出し・ワーカーの起動・`Check::Command` の実行はしない（DESIGN.md 原則 1〜4）。
//! - DB は API 専用の `SqliteStore` 接続を 1 つ持ち、呼び出しは `spawn_blocking` で行う（ADR-0013 D3）。
//! - デーモンの状態は `tokio::sync::watch` の `DaemonSnapshot` から読む（ADR-0013 D4）。

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use task_core::AccountAdapter;
use task_ops::daemon::DaemonSnapshot;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

mod accounts;
mod admin;
mod approvals;
/// ADR-0043 D5（Phase 54）: 変更の取り込み（差分・merge・PR・衝突タスク）。
pub mod changes;
pub mod conversation;
mod files;
mod handlers;
/// ADR-0044 D6（Phase 55）: 案件・途中目標の中止・一時停止・アーカイブ。
pub mod lifecycle;
pub mod memory;
mod middleware;
pub mod notify;
mod problem;
pub mod milestones;
pub mod project_plan;
mod query;
/// ADR-0043 D1（Phase 52）: 案件のリポジトリ（`project_repos`）の CRUD。
pub mod repos;
pub mod releases;
mod reports;
pub mod schema;
pub mod secrets;
mod sse;
pub mod timeline;
mod state;
mod stats;
/// ADR-0043 D6（Phase 52）: タスクの作業ツリーの閲覧（読み取り）。
pub mod tree;
pub mod types;

pub use approvals::{
    ApprovalDecideBody, ApprovalDecideResult, ApprovalList, StandingRuleCreateBody, StandingRuleList,
};
pub use conversation::{MessageAccepted, MessageList, MessagePostBody};
pub use memory::MemoryView;
pub use milestones::{MilestoneDecideBody, MilestoneDecided};
pub use project_plan::{ProjectPlanAccepted, ProjectPlanBody};
pub use admin::{
    AccountAdminError, AccountCheckOutcome, AccountLoginCodeOutcome, AccountLoginStartOutcome, AdminRequest,
    CheckError, ClusterAdminError, ClusterConnectCodeOutcome, ClusterConnectStartOutcome, NotifyAdminError,
    NotifyTestOutcome, ProviderCheckOutcome, ProviderCheckResult,
};
pub use notify::{NotifyRecent, NotifyTestResult, NotifyView};
pub use releases::{BRANCH_COMMITS_LIMIT, ReleasePromoteError, ReleaseSource, ReleasesFs, SharedReleaseSource};
pub use reports::{ReportDetail, ReportList, ReportsNotifiedResult, ReportsReadBody, ReportsReadResult};
pub use schema::{API_V1_SCHEMA_JSON, ApiV1Schema, api_v1_schema_json, api_v1_schema_value};
pub use state::{ApiState, StreamTuning};
pub use stats::classify_outcome;
pub use tree::MAX_TEXT_BYTES;
pub use types::{
    AnswerBody, ApiConfigView, ArtifactList, ArtifactView, CancelBody, ClusterConfigView, ClusterConnectCodeBody,
    ClusterConnectResult, ClusterConnectStart, ClusterView, Clusters, ConfigView, DaemonView, DailyUsage, DbInfo,
    DecisionBody, EventsPage, GenreConfigView, Health, Problem, ProviderConfigView, ProviderStats, ProviderView,
    Providers, ReleaseChanges, ReleaseCommit, ReleaseItem, ReleasePromoteAccepted, ReleaseRunning, ReleaseVerify,
    Releases, RetryBody,
    ReviewerConfigView, RoleConfigView, RunList, SecretList, SecretPutBody, SecretPutResult, SecretUse, SecretView,
    StreamHeartbeat, StreamHello, StreamReset, ValidationError,
};
// ---- ADR-0043（Phase 52）: 案件のリポジトリとファイル閲覧 ----
pub use types::{RepoCreateBody, RepoList, RepoPatchBody, TreeEntry, TreeFileView, TreeRepoView, TreeView};
// ---- ADR-0043 D5（Phase 54）: 変更の取り込み ----
pub use types::{
    ChangeDiffView, ChangesView, IntegrateBody, IntegrateResult, ProjectIntegrationItem, ProjectIntegrations,
    RepoChangesView,
};

/// `GET /health` の `api_version`。互換性を壊す変更は `/api/v2` で行う（ADR-0013 D8）。
pub const API_VERSION: &str = "1";
/// 全エンドポイントのベースパス。
pub const BASE_PATH: &str = "/api/v1";
/// SSE の同時接続数の上限（api.md §1.1 / §4。設定キーにしない）。
pub const MAX_STREAMS: usize = 16;
/// SSE の `events_since` ポーリング間隔（api.md §4）。
pub const STREAM_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// SSE の `heartbeat` 間隔（api.md §4）。
pub const STREAM_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
/// 再開位置から最新までがこの件数を超えたら `reset` を送る（api.md §4）。
pub const STREAM_RESET_THRESHOLD: u64 = 10_000;
/// SSE の 1 回のポーリングで読む最大件数（api.md §4）。
pub const STREAM_BATCH: usize = 1_000;
/// 変更系の要求本文の上限（api.md §1.2）。
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// `X-Taskd-Sha256-Current` / `sha256_current` を計算する最大ファイルサイズ（api.md §3.8）。
pub const SHA256_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// taskd が API を起動するときに渡す設定（`[api]` と、ビュー・`GET /config` に必要な値）。
#[derive(Clone)]
pub struct ApiSettings {
    pub listen: SocketAddr,
    /// `token_file` の内容（前後の空白除去済み）。`None` なら認証しない（loopback のみの構成）。
    pub token: Option<String>,
    /// `[api] allowed_hosts`（`localhost` / `127.0.0.1` / `[::1]` / `listen` のホストは常に許可）。
    pub allowed_hosts: Vec<String>,
    /// API 専用の `SqliteStore` を `open_with` で開く DB のパス。
    pub db_path: PathBuf,
    pub busy_timeout: Duration,
    pub view: task_ops::view::ViewContext,
    pub config_view: ConfigView,
    /// ADR-0016 D1 / M3: `[[roles]]`。`POST /tasks` で省略された `tier` / `adapter` / 予算の既定に使う。
    pub roles: Vec<task_core::RoleSpec>,
    /// ADR-0027 D1: `[[genres]]`。`POST /tasks` の `genre` の検証と役割の既定の解決に使う。API は常に
    /// 完全な設定を持つので、ここが空でなければ知らない `genre` / `genre` と `role` の不整合は常に 422。
    pub genres: Vec<task_core::GenreSpec>,
    /// Phase 30（ADR-0033 D4 追記）: 対話は常にこの分野で走る（ノードの `genre` は使わない）。
    /// `[conversation] genre`（既定 `task_core::CONVERSATION_GENRE = "secretary"`）。
    pub conversation_genre: String,
    pub taskd_version: String,
    /// ディスパッチャのスナップショットと同じ値。
    pub instance_id: String,
    /// RFC 3339。
    pub started_at: String,
    /// ADR-0017 M1: `providers.d/<id>.toml` の書き込み先。`providers_include` が未設定なら `None`
    /// （そのときは管理系の作成/変更/削除が使えない）。
    pub providers_dir: Option<PathBuf>,
    /// ADR-0017 M2: `reload` / `check` を taskd（ワーカー起動ができる側）へ委譲するチャネル。`None` なら両方使えない。
    pub admin_tx: Option<mpsc::Sender<AdminRequest>>,
    /// ADR-0024 D1 / ADR-0025 D1/D6: アダプタごとの `[accounts]` の根ディレクトリの絶対パス（設定されている
    /// アダプタだけキーを持つ）。`[accounts]` が無ければ空（そのときは `GET /accounts` が
    /// `{root: null, roots: {}, items: []}`、管理系は 409 `accounts_unavailable`）。
    pub accounts_roots: HashMap<AccountAdapter, PathBuf>,
    /// ADR-0024 D1: `[accounts] max_runs_per_account`。
    pub max_runs_per_account: usize,
    /// ADR-0030 D1: `[secrets] dir` の絶対パス。`None` なら秘密の管理系は 409 `secrets_unavailable`。
    pub secrets_dir: Option<PathBuf>,
    /// ADR-0030 D3: 秘密 id → それを使っている adapter/provider の `env_from_secrets`（`GET /secrets` の
    /// `used_by`）。taskd が設定から導いて渡す（task-api は再計算しない）。
    pub secret_usage: HashMap<String, Vec<types::SecretUse>>,
    /// ADR-0033 D6（GUI 監査対応 Phase 29）: `[memory] dir` の絶対パス。`None` なら `GET /org/{id}/memory`
    /// は 409 `memory_unavailable`。
    pub memory_dir: Option<PathBuf>,
    /// ADR-0037 D2（Phase 39）: `[notify] discord_webhook_secret`。この id の秘密が `[secrets] dir` に
    /// あれば「設定済み」。**値は API では読まない**（指紋だけ出す）。
    pub notify_secret_id: String,
    /// ADR-0037 D3: `[notify] gui_base_url`（文面のリンクの根。無ければリンク無し）。
    pub notify_gui_base_url: Option<String>,
    /// ADR-0040 D6（Phase 48）: `[selfdeploy] releases_dir` を読む係（taskd が渡す。task-api は
    /// リリースのファイル規約を知らない）。`None` なら `GET /releases` は空、昇格は 409。
    pub releases: Option<SharedReleaseSource>,
    /// ADR-0040 D4（Phase 47）: このプロセスのリリース（`--release <sha12>` / `TASKD_RELEASE` / `"dev"`）。
    /// `GET /health` の `release`。
    pub release: String,
    /// ADR-0040 D3: `--mode`（`normal` / `verify`）。`GET /health` の `mode`。
    pub mode: task_core::DaemonMode,
    /// ADR-0040 D4: いまの役割。**tick ループだけが書き、API は読むだけ**。`standby` / `draining` の間は
    /// ディスパッチャの状態を要する管理 API（`reload` / `check` / クラスタ接続 / アカウントのログイン中継 /
    /// `notify/test`）が 503 `standby` になる。
    pub role: task_core::SharedRole,
    /// ADR-0043 D5（Phase 54）: `[github]`（`gh` の場所と「Celeris で merge」の方法）。
    pub github: GithubSettings,
}

/// ADR-0043 D5（Phase 54）: `[github]` の写し。taskd が設定から渡す（task-api は TOML を読まない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubSettings {
    /// `gh` CLI の場所（PATH にあれば `"gh"`）。
    pub gh: String,
    /// `gh pr merge --<method>`（`merge` / `squash` / `rebase`）。
    pub merge_method: String,
}

impl Default for GithubSettings {
    fn default() -> Self {
        Self {
            gh: "gh".to_string(),
            merge_method: "merge".to_string(),
        }
    }
}

impl std::fmt::Debug for ApiSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiSettings")
            .field("listen", &self.listen)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("allowed_hosts", &self.allowed_hosts)
            .field("db_path", &self.db_path)
            .field("busy_timeout", &self.busy_timeout)
            .field("view", &self.view)
            .field("config_view", &self.config_view)
            .field("roles", &self.roles)
            .field("genres", &self.genres)
            .field("conversation_genre", &self.conversation_genre)
            .field("taskd_version", &self.taskd_version)
            .field("instance_id", &self.instance_id)
            .field("started_at", &self.started_at)
            .field("providers_dir", &self.providers_dir)
            .field("admin_tx", &self.admin_tx.as_ref().map(|_| "<sender>"))
            .field("accounts_roots", &self.accounts_roots)
            .field("max_runs_per_account", &self.max_runs_per_account)
            .field("secrets_dir", &self.secrets_dir)
            .field("secret_usage", &self.secret_usage.keys().collect::<Vec<_>>())
            .field("memory_dir", &self.memory_dir)
            .field("notify_secret_id", &self.notify_secret_id)
            .field("notify_gui_base_url", &self.notify_gui_base_url)
            .field("releases", &self.releases.as_ref().map(|_| "<source>"))
            .field("release", &self.release)
            .field("mode", &self.mode)
            .field("role", &self.role.get())
            .finish()
    }
}

/// API の起動・実行の失敗（HTTP のエラー応答 `Problem` とは別）。
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("cannot open the API database connection: {0}")]
    Store(#[from] task_core::StoreError),
    #[error("cannot inspect the database journal mode: {0}")]
    JournalMode(#[from] rusqlite::Error),
    #[error("cannot bind the API listener on {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("API server failed: {0}")]
    Serve(#[source] std::io::Error),
    #[error("API startup task failed: {0}")]
    Startup(String),
}

/// `/api/v1` の全エンドポイントと共通の検査（Host / 認証 / Origin / Content-Type / 本文サイズ）を持つルータ。
pub fn router(state: ApiState) -> axum::Router {
    handlers::router(state)
}

/// `settings.listen` に bind し、`shutdown` が完了したら SSE を閉じて graceful に止める。
pub async fn serve(
    settings: ApiSettings,
    daemon: watch::Receiver<Option<DaemonSnapshot>>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ApiError> {
    let addr = settings.listen;
    let state = tokio::task::spawn_blocking(move || ApiState::new(settings, daemon))
        .await
        .map_err(|e| ApiError::Startup(e.to_string()))??;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| ApiError::Bind { addr, source })?;
    serve_with_listener(listener, state, shutdown).await
}

/// bind 済みの `listener` と作成済みの `state` で API を動かす（`serve` の本体。テストは `127.0.0.1:0` で使う）。
pub async fn serve_with_listener(
    listener: TcpListener,
    state: ApiState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ApiError> {
    if let Ok(addr) = listener.local_addr() {
        tracing::info!(%addr, "taskd API listening");
    }
    let app = router(state.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            state.close_streams();
        })
        .await
        .map_err(ApiError::Serve)
}
