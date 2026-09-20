//! 変更の取り込み（ADR-0043 D5。Phase 54）:
//!
//! - `GET  /tasks/{id}/changes` — リポジトリごとの差分の要約（読み取り。トークンは要らない）
//! - `GET  /tasks/{id}/changes/{repo}/diff?path=` — 1 ファイルの unified diff（200 KiB で切る）
//! - `POST /tasks/{id}/changes/{repo}/integrate` — **管理系**。`merge` / `pr` / `discard`
//! - `POST /tasks/{id}/changes/{repo}/pr/merge` — **管理系**。`gh pr merge`
//! - `GET  /projects/{id}/integrations` — 案件の PR と取り込みの一覧（読み取り）
//!
//! **取り込みを押せるのは人だけ**（SPEC §3.6 / ADR-0043 D5）。ワーカーのプロトコル（`docs/protocol/`）には
//! この経路を出さない ＝ 組織の「人」が `main` を動かす道は無い。
//!
//! 境界について（ADR-0013 は「API はワーカーの起動・コマンドの実行をしない」と決めている）:
//! ここは **`git` と `gh` だけ**を、上限付きで、人が押したときと画面を開いたときに起こす
//! （`task_ops::changes`）。ワーカーも LLM も起こさない。ADR-0043 D5 がこの API を要求しているので、
//! この Phase の逸脱として PROGRESS（P54-3）に書いてある。
//!
//! 見る場所は Phase 52 と同じ**目印**（`<task_dir>/worktree.json`）。目印が無いタスクは 404。

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use task_core::{
    IntegrationMethod, IntegrationState, ProjectId, ReportFilter, ReportStore, RepoId, Task, TaskId, TaskIntegration,
    TaskStore,
};
use task_ops::changes::{self as ops_changes, MergeOutcome};
use time::OffsetDateTime;

use crate::handlers::{ApiResult, Params, json_response, no_query, parse_project_id, read_json};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, store_problem};
use crate::query::{QueryParams, parse_task_id};
use crate::state::ApiState;
use crate::types::{
    ChangeDiffView, ChangesView, IntegrateBody, IntegrateResult, ProjectIntegrationItem, ProjectIntegrations,
    RepoChangesView, ValidationError,
};

/// ADR-0043 D5: 案件画面の一覧で 1 回に同期する PR の上限（画面を開くたびに `gh` を起こすので抑える）。
pub const MAX_REFRESH_PER_CALL: usize = 20;
/// 案件画面に出す取り込みの件数の上限。
const MAX_PROJECT_ITEMS: usize = 200;
/// PR の本文に入れる報告の行数。
const REPORT_BODY_LINES: usize = 12;

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new()
        .route("/api/v1/tasks/{id}/changes", get(changes))
        .route("/api/v1/tasks/{id}/changes/{repo}/diff", get(diff))
        .route("/api/v1/tasks/{id}/changes/{repo}/integrate", post(integrate))
        .route("/api/v1/tasks/{id}/changes/{repo}/pr/merge", post(pr_merge))
        .route("/api/v1/projects/{id}/integrations", get(project_integrations))
}

// ---------------------------------------------------------------------------
// 見る先の組み立て（目印 + 案件のリポジトリ）
// ---------------------------------------------------------------------------

/// 取り込みの対象になる git のリポジトリ 1 件。
#[derive(Debug, Clone)]
struct RepoTarget {
    name: String,
    /// `project_repos.id`。Phase 49 の 1 リポジトリのタスクは `None`。
    repo_id: Option<RepoId>,
    /// 元のリポジトリ（人のチェックアウト）。
    source: PathBuf,
    /// タスクの作業ツリー（`<task_dir>/repos/<name>` か `<task_dir>/tree`）。
    worktree: PathBuf,
    branch: String,
    /// 目印に書いてある base（`merge-base` が取れなかったときの保険）。
    base: Option<String>,
    default_branch: String,
}

/// そのタスクの git のリポジトリを並べる（`dir` のリポジトリは対象外。ADR-0043 D5）。
fn targets_for(store: &dyn TaskStore, task: &Task, workspace_root: &Path) -> Result<Vec<RepoTarget>, ApiProblem> {
    let marker = crate::tree::marker_of(task, workspace_root)?;
    let repos = crate::tree::marker_repos(&marker);
    // その案件のリポジトリの行（`default_branch` と `repo_id` を引くため）。
    let rows = match task.project_id {
        Some(project_id) => store.repo_list(project_id).map_err(store_problem)?,
        None => Vec::new(),
    };
    let mut out = Vec::new();
    for repo in repos.iter().filter(|r| r.kind == "git") {
        let Some(branch) = repo.branch.clone().filter(|b| !b.is_empty()) else {
            continue;
        };
        let row = task
            .repos
            .iter()
            .find(|r| r.name == repo.name)
            .and_then(|r| rows.iter().find(|row| row.id == r.repo_id))
            .or_else(|| rows.iter().find(|row| row.name == repo.name));
        let source = PathBuf::from(&repo.source);
        let default_branch = ops_changes::default_branch(&source, row.and_then(|r| r.default_branch.as_deref()));
        out.push(RepoTarget {
            name: repo.name.clone(),
            repo_id: row.map(|r| r.id),
            source,
            worktree: PathBuf::from(&repo.dir),
            branch,
            base: repo.base.clone().filter(|b| !b.is_empty()),
            default_branch,
        });
    }
    Ok(out)
}

fn pick_target(targets: &[RepoTarget], name: &str) -> Result<RepoTarget, ApiProblem> {
    targets
        .iter()
        .find(|t| t.name == name)
        .cloned()
        .ok_or_else(|| ApiProblem::file_not_found(format!("repo not found in this task: {name}")))
}

fn load_task(store: &dyn TaskStore, task_id: TaskId) -> Result<Task, ApiProblem> {
    store
        .get(task_id)
        .map_err(store_problem)?
        .ok_or_else(|| ApiProblem::task_not_found(task_id))
}

// ---------------------------------------------------------------------------
// PR の同期（ADR-0043 D5: 画面を開いたときだけ）
// ---------------------------------------------------------------------------

/// 開いている PR の状態を `gh pr view` で見直す。merge されていたら worktree とブランチを片付ける。
/// `gh` が失敗したら**記録は触らない**（画面を開いただけで `failed` にしない）。
fn refresh_pr(
    store: &dyn TaskStore,
    gh: &str,
    target: Option<&RepoTarget>,
    integration: TaskIntegration,
    now: OffsetDateTime,
) -> TaskIntegration {
    if !integration.state.needs_refresh() {
        return integration;
    }
    let (Some(number), Some(target)) = (integration.pr_number, target) else {
        return integration;
    };
    let Ok(view) = ops_changes::gh_pr_view(gh, &target.source, number) else {
        return integration;
    };
    let Some(state) = TaskIntegration::pr_state(&view.state) else {
        return integration;
    };
    let mut next = integration;
    if state == IntegrationState::Merged {
        // ADR-0043 D5: merge されたら（開いたときに検知して）worktree を消す。
        if let Err(e) = ops_changes::remove_worktree_and_branch(&target.source, Some(&target.worktree), &target.branch)
        {
            tracing::warn!(repo = %target.name, error = %e, "cannot clean up the worktree of a merged pull request");
        }
        next.merged_at = view
            .merged_at
            .as_deref()
            .and_then(|s| OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok())
            .or(Some(now));
    }
    if state == next.state && view.url.as_deref() == next.pr_url.as_deref() {
        return next;
    }
    next.state = state;
    if let Some(url) = view.url {
        next.pr_url = Some(url);
    }
    next.updated_at = now;
    if let Err(e) = store.integration_put(&next) {
        tracing::warn!(error = %e, "cannot record the refreshed pull request state");
    }
    next
}

// ---------------------------------------------------------------------------
// GET /tasks/{id}/changes
// ---------------------------------------------------------------------------

async fn changes(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let task_id = parse_task_id(&id)?;
    let workspace_root = state.inner.view.workspace_root.clone();
    let github = state.inner.github.clone();
    let view = state
        .blocking(move |store| {
            let task = load_task(store, task_id)?;
            let targets = targets_for(store, &task, &workspace_root)?;
            let now = OffsetDateTime::now_utc();
            let mut repos = Vec::with_capacity(targets.len());
            for target in &targets {
                let c = ops_changes::changes(
                    &target.source,
                    Some(&target.worktree),
                    &target.branch,
                    &target.default_branch,
                    target.base.as_deref(),
                );
                let integration = store
                    .integration_latest(task_id, &target.name)
                    .map_err(store_problem)?
                    .map(|i| refresh_pr(store, &github.gh, Some(target), i, now));
                repos.push(RepoChangesView {
                    repo: target.name.clone(),
                    branch: target.branch.clone(),
                    default_branch: target.default_branch.clone(),
                    base: c.base,
                    head: c.head,
                    ahead: c.ahead,
                    files: c.files,
                    stat: c.stat,
                    dirty: c.dirty,
                    missing: c.missing,
                    origin: ops_changes::has_origin(&target.source),
                    integration,
                });
            }
            // `gh` が使えるか（プロセス内で 60 秒だけ覚える。ADR-0043 D5）。
            let gh = targets
                .first()
                .is_some_and(|t| ops_changes::gh_authenticated(&github.gh, &t.source));
            Ok(ChangesView {
                task_id: task_id.to_string(),
                repos,
                gh,
                merge_method: github.merge_method.clone(),
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

// ---------------------------------------------------------------------------
// GET /tasks/{id}/changes/{repo}/diff?path=
// ---------------------------------------------------------------------------

async fn diff(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params((id, repo)): Params<(String, String)>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    let task_id = parse_task_id(&id)?;
    let query = QueryParams::parse(raw.as_deref(), &["path"])?;
    let path = query
        .single("path")?
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| ApiProblem::bad_request("path is required"))?
        .to_string();
    // 境界は Phase 52（§3.72）と同じ規則。`git` に渡す前にここで弾く。
    if Path::new(&path).is_absolute()
        || Path::new(&path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::Prefix(_)))
    {
        return Err(ApiProblem::path_forbidden("path must be relative and must not contain `..`"));
    }
    let workspace_root = state.inner.view.workspace_root.clone();
    let view = state
        .blocking(move |store| {
            let task = load_task(store, task_id)?;
            let targets = targets_for(store, &task, &workspace_root)?;
            let target = pick_target(&targets, &repo)?;
            let diff = ops_changes::file_diff(
                &target.source,
                Some(&target.worktree),
                &target.branch,
                &target.default_branch,
                target.base.as_deref(),
                &path,
            )
            .ok_or_else(|| ApiProblem::file_not_found(format!("{repo} has no working tree or branch any more")))?;
            Ok(ChangeDiffView {
                repo: target.name.clone(),
                path: diff.path,
                diff: diff.diff,
                truncated: diff.truncated,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

// ---------------------------------------------------------------------------
// POST /tasks/{id}/changes/{repo}/integrate（管理系。人だけ）
// ---------------------------------------------------------------------------

/// ADR-0043 D5: 人のチェックアウトが default_branch を編集中（409）。
fn default_branch_busy(detail: String) -> ApiProblem {
    ApiProblem::new(StatusCode::CONFLICT, "default_branch_busy", detail)
}

/// ADR-0043 D5: `pr` の前提（`origin` と `gh`）が揃っていない（409）。
fn pr_unavailable(detail: impl Into<String>) -> ApiProblem {
    ApiProblem::new(StatusCode::CONFLICT, "pr_unavailable", detail)
}

async fn integrate(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params((id, repo)): Params<(String, String)>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let task_id = parse_task_id(&id)?;
    let request: IntegrateBody = read_json(body, false).await?;
    if request.method == IntegrationMethod::Discard && !request.confirm {
        return Err(ApiProblem::validation(vec![ValidationError {
            field: Some("confirm".into()),
            message: "discard removes the worktree and the branch; send {\"confirm\": true}".into(),
        }]));
    }
    let workspace_root = state.inner.view.workspace_root.clone();
    let github = state.inner.github.clone();
    let gui_base_url = state.inner.notify_gui_base_url.clone();
    let result = state
        .blocking(move |store| {
            let task = load_task(store, task_id)?;
            let targets = targets_for(store, &task, &workspace_root)?;
            let target = pick_target(&targets, &repo)?;
            let now = OffsetDateTime::now_utc();
            let note = request.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
            let record = |state: IntegrationState, detail: String| {
                let mut integration = TaskIntegration::new(
                    task_id,
                    target.repo_id,
                    target.name.clone(),
                    request.method,
                    state,
                    now,
                );
                integration.detail = match (note, detail.trim().is_empty()) {
                    (Some(note), true) => Some(note.to_string()),
                    (Some(note), false) => Some(format!("{note} / {detail}")),
                    (None, true) => None,
                    (None, false) => Some(detail),
                };
                integration
            };
            let (mut integration, child) = match request.method {
                IntegrationMethod::Discard => match ops_changes::discard(
                    &target.source,
                    Some(&target.worktree),
                    &target.branch,
                ) {
                    Ok(()) => (
                        record(IntegrationState::Done, format!("{} を捨てました", target.branch)),
                        None,
                    ),
                    Err(detail) => (record(IntegrationState::Failed, detail), None),
                },
                IntegrationMethod::Merge => {
                    let temp = workspace_root
                        .join(".integrate")
                        .join(ulid::Ulid::new().to_string());
                    match ops_changes::merge_into_default_branch(
                        &target.source,
                        &target.branch,
                        &target.default_branch,
                        &temp,
                    ) {
                        // ADR-0043 D5: 人が片付けてから押す。何も触っていないので記録も残さない。
                        MergeOutcome::Busy { detail } => return Err(default_branch_busy(detail)),
                        MergeOutcome::Failed { detail } => (record(IntegrationState::Failed, detail), None),
                        MergeOutcome::Conflict { files } => {
                            let child = task_ops::changes::conflict_child_task(
                                &task,
                                &target.name,
                                &target.worktree,
                                &target.default_branch,
                                &files,
                                now,
                            );
                            store
                                .create_task(
                                    &child,
                                    vec![task_core::Event::Created {
                                        task: Box::new(child.clone()),
                                    }],
                                )
                                .map_err(store_problem)?;
                            let listed = if files.is_empty() {
                                String::new()
                            } else {
                                format!("（{}）", files.join(", "))
                            };
                            (
                                record(
                                    IntegrationState::Conflict,
                                    format!(
                                        "{} への rebase が衝突しました{listed}。解消タスク {} を作りました",
                                        target.default_branch, child.id
                                    ),
                                ),
                                Some(child.id),
                            )
                        }
                        MergeOutcome::Merged { sha, fast_forwarded } => {
                            let mut detail = format!(
                                "{} を {} まで進めました（{}）",
                                target.default_branch,
                                sha.chars().take(12).collect::<String>(),
                                if fast_forwarded { "fast-forward" } else { "ref のみ" }
                            );
                            if let Err(e) = ops_changes::remove_worktree_and_branch(
                                &target.source,
                                Some(&target.worktree),
                                &target.branch,
                            ) {
                                detail = format!("{detail}。後片付けに失敗: {e}");
                            }
                            let mut integration = record(IntegrationState::Done, detail);
                            integration.merged_at = Some(now);
                            (integration, None)
                        }
                    }
                }
                IntegrationMethod::Pr => {
                    if !ops_changes::has_origin(&target.source) {
                        return Err(pr_unavailable(format!(
                            "{} には origin リモートがありません（PR は作れません）",
                            target.name
                        )));
                    }
                    if !ops_changes::gh_authenticated(&github.gh, &target.source) {
                        return Err(pr_unavailable(
                            "gh が使えません（PATH に無いか、認証されていません）".to_string(),
                        ));
                    }
                    match ops_changes::push_branch(&target.source, &target.branch) {
                        Err(detail) => (record(IntegrationState::Failed, detail), None),
                        Ok(()) => {
                            let report = latest_report_for(store, &task);
                            let body = pr_body(&task, report.as_ref(), gui_base_url.as_deref());
                            match ops_changes::gh_pr_create(
                                &github.gh,
                                &target.source,
                                &target.default_branch,
                                &target.branch,
                                &task.title,
                                &body,
                            ) {
                                Err(detail) => (record(IntegrationState::Failed, detail), None),
                                Ok(pr) => {
                                    let mut integration =
                                        record(IntegrationState::Open, format!("PR #{} を作りました", pr.number));
                                    integration.pr_number = Some(pr.number);
                                    integration.pr_url = Some(pr.url);
                                    (integration, None)
                                }
                            }
                        }
                    }
                }
            };
            integration.updated_at = now;
            store.integration_put(&integration).map_err(store_problem)?;
            tracing::info!(
                who = "admin",
                op = "integrate",
                task_id = %task_id,
                repo = %target.name,
                method = %request.method.as_str(),
                state = %integration.state.as_str(),
                "admin: task changes integrated"
            );
            Ok(IntegrateResult {
                integration,
                child_task_id: child.map(|id| id.to_string()),
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

// ---------------------------------------------------------------------------
// POST /tasks/{id}/changes/{repo}/pr/merge（管理系。人だけ）
// ---------------------------------------------------------------------------

async fn pr_merge(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params((id, repo)): Params<(String, String)>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let task_id = parse_task_id(&id)?;
    let workspace_root = state.inner.view.workspace_root.clone();
    let github = state.inner.github.clone();
    let result = state
        .blocking(move |store| {
            let task = load_task(store, task_id)?;
            let targets = targets_for(store, &task, &workspace_root)?;
            let target = pick_target(&targets, &repo)?;
            let now = OffsetDateTime::now_utc();
            let integration = store
                .integration_latest(task_id, &target.name)
                .map_err(store_problem)?
                .ok_or_else(|| pr_unavailable(format!("{} にはまだ PR がありません", target.name)))?;
            let Some(number) = integration.pr_number.filter(|_| integration.state == IntegrationState::Open) else {
                return Err(pr_unavailable(format!(
                    "{} の PR は開いていません（いまは {}）",
                    target.name,
                    integration.state.as_str()
                )));
            };
            let mut integration = integration;
            if let Err(detail) = ops_changes::gh_pr_merge(&github.gh, &target.source, number, &github.merge_method) {
                integration.state = IntegrationState::Failed;
                integration.detail = Some(detail);
                integration.updated_at = now;
                store.integration_put(&integration).map_err(store_problem)?;
                return Ok(IntegrateResult {
                    integration,
                    child_task_id: None,
                });
            }
            tracing::info!(
                who = "admin",
                op = "pr_merge",
                task_id = %task_id,
                repo = %target.name,
                pr = number,
                "admin: pull request merged through Celeris"
            );
            // merge した直後に同期して、merge されていれば worktree とブランチを片付ける。
            let integration = refresh_pr(store, &github.gh, Some(&target), integration, now);
            Ok(IntegrateResult {
                integration,
                child_task_id: None,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

// ---------------------------------------------------------------------------
// GET /projects/{id}/integrations
// ---------------------------------------------------------------------------

async fn project_integrations(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let project_id: ProjectId = parse_project_id(&id)?;
    let workspace_root = state.inner.view.workspace_root.clone();
    let github = state.inner.github.clone();
    let view = state
        .blocking(move |store| {
            if store.project_get(project_id).map_err(store_problem)?.is_none() {
                return Err(ApiProblem::project_not_found(&project_id.to_string()));
            }
            let rows = store
                .integration_list_for_project(project_id, MAX_PROJECT_ITEMS)
                .map_err(store_problem)?;
            let now = OffsetDateTime::now_utc();
            let mut refreshed = 0usize;
            let mut items = Vec::with_capacity(rows.len());
            for row in rows {
                let Some(task) = store.get(row.task_id).map_err(store_problem)? else {
                    continue;
                };
                // ADR-0043 D5: 1 回の呼び出しで同期する PR は 20 件まで。
                let integration = if row.state.needs_refresh() && refreshed < MAX_REFRESH_PER_CALL {
                    refreshed += 1;
                    let target = targets_for(store, &task, &workspace_root)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|t| t.name == row.repo);
                    refresh_pr(store, &github.gh, target.as_ref(), row, now)
                } else {
                    row
                };
                items.push(ProjectIntegrationItem {
                    integration,
                    task_title: task.title.clone(),
                    task_status: task.status,
                });
            }
            Ok(ProjectIntegrations { items })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

// ---------------------------------------------------------------------------
// PR の本文（決定的。LLM は使わない）
// ---------------------------------------------------------------------------

/// そのタスクの最新の報告（`reports.task_id`）。無ければ `None`。
/// `ReportFilter` に `task_id` が無いので、案件（あれば）で絞ってから突き合わせる。
fn latest_report_for(store: &(impl ReportStore + ?Sized), task: &Task) -> Option<task_core::Report> {
    let filter = ReportFilter {
        project_id: task.project_id,
        limit: 200,
        ..ReportFilter::default()
    };
    store
        .report_list(&filter)
        .ok()?
        .into_iter()
        .find(|r| r.task_id == Some(task.id))
}

/// ADR-0043 D5 の PR の本文（**決定的**: 目的 / 受け入れ条件 / 最新の報告の要約 / Celeris のタスクへのリンク）。
pub fn pr_body(task: &Task, report: Option<&task_core::Report>, gui_base_url: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("## 目的\n\n");
    out.push_str(task.objective.trim());
    out.push_str("\n\n## 受け入れ条件\n\n");
    if task.acceptance.is_empty() {
        out.push_str("（なし）\n");
    } else {
        for criterion in &task.acceptance {
            out.push_str(&format!("- {}\n", criterion.text.trim()));
        }
    }
    if let Some(report) = report {
        out.push_str("\n## 最新の報告\n\n");
        out.push_str(&format!("**{}**\n\n", report.headline.trim()));
        let body: Vec<&str> = report.body.lines().take(REPORT_BODY_LINES).collect();
        if !body.is_empty() {
            out.push_str(body.join("\n").trim_end());
            out.push('\n');
        }
        if report.body.lines().count() > REPORT_BODY_LINES {
            out.push_str("\n（報告の続きは Celeris で読めます）\n");
        }
    }
    out.push_str("\n---\n\n");
    match gui_base_url.map(str::trim).filter(|u| !u.is_empty()) {
        Some(base) => out.push_str(&format!(
            "Celeris task {id}: {base}/tasks/{id}\n",
            id = task.id,
            base = base.trim_end_matches('/')
        )),
        None => out.push_str(&format!("Celeris task {}\n", task.id)),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Budget, Check, Criterion, Report, ReportKind, Status, TaskKind, Tier, WorkerHint, WorkspaceSpec};

    fn task() -> Task {
        let id = TaskId::new();
        let now = OffsetDateTime::now_utc();
        Task { mode: Default::default(), skills: Vec::new(),
            repos: Vec::new(),
            id,
            parent_id: None,
            kind: TaskKind::Execute,
            title: "ワークスペース A2".into(),
            objective: "変更の取り込みを作る".into(),
            acceptance: vec![
                Criterion { text: "テストが通る".into(), check: Check::Human },
                Criterion {
                    text: "`cargo test` が exit 0".into(),
                    check: Check::Command { cmd: "cargo test".into(), expect_exit: 0 },
                },
            ],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Done,
            priority: 0,
            worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
            workspace: WorkspaceSpec::Local { path: PathBuf::from("/srv/repo"), mode: None },
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
            labels: Vec::new(),
            category: task_core::TaskCategory::default(),
            conversation: None,
        }
    }

    /// ADR-0043 D5: PR の本文は決定的（同じタスクからは同じ文字列）。
    #[test]
    fn the_pull_request_body_is_deterministic_and_links_back_to_celeris() {
        let task = task();
        let body = pr_body(&task, None, Some("http://192.168.1.103:7700/"));
        assert_eq!(body, pr_body(&task, None, Some("http://192.168.1.103:7700/")));
        assert!(body.contains("## 目的"), "{body}");
        assert!(body.contains("変更の取り込みを作る"), "{body}");
        assert!(body.contains("- テストが通る"), "{body}");
        assert!(
            body.contains(&format!("Celeris task {id}: http://192.168.1.103:7700/tasks/{id}", id = task.id)),
            "{body}"
        );
        // `gui_base_url` が無ければリンクは付けない。
        let plain = pr_body(&task, None, None);
        assert!(plain.contains(&format!("Celeris task {}", task.id)), "{plain}");
        assert!(!plain.contains("http"), "{plain}");
    }

    /// 報告があれば見出しと本文の先頭が入る（長い本文は切る）。
    #[test]
    fn the_pull_request_body_summarises_the_latest_report() {
        let task = task();
        let report = Report {
            id: task_core::ReportId::new(),
            project_id: None,
            node_id: "impl".into(),
            task_id: Some(task.id),
            kind: ReportKind::Result,
            level: 1,
            headline: "取り込みを実装した".into(),
            body: (1..=20).map(|i| format!("行 {i}")).collect::<Vec<_>>().join("\n"),
            sources: vec![],
            read_at: None,
            created_at: OffsetDateTime::now_utc(),
        };
        let body = pr_body(&task, Some(&report), None);
        assert!(body.contains("**取り込みを実装した**"), "{body}");
        assert!(body.contains("行 1"), "{body}");
        assert!(body.contains(&format!("行 {REPORT_BODY_LINES}")), "{body}");
        assert!(!body.contains(&format!("行 {}", REPORT_BODY_LINES + 1)), "{body}");
        assert!(body.contains("（報告の続きは Celeris で読めます）"), "{body}");
    }
}
