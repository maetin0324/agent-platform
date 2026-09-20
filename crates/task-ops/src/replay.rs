//! `celerisctl replay` の再構築ロジック — DESIGN.md §4.3 / §5.9, ADR-0002「結果」節, ADR-0004 D6
//! （ADR-0013 D7）。`events` から `tasks` を再構築し、実際の `tasks` テーブルと突き合わせる。
//! `celerisctl` は出力整形と exit code だけを持つ。

use schemars::JsonSchema;
use serde::Serialize;
use task_core::{Event, Status, Task, TaskId, TaskStore};

use crate::error::OpsError;

/// `replay` が検出した 1 件の食い違い。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ReplayMismatch {
    pub task_id: TaskId,
    /// `"status"` または `"attempts"`。
    pub field: String,
    /// `events` から再構築した値。
    pub replayed: String,
    /// `tasks` テーブルに保存されている値。
    pub stored: String,
}

/// `replay` の結果。
#[derive(Debug, Clone, PartialEq, Default, Serialize, JsonSchema)]
pub struct ReplayReport {
    pub tasks: usize,
    pub mismatches: Vec<ReplayMismatch>,
}

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
                // ADR-0044 D2（Phase 53）: `reopen` は attempts を **0 に戻す**唯一のトリガ
                // （`Trigger::Reopen`）。ここに書かないと、再開したタスクは毎回
                // `attempts: replayed=N stored=0` の不一致として報告され続ける。
                if reason == "reopen" {
                    state = Some((*to, 0));
                    continue;
                }
                let bump = matches!(reason.as_str(), "worker_error" | "lease_expired" | "review_fail")
                    // ADR-0021 D1: `child_failed` は「やり直し」のときだけ attempts を使う
                    // （人の判断待ち = blocked に落ちるときは据え置き）。
                    || (reason == "child_failed" && *to == Status::Ready);
                state = Some((*to, if bump { attempts + 1 } else { attempts }));
            }
            _ => {}
        }
    }
    state
}

fn diff_task(task: &Task, events: &[(u64, Event)]) -> Vec<ReplayMismatch> {
    let mut mismatches = Vec::new();
    match replay_status_and_attempts(events) {
        Some((status, attempts)) => {
            if status != task.status {
                mismatches.push(ReplayMismatch {
                    task_id: task.id,
                    field: "status".to_string(),
                    replayed: format!("{status:?}"),
                    stored: format!("{:?}", task.status),
                });
            }
            if attempts != task.attempts {
                mismatches.push(ReplayMismatch {
                    task_id: task.id,
                    field: "attempts".to_string(),
                    replayed: attempts.to_string(),
                    stored: task.attempts.to_string(),
                });
            }
        }
        None => mismatches.push(ReplayMismatch {
            task_id: task.id,
            field: "status".to_string(),
            replayed: "<no Created event>".to_string(),
            stored: format!("{:?}", task.status),
        }),
    }
    mismatches
}

/// `store` の全タスクについて `events` から再構築した状態と `tasks` テーブルを突き合わせる。
pub fn replay(store: &dyn TaskStore) -> Result<ReplayReport, OpsError> {
    let tasks = store.list(None)?;
    let mut all_mismatches = Vec::new();
    for task in &tasks {
        let events = store.events_for(task.id)?;
        all_mismatches.extend(diff_task(task, &events));
    }
    Ok(ReplayReport {
        tasks: tasks.len(),
        mismatches: all_mismatches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use task_core::{
        ArtifactRef, Budget, Check, Criterion, SqliteStore, Tier, TaskKind, Trigger, WorkerHint,
        WorkspaceSpec,
    };
    use time::OffsetDateTime;

    fn sample_task(status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            repos: Vec::new(),
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
                path: PathBuf::from("/tmp/ws"), mode: None,
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

        let report = replay(&store).expect("replay");
        assert!(report.mismatches.is_empty(), "expected no mismatches");
        assert_eq!(report.tasks, 1);
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

        let report = replay(&store).expect("replay");
        // tasks.status は insert 時点の Draft のまま（イベントは追記しただけで
        // tasks 行を更新していない）なので、replay 側の Ready と食い違う。
        assert!(report.mismatches.iter().any(|m| m.field == "status"));
        assert!(report.mismatches.iter().any(|m| m.field == "attempts"));
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

    /// ADR-0016 D3: `Trigger::Aggregate`（reviewing -> ready, `reason: "aggregate"`）は attempts を増やさない。
    #[test]
    fn replay_aggregate_transition_does_not_bump_attempts() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Reviewing);
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
                    from: Status::Reviewing,
                    to: Status::Ready,
                    reason: "aggregate".to_string(),
                },
            )
            .expect("append transitioned");

        let events = store.events_for(task.id).expect("events_for");
        let (status, attempts) = replay_status_and_attempts(&events).expect("some state");
        assert_eq!(status, Status::Ready);
        assert_eq!(attempts, 0, "aggregate はリトライ回数を増やさない");
    }

    /// ADR-0021 D1: `child_failed` は **ready に戻るときだけ** attempts を使う（blocked は人の判断待ちなので据え置き）。
    #[test]
    fn replay_child_failed_bumps_attempts_only_when_it_retries() {
        let replayed = |to: Status| {
            let store = SqliteStore::open_in_memory().expect("open");
            let task = sample_task(Status::Reviewing);
            store.insert(&task).expect("insert");
            store
                .append_event(task.id, &Event::Created { task: Box::new(task.clone()) })
                .expect("append created");
            store
                .append_event(
                    task.id,
                    &Event::Transitioned { from: Status::Reviewing, to, reason: "child_failed".to_string() },
                )
                .expect("append transitioned");
            let events = store.events_for(task.id).expect("events_for");
            replay_status_and_attempts(&events).expect("some state")
        };
        assert_eq!(replayed(Status::Ready), (Status::Ready, 1), "やり直しは attempts を使う");
        assert_eq!(replayed(Status::Blocked), (Status::Blocked, 0), "人に聞くときは使わない");
    }

    /// ADR-0044 D2（Phase 53）: `reopen` は attempts を **0 に戻す**唯一のトリガ。
    /// 畳み込みがこれを知らないと、再開したタスクは毎回 `attempts` の不一致として報告され続ける
    /// （ADR-0004 D6 の不変条件の検査が狼少年になる。Phase 53 の監査で発見）。
    #[test]
    fn replay_follows_reopen_back_to_zero_attempts() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut task = sample_task(Status::Draft);
        task.budget.max_retries = 0;
        store
            .create_task(&task, vec![Event::Created { task: Box::new(task.clone()) }])
            .expect("create");
        // draft → ready → running → failed（attempts を 1 使う）。
        store.apply_transition(task.id, Trigger::Accept, None).expect("accept");
        store.apply_transition(task.id, Trigger::Dispatch, None).expect("dispatch");
        store
            .apply_transition(task.id, Trigger::WorkerError { retryable: false }, None)
            .expect("worker_error");
        assert_eq!(store.get(task.id).expect("get").expect("some").attempts, 1);

        // 人が再開する（attempts は 0 に戻る）。
        crate::comment::reopen(&store, task.id, None).expect("reopen");
        let stored = store.get(task.id).expect("get").expect("some");
        assert_eq!((stored.status, stored.attempts), (Status::Ready, 0));

        // `celerisctl replay` は不一致を報告しない。
        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(replay_status_and_attempts(&events), Some((Status::Ready, 0)));
        let report = replay(&store).expect("replay");
        assert_eq!(report.mismatches, Vec::new(), "{report:?}");
    }
}
