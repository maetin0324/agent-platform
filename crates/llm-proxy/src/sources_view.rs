//! `GET /llm/sources`（ADR-0053 D4。API 自体は Phase 66 だが、型と算出はここで用意する。Phase 65）。
//!
//! 供給元ごとの到達性・アカウントの残量・cooldown・直近 1 時間の要求/token 数を 1 つのビューにまとめる。
//! 判断はしない（見えるようにするだけ）。

use schemars::JsonSchema;
use serde::Serialize;

use task_dispatch::accounts::{measured_remaining, scan_accounts};

use crate::server::ProxyState;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AccountSourceView {
    pub id: String,
    pub logged_in: bool,
    /// 0.0〜1.0（測れないときは `null`。`task_dispatch::accounts::measured_remaining` と同じ規律
    /// で、値を捏造しない）。
    pub remaining: Option<f64>,
    pub cooldown_until: Option<i64>,
    pub cooldown_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SourceView {
    /// `claude-oauth` / `codex-oauth` / `openai-compatible:<id>`。
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    /// `openai-compatible` だけ probe する。oauth のプールは `null`（到達性ではなくアカウントの残量で見る）。
    pub reachable: Option<bool>,
    pub accounts: Vec<AccountSourceView>,
    pub last_hour_requests: u64,
    pub last_hour_prompt_tokens: u64,
    pub last_hour_completion_tokens: u64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SourcesView {
    pub sources: Vec<SourceView>,
}

impl ProxyState {
    /// `now` は Unix 秒（直近 1 時間の起点計算に使う）。
    pub async fn sources_view(&self, now: i64) -> SourcesView {
        let hourly = self
            .db_path
            .as_ref()
            .and_then(|path| crate::log::open(path, self.busy_timeout).ok())
            .and_then(|conn| crate::log::hourly_counts_by_source(&conn, now - 3600).ok())
            .unwrap_or_default();

        let mut sources = Vec::new();

        if let Some(cfg) = &self.config.sources.claude_oauth {
            let dirs = scan_accounts(&cfg.accounts_dir, task_core::AccountAdapter::ClaudeCode);
            let accounts = dirs
                .iter()
                .map(|d| account_view(&d.id, d.logged_in, self.claude_book.as_deref(), now))
                .collect();
            let counts = hourly.get("claude-oauth").cloned().unwrap_or_default();
            sources.push(SourceView {
                id: "claude-oauth".to_string(),
                kind: "claude-oauth".to_string(),
                enabled: cfg.enabled,
                reachable: None,
                accounts,
                last_hour_requests: counts.requests,
                last_hour_prompt_tokens: counts.prompt_tokens,
                last_hour_completion_tokens: counts.completion_tokens,
            });
        }
        if let Some(cfg) = &self.config.sources.codex_oauth {
            let dirs = scan_accounts(&cfg.accounts_dir, task_core::AccountAdapter::Codex);
            let accounts = dirs
                .iter()
                .map(|d| account_view(&d.id, d.logged_in, self.codex_book.as_deref(), now))
                .collect();
            let counts = hourly.get("codex-oauth").cloned().unwrap_or_default();
            sources.push(SourceView {
                id: "codex-oauth".to_string(),
                kind: "codex-oauth".to_string(),
                enabled: cfg.enabled,
                reachable: None,
                accounts,
                last_hour_requests: counts.requests,
                last_hour_prompt_tokens: counts.prompt_tokens,
                last_hour_completion_tokens: counts.completion_tokens,
            });
        }
        for relay_cfg in &self.config.sources.openai_compatible {
            let reachable = self.reachable(relay_cfg).await;
            let id = format!("openai-compatible:{}", relay_cfg.id);
            let counts = hourly.get(&id).cloned().unwrap_or_default();
            sources.push(SourceView {
                id,
                kind: "openai-compatible".to_string(),
                enabled: relay_cfg.enabled,
                reachable: Some(reachable),
                accounts: Vec::new(),
                last_hour_requests: counts.requests,
                last_hour_prompt_tokens: counts.prompt_tokens,
                last_hour_completion_tokens: counts.completion_tokens,
            });
        }
        SourcesView { sources }
    }
}

fn account_view(
    id: &str,
    logged_in: bool,
    book: Option<&std::sync::Mutex<task_dispatch::accounts::AccountBook>>,
    now: i64,
) -> AccountSourceView {
    let Some(book) = book else {
        return AccountSourceView {
            id: id.to_string(),
            logged_in,
            remaining: None,
            cooldown_until: None,
            cooldown_reason: None,
        };
    };
    let guard = book.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.state(id);
    let remaining = state.and_then(|s| s.usage.as_ref()).and_then(|u| measured_remaining(u, now));
    let cooldown = state.and_then(|s| s.cooldown);
    AccountSourceView {
        id: id.to_string(),
        logged_in,
        remaining,
        cooldown_until: cooldown.map(|c| c.until),
        cooldown_reason: cooldown.map(|c| format!("{:?}", c.reason)),
    }
}
