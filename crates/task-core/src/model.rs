//! ドメインモデル型。DESIGN.md §4 を実装する。純粋なデータ定義のみで、
//! I/O・LLM呼び出し・プロセス起動を行うロジックはここに置かない（ADR-0001 D2）。

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

/// タスクの一意識別子（ULID）。DESIGN §4.1。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct TaskId(#[schemars(with = "String")] pub Ulid);

impl TaskId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for TaskId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// DESIGN §4.1 の `TaskKind`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Plan,
    Execute,
    Review,
    Approval,
}

/// ADR-0002 D1 の状態集合。終端は `done | failed | cancelled`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Draft,
    Ready,
    Running,
    Blocked,
    Reviewing,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    /// ADR-0002 D1: 終端状態 = `done | failed | cancelled`。
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Done | Status::Failed | Status::Cancelled)
    }
}

/// DESIGN §5.4 の `WorkerHint`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Frontier,
    Standard,
    Cheap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkerHint {
    pub tier: Tier,
    pub adapter: Option<String>,
}

/// ADR-0041 D1: ローカルの作業場所の使い方。並列のタスクが同じ作業ツリーで `git checkout` して
/// 互いの未コミット変更を壊すのを止めるため、既定ではタスクごとに worktree を切る。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    /// 既定。`path` が git リポジトリなら、タスクごとに `git worktree` を切ってその中で作業する。
    #[default]
    Worktree,
    /// 従来どおり `path` をそのまま作業ディレクトリにする（自分専用の使い捨てリポジトリ向け）。
    Shared,
}

/// DESIGN §5.8 の境界。`Remote{cluster, path}` は `[[clusters]] id` と**クラスタ側の**作業ディレクトリ（ADR-0018、Phase 12）。
/// celeris はその写しを `workspace_root/<task_id>` に持ち、コマンドはクラスタで実行する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceSpec {
    Local {
        path: PathBuf,
        /// ADR-0041 D1: 省略時は `Worktree`。省略したものは JSON にも出さない（Phase 48 までの
        /// `{"kind":"local","path":"…"}` と 1 バイトも変わらない）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<WorkspaceMode>,
    },
    Remote {
        cluster: String,
        path: PathBuf,
    },
}

impl WorkspaceSpec {
    /// ADR-0005 D3 以来の `Local{path}`（`mode` は省略 = 既定の `Worktree`）。
    pub fn local(path: impl Into<PathBuf>) -> WorkspaceSpec {
        WorkspaceSpec::Local {
            path: path.into(),
            mode: None,
        }
    }

    /// ADR-0041 D1: ローカルの作業場所の使い方。`Remote` は従来の経路（`Shared` 相当）。
    pub fn local_mode(&self) -> WorkspaceMode {
        match self {
            WorkspaceSpec::Local { mode, .. } => mode.unwrap_or_default(),
            WorkspaceSpec::Remote { .. } => WorkspaceMode::Shared,
        }
    }

    /// ADR-0039 D5: `Local` の `~` / `~/…` を `home` で展開した複製。`Remote` の `~` は**クラスタ側の home**
    /// なので触らない（celeris には展開できない）。`home` が無い、`~` で始まらないときはそのまま。
    pub fn with_home_expanded(&self, home: Option<&std::path::Path>) -> WorkspaceSpec {
        match self {
            WorkspaceSpec::Local { path, mode } => WorkspaceSpec::Local {
                path: expand_home(path, home),
                mode: *mode,
            },
            other => other.clone(),
        }
    }
}

/// ADR-0039 D5: 先頭の `~`（単独か `~/…`）を `home` に置き換える。それ以外は何もしない（純粋関数）。
/// `~user` のような別ユーザ指定は展開しない（celeris はその home を知らない）。
pub fn expand_home(path: &std::path::Path, home: Option<&std::path::Path>) -> PathBuf {
    let Some(home) = home else {
        return path.to_path_buf();
    };
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home.to_path_buf();
    }
    match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => path.to_path_buf(),
    }
}

/// ADR-0039 D5: `$HOME`（空文字列は無しとみなす）。`~` の展開の入口（API・分解・委譲）だけが使う。
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// DESIGN §4.1 の `Budget`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Budget {
    pub max_turns: u32,
    pub max_wall_secs: u64,
    pub max_retries: u32,
}

/// DESIGN §4.1 の `Lease`。ADR-0002 D7: `expires_at` は
/// `budget.max_wall_secs + 猶予` から `acquire_lease` 呼び出し側が計算する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Lease {
    pub worker_run_id: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub expires_at: OffsetDateTime,
}

/// DESIGN §5.3/§5.7 の `Check` 種別。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Check {
    Command { cmd: String, expect_exit: i32 },
    ArtifactExists { name: String },
    Reviewer,
    Human,
}

/// DESIGN §4.1 の `Criterion`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Criterion {
    pub text: String,
    pub check: Check,
}

/// DESIGN §4.4 の `ArtifactRef`。実体は `workspace/<task_id>/artifacts/` 配下。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactRef {
    pub name: String,
    pub path: String,
    pub sha256: String,
    pub kind: String,
}

/// ADR-0044 D3: タスクの種類。既定は `Other`。状態機械は見ない（人とボードのための分類）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskCategory {
    Feature,
    Bug,
    Research,
    Ops,
    Docs,
    #[default]
    Other,
}

impl TaskCategory {
    /// serde の `skip_serializing_if` 用。既定の `other` は JSON に出さないので、導入前のタスクの
    /// JSON と 1 バイトも変わらない。
    pub fn is_default(&self) -> bool {
        *self == TaskCategory::Other
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TaskCategory::Feature => "feature",
            TaskCategory::Bug => "bug",
            TaskCategory::Research => "research",
            TaskCategory::Ops => "ops",
            TaskCategory::Docs => "docs",
            TaskCategory::Other => "other",
        }
    }

    /// snake_case の名前から引く（API のクエリと PATCH の検証に使う）。
    pub fn parse(s: &str) -> Option<TaskCategory> {
        match s {
            "feature" => Some(TaskCategory::Feature),
            "bug" => Some(TaskCategory::Bug),
            "research" => Some(TaskCategory::Research),
            "ops" => Some(TaskCategory::Ops),
            "docs" => Some(TaskCategory::Docs),
            "other" => Some(TaskCategory::Other),
            _ => None,
        }
    }
}

/// ADR-0046 D4（Phase 59）: タスクの進め方。前置きに足す規則とレビューの厳しさを切り替える。
/// 既定は `Production`（導入前のタスクは全部これ。従来の挙動と同じ）。状態機械は見ない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskMode {
    /// 動くことを最短で示す。レビューは明示の `acceptance` だけ（リポジトリの `check` は使わない）。
    Prototype,
    /// 既定。`acceptance` ＋ リポジトリの `check`（ADR-0043 D4）。
    #[default]
    Production,
    /// 主張には出典か計測を付ける。`acceptance` ＋ 結果に `sources`（または計測の記録）が無ければ不合格。
    Research,
}

impl TaskMode {
    /// serde の `skip_serializing_if` 用。既定の `production` は JSON に出さないので、導入前のタスクの
    /// JSON と 1 バイトも変わらない。
    pub fn is_default(&self) -> bool {
        *self == TaskMode::Production
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TaskMode::Prototype => "prototype",
            TaskMode::Production => "production",
            TaskMode::Research => "research",
        }
    }

    pub fn parse(s: &str) -> Option<TaskMode> {
        match s {
            "prototype" => Some(TaskMode::Prototype),
            "production" => Some(TaskMode::Production),
            "research" => Some(TaskMode::Research),
            _ => None,
        }
    }
}

/// ADR-0046 D2: 1 タスクに書ける skill（必要な能力タグ）の上限。
pub const MAX_SKILLS: usize = 12;

/// ADR-0046 D2: skill の一覧を検証する（重複は取り除き、順は保つ）。違反があれば理由を返す。
pub fn normalize_skills(skills: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::with_capacity(skills.len());
    for skill in skills {
        if !crate::profile::is_valid_skill(skill) {
            return Err(format!(
                "skill {skill:?} must match [a-z0-9._-] (lowercase, 1..=64 characters)"
            ));
        }
        if !out.iter().any(|s| s == skill) {
            out.push(skill.clone());
        }
    }
    if out.len() > MAX_SKILLS {
        return Err(format!(
            "at most {MAX_SKILLS} skills are allowed (got {})",
            out.len()
        ));
    }
    Ok(out)
}

/// ADR-0044 D3: 1 タスクに付けられるラベルの上限。
pub const MAX_LABELS: usize = 8;

/// ADR-0044 D3: ラベルは小文字の `[a-z0-9-]` だけ（空でない、64 文字以内）。純粋関数。
pub fn is_valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.chars().count() <= 64
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// ADR-0044 D3: ラベルの一覧を検証する（重複は取り除き、順は保つ）。違反があれば理由を返す。
pub fn normalize_labels(labels: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::with_capacity(labels.len());
    for label in labels {
        if !is_valid_label(label) {
            return Err(format!(
                "label {label:?} must match [a-z0-9-] (lowercase, 1..=64 characters)"
            ));
        }
        if !out.iter().any(|l| l == label) {
            out.push(label.clone());
        }
    }
    if out.len() > MAX_LABELS {
        return Err(format!(
            "at most {MAX_LABELS} labels are allowed (got {})",
            out.len()
        ));
    }
    Ok(out)
}

/// ADR-0044 D3: 優先度のラベル（P0〜P3）と `Task.priority`（`i32`。大きいほど先）の対応。
/// P0 = 30 / P1 = 20 / P2 = 10 / P3 = 0。既定は P2。
pub const PRIORITY_LABELS: [(&str, i32); 4] = [("P0", 30), ("P1", 20), ("P2", 10), ("P3", 0)];

/// ADR-0044 D3: 既定の優先度（P2 = 10）。
pub const DEFAULT_PRIORITY: i32 = 10;

/// ADR-0044 D3: `"P1"` のようなラベルを `i32` に写す（大文字小文字は区別しない）。知らない値は `None`。
pub fn priority_from_label(label: &str) -> Option<i32> {
    let upper = label.trim().to_ascii_uppercase();
    PRIORITY_LABELS
        .iter()
        .find(|(name, _)| *name == upper)
        .map(|(_, v)| *v)
}

/// ADR-0044 D3: `i32` を P0〜P3 に丸めて返す（30 以上 = P0、20..30 = P1、10..20 = P2、10 未満 = P3）。
pub fn priority_label(priority: i32) -> &'static str {
    if priority >= 30 {
        "P0"
    } else if priority >= 20 {
        "P1"
    } else if priority >= 10 {
        "P2"
    } else {
        "P3"
    }
}

/// DESIGN §4.1 の `Task`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Task {
    pub id: TaskId,
    pub parent_id: Option<TaskId>,
    pub kind: TaskKind,
    pub title: String,
    pub objective: String,
    pub acceptance: Vec<Criterion>,
    pub inputs: Vec<ArtifactRef>,
    pub depends_on: Vec<TaskId>,
    pub status: Status,
    pub priority: i32,
    pub worker_hint: WorkerHint,
    pub workspace: WorkspaceSpec,
    // ---- ADR-0043 D2（Phase 52）: このタスクが使う案件のリポジトリ ----
    /// ADR-0043 D2: このタスクが使う案件のリポジトリ（`project_repos`）。空なら継承の規則
    /// （明示 > 親 > 案件の primary）で決まった結果が空だった、または案件にリポジトリが無い
    /// （純粋な調査などコードを伴わないタスク）。`repos[0]` がワーカーのカレントディレクトリになる。
    /// 導入前のタスクには無いので既定は空（従来どおり `workspace` 1 つで動く）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<crate::repos::RepoRef>,
    // ---- ここまで ADR-0043 D2 ----
    pub budget: Budget,
    pub attempts: u32,
    pub lease: Option<Lease>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub updated_at: OffsetDateTime,
    /// ADR-0016 D1: 役割名（自由記述。`[[roles]] id` と一致すれば既定と指示文が効く）。状態機械は見ない。
    /// 導入前のタスクには無いので任意。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// ADR-0027 D1: 分野名（自由記述。`[[genres]] id` と一致すれば既定の役割・プロンプトの説明が効く）。
    /// 状態機械は見ない。導入前のタスクには無いので任意。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// ADR-0016 D3: true なら、委譲した子が全て終端になった後に集約 run を 1 回だけ行い `artifacts/summary.md` を作らせる。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub aggregate: bool,
    /// ADR-0033 D2: このタスクが属する案件。導入前のタスク・案件に属さないタスクには無い。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<crate::org::ProjectId>,
    /// ADR-0033 D2: このタスクが属する途中目標（`project_id` の案件のもの）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone_id: Option<crate::org::MilestoneId>,
    /// ADR-0033 D2: 割り当てられた組織のノード（`org_nodes.id`）。あれば `worker_hint` の解決で
    /// 役割・分野より先に見る。無ければ従来どおり（互換）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    // ---- ADR-0044 D3（Phase 53）: ラベルと種類。ここから（ADR-0043 D2 の `repos` はこの外に足す）----
    /// ADR-0044 D3: 自由なラベル（小文字・`[a-z0-9-]`・最大 8 個）。導入前のタスクには無いので既定は空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// ADR-0044 D3: 種類（既定 `other`）。導入前のタスクには無いので既定で埋まる。
    #[serde(default, skip_serializing_if = "TaskCategory::is_default")]
    pub category: TaskCategory,
    // ---- ADR-0044 D3（Phase 53）: ここまで ----
    // ---- ADR-0046 D2 / D4（Phase 59）: 必要な能力タグと進め方。ここから ----
    /// ADR-0046 D2: このタスクに必要な能力タグ（`org_nodes` の実効 `skills` と突き合わせて担当を決める。
    /// ADR-0046 D5 の matching）。導入前のタスクには無いので既定は空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// ADR-0046 D4: 進め方（`prototype` / `production` / `research`。既定 `production`）。
    #[serde(default, skip_serializing_if = "TaskMode::is_default")]
    pub mode: TaskMode,
    // ---- ADR-0046 D2 / D4（Phase 59）: ここまで ----
    /// ADR-0033 D4（Phase 24）: 対話由来のタスクなら、きっかけになった人の発言（`messages.id`）。
    /// run が終わると、その結果が `assignee` のノードの返事として `messages` に入る。
    /// **DB の列は増やさない**（`json` 列の中だけ。導入前のタスクには無いので任意）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<crate::message::MessageId>,
}

/// ADR-0016 D1: `[[roles]]` の 1 行。役割ごとの既定（タスクの値 > 役割の既定 > 全体の既定）とプロンプトに前置きする指示文。
/// 純粋なデータ。celeris の設定から写し、task-ops（作成時の既定）とディスパッチャ（run 時の指示文）が使う。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoleSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_secs: Option<u64>,
    /// ワーカーのプロンプトに前置きする指示文（何を任され、何を任せてよいか）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl RoleSpec {
    /// `roles` から `id` の行を探す。
    pub fn find<'a>(roles: &'a [RoleSpec], id: &str) -> Option<&'a RoleSpec> {
        roles.iter().find(|r| r.id == id)
    }
}

/// ADR-0027 D1: `[[genres]]` の 1 行。分野の説明・既定の役割・分野に属する役割の一覧。
/// 分野そのものにはアダプタを持たせない（D2: `default_role` が指す役割が持つ）。
/// 純粋なデータ。celeris の設定から写し、task-ops（作成時の既定・検証）とディスパッチャ（run 時のプロンプト）が使う。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GenreSpec {
    pub id: String,
    pub description: String,
    /// ADR-0028 D1: この分野で「できること」の自由記述の一覧（固定 enum にしない）。空なら出力にも出さない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// ADR-0028 D1: この分野に投げるときに用意すべきものの目安（自由記述。celeris は中身を検査しない）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_artifacts: Vec<String>,
    /// ADR-0028 D1: この分野から戻ってくるものの目安（自由記述）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_artifacts: Vec<String>,
    /// タスクに `role` が無いときに、この分野の既定として使う役割 id（`roles` に含まれること。設定検証で確認する）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_role: Option<String>,
    /// この分野に属する役割 id の一覧。`genre` と `role` を両方指定したタスクは、`role` がここに無ければ設定エラー。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
}

impl GenreSpec {
    /// `genres` から `id` の行を探す。
    pub fn find<'a>(genres: &'a [GenreSpec], id: &str) -> Option<&'a GenreSpec> {
        genres.iter().find(|g| g.id == id)
    }

    /// `role_id` を `roles` に含む分野がちょうど 1 つだけあれば、その id を返す（ADR-0027 D1: 委譲で
    /// 分野を省略したときに役割から分野を推定するため）。0 件・2 件以上は `None`（一意に決まらない）。
    pub fn unique_for_role(genres: &[GenreSpec], role_id: &str) -> Option<String> {
        let mut matching = genres
            .iter()
            .filter(|g| g.roles.iter().any(|r| r == role_id));
        let first = matching.next()?;
        if matching.next().is_some() {
            None
        } else {
            Some(first.id.clone())
        }
    }

    /// Phase 38（ADR-0028 追記）: `output_artifacts` の**名前だけ**（`名前: 説明` の `:` の前）。
    /// 計画の `artifact_exists` の照合に使えるのはこの一覧だけである。
    pub fn output_artifact_names(&self) -> Vec<&str> {
        self.output_artifacts
            .iter()
            .map(|a| artifact_entry_name(a))
            .collect()
    }

    /// Phase 38（ADR-0028 追記）: この分野の担当が動く「ハーネス」のアダプタ id
    /// （`default_role` の役割の `adapter`）。`default_role` が無い・役割が無い・アダプタ指定が無ければ `None`。
    pub fn harness_adapter<'a>(&self, roles: &'a [RoleSpec]) -> Option<&'a str> {
        let default_role = self.default_role.as_deref()?;
        RoleSpec::find(roles, default_role)?.adapter.as_deref()
    }

    /// Phase 38（ADR-0028 追記）: 「ハーネス系の分野」か（決定的。`default_role` のアダプタが
    /// `HARNESS_ADAPTERS` のどれか）。ハーネス系の担当は成果物の名前を選べないので、計画は
    /// `output_artifacts` の名前だけを `artifact_exists` に使える（Phase 38 の実機の不合格から）。
    pub fn is_harness(&self, roles: &[RoleSpec]) -> bool {
        self.harness_adapter(roles)
            .is_some_and(|a| HARNESS_ADAPTERS.contains(&a))
    }
}

/// Phase 38（ADR-0028 追記）: 成果物の名前を自分で決められない（固定の名前しか書けない）アダプタ。
/// `paperqa` は `answer.md` / `papers.json` / `sources.json` / `queries.json`、
/// `local-deep-research` は `report.md` / `sources.json` / `research.json` しか書かない。
pub const HARNESS_ADAPTERS: [&str; 2] = ["paperqa", "local-deep-research"];

/// Phase 38（ADR-0028 追記）: `input_artifacts` / `output_artifacts` の 1 要素は `名前` か
/// `名前: 説明`。その**名前**の部分（`:` の前。前後の空白は落とす）。
pub fn artifact_entry_name(entry: &str) -> &str {
    match entry.split_once(':') {
        Some((name, _)) => name.trim(),
        None => entry.trim(),
    }
}

/// Phase 38（ADR-0028 追記）: `名前: 説明` の**説明**の部分（無ければ `None`）。
pub fn artifact_entry_description(entry: &str) -> Option<&str> {
    let (_, description) = entry.split_once(':')?;
    let description = description.trim();
    if description.is_empty() {
        None
    } else {
        Some(description)
    }
}

/// DESIGN §5.3 の `usage`。取れない項目は省略可。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// run の役割（ADR-0014 D1）。`Event::WorkerStarted` / `WorkerFinished` の `role`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunRole {
    Worker,
    Reviewer,
}

/// ADR-0048 D2（Phase 60a）: ワーカーの進行の種別。アダプタごとの差はアダプタ側で吸収し、
/// Console（ADR-0048 D1）はこの 5 種だけを知る。`comment` はプロトコルの別 type（ADR-0044 D2）の
/// ままなのでここには無い。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProgressKind {
    /// 道具を使った（`tool` に名前、`summary` に入力の 1 行要約）。
    ToolUse,
    /// 道具の結果（`summary` に先頭の要約、`error` に失敗の印）。
    ToolResult,
    /// モデルの発話。
    Text,
    /// 思考（要約だけ。本文は流さない）。
    Thinking,
    /// アダプタの節目（起動・段取り・終わりなど）。
    Status,
}

impl ProgressKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProgressKind::ToolUse => "tool_use",
            ProgressKind::ToolResult => "tool_result",
            ProgressKind::Text => "text",
            ProgressKind::Thinking => "thinking",
            ProgressKind::Status => "status",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tool_use" => Some(ProgressKind::ToolUse),
            "tool_result" => Some(ProgressKind::ToolResult),
            "text" => Some(ProgressKind::Text),
            "thinking" => Some(ProgressKind::Thinking),
            "status" => Some(ProgressKind::Status),
            _ => None,
        }
    }
}

/// ADR-0048 D2: `detail` の上限（4 KiB）。超えたら切って `truncated = true` にする。
pub const PROGRESS_DETAIL_MAX_BYTES: usize = 4 * 1024;

/// ADR-0048 D2（Phase 60a）: 進行 1 件の構造化フィールド。`Event::WorkerProgress` と
/// ワーカープロトコルの `progress` 行で同じ形を使う。既定（`Default`）は「従来の文字列だけ」。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProgressFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProgressKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
}

impl ProgressFields {
    /// 種別だけ。
    pub fn of(kind: ProgressKind) -> Self {
        Self {
            kind: Some(kind),
            ..Self::default()
        }
    }

    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// `detail` を 4 KiB で切って入れる（切ったら `truncated = true`）。空文字は入れない。
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if detail.is_empty() {
            return self;
        }
        let (text, truncated) = truncate_detail(&detail);
        self.detail = Some(text);
        self.truncated = self.truncated || truncated;
        self
    }

    pub fn with_error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }

    /// 構造化フィールドが 1 つも無い（＝従来の文字列 progress）。
    pub fn is_plain(&self) -> bool {
        self.kind.is_none()
            && self.tool.is_none()
            && self.summary.is_none()
            && self.detail.is_none()
            && !self.truncated
            && !self.error
    }
}

/// `PROGRESS_DETAIL_MAX_BYTES` で切る（UTF-8 の境界を守る）。戻り値の `bool` は切ったか。
pub fn truncate_detail(detail: &str) -> (String, bool) {
    if detail.len() <= PROGRESS_DETAIL_MAX_BYTES {
        return (detail.to_string(), false);
    }
    let mut end = PROGRESS_DETAIL_MAX_BYTES;
    while end > 0 && !detail.is_char_boundary(end) {
        end -= 1;
    }
    (detail[..end].to_string(), true)
}

/// DESIGN §4.3 の `Event`（追記専用）。ADR-0002 D2: `Transitioned` は遷移の
/// *結果* を記録するものであり、`transition()` の入力（`Trigger`）とは別物。
/// `JsonSchema` は ADR-0013 D8: `docs/api/v1/event.schema.json`（`EventRow` 経由）の契約に使う。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Created {
        task: Box<Task>,
    },
    Transitioned {
        from: Status,
        to: Status,
        reason: String,
    },
    WorkerStarted {
        run_id: String,
        adapter: String,
        model: String,
        /// どのプロバイダ（= アカウント）で実行したか（ADR-0012 D1）。導入前のイベントには無いので任意。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// プール（`account_pool = true`）で選ばれた Claude アカウントの id（ADR-0024 D4）。プールを使わない
        /// プロバイダ・導入前のイベントには無い。`provider`（アダプタ×プロバイダ行）とは別軸。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
        /// ADR-0014 D1: `None` はワーカー run、`Some(Reviewer)` は Reviewer run（ワーカー run に `Some(Worker)` は書かない）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<RunRole>,
        /// ADR-0016 D1: run 開始時のタスクの役割名（`Task.role`）。役割の無いタスク・導入前のイベントには無い。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_role: Option<String>,
    },
    /// ADR-0048 D2（Phase 60a）: 構造化した進行。`msg` は従来どおり人が読む 1 行で、
    /// `kind` / `tool` / `summary` / `detail` / `truncated` / `error` は**追加のみ**
    /// （付けないワーカー・導入前のイベントは全て `None` / `false` として読める）。
    /// 構築は `Event::worker_progress` / `Event::worker_progress_with` を使う。
    WorkerProgress {
        run_id: String,
        msg: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ProgressKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        error: bool,
    },
    ArtifactProduced {
        run_id: String,
        artifact: ArtifactRef,
    },
    WorkerFinished {
        run_id: String,
        outcome: String,
        usage: Option<Usage>,
        /// ADR-0014 D1: `WorkerStarted.role` と同じ（`None` はワーカー run）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<RunRole>,
    },
    ReviewVerdict {
        run_id: String,
        criterion_idx: usize,
        pass: bool,
        reason: String,
    },
    ApprovalRequested,
    ApprovalDecided {
        by: String,
        approved: bool,
        note: Option<String>,
    },
    /// `blocked` のタスクへの人間の回答（ADR-0010 D3, P-10）。`Transitioned{reason:"answer"}` と同一トランザクションで
    /// 追記し、次の run の `context.answers` に載せる。
    Answered {
        question: String,
        answer: String,
    },
    /// ADR-0021 D2: ディスパッチャが人間に出した質問（run の終了ではないので `WorkerFinished` は使わない）。
    /// 状態は変えない（`replay` は無視する）。同じトランザクションの `Transitioned{to: blocked}` と対で記録する。
    QuestionRaised {
        /// この質問のきっかけになった run（親の直近の run）。
        run_id: String,
        text: String,
    },
    /// ADR-0016 D2: 実行中の run が `delegate` で提案し、検証を通って挿入された子タスク。状態は変えない（`replay` は無視する）。
    Delegated {
        run_id: String,
        task_ids: Vec<TaskId>,
    },
    /// ADR-0018 D2: クラスタへの ssh 多重接続が無く、そのクラスタでは実行できない（人のログイン待ち）。
    ClusterUnavailable {
        cluster: String,
        /// `~/.ssh/config` の `Host` 名（`scripts/cluster-login.sh <host>` を案内するため）。第 1 段階の行には無いので任意。
        #[serde(default)]
        host: String,
        reason: String,
    },
    ProviderThrottled {
        provider: String,
        #[serde(with = "time::serde::rfc3339")]
        #[schemars(with = "String")]
        until: OffsetDateTime,
        /// 供給側失敗の種別（ADR-0013 D9）: `throttled | auth_failed | exhausted | spawn`。
        /// 導入前のイベントには無いので任意。このバリアントを構築している箇所は現状無い
        /// （`grep -rn ProviderThrottled crates` で確認済み）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Phase 31（実機の事故、2026-09-18）: `failed`/`cancelled` を複製してやり直したときの新しいタスクに
    /// 記録する。`from` = 元のタスク。状態は変えない（`Created` が初期状態を与える）。
    Retried {
        from: TaskId,
    },
    /// ADR-0044 D1（Phase 53）: 人がタスクを編集した（`PATCH /tasks/{id}`）。`fields` は変えた項目の名前
    /// （`title` / `priority` / `labels` …。並びは決定的）、`by` は `"human"`。状態は変えない
    /// （`replay` は無視する）。
    Edited {
        fields: Vec<String>,
        by: String,
    },
    /// ADR-0046 D5（Phase 59）: `assignee` が無いタスクの担当を matching が決めた。状態は変えない
    /// （`replay` は無視する）。GUI のタスク画面が「なぜこの担当か」をこの 1 件から出す。
    Assigned {
        /// 決まった担当（`org_nodes.id`）。
        node: String,
        /// タスクの skills とノードの実効 skills の重なりの数。
        score: usize,
        /// 決め手（決定的な文面。LLM は使わない）。
        reason: String,
    },
}

impl Event {
    /// 従来どおりの文字列だけの進行（ADR-0048 D2 の構造化フィールドは付けない）。
    pub fn worker_progress(run_id: impl Into<String>, msg: impl Into<String>) -> Self {
        Event::WorkerProgress {
            run_id: run_id.into(),
            msg: msg.into(),
            kind: None,
            tool: None,
            summary: None,
            detail: None,
            truncated: false,
            error: false,
        }
    }

    /// ADR-0048 D2: 構造化した進行。
    pub fn worker_progress_with(
        run_id: impl Into<String>,
        msg: impl Into<String>,
        fields: ProgressFields,
    ) -> Self {
        Event::WorkerProgress {
            run_id: run_id.into(),
            msg: msg.into(),
            kind: fields.kind,
            tool: fields.tool,
            summary: fields.summary,
            detail: fields.detail,
            truncated: fields.truncated,
            error: fields.error,
        }
    }

    /// ADR-0048 D2: `WorkerProgress` の構造化フィールドを取り出す（他のイベントは `None`）。
    pub fn progress_fields(&self) -> Option<ProgressFields> {
        match self {
            Event::WorkerProgress {
                kind,
                tool,
                summary,
                detail,
                truncated,
                error,
                ..
            } => Some(ProgressFields {
                kind: *kind,
                tool: tool.clone(),
                summary: summary.clone(),
                detail: detail.clone(),
                truncated: *truncated,
                error: *error,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literature() -> GenreSpec {
        GenreSpec {
            id: "literature".into(),
            description: "related work".into(),
            output_artifacts: vec![
                "answer.md: 引用付きの答え".into(),
                "papers.json: 検索した論文の一覧（コーパス）".into(),
                "sources.json".into(),
            ],
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-reader".into()],
            ..GenreSpec::default()
        }
    }

    /// Phase 38（ADR-0028 追記）: `output_artifacts` の 1 要素は `名前` でも `名前: 説明` でもよい。
    #[test]
    fn artifact_entries_may_carry_a_description_after_the_colon() {
        assert_eq!(artifact_entry_name("answer.md"), "answer.md");
        assert_eq!(artifact_entry_description("answer.md"), None);
        assert_eq!(
            artifact_entry_name("papers.json: 検索した論文の一覧"),
            "papers.json"
        );
        assert_eq!(
            artifact_entry_description("papers.json: 検索した論文の一覧"),
            Some("検索した論文の一覧")
        );
        // 説明が空（`名前:` だけ）なら説明なし扱い。前後の空白は落ちる。
        assert_eq!(artifact_entry_name("  report.md :  "), "report.md");
        assert_eq!(artifact_entry_description("report.md:   "), None);
        assert_eq!(
            literature().output_artifact_names(),
            vec!["answer.md", "papers.json", "sources.json"]
        );
    }

    /// Phase 38（ADR-0028 追記）: 「ハーネス系の分野」は `default_role` の役割のアダプタで決まる（決定的）。
    #[test]
    fn a_genre_is_a_harness_genre_when_its_default_role_runs_paperqa_or_ldr() {
        let paperqa = vec![RoleSpec {
            id: "literature-reader".into(),
            adapter: Some("paperqa".into()),
            ..RoleSpec::default()
        }];
        assert_eq!(literature().harness_adapter(&paperqa), Some("paperqa"));
        assert!(literature().is_harness(&paperqa));

        let ldr = vec![RoleSpec {
            id: "literature-reader".into(),
            adapter: Some("local-deep-research".into()),
            ..RoleSpec::default()
        }];
        assert!(literature().is_harness(&ldr));

        // claude-code / codex / acp / アダプタ指定なし / `default_role` なしはハーネス系でない。
        let coding = vec![RoleSpec {
            id: "literature-reader".into(),
            adapter: Some("claude-code".into()),
            ..RoleSpec::default()
        }];
        assert_eq!(literature().harness_adapter(&coding), Some("claude-code"));
        assert!(!literature().is_harness(&coding));
        let bare = vec![RoleSpec {
            id: "literature-reader".into(),
            ..RoleSpec::default()
        }];
        assert_eq!(literature().harness_adapter(&bare), None);
        assert!(!literature().is_harness(&bare));
        let no_default = GenreSpec {
            default_role: None,
            ..literature()
        };
        assert!(!no_default.is_harness(&paperqa));
    }

    /// ADR-0041 D1: `Local` の `mode` は省略でき（既定 `worktree`）、省略したものは JSON にも出ない。
    #[test]
    fn the_local_workspace_mode_defaults_to_worktree_and_stays_out_of_the_json_when_omitted() {
        let plain: WorkspaceSpec =
            serde_json::from_str(r#"{"kind":"local","path":"/srv/repo"}"#).expect("parse");
        assert_eq!(
            plain,
            WorkspaceSpec::Local {
                path: PathBuf::from("/srv/repo"),
                mode: None
            }
        );
        assert_eq!(
            plain.local_mode(),
            WorkspaceMode::Worktree,
            "既定は worktree"
        );
        // Phase 48 までと 1 バイトも変わらない。
        assert_eq!(
            serde_json::to_string(&plain).expect("json"),
            r#"{"kind":"local","path":"/srv/repo"}"#
        );
        assert_eq!(WorkspaceSpec::local("/srv/repo"), plain);

        for (text, mode) in [
            ("shared", WorkspaceMode::Shared),
            ("worktree", WorkspaceMode::Worktree),
        ] {
            let spec: WorkspaceSpec = serde_json::from_str(&format!(
                r#"{{"kind":"local","path":"/srv/repo","mode":"{text}"}}"#
            ))
            .expect("parse");
            assert_eq!(spec.local_mode(), mode);
            assert!(
                serde_json::to_string(&spec)
                    .expect("json")
                    .contains(&format!(r#""mode":"{text}""#))
            );
        }

        // 知らない値は受け付けない。`Remote` は従来の経路（`Shared` 相当）。
        assert!(
            serde_json::from_str::<WorkspaceSpec>(r#"{"kind":"local","path":"/x","mode":"bogus"}"#)
                .is_err()
        );
        let remote = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::from("/work/x"),
        };
        assert_eq!(remote.local_mode(), WorkspaceMode::Shared);

        // `~` の展開で `mode` は落ちない（ADR-0039 D5）。
        let home = PathBuf::from("/home/u");
        let tilde = WorkspaceSpec::Local {
            path: PathBuf::from("~/repo"),
            mode: Some(WorkspaceMode::Shared),
        };
        assert_eq!(
            tilde.with_home_expanded(Some(&home)),
            WorkspaceSpec::Local {
                path: PathBuf::from("/home/u/repo"),
                mode: Some(WorkspaceMode::Shared)
            }
        );
    }
}
