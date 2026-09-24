//! `events` からの派生ビュー（DESIGN.md §5.2/§5.9, ADR-0010 D3/D5/D6/D7, ADR-0011, ADR-0013 D7）。
//!
//! 元は `task-dispatch::dispatcher` と `celerisctl` の `commands/gate.rs` にあった純粋関数をそのまま移した。
//! `task-core` の型だけを使う（`task_worker::{PriorReview, Answer}` は使わない。ワーカープロトコルの
//! 型への写像は呼び出し側 — `task-dispatch` の dispatcher や `celerisctl` の `worker.rs` — で行う）。

use std::collections::HashMap;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;
use task_core::{ArtifactRef, Event, RunRole, Status, Task};

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
            Event::WorkerProgress { msg, .. } if msg.starts_with(REVIEWER_REQUEUED_PREFIX) => {
                n += 1
            }
            _ => {}
        }
    }
    n
}

/// ADR-0054 D2（Phase 113）: Reviewer run **自身のインフラ都合の失敗**（`is_error` の結果・
/// プロセス失敗・resume 拒否など。プロバイダが分類できた供給側失敗＝[`consecutive_reviewer_requeues`]
/// とは別物）で延期したときの `WorkerProgress.msg` の接頭辞。
pub const REVIEWER_INFRA_FAILURE_PREFIX: &str = "reviewer run infra failure: ";

/// 現在の reviewing での、Reviewer run 自身のインフラ都合の失敗による連続延期回数
/// （[`consecutive_reviewer_requeues`] と同じ数え方。最後の `Transitioned` 以降の
/// [`REVIEWER_INFRA_FAILURE_PREFIX`] 付き `WorkerProgress` を数える。プロバイダの供給側失敗の
/// カウンタとは別に、`[review] max_reviewer_retries` と組み合わせて使う）。
pub fn consecutive_reviewer_infra_failures(events: &[(u64, Event)]) -> u32 {
    let mut n = 0;
    for (_, ev) in events.iter().rev() {
        match ev {
            Event::Transitioned { .. } => break,
            Event::WorkerProgress { msg, .. } if msg.starts_with(REVIEWER_INFRA_FAILURE_PREFIX) => {
                n += 1
            }
            _ => {}
        }
    }
    n
}

/// ADR-0070 D3（Phase 116）: `Trigger::InfraRequeue`（`Event::Transitioned.reason ==
/// "infra_requeue"`）による、現在の試行での連続再試行回数。`consecutive_requeues` と対称
/// （新しい順に `infra_requeue` を数え、`dispatch` は読み飛ばし、それ以外の reason で止まる）。
/// `[dispatch] max_infra_retries` と組み合わせて使う。
pub fn consecutive_infra_requeues(events: &[(u64, Event)]) -> u32 {
    let mut n = 0;
    for (_, ev) in events.iter().rev() {
        if let Event::Transitioned { reason, .. } = ev {
            match reason.as_str() {
                "infra_requeue" => n += 1,
                "dispatch" => {}
                _ => break,
            }
        }
    }
    n
}

/// ADR-0070 D3: インフラ都合の再試行のバックオフ（1 回目 30 秒、2 回目 2 分、3 回目以降は 5 分固定）。
/// `n` は「これから何回目の再試行か」（`consecutive_infra_requeues` の値 + 1）。`n == 0` は 0 秒。
pub fn infra_backoff_delay(n: u32) -> Duration {
    match n {
        0 => Duration::ZERO,
        1 => Duration::from_secs(30),
        2 => Duration::from_secs(120),
        _ => Duration::from_secs(300),
    }
}

/// ADR-0070 D1（Phase 116）: `failed` の分類。`infra` はレース・切替・供給側都合、`work` はレビュー
/// 不合格やワーカー自身の明示的な失敗（人が中身を見て判断すべきもの）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    Infra,
    Work,
}

impl FailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureClass::Infra => "infra",
            FailureClass::Work => "work",
        }
    }
}

/// ADR-0070 D3 が最終的な失敗（`WorkerError{retryable:false}`）に付ける接頭辞
/// （ADR-0054 D2 の reviewer 側 `"reviewer infra failure ×N"` と同じ書式）。
pub const INFRA_FAILURE_MARKER: &str = "infra failure ×";
/// 供給側失敗の requeue 上限到達（ADR-0011 P-38）の文言。人の目には「レート制限が続いた」ことを
/// 意味するので、これも `infra` に分類する。
const REQUEUE_LIMIT_MARKER: &str = "requeue limit (";

/// 文字列の最初の行（前後の空白を落とす）。空なら空文字列のまま。
fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

/// ADR-0070 D1 追記（Phase 116。本番で確認: 2026-09-24 09:21Z、`/home` が満杯になり run が
/// `adapter: io error: No space left on device (os error 28)` で 2 回失敗した）: OS のディスク
/// 不足エラー文言。`std::io::Error`（`ENOSPC`）の `Display` がこの文字列を含む。ディスク残量の事前
/// チェックは別 Phase（今回は分類と文言だけ）。
const DISK_FULL_MARKER: &str = "No space left on device";

/// ディスク不足なら（`class` に関わらず）`infra` に強制し、人が読む理由の先頭に
/// 「ディスク不足: 」を足す。それ以外はそのまま返す。
fn annotate_disk_full(class: FailureClass, reason: String) -> (FailureClass, String) {
    if reason.contains(DISK_FULL_MARKER) && !reason.starts_with("ディスク不足: ") {
        (FailureClass::Infra, format!("ディスク不足: {reason}"))
    } else {
        (class, reason)
    }
}

/// ADR-0070 D1: `failed` に落ちた理由を分類し、人が読む 1 行を作る（純粋関数。`events` は `failed` の
/// タスクの全イベント、新しい順に走査する）。`Failed` でないタスクに対して呼んでも構わない
/// （その場合は最後に見つかった `WorkerFinished`/`ReviewVerdict` から意味のない分類を返すだけ）。
pub fn classify_task_failure(events: &[(u64, Event)]) -> (FailureClass, String) {
    let last_transition_reason = events.iter().rev().find_map(|(_, e)| match e {
        Event::Transitioned { reason, .. } => Some(reason.as_str()),
        _ => None,
    });
    if last_transition_reason == Some("review_fail") {
        if let Some(reason) = events.iter().rev().find_map(|(_, e)| match e {
            Event::ReviewVerdict {
                pass: false,
                reason,
                ..
            } => Some(reason.as_str()),
            _ => None,
        }) {
            return annotate_disk_full(FailureClass::Work, first_line(reason));
        }
        return (FailureClass::Work, "レビュー不合格".to_string());
    }
    for (_, e) in events.iter().rev() {
        if let Event::WorkerFinished {
            outcome, role: None, ..
        } = e
        {
            if outcome.starts_with(INFRA_FAILURE_MARKER) || outcome.contains(REQUEUE_LIMIT_MARKER) {
                return annotate_disk_full(FailureClass::Infra, first_line(outcome));
            }
            return annotate_disk_full(FailureClass::Work, first_line(outcome));
        }
    }
    (FailureClass::Work, "原因不明の失敗".to_string())
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
            Event::ArtifactProduced {
                run_id: r,
                artifact,
            } if r == run_id => Some(artifact.clone()),
            _ => None,
        })
        .collect()
}

/// ADR-0067 D4: その run の `ArtifactProduced` を、`GET /tasks/{id}/artifacts/{idx}` と同じ添字
/// （`ArtifactProduced` を**全 run を通じて**出現順に 0 始まりで数えたもの。`crates/task-api/src/files.rs`
/// の `produced(rows).enumerate()` と同じ規則）付きで返す。GUI が承認画面からその場で本文を取りに行くのに使う。
pub fn artifacts_for_run_with_idx(
    events: &[(u64, Event)],
    run_id: &str,
) -> Vec<(usize, ArtifactRef)> {
    events
        .iter()
        .filter_map(|(_, ev)| match ev {
            Event::ArtifactProduced { run_id: r, artifact } => Some((r, artifact)),
            _ => None,
        })
        .enumerate()
        .filter(|(_, (r, _))| *r == run_id)
        .map(|(idx, (_, artifact))| (idx, artifact.clone()))
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

/// ADR-0062 Phase 108: 直近の `Blocked` への遷移が `Trigger::Unroutable`（reason `"unroutable"`）に
/// よるものか。B1（担当に `cluster:<id>` が無い）と ADR-0046 D5（担当の候補が無い）はどちらも同じ
/// trigger を使うので区別しない。`PATCH /tasks/{id}` が `workspace`/`assignee` の変更で経路が通った
/// `blocked` タスクを `ready` に戻せるかの判定に使う（worker が聞いた質問による `blocked` とは分ける。
/// そちらは `Trigger::WorkerQuestion`、reason は `"worker_question"`）。
pub fn latest_block_is_unroutable(events: &[(u64, Event)]) -> bool {
    events
        .iter()
        .rev()
        .find_map(|(_, e)| match e {
            Event::Transitioned {
                to: Status::Blocked,
                reason,
                ..
            } => Some(reason == "unroutable"),
            _ => None,
        })
        .unwrap_or(false)
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
            metrics: None,
        };
        let events: Vec<(u64, Event)> = vec![
            started("run-1", None),
            finished("run-1", "question: which version?", None),
            started("rev-1", Some(RunRole::Reviewer)),
            finished(
                "rev-1",
                "question: reviewer asked instead of judging",
                Some(RunRole::Reviewer),
            ),
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
            (1, Event::worker_progress("r", "noise")),
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
                AnswerNote {
                    question: "q1".into(),
                    answer: "a1".into()
                },
                AnswerNote {
                    question: "q2".into(),
                    answer: "a2".into()
                },
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
        let events: Vec<(u64, Event)> =
            vec![(0, transitioned("requeue")), (1, transitioned("accept"))];
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
                Event::worker_progress("r", format!("{REVIEWER_REQUEUED_PREFIX}throttled")),
            ),
            (
                2,
                Event::worker_progress("r", format!("{REVIEWER_REQUEUED_PREFIX}auth failed")),
            ),
        ];
        assert_eq!(consecutive_reviewer_requeues(&events), 2);
    }

    #[test]
    fn consecutive_reviewer_requeues_ignores_unrelated_progress() {
        let events: Vec<(u64, Event)> = vec![
            (0, transitioned("worker_done")),
            (1, Event::worker_progress("r", "unrelated")),
        ];
        assert_eq!(consecutive_reviewer_requeues(&events), 0);
    }

    fn worker_finished(outcome: &str) -> Event {
        Event::WorkerFinished {
            run_id: "run-1".into(),
            outcome: outcome.into(),
            usage: None,
            role: None,
            metrics: None,
        }
    }

    #[test]
    fn consecutive_infra_requeues_counts_since_last_non_requeue_transition() {
        let events: Vec<(u64, Event)> = vec![
            (0, transitioned("worker_error")),
            (1, transitioned("dispatch")),
            (2, transitioned("infra_requeue")),
            (3, transitioned("dispatch")),
            (4, transitioned("infra_requeue")),
        ];
        assert_eq!(consecutive_infra_requeues(&events), 2);
        // 供給側失敗の `requeue` は別カウンタ（混ざらない）。
        let mixed: Vec<(u64, Event)> = vec![(0, transitioned("infra_requeue")), (1, transitioned("requeue"))];
        assert_eq!(consecutive_infra_requeues(&mixed), 0);
        assert_eq!(consecutive_requeues(&mixed), 1);
    }

    #[test]
    fn infra_backoff_delay_escalates_then_caps() {
        assert_eq!(infra_backoff_delay(0), Duration::ZERO);
        assert_eq!(infra_backoff_delay(1), Duration::from_secs(30));
        assert_eq!(infra_backoff_delay(2), Duration::from_secs(120));
        assert_eq!(infra_backoff_delay(3), Duration::from_secs(300));
        assert_eq!(infra_backoff_delay(10), Duration::from_secs(300));
    }

    #[test]
    fn classify_task_failure_marks_infra_exhaustion_as_infra() {
        let events: Vec<(u64, Event)> = vec![
            (0, worker_finished("infra_requeue: lease expired (run_id=run-1)")),
            (1, transitioned("infra_requeue")),
            (
                2,
                worker_finished("infra failure ×5: adapter: session resume rejected"),
            ),
            (
                3,
                Event::Transitioned {
                    from: task_core::Status::Running,
                    to: task_core::Status::Failed,
                    reason: "worker_error".into(),
                },
            ),
        ];
        let (class, reason) = classify_task_failure(&events);
        assert_eq!(class, FailureClass::Infra);
        assert_eq!(reason, "infra failure ×5: adapter: session resume rejected");
    }

    /// ADR-0070 D1 追記（Phase 116。本番: 2026-09-24 09:21Z、`/home` が満杯で run が 2 回失敗）:
    /// ディスク不足のエラーは（work 経路で拾われても）`infra` に強制され、「ディスク不足」の文言が
    /// 理由の先頭に付く。
    #[test]
    fn classify_task_failure_forces_disk_full_errors_to_infra() {
        let events: Vec<(u64, Event)> = vec![(
            0,
            worker_finished("error(retryable=false): adapter: io error: No space left on device (os error 28)"),
        )];
        let (class, reason) = classify_task_failure(&events);
        assert_eq!(class, FailureClass::Infra);
        assert!(reason.starts_with("ディスク不足: "), "{reason}");
        assert!(reason.contains("No space left on device"), "{reason}");
    }

    #[test]
    fn classify_task_failure_marks_requeue_limit_reached_as_infra() {
        let events: Vec<(u64, Event)> = vec![(
            0,
            worker_finished("error(retryable=true): requeue limit (3) reached: adapter: 429"),
        )];
        let (class, _) = classify_task_failure(&events);
        assert_eq!(class, FailureClass::Infra);
    }

    #[test]
    fn classify_task_failure_marks_review_fail_and_worker_error_as_work() {
        let review_fail: Vec<(u64, Event)> = vec![
            (
                0,
                Event::ReviewVerdict {
                    run_id: "rev-1".into(),
                    criterion_idx: 0,
                    pass: false,
                    reason: "テストが落ちている\n詳細は省略".into(),
                },
            ),
            (
                1,
                Event::Transitioned {
                    from: task_core::Status::Reviewing,
                    to: task_core::Status::Failed,
                    reason: "review_fail".into(),
                },
            ),
        ];
        let (class, reason) = classify_task_failure(&review_fail);
        assert_eq!(class, FailureClass::Work);
        assert_eq!(reason, "テストが落ちている");

        let worker_error: Vec<(u64, Event)> = vec![(
            0,
            worker_finished("error(retryable=false): max_turns exceeded"),
        )];
        let (class, reason) = classify_task_failure(&worker_error);
        assert_eq!(class, FailureClass::Work);
        assert_eq!(reason, "error(retryable=false): max_turns exceeded");
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
        assert_eq!(
            retry_backoff(Duration::ZERO, Duration::from_secs(300), 5),
            Duration::ZERO
        );
    }

    fn artifact(name: &str) -> ArtifactRef {
        ArtifactRef {
            name: name.to_string(),
            path: format!("artifacts/{name}"),
            sha256: "abc".to_string(),
            kind: "doc".to_string(),
            declared: true,
        }
    }

    #[test]
    fn artifacts_for_run_filters_by_run_id() {
        let events: Vec<(u64, Event)> = vec![
            (
                0,
                Event::ArtifactProduced {
                    run_id: "run-1".into(),
                    artifact: artifact("a"),
                },
            ),
            (
                1,
                Event::ArtifactProduced {
                    run_id: "run-2".into(),
                    artifact: artifact("b"),
                },
            ),
            (
                2,
                Event::ArtifactProduced {
                    run_id: "run-1".into(),
                    artifact: artifact("c"),
                },
            ),
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
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
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
            worker_hint: task_core::WorkerHint {
                tier: task_core::Tier::Standard,
                adapter: None,
            },
            workspace: task_core::WorkspaceSpec::Local {
                path: "/tmp".into(),
                mode: None,
            },
            budget: task_core::Budget {
                max_turns: 1,
                max_wall_secs: 1,
                max_retries: 1,
            },
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
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
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
            Event::ApprovalDecided {
                by: "human".into(),
                approved: false,
                note: Some("nope".into()),
            },
        )];
        assert_eq!(approval_decision_note(&with_note), ": nope");

        let without_note: Vec<(u64, Event)> = vec![(
            0,
            Event::ApprovalDecided {
                by: "human".into(),
                approved: false,
                note: None,
            },
        )];
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
                metrics: None,
            },
        )];
        assert_eq!(latest_question(&events), "which version?");
    }

    #[test]
    fn latest_question_empty_without_worker_finished() {
        assert_eq!(latest_question(&[]), "");
    }
}
