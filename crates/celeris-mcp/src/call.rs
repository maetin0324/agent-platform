//! ADR-0056 Phase 101: `celerisctl mcp call`（**DB は開かない**。`stdio` と同じ `reqwest::blocking`
//! の HTTP クライアントを再利用する）。
//!
//! `initialize` → `tools/call`（または `--list` なら `tools/list`）を 1 回だけ行い、結果を返す。
//! `celerisctl` 側（`commands/mcp.rs`）は出力の整形と exit code だけを持つ薄い呼び出し元。

use serde_json::Value;

/// `call_tool` / `list_tools` の失敗。`Display` が celerisctl の stderr 文面になる。
#[derive(Debug)]
pub enum CallError {
    /// サーバーに届かなかった（接続不可・タイムアウト等）。
    Transport(String),
    /// HTTP レベルで非 2xx（認証の 401 等）。
    Http { status: u16, body: String },
    /// JSON-RPC のエラー応答（`-32602` の invalid params 等。tool 側の `ToolError` もここに写る）。
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Transport(e) => write!(f, "MCP サーバーに届きませんでした: {e}"),
            CallError::Http { status, body } => {
                write!(f, "MCP サーバーが HTTP {status} を返しました: {body}")
            }
            CallError::Rpc { code, message, data } => match data {
                Some(d) => write!(f, "MCP エラー {code}: {message} ({d})"),
                None => write!(f, "MCP エラー {code}: {message}"),
            },
        }
    }
}

impl std::error::Error for CallError {}

/// `tools/list` の 1 件（`--list` の出力）。
pub struct ToolInfo {
    pub name: String,
    pub description: String,
}

fn build_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

fn post_rpc(
    client: &reqwest::blocking::Client,
    base_url: &str,
    token: Option<&str>,
    session: Option<&str>,
    body: Value,
) -> Result<Value, CallError> {
    let mut req = client
        .post(format!("{base_url}/mcp"))
        .header("content-type", "application/json")
        .json(&body);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    if let Some(sid) = session {
        req = req.header("mcp-session-id", sid);
    }
    let resp = req.send().map_err(|e| CallError::Transport(e.to_string()))?;
    let status = resp.status();
    let session_header = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let text = resp.text().map_err(|e| CallError::Transport(e.to_string()))?;
    if !status.is_success() {
        return Err(CallError::Http {
            status: status.as_u16(),
            body: text,
        });
    }
    let mut value: Value = serde_json::from_str(&text)
        .map_err(|e| CallError::Transport(format!("invalid response body: {e}")))?;
    if let (Some(sid), Value::Object(map)) = (session_header, &mut value) {
        // 呼び出し元が `initialize` の応答からセッション id を拾えるよう、ヘッダの値を
        // 応答オブジェクトに `_session_id` として足しておく（JSON-RPC の枠外の値なので
        // `jsonrpc`/`id`/`result`/`error` とはぶつからない）。
        map.insert("_session_id".to_string(), Value::String(sid));
    }
    Ok(value)
}

fn rpc_error(value: &Value) -> Option<CallError> {
    let error = value.get("error")?;
    Some(CallError::Rpc {
        code: error.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
        message: error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error")
            .to_string(),
        data: error.get("data").cloned(),
    })
}

/// `initialize` を 1 回行い、セッション id を返す。
fn initialize(
    client: &reqwest::blocking::Client,
    base_url: &str,
    token: Option<&str>,
) -> Result<String, CallError> {
    let value = post_rpc(
        client,
        base_url,
        token,
        None,
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
    )?;
    if let Some(e) = rpc_error(&value) {
        return Err(e);
    }
    value
        .get("_session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| CallError::Transport("initialize did not return Mcp-Session-Id".to_string()))
}

/// `initialize` → `tools/list`。名前と description の一覧（`--list`）。
pub fn list_tools(base_url: &str, token: Option<&str>) -> Result<Vec<ToolInfo>, CallError> {
    let client = build_client();
    let session = initialize(&client, base_url, token)?;
    let value = post_rpc(
        &client,
        base_url,
        token,
        Some(&session),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )?;
    if let Some(e) = rpc_error(&value) {
        return Err(e);
    }
    let tools = value["result"]["tools"].as_array().cloned().unwrap_or_default();
    Ok(tools
        .into_iter()
        .map(|t| ToolInfo {
            name: t.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            description: t
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
        .collect())
}

/// `initialize` → `tools/call`。`structuredContent` があればそれを整形 JSON にした文字列、無ければ
/// `content[0].text`（JSON なら整形、そうでなければそのまま）を返す。
pub fn call_tool(
    base_url: &str,
    token: Option<&str>,
    tool: &str,
    arguments: Value,
) -> Result<String, CallError> {
    let client = build_client();
    let session = initialize(&client, base_url, token)?;
    let value = post_rpc(
        &client,
        base_url,
        token,
        Some(&session),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        }),
    )?;
    if let Some(e) = rpc_error(&value) {
        return Err(e);
    }
    let result = value.get("result").cloned().unwrap_or(Value::Null);
    if let Some(structured) = result.get("structuredContent") {
        return Ok(serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string()));
    }
    if let Some(text) = result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
    {
        return Ok(match serde_json::from_str::<Value>(text) {
            Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| text.to_string()),
            Err(_) => text.to_string(),
        });
    }
    Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Arc;

    async fn spawn(
        scopes: Vec<task_core::McpScope>,
    ) -> (SocketAddr, tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<std::io::Result<()>>) {
        let store = Arc::new(task_core::SqliteStore::open_in_memory().expect("open"));
        {
            use task_core::McpClientStore;
            store
                .mcp_client_create(&task_core::McpClient {
                    id: "c1".into(),
                    name: "c1".into(),
                    token_hash: Some(crate::auth::hash_token("secret")),
                    scopes,
                    created_at: time::OffsetDateTime::now_utc(),
                    last_used_at: None,
                    revoked_at: None,
                })
                .expect("create client");
        }
        let state = crate::state::McpState::from_store(store, 60, Vec::new(), Vec::new(), "secretary".to_string(), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(crate::serve(listener, state, crate::config::ListenerAuth::Token, async {
            let _ = stop_rx.await;
        }));
        (addr, stop_tx, handle)
    }

    #[test]
    fn list_tools_returns_names_and_descriptions_filtered_by_scope() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let (addr, stop_tx, handle) = rt.block_on(spawn(vec![task_core::McpScope::TasksRead]));
        let base_url = format!("http://{addr}");

        let tools = list_tools(&base_url, Some("secret")).expect("list_tools");
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"tasks_list"), "{names:?}");
        assert!(names.contains(&"tasks_get"), "{names:?}");
        assert!(!names.contains(&"knowledge_list"), "{names:?}");
        assert!(!tools[0].description.is_empty());

        let _ = stop_tx.send(());
        rt.block_on(handle).expect("join").expect("serve");
    }

    #[test]
    fn call_tool_returns_structured_content_as_pretty_json() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let (addr, stop_tx, handle) = rt.block_on(spawn(vec![task_core::McpScope::TasksRead]));
        let base_url = format!("http://{addr}");

        let out = call_tool(&base_url, Some("secret"), "tasks_list", serde_json::json!({}))
            .expect("call_tool");
        let parsed: Value = serde_json::from_str(&out).expect("pretty json");
        assert_eq!(parsed["items"], serde_json::json!([]));

        let _ = stop_tx.send(());
        rt.block_on(handle).expect("join").expect("serve");
    }

    #[test]
    fn call_tool_without_the_scope_is_a_rpc_error() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let (addr, stop_tx, handle) = rt.block_on(spawn(vec![]));
        let base_url = format!("http://{addr}");

        let err = call_tool(&base_url, Some("secret"), "tasks_list", serde_json::json!({}))
            .expect_err("should fail");
        assert!(matches!(err, CallError::Rpc { code, .. } if code == crate::rpc::METHOD_NOT_FOUND));

        let _ = stop_tx.send(());
        rt.block_on(handle).expect("join").expect("serve");
    }

    #[test]
    fn missing_or_wrong_token_is_a_http_error() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let (addr, stop_tx, handle) = rt.block_on(spawn(vec![task_core::McpScope::TasksRead]));
        let base_url = format!("http://{addr}");

        let err = call_tool(&base_url, Some("wrong"), "tasks_list", serde_json::json!({}))
            .expect_err("should fail");
        assert!(matches!(err, CallError::Http { status: 401, .. }), "{err}");

        let _ = stop_tx.send(());
        rt.block_on(handle).expect("join").expect("serve");
    }

    #[test]
    fn unreachable_server_is_a_transport_error() {
        // `bind` した口を先に閉じる（何も listen していないアドレス）ことで接続不可を作る。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let base_url = format!("http://{addr}");

        let err = call_tool(&base_url, Some("secret"), "tasks_list", serde_json::json!({}))
            .expect_err("should fail");
        assert!(matches!(err, CallError::Transport(_)), "{err}");
    }
}
