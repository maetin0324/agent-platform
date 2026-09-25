//! ADR-0072（Task execution decomposition）Phase E2: ExecutionPlan / WorkUnit のデータモデルと
//! 決定的な scheduler（D5・D14・D15）。
//!
//! 純粋なデータ定義と純粋関数だけを置く（I/O・LLM 呼び出しはしない。ADR-0001 D2）。永続化（`execution_plans`
//! / `work_units` / `runs` の 3 表。D5）は `task_core::store` が行う。

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// D14: 計画 JSON の schema 版（`docs/protocol/execution-plan.schema.json`）。
pub const EXECUTION_PLAN_SCHEMA: &str = "celeris.execution-plan/1";

/// `execution_plans.id` / `work_units.id` に使う ULID の発行（`TaskId` 等と同じ ULID 系を使う。
/// 型付きの id にしていないのは、この 2 表が対応する Event の中に既に `plan_id` / `work_unit_id` が
/// 文字列で入っているため。I/O は無い純粋な採番）。
pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

// ---------------------------------------------------------------------------
// D14: Planner が出す（または人が書く）計画の形
// ---------------------------------------------------------------------------

/// D14: WorkUnit の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkUnitKind {
    Investigate,
    Design,
    Implement,
    Test,
    Release,
    Repair,
    Other,
}

impl WorkUnitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkUnitKind::Investigate => "investigate",
            WorkUnitKind::Design => "design",
            WorkUnitKind::Implement => "implement",
            WorkUnitKind::Test => "test",
            WorkUnitKind::Release => "release",
            WorkUnitKind::Repair => "repair",
            WorkUnitKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "investigate" => Some(WorkUnitKind::Investigate),
            "design" => Some(WorkUnitKind::Design),
            "implement" => Some(WorkUnitKind::Implement),
            "test" => Some(WorkUnitKind::Test),
            "release" => Some(WorkUnitKind::Release),
            "repair" => Some(WorkUnitKind::Repair),
            "other" => Some(WorkUnitKind::Other),
            _ => None,
        }
    }
}

/// D14: `checks` は決定的な検査だけ（`Command`。E4 で実行する。E2 は schema と検証のみ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkUnitCheck {
    pub cmd: String,
    #[serde(default)]
    pub expect_exit: i32,
}

/// D14: WU が読むべき context のヒント。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkUnitContext {
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub from_work_units: Vec<String>,
    #[serde(default)]
    pub knowledge: Vec<String>,
}

/// D14/D18: WU ごとの予算（任意。書かなければ D18 の既定を使う）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkUnitBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_secs: Option<u64>,
}

/// D14: 計画の中の 1 WorkUnit の spec。**`assignee` / `tier` / `model` の欄は持たない**
/// （`deny_unknown_fields` により、書かれていれば schema 違反になる。D14）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkUnitSpec {
    /// `[a-z0-9-]{1,32}`。計画の中で一意（D14）。
    pub key: String,
    pub kind: WorkUnitKind,
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub done_when: Vec<String>,
    #[serde(default)]
    pub checks: Vec<WorkUnitCheck>,
    #[serde(default)]
    pub context: WorkUnitContext,
    /// `[[genres]]` にある id だけを許す（D14。検証は担当の profile を知る呼び出し側が行う。
    /// ここでは形だけ見る）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// D21: `TaskFeatures` の上書きヒント（任意の JSON。E3 以降の routing が読む）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<WorkUnitBudget>,
    #[serde(default)]
    pub outputs: Vec<String>,
}

/// D14: Planner の出力（または人が `PUT`/`POST` で書く計画）そのもの。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlanSpec {
    pub schema: String,
    pub rationale: String,
    pub work_units: Vec<WorkUnitSpec>,
}

/// 生成したスキーマ（`docs/protocol/execution-plan.schema.json`。`UPDATE_SCHEMA=1` で再生成）。
pub fn schema_value() -> serde_json::Value {
    let schema = schemars::schema_for!(ExecutionPlanSpec);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

// ---------------------------------------------------------------------------
// D14: 検証
// ---------------------------------------------------------------------------

/// D18: 検証・丸めに使う上限（既定値は ADR-0072 D18 の表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionLimits {
    pub max_work_units: usize,
    pub work_unit_max_turns: u32,
    pub work_unit_max_wall_secs: u64,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        ExecutionLimits {
            max_work_units: 8,
            work_unit_max_turns: 80,
            work_unit_max_wall_secs: 3600,
        }
    }
}

/// D14: 検証エラー（すべて拒否理由。1 回だけ再試行し、それでも駄目なら atomic に倒す。呼び出し側の責務）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanValidationError {
    /// `schema` 欄が `celeris.execution-plan/1` ではない。
    WrongSchema {
        found: String,
    },
    NoWorkUnits,
    TooManyWorkUnits {
        count: usize,
        max: usize,
    },
    InvalidKey {
        key: String,
    },
    DuplicateKey {
        key: String,
    },
    UnknownDependency {
        key: String,
        depends_on: String,
    },
    CyclicDependency {
        cycle: Vec<String>,
    },
    /// title の正規化一致、または objective のトークン Jaccard ≥ 0.9。
    DuplicateWorkUnit {
        a: String,
        b: String,
        reason: String,
    },
    /// replan（D14）: done の WU の key/spec が変わっている。E2 では replan を発行しないので、
    /// 呼び出し側が既存の done WU を渡したときだけ検査する。
    DoneWorkUnitChanged {
        key: String,
    },
}

impl std::fmt::Display for PlanValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanValidationError::WrongSchema { found } => {
                write!(f, "schema must be {EXECUTION_PLAN_SCHEMA}, found {found}")
            }
            PlanValidationError::NoWorkUnits => write!(f, "work_units must not be empty"),
            PlanValidationError::TooManyWorkUnits { count, max } => {
                write!(f, "too many work_units: {count} > {max}")
            }
            PlanValidationError::InvalidKey { key } => {
                write!(
                    f,
                    "invalid work unit key: {key:?} (must match [a-z0-9-]{{1,32}})"
                )
            }
            PlanValidationError::DuplicateKey { key } => {
                write!(f, "duplicate work unit key: {key}")
            }
            PlanValidationError::UnknownDependency { key, depends_on } => {
                write!(f, "work unit {key} depends on unknown key {depends_on}")
            }
            PlanValidationError::CyclicDependency { cycle } => {
                write!(f, "cyclic dependency: {}", cycle.join(" -> "))
            }
            PlanValidationError::DuplicateWorkUnit { a, b, reason } => {
                write!(f, "work units {a} and {b} look like duplicates ({reason})")
            }
            PlanValidationError::DoneWorkUnitChanged { key } => {
                write!(f, "done work unit {key} must not change on replan")
            }
        }
    }
}

impl std::error::Error for PlanValidationError {}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn normalize_title(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn token_set(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// D14: 検証を通った計画（budget を D18 の上限に丸めた写しと、丸めた記録）。
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedPlan {
    pub spec: ExecutionPlanSpec,
    /// 丸めたことの記録（`work unit <key>: max_turns 120 -> 80` のような 1 行ずつ）。
    pub rounding_notes: Vec<String>,
    /// トポロジカル順（`work_units` の index）。`work_units.seq` に使う。
    pub topological_order: Vec<usize>,
}

/// D14: 計画を検証する。`done_work_units`（replan で持ち越す既存の done WU の `(key, spec)`）が
/// 空でなければ、それらの key と spec が新しい計画でも変わっていないことを確かめる（E2 では常に空。
/// replan は E4）。
pub fn validate(
    spec: &ExecutionPlanSpec,
    limits: ExecutionLimits,
    done_work_units: &[(String, WorkUnitSpec)],
) -> Result<ValidatedPlan, Vec<PlanValidationError>> {
    let mut errors = Vec::new();

    if spec.schema != EXECUTION_PLAN_SCHEMA {
        errors.push(PlanValidationError::WrongSchema {
            found: spec.schema.clone(),
        });
    }
    if spec.work_units.is_empty() {
        errors.push(PlanValidationError::NoWorkUnits);
    }
    if spec.work_units.len() > limits.max_work_units {
        errors.push(PlanValidationError::TooManyWorkUnits {
            count: spec.work_units.len(),
            max: limits.max_work_units,
        });
    }

    let mut seen_keys: BTreeSet<&str> = BTreeSet::new();
    for wu in &spec.work_units {
        if !valid_key(&wu.key) {
            errors.push(PlanValidationError::InvalidKey {
                key: wu.key.clone(),
            });
            continue;
        }
        if !seen_keys.insert(wu.key.as_str()) {
            errors.push(PlanValidationError::DuplicateKey {
                key: wu.key.clone(),
            });
        }
    }

    let known_keys: BTreeSet<&str> = spec.work_units.iter().map(|w| w.key.as_str()).collect();
    for wu in &spec.work_units {
        for dep in &wu.depends_on {
            if !known_keys.contains(dep.as_str()) {
                errors.push(PlanValidationError::UnknownDependency {
                    key: wu.key.clone(),
                    depends_on: dep.clone(),
                });
            }
        }
    }

    // トポロジカルソート（循環の検出も兼ねる。Kahn's algorithm、決定的に `key` 昇順で tie-break）。
    let mut topological_order = Vec::new();
    if errors.is_empty() {
        match topo_sort(&spec.work_units) {
            Ok(order) => topological_order = order,
            Err(cycle) => errors.push(PlanValidationError::CyclicDependency { cycle }),
        }
    }

    // 重複の検出: title を正規化して一致、または objective のトークン Jaccard >= 0.9。
    for i in 0..spec.work_units.len() {
        for j in (i + 1)..spec.work_units.len() {
            let a = &spec.work_units[i];
            let b = &spec.work_units[j];
            if normalize_title(&a.title) == normalize_title(&b.title) && !a.title.is_empty() {
                errors.push(PlanValidationError::DuplicateWorkUnit {
                    a: a.key.clone(),
                    b: b.key.clone(),
                    reason: "identical normalized title".to_string(),
                });
                continue;
            }
            let sim = jaccard(&token_set(&a.objective), &token_set(&b.objective));
            if sim >= 0.9 {
                errors.push(PlanValidationError::DuplicateWorkUnit {
                    a: a.key.clone(),
                    b: b.key.clone(),
                    reason: format!("objective token overlap {sim:.2}"),
                });
            }
        }
    }

    // replan の不変条件: done の WU の key/spec は変わらない。
    let by_key: BTreeMap<&str, &WorkUnitSpec> = spec
        .work_units
        .iter()
        .map(|w| (w.key.as_str(), w))
        .collect();
    for (key, done_spec) in done_work_units {
        match by_key.get(key.as_str()) {
            Some(new_spec) if *new_spec == done_spec => {}
            _ => errors.push(PlanValidationError::DoneWorkUnitChanged { key: key.clone() }),
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    // D18: budget を丸める（丸めたことを記録する）。
    let mut rounded = spec.clone();
    let mut rounding_notes = Vec::new();
    for wu in &mut rounded.work_units {
        if let Some(budget) = &mut wu.budget {
            if let Some(turns) = budget.max_turns
                && turns > limits.work_unit_max_turns
            {
                rounding_notes.push(format!(
                    "work unit {}: max_turns {} -> {}",
                    wu.key, turns, limits.work_unit_max_turns
                ));
                budget.max_turns = Some(limits.work_unit_max_turns);
            }
            if let Some(wall) = budget.max_wall_secs
                && wall > limits.work_unit_max_wall_secs
            {
                rounding_notes.push(format!(
                    "work unit {}: max_wall_secs {} -> {}",
                    wu.key, wall, limits.work_unit_max_wall_secs
                ));
                budget.max_wall_secs = Some(limits.work_unit_max_wall_secs);
            }
        }
    }

    Ok(ValidatedPlan {
        spec: rounded,
        rounding_notes,
        topological_order,
    })
}

/// Kahn's algorithm。`Ok` はトポロジカル順（`work_units` の index。同順位は `key` 昇順）、
/// `Err` は見つかった循環（key の列）。
fn topo_sort(work_units: &[WorkUnitSpec]) -> Result<Vec<usize>, Vec<String>> {
    let index_of: BTreeMap<&str, usize> = work_units
        .iter()
        .enumerate()
        .map(|(i, w)| (w.key.as_str(), i))
        .collect();
    let mut in_degree: Vec<usize> = vec![0; work_units.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); work_units.len()];
    for (i, wu) in work_units.iter().enumerate() {
        for dep in &wu.depends_on {
            if let Some(&dep_i) = index_of.get(dep.as_str()) {
                dependents[dep_i].push(i);
                in_degree[i] += 1;
            }
        }
    }
    // 決定的に key 昇順で並べた候補集合を都度取り出す。
    let mut ready: BTreeSet<(&str, usize)> = work_units
        .iter()
        .enumerate()
        .filter(|(i, _)| in_degree[*i] == 0)
        .map(|(i, w)| (w.key.as_str(), i))
        .collect();
    let mut order = Vec::new();
    while let Some((_, i)) = ready.iter().next().copied() {
        ready.remove(&(work_units[i].key.as_str(), i));
        order.push(i);
        for &dep in &dependents[i] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 {
                ready.insert((work_units[dep].key.as_str(), dep));
            }
        }
    }
    if order.len() == work_units.len() {
        Ok(order)
    } else {
        let cycle: Vec<String> = (0..work_units.len())
            .filter(|i| !order.contains(i))
            .map(|i| work_units[i].key.clone())
            .collect();
        Err(cycle)
    }
}

// ---------------------------------------------------------------------------
// D6: WorkUnit / Run の状態
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkUnitStatus {
    Pending,
    Ready,
    NeedsContinuation,
    Running,
    Done,
    Failed,
    Blocked,
    Superseded,
    Cancelled,
}

impl WorkUnitStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkUnitStatus::Pending => "pending",
            WorkUnitStatus::Ready => "ready",
            WorkUnitStatus::NeedsContinuation => "needs_continuation",
            WorkUnitStatus::Running => "running",
            WorkUnitStatus::Done => "done",
            WorkUnitStatus::Failed => "failed",
            WorkUnitStatus::Blocked => "blocked",
            WorkUnitStatus::Superseded => "superseded",
            WorkUnitStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(WorkUnitStatus::Pending),
            "ready" => Some(WorkUnitStatus::Ready),
            "needs_continuation" => Some(WorkUnitStatus::NeedsContinuation),
            "running" => Some(WorkUnitStatus::Running),
            "done" => Some(WorkUnitStatus::Done),
            "failed" => Some(WorkUnitStatus::Failed),
            "blocked" => Some(WorkUnitStatus::Blocked),
            "superseded" => Some(WorkUnitStatus::Superseded),
            "cancelled" => Some(WorkUnitStatus::Cancelled),
            _ => None,
        }
    }

    /// この状態が「まだ計画の実行に関わる」か（superseded/cancelled は外れる）。
    pub fn is_active(self) -> bool {
        !matches!(self, WorkUnitStatus::Superseded | WorkUnitStatus::Cancelled)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            WorkUnitStatus::Done | WorkUnitStatus::Superseded | WorkUnitStatus::Cancelled
        )
    }
}

/// D6: `work_units.blocked_reason`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkUnitBlockedReason {
    Question,
    DependencyFailed,
    Limit,
    /// ADR-0072 D17 3.（Phase E4b 項目2）: worker の checkpoint/result.json が `plan_issue`
    /// （計画そのものが誤っているという 1 文の申告）を書いた。replan の余地があれば
    /// `Trigger::Continue{why: Replan}` で即座に Task を Ready へ戻す（Blocked のままにはしない）ので、
    /// この行が実際に `Task.status == Blocked` と一緒に残るのは replan の上限を使い切ったときだけ。
    PlanIssue,
}

impl WorkUnitBlockedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkUnitBlockedReason::Question => "question",
            WorkUnitBlockedReason::DependencyFailed => "dependency_failed",
            WorkUnitBlockedReason::Limit => "limit",
            WorkUnitBlockedReason::PlanIssue => "plan_issue",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "question" => Some(WorkUnitBlockedReason::Question),
            "dependency_failed" => Some(WorkUnitBlockedReason::DependencyFailed),
            "limit" => Some(WorkUnitBlockedReason::Limit),
            "plan_issue" => Some(WorkUnitBlockedReason::PlanIssue),
            _ => None,
        }
    }
}

/// D5: `execution_plans.origin`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanOrigin {
    Planner,
    Human,
    Repair,
    Fixture,
}

impl PlanOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            PlanOrigin::Planner => "planner",
            PlanOrigin::Human => "human",
            PlanOrigin::Repair => "repair",
            PlanOrigin::Fixture => "fixture",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "planner" => Some(PlanOrigin::Planner),
            "human" => Some(PlanOrigin::Human),
            "repair" => Some(PlanOrigin::Repair),
            "fixture" => Some(PlanOrigin::Fixture),
            _ => None,
        }
    }
}

/// D5: `execution_plans.status`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Active,
    Superseded,
    Completed,
    Abandoned,
}

impl PlanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PlanStatus::Active => "active",
            PlanStatus::Superseded => "superseded",
            PlanStatus::Completed => "completed",
            PlanStatus::Abandoned => "abandoned",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(PlanStatus::Active),
            "superseded" => Some(PlanStatus::Superseded),
            "completed" => Some(PlanStatus::Completed),
            "abandoned" => Some(PlanStatus::Abandoned),
            _ => None,
        }
    }
}

/// D5: `runs.role`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunIndexRole {
    Worker,
    Reviewer,
    Planner,
    WrapUp,
}

impl RunIndexRole {
    pub fn as_str(self) -> &'static str {
        match self {
            RunIndexRole::Worker => "worker",
            RunIndexRole::Reviewer => "reviewer",
            RunIndexRole::Planner => "planner",
            RunIndexRole::WrapUp => "wrap_up",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "worker" => Some(RunIndexRole::Worker),
            "reviewer" => Some(RunIndexRole::Reviewer),
            "planner" => Some(RunIndexRole::Planner),
            "wrap_up" => Some(RunIndexRole::WrapUp),
            _ => None,
        }
    }
}

/// D5: `runs.status`（Run の終わり方。`RunEnd` とほぼ対応するが `running` を持つ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunIndexStatus {
    Running,
    Completed,
    Yielded,
    BudgetExhausted,
    Question,
    Failed,
    HarnessError,
    Cancelled,
}

impl RunIndexStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunIndexStatus::Running => "running",
            RunIndexStatus::Completed => "completed",
            RunIndexStatus::Yielded => "yielded",
            RunIndexStatus::BudgetExhausted => "budget_exhausted",
            RunIndexStatus::Question => "question",
            RunIndexStatus::Failed => "failed",
            RunIndexStatus::HarnessError => "harness_error",
            RunIndexStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "running" => Some(RunIndexStatus::Running),
            "completed" => Some(RunIndexStatus::Completed),
            "yielded" => Some(RunIndexStatus::Yielded),
            "budget_exhausted" => Some(RunIndexStatus::BudgetExhausted),
            "question" => Some(RunIndexStatus::Question),
            "failed" => Some(RunIndexStatus::Failed),
            "harness_error" => Some(RunIndexStatus::HarnessError),
            "cancelled" => Some(RunIndexStatus::Cancelled),
            _ => None,
        }
    }

    /// `RunEnd`（D7）から `runs.status` へ。
    pub fn from_run_end(end: crate::execution::RunEnd) -> Self {
        use crate::execution::RunEnd;
        match end {
            RunEnd::Completed => RunIndexStatus::Completed,
            RunEnd::Yielded => RunIndexStatus::Yielded,
            RunEnd::BudgetExhausted { .. } => RunIndexStatus::BudgetExhausted,
            RunEnd::Question => RunIndexStatus::Question,
            RunEnd::Failed { .. } => RunIndexStatus::Failed,
            RunEnd::HarnessError { .. } => RunIndexStatus::HarnessError,
            RunEnd::Cancelled => RunIndexStatus::Cancelled,
        }
    }
}

// ---------------------------------------------------------------------------
// 派生の索引の行（D5）。永続化そのものは `task_core::store` が行う。
// ---------------------------------------------------------------------------

/// `execution_plans` の 1 行。
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionPlanRow {
    pub id: String,
    pub task_id: String,
    pub version: u32,
    pub origin: PlanOrigin,
    pub planner_run_id: Option<String>,
    pub status: PlanStatus,
    pub spec: ExecutionPlanSpec,
    pub created_at: String,
    pub superseded_at: Option<String>,
}

/// `work_units` の 1 行。
#[derive(Debug, Clone, PartialEq)]
pub struct WorkUnitRow {
    pub id: String,
    pub task_id: String,
    pub plan_id: String,
    pub key: String,
    pub seq: u32,
    pub kind: WorkUnitKind,
    pub status: WorkUnitStatus,
    pub blocked_reason: Option<WorkUnitBlockedReason>,
    pub depends_on: Vec<String>,
    pub runs: u32,
    pub continuations: u32,
    pub retries: u32,
    pub last_run_id: Option<String>,
    pub last_checkpoint_run_id: Option<String>,
    pub spec: WorkUnitSpec,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkUnitRow {
    pub fn new(
        id: String,
        task_id: String,
        plan_id: String,
        seq: u32,
        spec: WorkUnitSpec,
        status: WorkUnitStatus,
        created_at: String,
    ) -> Self {
        WorkUnitRow {
            id,
            task_id,
            plan_id,
            key: spec.key.clone(),
            seq,
            kind: spec.kind,
            status,
            blocked_reason: None,
            depends_on: spec.depends_on.clone(),
            runs: 0,
            continuations: 0,
            retries: 0,
            last_run_id: None,
            last_checkpoint_run_id: None,
            spec,
            created_at: created_at.clone(),
            updated_at: created_at,
        }
    }
}

/// `runs` の 1 行。
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub run_id: String,
    pub task_id: String,
    pub work_unit_id: Option<String>,
    pub role: RunIndexRole,
    pub seq: u32,
    pub status: RunIndexStatus,
    pub adapter: Option<String>,
    pub model: Option<String>,
    pub account: Option<String>,
    pub session_id: Option<String>,
    pub checkpoint: Option<crate::execution::Checkpoint>,
    pub usage: Option<crate::model::Usage>,
    pub metrics: Option<crate::model::RunMetrics>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

// ---------------------------------------------------------------------------
// D15: scheduler（決定的。ready queue・依存の伝播）
// ---------------------------------------------------------------------------

/// D15: 次に何をすべきか。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextStep {
    /// この WorkUnit（`work_units.id`）の Run を起こす。
    RunWorkUnit(String),
    /// Planner run を起こす（`replan` なら replan モード。E2 では発行しない）。
    RunPlanner {
        replan: bool,
    },
    AllDone,
    Stuck(String),
}

/// D15: `next_work_unit`。`needs_continuation` を優先し、次に `ready` を `seq` 順で選ぶ。
/// `units` は `superseded`/`cancelled` を含めてよい（無視する）。
pub fn next_work_unit(units: &[WorkUnitRow]) -> NextStep {
    let active: Vec<&WorkUnitRow> = units.iter().filter(|u| u.status.is_active()).collect();
    if active.is_empty() {
        return NextStep::Stuck("計画に有効な WorkUnit がありません".to_string());
    }
    if let Some(u) = active
        .iter()
        .filter(|u| u.status == WorkUnitStatus::NeedsContinuation)
        .min_by_key(|u| u.seq)
    {
        return NextStep::RunWorkUnit(u.id.clone());
    }
    if let Some(u) = active
        .iter()
        .filter(|u| u.status == WorkUnitStatus::Ready)
        .min_by_key(|u| u.seq)
    {
        return NextStep::RunWorkUnit(u.id.clone());
    }
    if active.iter().all(|u| u.status == WorkUnitStatus::Done) {
        return NextStep::AllDone;
    }
    if active
        .iter()
        .any(|u| matches!(u.status, WorkUnitStatus::Running))
    {
        // 直列実行（D6）なので、走っている WU があれば「次」は無い（呼ばれない想定）。
        return NextStep::Stuck("既に実行中の WorkUnit があります".to_string());
    }
    NextStep::Stuck(
        "実行できる WorkUnit がありません（blocked/failed のみ残っています）".to_string(),
    )
}

/// D15: 依存の解決。`depends_on` が全て `done` になった `pending` の WU を `ready` にする（`id` の集合を返す。
/// 呼び出し側が状態を書き換える）。
pub fn newly_ready(units: &[WorkUnitRow]) -> Vec<String> {
    let done: BTreeSet<&str> = units
        .iter()
        .filter(|u| u.status == WorkUnitStatus::Done)
        .map(|u| u.key.as_str())
        .collect();
    units
        .iter()
        .filter(|u| u.status == WorkUnitStatus::Pending)
        .filter(|u| u.depends_on.iter().all(|d| done.contains(d.as_str())))
        .map(|u| u.id.clone())
        .collect()
}

/// D15: WU が `failed` になったとき、それに（直接・間接に）依存する未着手の WU を
/// `blocked(dependency_failed)` にする対象の `id` を返す（推移閉包）。
pub fn dependents_to_block(units: &[WorkUnitRow], failed_key: &str) -> Vec<String> {
    let mut blocked_keys: BTreeSet<String> = BTreeSet::new();
    blocked_keys.insert(failed_key.to_string());
    let mut changed = true;
    while changed {
        changed = false;
        for u in units {
            if blocked_keys.contains(&u.key) {
                continue;
            }
            if matches!(
                u.status,
                WorkUnitStatus::Pending | WorkUnitStatus::Ready | WorkUnitStatus::Blocked
            ) && u.depends_on.iter().any(|d| blocked_keys.contains(d))
            {
                blocked_keys.insert(u.key.clone());
                changed = true;
            }
        }
    }
    blocked_keys.remove(failed_key);
    units
        .iter()
        .filter(|u| blocked_keys.contains(&u.key))
        .map(|u| u.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(key: &str, depends_on: &[&str]) -> WorkUnitSpec {
        WorkUnitSpec {
            key: key.to_string(),
            kind: WorkUnitKind::Implement,
            title: format!("title {key}"),
            objective: format!("objective for {key} which is sufficiently distinct"),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            done_when: vec![],
            checks: vec![],
            context: WorkUnitContext::default(),
            harness: None,
            features: None,
            budget: None,
            outputs: vec![],
        }
    }

    fn plan(work_units: Vec<WorkUnitSpec>) -> ExecutionPlanSpec {
        ExecutionPlanSpec {
            schema: EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "test".to_string(),
            work_units,
        }
    }

    #[test]
    fn valid_three_step_plan_passes_and_orders_topologically() {
        let p = plan(vec![spec("a", &[]), spec("b", &["a"]), spec("c", &["b"])]);
        let validated = validate(&p, ExecutionLimits::default(), &[]).expect("valid");
        let order: Vec<&str> = validated
            .topological_order
            .iter()
            .map(|&i| validated.spec.work_units[i].key.as_str())
            .collect();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn rejects_cycles() {
        let p = plan(vec![spec("a", &["b"]), spec("b", &["a"])]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::CyclicDependency { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_duplicate_keys() {
        let p = plan(vec![spec("a", &[]), spec("a", &[])]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            PlanValidationError::DuplicateKey { key } if key == "a"
        )));
    }

    #[test]
    fn rejects_unknown_dependency() {
        let p = plan(vec![spec("a", &["ghost"])]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::UnknownDependency { .. }))
        );
    }

    #[test]
    fn rejects_invalid_key_format() {
        let p = plan(vec![spec("Not Valid!", &[])]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::InvalidKey { .. }))
        );
    }

    #[test]
    fn rejects_too_many_work_units() {
        let units: Vec<WorkUnitSpec> = (0..10).map(|i| spec(&format!("wu{i}"), &[])).collect();
        let p = plan(units);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::TooManyWorkUnits { .. }))
        );
    }

    #[test]
    fn rejects_near_duplicate_objectives() {
        let mut a = spec("a", &[]);
        a.objective = "investigate the current dispatcher and review pipeline in depth".into();
        let mut b = spec("b", &[]);
        b.objective = "investigate the current dispatcher and review pipeline in depth!".into();
        let p = plan(vec![a, b]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::DuplicateWorkUnit { .. }))
        );
    }

    #[test]
    fn rounds_budget_to_the_limits_and_records_it() {
        let mut a = spec("a", &[]);
        a.budget = Some(WorkUnitBudget {
            max_turns: Some(999),
            max_wall_secs: Some(99999),
        });
        let p = plan(vec![a]);
        let validated = validate(&p, ExecutionLimits::default(), &[]).expect("valid");
        assert_eq!(
            validated.spec.work_units[0].budget.unwrap().max_turns,
            Some(80)
        );
        assert_eq!(
            validated.spec.work_units[0].budget.unwrap().max_wall_secs,
            Some(3600)
        );
        assert_eq!(
            validated.rounding_notes.len(),
            2,
            "{:?}",
            validated.rounding_notes
        );
    }

    #[test]
    fn rejects_changed_done_work_unit_on_replan() {
        let done_spec = spec("a", &[]);
        let mut changed = done_spec.clone();
        changed.objective = "a completely different objective now".to_string();
        let p = plan(vec![changed]);
        let errs = validate(
            &p,
            ExecutionLimits::default(),
            &[("a".to_string(), done_spec)],
        )
        .unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::DoneWorkUnitChanged { .. }))
        );
    }

    fn row(key: &str, seq: u32, status: WorkUnitStatus, depends_on: &[&str]) -> WorkUnitRow {
        WorkUnitRow::new(
            format!("wu-{key}"),
            "task".to_string(),
            "plan".to_string(),
            seq,
            spec(key, depends_on),
            status,
            "2026-09-24T00:00:00Z".to_string(),
        )
    }

    #[test]
    fn next_work_unit_prefers_needs_continuation_then_ready_by_seq() {
        let units = vec![
            row("a", 0, WorkUnitStatus::Done, &[]),
            row("c", 2, WorkUnitStatus::Ready, &[]),
            row("b", 1, WorkUnitStatus::NeedsContinuation, &[]),
        ];
        assert_eq!(next_work_unit(&units), NextStep::RunWorkUnit("wu-b".into()));

        let units2 = vec![
            row("a", 0, WorkUnitStatus::Done, &[]),
            row("c", 2, WorkUnitStatus::Ready, &[]),
            row("b", 1, WorkUnitStatus::Ready, &[]),
        ];
        assert_eq!(
            next_work_unit(&units2),
            NextStep::RunWorkUnit("wu-b".into())
        );
    }

    #[test]
    fn next_work_unit_all_done_when_everything_active_is_done() {
        let units = vec![
            row("a", 0, WorkUnitStatus::Done, &[]),
            row("b", 1, WorkUnitStatus::Superseded, &[]),
        ];
        assert_eq!(next_work_unit(&units), NextStep::AllDone);
    }

    #[test]
    fn newly_ready_promotes_pending_whose_dependencies_are_all_done() {
        let units = vec![
            row("a", 0, WorkUnitStatus::Done, &[]),
            row("b", 1, WorkUnitStatus::Pending, &["a"]),
            row("c", 2, WorkUnitStatus::Pending, &["b"]),
        ];
        assert_eq!(newly_ready(&units), vec!["wu-b".to_string()]);
    }

    #[test]
    fn dependents_to_block_finds_the_transitive_closure() {
        let units = vec![
            row("a", 0, WorkUnitStatus::Failed, &[]),
            row("b", 1, WorkUnitStatus::Pending, &["a"]),
            row("c", 2, WorkUnitStatus::Pending, &["b"]),
            row("d", 3, WorkUnitStatus::Done, &[]),
        ];
        let mut blocked = dependents_to_block(&units, "a");
        blocked.sort();
        assert_eq!(blocked, vec!["wu-b".to_string(), "wu-c".to_string()]);
    }

    /// ADR-0072 D8 / ADR-0003 D6: 生成スキーマとコミット済みファイルの一致。`UPDATE_SCHEMA=1` で再生成。
    #[test]
    fn committed_schema_matches_generated() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/protocol/execution-plan.schema.json"
        );
        let generated = serde_json::to_string_pretty(&schema_value()).unwrap() + "\n";
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e} (run with UPDATE_SCHEMA=1 to generate)"));
        assert_eq!(
            committed, generated,
            "schema drift: run `UPDATE_SCHEMA=1 cargo test -p task-core`"
        );
    }
}
