//! ADR-0056 D4（Phase 78）: `GET /mcp/clients` と `GET /mcp/calls?client=`。
//!
//! task-api は `celeris-mcp` を知らない（`crates/celeris-mcp` は `task-dispatch`/`task-worker` を
//! 知らない側にいるのと同じ境界。ADR-0017 M2）。読むのは `task_core::McpClientStore` /
//! `task_core::McpCallStore`（migration 0024）だけで、判断は無い（DESIGN 原則 1）。
//! トークンの値は `McpClient` に元から無い（`Serialize` が `token_hash` を出さない）。

use axum::extract::{RawQuery, State};

use crate::handlers::{ApiResult, json_response, no_query};
use crate::problem::store_problem;
use crate::query::QueryParams;
use crate::state::ApiState;

/// 1 回に読む `mcp_calls` の件数（ADR-0056 D4: 「直近 100 件」）。
const CALLS_LIMIT: usize = 100;

#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct McpClientsView {
    pub items: Vec<task_core::McpClient>,
}

#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct McpCallsView {
    pub items: Vec<task_core::McpCall>,
}

async fn list_clients(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    no_query(&raw)?;
    let items = state
        .blocking(|store| task_core::McpClientStore::mcp_client_list(store).map_err(store_problem))
        .await?;
    Ok(json_response(
        axum::http::StatusCode::OK,
        &McpClientsView { items },
    ))
}

async fn list_calls(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    let query = QueryParams::parse(raw.as_deref(), &["client"])?;
    let client = query.single("client")?.map(str::to_string);
    let items = state
        .blocking(move |store| {
            task_core::McpCallStore::mcp_calls_list(store, client.as_deref(), CALLS_LIMIT)
                .map_err(store_problem)
        })
        .await?;
    Ok(json_response(
        axum::http::StatusCode::OK,
        &McpCallsView { items },
    ))
}

pub(crate) fn routes() -> axum::Router<ApiState> {
    use axum::routing::get;
    axum::Router::new()
        .route("/api/v1/mcp/clients", get(list_clients))
        .route("/api/v1/mcp/calls", get(list_calls))
}
