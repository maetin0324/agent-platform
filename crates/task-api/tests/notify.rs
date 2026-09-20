//! ADR-0037 D4（Phase 39）: `GET /notify` と `POST /notify/test`。
//!
//! 実際の送信は celeris 側（`AdminRequest::NotifyTest`）なので、ここで見るのは task-api だけで
//! 完結する部分: 認証ガード（管理系は `token_file` の有無にかかわらず 401）、秘密が無いときの
//! 409 `notify_unavailable`、委譲できたときの 200、そして **`GET /notify` に URL が出ないこと**。

mod common;

use common::*;
use task_api::{AdminRequest, NotifyAdminError, NotifyTestOutcome};
use task_core::notify::{NotificationKind, NotificationStore};
use time::OffsetDateTime;
use tokio::sync::mpsc;

/// 偽の webhook URL（秘密ファイルに書く値。応答に出てはいけない）。
const WEBHOOK_URL: &str = "https://discord.invalid/api/webhooks/123/super-secret-token";

fn auth() -> String {
    format!("Bearer {TOKEN}")
}

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(path, &[("authorization", auth().as_str())])
}

fn p(path: &str) -> axum::http::Request<axum::body::Body> {
    post_json_with(
        path,
        &serde_json::json!({}),
        &[("authorization", auth().as_str())],
    )
}

fn spawn_test_double(
    result: Result<NotifyTestOutcome, NotifyAdminError>,
) -> mpsc::Sender<AdminRequest> {
    let (tx, mut rx) = mpsc::channel::<AdminRequest>(4);
    tokio::spawn(async move {
        if let Some(AdminRequest::NotifyTest { reply }) = rx.recv().await {
            let _ = reply.send(result);
        }
    });
    tx
}

/// `[secrets] dir` を作り、`register` が true なら webhook の秘密を書く。
fn env_with_webhook(
    register: bool,
    token: Option<String>,
    admin_tx: Option<mpsc::Sender<AdminRequest>>,
) -> (TestEnv, tempfile::TempDir) {
    let secrets_tmp = tempfile::tempdir().expect("tempdir");
    let dir = secrets_tmp.path().join("secrets");
    std::fs::create_dir_all(&dir).expect("mkdir");
    if register {
        std::fs::write(dir.join("discord-webhook"), format!("{WEBHOOK_URL}\n")).expect("write");
    }
    let env = TestEnv::with(EnvOptions {
        token,
        secrets_dir: Some(dir),
        admin_tx,
        notify_gui_base_url: Some("http://192.168.1.103:7700".into()),
        ..Default::default()
    });
    (env, secrets_tmp)
}

// ---- 認証（両構成）----

/// 管理系はトークンを設定した構成でも、トークンを付けなければ 401（ADR-0017 M3）。
#[tokio::test]
async fn the_test_endpoint_requires_a_token() {
    let (env, _tmp) = env_with_webhook(true, Some(TOKEN.into()), None);
    let app = env.router();
    let resp = send(
        &app,
        post_json("/api/v1/notify/test", &serde_json::json!({})),
    )
    .await;
    assert_problem(&resp, 401, "unauthorized");
}

/// `token_file` 未設定（loopback だけの構成）でも管理系は 401。
#[tokio::test]
async fn the_test_endpoint_requires_a_token_when_token_file_is_not_configured() {
    let (env, _tmp) = env_with_webhook(true, None, None);
    let app = env.router();
    let resp = send(
        &app,
        post_json("/api/v1/notify/test", &serde_json::json!({})),
    )
    .await;
    assert_problem(&resp, 401, "unauthorized");
    // 読み取りの `GET /notify` は通常の認証だけ（トークン未設定なら誰でも読める）。
    let resp = send(&app, get("/api/v1/notify")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
}

// ---- 409 / 200 ----

#[tokio::test]
async fn the_test_endpoint_is_409_when_no_webhook_is_registered() {
    let (env, _tmp) = env_with_webhook(false, Some(TOKEN.into()), None);
    let app = env.router();
    let resp = send(&app, p("/api/v1/notify/test")).await;
    let problem = assert_problem(&resp, 409, "notify_unavailable");
    assert!(
        problem["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("discord-webhook"),
        "{problem}"
    );
}

/// `[secrets]` 自体が無い構成でも 409（500 にはしない）。
#[tokio::test]
async fn the_test_endpoint_is_409_without_a_secrets_section() {
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    });
    let app = env.router();
    let resp = send(&app, p("/api/v1/notify/test")).await;
    assert_problem(&resp, 409, "notify_unavailable");
}

#[tokio::test]
async fn the_test_endpoint_returns_200_with_the_outcome_from_celeris() {
    let admin_tx = spawn_test_double(Ok(NotifyTestOutcome {
        ok: true,
        detail: Some("the test message was delivered".into()),
    }));
    let (env, _tmp) = env_with_webhook(true, Some(TOKEN.into()), Some(admin_tx));
    let app = env.router();
    let resp = send(&app, p("/api/v1/notify/test")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["detail"], "the test message was delivered");
    assert!(
        !resp.text().contains("discord.invalid"),
        "URL must not leak"
    );
}

/// 送れなかったときも 200（`ok = false`）。理由に URL は入らない。
#[tokio::test]
async fn a_failed_send_is_reported_as_ok_false() {
    let admin_tx = spawn_test_double(Ok(NotifyTestOutcome {
        ok: false,
        detail: Some("http status 404".into()),
    }));
    let (env, _tmp) = env_with_webhook(true, Some(TOKEN.into()), Some(admin_tx));
    let app = env.router();
    let resp = send(&app, p("/api/v1/notify/test")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["ok"], false);
    assert_eq!(body["detail"], "http status 404");
}

/// celeris 側が「送れない」と言ったら 409。
#[tokio::test]
async fn celeris_side_unavailability_becomes_409() {
    let admin_tx = spawn_test_double(Err(NotifyAdminError::Unavailable(
        "no Discord webhook is registered".into(),
    )));
    let (env, _tmp) = env_with_webhook(true, Some(TOKEN.into()), Some(admin_tx));
    let app = env.router();
    let resp = send(&app, p("/api/v1/notify/test")).await;
    assert_problem(&resp, 409, "notify_unavailable");
}

/// celeris に委譲できない構成（`admin_tx` が無い）は 409。
#[tokio::test]
async fn the_test_endpoint_is_409_without_an_admin_channel() {
    let (env, _tmp) = env_with_webhook(true, Some(TOKEN.into()), None);
    let app = env.router();
    let resp = send(&app, p("/api/v1/notify/test")).await;
    assert_problem(&resp, 409, "notify_unavailable");
}

// ---- GET /notify ----

#[tokio::test]
async fn get_notify_shows_the_fingerprint_and_recent_sends_but_never_the_url() {
    let (env, _tmp) = env_with_webhook(true, Some(TOKEN.into()), None);
    let now = OffsetDateTime::now_utc();
    let project_id = task_core::ProjectId::new();
    let row = env
        .store
        .notification_upsert_pending(
            NotificationKind::MilestoneReady,
            "01ABC",
            "途中目標『x』の仕事が終わりました",
            Some(project_id),
            now,
        )
        .expect("upsert")
        .expect("row");
    env.store
        .notification_mark(row.id, Some(true), None, now)
        .expect("mark");
    let app = env.router();

    let resp = send(&app, g("/api/v1/notify")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["configured"], true);
    assert_eq!(body["secret_id"], "discord-webhook");
    assert_eq!(body["gui_base_url"], "http://192.168.1.103:7700");
    let fingerprint = body["fingerprint"].as_str().expect("fingerprint");
    assert_eq!(fingerprint.len(), 8);
    assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));

    let recent = body["recent"].as_array().expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0]["kind"], "milestone_ready");
    assert_eq!(recent[0]["key"], "01ABC");
    assert_eq!(recent[0]["ok"], true);
    assert_eq!(recent[0]["attempts"], 1);
    assert!(recent[0]["sent_at"].is_string());
    // GUI 依頼 G13i-P1（ADR-0037 D6）: `milestone_ready` は途中目標の案件 id を運ぶ。
    assert_eq!(recent[0]["project_id"], project_id.to_string());

    // 値そのものは絶対に出ない（URL、トークン部分、どちらも）。
    let text = resp.text();
    assert!(!text.contains(WEBHOOK_URL), "{text}");
    assert!(!text.contains("discord.invalid"), "{text}");
    assert!(!text.contains("super-secret-token"), "{text}");
}

/// GUI 依頼 G13i-P1（ADR-0037 D6）: `project_id` が無い種（`bad_news` など）は応答に `project_id`
/// を出さない（`null` 相当。GUI はリンクを作らない）。
#[tokio::test]
async fn get_notify_omits_project_id_for_kinds_without_a_project() {
    let (env, _tmp) = env_with_webhook(true, Some(TOKEN.into()), None);
    let now = OffsetDateTime::now_utc();
    env.store
        .notification_upsert_pending(
            NotificationKind::BadNews,
            "r1",
            "悪い知らせ: テスト",
            None,
            now,
        )
        .expect("upsert");
    let app = env.router();

    let resp = send(&app, g("/api/v1/notify")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    let recent = body["recent"].as_array().expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0]["kind"], "bad_news");
    assert!(
        recent[0].get("project_id").is_none() || recent[0]["project_id"].is_null(),
        "{recent:?}"
    );
}

#[tokio::test]
async fn get_notify_reports_not_configured_without_a_secret() {
    let (env, _tmp) = env_with_webhook(false, Some(TOKEN.into()), None);
    let app = env.router();
    let resp = send(&app, g("/api/v1/notify")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["configured"], false);
    assert_eq!(body["secret_id"], "discord-webhook");
    assert!(body.get("fingerprint").is_none() || body["fingerprint"].is_null());
    assert_eq!(body["recent"].as_array().map(Vec::len), Some(0));
}
