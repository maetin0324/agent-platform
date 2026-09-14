//! `taskctl cancel` — DESIGN.md §5.9 / ADR-0010 D4（P-18）。
//!
//! 非終端（`draft/ready/running/blocked/reviewing`）のタスクだけを `Trigger::Cancel` で
//! `cancelled` にする。終端（`done/failed/cancelled`）はエラーにして exit 1、状態は変えない
//! （ADR-0010 D1, P-4）。子・後続タスクへの取り消し伝播（`Approval` の子、`depends_on` の
//! 後続）は `TaskStore::apply_transition` がストア側の同一トランザクションで行うため、
//! CLI 側では何もしない（ADR-0010 D2）。

use std::process::ExitCode;

use clap::Args;
use task_core::{TaskStore, Trigger};

use crate::error::CliError;
use crate::outln;

#[derive(Args, Debug)]
pub struct CancelArgs {
    pub id: String,
}

pub fn run(store: &dyn TaskStore, args: CancelArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;
    let task = store
        .get(id)?
        .ok_or_else(|| CliError::msg(format!("task not found: {id}")))?;

    if task.status.is_terminal() {
        return Err(CliError::msg(format!(
            "task {id} (status={:?}) cannot be cancelled",
            task.status
        )));
    }

    let outcome = store.apply_transition(id, Trigger::Cancel, None)?;
    outln!("{:?}", outcome.next);
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;
    use task_core::{
        ArtifactRef, Budget, Check, Criterion, SqliteStore, Status, Task, TaskId, TaskKind, Tier,
        WorkerHint, WorkspaceSpec,
    };
    use time::OffsetDateTime;

    fn sample_task(status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
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
            }],
            depends_on: vec![],
            status,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "/tmp/workspace".into(),
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
        }
    }

    #[test]
    fn run_cancels_draft_task() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");

        let result = run(
            &store,
            CancelArgs {
                id: task.id.to_string(),
            },
        )
        .expect("run cancel");
        assert_eq!(result, ExitCode::SUCCESS);

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Cancelled);
    }

    #[test]
    fn run_cancels_ready_task() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(Status::Ready);
        store.insert(&task).expect("insert");

        run(
            &store,
            CancelArgs {
                id: task.id.to_string(),
            },
        )
        .expect("run cancel");

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Cancelled);
    }

    #[test]
    fn run_cancels_running_task_acquired_via_lease() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(Status::Ready);
        store.insert(&task).expect("insert");
        let acquired = store
            .acquire_lease(task.id, "run-1", StdDuration::from_secs(60))
            .expect("acquire_lease");
        assert!(acquired);

        run(
            &store,
            CancelArgs {
                id: task.id.to_string(),
            },
        )
        .expect("run cancel");

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Cancelled);
        assert!(fetched.lease.is_none());
    }

    #[test]
    fn run_on_terminal_task_errors_and_leaves_status_unchanged() {
        let store = SqliteStore::open_in_memory().expect("open store");
        for status in [Status::Done, Status::Failed, Status::Cancelled] {
            let task = sample_task(status);
            store.insert(&task).expect("insert");

            let result = run(
                &store,
                CancelArgs {
                    id: task.id.to_string(),
                },
            );
            assert!(result.is_err(), "cancel of {status:?} should fail");

            let fetched = store.get(task.id).expect("get").expect("some");
            assert_eq!(fetched.status, status);
        }
    }

    #[test]
    fn run_on_missing_task_errors() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let missing_id = TaskId::new().to_string();

        let result = run(&store, CancelArgs { id: missing_id });
        assert!(result.is_err());
    }
}
