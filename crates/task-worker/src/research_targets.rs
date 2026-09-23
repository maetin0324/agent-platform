//! ADR-0063 Phase 109c A: 調査タスクの「対象」と「観点」を目的文から決定的に取り出す（LLM は使わない）。
//!
//! 文献調査（PaperQA）と Web 調査（LDR）の両方が、対象ごとに整理された回答を組み立てるために使う
//! 純関数。目的文の書き方に強く依存するので、取れなければ空の `Vec` を返し、呼び出し側は
//! 「対象ごとの整理」を諦めて従来どおり 1 本の問いに退化する。
//!
//! Phase 109f: 本番で目的文の末尾に人が足した節見出し「## 方針（人の指定、2026-09-23）」の括弧内が
//! 観点の列挙と誤認された（`research.json.aspects` が `["人の指定", "2026-09-23"]` になった）事故の
//! 修正。対象・観点の抽出は**目的文の最初の段落**（`\n\n` または `## ` 見出しより前）だけを見るように
//! 制限した。

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

/// 観点の候補として許す最大文字数。Phase 109f: 仕様上の目安は「10 文字を超える文」だが、実際に
/// 観点として通したい複合語（例: `deployment model` 16 文字、`server/core利用` 13 文字）がこれを
/// 超えるため、値そのものは緩めている（丸ごと混入したプロセの文を弾くのが目的で、複合語の観点を
/// 落とさないことを優先。`docs/adr/0063-research-tasks-resilience.md` Phase 109f 追記参照）。
const MAX_ASPECT_CANDIDATE_CHARS: usize = 24;

/// 観点として使えない語（人による付記や日付そのものを指す語。実際の事故: 「## 方針（人の指定、
/// 2026-09-23）」の括弧内が観点と誤認された、Phase 109f）。
const ASPECT_DENYLIST: &[&str] = &["人の指定", "人が指定", "指定", "日付"];

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

/// 「(必要なら…)」「(optional …)」「（任意…）」の丸括弧を丸ごと取り除く（対象の列挙の抽出前。
/// Phase 109f）。中身を対象として拾わないためで、かつ削除跡には何も挿入しない（
/// `BeeGFS-on-demand(必要ならDAOS/Lustre)、Mochi-Margo-Mercury` のように、括弧の外にある区切り文字
/// （`、`）で対象の列挙が途切れず繋がるようにするため）。
fn strip_optional_parens(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '(' | '（') {
            let close = if chars[i] == '(' { ')' } else { '）' };
            if let Some(offset) = chars[i + 1..].iter().position(|&c| c == close) {
                let inner: String = chars[i + 1..i + 1 + offset].iter().collect();
                let trimmed = inner.trim_start();
                let looks_optional = trimmed.starts_with("必要なら")
                    || trimmed.starts_with("任意")
                    || trimmed.to_ascii_lowercase().starts_with("optional");
                if looks_optional {
                    i = i + 1 + offset + 1; // 開き括弧〜閉じ括弧をまるごと読み飛ばす
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 半角/全角どちらかの丸括弧の開始位置を、対応する閉じ括弧の直後の位置とともに返す。
/// `text` の中に見出しの区切り（`\n\n` または `\n## `）があれば、そこより前だけを対象文とみなす
/// （Phase 109f: 目的文の最初の段落だけから対象・観点を取る）。
fn first_paragraph(text: &str) -> &str {
    let blank_pos = text.find("\n\n");
    let heading_pos = if let Some(stripped) = text.strip_prefix("## ") {
        let _ = stripped;
        Some(0)
    } else {
        text.find("\n## ").map(|i| i + 1)
    };
    match (blank_pos, heading_pos) {
        (Some(b), Some(h)) => &text[..b.min(h)],
        (Some(b), None) => &text[..b],
        (None, Some(h)) => &text[..h],
        (None, None) => text,
    }
}

/// 目的文（と `inputs`）から対象（targets）の一覧を取り出す。**最初の段落だけを見る**（Phase 109f）。
///
/// 明示の「対象:」（Phase 109f: `find_after_marker` 経由、最優先）が無ければ、`/` や
/// `、`・`・`・`，` で区切られた**固有名詞の列**（英数字の連なりが 2 つ以上、区切り文字だけで
/// 隣り合っているもの）を対象の列挙とみなす。「等」「など」のような接尾語は非 ASCII なので識別子の
/// 連なりに入らず、自然に落ちる。URL と「(必要なら…)」のような任意扱いの括弧は先に取り除く。それらしい
/// 列が無ければ空を返す。
pub fn research_targets(objective: &str) -> Vec<String> {
    let scope = first_paragraph(objective);
    if let Some(marker_targets) = extract_marker_targets(scope) {
        return marker_targets;
    }
    extract_target_chain(scope)
}

/// 明示の「対象:」/「対象：」行があれば最優先でそこから対象を取る（Phase 109c の `preamble.rs` の
/// 指示形式に対応。Phase 109f で追加）。行内の丸括弧（観点のラベルなど）はまるごと取り除いてから、
/// `/`/` / `/`、`/`,` のどれで区切られていても同じ結果になるよう空白を正規化する（実測の事故: 本番で
/// 「対象: CHFS / FINCHFS / … / io_uring（観点: …）」を投げたところ、`/` の前後の空白のせいで列挙が
/// 繋がらず、代わりに観点の中の `server/core` がそれっぽい鎖として拾われて `targets` が
/// `["model", "server", "core"]` になった、Phase 109f 追記）。マーカーが無い、または列挙が 2 件に
/// 満たなければ `None`。
fn extract_marker_targets(scope: &str) -> Option<Vec<String>> {
    let after = find_after_marker(scope, &["対象:", "対象："])?;
    let stop = after.find(['。', '\n']).unwrap_or(after.len());
    let raw_segment = &after[..stop];
    let without_parens = strip_all_parens(raw_segment);
    let normalized = collapse_whitespace_around_join_chars(without_parens.trim());
    let chain = extract_target_chain(&normalized);
    if chain.len() >= 2 {
        Some(chain)
    } else {
        None
    }
}

/// `(...)`/`（...）` を中身ごとまるごと 1 個の空白に置き換える（`strip_optional_parens` と違い、
/// 中身は問わない）。空白に置き換える（消してしまわない）のは、括弧の直後が識別子でない文字（例:
/// 「（観点: …）を調べる」の「を」）だと、括弧を消しただけではその文字と直前の識別子が区切りなしで
/// 繋がってしまい、複合語とみなす判定（`is_glued_to_non_ascii_word`）に誤って引っかかるため
/// （実測の回帰: 「対象: Alpha/Beta（観点: …）を調べる」で `Beta` が「betaを」の複合語扱いになり
/// 落ちた、Phase 109f）。置き換えた空白は `collapse_whitespace_around_join_chars` が区切り文字の
/// 前後なら畳んで消すので、対象の列挙の途中に付いている括弧（`preamble.rs` の明示形は使わないが
/// 念のため）でも列挙は繋がったままになる。
fn strip_all_parens(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '(' | '（') {
            let close = if chars[i] == '(' { ')' } else { '）' };
            if let Some(offset) = chars[i + 1..].iter().position(|&c| c == close) {
                out.push(' ');
                i = i + 1 + offset + 1; // 開き括弧〜閉じ括弧をまるごと読み飛ばす
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 区切り文字（`/`・`、`・`・`・`，`・`,`）の直前・直後の空白を取り除く。それ以外の空白（区切り文字に
/// 隣接しないもの）は 1 個の半角スペースに畳んで残す（単なる語の区切りとして、鎖を切る役目は保つ）。
/// `対象: A / B / C` のように区切りの前後に空白が入っていても `A/B/C` と同じ結果になるようにする
/// （Phase 109f。実測の事故の直し）。
fn collapse_whitespace_around_join_chars(text: &str) -> String {
    const JOIN_CHARS: [char; 5] = ['/', '、', '・', '，', ','];
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let next_is_join = chars.get(j).is_some_and(|c| JOIN_CHARS.contains(c));
            let prev_is_join = out.last().is_some_and(|c| JOIN_CHARS.contains(c));
            if !(next_is_join || prev_is_join) {
                out.push(' ');
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out.into_iter().collect()
}

/// `markers` のいずれかが最初に現れる位置の直後から末尾までを返す。
fn find_after_marker<'a>(text: &'a str, markers: &[&str]) -> Option<&'a str> {
    markers
        .iter()
        .filter_map(|m| text.find(m).map(|pos| (pos, m.len())))
        .min_by_key(|&(pos, _)| pos)
        .map(|(pos, len)| &text[pos + len..])
}

/// 対象の列挙を抜く本体（`research_targets`/`extract_marker_targets` から呼ばれる純関数）。
fn extract_target_chain(text: &str) -> Vec<String> {
    fn is_ident_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    }
    fn is_join_char(c: char) -> bool {
        matches!(c, '/' | '、' | '・' | '，' | ',')
    }
    // 識別子の直後に区切りなしで非 ASCII の文字（漢字・かな）が続く場合、それは列挙の一員ではなく
    // 日本語の複合語の一部（例: `RDMA統合` の `RDMA`）とみなして落とす（実測: `io_uring・RDMA統合` の
    // `RDMA` が対象に化けた、Phase 109f）。
    fn is_glued_to_non_ascii_word(chars: &[char], end: usize) -> bool {
        chars
            .get(end)
            .is_some_and(|c| c.is_alphabetic() && !c.is_ascii())
    }

    let without_optional_parens = strip_optional_parens(text);
    let without_urls = strip_urls(&without_optional_parens);
    let chars: Vec<char> = without_urls.chars().collect();
    let n = chars.len();

    // 1. 英数字（+ `-`/`_`）の連なりを拾う。2 文字未満は雑音として捨てる（`I/O` の `I`/`O` など）。
    //    直後に区切りなしで非 ASCII の文字が続く連なりも、複合語の一部として捨てる。
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if is_ident_char(chars[i]) {
            let start = i;
            while i < n && is_ident_char(chars[i]) {
                i += 1;
            }
            if i - start >= 2 && !is_glued_to_non_ascii_word(&chars, i) {
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

/// 目的文から観点の一覧を取り出す。**最初の段落だけを見る**（Phase 109f。以降の節と、その見出しの
/// 括弧は見ない）。優先順位: (1) 明示の「観点:」/「観点：」マーカー、(2) 括弧書きの列挙
/// `(a、b、c)`/`（a、b、c）`、(3) 「各…の A・B・C を整理」の形。いずれも 2 件に満たなければ既定リスト
/// （`DEFAULT_ASPECTS`）に落ちる。取れた候補は日付・付記の語・長すぎる文・URL・1 文字を弾く妥当性
/// チェックを通す（`is_valid_aspect_candidate`）。
pub fn research_aspects(objective: &str) -> Vec<String> {
    let scope = first_paragraph(objective);

    if let Some(items) = extract_marker_aspects(scope) {
        let valid = filter_valid_aspects(items);
        if valid.len() >= 2 {
            return valid;
        }
    }
    if let Some(items) = extract_parenthesized_list(scope) {
        let valid = filter_valid_aspects(items);
        if valid.len() >= 2 {
            return valid;
        }
    }
    if let Some(items) = extract_enumerate_aspects(scope) {
        let valid = filter_valid_aspects(items);
        if valid.len() >= 2 {
            return valid;
        }
    }
    DEFAULT_ASPECTS.iter().map(|s| s.to_string()).collect()
}

/// 明示の「観点:」/「観点：」マーカー以降、次の区切り（丸括弧・句点・改行）までを候補列とする。
fn extract_marker_aspects(scope: &str) -> Option<Vec<String>> {
    let after = find_after_marker(scope, &["観点:", "観点："])?;
    let stop = after
        .find(['(', '（', ')', '）', '。', '\n'])
        .unwrap_or(after.len());
    let segment = &after[..stop];
    let items = split_by_best_delimiter(segment);
    if items.len() >= 2 {
        Some(items)
    } else {
        None
    }
}

/// 「各…の A・B・C を整理」の形（「を整理」の直前、それより前にある最後の「の」からの区間）を
/// 観点の列挙として拾う（実際の事故で観点が書かれていた形。Phase 109f）。
fn extract_enumerate_aspects(scope: &str) -> Option<Vec<String>> {
    let pos = scope.find("を整理")?;
    let before = &scope[..pos];
    let start = before.rfind('の').map(|i| i + 'の'.len_utf8())?;
    let segment = &before[start..];
    let items = split_by_best_delimiter(segment);
    if items.len() >= 2 {
        Some(items)
    } else {
        None
    }
}

/// `・`/`、`/`，`/`,`/`/` のうち、`text` に実際に含まれる最初のもの（この優先順）だけで割る。
/// 見つからなければ空（＝列挙とみなさない）。`/` を優先度最後にしているのは、`server/core利用` の
/// ような区切り文字ではなく識別子の一部として使われる `/` を、`・`/`、` が使われている列挙の中で
/// 誤って割ってしまわないため。
fn split_by_best_delimiter(text: &str) -> Vec<String> {
    const DELIMS: [char; 5] = ['・', '、', '，', ',', '/'];
    let Some(delim) = DELIMS.into_iter().find(|&d| text.contains(d)) else {
        return Vec::new();
    };
    text.split(delim)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 観点の候補として妥当か（日付・付記の語・長すぎる文・URL・1 文字を弾く。Phase 109f）。
fn is_valid_aspect_candidate(raw: &str) -> bool {
    let s = raw.trim();
    let count = s.chars().count();
    if !(2..=MAX_ASPECT_CANDIDATE_CHARS).contains(&count) {
        return false;
    }
    if s.contains("http://") || s.contains("https://") {
        return false;
    }
    if contains_iso_date(s) || contains_year_with_kanji(s) {
        return false;
    }
    if ASPECT_DENYLIST.contains(&s) {
        return false;
    }
    true
}

fn filter_valid_aspects(items: Vec<String>) -> Vec<String> {
    items
        .into_iter()
        .map(|s| strip_leaked_aspect_label(&s))
        .filter(|s| is_valid_aspect_candidate(s))
        .collect()
}

/// 観点の候補の先頭に「観点:」/「観点：」/「aspects:」のラベルが残っていれば剥がす（防御的。
/// `extract_marker_aspects` はマーカーより後ろだけを見るのでこの経路では起きないはずだが、
/// `extract_parenthesized_list`/`extract_enumerate_aspects` が将来ラベル付きの文字列を返す形に
/// 変わっても壊れないようにする。Phase 109f 追記）。
fn strip_leaked_aspect_label(s: &str) -> String {
    let trimmed = s.trim();
    for label in ["観点:", "観点：", "aspects:", "Aspects:", "ASPECTS:"] {
        if let Some(rest) = trimmed.strip_prefix(label) {
            return rest.trim().to_string();
        }
    }
    trimmed.to_string()
}

/// `\d{4}-\d{2}-\d{2}`（ISO 日付）が含まれるか。
fn contains_iso_date(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n < 10 {
        return false;
    }
    (0..=n - 10).any(|i| {
        chars[i..i + 4].iter().all(char::is_ascii_digit)
            && chars[i + 4] == '-'
            && chars[i + 5..i + 7].iter().all(char::is_ascii_digit)
            && chars[i + 7] == '-'
            && chars[i + 8..i + 10].iter().all(char::is_ascii_digit)
    })
}

/// `\d{4}年`（和暦ではなく西暦の「年」表記）が含まれるか。
fn contains_year_with_kanji(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    if n < 5 {
        return false;
    }
    (0..=n - 5).any(|i| chars[i..i + 4].iter().all(char::is_ascii_digit) && chars[i + 4] == '年')
}

/// 目的文から比較先（「<対象>と比較」「<対象>との比較」）を決定的に取り出す（ADR-0063 Phase 109d C3。
/// LLM は使わない）。PaperQA の対象ごとの問いに「対象ごとに <比較先> と『公平比較可能』か『背景比較のみ』
/// かを分類せよ」という総括の問いを 1 本足すかどうかを決めるのに使う。見つからなければ `None`。
/// Phase 109f: 変更なし（目的文全体を見る。最初の段落に制限しない）。
pub fn comparison_target(objective: &str) -> Option<String> {
    fn is_ident_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    }
    let chars: Vec<char> = objective.chars().collect();
    for needle in ["との比較", "と比較"] {
        let needle_chars: Vec<char> = needle.chars().collect();
        if let Some(pos) = find_subsequence(&chars, &needle_chars) {
            // 「BenchFS と比較」のように識別子と「と」の間に空白が挟まることがある。
            let mut end = pos;
            while end > 0 && chars[end - 1].is_whitespace() {
                end -= 1;
            }
            let mut start = end;
            while start > 0 && is_ident_char(chars[start - 1]) {
                start -= 1;
            }
            if start < end {
                let ident: String = chars[start..end].iter().collect();
                if ident.chars().count() >= 2 {
                    return Some(ident);
                }
            }
        }
    }
    None
}

/// ADR-0063 Phase 109g A: `comparison_target` が見つかった文（前の「。」の直後から次の「。」まで）を、
/// 知識ベースに比較先の設計条件を書いたページが無いときの、総括の問い（比較分類の判断）の最後の拠り所
/// として返す。目的文全体を見る（`comparison_target` と同じく最初の段落に制限しない）。決定的、LLM は
/// 使わない。比較の言い回しが無ければ `None`。
pub fn comparison_target_paragraph(objective: &str) -> Option<String> {
    let chars: Vec<char> = objective.chars().collect();
    for needle in ["との比較", "と比較"] {
        let needle_chars: Vec<char> = needle.chars().collect();
        if let Some(pos) = find_subsequence(&chars, &needle_chars) {
            let end_needle = pos + needle_chars.len();
            let start = chars[..pos]
                .iter()
                .rposition(|&c| c == '。')
                .map(|i| i + 1)
                .unwrap_or(0);
            let end = chars[end_needle..]
                .iter()
                .position(|&c| c == '。')
                .map(|i| end_needle + i + 1)
                .unwrap_or(chars.len());
            let sentence: String = chars[start..end].iter().collect();
            let trimmed = sentence.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn find_subsequence(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == *needle)
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
    fn comparison_target_finds_an_ascii_identifier_before_and_compare() {
        assert_eq!(
            comparison_target("CHFS/FINCHFS を BenchFS と比較する"),
            Some("BenchFS".to_string())
        );
        assert_eq!(
            comparison_target("CHFS/FINCHFS を BenchFS との比較で調べる"),
            Some("BenchFS".to_string())
        );
    }

    #[test]
    fn comparison_target_is_none_without_a_compare_phrase() {
        assert!(comparison_target("CHFS/FINCHFS を調べる").is_none());
        assert!(comparison_target("").is_none());
    }

    /// ADR-0063 Phase 109g A: 知識ベースに比較先のページが無いときの最後の拠り所（目的文中の比較の
    /// 言い回しを含む 1 文）。
    #[test]
    fn comparison_target_paragraph_returns_the_sentence_around_the_compare_phrase() {
        let objective =
            "CHFS/FINCHFS の学術文献を調査する。BenchFSとの比較が『公平比較可能』か『背景比較のみ』かを\
分類すること。既存knowledgeの設計と対比できるよう根拠付きで書くこと。";
        assert_eq!(
            comparison_target_paragraph(objective),
            Some(
                "BenchFSとの比較が『公平比較可能』か『背景比較のみ』かを分類すること。".to_string()
            )
        );
    }

    #[test]
    fn comparison_target_paragraph_is_none_without_a_compare_phrase() {
        assert!(comparison_target_paragraph("CHFS/FINCHFS を調べる。").is_none());
        assert!(comparison_target_paragraph("").is_none());
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

    /// Phase 109f: 実際に本番で使われた目的文（先頭段落）。`## 方針（人の指定、2026-09-23）` を
    /// 足す前後で targets/aspects が変わらないことを、この定数を共有する 2 つのテストで確認する。
    const REAL_OBJECTIVE_FIRST_PARAGRAPH: &str = "CHFS/FINCHFS/GekkoFS/UnifyFS/BeeOND/BeeGFS-on-demand(必要ならDAOS/Lustre)、Mochi-Margo-Mercury、UCX、io_uring・RDMA統合、low-overhead RPC・非同期runtime分野の近年の学術文献を調査する。各システムの目的・semantics・deployment model・server/core利用・data pathを整理し、BenchFSとの比較が『公平比較可能』か『背景比較のみ』かを分類すること。既存knowledge(projects/benchfs/architecture-overview.md)のPluvio/Locusta等の設計と対比できるよう根拠付きで書くこと。";

    const REAL_OBJECTIVE_WITH_HUMAN_APPENDED_SECTION: &str = "CHFS/FINCHFS/GekkoFS/UnifyFS/BeeOND/BeeGFS-on-demand(必要ならDAOS/Lustre)、Mochi-Margo-Mercury、UCX、io_uring・RDMA統合、low-overhead RPC・非同期runtime分野の近年の学術文献を調査する。各システムの目的・semantics・deployment model・server/core利用・data pathを整理し、BenchFSとの比較が『公平比較可能』か『背景比較のみ』かを分類すること。既存knowledge(projects/benchfs/architecture-overview.md)のPluvio/Locusta等の設計と対比できるよう根拠付きで書くこと。\n\n## 方針（人の指定、2026-09-23）\n本文が有料で取得できない論文は、アブストラクト（OpenAlex / arXiv / Semantic Scholar のメタデータ）まで確認できれば妥協し、「本文未取得・アブストのみ」と明記する。";

    fn expected_real_objective_targets() -> Vec<String> {
        [
            "CHFS",
            "FINCHFS",
            "GekkoFS",
            "UnifyFS",
            "BeeOND",
            "BeeGFS-on-demand",
            "Mochi-Margo-Mercury",
            "UCX",
            "io_uring",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn expected_real_objective_aspects() -> Vec<String> {
        [
            "目的",
            "semantics",
            "deployment model",
            "server/core利用",
            "data path",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn real_objective_targets_exclude_the_optional_paren_and_the_glued_rdma_word() {
        // 実際の事故（Phase 109f、run 01M37A3JMMFXVY30SWD4EZ01JN）: `DAOS`/`Lustre` は
        // 「(必要なら…)」の中なので対象から除く。`RDMA` は `RDMA統合` という複合語の一部で
        // 単独の対象ではないので除く（`io_uring` は残る）。`Pluvio`/`Locusta`（既存 knowledge との対比
        // の対象。目的文の対象列とは別物）は最初の段落の最長の鎖ではないので入らない。
        assert_eq!(
            research_targets(REAL_OBJECTIVE_FIRST_PARAGRAPH),
            expected_real_objective_targets()
        );
    }

    #[test]
    fn real_objective_aspects_come_from_the_enumerate_pattern_not_the_optional_paren() {
        assert_eq!(
            research_aspects(REAL_OBJECTIVE_FIRST_PARAGRAPH),
            expected_real_objective_aspects()
        );
    }

    #[test]
    fn a_human_appended_heading_with_a_date_does_not_change_targets_or_aspects() {
        // 実際の事故（Phase 109f）: 目的文の末尾に人が足した「## 方針（人の指定、2026-09-23）」の
        // 括弧内が観点の列挙と誤認され、`research.json.aspects` が `["人の指定", "2026-09-23"]` に
        // なった。最初の段落だけを見るようにしたので、この節が増えても targets/aspects は変わらない。
        assert_eq!(
            research_targets(REAL_OBJECTIVE_WITH_HUMAN_APPENDED_SECTION),
            expected_real_objective_targets()
        );
        assert_eq!(
            research_aspects(REAL_OBJECTIVE_WITH_HUMAN_APPENDED_SECTION),
            expected_real_objective_aspects()
        );
    }

    #[test]
    fn an_explicit_marker_form_for_both_targets_and_aspects_takes_priority() {
        let objective = "対象: Alpha/Beta（観点: latency、throughput、cost）を調べる。";
        assert_eq!(
            research_targets(objective),
            vec!["Alpha".to_string(), "Beta".to_string()]
        );
        assert_eq!(
            research_aspects(objective),
            vec![
                "latency".to_string(),
                "throughput".to_string(),
                "cost".to_string()
            ]
        );
    }

    #[test]
    fn aspects_fall_back_to_default_when_only_a_date_and_a_meta_word_are_offered() {
        // 妥当性チェック単体の確認: 日付と付記の語だけの括弧書きは 2 件に満たない扱いになり、
        // 既定リストに落ちる。
        let objective = "CHFS/FINCHFS を比較する（人の指定、2026-09-23）。";
        assert_eq!(
            research_aspects(objective),
            DEFAULT_ASPECTS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    /// Phase 109f 追記（親からの追加観測、run `01M37AZ129EMB93N50MZ132S8K`）: 「対象:」の明示形でも、
    /// `/` の前後に空白が入る書き方（`preamble.rs` が例示する `CHFS / FINCHFS / …` の形そのもの）だと
    /// 区切りとして繋がらず、対象の列挙が空扱いになって一般走査にフォールバックし、代わりに観点の
    /// 括弧の中の `server/core` がそれっぽい鎖として拾われ `targets = ["model", "server", "core"]` に
    /// なっていた。観点も「観点: 目的」とラベルが残っていた。
    const PRODUCTION_MARKER_LINE: &str = "対象: CHFS / FINCHFS / GekkoFS / UnifyFS / BeeOND / \
Mochi-Margo-Mercury / UCX / io_uring（観点: 目的、file semantics、deployment model、\
server/core 利用、data path、BenchFS との比較分類）";

    fn expected_production_marker_targets() -> Vec<String> {
        [
            "CHFS",
            "FINCHFS",
            "GekkoFS",
            "UnifyFS",
            "BeeOND",
            "Mochi-Margo-Mercury",
            "UCX",
            "io_uring",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn expected_production_marker_aspects() -> Vec<String> {
        [
            "目的",
            "file semantics",
            "deployment model",
            "server/core 利用",
            "data path",
            "BenchFS との比較分類",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn production_marker_line_with_spaces_around_slashes_is_parsed_correctly() {
        assert_eq!(
            research_targets(PRODUCTION_MARKER_LINE),
            expected_production_marker_targets()
        );
        assert_eq!(
            research_aspects(PRODUCTION_MARKER_LINE),
            expected_production_marker_aspects()
        );
    }

    #[test]
    fn production_marker_line_is_unaffected_by_an_appended_human_section() {
        let objective = format!(
            "{PRODUCTION_MARKER_LINE}\n\n## 方針（人の指定、2026-09-23）\n本文が有料で取得できない\
論文は、アブストラクト（OpenAlex / arXiv / Semantic Scholar のメタデータ）まで確認できれば妥協し、\
「本文未取得・アブストのみ」と明記する。"
        );
        assert_eq!(
            research_targets(&objective),
            expected_production_marker_targets()
        );
        assert_eq!(
            research_aspects(&objective),
            expected_production_marker_aspects()
        );
    }
}
