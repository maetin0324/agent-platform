//! `celerisctl replay` の再構築ロジック — DESIGN.md §4.3 / §5.9, ADR-0002「結果」節, ADR-0004 D6
//! （ADR-0013 D7）。`events` から `tasks` を再構築し、実際の `tasks` テーブルと突き合わせる。
//! `celerisctl` は出力整形と exit code だけを持つ。
//!
//! ADR-0072 D5/D15（Phase E2b）: `work_units`/`runs`（`execution_plans`/`work_units`/`runs` のうち
//! この 2 表。`execution_plans` は E2 の範囲では replan が無く事実上 1 タスク 1 行なので対象外）も、
//! `events`（`ExecutionPlanned`/`WorkUnitTransitioned`/`WorkerStarted`/`WorkerFinished`/
//! `CheckpointSaved`）だけから再構築できる（D5「events が正本、3 つの表は派生の索引」）。

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Serialize;
use task_core::{
    Event, EventRow, ExecutionLimits, ExecutionPlanRow, ExecutionPlanSpec, PlanStatus,
    RunIndexRole, RunIndexStatus, RunRole, RunRow, Status, Task, TaskId, TaskStore,
    WorkUnitBlockedReason, WorkUnitRow, WorkUnitStatus, validate,
};

use crate::error::OpsError;

/// `replay` が検出した 1 件の食い違い。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ReplayMismatch {
    pub task_id: TaskId,
    /// `"status"` または `"attempts"`。
    pub field: String,
    /// `events` から再構築した値。
    pub replayed: String,
    /// `tasks` テーブルに保存されている値。
    pub stored: String,
}

/// `replay` の結果。
#[derive(Debug, Clone, PartialEq, Default, Serialize, JsonSchema)]
pub struct ReplayReport {
    pub tasks: usize,
    pub mismatches: Vec<ReplayMismatch>,
}

/// `Event` 列を畳み込んで `(status, attempts)` を再構築する（ADR-0004 D6）。
/// `Created` が初期値を与え、以降の `Transitioned` が上書きしていく。
fn replay_status_and_attempts(events: &[(u64, Event)]) -> Option<(Status, u32)> {
    let mut state: Option<(Status, u32)> = None;
    for (_, event) in events {
        match event {
            Event::Created { task } => {
                state = Some((task.status, task.attempts));
            }
            Event::Transitioned { to, reason, .. } => {
                let (_, attempts) = state.unwrap_or((*to, 0));
                // ADR-0044 D2（Phase 53）: `reopen` は attempts を **0 に戻す**唯一のトリガ
                // （`Trigger::Reopen`）。ここに書かないと、再開したタスクは毎回
                // `attempts: replayed=N stored=0` の不一致として報告され続ける。
                if reason == "reopen" {
                    state = Some((*to, 0));
                    continue;
                }
                let bump = matches!(reason.as_str(), "worker_error" | "lease_expired" | "review_fail")
                    // ADR-0021 D1: `child_failed` は「やり直し」のときだけ attempts を使う
                    // （人の判断待ち = blocked に落ちるときは据え置き）。
                    || (reason == "child_failed" && *to == Status::Ready);
                state = Some((*to, if bump { attempts + 1 } else { attempts }));
            }
            _ => {}
        }
    }
    state
}

fn diff_task(task: &Task, events: &[(u64, Event)]) -> Vec<ReplayMismatch> {
    let mut mismatches = Vec::new();
    match replay_status_and_attempts(events) {
        Some((status, attempts)) => {
            if status != task.status {
                mismatches.push(ReplayMismatch {
                    task_id: task.id,
                    field: "status".to_string(),
                    replayed: format!("{status:?}"),
                    stored: format!("{:?}", task.status),
                });
            }
            if attempts != task.attempts {
                mismatches.push(ReplayMismatch {
                    task_id: task.id,
                    field: "attempts".to_string(),
                    replayed: attempts.to_string(),
                    stored: task.attempts.to_string(),
                });
            }
        }
        None => mismatches.push(ReplayMismatch {
            task_id: task.id,
            field: "status".to_string(),
            replayed: "<no Created event>".to_string(),
            stored: format!("{:?}", task.status),
        }),
    }
    mismatches
}

/// `store` の全タスクについて `events` から再構築した状態と `tasks` テーブルを突き合わせる。
pub fn replay(store: &dyn TaskStore) -> Result<ReplayReport, OpsError> {
    let tasks = store.list(None)?;
    let mut all_mismatches = Vec::new();
    for task in &tasks {
        let events = store.events_for(task.id)?;
        all_mismatches.extend(diff_task(task, &events));
    }
    Ok(ReplayReport {
        tasks: tasks.len(),
        mismatches: all_mismatches,
    })
}

// ---------------------------------------------------------------------------
// ADR-0072 D5/D15（Phase E2b）: `work_units`/`runs` の再構築（events が正本、索引は派生。§(g) 後半）
// ---------------------------------------------------------------------------

/// `replay` が検出した 1 件の `work_units` の食い違い。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct WorkUnitMismatch {
    pub task_id: TaskId,
    pub key: String,
    /// 比較した欄の名前（`presence`/`status`/`runs`/… 下の `wu_field_pairs` 参照）。
    pub field: String,
    /// `events` から再構築した値。
    pub replayed: String,
    /// `work_units` テーブルに保存されている値。
    pub stored: String,
}

/// `replay` が検出した 1 件の `runs` の食い違い。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RunMismatch {
    pub task_id: TaskId,
    pub run_id: String,
    pub field: String,
    pub replayed: String,
    pub stored: String,
}

/// ADR-0072 D5/D17（Phase E4b 項目5）: `replay` が検出した 1 件の `execution_plans`（版の履歴）の
/// 食い違い。`version` で突き合わせる（`id` はフィールドの 1 つとして比較する。下記 `plan_field_pairs`
/// 参照。`Event::ExecutionPlanned.plan_id` が DB の `id` そのものなので、`work_units.id` と違って
/// events から確実に復元できる）。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ExecutionPlanMismatch {
    pub task_id: TaskId,
    pub version: u32,
    /// 比較した欄の名前（`presence`/`id`/`origin`/`status`/`spec_json`。下の `plan_field_pairs` 参照）。
    pub field: String,
    /// `events` から再構築した値。
    pub replayed: String,
    /// `execution_plans` テーブルに保存されている値。
    pub stored: String,
}

/// トポロジカル順（`work_units.seq`）だけを復元するために `validate` を「上限なし」で呼ぶ。計画は
/// 既に採用済み（＝一度 D14 の検証を通っている）ものを対象にするので、通常は必ず `Ok`。events が
/// 壊れている等で万一失敗しても、パニックせず元の並び順にフォールバックする。
fn topological_order(spec: &ExecutionPlanSpec) -> Vec<usize> {
    let permissive = ExecutionLimits {
        max_work_units: usize::MAX,
        work_unit_max_turns: u32::MAX,
        work_unit_max_wall_secs: u64::MAX,
        max_rationale_chars: usize::MAX,
        max_title_chars: usize::MAX,
        max_objective_chars: usize::MAX,
        max_done_when_items: usize::MAX,
        max_done_when_chars: usize::MAX,
        max_checks: usize::MAX,
        max_plan_json_bytes: usize::MAX,
        max_work_units_v2: usize::MAX,
        max_phases: usize::MAX,
    };
    match validate(spec, permissive, &[]) {
        Ok(v) => v.topological_order,
        Err(_) => (0..spec.work_units.len()).collect(),
    }
}

/// ADR-0072 D5/D15（Phase E2b）: `events` だけから `work_units`/`runs` の派生索引を作り直す（純粋関数、
/// I/O なし）。`ExecutionPlanned` が無いタスクは `work_units` を空にし、`runs` は Reviewer を含む
/// 全 run について作る（暗黙の WorkUnit。ADR-0072 D5「E2 以降、全タスクの run について書く」）。
///
/// 実装上の判断（`docs/adr/0072-task-execution-decomposition.md`「Phase E2b 実装時の逸脱・明確化」参照）:
/// - `work_units.id` は本来 `execution_plan_adopt` が発行する ULID だが、`Event::ExecutionPlanned` の
///   `plan.work_units` は `key` しか運ばない。最初にその WU が遷移した `WorkUnitTransitioned.work_unit_id`
///   から復元し、一度も遷移していない WU（実際にはほぼ起きない）だけ `rebuilt-<task_id>-<key>` を仮の
///   id にする。
/// - `work_units` の各カウンタ（`runs`/`continuations`/`retries`）は event 自体に差分が無いので、
///   `WorkUnitTransitioned.reason` の静的な文字列（`dispatch`/`continue`/`retry`/`completed`/…。
///   `task-dispatch::execution_scheduler` の decision と 1:1 対応する）から復元する。
/// - `created_at`/`updated_at`/`started_at`/`finished_at` は対応する `EventRow::ts` から作る。
///   scheduler が書く値は別の `OffsetDateTime::now_utc()` 呼び出し（event の追記とは別トランザクション。
///   `run_index_start`/`work_unit_transition` のコメント参照）なので厳密には一致しない。`diff_execution`
///   はこの 4 欄を比較対象にしない（下記）。
/// - `runs.session_id` と `work_units.last_checkpoint_run_id` は、現行の scheduler も常に `None` の
///   まま書いている（未配線。ADR-0072 D9(a) の wrap-up run、GUI の checkpoint 索引は未着手）ので、
///   ここも常に `None` にする。
pub fn rebuild_work_units_and_runs(
    task_id: TaskId,
    events: &[EventRow],
) -> (Vec<WorkUnitRow>, Vec<RunRow>) {
    let mut wu_rows: BTreeMap<String, WorkUnitRow> = BTreeMap::new();

    if let Some((plan_ts, plan_id, spec)) = events.iter().rev().find_map(|er| match &er.event {
        Event::ExecutionPlanned { plan_id, plan, .. } => {
            Some((er.ts.clone(), plan_id.clone(), (**plan).clone()))
        }
        _ => None,
    }) {
        let mut key_to_id: BTreeMap<String, String> = BTreeMap::new();
        for er in events {
            if let Event::WorkUnitTransitioned {
                work_unit_id, key, ..
            } = &er.event
            {
                key_to_id
                    .entry(key.clone())
                    .or_insert_with(|| work_unit_id.clone());
            }
        }
        // ADR-0074 D1.4（Phase F2b）: v2 は採用時と同じ規則（`materialize_work_units`）で統合 WU を
        // 足し、工程の障壁つきで初期状態を決める（v1 は従来と同じ結果）。
        let order = topological_order(&spec);
        for row in task_core::materialize_work_units(
            &task_id.to_string(),
            &plan_id,
            &spec,
            &order,
            &plan_ts,
            &mut |w| {
                key_to_id
                    .get(&w.key)
                    .cloned()
                    .unwrap_or_else(|| format!("rebuilt-{task_id}-{}", w.key))
            },
        ) {
            wu_rows.insert(row.id.clone(), row);
        }
    }
    // ADR-0074 D1.4: 統合の repair（`RepairScheduled{origin: integration}`）の直前に統合 WU が
    // pending に戻った（`merge_conflict` / `integration_check_failed`）なら、その統合 WU は repair に依存する。
    let mut pending_integration: Option<String> = None;

    let mut run_order: Vec<String> = Vec::new();
    let mut runs: BTreeMap<String, RunRow> = BTreeMap::new();
    // ADR-0072 D5/E1「run_seq は events から純粋に導出する」と同じ考え方: reviewer は別の連番
    // （`task_ops::derive::current_run_seq` が worker run だけを数えるのと対称）。
    let mut worker_seq = 0u32;
    let mut reviewer_seq = 0u32;

    for er in events {
        match &er.event {
            Event::WorkUnitTransitioned {
                work_unit_id,
                to,
                reason,
                run_id,
                ..
            } => {
                let Some(wu) = wu_rows.get_mut(work_unit_id) else {
                    continue;
                };
                if matches!(
                    reason.as_str(),
                    "merge_conflict" | "integration_check_failed"
                ) {
                    pending_integration = Some(work_unit_id.clone());
                }
                let blocked_reason = if *to == WorkUnitStatus::Blocked {
                    match reason.as_str() {
                        "question" | "integration_failed" => Some(WorkUnitBlockedReason::Question),
                        "plan_issue" => Some(WorkUnitBlockedReason::PlanIssue),
                        "dependency_failed" => Some(WorkUnitBlockedReason::DependencyFailed),
                        "limit" => Some(WorkUnitBlockedReason::Limit),
                        _ => wu.blocked_reason,
                    }
                } else {
                    None
                };
                wu.status = *to;
                wu.blocked_reason = blocked_reason;
                match reason.as_str() {
                    "dispatch" => {
                        wu.runs += 1;
                        wu.updated_at = er.ts.clone();
                        if let Some(rid) = run_id {
                            wu.last_run_id = Some(rid.clone());
                            if let Some(r) = runs.get_mut(rid) {
                                r.work_unit_id = Some(work_unit_id.clone());
                                r.seq = wu.runs;
                            }
                        }
                    }
                    "continue" => wu.continuations += 1,
                    "retry" => wu.retries += 1,
                    "completed" => {
                        if let Some(rid) = run_id {
                            wu.last_run_id = Some(rid.clone());
                        }
                    }
                    // "question" / "limit" / "failed" / "harness_error" / "dependency_failed" /
                    // "dependency_ready" / "restart_reconcile" / "answer": カウンタは動かさない
                    // （ADR-0072 D6 の表どおり。`crate::execution_scheduler` の decision と対応）。
                    _ => {}
                }
            }
            Event::WorkerStarted {
                run_id,
                adapter,
                model,
                account,
                role,
                ..
            } => {
                let is_reviewer = matches!(role, Some(RunRole::Reviewer));
                let seq = if is_reviewer {
                    reviewer_seq += 1;
                    reviewer_seq
                } else {
                    worker_seq += 1;
                    worker_seq
                };
                let row = RunRow {
                    run_id: run_id.clone(),
                    task_id: task_id.to_string(),
                    // "dispatch" の WorkUnitTransitioned（WorkerStarted の直後に来る）が来れば埋める。
                    work_unit_id: None,
                    role: if is_reviewer {
                        RunIndexRole::Reviewer
                    } else {
                        RunIndexRole::Worker
                    },
                    seq,
                    status: RunIndexStatus::Running,
                    adapter: Some(adapter.clone()),
                    model: Some(model.clone()),
                    account: account.clone(),
                    session_id: None,
                    checkpoint: None,
                    usage: None,
                    metrics: None,
                    started_at: er.ts.clone(),
                    finished_at: None,
                };
                run_order.push(run_id.clone());
                runs.insert(run_id.clone(), row);
            }
            Event::CheckpointSaved {
                run_id, checkpoint, ..
            } => {
                if let Some(r) = runs.get_mut(run_id) {
                    r.checkpoint = Some((**checkpoint).clone());
                }
            }
            // ADR-0074 D1.2（Phase F2b）: WU の commit（WU のブランチ・基点・HEAD の正本）。
            Event::WorkUnitCommitted {
                work_unit_id,
                branch,
                base,
                commit,
                ..
            } => {
                if let Some(wu) = wu_rows.get_mut(work_unit_id) {
                    if branch.starts_with("celeris-wu/") {
                        wu.branch = Some(branch.clone());
                        wu.base_commit = base.clone();
                    }
                    wu.head_commit = Some(commit.clone());
                }
            }
            // ADR-0074 D1.4: 統合後の Task ブランチの HEAD。
            Event::PhaseIntegrated {
                work_unit_id, head, ..
            } => {
                if let Some(wu) = wu_rows.get_mut(work_unit_id)
                    && !head.is_empty()
                {
                    wu.integrated_commit = Some(head.clone());
                }
            }
            Event::RepairScheduled {
                key,
                origin: task_core::execution::RepairOrigin::Integration,
                ..
            } => {
                if let Some(integ_id) = pending_integration.take()
                    && let Some(integ) = wu_rows.get_mut(&integ_id)
                    && !integ.depends_on.contains(key)
                {
                    integ.depends_on.push(key.clone());
                    integ.spec.depends_on.push(key.clone());
                }
            }
            Event::WorkerFinished {
                run_id,
                usage,
                metrics,
                end,
                ..
            } => {
                if let Some(r) = runs.get_mut(run_id) {
                    let end: Option<task_core::RunEnd> = *end;
                    r.status = end
                        .map(RunIndexStatus::from_run_end)
                        .unwrap_or(RunIndexStatus::HarnessError);
                    r.usage = *usage;
                    r.metrics = *metrics;
                    r.finished_at = Some(er.ts.clone());
                }
            }
            _ => {}
        }
    }

    let mut wu_out: Vec<WorkUnitRow> = wu_rows.into_values().collect();
    wu_out.sort_by_key(|w| w.seq);
    let run_out: Vec<RunRow> = run_order
        .into_iter()
        .filter_map(|id| runs.remove(&id))
        .collect();
    (wu_out, run_out)
}

/// ADR-0072 D5/D17（Phase E4b 項目5、E2b からの持ち越し）: `execution_plans`（版の履歴）を
/// `Event::ExecutionPlanned` だけから再構築する（純粋関数）。E2b は「E2 の範囲では replan が無く
/// 事実上 1 タスク 1 行」として対象外にしていたが、E4 で replan が入り複数版になりうる。
///
/// - `id`/`version`/`origin`/`spec` はそのままイベントの値（`plan_id` は `adopt_plan`/`replan` が
///   `new_id()` で発行し、行と event の両方に同じ値を書くので、`work_units.id`（`key` しか運ばれない）
///   と違って events から確実に復元できる）。
/// - `status`/`superseded_at`: 版を時系列に畳み込み、後続の `ExecutionPlanned.supersedes` に
///   名指しされた版を `superseded`（そのイベントの `EventRow::ts` を `superseded_at` に）にする。
///   最後まで supersede されなかった版が `active`。`execution_plan_adopt`/`execution_plan_replan`
///   は常にこの 2 状態しか書かない（`PlanStatus::Completed`/`Abandoned` は現行の実装では未使用）ので、
///   これで実際の scheduler の書き方と一致する。
/// - `created_at`: 自分の `EventRow::ts`。
/// - `planner_run_id`: `Event::ExecutionPlanned` 自体はこれを運ばない（D5 の event 表を参照。
///   `plan_id`/`version`/`origin`/`supersedes`/`reason`/`plan` の 6 欄のみ）ので、events だけからは
///   確実に復元できない。常に `None` にし、[`diff_execution_plans`] の比較対象からも外す
///   （`work_units`/`runs` の `created_at`/`updated_at`/`last_checkpoint_run_id` と同じ「タイムスタンプ
///   級で厳密には復元できない欄は比較しない」という規則の延長。`rebuild_work_units_and_runs` の
///   ドキュメント参照）。
pub fn rebuild_execution_plans(task_id: TaskId, events: &[EventRow]) -> Vec<ExecutionPlanRow> {
    let mut rows: Vec<ExecutionPlanRow> = Vec::new();
    for er in events {
        let Event::ExecutionPlanned {
            plan_id,
            version,
            origin,
            supersedes,
            plan,
            ..
        } = &er.event
        else {
            continue;
        };
        if let Some(superseded_id) = supersedes
            && let Some(prev) = rows.iter_mut().find(|p| &p.id == superseded_id)
        {
            prev.status = PlanStatus::Superseded;
            prev.superseded_at = Some(er.ts.clone());
        }
        rows.push(ExecutionPlanRow {
            id: plan_id.clone(),
            task_id: task_id.to_string(),
            version: *version,
            origin: *origin,
            planner_run_id: None,
            status: PlanStatus::Active,
            spec: (**plan).clone(),
            created_at: er.ts.clone(),
            superseded_at: None,
        });
    }
    rows
}

/// 比較対象の欄と文字列表現（id そのもの、および `created_at`/`updated_at`/`last_checkpoint_run_id`
/// は除く。理由は [`rebuild_work_units_and_runs`] のドキュメント参照）。
fn wu_field_pairs(w: &WorkUnitRow) -> Vec<(&'static str, String)> {
    vec![
        ("plan_id", w.plan_id.clone()),
        ("seq", w.seq.to_string()),
        ("kind", w.kind.as_str().to_string()),
        ("status", w.status.as_str().to_string()),
        (
            "blocked_reason",
            w.blocked_reason
                .map(|r| r.as_str().to_string())
                .unwrap_or_else(|| "none".to_string()),
        ),
        ("depends_on", w.depends_on.join(",")),
        ("runs", w.runs.to_string()),
        ("continuations", w.continuations.to_string()),
        ("retries", w.retries.to_string()),
        (
            "last_run_id",
            w.last_run_id.clone().unwrap_or_else(|| "none".to_string()),
        ),
        (
            "spec_json",
            serde_json::to_string(&w.spec).unwrap_or_default(),
        ),
        // ADR-0074 D1（Phase F2b）: events から復元できる v2 の列（lease は揮発なので比べない。
        // `branch`/`base_commit` は WU の worktree を切った時点で行に入り、events には commit の
        // ときに初めて出るので比べない）。
        (
            "phase",
            w.phase.clone().unwrap_or_else(|| "none".to_string()),
        ),
        (
            "head_commit",
            w.head_commit.clone().unwrap_or_else(|| "none".to_string()),
        ),
        (
            "integrated_commit",
            w.integrated_commit
                .clone()
                .unwrap_or_else(|| "none".to_string()),
        ),
    ]
}

/// 比較対象の欄（id・`session_id`・`started_at`/`finished_at` は除く）。
fn run_field_pairs(r: &RunRow) -> Vec<(&'static str, String)> {
    vec![
        (
            "work_unit_id",
            r.work_unit_id.clone().unwrap_or_else(|| "none".to_string()),
        ),
        ("role", r.role.as_str().to_string()),
        ("seq", r.seq.to_string()),
        ("status", r.status.as_str().to_string()),
        (
            "adapter",
            r.adapter.clone().unwrap_or_else(|| "none".to_string()),
        ),
        (
            "model",
            r.model.clone().unwrap_or_else(|| "none".to_string()),
        ),
        (
            "account",
            r.account.clone().unwrap_or_else(|| "none".to_string()),
        ),
        (
            "checkpoint_json",
            r.checkpoint
                .as_ref()
                .map(|c| serde_json::to_string(c).unwrap_or_default())
                .unwrap_or_else(|| "none".to_string()),
        ),
        (
            "usage_json",
            r.usage
                .as_ref()
                .map(|u| serde_json::to_string(u).unwrap_or_default())
                .unwrap_or_else(|| "none".to_string()),
        ),
        (
            "metrics_json",
            r.metrics
                .as_ref()
                .map(|m| serde_json::to_string(m).unwrap_or_default())
                .unwrap_or_else(|| "none".to_string()),
        ),
    ]
}

/// `replayed`/`stored` を `key`（`work_units`）/`run_id`（`runs`）で突き合わせ、業務上意味のある欄の
/// 食い違いを返す（[`rebuild_work_units_and_runs`] のドキュメントに書いた除外欄を除く）。
pub fn diff_execution(
    task_id: TaskId,
    replayed_units: &[WorkUnitRow],
    stored_units: &[WorkUnitRow],
    replayed_runs: &[RunRow],
    stored_runs: &[RunRow],
) -> (Vec<WorkUnitMismatch>, Vec<RunMismatch>) {
    let mut wu_mismatches = Vec::new();
    let stored_wu_by_key: BTreeMap<&str, &WorkUnitRow> =
        stored_units.iter().map(|u| (u.key.as_str(), u)).collect();
    let mut seen_keys: BTreeSet<&str> = BTreeSet::new();
    for r in replayed_units {
        seen_keys.insert(r.key.as_str());
        match stored_wu_by_key.get(r.key.as_str()) {
            None => wu_mismatches.push(WorkUnitMismatch {
                task_id,
                key: r.key.clone(),
                field: "presence".to_string(),
                replayed: "present".to_string(),
                stored: "<missing>".to_string(),
            }),
            Some(s) => {
                for ((field, rv), (_, sv)) in wu_field_pairs(r).into_iter().zip(wu_field_pairs(s)) {
                    if rv != sv {
                        wu_mismatches.push(WorkUnitMismatch {
                            task_id,
                            key: r.key.clone(),
                            field: field.to_string(),
                            replayed: rv,
                            stored: sv,
                        });
                    }
                }
            }
        }
    }
    for s in stored_units {
        if !seen_keys.contains(s.key.as_str()) {
            wu_mismatches.push(WorkUnitMismatch {
                task_id,
                key: s.key.clone(),
                field: "presence".to_string(),
                replayed: "<missing>".to_string(),
                stored: "present".to_string(),
            });
        }
    }

    let mut run_mismatches = Vec::new();
    let stored_run_by_id: BTreeMap<&str, &RunRow> =
        stored_runs.iter().map(|r| (r.run_id.as_str(), r)).collect();
    let mut seen_runs: BTreeSet<&str> = BTreeSet::new();
    for r in replayed_runs {
        seen_runs.insert(r.run_id.as_str());
        match stored_run_by_id.get(r.run_id.as_str()) {
            None => run_mismatches.push(RunMismatch {
                task_id,
                run_id: r.run_id.clone(),
                field: "presence".to_string(),
                replayed: "present".to_string(),
                stored: "<missing>".to_string(),
            }),
            Some(s) => {
                for ((field, rv), (_, sv)) in run_field_pairs(r).into_iter().zip(run_field_pairs(s))
                {
                    if rv != sv {
                        run_mismatches.push(RunMismatch {
                            task_id,
                            run_id: r.run_id.clone(),
                            field: field.to_string(),
                            replayed: rv,
                            stored: sv,
                        });
                    }
                }
            }
        }
    }
    for s in stored_runs {
        if !seen_runs.contains(s.run_id.as_str()) {
            run_mismatches.push(RunMismatch {
                task_id,
                run_id: s.run_id.clone(),
                field: "presence".to_string(),
                replayed: "<missing>".to_string(),
                stored: "present".to_string(),
            });
        }
    }

    (wu_mismatches, run_mismatches)
}

/// 比較対象の欄と文字列表現。`created_at`/`superseded_at`/`planner_run_id` は除く（理由は
/// [`rebuild_execution_plans`] のドキュメント参照）。`id` は比較に含める（`work_units.id` と違って
/// events から確実に復元できるため。同ドキュメント参照）。
fn plan_field_pairs(p: &ExecutionPlanRow) -> Vec<(&'static str, String)> {
    vec![
        ("id", p.id.clone()),
        ("origin", p.origin.as_str().to_string()),
        ("status", p.status.as_str().to_string()),
        (
            "spec_json",
            serde_json::to_string(&p.spec).unwrap_or_default(),
        ),
    ]
}

/// ADR-0072 D5/D17（Phase E4b 項目5）: `replayed`/`stored` を `version` で突き合わせ、業務上意味の
/// ある欄の食い違いを返す（[`rebuild_execution_plans`] のドキュメントに書いた除外欄を除く）。
/// [`diff_execution`] と対になる。
pub fn diff_execution_plans(
    task_id: TaskId,
    replayed: &[ExecutionPlanRow],
    stored: &[ExecutionPlanRow],
) -> Vec<ExecutionPlanMismatch> {
    let mut mismatches = Vec::new();
    let stored_by_version: BTreeMap<u32, &ExecutionPlanRow> =
        stored.iter().map(|p| (p.version, p)).collect();
    let mut seen_versions: BTreeSet<u32> = BTreeSet::new();
    for r in replayed {
        seen_versions.insert(r.version);
        match stored_by_version.get(&r.version) {
            None => mismatches.push(ExecutionPlanMismatch {
                task_id,
                version: r.version,
                field: "presence".to_string(),
                replayed: "present".to_string(),
                stored: "<missing>".to_string(),
            }),
            Some(s) => {
                for ((field, rv), (_, sv)) in
                    plan_field_pairs(r).into_iter().zip(plan_field_pairs(s))
                {
                    if rv != sv {
                        mismatches.push(ExecutionPlanMismatch {
                            task_id,
                            version: r.version,
                            field: field.to_string(),
                            replayed: rv,
                            stored: sv,
                        });
                    }
                }
            }
        }
    }
    for s in stored {
        if !seen_versions.contains(&s.version) {
            mismatches.push(ExecutionPlanMismatch {
                task_id,
                version: s.version,
                field: "presence".to_string(),
                replayed: "<missing>".to_string(),
                stored: "present".to_string(),
            });
        }
    }
    mismatches
}

/// [`check_and_apply_execution`] の戻り値: `(work_unit_mismatches, run_mismatches,
/// execution_plan_mismatches, applied_task_count)`。
pub type ExecutionCheckReport = (
    Vec<WorkUnitMismatch>,
    Vec<RunMismatch>,
    Vec<ExecutionPlanMismatch>,
    usize,
);

/// `celerisctl replay --check`/`--apply`（ADR-0072 D15: 索引と events が食い違えば events が勝つ）:
/// 全タスクについて [`rebuild_work_units_and_runs`]/[`rebuild_execution_plans`] と現在の索引を
/// 突き合わせ、`apply` が true なら食い違ったタスクの `execution_plans`/`work_units`/`runs` を
/// 再構築結果で上書きする（`events` は変えない）。`apply` して直したタスクの分は mismatch に
/// 含めない（3 つの表のうちどれか 1 つでも食い違えば、そのタスクの 3 つとも書き直す。
/// `execution_plans`/`work_units`/`runs` は互いに `plan_id`/`work_unit_id` で参照し合うため、
/// 一部だけ書き直すと整合が崩れる）。
pub fn check_and_apply_execution(
    store: &dyn TaskStore,
    apply: bool,
) -> Result<ExecutionCheckReport, OpsError> {
    let tasks = store.list(None)?;
    let mut wu_mismatches = Vec::new();
    let mut run_mismatches = Vec::new();
    let mut plan_mismatches = Vec::new();
    let mut applied = 0usize;
    for task in &tasks {
        let events = store.event_rows_for(task.id, None, usize::MAX)?;
        let (rebuilt_units, rebuilt_runs) = rebuild_work_units_and_runs(task.id, &events);
        let mut rebuilt_plans = rebuild_execution_plans(task.id, &events);
        let stored_units = store.work_units_for(task.id)?;
        let stored_runs = store.runs_for_task(task.id)?;
        let stored_plans = store.execution_plan_list(task.id)?;
        // `planner_run_id` は events から復元できない（rebuild のドキュメント参照）ので、`--apply`
        // で書き戻すときは既存の行から引き継ぐ（比較はしないが、既にある値を消したくない）。
        let stored_plan_by_id: BTreeMap<&str, &ExecutionPlanRow> =
            stored_plans.iter().map(|p| (p.id.as_str(), p)).collect();
        for p in &mut rebuilt_plans {
            if let Some(s) = stored_plan_by_id.get(p.id.as_str()) {
                p.planner_run_id = s.planner_run_id.clone();
            }
        }
        let (mut u_mm, mut r_mm) = diff_execution(
            task.id,
            &rebuilt_units,
            &stored_units,
            &rebuilt_runs,
            &stored_runs,
        );
        let mut p_mm = diff_execution_plans(task.id, &rebuilt_plans, &stored_plans);
        if apply && (!u_mm.is_empty() || !r_mm.is_empty() || !p_mm.is_empty()) {
            store.execution_plans_replace(task.id, rebuilt_plans)?;
            store.work_units_replace(task.id, rebuilt_units)?;
            store.runs_replace(task.id, rebuilt_runs)?;
            applied += 1;
            u_mm.clear();
            r_mm.clear();
            p_mm.clear();
        }
        wu_mismatches.append(&mut u_mm);
        run_mismatches.append(&mut r_mm);
        plan_mismatches.append(&mut p_mm);
    }
    Ok((wu_mismatches, run_mismatches, plan_mismatches, applied))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use task_core::{
        ArtifactRef, Budget, Check, Criterion, SqliteStore, TaskKind, Tier, Trigger, WorkerHint,
        WorkspaceSpec,
    };
    use time::OffsetDateTime;

    fn sample_task(status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".to_string(),
            objective: "o".to_string(),
            acceptance: vec![Criterion {
                text: "x".to_string(),
                check: Check::Human,
            }],
            inputs: vec![ArtifactRef {
                name: "n".to_string(),
                path: "p".to_string(),
                sha256: "s".to_string(),
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
                path: PathBuf::from("/tmp/ws"),
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

    #[test]
    fn replay_reports_zero_mismatches_when_events_match_current_state() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::Created {
                    task: Box::new(task.clone()),
                },
            )
            .expect("append created");
        store
            .apply_transition(task.id, Trigger::Accept, None)
            .expect("accept");

        let report = replay(&store).expect("replay");
        assert!(report.mismatches.is_empty(), "expected no mismatches");
        assert_eq!(report.tasks, 1);
    }

    #[test]
    fn replay_detects_status_drift_from_events() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::Created {
                    task: Box::new(task.clone()),
                },
            )
            .expect("append created");

        // tasks 側は insert 時点の Draft のまま更新せず、events だけに Transitioned を
        // 追記する（append_event は tasks 行を更新しないため、これだけで drift が作れる）。
        store
            .append_event(
                task.id,
                &Event::Transitioned {
                    from: Status::Draft,
                    to: Status::Ready,
                    reason: "worker_error".to_string(),
                },
            )
            .expect("append transitioned");

        let report = replay(&store).expect("replay");
        // tasks.status は insert 時点の Draft のまま（イベントは追記しただけで
        // tasks 行を更新していない）なので、replay 側の Ready と食い違う。
        assert!(report.mismatches.iter().any(|m| m.field == "status"));
        assert!(report.mismatches.iter().any(|m| m.field == "attempts"));
    }

    #[test]
    fn replay_counts_retry_reasons_into_attempts() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Running);
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::Created {
                    task: Box::new(task.clone()),
                },
            )
            .expect("append created");
        store
            .append_event(
                task.id,
                &Event::Transitioned {
                    from: Status::Running,
                    to: Status::Ready,
                    reason: "worker_error".to_string(),
                },
            )
            .expect("append 1");
        store
            .append_event(
                task.id,
                &Event::Transitioned {
                    from: Status::Ready,
                    to: Status::Running,
                    reason: "dispatch".to_string(),
                },
            )
            .expect("append 2");

        let events = store.events_for(task.id).expect("events_for");
        let (status, attempts) = replay_status_and_attempts(&events).expect("some state");
        assert_eq!(status, Status::Running);
        assert_eq!(attempts, 1, "dispatch はリトライ回数を増やさない");
    }

    /// ADR-0016 D3: `Trigger::Aggregate`（reviewing -> ready, `reason: "aggregate"`）は attempts を増やさない。
    #[test]
    fn replay_aggregate_transition_does_not_bump_attempts() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Reviewing);
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::Created {
                    task: Box::new(task.clone()),
                },
            )
            .expect("append created");
        store
            .append_event(
                task.id,
                &Event::Transitioned {
                    from: Status::Reviewing,
                    to: Status::Ready,
                    reason: "aggregate".to_string(),
                },
            )
            .expect("append transitioned");

        let events = store.events_for(task.id).expect("events_for");
        let (status, attempts) = replay_status_and_attempts(&events).expect("some state");
        assert_eq!(status, Status::Ready);
        assert_eq!(attempts, 0, "aggregate はリトライ回数を増やさない");
    }

    /// ADR-0021 D1: `child_failed` は **ready に戻るときだけ** attempts を使う（blocked は人の判断待ちなので据え置き）。
    #[test]
    fn replay_child_failed_bumps_attempts_only_when_it_retries() {
        let replayed = |to: Status| {
            let store = SqliteStore::open_in_memory().expect("open");
            let task = sample_task(Status::Reviewing);
            store.insert(&task).expect("insert");
            store
                .append_event(
                    task.id,
                    &Event::Created {
                        task: Box::new(task.clone()),
                    },
                )
                .expect("append created");
            store
                .append_event(
                    task.id,
                    &Event::Transitioned {
                        from: Status::Reviewing,
                        to,
                        reason: "child_failed".to_string(),
                    },
                )
                .expect("append transitioned");
            let events = store.events_for(task.id).expect("events_for");
            replay_status_and_attempts(&events).expect("some state")
        };
        assert_eq!(
            replayed(Status::Ready),
            (Status::Ready, 1),
            "やり直しは attempts を使う"
        );
        assert_eq!(
            replayed(Status::Blocked),
            (Status::Blocked, 0),
            "人に聞くときは使わない"
        );
    }

    /// ADR-0044 D2（Phase 53）: `reopen` は attempts を **0 に戻す**唯一のトリガ。
    /// 畳み込みがこれを知らないと、再開したタスクは毎回 `attempts` の不一致として報告され続ける
    /// （ADR-0004 D6 の不変条件の検査が狼少年になる。Phase 53 の監査で発見）。
    #[test]
    fn replay_follows_reopen_back_to_zero_attempts() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut task = sample_task(Status::Draft);
        task.budget.max_retries = 0;
        store
            .create_task(
                &task,
                vec![Event::Created {
                    task: Box::new(task.clone()),
                }],
            )
            .expect("create");
        // draft → ready → running → failed（attempts を 1 使う）。
        store
            .apply_transition(task.id, Trigger::Accept, None)
            .expect("accept");
        store
            .apply_transition(task.id, Trigger::Dispatch, None)
            .expect("dispatch");
        store
            .apply_transition(task.id, Trigger::WorkerError { retryable: false }, None)
            .expect("worker_error");
        assert_eq!(store.get(task.id).expect("get").expect("some").attempts, 1);

        // 人が再開する（attempts は 0 に戻る）。
        crate::comment::reopen(&store, task.id, None).expect("reopen");
        let stored = store.get(task.id).expect("get").expect("some");
        assert_eq!((stored.status, stored.attempts), (Status::Ready, 0));

        // `celerisctl replay` は不一致を報告しない。
        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(
            replay_status_and_attempts(&events),
            Some((Status::Ready, 0))
        );
        let report = replay(&store).expect("replay");
        assert_eq!(report.mismatches, Vec::new(), "{report:?}");
    }

    // -----------------------------------------------------------------------
    // ADR-0072 D5/D15（Phase E2b）: `rebuild_work_units_and_runs` / `diff_execution`
    // -----------------------------------------------------------------------

    /// `created_at`/`updated_at`/`started_at`/`finished_at` は `diff_execution` の比較対象外
    /// （[`rebuild_work_units_and_runs`] のドキュメント参照）なので、テストでは固定値でよい。
    const TS: &str = "2026-09-24T00:00:00Z";

    fn wu_spec(key: &str, depends_on: &[&str]) -> task_core::WorkUnitSpec {
        task_core::WorkUnitSpec {
            key: key.to_string(),
            kind: task_core::WorkUnitKind::Implement,
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

    fn worker_started(run_id: &str, role: Option<RunRole>) -> Event {
        Event::WorkerStarted {
            run_id: run_id.to_string(),
            adapter: "claude-code".to_string(),
            model: "test-model".to_string(),
            provider: None,
            account: None,
            role,
            task_role: None,
        }
    }

    fn worker_finished(run_id: &str, end: task_core::RunEnd) -> Event {
        Event::WorkerFinished {
            run_id: run_id.to_string(),
            outcome: "outcome".to_string(),
            usage: None,
            role: None,
            metrics: None,
            end: Some(end),
        }
    }

    fn sample_checkpoint(work_unit: &str, run_seq: u32) -> task_core::Checkpoint {
        task_core::Checkpoint {
            schema: task_core::CHECKPOINT_SCHEMA.into(),
            task_id: "t".into(),
            work_unit: Some(work_unit.into()),
            run_id: "r".into(),
            run_seq,
            end: task_core::CheckpointEnd::BudgetExhausted,
            source: task_core::CheckpointSource::Mechanical,
            completed: vec!["did something".into()],
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
            created_at: TS.to_string(),
        }
    }

    fn run_row(
        run_id: &str,
        task_id: TaskId,
        work_unit_id: &str,
        seq: u32,
        status: RunIndexStatus,
    ) -> RunRow {
        RunRow {
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            work_unit_id: Some(work_unit_id.to_string()),
            role: RunIndexRole::Worker,
            seq,
            status,
            adapter: Some("claude-code".to_string()),
            model: Some("test-model".to_string()),
            account: None,
            session_id: None,
            checkpoint: None,
            usage: None,
            metrics: None,
            started_at: TS.to_string(),
            finished_at: None,
        }
    }

    fn wu_transitioned(
        wu: &WorkUnitRow,
        from: WorkUnitStatus,
        to: WorkUnitStatus,
        reason: &str,
        run_id: Option<&str>,
    ) -> Event {
        Event::WorkUnitTransitioned {
            work_unit_id: wu.id.clone(),
            key: wu.key.clone(),
            from,
            to,
            reason: reason.to_string(),
            run_id: run_id.map(str::to_string),
        }
    }

    /// (1) E2 の fixture（A → B → C の依存順、B の retry → 質問 → 回答 → 最終的な失敗、A の
    /// continuation、C への `dependency_failed` の伝播）で、scheduler が書くのと同じ手順で
    /// `work_units`/`runs` の索引を手で書き（`store.work_unit_transition`/`run_index_start`/
    /// `run_index_finish` を dispatcher.rs と同じ呼び方で使う）、`rebuild_work_units_and_runs`
    /// が events だけからそれと一致する行を再構築できることを確かめる。
    #[test]
    fn rebuild_work_units_and_runs_matches_the_scheduler_written_index_for_the_e2_fixtures() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Running);
        store.insert(&task).expect("insert");
        let now = OffsetDateTime::now_utc();

        let spec = ExecutionPlanSpec {
            schema: task_core::EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "A -> B -> C".to_string(),
            work_units: vec![
                wu_spec("a", &[]),
                wu_spec("b", &["a"]),
                wu_spec("c", &["b"]),
            ],
            phases: Vec::new(),
            children: Vec::new(),
        };
        crate::execution::adopt_plan(
            &store,
            task.id,
            spec,
            task_core::PlanOrigin::Fixture,
            None,
            ExecutionLimits::default(),
            now,
        )
        .expect("adopt plan");

        let units = store.work_units_for(task.id).expect("units");
        let a = units.iter().find(|u| u.key == "a").expect("a").clone();
        let b = units.iter().find(|u| u.key == "b").expect("b").clone();
        let c = units.iter().find(|u| u.key == "c").expect("c").clone();

        // --- a: dispatch(r1) -> BudgetExhausted(継続) -> dispatch(r2) -> Completed ---
        store
            .append_event(task.id, &worker_started("r1", None))
            .unwrap();
        let mut a_row = a.clone();
        a_row.status = WorkUnitStatus::Running;
        a_row.runs = 1;
        a_row.last_run_id = Some("r1".into());
        store
            .work_unit_transition(
                task.id,
                a_row.clone(),
                wu_transitioned(
                    &a,
                    WorkUnitStatus::Ready,
                    WorkUnitStatus::Running,
                    "dispatch",
                    Some("r1"),
                ),
            )
            .unwrap();
        store
            .run_index_start(run_row("r1", task.id, &a.id, 1, RunIndexStatus::Running))
            .unwrap();

        let cp1 = sample_checkpoint("a", 1);
        store
            .append_event(
                task.id,
                &worker_finished(
                    "r1",
                    task_core::RunEnd::BudgetExhausted {
                        kind: task_core::BudgetKind::Turns,
                    },
                ),
            )
            .unwrap();
        store
            .append_event(
                task.id,
                &Event::CheckpointSaved {
                    run_id: "r1".into(),
                    work_unit_id: Some(a.id.clone()),
                    checkpoint: Box::new(cp1.clone()),
                },
            )
            .unwrap();
        a_row.status = WorkUnitStatus::NeedsContinuation;
        a_row.continuations = 1;
        store
            .work_unit_transition(
                task.id,
                a_row.clone(),
                wu_transitioned(
                    &a_row,
                    WorkUnitStatus::Running,
                    WorkUnitStatus::NeedsContinuation,
                    "continue",
                    Some("r1"),
                ),
            )
            .unwrap();
        store
            .run_index_finish(
                "r1",
                RunIndexStatus::BudgetExhausted,
                Some(cp1),
                None,
                None,
                now,
            )
            .unwrap();

        store
            .append_event(task.id, &worker_started("r2", None))
            .unwrap();
        a_row.status = WorkUnitStatus::Running;
        a_row.runs = 2;
        a_row.last_run_id = Some("r2".into());
        store
            .work_unit_transition(
                task.id,
                a_row.clone(),
                wu_transitioned(
                    &a_row,
                    WorkUnitStatus::NeedsContinuation,
                    WorkUnitStatus::Running,
                    "dispatch",
                    Some("r2"),
                ),
            )
            .unwrap();
        store
            .run_index_start(run_row("r2", task.id, &a.id, 2, RunIndexStatus::Running))
            .unwrap();

        store
            .append_event(
                task.id,
                &worker_finished("r2", task_core::RunEnd::Completed),
            )
            .unwrap();
        a_row.status = WorkUnitStatus::Done;
        store
            .work_unit_transition(
                task.id,
                a_row.clone(),
                wu_transitioned(
                    &a_row,
                    WorkUnitStatus::Running,
                    WorkUnitStatus::Done,
                    "completed",
                    Some("r2"),
                ),
            )
            .unwrap();
        let mut b_row = b.clone();
        b_row.status = WorkUnitStatus::Ready;
        store
            .work_unit_transition(
                task.id,
                b_row.clone(),
                wu_transitioned(
                    &b,
                    WorkUnitStatus::Pending,
                    WorkUnitStatus::Ready,
                    "dependency_ready",
                    None,
                ),
            )
            .unwrap();
        store
            .run_index_finish("r2", RunIndexStatus::Completed, None, None, None, now)
            .unwrap();

        // --- b: dispatch(r3) -> retryable failure(retry) -> dispatch(r4) -> Question -> answer
        //     -> dispatch(r5) -> 非 retryable failure(failed) ---
        store
            .append_event(task.id, &worker_started("r3", None))
            .unwrap();
        b_row.status = WorkUnitStatus::Running;
        b_row.runs = 1;
        b_row.last_run_id = Some("r3".into());
        store
            .work_unit_transition(
                task.id,
                b_row.clone(),
                wu_transitioned(
                    &b_row,
                    WorkUnitStatus::Ready,
                    WorkUnitStatus::Running,
                    "dispatch",
                    Some("r3"),
                ),
            )
            .unwrap();
        store
            .run_index_start(run_row("r3", task.id, &b.id, 1, RunIndexStatus::Running))
            .unwrap();

        store
            .append_event(
                task.id,
                &worker_finished("r3", task_core::RunEnd::Failed { retryable: true }),
            )
            .unwrap();
        b_row.status = WorkUnitStatus::Ready;
        b_row.retries = 1;
        store
            .work_unit_transition(
                task.id,
                b_row.clone(),
                wu_transitioned(
                    &b_row,
                    WorkUnitStatus::Running,
                    WorkUnitStatus::Ready,
                    "retry",
                    Some("r3"),
                ),
            )
            .unwrap();
        store
            .run_index_finish("r3", RunIndexStatus::Failed, None, None, None, now)
            .unwrap();

        store
            .append_event(task.id, &worker_started("r4", None))
            .unwrap();
        b_row.status = WorkUnitStatus::Running;
        b_row.runs = 2;
        b_row.last_run_id = Some("r4".into());
        store
            .work_unit_transition(
                task.id,
                b_row.clone(),
                wu_transitioned(
                    &b_row,
                    WorkUnitStatus::Ready,
                    WorkUnitStatus::Running,
                    "dispatch",
                    Some("r4"),
                ),
            )
            .unwrap();
        store
            .run_index_start(run_row("r4", task.id, &b.id, 2, RunIndexStatus::Running))
            .unwrap();

        store
            .append_event(task.id, &worker_finished("r4", task_core::RunEnd::Question))
            .unwrap();
        b_row.status = WorkUnitStatus::Blocked;
        b_row.blocked_reason = Some(task_core::WorkUnitBlockedReason::Question);
        store
            .work_unit_transition(
                task.id,
                b_row.clone(),
                wu_transitioned(
                    &b_row,
                    WorkUnitStatus::Running,
                    WorkUnitStatus::Blocked,
                    "question",
                    Some("r4"),
                ),
            )
            .unwrap();
        store
            .run_index_finish("r4", RunIndexStatus::Question, None, None, None, now)
            .unwrap();

        store
            .append_event(
                task.id,
                &Event::Answered {
                    question: "続けますか".into(),
                    answer: "続けてください".into(),
                },
            )
            .unwrap();
        b_row.status = WorkUnitStatus::Ready;
        b_row.blocked_reason = None;
        store
            .work_unit_transition(
                task.id,
                b_row.clone(),
                wu_transitioned(
                    &b_row,
                    WorkUnitStatus::Blocked,
                    WorkUnitStatus::Ready,
                    "answer",
                    None,
                ),
            )
            .unwrap();

        store
            .append_event(task.id, &worker_started("r5", None))
            .unwrap();
        b_row.status = WorkUnitStatus::Running;
        b_row.runs = 3;
        b_row.last_run_id = Some("r5".into());
        store
            .work_unit_transition(
                task.id,
                b_row.clone(),
                wu_transitioned(
                    &b_row,
                    WorkUnitStatus::Ready,
                    WorkUnitStatus::Running,
                    "dispatch",
                    Some("r5"),
                ),
            )
            .unwrap();
        store
            .run_index_start(run_row("r5", task.id, &b.id, 3, RunIndexStatus::Running))
            .unwrap();

        store
            .append_event(
                task.id,
                &worker_finished("r5", task_core::RunEnd::Failed { retryable: false }),
            )
            .unwrap();
        b_row.status = WorkUnitStatus::Failed;
        store
            .work_unit_transition(
                task.id,
                b_row.clone(),
                wu_transitioned(
                    &b_row,
                    WorkUnitStatus::Running,
                    WorkUnitStatus::Failed,
                    "failed",
                    Some("r5"),
                ),
            )
            .unwrap();
        let mut c_row = c.clone();
        c_row.status = WorkUnitStatus::Blocked;
        c_row.blocked_reason = Some(task_core::WorkUnitBlockedReason::DependencyFailed);
        store
            .work_unit_transition(
                task.id,
                c_row.clone(),
                wu_transitioned(
                    &c,
                    WorkUnitStatus::Pending,
                    WorkUnitStatus::Blocked,
                    "dependency_failed",
                    None,
                ),
            )
            .unwrap();
        store
            .run_index_finish("r5", RunIndexStatus::Failed, None, None, None, now)
            .unwrap();

        // --- 突き合わせ ---
        let events = store
            .event_rows_for(task.id, None, usize::MAX)
            .expect("event_rows_for");
        let (rebuilt_units, rebuilt_runs) = rebuild_work_units_and_runs(task.id, &events);
        let stored_units = store.work_units_for(task.id).expect("stored units");
        let stored_runs = store.runs_for_task(task.id).expect("stored runs");

        assert_eq!(rebuilt_units.len(), 3);
        assert_eq!(rebuilt_runs.len(), 5);

        let (wu_mismatches, run_mismatches) = diff_execution(
            task.id,
            &rebuilt_units,
            &stored_units,
            &rebuilt_runs,
            &stored_runs,
        );
        assert_eq!(wu_mismatches, Vec::new(), "{wu_mismatches:?}");
        assert_eq!(run_mismatches, Vec::new(), "{run_mismatches:?}");

        // 中身そのものも確認する（(c)(d)(e)(f) のシナリオが正しく畳み込まれていること）。
        let rebuilt_a = rebuilt_units.iter().find(|u| u.key == "a").unwrap();
        assert_eq!(rebuilt_a.status, WorkUnitStatus::Done);
        assert_eq!(rebuilt_a.runs, 2);
        assert_eq!(rebuilt_a.continuations, 1);
        let rebuilt_b = rebuilt_units.iter().find(|u| u.key == "b").unwrap();
        assert_eq!(rebuilt_b.status, WorkUnitStatus::Failed);
        assert_eq!(rebuilt_b.runs, 3);
        assert_eq!(rebuilt_b.retries, 1);
        let rebuilt_c = rebuilt_units.iter().find(|u| u.key == "c").unwrap();
        assert_eq!(rebuilt_c.status, WorkUnitStatus::Blocked);
        assert_eq!(
            rebuilt_c.blocked_reason,
            Some(task_core::WorkUnitBlockedReason::DependencyFailed)
        );
        assert_eq!(rebuilt_c.runs, 0);
    }

    /// (2) 索引（`work_units`/`runs`）を events に無い値で故意に壊しても、`check_and_apply_execution`
    /// が events から直す（`--apply` 相当）。
    #[test]
    fn check_and_apply_execution_fixes_a_corrupted_index() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Running);
        store.insert(&task).expect("insert");
        let now = OffsetDateTime::now_utc();

        let spec = ExecutionPlanSpec {
            schema: task_core::EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "A only".to_string(),
            work_units: vec![wu_spec("a", &[])],
            phases: Vec::new(),
            children: Vec::new(),
        };
        crate::execution::adopt_plan(
            &store,
            task.id,
            spec,
            task_core::PlanOrigin::Fixture,
            None,
            ExecutionLimits::default(),
            now,
        )
        .expect("adopt plan");
        let a = store
            .work_units_for(task.id)
            .expect("units")
            .into_iter()
            .next()
            .expect("a");

        store
            .append_event(task.id, &worker_started("r1", None))
            .unwrap();
        let mut a_row = a.clone();
        a_row.status = WorkUnitStatus::Running;
        a_row.runs = 1;
        a_row.last_run_id = Some("r1".into());
        store
            .work_unit_transition(
                task.id,
                a_row.clone(),
                wu_transitioned(
                    &a,
                    WorkUnitStatus::Ready,
                    WorkUnitStatus::Running,
                    "dispatch",
                    Some("r1"),
                ),
            )
            .unwrap();
        store
            .run_index_start(run_row("r1", task.id, &a.id, 1, RunIndexStatus::Running))
            .unwrap();
        store
            .append_event(
                task.id,
                &worker_finished("r1", task_core::RunEnd::Completed),
            )
            .unwrap();
        a_row.status = WorkUnitStatus::Done;
        store
            .work_unit_transition(
                task.id,
                a_row.clone(),
                wu_transitioned(
                    &a_row,
                    WorkUnitStatus::Running,
                    WorkUnitStatus::Done,
                    "completed",
                    Some("r1"),
                ),
            )
            .unwrap();
        store
            .run_index_finish("r1", RunIndexStatus::Completed, None, None, None, now)
            .unwrap();

        // events に無い状態へ、索引だけを直接壊す（events は変えない）。
        let mut corrupted = a_row.clone();
        corrupted.status = WorkUnitStatus::Blocked;
        corrupted.blocked_reason = Some(task_core::WorkUnitBlockedReason::Question);
        store
            .work_units_replace(task.id, vec![corrupted])
            .expect("corrupt work_units");
        let mut corrupted_run = store.run_index_get("r1").expect("get").expect("some");
        corrupted_run.status = RunIndexStatus::Failed;
        store
            .runs_replace(task.id, vec![corrupted_run])
            .expect("corrupt runs");

        // --check（apply=false）: 壊れたままで、差分が報告される。
        let (wu_mm, run_mm, plan_mm, applied) =
            check_and_apply_execution(&store, false).expect("check");
        assert!(!wu_mm.is_empty(), "expected a work_unit mismatch");
        assert!(!run_mm.is_empty(), "expected a run mismatch");
        assert_eq!(plan_mm, Vec::new(), "execution_plans was not corrupted");
        assert_eq!(applied, 0);
        assert_eq!(
            store
                .work_units_for(task.id)
                .expect("units")
                .into_iter()
                .next()
                .expect("a")
                .status,
            WorkUnitStatus::Blocked,
            "--check だけでは書き換えない"
        );

        // --apply: events に合わせて直る。
        let (wu_mm, run_mm, plan_mm, applied) =
            check_and_apply_execution(&store, true).expect("apply");
        assert_eq!(wu_mm, Vec::new(), "{wu_mm:?}");
        assert_eq!(run_mm, Vec::new(), "{run_mm:?}");
        assert_eq!(plan_mm, Vec::new(), "{plan_mm:?}");
        assert_eq!(applied, 1);
        let fixed = store
            .work_units_for(task.id)
            .expect("units")
            .into_iter()
            .next()
            .expect("a");
        assert_eq!(fixed.status, WorkUnitStatus::Done, "events が勝つ");
        let fixed_run = store.run_index_get("r1").expect("get").expect("some");
        assert_eq!(fixed_run.status, RunIndexStatus::Completed);

        // 直した後は再び --check しても差分ゼロ。
        let (wu_mm, run_mm, plan_mm, applied) =
            check_and_apply_execution(&store, false).expect("recheck");
        assert_eq!(wu_mm, Vec::new());
        assert_eq!(run_mm, Vec::new());
        assert_eq!(plan_mm, Vec::new());
        assert_eq!(applied, 0);
    }

    /// ADR-0072 D5/D17（Phase E4b 項目5、E2b からの持ち越し）: replan で `execution_plans` が
    /// 複数版になっても、`Event::ExecutionPlanned` だけから版の履歴（v1 = superseded、v2 = active、
    /// `supersedes` の対応）を再構築できる。索引を events に無い値へ故意に壊しても
    /// `check_and_apply_execution` が検出し（`--check`）、`--apply` で直す。`planner_run_id`
    /// （events からは復元できない欄）は比較対象に入らないが、`--apply` で書き戻しても消えない
    /// （既存の値を引き継ぐ）ことも確認する。
    #[test]
    fn check_and_apply_execution_rebuilds_the_replanned_execution_plans_history() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Running);
        store.insert(&task).expect("insert");
        let now = OffsetDateTime::now_utc();

        let v1_spec = ExecutionPlanSpec {
            schema: task_core::EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "v1".to_string(),
            work_units: vec![wu_spec("a", &[])],
            phases: Vec::new(),
            children: Vec::new(),
        };
        let v1 = crate::execution::adopt_plan(
            &store,
            task.id,
            v1_spec,
            task_core::PlanOrigin::Fixture,
            None,
            ExecutionLimits::default(),
            now,
        )
        .expect("adopt v1");

        let v2_spec = ExecutionPlanSpec {
            schema: task_core::EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "v2".to_string(),
            work_units: vec![wu_spec("a", &[]), wu_spec("b", &["a"])],
            phases: Vec::new(),
            children: Vec::new(),
        };
        let (v2, _diff) = crate::execution::replan(
            &store,
            task.id,
            v2_spec,
            "test replan".to_string(),
            task_core::PlanOrigin::Planner,
            Some("planner-run-1".to_string()),
            ExecutionLimits::default(),
            now,
        )
        .expect("replan to v2");

        // events だけから execution_plans の版の履歴を再構築できる。
        let events = store
            .event_rows_for(task.id, None, usize::MAX)
            .expect("events");
        let rebuilt = rebuild_execution_plans(task.id, &events);
        assert_eq!(rebuilt.len(), 2, "{rebuilt:?}");
        let r1 = rebuilt.iter().find(|p| p.version == 1).expect("v1");
        assert_eq!(r1.id, v1.id);
        assert_eq!(r1.status, PlanStatus::Superseded);
        assert!(r1.superseded_at.is_some(), "{r1:?}");
        let r2 = rebuilt.iter().find(|p| p.version == 2).expect("v2");
        assert_eq!(r2.id, v2.id);
        assert_eq!(r2.status, PlanStatus::Active);
        assert_eq!(r2.origin, task_core::PlanOrigin::Planner);
        assert!(r2.superseded_at.is_none());

        // --check: まだ索引を壊していないので差分ゼロ（`planner_run_id` は比較対象外なので、
        // stored に `Some("planner-run-1")` があっても不一致にならない）。
        let (_, _, plan_mm, applied) =
            check_and_apply_execution(&store, false).expect("check clean");
        assert_eq!(plan_mm, Vec::new(), "{plan_mm:?}");
        assert_eq!(applied, 0);

        // 索引だけを events に無い値で故意に壊す（v2 の origin を書き換える）。
        let mut corrupted_plans = store.execution_plan_list(task.id).expect("list");
        for p in &mut corrupted_plans {
            if p.version == 2 {
                p.origin = task_core::PlanOrigin::Human;
            }
        }
        store
            .execution_plans_replace(task.id, corrupted_plans)
            .expect("corrupt execution_plans");

        // --check（apply=false）: 壊れたままで、差分が報告される。
        let (wu_mm, run_mm, plan_mm, applied) =
            check_and_apply_execution(&store, false).expect("check corrupted");
        assert!(wu_mm.is_empty(), "{wu_mm:?}");
        assert!(run_mm.is_empty(), "{run_mm:?}");
        assert!(
            plan_mm
                .iter()
                .any(|m| m.version == 2 && m.field == "origin"),
            "{plan_mm:?}"
        );
        assert_eq!(applied, 0);
        let still_corrupted = store
            .execution_plan_list(task.id)
            .expect("list")
            .into_iter()
            .find(|p| p.version == 2)
            .expect("v2 present");
        assert_eq!(
            still_corrupted.origin,
            task_core::PlanOrigin::Human,
            "--check だけでは書き換えない"
        );

        // --apply: events に合わせて直る。
        let (wu_mm, run_mm, plan_mm, applied) =
            check_and_apply_execution(&store, true).expect("apply");
        assert_eq!(wu_mm, Vec::new(), "{wu_mm:?}");
        assert_eq!(run_mm, Vec::new(), "{run_mm:?}");
        assert_eq!(plan_mm, Vec::new(), "{plan_mm:?}");
        assert_eq!(applied, 1);
        let fixed = store
            .execution_plan_list(task.id)
            .expect("list")
            .into_iter()
            .find(|p| p.version == 2)
            .expect("v2 present");
        assert_eq!(
            fixed.origin,
            task_core::PlanOrigin::Planner,
            "events が勝つ"
        );
        assert_eq!(
            fixed.planner_run_id.as_deref(),
            Some("planner-run-1"),
            "--apply で書き戻しても、events から復元できない planner_run_id は既存の値のまま"
        );

        // 直した後は再び --check しても差分ゼロ。
        let (wu_mm, run_mm, plan_mm, applied) =
            check_and_apply_execution(&store, false).expect("recheck");
        assert_eq!(wu_mm, Vec::new());
        assert_eq!(run_mm, Vec::new());
        assert_eq!(plan_mm, Vec::new());
        assert_eq!(applied, 0);
    }

    /// (3) 計画の無い Task（暗黙の WorkUnit）では `work_units` は空、`runs` は worker/reviewer
    /// 双方を含む全 run 分になる。
    #[test]
    fn rebuild_work_units_and_runs_is_empty_for_a_task_without_a_plan_and_covers_every_run() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Running);
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::Created {
                    task: Box::new(task.clone()),
                },
            )
            .unwrap();

        // worker run #1（失敗して requeue、既存の atomic 経路。WU には無関係）。
        store
            .append_event(task.id, &worker_started("r1", None))
            .unwrap();
        store
            .append_event(
                task.id,
                &worker_finished("r1", task_core::RunEnd::Failed { retryable: true }),
            )
            .unwrap();
        // worker run #2（完了）。
        store
            .append_event(task.id, &worker_started("r2", None))
            .unwrap();
        store
            .append_event(
                task.id,
                &worker_finished("r2", task_core::RunEnd::Completed),
            )
            .unwrap();
        // reviewer run。
        store
            .append_event(task.id, &worker_started("rev-1", Some(RunRole::Reviewer)))
            .unwrap();
        store
            .append_event(
                task.id,
                &Event::WorkerFinished {
                    run_id: "rev-1".into(),
                    outcome: "reviewed".into(),
                    usage: None,
                    role: Some(RunRole::Reviewer),
                    metrics: None,
                    end: None,
                },
            )
            .unwrap();

        let events = store
            .event_rows_for(task.id, None, usize::MAX)
            .expect("event_rows_for");
        let (units, runs) = rebuild_work_units_and_runs(task.id, &events);
        assert_eq!(units, Vec::new(), "計画が無いタスクの work_units は空");
        assert_eq!(runs.len(), 3, "worker 2 件 + reviewer 1 件");
        assert!(runs.iter().all(|r| r.work_unit_id.is_none()));
        let worker_runs: Vec<&RunRow> = runs
            .iter()
            .filter(|r| r.role == RunIndexRole::Worker)
            .collect();
        assert_eq!(worker_runs.len(), 2);
        assert_eq!(worker_runs[0].seq, 1);
        assert_eq!(worker_runs[1].seq, 2);
        let reviewer_runs: Vec<&RunRow> = runs
            .iter()
            .filter(|r| r.role == RunIndexRole::Reviewer)
            .collect();
        assert_eq!(reviewer_runs.len(), 1);
        assert_eq!(reviewer_runs[0].seq, 1);
        // r2 は end=Completed が付いているので RunIndexStatus::Completed。r1 は end=Failed。
        assert_eq!(
            runs.iter().find(|r| r.run_id == "r2").unwrap().status,
            RunIndexStatus::Completed
        );
        assert_eq!(
            runs.iter().find(|r| r.run_id == "r1").unwrap().status,
            RunIndexStatus::Failed
        );
        // reviewer run は `end` を持たない run（`role: Some(Reviewer)` の既存の WorkerFinished は
        // `end: None` のことが多い）ので HarnessError にフォールバックする（ADR-0072 D7 の安全網）。
        assert_eq!(
            runs.iter().find(|r| r.run_id == "rev-1").unwrap().status,
            RunIndexStatus::HarnessError
        );
    }
}
