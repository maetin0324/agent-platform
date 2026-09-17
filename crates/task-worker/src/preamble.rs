//! プロンプトの前置きを 1 か所で組む（ADR-0033 D4 / D6 / D5、Phase 24 / 26）。
//!
//! 「人」らしさは**注入される記憶と brief** で作る（ADR-0033 D6）。ハーネスのプロセスは相変わらず
//! ステートレスで、状態はファイルと DB にある（DESIGN 原則 2）。ここは純粋関数だけで、I/O も LLM も無い。
//!
//! 並び（ADR-0033 D4 / Phase 24 の指示）:
//! 1. 役職と brief（`context.node`）
//! 2. 永続の認可（`context.standing_rules`。SPEC §3.6「永続の認可は文字で記録してエージェントに注入する」。
//!    Phase 26 が埋める: 担当宛て + 全員向け）
//! 3. 記憶（`context.memory`）
//! 4. 直近のやり取り（`context.conversation`）
//! 5. 役割の指示文（`context.role`。ADR-0016 D1 からある既存の節）
//! 6. 記憶の書き方の指示（記憶が有効な run にだけ）
//!
//! 検索ハーネス（`local-deep-research`）はこの前置きを**一切使わない**（ADR-0029 / ADR-0033 D6:
//! 検索に渡す問いを濁さないため。Phase 27 の監査 M-2）。
//!
//! `RunContext` が既定値（Phase 23 までの中身しか無い）のときの出力は、Phase 23 の
//! `claude_code::prompt_header` が出していた文字列と**バイト単位で同じ**になる（既存テストがそれを見る）。

use task_core::MessageRole;

use crate::protocol::RunContext;

/// 前置き（役割の指示文を含む）。`claude-code` / `codex` / `acp` / `paperqa` が使う。
pub fn render(context: &RunContext) -> String {
    let mut out = person_sections(context);
    out.push_str(&role_section(context));
    out.push_str(&memory_instructions(context));
    out
}

/// 1〜4（役職と brief → 永続の認可 → 記憶 → 直近のやり取り）。
fn person_sections(context: &RunContext) -> String {
    let mut out = String::new();
    if let Some(node) = &context.node {
        out.push_str(&format!("## あなた: {} ({})\n", node.name, node.id));
        if !node.brief.is_empty() {
            out.push_str(&node.brief);
            out.push('\n');
        }
        out.push('\n');
    }
    if !context.standing_rules.is_empty() {
        out.push_str("## 永続の認可（人が『今後ずっと』と決めたこと）\n");
        for rule in &context.standing_rules {
            out.push_str(&format!("- {rule}\n"));
        }
        out.push('\n');
    }
    if let Some(memory) = &context.memory
        && (!memory.notes.is_empty() || !memory.project.is_empty())
    {
        out.push_str("## 覚えていること (your long-term memory)\n");
        if !memory.notes.is_empty() {
            out.push_str("### 案件をまたぐ記憶\n");
            out.push_str(memory.notes.trim_end());
            out.push_str("\n\n");
        }
        if !memory.project.is_empty() {
            out.push_str("### この案件について\n");
            out.push_str(memory.project.trim_end());
            out.push_str("\n\n");
        }
    }
    if !context.conversation.is_empty() {
        out.push_str("## 直近のやり取り (this is a continuing conversation)\n");
        for turn in &context.conversation {
            let who = match turn.role {
                MessageRole::User => "人",
                MessageRole::Node => "あなた",
            };
            out.push_str(&format!("- {who}: {}\n", one_line(&turn.text)));
        }
        out.push('\n');
    }
    out
}

/// 5. 役割の指示文（ADR-0016 D1 / M3。Phase 23 までと同じ文面）。
fn role_section(context: &RunContext) -> String {
    let mut out = String::new();
    if let Some(role) = &context.role {
        out.push_str(&format!("## Role: {}\n", role.id));
        if !role.instructions.is_empty() {
            out.push_str(&role.instructions);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// 6. 記憶の書き方（ADR-0033 D6）。記憶が有効な run（`context.memory` がある）にだけ出す。
fn memory_instructions(context: &RunContext) -> String {
    if context.memory.is_none() {
        return String::new();
    }
    "## 覚えておくこと (how to write to your memory)\n\
     覚えておくべきこと（クラスタの使い方、人の好み、直近の相談）は `artifacts/result.json` の \
     `memory.notes` に、この案件だけの事は `memory.project` に、短い箇条書きの文字列の配列で返せ: \
     `{\"summary\": \"…\", \"evidence\": [], \"memory\": {\"notes\": [\"…\"], \"project\": [\"…\"]}}`。\
     覚えることが無ければ `memory` は書かなくてよい（空の配列でもよい）。ここに書いたものだけが次の run に \
     引き継がれる（この会話の他の部分は残らない）。\n\n"
        .to_string()
}

/// やり取りの 1 行化（前置きの箇条書きを崩さないため。中身は削らない）。
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ConversationTurn, MemoryContext, NodeContext, RoleContext};

    fn full_context() -> RunContext {
        RunContext {
            node: Some(NodeContext {
                id: "research-survey".into(),
                name: "関連研究調査課".into(),
                brief: "関連研究を洗い、先行研究との差分を言語化する。".into(),
            }),
            standing_rules: vec!["pegasus のジョブは常に 1 ノードで始めてよい".into()],
            memory: Some(MemoryContext {
                notes: "- 2026-09-10: pegasus は pjsub で投げる".into(),
                project: "- 2026-09-16: Pluvio は非同期ランタイム基盤".into(),
            }),
            conversation: vec![
                ConversationTurn { role: MessageRole::User, text: "先週の続きを\nお願い".into() },
                ConversationTurn { role: MessageRole::Node, text: "承知しました".into() },
            ],
            role: Some(RoleContext {
                id: "literature-reader".into(),
                instructions: "あなたは精読担当。".into(),
            }),
            ..RunContext::default()
        }
    }

    /// ADR-0033 D4 / Phase 24: 並びは 役職と brief → 永続の認可 → 記憶 → 直近のやり取り → 役割の指示文。
    #[test]
    fn the_sections_come_in_the_order_the_adr_asks_for() {
        let out = render(&full_context());
        let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("missing {needle:?} in:\n{out}"));
        assert!(at("## あなた: 関連研究調査課 (research-survey)") < at("## 永続の認可"));
        assert!(at("## 永続の認可") < at("## 覚えていること"));
        assert!(at("## 覚えていること") < at("## 直近のやり取り"));
        assert!(at("## 直近のやり取り") < at("## Role: literature-reader"));
        assert!(at("## Role: literature-reader") < at("## 覚えておくこと"));
        // 中身
        assert!(out.contains("関連研究を洗い"));
        assert!(out.contains("- pegasus のジョブは常に 1 ノードで始めてよい"));
        assert!(out.contains("### 案件をまたぐ記憶"));
        assert!(out.contains("### この案件について"));
        assert!(out.contains("- 人: 先週の続きを お願い"), "{out}");
        assert!(out.contains("- あなた: 承知しました"));
        assert!(out.contains("memory.notes"));
    }

    /// 空の `RunContext` では前置きは空文字（Phase 23 までの出力と 1 バイトも変わらない）。
    #[test]
    fn an_empty_context_renders_nothing_at_all() {
        assert_eq!(render(&RunContext::default()), "");
    }

    /// 役割だけがあるときは、Phase 23 の `prompt_header` と同じ `## Role:` 節だけを出す。
    #[test]
    fn a_role_only_context_renders_exactly_the_old_role_section() {
        let with_instructions = RunContext {
            role: Some(RoleContext { id: "lead".into(), instructions: "You coordinate.".into() }),
            ..RunContext::default()
        };
        assert_eq!(render(&with_instructions), "## Role: lead\nYou coordinate.\n\n");
        let bare = RunContext {
            role: Some(RoleContext { id: "lead".into(), instructions: String::new() }),
            ..RunContext::default()
        };
        assert_eq!(render(&bare), "## Role: lead\n\n");
    }

    /// 記憶が空（ファイルが無い）なら記憶の節は出ないが、書き方の指示は出る（次から覚えられるように）。
    #[test]
    fn empty_memory_shows_no_memory_section_but_still_explains_how_to_write_it() {
        let context = RunContext {
            memory: Some(MemoryContext::default()),
            ..RunContext::default()
        };
        let out = render(&context);
        assert!(!out.contains("## 覚えていること"), "{out}");
        assert!(out.contains("## 覚えておくこと"), "{out}");
        // `[memory]` を設定していない run には何も出ない。
        assert!(!render(&RunContext::default()).contains("覚えておくこと"));
    }
}
