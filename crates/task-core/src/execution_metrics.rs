//! ADR-0072 D19（Phase E5）: Task 単位の実行メトリクス。
//!
//! 純粋なデータ定義と純粋関数だけを置く（I/O・LLM 呼び出しはしない。ADR-0001 D2）。`summarize` は
//! `Task` と、そのタスクの `events`（古い順）だけから決定的に `ExecutionMetrics` を組み立てる。
//!
//! **ADR からの逸脱（`docs/adr/0072-task-execution-decomposition.md` の「Phase E5 実装時の
//! 逸脱・明確化」参照）**: `wall_ms`（D19「最初の dispatch から終端まで」）は、`Event` 自体が
//! タイムスタンプを持たない（`EventRow.ts` は store 層にしかない）ため、`Task.created_at` →
//! `Task.updated_at`（タスクが終端になったときの最後の書き込み）で近似する。`summarize` の
//! signature を `&Task, &[Event]` のまま保つための判断。

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::execution::{BudgetKind, RunEnd};
use crate::execution_gate::ExecutionMode;
use crate::execution_plan::{WorkUnitKind, WorkUnitStatus};
use crate::model::{Event, RunRole, Status, Task};
use crate::quota::{QuotaMethod, QuotaRunRecord, QuotaUse, QuotaWindowUse, aggregate_quota_use};

/// D19: Task 単位の実行メトリクス（`GET /tasks/{id}/execution` と `GET /metrics/execution` の材料）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionMetrics {
    /// D13: Complexity Gate の最終判定（`Task.routing.execution.mode`）。gate が判定していない
    /// Task（`gate = off`・E3 より前・対象外規則）は `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_mode: Option<ExecutionMode>,
    /// `[execution] gate = "shadow"` の判定だった（記録だけで実行には使わなかった）。
    #[serde(default)]
    pub gate_shadow: bool,
    /// このタスクの生涯で一度でも `Event::ExecutionPlanned` を受け取った（計画実行に入った）。
    #[serde(default)]
    pub has_plan: bool,
    /// 生涯で作られた WorkUnit の数（repair を含む。superseded/cancelled も数える）。
    pub work_units_total: u32,
    /// うち `done` になった数。
    pub work_units_done: u32,
    /// role ごとの run 数（`"worker"` / `"reviewer"` / `"planner"`）。
    #[serde(default)]
    pub runs_by_role: BTreeMap<String, u32>,
    /// D11: continuation（予算切れ・yield の続き）の回数（`Trigger::Continue{why: Continue}`）。
    pub continuations: u32,
    /// D7: 予算切れの種類ごとの run 数（`"turns"` / `"wall_clock"` / `"context"`）。
    #[serde(default)]
    pub budget_exhausted_by_kind: BTreeMap<String, u32>,
    /// `budget_exhausted_by_kind["turns"]` の便宜上のコピー（E1 の主要因）。
    pub max_turn_failures: u32,
    /// D11: retry（atomic の `Trigger::WorkerError{retryable:true}` の再試行 + WU の
    /// `work_unit_retry`）の回数。
    pub retries: u32,
    /// D16/ADR-0074 D6.2（Phase F1）: repair の class ごとの回数。`Event::RepairScheduled` から読む
    /// （`"planner"` は replan が自ら書いた `kind = repair` の WU）。この Event が無い旧いタスク
    /// （F1 より前に起きた repair）だけ、`ExecutionPlanned` の title の接頭辞から復元する
    /// フォールバックに倒れ、それでも復元できなければ `"unknown"`（events だけからの復元の
    /// 既知の限界。ADR-0072 の逸脱節を参照）。F1 以降に起きた repair はすべて `RepairScheduled` を
    /// 持つので `unknown` は出ない。
    #[serde(default)]
    pub repairs_by_class: BTreeMap<String, u32>,
    /// repair の総数。
    pub repairs_total: u32,
    /// D17: replan（`Event::ExecutionPlanned.supersedes.is_some()`）の回数。
    pub replans: u32,
    /// D19: この Task の run の中で観測した context 量の最大値。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_input_tokens: Option<u64>,
    /// 観測できた cached input tokens の合計。未報告の run は 0 と見なさず、全 run で未報告なら None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// `Task.created_at` から `Task.updated_at` まで（ミリ秒）。終端でなければ `None`
    /// （まだ進行中で終わりの時刻が無い）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
    pub final_status: Status,
    /// ADR-0074 D4.3（Phase F3 quota）: アカウント × 窓ごとの quota 消費の合計
    /// （`Event::QuotaEstimated` から。同じ `run_id` は最後の Event が有効）。
    #[serde(default)]
    pub quota: Vec<QuotaUse>,
    /// D4.3: quota が `unknown`（measured/apportioned/estimated のいずれでも決められなかった）
    /// だった run の数。
    #[serde(default)]
    pub quota_unknown_runs: u32,
    /// D4.3: 定価 USD（`cost_usd`）が完全か。単価不明のモデルを使った run（token はあるのに
    /// `Usage.cost_usd` が無い run）が 1 件でもあれば `false`（`cost_usd` はその分だけ過小）。
    /// run が 1 件も無ければ `true`（欠けようがない）。
    #[serde(default = "default_true")]
    pub cost_usd_complete: bool,
}

fn default_true() -> bool {
    true
}

/// `work_units.spec.title` の `"repair (<bucket>): …"` から bucket 名を読む
/// （`task_dispatch::Dispatcher::repair_bucket_of_title` と同じ字句規則。決定的な文字列解析のみ）。
fn repair_bucket_of_title(title: &str) -> Option<&str> {
    title.strip_prefix("repair (")?.split(')').next()
}

/// ADR-0074 D4.3（Phase F3 quota）: `Event::QuotaEstimated` 1 件分の、所有権を持つコピー
/// （run_id ごとに「最後の Event が有効」で畳み込むための中間表現）。
#[derive(Debug, Clone)]
struct QuotaRunSnapshot {
    work_unit_id: Option<String>,
    source: String,
    account: Option<String>,
    windows: Vec<QuotaWindowUse>,
}

/// D4.3: events から `Event::QuotaEstimated` を run_id ごとに畳み込む（同じ `run_id` は
/// 後から来た Event で上書き。`events` は古い順という契約なので、これで「最後の Event が有効」になる）。
fn latest_quota_by_run(events: &[Event]) -> BTreeMap<String, QuotaRunSnapshot> {
    let mut by_run: BTreeMap<String, QuotaRunSnapshot> = BTreeMap::new();
    for event in events {
        if let Event::QuotaEstimated {
            run_id,
            work_unit_id,
            source,
            account,
            windows,
            ..
        } = event
        {
            by_run.insert(
                run_id.clone(),
                QuotaRunSnapshot {
                    work_unit_id: work_unit_id.clone(),
                    source: source.clone(),
                    account: account.clone(),
                    windows: windows.clone(),
                },
            );
        }
    }
    by_run
}

/// ADR-0074 D4.3（Phase F3）: WU ごとの quota 消費（`TaskExecutionView` の WU に足す）。
/// `work_unit_id` が無い run（atomic/暗黙の WorkUnit）は `None` キーにまとめる。
pub fn group_quota_by_work_unit(events: &[Event]) -> BTreeMap<Option<String>, Vec<QuotaUse>> {
    let by_run = latest_quota_by_run(events);
    let mut by_wu: BTreeMap<Option<String>, Vec<QuotaRunRecord<'_>>> = BTreeMap::new();
    for snapshot in by_run.values() {
        by_wu
            .entry(snapshot.work_unit_id.clone())
            .or_default()
            .push(QuotaRunRecord {
                source: &snapshot.source,
                account: snapshot.account.as_deref(),
                windows: &snapshot.windows,
            });
    }
    by_wu
        .into_iter()
        .map(|(key, records)| (key, aggregate_quota_use(records)))
        .collect()
}

/// D19: `Task` と、そのタスクの events（古い順）から `ExecutionMetrics` を組み立てる純粋関数。
pub fn summarize(task: &Task, events: &[Event]) -> ExecutionMetrics {
    let gate = task.routing.as_ref().and_then(|r| r.execution.as_ref());
    let gate_mode = gate.map(|d| d.mode);
    let gate_shadow = gate.map(|d| d.shadow).unwrap_or(false);

    let mut has_plan = false;
    let mut keys_seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut status_by_key: BTreeMap<String, WorkUnitStatus> = BTreeMap::new();
    let mut repair_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut repair_bucket_by_key: BTreeMap<String, String> = BTreeMap::new();

    let mut runs_by_role: BTreeMap<String, u32> = BTreeMap::new();
    let mut continuations: u32 = 0;
    let mut retries: u32 = 0;
    let mut replans: u32 = 0;
    let mut budget_exhausted_by_kind: BTreeMap<String, u32> = BTreeMap::new();
    let mut peak_context_tokens: Option<u64> = None;
    let mut total_input_tokens: Option<u64> = None;
    let mut total_cache_read_tokens: Option<u64> = None;
    let mut total_output_tokens: Option<u64> = None;
    let mut cost_usd: Option<f64> = None;
    // ADR-0074 D4.3（Phase F3 quota）: token を持つのに `cost_usd` が無い run が 1 件でもあれば false。
    let mut cost_usd_complete = true;

    for event in events {
        match event {
            Event::ExecutionPlanned {
                plan, supersedes, ..
            } => {
                has_plan = true;
                if supersedes.is_some() {
                    replans += 1;
                }
                for wu in &plan.work_units {
                    keys_seen.insert(wu.key.clone());
                    status_by_key
                        .entry(wu.key.clone())
                        .or_insert(if wu.depends_on.is_empty() {
                            WorkUnitStatus::Ready
                        } else {
                            WorkUnitStatus::Pending
                        });
                    if wu.kind == WorkUnitKind::Repair {
                        repair_keys.insert(wu.key.clone());
                        if let Some(bucket) = repair_bucket_of_title(&wu.title) {
                            repair_bucket_by_key.insert(wu.key.clone(), bucket.to_string());
                        }
                    }
                }
            }
            Event::WorkUnitTransitioned {
                key, to, reason, ..
            } => {
                keys_seen.insert(key.clone());
                status_by_key.insert(key.clone(), *to);
                if reason == "review_repair" {
                    repair_keys.insert(key.clone());
                }
            }
            // ADR-0074 D6.2/§6 F1 (i)（Phase F1）: repair の class はこの Event から読む
            // （title の接頭辞の復元より優先。`unknown` を無くす）。
            Event::RepairScheduled { key, class, .. } => {
                repair_keys.insert(key.clone());
                repair_bucket_by_key.insert(key.clone(), class.clone());
            }
            Event::WorkerStarted { role, .. } => {
                let name = match role {
                    None | Some(RunRole::Worker) => "worker",
                    Some(RunRole::Reviewer) => "reviewer",
                    Some(RunRole::Planner) => "planner",
                };
                *runs_by_role.entry(name.to_string()).or_insert(0) += 1;
            }
            Event::WorkerFinished {
                usage,
                metrics,
                end,
                ..
            } => {
                if let Some(u) = usage {
                    if let Some(v) = u.input_tokens {
                        total_input_tokens = Some(total_input_tokens.unwrap_or(0) + v);
                    }
                    if let Some(v) = u.cache_read_tokens {
                        total_cache_read_tokens = Some(total_cache_read_tokens.unwrap_or(0) + v);
                    }
                    if let Some(v) = u.output_tokens {
                        total_output_tokens = Some(total_output_tokens.unwrap_or(0) + v);
                    }
                    if let Some(v) = u.cost_usd {
                        cost_usd = Some(cost_usd.unwrap_or(0.0) + v);
                    } else if u.input_tokens.is_some()
                        || u.output_tokens.is_some()
                        || u.cache_read_tokens.is_some()
                        || u.cache_creation_tokens.is_some()
                    {
                        // ADR-0074 D4.3（Phase F3 quota）: token はあるのに単価が無い
                        // （単価表に無いモデル。E6 report 問題 5）。
                        cost_usd_complete = false;
                    }
                }
                if let Some(m) = metrics
                    && let Some(p) = m.peak_context_tokens
                {
                    peak_context_tokens = Some(peak_context_tokens.map_or(p, |cur| cur.max(p)));
                }
                if let Some(RunEnd::BudgetExhausted { kind }) = end {
                    let name = match kind {
                        BudgetKind::Turns => "turns",
                        BudgetKind::WallClock => "wall_clock",
                        BudgetKind::Context => "context",
                    };
                    *budget_exhausted_by_kind
                        .entry(name.to_string())
                        .or_insert(0) += 1;
                }
            }
            Event::Transitioned { reason, to, .. } => match reason.as_str() {
                "continue" => continuations += 1,
                "work_unit_retry" => retries += 1,
                "worker_error" if *to == Status::Ready => retries += 1,
                _ => {}
            },
            _ => {}
        }
    }

    let work_units_total = keys_seen.len() as u32;
    let work_units_done = status_by_key
        .values()
        .filter(|s| **s == WorkUnitStatus::Done)
        .count() as u32;

    let mut repairs_by_class: BTreeMap<String, u32> = BTreeMap::new();
    for key in &repair_keys {
        let bucket = repair_bucket_by_key
            .get(key)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        *repairs_by_class.entry(bucket).or_insert(0) += 1;
    }
    let repairs_total = repair_keys.len() as u32;
    let max_turn_failures = budget_exhausted_by_kind.get("turns").copied().unwrap_or(0);

    let wall_ms = if task.status.is_terminal() {
        let dur = task.updated_at - task.created_at;
        Some(dur.whole_milliseconds().max(0) as u64)
    } else {
        None
    };

    // ADR-0074 D4.3（Phase F3 quota）: `Event::QuotaEstimated` は「同じ run_id は最後の Event が
    // 有効」（apportioned の再送）なので、専用の畳み込みを 1 回行ってから集計する。
    let quota_by_run = latest_quota_by_run(events);
    let quota_unknown_runs = quota_by_run
        .values()
        .filter(|s| crate::quota::representative_method(&s.windows) == QuotaMethod::Unknown)
        .count() as u32;
    let quota = aggregate_quota_use(quota_by_run.values().map(|s| QuotaRunRecord {
        source: &s.source,
        account: s.account.as_deref(),
        windows: &s.windows,
    }));

    ExecutionMetrics {
        gate_mode,
        gate_shadow,
        has_plan,
        work_units_total,
        work_units_done,
        runs_by_role,
        continuations,
        budget_exhausted_by_kind,
        max_turn_failures,
        retries,
        repairs_by_class,
        repairs_total,
        replans,
        peak_context_tokens,
        total_input_tokens,
        total_cache_read_tokens,
        total_output_tokens,
        cost_usd,
        wall_ms,
        final_status: task.status,
        quota,
        quota_unknown_runs,
        cost_usd_complete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_gate::{ExecutionGateDecision, GateSource};
    use crate::execution_plan::PlanOrigin;
    use crate::execution_plan::{
        EXECUTION_PLAN_SCHEMA, ExecutionPlanSpec, WorkUnitContext, WorkUnitSpec,
    };
    use crate::model::{TaskRouting, Usage};
    use crate::model_policy::tests::task as sample_task;

    fn wu(key: &str, kind: WorkUnitKind, title: &str, depends_on: &[&str]) -> WorkUnitSpec {
        WorkUnitSpec {
            key: key.to_string(),
            kind,
            title: title.to_string(),
            objective: format!("objective for {key}"),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            done_when: vec![],
            checks: vec![],
            context: WorkUnitContext::default(),
            harness: None,
            features: None,
            budget: None,
            outputs: vec![],
            phase: None,
        }
    }

    #[test]
    fn empty_events_give_all_zero_defaults() {
        let task = sample_task("x", vec![]);
        let m = summarize(&task, &[]);
        assert_eq!(m.gate_mode, None);
        assert!(!m.has_plan);
        assert_eq!(m.work_units_total, 0);
        assert_eq!(m.continuations, 0);
        assert_eq!(m.repairs_total, 0);
        assert_eq!(m.replans, 0);
        assert_eq!(m.final_status, task.status);
    }

    #[test]
    fn gate_decision_on_the_task_is_reflected() {
        let mut task = sample_task("x", vec![]);
        task.routing = Some(TaskRouting {
            execution: Some(ExecutionGateDecision {
                mode: ExecutionMode::Compound,
                source: GateSource::Policy,
                score: 6,
                threshold: 5,
                rule_id: "compound/score".to_string(),
                signals: vec![],
                policy_version: "exec-gate/1".to_string(),
                shadow: true,
            }),
            ..TaskRouting::default()
        });
        let m = summarize(&task, &[]);
        assert_eq!(m.gate_mode, Some(ExecutionMode::Compound));
        assert!(m.gate_shadow);
    }

    #[test]
    fn continuations_are_counted_from_the_continue_reason() {
        let task = sample_task("x", vec![]);
        let events = vec![
            Event::Transitioned {
                from: Status::Running,
                to: Status::Ready,
                reason: "continue".to_string(),
            },
            Event::Transitioned {
                from: Status::Running,
                to: Status::Ready,
                reason: "continue".to_string(),
            },
            Event::Transitioned {
                from: Status::Running,
                to: Status::Ready,
                reason: "worker_error".to_string(),
            },
        ];
        let m = summarize(&task, &events);
        assert_eq!(m.continuations, 2);
        assert_eq!(m.retries, 1);
    }

    #[test]
    fn budget_exhausted_is_grouped_by_kind_and_turns_is_mirrored() {
        let task = sample_task("x", vec![]);
        let fin = |kind: BudgetKind| Event::WorkerFinished {
            run_id: "r".to_string(),
            outcome: "continue: budget".to_string(),
            usage: None,
            role: None,
            metrics: None,
            end: Some(RunEnd::BudgetExhausted { kind }),
        };
        let events = vec![
            fin(BudgetKind::Turns),
            fin(BudgetKind::Turns),
            fin(BudgetKind::WallClock),
        ];
        let m = summarize(&task, &events);
        assert_eq!(m.budget_exhausted_by_kind.get("turns"), Some(&2));
        assert_eq!(m.budget_exhausted_by_kind.get("wall_clock"), Some(&1));
        assert_eq!(m.max_turn_failures, 2);
    }

    #[test]
    fn work_units_and_replans_are_derived_from_execution_planned_and_transitions() {
        let task = sample_task("x", vec![]);
        let plan1 = ExecutionPlanSpec {
            schema: EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "r".to_string(),
            work_units: vec![
                wu("a", WorkUnitKind::Implement, "a", &[]),
                wu("b", WorkUnitKind::Implement, "b", &["a"]),
            ],
            phases: Vec::new(),
            children: Vec::new(),
        };
        let plan2 = ExecutionPlanSpec {
            schema: EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "r".to_string(),
            work_units: vec![
                wu("a", WorkUnitKind::Implement, "a", &[]),
                wu("b", WorkUnitKind::Implement, "b", &["a"]),
                wu("c", WorkUnitKind::Implement, "c", &["b"]),
            ],
            phases: Vec::new(),
            children: Vec::new(),
        };
        let events = vec![
            Event::ExecutionPlanned {
                plan_id: "p1".to_string(),
                version: 1,
                origin: PlanOrigin::Planner,
                supersedes: None,
                reason: None,
                plan: Box::new(plan1),
            },
            Event::WorkUnitTransitioned {
                work_unit_id: "wu-a".to_string(),
                key: "a".to_string(),
                from: WorkUnitStatus::Ready,
                to: WorkUnitStatus::Done,
                reason: "completed".to_string(),
                run_id: Some("r1".to_string()),
            },
            Event::ExecutionPlanned {
                plan_id: "p2".to_string(),
                version: 2,
                origin: PlanOrigin::Human,
                supersedes: Some("p1".to_string()),
                reason: Some("replan".to_string()),
                plan: Box::new(plan2),
            },
        ];
        let m = summarize(&task, &events);
        assert!(m.has_plan);
        assert_eq!(m.work_units_total, 3, "{m:?}");
        assert_eq!(m.work_units_done, 1, "{m:?}");
        assert_eq!(m.replans, 1);
    }

    /// atomic 化のときに `ExecutionPlanned` に repair WU のタイトルごと載る経路（D16 の repair 分類が
    /// events から復元できる）。
    #[test]
    fn repairs_are_classified_from_the_execution_planned_spec_when_available() {
        let task = sample_task("x", vec![]);
        let plan = ExecutionPlanSpec {
            schema: EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "reviewer repair".to_string(),
            work_units: vec![
                wu("main", WorkUnitKind::Implement, "main", &[]),
                wu(
                    "repair-1",
                    WorkUnitKind::Repair,
                    "repair (format): 修復",
                    &[],
                ),
            ],
            phases: Vec::new(),
            children: Vec::new(),
        };
        let events = vec![Event::ExecutionPlanned {
            plan_id: "p1".to_string(),
            version: 1,
            origin: PlanOrigin::Repair,
            supersedes: None,
            reason: Some("review_repair".to_string()),
            plan: Box::new(plan),
        }];
        let m = summarize(&task, &events);
        assert_eq!(m.repairs_total, 1);
        assert_eq!(m.repairs_by_class.get("format"), Some(&1));
    }

    /// 既に計画のある Task に足された repair WU は `WorkUnitTransitioned` だけで作られ（D16）、
    /// class は events から復元できないので `"unknown"` に落ちる（既知の限界。ADR の逸脱節参照）。
    #[test]
    fn repairs_without_a_recoverable_title_fall_back_to_unknown() {
        let task = sample_task("x", vec![]);
        let events = vec![Event::WorkUnitTransitioned {
            work_unit_id: "wu-repair-1".to_string(),
            key: "repair-1".to_string(),
            from: WorkUnitStatus::Pending,
            to: WorkUnitStatus::Ready,
            reason: "review_repair".to_string(),
            run_id: None,
        }];
        let m = summarize(&task, &events);
        assert_eq!(m.repairs_total, 1);
        assert_eq!(m.repairs_by_class.get("unknown"), Some(&1));
    }

    /// ADR-0074 D6.2/§6 F1 (i)（Phase F1）: `RepairScheduled` があれば、title の接頭辞を復元しなくても
    /// class が分かる（`unknown` にならない）。
    #[test]
    fn repair_scheduled_event_names_the_class() {
        let task = sample_task("x", vec![]);
        let events = vec![
            Event::WorkUnitTransitioned {
                work_unit_id: "wu-repair-1".to_string(),
                key: "repair-1".to_string(),
                from: WorkUnitStatus::Pending,
                to: WorkUnitStatus::Ready,
                reason: "review_repair".to_string(),
                run_id: None,
            },
            Event::RepairScheduled {
                work_unit_id: "wu-repair-1".to_string(),
                key: "repair-1".to_string(),
                class: "review_timeout".to_string(),
                origin: crate::execution::RepairOrigin::Review,
            },
        ];
        let m = summarize(&task, &events);
        assert_eq!(m.repairs_total, 1);
        assert_eq!(m.repairs_by_class.get("review_timeout"), Some(&1));
        assert!(!m.repairs_by_class.contains_key("unknown"), "{m:?}");
    }

    /// planner が replan で自ら書いた repair WU は class `"planner"`（`unknown` にならない）。
    #[test]
    fn planner_authored_repair_work_units_are_classified_as_planner() {
        let task = sample_task("x", vec![]);
        let plan = ExecutionPlanSpec {
            schema: EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "r".to_string(),
            work_units: vec![wu(
                "repair-1",
                WorkUnitKind::Repair,
                "fix the thing directly (no title convention)",
                &[],
            )],
            phases: Vec::new(),
            children: Vec::new(),
        };
        let events = vec![
            Event::ExecutionPlanned {
                plan_id: "p1".to_string(),
                version: 2,
                origin: PlanOrigin::Planner,
                supersedes: Some("p0".to_string()),
                reason: Some("replan (planner run)".to_string()),
                plan: Box::new(plan),
            },
            Event::RepairScheduled {
                work_unit_id: "wu-repair-1".to_string(),
                key: "repair-1".to_string(),
                class: "planner".to_string(),
                origin: crate::execution::RepairOrigin::Planner,
            },
        ];
        let m = summarize(&task, &events);
        assert_eq!(m.repairs_by_class.get("planner"), Some(&1), "{m:?}");
        assert!(!m.repairs_by_class.contains_key("unknown"), "{m:?}");
    }

    #[test]
    fn tokens_cost_and_peak_context_are_summed_and_maxed() {
        let task = sample_task("x", vec![]);
        let fin = |input: u64, output: u64, cost: f64, peak: u64| Event::WorkerFinished {
            run_id: "r".to_string(),
            outcome: "done: ok".to_string(),
            usage: Some(Usage {
                input_tokens: Some(input),
                output_tokens: Some(output),
                cache_read_tokens: Some(input / 2),
                cache_creation_tokens: None,
                cost_usd: Some(cost),
            }),
            role: None,
            metrics: Some(crate::model::RunMetrics {
                wall_ms: 1000,
                retries: 0,
                peak_context_tokens: Some(peak),
                turns: Some(3),
            }),
            end: Some(RunEnd::Completed),
        };
        let events = vec![fin(100, 20, 0.5, 500), fin(200, 40, 0.25, 900)];
        let m = summarize(&task, &events);
        assert_eq!(m.total_input_tokens, Some(300));
        assert_eq!(m.total_cache_read_tokens, Some(150));
        assert_eq!(m.total_output_tokens, Some(60));
        assert_eq!(m.cost_usd, Some(0.75));
        assert_eq!(m.peak_context_tokens, Some(900));
    }

    #[test]
    fn runs_by_role_counts_worker_reviewer_and_planner() {
        let task = sample_task("x", vec![]);
        let started = |role: Option<RunRole>| Event::WorkerStarted {
            run_id: "r".to_string(),
            adapter: "fake".to_string(),
            model: "m".to_string(),
            provider: None,
            account: None,
            role,
            task_role: None,
        };
        let events = vec![
            started(None),
            started(Some(RunRole::Reviewer)),
            started(Some(RunRole::Planner)),
            started(Some(RunRole::Planner)),
        ];
        let m = summarize(&task, &events);
        assert_eq!(m.runs_by_role.get("worker"), Some(&1));
        assert_eq!(m.runs_by_role.get("reviewer"), Some(&1));
        assert_eq!(m.runs_by_role.get("planner"), Some(&2));
    }

    #[test]
    fn wall_ms_is_only_present_for_terminal_tasks() {
        let mut task = sample_task("x", vec![]);
        let m = summarize(&task, &[]);
        assert_eq!(m.wall_ms, None, "Draft はまだ終端ではない");
        task.status = Status::Done;
        task.updated_at = task.created_at + time::Duration::seconds(30);
        let m = summarize(&task, &[]);
        assert_eq!(m.wall_ms, Some(30_000));
    }

    // ---- ADR-0074 D4.3（Phase F3 quota）----

    fn quota_event(
        run_id: &str,
        work_unit_id: Option<&str>,
        source: &str,
        account: Option<&str>,
        windows: Vec<crate::quota::QuotaWindowUse>,
    ) -> Event {
        let weighted_tokens = 100.0;
        Event::QuotaEstimated {
            run_id: run_id.to_string(),
            work_unit_id: work_unit_id.map(str::to_string),
            source: source.to_string(),
            account: account.map(str::to_string),
            method: crate::quota::representative_method(&windows),
            windows,
            weighted_tokens,
            calibration: None,
            weights_version: crate::quota::WEIGHTS_VERSION.to_string(),
            list_price_usd: None,
        }
    }

    fn measured_window(
        window: crate::quota::QuotaWindow,
        used_pct: f64,
    ) -> crate::quota::QuotaWindowUse {
        crate::quota::QuotaWindowUse {
            window,
            before: Some(0.1),
            after: Some(0.1 + used_pct / 100.0),
            resets_at: Some(5_000),
            used_pct: Some(used_pct),
            method: QuotaMethod::Measured,
        }
    }

    #[test]
    fn cost_usd_complete_is_false_with_an_unpriced_model() {
        let task = sample_task("x", vec![]);
        let priced = Event::WorkerFinished {
            run_id: "r1".to_string(),
            outcome: "done: ok".to_string(),
            usage: Some(Usage {
                input_tokens: Some(100),
                output_tokens: Some(10),
                cache_read_tokens: None,
                cache_creation_tokens: None,
                cost_usd: Some(1.0),
            }),
            role: None,
            metrics: None,
            end: Some(RunEnd::Completed),
        };
        let unpriced = Event::WorkerFinished {
            run_id: "r2".to_string(),
            outcome: "done: ok".to_string(),
            usage: Some(Usage {
                input_tokens: Some(1_000_000),
                output_tokens: Some(200_000),
                cache_read_tokens: None,
                cache_creation_tokens: None,
                cost_usd: None, // 単価表に無いモデル（例: gpt-6-sol）
            }),
            role: None,
            metrics: None,
            end: Some(RunEnd::Completed),
        };
        let m = summarize(&task, std::slice::from_ref(&priced));
        assert!(m.cost_usd_complete, "priced-only は complete");

        let m = summarize(&task, &[priced, unpriced]);
        assert!(!m.cost_usd_complete, "unpriced な run が混じれば false");
        assert_eq!(
            m.cost_usd,
            Some(1.0),
            "cost_usd 自体は priced 分だけ合計する"
        );
    }

    #[test]
    fn cost_usd_complete_defaults_true_with_no_runs() {
        let task = sample_task("x", vec![]);
        let m = summarize(&task, &[]);
        assert!(m.cost_usd_complete);
    }

    #[test]
    fn quota_is_aggregated_by_source_account_and_window() {
        let task = sample_task("x", vec![]);
        let events = vec![
            quota_event(
                "r1",
                None,
                "claude-oauth",
                Some("a"),
                vec![measured_window(crate::quota::QuotaWindow::FiveHour, 4.0)],
            ),
            quota_event(
                "r2",
                None,
                "claude-oauth",
                Some("a"),
                vec![measured_window(crate::quota::QuotaWindow::FiveHour, 6.0)],
            ),
        ];
        let m = summarize(&task, &events);
        assert_eq!(m.quota.len(), 1, "{:?}", m.quota);
        let row = &m.quota[0];
        assert_eq!(row.source, "claude-oauth");
        assert_eq!(row.account.as_deref(), Some("a"));
        assert_eq!(row.runs, 2);
        assert_eq!(row.used_pct, Some(10.0));
        assert_eq!(m.quota_unknown_runs, 0);
    }

    /// D4.3: `apportioned` は同じ `run_id` にもう 1 件出ることがある（グループが閉じたとき）。
    /// 「最後の Event が有効」なので、`unknown`（先の暫定値）は上書きされて消える。
    #[test]
    fn quota_estimated_reemission_for_the_same_run_id_keeps_only_the_last_event() {
        let task = sample_task("x", vec![]);
        let pending = crate::quota::QuotaWindowUse {
            window: crate::quota::QuotaWindow::FiveHour,
            before: None,
            after: None,
            resets_at: None,
            used_pct: None,
            method: QuotaMethod::Unknown,
        };
        let resolved = measured_window(crate::quota::QuotaWindow::FiveHour, 9.0);
        let events = vec![
            quota_event("r1", None, "claude-oauth", Some("a"), vec![pending]),
            quota_event("r1", None, "claude-oauth", Some("a"), vec![resolved]),
        ];
        let m = summarize(&task, &events);
        assert_eq!(m.quota_unknown_runs, 0, "{:?}", m.quota);
        assert_eq!(m.quota[0].used_pct, Some(9.0));
        assert_eq!(
            m.quota[0].runs, 1,
            "1 回だけ数える（同じ run_id は畳み込む）"
        );
    }

    #[test]
    fn quota_unknown_runs_counts_runs_that_never_resolved() {
        let task = sample_task("x", vec![]);
        let unknown_window = crate::quota::QuotaWindowUse {
            window: crate::quota::QuotaWindow::FiveHour,
            before: None,
            after: None,
            resets_at: None,
            used_pct: None,
            method: QuotaMethod::Unknown,
        };
        let events = vec![quota_event(
            "r1",
            Some("wu-a"),
            "codex-oauth",
            Some("b"),
            vec![unknown_window],
        )];
        let m = summarize(&task, &events);
        assert_eq!(m.quota_unknown_runs, 1);
        assert_eq!(m.quota[0].used_pct, None, "unknown は 0 ではない");
    }

    #[test]
    fn group_quota_by_work_unit_splits_atomic_and_work_unit_runs() {
        let events = vec![
            quota_event(
                "r1",
                None,
                "claude-oauth",
                Some("a"),
                vec![measured_window(crate::quota::QuotaWindow::FiveHour, 2.0)],
            ),
            quota_event(
                "r2",
                Some("wu-a"),
                "claude-oauth",
                Some("a"),
                vec![measured_window(crate::quota::QuotaWindow::FiveHour, 3.0)],
            ),
        ];
        let by_wu = group_quota_by_work_unit(&events);
        assert_eq!(by_wu.get(&None).map(|v| v[0].used_pct), Some(Some(2.0)));
        assert_eq!(
            by_wu.get(&Some("wu-a".to_string())).map(|v| v[0].used_pct),
            Some(Some(3.0))
        );
    }
}
