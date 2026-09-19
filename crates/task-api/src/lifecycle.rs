//! ADR-0044 D6（Phase 55）: 案件・途中目標の **中止・一時停止・アーカイブ**。
//!
//! - `POST /projects/{id}/{cancel|pause|resume|archive|unarchive}`
//! - `POST /milestones/{id}/{cancel|pause|resume}`
//!
//! どれも**管理系**（`token_file` 未設定でも 401。ADR-0044 §5 Phase 53 追記「変更を伴う API は
//! すべて管理系に揃える」）。本文は取らない（`{}` でも空でもよい）。
//!
//! ハンドラは HTTP への写像だけで、判断と連鎖は `task_ops::lifecycle` が行う（DESIGN 原則 1〜4）。
//! 応答は 200 で、`cancel` は連鎖で `cancelled` になったタスク（と途中目標）を添える。
//! 404（知らない id）／409（その状態ではできない）／401（トークン無し）。

use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use task_ops::lifecycle;
use time::OffsetDateTime;

use crate::handlers::{ApiResult, Params, json_response, no_query, parse_milestone_id, parse_project_id, read_json};
use crate::middleware::require_admin;
use crate::problem::ops_problem;
use crate::state::ApiState;

/// 本文は取らない（`{}` か空）。余計なキーは 422。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyBody {}

/// 案件の 5 つの操作（URL の末尾）。
#[derive(Debug, Clone, Copy)]
enum ProjectAction {
    Cancel,
    Pause,
    Resume,
    Archive,
    Unarchive,
}

/// 途中目標の 3 つの操作。
#[derive(Debug, Clone, Copy)]
enum MilestoneAction {
    Cancel,
    Pause,
    Resume,
}

async fn project_action(
    state: ApiState,
    headers: HeaderMap,
    raw: Option<String>,
    body: Body,
    id: String,
    action: ProjectAction,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let project_id = parse_project_id(&id)?;
    let EmptyBody {} = read_json(body, true).await?;
    let trigger = match action {
        ProjectAction::Cancel => "project_cancel",
        ProjectAction::Pause => "project_pause",
        ProjectAction::Resume => "project_resume",
        ProjectAction::Archive => "project_archive",
        ProjectAction::Unarchive => "project_unarchive",
    };
    let result = state
        .blocking(move |store| {
            let outcome = match action {
                ProjectAction::Cancel => lifecycle::cancel_project(store, project_id),
                ProjectAction::Pause => lifecycle::pause_project(store, project_id),
                ProjectAction::Resume => lifecycle::resume_project(store, project_id),
                ProjectAction::Archive => {
                    lifecycle::archive_project(store, project_id, OffsetDateTime::now_utc())
                }
                ProjectAction::Unarchive => lifecycle::unarchive_project(store, project_id),
            };
            outcome.map_err(|e| ops_problem(store, e, Some(trigger)))
        })
        .await?;
    tracing::info!(who = "admin", op = trigger, project_id = %project_id, status = %result.project.status.as_str(), "admin: project lifecycle");
    Ok(json_response(StatusCode::OK, &result))
}

async fn milestone_action(
    state: ApiState,
    headers: HeaderMap,
    raw: Option<String>,
    body: Body,
    id: String,
    action: MilestoneAction,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let milestone_id = parse_milestone_id(&id)?;
    let EmptyBody {} = read_json(body, true).await?;
    let trigger = match action {
        MilestoneAction::Cancel => "milestone_cancel",
        MilestoneAction::Pause => "milestone_pause",
        MilestoneAction::Resume => "milestone_resume",
    };
    let result = state
        .blocking(move |store| {
            let outcome = match action {
                MilestoneAction::Cancel => lifecycle::cancel_milestone(store, milestone_id),
                MilestoneAction::Pause => lifecycle::pause_milestone(store, milestone_id),
                MilestoneAction::Resume => lifecycle::resume_milestone(store, milestone_id),
            };
            outcome.map_err(|e| ops_problem(store, e, Some(trigger)))
        })
        .await?;
    tracing::info!(who = "admin", op = trigger, milestone_id = %milestone_id, status = %result.milestone.status.as_str(), "admin: milestone lifecycle");
    Ok(json_response(StatusCode::OK, &result))
}

macro_rules! project_handler {
    ($name:ident, $action:expr) => {
        async fn $name(
            State(state): State<ApiState>,
            headers: HeaderMap,
            Params(id): Params<String>,
            RawQuery(raw): RawQuery,
            body: Body,
        ) -> ApiResult {
            project_action(state, headers, raw, body, id, $action).await
        }
    };
}

macro_rules! milestone_handler {
    ($name:ident, $action:expr) => {
        async fn $name(
            State(state): State<ApiState>,
            headers: HeaderMap,
            Params(id): Params<String>,
            RawQuery(raw): RawQuery,
            body: Body,
        ) -> ApiResult {
            milestone_action(state, headers, raw, body, id, $action).await
        }
    };
}

project_handler!(cancel_project, ProjectAction::Cancel);
project_handler!(pause_project, ProjectAction::Pause);
project_handler!(resume_project, ProjectAction::Resume);
project_handler!(archive_project, ProjectAction::Archive);
project_handler!(unarchive_project, ProjectAction::Unarchive);
milestone_handler!(cancel_milestone, MilestoneAction::Cancel);
milestone_handler!(pause_milestone, MilestoneAction::Pause);
milestone_handler!(resume_milestone, MilestoneAction::Resume);

pub(crate) fn routes() -> axum::Router<ApiState> {
    use axum::routing::post;
    axum::Router::new()
        .route("/api/v1/projects/{id}/cancel", post(cancel_project))
        .route("/api/v1/projects/{id}/pause", post(pause_project))
        .route("/api/v1/projects/{id}/resume", post(resume_project))
        .route("/api/v1/projects/{id}/archive", post(archive_project))
        .route("/api/v1/projects/{id}/unarchive", post(unarchive_project))
        .route("/api/v1/milestones/{id}/cancel", post(cancel_milestone))
        .route("/api/v1/milestones/{id}/pause", post(pause_milestone))
        .route("/api/v1/milestones/{id}/resume", post(resume_milestone))
}
