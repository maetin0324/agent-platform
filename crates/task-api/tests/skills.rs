//! Phase 82（ADR-0056 D3 続き）: `GET/PUT/DELETE /skills…`、`POST/DELETE /org/{id}/skills…`。
//!
//! 見るもの: 初期化していない KB（`initialized: false`）、`PUT` の作成・更新・検証違反（422）、
//! `mounted_by` が継承も含めて出ること、mount / unmount が celeris-mcp と同じ
//! `task_ops::knowledge::set_skill_mount` を通ること、mount されている skill は消せない（409
//! `skill_mounted`）、404（`skill_not_found` / `org_node_not_found`）、管理系の 401。
//!
//! **実ホームの `~/knowledge` には絶対に触らない**（`TestEnv` が tempdir の中に KB を作る）。
//! 外部ネットワークにも出ない（CLAUDE.md）。

mod common;

use common::*;
use serde_json::{Value, json};

const SAMPLE_SKILL_MD: &str = "---\nname: rust-review\ndescription: Rust のコードレビューの手順\n---\n\n# rust-review\n\n手順...\n";

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(
        path,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn pu(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    put_json_with(
        path,
        body,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn p(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    post_json_with(
        path,
        body,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn d(path: &str) -> axum::http::Request<axum::body::Body> {
    delete_with(
        path,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn env() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..EnvOptions::default()
    })
}

async fn seed_org(app: &axum::Router) {
    for body in [
        json!({"id": "secretary", "name": "秘書", "kind": "secretary", "brief": "案件を受け取る"}),
        json!({"id": "coding", "name": "コーディング部", "kind": "department", "parent_id": "secretary"}),
        json!({"id": "coding-poc", "name": "PoC 課", "kind": "section", "parent_id": "coding"}),
    ] {
        let resp = send(
            app,
            post_json_with(
                "/api/v1/org",
                &body,
                &[("authorization", format!("Bearer {TOKEN}").as_str())],
            ),
        )
        .await;
        assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    }
}

/// KB がまだ無いときは何も作らず `initialized: false`。GUI が「まだ何も無い」と出せる形。
#[tokio::test]
async fn an_uninitialized_knowledge_base_lists_no_skills() {
    let env = env();
    let app = env.router();

    let list = send(&app, g("/api/v1/skills")).await;
    assert_eq!(list.status.as_u16(), 200, "{}", list.text());
    assert_eq!(list.json()["initialized"], false);
    assert_eq!(list.json()["items"].as_array().map(Vec::len), Some(0));
    assert!(
        !env.knowledge_root.exists(),
        "読み取りが KB を作ってはいけない"
    );
}

/// `PUT /skills/{name}` は作成・更新の両方に使え、`GET /skills` / `GET /skills/{name}` に反映される。
/// 管理系なのでトークン必須。
#[tokio::test]
async fn put_skill_creates_and_updates_and_requires_admin() {
    let env = env();
    task_ops::knowledge::init(&env.knowledge_root).expect("knowledge init");
    let app = env.router();

    // トークンが無ければ 401。
    let unauth = send(
        &app,
        axum::http::Request::put("/api/v1/skills/rust-review")
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"skill_md": SAMPLE_SKILL_MD}).to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_problem(&unauth, 401, "unauthorized");

    let put = send(
        &app,
        pu(
            "/api/v1/skills/rust-review",
            &json!({"skill_md": SAMPLE_SKILL_MD}),
        ),
    )
    .await;
    assert_eq!(put.status.as_u16(), 200, "{}", put.text());
    assert_eq!(put.json()["path"], "skills/rust-review/SKILL.md");

    let list = send(&app, g("/api/v1/skills")).await;
    assert_eq!(list.json()["initialized"], true);
    let items = list.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "rust-review");
    assert_eq!(items[0]["description"], "Rust のコードレビューの手順");
    assert!(
        items[0]["mounted_by"].as_array().is_none_or(Vec::is_empty),
        "{items:?}"
    );
    assert!(items[0]["updated"].is_string(), "{items:?}");

    let detail = send(&app, g("/api/v1/skills/rust-review")).await;
    assert_eq!(detail.status.as_u16(), 200, "{}", detail.text());
    assert!(
        detail.json()["skill_md"]
            .as_str()
            .unwrap_or_default()
            .contains("rust-review")
    );

    // 名前の不一致は 422 `validation`。
    let mismatch = send(
        &app,
        pu(
            "/api/v1/skills/rust-review",
            &json!({"skill_md": "---\nname: other\ndescription: x\n---\n"}),
        ),
    )
    .await;
    assert_problem(&mismatch, 422, "validation");

    // 知らない skill は 404。
    let missing = send(&app, g("/api/v1/skills/does-not-exist")).await;
    assert_problem(&missing, 404, "skill_not_found");
}

/// `mounted_by` は継承も含めて出る（親で mount すれば子にも現れる）。mount / unmount は
/// celeris-mcp の `org_mount_skill`/`org_unmount_skill` と同じ `task_ops::knowledge::set_skill_mount` を
/// 通るので、重複を足さず、外すと消える。
#[tokio::test]
async fn mounting_a_skill_on_a_node_shows_up_in_mounted_by_including_children() {
    let env = env();
    task_ops::knowledge::init(&env.knowledge_root).expect("knowledge init");
    let app = env.router();
    seed_org(&app).await;

    send(
        &app,
        pu(
            "/api/v1/skills/rust-review",
            &json!({"skill_md": SAMPLE_SKILL_MD}),
        ),
    )
    .await;

    let mount = send(
        &app,
        p(
            "/api/v1/org/coding/skills",
            &json!({"skill": "rust-review"}),
        ),
    )
    .await;
    assert_eq!(mount.status.as_u16(), 200, "{}", mount.text());
    assert_eq!(
        mount.json()["profile"]["skills_mounts"],
        json!(["rust-review"]),
        "{}",
        mount.text()
    );

    // 冪等: もう一度 mount しても重複しない。
    let mount_again = send(
        &app,
        p(
            "/api/v1/org/coding/skills",
            &json!({"skill": "rust-review"}),
        ),
    )
    .await;
    assert_eq!(
        mount_again.json()["profile"]["skills_mounts"],
        json!(["rust-review"])
    );

    let list = send(&app, g("/api/v1/skills")).await;
    let mounted_by: Vec<String> = list.json()["items"][0]["mounted_by"]
        .as_array()
        .expect("mounted_by")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(mounted_by.contains(&"coding".to_string()), "{mounted_by:?}");
    assert!(
        mounted_by.contains(&"coding-poc".to_string()),
        "継いだ子にも出る: {mounted_by:?}"
    );
    assert!(!mounted_by.contains(&"secretary".to_string()));

    // mount されている間は消せない。
    let refused = send(&app, d("/api/v1/skills/rust-review")).await;
    assert_problem(&refused, 409, "skill_mounted");

    let unmount = send(&app, d("/api/v1/org/coding/skills/rust-review")).await;
    assert_eq!(unmount.status.as_u16(), 200, "{}", unmount.text());
    assert!(
        unmount.json()["profile"]["skills_mounts"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{}",
        unmount.text()
    );

    let list2 = send(&app, g("/api/v1/skills")).await;
    assert!(
        list2.json()["items"][0]["mounted_by"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{}",
        list2.text()
    );

    // 外れたので消せる。
    let deleted = send(&app, d("/api/v1/skills/rust-review")).await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.text());
    let gone = send(&app, g("/api/v1/skills/rust-review")).await;
    assert_problem(&gone, 404, "skill_not_found");
}

/// 知らないノードへの mount/unmount は 404。不正な skill 名は 422。すべて管理系（401）。
#[tokio::test]
async fn mount_validates_the_node_and_the_skill_name_and_requires_admin() {
    let env = env();
    task_ops::knowledge::init(&env.knowledge_root).expect("knowledge init");
    let app = env.router();
    seed_org(&app).await;

    let unknown_node = send(
        &app,
        p(
            "/api/v1/org/does-not-exist/skills",
            &json!({"skill": "rust-review"}),
        ),
    )
    .await;
    assert_problem(&unknown_node, 404, "org_node_not_found");

    let bad_name = send(
        &app,
        p("/api/v1/org/coding/skills", &json!({"skill": "Not Valid"})),
    )
    .await;
    assert_problem(&bad_name, 422, "validation");

    let unauth = send(
        &app,
        axum::http::Request::post("/api/v1/org/coding/skills")
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"skill": "rust-review"}).to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_problem(&unauth, 401, "unauthorized");

    let unmount_unauth = send(
        &app,
        axum::http::Request::delete("/api/v1/org/coding/skills/rust-review")
            .header("host", "localhost")
            .body(axum::body::Body::empty())
            .expect("request"),
    )
    .await;
    assert_problem(&unmount_unauth, 401, "unauthorized");
}
