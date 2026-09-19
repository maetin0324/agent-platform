//! api.md §8.8（SSE）: 2 秒以内の配信、`Last-Event-ID` での再開、10,001 件遅れの `reset`、17 本目の 503、
//! 切断後のポーリング停止、heartbeat と `task_id` の絞り込み、実際の loopback TCP での配信と停止。

mod common;

use std::time::Duration;

use common::*;
use task_api::StreamTuning;
use task_core::{Event, Status, TaskKind, TaskStore};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TWO_SECONDS: Duration = Duration::from_secs(2);

fn fast(env: &TestEnv) -> task_api::ApiState {
    env.state.clone().with_stream_tuning(StreamTuning {
        poll_interval: Duration::from_millis(20),
        heartbeat_interval: Duration::from_millis(100),
        ..StreamTuning::default()
    })
}

#[tokio::test]
async fn hello_then_created_event_arrives_within_two_seconds() {
    let env = TestEnv::new();
    let seeded = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&seeded);
    let app = env.router();

    let mut sse = open_stream(&app, get("/api/v1/stream")).await;
    assert_eq!(sse.status, 200);
    assert_eq!(sse.headers.get("content-type").and_then(|v| v.to_str().ok()), Some("text/event-stream; charset=utf-8"));
    assert_eq!(sse.headers.get("cache-control").and_then(|v| v.to_str().ok()), Some("no-store"));
    assert_eq!(sse.headers.get("x-accel-buffering").and_then(|v| v.to_str().ok()), Some("no"));

    let hello = sse.next_frame(Duration::from_millis(500)).await.expect("hello is flushed immediately");
    assert_eq!(hello.event, "hello");
    let latest = env.store.latest_event_id().expect("latest");
    assert_eq!(hello.data["cursor"], latest);
    assert!(hello.data["daemon"].is_null());
    assert!(hello.data["now"].is_string());

    // `taskctl add` 相当（別接続で task-ops を通して作る）。
    let spec: task_ops::add::NewTaskSpec = serde_json::from_value(serde_json::json!({
        "title": "sse probe", "objective": "x", "acceptance": [{"type": "human", "text": "y"}]
    }))
    .expect("spec");
    let started = tokio::time::Instant::now();
    let task = task_ops::add::create_task(&env.store, spec, OffsetDateTime::now_utc()).expect("create");
    let frame = sse.next_named("task.event", TWO_SECONDS).await.expect("task.event within 2 s");
    assert!(started.elapsed() < TWO_SECONDS);
    assert_eq!(frame.id, Some(latest + 1));
    assert_eq!(frame.data["id"], latest + 1);
    assert_eq!(frame.data["task_id"], task.id.to_string());
    assert_eq!(frame.data["seq"], 0);
    assert_eq!(frame.data["event"]["type"], "created");
    assert_eq!(frame.data["event"]["task"]["title"], "sse probe");
}

#[tokio::test]
async fn last_event_id_resumes_without_gaps_or_duplicates() {
    let env = TestEnv::new();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed_with(&task, (0..5).map(|i| progress(&format!("p{i}"))).collect());
    let app = task_api::router(fast(&env));
    let ids: Vec<u64> = env.store.events_since(0, 100).expect("rows").iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 6);

    let mut sse = open_stream(&app, get_with("/api/v1/stream", &[("last-event-id", &ids[1].to_string())])).await;
    let hello = sse.next_frame(TWO_SECONDS).await.expect("hello");
    assert_eq!(hello.data["cursor"], ids[1]);
    let mut received = Vec::new();
    for _ in 2..6 {
        received.push(sse.next_named("task.event", TWO_SECONDS).await.expect("backlog").id.expect("id"));
    }
    assert_eq!(received, ids[2..].to_vec());

    env.store.append_event(task.id, &progress("live")).expect("append");
    let live = sse.next_named("task.event", TWO_SECONDS).await.expect("live").id.expect("id");
    assert_eq!(live, ids[5] + 1);
    drop(sse);

    // 切断中に書かれた分を、最後に受け取った id から再開して取りこぼさない。
    env.store.append_event(task.id, &progress("missed-1")).expect("append");
    env.store.append_event(task.id, &progress("missed-2")).expect("append");
    let mut resumed = open_stream(&app, get_with("/api/v1/stream", &[("last-event-id", &live.to_string())])).await;
    assert_eq!(resumed.next_frame(TWO_SECONDS).await.expect("hello").data["cursor"], live);
    let first = resumed.next_named("task.event", TWO_SECONDS).await.expect("missed-1");
    let second = resumed.next_named("task.event", TWO_SECONDS).await.expect("missed-2");
    assert_eq!((first.id, second.id), (Some(live + 1), Some(live + 2)));
    assert_eq!(first.data["event"]["msg"], "missed-1");
    assert!(resumed.next_named("task.event", Duration::from_millis(200)).await.is_none(), "no duplicates");

    // `?after_id=` でも再開でき、`Last-Event-ID` が優先される。
    let mut by_query = open_stream(&app, get(&format!("/api/v1/stream?after_id={}", ids[4]))).await;
    assert_eq!(by_query.next_frame(TWO_SECONDS).await.expect("hello").data["cursor"], ids[4]);
    assert_eq!(by_query.next_named("task.event", TWO_SECONDS).await.expect("event").id, Some(ids[5]));
    let mut header_wins = open_stream(
        &app,
        get_with("/api/v1/stream?after_id=0", &[("last-event-id", &(live + 2).to_string())]),
    )
    .await;
    assert_eq!(header_wins.next_frame(TWO_SECONDS).await.expect("hello").data["cursor"], live + 2);
    assert!(header_wins.next_named("task.event", Duration::from_millis(200)).await.is_none());
}

#[tokio::test]
async fn cursors_far_behind_or_ahead_get_a_reset() {
    let env = TestEnv::new();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let base = env.store.latest_event_id().expect("latest");

    // 10,001 件を 1 トランザクションで足す（ディスパッチャの書き込み相当の行形式）。
    {
        let mut conn = rusqlite::Connection::open(&env.db_path).expect("open");
        conn.busy_timeout(Duration::from_secs(5)).expect("busy timeout");
        let tx = conn.transaction().expect("tx");
        let ts = OffsetDateTime::now_utc().format(&Rfc3339).expect("ts");
        let json = serde_json::to_string(&progress("bulk")).expect("json");
        {
            let mut stmt = tx.prepare("INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)").expect("prepare");
            for seq in 1..=10_001i64 {
                stmt.execute(rusqlite::params![task.id.to_string(), seq, ts, json]).expect("insert");
            }
        }
        tx.commit().expect("commit");
    }
    let latest = env.store.latest_event_id().expect("latest");
    assert_eq!(latest - base, 10_001);
    let app = task_api::router(fast(&env));

    let mut behind = open_stream(&app, get_with("/api/v1/stream", &[("last-event-id", &base.to_string())])).await;
    let hello = behind.next_frame(TWO_SECONDS).await.expect("hello");
    assert_eq!(hello.data["cursor"], latest);
    let reset = behind.next_frame(TWO_SECONDS).await.expect("reset");
    assert_eq!(reset.event, "reset");
    assert_eq!(reset.data, serde_json::json!({"reason": "cursor_too_old", "cursor": latest}));
    env.store.append_event(task.id, &progress("after reset")).expect("append");
    let next = behind.next_named("task.event", TWO_SECONDS).await.expect("event after reset");
    assert_eq!(next.id, Some(latest + 1), "old rows are not replayed after a reset");

    // ちょうど 10,000 件の遅れは reset しない。
    let within_id = env.store.latest_event_id().expect("latest") - 10_000;
    let mut within = open_stream(&app, get_with("/api/v1/stream", &[("last-event-id", &within_id.to_string())])).await;
    assert_eq!(within.next_frame(TWO_SECONDS).await.expect("hello").data["cursor"], within_id);
    let first = within.next_frame(TWO_SECONDS).await.expect("event");
    assert_eq!((first.event.as_str(), first.id), ("task.event", Some(within_id + 1)));

    let ahead_id = latest + 100;
    let mut ahead = open_stream(&app, get_with("/api/v1/stream", &[("last-event-id", &ahead_id.to_string())])).await;
    assert_eq!(ahead.next_frame(TWO_SECONDS).await.expect("hello").data["cursor"], latest + 1);
    let reset = ahead.next_frame(TWO_SECONDS).await.expect("reset");
    assert_eq!(reset.data, serde_json::json!({"reason": "cursor_ahead", "cursor": latest + 1}));
}

#[tokio::test]
async fn the_seventeenth_stream_is_rejected_with_503() {
    let env = TestEnv::new();
    let state = env.state.clone();
    let app = env.router();
    let mut open = Vec::new();
    for _ in 0..task_api::MAX_STREAMS {
        let mut sse = open_stream(&app, get("/api/v1/stream")).await;
        assert_eq!(sse.status, 200);
        assert!(sse.next_frame(TWO_SECONDS).await.is_some());
        open.push(sse);
    }
    assert_eq!(state.active_streams(), 16);

    let rejected = open_stream(&app, get("/api/v1/stream")).await;
    assert_eq!(rejected.status, 503);
    assert_eq!(rejected.headers.get("retry-after").and_then(|v| v.to_str().ok()), Some("5"));
    assert_eq!(rejected.headers.get("content-type").and_then(|v| v.to_str().ok()), Some("application/problem+json"));
    assert_eq!(rejected.into_body_json().await["code"], "too_many_streams");

    open.pop();
    assert!(eventually(TWO_SECONDS, || state.active_streams() == 15).await);
    let again = open_stream(&app, get("/api/v1/stream")).await;
    assert_eq!(again.status, 200);
}

#[tokio::test]
async fn disconnecting_stops_polling() {
    let env = TestEnv::new();
    let state = fast(&env);
    let app = task_api::router(state.clone());
    assert_eq!(state.stream_poll_count(), 0);

    let mut sse = open_stream(&app, get("/api/v1/stream")).await;
    assert!(sse.next_frame(TWO_SECONDS).await.is_some());
    let polls = state.stream_poll_count();
    assert!(eventually(TWO_SECONDS, || state.stream_poll_count() >= polls + 5).await, "polling while subscribed");
    assert_eq!(state.active_streams(), 1);

    drop(sse);
    assert!(eventually(TWO_SECONDS, || state.active_streams() == 0).await, "the stream slot is released");
    let after_disconnect = state.stream_poll_count();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(state.stream_poll_count(), after_disconnect, "no polling without subscribers");
}

#[tokio::test]
async fn heartbeat_and_task_filter() {
    let env = TestEnv::new();
    let a = new_task(TaskKind::Execute, Status::Draft);
    let b = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&a);
    env.seed(&b);
    let app = task_api::router(fast(&env));

    let mut sse = open_stream(&app, get(&format!("/api/v1/stream?task_id={}", a.id))).await;
    let hello = sse.next_frame(TWO_SECONDS).await.expect("hello");
    assert_eq!(hello.data["cursor"], env.store.latest_event_id().expect("latest"), "hello.cursor is not filtered");

    env.store.append_event(b.id, &progress("b")).expect("append");
    env.store.append_event(a.id, &progress("a")).expect("append");
    let frame = sse.next_named("task.event", TWO_SECONDS).await.expect("a's event");
    assert_eq!(frame.data["task_id"], a.id.to_string());
    assert_eq!(frame.data["event"]["msg"], "a");

    let beat = sse.next_named("heartbeat", TWO_SECONDS).await.expect("heartbeat");
    assert!(beat.data["now"].is_string());
    assert!(beat.id.is_none());
}

async fn read_until(stream: &mut tokio::net::TcpStream, buf: &mut Vec<u8>, needle: &str, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if String::from_utf8_lossy(buf).contains(needle) {
            return true;
        }
        let mut chunk = [0u8; 8192];
        match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return String::from_utf8_lossy(buf).contains(needle),
            Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_over_loopback_tcp_streams_events_and_closes_on_shutdown() {
    let env = TestEnv::new();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let state = fast(&env);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(task_api::serve_with_listener(listener, state.clone(), async move {
        let _ = stop_rx.await;
    }));

    let mut health = tokio::net::TcpStream::connect(addr).await.expect("connect");
    health
        .write_all(b"GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut buf = Vec::new();
    assert!(read_until(&mut health, &mut buf, "\"api_version\":\"1\"", TWO_SECONDS).await);
    assert!(String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 200"));

    let mut conn = tokio::net::TcpStream::connect(addr).await.expect("connect");
    conn.write_all(b"GET /api/v1/stream HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\n\r\n")
        .await
        .expect("write");
    let mut buf = Vec::new();
    assert!(read_until(&mut conn, &mut buf, "event: hello", TWO_SECONDS).await, "{}", String::from_utf8_lossy(&buf));
    assert!(String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 200"));

    env.store.append_event(task.id, &Event::ApprovalRequested).expect("append");
    assert!(read_until(&mut conn, &mut buf, "\"type\":\"approval_requested\"", TWO_SECONDS).await);
    assert!(String::from_utf8_lossy(&buf).contains("event: task.event\nid: "));

    let _ = stop_tx.send(());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut chunk = [0u8; 1024];
    loop {
        match tokio::time::timeout_at(deadline, conn.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) => break,
            Ok(Ok(_)) => continue,
            Err(_) => panic!("the SSE connection was not closed on shutdown"),
        }
    }
    let result = tokio::time::timeout(Duration::from_secs(5), server).await.expect("server stops").expect("join");
    assert!(result.is_ok(), "{result:?}");
    assert!(eventually(TWO_SECONDS, || state.active_streams() == 0).await);
}

#[tokio::test]
async fn serve_binds_the_configured_address_and_reports_bind_errors() {
    let env = TestEnv::new();
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let mut settings = settings(&env.db_path, &env.workspace_root, &env.docs_repo_root, EnvOptions::default());
    settings.listen = occupied.local_addr().expect("addr");
    let (_tx, rx) = tokio::sync::watch::channel(None);
    let err = task_api::serve(settings.clone(), rx.clone(), std::future::pending()).await;
    assert!(matches!(err, Err(task_api::ApiError::Bind { .. })), "{err:?}");

    settings.listen = "127.0.0.1:0".parse().expect("addr");
    let stopped = tokio::time::timeout(Duration::from_secs(5), task_api::serve(settings.clone(), rx.clone(), async {})).await;
    assert!(matches!(stopped, Ok(Ok(()))), "{stopped:?}");

    settings.db_path = env.dir.path().join("missing-dir").join("taskd.db");
    let err = task_api::serve(settings, rx, async {}).await;
    assert!(matches!(err, Err(task_api::ApiError::Store(_))), "{err:?}");
}
