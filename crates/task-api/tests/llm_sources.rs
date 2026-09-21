//! `GET /llm/sources`（ADR-0053 D4。Phase 65: API と型だけ。GUI 表示は Phase 66）。
//!
//! 見るもの: トークン必須、`llm_sources` 未設定は 409 `llm_proxy_unavailable`、設定済みなら
//! `LlmSourcesReader::view` が返した値そのままが JSON になること。

mod common;

use std::sync::Arc;

use common::*;
use task_api::{LlmSourceView, LlmSourcesReader, LlmSourcesView};

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(
        path,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

struct FakeReader;

#[async_trait::async_trait]
impl LlmSourcesReader for FakeReader {
    async fn view(&self, _now: i64) -> LlmSourcesView {
        LlmSourcesView {
            sources: vec![LlmSourceView {
                id: "openai-compatible:qwen".to_string(),
                kind: "openai-compatible".to_string(),
                enabled: true,
                reachable: Some(true),
                accounts: vec![],
                last_hour_requests: 3,
                last_hour_prompt_tokens: 40,
                last_hour_completion_tokens: 10,
            }],
        }
    }
}

fn env_with_llm_sources() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        llm_sources: Some(Arc::new(FakeReader)),
        ..Default::default()
    })
}

fn env_without_llm_sources() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    })
}

#[tokio::test]
async fn returns_409_when_llm_proxy_is_not_configured() {
    let env = env_without_llm_sources();
    let app = env.router();
    let resp = send(&app, g("/api/v1/llm/sources")).await;
    assert_eq!(resp.status.as_u16(), 409, "{}", resp.text());
    assert_eq!(resp.json()["code"], "llm_proxy_unavailable");
}

#[tokio::test]
async fn returns_the_readers_view_when_configured() {
    let env = env_with_llm_sources();
    let app = env.router();
    let resp = send(&app, g("/api/v1/llm/sources")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["sources"][0]["id"], "openai-compatible:qwen");
    assert_eq!(body["sources"][0]["reachable"], true);
    assert_eq!(body["sources"][0]["last_hour_requests"], 3);
}

#[tokio::test]
async fn requires_a_bearer_token() {
    let env = env_with_llm_sources();
    let app = env.router();
    let resp = send(&app, axum::http::Request::get("/api/v1/llm/sources").header("host", "127.0.0.1:7710").body(axum::body::Body::empty()).expect("request")).await;
    assert_eq!(resp.status.as_u16(), 401, "{}", resp.text());
}
