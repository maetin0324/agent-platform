//! ADR-0072（Phase E2）: ExecutionPlan の採用（`POST /tasks/{id}/execution-plan`、
//! `celerisctl execution plan set|show`）。
//!
//! 判断（D14 の検証・D15 の scheduler）はすべて `task_core::execution_plan` の純粋関数にあり、
//! ここは I/O（store 呼び出し）と id・時刻の発行だけを行う（ADR-0001 D2）。

use std::collections::BTreeSet;

use task_core::execution_plan::{PlanValidationError, validate};
use task_core::{
    Event, ExecutionLimits, ExecutionPlanRow, ExecutionPlanSpec, PlanOrigin, PlanStatus, TaskId,
    TaskStore, WorkUnitRow, WorkUnitStatus, new_id,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::OpsError;

fn format_rfc3339(t: OffsetDateTime) -> Result<String, OpsError> {
    t.format(&Rfc3339)
        .map_err(|e| OpsError::Validation(format!("time formatting error: {e}")))
}

/// D14 の検証エラーを 1 行の文言にする（`OpsError::Validation`。API は 422、`celerisctl` はそのまま表示）。
pub fn describe_validation_errors(errors: &[PlanValidationError]) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

/// D14/D5: 計画を検証し、新規に採用する（`execution_plans` / `work_units` の行と
/// `Event::ExecutionPlanned` を同じトランザクションで書く）。
///
/// - タスクが無ければ `OpsError::NotFound`。
/// - `spec` が D14 の検証に落ちれば `OpsError::Validation`。
/// - タスクに既に `active` な計画があれば `OpsError::Store(StoreError::InUse)`（E2 は新規のみ。
///   replan は Phase E4）。
pub fn adopt_plan(
    store: &dyn TaskStore,
    task_id: TaskId,
    spec: ExecutionPlanSpec,
    origin: PlanOrigin,
    planner_run_id: Option<String>,
    limits: ExecutionLimits,
    now: OffsetDateTime,
) -> Result<ExecutionPlanRow, OpsError> {
    if store.get(task_id)?.is_none() {
        return Err(OpsError::NotFound(task_id));
    }
    let validated = validate(&spec, limits, &[])
        .map_err(|errors| OpsError::Validation(describe_validation_errors(&errors)))?;

    let plan_id = new_id();
    let created_at = format_rfc3339(now)?;

    let work_units: Vec<WorkUnitRow> = validated
        .topological_order
        .iter()
        .enumerate()
        .map(|(seq, &idx)| {
            let wu_spec = validated.spec.work_units[idx].clone();
            let status = if wu_spec.depends_on.is_empty() {
                WorkUnitStatus::Ready
            } else {
                WorkUnitStatus::Pending
            };
            WorkUnitRow::new(
                new_id(),
                task_id.to_string(),
                plan_id.clone(),
                seq as u32,
                wu_spec,
                status,
                created_at.clone(),
            )
        })
        .collect();

    let plan = ExecutionPlanRow {
        id: plan_id.clone(),
        task_id: task_id.to_string(),
        version: 1,
        origin,
        planner_run_id,
        status: PlanStatus::Active,
        spec: validated.spec.clone(),
        created_at: created_at.clone(),
        superseded_at: None,
    };
    let event = Event::ExecutionPlanned {
        plan_id: plan_id.clone(),
        version: 1,
        origin,
        supersedes: None,
        reason: None,
        plan: Box::new(validated.spec),
    };
    store.execution_plan_adopt(task_id, plan.clone(), work_units, event)?;
    Ok(plan)
}

/// ADR-0072 D17（Phase E4）: [`replan`] が計算した差分（監査・GUI 用。版の履歴の「差分の件数」）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplanDiff {
    /// 新しい key（新規の WorkUnit）。
    pub added: Vec<String>,
    /// 既存（未完了）の WorkUnit で spec または依存が変わったもの。
    pub changed: Vec<String>,
    /// 新しい版に無くなった未完了の WorkUnit（`superseded` にする）。
    pub removed: Vec<String>,
}

/// D17: 計画を版更新する（旧 `active` な計画を `superseded` にし、新しい版を採用する）。
///
/// - `done` の WorkUnit は**保持する**（行に触れない。`validate` が key/spec 不変を検証済み）。
/// - 未完了で新しい版にも残る key は、その場で spec・依存・状態（`ready`/`pending`。依存がすべて
///   `done` なら `ready`）を更新する。
/// - 未完了で新しい版に無い key は `superseded`（`WorkUnitTransitioned{reason: "replan v<n>"}`）。
/// - 新しい key は新規の行として追加する。
///
/// - タスクが無ければ `OpsError::NotFound`。
/// - `active` な計画が無ければ `OpsError::Validation`（replan は既存の計画の上でだけ行う）。
/// - `spec` が D14 の検証（done 不変を含む）に落ちれば `OpsError::Validation`。
/// - 新しい key が、過去に（superseded 含め）使われた key と衝突すれば `OpsError::Validation`
///   （`work_units` の `UNIQUE(task_id, key)` を先に検査する）。
/// - 旧版が並行に置き換わっていれば `OpsError::Store(StoreError::InUse)`。
#[allow(clippy::too_many_arguments)]
pub fn replan(
    store: &dyn TaskStore,
    task_id: TaskId,
    spec: ExecutionPlanSpec,
    reason: String,
    origin: PlanOrigin,
    planner_run_id: Option<String>,
    limits: ExecutionLimits,
    now: OffsetDateTime,
) -> Result<(ExecutionPlanRow, ReplanDiff), OpsError> {
    if store.get(task_id)?.is_none() {
        return Err(OpsError::NotFound(task_id));
    }
    let Some(active) = store.execution_plan_active(task_id)? else {
        return Err(OpsError::Validation(
            "task has no active execution plan to replan".to_string(),
        ));
    };
    let all_units = store.work_units_for(task_id)?;
    let current: Vec<WorkUnitRow> = all_units
        .iter()
        .filter(|u| u.status.is_active())
        .cloned()
        .collect();
    let done_work_units: Vec<(String, task_core::WorkUnitSpec)> = current
        .iter()
        .filter(|u| u.status == WorkUnitStatus::Done)
        .map(|u| (u.key.clone(), u.spec.clone()))
        .collect();
    let validated = validate(&spec, limits, &done_work_units)
        .map_err(|errors| OpsError::Validation(describe_validation_errors(&errors)))?;

    let current_keys: BTreeSet<&str> = current.iter().map(|u| u.key.as_str()).collect();
    let new_keys: BTreeSet<&str> = validated
        .spec
        .work_units
        .iter()
        .map(|w| w.key.as_str())
        .collect();
    // `work_units.key` は `UNIQUE(task_id, key)`。過去（superseded を含む）に使われた key を
    // 「新しい」key として再利用しようとしたら拒否する（D5）。
    let all_keys_ever: BTreeSet<&str> = all_units.iter().map(|u| u.key.as_str()).collect();
    for key in new_keys.difference(&current_keys) {
        if all_keys_ever.contains(key) {
            return Err(OpsError::Validation(format!(
                "work unit key {key:?} was used by a superseded work unit and cannot be reused"
            )));
        }
    }

    let new_plan_id = new_id();
    let created_at = format_rfc3339(now)?;
    let new_version = active.version + 1;
    let done_keys: BTreeSet<&str> = done_work_units.iter().map(|(k, _)| k.as_str()).collect();

    let mut diff = ReplanDiff::default();
    let mut updated_work_units = Vec::new();
    let mut extra_events = Vec::new();

    // 削除: 現在アクティブだが新しい版に無い（done では起き得ない。validate が検証済み）。
    for u in &current {
        if u.status != WorkUnitStatus::Done && !new_keys.contains(u.key.as_str()) {
            let mut row = u.clone();
            let from = row.status;
            row.status = WorkUnitStatus::Superseded;
            row.blocked_reason = None;
            row.updated_at = created_at.clone();
            extra_events.push(Event::WorkUnitTransitioned {
                work_unit_id: row.id.clone(),
                key: row.key.clone(),
                from,
                to: WorkUnitStatus::Superseded,
                reason: format!("replan v{new_version}"),
                run_id: None,
            });
            diff.removed.push(u.key.clone());
            updated_work_units.push(row);
        }
    }

    let mut new_work_units = Vec::new();
    for (seq, &idx) in validated.topological_order.iter().enumerate() {
        let wu_spec = validated.spec.work_units[idx].clone();
        if done_keys.contains(wu_spec.key.as_str()) {
            // done は不変。行には触れない（`plan_id`/`seq` も元のまま）。
            continue;
        }
        let all_deps_done = wu_spec
            .depends_on
            .iter()
            .all(|d| done_keys.contains(d.as_str()));
        let status = if all_deps_done {
            WorkUnitStatus::Ready
        } else {
            WorkUnitStatus::Pending
        };
        match current.iter().find(|u| u.key == wu_spec.key) {
            Some(existing) => {
                let from = existing.status;
                let spec_changed = existing.spec != wu_spec;
                if spec_changed || from != status {
                    diff.changed.push(wu_spec.key.clone());
                }
                let mut row = existing.clone();
                row.plan_id = new_plan_id.clone();
                row.seq = seq as u32;
                row.depends_on = wu_spec.depends_on.clone();
                row.spec = wu_spec;
                row.status = status;
                row.blocked_reason = None;
                row.updated_at = created_at.clone();
                // D17: 未完了で持ち越した WU は replan のたびに窓を作り直す（D18「回答の時点から
                // 数え直す」と同じ考え方）。retries/continuations/runs を 0 に戻し、直前の run への
                // 参照も落とす（新しい版の spec の下で最初から試す）。
                row.runs = 0;
                row.continuations = 0;
                row.retries = 0;
                row.last_run_id = None;
                row.last_checkpoint_run_id = None;
                if from != status {
                    extra_events.push(Event::WorkUnitTransitioned {
                        work_unit_id: row.id.clone(),
                        key: row.key.clone(),
                        from,
                        to: status,
                        reason: format!("replan v{new_version}"),
                        run_id: None,
                    });
                }
                updated_work_units.push(row);
            }
            None => {
                diff.added.push(wu_spec.key.clone());
                let is_repair = wu_spec.kind == task_core::WorkUnitKind::Repair;
                let row = WorkUnitRow::new(
                    new_id(),
                    task_id.to_string(),
                    new_plan_id.clone(),
                    seq as u32,
                    wu_spec,
                    status,
                    created_at.clone(),
                );
                // ADR-0074 D6.2（Phase F1）: replan（LLM/人）が自ら `kind = repair` の WU を書いたら、
                // class を `"planner"` として残す（`execution_metrics` の `unknown` を無くす）。
                // daemon の決定的な repair（`try_review_repair`）はこの経路を通らない
                // （`store.review_repair_apply` を直接使う）ので二重に記録しない。
                if is_repair {
                    extra_events.push(Event::RepairScheduled {
                        work_unit_id: row.id.clone(),
                        key: row.key.clone(),
                        class: "planner".to_string(),
                        origin: task_core::execution::RepairOrigin::Planner,
                    });
                }
                new_work_units.push(row);
            }
        }
    }

    let new_plan = ExecutionPlanRow {
        id: new_plan_id.clone(),
        task_id: task_id.to_string(),
        version: new_version,
        origin,
        planner_run_id,
        status: PlanStatus::Active,
        spec: validated.spec.clone(),
        created_at: created_at.clone(),
        superseded_at: None,
    };
    // ADR-0074 D5.3（Phase F1）: 版の差分の件数を `reason` の後ろに決定的な形で足す（E5 の未実装
    // 「版の差分の件数」の解消）。
    let reason_with_diff = format!(
        "{reason} (added={}, changed={}, removed={})",
        diff.added.len(),
        diff.changed.len(),
        diff.removed.len()
    );
    let plan_event = Event::ExecutionPlanned {
        plan_id: new_plan_id,
        version: new_version,
        origin,
        supersedes: Some(active.id.clone()),
        reason: Some(reason_with_diff),
        plan: Box::new(validated.spec),
    };
    store.execution_plan_replan(
        task_id,
        active.id,
        new_plan.clone(),
        updated_work_units,
        new_work_units,
        extra_events,
        plan_event,
    )?;
    Ok((new_plan, diff))
}

/// タスクの `active` な計画と WorkUnit（`GET`/`celerisctl execution plan show` が使う）。
#[derive(Debug, Clone, PartialEq)]
pub struct PlanView {
    pub plan: ExecutionPlanRow,
    pub work_units: Vec<WorkUnitRow>,
}

/// タスクの `active` な計画を読む。無ければ `Ok(None)`（タスク自体が無ければ `OpsError::NotFound`）。
pub fn active_plan(store: &dyn TaskStore, task_id: TaskId) -> Result<Option<PlanView>, OpsError> {
    if store.get(task_id)?.is_none() {
        return Err(OpsError::NotFound(task_id));
    }
    let Some(plan) = store.execution_plan_active(task_id)? else {
        return Ok(None);
    };
    let work_units = store.work_units_for(task_id)?;
    Ok(Some(PlanView { plan, work_units }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use task_core::{
        ArtifactRef, Budget, Check, Criterion, SqliteStore, Task, TaskKind, Tier, WorkUnitContext,
        WorkUnitKind, WorkUnitSpec, WorkerHint, WorkspaceSpec,
    };

    fn wu(key: &str, depends_on: &[&str]) -> WorkUnitSpec {
        WorkUnitSpec {
            key: key.to_string(),
            kind: WorkUnitKind::Implement,
            title: format!("title {key}"),
            objective: format!("objective for the {key} step, spelled out plainly"),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            done_when: vec![],
            checks: vec![],
            context: WorkUnitContext::default(),
            harness: None,
            features: None,
            budget: None,
            outputs: vec![],
            phase: None,
        }
    }

    fn spec() -> ExecutionPlanSpec {
        ExecutionPlanSpec {
            schema: task_core::EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "A -> B -> C".to_string(),
            work_units: vec![wu("a", &[]), wu("b", &["a"]), wu("c", &["b"])],
            phases: Vec::new(),
            children: Vec::new(),
        }
    }

    fn sample_task() -> Task {
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
            status: task_core::Status::Draft,
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
    fn adopt_plan_rejects_a_missing_task() {
        let store = SqliteStore::open_in_memory().unwrap();
        let err = adopt_plan(
            &store,
            TaskId::new(),
            spec(),
            PlanOrigin::Human,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(matches!(err, OpsError::NotFound(_)));
    }

    #[test]
    fn adopt_plan_creates_ready_and_pending_work_units_in_topological_order() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        let plan = adopt_plan(
            &store,
            task.id,
            spec(),
            PlanOrigin::Human,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        assert_eq!(plan.version, 1);
        assert_eq!(plan.origin, PlanOrigin::Human);

        let view = active_plan(&store, task.id).unwrap().unwrap();
        assert_eq!(view.plan.id, plan.id);
        assert_eq!(view.work_units.len(), 3);
        assert_eq!(view.work_units[0].key, "a");
        assert_eq!(view.work_units[0].status, WorkUnitStatus::Ready);
        assert_eq!(view.work_units[1].key, "b");
        assert_eq!(view.work_units[1].status, WorkUnitStatus::Pending);
        assert_eq!(view.work_units[2].key, "c");
        assert_eq!(view.work_units[2].status, WorkUnitStatus::Pending);
    }

    #[test]
    fn adopt_plan_rejects_an_invalid_plan_without_writing_anything() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        let mut bad = spec();
        bad.work_units[1].depends_on = vec!["ghost".to_string()];
        let err = adopt_plan(
            &store,
            task.id,
            bad,
            PlanOrigin::Human,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(matches!(err, OpsError::Validation(_)), "{err:?}");
        assert!(active_plan(&store, task.id).unwrap().is_none());
    }

    #[test]
    fn adopt_plan_rejects_a_second_plan_for_the_same_task() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        adopt_plan(
            &store,
            task.id,
            spec(),
            PlanOrigin::Human,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        let err = adopt_plan(
            &store,
            task.id,
            spec(),
            PlanOrigin::Human,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(
            matches!(err, OpsError::Store(task_core::StoreError::InUse { .. })),
            "{err:?}"
        );
    }

    #[test]
    fn active_plan_is_none_for_a_task_without_a_plan() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        assert!(active_plan(&store, task.id).unwrap().is_none());
    }

    // ---- ADR-0072 D17（Phase E4）: replan ----

    fn adopt(store: &SqliteStore, task_id: TaskId) -> PlanView {
        adopt_plan(
            store,
            task_id,
            spec(),
            PlanOrigin::Fixture,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        active_plan(store, task_id).unwrap().unwrap()
    }

    fn mark_done(store: &SqliteStore, task_id: TaskId, key: &str) {
        let view = active_plan(store, task_id).unwrap().unwrap();
        let row = view.work_units.iter().find(|u| u.key == key).unwrap();
        let mut updated = row.clone();
        updated.status = task_core::WorkUnitStatus::Done;
        store
            .work_unit_transition(
                task_id,
                updated,
                Event::WorkUnitTransitioned {
                    work_unit_id: row.id.clone(),
                    key: row.key.clone(),
                    from: row.status,
                    to: task_core::WorkUnitStatus::Done,
                    reason: "completed".to_string(),
                    run_id: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn replan_rejects_when_there_is_no_active_plan() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        let err = replan(
            &store,
            task.id,
            spec(),
            "test".to_string(),
            PlanOrigin::Planner,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(matches!(err, OpsError::Validation(_)), "{err:?}");
    }

    /// D17 の人の依頼の例: A は done、M（migration）を追加、B は blocked by M
    /// （`depends_on: ["m"]`）、C は blocked by B。
    #[test]
    fn replan_keeps_done_work_units_and_applies_the_human_request_example() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        let v1 = adopt(&store, task.id);
        mark_done(&store, task.id, "a");

        let mut v2_spec = spec();
        v2_spec.work_units[1].depends_on = vec!["a".to_string(), "m".to_string()]; // b: blocked by m
        v2_spec.work_units.insert(1, wu("m", &[])); // migration, no deps
        let (new_plan, diff) = replan(
            &store,
            task.id,
            v2_spec,
            "human request: add migration m before b".to_string(),
            PlanOrigin::Human,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        assert_eq!(new_plan.version, 2);
        assert_eq!(new_plan.origin, PlanOrigin::Human);
        assert_eq!(diff.added, vec!["m".to_string()]);
        assert_eq!(diff.changed, vec!["b".to_string()]);
        assert!(diff.removed.is_empty(), "{diff:?}");

        // 旧版は superseded、新版が active。
        let plans = store.execution_plan_list(task.id).unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].id, v1.plan.id);
        assert_eq!(plans[0].status, PlanStatus::Superseded);
        assert_eq!(plans[1].id, new_plan.id);
        assert_eq!(plans[1].status, PlanStatus::Active);

        let units = store.work_units_for(task.id).unwrap();
        assert_eq!(units.len(), 4, "{units:?}"); // a, b, c（保持）+ m（追加）
        let a = units.iter().find(|u| u.key == "a").unwrap();
        assert_eq!(a.status, WorkUnitStatus::Done, "done の a は保持される");
        assert_eq!(a.plan_id, v1.plan.id, "done の行は元の plan_id のまま");
        let m = units.iter().find(|u| u.key == "m").unwrap();
        assert_eq!(m.status, WorkUnitStatus::Ready, "依存が無い m はすぐ ready");
        assert_eq!(m.plan_id, new_plan.id);
        let b = units.iter().find(|u| u.key == "b").unwrap();
        assert_eq!(
            b.status,
            WorkUnitStatus::Pending,
            "m がまだ done でないので b は pending"
        );
        assert_eq!(b.depends_on, vec!["a".to_string(), "m".to_string()]);
        let c = units.iter().find(|u| u.key == "c").unwrap();
        assert_eq!(
            c.status,
            WorkUnitStatus::Pending,
            "b 経由で m に依存 = pending"
        );

        let events = store.events_for(task.id).unwrap();
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                Event::ExecutionPlanned { version: 2, supersedes: Some(s), .. } if *s == v1.plan.id
            )),
            "{events:?}"
        );
    }

    #[test]
    fn replan_supersedes_work_units_that_are_dropped_from_the_new_plan() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        adopt(&store, task.id);
        mark_done(&store, task.id, "a");

        // c を落とす（b で終わる 2 段の計画に縮める）。
        let mut v2_spec = spec();
        v2_spec.work_units.truncate(2); // a, b だけ
        let (_, diff) = replan(
            &store,
            task.id,
            v2_spec,
            "drop c".to_string(),
            PlanOrigin::Human,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        assert_eq!(diff.removed, vec!["c".to_string()]);

        let units = store.work_units_for(task.id).unwrap();
        let c = units.iter().find(|u| u.key == "c").unwrap();
        assert_eq!(c.status, WorkUnitStatus::Superseded);
    }

    #[test]
    fn replan_rejects_a_changed_done_work_unit() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        adopt(&store, task.id);
        mark_done(&store, task.id, "a");

        let mut v2_spec = spec();
        v2_spec.work_units[0].objective = "a completely different objective now".to_string();
        let err = replan(
            &store,
            task.id,
            v2_spec,
            "test".to_string(),
            PlanOrigin::Human,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(matches!(err, OpsError::Validation(_)), "{err:?}");
        // 何も書き込まれていない。
        let units = store.work_units_for(task.id).unwrap();
        assert_eq!(units.len(), 3);
    }

    #[test]
    fn replan_rejects_reusing_a_superseded_key() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        adopt(&store, task.id);
        mark_done(&store, task.id, "a");

        let mut v2_spec = spec();
        v2_spec.work_units.truncate(2); // c を落とす（superseded になる）
        replan(
            &store,
            task.id,
            v2_spec,
            "drop c".to_string(),
            PlanOrigin::Human,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        // v3 で c を「新しい」key として使い回そうとすると拒否される（UNIQUE(task_id, key)）。
        let v3_spec = spec(); // a, b, c 全部（c は superseded 済みの key）
        let err = replan(
            &store,
            task.id,
            v3_spec,
            "reintroduce c".to_string(),
            PlanOrigin::Human,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(matches!(err, OpsError::Validation(_)), "{err:?}");
    }

    /// ADR-0074 D6.2/§6 F1 (i)（Phase F1）: replan（planner）が新しい `kind = repair` の WU を書いたら
    /// `Event::RepairScheduled{class: "planner"}` が残る（`execution_metrics` の `unknown` を無くす）。
    #[test]
    fn replan_records_repair_scheduled_for_a_planner_authored_repair_unit() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        adopt(&store, task.id);
        mark_done(&store, task.id, "a");

        let mut v2_spec = spec();
        let mut repair = wu("repair-1", &[]);
        repair.kind = task_core::WorkUnitKind::Repair;
        v2_spec.work_units.push(repair);
        let (_, diff) = replan(
            &store,
            task.id,
            v2_spec,
            "add a repair unit".to_string(),
            PlanOrigin::Planner,
            None,
            ExecutionLimits::default(),
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        assert_eq!(diff.added, vec!["repair-1".to_string()]);

        let events = store.events_for(task.id).unwrap();
        let scheduled = events
            .iter()
            .find_map(|(_, e)| match e {
                Event::RepairScheduled {
                    key, class, origin, ..
                } if key == "repair-1" => Some((class.clone(), *origin)),
                _ => None,
            })
            .expect("a RepairScheduled event for repair-1");
        assert_eq!(scheduled.0, "planner");
        assert_eq!(scheduled.1, task_core::execution::RepairOrigin::Planner);
    }
}
