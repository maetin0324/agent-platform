//! ADR-0072 D14（Phase E2）: `POST`/`GET /tasks/{id}/execution-plan`。
//!
//! 見るもの: 正常系（採用され、WorkUnit が pending/ready に分かれる）、管理系であること
//! （トークン必須）、404（タスクが無い）、422（D14 の検証エラー: 循環・重複・件数・key）、
//! 409（既に active な計画がある）。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{Status, TaskKind};

fn plan_body() -> Value {
    json!({
        "schema": "celeris.execution-plan/1",
        "rationale": "3 段階の直列計画",
        "work_units": [
            {"key": "a", "kind": "implement", "title": "A", "objective": "do A thoroughly and well"},
            {"key": "b", "kind": "implement", "title": "B", "objective": "do B thoroughly and well", "depends_on": ["a"]},
            {"key": "c", "kind": "implement", "title": "C", "objective": "do C thoroughly and well", "depends_on": ["b"]}
        ]
    })
}

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(
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

#[tokio::test]
async fn adopting_a_plan_creates_ready_and_pending_work_units() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);

    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &plan_body(),
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["version"], 1);
    assert_eq!(body["origin"], "human");
    assert_eq!(body["status"], "active");
    let work_units = body["work_units"].as_array().expect("work_units");
    assert_eq!(work_units.len(), 3);
    assert_eq!(work_units[0]["key"], "a");
    assert_eq!(work_units[0]["status"], "ready");
    assert_eq!(work_units[1]["key"], "b");
    assert_eq!(work_units[1]["status"], "pending");

    let got = send(
        &app,
        g(&format!("/api/v1/tasks/{}/execution-plan", task.id)),
    )
    .await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_eq!(got.json()["id"], body["id"]);
}

#[tokio::test]
async fn posting_without_a_token_is_unauthorized() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);

    let req = post_json_with(
        &format!("/api/v1/tasks/{}/execution-plan", task.id),
        &plan_body(),
        &[],
    );
    let resp = send(&app, req).await;
    assert_eq!(resp.status, 401, "{}", resp.text());
}

#[tokio::test]
async fn posting_to_an_unknown_task_is_not_found() {
    let env = env();
    let app = env.router();
    let missing = task_core::TaskId::new();
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{missing}/execution-plan"),
            &plan_body(),
        ),
    )
    .await;
    assert_eq!(resp.status, 404, "{}", resp.text());
}

#[tokio::test]
async fn getting_a_task_without_a_plan_is_not_found() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let resp = send(
        &app,
        g(&format!("/api/v1/tasks/{}/execution-plan", task.id)),
    )
    .await;
    assert_eq!(resp.status, 404, "{}", resp.text());
}

#[tokio::test]
async fn a_cyclic_plan_is_rejected_with_422() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let cyclic = json!({
        "schema": "celeris.execution-plan/1",
        "rationale": "bad",
        "work_units": [
            {"key": "a", "kind": "implement", "title": "A", "objective": "do A", "depends_on": ["b"]},
            {"key": "b", "kind": "implement", "title": "B", "objective": "do B", "depends_on": ["a"]}
        ]
    });
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &cyclic,
        ),
    )
    .await;
    assert_eq!(resp.status, 422, "{}", resp.text());
}

#[tokio::test]
async fn a_plan_with_an_unknown_harness_field_is_rejected_and_duplicate_keys_are_rejected() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let duplicate_keys = json!({
        "schema": "celeris.execution-plan/1",
        "rationale": "bad",
        "work_units": [
            {"key": "a", "kind": "implement", "title": "A", "objective": "do A"},
            {"key": "a", "kind": "implement", "title": "A2", "objective": "do A again"}
        ]
    });
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &duplicate_keys,
        ),
    )
    .await;
    assert_eq!(resp.status, 422, "{}", resp.text());

    // `assignee`/`tier`/`model` の欄は `deny_unknown_fields` で JSON の時点で拒否される（400。
    // D14 が要求するのは「schema 違反になる」ことで、拒否そのものは JSON parse の 400 でも
    // 満たされる。ドメインの検証エラー（循環・重複・件数）は 422。
    let mut with_assignee = plan_body();
    with_assignee["work_units"][0]["assignee"] = json!("someone");
    let resp2 = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &with_assignee,
        ),
    )
    .await;
    assert_eq!(resp2.status, 400, "{}", resp2.text());
}

#[tokio::test]
async fn a_second_plan_for_the_same_task_is_rejected_with_409() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let first = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &plan_body(),
        ),
    )
    .await;
    assert_eq!(first.status, 201, "{}", first.text());
    let second = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &plan_body(),
        ),
    )
    .await;
    assert_eq!(second.status, 409, "{}", second.text());
}
