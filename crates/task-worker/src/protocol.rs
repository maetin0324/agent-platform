//! ワーカープロトコル v1 の型（DESIGN §5.3, ADR-0003 D2, `docs/protocol/worker-protocol.md`）。
//! JSON Schema は `schemars` で生成し `docs/protocol/worker-protocol.schema.json` と
//! テストで一致を検証する（ADR-0003 D6）。

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{
    ArtifactRef, BudgetKind, DelegateTask, GenreSpec, ProgressFields, ProgressKind, Status, Task,
    TaskId, Usage,
};

/// `run.protocol`。v2（ADR-0016 M9）: `delegate` メッセージ、`context.role`、`context.children`、`task.role` / `task.aggregate` を追加。
/// v3（ADR-0027 D1）: `context.available_genres`、`task.genre`、`delegate` の `tasks[].genre` を追加。
/// v4（ADR-0033 D4/D6）: `context.node` / `context.memory` / `context.conversation` / `context.standing_rules` /
/// `context.organization`、`delegate` の `tasks[].assignee`、結果ファイルの `memory` を追加。
/// Phase 28（ADR-0033 D4 追記）: `context.conversation_addressee` を追加（対話 run は返事だけ。委譲不可）。
/// Phase 30（ADR-0033 D4 追記）: `context.work_genre` を追加（対話は常に対話用分野で走るが、担当ノード
/// 自身の仕事の分野があれば「仕事で使う道具」として前置きに渡す）。
/// Phase 33（ADR-0033 D4 追記）: `context.recent_work` を追加（対話 run にだけ、その担当の直近の仕事を渡す。
/// 実機で担当が自分の直近の失敗を知らずに聞き返した事故の再発防止）。
/// Phase 35（ADR-0036 D1/D5）: `run.artifacts_dir` を追加（成果物と結果ファイルの置き場。単独タスクでは
/// 従来の `<workspace>/artifacts` と同じ値なので、これを読まないワーカーも単独タスクではそのまま動く）。
/// Phase 38（ADR-0028 追記）: `context.available_genres[].harness` と `context.subject_genre` を追加
/// （計画とレビュアーに「ハーネスで動く分野の成果物の名前は固定」を伝えるため）。
/// Phase 59（ADR-0046 D1/D4/D6）: `context.profile`（実効 profile）、`context.mode`（進め方）、
/// `context.organization[].skills` / `.harnesses` を追加（版数は据え置き。追加だけなので v4 のまま）。
/// Phase 67（ADR-0054 D1）: `context.session`（継続セッションの手がかり）を追加（版数は据え置き）。
/// 全て追加のみで v1〜v3 のワーカーはそのまま動く。
pub const PROTOCOL_VERSION: u32 = 4;

/// 直前のレビュー結果（`context.prior_review[]`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PriorReview {
    pub criterion: usize,
    pub pass: bool,
    pub reason: String,
}

/// `Reviewer` check のための `context.review`（ADR-0007 D5）。`RunRequest.task` は合成した `Review` kind の
/// タスクで、対象タスクの `run` の `done` の内容と、判定すべき条件のインデックスを渡す。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewRequest {
    /// 対象 run の `done.summary`。
    pub summary: String,
    /// 対象 run の `done.evidence`。
    pub evidence: Vec<Evidence>,
    /// 判定すべき `task.acceptance` のインデックス（`Check::Reviewer` の条件）。
    pub criteria: Vec<usize>,
}

/// 以前の `question` への人間の回答（`context.answers[]`。ADR-0010 D3, P-10）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Answer {
    pub question: String,
    pub answer: String,
}

/// `context.role`（ADR-0016 D1 / M3）: タスクの役割と `[[roles]]` の指示文。アダプタはプロンプトの前置きにする。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoleContext {
    pub id: String,
    /// 設定に指示文が無ければ空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
}

/// `context.available_genres[].roles[]`（ADR-0027 D1）: 分野に属する役割の id。指示文までは渡さない
/// （長くなりすぎるため。プロンプトに前置きされる指示文は `context.role` 側の役目）ので `description` は
/// 今のところ常に省略だが、将来役割に説明文を持たせたときのために型としては残す。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GenreRoleContext {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `context.available_genres[]`（ADR-0027 D1）: 委譲できる run に渡す、使える分野と役割の一覧。
/// 「タスクの分野」ではなく「この run が子に割り当てられる分野の選択肢」を表す。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GenreContext {
    pub id: String,
    pub description: String,
    /// ADR-0028 D1/D2: この分野で「できること」の自由記述。空なら省略される。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// ADR-0028 D1/D2: この分野に渡すもの（目安、自由記述）。空なら省略される。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_artifacts: Vec<String>,
    /// ADR-0028 D1/D2: この分野から返るもの（目安、自由記述）。空なら省略される。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<GenreRoleContext>,
    /// Phase 38（ADR-0028 追記）: この分野の担当が動くハーネスのアダプタ id
    /// （`default_role` の役割の `adapter`。`GenreSpec::harness_adapter`）。`paperqa` /
    /// `local-deep-research` のとき、成果物の名前は**固定**で担当は別のファイルを書けない
    /// （`is_harness`）。決定的に決まる値で、プロンプトに「成果物の規約」を出すかの判断にだけ使う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
}

impl GenreContext {
    /// 設定（`[[genres]]` と `[[roles]]`）から組む（Phase 38: `harness` を埋めるため役割も要る）。
    pub fn from_spec(g: &GenreSpec, roles: &[task_core::RoleSpec]) -> Self {
        Self {
            harness: g.harness_adapter(roles).map(str::to_string),
            ..Self::from(g)
        }
    }

    /// Phase 38（ADR-0028 追記）: 成果物の名前を担当が選べない分野か（`harness` が
    /// `task_core::HARNESS_ADAPTERS` のどれか）。
    pub fn is_harness(&self) -> bool {
        self.harness
            .as_deref()
            .is_some_and(|a| task_core::HARNESS_ADAPTERS.contains(&a))
    }

    /// Phase 38（ADR-0028 追記）: `output_artifacts` の `(名前, 説明)`（`名前: 説明` 形式の解釈）。
    pub fn output_artifacts_named(&self) -> Vec<(&str, Option<&str>)> {
        self.output_artifacts
            .iter()
            .map(|a| {
                (
                    task_core::artifact_entry_name(a),
                    task_core::artifact_entry_description(a),
                )
            })
            .collect()
    }
}

impl From<&GenreSpec> for GenreContext {
    fn from(g: &GenreSpec) -> Self {
        Self {
            id: g.id.clone(),
            description: g.description.clone(),
            capabilities: g.capabilities.clone(),
            input_artifacts: g.input_artifacts.clone(),
            output_artifacts: g.output_artifacts.clone(),
            roles: g
                .roles
                .iter()
                .map(|id| GenreRoleContext {
                    id: id.clone(),
                    description: None,
                })
                .collect(),
            harness: None,
        }
    }
}

/// `context.children[]`（ADR-0016 D3 / M4）: 集約 run に渡す、委譲した子の要約。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChildSummary {
    pub id: TaskId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub status: Status,
    /// 直近のワーカー run の `WorkerFinished.outcome`（無ければ省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// 子のワークスペースに残った成果物（`ArtifactProduced` の一覧。パスは子のワークスペース相対）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,
    /// 子のワークスペース（絶対パス。集約 run が成果物を読むため）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// ADR-0041 D1: 子が worktree で作業したときのブランチ（`celeris/<child_id>`）。
    /// 親はこのブランチを merge して子の成果を統合する（統合は LLM の仕事。celeris はコミットしない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// `context.node`（ADR-0033 D4 / Phase 24）: この run をしている「人」（組織のノード）。
/// プロンプトの一番前に「あなたは誰で、何の担当か」として置かれる。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NodeContext {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub brief: String,
}

/// `context.memory`（ADR-0033 D6 / Phase 24）: 案件をまたぐ記憶と、この案件の引き出し。
/// どちらも `<memory_dir>/<node_id>/…` のファイルの中身（字数で切ったもの）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryContext {
    /// `notes.md`（クラスタの使い方、人の好み、直近の相談）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// `projects/<project_id>.md`（この案件だけの事）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project: String,
}

/// `context.conversation[]`（ADR-0033 D4 / Phase 24）: この案件でのこのノードと人の直近のやり取り。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationTurn {
    pub role: task_core::MessageRole,
    pub text: String,
}

/// `context.conversation_addressee`（ADR-0033 D4 / Phase 28）: この run が対話用タスクなら、相手が
/// 秘書かそれ以外かを表す。対話でない run では `None`。この値そのものは判断せず、`preamble::render`
/// が対話専用の指示文（作業を始めない・委譲不可）を出し分けるためだけに使う純粋なデータ
/// （判定はディスパッチャが `task.conversation` と組織図から決定的に行う。DESIGN 原則 1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationAddressee {
    Secretary,
    Other,
}

/// ADR-0054 D2（Phase 68）: CoS の対話 run に許す**読み取りだけの道具**（`celerisctl` のサブコマンド。
/// `docs/adr/0054-stateful-sessions-and-streaming-chat.md` D2: 「celerisctl knowledge search|get、
/// タスク・案件の一覧と詳細の read API」）。CoS 以外の対話・作業 run には効かない（`ConversationAddressee`
/// が `Secretary` のときだけ、各アダプタがこの一覧を自分のツール許可の書式に写す）。
/// 書く操作（`add` / `plan` / `approve` / `cancel` 等）は含めない。
pub const CONVERSATION_READONLY_CELERISCTL: &[&str] = &[
    "knowledge search",
    "knowledge get",
    "ls",
    "show",
    "projects ls",
    "projects show",
];

/// `context.recent_work[]`（ADR-0033 D4 / Phase 33: 実機の事故 — 担当が自分の直近の仕事を知らずに
/// 「対象タスク ID が必要です」と聞き返した — の再発防止）。対話 run にだけ、その担当の直近の仕事を渡す。
/// 生成は決定的（ストアのタスクとイベントから組む。LLM は使わない。DESIGN 原則 1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RecentWork {
    pub task_id: TaskId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_title: Option<String>,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// 終端の要約: `done` なら `summary` の 1 行目、`failed` なら理由、`blocked` なら質問。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// この仕事が残した成果物の名前（パスは含まない）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

/// `context.milestone_review`（ADR-0038 D1。Phase 41）: **途中目標レビューの対話 run** にだけ載る、
/// その途中目標と、そこまでの仕事の成果。人に向けて「得られた結果 → 達成の可否 → 次の提案」を書かせるための
/// 材料で、集めるのは決定的（ストアのタスク・イベントと成果物ファイルを読むだけ。LLM は使わない）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MilestoneReviewContext {
    pub milestone: MilestoneBrief,
    /// その途中目標に属する仕事（裏方は除く。作られた順）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<MilestoneTaskResult>,
}

/// `context.milestone_review.milestone`（ADR-0038 D1）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MilestoneBrief {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// `proposed` / `approved` / `in_progress` / `reached` / `redesigned`。
    pub status: String,
}

/// `context.milestone_review.tasks[]`（ADR-0038 D1）: 1 件の仕事とその終わり方・成果物の抜粋。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MilestoneTaskResult {
    pub title: String,
    pub status: Status,
    /// 終端の要約（`recent_work` と同じ組み立て）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// 主な成果物（`answer.md` / `report.md`）の先頭 4,000 字（決定的に切る）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub artifacts_excerpt: String,
}

/// `context.organization[]`（ADR-0033 D4 / Phase 24）: 組織図。分解・委譲できる run に渡し、
/// 「どの課に何を振るか」を `assignee` で指定させる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OrgNodeContext {
    pub id: String,
    pub name: String,
    /// `secretary` / `department` / `section`。
    pub kind: task_core::OrgKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub brief: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// ADR-0046 D2 / D6（Phase 59）: そのノードの**実効** skills（根→葉で継いだ結果）。
    /// CoS の対話 run と分解 run に「誰が何をできるか」を出すために渡す。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// ADR-0046 D3 / D6（Phase 59）: そのノードが受けられる**実効**ハーネスの id。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harnesses: Vec<String>,
    /// ADR-0046 D8 / Phase 98: そのノードの**実効** tools（`cluster:<id>` を含む）。CoS の対話 run の
    /// 「組織」一覧に添え、CoS が「どのノードにクラスタ作業を流せるか」を前置きから判断できるようにする
    /// （実機障害 2026-09-22: CoS が cluster-hpc の存在を知らず ssh 禁止を理由に断った）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
}

/// `context.clusters[]`（ADR-0059 D6 / Phase 99）: CoS の対話 run にだけ渡す `[[clusters]]` の一覧
/// （id・接続状態・実効の作業ディレクトリ）。CoS が `create_task.workspace` を組むときに、`path` を
/// どのクラスタのどの作業ディレクトリにすればよいか（あるいは登録されていないので `path` を省略すべきか）
/// を前置きから判断できるようにする（実機障害 2026-09-22: `~` をホームとして使い、ホームでは
/// worktree が切れず失敗した）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClusterContext {
    pub id: String,
    /// この tick で `ssh -O check` が成功したか（ADR-0018 D2）。
    pub connected: bool,
    /// 実効の作業ディレクトリ（DB の上書き `cluster_settings` > 設定ファイルの `work_dir`）。
    /// どちらも無ければ `null`（このクラスタへは `path` を明示しないと動かない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
}

impl From<&task_core::OrgNode> for OrgNodeContext {
    fn from(n: &task_core::OrgNode) -> Self {
        Self {
            id: n.id.clone(),
            name: n.name.clone(),
            kind: n.kind,
            parent_id: n.parent_id.clone(),
            brief: n.brief.clone(),
            genre: n.genre.clone(),
            skills: Vec::new(),
            harnesses: Vec::new(),
            tools: Vec::new(),
        }
    }
}

impl OrgNodeContext {
    /// ADR-0046 D6: 実効 profile（`task_core::profile::resolve`）から skills / harnesses / tools を
    /// 足した版。
    pub fn with_profile(n: &task_core::OrgNode, effective: &task_core::EffectiveProfile) -> Self {
        Self {
            skills: effective.skills.clone(),
            harnesses: effective.harnesses_allowed.clone(),
            tools: effective.tools.clone(),
            ..Self::from(n)
        }
    }
}

/// `run.context`。未知フィールドは無視する（前方互換）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunContext {
    pub prior_review: Vec<PriorReview>,
    pub inputs: Vec<ArtifactRef>,
    /// `celerisctl answer` で与えられた回答の履歴（時系列）。無ければ省略（ADR-0010 D3）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<Answer>,
    /// `Reviewer` check の run でのみ `Some`（ADR-0007 D5）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewRequest>,
    /// タスクに役割があるときだけ `Some`（ADR-0016 D1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<RoleContext>,
    /// 集約 run（`aggregate = true` の親の、子が全て終端になった後の run）でのみ非空（ADR-0016 D3）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ChildSummary>,
    /// ADR-0027 D1: 委譲できる run（`build_execute_prompt` を使う run）にだけ、設定済みの分野と
    /// その役割の一覧を渡す。委譲できない run（`Plan`/`Review`）や `[[genres]]` が空の設定では空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_genres: Vec<GenreContext>,
    /// ADR-0033 D4: `task.assignee` の組織ノード（担当が無いタスクでは `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeContext>,
    /// ADR-0033 D6: `[memory]` を設定し、担当が決まっている run にだけ載る長期記憶。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryContext>,
    /// ADR-0033 D4: 担当のノードとのこの案件での直近のやり取り（古い順）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversation: Vec<ConversationTurn>,
    /// ADR-0033 D5: 「今後ずっと」の認可（永続の認可）。**Phase 26 が埋める。今は常に空**。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub standing_rules: Vec<String>,
    /// ADR-0033 D4: 分解・委譲できる run に渡す組織図（どの課に何を振るかを `assignee` で決めさせる）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub organization: Vec<OrgNodeContext>,
    /// ADR-0059 D6（Phase 99）/ Phase 99b 追記: **CoS の対話 run** にだけ渡す `[[clusters]]` の一覧
    /// （id・接続状態・実効の作業ディレクトリ）。`recent_work` / `knowledge` / `profile` / `role` と
    /// 同じく継続中の run でも毎回渡す（クラスタの登録・接続状態は差分でなく「いまの状態」）。CoS 以外
    /// の run では常に空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clusters: Vec<ClusterContext>,
    /// ADR-0033 D4（Phase 28）: 対話用タスクの run にだけ `Some`。委譲・`Question` を使わせず、
    /// 返事だけを求める前置き（`preamble::conversation_instructions`）を出すための印。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_addressee: Option<ConversationAddressee>,
    /// Phase 30（ADR-0033 D4 追記）: 対話 run で、担当のノードが自分の仕事の分野（`node.genre`）を持つ
    /// ときだけ `Some`。対話そのものは常に対話用分野（`task.genre`）で走るが、その人が自分の得意分野を
    /// 知って答えられるように、前置きに「仕事で使う道具」として渡す（決定的。`[[genres]]` を引くだけ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_genre: Option<GenreContext>,
    /// Phase 33（ADR-0033 D4 追記。実機の事故の再発防止）: 対話 run にだけ、その担当の直近の仕事
    /// （最大 10 件、更新の新しい順。案件を選んでいる対話ならその案件のものを先に）を渡す。
    /// 通常の run（対話でない）では常に空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_work: Vec<RecentWork>,
    /// Phase 41（ADR-0038 D1）: **途中目標レビューの対話 run** にだけ `Some`。その途中目標と、そこまでの
    /// 仕事の成果（`preamble` が「これまでの結果」の節を出し、ADR D1 の指示文を足す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone_review: Option<MilestoneReviewContext>,
    /// Phase 38（ADR-0028 追記）: **レビュー run** にだけ、レビュー対象のタスクの分野の manifest を渡す。
    /// ハーネス系の分野（`GenreContext::is_harness`）なら、レビュアーのプロンプトに「成果物の名前は固定で、
    /// `papers.json` は検索コーパスであって答えではない」という規約を出す（実機で、計画が勝手に決めた
    /// ファイル名を基準にレビュアーが不合格にした事故から）。決定的（設定を引くだけ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_genre: Option<GenreContext>,
    /// Phase 43（ADR-0039 D3）: **案件が作業場所を決めている run** にだけ載る 1 行（そのコードがどこに
    /// あるか）。ディスパッチャが `projects.workspace` から決定的に組む（`preamble::workspace_note`）。
    /// 作業場所を決めていない案件・案件に属さないタスクでは `None` で、プロンプトは Phase 42 までと
    /// バイト単位で同じ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_note: Option<String>,
    // ---- ADR-0044 D2（Phase 53）: タスク単位のコメント。ここから ----
    /// ADR-0044 D2: そのタスクのコメントの**最新 20 件**（古い順）。空なら省略され、
    /// 「コメント」の節も出ない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<CommentContext>,
    /// ADR-0044 D2: **直前の run を止めた人のコメント**（次の run の前置きの先頭に
    /// 「人からの割り込み」として載る）。割り込みの直後の run にだけ入る。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt: Option<String>,
    /// ADR-0044 D2: この run はコメントを書ける（`{"type":"comment","body":"…"}`）か。
    /// `true` のときだけ前置きに「短い進捗や判断の記録はコメントに書け」の節を出す。
    /// **仕事の run はすべて true** なので、Phase 53 以降、普通の run の前置きにはこの節が必ず入る
    /// （Phase 52 とバイト単位で同じなのは、この 3 つが既定のまま = レビュー run と対話 run だけ）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub comments_enabled: bool,
    // ---- ADR-0044 D2（Phase 53）: ここまで ----
    // ---- ADR-0046（Phase 59）: 実効 profile と進め方。ここから ----
    /// ADR-0046 D1: この run の**実効 profile**（担当ノードの根→葉の merge ＋ タスクの上書き）。
    /// 担当が居ない run・profile を持たない組織では `None` で、前置きは Phase 58 までとバイト単位で同じ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<task_core::EffectiveProfile>,
    /// ADR-0046 D4: このタスクの進め方。既定（`production`）は `None` にして渡さない
    /// （前置きを Phase 58 までと変えないため）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<task_core::TaskMode>,
    // ---- ADR-0046（Phase 59）: ここまで ----
    // ---- ADR-0047 D2（Phase 61）: 知識ベース。ここから ----
    /// ADR-0047 D2: マウントされた知識の**索引だけ**（本文は入れない）。ディスパッチャが実効マウントと
    /// `index.json` から決定的に組む。`None` の run の前置きは Phase 60 までと 1 バイトも変わらない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<KnowledgeContext>,
    // ---- ADR-0047 D2（Phase 61）: ここまで ----
    // ---- ADR-0048 D3（Phase 60b）: CoS の対話に渡す進行中の案件。ここから ----
    /// ADR-0048 D3: **CoS の対話 run** にだけ渡す、進行中の案件（`proposed` / `active`）とその
    /// 途中目標の一覧。`actions` の `create_task.project` / `add_milestone.project` を選ぶ材料
    /// （決定的にストアを読むだけ。CoS 以外の run では常に空）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_projects: Vec<ActiveProjectContext>,
    // ---- ADR-0048 D3（Phase 60b）: ここまで ----
    // ---- ADR-0054 D1（Phase 67）: ノードごとの継続セッション。ここから ----
    /// ADR-0054 D1: この run が**継続セッション**（CoS の対話、または部門長のレビュー・切り分け run）
    /// のときだけ `Some`。アダプタはこれを見て `--session-id`/`--resume`（claude-code）、
    /// `codex exec resume`/`experimental_resume`（codex）、`session/load`（acp）を選ぶ。継続しない run
    /// （通常の仕事の run）では常に `None` で、前置き・アダプタの起動は Phase 66 までと 1 バイトも変わらない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionHandle>,
    /// ADR-0054 D1: `session` が**継続中**（`resume = true`）の run にだけ渡す、前回の run 以降に
    /// 起きたことの短い箇条書き（新しい人の発言・dispatch したタスクの終端と要約・認可の結果・新しい案件）。
    /// `session` が新規（`resume = false`。rollover・アカウント変更・resume 失敗の後を含む）のときは、
    /// 代わりに「前のセッションの要約」（ADR-0033 D4 の対話履歴の末尾 20 件）をここに入れることがある。
    /// どちらでもない通常の run では常に空で省略される（Phase 66 までと 1 バイトも変わらない）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_diff: Vec<String>,
    // ---- ADR-0054 D1（Phase 67）: ここまで ----
    // ---- ADR-0056 D3（Phase 79）: mount された skills。ここから ----
    /// ADR-0056 D3: 担当ノードの実効 profile が継いだ `skills_mounts`（Phase 78）のうち、KB の
    /// `skills/<name>/` に実在するものだけ（本文は含まない。`path` の `SKILL.md` をアダプタが読む）。
    /// 見つからない名前は run を落とさず、ディスパッチャが `status` の進行イベントを 1 行出して省く
    /// （このフィールドには乗らない）。届け方はアダプタごと（`claude-code` は `.claude/skills/<name>/`
    /// へコピー、`codex` は `AGENTS.md` の節、`acp` は前置きに埋め込む）。研究系アダプタ
    /// （paperqa / local-deep-research / langmem）は無視する。空なら省略され、前置き・作業場所は
    /// Phase 78 までと 1 バイトも変わらない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillMount>,
    // ---- ADR-0056 D3（Phase 79）: ここまで ----
}

/// `context.skills[]`（ADR-0056 D3。Phase 79）: mount された skill 1 件。ディスパッチャが KB から
/// 決定的に解決する（存在確認と frontmatter の `description` を読むだけ。本文はアダプタが `path` から
/// 読む）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkillMount {
    /// KB の `skills/<name>/` と同じ綴り。
    pub name: String,
    /// 絶対パス。KB の `skills/<name>/`（`SKILL.md` と付属ファイルを含むディレクトリ）。
    pub path: String,
    /// `SKILL.md` の frontmatter の `description`（決まらなければ空文字列）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// `context.session`（ADR-0054 D1。Phase 67）: 継続セッションの手がかり。
/// ディスパッチャが `node_sessions`（`task_core::NodeSession`）から決定的に組む。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionHandle {
    /// このセッションを継続する手段（`claude-code` / `codex` / `acp`）。アダプタは自分の `id()` と
    /// 一致しないときは無視する（別アダプタへ渡り歩くセッションは無い）。
    pub adapter: String,
    /// アダプタに渡す実際の値。`resume = false`（新規）のときはアダプタが**これから使う**セッション id
    /// （claude-code の `--session-id`。固定した ULID）、`resume = true`のときは**続ける**セッション id
    /// （claude-code の `--resume`、codex の resume id、acp の `session/load` の id）。
    pub session_id: String,
    /// `false` = 新規（このセッションの最初の run）。`true` = 続ける（2 回目以降）。
    pub resume: bool,
}

/// `context.active_projects[]`（ADR-0048 D3。Phase 60b）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActiveProjectContext {
    /// Repository names valid for create_task.repos within this project.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    pub id: String,
    pub title: String,
    /// `proposed` / `active`。
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub milestones: Vec<ActiveMilestoneContext>,
}

/// `context.active_projects[].milestones[]`（ADR-0048 D3。Phase 60b）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActiveMilestoneContext {
    pub id: String,
    pub title: String,
    /// `proposed` / `approved` / `in_progress` / `reached` / `redesigned`。
    pub status: String,
}

/// ADR-0047 D2（Phase 61）: 前置きに出す知識の索引（`preamble::knowledge_section` が読む）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgeContext {
    /// 実効マウント（組織の和 ＋ 案件の `projects/<slug>` ＋ タスクの明示）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<task_core::KnowledgeMount>,
    /// そのマウントで読めるページの索引（`path` / `title` / `tags` / `scope`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub index: Vec<task_core::KnowledgeItem>,
}

/// `context.comments[]`（ADR-0044 D2 / Phase 53）: タスクに付いたコメントの 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommentContext {
    /// `human` / `node` / `system`。
    pub author_kind: task_core::CommentAuthorKind,
    /// `node` のときの組織ノード id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub body: String,
    /// RFC 3339。
    pub at: String,
}

impl From<&task_core::TaskComment> for CommentContext {
    fn from(c: &task_core::TaskComment) -> Self {
        Self {
            author_kind: c.author_kind,
            author: c.author.clone(),
            body: c.body.clone(),
            at: c
                .created_at
                .format(&time::format_description::well_known::Rfc3339)
                // 書式化はまず失敗しないが、失敗しても空文字（`- [] 人: …`）にはしない。
                .unwrap_or_else(|_| c.created_at.to_string()),
        }
    }
}

/// `error.provider_failure`（任意）: 供給側の失敗の種別（ADR-0010 D5, P-21）。付いていればディスパッチャは
/// attempts を消費せず `requeue` し、プロバイダを cooldown にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderFailure {
    Throttled { retry_after_secs: u64 },
    AuthFailed,
    Exhausted,
}

/// `artifacts/review.json` の 1 判定（ADR-0007 D1/D5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewVerdictOut {
    pub criterion: usize,
    pub pass: bool,
    pub reason: String,
}

/// `Review` run がワークスペース直下 `artifacts/review.json` に書く出力（ADR-0007 D1/D5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewOutput {
    pub verdicts: Vec<ReviewVerdictOut>,
}

/// celeris → ワーカーの `run` メッセージ（1 行）。`{"type":"run", ...}`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename = "run")]
pub struct RunRequest {
    pub protocol: u32,
    pub task: Task,
    /// 絶対パス。`artifact.path` の基準で、`runs/` `inputs/` `artifacts/` の親。
    /// 既定ではワーカーの cwd でもある（`work_dir` が無いとき）。
    pub workspace: PathBuf,
    /// ADR-0041 D1: 絶対パス。ワーカーの cwd（ローカルの作業場所が git リポジトリで
    /// `mode = worktree` のとき、その run 用の worktree `<workspace>/tree`）。
    /// `None` なら `workspace` がそのまま cwd（Phase 48 までと同じ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<PathBuf>,
    /// 絶対パス。この run の成果物と結果ファイル（`result.json`）の置き場（ADR-0036 D1）。
    /// workspace を自分で所有するタスクは `<workspace>/artifacts`、親から継いだタスク（plan / delegate の子）は
    /// `<workspace>/.taskd/artifacts/<task_id>`。決めるのはディスパッチャで、アダプタはここに書くだけ。
    pub artifacts_dir: PathBuf,
    pub context: RunContext,
}

impl RunRequest {
    /// ワーカーを動かすディレクトリ（ADR-0041 D1: worktree があればそこ、無ければ `workspace`）。
    pub fn cwd(&self) -> &std::path::Path {
        self.work_dir.as_deref().unwrap_or(&self.workspace)
    }

    /// `artifacts_dir` の**ワーカーから見た**表記。
    ///
    /// - 従来（cwd == workspace）: workspace 相対（`artifacts` / `.taskd/artifacts/<task_id>`）。
    ///   単独タスクのプロンプトは Phase 48 までと 1 バイトも変わらない（ADR-0036 D3）。
    /// - worktree（cwd != workspace。ADR-0041 D1）: 成果物は作業ツリーの**外**にあるので絶対パス。
    ///   `..` を書かせない（`artifact.path` の規則 ADR-0003 D5 と衝突させない）。
    pub fn artifacts_rel(&self) -> String {
        if self.work_dir.is_some() {
            return self.artifacts_dir.to_string_lossy().into_owned();
        }
        task_core::artifacts::rel_from(&self.workspace, &self.artifacts_dir)
    }

    /// `artifacts_dir` 配下のファイルの絶対パス。
    pub fn artifact_path(&self, name: &str) -> PathBuf {
        self.artifacts_dir.join(name)
    }

    /// `artifacts_dir` 配下のファイルの workspace 相対パス（`artifacts/result.json` 等）。
    pub fn artifact_rel_path(&self, name: &str) -> String {
        format!("{}/{name}", self.artifacts_rel())
    }
}

/// `done.evidence[]`。`command` / `exit` / `stdout_tail` は、コマンドを伴わない条件（`ArtifactExists` / `Reviewer` / `Human`）では
/// 存在しないので任意（ADR-0012 D3, P-12。必須 → 任意の緩和なので後方互換）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub criterion: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
}

/// ワーカー → celeris のメッセージ。`done` / `error` / `question` は終端（ADR-0003 D3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerMessage {
    /// ADR-0048 D2（Phase 60a）: `msg` は従来どおり人が読む 1 行。`kind` / `tool` / `summary` /
    /// `detail` / `truncated` / `error` は**任意の追加**で、出さないワーカーはそのまま動く
    /// （`PROTOCOL_VERSION` は 4 のまま）。`detail` は 4 KiB で切る（`truncated`）。
    Progress {
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
    /// ADR-0044 D2（Phase 53）: タスクのコメント（任意回、非終端）。`progress` と違って**残る**
    /// （`task_comments` に `author_kind = node` で入り、次の run の前置きにも載る）。
    /// **追加のみ**なので `PROTOCOL_VERSION` は 4 のまま（この行を出さないワーカーはそのまま動く）。
    Comment {
        body: String,
    },
    /// ADR-0016 D2: 実行中の委譲の提案（任意回、非終端）。celeris は検証を通ったものだけ子タスクとして挿入し、
    /// 拒否した提案は理由を `WorkerProgress` に残す。run は失敗しない。
    Delegate {
        tasks: Vec<DelegateTask>,
    },
    Artifact {
        name: String,
        /// ワークスペース相対。絶対パス・`..`・ワークスペース外は拒否（ADR-0003 D5）。
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    Question {
        text: String,
    },
    Done {
        summary: String,
        evidence: Vec<Evidence>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    Error {
        message: String,
        retryable: bool,
        /// 供給側の失敗なら種別を付ける（ADR-0010 D5）。付いていれば `retryable` に関わらず `requeue` になる。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_failure: Option<ProviderFailure>,
    },
    /// ADR-0072 D9/D10（Phase E1）: graceful yield（`result.json` の `{"yield": {...}}`）。`checkpoint`
    /// は checkpoint の意味の欄を寛容に持つ生の JSON（`task_core::WorkerCheckpointInput` と同じ形）。
    /// この行を出さないワーカーはそのまま動く（追加のみ。`PROTOCOL_VERSION` は変えない）。
    Yielded {
        checkpoint: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    /// ADR-0072 D7（Phase E1）: turn / wall-clock / context の上限に当たった（予算切れでも usage を運ぶ）。
    /// 追加のみ（`PROTOCOL_VERSION` は変えない）。
    BudgetExhausted {
        kind: BudgetKind,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
}

impl WorkerMessage {
    /// 終端メッセージか（ADR-0003 D3）。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WorkerMessage::Question { .. }
                | WorkerMessage::Done { .. }
                | WorkerMessage::Error { .. }
                | WorkerMessage::Yielded { .. }
                | WorkerMessage::BudgetExhausted { .. }
        )
    }

    /// ADR-0048 D2（Phase 60a）: `progress` 行を組む（`fields` が既定なら従来の文字列だけの行になる）。
    pub fn progress(msg: impl Into<String>, fields: ProgressFields) -> Self {
        WorkerMessage::Progress {
            msg: msg.into(),
            kind: fields.kind,
            tool: fields.tool,
            summary: fields.summary,
            detail: fields.detail,
            truncated: fields.truncated,
            error: fields.error,
        }
    }

    /// ADR-0048 D2: `progress` 行の構造化フィールド（他のメッセージは `None`）。
    pub fn progress_fields(&self) -> Option<ProgressFields> {
        match self {
            WorkerMessage::Progress {
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

/// スキーマ生成用のルート。`docs/protocol/worker-protocol.schema.json` の内容と一致する。
#[derive(Debug, JsonSchema)]
#[allow(dead_code)]
pub struct ProtocolSchema {
    pub run: RunRequest,
    pub message: WorkerMessage,
    /// `artifacts/review.json`（ADR-0007 D1）。
    pub review_output: ReviewOutput,
}

/// 生成したスキーマ（`serde_json::Value`）。
pub fn schema_value() -> serde_json::Value {
    let schema = schemars::schema_for!(ProtocolSchema);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn worker_message_roundtrip_and_unknown_fields_ignored() {
        let line = r#"{"type":"done","summary":"s","evidence":[{"criterion":0,"command":"true","exit":0,"stdout_tail":""}],"extra":1}"#;
        let m: WorkerMessage = serde_json::from_str(line).unwrap();
        assert!(m.is_terminal());
        match &m {
            WorkerMessage::Done {
                summary,
                evidence,
                usage,
            } => {
                assert_eq!(summary, "s");
                assert_eq!(evidence.len(), 1);
                assert!(usage.is_none());
            }
            _ => panic!("expected done"),
        }
        let back = serde_json::to_string(&m).unwrap();
        assert!(back.starts_with(r#"{"type":"done""#));
        assert!(!back.contains("usage"));
    }

    #[test]
    fn unknown_type_is_a_parse_error_and_missing_required_is_error() {
        assert!(serde_json::from_str::<WorkerMessage>(r#"{"type":"bogus"}"#).is_err());
        assert!(
            serde_json::from_str::<WorkerMessage>(r#"{"type":"error","message":"m"}"#).is_err()
        );
        assert!(
            !serde_json::from_str::<WorkerMessage>(r#"{"type":"progress","msg":"m"}"#)
                .unwrap()
                .is_terminal()
        );
    }

    /// ADR-0048 D2（Phase 60a）: `progress` の構造化フィールドは**任意**。従来の `{"type":"progress","msg":…}`
    /// はそのまま読め（全て `None` / `false`）、書き戻しても余分な鍵は出ない。
    #[test]
    fn progress_accepts_both_the_plain_and_the_structured_form() {
        let plain: WorkerMessage =
            serde_json::from_str(r#"{"type":"progress","msg":"working"}"#).expect("plain");
        let fields = plain.progress_fields().expect("progress");
        assert!(fields.is_plain());
        assert_eq!(
            serde_json::to_string(&plain).expect("ser"),
            r#"{"type":"progress","msg":"working"}"#
        );

        let line = r#"{"type":"progress","msg":"tool_use: Bash …","kind":"tool_use","tool":"Bash",
            "summary":"cargo test --workspace","detail":"{\"command\":\"cargo test --workspace\"}","truncated":true,"error":false}"#;
        let structured: WorkerMessage = serde_json::from_str(line).expect("structured");
        let fields = structured.progress_fields().expect("progress");
        assert!(!fields.is_plain());
        assert_eq!(fields.kind, Some(ProgressKind::ToolUse));
        assert_eq!(fields.tool.as_deref(), Some("Bash"));
        assert_eq!(fields.summary.as_deref(), Some("cargo test --workspace"));
        assert!(fields.truncated);
        assert!(!fields.error);
        assert!(!structured.is_terminal());

        // 知らない `kind` の行は**行ごと**読めない（unknown type と同じ扱い）。混在は `msg` で救う。
        assert!(
            serde_json::from_str::<WorkerMessage>(
                r#"{"type":"progress","msg":"m","kind":"bogus"}"#
            )
            .is_err()
        );

        // 組み立て側（`WorkerMessage::progress`）と対称。
        let built = WorkerMessage::progress(
            "tool_result: ok",
            ProgressFields::of(ProgressKind::ToolResult)
                .with_summary("ok")
                .with_error(true),
        );
        let json = serde_json::to_string(&built).expect("ser");
        assert!(json.contains(r#""kind":"tool_result""#), "{json}");
        assert!(json.contains(r#""error":true"#), "{json}");
        assert!(!json.contains("truncated"), "{json}");
        assert_eq!(
            serde_json::from_str::<WorkerMessage>(&json).expect("round trip"),
            built
        );
    }

    /// ADR-0012 D3（P-12）: コマンドを伴わない条件の evidence は `criterion` だけでよく、旧形式（全フィールドあり）も読める。
    #[test]
    fn evidence_fields_other_than_criterion_are_optional() {
        let line = r#"{"type":"done","summary":"s","evidence":[{"criterion":1},{"criterion":0,"command":"cargo test","exit":0,"stdout_tail":"ok"}]}"#;
        let WorkerMessage::Done { evidence, .. } =
            serde_json::from_str::<WorkerMessage>(line).unwrap()
        else {
            panic!("expected done");
        };
        assert_eq!(
            evidence[0],
            Evidence {
                criterion: 1,
                command: None,
                exit: None,
                stdout_tail: None
            }
        );
        assert_eq!(evidence[1].command.as_deref(), Some("cargo test"));
        assert_eq!(evidence[1].exit, Some(0));
        assert_eq!(
            serde_json::to_string(&evidence[0]).unwrap(),
            r#"{"criterion":1}"#
        );
    }

    /// ADR-0016 D2: `delegate` は非終端で、`tasks` は `DelegateTask`。`depends_on` は整数と ID 文字列を混ぜられる。
    #[test]
    fn delegate_message_parses_and_is_not_terminal() {
        let line = r#"{"type":"delegate","tasks":[{"title":"a","objective":"o","acceptance":[{"text":"c","check":{"type":"human"}}],"role":"implementer","genre":"coding"},
            {"title":"b","objective":"o","acceptance":[{"text":"c","check":{"type":"command","cmd":"true","expect_exit":0}}],"depends_on":[0]}]}"#;
        let m: WorkerMessage = serde_json::from_str(line).unwrap();
        assert!(!m.is_terminal());
        let WorkerMessage::Delegate { tasks } = m else {
            panic!("expected delegate")
        };
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].role.as_deref(), Some("implementer"));
        // ADR-0027 D1: `genre` は任意。
        assert_eq!(tasks[0].genre.as_deref(), Some("coding"));
        assert_eq!(tasks[1].genre, None);
        assert_eq!(tasks[1].depends_on, vec![task_core::DelegateDep::Index(0)]);
    }

    #[test]
    fn run_request_serializes_with_type_tag() {
        let req = RunRequest {
            protocol: PROTOCOL_VERSION,
            task: sample_task(),
            workspace: PathBuf::from("/tmp/ws"),
            work_dir: None,
            artifacts_dir: PathBuf::from("/tmp/ws/artifacts"),
            context: RunContext::default(),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["type"], "run");
        assert_eq!(v["protocol"], 4);
        assert_eq!(v["task"]["kind"], "execute");
        let back: RunRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back, req);
    }

    /// ADR-0027 D1: `available_genres` は空なら省略され、非空なら分野と役割の一覧が乗る。
    #[test]
    fn available_genres_round_trips_and_is_omitted_when_empty() {
        let empty = RunContext::default();
        let v = serde_json::to_value(&empty).unwrap();
        assert!(v.get("available_genres").is_none());

        let genres = [task_core::GenreSpec {
            id: "literature".into(),
            description: "related work survey".into(),
            capabilities: vec![
                "academic literature search".into(),
                "citation graph traversal".into(),
            ],
            input_artifacts: vec!["question".into(), "pdf".into()],
            output_artifacts: vec!["answer.md".into(), "citations.json".into()],
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-scout".into(), "literature-reader".into()],
        }];
        let context = RunContext {
            available_genres: genres.iter().map(GenreContext::from).collect(),
            ..RunContext::default()
        };
        let v = serde_json::to_value(&context).unwrap();
        assert_eq!(v["available_genres"][0]["id"], "literature");
        assert_eq!(
            v["available_genres"][0]["roles"][1]["id"],
            "literature-reader"
        );
        assert_eq!(
            v["available_genres"][0]["capabilities"][1],
            "citation graph traversal"
        );
        assert_eq!(
            v["available_genres"][0]["input_artifacts"],
            serde_json::json!(["question", "pdf"])
        );
        assert_eq!(
            v["available_genres"][0]["output_artifacts"],
            serde_json::json!(["answer.md", "citations.json"])
        );
        let back: RunContext = serde_json::from_value(v).unwrap();
        assert_eq!(back, context);

        // 空なら 3 フィールドとも省略される（既存設定との互換）。
        let bare_genre = task_core::GenreSpec {
            id: "coding".into(),
            description: "write and fix code".into(),
            ..task_core::GenreSpec::default()
        };
        let bare_json = serde_json::to_value(GenreContext::from(&bare_genre)).unwrap();
        assert!(bare_json.get("capabilities").is_none());
        assert!(bare_json.get("input_artifacts").is_none());
        assert!(bare_json.get("output_artifacts").is_none());
    }

    /// Phase 38（ADR-0028 追記）: `harness`（`default_role` の役割のアダプタ）と `subject_genre` は
    /// 追加のみのフィールドで、無ければ JSON に出ない（旧ワーカー互換）。`名前: 説明` は名前と説明に分かれる。
    #[test]
    fn harness_and_subject_genre_are_optional_additions() {
        let roles = vec![task_core::RoleSpec {
            id: "literature-reader".into(),
            adapter: Some("paperqa".into()),
            ..task_core::RoleSpec::default()
        }];
        let spec = task_core::GenreSpec {
            id: "literature".into(),
            description: "related work".into(),
            output_artifacts: vec!["answer.md: 引用付きの答え".into(), "papers.json".into()],
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-reader".into()],
            ..task_core::GenreSpec::default()
        };
        // 役割を渡さずに組むと `harness` は付かない（従来の `From<&GenreSpec>`）。
        let bare = serde_json::to_value(GenreContext::from(&spec)).unwrap();
        assert!(bare.get("harness").is_none(), "{bare}");

        let genre = GenreContext::from_spec(&spec, &roles);
        assert_eq!(genre.harness.as_deref(), Some("paperqa"));
        assert!(genre.is_harness());
        assert_eq!(
            genre.output_artifacts_named(),
            vec![("answer.md", Some("引用付きの答え")), ("papers.json", None)]
        );

        let empty = serde_json::to_value(RunContext::default()).unwrap();
        assert!(empty.get("subject_genre").is_none(), "{empty}");
        let context = RunContext {
            subject_genre: Some(genre),
            ..RunContext::default()
        };
        let json = serde_json::to_value(&context).unwrap();
        assert_eq!(json["subject_genre"]["harness"], "paperqa");
        assert_eq!(
            json["subject_genre"]["output_artifacts"][0],
            "answer.md: 引用付きの答え"
        );
        let back: RunContext = serde_json::from_value(json).unwrap();
        assert_eq!(back, context);
    }

    /// ADR-0003 D6: 生成スキーマとコミット済みファイルの一致。`UPDATE_SCHEMA=1` で再生成する。
    #[test]
    fn committed_schema_matches_generated() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/protocol/worker-protocol.schema.json"
        );
        let generated = serde_json::to_string_pretty(&schema_value()).unwrap() + "\n";
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e} (run with UPDATE_SCHEMA=1 to generate)"));
        assert_eq!(
            committed, generated,
            "schema drift: run `UPDATE_SCHEMA=1 cargo test -p task-worker`"
        );
    }

    pub(crate) fn sample_task() -> Task {
        use task_core::*;
        let now = time::OffsetDateTime::now_utc();
        Task {
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion {
                text: "c".into(),
                check: Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Running,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: PathBuf::from("/tmp/ws"),
                mode: None,
            },
            budget: Budget {
                max_turns: 10,
                max_wall_secs: 60,
                max_retries: 1,
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
}
