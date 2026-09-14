//! api.md §8.5（操作）と §8.6（伝播）。状態変更は task-ops（gate / add / plan / replay）を通る。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{Event, Status, Task, TaskId, TaskKind, TaskStore};

fn transitions(env: &TestEnv, id: TaskId) -> usize {
    env.store.events_for(id).expect("events").len()
}

fn blocked_task(env: &TestEnv) -> Task {
    let task = new_task(TaskKind::Execute, Status::Blocked);
    env.seed_with(
        &task,
        vec![Event::WorkerFinished {
            run_id: ulid::Ulid::new().to_string(),
            outcome: "question: which db?".into(),
            usage: None,
        }],
    );
    task
}

#[tokio::test]
async fn approve_covers_accept_approve_and_both_invalid_cases() {
    let env = TestEnv::new();
    let app = env.router();

    // (1) draft（kind 不問）→ accept。
    let draft = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&draft);
    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/approve", draft.id), &json!({}))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["id"], draft.id.to_string());
    assert_eq!((body["from"].as_str(), body["to"].as_str(), body["reason"].as_str()), (Some("draft"), Some("ready"), Some("accept")));
    assert_eq!(env.status_of(draft.id), Status::Ready);

    // (2) approval + ready → approve + ApprovalDecided{by: human}。
    let approval = new_task(TaskKind::Approval, Status::Ready);
    env.seed(&approval);
    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/approve", approval.id), &json!({"note": "lgtm"}))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json()["to"], "done");
    let decided = env.store.events_for(approval.id).expect("events").into_iter().find_map(|(_, e)| match e {
        Event::ApprovalDecided { by, approved, note } => Some((by, approved, note)),
        _ => None,
    });
    assert_eq!(decided, Some(("human".to_string(), true, Some("lgtm".to_string()))));

    // (3) approval + done → 409 invalid_transition。
    let before = transitions(&env, approval.id);
    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/approve", approval.id), &json!({}))).await;
    let problem = assert_problem(&resp, 409, "invalid_transition");
    assert_eq!(problem["task_status"], "done");
    assert_eq!(problem["kind"], "approval");
    assert_eq!(problem["trigger"], "approve");
    assert!(problem["detail"].as_str().expect("detail").contains("cannot be approved"), "{problem}");
    assert_eq!(transitions(&env, approval.id), before);

    // (4) execute + ready → 409 invalid_transition。
    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/approve", draft.id), &json!({}))).await;
    let problem = assert_problem(&resp, 409, "invalid_transition");
    assert_eq!(problem["task_status"], "ready");
    assert_eq!(problem["kind"], "execute");
    assert_eq!(env.status_of(draft.id), Status::Ready);
}

#[tokio::test]
async fn approving_the_same_task_twice_is_an_invalid_transition() {
    let env = TestEnv::new();
    let app = env.router();
    let approval = new_task(TaskKind::Approval, Status::Ready);
    env.seed(&approval);
    let path = format!("/api/v1/tasks/{}/approve", approval.id);
    assert_eq!(send(&app, post_json(&path, &json!({}))).await.status, 200);
    assert_problem(&send(&app, post_json(&path, &json!({}))).await, 409, "invalid_transition");

    // 空本体は `{}` と同じ（Content-Type は必要）。
    let draft = new_task(TaskKind::Plan, Status::Draft);
    env.seed(&draft);
    let empty = axum::http::Request::post(format!("/api/v1/tasks/{}/approve", draft.id))
        .header("host", HOST)
        .header("content-type", "application/json")
        .body(axum::body::Body::empty())
        .expect("request");
    assert_eq!(send(&app, empty).await.status, 200);
    assert_eq!(env.status_of(draft.id), Status::Ready);
}

#[tokio::test]
async fn reject_covers_approval_and_draft() {
    let env = TestEnv::new();
    let app = env.router();

    let approval = new_task(TaskKind::Approval, Status::Ready);
    env.seed(&approval);
    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/reject", approval.id), &json!({"note": "no"}))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json()["to"], "failed");
    assert_eq!(resp.json()["reason"], "reject");
    let decided = env.store.events_for(approval.id).expect("events").into_iter().any(|(_, e)| {
        matches!(e, Event::ApprovalDecided { approved: false, ref by, ref note } if by == "human" && note.as_deref() == Some("no"))
    });
    assert!(decided);

    let draft = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&draft);
    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/reject", draft.id), &json!({}))).await;
    let problem = assert_problem(&resp, 409, "invalid_transition");
    assert_eq!(problem["trigger"], "reject");
    assert!(problem["detail"].as_str().expect("detail").contains("cannot be rejected"));
    assert_eq!(env.status_of(draft.id), Status::Draft);
}

#[tokio::test]
async fn answer_covers_blocked_not_blocked_and_blank() {
    let env = TestEnv::new();
    let app = env.router();

    let blocked = blocked_task(&env);
    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/answer", blocked.id), &json!({"answer": "sqlite"}))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!((resp.json()["from"].as_str(), resp.json()["to"].as_str()), (Some("blocked"), Some("ready")));
    let answered = env.store.events_for(blocked.id).expect("events").into_iter().find_map(|(_, e)| match e {
        Event::Answered { question, answer } => Some((question, answer)),
        _ => None,
    });
    assert_eq!(answered, Some(("which db?".to_string(), "sqlite".to_string())));

    let ready = new_task(TaskKind::Execute, Status::Ready);
    env.seed(&ready);
    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/answer", ready.id), &json!({"answer": "x"}))).await;
    let problem = assert_problem(&resp, 409, "invalid_transition");
    assert_eq!(problem["trigger"], "answer");
    assert!(problem["detail"].as_str().expect("detail").contains("only blocked tasks accept an answer"));

    let still_blocked = blocked_task(&env);
    let before = transitions(&env, still_blocked.id);
    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/answer", still_blocked.id), &json!({"answer": " \n\t "}))).await;
    let problem = assert_problem(&resp, 422, "validation");
    assert_eq!(problem["errors"], json!([{"field": "answer", "message": "answer must not be blank"}]));
    assert_eq!(env.status_of(still_blocked.id), Status::Blocked);
    assert_eq!(transitions(&env, still_blocked.id), before);

    let missing_field = send(&app, post_json(&format!("/api/v1/tasks/{}/answer", still_blocked.id), &json!({}))).await;
    assert_problem(&missing_field, 400, "bad_request");
}

#[tokio::test]
async fn cancel_of_terminal_tasks_is_invalid_and_non_terminal_is_cancelled() {
    let env = TestEnv::new();
    let app = env.router();
    for status in [Status::Done, Status::Failed, Status::Cancelled] {
        let task = new_task(TaskKind::Execute, status);
        env.seed(&task);
        let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/cancel", task.id), &json!({}))).await;
        let problem = assert_problem(&resp, 409, "invalid_transition");
        assert_eq!(problem["trigger"], "cancel");
        assert!(problem["detail"].as_str().expect("detail").contains("cannot be cancelled"));
        assert_eq!(env.status_of(task.id), status);
    }
    let ready = new_task(TaskKind::Execute, Status::Ready);
    env.seed(&ready);
    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/cancel", ready.id), &json!({"expected_status": "ready"}))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json()["to"], "cancelled");
}

#[tokio::test]
async fn expected_status_mismatch_is_a_conflict_that_changes_nothing() {
    let env = TestEnv::new();
    let app = env.router();
    let draft = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&draft);
    let before = transitions(&env, draft.id);

    for (op, body) in [
        ("approve", json!({"expected_status": "ready"})),
        ("reject", json!({"expected_status": "ready"})),
        ("cancel", json!({"expected_status": "blocked"})),
        ("answer", json!({"answer": "x", "expected_status": "blocked"})),
    ] {
        let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/{op}", draft.id), &body)).await;
        let problem = assert_problem(&resp, 409, "conflict");
        assert_eq!(problem["expected"], body["expected_status"], "{op}");
        assert_eq!(problem["actual"], "draft", "{op}");
    }
    assert_eq!(env.status_of(draft.id), Status::Draft);
    assert_eq!(transitions(&env, draft.id), before);
}

#[tokio::test]
async fn operations_on_missing_tasks_are_404() {
    let env = TestEnv::new();
    let app = env.router();
    let id = TaskId::new();
    for (op, body) in [
        ("approve", json!({})),
        ("reject", json!({})),
        ("cancel", json!({})),
        ("answer", json!({"answer": "x"})),
    ] {
        let resp = send(&app, post_json(&format!("/api/v1/tasks/{id}/{op}"), &body)).await;
        assert_problem(&resp, 404, "task_not_found");
    }
}

#[tokio::test]
async fn create_task_returns_201_with_location_and_cli_defaults() {
    let env = TestEnv::new();
    let app = env.router();
    let body = json!({
        "title": "add CLI parsing",
        "objective": "parse args",
        "acceptance": [
            {"type": "reviewer", "text": "the diff is minimal"},
            {"type": "command", "cmd": "cargo test"},
            {"type": "artifact_exists", "name": "bench.json"},
            {"type": "human", "text": "reviewer is happy"}
        ]
    });
    let resp = send(&app, post_json("/api/v1/tasks", &body)).await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let task = resp.json();
    let id = task["id"].as_str().expect("id").to_string();
    assert_eq!(resp.header("location"), Some(format!("/api/v1/tasks/{id}").as_str()));
    assert_eq!(task["kind"], "execute");
    assert_eq!(task["status"], "draft");
    assert_eq!(task["worker_hint"], json!({"tier": "standard", "adapter": null}));
    assert_eq!(task["budget"], json!({"max_turns": 10, "max_wall_secs": 600, "max_retries": 2}));
    assert_eq!(task["workspace"], json!({"kind": "local", "path": id}));
    let texts: Vec<&str> = task["acceptance"].as_array().expect("acceptance").iter().map(|c| c["text"].as_str().expect("text")).collect();
    assert_eq!(texts, vec!["the diff is minimal", "`cargo test` exits 0", "artifact bench.json exists", "reviewer is happy"]);
    let stored = env.store.get(id.parse().expect("id")).expect("get").expect("stored");
    assert_eq!(serde_json::to_value(&stored).expect("json"), task);

    let approval = json!({"title": "gate", "objective": "o", "kind": "approval", "acceptance": [{"type": "human", "text": "ok"}]});
    let resp = send(&app, post_json("/api/v1/tasks", &approval)).await;
    assert_eq!(resp.status, 201);
    assert_eq!(resp.json()["status"], "ready");
}

#[tokio::test]
async fn create_task_validation_errors_insert_nothing() {
    let env = TestEnv::new();
    let app = env.router();
    let failed = new_task(TaskKind::Execute, Status::Failed);
    let cancelled = new_task(TaskKind::Execute, Status::Cancelled);
    env.seed(&failed);
    env.seed(&cancelled);
    let missing = TaskId::new();
    let human = json!([{"type": "human", "text": "ok"}]);

    let cases: Vec<(Value, u16, &str, Option<Value>)> = vec![
        (
            json!({"title": "t", "objective": "o", "acceptance": []}),
            422,
            "validation",
            Some(json!([{"field": "acceptance", "message": "at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)"}])),
        ),
        (
            json!({"title": "t", "objective": "o", "acceptance": human, "depends_on": [missing.to_string()]}),
            422,
            "validation",
            Some(json!([{"field": "depends_on", "message": format!("dependency {missing} does not exist")}])),
        ),
        (
            json!({"title": "t", "objective": "o", "acceptance": human, "depends_on": [failed.id.to_string()]}),
            422,
            "validation",
            Some(json!([{"field": "depends_on", "message": format!("dependency {} has status Failed and cannot be depended on", failed.id)}])),
        ),
        (
            json!({"title": "t", "objective": "o", "acceptance": human, "depends_on": [cancelled.id.to_string()]}),
            422,
            "validation",
            Some(json!([{"field": "depends_on", "message": format!("dependency {} has status Cancelled and cannot be depended on", cancelled.id)}])),
        ),
        (json!({"objective": "o", "acceptance": human}), 400, "bad_request", None),
    ];
    for (body, status, code, errors) in cases {
        let resp = send(&app, post_json("/api/v1/tasks", &body)).await;
        let problem = assert_problem(&resp, status, code);
        if let Some(errors) = errors {
            assert_eq!(problem["errors"], errors);
            assert_eq!(problem["detail"], errors[0]["message"]);
        }
    }
    assert_eq!(env.store.list(None).expect("list").len(), 2, "nothing was inserted");
}

#[tokio::test]
async fn create_plan_returns_201_and_rejects_blank_goals() {
    let env = TestEnv::new();
    let app = env.router();
    let resp = send(&app, post_json("/api/v1/plans", &json!({"goal": "build the CLI\nwith tests"}))).await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let plan = resp.json();
    let id = plan["id"].as_str().expect("id");
    assert_eq!(resp.header("location"), Some(format!("/api/v1/tasks/{id}").as_str()));
    assert_eq!(plan["kind"], "plan");
    assert_eq!(plan["status"], "draft");
    assert_eq!(plan["title"], "build the CLI");
    assert_eq!(plan["acceptance"], json!([]));
    assert_eq!(plan["parent_id"], Value::Null);
    assert_eq!(plan["worker_hint"]["tier"], "frontier");
    assert_eq!(plan["budget"], json!({"max_turns": 30, "max_wall_secs": 900, "max_retries": 1}));

    let blank = send(&app, post_json("/api/v1/plans", &json!({"goal": "  \n "}))).await;
    let problem = assert_problem(&blank, 422, "validation");
    assert_eq!(problem["errors"], json!([{"field": "goal", "message": "goal must not be blank"}]));
    assert_eq!(env.store.list(None).expect("list").len(), 1);
}

#[tokio::test]
async fn replay_reports_zero_mismatches_after_api_operations() {
    let env = TestEnv::new();
    let app = env.router();
    let created = send(
        &app,
        post_json("/api/v1/tasks", &json!({"title": "t", "objective": "o", "acceptance": [{"type": "human", "text": "ok"}]})),
    )
    .await
    .json();
    let id = created["id"].as_str().expect("id").to_string();
    assert_eq!(send(&app, post_json(&format!("/api/v1/tasks/{id}/approve"), &json!({}))).await.status, 200);
    assert_eq!(send(&app, post_json(&format!("/api/v1/tasks/{id}/cancel"), &json!({}))).await.status, 200);
    assert_eq!(send(&app, post_json("/api/v1/plans", &json!({"goal": "g"}))).await.status, 201);

    let resp = send(&app, post_json("/api/v1/replay", &json!({}))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json(), json!({"tasks": 2, "mismatches": []}));

    let empty = axum::http::Request::post("/api/v1/replay")
        .header("host", HOST)
        .header("content-type", "application/json")
        .body(axum::body::Body::empty())
        .expect("request");
    assert_eq!(send(&app, empty).await.status, 200);
}

#[tokio::test]
async fn rejecting_an_approval_lists_cancelled_children_in_cascaded() {
    let env = TestEnv::new();
    let app = env.router();
    let approval = new_task(TaskKind::Approval, Status::Ready);
    let mut child = new_task(TaskKind::Execute, Status::Ready);
    child.parent_id = Some(approval.id);
    env.seed(&approval);
    env.seed(&child);

    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/reject", approval.id), &json!({}))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let cascaded = resp.json()["cascaded"].as_array().cloned().expect("cascaded");
    assert_eq!(cascaded.len(), 1);
    assert_eq!(cascaded[0]["id"], child.id.to_string());
    assert_eq!(cascaded[0]["status"], "cancelled");
    assert_eq!(env.status_of(child.id), Status::Cancelled);
}

#[tokio::test]
async fn cancelling_a_predecessor_lists_dependents_in_cascaded() {
    let env = TestEnv::new();
    let app = env.router();
    let first = new_task(TaskKind::Execute, Status::Ready);
    let mut second = new_task(TaskKind::Execute, Status::Draft);
    second.depends_on = vec![first.id];
    let mut third = new_task(TaskKind::Execute, Status::Draft);
    third.depends_on = vec![second.id];
    env.seed(&first);
    env.seed(&second);
    env.seed(&third);

    let resp = send(&app, post_json(&format!("/api/v1/tasks/{}/cancel", first.id), &json!({}))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let cascaded: Vec<String> = resp.json()["cascaded"]
        .as_array()
        .expect("cascaded")
        .iter()
        .map(|r| r["id"].as_str().expect("id").to_string())
        .collect();
    assert!(cascaded.contains(&second.id.to_string()) && cascaded.contains(&third.id.to_string()), "{cascaded:?}");
    let reason = env.store.events_for(second.id).expect("events").into_iter().rev().find_map(|(_, e)| match e {
        Event::Transitioned { reason, .. } => Some(reason),
        _ => None,
    });
    assert_eq!(reason.as_deref(), Some("dependency_failed"));
}
