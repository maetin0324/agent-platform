//! 成果物のパス検査と sha256（ADR-0003 D5, DESIGN §4.4）。

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use task_core::ArtifactRef;

#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("artifact path must be workspace-relative: {0}")]
    NotRelative(String),
    #[error("artifact path escapes workspace: {0}")]
    Escapes(String),
    #[error("artifact file not found: {0}")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// ワークスペース相対パスを検査し、実在すれば sha256 を計算して `ArtifactRef` を作る。
/// 絶対パス、`..` を含むパス、シンボリックリンク経由でワークスペース外に出るパスは拒否する。
/// `kind` 省略時は拡張子から推定する（無ければ `"file"`）。
pub fn resolve(
    workspace: &Path,
    name: &str,
    rel_path: &str,
    kind: Option<&str>,
) -> Result<ArtifactRef, ArtifactError> {
    let rel = Path::new(rel_path);
    if rel.is_absolute() || rel_path.is_empty() {
        return Err(ArtifactError::NotRelative(rel_path.to_string()));
    }
    if rel
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir))
    {
        return Err(ArtifactError::Escapes(rel_path.to_string()));
    }
    let full = workspace.join(rel);
    if !full.exists() {
        return Err(ArtifactError::NotFound(rel_path.to_string()));
    }
    let ws_canon = workspace.canonicalize()?;
    let full_canon = full.canonicalize()?;
    if !full_canon.starts_with(&ws_canon) {
        return Err(ArtifactError::Escapes(rel_path.to_string()));
    }
    let sha256 = sha256_file(&full_canon)?;
    let kind = kind
        .map(str::to_string)
        .or_else(|| rel.extension().map(|e| e.to_string_lossy().to_string()))
        .unwrap_or_else(|| "file".to_string());
    Ok(ArtifactRef {
        name: name.to_string(),
        path: rel_path.to_string(),
        sha256,
        kind,
    })
}

/// ファイルの sha256（16進小文字）。
pub fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let bytes = std::fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(format!("{:x}", h.finalize()))
}

/// `workspace` 内の成果物の絶対パス（検査済みの `ArtifactRef` 用）。
pub fn absolute_path(workspace: &Path, artifact: &ArtifactRef) -> PathBuf {
    workspace.join(&artifact.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_accepts_relative_file_and_computes_sha256() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(dir.path().join("artifacts/a.json"), b"{}").unwrap();
        let a = resolve(dir.path(), "a", "artifacts/a.json", None).unwrap();
        assert_eq!(a.kind, "json");
        assert_eq!(a.sha256, "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a");
        assert_eq!(absolute_path(dir.path(), &a), dir.path().join("artifacts/a.json"));
    }

    #[test]
    fn resolve_rejects_absolute_parent_missing_and_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"x").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();
        assert!(matches!(resolve(dir.path(), "a", "/etc/passwd", None), Err(ArtifactError::NotRelative(_))));
        assert!(matches!(resolve(dir.path(), "a", "../x", None), Err(ArtifactError::Escapes(_))));
        assert!(matches!(resolve(dir.path(), "a", "nope.txt", None), Err(ArtifactError::NotFound(_))));
        assert!(matches!(resolve(dir.path(), "a", "link", None), Err(ArtifactError::Escapes(_))));
    }
}
