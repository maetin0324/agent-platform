//! 組織（ADR-0033 D1）と案件・途中目標（D2）のモデル。純粋なデータ定義と決定的な検証だけを置く
//! （I/O・LLM 呼び出しはしない。ADR-0001 D2 / DESIGN 原則 1）。
//!
//! SPEC §3.2 の「組織は一つ、役割の木、各ノードは人」を DB の第一級エンティティにしたもの。
//! 設定から種を蒔き、以後は GUI → API → DB が正（ADR-0024 の accounts と同じ扱い）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

use crate::model::{GenreSpec, RoleSpec};

/// 案件の一意識別子（ULID）。`TaskId` と同じ形。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ProjectId(#[schemars(with = "String")] pub Ulid);

impl ProjectId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for ProjectId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ProjectId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// 途中目標の一意識別子（ULID）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct MilestoneId(#[schemars(with = "String")] pub Ulid);

impl MilestoneId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for MilestoneId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for MilestoneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for MilestoneId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// 組織のノードの種類（ADR-0033 D1）。`secretary` は根で 1 つだけ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrgKind {
    Secretary,
    Department,
    Section,
}

impl OrgKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OrgKind::Secretary => "secretary",
            OrgKind::Department => "department",
            OrgKind::Section => "section",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "secretary" => Some(OrgKind::Secretary),
            "department" => Some(OrgKind::Department),
            "section" => Some(OrgKind::Section),
            _ => None,
        }
    }

    /// 木の深さの序列（secretary > department > section）。親は子より小さい値であること。
    fn depth(self) -> u8 {
        match self {
            OrgKind::Secretary => 0,
            OrgKind::Department => 1,
            OrgKind::Section => 2,
        }
    }
}

/// 組織の 1 ノード（＝ SPEC §3.2 の「人」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OrgNode {
    /// 英小文字ケバブの id（`secretary` / `coding-frontend` 等）。設定の種とも API とも同じ文字列。
    pub id: String,
    /// 親ノードの id。`secretary`（根）だけが `None`。
    pub parent_id: Option<String>,
    pub name: String,
    pub kind: OrgKind,
    /// ADR-0027/0028 の `[[genres]] id`。その「人」が仕事に使うハーネスの束。部は持たなくてよい。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    /// 担当の一言（プロンプトに前置きされる）。
    #[serde(default)]
    pub brief: String,
    /// 同じ親の中での並び順（GUI の組織図の表示順）。
    #[serde(default)]
    pub position: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub updated_at: OffsetDateTime,
}

/// 組織の検証の失敗（ADR-0033 D1）。ストアが `StoreError::Invalid` に包み、API は 422 にする。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrgError {
    #[error("org node id must be lowercase kebab-case ([a-z0-9-], 1..64 chars): {0:?}")]
    InvalidId(String),
    #[error("org node name must not be blank")]
    BlankName,
    #[error("there can be only one secretary (already: {existing:?})")]
    DuplicateSecretary { existing: String },
    #[error("the secretary is the root and must not have a parent")]
    SecretaryHasParent,
    #[error("org node {id:?} must have a parent (only the secretary is a root)")]
    MissingParent { id: String },
    #[error("parent {parent:?} does not exist")]
    UnknownParent { parent: String },
    #[error("org node {id:?} cannot be its own ancestor")]
    Cycle { id: String },
    #[error("a {child} cannot be placed under a {parent}")]
    BadNesting { child: &'static str, parent: &'static str },
}

/// 英小文字ケバブの id（設定の種と API で同じ規則）。
pub fn valid_org_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

/// `existing`（DB にある全ノード。`node.id` の行があればそれも含む）に対して `node` を upsert してよいかを
/// 決定的に検証する（ADR-0033 D1）。LLM も I/O も使わない。
pub fn validate_upsert(existing: &[OrgNode], node: &OrgNode) -> Result<(), OrgError> {
    if !valid_org_id(&node.id) {
        return Err(OrgError::InvalidId(node.id.clone()));
    }
    if node.name.trim().is_empty() {
        return Err(OrgError::BlankName);
    }
    if node.kind == OrgKind::Secretary {
        if node.parent_id.is_some() {
            return Err(OrgError::SecretaryHasParent);
        }
        if let Some(other) = existing
            .iter()
            .find(|n| n.kind == OrgKind::Secretary && n.id != node.id)
        {
            return Err(OrgError::DuplicateSecretary { existing: other.id.clone() });
        }
        return Ok(());
    }

    // 根は secretary だけ（ADR-0033 D1: 組織は一つの木）。
    let Some(parent_id) = node.parent_id.as_deref() else {
        return Err(OrgError::MissingParent { id: node.id.clone() });
    };
    if parent_id == node.id {
        return Err(OrgError::Cycle { id: node.id.clone() });
    }
    let Some(parent) = existing.iter().find(|n| n.id == parent_id) else {
        return Err(OrgError::UnknownParent { parent: parent_id.to_string() });
    };
    // 自分を祖先にできない（親を辿って自分に戻ってこないこと）。種類の検査より先に見る。
    // 「自分の子孫にぶら下げた」は種類の順序違反としても現れるが、理由としては循環の方が正確。
    let mut seen = 0usize;
    let mut cursor = Some(parent);
    while let Some(current) = cursor {
        if current.id == node.id {
            return Err(OrgError::Cycle { id: node.id.clone() });
        }
        seen += 1;
        if seen > existing.len() {
            // 既存データが壊れている（親の連鎖が閉じている）場合も、ここで打ち切って拒否する。
            return Err(OrgError::Cycle { id: node.id.clone() });
        }
        cursor = current
            .parent_id
            .as_deref()
            .and_then(|p| existing.iter().find(|n| n.id == p));
    }
    if parent.kind.depth() >= node.kind.depth() {
        return Err(OrgError::BadNesting {
            child: node.kind.as_str(),
            parent: parent.kind.as_str(),
        });
    }
    // 監査 D-1: `kind` を変える更新（`PATCH`）は、自分自身と親だけでなく、既にぶら下がっている子とも
    // 整合しなければならない。そうしないと「department → section」のような変更で、既存の子（section）
    // が「section の下に section」という壊れた木を作ってしまう（そのまま気づかれず、以後その子は
    // 名前の変更すら拒否され続ける）。
    for child in existing.iter().filter(|n| n.parent_id.as_deref() == Some(node.id.as_str())) {
        if node.kind.depth() >= child.kind.depth() {
            return Err(OrgError::BadNesting {
                child: child.kind.as_str(),
                parent: node.kind.as_str(),
            });
        }
    }
    Ok(())
}

/// ADR-0033 D2 / Phase 23: `assignee` から仕事の道具立てを決定的に引く。
/// 組織のノードの `genre` → その分野の `default_role` → 役割（tier / adapter / 予算）。
/// 知らない `assignee` や分野・役割は `None` を返すだけで、ここではエラーにしない（呼び出し側が決める）。
pub fn assignee_defaults<'a>(
    org: &[OrgNode],
    assignee: &str,
    roles: &'a [RoleSpec],
    genres: &[GenreSpec],
) -> (Option<String>, Option<&'a RoleSpec>) {
    let Some(node) = org.iter().find(|n| n.id == assignee) else {
        return (None, None);
    };
    let Some(genre) = node.genre.clone() else {
        return (None, None);
    };
    let role = GenreSpec::find(genres, &genre)
        .and_then(|g| g.default_role.as_deref())
        .and_then(|r| RoleSpec::find(roles, r));
    (Some(genre), role)
}

/// ADR-0033 D4（Phase 24）: そのノードが属する「部」の id。`department` 自身はその id、`section` は
/// 親を辿って最初に見つかる `department`、`secretary` は `None`（部に属さない）。知らない id も `None`。
/// 「部をまたぐ連携は秘書が認める」（SPEC §3.1）の判定に使う決定的な関数。
pub fn department_of(org: &[OrgNode], id: &str) -> Option<String> {
    let mut cursor = org.iter().find(|n| n.id == id);
    for _ in 0..=org.len() {
        let node = cursor?;
        match node.kind {
            OrgKind::Department => return Some(node.id.clone()),
            OrgKind::Secretary => return None,
            OrgKind::Section => {
                cursor = node.parent_id.as_deref().and_then(|p| org.iter().find(|n| n.id == p));
            }
        }
    }
    None
}

/// 案件の状態（ADR-0033 D2、ADR-0044 D6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    /// 秘書が理解確認と方針を出し、人の返事待ち。
    Proposed,
    Active,
    /// ADR-0044 D6: 一時停止。属するタスクは dispatch されない（`ready` のまま）。
    /// 元の状態は `Project::paused_from` に持ち、`resume` でそこへ戻る。
    Paused,
    Done,
    /// ADR-0044 D6: 中止。属する非終端タスクは全部 `cancelled` にした後の終端。
    Cancelled,
}

impl ProjectStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectStatus::Proposed => "proposed",
            ProjectStatus::Active => "active",
            ProjectStatus::Paused => "paused",
            ProjectStatus::Done => "done",
            ProjectStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "proposed" => Some(ProjectStatus::Proposed),
            "active" => Some(ProjectStatus::Active),
            "paused" => Some(ProjectStatus::Paused),
            "done" => Some(ProjectStatus::Done),
            "cancelled" => Some(ProjectStatus::Cancelled),
            _ => None,
        }
    }

    /// ADR-0044 D6: 終端（これ以上動かない）。**アーカイブできるのは終端の案件だけ**。
    pub fn is_terminal(self) -> bool {
        matches!(self, ProjectStatus::Done | ProjectStatus::Cancelled)
    }
}

/// 案件（SPEC §3.3）。仕事の木は `tasks WHERE project_id = ?`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    pub id: ProjectId,
    pub title: String,
    /// 人が投げた依頼文そのまま。
    pub request: String,
    pub status: ProjectStatus,
    /// 秘書の理解確認・方針（Phase 24 で秘書が書く）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secretary_summary: Option<String>,
    /// ADR-0039 D1: この案件の作業場所（コードのある場所）。`Local` は手元の普段のパス（SPEC §2.1）、
    /// `Remote` はクラスタ側の作業ディレクトリ（ADR-0018 D1）。決めていない案件は `None`（従来どおり、
    /// 分解した仕事は親の workspace を継ぐ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<crate::model::WorkspaceSpec>,
    /// ADR-0044 D6: アーカイブした時刻。`None` ならアーカイブされていない。一覧は既定でこれが
    /// `Some` の案件（とそのタスク）を隠す。
    #[serde(default, skip_serializing_if = "Option::is_none", with = "time::serde::rfc3339::option")]
    #[schemars(with = "Option<String>")]
    pub archived_at: Option<OffsetDateTime>,
    /// ADR-0044 D6: `pause` する直前の状態（`resume` の戻り先）。`paused` でなければ `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_from: Option<ProjectStatus>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub updated_at: OffsetDateTime,
}

/// 途中目標の状態（ADR-0033 D2。SPEC §7 のアジャイル: 達成ごとに人が判定し、Go か再設計）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneStatus {
    Proposed,
    Approved,
    InProgress,
    Reached,
    Redesigned,
    /// ADR-0044 D6: 一時停止。属するタスクは dispatch されない（`ready` のまま）。
    /// 元の状態は `Milestone::paused_from` に持ち、`resume` でそこへ戻る。
    Paused,
    /// ADR-0044 D6: 中止。属する非終端タスクは全部 `cancelled` にした後の終端。
    Cancelled,
}

impl MilestoneStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            MilestoneStatus::Proposed => "proposed",
            MilestoneStatus::Approved => "approved",
            MilestoneStatus::InProgress => "in_progress",
            MilestoneStatus::Reached => "reached",
            MilestoneStatus::Redesigned => "redesigned",
            MilestoneStatus::Paused => "paused",
            MilestoneStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "proposed" => Some(MilestoneStatus::Proposed),
            "approved" => Some(MilestoneStatus::Approved),
            "in_progress" => Some(MilestoneStatus::InProgress),
            "reached" => Some(MilestoneStatus::Reached),
            "redesigned" => Some(MilestoneStatus::Redesigned),
            "paused" => Some(MilestoneStatus::Paused),
            "cancelled" => Some(MilestoneStatus::Cancelled),
            _ => None,
        }
    }

    /// ADR-0044 D6: 終端（これ以上動かない）。**達成（`reached`）・再設計（`redesigned`）・
    /// 中止（`cancelled`）の 3 つ**。中止の連鎖（案件ごと止めたとき）はこの 3 つを触らず、
    /// `pause` / `cancel` もこの 3 つには効かない（409）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            MilestoneStatus::Reached | MilestoneStatus::Redesigned | MilestoneStatus::Cancelled
        )
    }
}

/// 人が途中目標に返す 3 つの答え（ADR-0038 D2。SPEC §7）。**達成にするのは人の `ok` だけ**で、
/// 自由記述（`note`）は秘書への言葉として渡すだけ（LLM に 3 値を解釈させない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneDecision {
    /// 達成。提案された次の途中目標を承認し、その分解を起こす。
    Ok,
    /// 議論したい。状態は何も変えず、`note` を秘書への対話として送る。
    Discuss,
    /// 達成にしない。この途中目標と提案を `redesigned` にし、再設計を秘書に頼む。
    Ng,
}

impl MilestoneDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            MilestoneDecision::Ok => "ok",
            MilestoneDecision::Discuss => "discuss",
            MilestoneDecision::Ng => "ng",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(MilestoneDecision::Ok),
            "discuss" => Some(MilestoneDecision::Discuss),
            "ng" => Some(MilestoneDecision::Ng),
            _ => None,
        }
    }
}

/// 途中目標（ADR-0033 D2）。`seq` は案件の中での通し番号（1 始まり。ストアが採番する）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Milestone {
    pub id: MilestoneId,
    pub project_id: ProjectId,
    pub seq: i64,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: MilestoneStatus,
    /// ADR-0044 D6: `pause` する直前の状態（`resume` の戻り先）。`paused` でなければ `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_from: Option<MilestoneStatus>,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schemars(with = "String")]
    pub updated_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, kind: OrgKind) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            name: id.to_string(),
            kind,
            genre: None,
            brief: String::new(),
            position: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_second_secretary_is_rejected() {
        let existing = vec![node("secretary", None, OrgKind::Secretary)];
        let err = validate_upsert(&existing, &node("boss", None, OrgKind::Secretary)).expect_err("rejected");
        assert!(matches!(err, OrgError::DuplicateSecretary { .. }), "{err}");
        // 同じ id の更新は「1 つだけ」に反しない。
        validate_upsert(&existing, &node("secretary", None, OrgKind::Secretary)).expect("update is fine");
    }

    #[test]
    fn a_department_needs_an_existing_parent_and_the_root_must_be_the_secretary() {
        let existing = vec![node("secretary", None, OrgKind::Secretary)];
        assert!(matches!(
            validate_upsert(&existing, &node("coding", None, OrgKind::Department)),
            Err(OrgError::MissingParent { .. })
        ));
        assert!(matches!(
            validate_upsert(&existing, &node("coding", Some("ghost"), OrgKind::Department)),
            Err(OrgError::UnknownParent { .. })
        ));
        validate_upsert(&existing, &node("coding", Some("secretary"), OrgKind::Department)).expect("ok");
    }

    #[test]
    fn nesting_must_go_secretary_then_department_then_section() {
        let existing = vec![
            node("secretary", None, OrgKind::Secretary),
            node("coding", Some("secretary"), OrgKind::Department),
            node("frontend", Some("coding"), OrgKind::Section),
        ];
        assert!(matches!(
            validate_upsert(&existing, &node("sub", Some("frontend"), OrgKind::Section)),
            Err(OrgError::BadNesting { .. })
        ));
        assert!(matches!(
            validate_upsert(&existing, &node("dept", Some("frontend"), OrgKind::Department)),
            Err(OrgError::BadNesting { .. })
        ));
        validate_upsert(&existing, &node("perf", Some("coding"), OrgKind::Section)).expect("ok");
    }

    /// 監査 D-1: `PATCH /org/{id}` で `kind` を変える更新は、既にぶら下がっている子とも整合しなければ
    /// ならない。子を持つ `department` を `section` に変えようとすると「section の下に section」になり
    /// 拒否される。子が無ければ通る。
    #[test]
    fn changing_kind_is_rejected_if_it_would_break_an_existing_childs_nesting() {
        let existing = vec![
            node("secretary", None, OrgKind::Secretary),
            node("coding", Some("secretary"), OrgKind::Department),
            node("frontend", Some("coding"), OrgKind::Section),
        ];
        // coding（部）を section に変えると、既にぶら下がる frontend（課）が「section の下の section」になる。
        assert!(matches!(
            validate_upsert(&existing, &node("coding", Some("secretary"), OrgKind::Section)),
            Err(OrgError::BadNesting { .. })
        ));
        // 子が無ければ kind を変えても通る。
        let childless = vec![
            node("secretary", None, OrgKind::Secretary),
            node("infra", Some("secretary"), OrgKind::Department),
        ];
        validate_upsert(&childless, &node("infra", Some("secretary"), OrgKind::Section)).expect("no children, ok");
    }

    #[test]
    fn a_node_cannot_become_its_own_ancestor() {
        let existing = vec![
            node("secretary", None, OrgKind::Secretary),
            node("coding", Some("secretary"), OrgKind::Department),
            node("frontend", Some("coding"), OrgKind::Section),
        ];
        // coding の親を自分の子（frontend）にしようとする。
        assert!(matches!(
            validate_upsert(&existing, &node("coding", Some("frontend"), OrgKind::Department)),
            Err(OrgError::Cycle { .. })
        ));
        // 自分自身を親にするのも循環。
        assert!(matches!(
            validate_upsert(&existing, &node("coding", Some("coding"), OrgKind::Department)),
            Err(OrgError::Cycle { .. })
        ));
        // 既存データの親の連鎖が閉じていても（壊れた DB）、無限に辿らずに拒否する。
        let broken = vec![
            node("a", Some("b"), OrgKind::Department),
            node("b", Some("a"), OrgKind::Department),
        ];
        assert!(matches!(
            validate_upsert(&broken, &node("c", Some("a"), OrgKind::Section)),
            Err(OrgError::Cycle { .. })
        ));
    }

    #[test]
    fn ids_are_lowercase_kebab_and_names_are_not_blank() {
        let existing: Vec<OrgNode> = vec![];
        assert!(matches!(
            validate_upsert(&existing, &node("Secretary", None, OrgKind::Secretary)),
            Err(OrgError::InvalidId(_))
        ));
        let mut blank = node("secretary", None, OrgKind::Secretary);
        blank.name = "   ".into();
        assert!(matches!(validate_upsert(&existing, &blank), Err(OrgError::BlankName)));
    }

    #[test]
    fn department_of_walks_up_to_the_first_department() {
        let org = vec![
            node("secretary", None, OrgKind::Secretary),
            node("coding", Some("secretary"), OrgKind::Department),
            node("coding-poc", Some("coding"), OrgKind::Section),
            node("research", Some("secretary"), OrgKind::Department),
            node("research-survey", Some("research"), OrgKind::Section),
        ];
        assert_eq!(department_of(&org, "coding-poc").as_deref(), Some("coding"));
        assert_eq!(department_of(&org, "coding").as_deref(), Some("coding"));
        assert_eq!(department_of(&org, "research-survey").as_deref(), Some("research"));
        assert_eq!(department_of(&org, "secretary"), None);
        assert_eq!(department_of(&org, "ghost"), None);
        // 親の連鎖が閉じた壊れたデータでも止まる。
        let broken = vec![
            node("a", Some("b"), OrgKind::Section),
            node("b", Some("a"), OrgKind::Section),
        ];
        assert_eq!(department_of(&broken, "a"), None);
    }

    #[test]
    fn assignee_defaults_walk_the_node_genre_then_the_genre_default_role() {
        let roles = vec![RoleSpec {
            id: "implementer".into(),
            tier: Some(crate::model::Tier::Cheap),
            ..RoleSpec::default()
        }];
        let genres = vec![GenreSpec {
            id: "coding".into(),
            default_role: Some("implementer".into()),
            ..GenreSpec::default()
        }];
        let mut org = vec![node("coding-poc", Some("coding"), OrgKind::Section)];
        org[0].genre = Some("coding".into());
        let (genre, role) = assignee_defaults(&org, "coding-poc", &roles, &genres);
        assert_eq!(genre.as_deref(), Some("coding"));
        assert_eq!(role.map(|r| r.id.as_str()), Some("implementer"));
        // 知らない assignee・分野を持たないノードは何も返さない。
        assert_eq!(assignee_defaults(&org, "ghost", &roles, &genres), (None, None));
        let no_genre = vec![node("infra", Some("secretary"), OrgKind::Department)];
        assert_eq!(assignee_defaults(&no_genre, "infra", &roles, &genres), (None, None));
    }
}
