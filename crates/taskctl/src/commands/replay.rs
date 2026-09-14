//! `taskctl replay` — DESIGN.md §4.3 / §5.9, ADR-0002「結果」節, ADR-0004 D6。
//!
//! `events` から `tasks` を再構築し、実際の `tasks` テーブルと突き合わせて差分ゼロを検証する。

use std::process::ExitCode;

use clap::Args;
use task_core::{Event, Status, Task, TaskStore};

use crate::outln;

#[derive(Args, Debug)]
pub struct ReplayArgs {}

/// `Event` 列を畳み込んで `(status, attempts)` を再構築する（ADR-0004 D6）。
/// `Created` が初期値を与え、以降の `Transitioned` が上書きしていく。
fn replay_status_and_attempts(events: &[(u64, Event)]) -> Option<(Status, u32)> {
    let mut state: Option<(Status, u32)> = None;
    for (_, event) in events {
        match event {
            Event::Created { task } => {
                state = Some((task.status, task.attempts));
            }
            Event::Transitioned { to, reason, .. } => {
                let (_, attempts) = state.unwrap_or((*to, 0));
                let bump = matches!(
                    reason.as_str(),
                    "worker_error" | "lease_expired" | "review_fail"
                );
                state = Some((*to, if bump { attempts + 1 } else { attempts }));
            }
            _ => {}
        }
    }
    state
}

struct Mismatch {
    task_id: String,
    field: &'static str,
    expected: String,
    actual: String,
}

fn diff_task(task: &Task, events: &[(u64, Event)]) -> Vec<Mismatch> {
    let mut mismatches = Vec::new();
    match replay_status_and_attempts(events) {
        Some((status, attempts)) => {
            if status != task.status {
                mismatches.push(Mismatch {
                    task_id: task.id.to_string(),
                    field: "status",
                    expected: format!("{status:?}"),
                    actual: format!("{:?}", task.status),
                });
            }
            if attempts != task.attempts {
                mismatches.push(Mismatch {
                    task_id: task.id.to_string(),
                    field: "attempts",
                    expected: attempts.to_string(),
                    actual: task.attempts.to_string(),
                });
            }
        }
        None => mismatches.push(Mismatch {
            task_id: task.id.to_string(),
            field: "status",
            expected: "<no Created event>".to_string(),
            actual: format!("{:?}", task.status),
        }),
    }
    mismatches
}

pub fn run(store: &dyn TaskStore, _args: ReplayArgs) -> Result<ExitCode, crate::error::CliError> {
    let tasks = store.list(None)?;
    let mut all_mismatches = Vec::new();
    for task in &tasks {
        let events = store.events_for(task.id)?;
        all_mismatches.extend(diff_task(task, &events));
    }

    for m in &all_mismatches {
        outln!(
            "MISMATCH task={} field={} replayed={} stored={}",
            m.task_id, m.field, m.expected, m.actual
        );
    }
    outln!(
        "replay: {} mismatches across {} tasks",
        all_mismatches.len(),
        tasks.len()
    );

    if all_mismatches.is_empty() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use task_core::{
        ArtifactRef, Budget, Check, Criterion, SqliteStore, Tier, TaskId, TaskKind, Trigger,
        WorkerHint, WorkspaceSpec,
    };
    use time::OffsetDateTime;

    fn sample_task(status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".to_string(),
            objective: "o".to_string(),
            acceptance: vec![Criterion {
                text: "x".to_string(),
                check: Check::Human,
            }],
            inputs: vec![ArtifactRef {
                name: "n".to_string(),
                path: "p".to_string(),
                sha256: "s".to_string(),
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
        }
    }

    #[test]
    fn replay_reports_zero_mismatches_when_events_match_current_state() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::Created {
                    task: Box::new(task.clone()),
                },
            )
            .expect("append created");
        store
            .apply_transition(task.id, Trigger::Accept, None)
            .expect("accept");

        let tasks = store.list(None).expect("list");
        let mut mismatches = Vec::new();
        for t in &tasks {
            let events = store.events_for(t.id).expect("events_for");
            mismatches.extend(diff_task(t, &events));
        }
        assert!(mismatches.is_empty(), "expected no mismatches");
    }

    #[test]
    fn replay_detects_status_drift_from_events() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::Created {
                    task: Box::new(task.clone()),
                },
            )
            .expect("append created");

        // tasks 側は insert 時点の Draft のまま更新せず、events だけに Transitioned を
        // 追記する（append_event は tasks 行を更新しないため、これだけで drift が作れる）。
        store
            .append_event(
                task.id,
                &Event::Transitioned {
                    from: Status::Draft,
                    to: Status::Ready,
                    reason: "worker_error".to_string(),
                },
            )
            .expect("append transitioned");

        let fetched = store.get(task.id).expect("get").expect("some");
        let events = store.events_for(task.id).expect("events_for");
        let mismatches = diff_task(&fetched, &events);
        // tasks.status は insert 時点の Draft のまま（イベントは追記しただけで
        // tasks 行を更新していない）なので、replay 側の Ready と食い違う。
        assert!(mismatches.iter().any(|m| m.field == "status"));
        assert!(mismatches.iter().any(|m| m.field == "attempts"));
    }

    #[test]
    fn replay_counts_retry_reasons_into_attempts() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Running);
        store.insert(&task).expect("insert");
        store
            .append_event(
                task.id,
                &Event::Created {
                    task: Box::new(task.clone()),
                },
            )
            .expect("append created");
        store
            .append_event(
                task.id,
                &Event::Transitioned {
                    from: Status::Running,
                    to: Status::Ready,
                    reason: "worker_error".to_string(),
                },
            )
            .expect("append 1");
        store
            .append_event(
                task.id,
                &Event::Transitioned {
                    from: Status::Ready,
                    to: Status::Running,
                    reason: "dispatch".to_string(),
                },
            )
            .expect("append 2");

        let events = store.events_for(task.id).expect("events_for");
        let (status, attempts) = replay_status_and_attempts(&events).expect("some state");
        assert_eq!(status, Status::Running);
        assert_eq!(attempts, 1, "dispatch はリトライ回数を増やさない");
    }
}
