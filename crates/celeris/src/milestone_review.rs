//! 途中目標の判定を対話にする（ADR-0038 D1。Phase 41）。tick の中から同期で呼ばれる**判定だけ**の層。
//!
//! 1. `ready_milestones` — 「動いているものが無く、人の手が要る」途中目標を決定的に見つける
//!    （条件は ADR-0037 D5 の `milestone_ready` と同じ。`notify` もこれを使う）。
//! 2. `review_state`（`task_ops::milestone_review`）— その途中目標の**レビューの対話**
//!    （裏方 `support = "milestone_review"`）と、その返事。通知はこの返事が付いてから送る（D4）。
//! 3. `schedule` — レビューの対話 run をまだ起こしていない途中目標について、秘書の対話を 1 件だけ起こす。
//!    「まだ起こしていない」は**直近の done より後に作られたレビューがあるか**で見る（同じ done の集合で
//!    2 回目は起きない。Go を出して done が増えたら再び起きる）。
//!
//! LLM も HTTP も無い（DESIGN 原則 1: 判断は決定的、文面を書くのは対話 run の仕事）。
//!
//! Phase 42（実機 2026-09-18）: 途中目標に `ok` を押すと計画 run（`kind = plan`）が `done` になり、
//! 子は全部 `draft` で始まる。計画 run 自身を「仕事」に数えると、分解した直後に
//! 「動いているものが無く done が 1 件以上」が成立してしまい、人がまだ何も判定していないのに
//! レビューの対話がもう一度起きた。`support_kind`（`task_core::report`）が `kind = plan` を
//! `"plan"`（裏方）として返すようにし、`ready_milestones` の `support_kind(t).is_none()` の
//! 絞り込みから自然に外れるようにした。

use task_core::report::support_kind;
use task_core::{
    GenreSpec, ListFilter, ListOrder, Milestone, MilestoneStatus, Project, RoleSpec, Status,
    StoreError, Task, TaskStore,
};
use task_ops::milestone_review::ReviewState;
use time::OffsetDateTime;

/// 1 tick で見る案件の上限（`notify` と同じ）。
const PROJECT_SCAN: usize = 200;
/// 1 案件あたりに見るタスクの上限。
const TASK_SCAN: usize = 1_000;

/// 「動いているものが無く、人の手が要る」途中目標 1 件（ADR-0037 D5 / ADR-0038 D1）。
#[derive(Debug, Clone, PartialEq)]
pub struct ReadyMilestone {
    pub project: Project,
    pub milestone: Milestone,
    /// 終わった仕事（裏方を除く）の件数。通知の重複排除の鍵にも使う。
    pub done: usize,
    pub failed: usize,
    /// Go 待ちの `draft`（担当の表示名を出すために行ごと持つ）。
    pub waiting: Vec<Task>,
    /// 終わった仕事の中で一番新しい `updated_at`（レビューを起こし直す判断に使う）。
    pub last_done_at: Option<OffsetDateTime>,
}

/// 条件（ADR-0037 D1、実機 2026-09-18）: 途中目標が `reached` でなく、属する仕事（裏方を除く）が
/// 1 件以上あり、その中に ready / running / reviewing / blocked が **0 件**、done が **1 件以上**。
/// 並びは案件 id → 途中目標 id の昇順で決定的。
pub fn ready_milestones(store: &dyn TaskStore) -> Result<Vec<ReadyMilestone>, StoreError> {
    let mut out = Vec::new();
    let mut projects = store.project_list()?;
    projects.sort_by_key(|a| a.id);
    for project in projects.into_iter().take(PROJECT_SCAN) {
        let filter = ListFilter {
            project_id: Some(project.id),
            ..ListFilter::default()
        };
        let page = store.list_page(&filter, ListOrder::CreatedDesc, None, TASK_SCAN)?;
        let mut milestones = store.milestone_list(project.id)?;
        milestones.sort_by_key(|a| a.id);
        for milestone in milestones {
            if milestone.status == MilestoneStatus::Reached {
                continue;
            }
            let mut tasks: Vec<&Task> = page
                .items
                .iter()
                .filter(|t| t.milestone_id == Some(milestone.id) && support_kind(t).is_none())
                .collect();
            if tasks.is_empty() {
                continue;
            }
            tasks.sort_by_key(|t| t.id);
            let active = tasks
                .iter()
                .filter(|t| {
                    matches!(
                        t.status,
                        Status::Ready | Status::Running | Status::Reviewing | Status::Blocked
                    )
                })
                .count();
            if active > 0 {
                continue;
            }
            let done: Vec<&&Task> = tasks.iter().filter(|t| t.status == Status::Done).collect();
            if done.is_empty() {
                continue;
            }
            out.push(ReadyMilestone {
                project: project.clone(),
                milestone,
                done: done.len(),
                failed: tasks.iter().filter(|t| t.status == Status::Failed).count(),
                waiting: tasks
                    .iter()
                    .filter(|t| t.status == Status::Draft)
                    .map(|t| (*t).clone())
                    .collect(),
                last_done_at: done.iter().map(|t| t.updated_at).max(),
            });
        }
    }
    Ok(out)
}

/// レビューの対話 run をまだ起こしていない途中目標について、秘書の対話を 1 件だけ起こす（ADR-0038 D1）。
/// 「まだ起こしていない」＝ レビューの対話が 1 つも無いか、**一番新しいレビューより後に done が増えた**こと
/// （同じ done の集合では 2 回目は起きない）。秘書がいない構成や検証で弾かれたものは警告だけ残して飛ばす
/// （tick は止めない）。返すのは作った対話タスク。
pub fn schedule(
    store: &dyn TaskStore,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    conversation_genre: &str,
    now: OffsetDateTime,
) -> Result<Vec<Task>, StoreError> {
    let mut started = Vec::new();
    for ready in ready_milestones(store)? {
        let state =
            task_ops::milestone_review::review_state(store, ready.project.id, ready.milestone.id)?;
        if !needs_review(&state, ready.last_done_at) {
            continue;
        }
        let text = task_ops::milestone_review::review_request_text(
            &ready.milestone,
            ready.done,
            ready.failed,
            ready.waiting.len(),
        );
        match task_ops::milestone_review::start_review(
            store,
            &ready.project,
            &ready.milestone,
            &text,
            roles,
            genres,
            conversation_genre,
            now,
        ) {
            Ok(conversation) => {
                tracing::info!(
                    project_id = %ready.project.id,
                    milestone_id = %ready.milestone.id,
                    task_id = %conversation.task.id,
                    done = ready.done,
                    "milestone review: asked the secretary to summarise the results and propose the next milestone"
                );
                started.push(conversation.task);
            }
            Err(e) => tracing::warn!(
                milestone_id = %ready.milestone.id,
                error = %e,
                "milestone review: could not start the secretary's review"
            ),
        }
    }
    Ok(started)
}

/// レビューを起こすか（純粋。`last_done_at` は終わった仕事の中で一番新しい `updated_at`）。
fn needs_review(state: &ReviewState, last_done_at: Option<OffsetDateTime>) -> bool {
    match (&state.task, last_done_at) {
        (None, _) => true,
        // 一番新しいレビューより後に終わった仕事があれば、結果が増えているのでもう一度まとめてもらう。
        (Some(task), Some(done_at)) => task.created_at < done_at,
        (Some(_), None) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000 + secs)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }

    fn review_task(created_at: OffsetDateTime) -> Task {
        use task_core::{
            Budget, MilestoneId, Status, TaskId, TaskKind, Tier, WorkerHint, WorkspaceSpec,
        };
        Task {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "対話".into(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: 1,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "ws".into(),
                mode: None,
            },
            budget: Budget {
                max_turns: 1,
                max_wall_secs: 1,
                max_retries: 0,
            },
            attempts: 0,
            lease: None,
            created_at,
            updated_at: created_at,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: Some(MilestoneId::new()),
            assignee: Some("secretary".into()),
            conversation: Some(task_core::MessageId::new()),
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    /// ADR-0038 D1: 同じ done の集合では 1 回だけ。done が増えたら（Go の後）また起きる。
    #[test]
    fn a_review_is_needed_once_per_set_of_finished_work() {
        assert!(
            needs_review(&ReviewState::default(), Some(at(10))),
            "レビューがまだ無ければ起こす"
        );
        let state = ReviewState {
            task: Some(review_task(at(20))),
            reply: None,
        };
        assert!(
            !needs_review(&state, Some(at(10))),
            "同じ done の集合では 2 回目は起きない"
        );
        assert!(
            needs_review(&state, Some(at(30))),
            "レビューの後に done が増えたら再び起こす"
        );
        assert!(
            !needs_review(&state, None),
            "終わった仕事が無ければ起こさない"
        );
    }

    // ---- Phase 42（実機 2026-09-18）: 計画タスクは途中目標の「仕事」に数えない ----

    fn task_with(
        kind: task_core::TaskKind,
        status: Status,
        project_id: task_core::ProjectId,
        milestone_id: task_core::MilestoneId,
        created_at: OffsetDateTime,
    ) -> Task {
        use task_core::{Budget, TaskId, Tier, WorkerHint, WorkspaceSpec};
        Task {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind,
            title: "仕事".into(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status,
            priority: 1,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "ws".into(),
                mode: None,
            },
            budget: Budget {
                max_turns: 1,
                max_wall_secs: 1,
                max_retries: 0,
            },
            attempts: 0,
            lease: None,
            created_at,
            updated_at: created_at,
            role: None,
            genre: None,
            aggregate: false,
            project_id: Some(project_id),
            milestone_id: Some(milestone_id),
            assignee: None,
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    fn seed_project_and_milestone(
        store: &task_core::SqliteStore,
        title: &str,
    ) -> (task_core::Project, Milestone) {
        use task_core::{Project, ProjectId, ProjectStatus};
        let now = OffsetDateTime::now_utc();
        let project = Project {
            archived_at: None,
            paused_from: None,
            id: ProjectId::new(),
            title: title.into(),
            request: "調べて".into(),
            status: ProjectStatus::Active,
            secretary_summary: None,
            workspace: None,
            created_at: now,
            updated_at: now,
        };
        store
            .project_create(&project)
            .unwrap_or_else(|e| panic!("project: {e}"));
        let milestone = store
            .milestone_create(project.id, "途中目標", "", MilestoneStatus::InProgress)
            .unwrap_or_else(|e| panic!("milestone: {e}"));
        (project, milestone)
    }

    /// Phase 42: 分解直後（計画 run が `done`、子は全部 `draft`）は「動いているものが無く done が
    /// 1 件以上」に見えてはいけない。計画 run は仕事に数えない。
    #[test]
    fn a_milestone_is_not_stalled_right_after_decomposition() {
        let store =
            task_core::SqliteStore::open_in_memory().unwrap_or_else(|e| panic!("open: {e}"));
        let (project, milestone) = seed_project_and_milestone(&store, "分解直後");
        let now = OffsetDateTime::now_utc();
        store
            .insert(&task_with(
                task_core::TaskKind::Plan,
                Status::Done,
                project.id,
                milestone.id,
                now,
            ))
            .unwrap_or_else(|e| panic!("insert plan: {e}"));
        store
            .insert(&task_with(
                task_core::TaskKind::Execute,
                Status::Draft,
                project.id,
                milestone.id,
                now,
            ))
            .unwrap_or_else(|e| panic!("insert child 1: {e}"));
        store
            .insert(&task_with(
                task_core::TaskKind::Execute,
                Status::Draft,
                project.id,
                milestone.id,
                now,
            ))
            .unwrap_or_else(|e| panic!("insert child 2: {e}"));

        let ready = ready_milestones(&store).unwrap_or_else(|e| panic!("ready: {e}"));
        assert!(
            ready.is_empty(),
            "計画 run が done でも、子が全部 draft なら人の Go 待ちであってレビュー対象ではない"
        );
    }

    /// Phase 42: 子が 1 件 `done` になり、他が動いていなければレビュー対象になる
    /// （計画 run 自身は done の件数に数えない）。
    #[test]
    fn a_milestone_is_stalled_once_a_child_is_done_and_nothing_else_is_moving() {
        let store =
            task_core::SqliteStore::open_in_memory().unwrap_or_else(|e| panic!("open: {e}"));
        let (project, milestone) = seed_project_and_milestone(&store, "1 件完了");
        let now = OffsetDateTime::now_utc();
        store
            .insert(&task_with(
                task_core::TaskKind::Plan,
                Status::Done,
                project.id,
                milestone.id,
                now,
            ))
            .unwrap_or_else(|e| panic!("insert plan: {e}"));
        store
            .insert(&task_with(
                task_core::TaskKind::Execute,
                Status::Done,
                project.id,
                milestone.id,
                now,
            ))
            .unwrap_or_else(|e| panic!("insert done child: {e}"));
        store
            .insert(&task_with(
                task_core::TaskKind::Execute,
                Status::Draft,
                project.id,
                milestone.id,
                now,
            ))
            .unwrap_or_else(|e| panic!("insert waiting child: {e}"));

        let ready = ready_milestones(&store).unwrap_or_else(|e| panic!("ready: {e}"));
        assert_eq!(
            ready.len(),
            1,
            "計画 run を除けば done 1 件・待ち 1 件で仕事は止まっている"
        );
        assert_eq!(ready[0].milestone.id, milestone.id);
        assert_eq!(ready[0].done, 1, "計画 run は done の件数に数えない");
        assert_eq!(ready[0].waiting.len(), 1);
    }
}
