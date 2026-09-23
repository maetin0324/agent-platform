//! ADR-0066 D2: 終端タスクの作業場所から、ビルド生成物だけを自動で刈る。
//!
//! ADR-0043 D2 は「中止（cancel）のときだけ worktree ごと消す」と決めている。ここではそれを変えず
//! （done / failed のまま残った worktree は差分を見るために必要）、終端になってから
//! `[workspace] prune_after_secs` 経った作業場所から、`target/` 等のビルド生成物だけを削る。
//! ソースツリー（`.git` を含む）と `artifacts/`、`.celeris/` は残す。
//!
//! シンボリックリンク（ADR-0043 D2 の `dir` リポジトリ）は対象外にする: リンク先は人の実体なので、
//! 生成物であっても celeris が消してよいものではない。

use std::path::{Path, PathBuf};

use task_core::{Status, StoreError, TaskId, TaskStore};
use time::OffsetDateTime;

/// 刈る対象の候補（各 worktree のディレクトリからの相対パス）。ADR-0066 D2。
pub const PRUNABLE_SUBPATHS: &[&str] = &[
    "target",
    "node_modules",
    "build",
    ".venv",
    "gui/node_modules",
    "gui/build",
];

/// 1 タスクの作業場所で見つかった、刈れる生成物。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneCandidate {
    pub task_id: TaskId,
    pub task_dir: PathBuf,
    /// 存在が確認できた、消してよい絶対パス（1 件以上）。
    pub paths: Vec<PathBuf>,
}

/// シンボリックリンクではない実在のディレクトリか。
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.is_dir())
        .unwrap_or(false)
}

/// `task_dir` の下にある worktree（git worktree。シンボリックリンクは含まない）を 1 段だけ列挙する。
/// ADR-0043 D2 の複数リポジトリ（`repos/<name>/`）と、Phase 49 の 1 リポジトリだけの旧い形
/// （`tree/`）の両方を見る。
fn worktree_dirs(task_dir: &Path) -> Vec<PathBuf> {
    let repos_dir = task_dir.join(crate::task_repos::REPOS_DIR_NAME);
    if repos_dir.is_dir() {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&repos_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if is_real_dir(&path) {
                    out.push(path);
                }
            }
        }
        out.sort();
        return out;
    }
    let legacy = task_dir.join(crate::local_worktree::WORKTREE_DIR_NAME);
    if is_real_dir(&legacy) {
        vec![legacy]
    } else {
        Vec::new()
    }
}

/// この作業場所に残っている、刈れる生成物の絶対パスを集める（存在するものだけ。無ければ空）。
pub fn prunable_paths(task_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for repo_dir in worktree_dirs(task_dir) {
        for rel in PRUNABLE_SUBPATHS {
            let candidate = repo_dir.join(rel);
            if is_real_dir(&candidate) {
                out.push(candidate);
            }
        }
    }
    out
}

/// 終端（done / failed / cancelled）になってから `after_secs` 以上経ち、まだ刈れる生成物が残っている
/// 作業場所を、決定的な順（task id の文字列順）ですべて集める。`after_secs == 0` は「無効」（常に空）。
pub fn find_prune_candidates(
    store: &dyn TaskStore,
    workspace_root: &Path,
    now: OffsetDateTime,
    after_secs: u64,
) -> Result<Vec<PruneCandidate>, StoreError> {
    if after_secs == 0 {
        return Ok(Vec::new());
    }
    let mut terminal = Vec::new();
    for status in [Status::Done, Status::Failed, Status::Cancelled] {
        terminal.extend(store.list(Some(status))?);
    }
    terminal.sort_by_key(|a| a.id.to_string());
    let mut out = Vec::new();
    for task in terminal {
        let age = now - task.updated_at;
        if age.whole_seconds() < after_secs as i64 {
            continue;
        }
        let task_dir = workspace_root.join(task.id.to_string());
        let paths = prunable_paths(&task_dir);
        if !paths.is_empty() {
            out.push(PruneCandidate {
                task_id: task.id,
                task_dir,
                paths,
            });
        }
    }
    Ok(out)
}

/// `find_prune_candidates` の最初の 1 件（dispatcher の tick が「1 tick に最大 1 か所」で使う）。
pub fn find_prune_candidate(
    store: &dyn TaskStore,
    workspace_root: &Path,
    now: OffsetDateTime,
    after_secs: u64,
) -> Result<Option<PruneCandidate>, StoreError> {
    Ok(find_prune_candidates(store, workspace_root, now, after_secs)?
        .into_iter()
        .next())
}

/// 実際に消す。消せなかったパスは無視して残りを続ける（途中で 1 つ失敗しても他は消す）。
/// 戻り値は実際に消せたパス（呼び出し側が `workspace_pruned` イベントの `removed` に使う）。
pub fn prune(candidate: &PruneCandidate) -> Vec<PathBuf> {
    candidate
        .paths
        .iter()
        .filter(|path| std::fs::remove_dir_all(path).is_ok())
        .cloned()
        .collect()
}

/// `workspace_pruned` イベントの `removed` に載せる、作業場所からの相対パス（表示用）。
pub fn relative_removed(task_dir: &Path, removed: &[PathBuf]) -> Vec<String> {
    removed
        .iter()
        .map(|p| {
            p.strip_prefix(task_dir)
                .map(|rel| rel.display().to_string())
                .unwrap_or_else(|_| p.display().to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{
        Budget, Check, Criterion, Event, SqliteStore, Task, TaskKind, Trigger, WorkerHint,
        WorkspaceSpec,
    };

    fn store_with_task(status: Status, updated_at: OffsetDateTime) -> (SqliteStore, TaskId) {
        let store = SqliteStore::open_in_memory().expect("open store");
        let id = insert_task(&store, status, updated_at);
        (store, id)
    }

    /// `store` に 1 件タスクを足し、`Trigger` を積み重ねて `status` まで進めてから、`updated_at` だけを
    /// 望みの時刻に書き戻す（`update_task` は `status`/`attempts`/`lease` を DB の現在値から取るので、
    /// 状態機械を通さずに `status` を直接書くことはできない。ADR-0002 の保護）。
    fn insert_task(store: &SqliteStore, status: Status, updated_at: OffsetDateTime) -> TaskId {
        let id = TaskId::new();
        let now = OffsetDateTime::now_utc();
        let task = Task {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id,
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion {
                text: "c".into(),
                check: Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: 0,
            worker_hint: WorkerHint {
                tier: task_core::Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: PathBuf::from("/tmp/ws"),
                mode: None,
            },
            budget: Budget {
                max_turns: 10,
                max_wall_secs: 60,
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
            labels: vec![],
            category: Default::default(),
            conversation: None,
        };
        store.insert(&task).expect("insert");
        match status {
            Status::Cancelled => {
                store
                    .apply_transition_with_events(id, Trigger::Cancel, vec![])
                    .expect("cancel");
            }
            Status::Done => {
                store
                    .apply_transition_with_events(id, Trigger::Dispatch, vec![])
                    .expect("dispatch");
                store
                    .apply_transition_with_events(id, Trigger::WorkerDone, vec![])
                    .expect("worker_done");
                store
                    .apply_transition_with_events(id, Trigger::ReviewPass, vec![])
                    .expect("review_pass");
            }
            Status::Failed => {
                store
                    .apply_transition_with_events(id, Trigger::Dispatch, vec![])
                    .expect("dispatch");
                store
                    .apply_transition_with_events(
                        id,
                        Trigger::WorkerError { retryable: false },
                        vec![],
                    )
                    .expect("worker_error");
            }
            Status::Ready => {}
            other => panic!("unsupported status for this test helper: {other:?}"),
        }
        let mut current = store.get(id).expect("get").expect("task exists");
        current.updated_at = updated_at;
        store
            .update_task(&current, Event::worker_progress("test", "backdated"))
            .expect("backdate updated_at");
        id
    }

    #[test]
    fn prunable_paths_finds_target_under_repos_and_ignores_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let task_dir = dir.path();
        let repo_dir = task_dir.join("repos").join("benchfs");
        std::fs::create_dir_all(repo_dir.join("target")).unwrap();
        std::fs::create_dir_all(repo_dir.join("gui").join("node_modules")).unwrap();
        std::fs::create_dir_all(repo_dir.join("src")).unwrap();
        // シンボリックリンクのリポジトリ（`dir` 種別）は対象外。
        let real = dir.path().join("real-elsewhere");
        std::fs::create_dir_all(real.join("target")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, task_dir.join("repos").join("data")).unwrap();

        let mut found = prunable_paths(task_dir);
        found.sort();
        let mut expected = vec![
            repo_dir.join("target"),
            repo_dir.join("gui").join("node_modules"),
        ];
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn prunable_paths_falls_back_to_the_legacy_tree_dir() {
        let dir = tempfile::tempdir().unwrap();
        let task_dir = dir.path();
        std::fs::create_dir_all(task_dir.join("tree").join("target")).unwrap();
        let found = prunable_paths(task_dir);
        assert_eq!(found, vec![task_dir.join("tree").join("target")]);
    }

    #[test]
    fn find_prune_candidates_only_returns_terminal_tasks_old_enough_with_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path();
        let now = OffsetDateTime::now_utc();

        let (store, done_id) = store_with_task(Status::Done, now - time::Duration::days(2));
        std::fs::create_dir_all(
            workspace_root
                .join(done_id.to_string())
                .join("repos")
                .join("r")
                .join("target"),
        )
        .unwrap();

        // 終端だが新しすぎる（`after_secs` 未満）ので候補に入らない。
        let fresh_id = insert_task(&store, Status::Failed, now);
        std::fs::create_dir_all(
            workspace_root
                .join(fresh_id.to_string())
                .join("repos")
                .join("r")
                .join("target"),
        )
        .unwrap();

        // 終端でも生成物が残っていないので候補に入らない。
        let clean_id = insert_task(&store, Status::Cancelled, now - time::Duration::days(2));

        // 終端ではない（`ready`）ので候補に入らない。
        let ready_id = TaskId::new();

        let candidates = find_prune_candidates(&store, workspace_root, now, 86400).unwrap();
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert_eq!(candidates[0].task_id, done_id);
        assert_eq!(candidates[0].paths.len(), 1);
        let _ = (fresh_id, clean_id, ready_id);
    }

    #[test]
    fn after_secs_zero_disables_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path();
        let now = OffsetDateTime::now_utc();
        let (store, done_id) = store_with_task(Status::Done, now - time::Duration::days(10));
        std::fs::create_dir_all(
            workspace_root
                .join(done_id.to_string())
                .join("repos")
                .join("r")
                .join("target"),
        )
        .unwrap();
        let candidates = find_prune_candidates(&store, workspace_root, now, 0).unwrap();
        assert!(candidates.is_empty());
    }

    #[test]
    fn prune_removes_the_candidate_paths_and_keeps_the_source_tree() {
        let dir = tempfile::tempdir().unwrap();
        let task_dir = dir.path().join("01TASK");
        let repo_dir = task_dir.join("repos").join("benchfs");
        std::fs::create_dir_all(repo_dir.join("target").join("debug")).unwrap();
        std::fs::create_dir_all(repo_dir.join("src")).unwrap();
        std::fs::write(repo_dir.join("src").join("main.rs"), "fn main() {}").unwrap();
        let candidate = PruneCandidate {
            task_id: TaskId::new(),
            task_dir: task_dir.clone(),
            paths: vec![repo_dir.join("target")],
        };
        let removed = prune(&candidate);
        assert_eq!(removed, vec![repo_dir.join("target")]);
        assert!(!repo_dir.join("target").exists());
        assert!(repo_dir.join("src").join("main.rs").exists());
        let rel = relative_removed(&task_dir, &removed);
        assert_eq!(rel, vec!["repos/benchfs/target".to_string()]);
    }
}
