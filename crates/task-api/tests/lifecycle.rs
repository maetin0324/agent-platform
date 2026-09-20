//! ADR-0044 D6（Phase 55）: 案件・途中目標の **中止・一時停止・アーカイブ** の API。
//!
//! `POST /projects/{id}/{cancel|pause|resume|archive|unarchive}` と
//! `POST /milestones/{id}/{cancel|pause|resume}`。どれも管理系（bearer 必須。401 は
//! `operations.rs` の `project_and_milestone_mutations_require_a_bearer_token` が見る）。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{ListFilter, ListOrder, Status, Task, TaskKind, TaskStore};

/// 案件を 1 件作り、その id を返す。
async fn a_project(app: &axum::Router, title: &str) -> String {
    let resp = send(
        app,
        post_admin(
            "/api/v1/projects",
            &json!({"title": title, "request": "やって"}),
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    resp.json()["id"].as_str().expect("id").to_string()
}

async fn a_milestone(app: &axum::Router, project_id: &str, title: &str) -> String {
    let resp = send(
        app,
        post_admin(
            &format!("/api/v1/projects/{project_id}/milestones"),
            &json!({"title": title}),
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    resp.json()["id"].as_str().expect("id").to_string()
}

/// その案件（と任意で途中目標）に属するタスクを 1 件仕込む。
fn seed_task(env: &TestEnv, project_id: &str, milestone_id: Option<&str>, status: Status) -> Task {
    let mut task = new_task(TaskKind::Execute, status);
    task.project_id = Some(project_id.parse().expect("project id"));
    task.milestone_id = milestone_id.map(|m| m.parse().expect("milestone id"));
    if status == Status::Running {
        task.lease = Some(task_core::Lease {
            worker_run_id: "run-1".into(),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(60),
        });
    }
    env.seed(&task);
    task
}

async fn act(app: &axum::Router, path: &str) -> common::Resp {
    send(app, post_admin(path, &json!({}))).await
}

// ---- 中止（連鎖） ----

/// 案件の中止は、属する**非終端タスクを全部** `cancelled` にし、非終端の途中目標も `cancelled` にし、
/// 案件自身を `cancelled` にする。タスクには `reason = "project_cancelled"` が残る。
#[tokio::test]
async fn cancelling_a_project_cascades_to_its_tasks_and_milestones() {
    let env = admin_env();
    let app = env.router();
    let project = a_project(&app, "止める案件").await;
    let milestone = a_milestone(&app, &project, "途中目標").await;
    let running = seed_task(&env, &project, Some(&milestone), Status::Running);
    let ready = seed_task(&env, &project, None, Status::Ready);
    let done = seed_task(&env, &project, None, Status::Done);

    let resp = act(&app, &format!("/api/v1/projects/{project}/cancel")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["project"]["status"], "cancelled");
    let cancelled: Vec<String> = body["cancelled_tasks"]
        .as_array()
        .expect("cancelled_tasks")
        .iter()
        .map(|t| t["id"].as_str().expect("id").to_string())
        .collect();
    assert!(cancelled.contains(&running.id.to_string()), "{cancelled:?}");
    assert!(cancelled.contains(&ready.id.to_string()), "{cancelled:?}");
    assert!(!cancelled.contains(&done.id.to_string()), "{cancelled:?}");
    assert_eq!(
        body["cancelled_milestones"],
        json!([milestone]),
        "{}",
        body["cancelled_milestones"]
    );

    assert_eq!(env.status_of(running.id), Status::Cancelled);
    assert_eq!(env.status_of(ready.id), Status::Cancelled);
    assert_eq!(env.status_of(done.id), Status::Done);
    assert_eq!(
        transition_reason(&env, running.id),
        Some("project_cancelled".to_string())
    );

    // 二度目は 409。
    let again = act(&app, &format!("/api/v1/projects/{project}/cancel")).await;
    assert_problem(&again, 409, "invalid_transition");
}

/// 途中目標の中止はその途中目標のタスクだけを `cancelled` にし、理由は `milestone_cancelled`。
/// 案件の状態は変わらない。
#[tokio::test]
async fn cancelling_a_milestone_only_touches_its_own_tasks() {
    let env = admin_env();
    let app = env.router();
    let project = a_project(&app, "案件").await;
    let milestone = a_milestone(&app, &project, "途中目標").await;
    let inside = seed_task(&env, &project, Some(&milestone), Status::Ready);
    let outside = seed_task(&env, &project, None, Status::Ready);

    let resp = act(&app, &format!("/api/v1/milestones/{milestone}/cancel")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json()["milestone"]["status"], "cancelled");
    assert_eq!(env.status_of(inside.id), Status::Cancelled);
    assert_eq!(env.status_of(outside.id), Status::Ready);
    assert_eq!(
        transition_reason(&env, inside.id),
        Some("milestone_cancelled".to_string())
    );

    let detail = send(&app, get_admin(&format!("/api/v1/projects/{project}")))
        .await
        .json();
    assert_eq!(detail["project"]["status"], "proposed");
}

fn transition_reason(env: &TestEnv, id: task_core::TaskId) -> Option<String> {
    env.store
        .events_for(id)
        .expect("events")
        .into_iter()
        .rev()
        .find_map(|(_, e)| match e {
            task_core::Event::Transitioned { reason, .. } => Some(reason),
            _ => None,
        })
}

// ---- 一時停止と再開 ----

/// `pause` は元の状態を `paused_from` に覚え、`resume` がそこへ戻す。`ready` のタスクは**触らない**
/// （状態機械は動かさない）が、`ready_tasks`（dispatch の候補）からは消える。
#[tokio::test]
async fn pausing_a_project_stops_dispatch_and_resume_restores_the_previous_status() {
    let env = admin_env();
    let app = env.router();
    let project = a_project(&app, "止めたり動かしたり").await;
    let task = seed_task(&env, &project, None, Status::Ready);

    let paused = act(&app, &format!("/api/v1/projects/{project}/pause")).await;
    assert_eq!(paused.status, 200, "{}", paused.text());
    assert_eq!(paused.json()["project"]["status"], "paused");
    assert_eq!(paused.json()["project"]["paused_from"], "proposed");
    assert_eq!(env.status_of(task.id), Status::Ready);
    assert!(!ready_ids(&env).contains(&task.id.to_string()));

    // 二度目の pause は 409。
    assert_problem(
        &act(&app, &format!("/api/v1/projects/{project}/pause")).await,
        409,
        "invalid_transition",
    );

    let resumed = act(&app, &format!("/api/v1/projects/{project}/resume")).await;
    assert_eq!(resumed.status, 200, "{}", resumed.text());
    assert_eq!(resumed.json()["project"]["status"], "proposed");
    assert_eq!(resumed.json()["project"]["paused_from"], Value::Null);
    assert!(ready_ids(&env).contains(&task.id.to_string()));

    // `paused` でない案件の resume は 409。
    assert_problem(
        &act(&app, &format!("/api/v1/projects/{project}/resume")).await,
        409,
        "invalid_transition",
    );
}

/// 途中目標の `pause` はその途中目標のタスクだけを dispatch から外す。
#[tokio::test]
async fn pausing_a_milestone_stops_only_its_own_tasks() {
    let env = admin_env();
    let app = env.router();
    let project = a_project(&app, "案件").await;
    let milestone = a_milestone(&app, &project, "途中目標").await;
    let inside = seed_task(&env, &project, Some(&milestone), Status::Ready);
    let outside = seed_task(&env, &project, None, Status::Ready);

    let paused = act(&app, &format!("/api/v1/milestones/{milestone}/pause")).await;
    assert_eq!(paused.status, 200, "{}", paused.text());
    assert_eq!(paused.json()["milestone"]["status"], "paused");
    assert_eq!(paused.json()["milestone"]["paused_from"], "proposed");
    let ready = ready_ids(&env);
    assert!(!ready.contains(&inside.id.to_string()));
    assert!(ready.contains(&outside.id.to_string()));

    let resumed = act(&app, &format!("/api/v1/milestones/{milestone}/resume")).await;
    assert_eq!(resumed.status, 200, "{}", resumed.text());
    assert_eq!(resumed.json()["milestone"]["status"], "proposed");
    assert!(ready_ids(&env).contains(&inside.id.to_string()));
}

fn ready_ids(env: &TestEnv) -> Vec<String> {
    env.store
        .ready_tasks(100)
        .expect("ready tasks")
        .into_iter()
        .map(|t| t.id.to_string())
        .collect()
}

// ---- アーカイブ ----

/// アーカイブできるのは終端（`done` / `cancelled`）の案件だけ。アーカイブすると
/// `GET /projects` と `GET /tasks` から既定で消え、`?archived=1` で見える。
#[tokio::test]
async fn archive_requires_a_terminal_project_and_hides_it_and_its_tasks_by_default() {
    let env = admin_env();
    let app = env.router();
    let project = a_project(&app, "終わった案件").await;
    let task = seed_task(&env, &project, None, Status::Done);

    // 動いている案件は 409。
    assert_problem(
        &act(&app, &format!("/api/v1/projects/{project}/archive")).await,
        409,
        "invalid_transition",
    );

    // 中止してから（＝終端にしてから）ならアーカイブできる。
    assert_eq!(
        act(&app, &format!("/api/v1/projects/{project}/cancel"))
            .await
            .status,
        200
    );
    let archived = act(&app, &format!("/api/v1/projects/{project}/archive")).await;
    assert_eq!(archived.status, 200, "{}", archived.text());
    assert!(archived.json()["project"]["archived_at"].is_string());

    // 一覧から消える。
    let list = send(&app, get_admin("/api/v1/projects")).await.json();
    assert!(!project_ids(&list).contains(&project), "{list}");
    let all = send(&app, get_admin("/api/v1/projects?archived=1"))
        .await
        .json();
    assert!(project_ids(&all).contains(&project), "{all}");

    // タスクも隠れる（ただし個別の `GET /tasks/{id}` は従来どおり見える）。
    let tasks = send(&app, get_admin("/api/v1/tasks")).await.json();
    assert!(!task_ids(&tasks).contains(&task.id.to_string()), "{tasks}");
    let tasks = send(&app, get_admin("/api/v1/tasks?archived=1"))
        .await
        .json();
    assert!(task_ids(&tasks).contains(&task.id.to_string()), "{tasks}");
    assert_eq!(
        send(&app, get_admin(&format!("/api/v1/tasks/{}", task.id)))
            .await
            .status,
        200
    );

    // 解除すれば戻る（二度押しても 409 にはしない）。
    assert_eq!(
        act(&app, &format!("/api/v1/projects/{project}/archive"))
            .await
            .status,
        200
    );
    let unarchived = act(&app, &format!("/api/v1/projects/{project}/unarchive")).await;
    assert_eq!(unarchived.status, 200, "{}", unarchived.text());
    assert_eq!(unarchived.json()["project"]["archived_at"], Value::Null);
    let list = send(&app, get_admin("/api/v1/projects")).await.json();
    assert!(project_ids(&list).contains(&project), "{list}");
}

fn project_ids(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|p| p["id"].as_str().expect("id").to_string())
        .collect()
}

fn task_ids(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|t| t["id"].as_str().expect("id").to_string())
        .collect()
}

/// ADR-0044 D6（Phase 55）: `paused` / `cancelled` は**専用のエンドポイントでしか入れない**。
/// `PATCH` で入れると `paused_from`（`resume` の戻り先）が空のままになり、中止の連鎖も起きないので 422。
/// GUI の「状態を直接変える」プルダウンからもこの 2 つは外してある（`gui/app/routes/projects.$id.tsx`）。
#[tokio::test]
async fn patch_cannot_set_paused_or_cancelled_on_a_project_or_a_milestone() {
    let env = admin_env();
    let app = env.router();
    let project = a_project(&app, "案件").await;
    let milestone = a_milestone(&app, &project, "途中目標").await;

    for status in ["paused", "cancelled"] {
        let resp = send(
            &app,
            patch_admin(
                &format!("/api/v1/projects/{project}"),
                &json!({"status": status}),
            ),
        )
        .await;
        let problem = assert_problem(&resp, 422, "validation");
        assert_eq!(problem["errors"][0]["field"], "status", "{problem}");

        let resp = send(
            &app,
            patch_admin(
                &format!("/api/v1/milestones/{milestone}"),
                &json!({"status": status}),
            ),
        )
        .await;
        let problem = assert_problem(&resp, 422, "validation");
        assert_eq!(problem["errors"][0]["field"], "status", "{problem}");
    }

    // 何も変わっていない。他の状態は従来どおり `PATCH` で入る。
    let detail = send(&app, get_admin(&format!("/api/v1/projects/{project}")))
        .await
        .json();
    assert_eq!(detail["project"]["status"], "proposed");
    assert_eq!(
        send(
            &app,
            patch_admin(
                &format!("/api/v1/projects/{project}"),
                &json!({"status": "active"})
            )
        )
        .await
        .status,
        200
    );
    assert_eq!(
        send(
            &app,
            patch_admin(
                &format!("/api/v1/milestones/{milestone}"),
                &json!({"status": "in_progress"})
            )
        )
        .await
        .status,
        200
    );
}

/// 知らない id は 404（案件も途中目標も）。ULID でない id も既存の `parse_project_id` /
/// `parse_milestone_id` と同じく 404（`GET /projects/{id}` と揃える）。
#[tokio::test]
async fn unknown_and_malformed_ids_are_404() {
    let env = admin_env();
    let app = env.router();
    let project = task_core::ProjectId::new();
    let milestone = task_core::MilestoneId::new();

    assert_problem(
        &act(&app, &format!("/api/v1/projects/{project}/pause")).await,
        404,
        "project_not_found",
    );
    assert_problem(
        &act(&app, &format!("/api/v1/milestones/{milestone}/cancel")).await,
        404,
        "milestone_not_found",
    );
    assert_problem(
        &act(&app, "/api/v1/projects/not-a-ulid/cancel").await,
        404,
        "project_not_found",
    );
    assert_problem(
        &act(&app, "/api/v1/milestones/not-a-ulid/pause").await,
        404,
        "milestone_not_found",
    );
    let _ = (ListFilter::default(), ListOrder::CreatedDesc, &env);
}
