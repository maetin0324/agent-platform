//! api.md §8.3（一覧）、§8.4（詳細）と、受信箱・DAG・run 一覧の「task-ops の結果がそのまま JSON で返る」確認。
//! 中身は task-ops のビュー（`task_list` / `task_detail` / `inbox` / `graph` / `runs`）で、API はそれを JSON にするだけ。

mod common;

use std::collections::HashSet;

use common::*;
use serde_json::{Value, json};
use task_core::{Event, Status, TaskId, TaskKind, TaskStore};
use time::{Duration as TimeDuration, OffsetDateTime};

const REQUIRES_VIEWS: &str = "requires task-ops views (Phase 9b I1)";

fn ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["id"].as_str().expect("id").to_string())
        .collect()
}

#[tokio::test]
async fn list_pages_through_250_tasks_in_every_order() {
    let _ = REQUIRES_VIEWS;
    let env = TestEnv::new();
    let app = env.router();
    let base = OffsetDateTime::now_utc() - TimeDuration::days(1);
    let mut tasks = Vec::new();
    for i in 0..250i64 {
        let mut task = new_task(
            TaskKind::Execute,
            if i % 3 == 0 {
                Status::Ready
            } else {
                Status::Draft
            },
        );
        task.title = format!("task {i:03}");
        task.priority = i32::try_from(i % 5).expect("priority");
        task.created_at = base + TimeDuration::seconds(i % 17);
        task.updated_at = base + TimeDuration::seconds((i * 7) % 23);
        env.seed(&task);
        tasks.push(task);
    }

    for order in ["dispatch", "updated_desc", "created_desc"] {
        let mut expected = tasks.clone();
        match order {
            "dispatch" => expected.sort_by(|a, b| {
                b.priority
                    .cmp(&a.priority)
                    .then(a.created_at.cmp(&b.created_at))
                    .then(a.id.to_string().cmp(&b.id.to_string()))
            }),
            "updated_desc" => expected.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then(b.id.to_string().cmp(&a.id.to_string()))
            }),
            _ => expected.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then(b.id.to_string().cmp(&a.id.to_string()))
            }),
        }
        let expected: Vec<String> = expected.iter().map(|t| t.id.to_string()).collect();

        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let path = match &cursor {
                Some(c) => format!("/api/v1/tasks?order={order}&limit=100&cursor={c}"),
                None => format!("/api/v1/tasks?order={order}&limit=100"),
            };
            let resp = send(&app, get(&path)).await;
            assert_eq!(resp.status, 200, "{}", resp.text());
            let page = resp.json();
            assert_eq!(page["total"], 250);
            assert_eq!(page["counts_by_status"]["ready"], 84);
            assert_eq!(page["counts_by_status"]["draft"], 166);
            seen.extend(ids(&page));
            pages += 1;
            match page["next_cursor"].as_str() {
                Some(c) => cursor = Some(c.to_string()),
                None => break,
            }
        }
        assert_eq!(pages, 3, "order {order}");
        assert_eq!(
            seen.iter().collect::<HashSet<_>>().len(),
            250,
            "duplicates in {order}"
        );
        assert_eq!(seen, expected, "order {order}");
    }
}

#[tokio::test]
async fn list_title_query_treats_percent_and_underscore_literally() {
    let env = TestEnv::new();
    let app = env.router();
    for title in ["100% done", "100 done", "a_b", "axb"] {
        let mut task = new_task(TaskKind::Execute, Status::Draft);
        task.title = title.into();
        env.seed(&task);
    }
    let titles = |page: &Value| -> Vec<String> {
        page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|i| i["title"].as_str().expect("title").to_string())
            .collect()
    };
    let percent = send(&app, get("/api/v1/tasks?q=%25")).await.json();
    assert_eq!(titles(&percent), vec!["100% done"]);
    let underscore = send(&app, get("/api/v1/tasks?q=_")).await.json();
    assert_eq!(titles(&underscore), vec!["a_b"]);
    let filtered = send(&app, get("/api/v1/tasks?q=done&status=draft,ready"))
        .await
        .json();
    assert_eq!(filtered["total"], 2);
}

#[tokio::test]
async fn tampered_cursor_is_a_bad_request() {
    let env = TestEnv::new();
    let app = env.router();
    for _ in 0..3 {
        env.seed(&new_task(TaskKind::Execute, Status::Draft));
    }
    let page = send(&app, get("/api/v1/tasks?limit=1")).await.json();
    let cursor = page["next_cursor"].as_str().expect("cursor").to_string();
    for tampered in [
        format!("{cursor}0"),
        cursor.replacen(&cursor[..2], "zz", 1),
        "7b7d".to_string(),
    ] {
        let resp = send(
            &app,
            get(&format!("/api/v1/tasks?limit=1&cursor={tampered}")),
        )
        .await;
        assert_problem(&resp, 400, "bad_request");
    }
}

#[tokio::test]
async fn list_rejects_bad_query_values_before_calling_task_ops() {
    let env = TestEnv::new();
    let app = env.router();
    let long_q = "x".repeat(201);
    for path in [
        "/api/v1/tasks?status=bogus".to_string(),
        "/api/v1/tasks?status=ready,bogus".to_string(),
        "/api/v1/tasks?kind=chore".to_string(),
        "/api/v1/tasks?parent=xyz".to_string(),
        "/api/v1/tasks?root_only=maybe".to_string(),
        "/api/v1/tasks?order=sideways".to_string(),
        "/api/v1/tasks?limit=0".to_string(),
        "/api/v1/tasks?limit=ten".to_string(),
        format!("/api/v1/tasks?q={long_q}"),
        "/api/v1/tasks?cursor=a&cursor=b".to_string(),
        "/api/v1/tasks?statuses=ready".to_string(),
    ] {
        assert_problem(&send(&app, get(&path)).await, 400, "bad_request");
    }
}

#[tokio::test]
async fn detail_matches_task_ops_task_detail_byte_for_byte() {
    let env = TestEnv::new();
    let app = env.router();
    let parent = new_task(TaskKind::Execute, Status::Running);
    let run_id = ulid::Ulid::new().to_string();
    env.seed_with(
        &parent,
        vec![
            Event::WorkerStarted {
                run_id: run_id.clone(),
                adapter: "fake".into(),
                model: "m".into(),
                provider: Some("claude-a".into()),
                account: None,
                role: None,
                task_role: None,
            },
            Event::worker_progress(run_id.clone(), "working"),
        ],
    );
    let run_dir = env.workspace(&parent).join("runs").join(&run_id);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(run_dir.join("stdout.jsonl"), "{}\n").expect("stdout");

    let resp = send(&app, get(&format!("/api/v1/tasks/{}", parent.id))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let api: Value = resp.json();

    let mut expected = task_ops::view::task_detail(
        &env.store,
        parent.id,
        &env.view_context(),
        OffsetDateTime::now_utc(),
    )
    .expect("task_detail");
    expected.timers.now = api["timers"]["now"]
        .as_str()
        .expect("timers.now")
        .to_string();
    assert_eq!(expected.runs.len(), 1);
    for run in &mut expected.runs {
        assert!(run.files.is_none(), "task-ops leaves files to task-api");
        run.files = Some(task_ops::view::RunFiles {
            stdout: true,
            stderr: false,
            result: false,
            request: false,
            prompt: false,
        });
    }
    let expected_bytes = serde_json::to_vec(&expected).expect("serialize");
    assert_eq!(
        String::from_utf8_lossy(&resp.body),
        String::from_utf8_lossy(&expected_bytes)
    );
    assert_eq!(resp.body, expected_bytes);
}

#[tokio::test]
async fn detail_of_a_missing_task_is_404() {
    let env = TestEnv::new();
    let resp = send(
        &env.router(),
        get(&format!("/api/v1/tasks/{}", TaskId::new())),
    )
    .await;
    assert_problem(&resp, 404, "task_not_found");
}

#[tokio::test]
async fn runs_list_fills_files_from_the_run_directory() {
    let env = TestEnv::new();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Running);
    let first = ulid::Ulid::new().to_string();
    let second = ulid::Ulid::new().to_string();
    let started = |run_id: &str| Event::WorkerStarted {
        run_id: run_id.to_string(),
        adapter: "fake".into(),
        model: "m".into(),
        provider: None,
        account: None,
        role: None,
        task_role: None,
    };
    env.seed_with(&task, vec![started(&first), started(&second)]);
    let run_dir = env.workspace(&task).join("runs").join(&first);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(run_dir.join("stdout.jsonl"), "{}\n").expect("stdout");
    std::fs::write(run_dir.join("result.json"), "{}\n").expect("result");
    // ADR-0023 D2: ワーカーに渡した指示（`run_subprocess` が書く）。
    std::fs::write(run_dir.join("request.json"), "{}\n").expect("request");

    let body = send(&app, get(&format!("/api/v1/tasks/{}/runs", task.id)))
        .await
        .json();
    let runs = body["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 2);
    assert_eq!(
        runs[0]["files"],
        json!({"stdout": true, "stderr": false, "result": true, "request": true, "prompt": false})
    );
    assert_eq!(
        runs[1]["files"],
        json!({"stdout": false, "stderr": false, "result": false, "request": false, "prompt": false})
    );

    // ADR-0023 D2: `GET …/runs/{run_id}/request` は application/json で中身を返す。
    let resp = send(
        &app,
        get(&format!("/api/v1/tasks/{}/runs/{first}/request", task.id)),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.header("content-type"), Some("application/json"));
    // run のディレクトリごと無い run は 404（既存の stdout/stderr/result と同じ扱い）。
    let resp = send(
        &app,
        get(&format!("/api/v1/tasks/{}/runs/{second}/request", task.id)),
    )
    .await;
    assert_problem(&resp, 404, "run_not_found");
}

#[tokio::test]
async fn runs_of_a_missing_task_is_404() {
    let env = TestEnv::new();
    let resp = send(
        &env.router(),
        get(&format!("/api/v1/tasks/{}/runs", TaskId::new())),
    )
    .await;
    assert_problem(&resp, 404, "task_not_found");
}

#[tokio::test]
async fn inbox_returns_the_task_ops_inbox_as_json() {
    let env = TestEnv::new();
    let app = env.router();
    env.seed(&new_task(TaskKind::Approval, Status::Ready));
    env.seed(&new_task(TaskKind::Execute, Status::Draft));
    env.seed_with(
        &new_task(TaskKind::Execute, Status::Blocked),
        vec![Event::WorkerFinished {
            run_id: ulid::Ulid::new().to_string(),
            outcome: "question: which db?".into(),
            usage: None,
            role: None,
        }],
    );

    let resp = send(&app, get("/api/v1/inbox")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let expected = task_ops::inbox::inbox(
        &env.store,
        None,
        &env.view_context(),
        OffsetDateTime::now_utc(),
        &|_, _| vec![],
    )
    .expect("inbox");
    assert_eq!(
        resp.json(),
        serde_json::to_value(&expected).expect("serialize")
    );
}

#[tokio::test]
async fn graph_returns_the_task_ops_graph_as_json() {
    let env = TestEnv::new();
    let app = env.router();
    let first = new_task(TaskKind::Execute, Status::Done);
    let mut second = new_task(TaskKind::Execute, Status::Ready);
    second.depends_on = vec![first.id];
    env.seed(&first);
    env.seed(&second);

    let all = send(&app, get("/api/v1/graph")).await;
    assert_eq!(all.status, 200, "{}", all.text());
    let expected = task_ops::graph::graph(&env.store, None, None, true).expect("graph");
    assert_eq!(
        all.json(),
        serde_json::to_value(&expected).expect("serialize")
    );

    let rooted = send(
        &app,
        get(&format!(
            "/api/v1/graph?root={}&depth=1&include_terminal=false",
            second.id
        )),
    )
    .await;
    let expected =
        task_ops::graph::graph(&env.store, Some(second.id), Some(1), false).expect("graph");
    assert_eq!(
        rooted.json(),
        serde_json::to_value(&expected).expect("serialize")
    );

    let missing = send(&app, get(&format!("/api/v1/graph?root={}", TaskId::new()))).await;
    assert_problem(&missing, 404, "task_not_found");
}

#[tokio::test]
async fn task_events_page_by_seq_with_type_filter() {
    let env = TestEnv::new();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed_with(&task, (0..6).map(|i| progress(&format!("p{i}"))).collect());
    env.store
        .apply_transition(task.id, task_core::Trigger::Accept, None)
        .expect("accept");
    env.store
        .append_event(task.id, &progress("late"))
        .expect("append");
    let other = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&other);

    let first = send(
        &app,
        get(&format!("/api/v1/tasks/{}/events?limit=3", task.id)),
    )
    .await
    .json();
    let seqs: Vec<u64> = first["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["seq"].as_u64().expect("seq"))
        .collect();
    assert_eq!(seqs, vec![0, 1, 2]);
    assert_eq!(first["has_more"], true);
    assert!(first["items"][0]["id"].as_u64().is_some());
    assert!(first["items"][0]["ts"].as_str().is_some());
    assert_eq!(first["items"][0]["event"]["type"], "created");

    let rest = send(
        &app,
        get(&format!(
            "/api/v1/tasks/{}/events?after_seq=2&limit=100",
            task.id
        )),
    )
    .await
    .json();
    let seqs: Vec<u64> = rest["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["seq"].as_u64().expect("seq"))
        .collect();
    assert_eq!(seqs, vec![3, 4, 5, 6, 7, 8]);
    assert_eq!(rest["has_more"], false);

    let transitions = send(
        &app,
        get(&format!(
            "/api/v1/tasks/{}/events?types=transitioned&limit=1",
            task.id
        )),
    )
    .await
    .json();
    assert_eq!(transitions["items"].as_array().expect("items").len(), 1);
    assert_eq!(transitions["items"][0]["event"]["type"], "transitioned");
    assert_eq!(transitions["has_more"], false);

    let progress_page = send(
        &app,
        get(&format!(
            "/api/v1/tasks/{}/events?types=worker_progress&limit=6",
            task.id
        )),
    )
    .await
    .json();
    assert_eq!(progress_page["items"].as_array().expect("items").len(), 6);
    assert_eq!(progress_page["has_more"], true);

    let missing = send(
        &app,
        get(&format!("/api/v1/tasks/{}/events", TaskId::new())),
    )
    .await;
    assert_problem(&missing, 404, "task_not_found");
}

#[tokio::test]
async fn global_events_page_by_id_with_task_and_type_filters() {
    let env = TestEnv::new();
    let app = env.router();
    let a = new_task(TaskKind::Execute, Status::Draft);
    let b = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&a);
    env.seed(&b);
    for i in 0..3 {
        env.store
            .append_event(a.id, &progress(&format!("a{i}")))
            .expect("append");
        env.store
            .append_event(b.id, &progress(&format!("b{i}")))
            .expect("append");
    }
    let latest = env.store.latest_event_id().expect("latest");

    let all = send(&app, get("/api/v1/events")).await.json();
    let all_ids: Vec<u64> = all["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["id"].as_u64().expect("id"))
        .collect();
    assert_eq!(all_ids.len() as u64, latest);
    assert!(all_ids.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(all["has_more"], false);

    let paged = send(
        &app,
        get(&format!("/api/v1/events?after_id={}&limit=2", all_ids[1])),
    )
    .await
    .json();
    let paged_ids: Vec<u64> = paged["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["id"].as_u64().expect("id"))
        .collect();
    assert_eq!(paged_ids, all_ids[2..4].to_vec());
    assert_eq!(paged["has_more"], true);

    let only_b = send(
        &app,
        get(&format!(
            "/api/v1/events?task_id={}&types=worker_progress&after_id={}",
            b.id, all_ids[3]
        )),
    )
    .await
    .json();
    let items = only_b["items"].as_array().expect("items");
    assert!(
        items
            .iter()
            .all(|i| i["task_id"] == b.id.to_string() && i["event"]["type"] == "worker_progress")
    );
    assert!(
        items
            .iter()
            .all(|i| i["id"].as_u64().expect("id") > all_ids[3])
    );
    assert_eq!(items.len(), 2, "{only_b}");
}
