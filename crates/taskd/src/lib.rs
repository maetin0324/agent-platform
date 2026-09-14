//! taskd: デーモン本体（DESIGN §3, §5.2, ADR-0005 D7）。設定読込、ログ初期化、tick ループ。
//! 判断ロジックは `task-dispatch` にあり、ここはループと配線だけ。

pub mod config;

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use task_api::types::{ApiConfigView, ConfigView, ProviderConfigView, ReviewerConfigView};
use task_api::{ApiError, ApiSettings, ApiState};
use task_core::{SqliteStore, StoreError, StoreOptions, TaskStore};
use task_ops::view::ViewContext;
use task_dispatch::{DispatchError, Dispatcher, ProviderId, SnapshotPublisher, StaticPolicy, TickReport};
use task_ops::daemon::ProviderLive;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use task_worker::{ClaudeCodeAdapter, ClaudeCodeConfig, CodexAdapter, CodexConfig, FakeAdapter, WorkerAdapter};

pub use config::{Config, ConfigError};

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("dispatch: {0}")]
    Dispatch(#[from] DispatchError),
    #[error("api: {0}")]
    Api(#[from] ApiError),
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

/// `[adapters.<種別>].env` にプロバイダの `env` を重ねる（同名キーはプロバイダが優先。順序は決定的）。
fn merged_env(base: &HashMap<String, String>, provider: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut merged: BTreeMap<String, String> = base.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    merged.extend(provider.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged.into_iter().collect()
}

/// プロバイダの `model` が空でなければそれ、空なら `[adapters.<種別>].model`（ADR-0012 D1）。
fn effective_model(provider_model: &str, adapter_model: &Option<String>) -> Option<String> {
    if provider_model.is_empty() {
        adapter_model.clone()
    } else {
        Some(provider_model.to_string())
    }
}

/// ADR-0012 D1: `[[providers]]` の各行（= 1 アカウント）ごとにアダプタのインスタンスを作る。`[adapters.<種別>]` を基本設定とし、
/// プロバイダの `env` と `model` を重ねる。キーはプロバイダ ID。
pub fn build_adapters(config: &Config) -> HashMap<ProviderId, Arc<dyn WorkerAdapter>> {
    let mut adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>> = HashMap::new();
    for p in &config.providers {
        let adapter: Arc<dyn WorkerAdapter> = match p.adapter.as_str() {
            ClaudeCodeAdapter::ID => {
                let base = &config.adapters.claude_code;
                Arc::new(ClaudeCodeAdapter::new(ClaudeCodeConfig {
                    command: base.command.clone(),
                    extra_args: base.extra_args.clone(),
                    permission_mode: base.permission_mode.clone(),
                    model: effective_model(&p.model, &base.model),
                    env: merged_env(&base.env, &p.env),
                }))
            }
            CodexAdapter::ID => {
                let base = &config.adapters.codex;
                Arc::new(CodexAdapter::new(CodexConfig {
                    command: base.command.clone(),
                    extra_args: base.extra_args.clone(),
                    model: effective_model(&p.model, &base.model),
                    env: merged_env(&base.env, &p.env),
                }))
            }
            // `Config::validate` が fake / claude-code / codex 以外を拒否している。
            _ => {
                let mut fake = FakeAdapter::new(config.adapters.fake.command.clone());
                fake.set_env(merged_env(&config.adapters.fake.env, &p.env));
                Arc::new(fake)
            }
        };
        adapters.insert(p.id.clone(), adapter);
    }
    adapters
}

/// `WorkerStarted.model` に記録する、プロバイダごとの実効モデル名（ADR-0012 D1）。
pub fn effective_models(config: &Config) -> HashMap<ProviderId, String> {
    config
        .providers
        .iter()
        .map(|p| {
            let adapter_model = match p.adapter.as_str() {
                ClaudeCodeAdapter::ID => config.adapters.claude_code.model.clone(),
                CodexAdapter::ID => config.adapters.codex.model.clone(),
                _ => None,
            };
            (p.id.clone(), effective_model(&p.model, &adapter_model).unwrap_or_default())
        })
        .collect()
}

/// ADR-0013 D4: デーモンのスナップショットに載せる `[[providers]]` の定義（`in_use` はディスパッチャが毎 tick 埋める）。
pub fn provider_lives(config: &Config) -> Vec<ProviderLive> {
    let models = effective_models(config);
    config
        .providers
        .iter()
        .map(|p| ProviderLive {
            id: p.id.clone(),
            adapter: p.adapter.clone(),
            tiers: p.tiers.clone(),
            concurrency: p.concurrency,
            model: models.get(&p.id).filter(|m| !m.is_empty()).cloned(),
            in_use: 0,
        })
        .collect()
}

/// スナップショットの `hostname`: `/proc/sys/kernel/hostname`（Linux）→ `HOSTNAME` → `"unknown"`。
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "unknown".to_string())
}

/// 設定から `Dispatcher` を組み立てる。
pub fn build_dispatcher(config: &Config) -> Result<Dispatcher, DaemonError> {
    let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open(&config.db)?);
    let policy = StaticPolicy::new(
        config.provider_specs(),
        std::time::Duration::from_secs(config.error_cooldown_secs),
    );
    Ok(Dispatcher::new(
        store,
        Box::new(policy),
        effective_models(config),
        build_adapters(config),
        config.dispatch_config(),
    ))
}

/// ADR-0013 / `docs/gui/api.md` §3.21: `GET /api/v1/config` に出す設定の要約。env は**キー名だけ**、トークンとその場所は出さない。
pub fn config_view(config: &Config, listen: SocketAddr) -> ConfigView {
    let models = effective_models(config);
    ConfigView {
        config_path: config.source_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
        db: config.db.display().to_string(),
        workspace_root: config.workspace_root.display().to_string(),
        tick_ms: config.tick_ms,
        max_concurrency: config.max_concurrency,
        lease_grace_secs: config.lease_grace_secs,
        idle_timeout_secs: config.idle_timeout_secs,
        kill_grace_secs: config.kill_grace_secs,
        review_timeout_secs: config.review_timeout_secs,
        error_cooldown_secs: config.error_cooldown_secs,
        retry_backoff_base_secs: config.retry_backoff_base_secs,
        retry_backoff_max_secs: config.retry_backoff_max_secs,
        max_requeues: config.max_requeues,
        plan_auto_accept: config.plan.auto_accept,
        reviewer: ReviewerConfigView { adapter: config.reviewer.adapter.clone(), tier: config.reviewer.tier },
        providers: config
            .providers
            .iter()
            .map(|p| {
                let mut env_keys: Vec<String> = p.env.keys().cloned().collect();
                env_keys.sort();
                ProviderConfigView {
                    id: p.id.clone(),
                    adapter: p.adapter.clone(),
                    tiers: p.tiers.clone(),
                    concurrency: p.concurrency,
                    model: models.get(&p.id).filter(|m| !m.is_empty()).cloned(),
                    env_keys,
                }
            })
            .collect(),
        api: ApiConfigView {
            bind: listen.to_string(),
            auth_required: config.api.token_file.is_some(),
            allowed_hosts: config.api.allowed_hosts.clone(),
        },
    }
}

/// `task_api::ApiSettings` を設定から作る。`instance_id` / `started_at` はディスパッチャのスナップショットと同じ値を渡す。
pub fn api_settings(
    config: &Config,
    listen: SocketAddr,
    token: Option<String>,
    instance_id: String,
    started_at: String,
) -> ApiSettings {
    ApiSettings {
        listen,
        token,
        allowed_hosts: config.api.allowed_hosts.clone(),
        db_path: config.db.clone(),
        busy_timeout: StoreOptions::default().busy_timeout,
        view: ViewContext {
            workspace_root: config.workspace_root.clone(),
            retry_backoff_base: Duration::from_secs(config.retry_backoff_base_secs),
            retry_backoff_max: Duration::from_secs(config.retry_backoff_max_secs),
            max_requeues: config.max_requeues,
        },
        config_view: config_view(config, listen),
        taskd_version: env!("CARGO_PKG_VERSION").to_string(),
        instance_id,
        started_at,
    }
}

/// 動いている API サーバ。`stop` で graceful に止める。
struct RunningApi {
    stop: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<Result<(), ApiError>>,
}

impl RunningApi {
    async fn stop(self) {
        let _ = self.stop.send(());
        match tokio::time::timeout(Duration::from_secs(5), self.handle).await {
            Ok(Ok(Ok(()))) => tracing::info!("api stopped"),
            Ok(Ok(Err(e))) => tracing::error!(error = %e, "api server failed"),
            Ok(Err(e)) => tracing::error!(error = %e, "api task panicked"),
            Err(_) => tracing::warn!("api did not stop within 5s"),
        }
    }
}

/// ADR-0013 D3 / D4: ディスパッチャにスナップショットの送り口を付け、API 専用の DB 接続を開いて bind する。
/// 開けない・bind できないときは起動を失敗させる（黙って API 無しで動かない）。
async fn start_api(config: &Config, listen: SocketAddr, dispatcher: &mut Dispatcher) -> Result<RunningApi, DaemonError> {
    let token = config.api.read_token()?;
    let instance_id = ulid::Ulid::new().to_string();
    let started_at = OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_default();
    let (tx, rx) = tokio::sync::watch::channel(None);
    dispatcher.set_snapshot_publisher(SnapshotPublisher {
        tx,
        instance_id: instance_id.clone(),
        hostname: hostname(),
        started_at: started_at.clone(),
        tick_ms: config.tick_ms,
        providers: provider_lives(config),
    });
    let settings = api_settings(config, listen, token, instance_id, started_at);
    let state = tokio::task::spawn_blocking(move || ApiState::new(settings, rx))
        .await
        .map_err(|e| ApiError::Startup(e.to_string()))??;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|source| ApiError::Bind { addr: listen, source })?;
    let addr = listener.local_addr().unwrap_or(listen);
    tracing::info!(%addr, auth_required = config.api.token_file.is_some(), "api listening");
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(task_api::serve_with_listener(listener, state, async move {
        let _ = stop_rx.await;
    }));
    Ok(RunningApi { stop, handle })
}

/// デーモン本体。`[api]` があれば同じランタイムで HTTP API も動かし、tick ループの終了時に止める。
pub async fn run(config: Config, opts: RunOptions) -> Result<Exit, DaemonError> {
    let mut dispatcher = build_dispatcher(&config)?;
    let api = match config.api.listen {
        Some(listen) => Some(start_api(&config, listen, &mut dispatcher).await?),
        None => None,
    };
    let result = tick_loop(&mut dispatcher, &config, opts).await;
    if let Some(api) = api {
        api.stop().await;
    }
    result
}

/// tick ループ。SIGINT/SIGTERM で停止する。
async fn tick_loop(dispatcher: &mut Dispatcher, config: &Config, opts: RunOptions) -> Result<Exit, DaemonError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0012 D1: 同じ claude-code を使う 2 アカウントが、それぞれの env と model を持つアダプタになる。
    #[test]
    fn build_adapters_creates_one_adapter_per_provider_with_merged_env_and_model() {
        let text = r#"
[adapters.claude_code]
model = "adapter-default-model"
env = { SHARED = "base", CLAUDE_CONFIG_DIR = "/base" }

[[providers]]
id = "acct-a"
adapter = "claude-code"
model = "model-a"
env = { CLAUDE_CONFIG_DIR = "/accounts/a" }

[[providers]]
id = "acct-b"
adapter = "claude-code"
env = { CLAUDE_CONFIG_DIR = "/accounts/b" }

[[providers]]
id = "local-fake"
adapter = "fake"
model = "fake"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.validate().unwrap();
        let adapters = build_adapters(&cfg);
        assert_eq!(adapters.len(), 3);
        assert_eq!(adapters["acct-a"].id(), "claude-code");
        assert_eq!(adapters["local-fake"].id(), "fake");

        let a = merged_env(&cfg.adapters.claude_code.env, &cfg.providers[0].env);
        assert_eq!(a, vec![("CLAUDE_CONFIG_DIR".into(), "/accounts/a".into()), ("SHARED".into(), "base".into())]);
        let b = merged_env(&cfg.adapters.claude_code.env, &cfg.providers[1].env);
        assert!(b.contains(&("CLAUDE_CONFIG_DIR".into(), "/accounts/b".into())));

        let models = effective_models(&cfg);
        assert_eq!(models["acct-a"], "model-a");
        assert_eq!(models["acct-b"], "adapter-default-model");
        assert_eq!(models["local-fake"], "fake");

        // ADR-0013 D4: スナップショットの定義部分は設定の順・実効モデル（env は載せない）。
        let lives = provider_lives(&cfg);
        let ids: Vec<&str> = lives.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["acct-a", "acct-b", "local-fake"]);
        assert_eq!(lives[1].model.as_deref(), Some("adapter-default-model"));
        assert_eq!((lives[0].adapter.as_str(), lives[0].concurrency, lives[0].in_use), ("claude-code", 1, 0));
        assert!(!hostname().is_empty());

        // ADR-0013 D11: /config の要約には env のキー名だけが載り、値は載らない。
        let view = config_view(&cfg, "127.0.0.1:7710".parse().unwrap());
        assert_eq!(view.providers[0].env_keys, ["CLAUDE_CONFIG_DIR"]);
        assert_eq!(view.providers[1].model.as_deref(), Some("adapter-default-model"));
        assert_eq!((view.api.bind.as_str(), view.api.auth_required), ("127.0.0.1:7710", false));
        let json = serde_json::to_string(&view).unwrap();
        for secret in ["/accounts/a", "/accounts/b", "/base", "base\""] {
            assert!(!json.contains(secret), "{secret} leaked: {json}");
        }
    }
}
