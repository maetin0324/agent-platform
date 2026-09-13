//! taskd: デーモン本体（DESIGN §3, §5.2, ADR-0005 D7）。設定読込、ログ初期化、tick ループ。
//! 判断ロジックは `task-dispatch` にあり、ここはループと配線だけ。

pub mod config;

use std::collections::HashMap;
use std::sync::Arc;

use task_core::{SqliteStore, StoreError, TaskStore};
use task_dispatch::{AdapterId, DispatchError, Dispatcher, StaticPolicy, TickReport};
use task_worker::{FakeAdapter, WorkerAdapter};

pub use config::{Config, ConfigError};

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("dispatch: {0}")]
    Dispatch(#[from] DispatchError),
}

/// ループの終了条件。
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    /// idle な tick で exit する（テスト・バッチ用）。
    pub until_idle: bool,
    /// tick 数の上限（0 = 無制限）。
    pub max_ticks: u64,
}

/// ループ終了の理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Idle,
    MaxTicks,
    Signal,
}

/// 設定から `Dispatcher` を組み立てる。
pub fn build_dispatcher(config: &Config) -> Result<Dispatcher, DaemonError> {
    let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open(&config.db)?);
    let specs = config.provider_specs();
    let models: HashMap<String, String> = specs.iter().map(|p| (p.id.clone(), p.model.clone())).collect();
    let policy = StaticPolicy::new(
        specs,
        std::time::Duration::from_secs(config.error_cooldown_secs),
    );
    let mut adapters: HashMap<AdapterId, Arc<dyn WorkerAdapter>> = HashMap::new();
    let mut fake = FakeAdapter::new(config.adapters.fake.command.clone());
    fake.set_env(
        config
            .adapters
            .fake
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    adapters.insert(FakeAdapter::ID.to_string(), Arc::new(fake));
    Ok(Dispatcher::new(
        store,
        Box::new(policy),
        models,
        adapters,
        config.dispatch_config(),
    ))
}

/// tick ループ。SIGINT/SIGTERM で停止する。
pub async fn run(config: Config, opts: RunOptions) -> Result<Exit, DaemonError> {
    let mut dispatcher = build_dispatcher(&config)?;
    let tick = config.tick();
    let mut ticks: u64 = 0;
    tracing::info!(db = %config.db.display(), workspace_root = %config.workspace_root.display(), max_concurrency = config.max_concurrency, tick_ms = config.tick_ms, "taskd started");

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    loop {
        let report: TickReport = dispatcher.tick()?;
        ticks += 1;
        if report.reclaimed + report.dispatched + report.finished + report.reviewed > 0 {
            tracing::info!(ticks, ?report, "tick");
        } else {
            tracing::debug!(ticks, ?report, "tick");
        }
        if opts.until_idle && report.idle {
            tracing::info!(ticks, "idle; exiting");
            return Ok(Exit::Idle);
        }
        if opts.max_ticks > 0 && ticks >= opts.max_ticks {
            tracing::info!(ticks, "max ticks reached; exiting");
            return Ok(Exit::MaxTicks);
        }
        let term = async {
            match sigterm.as_mut() {
                Some(s) => {
                    s.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(tick) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT; exiting");
                return Ok(Exit::Signal);
            }
            _ = term => {
                tracing::info!("SIGTERM; exiting");
                return Ok(Exit::Signal);
            }
        }
    }
}
