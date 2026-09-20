//! `POST /projects/{id}/plan`（ADR-0033 D4 追記。GUI 監査対応 Phase 29）: 分解を起こす。
//!
//! GUI の「この方針で進める」の入口。人が「それで進めて」と言ったときに、案件の依頼・途中目標・
//! 人の一言・秘書との直近のやり取りをまとめた `kind = plan` のタスクを 1 件作る（`task_ops::project_plan`）。
//! **管理系**（`token_file` 未設定でも 401）: `POST /projects` や `POST /org/{id}/messages` と同じ規律
//! （人格を持つノードに仕事を起こす経路）。応答は 202 `{task_id}`（run を待たない）。

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{MilestoneId, TaskId, TaskStore};
use time::OffsetDateTime;

use crate::handlers::{ApiResult, Params, json_response, no_query, parse_project_id, read_json};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, ops_problem, store_problem};
use crate::state::ApiState;

/// `POST /projects/{id}/plan` の要求本文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectPlanBody {
    /// 分解の対象にする途中目標。省略すると `approved` / `in_progress` のものを文脈として渡すだけで、
    /// 特定の 1 件をこのタスクに紐づけない。
    #[serde(default)]
    pub milestone_id: Option<MilestoneId>,
    /// 人の一言（任意）。
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /projects/{id}/plan` の応答（202）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectPlanAccepted {
    pub task_id: TaskId,
}

pub(crate) async fn create_project_plan(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let project_id = parse_project_id(&id)?;
    let post: ProjectPlanBody = read_json(body, true).await?;
    let roles = state.inner.roles.clone();
    let genres = state.inner.genres.clone();
    let started = state
        .blocking(move |store| {
            let Some(project) = store.project_get(project_id).map_err(store_problem)? else {
                return Err(ApiProblem::project_not_found(&project_id.to_string()));
            };
            task_ops::project_plan::start(
                store,
                &project,
                post.milestone_id,
                post.note.as_deref(),
                &roles,
                &genres,
                OffsetDateTime::now_utc(),
            )
            .map_err(|e| ops_problem(store, e, None))
        })
        .await?;
    tracing::info!(
        who = "admin",
        op = "project_plan",
        project_id = %project_id,
        task_id = %started.task.id,
        "admin: decomposition started for a project"
    );
    Ok(json_response(
        StatusCode::ACCEPTED,
        &ProjectPlanAccepted {
            task_id: started.task.id,
        },
    ))
}

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new().route(
        "/api/v1/projects/{id}/plan",
        axum::routing::post(create_project_plan),
    )
}
