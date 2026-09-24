//! JSON-RPC 2.0 の芯と MCP 2025-06-18 のメソッド群（ADR-0056 D5: 最小実装）。
//!
//! `initialize` / `notifications/initialized` / `ping` / `tools/list` / `tools/call` /
//! `resources/list` / `resources/read` のディスパッチだけをここに置く。HTTP（`Mcp-Session-Id` の
//! 出し入れ、`Authorization` の読み取り）は `crate::http` にある。

use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use task_core::McpCall;
use time::OffsetDateTime;

use crate::auth::AuthedClient;
use crate::state::McpState;
use crate::tools::{self, ToolErrorCode};

pub const JSONRPC_VERSION: &str = "2.0";
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "celeris-mcp";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: &'static str,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorBody>,
}

#[derive(Debug, Serialize)]
pub struct RpcErrorBody {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RpcResponse {
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: serde_json::Value, code: i64, message: impl Into<String>) -> Self {
        Self::err_with_data(id, code, message, None)
    }

    pub fn err_with_data(
        id: serde_json::Value,
        code: i64,
        message: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(RpcErrorBody {
                code,
                message: message.into(),
                data,
            }),
        }
    }
}

// JSON-RPC の予約コード。
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
// このサーバー固有（JSON-RPC の予約範囲外）。
pub const RATE_LIMITED: i64 = -32000;
pub const NOT_FOUND: i64 = -32001;
pub const REJECTED: i64 = -32002;

fn tool_error_code(kind: ToolErrorCode) -> i64 {
    match kind {
        ToolErrorCode::InvalidParams => INVALID_PARAMS,
        ToolErrorCode::NotFound => NOT_FOUND,
        ToolErrorCode::Rejected => REJECTED,
        ToolErrorCode::Internal => INTERNAL_ERROR,
    }
}

fn error_kind_label(kind: ToolErrorCode) -> &'static str {
    match kind {
        ToolErrorCode::InvalidParams => "invalid_params",
        ToolErrorCode::NotFound => "not_found",
        ToolErrorCode::Rejected => "rejected",
        ToolErrorCode::Internal => "internal",
    }
}

/// `id` が無い（JSON-RPC の通知）か。
pub fn is_notification(req: &RpcRequest) -> bool {
    req.id.is_none()
}

/// メソッドを実行する（`initialize` 以外は呼び出し側がセッションを確認済みという前提）。
/// 戻り値はレスポンス本体（`id` は `req.id` をそのまま使う。通知なら呼び出し側が使わない）。
pub async fn dispatch(
    state: &Arc<McpState>,
    client: &AuthedClient,
    req: &RpcRequest,
    new_session_id: impl FnOnce() -> String,
) -> (RpcResponse, Option<String>) {
    let id = req.id.clone().unwrap_or(serde_json::Value::Null);
    match req.method.as_str() {
        "initialize" => {
            let session_id = new_session_id();
            let result = serde_json::json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {"listChanged": false},
                    "resources": {"listChanged": false}
                },
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            });
            (RpcResponse::ok(id, result), Some(session_id))
        }
        "notifications/initialized" => (RpcResponse::ok(id, serde_json::json!({})), None),
        "ping" => (RpcResponse::ok(id, serde_json::json!({})), None),
        "tools/list" => {
            let items: Vec<_> = tools::all()
                .into_iter()
                .filter(|t| client.has_scope(t.scope))
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": (t.input_schema)(),
                    })
                })
                .collect();
            (
                RpcResponse::ok(id, serde_json::json!({"tools": items})),
                None,
            )
        }
        "tools/call" => (
            call_tool(state, client, &id, req.params.clone()).await,
            None,
        ),
        "resources/list" => match crate::resources::list(state, client).await {
            Ok(items) => (
                RpcResponse::ok(id, serde_json::json!({"resources": items})),
                None,
            ),
            Err(e) => (
                RpcResponse::err(id, tool_error_code(e.code), e.message),
                None,
            ),
        },
        "resources/read" => {
            let uri = req
                .params
                .as_ref()
                .and_then(|p| p.get("uri"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let Some(uri) = uri else {
                return (
                    RpcResponse::err(id, INVALID_PARAMS, "params.uri is required"),
                    None,
                );
            };
            match crate::resources::read(state, client, &uri).await {
                Ok(out) => {
                    let text = serde_json::to_string(&out.value).unwrap_or_default();
                    (
                        RpcResponse::ok(
                            id,
                            serde_json::json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": text}]}),
                        ),
                        None,
                    )
                }
                Err(e) => (
                    RpcResponse::err(id, tool_error_code(e.code), e.message),
                    None,
                ),
            }
        }
        other => (
            RpcResponse::err(id, METHOD_NOT_FOUND, format!("unknown method {other:?}")),
            None,
        ),
    }
}

/// `tools/call`（ADR-0056 D4: スコープ外は `tools/list` に出さない = 呼ばれたら `-32601`。流量制限。
/// `mcp_calls` への記録）。
async fn call_tool(
    state: &Arc<McpState>,
    client: &AuthedClient,
    id: &serde_json::Value,
    params: Option<serde_json::Value>,
) -> RpcResponse {
    let Some(params) = params else {
        return RpcResponse::err(id.clone(), INVALID_PARAMS, "params is required");
    };
    let Some(name) = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        return RpcResponse::err(id.clone(), INVALID_PARAMS, "params.name is required");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let Some(def) = tools::find(&name) else {
        return RpcResponse::err(
            id.clone(),
            METHOD_NOT_FOUND,
            format!("unknown tool {name:?}"),
        );
    };
    if !client.has_scope(def.scope) {
        return RpcResponse::err(
            id.clone(),
            METHOD_NOT_FOUND,
            format!("unknown tool {name:?}"),
        );
    }

    // ADR-0056 D4: クライアントごとの流量制限。
    let retry_after = {
        let mut limiter = state.limiter.lock().unwrap_or_else(|e| e.into_inner());
        limiter
            .check(&client.id, state.rate_limit_per_min, Instant::now())
            .err()
    };
    if let Some(retry_after) = retry_after {
        record_call(state, &client.id, &name, false, Some("rate_limited"), 0).await;
        return RpcResponse::err_with_data(
            id.clone(),
            RATE_LIMITED,
            "rate limit exceeded",
            Some(serde_json::json!({"retry_after": retry_after})),
        );
    }

    let started = Instant::now();
    let result = (def.call)(state, client, arguments).await;
    let latency_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    match result {
        Ok(out) => {
            record_call(state, &client.id, &name, true, None, latency_ms).await;
            let text = serde_json::to_string(&out.value).unwrap_or_default();
            RpcResponse::ok(
                id.clone(),
                serde_json::json!({
                    "content": [{"type": "text", "text": text}],
                    "structuredContent": out.value,
                }),
            )
        }
        Err(e) => {
            record_call(
                state,
                &client.id,
                &name,
                false,
                Some(error_kind_label(e.code)),
                latency_ms,
            )
            .await;
            RpcResponse::err(id.clone(), tool_error_code(e.code), e.message)
        }
    }
}

async fn record_call(
    state: &Arc<McpState>,
    client_id: &str,
    tool: &str,
    ok: bool,
    error_kind: Option<&str>,
    latency_ms: i64,
) {
    let call = McpCall {
        id: ulid::Ulid::new().to_string(),
        client_id: client_id.to_string(),
        tool: tool.to_string(),
        ok,
        error_kind: error_kind.map(str::to_string),
        latency_ms,
        at: OffsetDateTime::now_utc(),
    };
    state
        .blocking(move |store| {
            use task_core::McpCallStore;
            if let Err(e) = store.mcp_call_record(&call) {
                tracing::warn!(error = %e, "celeris-mcp: failed to record mcp_calls row");
            }
        })
        .await;
}
