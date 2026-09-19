//! ADR-0027 D1（Phase 16 の受け入れ 1）: `POST /tasks` の `genre` の検証（422）と正常系、`GET /tasks?genre=`
//! の絞り込み、`GET /config` の `genres[]`。API は常に完全な設定を持つので、`[[genres]]` が 1 件でも
//! あれば知らない `genre` / `genre` と `role` の不整合は常に 422（taskctl の「`--config` 無し」の緩さは無い）。

mod common;

use common::*;
use serde_json::json;
use task_core::{GenreSpec, RoleSpec, TaskStore, Tier};

fn env_with_coding_genre() -> TestEnv {
    TestEnv::with(EnvOptions {
        // ADR-0044 §5 Phase 53 追記（Phase 55）: `POST /tasks` は管理系（bearer 必須）。
        token: Some(TOKEN.to_string()),
        roles: vec![
            RoleSpec {
                id: "lead".into(),
                tier: Some(Tier::Frontier),
                adapter: None,
                max_turns: Some(40),
                max_wall_secs: None,
                instructions: Some("You lead the work.".into()),
            },
            RoleSpec {
                id: "implementer".into(),
                tier: Some(Tier::Cheap),
                adapter: Some("fake".into()),
                max_turns: None,
                max_wall_secs: Some(900),
                instructions: None,
            },
        ],
        genres: vec![GenreSpec {
            id: "coding".into(),
            description: "write and fix code".into(),
            default_role: Some("implementer".into()),
            roles: vec!["lead".into(), "implementer".into()],
            ..GenreSpec::default()
        }],
        ..EnvOptions::default()
    })
}

fn task_body(genre: Option<&str>, role: Option<&str>) -> serde_json::Value {
    let mut body = json!({
        "title": "do the thing",
        "objective": "make it work",
        "acceptance": [{"type": "human", "text": "it works"}],
    });
    if let Some(g) = genre {
        body["genre"] = json!(g);
    }
    if let Some(r) = role {
        body["role"] = json!(r);
    }
    body
}

/// 受け入れ 1: 知らない `genre` は 422（`[[genres]]` が設定されているとき常に検証する）。
#[tokio::test]
async fn create_task_with_unknown_genre_is_422() {
    let env = env_with_coding_genre();
    let app = env.router();

    let resp = send(&app, post_admin("/api/v1/tasks", &task_body(Some("literature"), None))).await;
    let problem = assert_problem(&resp, 422, "validation");
    assert!(
        problem["detail"].as_str().expect("detail").contains("literature"),
        "{problem}"
    );
    assert!(env.store.list(None).expect("list").is_empty(), "nothing must be inserted");
}

/// 受け入れ 1: `genre` はあるが `role` がその分野の `roles` に無ければ 422。
#[tokio::test]
async fn create_task_with_role_not_in_genre_is_422() {
    let env = env_with_coding_genre();
    let app = env.router();

    let resp = send(&app, post_admin("/api/v1/tasks", &task_body(Some("coding"), Some("literature-scout")))).await;
    let problem = assert_problem(&resp, 422, "validation");
    assert!(
        problem["detail"].as_str().expect("detail").contains("literature-scout"),
        "{problem}"
    );
    assert!(env.store.list(None).expect("list").is_empty(), "nothing must be inserted");
}

/// 受け入れ 1/2: 有効な `genre` は 201 になり、分野の `default_role` の既定（役割は付かない）が効く。
#[tokio::test]
async fn create_task_with_valid_genre_returns_201_and_applies_genre_defaults() {
    let env = env_with_coding_genre();
    let app = env.router();

    let resp = send(&app, post_admin("/api/v1/tasks", &task_body(Some("coding"), None))).await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let task = resp.json();
    assert_eq!(task["genre"], "coding");
    assert!(task.get("role").is_none(), "genre alone must not set role: {task}");
    // `coding` の `default_role` は `implementer`（tier=cheap, adapter=fake, max_wall_secs=900）。
    assert_eq!(task["worker_hint"], json!({"tier": "cheap", "adapter": "fake"}));
    assert_eq!(task["budget"], json!({"max_turns": 10, "max_wall_secs": 900, "max_retries": 2}));

    let id = task["id"].as_str().expect("id").to_string();
    let resp = send(&app, get_admin(&format!("/api/v1/tasks/{id}"))).await;
    assert_eq!(resp.json()["task"]["genre"], "coding");
}

/// 受け入れ 1: `genre` + 分野の `roles` に含まれる `role` は 201。
#[tokio::test]
async fn create_task_with_genre_and_matching_role_returns_201() {
    let env = env_with_coding_genre();
    let app = env.router();

    let resp = send(&app, post_admin("/api/v1/tasks", &task_body(Some("coding"), Some("lead")))).await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let task = resp.json();
    assert_eq!(task["genre"], "coding");
    assert_eq!(task["role"], "lead");
}

/// `GET /tasks?genre=` は完全一致で絞り込む（`kind=` と同じ形）。
#[tokio::test]
async fn list_tasks_filters_by_genre() {
    let env = env_with_coding_genre();
    let app = env.router();

    let coding = send(&app, post_admin("/api/v1/tasks", &task_body(Some("coding"), None))).await.json();
    let plain = send(&app, post_admin("/api/v1/tasks", &task_body(None, None))).await.json();

    let resp = send(&app, get_admin("/api/v1/tasks?genre=coding")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let list = resp.json();
    let ids: Vec<&str> = list["items"].as_array().expect("items").iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec![coding["id"].as_str().unwrap()]);
    assert!(!ids.contains(&plain["id"].as_str().unwrap()));
}

/// `GET /config` に `genres[]` が出る（taskd 側の要約と同じ形）。
#[tokio::test]
async fn config_shows_genres() {
    let env = env_with_coding_genre();
    let app = env.router();

    let resp = send(&app, get_admin("/api/v1/config")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let config = resp.json();
    // ADR-0027 D1: `config_view()` のテストヘルパの固定値（`GET /config` は taskd 起動時に作った
    // `ConfigView` をそのまま返すので、`EnvOptions.genres` とは独立: taskd 側の実装は
    // `crates/taskd/src/lib.rs` の `config_view` を見ること）。
    assert_eq!(
        config["genres"],
        json!([{
            "id": "coding",
            "description": "write and fix code",
            "default_role": "lead",
            "roles": ["lead"],
        }])
    );
}
