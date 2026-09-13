//! `taskd.toml`（DESIGN §2, ADR-0005 D7）。相対パス（`db`, `workspace_root`）は設定ファイルのある
//! ディレクトリからの相対と解釈する。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use task_core::Tier;
use task_dispatch::{DispatchConfig, ProviderSpec};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_db")]
    pub db: PathBuf,
    #[serde(default = "default_workspace_root")]
    pub workspace_root: PathBuf,
    #[serde(default = "default_tick_ms")]
    pub tick_ms: u64,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_lease_grace_secs")]
    pub lease_grace_secs: u64,
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_kill_grace_secs")]
    pub kill_grace_secs: u64,
    #[serde(default = "default_review_timeout_secs")]
    pub review_timeout_secs: u64,
    #[serde(default = "default_error_cooldown_secs")]
    pub error_cooldown_secs: u64,
    #[serde(default)]
    pub adapters: AdaptersConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptersConfig {
    #[serde(default)]
    pub fake: FakeConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FakeConfig {
    /// 起動するコマンド（省略時は `FakeAdapter::default_command()`）。
    #[serde(default)]
    pub command: Vec<String>,
    /// 追加の環境変数。
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub id: String,
    pub adapter: String,
    #[serde(default = "default_tiers")]
    pub tiers: Vec<Tier>,
    #[serde(default = "default_provider_concurrency")]
    pub concurrency: usize,
    #[serde(default)]
    pub model: String,
}

fn default_db() -> PathBuf {
    PathBuf::from("taskd.sqlite3")
}
fn default_workspace_root() -> PathBuf {
    PathBuf::from("workspaces")
}
fn default_tick_ms() -> u64 {
    2000
}
fn default_max_concurrency() -> usize {
    4
}
fn default_lease_grace_secs() -> u64 {
    60
}
fn default_idle_timeout_secs() -> u64 {
    300
}
fn default_kill_grace_secs() -> u64 {
    10
}
fn default_review_timeout_secs() -> u64 {
    600
}
fn default_error_cooldown_secs() -> u64 {
    300
}
fn default_tiers() -> Vec<Tier> {
    vec![Tier::Frontier, Tier::Standard, Tier::Cheap]
}
fn default_provider_concurrency() -> usize {
    1
}

impl Config {
    /// ファイルから読み、相対パスを設定ファイルのディレクトリ基準で絶対化する。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut cfg: Config = toml::from_str(&text)?;
        let base = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let base = base.canonicalize().unwrap_or(base);
        if cfg.db.is_relative() {
            cfg.db = base.join(&cfg.db);
        }
        if cfg.workspace_root.is_relative() {
            cfg.workspace_root = base.join(&cfg.workspace_root);
        }
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_concurrency == 0 {
            return Err(ConfigError::Invalid("max_concurrency must be >= 1".into()));
        }
        if self.tick_ms == 0 {
            return Err(ConfigError::Invalid("tick_ms must be >= 1".into()));
        }
        if self.providers.is_empty() {
            return Err(ConfigError::Invalid("at least one [[providers]] entry is required".into()));
        }
        for p in &self.providers {
            if p.adapter != task_worker::FakeAdapter::ID {
                return Err(ConfigError::Invalid(format!(
                    "provider {}: adapter {:?} is not available in this build (Phase 3: fake only)",
                    p.id, p.adapter
                )));
            }
            if p.concurrency == 0 {
                return Err(ConfigError::Invalid(format!("provider {}: concurrency must be >= 1", p.id)));
            }
        }
        Ok(())
    }

    pub fn tick(&self) -> Duration {
        Duration::from_millis(self.tick_ms)
    }

    pub fn dispatch_config(&self) -> DispatchConfig {
        DispatchConfig {
            max_concurrency: self.max_concurrency,
            lease_grace: Duration::from_secs(self.lease_grace_secs),
            idle_timeout: Duration::from_secs(self.idle_timeout_secs),
            kill_grace: Duration::from_secs(self.kill_grace_secs),
            review_timeout: Duration::from_secs(self.review_timeout_secs),
            workspace_root: self.workspace_root.clone(),
        }
    }

    pub fn provider_specs(&self) -> Vec<ProviderSpec> {
        self.providers
            .iter()
            .map(|p| ProviderSpec {
                id: p.id.clone(),
                adapter: p.adapter.clone(),
                tiers: p.tiers.clone(),
                concurrency: p.concurrency,
                model: p.model.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_example_config_and_resolves_relative_paths() {
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/taskd.example.toml"));
        let cfg = Config::load(path).unwrap();
        assert!(cfg.db.is_absolute());
        assert!(cfg.workspace_root.is_absolute());
        assert_eq!(cfg.max_concurrency, 2);
        assert_eq!(cfg.providers[0].adapter, "fake");
        assert_eq!(cfg.tick(), Duration::from_millis(2000));
        assert_eq!(cfg.provider_specs()[0].concurrency, 2);
    }

    #[test]
    fn rejects_unknown_adapter_and_missing_providers() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(matches!(cfg.validate(), Err(ConfigError::Invalid(_))));
        let cfg: Config = toml::from_str("[[providers]]\nid = \"x\"\nadapter = \"claude-code\"\n").unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("claude-code"));
        assert!(toml::from_str::<Config>("bogus = 1\n").is_err());
    }
}
