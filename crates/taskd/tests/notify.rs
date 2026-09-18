//! ADR-0037（Phase 39）: 人の判断が要るときだけ Discord に知らせる。
//!
//! 見るもの:
//! - 判定 5 種それぞれ「条件成立で 1 件」「2 回目の tick で増えない」「条件が解消したら作らない」。
//! - 送信は**偽の HTTP サーバ**（`tokio::net::TcpListener` で 1 リクエスト受けて 204 を返す）へ。
//!   外部ネットワークには出ない（CLAUDE.md の禁止事項）。
//! - 3 回失敗したら諦める。秘密が無ければ送らない（pending も溜めない）。
//! - 失敗の文面に URL・ホスト名が出ない。

use std::path::Path;

use task_core::approval::{Approval, ApprovalId, ApprovalStore, Decision};
use task_core::message::{Message, MessageId, MessageRole};
use task_core::notify::{MAX_NOTIFY_ATTEMPTS, NotificationKind, NotificationStore};
use task_core::org::{OrgKind, OrgNode};
use task_core::report::{Report, ReportId, ReportKind, ReportStore};
use task_core::{
    Budget, Check, Criterion, MilestoneStatus, Project, ProjectId, ProjectStatus, SqliteStore, Status, Task,
    TaskId, TaskKind, TaskStore, Tier, Trigger, WorkerHint, WorkspaceSpec,
};
use taskd::notify::{self, NotifyConfig, SendResult};
use time::OffsetDateTime;

fn at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_800_000_000 + secs).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

struct Env {
    _dir: tempfile::TempDir,
    store: SqliteStore,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let store = SqliteStore::open(&dir.path().join("taskd.db")).unwrap_or_else(|e| panic!("open: {e}"));
        Self { _dir: dir, store }
    }

    fn as_store(&self) -> &dyn TaskStore {
        &self.store
    }

    /// 判定して pending を作り、その種の件数を返す。
    fn schedule(&self, kind: NotificationKind) -> usize {
        let created = notify::schedule(self.as_store(), &NotifyConfig::default(), OffsetDateTime::now_utc())
            .unwrap_or_else(|e| panic!("schedule: {e}"));
        created.iter().filter(|n| n.kind == kind).count()
    }

    /// 判定だけ（DB には書かない）。
    fn scanned(&self, kind: NotificationKind) -> Vec<String> {
        notify::scan(self.as_store(), None)
            .unwrap_or_else(|e| panic!("scan: {e}"))
            .into_iter()
            .filter(|c| c.kind == kind)
            .map(|c| c.key)
            .collect()
    }

    fn seed_org(&self) {
        let now = at(0);
        for (id, name, parent, kind) in [
            ("secretary", "秘書", None, OrgKind::Secretary),
            ("poc", "検証課", Some("secretary"), OrgKind::Section),
        ] {
            self.store
                .org_upsert(&OrgNode {
                    id: id.into(),
                    parent_id: parent.map(str::to_string),
                    name: name.into(),
                    kind,
                    genre: None,
                    brief: String::new(),
                    position: 0,
                    created_at: now,
                    updated_at: now,
                })
                .unwrap_or_else(|e| panic!("org: {e}"));
        }
    }

    fn seed_project(&self, status: ProjectStatus) -> ProjectId {
        let project = Project {
            id: ProjectId::new(),
            title: "Pluvio の検証".into(),
            request: "調べて".into(),
            status,
            secretary_summary: None,
            created_at: at(0),
            updated_at: at(0),
        };
        self.store.project_create(&project).unwrap_or_else(|e| panic!("project: {e}"));
        project.id
    }
}

fn task(status: Status) -> Task {
    let now = at(0);
    Task {
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: "候補テーマの統合".into(),
        objective: "o".into(),
        acceptance: vec![Criterion { text: "c".into(), check: Check::Human }],
        inputs: vec![],
        depends_on: vec![],
        status,
        priority: 0,
        worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
        workspace: WorkspaceSpec::Local { path: "ws".into() },
        budget: Budget { max_turns: 1, max_wall_secs: 1, max_retries: 0 },
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
    }
}

// ---- 1. milestone_ready ----

#[test]
fn milestone_ready_fires_once_when_every_real_task_is_terminal() {
    let env = Env::new();
    env.seed_org();
    let project = env.seed_project(ProjectStatus::Active);
    let milestone = env
        .store
        .milestone_create(project, "候補テーマの選定", "", MilestoneStatus::InProgress)
        .unwrap_or_else(|e| panic!("milestone: {e}"));

    // まだ走っている仕事があるうちは知らせない。
    let mut running = task(Status::Running);
    running.project_id = Some(project);
    running.milestone_id = Some(milestone.id);
    env.store.insert(&running).unwrap_or_else(|e| panic!("insert: {e}"));
    let mut done = task(Status::Done);
    done.project_id = Some(project);
    done.milestone_id = Some(milestone.id);
    env.store.insert(&done).unwrap_or_else(|e| panic!("insert: {e}"));
    assert_eq!(env.schedule(NotificationKind::MilestoneReady), 0);

    // 裏方（レビュー）は数えない: 終端でなくても判定に影響しない。
    let mut support = task(Status::Running);
    support.kind = TaskKind::Review;
    support.project_id = Some(project);
    support.milestone_id = Some(milestone.id);
    env.store.insert(&support).unwrap_or_else(|e| panic!("insert: {e}"));

    // 残りが終端になったら 1 件。
    env.store
        .apply_transition(running.id, Trigger::Cancel, None)
        .unwrap_or_else(|e| panic!("cancel: {e}"));
    assert_eq!(env.schedule(NotificationKind::MilestoneReady), 1);
    // 2 回目の tick では増えない。
    assert_eq!(env.schedule(NotificationKind::MilestoneReady), 0);

    let rows = env.store.notification_recent(10).unwrap_or_else(|e| panic!("recent: {e}"));
    let row = rows
        .iter()
        .find(|n| n.kind == NotificationKind::MilestoneReady)
        .unwrap_or_else(|| panic!("no milestone_ready row"));
    assert_eq!(row.key, milestone.id.to_string());
    assert!(row.body.contains("候補テーマの選定"), "{}", row.body);
    assert!(row.body.contains("done 1 / failed 0"), "{}", row.body);

    // 条件が解消（`reached` にした）ら、もう候補に出てこない。
    env.store
        .milestone_set_status(milestone.id, MilestoneStatus::Reached)
        .unwrap_or_else(|e| panic!("set: {e}"));
    assert!(env.scanned(NotificationKind::MilestoneReady).is_empty());
}

#[test]
fn a_milestone_without_any_real_task_is_never_ready() {
    let env = Env::new();
    let project = env.seed_project(ProjectStatus::Active);
    let milestone = env
        .store
        .milestone_create(project, "まだ仕事が無い", "", MilestoneStatus::Approved)
        .unwrap_or_else(|e| panic!("milestone: {e}"));
    // 裏方だけでは「終わった」とみなさない。
    let mut support = task(Status::Done);
    support.kind = TaskKind::Approval;
    support.project_id = Some(project);
    support.milestone_id = Some(milestone.id);
    env.store.insert(&support).unwrap_or_else(|e| panic!("insert: {e}"));
    assert_eq!(env.schedule(NotificationKind::MilestoneReady), 0);
}

// ---- 2. approval_pending ----

#[test]
fn approval_pending_fires_once_per_undecided_approval() {
    let env = Env::new();
    env.seed_org();
    let approval = Approval {
        id: ApprovalId::new(),
        project_id: None,
        node_id: "poc".into(),
        task_id: None,
        question: "本番の DB を触ってよいですか".into(),
        decision: None,
        answer: None,
        created_at: at(0),
        decided_at: None,
    };
    env.store.approval_append(&approval).unwrap_or_else(|e| panic!("approval: {e}"));

    assert_eq!(env.schedule(NotificationKind::ApprovalPending), 1);
    assert_eq!(env.schedule(NotificationKind::ApprovalPending), 0);

    let rows = env.store.notification_recent(10).unwrap_or_else(|e| panic!("recent: {e}"));
    let row = rows
        .iter()
        .find(|n| n.kind == NotificationKind::ApprovalPending)
        .unwrap_or_else(|| panic!("no approval_pending row"));
    assert_eq!(row.key, approval.id.to_string());
    // 担当はノードの表示名で出る。
    assert!(row.body.contains("検証課"), "{}", row.body);
    assert!(row.body.contains("本番の DB"), "{}", row.body);

    // 人が答えたら候補から消える。
    env.store
        .approval_decide(approval.id, Decision::Once, Some("よい".into()), at(10))
        .unwrap_or_else(|e| panic!("decide: {e}"));
    assert!(env.scanned(NotificationKind::ApprovalPending).is_empty());
}

// ---- 3. question_blocked ----

#[test]
fn question_blocked_fires_once_and_defers_to_approval_pending() {
    let env = Env::new();
    env.seed_org();
    let mut blocked = task(Status::Blocked);
    blocked.assignee = Some("poc".into());
    env.store.insert(&blocked).unwrap_or_else(|e| panic!("insert: {e}"));

    assert_eq!(env.schedule(NotificationKind::QuestionBlocked), 1);
    assert_eq!(env.schedule(NotificationKind::QuestionBlocked), 0);
    let rows = env.store.notification_recent(10).unwrap_or_else(|e| panic!("recent: {e}"));
    let row = rows
        .iter()
        .find(|n| n.kind == NotificationKind::QuestionBlocked)
        .unwrap_or_else(|| panic!("no question_blocked row"));
    assert_eq!(row.key, blocked.id.to_string());
    assert!(row.body.contains("検証課"), "{}", row.body);

    // 認可がある `blocked` は `approval_pending` に任せる（二重に知らせない）。
    let other = {
        let mut t = task(Status::Blocked);
        t.assignee = Some("poc".into());
        t
    };
    env.store.insert(&other).unwrap_or_else(|e| panic!("insert: {e}"));
    env.store
        .approval_append(&Approval {
            id: ApprovalId::new(),
            project_id: None,
            node_id: "poc".into(),
            task_id: Some(other.id),
            question: "聞きたい".into(),
            decision: None,
            answer: None,
            created_at: at(0),
            decided_at: None,
        })
        .unwrap_or_else(|e| panic!("approval: {e}"));
    assert!(
        !env.scanned(NotificationKind::QuestionBlocked).contains(&other.id.to_string()),
        "認可がある blocked は question_blocked にしない"
    );
    assert!(env.scanned(NotificationKind::ApprovalPending).len() == 1);

    // 答えてタスクが動き出したら候補から消える。
    env.store
        .apply_transition(blocked.id, Trigger::Answer, None)
        .unwrap_or_else(|e| panic!("answer: {e}"));
    assert!(!env.scanned(NotificationKind::QuestionBlocked).contains(&blocked.id.to_string()));
}

// ---- 4. bad_news ----

#[test]
fn bad_news_fires_once_for_level_zero_reports_only() {
    let env = Env::new();
    let report = Report {
        id: ReportId::new(),
        project_id: None,
        node_id: "secretary".into(),
        task_id: None,
        kind: ReportKind::BadNews,
        level: 0,
        headline: "クラスタに入れません".into(),
        body: "b".into(),
        sources: vec![],
        read_at: None,
        created_at: at(0),
    };
    env.store.report_append(&report).unwrap_or_else(|e| panic!("report: {e}"));
    // 下の階層の複製（level > 0）と、悪くない報告は知らせない。
    for (kind, level) in [(ReportKind::BadNews, 1), (ReportKind::Result, 0)] {
        env.store
            .report_append(&Report {
                id: ReportId::new(),
                kind,
                level,
                node_id: "poc".into(),
                ..report.clone()
            })
            .unwrap_or_else(|e| panic!("report: {e}"));
    }

    assert_eq!(env.schedule(NotificationKind::BadNews), 1);
    assert_eq!(env.schedule(NotificationKind::BadNews), 0);
    assert_eq!(env.scanned(NotificationKind::BadNews), vec![report.id.to_string()]);

    let rows = env.store.notification_recent(10).unwrap_or_else(|e| panic!("recent: {e}"));
    let row = rows
        .iter()
        .find(|n| n.kind == NotificationKind::BadNews)
        .unwrap_or_else(|| panic!("no bad_news row"));
    assert!(row.body.contains("クラスタに入れません"), "{}", row.body);
}

#[test]
fn a_store_without_bad_news_produces_nothing() {
    let env = Env::new();
    assert_eq!(env.schedule(NotificationKind::BadNews), 0);
    assert!(env.scanned(NotificationKind::BadNews).is_empty());
}

// ---- 5. secretary_reply ----

#[test]
fn secretary_reply_fires_once_when_a_proposed_project_gets_a_node_message() {
    let env = Env::new();
    env.seed_org();
    let project = env.seed_project(ProjectStatus::Proposed);

    // 人の発言だけでは知らせない（返事待ちなのはこちらではない）。
    let user = Message {
        id: MessageId::new(),
        node_id: "secretary".into(),
        project_id: Some(project),
        role: MessageRole::User,
        text: "お願いします".into(),
        run_id: None,
        task_id: None,
        created_at: at(0),
    };
    env.store.message_append(&user).unwrap_or_else(|e| panic!("message: {e}"));
    assert_eq!(env.schedule(NotificationKind::SecretaryReply), 0);

    env.store
        .message_append(&Message {
            id: MessageId::new(),
            role: MessageRole::Node,
            text: "理解しました。最初の途中目標はこうします".into(),
            created_at: at(1),
            ..user.clone()
        })
        .unwrap_or_else(|e| panic!("message: {e}"));

    assert_eq!(env.schedule(NotificationKind::SecretaryReply), 1);
    assert_eq!(env.schedule(NotificationKind::SecretaryReply), 0);
    assert_eq!(env.scanned(NotificationKind::SecretaryReply), vec![project.to_string()]);

    // 人が返事をして案件が動き出したら（`proposed` でなくなったら）候補から消える。
    env.store
        .project_set_status(project, ProjectStatus::Active)
        .unwrap_or_else(|e| panic!("status: {e}"));
    assert!(env.scanned(NotificationKind::SecretaryReply).is_empty());
}

// ---- 文面のリンク ----

#[test]
fn links_are_added_only_when_a_gui_base_url_is_configured() {
    let env = Env::new();
    env.seed_org();
    let project = env.seed_project(ProjectStatus::Active);
    let milestone = env
        .store
        .milestone_create(project, "m", "", MilestoneStatus::Approved)
        .unwrap_or_else(|e| panic!("milestone: {e}"));
    let mut done = task(Status::Done);
    done.project_id = Some(project);
    done.milestone_id = Some(milestone.id);
    env.store.insert(&done).unwrap_or_else(|e| panic!("insert: {e}"));

    let without = notify::scan(env.as_store(), None).unwrap_or_else(|e| panic!("scan: {e}"));
    assert!(without.iter().all(|c| !c.body.contains("http")));

    let with = notify::scan(env.as_store(), Some("http://192.168.1.103:7700"))
        .unwrap_or_else(|e| panic!("scan: {e}"));
    let body = &with
        .iter()
        .find(|c| c.kind == NotificationKind::MilestoneReady)
        .unwrap_or_else(|| panic!("no candidate"))
        .body;
    assert!(
        body.contains(&format!("http://192.168.1.103:7700/projects/{project}")),
        "{body}"
    );
}

// ---- 送信（偽の HTTP サーバ。外部ネットワークには出ない）----

/// `127.0.0.1:0` で待ち受け、リクエストを `max` 件受けて `status` を返す。返るのは URL と、
/// 受け取った本文を集めるハンドル。
async fn fake_webhook(max: usize, status: u16) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|e| panic!("bind: {e}"));
    let addr = listener.local_addr().unwrap_or_else(|e| panic!("addr: {e}"));
    let handle = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut bodies = Vec::new();
        for _ in 0..max {
            let Ok((mut socket, _)) = listener.accept().await else { break };
            let mut buf = vec![0u8; 8192];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            bodies.push(String::from_utf8_lossy(&buf[..n]).to_string());
            let response = format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
            let _ = socket.shutdown().await;
        }
        bodies
    });
    (format!("http://{addr}/hook"), handle)
}

#[tokio::test]
async fn a_pending_notification_is_posted_once_and_marked_sent() {
    let env = Env::new();
    let row = env
        .store
        .notification_upsert_pending(NotificationKind::BadNews, "r1", "悪い知らせ: テスト", at(0))
        .unwrap_or_else(|e| panic!("upsert: {e}"))
        .unwrap_or_else(|| panic!("row"));
    let (url, server) = fake_webhook(1, 204).await;
    let client = notify::client().unwrap_or_else(|| panic!("client"));

    let (tx, mut rx) = tokio::sync::mpsc::channel::<SendResult>(4);
    notify::spawn_send(client, url, &row, tx);
    let result = rx.recv().await.unwrap_or_else(|| panic!("no result"));
    assert!(result.ok, "{result:?}");
    assert_eq!(result.id, row.id);

    let bodies = server.await.unwrap_or_else(|e| panic!("server: {e}"));
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].starts_with("POST /hook "), "{}", bodies[0]);
    assert!(bodies[0].contains("悪い知らせ"), "{}", bodies[0]);
    assert!(bodies[0].contains("\"username\":\"taskd\""), "{}", bodies[0]);

    // 次の tick で台帳に書かれる。
    let pending = env.store.notification_pending().unwrap_or_else(|e| panic!("pending: {e}"));
    notify::record(env.as_store(), &pending, &result, at(1)).unwrap_or_else(|e| panic!("record: {e}"));
    assert!(env.store.notification_pending().unwrap_or_default().is_empty());
    let recent = env.store.notification_recent(5).unwrap_or_else(|e| panic!("recent: {e}"));
    assert_eq!(recent[0].ok, Some(true));
    assert_eq!(recent[0].sent_at, Some(at(1)));
}

#[tokio::test]
async fn a_failing_webhook_is_retried_three_times_and_then_given_up() {
    let env = Env::new();
    let row = env
        .store
        .notification_upsert_pending(NotificationKind::BadNews, "r1", "b", at(0))
        .unwrap_or_else(|e| panic!("upsert: {e}"))
        .unwrap_or_else(|| panic!("row"));
    // 500 を返す偽サーバ（4 回分受けられるが、諦めるので 3 回しか来ない）。
    let (url, server) = fake_webhook(4, 500).await;
    let client = notify::client().unwrap_or_else(|| panic!("client"));

    let mut attempts = 0;
    for tick in 0..5 {
        let pending = env.store.notification_pending().unwrap_or_else(|e| panic!("pending: {e}"));
        let Some(next) = pending.first().cloned() else { break };
        attempts += 1;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<SendResult>(4);
        notify::spawn_send(client.clone(), url.clone(), &next, tx);
        let result = rx.recv().await.unwrap_or_else(|| panic!("no result"));
        assert!(!result.ok);
        let error = result.error.clone().unwrap_or_default();
        assert_eq!(error, "http status 500");
        // 失敗の文面に URL・ホスト名は出ない（ADR-0037 D3）。
        assert!(!error.contains("127.0.0.1"), "{error}");
        assert!(!error.contains("http://"), "{error}");
        notify::record(env.as_store(), &pending, &result, at(tick)).unwrap_or_else(|e| panic!("record: {e}"));
    }
    assert_eq!(attempts, MAX_NOTIFY_ATTEMPTS as usize, "3 回で諦める");

    let recent = env.store.notification_recent(5).unwrap_or_else(|e| panic!("recent: {e}"));
    let found = recent.iter().find(|n| n.id == row.id).unwrap_or_else(|| panic!("row"));
    assert_eq!(found.ok, Some(false));
    assert_eq!(found.attempts, MAX_NOTIFY_ATTEMPTS);
    assert!(found.sent_at.is_none());
    assert!(
        found.error.as_deref().unwrap_or("").contains("gave up"),
        "{:?}",
        found.error
    );
    drop(server);
}

#[tokio::test]
async fn an_unreachable_webhook_never_reveals_the_host() {
    // 誰も待っていないポート（接続できない）。偽サーバを立ててすぐ落とす。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|e| panic!("bind: {e}"));
    let addr = listener.local_addr().unwrap_or_else(|e| panic!("addr: {e}"));
    drop(listener);
    let client = notify::client().unwrap_or_else(|| panic!("client"));
    let error = notify::post_webhook(&client, &format!("http://{addr}/hook"), "x")
        .await
        .err()
        .unwrap_or_else(|| panic!("expected a failure"));
    assert!(!error.contains("127.0.0.1"), "{error}");
    assert!(!error.contains(&addr.port().to_string()), "{error}");
    assert!(!error.contains("hook"), "{error}");
}

// ---- 秘密が無い間は送らない（ADR-0037 D2）----

#[test]
fn without_a_secret_nothing_is_sent_and_no_pending_row_is_kept() {
    let env = Env::new();
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    // `[secrets]` が無い / ファイルが無い、どちらでも URL は読めない。
    assert_eq!(notify::webhook_url(None, "discord-webhook"), None);
    assert_eq!(notify::webhook_url(Some(dir.path()), "discord-webhook"), None);

    env.store
        .report_append(&Report {
            id: ReportId::new(),
            project_id: None,
            node_id: "secretary".into(),
            task_id: None,
            kind: ReportKind::BadNews,
            level: 0,
            headline: "落ちました".into(),
            body: String::new(),
            sources: vec![],
            read_at: None,
            created_at: at(0),
        })
        .unwrap_or_else(|e| panic!("report: {e}"));

    // 判定はする（1 件できる）。
    assert_eq!(env.schedule(NotificationKind::BadNews), 1);
    // が、送れないので pending は溜めずに畳む。
    let pending = env.store.notification_pending().unwrap_or_else(|e| panic!("pending: {e}"));
    assert_eq!(pending.len(), 1);
    let discarded =
        notify::discard_pending(env.as_store(), &pending, at(1)).unwrap_or_else(|e| panic!("discard: {e}"));
    assert_eq!(discarded, 1);
    assert!(env.store.notification_pending().unwrap_or_default().is_empty());

    let recent = env.store.notification_recent(5).unwrap_or_else(|e| panic!("recent: {e}"));
    assert_eq!(recent[0].ok, Some(false));
    assert_eq!(recent[0].error.as_deref(), Some(notify::NOT_CONFIGURED));
    assert_eq!(recent[0].attempts, 0, "送っていないので試行は 0");

    // 後から秘密を登録しても、その間の出来事は蒸し返さない（`(kind, key)` は既に埋まっている）。
    assert_eq!(env.schedule(NotificationKind::BadNews), 0);
}

#[test]
fn the_webhook_secret_is_read_from_the_secrets_dir() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    std::fs::write(dir.path().join("discord-webhook"), "https://example.invalid/webhooks/1/abc\n")
        .unwrap_or_else(|e| panic!("write: {e}"));
    assert_eq!(
        notify::webhook_url(Some(dir.path() as &Path), "discord-webhook").as_deref(),
        Some("https://example.invalid/webhooks/1/abc")
    );
    // 別の id を指せば見つからない。
    assert_eq!(notify::webhook_url(Some(dir.path()), "other"), None);
}

// ---- 実機相当: `POST /notify/test` の中身を偽サーバ（`[secrets]` に入れた URL）へ 1 回通す ----

#[tokio::test]
async fn send_test_posts_one_message_to_the_url_in_the_secrets_dir() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let (url, server) = fake_webhook(1, 204).await;
    std::fs::write(dir.path().join("discord-webhook"), format!("{url}\n"))
        .unwrap_or_else(|e| panic!("write: {e}"));
    let client = notify::client();

    let result = notify::send_test(client.as_ref(), Some(dir.path()), "discord-webhook").await;
    assert_eq!(result, notify::TestSend::Sent, "{result:?}");
    let bodies = server.await.unwrap_or_else(|e| panic!("server: {e}"));
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].starts_with("POST /hook "), "{}", bodies[0]);
    assert!(bodies[0].contains("taskd"), "{}", bodies[0]);

    // 秘密が無ければ送らない（API はこれを 409 `notify_unavailable` にする）。
    let missing = notify::send_test(client.as_ref(), Some(dir.path()), "nope").await;
    match missing {
        notify::TestSend::NotConfigured(detail) => {
            assert!(detail.contains("nope"), "{detail}");
            assert!(!detail.contains("127.0.0.1"), "{detail}");
        }
        other => panic!("expected NotConfigured, got {other:?}"),
    }
}
