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
/// どれにも当たらなければ `None`（例: モデル非対応のような celeris 側では対処しようのないエラー。
/// ADR-0010 D5）。
///
/// HTTP ステータス（`429` 等）は独立したトークンとして、かつ「ステータスの文脈」にあるときだけ一致
/// させる（前後が英数字・`.`・`:` でない、かつ `HTTP 529` / `status:429` / `Error 529` / `(529)` /
/// `401 Unauthorized` のようにステータスであることを示す語や記法が前後にある）。stderr 末尾の
/// スタックトレースに含まれる `cli.js:4291:17` のような位置情報や、Python の rich トレースバックに
/// 現れる `pypdf/_page.py:529 in __getitem__` のようなファイル名:行番号を供給側失敗と誤分類し、
/// attempts を消費しない requeue を無期限に繰り返すのを防ぐ（Phase 7 監査の指摘、Phase 34 実機の
/// U34-1 / P-87）。
pub fn classify_provider_failure(text: &str) -> Option<ProviderFailure> {
    let lower = text.to_lowercase();
    if EXHAUSTED_PATTERNS.iter().any(|p| lower.contains(p)) {
        return Some(ProviderFailure::Exhausted);
    }
    // コード（`429` 等）の判定は数字そのものは大文字小文字の影響を受けないので、`has_status_context`
    // の「直後が大文字始まりの語」判定のために元の大文字小文字を保った `text` をそのまま渡す。
    if THROTTLED_PATTERNS.iter().any(|p| lower.contains(p)) || THROTTLED_CODES.iter().any(|c| contains_code(text, c)) {
        return Some(ProviderFailure::Throttled { retry_after_secs: 60 });
    }
    if AUTH_FAILED_PATTERNS.iter().any(|p| lower.contains(p)) || AUTH_FAILED_CODES.iter().any(|c| contains_code(text, c)) {
        return Some(ProviderFailure::AuthFailed);
    }
    None
}

/// `code` が独立したトークンとして、かつステータスの文脈で現れるか。
///
/// まず前後が英数字・`.` なら数字列の一部とみなす。`:` は位置情報（`file.js:429:17` の `:17`、
/// `12:429` の `12:`）を作る場合、つまり `:` の向こう側が数字のときだけ一部とみなす。加えて、`:` の
/// 手前が拡張子付きのファイル名（`pypdf/_page.py:529` のような `[\w/.-]+\.\w{1,5}:` の形）のときも
/// 一部とみなす（rich トレースバックの `file.py:529 in __getitem__` を弾く。U34-1 / P-87）。
///
/// 独立したトークンだとわかっても、それだけでは供給側の HTTP ステータスとは限らない（単なる版番号や
/// 行番号のこともある）。そこで前後に「ステータスの文脈」（`HTTP` / `status` / `error` の直後、
/// `(529)` のような括弧内、`401 Unauthorized` のようにステータス番号の直後に大文字始まりの語が続く）
/// があるときだけ一致とみなす。
fn contains_code(text: &str, code: &str) -> bool {
    let is_joined = |c: char| c.is_ascii_alphanumeric() || c == '.';
    text.match_indices(code).any(|(start, _)| {
        let end = start + code.len();
        let mut before = text[..start].chars().rev();
        let mut after = text[end..].chars();
        let joined_before = match before.next() {
            Some(':') => {
                before.next().is_some_and(|c| c.is_ascii_digit()) || ends_with_file_path(&text[..start - 1])
            }
            Some(c) => is_joined(c),
            None => false,
        };
        let joined_after = match after.next() {
            Some(':') => after.next().is_some_and(|c| c.is_ascii_digit()),
            Some(c) => is_joined(c),
            None => false,
        };
        !joined_before && !joined_after && has_status_context(text, start, end)
    })
}

/// `prefix`（コロンの手前までの文字列）が、拡張子付きのファイルパス（`pypdf/_page.py` や
/// `cli.js` のような、末尾が `.` + 1〜5 文字の単語文字になっている非空白トークン）で終わっているか。
fn ends_with_file_path(prefix: &str) -> bool {
    let token = prefix
        .rsplit(|c: char| c.is_whitespace() || c == '(' || c == '"' || c == '\'' || c == ',')
        .next()
        .unwrap_or("");
    match token.rfind('.') {
        Some(dot_idx) => {
            let ext = &token[dot_idx + 1..];
            let base = &token[..dot_idx];
            !base.is_empty()
                && !ext.is_empty()
                && ext.len() <= 5
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && base.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-' | '.'))
        }
        None => false,
    }
}

/// `text[start..end]`（数字列そのもの）の前後に、HTTP ステータスであることを示す文脈があるか。
/// - 直前（空白・`:`・`=` を除いた最後の単語）が `http` / `status` / `error`
/// - 直前が `(`（丸括弧の中の番号、例: `(529)`）
/// - 直後が空白を挟んで大文字始まりの単語（例: `401 Unauthorized`、`529 Too Many`）
fn has_status_context(text: &str, start: usize, end: usize) -> bool {
    let prefix = text[..start].trim_end_matches([' ', '\t', ':', '=']);
    if prefix.ends_with('(') {
        return true;
    }
    let last_word: String = prefix
        .rsplit(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(last_word.as_str(), "http" | "status" | "error") {
        return true;
    }
    let suffix = text[end..].trim_start_matches([' ', '\t']);
    let mut chars = suffix.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
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

    /// U34-1 / P-87: Python の rich トレースバックの `file.py:529` 形（`:` の後に数字が続かず、行番号
    /// だけで終わる）を HTTP 529 と誤認しない。実機（Phase 34 通し）の出力そのものを使う。
    #[test]
    fn real_traceback_file_line_is_not_a_status_code() {
        let text = "\
Traceback (most recent call last):
  File \"/home/user/.venv/lib/python3.11/site-packages/pqa/main.py\", line 42, in run
    from PIL import Image
ModuleNotFoundError: No module named 'PIL'

During handling of the above exception, another exception occurred:

Traceback (most recent call last):
  File \"/home/user/.venv/lib/python3.11/site-packages/pypdf/_page.py\", line 529, in __getitem__
    ] pypdf/_page.py:529 in __getitem__
ImportError: cannot import name 'Image' from 'PIL' (unknown location)
";
        assert_eq!(classify_provider_failure(text), None, "{text}");
    }

    /// 決定に挙げた「ステータスの文脈」の形はすべて Throttled/AuthFailed に一致する。
    #[test]
    fn status_context_forms_match() {
        for text in ["HTTP 529 received", "status 529", "529 Too Many Requests", "Error 529", "got (529) back"] {
            assert_eq!(
                classify_provider_failure(text),
                Some(ProviderFailure::Throttled { retry_after_secs: 60 }),
                "{text}"
            );
        }
    }

    /// 位置情報でも数字が単独で残らない限り誤って一致しない: `[\w/.-]+\.\w{1,5}:\d+` は拡張子を問わず
    /// 汎用に無視する。
    #[test]
    fn generic_file_extensions_before_line_numbers_are_ignored() {
        for text in [
            "at foo/bar.rs:529:1",
            "in module.go:401",
            "see script.rb:429 for details",
            "src/main.ts:529: unexpected token",
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
