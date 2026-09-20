//! ADR-0048 D1/D2（Phase 60a）: Console の読み取り側。
//!
//! - 偽アダプタの run と同じ形（task → progress（run ごとに 1 件）→ report）が 1 本になること
//! - 範囲（`all` / `project:<id>` / `node:<id>`）の絞り込み
//! - カーソルでの続き読み（`since`）と `limit`
//! - SSE が `progress` ブロック → 終了の `task` ブロックの順に流すこと
//! - `GET /tasks/{id}/runs/{run}/events`（折り畳んだ進行を開いたときの全行）
//!
//! 外部ネットワークには出ない（tempfile の SQLite と `tower::ServiceExt::oneshot`）。

mod common;

use std::time::Duration;

use common::*;
use task_api::StreamTuning;
use task_core::{
    Approval, ApprovalStore, Event, Message, MessageId, MessageRole, MilestoneStatus, ProgressFields, ProgressKind,
    Project, ProjectId, Report, ReportKind, ReportStore, Status, Task, TaskKind, TaskStore,
};
use time::OffsetDateTime;

const RUN: &str = "01J9ZX5T3K8Q7W6V5R4P3N2M1J";

fn fast(env: &TestEnv) -> task_api::ApiState {
    env.state.clone().with_stream_tuning(StreamTuning {
        poll_interval: Duration::from_millis(20),
        heartbeat_interval: Duration::from_millis(500),
        ..StreamTuning::default()
    })
}

fn kinds(page: &serde_json::Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|b| b["kind"].as_str().unwrap_or("?").to_string())
        .collect()
}

fn project(env: &TestEnv, title: &str) -> Project {
    let now = OffsetDateTime::now_utc();
    let project = Project {
        id: ProjectId::new(),
        title: title.to_string(),
        request: "do it".into(),
        status: task_core::ProjectStatus::Active,
        secretary_summary: None,
        workspace: None,
        archived_at: None,
        paused_from: None,
        created_at: now,
        updated_at: now,
    };
    env.store.project_create(&project).expect("project");
    project
}

fn message(node_id: &str, project_id: Option<ProjectId>, role: MessageRole, text: &str) -> Message {
    Message {
        id: MessageId::new(),
        node_id: node_id.to_string(),
        project_id,
        role,
        text: text.to_string(),
        run_id: None,
        task_id: None,
        created_at: OffsetDateTime::now_utc(),
    }
}

/// 偽アダプタの run と同じ形（`ready → running`、進行 5 行、`running → done`）を 1 つのタスクに積む。
fn fake_run(env: &TestEnv, task: &Task) {
    env.store
        .append_event(
            task.id,
            &Event::Transitioned {
                from: Status::Ready,
                to: Status::Running,
                reason: "dispatch".into(),
            },
        )
        .expect("transition");
    env.store
        .append_event(
            task.id,
            &Event::WorkerStarted {
                run_id: RUN.into(),
                adapter: "fake".into(),
                model: "none".into(),
                provider: None,
                account: None,
                role: None,
                task_role: None,
            },
        )
        .expect("started");
    // ADR-0048 D2: `fake` は節目（`status`）だけ、`claude-code` 相当の道具の行も混ぜて数を見る。
    env.store
        .append_event(
            task.id,
            &Event::worker_progress_with(
                RUN,
                "fake worker",
                ProgressFields::of(ProgressKind::Status).with_summary("fake worker"),
            ),
        )
        .expect("progress");
    for i in 0..3 {
        env.store
            .append_event(
                task.id,
                &Event::worker_progress_with(
                    RUN,
                    format!("tool_use: Bash cargo test {i}"),
                    ProgressFields::of(ProgressKind::ToolUse)
                        .with_tool("Bash")
                        .with_summary(format!("cargo test {i}")),
                ),
            )
            .expect("progress");
    }
    // 従来の文字列だけの進行も混ざる（ADR-0048 D2: 混在できる）。
    env.store
        .append_event(task.id, &Event::worker_progress(RUN, "plain line"))
        .expect("progress");
    env.store
        .append_event(
            task.id,
            &Event::Transitioned {
                from: Status::Running,
                to: Status::Done,
                reason: "worker_done".into(),
            },
        )
        .expect("transition");
}

fn report(env: &TestEnv, project_id: Option<ProjectId>, node_id: &str, headline: &str) -> Report {
    let report = Report {
        id: task_core::ReportId::new(),
        project_id,
        node_id: node_id.to_string(),
        task_id: None,
        kind: ReportKind::Progress,
        level: 0,
        headline: headline.to_string(),
        body: "本文".into(),
        sources: Vec::new(),
        read_at: None,
        created_at: OffsetDateTime::now_utc(),
    };
    env.store.report_append(&report).expect("report");
    report
}

/// 受け入れ 1: 偽アダプタの run が `task` → `progress`（run ごとに 1 件）→ `report` の 1 本になる。
#[tokio::test]
async fn a_fake_run_becomes_one_task_progress_and_report_stream() {
    let env = TestEnv::new();
    let mut task = new_task(TaskKind::Execute, Status::Ready);
    task.assignee = Some("coding".into());
    task.title = "直す".into();
    env.seed(&task);
    fake_run(&env, &task);
    report(&env, None, "secretary", "直した");

    let app = env.router();
    let resp = send(&app, get("/api/v1/console")).await;
    assert_eq!(resp.status, 200);
    let page = resp.json();
    assert_eq!(
        kinds(&page),
        vec!["task", "progress", "task", "report"],
        "{page:#}"
    );

    let items = page["items"].as_array().expect("items");
    // 進行は run ごとに 1 件に束ねられ、件数と道具の回数、最後の `status` を持つ。
    let progress = &items[1];
    assert_eq!(progress["progress"]["run_id"], RUN);
    assert_eq!(progress["progress"]["count"], 5);
    assert_eq!(progress["progress"]["tool_count"], 3);
    assert_eq!(progress["progress"]["last_status"], "fake worker");
    assert_eq!(progress["title"], "直す");
    assert_eq!(progress["assignee"], "coding");
    // 折り畳みの見出し用に、始めと終わりの数行だけを載せる（全行は run の events で取る）。
    assert_eq!(progress["progress"]["first"].as_array().map(Vec::len), Some(3));
    assert_eq!(progress["progress"]["last"].as_array().map(Vec::len), Some(2));
    assert_eq!(progress["progress"]["first"][0]["kind"], "status");
    assert_eq!(progress["progress"]["first"][1]["tool"], "Bash");
    // 構造化されていない行は `msg` がそのまま出る。
    assert_eq!(progress["progress"]["last"][1]["text"], "plain line");

    // `task` ブロックは 1 行（担当・harness・tier・mode・経過）。
    assert_eq!(items[0]["task"]["to"], "running");
    assert_eq!(items[0]["task"]["assignee"], "coding");
    assert_eq!(items[0]["task"]["tier"], "standard");
    assert!(items[0]["task"]["elapsed_secs"].is_number());
    assert_eq!(items[2]["task"]["to"], "done");
    assert_eq!(items[2]["task"]["reason"], "worker_done");
    assert_eq!(items[3]["report"]["headline"], "直した");
    assert!(page["next_cursor"].is_string());
}

/// 受け入れ 1: 範囲の 3 種（`all` / `project:<id>` / `node:<id>`）。
#[tokio::test]
async fn scopes_filter_by_project_and_by_node() {
    let env = TestEnv::new();
    let pluvio = project(&env, "Pluvio");

    let mut mine = new_task(TaskKind::Execute, Status::Ready);
    mine.project_id = Some(pluvio.id);
    mine.assignee = Some("coding".into());
    env.seed(&mine);
    fake_run(&env, &mine);

    let mut other = new_task(TaskKind::Execute, Status::Ready);
    other.assignee = Some("research".into());
    env.seed(&other);
    fake_run(&env, &other);

    env.store
        .message_append(&message("secretary", Some(pluvio.id), MessageRole::User, "進めて"))
        .expect("message");
    env.store
        .message_append(&message("research", None, MessageRole::Node, "読みました"))
        .expect("message");
    report(&env, Some(pluvio.id), "secretary", "案件の報告");
    report(&env, None, "research", "雑報告");

    let app = env.router();

    // `all` は全部（タスク 2 件分の遷移 4 + 進行 2 + 対話 2 + 報告 2）。
    let all = send(&app, get("/api/v1/console?scope=all")).await.json();
    assert_eq!(kinds(&all).len(), 10, "{all:#}");

    // 案件で絞る: その案件のタスクと、その案件についての対話・報告だけ。
    let scoped = send(&app, get(&format!("/api/v1/console?scope=project:{}", pluvio.id))).await;
    let page = scoped.json();
    assert_eq!(kinds(&page), vec!["task", "progress", "task", "human", "report"], "{page:#}");
    for item in page["items"].as_array().expect("items") {
        let has_other = item.to_string().contains(&other.id.to_string());
        assert!(!has_other, "案件の外のタスクが混ざった: {item}");
    }

    // ノードで絞る: そのノードのタスクと、そのノードとの対話・報告だけ。
    let page = send(&app, get("/api/v1/console?scope=node:research")).await.json();
    assert_eq!(kinds(&page), vec!["task", "progress", "task", "reply", "report"], "{page:#}");
    assert_eq!(page["items"][3]["text"], "読みました");
    assert_eq!(page["items"][4]["report"]["headline"], "雑報告");

    // 形の違う範囲は 400。
    let bad = send(&app, get("/api/v1/console?scope=project:nope")).await;
    assert_problem(&bad, 400, "bad_request");
    let bad = send(&app, get("/api/v1/console?scope=bogus")).await;
    assert_problem(&bad, 400, "bad_request");
    let bad = send(&app, get("/api/v1/console?bogus=1")).await;
    assert_problem(&bad, 400, "bad_request");
}

/// 受け入れ 1: `limit` と `since` で続きが読める（同じブロックを 2 回返さない）。
#[tokio::test]
async fn the_cursor_pages_through_without_repeating_blocks() {
    let env = TestEnv::new();
    let mut task = new_task(TaskKind::Execute, Status::Ready);
    task.assignee = Some("coding".into());
    env.seed(&task);
    fake_run(&env, &task);
    for i in 0..4 {
        report(&env, None, "secretary", &format!("報告 {i}"));
    }

    let app = env.router();
    let first = send(&app, get("/api/v1/console?limit=3")).await.json();
    assert_eq!(first["items"].as_array().map(Vec::len), Some(3));
    let cursor = first["next_cursor"].as_str().expect("next_cursor").to_string();

    // 続きは重ならない。
    let second = send(&app, get(&format!("/api/v1/console?limit=10&since={cursor}"))).await.json();
    let seen: Vec<String> = first["items"]
        .as_array()
        .expect("items")
        .iter()
        .chain(second["items"].as_array().expect("items"))
        .map(|b| b["cursor"].as_str().unwrap_or_default().to_string())
        .collect();
    let unique: std::collections::HashSet<&String> = seen.iter().collect();
    assert_eq!(seen.len(), unique.len(), "同じブロックを 2 回返した: {seen:?}");

    // 全部読み切ったら空（カーソルはそのまま返る）。
    let last = second["next_cursor"].as_str().expect("next_cursor").to_string();
    let empty = send(&app, get(&format!("/api/v1/console?since={last}"))).await.json();
    assert_eq!(empty["items"].as_array().map(Vec::len), Some(0), "{empty:#}");
    assert_eq!(empty["next_cursor"], last);

    // 壊れたカーソルは 400、`limit` の上限は丸める。
    let bad = send(&app, get("/api/v1/console?since=nope")).await;
    assert_problem(&bad, 400, "bad_request");
    let clamped = send(&app, get("/api/v1/console?limit=9999")).await;
    assert_eq!(clamped.status, 200);
}

/// 受け入れ 1: 認可・途中目標・質問のブロック（その場で答えるための状態ごと）。
#[tokio::test]
async fn approvals_milestones_and_questions_carry_their_state() {
    let env = TestEnv::new();
    let pluvio = project(&env, "Pluvio");
    let mut task = new_task(TaskKind::Execute, Status::Blocked);
    task.project_id = Some(pluvio.id);
    task.assignee = Some("coding".into());
    env.seed(&task);
    env.store
        .append_event(
            task.id,
            &Event::QuestionRaised {
                run_id: RUN.into(),
                text: "どのブランチに入れますか".into(),
            },
        )
        .expect("question");

    let approval = Approval {
        id: task_core::ApprovalId::new(),
        project_id: Some(pluvio.id),
        node_id: "coding".into(),
        task_id: Some(task.id),
        question: "別の部の課に頼んでよいか".into(),
        decision: None,
        answer: None,
        created_at: OffsetDateTime::now_utc(),
        decided_at: None,
    };
    env.store.approval_append(&approval).expect("approval");
    let milestone = env
        .store
        .milestone_create(pluvio.id, "第 1 目標", "まず動かす", MilestoneStatus::Proposed)
        .expect("milestone");

    let app = env.router();
    let page = send(&app, get(&format!("/api/v1/console?scope=project:{}", pluvio.id))).await.json();
    let items = page["items"].as_array().expect("items").clone();

    let question = items.iter().find(|b| b["kind"] == "question").expect("question block");
    assert_eq!(question["text"], "どのブランチに入れますか");
    assert_eq!(question["answered"], false);
    assert_eq!(question["node_id"], "coding");
    assert_eq!(question["run_id"], RUN);

    let block = items.iter().find(|b| b["kind"] == "approval").expect("approval block");
    assert_eq!(block["approval"]["id"], approval.id.to_string());
    assert!(block["approval"]["decision"].is_null(), "未決のまま渡る");

    let block = items.iter().find(|b| b["kind"] == "milestone").expect("milestone block");
    assert_eq!(block["milestone"]["id"], milestone.id.to_string());
    assert_eq!(block["milestone"]["status"], "proposed");
    assert!(block["review"].is_null(), "秘書の返事はまだ無い");

    // 人が答えたら `answered` になる。
    env.store
        .append_event(
            task.id,
            &Event::Answered {
                question: "どのブランチに入れますか".into(),
                answer: "main で".into(),
            },
        )
        .expect("answered");
    let page = send(&app, get(&format!("/api/v1/console?scope=project:{}", pluvio.id))).await.json();
    let question = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|b| b["kind"] == "question")
        .expect("question block")
        .clone();
    assert_eq!(question["answered"], true);
    assert_eq!(question["answer"], "main で");
}

/// 受け入れ 1: SSE が `progress` ブロックを流し、終わると `task` ブロックが来る。
#[tokio::test]
async fn the_stream_emits_a_progress_block_and_then_the_finished_task_block() {
    let env = TestEnv::new();
    let mut task = new_task(TaskKind::Execute, Status::Running);
    task.assignee = Some("coding".into());
    env.seed(&task);
    let app = task_api::router(fast(&env));

    let mut sse = open_stream(&app, get("/api/v1/console/stream?scope=all")).await;
    assert_eq!(sse.status, 200);
    assert_eq!(
        sse.headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("text/event-stream; charset=utf-8")
    );
    let hello = sse.next_frame(Duration::from_millis(500)).await.expect("hello");
    assert_eq!(hello.event, "hello");
    assert_eq!(hello.data["scope"], "all");
    assert!(hello.data["cursor"].is_string());

    env.store
        .append_event(
            task.id,
            &Event::worker_progress_with(
                RUN,
                "tool_use: Bash cargo test",
                ProgressFields::of(ProgressKind::ToolUse)
                    .with_tool("Bash")
                    .with_summary("cargo test --workspace"),
            ),
        )
        .expect("progress");
    let frame = sse
        .next_named("console.block", Duration::from_secs(3))
        .await
        .expect("progress block");
    assert_eq!(frame.data["kind"], "progress");
    assert_eq!(frame.data["progress"]["run_id"], RUN);
    assert_eq!(frame.data["progress"]["tool_count"], 1);
    assert_eq!(frame.data["progress"]["first"][0]["text"], "cargo test --workspace");

    env.store
        .append_event(
            task.id,
            &Event::Transitioned {
                from: Status::Running,
                to: Status::Done,
                reason: "worker_done".into(),
            },
        )
        .expect("transition");
    let frame = loop {
        let frame = sse
            .next_named("console.block", Duration::from_secs(3))
            .await
            .expect("task block");
        if frame.data["kind"] == "task" {
            break frame;
        }
    };
    assert_eq!(frame.data["task"]["to"], "done");
    assert_eq!(frame.data["task"]["assignee"], "coding");
}

/// 受け入れ 2: 折り畳んだ進行を開いたときの全行（`GET /tasks/{id}/runs/{run}/events`）。
#[tokio::test]
async fn run_events_returns_only_that_runs_rows() {
    let env = TestEnv::new();
    let task = new_task(TaskKind::Execute, Status::Running);
    env.seed(&task);
    fake_run(&env, &task);
    env.store
        .append_event(task.id, &Event::worker_progress("other-run", "別の run"))
        .expect("progress");

    let app = env.router();
    let resp = send(&app, get(&format!("/api/v1/tasks/{}/runs/{RUN}/events", task.id))).await;
    assert_eq!(resp.status, 200);
    let page = resp.json();
    let items = page["items"].as_array().expect("items");
    // `worker_started` 1 + `worker_progress` 5（遷移は run に紐づかないので入らない）。
    assert_eq!(items.len(), 6, "{page:#}");
    assert!(items.iter().all(|row| row["event"]["run_id"] == RUN), "{page:#}");
    assert_eq!(page["has_more"], false);
    // 構造化フィールドがイベントに残っている（ADR-0048 D2）。
    let tool = items
        .iter()
        .find(|row| row["event"]["kind"] == "tool_use")
        .expect("tool_use row");
    assert_eq!(tool["event"]["tool"], "Bash");
    assert_eq!(tool["event"]["summary"], "cargo test 0");
    // 構造化していない進行は余計な鍵を持たない（追加のみの互換）。
    let plain = items
        .iter()
        .find(|row| row["event"]["msg"] == "plain line")
        .expect("plain row");
    assert!(plain["event"]["kind"].is_null() && plain["event"]["tool"].is_null(), "{plain}");

    // 知らないタスクは 404、`limit` は効く。
    let missing = send(
        &app,
        get(&format!("/api/v1/tasks/{}/runs/{RUN}/events", task_core::TaskId::new())),
    )
    .await;
    assert_problem(&missing, 404, "task_not_found");
    let page = send(&app, get(&format!("/api/v1/tasks/{}/runs/{RUN}/events?limit=2", task.id)))
        .await
        .json();
    assert_eq!(page["items"].as_array().map(Vec::len), Some(2));
    assert_eq!(page["has_more"], true);
}
