//! 文書（ADR-0044 D7。Phase 57）。**正本は git**（案件の primary リポジトリの `[outputs].docs`）:
//!
//! - `GET    /projects/{id}/docs?q=` — ツリー（`q` は `git grep -il`）。読み取り
//! - `GET    /projects/{id}/docs/page?path=` — 1 ページ（raw / html / front matter / 履歴 / etag）。読み取り
//! - `POST   /projects/{id}/docs/init` — **管理系**。文書リポジトリを用意する（無い案件だけ）
//! - `PUT    /projects/{id}/docs/page` — **管理系**。default_branch に直接コミット（`etag` 必須）
//! - `DELETE /projects/{id}/docs/page?path=&etag=` — **管理系**
//! - `POST   /tasks/{id}/artifacts/promote` — **管理系**。成果物をページに昇格する
//!
//! 境界（ADR-0044 D7 / ADR-0043 D5 と同じ規則）:
//!
//! - パスは**リポジトリ相対**で、文書の根の外に出られない（`..`・絶対パスは 403）。`.md` 以外は 422
//! - 人の編集は `etag`（blob sha）で衝突を見る。違えば 409 `etag_mismatch`
//! - 人のチェックアウトが default_branch を出していて dirty なら 409 `default_branch_busy`
//! - 読み取りは**何も作らない**。文書リポジトリを作るのは管理系（`docs/init`、`PUT`、昇格）だけ
//!
//! ADR-0013 は「API はコマンドを実行しない」と決めているが、ここは `task_ops::changes` / `task_ops::docs` を
//! 通して **`git` だけ**を上限付きで起こす（Phase 54 の P54-3 と同じ逸脱。ワーカーも LLM も起こさない）。

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{
    Project, ProjectId, ProjectRepo, RepoId, RepoKind, RepoRun, SqliteStore, Task, TaskStore,
    WorkspaceSpec,
};
use task_ops::changes as ops_changes;
use task_ops::docs::{self as ops_docs, DocCommit, PageEdit, PathError, WriteOutcome};
use time::OffsetDateTime;

use crate::handlers::{ApiResult, Params, json_response, no_query, parse_project_id, read_json};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, store_problem};
use crate::query::{QueryParams, parse_task_id};
use crate::state::ApiState;
use crate::types::ValidationError;

/// ツリーに出すページの上限（題名を読むために 1 枚ずつ `git show` を起こすので抑える）。
pub const MAX_TREE_PAGES: usize = 500;

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new()
        .route("/api/v1/projects/{id}/docs", get(docs_tree))
        .route("/api/v1/projects/{id}/docs/init", post(docs_init))
        .route(
            "/api/v1/projects/{id}/docs/maintenance",
            get(maintenance_view).post(maintenance_action),
        )
        .route(
            "/api/v1/projects/{id}/docs/page",
            get(docs_page).put(put_page).delete(delete_page),
        )
        .route("/api/v1/tasks/{id}/artifacts/promote", post(promote))
}

// ---------------------------------------------------------------------------
// 応答の型（`docs/gui/api.md` §3.92〜3.97）
// ---------------------------------------------------------------------------

/// ツリーの 1 件（`GET /projects/{id}/docs`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocItem {
    /// リポジトリ相対のパス（`docs/research/xxx.md`）。
    pub path: String,
    /// front matter の `title` → 1 行目の `# ` → ファイル名。
    pub title: String,
    /// 最後にこのページを触ったコミットの時刻（RFC 3339）。履歴が読めなければ `null`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit: Option<DocCommit>,
}

/// `GET /projects/{id}/docs`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocsTree {
    pub project_id: ProjectId,
    /// 文書の根になっているリポジトリの名前（`project_repos.name`）。
    pub repo: String,
    /// リポジトリの中での文書の根（`[outputs].docs`。既定 `docs`）。
    pub root: String,
    pub default_branch: String,
    /// `?q=` で絞ったならその文字列。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    pub items: Vec<DocItem>,
    /// [`MAX_TREE_PAGES`] で切った。
    pub truncated: bool,
}

/// `GET /projects/{id}/docs/page`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocPage {
    pub project_id: ProjectId,
    pub repo: String,
    pub root: String,
    pub default_branch: String,
    pub path: String,
    pub title: String,
    /// Markdown のもと（front matter を含む）。`too_large` なら空。
    pub raw: String,
    /// サーバで描画した HTML（生 HTML は捨ててある）。
    pub html: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// front matter の `tasks`（逆リンク）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
    /// 直近 20 件（新しい順）。
    pub history: Vec<DocCommit>,
    /// いまの blob sha。`PUT` / `DELETE` にそのまま渡す。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    pub too_large: bool,
}

/// `PUT /projects/{id}/docs/page` の本文。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocPagePutBody {
    pub path: String,
    pub body: String,
    /// 既にあるページを直すときは必須（無ければ 409 `etag_mismatch`）。
    #[serde(default)]
    pub etag: Option<String>,
    /// コミットメッセージ（既定 `docs: <path>`）。
    #[serde(default)]
    pub message: Option<String>,
}

/// `PUT` / `DELETE` / 昇格の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocPageResult {
    pub project_id: ProjectId,
    pub repo: String,
    pub path: String,
    /// 書いた後の blob sha（削除なら `null`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// default_branch の新しい先端。
    pub sha: String,
    pub deleted: bool,
    /// 中身が同じだったので新しいコミットは作らなかった。
    pub unchanged: bool,
}

/// `POST /projects/{id}/docs/init` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocsInitResult {
    pub project_id: ProjectId,
    pub repo: String,
    pub root: String,
    pub default_branch: String,
    /// この呼び出しで新しく作った（既にあったなら `false`）。
    pub created: bool,
    /// 作った（または見つけた）リポジトリの場所。
    pub path: String,
}

/// `POST /tasks/{id}/artifacts/promote` の本文。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactPromoteBody {
    /// 成果物の名前（`ArtifactRef.name`）。
    pub name: String,
    /// 宛先（リポジトリ相対。`docs/research/xxx.md`）。
    pub path: String,
    /// front matter の `title`（省略時は成果物の中身から）。
    #[serde(default)]
    pub title: Option<String>,
    /// 宛先が既にあっても上書きする。
    #[serde(default)]
    pub overwrite: bool,
}

// ---------------------------------------------------------------------------
// 文書の根（ADR-0044 D7）
// ---------------------------------------------------------------------------

/// 文書の根（決まった後の形）。
#[derive(Debug, Clone)]
struct DocsTarget {
    repo: String,
    /// 元のリポジトリ（人のチェックアウト）。
    path: PathBuf,
    /// リポジトリの中での文書の根（`docs`）。
    root: String,
    default_branch: String,
    /// この呼び出しで作った。
    created: bool,
}

fn docs_unavailable(detail: impl Into<String>) -> ApiProblem {
    ApiProblem::new(StatusCode::CONFLICT, "docs_unavailable", detail)
}

fn etag_mismatch(etag: Option<String>) -> ApiProblem {
    let problem = ApiProblem::new(
        StatusCode::CONFLICT,
        "etag_mismatch",
        "the page changed since it was read (reload and edit again)",
    );
    match etag {
        Some(etag) => problem.with_extra("etag", etag),
        None => problem,
    }
}

fn default_branch_busy(detail: String) -> ApiProblem {
    ApiProblem::new(StatusCode::CONFLICT, "default_branch_busy", detail)
}

fn page_not_found(path: &str) -> ApiProblem {
    ApiProblem::new(
        StatusCode::NOT_FOUND,
        "page_not_found",
        format!("page not found: {path}"),
    )
}

/// パスの検査（`..`・絶対パスは 403、`.md` 以外は 422）。
fn page_path(root: &str, raw: &str) -> Result<String, ApiProblem> {
    ops_docs::page_path(root, raw).map_err(|e| match e {
        PathError::Forbidden => ApiProblem::path_forbidden(e.to_string()),
        PathError::NotMarkdown => ApiProblem::validation(vec![ValidationError {
            field: Some("path".into()),
            message: e.to_string(),
        }]),
        PathError::Empty => ApiProblem::bad_request("path is required"),
    })
}

fn load_project(store: &SqliteStore, id: ProjectId) -> Result<Project, ApiProblem> {
    store
        .project_get(id)
        .map_err(store_problem)?
        .ok_or_else(|| ApiProblem::project_not_found(&id.to_string()))
}

/// ADR-0044 D7: 案件の文書の根を決める。
///
/// primary リポジトリ（ADR-0043 D1）が git で手元にあれば、その `[outputs].docs`（既定 `docs`）。
/// primary が `dir` か、リポジトリがそもそも無ければ、`create` のときだけ `~/workspace/<slug>/` に
/// 文書リポジトリを作って primary に登録する（`create` でなければ 409 `docs_unavailable`）。
fn docs_target(
    store: &SqliteStore,
    project: &Project,
    docs_repo_root: Option<&Path>,
    create: bool,
) -> Result<DocsTarget, ApiProblem> {
    let repos = store.repo_list(project.id).map_err(store_problem)?;
    let primary = repos.iter().find(|r| r.is_primary);
    if let Some(repo) = primary {
        match (&repo.location, repo.kind) {
            (WorkspaceSpec::Local { path, .. }, RepoKind::Git) => {
                if !path.join(".git").exists() {
                    return Err(docs_unavailable(format!(
                        "主なリポジトリ {} が手元にありません（{}）",
                        repo.name,
                        path.display()
                    )));
                }
                let (config, warning) = task_core::workspace_config::load_or_default(path);
                if let Some(warning) = warning {
                    tracing::warn!(repo = %repo.name, warning, "workspace.toml could not be read; using defaults");
                }
                return Ok(DocsTarget {
                    repo: repo.name.clone(),
                    default_branch: ops_changes::default_branch(
                        path,
                        repo.default_branch.as_deref(),
                    ),
                    path: path.clone(),
                    root: ops_docs::normalize_root(&config.outputs.docs),
                    created: false,
                });
            }
            // リモートのリポジトリ（ADR-0018 / 0019）は celeris からファイルが見えないので、この Phase では未対応。
            (WorkspaceSpec::Remote { cluster, .. }, _) => {
                return Err(docs_unavailable(format!(
                    "主なリポジトリ {} はリモート（{cluster}）です。文書はまだ手元のリポジトリだけです",
                    repo.name
                )));
            }
            // `dir` の primary は文書を持てない（ADR-0044 D7）。下で作る。
            (WorkspaceSpec::Local { .. }, RepoKind::Dir) => {}
        }
    }
    if !create {
        return Err(docs_unavailable(
            "この案件にはまだ文書リポジトリがありません（`POST /projects/{id}/docs/init` で用意する）",
        ));
    }
    create_docs_repo(store, project, docs_repo_root, &repos)
}

/// ADR-0044 D7: `~/workspace/<案件 slug>/` に文書リポジトリを作り、primary として登録する。
fn create_docs_repo(
    store: &SqliteStore,
    project: &Project,
    docs_repo_root: Option<&Path>,
    existing: &[ProjectRepo],
) -> Result<DocsTarget, ApiProblem> {
    let Some(base) = docs_repo_root else {
        return Err(docs_unavailable(
            "文書リポジトリの置き場が分かりません（$HOME が設定されていません）",
        ));
    };
    let project_id = project.id.to_string();
    let dir = ops_docs::docs_repo_dir(base, &project.title, &project_id);
    let fresh = !dir.join(".git").exists();
    ops_docs::init_docs_repo(&dir, &project.title).map_err(docs_unavailable)?;

    let location = WorkspaceSpec::local(&dir);
    // 名前は案件の中で一意（ディレクトリ名 → `-docs` → 案件 id の末尾）。
    let base_name = task_core::default_repo_name(&location);
    let name = [
        base_name.clone(),
        format!("{base_name}-docs"),
        format!("{base_name}-{}", project_id.to_ascii_lowercase()),
    ]
    .into_iter()
    .find(|candidate| {
        task_core::valid_repo_name(candidate) && !existing.iter().any(|r| &r.name == candidate)
    })
    .unwrap_or(base_name);
    let repo = ProjectRepo {
        id: RepoId::new(),
        project_id: project.id,
        name: name.clone(),
        kind: RepoKind::Git,
        location,
        default_branch: Some("main".to_string()),
        sync: None,
        run: RepoRun::Auto,
        is_primary: true,
        created_at: OffsetDateTime::now_utc(),
    };
    store.repo_create(&repo).map_err(store_problem)?;
    // `repo_create` は最初の 1 件しか自動で primary にしないので、`dir` の primary があるときは明示する。
    store.repo_set_primary(repo.id).map_err(store_problem)?;
    tracing::info!(
        who = "admin",
        op = "docs_init",
        project_id = %project.id,
        repo = %name,
        path = %dir.display(),
        "admin: created the default documents repository"
    );
    Ok(DocsTarget {
        repo: name,
        path: dir,
        root: task_core::workspace_config::DEFAULT_DOCS.to_string(),
        default_branch: "main".to_string(),
        created: fresh,
    })
}

/// GUI の文書タブの URL（ページの中の `[[…]]` のリンク先）。
fn link_base(project_id: ProjectId) -> String {
    format!("/projects/{project_id}/docs")
}

/// 一時 worktree の置き場（取り込みの `.integrate` と同じ流儀）。
fn temp_worktree(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(".docs")
        .join(ulid::Ulid::new().to_string())
}

// ---------------------------------------------------------------------------
// GET /projects/{id}/docs
// ---------------------------------------------------------------------------

async fn docs_tree(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    let project_id = parse_project_id(&id)?;
    let query = QueryParams::parse(raw.as_deref(), &["q"])?;
    let q = query
        .single("q")?
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_string);
    let docs_repo_root = state.inner.docs_repo_root.clone();
    let view = state
        .blocking(move |store| {
            let project = load_project(store, project_id)?;
            let target = docs_target(store, &project, docs_repo_root.as_deref(), false)?;
            let mut paths = match &q {
                Some(q) => ops_docs::grep(&target.path, &target.default_branch, &target.root, q),
                None => ops_docs::list(&target.path, &target.default_branch, &target.root),
            };
            paths.sort();
            paths.dedup();
            let truncated = paths.len() > MAX_TREE_PAGES;
            paths.truncate(MAX_TREE_PAGES);
            let last = ops_docs::last_commits(&target.path, &target.default_branch, &target.root);
            let items = paths
                .into_iter()
                .map(|path| {
                    let raw = ops_docs::read_page(&target.path, &target.default_branch, &path)
                        .unwrap_or_default();
                    let commit = last.get(&path).cloned();
                    DocItem {
                        title: ops_docs::title_of(&raw, &path),
                        updated_at: commit.as_ref().map(|c| c.at.clone()),
                        last_commit: commit,
                        path,
                    }
                })
                .collect();
            Ok(DocsTree {
                project_id,
                repo: target.repo,
                root: target.root,
                default_branch: target.default_branch,
                q,
                items,
                truncated,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

// ---------------------------------------------------------------------------
// GET /projects/{id}/docs/page
// ---------------------------------------------------------------------------

async fn docs_page(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    let project_id = parse_project_id(&id)?;
    let query = QueryParams::parse(raw.as_deref(), &["path"])?;
    let requested = query.single("path")?.unwrap_or_default().to_string();
    let docs_repo_root = state.inner.docs_repo_root.clone();
    let view = state
        .blocking(move |store| {
            let project = load_project(store, project_id)?;
            let target = docs_target(store, &project, docs_repo_root.as_deref(), false)?;
            let path = page_path(&target.root, &requested)?;
            let raw = ops_docs::read_page(&target.path, &target.default_branch, &path)
                .ok_or_else(|| page_not_found(&path))?;
            Ok(page_view(project_id, &target, &path, raw))
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

fn page_view(project_id: ProjectId, target: &DocsTarget, path: &str, raw: String) -> DocPage {
    let too_large = raw.len() > ops_docs::MAX_PAGE_BYTES;
    let (front, body) = ops_docs::front_matter(&raw);
    let html = if too_large {
        String::new()
    } else {
        ops_docs::render(body, &target.root, path, &link_base(project_id))
    };
    DocPage {
        project_id,
        repo: target.repo.clone(),
        root: target.root.clone(),
        default_branch: target.default_branch.clone(),
        title: ops_docs::title_of(&raw, path),
        tags: front.tags,
        tasks: front.tasks,
        history: ops_docs::history(&target.path, &target.default_branch, path),
        etag: ops_docs::blob_sha(&target.path, &target.default_branch, path),
        raw: if too_large { String::new() } else { raw },
        html,
        path: path.to_string(),
        too_large,
    }
}

// ---------------------------------------------------------------------------
// POST /projects/{id}/docs/init（管理系）
// ---------------------------------------------------------------------------

async fn docs_init(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let project_id = parse_project_id(&id)?;
    let docs_repo_root = state.inner.docs_repo_root.clone();
    let result = state
        .blocking(move |store| {
            let project = load_project(store, project_id)?;
            let target = docs_target(store, &project, docs_repo_root.as_deref(), true)?;
            Ok(DocsInitResult {
                project_id,
                repo: target.repo,
                root: target.root,
                default_branch: target.default_branch,
                created: target.created,
                path: target.path.to_string_lossy().into_owned(),
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

// ---------------------------------------------------------------------------
// PUT / DELETE /projects/{id}/docs/page（管理系）
// ---------------------------------------------------------------------------

async fn put_page(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let project_id = parse_project_id(&id)?;
    let request: DocPagePutBody = read_json(body, false).await?;
    let docs_repo_root = state.inner.docs_repo_root.clone();
    let workspace_root = state.inner.view.workspace_root.clone();
    let result = state
        .blocking(move |store| {
            let project = load_project(store, project_id)?;
            let target = docs_target(store, &project, docs_repo_root.as_deref(), true)?;
            let path = page_path(&target.root, &request.path)?;
            let message = request
                .message
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("docs: {path}"));
            let edit = PageEdit {
                path,
                body: Some(request.body.clone()),
                etag: request.etag.clone().filter(|e| !e.trim().is_empty()),
                message,
                overwrite: false,
            };
            write_page(&target, &workspace_root, project_id, edit)
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

async fn delete_page(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    require_admin(&state, &headers)?;
    let project_id = parse_project_id(&id)?;
    let query = QueryParams::parse(raw.as_deref(), &["path", "etag", "message"])?;
    let requested = query.single("path")?.unwrap_or_default().to_string();
    let etag = query
        .single("etag")?
        .map(str::to_string)
        .filter(|e| !e.trim().is_empty());
    let message = query
        .single("message")?
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string);
    let docs_repo_root = state.inner.docs_repo_root.clone();
    let workspace_root = state.inner.view.workspace_root.clone();
    let result = state
        .blocking(move |store| {
            let project = load_project(store, project_id)?;
            let target = docs_target(store, &project, docs_repo_root.as_deref(), false)?;
            let path = page_path(&target.root, &requested)?;
            let message = message
                .clone()
                .unwrap_or_else(|| format!("docs: remove {path}"));
            let edit = PageEdit {
                path,
                body: None,
                etag,
                message,
                overwrite: false,
            };
            write_page(&target, &workspace_root, project_id, edit)
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

/// `commit_page` の結果を HTTP に写す（409 の 2 種と 404 はここで決める）。
fn write_page(
    target: &DocsTarget,
    workspace_root: &Path,
    project_id: ProjectId,
    edit: PageEdit,
) -> Result<DocPageResult, ApiProblem> {
    let deleted = edit.body.is_none();
    let temp = temp_worktree(workspace_root);
    match ops_docs::commit_page(&target.path, &target.default_branch, &temp, &edit) {
        WriteOutcome::Written {
            sha,
            etag,
            unchanged,
            ..
        } => {
            tracing::info!(
                who = "admin",
                op = if deleted { "docs_delete" } else { "docs_put" },
                project_id = %project_id,
                repo = %target.repo,
                path = %edit.path,
                unchanged,
                "admin: document page committed"
            );
            Ok(DocPageResult {
                project_id,
                repo: target.repo.clone(),
                path: edit.path,
                etag,
                sha,
                deleted,
                unchanged,
            })
        }
        WriteOutcome::EtagMismatch { etag } => Err(etag_mismatch(etag)),
        WriteOutcome::Busy { detail } => Err(default_branch_busy(detail)),
        WriteOutcome::Missing => Err(page_not_found(&edit.path)),
        WriteOutcome::Failed { detail } => Err(docs_unavailable(detail)),
    }
}

// ---------------------------------------------------------------------------
// POST /tasks/{id}/artifacts/promote（管理系）
// ---------------------------------------------------------------------------

async fn promote(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let task_id = parse_task_id(&id)?;
    let request: ArtifactPromoteBody = read_json(body, false).await?;
    if request.name.trim().is_empty() {
        return Err(ApiProblem::validation(vec![ValidationError {
            field: Some("name".into()),
            message: "name must not be blank".into(),
        }]));
    }
    let docs_repo_root = state.inner.docs_repo_root.clone();
    let documentation_state_dir = state.inner.documentation_state_dir.clone();
    let workspace_root = state.inner.view.workspace_root.clone();
    let result = state
        .blocking(move |store| {
            let task = store
                .get(task_id)
                .map_err(store_problem)?
                .ok_or_else(|| ApiProblem::task_not_found(task_id))?;
            let project_id = task.project_id.ok_or_else(|| {
                docs_unavailable("このタスクは案件に属していません（文書の置き場がありません）")
            })?;
            let project = load_project(store, project_id)?;
            let target = docs_target(store, &project, docs_repo_root.as_deref(), true)?;
            let path = page_path(&target.root, &request.path)?;
            // Promotion is an explicit human publication decision. An adopted policy may
            // still forbid publishing residue/history/generated/unknown destinations.
            let policy = task_ops::docs_maintenance::load_policy(
                &documentation_state_dir, &format!("{project_id}:{}", target.repo),
            ).map_err(maintenance_problem)?;
            let category = policy.categories.get(&path).copied()
                .unwrap_or(task_ops::docs_maintenance::Category::Canonical);
            if !task_ops::docs_maintenance::may_publish(category, true, true) {
                return Err(maintenance_problem("policy does not classify this destination as current human-facing documentation"));
            }
            let content = artifact_text(store, &task, &workspace_root, request.name.trim())?;
            let current = ops_docs::blob_sha(&target.path, &target.default_branch, &path);
            if current.is_some() && !request.overwrite {
                return Err(ApiProblem::new(
                    StatusCode::CONFLICT,
                    "page_exists",
                    format!("page already exists: {path}（上書きするなら overwrite: true）"),
                ));
            }
            let title = request
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| ops_docs::title_of(&content, &path));
            let body = ops_docs::merge_front_matter(&content, Some(&title), &task_id.to_string());
            let edit = PageEdit {
                message: format!("docs: {path}（{} から昇格）", request.name.trim()),
                path,
                body: Some(body),
                etag: current,
                overwrite: true,
            };
            write_page(&target, &workspace_root, project_id, edit)
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

/// そのタスクの成果物（`ArtifactProduced` に記録された名前）の中身。無ければ 404。
fn artifact_text(
    store: &SqliteStore,
    task: &Task,
    workspace_root: &Path,
    name: &str,
) -> Result<String, ApiProblem> {
    let rows = store
        .event_rows_for(task.id, None, usize::MAX)
        .map_err(store_problem)?;
    let views = crate::files::artifact_views(task, workspace_root, &rows);
    // 同じ名前が何度も出るなら**最後**のもの（いちばん新しい run）。
    let view = views
        .iter()
        .rev()
        .find(|v| v.artifact.name == name)
        .ok_or_else(|| {
            ApiProblem::new(
                StatusCode::NOT_FOUND,
                "artifact_not_found",
                format!("artifact not found: {name}"),
            )
        })?;
    let ws = crate::files::canonical_workspace(task, workspace_root)?;
    let path = crate::files::resolve_artifact(&ws, &view.artifact.path)?;
    let bytes = std::fs::read(&path).map_err(|_| {
        ApiProblem::new(
            StatusCode::NOT_FOUND,
            "artifact_not_found",
            format!("artifact not readable: {name}"),
        )
    })?;
    String::from_utf8(bytes).map_err(|_| {
        ApiProblem::validation(vec![ValidationError {
            field: Some("name".into()),
            message: format!("artifact is not UTF-8 text: {name}"),
        }])
    })
}

// ---------------------------------------------------------------------------
// 逆リンク（`GET /tasks/{id}/timeline` に載せる。ADR-0044 D7）
// ---------------------------------------------------------------------------

/// そのタスクを front matter の `tasks:` に持つページ（安い `git grep -l <task id>` で絞ってから読む）。
/// 文書の根が無い案件・git が動かないときは**何も足さない**（タイムラインは壊さない）。
pub(crate) fn backlinks(
    store: &SqliteStore,
    task: &Task,
    docs_repo_root: Option<&Path>,
) -> Vec<(String, String, String, ProjectId)> {
    let Some(project_id) = task.project_id else {
        return Vec::new();
    };
    let Ok(project) = load_project(store, project_id) else {
        return Vec::new();
    };
    let Ok(target) = docs_target(store, &project, docs_repo_root, false) else {
        return Vec::new();
    };
    let id = task.id.to_string();
    let paths = ops_docs::grep(&target.path, &target.default_branch, &target.root, &id);
    if paths.is_empty() {
        return Vec::new();
    }
    let last = ops_docs::last_commits(&target.path, &target.default_branch, &target.root);
    let mut out = Vec::new();
    for path in paths {
        let Some(raw) = ops_docs::read_page(&target.path, &target.default_branch, &path) else {
            continue;
        };
        let (front, _) = ops_docs::front_matter(&raw);
        // 本文にたまたま id が出ただけのページは載せない（front matter の `tasks:` が紐付け）。
        if !front.tasks.iter().any(|t| t.trim() == id) {
            continue;
        }
        let at = last.get(&path).map(|c| c.at.clone()).unwrap_or_default();
        out.push((
            at,
            path.clone(),
            ops_docs::title_of(&raw, &path),
            project_id,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// パスの境界は `task_ops::docs` と同じ（403 / 422 / 400 に写す）。
    #[test]
    fn page_paths_map_to_the_right_problem() {
        assert_eq!(page_path("docs", "a.md").expect("ok"), "docs/a.md");
        assert_eq!(
            page_path("docs", "../escape.md").err().map(|p| p.code()),
            Some("path_forbidden")
        );
        assert_eq!(
            page_path("docs", "a.txt").err().map(|p| p.code()),
            Some("validation")
        );
        assert_eq!(
            page_path("docs", " ").err().map(|p| p.code()),
            Some("bad_request")
        );
    }
}

// Repository documentation lifecycle. Reports and policy live outside the repository.
async fn maintenance_view(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let project_id = parse_project_id(&id)?;
    let docs_repo_root = state.inner.docs_repo_root.clone();
    let documentation_state_dir = state.inner.documentation_state_dir.clone();
    let result = state
        .blocking(move |store| {
            let project = load_project(store, project_id)?;
            let target = docs_target(store, &project, docs_repo_root.as_deref(), false)?;

            let policy = task_ops::docs_maintenance::load_policy(
                &documentation_state_dir,
                &format!("{project_id}:{}", target.repo),
            )
            .map_err(maintenance_problem)?;
            let audit = task_ops::docs_maintenance::audit_with_policy(
                &target.path,
                &target.default_branch,
                &policy,
            )
            .map_err(maintenance_problem)?;
            let proposal = task_ops::docs_maintenance::proposal(&audit);
            let saved_report = std::fs::read(documentation_state_dir.join("repository-docs/reports").join(format!("{project_id}.json")))
                .ok().and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok());
            Ok(serde_json::json!({"audit":audit,"proposal":proposal,"policy":policy,"saved_report":saved_report}))
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

fn maintenance_problem(detail: impl Into<String>) -> ApiProblem {
    ApiProblem::new(StatusCode::CONFLICT, "docs_maintenance", detail)
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum MaintenanceAction {
    Audit,
    Adopt {
        policy: task_ops::docs_maintenance::Policy,
    },
    Approve {
        plan: task_ops::docs_maintenance::ReconcilePlan,
    },
    Apply {
        plan: task_ops::docs_maintenance::ReconcilePlan,
    },
}

async fn maintenance_action(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let project_id = parse_project_id(&id)?;
    let action: MaintenanceAction = read_json(body, false).await?;
    let docs_repo_root = state.inner.docs_repo_root.clone();
    let documentation_state_dir = state.inner.documentation_state_dir.clone();
    let workspace_root = state.inner.view.workspace_root.clone();
    let roles = state.inner.roles.clone();
    let genres = state.inner.genres.clone();
    let result = state
        .blocking(move |store| {
            use task_ops::docs_maintenance as maint;
            let project = load_project(store, project_id)?;
            let target = docs_target(store, &project, docs_repo_root.as_deref(), false)?;
            let state_dir = documentation_state_dir;
            let key = format!("{project_id}:{}", target.repo);
            match action {
                MaintenanceAction::Audit => {
                    let policy = maint::load_policy(&state_dir, &key).map_err(maintenance_problem)?;
                    let audit = maint::audit_with_policy(&target.path, &target.default_branch, &policy)
                        .map_err(maintenance_problem)?;
                    let result =
                        serde_json::json!({"audit": audit, "proposal": maint::proposal(&audit)});
                    let directory = state_dir.join("repository-docs/reports");
                    std::fs::create_dir_all(&directory)
                        .map_err(|e| maintenance_problem(e.to_string()))?;
                    std::fs::write(
                        directory.join(format!("{project_id}.json")),
                        serde_json::to_vec_pretty(&result).map_err(|e| maintenance_problem(e.to_string()))?,
                    )
                    .map_err(|e| maintenance_problem(e.to_string()))?;
                    Ok(result)
                }
                MaintenanceAction::Adopt { policy } => {
                    maint::save_policy(&state_dir, &key, &policy).map_err(maintenance_problem)?;
                    Ok(serde_json::json!({"policy":policy}))
                }
                MaintenanceAction::Approve { plan } => {
                    maint::approve_plan(&state_dir, &key, &plan).map_err(maintenance_problem)?;
                    Ok(serde_json::json!({"approved":true,"plan":plan}))
                }
                MaintenanceAction::Apply { plan } => {
                    let worktree = state_dir
                        .join("repository-docs/worktrees")
                        .join(ulid::Ulid::new().to_string());
                    let sha = maint::apply_plan(
                        &target.path,
                        &target.default_branch,
                        &worktree,
                        &plan,
                        &state_dir,
                        &key,
                    )
                    .map_err(maintenance_problem)?;
                    // A draft task makes the isolated result visible to the existing changes/review UI.
                // It cannot race dispatch before the marker and evidence have been written.
                let spec: task_ops::add::NewTaskSpec = serde_json::from_value(serde_json::json!({
                    "title": format!("文書整理の検証: {}", target.repo),
                    "objective": "承認済み文書整理の差分を検証し、既存のレビュー・マージ経路へ引き渡す。対象を広げず、本番/default branchへ直接反映しない。成果物 reconciliation-plan.json と作業ツリーのコミットを確認する。",
                    "acceptance": [{"type":"reviewer","text":"差分が承認済み reconciliation-plan.json と一致し文書のリンク・内容が妥当"}],
                    "project_id": project_id, "repos": [target.repo], "status":"draft",
                    "skills":["software"], "workspace":target.path
                })).map_err(|e| maintenance_problem(e.to_string()))?;
                let task = task_ops::add::create_task_with_roles(store, spec, &roles, &genres, OffsetDateTime::now_utc())
                    .map_err(|e| maintenance_problem(e.to_string()))?;
                let task_dir = workspace_root.join(task.id.to_string());
                let attached = task_dir.join("repos").join(&target.repo);
                std::fs::create_dir_all(task_dir.join("repos")).map_err(|e| maintenance_problem(e.to_string()))?;
                let moved = ops_changes::git(&target.path, &["worktree", "move", &worktree.to_string_lossy(), &attached.to_string_lossy()], std::time::Duration::from_secs(30))
                    .is_some_and(|output| output.ok);
                if !moved { return Err(maintenance_problem("could not attach reconciliation worktree to verification task")); }
                let worktree = attached;
                let branch = ops_changes::current_branch(&worktree).ok_or_else(|| maintenance_problem("worktree branch missing"))?;
                task_ops::workspace::write_marker(&task_dir, &task_ops::workspace::WorktreeMarker {
                    repo: target.path.display().to_string(), dir: worktree.display().to_string(),
                    branch: branch.clone(), base: plan.revision.clone(), base_kind: target.default_branch.clone(),
                    repos: vec![task_ops::workspace::WorktreeMarkerRepo {
                        name:target.repo, kind:"git".into(), source:target.path.display().to_string(),
                        dir:worktree.display().to_string(), branch:Some(branch), base:Some(plan.revision.clone()),
                        base_kind:Some(target.default_branch),
                    }],
                }).map_err(|e| maintenance_problem(e.to_string()))?;
                let artifacts = task_dir.join("artifacts");
                std::fs::create_dir_all(&artifacts).map_err(|e| maintenance_problem(e.to_string()))?;
                std::fs::write(artifacts.join("reconciliation-plan.json"),serde_json::to_vec_pretty(&plan).map_err(|e| maintenance_problem(e.to_string()))?)
                    .map_err(|e| maintenance_problem(e.to_string()))?;
                Ok(serde_json::json!({"sha":sha,"worktree":worktree,"merged":false,"task_id":task.id}))
                }
            }
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}
