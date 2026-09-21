//! ADR-0056 D1/D2/D4/D5（Phase 78）: **偽の MCP クライアント**（本物の HTTP）で celeris-mcp を通す。
//!
//! 実際に `127.0.0.1:0` に bind し、`reqwest` で `initialize` → `tools/list` → `tools/call` の
//! 往復を確かめる（外部ネットワークには出ない）。

use std::net::SocketAddr;
use std::sync::Arc;

use celeris_mcp::config::ListenerAuth;
use celeris_mcp::state::McpState;
use serde_json::{Value, json};
use task_core::{
    GenreSpec, McpClient, McpClientStore, McpScope, OrgKind, OrgNode, RoleSpec, SqliteStore,
    TaskStore, Tier,
};
use time::OffsetDateTime;

// ---------------------------------------------------------------------------
// 土台
// ---------------------------------------------------------------------------

struct Server {
    base_url: String,
    store: Arc<SqliteStore>,
    _kb_root: Option<tempfile::TempDir>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl Server {
    async fn stop(mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.handle.take() {
            let _ = h.await;
        }
    }
}

fn roles_and_genres() -> (Vec<RoleSpec>, Vec<GenreSpec>) {
    (
        vec![RoleSpec {
            id: "secretary".into(),
            tier: Some(Tier::Standard),
            adapter: Some("claude-code".into()),
            ..RoleSpec::default()
        }],
        vec![GenreSpec {
            id: "secretary".into(),
            description: "人と話す".into(),
            default_role: Some("secretary".into()),
            roles: vec!["secretary".into()],
            ..GenreSpec::default()
        }],
    )
}

fn seed_org(store: &SqliteStore) {
    let now = OffsetDateTime::now_utc();
    let node = |id: &str, parent: Option<&str>, kind: OrgKind, name: &str| OrgNode {
        id: id.to_string(),
        parent_id: parent.map(str::to_string),
        name: name.to_string(),
        kind,
        genre: if id == "cos" { Some("secretary".to_string()) } else { None },
        brief: String::new(),
        profile: Default::default(),
        position: 0,
        created_at: now,
        updated_at: now,
    };
    store.org_upsert(&node("cos", None, OrgKind::Secretary, "Chief of Staff")).unwrap();
    store
        .org_upsert(&node("engineering", Some("cos"), OrgKind::Department, "Engineering"))
        .unwrap();
}

async fn spawn_server_with(
    store: Arc<SqliteStore>,
    auth: ListenerAuth,
    kb_root: Option<tempfile::TempDir>,
    rate_limit_per_min: u32,
) -> Server {
    let (roles, genres) = roles_and_genres();
    let knowledge_root = kb_root.as_ref().map(|d| d.path().to_path_buf());
    let state = McpState::from_store(
        Arc::clone(&store),
        rate_limit_per_min,
        roles,
        genres,
        "secretary".to_string(),
        knowledge_root,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("addr");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(celeris_mcp::serve(listener, state, auth, async {
        let _ = stop_rx.await;
    }));
    Server {
        base_url: format!("http://{addr}"),
        store,
        _kb_root: kb_root,
        stop: Some(stop_tx),
        handle: Some(handle),
    }
}

/// トークン認証 1 口。KB は初期化済みの一時ディレクトリ。
async fn spawn_token_server(rate_limit_per_min: u32) -> Server {
    let store = Arc::new(SqliteStore::open_in_memory().expect("open"));
    seed_org(&store);
    let kb_root = tempfile::tempdir().expect("tmp");
    task_ops::knowledge::init(kb_root.path()).expect("kb init");
    spawn_server_with(store, ListenerAuth::Token, Some(kb_root), rate_limit_per_min).await
}

fn create_client(store: &SqliteStore, id: &str, token: Option<&str>, scopes: Vec<McpScope>) {
    store
        .mcp_client_create(&McpClient {
            id: id.to_string(),
            name: id.to_string(),
            token_hash: token.map(celeris_mcp::auth::hash_token),
            scopes,
            created_at: OffsetDateTime::now_utc(),
            last_used_at: None,
            revoked_at: None,
        })
        .expect("create client");
}

async fn rpc(
    client: &reqwest::Client,
    base_url: &str,
    token: Option<&str>,
    session: Option<&str>,
    body: Value,
) -> reqwest::Response {
    let mut req = client.post(format!("{base_url}/mcp")).json(&body);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    if let Some(s) = session {
        req = req.header("mcp-session-id", s);
    }
    req.send().await.expect("send")
}

async fn initialize(client: &reqwest::Client, base_url: &str, token: Option<&str>) -> String {
    let resp = rpc(
        client,
        base_url,
        token,
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
    )
    .await;
    assert_eq!(resp.status(), 200, "initialize should succeed");
    resp.headers()
        .get("mcp-session-id")
        .expect("Mcp-Session-Id header")
        .to_str()
        .expect("ascii")
        .to_string()
}

// ---------------------------------------------------------------------------
// initialize → tools/list → tools/call
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialize_then_tools_list_then_ping_round_trip() {
    let server = spawn_token_server(60).await;
    create_client(&server.store, "c1", Some("secret"), McpScope::DEFAULT.to_vec());
    let client = reqwest::Client::new();
    let session = initialize(&client, &server.base_url, Some("secret")).await;

    let resp = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    let tools = body["result"]["tools"].as_array().expect("tools array");
    assert!(tools.iter().any(|t| t["name"] == "knowledge_search"));
    // 既定スコープ（org:write / skills:write 無し）には org_create_node は出ない。
    assert!(!tools.iter().any(|t| t["name"] == "org_create_node"));

    let resp = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 3, "method": "ping", "params": {}}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["result"], json!({}));

    server.stop().await;
}

#[tokio::test]
async fn tools_list_is_filtered_by_scope() {
    let server = spawn_token_server(60).await;
    create_client(&server.store, "reader", Some("t1"), vec![McpScope::KnowledgeRead]);
    create_client(
        &server.store,
        "writer",
        Some("t2"),
        vec![McpScope::KnowledgeRead, McpScope::OrgWrite, McpScope::SkillsWrite],
    );
    let client = reqwest::Client::new();

    let session = initialize(&client, &server.base_url, Some("t1")).await;
    let resp = rpc(
        &client,
        &server.base_url,
        Some("t1"),
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await;
    let body: Value = resp.json().await.expect("json");
    let mut names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["knowledge_get", "knowledge_list", "knowledge_search"]);

    let session2 = initialize(&client, &server.base_url, Some("t2")).await;
    let resp = rpc(
        &client,
        &server.base_url,
        Some("t2"),
        Some(&session2),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await;
    let body: Value = resp.json().await.expect("json");
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"org_create_node"));
    assert!(names.contains(&"skills_put"));

    server.stop().await;
}

// ---------------------------------------------------------------------------
// 認証
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_or_revoked_token_is_401() {
    let server = spawn_token_server(60).await;
    create_client(&server.store, "c1", Some("secret"), vec![]);
    {
        use task_core::McpClientStore;
        server.store.mcp_client_revoke("c1", OffsetDateTime::now_utc()).unwrap();
    }
    let client = reqwest::Client::new();

    let resp = rpc(
        &client,
        &server.base_url,
        None,
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
    )
    .await;
    assert_eq!(resp.status(), 401, "missing token");

    let resp = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
    )
    .await;
    assert_eq!(resp.status(), 401, "revoked token");

    server.stop().await;
}

#[tokio::test]
async fn a_fixed_none_listener_binds_every_request_to_its_named_client() {
    let store = Arc::new(SqliteStore::open_in_memory().expect("open"));
    seed_org(&store);
    create_client(&store, "chatgpt", None, vec![McpScope::KnowledgeRead, McpScope::KnowledgePropose]);
    let kb_root = tempfile::tempdir().expect("tmp");
    task_ops::knowledge::init(kb_root.path()).expect("kb init");
    let server = spawn_server_with(
        Arc::clone(&store),
        ListenerAuth::Fixed("chatgpt".to_string()),
        Some(kb_root),
        60,
    )
    .await;
    let client = reqwest::Client::new();

    // Bearer が無くても（あっても無視して）このクライアントとして扱われる。
    let session = initialize(&client, &server.base_url, None).await;
    let resp = rpc(
        &client,
        &server.base_url,
        None,
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "knowledge_propose",
            "arguments": {"title": "t", "body": "b", "scope": "user"}
        }}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert!(body["result"]["content"].is_array(), "{body}");

    let calls = {
        use task_core::McpCallStore;
        store.mcp_calls_list(Some("chatgpt"), 100).unwrap()
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].client_id, "chatgpt");
    assert!(calls[0].ok);

    server.stop().await;
}

#[tokio::test]
async fn session_is_required_after_initialize() {
    let server = spawn_token_server(60).await;
    create_client(&server.store, "c1", Some("secret"), McpScope::DEFAULT.to_vec());
    let client = reqwest::Client::new();
    let resp = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        None,
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping", "params": {}}),
    )
    .await;
    assert_eq!(resp.status(), 400);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// knowledge_propose: _inbox / mcp:<client> / 秘密の拒否
// ---------------------------------------------------------------------------

#[tokio::test]
async fn knowledge_propose_writes_to_inbox_with_the_mcp_source_and_rejects_secrets() {
    let server = spawn_token_server(60).await;
    create_client(&server.store, "chatgpt", Some("secret"), vec![McpScope::KnowledgePropose]);
    let client = reqwest::Client::new();
    let session = initialize(&client, &server.base_url, Some("secret")).await;

    let resp = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "knowledge_propose",
            "arguments": {
                "title": "pegasus の使い方",
                "body": "pjsub で投げる。",
                "scope": "environment",
                "tags": ["pegasus"]
            }
        }}),
    )
    .await;
    let body: Value = resp.json().await.expect("json");
    let text = body["result"]["content"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).expect("inner json");
    let path = parsed["path"].as_str().expect("path");
    assert!(path.starts_with("_inbox/"), "{path}");

    let kb_root = server._kb_root.as_ref().expect("kb root").path();
    let raw = std::fs::read_to_string(kb_root.join(path)).expect("read candidate");
    assert!(raw.contains("mcp:chatgpt"), "{raw}");

    // 秘密は拒否される。
    let resp = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "knowledge_propose",
            "arguments": {
                "title": "token",
                "body": "sk-abcdefghijklmnopqrstuvwxyz012345",
                "scope": "environment"
            }
        }}),
    )
    .await;
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["error"]["code"], -32002, "{body}");

    server.stop().await;
}

// ---------------------------------------------------------------------------
// console_instruct / console_reply
// ---------------------------------------------------------------------------

#[tokio::test]
async fn console_instruct_records_an_mcp_author_and_console_reply_returns_the_reply_and_actions() {
    let server = spawn_token_server(60).await;
    create_client(&server.store, "chatgpt", Some("secret"), vec![McpScope::ConsoleInstruct]);
    let client = reqwest::Client::new();
    let session = initialize(&client, &server.base_url, Some("secret")).await;

    let resp = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "console_instruct",
            "arguments": {"text": "調査結果をタスクにして"}
        }}),
    )
    .await;
    let body: Value = resp.json().await.expect("json");
    let text = body["result"]["content"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).expect("inner json");
    let task_id: task_core::TaskId = parsed["task_id"].as_str().expect("task_id").parse().expect("ulid");

    // `author = mcp:chatgpt` の人の発言が入っている。
    let messages = server.store.message_page(Some("cos"), None, None, 100).unwrap();
    let human = messages
        .iter()
        .find(|m| m.role == task_core::MessageRole::User && m.task_id == Some(task_id))
        .expect("human message");
    assert_eq!(
        human.metadata.as_ref().and_then(|m| m.author.clone()),
        Some("mcp:chatgpt".to_string())
    );

    // まだ終わっていないので pending。
    let resp = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "console_reply",
            "arguments": {"task_id": task_id.to_string()}
        }}),
    )
    .await;
    let body: Value = resp.json().await.expect("json");
    let text = body["result"]["content"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).expect("inner json");
    assert_eq!(parsed["state"], "pending");

    // CoS の run が終わって返事する（偽のディスパッチャの代わりに直接ストアへ書く。
    // task-api の対話テストと同じ流儀）。
    let task = server.store.get(task_id).unwrap().unwrap();
    server.store.apply_transition(task_id, task_core::Trigger::Dispatch, None).unwrap();
    task_ops::conversation::record_reply(
        server.store.as_ref(),
        &task,
        "run-1",
        "3 件のタスクを作りました",
        OffsetDateTime::now_utc(),
    )
    .unwrap();
    server.store.apply_transition(task_id, task_core::Trigger::WorkerDone, None).unwrap();
    server.store.apply_transition(task_id, task_core::Trigger::ReviewPass, None).unwrap();

    let resp = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
            "name": "console_reply",
            "arguments": {"task_id": task_id.to_string(), "wait_secs": 1}
        }}),
    )
    .await;
    let body: Value = resp.json().await.expect("json");
    let text = body["result"]["content"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).expect("inner json");
    assert_eq!(parsed["state"], "done");
    assert_eq!(parsed["reply"], "3 件のタスクを作りました");

    server.stop().await;
}

// ---------------------------------------------------------------------------
// org_create_node: tools / permissions を無視する
// ---------------------------------------------------------------------------

#[tokio::test]
async fn org_create_node_ignores_tools_and_permissions() {
    let server = spawn_token_server(60).await;
    create_client(&server.store, "c1", Some("secret"), vec![McpScope::OrgWrite]);
    let client = reqwest::Client::new();
    let session = initialize(&client, &server.base_url, Some("secret")).await;

    let resp = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "org_create_node",
            "arguments": {
                "parent_id": "engineering",
                "id": "backend",
                "name": "Backend",
                "profile": {
                    "skills": ["rust"],
                    "tools": ["gh"],
                    "permissions": {"approvals": ["cluster:pegasus"]}
                }
            }
        }}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert!(body.get("error").is_none(), "{body}");

    let node = server.store.org_get("backend").unwrap().expect("node");
    assert_eq!(node.profile.skills, vec!["rust".to_string()]);
    assert!(node.profile.tools.is_empty(), "tools must be ignored");
    assert!(node.profile.permissions.approvals.is_empty(), "permissions must be ignored");

    server.stop().await;
}

// ---------------------------------------------------------------------------
// 流量制限
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rate_limit_returns_an_error_with_retry_after() {
    let server = spawn_token_server(1).await;
    create_client(&server.store, "c1", Some("secret"), vec![McpScope::KnowledgeRead]);
    let client = reqwest::Client::new();
    let session = initialize(&client, &server.base_url, Some("secret")).await;

    let call = || {
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {
            "name": "knowledge_list", "arguments": {}
        }})
    };
    let resp = rpc(&client, &server.base_url, Some("secret"), Some(&session), call()).await;
    let body: Value = resp.json().await.expect("json");
    assert!(body.get("error").is_none(), "{body}");

    let resp = rpc(&client, &server.base_url, Some("secret"), Some(&session), call()).await;
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["error"]["code"], -32000, "{body}");
    assert!(body["error"]["data"]["retry_after"].is_number(), "{body}");

    server.stop().await;
}

// ---------------------------------------------------------------------------
// mcp_calls の記録
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tools_call_is_recorded_in_mcp_calls() {
    let server = spawn_token_server(60).await;
    create_client(&server.store, "c1", Some("secret"), vec![McpScope::KnowledgeRead]);
    let client = reqwest::Client::new();
    let session = initialize(&client, &server.base_url, Some("secret")).await;

    let _ = rpc(
        &client,
        &server.base_url,
        Some("secret"),
        Some(&session),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "knowledge_list", "arguments": {}
        }}),
    )
    .await;

    let calls = {
        use task_core::McpCallStore;
        server.store.mcp_calls_list(Some("c1"), 100).unwrap()
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool, "knowledge_list");
    assert!(calls[0].ok);
    assert!(calls[0].error_kind.is_none());

    server.stop().await;
}

