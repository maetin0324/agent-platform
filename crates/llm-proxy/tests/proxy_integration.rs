//! 統合テスト（ADR-0053 D1。Phase 65）: **偽の上流**（`127.0.0.1:0` の実サーバ）に対して
//! `llm-proxy` の axum サーバ（同じく `127.0.0.1:0`）を実際に HTTP で叩く。外部ネットワークには出ない。
//!
//! フィクスチャの資格情報は明らかな偽値（`fake-...`）を使い、値そのものをアサーションに出さない
//! （出すのは「含まれないこと」の否定チェックだけ）。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use axum::extract::State as AxumState;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use llm_proxy::config::{
    ClaudeOauthConfig, CodexOauthConfig, LlmProxyConfig, OpenAiCompatibleConfig,
};
use llm_proxy::{ProxyState, router};
use serde_json::{Value, json};
use task_core::SharedRole;
use task_dispatch::accounts::AccountBook;
use tokio::net::TcpListener;

// ---------------------------------------------------------------------------
// テストの下ごしらえ
// ---------------------------------------------------------------------------

async fn spawn(app: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, handle)
}

fn write_claude_credentials(
    dir: &std::path::Path,
    account_id: &str,
    access_token: &str,
    refresh_token: &str,
    expires_at_ms: i64,
) {
    let account_dir = dir.join(account_id);
    std::fs::create_dir_all(&account_dir).expect("mkdir");
    let value = json!({
        "claudeAiOauth": {
            "accessToken": access_token,
            "refreshToken": refresh_token,
            "expiresAt": expires_at_ms,
            "scopes": ["user:inference"],
            "subscriptionType": "max",
        }
    });
    std::fs::write(
        account_dir.join(".credentials.json"),
        serde_json::to_vec_pretty(&value).expect("json"),
    )
    .expect("write");
}

fn write_codex_credentials(
    dir: &std::path::Path,
    account_id: &str,
    access_token: &str,
    refresh_token: &str,
) {
    let account_dir = dir.join(account_id);
    std::fs::create_dir_all(&account_dir).expect("mkdir");
    let value = json!({
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": "fake-id-token",
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": format!("chatgpt-{account_id}"),
        },
        "last_refresh": "2026-09-21T00:00:00Z",
    });
    std::fs::write(
        account_dir.join("auth.json"),
        serde_json::to_vec_pretty(&value).expect("json"),
    )
    .expect("write");
}

fn far_future_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp() + 3600) * 1000
}

fn make_db(tmp: &std::path::Path) -> std::path::PathBuf {
    let path = tmp.join("celeris.sqlite3");
    let conn = rusqlite::Connection::open(&path).expect("open db");
    conn.execute_batch(include_str!(
        "../../task-core/migrations/0022_llm_proxy_requests.sql"
    ))
    .expect("create table");
    path
}

fn base_config() -> LlmProxyConfig {
    LlmProxyConfig {
        prefer_free: false,
        ..LlmProxyConfig::default()
    }
}

async fn get_json(
    client: &reqwest::Client,
    addr: SocketAddr,
    path: &str,
    token: Option<&str>,
) -> reqwest::Response {
    let mut req = client.get(format!("http://{addr}{path}"));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    req.send().await.expect("send")
}

async fn post_chat(
    client: &reqwest::Client,
    addr: SocketAddr,
    body: &Value,
    token: Option<&str>,
) -> reqwest::Response {
    let mut req = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(body);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    req.send().await.expect("send")
}

// ---------------------------------------------------------------------------
// 偽の Anthropic 上流
// ---------------------------------------------------------------------------

#[derive(Default)]
struct AnthropicFake {
    captured: StdMutex<Vec<Value>>,
}

fn sse_body(events: &[(&str, Value)]) -> String {
    events
        .iter()
        .map(|(e, d)| format!("event: {e}\ndata: {d}\n\n"))
        .collect()
}

async fn anthropic_messages(
    AxumState(fake): AxumState<Arc<AnthropicFake>>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    fake.captured.lock().expect("lock").push(body.clone());

    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let has_tools = body
        .get("tools")
        .and_then(|t| t.as_array())
        .is_some_and(|a| !a.is_empty());
    let has_tool_result = body["messages"]
        .as_array()
        .map(|ms| {
            ms.iter().any(|m| {
                m["content"]
                    .as_array()
                    .map(|c| c.iter().any(|b| b["type"] == "tool_result"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if stream {
        let text = sse_body(&[
            (
                "message_start",
                json!({"message": {"id": "msg_stream_1", "usage": {"input_tokens": 5}}}),
            ),
            (
                "content_block_start",
                json!({"index": 0, "content_block": {"type": "text"}}),
            ),
            (
                "content_block_delta",
                json!({"index": 0, "delta": {"type": "text_delta", "text": "Hello"}}),
            ),
            (
                "content_block_delta",
                json!({"index": 0, "delta": {"type": "text_delta", "text": " world"}}),
            ),
            ("content_block_stop", json!({"index": 0})),
            (
                "message_delta",
                json!({"delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
            ),
            ("message_stop", json!({})),
        ]);
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            text,
        )
            .into_response();
    }

    if has_tool_result {
        return Json(json!({
            "id": "msg_final",
            "content": [{"type": "text", "text": "The weather is sunny."}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "completion_tokens": 4, "output_tokens": 4}
        }))
        .into_response();
    }

    if has_tools {
        return Json(json!({
            "id": "msg_tool",
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Tsukuba"}}],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 8, "output_tokens": 2}
        }))
        .into_response();
    }

    Json(json!({
        "id": "msg_1",
        "content": [{"type": "text", "text": "Hello world"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 5, "output_tokens": 3}
    }))
    .into_response()
}

async fn anthropic_refresh(Json(_body): Json<Value>) -> axum::response::Response {
    Json(json!({"access_token": "fresh-access", "refresh_token": "fresh-refresh", "expires_in": 3600})).into_response()
}

fn anthropic_router(fake: Arc<AnthropicFake>) -> Router {
    Router::new()
        .route("/v1/messages", post(anthropic_messages))
        .route("/oauth/token", post(anthropic_refresh))
        .with_state(fake)
}

fn claude_source(addr: SocketAddr, accounts_dir: std::path::PathBuf) -> ClaudeOauthConfig {
    ClaudeOauthConfig {
        accounts_dir,
        base_url: format!("http://{addr}"),
        token_url: format!("http://{addr}/oauth/token"),
        client_id: "test-client".to_string(),
        enabled: true,
    }
}

fn chat_request(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "stream": stream,
        "messages": [
            {"role": "system", "content": "You are terse."},
            {"role": "user", "content": "hi"},
        ]
    })
}

// ---------------------------------------------------------------------------
// Claude: 非 stream の要求/応答
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claude_non_stream_round_trip() {
    let fake = Arc::new(AnthropicFake::default());
    let (upstream_addr, _h1) = spawn(anthropic_router(fake.clone())).await;

    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "fake-access-a",
        "fake-refresh-a",
        far_future_ms(),
    );

    let mut config = base_config();
    config.sources.claude_oauth = Some(claude_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));

    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;

    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("claude/standard", false), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-celeris-source")
            .and_then(|v| v.to_str().ok()),
        Some("claude-oauth")
    );
    assert_eq!(
        resp.headers()
            .get("x-celeris-account")
            .and_then(|v| v.to_str().ok()),
        Some("acct-a")
    );
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["choices"][0]["message"]["content"], "Hello world");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(body["model"], "claude/standard");

    // system メッセージが `system` フィールドに分離されていること。
    let captured = fake.captured.lock().expect("lock");
    assert_eq!(captured[0]["system"], "You are terse.");
    // ADR-0069 Phase 118 D2: default_claude_models の standard は claude-opus-5-5（実測 ID）。
    assert_eq!(captured[0]["model"], "claude-opus-5-5");
}

// ---------------------------------------------------------------------------
// Claude: stream の SSE
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claude_stream_round_trip() {
    let fake = Arc::new(AnthropicFake::default());
    let (upstream_addr, _h1) = spawn(anthropic_router(fake.clone())).await;

    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "fake-access-a",
        "fake-refresh-a",
        far_future_ms(),
    );

    let mut config = base_config();
    config.sources.claude_oauth = Some(claude_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));

    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;

    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("claude/standard", true), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let text = resp.text().await.expect("text");
    assert!(text.contains("\"content\":\"Hello\""), "{text}");
    assert!(text.contains("\"content\":\" world\""), "{text}");
    assert!(text.contains("\"finish_reason\":\"stop\""), "{text}");
    assert!(text.contains("\"prompt_tokens\":5"), "{text}");
    assert!(text.contains("\"completion_tokens\":3"), "{text}");
    assert!(text.ends_with("data: [DONE]\n\n"), "{text}");
}

// ---------------------------------------------------------------------------
// Claude: tool call round trip（要求・応答の両方向）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claude_tool_call_round_trip() {
    let fake = Arc::new(AnthropicFake::default());
    let (upstream_addr, _h1) = spawn(anthropic_router(fake.clone())).await;

    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "fake-access-a",
        "fake-refresh-a",
        far_future_ms(),
    );

    let mut config = base_config();
    config.sources.claude_oauth = Some(claude_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;
    let client = reqwest::Client::new();

    let tools = json!([{
        "type": "function",
        "function": {"name": "get_weather", "description": "look up weather", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}
    }]);
    let first = json!({
        "model": "claude/standard",
        "stream": false,
        "tools": tools,
        "messages": [{"role": "user", "content": "weather in Tsukuba?"}]
    });
    let resp = post_chat(&client, addr, &first, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    let call = &body["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(call["function"]["name"], "get_weather");
    assert!(
        call["function"]["arguments"]
            .as_str()
            .expect("args")
            .contains("Tsukuba")
    );
    let call_id = call["id"].as_str().expect("id").to_string();

    let second = json!({
        "model": "claude/standard",
        "stream": false,
        "tools": tools,
        "messages": [
            {"role": "user", "content": "weather in Tsukuba?"},
            {"role": "assistant", "content": null, "tool_calls": [call.clone()]},
            {"role": "tool", "tool_call_id": call_id, "content": "sunny"},
        ]
    });
    let resp2 = post_chat(&client, addr, &second, None).await;
    assert_eq!(resp2.status(), StatusCode::OK);
    let body2: Value = resp2.json().await.expect("json");
    assert_eq!(
        body2["choices"][0]["message"]["content"],
        "The weather is sunny."
    );
    assert_eq!(body2["choices"][0]["finish_reason"], "stop");

    // 上流が受け取った 2 回目の要求に tool_use / tool_result が正しく写っていること。
    let captured = fake.captured.lock().expect("lock");
    let second_sent = &captured[1];
    let assistant_msg = second_sent["messages"][1]["content"][0].clone();
    assert_eq!(assistant_msg["type"], "tool_use");
    assert_eq!(assistant_msg["name"], "get_weather");
    let tool_result_msg = second_sent["messages"][2]["content"][0].clone();
    assert_eq!(tool_result_msg["type"], "tool_result");
    assert_eq!(tool_result_msg["content"], "sunny");
}

// ---------------------------------------------------------------------------
// Claude: 401 → refresh → retry（同じファイルへ書き戻る）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claude_401_refreshes_and_retries_then_writes_back_the_credentials_file() {
    #[derive(Default)]
    struct Once401 {
        calls: AtomicU32,
    }
    async fn handler(
        AxumState(state): AxumState<Arc<Once401>>,
        headers: HeaderMap,
    ) -> axum::response::Response {
        let n = state.calls.fetch_add(1, Ordering::SeqCst);
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if n == 0 {
            assert!(auth.contains("old-access"));
            return StatusCode::UNAUTHORIZED.into_response();
        }
        assert!(
            auth.contains("fresh-access"),
            "expected the refreshed token, got {auth}"
        );
        Json(json!({
            "id": "msg_ok",
            "content": [{"type": "text", "text": "ok after refresh"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }))
        .into_response()
    }
    async fn refresh_handler() -> axum::response::Response {
        Json(json!({"access_token": "fresh-access", "refresh_token": "fresh-refresh", "expires_in": 3600})).into_response()
    }
    let once = Arc::new(Once401::default());
    let app = Router::new()
        .route("/v1/messages", post(handler))
        .route("/oauth/token", post(refresh_handler))
        .with_state(once);
    let (upstream_addr, _h1) = spawn(app).await;

    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "old-access",
        "old-refresh",
        far_future_ms(),
    );

    let mut config = base_config();
    config.sources.claude_oauth = Some(claude_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("claude/standard", false), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["choices"][0]["message"]["content"], "ok after refresh");

    let on_disk: Value = serde_json::from_str(
        &std::fs::read_to_string(accounts_tmp.path().join("acct-a").join(".credentials.json"))
            .expect("read"),
    )
    .expect("json");
    assert_eq!(on_disk["claudeAiOauth"]["accessToken"], "fresh-access");
    assert_eq!(on_disk["claudeAiOauth"]["subscriptionType"], "max"); // 保存されている他の値は残る
}

// ---------------------------------------------------------------------------
// Claude: 429 → 次の候補（cooldown が付き、2 回目は最初から別アカウントが選ばれる）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claude_429_falls_back_to_the_next_account_and_records_a_cooldown() {
    async fn always_429() -> axum::response::Response {
        (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "30")]).into_response()
    }
    async fn always_ok() -> axum::response::Response {
        Json(json!({
            "id": "msg_ok",
            "content": [{"type": "text", "text": "from the healthy account"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }))
        .into_response()
    }
    let (bad_addr, _h1) = spawn(Router::new().route("/v1/messages", post(always_429))).await;
    let (good_addr, _h2) = spawn(Router::new().route("/v1/messages", post(always_ok))).await;

    // 2 つの account dir をそれぞれ別の上流に向けるわけにはいかない（1 つの `base_url` を共有するため）、
    // 代わりに account id の辞書順で `bad`（先に選ばれる）と `good` を作り、`bad` 用の上流を
    // 429 応答のものにして、両方が同じ `base_url` を指す構成にする（Anthropic の base_url は 1 つ）。
    // ここでは `bad` アカウントの資格情報を「悪い上流」に向けるため、`base_url` はアカウント単位ではなく
    // ソース単位という ADR の設計上、1 上流のみを 429 にして cooldown 後に同じ上流の別アカウントで
    // 再試行できることを確認する（同じ上流が 2 アカウント目では 200 を返す構成にする）。
    async fn first_429_then_ok(
        AxumState(counter): AxumState<Arc<AtomicU32>>,
    ) -> axum::response::Response {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "30")]).into_response()
        } else {
            Json(json!({
                "id": "msg_ok",
                "content": [{"type": "text", "text": "second account succeeded"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }))
            .into_response()
        }
    }
    let counter = Arc::new(AtomicU32::new(0));
    let app = Router::new()
        .route("/v1/messages", post(first_429_then_ok))
        .with_state(counter);
    let (upstream_addr, _h3) = spawn(app).await;
    let _ = bad_addr;
    let _ = good_addr;

    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "fake-access-a",
        "fake-refresh-a",
        far_future_ms(),
    );
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-b",
        "fake-access-b",
        "fake-refresh-b",
        far_future_ms(),
    );

    let mut config = base_config();
    config.sources.claude_oauth = Some(claude_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    let book = Arc::new(StdMutex::new(AccountBook::new_in_memory()));
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(book.clone()),
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h4) = spawn(router(state)).await;
    let client = reqwest::Client::new();

    let resp = post_chat(&client, addr, &chat_request("claude/standard", false), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-celeris-account")
            .and_then(|v| v.to_str().ok()),
        Some("acct-b")
    );
    let body: Value = resp.json().await.expect("json");
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "second account succeeded"
    );

    // `acct-a`（辞書順で先。最初に 429 を受けた）が cooldown になっていること。
    let guard = book.lock().expect("lock");
    assert!(guard.state("acct-a").expect("state").cooldown.is_some());
}

// ---------------------------------------------------------------------------
// 偽の Codex Responses 上流
// ---------------------------------------------------------------------------

/// Phase 65b: 本番で `codex-oauth` の要求が全て 400 で落ちていた（ChatGPT の Codex backend は
/// Codex CLI が送る形以外を拒否する）。上流はもう非 stream の JSON を返さない（**Codex backend が
/// それを受け付けないため**）。ここの偽の上流は Codex CLI と同じ形で要求が来ることを検証しつつ、
/// いつも SSE を返す（`has_output` で「ツール結果が来ているか」を見て、最終テキストかツール呼び出し
/// かを切り替える）。
#[derive(Default)]
struct CodexFake {
    captured: StdMutex<Vec<Value>>,
}

async fn codex_responses(
    AxumState(fake): AxumState<Arc<CodexFake>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    assert!(headers.get("chatgpt-account-id").is_some());
    assert!(headers.get("originator").is_some());
    assert!(
        headers.get("user-agent").is_some(),
        "codex_cli_rs の User-Agent が付くこと"
    );
    fake.captured.lock().expect("lock").push(body.clone());

    let has_output = body["input"]
        .as_array()
        .map(|items| items.iter().any(|i| i["type"] == "function_call_output"))
        .unwrap_or(false);

    if has_output {
        let text = sse_body(&[
            (
                "response.created",
                json!({"response": {"id": "resp_final"}}),
            ),
            (
                "response.output_text.delta",
                json!({"delta": "It is sunny "}),
            ),
            (
                "response.output_text.delta",
                json!({"delta": "in Tsukuba."}),
            ),
            (
                "response.completed",
                json!({"response": {"usage": {"input_tokens": 12, "output_tokens": 5}}}),
            ),
        ]);
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            text,
        )
            .into_response();
    }

    let text = sse_body(&[
        ("response.created", json!({"response": {"id": "resp_call"}})),
        (
            "response.output_item.done",
            json!({"item": {"type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": "{\"city\":\"Tsukuba\"}"}}),
        ),
        (
            "response.completed",
            json!({"response": {"usage": {"input_tokens": 9, "output_tokens": 3}}}),
        ),
    ]);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        text,
    )
        .into_response()
}

/// `codex_stream_round_trip` 専用の偽の上流（テキストだけの SSE を固定で返す）。
async fn codex_stream_text_responses(
    headers: HeaderMap,
    Json(_body): Json<Value>,
) -> axum::response::Response {
    assert!(headers.get("chatgpt-account-id").is_some());
    let text = sse_body(&[
        (
            "response.created",
            json!({"response": {"id": "resp_stream_1"}}),
        ),
        ("response.output_text.delta", json!({"delta": "Hi"})),
        ("response.output_text.delta", json!({"delta": " there"})),
        (
            "response.completed",
            json!({"response": {"usage": {"input_tokens": 6, "output_tokens": 2}}}),
        ),
    ]);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        text,
    )
        .into_response()
}

async fn codex_refresh() -> axum::response::Response {
    Json(json!({"access_token": "fresh-gpt-access", "refresh_token": "fresh-gpt-refresh"}))
        .into_response()
}

fn codex_router(fake: Arc<CodexFake>) -> Router {
    Router::new()
        .route("/responses", post(codex_responses))
        .route("/oauth/token", post(codex_refresh))
        .with_state(fake)
}

fn codex_stream_router() -> Router {
    Router::new()
        .route("/responses", post(codex_stream_text_responses))
        .route("/oauth/token", post(codex_refresh))
}

fn codex_source(addr: SocketAddr, accounts_dir: std::path::PathBuf) -> CodexOauthConfig {
    CodexOauthConfig {
        accounts_dir,
        responses_url: format!("http://{addr}/responses"),
        token_url: format!("http://{addr}/oauth/token"),
        client_id: "test-client".to_string(),
        enabled: true,
        user_agent: "codex_cli_rs/0.45.0".to_string(),
        send_sampling_params: false,
        reasoning_effort: None,
    }
}

/// Phase 65b 受け入れ条件 (a) + (b): 非 stream のクライアント要求が、上流には
/// `store:false`/`stream:true`/`instructions` あり・`temperature`/`max_output_tokens` 無しで送られ、
/// SSE の上流応答（ツール呼び出し + usage を含む）が 1 つの集約された応答になること。
#[tokio::test]
async fn codex_non_stream_round_trip() {
    let fake = Arc::new(CodexFake::default());
    let (upstream_addr, _h1) = spawn(codex_router(fake.clone())).await;
    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_codex_credentials(
        accounts_tmp.path(),
        "acct-g",
        "fake-gpt-access",
        "fake-gpt-refresh",
    );

    let mut config = base_config();
    config.sources.codex_oauth = Some(codex_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        None,
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("gpt/standard", false), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-celeris-source")
            .and_then(|v| v.to_str().ok()),
        Some("codex-oauth")
    );
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(body["usage"]["prompt_tokens"], 9);
    assert_eq!(body["usage"]["completion_tokens"], 3);

    let captured = fake.captured.lock().expect("lock");
    let sent = &captured[0];
    assert_eq!(sent["store"], false, "{sent}");
    assert_eq!(
        sent["stream"], true,
        "クライアントは stream:false で要求したが上流へはいつも stream:true: {sent}"
    );
    assert!(
        sent["instructions"].as_str().is_some_and(|s| !s.is_empty()),
        "{sent}"
    );
    assert!(sent.get("temperature").is_none(), "{sent}");
    assert!(sent.get("max_output_tokens").is_none(), "{sent}");
}

#[tokio::test]
async fn codex_tool_call_round_trip() {
    let fake = Arc::new(CodexFake::default());
    let (upstream_addr, _h1) = spawn(codex_router(fake)).await;
    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_codex_credentials(
        accounts_tmp.path(),
        "acct-g",
        "fake-gpt-access",
        "fake-gpt-refresh",
    );
    let mut config = base_config();
    config.sources.codex_oauth = Some(codex_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        None,
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;
    let client = reqwest::Client::new();

    let first = json!({"model": "gpt/standard", "stream": false, "messages": [{"role": "user", "content": "weather?"}]});
    let resp = post_chat(&client, addr, &first, None).await;
    let body: Value = resp.json().await.expect("json");
    let call = body["choices"][0]["message"]["tool_calls"][0].clone();
    let call_id = call["id"].as_str().expect("id").to_string();

    let second = json!({
        "model": "gpt/standard",
        "stream": false,
        "messages": [
            {"role": "user", "content": "weather?"},
            {"role": "assistant", "content": null, "tool_calls": [call]},
            {"role": "tool", "tool_call_id": call_id, "content": "sunny in tsukuba"},
        ]
    });
    let resp2 = post_chat(&client, addr, &second, None).await;
    let body2: Value = resp2.json().await.expect("json");
    assert_eq!(
        body2["choices"][0]["message"]["content"],
        "It is sunny in Tsukuba."
    );
}

#[tokio::test]
async fn codex_stream_round_trip() {
    let (upstream_addr, _h1) = spawn(codex_stream_router()).await;
    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_codex_credentials(
        accounts_tmp.path(),
        "acct-g",
        "fake-gpt-access",
        "fake-gpt-refresh",
    );
    let mut config = base_config();
    config.sources.codex_oauth = Some(codex_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        None,
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("gpt/cheap", true), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let text = resp.text().await.expect("text");
    assert!(text.contains("\"content\":\"Hi\""), "{text}");
    assert!(text.contains("\"content\":\" there\""), "{text}");
    assert!(text.contains("\"finish_reason\":\"stop\""), "{text}");
    assert!(text.ends_with("data: [DONE]\n\n"));
}

/// Phase 65b 受け入れ条件 (c): ChatGPT backend が返す `{"detail": "..."}` 形の 400 が、
/// プロキシのエラー本文にそのまま（要約として）残ること。
#[tokio::test]
async fn codex_400_upstream_error_surfaces_the_detail_field() {
    async fn bad_request(Json(_body): Json<Value>) -> axum::response::Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"detail": "Store must be set to false"})),
        )
            .into_response()
    }
    let app = Router::new()
        .route("/responses", post(bad_request))
        .route("/oauth/token", post(codex_refresh));
    let (upstream_addr, _h1) = spawn(app).await;
    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_codex_credentials(
        accounts_tmp.path(),
        "acct-g",
        "fake-gpt-access",
        "fake-gpt-refresh",
    );

    let mut config = base_config();
    config.sources.codex_oauth = Some(codex_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        None,
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("gpt/standard", false), None).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("json");
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("message")
            .contains("Store must be set to false"),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// openai-compatible relay: そのまま中継
// ---------------------------------------------------------------------------

async fn relay_models() -> axum::response::Response {
    Json(json!({"object": "list", "data": [{"id": "qwen3.8-27b", "object": "model"}]}))
        .into_response()
}
async fn relay_chat(Json(body): Json<Value>) -> axum::response::Response {
    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if stream {
        let text = "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"qwen3.8-27b\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n".to_string();
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            text,
        )
            .into_response();
    }
    Json(json!({
        "id": "1", "object": "chat.completion", "model": "qwen3.8-27b",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "relayed reply"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 4, "completion_tokens": 2, "total_tokens": 6}
    }))
    .into_response()
}

fn relay_router() -> Router {
    Router::new()
        .route("/v1/models", get(relay_models))
        .route("/v1/chat/completions", post(relay_chat))
}

#[tokio::test]
async fn relay_non_stream_passes_through_and_rewrites_the_model_field() {
    let (upstream_addr, _h1) = spawn(relay_router()).await;
    let mut config = base_config();
    config
        .sources
        .openai_compatible
        .push(OpenAiCompatibleConfig {
            id: "qwen".into(),
            base_url: format!("http://{upstream_addr}/v1"),
            api_key: None,
            enabled: true,
        });
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        None,
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("qwen/cheap", false), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-celeris-source")
            .and_then(|v| v.to_str().ok()),
        Some("openai-compatible:qwen")
    );
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["choices"][0]["message"]["content"], "relayed reply");
    assert_eq!(body["model"], "qwen/cheap"); // 要求した抽象名に書き換わっている
}

#[tokio::test]
async fn relay_stream_passes_through() {
    let (upstream_addr, _h1) = spawn(relay_router()).await;
    let mut config = base_config();
    config
        .sources
        .openai_compatible
        .push(OpenAiCompatibleConfig {
            id: "qwen".into(),
            base_url: format!("http://{upstream_addr}/v1"),
            api_key: None,
            enabled: true,
        });
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        None,
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("qwen/cheap", true), None).await;
    let text = resp.text().await.expect("text");
    assert!(text.contains("\"content\":\"hi\""), "{text}");
    assert!(text.contains("\"model\":\"qwen/cheap\""), "{text}");
    assert!(text.ends_with("data: [DONE]\n\n"));
}

// ---------------------------------------------------------------------------
// 選択: free 優先、cooldown を経て次の候補
// ---------------------------------------------------------------------------

#[tokio::test]
async fn celeris_tier_prefers_the_reachable_relay_when_prefer_free() {
    let (relay_addr, _h1) = spawn(relay_router()).await;
    let (anthropic_addr, _h2) = spawn(anthropic_router(Arc::new(AnthropicFake::default()))).await;
    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "fake-access-a",
        "fake-refresh-a",
        far_future_ms(),
    );

    let mut config = LlmProxyConfig {
        prefer_free: true,
        ..LlmProxyConfig::default()
    };
    config.sources.claude_oauth = Some(claude_source(
        anthropic_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    config
        .sources
        .openai_compatible
        .push(OpenAiCompatibleConfig {
            id: "qwen".into(),
            base_url: format!("http://{relay_addr}/v1"),
            api_key: None,
            enabled: true,
        });
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h3) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("celeris/cheap", false), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-celeris-source")
            .and_then(|v| v.to_str().ok()),
        Some("openai-compatible:qwen")
    );
}

#[tokio::test]
async fn celeris_tier_falls_back_to_the_account_pool_when_no_relay_is_reachable() {
    let (anthropic_addr, _h2) = spawn(anthropic_router(Arc::new(AnthropicFake::default()))).await;
    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "fake-access-a",
        "fake-refresh-a",
        far_future_ms(),
    );

    let mut config = LlmProxyConfig {
        prefer_free: true,
        ..LlmProxyConfig::default()
    };
    config.sources.claude_oauth = Some(claude_source(
        anthropic_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    // 到達しない relay（bind していないポート）を 1 つ入れておく。
    config
        .sources
        .openai_compatible
        .push(OpenAiCompatibleConfig {
            id: "qwen".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: None,
            enabled: true,
        });
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h3) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("celeris/cheap", false), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-celeris-source")
            .and_then(|v| v.to_str().ok()),
        Some("claude-oauth")
    );
}

// ---------------------------------------------------------------------------
// bearer / embeddings / models / healthz / standby
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bearer_is_required_when_a_token_is_configured() {
    let state = ProxyState::new(
        base_config(),
        reqwest::Client::new(),
        None,
        None,
        Some("s3cr3t".to_string()),
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h) = spawn(router(state)).await;
    let client = reqwest::Client::new();

    let unauthorized = get_json(&client, addr, "/v1/models", None).await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let wrong = get_json(&client, addr, "/v1/models", Some("nope")).await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let ok = get_json(&client, addr, "/v1/models", Some("s3cr3t")).await;
    assert_eq!(ok.status(), StatusCode::OK);

    // /healthz は認証なしで通る。
    let health = get_json(&client, addr, "/healthz", None).await;
    assert_eq!(health.status(), StatusCode::OK);
}

#[tokio::test]
async fn embeddings_are_not_implemented() {
    let state = ProxyState::new(
        base_config(),
        reqwest::Client::new(),
        None,
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v1/embeddings"))
        .json(&json!({"model": "celeris/cheap", "input": "hi"}))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn models_lists_only_the_configured_sources() {
    let mut config = base_config();
    config
        .sources
        .openai_compatible
        .push(OpenAiCompatibleConfig {
            id: "qwen".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            api_key: None,
            enabled: true,
        });
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        None,
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = get_json(&client, addr, "/v1/models", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("json");
    let ids: Vec<String> = body["data"]
        .as_array()
        .expect("data")
        .iter()
        .map(|m| m["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(ids.contains(&"celeris/cheap".to_string()));
    assert!(ids.contains(&"qwen/standard".to_string()));
    assert!(!ids.iter().any(|id| id.starts_with("claude/")));
    assert!(!ids.iter().any(|id| id.starts_with("gpt/")));
}

#[tokio::test]
async fn standby_returns_503_like_other_admin_endpoints() {
    let role = SharedRole::new(task_core::InstanceRole::Standby);
    let state = ProxyState::new(
        base_config(),
        reqwest::Client::new(),
        None,
        None,
        None,
        role,
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = get_json(&client, addr, "/v1/models", None).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    // /healthz は role に関わらず liveness を返す。
    let health = get_json(&client, addr, "/healthz", None).await;
    assert_eq!(health.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// 要求の記録（`llm_proxy_requests`）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_successful_request_is_recorded_with_tokens_and_no_body() {
    let fake = Arc::new(AnthropicFake::default());
    let (upstream_addr, _h1) = spawn(anthropic_router(fake)).await;
    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "fake-access-a",
        "fake-refresh-a",
        far_future_ms(),
    );
    let db_tmp = tempfile::tempdir().expect("tmp");
    let db_path = make_db(db_tmp.path());

    let mut config = base_config();
    config.sources.claude_oauth = Some(claude_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        None,
        SharedRole::default(),
        Some(db_path.clone()),
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("claude/standard", false), None).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let conn = rusqlite::Connection::open(&db_path).expect("open");
    let (source, account, status, prompt, completion): (String, String, String, i64, i64) = conn
        .query_row(
            "SELECT source, account, status, prompt_tokens, completion_tokens FROM llm_proxy_requests",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("row");
    assert_eq!(source, "claude-oauth");
    assert_eq!(account, "acct-a");
    assert_eq!(status, "ok");
    assert_eq!(prompt, 5);
    assert_eq!(completion, 3);
}

// ---------------------------------------------------------------------------
// sources_view（GET /llm/sources の材料。ADR-0053 D4。API 化は Phase 66）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sources_view_reports_cooldown_and_hourly_counts() {
    let fake = Arc::new(AnthropicFake::default());
    let (upstream_addr, _h1) = spawn(anthropic_router(fake)).await;
    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "fake-access-a",
        "fake-refresh-a",
        far_future_ms(),
    );
    let db_tmp = tempfile::tempdir().expect("tmp");
    let db_path = make_db(db_tmp.path());

    let mut config = base_config();
    config.sources.claude_oauth = Some(claude_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    let book = Arc::new(StdMutex::new(AccountBook::new_in_memory()));
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    book.lock().expect("lock").set_cooldown(
        "acct-a",
        task_dispatch::accounts::AccountCooldown {
            until: now + 500,
            reason: task_dispatch::accounts::AccountCooldownReason::Throttled,
        },
        now,
    );
    let state = llm_proxy::ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(book),
        None,
        None,
        SharedRole::default(),
        Some(db_path),
        std::time::Duration::from_secs(5),
    );
    let view = state.sources_view(now).await;
    let claude = view
        .sources
        .iter()
        .find(|s| s.id == "claude-oauth")
        .expect("claude source");
    let acct = claude
        .accounts
        .iter()
        .find(|a| a.id == "acct-a")
        .expect("acct-a");
    assert!(acct.cooldown_until.is_some());
    assert_eq!(acct.cooldown_reason.as_deref(), Some("Throttled"));
    assert_eq!(claude.last_hour_requests, 0);
}

// ---------------------------------------------------------------------------
// 秘密の値がログに出ない
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct CapturingWriter(Arc<StdMutex<Vec<u8>>>);

impl std::io::Write for CapturingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturingWriter {
    type Writer = CapturingWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn no_secret_value_appears_in_the_logs_even_across_a_token_refresh() {
    async fn handler(
        headers: HeaderMap,
        calls: AxumState<Arc<AtomicU32>>,
    ) -> axum::response::Response {
        let AxumState(calls) = calls;
        let n = calls.fetch_add(1, Ordering::SeqCst);
        let _ = headers;
        if n == 0 {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Json(json!({"id": "1", "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 1}})).into_response()
    }
    async fn refresh() -> axum::response::Response {
        Json(json!({"access_token": "brand-new-secret-access", "refresh_token": "brand-new-secret-refresh", "expires_in": 3600})).into_response()
    }
    let calls = Arc::new(AtomicU32::new(0));
    let app = Router::new()
        .route("/v1/messages", post(handler))
        .with_state(calls)
        .route("/oauth/token", post(refresh));
    let (upstream_addr, _h1) = spawn(app).await;

    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "super-secret-original-access",
        "super-secret-original-refresh",
        far_future_ms(),
    );

    let mut config = base_config();
    config.sources.claude_oauth = Some(claude_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(Arc::new(StdMutex::new(AccountBook::new_in_memory()))),
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );

    let writer = CapturingWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (addr, _h2) = spawn(router(state)).await;
    let client = reqwest::Client::new();
    let resp = post_chat(&client, addr, &chat_request("claude/standard", false), None).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 少し待ってログの flush を確実にする（同一スレッドの current_thread ランタイムなので基本は同期的）。
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let captured = String::from_utf8_lossy(&writer.0.lock().expect("lock")).to_string();
    for secret in [
        "super-secret-original-access",
        "super-secret-original-refresh",
        "brand-new-secret-access",
        "brand-new-secret-refresh",
    ] {
        assert!(
            !captured.contains(secret),
            "log leaked a secret value: {captured}"
        );
    }
}

// ---------------------------------------------------------------------------
// ADR-0063 Phase 109b C1: 候補が一時的に全部無くなったときの再走査
// ---------------------------------------------------------------------------

/// 候補が 1 周目は全滅（cooldown 中）で、1 秒待った 2 周目には cooldown が切れていて成功する
/// （即 503 にはしない）。`until` は実時間で 1 秒後に設定し、実際に 1 秒待たせて確かめる。
#[tokio::test]
async fn no_source_available_rescans_after_a_short_wait_and_then_succeeds() {
    let fake = Arc::new(AnthropicFake::default());
    let (upstream_addr, _h1) = spawn(anthropic_router(fake.clone())).await;

    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "fake-access-a",
        "fake-refresh-a",
        far_future_ms(),
    );

    let mut config = base_config();
    config.sources.claude_oauth = Some(claude_source(
        upstream_addr,
        accounts_tmp.path().to_path_buf(),
    ));

    // `until = now + 2`（1 秒ではなく）: 秒未満の端数が切り捨てられる `unix_timestamp()` の境界
    // （セットアップが `T.999` 秒で行われると `until = T+1` は数ミリ秒後にはもう過去になり得る）で
    // 1 周目からすり抜けてテストが flaky にならないよう、確実に 1 回はすり抜けない余裕を持たせる。
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut book = AccountBook::new_in_memory();
    book.set_cooldown(
        "acct-a",
        task_dispatch::accounts::AccountCooldown {
            until: now + 2,
            reason: task_dispatch::accounts::AccountCooldownReason::Throttled,
        },
        now,
    );
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(Arc::new(StdMutex::new(book))),
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h2) = spawn(router(state)).await;

    let client = reqwest::Client::new();
    let started = std::time::Instant::now();
    let resp = post_chat(&client, addr, &chat_request("claude/standard", false), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(900),
        "少なくとも 1 回は再走査の待ちがあったはず: {:?}",
        started.elapsed()
    );
    assert_eq!(
        resp.headers()
            .get("x-celeris-account")
            .and_then(|v| v.to_str().ok()),
        Some("acct-a")
    );
}

/// 候補が最後まで無い（cooldown が切れない）場合は、最大 2 周（合計 2 秒）待った上で 503
/// `no_source_available` と `Retry-After: 5` を返す。
#[tokio::test]
async fn no_source_available_gives_up_with_retry_after_when_nothing_ever_recovers() {
    let accounts_tmp = tempfile::tempdir().expect("tmp");
    write_claude_credentials(
        accounts_tmp.path(),
        "acct-a",
        "fake-access-a",
        "fake-refresh-a",
        far_future_ms(),
    );

    let mut config = base_config();
    config.sources.claude_oauth = Some(claude_source(
        "127.0.0.1:1".parse().unwrap(),
        accounts_tmp.path().to_path_buf(),
    ));

    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut book = AccountBook::new_in_memory();
    book.set_cooldown(
        "acct-a",
        task_dispatch::accounts::AccountCooldown {
            until: now + 3600,
            reason: task_dispatch::accounts::AccountCooldownReason::Throttled,
        },
        now,
    );
    let state = ProxyState::new(
        config,
        reqwest::Client::new(),
        Some(Arc::new(StdMutex::new(book))),
        None,
        None,
        SharedRole::default(),
        None,
        std::time::Duration::from_secs(5),
    );
    let (addr, _h) = spawn(router(state)).await;

    let client = reqwest::Client::new();
    let started = std::time::Instant::now();
    let resp = post_chat(&client, addr, &chat_request("claude/standard", false), None).await;
    let elapsed = started.elapsed();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        resp.headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("5")
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(1900),
        "2 回分の再走査待ちがあったはず: {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(4),
        "3 秒以内という要件: {elapsed:?}"
    );
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["error"]["type"], "no_source_available");
}
