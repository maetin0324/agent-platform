//! ADR-0069 D3（Phase 114）: タスクの性質（`TaskFeatures`）から **lane**（`Tier` の直列化名
//! `frontier` / `standard` / `cheap` を品質／予算の lane として読む）を決める決定的な policy。
//!
//! - 単一スコアではなく**規則表**（上から評価し、最初に当たった規則の `rule_id` を記録する）。
//! - 特徴量は `TaskFeatures::infer(&Task)` がタスクの列から決定的に作る（LLM は呼ばない。DESIGN 原則 1）。
//! - lane → provider / model / reasoning effort は別の層（`model_routing`・`TieredAdapter`）。残量による
//!   調整（`model_routing::select_tier`）はこの後に別の層として効く。
//! - Phase 2 の shadow 分類器のための境界（`ShadowClassifier` / `LaneDecision.shadow`）だけを予約する。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{
    Check, Task, TaskCategory, TaskKind, TaskMode, Tier, TierSource, WorkspaceSpec,
};

/// policy の版（監査記録に残す。規則表を変えたら上げる）。
pub const LANE_POLICY_VERSION: &str = "lane-policy/1";

/// 各軸の段階（小さな順序尺度）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Low,
    Medium,
    High,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Low => "low",
            Level::Medium => "medium",
            Level::High => "high",
        }
    }
}

/// ADR-0069 D3: lane を決めるためのタスクの性質（9 軸）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskFeatures {
    /// 判断の重さ（設計・方針・調査の結論を出すか）。
    pub judgment: Level,
    /// 要求の曖昧さ。
    pub ambiguity: Level,
    /// 結果を決定的に検証できるか（command の検査があるか）。
    pub verifiability: Level,
    /// やり直しやすさ（worktree の中の変更は高い、クラスタ・本番操作は低い）。
    pub reversibility: Level,
    /// 失敗したときの損失。
    pub consequence: Level,
    /// 読むべき文脈の大きさ。
    pub context_size: Level,
    /// 道具（コマンド実行・リモート）を使う度合い。
    pub tool_intensity: Level,
    /// 想定される長さ（budget から）。
    pub expected_length: Level,
    /// 複数のリポジトリ・領域をまたぐか。
    pub cross_cutting: Level,
}

/// ADR-0069 D3: `TaskFeatures` の明示の上書き（書いた軸だけが勝つ）。API の `features` と CoS の
/// `create_task.features` から入る（features は「仕事の性質の記述」であってモデルの選択ではない）。
/// ADR-0074 D5.1（Phase F1）: `deny_unknown_fields`（`lane`/`assignee`/`tier`/`model` のような
/// 担当・モデルの選択を紛れ込ませない。書けば schema 違反として計画の検証エラーになる）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskFeatureHints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judgment: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambiguity: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifiability: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversibility: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consequence: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_size: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_intensity: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_length: Option<Level>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cross_cutting: Option<Level>,
}

impl TaskFeatureHints {
    pub fn is_empty(&self) -> bool {
        *self == TaskFeatureHints::default()
    }

    /// 書いた軸だけを `features` に上書きし、上書きした軸の名前を返す。
    pub fn apply(&self, features: &mut TaskFeatures) -> Vec<&'static str> {
        let mut out = Vec::new();
        let pairs: [(&'static str, Option<Level>, &mut Level); 9] = [
            ("judgment", self.judgment, &mut features.judgment),
            ("ambiguity", self.ambiguity, &mut features.ambiguity),
            (
                "verifiability",
                self.verifiability,
                &mut features.verifiability,
            ),
            (
                "reversibility",
                self.reversibility,
                &mut features.reversibility,
            ),
            ("consequence", self.consequence, &mut features.consequence),
            (
                "context_size",
                self.context_size,
                &mut features.context_size,
            ),
            (
                "tool_intensity",
                self.tool_intensity,
                &mut features.tool_intensity,
            ),
            (
                "expected_length",
                self.expected_length,
                &mut features.expected_length,
            ),
            (
                "cross_cutting",
                self.cross_cutting,
                &mut features.cross_cutting,
            ),
        ];
        for (name, hint, slot) in pairs {
            if let Some(level) = hint {
                *slot = level;
                out.push(name);
            }
        }
        out
    }
}

/// 判断が重い仕事の語（設計・方針・選定）。
const JUDGMENT_WORDS: &[&str] = &[
    "設計",
    "方針",
    "選定",
    "比較検討",
    "アーキテクチャ",
    "トレードオフ",
    "戦略",
    "design",
    "architecture",
    "trade-off",
    "tradeoff",
    "strategy",
];

/// 機械的な仕事の語。
const MECHANICAL_WORDS: &[&str] = &[
    "typo",
    "誤字",
    "rename",
    "リネーム",
    "改名",
    "整形",
    "cargo fmt",
    "formatting",
    "bump",
    "置換",
    "一括置換",
    "clippy",
    "lint の警告",
    "定型",
];

/// 探索的（要求が固まっていない）な仕事の語。
const EXPLORATORY_WORDS: &[&str] = &[
    "検討",
    "探索",
    "要調査",
    "可能性",
    "investigate",
    "explore",
    "brainstorm",
];

/// やり直しにくい操作の語。
const IRREVERSIBLE_WORDS: &[&str] = &[
    "本番",
    "デプロイ",
    "deploy",
    "マイグレーション",
    "migration",
    "drop table",
    "rm -rf",
    "外部に投稿",
    "送信",
];

/// 失敗の損失が大きい領域の語。
const HIGH_STAKES_WORDS: &[&str] = &[
    "本番",
    "セキュリティ",
    "security",
    "認証",
    "課金",
    "billing",
    "データ損失",
    "migration",
    "マイグレーション",
];

/// 横断的な仕事の語。
const CROSS_CUTTING_WORDS: &[&str] = &["横断", "cross-cutting", "全クレート", "workspace-wide"];

/// 判断を主とするハーネス。
const JUDGMENT_HARNESSES: &[&str] = &["literature", "web-research", "writing", "plan"];

/// 機械的な定型 run のハーネス。
const ROUTINE_HARNESSES: &[&str] = &["smoke", "knowledge"];

/// 道具を多く使うハーネス。
const TOOL_HARNESSES: &[&str] = &["coding", "data-analysis"];

const PATH_SUFFIXES: &[&str] = &[
    ".rs", ".ts", ".tsx", ".js", ".py", ".md", ".toml", ".json", ".yaml", ".yml", ".sh", ".sql",
];

fn contains_any(text: &str, words: &[&str]) -> bool {
    words.iter().any(|w| text.contains(w))
}

fn mentions_path(text: &str) -> bool {
    text.split(|c: char| {
        c.is_whitespace() || matches!(c, '`' | '「' | '」' | '（' | '）' | '(' | ')')
    })
    .any(|tok| {
        let tok = tok.trim_matches(|c: char| matches!(c, ',' | '.' | '、' | '。' | ':'));
        (tok.contains('/') && !tok.starts_with("http"))
            || PATH_SUFFIXES
                .iter()
                .any(|s| tok.len() > s.len() && tok.ends_with(s))
    })
}

impl TaskFeatures {
    /// タスクの列から決定的に作る（LLM は呼ばない）。上書き（`Task.routing.features`）は含めない。
    pub fn infer(task: &Task) -> TaskFeatures {
        let harness = task.genre.as_deref().unwrap_or("");
        let text = format!("{}\n{}", task.title, task.objective).to_lowercase();
        let objective_len = task.objective.chars().count();
        let has_command = task
            .acceptance
            .iter()
            .any(|c| matches!(c.check, Check::Command { .. }));
        let has_artifact = task.acceptance.iter().any(|c| {
            matches!(
                c.check,
                Check::ArtifactExists { .. } | Check::KnowledgePage { .. }
            )
        });
        let has_human = task
            .acceptance
            .iter()
            .any(|c| matches!(c.check, Check::Human));
        let remote = matches!(task.workspace, WorkspaceSpec::Remote { .. });
        let mechanical = contains_any(&text, MECHANICAL_WORDS);
        let exploratory = contains_any(&text, EXPLORATORY_WORDS);

        let judgment = if task.kind == TaskKind::Plan
            || task.mode == TaskMode::Research
            || task.category == TaskCategory::Research
            || JUDGMENT_HARNESSES.contains(&harness)
            || contains_any(&text, JUDGMENT_WORDS)
        {
            Level::High
        } else if ROUTINE_HARNESSES.contains(&harness) || mechanical {
            Level::Low
        } else {
            Level::Medium
        };

        let ambiguity = if exploratory || (!has_command && !has_artifact && objective_len < 80) {
            Level::High
        } else if has_command && (mechanical || mentions_path(&text)) {
            Level::Low
        } else {
            Level::Medium
        };

        let verifiability = if has_command && !has_human {
            Level::High
        } else if !has_command && !has_artifact {
            Level::Low
        } else {
            Level::Medium
        };

        let reversibility = if remote
            || task.category == TaskCategory::Ops
            || contains_any(&text, IRREVERSIBLE_WORDS)
        {
            Level::Low
        } else if has_human {
            Level::Medium
        } else {
            Level::High
        };

        let consequence = if task.category == TaskCategory::Ops
            || task.priority >= 30
            || remote
            || contains_any(&text, HIGH_STAKES_WORDS)
        {
            Level::High
        } else if task.mode == TaskMode::Prototype
            || task.category == TaskCategory::Docs
            || ROUTINE_HARNESSES.contains(&harness)
        {
            Level::Low
        } else {
            Level::Medium
        };

        let context_size = if task.repos.len() >= 2 || objective_len > 4000 {
            Level::High
        } else if task.repos.len() <= 1 && objective_len < 400 {
            Level::Low
        } else {
            Level::Medium
        };

        let tool_intensity = if remote || (TOOL_HARNESSES.contains(&harness) && has_command) {
            Level::High
        } else if !has_command && !TOOL_HARNESSES.contains(&harness) {
            Level::Low
        } else {
            Level::Medium
        };

        let expected_length = if task.budget.max_wall_secs >= 3600 || task.budget.max_turns >= 60 {
            Level::High
        } else if task.budget.max_wall_secs <= 600 && task.budget.max_turns <= 10 {
            Level::Low
        } else {
            Level::Medium
        };

        let cross_cutting = if task.repos.len() >= 2
            || task.skills.len() >= 4
            || contains_any(&text, CROSS_CUTTING_WORDS)
        {
            Level::High
        } else if task.skills.len() <= 1 {
            Level::Low
        } else {
            Level::Medium
        };

        TaskFeatures {
            judgment,
            ambiguity,
            verifiability,
            reversibility,
            consequence,
            context_size,
            tool_intensity,
            expected_length,
            cross_cutting,
        }
    }

    /// `infer` に明示の上書きを重ねる。戻り値の 2 つ目は上書きした軸の名前。
    pub fn infer_with_hints(
        task: &Task,
        hints: Option<&TaskFeatureHints>,
    ) -> (TaskFeatures, Vec<&'static str>) {
        let mut features = TaskFeatures::infer(task);
        let overridden = hints.map(|h| h.apply(&mut features)).unwrap_or_default();
        (features, overridden)
    }
}

/// lane の順位（cheap < standard < frontier）。
pub fn lane_rank(lane: Tier) -> u8 {
    match lane {
        Tier::Cheap => 0,
        Tier::Standard => 1,
        Tier::Frontier => 2,
    }
}

/// 1 段上の lane（frontier の上は無い）。
pub fn lane_up(lane: Tier) -> Option<Tier> {
    match lane {
        Tier::Cheap => Some(Tier::Standard),
        Tier::Standard => Some(Tier::Frontier),
        Tier::Frontier => None,
    }
}

/// 低い順の全 lane。
pub const LANES: [Tier; 3] = [Tier::Cheap, Tier::Standard, Tier::Frontier];

fn lane_name(lane: Tier) -> &'static str {
    match lane {
        Tier::Cheap => "cheap",
        Tier::Standard => "standard",
        Tier::Frontier => "frontier",
    }
}

/// ADR-0069 D2: 組織（実効 profile）から継いだ lane の天井。`allowed` が空なら集合の制限なし。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LaneCeiling {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<Tier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lane: Option<Tier>,
}

impl LaneCeiling {
    pub fn permits(&self, lane: Tier) -> bool {
        (self.allowed.is_empty() || self.allowed.contains(&lane))
            && self
                .max_lane
                .is_none_or(|max| lane_rank(lane) <= lane_rank(max))
    }

    /// 許される最も高い lane（何も許されないなら `None`）。
    pub fn top(&self) -> Option<Tier> {
        LANES.iter().rev().copied().find(|l| self.permits(*l))
    }

    /// 天井に丸める: 許されていればそのまま。そうでなければ**下側で最も近い** lane、無ければ上側で
    /// 最も近い lane。丸めたときだけ理由を返す。何も許されない天井（設定の矛盾）なら元の lane のまま
    /// 理由だけ返す。
    pub fn clamp(&self, lane: Tier) -> (Tier, Option<String>) {
        if self.permits(lane) {
            return (lane, None);
        }
        let describe = || {
            let allowed: Vec<&str> = self.allowed.iter().map(|t| lane_name(*t)).collect();
            format!(
                "org ceiling (allowed_tiers=[{}], max_lane={})",
                allowed.join(","),
                self.max_lane.map(lane_name).unwrap_or("none")
            )
        };
        let below = LANES
            .iter()
            .rev()
            .copied()
            .find(|l| lane_rank(*l) < lane_rank(lane) && self.permits(*l));
        let above = LANES
            .iter()
            .copied()
            .find(|l| lane_rank(*l) > lane_rank(lane) && self.permits(*l));
        match below.or(above) {
            Some(to) => (
                to,
                Some(format!(
                    "{}: {} -> {}",
                    describe(),
                    lane_name(lane),
                    lane_name(to)
                )),
            ),
            None => (
                lane,
                Some(format!(
                    "{}: no lane satisfies the ceiling; kept {}",
                    describe(),
                    lane_name(lane)
                )),
            ),
        }
    }
}

/// ADR-0069 §5（Phase 2 の予約）: shadow mode の分類器（例: Jev）の判断。lane は heuristic のままで、
/// これは並べて記録するだけ。**Phase 1 では作られない**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShadowDecision {
    pub classifier: String,
    pub lane: Tier,
    /// 0.0..=1.0。
    pub confidence: f64,
}

/// ADR-0069 §5（Phase 2 の予約）: shadow 分類器の境界。**実装は無い**（Phase 2）。
pub trait ShadowClassifier: Send + Sync {
    fn id(&self) -> &str;
    fn classify(&self, task: &Task, features: &TaskFeatures) -> Option<ShadowDecision>;
}

/// ADR-0069 D3: lane の決定（監査記録の本体）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LaneDecision {
    /// 最終的な lane（天井・エスカレーションの後。残量による調整の前）。
    pub lane: Tier,
    /// 規則表が出した lane（天井・エスカレーションの前）。
    pub proposed: Tier,
    pub source: TierSource,
    pub rule_id: String,
    pub policy_version: String,
    pub features: TaskFeatures,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clamped_by: Option<String>,
    /// LLM が書いた tier（`TierSource::Hint`。記録するだけ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<Tier>,
    /// リトライでのエスカレーションの判断（`retry_policy`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<String>,
    /// Phase 2 の予約（shadow 分類器）。Phase 1 では常に `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<ShadowDecision>,
}

/// ADR-0069 D5: `Event::RoutingDecided` の中身。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RoutingRecord {
    /// Ownership 層: 担当（`org_nodes.id`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_node: Option<String>,
    /// Harness 層: ハーネス id（`Task.genre`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Model 層（lane）。
    pub decision: LaneDecision,
    /// Model 層（lane → provider / model）。
    pub resolution: crate::model_routing::LaneResolution,
    /// 残量による調整の理由（`select_tier`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_reason: Option<String>,
    /// ADR-0072 D21（Phase E2）: この run が属する WorkUnit（計画のある Task の WU の run だけ。
    /// 暗黙の WorkUnit・導入前のイベントには無い）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit_id: Option<String>,
}

/// 規則 1 件。`when` が真なら `lane`。
struct LaneRule {
    id: &'static str,
    lane: Tier,
    condition: &'static str,
    when: fn(&TaskFeatures) -> bool,
}

/// ADR-0069 D3 の規則表（上から評価する。最後の `standard/default` は必ず当たる）。
const RULES: &[LaneRule] = &[
    LaneRule {
        id: "frontier/judgment-under-uncertainty",
        lane: Tier::Frontier,
        condition: "judgment=high and (ambiguity=high or verifiability=low)",
        when: |f| {
            f.judgment == Level::High
                && (f.ambiguity == Level::High || f.verifiability == Level::Low)
        },
    },
    LaneRule {
        id: "frontier/costly-and-unverifiable",
        lane: Tier::Frontier,
        condition: "consequence=high and verifiability=low",
        when: |f| f.consequence == Level::High && f.verifiability == Level::Low,
    },
    LaneRule {
        id: "frontier/broad-judgment",
        lane: Tier::Frontier,
        condition: "judgment=high and cross_cutting=high",
        when: |f| f.judgment == Level::High && f.cross_cutting == Level::High,
    },
    LaneRule {
        id: "cheap/mechanical-verifiable-reversible",
        lane: Tier::Cheap,
        condition: "judgment=low and ambiguity=low and verifiability=high and reversibility=high and consequence!=high",
        when: |f| {
            f.judgment == Level::Low
                && f.ambiguity == Level::Low
                && f.verifiability == Level::High
                && f.reversibility == Level::High
                && f.consequence != Level::High
        },
    },
    LaneRule {
        id: "standard/default",
        lane: Tier::Standard,
        condition: "no frontier/cheap rule matched",
        when: |_| true,
    },
];

/// ADR-0069 D3: 決定的な lane policy。
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelPolicy;

impl ModelPolicy {
    /// 規則表を上から当て、天井で丸める。`source` は `Default`（呼び出し側が上書きする）。
    pub fn decide(&self, features: &TaskFeatures, ceiling: &LaneCeiling) -> LaneDecision {
        let (id, proposed, condition) = RULES
            .iter()
            .find(|r| (r.when)(features))
            .map(|r| (r.id, r.lane, r.condition))
            .unwrap_or(("standard/default", Tier::Standard, "no rule matched"));
        let (lane, clamped_by) = ceiling.clamp(proposed);
        let mut reasons = vec![format!("{id}: {condition}")];
        if let Some(c) = &clamped_by {
            reasons.push(c.clone());
        }
        LaneDecision {
            lane,
            proposed,
            source: TierSource::Default,
            rule_id: id.to_string(),
            policy_version: LANE_POLICY_VERSION.to_string(),
            features: *features,
            reasons,
            clamped_by,
            hint: None,
            escalation: None,
            shadow: None,
        }
    }
}

/// ADR-0069 D3: そのタスクの lane を決める。`routing` を持たないタスク・execute でないタスクは
/// `None`（従来どおり `worker_hint.tier` のまま）。人の明示・System の tier は policy も天井も当てず、
/// その旨を記録するだけ。
pub fn decide_for_task(task: &Task, ceiling: &LaneCeiling) -> Option<LaneDecision> {
    let routing = task.routing.as_ref()?;
    if task.kind != TaskKind::Execute {
        return None;
    }
    let (features, overridden) = TaskFeatures::infer_with_hints(task, routing.features.as_ref());
    if !routing.tier_source.policy_decides() {
        let (rule_id, why) = match routing.tier_source {
            TierSource::Human => (
                "explicit/human",
                "tier explicitly set by a human; policy and org ceiling not applied",
            ),
            _ => (
                "explicit/system",
                "tier fixed by celeris code; policy not applied",
            ),
        };
        return Some(LaneDecision {
            lane: task.worker_hint.tier,
            proposed: task.worker_hint.tier,
            source: routing.tier_source,
            rule_id: rule_id.to_string(),
            policy_version: LANE_POLICY_VERSION.to_string(),
            features,
            reasons: vec![why.to_string()],
            clamped_by: None,
            hint: None,
            escalation: None,
            shadow: None,
        });
    }
    let mut decision = ModelPolicy.decide(&features, ceiling);
    decision.source = routing.tier_source;
    if routing.tier_source == TierSource::Hint {
        decision.hint = Some(task.worker_hint.tier);
        decision.reasons.push(format!(
            "LLM-supplied tier {} recorded as a hint only",
            lane_name(task.worker_hint.tier)
        ));
    }
    if !overridden.is_empty() {
        decision
            .reasons
            .push(format!("features overridden: {}", overridden.join(", ")));
    }
    Some(decision)
}

/// ADR-0072 D21（Phase E3）: Task を複製し、`objective`/`acceptance`/`budget`/`genre` を WorkUnit の
/// spec に差し替えた「WU の view」を作る（`routing`・`kind` 等はそのまま Task のものを継ぐ）。
/// `TaskFeatures::infer` を WU 単位で計算するための入力を組み立てるだけの純粋関数。
fn work_unit_view(task: &Task, wu: &crate::execution_plan::WorkUnitRow) -> Task {
    let mut view = task.clone();
    view.objective = wu.spec.objective.clone();
    view.acceptance = if wu.spec.checks.is_empty() {
        wu.spec
            .done_when
            .iter()
            .map(|text| crate::model::Criterion {
                text: text.clone(),
                check: Check::Human,
            })
            .collect()
    } else {
        wu.spec
            .checks
            .iter()
            .map(|c| crate::model::Criterion {
                text: format!("check: {}", c.cmd),
                check: Check::Command {
                    cmd: c.cmd.clone(),
                    expect_exit: c.expect_exit,
                },
            })
            .collect()
    };
    // ADR-0072 D18: WU の予算の既定は `max(task.budget.max_turns, 30)` / `max(task.budget.max_wall_secs,
    // 1800)`（planner が書かなかったとき）。
    view.budget.max_turns = wu
        .spec
        .budget
        .and_then(|b| b.max_turns)
        .unwrap_or_else(|| task.budget.max_turns.max(30));
    view.budget.max_wall_secs = wu
        .spec
        .budget
        .and_then(|b| b.max_wall_secs)
        .unwrap_or_else(|| task.budget.max_wall_secs.max(1800));
    if let Some(harness) = &wu.spec.harness {
        view.genre = Some(harness.clone());
    }
    view
}

/// ADR-0074 D5.2（Phase F1）: `[execution] work_unit_lane_cap`。既定 `Task`（WU の lane は
/// `max(Task の lane, standard)` を超えない）。`None` は上限を掛けない（デバッグ・実験用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkUnitLaneCap {
    #[default]
    Task,
    None,
}

impl WorkUnitLaneCap {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkUnitLaneCap::Task => "task",
            WorkUnitLaneCap::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "task" => Some(WorkUnitLaneCap::Task),
            "none" => Some(WorkUnitLaneCap::None),
            _ => None,
        }
    }
}

/// ADR-0072 D21（Phase E3）/ ADR-0074 D5.1/D5.2（Phase F1）: WU の `harness`（無ければ Task の genre）と
/// WU の `features` の上書きで、その WU の lane を決める（`RoutingDecided.record.work_unit_id` に残す
/// ための `LaneDecision`）。`decide_for_task` と同じ規則（Task が `routing` を持たない・execute で
/// なければ `None`）。
///
/// `task_lane`: D5.2 の上限に使う「Task の lane」（呼び出し側が Task 自身の `decide_for_task` から
/// 渡す）。`work_unit_lane_cap = "task"` のときだけ `Some` を渡す。WU の lane が
/// `max(task_lane, standard)` を超えていれば standard 側に丸め、`clamped_by` に理由を残す（下げる
/// 方向 = cheap は制限しない）。
pub fn decide_for_work_unit(
    task: &Task,
    wu: &crate::execution_plan::WorkUnitRow,
    ceiling: &LaneCeiling,
    task_lane: Option<Tier>,
) -> Option<LaneDecision> {
    let mut view = work_unit_view(task, wu);
    // D21: WU の `features` があれば、Task の `routing.features` の代わりにそれを使う（WU 単位の
    // 上書き）。無ければ Task の hints をそのまま継ぐ。読めない値は寛容に無視する（v1 の保存済み
    // 計画をそのまま読めるようにするため。読めるかどうかの検証は `execution_plan::validate` が
    // 採用時に行う。ADR-0074 D5.1）。
    let wu_hints: Option<TaskFeatureHints> = wu
        .spec
        .features
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    if let (Some(routing), Some(hints)) = (&mut view.routing, wu_hints) {
        routing.features = Some(hints);
    }
    let mut decision = decide_for_task(&view, ceiling)?;
    if let Some(task_lane) = task_lane {
        let cap = if lane_rank(task_lane) > lane_rank(Tier::Standard) {
            task_lane
        } else {
            Tier::Standard
        };
        if lane_rank(decision.lane) > lane_rank(cap) {
            let note = format!(
                "work-unit lane cap (task lane {}): {} -> {}",
                lane_name(task_lane),
                lane_name(decision.lane),
                lane_name(cap)
            );
            decision.clamped_by = Some(match decision.clamped_by.take() {
                Some(prev) => format!("{prev}; {note}"),
                None => note.clone(),
            });
            decision.reasons.push(note);
            decision.lane = cap;
        }
    }
    // ADR-0074 D5.1: 5 軸のどれかが欠けていれば「WU の features は部分上書きだった」と残す
    // （`wu_hints` は `TaskFeatureHints: Copy` なので上の代入の後もそのまま読める）。
    if let Some(hints) = &wu_hints {
        let complete = hints.judgment.is_some()
            && hints.ambiguity.is_some()
            && hints.verifiability.is_some()
            && hints.reversibility.is_some()
            && hints.consequence.is_some();
        if !complete {
            decision
                .reasons
                .push("features_source: work_unit(partial)".to_string());
        }
    }
    Some(decision)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::{Budget, Criterion, Status, TaskId, TaskRouting, WorkerHint};

    fn f(
        judgment: Level,
        ambiguity: Level,
        verifiability: Level,
        reversibility: Level,
        consequence: Level,
    ) -> TaskFeatures {
        TaskFeatures {
            judgment,
            ambiguity,
            verifiability,
            reversibility,
            consequence,
            context_size: Level::Low,
            tool_intensity: Level::Medium,
            expected_length: Level::Low,
            cross_cutting: Level::Low,
        }
    }

    pub(crate) fn task(objective: &str, acceptance: Vec<Criterion>) -> Task {
        let now = time::OffsetDateTime::UNIX_EPOCH;
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: objective.into(),
            acceptance,
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: 10,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::local("/tmp/x"),
            repos: vec![],
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
            genre: Some("coding".into()),
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            labels: vec![],
            category: TaskCategory::Other,
            skills: vec![],
            mode: TaskMode::Production,
            conversation: None,
            routing: Some(TaskRouting::default()),
        }
    }

    fn cmd() -> Criterion {
        Criterion {
            text: "tests pass".into(),
            check: Check::Command {
                cmd: "cargo test".into(),
                expect_exit: 0,
            },
        }
    }

    fn reviewer() -> Criterion {
        Criterion {
            text: "looks right".into(),
            check: Check::Reviewer,
        }
    }

    /// 表駆動: 3 つの基本ケースと境界の組み合わせ。
    #[test]
    fn rule_table_maps_features_to_lanes() {
        use Level::*;
        let open = LaneCeiling::default();
        let cases: &[(TaskFeatures, Tier, &str)] = &[
            // 基本 1: 機械的 + 検証しやすい + 戻せる → cheap
            (
                f(Low, Low, High, High, Low),
                Tier::Cheap,
                "cheap/mechanical-verifiable-reversible",
            ),
            // 基本 2: 判断が重い + 曖昧 → frontier
            (
                f(High, High, Medium, High, Medium),
                Tier::Frontier,
                "frontier/judgment-under-uncertainty",
            ),
            // 基本 3: どちらでもない → standard
            (
                f(Medium, Medium, Medium, High, Medium),
                Tier::Standard,
                "standard/default",
            ),
            // 判断が重く、検証できない → frontier
            (
                f(High, Low, Low, High, Low),
                Tier::Frontier,
                "frontier/judgment-under-uncertainty",
            ),
            // 損失が大きく検証できない → frontier（判断は中）
            (
                f(Medium, Medium, Low, Medium, High),
                Tier::Frontier,
                "frontier/costly-and-unverifiable",
            ),
            // 機械的だが損失が大きい → cheap にしない
            (
                f(Low, Low, High, High, High),
                Tier::Standard,
                "standard/default",
            ),
            // 機械的だが戻せない → cheap にしない
            (
                f(Low, Low, High, Low, Low),
                Tier::Standard,
                "standard/default",
            ),
            // 判断が重いが検証でき、曖昧でもない → standard（単一スコアなら frontier 寄りになり得る）
            (
                f(High, Low, High, High, Medium),
                Tier::Standard,
                "standard/default",
            ),
        ];
        for (features, lane, rule) in cases {
            let d = ModelPolicy.decide(features, &open);
            assert_eq!(d.lane, *lane, "{features:?}");
            assert_eq!(d.rule_id, *rule, "{features:?}");
            assert_eq!(d.policy_version, LANE_POLICY_VERSION);
            assert!(d.clamped_by.is_none());
        }
        // 判断が重く横断的 → frontier/broad-judgment
        let mut broad = f(High, Low, High, High, Medium);
        broad.cross_cutting = High;
        assert_eq!(
            ModelPolicy.decide(&broad, &open).rule_id,
            "frontier/broad-judgment"
        );
    }

    #[test]
    fn org_ceiling_clamps_the_lane_and_records_why() {
        use Level::*;
        let hard = f(High, High, Low, Low, High);
        let ceiling = LaneCeiling {
            allowed: vec![Tier::Standard, Tier::Cheap],
            max_lane: None,
        };
        let d = ModelPolicy.decide(&hard, &ceiling);
        assert_eq!(d.proposed, Tier::Frontier);
        assert_eq!(d.lane, Tier::Standard);
        assert!(
            d.clamped_by
                .as_deref()
                .is_some_and(|c| c.contains("frontier -> standard"))
        );
        // max_lane は allowed より厳しければ勝つ
        let ceiling = LaneCeiling {
            allowed: vec![],
            max_lane: Some(Tier::Cheap),
        };
        assert_eq!(ModelPolicy.decide(&hard, &ceiling).lane, Tier::Cheap);
        // 下に許される lane が無ければ上の最も近いもの
        let ceiling = LaneCeiling {
            allowed: vec![Tier::Frontier],
            max_lane: None,
        };
        let easy = f(Low, Low, High, High, Low);
        assert_eq!(ModelPolicy.decide(&easy, &ceiling).lane, Tier::Frontier);
        assert_eq!(ceiling.top(), Some(Tier::Frontier));
    }

    #[test]
    fn infer_is_deterministic_and_reads_the_task() {
        let mechanical = task("crates/task-core/src/model.rs の typo を直す", vec![cmd()]);
        let d = decide_for_task(&mechanical, &LaneCeiling::default()).unwrap();
        assert_eq!(d.lane, Tier::Cheap, "{:?}", d.features);

        let mut design = task(
            "ルーティングの方針を設計し、トレードオフを比較検討する",
            vec![reviewer()],
        );
        design.genre = Some("writing".into());
        let d = decide_for_task(&design, &LaneCeiling::default()).unwrap();
        assert_eq!(d.lane, Tier::Frontier, "{:?}", d.features);

        let normal = task(
            "API に新しいエンドポイントを足してテストを書く。既存のハンドラと同じ形にする。",
            vec![cmd(), reviewer()],
        );
        let d = decide_for_task(&normal, &LaneCeiling::default()).unwrap();
        assert_eq!(d.lane, Tier::Standard, "{:?}", d.features);
        assert_eq!(TaskFeatures::infer(&normal), TaskFeatures::infer(&normal));
    }

    #[test]
    fn hints_override_axes_and_explicit_tiers_skip_the_policy() {
        let mut t = task("crates/x.rs の typo を直す", vec![cmd()]);
        t.routing = Some(TaskRouting {
            tier_source: TierSource::Hint,
            features: Some(TaskFeatureHints {
                judgment: Some(Level::High),
                ambiguity: Some(Level::High),
                ..TaskFeatureHints::default()
            }),
            ..TaskRouting::default()
        });
        t.worker_hint.tier = Tier::Cheap;
        let d = decide_for_task(&t, &LaneCeiling::default()).unwrap();
        assert_eq!(d.lane, Tier::Frontier);
        assert_eq!(d.hint, Some(Tier::Cheap));
        assert!(d.reasons.iter().any(|r| r.contains("judgment, ambiguity")));

        // 人の明示は policy も天井も当てない
        t.routing = Some(TaskRouting {
            tier_source: TierSource::Human,
            ..TaskRouting::default()
        });
        let ceiling = LaneCeiling {
            allowed: vec![Tier::Frontier],
            max_lane: None,
        };
        let d = decide_for_task(&t, &ceiling).unwrap();
        assert_eq!(
            (d.lane, d.rule_id.as_str()),
            (Tier::Cheap, "explicit/human")
        );

        // routing の無い既存タスク・execute 以外は対象外
        t.routing = None;
        assert!(decide_for_task(&t, &ceiling).is_none());
        t.routing = Some(TaskRouting::default());
        t.kind = TaskKind::Plan;
        assert!(decide_for_task(&t, &ceiling).is_none());
    }

    #[test]
    fn decision_serializes_with_the_audit_fields_and_old_tasks_still_parse() {
        let t = task("x", vec![cmd()]);
        let d = decide_for_task(&t, &LaneCeiling::default()).unwrap();
        let v = serde_json::to_value(&d).unwrap();
        for key in ["lane", "rule_id", "policy_version", "features", "reasons"] {
            assert!(v.get(key).is_some(), "{key} missing in {v}");
        }
        assert!(v.get("shadow").is_none());
        // 導入前のタスク（`routing` 無し）はそのまま読める
        let mut old = serde_json::to_value(&t).unwrap();
        old.as_object_mut().unwrap().remove("routing");
        let back: Task = serde_json::from_value(old).unwrap();
        assert!(back.routing.is_none());
    }

    // ---- ADR-0074 D5.1/D5.2（Phase F1）: WU ごとの features と lane の上限 ----

    fn wu_row(features: Option<serde_json::Value>, checks: Vec<Criterion>) -> crate::WorkUnitRow {
        use crate::execution_plan::{
            WorkUnitCheck, WorkUnitContext, WorkUnitKind, WorkUnitSpec, WorkUnitStatus,
        };
        let spec = WorkUnitSpec {
            key: "a".to_string(),
            kind: WorkUnitKind::Implement,
            title: "a".to_string(),
            objective: "do a".to_string(),
            depends_on: vec![],
            done_when: vec![],
            checks: checks
                .into_iter()
                .filter_map(|c| match c.check {
                    Check::Command { cmd, expect_exit } => Some(WorkUnitCheck { cmd, expect_exit }),
                    _ => None,
                })
                .collect(),
            context: WorkUnitContext::default(),
            harness: None,
            features,
            budget: None,
            outputs: vec![],
        };
        crate::WorkUnitRow::new(
            "wu-a".to_string(),
            "task".to_string(),
            "plan".to_string(),
            0,
            spec,
            WorkUnitStatus::Ready,
            "2026-09-26T00:00:00Z".to_string(),
        )
    }

    /// ADR-0074 §6 F1 (c): judgment=low/ambiguity=low/verifiability=high/reversibility=high と
    /// `checks` を持つ WU は cheap になる。
    #[test]
    fn work_unit_features_lower_a_mechanical_unit_to_cheap() {
        let t = task(
            "設計方針を比較検討する（Task 自体は判断が重い）",
            vec![reviewer()],
        );
        let wu = wu_row(
            Some(serde_json::json!({
                "judgment": "low", "ambiguity": "low", "verifiability": "high",
                "reversibility": "high", "consequence": "low"
            })),
            vec![cmd()],
        );
        let d = decide_for_work_unit(&t, &wu, &LaneCeiling::default(), None)
            .expect("decision for execute task");
        assert_eq!(d.lane, Tier::Cheap, "{d:?}");
    }

    /// features が無い WU は、Task の `routing.features`（明示のヒント）をそのまま継ぐ
    /// （WU 自身の `features` が上書きするのは、WU にそれがあるときだけ）。
    #[test]
    fn work_unit_without_features_inherits_the_task_hints() {
        let mut t = task("do a", vec![cmd()]);
        t.routing = Some(TaskRouting {
            features: Some(TaskFeatureHints {
                judgment: Some(Level::High),
                ambiguity: Some(Level::High),
                ..TaskFeatureHints::default()
            }),
            ..TaskRouting::default()
        });
        let wu = wu_row(None, vec![cmd()]);
        let d = decide_for_work_unit(&t, &wu, &LaneCeiling::default(), None).unwrap();
        assert_eq!(d.lane, Tier::Frontier, "{d:?}");
    }

    /// (c): Task が standard のとき、WU の features が frontier を示しても standard に丸まり、
    /// `clamped_by` に理由が残る（D5.2 の上限 = max(Task の lane, standard)）。
    #[test]
    fn work_unit_lane_is_capped_by_the_task_lane() {
        let t = task(
            "API に新しいエンドポイントを足してテストを書く。既存のハンドラと同じ形にする。",
            vec![cmd(), reviewer()],
        );
        // Task 自身は standard（判断も曖昧さも中程度）。
        let task_decision = decide_for_task(&t, &LaneCeiling::default()).unwrap();
        assert_eq!(task_decision.lane, Tier::Standard, "{task_decision:?}");

        let wu = wu_row(
            Some(serde_json::json!({
                "judgment": "high", "ambiguity": "high", "verifiability": "medium",
                "reversibility": "high", "consequence": "medium"
            })),
            vec![],
        );
        // 上限を掛けない（`work_unit_lane_cap = "none"` に相当）: そのまま frontier。
        let uncapped = decide_for_work_unit(&t, &wu, &LaneCeiling::default(), None).unwrap();
        assert_eq!(uncapped.lane, Tier::Frontier, "{uncapped:?}");
        assert!(uncapped.clamped_by.is_none());

        // 上限を掛ける（既定 `work_unit_lane_cap = "task"`）: standard に丸まる。
        let capped =
            decide_for_work_unit(&t, &wu, &LaneCeiling::default(), Some(task_decision.lane))
                .unwrap();
        assert_eq!(capped.lane, Tier::Standard, "{capped:?}");
        assert!(
            capped
                .clamped_by
                .as_deref()
                .is_some_and(|c| c.contains("work-unit lane cap")),
            "{capped:?}"
        );
    }

    /// D5.2: 上限は「下げる」方向には効かない。Task が standard でも、WU の features が cheap を
    /// 示せば cheap のまま。
    #[test]
    fn work_unit_lane_cap_does_not_prevent_routing_cheaper_than_the_task() {
        let t = task(
            "API に新しいエンドポイントを足してテストを書く。既存のハンドラと同じ形にする。",
            vec![cmd(), reviewer()],
        );
        let task_decision = decide_for_task(&t, &LaneCeiling::default()).unwrap();
        assert_eq!(task_decision.lane, Tier::Standard, "{task_decision:?}");
        let wu = wu_row(
            Some(serde_json::json!({
                "judgment": "low", "ambiguity": "low", "verifiability": "high",
                "reversibility": "high", "consequence": "low"
            })),
            vec![cmd()],
        );
        let d = decide_for_work_unit(&t, &wu, &LaneCeiling::default(), Some(task_decision.lane))
            .unwrap();
        assert_eq!(d.lane, Tier::Cheap, "{d:?}");
    }

    /// Task 自身が frontier のとき、WU の上限は `max(frontier, standard) = frontier` になる
    /// （frontier の WU をさらに下げない）。
    #[test]
    fn work_unit_lane_cap_follows_a_frontier_task() {
        let mut t = task(
            "ルーティングの方針を設計し、トレードオフを比較検討する",
            vec![reviewer()],
        );
        t.genre = Some("writing".into());
        let task_decision = decide_for_task(&t, &LaneCeiling::default()).unwrap();
        assert_eq!(task_decision.lane, Tier::Frontier, "{task_decision:?}");
        let wu = wu_row(
            Some(serde_json::json!({
                "judgment": "high", "ambiguity": "high", "verifiability": "low",
                "reversibility": "high", "consequence": "medium"
            })),
            vec![],
        );
        let d = decide_for_work_unit(&t, &wu, &LaneCeiling::default(), Some(task_decision.lane))
            .unwrap();
        assert_eq!(d.lane, Tier::Frontier, "{d:?}");
    }

    /// D5.1: 5 軸のうち一部だけを書いた WU は拒否されず（`execution_plan::validate` の役目とは別）、
    /// `reasons` に `features_source: work_unit(partial)` が残る。
    #[test]
    fn partial_work_unit_features_are_noted_as_partial() {
        let t = task("x", vec![cmd()]);
        let wu = wu_row(Some(serde_json::json!({"judgment": "low"})), vec![cmd()]);
        let d = decide_for_work_unit(&t, &wu, &LaneCeiling::default(), None).unwrap();
        assert!(
            d.reasons
                .iter()
                .any(|r| r == "features_source: work_unit(partial)"),
            "{d:?}"
        );
    }
}
