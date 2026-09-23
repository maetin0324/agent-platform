//! `approve` / `reject` / `answer` / `cancel` の判断と検証 — DESIGN.md §5.9 / ADR-0002 D4 /
//! ADR-0004 D1-D3 / ADR-0010 D3/D4（ADR-0013 D7）。
//!
//! 元は `celerisctl` の `commands/gate.rs` と `commands/cancel.rs` にあったロジックをそのまま移した。
//! 状態変更は `TaskStore::apply_transition` だけで行う。`expected` が `Some` で現在の `status` と
//! 違えば、遷移を試みずに `OpsError::Conflict` を返す。

use task_core::approval::Decision;
use task_core::{Event, Status, TaskId, TaskKind, TaskStore, Trigger};
use time::OffsetDateTime;

use crate::derive::latest_question;
use crate::error::OpsError;
use crate::view::{TaskRef, task_ref};

/// `events_since` を読むときの「大きめの limit」（`docs/gui/api.md` §5.7）。伝播は同一トランザクション
/// 内で完結するので、通常はこの上限にかからない。
const CASCADE_EVENTS_LIMIT: usize = 100_000;

/// 状態変更の結果（承認・却下・回答・取り消し共通）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct TransitionResult {
    pub id: TaskId,
    pub from: Status,
    pub to: Status,
    pub reason: String,
    /// この遷移の伝播で `cancelled` になった、対象タスク以外のタスク（`docs/gui/api.md` §5.7）。
    #[serde(default)]
    pub cascaded: Vec<TaskRef>,
}

/// `since_id`（遷移前の `latest_event_id()`）より後に追記されたイベントのうち、`subject` 以外の
/// タスクに付いた `Transitioned{to: Cancelled, reason: "cancel" | "dependency_failed"}` を
/// `TaskRef` にして返す（`docs/gui/api.md` §5.7）。
fn collect_cascaded(
    store: &dyn TaskStore,
    subject: TaskId,
    since_id: u64,
) -> Result<Vec<TaskRef>, OpsError> {
    let rows = store.events_since(since_id, CASCADE_EVENTS_LIMIT)?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        if row.task_id == subject {
            continue;
        }
        let Event::Transitioned { to, reason, .. } = &row.event else {
            continue;
        };
        if *to != Status::Cancelled || (reason != "cancel" && reason != "dependency_failed") {
            continue;
        }
        if !seen.insert(row.task_id) {
            continue;
        }
        if let Some(t) = store.get(row.task_id)? {
            out.push(task_ref(&t));
        }
    }
    Ok(out)
}

fn check_expected(actual: Status, expected: Option<Status>) -> Result<(), OpsError> {
    match expected {
        Some(exp) if exp != actual => Err(OpsError::Conflict {
            expected: exp,
            actual,
        }),
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
    approve_as(store, id, "human", note, expected)
}

/// ADR-0056 Phase 101: `approve` と同じ判断・遷移だが、`Event::ApprovalDecided.by` を渡せる。
/// MCP の `task_approve` は `mcp:<client_id>` を渡す。`approve`（GUI/HTTP の `POST
/// /tasks/{id}/approve`）は `"human"` のまま（挙動を変えない）。
pub fn approve_as(
    store: &dyn TaskStore,
    id: TaskId,
    by: &str,
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
                by: by.to_string(),
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
    let since_id = store.latest_event_id()?;
    let outcome = store.apply_transition(id, trigger, extra_event)?;
    let cascaded = collect_cascaded(store, id, since_id)?;
    Ok(TransitionResult {
        id,
        from,
        to: outcome.next,
        reason: outcome.reason.to_string(),
        cascaded,
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
    reject_as(store, id, "human", note, expected)
}

/// ADR-0056 Phase 101: `reject` と同じ判断・遷移だが、`Event::ApprovalDecided.by` を渡せる。
/// MCP の `task_reject` は `mcp:<client_id>` を渡す。`reject`（GUI/HTTP の `POST
/// /tasks/{id}/reject`）は `"human"` のまま（挙動を変えない）。
pub fn reject_as(
    store: &dyn TaskStore,
    id: TaskId,
    by: &str,
    note: Option<String>,
    expected: Option<Status>,
) -> Result<TransitionResult, OpsError> {
    let task = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    check_expected(task.status, expected)?;

    if task.kind == TaskKind::Approval && task.status == Status::Ready {
        let from = task.status;
        let since_id = store.latest_event_id()?;
        let outcome = store.apply_transition(
            id,
            Trigger::Reject,
            Some(Event::ApprovalDecided {
                by: by.to_string(),
                approved: false,
                note,
            }),
        )?;
        let cascaded = collect_cascaded(store, id, since_id)?;
        Ok(TransitionResult {
            id,
            from,
            to: outcome.next,
            reason: outcome.reason.to_string(),
            cascaded,
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
    let since_id = store.latest_event_id()?;
    let outcome = store.apply_transition(
        id,
        Trigger::Answer,
        Some(Event::Answered {
            question,
            answer: answer.clone(),
        }),
    )?;
    let cascaded = collect_cascaded(store, id, since_id)?;
    // GUI 監査 H2: `POST /tasks/{id}/answer` で答えたときも、そのタスクの未決の `approvals` を
    // `once` + 同じ答えで決定済みにする（決定的。無ければ何もしない）。`POST /approvals/{id}/decide`
    // は既にこの経路（`gate::answer`）に相乗りしているので、これで両方向が揃う（`task_ops::approval`）。
    settle_pending_approvals(store, id, &answer)?;
    Ok(TransitionResult {
        id,
        from,
        to: outcome.next,
        reason: outcome.reason.to_string(),
        cascaded,
    })
}

/// `task_id` に紐づく未決の `approvals` を、渡された `answer` で `once` に決定する。
/// `task_ops::approval::decide` は呼ばない（そちらは決定のたびに `gate::answer` を呼び直すため、
/// ここから呼ぶと循環する。ストアへの書き込みだけをここで完結させる）。
pub(crate) fn settle_pending_approvals(
    store: &dyn TaskStore,
    task_id: TaskId,
    answer: &str,
) -> Result<(), OpsError> {
    let now = OffsetDateTime::now_utc();
    let pending = store.approval_list(Some(true), None, None)?;
    for approval in pending.into_iter().filter(|a| a.task_id == Some(task_id)) {
        store.approval_decide(approval.id, Decision::Once, Some(answer.to_string()), now)?;
    }
    Ok(())
}

/// 非終端（`draft/ready/running/blocked/reviewing`）のタスクだけを `Trigger::Cancel` で
/// `cancelled` にする。終端はエラーにし、状態は変えない（ADR-0010 D1, P-4）。子・後続への
/// 取り消し伝播は `TaskStore::apply_transition` がストア側の同一トランザクションで行う。
pub fn cancel(
    store: &dyn TaskStore,
    id: TaskId,
    expected: Option<Status>,
) -> Result<TransitionResult, OpsError> {
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
    let since_id = store.latest_event_id()?;
    let outcome = store.apply_transition(id, Trigger::Cancel, None)?;
    let cascaded = collect_cascaded(store, id, since_id)?;
    Ok(TransitionResult {
        id,
        from,
        to: outcome.next,
        reason: outcome.reason.to_string(),
        cascaded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{
        ArtifactRef, Budget, Check, Criterion, SqliteStore, Task, Tier, WorkerHint, WorkspaceSpec,
    };
    use time::OffsetDateTime;

    fn sample_task(kind: TaskKind, status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
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
            declared: true,
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
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
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
        assert!(
            events
                .iter()
                .any(|(_, e)| matches!(e, Event::ApprovalDecided { approved: true, .. }))
        );
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
        assert_eq!(
            fetched.status,
            Status::Draft,
            "no transition should have happened"
        );
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

    /// GUI 監査 H2（Phase 29）: `answer` は、そのタスクの未決の `approvals` を `once` + 同じ答えで
    /// 決定済みにする（`GET /approvals?pending=true` からそのタスクの行が消える）。
    /// 他のタスクの未決の approvals は触らない。approvals が無ければ何もしない（エラーにならない）。
    #[test]
    fn answer_settles_the_tasks_pending_approvals_as_once_with_the_same_answer() {
        use task_core::approval::{Approval, ApprovalId, ApprovalStore};

        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&task).expect("insert");
        let other = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&other).expect("insert other");

        let now = OffsetDateTime::now_utc();
        let mine = Approval {
            id: ApprovalId::new(),
            project_id: None,
            node_id: "secretary".into(),
            task_id: Some(task.id),
            question: "どのクラスタを使いますか".into(),
            decision: None,
            answer: None,
            created_at: now,
            decided_at: None,
        };
        let unrelated = Approval {
            id: ApprovalId::new(),
            project_id: None,
            node_id: "secretary".into(),
            task_id: Some(other.id),
            question: "別の質問".into(),
            decision: None,
            answer: None,
            created_at: now,
            decided_at: None,
        };
        store.approval_append(&mine).expect("append mine");
        store.approval_append(&unrelated).expect("append unrelated");

        answer(&store, task.id, "pegasus".to_string(), None).expect("answer");

        let decided = store.approval_get(mine.id).expect("get").expect("some");
        assert_eq!(decided.decision, Some(Decision::Once));
        assert_eq!(decided.answer.as_deref(), Some("pegasus"));
        assert!(decided.decided_at.is_some());

        // 他のタスクの approval は手つかず。
        let untouched = store
            .approval_get(unrelated.id)
            .expect("get")
            .expect("some");
        assert!(untouched.is_pending());

        // approvals が無いタスクへの answer はエラーにならない。
        let plain = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&plain).expect("insert plain");
        assert!(answer(&store, plain.id, "x".to_string(), None).is_ok());
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
                    role: None,
                    metrics: None,
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
        assert_eq!(
            fetched.status,
            Status::Ready,
            "no transition should have happened"
        );
    }

    // ---- cascaded (docs/gui/api.md §5.7) ----

    #[test]
    fn reject_approval_cascades_cancel_to_its_own_children() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let approval = sample_task(TaskKind::Approval, Status::Ready);
        store.insert(&approval).expect("insert approval");

        let mut child = sample_task(TaskKind::Execute, Status::Draft);
        child.parent_id = Some(approval.id);
        store.insert(&child).expect("insert child");

        let result = reject(&store, approval.id, None, None).expect("reject");
        assert_eq!(result.to, Status::Failed);
        assert_eq!(result.cascaded.len(), 1);
        assert_eq!(result.cascaded[0].id, child.id);
        assert_eq!(result.cascaded[0].status, Status::Cancelled);

        let fetched_child = store.get(child.id).expect("get").expect("some");
        assert_eq!(fetched_child.status, Status::Cancelled);
    }

    #[test]
    fn cancel_cascades_dependency_failed_to_dependents() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let upstream = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&upstream).expect("insert upstream");

        let mut downstream = sample_task(TaskKind::Execute, Status::Draft);
        downstream.depends_on = vec![upstream.id];
        store.insert(&downstream).expect("insert downstream");

        let result = cancel(&store, upstream.id, None).expect("cancel");
        assert_eq!(result.to, Status::Cancelled);
        assert_eq!(result.cascaded.len(), 1);
        assert_eq!(result.cascaded[0].id, downstream.id);

        let fetched_downstream = store.get(downstream.id).expect("get").expect("some");
        assert_eq!(fetched_downstream.status, Status::Cancelled);
    }

    #[test]
    fn cancel_without_children_or_dependents_has_empty_cascaded() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Ready);
        store.insert(&task).expect("insert");

        let result = cancel(&store, task.id, None).expect("cancel");
        assert!(result.cascaded.is_empty());
    }

    #[test]
    fn approve_without_cascading_effects_has_empty_cascaded() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Draft);
        store.insert(&task).expect("insert");

        let result = approve(&store, task.id, None, None).expect("approve");
        assert!(result.cascaded.is_empty());
    }
}
