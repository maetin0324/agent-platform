//! `celerisctl add` の判断と検証 — DESIGN.md §5.9 / ADR-0004 D4 / ADR-0010 D4（P-17, P-19, ADR-0013 D7）。
//!
//! `NewTaskSpec` から `Task` を組み立て、`TaskStore::create_task` で `insert` + `Event::Created`
//! を単一トランザクションとして書き込む（ADR-0010 D2）。初期 `status` は ADR-0002 D4 のとおり
//! `kind == Approval` なら `Ready`、それ以外は `Draft`。
//!
//! `acceptance` の並び順は呼び出し側（`celerisctl` の CLI 引数写像）の責務。ここでは渡された順を
//! そのまま使う。条件のテキスト規則: `Command` は `` `<cmd>` exits 0 ``、`ArtifactExists` は
//! `artifact <name> exists`（現在の `celerisctl add` と同じ）。
//!
//! `depends_on` に渡した各 ID は、存在しないか `failed`/`cancelled` ならエラーにし、
//! 何も挿入しない（挿入した瞬間に後続が永久に進まない状態を作らないため）。
//!
//! `workspace` を省略した場合は `WorkspaceSpec::Local{ path: "<task_id>" }`（相対パス）になる。
//! ディスパッチャが `workspace_root` 基準で解決する（ADR-0005 D3, ADR-0010 D4, P-19）。

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{
    Budget, Check, Criterion, GenreSpec, MilestoneId, ProjectId, RoleSpec, Status, Task, TaskId,
    TaskKind, TaskStore, Tier, WorkerHint, WorkspaceSpec,
};
use time::OffsetDateTime;

use crate::error::OpsError;

/// 受け入れ条件 1 件の指定。現在の `celerisctl add` の `--accept`/`--check-cmd`/
/// `--check-artifact`/`--check-reviewer` に対応する。API の `POST /tasks` の `acceptance[]` でもある（`docs/gui/api.md` §3.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CriterionSpec {
    /// `Check::Human`。
    Human { text: String },
    /// `Check::Command`。
    Command {
        cmd: String,
        #[serde(default)]
        expect_exit: i32,
    },
    /// `Check::ArtifactExists`。
    ArtifactExists { name: String },
    /// `Check::KnowledgePage`（ADR-0067 D2: 知識ベースのページ参照）。
    KnowledgePage { path: String },
    /// `Check::Reviewer`。
    Reviewer { text: String },
}

impl CriterionSpec {
    pub(crate) fn into_criterion(self) -> Criterion {
        match self {
            CriterionSpec::Human { text } => Criterion {
                text,
                check: Check::Human,
            },
            CriterionSpec::Command { cmd, expect_exit } => Criterion {
                text: format!("`{cmd}` exits 0"),
                check: Check::Command { cmd, expect_exit },
            },
            CriterionSpec::ArtifactExists { name } => Criterion {
                text: format!("artifact {name} exists"),
                check: Check::ArtifactExists { name },
            },
            CriterionSpec::KnowledgePage { path } => Criterion {
                text: format!("knowledge base page {path} exists"),
                check: Check::KnowledgePage { path },
            },
            CriterionSpec::Reviewer { text } => Criterion {
                text,
                check: Check::Reviewer,
            },
        }
    }
}

/// `celerisctl add` から組み立てる新規タスクの指定。API の `POST /tasks` の本文でもある（`docs/gui/api.md` §3.4）。
/// 省略時の既定は `celerisctl add` と同じ。
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewTaskSpec {
    pub title: String,
    pub objective: String,
    pub acceptance: Vec<CriterionSpec>,
    #[serde(default = "default_kind")]
    pub kind: TaskKind,
    /// 省略時は役割の既定 → `standard`（ADR-0016 D1 / M3: タスクの値 > 役割の既定 > 全体の既定）。
    #[serde(default)]
    pub tier: Option<Tier>,
    /// ADR-0044 D3: `"P1"` のようなラベルでも `20` のような整数でも書ける。**省略時は P2**
    /// （= `task_core::DEFAULT_PRIORITY` = 10）。`celerisctl add` は `--priority` の既定 0 を明示して渡すので
    /// 従来どおり。
    #[serde(default)]
    pub priority: Option<PriorityInput>,
    #[serde(default)]
    pub parent: Option<TaskId>,
    #[serde(default)]
    pub depends_on: Vec<TaskId>,
    /// 省略時は役割の既定 → 10。
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// 省略時は役割の既定 → 600。
    #[serde(default)]
    pub max_wall_secs: Option<u64>,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// ADR-0016 D1: 役割名（自由記述）。`[[roles]]` にあれば省略値の既定と run 時の指示文が効く。
    #[serde(default)]
    pub role: Option<String>,
    /// ADR-0027 D1: 分野名（自由記述）。省略時は `role` の分野（`[[genres]] roles` に含む分野がちょうど
    /// 1 つのとき）を継ぐ。`genres` が設定されていれば、知らない `genre` や `role` とその分野の不整合は
    /// エラー（`genres` が空の設定では検証しない。分野は任意）。
    #[serde(default)]
    pub genre: Option<String>,
    /// ADR-0016 D3: 委譲した子が全て終端になった後に集約 run を 1 回行う。
    #[serde(default)]
    pub aggregate: bool,
    /// ADR-0033 D2: このタスクが属する案件。存在しない案件はエラー。
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// ADR-0033 D2: このタスクが属する途中目標。`project_id` と同じ案件のものであること。
    #[serde(default)]
    pub milestone_id: Option<MilestoneId>,
    /// ADR-0033 D2: 割り当てる組織のノード（`org_nodes.id`）。既定の解決で役割・分野より先に見る。
    /// 存在しないノードはエラー。
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    /// ADR-0018: 指定すると `WorkspaceSpec::Remote{cluster, path}` になり、コマンドはそのクラスタで実行される。
    /// `workspace` がクラスタ側の作業ディレクトリ（既存プロジェクトでよい）。
    #[serde(default)]
    pub cluster: Option<String>,
    /// ADR-0059 D1: `cluster` を指定したときの `WorkspaceSpec::Remote.mode`。省略時は `Worktree`
    /// （従来どおりクラスタの `sync` 設定に従う）。`Local` タスク（`cluster` 無し）には関係ない。
    #[serde(default)]
    pub workspace_mode: Option<task_core::WorkspaceMode>,
    /// 省略時は役割の既定 → 指定なし。
    #[serde(default)]
    pub adapter: Option<String>,
    /// ADR-0043 D2: このタスクが使う案件のリポジトリを**名前で**指定する（`project_repos.name`）。
    /// 省略すると **親 → 案件の primary** を継ぐ。案件に無い名前は 422、リモートのリポジトリを
    /// 他と混ぜたものも 422（`task_core::resolve_task_repos`）。
    #[serde(default)]
    pub repos: Vec<String>,
    // ---- ADR-0044 D1/D3（Phase 53）: 人が作るタスク。ここから ----
    /// ADR-0044 D3: ラベル（小文字 `[a-z0-9-]`、最大 8 個）。省略時は無し。
    #[serde(default)]
    pub labels: Vec<String>,
    /// ADR-0046 D2（Phase 59）: このタスクに必要な能力タグ（小文字 `[a-z0-9._-]`、最大 12 個）。
    /// `assignee` を書かなければ、これとノードの実効 `skills` の重なりで担当が決まる（D5 の matching）。
    #[serde(default)]
    pub skills: Vec<String>,
    /// ADR-0046 D4（Phase 59）: 進め方（`prototype` / `production` / `research`）。省略時は `production`。
    #[serde(default)]
    pub mode: Option<task_core::TaskMode>,
    /// ADR-0044 D3: 種類。省略時は `other`。
    #[serde(default)]
    pub category: Option<task_core::TaskCategory>,
    /// ADR-0044 D1: 初期状態。`draft` か `ready` だけ（それ以外は 422）。**省略時は呼び出し側の既定**
    /// （`celerisctl add` と委譲・計画の経路は従来どおり `draft`、`POST /tasks` は `ready`。人は Go を出す
    /// 側なので draft を挟まない）。`kind = approval` は従来どおり常に `ready`。
    #[serde(default)]
    pub status: Option<Status>,
    // ---- ADR-0044 D1/D3（Phase 53）: ここまで ----
    /// ADR-0069 D3（Phase 114）: lane policy の `TaskFeatures` の明示の上書き（書いた軸だけが勝つ）。
    #[serde(default)]
    pub features: Option<task_core::TaskFeatureHints>,
    /// ADR-0069 D1: この spec の出自。**API の JSON からは入らない**（`serde(skip)`。偽装できない）。
    /// 既定は人（`POST /tasks` / `celerisctl add`）。LLM の経路（CoS の actions）はコードが `Agent` を立てる。
    #[serde(skip)]
    pub provenance: SpecProvenance,
}

/// ADR-0069 D1: `NewTaskSpec` を誰が書いたか。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpecOrigin {
    /// 人（API / CLI）。`tier` / `assignee` は人の明示として従う。
    #[default]
    Human,
    /// LLM（CoS の Console actions）。`tier` はヒント、`assignee` は人の明示が確かめられたときだけ残る。
    Agent,
    /// celeris のコード（計画 run・報告・知識整理など）。`tier` は固定値として従う。
    System,
}

/// ADR-0069 D1: spec の出自と、LLM 経路で捨てた値（`Task.routing` に写す）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecProvenance {
    pub origin: SpecOrigin,
    /// `origin = Agent` でも、人の発言に `tier:<lane>` があった（人の明示の tier）。
    pub human_explicit_tier: bool,
    /// LLM が書いたが人の明示ではないので捨てた担当。
    pub dropped_assignee: Option<String>,
}

impl SpecProvenance {
    pub fn system() -> Self {
        Self {
            origin: SpecOrigin::System,
            ..Self::default()
        }
    }

    /// ADR-0069 D1: `tier` を誰が決めたか。
    pub fn tier_source(&self, tier_given: bool) -> task_core::TierSource {
        use task_core::TierSource;
        // celeris のコードが作るタスク（計画 run・報告・知識整理）は lane policy の対象にしない。
        if self.origin == SpecOrigin::System {
            return TierSource::System;
        }
        if !tier_given {
            return TierSource::Default;
        }
        match self.origin {
            SpecOrigin::Human => TierSource::Human,
            SpecOrigin::System => TierSource::System,
            SpecOrigin::Agent if self.human_explicit_tier => TierSource::Human,
            SpecOrigin::Agent => TierSource::Hint,
        }
    }
}

/// ADR-0044 D3: `priority` の入力。`"P1"` のようなラベルでも整数でも書ける（API は `priority_label` を
/// 返すので、GUI はラベルだけを扱えばよい。`i32` は互換のため残す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum PriorityInput {
    /// `"P0"` 〜 `"P3"`。
    Label(PriorityLabel),
    /// 生の `i32`（大きいほど先。従来どおり）。
    Number(i32),
}

/// ADR-0044 D3 の優先度のラベル。P0 = 30 / P1 = 20 / P2 = 10 / P3 = 0（`task_core::PRIORITY_LABELS`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum PriorityLabel {
    P0,
    P1,
    P2,
    P3,
}

impl PriorityInput {
    /// `Task.priority`（`i32`）に写す。
    pub fn to_i32(self) -> i32 {
        match self {
            PriorityInput::Number(n) => n,
            PriorityInput::Label(PriorityLabel::P0) => 30,
            PriorityInput::Label(PriorityLabel::P1) => 20,
            PriorityInput::Label(PriorityLabel::P2) => 10,
            PriorityInput::Label(PriorityLabel::P3) => 0,
        }
    }
}

fn default_kind() -> TaskKind {
    TaskKind::Execute
}

/// ADR-0043 D2: `POST /tasks` の `repos`（名前）を解決する。**明示 > 親 > 案件の primary**。
///
/// - 案件に属さないタスク（`project_id` が無い）で `repos` を書いたら 422
/// - 案件に無い名前は 422、リモートのリポジトリを他と混ぜたものも 422
fn resolve_repos(
    store: &dyn TaskStore,
    project_id: Option<ProjectId>,
    parent: Option<TaskId>,
    names: &[String],
) -> Result<Vec<task_core::RepoRef>, OpsError> {
    let Some(project_id) = project_id else {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        return Err(OpsError::Validation(
            "repos can only be used on a task that belongs to a project".to_string(),
        ));
    };
    let available = store.repo_list(project_id)?;
    if !names.is_empty() {
        return task_core::resolve_task_repos(&available, names)
            .map_err(|e| OpsError::Validation(e.to_string()));
    }
    // 親から継ぐ（親が持っていなければ案件の primary）。
    if let Some(parent) = parent
        && let Some(parent) = store.get(parent)?
        && !parent.repos.is_empty()
    {
        return Ok(parent.repos);
    }
    Ok(available
        .iter()
        .find(|r| r.is_primary)
        .map(|r| vec![task_core::RepoRef::of(r)])
        .unwrap_or_default())
}
/// 全体の既定（`celerisctl add` と API で共通）。
pub const DEFAULT_TIER: Tier = Tier::Standard;
pub const DEFAULT_MAX_TURNS: u32 = 10;
pub const DEFAULT_MAX_WALL_SECS: u64 = 600;
pub const DEFAULT_MAX_RETRIES: u32 = 2;
fn default_max_retries() -> u32 {
    DEFAULT_MAX_RETRIES
}

fn build_acceptance(specs: Vec<CriterionSpec>) -> Result<Vec<Criterion>, OpsError> {
    if specs.is_empty() {
        return Err(OpsError::Validation(
            "at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)"
                .to_string(),
        ));
    }
    let acceptance: Vec<Criterion> = specs
        .into_iter()
        .map(CriterionSpec::into_criterion)
        .collect();
    // ADR-0067 D2: `human` チェックには artifacts か知識ベースの参照を伴わせる（本番事故の再発防止）。
    task_core::validate_human_checks_have_deliverable(&acceptance).map_err(OpsError::Validation)?;
    Ok(acceptance)
}

/// `depends_on` の各 ID が存在し、かつ `failed`/`cancelled` でないことを検証する
/// （ADR-0010 D4）。違反があれば挿入前にエラーを返す。
fn validate_depends_on(store: &dyn TaskStore, depends_on: &[TaskId]) -> Result<(), OpsError> {
    for dep_id in depends_on {
        match store.get(*dep_id)? {
            None => {
                return Err(OpsError::Validation(format!(
                    "dependency {dep_id} does not exist"
                )));
            }
            Some(dep) if matches!(dep.status, Status::Failed | Status::Cancelled) => {
                return Err(OpsError::Validation(format!(
                    "dependency {dep_id} has status {:?} and cannot be depended on",
                    dep.status
                )));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// ADR-0033 D2: `project_id` / `milestone_id` の整合（存在すること、途中目標がその案件のものであること）。
fn validate_project_and_milestone(
    store: &dyn TaskStore,
    spec: &NewTaskSpec,
) -> Result<(), OpsError> {
    let project = match spec.project_id {
        Some(id) => {
            let Some(project) = store.project_get(id)? else {
                return Err(OpsError::Validation(format!("project {id} does not exist")));
            };
            Some(project)
        }
        None => None,
    };
    if let Some(milestone_id) = spec.milestone_id {
        let Some(project) = project else {
            return Err(OpsError::Validation(
                "milestone_id requires project_id".to_string(),
            ));
        };
        if !store
            .milestone_list(project.id)?
            .iter()
            .any(|m| m.id == milestone_id)
        {
            return Err(OpsError::Validation(format!(
                "milestone {milestone_id} does not belong to project {}",
                project.id
            )));
        }
    }
    Ok(())
}

/// `spec` から `Task` を組み立て、`store.create_task` で原子的に挿入する（役割・分野の既定は無し = 全体の既定だけ）。
pub fn create_task(
    store: &dyn TaskStore,
    spec: NewTaskSpec,
    now: OffsetDateTime,
) -> Result<Task, OpsError> {
    create_task_with_roles(store, spec, &[], &[], now)
}

/// ADR-0016 D1 / M3, ADR-0027 D1: `spec` の省略値を `roles`（`[[roles]]`）の既定 → `genres`（`[[genres]]`）の
/// `default_role` の既定 → 全体の既定の順で埋めてから挿入する。`spec.role` が `roles` に無くてもエラーに
/// しない（役割名は自由記述。既定と指示文が無いだけ）。`genres` が空でなければ、知らない `genre` や
/// `genre` + `role` の不整合（`role` がその分野の `roles` に無い）はエラーにする（`genres` が空の設定
/// では検証しない。celerisctl の `--config` 無しはこちらに当たる）。
pub fn create_task_with_roles(
    store: &dyn TaskStore,
    spec: NewTaskSpec,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    now: OffsetDateTime,
) -> Result<Task, OpsError> {
    let task = build_task(store, spec, roles, genres, true, now)?;
    store.create_task(&task, vec![])?;
    Ok(task)
}

/// ADR-0034 D3（Phase 25 の監査 L-4）: **受け入れ条件を持たない裏方のタスク**（報告のまとめ run）を、
/// 上と**同じ解決順**（タスクの値 > 役割の既定 > `assignee` 由来 > 分野の既定 > 全体の既定）で作る。
///
/// 通常の作成経路との違いは 2 つだけ: 受け入れ条件が空でもよい（出力は「1 件の報告」そのもので、決定的に
/// 確かめられるものが無い。条件ゼロのレビューは全 pass = `done`）、そして人の承認を待たずに `ready` で
/// 始まる（起こしたのは人ではなく tick ループの決定的な判断）。`Event::Created` を 1 件残す。
/// `spec.role` が `[[roles]]` に無い構成でも落ちない（既定が埋まらないだけ）。
pub fn create_support_task(
    store: &dyn TaskStore,
    spec: NewTaskSpec,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    now: OffsetDateTime,
) -> Result<Task, OpsError> {
    let mut task = build_task(store, spec, roles, genres, false, now)?;
    task.status = Status::Ready;
    store.create_task(
        &task,
        vec![task_core::Event::Created {
            task: Box::new(task.clone()),
        }],
    )?;
    Ok(task)
}

/// `spec` を検証して `Task` を組み立てる（挿入はしない）。`require_acceptance = false` なら
/// 受け入れ条件が空でもよい（`create_support_task` 専用）。
fn build_task(
    store: &dyn TaskStore,
    spec: NewTaskSpec,
    roles: &[RoleSpec],
    genres: &[GenreSpec],
    require_acceptance: bool,
    now: OffsetDateTime,
) -> Result<Task, OpsError> {
    let role = spec.role.as_deref().and_then(|r| RoleSpec::find(roles, r));
    if !genres.is_empty()
        && let Some(g) = &spec.genre
    {
        let Some(genre_spec) = GenreSpec::find(genres, g) else {
            return Err(OpsError::Validation(format!("unknown genre: {g:?}")));
        };
        if let Some(r) = &spec.role
            && !genre_spec.roles.iter().any(|x| x == r)
        {
            return Err(OpsError::Validation(format!(
                "role {r:?} is not one of genre {g:?}'s roles"
            )));
        }
    }
    // ADR-0033 D2（監査 D-2）: `assignee` があれば、その組織ノードの分野（`org_nodes.genre`）→ その分野の
    // `default_role` を引いて `org_role` に持つ。ただし解決順は task > role > assignee > genre.default_role >
    // 全体の既定なので、`org_role` が効くのは **`spec.role` が明示されていないとき** だけ（下の budget /
    // worker_hint の組み立てで `role` を `org_role` より先に見る）。`assignee` が無ければ `org_role` は
    // `None` のまま。
    let (assignee_genre, org_role) = match spec.assignee.as_deref() {
        Some(assignee) => {
            let org = store.org_list()?;
            if !org.iter().any(|n| n.id == assignee) {
                return Err(OpsError::Validation(format!(
                    "assignee {assignee:?} is not an org node"
                )));
            }
            // ADR-0046 D5: 明示の `assignee` が、そのタスクのハーネスを `harnesses.allowed` に
            // 持たなければ 422 で差し戻す（profile を持たないノードは従来どおり通る）。
            crate::matching::assignee_accepts(&org, assignee, spec.genre.as_deref())
                .map_err(OpsError::Validation)?;
            task_core::assignee_defaults(&org, assignee, roles, genres)
        }
        None => (None, None),
    };
    validate_project_and_milestone(store, &spec)?;
    let genre_id = spec
        .genre
        .clone()
        .or_else(|| {
            spec.role
                .as_deref()
                .and_then(|r| GenreSpec::unique_for_role(genres, r))
        })
        .or(assignee_genre);
    let genre_role = genre_id
        .as_deref()
        .and_then(|g| GenreSpec::find(genres, g))
        .and_then(|g| g.default_role.as_deref())
        .and_then(|r| RoleSpec::find(roles, r));
    // ADR-0044 D1（Phase 53）: `kind = approval` は従来どおり常に `ready`。それ以外は `spec.status`
    // （`draft` か `ready` だけ）が勝ち、省略時は従来どおり `draft`（`POST /tasks` のハンドラが
    // 省略時に `ready` を入れる。人は Go を出す側なので draft を挟まない）。
    let status = if spec.kind == TaskKind::Approval {
        Status::Ready
    } else {
        match spec.status {
            None => Status::Draft,
            Some(s @ (Status::Draft | Status::Ready)) => s,
            Some(other) => {
                return Err(OpsError::Validation(format!(
                    "status must be \"draft\" or \"ready\" when creating a task (got {other:?})"
                )));
            }
        }
    };
    // ADR-0044 D3（Phase 53）: ラベルの検証（小文字 `[a-z0-9-]`、最大 8 個、重複は畳む）。
    let labels = task_core::normalize_labels(&spec.labels).map_err(OpsError::Validation)?;
    // ADR-0046 D2: 必要な能力タグ（綴りの規則は `labels` と同じ扱いで、違反は 422）。
    let skills = task_core::normalize_skills(&spec.skills).map_err(OpsError::Validation)?;
    let category = spec.category.unwrap_or_default();
    // ADR-0044 D3: 省略時は P2（`celerisctl add` は `--priority` の既定 0 を明示して渡す）。
    let priority = spec
        .priority
        .map(PriorityInput::to_i32)
        .unwrap_or(task_core::DEFAULT_PRIORITY);

    // ADR-0014 D3（P-G16）: 空白だけの title / objective と、存在しない親を拒否する（celerisctl add も同じ関数を通る）。
    if spec.title.trim().is_empty() {
        return Err(OpsError::Validation("title must not be blank".to_string()));
    }
    if spec.objective.trim().is_empty() {
        return Err(OpsError::Validation(
            "objective must not be blank".to_string(),
        ));
    }
    let acceptance = if require_acceptance {
        build_acceptance(spec.acceptance)?
    } else {
        spec.acceptance
            .into_iter()
            .map(CriterionSpec::into_criterion)
            .collect()
    };
    if let Some(parent) = spec.parent
        && store.get(parent)?.is_none()
    {
        return Err(OpsError::Validation(format!(
            "parent {parent} does not exist"
        )));
    }
    validate_depends_on(store, &spec.depends_on)?;

    // ADR-0043 D2: このタスクが使う案件のリポジトリ（明示 > 親 > 案件の primary）。
    let repos = resolve_repos(store, spec.project_id, spec.parent, &spec.repos)?;

    let id = TaskId::new();
    let workspace = match (spec.cluster, spec.workspace) {
        // ADR-0018: クラスタ指定。path はクラスタ側の作業ディレクトリ（絶対パスで指定する）。
        // ADR-0059 D1: `workspace_mode` が `WorkspaceSpec::Remote.mode` になる（省略時 = 従来どおり）。
        (Some(cluster), Some(path)) => WorkspaceSpec::Remote {
            cluster,
            path,
            mode: spec.workspace_mode,
        },
        (Some(cluster), None) => WorkspaceSpec::Remote {
            cluster,
            path: PathBuf::from(id.to_string()),
            mode: spec.workspace_mode,
        },
        (None, Some(path)) => WorkspaceSpec::Local { path, mode: None },
        (None, None) => WorkspaceSpec::Local {
            path: PathBuf::from(id.to_string()),
            mode: None,
        },
    };

    // ADR-0033 D2（監査 D-2）: tier / adapter / budget = タスクの値 > 役割の既定（`role` を明示） >
    // `assignee` 由来の既定（ノードの分野の `default_role`）> 分野の既定（`genre` から引いた `default_role`）>
    // 全体の既定。`role` が `assignee` より先に来る（ADR-0016 D1「タスクの値 > 役割の既定」に揃える）。
    let budget = Budget {
        max_turns: spec
            .max_turns
            .or(role.and_then(|r| r.max_turns))
            .or(org_role.and_then(|r| r.max_turns))
            .or(genre_role.and_then(|r| r.max_turns))
            .unwrap_or(DEFAULT_MAX_TURNS),
        max_wall_secs: spec
            .max_wall_secs
            .or(role.and_then(|r| r.max_wall_secs))
            .or(org_role.and_then(|r| r.max_wall_secs))
            .or(genre_role.and_then(|r| r.max_wall_secs))
            .unwrap_or(DEFAULT_MAX_WALL_SECS),
        max_retries: spec.max_retries,
    };

    // ADR-0069 D1 / D3: routing の出自（tier を誰が決めたか・捨てた担当・features の上書き）。
    let routing = task_core::TaskRouting {
        tier_source: spec.provenance.tier_source(spec.tier.is_some()),
        assignee_explicit: spec.assignee.is_some(),
        dropped_assignee: spec.provenance.dropped_assignee.clone(),
        features: spec.features.filter(|f| !f.is_empty()),
    };
    let task = Task {
        routing: Some(routing),
        repos,
        id,
        parent_id: spec.parent,
        kind: spec.kind,
        title: spec.title,
        objective: spec.objective,
        acceptance,
        inputs: vec![],
        depends_on: spec.depends_on,
        status,
        priority,
        worker_hint: WorkerHint {
            tier: spec
                .tier
                .or(role.and_then(|r| r.tier))
                .or(org_role.and_then(|r| r.tier))
                .or(genre_role.and_then(|r| r.tier))
                .unwrap_or(DEFAULT_TIER),
            adapter: spec
                .adapter
                .or_else(|| role.and_then(|r| r.adapter.clone()))
                .or_else(|| org_role.and_then(|r| r.adapter.clone()))
                .or_else(|| genre_role.and_then(|r| r.adapter.clone())),
        },
        workspace,
        budget,
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
        role: spec.role,
        genre: genre_id,
        aggregate: spec.aggregate,
        project_id: spec.project_id,
        milestone_id: spec.milestone_id,
        assignee: spec.assignee,
        conversation: None,
        labels,
        skills,
        mode: spec.mode.unwrap_or_default(),
        category,
    };
    Ok(task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{Event, SqliteStore};

    fn base_spec() -> NewTaskSpec {
        NewTaskSpec {
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            title: "do something".to_string(),
            objective: "make it work".to_string(),
            // ADR-0067 D2: `human` チェックには成果物か知識ベースの参照が要る。
            acceptance: vec![
                CriterionSpec::Human {
                    text: "it works".to_string(),
                },
                CriterionSpec::ArtifactExists {
                    name: "result.md".to_string(),
                },
            ],
            kind: TaskKind::Execute,
            tier: None,
            priority: Some(PriorityInput::Number(0)),
            parent: None,
            depends_on: vec![],
            max_turns: None,
            max_wall_secs: None,
            max_retries: 2,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            workspace: Some(PathBuf::from("/tmp/workspace")),
            cluster: None,
            workspace_mode: None,
            adapter: None,
            labels: Vec::new(),
            category: None,
            status: None,
            features: None,
            provenance: SpecProvenance::default(),
        }
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    #[test]
    fn create_task_inserts_task_and_created_event() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let spec = base_spec();

        let task = create_task(&store, spec, now()).expect("create_task");

        assert_eq!(task.title, "do something");
        assert_eq!(task.objective, "make it work");
        assert_eq!(task.kind, TaskKind::Execute);
        assert_eq!(task.status, Status::Draft);
        assert_eq!(task.worker_hint.tier, Tier::Standard);
        assert_eq!(task.worker_hint.adapter, None);
        assert_eq!(task.budget.max_turns, 10);
        assert_eq!(task.budget.max_wall_secs, 600);
        assert_eq!(task.budget.max_retries, 2);
        assert_eq!(task.attempts, 0);
        assert!(task.lease.is_none());
        assert_eq!(
            task.workspace,
            WorkspaceSpec::Local {
                path: PathBuf::from("/tmp/workspace"),
                mode: None
            }
        );

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched, task);

        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(events.len(), 1);
        match &events[0].1 {
            Event::Created { task: created } => assert_eq!(created.id, task.id),
            other => panic!("expected Created event, got {other:?}"),
        }
    }

    /// ADR-0016 D1 / M3: タスクの値 > 役割の既定 > 全体の既定。設定に無い役割は名前だけ保存する。
    #[test]
    fn create_task_with_roles_fills_omitted_values_from_the_role_then_global_defaults() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let roles = vec![RoleSpec {
            id: "lead".to_string(),
            tier: Some(Tier::Frontier),
            adapter: Some("claude-code".to_string()),
            max_turns: Some(40),
            max_wall_secs: None,
            instructions: Some("you lead".to_string()),
        }];
        let mut spec = base_spec();
        spec.role = Some("lead".to_string());
        spec.aggregate = true;
        spec.max_turns = Some(7);
        let task = create_task_with_roles(&store, spec, &roles, &[], now()).expect("create");
        assert_eq!(task.role.as_deref(), Some("lead"));
        assert!(task.aggregate);
        assert_eq!(task.worker_hint.tier, Tier::Frontier, "role default");
        assert_eq!(
            task.worker_hint.adapter.as_deref(),
            Some("claude-code"),
            "role default"
        );
        assert_eq!(task.budget.max_turns, 7, "task value wins");
        assert_eq!(task.budget.max_wall_secs, 600, "global default");
        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["role"], "lead");
        assert_eq!(json["aggregate"], true);

        let mut spec = base_spec();
        spec.role = Some("nobody".to_string());
        let task = create_task_with_roles(&store, spec, &roles, &[], now())
            .expect("unknown role is allowed");
        assert_eq!(task.role.as_deref(), Some("nobody"));
        assert_eq!(task.worker_hint.tier, Tier::Standard);
        assert_eq!(task.budget.max_turns, 10);
        let json = serde_json::to_value(&task).unwrap();
        assert!(json.get("aggregate").is_none(), "false is omitted: {json}");

        // 役割なしの JSON（旧クライアント）はそのまま読める。
        let spec: NewTaskSpec = serde_json::from_str(
            r#"{"title":"t","objective":"o","acceptance":[{"type":"human","text":"x"}],"tier":"cheap","max_turns":3}"#,
        )
        .unwrap();
        assert_eq!(spec.tier, Some(Tier::Cheap));
        assert_eq!(spec.max_turns, Some(3));
        assert_eq!(spec.max_wall_secs, None);
        assert!(spec.role.is_none());
    }

    // ---- ADR-0033 D2（Phase 23）: assignee から先に解決する ----

    fn org_node(
        id: &str,
        parent: Option<&str>,
        kind: task_core::OrgKind,
        genre: Option<&str>,
    ) -> task_core::OrgNode {
        let now = OffsetDateTime::now_utc();
        task_core::OrgNode {
            profile: Default::default(),
            id: id.into(),
            parent_id: parent.map(str::to_string),
            name: id.into(),
            kind,
            genre: genre.map(str::to_string),
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn org_store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().expect("open store");
        store
            .org_upsert(&org_node(
                "secretary",
                None,
                task_core::OrgKind::Secretary,
                None,
            ))
            .expect("secretary");
        store
            .org_upsert(&org_node(
                "research",
                Some("secretary"),
                task_core::OrgKind::Department,
                None,
            ))
            .expect("department");
        store
            .org_upsert(&org_node(
                "research-survey",
                Some("research"),
                task_core::OrgKind::Section,
                Some("literature"),
            ))
            .expect("section");
        store
    }

    fn literature_setup() -> (Vec<RoleSpec>, Vec<GenreSpec>) {
        let roles = vec![
            RoleSpec {
                id: "literature-reader".into(),
                tier: Some(Tier::Cheap),
                adapter: Some("paperqa".into()),
                max_turns: Some(5),
                max_wall_secs: Some(1200),
                instructions: None,
            },
            RoleSpec {
                id: "lead".into(),
                tier: Some(Tier::Frontier),
                adapter: Some("claude-code".into()),
                ..RoleSpec::default()
            },
        ];
        let genres = vec![genre(
            "literature",
            Some("literature-reader"),
            &["literature-reader"],
        )];
        (roles, genres)
    }

    /// `assignee` があれば、そのノードの分野 →`default_role`→ 役割の既定で `WorkerHint` が埋まる。
    #[test]
    fn assignee_fills_the_worker_hint_from_the_org_nodes_genre() {
        let store = org_store();
        let (roles, genres) = literature_setup();
        let mut spec = base_spec();
        spec.assignee = Some("research-survey".into());
        let task = create_task_with_roles(&store, spec, &roles, &genres, now()).expect("create");
        assert_eq!(task.assignee.as_deref(), Some("research-survey"));
        assert_eq!(
            task.genre.as_deref(),
            Some("literature"),
            "the node's genre is adopted"
        );
        assert_eq!(task.worker_hint.tier, Tier::Cheap);
        assert_eq!(task.worker_hint.adapter.as_deref(), Some("paperqa"));
        assert_eq!(task.budget.max_turns, 5);
        assert_eq!(task.budget.max_wall_secs, 1200);
        assert_eq!(task.role, None, "assignee does not invent a role name");
    }

    /// ADR-0069 D1（Phase 114）: 人の経路（API / CLI。`SpecOrigin::Human` が既定）の `assignee` / `tier` は
    /// 人の明示として従い、`routing` に出自が残る。`provenance` は API の JSON からは偽装できない。
    #[test]
    fn human_spec_records_explicit_provenance_and_cannot_be_spoofed() {
        let store = org_store();
        let (roles, genres) = literature_setup();
        let mut spec = base_spec();
        spec.assignee = Some("research-survey".into());
        spec.tier = Some(Tier::Frontier);
        let task = create_task_with_roles(&store, spec, &roles, &genres, now()).expect("create");
        assert_eq!(task.assignee.as_deref(), Some("research-survey"));
        let routing = task.routing.expect("routing");
        assert_eq!(routing.tier_source, task_core::TierSource::Human);
        assert!(routing.assignee_explicit);

        // tier を書かなければ policy が決める（Default）。System はいつも System。
        let task = create_task_with_roles(&store, base_spec(), &roles, &genres, now()).unwrap();
        assert_eq!(
            task.routing.map(|r| r.tier_source),
            Some(task_core::TierSource::Default)
        );
        let mut spec = base_spec();
        spec.provenance = SpecProvenance::system();
        let task = create_task_with_roles(&store, spec, &roles, &genres, now()).unwrap();
        assert_eq!(
            task.routing.map(|r| r.tier_source),
            Some(task_core::TierSource::System)
        );

        // `provenance` は `serde(skip)`: JSON に書いても未知フィールドとして拒否される。
        let err = serde_json::from_str::<NewTaskSpec>(
            r#"{"title":"t","objective":"o","acceptance":[],"provenance":{"origin":"system"}}"#,
        );
        assert!(err.is_err());
    }

    /// タスク自身の値は `assignee` より強い。`role` を明示したら、その役割・分野が優先される。
    #[test]
    fn explicit_values_still_win_over_the_assignee() {
        let store = org_store();
        let (roles, genres) = literature_setup();
        let mut spec = base_spec();
        spec.assignee = Some("research-survey".into());
        spec.tier = Some(Tier::Frontier);
        spec.max_turns = Some(33);
        let task = create_task_with_roles(&store, spec, &roles, &genres, now()).expect("create");
        assert_eq!(task.worker_hint.tier, Tier::Frontier, "the task value wins");
        assert_eq!(task.budget.max_turns, 33);
        assert_eq!(
            task.worker_hint.adapter.as_deref(),
            Some("paperqa"),
            "still filled from the assignee"
        );

        // 監査 D-2: `role` を明示すると、`role` の tier/adapter が `assignee` 由来の既定より勝つ
        // （解決順は task > role > assignee > genre.default_role）。`role` に無いフィールド（ここでは
        // `max_turns`）は次の階層（assignee の既定）まで降りて埋まる。`assignee` は常に記録される。
        let mut spec = base_spec();
        spec.assignee = Some("research-survey".into());
        spec.role = Some("lead".into());
        let task = create_task_with_roles(&store, spec, &roles, &genres, now())
            .expect("role wins over assignee");
        assert_eq!(task.role.as_deref(), Some("lead"));
        assert_eq!(
            task.assignee.as_deref(),
            Some("research-survey"),
            "assignee is still recorded"
        );
        assert_eq!(
            task.worker_hint.tier,
            Tier::Frontier,
            "the role's tier wins over the assignee's default"
        );
        assert_eq!(
            task.worker_hint.adapter.as_deref(),
            Some("claude-code"),
            "the role's adapter wins over the assignee's default"
        );
        assert_eq!(
            task.budget.max_turns, 5,
            "the role has no max_turns of its own, so the assignee's default fills it"
        );

        let mut spec = base_spec();
        spec.assignee = Some("research-survey".into());
        spec.role = Some("lead".into());
        spec.genre = Some("literature".into());
        let err = create_task_with_roles(&store, spec, &roles, &genres, now()).unwrap_err();
        assert!(err.to_string().contains("is not one of genre"), "{err}");
    }

    /// 分野を持たないノード（部）や `assignee` 無しでは、従来の解決順がそのまま残る。
    #[test]
    fn without_an_assignee_nothing_changes() {
        let store = org_store();
        let (roles, genres) = literature_setup();
        let task =
            create_task_with_roles(&store, base_spec(), &roles, &genres, now()).expect("create");
        assert_eq!(task.worker_hint.tier, Tier::Standard, "global default");
        assert_eq!(task.worker_hint.adapter, None);
        assert_eq!(task.budget.max_turns, DEFAULT_MAX_TURNS);
        assert_eq!(task.assignee, None);

        let mut spec = base_spec();
        spec.assignee = Some("research".into());
        let task = create_task_with_roles(&store, spec, &roles, &genres, now()).expect("create");
        assert_eq!(task.assignee.as_deref(), Some("research"));
        assert_eq!(task.genre, None, "a department has no genre");
        assert_eq!(task.worker_hint.tier, Tier::Standard);
    }

    /// 知らない `assignee`・存在しない案件・案件違いの途中目標は 422 相当の検証エラー。
    #[test]
    fn unknown_assignee_project_or_milestone_is_rejected() {
        let store = org_store();
        let (roles, genres) = literature_setup();
        let mut spec = base_spec();
        spec.assignee = Some("nobody".into());
        let err = create_task_with_roles(&store, spec, &roles, &genres, now()).unwrap_err();
        assert!(err.to_string().contains("is not an org node"), "{err}");

        let mut spec = base_spec();
        spec.project_id = Some(task_core::ProjectId::new());
        let err = create_task_with_roles(&store, spec, &roles, &genres, now()).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");

        let now_ts = OffsetDateTime::now_utc();
        let project = task_core::Project {
            archived_at: None,
            paused_from: None,
            id: task_core::ProjectId::new(),
            title: "t".into(),
            request: "r".into(),
            status: task_core::ProjectStatus::Active,
            secretary_summary: None,
            workspace: None,
            created_at: now_ts,
            updated_at: now_ts,
        };
        store.project_create(&project).unwrap();
        let other = task_core::Project {
            id: task_core::ProjectId::new(),
            ..project.clone()
        };
        store.project_create(&other).unwrap();
        let milestone = store
            .milestone_create(other.id, "m", "", task_core::MilestoneStatus::Approved)
            .unwrap();

        let mut spec = base_spec();
        spec.project_id = Some(project.id);
        spec.milestone_id = Some(milestone.id);
        let err = create_task_with_roles(&store, spec, &roles, &genres, now()).unwrap_err();
        assert!(
            err.to_string().contains("does not belong to project"),
            "{err}"
        );

        let mut spec = base_spec();
        spec.milestone_id = Some(milestone.id);
        let err = create_task_with_roles(&store, spec, &roles, &genres, now()).unwrap_err();
        assert!(
            err.to_string().contains("milestone_id requires project_id"),
            "{err}"
        );

        // 正しい組み合わせは通り、列にも載る。
        let mut spec = base_spec();
        spec.project_id = Some(other.id);
        spec.milestone_id = Some(milestone.id);
        let task = create_task_with_roles(&store, spec, &roles, &genres, now()).expect("create");
        assert_eq!(task.project_id, Some(other.id));
        assert_eq!(task.milestone_id, Some(milestone.id));
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

    /// ADR-0027 D1: タスクの値 > 役割の既定 > 分野の既定（`default_role` の役割）> 全体の既定。
    /// 役割が無くても分野だけで既定が効く（「genre だけ」のケース）。
    #[test]
    fn create_task_with_roles_applies_genre_default_role_when_task_has_no_role() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let roles = vec![RoleSpec {
            id: "literature-reader".to_string(),
            tier: Some(Tier::Standard),
            adapter: Some("acp".to_string()),
            max_turns: Some(5),
            max_wall_secs: Some(1200),
            instructions: None,
        }];
        let genres = vec![genre(
            "literature",
            Some("literature-reader"),
            &["literature-reader"],
        )];
        let mut spec = base_spec();
        spec.genre = Some("literature".to_string());
        let task = create_task_with_roles(&store, spec, &roles, &genres, now()).expect("create");
        assert_eq!(task.role, None, "genre alone must not set the task's role");
        assert_eq!(task.genre.as_deref(), Some("literature"));
        assert_eq!(task.worker_hint.tier, Tier::Standard);
        assert_eq!(task.worker_hint.adapter.as_deref(), Some("acp"));
        assert_eq!(task.budget.max_turns, 5);
        assert_eq!(task.budget.max_wall_secs, 1200);
    }

    /// ADR-0027 D1: `genre` 未指定で `role` がちょうど 1 つの分野に属するなら、その分野を継ぐ。
    #[test]
    fn create_task_with_roles_infers_genre_from_a_role_that_belongs_to_exactly_one_genre() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let roles = vec![RoleSpec {
            id: "literature-scout".to_string(),
            ..RoleSpec::default()
        }];
        let genres = vec![genre(
            "literature",
            Some("literature-reader"),
            &["literature-scout"],
        )];
        let mut spec = base_spec();
        spec.role = Some("literature-scout".to_string());
        let task = create_task_with_roles(&store, spec, &roles, &genres, now()).expect("create");
        assert_eq!(task.genre.as_deref(), Some("literature"));
    }

    /// ADR-0027 D1: `genres` が設定されているとき、知らない `genre` はエラー、`genre` + `role` の
    /// 不整合（`role` がその分野の `roles` に無い）もエラー（何も挿入しない）。`genres` が空の設定
    /// （`--config` 無しの `celerisctl add`）では検証しない。
    #[test]
    fn create_task_with_roles_rejects_unknown_genre_and_role_genre_mismatch_only_when_genres_configured()
     {
        let store = SqliteStore::open_in_memory().expect("open store");
        let genres = vec![genre(
            "coding",
            Some("implementer"),
            &["lead", "implementer"],
        )];

        let mut spec = base_spec();
        spec.genre = Some("literature".to_string());
        let err = create_task_with_roles(&store, spec, &[], &genres, now()).unwrap_err();
        assert!(matches!(err, OpsError::Validation(_)), "{err:?}");
        assert!(store.list(None).expect("list").is_empty());

        let mut spec = base_spec();
        spec.genre = Some("coding".to_string());
        spec.role = Some("literature-scout".to_string());
        let err = create_task_with_roles(&store, spec, &[], &genres, now()).unwrap_err();
        assert!(matches!(err, OpsError::Validation(_)), "{err:?}");
        assert!(store.list(None).expect("list").is_empty());

        // `genres` が空: 分野を使わない設定では検証しない（自由記述のまま保存する）。
        let mut spec = base_spec();
        spec.genre = Some("literature".to_string());
        let task =
            create_task_with_roles(&store, spec, &[], &[], now()).expect("no genres configured");
        assert_eq!(task.genre.as_deref(), Some("literature"));
    }

    #[test]
    fn create_task_with_approval_kind_starts_ready() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.kind = TaskKind::Approval;

        let task = create_task(&store, spec, now()).expect("create_task");
        assert_eq!(task.status, Status::Ready);
    }

    #[test]
    fn create_task_preserves_acceptance_order_as_given() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.acceptance = vec![
            CriterionSpec::Human {
                text: "human check".to_string(),
            },
            CriterionSpec::Command {
                cmd: "cargo test".to_string(),
                expect_exit: 0,
            },
            CriterionSpec::ArtifactExists {
                name: "bench.json".to_string(),
            },
            CriterionSpec::Reviewer {
                text: "looks good".to_string(),
            },
        ];

        let task = create_task(&store, spec, now()).expect("create_task");
        assert_eq!(task.acceptance.len(), 4);
        assert_eq!(task.acceptance[0].check, Check::Human);
        assert!(matches!(task.acceptance[1].check, Check::Command { .. }));
        assert!(matches!(
            task.acceptance[2].check,
            Check::ArtifactExists { .. }
        ));
        assert_eq!(task.acceptance[3].check, Check::Reviewer);
    }

    #[test]
    fn create_task_check_cmd_produces_command_criterion_with_expected_text() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.acceptance = vec![CriterionSpec::Command {
            cmd: "cargo test".to_string(),
            expect_exit: 0,
        }];

        let task = create_task(&store, spec, now()).expect("create_task");
        assert_eq!(task.acceptance.len(), 1);
        assert_eq!(task.acceptance[0].text, "`cargo test` exits 0");
        assert_eq!(
            task.acceptance[0].check,
            Check::Command {
                cmd: "cargo test".to_string(),
                expect_exit: 0
            }
        );
    }

    #[test]
    fn create_task_check_artifact_produces_artifact_exists_criterion_with_expected_text() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.acceptance = vec![CriterionSpec::ArtifactExists {
            name: "bench.json".to_string(),
        }];

        let task = create_task(&store, spec, now()).expect("create_task");
        assert_eq!(task.acceptance.len(), 1);
        assert_eq!(task.acceptance[0].text, "artifact bench.json exists");
        assert_eq!(
            task.acceptance[0].check,
            Check::ArtifactExists {
                name: "bench.json".to_string()
            }
        );
    }

    #[test]
    fn create_task_check_reviewer_produces_reviewer_criterion_with_text_verbatim() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.acceptance = vec![CriterionSpec::Reviewer {
            text: "the diff is minimal and well-tested".to_string(),
        }];

        let task = create_task(&store, spec, now()).expect("create_task");
        assert_eq!(task.acceptance.len(), 1);
        assert_eq!(
            task.acceptance[0].text,
            "the diff is minimal and well-tested"
        );
        assert_eq!(task.acceptance[0].check, Check::Reviewer);
    }

    #[test]
    fn create_task_without_workspace_defaults_to_relative_task_id_path() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.workspace = None;

        let task = create_task(&store, spec, now()).expect("create_task");
        assert_eq!(
            task.workspace,
            WorkspaceSpec::Local {
                path: PathBuf::from(task.id.to_string()),
                mode: None
            }
        );
    }

    #[test]
    fn create_task_without_any_acceptance_criterion_errors_and_inserts_nothing() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.acceptance = vec![];

        let result = create_task(&store, spec, now());
        assert!(matches!(result, Err(OpsError::Validation(_))));
        assert!(store.list(None).expect("list tasks").is_empty());
    }

    #[test]
    fn create_task_with_missing_dependency_errors_and_inserts_nothing() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.depends_on = vec![TaskId::new()];

        let result = create_task(&store, spec, now());
        assert!(matches!(result, Err(OpsError::Validation(_))));
        assert!(store.list(None).expect("list tasks").is_empty());
    }

    /// ADR-0018: `cluster` を指定すると `WorkspaceSpec::Remote` になり、`workspace` はクラスタ側のパスになる。
    /// ADR-0059 D1: `workspace_mode` を省略すると `mode: None`（従来どおりクラスタの `sync` に従う）。
    #[test]
    fn create_task_with_cluster_makes_a_remote_workspace() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut spec = base_spec();
        spec.cluster = Some("pegasus".to_string());
        spec.workspace = Some(PathBuf::from("/work/NBB/rmaeda/workspace/rust/benchfs"));
        let task = create_task(&store, spec, now()).expect("create");
        assert_eq!(
            task.workspace,
            task_core::WorkspaceSpec::Remote {
                cluster: "pegasus".to_string(),
                path: PathBuf::from("/work/NBB/rmaeda/workspace/rust/benchfs"),
                mode: None,
            }
        );

        // workspace を省略するとタスク ID のディレクトリ（クラスタ側の相対パス）になる。
        let mut spec = base_spec();
        spec.cluster = Some("pegasus".to_string());
        spec.workspace = None;
        let task = create_task(&store, spec, now()).expect("create");
        assert_eq!(
            task.workspace,
            task_core::WorkspaceSpec::Remote {
                cluster: "pegasus".to_string(),
                path: PathBuf::from(task.id.to_string()),
                mode: None,
            }
        );

        // ADR-0059 D1: `workspace_mode = "shared"` を明示すると `mode: Some(Shared)` になる。
        let mut spec = base_spec();
        spec.cluster = Some("pegasus".to_string());
        spec.workspace = Some(PathBuf::from("~"));
        spec.workspace_mode = Some(task_core::WorkspaceMode::Shared);
        let task = create_task(&store, spec, now()).expect("create");
        assert_eq!(
            task.workspace,
            task_core::WorkspaceSpec::Remote {
                cluster: "pegasus".to_string(),
                path: PathBuf::from("~"),
                mode: Some(task_core::WorkspaceMode::Shared),
            }
        );
    }

    /// ADR-0014 D3（P-G16）: 空白だけの title / objective、存在しない親は検証エラーで、何も挿入しない。
    #[test]
    fn create_task_rejects_blank_title_or_objective_and_missing_parent() {
        type Mutate = fn(&mut NewTaskSpec);
        let store = SqliteStore::open_in_memory().expect("open store");
        let cases: Vec<(Mutate, &str)> = vec![
            (|s| s.title = "  ".into(), "title must not be blank"),
            (|s| s.objective = "\n".into(), "objective must not be blank"),
            (|s| s.parent = Some(TaskId::new()), "does not exist"),
        ];
        for (mutate, expected) in cases {
            let mut spec = base_spec();
            mutate(&mut spec);
            match create_task(&store, spec, now()) {
                Err(OpsError::Validation(msg)) => assert!(msg.contains(expected), "{msg}"),
                other => {
                    panic!("expected a validation error containing {expected:?}, got {other:?}")
                }
            }
        }
        assert!(store.list(None).expect("list tasks").is_empty());

        let parent = create_task(&store, base_spec(), now()).expect("parent");
        let mut child = base_spec();
        child.parent = Some(parent.id);
        assert_eq!(
            create_task(&store, child, now()).expect("child").parent_id,
            Some(parent.id)
        );
    }

    #[test]
    fn create_task_with_failed_dependency_errors_and_inserts_nothing() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let mut dep_spec = base_spec();
        dep_spec.workspace = Some(PathBuf::from("/tmp/dep"));
        let dep = create_task(&store, dep_spec, now()).expect("create dep");

        // Drive the dependency to `failed` via a valid path:
        // draft -> accept -> ready -> acquire_lease -> running -> worker_error(false) -> failed.
        store
            .apply_transition(dep.id, task_core::Trigger::Accept, None)
            .expect("accept dep");
        let acquired = store
            .acquire_lease(dep.id, "run-dep", std::time::Duration::from_secs(60))
            .expect("acquire lease");
        assert!(acquired);
        store
            .apply_transition(
                dep.id,
                task_core::Trigger::WorkerError { retryable: false },
                None,
            )
            .expect("fail dep");
        assert_eq!(
            store.get(dep.id).expect("get").expect("some").status,
            Status::Failed
        );

        let mut spec = base_spec();
        spec.depends_on = vec![dep.id];

        let result = create_task(&store, spec, now());
        assert!(result.is_err());
        // Only the dependency task should exist; the new task must not be inserted.
        assert_eq!(store.list(None).expect("list tasks").len(), 1);
    }
}
