//! GUI 監査対応 Phase 29: `POST /projects/{id}/plan`（ADR-0033 D4 追記「分解は人が
//! `POST /projects/{id}/plan` で起こす」）。
//!
//! 見るもの: 正しい `goal` / `project_id` / `assignee` で plan タスクが作られること、案件・途中目標の
//! 状態遷移、管理系の 401（トークンあり構成と `token_file` 未設定構成の両方）、404（案件が無い）、
//! 422（案件に属さない途中目標）。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{Status, TaskKind, TaskStore};

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(
        path,
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

fn env_with_token() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    })
}

async fn seed_secretary(app: &axum::Router) {
    let resp = send(
        app,
        p("/api/v1/org", &json!({"id": "secretary", "name": "秘書", "kind": "secretary", "brief": "案件を受け取る"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
}

async fn create_project(app: &axum::Router, title: &str, request: &str) -> String {
    let resp = send(
        app,
        p(
            "/api/v1/projects",
            &json!({"title": title, "request": request}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    resp.json()["id"].as_str().expect("id").to_string()
}

#[tokio::test]
async fn plan_creates_a_ready_plan_task_with_the_goal_project_and_secretary_assignee() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    let project_id = create_project(&app, "Pluvio 新テーマ", "隣接分野を探して欲しい").await;

    let resp = send(
        &app,
        p(
            &format!("/api/v1/projects/{project_id}/plan"),
            &json!({"note": "急がなくてよい"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    let task_id = resp.json()["task_id"]
        .as_str()
        .expect("task_id")
        .to_string();

    let task = env
        .store
        .get(task_id.parse().expect("task id"))
        .expect("get")
        .expect("some");
    assert_eq!(task.kind, TaskKind::Plan);
    assert_eq!(task.status, Status::Ready, "その場で走らせる");
    assert_eq!(
        task.project_id.map(|p| p.to_string()),
        Some(project_id.clone())
    );
    assert_eq!(task.assignee.as_deref(), Some("secretary"));
    assert!(task.objective.contains("隣接分野を探して欲しい"));
    assert!(task.objective.contains("急がなくてよい"));

    // proposed だった案件が active になる。
    let project = send(&app, g(&format!("/api/v1/projects/{project_id}")))
        .await
        .json();
    assert_eq!(project["project"]["status"], "active");
}

#[tokio::test]
async fn plan_with_a_milestone_marks_it_in_progress_and_carries_it_on_the_task() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    let project_id = create_project(&app, "t", "r").await;
    let milestone: Value = send(
        &app,
        p(
            &format!("/api/v1/projects/{project_id}/milestones"),
            &json!({"title": "隣接領域の調査", "status": "approved"}),
        ),
    )
    .await
    .json();
    let milestone_id = milestone["id"].as_str().expect("id").to_string();

    let resp = send(
        &app,
        p(
            &format!("/api/v1/projects/{project_id}/plan"),
            &json!({"milestone_id": milestone_id}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    let task_id: task_core::TaskId = resp.json()["task_id"]
        .as_str()
        .expect("task_id")
        .parse()
        .expect("id");
    let task = env.store.get(task_id).expect("get").expect("some");
    assert_eq!(
        task.milestone_id.map(|m| m.to_string()),
        Some(milestone_id.clone())
    );
    assert!(task.objective.contains("隣接領域の調査"));

    let detail = send(&app, g(&format!("/api/v1/projects/{project_id}")))
        .await
        .json();
    let updated = detail["milestones"]
        .as_array()
        .expect("milestones")
        .iter()
        .find(|m| m["id"] == milestone_id)
        .expect("milestone");
    assert_eq!(updated["status"], "in_progress");
}

/// 偽プランナーの出力から作られる子は、親（plan タスク）と同じ案件に属する（`materialize` が
/// `project_id` / `milestone_id` を継ぐ。ADR-0033 D2 の監査 D-3、Phase 23 で確定済み）。
#[tokio::test]
async fn children_materialized_from_the_plan_output_stay_in_the_same_project() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    let project_id = create_project(&app, "t", "r").await;
    let plan_id: task_core::TaskId = send(
        &app,
        p(&format!("/api/v1/projects/{project_id}/plan"), &json!({})),
    )
    .await
    .json()["task_id"]
        .as_str()
        .expect("task_id")
        .parse()
        .expect("id");
    let plan_task = env.store.get(plan_id).expect("get").expect("some");

    let plan_output = task_core::PlanOutput {
        tasks: vec![task_core::NewTask {
            harness: None,
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            title: "a".into(),
            objective: "b".into(),
            acceptance: vec![task_core::Criterion {
                text: "c".into(),
                check: task_core::Check::Human,
            }],
            depends_on: vec![],
            kind: task_core::NewTaskKind::Execute,
            tier: None,
            role: None,
            genre: None,
            assignee: None,
            workspace: None,
            category: None,
            labels: Vec::new(),
            partial_ok: None,
        }],
    };
    let children = task_core::plan::materialize(
        &plan_task,
        &plan_output,
        &[],
        &[],
        &[],
        task_core::WorkspaceContext::default(),
        time::OffsetDateTime::now_utc(),
    );
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].project_id, plan_task.project_id);
    assert_eq!(
        children[0].project_id.map(|p| p.to_string()),
        Some(project_id)
    );
}

#[tokio::test]
async fn plan_on_an_unknown_project_is_404_and_a_foreign_milestone_is_422() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;

    let resp = send(
        &app,
        p(
            "/api/v1/projects/01ARZ3NDEKTSV4RRFFQ69G5FAV/plan",
            &json!({}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 404, "{}", resp.text());

    let project_id = create_project(&app, "t", "r").await;
    let other_id = create_project(&app, "u", "s").await;
    let other_milestone: Value = send(
        &app,
        p(
            &format!("/api/v1/projects/{other_id}/milestones"),
            &json!({"title": "別案件の目標"}),
        ),
    )
    .await
    .json();
    let resp = send(
        &app,
        p(
            &format!("/api/v1/projects/{project_id}/plan"),
            &json!({"milestone_id": other_milestone["id"]}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 422, "{}", resp.text());
}

#[tokio::test]
async fn admin_endpoints_require_a_token() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    let project_id = create_project(&app, "t", "r").await;

    let resp = send(
        &app,
        post_json(&format!("/api/v1/projects/{project_id}/plan"), &json!({})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401);
}

#[tokio::test]
async fn admin_endpoints_are_401_even_without_a_configured_token() {
    let env = TestEnv::new();
    let app = env.router();

    let resp = send(
        &app,
        post_json(
            "/api/v1/projects/01ARZ3NDEKTSV4RRFFQ69G5FAV/plan",
            &json!({}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401);
}
