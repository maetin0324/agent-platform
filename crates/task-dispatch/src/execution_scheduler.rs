//! ADR-0072 D6/D11/D12/D15（Phase E2）: WorkUnit の状態遷移の決定。
//!
//! 純粋関数（I/O は無い。ADR-0001 D2）。`dispatcher.rs` が「この run がどう終わったか」（[`RunEnd`]・
//! checkpoint・進捗）を渡すと、ここが「この WorkUnit と Task はどうなるか」（[`WuDecision`]）を返す。
//! 実際に store へ書き込む（`work_unit_transition` の呼び出し・トランザクション）のは `dispatcher.rs`
//! の責務のまま。

use task_core::execution::RunEnd;
use task_core::{
    Checkpoint, ContinueWhy, Trigger, WorkUnitBlockedReason, WorkUnitRow, WorkUnitStatus,
    checkpoint_shows_progress, dependents_to_block, newly_ready,
};

/// D18 の上限（呼び出し側が `[execution]` の設定または既定値から組み立てる）。
#[derive(Debug, Clone, Copy)]
pub struct WuLimits {
    pub max_continuations: u32,
    pub no_progress_limit: u32,
    pub max_retries: u32,
}

/// [`decide`] の結果。`dispatcher.rs` はこれを見て `work_unit_transition`（更新された WU の行 +
/// `WorkUnitTransitioned`）と、Task レベルの `Trigger` を適用する。
#[derive(Debug, Clone, PartialEq)]
pub struct WuDecision {
    /// この WU の新しい行（`status`・カウンタを更新済み）。
    pub updated: WorkUnitRow,
    /// `WorkUnitTransitioned.reason`（決定的な静的文字列）。
    pub reason: &'static str,
    /// Task レベルの trigger。
    pub trigger: Trigger,
    /// `WorkerFinished.outcome` に付け足す接頭辞・文言（D12 の `"work unit <key> failed: "` 等。
    /// 無ければ `None` で、呼び出し側は通常どおりの `outcome_str` を使う）。
    pub outcome_override: Option<String>,
    /// D15: 失敗の伝播で `blocked(dependency_failed)` にする、他の WU の新しい行。
    pub newly_blocked: Vec<WorkUnitRow>,
    /// D15: この WU が `done` になったことで `pending → ready` に上がる、他の WU の新しい行。
    pub newly_ready: Vec<WorkUnitRow>,
    /// 計画の中で、この決定の後に非終端（有効）の WU がもう無い（＝ Task を完了させてよい）。
    pub plan_complete: bool,
}

/// D9/D18: continuation（予算切れ・yield）の判定に要る、checkpoint 周りの入力をまとめたもの。
#[derive(Debug, Clone, Copy, Default)]
pub struct ContinuationInputs<'a> {
    /// D8 で合成した確定値（`end.is_continuable()` のときだけ `Some`）。
    pub checkpoint: Option<&'a Checkpoint>,
    /// この WU の直前の checkpoint（進捗判定用。無ければ `None`）。
    pub prev_checkpoint: Option<&'a Checkpoint>,
    pub no_progress_before: u32,
}

/// D6: この WU の run が `end` で終わったとき、WU と Task がどうなるかを決める。
///
/// - `wu` は「いま `running` の WU」の現在の行（呼び出し側が `status = Running` であることを保証する）。
/// - `all_units` はこの Task の全 WU（`wu` 自身を含む。伝播・完了判定に使う）。
/// - `continuation` は D8/D18 の checkpoint 周りの入力（[`ContinuationInputs`]）。
/// - `limits` は D18 の上限。
pub fn decide(
    end: RunEnd,
    run_id: &str,
    wu: &WorkUnitRow,
    all_units: &[WorkUnitRow],
    continuation_inputs: ContinuationInputs<'_>,
    limits: WuLimits,
) -> WuDecision {
    // ADR-0072 D17 3.（Phase E4b 項目2）: `end` の種類に関わらず、合成した checkpoint が
    // `plan_issue` を持っていれば最優先する（continuation の判定より前）。checkpoint はいまのところ
    // `end.is_continuable()`（Yielded/BudgetExhausted）のときだけ合成される（D8/E2b の制約）ので、
    // 実際にこの分岐に入るのはその 2 つの終わり方だけ。
    if continuation_inputs
        .checkpoint
        .is_some_and(|cp| cp.plan_issue.is_some())
    {
        return blocked(wu, WorkUnitBlockedReason::PlanIssue);
    }
    match end {
        RunEnd::Completed => complete(wu, run_id, all_units),
        RunEnd::Yielded | RunEnd::BudgetExhausted { .. } => continuation(
            wu,
            continuation_inputs.checkpoint,
            continuation_inputs.prev_checkpoint,
            continuation_inputs.no_progress_before,
            limits,
        ),
        RunEnd::Question => blocked(wu, WorkUnitBlockedReason::Question),
        RunEnd::Failed { retryable } => failed(wu, all_units, retryable, limits),
        // ADR-0072 D6 表: harness_error は「ready、checkpoint があれば needs_continuation」。
        // checkpoint はこの分類では合成していない（`end.is_continuable()` が false のため）ので、
        // E2 では単純に ready へ戻す（`retries` は数えない。既存の Requeue/InfraRequeue の
        // カウンタが別に効く）。
        RunEnd::HarnessError { .. } | RunEnd::Cancelled => reset_to_ready(wu),
    }
}

fn now_wu(mut wu: WorkUnitRow, status: WorkUnitStatus) -> WorkUnitRow {
    wu.status = status;
    // ADR-0074 D1.5（Phase F2）: `running` を離れたら WU の lease を外す（v1 では常に空のまま）。
    if status != WorkUnitStatus::Running {
        wu.clear_lease();
    }
    if status != WorkUnitStatus::Blocked {
        wu.blocked_reason = None;
    }
    wu
}

fn complete(wu: &WorkUnitRow, run_id: &str, all_units: &[WorkUnitRow]) -> WuDecision {
    let mut updated = now_wu(wu.clone(), WorkUnitStatus::Done);
    updated.last_run_id = Some(run_id.to_string());

    // D15: この WU が done になったので、`all_units` を仮に更新した上で依存の解決と完了判定を行う。
    let mut projected: Vec<WorkUnitRow> = all_units
        .iter()
        .map(|u| {
            if u.id == wu.id {
                updated.clone()
            } else {
                u.clone()
            }
        })
        .collect();
    let ready_ids = newly_ready(&projected);
    let mut ready_rows = Vec::new();
    for u in projected.iter_mut() {
        if ready_ids.contains(&u.id) {
            u.status = WorkUnitStatus::Ready;
            ready_rows.push(u.clone());
        }
    }

    let plan_complete = projected
        .iter()
        .filter(|u| u.status.is_active())
        .all(|u| u.status == WorkUnitStatus::Done);

    WuDecision {
        updated,
        reason: "completed",
        trigger: if plan_complete {
            Trigger::WorkerDone
        } else {
            Trigger::Continue {
                why: ContinueWhy::Advance,
            }
        },
        outcome_override: None,
        newly_blocked: Vec::new(),
        newly_ready: ready_rows,
        plan_complete,
    }
}

fn continuation(
    wu: &WorkUnitRow,
    checkpoint: Option<&Checkpoint>,
    prev_checkpoint: Option<&Checkpoint>,
    no_progress_before: u32,
    limits: WuLimits,
) -> WuDecision {
    let progressed = checkpoint_shows_progress(
        prev_checkpoint,
        checkpoint.expect(
            "continuation() is only called when RunEnd::is_continuable(); a checkpoint was merged",
        ),
    );
    let no_progress = if progressed {
        0
    } else {
        no_progress_before + 1
    };

    if wu.continuations >= limits.max_continuations || no_progress >= limits.no_progress_limit {
        let mut updated = now_wu(wu.clone(), WorkUnitStatus::Blocked);
        updated.blocked_reason = Some(WorkUnitBlockedReason::Limit);
        return WuDecision {
            updated,
            reason: "limit",
            trigger: Trigger::WorkerQuestion,
            outcome_override: Some(format!(
                "question: WorkUnit {} が進みません（continuation {} 回 / 進捗なし {} 回）。予算を増やして続ける／分割し直す（replan）／中止のいずれかを選んでください。",
                wu.key, wu.continuations, no_progress
            )),
            newly_blocked: Vec::new(),
            newly_ready: Vec::new(),
            plan_complete: false,
        };
    }

    let mut updated = now_wu(wu.clone(), WorkUnitStatus::NeedsContinuation);
    updated.continuations += 1;
    WuDecision {
        updated,
        reason: "continue",
        trigger: Trigger::Continue {
            why: ContinueWhy::Continue,
        },
        outcome_override: None,
        newly_blocked: Vec::new(),
        newly_ready: Vec::new(),
        plan_complete: false,
    }
}

fn blocked(wu: &WorkUnitRow, reason: WorkUnitBlockedReason) -> WuDecision {
    let mut updated = now_wu(wu.clone(), WorkUnitStatus::Blocked);
    updated.blocked_reason = Some(reason);
    WuDecision {
        updated,
        reason: reason.as_str(),
        trigger: Trigger::WorkerQuestion,
        outcome_override: None,
        newly_blocked: Vec::new(),
        newly_ready: Vec::new(),
        plan_complete: false,
    }
}

fn reset_to_ready(wu: &WorkUnitRow) -> WuDecision {
    let updated = now_wu(wu.clone(), WorkUnitStatus::Ready);
    WuDecision {
        updated,
        reason: "harness_error",
        // Task レベルの trigger は呼び出し側（dispatcher.rs）が Requeue/InfraRequeue/WorkerError の
        // 既存の分類のまま使う。ここでは WU の状態だけを決める（`trigger` はダミーで上書きされる）。
        trigger: Trigger::Requeue,
        outcome_override: None,
        newly_blocked: Vec::new(),
        newly_ready: Vec::new(),
        plan_complete: false,
    }
}

/// D12 の 3 / D11 の retry: WU が失敗したとき、retry の余地があれば `ready` に戻し（retries+1）、
/// 無ければ `failed` にして依存先を `blocked(dependency_failed)` にする（E2 に planner は無いので
/// replan できず、Task は `WorkerError{retryable:false}` で `failed` にする。D12）。
fn failed(
    wu: &WorkUnitRow,
    all_units: &[WorkUnitRow],
    retryable: bool,
    limits: WuLimits,
) -> WuDecision {
    if retryable && wu.retries < limits.max_retries {
        let mut updated = now_wu(wu.clone(), WorkUnitStatus::Ready);
        updated.retries += 1;
        return WuDecision {
            updated,
            reason: "retry",
            trigger: Trigger::Continue {
                why: ContinueWhy::WorkUnitRetry,
            },
            outcome_override: None,
            newly_blocked: Vec::new(),
            newly_ready: Vec::new(),
            plan_complete: false,
        };
    }

    let updated = now_wu(wu.clone(), WorkUnitStatus::Failed);
    let mut projected: Vec<WorkUnitRow> = all_units
        .iter()
        .map(|u| {
            if u.id == wu.id {
                updated.clone()
            } else {
                u.clone()
            }
        })
        .collect();
    let blocked_ids = dependents_to_block(&projected, &wu.key);
    let mut blocked_rows = Vec::new();
    for u in projected.iter_mut() {
        if blocked_ids.contains(&u.id) {
            u.status = WorkUnitStatus::Blocked;
            u.blocked_reason = Some(WorkUnitBlockedReason::DependencyFailed);
            blocked_rows.push(u.clone());
        }
    }
    WuDecision {
        updated,
        reason: "failed",
        trigger: Trigger::WorkerError { retryable: false },
        outcome_override: Some(format!("work unit {} failed", wu.key)),
        newly_blocked: blocked_rows,
        newly_ready: Vec::new(),
        plan_complete: false,
    }
}

/// ADR-0074 D1.6（Phase F2b）: v2 の WU の run が終わって WU の行を書いた後（`units` はその後の
/// 全行）、Task をどうするか。純粋関数（`dispatcher.rs` がこれを見て遷移・統合を行う）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseSettle {
    /// 同じ Task の他の WU の run（または統合）が走っている。Task は遷移しない（兄弟は止めない）。
    Wait,
    /// 現在の工程に起こせる WU がある（走っているものは無い）。`Continue{advance}`（今と同じ）。
    Advance,
    /// 現在の工程の WU がすべて done。この統合 WU（id）を走らせる（Task は遷移しない）。
    Integrate(String),
    /// 現在の工程に `blocked(question)` の WU（id）がある。`WorkerQuestion`。
    Question(String),
    /// 現在の工程に failed / `blocked(limit|plan_issue|dependency_failed)` の WU（id）がある。
    /// replan できれば `Continue{replan}`、できなければ D12。
    Failure(String),
    /// 有効な WU がすべて done（最後の工程の統合まで済んだ）。`WorkerDone`。
    AllDone,
}

/// ADR-0074 D1.6: [`PhaseSettle`] を決める。
///
/// - 走っている WU（`running`。統合 WU を含む）があれば `Wait`（in-flight が 0 になってから決める）。
/// - 現在の工程（有効で終端でない行のうち `seq` 最小の行の工程。`runnable_work_units` と同じ）で、
///   question → 失敗（failed を先に、次に limit / plan_issue / dependency_failed）→ 起こせる WU →
///   統合の順に見る。人の入力（question）を先にするのは、replan で質問を捨てないため。
pub fn settle_phase(units: &[WorkUnitRow]) -> PhaseSettle {
    let active: Vec<&WorkUnitRow> = units.iter().filter(|u| u.status.is_active()).collect();
    if active.iter().any(|u| u.status == WorkUnitStatus::Running) {
        return PhaseSettle::Wait;
    }
    let in_play: Vec<&WorkUnitRow> = active
        .iter()
        .copied()
        .filter(|u| !u.status.is_terminal())
        .collect();
    let Some(current) = in_play.iter().min_by_key(|u| u.seq) else {
        return PhaseSettle::AllDone;
    };
    let phase = current.phase.clone();
    let mut in_phase: Vec<&WorkUnitRow> = in_play
        .iter()
        .copied()
        .filter(|u| u.phase == phase)
        .collect();
    in_phase.sort_by_key(|u| u.seq);
    if let Some(q) = in_phase.iter().find(|u| {
        u.status == WorkUnitStatus::Blocked
            && u.blocked_reason == Some(WorkUnitBlockedReason::Question)
    }) {
        return PhaseSettle::Question(q.id.clone());
    }
    if let Some(f) = in_phase
        .iter()
        .find(|u| u.status == WorkUnitStatus::Failed)
        .or_else(|| {
            in_phase.iter().find(|u| {
                u.status == WorkUnitStatus::Blocked
                    && matches!(
                        u.blocked_reason,
                        Some(WorkUnitBlockedReason::Limit)
                            | Some(WorkUnitBlockedReason::PlanIssue)
                            | Some(WorkUnitBlockedReason::DependencyFailed)
                    )
            })
        })
    {
        return PhaseSettle::Failure(f.id.clone());
    }
    if in_phase.iter().any(|u| {
        u.kind != task_core::WorkUnitKind::Integrate
            && matches!(
                u.status,
                WorkUnitStatus::Ready | WorkUnitStatus::NeedsContinuation
            )
    }) {
        return PhaseSettle::Advance;
    }
    let phase_done = active
        .iter()
        .filter(|u| u.phase == phase && u.kind != task_core::WorkUnitKind::Integrate)
        .all(|u| u.status == WorkUnitStatus::Done);
    if phase_done
        && let Some(integ) = in_phase.iter().find(|u| {
            u.kind == task_core::WorkUnitKind::Integrate
                && matches!(u.status, WorkUnitStatus::Pending | WorkUnitStatus::Ready)
        })
    {
        return PhaseSettle::Integrate(integ.id.clone());
    }
    // ここに来るのは「pending だけが残り、依存も統合も進められない」一瞬の不整合だけ（通常の
    // 経路に戻して gate に任せる）。
    PhaseSettle::Advance
}

/// D18: 人の回答（`Trigger::Answer`）で `blocked` の WU を再開する（窓は 0 に戻す。D18「回答の時点
/// から数え直す」）。`blocked_reason = Limit` なら `needs_continuation`（continuation の続き）、
/// `Question` なら `ready`（最初から）に戻す。`DependencyFailed` は人の回答では戻らない（依存先の
/// 計画が直らない限り解消しない。E2 には該当しない: E2 の失敗した Task は即 `failed` になる）。
pub fn resume_after_answer(wu: &WorkUnitRow) -> WorkUnitRow {
    match wu.blocked_reason {
        Some(WorkUnitBlockedReason::Limit) => now_wu(wu.clone(), WorkUnitStatus::NeedsContinuation),
        _ => now_wu(wu.clone(), WorkUnitStatus::Ready),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{WorkUnitContext, WorkUnitKind, WorkUnitSpec};

    fn row(key: &str, status: WorkUnitStatus, depends_on: &[&str]) -> WorkUnitRow {
        let spec = WorkUnitSpec {
            key: key.to_string(),
            kind: WorkUnitKind::Implement,
            title: key.to_string(),
            objective: format!("objective {key}"),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            done_when: vec![],
            checks: vec![],
            context: WorkUnitContext::default(),
            harness: None,
            features: None,
            budget: None,
            outputs: vec![],
            phase: None,
        };
        WorkUnitRow::new(
            format!("id-{key}"),
            "task".into(),
            "plan".into(),
            0,
            spec,
            status,
            "2026-09-24T00:00:00Z".into(),
        )
    }

    fn prow(
        key: &str,
        seq: u32,
        phase: &str,
        status: WorkUnitStatus,
        depends_on: &[&str],
    ) -> WorkUnitRow {
        let mut r = row(key, status, depends_on);
        r.seq = seq;
        r.phase = Some(phase.to_string());
        r.spec.phase = Some(phase.to_string());
        r
    }

    fn integ(phase: &str, seq: u32, status: WorkUnitStatus, deps: &[&str]) -> WorkUnitRow {
        let mut r = prow(&format!("integrate-{phase}"), seq, phase, status, deps);
        r.kind = task_core::WorkUnitKind::Integrate;
        r.spec.kind = task_core::WorkUnitKind::Integrate;
        r
    }

    #[test]
    fn settle_waits_while_a_sibling_is_running_then_reports_the_question() {
        use WorkUnitStatus::*;
        let mut q = prow("a", 0, "build", Blocked, &[]);
        q.blocked_reason = Some(WorkUnitBlockedReason::Question);
        let units = vec![
            q.clone(),
            prow("b", 1, "build", Running, &[]),
            integ("build", 2, Pending, &["a", "b"]),
        ];
        assert_eq!(settle_phase(&units), PhaseSettle::Wait);
        let units = vec![
            q,
            prow("b", 1, "build", Done, &[]),
            integ("build", 2, Pending, &["a", "b"]),
        ];
        assert_eq!(settle_phase(&units), PhaseSettle::Question("id-a".into()));
    }

    #[test]
    fn settle_reports_a_failure_after_in_flight_reaches_zero() {
        use WorkUnitStatus::*;
        let units = vec![
            prow("a", 0, "build", Failed, &[]),
            prow("b", 1, "build", Done, &[]),
            prow("c", 2, "build", Ready, &[]),
            integ("build", 3, Pending, &["a", "b", "c"]),
        ];
        assert_eq!(settle_phase(&units), PhaseSettle::Failure("id-a".into()));
    }

    #[test]
    fn settle_advances_integrates_and_finishes() {
        use WorkUnitStatus::*;
        let units = vec![
            prow("a", 0, "build", Done, &[]),
            prow("b", 1, "build", Ready, &[]),
            integ("build", 2, Pending, &["a", "b"]),
            prow("c", 3, "verify", Pending, &["a"]),
            integ("verify", 4, Pending, &["c"]),
        ];
        assert_eq!(settle_phase(&units), PhaseSettle::Advance);
        let units = vec![
            prow("a", 0, "build", Done, &[]),
            prow("b", 1, "build", Done, &[]),
            integ("build", 2, Pending, &["a", "b"]),
            prow("c", 3, "verify", Pending, &["a"]),
            integ("verify", 4, Pending, &["c"]),
        ];
        assert_eq!(
            settle_phase(&units),
            PhaseSettle::Integrate("id-integrate-build".into())
        );
        let units = vec![
            prow("a", 0, "build", Done, &[]),
            integ("build", 1, Done, &["a"]),
        ];
        assert_eq!(settle_phase(&units), PhaseSettle::AllDone);
    }

    fn limits() -> WuLimits {
        WuLimits {
            max_continuations: 3,
            no_progress_limit: 2,
            max_retries: 2,
        }
    }

    fn no_ci() -> ContinuationInputs<'static> {
        ContinuationInputs::default()
    }

    #[test]
    fn completing_the_last_work_unit_marks_the_plan_complete_and_triggers_worker_done() {
        let a = row("a", WorkUnitStatus::Running, &[]);
        let units = vec![a.clone()];
        let d = decide(RunEnd::Completed, "r1", &a, &units, no_ci(), limits());
        assert_eq!(d.updated.status, WorkUnitStatus::Done);
        assert_eq!(d.trigger, Trigger::WorkerDone);
        assert!(d.plan_complete);
    }

    #[test]
    fn completing_a_work_unit_with_more_pending_advances_and_promotes_dependents() {
        let a = row("a", WorkUnitStatus::Running, &[]);
        let b = row("b", WorkUnitStatus::Pending, &["a"]);
        let units = vec![a.clone(), b.clone()];
        let d = decide(RunEnd::Completed, "r1", &a, &units, no_ci(), limits());
        assert_eq!(
            d.trigger,
            Trigger::Continue {
                why: ContinueWhy::Advance
            }
        );
        assert!(!d.plan_complete);
        assert_eq!(d.newly_ready.len(), 1);
        assert_eq!(d.newly_ready[0].key, "b");
        assert_eq!(d.newly_ready[0].status, WorkUnitStatus::Ready);
    }

    #[test]
    fn a_retryable_failure_within_the_limit_retries_the_same_work_unit() {
        let a = row("a", WorkUnitStatus::Running, &[]);
        let units = vec![a.clone()];
        let d = decide(
            RunEnd::Failed { retryable: true },
            "r1",
            &a,
            &units,
            no_ci(),
            limits(),
        );
        assert_eq!(d.updated.status, WorkUnitStatus::Ready);
        assert_eq!(d.updated.retries, 1);
        assert_eq!(
            d.trigger,
            Trigger::Continue {
                why: ContinueWhy::WorkUnitRetry
            }
        );
    }

    #[test]
    fn a_failure_at_the_retry_limit_fails_the_work_unit_and_blocks_dependents() {
        let mut a = row("a", WorkUnitStatus::Running, &[]);
        a.retries = 2; // already at max_retries
        let b = row("b", WorkUnitStatus::Pending, &["a"]);
        let c = row("c", WorkUnitStatus::Pending, &["b"]);
        let units = vec![a.clone(), b, c];
        let d = decide(
            RunEnd::Failed { retryable: true },
            "r1",
            &a,
            &units,
            no_ci(),
            limits(),
        );
        assert_eq!(d.updated.status, WorkUnitStatus::Failed);
        assert_eq!(d.trigger, Trigger::WorkerError { retryable: false });
        assert_eq!(d.outcome_override.as_deref(), Some("work unit a failed"));
        let mut blocked_keys: Vec<&str> = d.newly_blocked.iter().map(|u| u.key.as_str()).collect();
        blocked_keys.sort();
        assert_eq!(blocked_keys, vec!["b", "c"]);
        assert!(
            d.newly_blocked
                .iter()
                .all(|u| u.blocked_reason == Some(WorkUnitBlockedReason::DependencyFailed))
        );
    }

    #[test]
    fn a_non_retryable_failure_fails_immediately_regardless_of_retries_so_far() {
        let a = row("a", WorkUnitStatus::Running, &[]);
        let units = vec![a.clone()];
        let d = decide(
            RunEnd::Failed { retryable: false },
            "r1",
            &a,
            &units,
            no_ci(),
            limits(),
        );
        assert_eq!(d.updated.status, WorkUnitStatus::Failed);
    }

    #[test]
    fn a_question_blocks_the_work_unit_and_the_task() {
        let a = row("a", WorkUnitStatus::Running, &[]);
        let units = vec![a.clone()];
        let d = decide(RunEnd::Question, "r1", &a, &units, no_ci(), limits());
        assert_eq!(d.updated.status, WorkUnitStatus::Blocked);
        assert_eq!(
            d.updated.blocked_reason,
            Some(WorkUnitBlockedReason::Question)
        );
        assert_eq!(d.trigger, Trigger::WorkerQuestion);
    }

    fn checkpoint(completed: usize) -> Checkpoint {
        Checkpoint {
            schema: task_core::CHECKPOINT_SCHEMA.into(),
            task_id: "t".into(),
            work_unit: Some("a".into()),
            run_id: "r".into(),
            run_seq: 1,
            end: task_core::CheckpointEnd::BudgetExhausted,
            source: task_core::CheckpointSource::Mechanical,
            completed: (0..completed).map(|i| format!("c{i}")).collect(),
            remaining: vec![],
            decisions: vec![],
            files_changed: vec![],
            tests_run: vec![],
            known_failures: vec![],
            artifact_refs: vec![],
            next_action: "next".into(),
            open_questions: vec![],
            plan_issue: None,
            repo_state: None,
            recent_activity: vec![],
            created_at: "2026-09-24T00:00:00Z".into(),
        }
    }

    /// ADR-0072 D17 3.（Phase E4b 項目2）: checkpoint に `plan_issue` があれば、まだ continuation の
    /// 上限に達していなくても（`continuations = 0`）`blocked(plan_issue)` になる。`limit`/`continue` の
    /// 判定より優先される。
    #[test]
    fn a_plan_issue_in_the_checkpoint_blocks_the_work_unit_even_within_budget() {
        let a = row("a", WorkUnitStatus::Running, &[]);
        let units = vec![a.clone()];
        let mut cp = checkpoint(1);
        cp.plan_issue = Some("migration M is needed before this work unit".into());
        let d = decide(
            RunEnd::BudgetExhausted {
                kind: task_core::BudgetKind::Turns,
            },
            "r1",
            &a,
            &units,
            ContinuationInputs {
                checkpoint: Some(&cp),
                prev_checkpoint: None,
                no_progress_before: 0,
            },
            limits(),
        );
        assert_eq!(d.updated.status, WorkUnitStatus::Blocked);
        assert_eq!(
            d.updated.blocked_reason,
            Some(WorkUnitBlockedReason::PlanIssue)
        );
        assert_eq!(d.reason, "plan_issue");
        assert_eq!(d.trigger, Trigger::WorkerQuestion);
        // continuations は増えない（continuation の判定に入る前に分岐する）。
        assert_eq!(d.updated.continuations, 0);
    }

    #[test]
    fn budget_exhausted_within_limits_moves_to_needs_continuation() {
        let a = row("a", WorkUnitStatus::Running, &[]);
        let units = vec![a.clone()];
        let cp = checkpoint(1);
        let d = decide(
            RunEnd::BudgetExhausted {
                kind: task_core::BudgetKind::Turns,
            },
            "r1",
            &a,
            &units,
            ContinuationInputs {
                checkpoint: Some(&cp),
                prev_checkpoint: None,
                no_progress_before: 0,
            },
            limits(),
        );
        assert_eq!(d.updated.status, WorkUnitStatus::NeedsContinuation);
        assert_eq!(d.updated.continuations, 1);
        assert_eq!(
            d.trigger,
            Trigger::Continue {
                why: ContinueWhy::Continue
            }
        );
    }

    #[test]
    fn hitting_the_continuation_limit_blocks_with_a_question() {
        let mut a = row("a", WorkUnitStatus::Running, &[]);
        a.continuations = 3; // at max_continuations
        let units = vec![a.clone()];
        let cp = checkpoint(1);
        let d = decide(
            RunEnd::BudgetExhausted {
                kind: task_core::BudgetKind::Turns,
            },
            "r1",
            &a,
            &units,
            ContinuationInputs {
                checkpoint: Some(&cp),
                prev_checkpoint: None,
                no_progress_before: 0,
            },
            limits(),
        );
        assert_eq!(d.updated.status, WorkUnitStatus::Blocked);
        assert_eq!(d.updated.blocked_reason, Some(WorkUnitBlockedReason::Limit));
        assert_eq!(d.trigger, Trigger::WorkerQuestion);
    }

    #[test]
    fn no_progress_at_the_limit_blocks_even_under_the_continuation_cap() {
        let a = row("a", WorkUnitStatus::Running, &[]);
        let units = vec![a.clone()];
        let cp = checkpoint(1);
        let prev = checkpoint(1); // same `completed` count => no progress
        let d = decide(
            RunEnd::BudgetExhausted {
                kind: task_core::BudgetKind::Turns,
            },
            "r1",
            &a,
            &units,
            ContinuationInputs {
                checkpoint: Some(&cp),
                prev_checkpoint: Some(&prev),
                // already 1 consecutive no-progress; this one makes 2 = the limit
                no_progress_before: 1,
            },
            limits(),
        );
        assert_eq!(d.updated.status, WorkUnitStatus::Blocked);
        assert_eq!(d.updated.blocked_reason, Some(WorkUnitBlockedReason::Limit));
    }

    #[test]
    fn resume_after_answer_restores_needs_continuation_for_a_limit_block_and_ready_otherwise() {
        let mut a = row("a", WorkUnitStatus::Blocked, &[]);
        a.blocked_reason = Some(WorkUnitBlockedReason::Limit);
        assert_eq!(
            resume_after_answer(&a).status,
            WorkUnitStatus::NeedsContinuation
        );

        let mut b = row("b", WorkUnitStatus::Blocked, &[]);
        b.blocked_reason = Some(WorkUnitBlockedReason::Question);
        assert_eq!(resume_after_answer(&b).status, WorkUnitStatus::Ready);
    }
}
