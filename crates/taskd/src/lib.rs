//! taskd: デーモン本体（DESIGN §3, §5.2, ADR-0005 D7）。設定読込、ログ初期化、tick ループ。
//! 判断ロジックは `task-dispatch` にあり、ここはループと配線だけ。

pub mod config;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use task_core::{SqliteStore, StoreError, TaskStore};
use task_dispatch::{DispatchError, Dispatcher, ProviderId, StaticPolicy, TickReport};
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
    }
}
