//! プロンプトの前置きを 1 か所で組む（ADR-0033 D4 / D6 / D5、Phase 24 / 26）。
//!
//! 「人」らしさは**注入される記憶と brief** で作る（ADR-0033 D6）。ハーネスのプロセスは相変わらず
//! ステートレスで、状態はファイルと DB にある（DESIGN 原則 2）。ここは純粋関数だけで、I/O も LLM も無い。
//!
//! 並び（ADR-0033 D4 / Phase 24 の指示。Phase 30 で 1 の直後に「仕事で使う道具」を追加。
//! Phase 33 で 3 の直後に「あなたの直近の仕事」を追加。Phase 53 / ADR-0044 D2 で**先頭に**
//! 「コメント」を追加）:
//! 0. コメント（`context.comments` / `context.interrupt`。ADR-0044 D2: 人がコメントで run を止めたら、
//!    次の run の**先頭**に「**人からの割り込み**: …」として出す。続けてコメントの糸を最新 20 件）
//! 1. 役職と brief（`context.node`）＋ 対話 run で担当が自分の仕事の分野を持つときは「仕事で使う道具」
//!    （`context.work_genre`。Phase 30: 対話は常に対話用分野で走るが、その人が自分の得意分野を知って
//!    答えられるように 1 行足す）
//! 2. 永続の認可（`context.standing_rules`。SPEC §3.6「永続の認可は文字で記録してエージェントに注入する」。
//!    Phase 26 が埋める: 担当宛て + 全員向け）
//! 3. 記憶（`context.memory`）
//! 4. あなたの直近の仕事（`context.recent_work`。Phase 33: 実機で対話 run の担当が自分の直近の失敗を
//!    知らずに「対象タスク ID が必要です」と聞き返した事故の再発防止。対話 run にだけ出す）
//! 5. 途中目標のここまでの結果（`context.milestone_review`。Phase 41 / ADR-0038 D1: 途中目標レビューの
//!    対話 run にだけ出す。その途中目標と、属する仕事の終わり方・成果物の抜粋。以下は 1 つずつ繰り下がる）
//! 5. 直近のやり取り（`context.conversation`）
//! 6. 役割の指示文（`context.role`。ADR-0016 D1 からある既存の節）
//! 7. 記憶の書き方の指示（記憶が有効な run にだけ）と、コメントの書き方
//!    （`context.comments_enabled` の run にだけ。ADR-0044 D2）
//! 8. 対話専用の指示（`context.conversation_addressee`。Phase 28: 対話 run は返事だけをする。
//!    末尾に足す。ADR-0033 D4 追記。Phase 33 で「自分の直近の仕事を先に見ること」を一文追加）
//!
//! 検索ハーネス（`local-deep-research`）はこの前置きを**一切使わない**（ADR-0029 / ADR-0033 D6:
//! 検索に渡す問いを濁さないため。Phase 27 の監査 M-2）。
//!
//! `RunContext` が既定値（Phase 23 までの中身しか無い）のときの出力は、Phase 23 の
//! `claude_code::prompt_header` が出していた文字列と**バイト単位で同じ**になる（既存テストがそれを見る）。

use task_core::MessageRole;

use crate::protocol::{ConversationAddressee, MilestoneReviewContext, RunContext};

/// 前置き（役割の指示文を含む）。`claude-code` / `codex` / `acp` / `paperqa` が使う。
/// `artifacts` は成果物ディレクトリの workspace 相対表記（`RunRequest::artifacts_rel`。ADR-0036 D3。
/// 単独タスクでは `artifacts` なので出力は Phase 34 までとバイト単位で同じ）。
pub fn render(context: &RunContext, artifacts: &str) -> String {
    // ADR-0044 D2（Phase 53）: コメントは**前置きの先頭**（人が割り込んだら最初に目に入る）。
    // コメントが 1 件も無ければ何も出さないので、Phase 52 までの出力とバイト単位で同じ。
    let mut out = comments_section(context);
    out.push_str(&person_sections(context));
    out.push_str(&workspace_section(context));
    out.push_str(&role_section(context));
    out.push_str(&memory_instructions(context, artifacts));
    // ADR-0044 D2（Phase 53）: コメントの書き方（`comments_enabled` の run にだけ）。
    out.push_str(&comment_instructions(context));
    out.push_str(&conversation_instructions(context));
    // Phase 41（ADR-0038 D1）: 途中目標レビューの対話 run には、対話の指示のさらに後ろに
    // 「結果 → 達成の可否 → 次の提案」の指示を足す（対話の指示は消さない）。
    if let Some(review) = &context.milestone_review {
        out.push_str(&milestone_review_instructions(review));
    }
    out
}

/// ADR-0044 D2（Phase 53）: 「コメント」の節。**前置きの先頭**に出す。
///
/// - 人のコメントで直前の run を止めた（`context.interrupt`）ときは、その本文を
///   「**人からの割り込み**: …」として**いちばん先**に置く（ADR-0044 D2 の表）。
///   割り込みの後に人がさらに書き足していれば、その**最新の**人のコメントがここに出る
///   （どちらも人が読ませたい文なので先頭に出してよい、という判断。`interrupting_comment`）。
/// - 続けてコメントの糸（最新 20 件、古い順）を出す。
/// - コメントが 1 件も無く割り込みも無ければ、この節ごと出さない（既存の出力を変えない）。
fn comments_section(context: &RunContext) -> String {
    if context.interrupt.is_none() && context.comments.is_empty() {
        return String::new();
    }
    let mut out = String::from("## コメント (comments on this task)\n");
    if let Some(interrupt) = &context.interrupt {
        out.push_str(&format!(
            "**人からの割り込み**: {}\n（この run はこのコメントで止められた。まずこれに応えること）\n",
            interrupt.trim()
        ));
    }
    for comment in &context.comments {
        let who = match comment.author_kind {
            task_core::CommentAuthorKind::Human => "人".to_string(),
            task_core::CommentAuthorKind::Node => match &comment.author {
                Some(id) => id.clone(),
                None => "担当".to_string(),
            },
            task_core::CommentAuthorKind::System => "taskd".to_string(),
        };
        out.push_str(&format!("- [{}] {}: {}\n", comment.at, who, one_line(&comment.body)));
    }
    out.push('\n');
    out
}

/// ADR-0044 D2（Phase 53）: ワーカーへの指示（コメントの書き方）。`comments_enabled` の run にだけ出す。
fn comment_instructions(context: &RunContext) -> String {
    if !context.comments_enabled {
        return String::new();
    }
    "## コメント (how to leave a note on this task)\n\
     短い進捗や判断の記録はコメントに書け（`{\"type\":\"comment\",\"body\":\"…\"}` を 1 行出す）。\
     `progress` と違ってコメントは**残り**、人にも次の run にも見える。長い成果は成果物に書くこと。\n\n"
        .to_string()
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
    out.push_str(&milestone_review_section(context));
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

/// ADR-0039 D3: 案件の作業場所から、前置きに出す 1 行を組む（純粋関数。ディスパッチャがこれを
/// `RunContext::workspace_note` に入れる）。`Remote` は ADR-0018 D1 の「クラスタ側が正、手元は写し」を書く。
pub fn workspace_note(spec: &task_core::WorkspaceSpec) -> String {
    match spec {
        task_core::WorkspaceSpec::Local { path, .. } => {
            format!("この案件のコードは `{}` にある。", path.display())
        }
        task_core::WorkspaceSpec::Remote { cluster, path } => format!(
            "この案件のコードはクラスタ {cluster} の `{}` にある。いまのカレントディレクトリはその写しで、\
             taskd が run の前後で同期する。",
            path.display()
        ),
    }
}

/// ADR-0041 D1: タスクごとの worktree を前置きに書く 1 行（純粋関数。ディスパッチャが
/// `workspace_note` の後ろに足す）。「このブランチにコミットせよ」までをここに書く。
pub fn worktree_note(repo: &std::path::Path, dir: &std::path::Path, branch: &str, base_sha12: &str, base_kind: &str) -> String {
    format!(
        "作業ツリー `{}`（`{}` の worktree）、ブランチ `{branch}`、base `{base_sha12}`（{base_kind}）。\
         このブランチにコミットせよ。`main` に直接コミットするな。`git checkout` でブランチを変えるな。",
        dir.display(),
        repo.display()
    )
}

/// 作業場所の節（ADR-0039 D3）。**案件が作業場所を決めている run にだけ**出す。
/// 実機の事故（2026-09-18）: 空の workspace に置かれた子タスクが、自分で `ssh` してリモートの
/// 作業ツリーに直接書いた。SPEC §3.7 追記「手元で編集してリモートで検証」をここで明示する。
fn workspace_section(context: &RunContext) -> String {
    let Some(note) = &context.workspace_note else {
        return String::new();
    };
    format!(
        "## 作業場所 (where this project's code lives)\n\
         {note}\n\
         編集はこの run の作業ディレクトリ（カレントディレクトリ）で行うこと。**別のホストの作業ツリーへ \
         `ssh` で直接書き込んではいけない**（同期は taskd が行う。検証・計測だけをリモートで実行する。\
         SPEC §3.7「手元で編集してリモートで検証」）。\n\n"
    )
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
fn memory_instructions(context: &RunContext, artifacts: &str) -> String {
    if context.memory.is_none() {
        return String::new();
    }
    format!(
        "## 覚えておくこと (how to write to your memory)\n\
         覚えておくべきこと（クラスタの使い方、人の好み、直近の相談）は `{artifacts}/result.json` の \
         `memory.notes` に、この案件だけの事は `memory.project` に、短い箇条書きの文字列の配列で返せ: \
         `{{\"summary\": \"…\", \"evidence\": [], \"memory\": {{\"notes\": [\"…\"], \"project\": [\"…\"]}}}}`。\
         覚えることが無ければ `memory` は書かなくてよい（空の配列でもよい）。ここに書いたものだけが次の run に \
         引き継がれる（この会話の他の部分は残らない）。\n\n"
    )
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

/// 節 4.5: 途中目標のここまでの結果（Phase 41 / ADR-0038 D1）。レビューの対話 run にだけ出す。
/// 中身は決定的に集めたものをそのまま並べるだけ（要約は run の仕事）。
fn milestone_review_section(context: &RunContext) -> String {
    let Some(review) = &context.milestone_review else {
        return String::new();
    };
    let mut out = String::new();
    out.push_str(&format!(
        "## 途中目標『{}』のここまで ({})\n",
        review.milestone.title, review.milestone.status
    ));
    if !review.milestone.description.is_empty() {
        out.push_str(&format!("{}\n", one_line(&review.milestone.description)));
    }
    for task in &review.tasks {
        out.push_str(&format!("\n### [{}] {}\n", status_label(task.status), one_line(&task.title)));
        if let Some(outcome) = &task.outcome {
            out.push_str(&format!("要約: {}\n", one_line(outcome)));
        }
        if !task.artifacts_excerpt.is_empty() {
            out.push_str("成果物の抜粋:\n");
            out.push_str(task.artifacts_excerpt.trim_end());
            out.push('\n');
        }
    }
    out.push('\n');
    out
}

/// 節 7 の追記（Phase 41 / ADR-0038 D1）: 途中目標レビューの対話 run にだけ足す指示。
/// 「結果 → 達成の可否 → 次の提案 → 判断を仰ぎたい点」を書かせ、次の途中目標は結果ファイルの
/// `milestone_proposal` にも書かせる（taskd はそこだけを決定的に読む）。
fn milestone_review_instructions(review: &MilestoneReviewContext) -> String {
    format!(
        "## 途中目標の判定をお願いする返事です (milestone review)\n\
         途中目標『{}』の仕事が止まりました。人に向けて、数十秒で読める分量で次を書いてください: \
         (a) この途中目標までで**得られた結果**の要約（数字・候補・出典）(b) 達成と言えるか\
         （言えないなら何が足りないか）(c) **次の途中目標の提案**（1 つ。題名と説明）\
         (d) 判断を仰ぎたい点。\
         次の途中目標は結果ファイルの `milestone_proposal` にも \
         `{{\"milestone_proposal\": {{\"title\": \"…\", \"description\": \"…\"}}}}` の形で書いてください\
         （人が「ok」を押すと、これが次の途中目標になります）。達成の可否を決めるのは人です。\n\n",
        review.milestone.title
    )
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
        let out = render(&full_context(), "artifacts");
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
        let out = render(&context, "artifacts");
        let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("missing {needle:?} in:\n{out}"));
        assert!(at("## あなた: 関連研究調査課 (research-survey)") < at("あなたの仕事で使う道具"));
        assert!(at("あなたの仕事で使う道具") < at("## 永続の認可"));
        assert!(
            out.contains("あなたの仕事で使う道具（分野）: web 検索で先行研究を洗う（できること: web 検索、証拠の収集）"),
            "{out}"
        );

        // 担当が自分の仕事の分野を持たない（対話用分野のみで走る）ときは何も足さない。
        let without = RunContext { work_genre: None, ..full_context() };
        let out = render(&without, "artifacts");
        assert!(!out.contains("あなたの仕事で使う道具"), "{out}");

        // `context.node` が無ければ、`work_genre` があっても出さない（役職の節そのものが無いため）。
        let no_node = RunContext {
            node: None,
            work_genre: Some(GenreContext { id: "coding".into(), description: "d".into(), ..GenreContext::default() }),
            ..RunContext::default()
        };
        assert!(!render(&no_node, "artifacts").contains("あなたの仕事で使う道具"));
    }

    /// 空の `RunContext` では前置きは空文字（Phase 23 までの出力と 1 バイトも変わらない）。
    #[test]
    fn an_empty_context_renders_nothing_at_all() {
        assert_eq!(render(&RunContext::default(), "artifacts"), "");
    }

    /// ADR-0044 D2（Phase 53）: コメントの節は**前置きの先頭**。人の割り込みがいちばん先に来て、
    /// その後にコメントの糸（古い順）が並ぶ。`comments_enabled` の run には書き方の指示も付く。
    /// コメントが 1 件も無い run の前置きは Phase 52 までと 1 バイトも変わらない。
    #[test]
    fn comments_come_first_and_the_interruption_is_the_very_first_line() {
        let context = RunContext {
            interrupt: Some("方針を変えたい。まず設計を書いて".into()),
            comments: vec![
                crate::protocol::CommentContext {
                    author_kind: task_core::CommentAuthorKind::Node,
                    author: Some("impl".into()),
                    body: "ビルドは通った\n（続き）".into(),
                    at: "2026-09-19T01:00:00Z".into(),
                },
                crate::protocol::CommentContext {
                    author_kind: task_core::CommentAuthorKind::Human,
                    author: None,
                    body: "方針を変えたい。まず設計を書いて".into(),
                    at: "2026-09-19T02:00:00Z".into(),
                },
            ],
            comments_enabled: true,
            ..full_context()
        };
        let out = render(&context, "artifacts");
        assert!(out.starts_with("## コメント (comments on this task)\n"), "{out}");
        let interrupt_at = out.find("**人からの割り込み**").expect("interrupt line");
        let thread_at = out.find("- [2026-09-19T01:00:00Z] impl:").expect("thread line");
        assert!(interrupt_at < thread_at, "割り込みが糸より先: {out}");
        // 複数行の本文は 1 行に畳む（他の節と同じ規則）。
        assert!(out.contains("- [2026-09-19T01:00:00Z] impl: ビルドは通った （続き）"), "{out}");
        assert!(out.contains("- [2026-09-19T02:00:00Z] 人: 方針を変えたい。まず設計を書いて"), "{out}");
        // 役職の節はコメントの後ろ。
        assert!(out.find("## あなた:").expect("node section") > interrupt_at, "{out}");
        // 書き方の指示（ADR-0044 D2）。
        assert!(out.contains("短い進捗や判断の記録はコメントに書け"), "{out}");
        assert!(out.contains(r#"{"type":"comment","body":"…"}"#), "{out}");

        // コメントが無ければ節ごと出ない（`comments_enabled` だけなら指示だけ）。
        let quiet = RunContext { comments_enabled: true, ..full_context() };
        let quiet_out = render(&quiet, "artifacts");
        assert!(!quiet_out.contains("## コメント (comments on this task)"), "{quiet_out}");
        assert!(quiet_out.contains("短い進捗や判断の記録はコメントに書け"), "{quiet_out}");
        let silent = render(&full_context(), "artifacts");
        assert!(!silent.contains("コメント"), "{silent}");
    }

    /// 役割だけがあるときは、Phase 23 の `prompt_header` と同じ `## Role:` 節だけを出す。
    #[test]
    fn a_role_only_context_renders_exactly_the_old_role_section() {
        let with_instructions = RunContext {
            role: Some(RoleContext { id: "lead".into(), instructions: "You coordinate.".into() }),
            ..RunContext::default()
        };
        assert_eq!(render(&with_instructions, "artifacts"), "## Role: lead\nYou coordinate.\n\n");
        let bare = RunContext {
            role: Some(RoleContext { id: "lead".into(), instructions: String::new() }),
            ..RunContext::default()
        };
        assert_eq!(render(&bare, "artifacts"), "## Role: lead\n\n");
    }

    /// Phase 28（ADR-0033 D4 追記）: 対話 run にだけ、末尾に「返事だけをする」指示が付く。
    /// 秘書宛ては (a)〜(d)、それ以外は「聞かれたことに答える」。通常タスクの前置きは 1 バイトも変わらない。
    #[test]
    fn conversation_runs_get_a_reply_only_instruction_appended_at_the_end() {
        let ordinary = full_context();
        let ordinary_out = render(&ordinary, "artifacts");
        assert!(!ordinary_out.contains("これは対話です"), "{ordinary_out}");

        let secretary = RunContext {
            conversation_addressee: Some(ConversationAddressee::Secretary),
            ..ordinary.clone()
        };
        let out = render(&secretary, "artifacts");
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
        let out = render(&other, "artifacts");
        assert!(out.contains("聞かれたことに答え"));
        assert!(!out.contains("(a) 理解の確認"), "{out}");
        assert!(out.contains("自分の直近の仕事とその結果は上に書いてある"), "{out}");

        // 対話でない run（既定値の `None`）では何も足さない。
        assert_eq!(render(&RunContext::default(), "artifacts"), "");
    }

    /// 記憶が空（ファイルが無い）なら記憶の節は出ないが、書き方の指示は出る（次から覚えられるように）。
    #[test]
    fn empty_memory_shows_no_memory_section_but_still_explains_how_to_write_it() {
        let context = RunContext {
            memory: Some(MemoryContext::default()),
            ..RunContext::default()
        };
        let out = render(&context, "artifacts");
        assert!(!out.contains("## 覚えていること"), "{out}");
        assert!(out.contains("## 覚えておくこと"), "{out}");
        // `[memory]` を設定していない run には何も出ない。
        assert!(!render(&RunContext::default(), "artifacts").contains("覚えておくこと"));
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
        let out = render(&context, "artifacts");
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
        assert!(!render(&without, "artifacts").contains("あなたの直近の仕事"));
        // 対話でない通常 run の前置きは 1 バイトも変わらない（既定値には `recent_work` が無い）。
        assert_eq!(render(&RunContext::default(), "artifacts"), "");
    }

    /// Phase 41（ADR-0038 D1）: レビューの対話 run にだけ、途中目標とそこまでの成果の節が出て、
    /// 末尾に「結果 → 達成の可否 → 次の提案」の指示が足される（対話の指示は消えない）。
    #[test]
    fn a_milestone_review_shows_the_results_and_asks_for_the_next_proposal() {
        use crate::protocol::{MilestoneBrief, MilestoneReviewContext, MilestoneTaskResult};
        let context = RunContext {
            conversation_addressee: Some(ConversationAddressee::Secretary),
            milestone_review: Some(MilestoneReviewContext {
                milestone: MilestoneBrief {
                    id: "01HM".into(),
                    title: "隣接領域の動向調査".into(),
                    description: "近い分野の直近 3 年を洗う".into(),
                    status: "in_progress".into(),
                },
                tasks: vec![MilestoneTaskResult {
                    title: "web 調査".into(),
                    status: Status::Done,
                    outcome: Some("候補を 3 本に絞った".into()),
                    artifacts_excerpt: "# answer.md\n候補 A / 候補 B / 候補 C".into(),
                }],
            }),
            ..full_context()
        };
        let out = render(&context, "artifacts");
        let at = |needle: &str| out.find(needle).unwrap_or_else(|| panic!("missing {needle:?} in:\n{out}"));
        // 節は「あなたの直近の仕事」の後、直近のやり取りの前。
        assert!(at("## 覚えていること") < at("## 途中目標『隣接領域の動向調査』のここまで"), "{out}");
        assert!(at("## 途中目標『隣接領域の動向調査』のここまで") < at("## 直近のやり取り"), "{out}");
        assert!(out.contains("(in_progress)"), "{out}");
        assert!(out.contains("### [done] web 調査"), "{out}");
        assert!(out.contains("要約: 候補を 3 本に絞った"), "{out}");
        assert!(out.contains("候補 A / 候補 B / 候補 C"), "{out}");
        // 指示は対話の指示の後ろ。
        assert!(at("これは対話です") < at("## 途中目標の判定をお願いする返事です"), "{out}");
        assert!(out.contains("(c) **次の途中目標の提案**"), "{out}");
        assert!(out.contains("milestone_proposal"), "{out}");

        // レビューでない run には何も出ない（通常の対話 run の前置きは 1 バイトも変わらない）。
        let plain = RunContext { milestone_review: None, ..context.clone() };
        let plain_out = render(&plain, "artifacts");
        assert!(!plain_out.contains("途中目標の判定をお願いする返事です"), "{plain_out}");
        assert!(!plain_out.contains("のここまで"), "{plain_out}");
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
