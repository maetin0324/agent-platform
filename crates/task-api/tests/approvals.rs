//! ADR-0033 D5（Phase 26）: `GET /approvals`・`POST /approvals/{id}/decide`・`GET /standing-rules`・
//! `POST /standing-rules`・`DELETE /standing-rules/{id}`。
//!
//! 見るもの: 一覧の絞り込み、`once`/`standing`/`denied` それぞれの後にタスクが再開して `answers[]` に
//! 入ること、`standing` で `standing_rules` に入ること、管理系の 401（トークンあり・`token_file` 未設定の
//! **両方**）、404。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::approval::{Approval, ApprovalId, ApprovalStore, StandingRuleId};
use task_core::org::{OrgKind, OrgNode};
use task_core::{
    Budget, Check, Criterion, ProjectId, Status, Task, TaskId, TaskKind, TaskStore, Tier,
    WorkerHint, WorkspaceSpec,
};
use time::OffsetDateTime;

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

fn d(path: &str) -> axum::http::Request<axum::body::Body> {
    delete_with(
        path,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn env_with_token() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    })
}

fn node(id: &str, parent: Option<&str>, kind: OrgKind) -> OrgNode {
    let now = OffsetDateTime::now_utc();
    OrgNode {
        profile: Default::default(),
        id: id.into(),
        parent_id: parent.map(str::to_string),
        name: id.into(),
        kind,
        genre: None,
        brief: String::new(),
        position: 0,
        created_at: now,
        updated_at: now,
    }
}

fn seed_org(env: &TestEnv) {
    env.store
        .org_upsert(&node("secretary", None, OrgKind::Secretary))
        .expect("seed");
    env.store
        .org_upsert(&node("coding-poc", Some("secretary"), OrgKind::Section))
        .expect("seed");
}

/// `Blocked` のタスク（既存の「質問に答える」経路で再開できる状態）と、それを指す保留中の `Approval` を作る。
fn blocked_task_with_approval(
    env: &TestEnv,
    node_id: &str,
    project: Option<ProjectId>,
) -> (Task, Approval) {
    let now = OffsetDateTime::now_utc();
    let id = TaskId::new();
    let task = Task {
        mode: Default::default(),
        skills: Vec::new(),
        repos: Vec::new(),
        id,
        parent_id: None,
        kind: TaskKind::Execute,
        title: "調べる".into(),
        objective: "o".into(),
        acceptance: vec![Criterion {
            text: "c".into(),
            check: Check::Human,
        }],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Blocked,
        priority: 0,
        worker_hint: WorkerHint {
            tier: Tier::Standard,
            adapter: None,
        },
        workspace: WorkspaceSpec::Local {
            path: "ws".into(),
            mode: None,
        },
        budget: Budget {
            max_turns: 1,
            max_wall_secs: 1,
            max_retries: 0,
        },
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
        role: None,
        genre: None,
        aggregate: false,
        project_id: project,
        milestone_id: None,
        assignee: Some(node_id.to_string()),
        conversation: None,
        labels: Vec::new(),
        category: Default::default(),
    };
    env.store.create_task(&task, vec![]).expect("create task");
    let approval = Approval {
        id: ApprovalId::new(),
        project_id: project,
        node_id: node_id.to_string(),
        task_id: Some(id),
        question: "どのクラスタを使いますか".into(),
        decision: None,
        answer: None,
        created_at: now,
        decided_at: None,
    };
    env.store.approval_append(&approval).expect("append");
    (task, approval)
}

#[tokio::test]
async fn approvals_are_listed_oldest_first_and_can_be_filtered() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&env);
    let project = ProjectId::new();
    let (_, a) = blocked_task_with_approval(&env, "coding-poc", Some(project));
    let (_, b) = blocked_task_with_approval(&env, "secretary", None);

    let resp = send(&app, g("/api/v1/approvals")).await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], a.id.to_string(), "oldest first");

    let resp = send(&app, g("/api/v1/approvals?node=secretary")).await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], b.id.to_string());

    let resp = send(&app, g(&format!("/api/v1/approvals?project={project}"))).await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], a.id.to_string());

    // 決定済みは `pending=true` から外れる。
    env.store
        .approval_decide(
            a.id,
            task_core::approval::Decision::Once,
            Some("pegasus".into()),
            OffsetDateTime::now_utc(),
        )
        .expect("decide");
    let resp = send(&app, g("/api/v1/approvals?pending=true")).await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], b.id.to_string());

    // 全件（pending 無し）は両方出る。
    let resp = send(&app, g("/api/v1/approvals")).await;
    assert_eq!(resp.json()["items"].as_array().map(Vec::len), Some(2));

    // R5（Phase 27）: `pending=false` は**決定済みだけ**（以前は全件を返していた）。
    let resp = send(&app, g("/api/v1/approvals?pending=false")).await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["id"], a.id.to_string());
    assert_eq!(items[0]["decision"], "once");
    // 他の絞り込みと AND で効く。
    let resp = send(&app, g("/api/v1/approvals?pending=false&node=secretary")).await;
    assert_eq!(resp.json()["items"].as_array().map(Vec::len), Some(0));
    let resp = send(
        &app,
        g(&format!(
            "/api/v1/approvals?pending=false&project={project}"
        )),
    )
    .await;
    assert_eq!(resp.json()["items"].as_array().map(Vec::len), Some(1));

    // 知らないクエリは 400。
    let resp = send(&app, g("/api/v1/approvals?nope=1")).await;
    assert_eq!(resp.status.as_u16(), 400);
}

#[tokio::test]
async fn once_reopens_the_task_through_the_existing_answer_path() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&env);
    let (task, approval) = blocked_task_with_approval(&env, "coding-poc", None);

    let resp = send(
        &app,
        p(
            &format!("/api/v1/approvals/{}/decide", approval.id),
            &json!({"decision": "once", "answer": "pegasus"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["approval"]["decision"], "once");
    assert_eq!(body["approval"]["answer"], "pegasus");
    assert!(body["approval"]["decided_at"].is_string());
    assert_eq!(body["transition"]["to"], "ready");
    assert!(body.get("standing_rule").is_none(), "{body}");

    assert_eq!(
        env.status_of(task.id),
        Status::Ready,
        "既存の答える経路で再開する"
    );
    let events = env.store.events_for(task.id).expect("events");
    let answered = events.iter().rev().find_map(|(_, e)| match e {
        task_core::Event::Answered { answer, .. } => Some(answer.clone()),
        _ => None,
    });
    assert_eq!(answered.as_deref(), Some("pegasus"), "answers[] に積まれる");

    // standing_rules は増えない。
    assert!(env.store.standing_rule_list(None).expect("list").is_empty());
}

#[tokio::test]
async fn standing_also_reopens_the_task_and_adds_a_rule_scoped_to_the_node_by_default() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&env);
    let (task, approval) = blocked_task_with_approval(&env, "coding-poc", None);

    let resp = send(
        &app,
        p(
            &format!("/api/v1/approvals/{}/decide", approval.id),
            &json!({"decision": "standing", "answer": "pegasus は 1 ノードで始めてよい"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["standing_rule"]["node_id"], "coding-poc");
    assert_eq!(
        body["standing_rule"]["rule"],
        "pegasus は 1 ノードで始めてよい"
    );
    assert_eq!(body["transition"]["to"], "ready");

    assert_eq!(env.status_of(task.id), Status::Ready);
    let rules = env
        .store
        .standing_rule_list(Some("coding-poc"))
        .expect("list");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].node_id.as_deref(), Some("coding-poc"));

    // 他ノードには効かない。
    assert!(
        env.store
            .standing_rule_list(Some("secretary"))
            .expect("list")
            .is_empty()
    );
}

#[tokio::test]
async fn standing_with_scope_all_applies_to_every_node() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&env);
    let (_, approval) = blocked_task_with_approval(&env, "coding-poc", None);

    let resp = send(
        &app,
        p(
            &format!("/api/v1/approvals/{}/decide", approval.id),
            &json!({"decision": "standing", "answer": "深夜は連絡しない", "scope": "all"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(resp.json()["standing_rule"]["node_id"], Value::Null);

    // 全員向けなので他ノードの一覧にも出る。
    assert_eq!(
        env.store
            .standing_rule_list(Some("secretary"))
            .expect("list")
            .len(),
        1
    );

    // 未知の scope は 400。
    let (_, approval2) = blocked_task_with_approval(&env, "coding-poc", None);
    let resp = send(
        &app,
        p(
            &format!("/api/v1/approvals/{}/decide", approval2.id),
            &json!({"decision": "standing", "answer": "x", "scope": "bogus"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 400);
}

#[tokio::test]
async fn denied_prefixes_the_answer_so_the_worker_can_tell() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&env);
    let (task, approval) = blocked_task_with_approval(&env, "coding-poc", None);

    let resp = send(
        &app,
        p(
            &format!("/api/v1/approvals/{}/decide", approval.id),
            &json!({"decision": "denied", "answer": "予算超過"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(resp.json()["approval"]["decision"], "denied");

    assert_eq!(env.status_of(task.id), Status::Ready);
    let events = env.store.events_for(task.id).expect("events");
    let answered = events.iter().rev().find_map(|(_, e)| match e {
        task_core::Event::Answered { answer, .. } => Some(answer.clone()),
        _ => None,
    });
    assert_eq!(answered.as_deref(), Some("認めない: 予算超過"));
}

/// GUI 監査 H2（Phase 29）: `POST /tasks/{id}/answer` で答えたときも、そのタスクの未決の approvals が
/// `once` + 同じ答えで決定済みになる（`GET /approvals?pending=true` からその行が消える）。
#[tokio::test]
async fn answering_a_task_directly_also_settles_its_pending_approval() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&env);
    let (task, approval) = blocked_task_with_approval(&env, "coding-poc", None);

    let resp = send(
        &app,
        p(
            &format!("/api/v1/tasks/{}/answer", task.id),
            &json!({"answer": "pegasus"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(env.status_of(task.id), Status::Ready);

    let decided = env
        .store
        .approval_get(approval.id)
        .expect("get")
        .expect("some");
    assert_eq!(decided.decision, Some(task_core::approval::Decision::Once));
    assert_eq!(decided.answer.as_deref(), Some("pegasus"));

    let resp = send(&app, g("/api/v1/approvals?pending=true")).await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert!(items.is_empty(), "{items:?}");
}

#[tokio::test]
async fn deciding_an_unknown_approval_is_404_and_a_blank_answer_is_422() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&env);
    let (_, approval) = blocked_task_with_approval(&env, "coding-poc", None);

    let resp = send(
        &app,
        p(
            &format!("/api/v1/approvals/{}/decide", ApprovalId::new()),
            &json!({"decision": "once", "answer": "x"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 404);
    assert_eq!(resp.json()["code"], "approval_not_found");

    let resp = send(
        &app,
        p(
            "/api/v1/approvals/not-a-ulid/decide",
            &json!({"decision": "once", "answer": "x"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 404);

    let resp = send(
        &app,
        p(
            &format!("/api/v1/approvals/{}/decide", approval.id),
            &json!({"decision": "once", "answer": "   "}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 422, "{}", resp.text());
}

#[tokio::test]
async fn standing_rules_can_be_listed_created_and_deleted() {
    let env = env_with_token();
    let app = env.router();

    let resp = send(
        &app,
        p(
            "/api/v1/standing-rules",
            &json!({"rule": "深夜は連絡しない"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    let global = resp.json();
    assert_eq!(global["node_id"], Value::Null);

    let resp = send(
        &app,
        p(
            "/api/v1/standing-rules",
            &json!({"node_id": "coding-poc", "rule": "pegasus は 1 ノードで始めてよい"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201);
    let for_coding = resp.json();

    let resp = send(&app, g("/api/v1/standing-rules")).await;
    assert_eq!(
        resp.json()["items"].as_array().map(Vec::len),
        Some(2),
        "絞り込み無しは全件"
    );

    let resp = send(&app, g("/api/v1/standing-rules?node=coding-poc")).await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 2, "全員向け + そのノード向け");

    let resp = send(
        &app,
        d(&format!(
            "/api/v1/standing-rules/{}",
            for_coding["id"].as_str().expect("id")
        )),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 204);
    let resp = send(&app, g("/api/v1/standing-rules?node=coding-poc")).await;
    assert_eq!(resp.json()["items"].as_array().map(Vec::len), Some(1));

    // 無い id は 404（2 回目の削除、ULID でない文字列も）。
    let resp = send(
        &app,
        d(&format!(
            "/api/v1/standing-rules/{}",
            for_coding["id"].as_str().expect("id")
        )),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 404);
    let resp = send(&app, d("/api/v1/standing-rules/not-a-ulid")).await;
    assert_eq!(resp.status.as_u16(), 404);

    // 空白だけの rule は 422。
    let resp = send(&app, p("/api/v1/standing-rules", &json!({"rule": "   "}))).await;
    assert_eq!(resp.status.as_u16(), 422);
    let _ = global;
}

/// トークンを設定した構成での管理系（decide / standing-rules の作成・削除）の 401。
#[tokio::test]
async fn admin_operations_require_a_token_when_one_is_configured() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&env);
    let (_, approval) = blocked_task_with_approval(&env, "coding-poc", None);

    let resp = send(
        &app,
        post_json(
            &format!("/api/v1/approvals/{}/decide", approval.id),
            &json!({"decision": "once", "answer": "x"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401);
    let resp = send(
        &app,
        post_json("/api/v1/standing-rules", &json!({"rule": "x"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401);
    let resp = send(
        &app,
        delete_with(
            &format!("/api/v1/standing-rules/{}", StandingRuleId::new()),
            &[],
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401);

    // 読み取りは従来どおり通る（トークン付きなら）。
    let resp = send(&app, g("/api/v1/approvals")).await;
    assert_eq!(resp.status.as_u16(), 200);
    let resp = send(&app, g("/api/v1/standing-rules")).await;
    assert_eq!(resp.status.as_u16(), 200);
}

/// `token_file` が無い構成でも、管理系は 401（ADR-0017 D1 と同じ規律）。
#[tokio::test]
async fn admin_operations_are_401_even_without_a_configured_token() {
    let env = TestEnv::new();
    let app = env.router();
    seed_org(&env);
    let (_, approval) = blocked_task_with_approval(&env, "coding-poc", None);

    let resp = send(
        &app,
        post_json(
            &format!("/api/v1/approvals/{}/decide", approval.id),
            &json!({"decision": "once", "answer": "x"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401);
    let resp = send(
        &app,
        post_json("/api/v1/standing-rules", &json!({"rule": "x"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401);
    let resp = send(
        &app,
        delete_with(
            &format!("/api/v1/standing-rules/{}", StandingRuleId::new()),
            &[],
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401);

    // 読み取りは無認証でも通る。
    let resp = send(&app, get("/api/v1/approvals")).await;
    assert_eq!(resp.status.as_u16(), 200);
    let resp = send(&app, get("/api/v1/standing-rules")).await;
    assert_eq!(resp.status.as_u16(), 200);
}

#[tokio::test]
async fn the_daemon_snapshot_carries_the_pending_approval_count() {
    let env = TestEnv::new();
    let app = env.router();
    env.daemon_tx.send_replace(Some(snapshot(3)));
    seed_org(&env);

    let resp = send(&app, get("/api/v1/daemon")).await;
    assert_eq!(resp.json()["snapshot"]["approvals_pending"], 0);

    let (_, approval) = blocked_task_with_approval(&env, "coding-poc", None);
    let resp = send(&app, get("/api/v1/daemon")).await;
    assert_eq!(resp.json()["snapshot"]["approvals_pending"], 1);

    env.store
        .approval_decide(
            approval.id,
            task_core::approval::Decision::Once,
            Some("x".into()),
            OffsetDateTime::now_utc(),
        )
        .expect("decide");
    let resp = send(&app, get("/api/v1/daemon")).await;
    assert_eq!(resp.json()["snapshot"]["approvals_pending"], 0);
}
