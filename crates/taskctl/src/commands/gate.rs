//! `taskctl approve` / `taskctl reject` / `taskctl answer` — DESIGN.md §5.9 / ADR-0002 D4 /
//! ADR-0004 D1-D3 / ADR-0010 D3。
//!
//! 3コマンドとも状態変更は `TaskStore::apply_transition` だけで行う（`store.get` の後に
//! 手動で `UPDATE` はしない）。`approve`/`reject` の遷移写像は ADR-0002 D4 のとおり:
//! `status == Draft`（kind 不問）は `Trigger::Accept`、`kind == Approval && status == Ready` は
//! `Trigger::Approve`/`Trigger::Reject` で `Event::ApprovalDecided` を同一トランザクションに追記する。
//! `draft` への `reject` は成功させない（ADR-0004 D2: P-5 は不採用。draft の取り消しは
//! `cancel` を使う）。`answer` は `Blocked` タスクのみ `Trigger::Answer` を適用し、回答テキストは
//! `Event::Answered{question, answer}` として `Trigger::Answer` と同一トランザクションで永続化する
//! （ADR-0010 D3, P-10）。`question` はそのタスクの直近の `WorkerFinished{outcome}` のうち
//! `"question: "` で始まるものから接頭辞を除いて取り出す（無ければ空文字列）。

use std::process::ExitCode;

use clap::Args;
use task_core::{Event, Status, TaskKind, TaskStore, Trigger};

use crate::error::CliError;
use crate::outln;

#[derive(Args, Debug)]
pub struct ApproveArgs {
    pub id: String,

    #[arg(long)]
    pub note: Option<String>,
}

#[derive(Args, Debug)]
pub struct RejectArgs {
    pub id: String,

    #[arg(long)]
    pub note: Option<String>,
}

#[derive(Args, Debug)]
pub struct AnswerArgs {
    pub id: String,

    pub answer: String,
}

pub fn run_approve(store: &dyn TaskStore, args: ApproveArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;
    let task = store
        .get(id)?
        .ok_or_else(|| CliError::msg(format!("task not found: {id}")))?;

    let (trigger, extra_event) = if task.status == Status::Draft {
        (Trigger::Accept, None)
    } else if task.kind == TaskKind::Approval && task.status == Status::Ready {
        (
            Trigger::Approve,
            Some(Event::ApprovalDecided {
                by: "human".to_string(),
                approved: true,
                note: args.note.clone(),
            }),
        )
    } else {
        return Err(CliError::msg(format!(
            "task {id} (kind={:?}, status={:?}) cannot be approved",
            task.kind, task.status
        )));
    };

    let outcome = store.apply_transition(id, trigger, extra_event)?;
    outln!("{:?}", outcome.next);
    Ok(ExitCode::SUCCESS)
}

pub fn run_reject(store: &dyn TaskStore, args: RejectArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;
    let task = store
        .get(id)?
        .ok_or_else(|| CliError::msg(format!("task not found: {id}")))?;

    if task.kind == TaskKind::Approval && task.status == Status::Ready {
        let outcome = store.apply_transition(
            id,
            Trigger::Reject,
            Some(Event::ApprovalDecided {
                by: "human".to_string(),
                approved: false,
                note: args.note.clone(),
            }),
        )?;
        outln!("{:?}", outcome.next);
        Ok(ExitCode::SUCCESS)
    } else {
        Err(CliError::msg(format!(
            "task {id} (kind={:?}, status={:?}) cannot be rejected",
            task.kind, task.status
        )))
    }
}

/// 直近の `WorkerFinished{outcome}` のうち `"question: "` で始まるものから、接頭辞を
/// 除いた質問文を取り出す（ADR-0010 D3）。`events_for` を後ろから見て最初に見つかった
/// ものを使う。無ければ空文字列。
fn latest_question(events: &[(u64, Event)]) -> String {
    events
        .iter()
        .rev()
        .find_map(|(_, event)| match event {
            Event::WorkerFinished { outcome, .. } => {
                outcome.strip_prefix("question: ").map(str::to_string)
            }
            _ => None,
        })
        .unwrap_or_default()
}

pub fn run_answer(store: &dyn TaskStore, args: AnswerArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;
    let task = store
        .get(id)?
        .ok_or_else(|| CliError::msg(format!("task not found: {id}")))?;

    if task.status != Status::Blocked {
        return Err(CliError::msg(format!(
            "task {id} (status={:?}) cannot be answered; only blocked tasks accept an answer",
            task.status
        )));
    }

    let question = latest_question(&store.events_for(id)?);
    let outcome = store.apply_transition(
        id,
        Trigger::Answer,
        Some(Event::Answered {
            question,
            answer: args.answer.clone(),
        }),
    )?;
    outln!(
        "answer recorded; task {id} moved to {:?}",
        outcome.next
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{
        ArtifactRef, Budget, Check, Criterion, SqliteStore, Task, TaskId, Tier, WorkerHint,
        WorkspaceSpec,
    };
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
    fn run_approve_moves_draft_to_ready() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Draft);
        store.insert(&task).expect("insert");

        let result = run_approve(
            &store,
            ApproveArgs {
                id: task.id.to_string(),
                note: None,
            },
        )
        .expect("run_approve");
        assert_eq!(result, ExitCode::SUCCESS);

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Ready);
    }

    #[test]
    fn run_approve_on_approval_ready_moves_to_done_and_records_event() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Approval, Status::Ready);
        store.insert(&task).expect("insert");

        run_approve(
            &store,
            ApproveArgs {
                id: task.id.to_string(),
                note: Some("looks good".to_string()),
            },
        )
        .expect("run_approve");

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
    fn run_reject_on_approval_ready_moves_to_failed_and_records_event() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Approval, Status::Ready);
        store.insert(&task).expect("insert");

        run_reject(
            &store,
            RejectArgs {
                id: task.id.to_string(),
                note: Some("not good enough".to_string()),
            },
        )
        .expect("run_reject");

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
    fn run_reject_on_draft_is_an_error_and_does_not_change_status() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Draft);
        store.insert(&task).expect("insert");

        let result = run_reject(
            &store,
            RejectArgs {
                id: task.id.to_string(),
                note: None,
            },
        );
        assert!(result.is_err());

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Draft);
    }

    #[test]
    fn run_answer_on_blocked_moves_to_ready() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&task).expect("insert");

        let result = run_answer(
            &store,
            AnswerArgs {
                id: task.id.to_string(),
                answer: "the answer".to_string(),
            },
        )
        .expect("run_answer");
        assert_eq!(result, ExitCode::SUCCESS);

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Ready);
    }

    /// ADR-0010 D3（P-10）: `answer` は `Event::Answered{question, answer}` を
    /// `Trigger::Answer` の `Event::Transitioned` と同一トランザクションで、その直後に
    /// 追記する。`question` は直近の `WorkerFinished{outcome:"question: ..."}` から取る。
    #[test]
    fn run_answer_persists_answered_event_with_question_from_worker_finished() {
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

        run_answer(
            &store,
            AnswerArgs {
                id: task.id.to_string(),
                answer: "use v2".to_string(),
            },
        )
        .expect("run_answer");

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
    fn run_answer_without_worker_finished_uses_empty_question() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let task = sample_task(TaskKind::Execute, Status::Blocked);
        store.insert(&task).expect("insert");

        run_answer(
            &store,
            AnswerArgs {
                id: task.id.to_string(),
                answer: "the answer".to_string(),
            },
        )
        .expect("run_answer");

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
}
