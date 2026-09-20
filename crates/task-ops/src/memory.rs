//! 記憶を読む（ADR-0033 D6 の読み取り側。GUI 監査対応 Phase 29 / H3）。
//!
//! 書き込みは run の前後にワーカー側（`task_worker::memory::MemoryDir`）が行う（ADR-0033 D6）。
//! ここは同じファイル配置の規約を読むだけで、**書き込み API は無い**（記憶はワーカーが書く。人が
//! 直したければファイルを直接編集する。`GET /org/{id}/memory` がパスを返すのはそのため）。
//!
//! `task_worker::memory::MemoryDir::load` は前置き用に上限（既定 8,000 字）で切るが、ここは**全文**を返す
//! （人が読む画面なので、切る理由が無い）。I/O はファイルの読み取りだけで、LLM は呼ばない（DESIGN 原則 1）。

use std::path::{Path, PathBuf};

/// `<dir>/<node_id>/notes.md`（`task_worker::memory::MemoryDir::notes_path` と同じ規約）。
pub fn notes_path(dir: &Path, node_id: &str) -> PathBuf {
    dir.join(node_id).join("notes.md")
}

/// `<dir>/<node_id>/projects/<project_id>.md`。
pub fn project_path(dir: &Path, node_id: &str, project_id: &str) -> PathBuf {
    dir.join(node_id).join("projects").join(format!("{project_id}.md"))
}

/// ファイルの全文を読む（無ければ空文字列。読めなくても呼び出し側は止めない）。
pub fn read_full(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// `GET /org/{id}/memory` の中身（`task-api` が JSON に写す）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryView {
    pub notes: String,
    pub project: Option<String>,
    pub notes_path: PathBuf,
    pub project_path: Option<PathBuf>,
}

/// `dir`（`[memory] dir`）とノード・案件から `MemoryView` を組む。`project_id` が無ければ
/// `project` / `project_path` は `None`。
pub fn read_memory(dir: &Path, node_id: &str, project_id: Option<&str>) -> MemoryView {
    let notes_path = notes_path(dir, node_id);
    let notes = read_full(&notes_path);
    match project_id {
        Some(project_id) => {
            let path = project_path(dir, node_id, project_id);
            let project = read_full(&path);
            MemoryView {
                notes,
                project: Some(project),
                notes_path,
                project_path: Some(path),
            }
        }
        None => MemoryView {
            notes,
            project: None,
            notes_path,
            project_path: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_match_the_documented_layout() {
        let dir = PathBuf::from("/var/lib/celeris/memory");
        assert_eq!(notes_path(&dir, "secretary"), dir.join("secretary/notes.md"));
        assert_eq!(
            project_path(&dir, "secretary", "P1"),
            dir.join("secretary/projects/P1.md")
        );
    }

    #[test]
    fn missing_files_read_as_empty_and_present_files_read_in_full() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().to_path_buf();

        // 何も書かれていない: 空文字列（エラーにしない）。
        let view = read_memory(&dir, "secretary", Some("P1"));
        assert_eq!(view.notes, "");
        assert_eq!(view.project.as_deref(), Some(""));
        assert_eq!(view.notes_path, dir.join("secretary/notes.md"));
        assert_eq!(view.project_path, Some(dir.join("secretary/projects/P1.md")));

        // 上限を超える長さでも全文を返す（前置き用の 8,000 字カットとは別）。
        std::fs::create_dir_all(dir.join("secretary/projects")).expect("mkdir");
        let long = "あ".repeat(10_000);
        std::fs::write(notes_path(&dir, "secretary"), &long).expect("write");
        std::fs::write(project_path(&dir, "secretary", "P1"), "project notes").expect("write");
        let view = read_memory(&dir, "secretary", Some("P1"));
        assert_eq!(view.notes.chars().count(), 10_000, "not truncated");
        assert_eq!(view.project.as_deref(), Some("project notes"));

        // `project_id` が無ければ `project` / `project_path` は `None`。
        let view = read_memory(&dir, "secretary", None);
        assert_eq!(view.project, None);
        assert_eq!(view.project_path, None);

        // 知らないノードは空文字列（404 の判定は呼び出し側＝組織の存在確認）。
        let view = read_memory(&dir, "ghost", None);
        assert_eq!(view.notes, "");
    }
}
