//! ADR-0016 D1 / D3 / M3（DESIGN §6 Phase 10 の受け入れ 1・5）: `POST /tasks` の `role` / `aggregate` と
//! `[[roles]]` の既定の適用、`GET /config` の `roles[]` / `delegation`（指示文の本文は出さない）。

mod common;

use common::*;
use serde_json::json;
use task_core::{RoleSpec, TaskStore, Tier};

fn env_with_lead_role() -> TestEnv {
    TestEnv::with(EnvOptions {
        // ADR-0044 §5 Phase 53 追記（Phase 55）: `POST /tasks` は管理系（bearer 必須）。
        token: Some(TOKEN.to_string()),
        roles: vec![
            RoleSpec {
                id: "lead".into(),
                tier: Some(Tier::Frontier),
                adapter: None,
                max_turns: Some(40),
                max_wall_secs: None,
                instructions: Some("You lead the work. Delegate implementation.".into()),
            },
            RoleSpec {
                id: "implementer".into(),
                tier: Some(Tier::Cheap),
                adapter: Some("fake".into()),
                max_turns: None,
                max_wall_secs: Some(900),
                instructions: None,
            },
        ],
        ..EnvOptions::default()
    })
}

/// 受け入れ 1: `role` を指定すると `[[roles]]` の既定が入り、書いていない値は全体の既定のまま。
#[tokio::test]
async fn create_task_with_role_applies_role_defaults() {
    let env = env_with_lead_role();
    let app = env.router();

    let body = json!({
        "title": "lead the phase",
        "objective": "split the work and review it",
        "acceptance": [{"type": "human", "text": "the plan is sound"}],
        "role": "lead",
    });
    let resp = send(&app, post_admin("/api/v1/tasks", &body)).await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let task = resp.json();
    assert_eq!(task["role"], "lead");
    assert_eq!(task["worker_hint"]["tier"], "frontier");
    // 役割の既定（40）と全体の既定（600 / 2）が混ざる。
    assert_eq!(
        task["budget"],
        json!({"max_turns": 40, "max_wall_secs": 600, "max_retries": 2})
    );
    // `aggregate` は false のとき直列化されない（`skip_serializing_if`）。
    assert!(task.get("aggregate").is_none(), "{task}");

    // 保存された内容も同じ（作成時に解決している）。
    let id = task["id"].as_str().expect("id").to_string();
    let stored = env
        .store
        .get(id.parse().expect("id"))
        .expect("get")
        .expect("stored");
    assert_eq!(serde_json::to_value(&stored).expect("json"), task);

    // 受け入れ 5: 詳細にも `role` が出る。
    let resp = send(&app, get_admin(&format!("/api/v1/tasks/{id}"))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json()["task"]["role"], "lead");
}

/// タスクに書いた値は役割の既定より優先する。`adapter` も役割から入る。`aggregate` はそのまま保存される。
#[tokio::test]
async fn task_values_win_over_role_defaults_and_aggregate_is_stored() {
    let env = env_with_lead_role();
    let app = env.router();

    let body = json!({
        "title": "write the code",
        "objective": "make it work",
        "acceptance": [{"type": "human", "text": "it works"}],
        "role": "implementer",
        "tier": "standard",
        "aggregate": true,
    });
    let resp = send(&app, post_admin("/api/v1/tasks", &body)).await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let task = resp.json();
    assert_eq!(task["role"], "implementer");
    assert_eq!(task["aggregate"], true);
    // タスクの tier が勝ち、adapter と max_wall_secs は役割の既定。
    assert_eq!(
        task["worker_hint"],
        json!({"tier": "standard", "adapter": "fake"})
    );
    assert_eq!(
        task["budget"],
        json!({"max_turns": 10, "max_wall_secs": 900, "max_retries": 2})
    );
}

/// 役割名は自由記述: `[[roles]]` に無い名前でもエラーにせず、名前だけ保存する（既定は全体の既定）。
#[tokio::test]
async fn unknown_role_is_stored_without_defaults() {
    let env = env_with_lead_role();
    let app = env.router();

    let body = json!({
        "title": "research",
        "objective": "read the papers",
        "acceptance": [{"type": "human", "text": "a summary exists"}],
        "role": "researcher",
    });
    let resp = send(&app, post_admin("/api/v1/tasks", &body)).await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let task = resp.json();
    assert_eq!(task["role"], "researcher");
    assert_eq!(
        task["worker_hint"],
        json!({"tier": "standard", "adapter": null})
    );
    assert_eq!(
        task["budget"],
        json!({"max_turns": 10, "max_wall_secs": 600, "max_retries": 2})
    );
}

/// `GET /config` に `roles[]`（`has_instructions` だけ）と `delegation` が出る。指示文の本文は出さない。
#[tokio::test]
async fn config_shows_roles_without_instruction_text_and_delegation_limits() {
    let env = TestEnv::new();
    let app = env.router();

    let resp = send(&app, get_admin("/api/v1/config")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let config = resp.json();
    assert_eq!(
        config["roles"],
        json!([{
            "id": "lead",
            "tier": "frontier",
            "adapter": null,
            "max_turns": 40,
            "max_wall_secs": null,
            "has_instructions": true,
        }])
    );
    assert_eq!(
        config["delegation"],
        // ADR-0021 D4: `on_child_failure` も出す（GUI が「子が失敗したらどうなるか」を説明できるように）。
        json!({"max_delegate_per_run": 8, "max_tree_depth": 5, "max_tree_runs": 100, "on_child_failure": "retry_then_ask"})
    );
    assert!(config["roles"][0].get("instructions").is_none(), "{config}");
    // 指示文の本文はどこにも出ない（`has_instructions` の真偽だけ）。
    assert!(
        !resp.text().contains("You lead"),
        "the instruction text must not be exposed: {}",
        resp.text()
    );
}
