//! `ProviderPolicy` と `StaticPolicy`（DESIGN §5.5, ADR-0005 D6）。implementer（unit C）が実装する。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use task_core::{Tier, WorkerHint};

pub type AdapterId = String;
pub type ProviderId = String;

/// `report` に渡す供給側の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOutcome {
    Ok,
    Throttled { retry_after: Duration },
    AuthFailed,
    Exhausted,
}

/// 設定表の 1 行（`[[providers]]`）。上から順に優先。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSpec {
    pub id: ProviderId,
    pub adapter: AdapterId,
    pub tiers: Vec<Tier>,
    pub concurrency: usize,
    pub model: String,
}

/// DESIGN §5.5 の trait。シグネチャは変えない（供給層との境界）。
pub trait ProviderPolicy: Send {
    fn pick(&self, hint: &WorkerHint, now: Instant) -> Option<(AdapterId, ProviderId)>;
    fn report(&mut self, provider: ProviderId, outcome: &ProviderOutcome);
    fn concurrency_limit(&self, provider: ProviderId) -> usize;
}

/// 設定表の優先順位どおりに選ぶ。Throttled は cooldown まで除外。
#[derive(Debug)]
pub struct StaticPolicy {
    providers: Vec<ProviderSpec>,
    error_cooldown: Duration,
    cooldown_until: HashMap<ProviderId, Instant>,
}

impl StaticPolicy {
    pub fn new(providers: Vec<ProviderSpec>, error_cooldown: Duration) -> Self {
        Self {
            providers,
            error_cooldown,
            cooldown_until: HashMap::new(),
        }
    }

    pub fn providers(&self) -> &[ProviderSpec] {
        &self.providers
    }

    /// 指定プロバイダの `model`（`WorkerStarted.model` 用）。
    pub fn model_of(&self, provider: &str) -> Option<&str> {
        self.providers
            .iter()
            .find(|p| p.id == provider)
            .map(|p| p.model.as_str())
    }
}

impl ProviderPolicy for StaticPolicy {
    fn pick(&self, hint: &WorkerHint, now: Instant) -> Option<(AdapterId, ProviderId)> {
        self.providers
            .iter()
            .find(|p| {
                let adapter_ok = hint.adapter.as_deref().is_none_or(|a| p.adapter == a);
                let tier_ok = p.tiers.contains(&hint.tier);
                let cooldown_ok = self
                    .cooldown_until
                    .get(&p.id)
                    .is_none_or(|until| *until <= now);
                adapter_ok && tier_ok && cooldown_ok
            })
            .map(|p| {
                tracing::debug!(provider = %p.id, adapter = %p.adapter, "policy: picked provider");
                (p.adapter.clone(), p.id.clone())
            })
    }

    fn report(&mut self, provider: ProviderId, outcome: &ProviderOutcome) {
        match outcome {
            ProviderOutcome::Ok => {}
            ProviderOutcome::Throttled { retry_after } => {
                let until = Instant::now() + *retry_after;
                tracing::debug!(%provider, ?until, "policy: throttled");
                self.cooldown_until.insert(provider, until);
            }
            ProviderOutcome::AuthFailed | ProviderOutcome::Exhausted => {
                let until = Instant::now() + self.error_cooldown;
                tracing::debug!(%provider, ?until, ?outcome, "policy: error cooldown");
                self.cooldown_until.insert(provider, until);
            }
        }
    }

    fn concurrency_limit(&self, provider: ProviderId) -> usize {
        self.providers
            .iter()
            .find(|p| p.id == provider)
            .map(|p| p.concurrency)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, adapter: &str, tiers: &[Tier], concurrency: usize) -> ProviderSpec {
        ProviderSpec {
            id: id.to_string(),
            adapter: adapter.to_string(),
            tiers: tiers.to_vec(),
            concurrency,
            model: format!("{id}-model"),
        }
    }

    fn hint(tier: Tier, adapter: Option<&str>) -> WorkerHint {
        WorkerHint {
            tier,
            adapter: adapter.map(str::to_string),
        }
    }

    #[test]
    fn picks_first_matching_by_priority() {
        let policy = StaticPolicy::new(
            vec![
                spec("p1", "claude-code", &[Tier::Frontier], 2),
                spec("p2", "codex", &[Tier::Frontier], 2),
            ],
            Duration::from_secs(5),
        );
        let now = Instant::now();
        assert_eq!(
            policy.pick(&hint(Tier::Frontier, None), now),
            Some(("claude-code".to_string(), "p1".to_string()))
        );
    }

    #[test]
    fn filters_by_adapter_hint() {
        let policy = StaticPolicy::new(
            vec![
                spec("p1", "claude-code", &[Tier::Frontier], 2),
                spec("p2", "codex", &[Tier::Frontier], 2),
            ],
            Duration::from_secs(5),
        );
        let now = Instant::now();
        assert_eq!(
            policy.pick(&hint(Tier::Frontier, Some("codex")), now),
            Some(("codex".to_string(), "p2".to_string()))
        );
        assert_eq!(policy.pick(&hint(Tier::Frontier, Some("dsh")), now), None);
    }

    #[test]
    fn filters_by_tier() {
        let policy = StaticPolicy::new(
            vec![
                spec("p1", "claude-code", &[Tier::Standard], 2),
                spec("p2", "codex", &[Tier::Frontier], 2),
            ],
            Duration::from_secs(5),
        );
        let now = Instant::now();
        assert_eq!(
            policy.pick(&hint(Tier::Frontier, None), now),
            Some(("codex".to_string(), "p2".to_string()))
        );
    }

    #[test]
    fn throttled_provider_is_skipped_until_retry_after() {
        let mut policy = StaticPolicy::new(
            vec![
                spec("p1", "claude-code", &[Tier::Frontier], 2),
                spec("p2", "codex", &[Tier::Frontier], 2),
            ],
            Duration::from_secs(5),
        );
        let now = Instant::now();
        policy.report(
            "p1".to_string(),
            &ProviderOutcome::Throttled {
                retry_after: Duration::from_secs(10),
            },
        );
        assert_eq!(
            policy.pick(&hint(Tier::Frontier, None), now),
            Some(("codex".to_string(), "p2".to_string()))
        );
        assert_eq!(
            policy.pick(&hint(Tier::Frontier, None), now + Duration::from_secs(11)),
            Some(("claude-code".to_string(), "p1".to_string()))
        );
    }

    #[test]
    fn throttled_only_provider_yields_none() {
        let mut policy = StaticPolicy::new(
            vec![spec("p1", "claude-code", &[Tier::Frontier], 2)],
            Duration::from_secs(5),
        );
        let now = Instant::now();
        policy.report(
            "p1".to_string(),
            &ProviderOutcome::Throttled {
                retry_after: Duration::from_secs(10),
            },
        );
        assert_eq!(policy.pick(&hint(Tier::Frontier, None), now), None);
    }

    #[test]
    fn auth_failed_uses_error_cooldown() {
        let mut policy = StaticPolicy::new(
            vec![
                spec("p1", "claude-code", &[Tier::Frontier], 2),
                spec("p2", "codex", &[Tier::Frontier], 2),
            ],
            Duration::from_secs(5),
        );
        let now = Instant::now();
        policy.report("p1".to_string(), &ProviderOutcome::AuthFailed);
        assert_eq!(
            policy.pick(&hint(Tier::Frontier, None), now),
            Some(("codex".to_string(), "p2".to_string()))
        );
        assert_eq!(
            policy.pick(&hint(Tier::Frontier, None), now + Duration::from_secs(6)),
            Some(("claude-code".to_string(), "p1".to_string()))
        );
    }

    #[test]
    fn exhausted_uses_error_cooldown() {
        let mut policy = StaticPolicy::new(
            vec![spec("p1", "claude-code", &[Tier::Frontier], 2)],
            Duration::from_secs(5),
        );
        let now = Instant::now();
        policy.report("p1".to_string(), &ProviderOutcome::Exhausted);
        assert_eq!(policy.pick(&hint(Tier::Frontier, None), now), None);
        assert_eq!(
            policy.pick(&hint(Tier::Frontier, None), now + Duration::from_secs(6)),
            Some(("claude-code".to_string(), "p1".to_string()))
        );
    }

    #[test]
    fn ok_does_not_clear_cooldown() {
        let mut policy = StaticPolicy::new(
            vec![spec("p1", "claude-code", &[Tier::Frontier], 2)],
            Duration::from_secs(5),
        );
        let now = Instant::now();
        policy.report(
            "p1".to_string(),
            &ProviderOutcome::Throttled {
                retry_after: Duration::from_secs(10),
            },
        );
        policy.report("p1".to_string(), &ProviderOutcome::Ok);
        assert_eq!(policy.pick(&hint(Tier::Frontier, None), now), None);
    }

    #[test]
    fn report_unknown_provider_does_not_panic() {
        let mut policy = StaticPolicy::new(Vec::new(), Duration::from_secs(5));
        policy.report("unknown".to_string(), &ProviderOutcome::AuthFailed);
        policy.report(
            "unknown".to_string(),
            &ProviderOutcome::Throttled {
                retry_after: Duration::from_secs(1),
            },
        );
    }

    #[test]
    fn concurrency_limit_returns_spec_value_or_zero() {
        let policy = StaticPolicy::new(
            vec![spec("p1", "claude-code", &[Tier::Frontier], 3)],
            Duration::from_secs(5),
        );
        assert_eq!(policy.concurrency_limit("p1".to_string()), 3);
        assert_eq!(policy.concurrency_limit("unknown".to_string()), 0);
    }
}
