//! ADR-0040 D3 / D4（Phase 47）: `standby` / `draining` の API のふるまいと `GET /health` の
//! `release` / `mode` / `role`。
//!
//! - ディスパッチャの状態を要する管理系（`reload` / `check` / クラスタ接続 / アカウントのログイン中継 /
//!   `notify/test`）は `503 {"detail":"standby"}` に `Retry-After: 2` を付けて返す。
//! - 通常の読み書き（`GET /tasks` など）は standby でもそのまま動く（同じ DB を見ている）。
//! - `active` に戻れば同じ要求が通る（役割は `SharedRole` 経由で tick ループが書き換える）。

mod common;

use common::*;
use serde_json::json;
use task_core::{DaemonMode, InstanceRole, SharedRole};

fn auth() -> String {
    format!("Bearer {TOKEN}")
}

fn env_with_role(role: InstanceRole) -> (TestEnv, SharedRole, tokio::sync::mpsc::Receiver<task_api::AdminRequest>) {
    let shared = SharedRole::new(role);
    // `admin_tx` があるのに 503 になる（＝ 役割だけで断っている）ことを見るため、受信側も保持する。
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        admin_tx: Some(tx),
        release: "sha12sha12ab".into(),
        mode: DaemonMode::Normal,
        role: shared.clone(),
        ..Default::default()
    });
    (env, shared, rx)
}

/// (f) standby では `POST /reload` が 503 `standby` + `Retry-After: 2`。active に戻れば通る。
#[tokio::test]
async fn standby_refuses_the_dispatcher_admin_endpoints_with_503_and_retry_after() {
    let (env, role, mut admin_rx) = env_with_role(InstanceRole::Standby);
    let app = env.router();
    let auth = auth();

    let resp = send(&app, post_json_with("/api/v1/reload", &json!({}), &[("authorization", &auth)])).await;
    let problem = assert_problem(&resp, 503, "standby");
    assert_eq!(problem["detail"], json!("standby"));
    assert_eq!(resp.header("retry-after"), Some("2"));
    assert!(admin_rx.try_recv().is_err(), "503 の間はディスパッチャへ要求を送らない");

    // 引き継ぎが終わって active になれば、同じ要求が tick ループまで届く（ここでは応答を待たない）。
    role.set(InstanceRole::Active);
    let app2 = app.clone();
    let auth2 = auth.clone();
    let pending = tokio::spawn(async move {
        send(&app2, post_json_with("/api/v1/reload", &json!({}), &[("authorization", &auth2)])).await
    });
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), admin_rx.recv())
        .await
        .unwrap_or_else(|_| panic!("active になったのに reload が届かない"));
    match request {
        Some(task_api::AdminRequest::Reload { reply }) => {
            let _ = reply.send(Ok(()));
        }
        other => panic!("expected a reload request, got {}", other.is_some()),
    }
    let resp = pending.await.unwrap_or_else(|e| panic!("join: {e}"));
    assert_eq!(resp.status, 200, "{}", resp.text());
}

/// `draining` も standby と同じく 503（listener を閉じるまでの間に届いた要求）。
/// 管理系のうちディスパッチャを要するものは全部同じ扱い。
#[tokio::test]
async fn draining_refuses_the_same_endpoints_and_plain_endpoints_keep_working() {
    let (env, role, _admin_rx) = env_with_role(InstanceRole::Draining);
    let app = env.router();
    let auth = auth();

    for (method, path, body) in [
        ("POST", "/api/v1/reload", Some(json!({}))),
        ("POST", "/api/v1/providers/p1/check", Some(json!({}))),
        ("POST", "/api/v1/notify/test", Some(json!({}))),
        ("POST", "/api/v1/accounts/a/login", Some(json!({}))),
        ("POST", "/api/v1/accounts/a/login/code", Some(json!({"code": "x"}))),
        ("POST", "/api/v1/clusters/c1/connect", Some(json!({}))),
        ("POST", "/api/v1/clusters/c1/connect/code", Some(json!({"code": "x"}))),
    ] {
        assert_eq!(method, "POST");
        let body = body.unwrap_or(json!({}));
        let resp = send(&app, post_json_with(path, &body, &[("authorization", &auth)])).await;
        let problem = assert_problem(&resp, 503, "standby");
        assert_eq!(problem["detail"], json!("standby"), "{path}");
        assert_eq!(resp.header("retry-after"), Some("2"), "{path}");
    }

    // 読み取りは動く（standby / draining も同じ DB を見ている）。
    let resp = send(&app, get_with("/api/v1/tasks", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    // 認証が要る管理系でも、ディスパッチャを要さないもの（秘密の一覧）は役割で断らない。
    role.set(InstanceRole::Standby);
    let resp = send(&app, get_with("/api/v1/health", &[])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
}

/// (h) `GET /health` に `release` / `mode` / `role` が載る（`verify` と `standby` も出る）。
#[tokio::test]
async fn health_carries_the_release_the_mode_and_the_role() {
    let shared = SharedRole::new(InstanceRole::Standby);
    let env = TestEnv::with(EnvOptions {
        release: "abcdef123456".into(),
        mode: DaemonMode::Verify,
        role: shared.clone(),
        ..Default::default()
    });
    let app = env.router();

    let resp = send(&app, get("/api/v1/health")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["release"], json!("abcdef123456"));
    assert_eq!(body["mode"], json!("verify"));
    assert_eq!(body["role"], json!("standby"));
    // 既存のフィールドはそのまま。
    assert_eq!(body["api_version"], json!("1"));
    assert_eq!(body["schema_version"], json!(task_core::SCHEMA_VERSION));

    for role in InstanceRole::ALL {
        shared.set(role);
        let resp = send(&app, get("/api/v1/health")).await;
        assert_eq!(resp.json()["role"], json!(role.as_str()));
    }
}
