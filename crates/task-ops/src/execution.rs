//! ADR-0072（Phase E2）: ExecutionPlan の採用（`POST /tasks/{id}/execution-plan`、
//! `celerisctl execution plan set|show`）。
//!
//! 判断（D14 の検証・D15 の scheduler）はすべて `task_core::execution_plan` の純粋関数にあり、
//! ここは I/O（store 呼び出し）と id・時刻の発行だけを行う（ADR-0001 D2）。

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
        }
    }

    fn spec() -> ExecutionPlanSpec {
        ExecutionPlanSpec {
            schema: task_core::EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "A -> B -> C".to_string(),
            work_units: vec![wu("a", &[]), wu("b", &["a"]), wu("c", &["b"])],
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
}
