//! ローカルの作業場所の置き場（ADR-0041 D1）。
//!
//! `WorkspaceSpec::Local` の `mode = worktree`（既定）で、`path` が git リポジトリのときだけ、
//! taskd はタスクごとに `git worktree` を切る。そのとき **run の足回り**（`runs/`, `inputs/`,
//! `artifacts/`）は worktree の外、`<workspace_root>/<task_id>/` に置き、作業ツリーそのものは
//! `<workspace_root>/<task_id>/tree` になる。作業ツリーの中に `runs/` を作ると
//! `git status --porcelain` が常に汚れ、終端で worktree を消せなくなるため。
//!
//! ここは「どこを見ればよいか」を 1 か所に決める純粋な関数（+ 目印ファイルの有無だけを見る）。
//! ディスパッチャは dispatch の時点で `git rev-parse` まで見て決め、その結果を目印
//! （`<task_dir>/worktree.json`）として残す。API・`taskctl` はその目印だけを見る
//! （git を起こさない。worktree を消した後も `runs/` と `artifacts/` が引けるように、目印は消さない）。

use std::path::{Path, PathBuf};

use task_core::{Task, WorkspaceMode, WorkspaceSpec};

/// worktree を切ったタスクの目印（`<workspace_root>/<task_id>/worktree.json`）。
pub const WORKTREE_MARKER: &str = "worktree.json";

/// 作業ツリーのディレクトリ名（`<workspace_root>/<task_id>/tree`）。
pub const WORKTREE_DIR_NAME: &str = "tree";

/// そのタスクの**足回りのディレクトリ**（`runs/`, `inputs/`, `artifacts/` があるところ）。
///
/// - `Local` で worktree を切った（目印がある）→ `<workspace_root>/<task_id>`
/// - `Local`（従来）→ `path`（相対なら `workspace_root` 基準）
/// - `Remote` → 手元の写し `<workspace_root>/<task_id>`（ADR-0018 D1。従来どおり）
pub fn local_dir(task: &Task, workspace_root: &Path) -> PathBuf {
    match &task.workspace {
        WorkspaceSpec::Local { path, .. } => {
            let per_task = workspace_root.join(task.id.to_string());
            if task.workspace.local_mode() == WorkspaceMode::Worktree && per_task.join(WORKTREE_MARKER).is_file() {
                per_task
            } else {
                workspace_root.join(path)
            }
        }
        WorkspaceSpec::Remote { .. } => workspace_root.join(task.id.to_string()),
    }
}

/// 目印に書く内容（ADR-0041 D1。人が読む・API が読む）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorktreeMarker {
    /// 元のリポジトリ（`WorkspaceSpec::Local.path`）。
    pub repo: String,
    /// 作業ツリー（`<workspace_root>/<task_id>/tree`）。
    pub dir: String,
    /// ブランチ（`<branch_prefix><task_id>`）。
    pub branch: String,
    /// 切り出した base の sha（全長）。
    pub base: String,
    /// base をどこから取ったか（`main` / `current` / `head`）。
    pub base_kind: String,
}

/// 目印を書く（worktree を用意したディスパッチャが 1 回だけ。上書きしてよい）。
pub fn write_marker(task_dir: &Path, marker: &WorktreeMarker) -> std::io::Result<()> {
    std::fs::create_dir_all(task_dir)?;
    let text = serde_json::to_string_pretty(marker).map_err(std::io::Error::other)?;
    std::fs::write(task_dir.join(WORKTREE_MARKER), format!("{text}\n"))
}

/// 目印を読む（無い・壊れていれば `None`）。
pub fn read_marker(task_dir: &Path) -> Option<WorktreeMarker> {
    let text = std::fs::read_to_string(task_dir.join(WORKTREE_MARKER)).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(path: &str, mode: Option<WorkspaceMode>) -> Task {
        use task_core::*;
        let now = time::OffsetDateTime::now_utc();
        Task {
            id: task_core::TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion { text: "c".into(), check: Check::Human }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: 0,
            worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
            workspace: WorkspaceSpec::Local { path: PathBuf::from(path), mode },
            budget: Budget { max_turns: 1, max_wall_secs: 1, max_retries: 0 },
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
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    /// 目印が無ければ従来どおり `path`（`mode` の既定が `worktree` でも変わらない）。
    #[test]
    fn without_the_marker_the_local_dir_is_the_path() {
        let root = tempfile::tempdir().expect("tempdir");
        let t = task("/srv/repo", None);
        assert_eq!(local_dir(&t, root.path()), PathBuf::from("/srv/repo"));
    }

    /// 目印があれば `<workspace_root>/<task_id>`（worktree を消した後も同じ）。
    #[test]
    fn with_the_marker_the_local_dir_is_the_per_task_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        let t = task("/srv/repo", None);
        let per_task = root.path().join(t.id.to_string());
        std::fs::create_dir_all(&per_task).expect("mkdir");
        std::fs::write(per_task.join(WORKTREE_MARKER), "{}").expect("write");
        assert_eq!(local_dir(&t, root.path()), per_task);
    }

    /// `mode = shared` は目印があっても従来どおり（worktree を切らないので目印も付かないが、念のため）。
    #[test]
    fn shared_mode_ignores_the_marker() {
        let root = tempfile::tempdir().expect("tempdir");
        let t = task("/srv/repo", Some(WorkspaceMode::Shared));
        let per_task = root.path().join(t.id.to_string());
        std::fs::create_dir_all(&per_task).expect("mkdir");
        std::fs::write(per_task.join(WORKTREE_MARKER), "{}").expect("write");
        assert_eq!(local_dir(&t, root.path()), PathBuf::from("/srv/repo"));
    }
}
