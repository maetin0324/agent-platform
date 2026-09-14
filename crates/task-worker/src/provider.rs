//! 供給側失敗の文字列分類（ADR-0010 D5）。`claude-code`/`codex` のエラー文面（結果メッセージ・
//! `stderr` 末尾）を、決定的な部分一致規則で `ProviderFailure` に写す純粋関数。LLM を呼ばない・
//! ネットワークを見ない（原則: ディスパッチャ/アダプタに協調判断の LLM 呼び出しを入れない）。

use crate::protocol::ProviderFailure;

const EXHAUSTED_PATTERNS: &[&str] = &["usage limit", "quota", "credit balance"];
const THROTTLED_PATTERNS: &[&str] = &["rate limit", "rate_limit", "overloaded"];
const THROTTLED_CODES: &[&str] = &["429", "529"];
const AUTH_FAILED_PATTERNS: &[&str] = &["invalid api key", "authentication", "not logged in", "/login"];
const AUTH_FAILED_CODES: &[&str] = &["401"];

/// `text` を大文字小文字を無視した部分一致で分類する。判定順は Exhausted → Throttled → AuthFailed
/// （`usage limit` のような文言が `rate limit` 等と紛れないよう、より具体的な枯渇を先に見る）。
/// どれにも当たらなければ `None`（例: モデル非対応のような taskd 側では対処しようのないエラー。
/// ADR-0010 D5）。
///
/// HTTP ステータス（`429` 等）は独立したトークンとしてだけ一致させる（前後が英数字・`.`・`:` でない）。
/// stderr 末尾のスタックトレースに含まれる `cli.js:4291:17` のような位置情報を供給側失敗と誤分類し、
/// attempts を消費しない requeue を無期限に繰り返すのを防ぐ（Phase 7 監査の指摘）。
pub fn classify_provider_failure(text: &str) -> Option<ProviderFailure> {
    let lower = text.to_lowercase();
    if EXHAUSTED_PATTERNS.iter().any(|p| lower.contains(p)) {
        return Some(ProviderFailure::Exhausted);
    }
    if THROTTLED_PATTERNS.iter().any(|p| lower.contains(p)) || THROTTLED_CODES.iter().any(|c| contains_code(&lower, c)) {
        return Some(ProviderFailure::Throttled { retry_after_secs: 60 });
    }
    if AUTH_FAILED_PATTERNS.iter().any(|p| lower.contains(p)) || AUTH_FAILED_CODES.iter().any(|c| contains_code(&lower, c)) {
        return Some(ProviderFailure::AuthFailed);
    }
    None
}

/// `code` が独立したトークンとして現れるか。前後が英数字・`.` なら数字列の一部とみなす。`:` は位置情報
/// （`file.js:429:17` の `:17`、`12:429` の `12:`）を作る場合、つまり `:` の向こう側が数字のときだけ一部とみなす
/// （`HTTP 529: too many requests` や `status:429` は一致させる）。
fn contains_code(text: &str, code: &str) -> bool {
    let is_joined = |c: char| c.is_ascii_alphanumeric() || c == '.';
    text.match_indices(code).any(|(start, _)| {
        let mut before = text[..start].chars().rev();
        let mut after = text[start + code.len()..].chars();
        let joined_before = match before.next() {
            Some(':') => before.next().is_some_and(|c| c.is_ascii_digit()),
            Some(c) => is_joined(c),
            None => false,
        };
        let joined_after = match after.next() {
            Some(':') => after.next().is_some_and(|c| c.is_ascii_digit()),
            Some(c) => is_joined(c),
            None => false,
        };
        !joined_before && !joined_after
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_exhausted() {
        assert_eq!(classify_provider_failure("You've hit your usage limit"), Some(ProviderFailure::Exhausted));
        assert_eq!(classify_provider_failure("Quota exceeded for this project"), Some(ProviderFailure::Exhausted));
        assert_eq!(
            classify_provider_failure("Your credit balance is too low to access the API"),
            Some(ProviderFailure::Exhausted)
        );
    }

    #[test]
    fn classifies_throttled_with_fixed_retry_after() {
        assert_eq!(
            classify_provider_failure("API Error: 429 rate limit exceeded"),
            Some(ProviderFailure::Throttled { retry_after_secs: 60 })
        );
        assert_eq!(
            classify_provider_failure("Overloaded, please retry later"),
            Some(ProviderFailure::Throttled { retry_after_secs: 60 })
        );
        assert_eq!(
            classify_provider_failure("HTTP 529: too many requests"),
            Some(ProviderFailure::Throttled { retry_after_secs: 60 })
        );
        assert_eq!(
            classify_provider_failure("upstream returned rate_limit_error"),
            Some(ProviderFailure::Throttled { retry_after_secs: 60 })
        );
        assert_eq!(classify_provider_failure("status=429"), Some(ProviderFailure::Throttled { retry_after_secs: 60 }));
    }

    #[test]
    fn classifies_auth_failed() {
        assert_eq!(
            classify_provider_failure("Invalid API key \u{b7} Please run /login"),
            Some(ProviderFailure::AuthFailed)
        );
        assert_eq!(classify_provider_failure("401 Unauthorized"), Some(ProviderFailure::AuthFailed));
        assert_eq!(classify_provider_failure("authentication required"), Some(ProviderFailure::AuthFailed));
        assert_eq!(classify_provider_failure("you are not logged in"), Some(ProviderFailure::AuthFailed));
    }

    #[test]
    fn exhausted_takes_priority_over_throttled_patterns() {
        // "usage limit" 自体には throttled のパターンは含まれないが、判定順（Exhausted が先）を
        // 明示的に固定するため、優先順位そのものを検証する。
        assert_eq!(classify_provider_failure("usage limit reached, try again tomorrow"), Some(ProviderFailure::Exhausted));
    }

    #[test]
    fn unmatched_text_returns_none() {
        assert_eq!(
            classify_provider_failure(
                "The 'gpt-5.4' model is not supported when using Codex with a ChatGPT account."
            ),
            None
        );
        assert_eq!(classify_provider_failure(""), None);
    }

    /// 監査の指摘: スタックトレースの行・列番号やバージョン番号に含まれる数字列は供給側失敗ではない。
    #[test]
    fn status_codes_inside_positions_or_numbers_do_not_match() {
        for text in [
            "TypeError: x is undefined\n    at run (cli.js:4291:17)",
            "at file.js:429:17",
            "node v14290.1",
            "exit code 14011",
            "request id 4015xyz",
        ] {
            assert_eq!(classify_provider_failure(text), None, "{text}");
        }
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(
            classify_provider_failure("RATE LIMIT EXCEEDED"),
            Some(ProviderFailure::Throttled { retry_after_secs: 60 })
        );
        assert_eq!(classify_provider_failure("NOT LOGGED IN"), Some(ProviderFailure::AuthFailed));
    }
}
