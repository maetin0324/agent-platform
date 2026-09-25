//! ADR-0072（Task execution decomposition）Phase E1: Run lifecycle / checkpoint / continuation。
//!
//! 純粋なデータ定義と純粋関数だけを置く（I/O・LLM 呼び出し・プロセス起動はしない。ADR-0001 D2）。
//! git の読み取りや `checkpoint.json` の読み込みは `task-dispatch::checkpoint`（I/O 層）が行い、
//! ここに集めた事実（[`MechanicalCheckpoint`]）と worker の申告（[`WorkerCheckpointInput`]）を
//! [`merge_checkpoint`] で決定的に合成する（ADR-0072 D8）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// D8: checkpoint の JSON schema 版。
pub const CHECKPOINT_SCHEMA: &str = "celeris.checkpoint/1";
/// D8: checkpoint 全体の上限（16 KiB）。
pub const CHECKPOINT_MAX_BYTES: usize = 16 * 1024;
/// D8: 配列 1 つあたりの上限（30 件）。
pub const CHECKPOINT_MAX_ITEMS: usize = 30;
/// D8: 文字列 1 つあたりの上限（500 文字）。
pub const CHECKPOINT_MAX_STRING_CHARS: usize = 500;

// ---------------------------------------------------------------------------
// D7: Run の終わり方
// ---------------------------------------------------------------------------

/// D7: 予算切れの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BudgetKind {
    Turns,
    WallClock,
    Context,
}

/// D7: harness / 供給側都合の失敗の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HarnessErrorClass {
    Supply,
    Infra,
    LeaseExpired,
    IdleTimeout,
}

/// D7: `task_core::execution::RunEnd`（`WorkerFinished.end` に入れる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEnd {
    Completed,
    Yielded,
    BudgetExhausted { kind: BudgetKind },
    Question,
    Failed { retryable: bool },
    HarnessError { class: HarnessErrorClass },
    Cancelled,
}

impl RunEnd {
    /// この Run の終わり方が continuation（D9/D11）の対象か（予算切れ、または自己申告の yield）。
    pub fn is_continuable(self) -> bool {
        matches!(self, RunEnd::BudgetExhausted { .. } | RunEnd::Yielded)
    }

    /// D8 の checkpoint の `end` 欄に写す（continuation 対象でなければ `None`）。
    pub fn as_checkpoint_end(self) -> Option<CheckpointEnd> {
        match self {
            RunEnd::Completed => Some(CheckpointEnd::Completed),
            RunEnd::Yielded => Some(CheckpointEnd::Yielded),
            RunEnd::BudgetExhausted { .. } => Some(CheckpointEnd::BudgetExhausted),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// D6: Trigger::Continue の `why`
// ---------------------------------------------------------------------------

/// D6: `Trigger::Continue{why}` の種類。E1 で使うのは [`ContinueWhy::Continue`] だけ（予算切れ /
/// yield の続き）。他は E2 以降の配線先（`advance` = WU 完了で次へ、`work_unit_retry` = WU の
/// retry、`planned` = planner の採用、`replan` = 再計画）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContinueWhy {
    Continue,
    Advance,
    WorkUnitRetry,
    Planned,
    Replan,
}

impl ContinueWhy {
    /// `Event::Transitioned.reason` に入る静的な名前（D6 の表）。
    pub fn name(self) -> &'static str {
        match self {
            ContinueWhy::Continue => "continue",
            ContinueWhy::Advance => "advance",
            ContinueWhy::WorkUnitRetry => "work_unit_retry",
            ContinueWhy::Planned => "planned",
            ContinueWhy::Replan => "replan",
        }
    }
}

// ---------------------------------------------------------------------------
// D8: checkpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointEnd {
    Completed,
    Yielded,
    BudgetExhausted,
}

/// checkpoint を合成した出所（D8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointSource {
    Worker,
    Yield,
    Mechanical,
    Merged,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CheckpointDecision {
    pub what: String,
    pub why: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CheckpointFileChange {
    pub path: String,
    /// `"added" | "modified" | "deleted"`（自由記述。worker の申告も daemon の git 検出もここに入る）。
    pub change: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CheckpointTestRun {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CheckpointKnownFailure {
    pub what: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CheckpointArtifactRef {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoState {
    pub branch: String,
    pub base: String,
    pub head: String,
    pub uncommitted: bool,
    pub diff_stat: String,
}

/// D8: daemon が確定させた checkpoint（`celeris.checkpoint/1`）。`CheckpointSaved` イベントと
/// `runs/<run_id>/checkpoint.json` に残す。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Checkpoint {
    pub schema: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit: Option<String>,
    pub run_id: String,
    pub run_seq: u32,
    pub end: CheckpointEnd,
    pub source: CheckpointSource,
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default)]
    pub remaining: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<CheckpointDecision>,
    #[serde(default)]
    pub files_changed: Vec<CheckpointFileChange>,
    #[serde(default)]
    pub tests_run: Vec<CheckpointTestRun>,
    #[serde(default)]
    pub known_failures: Vec<CheckpointKnownFailure>,
    #[serde(default)]
    pub artifact_refs: Vec<CheckpointArtifactRef>,
    pub next_action: String,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_issue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_state: Option<RepoState>,
    #[serde(default)]
    pub recent_activity: Vec<String>,
    pub created_at: String,
}

/// ワーカーが `<artifacts_dir>/checkpoint.json` に書く、または `result.json` の `yield` に添える
/// 申告（D8 の意味の欄だけ）。**寛容に読む**（`deny_unknown_fields` にしない。未知の欄は捨てる）。
/// JSON として読めない・配列であるべき欄が配列でない等の**型の不一致は schema 違反**として扱い、
/// 呼び出し側（`task-dispatch::checkpoint`）は `None` として [`merge_checkpoint`] に渡す。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct WorkerCheckpointInput {
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default)]
    pub remaining: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<CheckpointDecision>,
    #[serde(default)]
    pub files_changed: Vec<CheckpointFileChange>,
    #[serde(default)]
    pub tests_run: Vec<CheckpointTestRun>,
    #[serde(default)]
    pub known_failures: Vec<CheckpointKnownFailure>,
    #[serde(default)]
    pub artifact_refs: Vec<CheckpointArtifactRef>,
    #[serde(default)]
    pub next_action: Option<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub plan_issue: Option<String>,
}

/// worker の checkpoint.json（文字列）を寛容に読む。JSON として不正、または既知の欄の型が
/// 合わなければ `None`（D8: 「schema 違反のときは mechanical だけで作る」）。
pub fn parse_worker_checkpoint(text: &str) -> Option<WorkerCheckpointInput> {
    serde_json::from_str(text).ok()
}

/// daemon が git などから決定的に集めた「事実」（D8 の `mechanical`）。I/O は
/// `task-dispatch::checkpoint` が行い、ここには結果だけを渡す。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MechanicalCheckpoint {
    pub repo_state: Option<RepoState>,
    pub files_changed: Vec<CheckpointFileChange>,
    pub tests_run: Vec<CheckpointTestRun>,
    pub recent_activity: Vec<String>,
}

/// [`merge_checkpoint`] が確定値を組み立てるのに要る、run に固有の情報。
#[derive(Debug, Clone, PartialEq)]
pub struct CheckpointContext {
    pub task_id: String,
    /// E1 では常に `None`（暗黙の WorkUnit）。E2 以降で WorkUnit の id を持つ。
    pub work_unit: Option<String>,
    pub run_id: String,
    pub run_seq: u32,
    pub end: CheckpointEnd,
    pub created_at: String,
}

const NO_CHECKPOINT_REMAINING: &str = "（checkpoint がありません）元の目的を続けてください";
const NO_CHECKPOINT_NEXT_ACTION: &str = "git diff で現状を確認して続きから";

/// D8 の合成規則: 意味の欄は worker を採り、事実の欄（`files_changed` のパス集合）は mechanical を
/// 正として worker の注記を path で添える。`tests_run` は和集合（コマンドで重複を除く）。worker が
/// 無い・schema 違反なら mechanical だけ（`source = mechanical`、`remaining`/`next_action` は
/// 既定文言）。合成後は [`truncate_checkpoint`] で 16 KiB に切り詰める。
pub fn merge_checkpoint(
    worker: Option<WorkerCheckpointInput>,
    mechanical: MechanicalCheckpoint,
    ctx: CheckpointContext,
) -> Checkpoint {
    let source = if worker.is_some() {
        CheckpointSource::Merged
    } else {
        CheckpointSource::Mechanical
    };

    let (
        completed,
        remaining,
        decisions,
        known_failures,
        artifact_refs,
        next_action,
        open_questions,
        plan_issue,
        worker_files,
    ) = match &worker {
        Some(w) => (
            w.completed.clone(),
            w.remaining.clone(),
            w.decisions.clone(),
            w.known_failures.clone(),
            w.artifact_refs.clone(),
            w.next_action
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| NO_CHECKPOINT_NEXT_ACTION.to_string()),
            w.open_questions.clone(),
            w.plan_issue.clone(),
            w.files_changed.clone(),
        ),
        None => (
            Vec::new(),
            vec![NO_CHECKPOINT_REMAINING.to_string()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            NO_CHECKPOINT_NEXT_ACTION.to_string(),
            Vec::new(),
            None,
            Vec::new(),
        ),
    };

    // 事実の欄は mechanical を正とし、worker の注記を path で添える。
    let files_changed: Vec<CheckpointFileChange> = mechanical
        .files_changed
        .into_iter()
        .map(|mut f| {
            if let Some(note) = worker_files
                .iter()
                .find(|wf| wf.path == f.path)
                .and_then(|wf| wf.note.clone())
            {
                f.note = Some(note);
            }
            f
        })
        .collect();

    // tests_run は和集合（コマンドで重複を除く。mechanical を先に、worker の未出のコマンドを足す）。
    let mut tests_run = mechanical.tests_run;
    if let Some(w) = &worker {
        for t in &w.tests_run {
            if !tests_run.iter().any(|e| e.command == t.command) {
                tests_run.push(t.clone());
            }
        }
    }

    let mut checkpoint = Checkpoint {
        schema: CHECKPOINT_SCHEMA.to_string(),
        task_id: ctx.task_id,
        work_unit: ctx.work_unit,
        run_id: ctx.run_id,
        run_seq: ctx.run_seq,
        end: ctx.end,
        source,
        completed,
        remaining,
        decisions,
        files_changed,
        tests_run,
        known_failures,
        artifact_refs,
        next_action,
        open_questions,
        plan_issue,
        repo_state: mechanical.repo_state,
        recent_activity: mechanical.recent_activity,
        created_at: ctx.created_at,
    };
    truncate_checkpoint(&mut checkpoint);
    checkpoint
}

fn truncate_string(s: &mut String) {
    if s.chars().count() > CHECKPOINT_MAX_STRING_CHARS {
        let truncated: String = s.chars().take(CHECKPOINT_MAX_STRING_CHARS).collect();
        *s = format!("{truncated}…");
    }
}

fn truncate_strings(v: &mut [String]) {
    for s in v.iter_mut() {
        truncate_string(s);
    }
}

fn pop_one<T>(v: &mut Vec<T>) -> bool {
    v.pop().is_some()
}

fn checkpoint_byte_len(cp: &Checkpoint) -> usize {
    serde_json::to_vec(cp).map(|v| v.len()).unwrap_or(0)
}

/// D8: 配列 30 件・文字列 500 文字の上限を適用し、それでも全体が 16 KiB を超えるなら決定的に
/// 縮める（`recent_activity` → `tests_run` → `known_failures` → `files_changed` → `decisions` →
/// `artifact_refs` → `open_questions` → `remaining` → `completed` の順に末尾から落とし、
/// それでも収まらなければ `next_action` を空にする）。
pub fn truncate_checkpoint(cp: &mut Checkpoint) {
    truncate_string(&mut cp.next_action);
    if let Some(pi) = &mut cp.plan_issue {
        truncate_string(pi);
    }
    cp.completed.truncate(CHECKPOINT_MAX_ITEMS);
    truncate_strings(&mut cp.completed);
    cp.remaining.truncate(CHECKPOINT_MAX_ITEMS);
    truncate_strings(&mut cp.remaining);
    cp.open_questions.truncate(CHECKPOINT_MAX_ITEMS);
    truncate_strings(&mut cp.open_questions);
    cp.recent_activity.truncate(CHECKPOINT_MAX_ITEMS);
    truncate_strings(&mut cp.recent_activity);

    cp.decisions.truncate(CHECKPOINT_MAX_ITEMS);
    for d in &mut cp.decisions {
        truncate_string(&mut d.what);
        truncate_string(&mut d.why);
    }
    cp.files_changed.truncate(CHECKPOINT_MAX_ITEMS);
    for f in &mut cp.files_changed {
        truncate_string(&mut f.path);
        truncate_string(&mut f.change);
        if let Some(n) = &mut f.note {
            truncate_string(n);
        }
    }
    cp.tests_run.truncate(CHECKPOINT_MAX_ITEMS);
    for t in &mut cp.tests_run {
        truncate_string(&mut t.command);
        if let Some(s) = &mut t.summary {
            truncate_string(s);
        }
    }
    cp.known_failures.truncate(CHECKPOINT_MAX_ITEMS);
    for k in &mut cp.known_failures {
        truncate_string(&mut k.what);
        if let Some(d) = &mut k.detail {
            truncate_string(d);
        }
    }
    cp.artifact_refs.truncate(CHECKPOINT_MAX_ITEMS);
    for a in &mut cp.artifact_refs {
        truncate_string(&mut a.path);
    }
    if let Some(rs) = &mut cp.repo_state {
        truncate_string(&mut rs.branch);
        truncate_string(&mut rs.base);
        truncate_string(&mut rs.head);
        truncate_string(&mut rs.diff_stat);
    }

    // 全体の上限（16 KiB）。決定的な優先順位で末尾から落とす。
    while checkpoint_byte_len(cp) > CHECKPOINT_MAX_BYTES {
        let shrank = pop_one(&mut cp.recent_activity)
            || pop_one(&mut cp.tests_run)
            || pop_one(&mut cp.known_failures)
            || pop_one(&mut cp.files_changed)
            || pop_one(&mut cp.decisions)
            || pop_one(&mut cp.artifact_refs)
            || pop_one(&mut cp.open_questions)
            || pop_one(&mut cp.remaining)
            || pop_one(&mut cp.completed);
        if shrank {
            continue;
        }
        if !cp.next_action.is_empty() {
            cp.next_action.clear();
            continue;
        }
        // これ以上縮められない（すべて空でもまだ超過。理論上到達しない — schema/task_id/run_id 等の
        // 固定欄だけで 16 KiB を超えることはない）。無限ループを避けて諦める。
        break;
    }
}

/// D18: 進捗の定義。次のどれかが起きていれば進捗あり: `files_changed` のパス集合か HEAD が変わる /
/// `completed` が増える / `remaining` が減る。前の checkpoint が無ければ（この WU/Task で最初の
/// checkpoint）常に進捗ありとする。
pub fn checkpoint_shows_progress(prev: Option<&Checkpoint>, next: &Checkpoint) -> bool {
    let Some(prev) = prev else {
        return true;
    };
    let prev_paths: std::collections::BTreeSet<&str> =
        prev.files_changed.iter().map(|f| f.path.as_str()).collect();
    let next_paths: std::collections::BTreeSet<&str> =
        next.files_changed.iter().map(|f| f.path.as_str()).collect();
    if prev_paths != next_paths {
        return true;
    }
    let prev_head = prev.repo_state.as_ref().map(|r| r.head.as_str());
    let next_head = next.repo_state.as_ref().map(|r| r.head.as_str());
    if prev_head != next_head {
        return true;
    }
    if next.completed.len() > prev.completed.len() {
        return true;
    }
    if next.remaining.len() < prev.remaining.len() {
        return true;
    }
    false
}

/// D7: context 超過の実機の文言は未確認（ADR-0072 §7 U1）。既知の候補の字句で判定する
/// （分類できなければ従来どおり字句判定なし = 呼び出し側が安全側の `WorkerError`/`Failed` に倒す）。
/// `retry_policy::is_budget_outcome` と各アダプタの終端判定の両方から使う単一の語彙源。
pub fn looks_like_context_exceeded(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "prompt is too long",
        "context window",
        "context_length_exceeded",
        "context length exceeded",
        "context_length",
        "maximum context length",
        "exceeds the model's context",
        "exceeds context limit",
    ]
    .iter()
    .any(|w| t.contains(w))
}

/// 生成したスキーマ（`docs/protocol/checkpoint.schema.json`。`UPDATE_SCHEMA=1` で再生成）。
pub fn schema_value() -> serde_json::Value {
    let schema = schemars::schema_for!(Checkpoint);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

// ---------------------------------------------------------------------------
// ADR-0072 D16（Phase E4）: reviewer repair（不合格を局所的に修復する）
// ---------------------------------------------------------------------------

/// D16: 最終レビューで不合格になった 1 条件の、分類に要る最小限の情報。`pass = false` の
/// `Verdict` だけを渡す想定（呼び出し側が `task.acceptance[criterion_idx].check` を添える）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCheck {
    pub check: crate::model::Check,
    pub reason: String,
    /// `Check::Reviewer` の不合格に添えられた `review.json` の `repair` ヒント（D16: `{"scope":"local",
    /// "class":"format|lint|test|doc|other","hint":"…"}`）。それ以外の `check` では常に `None`。
    pub repair_hint: Option<ReviewRepairHint>,
}

/// `review.json` の `repair` ヒント（`scope`/`class` だけを使う。`hint` の自由記述は呼び出し側の
/// ログ用で分類には使わない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRepairHint {
    pub scope: String,
    pub class: String,
}

/// D16: `reviewer_local` の repair WU の予算を決めるための、内側の（サブ）分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerRepairKind {
    Format,
    Lint,
    Test,
    Other,
}

impl ReviewerRepairKind {
    fn parse(class: &str) -> Self {
        match class {
            "format" => ReviewerRepairKind::Format,
            "lint" => ReviewerRepairKind::Lint,
            "test" => ReviewerRepairKind::Test,
            _ => ReviewerRepairKind::Other,
        }
    }
}

/// D16 の分類表: `format` / `lint` / `test_small` / `reviewer_local` / `merge_base`。この 5 つが
/// `repairs` の「同じ class」カウンタ（既定 2 回まで）のバケットにもなる。**`merge_base` はこの
/// 関数からは返らない**（配送〈`crates/celeris/src/delivery.rs`〉自身の技術的な失敗から直接組み立てる。
/// D16 の表の条件が review の verdict ではなく配送の失敗そのものを指しているため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairClass {
    Format,
    Lint,
    TestSmall,
    ReviewerLocal(ReviewerRepairKind),
    MergeBase,
}

impl RepairClass {
    /// `repairs` の「同じ class は 2 回まで」の集計に使うキー（`reviewer_local` はサブ分類を問わず
    /// 1 つのバケット）。
    pub fn bucket(self) -> &'static str {
        match self {
            RepairClass::Format => "format",
            RepairClass::Lint => "lint",
            RepairClass::TestSmall => "test_small",
            RepairClass::ReviewerLocal(_) => "reviewer_local",
            RepairClass::MergeBase => "merge_base",
        }
    }

    /// D16 の表の repair WU の予算（`max_turns`, `max_wall_secs`）。
    pub fn budget(self) -> (u32, u64) {
        match self {
            RepairClass::Format => (12, 600),
            RepairClass::Lint => (20, 1200),
            RepairClass::TestSmall | RepairClass::MergeBase => (30, 1800),
            RepairClass::ReviewerLocal(kind) => match kind {
                ReviewerRepairKind::Format => (12, 600),
                ReviewerRepairKind::Lint => (20, 1200),
                ReviewerRepairKind::Test | ReviewerRepairKind::Other => (30, 1800),
            },
        }
    }
}

/// D16: 修復できる不合格かどうかの判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairDecision {
    Repairable(RepairClass),
    /// 修復できない不合格が 1 つでもある、または修復できる種類が混在している
    /// （`Check::Human` の不合格を含む場合も、この列挙値になる）。
    Substantive,
}

const FMT_COMMAND_WORDS: [&str; 7] = [
    "cargo fmt",
    "rustfmt",
    "prettier",
    "biome format",
    "gofmt",
    "black",
    "ruff format",
];
const LINT_COMMAND_WORDS: [&str; 7] = [
    "clippy",
    "eslint",
    "biome check",
    "biome lint",
    "ruff check",
    "tsc",
    "typecheck",
];
const TEST_COMMAND_WORDS: [&str; 5] = ["cargo test", "pnpm test", "vitest", "pytest", "go test"];

/// `reason`（コマンドの stdout/stderr tail を含む）から、失敗したテストの件数を読む。
/// `test result: FAILED. N passed; M failed`、`M failed` のような字句を探す（決定的な字句判定。
/// D16）。見つからなければ `None`（test_small と判定できない = substantive に倒れる）。
fn failed_test_count(reason: &str) -> Option<u32> {
    let bytes = reason.as_bytes();
    let needle = b" failed";
    let mut best: Option<u32> = None;
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // needle の直前にある数字の連続を後ろ向きに読む。
            let mut j = i;
            while j > 0 && bytes[j - 1].is_ascii_digit() {
                j -= 1;
            }
            if j < i
                && let Ok(n) = reason[j..i].parse::<u32>()
            {
                best = Some(n);
            }
        }
        i += 1;
    }
    best
}

fn classify_command(cmd: &str, reason: &str) -> Option<RepairClass> {
    let c = cmd.to_lowercase();
    if FMT_COMMAND_WORDS.iter().any(|w| c.contains(w)) {
        return Some(RepairClass::Format);
    }
    if LINT_COMMAND_WORDS.iter().any(|w| c.contains(w)) {
        return Some(RepairClass::Lint);
    }
    if TEST_COMMAND_WORDS.iter().any(|w| c.contains(w)) {
        return match failed_test_count(reason) {
            Some(n) if (1..=3).contains(&n) => Some(RepairClass::TestSmall),
            _ => None,
        };
    }
    None
}

/// D16「reason が fmt / clippy の語だけを指す（字句の予備判定）」: 明示の `repair` ヒントが無い
/// `Check::Reviewer` の不合格に対する、決定的な字句フォールバック。
fn classify_reviewer_reason(reason: &str) -> Option<RepairClass> {
    let r = reason.to_lowercase();
    let mentions_fmt = r.contains("fmt") || r.contains("format");
    let mentions_lint = r.contains("clippy") || r.contains("lint");
    if mentions_fmt && !mentions_lint {
        return Some(RepairClass::ReviewerLocal(ReviewerRepairKind::Format));
    }
    if mentions_lint && !mentions_fmt {
        return Some(RepairClass::ReviewerLocal(ReviewerRepairKind::Lint));
    }
    None
}

fn classify_one(f: &FailedCheck) -> Option<RepairClass> {
    match &f.check {
        crate::model::Check::Command { cmd, .. } => classify_command(cmd, &f.reason),
        crate::model::Check::Reviewer => match &f.repair_hint {
            Some(hint) if hint.scope == "local" => Some(RepairClass::ReviewerLocal(
                ReviewerRepairKind::parse(&hint.class),
            )),
            Some(_) => None,
            None => classify_reviewer_reason(&f.reason),
        },
        // ArtifactExists / KnowledgePage / Human: D16 の表に無い = 修復できない。
        _ => None,
    }
}

/// D16: 不合格の verdict をすべて見て、**全部が同じ修復できる class** のときだけ
/// `RepairDecision::Repairable` にする（1 つでも修復できない、または class が混在すれば
/// `Substantive`）。`failing` は空を渡さない（呼び出し側は不合格が無ければそもそも呼ばない）。
pub fn classify_review_failure(failing: &[FailedCheck]) -> RepairDecision {
    let mut classes = failing.iter().map(classify_one);
    let Some(Some(first)) = classes.next() else {
        return RepairDecision::Substantive;
    };
    for c in classes {
        match c {
            Some(c) if c.bucket() == first.bucket() => {}
            _ => return RepairDecision::Substantive,
        }
    }
    RepairDecision::Repairable(first)
}

/// D16: repair WU の objective（**最小の context**。元の実装の context は作り直さない）。決定的な
/// 文字列合成のみ（I/O は無い）。`failing_details` は `Verdict.reason`（cmd / exit / stdout・stderr の
/// tail をそのまま含む）を 1 件 1 行ずつ渡す。`diff_stat`（`git diff --stat` の要約）は任意。
pub fn build_repair_objective(
    class: RepairClass,
    failing_details: &[String],
    task_title: &str,
    task_objective: &str,
    diff_stat: Option<&str>,
) -> String {
    let objective_preview: String = task_objective.chars().take(600).collect();
    let mut s = String::new();
    s.push_str(
        "次の検査が失敗した。失敗を直すことだけをせよ。設計や他のコードは変えるな。\
直した後に同じコマンドを実行して exit を確かめよ。\n\n",
    );
    s.push_str(&format!("## 分類\n{}\n\n", class.bucket()));
    s.push_str("## 失敗した検査\n");
    for d in failing_details {
        s.push_str(&format!("- {d}\n"));
    }
    s.push_str(&format!(
        "\n## 対象タスク（参考。全文ではない）\n- title: {task_title}\n- objective（先頭 600 文字）: {objective_preview}\n"
    ));
    if let Some(stat) = diff_stat {
        s.push_str(&format!("\n## git diff --stat\n{stat}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(end: CheckpointEnd) -> CheckpointContext {
        CheckpointContext {
            task_id: "01TASK".into(),
            work_unit: None,
            run_id: "01RUN".into(),
            run_seq: 2,
            end,
            created_at: "2026-09-24T12:00:00Z".into(),
        }
    }

    #[test]
    fn missing_worker_checkpoint_falls_back_to_mechanical_defaults() {
        let mechanical = MechanicalCheckpoint {
            repo_state: Some(RepoState {
                branch: "celeris/x".into(),
                base: "main@abc".into(),
                head: "def".into(),
                uncommitted: true,
                diff_stat: "1 file changed".into(),
            }),
            files_changed: vec![CheckpointFileChange {
                path: "src/lib.rs".into(),
                change: "modified".into(),
                note: None,
            }],
            tests_run: vec![],
            recent_activity: vec!["Bash: cargo test".into()],
        };
        let cp = merge_checkpoint(None, mechanical, ctx(CheckpointEnd::BudgetExhausted));
        assert_eq!(cp.source, CheckpointSource::Mechanical);
        assert_eq!(cp.remaining, vec![NO_CHECKPOINT_REMAINING.to_string()]);
        assert_eq!(cp.next_action, NO_CHECKPOINT_NEXT_ACTION);
        assert_eq!(cp.files_changed.len(), 1);
        assert_eq!(cp.schema, CHECKPOINT_SCHEMA);
    }

    #[test]
    fn schema_violation_is_treated_like_missing_checkpoint() {
        // 型が合わない（`completed` が配列でない）。
        assert!(parse_worker_checkpoint(r#"{"completed": "not-an-array"}"#).is_none());
        // 壊れた JSON。
        assert!(parse_worker_checkpoint("{not json").is_none());
        // 空オブジェクトは schema 違反ではない（全欄が既定値で読める）。
        assert!(parse_worker_checkpoint("{}").is_some());
    }

    #[test]
    fn worker_checkpoint_wins_semantic_fields_mechanical_wins_factual_fields() {
        let worker = WorkerCheckpointInput {
            completed: vec!["store に execution.rs を追加".into()],
            remaining: vec!["dispatcher の配線".into()],
            next_action: Some("dispatcher.rs の on_worker_finished を直す".into()),
            files_changed: vec![CheckpointFileChange {
                path: "crates/task-core/src/execution.rs".into(),
                change: "added".into(),
                note: Some("型と純粋関数".into()),
            }],
            ..Default::default()
        };
        let mechanical = MechanicalCheckpoint {
            files_changed: vec![
                CheckpointFileChange {
                    path: "crates/task-core/src/execution.rs".into(),
                    change: "added".into(),
                    note: None,
                },
                CheckpointFileChange {
                    path: "crates/task-core/src/model.rs".into(),
                    change: "modified".into(),
                    note: None,
                },
            ],
            ..Default::default()
        };
        let cp = merge_checkpoint(
            Some(worker),
            mechanical,
            ctx(CheckpointEnd::BudgetExhausted),
        );
        assert_eq!(cp.source, CheckpointSource::Merged);
        assert_eq!(
            cp.completed,
            vec!["store に execution.rs を追加".to_string()]
        );
        assert_eq!(cp.next_action, "dispatcher.rs の on_worker_finished を直す");
        // mechanical が正（git が見つけた 2 ファイルとも残る）。worker の note は path で付く。
        assert_eq!(cp.files_changed.len(), 2);
        let annotated = cp
            .files_changed
            .iter()
            .find(|f| f.path.ends_with("execution.rs"))
            .unwrap();
        assert_eq!(annotated.note.as_deref(), Some("型と純粋関数"));
        let other = cp
            .files_changed
            .iter()
            .find(|f| f.path.ends_with("model.rs"))
            .unwrap();
        assert_eq!(other.note, None);
    }

    #[test]
    fn tests_run_is_a_union_deduplicated_by_command() {
        let worker = WorkerCheckpointInput {
            tests_run: vec![
                CheckpointTestRun {
                    command: "cargo test -p task-core".into(),
                    exit: Some(0),
                    summary: Some("12 passed".into()),
                },
                CheckpointTestRun {
                    command: "cargo clippy".into(),
                    exit: Some(0),
                    summary: None,
                },
            ],
            ..Default::default()
        };
        let mechanical = MechanicalCheckpoint {
            tests_run: vec![CheckpointTestRun {
                command: "cargo test -p task-core".into(),
                exit: None,
                summary: None,
            }],
            ..Default::default()
        };
        let cp = merge_checkpoint(
            Some(worker),
            mechanical,
            ctx(CheckpointEnd::BudgetExhausted),
        );
        assert_eq!(cp.tests_run.len(), 2, "{:?}", cp.tests_run);
        // mechanical のコマンドが先（重複除去は worker 側を捨てる）。
        assert_eq!(cp.tests_run[0].command, "cargo test -p task-core");
        assert_eq!(cp.tests_run[0].exit, None);
        assert_eq!(cp.tests_run[1].command, "cargo clippy");
    }

    #[test]
    fn truncate_checkpoint_enforces_item_and_string_caps() {
        let mut cp = Checkpoint {
            schema: CHECKPOINT_SCHEMA.into(),
            task_id: "t".into(),
            work_unit: None,
            run_id: "r".into(),
            run_seq: 1,
            end: CheckpointEnd::BudgetExhausted,
            source: CheckpointSource::Mechanical,
            completed: (0..50).map(|i| format!("item {i}")).collect(),
            remaining: vec!["x".repeat(1000)],
            decisions: vec![],
            files_changed: vec![],
            tests_run: vec![],
            known_failures: vec![],
            artifact_refs: vec![],
            next_action: String::new(),
            open_questions: vec![],
            plan_issue: None,
            repo_state: None,
            recent_activity: vec![],
            created_at: "2026-09-24T00:00:00Z".into(),
        };
        truncate_checkpoint(&mut cp);
        assert_eq!(cp.completed.len(), CHECKPOINT_MAX_ITEMS);
        assert!(cp.remaining[0].chars().count() <= CHECKPOINT_MAX_STRING_CHARS + 1);
        assert!(cp.remaining[0].ends_with('…'));
    }

    #[test]
    fn truncate_checkpoint_shrinks_to_the_overall_byte_cap() {
        let mut cp = Checkpoint {
            schema: CHECKPOINT_SCHEMA.into(),
            task_id: "t".into(),
            work_unit: None,
            run_id: "r".into(),
            run_seq: 1,
            end: CheckpointEnd::BudgetExhausted,
            source: CheckpointSource::Merged,
            completed: (0..CHECKPOINT_MAX_ITEMS)
                .map(|i| format!("completed item number {i} ").repeat(5))
                .collect(),
            remaining: (0..CHECKPOINT_MAX_ITEMS)
                .map(|i| format!("remaining item number {i} ").repeat(5))
                .collect(),
            decisions: (0..CHECKPOINT_MAX_ITEMS)
                .map(|i| CheckpointDecision {
                    what: format!("decision {i}").repeat(5),
                    why: "because".repeat(5),
                })
                .collect(),
            files_changed: (0..CHECKPOINT_MAX_ITEMS)
                .map(|i| CheckpointFileChange {
                    path: format!("crates/some/very/long/path/file_{i}.rs"),
                    change: "modified".into(),
                    note: Some("a fairly long note about the change".repeat(3)),
                })
                .collect(),
            tests_run: (0..CHECKPOINT_MAX_ITEMS)
                .map(|i| CheckpointTestRun {
                    command: format!("cargo test -p crate_{i}"),
                    exit: Some(0),
                    summary: Some("passed".repeat(10)),
                })
                .collect(),
            known_failures: vec![],
            artifact_refs: vec![],
            next_action: "next".repeat(50),
            open_questions: vec![],
            plan_issue: None,
            repo_state: None,
            recent_activity: (0..CHECKPOINT_MAX_ITEMS)
                .map(|i| format!("Bash: cargo test -p crate_{i}").repeat(3))
                .collect(),
            created_at: "2026-09-24T00:00:00Z".into(),
        };
        truncate_checkpoint(&mut cp);
        let bytes = checkpoint_byte_len(&cp);
        assert!(bytes <= CHECKPOINT_MAX_BYTES, "got {bytes} bytes");
    }

    #[test]
    fn progress_definition_matches_d18() {
        let base = Checkpoint {
            schema: CHECKPOINT_SCHEMA.into(),
            task_id: "t".into(),
            work_unit: None,
            run_id: "r1".into(),
            run_seq: 1,
            end: CheckpointEnd::BudgetExhausted,
            source: CheckpointSource::Mechanical,
            completed: vec!["a".into()],
            remaining: vec!["b".into(), "c".into()],
            decisions: vec![],
            files_changed: vec![CheckpointFileChange {
                path: "a.rs".into(),
                change: "modified".into(),
                note: None,
            }],
            tests_run: vec![],
            known_failures: vec![],
            artifact_refs: vec![],
            next_action: "n".into(),
            open_questions: vec![],
            plan_issue: None,
            repo_state: Some(RepoState {
                branch: "b".into(),
                base: "base".into(),
                head: "h1".into(),
                uncommitted: true,
                diff_stat: "1 file".into(),
            }),
            recent_activity: vec![],
            created_at: "2026-09-24T00:00:00Z".into(),
        };
        // 最初の checkpoint は常に進捗あり。
        assert!(checkpoint_shows_progress(None, &base));

        let mut same = base.clone();
        same.run_id = "r2".into();
        assert!(
            !checkpoint_shows_progress(Some(&base), &same),
            "何も変わっていなければ進捗なし"
        );

        let mut new_file = base.clone();
        new_file.files_changed.push(CheckpointFileChange {
            path: "b.rs".into(),
            change: "added".into(),
            note: None,
        });
        assert!(checkpoint_shows_progress(Some(&base), &new_file));

        let mut new_head = base.clone();
        new_head.repo_state.as_mut().unwrap().head = "h2".into();
        assert!(checkpoint_shows_progress(Some(&base), &new_head));

        let mut more_completed = base.clone();
        more_completed.completed.push("d".into());
        assert!(checkpoint_shows_progress(Some(&base), &more_completed));

        let mut less_remaining = base.clone();
        less_remaining.remaining.pop();
        assert!(checkpoint_shows_progress(Some(&base), &less_remaining));
    }

    #[test]
    fn context_exceeded_phrases_are_recognized() {
        assert!(looks_like_context_exceeded(
            "Prompt is too long: 250000 tokens"
        ));
        assert!(looks_like_context_exceeded(
            "Error: context_length_exceeded"
        ));
        assert!(!looks_like_context_exceeded("wall clock exceeded"));
    }

    // ADR-0072 D16（Phase E4）: classify_review_failure の分類表。

    fn command(cmd: &str, reason: &str) -> FailedCheck {
        FailedCheck {
            check: crate::model::Check::Command {
                cmd: cmd.to_string(),
                expect_exit: 0,
            },
            reason: reason.to_string(),
            repair_hint: None,
        }
    }

    fn reviewer(reason: &str, hint: Option<(&str, &str)>) -> FailedCheck {
        FailedCheck {
            check: crate::model::Check::Reviewer,
            reason: reason.to_string(),
            repair_hint: hint.map(|(scope, class)| ReviewRepairHint {
                scope: scope.to_string(),
                class: class.to_string(),
            }),
        }
    }

    #[test]
    fn classifies_a_fmt_check_command_as_format() {
        let d = classify_review_failure(&[command(
            "cargo fmt --all -- --check",
            "cmd=\"cargo fmt --all -- --check\" exit=Some(1) expected=0",
        )]);
        assert_eq!(d, RepairDecision::Repairable(RepairClass::Format));
        assert_eq!(RepairClass::Format.bucket(), "format");
        assert_eq!(RepairClass::Format.budget(), (12, 600));
    }

    #[test]
    fn classifies_a_clippy_command_as_lint() {
        let d = classify_review_failure(&[command(
            "cargo clippy --workspace -- -D warnings",
            "cmd=... exit=Some(1)",
        )]);
        assert_eq!(d, RepairDecision::Repairable(RepairClass::Lint));
        assert_eq!(RepairClass::Lint.budget(), (20, 1200));
    }

    #[test]
    fn classifies_a_small_test_failure_as_test_small() {
        let d = classify_review_failure(&[command(
            "cargo test --workspace",
            "stdout_tail=\"test result: FAILED. 12 passed; 3 failed; 0 ignored\"",
        )]);
        assert_eq!(d, RepairDecision::Repairable(RepairClass::TestSmall));
        assert_eq!(RepairClass::TestSmall.budget(), (30, 1800));
    }

    #[test]
    fn a_large_test_failure_is_not_test_small() {
        let d = classify_review_failure(&[command(
            "cargo test --workspace",
            "stdout_tail=\"test result: FAILED. 2 passed; 40 failed; 0 ignored\"",
        )]);
        assert_eq!(d, RepairDecision::Substantive);
    }

    #[test]
    fn a_test_command_without_a_failed_count_is_substantive() {
        let d = classify_review_failure(&[command(
            "cargo test --workspace",
            "exec failed: no such file or directory",
        )]);
        assert_eq!(d, RepairDecision::Substantive);
    }

    #[test]
    fn classifies_an_unrelated_command_as_substantive() {
        let d = classify_review_failure(&[command("./scripts/deploy.sh", "exit=Some(1)")]);
        assert_eq!(d, RepairDecision::Substantive);
    }

    #[test]
    fn classifies_a_reviewer_repair_hint_as_reviewer_local() {
        let d = classify_review_failure(&[reviewer(
            "reviewer(r1): fmt is off",
            Some(("local", "format")),
        )]);
        assert_eq!(
            d,
            RepairDecision::Repairable(RepairClass::ReviewerLocal(ReviewerRepairKind::Format))
        );
        assert_eq!(
            RepairClass::ReviewerLocal(ReviewerRepairKind::Format).bucket(),
            "reviewer_local"
        );
    }

    #[test]
    fn classifies_a_reviewer_reason_lexically_when_no_hint_is_given() {
        let d = classify_review_failure(&[reviewer(
            "reviewer(r1): please run cargo fmt before merging",
            None,
        )]);
        assert_eq!(
            d,
            RepairDecision::Repairable(RepairClass::ReviewerLocal(ReviewerRepairKind::Format))
        );
    }

    #[test]
    fn a_reviewer_hint_with_a_non_local_scope_is_substantive() {
        let d = classify_review_failure(&[reviewer(
            "reviewer(r1): design is wrong",
            Some(("design", "other")),
        )]);
        assert_eq!(d, RepairDecision::Substantive);
    }

    #[test]
    fn a_human_check_failure_is_always_substantive() {
        let d = classify_review_failure(&[FailedCheck {
            check: crate::model::Check::Human,
            reason: "rejected".to_string(),
            repair_hint: None,
        }]);
        assert_eq!(d, RepairDecision::Substantive);
    }

    #[test]
    fn mixing_a_repairable_and_a_substantive_failure_is_substantive() {
        let d = classify_review_failure(&[
            command("cargo fmt --all -- --check", "stdout_tail=\"diff\""),
            FailedCheck {
                check: crate::model::Check::Human,
                reason: "needs a human call".to_string(),
                repair_hint: None,
            },
        ]);
        assert_eq!(d, RepairDecision::Substantive);
    }

    #[test]
    fn build_repair_objective_does_not_include_the_full_original_objective() {
        let long_objective = "x".repeat(3000);
        let obj = build_repair_objective(
            RepairClass::Format,
            &["cmd=\"cargo fmt --check\" exit=Some(1)".to_string()],
            "some task",
            &long_objective,
            Some("1 file changed"),
        );
        assert!(!obj.contains(&long_objective));
        assert!(obj.contains(&"x".repeat(600)));
        assert!(!obj.contains(&"x".repeat(601)));
        assert!(obj.contains("cargo fmt --check"));
        assert!(obj.contains("1 file changed"));
    }

    #[test]
    fn mixing_two_different_repairable_classes_is_substantive() {
        let d = classify_review_failure(&[
            command("cargo fmt --all -- --check", "diff"),
            command("cargo clippy -- -D warnings", "warning: ..."),
        ]);
        assert_eq!(d, RepairDecision::Substantive);
    }

    /// ADR-0072 D8 / ADR-0003 D6: 生成スキーマとコミット済みファイルの一致。`UPDATE_SCHEMA=1` で再生成。
    #[test]
    fn committed_schema_matches_generated() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/protocol/checkpoint.schema.json"
        );
        let generated = serde_json::to_string_pretty(&schema_value()).unwrap() + "\n";
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e} (run with UPDATE_SCHEMA=1 to generate)"));
        assert_eq!(
            committed, generated,
            "schema drift: run `UPDATE_SCHEMA=1 cargo test -p task-core`"
        );
    }
}
