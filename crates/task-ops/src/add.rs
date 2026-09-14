//! `taskctl add` の判断と検証 — DESIGN.md §5.9 / ADR-0004 D4 / ADR-0010 D4（P-17, P-19, ADR-0013 D7）。
//!
//! `NewTaskSpec` から `Task` を組み立て、`TaskStore::create_task` で `insert` + `Event::Created`
//! を単一トランザクションとして書き込む（ADR-0010 D2）。初期 `status` は ADR-0002 D4 のとおり
//! `kind == Approval` なら `Ready`、それ以外は `Draft`。
//!
//! `acceptance` の並び順は呼び出し側（`taskctl` の CLI 引数写像）の責務。ここでは渡された順を
//! そのまま使う。条件のテキスト規則: `Command` は `` `<cmd>` exits 0 ``、`ArtifactExists` は
//! `artifact <name> exists`（現在の `taskctl add` と同じ）。
//!
//! `depends_on` に渡した各 ID は、存在しないか `failed`/`cancelled` ならエラーにし、
//! 何も挿入しない（挿入した瞬間に後続が永久に進まない状態を作らないため）。
//!
//! `workspace` を省略した場合は `WorkspaceSpec::Local{ path: "<task_id>" }`（相対パス）になる。
//! ディスパッチャが `workspace_root` 基準で解決する（ADR-0005 D3, ADR-0010 D4, P-19）。

use std::path::PathBuf;

use task_core::{
    Budget, Check, Criterion, Status, Task, TaskId, TaskKind, TaskStore, Tier, WorkerHint,
    WorkspaceSpec,
};
use time::OffsetDateTime;

use crate::error::OpsError;

/// 受け入れ条件 1 件の指定。現在の `taskctl add` の `--accept`/`--check-cmd`/
/// `--check-artifact`/`--check-reviewer` に対応する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CriterionSpec {
    /// `Check::Human`。
    Human { text: String },
    /// `Check::Command`。
    Command { cmd: String, expect_exit: i32 },
    /// `Check::ArtifactExists`。
    ArtifactExists { name: String },
    /// `Check::Reviewer`。
    Reviewer { text: String },
}

impl CriterionSpec {
    fn into_criterion(self) -> Criterion {
        match self {
            CriterionSpec::Human { text } => Criterion {
                text,
                check: Check::Human,
            },
            CriterionSpec::Command { cmd, expect_exit } => Criterion {
                text: format!("`{cmd}` exits 0"),
                check: Check::Command { cmd, expect_exit },
            },
            CriterionSpec::ArtifactExists { name } => Criterion {
                text: format!("artifact {name} exists"),
                check: Check::ArtifactExists { name },
            },
            CriterionSpec::Reviewer { text } => Criterion {
                text,
                check: Check::Reviewer,
            },
        }
    }
}

/// `taskctl add` から組み立てる新規タスクの指定。
#[derive(Debug, Clone)]
pub struct NewTaskSpec {
    pub title: String,
    pub objective: String,
    pub acceptance: Vec<CriterionSpec>,
    pub kind: TaskKind,
    pub tier: Tier,
    pub priority: i32,
    pub parent: Option<TaskId>,
    pub depends_on: Vec<TaskId>,
    pub max_turns: u32,
    pub max_wall_secs: u64,
    pub max_retries: u32,
    pub workspace: Option<PathBuf>,
    pub adapter: Option<String>,
}

fn build_acceptance(specs: Vec<CriterionSpec>) -> Result<Vec<Criterion>, OpsError> {
    if specs.is_empty() {
        return Err(OpsError::Validation(
            "at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)"
                .to_string(),
        ));
    }
    Ok(specs.into_iter().map(CriterionSpec::into_criterion).collect())
}

/// `depends_on` の各 ID が存在し、かつ `failed`/`cancelled` でないことを検証する
/// （ADR-0010 D4）。違反があれば挿入前にエラーを返す。
fn validate_depends_on(store: &dyn TaskStore, depends_on: &[TaskId]) -> Result<(), OpsError> {
    for dep_id in depends_on {
        match store.get(*dep_id)? {
            None => {
                return Err(OpsError::Validation(format!(
                    "dependency {dep_id} does not exist"
                )));
            }
            Some(dep) if matches!(dep.status, Status::Failed | Status::Cancelled) => {
                return Err(OpsError::Validation(format!(
                    "dependency {dep_id} has status {:?} and cannot be depended on",
                    dep.status
                )));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// `spec` から `Task` を組み立て、`store.create_task` で原子的に挿入する。
pub fn create_task(store: &dyn TaskStore, spec: NewTaskSpec, now: OffsetDateTime) -> Result<Task, OpsError> {
    let status = if spec.kind == TaskKind::Approval {
        Status::Ready
    } else {
        Status::Draft
    };

    let acceptance = build_acceptance(spec.acceptance)?;
    validate_depends_on(store, &spec.depends_on)?;

    let id = TaskId::new();
    let workspace = match spec.workspace {
        Some(path) => WorkspaceSpec::Local { path },
        None => WorkspaceSpec::Local {
            path: PathBuf::from(id.to_string()),
        },
    };

    let budget = Budget {
        max_turns: spec.max_turns,
        max_wall_secs: spec.max_wall_secs,
        max_retries: spec.max_retries,
    };

    let task = Task {
        id,
        parent_id: spec.parent,
        kind: spec.kind,
        title: spec.title,
        objective: spec.objective,
        acceptance,
        inputs: vec![],
        depends_on: spec.depends_on,
        status,
        priority: spec.priority,
        worker_hint: WorkerHint {
            tier: spec.tier,
            adapter: spec.adapter,
        },
        workspace,
        budget,
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
    };

    store.create_task(&task, vec![])?;
    Ok(task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Event, SqliteStore};

    fn base_spec() -> NewTaskSpec {
        NewTaskSpec {
            title: "do something".to_string(),
            objective: "make it work".to_string(),
            acceptance: vec![CriterionSpec::Human {
                text: "it works".to_string(),
            }],
            kind: TaskKind::Execute,
            tier: Tier::Standard,
            priority: 0,
            parent: None,
            depends_on: vec![],
            max_turns: 10,
            max_wall_secs: 600,
            max_retries: 2,
            workspace: Some(PathBuf::from("/tmp/workspace")),
            adapter: None,
        }
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    #[test]
    fn create_task_inserts_task_and_created_event() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let spec = base_spec();

        let task = create_task(&store, spec, now()).expect("create_task");

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
        assert_eq!(fetched, task);

        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(events.len(), 1);
        match &events[0].1 {
            Event::Created { task: created } => assert_eq!(created.id, task.id),
            other => panic!("expected Created event, got {other:?}"),
        }
    }

    #[test]
    fn create_task_with_approval_kind_starts_ready() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.kind = TaskKind::Approval;

        let task = create_task(&store, spec, now()).expect("create_task");
        assert_eq!(task.status, Status::Ready);
    }

    #[test]
    fn create_task_preserves_acceptance_order_as_given() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.acceptance = vec![
            CriterionSpec::Human { text: "human check".to_string() },
            CriterionSpec::Command { cmd: "cargo test".to_string(), expect_exit: 0 },
            CriterionSpec::ArtifactExists { name: "bench.json".to_string() },
            CriterionSpec::Reviewer { text: "looks good".to_string() },
        ];

        let task = create_task(&store, spec, now()).expect("create_task");
        assert_eq!(task.acceptance.len(), 4);
        assert_eq!(task.acceptance[0].check, Check::Human);
        assert!(matches!(task.acceptance[1].check, Check::Command { .. }));
        assert!(matches!(task.acceptance[2].check, Check::ArtifactExists { .. }));
        assert_eq!(task.acceptance[3].check, Check::Reviewer);
    }

    #[test]
    fn create_task_check_cmd_produces_command_criterion_with_expected_text() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.acceptance = vec![CriterionSpec::Command {
            cmd: "cargo test".to_string(),
            expect_exit: 0,
        }];

        let task = create_task(&store, spec, now()).expect("create_task");
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
    fn create_task_check_artifact_produces_artifact_exists_criterion_with_expected_text() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.acceptance = vec![CriterionSpec::ArtifactExists {
            name: "bench.json".to_string(),
        }];

        let task = create_task(&store, spec, now()).expect("create_task");
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
    fn create_task_check_reviewer_produces_reviewer_criterion_with_text_verbatim() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.acceptance = vec![CriterionSpec::Reviewer {
            text: "the diff is minimal and well-tested".to_string(),
        }];

        let task = create_task(&store, spec, now()).expect("create_task");
        assert_eq!(task.acceptance.len(), 1);
        assert_eq!(
            task.acceptance[0].text,
            "the diff is minimal and well-tested"
        );
        assert_eq!(task.acceptance[0].check, Check::Reviewer);
    }

    #[test]
    fn create_task_without_workspace_defaults_to_relative_task_id_path() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.workspace = None;

        let task = create_task(&store, spec, now()).expect("create_task");
        assert_eq!(
            task.workspace,
            WorkspaceSpec::Local {
                path: PathBuf::from(task.id.to_string())
            }
        );
    }

    #[test]
    fn create_task_without_any_acceptance_criterion_errors_and_inserts_nothing() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.acceptance = vec![];

        let result = create_task(&store, spec, now());
        assert!(matches!(result, Err(OpsError::Validation(_))));
        assert!(store.list(None).expect("list tasks").is_empty());
    }

    #[test]
    fn create_task_with_missing_dependency_errors_and_inserts_nothing() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.depends_on = vec![TaskId::new()];

        let result = create_task(&store, spec, now());
        assert!(matches!(result, Err(OpsError::Validation(_))));
        assert!(store.list(None).expect("list tasks").is_empty());
    }

    #[test]
    fn create_task_with_failed_dependency_errors_and_inserts_nothing() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut dep_spec = base_spec();
        dep_spec.workspace = Some(PathBuf::from("/tmp/dep"));
        let dep = create_task(&store, dep_spec, now()).expect("create dep");

        // Drive the dependency to `failed` via a valid path:
        // draft -> accept -> ready -> acquire_lease -> running -> worker_error(false) -> failed.
        store
            .apply_transition(dep.id, task_core::Trigger::Accept, None)
            .expect("accept dep");
        let acquired = store
            .acquire_lease(dep.id, "run-dep", std::time::Duration::from_secs(60))
            .expect("acquire lease");
        assert!(acquired);
        store
            .apply_transition(
                dep.id,
                task_core::Trigger::WorkerError { retryable: false },
                None,
            )
            .expect("fail dep");
        assert_eq!(
            store.get(dep.id).expect("get").expect("some").status,
            Status::Failed
        );

        let mut spec = base_spec();
        spec.depends_on = vec![dep.id];

        let result = create_task(&store, spec, now());
        assert!(result.is_err());
        // Only the dependency task should exist; the new task must not be inserted.
        assert_eq!(store.list(None).expect("list tasks").len(), 1);
    }
}
