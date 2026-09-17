//! ADR-0033 D4（Phase 24）: `POST /org/{id}/messages`・`GET /org/{id}/messages`、そして
//! 案件を作った直後に秘書へ最初の相談が 1 件立つこと（SPEC §7）。
//!
//! 見るもの: 202 の形、対話用タスクの中身、run の `summary` が `role = node` の行になること
//! （偽のディスパッチャ相当の書き込みで確かめる）、`Error` のときの文面、未知のノードは 404、
//! 管理系の 401（トークンあり構成と `token_file` 未設定構成の両方）。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{GenreSpec, MessageRole, RoleSpec, Status, TaskStore, Tier};

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(path, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn p(path: &str, body: &Value) -> axum::http::Request<axum::body::Body> {
    post_json_with(path, body, &[("authorization", format!("Bearer {TOKEN}").as_str())])
}

fn env_with_token() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        roles: vec![
            RoleSpec {
                id: "secretary".into(),
                tier: Some(Tier::Standard),
                adapter: Some("claude-code".into()),
                max_turns: Some(40),
                ..RoleSpec::default()
            },
            RoleSpec {
                id: "literature-reader".into(),
                tier: Some(Tier::Cheap),
                adapter: Some("paperqa".into()),
                ..RoleSpec::default()
            },
        ],
        genres: vec![
            GenreSpec {
                id: "secretary".into(),
                description: "人と話し、案件を組織に流す".into(),
                default_role: Some("secretary".into()),
                roles: vec!["secretary".into()],
                ..GenreSpec::default()
            },
            GenreSpec {
                id: "literature".into(),
                description: "関連研究の調査".into(),
                default_role: Some("literature-reader".into()),
                roles: vec!["literature-reader".into()],
                ..GenreSpec::default()
            },
        ],
        ..Default::default()
    })
}

/// 秘書 → 研究部 → 関連研究調査課。
async fn seed_org(app: &axum::Router) {
    for body in [
        json!({"id": "secretary", "name": "秘書", "kind": "secretary", "genre": "secretary",
               "brief": "案件を受け取り、組織に流す"}),
        json!({"id": "research", "name": "研究部", "kind": "department", "parent_id": "secretary"}),
        json!({"id": "research-survey", "name": "関連研究調査課", "kind": "section",
               "parent_id": "research", "genre": "literature", "brief": "関連研究を洗う"}),
    ] {
        let resp = send(app, p("/api/v1/org", &body)).await;
        assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    }
}

/// 話しかけると `role = user` の行が入り、そのノードの対話用タスクが 1 件 `ready` になる。202 で id が返る。
#[tokio::test]
async fn talking_to_a_node_returns_202_and_makes_one_ready_conversation_task() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    let resp = send(
        &app,
        p("/api/v1/org/research-survey/messages", &json!({"text": "先週の続きで、隣接分野も見てほしい"})),
    )
    .await;
    assert_eq!(resp.status.as_u16(), 202, "{}", resp.text());
    let body = resp.json();
    let task_id: task_core::TaskId = body["task_id"].as_str().expect("task_id").parse().expect("ulid");
    assert!(body["message_id"].as_str().is_some_and(|s| s.len() == 26), "{body}");

    let task = env.store.get(task_id).expect("get").expect("task");
    assert_eq!(task.status, Status::Ready);
    assert_eq!(task.title, "対話: 先週の続きで、隣接分野も見てほしい");
    assert_eq!(task.objective, "先週の続きで、隣接分野も見てほしい");
    assert!(task.acceptance.is_empty());
    assert_eq!(task.assignee.as_deref(), Some("research-survey"));
    assert_eq!(task.genre.as_deref(), Some("literature"));
    assert_eq!(task.worker_hint.adapter.as_deref(), Some("paperqa"));
    assert!(task_core::is_conversation(&task), "対話由来の印が付く");

    // 読み取りは管理系ではない（下の `posting_a_message_is_an_admin_endpoint` で確かめる）。
    // このテスト環境はトークンを設定しているので、共通ガードのぶんだけトークンを付ける。
    let listed = send(&app, g("/api/v1/org/research-survey/messages")).await;
    assert_eq!(listed.status.as_u16(), 200, "{}", listed.text());
    let items = listed.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["role"], "user");
    assert_eq!(items[0]["text"], "先週の続きで、隣接分野も見てほしい");
    assert!(items[0].get("run_id").is_none(), "人の発言に run_id は無い");
    // R4（migration 0007）: 人の発言の行にも対話用タスクの id が入る。
    assert_eq!(items[0]["task_id"], json!(task_id.to_string()));

    // R3: `GET /tasks` の行に `assignee` と `conversation` が出る（GUI が仕事の木から隠せる）。
    let row = send(&app, g("/api/v1/tasks")).await.json()["items"][0].clone();
    assert_eq!(row["id"], json!(task_id.to_string()));
    assert_eq!(row["assignee"], "research-survey");
    assert_eq!(row["conversation"], true);

    // 2 通目は 1 通目の後ろに並ぶ（監査 M-3: 返事は送った順に返る）。
    let second = send(
        &app,
        p("/api/v1/org/research-survey/messages", &json!({"text": "追加で 1 点"})),
    )
    .await;
    assert_eq!(second.status.as_u16(), 202, "{}", second.text());
    let second_id: task_core::TaskId = second.json()["task_id"].as_str().expect("task_id").parse().expect("ulid");
    let second_task = env.store.get(second_id).expect("get").expect("task");
    assert_eq!(second_task.depends_on, vec![task_id]);
    assert!(
        !env.store.ready_tasks(10).expect("ready").iter().any(|t| t.id == second_id),
        "1 通目が終わるまで run しない"
    );
}

/// run が終わると `summary` が `role = node` の行になり（`run_id` 付き）、やり取りが古い順に積み上がる。
/// `Error` の run は「返事できませんでした: …」。ここではディスパッチャの書き込みを直接呼んで確かめる。
#[tokio::test]
async fn the_runs_summary_becomes_the_nodes_reply_and_errors_say_so() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    let accepted = send(&app, p("/api/v1/org/secretary/messages", &json!({"text": "状況を教えて"})))
        .await
        .json();
    let task_id: task_core::TaskId = accepted["task_id"].as_str().expect("id").parse().expect("ulid");
    let task = env.store.get(task_id).expect("get").expect("task");

    task_ops::conversation::record_reply(&env.store, &task, "run-1", "3 本の候補が出ています", time::OffsetDateTime::now_utc())
        .expect("reply");
    task_ops::conversation::record_reply(
        &env.store,
        &task,
        "run-2",
        &task_core::failure_reply("error(retryable=true): adapter: spawn failed"),
        time::OffsetDateTime::now_utc(),
    )
    .expect("reply");

    let items = send(&app, g("/api/v1/org/secretary/messages"))
        .await
        .json()["items"]
        .as_array()
        .cloned()
        .expect("items");
    assert_eq!(items.len(), 3, "古い順: 人 → 返事 → 失敗の返事");
    assert_eq!(items[0]["role"], "user");
    assert_eq!(items[1]["role"], "node");
    assert_eq!(items[1]["text"], "3 本の候補が出ています");
    assert_eq!(items[1]["run_id"], "run-1");
    assert!(
        items[2]["text"].as_str().is_some_and(|t| t.starts_with("返事できませんでした: ")),
        "{items:?}"
    );

    // `limit` は新しい方を残す。
    let last = send(&app, g("/api/v1/org/secretary/messages?limit=1")).await.json();
    assert_eq!(last["items"].as_array().expect("items").len(), 1);
    assert_eq!(last["items"][0]["run_id"], "run-2");
}

/// 案件ごとにスレッドが分かれる（`?project=`）。案件に紐づかない雑談は混ざらない。
#[tokio::test]
async fn threads_are_separated_by_project() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;
    let project = send(
        &app,
        p("/api/v1/projects", &json!({"title": "Pluvio", "request": "新テーマの模索、検証"})),
    )
    .await
    .json();
    let project_id = project["id"].as_str().expect("id").to_string();

    send(&app, p("/api/v1/org/secretary/messages", &json!({"text": "雑談"}))).await;
    let in_project = send(
        &app,
        p(
            "/api/v1/org/secretary/messages",
            &json!({"text": "この案件の方は？", "project_id": project_id}),
        ),
    )
    .await;
    assert_eq!(in_project.status.as_u16(), 202, "{}", in_project.text());

    let scoped = send(&app, g(&format!("/api/v1/org/secretary/messages?project={project_id}"))).await;
    let items = scoped.json()["items"].as_array().cloned().expect("items");
    // 案件を作った時点の最初の相談（SPEC §7）＋ 今の 1 件。
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["text"], "新テーマの模索、検証");
    assert_eq!(items[1]["text"], "この案件の方は？");

    let chat = send(&app, g("/api/v1/org/secretary/messages")).await.json();
    assert_eq!(chat["items"].as_array().expect("items").len(), 1);
    assert_eq!(chat["items"][0]["text"], "雑談");

    // 知らない案件の id は「そのスレッドが無い」だけ（空の一覧）。ULID でない文字列は 404。
    let unknown = send(&app, g("/api/v1/org/secretary/messages?project=01J9ZX5T3K8Q7W6V5R4P3N2M1H")).await;
    assert_eq!(unknown.status.as_u16(), 200, "{}", unknown.text());
    assert!(unknown.json()["items"].as_array().expect("items").is_empty());
    assert_problem(&send(&app, g("/api/v1/org/secretary/messages?project=nope")).await, 404, "project_not_found");
}

/// SPEC §7: 案件を作った直後に、秘書への対話用タスクが 1 件できて依頼文が渡っている。
#[tokio::test]
async fn creating_a_project_asks_the_secretary_first() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    let project = send(
        &app,
        p(
            "/api/v1/projects",
            &json!({"title": "Pluvio", "request": "Pluvio を基盤に用いた新たな研究テーマの模索、検証"}),
        ),
    )
    .await
    .json();
    let project_id = project["id"].as_str().expect("id").to_string();

    let items = send(&app, g(&format!("/api/v1/org/secretary/messages?project={project_id}")))
        .await
        .json()["items"]
        .as_array()
        .cloned()
        .expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["role"], "user");
    assert_eq!(items[0]["text"], "Pluvio を基盤に用いた新たな研究テーマの模索、検証");

    let tasks = send(&app, g(&format!("/api/v1/tasks?project={project_id}"))).await.json();
    assert_eq!(tasks["total"], 1);
    let task_id: task_core::TaskId = tasks["items"][0]["id"].as_str().expect("id").parse().expect("ulid");
    let task = env.store.get(task_id).expect("get").expect("task");
    assert_eq!(task.assignee.as_deref(), Some("secretary"));
    assert_eq!(task.project_id.map(|p| p.to_string()).as_deref(), Some(project_id.as_str()));
    assert_eq!(task.status, Status::Ready);
    assert_eq!(task.genre.as_deref(), Some("secretary"));
    assert!(task.title.starts_with("対話: "));
    let thread = env.store.message_list("secretary", task.project_id, 20).expect("list");
    assert_eq!(thread[0].role, MessageRole::User);
    assert_eq!(thread[0].task_id, Some(task.id), "R4: 1 往復と run を 1 段で辿れる");
    // R3: 案件の仕事の木からも対話用タスクが分かる。
    let detail = send(&app, g(&format!("/api/v1/projects/{project_id}"))).await.json();
    assert_eq!(detail["tasks"][0]["conversation"], true, "{detail}");
    assert_eq!(detail["tasks"][0]["assignee"], "secretary");
}

/// 秘書がいない構成（組織を種蒔きしていない）でも案件は作れる（対話が起きないだけ）。
#[tokio::test]
async fn a_project_can_be_created_without_an_organization() {
    let env = env_with_token();
    let app = env.router();
    let resp = send(&app, p("/api/v1/projects", &json!({"title": "t", "request": "r"}))).await;
    assert_eq!(resp.status.as_u16(), 201, "{}", resp.text());
    let project_id = resp.json()["id"].as_str().expect("id").to_string();
    let tasks = send(&app, g(&format!("/api/v1/tasks?project={project_id}"))).await.json();
    assert_eq!(tasks["total"], 0);
}

/// 知らないノードは 404、空の本文は 422、未知のクエリは 400。
#[tokio::test]
async fn unknown_nodes_and_bad_bodies_are_rejected() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;

    assert_problem(
        &send(&app, p("/api/v1/org/ghost/messages", &json!({"text": "hi"}))).await,
        404,
        "org_node_not_found",
    );
    assert_problem(&send(&app, g("/api/v1/org/ghost/messages")).await, 404, "org_node_not_found");
    assert_problem(
        &send(&app, p("/api/v1/org/secretary/messages", &json!({"text": "   "}))).await,
        422,
        "validation",
    );
    assert_problem(
        &send(&app, p("/api/v1/org/secretary/messages", &json!({"text": "x", "bogus": 1}))).await,
        400,
        "bad_request",
    );
    assert_problem(&send(&app, g("/api/v1/org/secretary/messages?bogus=1")).await, 400, "bad_request");
    assert_problem(
        &send(
            &app,
            p(
                "/api/v1/org/secretary/messages",
                &json!({"text": "x", "project_id": "01J9ZX5T3K8Q7W6V5R4P3N2M1H"}),
            ),
        )
        .await,
        422,
        "validation",
    );
}

/// 話しかけるのは管理系: トークンを設定していない構成でも 401（`POST /org` と同じ規律）。
#[tokio::test]
async fn posting_a_message_is_an_admin_endpoint() {
    let env = env_with_token();
    let app = env.router();
    seed_org(&app).await;
    // トークンを付けない要求は共通ガードで 401。
    assert_problem(
        &send(&app, post_json("/api/v1/org/secretary/messages", &json!({"text": "hi"}))).await,
        401,
        "unauthorized",
    );

    // `token_file` を設定していない構成でも管理系は 401（読み取りは通る）。
    let open = TestEnv::with(EnvOptions::default());
    let open_app = open.router();
    assert_problem(
        &send(&open_app, post_json("/api/v1/org/secretary/messages", &json!({"text": "hi"}))).await,
        401,
        "unauthorized",
    );
    assert_problem(&send(&open_app, get("/api/v1/org/secretary/messages")).await, 404, "org_node_not_found");
}
