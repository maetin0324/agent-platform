//! ADR-0048 D3（Phase 60b）: `POST /console/instruct`。
//!
//! 見るもの: 素の文（scope 無し）は CoS（根ノード）への対話、`@<node-id> ` 始まりの文と
//! `scope=node:<id>` はそのノードへの対話、`scope=project:<id>` は CoS にその案件を紐づけること、
//! 知らないノード・CoS が居ない組織は 404、空文は 422、管理系の 401（両構成）。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{GenreSpec, RoleSpec, TaskStore, Tier};

fn p(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    post_json_with(
        path,
        body,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(
        path,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn env_with_token() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        roles: vec![RoleSpec {
            id: "secretary".into(),
            tier: Some(Tier::Standard),
            adapter: Some("claude-code".into()),
            ..RoleSpec::default()
        }],
        genres: vec![GenreSpec {
            id: "secretary".into(),
            description: "人と話す".into(),
            default_role: Some("secretary".into()),
            roles: vec!["secretary".into()],
            ..GenreSpec::default()
        }],
        ..Default::default()
    })
}

/// CoS（`cos`）→ Engineering → Software Engineering。
async fn seed_org(app: &axum::Router) {
    for body in [
        json!({"id": "cos", "name": "Chief of Staff", "kind": "secretary", "genre": "secretary",
               "brief": "人と話す"}),
        json!({"id": "engineering", "name": "Engineering", "kind": "department", "parent_id": "cos"}),
        json!({"id": "software-engineering", "name": "Software Engineering", "kind": "section",
               "parent_id": "engineering", "brief": "コードを直す"}),
    ] {
        let resp = send(app, p("/api/v1/org", &body)).await;
        assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    }
}

/// scope 無しの素の文は CoS（`OrgKind::Secretary` の根ノード）への対話になる。
#[tokio::test]
async fn a_plain_instruction_talks_to_the_cos() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    let resp = send(
        &app,
        p(
            "/api/v1/console/instruct",
            &json!({"text": "今週の状況を教えて"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["node_id"], "cos");
    let task_id: task_core::TaskId = body["task_id"]
        .as_str()
        .expect("task_id")
        .parse()
        .expect("ulid");
    let task = env.store.get(task_id).expect("get").expect("task");
    assert_eq!(task.assignee.as_deref(), Some("cos"));
    assert_eq!(task.objective, "今週の状況を教えて");

    let items = send(&app, g("/api/v1/org/cos/messages")).await.json()["items"]
        .as_array()
        .cloned()
        .expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["text"], "今週の状況を教えて");
}

/// `@<node-id> ` 始まりの文はそのノードへの対話になり、`@mention` は本文から取り除かれる。
#[tokio::test]
async fn an_at_mention_talks_to_that_node_and_strips_the_mention() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    let resp = send(
        &app,
        p(
            "/api/v1/console/instruct",
            &json!({"text": "@software-engineering このバグを直して"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["node_id"], "software-engineering");

    let items = send(&app, g("/api/v1/org/software-engineering/messages"))
        .await
        .json()["items"]
        .as_array()
        .cloned()
        .expect("items");
    assert_eq!(
        items[0]["text"], "このバグを直して",
        "@mention は取り除かれる"
    );
}

/// `scope=node:<id>` は `@mention` が無くてもそのノードへの対話になる。
#[tokio::test]
async fn an_explicit_node_scope_routes_without_a_mention() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    let resp = send(
        &app,
        p(
            "/api/v1/console/instruct",
            &json!({"text": "このバグを直して", "scope": "node:software-engineering"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    assert_eq!(resp.json()["node_id"], "software-engineering");
}

/// `scope=project:<id>` は CoS への対話をその案件に紐づける。
#[tokio::test]
async fn a_project_scope_binds_the_cos_conversation_to_that_project() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    let created = send(
        &app,
        p(
            "/api/v1/projects",
            &json!({"title": "Pluvio", "request": "調べて"}),
        ),
    )
    .await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.text());
    let project_id = created.json()["id"].as_str().expect("id").to_string();

    let resp = send(
        &app,
        p(
            "/api/v1/console/instruct",
            &json!({"text": "進捗どうですか", "scope": format!("project:{project_id}")}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    assert_eq!(resp.json()["node_id"], "cos");
    let task_id: task_core::TaskId = resp.json()["task_id"]
        .as_str()
        .expect("task_id")
        .parse()
        .expect("ulid");
    let task = env.store.get(task_id).expect("get").expect("task");
    assert_eq!(task.project_id.map(|p| p.to_string()), Some(project_id));
}

/// 知らないノードへの `@mention` / `scope=node:` は 404。CoS が組織に居なければそれも 404。
#[tokio::test]
async fn unknown_targets_are_404() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    assert_problem(
        &send(
            &app,
            p("/api/v1/console/instruct", &json!({"text": "@ghost hi"})),
        )
        .await,
        404,
        "org_node_not_found",
    );
    assert_problem(
        &send(
            &app,
            p(
                "/api/v1/console/instruct",
                &json!({"text": "hi", "scope": "node:ghost"}),
            ),
        )
        .await,
        404,
        "org_node_not_found",
    );

    // 組織を種蒔きしていない（CoS が居ない）環境。
    let empty = env_with_token();
    let empty_app = empty.router();
    assert_problem(
        &send(
            &empty_app,
            p("/api/v1/console/instruct", &json!({"text": "hi"})),
        )
        .await,
        404,
        "org_node_not_found",
    );
}

/// 空白だけの本文は 422（`task_ops::conversation::start` の検証）。形の違う `scope` は 400。
#[tokio::test]
async fn blank_text_is_422_and_a_malformed_scope_is_400() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    assert_problem(
        &send(&app, p("/api/v1/console/instruct", &json!({"text": "   "}))).await,
        422,
        "validation",
    );
    assert_problem(
        &send(
            &app,
            p(
                "/api/v1/console/instruct",
                &json!({"text": "hi", "scope": "bogus"}),
            ),
        )
        .await,
        400,
        "bad_request",
    );
}

/// 管理系: トークンを付けない要求は 401。`token_file` 未設定構成でも 401。
#[tokio::test]
async fn instructing_the_console_is_an_admin_endpoint() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;
    assert_problem(
        &send(
            &app,
            post_json("/api/v1/console/instruct", &json!({"text": "hi"})),
        )
        .await,
        401,
        "unauthorized",
    );

    let open = TestEnv::with(EnvOptions::default());
    let open_app = open.router();
    assert_problem(
        &send(
            &open_app,
            post_json("/api/v1/console/instruct", &json!({"text": "hi"})),
        )
        .await,
        401,
        "unauthorized",
    );
}
