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
}
