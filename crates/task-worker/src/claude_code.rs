//! `claude-code` アダプタ（DESIGN §5.4, ADR-0003 D7, ADR-0006）。
//!
//! `claude` CLI は taskd 独自のワーカープロトコルを話さない。`--output-format stream-json` が吐く
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
use crate::protocol::{Answer, Evidence, ProviderFailure, RunContext, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, kill_now, reap_after_terminal, read_line_limited, read_tail, write_result_json,
};

/// `[adapters.claude_code]`（taskd.toml, ADR-0006 D6）。
#[derive(Debug, Clone)]
pub struct ClaudeCodeConfig {
    /// 起動するコマンド名／パス。既定 `"claude"`。
    pub command: String,
    /// 末尾に追加する引数。
    pub extra_args: Vec<String>,
    /// `--permission-mode`。既定 `"bypassPermissions"`（ADR-0006 D6: taskd は許可プロンプトに応答できない）。
    pub permission_mode: String,
    /// `--model`（省略時は claude の既定モデル）。
    pub model: Option<String>,
    /// 追加の環境変数（例: `CLAUDE_CONFIG_DIR`）。
    pub env: Vec<(String, String)>,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            command: "claude".to_string(),
            extra_args: Vec::new(),
            permission_mode: "bypassPermissions".to_string(),
            model: None,
            env: Vec::new(),
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
    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.env.extend(extra.iter().cloned());
        Some(Arc::new(ClaudeCodeAdapter::new(config)))
    }
}

/// タスクからワーカーへのプロンプトを組み立てる（ADR-0006 D2, ADR-0007 D7, 純粋関数）。`run_id` は
/// スキーマ変更を避けてプロンプト文面にのみ埋め込む（旧 P-11。ADR-0006 D2 参照）。`task.kind` で分岐する
/// （`Plan` はプランナー用、`Review` はレビュアー用、それ以外は Phase 4 のワーカー用プロンプト。ADR-0007 D7）。
pub fn build_prompt(task: &Task, context: &RunContext, run_id: &str) -> String {
    match task.kind {
        TaskKind::Plan => build_plan_prompt(task, context, run_id),
        TaskKind::Review => build_review_prompt(task, context, run_id),
        TaskKind::Execute | TaskKind::Approval => build_execute_prompt(task, context, run_id),
    }
}

/// 冒頭の共通部分（タイトル・run_id/attempt・前置き・分野・目的）。前置き（役職と brief・永続の認可・記憶・
/// 直近のやり取り・役割の指示文）は `crate::preamble::render` が組む（ADR-0016 D1 / M3, ADR-0033 D4 / D6）。
/// `task.genre` があり、その分野が `context.available_genres` に載っていれば、続けて `## Genre: <id>` と
/// 説明を出す（ADR-0027 D1）。
fn prompt_header(task: &Task, context: &RunContext, run_id: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Task: {}\n\n", task.title));
    out.push_str(&format!(
        "(run {run_id}, attempt {} of {})\n\n",
        task.attempts + 1,
        task.budget.max_retries + 1
    ));
    // ADR-0033 D4 / D6（Phase 24）: 役職と brief → 永続の認可 → 記憶 → 直近のやり取り → 役割の指示文。
    // 前置きは `crate::preamble` が 1 か所で組む（`RunContext` が空なら 1 バイトも増えない）。
    out.push_str(&crate::preamble::render(context));
    if let Some(genre_id) = &task.genre
        && let Some(genre) = context.available_genres.iter().find(|g| &g.id == genre_id)
    {
        out.push_str(&format!("## Genre: {}\n{}\n\n", genre.id, genre.description));
    }
    out.push_str(&format!("## Objective\n{}\n\n", task.objective));
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
            g.roles.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join(", ")
        };
        out.push_str(&format!("  役割: {roles}\n"));
    }
    out
}

/// `context.available_genres` があれば「使える専門家」節を足す（ADR-0027 D1, ADR-0028 D2）。委譲できる run
/// （`build_execute_prompt`）にだけ、この run が子に割り当てられる分野の能力・入出力・役割の選択肢を伝える。
fn available_genres_section(context: &RunContext) -> String {
    let mut out = String::new();
    if context.available_genres.is_empty() {
        return out;
    }
    out.push_str("## 使える専門家 (available genres and roles you can delegate to)\n");
    out.push_str(&genre_list_lines(context));
    out.push_str(
        "\nIf part of this work belongs to a different genre, delegate it with `role` set to one \
         of that genre's roles and `genre` set to its id in `artifacts/delegate.json`.\n\n",
    );
    out
}

/// Plan run 用の「使える専門家」節（ADR-0028 D3）。子タスクの `genre` / `role` を `artifacts/plan.json` で
/// 選べることを伝える点だけが `available_genres_section` と異なる（委譲ではなく分解なので）。
fn available_genres_section_for_plan(context: &RunContext) -> String {
    let mut out = String::new();
    if context.available_genres.is_empty() {
        return out;
    }
    out.push_str("## 使える専門家 (available genres and roles you can assign child tasks to)\n");
    out.push_str(&genre_list_lines(context));
    out.push_str(
        "\nIf a child task belongs to a different genre than this one, set its `genre` (and, one of \
         that genre's roles, its `role`) in `artifacts/plan.json`.\n\n",
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
        out.push_str(&format!("- {} [{kind}] {} (親: {parent}, 分野: {genre})", n.id, n.name));
        if !n.brief.is_empty() {
            out.push_str(&format!(" — {}", n.brief));
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

/// `context.children` があれば「集約 run」節を足す（ADR-0016 D3 / M4）。
fn children_section(context: &RunContext) -> String {
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
            c.artifacts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(",")
        };
        out.push_str(&format!(
            "- {} [{role}] status={status} outcome={outcome} workspace={workspace} artifacts={artifacts}\n",
            c.title
        ));
    }
    out.push_str(
        "\nSummarize the results of the delegated child tasks in `artifacts/summary.md`. \
         The reviewer will check that `artifacts/summary.md` exists.\n\n",
    );
    out
}


/// ADR-0033 D4（Phase 24）: 「どの課に何を振るか」を `assignee` で指定させる指示（Plan run 用）。
/// 組織図を渡していない run（Phase 23 までの構成）では何も出さない。
fn assignee_instructions_for_plan(context: &RunContext) -> String {
    if context.organization.is_empty() {
        return String::new();
    }
    "上の組織図を見て、**子タスクごとに `assignee` を必ず書け**（その仕事を任せる課の id）。\n\
     `role` は必要なときだけ書けばよい（書かなければその課の分野の既定の役割で走る）。\n\
     `role` を書いた場合は、そちらの tier / アダプタ / 予算が使われ、`assignee` は「誰の仕事か」だけを表す。\n\n"
        .to_string()
}

/// 同じことを委譲（`artifacts/delegate.json`）側にも書く（ADR-0033 D4）。
/// 部をまたぐ委譲は秘書の認可が要るので、それも伝える（SPEC §3.1）。
fn assignee_instructions_for_delegation(context: &RunContext) -> String {
    if context.organization.is_empty() {
        return String::new();
    }
    "委譲する子には、上の組織図を見て `assignee`（任せる課の id）を書け。`role` は必要なときだけでよい。\n\
     自分と**別の部**の課へ委譲したいときは、子は作られず、代わりに秘書への質問になる（部をまたぐ連携は \
     秘書が認める）。\n\n"
        .to_string()
}

/// `context.prior_review` があれば「前回の判定」として列挙する（ADR-0006 D2）。
fn prior_review_section(context: &RunContext) -> String {
    let mut out = String::new();
    if !context.prior_review.is_empty() {
        out.push_str("## Previous attempt's review result (this is a retry)\n");
        for pr in &context.prior_review {
            let verdict = if pr.pass { "pass" } else { "fail" };
            out.push_str(&format!("- criterion {}: {verdict} ({})\n", pr.criterion, pr.reason));
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

/// `artifacts/result.json` の書式指示（ADR-0006 D3。全 kind 共通）。
fn result_json_instructions() -> &'static str {
    "Always write `artifacts/result.json` (create the `artifacts/` directory if it does not exist \
     yet) as a single JSON object of the form `{\"summary\": \"<what you did>\", \"evidence\": []}`. \
     `evidence` may be left empty; if you fill it, each element must be an object of the form \
     `{\"criterion\": <index>, \"command\": \"<what you ran>\", \"exit\": <code>, \"stdout_tail\": \"...\"}` \
     (plain strings are not accepted; `command`, `exit` and `stdout_tail` may be omitted for a criterion that \
     did not involve running a command). \
     If you cannot proceed and need a decision from a human, instead write \
     `{\"question\": \"<your question>\"}` to `artifacts/result.json` and stop there. This is a \
     non-interactive run: you cannot ask a question any other way, and no one will read your final \
     chat message directly.\n"
}

/// 実行中の委譲の方法（ADR-0016 D2 / M8, ADR-0027 D1）。
fn delegation_instructions() -> &'static str {
    "If you want to delegate part of this work to another agent, write `artifacts/delegate.json` \
     (create the `artifacts/` directory if it does not exist yet) as a single JSON object of the form \
     `{\"tasks\":[{\"title\":\"...\",\"objective\":\"...\",\"acceptance\":[{\"text\":\"...\",\
     \"check\":{\"type\":\"command\",\"cmd\":\"...\",\"expect_exit\":0}}],\"role\":\"<optional>\",\
     \"genre\":\"<optional>\",\"assignee\":\"<optional org node id>\",\
     \"depends_on\":[<index into this array, or an existing task id>]}]}`. \
     `check` may also be \
     `{\"type\":\"artifact_exists\",\"name\":\"...\"}`, `{\"type\":\"reviewer\"}`, or `{\"type\":\"human\"}`. \
     taskd will validate this after this run ends and insert whatever proposals pass validation as child \
     tasks (how many are accepted per run is limited by configuration; any rejected proposal has its \
     reason recorded as an event you cannot see, but a human can). A task cannot list its own parent or \
     itself in `depends_on`. This task will not be considered done until any children you delegated have \
     finished.\n"
}

/// `Execute`（および `Approval`）用プロンプト（ADR-0006 D2。既存のワーカー用プロンプトのまま）。
fn build_execute_prompt(task: &Task, context: &RunContext, run_id: &str) -> String {
    let mut out = prompt_header(task, context, run_id);
    out.push_str("## Acceptance criteria\n");
    for (i, c) in task.acceptance.iter().enumerate() {
        let detail = match &c.check {
            Check::Command { cmd, expect_exit } => format!(
                " (a reviewer will independently re-run `{cmd}` in this directory afterwards and \
                 requires exit code {expect_exit}; your own claim of success is not trusted)"
            ),
            Check::ArtifactExists { name } => {
                format!(" (a reviewer will check that the file `artifacts/{name}` exists)")
            }
            Check::Reviewer | Check::Human => String::new(),
        };
        out.push_str(&format!("{}. {}{}\n", i, c.text, detail));
    }
    out.push('\n');
    out.push_str(&prior_review_section(context));
    out.push_str(&answers_section(context));
    out.push_str(&children_section(context));
    out.push_str(&organization_section(context));
    out.push_str(&available_genres_section(context));
    out.push_str(&assignee_instructions_for_delegation(context));
    out.push_str("## Instructions\n");
    out.push_str("Work in the current directory (it is a dedicated workspace for this task). ");
    out.push_str(delegation_instructions());
    out.push_str("When you are done:\n");
    out.push_str(result_json_instructions());
    out
}

/// `Plan` kind 用プロンプト（DESIGN §5.6, ADR-0007 D7）。目標を独立に検証可能な受け入れ条件を持つ
/// 子タスク群に分解させ、`artifacts/plan.json` に `PlanOutput` を書かせる。
fn build_plan_prompt(task: &Task, context: &RunContext, run_id: &str) -> String {
    let mut out = prompt_header(task, context, run_id);
    out.push_str(
        "## Instructions\n\
         Decompose this goal into a set of child tasks, each with an independently verifiable \
         acceptance criterion or criteria (DESIGN §5.6: \"目標を、独立に検証可能な受け入れ条件を持つ \
         子タスク群に分解せよ\"). Prefer 3 to 6 child tasks when the size of the goal makes that \
         reasonable (DESIGN §6 Phase 5 acceptance criteria); use fewer or more only if the goal \
         clearly requires it.\n\n",
    );
    out.push_str(&format!(
        "Write your decomposition to `artifacts/plan.json` (create the `artifacts/` directory if it \
         does not exist yet) as a single JSON object of exactly this shape:\n\
         ```json\n\
         {{\"tasks\":[{{\"title\":\"...\",\"objective\":\"...\",\
         \"acceptance\":[{{\"text\":\"...\",\"check\":{{\"type\":\"command\",\"cmd\":\"...\",\
         \"expect_exit\":0}}}}],\"depends_on\":[<index into this same tasks array>],\
         \"kind\":\"execute\"|\"plan\" (omit for \"execute\"),\
         \"assignee\":\"<org node id>\" (optional), \
         \"tier\":\"frontier\"|\"standard\"|\"cheap\" (optional)}}]}}\n\
         ```\n\
         `check` may also be `{{\"type\":\"artifact_exists\",\"name\":\"...\"}}`, \
         `{{\"type\":\"reviewer\"}}`, or `{{\"type\":\"human\"}}`. Unknown fields are rejected, so do not \
         add any field not shown above. `depends_on` indices refer to positions within this same \
         `tasks` array and must form a DAG (no self-reference, no cycles). Every child task must have \
         at least one `acceptance` entry. `kind:\"plan\"` children are only allowed while the total \
         decomposition depth stays within {} (DESIGN §5.6 \"分解の深さは上限 3\"; this plan itself \
         already counts toward that limit). Any `command` check will later be re-run for real inside \
         the child task's own working directory by an independent reviewer, so do not fabricate a \
         command whose result you have not actually observed.\n\n",
        task_core::plan::MAX_PLAN_DEPTH
    ));
    let schema = serde_json::to_string(&task_core::plan::schema_value())
        .unwrap_or_else(|_| "{}".to_string());
    out.push_str("### Schema for the `artifacts/plan.json` object\n```json\n");
    out.push_str(&schema);
    out.push_str("\n```\n\n");
    out.push_str(&prior_review_section(context));
    out.push_str(&answers_section(context));
    out.push_str(&organization_section(context));
    out.push_str(&assignee_instructions_for_plan(context));
    out.push_str(&available_genres_section_for_plan(context));
    out.push_str(result_json_instructions());
    out
}

/// `Review` kind 用プロンプト（DESIGN §5.7, ADR-0007 D5/D7）。対象タスクの成果物を読み取り専用で
/// 検証し `artifacts/review.json` に判定を書かせる。
fn build_review_prompt(task: &Task, context: &RunContext, run_id: &str) -> String {
    let mut out = prompt_header(task, context, run_id);
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
                let command = e.command.as_deref().map(|c| format!(" command `{c}`")).unwrap_or_default();
                let exit = e.exit.map(|x| format!(" exit={x}")).unwrap_or_default();
                let tail = e.stdout_tail.as_deref().map(|t| format!(" stdout_tail={t:?}")).unwrap_or_default();
                out.push_str(&format!("- criterion {}:{command}{exit}{tail}\n", e.criterion));
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
        out.push_str(&format!("- {} at `{}` (sha256={})\n", a.name, a.path, a.sha256));
    }
    out.push('\n');
    out.push_str(
        "## Result\n\
         Write your verdicts to `artifacts/review.json` (create the `artifacts/` directory if it does \
         not exist yet) as a single JSON object of exactly this shape: \
         `{\"verdicts\":[{\"criterion\":<index>,\"pass\":<bool>,\"reason\":\"...\"}]}`. You must write \
         exactly one verdict for each criterion listed under \"Criteria you must judge in this run\" \
         above.\n\n",
    );
    out.push_str(result_json_instructions());
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
    let result_path = req.workspace.join("artifacts").join("result.json");
    let _ = tokio::fs::remove_file(&result_path).await;
    clear_delegate_file(&req.workspace).await;

    let prompt = build_prompt(&req.task, &req.context, run_id);
    // ADR-0023 D2 / M1: この run で何を渡したかを残す（`request.json` は構造、`prompt.txt` は実際の文面）。
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &prompt, run_id).await;

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
        .arg(req.task.budget.max_turns.to_string())
        .arg("--no-session-persistence");
    if let Some(model) = &config.model {
        command.arg("--model").arg(model);
    }
    command.args(&config.extra_args);
    command
        .envs(config.env.iter().cloned())
        .current_dir(&req.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(AdapterError::Spawn)?;

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
            timeout_terminal = Some(Terminal::Error {
                message: "wall clock exceeded".into(),
                retryable: true,
            });
            force_kill = true;
            break;
        }
        let idle_elapsed = last_activity.elapsed();
        if idle_elapsed >= limits.idle_timeout {
            timeout_terminal = Some(Terminal::Error {
                message: "idle timeout".into(),
                retryable: true,
            });
            force_kill = true;
            break;
        }
        let wait = (limits.wall_clock - wall_elapsed).min(limits.idle_timeout - idle_elapsed);

        let outcome = match tokio::time::timeout(wait, read_line_limited(&mut reader, MAX_LINE_BYTES)).await {
            Err(_elapsed) => continue, // タイムアウト。ループ先頭で上限超過を検知する。
            Ok(Err(e)) => return Err(AdapterError::Io(e)),
            Ok(Ok(outcome)) => outcome,
        };

        match outcome {
            LineOutcome::Eof => break,
            LineOutcome::TooLong => {
                // claude 自身のフォーマットは taskd が定義したものではないため、寛容に無視する（ADR-0006 D5）。
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

    let (terminal, provider_failure): (Terminal, Option<ProviderFailure>) = match (timeout_terminal, &last_result) {
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
            (
                Terminal::Error {
                    message: format!("worker exited without a result message (exit={exit_repr})"),
                    retryable: true,
                },
                pf,
            )
        }
        (None, Some(meta)) => terminal_from_result(&req.workspace, meta).await,
    };

    forward_delegate_file(&req.workspace, sink).await;

    write_result_json(&run_dir, &terminal, provider_failure).await?;

    if let (Terminal::Error { message, .. }, Some(pf)) = (&terminal, provider_failure) {
        return Err(AdapterError::from_provider_failure(pf, message));
    }

    Ok(RunOutcome {
        terminal,
        exit_code: exit_status.code(),
    })
}

/// 壁時計の Unix 秒（ADR-0024 D4: 観測時刻は taskd の壁時計）。`claude_account` の確認・ログイン中継からも使う。
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
        "assistant" => {
            if let Some(content) = value.pointer("/message/content").and_then(|c| c.as_array()) {
                for item in content {
                    match item.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                sink.progress(&truncate(text, 500));
                            }
                        }
                        Some("tool_use") => {
                            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                            let input = item
                                .get("input")
                                .map(|v| truncate(&v.to_string(), 200))
                                .unwrap_or_default();
                            sink.progress(&format!("tool_use: {name} {input}"));
                        }
                        _ => {}
                    }
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
            let usage = value.get("usage").map(|u| Usage {
                input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()),
                output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()),
            });
            let result = value.get("result").and_then(|r| r.as_str()).map(|s| s.to_string());
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

/// `result` メッセージと結果ファイルから終端を合成する（ADR-0006 D3/D4, ADR-0010 D5）。呼び出し元は
/// `result` メッセージを一度でも観測できた場合にのみこれを呼ぶ（観測できなかった場合は
/// クラッシュとして扱い、この関数を呼ばずに `Error` にする。ADR-0006 D4）。`is_error`/`subtype != "success"`
/// のときは `result` のテキスト（無ければ `subtype`）を供給側失敗として分類する。
async fn terminal_from_result(
    workspace: &Path,
    last_result: &ResultMeta,
) -> (Terminal, Option<ProviderFailure>) {
    if last_result.is_error || last_result.subtype != "success" {
        let text_for_classification = last_result.result.clone().unwrap_or_else(|| last_result.subtype.clone());
        let pf = classify_provider_failure(&text_for_classification);
        let message = match &last_result.result {
            Some(result_text) => format!("claude result: {}: {result_text}", last_result.subtype),
            None => format!("claude result: {}", last_result.subtype),
        };
        return (Terminal::Error { message, retryable: true }, pf);
    }

    let result_path = workspace.join("artifacts").join("result.json");
    let text = match tokio::fs::read_to_string(&result_path).await {
        Ok(t) => t,
        Err(_) => {
            return (
                Terminal::Error {
                    message: "claude exited without artifacts/result.json".to_string(),
                    retryable: true,
                },
                None,
            );
        }
    };

    let terminal = match serde_json::from_str::<ResultFile>(&text) {
        Ok(rf) => {
            if let Some(question) = rf.question {
                Terminal::Question { text: question }
            } else if let Some(summary) = rf.summary {
                Terminal::Done {
                    summary,
                    evidence: lenient_evidence(rf.evidence),
                    usage: last_result.usage,
                }
            } else {
                Terminal::Error {
                    message: "artifacts/result.json has neither 'summary' nor 'question'".into(),
                    retryable: true,
                }
            }
        }
        Err(e) => Terminal::Error {
            message: format!("artifacts/result.json is not valid JSON: {e}"),
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
        delegated: Mutex<Vec<Vec<DelegateTask>>>,
        rate_limits: Mutex<Vec<RateLimitObservation>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress.lock().unwrap_or_else(|e| e.into_inner()).push(msg.to_string());
        }
        fn artifact(&self, _artifact: &ArtifactRef) {}
        fn delegate(&self, tasks: &[DelegateTask]) {
            self.delegated.lock().unwrap_or_else(|e| e.into_inner()).push(tasks.to_vec());
        }
        fn rate_limit(&self, obs: RateLimitObservation) {
            self.rate_limits.lock().unwrap_or_else(|e| e.into_inner()).push(obs);
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
            workspace,
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
        let prompt = build_prompt(&task, &context, "run-xyz");
        assert!(prompt.contains(&task.objective));
        assert!(prompt.contains("cargo test exit 101"));
        assert!(prompt.contains("artifacts/result.json"));
        assert!(prompt.contains("reviewer will independently re-run"));
        assert!(prompt.contains("run-xyz"));
        assert!(prompt.contains("attempt 1 of"));
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
        let execute_prompt = build_prompt(&execute_task, &context, "run-a1");
        assert!(execute_prompt.contains("## Answers from a human to your earlier questions"));
        assert!(execute_prompt.contains("- Q: which crate version?"));
        assert!(execute_prompt.contains("A: 1.0"));

        let mut plan_task = crate::protocol::tests::sample_task();
        plan_task.kind = task_core::TaskKind::Plan;
        let plan_prompt = build_prompt(&plan_task, &context, "run-a2");
        assert!(plan_prompt.contains("## Answers from a human to your earlier questions"));
        assert!(plan_prompt.contains("- Q: which crate version?"));

        // No answers: the section must not appear at all.
        let no_answers_prompt = build_prompt(&execute_task, &RunContext::default(), "run-a3");
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
        let prompt = build_prompt(&task, &context, "run-plan-1");
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
            }],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-review-1");
        assert!(prompt.contains("artifacts/review.json"));
        assert!(prompt.contains("criterion 0"));
        assert!(prompt.contains("added usage example"));
        assert!(prompt.contains("cargo test"));
        assert!(prompt.contains("artifacts/readme.diff"));
        assert!(prompt.contains("read-only"));

        // context.review = None must not panic and still produces a usable prompt.
        let none_context = RunContext::default();
        let prompt_none = build_prompt(&task, &none_context, "run-review-2");
        assert!(prompt_none.contains("no review context"));
        assert!(prompt_none.contains("artifacts/review.json"));
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
        let outcome = adapter.run(req, "run-1", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, evidence, usage } => {
                assert_eq!(summary, "added usage example");
                assert!(evidence.is_empty());
                assert_eq!(usage, Some(Usage { input_tokens: Some(10), output_tokens: Some(20) }));
            }
            other => panic!("expected done, got {other:?}"),
        }
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m == "working on it"));
        assert!(progress.iter().any(|m| m.starts_with("tool_use: Bash")));
        assert!(dir.path().join("runs/run-1/stdout.jsonl").is_file());

        // P-26 (ADR-0010 D10): the terminal is also normalized into `runs/<run_id>/result.json`,
        // readable by task-dispatch as a `WorkerMessage::Done`.
        let result_json = std::fs::read_to_string(dir.path().join("runs/run-1/result.json")).unwrap();
        match serde_json::from_str::<crate::protocol::WorkerMessage>(result_json.trim()).unwrap() {
            crate::protocol::WorkerMessage::Done { summary, .. } => assert_eq!(summary, "added usage example"),
            other => panic!("expected done in result.json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn success_without_result_file_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"echo '{"type":"result","subtype":"success","is_error":false}'"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-2", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
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
        let outcome = adapter.run(req, "run-3", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Question { text } => assert_eq!(text, "which crate version?"),
            other => panic!("expected question, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn error_subtype_wins_even_if_result_file_claims_done() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"claimed done","evidence":[]}' > artifacts/result.json
echo '{"type":"result","subtype":"error_max_turns","is_error":true}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-4", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("error_max_turns"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
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
        let outcome = adapter.run(req, "run-5", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("not valid JSON"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wall_clock_exceeded_kills_and_reports_error() {
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
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("wall clock exceeded"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
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
        let outcome = adapter.run(req, "run-8", default_limits(), &sink).await.unwrap();
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
        let outcome = adapter.run(req, "run-9", default_limits(), &sink).await.unwrap();
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
        let config = stub_claude(dir.path(), r#"echo '{"type":"result","subtype":"success","is_error":false}'"#);
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-10", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => panic!("expected error (stale file must be cleared, not reused), got {other:?}"),
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
        let outcome = adapter.run(req, "run-11", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, evidence, .. } => {
                assert_eq!(summary, "all good");
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].command.as_deref(), Some("cargo test"));
            }
            other => panic!("expected done, got {other:?}"),
        }
        let prompt = build_prompt(&crate::protocol::tests::sample_task(), &RunContext::default(), "r");
        assert!(prompt.contains("plain strings are not accepted"));
    }

    /// ADR-0033 D4 / D6（Phase 24）: 前置き（役職と brief・記憶・直近のやり取り）がプロンプトに入り、
    /// 並びは `## Task` の直後・`## Objective` の前。`RunContext` が Phase 23 までの中身なら出力は変わらない。
    #[test]
    fn build_prompt_puts_the_person_preamble_between_the_run_line_and_the_objective() {
        let task = crate::protocol::tests::sample_task();
        let bare = build_prompt(&task, &RunContext::default(), "run-p0");

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
        let prompt = build_prompt(&task, &context, "run-p1");
        let at = |n: &str| prompt.find(n).unwrap_or_else(|| panic!("missing {n:?} in\n{prompt}"));
        assert!(at("(run run-p1") < at("## あなた: 関連研究調査課 (research-survey)"));
        assert!(at("## あなた:") < at("## 覚えていること"));
        assert!(at("## 覚えていること") < at("## 直近のやり取り"));
        assert!(at("## 直近のやり取り") < at("## Objective"));
        assert!(prompt.contains("memory.notes"), "記憶の書き方の指示が付く");

        // Phase 23 までの `RunContext` では 1 バイトも変わらない。
        assert!(!bare.contains("## あなた"));
        assert!(!bare.contains("覚えておくこと"));
        assert_eq!(bare, build_prompt(&task, &RunContext::default(), "run-p0"));
    }

    /// ADR-0033 D4（Phase 24）: 組織図を渡した run には `## 組織図` と `assignee` の指示が入る。
    /// 渡していない run（Phase 23 までの構成）では出ない。
    #[test]
    fn build_prompt_includes_the_org_chart_and_the_assignee_instruction_only_when_present() {
        let mut task = crate::protocol::tests::sample_task();
        let org = vec![
            crate::protocol::OrgNodeContext {
                id: "research".into(),
                name: "研究部".into(),
                kind: task_core::OrgKind::Department,
                parent_id: Some("secretary".into()),
                brief: "課に振り分ける".into(),
                genre: None,
            },
            crate::protocol::OrgNodeContext {
                id: "research-survey".into(),
                name: "関連研究調査課".into(),
                kind: task_core::OrgKind::Section,
                parent_id: Some("research".into()),
                brief: "関連研究を洗う".into(),
                genre: Some("literature".into()),
            },
        ];
        let context = RunContext { organization: org, ..RunContext::default() };

        let execute = build_prompt(&task, &context, "run-o1");
        assert!(execute.contains("## 組織図 (who you can assign work to)"));
        assert!(execute.contains("- research-survey [課] 関連研究調査課 (親: research, 分野: literature) — 関連研究を洗う"));
        assert!(execute.contains("別の部"), "部をまたぐ委譲の注意が入る: {execute}");
        assert!(execute.contains("\"assignee\":\"<optional org node id>\""));

        task.kind = task_core::TaskKind::Plan;
        let plan = build_prompt(&task, &context, "run-o2");
        assert!(plan.contains("## 組織図"));
        assert!(plan.contains("子タスクごとに `assignee` を必ず書け"));
        assert!(plan.contains("`role` は必要なときだけ"));

        assert!(!build_prompt(&task, &RunContext::default(), "run-o3").contains("組織図"));
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
        let prompt = build_prompt(&task, &context, "run-role-1");
        assert!(prompt.contains("## Role: lead"));
        assert!(prompt.contains("You coordinate the work of others."));

        let no_role_prompt = build_prompt(&task, &RunContext::default(), "run-role-2");
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
        let prompt = build_prompt(&task, &context, "run-genre-1");
        assert!(prompt.contains("## Genre: literature"));
        assert!(prompt.contains("related work survey and novelty checks"));

        // available_genres が task.genre を含まない（あるいは空）なら Genre 見出しは出ない。
        let empty_prompt = build_prompt(&task, &RunContext::default(), "run-genre-2");
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
        let prompt = build_prompt(&task, &context, "run-avail-1");
        assert!(prompt.contains("使える専門家"));
        assert!(prompt.contains("literature-scout"));
        assert!(prompt.contains("literature-reader"));

        let no_genres_prompt = build_prompt(&task, &RunContext::default(), "run-avail-2");
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
            roles: vec!["literature-scout".into(), "literature-reader".into(), "novelty-skeptic".into()],
        };
        let context = RunContext {
            available_genres: vec![GenreContext::from(&genre_spec)],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-avail-shape");
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
        let prompt = build_prompt(&task, &context, "run-avail-omit");
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
        let prompt = build_prompt(&task, &context, "run-plan-avail-1");
        assert!(prompt.contains("使える専門家"));
        assert!(prompt.contains("literature-reader"));
        assert!(prompt.contains("artifacts/plan.json"));

        let no_genres_prompt = build_prompt(&task, &RunContext::default(), "run-plan-avail-2");
        assert!(!no_genres_prompt.contains("使える専門家"));
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
                outcome: Some("done".into()),
                artifacts: vec![],
                workspace: Some(std::path::PathBuf::from("/tmp/child-ws")),
            }],
            ..RunContext::default()
        };
        let prompt = build_prompt(&task, &context, "run-agg-1");
        assert!(prompt.contains("## Delegated child tasks (this is the aggregate run)"));
        assert!(prompt.contains("implement parser"));
        assert!(prompt.contains("artifacts/summary.md"));

        let no_children_prompt = build_prompt(&task, &RunContext::default(), "run-agg-2");
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
        let outcome = adapter.run(req, "run-delegate-1", default_limits(), &sink).await.unwrap();
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
        let outcome = adapter.run(req, "run-delegate-2", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { .. } => {}
            other => panic!("expected done, got {other:?}"),
        }
        assert!(sink.delegated.lock().unwrap().is_empty());
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m.contains("delegate.json ignored")), "{progress:?}");
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
        let outcome = adapter.run(req, "run-rate-1", default_limits(), &sink).await.unwrap();
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
        config.env.push(("CLAUDE_SECURESTORAGE_CONFIG_DIR".to_string(), "old-account-dir".to_string()));
        let base = ClaudeCodeAdapter::new(config);
        let with_env = base
            .with_env(&[("CLAUDE_SECURESTORAGE_CONFIG_DIR".to_string(), "new-account-dir".to_string())])
            .expect("claude-code supports with_env");

        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = with_env.run(req, "run-env-1", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let seen = std::fs::read_to_string(&out_file).unwrap();
        assert_eq!(seen, "new-account-dir");
    }
}
