//! 対話（ADR-0033 D4 / Phase 24）。人が組織のノードに話しかけ、そのノードが返事をする経路の
//! 「判断と検証」を 1 か所にまとめる（`task-api` と `task-dispatch` の両方から呼ぶ）。
//!
//! **新しいプロトコルは作らない**: 話しかけると `messages` に `role = user` の行が 1 つ増え、
//! 既存の `tasks` に `kind = execute` の対話用タスクが 1 件できるだけ。返事はその run の
//! `artifacts/result.json` の `summary`（ADR-0006 D3）で、ディスパッチャが `role = node` の行にする。
//!
//! I/O は `TaskStore` の読み書きだけ。LLM 呼び出しは無い（DESIGN 原則 1）。

use std::path::PathBuf;

use task_core::approval::{Approval, Decision, StandingRule};
use task_core::{
    Budget, DelegateTask, GenreSpec, ListFilter, ListOrder, Message, MessageId, MessageRole, OrgNode, ProjectId,
    RoleSpec, Status, Task, TaskId, TaskKind, TaskStore, Tier, Trigger, WorkerHint, WorkspaceSpec, conversation_title,
    department_of, failure_reply,
};
use time::OffsetDateTime;

use crate::error::OpsError;

/// 対話用 run の予算（ADR-0033 D4「budget は小さめ」）。返事 1 回ぶんなので短く切る。
/// 役割の既定（`[[roles]]`）より**こちらが勝つ**: 対話は「ひと言返す」仕事で、実装 run の予算とは別物。
/// Phase 28: 実機で `error_max_turns`（6 ターンで打ち切り）が起きたため 10 に上げた（読むだけでも数ターン
/// 使う。ADR-0033 D4 追記 / 実機の一本目、2026-09-17）。`max_wall_secs` は変えない。
pub const CONVERSATION_MAX_TURNS: u32 = 10;
pub const CONVERSATION_MAX_WALL_SECS: u64 = 300;
pub const CONVERSATION_MAX_RETRIES: u32 = 1;
/// 対話用タスクの優先度（人が画面の前で待っているので、通常のタスクより少しだけ前に出す）。
pub const CONVERSATION_PRIORITY: i32 = 1;
/// run の前置きに載せる直近のやり取りの既定件数（ADR-0033 D4）。
pub const CONVERSATION_HISTORY: usize = 20;
/// 直列化（Phase 27 の監査 M-3）のために見る「終端でないタスク」の件数の上限。
const OPEN_TASK_SCAN: usize = 500;

/// `start` の結果。API は 202 で `{message_id, task_id}` を返す。
#[derive(Debug, Clone, PartialEq)]
pub struct StartedConversation {
    pub message: Message,
    pub task: Task,
}

/// 人がノードに話しかける（ADR-0033 D4。Phase 30 で対話用分野の解決を変更）。`messages` に
/// `role = user` の行を入れ、そのノードの run を 1 回起こすための対話用タスク（`kind = execute`、
/// 受け入れ条件なし）を作って `ready` にする。
///
/// 道具立て（分野・役割・tier・アダプタ）は決定的に引く: **ノードの `genre` は使わない**（実機の事故:
/// 検索ハーネス genre のノードに話しかけると検索ハーネスが会話しようとして証拠ゲートで落ちた）。
/// 常に `conversation_genre`（設定 `[conversation] genre`、既定 `CONVERSATION_GENRE = "secretary"`）
/// → その分野の `default_role` → 役割の `tier` / `adapter`。予算は上の定数。
#[allow(clippy::too_many_arguments)]
pub fn start(
    store: &dyn TaskStore,
    node_id: &str,
    project_id: Option<ProjectId>,
    text: &str,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    conversation_genre: &str,
    now: OffsetDateTime,
) -> Result<StartedConversation, OpsError> {
    start_with_milestone(store, node_id, project_id, None, text, roles, genres, conversation_genre, now)
}

/// Phase 41（ADR-0038 D1）: `start` と同じ対話を、**途中目標に紐づけて**起こす（レビューの対話）。
/// `milestone_id` が入った対話用タスクは裏方の `support = "milestone_review"`（`task_core::support_kind`）
/// になり、仕事の木には出ない。人が話しかける経路（`start`）は常に `None` を渡す。
#[allow(clippy::too_many_arguments)]
pub fn start_with_milestone(
    store: &dyn TaskStore,
    node_id: &str,
    project_id: Option<ProjectId>,
    milestone_id: Option<task_core::MilestoneId>,
    text: &str,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    conversation_genre: &str,
    now: OffsetDateTime,
) -> Result<StartedConversation, OpsError> {
    if text.trim().is_empty() {
        return Err(OpsError::Validation("text must not be blank".to_string()));
    }
    let Some(node) = store.org_get(node_id)? else {
        return Err(OpsError::Validation(format!("{node_id:?} is not an org node")));
    };
    if let Some(project_id) = project_id
        && store.project_get(project_id)?.is_none()
    {
        return Err(OpsError::Validation(format!("project {project_id} does not exist")));
    }

    // 監査 M-3: 同じノード・同じ案件に未終了の対話タスクがあれば、その後ろに並べる（返事は送った順に返る）。
    let depends_on = open_conversation_tasks(store, &node.id, project_id)?;

    let mut task = conversation_task(&node, project_id, text, roles, genres, conversation_genre, now);
    task.depends_on = depends_on;
    task.milestone_id = milestone_id;
    let message = Message {
        id: MessageId::new(),
        node_id: node.id.clone(),
        project_id,
        role: MessageRole::User,
        text: text.to_string(),
        run_id: None,
        // R4（migration 0007）: 人の発言も返事も、同じ対話用タスクの id を持つ。
        task_id: Some(task.id),
        created_at: now,
    };
    store.message_append(&message)?;
    let task = Task {
        conversation: Some(message.id),
        ..task
    };
    store.create_task(&task, vec![])?;
    // 対話用タスクは人が承認するものではない（話しかけた時点が承認）。draft のままだと run が起きない。
    store.apply_transition(task.id, Trigger::Accept, None)?;
    let task = store.get(task.id)?.unwrap_or(task);
    Ok(StartedConversation { message, task })
}

/// 監査 M-3: そのノード・その案件の、まだ終端に達していない対話用タスク（古い順）。
/// 新しい対話タスクの `depends_on` に入れて、返事が送った順に返るようにする。
fn open_conversation_tasks(
    store: &dyn TaskStore,
    node_id: &str,
    project_id: Option<ProjectId>,
) -> Result<Vec<TaskId>, OpsError> {
    let filter = ListFilter {
        statuses: vec![
            Status::Draft,
            Status::Ready,
            Status::Running,
            Status::Blocked,
            Status::Reviewing,
        ],
        project_id,
        ..ListFilter::default()
    };
    let page = store.list_page(&filter, ListOrder::CreatedDesc, None, OPEN_TASK_SCAN)?;
    let mut open: Vec<&Task> = page
        .items
        .iter()
        .filter(|t| {
            task_core::is_conversation(t) && t.assignee.as_deref() == Some(node_id) && t.project_id == project_id
        })
        .collect();
    open.sort_by_key(|t| t.created_at);
    Ok(open.iter().map(|t| t.id).collect())
}

/// 対話用タスクを組み立てる（純粋。ストアは見ない）。`conversation`（人の発言 id）は呼び出し側が入れる。
fn conversation_task(
    node: &OrgNode,
    project_id: Option<ProjectId>,
    text: &str,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    conversation_genre: &str,
    now: OffsetDateTime,
) -> Task {
    // Phase 30: 対話はノードの `genre`（仕事のハーネス）に関係なく、常に対話用分野で走る
    // （実機の事故: 関連研究調査課＝検索ハーネスに話しかけたら検索ハーネスが会話しようとして落ちた）。
    let genre = GenreSpec::find(genres, conversation_genre);
    let role = genre
        .and_then(|g| g.default_role.as_deref())
        .and_then(|r| RoleSpec::find(roles, r));
    let id = TaskId::new();
    Task {
        repos: Vec::new(),
        id,
        parent_id: None,
        kind: TaskKind::Execute,
        title: conversation_title(text),
        objective: text.to_string(),
        // 受け入れ条件は無い（返事に合否は無い。レビューは素通りする）。
        acceptance: vec![],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Draft,
        priority: CONVERSATION_PRIORITY,
        worker_hint: WorkerHint {
            tier: role.and_then(|r| r.tier).unwrap_or(Tier::Standard),
            adapter: role.and_then(|r| r.adapter.clone()),
        },
        workspace: WorkspaceSpec::Local {
            path: PathBuf::from(id.to_string()), mode: None,
        },
        budget: Budget {
            max_turns: CONVERSATION_MAX_TURNS,
            max_wall_secs: CONVERSATION_MAX_WALL_SECS,
            max_retries: CONVERSATION_MAX_RETRIES,
        },
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
        role: role.map(|r| r.id.clone()),
        genre: genre.map(|g| g.id.clone()),
        aggregate: false,
        project_id,
        milestone_id: None,
        assignee: Some(node.id.clone()),
        // 対話由来の印（`tasks` の列は増やさない。`task_core::message` 参照）。呼び出し側が入れる。
        conversation: None,
        labels: Vec::new(),
        category: Default::default(),
    }
}

/// 対話用タスクの run が終わったときの返事（`role = node` の行）を追記する（ADR-0033 D4）。
/// `task` が対話用でなければ何もしない（`Ok(None)`）。`summary` が空なら run が何も言わなかったという
/// ことなので、その旨を残す（空行は入れない）。
pub fn record_reply(
    store: &dyn TaskStore,
    task: &Task,
    run_id: &str,
    text: &str,
    now: OffsetDateTime,
) -> Result<Option<Message>, OpsError> {
    if !task_core::is_conversation(task) {
        return Ok(None);
    }
    let Some(node_id) = task.assignee.clone() else {
        return Ok(None);
    };
    let text = if text.trim().is_empty() {
        failure_reply("返事の本文が空でした")
    } else {
        text.to_string()
    };
    let message = Message {
        id: MessageId::new(),
        node_id,
        project_id: task.project_id,
        role: MessageRole::Node,
        text,
        run_id: Some(run_id.to_string()),
        // R4（migration 0007）: 人の発言の行と同じ対話用タスクの id。
        task_id: Some(task.id),
        created_at: now,
    };
    store.message_append(&message)?;
    Ok(Some(message))
}

// ---- SPEC §3.1「部をまたぐ連携は秘書が認める」（ADR-0033 D4 最終項 + D5。Phase 27 で認可に接続）----

/// 部をまたぐ委譲の質問の先頭（`approvals.question` と `standing_rules.rule` の照合の鍵）。
/// Phase 27: 質問を人が読む自由文から**構造化した固定の形**に変えた。これが無いと、人が答えても
/// 次の run で同じ質問に戻る（Phase 24 の監査 H-1: 永久ループ）。
pub const CROSS_DEPARTMENT_PREFIX: &str = "cross-department: ";

/// 部をまたぐ委譲 1 件。`question` は `"cross-department: <from> -> <to>: <理由>"`、
/// 照合の鍵は `"cross-department: <from> -> <to>"`（前方一致で決定的に照合する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossDepartment {
    /// 委譲元のノード（`task.assignee`）。
    pub from: String,
    /// 委譲先のノード（提案の `assignee`）。
    pub to: String,
    /// 理由（提案の `title`）。
    pub title: String,
}

impl CrossDepartment {
    /// 認可を照合する鍵（`approvals.question` の先頭 / `standing_rules.rule` の先頭）。
    pub fn key(&self) -> String {
        format!("{CROSS_DEPARTMENT_PREFIX}{} -> {}", self.from, self.to)
    }

    /// `approvals.question` に入れる文面（鍵 + 理由）。
    pub fn question(&self) -> String {
        format!("{}: {}", self.key(), self.title)
    }
}

/// 質問文（`approvals.question`）が部をまたぐ委譲のものなら、その照合の鍵を返す。
/// `"cross-department: <from> -> <to>: <理由>"` の 3 つ目の `:` より前まで（ノードの id に `:` は使わない）。
pub fn cross_department_key(question: &str) -> Option<String> {
    let rest = question.strip_prefix(CROSS_DEPARTMENT_PREFIX)?;
    let pair = rest.split(':').next().unwrap_or(rest).trim_end();
    if pair.is_empty() {
        return None;
    }
    Some(format!("{CROSS_DEPARTMENT_PREFIX}{pair}"))
}

/// 1 件の部をまたぐ委譲について、人がもう答えているか（決定的。文字列の前方一致だけで決める）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossAuthorization {
    /// 通してよい（`once` / `standing` の決定、または `standing_rules` の規則がある）。
    Allowed,
    /// まだ聞いていない（秘書の認可待ち）。
    Pending,
    /// 人が認めなかった（`denied`）。
    Denied,
}

/// 認可の照合（純粋関数）。`rules` は委譲元宛て + 全員向け、`approvals` は委譲元のノード宛ての全件。
pub fn cross_authorization(
    key: &str,
    task_id: TaskId,
    approvals: &[Approval],
    rules: &[StandingRule],
) -> CrossAuthorization {
    // 「今後ずっと」は案件・タスクに依らず効く（SPEC §3.6）。
    if rules.iter().any(|r| r.rule.starts_with(key)) {
        return CrossAuthorization::Allowed;
    }
    // 同じタスクの、同じ鍵の決定のうち最も新しいもの（人が答え直せるので後勝ち）。
    let decided = approvals
        .iter()
        .filter(|a| a.task_id == Some(task_id) && a.question.starts_with(key))
        .filter_map(|a| a.decision.map(|d| (a.decided_at, a.created_at, d)))
        .max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)))
        .map(|(_, _, d)| d);
    match decided {
        Some(Decision::Once) | Some(Decision::Standing) => CrossAuthorization::Allowed,
        Some(Decision::Denied) => CrossAuthorization::Denied,
        None => CrossAuthorization::Pending,
    }
}

/// 委譲の提案を「そのまま子にしてよいもの」と「秘書の認可待ち」「人が認めなかったもの」に分ける。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegationSplit {
    /// 子を作ってよい提案（同じ部宛て、または認可済みの部またぎ）。
    pub allowed: Vec<DelegateTask>,
    /// 秘書に聞く部またぎ（子は作らない）。
    pub pending: Vec<CrossDepartment>,
    /// 人が認めなかった部またぎ（子は作らないが、もう聞かない）。
    pub denied: Vec<CrossDepartment>,
}

/// SPEC §3.1 / ADR-0033 D4・D5: 委譲の提案を認可の状態で振り分ける（Phase 27 の監査 H-1 / H-2 対応）。
///
/// - **バッチは分ける**: 同じ部宛ての提案はその場で子にし、部またぎの提案だけを質問にする
///   （Phase 24 は 1 件でも部またぎがあると全件を止めていた）。
/// - 委譲元に担当が無い / 担当が秘書（部に属さない）なら、この規則は効かない（誰にでも振れる）。
/// - 提案に `assignee` が無い、または組織に無い id なら、この規則は効かない（従来の委譲のまま）。
///
/// 読むのは `org_nodes` / `approvals` / `standing_rules` だけで、LLM は呼ばない（DESIGN 原則 1）。
pub fn split_delegation(
    store: &dyn TaskStore,
    org: &[OrgNode],
    parent: &Task,
    proposals: &[DelegateTask],
) -> Result<DelegationSplit, OpsError> {
    let mut split = DelegationSplit::default();
    let (Some(from), Some(from_dept)) = (
        parent.assignee.as_deref(),
        parent
            .assignee
            .as_deref()
            .and_then(|from| department_of(org, from)),
    ) else {
        split.allowed = proposals.to_vec();
        return Ok(split);
    };
    // 「全員向け + この委譲元向け」の規則と、この委譲元が聞いた認可の全件（決定を含む）。
    let rules = store.standing_rule_list(Some(from))?;
    let approvals = store.approval_list(None, None, Some(from))?;
    for proposal in proposals {
        let crossing = match proposal.assignee.as_deref() {
            Some(to) => match department_of(org, to) {
                Some(to_dept) if to_dept != from_dept => CrossDepartment {
                    from: from.to_string(),
                    to: to.to_string(),
                    title: proposal.title.clone(),
                },
                _ => {
                    split.allowed.push(proposal.clone());
                    continue;
                }
            },
            None => {
                split.allowed.push(proposal.clone());
                continue;
            }
        };
        match cross_authorization(&crossing.key(), parent.id, &approvals, &rules) {
            CrossAuthorization::Allowed => split.allowed.push(proposal.clone()),
            CrossAuthorization::Pending => {
                if !split.pending.contains(&crossing) {
                    split.pending.push(crossing);
                }
            }
            CrossAuthorization::Denied => {
                if !split.denied.contains(&crossing) {
                    split.denied.push(crossing);
                }
            }
        }
    }
    Ok(split)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{CONVERSATION_GENRE, Check, Criterion, DelegateTask, OrgKind, Project, ProjectStatus, SqliteStore};

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn node(id: &str, parent: Option<&str>, kind: OrgKind, genre: Option<&str>) -> OrgNode {
        let t = now();
        OrgNode {
            id: id.into(),
            parent_id: parent.map(str::to_string),
            name: format!("{id} さん"),
            kind,
            genre: genre.map(str::to_string),
            brief: format!("{id} の担当"),
            position: 0,
            created_at: t,
            updated_at: t,
        }
    }

    fn seed_org(store: &SqliteStore) {
        for n in [
            node("secretary", None, OrgKind::Secretary, Some("secretary")),
            node("research", Some("secretary"), OrgKind::Department, None),
            node("research-survey", Some("research"), OrgKind::Section, Some("literature")),
            node("research-data", Some("research"), OrgKind::Section, None),
            node("coding", Some("secretary"), OrgKind::Department, None),
            node("coding-poc", Some("coding"), OrgKind::Section, Some("coding")),
        ] {
            store.org_upsert(&n).expect("org upsert");
        }
    }

    fn specs() -> (Vec<RoleSpec>, Vec<GenreSpec>) {
        let roles = vec![
            RoleSpec {
                id: "secretary".into(),
                tier: Some(Tier::Standard),
                adapter: Some("claude-code".into()),
                max_turns: Some(40),
                ..RoleSpec::default()
            },
            RoleSpec {
                id: "literature-reader".into(),
                tier: Some(Tier::Cheap),
                adapter: Some("paperqa".into()),
                ..RoleSpec::default()
            },
        ];
        let genres = vec![
            GenreSpec {
                id: "secretary".into(),
                description: "人と話す".into(),
                default_role: Some("secretary".into()),
                roles: vec!["secretary".into()],
                ..GenreSpec::default()
            },
            GenreSpec {
                id: "literature".into(),
                description: "関連研究の調査".into(),
                default_role: Some("literature-reader".into()),
                roles: vec!["literature-reader".into()],
                ..GenreSpec::default()
            },
        ];
        (roles, genres)
    }

    #[test]
    fn talking_to_a_node_records_the_message_and_makes_one_ready_conversation_task() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let (roles, genres) = specs();
        let project = Project {
            id: ProjectId::new(),
            title: "Pluvio".into(),
            request: "新テーマの模索".into(),
            status: ProjectStatus::Proposed,
            secretary_summary: None,
            workspace: None,
            created_at: now(),
            updated_at: now(),
        };
        store.project_create(&project).expect("project");

        let started = start(
            &store,
            "secretary",
            Some(project.id),
            "この案件をお願いします",
            &roles,
            &genres,
            CONVERSATION_GENRE,
            now(),
        )
        .expect("start");

        assert_eq!(started.message.role, MessageRole::User);
        assert_eq!(store.message_list("secretary", Some(project.id), 20).expect("list").len(), 1);

        let task = store.get(started.task.id).expect("get").expect("task");
        assert_eq!(task.status, Status::Ready, "話しかけた時点で run できる");
        assert_eq!(task.kind, TaskKind::Execute);
        assert_eq!(task.title, "対話: この案件をお願いします");
        assert_eq!(task.objective, "この案件をお願いします");
        assert!(task.acceptance.is_empty());
        assert_eq!(task.assignee.as_deref(), Some("secretary"));
        assert_eq!(task.project_id, Some(project.id));
        assert_eq!(task.genre.as_deref(), Some("secretary"));
        assert_eq!(task.role.as_deref(), Some("secretary"));
        assert_eq!(task.worker_hint.adapter.as_deref(), Some("claude-code"));
        assert_eq!(task.budget.max_turns, CONVERSATION_MAX_TURNS, "役割の 40 ではなく対話用の予算");
        assert_eq!(task_core::conversation_origin(&task), Some(started.message.id));
    }

    /// Phase 30（ADR-0033 D4 追記）: 対話はノードの `genre`（仕事のハーネス）に関係なく、常に対話用分野
    /// （`[conversation] genre`、既定 `secretary`）で走る。実機の事故: 関連研究調査課
    /// （`genre = web-research` = Local Deep Research）に「なぜ web search に失敗しているのでしょうか？」
    /// と話しかけたら、検索ハーネスが会話しようとして検索し、0 件 → 証拠ゲートで `failed` になった。
    /// 検索ハーネスや PaperQA は会話ができない。
    #[test]
    fn conversation_always_uses_the_conversation_genre_regardless_of_the_nodes_own_genre() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let (roles, genres) = specs();

        // 分野を持たないノード（従来どおり対話用分野）。
        let started = start(&store, "research-data", None, "図表の体裁を相談したい", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start");
        assert_eq!(started.task.genre.as_deref(), Some(CONVERSATION_GENRE));
        assert_eq!(started.task.role.as_deref(), Some("secretary"));
        assert_eq!(started.task.project_id, None);

        // 分野を持つノード（`research-survey` = 関連研究調査課、`genre = literature`）に話しかけても、
        // その分野（実機では web-research = LDR）ではなく、常に対話用分野・役割・adapter で run する。
        let survey = start(&store, "research-survey", None, "先行研究の当て方", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start");
        assert_eq!(survey.task.genre.as_deref(), Some(CONVERSATION_GENRE), "ノードの genre ではなく対話用分野");
        assert_eq!(survey.task.role.as_deref(), Some("secretary"));
        assert_eq!(survey.task.worker_hint.adapter.as_deref(), Some("claude-code"), "paperqa ではなく対話用の adapter");
    }

    /// 完了条件: `research-survey`（`genre = web-research`）への対話タスクが `secretary` 分野の役割・adapter
    /// で作られる（LDR ではない）。前置きにそのノードの `brief` と「仕事で使う道具」が入ることは
    /// `task-worker::preamble` 側で検証する（ここは分野解決の決定的なロジックだけを見る）。
    #[test]
    fn a_web_research_node_still_talks_through_the_conversation_genre() {
        let store = SqliteStore::open_in_memory().expect("open");
        let t = now();
        store
            .org_upsert(&OrgNode {
                id: "secretary".into(),
                parent_id: None,
                name: "秘書".into(),
                kind: OrgKind::Secretary,
                genre: Some("secretary".into()),
                brief: "秘書".into(),
                position: 0,
                created_at: t,
                updated_at: t,
            })
            .expect("org upsert");
        store
            .org_upsert(&OrgNode {
                id: "research-survey".into(),
                parent_id: Some("secretary".into()),
                name: "関連研究調査課".into(),
                kind: OrgKind::Section,
                genre: Some("web-research".into()),
                brief: "関連研究を洗う。".into(),
                position: 0,
                created_at: t,
                updated_at: t,
            })
            .expect("org upsert");
        let (roles, mut genres) = specs();
        genres.push(GenreSpec {
            id: "web-research".into(),
            description: "web 検索で先行研究を洗う".into(),
            capabilities: vec!["web 検索".into()],
            default_role: None,
            roles: vec![],
            ..GenreSpec::default()
        });

        let started = start(
            &store,
            "research-survey",
            None,
            "なぜ web search に失敗しているのでしょうか？",
            &roles,
            &genres,
            CONVERSATION_GENRE,
            now(),
        )
        .expect("start");
        assert_eq!(started.task.genre.as_deref(), Some("secretary"), "web-research（LDR）ではない");
        assert_eq!(started.task.role.as_deref(), Some("secretary"));
        assert_eq!(started.task.worker_hint.adapter.as_deref(), Some("claude-code"));
    }

    #[test]
    fn unknown_nodes_projects_and_blank_text_are_rejected_without_writing_anything() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let (roles, genres) = specs();
        assert!(matches!(
            start(&store, "ghost", None, "hello", &roles, &genres, CONVERSATION_GENRE, now()),
            Err(OpsError::Validation(_))
        ));
        assert!(matches!(
            start(&store, "secretary", Some(ProjectId::new()), "hello", &roles, &genres, CONVERSATION_GENRE, now()),
            Err(OpsError::Validation(_))
        ));
        assert!(matches!(
            start(&store, "secretary", None, "   ", &roles, &genres, CONVERSATION_GENRE, now()),
            Err(OpsError::Validation(_))
        ));
        assert!(store.message_list("secretary", None, 20).expect("list").is_empty());
        assert!(store.message_list("ghost", None, 20).expect("list").is_empty());
    }

    #[test]
    fn the_reply_is_recorded_with_the_run_id_and_only_for_conversation_tasks() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let (roles, genres) = specs();
        let started = start(&store, "secretary", None, "状況を教えて", &roles, &genres, CONVERSATION_GENRE, now()).expect("start");

        let reply = record_reply(&store, &started.task, "run-1", "順調です", now())
            .expect("record")
            .expect("some");
        assert_eq!(reply.role, MessageRole::Node);
        assert_eq!(reply.run_id.as_deref(), Some("run-1"));
        let thread = store.message_list("secretary", None, 20).expect("list");
        assert_eq!(thread.len(), 2);
        assert_eq!(thread[1].text, "順調です");

        // 空の summary は「返事できませんでした」に寄せる（空行は残さない）。
        record_reply(&store, &started.task, "run-2", "  ", now()).expect("record");
        assert!(store.message_list("secretary", None, 20).expect("list")[2].text.starts_with("返事できませんでした"));

        // 対話由来でないタスクは何も書かない。
        let mut plain = started.task.clone();
        plain.conversation = None;
        assert_eq!(record_reply(&store, &plain, "run-3", "x", now()).expect("record"), None);
        assert_eq!(store.message_list("secretary", None, 20).expect("list").len(), 3);
    }

    fn proposal(assignee: Option<&str>, title: &str) -> DelegateTask {
        DelegateTask {
            title: title.into(),
            objective: "o".into(),
            acceptance: vec![Criterion { text: "c".into(), check: Check::Human }],
            role: None,
            genre: None,
            depends_on: vec![],
            tier: None,
            assignee: assignee.map(str::to_string),
            workspace: None,
        }
    }

    /// ADR-0033 D4 / SPEC §3.1: 部をまたぐ委譲は秘書に聞く。同じ部の中なら聞かない。
    /// Phase 27（監査 H-1 / H-2）: **バッチは分ける**（同じ部宛ての提案はその場で子にする）。
    #[test]
    fn a_delegation_to_another_department_becomes_a_question_and_the_batch_is_split() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let org = store.org_list().expect("org");
        let (roles, genres) = specs();
        let mut parent = start(&store, "research-survey", None, "調べて", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start")
            .task;

        // 同じ部（研究部）の課へ: 聞かない。
        let split = split_delegation(&store, &org, &parent, &[proposal(Some("research-data"), "整理")])
            .expect("split");
        assert_eq!(split.allowed.len(), 1);
        assert!(split.pending.is_empty() && split.denied.is_empty());
        // 担当なしの提案: 従来どおり（聞かない）。
        let split = split_delegation(&store, &org, &parent, &[proposal(None, "t")]).expect("split");
        assert_eq!(split.allowed.len(), 1);
        assert!(split.pending.is_empty());

        // 部またぎと同じ部宛てが混ざったバッチ: 同じ部宛てだけ子になり、部またぎは認可待ちになる。
        let split = split_delegation(
            &store,
            &org,
            &parent,
            &[proposal(Some("research-data"), "整理"), proposal(Some("coding-poc"), "PoC を書く")],
        )
        .expect("split");
        assert_eq!(split.allowed.len(), 1, "同じ部宛ては止めない: {split:?}");
        assert_eq!(split.allowed[0].assignee.as_deref(), Some("research-data"));
        assert_eq!(split.pending.len(), 1);
        assert_eq!(
            split.pending[0],
            CrossDepartment {
                from: "research-survey".into(),
                to: "coding-poc".into(),
                title: "PoC を書く".into()
            }
        );
        assert_eq!(split.pending[0].key(), "cross-department: research-survey -> coding-poc");
        assert_eq!(
            split.pending[0].question(),
            "cross-department: research-survey -> coding-poc: PoC を書く"
        );
        assert_eq!(
            cross_department_key(&split.pending[0].question()).as_deref(),
            Some(split.pending[0].key().as_str())
        );
        assert_eq!(cross_department_key("どのクラスタを使いますか"), None);

        // 委譲元が秘書（部に属さない）なら誰にでも振れる。
        parent.assignee = Some("secretary".into());
        let split = split_delegation(&store, &org, &parent, &[proposal(Some("coding-poc"), "t")]).expect("split");
        assert_eq!(split.allowed.len(), 1);
        // 担当を持たないタスクも従来どおり。
        parent.assignee = None;
        let split = split_delegation(&store, &org, &parent, &[proposal(Some("coding-poc"), "t")]).expect("split");
        assert_eq!(split.allowed.len(), 1);
    }

    /// Phase 27（監査 H-1）: 人が答えたら次の run で同じ委譲が通る。`once` はそのタスクだけ、
    /// `standing` は以後ずっと、`denied` は通さない。照合は鍵の前方一致で決定的。
    #[test]
    fn once_standing_and_denied_decide_whether_the_next_run_may_delegate_across_departments() {
        use task_core::approval::{Approval, ApprovalId, ApprovalStore, Decision, StandingRule, StandingRuleId};

        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let org = store.org_list().expect("org");
        let (roles, genres) = specs();
        let parent = start(&store, "research-survey", None, "調べて", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start")
            .task;
        let proposals = [proposal(Some("coding-poc"), "PoC を書く")];
        let crossing = CrossDepartment {
            from: "research-survey".into(),
            to: "coding-poc".into(),
            title: "PoC を書く".into(),
        };

        // まだ聞いていない: 認可待ち。
        assert_eq!(
            split_delegation(&store, &org, &parent, &proposals).expect("split").pending,
            vec![crossing.clone()]
        );

        // `once`: 同じタスクの次の run では通る。
        let approval = Approval {
            id: ApprovalId::new(),
            project_id: None,
            node_id: "research-survey".into(),
            task_id: Some(parent.id),
            question: crossing.question(),
            decision: None,
            answer: None,
            created_at: now(),
            decided_at: None,
        };
        store.approval_append(&approval).expect("append");
        store
            .approval_decide(approval.id, Decision::Once, Some("認める".into()), now())
            .expect("decide");
        let split = split_delegation(&store, &org, &parent, &proposals).expect("split");
        assert_eq!(split.allowed.len(), 1, "once: 子を作る");
        assert!(split.pending.is_empty());

        // 別のタスクの同じ委譲は、`once` では通らない（今回だけ）。
        let other = start(&store, "research-survey", None, "別の件", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start")
            .task;
        assert_eq!(
            split_delegation(&store, &org, &other, &proposals).expect("split").pending,
            vec![crossing.clone()]
        );

        // `standing`: 鍵をそのまま規則にすると、以後どのタスクでも通る。
        store
            .standing_rule_append(&StandingRule {
                id: StandingRuleId::new(),
                node_id: Some("research-survey".into()),
                rule: crossing.key(),
                created_at: now(),
            })
            .expect("rule");
        let split = split_delegation(&store, &org, &other, &proposals).expect("split");
        assert_eq!(split.allowed.len(), 1, "standing: 以後ずっと通る");

        // `denied`: 子を作らず、もう聞かない（`answers[]` の「認めない」がワーカーに見える）。
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let parent = start(&store, "research-survey", None, "調べて", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start")
            .task;
        let approval = Approval {
            id: ApprovalId::new(),
            project_id: None,
            node_id: "research-survey".into(),
            task_id: Some(parent.id),
            question: crossing.question(),
            decision: None,
            answer: None,
            created_at: now(),
            decided_at: None,
        };
        store.approval_append(&approval).expect("append");
        store
            .approval_decide(approval.id, Decision::Denied, Some("認めない".into()), now())
            .expect("decide");
        let split = split_delegation(&store, &org, &parent, &proposals).expect("split");
        assert!(split.allowed.is_empty() && split.pending.is_empty());
        assert_eq!(split.denied, vec![crossing]);
    }

    /// 監査 M-3: 同じノード・同じ案件の対話は直列化する（2 通目は 1 通目が終わるまで `ready` にならない）。
    #[test]
    fn a_second_message_waits_for_the_first_reply() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let (roles, genres) = specs();
        let project = Project {
            id: ProjectId::new(),
            title: "Pluvio".into(),
            request: "r".into(),
            status: ProjectStatus::Proposed,
            secretary_summary: None,
            workspace: None,
            created_at: now(),
            updated_at: now(),
        };
        store.project_create(&project).expect("project");

        let first = start(&store, "secretary", Some(project.id), "1 通目", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start")
            .task;
        let second = start(&store, "secretary", Some(project.id), "2 通目", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start")
            .task;
        assert_eq!(second.depends_on, vec![first.id]);
        let ready: Vec<TaskId> = store.ready_tasks(10).expect("ready").iter().map(|t| t.id).collect();
        assert_eq!(ready, vec![first.id], "2 通目はまだ run できない");

        // 別のノード宛て・別の案件（案件なし）は別の列（待たない）。
        let other_node = start(&store, "research-survey", Some(project.id), "別の人へ", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start")
            .task;
        assert!(other_node.depends_on.is_empty());
        let no_project = start(&store, "secretary", None, "雑談", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start")
            .task;
        assert!(no_project.depends_on.is_empty());

        // 1 通目が終われば 2 通目が run できる。
        store
            .acquire_lease(first.id, "run-1", std::time::Duration::from_secs(60))
            .expect("lease");
        store.apply_transition(first.id, Trigger::WorkerDone, None).expect("done");
        store.apply_transition(first.id, Trigger::ReviewPass, None).expect("pass");
        let ready: Vec<TaskId> = store.ready_tasks(10).expect("ready").iter().map(|t| t.id).collect();
        assert!(ready.contains(&second.id), "1 通目が done なら 2 通目が ready: {ready:?}");
    }

    /// P-78（ADR-0033 D4 / Phase 28）: 対話タスクの `depends_on` は返事を送った順に返すための直列化だけが
    /// 目的で、前の対話タスクの成否には意味が無い。前が `failed` に落ちても、次の対話タスクは
    /// `cancelled`（`DependencyFailed`）にならず、`ready` になる。
    #[test]
    fn a_failed_conversation_task_does_not_cancel_the_next_one() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let (roles, genres) = specs();

        let first = start(&store, "secretary", None, "1 通目", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start")
            .task;
        let second = start(&store, "secretary", None, "2 通目", &roles, &genres, CONVERSATION_GENRE, now())
            .expect("start")
            .task;
        assert_eq!(second.depends_on, vec![first.id]);

        // 1 通目が失敗（非 retryable なので即 `Failed`）しても、2 通目は `cancelled` に連鎖しない。
        store
            .acquire_lease(first.id, "run-1", std::time::Duration::from_secs(60))
            .expect("lease");
        store
            .apply_transition(first.id, Trigger::WorkerError { retryable: false }, None)
            .expect("fail");
        assert_eq!(store.get(first.id).expect("get").expect("some").status, Status::Failed);
        assert_eq!(
            store.get(second.id).expect("get").expect("some").status,
            Status::Ready,
            "対話タスクは DependencyFailed の対象から外れる"
        );
        let ready: Vec<TaskId> = store.ready_tasks(10).expect("ready").iter().map(|t| t.id).collect();
        assert!(ready.contains(&second.id), "前が failed でも次は ready になる: {ready:?}");
    }

    /// R4（migration 0007）: 1 往復の両方の行に、それを起こした対話用タスクの id が入る。
    #[test]
    fn both_sides_of_one_exchange_carry_the_conversation_task_id() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let (roles, genres) = specs();
        let started = start(&store, "secretary", None, "状況を教えて", &roles, &genres, CONVERSATION_GENRE, now()).expect("start");
        assert_eq!(started.message.task_id, Some(started.task.id));
        record_reply(&store, &started.task, "run-1", "順調です", now()).expect("record");
        let thread = store.message_list("secretary", None, 20).expect("list");
        assert!(thread.iter().all(|m| m.task_id == Some(started.task.id)), "{thread:?}");
    }
}
