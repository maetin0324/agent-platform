//! `taskctl add` — DESIGN.md §5.9 / ADR-0004 D4。
//!
//! `AddArgs` から `Task` を組み立てて `insert` し、同一内容を `Event::Created` として追記する。
//! 初期 `status` は ADR-0002 D4 のとおり `kind == Approval` なら `Ready`、それ以外は `Draft`。
//! `accept` の各要素は Phase 2 では常に `Criterion{ text, check: Check::Human }` になる
//! （自動検証は未実装、ADR-0004 D4）。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use task_core::{
    Budget, Check, Criterion, Event, Status, Task, TaskId, TaskKind, Tier, TaskStore, WorkerHint,
    WorkspaceSpec,
};
use time::OffsetDateTime;

use crate::error::{CliError, parse_task_id};

#[derive(Args, Debug)]
pub struct AddArgs {
    #[arg(long)]
    pub title: String,

    #[arg(long)]
    pub objective: String,

    /// 受け入れ条件。複数回指定できる（最低1つ必須）。
    #[arg(long = "accept", required = true)]
    pub accept: Vec<String>,

    #[arg(long, value_enum, default_value = "execute")]
    pub kind: KindArg,

    #[arg(long, value_enum, default_value = "standard")]
    pub tier: TierArg,

    #[arg(long, default_value_t = 0)]
    pub priority: i32,

    /// 親タスクの ID（省略可）。
    #[arg(long)]
    pub parent: Option<String>,

    /// 依存する先行タスクの ID。複数回指定できる。
    #[arg(long = "depends-on")]
    pub depends_on: Vec<String>,

    #[arg(long, default_value_t = 10)]
    pub max_turns: u32,

    #[arg(long, default_value_t = 600)]
    pub max_wall_secs: u64,

    #[arg(long, default_value_t = 2)]
    pub max_retries: u32,

    /// ワークスペースのローカルパス。省略時はカレントディレクトリ。
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum KindArg {
    Plan,
    Execute,
    Review,
    Approval,
}

impl From<KindArg> for TaskKind {
    fn from(value: KindArg) -> Self {
        match value {
            KindArg::Plan => TaskKind::Plan,
            KindArg::Execute => TaskKind::Execute,
            KindArg::Review => TaskKind::Review,
            KindArg::Approval => TaskKind::Approval,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TierArg {
    Frontier,
    Standard,
    Cheap,
}

impl From<TierArg> for Tier {
    fn from(value: TierArg) -> Self {
        match value {
            TierArg::Frontier => Tier::Frontier,
            TierArg::Standard => Tier::Standard,
            TierArg::Cheap => Tier::Cheap,
        }
    }
}

pub fn run(store: &dyn TaskStore, args: AddArgs) -> Result<ExitCode, CliError> {
    let kind: TaskKind = args.kind.into();
    let tier: Tier = args.tier.into();

    let status = if kind == TaskKind::Approval {
        Status::Ready
    } else {
        Status::Draft
    };

    let acceptance: Vec<Criterion> = args
        .accept
        .into_iter()
        .map(|text| Criterion {
            text,
            check: Check::Human,
        })
        .collect();

    let parent_id = match args.parent {
        Some(s) => Some(parse_task_id(&s)?),
        None => None,
    };

    let depends_on: Vec<TaskId> = args
        .depends_on
        .iter()
        .map(|s| parse_task_id(s))
        .collect::<Result<_, _>>()?;

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
        parent_id,
        kind,
        title: args.title,
        objective: args.objective,
        acceptance,
        inputs: vec![],
        depends_on,
        status,
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

    fn base_args() -> AddArgs {
        AddArgs {
            title: "do something".to_string(),
            objective: "make it work".to_string(),
            accept: vec!["it works".to_string()],
            kind: KindArg::Execute,
            tier: TierArg::Standard,
            priority: 0,
            parent: None,
            depends_on: vec![],
            max_turns: 10,
            max_wall_secs: 600,
            max_retries: 2,
            workspace: Some(PathBuf::from("/tmp/workspace")),
        }
    }

    #[test]
    fn run_inserts_task_and_created_event() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let args = base_args();

        let result = run(&store, args).expect("run add");
        assert_eq!(result, ExitCode::SUCCESS);

        let tasks = store.list(None).expect("list tasks");
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];

        assert_eq!(task.title, "do something");
        assert_eq!(task.objective, "make it work");
        assert_eq!(task.kind, TaskKind::Execute);
        assert_eq!(task.status, Status::Draft);
        assert_eq!(task.worker_hint.tier, Tier::Standard);
        assert_eq!(task.worker_hint.adapter, None);
        assert_eq!(task.budget.max_turns, 10);
        assert_eq!(task.budget.max_wall_secs, 600);
        assert_eq!(task.budget.max_retries, 2);
        assert_eq!(task.attempts, 0);
        assert!(task.lease.is_none());
        assert_eq!(
            task.workspace,
            WorkspaceSpec::Local {
                path: PathBuf::from("/tmp/workspace")
            }
        );

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched, *task);

        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(events.len(), 1);
        match &events[0].1 {
            Event::Created { task: created } => assert_eq!(created.id, task.id),
            other => panic!("expected Created event, got {other:?}"),
        }
    }

    #[test]
    fn run_with_approval_kind_starts_ready() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.kind = KindArg::Approval;

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, Status::Ready);
    }

    #[test]
    fn run_with_multiple_accept_creates_multiple_criteria() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.accept = vec![
            "criterion one".to_string(),
            "criterion two".to_string(),
            "criterion three".to_string(),
        ];

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!(task.acceptance.len(), 3);
        assert_eq!(task.acceptance[0].text, "criterion one");
        assert_eq!(task.acceptance[1].text, "criterion two");
        assert_eq!(task.acceptance[2].text, "criterion three");
        for criterion in &task.acceptance {
            assert_eq!(criterion.check, Check::Human);
        }
    }

    #[test]
    fn run_with_invalid_parent_id_returns_error() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.parent = Some("not-a-valid-id".to_string());

        let result = run(&store, args);
        assert!(matches!(result, Err(CliError::Message(_))));
    }

    #[test]
    fn run_without_workspace_uses_current_dir() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.workspace = None;

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        assert_eq!(tasks.len(), 1);
        let expected = std::env::current_dir().expect("current dir");
        assert_eq!(
            tasks[0].workspace,
            WorkspaceSpec::Local { path: expected }
        );
    }
}
