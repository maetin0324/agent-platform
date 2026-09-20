//! タスクの作業ツリーの閲覧（ADR-0043 D6。Phase 52）:
//! `GET /tasks/{id}/tree?repo=&path=`（一覧）と `GET /tasks/{id}/tree/file?repo=&path=`（本文）。
//!
//! **読み取り専用**で、他のタスクの読み取り（`GET /tasks/{id}/runs` 等）と同じくトークンは要らない。
//!
//! 境界（ADR-0003 D5 と同じ規則。ここがこの API の安全の全部）:
//!
//! - `repo` は目印（`<task_dir>/worktree.json`）に書かれた名前のどれか。それ以外は 404
//! - `path` は**そのリポジトリの作業ツリーからの相対パス**。`..`・絶対パス・Windows の prefix は 403
//! - 解決した実体（`canonicalize`）がそのリポジトリの根の外に出たら 403
//!   （`dir` のリポジトリはシンボリックリンクなので、**根そのものも canonicalize してから**比べる）
//! - 本文はテキストで 512 KiB まで。バイナリ（NUL を含む・UTF-8 でない）はサイズだけ返す

use std::path::{Component, Path, PathBuf};

use axum::http::StatusCode;
use axum::routing::get;
use task_core::{Task, TaskStore};
use task_ops::workspace::{WorktreeMarker, WorktreeMarkerRepo};

use crate::handlers::{ApiResult, Params, json_response};
use crate::problem::{ApiProblem, store_problem};
use crate::query::QueryParams;
use crate::query::parse_task_id;
use crate::state::ApiState;
use crate::types::{TreeEntry, TreeFileView, TreeRepoView, TreeView};

/// ADR-0043 D6: 本文を返す上限（テキストのみ）。これを超えたら `too_large`。
pub const MAX_TEXT_BYTES: u64 = 512 * 1024;

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new()
        .route("/api/v1/tasks/{id}/tree", get(tree))
        .route("/api/v1/tasks/{id}/tree/file", get(tree_file))
}

/// そのタスクの目印（`worktree.json`）。作業ツリーを持たないタスクは 404。
pub(crate) fn marker_of(task: &Task, workspace_root: &Path) -> Result<WorktreeMarker, ApiProblem> {
    let task_dir = workspace_root.join(task.id.to_string());
    task_ops::workspace::read_marker(&task_dir)
        .filter(|m| !m.repos.is_empty() || !m.dir.is_empty())
        .ok_or_else(|| ApiProblem::file_not_found("this task has no working tree"))
}

/// 目印の `repos`（Phase 49 の目印には無いので、そのときは先頭の 1 件を合成する）。
pub(crate) fn marker_repos(marker: &WorktreeMarker) -> Vec<WorktreeMarkerRepo> {
    if !marker.repos.is_empty() {
        return marker.repos.clone();
    }
    vec![WorktreeMarkerRepo {
        name: "tree".to_string(),
        kind: if marker.branch.is_empty() {
            "dir".into()
        } else {
            "git".into()
        },
        source: marker.repo.clone(),
        dir: marker.dir.clone(),
        branch: Some(marker.branch.clone()).filter(|b| !b.is_empty()),
        base: Some(marker.base.clone()).filter(|b| !b.is_empty()),
        base_kind: Some(marker.base_kind.clone()).filter(|b| !b.is_empty()),
    }]
}

fn repo_views(repos: &[WorktreeMarkerRepo]) -> Vec<TreeRepoView> {
    repos
        .iter()
        .map(|r| TreeRepoView {
            name: r.name.clone(),
            kind: r.kind.clone(),
            dir: r.dir.clone(),
            branch: r.branch.clone(),
            base: r.base.clone(),
        })
        .collect()
}

/// `?repo=` の解決。省略したら先頭（= ワーカーのカレントディレクトリになったリポジトリ）。
fn pick_repo(
    repos: &[WorktreeMarkerRepo],
    name: Option<&str>,
) -> Result<WorktreeMarkerRepo, ApiProblem> {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        None => repos
            .first()
            .cloned()
            .ok_or_else(|| ApiProblem::file_not_found("this task has no working tree")),
        Some(name) => repos
            .iter()
            .find(|r| r.name == name)
            .cloned()
            .ok_or_else(|| {
                ApiProblem::file_not_found(format!("repo not found in this task: {name}"))
            }),
    }
}

/// `?path=` の検査（ADR-0003 D5 と同じ規則）。`..`・絶対パス・prefix は 403。
fn relative_path(raw: Option<&str>) -> Result<PathBuf, ApiProblem> {
    let raw = raw.unwrap_or("").trim().trim_start_matches("./");
    if raw.is_empty() || raw == "." {
        return Ok(PathBuf::new());
    }
    let path = Path::new(raw);
    let escapes = path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        });
    if escapes {
        return Err(ApiProblem::path_forbidden(
            "path must be relative and must not contain `..`",
        ));
    }
    Ok(path.to_path_buf())
}

/// リポジトリの根（`canonicalize` 済み）。`dir` のリポジトリはシンボリックリンクなので、
/// 根そのものも解決してから比べる（そうしないと全てのファイルが「外」になる）。
fn repo_root(repo: &WorktreeMarkerRepo) -> Result<PathBuf, ApiProblem> {
    Path::new(&repo.dir).canonicalize().map_err(|_| {
        ApiProblem::file_not_found(format!("the working tree of {} does not exist", repo.name))
    })
}

/// 根 + 相対パスを解決し、根の**外**に出ていないことを確かめる（シンボリックリンクの脱出も 403）。
fn resolve_within(root: &Path, rel: &Path) -> Result<PathBuf, ApiProblem> {
    let candidate = root.join(rel);
    let canonical = candidate
        .canonicalize()
        .map_err(|_| ApiProblem::file_not_found("path does not exist"))?;
    if !canonical.starts_with(root) {
        return Err(ApiProblem::path_forbidden(
            "path resolves outside the working tree",
        ));
    }
    Ok(canonical)
}

fn rel_string(rel: &Path, name: &str) -> String {
    let joined = rel.join(name);
    joined.to_string_lossy().replace('\\', "/")
}

async fn tree(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    let task_id = parse_task_id(&id)?;
    let query = QueryParams::parse(raw.as_deref(), &["repo", "path"])?;
    let repo_name = query.single("repo")?.map(str::to_string);
    let rel = relative_path(query.single("path")?)?;
    let workspace_root = state.inner.view.workspace_root.clone();
    let view = state
        .blocking(move |store| {
            let Some(task) = store.get(task_id).map_err(store_problem)? else {
                return Err(ApiProblem::task_not_found(task_id));
            };
            let marker = marker_of(&task, &workspace_root)?;
            let repos = marker_repos(&marker);
            let repo = pick_repo(&repos, repo_name.as_deref())?;
            let root = repo_root(&repo)?;
            let dir = resolve_within(&root, &rel)?;
            if !dir.is_dir() {
                return Err(ApiProblem::path_forbidden("path is not a directory"));
            }
            let mut entries = Vec::new();
            let read = std::fs::read_dir(&dir)
                .map_err(|_| ApiProblem::file_not_found("directory could not be read"))?;
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                // `metadata()` はリンクを辿る（辿れなければ `other`）。
                let (kind, size) = match entry.metadata() {
                    Ok(m) if m.is_dir() => ("dir", None),
                    Ok(m) if m.is_file() => ("file", Some(m.len())),
                    _ => ("other", None),
                };
                entries.push(TreeEntry {
                    path: rel_string(&rel, &name),
                    name,
                    kind: kind.to_string(),
                    size,
                });
            }
            // ディレクトリが先、あとは名前順（決定的）。
            entries.sort_by(|a, b| {
                let rank = |k: &str| if k == "dir" { 0 } else { 1 };
                rank(&a.kind)
                    .cmp(&rank(&b.kind))
                    .then_with(|| a.name.cmp(&b.name))
            });
            Ok(TreeView {
                repo: repo.name.clone(),
                path: rel.to_string_lossy().replace('\\', "/"),
                repos: repo_views(&repos),
                entries,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

async fn tree_file(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    let task_id = parse_task_id(&id)?;
    let query = QueryParams::parse(raw.as_deref(), &["repo", "path"])?;
    let repo_name = query.single("repo")?.map(str::to_string);
    let rel = relative_path(query.single("path")?)?;
    if rel.as_os_str().is_empty() {
        return Err(ApiProblem::bad_request("path is required"));
    }
    let workspace_root = state.inner.view.workspace_root.clone();
    let view = state
        .blocking(move |store| {
            let Some(task) = store.get(task_id).map_err(store_problem)? else {
                return Err(ApiProblem::task_not_found(task_id));
            };
            let marker = marker_of(&task, &workspace_root)?;
            let repos = marker_repos(&marker);
            let repo = pick_repo(&repos, repo_name.as_deref())?;
            let root = repo_root(&repo)?;
            let file = resolve_within(&root, &rel)?;
            let meta = std::fs::metadata(&file)
                .map_err(|_| ApiProblem::file_not_found("file does not exist"))?;
            if !meta.is_file() {
                return Err(ApiProblem::path_forbidden("path is not a regular file"));
            }
            let size = meta.len();
            let path = rel.to_string_lossy().replace('\\', "/");
            if size > MAX_TEXT_BYTES {
                return Ok(TreeFileView {
                    repo: repo.name.clone(),
                    path,
                    size,
                    binary: false,
                    too_large: true,
                    text: None,
                });
            }
            let bytes = std::fs::read(&file)
                .map_err(|_| ApiProblem::file_not_found("file could not be read"))?;
            let binary = bytes.contains(&0) || std::str::from_utf8(&bytes).is_err();
            Ok(TreeFileView {
                repo: repo.name.clone(),
                path,
                size,
                binary,
                too_large: false,
                text: if binary {
                    None
                } else {
                    String::from_utf8(bytes).ok()
                },
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_reject_escapes() {
        assert_eq!(relative_path(None).expect("none"), PathBuf::new());
        assert_eq!(relative_path(Some("")).expect("empty"), PathBuf::new());
        assert_eq!(
            relative_path(Some("src/main.rs")).expect("ok"),
            PathBuf::from("src/main.rs")
        );
        assert_eq!(
            relative_path(Some("./src")).expect("ok"),
            PathBuf::from("src")
        );
        for bad in ["..", "../x", "src/../../etc", "/etc/passwd"] {
            assert_eq!(
                relative_path(Some(bad)).err().map(|p| p.code()),
                Some("path_forbidden"),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_symlink_that_escapes_the_repo_root_is_forbidden() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
        let outside = dir.path().join("secret.txt");
        std::fs::write(&outside, b"nope").unwrap_or_else(|e| panic!("{e}"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("escape.txt"))
            .unwrap_or_else(|e| panic!("{e}"));
        let canonical_root = root.canonicalize().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            resolve_within(&canonical_root, Path::new("escape.txt"))
                .err()
                .map(|p| p.code()),
            Some("path_forbidden")
        );
        std::fs::write(root.join("inside.txt"), b"ok").unwrap_or_else(|e| panic!("{e}"));
        assert!(resolve_within(&canonical_root, Path::new("inside.txt")).is_ok());
    }
}
