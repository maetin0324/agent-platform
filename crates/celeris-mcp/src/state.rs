//! `McpState`: ハンドラが共有するもの一式（ADR-0056 D5）。`task-api::ApiState` と同じ規律
//! （専用の `SqliteStore` 接続、`spawn_blocking` で同期の store 呼び出しを逃がす）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use task_core::{RoleSpec, SqliteStore, StoreOptions};

use crate::ratelimit::RateLimiter;

/// `McpState::new` の失敗。
#[derive(Debug, thiserror::Error)]
pub enum McpStateError {
    #[error("could not open the store: {0}")]
    Store(#[from] task_core::StoreError),
}

pub struct McpState {
    pub(crate) store: Arc<SqliteStore>,
    pub(crate) rate_limit_per_min: u32,
    pub(crate) limiter: Mutex<RateLimiter>,
    pub(crate) roles: Vec<RoleSpec>,
    pub(crate) genres: Vec<task_core::GenreSpec>,
    pub(crate) conversation_genre: String,
    pub(crate) knowledge_root: Option<PathBuf>,
    /// MCP セッション（`initialize` で発行）。`session_id -> client_id`。
    pub(crate) sessions: Mutex<HashMap<String, String>>,
}

impl McpState {
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        db_path: &std::path::Path,
        busy_timeout: std::time::Duration,
        rate_limit_per_min: u32,
        roles: Vec<RoleSpec>,
        genres: Vec<task_core::GenreSpec>,
        conversation_genre: String,
        knowledge_root: Option<PathBuf>,
        // ADR-0064 D5: `true` なら `wal_autocheckpoint=0`（celeris の背景チェックポイント tick が
        // 別に打つ前提。デーモンの本番経路だけ `true`）。
        background_checkpoint: bool,
    ) -> Result<Arc<Self>, McpStateError> {
        let store = SqliteStore::open_with(
            db_path,
            StoreOptions {
                busy_timeout,
                background_checkpoint,
                ..StoreOptions::default()
            },
        )?;
        Ok(Arc::new(Self {
            store: Arc::new(store),
            rate_limit_per_min,
            limiter: Mutex::new(RateLimiter::default()),
            roles,
            genres,
            conversation_genre,
            knowledge_root,
            sessions: Mutex::new(HashMap::new()),
        }))
    }

    /// テスト用: 開いてある `SqliteStore` をそのまま共有する（in-memory の店をテストごとに開き直さない
    /// ため）。
    #[allow(clippy::too_many_arguments)]
    pub fn from_store(
        store: Arc<SqliteStore>,
        rate_limit_per_min: u32,
        roles: Vec<RoleSpec>,
        genres: Vec<task_core::GenreSpec>,
        conversation_genre: String,
        knowledge_root: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            rate_limit_per_min,
            limiter: Mutex::new(RateLimiter::default()),
            roles,
            genres,
            conversation_genre,
            knowledge_root,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    /// 同期の store 呼び出しをブロッキングスレッドへ逃がす（`task_api::ApiState::blocking` と同じ形）。
    pub(crate) async fn blocking<T, F>(&self, f: F) -> T
    where
        T: Send + 'static,
        F: FnOnce(&SqliteStore) -> T + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        match tokio::task::spawn_blocking(move || f(&store)).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "celeris-mcp: blocking task panicked");
                // 呼び出し側は `T` を返せる必要があるため、panic はここでは起こさず伝播させる。
                std::panic::resume_unwind(e.into_panic())
            }
        }
    }

    /// `initialize` で新しいセッションを発行する。
    pub(crate) fn create_session(&self, client_id: &str) -> String {
        let id = ulid::Ulid::new().to_string();
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), client_id.to_string());
        id
    }

    /// そのセッションがこのクライアントのものか（`initialize` 後の全メソッドで要る）。
    pub(crate) fn session_client(&self, session_id: &str) -> Option<String> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }
}
