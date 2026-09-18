//! ADR-0038 D2（Phase 41）: `POST /milestones/{id}/decide`（`ok` / `discuss` / `ng`）と、
//! `GET /projects/{id}` の途中目標に付く `review` / `proposal`。
//!
//! 見るもの: 3 経路の状態遷移・計画 run の起動・対話の送信・提案の差し替え、`discuss` / `ng` の空 `note`
//! が 422、管理系の 401（トークンあり構成と `token_file` 未設定構成の両方）、404、409（`reached` 済み）。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{MilestoneStatus, Status, TaskKind, TaskStore};

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(path, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn p(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    post_json_with(path, body, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn env_with_token() -> TestEnv {
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

async fn create_project(app: &axum::Router) -> String {
    let resp = send(
        app,
        p("/api/v1/projects", &json!({"title": "Pluvio", "request": "隣接分野を探して欲しい"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    resp.json()["id"].as_str().expect("id").to_string()
}

async fn create_milestone(app: &axum::Router, project_id: &str, title: &str, status: &str) -> String {
    let resp = send(
        app,
        p(
            &format!("/api/v1/projects/{project_id}/milestones"),
            &json!({"title": title, "status": status}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    resp.json()["id"].as_str().expect("id").to_string()
}

fn milestone_status(detail: &Value, id: &str) -> String {
    detail["milestones"]
        .as_array()
        .expect("milestones")
        .iter()
        .find(|m| m["id"] == id)
        .unwrap_or_else(|| panic!("no milestone {id} in {detail}"))["status"]
        .as_str()
        .expect("status")
        .to_string()
}

/// ADR-0038 D2: `ok` は達成にして、提案された次の途中目標を承認し、その分解（計画 run）を起こす。
#[tokio::test]
async fn ok_reaches_the_milestone_approves_the_proposal_and_starts_the_decomposition() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    let project_id = create_project(&app).await;
    let milestone_id = create_milestone(&app, &project_id, "隣接領域の動向調査", "in_progress").await;
    let proposal_id = create_milestone(&app, &project_id, "候補の比較実験", "proposed").await;

    let resp = send(
        &app,
        p(
            &format!("/api/v1/milestones/{milestone_id}/decide"),
            &json!({"decision": "ok", "note": "その方針で進めてください"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["decision"], "ok");
    assert_eq!(body["milestone"]["status"], "reached");
    assert_eq!(body["next_milestone"]["id"], proposal_id);
    let plan_id: task_core::TaskId = body["plan_task_id"].as_str().expect("plan_task_id").parse().expect("id");

    // 計画 run は Phase 29 と同じ経路（秘書の plan タスク。人の一言が goal に入る）。
    let plan = env.store.get(plan_id).expect("get").expect("some");
    assert_eq!(plan.kind, TaskKind::Plan);
    assert_eq!(plan.status, Status::Ready);
    assert_eq!(plan.assignee.as_deref(), Some("secretary"));
    assert_eq!(plan.milestone_id.map(|m| m.to_string()), Some(proposal_id.clone()));
    assert!(plan.objective.contains("その方針で進めてください"), "{}", plan.objective);

    let detail = send(&app, g(&format!("/api/v1/projects/{project_id}"))).await.json();
    assert_eq!(milestone_status(&detail, &milestone_id), "reached");
    // 承認したうえで分解が始まったので `in_progress`（Phase 29 の計画 run が進める）。
    assert_eq!(milestone_status(&detail, &proposal_id), "in_progress");

    // 人の一言は秘書との対話にも残る。
    let messages = send(&app, g(&format!("/api/v1/org/secretary/messages?project={project_id}")))
        .await
        .json();
    assert!(
        messages["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|m| m["text"] == "その方針で進めてください" && m["role"] == "user"),
        "{messages}"
    );

    // 達成済みの途中目標はもう判定できない（409）。
    let resp = send(
        &app,
        p(&format!("/api/v1/milestones/{milestone_id}/decide"), &json!({"decision": "ok"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 409, "{}", resp.text());
    assert_eq!(resp.json()["code"], "milestone_reached", "{}", resp.text());
}

/// ADR-0038 D2: `discuss` は何も変えず、`note` を秘書への対話として送る。
#[tokio::test]
async fn discuss_changes_nothing_and_sends_the_note_to_the_secretary() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    let project_id = create_project(&app).await;
    let milestone_id = create_milestone(&app, &project_id, "隣接領域の動向調査", "in_progress").await;
    let proposal_id = create_milestone(&app, &project_id, "候補の比較実験", "proposed").await;

    let resp = send(
        &app,
        p(
            &format!("/api/v1/milestones/{milestone_id}/decide"),
            &json!({"decision": "discuss", "note": "候補 B の根拠が弱いのでは"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["milestone"]["status"], "in_progress");
    assert!(body["plan_task_id"].is_null(), "{body}");
    let task_id: task_core::TaskId = body["conversation_task_id"]
        .as_str()
        .expect("conversation_task_id")
        .parse()
        .expect("id");
    let task = env.store.get(task_id).expect("get").expect("some");
    assert!(task_core::is_conversation(&task));
    assert_eq!(task.milestone_id, None, "人との議論は裏方のレビュー run ではない");
    assert!(task.objective.contains("候補 B の根拠が弱いのでは"), "{}", task.objective);

    let detail = send(&app, g(&format!("/api/v1/projects/{project_id}"))).await.json();
    assert_eq!(milestone_status(&detail, &milestone_id), "in_progress");
    assert_eq!(milestone_status(&detail, &proposal_id), "proposed");
}

/// ADR-0038 D2: `ng` はこの途中目標と提案を `redesigned` にし、理由 + 再設計の依頼を秘書に送る。
#[tokio::test]
async fn ng_redesigns_the_milestone_and_its_proposal_and_asks_for_a_new_one() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    let project_id = create_project(&app).await;
    let milestone_id = create_milestone(&app, &project_id, "隣接領域の動向調査", "in_progress").await;
    let proposal_id = create_milestone(&app, &project_id, "候補の比較実験", "proposed").await;

    let resp = send(
        &app,
        p(
            &format!("/api/v1/milestones/{milestone_id}/decide"),
            &json!({"decision": "ng", "note": "調査の切り方が違う"}),
        ),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["milestone"]["status"], "redesigned");
    assert_eq!(body["next_milestone"]["status"], "redesigned");
    let task_id: task_core::TaskId = body["conversation_task_id"]
        .as_str()
        .expect("conversation_task_id")
        .parse()
        .expect("id");
    let task = env.store.get(task_id).expect("get").expect("some");
    assert!(task.objective.contains("調査の切り方が違う"), "{}", task.objective);
    assert!(task.objective.contains("再設計"), "{}", task.objective);
    assert!(task.objective.contains("milestone_proposal"), "{}", task.objective);

    let detail = send(&app, g(&format!("/api/v1/projects/{project_id}"))).await.json();
    assert_eq!(milestone_status(&detail, &milestone_id), "redesigned");
    assert_eq!(milestone_status(&detail, &proposal_id), "redesigned");
}

/// ADR-0038 D2: `discuss` / `ng` は理由が要る（空なら 422、何も変わらない）。
#[tokio::test]
async fn a_blank_note_is_422_for_discuss_and_ng_but_optional_for_ok() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    let project_id = create_project(&app).await;
    let milestone_id = create_milestone(&app, &project_id, "隣接領域の動向調査", "in_progress").await;

    for body in [
        json!({"decision": "discuss"}),
        json!({"decision": "discuss", "note": "   "}),
        json!({"decision": "ng", "note": ""}),
    ] {
        let resp = send(&app, p(&format!("/api/v1/milestones/{milestone_id}/decide"), &body)).await;
        assert_eq!(resp.status.as_u16(), 422, "{} for {body}", resp.text());
    }
    let detail = send(&app, g(&format!("/api/v1/projects/{project_id}"))).await.json();
    assert_eq!(milestone_status(&detail, &milestone_id), "in_progress");

    // `ok` では `note` は任意（提案が無ければ計画 run も起きない）。
    let resp = send(
        &app,
        p(&format!("/api/v1/milestones/{milestone_id}/decide"), &json!({"decision": "ok"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["milestone"]["status"], "reached");
    assert!(body["plan_task_id"].is_null(), "提案が無ければ分解は起こさない: {body}");
    assert!(body["next_milestone"].is_null(), "{body}");
}

/// 知らない途中目標は 404（id の形が違うものも）。
#[tokio::test]
async fn an_unknown_milestone_is_404() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    for id in ["01ARZ3NDEKTSV4RRFFQ69G5FAV", "not-an-ulid"] {
        let resp = send(&app, p(&format!("/api/v1/milestones/{id}/decide"), &json!({"decision": "ok"}))).await;
        assert_eq!(resp.status.as_u16(), 404, "{}", resp.text());
        assert_eq!(resp.json()["code"], "milestone_not_found", "{}", resp.text());
    }
}

/// 管理系（`token_file` 未設定でも 401）。
#[tokio::test]
async fn deciding_is_an_admin_endpoint() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    let project_id = create_project(&app).await;
    let milestone_id = create_milestone(&app, &project_id, "隣接領域の動向調査", "in_progress").await;

    let resp = send(
        &app,
        post_json(&format!("/api/v1/milestones/{milestone_id}/decide"), &json!({"decision": "ok"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401, "{}", resp.text());

    // `token_file` 未設定の構成でも管理系は 401。
    let open = TestEnv::new();
    let open_app = open.router();
    let resp = send(
        &open_app,
        post_json("/api/v1/milestones/01ARZ3NDEKTSV4RRFFQ69G5FAV/decide", &json!({"decision": "ok"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 401, "{}", resp.text());
}

/// ADR-0038 D1 / D4: `GET /projects/{id}` の途中目標に、秘書のレビューの返事と提案が付く。
#[tokio::test]
async fn the_project_view_carries_the_review_reply_and_the_proposal() {
    let env = env_with_token();
    let app = env.router();
    seed_secretary(&app).await;
    let project_id = create_project(&app).await;
    let milestone_id = create_milestone(&app, &project_id, "隣接領域の動向調査", "in_progress").await;

    // 返事が付く前は `review` も `proposal` も無い。
    let detail = send(&app, g(&format!("/api/v1/projects/{project_id}"))).await.json();
    let card = detail["milestones"]
        .as_array()
        .expect("milestones")
        .iter()
        .find(|m| m["id"] == milestone_id)
        .expect("milestone")
        .clone();
    assert!(card.get("review").is_none(), "{card}");
    assert!(card.get("proposal").is_none(), "{card}");
    assert_eq!(card["title"], "隣接領域の動向調査", "Milestone のフィールドは平らに出る");

    // taskd の tick がするのと同じこと: レビューの対話 → 返事 → 提案。
    let project = env
        .store
        .project_get(project_id.parse().expect("project id"))
        .expect("get")
        .expect("some");
    let milestone = env
        .store
        .milestone_list(project.id)
        .expect("list")
        .into_iter()
        .find(|m| m.id.to_string() == milestone_id)
        .expect("milestone");
    let started = task_ops::milestone_review::start_review(
        &env.store,
        &project,
        &milestone,
        "途中目標『隣接領域の動向調査』の仕事が止まりました。",
        &[],
        &[],
        task_core::CONVERSATION_GENRE,
        time::OffsetDateTime::now_utc(),
    )
    .expect("start review");
    task_ops::conversation::record_reply(
        &env.store,
        &started.task,
        "run-1",
        "候補を 3 本に絞りました。次は比較実験を提案します。",
        time::OffsetDateTime::now_utc(),
    )
    .expect("reply");
    let proposal = task_ops::milestone_review::record_proposal(
        &env.store,
        project.id,
        Some(milestone.id),
        "候補の比較実験",
        "3 本を同じ条件で比べる",
    )
    .expect("record")
    .expect("proposal");

    let detail = send(&app, g(&format!("/api/v1/projects/{project_id}"))).await.json();
    let card = detail["milestones"]
        .as_array()
        .expect("milestones")
        .iter()
        .find(|m| m["id"] == milestone_id)
        .expect("milestone")
        .clone();
    assert_eq!(card["review"]["text"], "候補を 3 本に絞りました。次は比較実験を提案します。", "{card}");
    assert!(card["review"]["message_id"].is_string(), "{card}");
    assert!(card["review"]["at"].is_string(), "{card}");
    assert_eq!(card["proposal"]["id"], proposal.id.to_string(), "{card}");
    assert_eq!(card["proposal"]["title"], "候補の比較実験", "{card}");
    // レビューの対話は裏方として仕事の木から隠せる。
    let support = detail["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .find(|t| t["id"] == started.task.id.to_string())
        .expect("the review task");
    assert_eq!(support["support"], "milestone_review", "{support}");

    // 人が `ok` を押すと、この提案が承認されて分解が始まる。
    let resp = send(
        &app,
        p(&format!("/api/v1/milestones/{milestone_id}/decide"), &json!({"decision": "ok"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    assert_eq!(resp.json()["next_milestone"]["id"], proposal.id.to_string());
    let after = env
        .store
        .milestone_list(project.id)
        .expect("list")
        .into_iter()
        .find(|m| m.id == milestone.id)
        .expect("milestone");
    assert_eq!(after.status, MilestoneStatus::Reached);
}
