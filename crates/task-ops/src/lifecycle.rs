//! ADR-0044 D6（Phase 55）: 案件・途中目標の **中止・一時停止・アーカイブ**。
//!
//! 3 階層（タスク・途中目標・案件）のうち、タスクの中止は従来どおり `crate::gate::cancel`。
//! ここは「上の 2 階層」を扱い、連鎖は**決定的・同期**に行う（ADR-0044 D6）:
//!
//! | 操作 | 何が起きる |
//! |---|---|
//! | `cancel` | 属する**非終端タスクを全部** `cancelled` にする（`Trigger::ProjectCancelled` / `MilestoneCancelled`。理由が `Event::Transitioned.reason` に残る）。案件の中止は非終端の途中目標も `cancelled` にする。最後に自分が `cancelled` |
//! | `pause` | 自分が `paused`。元の状態は `paused_from` に残す。**属するタスクは dispatch されない**（`ready` のまま。`TaskStore::ready_tasks` が見る）。走っている run は終わるまで走る |
//! | `resume` | `paused_from` へ戻す（無ければ案件は `active`、途中目標は `in_progress`）。`paused_from` は消す |
//! | `archive` | **終端（`done` / `cancelled`）の案件だけ**。`archived_at` を入れる。一覧から既定で隠れる |
//! | `unarchive` | `archived_at` を消す |
//!
//! 走っている run は「タスクが `running` でなくなった」ことでディスパッチャが気付き、
//! ADR-0044 §5 Phase 53 追記の統一された止め方（プロセスグループへ SIGTERM →
//! `kill_grace_secs` → SIGKILL）で止まる。worktree とブランチはその後の掃除
//! （ADR-0043 D2 / Phase 52 の `cleanup_cancelled_worktrees`）が消す。
//!
//! 通知は作らない（ADR-0044 D8）。案件・途中目標の変更そのものは `updated_at` にだけ残し、
//! **タスク側にはイベントが残る**（`Event::Transitioned{reason: "project_cancelled" | "milestone_cancelled"}`）。
//! 案件用のイベント表は作らない（D8 の「タイムラインに残るだけ」に対して、案件のタイムラインは
//! 既存の `messages` と報告で足りる）。
//!
//! LLM も I/O も使わない（DESIGN 原則 1）。

use task_core::{
    Milestone, MilestoneId, MilestoneStatus, Project, ProjectId, ProjectStatus, Status, TaskStore,
    Trigger,
};
use time::OffsetDateTime;

use crate::error::OpsError;
use crate::view::{TaskRef, task_ref};

/// 案件・途中目標の状態を変えたときの結果（API がそのまま返す）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct ProjectLifecycle {
    pub project: Project,
    /// この操作の連鎖で `cancelled` になったタスク（`cancel` 以外では空）。
    #[serde(default)]
    pub cancelled_tasks: Vec<TaskRef>,
    /// この操作の連鎖で `cancelled` になった途中目標の id（`cancel` 以外では空）。
    #[serde(default)]
    pub cancelled_milestones: Vec<MilestoneId>,
}

/// 途中目標の状態を変えたときの結果。
#[derive(Debug, Clone, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct MilestoneLifecycle {
    pub milestone: Milestone,
    #[serde(default)]
    pub cancelled_tasks: Vec<TaskRef>,
}

/// 中止の連鎖で 1 回に見るタスクの上限。案件のタスクは実機で数十〜数百件なので、
/// `GET /projects/{id}` の `PROJECT_TASKS_LIMIT`（500）より広く取っておく。
const CASCADE_TASK_LIMIT: usize = 5_000;

/// `filter` に合うタスクを（アーカイブの有無に関わらず）集める。索引の効く列で絞るので
/// 全件を読まない。
fn tasks_matching(
    store: &dyn TaskStore,
    filter: task_core::ListFilter,
) -> Result<Vec<task_core::Task>, OpsError> {
    Ok(store
        .list_page(
            &filter,
            task_core::ListOrder::CreatedDesc,
            None,
            CASCADE_TASK_LIMIT,
        )?
        .items)
}

/// その案件のタスク（`tasks.project_id` の索引で引く）。
fn project_tasks(store: &dyn TaskStore, id: ProjectId) -> Result<Vec<task_core::Task>, OpsError> {
    tasks_matching(
        store,
        task_core::ListFilter {
            project_id: Some(id),
            ..task_core::ListFilter::default()
        },
    )
}

/// その途中目標のタスク。
fn milestone_tasks(
    store: &dyn TaskStore,
    id: MilestoneId,
) -> Result<Vec<task_core::Task>, OpsError> {
    tasks_matching(
        store,
        task_core::ListFilter {
            milestone_id: Some(id),
            ..task_core::ListFilter::default()
        },
    )
}

/// 非終端のタスクに `trigger` を当てて `cancelled` にする。既に終端のものは触らない。
/// 1 件ごとに `apply_transition`（＝ストアの同一トランザクション）で、子・後続への伝播も
/// 従来の `cancel` と同じように起きる。伝播で先に `cancelled` になったタスクは、この後の
/// 反復で「終端なので触らない」になるだけで二重には数えない。
fn cancel_tasks(
    store: &dyn TaskStore,
    tasks: Vec<task_core::Task>,
    trigger: Trigger,
) -> Result<Vec<TaskRef>, OpsError> {
    let mut out = Vec::new();
    for task in tasks {
        // 直前の伝播で終端になっているかもしれないので、その都度いまの姿を見る。
        let Some(current) = store.get(task.id)? else {
            continue;
        };
        if current.status.is_terminal() {
            continue;
        }
        store.apply_transition(current.id, trigger, None)?;
        if let Some(after) = store.get(current.id)?
            && after.status == Status::Cancelled
        {
            out.push(task_ref(&after));
        }
    }
    Ok(out)
}

fn project_or_404(store: &dyn TaskStore, id: ProjectId) -> Result<Project, OpsError> {
    store.project_get(id)?.ok_or(OpsError::ProjectNotFound(id))
}

fn milestone_or_404(store: &dyn TaskStore, id: MilestoneId) -> Result<Milestone, OpsError> {
    store
        .milestone_get(id)?
        .ok_or(OpsError::MilestoneNotFound(id))
}

fn project_conflict(project: &Project, action: &str) -> OpsError {
    OpsError::InvalidLifecycle {
        subject: format!("project {}", project.id),
        context: format!("status={}", project.status.as_str()),
        action: action.to_string(),
    }
}

fn milestone_conflict(milestone: &Milestone, action: &str) -> OpsError {
    OpsError::InvalidLifecycle {
        subject: format!("milestone {}", milestone.id),
        context: format!("status={}", milestone.status.as_str()),
        action: action.to_string(),
    }
}

/// 中止できる途中目標か（＝終端でない。達成済み・再設計済み・中止済みは触らない）。
/// 案件ごとの連鎖にも、`POST /milestones/{id}/cancel` にも同じ判定を使う。
fn milestone_is_cancellable(status: MilestoneStatus) -> bool {
    !status.is_terminal()
}

// ---- 案件 ----

/// `POST /projects/{id}/cancel`。既に `cancelled` なら 409。
pub fn cancel_project(store: &dyn TaskStore, id: ProjectId) -> Result<ProjectLifecycle, OpsError> {
    let project = project_or_404(store, id)?;
    if project.status == ProjectStatus::Cancelled {
        return Err(project_conflict(&project, "cancelled"));
    }
    let cancelled_tasks =
        cancel_tasks(store, project_tasks(store, id)?, Trigger::ProjectCancelled)?;
    let mut cancelled_milestones = Vec::new();
    for milestone in store.milestone_list(id)? {
        if milestone_is_cancellable(milestone.status) {
            store.milestone_set_lifecycle(milestone.id, MilestoneStatus::Cancelled, Some(None))?;
            cancelled_milestones.push(milestone.id);
        }
    }
    store.project_set_lifecycle(id, ProjectStatus::Cancelled, Some(None))?;
    Ok(ProjectLifecycle {
        project: project_or_404(store, id)?,
        cancelled_tasks,
        cancelled_milestones,
    })
}

/// `POST /projects/{id}/pause`。終端（`done` / `cancelled`）と既に `paused` は 409。
pub fn pause_project(store: &dyn TaskStore, id: ProjectId) -> Result<ProjectLifecycle, OpsError> {
    let project = project_or_404(store, id)?;
    if project.status == ProjectStatus::Paused || project.status.is_terminal() {
        return Err(project_conflict(&project, "paused"));
    }
    store.project_set_lifecycle(id, ProjectStatus::Paused, Some(Some(project.status)))?;
    Ok(ProjectLifecycle {
        project: project_or_404(store, id)?,
        cancelled_tasks: Vec::new(),
        cancelled_milestones: Vec::new(),
    })
}

/// `POST /projects/{id}/resume`。`paused` でなければ 409。戻り先は `paused_from`（無ければ `active`）。
pub fn resume_project(store: &dyn TaskStore, id: ProjectId) -> Result<ProjectLifecycle, OpsError> {
    let project = project_or_404(store, id)?;
    if project.status != ProjectStatus::Paused {
        return Err(project_conflict(&project, "resumed"));
    }
    let back = project.paused_from.unwrap_or(ProjectStatus::Active);
    store.project_set_lifecycle(id, back, Some(None))?;
    Ok(ProjectLifecycle {
        project: project_or_404(store, id)?,
        cancelled_tasks: Vec::new(),
        cancelled_milestones: Vec::new(),
    })
}

/// `POST /projects/{id}/archive`。**終端（`done` / `cancelled`）の案件だけ**（他は 409）。
/// 既にアーカイブ済みなら何もせずそのまま返す（GUI の二度押しを 409 にしない）。
pub fn archive_project(
    store: &dyn TaskStore,
    id: ProjectId,
    now: OffsetDateTime,
) -> Result<ProjectLifecycle, OpsError> {
    let project = project_or_404(store, id)?;
    if !project.status.is_terminal() {
        return Err(project_conflict(&project, "archived"));
    }
    if project.archived_at.is_none() {
        store.project_set_archived_at(id, Some(now))?;
    }
    Ok(ProjectLifecycle {
        project: project_or_404(store, id)?,
        cancelled_tasks: Vec::new(),
        cancelled_milestones: Vec::new(),
    })
}

/// `POST /projects/{id}/unarchive`。アーカイブされていなければそのまま返す。
pub fn unarchive_project(
    store: &dyn TaskStore,
    id: ProjectId,
) -> Result<ProjectLifecycle, OpsError> {
    let project = project_or_404(store, id)?;
    if project.archived_at.is_some() {
        store.project_set_archived_at(id, None)?;
    }
    Ok(ProjectLifecycle {
        project: project_or_404(store, id)?,
        cancelled_tasks: Vec::new(),
        cancelled_milestones: Vec::new(),
    })
}

// ---- 途中目標 ----

/// `POST /milestones/{id}/cancel`。既に `cancelled` なら 409。
pub fn cancel_milestone(
    store: &dyn TaskStore,
    id: MilestoneId,
) -> Result<MilestoneLifecycle, OpsError> {
    let milestone = milestone_or_404(store, id)?;
    if !milestone_is_cancellable(milestone.status) {
        return Err(milestone_conflict(&milestone, "cancelled"));
    }
    let cancelled_tasks = cancel_tasks(
        store,
        milestone_tasks(store, id)?,
        Trigger::MilestoneCancelled,
    )?;
    store.milestone_set_lifecycle(id, MilestoneStatus::Cancelled, Some(None))?;
    Ok(MilestoneLifecycle {
        milestone: milestone_or_404(store, id)?,
        cancelled_tasks,
    })
}

/// `POST /milestones/{id}/pause`。`cancelled` と既に `paused` は 409。
pub fn pause_milestone(
    store: &dyn TaskStore,
    id: MilestoneId,
) -> Result<MilestoneLifecycle, OpsError> {
    let milestone = milestone_or_404(store, id)?;
    if milestone.status == MilestoneStatus::Paused || milestone.status.is_terminal() {
        return Err(milestone_conflict(&milestone, "paused"));
    }
    store.milestone_set_lifecycle(id, MilestoneStatus::Paused, Some(Some(milestone.status)))?;
    Ok(MilestoneLifecycle {
        milestone: milestone_or_404(store, id)?,
        cancelled_tasks: Vec::new(),
    })
}

/// `POST /milestones/{id}/resume`。`paused` でなければ 409。戻り先は `paused_from`
/// （無ければ `in_progress`）。
pub fn resume_milestone(
    store: &dyn TaskStore,
    id: MilestoneId,
) -> Result<MilestoneLifecycle, OpsError> {
    let milestone = milestone_or_404(store, id)?;
    if milestone.status != MilestoneStatus::Paused {
        return Err(milestone_conflict(&milestone, "resumed"));
    }
    let back = milestone.paused_from.unwrap_or(MilestoneStatus::InProgress);
    store.milestone_set_lifecycle(id, back, Some(None))?;
    Ok(MilestoneLifecycle {
        milestone: milestone_or_404(store, id)?,
        cancelled_tasks: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{
        ArtifactRef, Budget, Check, Criterion, ListFilter, ListOrder, SqliteStore, Task, TaskKind,
        Tier, WorkerHint, WorkspaceSpec,
    };

    fn store() -> SqliteStore {
        SqliteStore::open_in_memory().unwrap_or_else(|e| panic!("open: {e}"))
    }

    fn a_project(store: &dyn TaskStore, status: ProjectStatus) -> Project {
        let now = OffsetDateTime::now_utc();
        let project = Project {
            id: ProjectId::new(),
            title: "案件".to_string(),
            request: "やって".to_string(),
            status,
            secretary_summary: None,
            workspace: None,
            archived_at: None,
            paused_from: None,
            created_at: now,
            updated_at: now,
        };
        store
            .project_create(&project)
            .unwrap_or_else(|e| panic!("create: {e}"));
        project
    }

    fn a_task(
        store: &dyn TaskStore,
        project: Option<ProjectId>,
        milestone: Option<MilestoneId>,
        status: Status,
    ) -> Task {
        let now = OffsetDateTime::now_utc();
        let task = Task {
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: task_core::TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "仕事".to_string(),
            objective: "やる".to_string(),
            acceptance: vec![Criterion {
                text: "通る".to_string(),
                check: Check::Command {
                    cmd: "true".to_string(),
                    expect_exit: 0,
                },
            }],
            inputs: Vec::<ArtifactRef>::new(),
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
            project_id: project,
            milestone_id: milestone,
            assignee: None,
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
        };
        store
            .insert(&task)
            .unwrap_or_else(|e| panic!("insert: {e}"));
        task
    }

    #[test]
    fn cancelling_a_project_cancels_its_open_tasks_and_milestones_and_keeps_terminal_ones() {
        let store = store();
        let project = a_project(&store, ProjectStatus::Active);
        let running = a_task(&store, Some(project.id), None, Status::Running);
        let ready = a_task(&store, Some(project.id), None, Status::Ready);
        let done = a_task(&store, Some(project.id), None, Status::Done);
        let other = a_task(&store, None, None, Status::Ready);
        let reached = store
            .milestone_create(project.id, "済", "", MilestoneStatus::Reached)
            .unwrap_or_else(|e| panic!("{e}"));
        let open = store
            .milestone_create(project.id, "途中", "", MilestoneStatus::InProgress)
            .unwrap_or_else(|e| panic!("{e}"));

        let result = cancel_project(&store, project.id).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(result.project.status, ProjectStatus::Cancelled);
        assert_eq!(result.cancelled_tasks.len(), 2);
        assert_eq!(result.cancelled_milestones, vec![open.id]);

        let status_of = |id| {
            store
                .get(id)
                .unwrap_or_else(|e| panic!("{e}"))
                .map(|t| t.status)
        };
        assert_eq!(status_of(running.id), Some(Status::Cancelled));
        assert_eq!(status_of(ready.id), Some(Status::Cancelled));
        assert_eq!(status_of(done.id), Some(Status::Done));
        // 他の案件のタスクは触らない。
        assert_eq!(status_of(other.id), Some(Status::Ready));
        let milestones = store
            .milestone_list(project.id)
            .unwrap_or_else(|e| panic!("{e}"));
        let reached_now = milestones
            .iter()
            .find(|m| m.id == reached.id)
            .map(|m| m.status);
        assert_eq!(reached_now, Some(MilestoneStatus::Reached));
    }

    #[test]
    fn the_cascade_reason_says_which_level_cancelled_the_task() {
        let store = store();
        let project = a_project(&store, ProjectStatus::Active);
        let milestone = store
            .milestone_create(project.id, "途中", "", MilestoneStatus::InProgress)
            .unwrap_or_else(|e| panic!("{e}"));
        let by_project = a_task(&store, Some(project.id), None, Status::Ready);
        let by_milestone = a_task(&store, Some(project.id), Some(milestone.id), Status::Ready);

        cancel_milestone(&store, milestone.id).unwrap_or_else(|e| panic!("{e}"));
        cancel_project(&store, project.id).unwrap_or_else(|e| panic!("{e}"));

        let reasons = |id| {
            store
                .events_for(id)
                .unwrap_or_else(|e| panic!("{e}"))
                .into_iter()
                .filter_map(|(_, e)| match e {
                    task_core::Event::Transitioned { reason, .. } => Some(reason),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(reasons(by_milestone.id).contains(&"milestone_cancelled".to_string()));
        assert!(reasons(by_project.id).contains(&"project_cancelled".to_string()));
    }

    #[test]
    fn pause_remembers_the_previous_status_and_resume_restores_it() {
        let store = store();
        let project = a_project(&store, ProjectStatus::Proposed);
        let paused = pause_project(&store, project.id).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(paused.project.status, ProjectStatus::Paused);
        assert_eq!(paused.project.paused_from, Some(ProjectStatus::Proposed));

        // 二重の一時停止は 409。
        assert!(matches!(
            pause_project(&store, project.id),
            Err(OpsError::InvalidLifecycle { .. })
        ));

        let resumed = resume_project(&store, project.id).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(resumed.project.status, ProjectStatus::Proposed);
        assert_eq!(resumed.project.paused_from, None);
        assert!(matches!(
            resume_project(&store, project.id),
            Err(OpsError::InvalidLifecycle { .. })
        ));
    }

    #[test]
    fn milestone_pause_and_resume_round_trip() {
        let store = store();
        let project = a_project(&store, ProjectStatus::Active);
        let milestone = store
            .milestone_create(project.id, "途中", "", MilestoneStatus::InProgress)
            .unwrap_or_else(|e| panic!("{e}"));
        let paused = pause_milestone(&store, milestone.id).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(paused.milestone.status, MilestoneStatus::Paused);
        assert_eq!(
            paused.milestone.paused_from,
            Some(MilestoneStatus::InProgress)
        );
        let resumed = resume_milestone(&store, milestone.id).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(resumed.milestone.status, MilestoneStatus::InProgress);
        assert_eq!(resumed.milestone.paused_from, None);
    }

    #[test]
    fn only_a_terminal_project_can_be_archived_and_archived_tasks_are_hidden_by_default() {
        let store = store();
        let project = a_project(&store, ProjectStatus::Active);
        let task = a_task(&store, Some(project.id), None, Status::Done);
        let now = OffsetDateTime::now_utc();

        assert!(matches!(
            archive_project(&store, project.id, now),
            Err(OpsError::InvalidLifecycle { .. })
        ));

        store
            .project_set_lifecycle(project.id, ProjectStatus::Done, Some(None))
            .unwrap_or_else(|e| panic!("{e}"));
        let archived = archive_project(&store, project.id, now).unwrap_or_else(|e| panic!("{e}"));
        assert!(archived.project.archived_at.is_some());
        // 二度目は 409 にしない（そのまま返す）。
        assert!(archive_project(&store, project.id, now).is_ok());

        let hidden = ListFilter {
            hide_archived: true,
            ..ListFilter::default()
        };
        let page = store
            .list_page(&hidden, ListOrder::CreatedDesc, None, 100)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!page.items.iter().any(|t| t.id == task.id));

        let shown = ListFilter::default();
        let page = store
            .list_page(&shown, ListOrder::CreatedDesc, None, 100)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(page.items.iter().any(|t| t.id == task.id));

        unarchive_project(&store, project.id).unwrap_or_else(|e| panic!("{e}"));
        let page = store
            .list_page(&hidden, ListOrder::CreatedDesc, None, 100)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(page.items.iter().any(|t| t.id == task.id));
    }

    #[test]
    fn a_paused_project_stops_dispatch_and_resume_brings_it_back() {
        let store = store();
        let project = a_project(&store, ProjectStatus::Active);
        let task = a_task(&store, Some(project.id), None, Status::Ready);

        let ready = |s: &SqliteStore| {
            s.ready_tasks(100)
                .unwrap_or_else(|e| panic!("{e}"))
                .into_iter()
                .map(|t| t.id)
                .collect::<Vec<_>>()
        };
        assert!(ready(&store).contains(&task.id));

        pause_project(&store, project.id).unwrap_or_else(|e| panic!("{e}"));
        assert!(!ready(&store).contains(&task.id));
        // 状態機械は触らない: タスクは `ready` のまま。
        assert_eq!(
            store
                .get(task.id)
                .unwrap_or_else(|e| panic!("{e}"))
                .map(|t| t.status),
            Some(Status::Ready)
        );

        resume_project(&store, project.id).unwrap_or_else(|e| panic!("{e}"));
        assert!(ready(&store).contains(&task.id));
    }

    #[test]
    fn a_paused_milestone_stops_dispatch_of_its_tasks_only() {
        let store = store();
        let project = a_project(&store, ProjectStatus::Active);
        let milestone = store
            .milestone_create(project.id, "途中", "", MilestoneStatus::InProgress)
            .unwrap_or_else(|e| panic!("{e}"));
        let inside = a_task(&store, Some(project.id), Some(milestone.id), Status::Ready);
        let outside = a_task(&store, Some(project.id), None, Status::Ready);

        pause_milestone(&store, milestone.id).unwrap_or_else(|e| panic!("{e}"));
        let ready: Vec<_> = store
            .ready_tasks(100)
            .unwrap_or_else(|e| panic!("{e}"))
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert!(!ready.contains(&inside.id));
        assert!(ready.contains(&outside.id));
    }

    /// 終端の途中目標（達成・再設計・中止済み）は `cancel` も `pause` もできない（409）。
    /// GUI はこの規則でボタンを出し分ける（`gui/app/lib/lifecycle.ts`）。
    #[test]
    fn terminal_milestones_reject_cancel_and_pause() {
        let store = store();
        let project = a_project(&store, ProjectStatus::Active);
        for status in [
            MilestoneStatus::Reached,
            MilestoneStatus::Redesigned,
            MilestoneStatus::Cancelled,
        ] {
            let milestone = store
                .milestone_create(project.id, "終わった", "", status)
                .unwrap_or_else(|e| panic!("{e}"));
            assert!(
                matches!(
                    cancel_milestone(&store, milestone.id),
                    Err(OpsError::InvalidLifecycle { .. })
                ),
                "cancel {status:?}"
            );
            assert!(
                matches!(
                    pause_milestone(&store, milestone.id),
                    Err(OpsError::InvalidLifecycle { .. })
                ),
                "pause {status:?}"
            );
        }
    }

    #[test]
    fn unknown_ids_are_not_found() {
        let store = store();
        assert!(matches!(
            cancel_project(&store, ProjectId::new()),
            Err(OpsError::ProjectNotFound(_))
        ));
        assert!(matches!(
            pause_milestone(&store, MilestoneId::new()),
            Err(OpsError::MilestoneNotFound(_))
        ));
    }
}
