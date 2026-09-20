//! `POST /tasks/{id}/retry`（Phase 31。実機の事故、2026-09-18）: 失敗した仕事をやり直す。
//!
//! 「進行不能になった案件の調査タスクを人が一手でやり直す」手段が API にも GUI にも無かった事故から。
//! `failed` または `cancelled` のタスクを**複製して新しいタスクを作る**（`Failed`/`Cancelled` を非終端に
//! 戻す状態機械の遷移は足さない。DESIGN の状態機械を壊さないため）。`depends_on` は元と同じにし、
//! 元のタスクに依存していた未終端（またはその依存の失敗で `cancelled` になった）タスクの `depends_on` を
//! 新しい id に張り替える（後者は `draft` に戻す）。書き込みは `TaskStore::retry_task` に任せ、ここでは
//! 検証と `Task` の組み立てだけを行う。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{Status, Task, TaskId, TaskStore};
use time::OffsetDateTime;

use crate::error::OpsError;

/// `POST /tasks/{id}/retry` の応答（201）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetryResult {
    /// 新しく作られたタスク。
    pub task_id: TaskId,
    /// `depends_on` を新しいタスクへ張り替えた（元は `original` に依存していた）タスクの id。
    #[serde(default)]
    pub rewired: Vec<TaskId>,
}

/// `id` のタスクが `failed`/`cancelled` でなければ `OpsError::InvalidState`（API は 409）。
/// それ以外の検証は無い（複製元は既に一度検証を通っている）。
pub fn retry_task(
    store: &dyn TaskStore,
    id: TaskId,
    accept: bool,
    now: OffsetDateTime,
) -> Result<RetryResult, OpsError> {
    let original = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    if !matches!(original.status, Status::Failed | Status::Cancelled) {
        return Err(OpsError::InvalidState {
            id,
            context: format!("status={:?}", original.status),
            action: "retried".to_string(),
        });
    }

    let new_task = Task {
        // ADR-0043 D2: やり直しは元のタスクと同じリポジトリで作業する。
        repos: original.repos.clone(),
        id: TaskId::new(),
        parent_id: original.parent_id,
        kind: original.kind,
        title: original.title.clone(),
        objective: original.objective.clone(),
        acceptance: original.acceptance.clone(),
        inputs: original.inputs.clone(),
        depends_on: original.depends_on.clone(),
        status: if accept { Status::Ready } else { Status::Draft },
        priority: original.priority,
        worker_hint: original.worker_hint.clone(),
        workspace: original.workspace.clone(),
        budget: original.budget,
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
        role: original.role.clone(),
        genre: original.genre.clone(),
        aggregate: original.aggregate,
        project_id: original.project_id,
        milestone_id: original.milestone_id,
        assignee: original.assignee.clone(),
        conversation: None,
        // ADR-0044 D3（Phase 53）: やり直したタスクは元のラベル・種類を引き継ぐ（人が付けた分類なので）。
        labels: original.labels.clone(),
        category: original.category,
        // ADR-0046 D2 / D4（Phase 59）: やり直しは元の能力タグと進め方をそのまま引き継ぐ。
        skills: original.skills.clone(),
        mode: original.mode,
    };
    let new_id = new_task.id;
    let rewired = store.retry_task(id, &new_task)?;
    Ok(RetryResult {
        task_id: new_id,
        rewired,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{
        ArtifactRef, Budget, Check, Criterion, Event, MessageId, SqliteStore, TaskKind, Tier,
        Trigger, WorkerHint, WorkspaceSpec,
    };

    fn store() -> SqliteStore {
        SqliteStore::open_in_memory().expect("open")
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn base_spec(title: &str) -> crate::add::NewTaskSpec {
        crate::add::NewTaskSpec {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            title: title.to_string(),
            objective: "do it".to_string(),
            acceptance: vec![crate::add::CriterionSpec::Human {
                text: "looks right".to_string(),
            }],
            kind: TaskKind::Execute,
            tier: None,
            priority: Some(crate::add::PriorityInput::Number(0)),
            parent: None,
            depends_on: vec![],
            max_turns: None,
            max_wall_secs: None,
            max_retries: 2,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            workspace: None,
            cluster: None,
            adapter: None,
            labels: Vec::new(),
            category: None,
            status: None,
        }
    }

    fn make_failed(store: &SqliteStore, title: &str) -> Task {
        // ADR-0044 D3: ラベル・種類を付けてから失敗させる（やり直しが引き継ぐことを確かめるため）。
        let spec = crate::add::NewTaskSpec {
            labels: vec!["infra".into()],
            category: Some(task_core::TaskCategory::Bug),
            ..base_spec(title)
        };
        let task = crate::add::create_task(store, spec, now()).expect("create");
        store
            .apply_transition(task.id, Trigger::Accept, None)
            .expect("accept");
        store
            .apply_transition(task.id, Trigger::Dispatch, None)
            .expect("dispatch");
        let outcome = store
            .apply_transition(task.id, Trigger::WorkerError { retryable: false }, None)
            .expect("worker_error");
        assert_eq!(outcome.next, Status::Failed);
        store.get(task.id).expect("get").expect("some")
    }

    /// `store.insert` で直接書く（`add.rs` の検証を経由しない。依存関係の状態を自由に組み立てるため）。
    fn raw_task(status: Status, depends_on: Vec<TaskId>, conversation: Option<MessageId>) -> Task {
        let t = now();
        Task {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "dependent".to_string(),
            objective: "depend on it".to_string(),
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
            }],
            depends_on,
            status,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "/tmp/workspace".into(),
                mode: None,
            },
            budget: Budget {
                max_turns: 10,
                max_wall_secs: 600,
                max_retries: 2,
            },
            attempts: 0,
            lease: None,
            created_at: t,
            updated_at: t,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            conversation,
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    #[test]
    fn retry_duplicates_fields_and_records_retried_event() {
        let store = store();
        let original = make_failed(&store, "investigate incident");

        let result = retry_task(&store, original.id, false, now()).expect("retry");
        assert!(result.rewired.is_empty());
        let new_task = store.get(result.task_id).expect("get").expect("some");
        assert_eq!(new_task.status, Status::Draft);
        assert_eq!(new_task.attempts, 0);
        assert_eq!(new_task.title, original.title);
        assert_eq!(new_task.objective, original.objective);
        assert_eq!(new_task.acceptance, original.acceptance);
        assert_eq!(new_task.inputs, original.inputs);
        assert_eq!(new_task.worker_hint, original.worker_hint);
        assert_eq!(new_task.budget, original.budget);
        assert_eq!(new_task.role, original.role);
        assert_eq!(new_task.genre, original.genre);
        assert_eq!(new_task.project_id, original.project_id);
        assert_eq!(new_task.milestone_id, original.milestone_id);
        assert_eq!(new_task.assignee, original.assignee);
        assert_eq!(new_task.parent_id, original.parent_id);
        assert_eq!(new_task.workspace, original.workspace);
        assert_eq!(new_task.depends_on, original.depends_on);
        assert_eq!(new_task.conversation, None);
        // ADR-0044 D3（Phase 53）: 人が付けた分類（ラベル・種類）は引き継ぐ。
        assert_eq!(new_task.labels, original.labels);
        assert_eq!(new_task.category, original.category);

        let events: Vec<Event> = store
            .events_for(new_task.id)
            .expect("events")
            .into_iter()
            .map(|(_, e)| e)
            .collect();
        assert!(matches!(&events[0], Event::Created { task } if task.id == new_task.id));
        assert!(matches!(&events[1], Event::Retried { from } if *from == original.id));
    }

    #[test]
    fn retry_with_accept_starts_ready() {
        let store = store();
        let original = make_failed(&store, "retry me");
        let result = retry_task(&store, original.id, true, now()).expect("retry");
        let new_task = store.get(result.task_id).expect("get").expect("some");
        assert_eq!(new_task.status, Status::Ready);
    }

    #[test]
    fn retry_rewires_non_terminal_and_cascaded_cancelled_dependents_but_not_manual_cancels() {
        let store = store();
        // `original` は running のまま挿入し、apply_transition で failed に落として本物の cascade
        // （`DependencyFailed`）を起こす。
        let original = raw_task(Status::Running, vec![], None);
        store.insert(&original).expect("insert original");

        // 対話タスクは DependencyFailed の対象外（P-78）なので draft のまま残る。
        let conv_dependent = raw_task(Status::Draft, vec![original.id], Some(MessageId::new()));
        store
            .insert(&conv_dependent)
            .expect("insert conv dependent");

        // 普通の後続（ready）。original が failed になると cascade で cancelled(dependency_failed) になる。
        let normal_dependent = raw_task(Status::Ready, vec![original.id], None);
        store
            .insert(&normal_dependent)
            .expect("insert normal dependent");

        // 先に人が手動で cancel した後続。cascade は既に終端のタスクを飛ばす。
        let manually_cancelled = raw_task(Status::Ready, vec![original.id], None);
        store
            .insert(&manually_cancelled)
            .expect("insert manually cancelled");
        store
            .apply_transition(manually_cancelled.id, Trigger::Cancel, None)
            .expect("cancel");

        let outcome = store
            .apply_transition(original.id, Trigger::WorkerError { retryable: false }, None)
            .expect("worker_error");
        assert_eq!(outcome.next, Status::Failed);

        // cascade の前提を確認しておく。
        assert_eq!(
            store.get(conv_dependent.id).unwrap().unwrap().status,
            Status::Draft
        );
        assert_eq!(
            store.get(normal_dependent.id).unwrap().unwrap().status,
            Status::Cancelled
        );
        assert_eq!(
            store.get(manually_cancelled.id).unwrap().unwrap().status,
            Status::Cancelled
        );

        let result = retry_task(&store, original.id, false, now()).expect("retry");
        let mut rewired = result.rewired.clone();
        rewired.sort();
        let mut expected = vec![conv_dependent.id, normal_dependent.id];
        expected.sort();
        assert_eq!(rewired, expected);

        let conv_after = store.get(conv_dependent.id).expect("get").expect("some");
        assert_eq!(
            conv_after.status,
            Status::Draft,
            "対話タスクの状態は変えない"
        );
        assert_eq!(conv_after.depends_on, vec![result.task_id]);

        let normal_after = store.get(normal_dependent.id).expect("get").expect("some");
        assert_eq!(
            normal_after.status,
            Status::Draft,
            "dependency_failed による cancelled は draft に戻す"
        );
        assert_eq!(normal_after.depends_on, vec![result.task_id]);
        let normal_events: Vec<Event> = store
            .events_for(normal_dependent.id)
            .expect("events")
            .into_iter()
            .map(|(_, e)| e)
            .collect();
        assert!(normal_events.iter().any(|e| matches!(
            e,
            Event::Transitioned { from: Status::Cancelled, to: Status::Draft, reason } if reason == "retried"
        )));

        let manually_after = store
            .get(manually_cancelled.id)
            .expect("get")
            .expect("some");
        assert_eq!(
            manually_after.status,
            Status::Cancelled,
            "手動 cancel は対象外"
        );
        assert_eq!(manually_after.depends_on, vec![original.id], "張り替えない");
    }

    #[test]
    fn retry_rejects_non_terminal_or_done_tasks_with_409() {
        let store = store();
        let draft =
            crate::add::create_task(&store, base_spec("still draft"), now()).expect("create");
        let err = retry_task(&store, draft.id, false, now()).unwrap_err();
        assert!(matches!(err, OpsError::InvalidState { .. }));

        let done =
            crate::add::create_task(&store, base_spec("will be done"), now()).expect("create");
        store
            .apply_transition(done.id, Trigger::Accept, None)
            .expect("accept");
        store
            .apply_transition(done.id, Trigger::Dispatch, None)
            .expect("dispatch");
        store
            .apply_transition(done.id, Trigger::WorkerDone, None)
            .expect("worker_done");
        store
            .apply_transition(done.id, Trigger::ReviewPass, None)
            .expect("review_pass");
        let done = store.get(done.id).expect("get").expect("some");
        assert_eq!(done.status, Status::Done);
        let err = retry_task(&store, done.id, false, now()).unwrap_err();
        assert!(matches!(err, OpsError::InvalidState { .. }));
    }

    #[test]
    fn retry_missing_task_is_not_found() {
        let store = store();
        let err = retry_task(&store, TaskId::new(), false, now()).unwrap_err();
        assert!(matches!(err, OpsError::NotFound(_)));
    }
}
