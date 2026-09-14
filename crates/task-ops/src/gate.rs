//! `approve` / `reject` / `answer` / `cancel` の判断と検証 — DESIGN.md §5.9 / ADR-0002 D4 /
//! ADR-0004 D1-D3 / ADR-0010 D3/D4（ADR-0013 D7）。
//!
//! 元は `taskctl` の `commands/gate.rs` と `commands/cancel.rs` にあったロジックをそのまま移した。
//! 状態変更は `TaskStore::apply_transition` だけで行う。`expected` が `Some` で現在の `status` と
//! 違えば、遷移を試みずに `OpsError::Conflict` を返す。

use task_core::{Event, Status, TaskId, TaskKind, TaskStore, Trigger};

use crate::derive::latest_question;
use crate::error::OpsError;

/// 状態変更の結果（承認・却下・回答・取り消し共通）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TransitionResult {
    pub id: TaskId,
    pub from: Status,
    pub to: Status,
    pub reason: String,
}

fn check_expected(actual: Status, expected: Option<Status>) -> Result<(), OpsError> {
    match expected {
        Some(exp) if exp != actual => Err(OpsError::Conflict { expected: exp, actual }),
        _ => Ok(()),
    }
}

/// `status == Draft`（kind 不問）は `Trigger::Accept`、`kind == Approval && status == Ready` は
/// `Trigger::Approve` で `Event::ApprovalDecided` を同一トランザクションに追記する。
pub fn approve(
    store: &dyn TaskStore,
    id: TaskId,
    note: Option<String>,
    expected: Option<Status>,
) -> Result<TransitionResult, OpsError> {
    let task = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    check_expected(task.status, expected)?;

    let (trigger, extra_event) = if task.status == Status::Draft {
        (Trigger::Accept, None)
    } else if task.kind == TaskKind::Approval && task.status == Status::Ready {
        (
            Trigger::Approve,
            Some(Event::ApprovalDecided {
                by: "human".to_string(),
                approved: true,
                note,
            }),
        )
    } else {
        return Err(OpsError::InvalidState {
            id,
            context: format!("kind={:?}, status={:?}", task.kind, task.status),
            action: "approved".to_string(),
        });
    };

    let from = task.status;
    let outcome = store.apply_transition(id, trigger, extra_event)?;
    Ok(TransitionResult {
        id,
        from,
        to: outcome.next,
        reason: outcome.reason.to_string(),
    })
}

/// `kind == Approval && status == Ready` のみ許可する（`draft` への `reject` は
/// 成功させない。ADR-0004 D2: P-5 は不採用。draft の取り消しは `cancel` を使う）。
pub fn reject(
    store: &dyn TaskStore,
    id: TaskId,
    note: Option<String>,
    expected: Option<Status>,
) -> Result<TransitionResult, OpsError> {
    let task = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    check_expected(task.status, expected)?;

    if task.kind == TaskKind::Approval && task.status == Status::Ready {
        let from = task.status;
        let outcome = store.apply_transition(
            id,
            Trigger::Reject,
            Some(Event::ApprovalDecided {
                by: "human".to_string(),
                approved: false,
                note,
            }),
        )?;
        Ok(TransitionResult {
            id,
            from,
            to: outcome.next,
            reason: outcome.reason.to_string(),
        })
    } else {
        Err(OpsError::InvalidState {
            id,
            context: format!("kind={:?}, status={:?}", task.kind, task.status),
            action: "rejected".to_string(),
        })
    }
}

/// `Blocked` タスクのみ `Trigger::Answer` を適用し、回答テキストを
/// `Event::Answered{question, answer}` として同一トランザクションで永続化する（ADR-0010 D3, P-10）。
/// `question` は直近の `WorkerFinished{outcome:"question: ..."}` から取る（無ければ空文字列）。
pub fn answer(
    store: &dyn TaskStore,
    id: TaskId,
    answer: String,
    expected: Option<Status>,
) -> Result<TransitionResult, OpsError> {
    let task = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    check_expected(task.status, expected)?;

    if task.status != Status::Blocked {
        return Err(OpsError::InvalidState {
            id,
            context: format!("status={:?}", task.status),
            action: "answered; only blocked tasks accept an answer".to_string(),
        });
    }

    let question = latest_question(&store.events_for(id)?);
    let from = task.status;
    let outcome = store.apply_transition(
        id,
        Trigger::Answer,
        Some(Event::Answered { question, answer }),
    )?;
    Ok(TransitionResult {
        id,
        from,
        to: outcome.next,
        reason: outcome.reason.to_string(),
    })
}

/// 非終端（`draft/ready/running/blocked/reviewing`）のタスクだけを `Trigger::Cancel` で
/// `cancelled` にする。終端はエラーにし、状態は変えない（ADR-0010 D1, P-4）。子・後続への
/// 取り消し伝播は `TaskStore::apply_transition` がストア側の同一トランザクションで行う。
pub fn cancel(store: &dyn TaskStore, id: TaskId, expected: Option<Status>) -> Result<TransitionResult, OpsError> {
    let task = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    check_expected(task.status, expected)?;

    if task.status.is_terminal() {
        return Err(OpsError::InvalidState {
            id,
            context: format!("status={:?}", task.status),
            action: "cancelled".to_string(),
        });
    }

    let from = task.status;
    let outcome = store.apply_transition(id, Trigger::Cancel, None)?;
    Ok(TransitionResult {
        id,
        from,
        to: outcome.next,
        reason: outcome.reason.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{ArtifactRef, Budget, Check, Criterion, SqliteStore, Task, Tier, WorkerHint, WorkspaceSpec};
    use time::OffsetDateTime;

    fn sample_task(kind: TaskKind, status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind,
            title: "do something".to_string(),
            objective: "make it work".to_string(),
            acceptance: vec![Criterion {
                text: "tests pass".to_string(),
                check: Check::Command {
                    cmd: "true".to_string(),
                    expect_exit: 0,
                },
            }],
            inputs: vec![ArtifactRef {
                name: "spec".to_string(),
                path: "spec.md".to_string(),
                sha256: "abc".to_string(),
                kind: "doc".to_string(),
            }],
            depends_on: vec![],
            status,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "/tmp/workspace".into(),
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
        }
    }

    #[test]
    fn approve_moves_draft_to_ready() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Draft);
        store.insert(&task).expect("insert");

        let result = approve(&store, task.id, None, None).expect("approve");
        assert_eq!(result.from, Status::Draft);
        assert_eq!(result.to, Status::Ready);

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Ready);
    }

    #[test]
    fn approve_on_approval_ready_moves_to_done_and_records_event() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Approval, Status::Ready);
        store.insert(&task).expect("insert");

        approve(&store, task.id, Some("looks good".to_string()), None).expect("approve");

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Done);

        let events = store.events_for(task.id).expect("events_for");
        assert!(events.iter().any(|(_, e)| matches!(
            e,
            Event::ApprovalDecided {
                approved: true,
                ..
            }
        )));
    }

    #[test]
    fn approve_with_matching_expected_succeeds() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Draft);
        store.insert(&task).expect("insert");

        let result = approve(&store, task.id, None, Some(Status::Draft)).expect("approve");
        assert_eq!(result.to, Status::Ready);
    }

    #[test]
    fn approve_with_mismatched_expected_returns_conflict_without_transitioning() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Draft);
        store.insert(&task).expect("insert");

        let result = approve(&store, task.id, None, Some(Status::Ready));
        assert!(matches!(
            result,
            Err(OpsError::Conflict {
                expected: Status::Ready,
                actual: Status::Draft
            })
        ));

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Draft, "no transition should have happened");
    }

    #[test]
    fn reject_on_approval_ready_moves_to_failed_and_records_event() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Approval, Status::Ready);
        store.insert(&task).expect("insert");

        reject(&store, task.id, Some("not good enough".to_string()), None).expect("reject");

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Failed);

        let events = store.events_for(task.id).expect("events_for");
        assert!(events.iter().any(|(_, e)| matches!(
            e,
            Event::ApprovalDecided {
                approved: false,
                ..
            }
        )));
    }

    #[test]
    fn reject_on_draft_is_an_error_and_does_not_change_status() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Draft);
        store.insert(&task).expect("insert");

        let result = reject(&store, task.id, None, None);
        assert!(result.is_err());

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Draft);
    }

    #[test]
    fn reject_with_mismatched_expected_returns_conflict() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Approval, Status::Ready);
        store.insert(&task).expect("insert");

        let result = reject(&store, task.id, None, Some(Status::Draft));
        assert!(matches!(
            result,
            Err(OpsError::Conflict {
                expected: Status::Draft,
                actual: Status::Ready
            })
        ));
    }

    #[test]
    fn answer_on_blocked_moves_to_ready() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&task).expect("insert");

        let result = answer(&store, task.id, "the answer".to_string(), None).expect("answer");
        assert_eq!(result.to, Status::Ready);

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Ready);
    }

    /// ADR-0010 D3（P-10）: `answer` は `Event::Answered{question, answer}` を
    /// `Trigger::Answer` の `Event::Transitioned` と同一トランザクションで、その直後に
    /// 追記する。`question` は直近の `WorkerFinished{outcome:"question: ..."}` から取る。
    #[test]
    fn answer_persists_answered_event_with_question_from_worker_finished() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::WorkerFinished {
                    run_id: "run-1".to_string(),
                    outcome: "question: which version?".to_string(),
                    usage: None,
                },
            )
            .expect("append worker finished");

        answer(&store, task.id, "use v2".to_string(), None).expect("answer");

        let events = store.events_for(task.id).expect("events_for");
        let tail = &events[events.len() - 2..];
        assert!(matches!(
            &tail[0].1,
            Event::Transitioned { reason, .. } if reason == "answer"
        ));
        assert_eq!(
            tail[1].1,
            Event::Answered {
                question: "which version?".to_string(),
                answer: "use v2".to_string(),
            }
        );
    }

    /// `WorkerFinished` が無ければ `question` は空文字列になる。
    #[test]
    fn answer_without_worker_finished_uses_empty_question() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&task).expect("insert");

        answer(&store, task.id, "the answer".to_string(), None).expect("answer");

        let events = store.events_for(task.id).expect("events_for");
        let last = &events.last().expect("some event").1;
        assert_eq!(
            *last,
            Event::Answered {
                question: String::new(),
                answer: "the answer".to_string(),
            }
        );
    }

    #[test]
    fn answer_on_ready_task_is_an_error() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&task).expect("insert");

        let result = answer(&store, task.id, "irrelevant".to_string(), None);
        assert!(result.is_err());
    }

    #[test]
    fn cancels_draft_task() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Draft);
        store.insert(&task).expect("insert");

        let result = cancel(&store, task.id, None).expect("cancel");
        assert_eq!(result.to, Status::Cancelled);

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Cancelled);
    }

    #[test]
    fn cancels_ready_task() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&task).expect("insert");

        cancel(&store, task.id, None).expect("cancel");

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Cancelled);
    }

    #[test]
    fn cancels_running_task_acquired_via_lease() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&task).expect("insert");
        let acquired = store
            .acquire_lease(task.id, "run-1", std::time::Duration::from_secs(60))
            .expect("acquire_lease");
        assert!(acquired);

        cancel(&store, task.id, None).expect("cancel");

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Cancelled);
        assert!(fetched.lease.is_none());
    }

    #[test]
    fn cancel_on_terminal_task_errors_and_leaves_status_unchanged() {
        let store = SqliteStore::open_in_memory().expect("open store");
        for status in [Status::Done, Status::Failed, Status::Cancelled] {
            let task = sample_task(TaskKind::Execute, status);
            store.insert(&task).expect("insert");

            let result = cancel(&store, task.id, None);
            assert!(result.is_err(), "cancel of {status:?} should fail");

            let fetched = store.get(task.id).expect("get").expect("some");
            assert_eq!(fetched.status, status);
        }
    }

    #[test]
    fn cancel_on_missing_task_errors() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let missing_id = TaskId::new();

        let result = cancel(&store, missing_id, None);
        assert!(matches!(result, Err(OpsError::NotFound(id)) if id == missing_id));
    }

    #[test]
    fn cancel_with_mismatched_expected_returns_conflict_without_transitioning() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&task).expect("insert");

        let result = cancel(&store, task.id, Some(Status::Running));
        assert!(matches!(
            result,
            Err(OpsError::Conflict {
                expected: Status::Running,
                actual: Status::Ready
            })
        ));

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Ready, "no transition should have happened");
    }
}
