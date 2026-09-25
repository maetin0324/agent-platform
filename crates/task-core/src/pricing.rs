//! ADR-0061（Phase 104）: モデルの静的な単価表から `Usage` の推定コスト（USD）を計算する。
//!
//! **純粋関数のみ**（I/O・ネットワーク呼び出しはしない。ADR-0001 D2）。単価表はこのファイルに
//! 埋め込んだスナップショットであり、プロバイダの実際の請求額とは一致しない可能性がある
//! （取得時点の公開価格からの概算。運用側が実際の請求と突き合わせて `PRICE_TABLE` を更新する前提）。
//! モデル名は `HarnessSpec`/`ProviderSpec` に書かれた文字列（例: `"claude-sonnet-5"`,
//! `"gpt-5-codex"`）をそのまま渡す想定で、**前方一致**で単価表を引く（バージョン・日付付きの
//! モデル名にも耐えるため）。一致しないモデルは `None`（費用不明。0 円と偽らない）。

use crate::model::Usage;

/// 100 万トークンあたりの USD 単価。
#[derive(Debug, Clone, Copy)]
struct Price {
    input_per_million: Option<f64>,
    output_per_million: Option<f64>,
    cache_read_per_million: Option<f64>,
    cache_write_per_million: Option<f64>,
}

/// 前方一致で引く単価表（先に書いた行が優先。長い/具体的なプレフィックスを先に置く）。
/// 2026-09 時点の公開価格からの概算スナップショット。
const PRICE_TABLE: &[(&str, Price)] = &[
    (
        "claude-fable-5-1",
        Price {
            input_per_million: Some(10.0),
            output_per_million: Some(50.0),
            cache_read_per_million: Some(0.25),
            cache_write_per_million: Some(12.5),
        },
    ),
    (
        "claude-opus-5-5",
        Price {
            input_per_million: Some(4.0),
            output_per_million: Some(20.0),
            cache_read_per_million: Some(0.20),
            cache_write_per_million: Some(5.0),
        },
    ),
    (
        "claude-sonnet-5",
        Price {
            input_per_million: Some(2.0),
            output_per_million: Some(10.0),
            cache_read_per_million: Some(0.2),
            cache_write_per_million: Some(2.5),
        },
    ),
    (
        "claude-haiku-4-5",
        Price {
            input_per_million: Some(1.0),
            output_per_million: Some(5.0),
            cache_read_per_million: Some(0.1),
            cache_write_per_million: Some(1.25),
        },
    ),
    (
        "gpt-6-astra",
        Price {
            input_per_million: None,
            output_per_million: None,
            cache_read_per_million: None,
            cache_write_per_million: None,
        },
    ),
    (
        "gpt-6-sol",
        Price {
            input_per_million: None,
            output_per_million: None,
            cache_read_per_million: None,
            cache_write_per_million: None,
        },
    ),
    (
        "gpt-6-luna",
        Price {
            input_per_million: None,
            output_per_million: None,
            cache_read_per_million: None,
            cache_write_per_million: None,
        },
    ),
    (
        "claude-opus",
        Price {
            input_per_million: Some(15.0),
            output_per_million: Some(75.0),
            cache_read_per_million: Some(1.5),
            cache_write_per_million: Some(18.75),
        },
    ),
    (
        "claude-sonnet",
        Price {
            input_per_million: Some(3.0),
            output_per_million: Some(15.0),
            cache_read_per_million: Some(0.3),
            cache_write_per_million: Some(3.75),
        },
    ),
    (
        "claude-haiku",
        Price {
            input_per_million: Some(0.8),
            output_per_million: Some(4.0),
            cache_read_per_million: Some(0.08),
            cache_write_per_million: Some(1.0),
        },
    ),
    (
        "gpt-5",
        Price {
            input_per_million: Some(1.25),
            output_per_million: Some(10.0),
            cache_read_per_million: Some(0.125),
            cache_write_per_million: None,
        },
    ),
];

fn price_for(model: &str) -> Option<Price> {
    let model = model.to_ascii_lowercase();
    PRICE_TABLE
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map(|(_, price)| *price)
}

/// `model`（前方一致で単価表を引く）と `usage` から USD の推定コストを計算する。
/// 単価表に無いモデル、もしくは `usage` にトークンが 1 件も無ければ `None`。
pub fn estimate_cost_usd(model: &str, usage: &Usage) -> Option<f64> {
    if usage.input_tokens.is_none()
        && usage.output_tokens.is_none()
        && usage.cache_read_tokens.is_none()
        && usage.cache_creation_tokens.is_none()
    {
        return None;
    }
    let price = price_for(model)?;
    fn part(tokens: Option<u64>, rate: Option<f64>) -> Option<f64> {
        let tokens = tokens.unwrap_or(0);
        if tokens == 0 {
            Some(0.0)
        } else {
            Some(tokens as f64 / 1_000_000.0 * rate?)
        }
    }
    let cost = part(usage.input_tokens, price.input_per_million)?
        + part(usage.output_tokens, price.output_per_million)?
        + part(usage.cache_read_tokens, price.cache_read_per_million)?
        + part(usage.cache_creation_tokens, price.cache_write_per_million)?;
    Some(cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_model_computes_from_all_four_token_kinds() {
        let usage = Usage {
            input_tokens: Some(1_000_000),
            output_tokens: Some(500_000),
            cache_read_tokens: Some(2_000_000),
            cache_creation_tokens: Some(1_000_000),
            cost_usd: None,
        };
        let cost = estimate_cost_usd("claude-sonnet-5", &usage).expect("known model");
        // 2.0*1 + 10.0*0.5 + 0.2*2 + 2.5*1 = 9.9
        assert!((cost - 9.9).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn version_suffixes_still_match_by_prefix() {
        let usage = Usage {
            input_tokens: Some(1_000_000),
            ..Usage::default()
        };
        assert!(estimate_cost_usd("claude-opus-4-20260101", &usage).is_some());
        assert!(estimate_cost_usd("gpt-5-codex", &usage).is_some());
    }

    #[test]
    fn unknown_model_is_none() {
        let usage = Usage {
            input_tokens: Some(1000),
            ..Usage::default()
        };
        assert_eq!(estimate_cost_usd("some-local-llm", &usage), None);
    }

    #[test]
    fn default_claude_frontier_has_a_price() {
        let usage = Usage {
            input_tokens: Some(1_000_000),
            ..Usage::default()
        };
        assert_eq!(estimate_cost_usd("claude-fable-5-1", &usage), Some(10.0));
    }

    #[test]
    fn default_gpt_frontier_has_no_offline_price_source() {
        // docs/ と config/ には gpt-6-astra の単価根拠がない。
        let usage = Usage {
            input_tokens: Some(1_000_000),
            ..Usage::default()
        };
        assert_eq!(estimate_cost_usd("gpt-6-astra", &usage), None);
    }

    #[test]
    fn nonzero_tokens_with_missing_price_are_unknown() {
        let usage = Usage {
            cache_creation_tokens: Some(1),
            ..Usage::default()
        };
        assert_eq!(estimate_cost_usd("gpt-5-codex", &usage), None);
        let zero = Usage {
            cache_creation_tokens: Some(0),
            input_tokens: Some(1_000_000),
            ..Usage::default()
        };
        assert_eq!(estimate_cost_usd("gpt-5-codex", &zero), Some(1.25));
    }

    #[test]
    fn no_tokens_at_all_is_none_even_for_a_known_model() {
        assert_eq!(
            estimate_cost_usd("claude-sonnet-5", &Usage::default()),
            None
        );
    }

    #[test]
    fn missing_fields_are_treated_as_zero_not_as_unknown() {
        let usage = Usage {
            output_tokens: Some(1_000_000),
            ..Usage::default()
        };
        let cost = estimate_cost_usd("claude-haiku-4", &usage).expect("known model");
        assert!((cost - 4.0).abs() < 1e-9, "{cost}");
    }
}
