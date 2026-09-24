//! api.md §8.5（操作）と §8.6（伝播）。状態変更は task-ops（gate / add / plan / replay）を通る。
//!
//! ADR-0044 §5 Phase 53 追記（Phase 55）: **変更を伴う API はすべて管理系（bearer 必須）**になったので、
//! この一式は `admin_env()`（`token_file` 相当あり）と `post_admin` / `get_admin` を使う。
//! トークン無しが 401 になることは `every_mutating_endpoint_requires_a_bearer_token` が見る。

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
            role: None,
            metrics: None,
            end: None,
        }],
    );
    task
}

#[tokio::test]
async fn approve_covers_accept_approve_and_both_invalid_cases() {
    let env = admin_env();
    let app = env.router();

    // (1) draft（kind 不問）→ accept。
    let draft = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&draft);
    let resp = send(
        &app,
        post_admin(&format!("/api/v1/tasks/{}/approve", draft.id), &json!({})),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["id"], draft.id.to_string());
    assert_eq!(
        (
            body["from"].as_str(),
            body["to"].as_str(),
            body["reason"].as_str()
        ),
        (Some("draft"), Some("ready"), Some("accept"))
    );
    assert_eq!(env.status_of(draft.id), Status::Ready);

    // (2) approval + ready → approve + ApprovalDecided{by: human}。
    let approval = new_task(TaskKind::Approval, Status::Ready);
    env.seed(&approval);
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/approve", approval.id),
            &json!({"note": "lgtm"}),
        ),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json()["to"], "done");
    let decided = env
        .store
        .events_for(approval.id)
        .expect("events")
        .into_iter()
        .find_map(|(_, e)| match e {
            Event::ApprovalDecided { by, approved, note } => Some((by, approved, note)),
            _ => None,
        });
    assert_eq!(
        decided,
        Some(("human".to_string(), true, Some("lgtm".to_string())))
    );

    // (3) approval + done → 409 invalid_transition。
    let before = transitions(&env, approval.id);
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/approve", approval.id),
            &json!({}),
        ),
    )
    .await;
    let problem = assert_problem(&resp, 409, "invalid_transition");
    assert_eq!(problem["task_status"], "done");
    assert_eq!(problem["kind"], "approval");
    assert_eq!(problem["trigger"], "approve");
    assert!(
        problem["detail"]
            .as_str()
            .expect("detail")
            .contains("cannot be approved"),
        "{problem}"
    );
    assert_eq!(transitions(&env, approval.id), before);

    // (4) execute + ready → 409 invalid_transition。
    let resp = send(
        &app,
        post_admin(&format!("/api/v1/tasks/{}/approve", draft.id), &json!({})),
    )
    .await;
    let problem = assert_problem(&resp, 409, "invalid_transition");
    assert_eq!(problem["task_status"], "ready");
    assert_eq!(problem["kind"], "execute");
    assert_eq!(env.status_of(draft.id), Status::Ready);
}

#[tokio::test]
async fn approving_the_same_task_twice_is_an_invalid_transition() {
    let env = admin_env();
    let app = env.router();
    let approval = new_task(TaskKind::Approval, Status::Ready);
    env.seed(&approval);
    let path = format!("/api/v1/tasks/{}/approve", approval.id);
    assert_eq!(send(&app, post_admin(&path, &json!({}))).await.status, 200);
    assert_problem(
        &send(&app, post_admin(&path, &json!({}))).await,
        409,
        "invalid_transition",
    );

    // 空本体は `{}` と同じ（Content-Type は必要）。
    let draft = new_task(TaskKind::Plan, Status::Draft);
    env.seed(&draft);
    let empty = axum::http::Request::post(format!("/api/v1/tasks/{}/approve", draft.id))
        .header("host", HOST)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(axum::body::Body::empty())
        .expect("request");
    assert_eq!(send(&app, empty).await.status, 200);
    assert_eq!(env.status_of(draft.id), Status::Ready);
}

#[tokio::test]
async fn reject_covers_approval_and_draft() {
    let env = admin_env();
    let app = env.router();

    let approval = new_task(TaskKind::Approval, Status::Ready);
    env.seed(&approval);
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/reject", approval.id),
            &json!({"note": "no"}),
        ),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json()["to"], "failed");
    assert_eq!(resp.json()["reason"], "reject");
    let decided = env.store.events_for(approval.id).expect("events").into_iter().any(|(_, e)| {
        matches!(e, Event::ApprovalDecided { approved: false, ref by, ref note } if by == "human" && note.as_deref() == Some("no"))
    });
    assert!(decided);

    let draft = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&draft);
    let resp = send(
        &app,
        post_admin(&format!("/api/v1/tasks/{}/reject", draft.id), &json!({})),
    )
    .await;
    let problem = assert_problem(&resp, 409, "invalid_transition");
    assert_eq!(problem["trigger"], "reject");
    assert!(
        problem["detail"]
            .as_str()
            .expect("detail")
            .contains("cannot be rejected")
    );
    assert_eq!(env.status_of(draft.id), Status::Draft);
}

#[tokio::test]
async fn answer_covers_blocked_not_blocked_and_blank() {
    let env = admin_env();
    let app = env.router();

    let blocked = blocked_task(&env);
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/answer", blocked.id),
            &json!({"answer": "sqlite"}),
        ),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(
        (resp.json()["from"].as_str(), resp.json()["to"].as_str()),
        (Some("blocked"), Some("ready"))
    );
    let answered = env
        .store
        .events_for(blocked.id)
        .expect("events")
        .into_iter()
        .find_map(|(_, e)| match e {
            Event::Answered { question, answer } => Some((question, answer)),
            _ => None,
        });
    assert_eq!(
        answered,
        Some(("which db?".to_string(), "sqlite".to_string()))
    );

    let ready = new_task(TaskKind::Execute, Status::Ready);
    env.seed(&ready);
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/answer", ready.id),
            &json!({"answer": "x"}),
        ),
    )
    .await;
    let problem = assert_problem(&resp, 409, "invalid_transition");
    assert_eq!(problem["trigger"], "answer");
    assert!(
        problem["detail"]
            .as_str()
            .expect("detail")
            .contains("only blocked tasks accept an answer")
    );

    let still_blocked = blocked_task(&env);
    let before = transitions(&env, still_blocked.id);
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/answer", still_blocked.id),
            &json!({"answer": " \n\t "}),
        ),
    )
    .await;
    let problem = assert_problem(&resp, 422, "validation");
    assert_eq!(
        problem["errors"],
        json!([{"field": "answer", "message": "answer must not be blank"}])
    );
    assert_eq!(env.status_of(still_blocked.id), Status::Blocked);
    assert_eq!(transitions(&env, still_blocked.id), before);

    let missing_field = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/answer", still_blocked.id),
            &json!({}),
        ),
    )
    .await;
    assert_problem(&missing_field, 400, "bad_request");
}

#[tokio::test]
async fn cancel_of_terminal_tasks_is_invalid_and_non_terminal_is_cancelled() {
    let env = admin_env();
    let app = env.router();
    for status in [Status::Done, Status::Failed, Status::Cancelled] {
        let task = new_task(TaskKind::Execute, status);
        env.seed(&task);
        let resp = send(
            &app,
            post_admin(&format!("/api/v1/tasks/{}/cancel", task.id), &json!({})),
        )
        .await;
        let problem = assert_problem(&resp, 409, "invalid_transition");
        assert_eq!(problem["trigger"], "cancel");
        assert!(
            problem["detail"]
                .as_str()
                .expect("detail")
                .contains("cannot be cancelled")
        );
        assert_eq!(env.status_of(task.id), status);
    }
    let ready = new_task(TaskKind::Execute, Status::Ready);
    env.seed(&ready);
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/cancel", ready.id),
            &json!({"expected_status": "ready"}),
        ),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json()["to"], "cancelled");
}

#[tokio::test]
async fn expected_status_mismatch_is_a_conflict_that_changes_nothing() {
    let env = admin_env();
    let app = env.router();
    let draft = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&draft);
    let before = transitions(&env, draft.id);

    for (op, body) in [
        ("approve", json!({"expected_status": "ready"})),
        ("reject", json!({"expected_status": "ready"})),
        ("cancel", json!({"expected_status": "blocked"})),
        (
            "answer",
            json!({"answer": "x", "expected_status": "blocked"}),
        ),
    ] {
        let resp = send(
            &app,
            post_admin(&format!("/api/v1/tasks/{}/{op}", draft.id), &body),
        )
        .await;
        let problem = assert_problem(&resp, 409, "conflict");
        assert_eq!(problem["expected"], body["expected_status"], "{op}");
        assert_eq!(problem["actual"], "draft", "{op}");
    }
    assert_eq!(env.status_of(draft.id), Status::Draft);
    assert_eq!(transitions(&env, draft.id), before);
}

#[tokio::test]
async fn operations_on_missing_tasks_are_404() {
    let env = admin_env();
    let app = env.router();
    let id = TaskId::new();
    for (op, body) in [
        ("approve", json!({})),
        ("reject", json!({})),
        ("cancel", json!({})),
        ("answer", json!({"answer": "x"})),
        ("retry", json!({})),
    ] {
        let resp = send(&app, post_admin(&format!("/api/v1/tasks/{id}/{op}"), &body)).await;
        assert_problem(&resp, 404, "task_not_found");
    }
}

/// Phase 31（実機の事故、2026-09-18）: `POST /tasks/{id}/retry`。
#[tokio::test]
async fn retry_duplicates_a_failed_task_and_rewires_dependents() {
    let env = admin_env();
    let app = env.router();

    let mut original = new_task(TaskKind::Execute, Status::Failed);
    original.title = "investigate incident".into();
    original.objective = "find out why the LLM stopped".into();
    env.seed(&original);

    // 未終端の後続（対話タスク扱いではないが、テストの都合上そのまま draft/ready/blocked を直接 seed する
    // ことで、実機の cascade を経ずに「張り替え対象」の状態を再現する。ready にするには依存を空にしないと
    // `env.seed` は検証を経由しないのでそのまま status を書ける）。
    let mut draft_dependent = new_task(TaskKind::Execute, Status::Draft);
    draft_dependent.depends_on = vec![original.id];
    env.seed(&draft_dependent);

    // `dependency_failed` で cancelled になった後続。
    let mut cancelled_dependent = new_task(TaskKind::Execute, Status::Cancelled);
    cancelled_dependent.depends_on = vec![original.id];
    env.seed_with(
        &cancelled_dependent,
        vec![Event::Transitioned {
            from: Status::Ready,
            to: Status::Cancelled,
            reason: "dependency_failed".into(),
        }],
    );

    // 人が直接 cancel した後続（対象外）。
    let mut manually_cancelled = new_task(TaskKind::Execute, Status::Cancelled);
    manually_cancelled.depends_on = vec![original.id];
    env.seed_with(
        &manually_cancelled,
        vec![Event::Transitioned {
            from: Status::Ready,
            to: Status::Cancelled,
            reason: "cancel".into(),
        }],
    );

    // ADR-0070 D2 追記（Phase 116）: `accept` の既定が `true` に変わったので、この試験は
    // `draft` で始まることを明示的に指定する（依存の張り替えの検証が主眼で、既定値の検証ではない）。
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/retry", original.id),
            &json!({"accept": false}),
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let body = resp.json();
    let new_id: TaskId = body["task_id"]
        .as_str()
        .expect("task_id")
        .parse()
        .expect("parse");
    assert_eq!(
        resp.header("location"),
        Some(format!("/api/v1/tasks/{new_id}").as_str())
    );
    let mut rewired: Vec<String> = body["rewired"]
        .as_array()
        .expect("rewired")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    rewired.sort();
    let mut expected = vec![
        draft_dependent.id.to_string(),
        cancelled_dependent.id.to_string(),
    ];
    expected.sort();
    assert_eq!(rewired, expected);

    let new_task_row = env.store.get(new_id).expect("get").expect("some");
    assert_eq!(new_task_row.status, Status::Draft);
    assert_eq!(new_task_row.title, original.title);
    assert_eq!(new_task_row.objective, original.objective);
    assert_eq!(new_task_row.depends_on, original.depends_on);
    let events: Vec<Event> = env
        .store
        .events_for(new_id)
        .expect("events")
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    assert!(matches!(&events[0], Event::Created { task } if task.id == new_id));
    assert!(matches!(&events[1], Event::Retried { from } if *from == original.id));

    let draft_after = env
        .store
        .get(draft_dependent.id)
        .expect("get")
        .expect("some");
    assert_eq!(draft_after.status, Status::Draft);
    assert_eq!(draft_after.depends_on, vec![new_id]);

    let cancelled_after = env
        .store
        .get(cancelled_dependent.id)
        .expect("get")
        .expect("some");
    assert_eq!(
        cancelled_after.status,
        Status::Draft,
        "dependency_failed 由来の cancelled は draft に戻す"
    );
    assert_eq!(cancelled_after.depends_on, vec![new_id]);

    let manually_after = env
        .store
        .get(manually_cancelled.id)
        .expect("get")
        .expect("some");
    assert_eq!(
        manually_after.status,
        Status::Cancelled,
        "手動 cancel は対象外"
    );
    assert_eq!(manually_after.depends_on, vec![original.id]);
}

#[tokio::test]
async fn retry_with_accept_starts_ready_and_non_terminal_or_done_is_409() {
    let env = admin_env();
    let app = env.router();

    let failed = new_task(TaskKind::Execute, Status::Failed);
    env.seed(&failed);
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/retry", failed.id),
            &json!({"accept": true}),
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let new_id: TaskId = resp.json()["task_id"]
        .as_str()
        .expect("task_id")
        .parse()
        .expect("parse");
    assert_eq!(env.status_of(new_id), Status::Ready);

    for status in [
        Status::Draft,
        Status::Ready,
        Status::Running,
        Status::Blocked,
        Status::Reviewing,
        Status::Done,
    ] {
        let task = new_task(TaskKind::Execute, status);
        env.seed(&task);
        let resp = send(
            &app,
            post_admin(&format!("/api/v1/tasks/{}/retry", task.id), &json!({})),
        )
        .await;
        let problem = assert_problem(&resp, 409, "invalid_transition");
        assert!(
            problem["detail"]
                .as_str()
                .expect("detail")
                .contains("cannot be retried"),
            "{problem}"
        );
        assert_eq!(env.status_of(task.id), status);
    }
}

/// ADR-0070 D2 追記（Phase 116。本番で確認: `accept` を送らずに「やり直す」を押すと `draft` のまま
/// 止まり、「やり直したのに動かない」状態になった）: 本文を省略、または `accept` を書かなければ
/// 既定で `ready` になる。
#[tokio::test]
async fn retry_defaults_to_ready_when_accept_is_omitted() {
    let env = admin_env();
    let app = env.router();
    let failed = new_task(TaskKind::Execute, Status::Failed);
    env.seed(&failed);

    let resp = send(
        &app,
        post_admin(&format!("/api/v1/tasks/{}/retry", failed.id), &json!({})),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let new_id: TaskId = resp.json()["task_id"]
        .as_str()
        .expect("task_id")
        .parse()
        .expect("parse");
    assert_eq!(env.status_of(new_id), Status::Ready);
}

/// ADR-0070 D2 追記（Phase 116）: `draft` を `ready` にする専用の道具
/// （`POST /tasks/{id}/accept`）。`draft` 以外は 409。
#[tokio::test]
async fn accept_moves_a_draft_task_to_ready_and_rejects_other_statuses() {
    let env = admin_env();
    let app = env.router();
    let draft = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&draft);

    let resp = send(
        &app,
        post_admin(&format!("/api/v1/tasks/{}/accept", draft.id), &json!({})),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(env.status_of(draft.id), Status::Ready);

    let ready = new_task(TaskKind::Execute, Status::Ready);
    env.seed(&ready);
    let resp = send(
        &app,
        post_admin(&format!("/api/v1/tasks/{}/accept", ready.id), &json!({})),
    )
    .await;
    assert_problem(&resp, 409, "invalid_transition");
}

/// ADR-0062 Phase 108（本番の事故、2026-09-23）: `POST /tasks/{id}/retry` の `workspace` で
/// 複製先の作業場所を差し替える（remote で失敗し続けたタスクを Local にしてやり直す）。
/// `Remote.cluster` が設定に無ければ 422（`validated_workspace`、`PATCH` と同じ層で見る）。
#[tokio::test]
async fn retry_workspace_override_switches_to_local_and_validates_the_cluster() {
    let env = admin_env();
    let app = env.router();

    let mut failed = new_task(TaskKind::Execute, Status::Failed);
    failed.workspace = task_core::WorkspaceSpec::Remote {
        cluster: "pegasus".into(),
        path: "~".into(),
        mode: None,
    };
    env.seed(&failed);

    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/retry", failed.id),
            &json!({"workspace": {"kind": "local", "path": "/tmp/retry-local"}}),
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let new_id: TaskId = resp.json()["task_id"]
        .as_str()
        .expect("task_id")
        .parse()
        .expect("parse");
    let new_task_row = env.store.get(new_id).expect("get").expect("some");
    assert_eq!(
        new_task_row.workspace,
        task_core::WorkspaceSpec::local("/tmp/retry-local")
    );

    // 設定に無いクラスタは 422（複製は作られない）。
    let failed2 = new_task(TaskKind::Execute, Status::Failed);
    env.seed(&failed2);
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/retry", failed2.id),
            &json!({"workspace": {"kind": "remote", "cluster": "no-such-cluster", "path": "~"}}),
        ),
    )
    .await;
    assert_problem(&resp, 422, "validation");
}

#[tokio::test]
async fn create_task_returns_201_with_location_and_cli_defaults() {
    let env = admin_env();
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
    let resp = send(&app, post_admin("/api/v1/tasks", &body)).await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let task = resp.json();
    let id = task["id"].as_str().expect("id").to_string();
    assert_eq!(
        resp.header("location"),
        Some(format!("/api/v1/tasks/{id}").as_str())
    );
    assert_eq!(task["kind"], "execute");
    // ADR-0044 D1（Phase 53）: `POST /tasks` は人の作成なので `ready`（Go を挟まない）。
    assert_eq!(task["status"], "ready");
    // ADR-0044 D3: 省略した優先度は P2（= 10）、種類は `other`、ラベルは無し。
    assert_eq!(task["priority"], 10);
    assert_eq!(
        task.get("category"),
        None,
        "既定の other は JSON に出さない"
    );
    assert_eq!(task.get("labels"), None, "空のラベルは JSON に出さない");
    assert_eq!(
        task["worker_hint"],
        json!({"tier": "standard", "adapter": null})
    );
    assert_eq!(
        task["budget"],
        json!({"max_turns": 10, "max_wall_secs": 600, "max_retries": 2})
    );
    assert_eq!(task["workspace"], json!({"kind": "local", "path": id}));
    let texts: Vec<&str> = task["acceptance"]
        .as_array()
        .expect("acceptance")
        .iter()
        .map(|c| c["text"].as_str().expect("text"))
        .collect();
    assert_eq!(
        texts,
        vec![
            "the diff is minimal",
            "`cargo test` exits 0",
            "artifact bench.json exists",
            "reviewer is happy"
        ]
    );
    let stored = env
        .store
        .get(id.parse().expect("id"))
        .expect("get")
        .expect("stored");
    assert_eq!(serde_json::to_value(&stored).expect("json"), task);

    // ADR-0067 D2: `human` チェックには artifacts か知識ベースの参照が要る。
    let approval = json!({"title": "gate", "objective": "o", "kind": "approval", "acceptance": [{"type": "human", "text": "ok"}, {"type": "artifact_exists", "name": "result.md"}]});
    let resp = send(&app, post_admin("/api/v1/tasks", &approval)).await;
    assert_eq!(resp.status, 201);
    assert_eq!(resp.json()["status"], "ready");

    // ADR-0044 D1: `status: "draft"` を明示したときだけ Go 待ちの draft で始まる。
    let drafted = json!({"title": "later", "objective": "o", "acceptance": [{"type": "human", "text": "ok"}, {"type": "artifact_exists", "name": "result.md"}], "status": "draft"});
    let resp = send(&app, post_admin("/api/v1/tasks", &drafted)).await;
    assert_eq!(resp.status, 201);
    assert_eq!(resp.json()["status"], "draft");
    // `running` のような初期状態は 422。
    let bogus = json!({"title": "x", "objective": "o", "acceptance": [{"type": "human", "text": "ok"}, {"type": "artifact_exists", "name": "result.md"}], "status": "running"});
    assert_eq!(
        send(&app, post_admin("/api/v1/tasks", &bogus)).await.status,
        422
    );
}

#[tokio::test]
async fn create_task_validation_errors_insert_nothing() {
    let env = admin_env();
    let app = env.router();
    let failed = new_task(TaskKind::Execute, Status::Failed);
    let cancelled = new_task(TaskKind::Execute, Status::Cancelled);
    env.seed(&failed);
    env.seed(&cancelled);
    let missing = TaskId::new();
    // ADR-0067 D2: `human` チェックには artifacts か知識ベースの参照が要る。
    let human =
        json!([{"type": "human", "text": "ok"}, {"type": "artifact_exists", "name": "result.md"}]);

    let cases: Vec<(Value, u16, &str, Option<Value>)> = vec![
        (
            json!({"title": "t", "objective": "o", "acceptance": []}),
            422,
            "validation",
            Some(
                json!([{"field": "acceptance", "message": "at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)"}]),
            ),
        ),
        (
            json!({"title": "t", "objective": "o", "acceptance": human, "depends_on": [missing.to_string()]}),
            422,
            "validation",
            Some(
                json!([{"field": "depends_on", "message": format!("dependency {missing} does not exist")}]),
            ),
        ),
        (
            json!({"title": "t", "objective": "o", "acceptance": human, "depends_on": [failed.id.to_string()]}),
            422,
            "validation",
            Some(
                json!([{"field": "depends_on", "message": format!("dependency {} has status Failed and cannot be depended on", failed.id)}]),
            ),
        ),
        (
            json!({"title": "t", "objective": "o", "acceptance": human, "depends_on": [cancelled.id.to_string()]}),
            422,
            "validation",
            Some(
                json!([{"field": "depends_on", "message": format!("dependency {} has status Cancelled and cannot be depended on", cancelled.id)}]),
            ),
        ),
        (
            json!({"objective": "o", "acceptance": human}),
            400,
            "bad_request",
            None,
        ),
    ];
    for (body, status, code, errors) in cases {
        let resp = send(&app, post_admin("/api/v1/tasks", &body)).await;
        let problem = assert_problem(&resp, status, code);
        if let Some(errors) = errors {
            assert_eq!(problem["errors"], errors);
            assert_eq!(problem["detail"], errors[0]["message"]);
        }
    }
    assert_eq!(
        env.store.list(None).expect("list").len(),
        2,
        "nothing was inserted"
    );
}

#[tokio::test]
async fn create_plan_returns_201_and_rejects_blank_goals() {
    let env = admin_env();
    let app = env.router();
    let resp = send(
        &app,
        post_admin(
            "/api/v1/plans",
            &json!({"goal": "build the CLI\nwith tests"}),
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let plan = resp.json();
    let id = plan["id"].as_str().expect("id");
    assert_eq!(
        resp.header("location"),
        Some(format!("/api/v1/tasks/{id}").as_str())
    );
    assert_eq!(plan["kind"], "plan");
    assert_eq!(plan["status"], "draft");
    assert_eq!(plan["title"], "build the CLI");
    assert_eq!(plan["acceptance"], json!([]));
    assert_eq!(plan["parent_id"], Value::Null);
    assert_eq!(plan["worker_hint"]["tier"], "frontier");
    assert_eq!(
        plan["budget"],
        json!({"max_turns": 30, "max_wall_secs": 900, "max_retries": 1})
    );

    let blank = send(&app, post_admin("/api/v1/plans", &json!({"goal": "  \n "}))).await;
    let problem = assert_problem(&blank, 422, "validation");
    assert_eq!(
        problem["errors"],
        json!([{"field": "goal", "message": "goal must not be blank"}])
    );
    assert_eq!(env.store.list(None).expect("list").len(), 1);
}

#[tokio::test]
async fn replay_reports_zero_mismatches_after_api_operations() {
    let env = admin_env();
    let app = env.router();
    // ADR-0067 D2: `human` チェックには artifacts か知識ベースの参照が要る。
    let created = send(
        &app,
        post_admin(
            "/api/v1/tasks",
            &json!({"title": "t", "objective": "o", "acceptance": [{"type": "human", "text": "ok"}, {"type": "artifact_exists", "name": "result.md"}]}),
        ),
    )
    .await
    .json();
    let id = created["id"].as_str().expect("id").to_string();
    // ADR-0044 D1: `POST /tasks` は `ready` で作るので approve は要らない（cancel だけ通す）。
    assert_eq!(
        send(
            &app,
            post_admin(&format!("/api/v1/tasks/{id}/cancel"), &json!({}))
        )
        .await
        .status,
        200
    );
    assert_eq!(
        send(&app, post_admin("/api/v1/plans", &json!({"goal": "g"})))
            .await
            .status,
        201
    );

    let resp = send(&app, post_admin("/api/v1/replay", &json!({}))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json(), json!({"tasks": 2, "mismatches": []}));

    let empty = axum::http::Request::post("/api/v1/replay")
        .header("host", HOST)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(axum::body::Body::empty())
        .expect("request");
    assert_eq!(send(&app, empty).await.status, 200);
}

#[tokio::test]
async fn rejecting_an_approval_lists_cancelled_children_in_cascaded() {
    let env = admin_env();
    let app = env.router();
    let approval = new_task(TaskKind::Approval, Status::Ready);
    let mut child = new_task(TaskKind::Execute, Status::Ready);
    child.parent_id = Some(approval.id);
    env.seed(&approval);
    env.seed(&child);

    let resp = send(
        &app,
        post_admin(&format!("/api/v1/tasks/{}/reject", approval.id), &json!({})),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let cascaded = resp.json()["cascaded"]
        .as_array()
        .cloned()
        .expect("cascaded");
    assert_eq!(cascaded.len(), 1);
    assert_eq!(cascaded[0]["id"], child.id.to_string());
    assert_eq!(cascaded[0]["status"], "cancelled");
    assert_eq!(env.status_of(child.id), Status::Cancelled);
}

#[tokio::test]
async fn cancelling_a_predecessor_lists_dependents_in_cascaded() {
    let env = admin_env();
    let app = env.router();
    let first = new_task(TaskKind::Execute, Status::Ready);
    let mut second = new_task(TaskKind::Execute, Status::Draft);
    second.depends_on = vec![first.id];
    let mut third = new_task(TaskKind::Execute, Status::Draft);
    third.depends_on = vec![second.id];
    env.seed(&first);
    env.seed(&second);
    env.seed(&third);

    let resp = send(
        &app,
        post_admin(&format!("/api/v1/tasks/{}/cancel", first.id), &json!({})),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let cascaded: Vec<String> = resp.json()["cascaded"]
        .as_array()
        .expect("cascaded")
        .iter()
        .map(|r| r["id"].as_str().expect("id").to_string())
        .collect();
    assert!(
        cascaded.contains(&second.id.to_string()) && cascaded.contains(&third.id.to_string()),
        "{cascaded:?}"
    );
    let reason = env
        .store
        .events_for(second.id)
        .expect("events")
        .into_iter()
        .rev()
        .find_map(|(_, e)| match e {
            Event::Transitioned { reason, .. } => Some(reason),
            _ => None,
        });
    assert_eq!(reason.as_deref(), Some("dependency_failed"));
}

// ---- ADR-0044 §5 Phase 53 追記（Phase 55）: 変更を伴う API はすべて管理系 ----

/// `answer` / `cancel` / `retry` / `POST /tasks` / `approve` / `reject` / `POST /plans` / `POST /replay`、
/// および案件・途中目標の変更系は、`PATCH` / コメント / `reopen` と同じく **bearer 必須**。
/// トークンが無ければ 401 `unauthorized`、正しいトークンなら 401 にはならない。
#[tokio::test]
async fn every_mutating_endpoint_requires_a_bearer_token() {
    let env = admin_env();
    let app = env.router();
    let blocked = blocked_task(&env);
    let ready = new_task(TaskKind::Execute, Status::Ready);
    env.seed(&ready);
    let failed = new_task(TaskKind::Execute, Status::Failed);
    env.seed(&failed);
    let draft = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&draft);

    // ADR-0067 D2: `human` チェックには artifacts か知識ベースの参照が要る。
    let task_body = json!({"title": "t", "objective": "o", "acceptance": [{"type": "human", "text": "ok"}, {"type": "artifact_exists", "name": "result.md"}]});
    let cases: Vec<(String, Value)> = vec![
        ("/api/v1/tasks".to_string(), task_body.clone()),
        (format!("/api/v1/tasks/{}/approve", ready.id), json!({})),
        (format!("/api/v1/tasks/{}/reject", ready.id), json!({})),
        (
            format!("/api/v1/tasks/{}/answer", blocked.id),
            json!({"answer": "sqlite"}),
        ),
        (format!("/api/v1/tasks/{}/cancel", ready.id), json!({})),
        (format!("/api/v1/tasks/{}/retry", failed.id), json!({})),
        // ADR-0070 D2 追記（Phase 116）。
        (format!("/api/v1/tasks/{}/accept", draft.id), json!({})),
        ("/api/v1/plans".to_string(), json!({"goal": "g"})),
        ("/api/v1/replay".to_string(), json!({})),
    ];
    for (path, body) in &cases {
        let resp = send(&app, post_json(path, body)).await;
        assert_problem(&resp, 401, "unauthorized");
    }
    // 何も起きていない（401 は本文を読む前に返る）。
    assert_eq!(env.status_of(ready.id), Status::Ready);
    assert_eq!(env.status_of(blocked.id), Status::Blocked);
    assert_eq!(env.status_of(draft.id), Status::Draft);
    assert_eq!(env.store.list(None).expect("list").len(), 4);

    // トークンを付ければ 401 ではなくなる（成否はそれぞれの状態機械の話）。
    for (path, body) in &cases {
        let resp = send(&app, post_admin(path, body)).await;
        assert_ne!(resp.status, 401, "{path}: {}", resp.text());
    }
}

/// 案件・途中目標の変更系（`PATCH /projects/{id}`、`POST /projects/{id}/milestones`、
/// `PATCH /milestones/{id}`、および ADR-0044 D6 の 8 つ）も bearer 必須。
#[tokio::test]
async fn project_and_milestone_mutations_require_a_bearer_token() {
    let env = admin_env();
    let app = env.router();
    let project = send(
        &app,
        post_admin(
            "/api/v1/projects",
            &json!({"title": "案件", "request": "やって"}),
        ),
    )
    .await;
    assert_eq!(project.status, 201, "{}", project.text());
    let project_id = project.json()["id"].as_str().expect("id").to_string();
    let milestone = send(
        &app,
        post_admin(
            &format!("/api/v1/projects/{project_id}/milestones"),
            &json!({"title": "途中目標"}),
        ),
    )
    .await;
    assert_eq!(milestone.status, 201, "{}", milestone.text());
    let milestone_id = milestone.json()["id"].as_str().expect("id").to_string();

    let posts = [
        format!("/api/v1/projects/{project_id}/milestones"),
        format!("/api/v1/projects/{project_id}/cancel"),
        format!("/api/v1/projects/{project_id}/pause"),
        format!("/api/v1/projects/{project_id}/resume"),
        format!("/api/v1/projects/{project_id}/archive"),
        format!("/api/v1/projects/{project_id}/unarchive"),
        format!("/api/v1/milestones/{milestone_id}/cancel"),
        format!("/api/v1/milestones/{milestone_id}/pause"),
        format!("/api/v1/milestones/{milestone_id}/resume"),
    ];
    for path in &posts {
        assert_problem(
            &send(&app, post_json(path, &json!({}))).await,
            401,
            "unauthorized",
        );
    }
    assert_problem(
        &send(
            &app,
            patch_json_with(
                &format!("/api/v1/projects/{project_id}"),
                &json!({"status": "active"}),
                &[],
            ),
        )
        .await,
        401,
        "unauthorized",
    );
    assert_problem(
        &send(
            &app,
            patch_json_with(
                &format!("/api/v1/milestones/{milestone_id}"),
                &json!({"status": "approved"}),
                &[],
            ),
        )
        .await,
        401,
        "unauthorized",
    );

    // 何も変わっていない。
    let detail = send(&app, get_admin(&format!("/api/v1/projects/{project_id}")))
        .await
        .json();
    assert_eq!(detail["project"]["status"], "proposed");
    assert_eq!(detail["milestones"][0]["status"], "proposed");
}
