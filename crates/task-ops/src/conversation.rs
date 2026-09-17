//! 対話（ADR-0033 D4 / Phase 24）。人が組織のノードに話しかけ、そのノードが返事をする経路の
//! 「判断と検証」を 1 か所にまとめる（`task-api` と `task-dispatch` の両方から呼ぶ）。
//!
//! **新しいプロトコルは作らない**: 話しかけると `messages` に `role = user` の行が 1 つ増え、
//! 既存の `tasks` に `kind = execute` の対話用タスクが 1 件できるだけ。返事はその run の
//! `artifacts/result.json` の `summary`（ADR-0006 D3）で、ディスパッチャが `role = node` の行にする。
//!
//! I/O は `TaskStore` の読み書きだけ。LLM 呼び出しは無い（DESIGN 原則 1）。

use std::path::PathBuf;

use task_core::{
    Budget, CONVERSATION_GENRE, GenreSpec, Message, MessageId, MessageRole, OrgNode, ProjectId, RoleSpec, Status,
    Task, TaskId, TaskKind, TaskStore, Tier, Trigger, WorkerHint, WorkspaceSpec, conversation_title, department_of,
    failure_reply,
};
use time::OffsetDateTime;

use crate::error::OpsError;

/// 対話用 run の予算（ADR-0033 D4「budget は小さめ」）。返事 1 回ぶんなので短く切る。
/// 役割の既定（`[[roles]]`）より**こちらが勝つ**: 対話は「ひと言返す」仕事で、実装 run の予算とは別物。
pub const CONVERSATION_MAX_TURNS: u32 = 6;
pub const CONVERSATION_MAX_WALL_SECS: u64 = 300;
pub const CONVERSATION_MAX_RETRIES: u32 = 1;
/// 対話用タスクの優先度（人が画面の前で待っているので、通常のタスクより少しだけ前に出す）。
pub const CONVERSATION_PRIORITY: i32 = 1;
/// run の前置きに載せる直近のやり取りの既定件数（ADR-0033 D4）。
pub const CONVERSATION_HISTORY: usize = 20;

/// `start` の結果。API は 202 で `{message_id, task_id}` を返す。
#[derive(Debug, Clone, PartialEq)]
pub struct StartedConversation {
    pub message: Message,
    pub task: Task,
}

/// 人がノードに話しかける（ADR-0033 D4）。`messages` に `role = user` の行を入れ、そのノードの run を
/// 1 回起こすための対話用タスク（`kind = execute`、受け入れ条件なし）を作って `ready` にする。
///
/// 道具立て（分野・役割・tier・アダプタ）は決定的に引く: ノードの `genre` → 無ければ**対話用分野**
/// （`CONVERSATION_GENRE`）→ その分野の `default_role` → 役割の `tier` / `adapter`。予算は上の定数。
pub fn start(
    store: &dyn TaskStore,
    node_id: &str,
    project_id: Option<ProjectId>,
    text: &str,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
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

    let message = Message {
        id: MessageId::new(),
        node_id: node.id.clone(),
        project_id,
        role: MessageRole::User,
        text: text.to_string(),
        run_id: None,
        created_at: now,
    };
    store.message_append(&message)?;

    let task = conversation_task(&node, &message, roles, genres, now);
    store.create_task(&task, vec![])?;
    // 対話用タスクは人が承認するものではない（話しかけた時点が承認）。draft のままだと run が起きない。
    store.apply_transition(task.id, Trigger::Accept, None)?;
    let task = store.get(task.id)?.unwrap_or(task);
    Ok(StartedConversation { message, task })
}

/// 対話用タスクを組み立てる（純粋。ストアは見ない）。
fn conversation_task(
    node: &OrgNode,
    message: &Message,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    now: OffsetDateTime,
) -> Task {
    // ADR-0033 D4: 分野を持たないノード（部・執筆課など）は秘書と同じ対話用分野で run する。
    let genre_id = node.genre.clone().unwrap_or_else(|| CONVERSATION_GENRE.to_string());
    let genre = GenreSpec::find(genres, &genre_id);
    let role = genre
        .and_then(|g| g.default_role.as_deref())
        .and_then(|r| RoleSpec::find(roles, r));
    let id = TaskId::new();
    Task {
        id,
        parent_id: None,
        kind: TaskKind::Execute,
        title: conversation_title(&message.text),
        objective: message.text.clone(),
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
            path: PathBuf::from(id.to_string()),
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
        project_id: message.project_id,
        milestone_id: None,
        assignee: Some(node.id.clone()),
        // 対話由来の印（`json` 列の中だけ。DB の列は増やさない。`task_core::message` 参照）。
        conversation: Some(message.id),
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
        created_at: now,
    };
    store.message_append(&message)?;
    Ok(Some(message))
}

/// SPEC §3.1「部をまたぐ連携は秘書が認める」（ADR-0033 D4 最終項）。委譲の提案のうち、委譲元とは
/// **別の部**のノードを `assignee` にしているものがあれば、子を作らずに人（秘書）へ聞くための質問文を返す。
///
/// 判定は決定的で、組織図の `department` の祖先だけを見る:
/// - 委譲元に担当が無い / 担当が秘書（部に属さない）なら、この規則は効かない（誰にでも振れる）。
/// - 提案に `assignee` が無い、または組織に無い id なら、この規則は効かない（従来の委譲のまま）。
pub fn cross_department_question(
    org: &[OrgNode],
    parent: &Task,
    proposals: &[task_core::DelegateTask],
) -> Option<String> {
    let from = parent.assignee.as_deref()?;
    let from_dept = department_of(org, from)?;
    let name = |id: &str| {
        org.iter()
            .find(|n| n.id == id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    let mut crossings: Vec<String> = Vec::new();
    for proposal in proposals {
        let Some(to) = proposal.assignee.as_deref() else {
            continue;
        };
        let Some(to_dept) = department_of(org, to) else {
            continue;
        };
        if to_dept == from_dept {
            continue;
        }
        let line = format!("{} から {}（{}）へ委譲したい。認めるか", name(from), name(to), name(&to_dept));
        if !crossings.contains(&line) {
            crossings.push(line);
        }
    }
    if crossings.is_empty() {
        return None;
    }
    Some(format!(
        "部をまたぐ委譲は秘書の認可が要ります（SPEC §3.1）。\n{}\n認めるなら `taskctl answer {} \"認める\"` のように答えてください。",
        crossings.join("\n"),
        parent.id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Criterion, Check, DelegateTask, OrgKind, Project, ProjectStatus, SqliteStore};

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

    /// ADR-0033 D4: 分野を持たないノードは秘書と同じ対話用分野で run する（決定的なフォールバック）。
    #[test]
    fn a_node_without_a_genre_falls_back_to_the_conversation_genre() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let (roles, genres) = specs();
        let started = start(&store, "research-data", None, "図表の体裁を相談したい", &roles, &genres, now())
            .expect("start");
        assert_eq!(started.task.genre.as_deref(), Some(CONVERSATION_GENRE));
        assert_eq!(started.task.role.as_deref(), Some("secretary"));
        assert_eq!(started.task.project_id, None);
        // 分野を持つノードは自分の分野で run する。
        let survey = start(&store, "research-survey", None, "先行研究の当て方", &roles, &genres, now())
            .expect("start");
        assert_eq!(survey.task.genre.as_deref(), Some("literature"));
        assert_eq!(survey.task.worker_hint.adapter.as_deref(), Some("paperqa"));
    }

    #[test]
    fn unknown_nodes_projects_and_blank_text_are_rejected_without_writing_anything() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let (roles, genres) = specs();
        assert!(matches!(
            start(&store, "ghost", None, "hello", &roles, &genres, now()),
            Err(OpsError::Validation(_))
        ));
        assert!(matches!(
            start(&store, "secretary", Some(ProjectId::new()), "hello", &roles, &genres, now()),
            Err(OpsError::Validation(_))
        ));
        assert!(matches!(
            start(&store, "secretary", None, "   ", &roles, &genres, now()),
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
        let started = start(&store, "secretary", None, "状況を教えて", &roles, &genres, now()).expect("start");

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

    /// ADR-0033 D4 / SPEC §3.1: 部をまたぐ委譲は秘書に聞く。同じ部の中なら聞かない。
    #[test]
    fn a_delegation_to_another_department_becomes_a_question() {
        let store = SqliteStore::open_in_memory().expect("open");
        seed_org(&store);
        let org = store.org_list().expect("org");
        let (roles, genres) = specs();
        let mut parent = start(&store, "research-survey", None, "調べて", &roles, &genres, now())
            .expect("start")
            .task;

        let proposal = |assignee: Option<&str>| DelegateTask {
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion { text: "c".into(), check: Check::Human }],
            role: None,
            genre: None,
            depends_on: vec![],
            tier: None,
            assignee: assignee.map(str::to_string),
        };

        // 同じ部（研究部）の課へ: 聞かない。
        assert_eq!(cross_department_question(&org, &parent, &[proposal(Some("research-data"))]), None);
        // 担当なしの提案: 従来どおり（聞かない）。
        assert_eq!(cross_department_question(&org, &parent, &[proposal(None)]), None);
        // 別の部（コーディング部）の課へ: 聞く。
        let q = cross_department_question(&org, &parent, &[proposal(Some("coding-poc"))]).expect("question");
        assert!(q.contains("research-survey さん"), "{q}");
        assert!(q.contains("coding-poc さん"), "{q}");
        assert!(q.contains("認めるか"), "{q}");
        // 委譲元が秘書（部に属さない）なら誰にでも振れる。
        parent.assignee = Some("secretary".into());
        assert_eq!(cross_department_question(&org, &parent, &[proposal(Some("coding-poc"))]), None);
        // 担当を持たないタスクも従来どおり。
        parent.assignee = None;
        assert_eq!(cross_department_question(&org, &parent, &[proposal(Some("coding-poc"))]), None);
    }
}
