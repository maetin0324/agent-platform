//! ADR-0048 D3（Phase 60b）: CoS の結果ファイルが宣言した `actions` を**決定的に**実行する。
//!
//! ここは `task_worker::ConsoleAction`（宣言の形。読むだけ）を受け取り、検証して実行するだけで、
//! LLM は使わない（DESIGN 原則 1。ADR-0034 D7 / ADR-0038 D1 の `milestone_proposal` と同じ流儀）。
//! 検証に落ちた action は実行せず、理由を残す。呼び出し側（`task-dispatch`）は run ごとに 1 回だけ
//! これを呼ぶ（`TaskStore::console_action_run_claim` で冪等性を取る。同じ `run_id` の 2 回目は `Ok(None)`）。

use task_core::{
    ConsoleAction, Milestone, MilestoneId, MilestoneStatus, OrgNode, Project, ProjectId,
    ProjectRepo, ProjectStatus, RepoId, RepoRun, Status, Task, TaskId, TaskStore, WorkspaceSpec,
};
use time::OffsetDateTime;

use crate::add::{CriterionSpec, NewTaskSpec};
use crate::error::OpsError;

/// 実行できた action 1 件（`Message.metadata.actions_executed` に写す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutedAction {
    pub kind: &'static str,
    /// 人が読む 1 行（「→ タスクを作りました: …」）。
    pub summary: String,
    pub task_id: Option<TaskId>,
    pub project_id: Option<ProjectId>,
    pub milestone_id: Option<MilestoneId>,
}

/// 検証に落ちて実行しなかった action 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedAction {
    pub kind: String,
    pub reason: String,
}

/// `execute` の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionsOutcome {
    pub executed: Vec<ExecutedAction>,
    pub failed: Vec<FailedAction>,
}

impl ActionsOutcome {
    pub fn is_empty(&self) -> bool {
        self.executed.is_empty() && self.failed.is_empty()
    }

    /// 返事の本文に足す「実行できなかった action」の節（無ければ `None`）。
    pub fn failure_note(&self) -> Option<String> {
        if self.failed.is_empty() {
            return None;
        }
        let lines: Vec<String> = self
            .failed
            .iter()
            .map(|f| format!("- {}: {}", f.kind, f.reason))
            .collect();
        Some(format!(
            "\n\n実行できなかった action:\n{}",
            lines.join("\n")
        ))
    }

    /// `Message.metadata`（実行結果を伴う返事にだけ `Some`）。
    pub fn to_metadata(&self) -> Option<task_core::MessageMetadata> {
        if self.is_empty() {
            return None;
        }
        Some(task_core::MessageMetadata {
            actions_executed: self
                .executed
                .iter()
                .map(|e| task_core::MessageActionResult {
                    kind: e.kind.to_string(),
                    summary: e.summary.clone(),
                    task_id: e.task_id,
                    project_id: e.project_id,
                    milestone_id: e.milestone_id,
                })
                .collect(),
            actions_failed: self
                .failed
                .iter()
                .map(|f| task_core::MessageActionFailure {
                    kind: f.kind.clone(),
                    reason: f.reason.clone(),
                })
                .collect(),
            author: None,
        })
    }
}

/// CoS の対話 run の `actions` を実行する（ADR-0048 D3）。
///
/// - 冪等: `run_id` を一度でも `execute` したら（`TaskStore::console_action_run_claim` が `false` を
///   返したら）、2 回目以降は何もせず `Ok(None)`。
/// - `malformed`（`ConsoleAction` の形にすら合わなかった要素。呼び出し側 = `task-worker` の
///   `actions_from_result_json` が JSON を読んだ時点で分けたもの）は `failed` にそのまま写る。
#[allow(clippy::too_many_arguments)]
pub fn execute(
    store: &dyn TaskStore,
    org: &[OrgNode],
    roles: &[task_core::RoleSpec],
    genres: &[task_core::GenreSpec],
    task: &Task,
    run_id: &str,
    valid: &[ConsoleAction],
    malformed: &[String],
    now: OffsetDateTime,
) -> Result<Option<ActionsOutcome>, OpsError> {
    let source = source_request_context(store, task)?;
    if !store.console_action_run_claim(run_id, task.id, now)? {
        return Ok(None);
    }
    let mut outcome = ActionsOutcome::default();
    for reason in malformed {
        outcome.failed.push(FailedAction {
            kind: "unknown".to_string(),
            reason: reason.clone(),
        });
    }
    for action in valid {
        let mut action = action.clone();
        if let ConsoleAction::CreateTask {
            objective,
            acceptance,
            ..
        } = &mut action
        {
            objective.push_str(&source);
            // 空の acceptance は従来どおり入力エラー。自動条件で隠さない。
            if !acceptance.is_empty() {
                acceptance.push("元の依頼と成果の範囲が整合していること。修正・実装を依頼された場合、調査報告や提案だけでは合格にせず、実際の変更と検証の証拠を確認する。人が明示的に調査だけを求めた場合はその範囲を守る。分割した中間成果を依頼全体の完了として扱わない。".into());
            }
        }
        match execute_one(store, org, roles, genres, &action, now) {
            Ok(executed) => outcome.executed.push(executed),
            Err(reason) => outcome.failed.push(FailedAction {
                kind: action.kind().to_string(),
                reason,
            }),
        }
    }
    Ok(Some(outcome))
}

/// CoS が書いた要約とは独立に、依頼時点の会話をワーカーとレビュアーへ渡す。
fn source_request_context(store: &dyn TaskStore, task: &Task) -> Result<String, OpsError> {
    let mut out = format!(
        "\n\n## 元の依頼（対話タスク {}）\n{}\n",
        task.id, task.objective
    );
    if let (Some(node), Some(message_id)) = (&task.assignee, task.conversation) {
        let mut messages = store.message_list(
            node,
            task.project_id,
            crate::conversation::CONVERSATION_HISTORY,
        )?;
        messages.retain(|m| m.id == message_id || m.created_at < task.created_at);
        messages.sort_by_key(|m| (m.created_at, m.id));
        out.push_str("\n依頼時点までの対話（後の依頼で上書きしない）:\n");
        for message in messages {
            out.push_str(&format!("[{}] {}\n", message.role.as_str(), message.text));
        }
    }
    Ok(out)
}

fn execute_one(
    store: &dyn TaskStore,
    org: &[OrgNode],
    roles: &[task_core::RoleSpec],
    genres: &[task_core::GenreSpec],
    action: &ConsoleAction,
    now: OffsetDateTime,
) -> Result<ExecutedAction, String> {
    match action {
        ConsoleAction::CreateTask {
            tier,
            title,
            objective,
            acceptance,
            harness,
            skills,
            mode,
            repos,
            project,
            milestone,
            assignee,
        } => create_task_action(
            store, roles, genres, title, objective, acceptance, harness, skills, mode, repos,
            project, milestone, assignee, *tier, now,
        ),
        ConsoleAction::ProposeProject {
            title,
            request,
            repos,
        } => propose_project_action(store, title, request, repos, now),
        ConsoleAction::AddMilestone {
            project,
            title,
            description,
        } => add_milestone_action(store, project, title, description),
        ConsoleAction::AskHuman { text } => ask_human_action(text),
        // `org` は matching に使わない（D5 の matching は `assignee` 省略時にディスパッチャの
        // `assign_if_needed` が別途走る）。ここでは `assignee` の検証だけ `add::create_task_with_roles`
        // に任せる。
        #[allow(unreachable_patterns)]
        _ => {
            let _ = org;
            Err("unknown action".to_string())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn create_task_action(
    store: &dyn TaskStore,
    roles: &[task_core::RoleSpec],
    genres: &[task_core::GenreSpec],
    title: &str,
    objective: &str,
    acceptance: &[String],
    harness: &Option<String>,
    skills: &[String],
    mode: &Option<String>,
    repos: &[String],
    project: &Option<String>,
    milestone: &Option<String>,
    assignee: &Option<String>,
    tier: Option<task_core::Tier>,
    now: OffsetDateTime,
) -> Result<ExecutedAction, String> {
    if acceptance.is_empty() {
        return Err(
            "create_task には acceptance が 1 つ以上要る（人が読める受け入れ条件を書くこと）"
                .to_string(),
        );
    }
    let project_id = match project {
        Some(raw) if !raw.trim().is_empty() => Some(
            raw.parse::<ProjectId>()
                .map_err(|_| format!("project {raw:?} is not a valid id"))?,
        ),
        _ => None,
    };
    let milestone_id = match milestone {
        Some(raw) if !raw.trim().is_empty() => Some(
            raw.parse::<MilestoneId>()
                .map_err(|_| format!("milestone {raw:?} is not a valid id"))?,
        ),
        _ => None,
    };
    let mode = match mode {
        Some(raw) if !raw.trim().is_empty() => {
            Some(parse_mode(raw).ok_or_else(|| format!("unknown mode: {raw:?}"))?)
        }
        _ => None,
    };
    let spec = NewTaskSpec {
        title: title.to_string(),
        objective: objective.to_string(),
        acceptance: acceptance
            .iter()
            // Console からの小さな頼みを毎回人の承認待ちにしない: 受け入れ条件の文はレビュー担当が判定する
            // （人が見たいときはタスク画面で条件を直せる。ADR-0044 D1）。
            .map(|text| CriterionSpec::Reviewer { text: text.clone() })
            .collect(),
        kind: task_core::TaskKind::Execute,
        tier,
        priority: None,
        parent: None,
        depends_on: Vec::new(),
        max_turns: None,
        max_wall_secs: None,
        max_retries: crate::add::DEFAULT_MAX_RETRIES,
        role: None,
        genre: harness.clone(),
        aggregate: false,
        project_id,
        milestone_id,
        assignee: assignee.clone(),
        workspace: None,
        cluster: None,
        adapter: None,
        repos: repos.to_vec(),
        labels: Vec::new(),
        skills: skills.to_vec(),
        mode,
        category: None,
        // ADR-0048 D3: Console から（CoS の actions 経由で）作るタスクは人が Go 済みとして ready。
        status: Some(Status::Ready),
    };
    let task = crate::add::create_task_with_roles(store, spec, roles, genres, now)
        .map_err(|e| e.to_string())?;
    Ok(ExecutedAction {
        kind: "create_task",
        summary: format!("→ タスクを作りました: {}", task.title),
        task_id: Some(task.id),
        project_id: task.project_id,
        milestone_id: task.milestone_id,
    })
}

fn parse_mode(raw: &str) -> Option<task_core::TaskMode> {
    match raw {
        "prototype" => Some(task_core::TaskMode::Prototype),
        "production" => Some(task_core::TaskMode::Production),
        "research" => Some(task_core::TaskMode::Research),
        // `standard` is the default tier and models occasionally copy it into both
        // optional fields. Treat that common mix-up as the default production mode
        // instead of dropping an otherwise valid create_task action.
        "standard" => Some(task_core::TaskMode::Production),
        _ => None,
    }
}

/// ADR-0048 D3: `propose_project.repos[]` は**絶対パス**として読む（案件のリポジトリはまだ無いので
/// 名前では引けない。既存の `POST /projects/{id}/repos` と同じ決定的な既定: 名前は場所から、種類は
/// `.git` の有無から決める）。絶対パスでない要素が 1 つでもあれば action 全体を実行しない。
fn propose_project_action(
    store: &dyn TaskStore,
    title: &str,
    request: &str,
    repos: &[String],
    now: OffsetDateTime,
) -> Result<ExecutedAction, String> {
    if title.trim().is_empty() {
        return Err("title must not be blank".to_string());
    }
    if request.trim().is_empty() {
        return Err("request must not be blank".to_string());
    }
    let mut locations = Vec::new();
    for raw in repos {
        let path = std::path::PathBuf::from(raw);
        if !path.is_absolute() {
            return Err(format!(
                "repos は絶対パスで書くこと（相対パス・名前は不可）: {raw:?}"
            ));
        }
        locations.push(WorkspaceSpec::Local { path, mode: None });
    }
    let project = Project {
        archived_at: None,
        paused_from: None,
        id: ProjectId::new(),
        title: title.trim().to_string(),
        request: request.trim().to_string(),
        status: ProjectStatus::Proposed,
        secretary_summary: None,
        workspace: None,
        created_at: now,
        updated_at: now,
    };
    store
        .project_create(&project)
        .map_err(|e| format!("could not create the project: {e}"))?;
    let mut names: Vec<String> = Vec::new();
    for (i, location) in locations.into_iter().enumerate() {
        let mut name = task_core::default_repo_name(&location);
        if names.contains(&name) || !task_core::valid_repo_name(&name) {
            name = format!("repo-{}", i + 1);
        }
        names.push(name.clone());
        let repo = ProjectRepo {
            id: RepoId::new(),
            project_id: project.id,
            name,
            kind: task_core::store::detect_repo_kind(&location),
            location,
            default_branch: None,
            sync: None,
            run: RepoRun::Auto,
            is_primary: i == 0,
            created_at: now,
        };
        store
            .repo_create(&repo)
            .map_err(|e| format!("could not create the repo {:?}: {e}", repo.name))?;
    }
    Ok(ExecutedAction {
        kind: "propose_project",
        summary: format!("→ 案件を提案しました: {}", project.title),
        task_id: None,
        project_id: Some(project.id),
        milestone_id: None,
    })
}

/// ADR-0048 D3: `add_milestone` は既存の案件に `proposed` の途中目標を末尾に足す
/// （`store.milestone_create` が `seq` を自動で末尾にする）。
fn add_milestone_action(
    store: &dyn TaskStore,
    project: &str,
    title: &str,
    description: &str,
) -> Result<ExecutedAction, String> {
    if title.trim().is_empty() {
        return Err("title must not be blank".to_string());
    }
    let project_id = project
        .parse::<ProjectId>()
        .map_err(|_| format!("project {project:?} is not a valid id"))?;
    if store
        .project_get(project_id)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Err(format!("project {project_id} does not exist"));
    }
    let milestone: Milestone = store
        .milestone_create(
            project_id,
            title.trim(),
            description.trim(),
            MilestoneStatus::Proposed,
        )
        .map_err(|e| format!("could not create the milestone: {e}"))?;
    Ok(ExecutedAction {
        kind: "add_milestone",
        summary: format!("→ 途中目標を追加しました: {}", milestone.title),
        task_id: None,
        project_id: Some(project_id),
        milestone_id: Some(milestone.id),
    })
}

/// ADR-0048 D3: `ask_human` は taskd の側では何も作らない（CoS の返事そのものが人への問いかけ）。
/// 「実行できた」扱いにして、人に見える形（Console の `reply` の `actions_result`）に残すだけ。
fn ask_human_action(text: &str) -> Result<ExecutedAction, String> {
    if text.trim().is_empty() {
        return Err("text must not be blank".to_string());
    }
    Ok(ExecutedAction {
        kind: "ask_human",
        summary: format!("→ 判断を仰いでいます: {}", text.trim()),
        task_id: None,
        project_id: None,
        milestone_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{OrgKind, SqliteStore};

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn cos_task() -> Task {
        use task_core::{Budget, TaskId, TaskKind, Tier, WorkerHint, WorkspaceSpec};
        let t = now();
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
            status: Status::Running,
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
            created_at: t,
            updated_at: t,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: Some("cos".into()),
            conversation: Some(task_core::MessageId::new()),
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    fn seed_engineering(store: &SqliteStore) {
        let t = now();
        store
            .org_upsert(&OrgNode {
                profile: Default::default(),
                id: "cos".into(),
                parent_id: None,
                name: "Chief of Staff".into(),
                kind: OrgKind::Secretary,
                genre: None,
                brief: String::new(),
                position: 0,
                created_at: t,
                updated_at: t,
            })
            .unwrap();
        store
            .org_upsert(&OrgNode {
                profile: Default::default(),
                id: "engineering".into(),
                parent_id: Some("cos".into()),
                name: "Engineering".into(),
                kind: OrgKind::Department,
                genre: None,
                brief: String::new(),
                position: 0,
                created_at: t,
                updated_at: t,
            })
            .unwrap();
    }

    /// テスト専用の最小パーサ（`task_worker::actions_from_result_json` と同じ規則を、
    /// `task-worker` に依存せずに再現する。本物の解析のテストは `task-worker` 側にある）。
    fn parse(json: &str) -> (Vec<ConsoleAction>, Vec<String>) {
        let value: serde_json::Value = serde_json::from_str(json).expect("json");
        let items = value
            .get("actions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut valid = Vec::new();
        let mut malformed = Vec::new();
        for (i, item) in items.into_iter().enumerate() {
            match serde_json::from_value::<ConsoleAction>(item) {
                Ok(action) => valid.push(action),
                Err(e) => malformed.push(format!("action #{}: {e}", i + 1)),
            }
        }
        (valid, malformed)
    }

    /// `create_task`: 有効な action は `ready` のタスクを作り、`assignee` 省略なら matching は
    /// ここでは走らせない（ディスパッチャの `assign_if_needed` が次 tick で決める）。
    #[test]
    fn create_task_makes_a_ready_task_and_records_a_summary() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_engineering(&store);
        let task = cos_task();
        let parsed = parse(
            r#"{"actions":[{"type":"create_task","title":"直す","objective":"直して",
               "acceptance":["直った"],"harness":"coding","tier":"frontier"}]}"#,
        );
        let outcome = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-1",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .expect("not idempotent-skipped");
        assert_eq!(outcome.executed.len(), 1);
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        let created = outcome.executed[0].task_id.expect("task id");
        let stored = store.get(created).unwrap().expect("task exists");
        assert_eq!(stored.worker_hint.tier, task_core::Tier::Frontier);
        assert_eq!(stored.title, "直す");
        assert_eq!(stored.status, Status::Ready);
        assert_eq!(stored.genre.as_deref(), Some("coding"));
        assert_eq!(stored.assignee, None, "matching は別経路");
        assert!(outcome.executed[0].summary.contains("直す"));
    }

    /// 実機 2026-09-21: CoS が tier と mode の両方に `standard` を書き、正しい create_task
    /// 全体が捨てられた。tier の common value は既定の production mode として受ける。
    #[test]
    fn create_task_tolerates_standard_in_mode_as_production() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_engineering(&store);
        let task = cos_task();
        let parsed = parse(
            r#"{"actions":[{"type":"create_task","title":"直す","objective":"直して",
               "acceptance":["直った"],"tier":"standard","mode":"standard"}]}"#,
        );
        let outcome = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-standard-mode",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .expect("not idempotent-skipped");

        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        let created = outcome.executed[0].task_id.expect("task id");
        let stored = store.get(created).unwrap().expect("task exists");
        assert_eq!(stored.worker_hint.tier, task_core::Tier::Standard);
        assert_eq!(stored.mode, task_core::TaskMode::Production);
    }

    /// 検証に落ちた action（受け入れ条件無し）は実行されず、理由が残る。
    #[test]
    fn an_invalid_create_task_is_not_executed_and_gets_a_reason() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_engineering(&store);
        let task = cos_task();
        let parsed = parse(r#"{"actions":[{"type":"create_task","title":"t","objective":"o"}]}"#);
        let outcome = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-1",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .unwrap();
        assert!(outcome.executed.is_empty());
        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(outcome.failed[0].kind, "create_task");
        assert!(outcome.failed[0].reason.contains("acceptance"));
        assert!(store.list(None).unwrap().is_empty(), "何も作らない");
    }

    /// 知らない harness（`genres` が設定されていれば検証される）や存在しない project / milestone も
    /// 実行されない。
    #[test]
    fn unknown_project_or_milestone_is_rejected() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_engineering(&store);
        let task = cos_task();
        let parsed = parse(
            r#"{"actions":[{"type":"create_task","title":"t","objective":"o",
               "acceptance":["ok"],"project":"01ZZZZZZZZZZZZZZZZZZZZZZZZ"}]}"#,
        );
        let outcome = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-1",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .unwrap();
        assert!(outcome.executed.is_empty());
        assert_eq!(outcome.failed.len(), 1);
        assert!(outcome.failed[0].reason.contains("does not exist"));
    }

    /// `propose_project`: `proposed` の案件が作られ、絶対パスの repos は primary + 名前で入る。
    #[test]
    fn propose_project_creates_a_proposed_project_with_repos() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = cos_task();
        let parsed = parse(
            r#"{"actions":[{"type":"propose_project","title":"新案件","request":"やりたい",
               "repos":["/tmp/agent-platform"]}]}"#,
        );
        let outcome = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-1",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.executed.len(), 1);
        let project_id = outcome.executed[0].project_id.expect("project id");
        let project = store.project_get(project_id).unwrap().expect("project");
        assert_eq!(project.title, "新案件");
        assert_eq!(project.status, ProjectStatus::Proposed);
        let repos = store.repo_list(project_id).unwrap();
        assert_eq!(repos.len(), 1);
        assert!(repos[0].is_primary);
        assert_eq!(repos[0].name, "agent-platform");
    }

    /// 相対パスの repos は action 全体を実行しない（案件そのものも作らない）。
    #[test]
    fn propose_project_rejects_a_relative_repo_path() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = cos_task();
        let parsed = parse(
            r#"{"actions":[{"type":"propose_project","title":"t","request":"r",
               "repos":["relative/path"]}]}"#,
        );
        let outcome = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-1",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .unwrap();
        assert!(outcome.executed.is_empty());
        assert_eq!(outcome.failed.len(), 1);
        assert!(store.project_list().unwrap().is_empty());
    }

    /// `add_milestone`: 既存案件の末尾に `proposed` の途中目標を足す。知らない案件は失敗。
    #[test]
    fn add_milestone_appends_a_proposed_milestone() {
        let store = SqliteStore::open_in_memory().unwrap();
        let t = now();
        let project = Project {
            archived_at: None,
            paused_from: None,
            id: ProjectId::new(),
            title: "既存案件".into(),
            request: "r".into(),
            status: ProjectStatus::Active,
            secretary_summary: None,
            workspace: None,
            created_at: t,
            updated_at: t,
        };
        store.project_create(&project).unwrap();
        let task = cos_task();
        let parsed = parse(&format!(
            r#"{{"actions":[{{"type":"add_milestone","project":"{}","title":"次","description":"d"}}]}}"#,
            project.id
        ));
        let outcome = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-1",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.executed.len(), 1);
        let milestones = store.milestone_list(project.id).unwrap();
        assert_eq!(milestones.len(), 1);
        assert_eq!(milestones[0].title, "次");
        assert_eq!(milestones[0].status, MilestoneStatus::Proposed);

        let bad = parse(
            r#"{"actions":[{"type":"add_milestone","project":"01ZZZZZZZZZZZZZZZZZZZZZZZZ","title":"t"}]}"#,
        );
        let outcome = execute(&store, &[], &[], &[], &task, "run-2", &bad.0, &bad.1, now())
            .unwrap()
            .unwrap();
        assert!(outcome.executed.is_empty());
        assert!(outcome.failed[0].reason.contains("does not exist"));
    }

    /// `ask_human`: 実行済みとして記録するだけ（何も作らない）。
    #[test]
    fn ask_human_is_recorded_without_creating_anything() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = cos_task();
        let parsed = parse(r#"{"actions":[{"type":"ask_human","text":"どちらがよいですか"}]}"#);
        let outcome = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-1",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.executed.len(), 1);
        assert_eq!(outcome.executed[0].kind, "ask_human");
        assert!(outcome.executed[0].summary.contains("どちらがよいですか"));
    }

    /// 冪等性: 同じ `run_id` の 2 回目は何もしない（`Ok(None)`）。
    #[test]
    fn the_same_run_id_executes_actions_only_once() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_engineering(&store);
        let task = cos_task();
        let parsed = parse(
            r#"{"actions":[{"type":"create_task","title":"直す","objective":"直して","acceptance":["直った"]}]}"#,
        );
        let first = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-dup",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .expect("first run executes");
        assert_eq!(first.executed.len(), 1);
        assert_eq!(store.list(None).unwrap().len(), 1);

        let second = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-dup",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap();
        assert!(second.is_none(), "2 回目は何もしない");
        assert_eq!(
            store.list(None).unwrap().len(),
            1,
            "重複してタスクが増えない"
        );
    }

    /// malformed（`ConsoleAction` の形に合わなかった要素）も `failed` に写る。
    #[test]
    fn malformed_actions_from_parsing_are_reported_as_failures() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = cos_task();
        let parsed = parse(r#"{"actions":[{"type":"unknown_action"}]}"#);
        let outcome = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "run-1",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .unwrap();
        assert!(outcome.executed.is_empty());
        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(outcome.failed[0].kind, "unknown");
    }

    /// `failure_note` / `to_metadata`。
    #[test]
    fn outcome_formats_a_failure_note_and_metadata() {
        let outcome = ActionsOutcome {
            executed: vec![ExecutedAction {
                kind: "create_task",
                summary: "→ タスクを作りました: t".into(),
                task_id: Some(TaskId::new()),
                project_id: None,
                milestone_id: None,
            }],
            failed: vec![FailedAction {
                kind: "add_milestone".into(),
                reason: "project x does not exist".into(),
            }],
        };
        let note = outcome.failure_note().expect("note");
        assert!(note.contains("実行できなかった action"));
        assert!(note.contains("add_milestone: project x does not exist"));
        let metadata = outcome.to_metadata().expect("metadata");
        assert_eq!(metadata.actions_executed.len(), 1);
        assert_eq!(metadata.actions_failed.len(), 1);
        assert!(ActionsOutcome::default().failure_note().is_none());
        assert!(ActionsOutcome::default().to_metadata().is_none());
    }
    #[test]
    fn delegated_work_keeps_the_original_request_and_a_scope_review_condition() {
        let store = SqliteStore::open_in_memory().unwrap();
        seed_engineering(&store);
        let mut task = cos_task();
        task.objective = "スマホGUIを修正し、検証してください".into();
        let source_id = task_core::MessageId::new();
        task.conversation = Some(source_id);
        let source = task_core::Message {
            id: source_id,
            node_id: "cos".into(),
            project_id: None,
            role: task_core::MessageRole::User,
            text: task.objective.clone(),
            run_id: None,
            task_id: Some(task.id),
            metadata: None,
            created_at: now(),
        };
        store.message_append(&source).unwrap();
        store
            .message_append(&task_core::Message {
                id: task_core::MessageId::new(),
                text: "後から届いた別の依頼".into(),
                created_at: now() + time::Duration::seconds(1),
                ..source
            })
            .unwrap();
        let parsed = parse(
            r#"{"actions":[{"type":"create_task","title":"GUIを調査","objective":"改善案を書く","acceptance":["報告書がある"]}]}"#,
        );
        let outcome = execute(
            &store,
            &[],
            &[],
            &[],
            &task,
            "source-run",
            &parsed.0,
            &parsed.1,
            now(),
        )
        .unwrap()
        .unwrap();
        let created = store
            .get(outcome.executed[0].task_id.unwrap())
            .unwrap()
            .unwrap();
        assert!(
            created
                .objective
                .contains("スマホGUIを修正し、検証してください")
        );
        assert!(!created.objective.contains("後から届いた別の依頼"));
        assert!(
            created
                .acceptance
                .iter()
                .any(|c| c.text.contains("調査報告や提案だけでは合格にせず"))
        );
    }
}
