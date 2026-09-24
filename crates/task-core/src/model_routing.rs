//! Explicit tier bindings. Empty maps retain legacy model resolution.
use crate::Tier;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelBinding {
    pub name: String,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
    /// ADR-0068 D4（Phase 114）: この lane で使う reasoning effort（例 `"medium"`）。Phase 1 では
    /// 監査記録（`LaneResolution`）に残すだけで、CLI には渡さない。無ければ `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}
pub type TierModels = HashMap<Tier, ModelBinding>;

/// ADR-0068 D4: lane の設定上の reasoning effort（束縛が無い・effort を書いていなければ `None`）。
pub fn reasoning_effort(bindings: &TierModels, tier: Tier) -> Option<String> {
    bindings
        .get(&tier)
        .and_then(|b| b.reasoning_effort.clone())
        .filter(|e| !e.trim().is_empty())
}

/// ADR-0068 D4: lane → provider / account / model / reasoning effort の解決結果（監査記録）。
/// 解決そのものは従来の `select_provider` → `TieredAdapter::model_for_tier` → `select_tier` のまま。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LaneResolution {
    /// 残量による調整の後に実際に走らせる lane（`None` は解決前）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<Tier>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// 実行するモデル（アダプタの既定モデルなら空文字列のこともある）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}
pub fn resolve(bindings: &TierModels, tier: Tier) -> Result<Option<String>, String> {
    if bindings.is_empty() {
        return Ok(None);
    }
    let binding = bindings
        .get(&tier)
        .ok_or_else(|| format!("{tier:?}: model mapping is missing"))?;
    if let Some(reason) = &binding.unavailable_reason {
        return Err(format!("{}: {reason}", binding.name));
    }
    match binding.model_id.as_deref() {
        Some(id)
            if !id.trim().is_empty()
                && !id.starts_with('-')
                && !id.chars().any(|c| c.is_whitespace() || c.is_control()) =>
        {
            Ok(Some(id.to_owned()))
        }
        _ => Err(format!(
            "{}: executable model ID has not been configured",
            binding.name
        )),
    }
}

/// Requested tier represents the delegator's difficulty assessment. Quota is a
/// measured fraction, never dollars/tokens or task wall-clock/turn limits.
pub fn select_tier(requested: Tier, remaining: Option<f64>) -> Result<(Tier, String), String> {
    let Some(remaining) = remaining.filter(|r| r.is_finite() && (0.0..=1.0).contains(r)) else {
        return Ok((
            requested,
            format!("difficulty={requested:?}; quota remaining unknown; retain requested tier"),
        ));
    };
    if remaining <= 0.03 + f64::EPSILON {
        return Err("measured quota remaining <= 3%; execution deferred".into());
    }
    let selected = if remaining <= 0.10 + f64::EPSILON {
        Tier::Cheap
    } else if remaining <= 0.30 + f64::EPSILON && requested == Tier::Frontier {
        Tier::Standard
    } else {
        requested
    };
    Ok((
        selected,
        format!(
            "difficulty={requested:?}; measured quota remaining={:.1}%; selected={selected:?}",
            remaining * 100.0
        ),
    ))
}

pub const CREDENTIAL_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
];
pub fn credential_refs(env: &HashMap<String, String>) -> HashMap<String, String> {
    env.iter()
        .filter(|(key, _)| CREDENTIAL_KEYS.contains(&key.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tier_resolution_never_substitutes_a_missing_or_disabled_model() {
        let mut bindings = TierModels::new();
        assert_eq!(resolve(&bindings, Tier::Frontier), Ok(None));
        for (tier, name, id) in [
            (Tier::Frontier, "astra", "explicit-frontier"),
            (Tier::Standard, "sol", "explicit-standard"),
            (Tier::Cheap, "luna", "explicit-cheap"),
        ] {
            bindings.insert(
                tier,
                ModelBinding {
                    name: name.into(),
                    model_id: Some(id.into()),
                    unavailable_reason: None,
                    reasoning_effort: None,
                },
            );
            assert_eq!(resolve(&bindings, tier), Ok(Some(id.into())));
        }
        bindings.remove(&Tier::Frontier);
        assert!(resolve(&bindings, Tier::Frontier).is_err());
        let binding = bindings.get_mut(&Tier::Standard).unwrap();
        binding.unavailable_reason = Some("not available on subscription".into());
        assert!(
            resolve(&bindings, Tier::Standard)
                .unwrap_err()
                .contains("subscription")
        );
        for id in [
            None,
            Some(""),
            Some("bad model"),
            Some("-bad"),
            Some("bad\0id"),
        ] {
            bindings.get_mut(&Tier::Cheap).unwrap().model_id = id.map(str::to_owned);
            assert!(resolve(&bindings, Tier::Cheap).is_err());
        }
    }
    #[test]
    fn reasoning_effort_is_optional_and_old_bindings_still_parse() {
        let old: ModelBinding = serde_json::from_str(r#"{"name":"sol","model_id":"m"}"#).unwrap();
        assert_eq!(old.reasoning_effort, None);
        let mut bindings = TierModels::new();
        bindings.insert(Tier::Standard, old);
        assert_eq!(reasoning_effort(&bindings, Tier::Standard), None);
        bindings.get_mut(&Tier::Standard).unwrap().reasoning_effort = Some("high".into());
        assert_eq!(
            reasoning_effort(&bindings, Tier::Standard).as_deref(),
            Some("high")
        );
        assert_eq!(reasoning_effort(&bindings, Tier::Cheap), None);
    }
    #[test]
    fn difficulty_and_observed_quota_control_selection() {
        for tier in [Tier::Cheap, Tier::Standard, Tier::Frontier] {
            assert_eq!(select_tier(tier, None).unwrap().0, tier);
            assert_eq!(select_tier(tier, Some(0.9)).unwrap().0, tier);
            assert_eq!(select_tier(tier, Some(0.05)).unwrap().0, Tier::Cheap);
            assert!(select_tier(tier, Some(0.03)).is_err());
            assert!(select_tier(tier, Some(0.0)).is_err());
        }
        assert_eq!(
            select_tier(Tier::Frontier, Some(0.2)).unwrap().0,
            Tier::Standard
        );
        assert_eq!(select_tier(Tier::Cheap, Some(0.2)).unwrap().0, Tier::Cheap);
        assert_eq!(
            select_tier(Tier::Frontier, Some(1.0 - 0.70)).unwrap().0,
            Tier::Standard
        );
        for remaining in [f64::NAN, -1.0, 1.1] {
            assert!(
                select_tier(Tier::Frontier, Some(remaining))
                    .unwrap()
                    .1
                    .contains("unknown")
            );
        }
    }
}
