//! Planner の出力 `PlanOutput`（DESIGN §5.6, ADR-0007 D2）。純粋な型・検証・子タスク生成のみで、
//! I/O や LLM 呼び出しは無い。JSON Schema は `schemars` で生成し
//! `docs/protocol/plan-output.schema.json` と一致することをテストで検証する。

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::model::{Criterion, Status, Task, TaskId, TaskKind, Tier, WorkerHint};

/// DESIGN §5.6「分解の深さは上限 3」。`plan_depth`（その Plan 自身を含む祖先 Plan の数）が
/// これに達している Plan は、`kind = plan` の子を作れない。
pub const MAX_PLAN_DEPTH: u32 = 3;

/// 子タスクの kind。`Approval`/`Review` はプランナーからは作れない（承認ゲートは Phase 6、
/// Review kind は Reviewer の合成タスク専用。ADR-0007 D2/D5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NewTaskKind {
    #[default]
    Execute,
    Plan,
}

/// プランナーが返す子タスク 1 件。未知フィールドは拒否する（綴り間違いの検出。ADR-0007 D2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTask {
    pub title: String,
    pub objective: String,
    /// 1 件以上。`check` は DESIGN §5.7 の 4 種すべて使える。
    pub acceptance: Vec<Criterion>,
    /// 同じ `tasks` 配列内のインデックス。
    #[serde(default)]
    pub depends_on: Vec<usize>,
    #[serde(default)]
    pub kind: NewTaskKind,
    /// 省略時は `Standard`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// ADR-0016 D1 / M10: 子の役割名（任意）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// DESIGN §5.6 の `PlanOutput{ tasks: Vec<NewTask> }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanOutput {
    pub tasks: Vec<NewTask>,
}

/// 件数の上下限（ADR-0007 D2。既定 1..=20）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanLimits {
    pub min_tasks: usize,
    pub max_tasks: usize,
}

impl Default for PlanLimits {
    fn default() -> Self {
        Self {
            min_tasks: 1,
            max_tasks: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("plan has {actual} tasks; expected between {min} and {max}")]
    TaskCount { actual: usize, min: usize, max: usize },
    #[error("tasks[{index}].{field} must not be empty")]
    EmptyField { index: usize, field: &'static str },
    #[error("tasks[{index}].acceptance must have at least one criterion")]
    NoAcceptance { index: usize },
    #[error("tasks[{index}].acceptance[{criterion}].text must not be empty")]
    EmptyCriterion { index: usize, criterion: usize },
    #[error("tasks[{index}].depends_on[{position}] = {target} is out of range (0..{len})")]
    DependencyOutOfRange {
        index: usize,
        position: usize,
        target: usize,
        len: usize,
    },
    #[error("tasks[{index}] depends on itself")]
    SelfDependency { index: usize },
    #[error("dependency cycle involving tasks[{index}]")]
    Cycle { index: usize },
    #[error("tasks[{index}] is kind=plan but the plan depth would become {depth} (max {max})")]
    DepthExceeded { index: usize, depth: u32, max: u32 },
}

/// `PlanOutput` をデシリアライズして検証する。`plan_depth` はその Plan 自身を含む祖先 Plan の数。
pub fn parse_and_validate(
    json: &str,
    plan_depth: u32,
    limits: &PlanLimits,
) -> Result<PlanOutput, String> {
    let plan: PlanOutput = serde_json::from_str(json).map_err(|e| format!("invalid plan.json: {e}"))?;
    validate(&plan, plan_depth, limits).map_err(|e| e.to_string())?;
    Ok(plan)
}

/// 決定的な検証（ADR-0007 D2）。
pub fn validate(plan: &PlanOutput, plan_depth: u32, limits: &PlanLimits) -> Result<(), PlanError> {
    let len = plan.tasks.len();
    if len < limits.min_tasks || len > limits.max_tasks {
        return Err(PlanError::TaskCount {
            actual: len,
            min: limits.min_tasks,
            max: limits.max_tasks,
        });
    }
    for (index, t) in plan.tasks.iter().enumerate() {
        if t.title.trim().is_empty() {
            return Err(PlanError::EmptyField { index, field: "title" });
        }
        if t.objective.trim().is_empty() {
            return Err(PlanError::EmptyField {
                index,
                field: "objective",
            });
        }
        if t.acceptance.is_empty() {
            return Err(PlanError::NoAcceptance { index });
        }
        for (criterion, c) in t.acceptance.iter().enumerate() {
            if c.text.trim().is_empty() {
                return Err(PlanError::EmptyCriterion { index, criterion });
            }
        }
        for (position, &target) in t.depends_on.iter().enumerate() {
            if target >= len {
                return Err(PlanError::DependencyOutOfRange {
                    index,
                    position,
                    target,
                    len,
                });
            }
            if target == index {
                return Err(PlanError::SelfDependency { index });
            }
        }
        if t.kind == NewTaskKind::Plan && plan_depth + 1 > MAX_PLAN_DEPTH {
            return Err(PlanError::DepthExceeded {
                index,
                depth: plan_depth + 1,
                max: MAX_PLAN_DEPTH,
            });
        }
    }
    detect_cycle(plan)
}

/// DFS による閉路検出（三色法）。
fn detect_cycle(plan: &PlanOutput) -> Result<(), PlanError> {
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Grey,
        Black,
    }
    let n = plan.tasks.len();
    let mut color = vec![Color::White; n];
    for start in 0..n {
        if color[start] != Color::White {
            continue;
        }
        // 明示的スタック: (node, next_child_position)
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = Color::Grey;
        while let Some(&mut (node, ref mut pos)) = stack.last_mut() {
            if *pos < plan.tasks[node].depends_on.len() {
                let child = plan.tasks[node].depends_on[*pos];
                *pos += 1;
                match color[child] {
                    Color::White => {
                        color[child] = Color::Grey;
                        stack.push((child, 0));
                    }
                    Color::Grey => return Err(PlanError::Cycle { index: child }),
                    Color::Black => {}
                }
            } else {
                color[node] = Color::Black;
                stack.pop();
            }
        }
    }
    Ok(())
}

/// 検証済みの `PlanOutput` から子タスクを組み立てる（ADR-0007 D2）。`validate` を通した plan だけを渡すこと。
pub fn materialize(parent: &Task, plan: &PlanOutput, now: OffsetDateTime) -> Vec<Task> {
    let ids: Vec<TaskId> = plan.tasks.iter().map(|_| TaskId::new()).collect();
    let index_to_id: HashMap<usize, TaskId> = ids.iter().copied().enumerate().collect();
    plan.tasks
        .iter()
        .enumerate()
        .map(|(i, t)| Task {
            id: ids[i],
            parent_id: Some(parent.id),
            kind: match t.kind {
                NewTaskKind::Execute => TaskKind::Execute,
                NewTaskKind::Plan => TaskKind::Plan,
            },
            title: t.title.clone(),
            objective: t.objective.clone(),
            acceptance: t.acceptance.clone(),
            inputs: vec![],
            depends_on: t
                .depends_on
                .iter()
                .filter_map(|d| index_to_id.get(d).copied())
                .collect(),
            status: Status::Draft,
            priority: parent.priority,
            worker_hint: WorkerHint {
                tier: t.tier.unwrap_or(Tier::Standard),
                adapter: parent.worker_hint.adapter.clone(),
            },
            workspace: parent.workspace.clone(),
            budget: parent.budget,
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: t.role.clone(),
            aggregate: false,
        })
        .collect()
}

/// 生成したスキーマ（`serde_json::Value`）。
pub fn schema_value() -> serde_json::Value {
    let schema = schemars::schema_for!(PlanOutput);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Budget, Check, WorkspaceSpec};
    use std::path::PathBuf;

    fn new_task(title: &str, deps: Vec<usize>) -> NewTask {
        NewTask {
            title: title.into(),
            objective: format!("do {title}"),
            acceptance: vec![Criterion {
                text: "c".into(),
                check: Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            }],
            depends_on: deps,
            kind: NewTaskKind::Execute,
            tier: None,
            role: None,
        }
    }

    fn parent() -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Plan,
            title: "plan".into(),
            objective: "goal".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Reviewing,
            priority: 3,
            worker_hint: WorkerHint {
                tier: Tier::Frontier,
                adapter: Some("fake".into()),
            },
            workspace: WorkspaceSpec::Local {
                path: PathBuf::from("/tmp/ws"),
            },
            budget: Budget {
                max_turns: 30,
                max_wall_secs: 900,
                max_retries: 1,
            },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: None,
            aggregate: false,
        }
    }

    #[test]
    fn valid_plan_passes_and_materializes_children_with_inherited_fields() {
        let plan = PlanOutput {
            tasks: vec![new_task("a", vec![]), new_task("b", vec![0]), new_task("c", vec![0, 1])],
        };
        validate(&plan, 1, &PlanLimits::default()).unwrap();
        let p = parent();
        let children = materialize(&p, &plan, OffsetDateTime::now_utc());
        assert_eq!(children.len(), 3);
        for c in &children {
            assert_eq!(c.parent_id, Some(p.id));
            assert_eq!(c.status, Status::Draft);
            assert_eq!(c.priority, 3);
            assert_eq!(c.workspace, p.workspace);
            assert_eq!(c.budget, p.budget);
            assert_eq!(c.worker_hint.adapter.as_deref(), Some("fake"));
            assert_eq!(c.worker_hint.tier, Tier::Standard);
            assert_eq!(c.kind, TaskKind::Execute);
        }
        assert_eq!(children[1].depends_on, vec![children[0].id]);
        assert_eq!(children[2].depends_on, vec![children[0].id, children[1].id]);
    }

    #[test]
    fn rejects_count_empty_fields_and_missing_acceptance() {
        let limits = PlanLimits {
            min_tasks: 2,
            max_tasks: 3,
        };
        let one = PlanOutput {
            tasks: vec![new_task("a", vec![])],
        };
        assert!(matches!(
            validate(&one, 1, &limits),
            Err(PlanError::TaskCount { actual: 1, min: 2, max: 3 })
        ));
        let mut empty_title = PlanOutput {
            tasks: vec![new_task("a", vec![]), new_task("b", vec![])],
        };
        empty_title.tasks[1].title = "  ".into();
        assert!(matches!(
            validate(&empty_title, 1, &limits),
            Err(PlanError::EmptyField { index: 1, field: "title" })
        ));
        let mut no_acc = PlanOutput {
            tasks: vec![new_task("a", vec![]), new_task("b", vec![])],
        };
        no_acc.tasks[0].acceptance.clear();
        assert!(matches!(validate(&no_acc, 1, &limits), Err(PlanError::NoAcceptance { index: 0 })));
    }

    #[test]
    fn rejects_bad_dependencies_and_cycles() {
        let oor = PlanOutput {
            tasks: vec![new_task("a", vec![7])],
        };
        let err = validate(&oor, 1, &PlanLimits::default()).unwrap_err();
        assert!(matches!(err, PlanError::DependencyOutOfRange { index: 0, target: 7, .. }));
        assert!(err.to_string().contains("out of range"));

        let self_dep = PlanOutput {
            tasks: vec![new_task("a", vec![0])],
        };
        assert!(matches!(
            validate(&self_dep, 1, &PlanLimits::default()),
            Err(PlanError::SelfDependency { index: 0 })
        ));

        let cycle = PlanOutput {
            tasks: vec![new_task("a", vec![2]), new_task("b", vec![0]), new_task("c", vec![1])],
        };
        assert!(matches!(
            validate(&cycle, 1, &PlanLimits::default()),
            Err(PlanError::Cycle { .. })
        ));

        let diamond = PlanOutput {
            tasks: vec![
                new_task("a", vec![]),
                new_task("b", vec![0]),
                new_task("c", vec![0]),
                new_task("d", vec![1, 2]),
            ],
        };
        validate(&diamond, 1, &PlanLimits::default()).unwrap();
    }

    #[test]
    fn nested_plan_respects_depth_limit() {
        let mut nested = PlanOutput {
            tasks: vec![new_task("sub", vec![])],
        };
        nested.tasks[0].kind = NewTaskKind::Plan;
        validate(&nested, 1, &PlanLimits::default()).unwrap();
        validate(&nested, 2, &PlanLimits::default()).unwrap();
        assert!(matches!(
            validate(&nested, 3, &PlanLimits::default()),
            Err(PlanError::DepthExceeded { index: 0, depth: 4, max: 3 })
        ));
        let p = parent();
        let children = materialize(&p, &nested, OffsetDateTime::now_utc());
        assert_eq!(children[0].kind, TaskKind::Plan);
    }

    #[test]
    fn parse_rejects_unknown_fields_and_reports_serde_errors() {
        let ok = r#"{"tasks":[{"title":"t","objective":"o","acceptance":[{"text":"c","check":{"type":"reviewer"}}]}]}"#;
        let plan = parse_and_validate(ok, 1, &PlanLimits::default()).unwrap();
        assert_eq!(plan.tasks[0].acceptance[0].check, Check::Reviewer);
        assert_eq!(plan.tasks[0].kind, NewTaskKind::Execute);
        let unknown = r#"{"tasks":[{"title":"t","objective":"o","acceptance":[{"text":"c","check":{"type":"human"}}],"bogus":1}]}"#;
        let err = parse_and_validate(unknown, 1, &PlanLimits::default()).unwrap_err();
        assert!(err.contains("bogus"), "{err}");
        assert!(parse_and_validate("not json", 1, &PlanLimits::default()).is_err());
    }

    /// ADR-0007 D2 / ADR-0003 D6: 生成スキーマとコミット済みファイルの一致。`UPDATE_SCHEMA=1` で再生成。
    #[test]
    fn committed_schema_matches_generated() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/protocol/plan-output.schema.json");
        let generated = serde_json::to_string_pretty(&schema_value()).unwrap() + "\n";
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e} (run with UPDATE_SCHEMA=1 to generate)"));
        assert_eq!(committed, generated, "schema drift: run `UPDATE_SCHEMA=1 cargo test -p task-core`");
    }
}
