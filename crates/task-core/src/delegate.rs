//! 実行中の委譲（ADR-0016 D2 / M6 / M7）。ワーカーが `delegate` メッセージ（LLM アダプタは `artifacts/delegate.json`）で
//! 提案する子タスクの型と、ストアを見ない純粋な検証・組み立て。ストアを見る検証（既存 ID の依存、祖先、木の深さ・run 数）は
//! `task-ops::delegate` にある。I/O・LLM 呼び出しは無い。

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::model::{Criterion, RoleSpec, Status, Task, TaskId, TaskKind, Tier, WorkerHint};

/// `delegate.tasks[].depends_on[]` の 1 要素（ADR-0016 M7）: 同じ配列内のインデックス（整数）か、既存タスクの ID（文字列）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum DelegateDep {
    Index(usize),
    Id(String),
}

/// ワーカーが提案する子タスク 1 件。未知フィールドは拒否する（Plan の `NewTask` と同じ方針）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateTask {
    pub title: String,
    pub objective: String,
    /// 1 件以上。
    pub acceptance: Vec<Criterion>,
    /// 役割名（任意。`[[roles]]` にあれば既定と指示文が効く）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<DelegateDep>,
    /// 省略時は役割の既定 → 親の tier。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
}

/// 委譲の上限（ADR-0016 D2。既定 8 / 5 / 100）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DelegationLimits {
    /// 1 run あたりの件数（複数の `delegate` をまたいで数える）。
    pub max_delegate_per_run: usize,
    /// 木の深さ（根 = 1）。
    pub max_tree_depth: u32,
    /// 木全体のワーカー run 数。
    pub max_tree_runs: u32,
}

impl Default for DelegationLimits {
    fn default() -> Self {
        Self {
            max_delegate_per_run: 8,
            max_tree_depth: 5,
            max_tree_runs: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DelegateError {
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
    #[error("tasks[{index}].depends_on[{position}] = {id:?} is not a task id")]
    InvalidId { index: usize, position: usize, id: String },
}

/// ストアを見ない検証（ADR-0016 M7）: 空欄、配列内インデックスの範囲・自己参照・閉路、ID の書式。
/// 1 件ごとに結果を返す（通ったものだけを挿入するため）。閉路は関係する全ての要素を不合格にする。
pub fn validate_each(tasks: &[DelegateTask]) -> Vec<Result<(), DelegateError>> {
    let len = tasks.len();
    let mut results: Vec<Result<(), DelegateError>> = tasks
        .iter()
        .enumerate()
        .map(|(index, t)| validate_one(index, t, len))
        .collect();
    // 閉路検出（三色法）。インデックス依存だけを辿る。
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Grey,
        Black,
    }
    let mut color = vec![Color::White; len];
    for start in 0..len {
        if color[start] != Color::White {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = Color::Grey;
        while let Some(&mut (node, ref mut pos)) = stack.last_mut() {
            let deps = &tasks[node].depends_on;
            if *pos < deps.len() {
                let dep = &deps[*pos];
                *pos += 1;
                let DelegateDep::Index(child) = dep else { continue };
                if *child >= len || *child == node {
                    continue; // 範囲外・自己参照は validate_one が既に不合格にしている
                }
                match color[*child] {
                    Color::White => {
                        color[*child] = Color::Grey;
                        stack.push((*child, 0));
                    }
                    Color::Grey => {
                        // スタック上の child 以降が閉路。
                        let from = stack.iter().position(|(n, _)| n == child).unwrap_or(0);
                        for (n, _) in &stack[from..] {
                            if results[*n].is_ok() {
                                results[*n] = Err(DelegateError::Cycle { index: *n });
                            }
                        }
                    }
                    Color::Black => {}
                }
            } else {
                color[node] = Color::Black;
                stack.pop();
            }
        }
    }
    results
}

fn validate_one(index: usize, t: &DelegateTask, len: usize) -> Result<(), DelegateError> {
    if t.title.trim().is_empty() {
        return Err(DelegateError::EmptyField { index, field: "title" });
    }
    if t.objective.trim().is_empty() {
        return Err(DelegateError::EmptyField {
            index,
            field: "objective",
        });
    }
    if t.acceptance.is_empty() {
        return Err(DelegateError::NoAcceptance { index });
    }
    for (criterion, c) in t.acceptance.iter().enumerate() {
        if c.text.trim().is_empty() {
            return Err(DelegateError::EmptyCriterion { index, criterion });
        }
    }
    for (position, dep) in t.depends_on.iter().enumerate() {
        match dep {
            DelegateDep::Index(target) => {
                if *target >= len {
                    return Err(DelegateError::DependencyOutOfRange {
                        index,
                        position,
                        target: *target,
                        len,
                    });
                }
                if *target == index {
                    return Err(DelegateError::SelfDependency { index });
                }
            }
            DelegateDep::Id(id) => {
                if id.parse::<TaskId>().is_err() {
                    return Err(DelegateError::InvalidId {
                        index,
                        position,
                        id: id.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// 検証を通った提案（`accepted` はインデックス）から子タスクを組み立てる（ADR-0016 M2 / M3）。
/// `tier` はタスクの値 > 役割の既定 > 親の tier、`adapter` は役割の既定 > 親、`budget` は役割の既定 > 親。
/// 配列内インデックスの依存は、相手も `accepted` に入っているときだけ ID に写す（不合格の相手への依存は落とす）。
/// ID の依存は呼び出し側（task-ops）が検証済みのものだけを残して渡すこと。
pub fn materialize_delegated(
    parent: &Task,
    tasks: &[DelegateTask],
    accepted: &[usize],
    roles: &[RoleSpec],
    now: OffsetDateTime,
) -> Vec<Task> {
    let ids: HashMap<usize, TaskId> = accepted.iter().map(|&i| (i, TaskId::new())).collect();
    accepted
        .iter()
        .map(|&i| {
            let t = &tasks[i];
            let role = t.role.as_deref().and_then(|r| RoleSpec::find(roles, r));
            let depends_on: Vec<TaskId> = t
                .depends_on
                .iter()
                .filter_map(|d| match d {
                    DelegateDep::Index(j) => ids.get(j).copied(),
                    DelegateDep::Id(s) => s.parse::<TaskId>().ok(),
                })
                .collect();
            let mut budget = parent.budget;
            if let Some(v) = role.and_then(|r| r.max_turns) {
                budget.max_turns = v;
            }
            if let Some(v) = role.and_then(|r| r.max_wall_secs) {
                budget.max_wall_secs = v;
            }
            Task {
                id: ids[&i],
                parent_id: Some(parent.id),
                kind: TaskKind::Execute,
                title: t.title.clone(),
                objective: t.objective.clone(),
                acceptance: t.acceptance.clone(),
                inputs: vec![],
                depends_on,
                status: Status::Draft,
                priority: parent.priority,
                worker_hint: WorkerHint {
                    tier: t
                        .tier
                        .or(role.and_then(|r| r.tier))
                        .unwrap_or(parent.worker_hint.tier),
                    adapter: role
                        .and_then(|r| r.adapter.clone())
                        .or_else(|| parent.worker_hint.adapter.clone()),
                },
                workspace: parent.workspace.clone(),
                budget,
                attempts: 0,
                lease: None,
                created_at: now,
                updated_at: now,
                role: t.role.clone(),
                aggregate: false,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Budget, Check, WorkspaceSpec};
    use std::path::PathBuf;

    fn dt(title: &str, deps: Vec<DelegateDep>) -> DelegateTask {
        DelegateTask {
            title: title.into(),
            objective: format!("do {title}"),
            acceptance: vec![Criterion {
                text: "c".into(),
                check: Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            }],
            role: None,
            depends_on: deps,
            tier: None,
        }
    }

    fn parent() -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "p".into(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Running,
            priority: 3,
            worker_hint: WorkerHint {
                tier: Tier::Frontier,
                adapter: Some("fake".into()),
            },
            workspace: WorkspaceSpec::Local {
                path: PathBuf::from("/tmp/ws"),
            },
            budget: Budget {
                max_turns: 10,
                max_wall_secs: 600,
                max_retries: 2,
            },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: Some("lead".into()),
            aggregate: true,
        }
    }

    #[test]
    fn depends_on_accepts_index_or_id_and_rejects_other_shapes() {
        let json = r#"{"title":"a","objective":"o","acceptance":[{"text":"c","check":{"type":"human"}}],
            "depends_on":[0,"01J9ZX5T3K8Q7W6V5R4P3N2M1H"]}"#;
        let t: DelegateTask = serde_json::from_str(json).unwrap();
        assert_eq!(t.depends_on[0], DelegateDep::Index(0));
        assert_eq!(t.depends_on[1], DelegateDep::Id("01J9ZX5T3K8Q7W6V5R4P3N2M1H".into()));
        assert!(serde_json::from_str::<DelegateTask>(r#"{"title":"a","objective":"o","acceptance":[],"bogus":1}"#).is_err());
        assert!(serde_json::from_str::<DelegateTask>(r#"{"title":"a","objective":"o","acceptance":[],"depends_on":[true]}"#).is_err());
    }

    #[test]
    fn validate_each_reports_per_item_and_marks_cycles() {
        let tasks = vec![
            dt("ok", vec![]),
            dt("", vec![]),
            dt("self", vec![DelegateDep::Index(2)]),
            dt("range", vec![DelegateDep::Index(9)]),
            dt("cyc-a", vec![DelegateDep::Index(5)]),
            dt("cyc-b", vec![DelegateDep::Index(4)]),
            dt("bad-id", vec![DelegateDep::Id("nope".into())]),
            dt("dep-ok", vec![DelegateDep::Index(0)]),
        ];
        let r = validate_each(&tasks);
        assert!(r[0].is_ok());
        assert_eq!(r[1], Err(DelegateError::EmptyField { index: 1, field: "title" }));
        assert_eq!(r[2], Err(DelegateError::SelfDependency { index: 2 }));
        assert!(matches!(r[3], Err(DelegateError::DependencyOutOfRange { index: 3, target: 9, len: 8, .. })));
        assert!(matches!(r[4], Err(DelegateError::Cycle { .. })), "{:?}", r[4]);
        assert!(matches!(r[5], Err(DelegateError::Cycle { .. })), "{:?}", r[5]);
        assert!(matches!(r[6], Err(DelegateError::InvalidId { index: 6, .. })));
        assert!(r[7].is_ok());
        let mut no_acc = dt("x", vec![]);
        no_acc.acceptance.clear();
        assert_eq!(validate_each(&[no_acc])[0], Err(DelegateError::NoAcceptance { index: 0 }));
    }

    #[test]
    fn materialize_applies_role_defaults_and_maps_index_dependencies_of_accepted_only() {
        let p = parent();
        let roles = vec![RoleSpec {
            id: "implementer".into(),
            tier: Some(Tier::Cheap),
            adapter: Some("codex".into()),
            max_turns: Some(3),
            max_wall_secs: None,
            instructions: Some("implement".into()),
        }];
        let mut a = dt("a", vec![]);
        a.role = Some("implementer".into());
        let b = dt("b", vec![DelegateDep::Index(0), DelegateDep::Index(2)]);
        let mut c = dt("c", vec![]);
        c.tier = Some(Tier::Standard);
        c.role = Some("implementer".into());
        let tasks = vec![a, b, c];
        let out = materialize_delegated(&p, &tasks, &[0, 1], &roles, OffsetDateTime::now_utc());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].parent_id, Some(p.id));
        assert_eq!(out[0].status, Status::Draft);
        assert_eq!(out[0].role.as_deref(), Some("implementer"));
        assert_eq!(out[0].worker_hint.tier, Tier::Cheap);
        assert_eq!(out[0].worker_hint.adapter.as_deref(), Some("codex"));
        assert_eq!(out[0].budget.max_turns, 3);
        assert_eq!(out[0].budget.max_wall_secs, 600);
        assert_eq!(out[0].priority, 3);
        assert!(!out[0].aggregate);
        // b は a（採用）だけに依存し、c（不採用）への依存は落ちる。役割無しは親の tier / adapter。
        assert_eq!(out[1].depends_on, vec![out[0].id]);
        assert_eq!(out[1].worker_hint.tier, Tier::Frontier);
        assert_eq!(out[1].worker_hint.adapter.as_deref(), Some("fake"));
        // タスクの tier は役割の既定より優先。
        let out = materialize_delegated(&p, &tasks, &[2], &roles, OffsetDateTime::now_utc());
        assert_eq!(out[0].worker_hint.tier, Tier::Standard);
    }
}
