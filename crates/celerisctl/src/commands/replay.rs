//! `celerisctl replay` — DESIGN.md §4.3 / §5.9, ADR-0002「結果」節, ADR-0004 D6。
//! ADR-0072 D5/D15（Phase E2b）: `--check`/`--apply` で `work_units`/`runs` の再構築も行う。
//! ADR-0072 D17（Phase E4b 項目5）: `execution_plans`（replan で複数版になった版の履歴）も同様。
//!
//! 出力整形と exit code だけをここで持つ。再構築ロジックは `task_ops::replay`（ADR-0013 D7）
//! に移した。

use std::process::ExitCode;

use clap::Args;
use task_core::TaskStore;
use task_ops::replay::{check_and_apply_execution, replay};

use crate::error::CliError;
use crate::outln;

#[derive(Args, Debug, Default)]
pub struct ReplayArgs {
    /// events から `work_units`/`runs` を再構築し、現在の索引と突き合わせて差分を表示する
    /// （`task_id`/`status`/`attempts` の突き合わせは既定で常に行う）。
    #[arg(long)]
    pub check: bool,
    /// `--check` の差分があれば、`work_units`/`runs` の索引を再構築した結果で上書きする
    /// （ADR-0072 D15: events が勝つ。`events` そのものは変えない。`--check` を暗黙に含む）。
    #[arg(long)]
    pub apply: bool,
}

pub fn run(store: &dyn TaskStore, args: ReplayArgs) -> Result<ExitCode, CliError> {
    let report = replay(store)?;

    for m in &report.mismatches {
        outln!(
            "MISMATCH task={} field={} replayed={} stored={}",
            m.task_id,
            m.field,
            m.replayed,
            m.stored
        );
    }

    let mut total_mismatches = report.mismatches.len();
    if args.check || args.apply {
        let (wu_mismatches, run_mismatches, plan_mismatches, applied) =
            check_and_apply_execution(store, args.apply)?;
        for m in &wu_mismatches {
            outln!(
                "WORK_UNIT_MISMATCH task={} key={} field={} replayed={} stored={}",
                m.task_id,
                m.key,
                m.field,
                m.replayed,
                m.stored
            );
        }
        for m in &run_mismatches {
            outln!(
                "RUN_MISMATCH task={} run_id={} field={} replayed={} stored={}",
                m.task_id,
                m.run_id,
                m.field,
                m.replayed,
                m.stored
            );
        }
        // ADR-0072 D5/D17（Phase E4b 項目5）: `execution_plans`（版の履歴）の食い違い。
        for m in &plan_mismatches {
            outln!(
                "EXECUTION_PLAN_MISMATCH task={} version={} field={} replayed={} stored={}",
                m.task_id,
                m.version,
                m.field,
                m.replayed,
                m.stored
            );
        }
        total_mismatches += wu_mismatches.len() + run_mismatches.len() + plan_mismatches.len();
        if args.apply {
            outln!(
                "replay: applied execution index fixes for {} task(s)",
                applied
            );
        }
    }

    outln!(
        "replay: {} mismatches across {} tasks",
        total_mismatches,
        report.tasks
    );

    if total_mismatches == 0 {
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
        let result = run(&store, ReplayArgs::default()).expect("run replay");
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn run_reports_failure_exit_code_on_drift() {
        let store = SqliteStore::open_in_memory().expect("open");
        let now = time::OffsetDateTime::now_utc();
        let task = task_core::Task {
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
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
                path: "/tmp/ws".into(),
                mode: None,
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

        let result = run(&store, ReplayArgs::default()).expect("run replay");
        assert_eq!(result, ExitCode::FAILURE);
    }
}
