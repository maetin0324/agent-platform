//! api.md §8.9（daemon の watch）、`GET /providers`（設定 + スナップショット + 集計）、`GET /config`、
//! api.md §8.10（`GET /schema`）。

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use task_core::{Event, Status, TaskKind, TaskStore, Usage};
use time::OffsetDateTime;

#[tokio::test]
async fn daemon_view_is_null_until_a_snapshot_is_sent() {
    let env = TestEnv::new();
    let app = env.router();

    let before = send(&app, get("/api/v1/daemon")).await;
    assert_eq!(before.status, 200);
    let body = before.json();
    assert!(body["snapshot"].is_null());
    assert!(body["now"].as_str().is_some_and(|s| s.ends_with('Z')));

    let snapshot = snapshot(7);
    env.daemon_tx.send(Some(snapshot.clone())).expect("send snapshot");
    let after = send(&app, get("/api/v1/daemon")).await.json();
    assert_eq!(after["snapshot"], serde_json::to_value(&snapshot).expect("json"));
    assert_eq!(after["snapshot"]["cooldowns"][0]["reason"], "throttled");
    assert_eq!(after["snapshot"]["in_flight"][0]["kind"], "worker");
}

#[tokio::test]
async fn stream_sends_daemon_events_when_the_snapshot_changes() {
    let env = TestEnv::new();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let app = env.router();

    let mut sse = open_stream(&app, get(&format!("/api/v1/stream?task_id={}", task.id))).await;
    let hello = sse.next_frame(Duration::from_secs(2)).await.expect("hello");
    assert!(hello.data["daemon"].is_null());

    let first = snapshot(1);
    env.daemon_tx.send(Some(first.clone())).expect("send");
    let frame = sse.next_named("daemon", Duration::from_secs(2)).await.expect("daemon event");
    assert_eq!(frame.data, serde_json::to_value(&first).expect("json"));
    assert!(frame.id.is_none());

    let second = snapshot(2);
    env.daemon_tx.send(Some(second.clone())).expect("send");
    let frame = sse.next_named("daemon", Duration::from_secs(2)).await.expect("daemon event");
    assert_eq!(frame.data["ticks"], 2);

    let mut late = open_stream(&app, get("/api/v1/stream")).await;
    let hello = late.next_frame(Duration::from_secs(2)).await.expect("hello");
    assert_eq!(hello.data["daemon"], serde_json::to_value(&second).expect("json"));
    assert!(late.next_named("daemon", Duration::from_millis(300)).await.is_none(), "unchanged snapshot is not resent");
}

fn started(run_id: &str, provider: Option<&str>) -> Event {
    Event::WorkerStarted {
        run_id: run_id.into(),
        adapter: "claude-code".into(),
        model: "claude-sonnet-5".into(),
        provider: provider.map(str::to_string),
        role: None,
    }
}

fn finished(run_id: &str, outcome: &str, usage: Option<Usage>) -> Event {
    Event::WorkerFinished {
        run_id: run_id.into(),
        outcome: outcome.into(),
        usage,
        role: None,
    }
}

#[tokio::test]
async fn providers_combine_config_snapshot_and_incremental_stats() {
    let env = TestEnv::new();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Running);
    env.seed_with(
        &task,
        vec![
            started("r1", Some("claude-a")),
            finished("r1", "done: ok", Some(Usage { input_tokens: Some(100), output_tokens: Some(20) })),
            started("r2", Some("claude-b")),
            finished("r2", "requeue: throttled", None),
            started("r3", None),
            finished("r3", "error(retryable=false): boom", Some(Usage { input_tokens: None, output_tokens: Some(3) })),
            started("r4", Some("claude-a")),
        ],
    );
    let today = {
        let d = OffsetDateTime::now_utc().date();
        format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
    };

    let body = send(&app, get("/api/v1/providers")).await;
    assert_eq!(body.status, 200, "{}", body.text());
    let items = body.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items.len(), 2, "only configured providers, in config order");
    assert_eq!(items[0]["id"], "claude-a");
    assert_eq!(items[0]["adapter"], "claude-code");
    assert_eq!(items[0]["tiers"], json!(["frontier", "standard"]));
    assert_eq!(items[0]["concurrency"], 2);
    assert_eq!(items[0]["model"], "claude-sonnet-5");
    assert_eq!(items[0]["env_keys"], json!(["CLAUDE_CONFIG_DIR"]));
    assert!(items[0]["in_use"].is_null() && items[0]["cooldown"].is_null(), "no snapshot yet");
    assert_eq!(
        items[0]["stats"],
        json!({"runs": 2, "done": 1, "question": 0, "error": 0, "requeue": 0, "lease_expired": 0,
               "input_tokens": 100, "output_tokens": 20,
               "by_day": [{"day": today, "runs": 1, "input_tokens": 100, "output_tokens": 20}]})
    );
    assert_eq!(items[1]["id"], "claude-b");
    assert_eq!(items[1]["stats"]["requeue"], 1);
    assert_eq!(items[1]["stats"]["runs"], 1);

    env.daemon_tx.send(Some(snapshot(3))).expect("send");
    env.store.append_event(task.id, &finished("r4", "question: which?", None)).expect("append");
    env.store.append_event(task.id, &started("r5", Some("claude-b"))).expect("append");
    env.store.append_event(task.id, &finished("r5", "lease_expired", None)).expect("append");

    let items = send(&app, get("/api/v1/providers")).await.json()["items"].as_array().cloned().expect("items");
    assert_eq!(items[0]["in_use"], 1);
    assert!(items[0]["cooldown"].is_null());
    assert_eq!(items[0]["stats"]["question"], 1);
    assert_eq!(items[0]["stats"]["by_day"][0]["runs"], 2);
    assert_eq!(items[1]["in_use"], 0);
    assert_eq!(items[1]["cooldown"], json!({"provider": "claude-b", "until": "2026-09-14T00:05:00Z", "reason": "throttled"}));
    assert_eq!((items[1]["stats"]["runs"].as_u64(), items[1]["stats"]["lease_expired"].as_u64()), (Some(2), Some(1)));
}

#[tokio::test]
async fn config_is_returned_as_given_and_secrets_never_appear() {
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    });
    let app = env.router();
    let auth = format!("Bearer {TOKEN}");
    let resp = send(&app, get_with("/api/v1/config", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json(), serde_json::to_value(config_view()).expect("json"));

    for path in ["/api/v1/config", "/api/v1/providers", "/api/v1/daemon", "/api/v1/health", "/api/v1/events"] {
        let resp = send(&app, get_with(path, &[("authorization", &auth)])).await;
        assert_eq!(resp.status, 200, "{path}");
        assert!(!resp.text().contains(TOKEN), "{path} leaks the token");
    }
    let unauthorized = send(&app, get("/api/v1/config")).await;
    assert!(!unauthorized.text().contains(TOKEN));
}

#[tokio::test]
async fn schema_endpoint_returns_the_committed_file() {
    let env = TestEnv::new();
    let resp = send(&env.router(), get("/api/v1/schema")).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.header("content-type"), Some("application/schema+json"));
    let committed = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/api/v1/api-v1.schema.json"))
        .expect("committed schema");
    assert_eq!(resp.text(), committed);
    assert_eq!(resp.text(), task_api::API_V1_SCHEMA_JSON);
    let value: Value = resp.json();
    assert_eq!(value["title"], "ApiV1Schema");
    assert!(value["$defs"].is_object());
}
