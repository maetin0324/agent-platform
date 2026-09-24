//! ADR-0056 D1: `celerisctl mcp stdio`（stdio ↔ 手元の HTTP の橋）。中身は同じサーバーへの
//! 普通の HTTP 要求（`POST <base_url>/mcp`）で、`Mcp-Session-Id` を橋の中で覚えて次の行に載せる。
//!
//! 1 行 = 1 つの JSON-RPC メッセージ（newline-delimited JSON）。標準入力から読み、応答（通知は
//! 202 で応答本体が無いので何も書かない）を標準出力へ 1 行ずつ書く。

use std::io::{BufRead, Write};

/// 橋を 1 本、EOF まで動かす（`reader` が閉じたら `Ok(())`）。
pub fn run(
    base_url: &str,
    token: Option<&str>,
    mut reader: impl BufRead,
    mut writer: impl Write,
) -> Result<(), String> {
    let client = reqwest::blocking::Client::new();
    let mut session_id: Option<String> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("stdin を読めませんでした: {e}"))?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut req = client
            .post(format!("{base_url}/mcp"))
            .header("content-type", "application/json")
            .body(trimmed.to_string());
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(sid) = &session_id {
            req = req.header("mcp-session-id", sid.clone());
        }
        let resp = req
            .send()
            .map_err(|e| format!("MCP サーバーに届きませんでした: {e}"))?;
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            session_id = Some(sid.to_string());
        }
        if resp.status() == reqwest::StatusCode::ACCEPTED {
            // 通知（`id` 無し）は応答本体が無い。
            continue;
        }
        let body = resp
            .text()
            .map_err(|e| format!("応答を読めませんでした: {e}"))?;
        writeln!(writer, "{body}").map_err(|e| format!("stdout に書けませんでした: {e}"))?;
        writer
            .flush()
            .map_err(|e| format!("stdout を flush できませんでした: {e}"))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::TcpListener as StdTcpListener;
    use std::sync::Arc;

    fn free_addr() -> std::net::SocketAddr {
        let l = StdTcpListener::bind("127.0.0.1:0").expect("bind");
        l.local_addr().expect("addr")
    }

    #[test]
    fn a_round_trip_through_a_real_server_returns_one_line_per_call_and_reuses_the_session() {
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let addr = free_addr();
        let store = Arc::new(task_core::SqliteStore::open_in_memory().expect("open"));
        {
            use task_core::McpClientStore;
            store
                .mcp_client_create(&task_core::McpClient {
                    id: "c1".into(),
                    name: "c1".into(),
                    token_hash: Some(crate::auth::hash_token("secret")),
                    scopes: vec![],
                    created_at: time::OffsetDateTime::now_utc(),
                    last_used_at: None,
                    revoked_at: None,
                })
                .expect("create client");
        }
        let state = crate::state::McpState::from_store(
            store,
            60,
            Vec::new(),
            Vec::new(),
            "secretary".to_string(),
            None,
        );
        let listener = rt
            .block_on(tokio::net::TcpListener::bind(addr))
            .expect("bind tokio");
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = rt.spawn(crate::serve(
            listener,
            state,
            crate::config::ListenerAuth::Token,
            async {
                let _ = stop_rx.await;
            },
        ));

        let base_url = format!("http://{addr}");
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n\
             {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\",\"params\":{}}\n";
        let mut out = Vec::new();
        run(&base_url, Some("secret"), Cursor::new(input), &mut out).expect("bridge");
        let text = String::from_utf8(out).expect("utf8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        let first: serde_json::Value = serde_json::from_str(lines[0]).expect("json 1");
        assert_eq!(first["result"]["protocolVersion"], "2025-06-18");
        let second: serde_json::Value = serde_json::from_str(lines[1]).expect("json 2");
        assert_eq!(second["result"], serde_json::json!({}));

        let _ = stop_tx.send(());
        rt.block_on(server).expect("join").expect("serve");
    }
}
