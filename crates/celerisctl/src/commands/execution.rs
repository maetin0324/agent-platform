//! `celerisctl execution plan set|show`（ADR-0072 D14。Phase E2）。
//!
//! `set` は JSON ファイル（または `-` で stdin）から `celeris.execution-plan/1` を読み、D14 の検証を
//! 通してから採用する（`task_ops::execution::adopt_plan`、origin は常に `human`）。`show` は現在の
//! `active` な計画と WorkUnit を出す。判断は `task_ops::execution` にあり、ここは引数解析・I/O・
//! 出力整形だけ（ADR-0013 D7）。

use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use task_core::{ExecutionLimits, PlanOrigin, TaskStore};
use time::OffsetDateTime;

use crate::error::{CliError, parse_task_id};
use crate::outln;

#[derive(Subcommand, Debug)]
pub enum ExecutionCommand {
    Plan {
        #[command(subcommand)]
        command: ExecutionPlanCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum ExecutionPlanCommand {
    /// 計画を採用する（origin は常に human）。E2 は新規のみ: 既に active な計画があれば失敗する。
    Set(ExecutionPlanSetArgs),
    /// 現在の active な計画と WorkUnit を表示する。
    Show(ExecutionPlanShowArgs),
}

#[derive(Args, Debug)]
pub struct ExecutionPlanSetArgs {
    pub task_id: String,
    /// `celeris.execution-plan/1` の JSON ファイル。`-` で stdin から読む。
    #[arg(long)]
    pub file: PathBuf,
}

#[derive(Args, Debug)]
pub struct ExecutionPlanShowArgs {
    pub task_id: String,
}

pub fn run(store: &dyn TaskStore, command: ExecutionCommand) -> Result<ExitCode, CliError> {
    match command {
        ExecutionCommand::Plan { command } => match command {
            ExecutionPlanCommand::Set(args) => run_set(store, args),
            ExecutionPlanCommand::Show(args) => run_show(store, args),
        },
    }
}

fn read_plan_file(path: &PathBuf) -> Result<String, CliError> {
    if path.as_os_str() == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| CliError::msg(format!("cannot read stdin: {e}")))?;
        Ok(text)
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| CliError::msg(format!("cannot read {}: {e}", path.display())))
    }
}

fn run_set(store: &dyn TaskStore, args: ExecutionPlanSetArgs) -> Result<ExitCode, CliError> {
    let task_id = parse_task_id(&args.task_id)?;
    let text = read_plan_file(&args.file)?;
    let spec: task_core::ExecutionPlanSpec = serde_json::from_str(&text)
        .map_err(|e| CliError::msg(format!("invalid execution plan JSON: {e}")))?;
    let plan = task_ops::execution::adopt_plan(
        store,
        task_id,
        spec,
        PlanOrigin::Human,
        None,
        ExecutionLimits::default(),
        OffsetDateTime::now_utc(),
    )?;
    outln!(
        "{} version={} status={} work_units={}",
        plan.id,
        plan.version,
        plan.status.as_str(),
        plan.spec.work_units.len()
    );
    Ok(ExitCode::SUCCESS)
}

fn run_show(store: &dyn TaskStore, args: ExecutionPlanShowArgs) -> Result<ExitCode, CliError> {
    let task_id = parse_task_id(&args.task_id)?;
    let Some(view) = task_ops::execution::active_plan(store, task_id)? else {
        outln!("{task_id}: no execution plan");
        return Ok(ExitCode::SUCCESS);
    };
    outln!(
        "{} version={} origin={} status={}",
        view.plan.id,
        view.plan.version,
        view.plan.origin.as_str(),
        view.plan.status.as_str()
    );
    for wu in &view.work_units {
        let blocked = wu
            .blocked_reason
            .map(|r| format!(" blocked={}", r.as_str()))
            .unwrap_or_default();
        outln!(
            "  {} seq={} key={} kind={} status={}{} runs={} continuations={} retries={} depends_on={:?}",
            wu.id,
            wu.seq,
            wu.key,
            wu.kind.as_str(),
            wu.status.as_str(),
            blocked,
            wu.runs,
            wu.continuations,
            wu.retries,
            wu.depends_on
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use task_core::{Budget, SqliteStore, Status, TaskId, TaskKind, WorkerHint, WorkspaceSpec};

    fn sample_task() -> task_core::Task {
        let now = OffsetDateTime::now_utc();
        task_core::Task {
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "title".to_string(),
            objective: "objective".to_string(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Draft,
            priority: 0,
            worker_hint: WorkerHint {
                tier: task_core::Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "/tmp".into(),
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

    fn seed_task(store: &dyn TaskStore) -> TaskId {
        let task = sample_task();
        store.insert(&task).unwrap();
        task.id
    }

    fn plan_json() -> &'static str {
        r#"{
            "schema": "celeris.execution-plan/1",
            "rationale": "A then B",
            "work_units": [
                {"key": "a", "kind": "implement", "title": "A", "objective": "do A thoroughly"},
                {"key": "b", "kind": "implement", "title": "B", "objective": "do B thoroughly", "depends_on": ["a"]}
            ]
        }"#
    }

    #[test]
    fn set_then_show_round_trips() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task_id = seed_task(&store);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        std::fs::write(&path, plan_json()).unwrap();

        let result = run_set(
            &store,
            ExecutionPlanSetArgs {
                task_id: task_id.to_string(),
                file: path,
            },
        )
        .unwrap();
        assert_eq!(result, ExitCode::SUCCESS);

        let units = store.work_units_for(task_id).unwrap();
        assert_eq!(units.len(), 2);

        let result = run_show(
            &store,
            ExecutionPlanShowArgs {
                task_id: task_id.to_string(),
            },
        )
        .unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn show_without_a_plan_succeeds_and_says_so() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task_id = seed_task(&store);
        let result = run_show(
            &store,
            ExecutionPlanShowArgs {
                task_id: task_id.to_string(),
            },
        )
        .unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn set_rejects_an_invalid_plan() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task_id = seed_task(&store);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            r#"{{"schema":"celeris.execution-plan/1","rationale":"x","work_units":[]}}"#
        )
        .unwrap();

        let err = run_set(
            &store,
            ExecutionPlanSetArgs {
                task_id: task_id.to_string(),
                file: path,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CliError::Message(_)), "{err:?}");
    }

    #[test]
    fn set_rejects_an_unknown_task() {
        let store = SqliteStore::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        std::fs::write(&path, plan_json()).unwrap();

        let err = run_set(
            &store,
            ExecutionPlanSetArgs {
                task_id: TaskId::new().to_string(),
                file: path,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CliError::Message(_)), "{err:?}");
    }
}
