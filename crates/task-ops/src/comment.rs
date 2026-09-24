//! ADR-0044 D2（Phase 53）: タスク単位のコメントと、**人のコメントの効き方**（決定的）。
//!
//! | タスクの状態 | 何が起きる |
//! |---|---|
//! | `running` / `reviewing` | `Trigger::Interrupt` で `ready` に戻す（attempts 据え置き、理由 `comment`）。
//!   `Event::WorkerFinished{outcome: "interrupted: comment"}` を同じトランザクションで積む。報告は作らない |
//! | `blocked` | `answer` と同じ（ADR-0021 D2。質問への回答として渡す） |
//! | `ready` / `draft` | コメントを記録するだけ（次の run の前置きに載る） |
//! | `done` / `failed` / `cancelled` | コメントを記録するだけ（GUI が「再開」を出す。`reopen` は別の API） |
//!
//! 走っている run を実際に殺すのは**ディスパッチャ**（`abort_stale_runs`: ストア上で `running` で
//! なくなった run の `JoinHandle` を `abort()` する。tokio の `kill_on_drop` が**子プロセスに SIGKILL**
//! を送る）。**cancel と同じ経路**で、ADR-0044 D2 の「SIGTERM → `kill_grace_secs`」には**まだなって
//! いない**（cancel も昔から同じ。孫プロセスは残る。`docs/PROGRESS.md` の提案 P-53a）。
//! 打ち切ったタスクは同じ tick では dispatch し直さない（同じ worktree に 2 つの書き手を入れないため）。
//! ここは状態とコメントだけを決定的に書く。LLM は呼ばない（DESIGN 原則 1）。

use task_core::comment::{CommentAuthorKind, TaskComment};
use task_core::{Event, Status, TaskId, TaskStore, Trigger};
use time::OffsetDateTime;

use crate::error::OpsError;
use crate::gate::{self, TransitionResult};

/// 割り込みで止めた run の `WorkerFinished.outcome`（ADR-0044 D2 / D8）。
/// **`bad_news` にも `error_cooldown` にも数えない**（`classify_outcome` が `Interrupted` に分類する）。
pub const INTERRUPTED_OUTCOME: &str = "interrupted: comment";

/// 割り込みで止めた run の `outcome` の接頭辞（`view::classify_outcome` / `stats::classify_outcome` と対）。
pub const INTERRUPTED_OUTCOME_PREFIX: &str = "interrupted: ";

/// 人のコメントが何を起こしたか（ADR-0044 D2 の表）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CommentEffect {
    /// 記録しただけ（`ready` / `draft`）。
    Stored,
    /// 走っていた run を止めて `ready` に戻した（`running` / `reviewing`）。
    Interrupted,
    /// 質問への回答として渡した（`blocked`）。
    Answered,
    /// 終端なので記録しただけ。GUI は「再開」を出せる（`done` / `failed`。`cancelled` は再開できない）。
    Terminal,
}

/// `POST /tasks/{id}/comments` の結果。
#[derive(Debug, Clone, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct CommentResult {
    pub comment: TaskComment,
    pub effect: CommentEffect,
    /// 状態が動いたときだけ（`Interrupted` / `Answered`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<TransitionResult>,
    /// `Terminal` のとき、`POST /tasks/{id}/reopen` が使えるか（`cancelled` は `false`）。
    pub can_reopen: bool,
}

/// 人のコメント（ADR-0044 D2）。状態に応じて割り込み・回答・記録を決定的に行う。
pub fn post_human_comment(
    store: &dyn TaskStore,
    id: TaskId,
    body: String,
    now: OffsetDateTime,
) -> Result<CommentResult, OpsError> {
    post_human_comment_as(store, id, None, body, now)
}

/// ADR-0056 Phase 101: `post_human_comment` と同じ効き方（`ADR-0044 D2` の表）だが、
/// `author`（`CommentAuthorKind::Human` の `TaskComment.author`）を明示できる。MCP の
/// `task_comment` は `Some("mcp:<client_id>")` を渡す（`knowledge_propose` の `mcp:chatgpt` と
/// 同じ流儀）。GUI/HTTP の `POST /tasks/{id}/comments`（`post_human_comment`）は `None` のまま
/// （挙動もフィールドの見え方も、この Phase では 1 バイトも変えない）。
pub fn post_human_comment_as(
    store: &dyn TaskStore,
    id: TaskId,
    author: Option<String>,
    body: String,
    now: OffsetDateTime,
) -> Result<CommentResult, OpsError> {
    TaskComment::validate_body(&body).map_err(OpsError::Validation)?;
    let task = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    let comment = TaskComment::new(
        id,
        CommentAuthorKind::Human,
        author,
        body.clone(),
        None,
        now,
    );

    match task.status {
        // 走っている（レビュー中も含む）run を止めて `ready` に戻す。コメントと遷移は同じトランザクション。
        Status::Running | Status::Reviewing => {
            // `WorkerFinished{outcome: "interrupted: comment"}` は**いま走っているワーカー run**
            // （= リースの `worker_run_id`）にだけ付ける。`reviewing` のタスクのリースは既に外れていて、
            // 直近のワーカー run は `done: …` で終わっているので、そこに後から `interrupted` を重ねると
            // その run の記録（`outcome` / `finished_at` / `usage`）を壊す（Phase 53 の監査で発見）。
            // `reviewing` の割り込みではイベントを足さず、遷移（`Transitioned{reason:"comment"}`）だけ残す。
            let extra = match task.lease.as_ref().map(|l| l.worker_run_id.clone()) {
                Some(run_id) => vec![Event::WorkerFinished {
                    run_id,
                    outcome: INTERRUPTED_OUTCOME.to_string(),
                    usage: None,
                    role: None,
                    metrics: None,
                }],
                None => Vec::new(),
            };
            let from = task.status;
            let outcome = store
                .comment_add(&comment, Some((Trigger::Interrupt, extra)))?
                .ok_or_else(|| OpsError::Validation("interrupt produced no outcome".to_string()))?;
            Ok(CommentResult {
                comment,
                effect: CommentEffect::Interrupted,
                transition: Some(TransitionResult {
                    id,
                    from,
                    to: outcome.next,
                    reason: outcome.reason.to_string(),
                    cascaded: Vec::new(),
                }),
                can_reopen: false,
            })
        }
        // ADR-0021 D2: 質問待ちのタスクへのコメントは「回答」そのもの。
        // コメント・`Trigger::Answer`・`Event::Answered` を**同じトランザクション**で書く
        // （`gate::answer` を呼ぶと 2 つ目の tx になり、その間に blocked を抜けると
        // 「コメントだけ残って回答が消える」ので。Phase 53 の監査で発見）。
        Status::Blocked => {
            let question = crate::derive::latest_question(&store.events_for(id)?);
            let answered = Event::Answered {
                question,
                answer: body.clone(),
            };
            let outcome = store
                .comment_add(&comment, Some((Trigger::Answer, vec![answered])))?
                .ok_or_else(|| OpsError::Validation("answer produced no outcome".to_string()))?;
            // GUI 監査 H2 と同じ後始末（`gate::answer` と同じ関数。別の書き込みで、冪等）。
            gate::settle_pending_approvals(store, id, &body)?;
            Ok(CommentResult {
                comment,
                effect: CommentEffect::Answered,
                transition: Some(TransitionResult {
                    id,
                    from: Status::Blocked,
                    to: outcome.next,
                    reason: outcome.reason.to_string(),
                    cascaded: Vec::new(),
                }),
                can_reopen: false,
            })
        }
        Status::Done | Status::Failed | Status::Cancelled => {
            store.comment_add(&comment, None)?;
            Ok(CommentResult {
                comment,
                effect: CommentEffect::Terminal,
                transition: None,
                can_reopen: task.status != Status::Cancelled,
            })
        }
        Status::Draft | Status::Ready => {
            store.comment_add(&comment, None)?;
            Ok(CommentResult {
                comment,
                effect: CommentEffect::Stored,
                transition: None,
                can_reopen: false,
            })
        }
    }
}

/// 組織の「人」（ワーカーの `{"type":"comment"}` 行、秘書・lead が委譲先に書くもの）のコメント。
/// **人を起こさない**（状態は変えない。通知は ADR-0037 の 5 種のまま）。
pub fn post_node_comment(
    store: &dyn TaskStore,
    id: TaskId,
    author: Option<String>,
    run_id: Option<String>,
    body: String,
    now: OffsetDateTime,
) -> Result<TaskComment, OpsError> {
    TaskComment::validate_body(&body).map_err(OpsError::Validation)?;
    let comment = TaskComment::new(id, CommentAuthorKind::Node, author, body, run_id, now);
    store.comment_add(&comment, None)?;
    Ok(comment)
}

/// そのタスクのコメント（古い順）。
pub fn list_comments(store: &dyn TaskStore, id: TaskId) -> Result<Vec<TaskComment>, OpsError> {
    if store.get(id)?.is_none() {
        return Err(OpsError::NotFound(id));
    }
    Ok(store.comments_for(id)?)
}

/// ADR-0044 D2: `POST /tasks/{id}/reopen`。`done` / `failed` を `ready` に戻す（attempts は 0）。
/// `cancelled` は worktree もブランチも消してあるので再開しない（409）。
pub fn reopen(
    store: &dyn TaskStore,
    id: TaskId,
    expected: Option<Status>,
) -> Result<TransitionResult, OpsError> {
    let task = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    if let Some(exp) = expected
        && exp != task.status
    {
        return Err(OpsError::Conflict {
            expected: exp,
            actual: task.status,
        });
    }
    if !matches!(task.status, Status::Done | Status::Failed) {
        return Err(OpsError::InvalidState {
            id,
            context: format!("status={:?}", task.status),
            action: "reopened; only done or failed tasks can be reopened".to_string(),
        });
    }
    let from = task.status;
    let outcome = store.apply_transition(id, Trigger::Reopen, None)?;
    Ok(TransitionResult {
        id,
        from,
        to: outcome.next,
        reason: outcome.reason.to_string(),
        cascaded: Vec::new(),
    })
}

/// ADR-0044 D2: **直前の run を止めた人のコメント**（次の run の前置きの先頭に載せるもの）。
///
/// 決定的な規則（純粋関数）:
/// 1. 最後の `Transitioned{reason: "comment", to: ready}`（= `Interrupt`）を探す。無ければ `None`。
/// 2. それより後に「**その割り込みを受け取った run が終わった**」印があれば、もう消化済みなので `None`。
///    印は `WorkerFinished` のうち、`interrupted: ` で始まるもの（割り込みそのもの）・`requeue: `
///    （ADR-0010 P-21: 供給側の失敗。モデルは前置きを見ていない）・`lease_expired`（デーモンの死）を
///    **除いた**もの。`Transitioned{to: running}`（dispatch）や `WorkerStarted` も消化ではない
///    （割り込みの次の run は、まさにこの前置きを受け取る run だから）。
///    requeue / lease_expired を除かないと、割り込みの直後にレート制限が 1 回起きただけで
///    「前置きの先頭に載る」が消える（Phase 53 の監査で発見）。
/// 3. 残っていれば、その割り込みを起こした人のコメント = 最後の `author_kind = human` のコメント
///    （割り込みの後に人がさらに書き足していれば、その最新のもの。どちらも人が読ませたい文なので
///    先頭に出してよい、という判断）。
pub fn interrupting_comment<'a>(
    events: &[(u64, Event)],
    comments: &'a [TaskComment],
) -> Option<&'a TaskComment> {
    let idx = events.iter().rposition(|(_, e)| {
        matches!(e, Event::Transitioned { reason, to, .. } if reason == "comment" && *to == Status::Ready)
    })?;
    let consumed = events[idx + 1..].iter().any(
        |(_, e)| matches!(e, Event::WorkerFinished { outcome, .. } if consumes_interrupt(outcome)),
    );
    if consumed {
        return None;
    }
    comments
        .iter()
        .rev()
        .find(|c| c.author_kind == CommentAuthorKind::Human)
}

/// `WorkerFinished.outcome` が「割り込みを受け取った run が実際に走って終わった」印か（純粋関数）。
/// 割り込みそのもの・供給側の requeue・リース切れは**走っていない**ので印にならない。
fn consumes_interrupt(outcome: &str) -> bool {
    !outcome.starts_with(INTERRUPTED_OUTCOME_PREFIX)
        && !outcome.starts_with("requeue: ")
        && outcome != "lease_expired"
}

/// ADR-0054 Phase 113 D3: そのタスクが `failed` になった直近の遷移が `Trigger::ReviewFail`
/// （`Event::Transitioned{reason: "review_fail", to: Failed}`）か（純粋関数）。`ReviewFail` は
/// `task.status == Reviewing` のときしか発火せず（`transition.rs`）、`Reviewing` に入るのは実装 run が
/// `done`（`Trigger::WorkerDone`）した後だけなので、これが `true` なら「実装は最後まで通ったが、
/// review の判定（reviewer 条件・command 条件・human 条件のどれか）が不合格だった」ことを意味する。
/// 実装 run 自体が供給側失敗の requeue 上限や `max_retries` の枯渇で `failed` になった場合
/// （`Trigger::Requeue`/`WorkerError` 経由）はこの条件を満たさない（reopen で仕切り直すべき）。
pub fn review_fail_is_the_only_reason_for_failure(events: &[(u64, Event)]) -> bool {
    events.iter().rev().find_map(|(_, e)| match e {
        Event::Transitioned { reason, .. } => Some(reason.as_str()),
        _ => None,
    }) == Some("review_fail")
}

/// 既存成果を部署のレビュアーで再判定する。新しい実装runは作らない。
///
/// ADR-0054 Phase 113 D3 追記: `done` からに加えて、`failed` からも許す。ただし
/// [`review_fail_is_the_only_reason_for_failure`] が `true` のとき（＝直前の実装 run は `done` で、
/// review 判定だけが不合格だった。それ以外の理由で `failed` になったタスク、例えば実装 run 自体が
/// 供給側失敗の requeue 上限に達した場合は対象外）だけに限る。`task_core::transition::transition`
/// （純粋関数、event 履歴を持たない）は `Status::Failed` からの遷移そのものは許しているので、
/// ここで event 履歴を見て絞り込む。人の承認（`Check::Human`）が既に `Done` の `Approval` 子タスクで
/// 承認済みなら、`attempts` を変えずに再判定することで（`human_approval_title` が `attempts` を含むため）
/// ディスパッチャの `resolve_human_approvals` が同じ子タスクを見つけて再利用する（人に二度承認させない。
/// D2 と同じ考え方）。
pub fn rereview(
    store: &dyn TaskStore,
    id: TaskId,
    expected: Option<Status>,
) -> Result<TransitionResult, OpsError> {
    let task = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    if let Some(exp) = expected
        && exp != task.status
    {
        return Err(OpsError::Conflict {
            expected: exp,
            actual: task.status,
        });
    }
    if !task
        .acceptance
        .iter()
        .any(|c| matches!(c.check, task_core::Check::Reviewer))
        || task_core::support_kind(&task).is_some()
    {
        return Err(OpsError::Validation(
            "reviewer条件を持つ通常タスクだけを再レビューできます".into(),
        ));
    }
    if task.status == Status::Failed {
        let events = store.events_for(id)?;
        if !review_fail_is_the_only_reason_for_failure(&events) {
            return Err(OpsError::Validation(
                "failedなタスクを再レビューできるのは、直前の実装runがdoneでreview判定だけが\
                 不合格だった場合だけです（それ以外の理由でfailedになった場合はreopenしてください）"
                    .into(),
            ));
        }
    }
    let outcome = store.apply_transition(id, Trigger::Rereview, None)?;
    Ok(TransitionResult {
        id,
        from: task.status,
        to: outcome.next,
        reason: outcome.reason.into(),
        cascaded: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{
        ArtifactRef, Budget, Check, Criterion, SqliteStore, Task, TaskId, TaskKind, Tier,
        WorkerHint, WorkspaceSpec,
    };

    fn sample_task(status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion {
                text: "c".into(),
                check: Check::Human,
            }],
            inputs: Vec::<ArtifactRef>::new(),
            depends_on: vec![],
            status,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::local("/tmp/ws"),
            repos: Vec::new(),
            budget: Budget {
                max_turns: 1,
                max_wall_secs: 1,
                max_retries: 2,
            },
            attempts: 1,
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
    fn rereview_reuses_done_output_without_starting_an_implementation() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task(Status::Done);
        task.acceptance[0].check = Check::Reviewer;
        store.insert(&task).unwrap();
        let result = rereview(&store, task.id, Some(Status::Done)).unwrap();
        assert_eq!(result.to, Status::Reviewing);
        assert_eq!(store.get(task.id).unwrap().unwrap().attempts, task.attempts);
        assert!(rereview(&store, task.id, Some(Status::Done)).is_err());
        assert!(
            !store
                .events_for(task.id)
                .unwrap()
                .iter()
                .any(|(_, e)| matches!(e, Event::WorkerStarted { .. }))
        );
    }

    /// ADR-0054 Phase 113 D3/D4(d): `failed` は「直前の実装 run が `done` で、review 判定だけが
    /// 不合格だった」（最後の遷移が `review_fail`）ときだけ再レビューできる。それ以外の理由で
    /// `failed`（例: 実装 run 自体が requeue 上限を使い切った `WorkerError`）は拒否する。
    #[test]
    fn rereview_from_failed_requires_the_last_transition_to_be_review_fail() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task(Status::Running);
        task.acceptance[0].check = Check::Reviewer;
        task.budget.max_retries = 0;
        store.insert(&task).unwrap();
        // 実装 run 自体が失敗（供給側失敗の requeue 上限。`review_fail` ではない）で `failed` になった
        // ケースは対象外。
        store
            .apply_transition(task.id, Trigger::WorkerError { retryable: true }, None)
            .unwrap();
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Failed);
        let err = rereview(&store, task.id, None).unwrap_err();
        assert!(matches!(err, OpsError::Validation(_)), "{err:?}");

        // review 判定の不合格（`review_fail`）で `failed` になったケースは許す。
        let store2 = SqliteStore::open_in_memory().unwrap();
        let mut task2 = sample_task(Status::Reviewing);
        task2.acceptance[0].check = Check::Reviewer;
        task2.budget.max_retries = 0;
        store2.insert(&task2).unwrap();
        store2
            .apply_transition(task2.id, Trigger::ReviewFail, None)
            .unwrap();
        assert_eq!(
            store2.get(task2.id).unwrap().unwrap().status,
            Status::Failed
        );
        let result = rereview(&store2, task2.id, None).unwrap();
        assert_eq!(result.to, Status::Reviewing);
    }

    /// `running` のタスクは**必ずリースを持つ**（`acquire_lease` がそう作る）。割り込みの
    /// `WorkerFinished` はそのリースの run にだけ付くので、テストでも同じ形にする。
    fn store_with(status: Status) -> (SqliteStore, TaskId) {
        let store = SqliteStore::open_in_memory().expect("store");
        let mut task = sample_task(status);
        if status == Status::Running {
            task.lease = Some(task_core::Lease {
                worker_run_id: "run-1".into(),
                expires_at: OffsetDateTime::now_utc() + time::Duration::seconds(60),
            });
        }
        let id = task.id;
        store.insert(&task).expect("insert");
        (store, id)
    }

    /// ADR-0044 D2 の表を状態ごとに全部確かめる（`running` / `reviewing` は割り込み、`blocked` は回答、
    /// `ready` / `draft` は記録だけ、終端は記録だけ + `cancelled` は再開できない）。
    #[test]
    fn the_human_comment_effect_table_holds_for_every_status() {
        let now = OffsetDateTime::now_utc();
        for (status, expected, expect_status, can_reopen) in [
            (Status::Draft, CommentEffect::Stored, Status::Draft, false),
            (Status::Ready, CommentEffect::Stored, Status::Ready, false),
            (
                Status::Running,
                CommentEffect::Interrupted,
                Status::Ready,
                false,
            ),
            (
                Status::Reviewing,
                CommentEffect::Interrupted,
                Status::Ready,
                false,
            ),
            (
                Status::Blocked,
                CommentEffect::Answered,
                Status::Ready,
                false,
            ),
            (Status::Done, CommentEffect::Terminal, Status::Done, true),
            (
                Status::Failed,
                CommentEffect::Terminal,
                Status::Failed,
                true,
            ),
            (
                Status::Cancelled,
                CommentEffect::Terminal,
                Status::Cancelled,
                false,
            ),
        ] {
            let (store, id) = store_with(status);
            let result = post_human_comment(&store, id, "見てほしい".into(), now)
                .unwrap_or_else(|e| panic!("{status:?}: {e}"));
            assert_eq!(result.effect, expected, "{status:?}");
            assert_eq!(result.can_reopen, can_reopen, "{status:?}");
            let after = store.get(id).expect("get").expect("task");
            assert_eq!(after.status, expect_status, "{status:?}");
            // どの状態でもコメントは残る。
            let comments = store.comments_for(id).expect("comments");
            assert_eq!(comments.len(), 1, "{status:?}");
            assert_eq!(comments[0].author_kind, CommentAuthorKind::Human);

            if expected == CommentEffect::Interrupted {
                // attempts は据え置き、走っている run にだけ
                // `WorkerFinished{outcome:"interrupted: comment"}` が残る（`reviewing` はリースが
                // 無いので足さない。既に終わった run の記録を壊さないため）。
                assert_eq!(after.attempts, 1, "割り込みは試行を消費しない");
                let events = store.events_for(id).expect("events");
                let finished = events.iter().any(|(_, e)| {
                    matches!(e, Event::WorkerFinished { outcome, .. } if outcome == INTERRUPTED_OUTCOME)
                });
                assert_eq!(
                    finished,
                    status == Status::Running,
                    "{status:?}: WorkerFinished の有無"
                );
                assert!(
                    events
                        .iter()
                        .any(|(_, e)| matches!(e, Event::Transitioned { reason, .. } if reason == "comment")),
                    "{status:?}: Transitioned{{reason:\"comment\"}} が無い"
                );
                // 次の run の前置きの先頭に載る（割り込んだコメントとして引ける）。
                let found = interrupting_comment(&events, &comments).expect("interrupting comment");
                assert_eq!(found.body, "見てほしい");
            }
            if expected == CommentEffect::Answered {
                let events = store.events_for(id).expect("events");
                assert!(events.iter().any(|(_, e)| matches!(
                    e,
                    Event::Answered { answer, .. } if answer == "見てほしい"
                )));
            }
        }
    }

    /// ADR-0044 D2: `done` / `failed` は再開できて attempts が 0 に戻る。`cancelled` は 409 相当。
    #[test]
    fn reopen_resets_attempts_for_done_and_failed_but_refuses_cancelled() {
        for status in [Status::Done, Status::Failed] {
            let (store, id) = store_with(status);
            let result = reopen(&store, id, None).unwrap_or_else(|e| panic!("{status:?}: {e}"));
            assert_eq!(result.from, status);
            assert_eq!(result.to, Status::Ready);
            assert_eq!(result.reason, "reopen");
            let after = store.get(id).expect("get").expect("task");
            assert_eq!(after.attempts, 0);
        }
        let (store, id) = store_with(Status::Cancelled);
        assert!(matches!(
            reopen(&store, id, None),
            Err(OpsError::InvalidState { .. })
        ));
        let (store, id) = store_with(Status::Ready);
        assert!(matches!(
            reopen(&store, id, None),
            Err(OpsError::InvalidState { .. })
        ));
    }

    /// ワーカーのコメントは記録だけ（状態を変えない）。空の本文は拒否する。
    #[test]
    fn a_node_comment_only_records_and_blank_bodies_are_rejected() {
        let now = OffsetDateTime::now_utc();
        let (store, id) = store_with(Status::Running);
        let comment = post_node_comment(
            &store,
            id,
            Some("impl".into()),
            Some("run-1".into()),
            "ビルドが通った".into(),
            now,
        )
        .expect("node comment");
        assert_eq!(comment.author.as_deref(), Some("impl"));
        assert_eq!(
            store.get(id).expect("get").expect("task").status,
            Status::Running
        );
        assert!(matches!(
            post_node_comment(&store, id, None, None, "  ".into(), now),
            Err(OpsError::Validation(_))
        ));
        assert!(matches!(
            post_human_comment(&store, id, "".into(), now),
            Err(OpsError::Validation(_))
        ));
    }

    /// ADR-0044 D2（Phase 53 の監査）: 供給側の requeue（ADR-0010 P-21）とリース切れは
    /// 「割り込みを受け取った run が走った」ことにならないので、割り込みは**消えない**。
    #[test]
    fn a_requeue_or_a_lease_expiry_does_not_consume_the_interruption() {
        let now = OffsetDateTime::now_utc();
        for outcome in ["requeue: adapter: throttled", "lease_expired"] {
            let (store, id) = store_with(Status::Running);
            post_human_comment(&store, id, "止めて".into(), now).expect("comment");
            let comments = store.comments_for(id).expect("comments");
            store
                .apply_transition(id, Trigger::Dispatch, None)
                .expect("dispatch");
            store
                .apply_transition_with_events(
                    id,
                    Trigger::Requeue,
                    vec![Event::WorkerFinished {
                        run_id: "run-2".into(),
                        outcome: outcome.to_string(),
                        usage: None,
                        role: None,
                        metrics: None,
                    }],
                )
                .expect("requeue");
            let events = store.events_for(id).expect("events");
            assert!(
                interrupting_comment(&events, &comments).is_some(),
                "{outcome}: モデルは前置きを見ていないので割り込みは残る"
            );
        }
    }

    /// 割り込みは**次の run にだけ**載る。dispatch（`Transitioned{to: running}`）では消えず、
    /// その run が終わった（`WorkerFinished`）ら消える。
    #[test]
    fn the_interruption_is_only_carried_into_the_next_run() {
        let now = OffsetDateTime::now_utc();
        let (store, id) = store_with(Status::Running);
        post_human_comment(&store, id, "止めて".into(), now).expect("comment");
        let comments = store.comments_for(id).expect("comments");
        let events = store.events_for(id).expect("events");
        assert!(interrupting_comment(&events, &comments).is_some());

        // 次の run が始まっただけでは消えない（この run が前置きを受け取る）。
        store
            .apply_transition(id, Trigger::Dispatch, None)
            .expect("dispatch");
        let events = store.events_for(id).expect("events");
        assert!(
            interrupting_comment(&events, &comments).is_some(),
            "次の run はまだ受け取る"
        );

        // その run が終わったら消える。
        store
            .apply_transition_with_events(
                id,
                Trigger::WorkerDone,
                vec![Event::WorkerFinished {
                    run_id: "run-2".into(),
                    outcome: "done: 直した".into(),
                    usage: None,
                    role: None,
                    metrics: None,
                }],
            )
            .expect("worker_done");
        let events = store.events_for(id).expect("events");
        assert!(
            interrupting_comment(&events, &comments).is_none(),
            "消化済み"
        );
    }
}
