//! ADR-0044 B1（Phase 53）: `PATCH /tasks/{id}`、タスク単位のコメント（人の割り込み）、`reopen`、
//! ボードのフィルタと `q=`、タイムライン。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{Event, Status, Task, TaskId, TaskKind, TaskStore};

fn admin() -> [(&'static str, &'static str); 1] {
    [("authorization", "Bearer s3cret-token-value")]
}

fn env_with_token() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.to_string()),
        ..EnvOptions::default()
    })
}

/// `running` のタスクは**必ずリースを持つ**（`acquire_lease` がそう作る）。割り込みの
/// `WorkerFinished` はそのリースの run にだけ付くので、テストでも同じ形にする。
fn seeded(env: &TestEnv, status: Status) -> Task {
    let mut task = new_task(TaskKind::Execute, status);
    if status == Status::Running {
        task.lease = Some(task_core::Lease {
            worker_run_id: "run-1".into(),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::seconds(60),
        });
    }
    env.seed(&task);
    task
}

// ---- D1: PATCH /tasks/{id} ----

/// 書いた項目だけが変わり、`Event::Edited{fields}` が残る。`priority` はラベルでも整数でも書ける。
#[tokio::test]
async fn patch_task_changes_only_the_written_fields_and_records_an_edited_event() {
    let env = env_with_token();
    let app = env.router();
    let task = seeded(&env, Status::Ready);
    let id = task.id;

    let resp = send(
        &app,
        patch_json_with(
            &format!("/api/v1/tasks/{id}"),
            &json!({
                "title": "新しい題名",
                "priority": "P0",
                "labels": ["infra", "urgent"],
                "category": "ops",
                "tier": "frontier",
                "max_turns": 42
            }),
            &admin(),
        ),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(
        body["fields"],
        json!(["title", "priority", "labels", "category", "tier", "budget"])
    );
    assert_eq!(body["task"]["title"], "新しい題名");
    assert_eq!(body["task"]["priority"], 30);
    assert_eq!(body["task"]["labels"], json!(["infra", "urgent"]));
    assert_eq!(body["task"]["category"], "ops");
    assert_eq!(body["task"]["worker_hint"]["tier"], "frontier");
    assert_eq!(body["task"]["budget"]["max_turns"], 42);
    assert_eq!(
        body["task"]["objective"], task.objective,
        "書かなかった項目は変わらない"
    );

    let events = env.store.events_for(id).expect("events");
    let edited = events
        .iter()
        .find_map(|(_, e)| match e {
            Event::Edited { fields, by } => Some((fields.clone(), by.clone())),
            _ => None,
        })
        .expect("Edited event");
    assert_eq!(edited.1, "human");
    assert_eq!(edited.0.len(), 6);

    // `GET /tasks/{id}` にも `priority_label` が出る。
    let detail = send(&app, get_with(&format!("/api/v1/tasks/{id}"), &admin()))
        .await
        .json();
    assert_eq!(detail["priority_label"], "P0");
}

/// 検証と 409: 終端は編集できない、壊れたラベルは 422、`expected_status` 不一致は 409、
/// 何も書かない本文は 422。`running` は受け付けて次の run から効く。
#[tokio::test]
async fn patch_task_validates_and_refuses_terminal_tasks() {
    let env = env_with_token();
    let app = env.router();

    for status in [Status::Done, Status::Failed, Status::Cancelled] {
        let task = seeded(&env, status);
        let resp = send(
            &app,
            patch_json_with(
                &format!("/api/v1/tasks/{}", task.id),
                &json!({"title": "x"}),
                &admin(),
            ),
        )
        .await;
        let problem = assert_problem(&resp, 409, "invalid_transition");
        assert_eq!(
            problem["task_status"],
            serde_json::to_value(status).expect("status")
        );
    }

    let ready = seeded(&env, Status::Ready);
    let path = format!("/api/v1/tasks/{}", ready.id);
    // 空の本文。
    assert_problem(
        &send(&app, patch_json_with(&path, &json!({}), &admin())).await,
        422,
        "validation",
    );
    // 壊れたラベル。
    assert_problem(
        &send(
            &app,
            patch_json_with(&path, &json!({"labels": ["Bad Label"]}), &admin()),
        )
        .await,
        422,
        "validation",
    );
    // 知らない項目。
    assert_problem(
        &send(&app, patch_json_with(&path, &json!({"bogus": 1}), &admin())).await,
        400,
        "bad_request",
    );
    // `expected_status` 不一致。
    assert_problem(
        &send(
            &app,
            patch_json_with(
                &path,
                &json!({"title": "x", "expected_status": "running"}),
                &admin(),
            ),
        )
        .await,
        409,
        "conflict",
    );
    // トークン無しは 401（管理系）。
    assert_problem(
        &send(&app, patch_json_with(&path, &json!({"title": "x"}), &[])).await,
        401,
        "unauthorized",
    );
    assert_eq!(
        env.store.get(ready.id).expect("get").expect("task").title,
        ready.title
    );

    // `running` は受け付けるが状態は変わらない（次の run から効く）。
    let running = seeded(&env, Status::Running);
    let resp = send(
        &app,
        patch_json_with(
            &format!("/api/v1/tasks/{}", running.id),
            &json!({"objective": "直した目的"}),
            &admin(),
        ),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(env.status_of(running.id), Status::Running);
}

// ---- D2: コメントと割り込み・再開 ----

/// 人のコメントの効き方の表（ADR-0044 D2）を API の層で全状態ぶん確かめる。
#[tokio::test]
async fn posting_a_human_comment_follows_the_effect_table() {
    let env = env_with_token();
    let app = env.router();

    for (status, effect, next, can_reopen) in [
        (Status::Draft, "stored", Status::Draft, false),
        (Status::Ready, "stored", Status::Ready, false),
        (Status::Running, "interrupted", Status::Ready, false),
        (Status::Reviewing, "interrupted", Status::Ready, false),
        (Status::Done, "terminal", Status::Done, true),
        (Status::Failed, "terminal", Status::Failed, true),
        (Status::Cancelled, "terminal", Status::Cancelled, false),
    ] {
        let task = seeded(&env, status);
        let resp = send(
            &app,
            post_json_with(
                &format!("/api/v1/tasks/{}/comments", task.id),
                &json!({"body": "ちょっと待った"}),
                &admin(),
            ),
        )
        .await;
        assert_eq!(resp.status, 201, "{status:?}: {}", resp.text());
        let body = resp.json();
        assert_eq!(body["effect"], effect, "{status:?}");
        assert_eq!(body["can_reopen"], can_reopen, "{status:?}");
        assert_eq!(env.status_of(task.id), next, "{status:?}");
        if effect == "interrupted" {
            assert_eq!(body["transition"]["reason"], "comment");
            // 走っている run（= リースのある `running`）にだけ `WorkerFinished` を足す。
            // `reviewing` は直近のワーカー run が既に `done` で終わっているので足さない
            // （足すとその run の記録を壊す。Phase 53 の監査）。
            let finished = env
                .store
                .events_for(task.id)
                .expect("events")
                .iter()
                .any(|(_, e)| matches!(e, Event::WorkerFinished { outcome, .. } if outcome == "interrupted: comment"));
            assert_eq!(
                finished,
                status == Status::Running,
                "{status:?}: WorkerFinished の有無"
            );
        }
        // どの状態でも `GET` で読める。
        let list = send(
            &app,
            get_with(&format!("/api/v1/tasks/{}/comments", task.id), &admin()),
        )
        .await
        .json();
        assert_eq!(
            list["items"].as_array().map(Vec::len),
            Some(1),
            "{status:?}"
        );
        assert_eq!(list["items"][0]["author_kind"], "human");
    }

    // `blocked` は回答と同じ（`Event::Answered` が残り `ready` に戻る）。
    let blocked = new_task(TaskKind::Execute, Status::Blocked);
    env.seed_with(
        &blocked,
        vec![Event::WorkerFinished {
            run_id: "run-1".into(),
            outcome: "question: which db?".into(),
            usage: None,
            role: None,
            metrics: None,
        }],
    );
    let resp = send(
        &app,
        post_json_with(
            &format!("/api/v1/tasks/{}/comments", blocked.id),
            &json!({"body": "postgres で"}),
            &admin(),
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    assert_eq!(resp.json()["effect"], "answered");
    assert_eq!(env.status_of(blocked.id), Status::Ready);
    assert!(
        env.store
            .events_for(blocked.id)
            .expect("events")
            .iter()
            .any(|(_, e)| matches!(e, Event::Answered { answer, question } if answer == "postgres で" && question == "which db?"))
    );
}

/// 空の本文は 422、トークン無しは 401、知らないタスクは 404。
#[tokio::test]
async fn comment_validation_and_auth() {
    let env = env_with_token();
    let app = env.router();
    let task = seeded(&env, Status::Ready);
    let path = format!("/api/v1/tasks/{}/comments", task.id);
    assert_problem(
        &send(
            &app,
            post_json_with(&path, &json!({"body": "  "}), &admin()),
        )
        .await,
        422,
        "validation",
    );
    assert_problem(
        &send(&app, post_json_with(&path, &json!({"body": "x"}), &[])).await,
        401,
        "unauthorized",
    );
    // 以下は認証済みでの 404（トークンを設定した構成では読み取りにも認証が要る）。
    assert_problem(
        &send(
            &app,
            post_json_with(
                &format!("/api/v1/tasks/{}/comments", TaskId::new()),
                &json!({"body": "x"}),
                &admin(),
            ),
        )
        .await,
        404,
        "task_not_found",
    );
    assert_problem(
        &send(
            &app,
            get_with(
                &format!("/api/v1/tasks/{}/comments", TaskId::new()),
                &admin(),
            ),
        )
        .await,
        404,
        "task_not_found",
    );
}

/// `reopen`: `done` / `failed` は `ready`（attempts 0）、`cancelled` と非終端は 409。
#[tokio::test]
async fn reopen_restarts_done_and_failed_tasks_only() {
    let env = env_with_token();
    let app = env.router();

    for status in [Status::Done, Status::Failed] {
        let mut task = new_task(TaskKind::Execute, status);
        task.attempts = 3;
        env.seed(&task);
        let resp = send(
            &app,
            post_json_with(
                &format!("/api/v1/tasks/{}/reopen", task.id),
                &json!({}),
                &admin(),
            ),
        )
        .await;
        assert_eq!(resp.status, 200, "{status:?}: {}", resp.text());
        let body = resp.json();
        assert_eq!(body["to"], "ready");
        assert_eq!(body["reason"], "reopen");
        let after = env.store.get(task.id).expect("get").expect("task");
        assert_eq!(after.status, Status::Ready);
        assert_eq!(after.attempts, 0, "再開は attempts を 0 に戻す");
        // 再開した後は `actions` に `reopen` が無い。
        let detail = send(
            &app,
            get_with(&format!("/api/v1/tasks/{}", task.id), &admin()),
        )
        .await
        .json();
        let actions = detail["actions"].as_array().cloned().unwrap_or_default();
        assert!(!actions.contains(&json!("reopen")), "{detail}");
        assert!(actions.contains(&json!("edit")), "{detail}");
    }

    for status in [Status::Cancelled, Status::Ready] {
        let task = seeded(&env, status);
        assert_problem(
            &send(
                &app,
                post_json_with(
                    &format!("/api/v1/tasks/{}/reopen", task.id),
                    &json!({}),
                    &admin(),
                ),
            )
            .await,
            409,
            "invalid_transition",
        );
    }
    // 管理系。
    let done = seeded(&env, Status::Done);
    assert_problem(
        &send(
            &app,
            post_json_with(
                &format!("/api/v1/tasks/{}/reopen", done.id),
                &json!({}),
                &[],
            ),
        )
        .await,
        401,
        "unauthorized",
    );
}

// ---- D4: ボードのフィルタと検索 ----

/// `label`（AND）・`category`・`assignee`・`milestone`・`tier`・`priority`・`q`（コメント本文も）。
#[tokio::test]
async fn list_tasks_filters_by_label_category_tier_priority_and_comment_text() {
    let env = TestEnv::new();
    let app = env.router();

    let mut infra = new_task(TaskKind::Execute, Status::Ready);
    infra.title = "infra work".into();
    infra.labels = vec!["infra".into(), "urgent".into()];
    infra.category = task_core::TaskCategory::Ops;
    infra.priority = 30;
    infra.worker_hint.tier = task_core::Tier::Frontier;
    env.seed(&infra);

    let mut docs = new_task(TaskKind::Execute, Status::Ready);
    docs.title = "write docs".into();
    docs.labels = vec!["infra".into()];
    docs.category = task_core::TaskCategory::Docs;
    docs.priority = 0;
    docs.worker_hint.tier = task_core::Tier::Cheap;
    env.seed(&docs);

    let titles = |body: &Value| {
        body["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|t| t["title"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>()
    };

    let all = send(&app, get("/api/v1/tasks?label=infra")).await.json();
    assert_eq!(all["total"], 2, "{all}");
    let both = send(&app, get("/api/v1/tasks?label=infra&label=urgent"))
        .await
        .json();
    assert_eq!(
        titles(&both),
        vec!["infra work".to_string()],
        "ラベルは AND"
    );
    let ops = send(&app, get("/api/v1/tasks?category=ops")).await.json();
    assert_eq!(titles(&ops), vec!["infra work".to_string()]);
    let cheap = send(&app, get("/api/v1/tasks?tier=cheap")).await.json();
    assert_eq!(titles(&cheap), vec!["write docs".to_string()]);
    let p0 = send(&app, get("/api/v1/tasks?priority=P0")).await.json();
    assert_eq!(titles(&p0), vec!["infra work".to_string()]);
    assert_eq!(p0["items"][0]["priority_label"], "P0");
    assert_eq!(p0["items"][0]["category"], "ops");
    assert_eq!(p0["items"][0]["labels"], json!(["infra", "urgent"]));
    // 複数のフィルタは AND。
    let none = send(&app, get("/api/v1/tasks?label=urgent&tier=cheap"))
        .await
        .json();
    assert_eq!(none["total"], 0, "{none}");

    // `q` はコメント本文も見る。
    env.store
        .comment_add(
            &task_core::TaskComment::new(
                docs.id,
                task_core::CommentAuthorKind::Human,
                None,
                "zebra のことを調べて".into(),
                None,
                time::OffsetDateTime::now_utc(),
            ),
            None,
        )
        .expect("comment");
    let q = send(&app, get("/api/v1/tasks?q=zebra")).await.json();
    assert_eq!(titles(&q), vec!["write docs".to_string()], "{q}");

    // 知らない値は 400。
    assert_problem(
        &send(&app, get("/api/v1/tasks?category=bogus")).await,
        400,
        "bad_request",
    );
    assert_problem(
        &send(&app, get("/api/v1/tasks?tier=bogus")).await,
        400,
        "bad_request",
    );
    assert_problem(
        &send(&app, get("/api/v1/tasks?priority=bogus")).await,
        400,
        "bad_request",
    );
    assert_problem(
        &send(&app, get("/api/v1/tasks?label=Bad")).await,
        400,
        "bad_request",
    );
    assert_problem(
        &send(&app, get("/api/v1/tasks?milestone=nope")).await,
        400,
        "bad_request",
    );
}

// ---- D5: タイムライン ----

/// イベント・コメント・委譲が 1 本になり、時刻の昇順に並ぶ。
#[tokio::test]
async fn the_timeline_merges_events_comments_and_delegations_in_time_order() {
    let env = env_with_token();
    let app = env.router();
    let parent = seeded(&env, Status::Running);

    // 子（委譲）。
    let child = {
        let mut c = new_task(TaskKind::Execute, Status::Ready);
        c.parent_id = Some(parent.id);
        c.title = "子の仕事".into();
        c
    };
    env.store.insert(&child).expect("insert child");
    env.store
        .append_event(
            parent.id,
            &Event::Delegated {
                run_id: "run-1".into(),
                task_ids: vec![child.id],
            },
        )
        .expect("delegated");

    // 人のコメント（走っている run を止める）。
    let resp = send(
        &app,
        post_json_with(
            &format!("/api/v1/tasks/{}/comments", parent.id),
            &json!({"body": "方針を変えたい"}),
            &admin(),
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());

    let timeline = send(
        &app,
        get_with(&format!("/api/v1/tasks/{}/timeline", parent.id), &admin()),
    )
    .await;
    assert_eq!(timeline.status, 200, "{}", timeline.text());
    let body = timeline.json();
    assert_eq!(body["task_id"], parent.id.to_string());
    let items = body["items"].as_array().cloned().expect("items");
    let kinds: Vec<&str> = items
        .iter()
        .map(|i| i["kind"].as_str().unwrap_or_default())
        .collect();
    assert!(kinds.contains(&"comment"), "{kinds:?}");
    assert!(kinds.contains(&"delegation"), "{kinds:?}");
    assert!(kinds.contains(&"event"), "{kinds:?}");
    // 委譲は 1 件にまとまり、子のタスクが載る（`event` としては出ない）。
    let delegation = items
        .iter()
        .find(|i| i["kind"] == "delegation")
        .expect("delegation");
    assert_eq!(delegation["run_id"], "run-1");
    assert_eq!(delegation["tasks"][0]["title"], "子の仕事");
    assert!(
        !items
            .iter()
            .any(|i| i["kind"] == "event" && i["event"]["type"] == "delegated"),
        "委譲は 2 回出さない"
    );
    // 時刻の昇順。
    let times: Vec<&str> = items
        .iter()
        .map(|i| i["at"].as_str().unwrap_or_default())
        .collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    assert_eq!(times, sorted, "{times:?}");
    // 割り込みと `WorkerFinished` がイベントとして載る。
    assert!(items.iter().any(|i| i["kind"] == "event"
        && i["event"]["type"] == "transitioned"
        && i["event"]["reason"] == "comment"));
    assert!(items.iter().any(|i| i["kind"] == "event"
        && i["event"]["type"] == "worker_finished"
        && i["event"]["outcome"] == "interrupted: comment"));

    // 知らないタスクは 404。
    assert_problem(
        &send(
            &app,
            get_with(
                &format!("/api/v1/tasks/{}/timeline", TaskId::new()),
                &admin(),
            ),
        )
        .await,
        404,
        "task_not_found",
    );
}

/// ADR-0043 D5（Phase 54）× ADR-0044 D5: 人が押した取り込み（merge / PR / discard）が
/// `task_integrations` からタイムラインに出る。`at` は押した時刻（`created_at`）なので、
/// PR の同期（`updated_at`）で並びが動かない。
#[tokio::test]
async fn the_timeline_lists_the_integrations_of_this_task() {
    use task_core::{IntegrationMethod, IntegrationState, TaskIntegration};

    let env = env_with_token();
    let app = env.router();
    let task = seeded(&env, Status::Done);
    let now = time::OffsetDateTime::now_utc();

    // 1 件目: 取り込み（merge）。押した時刻がいちばん古い。
    let merged = TaskIntegration::new(
        task.id,
        None,
        "api",
        IntegrationMethod::Merge,
        IntegrationState::Done,
        now - time::Duration::seconds(120),
    );
    env.store.integration_put(&merged).expect("put merge");

    // 2 件目: PR（後から `gh pr view` で `merged` に同期された = `updated_at` だけ新しい）。
    let mut pr = TaskIntegration::new(
        task.id,
        None,
        "gui",
        IntegrationMethod::Pr,
        IntegrationState::Merged,
        now - time::Duration::seconds(60),
    );
    pr.pr_number = Some(42);
    pr.pr_url = Some("https://example.invalid/pr/42".into());
    pr.updated_at = now;
    env.store.integration_put(&pr).expect("put pr");

    let resp = send(
        &app,
        get_with(&format!("/api/v1/tasks/{}/timeline", task.id), &admin()),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    let items = body["items"].as_array().cloned().expect("items");
    let integrations: Vec<&Value> = items
        .iter()
        .filter(|i| i["kind"] == "integration")
        .collect();
    assert_eq!(integrations.len(), 2, "{items:?}");
    // 押した順（`created_at` の昇順）に並ぶ。
    assert_eq!(integrations[0]["action"], "merge");
    assert_eq!(integrations[0]["detail"], "api: done");
    assert_eq!(integrations[1]["action"], "pr");
    assert_eq!(
        integrations[1]["detail"],
        "gui: merged \u{2014} PR #42 https://example.invalid/pr/42"
    );
    let pressed_at = (now - time::Duration::seconds(60))
        .format(&time::format_description::well_known::Rfc3339)
        .expect("rfc3339");
    assert_eq!(
        integrations[1]["at"], pressed_at,
        "`at` は押した時刻（同期した時刻ではない）"
    );
    // タイムライン全体は時刻の昇順のまま。
    let times: Vec<&str> = items
        .iter()
        .map(|i| i["at"].as_str().unwrap_or_default())
        .collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    assert_eq!(times, sorted, "{times:?}");
}

/// ADR-0047 D4/D5（Phase 62）: このタスクの終端から起きた知識整理 run が `knowledge` 項目になる。
/// `scheduled`（未適用）では件数を出さず、適用されれば `ingested`/`inbox`/`discarded` を出す。
#[tokio::test]
async fn the_timeline_shows_the_knowledge_maintenance_run() {
    use task_core::{KnowledgeRunState, KnowledgeRunStore, KnowledgeRunSummary};

    let env = env_with_token();
    let app = env.router();
    let task = seeded(&env, Status::Done);
    let run_task = seeded(&env, Status::Done);
    let now = time::OffsetDateTime::now_utc();

    env.store
        .knowledge_run_create(task.id, run_task.id, now)
        .expect("create run");
    let scheduled = send(
        &app,
        get_with(&format!("/api/v1/tasks/{}/timeline", task.id), &admin()),
    )
    .await;
    let items = scheduled.json()["items"]
        .as_array()
        .cloned()
        .expect("items");
    let knowledge: Vec<&Value> = items.iter().filter(|i| i["kind"] == "knowledge").collect();
    assert_eq!(knowledge.len(), 1, "{items:?}");
    assert_eq!(knowledge[0]["state"], "scheduled");
    assert_eq!(knowledge[0]["run_task_id"], run_task.id.to_string());
    assert!(knowledge[0].get("ingested").is_none(), "{knowledge:?}");

    env.store
        .knowledge_run_finish(
            task.id,
            KnowledgeRunState::Done,
            now,
            Some(&KnowledgeRunSummary {
                candidates: 2,
                ingested: 1,
                inbox: 1,
                discarded: 0,
                discarded_reasons: Vec::new(),
                // ADR-0052 D2（Phase 64）: どの経路で抽出したか。
                via: Some("fallback:codex".to_string()),
            }),
            Some("fallback:codex"),
        )
        .expect("finish run");
    let applied = send(
        &app,
        get_with(&format!("/api/v1/tasks/{}/timeline", task.id), &admin()),
    )
    .await;
    let items = applied.json()["items"].as_array().cloned().expect("items");
    let knowledge: Vec<&Value> = items.iter().filter(|i| i["kind"] == "knowledge").collect();
    assert_eq!(knowledge.len(), 1, "{items:?}");
    assert_eq!(knowledge[0]["state"], "applied");
    assert_eq!(knowledge[0]["ingested"], 1);
    assert_eq!(knowledge[0]["inbox"], 1);
    assert_eq!(knowledge[0]["discarded"], 0);
    // ADR-0052 D2: cheap の汎用ハーネスで抽出したことが GUI に伝わる。
    assert_eq!(knowledge[0]["via"], "fallback:codex");

    // 知識整理 run を持たないタスクには出ない。
    let plain = seeded(&env, Status::Done);
    let resp = send(
        &app,
        get_with(&format!("/api/v1/tasks/{}/timeline", plain.id), &admin()),
    )
    .await;
    let items = resp.json()["items"].as_array().cloned().expect("items");
    assert!(!items.iter().any(|i| i["kind"] == "knowledge"), "{items:?}");
}

/// ADR-0044 D5: `worktree.json` のブランチのコミットが `changes.json` に入っているリリースが
/// タイムラインに出る。目印が無ければ何も出さない（タイムラインは落ちない）。
#[tokio::test]
async fn the_timeline_lists_the_releases_that_contain_this_tasks_commits() {
    use std::path::Path;
    use std::sync::Arc;

    /// `branch_commits` だけを答える偽のリリース置き場（git も `[selfdeploy]` も要らない）。
    struct FakeReleases {
        commits: Vec<String>,
    }

    impl task_api::ReleaseSource for FakeReleases {
        fn list(&self) -> task_api::ReleasesFs {
            task_api::ReleasesFs {
                current: Some("aaaaaaaaaaaa".into()),
                previous: None,
                items: vec![
                    release_item(
                        "bbbbbbbbbbbb",
                        "2026-09-19T02:00:00Z",
                        vec!["sha-mine", "sha-other"],
                    ),
                    release_item("cccccccccccc", "2026-09-19T01:00:00Z", vec!["sha-nobody"]),
                ],
            }
        }
        fn promote(
            &self,
            _sha12: &str,
        ) -> Result<task_api::types::ReleasePromoteAccepted, task_api::ReleasePromoteError>
        {
            Err(task_api::ReleasePromoteError::NotFound)
        }
        fn branch_commits(&self, _repo: &Path, branch: &str, _base: Option<&str>) -> Vec<String> {
            if branch.ends_with("-missing") {
                Vec::new()
            } else {
                self.commits.clone()
            }
        }
    }

    fn release_item(
        sha12: &str,
        built_at: &str,
        commits: Vec<&str>,
    ) -> task_api::types::ReleaseItem {
        task_api::types::ReleaseItem {
            sha12: sha12.into(),
            r#ref: None,
            built_at: Some(built_at.into()),
            schema_version: None,
            gate_ok: true,
            gate: None,
            verify: None,
            promoted_at: None,
            on_main: None,
            changes: Some(task_api::types::ReleaseChanges {
                base: None,
                stale: false,
                commit_count: commits.len(),
                file_count: 0,
                sensitive: vec![],
                commits: commits
                    .into_iter()
                    .map(|sha| task_api::types::ReleaseCommit {
                        sha: sha.into(),
                        subject: "s".into(),
                    })
                    .collect(),
            }),
            is_current: false,
            is_previous: false,
            promoting: false,
            promote_failed: None,
            problem: None,
        }
    }

    let source: task_api::SharedReleaseSource = Arc::new(FakeReleases {
        commits: vec!["sha-mine".into()],
    });
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.to_string()),
        releases: Some(source),
        ..EnvOptions::default()
    });
    let app = env.router();
    let task = seeded(&env, Status::Done);

    // 目印が無いうちはリリースを出さない。
    let before = send(
        &app,
        get_with(&format!("/api/v1/tasks/{}/timeline", task.id), &admin()),
    )
    .await
    .json();
    assert!(
        !before["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .any(|i| i["kind"] == "release"),
        "{before}"
    );

    // ADR-0041 D1 の目印を置く（`local_dir` が見るのは `<workspace_root>/<task_id>/worktree.json`）。
    let task_dir = env.workspace_root.join(task.id.to_string());
    std::fs::create_dir_all(&task_dir).expect("task dir");
    task_ops::workspace::write_marker(
        &task_dir,
        &task_ops::workspace::WorktreeMarker {
            repo: env.workspace_root.to_string_lossy().into_owned(),
            dir: task_dir.join("tree").to_string_lossy().into_owned(),
            branch: format!("celeris/{}", task.id),
            base: "0".repeat(40),
            base_kind: "main".into(),
            repos: Vec::new(),
        },
    )
    .expect("marker");

    let body = send(
        &app,
        get_with(&format!("/api/v1/tasks/{}/timeline", task.id), &admin()),
    )
    .await
    .json();
    let releases: Vec<&Value> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|i| i["kind"] == "release")
        .collect();
    assert_eq!(releases.len(), 1, "{body}");
    assert_eq!(releases[0]["sha12"], "bbbbbbbbbbbb");
    assert_eq!(
        releases[0]["commits"],
        json!(["sha-mine"]),
        "自分のコミットだけ"
    );
}
