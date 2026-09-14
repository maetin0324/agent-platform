//! `ProviderPolicy` と `StaticPolicy`（DESIGN §5.5, ADR-0005 D6, ADR-0012 D2）。

use std::collections::{HashMap, HashSet};
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

/// `ProviderPolicy::select` の結果（ADR-0012 D2, P-20 / P-33）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Picked { adapter: AdapterId, provider: ProviderId },
    /// 条件（adapter 指定・tier）に合うプロバイダはあるが、全て cooldown 中か除外されている。一時的。
    Busy,
    /// 条件に合うプロバイダが設定に 1 つも無い。設定を直さない限り解消しない。
    NoMatchingProvider,
}

/// DESIGN §5.5 の trait。既存 3 メソッドのシグネチャは変えない（供給層との境界）。
pub trait ProviderPolicy: Send {
    fn pick(&self, hint: &WorkerHint, now: Instant) -> Option<(AdapterId, ProviderId)>;
    fn report(&mut self, provider: ProviderId, outcome: &ProviderOutcome);
    fn concurrency_limit(&self, provider: ProviderId) -> usize;

    /// ADR-0012 D2: `excluded`（並列度の上限に達したプロバイダ等）を除いて選ぶ。既定実装は `pick` から導くので
    /// 「候補なし」と「一時的に不可」を区別できず、選べなければ `Busy`（従来どおり待つ）を返す。
    fn select(&self, hint: &WorkerHint, now: Instant, excluded: &HashSet<ProviderId>) -> Selection {
        match self.pick(hint, now) {
            Some((adapter, provider)) if !excluded.contains(&provider) => Selection::Picked { adapter, provider },
            _ => Selection::Busy,
        }
    }
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

    fn matches(p: &ProviderSpec, hint: &WorkerHint) -> bool {
        hint.adapter.as_deref().is_none_or(|a| p.adapter == a) && p.tiers.contains(&hint.tier)
    }

    fn cooling_down(&self, p: &ProviderSpec, now: Instant) -> bool {
        self.cooldown_until.get(&p.id).is_some_and(|until| *until > now)
    }
}

impl ProviderPolicy for StaticPolicy {
    fn pick(&self, hint: &WorkerHint, now: Instant) -> Option<(AdapterId, ProviderId)> {
        self.providers
            .iter()
            .find(|p| Self::matches(p, hint) && !self.cooling_down(p, now))
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

    /// ADR-0012 D2: 設定表の順に、条件に合い cooldown 中でも除外されてもいない最初の行。
    fn select(&self, hint: &WorkerHint, now: Instant, excluded: &HashSet<ProviderId>) -> Selection {
        let mut any_match = false;
        for p in &self.providers {
            if !Self::matches(p, hint) {
                continue;
            }
            any_match = true;
            if self.cooling_down(p, now) || excluded.contains(&p.id) {
                continue;
            }
            return Selection::Picked {
                adapter: p.adapter.clone(),
                provider: p.id.clone(),
            };
        }
        if any_match {
            Selection::Busy
        } else {
            Selection::NoMatchingProvider
        }
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

    /// ADR-0012 D2（P-20）: 除外されたプロバイダ（並列度の上限）と cooldown 中のプロバイダを飛ばして次の行へフォールバックする。
    #[test]
    fn select_falls_back_past_excluded_and_cooling_providers() {
        let mut policy = StaticPolicy::new(
            vec![
                spec("acct-a", "claude-code", &[Tier::Standard], 1),
                spec("acct-b", "claude-code", &[Tier::Standard], 1),
                spec("acct-c", "claude-code", &[Tier::Standard], 1),
            ],
            Duration::from_secs(5),
        );
        let now = Instant::now();
        let h = hint(Tier::Standard, Some("claude-code"));
        let picked = |p: &str| Selection::Picked { adapter: "claude-code".into(), provider: p.into() };
        assert_eq!(policy.select(&h, now, &HashSet::new()), picked("acct-a"));
        let excluded: HashSet<ProviderId> = ["acct-a".to_string()].into();
        assert_eq!(policy.select(&h, now, &excluded), picked("acct-b"));
        policy.report("acct-b".into(), &ProviderOutcome::Throttled { retry_after: Duration::from_secs(60) });
        assert_eq!(policy.select(&h, now, &excluded), picked("acct-c"));
        let all: HashSet<ProviderId> = ["acct-a".to_string(), "acct-c".to_string()].into();
        assert_eq!(policy.select(&h, now, &all), Selection::Busy);
    }

    /// ADR-0012 D2（P-33）: 設定に合う行が無い場合は `NoMatchingProvider`、合う行が cooldown 中なら `Busy`。
    #[test]
    fn select_distinguishes_no_matching_provider_from_busy() {
        let mut policy = StaticPolicy::new(vec![spec("p1", "codex", &[Tier::Frontier], 1)], Duration::from_secs(5));
        let now = Instant::now();
        assert_eq!(policy.select(&hint(Tier::Standard, None), now, &HashSet::new()), Selection::NoMatchingProvider);
        assert_eq!(policy.select(&hint(Tier::Frontier, Some("claude-code")), now, &HashSet::new()), Selection::NoMatchingProvider);
        policy.report("p1".into(), &ProviderOutcome::Exhausted);
        assert_eq!(policy.select(&hint(Tier::Frontier, None), now, &HashSet::new()), Selection::Busy);
    }

    /// `select` を実装しない既存のポリシー（供給層）は、`pick` からの既定実装で従来どおり動く。
    #[test]
    fn default_select_is_derived_from_pick() {
        struct PickOnly;
        impl ProviderPolicy for PickOnly {
            fn pick(&self, _hint: &WorkerHint, _now: Instant) -> Option<(AdapterId, ProviderId)> {
                Some(("fake".into(), "only".into()))
            }
            fn report(&mut self, _provider: ProviderId, _outcome: &ProviderOutcome) {}
            fn concurrency_limit(&self, _provider: ProviderId) -> usize {
                1
            }
        }
        let now = Instant::now();
        let h = hint(Tier::Standard, None);
        assert_eq!(
            PickOnly.select(&h, now, &HashSet::new()),
            Selection::Picked { adapter: "fake".into(), provider: "only".into() }
        );
        assert_eq!(PickOnly.select(&h, now, &["only".to_string()].into()), Selection::Busy);
    }
}
