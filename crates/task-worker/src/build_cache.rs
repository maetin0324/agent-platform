//! ADR-0066 D1: 同一リポジトリの worktree 間で cargo のビルドキャッシュを共有する。
//!
//! ローカル（クラスタではない）の git worktree タスクのホスト実行（コンテナではない）に、
//! `CARGO_TARGET_DIR=<build_cache_dir>/cargo/<repo-key>` を環境変数として与える。同じリポジトリを
//! 使う全タスクの worktree が同じ `CARGO_TARGET_DIR` を指すので、フルビルドのやり直しと大きな
//! `target/` の複製が worktree ごとに起きなくなる（cargo 自身のディレクトリロックで並走は直列化される）。
//!
//! ここは純粋関数だけ（LLM も I/O も無い。DESIGN 原則 1）。実際に環境変数として渡すかどうかの判断
//! （コンテナ・Remote は対象外）は `task-dispatch` 側で行う。

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// このキャッシュが使う環境変数名。
pub const CARGO_TARGET_DIR_VAR: &str = "CARGO_TARGET_DIR";

/// `cargo` のビルドキャッシュを置くサブディレクトリ（`<build_cache_dir>/cargo/`）。
const CARGO_SUBDIR: &str = "cargo";

/// リポジトリの実体（`project_repos.location.path` の絶対パス）から、決定的なキャッシュキーを作る。
/// `basename` + そのパス文字列の sha256 先頭 10 桁（同じ basename の別リポジトリを区別するため）。
/// `file_name` が取れない（`/` など）場合は `"repo"` を basename とする。
pub fn repo_cache_key(repo_source: &Path) -> String {
    let basename = repo_source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let slug = task_core::repos::slugify_repo_name(&basename);
    let mut hasher = Sha256::new();
    hasher.update(repo_source.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let short: String = digest.iter().take(5).map(|b| format!("{b:02x}")).collect();
    format!("{slug}-{short}")
}

/// このリポジトリの `CARGO_TARGET_DIR`（`<build_cache_dir>/cargo/<repo-key>`）。
pub fn cargo_target_dir(build_cache_dir: &Path, repo_source: &Path) -> PathBuf {
    build_cache_dir
        .join(CARGO_SUBDIR)
        .join(repo_cache_key(repo_source))
}

/// `run_worker` に渡す環境変数の 1 行（`(CARGO_TARGET_DIR, <path>)`）。
pub fn cargo_target_dir_env(build_cache_dir: &Path, repo_source: &Path) -> (String, String) {
    (
        CARGO_TARGET_DIR_VAR.to_string(),
        cargo_target_dir(build_cache_dir, repo_source)
            .display()
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_cache_key_is_deterministic_and_includes_the_basename() {
        let a = repo_cache_key(Path::new("/home/u/workspace/benchfs"));
        let b = repo_cache_key(Path::new("/home/u/workspace/benchfs"));
        assert_eq!(a, b);
        assert!(a.starts_with("benchfs-"), "{a}");
        // 桁数: basename + '-' + 10 桁の hex。
        assert_eq!(a.len(), "benchfs-".len() + 10);
    }

    #[test]
    fn repo_cache_key_differs_for_different_paths_with_the_same_basename() {
        let a = repo_cache_key(Path::new("/home/u/workspace/benchfs"));
        let b = repo_cache_key(Path::new("/home/u/other/benchfs"));
        assert_ne!(a, b);
    }

    #[test]
    fn repo_cache_key_falls_back_to_repo_for_a_path_with_no_file_name() {
        let key = repo_cache_key(Path::new("/"));
        assert!(key.starts_with("repo-"), "{key}");
    }

    #[test]
    fn cargo_target_dir_env_joins_the_build_cache_dir_cargo_and_the_key() {
        let (name, value) = cargo_target_dir_env(
            Path::new("/home/u/.local/celeris/build-cache"),
            Path::new("/home/u/workspace/benchfs"),
        );
        assert_eq!(name, "CARGO_TARGET_DIR");
        let key = repo_cache_key(Path::new("/home/u/workspace/benchfs"));
        assert_eq!(
            value,
            format!("/home/u/.local/celeris/build-cache/cargo/{key}")
        );
    }
}
