//! ADR-0072（Phase E2）: `POST /tasks/{id}/execution-plan`（origin human）、
//! `GET /tasks/{id}/execution-plan`。
//!
//! - `POST` は**管理系**（`token_file` 未設定でも 401）: 計画を作るのは人の判断なので、
//!   `POST /projects` / `POST /projects/{id}/repos` と同じ規律にする。
//! - 検証・採用そのものは `task_ops::execution::adopt_plan`（D14）。ハンドラは HTTP への写像だけ。
//! - 404（タスクが無い）、422（D14 の検証エラー）、409（既に `active` な計画がある。E2 は新規のみ、
//!   replan は E4）。
//!
//! ADR-0072 D19（Phase E5）: `GET /tasks/{id}/execution`（計画・WU 一覧・run 一覧・metrics・
//! ExecutionPhase を 1 つにまとめた深掘りビュー）と `GET /metrics/execution`（期間で集計した gate の
//! 判定分布・completion rate・continuation/repair/replan 頻度。集計そのものは `crate::stats`）。

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use task_core::{ExecutionLimits, TaskStore};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::handlers::{ApiResult, Params, json_response, no_query, read_json};
use crate::middleware::require_admin;
use crate::problem::{ApiProblem, ops_problem, store_problem};
use crate::query::{QueryParams, parse_task_id};
use crate::state::ApiState;
use crate::types::{ExecutionPlanView, TaskExecutionView};

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new()
        .route(
            "/api/v1/tasks/{id}/execution-plan",
            axum::routing::get(get_execution_plan).post(post_execution_plan),
        )
        .route(
            "/api/v1/tasks/{id}/execution",
            axum::routing::get(get_task_execution),
        )
        .route(
            "/api/v1/metrics/execution",
            axum::routing::get(get_execution_metrics),
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
                Some(view) => {
                    // ADR-0072 D17（Phase E4）: 版の履歴（`versions`）も添える（監査用）。
                    let versions = store
                        .execution_plan_list(task_id)
                        .map_err(crate::problem::store_problem)?;
                    Ok(ExecutionPlanView::new(view.plan, view.work_units, versions))
                }
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
            let versions = store
                .execution_plan_list(task_id)
                .map_err(crate::problem::store_problem)?;
            Ok(ExecutionPlanView::new(plan, work_units, versions))
        })
        .await?;
    tracing::info!(who = "admin", op = "execution_plan_adopt", task_id = %task_id, plan_id = %view.id, work_units = view.work_units.len(), "admin: execution plan adopted");
    Ok(json_response(StatusCode::CREATED, &view))
}

/// ADR-0072 D19（Phase E5）: `GET /tasks/{id}/execution`。`TaskDetail.execution`（D20 の要約）と
/// 同じ材料（`task_ops::view::task_detail`）を使い、計画は `ExecutionPlanView`（全文の
/// `WorkUnitSpec` 付き）で、run はファイルの有無も添えて返す。
async fn get_task_execution(
    axum::extract::State(state): axum::extract::State<ApiState>,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let task_id = parse_task_id(&id)?;
    let ctx = state.inner.view.clone();
    let view = state
        .blocking(move |store| {
            let mut detail =
                task_ops::view::task_detail(store, task_id, &ctx, OffsetDateTime::now_utc())
                    .map_err(|e| ops_problem(store, e, Some("task_execution_get")))?;
            let task = detail.task.clone();
            for run in &mut detail.runs {
                run.files = Some(crate::files::run_files(
                    &task,
                    &ctx.workspace_root,
                    &run.run_id,
                ));
            }
            let plan = match task_ops::execution::active_plan(store, task_id)
                .map_err(|e| ops_problem(store, e, Some("task_execution_get")))?
            {
                Some(pv) => {
                    let versions = store.execution_plan_list(task_id).map_err(store_problem)?;
                    Some(ExecutionPlanView::new(pv.plan, pv.work_units, versions))
                }
                None => None,
            };
            let (gate, phase, metrics) = match detail.execution {
                Some(e) => (e.gate, e.phase, e.metrics),
                None => (
                    None,
                    None,
                    task_core::summarize_execution_metrics(&task, &[]),
                ),
            };
            Ok(TaskExecutionView {
                gate,
                phase,
                plan,
                runs: detail.runs,
                metrics,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

fn bad_group_by(group_by: &str) -> ApiProblem {
    ApiProblem::bad_request(format!(
        "`group_by` must be one of {} (got {group_by:?})",
        crate::stats::EXECUTION_METRICS_GROUP_BY.join("|")
    ))
}

/// ADR-0072 D19（Phase E5）: `GET /metrics/execution?since=&group_by=gate_mode|genre|assignee|lane`。
/// 集計は `crate::stats::execution_metrics_summary` が索引行から行い、索引にない情報だけ
/// 該当タスクの events で補完する。
async fn get_execution_metrics(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    let query = QueryParams::parse(raw.as_deref(), &["since", "group_by"])?;
    let since = match query.single("since")? {
        Some(s) if !s.trim().is_empty() => Some(
            OffsetDateTime::parse(s.trim(), &Rfc3339)
                .map_err(|_| ApiProblem::bad_request("`since` must be an RFC 3339 timestamp"))?,
        ),
        _ => None,
    };
    let group_by = query.single("group_by")?.unwrap_or("gate_mode").to_string();
    if !crate::stats::EXECUTION_METRICS_GROUP_BY.contains(&group_by.as_str()) {
        return Err(bad_group_by(&group_by));
    }
    let summary = state
        .blocking(move |store| {
            crate::stats::execution_metrics_summary(store, since, &group_by).map_err(store_problem)
        })
        .await?;
    Ok(json_response(StatusCode::OK, &summary))
}
