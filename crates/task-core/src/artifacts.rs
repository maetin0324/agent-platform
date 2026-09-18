//! 成果物ディレクトリの決め方（ADR-0036）。
//!
//! 実機の事故（2026-09-18）: 計画 run が作った兄弟タスク 2 件が親の workspace を継ぎ、両方が
//! `<workspace>/artifacts/` に書いたため `sources.json` / `result.json` が上書きされた。
//! 成果物は**タスクごと**にする。ここは純粋関数だけ（判断も I/O も無い。DESIGN 原則 1〜4）。

use std::path::{Path, PathBuf};

use crate::model::Task;

/// 共有 workspace の成果物ディレクトリの接頭辞（ADR-0018 D1: `.taskd/` は taskd の管理用で、
/// クラスタ同期の両方向から除外されている）。
pub const SHARED_ARTIFACTS_PREFIX: &str = ".taskd/artifacts";

/// 単独タスクの成果物ディレクトリ名（ADR-0003 D5 以来の既定）。
pub const ARTIFACTS_DIR_NAME: &str = "artifacts";

/// そのタスクが `workspace_dir` を**自分で所有**しているか（ADR-0036 D1）。
///
/// 1. `parent_id` が無い → 所有（単独タスク。既存の挙動を変えない）
/// 2. ディレクトリの末尾の要素がそのタスクの id（既定の `workspace_root/<task_id>`、Remote の写し）→ 所有
/// 3. それ以外（親から継いだ path）→ 共有
pub fn owns_workspace(task: &Task, workspace_dir: &Path) -> bool {
    if task.parent_id.is_none() {
        return true;
    }
    workspace_dir
        .file_name()
        .map(|name| name.to_string_lossy() == task.id.to_string())
        .unwrap_or(false)
}

/// そのタスクの成果物ディレクトリ（`workspace_dir` 基準の絶対／相対はそのまま引き継ぐ。ADR-0036 D1）。
/// 所有していれば `<workspace_dir>/artifacts`、共有していれば `<workspace_dir>/.taskd/artifacts/<task_id>`。
pub fn artifacts_dir_for(task: &Task, workspace_dir: &Path) -> PathBuf {
    if owns_workspace(task, workspace_dir) {
        workspace_dir.join(ARTIFACTS_DIR_NAME)
    } else {
        workspace_dir.join(".taskd").join(ARTIFACTS_DIR_NAME).join(task.id.to_string())
    }
}

/// 成果物ディレクトリの workspace 相対表記（`artifacts` / `.taskd/artifacts/<task_id>`）。
/// プロンプトの文面（ADR-0036 D3）と `ArtifactRef.path` の組み立て（D4）に使う。区切りは常に `/`。
pub fn artifacts_rel_for(task: &Task, workspace_dir: &Path) -> String {
    if owns_workspace(task, workspace_dir) {
        ARTIFACTS_DIR_NAME.to_string()
    } else {
        format!("{SHARED_ARTIFACTS_PREFIX}/{}", task.id)
    }
}

/// `artifacts_dir` の workspace 相対表記。`artifacts_dir` が `workspace` 配下でなければ
/// 既定（`artifacts`）に倒す（ここで失敗させる価値は無い。防御的）。
pub fn rel_from(workspace: &Path, artifacts_dir: &Path) -> String {
    match artifacts_dir.strip_prefix(workspace) {
        Ok(rel) if rel.components().next().is_some() => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        _ => ARTIFACTS_DIR_NAME.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskId;

    fn task(parent: Option<TaskId>) -> Task {
        use crate::model::*;
        let now = time::OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: parent,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion { text: "c".into(), check: Check::Human }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: 0,
            worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
            workspace: WorkspaceSpec::Local { path: PathBuf::from("/tmp/ws") },
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
        }
    }

    /// 単独タスク（親なし）は、どんなディレクトリでも従来どおり `<workspace>/artifacts`。
    #[test]
    fn a_task_without_a_parent_owns_its_workspace() {
        let t = task(None);
        let dir = Path::new("/srv/workspaces/hello-crate");
        assert!(owns_workspace(&t, dir));
        assert_eq!(artifacts_dir_for(&t, dir), dir.join("artifacts"));
        assert_eq!(artifacts_rel_for(&t, dir), "artifacts");
    }

    /// 親から継いだ path の子は共有 → `.taskd/artifacts/<task_id>`（実機の事故の再発防止）。
    #[test]
    fn a_child_that_inherited_the_parents_path_gets_its_own_directory() {
        let parent = task(None);
        let child = task(Some(parent.id));
        let dir = Path::new("/srv/workspaces").join(parent.id.to_string());
        assert!(!owns_workspace(&child, &dir));
        assert_eq!(
            artifacts_dir_for(&child, &dir),
            dir.join(".taskd").join("artifacts").join(child.id.to_string())
        );
        assert_eq!(artifacts_rel_for(&child, &dir), format!(".taskd/artifacts/{}", child.id));
        // 兄弟同士でぶつからない。
        let sibling = task(Some(parent.id));
        assert_ne!(artifacts_dir_for(&child, &dir), artifacts_dir_for(&sibling, &dir));
    }

    /// 子でも、作業ディレクトリが自分の id（既定の `workspace_root/<task_id>`、Remote の写し）なら所有。
    #[test]
    fn a_child_with_its_own_directory_keeps_the_plain_artifacts_dir() {
        let parent = task(None);
        let child = task(Some(parent.id));
        let dir = Path::new("/srv/workspaces").join(child.id.to_string());
        assert!(owns_workspace(&child, &dir));
        assert_eq!(artifacts_dir_for(&child, &dir), dir.join("artifacts"));
    }

    #[test]
    fn rel_from_strips_the_workspace_and_falls_back_to_artifacts() {
        let ws = Path::new("/srv/ws");
        assert_eq!(rel_from(ws, &ws.join("artifacts")), "artifacts");
        assert_eq!(rel_from(ws, &ws.join(".taskd/artifacts/01H")), ".taskd/artifacts/01H");
        assert_eq!(rel_from(ws, Path::new("/elsewhere/artifacts")), "artifacts");
        assert_eq!(rel_from(ws, ws), "artifacts");
    }
}
