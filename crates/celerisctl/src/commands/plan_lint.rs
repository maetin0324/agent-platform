//! `celerisctl plan-lint`（ADR-0067 D5。Phase 111）。
//!
//! DB 上の `draft` / `ready` のタスクの受け入れ条件を、計画時の検証（ADR-0067 D2:
//! `task_core::validate_human_checks_have_deliverable`）と同じ規則で点検し、違反を一覧する。
//! **読み取り専用**（直しはしない。`ready_tasks`/`list` と同じ `TaskStore` の読み取りだけで、
//! LLM 呼び出しも書き込みも無い）。

use std::process::ExitCode;

use task_core::{Status, TaskStore, validate_human_checks_have_deliverable};

use crate::error::CliError;
use crate::outln;

/// 点検対象のタスク 1 件の違反。
pub struct Violation {
    pub task_id: task_core::TaskId,
    pub title: String,
    pub status: Status,
    pub reason: String,
}

/// DB 上の `draft`/`ready` タスクを D2 の規則で点検する（純粋な読み取り。判断は
/// `validate_human_checks_have_deliverable` に委ねる）。
pub fn lint(store: &dyn TaskStore) -> Result<Vec<Violation>, CliError> {
    let mut violations = Vec::new();
    for status in [Status::Draft, Status::Ready] {
        for task in store.list(Some(status))? {
            if let Err(reason) = validate_human_checks_have_deliverable(&task.acceptance) {
                violations.push(Violation {
                    task_id: task.id,
                    title: task.title,
                    status: task.status,
                    reason,
                });
            }
        }
    }
    violations.sort_by_key(|v| v.task_id);
    Ok(violations)
}

pub fn run(store: &dyn TaskStore) -> Result<ExitCode, CliError> {
    let violations = lint(store)?;
    if violations.is_empty() {
        outln!(
            "違反はありません（draft/ready のタスクの human チェックには全て artifacts か知識ベースの参照が付いています）。"
        );
        return Ok(ExitCode::SUCCESS);
    }
    for v in &violations {
        outln!("{} [{:?}] {:?}: {}", v.task_id, v.status, v.title, v.reason);
    }
    outln!("{} 件の違反。", violations.len());
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{
        Budget, Check, Criterion, SqliteStore, Task, TaskId, TaskKind, Tier, WorkerHint,
        WorkspaceSpec,
    };
    use time::OffsetDateTime;

    fn base_task(status: Status, acceptance: Vec<Criterion>) -> Task {
        let id = TaskId::new();
        let now = OffsetDateTime::now_utc();
        Task {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id,
            parent_id: None,
            kind: TaskKind::Execute,
            title: format!("task {id}"),
            objective: "do it".into(),
            acceptance,
            inputs: vec![],
            depends_on: vec![],
            status,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: id.to_string().into(),
                mode: None,
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
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            labels: Vec::new(),
            category: Default::default(),
            conversation: None,
        }
    }

    #[test]
    fn lints_draft_and_ready_tasks_but_not_other_statuses() {
        let store = SqliteStore::open_in_memory().expect("open");
        let bad_draft = base_task(
            Status::Draft,
            vec![Criterion {
                text: "a human looks".into(),
                check: Check::Human,
            }],
        );
        let bad_ready = base_task(
            Status::Ready,
            vec![Criterion {
                text: "a human looks".into(),
                check: Check::Human,
            }],
        );
        let ok_task = base_task(
            Status::Draft,
            vec![
                Criterion {
                    text: "a human looks".into(),
                    check: Check::Human,
                },
                Criterion {
                    text: "artifact exists".into(),
                    check: Check::ArtifactExists {
                        name: "result.md".into(),
                    },
                },
            ],
        );
        // done でも同じ違反を持つが、対象外（draft/ready だけ）。
        let done_bad = base_task(
            Status::Done,
            vec![Criterion {
                text: "a human looks".into(),
                check: Check::Human,
            }],
        );
        for t in [&bad_draft, &bad_ready, &ok_task, &done_bad] {
            store.insert(t).expect("insert");
        }

        let violations = lint(&store).expect("lint");
        let ids: Vec<TaskId> = violations.iter().map(|v| v.task_id).collect();
        assert!(ids.contains(&bad_draft.id));
        assert!(ids.contains(&bad_ready.id));
        assert!(!ids.contains(&ok_task.id));
        assert!(!ids.contains(&done_bad.id));
        assert_eq!(violations.len(), 2);
        for v in &violations {
            assert!(v.reason.contains("人が確認する成果物"));
        }
    }

    #[test]
    fn no_violations_when_every_human_check_has_a_deliverable() {
        let store = SqliteStore::open_in_memory().expect("open");
        let ok_task = base_task(
            Status::Ready,
            vec![
                Criterion {
                    text: "a human looks".into(),
                    check: Check::Human,
                },
                Criterion {
                    text: "kb page".into(),
                    check: Check::KnowledgePage {
                        path: "projects/x/decision.md".into(),
                    },
                },
            ],
        );
        store.insert(&ok_task).expect("insert");
        assert!(lint(&store).expect("lint").is_empty());
    }
}
