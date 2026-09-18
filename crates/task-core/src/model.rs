//! ドメインモデル型。DESIGN.md §4 を実装する。純粋なデータ定義のみで、
//! I/O・LLM呼び出し・プロセス起動を行うロジックはここに置かない（ADR-0001 D2）。

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

/// タスクの一意識別子（ULID）。DESIGN §4.1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
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

/// DESIGN §5.8 の境界。`Remote{cluster, path}` は `[[clusters]] id` と**クラスタ側の**作業ディレクトリ（ADR-0018、Phase 12）。
/// taskd はその写しを `workspace_root/<task_id>` に持ち、コマンドはクラスタで実行する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceSpec {
    Local { path: PathBuf },
    Remote { cluster: String, path: PathBuf },
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
    /// ADR-0033 D4（Phase 24）: 対話由来のタスクなら、きっかけになった人の発言（`messages.id`）。
    /// run が終わると、その結果が `assignee` のノードの返事として `messages` に入る。
    /// **DB の列は増やさない**（`json` 列の中だけ。導入前のタスクには無いので任意）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<crate::message::MessageId>,
}

/// ADR-0016 D1: `[[roles]]` の 1 行。役割ごとの既定（タスクの値 > 役割の既定 > 全体の既定）とプロンプトに前置きする指示文。
/// 純粋なデータ。taskd の設定から写し、task-ops（作成時の既定）とディスパッチャ（run 時の指示文）が使う。
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
/// 純粋なデータ。taskd の設定から写し、task-ops（作成時の既定・検証）とディスパッチャ（run 時のプロンプト）が使う。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GenreSpec {
    pub id: String,
    pub description: String,
    /// ADR-0028 D1: この分野で「できること」の自由記述の一覧（固定 enum にしない）。空なら出力にも出さない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// ADR-0028 D1: この分野に投げるときに用意すべきものの目安（自由記述。taskd は中身を検査しない）。
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
        let mut matching = genres.iter().filter(|g| g.roles.iter().any(|r| r == role_id));
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
        self.output_artifacts.iter().map(|a| artifact_entry_name(a)).collect()
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
        self.harness_adapter(roles).is_some_and(|a| HARNESS_ADAPTERS.contains(&a))
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
    if description.is_empty() { None } else { Some(description) }
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
    WorkerProgress {
        run_id: String,
        msg: String,
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
        assert_eq!(artifact_entry_name("papers.json: 検索した論文の一覧"), "papers.json");
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
        let bare = vec![RoleSpec { id: "literature-reader".into(), ..RoleSpec::default() }];
        assert_eq!(literature().harness_adapter(&bare), None);
        assert!(!literature().is_harness(&bare));
        let no_default = GenreSpec { default_role: None, ..literature() };
        assert!(!no_default.is_harness(&paperqa));
    }
}
