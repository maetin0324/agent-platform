//! api.md §8.1（認証）と §8.2（Host / Origin / Content-Type / 本文サイズ / 未知フィールド）、共通ヘッダ、404 / 405。

mod common;

use axum::body::{Body, Bytes};
use axum::http::Request;
use common::*;
use serde_json::json;
use task_core::{TaskStore, SCHEMA_VERSION};

fn valid_task_body() -> serde_json::Value {
    json!({"title": "t", "objective": "o", "acceptance": [{"type": "human", "text": "ok"}]})
}

/// ADR-0044 §5 Phase 53 追記（Phase 55）: **変更を伴う API はすべて管理系**なので、
/// 変更系のガード（Content-Type / 本文サイズ / 未知フィールド）を見るテストはトークン付きの
/// env を使い、要求に Bearer を付ける。
fn admin_env() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    })
}

fn admin() -> [(&'static str, String); 1] {
    [("authorization", format!("Bearer {TOKEN}"))]
}

/// `Request::post(..)` に Host / Content-Type / Bearer を付ける（本文はそのまま）。
fn admin_post(path: &str, content_type: Option<&str>, body: Body) -> Request<Body> {
    let mut builder = Request::post(path)
        .header("host", HOST)
        .header("authorization", format!("Bearer {TOKEN}"));
    if let Some(ct) = content_type {
        builder = builder.header("content-type", ct);
    }
    builder.body(body).expect("request")
}

#[tokio::test]
async fn bearer_token_is_required_when_configured() {
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    });
    let app = env.router();

    let missing = send(&app, get("/api/v1/events")).await;
    assert_problem(&missing, 401, "unauthorized");
    assert_eq!(missing.header("www-authenticate"), Some("Bearer realm=\"celeris\""));

    let wrong = send(&app, get_with("/api/v1/events", &[("authorization", "Bearer not-the-token")])).await;
    assert_problem(&wrong, 401, "unauthorized");

    let basic = send(&app, get_with("/api/v1/events", &[("authorization", "Basic czNjcmV0")])).await;
    assert_problem(&basic, 401, "unauthorized");

    let prefix = send(&app, get_with("/api/v1/events", &[("authorization", "Bearer s3cret")])).await;
    assert_problem(&prefix, 401, "unauthorized");

    let ok = send(&app, get_with("/api/v1/events", &[("authorization", &format!("Bearer {TOKEN}"))])).await;
    assert_eq!(ok.status, 200, "{}", ok.text());

    let lower = send(&app, get_with("/api/v1/events", &[("authorization", &format!("bearer {TOKEN}"))])).await;
    assert_eq!(lower.status, 200);

    // 未定義のパスも認証が先（経路の有無を漏らさない）。
    let unknown = send(&app, get("/api/v1/nope")).await;
    assert_problem(&unknown, 401, "unauthorized");

    // 変更系も同じ。
    let post = send(&app, post_json("/api/v1/tasks", &valid_task_body())).await;
    assert_problem(&post, 401, "unauthorized");
    assert!(env.store.list(None).expect("list").is_empty());
}

#[tokio::test]
async fn health_is_unauthenticated_but_still_host_checked() {
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    });
    let app = env.router();

    let health = send(&app, get("/api/v1/health")).await;
    assert_eq!(health.status, 200, "{}", health.text());
    assert_eq!(health.header("content-type"), Some("application/json; charset=utf-8"));
    let body = health.json();
    assert_eq!(body["api_version"], "1");
    assert_eq!(body["schema_version"], SCHEMA_VERSION);
    assert_eq!(body["celeris_version"], "0.9.0-test");
    assert_eq!(body["instance_id"], "01J9ZX5T3K8Q7W6V5R4P3N2M1H");
    assert_eq!(body["started_at"], "2026-09-14T00:00:00Z");
    assert!(body["now"].as_str().is_some_and(|s| s.ends_with('Z')));
    assert_eq!(body["db"]["journal_mode"], "wal");
    assert_eq!(body["db"]["busy_timeout_ms"], 5000);
    assert!(!health.text().contains(TOKEN));
    assert!(!health.text().contains("celeris.db"), "health must not expose the DB path");

    let evil = send(&app, get_with("/api/v1/health", &[("host", "evil.example")])).await;
    assert_problem(&evil, 400, "host_not_allowed");
}

#[tokio::test]
async fn no_token_means_no_authentication() {
    let env = TestEnv::new();
    let app = env.router();
    let ok = send(&app, get("/api/v1/events")).await;
    assert_eq!(ok.status, 200);
    let with_header = send(&app, get_with("/api/v1/events", &[("authorization", "Bearer whatever")])).await;
    assert_eq!(with_header.status, 200);
}

#[tokio::test]
async fn host_header_is_checked_against_the_allow_list() {
    let env = TestEnv::with(EnvOptions {
        allowed_hosts: vec!["celeris.lab.example".into()],
        ..Default::default()
    });
    let app = env.router();

    for host in ["evil.example", "evil.example:7710", "127.0.0.2", "celeris.lab.example.evil.example"] {
        let resp = send(&app, get_with("/api/v1/events", &[("host", host)])).await;
        assert_problem(&resp, 400, "host_not_allowed");
    }
    for host in ["localhost", "localhost:7710", "127.0.0.1", "127.0.0.1:9999", "[::1]:7710", "[::1]", "celeris.lab.example:7710", "CELERIS.lab.example"] {
        let resp = send(&app, get_with("/api/v1/events", &[("host", host)])).await;
        assert_eq!(resp.status, 200, "host {host}: {}", resp.text());
    }

    let no_host = Request::get("/api/v1/events").body(Body::empty()).expect("request");
    let resp = send(&app, no_host).await;
    assert_problem(&resp, 400, "host_not_allowed");

    let duplicated = Request::get("/api/v1/events")
        .header("host", HOST)
        .header("host", "evil.example")
        .body(Body::empty())
        .expect("request");
    assert_problem(&send(&app, duplicated).await, 400, "host_not_allowed");

    let absolute_form = Request::get("http://evil.example/api/v1/events")
        .header("host", HOST)
        .body(Body::empty())
        .expect("request");
    assert_problem(&send(&app, absolute_form).await, 400, "host_not_allowed");
    let absolute_ok = Request::get("http://localhost:7710/api/v1/events").body(Body::empty()).expect("request");
    assert_eq!(send(&app, absolute_ok).await.status, 200);

    // Host 検査は POST の他の検査より先。
    let evil_post = Request::post("/api/v1/tasks")
        .header("host", "evil.example")
        .header("origin", "http://evil.example")
        .body(Body::empty())
        .expect("request");
    assert_problem(&send(&app, evil_post).await, 400, "host_not_allowed");
}

#[tokio::test]
async fn post_with_origin_is_forbidden_and_changes_nothing() {
    let env = TestEnv::new();
    let app = env.router();
    let request = Request::post("/api/v1/tasks")
        .header("host", HOST)
        .header("content-type", "application/json")
        .header("origin", "http://localhost:7700")
        .body(Body::from(valid_task_body().to_string()))
        .expect("request");
    let resp = send(&app, request).await;
    assert_problem(&resp, 403, "origin_forbidden");
    assert!(env.store.list(None).expect("list").is_empty());

    // GET は Origin があっても通る（変更系だけの検査）。
    let get_resp = send(&app, get_with("/api/v1/events", &[("origin", "http://localhost:7700")])).await;
    assert_eq!(get_resp.status, 200);
    assert!(get_resp.headers.keys().all(|k| !k.as_str().starts_with("access-control-")));
}

#[tokio::test]
async fn post_requires_json_content_type() {
    let env = admin_env();
    let app = env.router();

    let without = admin_post("/api/v1/tasks", None, Body::from(valid_task_body().to_string()));
    assert_problem(&send(&app, without).await, 415, "unsupported_media_type");

    let form = admin_post(
        "/api/v1/tasks",
        Some("application/x-www-form-urlencoded"),
        Body::from("title=t"),
    );
    assert_problem(&send(&app, form).await, 415, "unsupported_media_type");

    let text = admin_post(
        "/api/v1/tasks/01J9ZX5T3K8Q7W6V5R4P3N2M1H/cancel",
        Some("text/plain"),
        Body::empty(),
    );
    assert_problem(&send(&app, text).await, 415, "unsupported_media_type");
    assert!(env.store.list(None).expect("list").is_empty());

    let with_charset = admin_post(
        "/api/v1/tasks",
        Some("Application/JSON; charset=utf-8"),
        Body::from(valid_task_body().to_string()),
    );
    assert_eq!(send(&app, with_charset).await.status, 201);
}

#[tokio::test]
async fn bodies_over_one_mebibyte_are_rejected_with_413() {
    let env = admin_env();
    let app = env.router();
    let big = vec![b' '; 1024 * 1024 + 1];

    let declared = Request::post("/api/v1/tasks")
        .header("host", HOST)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-length", big.len().to_string())
        .body(Body::from(big.clone()))
        .expect("request");
    assert_problem(&send(&app, declared).await, 413, "payload_too_large");

    // Content-Length 無し（chunked 相当）でも読み取り中に打ち切る。
    let chunks: Vec<Result<Bytes, std::io::Error>> = big.chunks(64 * 1024).map(|c| Ok(Bytes::copy_from_slice(c))).collect();
    let streamed = admin_post(
        "/api/v1/tasks",
        Some("application/json"),
        Body::from_stream(futures_util::stream::iter(chunks)),
    );
    assert_problem(&send(&app, streamed).await, 413, "payload_too_large");

    // ちょうど 1 MiB は上限内（本文としては JSON の空白だけなので 400）。
    let exact = vec![b' '; 1024 * 1024];
    let at_limit = admin_post("/api/v1/tasks", Some("application/json"), Body::from(exact));
    assert_problem(&send(&app, at_limit).await, 400, "bad_request");
    assert!(env.store.list(None).expect("list").is_empty());
}

#[tokio::test]
async fn malformed_and_unknown_fields_are_bad_requests() {
    let env = admin_env();
    let app = env.router();
    let admin = admin();
    let headers: Vec<(&str, &str)> = admin.iter().map(|(k, v)| (*k, v.as_str())).collect();

    let mut unknown = valid_task_body();
    unknown["bogus"] = json!(1);
    assert_problem(
        &send(&app, post_json_with("/api/v1/tasks", &unknown, &headers)).await,
        400,
        "bad_request",
    );

    let unknown_criterion = json!({"title": "t", "objective": "o", "acceptance": [{"type": "human", "text": "ok", "extra": true}]});
    assert_problem(
        &send(&app, post_json_with("/api/v1/tasks", &unknown_criterion, &headers)).await,
        400,
        "bad_request",
    );

    let wrong_type = json!({"title": "t", "objective": "o", "acceptance": [], "priority": "high"});
    assert_problem(
        &send(&app, post_json_with("/api/v1/tasks", &wrong_type, &headers)).await,
        400,
        "bad_request",
    );

    let syntax = admin_post("/api/v1/tasks", Some("application/json"), Body::from("{\"title\": "));
    assert_problem(&send(&app, syntax).await, 400, "bad_request");

    let id = task_core::TaskId::new();
    let approve_unknown = post_json_with(&format!("/api/v1/tasks/{id}/approve"), &json!({"nope": true}), &headers);
    assert_problem(&send(&app, approve_unknown).await, 400, "bad_request");
    let bad_status = post_json_with(
        &format!("/api/v1/tasks/{id}/cancel"),
        &json!({"expected_status": "sleeping"}),
        &headers,
    );
    assert_problem(&send(&app, bad_status).await, 400, "bad_request");
    let plan_unknown = post_json_with("/api/v1/plans", &json!({"goal": "g", "tier": "cheap", "color": "red"}), &headers);
    assert_problem(&send(&app, plan_unknown).await, 400, "bad_request");
    let replay_unknown = post_json_with("/api/v1/replay", &json!({"dry_run": true}), &headers);
    assert_problem(&send(&app, replay_unknown).await, 400, "bad_request");
    assert!(env.store.list(None).expect("list").is_empty());
}

#[tokio::test]
async fn common_headers_are_set_and_cors_is_never_emitted() {
    let env = TestEnv::new();
    let app = env.router();

    let ok = send(&app, get_with("/api/v1/health", &[("origin", "http://localhost:7700")])).await;
    assert_eq!(ok.header("cache-control"), Some("no-store"));
    assert_eq!(ok.header("x-content-type-options"), Some("nosniff"));
    let request_id = ok.header("x-request-id").expect("x-request-id");
    assert_eq!(request_id.len(), 26);
    assert!(request_id.parse::<ulid::Ulid>().is_ok());
    assert!(ok.headers.keys().all(|k| !k.as_str().starts_with("access-control-")));

    let preflight = Request::builder()
        .method("OPTIONS")
        .uri("/api/v1/tasks")
        .header("host", HOST)
        .header("origin", "http://localhost:7700")
        .header("access-control-request-method", "POST")
        .body(Body::empty())
        .expect("request");
    let resp = send(&app, preflight).await;
    assert_problem(&resp, 405, "method_not_allowed");
    assert!(resp.headers.keys().all(|k| !k.as_str().starts_with("access-control-")));
    assert_eq!(resp.header("cache-control"), Some("no-store"));
}

#[tokio::test]
async fn unknown_paths_are_404_and_wrong_methods_are_405() {
    let env = TestEnv::new();
    let app = env.router();

    assert_problem(&send(&app, get("/api/v1/nope")).await, 404, "not_found");
    assert_problem(&send(&app, get("/")).await, 404, "not_found");
    assert_problem(&send(&app, get("/api/v2/health")).await, 404, "not_found");

    let delete = Request::delete("/api/v1/tasks").header("host", HOST).body(Body::empty()).expect("request");
    assert_problem(&send(&app, delete).await, 405, "method_not_allowed");
    assert_problem(&send(&app, get("/api/v1/replay")).await, 405, "method_not_allowed");
    let post_health = post_json("/api/v1/health", &json!({}));
    assert_problem(&send(&app, post_health).await, 405, "method_not_allowed");
}

#[tokio::test]
async fn invalid_ids_and_query_values_are_bad_requests() {
    let env = TestEnv::new();
    let app = env.router();
    for path in [
        "/api/v1/tasks/not-a-ulid",
        "/api/v1/tasks/not-a-ulid/events",
        "/api/v1/tasks/not-a-ulid/runs",
        "/api/v1/tasks/not-a-ulid/artifacts",
        "/api/v1/events?limit=abc",
        "/api/v1/events?limit=0",
        "/api/v1/events?after_id=-1",
        "/api/v1/events?bogus=1",
        "/api/v1/events?types=transitioned,nope",
        "/api/v1/events?task_id=xyz",
        "/api/v1/events?limit=1&limit=2",
        "/api/v1/tasks/01J9ZX5T3K8Q7W6V5R4P3N2M1H/events?after_seq=-2",
        "/api/v1/graph?depth=x",
        "/api/v1/graph?root=nope",
        "/api/v1/graph?include_terminal=maybe",
        "/api/v1/health?verbose=1",
        "/api/v1/stream?after_id=x",
    ] {
        let resp = send(&app, get(path)).await;
        assert_problem(&resp, 400, "bad_request");
    }
    let bad_last_event_id = send(&app, get_with("/api/v1/stream", &[("last-event-id", "abc")])).await;
    assert_problem(&bad_last_event_id, 400, "bad_request");
    let id = task_core::TaskId::new();
    let bad_idx = send(&app, get(&format!("/api/v1/tasks/{id}/artifacts/first"))).await;
    assert_problem(&bad_idx, 400, "bad_request");
}
