//! `taskctl add` — DESIGN.md §5.9 / ADR-0004 D4 / ADR-0010 D4（P-17, P-19）。
//!
//! CLI 引数の解析（フラグ → `CriterionSpec` の固定順: accept → cmd → artifact → reviewer）と
//! 出力整形だけをここで行う。判断と検証（受け入れ条件の必須化、`depends_on` の検証、`Task` の
//! 組み立て）は `task-ops::add`（ADR-0013 D7）に移した。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use task_core::{TaskId, TaskKind, TaskStore, Tier};
use task_ops::add::{CriterionSpec, NewTaskSpec, create_task};
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

/// `--accept`/`--check-cmd`/`--check-artifact`/`--check-reviewer` から `CriterionSpec` を
/// 固定順（accept → cmd → artifact → reviewer）で組み立てる。並び順・必須チェック自体は
/// `task_ops::add::create_task` が行う。
fn build_criteria(args: &mut AddArgs) -> Vec<CriterionSpec> {
    let mut acceptance = Vec::new();
    acceptance.extend(
        std::mem::take(&mut args.accept)
            .into_iter()
            .map(|text| CriterionSpec::Human { text }),
    );
    acceptance.extend(
        std::mem::take(&mut args.check_cmd)
            .into_iter()
            .map(|cmd| CriterionSpec::Command { cmd, expect_exit: 0 }),
    );
    acceptance.extend(
        std::mem::take(&mut args.check_artifact)
            .into_iter()
            .map(|name| CriterionSpec::ArtifactExists { name }),
    );
    acceptance.extend(
        std::mem::take(&mut args.check_reviewer)
            .into_iter()
            .map(|text| CriterionSpec::Reviewer { text }),
    );
    acceptance
}

pub fn run(store: &dyn TaskStore, mut args: AddArgs) -> Result<ExitCode, CliError> {
    let acceptance = build_criteria(&mut args);

    let parent = match &args.parent {
        Some(s) => Some(parse_task_id(s)?),
        None => None,
    };

    let depends_on: Vec<TaskId> = args
        .depends_on
        .iter()
        .map(|s| parse_task_id(s))
        .collect::<Result<_, _>>()?;

    let spec = NewTaskSpec {
        title: args.title,
        objective: args.objective,
        acceptance,
        kind: args.kind.into(),
        tier: args.tier.into(),
        priority: args.priority,
        parent,
        depends_on,
        max_turns: args.max_turns,
        max_wall_secs: args.max_wall_secs,
        max_retries: args.max_retries,
        workspace: args.workspace,
        adapter: None,
    };

    let task = create_task(store, spec, OffsetDateTime::now_utc())?;

    outln!("{}", task.id);
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Check, Event, SqliteStore, Status};

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
        assert_eq!(task.status, Status::Draft);

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
    fn run_with_invalid_parent_id_returns_error() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.parent = Some("not-a-valid-id".to_string());

        let result = run(&store, args);
        assert!(matches!(result, Err(CliError::Message(_))));
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
}
