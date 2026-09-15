//! プロバイダ（アカウント）の管理（ADR-0017）。`create`/`patch`/`delete` は `providers.d/<id>.toml` への
//! ファイル読み書きだけで完結する（LLM もワーカーも起動しない、DESIGN §5.10 の境界を守る）。`reload`/`check` は
//! 稼働中の `Dispatcher` の差し替え・実際のワーカー起動が要るので、taskd（task-worker/task-dispatch に依存する側）
//! へ `AdminRequest` で委譲する（ADR-0017 M2）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::Tier;
use tokio::sync::oneshot;

use crate::types::ProviderConfigView;

/// task-api → taskd（`ApiSettings.admin_tx` 経由）。稼働中の `Dispatcher`・ワーカーの起動が要る操作だけを運ぶ。
pub enum AdminRequest {
    /// 設定ファイルと `providers.d/` を読み直し、稼働中のプロバイダ選定・アダプタ一式を差し替える。
    Reload { reply: oneshot::Sender<Result<(), String>> },
    /// 1 アカウントだけ短い疎通確認を行う（タスク・イベントには残さない。ADR-0017 D2）。
    Check {
        provider_id: String,
        reply: oneshot::Sender<Result<ProviderCheckResult, CheckError>>,
    },
}

/// `POST /api/v1/providers/{id}/check` の結果（ADR-0017 D2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCheckResult {
    Ok,
    AuthFailed,
    Throttled,
    SpawnFailed,
}

/// `check` が完了できなかった理由（taskd 側の都合。ハンドラがこれを HTTP へ写す）。
#[derive(Debug, Clone)]
pub enum CheckError {
    /// 指定した id が現在の設定（`[[providers]]` + `providers.d/`）に無い。
    NotFound,
    /// 設定の再読込に失敗した（`Config::load` のエラー文言）。
    ConfigInvalid(String),
    /// 受け取り側（taskd）に届かなかった（チャネルが閉じている・タイムアウト）。
    Unavailable(String),
}

/// `providers.d/<id>.toml` の中身（`taskd::config::ProviderConfig` と同じ形。ADR-0017 M1）。
/// task-api は taskd に依存できない（循環依存になる）ので、独立に同じ形の型を持つ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfigFile {
    pub id: String,
    pub adapter: String,
    #[serde(default = "default_tiers")]
    pub tiers: Vec<Tier>,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl ProviderConfigFile {
    pub fn to_view(&self) -> ProviderConfigView {
        let mut env_keys: Vec<String> = self.env.keys().cloned().collect();
        env_keys.sort();
        ProviderConfigView {
            id: self.id.clone(),
            adapter: self.adapter.clone(),
            tiers: self.tiers.clone(),
            concurrency: self.concurrency,
            model: (!self.model.is_empty()).then(|| self.model.clone()),
            env_keys,
        }
    }
}

fn default_tiers() -> Vec<Tier> {
    vec![Tier::Frontier, Tier::Standard, Tier::Cheap]
}

fn default_concurrency() -> usize {
    1
}

/// `POST /api/v1/providers` の本文。
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderCreateBody {
    pub id: String,
    pub adapter: String,
    #[serde(default)]
    pub tiers: Option<Vec<Tier>>,
    #[serde(default)]
    pub concurrency: Option<usize>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl ProviderCreateBody {
    pub fn into_file(self) -> ProviderConfigFile {
        ProviderConfigFile {
            id: self.id,
            adapter: self.adapter,
            tiers: self.tiers.unwrap_or_else(default_tiers),
            concurrency: self.concurrency.unwrap_or_else(default_concurrency),
            model: self.model.unwrap_or_default(),
            env: self.env,
        }
    }
}

/// `PATCH /api/v1/providers/{id}` の本文。`id`/`adapter` は変更できない（ADR-0017 D1: 並列度・tier・model。
/// `env` も自然な拡張として一緒に patch できるようにした）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProviderPatchBody {
    #[serde(default)]
    pub tiers: Option<Vec<Tier>>,
    #[serde(default)]
    pub concurrency: Option<usize>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

impl ProviderPatchBody {
    pub fn apply(&self, mut file: ProviderConfigFile) -> ProviderConfigFile {
        if let Some(tiers) = &self.tiers {
            file.tiers = tiers.clone();
        }
        if let Some(concurrency) = self.concurrency {
            file.concurrency = concurrency;
        }
        if let Some(model) = &self.model {
            file.model = model.clone();
        }
        if let Some(env) = &self.env {
            file.env = env.clone();
        }
        file
    }
}

pub const KNOWN_ADAPTERS: [&str; 3] = ["fake", "claude-code", "codex"];

/// ファイル名に安全に使える id か（`providers.d/<id>.toml` のパストラバーサル防止）。
pub fn valid_provider_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn valid_adapter(adapter: &str) -> bool {
    KNOWN_ADAPTERS.contains(&adapter)
}

pub fn provider_file_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.toml"))
}

#[derive(Debug, thiserror::Error)]
pub enum AdminFileError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to encode {path}: {source}")]
    Encode {
        path: PathBuf,
        #[source]
        source: toml::ser::Error,
    },
}

pub fn read_provider_file(path: &Path) -> Result<ProviderConfigFile, AdminFileError> {
    let text =
        std::fs::read_to_string(path).map_err(|source| AdminFileError::Read { path: path.to_path_buf(), source })?;
    toml::from_str(&text).map_err(|source| AdminFileError::Parse { path: path.to_path_buf(), source })
}

pub fn write_provider_file(dir: &Path, provider: &ProviderConfigFile) -> Result<(), AdminFileError> {
    std::fs::create_dir_all(dir).map_err(|source| AdminFileError::Write { path: dir.to_path_buf(), source })?;
    let path = provider_file_path(dir, &provider.id);
    let text =
        toml::to_string_pretty(provider).map_err(|source| AdminFileError::Encode { path: path.clone(), source })?;
    std::fs::write(&path, text).map_err(|source| AdminFileError::Write { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_reject_path_traversal_and_empty() {
        assert!(valid_provider_id("acct-b"));
        assert!(valid_provider_id("acct_2"));
        assert!(!valid_provider_id(""));
        assert!(!valid_provider_id("../escape"));
        assert!(!valid_provider_id("a/b"));
        assert!(!valid_provider_id(&"x".repeat(65)));
    }

    #[test]
    fn create_body_fills_defaults_like_provider_config() {
        let body = ProviderCreateBody {
            id: "acct-b".into(),
            adapter: "fake".into(),
            tiers: None,
            concurrency: None,
            model: None,
            env: HashMap::new(),
        };
        let file = body.into_file();
        assert_eq!(file.tiers, default_tiers());
        assert_eq!(file.concurrency, 1);
        assert_eq!(file.model, "");
    }

    #[test]
    fn patch_only_overwrites_provided_fields() {
        let file = ProviderConfigFile {
            id: "acct-b".into(),
            adapter: "fake".into(),
            tiers: vec![Tier::Standard],
            concurrency: 2,
            model: "m1".into(),
            env: HashMap::from([("K".to_string(), "v".to_string())]),
        };
        let patch = ProviderPatchBody {
            concurrency: Some(5),
            ..Default::default()
        };
        let patched = patch.apply(file);
        assert_eq!(patched.concurrency, 5);
        assert_eq!(patched.tiers, vec![Tier::Standard]);
        assert_eq!(patched.model, "m1");
        assert_eq!(patched.env.get("K"), Some(&"v".to_string()));
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let file = ProviderConfigFile {
            id: "acct-b".into(),
            adapter: "claude-code".into(),
            tiers: vec![Tier::Frontier],
            concurrency: 3,
            model: "".into(),
            env: HashMap::from([("CLAUDE_CONFIG_DIR".to_string(), "/x".to_string())]),
        };
        write_provider_file(dir.path(), &file).unwrap_or_else(|e| panic!("write: {e}"));
        let read = read_provider_file(&provider_file_path(dir.path(), "acct-b")).unwrap_or_else(|e| panic!("read: {e}"));
        assert_eq!(read.id, "acct-b");
        assert_eq!(read.concurrency, 3);
        assert_eq!(read.env.get("CLAUDE_CONFIG_DIR"), Some(&"/x".to_string()));
    }
}
