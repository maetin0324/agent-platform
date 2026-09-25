//! `claude-code` アダプタ（DESIGN §5.4, ADR-0003 D7, ADR-0006）。
//!
//! `claude` CLI は celeris 独自のワーカープロトコルを話さない。`--output-format stream-json` が吐く
//! Claude Code 自身のイベント（`system`/`assistant`/`user`/`result`）を読み、結果ファイル規約
//! （ADR-0006 D3: `artifacts/result.json`）と `result` メッセージ（D4）から `RunOutcome` を合成する。
//! 生存監視（wall-clock・無出力タイムアウト・SIGTERM→SIGKILL）は `subprocess.rs` の低レベル部分を再利用する。

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use task_core::{Check, RateLimitObservation, Task, TaskKind, Usage};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
use crate::delegate_file::{clear_delegate_file, forward_delegate_file};
use crate::progress;
use crate::protocol::{Answer, Evidence, ProviderFailure, RunContext, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, adopt_result_json_written_under_work_dir, kill_now,
    read_line_limited, read_tail, reap_after_terminal, write_result_json,
};

/// `[adapters.claude_code]`（config.toml, ADR-0006 D6）。
#[derive(Debug, Clone)]
pub struct ClaudeCodeConfig {
    /// 起動するコマンド名／パス。既定 `"claude"`。
    pub command: String,
    /// 末尾に追加する引数。
    pub extra_args: Vec<String>,
    /// `--permission-mode`。既定 `"bypassPermissions"`（ADR-0006 D6: celeris は許可プロンプトに応答できない）。
    pub permission_mode: String,
    /// `--model`（省略時は claude の既定モデル）。
    pub model: Option<String>,
    /// 追加の環境変数（例: `CLAUDE_CONFIG_DIR`）。
    pub env: Vec<(String, String)>,
    /// ADR-0043 D3（Phase 56）: `Some` なら `claude` をコンテナの中で起こす（`container::wrap`）。
    /// TOML には書かない（ディスパッチャが `with_container` で入れる）。
    pub container: Option<crate::container::SharedPlan>,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            command: "claude".to_string(),
            extra_args: Vec::new(),
            permission_mode: "bypassPermissions".to_string(),
            model: None,
            env: Vec::new(),
            container: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeCodeAdapter {
    config: ClaudeCodeConfig,
}

impl ClaudeCodeAdapter {
    pub const ID: &'static str = "claude-code";

    pub fn new(config: ClaudeCodeConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WorkerAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn run(
        &self,
        req: RunRequest,
        run_id: &str,
        limits: RunLimits,
        sink: &dyn EventSink,
    ) -> Result<RunOutcome, AdapterError> {
        run_claude_code(&self.config, &req, run_id, &limits, sink).await
    }

    /// ADR-0024 D2: `extra` を `config.env` の末尾に足した複製を返す。同名キーは後勝ち（`envs()` に渡す順で
    /// 最後に指定した値が使われる）ので、末尾に足すだけで `extra` が既存の同名キーに勝つ。
    fn with_model(&self, model: &str) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.model = Some(model.to_owned());
        Some(Arc::new(Self::new(config)))
    }
    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.env.extend(extra.iter().cloned());
        Some(Arc::new(ClaudeCodeAdapter::new(config)))
    }

    /// ADR-0043 D3（Phase 56）: コンテナの中で `claude` を起こす複製。
    fn with_container(&self, plan: crate::container::SharedPlan) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.container = Some(plan);
        Some(Arc::new(ClaudeCodeAdapter::new(config)))
    }

    /// ADR-0072 D14（Phase E4b 項目3）: `--permission-mode` を上書きした複製。planner run に
    /// `[execution.planner].permission_mode`（既定 `"plan"`）を実際の CLI 引数へ反映するために使う
    /// （`with_model`/`with_env` と同じ形。ADR-0072「Phase E3 実装時の逸脱・明確化」で見送っていた
    /// フック）。
    fn with_permission_mode(&self, mode: &str) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.permission_mode = mode.to_owned();
        Some(Arc::new(ClaudeCodeAdapter::new(config)))
    }
}

/// ADR-0006 Phase 115 D1（本番障害 01M3915FARENW8M0JM11XVF6W0 / 01M38T8N17MEWPTJQXGX1TNYJD）:
/// `work_dir`（実際の cwd）が `workspace` と異なる run（部署のリポジトリの git worktree で走るタスク）
/// だけ、プロンプトの先頭に「cwd と成果物ディレクトリは別」の注意を 2 行足す。`result_json_instructions`
/// は既に `artifacts_dir` の絶対パスで書く（`RunRequest::artifacts_rel` が `work_dir.is_some()` のとき
/// 絶対パスを返す）が、その絶対パスの指示を読み飛ばして相対 `artifacts/` を書いてしまう事故があったため、
/// 冒頭で明示的に注意する。`work_dir` が無い・`workspace` と同じなら何も足さない（既存の文面は
/// 1 バイトも変わらない）。純粋関数。
pub(crate) fn work_dir_note(
    work_dir: Option<&Path>,
    workspace: &Path,
    artifacts_dir: &Path,
) -> String {
    match work_dir {
        Some(wd) if wd != workspace => format!(
            "cwd は `{}`（リポジトリの worktree）。成果物ディレクトリは `{}`。\
             相対 `artifacts/` はリポジトリの中を指すので使わない。\n\n",
            wd.display(),
            artifacts_dir.display()
        ),
        _ => String::new(),
    }
}

/// タスクからワーカーへのプロンプトを組み立てる（ADR-0006 D2, ADR-0007 D7, 純粋関数）。`run_id` は
/// スキーマ変更を避けてプロンプト文面にのみ埋め込む（旧 P-11。ADR-0006 D2 参照）。`task.kind` で分岐する
/// （`Plan` はプランナー用、`Review` はレビュアー用、それ以外は Phase 4 のワーカー用プロンプト。ADR-0007 D7）。
/// `artifacts` は成果物ディレクトリの workspace 相対表記（`RunRequest::artifacts_rel`。ADR-0036 D3。
/// 単独タスクでは `artifacts` なので、文面は Phase 34 までと 1 バイトも変わらない）。
pub fn build_prompt(task: &Task, context: &RunContext, run_id: &str, artifacts: &str) -> String {
    // ADR-0072 D14（Phase E3）: task-local な planner run は `task.kind` に依らず（常に `Execute`）、
    // `context.execution_planner` の有無で選ぶ。
    if context.execution_planner.is_some() {
        return build_execution_plan_prompt(task, context, run_id, artifacts);
    }
    match task.kind {
        TaskKind::Plan => build_plan_prompt(task, context, run_id, artifacts),
        TaskKind::Review => build_review_prompt(task, context, run_id, artifacts),
        TaskKind::Execute | TaskKind::Approval => {
            build_execute_prompt(task, context, run_id, artifacts)
        }
    }
}

/// 冒頭の共通部分（タイトル・run_id/attempt・前置き・分野・目的）。前置き（役職と brief・永続の認可・記憶・
/// 直近のやり取り・役割の指示文）は `crate::preamble::render` が組む（ADR-0016 D1 / M3, ADR-0033 D4 / D6）。
/// `task.genre` があり、その分野が `context.available_genres` に載っていれば、続けて `## Genre: <id>` と
/// 説明を出す（ADR-0027 D1）。
fn prompt_header(task: &Task, context: &RunContext, run_id: &str, artifacts: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Task: {}\n\n", task.title));
    out.push_str(&format!(
        "(run {run_id}, attempt {} of {})\n\n",
        task.attempts + 1,
        task.budget.max_retries + 1
    ));
    // ADR-0033 D4 / D6（Phase 24）: 役職と brief → 永続の認可 → 記憶 → 直近のやり取り → 役割の指示文。
    // 前置きは `crate::preamble` が 1 か所で組む（`RunContext` が空なら 1 バイトも増えない）。
    out.push_str(&crate::preamble::render(context, artifacts));
    if let Some(genre_id) = &task.genre
        && let Some(genre) = context.available_genres.iter().find(|g| &g.id == genre_id)
    {
        out.push_str(&format!(
            "## Genre: {}\n{}\n\n",
            genre.id, genre.description
        ));
    }
    // ADR-0072 D9/D21（Phase E2）: 計画のある Task の WU の run では、Task 全体の objective では
    // なく WU の objective を出す（`context.work_unit` が無い run は E1 までと 1 バイトも変わらない）。
    match &context.work_unit {
        Some(wu) => {
            out.push_str(&format!("## Objective\n{}\n\n", wu.objective));
            out.push_str(&format!(
                "## このタスク全体の目的（参考）\n{}\n\n",
                wu.task_objective_excerpt
            ));
            if !wu.plan_overview.is_empty() {
                out.push_str("## 計画の中のこの WorkUnit\n");
                for line in &wu.plan_overview {
                    out.push_str(&format!("- {line}\n"));
                }
                out.push('\n');
            }
            if !wu.dependency_summaries.is_empty() {
                out.push_str("## 依存する WorkUnit の完了状況\n");
                for line in &wu.dependency_summaries {
                    out.push_str(&format!("- {line}\n"));
                }
                out.push('\n');
            }
        }
        None => {
            out.push_str(&format!("## Objective\n{}\n\n", task.objective));
        }
    }
    out
}

/// `context.available_genres` の一覧を「使える専門家」の箇条書きに描く（ADR-0027 D1, ADR-0028 D2）。
/// `できること` / `渡すもの…返るもの` の行は、それぞれの一覧が空なら出さない（ADR-0028 D1: 3 フィールドとも任意）。
/// 見出しと、末尾の使い方の説明（委譲 or plan.json）は呼び出し側が足す。
fn genre_list_lines(context: &RunContext) -> String {
    let mut out = String::new();
    for g in &context.available_genres {
        out.push_str(&format!("- {}: {}\n", g.id, g.description));
        if !g.capabilities.is_empty() {
            out.push_str(&format!("  できること: {}\n", g.capabilities.join(" / ")));
        }
        if !g.input_artifacts.is_empty() || !g.output_artifacts.is_empty() {
            out.push_str(&format!(
                "  渡すもの: {} → 返るもの: {}\n",
                g.input_artifacts.join(", "),
                g.output_artifacts.join(", ")
            ));
        }
        let roles = if g.roles.is_empty() {
            "-".to_string()
        } else {
            g.roles
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!("  役割: {roles}\n"));
    }
    out
}

/// `context.available_genres` があれば「使える専門家」節を足す（ADR-0027 D1, ADR-0028 D2）。委譲できる run
/// （`build_execute_prompt`）にだけ、この run が子に割り当てられる分野の能力・入出力・役割の選択肢を伝える。
fn available_genres_section(context: &RunContext, artifacts: &str) -> String {
    let mut out = String::new();
    if context.available_genres.is_empty() {
        return out;
    }
    out.push_str("## 使える専門家 (available genres and roles you can delegate to)\n");
    out.push_str(&genre_list_lines(context));
    out.push_str(&format!(
        "\nIf part of this work belongs to a different genre, delegate it with `role` set to one \
         of that genre's roles and `genre` set to its id in `{artifacts}/delegate.json`.\n\n"
    ));
    out
}

/// Plan run 用の「使える専門家」節（ADR-0028 D3）。子タスクの `genre` / `role` を `artifacts/plan.json` で
/// 選べることを伝える点だけが `available_genres_section` と異なる（委譲ではなく分解なので）。
fn available_genres_section_for_plan(context: &RunContext, artifacts: &str) -> String {
    let mut out = String::new();
    if context.available_genres.is_empty() {
        return out;
    }
    out.push_str("## 使える専門家 (available genres and roles you can assign child tasks to)\n");
    out.push_str(&genre_list_lines(context));
    out.push_str(&format!(
        "\nIf a child task belongs to a different genre than this one, set its `genre` (and, one of \
         that genre's roles, its `role`) in `{artifacts}/plan.json`.\n\n"
    ));
    out
}

/// 1 つの分野の「成果物の名前は固定」の箇条書き（Phase 38。`名前: 説明` の `名前` と `説明` に分けて出す）。
fn harness_artifact_lines(genre: &crate::protocol::GenreContext) -> String {
    let mut out = format!(
        "- {}: この分野の担当は**ハーネス**で動く。成果物は次の名前で固定され、担当が別のファイルを書くことはできない。\n",
        genre.id
    );
    for (name, description) in genre.output_artifacts_named() {
        match description {
            Some(d) => out.push_str(&format!("  - `{name}`: {d}\n")),
            None => out.push_str(&format!("  - `{name}`\n")),
        }
    }
    out
}

/// Phase 38（ADR-0028 追記。実機のレビュー不合格から）: **Plan run** に、ハーネスで動く分野
/// （`GenreContext::is_harness`）の成果物の規約を出す。実機で、計画が研究文献調査課（PaperQA2）に
/// 「候補テーマを `candidates.json` にまとめよ」と書き、`artifact_exists: candidates.json` を条件に付けた。
/// `candidates.json`（現 `papers.json`）はハーネスが書く検索コーパスの固定名で、担当は計画が決めた名前の
/// ファイルを書けないため、答えの中身が良かったのにレビュアーが基準どおり不合格にした。
/// ハーネスでない分野（coding 等）しか無い設定では**何も出さない**（従来の文面と 1 バイトも変わらない）。
fn harness_artifacts_section_for_plan(context: &RunContext) -> String {
    let harness: Vec<&crate::protocol::GenreContext> = context
        .available_genres
        .iter()
        .filter(|g| g.is_harness() && !g.output_artifacts.is_empty())
        .collect();
    if harness.is_empty() {
        return String::new();
    }
    let mut out = String::from("## ハーネスで動く分野の成果物（名前は固定）\n");
    for genre in harness {
        out.push_str(&harness_artifact_lines(genre));
    }
    out.push_str(
        "受け入れ条件（`artifact_exists`）にはこの名前だけを使うこと。上に無い名前のファイルを要求しても、\n\
         その担当は書けない（条件は celeris が落とし、警告が残る）。**内容の要求は `objective` に書き、\n\
         レビュアー条件（`{\"type\":\"reviewer\"}`）で判定させること**（「X を Y に書け」ではなく\n\
         「答えに X を含めよ」）。\n\n",
    );
    out
}

/// ADR-0033 D4（Phase 24）: 組織図（id / name / brief / genre）。分解・委譲できる run にだけ渡り、
/// 「どの課に何を振るか」を `assignee` で決めさせる。空なら何も出さない（Phase 23 までと同じ出力）。
fn organization_section(context: &RunContext) -> String {
    let mut out = String::new();
    if context.organization.is_empty() {
        return out;
    }
    out.push_str("## 組織図 (who you can assign work to)\n");
    for n in &context.organization {
        let kind = match n.kind {
            task_core::OrgKind::Secretary => "秘書",
            task_core::OrgKind::Department => "部",
            task_core::OrgKind::Section => "課",
        };
        let parent = n.parent_id.as_deref().unwrap_or("-");
        let genre = n.genre.as_deref().unwrap_or("-");
        out.push_str(&format!(
            "- {} [{kind}] {} (親: {parent}, 分野: {genre})",
            n.id, n.name
        ));
        if !n.brief.is_empty() {
            out.push_str(&format!(" — {}", n.brief));
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

/// `context.children` があれば「集約 run」節を足す（ADR-0016 D3 / M4）。
fn children_section(context: &RunContext, artifacts: &str) -> String {
    let mut out = String::new();
    if context.children.is_empty() {
        return out;
    }
    out.push_str("## Delegated child tasks (this is the aggregate run)\n");
    for c in &context.children {
        let role = c.role.as_deref().unwrap_or("-");
        let status = serde_json::to_string(&c.status).unwrap_or_default();
        let status = status.trim_matches('"');
        let outcome = c.outcome.as_deref().unwrap_or("-");
        let workspace = c
            .workspace
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        let artifacts = if c.artifacts.is_empty() {
            "-".to_string()
        } else {
            c.artifacts
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        };
        // ADR-0041 D1: 子が worktree で作業したときだけブランチを足す（無い子の文面は従来どおり）。
        let branch = match &c.branch {
            Some(b) => format!(" branch={b}"),
            None => String::new(),
        };
        out.push_str(&format!(
            "- {} [{role}] status={status} outcome={outcome} workspace={workspace}{branch} artifacts={artifacts}\n",
            c.title
        ));
    }
    out.push_str(&format!(
        "\nSummarize the results of the delegated child tasks in `{artifacts}/summary.md`. \
         The reviewer will check that `{artifacts}/summary.md` exists.\n\n"
    ));
    out
}

/// ADR-0046 D2 / D4 / D5（Phase 59）: 計画には**人選をさせない**。
///
/// Phase 58 までは「上の組織図を見て子タスクごとに `assignee` を必ず書け」と指示していたが、
/// ADR-0046 D5 で担当は決定的な matching が決めるようになった。代わりに、子タスクごとに
/// **`harness` / `skills` / `mode`** を宣言させる（それが matching の入力になる）。
/// 組織図を渡していない run（Phase 23 までの構成）では何も出さない。
fn assignee_instructions_for_plan(context: &RunContext) -> String {
    if context.organization.is_empty() {
        return String::new();
    }
    "## 誰がやるか (ADR-0046 D5)\n\
     **`assignee`（担当）は書くな。組織図から人を選ぶ必要は無い。** 誰がやるかは celeris が決定的に決める\n\
     （必要な skill と harness の重なりで選ぶ）。代わりに、子タスクごとに次の 3 つを書け:\n\
     - `harness`: その仕事の実行契約の id（下の「使える分野」から選ぶ）\n\
     - `skills`: その仕事に**必要な能力タグ**の配列（例 `[\"rust\",\"sqlite\"]`）。小文字・`[a-z0-9._-]`、最大 12 個。\n\
     \u{3000}分からなければ空でよい（その harness を既定に持つ担当に回る）\n\
     - `mode`: `prototype`（動くことを最短で示す）/ `production`（既定。テストと lint を通す）/ \
     `research`（主張に出典か計測を付ける）\n\
     - `repos` と、仕事の制約（検証方法・戻せるか・失敗したときの損失）は objective と acceptance に書け。\n\
     **担当（`assignee`）とモデル（`tier`）は選ぶな。** 書いても使われない（ADR-0069: 担当は matching、\n\
     lane は仕事の性質から celeris が決定的に決める）。\n\n"
        .to_string()
}

/// 同じことを委譲（`artifacts/delegate.json`）側にも書く（ADR-0033 D4）。
/// 部をまたぐ委譲は秘書の認可が要るので、それも伝える（SPEC §3.1）。
fn assignee_instructions_for_delegation(context: &RunContext) -> String {
    if context.organization.is_empty() {
        return String::new();
    }
    "委譲する子には goal（title / objective）・受け入れ条件・`genre`（harness）を書け。**担当（`assignee`）と\n\
     モデル（`tier`）は選ぶな**。書いても使われない（ADR-0069: 担当は celeris が skills と harness から決定的に\n\
     選び、lane は仕事の性質から決める）。上の組織図は「どんな担当がいるか」を知るためだけに使え。\n\n"
        .to_string()
}

/// `context.prior_review` があれば「前回の判定」として列挙する（ADR-0006 D2）。
fn prior_review_section(context: &RunContext) -> String {
    let mut out = String::new();
    if !context.prior_review.is_empty() {
        out.push_str("## Previous attempt's review result (this is a retry)\n");
        for pr in &context.prior_review {
            let verdict = if pr.pass { "pass" } else { "fail" };
            out.push_str(&format!(
                "- criterion {}: {verdict} ({})\n",
                pr.criterion, pr.reason
            ));
        }
        out.push('\n');
    }
    out
}

/// `context.answers` があれば「以前の質問への人間の回答」節として列挙する（ADR-0010 D3, P-10）。
/// Review プロンプトには使わない（`build_review_prompt` からは呼ばない）。
fn answers_section(context: &RunContext) -> String {
    let mut out = String::new();
    if !context.answers.is_empty() {
        out.push_str("## Answers from a human to your earlier questions\n");
        for Answer { question, answer } in &context.answers {
            out.push_str(&format!("- Q: {question}\n  A: {answer}\n"));
        }
        out.push('\n');
    }
    out
}

/// 結果ファイル（`<artifacts_dir>/result.json`）の書式指示（ADR-0006 D3。全 kind 共通。ADR-0036 D3）。
fn result_json_instructions(artifacts: &str) -> String {
    format!(
        "Always write `{artifacts}/result.json` (create the `{artifacts}/` directory if it does not exist \
         yet) as a single JSON object of the form `{{\"summary\": \"<what you did>\", \"evidence\": []}}`. \
         `evidence` may be left empty; if you fill it, each element must be an object of the form \
         `{{\"criterion\": <index>, \"command\": \"<what you ran>\", \"exit\": <code>, \"stdout_tail\": \"...\"}}` \
         (plain strings are not accepted; `command`, `exit` and `stdout_tail` may be omitted for a criterion that \
         did not involve running a command). \
         If you cannot proceed and need a decision from a human, instead write \
         `{{\"question\": \"<your question>\"}}` to `{artifacts}/result.json` and stop there. This is a \
         non-interactive run: you cannot ask a question any other way, and no one will read your final \
         chat message directly.\n"
    )
}

/// ADR-0072 D10（Phase E1）: 予算の予告と rolling checkpoint の指示。coding 系の execute run
/// （対話は除く。D10）にだけ足す。claude-code は `--max-turns` で turn の上限を実際に強制するが、
/// codex/acp/aider は強制しない（目安として出す。§7 U1 と同じ「分類できなければ安全側」の考え方）。
/// **決定的**（壁時計の現在時刻は埋め込まない。DESIGN 原則 1 / 同じ入力から同じプロンプトを保つため。
/// 「開始時刻」は `WorkerStarted` イベントに残るので、ここに書かなくても run の記録からは追える）。
fn budget_preamble(task: &Task, artifacts: &str) -> String {
    format!(
        "## 予算 (budget)\n\
         この run の上限の目安: 最大 {} turn（claude-code はこれを `--max-turns` で実際に強制する。\
         他の harness では目安）/ 最大 {} 秒（壁時計はどの harness でも celeris が強制する）。\n\
         残りが約 20% になったら、または context が長くなってきたと感じたら、作業を区切りのよい所で \
         止め、`{artifacts}/checkpoint.json` を最新の状態にしてから `{artifacts}/result.json` に \
         `{{\"yield\": {{\"completed\": [...], \"remaining\": [...], \"next_action\": \"...\"}}}}` \
         （フィールドは全て任意、`checkpoint.json` と同じ形）を書いて終了せよ（予算切れで打ち切られる \
         より、区切って自分から止まる方が良い。これは失敗ではなく、続きは新しい session が checkpoint \
         から引き継ぐ）。\n\
         意味のある区切り（1 つの小目標の完了・方針の決定・テストの実行）のたびに \
         `{artifacts}/checkpoint.json` を次の形で上書きせよ（無ければ新規に作る。全欄が任意）:\n\
         `{{\"completed\":[\"...\"],\"remaining\":[\"...\"],\
         \"decisions\":[{{\"what\":\"...\",\"why\":\"...\"}}],\
         \"files_changed\":[{{\"path\":\"...\",\"change\":\"modified\"}}],\
         \"tests_run\":[{{\"command\":\"...\",\"exit\":0}}],\
         \"known_failures\":[{{\"what\":\"...\"}}],\"next_action\":\"...\"}}`\n\n",
        task.budget.max_turns, task.budget.max_wall_secs
    )
}

/// 実行中の委譲の方法（ADR-0016 D2 / M8, ADR-0027 D1）。
fn delegation_instructions(artifacts: &str) -> String {
    format!(
        "If you want to delegate part of this work to another agent, write `{artifacts}/delegate.json` \
         (create the `{artifacts}/` directory if it does not exist yet) as a single JSON object of the form \
         `{{\"tasks\":[{{\"title\":\"...\",\"objective\":\"...\",\"acceptance\":[{{\"text\":\"...\",\
         \"check\":{{\"type\":\"command\",\"cmd\":\"...\",\"expect_exit\":0}}}}],\"role\":\"<optional>\",\
         \"genre\":\"<optional>\",\
         \"depends_on\":[<index into this array, or an existing task id>]}}]}}`. \
         Do not choose an assignee or a model tier: celeris assigns the owner deterministically and picks the \
         model lane from the task's nature (ADR-0069). Describe the work, its acceptance checks, and constraints instead.\n\
         `check` may also be \
         `{{\"type\":\"artifact_exists\",\"name\":\"...\"}}`, `{{\"type\":\"reviewer\"}}`, or `{{\"type\":\"human\"}}`. \
         celeris will validate this after this run ends and insert whatever proposals pass validation as child \
         tasks (how many are accepted per run is limited by configuration; any rejected proposal has its \
         reason recorded as an event you cannot see, but a human can). A task cannot list its own parent or \
         itself in `depends_on`. This task will not be considered done until any children you delegated have \
         finished.\n"
    )
}

/// ADR-0039 D3: 案件が作業場所を決めている run にだけ、委譲の指示に「子は同じ作業場所を継ぐ」を足す。
/// 決めていない案件では空文字列（Phase 42 までと 1 バイトも変わらない）。
fn delegate_workspace_instruction(context: &RunContext) -> String {
    if context.workspace_note.is_none() {
        return String::new();
    }
    "Children you delegate inherit this project's workspace (the location described above), so do not \
     tell them to `ssh` into another host and edit files there. Only if a child must work somewhere else \
     (a different repository or cluster), give it a `workspace` of \
     `{\"kind\":\"local\",\"path\":\"...\"}` or `{\"kind\":\"remote\",\"cluster\":\"...\",\"path\":\"...\"}`.\n"
        .to_string()
}

/// ADR-0039 D3: 計画 run（`build_plan_prompt`）用。子タスクが継ぐ作業場所と、`plan.json` の `workspace` の
/// 使いどころを伝える。案件が作業場所を決めていない run では空文字列（従来どおりの文面）。
fn workspace_section_for_plan(context: &RunContext) -> String {
    let Some(note) = &context.workspace_note else {
        return String::new();
    };
    format!(
        "## 子タスクの作業場所 (the workspace child tasks inherit)\n\
         {note}\n\
         コードを扱う仕事はこの場所で行われ、分解した子タスクはこの作業場所をそのまま継ぐ。担当に \
         `ssh` でリモートの作業ツリーへ直接書かせてはいけない（同期は celeris が行う）。**別の場所**\
         （別のリポジトリ・別のクラスタ）で作業させたい子にだけ、`workspace` に \
         `{{\"kind\":\"local\",\"path\":\"...\"}}` か \
         `{{\"kind\":\"remote\",\"cluster\":\"...\",\"path\":\"...\"}}` を書け。\n\n"
    )
}

/// `Execute`（および `Approval`）用プロンプト（ADR-0006 D2。既存のワーカー用プロンプトのまま）。
fn build_execute_prompt(
    task: &Task,
    context: &RunContext,
    run_id: &str,
    artifacts: &str,
) -> String {
    let mut out = prompt_header(task, context, run_id, artifacts);
    // ADR-0072 D9（Phase E1）: 続きの実行（continuation）の節。`context.continuation` が無い run の
    // 出力はここで 1 バイトも増えない。
    out.push_str(&crate::preamble::continuation_section(context));
    // ADR-0072 D10（Phase E1）: 予算の予告と rolling checkpoint の指示。対話 run には出さない
    // （D10: 対話・レビュー・計画・研究 harness は除く）。
    if context.conversation_addressee.is_none() {
        out.push_str(&budget_preamble(task, artifacts));
    }
    // ADR-0072 D9（Phase E2）: 計画のある Task の WU の run では、`## Acceptance criteria` は
    // この WU の `done_when` にし、Task の受け入れ条件は「最終レビューで確かめる」参考として別に出す
    // （`context.work_unit` が無い run は E1 までと 1 バイトも変わらない）。
    if let Some(wu) = &context.work_unit {
        out.push_str("## Acceptance criteria (this WorkUnit)\n");
        if wu.done_when.is_empty() {
            out.push_str(
                "(no explicit done_when was given for this WorkUnit; use the objective above)\n",
            );
        }
        for (i, c) in wu.done_when.iter().enumerate() {
            out.push_str(&format!("{i}. {c}\n"));
        }
        out.push('\n');
        out.push_str("## 最終レビューで確かめる Task の受け入れ条件（参考）\n");
        for (i, c) in task.acceptance.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i, c.text));
        }
        out.push('\n');
    } else {
        out.push_str("## Acceptance criteria\n");
        for (i, c) in task.acceptance.iter().enumerate() {
            let detail = match &c.check {
                Check::Command { cmd, expect_exit } => format!(
                    " (a reviewer will independently re-run `{cmd}` in this directory afterwards and \
                     requires exit code {expect_exit}; your own claim of success is not trusted)"
                ),
                Check::ArtifactExists { name } => {
                    format!(" (a reviewer will check that the file `{artifacts}/{name}` exists)")
                }
                Check::KnowledgePage { path } => {
                    format!(" (a human will check the knowledge base page `{path}`)")
                }
                Check::Reviewer | Check::Human => String::new(),
            };
            out.push_str(&format!("{}. {}{}\n", i, c.text, detail));
        }
        out.push('\n');
    }
    out.push_str(&prior_review_section(context));
    out.push_str(&answers_section(context));
    out.push_str(&children_section(context, artifacts));
    out.push_str(&organization_section(context));
    out.push_str(&available_genres_section(context, artifacts));
    out.push_str(&assignee_instructions_for_delegation(context));
    out.push_str("## Instructions\n");
    out.push_str("Work in the current directory (it is a dedicated workspace for this task). ");
    // ADR-0054 D2 / Phase 98 追記: 対話 run（`conversation_addressee` が Some）は結果ファイルを書いて
    // 返事だけをする（`preamble::conversation_instructions` の `actions` 経由でしか仕事を作れない）。
    // 対話 run の一部（CoS）は read-only サンドボックスで `artifacts/delegate.json` を書けないため、
    // 汎用の delegate.json 段落をここで出すと「委譲ファイルを作成できない」と誤って諦める
    // （実機障害 2026-09-22 00:18 UTC、task 01M337NT3QT1FR1G6WHS9G6NDA）。対話でない run の文面は
    // 1 バイトも変えない。
    if context.conversation_addressee.is_some() {
        out.push_str(
            "この run は返事だけを書く。仕事は返事の `actions` で作る（ファイルは書けない）。\n",
        );
    } else {
        out.push_str(&delegation_instructions(artifacts));
        out.push_str(&delegate_workspace_instruction(context));
    }
    out.push_str("When you are done:\n");
    out.push_str(&result_json_instructions(artifacts));
    out
}

/// `Plan` kind 用プロンプト（DESIGN §5.6, ADR-0007 D7）。目標を独立に検証可能な受け入れ条件を持つ
/// 子タスク群に分解させ、`artifacts/plan.json` に `PlanOutput` を書かせる。
fn build_plan_prompt(task: &Task, context: &RunContext, run_id: &str, artifacts: &str) -> String {
    let mut out = prompt_header(task, context, run_id, artifacts);
    out.push_str(
        "## Instructions\n\
         Decompose this goal into a set of child tasks, each with an independently verifiable \
         acceptance criterion or criteria (DESIGN §5.6: \"目標を、独立に検証可能な受け入れ条件を持つ \
         子タスク群に分解せよ\"). Prefer 3 to 6 child tasks when the size of the goal makes that \
         reasonable (DESIGN §6 Phase 5 acceptance criteria); use fewer or more only if the goal \
         clearly requires it.\n\n",
    );
    out.push_str(&format!(
        "Write your decomposition to `{artifacts}/plan.json` (create the `{artifacts}/` directory if it \
         does not exist yet) as a single JSON object of exactly this shape:\n\
         ```json\n\
         {{\"tasks\":[{{\"title\":\"...\",\"objective\":\"...\",\
         \"acceptance\":[{{\"text\":\"...\",\"check\":{{\"type\":\"command\",\"cmd\":\"...\",\
         \"expect_exit\":0}}}}],\"depends_on\":[<index into this same tasks array>],\
         \"kind\":\"execute\"|\"plan\" (omit for \"execute\"),\
         \"harness\":\"<harness id>\", \"skills\":[\"<skill tag>\"], \
         \"mode\":\"prototype\"|\"production\"|\"research\" (optional), \
         \"repos\":[\"<repo name>\"] (optional)}}]}}\n\
         ```\n\
         Do not write `assignee` or `tier`: the owner and the model lane are decided by celeris (ADR-0069).\n\
         `check` may also be `{{\"type\":\"artifact_exists\",\"name\":\"...\"}}`, \
         `{{\"type\":\"knowledge_page\",\"path\":\"...\"}}` (a page in the knowledge base), \
         `{{\"type\":\"reviewer\"}}`, or `{{\"type\":\"human\"}}`. Unknown fields are rejected, so do not \
         add any field not shown above. `depends_on` indices refer to positions within this same \
         `tasks` array and must form a DAG (no self-reference, no cycles). Every child task must have \
         at least one `acceptance` entry. If a child has a `human` check, its `acceptance` must also \
         include an `artifact_exists` or `knowledge_page` check pointing at the human-readable material \
         (ADR-0067: it must live in registered artifacts or the knowledge base, not in the target \
         repository's tracked files, so a human can see it from the GUI). `kind:\"plan\"` children are \
         only allowed while the total decomposition depth stays within {} (DESIGN §5.6 \"分解の深さは \
         上限 3\"; this plan itself already counts toward that limit). Any `command` check will later be \
         re-run for real inside the child task's own working directory by an independent reviewer, so \
         do not fabricate a command whose result you have not actually observed.\n\n",
        task_core::plan::MAX_PLAN_DEPTH
    ));
    let schema = serde_json::to_string(&task_core::plan::schema_value())
        .unwrap_or_else(|_| "{}".to_string());
    out.push_str(&format!(
        "### Schema for the `{artifacts}/plan.json` object\n```json\n"
    ));
    out.push_str(&schema);
    out.push_str("\n```\n\n");
    out.push_str(&prior_review_section(context));
    out.push_str(&answers_section(context));
    // ADR-0046 D5（Phase 59）: 計画に組織図は渡さない（人選をさせない）。代わりに harness / skills /
    // mode を宣言させ、担当は決定的な matching が決める。
    out.push_str(&assignee_instructions_for_plan(context));
    out.push_str(&available_genres_section_for_plan(context, artifacts));
    out.push_str(&workspace_section_for_plan(context));
    out.push_str(&harness_artifacts_section_for_plan(context));
    out.push_str(&result_json_instructions(artifacts));
    out
}

/// ADR-0072 D14（Phase E3）: task-local な planner run 用プロンプト。Complexity Gate が compound と
/// 判定した Task を WorkUnit の集合に分解させ、`artifacts/execution-plan.json` に
/// `ExecutionPlanSpec`（`celeris.execution-plan/1`）を書かせる。この run 自身は普通のワーカー run と
/// 同じ `result.json` の契約（`summary`/`evidence`、または `question`）で終わる。計画の検証・採用・
/// 不正なら 1 回だけ再試行・それでも駄目なら atomic に倒す判断は daemon（`task-dispatch`）が行う
/// （このプロンプトの役目ではない）。
fn build_execution_plan_prompt(
    task: &Task,
    context: &RunContext,
    run_id: &str,
    artifacts: &str,
) -> String {
    let mut out = prompt_header(task, context, run_id, artifacts);
    out.push_str(
        "## Instructions\n\
         This task was judged too large or too broad for a single session (ADR-0072 Complexity Gate). \
         Decompose it into a small number of WorkUnits, each of which fits comfortably in one context \
         window (investigation, design, implementation, tests, release — whatever split makes sense for \
         this goal). Prefer the smallest number of WorkUnits that keeps each one focused; do not split \
         further than the goal actually requires. You are planning, not doing the work yourself: do not \
         write code or run the actual implementation in this run.\n\n",
    );
    let planner_schema = task_core::EXECUTION_PLAN_SCHEMA;
    let max_work_units = context
        .execution_planner
        .as_ref()
        .map(|p| p.max_work_units)
        .unwrap_or(8);
    let work_unit_max_turns = context
        .execution_planner
        .as_ref()
        .map(|p| p.work_unit_max_turns)
        .unwrap_or(80);
    let work_unit_max_wall_secs = context
        .execution_planner
        .as_ref()
        .map(|p| p.work_unit_max_wall_secs)
        .unwrap_or(3600);
    let default_max_turns = context
        .execution_planner
        .as_ref()
        .map(|p| p.default_max_turns)
        .unwrap_or(30);
    let default_max_wall_secs = context
        .execution_planner
        .as_ref()
        .map(|p| p.default_max_wall_secs)
        .unwrap_or(1800);
    out.push_str(&format!(
        "Write your plan to `{artifacts}/execution-plan.json` (create the `{artifacts}/` directory if it \
         does not exist yet) as a single JSON object of exactly this shape:\n\
         ```json\n\
         {{\"schema\":\"{planner_schema}\",\"rationale\":\"...\",\"work_units\":[{{\"key\":\"survey\",\
         \"kind\":\"investigate\"|\"design\"|\"implement\"|\"test\"|\"release\"|\"repair\"|\"other\",\
         \"title\":\"...\",\"objective\":\"...\",\"depends_on\":[\"<key of another work unit>\"],\
         \"done_when\":[\"...\"],\"checks\":[{{\"cmd\":\"...\",\"expect_exit\":0}}],\
         \"context\":{{\"paths\":[\"...\"],\"from_work_units\":[\"<key>\"],\"knowledge\":[\"...\"]}},\
         \"harness\":\"<genre id, or omit to inherit this task's genre>\",\
         \"budget\":{{\"max_turns\":40,\"max_wall_secs\":1800}} (optional),\
         \"outputs\":[\"...\"]}}]}}\n\
         ```\n\
         Do not write `assignee`, `tier`, or `model`: the owner never changes (WorkUnits inherit this \
         task's owner) and the model lane is decided by celeris from each WorkUnit's nature (ADR-0069). \
         Unknown fields are rejected, so do not add any field not shown above. `key` must match \
         `[a-z0-9-]{{1,32}}` and be unique within this plan. `depends_on` refers to other `key`s in this \
         same array and must form a DAG (no cycles). `checks` may only be `{{\"cmd\":\"...\",\
         \"expect_exit\":0}}` (a deterministic command check; do not fabricate one you have not actually \
         run — it will really be executed later). `harness`, if set, must be one of the genre ids listed \
         below (available genres); an unset `harness` inherits this task's own genre. Plan at most \
         {max_work_units} WorkUnits (a plan with more will be rejected and this run retried once). If \
         you write a `budget` for a WorkUnit, `max_turns` will be capped at {work_unit_max_turns} and \
         `max_wall_secs` at {work_unit_max_wall_secs}; if you omit it, the default is \
         max({default_max_turns}, task budget) turns and max({default_max_wall_secs}, task budget) \
         seconds.\n\n"
    ));
    let schema = serde_json::to_string(&task_core::execution_plan::schema_value())
        .unwrap_or_else(|_| "{}".to_string());
    out.push_str(&format!(
        "### Schema for the `{artifacts}/execution-plan.json` object\n```json\n"
    ));
    out.push_str(&schema);
    out.push_str("\n```\n\n");
    if let Some(planner) = &context.execution_planner {
        out.push_str(&format!(
            "## Why this task was judged compound (Complexity Gate, rule `{}`, score {})\n",
            planner.gate_rule_id, planner.gate_score
        ));
        if planner.gate_signals.is_empty() {
            out.push_str(
                "(no scored signals — a human or the CoS marked this task compound directly)\n\n",
            );
        } else {
            for line in &planner.gate_signals {
                out.push_str(&format!("- {line}\n"));
            }
            out.push('\n');
        }
    }
    // ADR-0072 D17（Phase E4b 項目1）: replan のときだけ、今の計画・WU の状態・起こした理由を足す
    // （`replan = false` の run は 1 バイトも変わらない）。
    if let Some(planner) = context.execution_planner.as_ref().filter(|p| p.replan) {
        out.push_str(&replan_context_section(planner));
    }
    if !context.available_genres.is_empty() {
        out.push_str("## Available genres (valid values for a WorkUnit's `harness`)\n");
        out.push_str(&genre_list_lines(context));
        out.push('\n');
    }
    out.push_str(&prior_review_section(context));
    out.push_str(&answers_section(context));
    out.push_str(&result_json_instructions(artifacts));
    out
}

/// ADR-0072 D17（Phase E4b 項目1）: replan run のプロンプトに足す節。今の計画の版・WU ごとの状態・
/// 起こした理由・保持すべき `done` の WU の key を出す。`v2` の出力は `done` の WU を変えてはいけない、
/// と明示する（D14 の検証がこれを拒否することも書く）。
fn replan_context_section(planner: &crate::protocol::ExecutionPlannerContext) -> String {
    let mut out = String::from("## You are REPLANNING an existing execution plan\n");
    out.push_str(
        "This is not the first plan for this task: a previous plan already ran, and something \
         about it needs to change. Write a full new plan version (not a diff) that reflects the \
         current reality below.\n\n",
    );
    if let Some(version) = planner.current_plan_version {
        out.push_str(&format!("Current (superseded) plan version: v{version}.\n"));
    }
    out.push_str(&format!(
        "Why this replan was triggered: {}\n\n",
        if planner.replan_reason.is_empty() {
            "(not recorded)"
        } else {
            planner.replan_reason.as_str()
        }
    ));
    if !planner.work_unit_summaries.is_empty() {
        out.push_str("### WorkUnits in the current plan\n");
        for line in &planner.work_unit_summaries {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }
    if planner.preserve_done_keys.is_empty() {
        out.push_str(
            "No WorkUnit in the current plan is done yet, so you may replace all of them.\n\n",
        );
    } else {
        out.push_str(&format!(
            "IMPORTANT: these WorkUnit keys are already done and MUST appear unchanged (same \
             `key`, same spec — objective, depends_on, done_when, checks, context, harness, \
             budget, outputs) in your new plan; do not edit, rename, remove, or reorder them. A \
             plan that changes a done WorkUnit will be rejected: {}.\n\n",
            planner.preserve_done_keys.join(", ")
        ));
    }
    out
}

/// Phase 38（ADR-0028 追記）: レビュー対象のタスクがハーネスで動く分野なら、レビュアーにも成果物の規約を
/// 渡す（`context.subject_genre`。ディスパッチャが決定的に入れる）。ハーネスでない分野・分野が無いタスクの
/// レビューでは何も出さない（従来の文面と 1 バイトも変わらない）。
fn harness_artifacts_section_for_review(context: &RunContext) -> String {
    let Some(genre) = context.subject_genre.as_ref().filter(|g| g.is_harness()) else {
        return String::new();
    };
    let names = genre.output_artifacts_named();
    let Some(&(answer, _)) = names.first() else {
        return String::new();
    };
    let mut out = String::from("## この担当の成果物（名前は固定。判定はこの前提で行う）\n");
    out.push_str(&harness_artifact_lines(genre));
    out.push_str(&format!(
        "上のどれかに「答え」があり、それ以外は道具の記録である（例: `papers.json` は検索したコーパスで\n\
         あって答えではない。答えは `{answer}`）。テーマ候補・考察・結論といった**内容は `{answer}` の中で\n\
         判定せよ**。受け入れ条件が上に無いファイル名を求めていても、その担当にはそれを書く手段が無いので、\n\
         ファイル名の不一致だけを理由に不合格にはせず、要求された**内容**が `{answer}` にあるかで判定せよ。\n\n"
    ));
    out
}

/// `Review` kind 用プロンプト（DESIGN §5.7, ADR-0007 D5/D7）。対象タスクの成果物を読み取り専用で
/// 検証し `artifacts/review.json` に判定を書かせる。
fn build_review_prompt(task: &Task, context: &RunContext, run_id: &str, artifacts: &str) -> String {
    let mut out = prompt_header(task, context, run_id, artifacts);
    out.push_str(
        "## Instructions\n\
         You are a reviewer independently verifying another worker's output. You must not modify any \
         files in this directory — this is a read-only inspection of the working directory. If in \
         doubt about whether a criterion is actually satisfied, set `pass` to false and explain why: a \
         false pass is worse than a false fail (completion is decided by review, not by the worker's \
         own claim).\n\n",
    );
    out.push_str("## Acceptance criteria of the task under review\n");
    for (i, c) in task.acceptance.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i, c.text));
    }
    out.push('\n');
    out.push_str(&harness_artifacts_section_for_review(context));
    match &context.review {
        Some(review) => {
            out.push_str(&format!(
                "## Worker's self-reported summary (not to be trusted blindly)\n{}\n\n",
                review.summary
            ));
            out.push_str("## Criteria you must judge in this run\n");
            for idx in &review.criteria {
                out.push_str(&format!("- criterion {idx}\n"));
            }
            out.push('\n');
            out.push_str("## Evidence self-reported by the worker (not to be trusted blindly)\n");
            if review.evidence.is_empty() {
                out.push_str("(none reported)\n");
            }
            for e in &review.evidence {
                // ADR-0012 D3: command / exit / stdout_tail は任意。
                let command = e
                    .command
                    .as_deref()
                    .map(|c| format!(" command `{c}`"))
                    .unwrap_or_default();
                let exit = e.exit.map(|x| format!(" exit={x}")).unwrap_or_default();
                let tail = e
                    .stdout_tail
                    .as_deref()
                    .map(|t| format!(" stdout_tail={t:?}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- criterion {}:{command}{exit}{tail}\n",
                    e.criterion
                ));
            }
            out.push('\n');
        }
        None => {
            out.push_str("## Review context\nno review context\n\n");
        }
    }
    out.push_str("## Input artifacts produced by the run under review\n");
    if context.inputs.is_empty() {
        out.push_str("(none)\n");
    }
    for a in &context.inputs {
        out.push_str(&format!(
            "- {} at `{}` (sha256={})\n",
            a.name, a.path, a.sha256
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "## Result\n\
         Write your verdicts to `{artifacts}/review.json` (create the `{artifacts}/` directory if it does \
         not exist yet) as a single JSON object of exactly this shape: \
         `{{\"verdicts\":[{{\"criterion\":<index>,\"pass\":<bool>,\"reason\":\"...\"}}]}}`. You must write \
         exactly one verdict for each criterion listed under \"Criteria you must judge in this run\" \
         above.\n\n"
    ));
    out.push_str(&result_json_instructions(artifacts));
    out
}

/// `artifacts/result.json`（ADR-0006 D3）。
#[derive(Debug, Deserialize)]
struct ResultFile {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    question: Option<String>,
    /// 生の JSON で受け、`lenient_evidence` で整形する（ADR-0006 D3: `evidence` の内容の正確さは要求しない。
    /// Phase 5 のドッグフードで、ワーカーが文字列の配列を書いて run 全体が `error` になる事故があった）。
    #[serde(default)]
    evidence: serde_json::Value,
    /// ADR-0072 D9/D10（Phase E1）: graceful yield（`{"yield": {...checkpoint の意味の欄...}}`）。
    /// 中身は寛容に読む（`task_core::WorkerCheckpointInput` と同じ形。生の JSON のまま保持し、
    /// 検証・合成はディスパッチャ側の `task_core::merge_checkpoint` が行う）。
    #[serde(default, rename = "yield")]
    r#yield: Option<serde_json::Value>,
}

/// `evidence` のうち `Evidence` として読めた要素だけを残す。配列でない／要素が不正でも `done` を失敗にしない。
fn lenient_evidence(value: serde_json::Value) -> Vec<Evidence> {
    match value {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| serde_json::from_value::<Evidence>(item).ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// stream-json の最後に観測した `{"type":"result",...}`（ADR-0006 D4）。
#[derive(Debug, Clone)]
struct ResultMeta {
    subtype: String,
    is_error: bool,
    usage: Option<Usage>,
    /// `result` フィールド（文字列。エラー時の文面）。供給側失敗の分類に使う（ADR-0010 D5）。
    result: Option<String>,
}

async fn run_claude_code(
    config: &ClaudeCodeConfig,
    req: &RunRequest,
    run_id: &str,
    limits: &RunLimits,
    sink: &dyn EventSink,
) -> Result<RunOutcome, AdapterError> {
    let run_dir = req.workspace.join("runs").join(run_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    let stdout_log_path = run_dir.join("stdout.jsonl");
    let stderr_log_path = run_dir.join("stderr.log");
    // `stderr_task` (below) moves a copy into its `async move` block; this one stays available for
    // the crash-classification read after the loop (ADR-0010 D5).
    let stderr_log_path_for_task = stderr_log_path.clone();

    // 前回の run（リトライ）が残した結果ファイルを、今回の run の結果と誤読しないよう先に消す
    // （監査で指摘。ADR-0006 D3 は「この run が書いたファイル」を前提にしている）。
    // ADR-0036 D1/D2: 置き場はディスパッチャが決めた `artifacts_dir`（共有 workspace ではタスクごと）。
    let artifacts_rel = req.artifacts_rel();
    let result_path = req.artifact_path("result.json");
    let _ = tokio::fs::remove_file(&result_path).await;
    clear_delegate_file(&req.artifacts_dir).await;
    // ADR-0006 Phase 115 D2: 同じ理由（前回の run の名残と誤読しない）で、work_dir 側の名残候補も消す
    // （`work_dir != workspace` のときだけ意味がある。無ければ何もしない）。
    if let Some(work_dir) = req.work_dir.as_deref()
        && work_dir != req.workspace
    {
        let _ = tokio::fs::remove_file(work_dir.join("artifacts").join("result.json")).await;
    }

    let prompt = format!(
        "{}{}",
        work_dir_note(req.work_dir.as_deref(), &req.workspace, &req.artifacts_dir),
        build_prompt(&req.task, &req.context, run_id, &artifacts_rel)
    );
    // ADR-0023 D2 / M1: この run で何を渡したかを残す（`request.json` は構造、`prompt.txt` は実際の文面）。
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &prompt, run_id).await;
    // ADR-0056 D3（Phase 79）: mount された skills を `.claude/skills/<name>/` に写す（claude-code が
    // 自動で読む形式。run が失敗しても打ち切らない。読み取れる限りは失敗しない見込み — 失敗すれば
    // ワーカー起動前の警告としてログに残す）。
    if let Err(e) = crate::skills::deliver_claude_code(req.cwd(), &req.context.skills).await {
        warn!("run {run_id}: failed to deliver skills to .claude/skills: {e}");
    }

    let mut command = Command::new(&config.command);
    command
        .arg("-p")
        .arg(&prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--permission-mode")
        .arg(&config.permission_mode)
        .arg("--max-turns")
        .arg(req.task.budget.max_turns.to_string());
    // ADR-0054 D1（Phase 67）: `context.session`（この run が継続セッションの一部）が無ければ、
    // Phase 66 までと同じ `--no-session-persistence`（session を残さない）。ある場合は、そのアダプタが
    // `claude-code` のときだけ、初回は `--session-id <id>`（これから使う id を固定）、2 回目以降は
    // `--resume <id>`（続ける）に切り替える。他アダプタ向けの `session` は無視する（渡り歩きは無い）。
    // ADR-0054 D1（Phase 67）: `resume` を頼んだ run かどうかは、後段の crash 分類（resume 失敗の
    // 検出）でも使う。
    let is_resuming = req
        .context
        .session
        .as_ref()
        .is_some_and(|s| s.adapter == ClaudeCodeAdapter::ID && s.resume);
    // ADR-0054 Phase 67b 追記: Claude Code CLI 2.1.278 は `--session-id`/`--resume` に渡す id が UUID
    // でなければ拒否する（本番で ULID を渡してすべての CoS 対話・部門長レビュー run が失敗した事故。
    // 2026-09-21）。celeris 側の発行（`crate::sessions` 相当。呼び出し元は `resolve_node_session` の
    // 自己修復）は Phase 67b で直したが、ここでも**境界で** spawn 前に拒否する（将来の回帰がテストで
    // 静かに ULID を通してしまわないよう、falsely-loud に落とす）。
    match req.context.session.as_ref() {
        Some(session) if session.adapter == ClaudeCodeAdapter::ID && session.resume => {
            if !crate::provider::is_valid_uuid(&session.session_id) {
                return Err(AdapterError::Other(format!(
                    "refusing to --resume claude-code session id {:?}: not a valid UUID \
                     (Claude Code CLI 2.1.278+ requires one; ADR-0054 Phase 67b)",
                    session.session_id
                )));
            }
            command.arg("--resume").arg(&session.session_id);
        }
        Some(session) if session.adapter == ClaudeCodeAdapter::ID => {
            if !crate::provider::is_valid_uuid(&session.session_id) {
                return Err(AdapterError::Other(format!(
                    "refusing to --session-id claude-code session id {:?}: not a valid UUID \
                     (Claude Code CLI 2.1.278+ requires one; ADR-0054 Phase 67b)",
                    session.session_id
                )));
            }
            command.arg("--session-id").arg(&session.session_id);
        }
        _ => {
            command.arg("--no-session-persistence");
        }
    }
    if let Some(model) = &config.model {
        command.arg("--model").arg(model);
    }
    // ADR-0054 D2（Phase 68）: CoS の対話 run だけ、読み取りだけの `celerisctl` を許す
    // （`Bash(celerisctl <サブコマンド>:*)` の形。claude-code の `--allowedTools` はこの許可リストに
    // 無い道具を拒否する＝それ以外は禁止のまま。ADR-0033 D4 の「対話 run は道具を使わない」の例外）。
    if req.context.conversation_addressee == Some(crate::protocol::ConversationAddressee::Secretary)
    {
        let allowed = crate::protocol::CONVERSATION_READONLY_CELERISCTL
            .iter()
            .map(|sub| format!("Bash(celerisctl {sub}:*)"))
            .collect::<Vec<_>>()
            .join(",");
        command.arg("--allowedTools").arg(allowed);
    }
    command.args(&config.extra_args);
    command
        .envs(config.env.iter().cloned())
        .current_dir(req.cwd());
    // ★ ADR-0043 D3 の差し込み点（コンテナ実行）。`None` ならそのまま（ホスト実行は変わらない）。
    let mut command = crate::container::wrap(command, config.container.as_deref());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(AdapterError::Spawn)?;
    // ADR-0044 §5 Phase 53 追記（Phase 55）: この run のプロセスグループを覚える（`kill_tree` の入口）。
    let _process_group = crate::process_group::ProcessGroup::register(run_id, child.id());
    // ADR-0054 D1（Phase 67）: 起動できたら、このアダプタ宛ての継続セッションの id をそのまま報告する
    // （claude-code は id を`自分で`固定するので、成功した spawn の直後に確定する）。
    if let Some(session) = req
        .context
        .session
        .as_ref()
        .filter(|s| s.adapter == ClaudeCodeAdapter::ID)
    {
        sink.session_established(&session.session_id);
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AdapterError::Other("worker stdout was not piped".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AdapterError::Other("worker stderr was not piped".into()))?;

    let stderr_task = tokio::spawn(async move {
        let mut reader = stderr;
        match tokio::fs::File::create(&stderr_log_path_for_task).await {
            Ok(mut file) => {
                if let Err(e) = tokio::io::copy(&mut reader, &mut file).await {
                    warn!("failed to write worker stderr.log: {e}");
                }
            }
            Err(e) => warn!("failed to create worker stderr.log: {e}"),
        }
    });

    let mut stdout_file = tokio::fs::File::create(&stdout_log_path).await?;
    let mut reader = BufReader::new(stdout);

    let start = Instant::now();
    let mut last_activity = Instant::now();
    let mut last_result: Option<ResultMeta> = None;
    let mut force_kill = false;
    let mut timeout_terminal: Option<Terminal> = None;

    loop {
        let wall_elapsed = start.elapsed();
        if wall_elapsed >= limits.wall_clock {
            // ADR-0072 D7（Phase E1）: wall-clock の打ち切りは予算切れ（continuation の対象）。
            // usage はここでは取れない（`result` メッセージを観測する前に打ち切っている）。
            timeout_terminal = Some(Terminal::BudgetExhausted {
                kind: task_core::BudgetKind::WallClock,
                message: "wall clock exceeded".into(),
                usage: None,
            });
            force_kill = true;
            break;
        }
        let idle_elapsed = last_activity.elapsed();
        if idle_elapsed >= limits.idle_timeout {
            // ADR-0072 D7（Phase E1）: idle timeout は E1 では harness_error に分類変更しない
            // （§7 U7。従来どおり retryable な `Error` のまま attempts を消費する）。
            timeout_terminal = Some(Terminal::Error {
                message: "idle timeout".into(),
                retryable: true,
            });
            force_kill = true;
            break;
        }
        let wait = (limits.wall_clock - wall_elapsed).min(limits.idle_timeout - idle_elapsed);

        let outcome = match tokio::time::timeout(
            wait,
            read_line_limited(&mut reader, MAX_LINE_BYTES),
        )
        .await
        {
            Err(_elapsed) => continue, // タイムアウト。ループ先頭で上限超過を検知する。
            Ok(Err(e)) => return Err(AdapterError::Io(e)),
            Ok(Ok(outcome)) => outcome,
        };

        match outcome {
            LineOutcome::Eof => break,
            LineOutcome::TooLong => {
                // claude 自身のフォーマットは celeris が定義したものではないため、寛容に無視する（ADR-0006 D5）。
                sink.heartbeat();
                last_activity = Instant::now();
                warn!("run {run_id}: discarding overlong line from claude stdout");
            }
            LineOutcome::Line(bytes) => {
                sink.heartbeat();
                last_activity = Instant::now();
                stdout_file.write_all(&bytes).await?;
                stdout_file.write_all(b"\n").await?;
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    handle_line(trimmed, sink, &mut last_result);
                }
            }
        }
    }

    let exit_status = if force_kill {
        kill_now(&mut child, limits.kill_grace).await?
    } else {
        reap_after_terminal(&mut child, limits.kill_grace).await?
    };

    if let Err(e) = stderr_task.await {
        warn!("run {run_id}: stderr capture task failed: {e}");
    }
    stdout_file.flush().await?;

    let (terminal, provider_failure): (Terminal, Option<ProviderFailure>) =
        match (timeout_terminal, &last_result) {
            // タイムアウト（wall-clock / idle）は分類しない（ADR-0010 D5）。
            (Some(t), _) => (t, None),
            // `result` メッセージを一度も観測できずに exit した場合はクラッシュとして扱い、
            // artifacts/result.json（前回の run の名残や書きかけの内容）を一切信用しない（ADR-0006 D4）。
            // stderr.log の末尾を供給側失敗として分類する（ADR-0010 D5）。
            (None, None) => {
                let exit_repr = match exit_status.code() {
                    Some(code) => code.to_string(),
                    None => "signal".to_string(),
                };
                let tail = read_tail(&stderr_log_path, 4096).await;
                let pf = classify_provider_failure(&tail);
                // ADR-0054 D1（Phase 67）: resume を頼んだ run が、セッションを拒否されたように見える
                // crash なら報告する（ディスパッチャが `node_sessions` を retire し、次の run は新規
                // セッションになる）。文言は実機で確認していない（`provider::looks_like_resume_rejection`
                // のコメント参照）。
                if is_resuming && crate::provider::looks_like_resume_rejection(&tail) {
                    sink.session_resume_failed(&tail);
                }
                (
                    Terminal::Error {
                        message: format!(
                            "worker exited without a result message (exit={exit_repr})"
                        ),
                        retryable: true,
                    },
                    pf,
                )
            }
            (None, Some(meta)) => {
                // ADR-0006 Phase 115 D2: `result.json` を読む前に、work_dir 側の名残を採用する
                // （Phase 112 D3 相当の「回収」はこのアダプタには無いが、同じ原則で最優先に判定する）。
                adopt_result_json_written_under_work_dir(
                    &req.artifacts_dir,
                    req.work_dir.as_deref(),
                    &req.workspace,
                    run_id,
                )
                .await;
                let outcome = terminal_from_result(
                    &req.artifacts_dir,
                    &artifacts_rel,
                    meta,
                    config.model.as_deref(),
                )
                .await;
                // ADR-0054 D1（Phase 113 追記）: `result` メッセージは一度観測できたが、供給側の失敗
                // （`subtype: error_during_execution` かつ `is_error`）として終わった run は、上の
                // `(None, None)` のクラッシュ分類（resume 拒否の検出）を一切通らなかった。本番の
                // タスク 01M35X86XTK84F97QW0CN5PGMR / reviewer run 01M388BENASH3JEBWFS03KEQYT はこの
                // 穴に落ちた（stderr は `No conversation found with session ID: …` の 1 行だったが、
                // stdout の `result` の `result` フィールドにはその文言が無かったため、`result` を
                // 観測できてしまった＝ここに来て、resume 拒否として扱われなかった）。resume を頼んだ
                // run が `error_during_execution`/`is_error` で終わったときは、stderr の末尾と
                // `result` メッセージ自身の文面の両方を resume 拒否の文言と照らす。
                if is_resuming && meta.is_error && meta.subtype == "error_during_execution" {
                    let tail = read_tail(&stderr_log_path, 4096).await;
                    let result_text = meta.result.as_deref().unwrap_or("");
                    if crate::provider::looks_like_resume_rejection(&tail)
                        || crate::provider::looks_like_resume_rejection(result_text)
                    {
                        sink.session_resume_failed(&tail);
                    }
                }
                outcome
            }
        };

    forward_delegate_file(&req.artifacts_dir, sink).await;

    write_result_json(&run_dir, &terminal, provider_failure).await?;

    if let (Terminal::Error { message, .. }, Some(pf)) = (&terminal, provider_failure) {
        return Err(AdapterError::from_provider_failure(pf, message));
    }
    // ADR-0072 E2（P-E0-2 の修正）: `result.json` そのものが無かった run（`RESULT_JSON_MISSING_MARKER`）
    // は `ProviderFailure` を持たない `Err(AdapterError)` にする。`provider_failure_outcome` は
    // これを分類できない失敗として扱い、ディスパッチャは ADR-0070 D3 の `InfraRequeue`
    // （attempts を消費しない。上限に達したときだけ `WorkerError{retryable:false}`）に倒す。
    if let Terminal::Error { message, .. } = &terminal
        && message.starts_with(RESULT_JSON_MISSING_MARKER)
    {
        return Err(AdapterError::Other(message.clone()));
    }

    Ok(RunOutcome {
        terminal,
        exit_code: exit_status.code(),
    })
}

/// 壁時計の Unix 秒（ADR-0024 D4: 観測時刻は celeris の壁時計）。`claude_account` の確認・ログイン中継からも使う。
pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// stream-json の 1 行を解釈する。既知でない `type` や JSON として不正な行は無視する（ADR-0006 D5）。
fn handle_line(line: &str, sink: &dyn EventSink, last_result: &mut Option<ResultMeta>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let Some(ty) = value.get("type").and_then(|t| t.as_str()) else {
        return;
    };
    if ty == "rate_limit_event"
        && let Some(obs) = RateLimitObservation::from_stream_json(&value, now_unix_secs())
    {
        sink.rate_limit(obs);
    }
    match ty {
        // ADR-0048 D2（Phase 60a）: stream-json → 正規化した進行。`msg` の文面は Phase 59 までと同じ
        // （`tool_use: <名前> <入力>` / 本文そのまま）で、構造化フィールドを**足すだけ**。
        // 本文（`text`）は 1 つの assistant メッセージ分をまとめて 1 件にする。
        "assistant" => {
            if let Some(content) = value.pointer("/message/content").and_then(|c| c.as_array()) {
                let mut texts: Vec<&str> = Vec::new();
                let flush = |texts: &mut Vec<&str>, sink: &dyn EventSink| {
                    if texts.is_empty() {
                        return;
                    }
                    let joined = texts.join("\n");
                    texts.clear();
                    let msg = truncate(&joined, 500);
                    sink.progress_with(&msg, &progress::text(&joined));
                };
                for item in content {
                    match item.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                texts.push(text);
                            }
                        }
                        Some("thinking") => {
                            flush(&mut texts, sink);
                            // 思考は**要約だけ**（本文は流さない）。要約が取れなければ何も出さない。
                            let summary = item
                                .get("thinking")
                                .or_else(|| item.get("text"))
                                .and_then(|t| t.as_str())
                                .unwrap_or("");
                            if !summary.trim().is_empty() {
                                let fields = progress::thinking(&progress::one_line(summary));
                                let msg = format!(
                                    "thinking: {}",
                                    fields.summary.clone().unwrap_or_default()
                                );
                                sink.progress_with(&msg, &fields);
                            }
                        }
                        Some("tool_use") => {
                            flush(&mut texts, sink);
                            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                            let input = item.get("input");
                            let shown = input
                                .map(|v| truncate(&v.to_string(), 200))
                                .unwrap_or_default();
                            sink.progress_with(
                                &format!("tool_use: {name} {shown}"),
                                &progress::tool_use(name, input),
                            );
                        }
                        _ => {}
                    }
                }
                flush(&mut texts, sink);
            }
        }
        // 道具の結果は `user` メッセージに `tool_result` として返る（`is_error` が失敗の印）。
        "user" => {
            if let Some(content) = value.pointer("/message/content").and_then(|c| c.as_array()) {
                for item in content {
                    if item.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                        continue;
                    }
                    let body = tool_result_text(item);
                    let error = item
                        .get("is_error")
                        .and_then(|b| b.as_bool())
                        .unwrap_or(false);
                    let fields = progress::tool_result(None, &body, error);
                    let head = fields.summary.clone().unwrap_or_default();
                    let msg = if error {
                        format!("tool_result (error): {head}")
                    } else {
                        format!("tool_result: {head}")
                    };
                    sink.progress_with(&msg, &fields);
                }
            }
        }
        "result" => {
            let subtype = value
                .get("subtype")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let is_error = value
                .get("is_error")
                .and_then(|b| b.as_bool())
                .unwrap_or(subtype != "success");
            // ADR-0061（Phase 104）: claude-code CLI の `result.usage` は Anthropic API と同じ形
            // （`cache_creation_input_tokens` / `cache_read_input_tokens` を含む）。`cost_usd` はここでは
            // 計算しない（model 文字列は呼び出し元でしか分からない。`terminal_from_result` が埋める）。
            let usage = value.get("usage").map(|u| Usage {
                input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()),
                output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()),
                cache_read_tokens: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()),
                cache_creation_tokens: u
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64()),
                cost_usd: None,
            });
            let result = value
                .get("result")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string());
            *last_result = Some(ResultMeta {
                subtype,
                is_error,
                usage,
                result,
            });
        }
        _ => {}
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// ADR-0048 D2: `tool_result` の本文。Claude Code は文字列の `content` と、`[{"type":"text","text":…}]`
/// の配列の両方を出す（どちらも読む）。
fn tool_result_text(item: &serde_json::Value) -> String {
    let Some(content) = item.get("content") else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(parts) = content.as_array() {
        let joined: Vec<&str> = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect();
        if !joined.is_empty() {
            return joined.join("\n");
        }
    }
    content.to_string()
}

/// `result` メッセージと結果ファイルから終端を合成する（ADR-0006 D3/D4, ADR-0010 D5）。呼び出し元は
/// `result` メッセージを一度でも観測できた場合にのみこれを呼ぶ（観測できなかった場合は
/// クラッシュとして扱い、この関数を呼ばずに `Error` にする。ADR-0006 D4）。`is_error`/`subtype != "success"`
/// のときは `result` のテキスト（無ければ `subtype`）を供給側失敗として分類する。
/// ADR-0061（Phase 104）: model が分かる時だけ静的単価表から USD を推定して埋める（不明なモデル・
/// token 欠落は `None` のまま。ここでは断定しない）。
fn usage_with_cost(usage: Option<Usage>, model: Option<&str>) -> Option<Usage> {
    usage.map(|mut u| {
        if let Some(model) = model {
            u.cost_usd = task_core::estimate_cost_usd(model, &u);
        }
        u
    })
}

/// ADR-0072 E2（P-E0-2 の修正）: `result` メッセージは観測できたのに `result.json` 自体が無いときの
/// `Terminal::Error.message` の接頭辞。`run` がこの接頭辞を見て `Err(AdapterError)`（InfraRequeue）に
/// 倒す（他の `result.json` の失敗〈壊れた JSON・必須欄の欠落〉は worker 自身の誤りのまま）。
const RESULT_JSON_MISSING_MARKER: &str = "claude exited without ";

async fn terminal_from_result(
    artifacts_dir: &Path,
    artifacts_rel: &str,
    last_result: &ResultMeta,
    model: Option<&str>,
) -> (Terminal, Option<ProviderFailure>) {
    if last_result.is_error || last_result.subtype != "success" {
        // ADR-0072 D7（Phase E1）: `error_max_turns` は `--max-turns` の上限に当たったという
        // claude-code 自身の分類なので、字句判定なしで構造化できる（wall-clock は別経路。上の呼び出し元
        // の loop の timeout で検出する）。それ以外の `is_error`/`subtype != success` は、`result` の
        // 文言が context 超過の語彙に当たれば `BudgetExhausted{kind: Context}`（§7 U1: 実機の文言は
        // 未確認。分類できなければ従来どおり `Error`）。usage は予算切れでも運ぶ（従来は捨てていた）。
        let usage = usage_with_cost(last_result.usage, model);
        if last_result.subtype == "error_max_turns" {
            return (
                Terminal::BudgetExhausted {
                    kind: task_core::BudgetKind::Turns,
                    message: "claude result: error_max_turns".to_string(),
                    usage,
                },
                None,
            );
        }
        let text_for_classification = last_result
            .result
            .clone()
            .unwrap_or_else(|| last_result.subtype.clone());
        if task_core::looks_like_context_exceeded(&text_for_classification) {
            return (
                Terminal::BudgetExhausted {
                    kind: task_core::BudgetKind::Context,
                    message: format!(
                        "claude result: {}: {text_for_classification}",
                        last_result.subtype
                    ),
                    usage,
                },
                None,
            );
        }
        let pf = classify_provider_failure(&text_for_classification);
        let message = match &last_result.result {
            Some(result_text) => format!("claude result: {}: {result_text}", last_result.subtype),
            None => format!("claude result: {}", last_result.subtype),
        };
        return (
            Terminal::Error {
                message,
                retryable: true,
            },
            pf,
        );
    }

    let result_path = artifacts_dir.join("result.json");
    let text = match tokio::fs::read_to_string(&result_path).await {
        Ok(t) => t,
        Err(_) => {
            // ADR-0072 E2（P-E0-2 の修正）: `result` メッセージは観測できた（= claude 自身は正常に
            // 終わったと申告した）のに `result.json` そのものが無い（Phase 112 D3 / 115 D2 の
            // work_dir 側の回収を試みた後もなお無い）。これは worker 自身の判断の誤りというより、
            // 書き込みが間に合わなかった・消えたという供給側/インフラ側の事情に近い。呼び出し元
            // （`run`）はこの文言（`RESULT_JSON_MISSING_MARKER`）を見て `Err(AdapterError)`
            // （`ProviderFailure` は無し）に倒し、ADR-0070 D3 どおり attempts を消費しない
            // `InfraRequeue` の経路に乗せる（従来は `Ok(Terminal::Error{retryable:true})` になり、
            // `WorkerError{true}` として attempts を消費していた）。
            return (
                Terminal::Error {
                    message: format!("{RESULT_JSON_MISSING_MARKER}{artifacts_rel}/result.json"),
                    retryable: true,
                },
                None,
            );
        }
    };

    let terminal = match serde_json::from_str::<ResultFile>(&text) {
        Ok(rf) => {
            // ADR-0072 D9: 優先順位は `question` > `summary` > `yield`。
            if let Some(question) = rf.question {
                Terminal::Question { text: question }
            } else if let Some(summary) = rf.summary {
                Terminal::Done {
                    summary,
                    evidence: lenient_evidence(rf.evidence),
                    usage: usage_with_cost(last_result.usage, model),
                }
            } else if let Some(checkpoint) = rf.r#yield {
                Terminal::Yielded {
                    checkpoint,
                    usage: usage_with_cost(last_result.usage, model),
                }
            } else {
                Terminal::Error {
                    message: format!(
                        "{artifacts_rel}/result.json has neither 'summary', 'question' nor 'yield'"
                    ),
                    retryable: true,
                }
            }
        }
        Err(e) => Terminal::Error {
            message: format!("{artifacts_rel}/result.json is not valid JSON: {e}"),
            retryable: true,
        },
    };
    (terminal, None)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use task_core::{ArtifactRef, DelegateTask, RateLimitObservation};

    use super::*;
    use crate::protocol::{GenreContext, PROTOCOL_VERSION, RunContext};

    #[derive(Default)]
    struct RecordingSink {
        progress: Mutex<Vec<String>>,
        /// ADR-0048 D2（Phase 60a）: 構造化した進行（`msg` と一緒に）。
        structured: Mutex<Vec<(String, task_core::ProgressFields)>>,
        delegated: Mutex<Vec<Vec<DelegateTask>>>,
        rate_limits: Mutex<Vec<RateLimitObservation>>,
        /// ADR-0054 D1（Phase 67）: `session_established` に報告された id。
        session_established: Mutex<Vec<String>>,
        /// ADR-0054 D1（Phase 67）: `session_resume_failed` に報告された理由。
        session_resume_failed: Mutex<Vec<String>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(msg.to_string());
        }
        fn progress_with(&self, msg: &str, fields: &task_core::ProgressFields) {
            self.progress(msg);
            self.structured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((msg.to_string(), fields.clone()));
        }
        fn artifact(&self, _artifact: &ArtifactRef) {}
        fn delegate(&self, tasks: &[DelegateTask]) {
            self.delegated
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(tasks.to_vec());
        }
        fn rate_limit(&self, obs: RateLimitObservation) {
            self.rate_limits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(obs);
        }
        fn session_established(&self, session_id: &str) {
            self.session_established
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(session_id.to_string());
        }
        fn session_resume_failed(&self, reason: &str) {
            self.session_resume_failed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(reason.to_string());
        }
    }

    fn stub_claude(dir: &Path, script: &str) -> ClaudeCodeConfig {
        let path = dir.join("claude_stub.sh");
        // ETXTBSY 対策（ADR-0010 D10）: テストプロセス自身が書き込み fd を持たないよう別プロセスで書く。
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        ClaudeCodeConfig {
            command: path.to_string_lossy().into_owned(),
            ..ClaudeCodeConfig::default()
        }
    }

    fn sample_req(workspace: std::path::PathBuf) -> RunRequest {
        RunRequest {
            protocol: PROTOCOL_VERSION,
            task: crate::protocol::tests::sample_task(),
            artifacts_dir: workspace.join("artifacts"),
            workspace,
            work_dir: None,
            context: RunContext::default(),
        }
    }

    fn default_limits() -> RunLimits {
        RunLimits {
            wall_clock: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
            kill_grace: Duration::from_millis(200),
        }
    }

    #[test]
    fn build_prompt_includes_objective_criteria_and_result_file_instructions() {
        let task = crate::protocol::tests::sample_task();
        let mut context = RunContext::default();
        context.prior_review.push(crate::protocol::PriorReview {
            criterion: 0,
            pass: false,
            reason: "cargo test exit 101".into(),
        });
        let prompt = build_prompt(&task, &context, "run-xyz", "artifacts");
        assert!(prompt.contains(&task.objective));
        assert!(prompt.contains("cargo test exit 101"));
        assert!(prompt.contains("artifacts/result.json"));
        assert!(prompt.contains("reviewer will independently re-run"));
        assert!(prompt.contains("run-xyz"));
        assert!(prompt.contains("attempt 1 of"));
    }

    /// ADR-0072 D9/D21（Phase E2）: `context.work_unit` があれば `## Objective` は WU の objective に
    /// 差し替わり、Task 全体の目的は参考として、受け入れ条件は WU の `done_when` になる。
    /// `context.work_unit` が無い run は前のテストのとおり 1 バイトも変わらない。
    #[test]
    fn build_prompt_replaces_the_objective_and_acceptance_with_the_work_unit_when_present() {
        let task = crate::protocol::tests::sample_task();
        let context = RunContext {
            work_unit: Some(crate::protocol::WorkUnitPromptContext {
                key: "core-model".into(),
                title: "core model".into(),
                objective: "add the WorkUnit data model".into(),
                done_when: vec!["cargo test -p task-core passes".into()],
                task_objective_excerpt: task.objective.clone(),
                dependency_summaries: vec!["survey: 完了".into()],
                plan_overview: vec!["survey done, core-model running, tests pending".into()],
            }),
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-wu", "artifacts");
        assert!(prompt.contains("add the WorkUnit data model"));
        assert!(prompt.contains("## このタスク全体の目的（参考）"));
        assert!(prompt.contains(&task.objective));
        assert!(prompt.contains("cargo test -p task-core passes"));
        assert!(prompt.contains("## 最終レビューで確かめる Task の受け入れ条件（参考）"));
        assert!(prompt.contains("survey: 完了"));
        assert!(prompt.contains("survey done, core-model running, tests pending"));
    }

    /// ADR-0072 D10（Phase E1）: 予算の予告と rolling checkpoint の指示は execute run（対話を除く）
    /// に出るが、対話 run には出ない（D10 の除外表のとおり）。
    #[test]
    fn budget_preamble_appears_for_execute_runs_but_not_conversation_runs() {
        let task = crate::protocol::tests::sample_task();
        let ordinary = build_prompt(&task, &RunContext::default(), "run-budget", "artifacts");
        assert!(ordinary.contains("## 予算 (budget)"), "{ordinary}");
        assert!(ordinary.contains("checkpoint.json"), "{ordinary}");
        assert!(ordinary.contains(&format!("{}", task.budget.max_turns)));
        assert!(ordinary.contains(&format!("{}", task.budget.max_wall_secs)));

        let conversation_context = RunContext {
            conversation_addressee: Some(crate::protocol::ConversationAddressee::Other),
            ..RunContext::default()
        };
        let conversation = build_prompt(&task, &conversation_context, "run-conv", "artifacts");
        assert!(!conversation.contains("## 予算 (budget)"), "{conversation}");
    }

    /// ADR-0072 D9（Phase E1）: `request.json`/`prompt.txt` に続きの実行の節と checkpoint が載る。
    /// `context.continuation` を持たない run のプロンプトは、D10 の追加分を除きバイト単位で同じ
    /// （budget_preamble/continuation_section 以外の内容は変わらない）。
    #[test]
    fn build_prompt_carries_the_continuation_section_when_present() {
        let task = crate::protocol::tests::sample_task();
        let context = RunContext {
            continuation: Some(crate::protocol::ContinuationContext {
                run_seq: 2,
                previous_end: "budget_exhausted(turns)".into(),
                checkpoint: serde_json::json!({
                    "completed": ["A"],
                    "remaining": ["B"],
                    "next_action": "do B",
                }),
                prior_runs: vec!["Run #1 budget_exhausted(turns)".into()],
            }),
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-cont", "artifacts");
        assert!(prompt.contains("## 続きの実行（Run #2）"), "{prompt}");
        assert!(
            prompt.contains("### checkpoint（Run #1 の終わり）"),
            "{prompt}"
        );
        assert!(prompt.contains("do B"), "{prompt}");
        // 前の run の会話全文は載らない。
        assert!(
            !prompt.contains("prior conversation transcript"),
            "{prompt}"
        );
    }

    /// Phase 98（ADR-0054 D2、実機障害 2026-09-22）: 対話 run（`conversation_addressee` が Some）の
    /// 前置きには `artifacts/delegate.json` の段落が出ず、代わりに「返事だけを書く」1 文が入る。
    /// 対話でない run の前置きは Phase 97 までと 1 バイトも変わらない。
    #[test]
    fn conversation_runs_do_not_get_the_delegate_json_paragraph() {
        let task = crate::protocol::tests::sample_task();
        let ordinary = build_prompt(&task, &RunContext::default(), "run-ord", "artifacts");
        assert!(ordinary.contains("artifacts/delegate.json"), "{ordinary}");
        assert!(
            !ordinary.contains("この run は返事だけを書く"),
            "{ordinary}"
        );

        let secretary_context = RunContext {
            conversation_addressee: Some(crate::protocol::ConversationAddressee::Secretary),
            ..RunContext::default()
        };
        let secretary = build_prompt(&task, &secretary_context, "run-cos", "artifacts");
        assert!(
            !secretary.contains("artifacts/delegate.json"),
            "{secretary}"
        );
        assert!(
            secretary.contains(
                "この run は返事だけを書く。仕事は返事の `actions` で作る（ファイルは書けない）。"
            ),
            "{secretary}"
        );

        let other_context = RunContext {
            conversation_addressee: Some(crate::protocol::ConversationAddressee::Other),
            ..RunContext::default()
        };
        let other = build_prompt(&task, &other_context, "run-other", "artifacts");
        assert!(!other.contains("artifacts/delegate.json"), "{other}");
        assert!(
            other.contains(
                "この run は返事だけを書く。仕事は返事の `actions` で作る（ファイルは書けない）。"
            ),
            "{other}"
        );

        // 対話でない run（Phase 97 までの構成）は前置きが 1 バイトも変わらない。
        assert_eq!(
            ordinary,
            build_prompt(&task, &RunContext::default(), "run-ord", "artifacts")
        );
    }

    /// `context.answers`（ADR-0010 D3, P-10）は Execute/Plan プロンプトに反映される。
    #[test]
    fn build_prompt_includes_answers_from_human_for_execute_and_plan() {
        let mut context = RunContext::default();
        context.answers.push(crate::protocol::Answer {
            question: "which crate version?".into(),
            answer: "1.0".into(),
        });

        let execute_task = crate::protocol::tests::sample_task();
        let execute_prompt = build_prompt(&execute_task, &context, "run-a1", "artifacts");
        assert!(execute_prompt.contains("## Answers from a human to your earlier questions"));
        assert!(execute_prompt.contains("- Q: which crate version?"));
        assert!(execute_prompt.contains("A: 1.0"));

        let mut plan_task = crate::protocol::tests::sample_task();
        plan_task.kind = task_core::TaskKind::Plan;
        let plan_prompt = build_prompt(&plan_task, &context, "run-a2", "artifacts");
        assert!(plan_prompt.contains("## Answers from a human to your earlier questions"));
        assert!(plan_prompt.contains("- Q: which crate version?"));

        // No answers: the section must not appear at all.
        let no_answers_prompt =
            build_prompt(&execute_task, &RunContext::default(), "run-a3", "artifacts");
        assert!(!no_answers_prompt.contains("Answers from a human"));
    }

    #[test]
    fn build_prompt_for_plan_kind_includes_schema_and_plan_json_instructions() {
        let mut task = crate::protocol::tests::sample_task();
        task.kind = task_core::TaskKind::Plan;
        let mut context = RunContext::default();
        context.prior_review.push(crate::protocol::PriorReview {
            criterion: 0,
            pass: false,
            reason: "tasks[2].depends_on[0] = 7 is out of range".into(),
        });
        let prompt = build_prompt(&task, &context, "run-plan-1", "artifacts");
        assert!(prompt.contains("artifacts/plan.json"));
        assert!(prompt.contains("\"tasks\""));
        assert!(prompt.contains("depends_on"));
        assert!(prompt.contains(&task_core::MAX_PLAN_DEPTH.to_string()));
        assert!(prompt.contains("PlanOutput") || prompt.contains("NewTask"));
        assert!(prompt.contains("tasks[2].depends_on[0] = 7 is out of range"));
        assert!(prompt.contains("artifacts/result.json"));
    }

    #[test]
    fn build_prompt_for_review_kind_includes_review_json_and_context() {
        let mut task = crate::protocol::tests::sample_task();
        task.kind = task_core::TaskKind::Review;
        let context = RunContext {
            review: Some(crate::protocol::ReviewRequest {
                summary: "added usage example".into(),
                evidence: vec![crate::protocol::Evidence {
                    criterion: 0,
                    command: Some("cargo test".into()),
                    exit: Some(0),
                    stdout_tail: Some("test result: ok".into()),
                }],
                criteria: vec![0],
            }),
            inputs: vec![ArtifactRef {
                name: "readme.diff".into(),
                path: "artifacts/readme.diff".into(),
                sha256: "deadbeef".into(),
                kind: "diff".into(),
                declared: true,
            }],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-review-1", "artifacts");
        assert!(prompt.contains("artifacts/review.json"));
        assert!(prompt.contains("criterion 0"));
        assert!(prompt.contains("added usage example"));
        assert!(prompt.contains("cargo test"));
        assert!(prompt.contains("artifacts/readme.diff"));
        assert!(prompt.contains("read-only"));

        // context.review = None must not panic and still produces a usable prompt.
        let none_context = RunContext::default();
        let prompt_none = build_prompt(&task, &none_context, "run-review-2", "artifacts");
        assert!(prompt_none.contains("no review context"));
        assert!(prompt_none.contains("artifacts/review.json"));
    }

    /// ADR-0048 D2（Phase 60a）: stream-json の実物に近い標本（`tests/fixtures/claude-code-stream.jsonl`）を
    /// 1 行ずつ `handle_line` に通し、`tool_use` / `tool_result` / `text` / `thinking` の写像を確かめる。
    /// 外部ネットワークには出ない（ファイルを読むだけ）。
    #[test]
    fn stream_json_maps_to_structured_progress() {
        use task_core::ProgressKind;

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/claude-code-stream.jsonl"
        );
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let sink = RecordingSink::default();
        let mut last_result = None;
        for line in text.lines() {
            handle_line(line, &sink, &mut last_result);
        }
        let items = sink
            .structured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let kinds: Vec<Option<ProgressKind>> = items.iter().map(|(_, f)| f.kind).collect();
        assert_eq!(
            kinds,
            vec![
                Some(ProgressKind::Thinking),
                Some(ProgressKind::Text),
                Some(ProgressKind::ToolUse),
                Some(ProgressKind::ToolResult),
                Some(ProgressKind::ToolUse),
                Some(ProgressKind::ToolResult),
                Some(ProgressKind::ToolUse),
                Some(ProgressKind::ToolResult),
                Some(ProgressKind::ToolUse),
            ],
            "{items:#?}"
        );

        // thinking は要約だけ（本文は流さない）。
        assert_eq!(
            items[0].1.summary.as_deref(),
            Some("まず現状のテストを確かめる それから直す")
        );
        assert!(items[0].1.detail.is_none());
        // 1 つの assistant メッセージの本文は 1 件にまとまる。
        assert_eq!(
            items[1].1.summary.as_deref(),
            Some("まずテストを回します。 結果を見てから直します。")
        );
        assert_eq!(
            items[1].0,
            "まずテストを回します。\n結果を見てから直します。"
        );
        // Bash は入力のコマンドが 1 行要約、`detail` は入力そのもの。
        assert_eq!(items[2].1.tool.as_deref(), Some("Bash"));
        assert_eq!(
            items[2].1.summary.as_deref(),
            Some("cargo test --workspace")
        );
        assert!(
            items[2]
                .1
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("run the tests")
        );
        assert!(items[2].0.starts_with("tool_use: Bash"), "{}", items[2].0);
        // tool_result は先頭 200 文字の要約と失敗の印。
        assert_eq!(
            items[3].1.summary.as_deref(),
            Some("test result: ok. 812 passed; 0 failed")
        );
        assert!(!items[3].1.error);
        // Read はパス、Grep は模様。配列の `content` も読める。
        assert_eq!(
            items[4].1.summary.as_deref(),
            Some("/repo/crates/task-api/src/console.rs")
        );
        assert_eq!(
            items[5].1.summary.as_deref(),
            Some("//! Console の読み取り側")
        );
        assert_eq!(items[6].1.summary.as_deref(), Some("fn console"));
        // `is_error` は `error` に写る。
        assert!(items[7].1.error, "{:?}", items[7]);
        assert_eq!(items[7].1.summary.as_deref(), Some("No files found"));
        assert!(items[7].0.contains("(error)"));
        // 知らない道具は入力そのものの先頭（120 文字）。
        assert_eq!(items[8].1.tool.as_deref(), Some("WebFetch"));
        assert!(
            items[8]
                .1
                .summary
                .as_deref()
                .unwrap_or("")
                .contains("example.invalid")
        );
        // `result` は進行ではない（終端の合成に使う）。
        assert!(last_result.is_some());
    }

    #[tokio::test]
    async fn happy_path_progress_and_done_from_result_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"working on it"}]}}'
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}'
printf '%s' '{"summary":"added usage example","evidence":[]}' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":10,"output_tokens":20}}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-1", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done {
                summary,
                evidence,
                usage,
            } => {
                assert_eq!(summary, "added usage example");
                assert!(evidence.is_empty());
                assert_eq!(
                    usage,
                    Some(Usage {
                        input_tokens: Some(10),
                        output_tokens: Some(20),
                        cache_read_tokens: None,
                        cache_creation_tokens: None,
                        cost_usd: None,
                    })
                );
            }
            other => panic!("expected done, got {other:?}"),
        }
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m == "working on it"));
        assert!(progress.iter().any(|m| m.starts_with("tool_use: Bash")));
        assert!(dir.path().join("runs/run-1/stdout.jsonl").is_file());

        // P-26 (ADR-0010 D10): the terminal is also normalized into `runs/<run_id>/result.json`,
        // readable by task-dispatch as a `WorkerMessage::Done`.
        let result_json =
            std::fs::read_to_string(dir.path().join("runs/run-1/result.json")).unwrap();
        match serde_json::from_str::<crate::protocol::WorkerMessage>(result_json.trim()).unwrap() {
            crate::protocol::WorkerMessage::Done { summary, .. } => {
                assert_eq!(summary, "added usage example")
            }
            other => panic!("expected done in result.json, got {other:?}"),
        }
    }

    /// ADR-0056 D3（Phase 79）: `context.skills` に乗った skill は、run 開始時に
    /// `.claude/skills/<name>/SKILL.md`（＋付属ファイル）として作業場所に写る。
    #[tokio::test]
    async fn mounted_skills_are_copied_into_dot_claude_skills() {
        let dir = tempfile::tempdir().unwrap();
        let kb = tempfile::tempdir().unwrap();
        let skill_dir = kb.path().join("rust-review");
        std::fs::create_dir_all(skill_dir.join("refs")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: rust-review\ndescription: d\n---\n\nbody\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("refs/checklist.md"), "1. fmt\n").unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.skills = vec![crate::protocol::SkillMount {
            name: "rust-review".into(),
            path: skill_dir.display().to_string(),
            description: "d".into(),
        }];
        let sink = RecordingSink::default();
        adapter
            .run(req, "run-skills", default_limits(), &sink)
            .await
            .unwrap();
        let delivered = dir.path().join(".claude/skills/rust-review");
        assert!(
            std::fs::read_to_string(delivered.join("SKILL.md"))
                .unwrap()
                .contains("body")
        );
        assert_eq!(
            std::fs::read_to_string(delivered.join("refs/checklist.md")).unwrap(),
            "1. fmt\n"
        );
    }

    /// ADR-0036 D1/D2/D3: 共有 workspace のタスクは `.taskd/artifacts/<task_id>/result.json` を読み書きし、
    /// プロンプトにもその相対パスが出る。隣（兄弟）が共有 `artifacts/` に置いた結果ファイルは読まない。
    #[tokio::test]
    async fn a_shared_workspace_task_uses_its_own_artifacts_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p .taskd/artifacts/T1
printf '%s' '{"summary":"mine","evidence":[]}' > .taskd/artifacts/T1/result.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/result.json"),
            r#"{"summary":"sibling"}"#,
        )
        .unwrap();
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.artifacts_dir = dir.path().join(".taskd/artifacts/T1");
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-shared", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert_eq!(summary, "mine"),
            other => panic!("expected done, got {other:?}"),
        }
        let prompt =
            std::fs::read_to_string(dir.path().join("runs/run-shared/prompt.txt")).unwrap();
        assert!(
            prompt.contains(".taskd/artifacts/T1/result.json"),
            "{prompt}"
        );
        assert!(!prompt.contains("`artifacts/result.json`"), "{prompt}");
        // 兄弟のファイルは消していない（自分のディレクトリだけを掃除する）。
        assert_eq!(
            std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap(),
            r#"{"summary":"sibling"}"#
        );
    }

    /// ADR-0006 Phase 115 D1（本番障害 01M3915FARENW8M0JM11XVF6W0）: `work_dir != workspace`
    /// （部署のリポジトリの git worktree で走るタスク）だけ、プロンプト冒頭に cwd と成果物ディレクトリの
    /// 絶対パスの注意が 2 行出る（D4(a)）。`work_dir` が無い他の全テストの文面は変わらない。
    #[tokio::test]
    async fn work_dir_note_appears_in_the_prompt_when_work_dir_differs_from_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let work_dir = dir.path().join("repos/agent-platform");
        std::fs::create_dir_all(&work_dir).unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let mut req = sample_req(dir.path().to_path_buf());
        req.work_dir = Some(work_dir.clone());
        let sink = RecordingSink::default();
        let adapter = ClaudeCodeAdapter::new(config);
        let outcome = adapter
            .run(req, "run-wd-1", default_limits(), &sink)
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { .. }),
            "{:?}",
            outcome.terminal
        );
        let prompt = std::fs::read_to_string(dir.path().join("runs/run-wd-1/prompt.txt")).unwrap();
        assert!(
            prompt.contains(&format!("cwd は `{}`", work_dir.display())),
            "{prompt}"
        );
        assert!(
            prompt.contains(&format!(
                "成果物ディレクトリは `{}`",
                dir.path().join("artifacts").display()
            )),
            "{prompt}"
        );
        assert!(
            prompt.contains("相対 `artifacts/` はリポジトリの中を指すので使わない"),
            "{prompt}"
        );
    }

    /// ADR-0006 Phase 115 D2（本番障害 01M3915FARENW8M0JM11XVF6W0 / 01M38T8N17MEWPTJQXGX1TNYJD）:
    /// 偽 claude スタブが（指示を読み違えて）cwd 相対の `artifacts/result.json`（=
    /// `<work_dir>/artifacts/result.json`）に書いても、正しい置き場（`<artifacts_dir>/result.json`）へ
    /// 移して採用し `Done` になる。worktree 側には残らない（D4(b)）。
    #[tokio::test]
    async fn a_result_json_written_under_work_dir_is_adopted_and_not_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let work_dir = dir.path().join("repos/agent-platform");
        std::fs::create_dir_all(&work_dir).unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"wrote to the worktree by mistake","evidence":[]}' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let mut req = sample_req(dir.path().to_path_buf());
        req.work_dir = Some(work_dir.clone());
        let sink = RecordingSink::default();
        let adapter = ClaudeCodeAdapter::new(config);
        let outcome = adapter
            .run(req, "run-wd-2", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => {
                assert_eq!(summary, "wrote to the worktree by mistake")
            }
            other => panic!("expected done, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap(),
            r#"{"summary":"wrote to the worktree by mistake","evidence":[]}"#
        );
        assert!(
            !work_dir.join("artifacts").exists(),
            "the stray artifacts/ dir under work_dir should be gone"
        );
    }

    /// ADR-0072 E2（P-E0-2 の修正）: `result` メッセージは観測できたのに `result.json` が無いのは
    /// `Err(AdapterError::Other)`（`ProviderFailure` 無し）になる。従来は `Ok(Terminal::Error{retryable:
    /// true})` になり、`WorkerError{true}` として attempts を消費していた（ADR-0070 D3 の想定と
    /// 食い違っていた）。この `Err` はディスパッチャの `provider_failure_outcome` が分類できない
    /// 失敗として扱い、`InfraRequeue`（attempts を消費しない）に倒す。
    #[tokio::test]
    async fn success_without_result_file_is_an_infra_failure_not_a_retryable_worker_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"echo '{"type":"result","subtype":"success","is_error":false}'"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-2", default_limits(), &sink)
            .await
            .unwrap_err();
        match err {
            AdapterError::Other(message) => {
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => panic!("expected AdapterError::Other, got {other:?}"),
        }
        // `provider_failure_reason`/`provider_failure_outcome`（task-dispatch）はこれを分類できない
        // 失敗として扱う（`None`）。task-worker からは直接呼べないので、`AdapterError::Other` である
        // ことの確認をもって代える（`dispatcher.rs` 側の網羅テストが `None` → `InfraRequeue` を見る）。
    }

    #[tokio::test]
    async fn question_in_result_file_blocks_task() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"question":"which crate version?"}' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-3", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Question { text } => assert_eq!(text, "which crate version?"),
            other => panic!("expected question, got {other:?}"),
        }
    }

    /// ADR-0072 D7（Phase E1）: `error_max_turns` は `result.json` が書けていても
    /// `Terminal::BudgetExhausted{kind: Turns}` になる（result.json より優先。継続の対象で、
    /// `Error` ではない）。usage があれば運ぶ（従来は捨てていた）。
    #[tokio::test]
    async fn error_max_turns_subtype_wins_and_becomes_budget_exhausted_with_usage() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"claimed done","evidence":[]}' > artifacts/result.json
echo '{"type":"result","subtype":"error_max_turns","is_error":true,"usage":{"input_tokens":100,"output_tokens":50}}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-4", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::BudgetExhausted {
                kind,
                message,
                usage,
            } => {
                assert_eq!(kind, task_core::BudgetKind::Turns);
                assert!(message.contains("error_max_turns"), "{message}");
                let usage = usage.expect("usage carried through");
                assert_eq!(usage.input_tokens, Some(100));
                assert_eq!(usage.output_tokens, Some(50));
            }
            other => panic!("expected budget_exhausted, got {other:?}"),
        }
    }

    /// ADR-0072 D9（Phase E1）: `result.json` の `{"yield": {...}}` が `Terminal::Yielded` になる。
    #[tokio::test]
    async fn result_yield_becomes_terminal_yielded() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"yield":{"completed":["A"],"remaining":["B"],"next_action":"do B"}}' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":10,"output_tokens":20}}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-yield", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Yielded { checkpoint, usage } => {
                assert_eq!(checkpoint["next_action"], "do B");
                let usage = usage.expect("usage carried through");
                assert_eq!(usage.input_tokens, Some(10));
            }
            other => panic!("expected yielded, got {other:?}"),
        }
    }

    /// `result.is_error`（or `subtype != "success"`) のとき `result` テキストを分類する（ADR-0010 D5）。
    /// `Throttled` が当たれば `AdapterError::Throttled` として返り、result.json は書かれる。
    #[tokio::test]
    async fn result_text_classified_as_throttled_surfaces_as_adapter_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"echo '{"type":"result","subtype":"success","is_error":true,"result":"API Error: 429 rate limit exceeded"}'"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-4b", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::Throttled { .. }), "{err:?}");
        assert!(dir.path().join("runs/run-4b/result.json").is_file());
    }

    /// 同じく `AuthFailed` の分類（ADR-0010 D5）。
    #[tokio::test]
    async fn result_text_classified_as_auth_failed_surfaces_as_adapter_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"echo '{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login"}'"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-4c", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::AuthFailed(_)), "{err:?}");
        assert!(dir.path().join("runs/run-4c/result.json").is_file());
    }

    /// `result` メッセージを一度も観測できずに exit した場合も、stderr の末尾を分類する（ADR-0010 D5）。
    #[tokio::test]
    async fn crash_with_matching_stderr_is_classified_as_provider_failure() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), "echo 'fatal: 401 Unauthorized' 1>&2; exit 9");
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-4d", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::AuthFailed(_)), "{err:?}");
    }

    #[tokio::test]
    async fn invalid_result_file_json_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf 'not json' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-5", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("not valid JSON"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// ADR-0072 D7（Phase E1）: wall-clock の打ち切りは `Terminal::BudgetExhausted{kind: WallClock}`
    /// になる（continuation の対象。従来の `Error` ではない）。
    #[tokio::test]
    async fn wall_clock_exceeded_kills_and_reports_budget_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), "sleep 30");
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_millis(300),
            idle_timeout: Duration::from_secs(30),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-6", limits, &sink).await.unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        match outcome.terminal {
            Terminal::BudgetExhausted {
                kind,
                message,
                usage,
            } => {
                assert_eq!(kind, task_core::BudgetKind::WallClock);
                assert!(message.contains("wall clock exceeded"), "{message}");
                assert!(usage.is_none(), "wall-clock 打ち切りでは usage は取れない");
            }
            other => panic!("expected budget_exhausted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn idle_timeout_kills_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"echo '{"type":"assistant","message":{"content":[{"type":"text","text":"start"}]}}'
sleep 30
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_secs(30),
            idle_timeout: Duration::from_millis(300),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-7", limits, &sink).await.unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("idle timeout"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn crash_without_result_message_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), "exit 9");
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-8", default_limits(), &sink)
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, Some(9));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=9"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// クラッシュ前に（あるいは前回の run の名残として）`artifacts/result.json` が存在していても、
    /// `result` メッセージを一度も観測できなければ絶対に信用しない（監査で発見した不具合の回帰テスト。
    /// ADR-0006 D4）。
    #[tokio::test]
    async fn stale_result_file_without_result_message_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"looks done but crashed before saying so","evidence":[]}' > artifacts/result.json
exit 9
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-9", default_limits(), &sink)
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, Some(9));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=9"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// 前回の run が残した `artifacts/result.json` は、今回の run 開始時に消される
    /// （監査で発見した不具合の回帰テスト。ADR-0006 D3）。
    #[tokio::test]
    async fn stale_result_file_from_previous_run_is_cleared_before_this_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/result.json"),
            r#"{"summary":"stale from a previous attempt","evidence":[]}"#,
        )
        .unwrap();
        let config = stub_claude(
            dir.path(),
            r#"echo '{"type":"result","subtype":"success","is_error":false}'"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        // ADR-0072 E2（P-E0-2 の修正）: `result` は観測できたが `result.json` が無い（= 消された
        // stale file が再利用されていない証拠）ので、いまは `Err(AdapterError::Other)` になる
        // （InfraRequeue。attempts を消費しない）。
        let err = adapter
            .run(req, "run-10", default_limits(), &sink)
            .await
            .unwrap_err();
        match err {
            AdapterError::Other(message) => {
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => {
                panic!("expected error (stale file must be cleared, not reused), got {other:?}")
            }
        }
    }

    /// Phase 5 ドッグフードの回帰: `evidence` が文字列の配列など不正な形でも、`summary` があれば `done`
    /// として扱い、読めない要素は捨てる（ADR-0006 D3）。
    #[tokio::test]
    async fn malformed_evidence_in_result_file_does_not_fail_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"all good","evidence":["cargo test: 4 passed",{"criterion":0,"command":"cargo test","exit":0,"stdout_tail":""},42]}' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-11", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done {
                summary, evidence, ..
            } => {
                assert_eq!(summary, "all good");
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].command.as_deref(), Some("cargo test"));
            }
            other => panic!("expected done, got {other:?}"),
        }
        let prompt = build_prompt(
            &crate::protocol::tests::sample_task(),
            &RunContext::default(),
            "r",
            "artifacts",
        );
        assert!(prompt.contains("plain strings are not accepted"));
    }

    /// ADR-0033 D4 / D6（Phase 24）: 前置き（役職と brief・記憶・直近のやり取り）がプロンプトに入り、
    /// 並びは `## Task` の直後・`## Objective` の前。`RunContext` が Phase 23 までの中身なら出力は変わらない。
    #[test]
    fn build_prompt_puts_the_person_preamble_between_the_run_line_and_the_objective() {
        let task = crate::protocol::tests::sample_task();
        let bare = build_prompt(&task, &RunContext::default(), "run-p0", "artifacts");

        let context = RunContext {
            node: Some(crate::protocol::NodeContext {
                id: "research-survey".into(),
                name: "関連研究調査課".into(),
                brief: "関連研究を洗う。".into(),
            }),
            memory: Some(crate::protocol::MemoryContext {
                notes: "- 2026-09-10: pegasus は pjsub".into(),
                project: String::new(),
            }),
            conversation: vec![crate::protocol::ConversationTurn {
                role: task_core::MessageRole::User,
                text: "先週の続き".into(),
            }],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-p1", "artifacts");
        let at = |n: &str| {
            prompt
                .find(n)
                .unwrap_or_else(|| panic!("missing {n:?} in\n{prompt}"))
        };
        assert!(at("(run run-p1") < at("## あなた: 関連研究調査課 (research-survey)"));
        assert!(at("## あなた:") < at("## 覚えていること"));
        assert!(at("## 覚えていること") < at("## 直近のやり取り"));
        assert!(at("## 直近のやり取り") < at("## Objective"));
        assert!(prompt.contains("memory.notes"), "記憶の書き方の指示が付く");

        // Phase 23 までの `RunContext` では 1 バイトも変わらない。
        assert!(!bare.contains("## あなた"));
        assert!(!bare.contains("覚えておくこと"));
        assert_eq!(
            bare,
            build_prompt(&task, &RunContext::default(), "run-p0", "artifacts")
        );
    }

    /// ADR-0033 D4（Phase 24）: 組織図を渡した run には `## 組織図` と `assignee` の指示が入る。
    /// 渡していない run（Phase 23 までの構成）では出ない。
    #[test]
    fn build_prompt_includes_the_org_chart_and_the_assignee_instruction_only_when_present() {
        let mut task = crate::protocol::tests::sample_task();
        let org = vec![
            crate::protocol::OrgNodeContext {
                harnesses: Vec::new(),
                skills: Vec::new(),
                tools: Vec::new(),
                id: "research".into(),
                name: "研究部".into(),
                kind: task_core::OrgKind::Department,
                parent_id: Some("secretary".into()),
                brief: "課に振り分ける".into(),
                genre: None,
            },
            crate::protocol::OrgNodeContext {
                harnesses: Vec::new(),
                skills: Vec::new(),
                tools: Vec::new(),
                id: "research-survey".into(),
                name: "関連研究調査課".into(),
                kind: task_core::OrgKind::Section,
                parent_id: Some("research".into()),
                brief: "関連研究を洗う".into(),
                genre: Some("literature".into()),
            },
        ];
        let context = RunContext {
            organization: org,
            ..RunContext::default()
        };

        let execute = build_prompt(&task, &context, "run-o1", "artifacts");
        assert!(execute.contains("## 組織図 (who you can assign work to)"));
        assert!(execute.contains("- research-survey [課] 関連研究調査課 (親: research, 分野: literature) — 関連研究を洗う"));
        // ADR-0069 D1（Phase 114）: 委譲でも担当とモデルは選ばせない（書いても使われない）。
        assert!(execute.contains("**担当（`assignee`）と"), "{execute}");
        assert!(!execute.contains("\"assignee\":\"<optional org node id>\""));

        task.kind = task_core::TaskKind::Plan;
        let plan = build_prompt(&task, &context, "run-o2", "artifacts");
        // ADR-0046 D5（Phase 59）: 計画は人選をしない。組織図も渡さない。
        assert!(
            plan.contains("**担当（`assignee`）とモデル（`tier`）は選ぶな。**"),
            "{plan}"
        );
        assert!(
            plan.contains("`harness`: その仕事の実行契約の id"),
            "{plan}"
        );
        assert!(plan.contains("`mode`: `prototype`"), "{plan}");
        assert!(
            !plan.contains("## 組織図 (who you can assign work to)"),
            "{plan}"
        );

        assert!(
            !build_prompt(&task, &RunContext::default(), "run-o3", "artifacts").contains("組織図")
        );
    }

    /// ADR-0016 D1 / M3: `context.role` があれば `## Role: <id>` と指示文がプロンプトに入る。無ければ入らない。
    #[test]
    fn build_prompt_includes_role_header_when_present_and_omits_it_when_absent() {
        let task = crate::protocol::tests::sample_task();
        let context = RunContext {
            role: Some(crate::protocol::RoleContext {
                id: "lead".into(),
                instructions: "You coordinate the work of others.".into(),
            }),
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-role-1", "artifacts");
        assert!(prompt.contains("## Role: lead"));
        assert!(prompt.contains("You coordinate the work of others."));

        let no_role_prompt = build_prompt(&task, &RunContext::default(), "run-role-2", "artifacts");
        assert!(!no_role_prompt.contains("## Role"));
    }

    /// ADR-0027 D1: `task.genre` があり、その分野が `context.available_genres` に載っていれば
    /// `## Genre: <id>` と説明がプロンプトに入る。載っていなければ（委譲できない run など）出ない。
    #[test]
    fn build_prompt_includes_genre_header_only_when_the_genre_is_in_available_genres() {
        let mut task = crate::protocol::tests::sample_task();
        task.genre = Some("literature".into());
        let genre_spec = task_core::GenreSpec {
            id: "literature".into(),
            description: "related work survey and novelty checks".into(),
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-reader".into()],
            ..task_core::GenreSpec::default()
        };
        let context = RunContext {
            available_genres: vec![GenreContext::from(&genre_spec)],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-genre-1", "artifacts");
        assert!(prompt.contains("## Genre: literature"));
        assert!(prompt.contains("related work survey and novelty checks"));

        // available_genres が task.genre を含まない（あるいは空）なら Genre 見出しは出ない。
        let empty_prompt = build_prompt(&task, &RunContext::default(), "run-genre-2", "artifacts");
        assert!(!empty_prompt.contains("## Genre"));
    }

    /// ADR-0027 D1: `context.available_genres` が非空なら「使える専門家」節が Execute プロンプトに入り、
    /// 空なら入らない。
    #[test]
    fn build_prompt_includes_available_genres_section_only_when_present() {
        let task = crate::protocol::tests::sample_task();
        let genre_spec = task_core::GenreSpec {
            id: "literature".into(),
            description: "related work survey".into(),
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-scout".into(), "literature-reader".into()],
            ..task_core::GenreSpec::default()
        };
        let context = RunContext {
            available_genres: vec![GenreContext::from(&genre_spec)],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-avail-1", "artifacts");
        assert!(prompt.contains("使える専門家"));
        assert!(prompt.contains("literature-scout"));
        assert!(prompt.contains("literature-reader"));

        let no_genres_prompt =
            build_prompt(&task, &RunContext::default(), "run-avail-2", "artifacts");
        assert!(!no_genres_prompt.contains("使える専門家"));
    }

    /// ADR-0028 D2: `capabilities` / `input_artifacts` / `output_artifacts` があれば「できること」と
    /// 「渡すもの…返るもの」の行が、ADR に書かれた通りの形で出る。
    #[test]
    fn available_genres_section_renders_the_adr_0028_d2_shape() {
        let task = crate::protocol::tests::sample_task();
        let genre_spec = task_core::GenreSpec {
            id: "related-research".into(),
            description: "先行研究の確認・新規性の検討".into(),
            capabilities: vec![
                "学術文献の検索".into(),
                "引用グラフの探索".into(),
                "PDF 全文からの根拠抽出".into(),
            ],
            input_artifacts: vec!["question".into(), "pdf".into(), "bibliography".into()],
            output_artifacts: vec!["answer.md".into(), "citations.json".into()],
            default_role: Some("literature-reader".into()),
            roles: vec![
                "literature-scout".into(),
                "literature-reader".into(),
                "novelty-skeptic".into(),
            ],
        };
        let context = RunContext {
            available_genres: vec![GenreContext::from(&genre_spec)],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-avail-shape", "artifacts");
        assert!(
            prompt.contains(
                "- related-research: 先行研究の確認・新規性の検討\n\
                 \u{20}\u{20}できること: 学術文献の検索 / 引用グラフの探索 / PDF 全文からの根拠抽出\n\
                 \u{20}\u{20}渡すもの: question, pdf, bibliography → 返るもの: answer.md, citations.json\n\
                 \u{20}\u{20}役割: literature-scout, literature-reader, novelty-skeptic\n"
            ),
            "{prompt}"
        );
    }

    /// ADR-0028 D1: `capabilities` / `input_artifacts` / `output_artifacts` が空なら、それぞれの行を
    /// 出さない（既存設定との互換）。
    #[test]
    fn available_genres_section_omits_lines_whose_list_is_empty() {
        let task = crate::protocol::tests::sample_task();
        let genre_spec = task_core::GenreSpec {
            id: "coding".into(),
            description: "write and fix code".into(),
            roles: vec!["implementer".into()],
            ..task_core::GenreSpec::default()
        };
        let context = RunContext {
            available_genres: vec![GenreContext::from(&genre_spec)],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-avail-omit", "artifacts");
        assert!(!prompt.contains("できること"));
        assert!(!prompt.contains("渡すもの"));
        assert!(prompt.contains("- coding: write and fix code\n  役割: implementer\n"));
    }

    /// ADR-0028 D3: Plan run のプロンプトにも「使える専門家」節が入る（今までは Execute/Approval だけ）。
    #[test]
    fn build_plan_prompt_includes_available_genres_section_when_present() {
        let mut task = crate::protocol::tests::sample_task();
        task.kind = task_core::TaskKind::Plan;
        let genre_spec = task_core::GenreSpec {
            id: "literature".into(),
            description: "related work survey".into(),
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-reader".into()],
            ..task_core::GenreSpec::default()
        };
        let context = RunContext {
            available_genres: vec![GenreContext::from(&genre_spec)],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-plan-avail-1", "artifacts");
        assert!(prompt.contains("使える専門家"));
        assert!(prompt.contains("literature-reader"));
        assert!(prompt.contains("artifacts/plan.json"));

        let no_genres_prompt = build_prompt(
            &task,
            &RunContext::default(),
            "run-plan-avail-2",
            "artifacts",
        );
        assert!(!no_genres_prompt.contains("使える専門家"));
    }

    /// ADR-0072 D14（Phase E3）: `context.execution_planner` があれば、`task.kind` に関わらず
    /// planner 用プロンプトを選ぶ（compound と判定された Task は常に `kind == Execute` のまま）。
    /// 計画の schema・gate の根拠・上限・使える genre の一覧が載る。
    #[test]
    fn build_prompt_selects_the_execution_plan_prompt_when_execution_planner_is_present() {
        let task = crate::protocol::tests::sample_task();
        assert_eq!(task.kind, task_core::TaskKind::Execute);
        let genre_spec = task_core::GenreSpec {
            id: "coding".into(),
            description: "write and fix code".into(),
            roles: vec!["implementer".into()],
            ..task_core::GenreSpec::default()
        };
        let planner_ctx = crate::protocol::ExecutionPlannerContext {
            gate_rule_id: "compound/score".to_string(),
            gate_score: 6,
            gate_signals: vec!["F2: expected_length=high (+2)".to_string()],
            max_work_units: 5,
            work_unit_max_turns: 60,
            work_unit_max_wall_secs: 1800,
            default_max_turns: 30,
            default_max_wall_secs: 1800,
            replan: false,
            replan_reason: String::new(),
            current_plan_version: None,
            work_unit_summaries: Vec::new(),
            preserve_done_keys: Vec::new(),
        };
        let context = RunContext {
            available_genres: vec![GenreContext::from(&genre_spec)],
            execution_planner: Some(planner_ctx),
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-planner-1", "artifacts");

        // 通常の execute プロンプト（Objective の後の委譲節など）ではなく、計画作成の指示になる。
        assert!(prompt.contains("artifacts/execution-plan.json"));
        assert!(prompt.contains(task_core::EXECUTION_PLAN_SCHEMA));
        assert!(prompt.contains("\"work_units\""));
        // D18 の上限（テストの `ExecutionPlannerContext` の値）が文面に出る。
        assert!(prompt.contains("Plan at most 5 WorkUnits"));
        assert!(prompt.contains("capped at 60"));
        // gate の根拠。
        assert!(prompt.contains("compound/score"));
        assert!(prompt.contains("F2: expected_length=high (+2)"));
        // 使える genre の一覧（WU の harness に使える id）。
        assert!(prompt.contains("Available genres"));
        assert!(prompt.contains("coding: write and fix code"));
        // `assignee`/`tier`/`model` を書くなと明示している。
        assert!(prompt.contains("Do not write `assignee`, `tier`, or `model`"));
        // ADR-0072 D17（Phase E4b 項目1）: `replan = false`（初回 planning）には replan 節が出ない。
        assert!(!prompt.contains("REPLANNING an existing execution plan"));

        // `execution_planner` が無ければ従来どおりの execute プロンプト。
        let normal_prompt =
            build_prompt(&task, &RunContext::default(), "run-planner-2", "artifacts");
        assert!(!normal_prompt.contains("artifacts/execution-plan.json"));
    }

    /// ADR-0072 D17（Phase E4b 項目1）: replan のときは「今の計画（版・WU の状態・完了/失敗の要約）」
    /// 「起こした理由」「保持すべき done の WU の key」が文面に載り、v2 は done の WU を変えてはならない
    /// と明示する。初回 planning（`replan = false`）のプロンプトは、この節を出さない限り 1 バイトも
    /// 変わらない（上のテストで確認済み）。
    #[test]
    fn build_execution_plan_prompt_replan_includes_current_plan_and_reason() {
        let task = crate::protocol::tests::sample_task();
        let planner_ctx = crate::protocol::ExecutionPlannerContext {
            gate_rule_id: "compound/score".to_string(),
            gate_score: 6,
            gate_signals: Vec::new(),
            max_work_units: 8,
            work_unit_max_turns: 80,
            work_unit_max_wall_secs: 3600,
            default_max_turns: 30,
            default_max_wall_secs: 1800,
            replan: true,
            replan_reason: "work unit b failed: boom again".to_string(),
            current_plan_version: Some(1),
            work_unit_summaries: vec![
                "a (implement) status=done: implemented the core model".to_string(),
                "b (test) status=failed: work unit b failed: boom again".to_string(),
                "c (release) status=blocked (dependency_failed): waiting on b".to_string(),
            ],
            preserve_done_keys: vec!["a".to_string()],
        };
        let context = RunContext {
            execution_planner: Some(planner_ctx),
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-planner-2", "artifacts");

        assert!(prompt.contains("REPLANNING an existing execution plan"));
        assert!(prompt.contains("Current (superseded) plan version: v1."));
        assert!(prompt.contains("Why this replan was triggered: work unit b failed: boom again"));
        assert!(prompt.contains("a (implement) status=done: implemented the core model"));
        assert!(prompt.contains("b (test) status=failed: work unit b failed: boom again"));
        assert!(prompt.contains("c (release) status=blocked (dependency_failed): waiting on b"));
        assert!(prompt.contains("MUST appear unchanged"));
        assert!(prompt.contains("will be rejected: a."));
    }

    /// Phase 38（ADR-0028 追記）テスト用: ハーネス系の `literature`（`default_role` が `paperqa`）と、
    /// ハーネスでない `coding`（`claude-code`）。`output_artifacts` は `名前: 説明` の形を混ぜる。
    fn harness_genre_contexts() -> (GenreContext, GenreContext) {
        let roles = vec![
            task_core::RoleSpec {
                id: "literature-reader".into(),
                adapter: Some("paperqa".into()),
                ..task_core::RoleSpec::default()
            },
            task_core::RoleSpec {
                id: "implementer".into(),
                adapter: Some("claude-code".into()),
                ..task_core::RoleSpec::default()
            },
        ];
        let literature = task_core::GenreSpec {
            id: "literature".into(),
            description: "関連研究の調査".into(),
            output_artifacts: vec![
                "answer.md: 引用付きの答え".into(),
                "papers.json: 検索した論文の一覧（コーパス）".into(),
                "sources.json".into(),
            ],
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-reader".into()],
            ..task_core::GenreSpec::default()
        };
        let coding = task_core::GenreSpec {
            id: "coding".into(),
            description: "コードを書く".into(),
            output_artifacts: vec!["diff".into()],
            default_role: Some("implementer".into()),
            roles: vec!["implementer".into()],
            ..task_core::GenreSpec::default()
        };
        (
            GenreContext::from_spec(&literature, &roles),
            GenreContext::from_spec(&coding, &roles),
        )
    }

    /// Phase 38（ADR-0028 追記。実機のレビュー不合格から）: Plan run のプロンプトに、ハーネスで動く分野の
    /// 成果物の規約（固定の名前と `名前: 説明` の説明、`artifact_exists` にはこの名前だけ、内容は
    /// objective とレビュアー条件で）が出る。ハーネスでない分野は載らない。
    #[test]
    fn build_plan_prompt_states_the_artifact_convention_for_harness_genres() {
        let mut task = crate::protocol::tests::sample_task();
        task.kind = task_core::TaskKind::Plan;
        let (literature, coding) = harness_genre_contexts();
        let context = RunContext {
            available_genres: vec![literature, coding],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-plan-harness-1", "artifacts");
        assert!(
            prompt.contains(
                "## ハーネスで動く分野の成果物（名前は固定）\n\
                 - literature: この分野の担当は**ハーネス**で動く。成果物は次の名前で固定され、担当が別のファイルを書くことはできない。\n\
                 \u{20}\u{20}- `answer.md`: 引用付きの答え\n\
                 \u{20}\u{20}- `papers.json`: 検索した論文の一覧（コーパス）\n\
                 \u{20}\u{20}- `sources.json`\n"
            ),
            "{prompt}"
        );
        assert!(
            prompt.contains("受け入れ条件（`artifact_exists`）にはこの名前だけを使うこと。"),
            "{prompt}"
        );
        assert!(
            prompt.contains("レビュアー条件（`{\"type\":\"reviewer\"}`）で判定させること"),
            "{prompt}"
        );
        // ハーネスでない分野（coding）は規約の節に出ない（「使える専門家」節には出る）。
        assert!(
            !prompt.contains("- coding: この分野の担当は**ハーネス**で動く"),
            "{prompt}"
        );
        assert!(prompt.contains("- coding: コードを書く"), "{prompt}");
    }

    /// Phase 38: ハーネスでない分野しか無い設定（coding だけ、あるいは `harness` が無い旧プロトコルの
    /// ワーカー）では規約の節は**空**で、Plan / Execute プロンプトは Phase 37 までと 1 バイトも変わらない。
    #[test]
    fn the_artifact_convention_is_absent_without_a_harness_genre() {
        let mut task = crate::protocol::tests::sample_task();
        task.kind = task_core::TaskKind::Plan;
        let (literature, coding) = harness_genre_contexts();
        let coding_only = RunContext {
            available_genres: vec![coding],
            ..RunContext::default()
        };
        assert_eq!(harness_artifacts_section_for_plan(&coding_only), "");
        let prompt = build_prompt(&task, &coding_only, "run-plan-harness-2", "artifacts");
        // Phase 59（ADR-0046 D3）: 計画の JSON スキーマには `harness` の説明が入るので、"ハーネス" の
        // 文字だけでは判定できない。規約の節そのものが無いことを見る。
        assert!(
            !prompt.contains("## ハーネスで動く分野の成果物"),
            "{prompt}"
        );

        // `output_artifacts` を書いていないハーネス系の分野も、出す名前が無いので節は出ない。
        let bare = RunContext {
            available_genres: vec![GenreContext {
                output_artifacts: Vec::new(),
                ..literature
            }],
            ..RunContext::default()
        };
        assert_eq!(harness_artifacts_section_for_plan(&bare), "");

        // Execute プロンプト（委譲側）には元から出さない。
        let mut execute = crate::protocol::tests::sample_task();
        execute.kind = task_core::TaskKind::Execute;
        let (literature, _) = harness_genre_contexts();
        let context = RunContext {
            available_genres: vec![literature],
            ..RunContext::default()
        };
        let execute_prompt = build_prompt(&execute, &context, "run-exec-harness", "artifacts");
        assert!(
            !execute_prompt.contains("## ハーネスで動く分野の成果物"),
            "{execute_prompt}"
        );
    }

    /// Phase 43（ADR-0039 D3）: 案件が作業場所を決めている run では、前置きに「## 作業場所」が出て
    /// 「`ssh` で直接書くな」が入る。委譲できる run には「子は同じ作業場所を継ぐ」も足す。
    #[test]
    fn build_prompt_states_the_project_workspace_when_the_project_has_one() {
        let task = crate::protocol::tests::sample_task();
        let context = RunContext {
            workspace_note: Some(crate::preamble::workspace_note(
                &task_core::WorkspaceSpec::Remote {
                    cluster: "pegasus".into(),
                    path: std::path::PathBuf::from("/work/NBB/rmaeda/workspace/rust/benchfs"),
                    mode: None,
                },
            )),
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-ws-1", "artifacts");
        assert!(
            prompt.contains("## 作業場所 (where this project's code lives)"),
            "{prompt}"
        );
        assert!(
            prompt.contains("この案件のコードはクラスタ pegasus の `/work/NBB/rmaeda/workspace/rust/benchfs` にある。"),
            "{prompt}"
        );
        assert!(
            prompt.contains("`ssh` で直接書き込んではいけない"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Children you delegate inherit this project's workspace"),
            "{prompt}"
        );

        // Local の案件では「クラスタ」とは言わない。
        let local = RunContext {
            workspace_note: Some(crate::preamble::workspace_note(
                &task_core::WorkspaceSpec::Local {
                    path: std::path::PathBuf::from("/home/rmaeda/workspace/rust/pluvio-poc"),
                    mode: None,
                },
            )),
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &local, "run-ws-2", "artifacts");
        assert!(
            prompt.contains("この案件のコードは `/home/rmaeda/workspace/rust/pluvio-poc` にある。"),
            "{prompt}"
        );
    }

    /// Phase 43（ADR-0039 D3）: 計画 run には「子タスクの作業場所」と `plan.json` の `workspace` の
    /// 使いどころが出る。作業場所を決めていない案件のプロンプトは Phase 42 までと**バイト単位で同じ**。
    #[test]
    fn build_plan_prompt_states_the_workspace_children_inherit_only_when_the_project_has_one() {
        let mut task = crate::protocol::tests::sample_task();
        task.kind = task_core::TaskKind::Plan;
        let bare = build_prompt(&task, &RunContext::default(), "run-ws-3", "artifacts");
        // 節そのものは出ない（`plan.json` のスキーマには `workspace` の説明が元から載っている）。
        assert!(!bare.contains("## 子タスクの作業場所"), "{bare}");
        assert!(!bare.contains("## 作業場所"), "{bare}");

        let context = RunContext {
            workspace_note: Some(crate::preamble::workspace_note(
                &task_core::WorkspaceSpec::Local {
                    path: std::path::PathBuf::from("/home/rmaeda/workspace/rust/pluvio-poc"),
                    mode: None,
                },
            )),
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-ws-3", "artifacts");
        assert!(
            prompt.contains("## 子タスクの作業場所 (the workspace child tasks inherit)"),
            "{prompt}"
        );
        assert!(
            prompt.contains("分解した子タスクはこの作業場所をそのまま継ぐ"),
            "{prompt}"
        );
        assert!(
            prompt.contains("`{\"kind\":\"remote\",\"cluster\":\"...\",\"path\":\"...\"}`"),
            "{prompt}"
        );
        // 作業場所の 2 節（と、両方に共通する前置き）を取り除けば、Phase 42 までのプロンプトと
        // バイト単位で一致する（ADR-0067 D1: 前置きに常に「成果物の置き場所」の節が付くようになったので、
        // `bare` 側の前置きも同じだけ取り除いて比べる）。
        let preamble = crate::preamble::render(&context, "artifacts");
        assert!(!preamble.is_empty());
        let stripped = prompt
            .replace(&workspace_section_for_plan(&context), "")
            .replace(&preamble, "");
        let bare_preamble = crate::preamble::render(&RunContext::default(), "artifacts");
        let bare_stripped = bare.replace(&bare_preamble, "");
        assert_eq!(stripped.len(), bare_stripped.len());
        assert!(
            stripped == bare_stripped,
            "作業場所の節・共通の前置き以外は 1 バイトも変わらない"
        );
    }

    /// Phase 43: 案件が作業場所を決めていない run（既存のタスク）は、Execute プロンプトも従来どおり
    /// （ADR-0067 D1 で前置きに常に付く「成果物の置き場所」の節を除けば Phase 42 までと変わらない）。
    #[test]
    fn a_project_without_a_workspace_keeps_the_previous_prompt_byte_for_byte() {
        let task = crate::protocol::tests::sample_task();
        let before = build_prompt(&task, &RunContext::default(), "run-ws-4", "artifacts");
        assert!(!before.contains("## 作業場所"), "{before}");
        assert_eq!(delegate_workspace_instruction(&RunContext::default()), "");
        assert_eq!(
            crate::preamble::render(&RunContext::default(), "artifacts"),
            crate::preamble::deliverables_placement_note()
        );
    }

    /// Phase 38（ADR-0028 追記）: レビュアーのプロンプトにも同じ規約が出る（対象タスクの分野が
    /// ハーネス系のときだけ）。`papers.json` は答えではなく、内容は `answer.md` で判定させる。
    #[test]
    fn build_review_prompt_states_the_artifact_convention_only_for_harness_genres() {
        let mut task = crate::protocol::tests::sample_task();
        task.kind = task_core::TaskKind::Review;
        let (literature, coding) = harness_genre_contexts();
        let context = RunContext {
            subject_genre: Some(literature),
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-review-harness-1", "artifacts");
        assert!(
            prompt.contains("## この担当の成果物（名前は固定。判定はこの前提で行う）"),
            "{prompt}"
        );
        assert!(
            prompt.contains("  - `papers.json`: 検索した論文の一覧（コーパス）\n"),
            "{prompt}"
        );
        assert!(
            prompt.contains(
                "`papers.json` は検索したコーパスで\nあって答えではない。答えは `answer.md`"
            ),
            "{prompt}"
        );
        assert!(
            prompt.contains("ファイル名の不一致だけを理由に不合格にはせず"),
            "{prompt}"
        );

        // ハーネスでない分野・分野が渡っていないレビューでは何も出ない。
        let coding_context = RunContext {
            subject_genre: Some(coding),
            ..RunContext::default()
        };
        assert_eq!(harness_artifacts_section_for_review(&coding_context), "");
        let none = build_prompt(
            &task,
            &RunContext::default(),
            "run-review-harness-2",
            "artifacts",
        );
        assert!(!none.contains("この担当の成果物"), "{none}");
    }

    /// ADR-0016 D3 / M4: `context.children` が非空なら集約 run の節が入り、成果物のまとめ方の指示が付く。
    /// 無ければ節自体が出ない。
    #[test]
    fn build_prompt_includes_children_section_only_when_present() {
        let task = crate::protocol::tests::sample_task();
        let context = RunContext {
            children: vec![crate::protocol::ChildSummary {
                id: task_core::TaskId::new(),
                title: "implement parser".into(),
                role: Some("implementer".into()),
                status: task_core::Status::Done,
                branch: None,
                outcome: Some("done".into()),
                artifacts: vec![],
                workspace: Some(std::path::PathBuf::from("/tmp/child-ws")),
            }],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-agg-1", "artifacts");
        assert!(prompt.contains("## Delegated child tasks (this is the aggregate run)"));
        assert!(prompt.contains("implement parser"));
        assert!(prompt.contains("artifacts/summary.md"));

        let no_children_prompt =
            build_prompt(&task, &RunContext::default(), "run-agg-2", "artifacts");
        assert!(!no_children_prompt.contains("Delegated child tasks"));
        assert!(!no_children_prompt.contains("artifacts/summary.md"));
    }

    /// ADR-0016 M8: run の終わりに `artifacts/delegate.json` があれば、`sink.delegate` が 1 回呼ばれる。
    #[tokio::test]
    async fn delegate_json_written_by_worker_is_forwarded_to_sink() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"delegated two subtasks","evidence":[]}' > artifacts/result.json
printf '%s' '{"tasks":[{"title":"a","objective":"do a","acceptance":[{"text":"c","check":{"type":"human"}}]},{"title":"b","objective":"do b","acceptance":[{"text":"c","check":{"type":"human"}}]}]}' > artifacts/delegate.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-delegate-1", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let delegated = sink.delegated.lock().unwrap();
        assert_eq!(delegated.len(), 1);
        assert_eq!(delegated[0].len(), 2);
    }

    /// 壊れた `artifacts/delegate.json` は `progress` に警告を残すだけで run は失敗させない（ADR-0016 M8）。
    #[tokio::test]
    async fn malformed_delegate_json_is_ignored_and_run_still_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"done, but wrote bad delegate.json","evidence":[]}' > artifacts/result.json
printf 'not json' > artifacts/delegate.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-delegate-2", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { .. } => {}
            other => panic!("expected done, got {other:?}"),
        }
        assert!(sink.delegated.lock().unwrap().is_empty());
        let progress = sink.progress.lock().unwrap();
        assert!(
            progress.iter().any(|m| m.contains("delegate.json ignored")),
            "{progress:?}"
        );
    }

    /// ADR-0024 D4: `rate_limit_event` を解析すると `sink.rate_limit` に観測値が渡る。ADR に載っている
    /// 実測の行そのものを使う。
    #[tokio::test]
    async fn rate_limit_event_line_is_forwarded_to_the_sink() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
echo '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1789605600,"rateLimitType":"five_hour","overageStatus":"rejected","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.14,"resetsAt":1789605600},"seven_day":{"utilization":0.24,"resetsAt":1790031600}}}}'
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-rate-1", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let observed = sink.rate_limits.lock().unwrap();
        assert_eq!(observed.len(), 1);
        let obs = &observed[0];
        assert_eq!(obs.five_hour.map(|w| w.utilization), Some(0.14));
        assert_eq!(obs.seven_day.map(|w| w.utilization), Some(0.24));
        assert_eq!(obs.status.as_deref(), Some("allowed"));
    }

    /// ADR-0024 D2: `with_env` の追加分は、既存の同名キーより後に環境を組み立てるので勝つ。
    #[tokio::test]
    async fn with_env_overrides_a_same_name_key_already_in_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("env-seen.txt");
        let mut config = stub_claude(
            dir.path(),
            &format!(
                r#"mkdir -p artifacts
printf '%s' "$CLAUDE_SECURESTORAGE_CONFIG_DIR" > {out}
printf '%s' '{{"summary":"ok","evidence":[]}}' > artifacts/result.json
echo '{{"type":"result","subtype":"success","is_error":false}}'
"#,
                out = out_file.display()
            ),
        );
        config.env.push((
            "CLAUDE_SECURESTORAGE_CONFIG_DIR".to_string(),
            "old-account-dir".to_string(),
        ));
        let base = ClaudeCodeAdapter::new(config);
        let with_env = base
            .with_env(&[(
                "CLAUDE_SECURESTORAGE_CONFIG_DIR".to_string(),
                "new-account-dir".to_string(),
            )])
            .expect("claude-code supports with_env");

        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = with_env
            .run(req, "run-env-1", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let seen = std::fs::read_to_string(&out_file).unwrap();
        assert_eq!(seen, "new-account-dir");
    }

    /// ADR-0072 D14（Phase E4b 項目3）: `with_permission_mode` の複製は `--permission-mode` の値を
    /// 上書きする（`[execution.planner].permission_mode` を実際の CLI 引数に反映するための実行時配線。
    /// E3 実装時に見送っていたフック）。既定のアダプタ（`bypassPermissions`）の CLI 引数には出ない。
    #[tokio::test]
    async fn with_permission_mode_overrides_the_permission_mode_argument() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("args-seen.txt");
        let config = stub_claude(
            dir.path(),
            &format!(
                r#"mkdir -p artifacts
printf '%s' "$*" > {out}
printf '%s' '{{"summary":"ok","evidence":[]}}' > artifacts/result.json
echo '{{"type":"result","subtype":"success","is_error":false}}'
"#,
                out = out_file.display()
            ),
        );
        assert_eq!(config.permission_mode, "bypassPermissions");
        let base = ClaudeCodeAdapter::new(config);
        let planner_mode = base
            .with_permission_mode("plan")
            .expect("claude-code supports with_permission_mode");

        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = planner_mode
            .run(req, "run-permission-1", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let seen = std::fs::read_to_string(&out_file).unwrap();
        assert!(
            seen.contains("--permission-mode plan"),
            "expected the overridden permission mode in the CLI args: {seen}"
        );
        assert!(
            !seen.contains("bypassPermissions"),
            "the adapter's default permission mode must not leak through: {seen}"
        );
    }
    #[tokio::test]
    async fn tier_binding_reaches_cli_model_argument_and_preserves_account_env() {
        use task_core::{Tier, model_routing::ModelBinding};
        for tier in [Tier::Frontier, Tier::Standard, Tier::Cheap] {
            let dir = tempfile::tempdir().unwrap();
            let config = stub_claude(
                dir.path(),
                r#"
for a in "$@"; do printf '%s\0' "$a" >> args.log; done
printf '%s' "$ROUTING_ACCOUNT" > account.log
mkdir -p artifacts
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
printf '%s\n' '{"type":"turn.completed"}'
"#,
            );
            let expected = format!("explicit-{tier:?}");
            let adapter = crate::tiered::TieredAdapter {
                base: Arc::new(ClaudeCodeAdapter::new(config)),
                account_id: Some("account-a".into()),
                credential_error: None,
                models: [(
                    tier,
                    ModelBinding {
                        name: "requested-name".into(),
                        model_id: Some(expected.clone()),
                        unavailable_reason: None,
                        reasoning_effort: None,
                    },
                )]
                .into(),
            };
            let adapter = adapter
                .with_env(&[("ROUTING_ACCOUNT".into(), "account-a".into())])
                .unwrap();
            assert_eq!(adapter.account_id(), Some("account-a"));
            let mut req = sample_req(dir.path().to_path_buf());
            req.task.worker_hint.tier = tier;
            let _ = adapter
                .run(req, "tier-run", default_limits(), &RecordingSink::default())
                .await
                .unwrap();
            let args = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
            let args: Vec<_> = args.split('\0').collect();
            let model = args.windows(2).find(|pair| pair[0] == "--model").unwrap()[1];
            assert_eq!(model, expected);
            assert_eq!(
                std::fs::read_to_string(dir.path().join("account.log")).unwrap(),
                "account-a"
            );
        }
    }

    /// ADR-0069 Phase 118 D1: claude-code には effort 相当の CLI 引数・環境変数が無い
    /// （`claude --help` を実行して確認できる本物の CLI が無いこの環境では、この判断は運用側の
    /// 実測メモに基づく）。`WorkerAdapter` の既定（`supports_reasoning_effort() == false`、
    /// `with_reasoning_effort` は `None`）のままなので、設定に `reasoning_effort` を書いても argv には
    /// 一切現れない（`--model` は従来どおり渡る）。
    #[tokio::test]
    async fn tier_reasoning_effort_does_not_reach_claude_code_argv() {
        use task_core::{Tier, model_routing::ModelBinding};
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let base = ClaudeCodeAdapter::new(config);
        assert!(!base.supports_reasoning_effort());
        assert!(base.with_reasoning_effort("high").is_none());
        let adapter = crate::tiered::TieredAdapter {
            base: Arc::new(base),
            account_id: None,
            credential_error: None,
            models: [(
                Tier::Standard,
                ModelBinding {
                    name: "requested-name".into(),
                    model_id: Some("claude-sonnet-5".into()),
                    unavailable_reason: None,
                    reasoning_effort: Some("medium".into()),
                },
            )]
            .into(),
        };
        let mut req = sample_req(dir.path().to_path_buf());
        req.task.worker_hint.tier = Tier::Standard;
        let _ = adapter
            .run(
                req,
                "tier-run-1",
                default_limits(),
                &RecordingSink::default(),
            )
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(
            !args
                .iter()
                .any(|a| a.to_ascii_lowercase().contains("reasoning") || a == "medium"),
            "{args:?}"
        );
        let model = args.windows(2).find(|pair| pair[0] == "--model").unwrap()[1].clone();
        assert_eq!(model, "claude-sonnet-5");
    }

    fn args_log_script() -> &'static str {
        r#"
for a in "$@"; do printf '%s\0' "$a" >> args.log; done
mkdir -p artifacts
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
printf '%s\n' '{"type":"turn.completed"}'
"#
    }

    fn captured_args(dir: &std::path::Path) -> Vec<String> {
        let args = std::fs::read_to_string(dir.join("args.log")).unwrap();
        args.split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// ADR-0054 D1（Phase 67）: `context.session` が無ければ Phase 66 までと同じ
    /// `--no-session-persistence`。
    #[tokio::test]
    async fn without_a_session_the_cli_keeps_no_session_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let _ = adapter
            .run(req, "run-1", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(args.contains(&"--no-session-persistence".to_string()));
        assert!(!args.contains(&"--session-id".to_string()));
        assert!(!args.contains(&"--resume".to_string()));
    }

    /// ADR-0054 D1（Phase 67）: 継続セッションの**最初の run**（`resume: false`）は
    /// `--session-id <id>`（これから使う id を固定）で、`--no-session-persistence` は付かない。
    #[tokio::test]
    async fn a_fresh_session_passes_session_id_not_no_session_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: ClaudeCodeAdapter::ID.to_string(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            resume: false,
        });
        let _ = adapter
            .run(req, "run-1", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(!args.contains(&"--no-session-persistence".to_string()));
        let idx = args
            .iter()
            .position(|a| a == "--session-id")
            .expect("--session-id present");
        assert_eq!(args[idx + 1], "550e8400-e29b-41d4-a716-446655440000");
        assert!(!args.contains(&"--resume".to_string()));
    }

    /// ADR-0054 D1（Phase 67）: 継続セッションの**2 回目以降**（`resume: true`）は `--resume <id>`。
    #[tokio::test]
    async fn a_continuing_session_passes_resume() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: ClaudeCodeAdapter::ID.to_string(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-2", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(!args.contains(&"--no-session-persistence".to_string()));
        assert!(!args.contains(&"--session-id".to_string()));
        let idx = args
            .iter()
            .position(|a| a == "--resume")
            .expect("--resume present");
        assert_eq!(args[idx + 1], "550e8400-e29b-41d4-a716-446655440000");
    }

    /// `context.session` が別アダプタ向けなら無視する（渡り歩きは無い）。
    #[tokio::test]
    async fn a_session_for_another_adapter_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: "codex".to_string(),
            session_id: "codex-session".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-3", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(args.contains(&"--no-session-persistence".to_string()));
    }

    /// ADR-0054 D2（Phase 68）: CoS の対話 run（`conversation_addressee = Secretary`）だけ
    /// `--allowedTools` に読み取り専用の `celerisctl` サブコマンドが渡る。それ以外の run には
    /// 付かない（対話 run は道具を使わないという ADR-0033 D4 の原則のまま）。
    #[tokio::test]
    async fn the_cos_conversation_run_gets_a_readonly_tool_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.conversation_addressee =
            Some(crate::protocol::ConversationAddressee::Secretary);
        let _ = adapter
            .run(req, "run-cos", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        let idx = args
            .iter()
            .position(|a| a == "--allowedTools")
            .expect("--allowedTools present");
        let allowed = &args[idx + 1];
        assert!(
            allowed.contains("Bash(celerisctl knowledge search:*)"),
            "{allowed}"
        );
        assert!(
            allowed.contains("Bash(celerisctl knowledge get:*)"),
            "{allowed}"
        );
        assert!(allowed.contains("Bash(celerisctl ls:*)"), "{allowed}");
        assert!(allowed.contains("Bash(celerisctl show:*)"), "{allowed}");
        assert!(
            allowed.contains("Bash(celerisctl projects ls:*)"),
            "{allowed}"
        );
        assert!(
            allowed.contains("Bash(celerisctl projects show:*)"),
            "{allowed}"
        );
    }

    /// 対話でない run・CoS 以外の対話（`Other`）には `--allowedTools` は付かない。
    #[tokio::test]
    async fn non_cos_runs_get_no_tool_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        assert_eq!(req.context.conversation_addressee, None);
        let _ = adapter
            .run(
                req,
                "run-plain",
                default_limits(),
                &RecordingSink::default(),
            )
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(!args.contains(&"--allowedTools".to_string()));

        let dir2 = tempfile::tempdir().unwrap();
        let config2 = stub_claude(dir2.path(), args_log_script());
        let adapter2 = ClaudeCodeAdapter::new(config2);
        let mut req2 = sample_req(dir2.path().to_path_buf());
        req2.context.conversation_addressee = Some(crate::protocol::ConversationAddressee::Other);
        let _ = adapter2
            .run(
                req2,
                "run-other",
                default_limits(),
                &RecordingSink::default(),
            )
            .await
            .unwrap();
        let args2 = captured_args(dir2.path());
        assert!(!args2.contains(&"--allowedTools".to_string()));
    }

    /// `runs/<run_id>/request.json` に `context.session` がそのまま残る（実装依頼の受け入れ条件:
    /// resume の有無が `request.json` から読み取れること）。
    #[tokio::test]
    async fn request_json_records_the_session_handle() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: ClaudeCodeAdapter::ID.to_string(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-4", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let request_json =
            std::fs::read_to_string(dir.path().join("runs").join("run-4").join("request.json"))
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&request_json).unwrap();
        assert_eq!(value["context"]["session"]["resume"], true);
        assert_eq!(
            value["context"]["session"]["session_id"],
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(value["context"]["session"]["adapter"], "claude-code");
    }

    /// ADR-0054 D1（Phase 67）: `--session-id` で spawn できたら、resume していなくても
    /// `session_established` を報告する（次の run から確実に resume できるよう、celeris 自身が
    /// 選んだ id をそのまま確認させる）。
    #[tokio::test]
    async fn a_fresh_session_reports_session_established() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: ClaudeCodeAdapter::ID.to_string(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            resume: false,
        });
        let sink = RecordingSink::default();
        let _ = adapter
            .run(req, "run-5", default_limits(), &sink)
            .await
            .unwrap();
        assert_eq!(
            sink.session_established.lock().unwrap().as_slice(),
            ["550e8400-e29b-41d4-a716-446655440000".to_string()]
        );
        assert!(sink.session_resume_failed.lock().unwrap().is_empty());
    }

    /// ADR-0054 D1（Phase 67）: `--resume` を頼んだ run が、既知の「セッションが見つからない」文言を
    /// 含む stderr で crash したら `session_resume_failed` を報告する（実機での文言は未確認。
    /// `provider::looks_like_resume_rejection` のコメント参照）。
    #[tokio::test]
    async fn a_rejected_resume_reports_session_resume_failed() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            "echo 'Error: No conversation found for session 01ARZ3' 1>&2; exit 1",
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: ClaudeCodeAdapter::ID.to_string(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            resume: true,
        });
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-6", default_limits(), &sink).await;
        // 供給側失敗としては分類されない文面なので run 自体は retryable な通常のエラーで返る。
        match outcome {
            Ok(o) => assert!(matches!(
                o.terminal,
                Terminal::Error {
                    retryable: true,
                    ..
                }
            )),
            Err(e) => panic!("expected Ok(Terminal::Error), got {e:?}"),
        }
        let failed = sink.session_resume_failed.lock().unwrap();
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].contains("No conversation found"), "{failed:?}");
    }

    /// resume していない run が同じ文言で crash しても `session_resume_failed` は報告しない
    /// （resume を頼んでいない run には関係が無い判断のため）。
    #[tokio::test]
    async fn a_crash_without_resuming_does_not_report_session_resume_failed() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            "echo 'Error: No conversation found for session 01ARZ3' 1>&2; exit 1",
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let _ = adapter.run(req, "run-7", default_limits(), &sink).await;
        assert!(sink.session_resume_failed.lock().unwrap().is_empty());
    }

    /// Phase 113 D1/D4(a)（ADR-0054 追記。本番のタスク 01M35X86XTK84F97QW0CN5PGMR / reviewer run
    /// 01M388BENASH3JEBWFS03KEQYT の再現）: `result` メッセージを一度観測できた run（＝上の
    /// `a_rejected_resume_reports_session_resume_failed` が使う「result を一度も観測できずクラッシュ」
    /// 経路ではない）でも、`subtype: error_during_execution` かつ `is_error` で、stderr の末尾が
    /// resume 拒否の文言なら `session_resume_failed` を報告する。Phase 67 時点はこの経路をまったく
    /// チェックしておらず、本番ではこの形（`error_during_execution` の `result` が出た上で stderr に
    /// 理由が 1 行だけ）で self-heal が働かなかった。
    #[tokio::test]
    async fn phase_113_a_rejected_resume_with_an_observed_result_message_reports_session_resume_failed()
     {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            "echo '{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true}'; \
             echo 'No conversation found with session ID: 01a0d017-e32a-4cad-b10c-0cb63869ae13' 1>&2; \
             exit 1",
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: ClaudeCodeAdapter::ID.to_string(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            resume: true,
        });
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-113a", default_limits(), &sink).await;
        match outcome {
            Ok(o) => assert!(matches!(
                o.terminal,
                Terminal::Error {
                    retryable: true,
                    ..
                }
            )),
            Err(e) => panic!("expected Ok(Terminal::Error), got {e:?}"),
        }
        let failed = sink.session_resume_failed.lock().unwrap();
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].contains("No conversation found"), "{failed:?}");
    }

    /// Phase 113: 同じ `error_during_execution`/`is_error` でも resume を頼んでいない run では
    /// `session_resume_failed` を報告しない（resume していない run には関係の無い判断のため）。
    #[tokio::test]
    async fn phase_113_an_error_during_execution_without_resuming_does_not_report_session_resume_failed()
     {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            "echo '{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true}'; \
             echo 'No conversation found with session ID: 01a0d017-e32a-4cad-b10c-0cb63869ae13' 1>&2; \
             exit 1",
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let _ = adapter.run(req, "run-113b", default_limits(), &sink).await;
        assert!(sink.session_resume_failed.lock().unwrap().is_empty());
    }

    /// Phase 113: `error_during_execution`/`is_error` の resume run でも、stderr が resume 拒否の
    /// 文言を含まなければ `session_resume_failed` は報告しない（他のインフラ都合の失敗まで
    /// resume 拒否として誤検出しない）。
    #[tokio::test]
    async fn phase_113_an_error_during_execution_without_resume_wording_does_not_report_session_resume_failed()
     {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            "echo '{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true}'; \
             echo 'internal error: unexpected panic' 1>&2; \
             exit 1",
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: ClaudeCodeAdapter::ID.to_string(),
            session_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            resume: true,
        });
        let sink = RecordingSink::default();
        let _ = adapter.run(req, "run-113c", default_limits(), &sink).await;
        assert!(sink.session_resume_failed.lock().unwrap().is_empty());
    }

    /// ADR-0054 Phase 67b 追記: `--resume` に渡す id が UUID でなければ、spawn する**前**に拒否する
    /// （本番で ULID を渡してすべての CoS 対話・部門長レビュー run が失敗した事故の再発防止。
    /// `resolve_node_session` 側の自己修復（Phase 67b）を将来の回帰が回避しても、ここでテストが
    /// 静かに ULID を通さず落ちるようにする）。
    #[tokio::test]
    async fn a_non_uuid_resume_id_is_refused_without_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: ClaudeCodeAdapter::ID.to_string(),
            session_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            resume: true,
        });
        let err = adapter
            .run(req, "run-8", default_limits(), &RecordingSink::default())
            .await
            .expect_err("a non-UUID resume id must be refused, not spawned");
        assert!(format!("{err}").contains("not a valid UUID"), "{err}");
        assert!(
            !dir.path().join("args.log").exists(),
            "the claude stub must not have been spawned"
        );
    }

    /// 同じ拒否を、初回の `--session-id`（`resume: false`）でも見る。
    #[tokio::test]
    async fn a_non_uuid_fresh_session_id_is_refused_without_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), args_log_script());
        let adapter = ClaudeCodeAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: ClaudeCodeAdapter::ID.to_string(),
            session_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            resume: false,
        });
        let err = adapter
            .run(req, "run-9", default_limits(), &RecordingSink::default())
            .await
            .expect_err("a non-UUID session id must be refused, not spawned");
        assert!(format!("{err}").contains("not a valid UUID"), "{err}");
        assert!(
            !dir.path().join("args.log").exists(),
            "the claude stub must not have been spawned"
        );
    }
}
