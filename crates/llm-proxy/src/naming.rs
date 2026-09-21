//! モデル名の抽象（ADR-0053 D1）。`celeris/<tier>` / `claude/<tier>` / `gpt/<tier>` / `qwen/<tier>` と、
//! 供給元を明示した素通り（`claude:claude-sonnet-5`）。判断（選択）は `crate::selection` にある。

use task_core::Tier;

/// 要求されたモデル名がどの供給元の種類を指すか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// `claude-oauth`。
    Claude,
    /// `codex-oauth`。
    Gpt,
    /// `openai-compatible`（既定の実装は Qwen）。
    Qwen,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Claude => "claude",
            SourceKind::Gpt => "gpt",
            SourceKind::Qwen => "qwen",
        }
    }
}

/// `celeris/<tier>` の `Any` は供給元をプロキシが選ぶ。他は供給元を固定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceScope {
    Any,
    Only(SourceKind),
}

/// 解析されたモデル要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRequest {
    /// `celeris/<tier>` / `claude/<tier>` / `gpt/<tier>` / `qwen/<tier>`。
    Tiered { scope: SourceScope, tier: Tier },
    /// `<source>:<concrete-model>`。tier 写像を経由せず、その供給元へそのまま渡す。
    Explicit { source: SourceKind, model: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelNameError {
    #[error("empty model name")]
    Empty,
    #[error("unknown model name: {0}")]
    Unknown(String),
    #[error("unknown tier {1:?} for {0}: use frontier | standard | cheap")]
    UnknownTier(String, String),
}

fn parse_tier(s: &str) -> Option<Tier> {
    match s {
        "frontier" => Some(Tier::Frontier),
        "standard" => Some(Tier::Standard),
        "cheap" => Some(Tier::Cheap),
        _ => None,
    }
}

fn parse_source(prefix: &str) -> Option<SourceKind> {
    match prefix {
        "claude" => Some(SourceKind::Claude),
        "gpt" => Some(SourceKind::Gpt),
        "qwen" => Some(SourceKind::Qwen),
        _ => None,
    }
}

/// `name` を解析する。素通り（`:` 区切り）を tier 表記（`/` 区切り）より先に見る
/// （ADR-0053: 「供給元を接頭辞で明示したときだけ許す」）。
pub fn parse_model(name: &str) -> Result<ModelRequest, ModelNameError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ModelNameError::Empty);
    }
    if let Some((prefix, concrete)) = name.split_once(':') {
        let Some(source) = parse_source(prefix) else {
            return Err(ModelNameError::Unknown(name.to_string()));
        };
        if concrete.is_empty() {
            return Err(ModelNameError::Unknown(name.to_string()));
        }
        return Ok(ModelRequest::Explicit {
            source,
            model: concrete.to_string(),
        });
    }
    let Some((prefix, rest)) = name.split_once('/') else {
        return Err(ModelNameError::Unknown(name.to_string()));
    };
    let scope = if prefix == "celeris" {
        SourceScope::Any
    } else if let Some(source) = parse_source(prefix) {
        SourceScope::Only(source)
    } else {
        return Err(ModelNameError::Unknown(name.to_string()));
    };
    let Some(tier) = parse_tier(rest) else {
        return Err(ModelNameError::UnknownTier(prefix.to_string(), rest.to_string()));
    };
    Ok(ModelRequest::Tiered { scope, tier })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn celeris_tier_is_any_scope() {
        assert_eq!(
            parse_model("celeris/cheap").unwrap(),
            ModelRequest::Tiered {
                scope: SourceScope::Any,
                tier: Tier::Cheap
            }
        );
    }

    #[test]
    fn source_prefixed_tiers_restrict_scope() {
        for (name, source, tier) in [
            ("claude/frontier", SourceKind::Claude, Tier::Frontier),
            ("gpt/standard", SourceKind::Gpt, Tier::Standard),
            ("qwen/cheap", SourceKind::Qwen, Tier::Cheap),
        ] {
            assert_eq!(
                parse_model(name).unwrap(),
                ModelRequest::Tiered {
                    scope: SourceScope::Only(source),
                    tier
                }
            );
        }
    }

    #[test]
    fn explicit_passthrough_requires_a_known_source_prefix() {
        assert_eq!(
            parse_model("claude:claude-sonnet-5").unwrap(),
            ModelRequest::Explicit {
                source: SourceKind::Claude,
                model: "claude-sonnet-5".to_string()
            }
        );
        assert!(parse_model("anthropic:claude-sonnet-5").is_err());
        assert!(parse_model("claude:").is_err());
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert!(parse_model("").is_err());
        assert!(parse_model("gpt-4").is_err());
        assert!(parse_model("celeris/ultra").is_err());
        assert!(parse_model("mistral/cheap").is_err());
    }
}
