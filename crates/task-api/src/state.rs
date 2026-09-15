//! `ApiState`: ハンドラが共有するもの一式（API 専用のストア接続、設定の写し、デーモンの `watch`、SSE と replay の状態、
//! プロバイダ集計）。

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use task_core::{SqliteStore, StoreOptions};
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
    pub(crate) taskd_version: String,
    pub(crate) instance_id: String,
    pub(crate) started_at: String,
    pub(crate) daemon: watch::Receiver<Option<DaemonSnapshot>>,
    pub(crate) shutdown: watch::Sender<bool>,
    pub(crate) streams: AtomicUsize,
    pub(crate) stream_polls: AtomicU64,
    pub(crate) replay_running: AtomicBool,
    pub(crate) stats: Mutex<StatsState>,
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
            taskd_version: settings.taskd_version,
            instance_id: settings.instance_id,
            started_at: settings.started_at,
            daemon,
            shutdown,
            streams: AtomicUsize::new(0),
            stream_polls: AtomicU64::new(0),
            replay_running: AtomicBool::new(false),
            stats: Mutex::new(StatsState::default()),
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

    /// 全 SSE 接続を閉じる（taskd の停止時。`serve` は shutdown で呼ぶ）。以後の購読もすぐ閉じる。
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
