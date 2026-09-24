//! ADR-0056 D1: Streamable HTTP の口（`POST /mcp`）。`Mcp-Session-Id` の出し入れと
//! `Authorization` の読み取りはここに閉じる。JSON-RPC のメソッド分岐は `crate::rpc`。
//!
//! - `POST /mcp`: JSON-RPC 1 件を受け、JSON で応答する（SSE 応答は実装しない。`Accept:
//!   text/event-stream` でも JSON を返す。**逸脱として `docs/mcp.md` に明記**）。
//! - `GET /mcp`: サーバー起点のストリーム購読は実装しない。**405**（ADR-0056 D5 の許容範囲）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::auth;
use crate::config::ListenerAuth;
use crate::rpc::{self, RpcRequest, RpcResponse};
use crate::state::McpState;

const SESSION_HEADER: &str = "mcp-session-id";

#[derive(Clone)]
struct HttpState {
    mcp: Arc<McpState>,
    listener_auth: Arc<ListenerAuth>,
}

/// 1 口ぶんの axum ルータ（口ごとに `listener_auth` が違うので、口ごとに作る）。
pub fn router(state: Arc<McpState>, listener_auth: ListenerAuth) -> Router {
    let http_state = HttpState {
        mcp: state,
        listener_auth: Arc::new(listener_auth),
    };
    Router::new()
        .route("/mcp", get(get_mcp).post(post_mcp))
        .with_state(http_state)
}

async fn get_mcp() -> impl IntoResponse {
    StatusCode::METHOD_NOT_ALLOWED
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}

async fn post_mcp(State(state): State<HttpState>, headers: HeaderMap, body: Bytes) -> Response {
    // 1. JSON として読めるか（読めなければ parse error）。
    let value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            let resp = RpcResponse::err(serde_json::Value::Null, rpc::PARSE_ERROR, "invalid JSON");
            return (StatusCode::BAD_REQUEST, Json(resp)).into_response();
        }
    };
    // 2. JSON-RPC の形をしているか（`method` が要る）。
    let req: RpcRequest = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            let resp = RpcResponse::err(
                serde_json::Value::Null,
                rpc::INVALID_REQUEST,
                format!("invalid JSON-RPC request: {e}"),
            );
            return (StatusCode::BAD_REQUEST, Json(resp)).into_response();
        }
    };

    // 3. 認証（口の方式にしたがう。トークンの値はここでしか扱わない）。
    let bearer_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let listener_auth = (*state.listener_auth).clone();
    let mcp = Arc::clone(&state.mcp);
    let authed = mcp
        .blocking(move |store| {
            let bearer = auth::bearer_token(bearer_header.as_deref());
            auth::authenticate(
                store,
                &listener_auth,
                bearer,
                time::OffsetDateTime::now_utc(),
            )
        })
        .await;
    let client = match authed {
        Ok(c) => c,
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))],
                Json(serde_json::json!({"error": "unauthorized"})),
            )
                .into_response();
        }
    };

    // 4. セッション（`initialize` 以外は `Mcp-Session-Id` が要る。ADR-0056 D1）。
    if req.method != "initialize" {
        let session_id = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok());
        match session_id {
            None => {
                return json_error(StatusCode::BAD_REQUEST, "Mcp-Session-Id header is required");
            }
            Some(sid) => match state.mcp.session_client(sid) {
                Some(owner) if owner == client.id => {}
                Some(_) => {
                    return json_error(StatusCode::BAD_REQUEST, "session belongs to another client");
                }
                None => return json_error(StatusCode::NOT_FOUND, "unknown or expired session"),
            },
        }
    }

    // 5. 通知（`id` 無し）は応答本体を持たない（202）。
    if rpc::is_notification(&req) {
        let _ = rpc::dispatch(&state.mcp, &client, &req, String::new).await;
        return StatusCode::ACCEPTED.into_response();
    }

    let mcp_for_session = Arc::clone(&state.mcp);
    let client_id_for_session = client.id.clone();
    let (resp, new_session) = rpc::dispatch(&state.mcp, &client, &req, move || {
        mcp_for_session.create_session(&client_id_for_session)
    })
    .await;
    let mut response = (StatusCode::OK, Json(resp)).into_response();
    if let Some(session_id) = new_session
        && let Ok(value) = HeaderValue::from_str(&session_id)
    {
        response.headers_mut().insert(SESSION_HEADER, value);
    }
    response
}
