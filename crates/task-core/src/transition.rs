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
    WorkerError { retryable: bool },
    LeaseExpired,
    ReviewPass,
    ReviewFail,
    Answer,
    Approve,
    Reject,
    Cancel,
}

impl Trigger {
    /// snake_case の trigger 名。成功時の `Outcome::reason` および失敗時の
    /// `InvalidTransition` のメッセージに使う。
    fn name(&self) -> &'static str {
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
        // Cancel はどの status からでも常に成功する自己遷移も含む万能トリガー。
        Trigger::Cancel => Ok(Outcome {
            next: Status::Cancelled,
            attempts: s.attempts,
            reason: t.name(),
        }),

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
            Trigger::Cancel => expect_ok(Status::Cancelled),
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

    /// status,kind × 単純トリガー(9種)の直積を全網羅する。
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
                            assert_eq!(outcome.reason, trigger.name());
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
        // 4 kinds * 8 statuses * 9 triggers
        assert_eq!(count, 4 * 8 * 9);
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

        // 4 kinds * 8 statuses * (4 retry_cases * 2 non_worker_error_triggers + 4 retry_cases * 2 worker_error retryable)
        assert_eq!(count, 4 * 8 * (4 * 2 + 4 * 2));
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

    /// Cancel は終端状態からの自己遷移も含めて常に成功する。
    #[test]
    fn cancel_always_succeeds_including_terminal_self_transition() {
        for kind in ALL_KINDS {
            for status in ALL_STATUSES {
                let s = StateView {
                    kind,
                    status,
                    attempts: 2,
                    max_retries: 5,
                };
                let outcome = transition(&s, &Trigger::Cancel).unwrap_or_else(|e| {
                    panic!("Cancel must always succeed, got Err({e}) for kind={kind:?} status={status:?}");
                });
                assert_eq!(outcome.next, Status::Cancelled);
                assert_eq!(outcome.attempts, 2);
                assert_eq!(outcome.reason, "cancel");
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
