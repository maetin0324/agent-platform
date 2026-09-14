//! `taskctl plan` — DESIGN.md §5.9 / ADR-0007 D6。
//!
//! 大目標を表す文字列 1 つから根の `Plan` タスクを組み立てて `insert` し、
//! 同一内容を `Event::Created` として追記する。子タスクの生成はプランナー
//! （ワーカー）の出力を Reviewer が検証・展開する経路で行うため、ここでは
//! `acceptance = []`（暗黙のプラン検証条件のみ。ADR-0007 D4）で `Draft` の
//! 1 タスクを作るだけに留める。`--parent` は受け付けない（根の Plan のみ）。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use task_core::{
    Budget, Event, Status, Task, TaskId, TaskKind, TaskStore, Tier, WorkerHint, WorkspaceSpec,
};
use time::OffsetDateTime;

use crate::commands::add::TierArg;
use crate::error::CliError;

const TITLE_MAX_CHARS: usize = 80;
const DEFAULT_MAX_TURNS: u32 = 30;
const DEFAULT_MAX_WALL_SECS: u64 = 900;
const DEFAULT_MAX_RETRIES: u32 = 1;

#[derive(Args, Debug)]
pub struct PlanArgs {
    /// 大目標。1行目の先頭80文字が `title` になる。
    pub goal: String,

    /// ワークスペースのローカルパス。省略時はカレントディレクトリ。
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    #[arg(long, value_enum, default_value = "frontier")]
    pub tier: TierArg,

    #[arg(long, default_value_t = 0)]
    pub priority: i32,

    #[arg(long, default_value_t = DEFAULT_MAX_TURNS)]
    pub max_turns: u32,

    #[arg(long, default_value_t = DEFAULT_MAX_WALL_SECS)]
    pub max_wall_secs: u64,

    #[arg(long, default_value_t = DEFAULT_MAX_RETRIES)]
    pub max_retries: u32,
}

/// 文字列の1行目を、char境界を保ったまま先頭 `max_chars` 文字に切り詰める。
fn truncate_title(goal: &str, max_chars: usize) -> String {
    let first_line = goal.lines().next().unwrap_or("");
    first_line.chars().take(max_chars).collect()
}

pub fn run(store: &dyn TaskStore, args: PlanArgs) -> Result<ExitCode, CliError> {
    if args.goal.trim().is_empty() {
        return Err(CliError::msg("goal must not be blank"));
    }

    let tier: Tier = args.tier.into();
    let title = truncate_title(&args.goal, TITLE_MAX_CHARS);

    let workspace = match args.workspace {
        Some(path) => WorkspaceSpec::Local { path },
        None => {
            let path = std::env::current_dir()
                .map_err(|e| CliError::msg(format!("failed to get current directory: {e}")))?;
            WorkspaceSpec::Local { path }
        }
    };

    let budget = Budget {
        max_turns: args.max_turns,
        max_wall_secs: args.max_wall_secs,
        max_retries: args.max_retries,
    };

    let now = OffsetDateTime::now_utc();

    let task = Task {
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Plan,
        title,
        objective: args.goal,
        acceptance: vec![],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Draft,
        priority: args.priority,
        worker_hint: WorkerHint {
            tier,
            adapter: None,
        },
        workspace,
        budget,
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
    };

    store.insert(&task)?;
    store.append_event(
        task.id,
        &Event::Created {
            task: Box::new(task.clone()),
        },
    )?;

    println!("{}", task.id);
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::SqliteStore;

    fn base_args(goal: &str) -> PlanArgs {
        PlanArgs {
            goal: goal.to_string(),
            workspace: Some(PathBuf::from("/tmp/workspace")),
            tier: TierArg::Frontier,
            priority: 0,
            max_turns: DEFAULT_MAX_TURNS,
            max_wall_secs: DEFAULT_MAX_WALL_SECS,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }

    #[test]
    fn run_inserts_plan_task_and_created_event() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let goal = "add CLI argument parsing to hello-crate".to_string();
        let args = base_args(&goal);

        let result = run(&store, args).expect("run plan");
        assert_eq!(result, ExitCode::SUCCESS);

        let tasks = store.list(None).expect("list tasks");
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];

        assert_eq!(task.kind, TaskKind::Plan);
        assert_eq!(task.status, Status::Draft);
        assert!(task.acceptance.is_empty());
        assert_eq!(task.worker_hint.tier, Tier::Frontier);
        assert_eq!(task.worker_hint.adapter, None);
        assert_eq!(task.budget.max_turns, DEFAULT_MAX_TURNS);
        assert_eq!(task.budget.max_wall_secs, DEFAULT_MAX_WALL_SECS);
        assert_eq!(task.budget.max_retries, DEFAULT_MAX_RETRIES);
        assert_eq!(task.objective, goal);
        assert_eq!(task.title, goal);
        assert!(task.parent_id.is_none());

        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(events.len(), 1);
        match &events[0].1 {
            Event::Created { task: created } => assert_eq!(created.id, task.id),
            other => panic!("expected Created event, got {other:?}"),
        }
    }

    #[test]
    fn run_truncates_multiline_goal_title_but_keeps_full_objective() {
        let store = SqliteStore::open_in_memory().expect("open store");
        // 1行目にマルチバイト文字を含み、80文字を超える長さにする。
        let first_line: String = "目".repeat(90);
        let goal = format!("{first_line}\nsecond line\nthird line");
        let args = base_args(&goal);

        run(&store, args).expect("run plan");

        let tasks = store.list(None).expect("list tasks");
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];

        let expected_title: String = first_line.chars().take(TITLE_MAX_CHARS).collect();
        assert_eq!(task.title, expected_title);
        assert_eq!(task.title.chars().count(), TITLE_MAX_CHARS);
        assert_eq!(task.objective, goal);
    }

    #[test]
    fn run_with_blank_goal_returns_error() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let args = base_args("   \n  \t ");

        let result = run(&store, args);
        assert!(matches!(result, Err(CliError::Message(_))));
    }

    #[test]
    fn run_with_custom_tier_priority_and_retries() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args("do something useful");
        args.tier = TierArg::Standard;
        args.max_retries = 0;
        args.priority = 5;

        run(&store, args).expect("run plan");

        let tasks = store.list(None).expect("list tasks");
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!(task.worker_hint.tier, Tier::Standard);
        assert_eq!(task.budget.max_retries, 0);
        assert_eq!(task.priority, 5);
    }
}
