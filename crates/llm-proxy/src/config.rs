//! `[llm_proxy]`（ADR-0053 D1/D2。Phase 65）。celeris の `Config` に埋め込まれる設定の型。
//!
//! ここは値の入れ物だけで、判断は持たない（DESIGN 原則 1 と同じ規律をこのクレートにも適用する）。

use std::collections::HashMap;
use std::net::SocketAddr;

use serde::Deserialize;
use task_core::Tier;

/// `[llm_proxy]`。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LlmProxyConfig {
    /// 省略時は「どれかの source が設定されていれば有効」（`LlmProxyConfig::effective_enabled` が判定する）。
    #[serde(default)]
    pub enabled: Option<bool>,
    /// loopback のみで bind する（ADR-0053 D1）。既定 `127.0.0.1:18100`。
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// `celeris/<tier>` の選択で無料の到達可能な source を最優先するか。
    #[serde(default = "default_prefer_free")]
    pub prefer_free: bool,
    /// 到達性 probe（`GET <base_url>/models`）のキャッシュ寿命（秒）。
    #[serde(default = "default_probe_cache_secs")]
    pub probe_cache_secs: u64,
    /// 1 アカウントに同時に流してよいプロキシ要求数（ADR-0024 D3 の `IN_USE_PENALTY` を使うための
    /// `max_runs_per_account` 相当。CLI ワーカーの `[accounts] max_runs_per_account` とは別枠で数える
    /// （`docs/llm-source.md` に明記。値そのものは共有しないが、cooldown・観測値は同じ帳簿を共有する）。
    #[serde(default = "default_max_concurrent_per_account")]
    pub max_concurrent_per_account: usize,
    /// 429/401 で cooldown を付けるときの既定の秒数（観測から更に長い期限が分かればそちらを使う。
    /// `task_dispatch::accounts::cooldown_for_failure` の `fallback_secs`）。
    #[serde(default = "default_cooldown_fallback_secs")]
    pub cooldown_fallback_secs: u64,
    #[serde(default)]
    pub sources: SourcesConfig,
    #[serde(default)]
    pub models: ModelsConfig,
}

impl Default for LlmProxyConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            listen: default_listen(),
            prefer_free: default_prefer_free(),
            probe_cache_secs: default_probe_cache_secs(),
            max_concurrent_per_account: default_max_concurrent_per_account(),
            cooldown_fallback_secs: default_cooldown_fallback_secs(),
            sources: SourcesConfig::default(),
            models: ModelsConfig::default(),
        }
    }
}

impl LlmProxyConfig {
    /// `enabled` が明示されていればその値、省略時は「どれかの source がある」かどうか。
    pub fn effective_enabled(&self) -> bool {
        self.enabled.unwrap_or_else(|| {
            self.sources.claude_oauth.is_some()
                || self.sources.codex_oauth.is_some()
                || !self.sources.openai_compatible.is_empty()
        })
    }
}

fn default_listen() -> SocketAddr {
    "127.0.0.1:18100".parse().unwrap_or_else(|e| unreachable!("hardcoded address: {e}"))
}
fn default_prefer_free() -> bool {
    true
}
fn default_probe_cache_secs() -> u64 {
    60
}
fn default_max_concurrent_per_account() -> usize {
    4
}
fn default_cooldown_fallback_secs() -> u64 {
    300
}

/// `[llm_proxy.sources]`。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourcesConfig {
    /// `[llm_proxy.sources.claude_oauth]`。無ければ `claude-oauth` は無効
    /// （`claude/<tier>` は 422、`celeris/<tier>` の候補にも入らない）。
    #[serde(default)]
    pub claude_oauth: Option<ClaudeOauthConfig>,
    /// `[llm_proxy.sources.codex_oauth]`。無ければ `codex-oauth` は無効。
    #[serde(default)]
    pub codex_oauth: Option<CodexOauthConfig>,
    /// `[[llm_proxy.sources.openai_compatible]]`。0 件でもよい（`qwen/<tier>` は 422 になる）。
    #[serde(default)]
    pub openai_compatible: Vec<OpenAiCompatibleConfig>,
}

/// `[llm_proxy.sources.claude_oauth]`（ADR-0053 D1-1）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClaudeOauthConfig {
    /// `[accounts] claude_dir` を再利用する（celeris が `Config::load` でここへ絶対パスを埋める。
    /// 空のままなら `[accounts] claude_dir` が無いということで、`effective_enabled` な状態では
    /// 設定エラーになる。`docs/llm-source.md` 参照）。
    #[serde(default)]
    pub accounts_dir: std::path::PathBuf,
    #[serde(default = "default_claude_base_url")]
    pub base_url: String,
    /// OAuth のトークン更新エンドポイント（テストは偽の上流に向ける）。
    #[serde(default = "default_claude_token_url")]
    pub token_url: String,
    /// Claude Code CLI と同じ client_id（未確認。`docs/llm-source.md` に注記）。
    #[serde(default = "default_claude_client_id")]
    pub client_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_claude_base_url() -> String {
    "https://api.anthropic.com".to_string()
}
fn default_claude_token_url() -> String {
    "https://console.anthropic.com/v1/oauth/token".to_string()
}
fn default_claude_client_id() -> String {
    "9d1c250a-e61b-44d9-88ed-5944d1962f5e".to_string()
}

/// `[llm_proxy.sources.codex_oauth]`（ADR-0053 D1-2）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CodexOauthConfig {
    /// `[accounts] codex_dir` を再利用する（celeris が `Config::load` で埋める。上と同じ規律）。
    #[serde(default)]
    pub accounts_dir: std::path::PathBuf,
    #[serde(default = "default_codex_responses_url")]
    pub responses_url: String,
    #[serde(default = "default_codex_token_url")]
    pub token_url: String,
    #[serde(default = "default_codex_client_id")]
    pub client_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_codex_responses_url() -> String {
    "https://chatgpt.com/backend-api/codex/responses".to_string()
}
fn default_codex_token_url() -> String {
    "https://auth.openai.com/oauth/token".to_string()
}
fn default_codex_client_id() -> String {
    "app_EMoamEEZ73f0CkXaXp7hrann".to_string()
}

/// `[[llm_proxy.sources.openai_compatible]]`（ADR-0053 D1-3）。既存の Qwen 等をそのまま中継する。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OpenAiCompatibleConfig {
    /// `qwen:<model>` の接頭辞や `x-celeris-source` に出る id（設定順が「同点は設定順」の基準）。
    pub id: String,
    pub base_url: String,
    /// `[secrets]` の解決は celeris 側で行い、平文の値をここに渡す（無ければ `Authorization` を付けない）。
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// `[llm_proxy.models]`: tier → 供給元ごとの実モデル ID（ADR-0053 D1）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    #[serde(default = "default_claude_models")]
    pub claude: HashMap<Tier, String>,
    #[serde(default = "default_gpt_models")]
    pub gpt: HashMap<Tier, String>,
    #[serde(default = "default_qwen_models")]
    pub qwen: HashMap<Tier, String>,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            claude: default_claude_models(),
            gpt: default_gpt_models(),
            qwen: default_qwen_models(),
        }
    }
}

/// **未確認**（`docs/llm-source.md` 参照。既存の設定例と同じ命名慣習に沿った既定値で、運用前に
/// `[llm_proxy.models]` で確認・上書きすること。`config/celeris.model-tiers.example.toml` が
/// 同種のモデル ID を「実行モデルID未確認」と明記しているのと同じ注意)。
fn default_claude_models() -> HashMap<Tier, String> {
    HashMap::from([
        (Tier::Frontier, "claude-opus-4-5".to_string()),
        (Tier::Standard, "claude-sonnet-5".to_string()),
        (Tier::Cheap, "claude-haiku-4-5".to_string()),
    ])
}

fn default_gpt_models() -> HashMap<Tier, String> {
    HashMap::from([
        (Tier::Frontier, "gpt-5-codex".to_string()),
        (Tier::Standard, "gpt-5".to_string()),
        (Tier::Cheap, "gpt-5-mini".to_string()),
    ])
}

/// ADR-0053 D1: Qwen は tier に関わらず `qwen3.8-27b`。
fn default_qwen_models() -> HashMap<Tier, String> {
    let model = "qwen3.8-27b".to_string();
    HashMap::from([
        (Tier::Frontier, model.clone()),
        (Tier::Standard, model.clone()),
        (Tier::Cheap, model),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_enabled_is_false_without_any_source() {
        let cfg = LlmProxyConfig::default();
        assert!(!cfg.effective_enabled());
    }

    #[test]
    fn effective_enabled_is_true_with_a_relay_source() {
        let mut cfg = LlmProxyConfig::default();
        cfg.sources.openai_compatible.push(OpenAiCompatibleConfig {
            id: "qwen".into(),
            base_url: "http://127.0.0.1:18000/v1".into(),
            api_key: None,
            enabled: true,
        });
        assert!(cfg.effective_enabled());
    }

    #[test]
    fn explicit_enabled_overrides_auto_detection() {
        let mut cfg = LlmProxyConfig {
            enabled: Some(false),
            ..LlmProxyConfig::default()
        };
        cfg.sources.openai_compatible.push(OpenAiCompatibleConfig {
            id: "qwen".into(),
            base_url: "http://127.0.0.1:18000/v1".into(),
            api_key: None,
            enabled: true,
        });
        assert!(!cfg.effective_enabled());
    }
}
