//! `taskctl ls` / `taskctl show` / `taskctl log` — DESIGN.md §5.9。読み取り専用コマンド。
//!
//! `run_ls` は `store.list(status)` の結果をフラット、または `--tree` で `parent_id` に
//! よる親子インデント表示する。`run_show` はタスクの詳細と `events_for` の要約を表示する。
//! `run_log` は `events_for` を `seq` 順に表示し、`--follow` は 500ms 間隔でポーリングする
//! （Phase 2 の時点ではイベントを継続的に追記するディスパッチャがまだ無いため、`--follow` は
//! Phase 3 以降で実用になる）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, ValueEnum};
use task_core::{Check, Status, Task, TaskId, TaskStore};
use task_ops::view::{self, ViewContext};
use time::OffsetDateTime;

use crate::error::CliError;
use crate::outln;

#[derive(Args, Debug)]
pub struct LsArgs {
    #[arg(long, value_enum)]
    pub status: Option<StatusArg>,

    #[arg(long)]
    pub tree: bool,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    pub id: String,

    /// `TaskDetail` を JSON で出力する（`GET /api/v1/tasks/{id}` と同一。`docs/gui/api.md` §3.5）。
    #[arg(long)]
    pub json: bool,

    /// `--json` のときの `ViewContext.workspace_root`。省略時は `./workspaces`
    /// （`taskctl` には `taskd.toml` の設定が無いための既定値。`run_show_json` の doc を参照）。
    #[arg(long)]
    pub workspace_root: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct LogArgs {
    pub id: String,

    #[arg(long)]
    pub follow: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusArg {
    Draft,
    Ready,
    Running,
    Blocked,
    Reviewing,
    Done,
    Failed,
    Cancelled,
}

impl From<StatusArg> for Status {
    fn from(value: StatusArg) -> Self {
        match value {
            StatusArg::Draft => Status::Draft,
            StatusArg::Ready => Status::Ready,
            StatusArg::Running => Status::Running,
            StatusArg::Blocked => Status::Blocked,
            StatusArg::Reviewing => Status::Reviewing,
            StatusArg::Done => Status::Done,
            StatusArg::Failed => Status::Failed,
            StatusArg::Cancelled => Status::Cancelled,
        }
    }
}

fn print_task_line(task: &Task, indent: usize) {
    let prefix = "  ".repeat(indent);
    outln!(
        "{prefix}{} {:?} {:?} {}",
        task.id, task.status, task.kind, task.title
    );
}

fn print_tree(tasks: &[Task]) {
    let mut children: HashMap<TaskId, Vec<&Task>> = HashMap::new();
    let mut roots: Vec<&Task> = Vec::new();
    for task in tasks {
        match task.parent_id {
            Some(parent) => children.entry(parent).or_default().push(task),
            None => roots.push(task),
        }
    }
    for root in roots {
        print_task_line(root, 0);
        if let Some(kids) = children.get(&root.id) {
            for kid in kids {
                print_task_line(kid, 1);
            }
        }
    }
}

fn check_kind_name(check: &Check) -> &'static str {
    match check {
        Check::Command { .. } => "command",
        Check::ArtifactExists { .. } => "artifact_exists",
        Check::Reviewer => "reviewer",
        Check::Human => "human",
    }
}

fn print_events(store: &dyn TaskStore, id: TaskId) -> Result<(), CliError> {
    let events = store.events_for(id)?;
    for (seq, event) in events {
        outln!("[{seq}] {event:?}");
    }
    Ok(())
}

pub fn run_ls(store: &dyn TaskStore, args: LsArgs) -> Result<ExitCode, CliError> {
    let status = args.status.map(Status::from);
    let tasks = store.list(status)?;
    if args.tree {
        print_tree(&tasks);
    } else {
        for task in &tasks {
            print_task_line(task, 0);
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn run_show(store: &dyn TaskStore, args: ShowArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;

    if args.json {
        return run_show_json(store, id, args.workspace_root);
    }

    let task = store
        .get(id)?
        .ok_or_else(|| CliError::Message(format!("task not found: {}", args.id)))?;

    outln!("id: {}", task.id);
    outln!("status: {:?}", task.status);
    outln!("kind: {:?}", task.kind);
    outln!("title: {}", task.title);
    outln!("objective: {}", task.objective);
    outln!("priority: {}", task.priority);
    outln!("attempts: {}", task.attempts);
    outln!("budget: {:?}", task.budget);
    outln!("lease: {:?}", task.lease);
    match task.parent_id {
        Some(parent) => outln!("parent_id: {parent}"),
        None => outln!("parent_id: (none)"),
    }
    let depends_on: Vec<String> = task.depends_on.iter().map(TaskId::to_string).collect();
    outln!("depends_on: [{}]", depends_on.join(", "));
    outln!("acceptance:");
    for criterion in &task.acceptance {
        outln!(
            "  - {} ({})",
            criterion.text,
            check_kind_name(&criterion.check)
        );
    }

    outln!("events:");
    print_events(store, id)?;

    Ok(ExitCode::SUCCESS)
}

/// `taskctl show --json <id>` の中身。`task_ops::view::task_detail` の結果を JSON 文字列に
/// する（`GET /api/v1/tasks/{id}` と同一。`docs/gui/api.md` §3.5）。出力そのものをテストしやすい
/// よう `run_show_json` から分離してある。
///
/// `taskctl` には `taskd.toml` を読む仕組みが無いため、`ViewContext` は次の既定値で作る
/// （`taskd.toml` の対応する既定値と同じ。`taskd::config` の `default_workspace_root` /
/// `default_retry_backoff_base_secs` / `default_retry_backoff_max_secs` / `default_max_requeues`）:
/// - `workspace_root`: `./workspaces`（`--workspace-root` で上書き可）
/// - `retry_backoff`: base 10 秒 / max 300 秒
/// - `max_requeues`: 5
fn task_detail_json(store: &dyn TaskStore, id: TaskId, workspace_root: Option<PathBuf>) -> Result<String, CliError> {
    let ctx = ViewContext {
        workspace_root: workspace_root.unwrap_or_else(|| PathBuf::from("./workspaces")),
        retry_backoff_base: Duration::from_secs(10),
        retry_backoff_max: Duration::from_secs(300),
        max_requeues: 5,
    };
    let detail = view::task_detail(store, id, &ctx, OffsetDateTime::now_utc())?;
    // `GET /api/v1/tasks/{id}` と同じ compact な直列化（docs/gui/api.md §3.5）。整形は `jq` 等で行う。
    serde_json::to_string(&detail).map_err(|e| CliError::msg(format!("failed to encode task detail: {e}")))
}

fn run_show_json(store: &dyn TaskStore, id: TaskId, workspace_root: Option<PathBuf>) -> Result<ExitCode, CliError> {
    let json = task_detail_json(store, id, workspace_root)?;
    outln!("{json}");
    Ok(ExitCode::SUCCESS)
}

pub fn run_log(store: &dyn TaskStore, args: LogArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;

    if !args.follow {
        print_events(store, id)?;
        return Ok(ExitCode::SUCCESS);
    }

    // Phase 2 の時点ではディスパッチャ等、別プロセスがタスクのイベントを継続的に
    // 追記する仕組みはまだ存在しない。そのため --follow は「今後追記されるイベント
    // を待ち受ける」だけの単純なポーリングループとして実装しておき、Phase 3 以降で
    // ディスパッチャが実装された際にそのまま使えるようにする。
    let mut last_seq: Option<u64> = None;
    loop {
        let events = store.events_for(id)?;
        for (seq, event) in &events {
            if last_seq.is_none_or(|last| *seq > last) {
                outln!("[{seq}] {event:?}");
                last_seq = Some(*seq);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Budget, SqliteStore, TaskKind, WorkerHint, WorkspaceSpec};
    use time::OffsetDateTime;

    fn sample_task(status: Status, parent_id: Option<TaskId>) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id,
            kind: TaskKind::Execute,
            title: "title".to_string(),
            objective: "objective".to_string(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status,
            priority: 0,
            worker_hint: WorkerHint {
                tier: task_core::Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "/tmp".into(),
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
        }
    }

    #[test]
    fn run_ls_without_filter_lists_all_tasks() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let t1 = sample_task(Status::Draft, None);
        let t2 = sample_task(Status::Ready, None);
        store.insert(&t1).expect("insert t1");
        store.insert(&t2).expect("insert t2");

        let expected = store.list(None).expect("list").len();
        assert_eq!(expected, 2);

        let result = run_ls(
            &store,
            LsArgs {
                status: None,
                tree: false,
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn run_ls_with_status_filter_matches_store_list() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let t1 = sample_task(Status::Draft, None);
        let t2 = sample_task(Status::Ready, None);
        store.insert(&t1).expect("insert t1");
        store.insert(&t2).expect("insert t2");

        let expected = store.list(Some(Status::Ready)).expect("list").len();
        assert_eq!(expected, 1);

        let result = run_ls(
            &store,
            LsArgs {
                status: Some(StatusArg::Ready),
                tree: false,
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn run_ls_tree_groups_children_under_parent() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let parent = sample_task(Status::Draft, None);
        let child = sample_task(Status::Draft, Some(parent.id));
        store.insert(&parent).expect("insert parent");
        store.insert(&child).expect("insert child");

        let result = run_ls(
            &store,
            LsArgs {
                status: None,
                tree: true,
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn run_show_found_task_succeeds() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(Status::Draft, None);
        store.insert(&task).expect("insert task");

        let result = run_show(
            &store,
            ShowArgs {
                id: task.id.to_string(),
                json: false,
                workspace_root: None,
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn run_show_missing_task_errors() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let missing_id = TaskId::new().to_string();

        let result = run_show(
            &store,
            ShowArgs {
                id: missing_id,
                json: false,
                workspace_root: None,
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn run_show_json_prints_task_detail_with_actions_and_matches_task_id() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(Status::Draft, None);
        store.insert(&task).expect("insert task");

        let result = run_show(
            &store,
            ShowArgs {
                id: task.id.to_string(),
                json: true,
                workspace_root: None,
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn task_detail_json_is_parseable_and_contains_task_id_and_actions() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(Status::Draft, None);
        store.insert(&task).expect("insert task");

        let json = task_detail_json(&store, task.id, None).expect("task_detail_json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("output must be valid json");
        assert_eq!(value["task"]["id"], serde_json::Value::String(task.id.to_string()));
        let actions = value["actions"].as_array().expect("actions must be an array");
        assert!(!actions.is_empty(), "a draft task should have at least the `approve` action");
    }

    #[test]
    fn run_show_json_missing_task_errors() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let missing_id = TaskId::new().to_string();

        let result = run_show(
            &store,
            ShowArgs {
                id: missing_id,
                json: true,
                workspace_root: None,
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn run_log_without_follow_succeeds() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(Status::Draft, None);
        store.insert(&task).expect("insert task");

        let result = run_log(
            &store,
            LogArgs {
                id: task.id.to_string(),
                follow: false,
            },
        );
        assert!(result.is_ok());
    }
}
