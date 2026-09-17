//! `taskctl plan` の判断 — DESIGN.md §5.9 / ADR-0007 D6 / ADR-0010 D4（P-19, ADR-0013 D7）。
//!
//! 大目標を表す文字列 1 つから根の `Plan` タスクを組み立て、`TaskStore::create_task` で
//! `insert` + `Event::Created` を単一トランザクションとして書き込む（ADR-0010 D2）。
//! 子タスクの生成はプランナー（ワーカー）の出力を Reviewer が検証・展開する経路で行うため、
//! ここでは `acceptance = []`（暗黙のプラン検証条件のみ。ADR-0007 D4）で `Draft` の
//! 1 タスクを作るだけに留める。根の Plan のみを作る（`parent_id` は常に `None`）。
//! `workspace` を省略した場合は `WorkspaceSpec::Local{ path: "<task_id>" }`（相対パス、P-19）。

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{Budget, Status, Task, TaskId, TaskKind, TaskStore, Tier, WorkerHint, WorkspaceSpec};
use time::OffsetDateTime;

use crate::error::OpsError;

const TITLE_MAX_CHARS: usize = 80;

/// `taskctl plan` から組み立てる新規 Plan タスクの指定。API の `POST /plans` の本文でもある（`docs/gui/api.md` §3.14）。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPlanSpec {
    /// 大目標。1行目の先頭80文字が `title` になる。
    pub goal: String,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    #[serde(default = "default_plan_tier")]
    pub tier: Tier,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_plan_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_plan_max_wall_secs")]
    pub max_wall_secs: u64,
    #[serde(default = "default_plan_max_retries")]
    pub max_retries: u32,
}

fn default_plan_tier() -> Tier {
    Tier::Frontier
}
fn default_plan_max_turns() -> u32 {
    30
}
fn default_plan_max_wall_secs() -> u64 {
    900
}
fn default_plan_max_retries() -> u32 {
    1
}

/// 文字列の1行目を、char境界を保ったまま先頭 `max_chars` 文字に切り詰める。
fn truncate_title(goal: &str, max_chars: usize) -> String {
    let first_line = goal.lines().next().unwrap_or("");
    first_line.chars().take(max_chars).collect()
}

/// `spec` から `Plan` kind の `Task` を組み立て、`store.create_task` で原子的に挿入する。
pub fn create_plan(store: &dyn TaskStore, spec: NewPlanSpec, now: OffsetDateTime) -> Result<Task, OpsError> {
    if spec.goal.trim().is_empty() {
        return Err(OpsError::Validation("goal must not be blank".to_string()));
    }

    let title = truncate_title(&spec.goal, TITLE_MAX_CHARS);

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
        parent_id: None,
        kind: TaskKind::Plan,
        title,
        objective: spec.goal,
        acceptance: vec![],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Draft,
        priority: spec.priority,
        worker_hint: WorkerHint {
            tier: spec.tier,
            adapter: None,
        },
        workspace,
        budget,
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
    };

    store.create_task(&task, vec![])?;
    Ok(task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Event, SqliteStore};

    const DEFAULT_MAX_TURNS: u32 = 30;
    const DEFAULT_MAX_WALL_SECS: u64 = 900;
    const DEFAULT_MAX_RETRIES: u32 = 1;

    fn base_spec(goal: &str) -> NewPlanSpec {
        NewPlanSpec {
            goal: goal.to_string(),
            workspace: Some(PathBuf::from("/tmp/workspace")),
            tier: Tier::Frontier,
            priority: 0,
            max_turns: DEFAULT_MAX_TURNS,
            max_wall_secs: DEFAULT_MAX_WALL_SECS,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }

    #[test]
    fn create_plan_inserts_plan_task_and_created_event() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let goal = "add CLI argument parsing to hello-crate".to_string();
        let spec = base_spec(&goal);

        let task = create_plan(&store, spec, OffsetDateTime::now_utc()).expect("create_plan");

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
    fn create_plan_truncates_multiline_goal_title_but_keeps_full_objective() {
        let store = SqliteStore::open_in_memory().expect("open store");
        // 1行目にマルチバイト文字を含み、80文字を超える長さにする。
        let first_line: String = "目".repeat(90);
        let goal = format!("{first_line}\nsecond line\nthird line");
        let spec = base_spec(&goal);

        let task = create_plan(&store, spec, OffsetDateTime::now_utc()).expect("create_plan");

        let expected_title: String = first_line.chars().take(TITLE_MAX_CHARS).collect();
        assert_eq!(task.title, expected_title);
        assert_eq!(task.title.chars().count(), TITLE_MAX_CHARS);
        assert_eq!(task.objective, goal);
    }

    #[test]
    fn create_plan_with_blank_goal_returns_error() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let spec = base_spec("   \n  \t ");

        let result = create_plan(&store, spec, OffsetDateTime::now_utc());
        assert!(matches!(result, Err(OpsError::Validation(_))));
    }

    #[test]
    fn create_plan_with_custom_tier_priority_and_retries() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec("do something useful");
        spec.tier = Tier::Standard;
        spec.max_retries = 0;
        spec.priority = 5;

        let task = create_plan(&store, spec, OffsetDateTime::now_utc()).expect("create_plan");
        assert_eq!(task.worker_hint.tier, Tier::Standard);
        assert_eq!(task.budget.max_retries, 0);
        assert_eq!(task.priority, 5);
    }

    /// P-19: `workspace` 省略時は `<task_id>`（相対パス）になる。
    #[test]
    fn create_plan_without_workspace_defaults_to_relative_task_id_path() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec("do something useful");
        spec.workspace = None;

        let task = create_plan(&store, spec, OffsetDateTime::now_utc()).expect("create_plan");
        assert_eq!(
            task.workspace,
            WorkspaceSpec::Local {
                path: PathBuf::from(task.id.to_string())
            }
        );
    }
}
