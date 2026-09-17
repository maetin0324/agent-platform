//! `taskctl add` — DESIGN.md §5.9 / ADR-0004 D4 / ADR-0010 D4（P-17, P-19）。
//!
//! CLI 引数の解析（フラグ → `CriterionSpec` の固定順: accept → cmd → artifact → reviewer）と
//! 出力整形だけをここで行う。判断と検証（受け入れ条件の必須化、`depends_on` の検証、`Task` の
//! 組み立て）は `task-ops::add`（ADR-0013 D7）に移した。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use task_core::{GenreSpec, RoleSpec, TaskId, TaskKind, TaskStore, Tier};
use task_ops::add::{CriterionSpec, NewTaskSpec, create_task_with_roles};
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

    /// 省略時は役割（--role）の既定 → standard（ADR-0016 D1）。
    #[arg(long, value_enum)]
    pub tier: Option<TierArg>,

    #[arg(long, default_value_t = 0)]
    pub priority: i32,

    /// 親タスクの ID（省略可）。
    #[arg(long)]
    pub parent: Option<String>,

    /// 依存する先行タスクの ID。複数回指定できる。存在しない、または failed/cancelled ならエラー。
    #[arg(long = "depends-on")]
    pub depends_on: Vec<String>,

    /// 省略時は役割（--role）の既定 → 10（ADR-0016 D1）。
    #[arg(long)]
    pub max_turns: Option<u32>,

    /// 省略時は役割（--role）の既定 → 600（ADR-0016 D1）。
    #[arg(long)]
    pub max_wall_secs: Option<u64>,

    #[arg(long, default_value_t = 2)]
    pub max_retries: u32,

    /// ADR-0016 D1: 役割名（自由記述）。`--config` があれば `[[roles]]` の既定（tier / adapter / 予算）を
    /// 埋め、run 時に指示文が前置きされる。`--config` が無いときは名前だけ保存する。
    #[arg(long)]
    pub role: Option<String>,

    /// ADR-0027 D1: 分野名（自由記述）。`--config` があれば `[[genres]]` の既定（`default_role` の役割の
    /// tier / adapter / 予算）を埋め、知らない分野や `--role` との不整合（`role` がその分野の `roles` に
    /// 無い）は起動時にエラーにする。`--config` が無いときは名前だけ保存する（検証しない）。
    #[arg(long)]
    pub genre: Option<String>,

    /// ADR-0016 D3: 委譲した子が全て終端になった後に集約 run を 1 回行い、`artifacts/summary.md` を書かせる。
    #[arg(long, default_value_t = false)]
    pub aggregate: bool,

    /// `[[roles]]` を読む `taskd.toml`。`--role` の既定をここから解決する。
    /// P-54: 省略時は環境変数 `TASKD_CONFIG` を見る。
    #[arg(long, env = "TASKD_CONFIG")]
    pub config: Option<PathBuf>,

    /// ワークスペースのローカルパス。省略時は `<task_id>`（相対パス、P-19）。
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// ADR-0018: クラスタ（`[[clusters]] id`）でコマンドを実行する。`--workspace` にはクラスタ側の
    /// 作業ディレクトリ（既存プロジェクトでよい）を絶対パスで指定する。
    #[arg(long)]
    pub cluster: Option<String>,
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

/// `--config` があれば `[[roles]]` と `[[genres]]` を読む。無ければ両方とも空（役割名／分野名だけ保存する。
/// `--role`/`--genre` が指定されているのに `--config` が無ければ警告する）。
fn load_roles_and_genres(
    config: Option<&PathBuf>,
    role: Option<&str>,
    genre: Option<&str>,
) -> Result<(Vec<RoleSpec>, Vec<GenreSpec>), CliError> {
    match config {
        Some(path) => {
            let config = taskd::Config::load(path)
                .map_err(|e| CliError::msg(format!("failed to load config {}: {e}", path.display())))?;
            Ok((config.role_specs(), config.genre_specs()))
        }
        None => {
            if role.is_some() {
                eprintln!(
                    "warning: --role given without --config; role defaults and instructions are not applied at creation"
                );
            }
            if genre.is_some() {
                eprintln!(
                    "warning: --genre given without --config; genre defaults and validation are not applied at creation"
                );
            }
            Ok((Vec::new(), Vec::new()))
        }
    }
}

pub fn run(store: &dyn TaskStore, mut args: AddArgs) -> Result<ExitCode, CliError> {
    let acceptance = build_criteria(&mut args);
    let (roles, genres) =
        load_roles_and_genres(args.config.as_ref(), args.role.as_deref(), args.genre.as_deref())?;

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
        tier: args.tier.map(Into::into),
        priority: args.priority,
        parent,
        depends_on,
        max_turns: args.max_turns,
        max_wall_secs: args.max_wall_secs,
        max_retries: args.max_retries,
        role: args.role,
        genre: args.genre,
        aggregate: args.aggregate,
        workspace: args.workspace,
        cluster: args.cluster,
        adapter: None,
    };

    let task = create_task_with_roles(store, spec, &roles, &genres, OffsetDateTime::now_utc())?;

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
            tier: Some(TierArg::Standard),
            priority: 0,
            parent: None,
            depends_on: vec![],
            max_turns: Some(10),
            max_wall_secs: Some(600),
            max_retries: 2,
            role: None,
            genre: None,
            aggregate: false,
            config: None,
            workspace: Some(PathBuf::from("/tmp/workspace")),
            cluster: None,
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

    /// ADR-0016 D1 / M3: `--role` + `--config` で、省略した tier / 予算が役割の既定になる。
    #[test]
    fn run_with_role_and_config_applies_role_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("taskd.toml");
        std::fs::write(
            &config_path,
            r#"[[roles]]
id = "lead"
tier = "frontier"
max_turns = 40
max_wall_secs = 1800
instructions = "You lead the work."

[[providers]]
id = "fake-local"
adapter = "fake"
"#,
        )
        .expect("write config");

        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.role = Some("lead".to_string());
        args.config = Some(config_path);
        args.tier = None;
        args.max_turns = None;
        args.max_wall_secs = None;

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        let task = &tasks[0];
        assert_eq!(task.role.as_deref(), Some("lead"));
        assert_eq!(task.worker_hint.tier, Tier::Frontier);
        assert_eq!((task.budget.max_turns, task.budget.max_wall_secs), (40, 1800));
        // --max-retries は役割の既定を持たない（既定 2 のまま）。
        assert_eq!(task.budget.max_retries, 2);
        assert!(!task.aggregate);
    }

    /// `--config` 無しの `--role` は役割名だけを保存する（既定は全体の既定。警告は stderr）。
    #[test]
    fn run_with_role_but_no_config_stores_the_name_only() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.role = Some("lead".to_string());
        args.aggregate = true;
        args.tier = None;
        args.max_turns = None;
        args.max_wall_secs = None;

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        let task = &tasks[0];
        assert_eq!(task.role.as_deref(), Some("lead"));
        assert_eq!(task.worker_hint.tier, Tier::Standard);
        assert_eq!((task.budget.max_turns, task.budget.max_wall_secs), (10, 600));
        assert!(task.aggregate);
    }

    /// ADR-0027 D1: `--genre` + `--config` で、その分野の `default_role` の役割の既定（tier / 予算）が
    /// role 未指定の子に効く。
    #[test]
    fn run_with_genre_and_config_applies_genre_default_role_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("taskd.toml");
        std::fs::write(
            &config_path,
            r#"[[roles]]
id = "literature-reader"
tier = "standard"
max_turns = 5
max_wall_secs = 1200

[[genres]]
id = "literature"
description = "related work survey"
default_role = "literature-reader"
roles = ["literature-reader"]

[[providers]]
id = "fake-local"
adapter = "fake"
"#,
        )
        .expect("write config");

        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.genre = Some("literature".to_string());
        args.config = Some(config_path);
        args.tier = None;
        args.max_turns = None;
        args.max_wall_secs = None;

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        let task = &tasks[0];
        assert_eq!(task.role, None, "genre alone must not set role");
        assert_eq!(task.genre.as_deref(), Some("literature"));
        assert_eq!(task.worker_hint.tier, Tier::Standard);
        assert_eq!((task.budget.max_turns, task.budget.max_wall_secs), (5, 1200));
    }

    /// `--config` 無しの `--genre` は分野名だけを保存する（検証しない。警告は stderr）。
    #[test]
    fn run_with_genre_but_no_config_stores_the_name_only() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.genre = Some("literature".to_string());

        run(&store, args).expect("run add");

        let tasks = store.list(None).expect("list tasks");
        let task = &tasks[0];
        assert_eq!(task.genre.as_deref(), Some("literature"));
    }

    /// ADR-0027 D1: `--config` があるとき、知らない `--genre` や `--genre` + `--role` の不整合はエラー（何も挿入しない）。
    #[test]
    fn run_with_unknown_genre_or_role_genre_mismatch_errors_when_config_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("taskd.toml");
        std::fs::write(
            &config_path,
            r#"[[roles]]
id = "implementer"

[[genres]]
id = "coding"
description = "d"
roles = ["implementer"]

[[providers]]
id = "fake-local"
adapter = "fake"
"#,
        )
        .expect("write config");

        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.genre = Some("literature".to_string());
        args.config = Some(config_path.clone());
        let result = run(&store, args);
        assert!(matches!(result, Err(CliError::Message(_))));
        assert!(store.list(None).expect("list tasks").is_empty());

        let mut args = base_args();
        args.genre = Some("coding".to_string());
        args.role = Some("literature-scout".to_string());
        args.config = Some(config_path);
        let result = run(&store, args);
        assert!(matches!(result, Err(CliError::Message(_))));
        assert!(store.list(None).expect("list tasks").is_empty());
    }

    /// 読めない `--config` はエラー（何も挿入しない）。
    #[test]
    fn run_with_unreadable_config_errors_and_inserts_nothing() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut args = base_args();
        args.config = Some(PathBuf::from("/nonexistent/taskd.toml"));

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
