//! ADR-0068 D5: `GET /tasks/{id}/routing`（run ごとの routing の監査と、タスクの routing の出自）。

mod common;

use common::*;
use serde_json::json;
use task_core::{Event, Status, TaskId, TaskKind, TaskRouting, TaskStore, TierSource};

fn admin() -> [(&'static str, &'static str); 1] {
    [("authorization", "Bearer s3cret-token-value")]
}

fn env_with_token() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.to_string()),
        ..EnvOptions::default()
    })
}

fn routing_decided(run_id: &str, escalation: Option<&str>) -> Event {
    serde_json::from_value(json!({
        "type": "routing_decided",
        "run_id": run_id,
        "record": {
            "org_node": "software-engineering",
            "harness": "coding",
            "decision": {
                "lane": "standard",
                "proposed": "standard",
                "source": "default",
                "rule_id": "standard/default",
                "policy_version": "v1",
                "features": {
                    "judgment": "low", "ambiguity": "low", "verifiability": "high",
                    "reversibility": "high", "consequence": "low", "context_size": "medium",
                    "tool_intensity": "medium", "expected_length": "medium", "cross_cutting": "low"
                },
                "reasons": ["no rule above matched"],
                "escalation": escalation
            },
            "resolution": {
                "lane": "standard",
                "adapter": "claude-code",
                "provider": "cc-1",
                "model_id": "model-std",
                "reasoning_effort": "medium"
            }
        }
    }))
    .expect("routing_decided event")
}

/// run の監査（lane・model・規則・features・メトリクス・レビュー）と、捨てた担当が返る。
#[tokio::test]
async fn routing_returns_per_run_audit_and_dropped_assignee() {
    let env = env_with_token();
    let app = env.router();
    let mut task = new_task(TaskKind::Execute, Status::Done);
    task.assignee = Some("software-engineering".into());
    task.routing = Some(TaskRouting {
        tier_source: TierSource::Hint,
        dropped_assignee: Some("research".into()),
        ..TaskRouting::default()
    });
    env.seed(&task);
    let id = task.id;
    for (run, esc) in [("run-1", None), ("run-2", Some("escalated: standard -> frontier"))] {
        env.store
            .append_event(id, &routing_decided(run, esc))
            .expect("routing");
        env.store
            .append_event(
                id,
                &Event::WorkerFinished {
                    run_id: run.into(),
                    outcome: "done: ok".into(),
                    usage: None,
                    role: None,
                    metrics: Some(task_core::RunMetrics {
                        wall_ms: 1234,
                        retries: 1,
                    }),
                },
            )
            .expect("finished");
    }

    let resp = send(
        &app,
        get_with(&format!("/api/v1/tasks/{id}/routing"), &admin()),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["task_id"], id.to_string());
    assert_eq!(body["assignee"], "software-engineering");
    assert_eq!(body["routing"]["dropped_assignee"], "research");
    assert_eq!(body["routing"]["tier_source"], "hint");
    let runs = body["runs"].as_array().cloned().expect("runs");
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0]["run_id"], "run-1");
    assert_eq!(runs[0]["org_node"], "software-engineering");
    assert_eq!(runs[0]["harness"], "coding");
    assert_eq!(runs[0]["lane"], "standard");
    assert_eq!(runs[0]["model"], "model-std");
    assert_eq!(runs[0]["rule_id"], "standard/default");
    assert_eq!(runs[0]["policy_version"], "v1");
    assert_eq!(runs[0]["features"]["judgment"], "low");
    assert_eq!(runs[0]["wall_ms"], 1234);
    assert_eq!(runs[0]["retries"], 1);
    assert!(runs[0].get("escalation").is_none());
    assert_eq!(runs[1]["escalation"], "escalated: standard -> frontier");
}

/// run の無いタスクは `runs: []`（200）。知らないタスクは 404、クエリは 400、トークン無しは 401。
#[tokio::test]
async fn routing_is_empty_without_runs_and_404_for_unknown_tasks() {
    let env = env_with_token();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Ready);
    env.seed(&task);

    let resp = send(
        &app,
        get_with(&format!("/api/v1/tasks/{}/routing", task.id), &admin()),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["runs"], json!([]));
    assert!(body.get("routing").is_none() || body["routing"]["dropped_assignee"].is_null());

    let resp = send(
        &app,
        get_with(&format!("/api/v1/tasks/{}/routing", TaskId::new()), &admin()),
    )
    .await;
    assert_eq!(resp.status, 404, "{}", resp.text());

    let resp = send(
        &app,
        get_with(&format!("/api/v1/tasks/{}/routing?x=1", task.id), &admin()),
    )
    .await;
    assert_eq!(resp.status, 400, "{}", resp.text());

    let resp = send(&app, get(&format!("/api/v1/tasks/{}/routing", task.id))).await;
    assert_eq!(resp.status, 401, "{}", resp.text());
}
