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
    /// で、値を捏造しない）。短期・長期のうち**厳しい方**（残りが少ない方）。
    pub remaining: Option<f64>,
    /// ADR-0053 D4（Phase 66）: 短期枠（Claude の 5 時間 / Codex の週内相当）だけの残り。0.0〜1.0。
    /// 測れないときは `null`（値を捏造しない）。
    pub remaining_short: Option<f64>,
    /// ADR-0053 D4: 長期枠（7 日）だけの残り。0.0〜1.0。測れないときは `null`。
    pub remaining_long: Option<f64>,
    pub cooldown_until: Option<i64>,
    pub cooldown_reason: Option<String>,
}

/// ADR-0053 D4: 1 枠だけの残り（`measured_remaining` と同じ「観測が新しく、枠が有効」という規律を
/// 1 枠に適用する）。観測が古い（300 秒超）・未来（壁時計のずれ）・枠が無い／期限切れ／範囲外の
/// `utilization` はすべて `None`（測れない、を捏造しない）。
fn window_remaining(obs: &task_core::RateLimitObservation, now: i64, window: Option<task_core::RateWindow>) -> Option<f64> {
    if now < obs.observed_at || now - obs.observed_at > 300 {
        return None;
    }
    let w = window?;
    if w.resets_at <= now || !w.utilization.is_finite() || !(0.0..=1.0).contains(&w.utilization) {
        return None;
    }
    Some(1.0 - w.utilization)
}

/// ADR-0053 D4: `celeris/<tier>` が今どこに解決するか（表示専用。副作用なし）。
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CelerisTierView {
    /// `"frontier"` / `"standard"` / `"cheap"`。
    pub tier: String,
    /// 解決先の供給元 id（`sources[].id` と同じ形）。今選べる候補が無ければ `null`。
    pub resolves_to: Option<String>,
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
    /// ADR-0053 D4（Phase 66）: `celeris/<tier>` が今どこに解決するか（3 tier とも）。
    pub celeris_tiers: Vec<CelerisTierView>,
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
        // ADR-0053 D4（Phase 66）: 3 tier とも同じ決定的な選択（`server.rs::attempts_for`）で解決先を見る。
        // tier ごとにモデル写像が無ければ「選べない」= `None`（値を捏造しない）。
        let mut celeris_tiers = Vec::new();
        for tier in [task_core::Tier::Frontier, task_core::Tier::Standard, task_core::Tier::Cheap] {
            let resolves_to = self.resolves_tier(tier, now).await;
            celeris_tiers.push(CelerisTierView {
                tier: tier_str(tier).to_string(),
                resolves_to,
            });
        }
        SourcesView {
            sources,
            celeris_tiers,
        }
    }
}

fn tier_str(tier: task_core::Tier) -> &'static str {
    match tier {
        task_core::Tier::Frontier => "frontier",
        task_core::Tier::Standard => "standard",
        task_core::Tier::Cheap => "cheap",
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
            remaining_short: None,
            remaining_long: None,
            cooldown_until: None,
            cooldown_reason: None,
        };
    };
    let guard = book.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.state(id);
    let usage = state.and_then(|s| s.usage.as_ref());
    let remaining = usage.and_then(|u| measured_remaining(u, now));
    let remaining_short = usage.and_then(|u| window_remaining(u, now, u.five_hour));
    let remaining_long = usage.and_then(|u| window_remaining(u, now, u.seven_day));
    let cooldown = state.and_then(|s| s.cooldown);
    AccountSourceView {
        id: id.to_string(),
        logged_in,
        remaining,
        remaining_short,
        remaining_long,
        cooldown_until: cooldown.map(|c| c.until),
        cooldown_reason: cooldown.map(|c| format!("{:?}", c.reason)),
    }
}
