//! 組織 = Agent Profile の継承木（ADR-0046 D1）。純粋なデータ定義と決定的な merge だけを置く
//! （I/O・LLM 呼び出しはしない。ADR-0001 D2 / DESIGN 原則 1）。
//!
//! ノードは `profile` を持ち、子は親を継ぐ。継ぎ方は D1 の規則そのまま:
//!
//! | 項目 | 規則 |
//! |---|---|
//! | `skills` / `knowledge` / `tools` / `harnesses.allowed` / `permissions.approvals` | 親と**和**（根→葉の順、重複は落とす） |
//! | `deny_tools` | 和。ただし**常に勝つ**（実効の `tools` から引く） |
//! | `run` / `model.tier` / `harnesses.default` / `review.*` | **子が勝つ** |
//! | `model.allowed_tiers` | **交わり**（空の親は制限なし） |
//! | `policy` | 根→葉の順に**連結**（そのまま並べる） |
//! | `budget.max_lane` / `budget.max_attempts` | **最小**（最も厳しい値が勝つ。ADR-0069 D2） |
//!
//! 最後に**タスクの上書き**（`EffectiveProfile::with_task`）。ADR-0033 D2 の
//! 「task > role > assignee > genre」はこれに置き換わる。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::knowledge::KnowledgeMount;
use crate::model::{Task, Tier};
use crate::org::OrgNode;

/// ADR-0046 D8: `tools` の語彙（今回）。`cluster:<id>` だけが接頭辞つき。
pub const TOOL_VOCABULARY: [&str; 4] = ["gh", "tavily", "exa", "docker"];

/// ADR-0046 D8: クラスタの道具の接頭辞（`cluster:pegasus` など）。
pub const CLUSTER_TOOL_PREFIX: &str = "cluster:";

/// ADR-0046 D6: 根ノード（Chief of Staff）の id。
pub const COS_ID: &str = "cos";

/// ADR-0046 D6: 根ノードの表示名。
pub const COS_NAME: &str = "Chief of Staff";

/// ADR-0046 D1: `run`（どこで動かすか）。子が勝つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileRun {
    Host,
    Container,
}

impl ProfileRun {
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileRun::Host => "host",
            ProfileRun::Container => "container",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "host" => Some(ProfileRun::Host),
            "container" => Some(ProfileRun::Container),
            _ => None,
        }
    }
}

/// ADR-0046 D1 / D3: このノードが受けられるハーネス。`allowed` は親と和、`default` は子が勝つ。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HarnessPrefs {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl HarnessPrefs {
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty() && self.default.is_none()
    }
}

/// ADR-0046 D1: モデルの段（`tier` は子が勝つ、`allowed_tiers` は交わり）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tiers: Vec<Tier>,
}

impl ModelPrefs {
    pub fn is_empty(&self) -> bool {
        self.tier.is_none() && self.allowed_tiers.is_empty()
    }
}

/// ADR-0046 D1: レビューの既定（子が勝つ）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// ADR-0069 D2 / D6（Phase 114）: レビュー不合格のやり直しで lane を 1 段上げてよいか
    /// （`false` なら上げない。省略時は上げてよい）。子が勝つ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalate_on_fail: Option<bool>,
}

impl ReviewPrefs {
    pub fn is_empty(&self) -> bool {
        self.harness.is_none() && self.tier.is_none() && self.escalate_on_fail.is_none()
    }
}

/// ADR-0069 D2（Phase 114）: そのノード以下の予算の天井。どちらも**最も厳しい値が勝つ**
/// （根→葉の最小。子は緩められない）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BudgetPrefs {
    /// 使ってよい最も高い lane（`allowed_tiers` と合わせて天井になる）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lane: Option<Tier>,
    /// 1 タスクあたりの試行回数の上限（エスカレーション込み。タスクの `max_retries + 1` も超えない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
}

impl BudgetPrefs {
    pub fn is_empty(&self) -> bool {
        self.max_lane.is_none() && self.max_attempts.is_none()
    }
}

/// ADR-0046 D1 / D8: そのノード以下で once / standing の対象になる操作の名前（親と和）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<String>,
}

impl Permissions {
    pub fn is_empty(&self) -> bool {
        self.approvals.is_empty()
    }
}

/// ADR-0046 D1: ノードが持つ profile。**全ての項目が任意**（既定は空）で、空の profile は
/// `org_nodes.profile_json` にも JSON にも出ない（導入前のノードと 1 バイトも変わらない）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// 能力タグ（ADR-0046 D2）。親と和。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// ADR-0047 の知識のマウント。親と和。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge: Vec<KnowledgeMount>,
    /// ADR-0056 D3（Phase 78）: KB の `skills/<name>/SKILL.md` をこのノードに mount する（skill 名の
    /// 一覧）。継承は `knowledge` と同じ規則（親と和。届け方は Phase 79）。`skills`（マッチングの能力
    /// タグ）とは別物。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills_mounts: Vec<String>,
    #[serde(default, skip_serializing_if = "HarnessPrefs::is_empty")]
    pub harnesses: HarnessPrefs,
    /// ADR-0046 D8 の語彙。親と和。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// 禁止する道具。和だが**常に勝つ**（実効の `tools` から引かれる）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<ProfileRun>,
    #[serde(default, skip_serializing_if = "ModelPrefs::is_empty")]
    pub model: ModelPrefs,
    /// 根→葉の順に連結される（「文化」の箇条書き）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy: Vec<String>,
    #[serde(default, skip_serializing_if = "ReviewPrefs::is_empty")]
    pub review: ReviewPrefs,
    #[serde(default, skip_serializing_if = "Permissions::is_empty")]
    pub permissions: Permissions,
    /// ADR-0069 D2（Phase 114）: 予算の天井（最も厳しい値が勝つ）。
    #[serde(default, skip_serializing_if = "BudgetPrefs::is_empty")]
    pub budget: BudgetPrefs,
}

impl Profile {
    /// 空の profile か（`skip_serializing_if` 用）。
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
            && self.knowledge.is_empty()
            && self.skills_mounts.is_empty()
            && self.harnesses.is_empty()
            && self.tools.is_empty()
            && self.deny_tools.is_empty()
            && self.run.is_none()
            && self.model.is_empty()
            && self.policy.is_empty()
            && self.review.is_empty()
            && self.permissions.is_empty()
            && self.budget.is_empty()
    }
}

/// ADR-0046 D1: 根から葉まで merge した結果。前置き・matching・道具の受け渡しはこれだけを見る。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectiveProfile {
    /// 対象のノード（知らない id なら空文字列）。
    pub node_id: String,
    /// 根→葉のノード id（GUI が「どこから継いだか」を出すため）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge: Vec<KnowledgeMount>,
    /// ADR-0056 D3（Phase 78）: 継いだ後の skills mount（skill 名。`knowledge` と同じ和の規則）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills_mounts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harnesses_allowed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_default: Option<String>,
    /// `deny_tools` を引いた後の道具。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<ProfileRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tiers: Vec<Tier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_tier: Option<Tier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<String>,
    /// ADR-0069 D2: 継いだ lane の上限（根→葉の最小）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lane: Option<Tier>,
    /// ADR-0069 D2: 継いだ試行回数の上限（根→葉の最小）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    /// ADR-0069 D6: レビュー不合格で lane を上げてよいか（子が勝つ。`None` は上げてよい）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_escalate_on_fail: Option<bool>,
}

impl EffectiveProfile {
    /// ADR-0069 D2: このノードの lane の天井（`allowed_tiers` と `budget.max_lane`）。
    pub fn lane_ceiling(&self) -> crate::model_policy::LaneCeiling {
        crate::model_policy::LaneCeiling {
            allowed: self.allowed_tiers.clone(),
            max_lane: self.max_lane,
        }
    }

    /// 継承した結果が「何も無い」か（`chain` と `node_id` 以外が空）。Phase 59 より前の組織
    /// （profile を 1 つも書いていない）では真になり、前置きに profile の節を出さない。
    pub fn is_trivial(&self) -> bool {
        self.skills.is_empty()
            && self.knowledge.is_empty()
            && self.skills_mounts.is_empty()
            && self.harnesses_allowed.is_empty()
            && self.harness_default.is_none()
            && self.tools.is_empty()
            && self.deny_tools.is_empty()
            && self.run.is_none()
            && self.tier.is_none()
            && self.allowed_tiers.is_empty()
            && self.policy.is_empty()
            && self.review_harness.is_none()
            && self.review_tier.is_none()
            && self.approvals.is_empty()
            && self.max_lane.is_none()
            && self.max_attempts.is_none()
            && self.review_escalate_on_fail.is_none()
    }

    /// そのハーネスをこのノードが受けられるか（ADR-0046 D3 / D5）。`harnesses_allowed` が空なら
    /// 「何も受けられない」（matching の候補から外れる）。
    pub fn allows_harness(&self, harness: &str) -> bool {
        self.harnesses_allowed.iter().any(|h| h == harness)
    }

    /// そのノードがその道具を使えるか（ADR-0046 D8）。
    pub fn has_tool(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }

    /// ADR-0046 D8: 使えるクラスタの id（`cluster:<id>` の `<id>`。並びは `tools` の順）。
    pub fn clusters(&self) -> Vec<&str> {
        self.tools
            .iter()
            .filter_map(|t| t.strip_prefix(CLUSTER_TOOL_PREFIX))
            .filter(|id| !id.is_empty())
            .collect()
    }

    /// ADR-0046 D1: 最後に効く**タスクの上書き**。
    ///
    /// - `harness`（`Task.genre` = ハーネス id）は `harness_default` を上書きし、`harnesses_allowed`
    ///   には足さない（受けられるかどうかは matching / 422 の判定で別に見る）。
    /// - `tier`（`Task.worker_hint.tier`）は `tier` を上書きする。
    /// - `skills`（`Task.skills`）は空でなければ `skills` を置き換える（そのタスクに要る能力）。
    ///
    /// `run` と `repos` はタスクの列に無い（`run` はリポジトリの `run`（ADR-0043 D3）が、`repos` は
    /// `Task.repos` がそれぞれ既に持っている）ので、ここでは触らない。
    pub fn with_task(mut self, task: &Task) -> Self {
        if let Some(harness) = task.genre.as_ref().filter(|g| !g.is_empty()) {
            self.harness_default = Some(harness.clone());
        }
        self.tier = Some(task.worker_hint.tier);
        if !task.skills.is_empty() {
            self.skills = task.skills.clone();
        }
        self
    }
}

/// 根→葉のノードの並び（`node_id` を含む）。知らない id・親の連鎖が壊れているときは、
/// 辿れたところまでを返す（無限には辿らない）。
pub fn ancestry<'a>(nodes: &'a [OrgNode], node_id: &str) -> Vec<&'a OrgNode> {
    let mut chain: Vec<&OrgNode> = Vec::new();
    let mut cursor = nodes.iter().find(|n| n.id == node_id);
    let mut seen = 0usize;
    while let Some(node) = cursor {
        chain.push(node);
        seen += 1;
        if seen > nodes.len() {
            break;
        }
        cursor = node
            .parent_id
            .as_deref()
            .and_then(|p| nodes.iter().find(|n| n.id == p));
        if let Some(next) = cursor
            && chain.iter().any(|c| c.id == next.id)
        {
            break;
        }
    }
    chain.reverse();
    chain
}

/// ADR-0046 D1: 根から葉まで merge した実効 profile（純粋関数）。
pub fn resolve(nodes: &[OrgNode], node_id: &str) -> EffectiveProfile {
    let chain = ancestry(nodes, node_id);
    let mut out = EffectiveProfile {
        node_id: chain.last().map(|n| n.id.clone()).unwrap_or_default(),
        chain: chain.iter().map(|n| n.id.clone()).collect(),
        ..EffectiveProfile::default()
    };
    // `allowed_tiers` は交わり。空の親は「制限なし」なので、最初に非空を見るまでは `None`。
    let mut allowed_tiers: Option<Vec<Tier>> = None;
    for node in &chain {
        let p = &node.profile;
        push_unique(&mut out.skills, p.skills.iter().cloned());
        for k in &p.knowledge {
            if !out.knowledge.contains(k) {
                out.knowledge.push(k.clone());
            }
        }
        push_unique(&mut out.skills_mounts, p.skills_mounts.iter().cloned());
        push_unique(
            &mut out.harnesses_allowed,
            p.harnesses.allowed.iter().cloned(),
        );
        push_unique(&mut out.tools, p.tools.iter().cloned());
        push_unique(&mut out.deny_tools, p.deny_tools.iter().cloned());
        push_unique(&mut out.approvals, p.permissions.approvals.iter().cloned());
        // policy は連結（重複も残す。根→葉の順）。
        out.policy.extend(p.policy.iter().cloned());
        // 子が勝つ。
        if p.harnesses.default.is_some() {
            out.harness_default = p.harnesses.default.clone();
        }
        if p.run.is_some() {
            out.run = p.run;
        }
        if p.model.tier.is_some() {
            out.tier = p.model.tier;
        }
        if p.review.harness.is_some() {
            out.review_harness = p.review.harness.clone();
        }
        if p.review.tier.is_some() {
            out.review_tier = p.review.tier;
        }
        if p.review.escalate_on_fail.is_some() {
            out.review_escalate_on_fail = p.review.escalate_on_fail;
        }
        // ADR-0069 D2: 予算の天井は最も厳しい値が勝つ（子は緩められない）。
        if let Some(max) = p.budget.max_lane {
            out.max_lane = Some(match out.max_lane {
                Some(cur)
                    if crate::model_policy::lane_rank(cur)
                        <= crate::model_policy::lane_rank(max) =>
                {
                    cur
                }
                _ => max,
            });
        }
        if let Some(max) = p.budget.max_attempts {
            out.max_attempts = Some(out.max_attempts.map_or(max, |cur| cur.min(max)));
        }
        // 交わり（空は制限なし）。
        if !p.model.allowed_tiers.is_empty() {
            allowed_tiers = Some(match allowed_tiers {
                None => p.model.allowed_tiers.clone(),
                Some(current) => current
                    .into_iter()
                    .filter(|t| p.model.allowed_tiers.contains(t))
                    .collect(),
            });
        }
    }
    out.allowed_tiers = allowed_tiers.unwrap_or_default();
    // deny_tools は常に勝つ。
    out.tools.retain(|t| !out.deny_tools.iter().any(|d| d == t));
    out
}

fn push_unique(out: &mut Vec<String>, items: impl IntoIterator<Item = String>) {
    for item in items {
        if !out.iter().any(|x| x == &item) {
            out.push(item);
        }
    }
}

/// profile の検証の失敗（ADR-0046 D1）。API は 422 にする。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    #[error("unknown tool {tool:?} (allowed: gh, tavily, exa, docker, cluster:<id>)")]
    UnknownTool { tool: String },
    #[error("unknown harness {harness:?} in {field} (known: {known})")]
    UnknownHarness {
        harness: String,
        field: &'static str,
        known: String,
    },
    #[error("skill {skill:?} must match [a-z0-9._-] (lowercase, 1..=64 characters)")]
    InvalidSkill { skill: String },
    /// ADR-0056 D3（Phase 78）: `skills_mounts` の名前は KB の `skills/<name>/` と同じ綴り。
    #[error("skill mount {name:?} must match [a-z0-9-] (lowercase, 1..=64 characters)")]
    InvalidSkillMount { name: String },
}

/// ADR-0046 D2: skill タグの綴り（小文字・`[a-z0-9._-]`・1..=64 文字）。
pub fn is_valid_skill(skill: &str) -> bool {
    !skill.is_empty()
        && skill.chars().count() <= 64
        && skill.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.'
        })
}

/// ADR-0046 D8: 道具の綴りが語彙にあるか（`cluster:<id>` は `<id>` が非空であればよい。
/// クラスタが `[[clusters]]` にあるかは設定側の話なのでここでは見ない）。
pub fn is_known_tool(tool: &str) -> bool {
    if let Some(id) = tool.strip_prefix(CLUSTER_TOOL_PREFIX) {
        return !id.is_empty();
    }
    TOOL_VOCABULARY.contains(&tool)
}

/// ADR-0046 D1: profile の決定的な検証。`known_harnesses` が空なら harness の検査はしない
/// （`[[genres]]` / `[[harnesses]]` を使わない最小構成を壊さないため。`handlers::genre` と同じ規律）。
pub fn validate_profile(profile: &Profile, known_harnesses: &[String]) -> Result<(), ProfileError> {
    for skill in &profile.skills {
        if !is_valid_skill(skill) {
            return Err(ProfileError::InvalidSkill {
                skill: skill.clone(),
            });
        }
    }
    for name in &profile.skills_mounts {
        if !crate::knowledge::is_valid_skill_name(name) {
            return Err(ProfileError::InvalidSkillMount { name: name.clone() });
        }
    }
    for tool in profile.tools.iter().chain(profile.deny_tools.iter()) {
        if !is_known_tool(tool) {
            return Err(ProfileError::UnknownTool { tool: tool.clone() });
        }
    }
    if known_harnesses.is_empty() {
        return Ok(());
    }
    let known = || known_harnesses.join(", ");
    for h in &profile.harnesses.allowed {
        if !known_harnesses.iter().any(|k| k == h) {
            return Err(ProfileError::UnknownHarness {
                harness: h.clone(),
                field: "harnesses.allowed",
                known: known(),
            });
        }
    }
    for (field, value) in [
        ("harnesses.default", profile.harnesses.default.as_deref()),
        ("review.harness", profile.review.harness.as_deref()),
    ] {
        if let Some(h) = value
            && !known_harnesses.iter().any(|k| k == h)
        {
            return Err(ProfileError::UnknownHarness {
                harness: h.to_string(),
                field,
                known: known(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::OrgKind;
    use time::OffsetDateTime;

    fn node(id: &str, parent: Option<&str>, profile: Profile) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            name: id.to_string(),
            kind: if parent.is_none() {
                OrgKind::Secretary
            } else {
                OrgKind::Department
            },
            genre: None,
            brief: String::new(),
            profile,
            position: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn tree() -> Vec<OrgNode> {
        vec![
            node(
                "cos",
                None,
                Profile {
                    skills: vec!["coordination".into()],
                    tools: vec!["gh".into(), "docker".into()],
                    policy: vec!["根の方針".into()],
                    model: ModelPrefs {
                        tier: Some(Tier::Standard),
                        allowed_tiers: vec![Tier::Frontier, Tier::Standard, Tier::Cheap],
                    },
                    harnesses: HarnessPrefs {
                        allowed: vec!["conversation".into(), "plan".into()],
                        default: Some("conversation".into()),
                    },
                    permissions: Permissions {
                        approvals: vec!["external-post".into()],
                    },
                    run: Some(ProfileRun::Host),
                    ..Profile::default()
                },
            ),
            node(
                "engineering",
                Some("cos"),
                Profile {
                    skills: vec!["software".into(), "coordination".into()],
                    harnesses: HarnessPrefs {
                        allowed: vec!["coding".into()],
                        default: None,
                    },
                    policy: vec!["部の方針".into()],
                    model: ModelPrefs {
                        tier: None,
                        allowed_tiers: vec![Tier::Standard, Tier::Cheap],
                    },
                    ..Profile::default()
                },
            ),
            node(
                "software-engineering",
                Some("engineering"),
                Profile {
                    skills: vec!["rust".into()],
                    harnesses: HarnessPrefs {
                        allowed: vec![],
                        default: Some("coding".into()),
                    },
                    deny_tools: vec!["docker".into()],
                    tools: vec!["tavily".into()],
                    policy: vec!["課の方針".into()],
                    model: ModelPrefs {
                        tier: Some(Tier::Cheap),
                        allowed_tiers: vec![],
                    },
                    run: Some(ProfileRun::Container),
                    review: ReviewPrefs {
                        escalate_on_fail: None,
                        harness: Some("reviewer".into()),
                        tier: Some(Tier::Cheap),
                    },
                    permissions: Permissions {
                        approvals: vec!["cluster-write".into()],
                    },
                    ..Profile::default()
                },
            ),
        ]
    }

    /// ADR-0046 §4-1: 和・子勝ち・deny 勝ち・交わり・連結。
    #[test]
    fn resolve_merges_lists_by_union_scalars_by_child_and_tiers_by_intersection() {
        let org = tree();
        let eff = resolve(&org, "software-engineering");
        assert_eq!(eff.node_id, "software-engineering");
        assert_eq!(
            eff.chain,
            vec!["cos", "engineering", "software-engineering"]
        );
        // 和（根→葉の順、重複は落ちる）。
        assert_eq!(eff.skills, vec!["coordination", "software", "rust"]);
        assert_eq!(
            eff.harnesses_allowed,
            vec!["conversation", "plan", "coding"]
        );
        assert_eq!(eff.approvals, vec!["external-post", "cluster-write"]);
        // deny が常に勝つ（`docker` は根で与えられていても消える）。
        assert_eq!(eff.tools, vec!["gh", "tavily"]);
        assert_eq!(eff.deny_tools, vec!["docker"]);
        // 子が勝つ。
        assert_eq!(eff.harness_default.as_deref(), Some("coding"));
        assert_eq!(eff.run, Some(ProfileRun::Container));
        assert_eq!(eff.tier, Some(Tier::Cheap));
        assert_eq!(eff.review_harness.as_deref(), Some("reviewer"));
        assert_eq!(eff.review_tier, Some(Tier::Cheap));
        // 交わり（葉の空は「制限なし」なので親の交わりがそのまま残る）。
        assert_eq!(eff.allowed_tiers, vec![Tier::Standard, Tier::Cheap]);
        // 連結（根→葉。重複も残す）。
        assert_eq!(eff.policy, vec!["根の方針", "部の方針", "課の方針"]);
    }

    /// ADR-0056 D3（Phase 78）: `skills_mounts` は `knowledge` と同じ和の規則（親と子の重複は落ちる）。
    #[test]
    fn skills_mounts_are_unioned_like_knowledge_mounts() {
        let org = vec![
            node(
                "cos",
                None,
                Profile {
                    skills_mounts: vec!["writing".to_string()],
                    ..Profile::default()
                },
            ),
            node(
                "engineering",
                Some("cos"),
                Profile {
                    skills_mounts: vec!["rust-review".to_string(), "writing".to_string()],
                    ..Profile::default()
                },
            ),
        ];
        assert_eq!(
            resolve(&org, "cos").skills_mounts,
            vec!["writing".to_string()]
        );
        assert_eq!(
            resolve(&org, "engineering").skills_mounts,
            vec!["writing".to_string(), "rust-review".to_string()]
        );
    }

    /// 親が空の `allowed_tiers`（制限なし）でも、子の制限はそのまま効く。
    #[test]
    fn an_empty_parent_allowed_tiers_means_no_restriction() {
        let org = vec![
            node("cos", None, Profile::default()),
            node(
                "engineering",
                Some("cos"),
                Profile {
                    model: ModelPrefs {
                        tier: None,
                        allowed_tiers: vec![Tier::Cheap],
                    },
                    ..Profile::default()
                },
            ),
        ];
        assert_eq!(
            resolve(&org, "engineering").allowed_tiers,
            vec![Tier::Cheap]
        );
        assert!(resolve(&org, "cos").allowed_tiers.is_empty());
    }

    #[test]
    fn resolve_of_an_unknown_node_is_empty_and_a_broken_chain_terminates() {
        let org = tree();
        let eff = resolve(&org, "ghost");
        assert_eq!(eff, EffectiveProfile::default());
        // 親の連鎖が閉じた壊れたデータでも止まる。
        let broken = vec![
            node("a", Some("b"), Profile::default()),
            node("b", Some("a"), Profile::default()),
        ];
        let eff = resolve(&broken, "a");
        assert!(eff.chain.len() <= broken.len() + 1, "{:?}", eff.chain);
    }

    #[test]
    fn effective_tools_expose_the_clusters_and_the_allows_helpers() {
        let org = vec![node(
            "cluster-hpc",
            None,
            Profile {
                tools: vec![
                    "cluster:pegasus".into(),
                    "cluster:sirius".into(),
                    "gh".into(),
                ],
                harnesses: HarnessPrefs {
                    allowed: vec!["coding".into()],
                    default: None,
                },
                ..Profile::default()
            },
        )];
        let eff = resolve(&org, "cluster-hpc");
        assert_eq!(eff.clusters(), vec!["pegasus", "sirius"]);
        assert!(eff.has_tool("gh"));
        assert!(!eff.has_tool("docker"));
        assert!(eff.allows_harness("coding"));
        assert!(!eff.allows_harness("literature"));
    }

    /// ADR-0046 D1: タスクの上書きが最後に効く。
    #[test]
    fn with_task_overrides_harness_tier_and_skills() {
        let org = tree();
        let eff = resolve(&org, "software-engineering");
        let mut task = crate::model::Task {
            genre: Some("literature".into()),
            skills: vec!["benchmark".into()],
            ..sample_task()
        };
        task.worker_hint.tier = Tier::Frontier;
        let out = eff.clone().with_task(&task);
        assert_eq!(out.harness_default.as_deref(), Some("literature"));
        assert_eq!(out.tier, Some(Tier::Frontier));
        assert_eq!(out.skills, vec!["benchmark"]);
        // `harnesses_allowed` は増えない（受けられるかは別に判定する）。
        assert_eq!(out.harnesses_allowed, eff.harnesses_allowed);
        // skills が空のタスクはノードの skills をそのまま残す。
        let bare = crate::model::Task {
            genre: None,
            skills: vec![],
            ..sample_task()
        };
        assert_eq!(eff.clone().with_task(&bare).skills, eff.skills);
    }

    fn sample_task() -> crate::model::Task {
        use crate::model::{Budget, Status, TaskKind, WorkerHint, WorkspaceSpec};
        let now = OffsetDateTime::now_utc();
        crate::model::Task {
            routing: None,
            id: crate::model::TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Draft,
            priority: 10,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::local("/tmp"),
            repos: vec![],
            budget: Budget {
                max_turns: 1,
                max_wall_secs: 1,
                max_retries: 0,
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
            labels: vec![],
            category: crate::model::TaskCategory::Other,
            skills: vec![],
            mode: crate::model::TaskMode::Production,
            conversation: None,
        }
    }

    #[test]
    fn validate_profile_rejects_unknown_tools_harnesses_and_skills() {
        let known = vec!["coding".to_string(), "conversation".to_string()];
        let ok = Profile {
            skills: vec!["io_uring".into(), "rust".into()],
            tools: vec!["gh".into(), "cluster:pegasus".into()],
            harnesses: HarnessPrefs {
                allowed: vec!["coding".into()],
                default: Some("coding".into()),
            },
            review: ReviewPrefs {
                escalate_on_fail: None,
                harness: Some("conversation".into()),
                tier: None,
            },
            ..Profile::default()
        };
        validate_profile(&ok, &known).expect("ok");

        let bad_tool = Profile {
            tools: vec!["kubectl".into()],
            ..Profile::default()
        };
        assert!(matches!(
            validate_profile(&bad_tool, &known),
            Err(ProfileError::UnknownTool { .. })
        ));
        // `cluster:` だけ（id が空）も駄目。
        let empty_cluster = Profile {
            tools: vec!["cluster:".into()],
            ..Profile::default()
        };
        assert!(validate_profile(&empty_cluster, &known).is_err());
        let bad_harness = Profile {
            harnesses: HarnessPrefs {
                allowed: vec!["ghost".into()],
                default: None,
            },
            ..Profile::default()
        };
        assert!(matches!(
            validate_profile(&bad_harness, &known),
            Err(ProfileError::UnknownHarness {
                field: "harnesses.allowed",
                ..
            })
        ));
        let bad_review = Profile {
            review: ReviewPrefs {
                escalate_on_fail: None,
                harness: Some("ghost".into()),
                tier: None,
            },
            ..Profile::default()
        };
        assert!(matches!(
            validate_profile(&bad_review, &known),
            Err(ProfileError::UnknownHarness {
                field: "review.harness",
                ..
            })
        ));
        let bad_skill = Profile {
            skills: vec!["IO_Uring".into()],
            ..Profile::default()
        };
        assert!(matches!(
            validate_profile(&bad_skill, &known),
            Err(ProfileError::InvalidSkill { .. })
        ));
        // `known_harnesses` が空なら harness は検査しない（最小構成）。
        validate_profile(&bad_harness, &[]).expect("no registry: skip");

        // ADR-0056 D3: `skills_mounts` は `[a-z0-9-]{1,64}`。
        let bad_skill_mount = Profile {
            skills_mounts: vec!["Rust Review".into()],
            ..Profile::default()
        };
        assert!(matches!(
            validate_profile(&bad_skill_mount, &known),
            Err(ProfileError::InvalidSkillMount { .. })
        ));
        let ok_skill_mount = Profile {
            skills_mounts: vec!["rust-review".into()],
            ..Profile::default()
        };
        validate_profile(&ok_skill_mount, &known).expect("ok skill mount");
    }

    /// 空の profile は JSON に何も出さない（導入前のノードと 1 バイトも変わらない）。
    #[test]
    fn an_empty_profile_serializes_to_an_empty_object() {
        assert!(Profile::default().is_empty());
        assert_eq!(
            serde_json::to_string(&Profile::default()).expect("json"),
            "{}"
        );
        let round: Profile = serde_json::from_str("{}").expect("parse");
        assert_eq!(round, Profile::default());
        // 知らない項目は拒否する（綴り間違いの検出）。
        assert!(serde_json::from_str::<Profile>(r#"{"skils":[]}"#).is_err());
        // `run` の enum は綴りを見る（API は 422 にする）。
        assert!(serde_json::from_str::<Profile>(r#"{"run":"bogus"}"#).is_err());
        assert!(serde_json::from_str::<Profile>(r#"{"run":"container"}"#).is_ok());
        assert!(serde_json::from_str::<Profile>(r#"{"model":{"tier":"bogus"}}"#).is_err());
    }

    /// ADR-0069 D2（Phase 114）: 予算の天井は最も厳しい値が勝ち、`allowed_tiers` と合わせて
    /// lane の天井になる。導入前の profile（`budget` / `escalate_on_fail` 無し）はそのまま読める。
    #[test]
    fn budget_ceiling_takes_the_strictest_value_and_old_profiles_parse() {
        let mut nodes = tree();
        nodes[0].profile.budget = BudgetPrefs {
            max_lane: Some(Tier::Standard),
            max_attempts: Some(3),
        };
        // 子が緩めようとしても効かない
        nodes[1].profile.budget = BudgetPrefs {
            max_lane: Some(Tier::Frontier),
            max_attempts: Some(5),
        };
        nodes[1].profile.review.escalate_on_fail = Some(false);
        let eff = resolve(&nodes, "engineering");
        assert_eq!(eff.max_lane, Some(Tier::Standard));
        assert_eq!(eff.max_attempts, Some(3));
        assert_eq!(eff.review_escalate_on_fail, Some(false));
        let ceiling = eff.lane_ceiling();
        assert!(!ceiling.permits(Tier::Frontier));
        assert_eq!(ceiling.clamp(Tier::Frontier).0, Tier::Standard);
        // 子がさらに厳しくするのは効く
        nodes[1].profile.budget.max_lane = Some(Tier::Cheap);
        assert_eq!(resolve(&nodes, "engineering").max_lane, Some(Tier::Cheap));

        let old: Profile =
            serde_json::from_str(r#"{"model":{"tier":"cheap"},"review":{"tier":"cheap"}}"#)
                .unwrap();
        assert!(old.budget.is_empty());
        assert_eq!(old.review.escalate_on_fail, None);
        // 空の budget は JSON に出ない（導入前と 1 バイトも変わらない）
        assert_eq!(
            serde_json::to_string(&old).unwrap(),
            r#"{"model":{"tier":"cheap"},"review":{"tier":"cheap"}}"#
        );
    }
}
