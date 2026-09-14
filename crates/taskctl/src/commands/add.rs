//! `taskctl add` — DESIGN.md §5.9 / ADR-0004 D4 / ADR-0010 D4（P-17, P-19）。
//!
//! `AddArgs` から `Task` を組み立て、`TaskStore::create_task` で `insert` + `Event::Created`
//! を単一トランザクションとして書き込む（ADR-0010 D2）。初期 `status` は ADR-0002 D4 のとおり
//! `kind == Approval` なら `Ready`、それ以外は `Draft`。
//!
//! 受け入れ条件は `--accept`（`Check::Human`）、`--check-cmd`（`Check::Command{expect_exit:0}`）、
//! `--check-artifact`（`Check::ArtifactExists`）、`--check-reviewer`（`Check::Reviewer`）のいずれも
//! 複数回指定でき、`acceptance` の並びは accept → cmd → artifact → reviewer の固定順になる。
//! 4 種の合計が 1 つも無ければエラー（`--accept` 単独必須ではなくなった。ADR-0010 D4）。
//!
//! `--depends-on` に渡した各 ID は、存在しないか `failed`/`cancelled` ならエラーにし、
//! 何も挿入しない（挿入した瞬間に後続が永久に進まない状態を作らないため）。
//!
//! `--workspace` を省略した場合は `WorkspaceSpec::Local{ path: "<task_id>" }`（相対パス）に
//! なる。ディスパッチャが `workspace_root` 基準で解決する（ADR-0005 D3, ADR-0010 D4, P-19）。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use task_core::{
    Budget, Check, Criterion, Status, Task, TaskId, TaskKind, Tier, TaskStore, WorkerHint,
    WorkspaceSpec,
};
use time::OffsetDateTime;

use crate::error::{CliError, parse_task_id};
use crate::outln;

#[derive(Args, Debug)]
pub struct AddArgs {
    #[arg(long)]
    pub title: String,

    #[arg(long)]
    pub objective: String,

    /// 人間が確認する受け入れ条件（`Check::Human`）。複数回指定できる。
    #[arg(long = "accept")]
    pub accept: Vec<String>,

    /// コマンドが exit 0 で終わることを条件にする（`Check::Command`）。複数回指定できる。
    #[arg(long = "check-cmd")]
    pub check_cmd: Vec<String>,

    /// 成果物が存在することを条件にする（`Check::ArtifactExists`）。複数回指定できる。
    #[arg(long = "check-artifact")]
    pub check_artifact: Vec<String>,

    /// レビュアー（別 LLM 実行）による判定を条件にする（`Check::Reviewer`）。複数回指定できる。
    #[arg(long = "check-reviewer")]
    pub check_reviewer: Vec<String>,

    #[arg(long, value_enum, default_value = "execute")]
    pub kind: KindArg,

    #[arg(long, value_enum, default_value = "standard")]
    pub tier: TierArg,

    #[arg(long, default_value_t = 0)]
    pub priority: i32,

    /// 親タスクの ID（省略可）。
    #[arg(long)]
    pub parent: Option<String>,

    /// 依存する先行タスクの ID。複数回指定できる。存在しない、または failed/cancelled ならエラー。
    #[arg(long = "depends-on")]
    pub depends_on: Vec<String>,

    #[arg(long, default_value_t = 10)]
    pub max_turns: u32,

    #[arg(long, default_value_t = 600)]
    pub max_wall_secs: u64,

    #[arg(long, default_value_t = 2)]
    pub max_retries: u32,

    /// ワークスペースのローカルパス。省略時は `<task_id>`（相対パス、P-19）。
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

/// `--accept`/`--check-cmd`/`--check-artifact`/`--check-reviewer` から `acceptance` を
/// 固定順（accept → cmd → artifact → reviewer）で組み立てる。1 つも無ければエラー。
fn build_acceptance(args: &mut AddArgs) -> Result<Vec<Criterion>, CliError> {
    let mut acceptance = Vec::new();
    acceptance.extend(std::mem::take(&mut args.accept).into_iter().map(|text| Criterion {
        text,
        check: Check::Human,
    }));
    acceptance.extend(std::mem::take(&mut args.check_cmd).into_iter().map(|cmd| Criterion {
        text: format!("`{cmd}` exits 0"),
        check: Check::Command { cmd, expect_exit: 0 },
    }));
    acceptance.extend(
        std::mem::take(&mut args.check_artifact)
            .into_iter()
            .map(|name| Criterion {
                text: format!("artifact {name} exists"),
                check: Check::ArtifactExists { name },
            }),
    );
    acceptance.extend(
        std::mem::take(&mut args.check_reviewer)
            .into_iter()
            .map(|text| Criterion {
                text,
                check: Check::Reviewer,
            }),
    );

    if acceptance.is_empty() {
        return Err(CliError::msg(
            "at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)",
        ));
    }
    Ok(acceptance)
}

/// `depends_on` の各 ID が存在し、かつ `failed`/`cancelled` でないことを検証する
/// （ADR-0010 D4）。違反があれば挿入前にエラーを返す。
fn validate_depends_on(store: &dyn TaskStore, depends_on: &[TaskId]) -> Result<(), CliError> {
    for dep_id in depends_on {
        match store.get(*dep_id)? {
            None => {
                return Err(CliError::msg(format!(
                    "dependency {dep_id} does not exist"
                )));
            }
            Some(dep) if matches!(dep.status, Status::Failed | Status::Cancelled) => {
                return Err(CliError::msg(format!(
                    "dependency {dep_id} has status {:?} and cannot be depended on",
                    dep.status
                )));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

pub fn run(store: &dyn TaskStore, mut args: AddArgs) -> Result<ExitCode, CliError> {
    let kind: TaskKind = args.kind.into();
    let tier: Tier = args.tier.into();

    let status = if kind == TaskKind::Approval {
        Status::Ready
    } else {
        Status::Draft
    };

    let acceptance = build_acceptance(&mut args)?;

    let parent_id = match &args.parent {
        Some(s) => Some(parse_task_id(s)?),
        None => None,
    };

    let depends_on: Vec<TaskId> = args
        .depends_on
        .iter()
        .map(|s| parse_task_id(s))
        .collect::<Result<_, _>>()?;
    validate_depends_on(store, &depends_on)?;

    let id = TaskId::new();
    let workspace = match args.workspace {
        Some(path) => WorkspaceSpec::Local { path },
        None => WorkspaceSpec::Local {
            path: PathBuf::from(id.to_string()),
        },
    };

    let budget = Budget {
        max_turns: args.max_turns,
        max_wall_secs: args.max_wall_secs,
        max_retries: args.max_retries,
    };

    let now = OffsetDateTime::now_utc();

    let task = Task {
        id,
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

    store.create_task(&task, vec![])?;

    outln!("{}", task.id);
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Event, SqliteStore};

    fn base_args() -> AddArgs {
        AddArgs {
            title: "do something".to_string(),
            objective: "make it work".to_string(),
            accept: vec!["it works".to_string()],
            check_cmd: vec![],
            check_artifact: vec![],
            check_reviewer: vec![],
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
    fn run_without_workspace_defaults_to_relative_task_id_path() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.workspace = None;

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!(
            task.workspace,
            WorkspaceSpec::Local {
                path: PathBuf::from(task.id.to_string())
            }
        );
    }

    #[test]
    fn run_check_cmd_produces_command_criterion_with_expected_text() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.accept = vec![];
        args.check_cmd = vec!["cargo test".to_string()];

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!(task.acceptance.len(), 1);
        assert_eq!(task.acceptance[0].text, "`cargo test` exits 0");
        assert_eq!(
            task.acceptance[0].check,
            Check::Command {
                cmd: "cargo test".to_string(),
                expect_exit: 0
            }
        );
    }

    #[test]
    fn run_check_artifact_produces_artifact_exists_criterion_with_expected_text() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.accept = vec![];
        args.check_artifact = vec!["bench.json".to_string()];

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        let task = &tasks[0];
        assert_eq!(task.acceptance.len(), 1);
        assert_eq!(task.acceptance[0].text, "artifact bench.json exists");
        assert_eq!(
            task.acceptance[0].check,
            Check::ArtifactExists {
                name: "bench.json".to_string()
            }
        );
    }

    #[test]
    fn run_check_reviewer_produces_reviewer_criterion_with_text_verbatim() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.accept = vec![];
        args.check_reviewer = vec!["the diff is minimal and well-tested".to_string()];

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        let task = &tasks[0];
        assert_eq!(task.acceptance.len(), 1);
        assert_eq!(
            task.acceptance[0].text,
            "the diff is minimal and well-tested"
        );
        assert_eq!(task.acceptance[0].check, Check::Reviewer);
    }

    #[test]
    fn run_acceptance_order_is_accept_then_cmd_then_artifact_then_reviewer() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.accept = vec!["human check".to_string()];
        args.check_cmd = vec!["cargo test".to_string()];
        args.check_artifact = vec!["bench.json".to_string()];
        args.check_reviewer = vec!["looks good".to_string()];

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        let task = &tasks[0];
        assert_eq!(task.acceptance.len(), 4);
        assert_eq!(task.acceptance[0].check, Check::Human);
        assert!(matches!(task.acceptance[1].check, Check::Command { .. }));
        assert!(matches!(
            task.acceptance[2].check,
            Check::ArtifactExists { .. }
        ));
        assert_eq!(task.acceptance[3].check, Check::Reviewer);
    }

    #[test]
    fn run_without_any_acceptance_criterion_errors_and_inserts_nothing() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.accept = vec![];

        let result = run(&store, args);
        assert!(matches!(result, Err(CliError::Message(_))));
        assert!(store.list(None).expect("list tasks").is_empty());
    }

    #[test]
    fn run_with_missing_dependency_errors_and_inserts_nothing() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.depends_on = vec![TaskId::new().to_string()];

        let result = run(&store, args);
        assert!(result.is_err());
        assert!(store.list(None).expect("list tasks").is_empty());
    }

    #[test]
    fn run_with_failed_dependency_errors_and_inserts_nothing() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut dep_args = base_args();
        dep_args.workspace = Some(PathBuf::from("/tmp/dep"));
        run(&store, dep_args).expect("run add dep");
        let dep_id = store.list(None).expect("list tasks")[0].id;

        // Drive the dependency to `failed` via a valid path:
        // draft -> accept -> ready -> acquire_lease -> running -> worker_error(false) -> failed.
        store
            .apply_transition(dep_id, task_core::Trigger::Accept, None)
            .expect("accept dep");
        let acquired = store
            .acquire_lease(dep_id, "run-dep", std::time::Duration::from_secs(60))
            .expect("acquire lease");
        assert!(acquired);
        store
            .apply_transition(
                dep_id,
                task_core::Trigger::WorkerError { retryable: false },
                None,
            )
            .expect("fail dep");
        assert_eq!(
            store.get(dep_id).expect("get").expect("some").status,
            Status::Failed
        );

        let mut args = base_args();
        args.depends_on = vec![dep_id.to_string()];

        let result = run(&store, args);
        assert!(result.is_err());
        // Only the dependency task should exist; the new task must not be inserted.
        assert_eq!(store.list(None).expect("list tasks").len(), 1);
    }
}
