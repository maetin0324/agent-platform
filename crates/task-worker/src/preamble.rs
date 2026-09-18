//! プロンプトの前置きを 1 か所で組む（ADR-0033 D4 / D6 / D5、Phase 24 / 26）。
//!
//! 「人」らしさは**注入される記憶と brief** で作る（ADR-0033 D6）。ハーネスのプロセスは相変わらず
//! ステートレスで、状態はファイルと DB にある（DESIGN 原則 2）。ここは純粋関数だけで、I/O も LLM も無い。
//!
//! 並び（ADR-0033 D4 / Phase 24 の指示。Phase 30 で 1 の直後に「仕事で使う道具」を追加。
//! Phase 33 で 3 の直後に「あなたの直近の仕事」を追加）:
//! 1. 役職と brief（`context.node`）＋ 対話 run で担当が自分の仕事の分野を持つときは「仕事で使う道具」
//!    （`context.work_genre`。Phase 30: 対話は常に対話用分野で走るが、その人が自分の得意分野を知って
//!    答えられるように 1 行足す）
//! 2. 永続の認可（`context.standing_rules`。SPEC §3.6「永続の認可は文字で記録してエージェントに注入する」。
//!    Phase 26 が埋める: 担当宛て + 全員向け）
//! 3. 記憶（`context.memory`）
//! 4. あなたの直近の仕事（`context.recent_work`。Phase 33: 実機で対話 run の担当が自分の直近の失敗を
//!    知らずに「対象タスク ID が必要です」と聞き返した事故の再発防止。対話 run にだけ出す）
//! 5. 直近のやり取り（`context.conversation`）
//! 6. 役割の指示文（`context.role`。ADR-0016 D1 からある既存の節）
//! 7. 記憶の書き方の指示（記憶が有効な run にだけ）
//! 8. 対話専用の指示（`context.conversation_addressee`。Phase 28: 対話 run は返事だけをする。
//!    末尾に足す。ADR-0033 D4 追記。Phase 33 で「自分の直近の仕事を先に見ること」を一文追加）
//!
//! 検索ハーネス（`local-deep-research`）はこの前置きを**一切使わない**（ADR-0029 / ADR-0033 D6:
//! 検索に渡す問いを濁さないため。Phase 27 の監査 M-2）。
//!
//! `RunContext` が既定値（Phase 23 までの中身しか無い）のときの出力は、Phase 23 の
//! `claude_code::prompt_header` が出していた文字列と**バイト単位で同じ**になる（既存テストがそれを見る）。

use task_core::MessageRole;

use crate::protocol::{ConversationAddressee, RunContext};

/// 前置き（役割の指示文を含む）。`claude-code` / `codex` / `acp` / `paperqa` が使う。
pub fn render(context: &RunContext) -> String {
    let mut out = person_sections(context);
    out.push_str(&role_section(context));
    out.push_str(&memory_instructions(context));
    out.push_str(&conversation_instructions(context));
    out
}

/// 1〜5（役職と brief → 永続の認可 → 記憶 → あなたの直近の仕事 → 直近のやり取り）。
fn person_sections(context: &RunContext) -> String {
    let mut out = String::new();
    if let Some(node) = &context.node {
        out.push_str(&format!("## あなた: {} ({})\n", node.name, node.id));
        if !node.brief.is_empty() {
            out.push_str(&node.brief);
            out.push('\n');
        }
        // Phase 30（ADR-0033 D4 追記）: 対話は常に対話用分野で走るが、担当ノード自身の仕事の分野が
        // あれば「仕事で使う道具」を 1 行足す（その人が自分の得意分野を知って答えられるように）。
        if let Some(genre) = &context.work_genre {
            out.push_str(&format!("あなたの仕事で使う道具（分野）: {}", genre.description));
            if !genre.capabilities.is_empty() {
                out.push_str(&format!("（できること: {}）", genre.capabilities.join("、")));
            }
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
    if !context.recent_work.is_empty() {
        out.push_str("## あなたの直近の仕事\n");
        for w in &context.recent_work {
            out.push_str(&format!("- [{}] {}", status_label(w.status), one_line(&w.title)));
            if let Some(project_title) = &w.project_title {
                out.push_str(&format!("（案件: {}）", one_line(project_title)));
            }
            if let Some(outcome) = &w.outcome {
                out.push_str(&format!(": {}", one_line(outcome)));
            }
            if !w.artifacts.is_empty() {
                out.push_str(&format!(" 成果物: {}", w.artifacts.join(", ")));
            }
            out.push('\n');
        }
        out.push('\n');
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

/// 節 7: 対話専用の指示（Phase 28 / ADR-0033 D4 追記）。実機で秘書が「返事の代わりに仕事を始めた」
/// （委譲・多ターンの調査・最終試行での代筆）ため、対話 run には**返事だけをする**ことを明示する。
/// 秘書宛てには SPEC §7 の (a)〜(d)、それ以外のノード宛てには「聞かれたことに答える」に文面を分ける。
/// 対話でない run（`conversation_addressee` が `None`）では何も出さない。
fn conversation_instructions(context: &RunContext) -> String {
    match context.conversation_addressee {
        Some(ConversationAddressee::Secretary) => {
            "## これは対話です (this is a conversation, not a work order)\n\
             この返事では作業を始めないでください。委譲・実装・調査は、人が方針と途中目標を承認してから \
             始まります。返事には次を、人が数十秒で読める分量で書いてください: \
             (a) 理解の確認 (b) 方針 (c) 最初の途中目標の提案 (d) 判断を仰ぎたいこと。\
             ファイルの作成や大きな探索は不要です。\
             自分の直近の仕事とその結果は上に書いてある。人に聞き返す前に、まずそれを見て答えること。\n\n"
                .to_string()
        }
        Some(ConversationAddressee::Other) => {
            "## これは対話です (this is a conversation, not a work order)\n\
             この返事では作業を始めないでください。委譲・実装・調査は、人が方針と途中目標を承認してから \
             始まります。聞かれたことに答え、必要なら次にやりたいことを書いてください。\
             ファイルの作成や大きな探索は不要です。\
             自分の直近の仕事とその結果は上に書いてある。人に聞き返す前に、まずそれを見て答えること。\n\n"
                .to_string()
        }
        None => String::new(),
    }
}

/// やり取りの 1 行化（前置きの箇条書きを崩さないため。中身は削らない）。
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `context.recent_work[].status` の表示名（`Status` の `snake_case` 表現。`task_core::model::Status` の
/// `#[serde(rename_all = "snake_case")]` と同じ）。
fn status_label(status: task_core::Status) -> &'static str {
    use task_core::Status::*;
    match status {
        Draft => "draft",
        Ready => "ready",
        Running => "running",
        Blocked => "blocked",
        Reviewing => "reviewing",
        Done => "done",
        Failed => "failed",
        Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ConversationTurn, GenreContext, MemoryContext, NodeContext, RoleContext};
    use task_core::Status;

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

    /// Phase 30（ADR-0033 D4 追記）: 対話は常に対話用分野で走るが、担当ノード自身の仕事の分野が
    /// あれば「仕事で使う道具」を役職と brief の直後に 1 行足す（実機の事故の再発防止: 関連研究調査課
    /// ＝検索ハーネスに話しかけても、検索ハーネスの run にはしない。その人に自分の分野を知らせるだけ）。
    #[test]
    fn a_work_genre_is_shown_right_after_the_brief_when_present() {
        let context = RunContext {
            work_genre: Some(GenreContext {
                id: "web-research".into(),
                description: "web 検索で先行研究を洗う".into(),
                capabilities: vec!["web 検索".into(), "証拠の収集".into()],
                ..GenreContext::default()
            }),
            ..full_context()
        };
        let out = render(&context);
        let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("missing {needle:?} in:\n{out}"));
        assert!(at("## あなた: 関連研究調査課 (research-survey)") < at("あなたの仕事で使う道具"));
        assert!(at("あなたの仕事で使う道具") < at("## 永続の認可"));
        assert!(
            out.contains("あなたの仕事で使う道具（分野）: web 検索で先行研究を洗う（できること: web 検索、証拠の収集）"),
            "{out}"
        );

        // 担当が自分の仕事の分野を持たない（対話用分野のみで走る）ときは何も足さない。
        let without = RunContext { work_genre: None, ..full_context() };
        let out = render(&without);
        assert!(!out.contains("あなたの仕事で使う道具"), "{out}");

        // `context.node` が無ければ、`work_genre` があっても出さない（役職の節そのものが無いため）。
        let no_node = RunContext {
            node: None,
            work_genre: Some(GenreContext { id: "coding".into(), description: "d".into(), ..GenreContext::default() }),
            ..RunContext::default()
        };
        assert!(!render(&no_node).contains("あなたの仕事で使う道具"));
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

    /// Phase 28（ADR-0033 D4 追記）: 対話 run にだけ、末尾に「返事だけをする」指示が付く。
    /// 秘書宛ては (a)〜(d)、それ以外は「聞かれたことに答える」。通常タスクの前置きは 1 バイトも変わらない。
    #[test]
    fn conversation_runs_get_a_reply_only_instruction_appended_at_the_end() {
        let ordinary = full_context();
        let ordinary_out = render(&ordinary);
        assert!(!ordinary_out.contains("これは対話です"), "{ordinary_out}");

        let secretary = RunContext {
            conversation_addressee: Some(ConversationAddressee::Secretary),
            ..ordinary.clone()
        };
        let out = render(&secretary);
        assert!(out.starts_with(&ordinary_out), "対話の指示は末尾に足すだけ: {out}");
        assert!(out.contains("この返事では作業を始めないでください"));
        assert!(out.contains("(a) 理解の確認"));
        assert!(out.contains("(d) 判断を仰ぎたいこと"));
        // Phase 33: 人に聞き返す前に、まず「あなたの直近の仕事」を見るよう促す一文。
        assert!(out.contains("自分の直近の仕事とその結果は上に書いてある"), "{out}");

        let other = RunContext {
            conversation_addressee: Some(ConversationAddressee::Other),
            ..RunContext::default()
        };
        let out = render(&other);
        assert!(out.contains("聞かれたことに答え"));
        assert!(!out.contains("(a) 理解の確認"), "{out}");
        assert!(out.contains("自分の直近の仕事とその結果は上に書いてある"), "{out}");

        // 対話でない run（既定値の `None`）では何も足さない。
        assert_eq!(render(&RunContext::default()), "");
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

    /// Phase 33（実機の事故 — 担当が自分の直近の失敗を知らずに聞き返した — の再発防止）:
    /// `context.recent_work` は記憶の直後、直近のやり取りより前に 1 行ずつ出す。
    #[test]
    fn recent_work_is_shown_right_after_memory_and_before_conversation() {
        let context = RunContext {
            recent_work: vec![
                task_worker_recent_work_sample(
                    Status::Failed,
                    "web-research タスク A",
                    Some("Pluvio の関連研究調査"),
                    Some("web search returned nothing (possible search path failure: expired key, CAPTCHA, or network block)"),
                    &[],
                ),
                task_worker_recent_work_sample(
                    Status::Done,
                    "先行研究のまとめ",
                    None,
                    Some("Pluvio と比較可能な非同期ランタイムを 3 件確認した"),
                    &["survey.md".into()],
                ),
            ],
            ..full_context()
        };
        let out = render(&context);
        let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("missing {needle:?} in:\n{out}"));
        assert!(at("## 覚えていること") < at("## あなたの直近の仕事"), "{out}");
        assert!(at("## あなたの直近の仕事") < at("## 直近のやり取り"), "{out}");
        assert!(
            out.contains(
                "- [failed] web-research タスク A（案件: Pluvio の関連研究調査）: web search returned nothing \
                 (possible search path failure: expired key, CAPTCHA, or network block)"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "- [done] 先行研究のまとめ: Pluvio と比較可能な非同期ランタイムを 3 件確認した 成果物: survey.md"
            ),
            "{out}"
        );

        // 空なら節そのものが無い。
        let without = RunContext { recent_work: Vec::new(), ..full_context() };
        assert!(!render(&without).contains("あなたの直近の仕事"));
        // 対話でない通常 run の前置きは 1 バイトも変わらない（既定値には `recent_work` が無い）。
        assert_eq!(render(&RunContext::default()), "");
    }

    fn task_worker_recent_work_sample(
        status: Status,
        title: &str,
        project_title: Option<&str>,
        outcome: Option<&str>,
        artifacts: &[String],
    ) -> crate::protocol::RecentWork {
        crate::protocol::RecentWork {
            task_id: task_core::TaskId::new(),
            title: title.to_string(),
            project_title: project_title.map(str::to_string),
            status,
            finished_at: Some("2026-09-18T00:00:00Z".to_string()),
            outcome: outcome.map(str::to_string),
            artifacts: artifacts.to_vec(),
        }
    }
}
