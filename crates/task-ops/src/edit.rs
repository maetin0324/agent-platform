//! ADR-0044 D1（Phase 53）: 人がタスクを編集する（`PATCH /tasks/{id}`）。
//!
//! - 終端（`done` / `failed` / `cancelled`）のタスクは編集できない（API は 409）。
//! - `running` / `reviewing` は**受け付けるが次の run から効く**（走っている run は止めない。止めたければ
//!   D2 のコメントか D6 の中止）。
//! - 状態機械は通らない（`status` / `attempts` / `lease` は触らない）。書き込みは
//!   `TaskStore::update_task` 1 回で、`Event::Edited{fields, by:"human"}` を同じトランザクションに積む。
//!
//! LLM は呼ばない。検証は作成時（`add::build_task`）と同じ規則を使う。
//!
//! ADR-0062 Phase 108 追記: `workspace` は上記の例外で、`draft`/`ready`/`blocked`/`failed` を受け付ける
//! （`running`/`reviewing`/`done`/`cancelled` は 409）。さらに `workspace`/`assignee` の変更で B1
//! （担当に `cluster:<id>` が無い）の `blocked` の経路が通った場合だけ、`gate::answer` と同じ
//! `Trigger::Answer` を使って `ready` に戻す（状態機械を通る唯一の例外。他の項目は従来どおり触らない）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{
    Budget, Event, GenreSpec, MilestoneId, Status, StoreError, Task, TaskCategory, TaskId,
    TaskStore, Tier, Trigger, WorkspaceSpec,
};
use time::OffsetDateTime;

use crate::add::{CriterionSpec, PriorityInput};
use crate::error::OpsError;

/// `PATCH /tasks/{id}` の本文（ADR-0044 D1）。**書いた項目だけ**が変わる。
/// `Option<Option<T>>` の項目は「省略 = 変えない / `null` = 消す / 値 = その値にする」。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskEdit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    /// 差し替え（部分更新はしない）。1 件以上。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<Vec<CriterionSpec>>,
    /// ADR-0044 D3: `"P1"` でも `20` でもよい。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<PriorityInput>,
    /// ADR-0044 D3: 差し替え（小文字 `[a-z0-9-]`、最大 8 個）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<TaskCategory>,
    /// ADR-0043 D2（Phase 52 / A1）: このタスクが使う案件のリポジトリを**名前で**差し替える
    /// （`project_repos.name`。空配列で「リポジトリを使わない」）。名前は `POST /tasks` と同じ規則で
    /// **そのタスクの案件の中**から解決する（知らない名前・リモートと他の混在は 422、案件に属さない
    /// タスクで空でない `repos` を書くのも 422）。走っている run には効かず、次の run の worktree から。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
    /// 組織のノード（`null` で外す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<Option<String>>,
    /// 役割名（`null` で外す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Option<String>>,
    /// ADR-0033 D2 の最上位「タスク」の tier 指定（`worker_hint.tier`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// `worker_hint.adapter`（`null` で外す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<Option<String>>,
    /// 途中目標（`null` で外す）。そのタスクの案件のものであること。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone_id: Option<Option<MilestoneId>>,
    /// 差し替え。存在しない・`failed`/`cancelled`・自分自身はエラー。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<TaskId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// ADR-0046 D2（Phase 59）: 必要な能力タグの差し替え（小文字 `[a-z0-9._-]`、最大 12 個）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// ADR-0046 D4（Phase 59）: 進め方（`prototype` / `production` / `research`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<task_core::TaskMode>,
    /// ADR-0046 D3（Phase 59）: ハーネス（`tasks.genre` 列をそのまま harness id として使う）。
    /// `null` で外す。`genres`（= ハーネスのレジストリの射影）が空でなければ知らない id は 422。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<Option<String>>,
    /// 楽観的排他（現在の `status` と違えば 409）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_status: Option<Status>,
    /// ADR-0062 Phase 108 追記: 作業場所の差し替え。検証は `POST /tasks` と同じ規則
    /// （`Remote.cluster` が設定に存在すること — API 層〈`validated_workspace`〉で見る、
    /// `Remote` かつ明示の担当が `cluster:<id>` を持たなければ 422、`Local.path` は空でないこと）。
    /// **この項目だけは `draft`/`ready`/`blocked`/`failed` でも受け付ける**（他の項目は従来どおり
    /// 終端〈`done`/`failed`/`cancelled`〉で 409。`running`/`reviewing` への `workspace` 編集は 409）。
    /// `blocked`（B1 の unroutable）だったタスクは、この編集または `assignee` の変更で経路が通れば
    /// その場で `ready` に戻す（下記 `edit_task` を見よ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceSpec>,
}

impl TaskEdit {
    /// 1 つも項目が書かれていない（`expected_status` だけ）か。
    pub fn is_empty(&self) -> bool {
        *self
            == TaskEdit {
                expected_status: self.expected_status,
                ..TaskEdit::default()
            }
    }
}

/// `PATCH /tasks/{id}` の結果。
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct EditResult {
    pub task: Task,
    /// 実際に変えた項目の名前（決定的な並び。何も変わらなければ空）。
    pub fields: Vec<String>,
}

/// ADR-0044 D1: 編集を適用する。終端のタスクは `OpsError::InvalidState`（API は 409）。
/// `genres` は `role` と `genre` の整合検証に使う（作成時＝`add::build_task` と同じ規則。空なら検証しない）。
///
/// **`tier` / `adapter` / 予算は再解決しない**（ADR-0033 D2 の「タスク > 役割 > 担当 > 分野」は
/// **作成時に 1 回**だけ効く）。`assignee` や `role` を変えても、既に焼き付いた `worker_hint` と
/// `budget` はそのまま残る — 変えたければ同じ `PATCH` で明示的に書く（`docs/gui/api.md` §3.74）。
pub fn edit_task(
    store: &dyn TaskStore,
    id: TaskId,
    edit: TaskEdit,
    genres: &[GenreSpec],
    now: OffsetDateTime,
) -> Result<EditResult, OpsError> {
    let mut task = store.get(id)?.ok_or(OpsError::NotFound(id))?;
    if let Some(expected) = edit.expected_status
        && expected != task.status
    {
        return Err(OpsError::Conflict {
            expected,
            actual: task.status,
        });
    }
    // ADR-0062 Phase 108: `workspace` を含む編集は draft/ready/blocked/failed だけ許す
    // （running/reviewing/done/cancelled は 409）。`failed` は従来の終端チェックの対象だが、
    // 作業場所を直してやり直せるようにするための例外。`workspace` を含まない編集は従来どおり
    // 終端（done/failed/cancelled）を拒む。
    if edit.workspace.is_some() {
        if !matches!(
            task.status,
            Status::Draft | Status::Ready | Status::Blocked | Status::Failed
        ) {
            return Err(OpsError::InvalidState {
                id,
                context: format!("status={:?}", task.status),
                action:
                    "edited (workspace); only draft/ready/blocked/failed accept a workspace change"
                        .to_string(),
            });
        }
    } else if task.status.is_terminal() {
        // ADR-0044 D1: 終端のタスクは編集できない（やり直すなら `retry`、再開するなら `reopen`）。
        return Err(OpsError::InvalidState {
            id,
            context: format!("status={:?}", task.status),
            action: "edited; terminal tasks cannot be edited".to_string(),
        });
    }

    let original_status = task.status;
    let mut fields: Vec<String> = Vec::new();

    if let Some(title) = edit.title {
        if title.trim().is_empty() {
            return Err(OpsError::Validation("title must not be blank".to_string()));
        }
        if title != task.title {
            task.title = title;
            fields.push("title".to_string());
        }
    }
    if let Some(objective) = edit.objective {
        if objective.trim().is_empty() {
            return Err(OpsError::Validation(
                "objective must not be blank".to_string(),
            ));
        }
        if objective != task.objective {
            task.objective = objective;
            fields.push("objective".to_string());
        }
    }
    if let Some(acceptance) = edit.acceptance {
        if acceptance.is_empty() {
            return Err(OpsError::Validation(
                "acceptance must have at least one criterion".to_string(),
            ));
        }
        let built: Vec<_> = acceptance
            .into_iter()
            .map(CriterionSpec::into_criterion)
            .collect();
        if built != task.acceptance {
            task.acceptance = built;
            fields.push("acceptance".to_string());
        }
    }
    if let Some(priority) = edit.priority {
        let value = priority.to_i32();
        if value != task.priority {
            task.priority = value;
            fields.push("priority".to_string());
        }
    }
    if let Some(labels) = edit.labels {
        let normalized = task_core::normalize_labels(&labels).map_err(OpsError::Validation)?;
        if normalized != task.labels {
            task.labels = normalized;
            fields.push("labels".to_string());
        }
    }
    if let Some(category) = edit.category
        && category != task.category
    {
        task.category = category;
        fields.push("category".to_string());
    }
    // ADR-0046 D2（Phase 59）: 必要な能力タグ（差し替え）。
    if let Some(skills) = edit.skills {
        let normalized = task_core::normalize_skills(&skills).map_err(OpsError::Validation)?;
        if normalized != task.skills {
            task.skills = normalized;
            fields.push("skills".to_string());
        }
    }
    // ADR-0046 D4（Phase 59）: 進め方。
    if let Some(mode) = edit.mode
        && mode != task.mode
    {
        task.mode = mode;
        fields.push("mode".to_string());
    }
    // ADR-0046 D3（Phase 59）: ハーネス（`tasks.genre` 列）。知らない id は 422
    // （`genres` が空の設定では検証しない。作成時と同じ規律）。
    if let Some(harness) = edit.harness {
        if let Some(id) = harness.as_deref()
            && !genres.is_empty()
            && GenreSpec::find(genres, id).is_none()
        {
            return Err(OpsError::Validation(format!("unknown harness: {id:?}")));
        }
        if harness != task.genre {
            task.genre = harness;
            fields.push("harness".to_string());
        }
    }
    if let Some(names) = edit.repos {
        // ADR-0043 D2: 名前 → `RepoRef`。解決の規則は `POST /tasks`（`add::resolve_repos` の
        // 「明示」の枝）と同じ ＝ **そのタスクの案件の中**から引く。継承（親 → primary）は
        // 作成時だけの規則なので、編集で `[]` と書けば「リポジトリを使わない」になる。
        let resolved = if names.is_empty() {
            Vec::new()
        } else {
            let Some(project_id) = task.project_id else {
                return Err(OpsError::Validation(
                    "repos can only be used on a task that belongs to a project".to_string(),
                ));
            };
            let available = store.repo_list(project_id)?;
            task_core::resolve_task_repos(&available, &names)
                .map_err(|e| OpsError::Validation(e.to_string()))?
        };
        if resolved != task.repos {
            task.repos = resolved;
            fields.push("repos".to_string());
        }
    }
    if let Some(assignee) = edit.assignee {
        if let Some(node_id) = assignee.as_deref() {
            let org = store.org_list()?;
            if !org.iter().any(|n| n.id == node_id) {
                return Err(OpsError::Validation(format!(
                    "assignee {node_id:?} is not an org node"
                )));
            }
        }
        if assignee != task.assignee {
            task.assignee = assignee;
            fields.push("assignee".to_string());
        }
    }
    // ADR-0062 Phase 108: 作業場所の差し替え。`Remote.cluster` が設定にあることは API 層
    // （`validated_workspace`）が見る。ここでは `Local.path` が空でないことと、`Task` としての
    // 整合（下の cluster:<id> の検証）だけを見る。
    if let Some(workspace) = edit.workspace {
        if let WorkspaceSpec::Local { path, .. } = &workspace
            && path.as_os_str().is_empty()
        {
            return Err(OpsError::Validation(
                "workspace.path must not be empty".to_string(),
            ));
        }
        if workspace != task.workspace {
            task.workspace = workspace;
            fields.push("workspace".to_string());
        }
    }
    if let Some(role) = edit.role {
        // 作成時（`add::build_task`）と同じ規則: `genre` が設定にあり、その分野の `roles` に
        // 含まれない役割は拒否する（`genre` はこの API では変えられないので、片方だけ壊せてしまう）。
        if let (Some(role_id), Some(genre_id)) = (role.as_deref(), task.genre.as_deref())
            && !genres.is_empty()
            && let Some(genre_spec) = GenreSpec::find(genres, genre_id)
            && !genre_spec.roles.iter().any(|r| r == role_id)
        {
            return Err(OpsError::Validation(format!(
                "role {role_id:?} is not one of genre {genre_id:?}'s roles"
            )));
        }
        if role != task.role {
            task.role = role;
            fields.push("role".to_string());
        }
    }
    if let Some(tier) = edit.tier
        && tier != task.worker_hint.tier
    {
        task.worker_hint.tier = tier;
        fields.push("tier".to_string());
    }
    if let Some(adapter) = edit.adapter
        && adapter != task.worker_hint.adapter
    {
        task.worker_hint.adapter = adapter;
        fields.push("adapter".to_string());
    }
    if let Some(milestone_id) = edit.milestone_id {
        if let Some(mid) = milestone_id {
            let Some(project_id) = task.project_id else {
                return Err(OpsError::Validation(
                    "milestone_id requires the task to belong to a project".to_string(),
                ));
            };
            if !store
                .milestone_list(project_id)?
                .iter()
                .any(|m| m.id == mid)
            {
                return Err(OpsError::Validation(format!(
                    "milestone {mid} does not belong to project {project_id}"
                )));
            }
        }
        if milestone_id != task.milestone_id {
            task.milestone_id = milestone_id;
            fields.push("milestone_id".to_string());
        }
    }
    if let Some(depends_on) = edit.depends_on {
        for dep in &depends_on {
            if *dep == id {
                return Err(OpsError::Validation(format!(
                    "task {id} cannot depend on itself"
                )));
            }
            match store.get(*dep)? {
                None => {
                    return Err(OpsError::Validation(format!(
                        "dependency {dep} does not exist"
                    )));
                }
                Some(d) if matches!(d.status, Status::Failed | Status::Cancelled) => {
                    return Err(OpsError::Validation(format!(
                        "dependency {dep} has status {:?} and cannot be depended on",
                        d.status
                    )));
                }
                Some(_) => {}
            }
        }
        // 作成時は「新しい id は誰の `depends_on` にも入っていない」ので循環は作れないが、編集は作れる。
        // 循環した 2 件は `ready_tasks` が永久に返さず（先行が `done` にならない）、エラーも通知も
        // 出ないまま止まる（Phase 53 の監査で発見）ので、ここで閉じる。
        if let Some(cycle) = reaches(store, &depends_on, id)? {
            return Err(OpsError::Validation(format!(
                "depends_on would create a cycle: {cycle} depends on {id}"
            )));
        }
        if depends_on != task.depends_on {
            task.depends_on = depends_on;
            fields.push("depends_on".to_string());
        }
    }
    // ADR-0046 D5（Phase 59）: 担当かハーネスを変えたら、その担当がそのハーネスを受けられること。
    if fields.iter().any(|f| f == "assignee" || f == "harness")
        && let Some(assignee) = task.assignee.as_deref()
    {
        let org = store.org_list()?;
        crate::matching::assignee_accepts(&org, assignee, task.genre.as_deref())
            .map_err(OpsError::Validation)?;
    }
    // ADR-0062 B1/B3・Phase 108 追記: `workspace` か `assignee` を変えて、その結果が明示の `Remote` で
    // 担当が居るなら、その担当が `cluster:<id>` を持つこと（無ければ 422）。
    if fields.iter().any(|f| f == "workspace" || f == "assignee")
        && let WorkspaceSpec::Remote { cluster, .. } = &task.workspace
        && let Some(assignee) = task.assignee.as_deref()
    {
        let org = store.org_list()?;
        crate::matching::assignee_has_cluster_tool(&org, assignee, cluster)
            .map_err(OpsError::Validation)?;
    }
    let budget = Budget {
        max_turns: edit.max_turns.unwrap_or(task.budget.max_turns),
        max_wall_secs: edit.max_wall_secs.unwrap_or(task.budget.max_wall_secs),
        max_retries: edit.max_retries.unwrap_or(task.budget.max_retries),
    };
    if budget != task.budget {
        task.budget = budget;
        fields.push("budget".to_string());
    }

    if fields.is_empty() {
        // 何も変わらないなら書かない（イベントも積まない）。
        return Ok(EditResult { task, fields });
    }
    task.updated_at = now;
    // `status` / `attempts` / `lease` はストアが**トランザクションの中で読み直した**値で上書きする
    // （編集中にディスパッチャがリースを取っていても壊さない）。返ってくるのがその結果。
    let mut task = store.update_task(
        &task,
        Event::Edited {
            fields: fields.clone(),
            by: "human".to_string(),
        },
    )?;

    // ADR-0062 Phase 108: `blocked`（B1/ADR-0046 D5 の unroutable）だったタスクで、この編集が
    // `workspace`/`assignee` を変えたなら、上の検証を通った時点で経路は解決している
    // （明示 `Remote` は `cluster:<id>` の検証を済ませ、`assignee` の harness は D5 の検証を済ませて
    // いる）。既存の「質問に答える」経路（`gate::answer` と同じ `Trigger::Answer` +
    // `Event::Answered` + 未決の approvals を settle）に相乗りして `ready` に戻す。
    // worker が聞いた質問による `blocked`（`Trigger::WorkerQuestion`）はここでは触らない
    // （`latest_block_is_unroutable` が見分ける）。
    if original_status == Status::Blocked
        && fields.iter().any(|f| f == "workspace" || f == "assignee")
    {
        let events = store.events_for(id)?;
        if crate::derive::latest_block_is_unroutable(&events) {
            let question = crate::derive::latest_question(&events);
            let answer = "解決済み（作業場所/担当の変更）".to_string();
            match store.apply_transition(
                id,
                Trigger::Answer,
                Some(Event::Answered {
                    question,
                    answer: answer.clone(),
                }),
            ) {
                Ok(outcome) => {
                    task.status = outcome.next;
                    crate::gate::settle_pending_approvals(store, id, &answer)?;
                }
                // 競合（その間に別の何かが状態を動かした）は無視する。編集そのものは既に書けている。
                Err(StoreError::InvalidTransition(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(EditResult { task, fields })
}

/// `starts` から `depends_on` をたどって `target` に着くか（着くなら最初に見つけた経路上の id）。
/// 深さではなく「訪れた集合」で止めるので、既に循環している DB でも終わる。
fn reaches(
    store: &dyn TaskStore,
    starts: &[TaskId],
    target: TaskId,
) -> Result<Option<TaskId>, OpsError> {
    let mut seen: std::collections::HashSet<TaskId> = std::collections::HashSet::new();
    let mut stack: Vec<TaskId> = starts.to_vec();
    while let Some(id) = stack.pop() {
        if id == target {
            return Ok(Some(id));
        }
        if !seen.insert(id) {
            continue;
        }
        if let Some(task) = store.get(id)? {
            stack.extend(task.depends_on.iter().copied());
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{
        ArtifactRef, Check, Criterion, OrgKind, OrgNode, Profile, SqliteStore, TaskKind,
        WorkerHint, WorkspaceSpec,
    };

    fn task_with(status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
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
            project_id: None,
            milestone_id: None,
            assignee: None,
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    /// ADR-0044 D1: 書いた項目だけが変わり、`Event::Edited{fields}` が残る。列（検索用の写し）も揃う。
    #[test]
    fn an_edit_changes_only_the_written_fields_and_records_an_edited_event() {
        let store = SqliteStore::open_in_memory().expect("store");
        let task = task_with(Status::Ready);
        let id = task.id;
        store.insert(&task).expect("insert");

        let edit = TaskEdit {
            title: Some("新しい題名".into()),
            priority: Some(PriorityInput::Label(crate::add::PriorityLabel::P0)),
            labels: Some(vec!["infra".into(), "infra".into(), "urgent".into()]),
            category: Some(TaskCategory::Bug),
            tier: Some(Tier::Frontier),
            max_turns: Some(30),
            ..TaskEdit::default()
        };
        let result = edit_task(&store, id, edit, &[], OffsetDateTime::now_utc()).expect("edit");
        assert_eq!(
            result.fields,
            vec!["title", "priority", "labels", "category", "tier", "budget"]
        );
        let after = store.get(id).expect("get").expect("task");
        assert_eq!(after.title, "新しい題名");
        assert_eq!(after.priority, 30);
        assert_eq!(
            after.labels,
            vec!["infra".to_string(), "urgent".to_string()],
            "重複は畳む"
        );
        assert_eq!(after.category, TaskCategory::Bug);
        assert_eq!(after.worker_hint.tier, Tier::Frontier);
        assert_eq!(after.budget.max_turns, 30);
        assert_eq!(after.objective, "o", "書かなかった項目は変わらない");
        assert_eq!(after.status, Status::Ready, "状態機械は通らない");

        let events = store.events_for(id).expect("events");
        let edited = events
            .iter()
            .find_map(|(_, e)| match e {
                Event::Edited { fields, by } => Some((fields.clone(), by.clone())),
                _ => None,
            })
            .expect("Edited event");
        assert_eq!(edited.1, "human");
        assert_eq!(edited.0, result.fields);
    }

    /// ADR-0044 D1: 終端のタスクは 409（`InvalidState`）。`running` / `reviewing` は受け付ける。
    #[test]
    fn terminal_tasks_refuse_edits_but_running_and_reviewing_accept_them() {
        for status in [Status::Done, Status::Failed, Status::Cancelled] {
            let store = SqliteStore::open_in_memory().expect("store");
            let task = task_with(status);
            let id = task.id;
            store.insert(&task).expect("insert");
            let err = edit_task(
                &store,
                id,
                TaskEdit {
                    title: Some("x".into()),
                    ..TaskEdit::default()
                },
                &[],
                OffsetDateTime::now_utc(),
            )
            .unwrap_err();
            assert!(
                matches!(err, OpsError::InvalidState { .. }),
                "{status:?}: {err}"
            );
        }
        for status in [Status::Running, Status::Reviewing] {
            let store = SqliteStore::open_in_memory().expect("store");
            let task = task_with(status);
            let id = task.id;
            store.insert(&task).expect("insert");
            let result = edit_task(
                &store,
                id,
                TaskEdit {
                    title: Some("x".into()),
                    ..TaskEdit::default()
                },
                &[],
                OffsetDateTime::now_utc(),
            )
            .unwrap_or_else(|e| panic!("{status:?}: {e}"));
            assert_eq!(result.fields, vec!["title"]);
            assert_eq!(
                store.get(id).expect("get").expect("task").status,
                status,
                "走っている run は止めない（次の run から効く）"
            );
        }
    }

    /// 検証: 空の題名・壊れたラベル・自分への依存・知らない担当は拒否し、何も書かない。
    #[test]
    fn invalid_edits_are_rejected_without_writing() {
        let store = SqliteStore::open_in_memory().expect("store");
        let task = task_with(Status::Ready);
        let id = task.id;
        store.insert(&task).expect("insert");

        for edit in [
            TaskEdit {
                title: Some("  ".into()),
                ..TaskEdit::default()
            },
            TaskEdit {
                labels: Some(vec!["Bad Label".into()]),
                ..TaskEdit::default()
            },
            TaskEdit {
                labels: Some((0..9).map(|i| format!("l{i}")).collect()),
                ..TaskEdit::default()
            },
            TaskEdit {
                depends_on: Some(vec![id]),
                ..TaskEdit::default()
            },
            TaskEdit {
                // 存在しない先行は拒否（循環の検査より前に落ちる）。
                depends_on: Some(vec![TaskId::new()]),
                ..TaskEdit::default()
            },
            TaskEdit {
                assignee: Some(Some("nobody".into())),
                ..TaskEdit::default()
            },
            TaskEdit {
                acceptance: Some(vec![]),
                ..TaskEdit::default()
            },
        ] {
            let err = edit_task(&store, id, edit, &[], OffsetDateTime::now_utc()).unwrap_err();
            assert!(matches!(err, OpsError::Validation(_)), "{err}");
        }
        let after = store.get(id).expect("get").expect("task");
        assert_eq!(after.title, "t");
        assert!(
            store.events_for(id).expect("events").is_empty(),
            "何も積まない"
        );
    }

    /// Phase 53 の監査: `PATCH depends_on` で**循環**は作れない（作成時は構造上できなかった）。
    #[test]
    fn depends_on_cannot_create_a_cycle() {
        let store = SqliteStore::open_in_memory().expect("store");
        let a = task_with(Status::Ready);
        let mut b = task_with(Status::Ready);
        b.depends_on = vec![a.id];
        store.insert(&a).expect("insert a");
        store.insert(&b).expect("insert b");

        // a が b に依存すると a → b → a の循環になる。
        let err = edit_task(
            &store,
            a.id,
            TaskEdit {
                depends_on: Some(vec![b.id]),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(
            matches!(err, OpsError::Validation(ref m) if m.contains("cycle")),
            "{err}"
        );
        assert!(
            store
                .get(a.id)
                .expect("get")
                .expect("task")
                .depends_on
                .is_empty()
        );

        // 循環にならない張り替えは通る。
        let c = task_with(Status::Ready);
        store.insert(&c).expect("insert c");
        let result = edit_task(
            &store,
            a.id,
            TaskEdit {
                depends_on: Some(vec![c.id]),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .expect("edit");
        assert_eq!(result.fields, vec!["depends_on"]);
    }

    /// Phase 53 の監査: `genre` と食い違う `role` は作成時と同じく拒否する。
    #[test]
    fn a_role_outside_the_tasks_genre_is_rejected() {
        let store = SqliteStore::open_in_memory().expect("store");
        let mut task = task_with(Status::Ready);
        task.genre = Some("coding".into());
        let id = task.id;
        store.insert(&task).expect("insert");
        let genres = vec![task_core::GenreSpec {
            id: "coding".into(),
            description: "write code".into(),
            roles: vec!["implementer".into()],
            ..task_core::GenreSpec::default()
        }];

        let err = edit_task(
            &store,
            id,
            TaskEdit {
                role: Some(Some("literature-scout".into())),
                ..TaskEdit::default()
            },
            &genres,
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(
            matches!(err, OpsError::Validation(ref m) if m.contains("is not one of genre")),
            "{err}"
        );

        let result = edit_task(
            &store,
            id,
            TaskEdit {
                role: Some(Some("implementer".into())),
                ..TaskEdit::default()
            },
            &genres,
            OffsetDateTime::now_utc(),
        )
        .expect("edit");
        assert_eq!(result.fields, vec!["role"]);
    }

    /// Phase 53 の監査: 編集の最中にディスパッチャがリースを取っても、`status` / `attempts` /
    /// `lease` は**ストアがトランザクションの中で読んだ値**が残る（編集で run を殺さない）。
    #[test]
    fn an_edit_never_overwrites_the_status_attempts_or_lease() {
        let store = SqliteStore::open_in_memory().expect("store");
        let task = task_with(Status::Ready);
        let id = task.id;
        store.insert(&task).expect("insert");

        // 人が「ready のタスク」を読んで編集フォームを開く（この時点の写し）。
        let stale = store.get(id).expect("get").expect("task");
        assert_eq!(stale.status, Status::Ready);

        // その間にディスパッチャが dispatch してリースを取る。
        store
            .acquire_lease(id, "run-1", std::time::Duration::from_secs(60))
            .expect("lease");
        assert_eq!(
            store.get(id).expect("get").expect("task").status,
            Status::Running
        );

        // 古い写しを持ったまま編集しても、状態機械の 3 つは巻き戻らない。
        let result = edit_task(
            &store,
            id,
            TaskEdit {
                title: Some("編集した".into()),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .expect("edit");
        assert_eq!(result.task.status, Status::Running);
        assert!(result.task.lease.is_some());
        let after = store.get(id).expect("get").expect("task");
        assert_eq!(after.status, Status::Running, "json も running のまま");
        assert_eq!(after.title, "編集した");
        assert_eq!(
            after.lease.as_ref().map(|l| l.worker_run_id.as_str()),
            Some("run-1"),
            "リースを消さない"
        );
        // `ready_tasks` が見る列も running のまま（列と json が食い違わない）。
        assert!(store.ready_tasks(10).expect("ready").is_empty());
    }

    /// `expected_status` が現在と違えば 409。何も変えない編集はイベントを積まない。
    #[test]
    fn expected_status_conflicts_and_a_no_op_edit_records_nothing() {
        let store = SqliteStore::open_in_memory().expect("store");
        let task = task_with(Status::Ready);
        let id = task.id;
        store.insert(&task).expect("insert");
        let err = edit_task(
            &store,
            id,
            TaskEdit {
                title: Some("x".into()),
                expected_status: Some(Status::Running),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(matches!(err, OpsError::Conflict { .. }), "{err}");

        let result = edit_task(
            &store,
            id,
            TaskEdit {
                title: Some("t".into()),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .expect("no-op edit");
        assert!(result.fields.is_empty());
        assert!(store.events_for(id).expect("events").is_empty());
    }

    /// ADR-0043 D2 + ADR-0044 D1（Phase 52 + 53 のマージ）: `PATCH` の `repos` は
    /// **そのタスクの案件の中**から名前で引く（`POST /tasks` と同じ規則）。知らない名前は 422、
    /// 空配列は「リポジトリを使わない」、案件に属さないタスクの `repos` も 422。
    #[test]
    fn repos_are_resolved_by_name_within_the_tasks_project() {
        use task_core::{
            Project, ProjectId, ProjectRepo, ProjectStatus, RepoId, RepoKind, RepoRun,
        };

        let store = SqliteStore::open_in_memory().expect("store");
        let now = OffsetDateTime::now_utc();
        let project = Project {
            archived_at: None,
            paused_from: None,
            id: ProjectId::new(),
            title: "benchfs".into(),
            request: "複数リポジトリの案件".into(),
            status: ProjectStatus::Active,
            secretary_summary: None,
            workspace: None,
            created_at: now,
            updated_at: now,
        };
        store.project_create(&project).expect("project");
        let code = ProjectRepo {
            id: RepoId::new(),
            project_id: project.id,
            name: "benchfs".into(),
            kind: RepoKind::Git,
            location: WorkspaceSpec::local("/srv/benchfs"),
            default_branch: None,
            sync: None,
            run: RepoRun::Auto,
            is_primary: false,
            created_at: now,
        };
        store.repo_create(&code).expect("code repo");
        let paper = ProjectRepo {
            id: RepoId::new(),
            name: "benchfs-paper".into(),
            location: WorkspaceSpec::local("/srv/benchfs-paper"),
            ..code.clone()
        };
        store.repo_create(&paper).expect("paper repo");

        let mut task = task_with(Status::Ready);
        task.project_id = Some(project.id);
        let id = task.id;
        store.insert(&task).expect("insert");

        // 名前で差し替え → `fields` に `repos`、`Task.repos` が解決済みの参照になる。
        let result = edit_task(
            &store,
            id,
            TaskEdit {
                repos: Some(vec!["benchfs-paper".into(), "benchfs".into()]),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .expect("edit repos");
        assert_eq!(result.fields, vec!["repos".to_string()]);
        assert_eq!(
            result
                .task
                .repos
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["benchfs-paper", "benchfs"],
            "書いた順がそのまま（先頭が cwd）"
        );
        assert_eq!(result.task.repos[0].repo_id, paper.id);
        // 列と json の両方に残る。
        assert_eq!(store.get(id).expect("get").expect("task").repos.len(), 2);

        // 案件に無い名前は 422。
        let err = edit_task(
            &store,
            id,
            TaskEdit {
                repos: Some(vec!["unknown".into()]),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(matches!(err, OpsError::Validation(_)), "{err}");

        // 空配列は「リポジトリを使わない」（継承しない）。
        let cleared = edit_task(
            &store,
            id,
            TaskEdit {
                repos: Some(Vec::new()),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .expect("clear repos");
        assert_eq!(cleared.fields, vec!["repos".to_string()]);
        assert!(cleared.task.repos.is_empty());

        // 案件に属さないタスクの `repos` は 422。
        let orphan = task_with(Status::Ready);
        let orphan_id = orphan.id;
        store.insert(&orphan).expect("insert orphan");
        let err = edit_task(
            &store,
            orphan_id,
            TaskEdit {
                repos: Some(vec!["benchfs".into()]),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(matches!(err, OpsError::Validation(_)), "{err}");
    }

    // ---- ADR-0062 Phase 108: `PATCH /tasks/{id}` の `workspace` ----

    /// `cos` の下に `web-research`（道具なし）と `cluster-hpc`（`cluster:sirius` を持つ）。
    fn cluster_org() -> Vec<OrgNode> {
        let now = OffsetDateTime::now_utc();
        let dept = |id: &str, tools: &[&str]| OrgNode {
            profile: Profile {
                tools: tools.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            id: id.to_string(),
            parent_id: Some("cos".to_string()),
            name: id.to_string(),
            kind: OrgKind::Department,
            genre: None,
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        };
        vec![
            OrgNode {
                profile: Default::default(),
                id: "cos".into(),
                parent_id: None,
                name: "cos".into(),
                kind: OrgKind::Secretary,
                genre: None,
                brief: String::new(),
                position: 0,
                created_at: now,
                updated_at: now,
            },
            dept("web-research", &[]),
            dept("cluster-hpc", &["cluster:sirius"]),
        ]
    }

    /// (a) `ready` のタスクを `Local` に PATCH → 200、`workspace` が変わる。
    #[test]
    fn workspace_can_be_switched_to_local_on_a_ready_task() {
        let store = SqliteStore::open_in_memory().expect("store");
        let task = task_with(Status::Ready);
        let id = task.id;
        store.insert(&task).expect("insert");

        let result = edit_task(
            &store,
            id,
            TaskEdit {
                workspace: Some(WorkspaceSpec::local("/tmp/ws2")),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .expect("edit");
        assert_eq!(result.fields, vec!["workspace".to_string()]);
        assert_eq!(result.task.workspace, WorkspaceSpec::local("/tmp/ws2"));
        assert_eq!(
            store.get(id).expect("get").expect("task").workspace,
            WorkspaceSpec::local("/tmp/ws2")
        );
    }

    /// (b) B1（unroutable）で `blocked` になったタスクを `Local` に PATCH すると `ready` に戻り、
    /// 質問の approval が閉じる（既存の「質問に答える」経路に相乗り）。
    #[test]
    fn a_blocked_unroutable_task_returns_to_ready_when_the_workspace_resolves_it() {
        use task_core::approval::{Approval, ApprovalId, ApprovalStore};

        let store = SqliteStore::open_in_memory().expect("store");
        let mut task = task_with(Status::Ready);
        task.workspace = WorkspaceSpec::Remote {
            cluster: "sirius".into(),
            path: "~".into(),
            mode: None,
        };
        task.assignee = Some("web-research".into());
        let id = task.id;
        store.insert(&task).expect("insert");

        let question = "担当 `web-research` には道具 `cluster:sirius` が無いため…".to_string();
        store
            .apply_transition(
                id,
                Trigger::Unroutable,
                Some(Event::QuestionRaised {
                    run_id: format!("cluster-routing-{id}"),
                    text: question.clone(),
                }),
            )
            .expect("block");
        assert_eq!(
            store.get(id).expect("get").expect("task").status,
            Status::Blocked
        );
        let approval = Approval {
            id: ApprovalId::new(),
            project_id: None,
            node_id: "web-research".into(),
            task_id: Some(id),
            question: question.clone(),
            decision: None,
            answer: None,
            created_at: OffsetDateTime::now_utc(),
            decided_at: None,
        };
        store.approval_append(&approval).expect("append approval");

        let result = edit_task(
            &store,
            id,
            TaskEdit {
                workspace: Some(WorkspaceSpec::local("/tmp/ws-local")),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .expect("edit");
        assert_eq!(
            result.task.status,
            Status::Ready,
            "経路が通ったので ready に戻る"
        );
        assert_eq!(
            store.get(id).expect("get").expect("task").status,
            Status::Ready
        );
        let decided = store.approval_get(approval.id).expect("get").expect("some");
        assert!(!decided.is_pending(), "B1 の質問は解決済みとして閉じる");

        let events = store.events_for(id).expect("events");
        assert!(events.iter().any(|(_, e)| matches!(
            e,
            Event::Answered { answer, .. } if answer.contains("解決済み")
        )));
    }

    /// (c) 明示の `Remote` で担当（`web-research`）が `cluster:sirius` を持たなければ 422。
    #[test]
    fn an_explicit_remote_workspace_is_rejected_when_the_assignee_lacks_the_cluster_tool() {
        let store = SqliteStore::open_in_memory().expect("store");
        for node in cluster_org() {
            store.org_upsert(&node).expect("seed org");
        }
        let mut task = task_with(Status::Ready);
        task.assignee = Some("web-research".into());
        let id = task.id;
        store.insert(&task).expect("insert");

        let err = edit_task(
            &store,
            id,
            TaskEdit {
                workspace: Some(WorkspaceSpec::Remote {
                    cluster: "sirius".into(),
                    path: "~".into(),
                    mode: None,
                }),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        let OpsError::Validation(msg) = err else {
            panic!("{err}");
        };
        assert!(msg.contains("cluster:sirius"), "{msg}");
        assert!(msg.contains("cluster-hpc"), "候補ノードを挙げる: {msg}");
        assert_eq!(
            store.get(id).expect("get").expect("task").workspace,
            WorkspaceSpec::local("/tmp/ws"),
            "検証に落ちたので何も書かない"
        );
    }

    /// (d) `running` への `workspace` 編集は 409（`draft`/`ready`/`blocked`/`failed` だけ許す）。
    #[test]
    fn workspace_edits_are_refused_on_running_tasks() {
        let store = SqliteStore::open_in_memory().expect("store");
        let task = task_with(Status::Running);
        let id = task.id;
        store.insert(&task).expect("insert");

        let err = edit_task(
            &store,
            id,
            TaskEdit {
                workspace: Some(WorkspaceSpec::local("/tmp/ws2")),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .unwrap_err();
        assert!(matches!(err, OpsError::InvalidState { .. }), "{err}");
        assert_eq!(
            store.get(id).expect("get").expect("task").workspace,
            WorkspaceSpec::local("/tmp/ws")
        );
    }

    /// (e) `assignee` を `cluster-hpc`（`cluster:sirius` を持つ）に変えつつ `Remote` のまま → 200。
    #[test]
    fn changing_the_assignee_to_a_node_with_the_cluster_tool_is_accepted() {
        let store = SqliteStore::open_in_memory().expect("store");
        for node in cluster_org() {
            store.org_upsert(&node).expect("seed org");
        }
        let mut task = task_with(Status::Ready);
        task.workspace = WorkspaceSpec::Remote {
            cluster: "sirius".into(),
            path: "~".into(),
            mode: None,
        };
        let id = task.id;
        store.insert(&task).expect("insert");

        let result = edit_task(
            &store,
            id,
            TaskEdit {
                assignee: Some(Some("cluster-hpc".into())),
                ..TaskEdit::default()
            },
            &[],
            OffsetDateTime::now_utc(),
        )
        .expect("edit");
        assert_eq!(result.fields, vec!["assignee".to_string()]);
        assert_eq!(result.task.assignee.as_deref(), Some("cluster-hpc"));
    }
}
