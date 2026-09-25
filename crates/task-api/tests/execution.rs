//! ADR-0072 D14（Phase E2）/ D17（Phase E4/E4b）/ D19（Phase E5）: `POST`/`GET
//! /tasks/{id}/execution-plan`、`GET /tasks/{id}/execution`、`GET /metrics/execution`。
//!
//! 見るもの: 正常系（採用され、WorkUnit が pending/ready に分かれる）、管理系であること
//! （トークン必須）、404（タスクが無い）、422（D14 の検証エラー: 循環・重複・件数・key）、
//! 409（既に active な計画がある）、replan 後の `versions`（Phase E4b 項目4。E4 では
//! `task_ops::execution::replan` と `execution_plan_list` の単体テストでしか確認していなかった
//! HTTP 経路）、`GET /tasks/{id}/execution` の gate・plan・metrics（Phase E5）、
//! `GET /metrics/execution` の集計・フィルタ（Phase E5）。

mod common;

use common::*;
use serde_json::{Value, json};
use task_core::{Event, Status, TaskKind, TaskStore};

fn plan_body() -> Value {
    json!({
        "schema": "celeris.execution-plan/1",
        "rationale": "3 段階の直列計画",
        "work_units": [
            {"key": "a", "kind": "implement", "title": "A", "objective": "do A thoroughly and well"},
            {"key": "b", "kind": "implement", "title": "B", "objective": "do B thoroughly and well", "depends_on": ["a"]},
            {"key": "c", "kind": "implement", "title": "C", "objective": "do C thoroughly and well", "depends_on": ["b"]}
        ]
    })
}

fn g(path: &str) -> axum::http::Request<axum::body::Body> {
    get_with(
        path,
        &[("authorization", format!("Bearer {TOKEN}").as_str())],
    )
}

fn env() -> TestEnv {
    TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..EnvOptions::default()
    })
}

#[tokio::test]
async fn adopting_a_plan_creates_ready_and_pending_work_units() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);

    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &plan_body(),
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["version"], 1);
    assert_eq!(body["origin"], "human");
    assert_eq!(body["status"], "active");
    let work_units = body["work_units"].as_array().expect("work_units");
    assert_eq!(work_units.len(), 3);
    assert_eq!(work_units[0]["key"], "a");
    assert_eq!(work_units[0]["status"], "ready");
    assert_eq!(work_units[1]["key"], "b");
    assert_eq!(work_units[1]["status"], "pending");

    let got = send(
        &app,
        g(&format!("/api/v1/tasks/{}/execution-plan", task.id)),
    )
    .await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_eq!(got.json()["id"], body["id"]);
}

#[tokio::test]
async fn posting_without_a_token_is_unauthorized() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);

    let req = post_json_with(
        &format!("/api/v1/tasks/{}/execution-plan", task.id),
        &plan_body(),
        &[],
    );
    let resp = send(&app, req).await;
    assert_eq!(resp.status, 401, "{}", resp.text());
}

#[tokio::test]
async fn posting_to_an_unknown_task_is_not_found() {
    let env = env();
    let app = env.router();
    let missing = task_core::TaskId::new();
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{missing}/execution-plan"),
            &plan_body(),
        ),
    )
    .await;
    assert_eq!(resp.status, 404, "{}", resp.text());
}

#[tokio::test]
async fn getting_a_task_without_a_plan_is_not_found() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let resp = send(
        &app,
        g(&format!("/api/v1/tasks/{}/execution-plan", task.id)),
    )
    .await;
    assert_eq!(resp.status, 404, "{}", resp.text());
}

#[tokio::test]
async fn a_cyclic_plan_is_rejected_with_422() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let cyclic = json!({
        "schema": "celeris.execution-plan/1",
        "rationale": "bad",
        "work_units": [
            {"key": "a", "kind": "implement", "title": "A", "objective": "do A", "depends_on": ["b"]},
            {"key": "b", "kind": "implement", "title": "B", "objective": "do B", "depends_on": ["a"]}
        ]
    });
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &cyclic,
        ),
    )
    .await;
    assert_eq!(resp.status, 422, "{}", resp.text());
}

#[tokio::test]
async fn a_plan_with_an_unknown_harness_field_is_rejected_and_duplicate_keys_are_rejected() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let duplicate_keys = json!({
        "schema": "celeris.execution-plan/1",
        "rationale": "bad",
        "work_units": [
            {"key": "a", "kind": "implement", "title": "A", "objective": "do A"},
            {"key": "a", "kind": "implement", "title": "A2", "objective": "do A again"}
        ]
    });
    let resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &duplicate_keys,
        ),
    )
    .await;
    assert_eq!(resp.status, 422, "{}", resp.text());

    // `assignee`/`tier`/`model` の欄は `deny_unknown_fields` で JSON の時点で拒否される（400。
    // D14 が要求するのは「schema 違反になる」ことで、拒否そのものは JSON parse の 400 でも
    // 満たされる。ドメインの検証エラー（循環・重複・件数）は 422。
    let mut with_assignee = plan_body();
    with_assignee["work_units"][0]["assignee"] = json!("someone");
    let resp2 = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &with_assignee,
        ),
    )
    .await;
    assert_eq!(resp2.status, 400, "{}", resp2.text());
}

#[tokio::test]
async fn a_second_plan_for_the_same_task_is_rejected_with_409() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);
    let first = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &plan_body(),
        ),
    )
    .await;
    assert_eq!(first.status, 201, "{}", first.text());
    let second = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &plan_body(),
        ),
    )
    .await;
    assert_eq!(second.status, 409, "{}", second.text());
}

/// ADR-0072 D17（Phase E4b 項目4）: replan（`POST` は新規採用専用で 409 を返すので、
/// `task_ops::execution::replan` を dispatcher の `on_planner_finished` と同じ経路で直接呼ぶ）の後、
/// `GET /tasks/{id}/execution-plan` の本体は最新の `active` な版を返し、`versions` に v1
/// （`superseded`）と v2（`active`）が並ぶ。`Event::ExecutionPlanned.supersedes` で v1 の
/// plan_id を指していることも events から確認する（D17「版の履歴が監査できる」）。
#[tokio::test]
async fn getting_a_replanned_task_returns_the_active_version_with_both_versions_listed() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&task);

    let v1_resp = send(
        &app,
        post_admin(
            &format!("/api/v1/tasks/{}/execution-plan", task.id),
            &plan_body(),
        ),
    )
    .await;
    assert_eq!(v1_resp.status, 201, "{}", v1_resp.text());
    let v1 = v1_resp.json();
    let v1_id = v1["id"].as_str().expect("v1 id").to_string();
    assert_eq!(v1["versions"].as_array().expect("versions").len(), 1);

    // v2: `a` は変えず（done ではないのでここでは変えてもよいが、変えない方が現実の replan に近い）、
    // `d` を足す（D17 が挙げる例と同じ形の「追加」）。
    let mut v2_spec = plan_body();
    v2_spec["work_units"].as_array_mut().unwrap().push(json!({
        "key": "d", "kind": "test", "title": "D", "objective": "verify everything end to end",
        "depends_on": ["c"]
    }));
    let spec: task_core::ExecutionPlanSpec =
        serde_json::from_value(v2_spec).expect("valid plan spec");
    let (v2, _diff) = task_ops::execution::replan(
        &env.store,
        task.id,
        spec,
        "replan test".to_string(),
        task_core::PlanOrigin::Planner,
        None,
        task_core::ExecutionLimits::default(),
        time::OffsetDateTime::now_utc(),
    )
    .expect("replan");
    assert_eq!(v2.version, 2);

    let got = send(
        &app,
        g(&format!("/api/v1/tasks/{}/execution-plan", task.id)),
    )
    .await;
    assert_eq!(got.status, 200, "{}", got.text());
    let body = got.json();
    // 本体は最新の active（v2）。
    assert_eq!(body["id"], v2.id);
    assert_eq!(body["version"], 2);
    assert_eq!(body["status"], "active");
    assert_eq!(body["origin"], "planner");
    let work_unit_keys: Vec<&str> = body["work_units"]
        .as_array()
        .expect("work_units")
        .iter()
        .map(|w| w["key"].as_str().unwrap())
        .collect();
    assert!(
        work_unit_keys.contains(&"d"),
        "v2 で足した work unit が本体の work_units にも出る: {work_unit_keys:?}"
    );

    let versions = body["versions"].as_array().expect("versions");
    assert_eq!(versions.len(), 2, "{versions:?}");
    let ver1 = versions
        .iter()
        .find(|v| v["version"] == 1)
        .expect("v1 present");
    assert_eq!(ver1["id"], v1_id);
    assert_eq!(ver1["status"], "superseded");
    assert_eq!(ver1["origin"], "human");
    assert!(
        ver1["superseded_at"].is_string(),
        "superseded_at is set: {ver1:?}"
    );
    let ver2 = versions
        .iter()
        .find(|v| v["version"] == 2)
        .expect("v2 present");
    assert_eq!(ver2["id"], v2.id);
    assert_eq!(ver2["status"], "active");
    assert_eq!(ver2["origin"], "planner");
    assert!(
        ver2["superseded_at"].is_null(),
        "the active version has no superseded_at: {ver2:?}"
    );

    // events: `ExecutionPlanned{version: 2, supersedes: Some(v1_id), ..}` で監査できる。
    let events = env.store.events_for(task.id).expect("events");
    let supersedes_v1 = events.iter().any(|(_, e)| {
        matches!(
            e,
            Event::ExecutionPlanned {
                version: 2,
                supersedes: Some(s),
                ..
            } if s == &v1_id
        )
    });
    assert!(supersedes_v1, "{events:?}");
}

// ---- ADR-0072 D19（Phase E5）: GET /tasks/{id}/execution, GET /metrics/execution ----

fn plan_spec() -> task_core::ExecutionPlanSpec {
    serde_json::from_value(plan_body()).expect("plan spec")
}

fn gate_decision(mode: task_core::ExecutionMode) -> task_core::ExecutionGateDecision {
    task_core::ExecutionGateDecision {
        mode,
        source: task_core::GateSource::Policy,
        score: if mode == task_core::ExecutionMode::Compound {
            6
        } else {
            1
        },
        threshold: 5,
        rule_id: "test/score".to_string(),
        signals: vec![],
        policy_version: task_core::EXECUTION_GATE_POLICY_VERSION.to_string(),
        shadow: false,
    }
}

#[tokio::test]
async fn task_execution_for_an_unknown_task_is_not_found() {
    let env = env();
    let app = env.router();
    let missing = task_core::TaskId::new();
    let resp = send(&app, g(&format!("/api/v1/tasks/{missing}/execution"))).await;
    assert_eq!(resp.status, 404, "{}", resp.text());
}

/// 計画も gate の判定も無いタスクでも 200（metrics は既定値。D20 の「直接実行」に対応）。
#[tokio::test]
async fn task_execution_with_no_activity_returns_zeroed_metrics_and_no_plan() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Done);
    env.seed(&task);

    let resp = send(&app, g(&format!("/api/v1/tasks/{}/execution", task.id))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert!(body["plan"].is_null(), "{body}");
    assert!(body["gate"].is_null(), "{body}");
    assert_eq!(body["metrics"]["work_units_total"], 0);
    assert_eq!(body["runs"].as_array().expect("runs").len(), 0);
}

/// gate と計画のあるタスク: `plan.work_units`・`versions`・`phase` が出る。
#[tokio::test]
async fn task_execution_reports_gate_plan_and_phase() {
    let env = env();
    let app = env.router();
    let mut task = new_task(TaskKind::Execute, Status::Running);
    task.routing = Some(task_core::TaskRouting {
        execution: Some(gate_decision(task_core::ExecutionMode::Compound)),
        ..task_core::TaskRouting::default()
    });
    env.seed_with(
        &task,
        vec![task_core::Event::ExecutionGated {
            decision: Box::new(task.routing.as_ref().unwrap().execution.clone().unwrap()),
        }],
    );
    task_ops::execution::adopt_plan(
        &env.store,
        task.id,
        plan_spec(),
        task_core::PlanOrigin::Fixture,
        None,
        task_core::ExecutionLimits::default(),
        time::OffsetDateTime::now_utc(),
    )
    .expect("adopt_plan");

    let resp = send(&app, g(&format!("/api/v1/tasks/{}/execution", task.id))).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["gate"]["mode"], "compound");
    let plan = &body["plan"];
    assert_eq!(plan["version"], 1);
    let work_units = plan["work_units"].as_array().expect("work_units");
    assert_eq!(work_units.len(), 3);
    let versions = plan["versions"].as_array().expect("versions");
    assert_eq!(versions.len(), 1);
    assert_eq!(body["metrics"]["work_units_total"], 3);
    assert_eq!(body["metrics"]["gate_mode"], "compound");
}

#[tokio::test]
async fn execution_metrics_rejects_an_unknown_group_by() {
    let env = env();
    let app = env.router();
    let resp = send(&app, g("/api/v1/metrics/execution?group_by=bogus")).await;
    assert_eq!(resp.status, 400, "{}", resp.text());
}

#[tokio::test]
async fn execution_metrics_rejects_a_malformed_since() {
    let env = env();
    let app = env.router();
    let resp = send(&app, g("/api/v1/metrics/execution?since=not-a-date")).await;
    assert_eq!(resp.status, 400, "{}", resp.text());
}

/// gate の判定分布（既定の `group_by = gate_mode`）: atomic 1 件・compound 1 件が別グループになる。
#[tokio::test]
async fn execution_metrics_groups_by_gate_mode_by_default() {
    let env = env();
    let app = env.router();

    let mut atomic_task = new_task(TaskKind::Execute, Status::Done);
    atomic_task.routing = Some(task_core::TaskRouting {
        execution: Some(gate_decision(task_core::ExecutionMode::Atomic)),
        ..task_core::TaskRouting::default()
    });
    env.seed(&atomic_task);

    let mut compound_task = new_task(TaskKind::Execute, Status::Failed);
    compound_task.routing = Some(task_core::TaskRouting {
        execution: Some(gate_decision(task_core::ExecutionMode::Compound)),
        ..task_core::TaskRouting::default()
    });
    env.seed(&compound_task);

    let resp = send(&app, g("/api/v1/metrics/execution")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let body = resp.json();
    assert_eq!(body["group_by"], "gate_mode");
    assert_eq!(body["total_tasks"], 2);
    let groups = body["groups"].as_array().expect("groups");
    let atomic = groups
        .iter()
        .find(|g| g["key"] == "atomic")
        .expect("atomic group");
    assert_eq!(atomic["tasks"], 1);
    assert_eq!(atomic["done"], 1);
    let compound = groups
        .iter()
        .find(|g| g["key"] == "compound")
        .expect("compound group");
    assert_eq!(compound["tasks"], 1);
    assert_eq!(compound["failed"], 1);
}

/// `since` は `updated_at` で絞る（未来の `since` なら何も残らない）。
#[tokio::test]
async fn execution_metrics_since_filters_out_tasks_updated_before_it() {
    let env = env();
    let app = env.router();
    let task = new_task(TaskKind::Execute, Status::Done);
    env.seed(&task);

    let future = (time::OffsetDateTime::now_utc() + time::Duration::days(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let resp = send(
        &app,
        g(&format!("/api/v1/metrics/execution?since={future}")),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json()["total_tasks"], 0);
}
