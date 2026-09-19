//! `taskctl replay` — DESIGN.md §4.3 / §5.9, ADR-0002「結果」節, ADR-0004 D6。
//!
//! 出力整形と exit code だけをここで持つ。再構築ロジックは `task_ops::replay`（ADR-0013 D7）
//! に移した。

use std::process::ExitCode;

use clap::Args;
use task_core::TaskStore;
use task_ops::replay::replay;

use crate::error::CliError;
use crate::outln;

#[derive(Args, Debug)]
pub struct ReplayArgs {}

pub fn run(store: &dyn TaskStore, _args: ReplayArgs) -> Result<ExitCode, CliError> {
    let report = replay(store)?;

    for m in &report.mismatches {
        outln!(
            "MISMATCH task={} field={} replayed={} stored={}",
            m.task_id, m.field, m.replayed, m.stored
        );
    }
    outln!(
        "replay: {} mismatches across {} tasks",
        report.mismatches.len(),
        report.tasks
    );

    if report.mismatches.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Event, SqliteStore, Status};

    #[test]
    fn run_reports_success_when_no_tasks() {
        let store = SqliteStore::open_in_memory().expect("open");
        let result = run(&store, ReplayArgs {}).expect("run replay");
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn run_reports_failure_exit_code_on_drift() {
        let store = SqliteStore::open_in_memory().expect("open");
        let now = time::OffsetDateTime::now_utc();
        let task = task_core::Task {
            repos: Vec::new(),
            id: task_core::TaskId::new(),
            parent_id: None,
            kind: task_core::TaskKind::Execute,
            title: "t".to_string(),
            objective: "o".to_string(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Draft,
            priority: 0,
            worker_hint: task_core::WorkerHint {
                tier: task_core::Tier::Standard,
                adapter: None,
            },
            workspace: task_core::WorkspaceSpec::Local {
                path: "/tmp/ws".into(), mode: None,
            },
            budget: task_core::Budget {
                max_turns: 1,
                max_wall_secs: 1,
                max_retries: 1,
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
        };
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
                    from: Status::Draft,
                    to: Status::Ready,
                    reason: "accept".to_string(),
                },
            )
            .expect("append transitioned");

        let result = run(&store, ReplayArgs {}).expect("run replay");
        assert_eq!(result, ExitCode::FAILURE);
    }
}
