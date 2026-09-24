//! 実行中の委譲（ADR-0016 D2 / M6 / M7）。ワーカーが `delegate` メッセージ（LLM アダプタは `artifacts/delegate.json`）で
//! 提案する子タスクの型と、ストアを見ない純粋な検証・組み立て。ストアを見る検証（既存 ID の依存、祖先、木の深さ・run 数）は
//! `task-ops::delegate` にある。I/O・LLM 呼び出しは無い。

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::model::{
    Criterion, GenreSpec, RoleSpec, Status, Task, TaskId, TaskKind, Tier, WorkerHint, WorkspaceSpec,
};
use crate::org::OrgNode;

/// `delegate.tasks[].depends_on[]` の 1 要素（ADR-0016 M7）: 同じ配列内のインデックス（整数）か、既存タスクの ID（文字列）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum DelegateDep {
    Index(usize),
    Id(String),
}

/// ワーカーが提案する子タスク 1 件。未知フィールドは拒否する（Plan の `NewTask` と同じ方針）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateTask {
    pub title: String,
    pub objective: String,
    /// 1 件以上。
    pub acceptance: Vec<Criterion>,
    /// 役割名（任意。`[[roles]]` にあれば既定と指示文が効く）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// ADR-0027 D1: 分野名（任意）。未指定なら `role` の分野（一意なら）→ 親の分野の順で継ぐ。
    /// `role` と両方指定したときは `role` がその分野の `roles` に含まれること（含まれなければ設定エラー）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<DelegateDep>,
    /// 省略時は役割の既定 → 親の tier。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// ADR-0033 D4（Phase 24）: 割り当てる組織のノード（`org_nodes.id`）。「誰の仕事か」を表す。
    /// `role` を書かなかったときだけ、そのノードの分野が既定（tier / adapter / 予算）の解決に使われる
    /// （タスクの値 > 役割の既定 > 分野の既定 > 親の値。ADR-0016 D1 の優先順に合わせる）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// ADR-0039 D2: この子の作業場所（任意）。**書かなければ案件の作業場所 → 親の workspace** を継ぐので、
    /// 別の場所（別のリポジトリ・別のクラスタ）で作業させたいときにだけ書く。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceSpec>,
}

/// 委譲の上限（ADR-0016 D2。既定 8 / 5 / 100）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DelegationLimits {
    /// 1 run あたりの件数（複数の `delegate` をまたいで数える）。
    pub max_delegate_per_run: usize,
    /// 木の深さ（根 = 1）。
    pub max_tree_depth: u32,
    /// 木全体のワーカー run 数。
    pub max_tree_runs: u32,
    /// ADR-0021 D4: 委譲した子が失敗したときの親の扱い。
    pub on_child_failure: OnChildFailure,
}

/// ADR-0021: 委譲した子が `failed` になったとき、親をどうするか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnChildFailure {
    /// 既定: 親をやり直す（attempts 消費）。やり直せないなら人間に質問して待つ（`blocked`）。**親を failed にはしない**。
    #[default]
    RetryThenAsk,
    /// ADR-0016 M5 までの挙動: 子の失敗を見ずに親を完了させる。
    Ignore,
}

impl Default for DelegationLimits {
    fn default() -> Self {
        Self {
            max_delegate_per_run: 8,
            max_tree_depth: 5,
            max_tree_runs: 100,
            on_child_failure: OnChildFailure::RetryThenAsk,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DelegateError {
    #[error("tasks[{index}].{field} must not be empty")]
    EmptyField { index: usize, field: &'static str },
    #[error("tasks[{index}].acceptance must have at least one criterion")]
    NoAcceptance { index: usize },
    #[error("tasks[{index}].acceptance[{criterion}].text must not be empty")]
    EmptyCriterion { index: usize, criterion: usize },
    /// ADR-0067 D2: `human` チェックには `artifact_exists`/`knowledge_page` の参照が要る。
    #[error("tasks[{index}]: {reason}")]
    NoHumanDeliverable { index: usize, reason: String },
    #[error("tasks[{index}].depends_on[{position}] = {target} is out of range (0..{len})")]
    DependencyOutOfRange {
        index: usize,
        position: usize,
        target: usize,
        len: usize,
    },
    #[error("tasks[{index}] depends on itself")]
    SelfDependency { index: usize },
    #[error("dependency cycle involving tasks[{index}]")]
    Cycle { index: usize },
    #[error("tasks[{index}].depends_on[{position}] = {id:?} is not a task id")]
    InvalidId {
        index: usize,
        position: usize,
        id: String,
    },
    /// ADR-0027 D1: 知らない `genre`。
    #[error("tasks[{index}].genre {genre:?} is not a known genre")]
    UnknownGenre { index: usize, genre: String },
    /// ADR-0027 D1: `genre` と `role` を両方指定したが、`role` がその分野の `roles` に含まれない。
    #[error("tasks[{index}].role {role:?} is not one of genre {genre:?}'s roles")]
    RoleNotInGenre {
        index: usize,
        role: String,
        genre: String,
    },
}

/// ストアを見ない検証（ADR-0016 M7 / ADR-0027 D1）: 空欄、配列内インデックスの範囲・自己参照・閉路、
/// ID の書式、`genre` が知っている分野か、`genre` と `role` を両方指定したときの整合。1 件ごとに結果を
/// 返す（通ったものだけを挿入するため）。閉路は関係する全ての要素を不合格にする。`genres` が空（分野を
/// 使わない設定）なら `genre` を指定した提案は全て `UnknownGenre` になる。
pub fn validate_each(
    tasks: &[DelegateTask],
    genres: &[GenreSpec],
) -> Vec<Result<(), DelegateError>> {
    let len = tasks.len();
    let mut results: Vec<Result<(), DelegateError>> = tasks
        .iter()
        .enumerate()
        .map(|(index, t)| validate_one(index, t, len, genres))
        .collect();
    // 閉路検出（三色法）。インデックス依存だけを辿る。
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Grey,
        Black,
    }
    let mut color = vec![Color::White; len];
    for start in 0..len {
        if color[start] != Color::White {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        color[start] = Color::Grey;
        while let Some(&mut (node, ref mut pos)) = stack.last_mut() {
            let deps = &tasks[node].depends_on;
            if *pos < deps.len() {
                let dep = &deps[*pos];
                *pos += 1;
                let DelegateDep::Index(child) = dep else {
                    continue;
                };
                if *child >= len || *child == node {
                    continue; // 範囲外・自己参照は validate_one が既に不合格にしている
                }
                match color[*child] {
                    Color::White => {
                        color[*child] = Color::Grey;
                        stack.push((*child, 0));
                    }
                    Color::Grey => {
                        // スタック上の child 以降が閉路。
                        let from = stack.iter().position(|(n, _)| n == child).unwrap_or(0);
                        for (n, _) in &stack[from..] {
                            if results[*n].is_ok() {
                                results[*n] = Err(DelegateError::Cycle { index: *n });
                            }
                        }
                    }
                    Color::Black => {}
                }
            } else {
                color[node] = Color::Black;
                stack.pop();
            }
        }
    }
    results
}

fn validate_one(
    index: usize,
    t: &DelegateTask,
    len: usize,
    genres: &[GenreSpec],
) -> Result<(), DelegateError> {
    if let Some(genre) = &t.genre {
        let Some(spec) = GenreSpec::find(genres, genre) else {
            return Err(DelegateError::UnknownGenre {
                index,
                genre: genre.clone(),
            });
        };
        if let Some(role) = &t.role
            && !spec.roles.iter().any(|r| r == role)
        {
            return Err(DelegateError::RoleNotInGenre {
                index,
                role: role.clone(),
                genre: genre.clone(),
            });
        }
    }
    if t.title.trim().is_empty() {
        return Err(DelegateError::EmptyField {
            index,
            field: "title",
        });
    }
    if t.objective.trim().is_empty() {
        return Err(DelegateError::EmptyField {
            index,
            field: "objective",
        });
    }
    if t.acceptance.is_empty() {
        return Err(DelegateError::NoAcceptance { index });
    }
    for (criterion, c) in t.acceptance.iter().enumerate() {
        if c.text.trim().is_empty() {
            return Err(DelegateError::EmptyCriterion { index, criterion });
        }
    }
    // ADR-0067 D2: `human` チェックを持つなら、成果物か知識ベースの参照が要る。
    if let Err(reason) = crate::model::validate_human_checks_have_deliverable(&t.acceptance) {
        return Err(DelegateError::NoHumanDeliverable { index, reason });
    }
    for (position, dep) in t.depends_on.iter().enumerate() {
        match dep {
            DelegateDep::Index(target) => {
                if *target >= len {
                    return Err(DelegateError::DependencyOutOfRange {
                        index,
                        position,
                        target: *target,
                        len,
                    });
                }
                if *target == index {
                    return Err(DelegateError::SelfDependency { index });
                }
            }
            DelegateDep::Id(id) => {
                if id.parse::<TaskId>().is_err() {
                    return Err(DelegateError::InvalidId {
                        index,
                        position,
                        id: id.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// `resolve_child_defaults` に渡す、子タスクが明示した値（ADR-0016 D1 / ADR-0027 D1 / ADR-0033 D4）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChildSpec<'a> {
    /// 明示された分野（`delegate.json` / `plan.json` の `genre`）。
    pub genre: Option<&'a str>,
    /// 明示された役割（`role`）。**書いてあれば tier / adapter / 予算はこれが勝つ**（ADR-0016 D1）。
    pub role: Option<&'a str>,
    pub tier: Option<Tier>,
    /// ADR-0033 D4: 割り当てる組織のノード。`role` が無いときだけ既定の解決に効く。
    pub assignee: Option<&'a str>,
}

/// ADR-0039 D2: 子タスク（分解・委譲）の**作業場所**を決めるための文脈。`Default`（案件の作業場所も
/// `$HOME` も無し）なら Phase 42 までと同じ挙動（子は親の workspace を継ぐ）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkspaceContext<'a> {
    /// その子が属する案件の作業場所（`projects.workspace` = primary のリポジトリの写し）。
    /// 決めていない案件では `None`。
    pub project: Option<&'a WorkspaceSpec>,
    /// `~` の展開に使う `$HOME`（ADR-0039 D5。`Local` のパスにだけ効く）。
    pub home: Option<&'a std::path::Path>,
    /// ADR-0043 D1 / D2: その案件のリポジトリ（primary が先頭。`repo_list` の順）。
    /// 子の `repos` の継承（明示 > 親 > 案件の primary）に使う。
    pub repos: &'a [crate::repos::ProjectRepo],
}

impl WorkspaceContext<'_> {
    /// 子 1 件の作業場所: **タスクが明示 > 案件の workspace > 親の workspace（従来）**（ADR-0039 D2）。
    /// 明示・案件のどちらから来た `Local` のパスも `~` を展開する（D5。`Remote` の `~` はクラスタ側の
    /// home なので触らない）。親から継いだときは（既に展開済みの値なので）そのまま。
    pub fn child_workspace(
        &self,
        parent: &Task,
        explicit: Option<&WorkspaceSpec>,
    ) -> WorkspaceSpec {
        match explicit.or(self.project) {
            Some(spec) => spec.with_home_expanded(self.home),
            None => parent.workspace.clone(),
        }
    }

    /// ADR-0043 D2: 子 1 件が使うリポジトリ: **明示（名前）> 親 > 案件の primary**。
    /// 知らない名前は無視する（検証は `validate`（計画）と API（`POST /tasks`）が先に済ませる）。
    pub fn child_repos(&self, parent: &Task, explicit: &[String]) -> Vec<crate::repos::RepoRef> {
        if !explicit.is_empty() {
            let picked: Vec<crate::repos::RepoRef> = explicit
                .iter()
                .filter_map(|name| self.repos.iter().find(|r| r.name == name.trim()))
                .map(crate::repos::RepoRef::of)
                .collect();
            if !picked.is_empty() {
                return picked;
            }
        }
        if !parent.repos.is_empty() {
            return parent.repos.clone();
        }
        self.repos
            .iter()
            .find(|r| r.is_primary)
            .map(|r| vec![crate::repos::RepoRef::of(r)])
            .unwrap_or_default()
    }
}

/// `resolve_child_defaults` が決めた、子タスクの分野・担当・tier・アダプタ・budget の既定
/// （ADR-0016 D1 / ADR-0027 D1 / ADR-0028 D3 / ADR-0033 D4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedChildDefaults {
    pub genre: Option<String>,
    /// 子に記録する担当。ADR-0069 D1（Phase 114）で**常に `None`**（LLM が書いた担当は捨て、
    /// ADR-0046 D5 の matching が決める）。
    pub assignee: Option<String>,
    /// ADR-0069 D1: LLM（計画・委譲）が書いたが捨てた担当（監査用。`Task.routing.dropped_assignee`）。
    pub dropped_assignee: Option<String>,
    /// ADR-0069 D1: `tier` の出自（計画・委譲が書いた tier はヒント）。
    pub tier_source: crate::model::TierSource,
    pub tier: Tier,
    pub adapter: Option<String>,
    pub max_turns: u32,
    pub max_wall_secs: u64,
}

/// 子タスクの分野と `tier` / `adapter` / `budget` の既定を決める（ADR-0016 D1, ADR-0027 D1, ADR-0028 D3）。
/// 委譲（`materialize_delegated`）と Planner（`plan::materialize`）の両方が使う共通の決め方:
/// - 分野: `explicit_genre`（明示）> `role_id` が一意に属する分野 > `assignee` のノードの分野 > `parent.genre`。
/// - `tier` / `adapter` / `max_turns` / `max_wall_secs`: タスクの値（`task_tier`。budget 側は呼び出し元に
///   フィールドが無いので常に次点から） > 役割（`role_id`）の既定 > 分野の既定（決めた分野の `default_role`
///   の役割）の既定 > 親の値。
pub fn resolve_child_defaults(
    parent: &Task,
    child: ChildSpec<'_>,
    org: &[OrgNode],
    roles: &[RoleSpec],
    genres: &[GenreSpec],
) -> ResolvedChildDefaults {
    let ChildSpec {
        genre: explicit_genre,
        role: role_id,
        tier: task_tier,
        assignee,
    } = child;
    let role = role_id.and_then(|r| RoleSpec::find(roles, r));
    // ADR-0069 D1（Phase 114）: 計画・委譲（LLM）が書いた担当は**捨てる**（ADR-0046 D5 の「LLM が
    // 人選する経路は無くす」を実装で強制する。以前は組織にある id ならそのまま子に記録していた）。
    // 捨てた値は監査のために `dropped_assignee` に残す。担当はディスパッチャの matching が決める。
    let _ = org;
    let dropped_assignee = assignee
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string);
    let assignee: Option<String> = None;
    // 分野: 明示 > `role` が一意に属する分野 > 親の分野。
    let genre = explicit_genre
        .map(str::to_string)
        .or_else(|| role_id.and_then(|r| GenreSpec::unique_for_role(genres, r)))
        .or_else(|| parent.genre.clone());
    let tier_source = if task_tier.is_some() {
        crate::model::TierSource::Hint
    } else {
        crate::model::TierSource::Default
    };
    let genre_role = genre
        .as_deref()
        .and_then(|g| GenreSpec::find(genres, g))
        .and_then(|g| g.default_role.as_deref())
        .and_then(|r| RoleSpec::find(roles, r));
    ResolvedChildDefaults {
        tier: task_tier
            .or(role.and_then(|r| r.tier))
            .or(genre_role.and_then(|r| r.tier))
            .unwrap_or(parent.worker_hint.tier),
        adapter: role
            .and_then(|r| r.adapter.clone())
            .or_else(|| genre_role.and_then(|r| r.adapter.clone()))
            .or_else(|| parent.worker_hint.adapter.clone()),
        max_turns: role
            .and_then(|r| r.max_turns)
            .or_else(|| genre_role.and_then(|r| r.max_turns))
            .unwrap_or(parent.budget.max_turns),
        max_wall_secs: role
            .and_then(|r| r.max_wall_secs)
            .or_else(|| genre_role.and_then(|r| r.max_wall_secs))
            .unwrap_or(parent.budget.max_wall_secs),
        genre,
        assignee,
        dropped_assignee,
        tier_source,
    }
}

/// ADR-0062 B2（Phase 107）: 継承した（そのタスク自身は明示していない）Remote の workspace を、
/// 担当が `cluster:<id>` を持たない場合は専用の Local（`workspace_root/<task_id>`）に落とす。
/// 明示された workspace（`was_explicit == true`）はここでは触らない（検証で別途拒否する経路。
/// `task_ops::actions::create_task_action` / matching の候補フィルタ）。
/// `assignee` が未定（matching に任せる）なら、その場では判定せず Remote のまま返す
/// （ADR-0046 D5 の matching が `cluster:<id>` を持つノードだけを候補にするので、後で矛盾しない）。
pub(crate) fn downgrade_inherited_remote_if_needed(
    workspace: WorkspaceSpec,
    was_explicit: bool,
    assignee: Option<&str>,
    org: &[OrgNode],
    task_id: TaskId,
) -> (WorkspaceSpec, Option<String>) {
    if was_explicit {
        return (workspace, None);
    }
    let WorkspaceSpec::Remote { cluster, .. } = &workspace else {
        return (workspace, None);
    };
    let Some(assignee) = assignee else {
        return (workspace, None);
    };
    let effective = crate::profile::resolve(org, assignee);
    let wanted = format!("{}{cluster}", crate::profile::CLUSTER_TOOL_PREFIX);
    if effective.has_tool(&wanted) {
        return (workspace, None);
    }
    let reason = format!("workspace inherited as local: assignee {assignee} has no {wanted}");
    (
        WorkspaceSpec::Local {
            path: std::path::PathBuf::from(task_id.to_string()),
            mode: None,
        },
        Some(reason),
    )
}

/// 検証を通った提案（`accepted` はインデックス）から子タスクを組み立てる（ADR-0016 M2 / M3, ADR-0027 D1）。
/// `tier` / `adapter` / `budget` はタスクの値 > 役割の既定 > 分野の既定（`default_role` の役割）> 親の値。
/// 子の分野は `genre` > `role` の分野（`roles` に含む分野がちょうど 1 つのとき）> 親の分野の順で決める
/// （分野の既定は tier/adapter/budget の穴埋めにだけ使い、`role` そのものは書き換えない）。
/// 配列内インデックスの依存は、相手も `accepted` に入っているときだけ ID に写す（不合格の相手への依存は落とす）。
/// ID の依存は呼び出し側（task-ops）が検証済みのものだけを残して渡すこと。`tasks[accepted[..]]` は
/// `validate_each` を通った（= `genre`/`role` の整合が取れた）ものだけを渡すこと。
/// ADR-0039 D2: `workspace` は子の作業場所の文脈（案件の workspace と `$HOME`）。
#[allow(clippy::too_many_arguments)]
pub fn materialize_delegated(
    parent: &Task,
    tasks: &[DelegateTask],
    accepted: &[usize],
    org: &[OrgNode],
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    workspace: WorkspaceContext<'_>,
    now: OffsetDateTime,
) -> Vec<Task> {
    materialize_delegated_logging(
        parent,
        tasks,
        accepted,
        org,
        roles,
        genres,
        workspace,
        now,
        &mut |_, _| {},
    )
}

/// [`materialize_delegated`] と同じだが、ADR-0062 B2 の「Remote → Local への降格」が起きるたびに
/// `on_downgrade(task_id, reason)` を呼ぶ（呼び出し側が tracing で 1 回だけログに残すため）。
#[allow(clippy::too_many_arguments)]
pub fn materialize_delegated_logging(
    parent: &Task,
    tasks: &[DelegateTask],
    accepted: &[usize],
    org: &[OrgNode],
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    workspace: WorkspaceContext<'_>,
    now: OffsetDateTime,
    on_downgrade: &mut dyn FnMut(TaskId, &str),
) -> Vec<Task> {
    let ids: HashMap<usize, TaskId> = accepted.iter().map(|&i| (i, TaskId::new())).collect();
    accepted
        .iter()
        .map(|&i| {
            let t = &tasks[i];
            let defaults = resolve_child_defaults(
                parent,
                ChildSpec {
                    genre: t.genre.as_deref(),
                    role: t.role.as_deref(),
                    tier: t.tier,
                    assignee: t.assignee.as_deref(),
                },
                org,
                roles,
                genres,
            );
            let depends_on: Vec<TaskId> = t
                .depends_on
                .iter()
                .filter_map(|d| match d {
                    DelegateDep::Index(j) => ids.get(j).copied(),
                    DelegateDep::Id(s) => s.parse::<TaskId>().ok(),
                })
                .collect();
            let mut budget = parent.budget;
            budget.max_turns = defaults.max_turns;
            budget.max_wall_secs = defaults.max_wall_secs;
            Task {
                id: ids[&i],
                parent_id: Some(parent.id),
                kind: TaskKind::Execute,
                title: t.title.clone(),
                objective: t.objective.clone(),
                acceptance: t.acceptance.clone(),
                inputs: vec![],
                depends_on,
                status: Status::Draft,
                priority: parent.priority,
                // ADR-0069 D1: 委譲の子は lane policy の対象（tier はヒント、捨てた担当を記録）。
                routing: Some(crate::model::TaskRouting {
                    tier_source: defaults.tier_source,
                    dropped_assignee: defaults.dropped_assignee.clone(),
                    ..Default::default()
                }),
                worker_hint: WorkerHint {
                    tier: defaults.tier,
                    adapter: defaults.adapter,
                },
                // ADR-0039 D2: 明示 > 案件の workspace > 親の workspace（従来）。
                // ADR-0062 B2: 継承した Remote は、担当が cluster:<id> を持たなければ Local に落とす。
                workspace: {
                    let raw = workspace.child_workspace(parent, t.workspace.as_ref());
                    let (ws, reason) = downgrade_inherited_remote_if_needed(
                        raw,
                        t.workspace.is_some(),
                        defaults.assignee.as_deref(),
                        org,
                        ids[&i],
                    );
                    if let Some(reason) = reason {
                        on_downgrade(ids[&i], &reason);
                    }
                    ws
                },
                // ADR-0043 D2: 委譲の子は親のリポジトリを継ぐ（親が持たなければ案件の primary）。
                repos: workspace.child_repos(parent, &[]),
                budget,
                attempts: 0,
                lease: None,
                created_at: now,
                updated_at: now,
                role: t.role.clone(),
                genre: defaults.genre,
                aggregate: false,
                // ADR-0033 D2: 案件の仕事の木は `tasks WHERE project_id = ?` なので、分解した子も
                // 同じ案件・途中目標に属する（担当は Phase 24 で計画が指定するまで空）。
                project_id: parent.project_id,
                milestone_id: parent.milestone_id,
                assignee: defaults.assignee,
                conversation: None,
                labels: Vec::new(),
                category: Default::default(),
                // ADR-0046 D2 / D4（Phase 59）: 委譲の子は親の進め方を継ぐ。必要な能力タグは
                // 委譲の提案には無い（親が明示の担当を決めるか、親の担当がそのまま受ける）。
                skills: Vec::new(),
                mode: parent.mode,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Budget, Check, WorkspaceSpec};
    use std::path::PathBuf;

    fn dt(title: &str, deps: Vec<DelegateDep>) -> DelegateTask {
        DelegateTask {
            title: title.into(),
            objective: format!("do {title}"),
            acceptance: vec![Criterion {
                text: "c".into(),
                check: Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            }],
            role: None,
            genre: None,
            depends_on: deps,
            tier: None,
            assignee: None,
            workspace: None,
        }
    }

    fn parent() -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "p".into(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Running,
            priority: 3,
            worker_hint: WorkerHint {
                tier: Tier::Frontier,
                adapter: Some("fake".into()),
            },
            workspace: WorkspaceSpec::Local {
                path: PathBuf::from("/tmp/ws"),
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
            role: Some("lead".into()),
            genre: Some("coding".into()),
            aggregate: true,
            project_id: None,
            milestone_id: None,
            assignee: None,
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    #[test]
    fn depends_on_accepts_index_or_id_and_rejects_other_shapes() {
        let json = r#"{"title":"a","objective":"o","acceptance":[{"text":"c","check":{"type":"human"}}],
            "depends_on":[0,"01J9ZX5T3K8Q7W6V5R4P3N2M1H"]}"#;
        let t: DelegateTask = serde_json::from_str(json).unwrap();
        assert_eq!(t.depends_on[0], DelegateDep::Index(0));
        assert_eq!(
            t.depends_on[1],
            DelegateDep::Id("01J9ZX5T3K8Q7W6V5R4P3N2M1H".into())
        );
        assert!(
            serde_json::from_str::<DelegateTask>(
                r#"{"title":"a","objective":"o","acceptance":[],"bogus":1}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<DelegateTask>(
                r#"{"title":"a","objective":"o","acceptance":[],"depends_on":[true]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn validate_each_reports_per_item_and_marks_cycles() {
        let tasks = vec![
            dt("ok", vec![]),
            dt("", vec![]),
            dt("self", vec![DelegateDep::Index(2)]),
            dt("range", vec![DelegateDep::Index(9)]),
            dt("cyc-a", vec![DelegateDep::Index(5)]),
            dt("cyc-b", vec![DelegateDep::Index(4)]),
            dt("bad-id", vec![DelegateDep::Id("nope".into())]),
            dt("dep-ok", vec![DelegateDep::Index(0)]),
        ];
        let r = validate_each(&tasks, &[]);
        assert!(r[0].is_ok());
        assert_eq!(
            r[1],
            Err(DelegateError::EmptyField {
                index: 1,
                field: "title"
            })
        );
        assert_eq!(r[2], Err(DelegateError::SelfDependency { index: 2 }));
        assert!(matches!(
            r[3],
            Err(DelegateError::DependencyOutOfRange {
                index: 3,
                target: 9,
                len: 8,
                ..
            })
        ));
        assert!(
            matches!(r[4], Err(DelegateError::Cycle { .. })),
            "{:?}",
            r[4]
        );
        assert!(
            matches!(r[5], Err(DelegateError::Cycle { .. })),
            "{:?}",
            r[5]
        );
        assert!(matches!(
            r[6],
            Err(DelegateError::InvalidId { index: 6, .. })
        ));
        assert!(r[7].is_ok());
        let mut no_acc = dt("x", vec![]);
        no_acc.acceptance.clear();
        assert_eq!(
            validate_each(&[no_acc], &[])[0],
            Err(DelegateError::NoAcceptance { index: 0 })
        );
    }

    fn genre(id: &str, default_role: Option<&str>, roles: &[&str]) -> GenreSpec {
        GenreSpec {
            id: id.into(),
            description: format!("{id} description"),
            default_role: default_role.map(str::to_string),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            ..GenreSpec::default()
        }
    }

    /// ADR-0027 D1: `genre` が知らない id なら `UnknownGenre`。`genre` + `role` で `role` がその分野の
    /// `roles` に無ければ `RoleNotInGenre`。分野を使わない設定（`genres` が空）でも同じ扱い。
    #[test]
    fn validate_each_rejects_unknown_genre_and_role_not_in_genre() {
        let genres = vec![genre(
            "coding",
            Some("implementer"),
            &["lead", "implementer"],
        )];
        let mut unknown = dt("a", vec![]);
        unknown.genre = Some("literature".into());
        let mut mismatched = dt("b", vec![]);
        mismatched.genre = Some("coding".into());
        mismatched.role = Some("literature-scout".into());
        let mut ok = dt("c", vec![]);
        ok.genre = Some("coding".into());
        ok.role = Some("implementer".into());
        let r = validate_each(&[unknown, mismatched, ok], &genres);
        assert_eq!(
            r[0],
            Err(DelegateError::UnknownGenre {
                index: 0,
                genre: "literature".into()
            })
        );
        assert_eq!(
            r[1],
            Err(DelegateError::RoleNotInGenre {
                index: 1,
                role: "literature-scout".into(),
                genre: "coding".into(),
            })
        );
        assert!(r[2].is_ok());

        // 分野を使わない設定（`[[genres]]` が無い）でも `genre` を指定すれば同じくエラー。
        let mut no_config = dt("d", vec![]);
        no_config.genre = Some("coding".into());
        assert_eq!(
            validate_each(&[no_config], &[])[0],
            Err(DelegateError::UnknownGenre {
                index: 0,
                genre: "coding".into()
            })
        );
    }

    #[test]
    fn materialize_applies_role_defaults_and_maps_index_dependencies_of_accepted_only() {
        let p = parent();
        let roles = vec![RoleSpec {
            id: "implementer".into(),
            tier: Some(Tier::Cheap),
            adapter: Some("codex".into()),
            max_turns: Some(3),
            max_wall_secs: None,
            instructions: Some("implement".into()),
        }];
        let mut a = dt("a", vec![]);
        a.role = Some("implementer".into());
        let b = dt("b", vec![DelegateDep::Index(0), DelegateDep::Index(2)]);
        let mut c = dt("c", vec![]);
        c.tier = Some(Tier::Standard);
        c.role = Some("implementer".into());
        let tasks = vec![a, b, c];
        let out = materialize_delegated(
            &p,
            &tasks,
            &[0, 1],
            &[],
            &roles,
            &[],
            WorkspaceContext::default(),
            OffsetDateTime::now_utc(),
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].parent_id, Some(p.id));
        assert_eq!(out[0].status, Status::Draft);
        assert_eq!(out[0].role.as_deref(), Some("implementer"));
        assert_eq!(out[0].worker_hint.tier, Tier::Cheap);
        assert_eq!(out[0].worker_hint.adapter.as_deref(), Some("codex"));
        assert_eq!(out[0].budget.max_turns, 3);
        assert_eq!(out[0].budget.max_wall_secs, 600);
        assert_eq!(out[0].priority, 3);
        assert!(!out[0].aggregate);
        // 分野の設定が無くても、role/genre 共に未指定な子は親の分野をそのまま継ぐ。
        assert_eq!(out[0].genre.as_deref(), Some("coding"));
        // b は a（採用）だけに依存し、c（不採用）への依存は落ちる。役割無しは親の tier / adapter。
        assert_eq!(out[1].depends_on, vec![out[0].id]);
        assert_eq!(out[1].worker_hint.tier, Tier::Frontier);
        assert_eq!(out[1].worker_hint.adapter.as_deref(), Some("fake"));
        // タスクの tier は役割の既定より優先。
        let out = materialize_delegated(
            &p,
            &tasks,
            &[2],
            &[],
            &roles,
            &[],
            WorkspaceContext::default(),
            OffsetDateTime::now_utc(),
        );
        assert_eq!(out[0].worker_hint.tier, Tier::Standard);
    }

    /// ADR-0027 D1: 子の分野は `genre` の明示 > `role` の分野（一意なら） > 親の分野の順。
    #[test]
    fn materialize_resolves_genre_by_precedence() {
        let p = parent(); // genre = Some("coding")
        let genres = vec![
            genre("coding", Some("implementer"), &["lead", "implementer"]),
            genre(
                "literature",
                Some("literature-reader"),
                &["literature-scout", "literature-reader"],
            ),
        ];

        // 1. 明示した genre が最優先（role の分野や親の分野より勝つ）。
        let mut explicit = dt("explicit", vec![]);
        explicit.role = Some("implementer".into()); // coding の役割
        explicit.genre = Some("literature".into());
        let out = materialize_delegated(
            &p,
            &[explicit],
            &[0],
            &[],
            &[],
            &genres,
            WorkspaceContext::default(),
            OffsetDateTime::now_utc(),
        );
        assert_eq!(out[0].genre.as_deref(), Some("literature"));

        // 2. genre 未指定・role がちょうど 1 つの分野に属する: その分野を継ぐ。
        let mut by_role = dt("by-role", vec![]);
        by_role.role = Some("literature-scout".into());
        let out = materialize_delegated(
            &p,
            &[by_role],
            &[0],
            &[],
            &[],
            &genres,
            WorkspaceContext::default(),
            OffsetDateTime::now_utc(),
        );
        assert_eq!(out[0].genre.as_deref(), Some("literature"));

        // 3. role が複数の分野に属する（一意に決まらない）: 親の分野を継ぐ。
        let ambiguous_genres = vec![
            genre("coding", None, &["shared"]),
            genre("literature", None, &["shared"]),
        ];
        let mut ambiguous = dt("ambiguous", vec![]);
        ambiguous.role = Some("shared".into());
        let out = materialize_delegated(
            &p,
            &[ambiguous],
            &[0],
            &[],
            &[],
            &ambiguous_genres,
            WorkspaceContext::default(),
            OffsetDateTime::now_utc(),
        );
        assert_eq!(
            out[0].genre.as_deref(),
            Some("coding"),
            "falls back to the parent's genre"
        );

        // 4. genre も role も無い: 親の分野を継ぐ。
        let none_of_either = dt("neither", vec![]);
        let out = materialize_delegated(
            &p,
            &[none_of_either],
            &[0],
            &[],
            &[],
            &genres,
            WorkspaceContext::default(),
            OffsetDateTime::now_utc(),
        );
        assert_eq!(out[0].genre.as_deref(), Some("coding"));
    }

    /// ADR-0027 D1: role 無しで genre だけ指定した子は、その分野の `default_role` の既定
    /// （tier / adapter / budget）を借りる（「genre だけ」のケース）。
    #[test]
    fn materialize_applies_genre_default_role_when_task_has_no_role() {
        let p = parent();
        let roles = vec![RoleSpec {
            id: "literature-reader".into(),
            tier: Some(Tier::Standard),
            adapter: Some("acp".into()),
            max_turns: Some(5),
            max_wall_secs: Some(1200),
            instructions: None,
        }];
        let genres = vec![genre(
            "literature",
            Some("literature-reader"),
            &["literature-reader"],
        )];
        let mut t = dt("investigate", vec![]);
        t.genre = Some("literature".into());
        let out = materialize_delegated(
            &p,
            &[t],
            &[0],
            &[],
            &roles,
            &genres,
            WorkspaceContext::default(),
            OffsetDateTime::now_utc(),
        );
        assert_eq!(
            out[0].role, None,
            "genre alone must not set the task's role"
        );
        assert_eq!(out[0].genre.as_deref(), Some("literature"));
        assert_eq!(out[0].worker_hint.tier, Tier::Standard);
        assert_eq!(out[0].worker_hint.adapter.as_deref(), Some("acp"));
        assert_eq!(out[0].budget.max_turns, 5);
        assert_eq!(out[0].budget.max_wall_secs, 1200);
    }

    /// ADR-0027 D1 の決め方の順（tier/adapter/budget）: タスクの値 > 役割の既定 > 分野の既定 > 親の値。
    /// 役割自身に既定が無いフィールドだけ、分野の既定（`default_role` の役割）が埋める。
    #[test]
    fn materialize_role_default_wins_over_genre_default_which_wins_over_parent() {
        let p = parent(); // tier = Frontier, adapter = fake
        let roles = vec![
            RoleSpec {
                id: "implementer".into(),
                tier: None, // 役割自身は tier を決めない -> 分野の既定に委ねる
                adapter: Some("codex".into()),
                max_turns: None,
                max_wall_secs: None,
                instructions: None,
            },
            RoleSpec {
                id: "lead".into(),
                tier: Some(Tier::Cheap),
                adapter: None,
                max_turns: Some(20),
                max_wall_secs: None,
                instructions: None,
            },
        ];
        let genres = vec![genre("coding", Some("lead"), &["lead", "implementer"])];
        let mut t = dt("impl", vec![]);
        t.role = Some("implementer".into());
        t.genre = Some("coding".into());
        let out = materialize_delegated(
            &p,
            &[t],
            &[0],
            &[],
            &roles,
            &genres,
            WorkspaceContext::default(),
            OffsetDateTime::now_utc(),
        );
        // adapter は role（implementer）の既定が優先（parent の "fake" にも分野の既定にも負けない）。
        assert_eq!(out[0].worker_hint.adapter.as_deref(), Some("codex"));
        // tier は role（implementer）に既定が無いので、分野の既定役割（lead）の tier を借りる
        // （親の tier = Frontier ではなく Cheap になる）。
        assert_eq!(out[0].worker_hint.tier, Tier::Cheap);
        // max_turns も role に無いので分野の既定役割から。
        assert_eq!(out[0].budget.max_turns, 20);
    }

    /// ADR-0033 D4（Phase 24）→ ADR-0069 D1（Phase 114）: 委譲（LLM）が書いた `assignee` は**捨て**、
    /// `routing.dropped_assignee` に残す。担当の分野も既定の解決に使わない（担当は matching が決める）。
    #[test]
    fn materialize_drops_the_delegated_assignee_and_records_it() {
        use crate::org::{OrgKind, OrgNode};
        let now = OffsetDateTime::now_utc();
        let node = |id: &str, genre_id: Option<&str>| OrgNode {
            profile: Default::default(),
            id: id.into(),
            parent_id: Some("research".into()),
            name: id.into(),
            kind: OrgKind::Section,
            genre: genre_id.map(str::to_string),
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        };
        let org = vec![
            node("research-survey", Some("literature")),
            node("research-data", None),
        ];
        let roles = vec![
            RoleSpec {
                id: "literature-reader".into(),
                tier: Some(Tier::Cheap),
                adapter: Some("paperqa".into()),
                max_turns: Some(5),
                ..RoleSpec::default()
            },
            RoleSpec {
                id: "writer".into(),
                tier: Some(Tier::Standard),
                adapter: Some("claude-code".into()),
                ..RoleSpec::default()
            },
        ];
        let genres = vec![genre(
            "literature",
            Some("literature-reader"),
            &["literature-reader"],
        )];
        let p = parent();

        // 1. assignee だけ: そのノードの分野 → default_role の既定が効く。
        let mut t = dt("survey", vec![]);
        t.assignee = Some("research-survey".into());
        let out = materialize_delegated(
            &p,
            &[t],
            &[0],
            &org,
            &roles,
            &genres,
            WorkspaceContext::default(),
            now,
        );
        assert_eq!(out[0].assignee, None);
        assert_eq!(
            out[0]
                .routing
                .as_ref()
                .and_then(|r| r.dropped_assignee.as_deref()),
            Some("research-survey")
        );
        assert_eq!(
            out[0].genre, p.genre,
            "the dropped assignee's genre is not used"
        );
        assert_eq!(
            out[0].routing.as_ref().map(|r| r.tier_source),
            Some(crate::model::TierSource::Default)
        );

        // 2. assignee + role: tier / adapter / 予算は role が勝ち、assignee は担当としてだけ残る。
        let mut t = dt("survey", vec![]);
        t.assignee = Some("research-survey".into());
        t.role = Some("writer".into());
        let out = materialize_delegated(
            &p,
            &[t],
            &[0],
            &org,
            &roles,
            &genres,
            WorkspaceContext::default(),
            now,
        );
        assert_eq!(out[0].assignee, None);
        assert_eq!(out[0].worker_hint.adapter.as_deref(), Some("claude-code"));
        assert_eq!(out[0].worker_hint.tier, Tier::Standard);

        // 3. 分野を持たないノード・組織に無い id は既定を変えない（知らない id は担当にもしない）。
        let mut t = dt("tidy", vec![]);
        t.assignee = Some("research-data".into());
        let out = materialize_delegated(
            &p,
            &[t],
            &[0],
            &org,
            &roles,
            &genres,
            WorkspaceContext::default(),
            now,
        );
        assert_eq!(out[0].assignee, None);
        assert_eq!(out[0].genre, p.genre);
        let mut t = dt("ghost", vec![]);
        t.assignee = Some("nobody".into());
        let out = materialize_delegated(
            &p,
            &[t],
            &[0],
            &org,
            &roles,
            &genres,
            WorkspaceContext::default(),
            now,
        );
        assert_eq!(out[0].assignee, None);
    }

    /// ADR-0039 D2: 委譲した子の作業場所も **明示 > 案件 > 親** の 3 段。案件が Remote なら子も Remote。
    #[test]
    fn delegated_child_workspace_is_explicit_then_project_then_parent() {
        let p = parent();
        let now = OffsetDateTime::now_utc();
        let project = WorkspaceSpec::Local {
            path: PathBuf::from("/home/rmaeda/workspace/rust/pluvio-poc"),
            mode: None,
        };
        let explicit = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::from("/work/NBB/rmaeda/workspace/rust/benchfs"),
            mode: None,
        };

        // 1. 案件も明示も無い → 親を継ぐ（従来）。
        let out = materialize_delegated(
            &p,
            &[dt("a", vec![])],
            &[0],
            &[],
            &[],
            &[],
            WorkspaceContext::default(),
            now,
        );
        assert_eq!(out[0].workspace, p.workspace);

        // 2. 案件の作業場所 > 親。
        let ws = WorkspaceContext {
            repos: &[],
            project: Some(&project),
            home: None,
        };
        let out = materialize_delegated(&p, &[dt("a", vec![])], &[0], &[], &[], &[], ws, now);
        assert_eq!(out[0].workspace, project);

        // 3. 明示 > 案件（別のクラスタで検証させたいとき）。
        let mut t = dt("b", vec![]);
        t.workspace = Some(explicit.clone());
        let out = materialize_delegated(&p, &[t], &[0], &[], &[], &[], ws, now);
        assert_eq!(out[0].workspace, explicit);

        // 4. 案件が Remote なら子も Remote（ADR-0018 の写し + `.taskd/remote-exec` 経路に乗る）。
        let ws = WorkspaceContext {
            repos: &[],
            project: Some(&explicit),
            home: None,
        };
        let out = materialize_delegated(&p, &[dt("c", vec![])], &[0], &[], &[], &[], ws, now);
        assert_eq!(out[0].workspace, explicit);
    }

    fn org_node_with_tools(id: &str, tools: &[&str]) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            id: id.into(),
            parent_id: Some("cos".into()),
            name: id.into(),
            kind: crate::org::OrgKind::Department,
            genre: None,
            brief: String::new(),
            profile: crate::profile::Profile {
                tools: tools.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            position: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// ADR-0062 B2（Phase 107）→ ADR-0069 D1（Phase 114）: 委譲が書いた担当は捨てるので、案件から継いだ
    /// Remote workspace は担当未定のまま Remote に残る（matching が `cluster:<id>` を持つノードだけを
    /// 候補にする）。明示した子（`t.workspace = Some(..)`）も従来どおり落とさない。
    #[test]
    fn inherited_remote_workspace_stays_remote_because_the_delegated_assignee_is_dropped() {
        let p = parent();
        let now = OffsetDateTime::now_utc();
        let project = WorkspaceSpec::Remote {
            cluster: "sirius".into(),
            path: PathBuf::from("/work/NBB/rmaeda/workspace/rust/benchfs"),
            mode: None,
        };
        let ws = WorkspaceContext {
            repos: &[],
            project: Some(&project),
            home: None,
        };
        let org = vec![
            org_node_with_tools("web-research", &["tavily", "exa"]),
            org_node_with_tools("cluster-hpc", &["cluster:sirius"]),
        ];

        // 以前は Local(<task_id>) に落ちた。今は担当を捨てるので Remote のまま、reason も出ない。
        let mut without_tool = dt("survey", vec![]);
        without_tool.assignee = Some("web-research".into());
        let mut reasons: Vec<(TaskId, String)> = Vec::new();
        let out = materialize_delegated_logging(
            &p,
            &[without_tool],
            &[0],
            &org,
            &[],
            &[],
            ws,
            now,
            &mut |id, reason| reasons.push((id, reason.to_string())),
        );
        assert_eq!(out[0].workspace, project);
        assert!(reasons.is_empty(), "{reasons:?}");

        // 担当が cluster:sirius を持つ → Remote のまま、reason は出ない。
        let mut with_tool = dt("compute", vec![]);
        with_tool.assignee = Some("cluster-hpc".into());
        let mut reasons2: Vec<(TaskId, String)> = Vec::new();
        let out2 = materialize_delegated_logging(
            &p,
            &[with_tool],
            &[0],
            &org,
            &[],
            &[],
            ws,
            now,
            &mut |id, reason| reasons2.push((id, reason.to_string())),
        );
        assert_eq!(out2[0].workspace, project);
        assert!(reasons2.is_empty());

        // 明示した子は落とさない（担当が道具を持たなくても、明示は明示のまま。検証は別経路で拒否する）。
        let mut explicit_child = dt("explicit", vec![]);
        explicit_child.assignee = Some("web-research".into());
        explicit_child.workspace = Some(project.clone());
        let out3 = materialize_delegated(&p, &[explicit_child], &[0], &org, &[], &[], ws, now);
        assert_eq!(out3[0].workspace, project);
    }

    /// ADR-0062 B2 の降格そのもの（担当が決まっている経路のための関数。ADR-0069 以降、計画・委譲は
    /// 担当を渡さないのでここには `None` が来るが、規則自体は保つ）。
    #[test]
    fn downgrade_rule_still_applies_when_an_assignee_is_known() {
        let project = WorkspaceSpec::Remote {
            cluster: "sirius".into(),
            path: PathBuf::from("/work/x"),
            mode: None,
        };
        let org = vec![
            org_node_with_tools("web-research", &["tavily"]),
            org_node_with_tools("cluster-hpc", &["cluster:sirius"]),
        ];
        let id = TaskId::new();
        let (ws, reason) = downgrade_inherited_remote_if_needed(
            project.clone(),
            false,
            Some("web-research"),
            &org,
            id,
        );
        assert!(matches!(ws, WorkspaceSpec::Local { .. }));
        assert!(reason.is_some_and(|r| r.contains("cluster:sirius")));
        let (ws, reason) = downgrade_inherited_remote_if_needed(
            project.clone(),
            false,
            Some("cluster-hpc"),
            &org,
            id,
        );
        assert_eq!(ws, project);
        assert!(reason.is_none());
        let (ws, _) = downgrade_inherited_remote_if_needed(project.clone(), false, None, &org, id);
        assert_eq!(ws, project);
    }

    /// ADR-0039 D5: `~` は `$HOME` で展開する。`Remote` の `~` はクラスタ側の home なので触らない。
    #[test]
    fn tilde_is_expanded_for_local_workspaces_only() {
        let home = PathBuf::from("/home/rmaeda");
        let local = WorkspaceSpec::Local {
            path: PathBuf::from("~/workspace/rust/pluvio-poc"),
            mode: None,
        };
        assert_eq!(
            local.with_home_expanded(Some(&home)),
            WorkspaceSpec::Local {
                path: PathBuf::from("/home/rmaeda/workspace/rust/pluvio-poc"),
                mode: None
            }
        );
        assert_eq!(
            local.with_home_expanded(None),
            local,
            "$HOME が無ければそのまま"
        );
        let remote = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::from("~/workspace/rust/benchfs"),
            mode: None,
        };
        assert_eq!(remote.with_home_expanded(Some(&home)), remote);
        let absolute = WorkspaceSpec::Local {
            path: PathBuf::from("/tmp/ws"),
            mode: None,
        };
        assert_eq!(absolute.with_home_expanded(Some(&home)), absolute);
        // `~user` は展開しない（その home を知らない）。
        let other = WorkspaceSpec::Local {
            path: PathBuf::from("~someone/ws"),
            mode: None,
        };
        assert_eq!(other.with_home_expanded(Some(&home)), other);
        assert_eq!(
            crate::model::expand_home(std::path::Path::new("~"), Some(&home)),
            home
        );
    }
}
