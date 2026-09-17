//! ADR-0033 D3（Phase 25）: `GET /reports`・`GET /reports/{id}`・`POST /reports/read`・`POST /reports/notified`。
//!
//! 見るもの: 一覧の絞り込みと並び、`sources` の展開、管理系の 401（トークン未設定の構成でも）、
//! 404、`GET /daemon` に載る未読の件数と通知の判定。

mod common;

use common::*;
use serde_json::json;
use task_core::report::{Report, ReportId, ReportKind, ReportStore};
use task_core::{ProjectId, SqliteStore};
use time::OffsetDateTime;

fn auth_header() -> String {
    format!("Bearer {TOKEN}")
}

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(path, &[("authorization", auth_header().as_str())])
}

fn p(path: &str, body: &serde_json::Value) -> axum::http::Request<axum::body::Body> {
    post_json_with(path, body, &[("authorization", auth_header().as_str())])
}

fn env_with_token() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    })
}

#[allow(clippy::too_many_arguments)]
fn report(
    store: &SqliteStore,
    node_id: &str,
    level: u32,
    kind: ReportKind,
    project: Option<ProjectId>,
    headline: &str,
    at: OffsetDateTime,
    sources: Vec<ReportId>,
) -> Report {
    let report = Report {
        id: ReportId::new(),
        project_id: project,
        node_id: node_id.into(),
        task_id: None,
        kind,
        level,
        headline: headline.into(),
        body: format!("{headline} の本文"),
        sources,
        read_at: None,
        created_at: at,
    };
    store.report_append(&report).expect("append");
    report
}

#[tokio::test]
async fn reports_are_listed_newest_first_and_can_be_filtered() {
    let env = env_with_token();
    let app = env.router();
    let now = OffsetDateTime::now_utc();
    let project = ProjectId::new();
    let other = ProjectId::new();

    let old = report(
        &env.store,
        "coding-poc",
        2,
        ReportKind::Result,
        Some(project),
        "古い結果",
        now - time::Duration::hours(2),
        vec![],
    );
    let new = report(
        &env.store,
        "coding-poc",
        2,
        ReportKind::Result,
        Some(project),
        "新しい結果",
        now - time::Duration::minutes(1),
        vec![],
    );
    let top = report(
        &env.store,
        "secretary",
        0,
        ReportKind::BadNews,
        Some(other),
        "落ちました",
        now - time::Duration::minutes(30),
        vec![old.id],
    );

    let resp = send(&app, g("/api/v1/reports")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["headline"], "新しい結果", "新しい順");
    assert_eq!(items[2]["headline"], "古い結果");

    // 案件で絞る。
    let resp = send(&app, g(&format!("/api/v1/reports?project={project}"))).await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 2);

    // ノードで絞る。
    let resp = send(&app, g("/api/v1/reports?node=secretary")).await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], top.id.to_string());

    // level と未読で絞る（秘書レベルの未読 = 人が見る報告）。
    let resp = send(&app, g("/api/v1/reports?level=0&unread=true")).await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "bad_news");

    // limit。
    let resp = send(&app, g("/api/v1/reports?limit=1")).await;
    assert_eq!(resp.json()["items"].as_array().map(Vec::len), Some(1));

    // 知らないクエリは 400。
    let resp = send(&app, g("/api/v1/reports?nope=1")).await;
    assert_eq!(resp.status.as_u16(), 400);

    // 1 件取得は sources を展開する。
    let resp = send(&app, g(&format!("/api/v1/reports/{}", top.id))).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["report"]["headline"], "落ちました");
    let expanded = body["sources_expanded"].as_array().cloned().expect("sources");
    assert_eq!(expanded.len(), 1);
    assert_eq!(expanded[0]["id"], old.id.to_string());
    assert_eq!(new.sources.len(), 0);

    // 無い id は 404（ULID でない文字列も）。
    let resp = send(&app, g(&format!("/api/v1/reports/{}", ReportId::new()))).await;
    assert_eq!(resp.status.as_u16(), 404);
    assert_eq!(resp.json()["code"], "report_not_found");
    let resp = send(&app, g("/api/v1/reports/not-a-ulid")).await;
    assert_eq!(resp.status.as_u16(), 404);
}

#[tokio::test]
async fn marking_reports_read_is_an_admin_operation() {
    let env = env_with_token();
    let app = env.router();
    let now = OffsetDateTime::now_utc();
    let one = report(&env.store, "secretary", 0, ReportKind::Result, None, "まとめ", now, vec![]);

    // トークンが無ければ 401（共通ガード）。
    let resp = send(&app, post_json("/api/v1/reports/read", &json!({"ids": [one.id.to_string()]}))).await;
    assert_eq!(resp.status.as_u16(), 401);

    let resp = send(&app, p("/api/v1/reports/read", &json!({"ids": [one.id.to_string()]}))).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(resp.json()["updated"], 1);
    // 2 回目は 0 件（既読は触らない）。
    let resp = send(&app, p("/api/v1/reports/read", &json!({"ids": [one.id.to_string()]}))).await;
    assert_eq!(resp.json()["updated"], 0);

    // 知らない id は 404。
    let resp = send(&app, p("/api/v1/reports/read", &json!({"ids": ["not-a-ulid"]}))).await;
    assert_eq!(resp.status.as_u16(), 404);

    let resp = send(&app, p("/api/v1/reports/notified", &json!({}))).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert!(resp.json()["last_notified_at"].as_str().is_some());
}

/// `token_file` が無い構成でも、管理系（既読・通知）は 401（ADR-0017 D1 と同じ規律）。
#[tokio::test]
async fn admin_operations_are_401_even_without_a_configured_token() {
    let env = TestEnv::new();
    let app = env.router();
    let resp = send(&app, post_json("/api/v1/reports/read", &json!({"ids": []}))).await;
    assert_eq!(resp.status.as_u16(), 401);
    let resp = send(&app, post_json("/api/v1/reports/notified", &json!({}))).await;
    assert_eq!(resp.status.as_u16(), 401);
    // 読み取りは従来どおり通る。
    let resp = send(&app, get("/api/v1/reports")).await;
    assert_eq!(resp.status.as_u16(), 200);
}

#[tokio::test]
async fn the_daemon_snapshot_carries_the_unread_counts_and_the_notification_decision() {
    let env = TestEnv::new();
    let app = env.router();
    env.daemon_tx.send_replace(Some(snapshot(3)));
    let now = OffsetDateTime::now_utc();

    // 未読が無ければ通知しない。
    let resp = send(&app, get("/api/v1/daemon")).await;
    let reports = resp.json()["snapshot"]["reports"].clone();
    assert_eq!(reports["unread_secretary"], 0);
    assert_eq!(reports["notify_now"], false);

    // 秘書レベルの未読ができ、まだ通知したことが無ければ通知する。
    let good = report(&env.store, "secretary", 0, ReportKind::Result, None, "まとめ", now, vec![]);
    let resp = send(&app, get("/api/v1/daemon")).await;
    let reports = resp.json()["snapshot"]["reports"].clone();
    assert_eq!(reports["unread_secretary"], 1);
    assert_eq!(reports["unread_bad_news"], 0);
    assert_eq!(reports["notify_now"], true);

    // 通知時刻は管理系でしか進められないので、トークンのある環境で確かめる。
    let env2 = env_with_token();
    let app2 = env2.router();
    env2.daemon_tx.send_replace(Some(snapshot(3)));
    let unread = report(&env2.store, "secretary", 0, ReportKind::Result, None, "まとめ", now, vec![]);
    let resp = send(&app2, p("/api/v1/reports/notified", &json!({}))).await;
    assert_eq!(resp.status.as_u16(), 200);
    let resp = send(&app2, g("/api/v1/daemon")).await;
    let reports = resp.json()["snapshot"]["reports"].clone();
    assert_eq!(reports["unread_secretary"], 1);
    assert_eq!(reports["notify_now"], false, "2 時間未満は通知しない");
    assert!(reports["last_notified_at"].as_str().is_some());

    // 悪い知らせは 2 時間を待たずに通知する。
    report(
        &env2.store,
        "secretary",
        0,
        ReportKind::BadNews,
        None,
        "ノードが落ちました",
        now,
        vec![],
    );
    let resp = send(&app2, g("/api/v1/daemon")).await;
    let reports = resp.json()["snapshot"]["reports"].clone();
    assert_eq!(reports["unread_bad_news"], 1);
    assert_eq!(reports["notify_now"], true);
    assert_eq!(good.level, 0);
    assert_eq!(unread.level, 0);
}
