//! ADR-0069 D5（Phase 114）: run ごとの routing の監査記録（担当・harness・lane・model・features・規則・
//! policy の版）と、Phase 104 のメトリクス（cost・tokens・wall time・retries）とレビュー結果を
//! 突き合わせる純粋関数。イベントは古い順に渡す（`TaskStore::events_for` の順）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{Event, RunRole, Task, TaskId, Tier};
use crate::model_policy::TaskFeatures;

/// レビューの結果（その run の後の `review_pass` / `review_fail`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewResult {
    pub passed: bool,
    /// 不合格だった受け入れ条件の番号。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_criteria: Vec<usize>,
}

/// ワーカー run 1 件の routing の監査。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RoutingAudit {
    pub task_id: TaskId,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<Tier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<TaskFeatures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewResult>,
}

/// タスク 1 件のイベント列から、ワーカー run ごとの監査を作る（古い run が先）。
pub fn routing_audit(task: &Task, events: &[Event]) -> Vec<RoutingAudit> {
    let mut out: Vec<RoutingAudit> = Vec::new();
    // レビューの判定を最後のワーカー run に帰属させる。
    let mut last_worker: Option<usize> = None;
    let mut failed: Vec<usize> = Vec::new();
    let find = |out: &mut Vec<RoutingAudit>, run_id: &str| -> usize {
        match out.iter().position(|a| a.run_id == run_id) {
            Some(i) => i,
            None => {
                out.push(RoutingAudit {
                    task_id: task.id,
                    run_id: run_id.to_string(),
                    org_node: task.assignee.clone(),
                    harness: task.genre.clone(),
                    ..RoutingAudit::default()
                });
                out.len() - 1
            }
        }
    };
    for event in events {
        match event {
            Event::WorkerStarted {
                run_id,
                adapter,
                model,
                provider,
                account,
                role: None,
                ..
            } => {
                let i = find(&mut out, run_id);
                let a = &mut out[i];
                a.adapter = Some(adapter.clone());
                if !model.is_empty() {
                    a.model = Some(model.clone());
                }
                a.provider = provider.clone();
                a.account = account.clone();
                last_worker = Some(i);
                failed.clear();
            }
            Event::RoutingDecided { run_id, record } => {
                let i = find(&mut out, run_id);
                let a = &mut out[i];
                if record.org_node.is_some() {
                    a.org_node = record.org_node.clone();
                }
                if record.harness.is_some() {
                    a.harness = record.harness.clone();
                }
                a.lane = Some(record.resolution.lane.unwrap_or(record.decision.lane));
                if !record.resolution.model_id.is_empty() {
                    a.model = Some(record.resolution.model_id.clone());
                }
                a.reasoning_effort = record.resolution.reasoning_effort.clone();
                a.features = Some(record.decision.features);
                a.rule_id = Some(record.decision.rule_id.clone());
                a.policy_version = Some(record.decision.policy_version.clone());
                a.reasons = record.decision.reasons.clone();
                a.escalation = record.decision.escalation.clone();
            }
            Event::WorkerFinished {
                run_id,
                outcome,
                usage,
                role,
                metrics,
                ..
            } if *role != Some(RunRole::Reviewer) => {
                let i = find(&mut out, run_id);
                let a = &mut out[i];
                a.outcome = Some(outcome.clone());
                if let Some(u) = usage {
                    a.cost_usd = u.cost_usd;
                    a.input_tokens = u.input_tokens;
                    a.output_tokens = u.output_tokens;
                }
                if let Some(m) = metrics {
                    a.wall_ms = Some(m.wall_ms);
                    a.retries = Some(m.retries);
                }
            }
            Event::ReviewVerdict {
                criterion_idx,
                pass: false,
                ..
            } => failed.push(*criterion_idx),
            Event::Transitioned { reason, .. }
                if reason == "review_pass" || reason == "review_fail" =>
            {
                if let Some(i) = last_worker {
                    out[i].review = Some(ReviewResult {
                        passed: reason == "review_pass",
                        failed_criteria: std::mem::take(&mut failed),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RunMetrics, Status, TaskRouting, TierSource, Usage};
    use crate::model_policy::{LaneCeiling, decide_for_task};
    use crate::model_routing::LaneResolution;

    #[test]
    fn joins_routing_usage_wall_retries_and_review_per_run() {
        let mut t = crate::model_policy::tests::task("crates/x.rs の typo を直す", vec![]);
        t.assignee = Some("software-engineering".into());
        t.routing = Some(TaskRouting {
            tier_source: TierSource::Default,
            ..TaskRouting::default()
        });
        let decision = decide_for_task(&t, &LaneCeiling::default()).unwrap();
        let record = crate::model_policy::RoutingRecord {
            org_node: Some("software-engineering".into()),
            harness: Some("coding".into()),
            decision: decision.clone(),
            resolution: LaneResolution {
                lane: Some(Tier::Standard),
                adapter: "claude-code".into(),
                provider: Some("cc-1".into()),
                account: None,
                model_id: "model-std".into(),
                reasoning_effort: Some("medium".into()),
            },
            quota_reason: None,
        };
        let events = vec![
            Event::WorkerStarted {
                run_id: "r1".into(),
                adapter: "claude-code".into(),
                model: "model-std".into(),
                provider: Some("cc-1".into()),
                account: None,
                role: None,
                task_role: None,
            },
            Event::RoutingDecided {
                run_id: "r1".into(),
                record: Box::new(record),
            },
            Event::WorkerFinished {
                run_id: "r1".into(),
                outcome: "done: ok".into(),
                usage: Some(Usage {
                    input_tokens: Some(100),
                    output_tokens: Some(20),
                    cost_usd: Some(0.5),
                    ..Usage::default()
                }),
                role: None,
                metrics: Some(RunMetrics {
                    wall_ms: 1234,
                    retries: 1,
                    peak_context_tokens: None,
                    turns: None,
                }),
                end: None,
            },
            // reviewer run の WorkerFinished は数えない
            Event::WorkerFinished {
                run_id: "rev".into(),
                outcome: "done: judged".into(),
                usage: None,
                role: Some(RunRole::Reviewer),
                metrics: None,
                end: None,
            },
            Event::ReviewVerdict {
                run_id: "rev".into(),
                criterion_idx: 1,
                pass: false,
                reason: "missing".into(),
            },
            Event::Transitioned {
                from: Status::Reviewing,
                to: Status::Ready,
                reason: "review_fail".into(),
            },
        ];
        let audit = routing_audit(&t, &events);
        assert_eq!(audit.len(), 1, "{audit:?}");
        let a = &audit[0];
        assert_eq!(a.run_id, "r1");
        assert_eq!(a.org_node.as_deref(), Some("software-engineering"));
        assert_eq!(a.harness.as_deref(), Some("coding"));
        assert_eq!(a.adapter.as_deref(), Some("claude-code"));
        assert_eq!(a.lane, Some(Tier::Standard));
        assert_eq!(a.model.as_deref(), Some("model-std"));
        assert_eq!(a.reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(a.features, Some(decision.features));
        assert_eq!(a.rule_id.as_deref(), Some(decision.rule_id.as_str()));
        assert_eq!(a.policy_version.as_deref(), Some("lane-policy/1"));
        assert_eq!(a.cost_usd, Some(0.5));
        assert_eq!((a.input_tokens, a.output_tokens), (Some(100), Some(20)));
        assert_eq!((a.wall_ms, a.retries), (Some(1234), Some(1)));
        assert_eq!(
            a.review,
            Some(ReviewResult {
                passed: false,
                failed_criteria: vec![1]
            })
        );
    }

    /// 導入前のイベント（`RoutingDecided` 無し）だけでも run は並び、routing の欄が空になるだけ。
    /// `RoutingDecided` を知らない旧いイベント JSON もそのまま読める。
    #[test]
    fn legacy_events_without_routing_still_aggregate() {
        let t = crate::model_policy::tests::task("x", vec![]);
        let old: Event = serde_json::from_str(
            r#"{"type":"worker_finished","run_id":"r0","outcome":"done: ok","usage":null}"#,
        )
        .unwrap();
        let audit = routing_audit(&t, &[old]);
        assert_eq!(audit.len(), 1);
        assert!(audit[0].lane.is_none() && audit[0].rule_id.is_none());
        assert_eq!(audit[0].outcome.as_deref(), Some("done: ok"));
    }
}
