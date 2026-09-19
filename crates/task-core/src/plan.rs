//! Planner の出力 `PlanOutput`（DESIGN §5.6, ADR-0007 D2）。純粋な型・検証・子タスク生成のみで、
//! I/O や LLM 呼び出しは無い。JSON Schema は `schemars` で生成し
//! `docs/protocol/plan-output.schema.json` と一致することをテストで検証する。

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::delegate::{ChildSpec, WorkspaceContext, resolve_child_defaults};
use crate::model::{
    Budget, Check, Criterion, GenreSpec, RoleSpec, Status, Task, TaskId, TaskKind, Tier, WorkerHint, WorkspaceSpec,
};
use crate::org::OrgNode;

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
    /// ADR-0028 D3: 子の分野名（任意）。明示 > `role` の分野（`roles` に含む分野がちょうど 1 つのとき）>
    /// 親の分野の順で解決する（ADR-0027 D1 の委譲と同じ規則）。`genre` を指定して知らない分野、または
    /// `role` と両方指定してその役割がその分野の `roles` に無ければ、Plan run は失敗する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// ADR-0033 D4（Phase 24）: 子を割り当てる組織のノード（`org_nodes.id`。SPEC §3.3「案件は組織の上から
    /// 入り、分解されて下へ流れる」）。プランナーは普段これだけを書けばよく、`role` は必要なときだけ書く。
    /// `role` を書いたときは tier / adapter / 予算はその役割が勝ち、`assignee` は「誰の仕事か」だけを表す。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// ADR-0039 D2: この子の作業場所（任意）。**書かなければ案件の作業場所 → 親の workspace** を継ぐので、
    /// 別の場所（別のリポジトリ・別のクラスタ）で作業させたいときにだけ書く。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceSpec>,
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
    /// ADR-0028 D3: 知らない `genre`。
    #[error("tasks[{index}].genre {genre:?} is not a known genre")]
    UnknownGenre { index: usize, genre: String },
    /// ADR-0028 D3: `genre` と `role` を両方指定したが、`role` がその分野の `roles` に含まれない。
    #[error("tasks[{index}].role {role:?} is not one of genre {genre:?}'s roles")]
    RoleNotInGenre { index: usize, role: String, genre: String },
}

/// `PlanOutput` をデシリアライズして検証する。`plan_depth` はその Plan 自身を含む祖先 Plan の数。
/// `genres` は ADR-0028 D3 の `genre`/`role` の整合検証に使う（`[[genres]]` が空の設定では、`genre` を
/// 指定した子は全て `UnknownGenre` になる）。
pub fn parse_and_validate(
    json: &str,
    plan_depth: u32,
    limits: &PlanLimits,
    genres: &[GenreSpec],
) -> Result<PlanOutput, String> {
    let plan: PlanOutput = serde_json::from_str(json).map_err(|e| format!("invalid plan.json: {e}"))?;
    validate(&plan, plan_depth, limits, genres).map_err(|e| e.to_string())?;
    Ok(plan)
}

/// 決定的な検証（ADR-0007 D2, ADR-0028 D3）。
pub fn validate(plan: &PlanOutput, plan_depth: u32, limits: &PlanLimits, genres: &[GenreSpec]) -> Result<(), PlanError> {
    let len = plan.tasks.len();
    if len < limits.min_tasks || len > limits.max_tasks {
        return Err(PlanError::TaskCount {
            actual: len,
            min: limits.min_tasks,
            max: limits.max_tasks,
        });
    }
    for (index, t) in plan.tasks.iter().enumerate() {
        if let Some(genre) = &t.genre {
            let Some(spec) = GenreSpec::find(genres, genre) else {
                return Err(PlanError::UnknownGenre { index, genre: genre.clone() });
            };
            if let Some(role) = &t.role
                && !spec.roles.iter().any(|r| r == role)
            {
                return Err(PlanError::RoleNotInGenre {
                    index,
                    role: role.clone(),
                    genre: genre.clone(),
                });
            }
        }
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

/// Phase 38（ADR-0028 追記。実機のレビュー不合格から）: **ハーネス系の分野**（`GenreSpec::is_harness`）の
/// 担当に「自分で決めた名前のファイルを書け」と要求する `artifact_exists` の受け入れ条件を落として、
/// 代わりに `objective` の末尾に「本当の成果物の名前」を注記する（**壊さず直す**: Plan run は失敗させず、
/// `Question` にもしない）。分野の決め方は `materialize` と同じ（`resolve_child_defaults`: 明示 > 役割 >
/// 担当 > 親）で、判定は決定的（LLM は使わない。DESIGN 原則 1）。
///
/// 戻り値は起きた修正の説明（呼び出し元が `warn` で残す）。`output_artifacts` が空の分野、ハーネスでない
/// 分野（coding 等）、名前が一致している条件には触らない。条件が 1 件も残らなくなるときだけ、落とす代わりに
/// **同じ文のレビュアー条件**にする（受け入れ条件が 0 件のタスクを作らないため。「内容はレビュアーに
/// 判定させる」という Phase 38 の方針とも一致する）。
pub fn fix_harness_artifacts(
    plan: &mut PlanOutput,
    parent: &Task,
    org: &[OrgNode],
    roles: &[RoleSpec],
    genres: &[GenreSpec],
) -> Vec<String> {
    let mut warnings = Vec::new();
    for (index, t) in plan.tasks.iter_mut().enumerate() {
        let resolved = resolve_child_defaults(
            parent,
            ChildSpec {
                genre: t.genre.as_deref(),
                role: t.role.as_deref(),
                tier: t.tier,
                assignee: t.assignee.as_deref(),
            },
            org,
            roles,
            genres,
        );
        let Some(spec) = resolved.genre.as_deref().and_then(|g| GenreSpec::find(genres, g)) else {
            continue;
        };
        if !spec.is_harness(roles) {
            continue;
        }
        let allowed = spec.output_artifact_names();
        let Some(&answer) = allowed.first() else {
            continue;
        };
        let mismatched: Vec<usize> = t
            .acceptance
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match &c.check {
                Check::ArtifactExists { name } if !allowed.contains(&name.trim()) => Some(i),
                _ => None,
            })
            .collect();
        if mismatched.is_empty() {
            continue;
        }
        let names: Vec<String> = mismatched
            .iter()
            .filter_map(|&i| match &t.acceptance[i].check {
                Check::ArtifactExists { name } => Some(name.clone()),
                _ => None,
            })
            .collect();
        let list = allowed.join(" / ");
        // 残るものが無くなるなら、落とす代わりに同じ文をレビュアー条件にする。
        if mismatched.len() == t.acceptance.len() {
            for &i in &mismatched {
                t.acceptance[i].check = Check::Reviewer;
            }
        } else {
            let mut kept = Vec::with_capacity(t.acceptance.len() - mismatched.len());
            for (i, c) in t.acceptance.iter().enumerate() {
                if !mismatched.contains(&i) {
                    kept.push(c.clone());
                }
            }
            t.acceptance = kept;
        }
        t.objective = format!(
            "{}\n（注: この担当の成果物は {list} に固定。要求した内容は {answer} の中で述べる）",
            t.objective.trim_end()
        );
        warnings.push(format!(
            "plan tasks[{index}] ({}): genre {:?} runs on a harness and can only write {list}; \
             dropped the artifact_exists criteria for {} and noted the real artifacts in the objective",
            t.title,
            spec.id,
            names.join(", ")
        ));
    }
    warnings
}

/// 検証済みの `PlanOutput` から子タスクを組み立てる（ADR-0007 D2, ADR-0028 D3）。`validate` を通した
/// plan だけを渡すこと（`genre`/`role` の不整合は既に無いという前提で、ここではエラーを返さない）。
/// `tier` / `adapter` / `budget` と分野は、委譲（`materialize_delegated`）と同じ決め方
/// （タスクの値 > 役割の既定 > 分野の既定 > 親の値。分野は 明示 > role の分野 > 親の分野）を使う。
pub fn materialize(
    parent: &Task,
    plan: &PlanOutput,
    org: &[OrgNode],
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    workspace: WorkspaceContext<'_>,
    now: OffsetDateTime,
) -> Vec<Task> {
    let ids: Vec<TaskId> = plan.tasks.iter().map(|_| TaskId::new()).collect();
    let index_to_id: HashMap<usize, TaskId> = ids.iter().copied().enumerate().collect();
    plan.tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let defaults = resolve_child_defaults(
                parent,
                ChildSpec {
                    genre: t.genre.as_deref(),
                    role: t.role.as_deref(),
                    tier: t.tier,
                    assignee: t.assignee.as_deref(),
                },
                org,
                roles,
                genres,
            );
            Task {
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
                    tier: defaults.tier,
                    adapter: defaults.adapter,
                },
                // ADR-0039 D2: 明示 > 案件の workspace > 親の workspace（従来）。
                workspace: workspace.child_workspace(parent, t.workspace.as_ref()),
                budget: Budget {
                    max_turns: defaults.max_turns,
                    max_wall_secs: defaults.max_wall_secs,
                    max_retries: parent.budget.max_retries,
                },
                attempts: 0,
                lease: None,
                created_at: now,
                updated_at: now,
                role: t.role.clone(),
                genre: defaults.genre,
                aggregate: false,
                // ADR-0033 D2: 案件の仕事の木は `tasks WHERE project_id = ?` なので、分解した子も
                // 同じ案件・途中目標に属する（担当は Phase 24 で計画が指定するまで空）。
                project_id: parent.project_id,
                milestone_id: parent.milestone_id,
                assignee: defaults.assignee,
                conversation: None,
            }
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
    use crate::model::{Check, WorkspaceSpec};
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
            genre: None,
            assignee: None,
            workspace: None,
        }
    }

    fn genre(id: &str, default_role: Option<&str>, roles: &[&str]) -> GenreSpec {
        GenreSpec {
            id: id.into(),
            description: format!("{id} description"),
            default_role: default_role.map(str::to_string),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            ..GenreSpec::default()
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
                path: PathBuf::from("/tmp/ws"), mode: None,
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
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            conversation: None,
        }
    }

    #[test]
    fn valid_plan_passes_and_materializes_children_with_inherited_fields() {
        let plan = PlanOutput {
            tasks: vec![new_task("a", vec![]), new_task("b", vec![0]), new_task("c", vec![0, 1])],
        };
        validate(&plan, 1, &PlanLimits::default(), &[]).unwrap();
        let p = parent();
        let children = materialize(&p, &plan, &[], &[], &[], WorkspaceContext::default(), OffsetDateTime::now_utc());
        assert_eq!(children.len(), 3);
        for c in &children {
            assert_eq!(c.parent_id, Some(p.id));
            assert_eq!(c.status, Status::Draft);
            assert_eq!(c.priority, 3);
            assert_eq!(c.workspace, p.workspace);
            assert_eq!(c.budget, p.budget);
            assert_eq!(c.worker_hint.adapter.as_deref(), Some("fake"));
            // ADR-0028 D3: tier に既定が無ければ（役割・分野・タスクいずれも無指定）親の tier を継ぐ
            // （委譲と同じ規則。以前は独立した既定 `Standard` だった）。
            assert_eq!(c.worker_hint.tier, Tier::Frontier);
            assert_eq!(c.kind, TaskKind::Execute);
        }
        assert_eq!(children[1].depends_on, vec![children[0].id]);
        assert_eq!(children[2].depends_on, vec![children[0].id, children[1].id]);
    }

    /// ADR-0033 D2（監査 D-3。GUI 監査対応 Phase 29 で確認）: 分解した子は親の `project_id` /
    /// `milestone_id` を必ず継ぐ（案件の仕事の木から子が消えないように）。偽プランナーの出力からでも同じ。
    #[test]
    fn materialize_carries_the_parents_project_and_milestone_id() {
        let mut p = parent();
        p.project_id = Some(crate::org::ProjectId::new());
        p.milestone_id = Some(crate::org::MilestoneId::new());
        let plan = PlanOutput { tasks: vec![new_task("a", vec![]), new_task("b", vec![])] };
        let children = materialize(&p, &plan, &[], &[], &[], WorkspaceContext::default(), OffsetDateTime::now_utc());
        for c in &children {
            assert_eq!(c.project_id, p.project_id, "child must stay in the parent's project");
            assert_eq!(c.milestone_id, p.milestone_id);
        }

        // 案件が無い Plan（従来どおり）では子にも付かない。
        let none = parent();
        let children = materialize(&none, &plan, &[], &[], &[], WorkspaceContext::default(), OffsetDateTime::now_utc());
        assert!(children.iter().all(|c| c.project_id.is_none() && c.milestone_id.is_none()));
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
            validate(&one, 1, &limits, &[]),
            Err(PlanError::TaskCount { actual: 1, min: 2, max: 3 })
        ));
        let mut empty_title = PlanOutput {
            tasks: vec![new_task("a", vec![]), new_task("b", vec![])],
        };
        empty_title.tasks[1].title = "  ".into();
        assert!(matches!(
            validate(&empty_title, 1, &limits, &[]),
            Err(PlanError::EmptyField { index: 1, field: "title" })
        ));
        let mut no_acc = PlanOutput {
            tasks: vec![new_task("a", vec![]), new_task("b", vec![])],
        };
        no_acc.tasks[0].acceptance.clear();
        assert!(matches!(validate(&no_acc, 1, &limits, &[]), Err(PlanError::NoAcceptance { index: 0 })));
    }

    #[test]
    fn rejects_bad_dependencies_and_cycles() {
        let oor = PlanOutput {
            tasks: vec![new_task("a", vec![7])],
        };
        let err = validate(&oor, 1, &PlanLimits::default(), &[]).unwrap_err();
        assert!(matches!(err, PlanError::DependencyOutOfRange { index: 0, target: 7, .. }));
        assert!(err.to_string().contains("out of range"));

        let self_dep = PlanOutput {
            tasks: vec![new_task("a", vec![0])],
        };
        assert!(matches!(
            validate(&self_dep, 1, &PlanLimits::default(), &[]),
            Err(PlanError::SelfDependency { index: 0 })
        ));

        let cycle = PlanOutput {
            tasks: vec![new_task("a", vec![2]), new_task("b", vec![0]), new_task("c", vec![1])],
        };
        assert!(matches!(
            validate(&cycle, 1, &PlanLimits::default(), &[]),
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
        validate(&diamond, 1, &PlanLimits::default(), &[]).unwrap();
    }

    #[test]
    fn nested_plan_respects_depth_limit() {
        let mut nested = PlanOutput {
            tasks: vec![new_task("sub", vec![])],
        };
        nested.tasks[0].kind = NewTaskKind::Plan;
        validate(&nested, 1, &PlanLimits::default(), &[]).unwrap();
        validate(&nested, 2, &PlanLimits::default(), &[]).unwrap();
        assert!(matches!(
            validate(&nested, 3, &PlanLimits::default(), &[]),
            Err(PlanError::DepthExceeded { index: 0, depth: 4, max: 3 })
        ));
        let p = parent();
        let children = materialize(&p, &nested, &[], &[], &[], WorkspaceContext::default(), OffsetDateTime::now_utc());
        assert_eq!(children[0].kind, TaskKind::Plan);
    }

    #[test]
    fn parse_rejects_unknown_fields_and_reports_serde_errors() {
        let ok = r#"{"tasks":[{"title":"t","objective":"o","acceptance":[{"text":"c","check":{"type":"reviewer"}}]}]}"#;
        let plan = parse_and_validate(ok, 1, &PlanLimits::default(), &[]).unwrap();
        assert_eq!(plan.tasks[0].acceptance[0].check, Check::Reviewer);
        assert_eq!(plan.tasks[0].kind, NewTaskKind::Execute);
        let unknown = r#"{"tasks":[{"title":"t","objective":"o","acceptance":[{"text":"c","check":{"type":"human"}}],"bogus":1}]}"#;
        let err = parse_and_validate(unknown, 1, &PlanLimits::default(), &[]).unwrap_err();
        assert!(err.contains("bogus"), "{err}");
        assert!(parse_and_validate("not json", 1, &PlanLimits::default(), &[]).is_err());
    }

    /// ADR-0028 D3: 子の分野は 明示 > `role` の分野（一意なら） > 親の分野の順で決まる（委譲と同じ規則）。
    #[test]
    fn materialize_resolves_genre_by_precedence() {
        let mut p = parent();
        p.genre = Some("coding".into());
        let genres = vec![
            genre("coding", Some("implementer"), &["lead", "implementer"]),
            genre("literature", Some("literature-reader"), &["literature-scout", "literature-reader"]),
        ];

        // 1. 明示した genre が最優先（`materialize` は検証済みの plan だけを渡す前提で、ここでは
        // role/genre の整合そのものは見ない）。
        let mut explicit = new_task("explicit", vec![]);
        explicit.role = Some("implementer".into());
        explicit.genre = Some("literature".into());
        let plan = PlanOutput { tasks: vec![explicit] };
        let children = materialize(&p, &plan, &[], &[], &genres, WorkspaceContext::default(), OffsetDateTime::now_utc());
        assert_eq!(children[0].genre.as_deref(), Some("literature"));

        // 2. genre 未指定・role が一意に決まる分野に属する。
        let mut by_role = new_task("by-role", vec![]);
        by_role.role = Some("literature-scout".into());
        let plan = PlanOutput { tasks: vec![by_role] };
        let children = materialize(&p, &plan, &[], &[], &genres, WorkspaceContext::default(), OffsetDateTime::now_utc());
        assert_eq!(children[0].genre.as_deref(), Some("literature"));

        // 3. genre も role も無ければ親の分野を継ぐ。
        let plan = PlanOutput { tasks: vec![new_task("neither", vec![])] };
        let children = materialize(&p, &plan, &[], &[], &genres, WorkspaceContext::default(), OffsetDateTime::now_utc());
        assert_eq!(children[0].genre.as_deref(), Some("coding"));
    }

    /// ADR-0028 D3: `tier` / `adapter` / `budget` は タスクの値 > 役割の既定 > 分野の既定
    /// （`default_role` の役割）> 親の値、の順（委譲と同じ規則）。
    #[test]
    fn materialize_applies_role_and_genre_defaults_like_delegation() {
        let p = parent(); // tier = Frontier, adapter = Some("fake")
        let roles = vec![RoleSpec {
            id: "implementer".into(),
            tier: None,
            adapter: Some("codex".into()),
            max_turns: None,
            max_wall_secs: None,
            instructions: None,
        }];
        let genres = vec![genre("coding", Some("lead"), &["lead", "implementer"])];
        let genre_default_role = vec![RoleSpec {
            id: "lead".into(),
            tier: Some(Tier::Cheap),
            adapter: None,
            max_turns: Some(20),
            max_wall_secs: None,
            instructions: None,
        }];
        let mut all_roles = roles;
        all_roles.extend(genre_default_role);
        let mut t = new_task("impl", vec![]);
        t.role = Some("implementer".into());
        t.genre = Some("coding".into());
        let plan = PlanOutput { tasks: vec![t] };
        let children = materialize(&p, &plan, &[], &all_roles, &genres, WorkspaceContext::default(), OffsetDateTime::now_utc());
        // adapter: role(implementer) の既定が優先。
        assert_eq!(children[0].worker_hint.adapter.as_deref(), Some("codex"));
        // tier: role に既定が無いので分野の既定役割（lead）から。
        assert_eq!(children[0].worker_hint.tier, Tier::Cheap);
        assert_eq!(children[0].budget.max_turns, 20);
        assert_eq!(children[0].budget.max_retries, p.budget.max_retries);
    }

    /// ADR-0033 D4（Phase 24）: 計画の `assignee` は子タスクに残り、`role` が無いときだけ
    /// そのノードの分野が既定（tier / adapter / 予算）の解決に効く。
    #[test]
    fn materialize_carries_the_assignee_and_uses_its_genre_only_without_a_role() {
        use crate::org::{OrgKind, OrgNode};
        let now = OffsetDateTime::now_utc();
        let node = |id: &str, genre_id: Option<&str>| OrgNode {
            id: id.into(),
            parent_id: Some("research".into()),
            name: id.into(),
            kind: OrgKind::Section,
            genre: genre_id.map(str::to_string),
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        };
        let org = vec![node("research-survey", Some("literature")), node("research-writing", None)];
        let roles = vec![
            RoleSpec {
                id: "literature-reader".into(),
                tier: Some(Tier::Cheap),
                adapter: Some("paperqa".into()),
                max_turns: Some(5),
                ..RoleSpec::default()
            },
            RoleSpec {
                id: "writer".into(),
                tier: Some(Tier::Frontier),
                adapter: Some("claude-code".into()),
                ..RoleSpec::default()
            },
        ];
        let genres = vec![genre("literature", Some("literature-reader"), &["literature-reader"])];
        let p = parent();

        let mut only_assignee = new_task("調べる", vec![]);
        only_assignee.assignee = Some("research-survey".into());
        let mut with_role = new_task("書く", vec![]);
        with_role.assignee = Some("research-writing".into());
        with_role.role = Some("writer".into());
        let mut unknown = new_task("誰？", vec![]);
        unknown.assignee = Some("nobody".into());
        let plan = PlanOutput { tasks: vec![only_assignee, with_role, unknown] };
        let children = materialize(&p, &plan, &org, &roles, &genres, WorkspaceContext::default(), now);

        assert_eq!(children[0].assignee.as_deref(), Some("research-survey"));
        assert_eq!(children[0].genre.as_deref(), Some("literature"));
        assert_eq!(children[0].worker_hint.adapter.as_deref(), Some("paperqa"));
        assert_eq!(children[0].budget.max_turns, 5);

        assert_eq!(children[1].assignee.as_deref(), Some("research-writing"));
        assert_eq!(children[1].worker_hint.adapter.as_deref(), Some("claude-code"));
        assert_eq!(children[1].worker_hint.tier, Tier::Frontier);

        // 組織に無い id は担当にしない（既定も変えない）。
        assert_eq!(children[2].assignee, None);
        assert_eq!(children[2].genre, p.genre);
    }

    /// Phase 38（ADR-0028 追記）テスト用: ハーネス系（`paperqa`）の `literature` と、ハーネスでない
    /// `coding`、それぞれの担当ノードを持つ組織・役割・分野。
    fn harness_setup() -> (Vec<crate::org::OrgNode>, Vec<RoleSpec>, Vec<GenreSpec>) {
        use crate::org::{OrgKind, OrgNode};
        let now = OffsetDateTime::now_utc();
        let node = |id: &str, genre_id: &str| OrgNode {
            id: id.into(),
            parent_id: Some("research".into()),
            name: id.into(),
            kind: OrgKind::Section,
            genre: Some(genre_id.to_string()),
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        };
        let org = vec![node("research-literature", "literature"), node("coding-poc", "coding")];
        let roles = vec![
            RoleSpec { id: "literature-reader".into(), adapter: Some("paperqa".into()), ..RoleSpec::default() },
            RoleSpec { id: "implementer".into(), adapter: Some("claude-code".into()), ..RoleSpec::default() },
        ];
        let genres = vec![
            GenreSpec {
                output_artifacts: vec![
                    "answer.md: 引用付きの答え".into(),
                    "papers.json: 検索した論文の一覧（コーパス）".into(),
                    "sources.json".into(),
                ],
                ..genre("literature", Some("literature-reader"), &["literature-reader"])
            },
            GenreSpec {
                output_artifacts: vec!["diff".into()],
                ..genre("coding", Some("implementer"), &["implementer"])
            },
        ];
        (org, roles, genres)
    }

    /// Phase 38（ADR-0028 追記。実機のレビュー不合格から）: ハーネス系の担当に「`candidates.json` に
    /// まとめよ」と要求した `artifact_exists` は落ち、`objective` に本当の成果物の名前が注記される。
    /// 一致している条件（`answer.md`）はそのまま残る。
    #[test]
    fn fix_harness_artifacts_drops_unknown_artifact_checks_and_notes_the_real_ones() {
        let (org, roles, genres) = harness_setup();
        let mut t = new_task("候補テーマの抽出", vec![]);
        t.assignee = Some("research-literature".into());
        t.objective = "候補テーマを 3〜5 件、引用付きで candidates.json にまとめよ".into();
        t.acceptance = vec![
            Criterion { text: "candidates.json がある".into(), check: Check::ArtifactExists { name: "candidates.json".into() } },
            Criterion { text: "answer.md がある".into(), check: Check::ArtifactExists { name: "answer.md".into() } },
        ];
        let mut plan = PlanOutput { tasks: vec![t] };
        let warnings = fix_harness_artifacts(&mut plan, &parent(), &org, &roles, &genres);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("candidates.json"), "{}", warnings[0]);
        assert!(warnings[0].contains("answer.md / papers.json / sources.json"), "{}", warnings[0]);
        let fixed = &plan.tasks[0];
        assert_eq!(
            fixed.acceptance,
            vec![Criterion { text: "answer.md がある".into(), check: Check::ArtifactExists { name: "answer.md".into() } }],
            "名前が一致する条件だけが残る"
        );
        assert!(
            fixed.objective.ends_with(
                "（注: この担当の成果物は answer.md / papers.json / sources.json に固定。要求した内容は answer.md の中で述べる）"
            ),
            "{}",
            fixed.objective
        );
        // 直した plan はそのまま検証を通り、子にできる（壊さず直す）。
        validate(&plan, 1, &PlanLimits::default(), &genres).unwrap();
    }

    /// Phase 38: 落とすと受け入れ条件が 0 件になるときは、同じ文をレビュアー条件にして残す
    /// （内容はレビュアーが `answer.md` の中で判定する）。
    #[test]
    fn fix_harness_artifacts_keeps_the_criterion_as_a_reviewer_check_when_nothing_else_remains() {
        let (org, roles, genres) = harness_setup();
        let mut t = new_task("候補テーマの抽出", vec![]);
        t.assignee = Some("research-literature".into());
        t.acceptance = vec![Criterion {
            text: "候補テーマ 3 件が引用付きで書かれている".into(),
            check: Check::ArtifactExists { name: "candidates.json".into() },
        }];
        let mut plan = PlanOutput { tasks: vec![t] };
        let warnings = fix_harness_artifacts(&mut plan, &parent(), &org, &roles, &genres);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(
            plan.tasks[0].acceptance,
            vec![Criterion { text: "候補テーマ 3 件が引用付きで書かれている".into(), check: Check::Reviewer }]
        );
        validate(&plan, 1, &PlanLimits::default(), &genres).unwrap();
    }

    /// Phase 38: ハーネスでない分野（coding = claude-code）と、名前が一致しているハーネスのタスクには
    /// 触らない（`objective` も `acceptance` も 1 バイトも変わらない）。
    #[test]
    fn fix_harness_artifacts_leaves_other_genres_and_matching_names_alone() {
        let (org, roles, genres) = harness_setup();
        let mut coding = new_task("実装", vec![]);
        coding.assignee = Some("coding-poc".into());
        coding.acceptance = vec![Criterion {
            text: "design.md がある".into(),
            check: Check::ArtifactExists { name: "design.md".into() },
        }];
        let mut literature = new_task("調べる", vec![]);
        literature.assignee = Some("research-literature".into());
        literature.acceptance = vec![Criterion {
            text: "answer.md がある".into(),
            check: Check::ArtifactExists { name: "answer.md".into() },
        }];
        let mut plan = PlanOutput { tasks: vec![coding, literature] };
        let before = plan.clone();
        let warnings = fix_harness_artifacts(&mut plan, &parent(), &org, &roles, &genres);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(plan, before);
    }

    /// ADR-0028 D3: 知らない `genre`、または `genre` + `role` の不整合は Plan の失敗になる。
    #[test]
    fn validate_rejects_unknown_genre_and_role_not_in_genre() {
        let genres = vec![genre("coding", Some("implementer"), &["lead", "implementer"])];

        let mut unknown = new_task("a", vec![]);
        unknown.genre = Some("literature".into());
        let plan = PlanOutput { tasks: vec![unknown] };
        assert_eq!(
            validate(&plan, 1, &PlanLimits::default(), &genres),
            Err(PlanError::UnknownGenre { index: 0, genre: "literature".into() })
        );

        let mut mismatched = new_task("b", vec![]);
        mismatched.genre = Some("coding".into());
        mismatched.role = Some("literature-scout".into());
        let plan = PlanOutput { tasks: vec![mismatched] };
        assert_eq!(
            validate(&plan, 1, &PlanLimits::default(), &genres),
            Err(PlanError::RoleNotInGenre {
                index: 0,
                role: "literature-scout".into(),
                genre: "coding".into(),
            })
        );

        // 分野を使わない設定（`genres` が空）でも `genre` を指定すれば同じくエラー。
        let mut no_config = new_task("c", vec![]);
        no_config.genre = Some("coding".into());
        let plan = PlanOutput { tasks: vec![no_config] };
        assert_eq!(
            validate(&plan, 1, &PlanLimits::default(), &[]),
            Err(PlanError::UnknownGenre { index: 0, genre: "coding".into() })
        );

        // parse_and_validate 経由でも同じ（Plan run の暗黙条件から見えるエラー文言）。
        let json = r#"{"tasks":[{"title":"t","objective":"o","acceptance":[{"text":"c","check":{"type":"human"}}],"genre":"literature"}]}"#;
        let err = parse_and_validate(json, 1, &PlanLimits::default(), &genres).unwrap_err();
        assert!(err.contains("literature"), "{err}");
    }

    /// ADR-0039 D2: 分解した子の作業場所は **明示 > 案件 > 親** の 3 段で決まる。
    #[test]
    fn child_workspace_is_explicit_then_project_then_parent() {
        let p = parent();
        let project = WorkspaceSpec::Local {
            path: PathBuf::from("/home/rmaeda/workspace/rust/pluvio-poc"), mode: None,
        };
        let explicit = WorkspaceSpec::Local {
            path: PathBuf::from("/home/rmaeda/workspace/rust/other"), mode: None,
        };

        // 案件も明示も無ければ従来どおり親を継ぐ。
        let plan = PlanOutput { tasks: vec![new_task("a", vec![])] };
        let children = materialize(&p, &plan, &[], &[], &[], WorkspaceContext::default(), OffsetDateTime::now_utc());
        assert_eq!(children[0].workspace, p.workspace);

        // 案件の作業場所は親より強い。
        let ws = WorkspaceContext { project: Some(&project), home: None };
        let children = materialize(&p, &plan, &[], &[], &[], ws, OffsetDateTime::now_utc());
        assert_eq!(children[0].workspace, project);

        // タスクが明示すれば案件より強い。
        let mut explicit_task = new_task("b", vec![]);
        explicit_task.workspace = Some(explicit.clone());
        let plan = PlanOutput {
            tasks: vec![new_task("a", vec![]), explicit_task],
        };
        let children = materialize(&p, &plan, &[], &[], &[], ws, OffsetDateTime::now_utc());
        assert_eq!(children[0].workspace, project);
        assert_eq!(children[1].workspace, explicit);
    }

    /// ADR-0039 D2: 案件が Remote なら子も Remote（従来の ADR-0018 経路に乗る）。D5: `~` は展開する。
    #[test]
    fn a_remote_project_makes_remote_children_and_tilde_is_expanded() {
        let p = parent();
        let remote = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::from("/work/NBB/rmaeda/workspace/rust/benchfs"),
        };
        let plan = PlanOutput { tasks: vec![new_task("a", vec![])] };
        let home = PathBuf::from("/home/rmaeda");
        let ws = WorkspaceContext { project: Some(&remote), home: Some(&home) };
        let children = materialize(&p, &plan, &[], &[], &[], ws, OffsetDateTime::now_utc());
        assert_eq!(children[0].workspace, remote, "Remote の path はクラスタ側なので触らない");

        let tilde = WorkspaceSpec::Local {
            path: PathBuf::from("~/workspace/rust/pluvio-poc"), mode: None,
        };
        let ws = WorkspaceContext { project: Some(&tilde), home: Some(&home) };
        let children = materialize(&p, &plan, &[], &[], &[], ws, OffsetDateTime::now_utc());
        assert_eq!(
            children[0].workspace,
            WorkspaceSpec::Local {
                path: PathBuf::from("/home/rmaeda/workspace/rust/pluvio-poc"), mode: None
            }
        );
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
