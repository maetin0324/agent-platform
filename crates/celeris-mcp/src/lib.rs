//! `celeris-mcp`（ADR-0056 D1/D2/D4/D5。Phase 78）: 外部エージェントが Celeris を操作する MCP
//! サーバー。JSON-RPC 2.0 + MCP 2025-06-18 Streamable HTTP の最小実装。
//!
//! `task-api` と同じく `task-dispatch` / `task-worker` を知らない（ADR-0017 M2）。必要な操作は
//! `task-ops` / `task-core` の既存関数を呼ぶだけで、ディスパッチャやストアに LLM 呼び出しは無い
//! （DESIGN 原則 1）。

pub mod auth;
pub mod config;
mod http;
pub mod ratelimit;
pub mod resources;
pub mod rpc;
pub mod state;
pub mod stdio;
pub mod tools;

pub use config::{ListenerAuth, McpConfig, McpConfigError, McpListenerConfig, ResolvedListener};
pub use http::router;
pub use state::{McpState, McpStateError};

use std::sync::Arc;

/// `listen` に bind して動かす（`llm_proxy::serve` と同じ形）。
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<McpState>,
    auth: ListenerAuth,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let app = router(state, auth);
    axum::serve(listener, app).with_graceful_shutdown(shutdown).await
}
