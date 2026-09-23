//! ADR-0063 Phase 109c A: 調査タスクの「対象」と「観点」を目的文から決定的に取り出す（LLM は使わない）。
//!
//! 文献調査（PaperQA）と Web 調査（LDR）の両方が、対象ごとに整理された回答を組み立てるために使う
//! 純関数。目的文の書き方に強く依存するので、取れなければ空の `Vec` を返し、呼び出し側は
//! 「対象ごとの整理」を諦めて従来どおり 1 本の問いに退化する。

use std::collections::BTreeSet;

/// 観点の既定リスト（deployment model: server/client 配置、core/thread 利用、cache/direct I/O、
/// file semantics、replication、data path、目的・semantics。ADR-0063 Phase 109c A）。
pub const DEFAULT_ASPECTS: &[&str] = &[
    "server/client 配置",
    "core/thread 利用",
    "cache/direct I/O",
    "file semantics",
    "replication",
    "data path",
    "目的・semantics",
];

/// `http(s)://` の URL を空白に置き換える（決定的。`paperqa.rs::extract_urls` と同じ境界文字）。
/// URL のパス区切り（`/`）が「対象の列挙」と誤認されるのを防ぐ（`github.com/otatebe/chfs` が
/// `com`/`otatebe`/`chfs` という対象列に化けた実測の事故、Phase 109c）。
fn strip_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = rest
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            '「' | '」' | '（' | '）' | '(' | ')' | '<' | '>' | '"' | '\'' | '　'
                        )
                })
                .unwrap_or(rest.len());
            out.push(' ');
            i += end;
        } else {
            let ch = rest.chars().next().expect("non-empty rest");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// 目的文（と `inputs`）から対象（targets）の一覧を取り出す。
///
/// `/` や `、`・`・`・`，` で区切られた**固有名詞の列**（英数字の連なりが 2 つ以上、区切り文字だけで
/// 隣り合っているもの）を対象の列挙とみなす。「等」「など」のような接尾語は非 ASCII なので識別子の
/// 連なりに入らず、自然に落ちる。URL は先に取り除く（パス区切りの `/` を対象の列挙と誤認しないため）。
/// それらしい列が無ければ空を返す。
pub fn research_targets(objective: &str) -> Vec<String> {
    fn is_ident_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    }
    fn is_join_char(c: char) -> bool {
        matches!(c, '/' | '、' | '・' | '，')
    }

    let without_urls = strip_urls(objective);
    let chars: Vec<char> = without_urls.chars().collect();
    let n = chars.len();

    // 1. 英数字（+ `-`/`_`）の連なりを拾う。2 文字未満は雑音として捨てる（`I/O` の `I`/`O` など）。
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if is_ident_char(chars[i]) {
            let start = i;
            while i < n && is_ident_char(chars[i]) {
                i += 1;
            }
            if i - start >= 2 {
                runs.push((start, i));
            }
        } else {
            i += 1;
        }
    }
    if runs.len() < 2 {
        return Vec::new();
    }

    // 2. 隣り合う連なりが区切り文字だけで繋がっているか（間に空白や他の文字が無いか）。
    let linked: Vec<bool> = (0..runs.len() - 1)
        .map(|k| {
            let (_, end) = runs[k];
            let (next_start, _) = runs[k + 1];
            next_start > end && chars[end..next_start].iter().copied().all(is_join_char)
        })
        .collect();

    // 3. 繋がっている連なりの最長の鎖を対象列とする（同点なら出現順で最初のもの）。
    let mut best: Vec<usize> = Vec::new();
    let mut current: Vec<usize> = vec![0];
    for (k, &is_linked) in linked.iter().enumerate() {
        if is_linked {
            current.push(k + 1);
        } else {
            if current.len() > best.len() {
                best = std::mem::take(&mut current);
            }
            current = vec![k + 1];
        }
    }
    if current.len() > best.len() {
        best = current;
    }
    if best.len() < 2 {
        return Vec::new();
    }

    let mut seen = BTreeSet::new();
    best.into_iter()
        .map(|k| chars[runs[k].0..runs[k].1].iter().collect::<String>())
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

/// 目的文の括弧書きの列挙（`(a、b、c)` / `（a、b、c）`）があればそれを観点として返す。
/// 無ければ既定リスト（`DEFAULT_ASPECTS`）。
pub fn research_aspects(objective: &str) -> Vec<String> {
    if let Some(items) = extract_parenthesized_list(objective) {
        return items;
    }
    DEFAULT_ASPECTS.iter().map(|s| s.to_string()).collect()
}

/// 半角/全角の丸括弧の中身を `、`/`,`/`，` で割った列挙（2 件以上あるものだけ）。最初に見つかったものを返す。
fn extract_parenthesized_list(text: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '(' | '（') {
            let close = if chars[i] == '(' { ')' } else { '）' };
            if let Some(offset) = chars[i + 1..].iter().position(|&c| c == close) {
                let inner: String = chars[i + 1..i + 1 + offset].iter().collect();
                let items: Vec<String> = inner
                    .split(['、', ',', '，'])
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if items.len() >= 2 {
                    return Some(items);
                }
                i = i + 1 + offset + 1;
                continue;
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_slash_separated_list_of_proper_nouns() {
        let objective = "CHFS/FINCHFS/GekkoFS/UnifyFS/BeeOND のデプロイモデルを比較調査する。";
        assert_eq!(
            research_targets(objective),
            vec!["CHFS", "FINCHFS", "GekkoFS", "UnifyFS", "BeeOND"]
        );
    }

    #[test]
    fn drops_a_trailing_etc_suffix_since_it_is_not_ascii() {
        let objective = "CHFS、FINCHFS、GekkoFS など、主要な ad-hoc HPC ファイルシステムを調べる。";
        assert_eq!(
            research_targets(objective),
            vec!["CHFS", "FINCHFS", "GekkoFS"]
        );
    }

    #[test]
    fn accepts_a_mixed_separator_style() {
        let objective = "Lustre・BeeGFS・WekaFS の一次情報を確認する。";
        assert_eq!(research_targets(objective), vec!["Lustre", "BeeGFS", "WekaFS"]);
    }

    #[test]
    fn does_not_mistake_a_url_path_for_a_target_list() {
        // 実測の事故（Phase 109c）: `github.com/otatebe/chfs` の `/` がパス区切りなのに
        // 対象の列挙（`com`/`otatebe`/`chfs`）と誤認されていた。1 件しか対象が無い目的文なので
        // 空（列挙にならない）。
        let objective = "CHFS（https://github.com/otatebe/chfs）を調べる";
        assert!(research_targets(objective).is_empty(), "{:?}", research_targets(objective));
    }

    #[test]
    fn a_real_target_list_survives_next_to_a_url() {
        let objective =
            "CHFS/FINCHFS を、それぞれの GitHub（https://github.com/otatebe/chfs）も見て調べる";
        assert_eq!(research_targets(objective), vec!["CHFS", "FINCHFS"]);
    }

    #[test]
    fn returns_empty_without_a_delimited_list() {
        assert!(research_targets("BenchFS の設計方針を調べる。").is_empty());
        assert!(research_targets("").is_empty());
    }

    #[test]
    fn a_single_short_token_pair_like_io_is_not_mistaken_for_a_target_list() {
        // `I/O` の `I`/`O` は 1 文字なので連なりとして数えない。
        assert!(research_targets("非同期 I/O ランタイムを調べる。").is_empty());
    }

    #[test]
    fn keeps_the_first_occurrence_when_two_lists_tie_in_length() {
        // 先に出てくる鎖（長さ 2）が採用される。
        let objective = "AA/BB を先に見て、その後 CC/DD も見る。";
        assert_eq!(research_targets(objective), vec!["AA", "BB"]);
    }

    #[test]
    fn aspects_prefer_a_parenthesized_list_in_the_objective() {
        let objective =
            "CHFS/FINCHFS のデプロイモデルを比較する（server/client 配置、cache 方式）。";
        assert_eq!(
            research_aspects(objective),
            vec!["server/client 配置", "cache 方式"]
        );
    }

    #[test]
    fn aspects_fall_back_to_the_default_list_without_parens() {
        assert_eq!(
            research_aspects("CHFS/FINCHFS を比較する。"),
            DEFAULT_ASPECTS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn aspects_ignore_a_single_item_parenthesized_note() {
        // 括弧の中身が 1 件だけなら列挙とみなさず既定リストに落ちる。
        assert_eq!(
            research_aspects("CHFS/FINCHFS を比較する（詳細は省略）。"),
            DEFAULT_ASPECTS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }
}
