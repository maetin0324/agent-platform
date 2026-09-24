//! ADR-0072（Phase E2）: `POST /tasks/{id}/execution-plan`（origin human）、
//! `GET /tasks/{id}/execution-plan`。
//!
//! - `POST` は**管理系**（`token_file` 未設定でも 401）: 計画を作るのは人の判断なので、
//!   `POST /projects` / `POST /projects/{id}/repos` と同じ規律にする。
//! - 検証・採用そのものは `task_ops::execution::adopt_plan`（D14）。ハンドラは HTTP への写像だけ。
//! - 404（タスクが無い）、422（D14 の検証エラー）、409（既に `active` な計画がある。E2 は新規のみ、
//!   replan は E4）。

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use task_core::{ExecutionLimits, TaskStore};
use time::OffsetDateTime;

use crate::handlers::{ApiResult, Params, json_response, no_query, read_json};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, ops_problem};
use crate::query::parse_task_id;
use crate::state::ApiState;
use crate::types::ExecutionPlanView;

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new().route(
        "/api/v1/tasks/{id}/execution-plan",
        axum::routing::get(get_execution_plan).post(post_execution_plan),
    )
}

fn no_active_plan(id: &str) -> ApiProblem {
    ApiProblem::new(
        StatusCode::NOT_FOUND,
        "execution_plan_not_found",
        format!("task {id} has no execution plan"),
    )
}

async fn get_execution_plan(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let task_id = parse_task_id(&id)?;
    let view = state
        .blocking(move |store| {
            match task_ops::execution::active_plan(store, task_id)
                .map_err(|e| ops_problem(store, e, Some("execution_plan_get")))?
            {
                Some(view) => Ok(ExecutionPlanView::new(view.plan, view.work_units)),
                None => Err(no_active_plan(&task_id.to_string())),
            }
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

async fn post_execution_plan(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let task_id = parse_task_id(&id)?;
    let spec: task_core::ExecutionPlanSpec = read_json(body, false).await?;
    let view = state
        .blocking(move |store| {
            let plan = task_ops::execution::adopt_plan(
                store,
                task_id,
                spec,
                task_core::PlanOrigin::Human,
                None,
                ExecutionLimits::default(),
                OffsetDateTime::now_utc(),
            )
            .map_err(|e| ops_problem(store, e, Some("execution_plan_adopt")))?;
            let work_units = store
                .work_units_for(task_id)
                .map_err(crate::problem::store_problem)?;
            Ok(ExecutionPlanView::new(plan, work_units))
        })
        .await?;
    tracing::info!(who = "admin", op = "execution_plan_adopt", task_id = %task_id, plan_id = %view.id, work_units = view.work_units.len(), "admin: execution plan adopted");
    Ok(json_response(StatusCode::CREATED, &view))
}
