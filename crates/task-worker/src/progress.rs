//! ADR-0048 D2（Phase 60a）: ワーカーの進行の正規化に使う、アダプタ共通の小道具。
//!
//! 「どのイベントがどの `kind` か」という**写像はアダプタごと**にある（`claude_code` / `codex` / `acp` /
//! `paperqa` / `local_deep_research`）。ここに置くのは、その写像が使う決定的で短い関数だけで、
//! 判断も I/O も LLM も無い。

use task_core::{ProgressFields, ProgressKind};

/// `summary` の上限（文字数）。`tool_use` の入力の 1 行要約。
pub(crate) const SUMMARY_MAX_CHARS: usize = 120;
/// `tool_result` の要約の上限（文字数）。
pub(crate) const RESULT_MAX_CHARS: usize = 200;

/// 文字数で切る（UTF-8 の境界を守る。切ったら末尾に `…`）。
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

/// アダプタの節目（`kind = status`）。
pub(crate) fn status() -> ProgressFields {
    ProgressFields::of(ProgressKind::Status)
}

/// 節目を 1 件出す（`paperqa` / `local-deep-research` はこれだけを使う。ADR-0048 D2）。
/// `msg` は従来どおりそのまま、`summary` は 1 行に畳んだもの。
pub(crate) fn emit_status(sink: &dyn crate::adapter::EventSink, msg: &str) {
    let summary = truncate_chars(&one_line(msg), SUMMARY_MAX_CHARS);
    sink.progress_with(msg, &status().with_summary(summary));
}

/// モデルの発話（`kind = text`）。`summary` は 1 行に畳んだ先頭、`detail` は本文そのもの（4 KiB）。
pub(crate) fn text(body: &str) -> ProgressFields {
    ProgressFields::of(ProgressKind::Text)
        .with_summary(truncate_chars(&one_line(body), SUMMARY_MAX_CHARS))
        .with_detail(body)
}

/// 道具を使った（`kind = tool_use`）。`summary` は入力の 1 行要約、`detail` は入力そのもの（4 KiB で切る）。
pub(crate) fn tool_use(tool: &str, input: Option<&serde_json::Value>) -> ProgressFields {
    let fields = ProgressFields::of(ProgressKind::ToolUse)
        .with_tool(tool)
        .with_summary(tool_input_summary(tool, input));
    match input {
        Some(v) => fields.with_detail(v.to_string()),
        None => fields,
    }
}

/// 道具の結果（`kind = tool_result`）。`summary` は先頭 200 文字、`detail` は本文（4 KiB で切る）。
pub(crate) fn tool_result(tool: Option<&str>, body: &str, error: bool) -> ProgressFields {
    let mut fields = ProgressFields::of(ProgressKind::ToolResult)
        .with_summary(truncate_chars(body, RESULT_MAX_CHARS))
        .with_error(error);
    if let Some(tool) = tool {
        fields = fields.with_tool(tool);
    }
    fields.with_detail(body)
}

/// 思考（`kind = thinking`）。**要約だけ**を残し、本文（`detail`）は流さない（ADR-0048 D2）。
pub(crate) fn thinking(body: &str) -> ProgressFields {
    ProgressFields::of(ProgressKind::Thinking).with_summary(truncate_chars(body, SUMMARY_MAX_CHARS))
}

/// 道具の入力の 1 行要約（ADR-0048 D2）。道具ごとに「人が見て分かる 1 つの値」を選ぶ:
/// `Bash` はコマンド、`Read` / `Write` / `Edit` はパス、`Grep` / `Glob` は模様、それ以外は入力の先頭。
/// 決まった鍵が無ければ入力そのものの先頭に落ちる（未知の道具でも何かは出る）。
pub(crate) fn tool_input_summary(tool: &str, input: Option<&serde_json::Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    let pick = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
            .map(str::to_string)
    };
    let chosen = match tool {
        "Bash" | "BashOutput" => pick(&["command"]),
        "Read" | "Write" | "Edit" | "NotebookEdit" => pick(&["file_path", "path", "notebook_path"]),
        "Grep" | "Glob" => pick(&["pattern"]),
        _ => None,
    };
    let summary = chosen.unwrap_or_else(|| input.to_string());
    truncate_chars(one_line(&summary).trim(), SUMMARY_MAX_CHARS)
}

/// 改行と連続する空白を 1 つの空白に畳む（Console の 1 行見出しに載せるため）。
pub(crate) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_characters_and_keeps_utf8_boundaries() {
        assert_eq!(truncate_chars("あいうえお", 5), "あいうえお");
        assert_eq!(truncate_chars("あいうえお", 3), "あいう…");
        assert_eq!(truncate_chars("", 3), "");
    }

    /// ADR-0048 D2: 道具ごとに要約に使う鍵が決まっている。
    #[test]
    fn the_summary_picks_the_one_value_a_human_reads() {
        let input =
            serde_json::json!({"command": "cargo test --workspace", "description": "run tests"});
        assert_eq!(
            tool_input_summary("Bash", Some(&input)),
            "cargo test --workspace"
        );
        let input = serde_json::json!({"file_path": "/x/y.rs", "offset": 1});
        assert_eq!(tool_input_summary("Read", Some(&input)), "/x/y.rs");
        let input = serde_json::json!({"pattern": "fn main", "path": "crates"});
        assert_eq!(tool_input_summary("Grep", Some(&input)), "fn main");
        // 知らない道具は入力そのものの先頭（1 行に畳む）。
        let input = serde_json::json!({"a": 1});
        assert_eq!(tool_input_summary("Whatever", Some(&input)), r#"{"a":1}"#);
        assert_eq!(tool_input_summary("Bash", None), "");
        // 改行は畳む。
        let input = serde_json::json!({"command": "a\n  b"});
        assert_eq!(tool_input_summary("Bash", Some(&input)), "a b");
    }

    /// `detail` は 4 KiB で切られ、切ったら `truncated`。`thinking` は `detail` を持たない。
    #[test]
    fn details_are_capped_and_thinking_carries_only_a_summary() {
        let long = "x".repeat(task_core::PROGRESS_DETAIL_MAX_BYTES + 10);
        let fields = tool_result(Some("Bash"), &long, true);
        assert_eq!(fields.kind, Some(ProgressKind::ToolResult));
        assert!(fields.error);
        assert_eq!(
            fields.summary.as_deref().map(|s| s.chars().count()),
            Some(RESULT_MAX_CHARS + 1)
        );
        assert_eq!(
            fields.detail.as_deref().map(str::len),
            Some(task_core::PROGRESS_DETAIL_MAX_BYTES)
        );
        assert!(fields.truncated);
        let fields = thinking("考えている");
        assert_eq!(fields.kind, Some(ProgressKind::Thinking));
        assert_eq!(fields.summary.as_deref(), Some("考えている"));
        assert!(fields.detail.is_none());
        assert!(!status().is_plain() && status().kind == Some(ProgressKind::Status));
    }
}
