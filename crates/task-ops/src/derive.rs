//! `events` からの派生ビュー（DESIGN.md §5.2/§5.9, ADR-0010 D3/D5/D6/D7, ADR-0011, ADR-0013 D7）。
//!
//! 元は `task-dispatch::dispatcher` と `taskctl` の `commands/gate.rs` にあった純粋関数をそのまま移した。
//! `task-core` の型だけを使う（`task_worker::{PriorReview, Answer}` は使わない。ワーカープロトコルの
//! 型への写像は呼び出し側 — `task-dispatch` の dispatcher や `taskctl` の `worker.rs` — で行う）。

use std::collections::HashMap;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;
use task_core::{ArtifactRef, Event, RunRole, Task};

/// `prior_review_from_events` の要素。`task_worker::PriorReview` と同じ形（フィールド名も同じ）。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ReviewNote {
    pub criterion: usize,
    pub pass: bool,
    pub reason: String,
}

/// `answers_from_events` の要素。`task_worker::Answer` と同じ形（フィールド名も同じ）。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct AnswerNote {
    pub question: String,
    pub answer: String,
}

/// 直前のレビュー（最後に `ReviewVerdict` を記録した run の全判定）を `context.prior_review` に写す。
pub fn prior_review_from_events(events: &[(u64, Event)]) -> Vec<ReviewNote> {
    let mut by_run: HashMap<&str, Vec<ReviewNote>> = HashMap::new();
    let mut last_run: Option<&str> = None;
    for (_, ev) in events {
        if let Event::ReviewVerdict {
            run_id,
            criterion_idx,
            pass,
            reason,
        } = ev
        {
            by_run.entry(run_id.as_str()).or_default().push(ReviewNote {
                criterion: *criterion_idx,
                pass: *pass,
                reason: reason.clone(),
            });
            last_run = Some(run_id.as_str());
        }
    }
    let mut out = last_run.and_then(|r| by_run.remove(r)).unwrap_or_default();
    out.sort_by_key(|p| p.criterion);
    out
}

/// そのタスクの全 `Event::Answered` を時系列で `context.answers` に写す（ADR-0010 D3, P-10）。
pub fn answers_from_events(events: &[(u64, Event)]) -> Vec<AnswerNote> {
    events
        .iter()
        .filter_map(|(_, ev)| match ev {
            Event::Answered { question, answer } => Some(AnswerNote {
                question: question.clone(),
                answer: answer.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// 現在の試行での連続 requeue 回数（ADR-0011 D2）。`Transitioned` を新しい順に見て `requeue` を数え、
/// `dispatch` は読み飛ばし、それ以外の reason で止まる。
pub fn consecutive_requeues(events: &[(u64, Event)]) -> u32 {
    let mut n = 0;
    for (_, ev) in events.iter().rev() {
        if let Event::Transitioned { reason, .. } = ev {
            match reason.as_str() {
                "requeue" => n += 1,
                "dispatch" => {}
                _ => break,
            }
        }
    }
    n
}

/// Reviewer run の供給側失敗で延期したときの `WorkerProgress.msg` の接頭辞（ADR-0010 D5）。
pub const REVIEWER_REQUEUED_PREFIX: &str = "reviewer run requeued: ";

/// 現在の reviewing での、Reviewer run の供給側失敗による連続延期回数（ADR-0011 D2）。
/// 最後の `Transitioned`（reviewing に入った遷移）以降の延期の `WorkerProgress` を数える。
pub fn consecutive_reviewer_requeues(events: &[(u64, Event)]) -> u32 {
    let mut n = 0;
    for (_, ev) in events.iter().rev() {
        match ev {
            Event::Transitioned { .. } => break,
            Event::WorkerProgress { msg, .. } if msg.starts_with(REVIEWER_REQUEUED_PREFIX) => n += 1,
            _ => {}
        }
    }
    n
}

/// ADR-0010 D6（P-3）: `min(base·2^(attempts-1), max)`。`attempts == 0` または `base == 0` なら 0。
pub fn retry_backoff(base: Duration, max: Duration, attempts: u32) -> Duration {
    if attempts == 0 || base.is_zero() {
        return Duration::ZERO;
    }
    let factor = 1u32.checked_shl(attempts - 1).unwrap_or(u32::MAX);
    base.checked_mul(factor).unwrap_or(max).min(max)
}

/// その run で `ArtifactProduced` された成果物。
pub fn artifacts_for_run(events: &[(u64, Event)], run_id: &str) -> Vec<ArtifactRef> {
    events
        .iter()
        .filter_map(|(_, ev)| match ev {
            Event::ArtifactProduced { run_id: r, artifact } if r == run_id => Some(artifact.clone()),
            _ => None,
        })
        .collect()
}

/// `role` が Reviewer run を指すか（`None` はワーカー run。ADR-0014 D1）。
pub fn is_reviewer(role: Option<RunRole>) -> bool {
    role == Some(RunRole::Reviewer)
}

/// 最後に `WorkerStarted` した**ワーカー** run の id（Reviewer run は除く。ADR-0014 D1）。
pub fn last_run_id(events: &[(u64, Event)]) -> Option<String> {
    events.iter().rev().find_map(|(_, ev)| match ev {
        Event::WorkerStarted { run_id, role, .. } if !is_reviewer(*role) => Some(run_id.clone()),
        _ => None,
    })
}

/// `Human` criterion 用の `Approval` 子タスクの `title`（既存子の照合キーにも使う。ADR-0008 D2）。
/// ADR-0010 D8（P-35）: 試行（`attempts + 1`）を含めるので、再レビューでは新しい子が作られる。
pub fn human_approval_title(task: &Task, idx: usize) -> String {
    format!(
        "Approval needed: {} — criterion {idx} (attempt {})",
        task.title,
        task.attempts + 1
    )
}

/// 直近の `Event::ApprovalDecided` の `note` を `": <note>"` の形で返す（無ければ空文字列）。
pub fn approval_decision_note(events: &[(u64, Event)]) -> String {
    events
        .iter()
        .rev()
        .find_map(|(_, e)| match e {
            Event::ApprovalDecided { note: Some(n), .. } => Some(format!(": {n}")),
            Event::ApprovalDecided { note: None, .. } => Some(String::new()),
            _ => None,
        })
        .unwrap_or_default()
}

/// 直近の `WorkerFinished{outcome}` のうち `"question: "` で始まるものから、接頭辞を
/// 除いた質問文を取り出す（ADR-0010 D3）。`events_for` を後ろから見て最初に見つかった
/// ものを使う。無ければ空文字列。Reviewer run の `WorkerFinished` は見ない（ADR-0014 D1）。
pub fn latest_question(events: &[(u64, Event)]) -> String {
    events
        .iter()
        .rev()
        .find_map(|(_, event)| match event {
            Event::WorkerFinished { outcome, role, .. } if !is_reviewer(*role) => {
                outcome.strip_prefix("question: ").map(str::to_string)
            }
            // ADR-0021 D2: ディスパッチャが出した質問（run の終了ではない）。
            Event::QuestionRaised { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0014 D1: Reviewer run の `WorkerStarted` / `WorkerFinished` は、ワーカー run を前提にする派生値から除く。
    #[test]
    fn last_run_id_and_latest_question_ignore_reviewer_runs() {
        let started = |run_id: &str, role| Event::WorkerStarted {
            run_id: run_id.into(),
            adapter: "fake".into(),
            model: "m".into(),
            provider: None,
            account: None,
            role,
            task_role: None,
        };
        let finished = |run_id: &str, outcome: &str, role| Event::WorkerFinished {
            run_id: run_id.into(),
            outcome: outcome.into(),
            usage: None,
            role,
        };
        let events: Vec<(u64, Event)> = vec![
            started("run-1", None),
            finished("run-1", "question: which version?", None),
            started("rev-1", Some(RunRole::Reviewer)),
            finished("rev-1", "question: reviewer asked instead of judging", Some(RunRole::Reviewer)),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, e)| (i as u64, e))
        .collect();
        assert_eq!(last_run_id(&events).as_deref(), Some("run-1"));
        assert_eq!(latest_question(&events), "which version?");
    }

    fn verdict(run_id: &str, criterion_idx: usize, pass: bool, reason: &str) -> Event {
        Event::ReviewVerdict {
            run_id: run_id.to_string(),
            criterion_idx,
            pass,
            reason: reason.to_string(),
        }
    }

    #[test]
    fn prior_review_from_events_uses_only_the_last_run_and_sorts_by_criterion() {
        let events: Vec<(u64, Event)> = vec![
            (0, verdict("run-1", 1, true, "old")),
            (1, verdict("run-1", 0, false, "old-0")),
            (2, verdict("run-2", 1, true, "new-1")),
            (3, verdict("run-2", 0, false, "new-0")),
        ];
        let prior = prior_review_from_events(&events);
        assert_eq!(prior.len(), 2);
        assert_eq!(prior[0].criterion, 0);
        assert_eq!(prior[0].reason, "new-0");
        assert!(!prior[0].pass);
        assert_eq!(prior[1].criterion, 1);
        assert_eq!(prior[1].reason, "new-1");
    }

    #[test]
    fn prior_review_from_events_empty_when_no_verdicts() {
        assert!(prior_review_from_events(&[]).is_empty());
    }

    #[test]
    fn answers_from_events_collects_in_order() {
        let events: Vec<(u64, Event)> = vec![
            (
                0,
                Event::Answered {
                    question: "q1".into(),
                    answer: "a1".into(),
                },
            ),
            (1, Event::WorkerProgress { run_id: "r".into(), msg: "noise".into() }),
            (
                2,
                Event::Answered {
                    question: "q2".into(),
                    answer: "a2".into(),
                },
            ),
        ];
        let answers = answers_from_events(&events);
        assert_eq!(
            answers,
            vec![
                AnswerNote { question: "q1".into(), answer: "a1".into() },
                AnswerNote { question: "q2".into(), answer: "a2".into() },
            ]
        );
    }

    fn transitioned(reason: &str) -> Event {
        Event::Transitioned {
            from: task_core::Status::Running,
            to: task_core::Status::Ready,
            reason: reason.to_string(),
        }
    }

    #[test]
    fn consecutive_requeues_counts_trailing_requeues_and_skips_dispatch() {
        let events: Vec<(u64, Event)> = vec![
            (0, transitioned("worker_error")),
            (1, transitioned("dispatch")),
            (2, transitioned("requeue")),
            (3, transitioned("dispatch")),
            (4, transitioned("requeue")),
        ];
        assert_eq!(consecutive_requeues(&events), 2);
    }

    #[test]
    fn consecutive_requeues_stops_at_non_requeue_non_dispatch_reason() {
        let events: Vec<(u64, Event)> = vec![(0, transitioned("requeue")), (1, transitioned("accept"))];
        // 新しい順に見るので末尾の "accept" で即座に止まり、0を返す。
        assert_eq!(consecutive_requeues(&events), 0);
    }

    #[test]
    fn consecutive_requeues_is_zero_with_no_events() {
        assert_eq!(consecutive_requeues(&[]), 0);
    }

    #[test]
    fn consecutive_reviewer_requeues_counts_since_last_transitioned() {
        let events: Vec<(u64, Event)> = vec![
            (0, transitioned("worker_done")),
            (
                1,
                Event::WorkerProgress {
                    run_id: "r".into(),
                    msg: format!("{REVIEWER_REQUEUED_PREFIX}throttled"),
                },
            ),
            (
                2,
                Event::WorkerProgress {
                    run_id: "r".into(),
                    msg: format!("{REVIEWER_REQUEUED_PREFIX}auth failed"),
                },
            ),
        ];
        assert_eq!(consecutive_reviewer_requeues(&events), 2);
    }

    #[test]
    fn consecutive_reviewer_requeues_ignores_unrelated_progress() {
        let events: Vec<(u64, Event)> = vec![
            (0, transitioned("worker_done")),
            (1, Event::WorkerProgress { run_id: "r".into(), msg: "unrelated".into() }),
        ];
        assert_eq!(consecutive_reviewer_requeues(&events), 0);
    }

    #[test]
    fn retry_backoff_exponential_with_cap() {
        let (base, max) = (Duration::from_secs(10), Duration::from_secs(300));
        assert_eq!(retry_backoff(base, max, 0), Duration::ZERO);
        assert_eq!(retry_backoff(base, max, 1), Duration::from_secs(10));
        assert_eq!(retry_backoff(base, max, 3), Duration::from_secs(40));
        assert_eq!(retry_backoff(base, max, 40), max);
    }

    #[test]
    fn retry_backoff_zero_base_is_always_zero() {
        assert_eq!(retry_backoff(Duration::ZERO, Duration::from_secs(300), 5), Duration::ZERO);
    }

    fn artifact(name: &str) -> ArtifactRef {
        ArtifactRef {
            name: name.to_string(),
            path: format!("artifacts/{name}"),
            sha256: "abc".to_string(),
            kind: "doc".to_string(),
        }
    }

    #[test]
    fn artifacts_for_run_filters_by_run_id() {
        let events: Vec<(u64, Event)> = vec![
            (0, Event::ArtifactProduced { run_id: "run-1".into(), artifact: artifact("a") }),
            (1, Event::ArtifactProduced { run_id: "run-2".into(), artifact: artifact("b") }),
            (2, Event::ArtifactProduced { run_id: "run-1".into(), artifact: artifact("c") }),
        ];
        let produced = artifacts_for_run(&events, "run-1");
        assert_eq!(produced.len(), 2);
        assert_eq!(produced[0].name, "a");
        assert_eq!(produced[1].name, "c");
    }

    #[test]
    fn last_run_id_returns_most_recent_worker_started() {
        let events: Vec<(u64, Event)> = vec![
            (
                0,
                Event::WorkerStarted {
                    run_id: "run-1".into(),
                    adapter: "fake".into(),
                    model: "m".into(),
                    provider: None,
                    account: None,
                    role: None,
                    task_role: None,
                },
            ),
            (
                1,
                Event::WorkerStarted {
                    run_id: "run-2".into(),
                    adapter: "fake".into(),
                    model: "m".into(),
                    provider: None,
                    account: None,
                    role: None,
                    task_role: None,
                },
            ),
        ];
        assert_eq!(last_run_id(&events), Some("run-2".to_string()));
    }

    #[test]
    fn last_run_id_none_without_worker_started() {
        assert_eq!(last_run_id(&[]), None);
    }

    fn sample_task(title: &str, attempts: u32) -> Task {
        let now = time::OffsetDateTime::now_utc();
        Task {
            id: task_core::TaskId::new(),
            parent_id: None,
            kind: task_core::TaskKind::Execute,
            title: title.to_string(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: task_core::Status::Running,
            priority: 0,
            worker_hint: task_core::WorkerHint { tier: task_core::Tier::Standard, adapter: None },
            workspace: task_core::WorkspaceSpec::Local { path: "/tmp".into() },
            budget: task_core::Budget { max_turns: 1, max_wall_secs: 1, max_retries: 1 },
            attempts,
            lease: None,
            created_at: now,
            updated_at: now,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
        }
    }

    #[test]
    fn human_approval_title_includes_attempt_number() {
        let task = sample_task("do it", 2);
        let title = human_approval_title(&task, 0);
        assert_eq!(title, "Approval needed: do it — criterion 0 (attempt 3)");
    }

    #[test]
    fn approval_decision_note_returns_note_or_empty() {
        let with_note: Vec<(u64, Event)> = vec![(
            0,
            Event::ApprovalDecided { by: "human".into(), approved: false, note: Some("nope".into()) },
        )];
        assert_eq!(approval_decision_note(&with_note), ": nope");

        let without_note: Vec<(u64, Event)> =
            vec![(0, Event::ApprovalDecided { by: "human".into(), approved: false, note: None })];
        assert_eq!(approval_decision_note(&without_note), "");

        assert_eq!(approval_decision_note(&[]), "");
    }

    #[test]
    fn latest_question_extracts_prefix_from_worker_finished() {
        let events: Vec<(u64, Event)> = vec![(
            0,
            Event::WorkerFinished {
                run_id: "run-1".into(),
                outcome: "question: which version?".into(),
                usage: None,
                role: None,
            },
        )];
        assert_eq!(latest_question(&events), "which version?");
    }

    #[test]
    fn latest_question_empty_without_worker_finished() {
        assert_eq!(latest_question(&[]), "");
    }
}
