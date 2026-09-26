//! 表示用のビュー型とその組み立て（ADR-0013 D7 / D12、`docs/gui/api.md` §3.3 / §3.5 / §5.2〜§5.4 / §6.2）。
//!
//! `celerisctl show --json`、API の `GET /tasks` / `GET /tasks/{id}` / `GET /tasks/{id}/runs` が同じ関数を使う。GUI は結果を表示するだけで
//! 再計算しない。I/O はストアの読み取りだけで、ファイル（`runs/<run_id>/` の存在確認など）は呼び出し側（task-api）が埋める。
//! 時刻は RFC 3339 の文字列。

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;
use task_core::{
    Check, Event, EventRow, ListFilter, ListOrder, RunRole, Status, Task, TaskId, TaskKind,
    TaskStore, Tier, Usage, WorkspaceSpec,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::derive::{self, AnswerNote, ReviewNote};
use crate::error::OpsError;

/// `event_rows_for` に渡す「実質無制限」の件数上限（タスク 1 件分の全イベントを読む用途）。
pub(crate) const ALL_EVENTS: usize = usize::MAX;

/// ビューの組み立てに必要な設定値（celeris の設定から呼び出し側が詰める）。
#[derive(Debug, Clone, PartialEq)]
pub struct ViewContext {
    /// `WorkspaceSpec::Local` の相対パスの基準。
    pub workspace_root: PathBuf,
    pub retry_backoff_base: Duration,
    pub retry_backoff_max: Duration,
    pub max_requeues: u32,
    /// ADR-0019 D2: `[[clusters]]` のうちビューに要る分（worktree のパスとブランチを出すため）。id → 設定。
    pub clusters: std::collections::HashMap<String, ClusterViewInfo>,
}

/// ADR-0019 D2 / ADR-0032 D1: `TaskDetail.worktree` を組み立てる（`sync` / `worktree_root`）のと、クラスタの
/// 認証方式（`auth`）を運ぶのに要るクラスタの設定。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClusterViewInfo {
    /// `"worktree"` のときだけ `TaskDetail.worktree` が出る（`"rsync"` / `"none"` では `null`）。
    pub sync: String,
    /// worktree を置く親ディレクトリ。`None` なら `<project>/.celeris-worktrees`。
    pub worktree_root: Option<PathBuf>,
    /// ADR-0032 D1: `"manual"` / `"publickey"` / `"totp"`（既定 `"manual"`）。
    pub auth: String,
}

/// worktree のブランチ名の接頭辞（ADR-0019 D2）。`task_worker::WorktreeSettings::default().branch_prefix` と同じ値。
pub const WORKTREE_BRANCH_PREFIX: &str = "celeris/";

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TaskRef {
    pub id: TaskId,
    pub title: String,
    pub kind: TaskKind,
    pub status: Status,
    /// 今この状態で許される操作（ADR-0015 D4。GUI は §5.4 の規則を再実装しない）。
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TaskSummary {
    pub id: TaskId,
    pub parent_id: Option<TaskId>,
    pub kind: TaskKind,
    pub status: Status,
    pub title: String,
    pub priority: i32,
    pub tier: Tier,
    pub adapter: Option<String>,
    pub attempts: u32,
    pub max_retries: u32,
    pub depends_on: Vec<TaskId>,
    pub created_at: String,
    pub updated_at: String,
    pub lease_expires_at: Option<String>,
    pub backoff_until: Option<String>,
    pub children: u32,
    pub pending_children: u32,
    /// ADR-0016 D1 の `Task.role`（GUI-R2: 一覧の各行に役割のラベルを出すため。`TaskDetail.role` と同じ値）。
    pub role: Option<String>,
    /// ADR-0027 D1 の `Task.genre`（`role` と同じ理由で一覧に出す。`TaskDetail.genre` と同じ値）。
    pub genre: Option<String>,
    /// ADR-0033 D2 の `Task.assignee`（組織のノード id。GUI-R3: 一覧に「誰の仕事か」を出すため）。
    pub assignee: Option<String>,
    /// 対話用タスク（人への返事のための run）か（GUI-R3: 仕事の木や一覧から隠せるように）。
    pub conversation: bool,
    /// GUI 監査 H4（Phase 29）: 裏方タスクの印（`"conversation"` | `"compaction"` | `"approval"` |
    /// `"review"` | `null`）。`task_core::support_kind` の決定的な判定。GUI はこれで仕事の木から裏方を外せる。
    pub support: Option<String>,
    /// 今この状態で許される操作（ADR-0015 D4）。
    pub actions: Vec<Action>,
    // ---- ADR-0044 D3/D4（Phase 53）: ボードのカードが要るもの。ここから ----
    /// ADR-0044 D3 の `Task.labels`。
    pub labels: Vec<String>,
    /// ADR-0044 D3 の `Task.category`。
    pub category: task_core::TaskCategory,
    /// ADR-0044 D3: `priority` を P0〜P3 に丸めたもの（`i32` は互換のため残す）。
    pub priority_label: String,
    /// ADR-0033 D2 の `Task.project_id`（ボードは案件で絞るので一覧にも出す）。
    pub project_id: Option<task_core::ProjectId>,
    /// ADR-0033 D2 の `Task.milestone_id`（カードに途中目標を出すため）。
    pub milestone_id: Option<task_core::MilestoneId>,
    // ---- ADR-0044 D3/D4（Phase 53）: ここまで ----
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TaskList {
    pub items: Vec<TaskSummary>,
    pub next_cursor: Option<String>,
    pub total: u64,
    /// status 名 → 件数（フィルタに関係なく DB 全体。0 件の status は現れない）。
    pub counts_by_status: BTreeMap<String, u64>,
}

/// `celerisctl show --json` と `GET /api/v1/tasks/{id}` の本体。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TaskDetail {
    pub task: Task,
    /// 手元の作業ディレクトリ（絶対パス）。`WorkspaceSpec::Remote` では写し `workspace_root/<task_id>`（run のログはここ。ADR-0018 D1）。
    pub workspace_dir: Option<String>,
    /// ADR-0018: `WorkspaceSpec::Remote` のクラスタ（`[[clusters]] id`）。ローカルのタスクは `null`。
    pub cluster: Option<String>,
    /// ADR-0016 D1: `Task.role`（GUI の表示用に最上位にも出す）。
    pub role: Option<String>,
    /// ADR-0027 D1: `Task.genre`（`role` と同じ理由で最上位にも出す）。
    pub genre: Option<String>,
    /// ADR-0044 D3（Phase 53）: `task.priority` を P0〜P3 に丸めたもの（GUI の編集フォーム用）。
    pub priority_label: String,
    /// ADR-0016 D2: 各 run が `delegate` で作った子（`Event::Delegated` の順）。
    pub delegated: Vec<DelegatedView>,
    pub timers: Timers,
    pub criteria: Vec<CriterionView>,
    pub runs: Vec<RunSummary>,
    pub prior_review: Vec<ReviewNote>,
    pub answers: Vec<AnswerNote>,
    pub latest_question: Option<String>,
    pub approvals: Vec<ApprovalLink>,
    pub dependencies: Vec<TaskRef>,
    pub dependents: Vec<TaskRef>,
    pub children: Vec<TaskRef>,
    pub actions: Vec<Action>,
    pub worker_run_hint: Option<String>,
    /// ADR-0019 D2: `sync = "worktree"` のクラスタで動くタスクの worktree。人はここを見て diff / commit する。
    pub worktree: Option<WorktreeView>,
    /// ADR-0070 D1（Phase 116）: `task.status == Failed` のときだけ `Some`（分類・理由・配送済みの release）。
    /// GUI のタスク詳細の赤いバナーの材料。
    pub failure: Option<FailureSummary>,
    /// ADR-0072 D20（Phase E5）: Execution 節。events に E-phase 由来の活動（gate の判定・計画・
    /// checkpoint・WorkUnit の遷移）が 1 件も無ければ `None`（D23: 既存の古いタスクの詳細を壊さない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionView>,
}

/// ADR-0072 D20（Phase E5）: タスクの状態バッジの横に出す、今どの段階かの導出値（D6 の R3 の代替。
/// 状態機械そのものには足さない）。`Task.status` から次のとおり決める（[`build_execution_view`] 参照）:
/// `reviewing` は常に `verifying`。`running` は、進行中の WorkUnit があれば `repairing`
/// （`kind = repair`）か `executing`、無ければ（計画はあるのに走っている WU が無い）planner run が
/// 動いていると見なして `planning`。それ以外（計画が無い・終端）は `None`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Planning,
    Executing,
    Repairing,
    Verifying,
}

/// D19/D20: タスク詳細の Execution 節そのもの。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ExecutionView {
    /// D13: Complexity Gate の判定（gate が判定していない Task には無い）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<task_core::ExecutionGateDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<ExecutionPhase>,
    /// 計画が無い Task（D20:「直接実行」の 1 行）は `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<ExecutionPlanOverview>,
    pub metrics: task_core::ExecutionMetrics,
}

/// D20: 計画の概要（現在アクティブでない Task でも、生涯で作った WU をまとめて見せる。
/// `versions` が replan の履歴）。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ExecutionPlanOverview {
    pub id: String,
    pub version: u32,
    pub origin: task_core::PlanOrigin,
    pub rationale: String,
    pub work_units: Vec<ExecutionWorkUnitView>,
    /// D17(f): 版の履歴（`version` 昇順。superseded を含む）。
    pub versions: Vec<ExecutionPlanVersionSummary>,
    /// ADR-0074 D1.1（Phase F2b）: v2 の工程（配列の順が実行順。v1 は空）。GUI は WU の表を工程ごとの
    /// 見出しでまとめる。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<task_core::PhaseSpec>,
    /// ADR-0074 D1.2（Phase F2b）: v2 の計画を並列 1 に倒した理由（`WorkUnitsSerialized`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serialized_reason: Option<String>,
}

/// D20: WU の表の 1 行。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ExecutionWorkUnitView {
    pub id: String,
    pub key: String,
    pub seq: u32,
    pub kind: task_core::WorkUnitKind,
    pub title: String,
    pub status: task_core::WorkUnitStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<task_core::WorkUnitBlockedReason>,
    pub depends_on: Vec<String>,
    /// D21: WU は Task の担当を継ぐ（`Task.assignee` と同じ値）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// 直近の run の routing（`RoutingDecided`）から。まだ 1 度も走っていなければ `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<Tier>,
    pub runs: u32,
    pub continuations: u32,
    pub retries: u32,
    /// 最後の checkpoint の全文（GUI は折り畳んで出す。D8）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checkpoint: Option<task_core::Checkpoint>,
    /// 直近の `WorkUnitTransitioned.reason`（失敗・レビューの理由。無ければ `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// ADR-0074 D1（Phase F2b）: v2 の工程の key（v1 は無し）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// ADR-0074 D1.2: WU のブランチ（`celeris-wu/<task_id>/<key>`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// ADR-0074 D1.2: `WorkUnitCommitted` の commit。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit: Option<String>,
    /// ADR-0074 D1.4: 統合 WU の統合後の Task ブランチの HEAD。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrated_commit: Option<String>,
    /// ADR-0074 D1.5: 今この WU を実行している run（同時に走っている run を GUI に出す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_run_id: Option<String>,
}

/// D17(f): `execution_plans` の 1 版（監査用）。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ExecutionPlanVersionSummary {
    pub id: String,
    pub version: u32,
    pub origin: task_core::PlanOrigin,
    pub status: task_core::PlanStatus,
    /// `Event::ExecutionPlanned.reason`（replan を起こした理由。新規採用なら `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<String>,
}

/// ADR-0070 D1（Phase 116）: `TaskDetail.failure` / 受信箱 `AttentionItem::Failed` が共有する形。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FailureSummary {
    pub class: derive::FailureClass,
    /// 人が読む理由 1 行（`derive::classify_task_failure` が作る）。
    pub reason: String,
    /// 配送済み（`deliveries` に `release` が付いた記録がある）なら sha12。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_release: Option<String>,
}

/// ADR-0019 D2: クラスタ側の worktree（celeris はここだけを触り、commit はしない）。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct WorktreeView {
    /// 元のリポジトリ（`WorkspaceSpec::Remote.path`）。
    pub project: String,
    /// worktree のパス（クラスタ上）。
    pub dir: String,
    /// worktree のブランチ（`celeris/<task_id>`）。celeris は commit しないので、変更は作業ツリーに残る。
    pub branch: String,
}

/// ADR-0016 D2: 1 回の `delegate`（`Event::Delegated`）の要約。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct DelegatedView {
    pub run_id: String,
    /// イベントの ts（`EventRow.ts`）。
    pub ts: String,
    /// 子の現在の状態。既に存在しない ID は落とす。
    pub tasks: Vec<TaskRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Timers {
    pub now: String,
    pub lease_expires_at: Option<String>,
    pub backoff_until: Option<String>,
    pub consecutive_requeues: u32,
    pub max_requeues: u32,
    pub consecutive_reviewer_requeues: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CriterionView {
    pub idx: usize,
    pub text: String,
    pub check: Check,
    pub latest_verdict: Option<VerdictView>,
    pub approval: Option<ApprovalLink>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct VerdictView {
    pub run_id: String,
    pub criterion_idx: usize,
    pub pass: bool,
    pub reason: String,
    pub ts: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RunSummary {
    pub run_id: String,
    /// ワーカー run か Reviewer run か（ADR-0014 D1。イベントに `role` が無ければ `worker`）。
    pub role: RunRole,
    pub adapter: String,
    pub model: String,
    pub provider: Option<String>,
    /// プールのアカウント（ADR-0024 D4 / ADR-0025）。`WorkerStarted.account` がある run だけ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: Option<RunOutcomeKind>,
    pub outcome_text: Option<String>,
    /// ADR-0072 D19/D20（Phase E1）: この run の構造化した終わり方（`WorkerFinished.end`）。
    /// 導入前の run・分類できなかった run は `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<task_core::RunEnd>,
    /// ADR-0072 D20（Phase E5）: この run が実行した WorkUnit の `key`（計画の無い Task、または
    /// `work_units`/`runs` の索引に無い導入前の run は `None`）。`runs()` 自体は events だけの
    /// 純粋関数なので、[`run_work_unit_keys`] で store から引いた後段が埋める。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit: Option<String>,
    pub usage: Option<Usage>,
    pub progress: u32,
    pub artifacts: u32,
    pub verdicts: u32,
    pub reviewer_deferrals: u32,
    /// `runs/<run_id>/` のファイルの有無。task-ops は `None` を入れ、task-api が埋める。
    pub files: Option<RunFiles>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RunFiles {
    pub stdout: bool,
    pub stderr: bool,
    pub result: bool,
    /// ADR-0023 D2: `runs/<run_id>/request.json`（ワーカーに渡した指示）。導入前の run には無いので既定は false。
    #[serde(default)]
    pub request: bool,
    /// ADR-0023 M1: `runs/<run_id>/prompt.txt`（claude-code / codex が実際に渡した文面）。fake には無い。
    #[serde(default)]
    pub prompt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcomeKind {
    Done,
    Question,
    Error,
    Requeue,
    LeaseExpired,
    /// ADR-0044 D2/D8（Phase 53）: 人のコメントで止めた run（`interrupted: comment`）。
    /// **失敗ではない**ので `bad_news` にも `error_cooldown` にも数えない。
    Interrupted,
    /// ADR-0072 D9/D11（Phase E1）: 予算切れ・yield の続き（`Trigger::Continue`）。**失敗ではない**
    /// （checkpoint から新しい run が続く。`Interrupted` と同じく集計には数えない）。
    Continued,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ApprovalLink {
    pub approval: TaskRef,
    pub criterion_idx: Option<usize>,
    pub attempt: Option<u32>,
    pub decided: Option<ApprovalDecisionView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ApprovalDecisionView {
    pub by: String,
    pub approved: bool,
    pub note: Option<String>,
    pub ts: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Approve,
    Reject,
    Answer,
    Cancel,
    /// Phase 31（実機の事故、2026-09-18）: `failed`/`cancelled` を複製してやり直す（`POST /tasks/{id}/retry`）。
    Retry,
    /// ADR-0044 D1（Phase 53）: 人が編集できる（`PATCH /tasks/{id}`。終端でないタスクだけ）。
    Edit,
    /// ADR-0044 D2（Phase 53）: 終端のタスクを同じ worktree のまま再開する（`POST /tasks/{id}/reopen`。
    /// `done` / `failed` だけ。`cancelled` は worktree を消してあるので `Retry` を使う）。
    Reopen,
    /// ADR-0070 D2（Phase 116）: 既存成果の再判定（`POST /tasks/{id}/rereview`。ADR-0051 /
    /// ADR-0054 Phase 113 D3）。`done`、または直前の遷移が `review_fail` だった `failed` だけ
    /// （`task_ops::comment::can_rereview` と同じ規則。events を要るので `actions(task)` 単体では
    /// 判定できず、`task_detail` / 受信箱の `build_attention` が events を渡して個別に足す）。
    Rereview,
}

pub fn task_ref(task: &Task) -> TaskRef {
    TaskRef {
        id: task.id,
        title: task.title.clone(),
        kind: task.kind,
        status: task.status,
        actions: actions(task),
    }
}

/// `docs/gui/api.md` §5.4: 今この状態で許される操作。
pub fn actions(task: &Task) -> Vec<Action> {
    let mut out = Vec::new();
    if task.status == Status::Draft
        || (task.kind == TaskKind::Approval && task.status == Status::Ready)
    {
        out.push(Action::Approve);
    }
    if task.kind == TaskKind::Approval && task.status == Status::Ready {
        out.push(Action::Reject);
    }
    if task.status == Status::Blocked {
        out.push(Action::Answer);
    }
    if !task.status.is_terminal() {
        out.push(Action::Cancel);
    }
    if matches!(task.status, Status::Failed | Status::Cancelled) {
        out.push(Action::Retry);
    }
    // ADR-0044 D1/D2（Phase 53）: 編集は終端でないタスク、再開は `done`/`failed` だけ。
    if !task.status.is_terminal() {
        out.push(Action::Edit);
    }
    if matches!(task.status, Status::Done | Status::Failed) {
        out.push(Action::Reopen);
    }
    out
}

/// ADR-0070 D2（Phase 116）: [`actions`] に `Action::Rereview` を足したもの（events が要るので
/// 別関数にした。`task_detail` と受信箱の `build_attention` が、それぞれ既に読んでいる events を
/// 渡して使う）。
pub fn actions_with_events(task: &Task, events: &[(u64, Event)]) -> Vec<Action> {
    let mut out = actions(task);
    if crate::comment::can_rereview(task, events) {
        out.push(Action::Rereview);
    }
    out
}

/// RFC 3339 文字列に整形する。`OffsetDateTime` の書式化が失敗することは実質無い想定だが、
/// パニックはしない（`Debug` 表現にフォールバックする）。
pub(crate) fn to_rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_else(|_| format!("{t:?}"))
}

fn std_duration_to_time_duration(d: Duration) -> time::Duration {
    time::Duration::new(d.as_secs() as i64, d.subsec_nanos() as i32)
}

/// `Status` の serde 表現（snake_case）。`counts_by_status` のキーに使う。
pub(crate) fn status_key(status: Status) -> &'static str {
    match status {
        Status::Draft => "draft",
        Status::Ready => "ready",
        Status::Running => "running",
        Status::Blocked => "blocked",
        Status::Reviewing => "reviewing",
        Status::Done => "done",
        Status::Failed => "failed",
        Status::Cancelled => "cancelled",
    }
}

/// `derive` の各関数（`(seq, Event)` の組を期待する）に渡すための写像。
pub(crate) fn seq_pairs(rows: &[EventRow]) -> Vec<(u64, Event)> {
    rows.iter().map(|r| (r.seq, r.event.clone())).collect()
}

fn lease_expires_at_str(task: &Task) -> Option<String> {
    if task.status != Status::Running {
        return None;
    }
    task.lease.as_ref().map(|l| to_rfc3339(l.expires_at))
}

/// `docs/gui/api.md` §5.3: `ready && attempts > 0` のときの `updated_at + retry_backoff(...)`。
/// `base == 0`、または結果が過去なら `None`。
fn backoff_until_str(task: &Task, ctx: &ViewContext, now: OffsetDateTime) -> Option<String> {
    if task.status != Status::Ready || task.attempts == 0 {
        return None;
    }
    let backoff =
        derive::retry_backoff(ctx.retry_backoff_base, ctx.retry_backoff_max, task.attempts);
    if backoff.is_zero() {
        return None;
    }
    let until = task.updated_at + std_duration_to_time_duration(backoff);
    if until > now {
        Some(to_rfc3339(until))
    } else {
        None
    }
}

/// `parent_id` ごとの `(children, pending_children)`（`pending` = 非終端）。一覧と受信箱で全件を 1 回だけ走査するために使う。
pub(crate) fn child_counts(all_tasks: &[Task]) -> HashMap<TaskId, (u32, u32)> {
    let mut counts: HashMap<TaskId, (u32, u32)> = HashMap::new();
    for t in all_tasks {
        if let Some(parent) = t.parent_id {
            let entry = counts.entry(parent).or_insert((0, 0));
            entry.0 += 1;
            if !t.status.is_terminal() {
                entry.1 += 1;
            }
        }
    }
    counts
}

pub(crate) fn build_task_summary(
    task: &Task,
    children: u32,
    pending_children: u32,
    ctx: &ViewContext,
    now: OffsetDateTime,
) -> TaskSummary {
    TaskSummary {
        id: task.id,
        parent_id: task.parent_id,
        kind: task.kind,
        status: task.status,
        title: task.title.clone(),
        priority: task.priority,
        tier: task.worker_hint.tier,
        adapter: task.worker_hint.adapter.clone(),
        attempts: task.attempts,
        max_retries: task.budget.max_retries,
        depends_on: task.depends_on.clone(),
        created_at: to_rfc3339(task.created_at),
        updated_at: to_rfc3339(task.updated_at),
        lease_expires_at: lease_expires_at_str(task),
        backoff_until: backoff_until_str(task, ctx, now),
        children,
        pending_children,
        role: task.role.clone(),
        genre: task.genre.clone(),
        assignee: task.assignee.clone(),
        conversation: task_core::is_conversation(task),
        support: task_core::support_kind(task).map(str::to_string),
        actions: actions(task),
        labels: task.labels.clone(),
        category: task.category,
        priority_label: task_core::priority_label(task.priority).to_string(),
        project_id: task.project_id,
        milestone_id: task.milestone_id,
    }
}

/// `outcome` 文字列を `RunOutcomeKind` に分類する（`docs/gui/api.md` §5.2）。`outcome_text` は
/// 接頭辞を除いた残りの文字列（`lease_expired` は完全一致で残りが無いので `None`。`error` は元の
/// 文字列全体を `outcome_text` に入れる。GUI が生の理由を表示できるようにするための判断）。
///
/// ADR-0072 D19（Phase E1）: `end`（`WorkerFinished.end`。構造化された分類）があれば、それを
/// 優先する。`end` が無い run（導入前・分類できなかった経路）は従来どおり字句判定にフォールバックする。
fn classify_outcome(
    outcome: &str,
    end: Option<&task_core::RunEnd>,
) -> (RunOutcomeKind, Option<String>) {
    if let Some(task_core::RunEnd::Yielded | task_core::RunEnd::BudgetExhausted { .. }) = end
        && let Some(text) = outcome.strip_prefix("continue: ")
    {
        // continuation した（`[execution] continuation = false` や上限到達で従来の `error(...)`/
        // `question: ...` に戻ったときは、その文字列どおりに分類する）。
        return (RunOutcomeKind::Continued, Some(text.to_string()));
    }
    if let Some(text) = outcome.strip_prefix("done: ") {
        (RunOutcomeKind::Done, Some(text.to_string()))
    } else if let Some(text) = outcome.strip_prefix("question: ") {
        (RunOutcomeKind::Question, Some(text.to_string()))
    } else if let Some(text) = outcome.strip_prefix("continue: ") {
        (RunOutcomeKind::Continued, Some(text.to_string()))
    } else if let Some(text) = outcome.strip_prefix("requeue: ") {
        (RunOutcomeKind::Requeue, Some(text.to_string()))
    } else if let Some(text) = outcome.strip_prefix("infra_requeue: ") {
        // ADR-0070 D3 / P-E0-3: インフラ都合の再試行も `Requeue` に数える（attempts を消費しない
        // 再試行という点で供給側の `requeue` と同じ性質）。
        (RunOutcomeKind::Requeue, Some(text.to_string()))
    } else if let Some(text) = outcome.strip_prefix("interrupted: ") {
        // ADR-0044 D2/D8: 人のコメントによる割り込み（失敗ではない）。
        (RunOutcomeKind::Interrupted, Some(text.to_string()))
    } else if outcome == "lease_expired" {
        (RunOutcomeKind::LeaseExpired, None)
    } else {
        (RunOutcomeKind::Error, Some(outcome.to_string()))
    }
}

/// `docs/gui/api.md` §5.2: そのタスクのイベント（`event_rows_for`）から run の要約を組み立てる。
pub fn runs(rows: &[EventRow]) -> Vec<RunSummary> {
    let mut order: Vec<String> = Vec::new();
    let mut by_run: HashMap<String, RunSummary> = HashMap::new();

    for row in rows {
        match &row.event {
            Event::WorkerStarted {
                run_id,
                adapter,
                model,
                provider,
                account,
                role,
                ..
            } => {
                if !by_run.contains_key(run_id) {
                    order.push(run_id.clone());
                }
                by_run.entry(run_id.clone()).or_insert_with(|| RunSummary {
                    run_id: run_id.clone(),
                    role: role.unwrap_or(RunRole::Worker),
                    adapter: adapter.clone(),
                    model: model.clone(),
                    provider: provider.clone(),
                    account: account.clone(),
                    started_at: row.ts.clone(),
                    finished_at: None,
                    outcome: None,
                    outcome_text: None,
                    end: None,
                    work_unit: None,
                    usage: None,
                    progress: 0,
                    artifacts: 0,
                    verdicts: 0,
                    reviewer_deferrals: 0,
                    files: None,
                });
            }
            Event::WorkerProgress { run_id, msg, .. } => {
                if let Some(r) = by_run.get_mut(run_id) {
                    r.progress += 1;
                    if msg.starts_with(derive::REVIEWER_REQUEUED_PREFIX) {
                        r.reviewer_deferrals += 1;
                    }
                }
            }
            Event::ArtifactProduced { run_id, .. } => {
                if let Some(r) = by_run.get_mut(run_id) {
                    r.artifacts += 1;
                }
            }
            Event::ReviewVerdict { run_id, .. } => {
                if let Some(r) = by_run.get_mut(run_id) {
                    r.verdicts += 1;
                }
            }
            Event::WorkerFinished {
                run_id,
                outcome,
                usage,
                end,
                ..
            } => {
                if let Some(r) = by_run.get_mut(run_id) {
                    r.finished_at = Some(row.ts.clone());
                    r.usage = *usage;
                    r.end = *end;
                    let (kind, text) = classify_outcome(outcome, end.as_ref());
                    r.outcome = Some(kind);
                    r.outcome_text = text;
                }
            }
            _ => {}
        }
    }

    let mut out: Vec<RunSummary> = order
        .into_iter()
        .filter_map(|id| by_run.remove(&id))
        .collect();
    out.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    out
}

/// ADR-0072 D20（Phase E5）: `run_id` → WorkUnit の `key`。`work_units`/`runs` の派生索引から
/// 引く（計画の無い Task なら空の map）。`runs()` の後段で `RunSummary.work_unit` を埋めるのに使う。
pub fn run_work_unit_keys(
    store: &dyn TaskStore,
    task_id: TaskId,
) -> Result<HashMap<String, String>, OpsError> {
    let units = store.work_units_for(task_id)?;
    if units.is_empty() {
        return Ok(HashMap::new());
    }
    let key_by_id: HashMap<&str, &str> = units
        .iter()
        .map(|u| (u.id.as_str(), u.key.as_str()))
        .collect();
    let runs = store.runs_for_task(task_id)?;
    Ok(runs
        .into_iter()
        .filter_map(|r| {
            let wu_id = r.work_unit_id?;
            let key = key_by_id.get(wu_id.as_str())?;
            Some((r.run_id, (*key).to_string()))
        })
        .collect())
}

/// `docs/gui/api.md` §5.3。
pub fn timers(task: &Task, rows: &[EventRow], ctx: &ViewContext, now: OffsetDateTime) -> Timers {
    let events = seq_pairs(rows);
    Timers {
        now: to_rfc3339(now),
        lease_expires_at: lease_expires_at_str(task),
        backoff_until: backoff_until_str(task, ctx, now),
        consecutive_requeues: derive::consecutive_requeues(&events),
        max_requeues: ctx.max_requeues,
        consecutive_reviewer_requeues: derive::consecutive_reviewer_requeues(&events),
    }
}

/// `docs/gui/api.md` §3.3。
pub fn task_summary(
    store: &dyn TaskStore,
    task: &Task,
    ctx: &ViewContext,
    now: OffsetDateTime,
) -> Result<TaskSummary, OpsError> {
    let (children, pending_children) = child_counts(&store.list(None)?)
        .get(&task.id)
        .copied()
        .unwrap_or((0, 0));
    Ok(build_task_summary(
        task,
        children,
        pending_children,
        ctx,
        now,
    ))
}

/// `docs/gui/api.md` §3.3: `list_page` の結果を `TaskSummary` に写し、`counts_by_status` を付ける。
pub fn task_list(
    store: &dyn TaskStore,
    filter: &ListFilter,
    order: ListOrder,
    cursor: Option<&str>,
    limit: usize,
    ctx: &ViewContext,
    now: OffsetDateTime,
) -> Result<TaskList, OpsError> {
    let page = store.list_page(filter, order, cursor, limit)?;

    // `children` / `pending_children` は `parent_id` で集計する。数千件を想定し、`list(None)` を
    // 1 回読んで全ページ分の項目に共通のカウントマップを使う（項目ごとに全件走査しない）。
    let child_counts = child_counts(&store.list(None)?);

    let items = page
        .items
        .iter()
        .map(|t| {
            let (children, pending_children) = child_counts.get(&t.id).copied().unwrap_or((0, 0));
            build_task_summary(t, children, pending_children, ctx, now)
        })
        .collect();

    let counts_by_status = store
        .count_by_status()?
        .into_iter()
        .map(|(s, n)| (status_key(s).to_string(), n))
        .collect();

    Ok(TaskList {
        items,
        next_cursor: page.next_cursor,
        total: page.total,
        counts_by_status,
    })
}

/// `docs/gui/api.md` §3.5。
pub fn task_detail(
    store: &dyn TaskStore,
    id: TaskId,
    ctx: &ViewContext,
    now: OffsetDateTime,
) -> Result<TaskDetail, OpsError> {
    let task = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    let rows = store.event_rows_for(id, None, ALL_EVENTS)?;
    let events = seq_pairs(&rows);

    let all_tasks = store.list(None)?;
    let by_id: HashMap<TaskId, Task> = all_tasks.iter().map(|t| (t.id, t.clone())).collect();

    let mut children_refs: Vec<TaskRef> = Vec::new();
    let mut approval_children: Vec<Task> = Vec::new();
    for t in &all_tasks {
        if t.parent_id == Some(id) {
            children_refs.push(task_ref(t));
            if t.kind == TaskKind::Approval {
                approval_children.push(t.clone());
            }
        }
    }
    children_refs.sort_by_key(|r| r.id);
    approval_children.sort_by_key(|t| t.id);

    // Human check の Approval 子。`parse_human_approval_title` で `(criterion_idx, attempt)` に対応付ける。
    let mut approvals: Vec<ApprovalLink> = Vec::with_capacity(approval_children.len());
    for child in &approval_children {
        let child_rows = store.event_rows_for(child.id, None, ALL_EVENTS)?;
        let decided = child_rows.iter().rev().find_map(|r| match &r.event {
            Event::ApprovalDecided { by, approved, note } => Some(ApprovalDecisionView {
                by: by.clone(),
                approved: *approved,
                note: note.clone(),
                ts: r.ts.clone(),
            }),
            _ => None,
        });
        let parsed = parse_human_approval_title(&child.title);
        approvals.push(ApprovalLink {
            approval: task_ref(child),
            criterion_idx: parsed.map(|(i, _)| i),
            attempt: parsed.map(|(_, a)| a),
            decided,
        });
    }

    let last_run = derive::last_run_id(&events);
    let criteria: Vec<CriterionView> = task
        .acceptance
        .iter()
        .enumerate()
        .map(|(idx, criterion)| {
            let latest_verdict = last_run.as_deref().and_then(|run_id| {
                rows.iter().rev().find_map(|r| match &r.event {
                    Event::ReviewVerdict {
                        run_id: rid,
                        criterion_idx,
                        pass,
                        reason,
                    } if rid == run_id && *criterion_idx == idx => Some(VerdictView {
                        run_id: rid.clone(),
                        criterion_idx: idx,
                        pass: *pass,
                        reason: reason.clone(),
                        ts: r.ts.clone(),
                    }),
                    _ => None,
                })
            });
            // 同じ criterion_idx の Approval 子が複数（再レビューで複数回）あれば、最新の attempt を使う。
            let approval = approvals
                .iter()
                .filter(|a| a.criterion_idx == Some(idx))
                .max_by_key(|a| a.attempt.unwrap_or(0))
                .cloned();
            CriterionView {
                idx,
                text: criterion.text.clone(),
                check: criterion.check.clone(),
                latest_verdict,
                approval,
            }
        })
        .collect();

    let dependencies: Vec<TaskRef> = task
        .depends_on
        .iter()
        .filter_map(|d| by_id.get(d).map(task_ref))
        .collect();
    let mut dependents: Vec<TaskRef> = all_tasks
        .iter()
        .filter(|t| t.depends_on.contains(&id))
        .map(task_ref)
        .collect();
    dependents.sort_by_key(|r| r.id);

    let mut run_summaries = runs(&rows);
    let wu_keys_by_run = run_work_unit_keys(store, id)?;
    for r in &mut run_summaries {
        r.work_unit = wu_keys_by_run.get(&r.run_id).cloned();
    }
    let timers_view = timers(&task, &rows, ctx, now);
    let prior_review = derive::prior_review_from_events(&events);
    let answers = derive::answers_from_events(&events);
    let latest_question_raw = derive::latest_question(&events);
    let latest_question = if latest_question_raw.is_empty() {
        None
    } else {
        Some(latest_question_raw)
    };

    // ADR-0041 D1: worktree を切ったタスクでは、run のログ・成果物は作業ツリーの外
    // （`<workspace_root>/<task_id>/`）にある。判定は `workspace::local_dir`（目印ファイルを見るだけ）。
    let workspace_dir = Some(
        crate::workspace::local_dir(&task, &ctx.workspace_root)
            .to_string_lossy()
            .into_owned(),
    );
    let cluster = match &task.workspace {
        WorkspaceSpec::Local { .. } => None,
        WorkspaceSpec::Remote { cluster, .. } => Some(cluster.clone()),
    };
    // ADR-0019 D2: `sync = "worktree"` のクラスタなら、worktree のパスとブランチを出す（人が diff / commit する場所）。
    let worktree = match &task.workspace {
        WorkspaceSpec::Remote { cluster, path, .. } => ctx
            .clusters
            .get(cluster)
            .filter(|c| c.sync == "worktree")
            .map(|c| {
                let root = c
                    .worktree_root
                    .clone()
                    .unwrap_or_else(|| path.join(".celeris-worktrees"));
                WorktreeView {
                    project: path.to_string_lossy().into_owned(),
                    dir: root
                        .join(task.id.to_string())
                        .to_string_lossy()
                        .into_owned(),
                    branch: format!("{WORKTREE_BRANCH_PREFIX}{}", task.id),
                }
            }),
        // ADR-0041 D1: ローカルも worktree を切る。目印（`worktree.json`）があればそれを出す。
        WorkspaceSpec::Local { path, .. } => {
            crate::workspace::read_marker(&ctx.workspace_root.join(task.id.to_string())).map(|m| {
                WorktreeView {
                    project: if m.repo.is_empty() {
                        path.to_string_lossy().into_owned()
                    } else {
                        m.repo
                    },
                    dir: m.dir,
                    branch: m.branch,
                }
            })
        }
    };

    let task_actions = actions_with_events(&task, &events);
    let failure = task_failure(&task, &events, store)?;
    let execution = build_execution_view(store, &task, &events)?;
    let worker_run_hint = if task.status.is_terminal() {
        None
    } else {
        Some(format!(
            "celerisctl worker run --config <config.toml> --task {id}"
        ))
    };

    let delegated: Vec<DelegatedView> = rows
        .iter()
        .filter_map(|r| match &r.event {
            Event::Delegated { run_id, task_ids } => Some(DelegatedView {
                run_id: run_id.clone(),
                ts: r.ts.clone(),
                tasks: task_ids
                    .iter()
                    .filter_map(|tid| by_id.get(tid).map(task_ref))
                    .collect(),
            }),
            _ => None,
        })
        .collect();
    let role = task.role.clone();
    let genre = task.genre.clone();
    let priority_label = task_core::priority_label(task.priority).to_string();

    Ok(TaskDetail {
        task,
        priority_label,
        workspace_dir,
        cluster,
        role,
        genre,
        delegated,
        timers: timers_view,
        criteria,
        runs: run_summaries,
        prior_review,
        answers,
        latest_question,
        approvals,
        dependencies,
        dependents,
        children: children_refs,
        actions: task_actions,
        worker_run_hint,
        worktree,
        failure,
        execution,
    })
}

/// ADR-0070 D1（Phase 116）: `task.status == Failed` のときだけ `Some`。分類・理由 1 行・
/// 配送済みなら release の sha12 を持つ。`deliveries` の読み取りは `TaskDetail` の他のフィールドと
/// 同じくストアから 1 回読むだけ（LLM は使わない）。
fn task_failure(
    task: &Task,
    events: &[(u64, Event)],
    store: &dyn TaskStore,
) -> Result<Option<FailureSummary>, OpsError> {
    if task.status != Status::Failed {
        return Ok(None);
    }
    let (class, reason) = derive::classify_task_failure(events);
    let delivered_release = store.delivery_get(task.id)?.and_then(|d| d.release.clone());
    Ok(Some(FailureSummary {
        class,
        reason,
        delivered_release,
    }))
}

/// ADR-0072 D19/D20（Phase E5）: Execution 節の組み立て。events に E-phase 由来の活動が 1 件も
/// 無ければ `None`（D23 の後方互換。既存の古いタスクの詳細を壊さない）。
fn build_execution_view(
    store: &dyn TaskStore,
    task: &Task,
    events: &[(u64, Event)],
) -> Result<Option<ExecutionView>, OpsError> {
    let has_activity = events.iter().any(|(_, e)| {
        matches!(
            e,
            Event::ExecutionGated { .. }
                | Event::ExecutionPlanned { .. }
                | Event::CheckpointSaved { .. }
                | Event::WorkUnitTransitioned { .. }
        )
    });
    if !has_activity {
        return Ok(None);
    }

    let event_list: Vec<Event> = events.iter().map(|(_, e)| e.clone()).collect();
    let metrics = task_core::summarize_execution_metrics(task, &event_list);
    let gate = task.routing.as_ref().and_then(|r| r.execution.clone());

    let all_units = store.work_units_for(task.id)?;
    let active_units: Vec<&task_core::WorkUnitRow> =
        all_units.iter().filter(|u| u.status.is_active()).collect();

    let phase = execution_phase(task, &active_units);

    let plan = if active_units.is_empty() {
        None
    } else {
        let audits = task_core::routing_audit(task, &event_list);
        let audit_by_run: HashMap<&str, &task_core::RoutingAudit> =
            audits.iter().map(|a| (a.run_id.as_str(), a)).collect();
        let mut work_units = Vec::with_capacity(active_units.len());
        for u in &active_units {
            let last_audit = u
                .last_run_id
                .as_deref()
                .and_then(|rid| audit_by_run.get(rid).copied());
            let last_checkpoint = match &u.last_checkpoint_run_id {
                Some(run_id) => store
                    .run_index_get(run_id)?
                    .and_then(|r| r.checkpoint.clone()),
                None => None,
            };
            let last_reason = events.iter().rev().find_map(|(_, e)| match e {
                Event::WorkUnitTransitioned {
                    work_unit_id,
                    reason,
                    ..
                } if *work_unit_id == u.id => Some(reason.clone()),
                _ => None,
            });
            work_units.push(ExecutionWorkUnitView {
                id: u.id.clone(),
                key: u.key.clone(),
                seq: u.seq,
                kind: u.kind,
                title: u.spec.title.clone(),
                status: u.status,
                blocked_reason: u.blocked_reason,
                depends_on: u.depends_on.clone(),
                assignee: task.assignee.clone(),
                harness: u.spec.harness.clone().or_else(|| task.genre.clone()),
                model: last_audit.and_then(|a| a.model.clone()),
                lane: last_audit.and_then(|a| a.lane),
                runs: u.runs,
                continuations: u.continuations,
                retries: u.retries,
                last_checkpoint,
                last_reason,
                created_at: u.created_at.clone(),
                updated_at: u.updated_at.clone(),
                phase: u.phase.clone(),
                branch: u.branch.clone(),
                head_commit: u.head_commit.clone(),
                integrated_commit: u.integrated_commit.clone(),
                running_run_id: if u.status == task_core::WorkUnitStatus::Running {
                    u.lease_run_id.clone()
                } else {
                    None
                },
            });
        }
        work_units.sort_by_key(|w| w.seq);

        let versions = store
            .execution_plan_list(task.id)?
            .into_iter()
            .map(|row| {
                let reason = event_list.iter().find_map(|e| match e {
                    Event::ExecutionPlanned {
                        plan_id, reason, ..
                    } if *plan_id == row.id => reason.clone(),
                    _ => None,
                });
                ExecutionPlanVersionSummary {
                    id: row.id,
                    version: row.version,
                    origin: row.origin,
                    status: row.status,
                    reason,
                    created_at: row.created_at,
                    superseded_at: row.superseded_at,
                }
            })
            .collect();

        let active_plan_row = store.execution_plan_active(task.id)?;
        let serialized_reason_of = |plan_id: &str| {
            event_list.iter().rev().find_map(|e| match e {
                Event::WorkUnitsSerialized { plan_id: p, reason } if p == plan_id => {
                    Some(reason.clone())
                }
                _ => None,
            })
        };
        match active_plan_row {
            Some(row) => {
                let serialized_reason = serialized_reason_of(&row.id);
                Some(ExecutionPlanOverview {
                    id: row.id,
                    version: row.version,
                    origin: row.origin,
                    rationale: row.spec.rationale,
                    work_units,
                    versions,
                    phases: row.spec.phases,
                    serialized_reason,
                })
            }
            None => {
                let latest = versions.last().cloned();
                latest.map(|latest| ExecutionPlanOverview {
                    serialized_reason: serialized_reason_of(&latest.id),
                    id: latest.id,
                    version: latest.version,
                    origin: latest.origin,
                    rationale: String::new(),
                    work_units,
                    versions,
                    phases: Vec::new(),
                })
            }
        }
    };

    Ok(Some(ExecutionView {
        gate,
        phase,
        plan,
        metrics,
    }))
}

/// D20: 今どの段階か（[`ExecutionPhase`] のドキュメント参照）。
fn execution_phase(
    task: &Task,
    active_units: &[&task_core::WorkUnitRow],
) -> Option<ExecutionPhase> {
    match task.status {
        Status::Reviewing => Some(ExecutionPhase::Verifying),
        Status::Running => {
            if active_units.is_empty() {
                None
            } else if let Some(u) = active_units
                .iter()
                .find(|u| u.status == task_core::WorkUnitStatus::Running)
            {
                if u.kind == task_core::WorkUnitKind::Repair {
                    Some(ExecutionPhase::Repairing)
                } else {
                    Some(ExecutionPhase::Executing)
                }
            } else {
                Some(ExecutionPhase::Planning)
            }
        }
        _ => None,
    }
}

/// `Approval needed: <title> — criterion <idx> (attempt <n>)`（`derive::human_approval_title` の書式）を解析して
/// `(criterion_idx, attempt)` を返す。書式に合わなければ `None`。
///
/// `<title>` 自体に ` — criterion ` を含む可能性があるため、マーカーは**末尾から**（`rfind`）探す
/// （実際の構造上のマーカーは常に最後に現れるものになる）。
pub fn parse_human_approval_title(title: &str) -> Option<(usize, u32)> {
    const PREFIX: &str = "Approval needed: ";
    const MARKER: &str = " — criterion ";

    if !title.starts_with(PREFIX) {
        return None;
    }
    let marker_pos = title.rfind(MARKER)?;
    let rest = &title[marker_pos + MARKER.len()..];
    let (idx_str, remainder) = rest.split_once(" (attempt ")?;
    let attempt_str = remainder.strip_suffix(')')?;
    let idx = idx_str.parse::<usize>().ok()?;
    let attempt = attempt_str.parse::<u32>().ok()?;
    Some((idx, attempt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;
    use task_core::{ArtifactRef, Budget, Criterion, Lease, SqliteStore, WorkerHint};

    fn view_ctx() -> ViewContext {
        ViewContext {
            workspace_root: PathBuf::from("/tmp/workspaces"),
            retry_backoff_base: StdDuration::from_secs(10),
            retry_backoff_max: StdDuration::from_secs(300),
            max_requeues: 5,
            clusters: Default::default(),
        }
    }

    fn sample_task(kind: TaskKind, status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind,
            title: "do something".to_string(),
            objective: "make it work".to_string(),
            acceptance: vec![Criterion {
                text: "tests pass".to_string(),
                check: Check::Command {
                    cmd: "true".to_string(),
                    expect_exit: 0,
                },
            }],
            inputs: vec![ArtifactRef {
                name: "spec".to_string(),
                path: "spec.md".to_string(),
                sha256: "abc".to_string(),
                kind: "doc".to_string(),
                declared: true,
            }],
            depends_on: vec![],
            status,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "workspace".into(),
                mode: None,
            },
            budget: Budget {
                max_turns: 10,
                max_wall_secs: 600,
                max_retries: 2,
            },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    fn row(seq: u64, ts: &str, task_id: TaskId, event: Event) -> EventRow {
        EventRow {
            id: seq,
            task_id,
            seq,
            ts: ts.to_string(),
            event,
        }
    }

    fn artifact(name: &str) -> ArtifactRef {
        ArtifactRef {
            name: name.to_string(),
            path: format!("artifacts/{name}"),
            sha256: "abc".to_string(),
            kind: "doc".to_string(),
            declared: true,
        }
    }

    /// プールの run（ADR-0024 D4）: `WorkerStarted.account` が要約に出る。
    #[test]
    fn runs_expose_the_pool_account_of_the_run() {
        let tid = TaskId::new();
        let pooled = vec![row(
            0,
            "t0",
            tid,
            started_with_account("r1", Some("claude-pool"), Some("acct-a")),
        )];
        let summaries = runs(&pooled);
        assert_eq!(summaries[0].account.as_deref(), Some("acct-a"));
        assert_eq!(summaries[0].provider.as_deref(), Some("claude-pool"));

        // プールでない run は None のまま（既存の形）。
        let plain = vec![row(0, "t0", tid, started("r2", Some("claude-a")))];
        assert!(runs(&plain)[0].account.is_none());
    }

    fn started_with_account(run_id: &str, provider: Option<&str>, account: Option<&str>) -> Event {
        let Event::WorkerStarted {
            run_id,
            adapter,
            model,
            provider,
            role,
            task_role,
            ..
        } = started(run_id, provider)
        else {
            unreachable!("started builds a WorkerStarted")
        };
        Event::WorkerStarted {
            run_id,
            adapter,
            model,
            provider,
            account: account.map(str::to_string),
            role,
            task_role,
        }
    }

    fn started(run_id: &str, provider: Option<&str>) -> Event {
        Event::WorkerStarted {
            run_id: run_id.to_string(),
            adapter: "claude-code".to_string(),
            model: "claude-sonnet-5".to_string(),
            provider: provider.map(str::to_string),
            account: None,
            role: None,
            task_role: None,
        }
    }

    fn finished(run_id: &str, outcome: &str) -> Event {
        Event::WorkerFinished {
            run_id: run_id.to_string(),
            outcome: outcome.to_string(),
            usage: None,
            role: None,
            metrics: None,
            end: None,
        }
    }

    // ---- runs ----

    /// ADR-0014 D1: Reviewer run も一覧に現れ、`role` で区別される（イベントに `role` が無ければ worker）。
    #[test]
    fn runs_include_reviewer_runs_with_role() {
        let task_id = TaskId::new();
        let events = vec![
            started("run-1", Some("acct-a")),
            finished("run-1", "done: implemented"),
            Event::WorkerStarted {
                run_id: "rev-1".into(),
                adapter: "claude-code".into(),
                model: "m".into(),
                provider: Some("acct-b".into()),
                account: None,
                role: Some(RunRole::Reviewer),
                task_role: None,
            },
            Event::WorkerFinished {
                run_id: "rev-1".into(),
                outcome: "done: reviewed".into(),
                usage: Some(Usage {
                    input_tokens: Some(5),
                    output_tokens: Some(7),
                    cache_read_tokens: None,
                    cache_creation_tokens: None,
                    cost_usd: None,
                }),
                role: Some(RunRole::Reviewer),
                metrics: None,
                end: None,
            },
        ];
        let rows: Vec<EventRow> = events
            .into_iter()
            .enumerate()
            .map(|(i, event)| EventRow {
                id: i as u64 + 1,
                task_id,
                seq: i as u64,
                ts: format!("2026-09-14T00:00:0{i}Z"),
                event,
            })
            .collect();
        let runs = runs(&rows);
        assert_eq!(runs.len(), 2);
        assert_eq!(
            (runs[0].run_id.as_str(), runs[0].role),
            ("run-1", RunRole::Worker)
        );
        assert_eq!(
            (runs[1].run_id.as_str(), runs[1].role),
            ("rev-1", RunRole::Reviewer)
        );
        assert_eq!(runs[1].provider.as_deref(), Some("acct-b"));
        assert_eq!(
            (runs[1].outcome, runs[1].outcome_text.as_deref()),
            (Some(RunOutcomeKind::Done), Some("reviewed"))
        );
        assert_eq!(runs[1].usage.and_then(|u| u.input_tokens), Some(5));
    }

    #[test]
    fn runs_classifies_done_outcome_with_provider_and_outcome_text() {
        let tid = TaskId::new();
        let rows = vec![
            row(
                0,
                "2024-01-01T00:00:00Z",
                tid,
                started("r1", Some("claude-a")),
            ),
            row(
                1,
                "2024-01-01T00:00:05Z",
                tid,
                finished("r1", "done: all good"),
            ),
        ];
        let summaries = runs(&rows);
        assert_eq!(summaries.len(), 1);
        let s = &summaries[0];
        assert_eq!(s.run_id, "r1");
        assert_eq!(s.provider.as_deref(), Some("claude-a"));
        assert_eq!(s.outcome, Some(RunOutcomeKind::Done));
        assert_eq!(s.outcome_text.as_deref(), Some("all good"));
        assert_eq!(s.finished_at.as_deref(), Some("2024-01-01T00:00:05Z"));
        assert_eq!(s.started_at, "2024-01-01T00:00:00Z");
        assert!(s.files.is_none());
    }

    #[test]
    fn runs_classifies_question_outcome() {
        let tid = TaskId::new();
        let rows = vec![
            row(0, "t0", tid, started("r1", None)),
            row(1, "t1", tid, finished("r1", "question: which?")),
        ];
        let s = &runs(&rows)[0];
        assert_eq!(s.outcome, Some(RunOutcomeKind::Question));
        assert_eq!(s.outcome_text.as_deref(), Some("which?"));
        assert!(s.provider.is_none());
    }

    #[test]
    fn runs_classifies_requeue_outcome() {
        let tid = TaskId::new();
        let rows = vec![
            row(0, "t0", tid, started("r1", None)),
            row(1, "t1", tid, finished("r1", "requeue: adapter: throttled")),
        ];
        let s = &runs(&rows)[0];
        assert_eq!(s.outcome, Some(RunOutcomeKind::Requeue));
        assert_eq!(s.outcome_text.as_deref(), Some("adapter: throttled"));
    }

    #[test]
    fn runs_classifies_lease_expired_as_exact_match() {
        let tid = TaskId::new();
        let rows = vec![
            row(0, "t0", tid, started("r1", None)),
            row(1, "t1", tid, finished("r1", "lease_expired")),
        ];
        let s = &runs(&rows)[0];
        assert_eq!(s.outcome, Some(RunOutcomeKind::LeaseExpired));
        assert!(s.outcome_text.is_none());
    }

    #[test]
    fn runs_classifies_anything_else_as_error() {
        let tid = TaskId::new();
        let rows = vec![
            row(0, "t0", tid, started("r1", None)),
            row(1, "t1", tid, finished("r1", "error(retryable=true): boom")),
        ];
        let s = &runs(&rows)[0];
        assert_eq!(s.outcome, Some(RunOutcomeKind::Error));
    }

    #[test]
    fn runs_in_progress_run_has_no_finished_at_or_outcome() {
        let tid = TaskId::new();
        let rows = vec![row(0, "t0", tid, started("r1", None))];
        let s = &runs(&rows)[0];
        assert!(s.finished_at.is_none());
        assert!(s.outcome.is_none());
    }

    #[test]
    fn runs_counts_progress_artifacts_verdicts_and_reviewer_deferrals() {
        let tid = TaskId::new();
        let rows = vec![
            row(0, "t0", tid, started("r1", None)),
            row(1, "t1", tid, Event::worker_progress("r1", "chugging along")),
            row(
                2,
                "t2",
                tid,
                Event::worker_progress(
                    "r1",
                    format!("{}throttled", derive::REVIEWER_REQUEUED_PREFIX),
                ),
            ),
            row(
                3,
                "t3",
                tid,
                Event::ArtifactProduced {
                    run_id: "r1".into(),
                    artifact: artifact("a"),
                },
            ),
            row(
                4,
                "t4",
                tid,
                Event::ReviewVerdict {
                    run_id: "r1".into(),
                    criterion_idx: 0,
                    pass: true,
                    reason: "ok".into(),
                },
            ),
            row(5, "t5", tid, finished("r1", "done: x")),
        ];
        let s = &runs(&rows)[0];
        assert_eq!(s.progress, 2);
        assert_eq!(s.artifacts, 1);
        assert_eq!(s.verdicts, 1);
        assert_eq!(s.reviewer_deferrals, 1);
    }

    #[test]
    fn runs_are_sorted_by_started_at_ascending_regardless_of_input_order() {
        let tid = TaskId::new();
        let rows = vec![
            row(0, "2024-01-02T00:00:00Z", tid, started("later", None)),
            row(1, "2024-01-01T00:00:00Z", tid, started("earlier", None)),
        ];
        let s = runs(&rows);
        assert_eq!(s[0].run_id, "earlier");
        assert_eq!(s[1].run_id, "later");
    }

    // ---- timers ----

    #[test]
    fn timers_reports_lease_expires_at_when_running() {
        let mut task = sample_task(TaskKind::Execute, Status::Running);
        let expires_at = OffsetDateTime::now_utc() + time::Duration::seconds(60);
        task.lease = Some(Lease {
            worker_run_id: "run-1".to_string(),
            expires_at,
        });
        let ctx = view_ctx();
        let t = timers(&task, &[], &ctx, OffsetDateTime::now_utc());
        assert_eq!(t.lease_expires_at, Some(to_rfc3339(expires_at)));
        // `timers.now` はスケルトンの `to_string()` ではなく RFC 3339 でなければならない。
        assert!(OffsetDateTime::parse(&t.now, &Rfc3339).is_ok());
    }

    #[test]
    fn timers_lease_expires_at_is_none_when_not_running() {
        let task = sample_task(TaskKind::Execute, Status::Ready);
        let ctx = view_ctx();
        let t = timers(&task, &[], &ctx, OffsetDateTime::now_utc());
        assert!(t.lease_expires_at.is_none());
    }

    #[test]
    fn timers_backoff_until_set_for_ready_task_with_attempts() {
        let now = OffsetDateTime::now_utc();
        let mut task = sample_task(TaskKind::Execute, Status::Ready);
        task.attempts = 2;
        task.updated_at = now;
        let ctx = view_ctx();
        let t = timers(&task, &[], &ctx, now);
        assert!(t.backoff_until.is_some());
    }

    #[test]
    fn timers_backoff_until_none_when_base_is_zero() {
        let now = OffsetDateTime::now_utc();
        let mut task = sample_task(TaskKind::Execute, Status::Ready);
        task.attempts = 2;
        task.updated_at = now;
        let mut ctx = view_ctx();
        ctx.retry_backoff_base = StdDuration::ZERO;
        let t = timers(&task, &[], &ctx, now);
        assert!(t.backoff_until.is_none());
    }

    #[test]
    fn timers_backoff_until_none_when_already_past() {
        let now = OffsetDateTime::now_utc();
        let mut task = sample_task(TaskKind::Execute, Status::Ready);
        task.attempts = 1;
        task.updated_at = now - time::Duration::hours(1);
        let ctx = view_ctx();
        let t = timers(&task, &[], &ctx, now);
        assert!(t.backoff_until.is_none());
    }

    #[test]
    fn timers_counts_consecutive_requeues_from_rows() {
        let now = OffsetDateTime::now_utc();
        let task = sample_task(TaskKind::Execute, Status::Ready);
        let rows = vec![
            row(
                0,
                "t0",
                task.id,
                Event::Transitioned {
                    from: Status::Running,
                    to: Status::Ready,
                    reason: "requeue".into(),
                },
            ),
            row(
                1,
                "t1",
                task.id,
                Event::Transitioned {
                    from: Status::Running,
                    to: Status::Ready,
                    reason: "dispatch".into(),
                },
            ),
            row(
                2,
                "t2",
                task.id,
                Event::Transitioned {
                    from: Status::Running,
                    to: Status::Ready,
                    reason: "requeue".into(),
                },
            ),
        ];
        let ctx = view_ctx();
        let t = timers(&task, &rows, &ctx, now);
        assert_eq!(t.consecutive_requeues, 2);
        assert_eq!(t.max_requeues, ctx.max_requeues);
    }

    // ---- task_list ----

    #[test]
    fn task_list_filters_by_status() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let t1 = sample_task(TaskKind::Execute, Status::Draft);
        let t2 = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&t1).expect("insert t1");
        store.insert(&t2).expect("insert t2");

        let ctx = view_ctx();
        let filter = ListFilter {
            statuses: vec![Status::Ready],
            ..Default::default()
        };
        let list = task_list(
            &store,
            &filter,
            ListOrder::CreatedDesc,
            None,
            100,
            &ctx,
            OffsetDateTime::now_utc(),
        )
        .expect("task_list");
        assert_eq!(list.items.len(), 1);
        assert_eq!(list.items[0].id, t2.id);
        assert_eq!(list.total, 1);
    }

    #[test]
    fn task_list_counts_by_status_ignores_filter() {
        let store = SqliteStore::open_in_memory().expect("open store");
        store
            .insert(&sample_task(TaskKind::Execute, Status::Draft))
            .expect("insert");
        store
            .insert(&sample_task(TaskKind::Execute, Status::Ready))
            .expect("insert");
        store
            .insert(&sample_task(TaskKind::Execute, Status::Ready))
            .expect("insert");

        let ctx = view_ctx();
        let filter = ListFilter {
            statuses: vec![Status::Ready],
            ..Default::default()
        };
        let list = task_list(
            &store,
            &filter,
            ListOrder::CreatedDesc,
            None,
            100,
            &ctx,
            OffsetDateTime::now_utc(),
        )
        .expect("task_list");
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.counts_by_status.get("draft").copied(), Some(1));
        assert_eq!(list.counts_by_status.get("ready").copied(), Some(2));
    }

    #[test]
    fn task_list_paginates_with_cursor_without_duplicates() {
        let store = SqliteStore::open_in_memory().expect("open store");
        for _ in 0..5 {
            store
                .insert(&sample_task(TaskKind::Execute, Status::Draft))
                .expect("insert");
        }
        let ctx = view_ctx();
        let now = OffsetDateTime::now_utc();

        let mut seen: Vec<TaskId> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = task_list(
                &store,
                &ListFilter::default(),
                ListOrder::CreatedDesc,
                cursor.as_deref(),
                2,
                &ctx,
                now,
            )
            .expect("task_list");
            seen.extend(page.items.iter().map(|t| t.id));
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 5);
    }

    #[test]
    fn task_list_reports_children_and_pending_children() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let parent = sample_task(TaskKind::Plan, Status::Done);
        store.insert(&parent).expect("insert parent");
        let mut c1 = sample_task(TaskKind::Execute, Status::Draft);
        c1.parent_id = Some(parent.id);
        let mut c2 = sample_task(TaskKind::Execute, Status::Done);
        c2.parent_id = Some(parent.id);
        store.insert(&c1).expect("insert c1");
        store.insert(&c2).expect("insert c2");

        let ctx = view_ctx();
        let list = task_list(
            &store,
            &ListFilter::default(),
            ListOrder::CreatedDesc,
            None,
            100,
            &ctx,
            OffsetDateTime::now_utc(),
        )
        .expect("task_list");
        let parent_summary = list
            .items
            .iter()
            .find(|t| t.id == parent.id)
            .expect("parent in list");
        assert_eq!(parent_summary.children, 2);
        assert_eq!(parent_summary.pending_children, 1);
    }

    // ---- task_detail ----

    /// ADR-0018 実装メモ M5: Remote のタスクは `cluster` にクラスタ id、`workspace_dir` に手元の写し（`workspace_root/<task_id>`）が出る。
    /// Local のタスクは `cluster: None`。
    #[test]
    fn task_detail_reports_cluster_and_mirror_for_remote_workspaces() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut remote = sample_task(TaskKind::Execute, Status::Ready);
        remote.workspace = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::from("/work/NBB/x/project"),
            mode: None,
        };
        store.insert(&remote).expect("insert");
        let local = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&local).expect("insert");

        let ctx = view_ctx();
        let detail =
            task_detail(&store, remote.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        assert_eq!(detail.cluster.as_deref(), Some("pegasus"));
        assert_eq!(
            detail.workspace_dir.as_deref(),
            Some(format!("/tmp/workspaces/{}", remote.id).as_str()),
            "the mirror where runs/ live"
        );
        let detail =
            task_detail(&store, local.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        assert_eq!(detail.cluster, None);
    }

    /// ADR-0019 D2: `sync = "worktree"` のクラスタでは、worktree のパスとブランチを出す（人が diff / commit する場所）。
    /// `rsync` / `none` のクラスタと Local のタスクでは `null`。
    #[test]
    fn task_detail_reports_the_worktree_for_worktree_clusters() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut on_worktree = sample_task(TaskKind::Execute, Status::Ready);
        on_worktree.workspace = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::from("/work/NBB/x/benchfs"),
            mode: None,
        };
        store.insert(&on_worktree).expect("insert");
        let mut on_rsync = sample_task(TaskKind::Execute, Status::Ready);
        on_rsync.workspace = WorkspaceSpec::Remote {
            cluster: "sirius".into(),
            path: PathBuf::from("/work/NBB/x/scratch"),
            mode: None,
        };
        store.insert(&on_rsync).expect("insert");

        let mut ctx = view_ctx();
        ctx.clusters.insert(
            "pegasus".to_string(),
            ClusterViewInfo {
                sync: "worktree".to_string(),
                worktree_root: None,
                ..Default::default()
            },
        );
        ctx.clusters.insert(
            "sirius".to_string(),
            ClusterViewInfo {
                sync: "rsync".to_string(),
                worktree_root: None,
                ..Default::default()
            },
        );

        let detail =
            task_detail(&store, on_worktree.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        let wt = detail.worktree.expect("worktree cluster");
        assert_eq!(wt.project, "/work/NBB/x/benchfs");
        assert_eq!(
            wt.dir,
            format!("/work/NBB/x/benchfs/.celeris-worktrees/{}", on_worktree.id)
        );
        assert_eq!(wt.branch, format!("celeris/{}", on_worktree.id));

        let detail =
            task_detail(&store, on_rsync.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        assert_eq!(
            detail.worktree, None,
            "rsync のクラスタには worktree が無い"
        );

        // worktree_root を設定したらそちらが親になる。
        ctx.clusters.insert(
            "pegasus".to_string(),
            ClusterViewInfo {
                sync: "worktree".to_string(),
                worktree_root: Some(PathBuf::from("/work/NBB/x/wt")),
                ..Default::default()
            },
        );
        let detail =
            task_detail(&store, on_worktree.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        assert_eq!(
            detail.worktree.expect("worktree").dir,
            format!("/work/NBB/x/wt/{}", on_worktree.id)
        );
    }

    /// GUI-R2（ADR-0016 D1）: 一覧の各行にも `role` が出る（詳細を N+1 で引かなくてよい）。
    #[test]
    fn task_list_items_carry_the_role() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut lead = sample_task(TaskKind::Execute, Status::Ready);
        lead.role = Some("lead".to_string());
        store.insert(&lead).expect("insert");
        let plain = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&plain).expect("insert");

        let list = task_list(
            &store,
            &ListFilter::default(),
            ListOrder::CreatedDesc,
            None,
            100,
            &view_ctx(),
            OffsetDateTime::now_utc(),
        )
        .expect("task_list");
        let role_of = |id| {
            list.items
                .iter()
                .find(|t| t.id == id)
                .expect("in list")
                .role
                .clone()
        };
        assert_eq!(role_of(lead.id).as_deref(), Some("lead"));
        assert_eq!(role_of(plain.id), None);
    }

    /// GUI 監査 H4（Phase 29）: 一覧の各項目に裏方の印が出る（決定的な優先順。対話が最優先）。
    #[test]
    fn task_list_items_carry_the_support_kind() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut plain = sample_task(TaskKind::Execute, Status::Ready);
        plain.role = None;
        store.insert(&plain).expect("insert plain");
        let mut compaction = sample_task(TaskKind::Execute, Status::Ready);
        compaction.role = Some(task_core::COMPACTION_ROLE.to_string());
        store.insert(&compaction).expect("insert compaction");
        let approval = sample_task(TaskKind::Approval, Status::Ready);
        store.insert(&approval).expect("insert approval");
        let review = sample_task(TaskKind::Review, Status::Reviewing);
        store.insert(&review).expect("insert review");

        let list = task_list(
            &store,
            &ListFilter::default(),
            ListOrder::CreatedDesc,
            None,
            100,
            &view_ctx(),
            OffsetDateTime::now_utc(),
        )
        .expect("task_list");
        let support_of = |id| {
            list.items
                .iter()
                .find(|t| t.id == id)
                .expect("in list")
                .support
                .clone()
        };
        assert_eq!(support_of(plain.id), None);
        assert_eq!(support_of(compaction.id).as_deref(), Some("compaction"));
        assert_eq!(support_of(approval.id).as_deref(), Some("approval"));
        assert_eq!(support_of(review.id).as_deref(), Some("review"));
    }

    /// ADR-0027 D1: `Task.genre` は `role` と同じ理由で一覧の各項目に出る。
    #[test]
    fn task_list_items_carry_the_genre() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut with_genre = sample_task(TaskKind::Execute, Status::Ready);
        with_genre.genre = Some("coding".to_string());
        store.insert(&with_genre).expect("insert");
        let plain = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&plain).expect("insert");

        let list = task_list(
            &store,
            &ListFilter::default(),
            ListOrder::CreatedDesc,
            None,
            100,
            &view_ctx(),
            OffsetDateTime::now_utc(),
        )
        .expect("task_list");
        let genre_of = |id| {
            list.items
                .iter()
                .find(|t| t.id == id)
                .expect("in list")
                .genre
                .clone()
        };
        assert_eq!(genre_of(with_genre.id).as_deref(), Some("coding"));
        assert_eq!(genre_of(plain.id), None);
    }

    /// ADR-0016 D1/D2: `role` はトップレベルにも出て、`delegated` は `Event::Delegated` から組み立てる。
    #[test]
    fn task_detail_reports_role_and_delegated_children() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut parent = sample_task(TaskKind::Execute, Status::Running);
        parent.role = Some("lead".to_string());
        store.insert(&parent).expect("insert parent");

        let mut child = sample_task(TaskKind::Execute, Status::Ready);
        child.parent_id = Some(parent.id);
        store.insert(&child).expect("insert child");

        let missing_child = TaskId::new();
        store
            .append_event(
                parent.id,
                &Event::Delegated {
                    run_id: "run-1".into(),
                    task_ids: vec![child.id, missing_child],
                },
            )
            .expect("append delegated");

        let ctx = view_ctx();
        let detail =
            task_detail(&store, parent.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        assert_eq!(detail.role.as_deref(), Some("lead"));
        assert_eq!(detail.delegated.len(), 1);
        assert_eq!(detail.delegated[0].run_id, "run-1");
        assert_eq!(
            detail.delegated[0].tasks.len(),
            1,
            "missing child id is dropped"
        );
        assert_eq!(detail.delegated[0].tasks[0].id, child.id);
    }

    /// ADR-0027 D1: `genre` も `role` と同じく最上位に出る。
    #[test]
    fn task_detail_reports_genre() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut task = sample_task(TaskKind::Execute, Status::Running);
        task.genre = Some("literature".to_string());
        store.insert(&task).expect("insert");

        let ctx = view_ctx();
        let detail = task_detail(&store, task.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        assert_eq!(detail.genre.as_deref(), Some("literature"));
    }

    #[test]
    fn task_detail_missing_task_returns_not_found() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let missing = TaskId::new();
        let ctx = view_ctx();
        let result = task_detail(&store, missing, &ctx, OffsetDateTime::now_utc());
        assert!(matches!(result, Err(OpsError::NotFound(id)) if id == missing));
    }

    #[test]
    fn task_detail_reports_dependencies_dependents_children_and_actions() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let dep = sample_task(TaskKind::Execute, Status::Done);
        store.insert(&dep).expect("insert dep");
        let mut task = sample_task(TaskKind::Execute, Status::Ready);
        task.depends_on = vec![dep.id];
        store.insert(&task).expect("insert task");
        let mut dependent = sample_task(TaskKind::Execute, Status::Draft);
        dependent.depends_on = vec![task.id];
        store.insert(&dependent).expect("insert dependent");
        let mut child = sample_task(TaskKind::Execute, Status::Draft);
        child.parent_id = Some(task.id);
        store.insert(&child).expect("insert child");

        let ctx = view_ctx();
        let detail =
            task_detail(&store, task.id, &ctx, OffsetDateTime::now_utc()).expect("task_detail");
        assert_eq!(
            detail.dependencies.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![dep.id]
        );
        assert_eq!(
            detail.dependents.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![dependent.id]
        );
        assert_eq!(
            detail.children.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![child.id]
        );
        assert!(detail.actions.contains(&Action::Cancel));
        assert!(detail.worker_run_hint.is_some());
    }

    #[test]
    fn task_detail_blocked_task_reports_latest_question() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&task).expect("insert task");
        store
            .append_event(
                task.id,
                &Event::WorkerFinished {
                    run_id: "r1".into(),
                    outcome: "question: which version?".into(),
                    usage: None,
                    role: None,
                    metrics: None,
                    end: None,
                },
            )
            .expect("append worker finished");

        let ctx = view_ctx();
        let detail =
            task_detail(&store, task.id, &ctx, OffsetDateTime::now_utc()).expect("task_detail");
        assert_eq!(detail.latest_question.as_deref(), Some("which version?"));
        assert!(detail.actions.contains(&Action::Answer));
    }

    #[test]
    fn task_detail_terminal_task_has_no_worker_run_hint() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Done);
        store.insert(&task).expect("insert task");
        let ctx = view_ctx();
        let detail =
            task_detail(&store, task.id, &ctx, OffsetDateTime::now_utc()).expect("task_detail");
        assert!(detail.worker_run_hint.is_none());
        // ADR-0044 D2（Phase 53）: `done` は編集できないが「再開」はできる。
        assert_eq!(detail.actions, vec![Action::Reopen]);
        assert!(detail.failure.is_none(), "done is not a failure");
    }

    /// ADR-0070 D1/D2（Phase 116。D6(a)）: `failed` のタスク詳細は `failure`（分類・理由・配送済みの
    /// release）を持ち、`review_fail` だけが原因のときだけ `Action::Rereview` が付く。
    #[test]
    fn task_detail_failed_task_reports_failure_and_rereview_when_review_fail_is_the_only_reason() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut task = sample_task(TaskKind::Execute, Status::Failed);
        task.acceptance = vec![Criterion {
            text: "reviewer checks it".into(),
            check: Check::Reviewer,
        }];
        store.insert(&task).expect("insert task");
        store
            .append_event(
                task.id,
                &Event::ReviewVerdict {
                    run_id: "rev-1".into(),
                    criterion_idx: 0,
                    pass: false,
                    reason: "テストが落ちている".into(),
                },
            )
            .expect("verdict");
        store
            .append_event(
                task.id,
                &Event::Transitioned {
                    from: Status::Reviewing,
                    to: Status::Failed,
                    reason: "review_fail".into(),
                },
            )
            .expect("transitioned");

        let ctx = view_ctx();
        let detail =
            task_detail(&store, task.id, &ctx, OffsetDateTime::now_utc()).expect("task_detail");
        let failure = detail.failure.expect("failure summary");
        assert_eq!(failure.class, derive::FailureClass::Work);
        assert_eq!(failure.reason, "テストが落ちている");
        assert!(failure.delivered_release.is_none());
        assert!(detail.actions.contains(&Action::Retry));
        assert!(detail.actions.contains(&Action::Rereview));
    }

    /// インフラ分類（`infra failure ×N`）は `Action::Rereview` を出さない
    /// （`review_fail` が原因ではないので `can_rereview` が偽になる）。
    #[test]
    fn task_detail_infra_failed_task_reports_infra_class_without_rereview() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Failed);
        store.insert(&task).expect("insert task");
        store
            .append_event(
                task.id,
                &Event::WorkerFinished {
                    run_id: "run-1".into(),
                    outcome: "infra failure ×5: adapter: session resume rejected".into(),
                    usage: None,
                    role: None,
                    metrics: None,
                    end: None,
                },
            )
            .expect("finished");

        let ctx = view_ctx();
        let detail =
            task_detail(&store, task.id, &ctx, OffsetDateTime::now_utc()).expect("task_detail");
        let failure = detail.failure.expect("failure summary");
        assert_eq!(failure.class, derive::FailureClass::Infra);
        assert!(detail.actions.contains(&Action::Retry));
        assert!(!detail.actions.contains(&Action::Rereview));
    }

    #[test]
    fn task_detail_links_human_approval_children_by_attempt_and_marks_decided() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut parent = sample_task(TaskKind::Execute, Status::Reviewing);
        parent.acceptance = vec![Criterion {
            text: "human ok".into(),
            check: Check::Human,
        }];
        store.insert(&parent).expect("insert parent");

        let title1 = {
            let mut t = parent.clone();
            t.attempts = 0;
            derive::human_approval_title(&t, 0)
        };
        let mut approval1 = sample_task(TaskKind::Approval, Status::Done);
        approval1.parent_id = Some(parent.id);
        approval1.title = title1;
        store.insert(&approval1).expect("insert approval1");
        store
            .append_event(
                approval1.id,
                &Event::ApprovalDecided {
                    by: "human".into(),
                    approved: true,
                    note: Some("lgtm".into()),
                },
            )
            .expect("append decided");

        let title2 = {
            let mut t = parent.clone();
            t.attempts = 1;
            derive::human_approval_title(&t, 0)
        };
        let mut approval2 = sample_task(TaskKind::Approval, Status::Ready);
        approval2.parent_id = Some(parent.id);
        approval2.title = title2;
        store.insert(&approval2).expect("insert approval2");

        let ctx = view_ctx();
        let detail =
            task_detail(&store, parent.id, &ctx, OffsetDateTime::now_utc()).expect("task_detail");

        assert_eq!(detail.approvals.len(), 2);
        let approved = detail
            .approvals
            .iter()
            .find(|a| a.approval.id == approval1.id)
            .expect("approval1 link present");
        assert_eq!(approved.criterion_idx, Some(0));
        assert_eq!(approved.attempt, Some(1));
        assert!(
            approved
                .decided
                .as_ref()
                .is_some_and(|d| d.approved && d.note.as_deref() == Some("lgtm"))
        );

        let pending = detail
            .approvals
            .iter()
            .find(|a| a.approval.id == approval2.id)
            .expect("approval2 link present");
        assert_eq!(pending.attempt, Some(2));
        assert!(pending.decided.is_none());

        assert_eq!(detail.criteria.len(), 1);
        assert_eq!(
            detail.criteria[0].approval.as_ref().map(|a| a.approval.id),
            Some(approval2.id)
        );
    }

    // ---- parse_human_approval_title ----

    #[test]
    fn parse_human_approval_title_roundtrips_with_human_approval_title() {
        let mut task = sample_task(TaskKind::Execute, Status::Reviewing);
        task.title = "fix the bug".into();
        task.attempts = 2;
        let title = derive::human_approval_title(&task, 4);
        assert_eq!(parse_human_approval_title(&title), Some((4, 3)));
    }

    #[test]
    fn parse_human_approval_title_handles_title_containing_the_marker() {
        let mut task = sample_task(TaskKind::Execute, Status::Reviewing);
        task.title = "weird task — criterion 9 (attempt 9)".into();
        task.attempts = 0;
        let title = derive::human_approval_title(&task, 2);
        assert_eq!(parse_human_approval_title(&title), Some((2, 1)));
    }

    #[test]
    fn parse_human_approval_title_none_for_unrelated_or_malformed_strings() {
        assert_eq!(parse_human_approval_title("just a regular title"), None);
        assert_eq!(
            parse_human_approval_title("Approval needed: x — criterion abc (attempt 1)"),
            None
        );
        assert_eq!(
            parse_human_approval_title("Approval needed: x — criterion 1 (attempt)"),
            None
        );
    }

    // ---- ADR-0072 D19/D20（Phase E5）: Execution 節 ----

    fn wu_spec(
        key: &str,
        kind: task_core::WorkUnitKind,
        depends_on: &[&str],
    ) -> task_core::WorkUnitSpec {
        task_core::WorkUnitSpec {
            key: key.to_string(),
            kind,
            title: format!("title {key}"),
            objective: format!("objective for {key}, spelled out plainly and distinctly"),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            done_when: vec![],
            checks: vec![],
            context: task_core::WorkUnitContext::default(),
            harness: None,
            features: None,
            budget: None,
            outputs: vec![],
            phase: None,
        }
    }

    fn plan_spec(work_units: Vec<task_core::WorkUnitSpec>) -> task_core::ExecutionPlanSpec {
        task_core::ExecutionPlanSpec {
            schema: task_core::EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "investigate then implement".to_string(),
            work_units,
            phases: Vec::new(),
            children: Vec::new(),
        }
    }

    fn set_status(
        store: &SqliteStore,
        task_id: TaskId,
        key: &str,
        status: task_core::WorkUnitStatus,
    ) {
        let units = store.work_units_for(task_id).expect("work_units_for");
        let row = units.iter().find(|u| u.key == key).expect("wu");
        let mut updated = row.clone();
        let from = updated.status;
        updated.status = status;
        store
            .work_unit_transition(
                task_id,
                updated,
                Event::WorkUnitTransitioned {
                    work_unit_id: row.id.clone(),
                    key: row.key.clone(),
                    from,
                    to: status,
                    reason: "completed".to_string(),
                    run_id: None,
                },
            )
            .expect("work_unit_transition");
    }

    /// (c): 計画も gate の判定も無い（events に E-phase の活動が無い）古いタスクは
    /// `execution: None` のまま（`task_detail` の他のフィールドは変わらない）。
    #[test]
    fn task_detail_execution_is_none_for_a_task_with_no_execution_activity() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(TaskKind::Execute, Status::Done);
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::WorkerFinished {
                    run_id: "r1".to_string(),
                    outcome: "done: ok".to_string(),
                    usage: None,
                    role: None,
                    metrics: None,
                    end: None,
                },
            )
            .expect("append");

        let ctx = view_ctx();
        let detail = task_detail(&store, task.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        assert!(detail.execution.is_none());
    }

    /// gate が atomic と判定しただけ（計画なし）の Task は Execution 節が出るが `plan` は無い
    /// （D20 の「直接実行」の 1 行に対応する材料）。
    #[test]
    fn task_detail_execution_shows_gate_only_when_there_is_no_plan() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut task = sample_task(TaskKind::Execute, Status::Running);
        task.routing = Some(task_core::TaskRouting {
            execution: Some(task_core::ExecutionGateDecision {
                mode: task_core::ExecutionMode::Atomic,
                source: task_core::GateSource::Policy,
                score: 1,
                threshold: 5,
                rule_id: "atomic/score".to_string(),
                signals: vec![],
                policy_version: task_core::EXECUTION_GATE_POLICY_VERSION.to_string(),
                shadow: false,
            }),
            ..task_core::TaskRouting::default()
        });
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::ExecutionGated {
                    decision: Box::new(task.routing.as_ref().unwrap().execution.clone().unwrap()),
                },
            )
            .expect("append");

        let ctx = view_ctx();
        let detail = task_detail(&store, task.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        let execution = detail.execution.expect("execution present");
        assert_eq!(
            execution.gate.map(|g| g.mode),
            Some(task_core::ExecutionMode::Atomic)
        );
        assert!(execution.plan.is_none());
        // 走っている WU が無い（そもそも計画が無い）ので phase は導出しない。
        assert!(execution.phase.is_none());
    }

    /// 計画のある Task: WU の表・現在の段階（`running` かつ WU が `running` なら `executing`）・
    /// done の件数を確かめる。
    #[test]
    fn task_detail_execution_reports_the_plan_and_work_unit_table() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(TaskKind::Execute, Status::Running);
        store.insert(&task).expect("insert");
        crate::execution::adopt_plan(
            &store,
            task.id,
            plan_spec(vec![
                wu_spec("survey", task_core::WorkUnitKind::Investigate, &[]),
                wu_spec("build", task_core::WorkUnitKind::Implement, &["survey"]),
            ]),
            task_core::PlanOrigin::Fixture,
            None,
            task_core::ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .expect("adopt_plan");
        set_status(&store, task.id, "survey", task_core::WorkUnitStatus::Done);
        set_status(&store, task.id, "build", task_core::WorkUnitStatus::Running);

        let ctx = view_ctx();
        let detail = task_detail(&store, task.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        let execution = detail.execution.expect("execution present");
        assert_eq!(execution.phase, Some(ExecutionPhase::Executing));
        let plan = execution.plan.expect("plan present");
        assert_eq!(plan.work_units.len(), 2);
        assert_eq!(plan.work_units[0].key, "survey");
        assert_eq!(plan.work_units[0].status, task_core::WorkUnitStatus::Done);
        assert_eq!(plan.work_units[1].key, "build");
        assert_eq!(
            plan.work_units[1].status,
            task_core::WorkUnitStatus::Running
        );
        assert_eq!(execution.metrics.work_units_total, 2);
        assert_eq!(execution.metrics.work_units_done, 1);
        assert_eq!(plan.versions.len(), 1);
    }

    /// replan の後、版の履歴（`versions`）に旧版・新版が理由付きで並ぶ（D17(f)）。
    #[test]
    fn task_detail_execution_reports_replan_version_history() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&task).expect("insert");
        crate::execution::adopt_plan(
            &store,
            task.id,
            plan_spec(vec![wu_spec("a", task_core::WorkUnitKind::Implement, &[])]),
            task_core::PlanOrigin::Fixture,
            None,
            task_core::ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .expect("adopt_plan");
        crate::execution::replan(
            &store,
            task.id,
            plan_spec(vec![
                wu_spec("a", task_core::WorkUnitKind::Implement, &[]),
                wu_spec("b", task_core::WorkUnitKind::Implement, &["a"]),
            ]),
            "add a follow-up step".to_string(),
            task_core::PlanOrigin::Human,
            None,
            task_core::ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .expect("replan");

        let ctx = view_ctx();
        let detail = task_detail(&store, task.id, &ctx, OffsetDateTime::now_utc()).expect("detail");
        let plan = detail
            .execution
            .expect("execution present")
            .plan
            .expect("plan");
        assert_eq!(plan.version, 2);
        assert_eq!(plan.versions.len(), 2);
        assert_eq!(plan.versions[0].version, 1);
        assert_eq!(plan.versions[0].status, task_core::PlanStatus::Superseded);
        assert_eq!(plan.versions[1].version, 2);
        assert_eq!(plan.versions[1].status, task_core::PlanStatus::Active);
        // ADR-0074 D5.3（Phase F1）: reason の後ろに差分の件数（added/changed/removed）が付く。
        assert_eq!(
            plan.versions[1].reason.as_deref(),
            Some("add a follow-up step (added=1, changed=0, removed=0)")
        );
    }

    /// `reviewing` は計画の有無に関わらず常に `verifying`。
    #[test]
    fn execution_phase_reviewing_is_always_verifying() {
        let task = sample_task(TaskKind::Execute, Status::Reviewing);
        assert_eq!(execution_phase(&task, &[]), Some(ExecutionPhase::Verifying));
    }
}
