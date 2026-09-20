//! タスク状態機械の純粋関数。DESIGN.md / ADR-0002 D2,D3,D8 で定義された
//! 遷移規則をそのまま実装する。I/O・時計・乱数は一切使わない（ADR-0001 D2）。

use crate::model::{Status, TaskKind};

/// `transition` の入力となる現在状態のスナップショット。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateView {
    pub kind: TaskKind,
    pub status: Status,
    pub attempts: u32,
    pub max_retries: u32,
}

/// タスクの状態遷移を駆動するトリガー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Accept,
    Dispatch,
    WorkerDone,
    WorkerQuestion,
    WorkerError {
        retryable: bool,
    },
    LeaseExpired,
    ReviewPass,
    ReviewFail,
    Answer,
    Approve,
    Reject,
    Cancel,
    /// 供給側失敗（レート制限・認証失敗・枯渇・起動失敗）: `running → ready`、attempts 据え置き（ADR-0010 D1, P-21）。
    Requeue,
    /// 先行タスクが `failed`/`cancelled` になった後続: 非終端 → `cancelled`（ADR-0010 D1, P-9）。
    DependencyFailed,
    /// 委譲した子が全て終端になり、集約 run を行う親: `reviewing → ready`、attempts 据え置き（ADR-0016 D3 / M1）。
    Aggregate,
    /// 委譲した子が失敗した親（ADR-0021 D1）: やり直せるなら `reviewing → ready`（attempts 消費）、
    /// やり直せないなら `reviewing → blocked`（人間の判断待ち）。**`failed` にはしない**。
    ChildFailed,
    /// ADR-0044 D2（Phase 53）: 人のコメントによる割り込み。`running`/`reviewing → ready`、
    /// attempts は据え置き（人が口を出しただけで試行を 1 回使わせない）。`Outcome::reason` は
    /// **`"comment"`**（`name()` の `"interrupt"` とは別。ADR-0044 D2 の表がそう書いている）。
    Interrupt,
    /// ADR-0044 D2（Phase 53）: 終端のタスクの再開（`POST /tasks/{id}/reopen`）。
    /// `done`/`failed → ready`、attempts は 0 に戻す。`cancelled` は再開しない（worktree が無い）。
    Reopen,
    /// ADR-0044 D6（Phase 55）: 案件の中止による連鎖。遷移は `Cancel` と同じ（非終端 → `cancelled`、
    /// attempts 据え置き）で、`Event::Transitioned.reason` だけが `"project_cancelled"` になる
    /// （タイムラインで「自分が止めたのか、案件ごと止まったのか」が読めるように）。
    ProjectCancelled,
    /// ADR-0044 D6（Phase 55）: 途中目標の中止による連鎖。理由は `"milestone_cancelled"`。
    MilestoneCancelled,
    /// ADR-0046 D5（Phase 59）: 担当が決まらない（matching の候補が 1 つも無い）タスク:
    /// `ready → blocked`、attempts 据え置き。人が組織を直すか担当を指定したら `Answer` で再開する
    /// （ADR-0021 の質問経路と同じ出口）。
    Unroutable,
}

impl Trigger {
    /// snake_case の trigger 名。成功時の `Outcome::reason` および失敗時の
    /// `InvalidTransition` のメッセージに使う。
    pub fn name(&self) -> &'static str {
        match self {
            Trigger::Accept => "accept",
            Trigger::Dispatch => "dispatch",
            Trigger::WorkerDone => "worker_done",
            Trigger::WorkerQuestion => "worker_question",
            Trigger::WorkerError { .. } => "worker_error",
            Trigger::LeaseExpired => "lease_expired",
            Trigger::ReviewPass => "review_pass",
            Trigger::ReviewFail => "review_fail",
            Trigger::Answer => "answer",
            Trigger::Approve => "approve",
            Trigger::Reject => "reject",
            Trigger::Cancel => "cancel",
            Trigger::Requeue => "requeue",
            Trigger::DependencyFailed => "dependency_failed",
            Trigger::Aggregate => "aggregate",
            Trigger::ChildFailed => "child_failed",
            Trigger::Interrupt => "interrupt",
            Trigger::Reopen => "reopen",
            Trigger::ProjectCancelled => "project_cancelled",
            Trigger::MilestoneCancelled => "milestone_cancelled",
            Trigger::Unroutable => "unroutable",
        }
    }

    /// `Outcome::reason`（`Event::Transitioned.reason` に入る文字列）。ふつうは `name()` と同じだが、
    /// `Interrupt` だけは ADR-0044 D2 の表のとおり `"comment"`（「なぜ ready に戻ったか」を人が読む）。
    pub fn reason(&self) -> &'static str {
        match self {
            Trigger::Interrupt => "comment",
            other => other.name(),
        }
    }
}

/// 遷移が成功した場合の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub next: Status,
    pub attempts: u32,
    pub reason: &'static str,
}

/// 遷移が許可されていない場合のエラー。失敗した `(status, kind, trigger)` を
/// 保持し、メッセージから追跡できるようにする。
#[derive(Debug, thiserror::Error)]
#[error("invalid transition: status={status:?} kind={kind:?} trigger={trigger}")]
pub struct InvalidTransition {
    pub status: Status,
    pub kind: TaskKind,
    pub trigger: &'static str,
}

fn invalid(s: &StateView, t: &Trigger) -> InvalidTransition {
    InvalidTransition {
        status: s.status,
        kind: s.kind,
        trigger: t.name(),
    }
}

/// リトライ判定込みの `attempts` 更新: `attempts + 1` が `max_retries` を
/// 超えたら `Failed`、超えなければ `Ready` を返す。
fn retry_or_fail(s: &StateView, reason: &'static str) -> Outcome {
    let attempts = s.attempts + 1;
    let next = if attempts > s.max_retries {
        Status::Failed
    } else {
        Status::Ready
    };
    Outcome {
        next,
        attempts,
        reason,
    }
}

/// タスクの状態機械。DESIGN.md / ADR-0002 D2,D3,D8 の遷移表を実装する純粋関数。
pub fn transition(s: &StateView, t: &Trigger) -> Result<Outcome, InvalidTransition> {
    match t {
        // ADR-0010 D1（P-4 / P-9）: Cancel と DependencyFailed は非終端状態からのみ cancelled へ。
        // ADR-0044 D6（Phase 55）: 案件・途中目標の中止による連鎖も同じ遷移（理由だけが違う）。
        Trigger::Cancel
        | Trigger::DependencyFailed
        | Trigger::ProjectCancelled
        | Trigger::MilestoneCancelled => {
            if s.status.is_terminal() {
                Err(invalid(s, t))
            } else {
                Ok(Outcome {
                    next: Status::Cancelled,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            }
        }

        // ADR-0010 D1（P-21）: 供給側失敗は attempts を消費せず ready に戻す。
        Trigger::Requeue => {
            if s.status == Status::Running {
                Ok(Outcome {
                    next: Status::Ready,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        // ADR-0016 M1: 集約 run は attempts を消費せず ready に戻す（reviewing からのみ）。
        Trigger::Aggregate => {
            if s.status == Status::Reviewing {
                Ok(Outcome {
                    next: Status::Ready,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        // ADR-0021 D1: 子の失敗は親が引き継がない。やり直せるなら ready、やり直せないなら人間に投げる（blocked）。
        Trigger::ChildFailed => {
            if s.status == Status::Reviewing {
                let attempts = s.attempts + 1;
                if attempts <= s.max_retries {
                    Ok(Outcome {
                        next: Status::Ready,
                        attempts,
                        reason: t.name(),
                    })
                } else {
                    // attempts は据え置き（人が答えたら、その回答を持って走り直せるようにする）。
                    Ok(Outcome {
                        next: Status::Blocked,
                        attempts: s.attempts,
                        reason: t.name(),
                    })
                }
            } else {
                Err(invalid(s, t))
            }
        }

        // ADR-0044 D2: 人のコメントで走っている run を止める。attempts は据え置き、理由は `comment`。
        Trigger::Interrupt => {
            if matches!(s.status, Status::Running | Status::Reviewing) {
                Ok(Outcome {
                    next: Status::Ready,
                    attempts: s.attempts,
                    reason: t.reason(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        // ADR-0044 D2: 終端のタスクの再開。`done`/`failed` からだけ（`cancelled` は worktree が無い）。
        Trigger::Reopen => {
            if matches!(s.status, Status::Done | Status::Failed) {
                Ok(Outcome {
                    next: Status::Ready,
                    attempts: 0,
                    reason: t.reason(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::Accept => {
            if s.status == Status::Draft {
                Ok(Outcome {
                    next: Status::Ready,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::Dispatch => {
            if s.status == Status::Ready && s.kind != TaskKind::Approval {
                Ok(Outcome {
                    next: Status::Running,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::WorkerDone => {
            if s.status == Status::Running {
                Ok(Outcome {
                    next: Status::Reviewing,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::WorkerQuestion => {
            if s.status == Status::Running {
                Ok(Outcome {
                    next: Status::Blocked,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        // ADR-0046 D5（Phase 59）: 担当が見つからないタスクは人に聞く（`ready → blocked`）。
        Trigger::Unroutable => {
            if s.status == Status::Ready {
                Ok(Outcome {
                    next: Status::Blocked,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::WorkerError { retryable } => {
            if s.status == Status::Running {
                let attempts = s.attempts + 1;
                let next = if *retryable && attempts <= s.max_retries {
                    Status::Ready
                } else {
                    Status::Failed
                };
                Ok(Outcome {
                    next,
                    attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::LeaseExpired => {
            if s.status == Status::Running {
                Ok(retry_or_fail(s, t.name()))
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::ReviewPass => {
            if s.status == Status::Reviewing {
                Ok(Outcome {
                    next: Status::Done,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::ReviewFail => {
            if s.status == Status::Reviewing {
                Ok(retry_or_fail(s, t.name()))
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::Answer => {
            if s.status == Status::Blocked {
                Ok(Outcome {
                    next: Status::Ready,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::Approve => {
            if s.kind == TaskKind::Approval && s.status == Status::Ready {
                Ok(Outcome {
                    next: Status::Done,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }

        Trigger::Reject => {
            if s.kind == TaskKind::Approval && s.status == Status::Ready {
                Ok(Outcome {
                    next: Status::Failed,
                    attempts: s.attempts,
                    reason: t.name(),
                })
            } else {
                Err(invalid(s, t))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [TaskKind; 4] = [
        TaskKind::Plan,
        TaskKind::Execute,
        TaskKind::Review,
        TaskKind::Approval,
    ];

    const ALL_STATUSES: [Status; 8] = [
        Status::Draft,
        Status::Ready,
        Status::Running,
        Status::Blocked,
        Status::Reviewing,
        Status::Done,
        Status::Failed,
        Status::Cancelled,
    ];

    /// テーブル駆動テストの1トリガー分の期待値。`None` は `Err` を期待する。
    struct Expected {
        next: Option<Status>,
    }

    fn expect_ok(next: Status) -> Expected {
        Expected { next: Some(next) }
    }

    fn expect_err() -> Expected {
        Expected { next: None }
    }

    /// `attempts`/`max_retries` を絡めない単純トリガーについて、仕様表を
    /// そのまま再現した期待値を返す。
    fn expected_simple(kind: TaskKind, status: Status, trigger: &Trigger) -> Expected {
        match trigger {
            // ADR-0010 D1: 非終端からのみ。
            Trigger::Cancel | Trigger::DependencyFailed => {
                if status.is_terminal() {
                    expect_err()
                } else {
                    expect_ok(Status::Cancelled)
                }
            }
            Trigger::Requeue => {
                if status == Status::Running {
                    expect_ok(Status::Ready)
                } else {
                    expect_err()
                }
            }
            Trigger::Aggregate => {
                if status == Status::Reviewing {
                    expect_ok(Status::Ready)
                } else {
                    expect_err()
                }
            }
            // ADR-0044 D2: 人のコメントの割り込みは `running`/`reviewing` からだけ。
            Trigger::Interrupt => {
                if matches!(status, Status::Running | Status::Reviewing) {
                    expect_ok(Status::Ready)
                } else {
                    expect_err()
                }
            }
            // ADR-0044 D2: 再開は `done`/`failed` からだけ（`cancelled` は不可）。
            Trigger::Reopen => {
                if matches!(status, Status::Done | Status::Failed) {
                    expect_ok(Status::Ready)
                } else {
                    expect_err()
                }
            }
            Trigger::Accept => {
                if status == Status::Draft {
                    expect_ok(Status::Ready)
                } else {
                    expect_err()
                }
            }
            Trigger::Dispatch => {
                if status == Status::Ready && kind != TaskKind::Approval {
                    expect_ok(Status::Running)
                } else {
                    expect_err()
                }
            }
            Trigger::WorkerDone => {
                if status == Status::Running {
                    expect_ok(Status::Reviewing)
                } else {
                    expect_err()
                }
            }
            Trigger::WorkerQuestion => {
                if status == Status::Running {
                    expect_ok(Status::Blocked)
                } else {
                    expect_err()
                }
            }
            // ADR-0046 D5（Phase 59）: 担当が見つからない `ready` のタスクだけが `blocked` になる。
            Trigger::Unroutable => {
                if status == Status::Ready {
                    expect_ok(Status::Blocked)
                } else {
                    expect_err()
                }
            }
            Trigger::ReviewPass => {
                if status == Status::Reviewing {
                    expect_ok(Status::Done)
                } else {
                    expect_err()
                }
            }
            Trigger::Answer => {
                if status == Status::Blocked {
                    expect_ok(Status::Ready)
                } else {
                    expect_err()
                }
            }
            Trigger::Approve => {
                if kind == TaskKind::Approval && status == Status::Ready {
                    expect_ok(Status::Done)
                } else {
                    expect_err()
                }
            }
            Trigger::Reject => {
                if kind == TaskKind::Approval && status == Status::Ready {
                    expect_ok(Status::Failed)
                } else {
                    expect_err()
                }
            }
            _ => unreachable!("handled by retry-aware helper"),
        }
    }

    /// status,kind × 単純トリガー(12種)の直積を全網羅する。
    #[test]
    fn table_simple_triggers_full_cross_product() {
        let simple_triggers = [
            Trigger::Accept,
            Trigger::Dispatch,
            Trigger::WorkerDone,
            Trigger::WorkerQuestion,
            Trigger::ReviewPass,
            Trigger::Answer,
            Trigger::Approve,
            Trigger::Reject,
            Trigger::Cancel,
            Trigger::Requeue,
            Trigger::DependencyFailed,
            Trigger::Aggregate,
            // ADR-0044 D2（Phase 53）: 割り込みと再開も attempts を絡めない（据え置き / 0 に戻す）。
            Trigger::Interrupt,
            Trigger::Reopen,
            // ADR-0046 D5（Phase 59）: 担当が決まらない `ready` → `blocked`（attempts 据え置き）。
            Trigger::Unroutable,
        ];

        let mut count = 0usize;
        for kind in ALL_KINDS {
            for status in ALL_STATUSES {
                for trigger in &simple_triggers {
                    count += 1;
                    let s = StateView {
                        kind,
                        status,
                        attempts: 0,
                        max_retries: 3,
                    };
                    let expected = expected_simple(kind, status, trigger);
                    let got = transition(&s, trigger);
                    match expected.next {
                        Some(next) => {
                            let outcome = got.unwrap_or_else(|e| {
                                panic!(
                                    "expected Ok(next={next:?}) for kind={kind:?} status={status:?} trigger={trigger:?}, got Err({e})"
                                )
                            });
                            assert_eq!(
                                outcome.next, next,
                                "kind={kind:?} status={status:?} trigger={trigger:?}"
                            );
                            assert_eq!(
                                outcome.attempts, 0,
                                "attempts should be unchanged: kind={kind:?} status={status:?} trigger={trigger:?}"
                            );
                            // ADR-0044 D2: `Interrupt` だけ reason が name と違う（`"comment"`）。
                            assert_eq!(outcome.reason, trigger.reason());
                        }
                        None => {
                            let err = got.unwrap_err();
                            assert_eq!(err.status, status);
                            assert_eq!(err.kind, kind);
                            assert_eq!(err.trigger, trigger.name());
                        }
                    }
                }
            }
        }
        // 4 kinds * 8 statuses * 15 triggers（Phase 53 で Interrupt / Reopen、Phase 59 で Unroutable を追加）
        assert_eq!(count, 4 * 8 * 15);
    }

    /// ADR-0044 D2（Phase 53）: 割り込みは attempts を消費せず理由は `comment`、再開は attempts を 0 に戻す。
    /// `cancelled` は再開できない（worktree が無い）。
    #[test]
    fn interrupt_keeps_attempts_and_reopen_resets_them() {
        for kind in ALL_KINDS {
            for status in [Status::Running, Status::Reviewing] {
                let s = StateView {
                    kind,
                    status,
                    attempts: 2,
                    max_retries: 2,
                };
                let outcome = transition(&s, &Trigger::Interrupt).unwrap_or_else(|e| {
                    panic!("expected Ok for {kind:?}/{status:?}, got Err({e})")
                });
                assert_eq!(outcome.next, Status::Ready);
                assert_eq!(outcome.attempts, 2, "割り込みは試行を 1 回使わせない");
                assert_eq!(outcome.reason, "comment");
            }
            for status in [Status::Done, Status::Failed] {
                let s = StateView {
                    kind,
                    status,
                    attempts: 5,
                    max_retries: 2,
                };
                let outcome = transition(&s, &Trigger::Reopen).unwrap_or_else(|e| {
                    panic!("expected Ok for {kind:?}/{status:?}, got Err({e})")
                });
                assert_eq!(outcome.next, Status::Ready);
                assert_eq!(outcome.attempts, 0, "再開は attempts を 0 に戻す");
                assert_eq!(outcome.reason, "reopen");
            }
            let cancelled = StateView {
                kind,
                status: Status::Cancelled,
                attempts: 0,
                max_retries: 2,
            };
            let err = transition(&cancelled, &Trigger::Reopen).unwrap_err();
            assert_eq!(err.trigger, "reopen");
            assert_eq!(err.status, Status::Cancelled);
        }
    }

    /// リトライ判定を含むトリガー (WorkerError{true/false}, LeaseExpired,
    /// ReviewFail) を、境界値 (attempts' <= max_retries / > max_retries) を
    /// 含めて網羅する。
    #[test]
    fn table_retry_triggers_full_cross_product() {
        // (attempts, max_retries, expect_retry) の組。境界値ケースと
        // max_retries = 0 の即失敗ケースを含む。
        let retry_cases: [(u32, u32, bool); 4] = [
            // attempts' = attempts + 1 <= max_retries -> リトライ (Ready)
            (0, 1, true),
            // attempts' = attempts + 1 > max_retries -> 失敗 (Failed)
            (1, 1, false),
            // max_retries = 0 の即失敗
            (0, 0, false),
            // 余裕のあるリトライ境界
            (2, 3, true),
        ];

        let retry_triggers_non_worker_error = [Trigger::LeaseExpired, Trigger::ReviewFail];

        let mut count = 0usize;

        for kind in ALL_KINDS {
            for status in ALL_STATUSES {
                // LeaseExpired: Running でのみ成功
                for &(attempts, max_retries, expect_retry) in &retry_cases {
                    for trigger in &retry_triggers_non_worker_error {
                        count += 1;
                        let required_status = match trigger {
                            Trigger::LeaseExpired => Status::Running,
                            Trigger::ReviewFail => Status::Reviewing,
                            _ => unreachable!(),
                        };
                        let s = StateView {
                            kind,
                            status,
                            attempts,
                            max_retries,
                        };
                        let got = transition(&s, trigger);
                        if status == required_status {
                            let outcome = got.unwrap_or_else(|e| {
                                panic!(
                                    "expected Ok for kind={kind:?} status={status:?} trigger={trigger:?} attempts={attempts} max_retries={max_retries}, got Err({e})"
                                )
                            });
                            let expected_next = if expect_retry {
                                Status::Ready
                            } else {
                                Status::Failed
                            };
                            assert_eq!(outcome.next, expected_next);
                            assert_eq!(outcome.attempts, attempts + 1);
                            assert_eq!(outcome.reason, trigger.name());
                        } else {
                            let err = got.unwrap_err();
                            assert_eq!(err.status, status);
                            assert_eq!(err.kind, kind);
                            assert_eq!(err.trigger, trigger.name());
                        }
                    }
                }

                // ADR-0021 D1: ChildFailed は Reviewing でのみ成功。やり直せるなら Ready（attempts +1）、
                // やり直せないなら **Failed ではなく Blocked**（attempts 据え置き）。
                for &(attempts, max_retries, expect_retry) in &retry_cases {
                    count += 1;
                    let s = StateView {
                        kind,
                        status,
                        attempts,
                        max_retries,
                    };
                    let got = transition(&s, &Trigger::ChildFailed);
                    if status == Status::Reviewing {
                        let outcome = got.unwrap_or_else(|e| {
                            panic!(
                                "expected Ok for kind={kind:?} status={status:?} trigger=ChildFailed attempts={attempts} max_retries={max_retries}, got Err({e})"
                            )
                        });
                        if expect_retry {
                            assert_eq!(outcome.next, Status::Ready);
                            assert_eq!(outcome.attempts, attempts + 1);
                        } else {
                            assert_eq!(
                                outcome.next,
                                Status::Blocked,
                                "子の失敗で親を failed にしない"
                            );
                            assert_eq!(
                                outcome.attempts, attempts,
                                "人の回答を待つ間は attempts を増やさない"
                            );
                        }
                        assert_eq!(outcome.reason, "child_failed");
                    } else {
                        let err = got.unwrap_err();
                        assert_eq!(err.status, status);
                        assert_eq!(err.kind, kind);
                        assert_eq!(err.trigger, "child_failed");
                    }
                }

                // WorkerError{retryable}: Running でのみ成功。
                // retryable=false は常に Failed (retry 判定を無視)。
                for &(attempts, max_retries, expect_retry) in &retry_cases {
                    for retryable in [true, false] {
                        count += 1;
                        let trigger = Trigger::WorkerError { retryable };
                        let s = StateView {
                            kind,
                            status,
                            attempts,
                            max_retries,
                        };
                        let got = transition(&s, &trigger);
                        if status == Status::Running {
                            let outcome = got.unwrap_or_else(|e| {
                                panic!(
                                    "expected Ok for kind={kind:?} status={status:?} trigger=WorkerError{{retryable:{retryable}}} attempts={attempts} max_retries={max_retries}, got Err({e})"
                                )
                            });
                            let expected_next = if retryable && expect_retry {
                                Status::Ready
                            } else {
                                Status::Failed
                            };
                            assert_eq!(outcome.next, expected_next);
                            assert_eq!(outcome.attempts, attempts + 1);
                            assert_eq!(outcome.reason, "worker_error");
                        } else {
                            let err = got.unwrap_err();
                            assert_eq!(err.status, status);
                            assert_eq!(err.kind, kind);
                            assert_eq!(err.trigger, "worker_error");
                        }
                    }
                }
            }
        }

        // 4 kinds * 8 statuses * (4 retry_cases * 2 non_worker_error_triggers + 4 retry_cases * 1 child_failed
        //                          + 4 retry_cases * 2 worker_error retryable)
        assert_eq!(count, 4 * 8 * (4 * 2 + 4 + 4 * 2));
    }

    /// 仕様の境界値の具体例をそのままテストする:
    /// max_retries=1, attempts=0 で WorkerError{retryable:true} -> Ready(1回目リトライ)
    /// その後 attempts=1 で再度失敗 -> Failed(2回目)
    #[test]
    fn worker_error_retry_then_fail_example_from_spec() {
        let s0 = StateView {
            kind: TaskKind::Execute,
            status: Status::Running,
            attempts: 0,
            max_retries: 1,
        };
        let o0 = transition(&s0, &Trigger::WorkerError { retryable: true }).unwrap_or_else(|e| {
            panic!("expected Ok, got Err({e})");
        });
        assert_eq!(o0.next, Status::Ready);
        assert_eq!(o0.attempts, 1);

        let s1 = StateView {
            kind: TaskKind::Execute,
            status: Status::Running,
            attempts: 1,
            max_retries: 1,
        };
        let o1 = transition(&s1, &Trigger::WorkerError { retryable: true }).unwrap_or_else(|e| {
            panic!("expected Ok, got Err({e})");
        });
        assert_eq!(o1.next, Status::Failed);
        assert_eq!(o1.attempts, 2);
    }

    /// ADR-0010 D1（P-4 / P-9 / P-21）: Cancel と DependencyFailed は非終端からのみ成功し attempts を保つ。
    /// Requeue は running からのみ ready へ戻り attempts を保つ（max_retries に達していても failed にしない）。
    #[test]
    fn cancel_dependency_failed_and_requeue_keep_attempts() {
        for kind in ALL_KINDS {
            for status in ALL_STATUSES {
                let s = StateView {
                    kind,
                    status,
                    attempts: 5,
                    max_retries: 5,
                };
                for trigger in [Trigger::Cancel, Trigger::DependencyFailed] {
                    match transition(&s, &trigger) {
                        Ok(outcome) => {
                            assert!(
                                !status.is_terminal(),
                                "{trigger:?} from terminal {status:?} must be invalid"
                            );
                            assert_eq!(outcome.next, Status::Cancelled);
                            assert_eq!(outcome.attempts, 5);
                            assert_eq!(outcome.reason, trigger.name());
                        }
                        Err(_) => assert!(
                            status.is_terminal(),
                            "{trigger:?} from {status:?} must be valid"
                        ),
                    }
                }
                match transition(&s, &Trigger::Requeue) {
                    Ok(outcome) => {
                        assert_eq!(status, Status::Running);
                        assert_eq!(outcome.next, Status::Ready);
                        assert_eq!(outcome.attempts, 5);
                        assert_eq!(outcome.reason, "requeue");
                    }
                    Err(_) => assert_ne!(status, Status::Running),
                }
            }
        }
    }

    /// Dispatch は Approval kind の場合、Ready であっても失敗する。
    #[test]
    fn dispatch_rejects_approval_kind_even_when_ready() {
        let s = StateView {
            kind: TaskKind::Approval,
            status: Status::Ready,
            attempts: 0,
            max_retries: 3,
        };
        let err = transition(&s, &Trigger::Dispatch).unwrap_err();
        assert_eq!(err.status, Status::Ready);
        assert_eq!(err.kind, TaskKind::Approval);
        assert_eq!(err.trigger, "dispatch");
    }

    /// InvalidTransition のメッセージに (status, kind, trigger) が含まれる。
    #[test]
    fn invalid_transition_message_contains_status_kind_trigger() {
        let s = StateView {
            kind: TaskKind::Plan,
            status: Status::Done,
            attempts: 0,
            max_retries: 3,
        };
        let err = transition(&s, &Trigger::Accept).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Done"));
        assert!(msg.contains("Plan"));
        assert!(msg.contains("accept"));
    }
}
