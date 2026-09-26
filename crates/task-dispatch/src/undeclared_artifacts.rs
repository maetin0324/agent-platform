//! ADR-0067 D3: 未申告の成果物を拾う。
//!
//! `claude-code` / `codex` / `acp` アダプタは `result.json` の `summary`/`question`/`evidence` しか読まず、
//! `Event::ArtifactProduced` を出さない（`sink.artifact()` を呼ぶのは `paperqa` / `local-deep-research` /
//! ストリーミングプロトコルのハーネスだけ）。そのため人が読む成果物を `artifacts/` の外に書いても、
//! `GET /tasks/{id}/artifacts` には現れない（本番事故、BenchFS 案件のタスク 01M35X86XTK84F97QW0CN5PGMR）。
//!
//! ここでは git worktree ではない `local` の作業場所（ADR-0036 の「所有」タスク・共有 workspace どちらも
//! 含む）に限り、run 完了後に `artifacts_dir` の外にある `*.md` を走査し、まだ登録されていないものを
//! 「未申告の成果物」（`declared: false`）として返す。呼び出し側（`run_worker`）が
//! `Event::ArtifactProduced` として記録する。LLM 呼び出しは無い（DESIGN 原則 1）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use task_core::ArtifactRef;

/// 件数の上限（ADR-0067 D3）。
pub const MAX_FILES: usize = 20;
/// 1 ファイルの上限（ADR-0067 D3）。
pub const MAX_BYTES: u64 = 1024 * 1024;
/// 配下を見ないディレクトリ名（ADR-0067 D3）。`.taskd` は celeris の管理用（ADR-0018 D1）で、
/// 共有 workspace では兄弟タスクの `artifacts/`（`.taskd/artifacts/<task_id>/`）もここに入る。
/// 自分の `artifacts_dir` だけでなく `.taskd` 全体を除外しないと、兄弟の成果物まで「未申告」として
/// 拾ってしまう（ADR-0036 の「兄弟の成果物を混ぜない」契約に反する）。
const EXCLUDED_DIRS: &[&str] = &["node_modules", ".venv", "target", ".git", ".taskd"];

/// `workspace_dir` の下にある `*.md` のうち、`artifacts_dir` の外にあり `existing_paths`
/// （workspace 相対パス。既に `Event::ArtifactProduced` で記録済みのもの）に無いものを、
/// `declared: false` の `ArtifactRef` として返す（`path` の昇順。上限 [`MAX_FILES`] 件・
/// 1 ファイル [`MAX_BYTES`] 以下・空ファイルは対象外）。
pub fn scan_undeclared_markdown_artifacts(
    workspace_dir: &Path,
    artifacts_dir: &Path,
    existing_paths: &HashSet<String>,
) -> Vec<ArtifactRef> {
    let artifacts_rel = artifacts_dir
        .strip_prefix(workspace_dir)
        .ok()
        .map(Path::to_path_buf);
    let mut found = Vec::new();
    // 相対パス（`workspace_dir` からの）を積むスタック。`PathBuf::new()` はルート自身。
    let mut stack: Vec<PathBuf> = vec![PathBuf::new()];
    while let Some(rel_dir) = stack.pop() {
        if found.len() >= MAX_FILES {
            break;
        }
        if let Some(ar) = &artifacts_rel
            && rel_dir == *ar
        {
            continue; // artifacts_dir 自身の配下は見ない（既に「成果物置き場」）。
        }
        let abs_dir = workspace_dir.join(&rel_dir);
        let Ok(entries) = std::fs::read_dir(&abs_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if found.len() >= MAX_FILES {
                break;
            }
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let rel_path = rel_dir.join(&name);
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if EXCLUDED_DIRS.contains(&name_str.as_ref()) {
                    continue;
                }
                stack.push(rel_path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if rel_path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() == 0 || metadata.len() > MAX_BYTES {
                continue;
            }
            let path_str = rel_path.to_string_lossy().replace('\\', "/");
            if existing_paths.contains(&path_str) {
                continue;
            }
            let Ok(sha256) = task_worker::artifact::sha256_file(&workspace_dir.join(&rel_path))
            else {
                continue;
            };
            found.push(ArtifactRef {
                name: name_str.into_owned(),
                path: path_str,
                sha256,
                kind: "md".to_string(),
                declared: false,
            });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.truncate(MAX_FILES);
    found
}

/// ADR-0074 D6.3（Phase F1 (j)）: 人が読む成果物の拡張子（git worktree の Task の走査対象）。
pub const HUMAN_ARTIFACT_EXTENSIONS: &[&str] = &["md", "html", "pdf", "csv", "png"];

/// ADR-0074 D6.3: 機械的なファイル（celeris 自身が書く、成果物ではないもの）。走査対象から外す
/// （`execution-plan*.json`/`project-plan*.json` は前方一致、`phase-reports/` はディレクトリごと除く）。
const EXCLUDED_ARTIFACT_FILENAMES: &[&str] = &[
    "checkpoint.json",
    "result.json",
    "request.json",
    "prompt.txt",
];

fn is_excluded_artifact_filename(name: &str) -> bool {
    EXCLUDED_ARTIFACT_FILENAMES.contains(&name)
        || (name.starts_with("execution-plan") && name.ends_with(".json"))
        || (name.starts_with("project-plan") && name.ends_with(".json"))
}

/// ADR-0074 D6.3（Phase F1 (j)）: git worktree の Task に未申告成果物の走査を広げる。ADR-0067 D3 の
/// 走査（[`scan_undeclared_markdown_artifacts`]）はリポジトリ全体を見るので worktree では使えない
/// （無関係なファイルまで拾ってしまう）。ここでは run の**成果物ディレクトリの中だけ**を見て、
/// `HUMAN_ARTIFACT_EXTENSIONS` のうち未登録のものを返す（`path` は `workspace_dir` からの相対）。
pub fn scan_undeclared_artifacts_in_dir(
    workspace_dir: &Path,
    artifacts_dir: &Path,
    existing_paths: &HashSet<String>,
) -> Vec<ArtifactRef> {
    let mut found = Vec::new();
    let mut stack: Vec<PathBuf> = vec![PathBuf::new()];
    while let Some(rel_dir) = stack.pop() {
        if found.len() >= MAX_FILES {
            break;
        }
        // D2.3（将来の phase-reports/。今はまだ発生しないが、あらかじめ除く）。
        if rel_dir.file_name().and_then(|n| n.to_str()) == Some("phase-reports") {
            continue;
        }
        let abs_dir = artifacts_dir.join(&rel_dir);
        let Ok(entries) = std::fs::read_dir(&abs_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if found.len() >= MAX_FILES {
                break;
            }
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let rel_path = rel_dir.join(&name);
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if EXCLUDED_DIRS.contains(&name_str.as_ref()) || name_str == "phase-reports" {
                    continue;
                }
                stack.push(rel_path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if is_excluded_artifact_filename(&name_str) {
                continue;
            }
            let ext_ok = rel_path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| HUMAN_ARTIFACT_EXTENSIONS.contains(&e));
            if !ext_ok {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() == 0 || metadata.len() > MAX_BYTES {
                continue;
            }
            let abs_path = artifacts_dir.join(&rel_path);
            let path_str = abs_path
                .strip_prefix(workspace_dir)
                .unwrap_or(&abs_path)
                .to_string_lossy()
                .replace('\\', "/");
            if existing_paths.contains(&path_str) {
                continue;
            }
            let Ok(sha256) = task_worker::artifact::sha256_file(&abs_path) else {
                continue;
            };
            let kind = rel_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string();
            found.push(ArtifactRef {
                name: name_str.into_owned(),
                path: path_str,
                sha256,
                kind,
                declared: false,
            });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.truncate(MAX_FILES);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn finds_markdown_outside_artifacts_dir_and_skips_declared_and_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        write(ws, "docs/paper/phase1/framing-candidates.md", "# framing\n");
        write(ws, "artifacts/result.json", "{}");
        write(ws, "artifacts/notes.md", "already inside artifacts/, skip");
        write(ws, "node_modules/pkg/readme.md", "skip: excluded dir");
        write(ws, "README.md", "declared already, skip");
        write(ws, "empty.md", "");

        let mut existing = HashSet::new();
        existing.insert("README.md".to_string());

        let found = scan_undeclared_markdown_artifacts(ws, &ws.join("artifacts"), &existing);
        let paths: Vec<&str> = found.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(paths, vec!["docs/paper/phase1/framing-candidates.md"]);
        assert!(!found[0].declared);
        assert_eq!(found[0].name, "framing-candidates.md");
        assert_eq!(found[0].kind, "md");
    }

    #[test]
    fn caps_at_max_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        for i in 0..(MAX_FILES + 5) {
            write(ws, &format!("doc-{i:02}.md"), "x");
        }
        let found = scan_undeclared_markdown_artifacts(ws, &ws.join("artifacts"), &HashSet::new());
        assert_eq!(found.len(), MAX_FILES);
    }

    #[test]
    fn skips_files_over_the_byte_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        write(ws, "big.md", &"x".repeat((MAX_BYTES + 1) as usize));
        write(ws, "small.md", "ok");
        let found = scan_undeclared_markdown_artifacts(ws, &ws.join("artifacts"), &HashSet::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "small.md");
    }

    // ---- ADR-0074 D6.3（Phase F1 (j)）: git worktree の Task の走査 ----

    /// git worktree の Task で `artifacts/report.md` が未申告成果物として拾われる。リポジトリ全体
    /// （worktree のコード）は走査しない（成果物ディレクトリの外は見ない）。
    #[test]
    fn git_worktree_task_registers_report_md_as_an_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        write(ws, "artifacts/report.md", "# report\n");
        write(ws, "artifacts/result.json", "{}"); // 機械的なファイル、拾わない
        write(ws, "artifacts/checkpoint.json", "{}");
        write(ws, "src/lib.rs", "fn main() {}"); // worktree のコード、拾わない（成果物置き場の外）
        write(ws, "README.md", "repo readme, not an artifact"); // 同上

        let found = scan_undeclared_artifacts_in_dir(ws, &ws.join("artifacts"), &HashSet::new());
        let paths: Vec<&str> = found.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(paths, vec!["artifacts/report.md"], "{found:?}");
        assert!(!found[0].declared);
        assert_eq!(found[0].kind, "md");
    }

    /// md 以外の人が読む拡張子（html/pdf/csv/png）も拾う。
    #[test]
    fn git_worktree_task_registers_other_human_readable_extensions() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        write(ws, "artifacts/dashboard.html", "<html></html>");
        write(ws, "artifacts/data.csv", "a,b\n1,2\n");
        write(ws, "artifacts/notes.txt", "not a tracked extension");

        let found = scan_undeclared_artifacts_in_dir(ws, &ws.join("artifacts"), &HashSet::new());
        let paths: Vec<&str> = found.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["artifacts/dashboard.html", "artifacts/data.csv"],
            "{found:?}"
        );
    }

    /// 既に `Event::ArtifactProduced` で登録済みのパスは拾わない。
    #[test]
    fn git_worktree_task_skips_already_declared_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        write(ws, "artifacts/report.md", "# report\n");
        let mut existing = HashSet::new();
        existing.insert("artifacts/report.md".to_string());
        let found = scan_undeclared_artifacts_in_dir(ws, &ws.join("artifacts"), &existing);
        assert!(found.is_empty(), "{found:?}");
    }
}
