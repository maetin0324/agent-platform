//! ADR-0032 D5（クラスタ接続の管理 API 3 本）: `POST /clusters/{id}/connect`、
//! `POST /clusters/{id}/connect/code`、`DELETE /clusters/{id}/connect`。実際の ssh 起動は taskd 側
//! （`AdminRequest::ClusterConnect*`）が行うので、ここでは task-api だけで完結する部分を確認する:
//! 認証ガード（管理系はすべて token 必須。`token_file` 未設定でも 401）、未知の cluster id の 404、
//! `ClusterAdminError` → HTTP の写像、コードの検証（空・空白・制御文字は 422 で `admin_tx` に届かない）、
//! 型違いの本文で値が反射しないこと、`GET /clusters` の `auth`/`connect_pending`（`prompt` は出ない）。
//! `tests/common::config_view()` は `id = "pegasus"`（`auth = "manual"`）のクラスタを 1 つ持つ。

mod common;

use common::*;
use serde_json::json;
use task_api::{AdminRequest, ClusterAdminError, ClusterConnectCodeOutcome, ClusterConnectStartOutcome};
use tokio::sync::mpsc;

const CLUSTER: &str = "pegasus";

fn auth() -> String {
    format!("Bearer {TOKEN}")
}

fn env_with_admin(admin_tx: mpsc::Sender<AdminRequest>) -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        admin_tx: Some(admin_tx),
        ..Default::default()
    })
}

fn spawn_start_double(result: Result<ClusterConnectStartOutcome, ClusterAdminError>) -> mpsc::Sender<AdminRequest> {
    let (tx, mut rx) = mpsc::channel::<AdminRequest>(4);
    tokio::spawn(async move {
        if let Some(AdminRequest::ClusterConnectStart { reply, .. }) = rx.recv().await {
            let _ = reply.send(result);
        }
    });
    tx
}

fn spawn_code_double(result: Result<ClusterConnectCodeOutcome, ClusterAdminError>) -> mpsc::Sender<AdminRequest> {
    let (tx, mut rx) = mpsc::channel::<AdminRequest>(4);
    tokio::spawn(async move {
        if let Some(AdminRequest::ClusterConnectCode { reply, .. }) = rx.recv().await {
            let _ = reply.send(result);
        }
    });
    tx
}

fn spawn_cancel_double(result: Result<(), ClusterAdminError>) -> mpsc::Sender<AdminRequest> {
    let (tx, mut rx) = mpsc::channel::<AdminRequest>(4);
    tokio::spawn(async move {
        if let Some(AdminRequest::ClusterConnectCancel { reply, .. }) = rx.recv().await {
            let _ = reply.send(result);
        }
    });
    tx
}

/// 管理系エンドポイントは `token_file` を設定していても、トークンを付けなければ 401（ADR-0017 M3）。
#[tokio::test]
async fn management_routes_all_require_a_token() {
    let env = TestEnv::with(EnvOptions { token: Some(TOKEN.into()), ..Default::default() });
    let app = env.router();

    let resp = send(&app, post_json(&format!("/api/v1/clusters/{CLUSTER}/connect"), &json!({}))).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(
        &app,
        post_json(&format!("/api/v1/clusters/{CLUSTER}/connect/code"), &json!({"code": "123456"})),
    )
    .await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, delete_with(&format!("/api/v1/clusters/{CLUSTER}/connect"), &[])).await;
    assert_problem(&resp, 401, "unauthorized");
}

/// ADR-0017 M3: `token_file` が**無くても**（loopback だけの構成でも）管理系は 401。`GET /clusters` は
/// 読み取りなので token 不要（`accounts_admin.rs`/`secrets_admin.rs` と同じ形の回帰テスト）。
#[tokio::test]
async fn management_routes_require_a_token_even_when_token_file_is_not_configured() {
    let env = TestEnv::with(EnvOptions { token: None, ..Default::default() });
    let app = env.router();

    let resp = send(&app, post_json(&format!("/api/v1/clusters/{CLUSTER}/connect"), &json!({}))).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(
        &app,
        post_json(&format!("/api/v1/clusters/{CLUSTER}/connect/code"), &json!({"code": "123456"})),
    )
    .await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, delete_with(&format!("/api/v1/clusters/{CLUSTER}/connect"), &[])).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, get("/api/v1/clusters")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
}

/// 未知の cluster id はすべて 404 `cluster_not_found`（`admin_tx` は無くても判定できる: `[[clusters]]`
/// に無い id は taskd に問い合わせる前に弾く）。
#[tokio::test]
async fn unknown_cluster_id_is_404_on_all_three_endpoints() {
    let env = TestEnv::with(EnvOptions { token: Some(TOKEN.into()), ..Default::default() });
    let app = env.router();
    let auth = auth();

    let resp = send(
        &app,
        post_json_with("/api/v1/clusters/does-not-exist/connect", &json!({}), &[("authorization", &auth)]),
    )
    .await;
    assert_problem(&resp, 404, "cluster_not_found");

    let resp = send(
        &app,
        post_json_with(
            "/api/v1/clusters/does-not-exist/connect/code",
            &json!({"code": "123456"}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_problem(&resp, 404, "cluster_not_found");

    let resp = send(&app, delete_with("/api/v1/clusters/does-not-exist/connect", &[("authorization", &auth)])).await;
    assert_problem(&resp, 404, "cluster_not_found");
}

/// `ClusterAdminError::NotSupported`（`auth = "manual"` に `connect` した）は 409 `cluster_connect_not_supported`。
#[tokio::test]
async fn connect_maps_not_supported_to_409() {
    let admin_tx = spawn_start_double(Err(ClusterAdminError::NotSupported));
    let env = env_with_admin(admin_tx);
    let app = env.router();

    let resp = send(
        &app,
        post_json_with(&format!("/api/v1/clusters/{CLUSTER}/connect"), &json!({}), &[("authorization", &auth())]),
    )
    .await;
    assert_problem(&resp, 409, "cluster_connect_not_supported");
}

/// `ClusterAdminError::NotStarted`（進行中のセッションが無いのに code を送った）は 409 `cluster_connect_not_started`。
#[tokio::test]
async fn connect_code_maps_not_started_to_409() {
    let admin_tx = spawn_code_double(Err(ClusterAdminError::NotStarted));
    let env = env_with_admin(admin_tx);
    let app = env.router();

    let resp = send(
        &app,
        post_json_with(
            &format!("/api/v1/clusters/{CLUSTER}/connect/code"),
            &json!({"code": "000000"}),
            &[("authorization", &auth())],
        ),
    )
    .await;
    assert_problem(&resp, 409, "cluster_connect_not_started");
}

/// taskd 側が `ClusterAdminError::InvalidCode` を返した場合も 422 `validation`（task-api 自身のローカルな
/// trim/制御文字チェックを通った、形式上は妥当なコードが taskd 側で拒否されたケース）。
#[tokio::test]
async fn connect_code_maps_admin_invalid_code_to_422() {
    let admin_tx = spawn_code_double(Err(ClusterAdminError::InvalidCode));
    let env = env_with_admin(admin_tx);
    let app = env.router();

    let resp = send(
        &app,
        post_json_with(
            &format!("/api/v1/clusters/{CLUSTER}/connect/code"),
            &json!({"code": "000000"}),
            &[("authorization", &auth())],
        ),
    )
    .await;
    assert_problem(&resp, 422, "validation");
}

/// `ClusterAdminError::Failed`（接続そのものの失敗）は 502 `cluster_connect_failed`（`login_failed` と同じ扱い）。
#[tokio::test]
async fn connect_maps_failed_to_502() {
    let admin_tx = spawn_start_double(Err(ClusterAdminError::Failed("ssh: connection refused".into())));
    let env = env_with_admin(admin_tx);
    let app = env.router();

    let resp = send(
        &app,
        post_json_with(&format!("/api/v1/clusters/{CLUSTER}/connect"), &json!({}), &[("authorization", &auth())]),
    )
    .await;
    assert_problem(&resp, 502, "cluster_connect_failed");
}

/// `connect` が成功すると `kind`/`prompt`/`expires_at` がそのまま応答になる（`connected` はコード不要）。
#[tokio::test]
async fn connect_returns_connected_without_a_code() {
    let admin_tx = spawn_start_double(Ok(ClusterConnectStartOutcome {
        kind: "connected".to_string(),
        prompt: None,
        expires_at_unix: None,
    }));
    let env = env_with_admin(admin_tx);
    let app = env.router();

    let resp = send(
        &app,
        post_json_with(&format!("/api/v1/clusters/{CLUSTER}/connect"), &json!({}), &[("authorization", &auth())]),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let v = resp.json();
    assert_eq!(v["kind"], json!("connected"));
    assert!(v.get("prompt").is_none());
}

/// `connect` が `needs_code` を返すとプロンプトと期限が応答に載る。
#[tokio::test]
async fn connect_returns_needs_code_with_prompt_and_expiry() {
    let admin_tx = spawn_start_double(Ok(ClusterConnectStartOutcome {
        kind: "needs_code".to_string(),
        prompt: Some("(rmaeda@130.158.241.2) Verification code: ".to_string()),
        expires_at_unix: Some(2_000_000_000),
    }));
    let env = env_with_admin(admin_tx);
    let app = env.router();

    let resp = send(
        &app,
        post_json_with(&format!("/api/v1/clusters/{CLUSTER}/connect"), &json!({}), &[("authorization", &auth())]),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let v = resp.json();
    assert_eq!(v["kind"], json!("needs_code"));
    assert_eq!(v["prompt"], json!("(rmaeda@130.158.241.2) Verification code: "));
    assert!(v["expires_at"].is_string());
}

/// 空・空白だけ・制御文字入りのコードは 422 `validation` で、`admin_tx` に何も送られない（trim/制御文字の
/// 検証は task-api 自身がコードを渡す前に行う）。
#[tokio::test]
async fn connect_code_rejects_blank_and_control_chars_without_reaching_admin() {
    let (admin_tx, mut admin_rx) = mpsc::channel::<AdminRequest>(4);
    tokio::spawn(async move {
        if admin_rx.recv().await.is_some() {
            panic!("an invalid code must not reach the admin channel");
        }
    });
    let env = env_with_admin(admin_tx);
    let app = env.router();
    let auth = auth();

    for bad in ["", "   ", "12\n34", "12\t34", "\u{7}bad"] {
        let resp = send(
            &app,
            post_json_with(
                &format!("/api/v1/clusters/{CLUSTER}/connect/code"),
                &json!({"code": bad}),
                &[("authorization", &auth)],
            ),
        )
        .await;
        assert_problem(&resp, 422, "validation");
    }
}

/// 型違いの本文（`{"code": 123456}`）は 400 で、serde のエラー文に値が反射しない（`/secrets` の
/// `secret_body_invalid` と同じ規律。監査指摘 D-6 相当）。`admin_tx` にも届かない。
#[tokio::test]
async fn connect_code_body_type_mismatch_does_not_reflect_the_value_and_skips_admin() {
    let (admin_tx, mut admin_rx) = mpsc::channel::<AdminRequest>(4);
    tokio::spawn(async move {
        if admin_rx.recv().await.is_some() {
            panic!("a body that fails to parse must not reach the admin channel");
        }
    });
    let env = env_with_admin(admin_tx);
    let app = env.router();
    let auth = auth();

    for body in [json!({"code": 123456}), json!({"code": ["a", "b"]}), json!({"notcode": "x"})] {
        let resp = send(
            &app,
            post_json_with(&format!("/api/v1/clusters/{CLUSTER}/connect/code"), &body, &[("authorization", &auth)]),
        )
        .await;
        assert_problem(&resp, 400, "bad_request");
        let text = resp.text();
        assert!(!text.contains("123456"), "value leaked in parse error: {text}");
    }
}

/// `connect/code` の成功は `{ok, detail}` を返す（コード自体は応答に含まれない）。
#[tokio::test]
async fn connect_code_success_returns_ok_and_detail_without_the_code() {
    let admin_tx = spawn_code_double(Ok(ClusterConnectCodeOutcome { ok: true, detail: Some("connected".into()) }));
    let env = env_with_admin(admin_tx);
    let app = env.router();

    let resp = send(
        &app,
        post_json_with(
            &format!("/api/v1/clusters/{CLUSTER}/connect/code"),
            &json!({"code": "654321"}),
            &[("authorization", &auth())],
        ),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let v = resp.json();
    assert_eq!(v["ok"], json!(true));
    assert_eq!(v["detail"], json!("connected"));
    assert!(!resp.text().contains("654321"));
}

/// `DELETE /clusters/{id}/connect` の成功は空オブジェクト。
#[tokio::test]
async fn disconnect_success_returns_empty_object() {
    let admin_tx = spawn_cancel_double(Ok(()));
    let env = env_with_admin(admin_tx);
    let app = env.router();

    let resp = send(&app, delete_with(&format!("/api/v1/clusters/{CLUSTER}/connect"), &[("authorization", &auth())])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json(), json!({}));
}

/// `DELETE` も `ClusterAdminError` を同じ規則で写す（ここでは `Failed` → 502）。
#[tokio::test]
async fn disconnect_maps_failed_to_502() {
    let admin_tx = spawn_cancel_double(Err(ClusterAdminError::Failed("no such control socket".into())));
    let env = env_with_admin(admin_tx);
    let app = env.router();

    let resp = send(&app, delete_with(&format!("/api/v1/clusters/{CLUSTER}/connect"), &[("authorization", &auth())])).await;
    assert_problem(&resp, 502, "cluster_connect_failed");
}

/// `admin_tx` が配線されていない構成（`[api]` はあるが taskd への経路が無い）では、既知の id でも
/// 内部エラーとして扱う（accounts/providers の `reload`/`check` と同じ、taskd が受け取れない場合の扱い）。
#[tokio::test]
async fn connect_without_admin_tx_does_not_panic() {
    let env = TestEnv::with(EnvOptions { token: Some(TOKEN.into()), ..Default::default() });
    let app = env.router();

    let resp = send(
        &app,
        post_json_with(&format!("/api/v1/clusters/{CLUSTER}/connect"), &json!({}), &[("authorization", &auth())]),
    )
    .await;
    assert_problem(&resp, 500, "internal");
}

/// `GET /clusters` は `auth`（設定から）と `connect_pending`（スナップショットから）を返すが、`prompt` は
/// 出さない（ADR-0032 D5: プロンプト文字列は `POST` の応答にだけ載る）。
#[tokio::test]
async fn get_clusters_reports_auth_and_connect_pending_but_never_a_prompt() {
    let env = TestEnv::new();
    let app = env.router();

    let mut snap = snapshot(1);
    snap.clusters[0].connect_pending = true;
    env.daemon_tx.send(Some(snap)).expect("send snapshot");

    let resp = send(&app, get("/api/v1/clusters")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let v = resp.json();
    let items = v["items"].as_array().expect("items array");
    let pegasus = items.iter().find(|c| c["id"] == CLUSTER).expect("pegasus present");
    assert_eq!(pegasus["auth"], json!("manual"));
    assert_eq!(pegasus["connect_pending"], json!(true));
    assert!(pegasus.get("prompt").is_none());
    assert!(!resp.text().contains("prompt"), "{}", resp.text());
}

/// スナップショットが無い（最初の tick 前）と `connect_pending` は `false`。
#[tokio::test]
async fn get_clusters_connect_pending_defaults_to_false_without_a_snapshot() {
    let env = TestEnv::new();
    let app = env.router();

    let resp = send(&app, get("/api/v1/clusters")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let v = resp.json();
    let items = v["items"].as_array().expect("items array");
    let pegasus = items.iter().find(|c| c["id"] == CLUSTER).expect("pegasus present");
    assert_eq!(pegasus["auth"], json!("manual"));
    assert_eq!(pegasus["connect_pending"], json!(false));
}
