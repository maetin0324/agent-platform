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
    // ADR-0054 D1（Phase 67）: 継続セッションの差分、または新規セッションの前のセッションの要約。
    // `context.session_diff` が空の run の前置きは Phase 66 までと 1 バイトも変わらない。
    out.push_str(&session_diff_section(context));
    // ADR-0046 D1 / D4（Phase 59）: 実効 profile（能力・方針・道具・知識・ハーネス）と進め方。
    // どちらも `None` / 空なら節ごと出さないので、Phase 58 までの出力とバイト単位で同じ。
    // 61（ADR-0047 の知識の索引）は `knowledge_section` を別に足す。ここには入れない。
    out.push_str(&profile_section(context));
    out.push_str(&mode_section(context));
    out.push_str(&organization_section(context));
    // ADR-0048 D3（Phase 60b）: CoS の対話 run にだけ、進行中の案件とその途中目標。
    // `context.active_projects` が空の run（CoS 以外）の前置きは Phase 60a までとバイト単位で同じ。
    out.push_str(&active_projects_section(context));
    // ADR-0059 D6（Phase 99）: CoS の対話 run にだけ、クラスタの一覧（id・接続状態・実効 work_dir）。
    // `context.clusters` が空の run の前置きは Phase 98 までと 1 バイトも変わらない。
    out.push_str(&clusters_section(context));
    out.push_str(&workspace_section(context));
    // ADR-0047 D2（Phase 61）: マウントされた知識の**索引だけ**（本文は入れない。道具で読む）。
    // `context.knowledge` が無い run の前置きは Phase 60 までと 1 バイトも変わらない。
    if let Some(knowledge) = &context.knowledge {
        out.push_str(&knowledge_section(&knowledge.mounts, &knowledge.index));
    }
    out.push_str(&role_section(context));
    out.push_str(&deliverables_placement_note());
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
            task_core::CommentAuthorKind::System => "celeris".to_string(),
        };
        out.push_str(&format!(
            "- [{}] {}: {}\n",
            comment.at,
            who,
            one_line(&comment.body)
        ));
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
            out.push_str(&format!(
                "あなたの仕事で使う道具（分野）: {}",
                genre.description
            ));
            if !genre.capabilities.is_empty() {
                out.push_str(&format!(
                    "（できること: {}）",
                    genre.capabilities.join("、")
                ));
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
            out.push_str(&format!(
                "- [{}] {}",
                status_label(w.status),
                one_line(&w.title)
            ));
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

/// ADR-0054 D1（Phase 67）: 「前回の run 以降」の節。継続中（`session.resume = true`）なら差分
/// （新しい人の発言・dispatch したタスクの終端と要約・認可の結果・新しい案件）、新規セッション
/// （rollover・アカウント変更・resume 失敗の後を含む）なら前のセッションの要約（ADR-0033 D4 の
/// 対話履歴の末尾 20 件）。どちらの文面を渡すかはディスパッチャが決める（このモジュールは並べるだけ）。
/// `context.session_diff` が空なら何も出さない（Phase 66 までと同じ出力）。
fn session_diff_section(context: &RunContext) -> String {
    if context.session_diff.is_empty() {
        return String::new();
    }
    let mut out = String::from("## 前回の run 以降 (since your last turn in this session)\n");
    for line in &context.session_diff {
        out.push_str(&format!("- {}\n", one_line(line)));
    }
    out.push('\n');
    out
}

/// ADR-0046 D1（Phase 59）: 「あなたの実効 profile」の節。根→葉で継いだ結果（`EffectiveProfile`）を
/// そのまま箇条書きにする。`context.profile` が `None` なら**何も出さない**（Phase 58 までと同じ出力）。
///
/// ADR-0046 D8: 道具は「使ってよいものの一覧」と「ここに無いものは使うな」を必ず書く。
pub fn profile_section(context: &RunContext) -> String {
    let Some(profile) = &context.profile else {
        return String::new();
    };
    if profile.skills.is_empty()
        && profile.policy.is_empty()
        && profile.tools.is_empty()
        && profile.deny_tools.is_empty()
        && profile.knowledge.is_empty()
        && profile.harnesses_allowed.is_empty()
        && profile.run.is_none()
        && profile.tier.is_none()
    {
        return String::new();
    }
    let mut out = String::from("## あなたの実効 profile (inherited from the org tree)\n");
    if !profile.chain.is_empty() {
        out.push_str(&format!("継承: {}\n", profile.chain.join(" > ")));
    }
    if !profile.skills.is_empty() {
        out.push_str(&format!("能力（skills）: {}\n", profile.skills.join(", ")));
    }
    if !profile.harnesses_allowed.is_empty() {
        out.push_str(&format!(
            "受けられるハーネス: {}{}\n",
            profile.harnesses_allowed.join(", "),
            match profile.harness_default.as_deref() {
                Some(d) => format!("（既定 {d}）"),
                None => String::new(),
            }
        ));
    }
    if let Some(tier) = profile.tier {
        let allowed = if profile.allowed_tiers.is_empty() {
            String::new()
        } else {
            format!(
                "（許可: {}）",
                profile
                    .allowed_tiers
                    .iter()
                    .map(tier_label)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        out.push_str(&format!("モデルの段: {}{allowed}\n", tier_label(&tier)));
    }
    if let Some(run) = profile.run {
        out.push_str(&format!(
            "実行場所: {}\n",
            match run {
                task_core::ProfileRun::Host => "host（この計算機の上）",
                task_core::ProfileRun::Container => "container（コンテナの中）",
            }
        ));
    }
    if !profile.knowledge.is_empty() {
        out.push_str(&format!(
            "使える知識: {}\n",
            profile
                .knowledge
                .iter()
                .map(knowledge_label)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // ADR-0046 D8: 道具は許可制。ここに無いものは使わせない。
    if profile.tools.is_empty() {
        out.push_str(
            "使ってよい外部の道具: **無し**。gh / tavily / exa / docker / クラスタへの ssh は\
             このタスクでは使うな（必要なら人に聞け）。\n",
        );
    } else {
        out.push_str(&format!(
            "使ってよい外部の道具: {}\n",
            profile.tools.join(", ")
        ));
        out.push_str(
            "ここに挙がっていない外部の道具（gh / tavily / exa / docker / クラスタへの ssh）は使うな。\n",
        );
    }
    if !profile.deny_tools.is_empty() {
        out.push_str(&format!(
            "**禁止された道具**: {}\n",
            profile.deny_tools.join(", ")
        ));
    }
    if !profile.policy.is_empty() {
        out.push_str("組織の方針（根から順に。上ほど強い）:\n");
        for line in &profile.policy {
            out.push_str(&format!("- {}\n", one_line(line)));
        }
    }
    out.push('\n');
    out
}

fn tier_label(tier: &task_core::Tier) -> &'static str {
    match tier {
        task_core::Tier::Frontier => "frontier",
        task_core::Tier::Standard => "standard",
        task_core::Tier::Cheap => "cheap",
    }
}

/// `KnowledgeMount::label`（ADR-0047 D2 の `kb:<scope>` / `repo:<name>` / `dir:<path>` / `memory[:<node>]`）
/// に、`repo` の文書ディレクトリ（`docs`。既定と違うときだけ）を添える（Phase 59 追記）。
fn knowledge_label(mount: &task_core::KnowledgeMount) -> String {
    let mut out = mount.label();
    if let Some(docs) = mount.docs.as_deref().filter(|d| !d.is_empty()) {
        out.push_str(&format!("（docs: {docs}）"));
    }
    out
}

/// ADR-0046 D4（Phase 59）: 「この仕事の進め方」の節。`context.mode` が `None`（= 既定の
/// `production`）なら**何も出さない**（Phase 58 までと同じ出力）。
pub fn mode_section(context: &RunContext) -> String {
    let Some(mode) = context.mode else {
        return String::new();
    };
    let (name, rules): (&str, &[&str]) = match mode {
        task_core::TaskMode::Prototype => (
            "prototype（試作）",
            &[
                "動くことを最短で示す。テストは動作確認の最小限でよい。",
                "捨てる前提で書く。作り込むな。",
                "結論と次の一手を summary に書く。",
                "レビューは明示の受け入れ条件だけ。リポジトリの検査コマンド（check）は使わない。",
            ],
        ),
        task_core::TaskMode::Production => (
            "production（本番）",
            &[
                "既存のテストと lint を通す。",
                "変更は小さく、理由をコミットに書く。",
                "レビューは受け入れ条件 ＋ リポジトリの検査コマンド（check）。",
            ],
        ),
        task_core::TaskMode::Research => (
            "research（研究）",
            &[
                "主張には出典か計測を付ける。",
                "数値は再現手順と一緒に書く。",
                "採らなかった案と理由も残す。",
                "結果に出典（`sources`）か計測の記録が無ければ不合格になる。",
            ],
        ),
    };
    let mut out = format!("## この仕事の進め方 (mode: {name})\n");
    for rule in rules {
        out.push_str(&format!("- {rule}\n"));
    }
    out.push('\n');
    out
}

/// ADR-0046 D6（Phase 59）: CoS（根ノード）の対話 run にだけ出す「組織の一覧」。
/// 誰が何をできるか（id / 名前 / skills / harnesses）を見せるが、**人選はしない**
/// （担当は D5 の matching が決定的に決める）。それ以外の run では何も出さない。
pub fn organization_section(context: &RunContext) -> String {
    if context.conversation_addressee != Some(ConversationAddressee::Secretary)
        || context.organization.is_empty()
    {
        return String::new();
    }
    let mut out = String::from("## 組織 (who is in the org)\n");
    for node in &context.organization {
        out.push_str(&format!("- `{}` {}", node.id, node.name));
        if !node.skills.is_empty() {
            out.push_str(&format!(" — skills: {}", node.skills.join(", ")));
        }
        if !node.harnesses.is_empty() {
            out.push_str(&format!(" / harnesses: {}", node.harnesses.join(", ")));
        }
        // Phase 98（ADR-0046 D8）: 道具（特に `cluster:<id>`）を 1 語ずつ添える。CoS がクラスタ作業を
        // どのノードに `create_task` で流せばよいかを前置きから判断できるようにする。
        if !node.tools.is_empty() {
            out.push_str(&format!(" / 道具: {}", node.tools.join(", ")));
        }
        out.push('\n');
    }
    out.push_str(
        "担当は celeris が決める（必要な skills と harness をタスクに書けば、そこから決定的に選ばれる）。\
         あなたが名指しで人を選ぶ必要はない。\n\n",
    );
    out
}

/// ADR-0048 D3（Phase 60b）: CoS の対話 run にだけ出す「進行中の案件」の節（id / 題名 / 状態と、
/// その途中目標）。`actions` の `create_task.project` / `add_milestone.project` を選ぶ材料。
fn active_projects_section(context: &RunContext) -> String {
    if context.conversation_addressee != Some(ConversationAddressee::Secretary)
        || context.active_projects.is_empty()
    {
        return String::new();
    }
    let mut out = String::from("## 進行中の案件 (active projects)\n");
    for project in &context.active_projects {
        out.push_str(&format!(
            "- `{}` {}（{}）\n",
            project.id, project.title, project.status
        ));
        if !project.repos.is_empty() {
            out.push_str(&format!(
                "  - 登録済み repos: {}（使用時の project: `{}`）\n",
                project
                    .repos
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                project.id
            ));
        }
        for milestone in &project.milestones {
            out.push_str(&format!(
                "  - 途中目標 `{}` {}（{}）\n",
                milestone.id, milestone.title, milestone.status
            ));
        }
    }
    out.push('\n');
    out
}

/// ADR-0059 D6（Phase 99）: CoS の対話 run にだけ出す「クラスタ」の節（id・接続状態・実効
/// work_dir）。`create_task.workspace` の `path` をどう組むか（登録済みの work_dir を使うか、
/// 未登録なら省略するか）の材料。
fn clusters_section(context: &RunContext) -> String {
    if context.conversation_addressee != Some(ConversationAddressee::Secretary)
        || context.clusters.is_empty()
    {
        return String::new();
    }
    let mut out = String::from("## クラスタ (clusters)\n");
    for cluster in &context.clusters {
        let connected = if cluster.connected {
            "接続中"
        } else {
            "未接続"
        };
        match &cluster.work_dir {
            Some(work_dir) => {
                out.push_str(&format!(
                    "- `{}`（{connected}） 作業ディレクトリ: `{work_dir}`\n",
                    cluster.id
                ));
            }
            None => {
                out.push_str(&format!(
                    "- `{}`（{connected}） 作業ディレクトリ: 未登録（`path` は省略し、\
                     人に登録を頼む）\n",
                    cluster.id
                ));
            }
        }
    }
    out.push('\n');
    out
}

/// ADR-0039 D3: 案件の作業場所から、前置きに出す 1 行を組む（純粋関数。ディスパッチャがこれを
/// `RunContext::workspace_note` に入れる）。`Remote` は ADR-0018 D1 の「クラスタ側が正、手元は写し」を書く。
pub fn workspace_note(spec: &task_core::WorkspaceSpec) -> String {
    match spec {
        task_core::WorkspaceSpec::Local { path, .. } => {
            format!("この案件のコードは `{}` にある。", path.display())
        }
        task_core::WorkspaceSpec::Remote { cluster, path, .. } => format!(
            "この案件のコードはクラスタ {cluster} の `{}` にある。いまのカレントディレクトリはその写しで、\
             celeris が run の前後で同期する。",
            path.display()
        ),
    }
}

/// ADR-0041 D1: タスクごとの worktree を前置きに書く 1 行（純粋関数。ディスパッチャが
/// `workspace_note` の後ろに足す）。「このブランチにコミットせよ」までをここに書く。
pub fn worktree_note(
    repo: &std::path::Path,
    dir: &std::path::Path,
    branch: &str,
    base_sha12: &str,
    base_kind: &str,
) -> String {
    format!(
        "作業ツリー `{}`（`{}` の worktree）、ブランチ `{branch}`、base `{base_sha12}`（{base_kind}）。\
         このブランチにコミットせよ。`main` に直接コミットするな。`git checkout` でブランチを変えるな。",
        dir.display(),
        repo.display()
    )
}

/// ADR-0043 D2 / D8: 前置きの「作業場所」に出すリポジトリ 1 件（純粋なデータ。ディスパッチャが組む）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoNote {
    /// 案件の中での名前（`project_repos.name`）。
    pub name: String,
    /// タスクの作業場所の中での相対パス（`repos/<name>/`）。
    pub dir: String,
    /// git の worktree か（偽なら「ディレクトリ。読み書き可。git ではない」）。
    pub git: bool,
    /// worktree のブランチ（git のときだけ）。
    pub branch: Option<String>,
    /// base の短縮 sha（git のときだけ）。
    pub base: Option<String>,
    /// base をどこから取ったか（`main` / `current` / `head`。git のときだけ）。
    pub base_kind: Option<String>,
    /// `workspace.toml` の `[workspace] description`（無ければ出さない）。
    pub description: Option<String>,
    /// `workspace.toml` の `[commands] check`（無ければ出さない）。
    pub check: Vec<String>,
    /// `workspace.toml` の `[outputs] docs`（既定 `docs`）。
    pub docs: String,
    /// `workspace.toml` の `[outputs] deliverables`（既定 `.`）。
    pub deliverables: String,
}

/// ADR-0066 D1（Phase 110b）: `[workspace] shared_build_cache` が有効で git のリポジトリがあるときだけ、
/// `repos_note` の後ろに足す 1 行（ディスパッチャが `shared_build_cache` の設定を見て呼ぶかどうかを決める。
/// ここは文面だけの純粋関数）。
pub fn shared_build_cache_note() -> &'static str {
    "`target/` はリポジトリ間で共有するビルドキャッシュにある（`CARGO_TARGET_DIR`）。worktree ごとに\
     再ビルドしない。\n"
}

/// ADR-0043 D2 / D8: タスクが複数のリポジトリを持つときの「作業場所」の本文（純粋関数）。
/// ディスパッチャが ADR-0039 D3 の `workspace_note` の代わりにこれを入れる。
///
/// 出る順は `repos` の順（先頭がカレントディレクトリ）。`repos` が空なら空文字列。
pub fn repos_note(repos: &[RepoNote]) -> String {
    let Some(first) = repos.first() else {
        return String::new();
    };
    let mut out = String::new();
    out.push_str("この案件のリポジトリのうち、このタスクが使うものは次のとおり:\n");
    for repo in repos {
        out.push_str(&format!("- `{}` → `{}`", repo.name, repo.dir));
        if repo.git {
            let branch = repo.branch.as_deref().unwrap_or("");
            let base = repo.base.as_deref().unwrap_or("");
            let base_kind = repo.base_kind.as_deref().unwrap_or("");
            out.push_str(&format!(
                "（worktree、ブランチ `{branch}`、base `{base}`（{base_kind}））"
            ));
        } else {
            out.push_str("（ディレクトリ。読み書き可。git ではない）");
        }
        if let Some(description) = repo.description.as_deref().filter(|d| !d.trim().is_empty()) {
            out.push_str(&format!(" — {}", description.trim()));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "カレントディレクトリは `{}`。編集はこの作業場所の中だけで行い、元のリポジトリには直接書くな。\n",
        first.dir
    ));
    if repos.iter().any(|r| r.git) {
        out.push_str(
            "git のリポジトリでは celeris が用意したブランチにコミットせよ。`main` に直接コミットするな。\
             `git checkout` でブランチを変えるな。\n",
        );
    }
    for repo in repos.iter().filter(|r| !r.check.is_empty()) {
        out.push_str(&format!(
            "`{}` のこのリポジトリの検査コマンド: {}\n",
            repo.name,
            repo.check
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(" / ")
        ));
    }
    // ADR-0043 D8: 成果物は案件のリポジトリの中。`artifacts/` は中間物だけ。
    out.push_str(&format!(
        "コード以外の成果物（図・表・原稿）は `{}`、文書は `{}` の下に置け。`artifacts/` は run の中間物・\
         ログ・機械向けの `result.json` だけで、人が読む成果物を置く場所ではない。\n",
        join_repo_path(&first.dir, &first.deliverables),
        join_repo_path(&first.dir, &first.docs)
    ));
    // ADR-0044 D7（Phase 57）: 文書の書き方（正本は git。人は GUI の「文書」タブで同じファイルを読む）。
    let docs_dir = join_repo_path(&first.dir, &first.docs);
    let docs_dir = if docs_dir.ends_with('/') {
        docs_dir
    } else {
        format!("{docs_dir}/")
    };
    out.push_str(&format!(
        "文書は `{docs_dir}` に Markdown で書く（題名は 1 行目の `# `。タスクとの紐付けは front matter の \
         `tasks: [<このタスクの id>]`）。既定のブランチに直接コミットせず、上のブランチに置け（人が取り込む）。\n"
    ));
    out
}

/// ADR-0043 D2: 計画 run に渡す「この案件のリポジトリ」1 件（純粋なデータ。ディスパッチャが組む）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectRepoNote {
    pub name: String,
    /// `git` / `dir`。
    pub kind: String,
    /// 置き場（`~/workspace/benchfs` / `pegasus:/work/...`）。
    pub location: String,
    /// `workspace.toml` の `[workspace] description`（無ければ出さない）。
    pub description: Option<String>,
    /// 案件の主なリポジトリ（成果物と文書の既定の置き場）。
    pub is_primary: bool,
}

/// ADR-0043 D2: 計画 run の前置きに出す「この案件のリポジトリ」の一覧（純粋関数）。
/// 子タスクはこの**名前**を `repos` に書く。空なら空文字列。
pub fn project_repos_note(repos: &[ProjectRepoNote]) -> String {
    if repos.is_empty() {
        return String::new();
    }
    let mut out = String::from("この案件のリポジトリ:\n");
    for repo in repos {
        out.push_str(&format!("- `{}`（{}", repo.name, repo.kind));
        if repo.is_primary {
            out.push_str("、主なリポジトリ");
        }
        out.push_str(&format!("）: {}", repo.location));
        if let Some(description) = repo.description.as_deref().filter(|d| !d.trim().is_empty()) {
            out.push_str(&format!(" — {}", description.trim()));
        }
        out.push('\n');
    }
    out.push_str("子タスクが使うリポジトリは `repos` に**この名前で**書く（例 `\"repos\": [\"");
    out.push_str(&repos[0].name);
    out.push_str(
        "\"]`）。書かなければ主なリポジトリを継ぐ。ここに無い名前を書くと計画は差し戻される。\n",
    );
    out
}

/// `repos/benchfs/` と `docs` → `repos/benchfs/docs`（`.` はリポジトリのルートそのもの）。
fn join_repo_path(dir: &str, rel: &str) -> String {
    let base = dir.trim_end_matches('/');
    let rel = rel.trim().trim_start_matches("./").trim_end_matches('/');
    if rel.is_empty() || rel == "." {
        format!("{base}/")
    } else {
        format!("{base}/{rel}")
    }
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
         `ssh` で直接書き込んではいけない**（同期は celeris が行う。検証・計測だけをリモートで実行する。\
         SPEC §3.7「手元で編集してリモートで検証」）。\n\n"
    )
}

/// ADR-0047 D2 / D3（Phase 61）: 知識の節（**索引だけ**。本文は入れない）。
///
/// `mounts` は実効マウント（ADR-0047 D2: 組織の和 ＋ 案件 ＋ タスクの明示）、`index` はそのマウントで
/// 読めるページの索引（[`task_core::knowledge::mount_matches`] で振り分ける）。純粋関数で、
/// マウントが 1 つも無い・索引が空なら**空文字列**（前置きは 1 バイトも変わらない）。
///
/// 出るもの: マウントの一覧 → D3 の使い方 → マウントごとの `path` / `title` / `tags`
/// （全体で最大 [`task_core::knowledge::MAX_PREAMBLE_ITEMS`] 件。溢れた分は件数だけ出す）。
pub fn knowledge_section(
    mounts: &[task_core::KnowledgeMount],
    index: &[task_core::KnowledgeItem],
) -> String {
    if mounts.is_empty() {
        return String::new();
    }
    let mut out = String::from("## 知識 (knowledge base — 索引だけ。本文は道具で読む)\n");
    out.push_str(&format!(
        "あなたが読める知識: {}。\n",
        mounts
            .iter()
            .map(|m| format!("`{}`", m.label()))
            .collect::<Vec<_>>()
            .join("、")
    ));
    // ADR-0047 D3 の案内文。
    out.push_str(
        "知識は `celerisctl knowledge search <語> [--scope …]` で探し、`celerisctl knowledge get <path>` で読む。\
         将来も使える事実を得たら `celerisctl knowledge record --title … --scope … --source task:<このタスクの id>` \
         で候補に入れる（一時的な情報・雑談・推測は入れない。出典を付ける）。候補は人が確認してから正本に入る。\n",
    );
    let mut shown = 0usize;
    let mut sections = String::new();
    for mount in mounts {
        let items: Vec<&task_core::KnowledgeItem> = index
            .iter()
            .filter(|item| task_core::knowledge::mount_matches(mount, item))
            .collect();
        if items.is_empty() {
            continue;
        }
        sections.push_str(&format!("### {}\n", mount.label()));
        for (n, item) in items.iter().enumerate() {
            if shown >= task_core::knowledge::MAX_PREAMBLE_ITEMS {
                sections.push_str(&format!(
                    "- （ほか {} 件。`search` で探す）\n",
                    items.len() - n
                ));
                break;
            }
            sections.push_str(&format!("- `{}` — {}", item.path, one_line(&item.title)));
            if !item.tags.is_empty() {
                sections.push_str(&format!("（{}）", item.tags.join("、")));
            }
            sections.push('\n');
            shown += 1;
        }
    }
    if sections.is_empty() {
        // マウントはあるが 1 件も読めるものが無い（KB がまだ無い・空）。索引の節は出さない。
        return String::new();
    }
    out.push_str(&sections);
    out.push('\n');
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

/// ADR-0067 D1: 人が読む成果物の置き場の規則（全 run 共通、短く固定）。本番事故
/// （BenchFS 案件のタスクが判断材料をリポジトリの `docs/` に書き、GUI から見えなかった）の再発防止。
pub(crate) fn deliverables_placement_note() -> String {
    "## 成果物の置き場所 (where human deliverables live)\n\
     調査報告・framing 案・比較表・提案書など**人が読んで判断する材料**は、成果物ディレクトリ\
     （`artifacts/`）か知識ベース（`projects/<project>/…` のページ）に置いてください。対象リポジトリの\
     追跡ファイル（`docs/` を含む）には Celeris 自身の判断過程・候補案・決定パケットを置かないこと\
     （そのリポジトリに書いてよいのはそのリポジトリ自身の成果 — コード・テスト・決定後の本文など）。\n\n"
        .to_string()
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
            format!(
                "## CoS の対話と仕事の開始 (conversation and authorized work)\n\
                 質問には簡潔に答え、実行・修正・改善の依頼には下記 actions で実際の仕事を作ってください。\
                 明示された通常の修正と検証は既に依頼された作業です。方針や途中目標を毎回再承認させないでください。\
                 この短い対話 run では大きな調査や実装を直接せず、実行担当へ委譲します。\
                 元の依頼が修正なら objective と acceptance に実装・検証・成果の引き渡しまで含め、\
                 調査や改善案だけに縮小しないでください。調査はその仕事の途中の手順です。\
                 調査だけを依頼された場合は調査まで。分割する場合も最終成果までの仕事と依存関係を残し、\
                 中間報告を依頼全体の完了と呼ばないでください。未完了なら不足と継続中の仕事を示してください。\
                 質問は実行に不可欠な情報の不足、依頼範囲の拡大、未許可の破壊的操作などの場合だけです。\
                 既存の仕事と結果、承認済みの範囲を確認し、同じ調査や承認要求を繰り返さないでください。\
                 クラスタ（pegasus / sirius / fern03 など）でコマンドを実行する・状態を見る・ジョブを流す\
                 依頼は、自分で実行しないでください。『ssh が禁止されている』『read-only』を理由に断らないで\
                 ください（ADR-0046 D8 のとおりこの対話 run 自身には道具が無いのが正常で、それは断る理由に\
                 なりません）。`cluster:<id>` を tools に持つノードの仕事として `create_task` を書き、\
                 `workspace` に `{{\"kind\":\"remote\",\"cluster\":\"<id>\",\"path\":\"<クラスタ側の作業\
                 ディレクトリ>\"}}` を入れてください。path は案件の作業場所、無ければ人の依頼文にある場所。\
                 コマンドを実行する・状態を見る・ジョブを流すだけでコードの diff を作らない仕事は、\
                 `workspace` に `\"mode\":\"shared\"` も付けてください（worktree を切らず、そのまま実行\
                 します）。この場合の `path` は下の「クラスタ」に出ている実効の作業ディレクトリを使い、\
                 `~` は使わないでください（HPC のホームは作業用ではないのが普通です）。指したいクラスタの\
                 作業ディレクトリが「未登録」なら `path` を省略し（celeris が登録され次第その場所を使い\
                 ます）、返事で人に「cluster-hpc へ依頼したが、<id> クラスタの作業ディレクトリが未登録\
                 なのでクラスタ画面で登録してほしい」のように一言添えてください。\
                 remote の仕事は `cluster:<id>` を持つノード（クラスタ一覧の道具欄）に流してください。\
                 持たないノードに名指しで流すと検証で落ちます。調査・執筆・分析の仕事（web-research /\
                 literature-research / scientific-writing / experiment-data など、`cluster:<id>` を\
                 持たないノード）は remote にしないでください。クラスタで実行する仕事だけを\
                 `cluster:<id>` を持つノードに `workspace.mode: shared` で流してください。案件の作業場所が\
                 remote でも、担当に道具が無ければ celeris が local に落とします（ADR-0062 B）。\n\n\
                 {}",
                actions_instructions()
            )
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

/// ADR-0048 D3（Phase 60b）: CoS の対話にだけ足す「動く」経路の説明（結果ファイルの宣言的な `actions`
/// を taskd が決定的に実行する。指示文はこれを説明するだけで、実行そのものはコードの仕事）。
fn actions_instructions() -> String {
    "## 人からの頼みを動かす (declaring actions)\n\
     人の発言から具体的な仕事や案件が要りそうなら、返事の `summary` とは別に、結果ファイルに \
     `\"actions\": [...]` を宣言してください（taskd が決定的に実行します。あなた自身がタスクを \
     作ったり道具を使ったりはしません）。\n\
     - `{\"type\": \"create_task\", \"title\": \"…\", \"objective\": \"…\", \"acceptance\": [\"…\"], \
     \"harness\": \"coding\", \"skills\": [\"rust\"], \"mode\": \"prototype\", \"repos\": [], \
     \"project\": \"<案件の id か null>\", \"milestone\": \"<途中目標の id か null>\", \
     \"workspace\": {\"kind\":\"remote\",\"cluster\":\"<id>\",\"path\":\"<作業ディレクトリ>\",\
     \"mode\":\"shared\"}}`\
     （`workspace` は省略可。クラスタでの仕事だけ入れる。\
     ローカルなら `{\"kind\":\"local\",\"path\":\"…\"}`）\n\
     `workspace.mode` はコマンドを実行するだけでコードの diff を作らない仕事のとき `\"shared\"` にします\
     （worktree を切らず、`path` にそのまま cd して実行します。ADR-0059）。コードを直す仕事では付けません\
     （省略時は worktree）。`create_task.mode`（下）とは別のフィールドです。\n\
     - `{\"type\": \"propose_project\", \"title\": \"…\", \"request\": \"…\", \"repos\": [\"/abs/path\"]}`\n\
     - `{\"type\": \"add_milestone\", \"project\": \"<案件の id>\", \"title\": \"…\", \"description\": \"…\"}`\n\
     - `{\"type\": \"ask_human\", \"text\": \"…\"}`\n\
     **あなた（CoS）は goal / harness / skills / mode / repos / 制約を定義し、担当（`assignee`）とモデル（`tier`）は\
     選びません**（ADR-0069）。担当は celeris が skills と harness から決定的に選び、モデルの lane は仕事の性質から\
     決めます。仕事の性質を伝えたいときは任意の `\"features\": {\"judgment\": \"high\", \"verifiability\": \"low\"}` \
     （各軸 low / medium / high。judgment, ambiguity, verifiability, reversibility, consequence, context_size, \
     tool_intensity, expected_length, cross_cutting）を書けます。人が発言で `@<担当 id>` や `tier:<lane>` と明示した\
     ときだけ、その値を `assignee` / `tier` に写してください（celeris は人の発言を確かめてから従います）。\n\
     `create_task.mode` は進め方で、prototype / production / research のいずれかです。通常実装は `mode: \"production\"` とし、mode に standard（tier の名前）は書かないでください。\n\
     `create_task.repos` は案件内の登録名です。指定するときは必ず所属する案件の ID を `project` に書き、\
     上の登録済み repos から選んでください。`project: null` と非空の `repos` の組み合わせは禁止です。\
     既存のコードを直す依頼は、そのリポジトリが登録された既存案件に紐づけます。\
     案件に属さない仕事は `project: null, repos: []`。判断できないときは推測せず質問してください。\n\
     目安: **1 つのタスクで 1 時間以内に終わり、承認が要らない変更**なら `create_task` を 1 つ書けば \
     十分です。「案件として」「途中目標に」のように人が儀式を求めていれば `propose_project` /\
     `add_milestone`。判断に必要な情報が欠けるときは `ask_human`。通常の実装判断は担当に任せます。案件が分かっていれば `project` \
     を書いてください（担当は書かなくても celeris が skills と harness から決定的に選びます）。\
     検証に落ちた action（知らない harness / repos / 案件など）は実行されず、理由が人に見えます。\n\
     人の確認が要る `acceptance`（`\"human\"`）を書くときは、必ず `artifact_exists` か\
     `knowledge_page`（知識ベースのページ参照）の条件も添えてください。人が読む決定材料は登録済みの\
     artifacts か知識ベースのページに置き（GUI から見える場所）、対象リポジトリの `docs/` などの\
     追跡ファイルには置きません（ADR-0067）。\n\
     調査系（`literature` / `web-research`）の `create_task` を書くときは、`objective` の 1 行目を \
     **`対象: <対象1> / <対象2> / …（観点: <観点1>、<観点2>、…）`** の明示形にしてください \
     （例: `対象: CHFS / FINCHFS / GekkoFS / UnifyFS / BeeOND（観点: server/client 配置、\
     cache/direct I/O、file semantics、replication）`。区切りは `/` でも `、`/`,` でも構いません）。\
     対象が 5 を超える \
     場合は対象ごとにタスクを分けてください（ADR-0063 Phase 109c A）。受け入れ条件は、対象ごとに分ける \
     か、レビュアー条件（`acceptance` のうち `check` が reviewer のもの）に「**対象ごとに**、指定の観点 \
     が一次情報（または文献）に基づいて整理されている。確認できない観点は『未確認』と明記されていれば \
     不合格の理由にしない」という一文を含めてください（ADR-0063 D3、Phase 109c D で具体化）。1 件の欠落 \
     で全体を落とさないためです。成果物の存在確認（`check: \"artifact_exists\"`）は `report.md` を \
     使ってください（`literature`/`web-research` のどちらのハーネスも `report.md` を書きます。\
     ADR-0063 Phase 109b A3）。観点に比較先との比較分類（例: 「BenchFS との比較分類」）を含める場合、\
     それは文献検索ではなく比較先の設計条件との照合による判断で、必ず『公平比較可能』か『背景比較のみ』\
     のどちらかを確度付きで出させてください。分類の欠落は不合格の理由になります（ADR-0063 Phase 109g）。\n\n"
        .to_string()
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
        out.push_str(&format!(
            "\n### [{}] {}\n",
            status_label(task.status),
            one_line(&task.title)
        ));
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
/// `milestone_proposal` にも書かせる（celeris はそこだけを決定的に読む）。
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
    use crate::protocol::{
        ConversationTurn, GenreContext, MemoryContext, NodeContext, RoleContext,
    };
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
                ConversationTurn {
                    role: MessageRole::User,
                    text: "先週の続きを\nお願い".into(),
                },
                ConversationTurn {
                    role: MessageRole::Node,
                    text: "承知しました".into(),
                },
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
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("missing {needle:?} in:\n{out}"))
        };
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
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("missing {needle:?} in:\n{out}"))
        };
        assert!(at("## あなた: 関連研究調査課 (research-survey)") < at("あなたの仕事で使う道具"));
        assert!(at("あなたの仕事で使う道具") < at("## 永続の認可"));
        assert!(
            out.contains("あなたの仕事で使う道具（分野）: web 検索で先行研究を洗う（できること: web 検索、証拠の収集）"),
            "{out}"
        );

        // 担当が自分の仕事の分野を持たない（対話用分野のみで走る）ときは何も足さない。
        let without = RunContext {
            work_genre: None,
            ..full_context()
        };
        let out = render(&without, "artifacts");
        assert!(!out.contains("あなたの仕事で使う道具"), "{out}");

        // `context.node` が無ければ、`work_genre` があっても出さない（役職の節そのものが無いため）。
        let no_node = RunContext {
            node: None,
            work_genre: Some(GenreContext {
                id: "coding".into(),
                description: "d".into(),
                ..GenreContext::default()
            }),
            ..RunContext::default()
        };
        assert!(!render(&no_node, "artifacts").contains("あなたの仕事で使う道具"));
    }

    /// 空の `RunContext` では前置きは「成果物の置き場所」の節だけ（ADR-0067 D1。Phase 23〜110 の出力は
    /// 空文字だったが、この節だけは context に関わらず常に出る）。
    #[test]
    fn an_empty_context_renders_only_the_deliverables_placement_note() {
        assert_eq!(
            render(&RunContext::default(), "artifacts"),
            deliverables_placement_note()
        );
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
        assert!(
            out.starts_with("## コメント (comments on this task)\n"),
            "{out}"
        );
        let interrupt_at = out.find("**人からの割り込み**").expect("interrupt line");
        let thread_at = out
            .find("- [2026-09-19T01:00:00Z] impl:")
            .expect("thread line");
        assert!(interrupt_at < thread_at, "割り込みが糸より先: {out}");
        // 複数行の本文は 1 行に畳む（他の節と同じ規則）。
        assert!(
            out.contains("- [2026-09-19T01:00:00Z] impl: ビルドは通った （続き）"),
            "{out}"
        );
        assert!(
            out.contains("- [2026-09-19T02:00:00Z] 人: 方針を変えたい。まず設計を書いて"),
            "{out}"
        );
        // 役職の節はコメントの後ろ。
        assert!(
            out.find("## あなた:").expect("node section") > interrupt_at,
            "{out}"
        );
        // 書き方の指示（ADR-0044 D2）。
        assert!(
            out.contains("短い進捗や判断の記録はコメントに書け"),
            "{out}"
        );
        assert!(out.contains(r#"{"type":"comment","body":"…"}"#), "{out}");

        // コメントが無ければ節ごと出ない（`comments_enabled` だけなら指示だけ）。
        let quiet = RunContext {
            comments_enabled: true,
            ..full_context()
        };
        let quiet_out = render(&quiet, "artifacts");
        assert!(
            !quiet_out.contains("## コメント (comments on this task)"),
            "{quiet_out}"
        );
        assert!(
            quiet_out.contains("短い進捗や判断の記録はコメントに書け"),
            "{quiet_out}"
        );
        let silent = render(&full_context(), "artifacts");
        assert!(!silent.contains("コメント"), "{silent}");
    }

    /// 役割だけがあるときは、Phase 23 の `prompt_header` と同じ `## Role:` 節だけを出す。
    #[test]
    fn a_role_only_context_renders_exactly_the_old_role_section() {
        let with_instructions = RunContext {
            role: Some(RoleContext {
                id: "lead".into(),
                instructions: "You coordinate.".into(),
            }),
            ..RunContext::default()
        };
        assert_eq!(
            render(&with_instructions, "artifacts"),
            format!(
                "## Role: lead\nYou coordinate.\n\n{}",
                deliverables_placement_note()
            )
        );
        let bare = RunContext {
            role: Some(RoleContext {
                id: "lead".into(),
                instructions: String::new(),
            }),
            ..RunContext::default()
        };
        assert_eq!(
            render(&bare, "artifacts"),
            format!("## Role: lead\n\n{}", deliverables_placement_note())
        );
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
        assert!(
            out.starts_with(&ordinary_out),
            "対話の指示は末尾に足すだけ: {out}"
        );
        assert!(!out.contains("この返事では作業を始めないでください"));
        assert!(out.contains("実装・検証・成果の引き渡し"));
        assert!(out.contains("通常の修正と検証は既に依頼された作業"));
        assert!(out.contains("既存の仕事と結果、承認済みの範囲"));

        let other = RunContext {
            conversation_addressee: Some(ConversationAddressee::Other),
            ..RunContext::default()
        };
        let out = render(&other, "artifacts");
        assert!(out.contains("聞かれたことに答え"));
        assert!(!out.contains("(a) 理解の確認"), "{out}");
        assert!(
            out.contains("自分の直近の仕事とその結果は上に書いてある"),
            "{out}"
        );

        // 対話でない run（既定値の `None`）では何も足さない。
        assert_eq!(
            render(&RunContext::default(), "artifacts"),
            deliverables_placement_note()
        );
    }

    /// Phase 98（ADR-0018、実機障害 2026-09-22）: CoS の対話にだけ「クラスタ作業は自分でやらず
    /// `create_task` で組織に流す」規則が付き、「ssh 禁止・read-only」を理由に断らないよう明示する。
    /// CoS 以外の対話・通常の run には出ない。
    #[test]
    fn secretary_instructions_tell_cos_to_route_cluster_work_via_create_task() {
        let secretary = RunContext {
            conversation_addressee: Some(ConversationAddressee::Secretary),
            ..RunContext::default()
        };
        let out = render(&secretary, "artifacts");
        assert!(out.contains("自分で実行しないでください"), "{out}");
        assert!(
            out.contains("『ssh が禁止されている』『read-only』を理由に断らないで"),
            "{out}"
        );
        assert!(out.contains("cluster:<id>"), "{out}");
        assert!(
            out.contains(
                r#""workspace": {"kind":"remote","cluster":"<id>","path":"<作業ディレクトリ>","mode":"shared"}"#
            ),
            "{out}"
        );
        // ADR-0059 D6: `~` を既定にしない・未登録なら `path` を省略して人に登録を頼む規則が入る。
        assert!(out.contains("`~` は使わないでください"), "{out}");
        assert!(out.contains("path` を省略し"), "{out}");
        // ADR-0062 B（Phase 107）: 持たないノードに流すと検証で落ちる、調査・執筆系は remote にしない。
        assert!(out.contains("検証で落ちます"), "{out}");
        assert!(out.contains("web-research"), "{out}");
        assert!(out.contains("celeris が local に落とします"), "{out}");

        let other = RunContext {
            conversation_addressee: Some(ConversationAddressee::Other),
            ..RunContext::default()
        };
        let out = render(&other, "artifacts");
        assert!(!out.contains("cluster:<id>"), "{out}");

        assert_eq!(
            render(&RunContext::default(), "artifacts"),
            deliverables_placement_note()
        );
    }

    /// Phase 98（ADR-0046 D8）: CoS 宛ての「組織」一覧に、各ノードの tools（`cluster:<id>` を含む）が
    /// 1 語ずつ添う。CoS がどのノードにクラスタ作業を流せばよいかを前置きから判断できるようにする。
    #[test]
    fn organization_section_shows_each_nodes_tools() {
        let secretary = RunContext {
            conversation_addressee: Some(ConversationAddressee::Secretary),
            organization: vec![
                crate::protocol::OrgNodeContext {
                    id: "cluster-hpc".into(),
                    name: "Cluster & HPC Operations".into(),
                    kind: task_core::OrgKind::Section,
                    parent_id: Some("operations".into()),
                    brief: String::new(),
                    genre: None,
                    skills: vec!["slurm".into()],
                    harnesses: vec!["coding".into()],
                    tools: vec![
                        "cluster:pegasus".into(),
                        "cluster:sirius".into(),
                        "cluster:fern03".into(),
                    ],
                },
                crate::protocol::OrgNodeContext {
                    id: "cos".into(),
                    name: "Chief of Staff".into(),
                    kind: task_core::OrgKind::Secretary,
                    parent_id: None,
                    brief: String::new(),
                    genre: None,
                    skills: Vec::new(),
                    harnesses: vec!["conversation".into()],
                    tools: Vec::new(),
                },
            ],
            ..RunContext::default()
        };
        let out = render(&secretary, "artifacts");
        assert!(out.contains("## 組織"), "{out}");
        assert!(
            out.contains(
                "- `cluster-hpc` Cluster & HPC Operations — skills: slurm / harnesses: coding / 道具: cluster:pegasus, cluster:sirius, cluster:fern03"
            ),
            "{out}"
        );
        // 道具が無いノードは「道具:」を出さない（従来どおり）。
        assert!(
            out.contains("- `cos` Chief of Staff / harnesses: conversation\n"),
            "{out}"
        );
    }

    /// ADR-0059 D6（Phase 99）: CoS 宛てに「クラスタ」の節が付き、実効 work_dir の有無で文面が変わる
    /// （登録済みならそのパス、未登録なら `path` を省略して人に登録を頼む案内）。CoS 以外・
    /// `context.clusters` が空の run には出ない。
    #[test]
    fn clusters_section_shows_the_effective_work_dir_or_that_it_is_unregistered() {
        let secretary = RunContext {
            conversation_addressee: Some(ConversationAddressee::Secretary),
            clusters: vec![
                crate::protocol::ClusterContext {
                    id: "pegasus".into(),
                    connected: true,
                    work_dir: Some("/work/NBB/rmaeda".into()),
                },
                crate::protocol::ClusterContext {
                    id: "sirius".into(),
                    connected: false,
                    work_dir: None,
                },
            ],
            ..RunContext::default()
        };
        let out = render(&secretary, "artifacts");
        assert!(out.contains("## クラスタ"), "{out}");
        assert!(
            out.contains("- `pegasus`（接続中） 作業ディレクトリ: `/work/NBB/rmaeda`"),
            "{out}"
        );
        assert!(
            out.contains("- `sirius`（未接続） 作業ディレクトリ: 未登録"),
            "{out}"
        );

        let other = RunContext {
            conversation_addressee: Some(ConversationAddressee::Other),
            clusters: vec![crate::protocol::ClusterContext {
                id: "pegasus".into(),
                connected: true,
                work_dir: Some("/work/NBB/rmaeda".into()),
            }],
            ..RunContext::default()
        };
        assert!(!render(&other, "artifacts").contains("## クラスタ"));

        // 空なら Phase 98 までと 1 バイトも変わらない（節ごと出ない）。
        let empty = RunContext {
            conversation_addressee: Some(ConversationAddressee::Secretary),
            ..RunContext::default()
        };
        assert!(!render(&empty, "artifacts").contains("## クラスタ"));

        // Phase 99b（ADR-0059 追記）: 継続中の run（`session_diff` あり = 他の全量節は落ちる）でも
        // `context.clusters` が渡っていれば「クラスタ」節は描かれる（`render` は単一の経路で、差分専用
        // の描画経路は無い。node/organization は継続中は `None`/空になる想定を模して確認する）。
        let continuing = RunContext {
            conversation_addressee: Some(ConversationAddressee::Secretary),
            session_diff: vec!["新しい人の発言: 続き".into()],
            clusters: vec![crate::protocol::ClusterContext {
                id: "pegasus".into(),
                connected: true,
                work_dir: Some("/work/NBB/rmaeda".into()),
            }],
            ..RunContext::default()
        };
        let out = render(&continuing, "artifacts");
        assert!(out.contains("## クラスタ"), "{out}");
        assert!(
            out.contains("- `pegasus`（接続中） 作業ディレクトリ: `/work/NBB/rmaeda`"),
            "{out}"
        );
        assert!(
            !out.contains("## 組織"),
            "継続中は組織の一覧を流し直さない: {out}"
        );
    }

    /// ADR-0048 D3（Phase 60b）: CoS の対話にだけ「進行中の案件」の節と `actions` の説明が付く。
    /// CoS 以外の対話・通常の run には出ない。
    #[test]
    fn cos_conversations_show_active_projects_and_the_actions_instructions() {
        let secretary = RunContext {
            conversation_addressee: Some(ConversationAddressee::Secretary),
            active_projects: vec![crate::protocol::ActiveProjectContext {
                repos: vec!["agent-platform".into()],
                id: "01PROJECT".into(),
                title: "Pluvio".into(),
                status: "active".into(),
                milestones: vec![crate::protocol::ActiveMilestoneContext {
                    id: "01MILESTONE".into(),
                    title: "隣接領域の調査".into(),
                    status: "in_progress".into(),
                }],
            }],
            ..RunContext::default()
        };
        let out = render(&secretary, "artifacts");
        assert!(out.contains("## 進行中の案件"), "{out}");
        assert!(out.contains("01PROJECT"), "{out}");
        assert!(out.contains("Pluvio"), "{out}");
        assert!(
            out.contains("登録済み repos: `agent-platform`（使用時の project: `01PROJECT`）"),
            "{out}"
        );
        assert!(out.contains("隣接領域の調査"), "{out}");
        assert!(out.contains("actions"), "{out}");
        assert!(out.contains("create_task"), "{out}");
        assert!(out.contains("propose_project"), "{out}");
        assert!(out.contains("add_milestone"), "{out}");
        assert!(out.contains("ask_human"), "{out}");
        assert!(out.contains("mode: \"production\""), "{out}");
        // ADR-0069 D1（Phase 114）: CoS は担当とモデルを選ばない。
        assert!(
            out.contains("担当（`assignee`）とモデル（`tier`）は"),
            "{out}"
        );
        assert!(!out.contains("\"assignee\": null"), "{out}");
        // ADR-0063 D3（Phase 109）: 調査系の受け入れ条件は「未確認」の一文（または `partial_ok`）で
        // 1 件の欠落による全体不合格を避ける、という案内が付く。
        assert!(out.contains("未確認"), "{out}");
        assert!(out.contains("literature"), "{out}");
        assert!(out.contains("web-research"), "{out}");

        // CoS 以外の対話には「進行中の案件」も `actions` の説明も出ない。
        let other = RunContext {
            conversation_addressee: Some(ConversationAddressee::Other),
            active_projects: secretary.active_projects.clone(),
            ..RunContext::default()
        };
        let out = render(&other, "artifacts");
        assert!(!out.contains("## 進行中の案件"), "{out}");
        assert!(!out.contains("create_task"), "{out}");

        // 案件が無ければ節ごと出ない（既存の出力を変えない）。
        let empty = RunContext {
            conversation_addressee: Some(ConversationAddressee::Secretary),
            ..RunContext::default()
        };
        assert!(!render(&empty, "artifacts").contains("## 進行中の案件"));
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
                    Some(
                        "web search returned nothing (possible search path failure: expired key, CAPTCHA, or network block)",
                    ),
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
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("missing {needle:?} in:\n{out}"))
        };
        assert!(
            at("## 覚えていること") < at("## あなたの直近の仕事"),
            "{out}"
        );
        assert!(
            at("## あなたの直近の仕事") < at("## 直近のやり取り"),
            "{out}"
        );
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
        let without = RunContext {
            recent_work: Vec::new(),
            ..full_context()
        };
        assert!(!render(&without, "artifacts").contains("あなたの直近の仕事"));
        // 対話でない通常 run の前置きは 1 バイトも変わらない（既定値には `recent_work` が無い）。
        assert_eq!(
            render(&RunContext::default(), "artifacts"),
            deliverables_placement_note()
        );
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
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("missing {needle:?} in:\n{out}"))
        };
        // 節は「あなたの直近の仕事」の後、直近のやり取りの前。
        assert!(
            at("## 覚えていること") < at("## 途中目標『隣接領域の動向調査』のここまで"),
            "{out}"
        );
        assert!(
            at("## 途中目標『隣接領域の動向調査』のここまで") < at("## 直近のやり取り"),
            "{out}"
        );
        assert!(out.contains("(in_progress)"), "{out}");
        assert!(out.contains("### [done] web 調査"), "{out}");
        assert!(out.contains("要約: 候補を 3 本に絞った"), "{out}");
        assert!(out.contains("候補 A / 候補 B / 候補 C"), "{out}");
        // 指示は対話の指示の後ろ。
        assert!(
            at("CoS の対話と仕事の開始") < at("## 途中目標の判定をお願いする返事です"),
            "{out}"
        );
        assert!(out.contains("(c) **次の途中目標の提案**"), "{out}");
        assert!(out.contains("milestone_proposal"), "{out}");

        // レビューでない run には何も出ない（通常の対話 run の前置きは 1 バイトも変わらない）。
        let plain = RunContext {
            milestone_review: None,
            ..context.clone()
        };
        let plain_out = render(&plain, "artifacts");
        assert!(
            !plain_out.contains("途中目標の判定をお願いする返事です"),
            "{plain_out}"
        );
        assert!(!plain_out.contains("のここまで"), "{plain_out}");
    }

    /// ADR-0044 D7（Phase 57）: 「作業場所」に文書の書き方が 1 行出る（題名は 1 行目、紐付けは
    /// front matter の `tasks:`、既定のブランチには直接コミットしない）。
    #[test]
    fn the_workspace_section_says_how_to_write_documents() {
        let note = RepoNote {
            name: "benchfs".into(),
            dir: "/ws/01J/repos/benchfs".into(),
            git: true,
            branch: Some("celeris/01J".into()),
            base: Some("abc1234".into()),
            base_kind: Some("main".into()),
            description: None,
            check: vec![],
            docs: "docs".into(),
            deliverables: ".".into(),
        };
        let out = repos_note(std::slice::from_ref(&note));
        assert!(
            out.contains("文書は `/ws/01J/repos/benchfs/docs/` に Markdown で書く"),
            "{out}"
        );
        assert!(out.contains("題名は 1 行目の `# `"), "{out}");
        assert!(
            out.contains("front matter の `tasks: [<このタスクの id>]`"),
            "{out}"
        );
        assert!(out.contains("既定のブランチに直接コミットせず"), "{out}");
        // `[outputs] docs` を変えるとその場所になる。
        let moved = RepoNote {
            docs: "doc/pages".into(),
            ..note
        };
        assert!(
            repos_note(&[moved]).contains("文書は `/ws/01J/repos/benchfs/doc/pages/` に"),
            "{out}"
        );
        // リポジトリが無いタスクの前置きは 1 バイトも変わらない（空）。
        assert_eq!(repos_note(&[]), "");
    }

    /// ADR-0047 D2（Phase 61）: 知識の節は**索引だけ**（本文は入れない）。マウントごとに並び、
    /// D3 の使い方（`search` / `get` / `record`）が出る。マウントが無い run の前置きは
    /// Phase 60 までと 1 バイトも変わらない。
    #[test]
    fn the_knowledge_section_lists_the_index_of_every_mount_kind() {
        use task_core::{KnowledgeItem, KnowledgeMount};
        let mounts = vec![
            KnowledgeMount::kb("environment/clusters"),
            KnowledgeMount::repo("pluvio", None),
            KnowledgeMount::memory(Some("cluster-hpc".into())),
        ];
        let index = vec![
            KnowledgeItem {
                path: "environment/clusters/pegasus.md".into(),
                title: "pegasus の使い方".into(),
                tags: vec!["hpc".into(), "cluster".into()],
                scope: Some("environment".into()),
                ..KnowledgeItem::default()
            },
            // 別の scope のページはこのマウントには出ない。
            KnowledgeItem {
                path: "user/profile.md".into(),
                title: "人のプロフィール".into(),
                scope: Some("user".into()),
                ..KnowledgeItem::default()
            },
            KnowledgeItem {
                path: "docs/design.md".into(),
                title: "design.md".into(),
                scope: Some("repo:pluvio".into()),
                ..KnowledgeItem::default()
            },
            KnowledgeItem {
                path: "/home/u/.local/celeris/memory/cluster-hpc/notes.md".into(),
                title: "あなたの手帳（案件をまたぐ記憶）".into(),
                scope: Some("memory:cluster-hpc".into()),
                ..KnowledgeItem::default()
            },
        ];
        let out = knowledge_section(&mounts, &index);
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("missing {needle:?} in:\n{out}"))
        };
        assert!(
            out.starts_with("## 知識 (knowledge base — 索引だけ。本文は道具で読む)\n"),
            "{out}"
        );
        assert!(
            out.contains("あなたが読める知識: `kb:environment/clusters`、`repo:pluvio`、`memory:cluster-hpc`。"),
            "{out}"
        );
        // ADR-0047 D3 の案内文。
        assert!(
            out.contains("`celerisctl knowledge search <語> [--scope …]` で探し"),
            "{out}"
        );
        assert!(
            out.contains("`celerisctl knowledge get <path>` で読む"),
            "{out}"
        );
        assert!(out.contains("`celerisctl knowledge record"), "{out}");
        assert!(out.contains("一時的な情報・雑談・推測は入れない"), "{out}");
        // マウントごとに並ぶ（マウントの順）。
        assert!(
            at("### kb:environment/clusters") < at("### repo:pluvio"),
            "{out}"
        );
        assert!(
            at("### repo:pluvio") < at("### memory:cluster-hpc"),
            "{out}"
        );
        assert!(
            out.contains("- `environment/clusters/pegasus.md` — pegasus の使い方（hpc、cluster）"),
            "{out}"
        );
        assert!(out.contains("- `docs/design.md` — design.md\n"), "{out}");
        assert!(
            out.contains("/memory/cluster-hpc/notes.md` — あなたの手帳"),
            "{out}"
        );
        // マウントしていない scope のページは出ない（本文も出ない）。
        assert!(!out.contains("user/profile.md"), "{out}");

        // マウントが無い・索引が空なら節ごと出ない。
        assert_eq!(knowledge_section(&[], &index), "");
        assert_eq!(knowledge_section(&mounts, &[]), "");

        // `render` に入れても、他の節の後ろ（作業場所の後、役割の前）に 1 回だけ出る。
        let context = RunContext {
            knowledge: Some(crate::protocol::KnowledgeContext {
                mounts: mounts.clone(),
                index: index.clone(),
            }),
            ..full_context()
        };
        let rendered = render(&context, "artifacts");
        let at = |needle: &str| {
            rendered
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle:?}"))
        };
        assert_eq!(
            rendered.matches("## 知識 (knowledge base").count(),
            1,
            "{rendered}"
        );
        assert!(
            at("## 覚えていること") < at("## 知識 (knowledge base"),
            "{rendered}"
        );
        assert!(
            at("## 知識 (knowledge base") < at("## Role: literature-reader"),
            "{rendered}"
        );
        // 知識を渡さない run には `knowledge_section` の見出しが出ない（ADR-0067 D1 の「成果物の置き場所」
        // の節は知識ベースに触れるので、素朴な「知識」という文字列の有無ではなく見出しそのものを見る）。
        assert!(!render(&full_context(), "artifacts").contains("## 知識 (knowledge base"));
        assert_eq!(
            render(&RunContext::default(), "artifacts"),
            deliverables_placement_note()
        );
    }

    /// ADR-0047 D2: 索引は最大 200 件（溢れた分は件数だけ）。
    #[test]
    fn the_knowledge_index_is_capped_at_two_hundred_items() {
        use task_core::{KnowledgeItem, KnowledgeMount};
        let mounts = vec![KnowledgeMount::kb("user")];
        let index: Vec<KnowledgeItem> = (0..250)
            .map(|n| KnowledgeItem {
                path: format!("user/p{n}.md"),
                title: format!("page {n}"),
                scope: Some("user".into()),
                ..KnowledgeItem::default()
            })
            .collect();
        let out = knowledge_section(&mounts, &index);
        assert_eq!(
            out.matches("- `user/p").count(),
            task_core::knowledge::MAX_PREAMBLE_ITEMS
        );
        assert!(out.contains("- （ほか 50 件。`search` で探す）"), "{out}");
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
