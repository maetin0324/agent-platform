//! GUI 監査対応 Phase 29 / H3: `GET /org/{id}/memory?project=`（読み取り、ADR-0033 D6）。
//!
//! 見るもの: `notes.md` / `projects/<project_id>.md` の全文（切らない）、無ければ空文字列、
//! `[memory]` 未設定は 409 `memory_unavailable`、未知のノードは 404、書き込み API は無いこと。

mod common;

use common::*;
use serde_json::{Value, json};

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(path, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn p(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    post_json_with(path, body, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn env_with_memory(dir: &std::path::Path) -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        memory_dir: Some(dir.to_path_buf()),
        ..Default::default()
    })
}

fn env_without_memory() -> TestEnv {
    TestEnv::with(EnvOptions { token: Some(TOKEN.into()), ..Default::default() })
}

async fn seed_secretary(app: &axum::Router) {
    let resp = send(
        app,
        p("/api/v1/org", &json!({"id": "secretary", "name": "秘書", "kind": "secretary", "brief": "案件を受け取る"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
}

#[tokio::test]
async fn missing_files_read_as_empty_and_paths_are_returned() {
    let memory_dir = tempfile::tempdir().expect("tempdir");
    let env = env_with_memory(memory_dir.path());
    let app = env.router();
    seed_secretary(&app).await;

    let resp = send(&app, g("/api/v1/org/secretary/memory")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["notes"], "");
    assert_eq!(body["project"], Value::Null);
    assert_eq!(
        body["notes_path"],
        memory_dir.path().join("secretary/notes.md").to_string_lossy().into_owned()
    );
    assert_eq!(body["project_path"], Value::Null);
}

#[tokio::test]
async fn existing_notes_and_a_projects_drawer_are_returned_in_full() {
    let memory_dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(memory_dir.path().join("secretary/projects")).expect("mkdir");
    let long = "あ".repeat(9_000);
    std::fs::write(memory_dir.path().join("secretary/notes.md"), &long).expect("write");
    std::fs::write(memory_dir.path().join("secretary/projects/P1.md"), "project notes").expect("write");
    let env = env_with_memory(memory_dir.path());
    let app = env.router();
    seed_secretary(&app).await;

    let resp = send(&app, g("/api/v1/org/secretary/memory?project=P1")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(
        body["notes"].as_str().expect("notes").chars().count(),
        9_000,
        "全文を返す（前置きの 8,000 字カットとは別）"
    );
    assert_eq!(body["project"], "project notes");
    assert_eq!(
        body["project_path"],
        memory_dir.path().join("secretary/projects/P1.md").to_string_lossy().into_owned()
    );
}

#[tokio::test]
async fn unconfigured_memory_is_409_and_an_unknown_node_is_404() {
    let env = env_without_memory();
    let app = env.router();
    seed_secretary(&app).await;

    let resp = send(&app, g("/api/v1/org/secretary/memory")).await;
    assert_eq!(resp.status.as_u16(), 409, "{}", resp.text());
    assert_eq!(resp.json()["code"], "memory_unavailable");

    let memory_dir = tempfile::tempdir().expect("tempdir");
    let env = env_with_memory(memory_dir.path());
    let app = env.router();
    seed_secretary(&app).await;
    let resp = send(&app, g("/api/v1/org/ghost/memory")).await;
    assert_eq!(resp.status.as_u16(), 404, "{}", resp.text());
    assert_eq!(resp.json()["code"], "org_node_not_found");
}

/// 書き込み API は無い（記憶はワーカーが書く。人が直したければファイルを編集する）。
#[tokio::test]
async fn there_is_no_write_endpoint() {
    let memory_dir = tempfile::tempdir().expect("tempdir");
    let env = env_with_memory(memory_dir.path());
    let app = env.router();
    seed_secretary(&app).await;

    let resp = send(&app, p("/api/v1/org/secretary/memory", &json!({"notes": "x"}))).await;
    assert_eq!(resp.status.as_u16(), 405, "{}", resp.text());
}
