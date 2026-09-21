//! `GET /llm/sources`（ADR-0053 D4。API は Phase 65 で用意し、GUI 表示は Phase 66）。
//!
//! task-api は `llm-proxy`（`crates/llm-proxy`）を知らない（ADR-0017 M2 と同じ境界: task-api は
//! `task-worker`/`task-dispatch` に依存しない。celeris がトレイト実装として渡す）。
//! 到達性の probe はネットワーク I/O なので、`ReleaseSource` の同期版ではなく非同期トレイトにする。

use std::sync::Arc;

use axum::extract::{RawQuery, State};

use crate::handlers::{ApiResult, json_response};
use crate::problem::ApiProblem;
use crate::state::ApiState;
use crate::types::LlmSourcesView;

/// celeris が `llm_proxy::ProxyState` を包んで渡す（`docs/llm-source.md`）。
#[async_trait::async_trait]
pub trait LlmSourcesReader: Send + Sync + 'static {
    /// `now` は Unix 秒。
    async fn view(&self, now: i64) -> LlmSourcesView;
}

pub type SharedLlmSourcesReader = Arc<dyn LlmSourcesReader>;

/// `GET /llm/sources`。トークン必須（他の観測系と同じ）。`[llm_proxy]` が無効なら 409。
pub(crate) async fn list(State(state): State<ApiState>, RawQuery(raw): RawQuery) -> ApiResult {
    crate::handlers::no_query(&raw)?;
    let Some(reader) = state.inner.llm_sources.clone() else {
        return Err(ApiProblem::llm_proxy_unavailable());
    };
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let view = reader.view(now).await;
    Ok(json_response(axum::http::StatusCode::OK, &view))
}

pub(crate) fn routes() -> axum::Router<ApiState> {
    use axum::routing::get;
    axum::Router::new().route("/api/v1/llm/sources", get(list))
}
