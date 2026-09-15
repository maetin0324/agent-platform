//! `task-api`: taskd の HTTP API v1（`docs/gui/api.md`、ADR-0013 D2〜D4 / D8 / D11）。
//!
//! - `/api/v1` 配下の 26 エンドポイント。JSON で応答し、エラーは `application/problem+json`、通知は SSE。
//! - ハンドラは協調判断をしない。読み取りはストアのクエリと `task-ops` のビュー、状態変更は `task-ops` 経由だけ。
//!   LLM 呼び出し・ワーカーの起動・`Check::Command` の実行はしない（DESIGN.md 原則 1〜4）。
//! - DB は API 専用の `SqliteStore` 接続を 1 つ持ち、呼び出しは `spawn_blocking` で行う（ADR-0013 D3）。
//! - デーモンの状態は `tokio::sync::watch` の `DaemonSnapshot` から読む（ADR-0013 D4）。

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use task_ops::daemon::DaemonSnapshot;
use tokio::net::TcpListener;
use tokio::sync::watch;

mod files;
mod handlers;
mod middleware;
mod problem;
mod query;
pub mod schema;
mod sse;
mod state;
mod stats;
pub mod types;

pub use schema::{API_V1_SCHEMA_JSON, ApiV1Schema, api_v1_schema_json, api_v1_schema_value};
pub use state::{ApiState, StreamTuning};
pub use stats::classify_outcome;
pub use types::{
    AnswerBody, ApiConfigView, ArtifactList, ArtifactView, CancelBody, ClusterConfigView, ClusterView, Clusters,
    ConfigView, DaemonView, DailyUsage, DbInfo, DecisionBody, EventsPage, Health, Problem, ProviderConfigView,
    ProviderStats, ProviderView, Providers, ReviewerConfigView, RoleConfigView, RunList, StreamHeartbeat, StreamHello,
    StreamReset, ValidationError,
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
    pub taskd_version: String,
    /// ディスパッチャのスナップショットと同じ値。
    pub instance_id: String,
    /// RFC 3339。
    pub started_at: String,
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
            .field("taskd_version", &self.taskd_version)
            .field("instance_id", &self.instance_id)
            .field("started_at", &self.started_at)
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
