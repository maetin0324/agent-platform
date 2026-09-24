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
    input_per_million: f64,
    output_per_million: f64,
    /// prompt cache の読み取り（無ければ `input_per_million` と同じ扱いにはしない。不明なら 0 として
    /// 安全側〈過小評価〉に倒す設計もあり得るが、ここでは「載っているモデルは cache 価格まで載せる」
    /// 方針で、載せていないモデルは cache_read/write tokens を無視〈0 扱い〉する）。
    cache_read_per_million: f64,
    cache_write_per_million: f64,
}

/// 前方一致で引く単価表（先に書いた行が優先。長い/具体的なプレフィックスを先に置く）。
/// 2026-09 時点の公開価格からの概算スナップショット。
const PRICE_TABLE: &[(&str, Price)] = &[
    (
        "claude-opus",
        Price {
            input_per_million: 15.0,
            output_per_million: 75.0,
            cache_read_per_million: 1.5,
            cache_write_per_million: 18.75,
        },
    ),
    (
        "claude-sonnet",
        Price {
            input_per_million: 3.0,
            output_per_million: 15.0,
            cache_read_per_million: 0.3,
            cache_write_per_million: 3.75,
        },
    ),
    (
        "claude-haiku",
        Price {
            input_per_million: 0.8,
            output_per_million: 4.0,
            cache_read_per_million: 0.08,
            cache_write_per_million: 1.0,
        },
    ),
    (
        "gpt-5",
        Price {
            input_per_million: 1.25,
            output_per_million: 10.0,
            cache_read_per_million: 0.125,
            cache_write_per_million: 0.0,
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
    let cost = usage.input_tokens.unwrap_or(0) as f64 / 1_000_000.0 * price.input_per_million
        + usage.output_tokens.unwrap_or(0) as f64 / 1_000_000.0 * price.output_per_million
        + usage.cache_read_tokens.unwrap_or(0) as f64 / 1_000_000.0 * price.cache_read_per_million
        + usage.cache_creation_tokens.unwrap_or(0) as f64 / 1_000_000.0
            * price.cache_write_per_million;
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
        // 3.0*1 + 15.0*0.5 + 0.3*2 + 3.75*1 = 3 + 7.5 + 0.6 + 3.75 = 14.85
        assert!((cost - 14.85).abs() < 1e-9, "{cost}");
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
