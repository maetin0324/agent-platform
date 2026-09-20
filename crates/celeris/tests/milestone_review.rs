//! ADR-0038 D1（Phase 41）: 途中目標の仕事が止まったら、秘書の「途中目標レビュー」の対話 run を
//! **1 回だけ**起こす（通知はその返事が付いてから。D4）。
//!
//! 見るもの:
//! - 同じ done の集合ではレビューが 1 回だけ起きる（2 回目の tick では増えない）。Go の後に done が
//!   増えたら再び起きる。
//! - 起きた対話は秘書の裏方（`support = "milestone_review"`）で、`milestone_id` を持ち、本文に結果の
//!   件数が入る。
//! - `reached` の途中目標、動いている仕事がある途中目標、done が無い途中目標では起きない。
//! - 通知（`milestone_ready`）は返事が `messages` に入ってから出て、文面に要約と提案の題名が入る。

use celeris::notify::{self, NotifyConfig};
use task_core::message::{Message, MessageId, MessageRole};
use task_core::notify::{NotificationKind, NotificationStore};
use task_core::org::{OrgKind, OrgNode};
use task_core::{
    Budget, Check, Criterion, Milestone, MilestoneStatus, Project, ProjectId, ProjectStatus,
    SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, Tier, Trigger, WorkerHint,
    WorkspaceSpec,
};
use time::OffsetDateTime;

/// 2023-11 を基準にする（`apply_transition` が打つ現在時刻より**前**であること: レビューを起こし直す
/// 判定が「レビューより後に done が増えたか」で決まるため）。
fn at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000 + secs).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

struct Env {
    _dir: tempfile::TempDir,
    store: SqliteStore,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let store = SqliteStore::open(&dir.path().join("celeris.db"))
            .unwrap_or_else(|e| panic!("open: {e}"));
        let env = Self { _dir: dir, store };
        env.store
            .org_upsert(&OrgNode {
                profile: Default::default(),
                id: "secretary".into(),
                parent_id: None,
                name: "秘書".into(),
                kind: OrgKind::Secretary,
                genre: None,
                brief: String::new(),
                position: 0,
                created_at: at(0),
                updated_at: at(0),
            })
            .unwrap_or_else(|e| panic!("org: {e}"));
        env
    }

    fn project(&self) -> Project {
        let project = Project {
            archived_at: None,
            paused_from: None,
            id: ProjectId::new(),
            title: "Pluvio の検証".into(),
            request: "調べて".into(),
            status: ProjectStatus::Active,
            secretary_summary: None,
            workspace: None,
            created_at: at(0),
            updated_at: at(0),
        };
        self.store
            .project_create(&project)
            .unwrap_or_else(|e| panic!("project: {e}"));
        project
    }

    fn milestone(&self, project: ProjectId, title: &str, status: MilestoneStatus) -> Milestone {
        self.store
            .milestone_create(project, title, "近い分野を洗う", status)
            .unwrap_or_else(|e| panic!("milestone: {e}"))
    }

    fn work(&self, project: ProjectId, milestone: &Milestone, status: Status) -> Task {
        let mut t = task(status);
        t.project_id = Some(project);
        t.milestone_id = Some(milestone.id);
        self.store
            .insert(&t)
            .unwrap_or_else(|e| panic!("insert: {e}"));
        t
    }

    /// tick 1 回ぶんのレビューの判定（作られた対話タスクを返す）。
    fn schedule(&self, now: OffsetDateTime) -> Vec<Task> {
        celeris::milestone_review::schedule(
            &self.store,
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            now,
        )
        .unwrap_or_else(|e| panic!("schedule: {e}"))
    }

    /// その対話 run の返事（ディスパッチャがするのと同じこと）。
    fn reply(&self, task_id: TaskId, project: ProjectId, text: &str) {
        self.store
            .message_append(&Message {
                id: MessageId::new(),
                node_id: "secretary".into(),
                project_id: Some(project),
                role: MessageRole::Node,
                text: text.to_string(),
                run_id: Some("run-1".into()),
                task_id: Some(task_id),
                metadata: None,
                created_at: at(100),
            })
            .unwrap_or_else(|e| panic!("message: {e}"));
    }
}

fn task(status: Status) -> Task {
    Task {
        mode: Default::default(),
        skills: Vec::new(),
        repos: Vec::new(),
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: "候補テーマの調査".into(),
        objective: "o".into(),
        acceptance: vec![Criterion {
            text: "c".into(),
            check: Check::Human,
        }],
        inputs: vec![],
        depends_on: vec![],
        status,
        priority: 0,
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
        created_at: at(0),
        updated_at: at(0),
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

/// 受け入れ 1: レビューは同じ done の集合で 1 回だけ起き、Go の後に done が増えたら再び起きる。
#[test]
fn the_review_run_happens_once_per_set_of_finished_work() {
    let env = Env::new();
    let project = env.project();
    let milestone = env.milestone(
        project.id,
        "隣接領域の動向調査",
        MilestoneStatus::InProgress,
    );
    env.work(project.id, &milestone, Status::Done);
    let draft = env.work(project.id, &milestone, Status::Draft);

    let started = env.schedule(at(10));
    assert_eq!(started.len(), 1, "レビューは 1 件: {started:?}");
    let review = &started[0];
    assert_eq!(review.assignee.as_deref(), Some("secretary"));
    assert_eq!(review.milestone_id, Some(milestone.id));
    assert_eq!(
        task_core::support_kind(review),
        Some("milestone_review"),
        "裏方の印"
    );
    assert_eq!(review.status, Status::Ready);
    assert!(
        review.objective.contains("隣接領域の動向調査"),
        "{}",
        review.objective
    );
    assert!(
        review.objective.contains("done 1 / failed 0、Go 待ち 1 件"),
        "{}",
        review.objective
    );

    // 2 回目の tick では起きない（done の集合が同じ）。
    assert!(
        env.schedule(at(20)).is_empty(),
        "同じ done の集合で 2 回目は起きない"
    );

    // Go: draft を最後まで進めて done にすると、結果が増えたのでもう一度まとめてもらう。
    for trigger in [
        Trigger::Accept,
        Trigger::Dispatch,
        Trigger::WorkerDone,
        Trigger::ReviewPass,
    ] {
        env.store
            .apply_transition(draft.id, trigger, None)
            .unwrap_or_else(|e| panic!("transition: {e}"));
    }
    let again = env.schedule(OffsetDateTime::now_utc());
    assert_eq!(again.len(), 1, "done が増えたら再び起きる: {again:?}");
    assert!(
        again[0].objective.contains("done 2"),
        "{}",
        again[0].objective
    );
}

/// 受け入れ 1: 動いている仕事がある／done が無い／`reached` の途中目標ではレビューは起きない。
#[test]
fn no_review_while_work_is_running_or_when_the_milestone_is_reached() {
    let env = Env::new();
    let project = env.project();
    let milestone = env.milestone(
        project.id,
        "隣接領域の動向調査",
        MilestoneStatus::InProgress,
    );
    let running = env.work(project.id, &milestone, Status::Running);
    env.work(project.id, &milestone, Status::Done);
    assert!(
        env.schedule(at(10)).is_empty(),
        "動いているものがあれば起こさない"
    );

    // draft しか無い（done が 0）途中目標でも起こさない。
    let fresh = env.milestone(project.id, "これから", MilestoneStatus::Approved);
    env.work(project.id, &fresh, Status::Draft);
    assert!(env.schedule(at(11)).is_empty());

    // 動いているものが片付けば起きるが、`reached` なら起きない。
    env.store
        .apply_transition(running.id, Trigger::Cancel, None)
        .unwrap_or_else(|e| panic!("cancel: {e}"));
    env.store
        .milestone_set_status(milestone.id, MilestoneStatus::Reached)
        .unwrap_or_else(|e| panic!("set: {e}"));
    assert!(env.schedule(at(12)).is_empty(), "reached なら起こさない");
    env.store
        .milestone_set_status(milestone.id, MilestoneStatus::InProgress)
        .unwrap_or_else(|e| panic!("set: {e}"));
    assert_eq!(env.schedule(at(13)).len(), 1);
}

/// 受け入れ 1 / D4: 通知はレビューの返事が付いてから出て、文面に要約と提案の題名が入る。
#[test]
fn the_notification_waits_for_the_reply_and_carries_the_summary() {
    let env = Env::new();
    let project = env.project();
    let milestone = env.milestone(
        project.id,
        "隣接領域の動向調査",
        MilestoneStatus::InProgress,
    );
    env.work(project.id, &milestone, Status::Done);

    let started = env.schedule(at(10));
    assert_eq!(started.len(), 1);
    // レビューの run が終わるまでは通知は出ない。
    let created = notify::schedule(&env.store, &NotifyConfig::default(), at(0), at(11))
        .unwrap_or_else(|e| panic!("notify: {e}"));
    assert!(
        created
            .iter()
            .all(|n| n.kind != NotificationKind::MilestoneReady),
        "返事の前に鳴ってはいけない: {created:?}"
    );

    // 返事（と、その返事が宣言した次の途中目標）が入ると鳴る。
    env.reply(
        started[0].id,
        project.id,
        "候補を 3 本に絞りました。次は比較実験です。",
    );
    task_ops::milestone_review::record_proposal(
        &env.store,
        project.id,
        Some(milestone.id),
        "候補の比較実験",
        "",
    )
    .unwrap_or_else(|e| panic!("record: {e}"));
    let created = notify::schedule(&env.store, &NotifyConfig::default(), at(0), at(12))
        .unwrap_or_else(|e| panic!("notify: {e}"));
    let row = created
        .iter()
        .find(|n| n.kind == NotificationKind::MilestoneReady)
        .unwrap_or_else(|| panic!("no milestone_ready: {created:?}"));
    assert!(row.body.contains("候補を 3 本に絞りました"), "{}", row.body);
    assert!(
        row.body.contains("次の提案: 『候補の比較実験』"),
        "{}",
        row.body
    );
    assert!(row.body.contains("ok / 議論 / ng"), "{}", row.body);
    assert_eq!(row.project_id, Some(project.id));

    // レビューの対話タスクそのものは「動いている仕事」に数えない（数えると条件が二度と成立しない）。
    assert_eq!(
        env.store
            .notification_recent(10)
            .unwrap_or_else(|e| panic!("recent: {e}"))
            .len(),
        1
    );
}
