//! ADR-0068 D5: `GET /tasks/{id}/routing`（読み取り）。
//!
//! なぜその担当（org）・harness・lane・model になったかを、`task_ops::routing_audit` がイベントから
//! 組み立てた run ごとの監査と、タスクの routing の出自（`Task.routing`、捨てた LLM の担当を含む）で返す。
//! 集めるのは決定的（ストアだけ）。LLM は関与しない。

use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use task_core::TaskStore;

use crate::handlers::{ApiResult, Params, json_response, no_query};
use crate::problem::{ApiProblem, ops_problem, store_problem};
use crate::query::parse_task_id;
use crate::state::ApiState;
use crate::types::TaskRoutingView;

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new().route("/api/v1/tasks/{id}/routing", axum::routing::get(routing))
}

/// `GET /tasks/{id}/routing`。知らないタスクは 404、run がまだ無いタスクは `runs: []`。
pub(crate) async fn routing(
    State(state): State<ApiState>,
    Params(id): Params<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let id = parse_task_id(&id)?;
    let view = state
        .blocking(move |store| {
            let task = store
                .get(id)
                .map_err(store_problem)?
                .ok_or_else(|| ApiProblem::task_not_found(id))?;
            let runs = task_ops::routing_audit::task_routing_audit(store, id)
                .map_err(|e| ops_problem(store, e, None))?;
            Ok(TaskRoutingView {
                task_id: id,
                assignee: task.assignee,
                routing: task.routing,
                runs,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}
