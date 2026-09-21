//! `GET /mcp/clients` / `GET /mcp/calls?client=`（ADR-0056 D4。Phase 78）。
//!
//! 見るもの: トークン必須、クライアント一覧にトークンの値が出ないこと、`client=` で呼び出しログを絞れること。

mod common;

use common::*;
use task_core::{McpCall, McpCallStore, McpClient, McpClientStore, McpScope};
use time::OffsetDateTime;

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(
        path,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn env() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    })
}

fn seed_client(env: &TestEnv, id: &str) {
    env.store
        .mcp_client_create(&McpClient {
            id: id.to_string(),
            name: id.to_string(),
            token_hash: Some(format!("deadbeef-{id}")),
            scopes: vec![McpScope::KnowledgeRead],
            created_at: OffsetDateTime::now_utc(),
            last_used_at: None,
            revoked_at: None,
        })
        .expect("create client");
}

#[tokio::test]
async fn lists_clients_without_the_token_value() {
    let env = env();
    seed_client(&env, "chatgpt");
    let app = env.router();
    let resp = send(&app, g("/api/v1/mcp/clients")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "chatgpt");
    assert_eq!(items[0]["scopes"][0], "knowledge:read");
    assert!(items[0].get("token_hash").is_none(), "{body}");
    assert!(!resp.text().contains("deadbeef"), "token leaked");
}

#[tokio::test]
async fn lists_calls_optionally_filtered_by_client() {
    let env = env();
    seed_client(&env, "a");
    seed_client(&env, "b");
    for (i, client_id) in ["a", "b", "a"].iter().enumerate() {
        env.store
            .mcp_call_record(&McpCall {
                id: format!("call-{i}"),
                client_id: client_id.to_string(),
                tool: "knowledge_search".to_string(),
                ok: true,
                error_kind: None,
                latency_ms: 5,
                at: OffsetDateTime::now_utc() + time::Duration::seconds(i as i64),
            })
            .expect("record");
    }
    let app = env.router();

    let resp = send(&app, g("/api/v1/mcp/calls")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(resp.json()["items"].as_array().expect("items").len(), 3);

    let resp = send(&app, g("/api/v1/mcp/calls?client=a")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let items = resp.json()["items"].as_array().expect("items").clone();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|c| c["client_id"] == "a"));
}

#[tokio::test]
async fn requires_a_bearer_token() {
    let env = env();
    let app = env.router();
    let resp = send(
        &app,
        axum::http::Request::get("/api/v1/mcp/clients")
            .header("host", "127.0.0.1:7710")
            .body(axum::body::Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401, "{}", resp.text());
}
