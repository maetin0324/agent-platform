//! 分解を起こす（GUI 監査対応 Phase 29。ADR-0033 D4 追記「分解は人が `POST /projects/{id}/plan` で
//! 起こす（GUI の『この方針で進める』）」）。
//!
//! SPEC §2.3「抽象的な研究の案件…関連研究調査 → 実装の計画立案 → … のような種々の仕事に分解され、
//! 実行される」の入口。**新しいタスクの種類は作らない**: 案件の依頼・途中目標・人の一言・秘書との
//! 直近のやり取りを 1 つの `goal` にまとめ、既存の `kind = plan` の仕組み
//! （`task_ops::add::create_support_task`）にそのまま渡すだけ。子タスク
//! （`PlanOutput.tasks[]` から作られるもの）は `task_core::plan::materialize` が親の
//! `project_id` / `milestone_id` を継ぐ（Phase 23 の監査 D-3 で確定済み）。`assignee` は秘書が
//! （組織図と分野の manifest を渡されたプロンプトの中で）振る（ADR-0033 D4）。
//!
//! I/O はストアの読み書きだけで、LLM は呼ばない（DESIGN 原則 1）。

use task_core::{
    GenreSpec, Message, Milestone, MilestoneId, MilestoneStatus, OrgKind, Project, ProjectStatus,
    RoleSpec, Task, TaskKind, TaskStore, Tier, WorkspaceSpec,
};
use time::OffsetDateTime;

use crate::add::{self, NewTaskSpec};
use crate::error::OpsError;

/// 直近のやり取りを `goal` に含める件数の上限（対話の履歴と同じ既定。ADR-0033 D4）。
pub const PLAN_HISTORY_LIMIT: usize = 20;

/// `goal` の 1 行目から `title` を切り出す上限（`task_ops::plan::create_plan` と同じ）。
const TITLE_MAX_CHARS: usize = 80;

/// 分解を起こす Plan タスクの既定の道具立て（旧 `celerisctl plan` の既定と同じ。ADR-0007 D6）。
/// `assignee`（秘書）の分野・役割の既定より**先に**効かせる（考える仕事なので予算を絞りたくない）。
const PLAN_TIER: Tier = Tier::Frontier;
const PLAN_MAX_TURNS: u32 = 30;
const PLAN_MAX_WALL_SECS: u64 = 900;
const PLAN_MAX_RETRIES: u32 = 1;

/// `start` の結果。API は 202 `{task_id}` を返す。
#[derive(Debug, Clone, PartialEq)]
pub struct StartedPlan {
    pub task: Task,
}

/// `POST /projects/{id}/plan`（ADR-0033 D4 追記）。`project` は呼び出し側（`task-api`）が存在を
/// 確認済みのものを渡す（404 の判定は API の責務。ここから先はすべて 422 系の検証）。
pub fn start(
    store: &dyn TaskStore,
    project: &Project,
    milestone_id: Option<MilestoneId>,
    note: Option<&str>,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    now: OffsetDateTime,
) -> Result<StartedPlan, OpsError> {
    let Some(secretary) = store
        .org_list()?
        .into_iter()
        .find(|n| n.kind == OrgKind::Secretary)
    else {
        return Err(OpsError::Validation(
            "no secretary is configured".to_string(),
        ));
    };

    let milestones = store.milestone_list(project.id)?;
    let target: Option<Milestone> = match milestone_id {
        Some(id) => Some(
            milestones
                .iter()
                .find(|m| m.id == id)
                .cloned()
                .ok_or_else(|| {
                    OpsError::Validation(format!(
                        "milestone {id} does not belong to project {}",
                        project.id
                    ))
                })?,
        ),
        None => None,
    };
    // SPEC §7 のアジャイル: 途中目標を明示しなければ、`approved` / `in_progress` のものを文脈として渡す
    // （複数あってもよい。どれから手を付けるかは秘書が計画の run で判断する）。
    let context: Vec<Milestone> = match &target {
        Some(m) => vec![m.clone()],
        None => milestones
            .into_iter()
            .filter(|m| {
                matches!(
                    m.status,
                    MilestoneStatus::Approved | MilestoneStatus::InProgress
                )
            })
            .collect(),
    };

    let history = store.message_list(&secretary.id, Some(project.id), PLAN_HISTORY_LIMIT)?;
    let goal = compose_goal(project, &context, note, &history);
    let title = truncate_title(&goal, TITLE_MAX_CHARS);
    // ADR-0039 D2 / D5: 案件の作業場所を `NewTaskSpec` の形（`workspace` + `cluster`）に写す。
    // `Local` の `~` はここで `$HOME` に展開する（DB には展開済みの絶対パスが入っている想定だが、
    // 直接 DB を書いた案件でも同じ結果になるように、入口でもう一度通す）。
    let home = task_core::home_dir();
    let (plan_workspace, plan_cluster, plan_workspace_mode) = match project
        .workspace
        .as_ref()
        .map(|w| w.with_home_expanded(home.as_deref()))
    {
        Some(WorkspaceSpec::Local { path, .. }) => (Some(path), None, None),
        Some(WorkspaceSpec::Remote {
            cluster,
            path,
            mode,
        }) => (Some(path), Some(cluster), mode),
        None => (None, None, None),
    };

    let spec = NewTaskSpec {
        // ADR-0043 D2: 分解を起こす計画 run も案件の primary を継ぐ（`resolve_repos` の既定）。
        repos: Vec::new(),
        title,
        objective: goal,
        acceptance: Vec::new(),
        kind: TaskKind::Plan,
        tier: Some(PLAN_TIER),
        priority: Some(add::PriorityInput::Number(0)),
        parent: None,
        depends_on: Vec::new(),
        max_turns: Some(PLAN_MAX_TURNS),
        max_wall_secs: Some(PLAN_MAX_WALL_SECS),
        max_retries: PLAN_MAX_RETRIES,
        role: None,
        genre: None,
        aggregate: false,
        project_id: Some(project.id),
        milestone_id: target.as_ref().map(|m| m.id),
        assignee: Some(secretary.id.clone()),
        // ADR-0039 D2: 案件が作業場所を決めていれば、分解を起こす計画 run 自身もそこで走る
        // （子はここから継ぐ。決めていなければ従来どおりタスクごとの作業ディレクトリ）。
        workspace: plan_workspace,
        cluster: plan_cluster,
        workspace_mode: plan_workspace_mode,
        adapter: None,
        // ADR-0044 D3: 裏方の計画タスクにラベル・種類は付けない（`create_support_task` が `ready` にする）。
        labels: Vec::new(),
        category: None,
        // ADR-0046 D2 / D4（Phase 59）: 計画 run 自身に能力タグは要らない。進め方は子ごとに
        // 計画が決める（`NewTask.mode`）ので、ここは既定のまま。
        skills: Vec::new(),
        mode: None,
        status: None,
        features: None,
        provenance: add::SpecProvenance::system(),
    };
    let task = add::create_support_task(store, spec, roles, genres, now)?;

    if project.status == ProjectStatus::Proposed {
        store.project_set_status(project.id, ProjectStatus::Active)?;
    }
    if let Some(m) = &target {
        store.milestone_set_status(m.id, MilestoneStatus::InProgress)?;
    }

    Ok(StartedPlan { task })
}

fn truncate_title(goal: &str, max_chars: usize) -> String {
    let first_line = goal.lines().next().unwrap_or("");
    let head: String = first_line.chars().take(max_chars).collect();
    if head.trim().is_empty() {
        "案件の分解".to_string()
    } else {
        head
    }
}

/// 決定的な組み立て（LLM は呼ばない）。案件の `request`、途中目標、人の一言、秘書との直近のやり取りを
/// 1 つの文字列にまとめる。プランナー（秘書の計画の run）へそのまま `objective` として渡される。
pub fn compose_goal(
    project: &Project,
    milestones: &[Milestone],
    note: Option<&str>,
    history: &[Message],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "案件: {}\n\n依頼:\n{}\n",
        project.title,
        project.request.trim()
    ));

    if !milestones.is_empty() {
        out.push_str("\n途中目標:\n");
        for m in milestones {
            let description = m.description.trim();
            if description.is_empty() {
                out.push_str(&format!("- [{}] {}\n", m.status.as_str(), m.title));
            } else {
                out.push_str(&format!(
                    "- [{}] {}: {description}\n",
                    m.status.as_str(),
                    m.title
                ));
            }
        }
    }

    if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
        out.push_str(&format!("\n人からの一言:\n{note}\n"));
    }

    if !history.is_empty() {
        out.push_str("\n秘書との直近のやり取り:\n");
        for m in history {
            out.push_str(&format!("[{}] {}\n", m.role.as_str(), m.text));
        }
    }

    // ADR-0067 D1: 人が読む決定材料の置き場ルール（分解される子タスクの acceptance に反映させる）。
    out.push_str(
        "\n\n人が確認する成果物（調査報告・候補案・比較表など）は artifacts か知識ベースのページに置き、\
         対象リポジトリの追跡ファイル（docs/ を含む）には置かないこと。human チェックを持つ子タスクの \
         acceptance には artifact_exists か knowledge_page の条件を必ず添えること（ADR-0067）。\n",
    );

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{MessageId, MessageRole, OrgNode, ProjectId, SqliteStore};

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn node(id: &str, parent: Option<&str>, kind: OrgKind, genre: Option<&str>) -> OrgNode {
        let t = now();
        OrgNode {
            profile: Default::default(),
            id: id.into(),
            parent_id: parent.map(str::to_string),
            name: id.into(),
            kind,
            genre: genre.map(str::to_string),
            brief: String::new(),
            position: 0,
            created_at: t,
            updated_at: t,
        }
    }

    fn seed_org(store: &SqliteStore) {
        store
            .org_upsert(&node(
                "secretary",
                None,
                OrgKind::Secretary,
                Some("secretary"),
            ))
            .expect("secretary");
    }

    fn sample_project(status: ProjectStatus) -> Project {
        let t = now();
        Project {
            archived_at: None,
            paused_from: None,
            id: ProjectId::new(),
            title: "Pluvio を基盤に用いた新テーマ".into(),
            request: "Pluvio を基盤に用いた新たな研究テーマの模索、検証をしたい".into(),
            status,
            secretary_summary: None,
            workspace: None,
            created_at: t,
            updated_at: t,
        }
    }

    #[test]
    fn compose_goal_includes_request_milestones_note_and_history() {
        let project = sample_project(ProjectStatus::Proposed);
        let milestone = Milestone {
            paused_from: None,
            id: MilestoneId::new(),
            project_id: project.id,
            seq: 1,
            title: "隣接領域の調査".into(),
            description: "候補 3〜5 件".into(),
            status: MilestoneStatus::Approved,
            created_at: now(),
            updated_at: now(),
        };
        let history = vec![
            Message {
                id: MessageId::new(),
                node_id: "secretary".into(),
                project_id: Some(project.id),
                role: MessageRole::User,
                text: "この案件をお願いします".into(),
                run_id: None,
                task_id: None,
                metadata: None,
                created_at: now(),
            },
            Message {
                id: MessageId::new(),
                node_id: "secretary".into(),
                project_id: Some(project.id),
                role: MessageRole::Node,
                text: "承知しました。方針を検討します。".into(),
                run_id: Some("run-1".into()),
                task_id: None,
                metadata: None,
                created_at: now(),
            },
        ];
        let goal = compose_goal(
            &project,
            std::slice::from_ref(&milestone),
            Some("急がなくてよい"),
            &history,
        );
        assert!(goal.contains(&project.request));
        assert!(goal.contains("[approved] 隣接領域の調査: 候補 3〜5 件"));
        assert!(goal.contains("急がなくてよい"));
        assert!(goal.contains("[user] この案件をお願いします"));
        assert!(goal.contains("[node] 承知しました"));

        // 空の途中目標・note・履歴は節ごと出さない。
        let bare = compose_goal(&project, &[], None, &[]);
        assert!(!bare.contains("途中目標"));
        assert!(!bare.contains("人からの一言"));
        assert!(!bare.contains("直近のやり取り"));
    }

    #[test]
    fn start_creates_a_ready_plan_task_assigned_to_the_secretary_and_activates_the_project() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let project = sample_project(ProjectStatus::Proposed);
        store.project_create(&project).expect("create project");

        let started =
            start(&store, &project, None, Some("よろしく"), &[], &[], now()).expect("start");
        assert_eq!(started.task.kind, TaskKind::Plan);
        assert_eq!(
            started.task.status,
            task_core::Status::Ready,
            "その場で走らせる"
        );
        assert_eq!(started.task.project_id, Some(project.id));
        assert_eq!(started.task.milestone_id, None);
        assert_eq!(started.task.assignee.as_deref(), Some("secretary"));
        assert!(started.task.objective.contains(&project.request));
        assert!(started.task.objective.contains("よろしく"));
        assert!(started.task.acceptance.is_empty());

        let updated = store.project_get(project.id).expect("get").expect("some");
        assert_eq!(
            updated.status,
            ProjectStatus::Active,
            "proposed から active になる"
        );
    }

    #[test]
    fn start_with_an_explicit_milestone_carries_it_and_marks_it_in_progress() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let project = sample_project(ProjectStatus::Active);
        store.project_create(&project).expect("create project");
        let milestone = store
            .milestone_create(
                project.id,
                "隣接領域の調査",
                "候補 3〜5 件",
                MilestoneStatus::Approved,
            )
            .expect("create milestone");

        let started =
            start(&store, &project, Some(milestone.id), None, &[], &[], now()).expect("start");
        assert_eq!(started.task.milestone_id, Some(milestone.id));
        assert!(started.task.objective.contains("隣接領域の調査"));

        let updated_milestones = store.milestone_list(project.id).expect("list");
        assert_eq!(updated_milestones[0].status, MilestoneStatus::InProgress);
    }

    #[test]
    fn start_without_a_milestone_includes_only_approved_and_in_progress_ones() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let project = sample_project(ProjectStatus::Active);
        store.project_create(&project).expect("create project");
        store
            .milestone_create(project.id, "提案中", "", MilestoneStatus::Proposed)
            .expect("create");
        let approved = store
            .milestone_create(project.id, "承認済み", "", MilestoneStatus::Approved)
            .expect("create");
        let reached = store
            .milestone_create(project.id, "達成済み", "", MilestoneStatus::Reached)
            .expect("create");

        let started = start(&store, &project, None, None, &[], &[], now()).expect("start");
        assert!(started.task.objective.contains("承認済み"));
        assert!(!started.task.objective.contains("提案中"));
        assert!(!started.task.objective.contains("達成済み"));
        assert_eq!(
            started.task.milestone_id, None,
            "明示しなければタスク自体には特定の途中目標を付けない"
        );

        // 明示していないので、途中目標の状態は変わらない。
        let milestones = store.milestone_list(project.id).expect("list");
        assert_eq!(
            milestones
                .iter()
                .find(|m| m.id == approved.id)
                .expect("approved")
                .status,
            MilestoneStatus::Approved
        );
        assert_eq!(
            milestones
                .iter()
                .find(|m| m.id == reached.id)
                .expect("reached")
                .status,
            MilestoneStatus::Reached
        );
    }

    #[test]
    fn start_includes_up_to_the_last_20_messages_with_the_secretary() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let project = sample_project(ProjectStatus::Active);
        store.project_create(&project).expect("create project");
        for i in 0..25 {
            store
                .message_append(&Message {
                    id: MessageId::new(),
                    node_id: "secretary".into(),
                    project_id: Some(project.id),
                    role: MessageRole::User,
                    text: format!("message {i}"),
                    run_id: None,
                    task_id: None,
                    metadata: None,
                    created_at: now() - time::Duration::minutes(25 - i),
                })
                .expect("append");
        }
        let started = start(&store, &project, None, None, &[], &[], now()).expect("start");
        assert!(
            !started.task.objective.contains("message 4"),
            "{}",
            started.task.objective
        );
        assert!(started.task.objective.contains("message 5"));
        assert!(started.task.objective.contains("message 24"));
    }

    #[test]
    fn start_rejects_an_unknown_milestone_or_a_missing_secretary() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let project = sample_project(ProjectStatus::Active);
        store.project_create(&project).expect("create project");
        let other = sample_project(ProjectStatus::Active);
        store.project_create(&other).expect("create project");
        let foreign_milestone = store
            .milestone_create(other.id, "別案件", "", MilestoneStatus::Approved)
            .expect("create");

        let err = start(
            &store,
            &project,
            Some(foreign_milestone.id),
            None,
            &[],
            &[],
            now(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("does not belong to project"),
            "{err}"
        );

        let no_secretary = SqliteStore::open_in_memory().expect("open");
        let err = start(&no_secretary, &project, None, None, &[], &[], now()).unwrap_err();
        assert!(err.to_string().contains("no secretary"), "{err}");
    }

    /// ADR-0039 D2: 分解を起こす計画 run は案件の作業場所で走る（`Local` / `Remote` の両方）。
    /// 作業場所を決めていない案件では従来どおりタスクごとの相対パス。
    #[test]
    fn the_plan_run_uses_the_projects_workspace() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);

        let plain = sample_project(ProjectStatus::Active);
        store.project_create(&plain).expect("create project");
        let started = start(&store, &plain, None, None, &[], &[], now()).expect("start");
        assert_eq!(
            started.task.workspace,
            WorkspaceSpec::Local {
                path: std::path::PathBuf::from(started.task.id.to_string()),
                mode: None
            },
            "作業場所を決めていない案件は従来どおり"
        );

        let mut local = sample_project(ProjectStatus::Active);
        local.workspace = Some(WorkspaceSpec::Local {
            path: std::path::PathBuf::from("/home/rmaeda/workspace/rust/pluvio-poc"),
            mode: None,
        });
        store.project_create(&local).expect("create project");
        let started = start(&store, &local, None, None, &[], &[], now()).expect("start");
        assert_eq!(
            started.task.workspace,
            local.workspace.clone().expect("some")
        );

        let mut remote = sample_project(ProjectStatus::Active);
        remote.workspace = Some(WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: std::path::PathBuf::from("/work/NBB/rmaeda/workspace/rust/benchfs"),
            mode: None,
        });
        store.project_create(&remote).expect("create project");
        let started = start(&store, &remote, None, None, &[], &[], now()).expect("start");
        assert_eq!(
            started.task.workspace,
            remote.workspace.clone().expect("some")
        );
    }
}
