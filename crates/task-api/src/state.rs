//! `ApiState`: ハンドラが共有するもの一式（API 専用のストア接続、設定の写し、デーモンの `watch`、SSE と replay の状態、
//! プロバイダ集計）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use task_core::{AccountAdapter, SqliteStore, StoreOptions};
use task_ops::daemon::DaemonSnapshot;
use task_ops::view::ViewContext;
use tokio::sync::watch;

use crate::middleware::{allowed_host_list, token_digest};
use crate::problem::ApiProblem;
use crate::stats::StatsState;
use crate::types::ConfigView;
use crate::{
    ApiError, ApiSettings, MAX_STREAMS, STREAM_HEARTBEAT_INTERVAL, STREAM_POLL_INTERVAL, STREAM_RESET_THRESHOLD,
};

/// SSE の上限と間隔。既定は api.md §4 の定数。テストでは短くしてよい。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamTuning {
    pub max_streams: usize,
    pub poll_interval: Duration,
    pub heartbeat_interval: Duration,
    pub reset_threshold: u64,
}

impl Default for StreamTuning {
    fn default() -> Self {
        Self {
            max_streams: MAX_STREAMS,
            poll_interval: STREAM_POLL_INTERVAL,
            heartbeat_interval: STREAM_HEARTBEAT_INTERVAL,
            reset_threshold: STREAM_RESET_THRESHOLD,
        }
    }
}

/// API のハンドラが共有する状態（`Clone` は `Arc` の複製）。
#[derive(Clone)]
pub struct ApiState {
    pub(crate) inner: Arc<Inner>,
    pub(crate) tuning: StreamTuning,
}

pub(crate) struct Inner {
    pub(crate) store: Arc<SqliteStore>,
    pub(crate) token_digest: Option<[u8; 32]>,
    pub(crate) allowed_hosts: Vec<String>,
    pub(crate) journal_mode: String,
    pub(crate) busy_timeout_ms: u64,
    pub(crate) view: ViewContext,
    pub(crate) config_view: ConfigView,
    /// ADR-0016 M3: `POST /tasks` の省略値を埋める `[[roles]]`。
    pub(crate) roles: Vec<task_core::RoleSpec>,
    /// ADR-0027 D1: `POST /tasks` の `genre` の検証・既定解決に使う `[[genres]]`。
    pub(crate) genres: Vec<task_core::GenreSpec>,
    /// Phase 30（ADR-0033 D4 追記）: 対話は常にこの分野で走る（ノードの `genre` は使わない）。
    pub(crate) conversation_genre: String,
    pub(crate) celeris_version: String,
    pub(crate) providers_dir: Option<std::path::PathBuf>,
    pub(crate) admin_tx: Option<tokio::sync::mpsc::Sender<crate::admin::AdminRequest>>,
    pub(crate) accounts_roots: HashMap<AccountAdapter, std::path::PathBuf>,
    pub(crate) max_runs_per_account: usize,
    /// ADR-0030 D1: `[secrets] dir`。`None` なら管理系は 409 `secrets_unavailable`。
    pub(crate) secrets_dir: Option<std::path::PathBuf>,
    /// ADR-0030 D3: 秘密 id → `used_by`（celeris が設定から渡す）。
    pub(crate) secret_usage: HashMap<String, Vec<crate::types::SecretUse>>,
    /// ADR-0033 D6（GUI 監査対応 Phase 29）: `[memory] dir`。`None` なら `GET /org/{id}/memory` は 409。
    pub(crate) memory_dir: Option<std::path::PathBuf>,
    /// ADR-0037 D2（Phase 39）: `[notify] discord_webhook_secret`。
    pub(crate) notify_secret_id: String,
    /// ADR-0037 D3: `[notify] gui_base_url`。
    pub(crate) notify_gui_base_url: Option<String>,
    /// ADR-0040 D6（Phase 48）: `[selfdeploy] releases_dir` を読む係（celeris が渡す）。`None` なら
    /// `GET /releases` は空、`POST /releases/{sha12}/promote` は 409。
    pub(crate) releases: Option<crate::releases::SharedReleaseSource>,
    /// ADR-0040 D4（Phase 47）: `GET /health` の `release` / `mode` と、管理 API の 503 に使う役割。
    pub(crate) release: String,
    pub(crate) mode: task_core::DaemonMode,
    pub(crate) role: task_core::SharedRole,
    /// ADR-0043 D5（Phase 54）: `[github]`（`gh` の場所と merge の方法）。
    pub(crate) github: crate::GithubSettings,
    /// ADR-0044 D7（Phase 57）: 既定の文書リポジトリを作る場所の根（`~/workspace`）。
    pub(crate) docs_repo_root: Option<std::path::PathBuf>,
    /// ADR-0047 D1（Phase 61）: `[knowledge] root`。`None` なら `/knowledge/*` は 409。
    pub(crate) knowledge_root: Option<std::path::PathBuf>,
    pub(crate) account_stats: Mutex<crate::stats::AccountStatsState>,
    pub(crate) instance_id: String,
    pub(crate) started_at: String,
    pub(crate) daemon: watch::Receiver<Option<DaemonSnapshot>>,
    pub(crate) shutdown: watch::Sender<bool>,
    pub(crate) streams: AtomicUsize,
    pub(crate) stream_polls: AtomicU64,
    pub(crate) replay_running: AtomicBool,
    pub(crate) stats: Mutex<StatsState>,
    /// ADR-0033 D3: GUI が最後に通知した時刻（`POST /reports/notified`）。DB には列が無いので
    /// API プロセスのメモリに持つ観測値（再起動で消える）。
    pub(crate) last_notified_at: Mutex<Option<time::OffsetDateTime>>,
}

impl ApiState {
    /// API 専用の `SqliteStore` を開き（`open_with`）、`journal_mode` を実測して状態を作る。
    pub fn new(settings: ApiSettings, daemon: watch::Receiver<Option<DaemonSnapshot>>) -> Result<Self, ApiError> {
        let store = SqliteStore::open_with(
            &settings.db_path,
            StoreOptions {
                busy_timeout: settings.busy_timeout,
            },
        )?;
        let journal_mode = measure_journal_mode(&settings)?;
        let (shutdown, _) = watch::channel(false);
        let inner = Inner {
            store: Arc::new(store),
            token_digest: settings.token.as_deref().map(token_digest),
            allowed_hosts: allowed_host_list(settings.listen, &settings.allowed_hosts),
            journal_mode,
            busy_timeout_ms: u64::try_from(settings.busy_timeout.as_millis()).unwrap_or(u64::MAX),
            view: settings.view,
            config_view: settings.config_view,
            roles: settings.roles,
            genres: settings.genres,
            conversation_genre: settings.conversation_genre,
            celeris_version: settings.celeris_version,
            providers_dir: settings.providers_dir,
            admin_tx: settings.admin_tx,
            accounts_roots: settings.accounts_roots,
            max_runs_per_account: settings.max_runs_per_account,
            secrets_dir: settings.secrets_dir,
            secret_usage: settings.secret_usage,
            memory_dir: settings.memory_dir,
            notify_secret_id: settings.notify_secret_id,
            notify_gui_base_url: settings.notify_gui_base_url,
            releases: settings.releases,
            release: settings.release,
            mode: settings.mode,
            role: settings.role,
            github: settings.github,
            docs_repo_root: settings.docs_repo_root,
            knowledge_root: settings.knowledge_root,
            account_stats: Mutex::new(crate::stats::AccountStatsState::default()),
            instance_id: settings.instance_id,
            started_at: settings.started_at,
            daemon,
            shutdown,
            streams: AtomicUsize::new(0),
            stream_polls: AtomicU64::new(0),
            replay_running: AtomicBool::new(false),
            stats: Mutex::new(StatsState::default()),
            last_notified_at: Mutex::new(None),
        };
        Ok(Self {
            inner: Arc::new(inner),
            tuning: StreamTuning::default(),
        })
    }

    /// SSE の上限と間隔を差し替える（テスト用。本番は既定値のまま）。
    pub fn with_stream_tuning(mut self, tuning: StreamTuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// SSE の購読ループが `events_since` を呼んだ累計回数（購読解除でポーリングが止まることの確認用）。
    pub fn stream_poll_count(&self) -> u64 {
        self.inner.stream_polls.load(Ordering::SeqCst)
    }

    /// 現在開いている SSE 接続の数。
    pub fn active_streams(&self) -> usize {
        self.inner.streams.load(Ordering::SeqCst)
    }

    /// 全 SSE 接続を閉じる（celeris の停止時。`serve` は shutdown で呼ぶ）。以後の購読もすぐ閉じる。
    pub fn close_streams(&self) {
        self.inner.shutdown.send_replace(true);
    }

    pub(crate) fn snapshot(&self) -> Option<DaemonSnapshot> {
        self.inner.daemon.borrow().clone()
    }

    /// DB を使う同期処理を `spawn_blocking` で実行する。
    pub(crate) async fn blocking<T, F>(&self, f: F) -> Result<T, ApiProblem>
    where
        T: Send + 'static,
        F: FnOnce(&SqliteStore) -> Result<T, ApiProblem> + Send + 'static,
    {
        let store = Arc::clone(&self.inner.store);
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .map_err(|e| ApiProblem::internal(format!("blocking task failed: {e}")))?
    }

    /// `POST /replay` の同時実行を 1 つに制限する。実行中なら `None`。
    pub(crate) fn try_begin_replay(&self) -> Option<ReplayGuard> {
        self.inner
            .replay_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| ReplayGuard {
                inner: Arc::clone(&self.inner),
            })
    }

    /// SSE の接続枠を 1 つ取る。上限なら `None`。
    pub(crate) fn try_open_stream(&self) -> Option<StreamSlot> {
        let max = self.tuning.max_streams;
        self.inner
            .streams
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n < max).then_some(n + 1))
            .ok()
            .map(|_| StreamSlot {
                inner: Arc::clone(&self.inner),
            })
    }
}

pub(crate) struct ReplayGuard {
    inner: Arc<Inner>,
}

impl Drop for ReplayGuard {
    fn drop(&mut self) {
        self.inner.replay_running.store(false, Ordering::SeqCst);
    }
}

pub(crate) struct StreamSlot {
    inner: Arc<Inner>,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        self.inner.streams.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `PRAGMA journal_mode` の実測値（WAL はファイルに持続する設定なので、別接続で読んでも同じ値になる）。
fn measure_journal_mode(settings: &ApiSettings) -> Result<String, ApiError> {
    let conn = rusqlite::Connection::open(&settings.db_path)?;
    conn.busy_timeout(settings.busy_timeout)?;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    Ok(mode.to_ascii_lowercase())
}
