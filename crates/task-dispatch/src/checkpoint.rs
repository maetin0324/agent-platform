//! ADR-0072 D8（Phase E1）: daemon が決定的に集める「事実」（mechanical checkpoint）。
//!
//! ここは git の読み取りと `<artifacts_dir>/checkpoint.json` の読み込みだけを行う I/O 層で、
//! 合成そのもの（[`task_core::merge_checkpoint`]）は task-core の純粋関数に任せる（ADR-0001 D2）。

use std::path::Path;

use task_core::{CheckpointFileChange, CheckpointTestRun, MechanicalCheckpoint, RepoState};

/// D8: `tests_run` の上限（この run の tool_use から拾う件数）。
pub const MAX_MECHANICAL_TESTS: usize = 10;
/// D8: `recent_activity` の上限（直近の tool_use 行数）。
pub const MAX_RECENT_ACTIVITY: usize = 20;

/// worker が書いた `<artifacts_dir>/checkpoint.json` を読む（無い・読めない・schema 違反は `None`。
/// D8: 「schema 違反のときは mechanical だけで作る」）。
pub fn read_worker_checkpoint(artifacts_dir: &Path) -> Option<task_core::WorkerCheckpointInput> {
    let text = std::fs::read_to_string(artifacts_dir.join("checkpoint.json")).ok()?;
    task_core::parse_worker_checkpoint(&text)
}

/// D8: `repo_state` / `files_changed`（git の読み取り。既存の `task_ops::changes` を再利用する）。
/// `cwd` が無い・git リポジトリでない（U9: remote の worktree は対象外）ときは空。
pub fn gather_repo_facts(
    cwd: Option<&Path>,
    branch: &str,
) -> (Option<RepoState>, Vec<CheckpointFileChange>) {
    let Some(dir) = cwd else {
        return (None, Vec::new());
    };
    let default_branch = task_ops::changes::default_branch(dir, None);
    let changes = task_ops::changes::changes(dir, Some(dir), branch, &default_branch, None);
    if changes.missing {
        return (None, Vec::new());
    }
    let files_changed = changes
        .files
        .iter()
        .map(|f| CheckpointFileChange {
            path: f.path.clone(),
            change: map_git_status(&f.status),
            note: None,
        })
        .collect();
    let repo_state = Some(RepoState {
        branch: branch.to_string(),
        base: changes.base.clone(),
        head: changes.head.clone(),
        uncommitted: changes.dirty,
        diff_stat: format!(
            "{} files changed, {} insertions(+), {} deletions(-)",
            changes.stat.files, changes.stat.additions, changes.stat.deletions
        ),
    });
    (repo_state, files_changed)
}

fn map_git_status(status: &str) -> String {
    match status.chars().next() {
        Some('A') => "added".to_string(),
        Some('D') => "deleted".to_string(),
        // `task_ops::changes` は追跡外のファイルを `"?"` として返す。
        Some('?') => "added".to_string(),
        _ => "modified".to_string(),
    }
}

/// D8 の `tests_run` を判定する字句（決定的）。
fn looks_like_test_command(cmd: &str) -> bool {
    const NEEDLES: [&str; 9] = [
        "cargo test",
        "cargo clippy",
        "pnpm test",
        "npm test",
        "npm run test",
        "yarn test",
        "vitest",
        "pytest",
        "go test",
    ];
    NEEDLES.iter().any(|n| cmd.contains(n))
}

/// この run の `WorkerProgress{kind: tool_use}`（`tool`・`summary`）と、それに続く
/// `tool_result` の `error` フラグの組。呼び出し側（`dispatcher.rs`）が `events_for` から
/// 抽出して渡す（ここは純粋な整形だけ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolActivity {
    Use {
        tool: Option<String>,
        summary: Option<String>,
    },
    Result {
        error: bool,
    },
}

/// D8: `tests_run`（テストのコマンドに当たるもの、最大 [`MAX_MECHANICAL_TESTS`] 件）と
/// `recent_activity`（直近の tool_use、最大 [`MAX_RECENT_ACTIVITY`] 行）。
pub fn tests_and_activity(activity: &[ToolActivity]) -> (Vec<CheckpointTestRun>, Vec<String>) {
    let mut tests_run = Vec::new();
    let mut recent_activity = Vec::new();
    let mut pending_test: Option<String> = None;
    for item in activity {
        match item {
            ToolActivity::Use { tool, summary } => {
                let tool_name = tool.clone().unwrap_or_else(|| "tool".to_string());
                let line = match summary {
                    Some(s) if !s.is_empty() => format!("{tool_name}: {s}"),
                    _ => tool_name.clone(),
                };
                recent_activity.push(line);
                // このコマンドがテストでなければ `None`。次の `tool_result` は対応しない扱いになる。
                pending_test = summary
                    .as_deref()
                    .filter(|s| looks_like_test_command(s))
                    .map(str::to_string);
            }
            ToolActivity::Result { error } => {
                if let Some(command) = pending_test.take()
                    && !tests_run
                        .iter()
                        .any(|t: &CheckpointTestRun| t.command == command)
                {
                    tests_run.push(CheckpointTestRun {
                        command,
                        exit: Some(if *error { 1 } else { 0 }),
                        summary: None,
                    });
                }
            }
        }
    }
    tests_run.truncate(MAX_MECHANICAL_TESTS);
    if recent_activity.len() > MAX_RECENT_ACTIVITY {
        let start = recent_activity.len() - MAX_RECENT_ACTIVITY;
        recent_activity = recent_activity.split_off(start);
    }
    (tests_run, recent_activity)
}

/// [`gather_repo_facts`] と [`tests_and_activity`] をまとめた [`MechanicalCheckpoint`]。
pub fn gather(cwd: Option<&Path>, branch: &str, activity: &[ToolActivity]) -> MechanicalCheckpoint {
    let (repo_state, files_changed) = gather_repo_facts(cwd, branch);
    let (tests_run, recent_activity) = tests_and_activity(activity);
    MechanicalCheckpoint {
        repo_state,
        files_changed,
        tests_run,
        recent_activity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_cwd_yields_empty_repo_facts() {
        let (repo_state, files) = gather_repo_facts(None, "celeris/x");
        assert!(repo_state.is_none());
        assert!(files.is_empty());
    }

    #[test]
    fn tests_and_activity_pairs_test_commands_with_the_following_result() {
        let activity = vec![
            ToolActivity::Use {
                tool: Some("Bash".into()),
                summary: Some("cargo test -p task-core".into()),
            },
            ToolActivity::Result { error: false },
            ToolActivity::Use {
                tool: Some("Edit".into()),
                summary: Some("crates/task-core/src/execution.rs".into()),
            },
            ToolActivity::Result { error: false },
            ToolActivity::Use {
                tool: Some("Bash".into()),
                summary: Some("cargo clippy --workspace".into()),
            },
            ToolActivity::Result { error: true },
        ];
        let (tests_run, recent_activity) = tests_and_activity(&activity);
        assert_eq!(tests_run.len(), 2);
        assert_eq!(tests_run[0].command, "cargo test -p task-core");
        assert_eq!(tests_run[0].exit, Some(0));
        assert_eq!(tests_run[1].command, "cargo clippy --workspace");
        assert_eq!(tests_run[1].exit, Some(1));
        assert_eq!(recent_activity.len(), 3);
        assert_eq!(recent_activity[0], "Bash: cargo test -p task-core");
        assert_eq!(
            recent_activity[1],
            "Edit: crates/task-core/src/execution.rs"
        );
    }

    #[test]
    fn recent_activity_keeps_only_the_most_recent_items() {
        let activity: Vec<ToolActivity> = (0..30)
            .map(|i| ToolActivity::Use {
                tool: Some("Bash".into()),
                summary: Some(format!("step {i}")),
            })
            .collect();
        let (_, recent_activity) = tests_and_activity(&activity);
        assert_eq!(recent_activity.len(), MAX_RECENT_ACTIVITY);
        assert_eq!(recent_activity[0], "Bash: step 10");
        assert_eq!(recent_activity.last().unwrap(), "Bash: step 29");
    }

    #[test]
    fn read_worker_checkpoint_returns_none_when_the_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_worker_checkpoint(dir.path()).is_none());
    }

    #[test]
    fn read_worker_checkpoint_reads_a_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("checkpoint.json"),
            r#"{"completed": ["a"], "next_action": "b"}"#,
        )
        .unwrap();
        let cp = read_worker_checkpoint(dir.path()).unwrap();
        assert_eq!(cp.completed, vec!["a".to_string()]);
        assert_eq!(cp.next_action.as_deref(), Some("b"));
    }
}
