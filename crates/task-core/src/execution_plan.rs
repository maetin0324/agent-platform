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

/// ADR-0074 D1.1（Phase F2）: `phases`（工程ごとの並列）を持つ計画の schema 版。v1 と同じ
/// `ExecutionPlanSpec` 型を使うが、`phases` が 1 つ以上、各 WorkUnit に `phase` が要る点が違う
/// （`validate` が schema の値で分ける。D1.1）。
pub const EXECUTION_PLAN_SCHEMA_V2: &str = "celeris.execution-plan/2";

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
    /// ADR-0074 D1.4（Phase F2）: 工程末尾の統合 WU（daemon が計画の採用時に足す system WU。
    /// `runs = 0`、LLM run を起こさない）。planner の出力に書かれていれば検証で拒否する
    /// （`validate` の `ReservedKind`。system WU 専用の予約語）。
    Integrate,
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
            WorkUnitKind::Integrate => "integrate",
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
            "integrate" => Some(WorkUnitKind::Integrate),
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
    /// ADR-0074 D1.1（Phase F2）: `celeris.execution-plan/2` では必須（`phases` にある key の
    /// いずれか）。`celeris.execution-plan/1` では無い（`Some` なら検証エラー。v1 の計画・
    /// プロンプトを 1 バイトも変えないため、この欄自体は常に存在するが v1 は `None` のまま）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
}

/// ADR-0074 D1.1（Phase F2）: `celeris.execution-plan/2` の工程。配列の順が実行順（D1.1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PhaseSpec {
    /// `[a-z0-9-]{1,32}`。計画の中で一意。
    pub key: String,
    pub kind: WorkUnitKind,
    pub title: String,
}

/// D14: Planner の出力（または人が `PUT`/`POST` で書く計画）そのもの。
///
/// ADR-0074 D1.1（Phase F2）: `schema` の値で v1 / v2 を分ける（`validate` が判定する）。
/// `phases` / `children` は v1 では常に空（`serde(default)` で省略も読める。v1 の JSON を
/// 1 バイトも変えないため、この 2 欄自体は型として増えるが v1 の出力・検証は変わらない）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlanSpec {
    pub schema: String,
    pub rationale: String,
    /// v2 のみ。v1 では空でなければならない（`validate`）。
    #[serde(default)]
    pub phases: Vec<PhaseSpec>,
    pub work_units: Vec<WorkUnitSpec>,
    /// D3.7: 子 Task の提案。F4 まで空だけを許す（`validate`）。F2 の時点では中身の schema を
    /// 決めていないので、素の JSON 値のまま持つ（今回のPhaseの先回りをしない）。
    #[serde(default)]
    pub children: Vec<serde_json::Value>,
}

/// 生成したスキーマ（`docs/protocol/execution-plan.schema.json`。`UPDATE_SCHEMA=1` で再生成）。
pub fn schema_value() -> serde_json::Value {
    let schema = schemars::schema_for!(ExecutionPlanSpec);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

// ---------------------------------------------------------------------------
// ADR-0074 D5.3（Phase F1）: replan の差分出力 `celeris.execution-plan-delta/1`
// ---------------------------------------------------------------------------

/// D5.3: 差分の schema 版（`docs/protocol/execution-plan-delta.schema.json`）。
pub const EXECUTION_PLAN_DELTA_SCHEMA: &str = "celeris.execution-plan-delta/1";

/// D5.3: 既存の WorkUnit の変える欄だけを書く（`key` は必須、他は書いた欄だけが上書きされる）。
/// 書かなかった欄は変わらない。**欄を「消す」ことはできない**（`harness`/`features`/`budget` を
/// 空に戻したいときは、値を書き換えるのではなく `remove` + `add` で作り直す。Phase F1 の簡略化。
/// `Option<Option<T>>` の二重オプションは素の serde では「省略」と「明示 null」を区別できないため）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkUnitPatch {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<WorkUnitKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_when: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<WorkUnitCheck>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<WorkUnitContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<WorkUnitBudget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<String>>,
}

/// D5.3: replan の差分出力。`base_version` は差分を当てる旧版（`ExecutionPlanRow.version`）。
/// `done` の WU・統合済みの工程は書かない（daemon が旧版から持ち越す）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlanDelta {
    pub schema: String,
    pub base_version: u32,
    pub rationale: String,
    #[serde(default)]
    pub add: Vec<WorkUnitSpec>,
    #[serde(default)]
    pub modify: Vec<WorkUnitPatch>,
    #[serde(default)]
    pub remove: Vec<String>,
}

/// 生成したスキーマ（`docs/protocol/execution-plan-delta.schema.json`）。
pub fn delta_schema_value() -> serde_json::Value {
    let schema = schemars::schema_for!(ExecutionPlanDelta);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

/// D5.3: 差分を旧版（`base`）に当てて新しい版の全体を作る（純粋関数。検証は呼び出し側が
/// `validate` で行う）。`remove` → `modify` → `add` の順に適用する。
///
/// - `remove` に無い key の WU は `base` のまま残る（`done` の WU を書かなくても持ち越される）。
/// - `modify` は既知の key だけに適用できる（`remove` された直後の key、または存在しない key を
///   指せば拒否する）。
/// - `add` の key が既存（`base` に残っているもの）と衝突すれば拒否する。
pub fn apply_delta(
    base: &ExecutionPlanSpec,
    delta: &ExecutionPlanDelta,
) -> Result<ExecutionPlanSpec, String> {
    let mut units: Vec<WorkUnitSpec> = base.work_units.clone();

    let remove_set: BTreeSet<&str> = delta.remove.iter().map(|s| s.as_str()).collect();
    units.retain(|w| !remove_set.contains(w.key.as_str()));

    for patch in &delta.modify {
        let Some(existing) = units.iter_mut().find(|w| w.key == patch.key) else {
            return Err(format!(
                "modify: work unit {:?} does not exist in the base plan (or was removed)",
                patch.key
            ));
        };
        if let Some(v) = patch.kind {
            existing.kind = v;
        }
        if let Some(v) = &patch.title {
            existing.title = v.clone();
        }
        if let Some(v) = &patch.objective {
            existing.objective = v.clone();
        }
        if let Some(v) = &patch.depends_on {
            existing.depends_on = v.clone();
        }
        if let Some(v) = &patch.done_when {
            existing.done_when = v.clone();
        }
        if let Some(v) = &patch.checks {
            existing.checks = v.clone();
        }
        if let Some(v) = &patch.context {
            existing.context = v.clone();
        }
        if let Some(v) = &patch.harness {
            existing.harness = Some(v.clone());
        }
        if let Some(v) = &patch.features {
            existing.features = Some(v.clone());
        }
        if let Some(v) = patch.budget {
            existing.budget = Some(v);
        }
        if let Some(v) = &patch.outputs {
            existing.outputs = v.clone();
        }
    }

    for spec in &delta.add {
        if units.iter().any(|w| w.key == spec.key) {
            return Err(format!(
                "add: work unit key {:?} already exists in the base plan",
                spec.key
            ));
        }
        units.push(spec.clone());
    }

    Ok(ExecutionPlanSpec {
        schema: base.schema.clone(),
        rationale: delta.rationale.clone(),
        work_units: units,
        // ADR-0074 D5.3 は work_units の差分だけを扱う（phases の再構成は扱わない）。F2 で
        // schema v2 が増えたので、差分は phases/children を base のまま持ち越す（replan で
        // 工程構成自体を作り直すことは無い。Phase F2 実装時の逸脱・明確化）。
        phases: base.phases.clone(),
        children: base.children.clone(),
    })
}

// ---------------------------------------------------------------------------
// D14: 検証
// ---------------------------------------------------------------------------

/// D18: 検証・丸めに使う上限（既定値は ADR-0072 D18 の表）。
/// ADR-0074 D5.3（Phase F1）: 計画のサイズ上限（§4）を足す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionLimits {
    pub max_work_units: usize,
    pub work_unit_max_turns: u32,
    pub work_unit_max_wall_secs: u64,
    /// `rationale` の文字数上限（既定 1,500）。
    pub max_rationale_chars: usize,
    /// WU の `title` の文字数上限（既定 120）。
    pub max_title_chars: usize,
    /// WU の `objective` の文字数上限（既定 2,000）。
    pub max_objective_chars: usize,
    /// WU の `done_when` の件数上限（既定 8）。
    pub max_done_when_items: usize,
    /// WU の `done_when` の 1 件あたりの文字数上限（既定 300）。
    pub max_done_when_chars: usize,
    /// WU の `checks` の件数上限（既定 6）。
    pub max_checks: usize,
    /// 計画の JSON 全体の大きさの上限（バイト、既定 24 KiB）。
    pub max_plan_json_bytes: usize,
    /// ADR-0074 §4（Phase F2）: `celeris.execution-plan/2` の `work_units` の件数上限（既定 10。
    /// 統合 WU・repair は数えない）。`max_work_units` は v1 専用のまま（既定 8。§4 の表）。
    pub max_work_units_v2: usize,
    /// ADR-0074 §4（Phase F2）: `phases` の件数上限（既定 5）。
    pub max_phases: usize,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        ExecutionLimits {
            max_work_units: 8,
            work_unit_max_turns: 80,
            work_unit_max_wall_secs: 3600,
            max_rationale_chars: 1_500,
            max_title_chars: 120,
            max_objective_chars: 2_000,
            max_done_when_items: 8,
            max_done_when_chars: 300,
            max_checks: 6,
            max_plan_json_bytes: 24 * 1024,
            max_work_units_v2: 10,
            max_phases: 5,
        }
    }
}

/// D14: 検証エラー（すべて拒否理由。1 回だけ再試行し、それでも駄目なら atomic に倒す。呼び出し側の責務）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanValidationError {
    /// `schema` 欄が `celeris.execution-plan/1` でも `/2`（ADR-0074 D1.1、Phase F2）でもない。
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
    /// ADR-0074 D5.1（Phase F1）: WU の `features` が `TaskFeatureHints` として読めない
    /// （`deny_unknown_fields` を含む型として不正）。
    InvalidFeatures {
        key: String,
        detail: String,
    },
    /// ADR-0074 D5.3（Phase F1）: `rationale` が上限を超える。
    RationaleTooLong {
        len: usize,
        max: usize,
    },
    /// ADR-0074 D5.3（Phase F1）: WU の `title` が上限を超える。
    TitleTooLong {
        key: String,
        len: usize,
        max: usize,
    },
    /// ADR-0074 D5.3（Phase F1）: WU の `objective` が上限を超える。
    ObjectiveTooLong {
        key: String,
        len: usize,
        max: usize,
    },
    /// ADR-0074 D5.3（Phase F1）: WU の `done_when` の件数が上限を超える。
    TooManyDoneWhen {
        key: String,
        count: usize,
        max: usize,
    },
    /// ADR-0074 D5.3（Phase F1）: WU の `done_when` の 1 件が上限を超える。
    DoneWhenItemTooLong {
        key: String,
        index: usize,
        len: usize,
        max: usize,
    },
    /// ADR-0074 D5.3（Phase F1）: WU の `checks` の件数が上限を超える。
    TooManyChecks {
        key: String,
        count: usize,
        max: usize,
    },
    /// ADR-0074 D5.3（Phase F1）: 計画の JSON 全体が上限を超える。
    PlanTooLarge {
        bytes: usize,
        max: usize,
    },
    /// ADR-0074 D1.1（Phase F2）: `celeris.execution-plan/1` に `phases` が書かれている
    /// （v1 は工程を持たない。1 バイトも変えない）。
    PhasesNotAllowedInV1,
    /// ADR-0074 D1.1（Phase F2）: `celeris.execution-plan/2` に `phases` が 1 つも無い。
    NoPhases,
    /// ADR-0074 §4（Phase F2）: `phases` の件数が上限を超える。
    TooManyPhases {
        count: usize,
        max: usize,
    },
    /// ADR-0074 D1.1（Phase F2）: 工程の `key` が `[a-z0-9-]{1,32}` に合わない。
    InvalidPhaseKey {
        key: String,
    },
    /// ADR-0074 D1.1（Phase F2）: 工程の `key` が重複している。
    DuplicatePhaseKey {
        key: String,
    },
    /// ADR-0074 D1.1（Phase F2）: `celeris.execution-plan/1` の WorkUnit に `phase` が書かれている
    /// （v1 は工程を持たない）。
    WorkUnitPhaseNotAllowedInV1 {
        key: String,
    },
    /// ADR-0074 D1.1（Phase F2）: `celeris.execution-plan/2` の WorkUnit に `phase` が無い（必須）。
    WorkUnitMissingPhase {
        key: String,
    },
    /// ADR-0074 D1.1（Phase F2）: WorkUnit の `phase` が `phases` に無い key を指している。
    UnknownWorkUnitPhase {
        key: String,
        phase: String,
    },
    /// ADR-0074 D1.1（Phase F2）: 依存先が自分より後の工程にある（拒否。D1.1 の規則 1）。
    DependencyInLaterPhase {
        key: String,
        depends_on: String,
    },
    /// ADR-0074 D1.1（Phase F2）: 同じ工程の中の依存が 2 つ以上ある（規則 2。鎖か木のみ許す）。
    TooManyIntraPhaseDependencies {
        key: String,
        phase: String,
    },
    /// ADR-0074 D3.7（Phase F2）: `children` が空でない（F4 まで空だけを許す）。
    NonEmptyChildren,
    /// ADR-0074 D1.4（Phase F2）: `kind = integrate` は daemon が足す system WU 専用の予約語で、
    /// 計画（planner・人）が自分の WorkUnit にこの kind を書くことはできない。
    ReservedKind {
        key: String,
    },
    /// ADR-0074 D1.4（Phase F2b）: `integrate-` で始まる key は daemon が足す統合 WU
    /// （`integrate-<phase>`）の予約語。
    ReservedKey {
        key: String,
    },
}

impl std::fmt::Display for PlanValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanValidationError::WrongSchema { found } => {
                write!(
                    f,
                    "schema must be {EXECUTION_PLAN_SCHEMA} or {EXECUTION_PLAN_SCHEMA_V2}, found {found}"
                )
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
            PlanValidationError::InvalidFeatures { key, detail } => {
                write!(
                    f,
                    "work unit {key}: features must parse as TaskFeatureHints: {detail}"
                )
            }
            PlanValidationError::RationaleTooLong { len, max } => {
                write!(f, "rationale is too long: {len} > {max} characters")
            }
            PlanValidationError::TitleTooLong { key, len, max } => {
                write!(
                    f,
                    "work unit {key}: title is too long: {len} > {max} characters"
                )
            }
            PlanValidationError::ObjectiveTooLong { key, len, max } => {
                write!(
                    f,
                    "work unit {key}: objective is too long: {len} > {max} characters"
                )
            }
            PlanValidationError::TooManyDoneWhen { key, count, max } => {
                write!(
                    f,
                    "work unit {key}: too many done_when items: {count} > {max}"
                )
            }
            PlanValidationError::DoneWhenItemTooLong {
                key,
                index,
                len,
                max,
            } => {
                write!(
                    f,
                    "work unit {key}: done_when[{index}] is too long: {len} > {max} characters"
                )
            }
            PlanValidationError::TooManyChecks { key, count, max } => {
                write!(f, "work unit {key}: too many checks: {count} > {max}")
            }
            PlanValidationError::PlanTooLarge { bytes, max } => {
                write!(f, "execution plan JSON is too large: {bytes} > {max} bytes")
            }
            PlanValidationError::PhasesNotAllowedInV1 => {
                write!(f, "phases must be empty in {EXECUTION_PLAN_SCHEMA}")
            }
            PlanValidationError::NoPhases => {
                write!(f, "phases must not be empty in {EXECUTION_PLAN_SCHEMA_V2}")
            }
            PlanValidationError::TooManyPhases { count, max } => {
                write!(f, "too many phases: {count} > {max}")
            }
            PlanValidationError::InvalidPhaseKey { key } => {
                write!(
                    f,
                    "invalid phase key: {key:?} (must match [a-z0-9-]{{1,32}})"
                )
            }
            PlanValidationError::DuplicatePhaseKey { key } => {
                write!(f, "duplicate phase key: {key}")
            }
            PlanValidationError::WorkUnitPhaseNotAllowedInV1 { key } => {
                write!(
                    f,
                    "work unit {key}: phase must not be set in {EXECUTION_PLAN_SCHEMA}"
                )
            }
            PlanValidationError::WorkUnitMissingPhase { key } => {
                write!(
                    f,
                    "work unit {key}: phase is required in {EXECUTION_PLAN_SCHEMA_V2}"
                )
            }
            PlanValidationError::UnknownWorkUnitPhase { key, phase } => {
                write!(f, "work unit {key}: unknown phase {phase:?}")
            }
            PlanValidationError::DependencyInLaterPhase { key, depends_on } => {
                write!(
                    f,
                    "work unit {key}: depends on {depends_on}, which is in a later phase"
                )
            }
            PlanValidationError::TooManyIntraPhaseDependencies { key, phase } => {
                write!(
                    f,
                    "work unit {key}: depends on more than one work unit within phase {phase} \
                     (at most one intra-phase dependency is allowed)"
                )
            }
            PlanValidationError::NonEmptyChildren => {
                write!(f, "children must be empty (not implemented until Phase F4)")
            }
            PlanValidationError::ReservedKind { key } => {
                write!(
                    f,
                    "work unit {key}: kind \"integrate\" is reserved for daemon-created integration work units"
                )
            }
            PlanValidationError::ReservedKey { key } => {
                write!(
                    f,
                    "work unit {key}: keys starting with \"{INTEGRATE_KEY_PREFIX}\" are reserved for daemon-created integration work units"
                )
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

    // ADR-0074 D1.1（Phase F2）: `schema` の値で v1 / v2 を分ける。どちらでもなければ
    // `WrongSchema` を出すが、それ以外の検査（v1 の形として）は続けて行う（複数のエラーを
    // 一度に返す既存の流儀。E2 のまま）。
    let is_v2 = spec.schema == EXECUTION_PLAN_SCHEMA_V2;
    if spec.schema != EXECUTION_PLAN_SCHEMA && !is_v2 {
        errors.push(PlanValidationError::WrongSchema {
            found: spec.schema.clone(),
        });
    }
    if spec.work_units.is_empty() {
        errors.push(PlanValidationError::NoWorkUnits);
    }
    let max_work_units = if is_v2 {
        limits.max_work_units_v2
    } else {
        limits.max_work_units
    };
    if spec.work_units.len() > max_work_units {
        errors.push(PlanValidationError::TooManyWorkUnits {
            count: spec.work_units.len(),
            max: max_work_units,
        });
    }

    // ADR-0074 D3.7（Phase F2）: `children` は F4 まで空だけを許す（v1/v2 共通）。
    if !spec.children.is_empty() {
        errors.push(PlanValidationError::NonEmptyChildren);
    }

    // ADR-0074 D1.1（Phase F2）: `phases`（v2 のみ）。
    if is_v2 {
        if spec.phases.is_empty() {
            errors.push(PlanValidationError::NoPhases);
        }
        if spec.phases.len() > limits.max_phases {
            errors.push(PlanValidationError::TooManyPhases {
                count: spec.phases.len(),
                max: limits.max_phases,
            });
        }
        let mut seen_phase_keys: BTreeSet<&str> = BTreeSet::new();
        for p in &spec.phases {
            if !valid_key(&p.key) {
                errors.push(PlanValidationError::InvalidPhaseKey { key: p.key.clone() });
                continue;
            }
            if !seen_phase_keys.insert(p.key.as_str()) {
                errors.push(PlanValidationError::DuplicatePhaseKey { key: p.key.clone() });
            }
        }
    } else if !spec.phases.is_empty() {
        errors.push(PlanValidationError::PhasesNotAllowedInV1);
    }

    // ADR-0074 D1.4（Phase F2）: `kind = integrate` は system WU 専用の予約語。
    for wu in &spec.work_units {
        if wu.kind == WorkUnitKind::Integrate {
            errors.push(PlanValidationError::ReservedKind {
                key: wu.key.clone(),
            });
        }
        // ADR-0074 D1.4（Phase F2b）: `integrate-<phase>` の key も予約（v2 のみ。v1 は不変）。
        if spec.schema == EXECUTION_PLAN_SCHEMA_V2 && wu.key.starts_with(INTEGRATE_KEY_PREFIX) {
            errors.push(PlanValidationError::ReservedKey {
                key: wu.key.clone(),
            });
        }
    }

    // ADR-0074 D1.1（Phase F2）: WorkUnit の `phase` は v2 で必須・既知の工程を指す、v1 で禁止。
    let phase_index: BTreeMap<&str, usize> = spec
        .phases
        .iter()
        .enumerate()
        .map(|(i, p)| (p.key.as_str(), i))
        .collect();
    for wu in &spec.work_units {
        if is_v2 {
            match &wu.phase {
                None => errors.push(PlanValidationError::WorkUnitMissingPhase {
                    key: wu.key.clone(),
                }),
                Some(phase) if !phase_index.contains_key(phase.as_str()) => {
                    errors.push(PlanValidationError::UnknownWorkUnitPhase {
                        key: wu.key.clone(),
                        phase: phase.clone(),
                    });
                }
                Some(_) => {}
            }
        } else if wu.phase.is_some() {
            errors.push(PlanValidationError::WorkUnitPhaseNotAllowedInV1 {
                key: wu.key.clone(),
            });
        }
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

    // ADR-0074 D1.1（Phase F2）: v2 の依存の規則 1・2（既知の key・既知の phase を持つ WU 同士に
    // 限る。上の 2 つの検査で既にエラーが出ている key/phase は静かに飛ばす。二重にエラーを
    // 積まないため）。
    if is_v2 {
        let wu_phase_of: BTreeMap<&str, usize> = spec
            .work_units
            .iter()
            .filter_map(|w| {
                w.phase
                    .as_deref()
                    .and_then(|p| phase_index.get(p))
                    .map(|&idx| (w.key.as_str(), idx))
            })
            .collect();
        for wu in &spec.work_units {
            let Some(&own_phase_idx) = wu_phase_of.get(wu.key.as_str()) else {
                continue;
            };
            let mut intra_phase_deps = 0usize;
            for dep in &wu.depends_on {
                let Some(&dep_phase_idx) = wu_phase_of.get(dep.as_str()) else {
                    continue;
                };
                match dep_phase_idx.cmp(&own_phase_idx) {
                    std::cmp::Ordering::Greater => {
                        errors.push(PlanValidationError::DependencyInLaterPhase {
                            key: wu.key.clone(),
                            depends_on: dep.clone(),
                        });
                    }
                    std::cmp::Ordering::Equal => intra_phase_deps += 1,
                    std::cmp::Ordering::Less => {}
                }
            }
            if intra_phase_deps > 1 {
                errors.push(PlanValidationError::TooManyIntraPhaseDependencies {
                    key: wu.key.clone(),
                    phase: wu.phase.clone().unwrap_or_default(),
                });
            }
        }
    }

    // ADR-0074 D5.1（Phase F1）: `features` は書かれていれば `TaskFeatureHints` として読めなければ
    // ならない（黙って捨てない）。5 軸のうちいくつか欠けているだけなら拒否しない
    // （`decide_for_work_unit` が Task から推定した値のまま補う）。
    for wu in &spec.work_units {
        if let Some(features) = &wu.features
            && let Err(e) =
                serde_json::from_value::<crate::model_policy::TaskFeatureHints>(features.clone())
        {
            errors.push(PlanValidationError::InvalidFeatures {
                key: wu.key.clone(),
                detail: e.to_string(),
            });
        }
    }

    // ADR-0074 D5.3（Phase F1）: 計画のサイズ上限（§4）。
    if spec.rationale.chars().count() > limits.max_rationale_chars {
        errors.push(PlanValidationError::RationaleTooLong {
            len: spec.rationale.chars().count(),
            max: limits.max_rationale_chars,
        });
    }
    for wu in &spec.work_units {
        let title_len = wu.title.chars().count();
        if title_len > limits.max_title_chars {
            errors.push(PlanValidationError::TitleTooLong {
                key: wu.key.clone(),
                len: title_len,
                max: limits.max_title_chars,
            });
        }
        let objective_len = wu.objective.chars().count();
        if objective_len > limits.max_objective_chars {
            errors.push(PlanValidationError::ObjectiveTooLong {
                key: wu.key.clone(),
                len: objective_len,
                max: limits.max_objective_chars,
            });
        }
        if wu.done_when.len() > limits.max_done_when_items {
            errors.push(PlanValidationError::TooManyDoneWhen {
                key: wu.key.clone(),
                count: wu.done_when.len(),
                max: limits.max_done_when_items,
            });
        }
        for (index, item) in wu.done_when.iter().enumerate() {
            let len = item.chars().count();
            if len > limits.max_done_when_chars {
                errors.push(PlanValidationError::DoneWhenItemTooLong {
                    key: wu.key.clone(),
                    index,
                    len,
                    max: limits.max_done_when_chars,
                });
            }
        }
        if wu.checks.len() > limits.max_checks {
            errors.push(PlanValidationError::TooManyChecks {
                key: wu.key.clone(),
                count: wu.checks.len(),
                max: limits.max_checks,
            });
        }
    }
    let plan_bytes = serde_json::to_vec(spec).map(|v| v.len()).unwrap_or(0);
    if plan_bytes > limits.max_plan_json_bytes {
        errors.push(PlanValidationError::PlanTooLarge {
            bytes: plan_bytes,
            max: limits.max_plan_json_bytes,
        });
    }

    // トポロジカルソート（循環の検出も兼ねる。Kahn's algorithm、決定的に `(phase, key)` 昇順で
    // tie-break。D1.1）。
    let mut topological_order = Vec::new();
    if errors.is_empty() {
        match topo_sort(&spec.work_units, &phase_index) {
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

/// Kahn's algorithm。`Ok` はトポロジカル順（`work_units` の index。同順位は
/// `(phase_rank, key)` 昇順で tie-break）、`Err` は見つかった循環（key の列）。
///
/// ADR-0074 D1.1（Phase F2）: `phase_rank` は工程の key → `phases` での出現順（v2）。v1 は
/// 常に空の map を渡す（すべて rank 0 のまま。挙動は従来と 1 バイトも変わらない）。v2 では
/// 工程をまたぐ依存は必ず前の工程 → 後の工程（`validate` が別途保証する）なので、この
/// tie-break は「依存の無い WU 同士がどちらの工程にいても後の工程が先に選ばれない」ことだけを
/// 保証する（表示・`seq` の並びのため。scheduler 自体は `phase` 列で絞り込む。D1.3）。
fn topo_sort(
    work_units: &[WorkUnitSpec],
    phase_rank: &BTreeMap<&str, usize>,
) -> Result<Vec<usize>, Vec<String>> {
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
    let rank_of = |wu: &WorkUnitSpec| -> usize {
        wu.phase
            .as_deref()
            .and_then(|p| phase_rank.get(p).copied())
            .unwrap_or(0)
    };
    // 決定的に (phase_rank, key) 昇順で並べた候補集合を都度取り出す。
    let mut ready: BTreeSet<(usize, &str, usize)> = work_units
        .iter()
        .enumerate()
        .filter(|(i, _)| in_degree[*i] == 0)
        .map(|(i, w)| (rank_of(w), w.key.as_str(), i))
        .collect();
    let mut order = Vec::new();
    while let Some((_, _, i)) = ready.iter().next().copied() {
        ready.remove(&(rank_of(&work_units[i]), work_units[i].key.as_str(), i));
        order.push(i);
        for &dep in &dependents[i] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 {
                ready.insert((rank_of(&work_units[dep]), work_units[dep].key.as_str(), dep));
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
    /// ADR-0074 D1（Phase F2 / migration 0027）: v2 の工程の key（v1・atomic は `None`）。
    /// 普通の WU は `spec.phase` の写し、統合 WU（`integrate-<phase>`）は自分の工程。
    pub phase: Option<String>,
    /// ADR-0074 D1.5: この WU を今実行している run（`acquire_work_unit_lease`）。揮発（replay で比べない）。
    pub lease_run_id: Option<String>,
    /// ADR-0074 D1.5: 上の lease の期限（RFC 3339）。揮発。
    pub lease_expires_at: Option<String>,
    /// ADR-0074 D1.2: `celeris-wu/<task_id>/<key>`（WU の worktree を切ったときだけ）。
    pub branch: Option<String>,
    /// ADR-0074 D1.2: WU の worktree を切った基点 sha。
    pub base_commit: Option<String>,
    /// ADR-0074 D1.2: `WorkUnitCommitted` の commit（run が done になったときの決定的な commit）。
    pub head_commit: Option<String>,
    /// ADR-0074 D1.4: `PhaseIntegrated` の Task ブランチの HEAD（統合 WU の行だけ）。
    pub integrated_commit: Option<String>,
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
            phase: spec.phase.clone(),
            spec,
            created_at: created_at.clone(),
            updated_at: created_at,
            lease_run_id: None,
            lease_expires_at: None,
            branch: None,
            base_commit: None,
            head_commit: None,
            integrated_commit: None,
        }
    }

    /// ADR-0074 D1.5: WU の lease を外す（`running` を離れる遷移で呼ぶ）。
    pub fn clear_lease(&mut self) {
        self.lease_run_id = None;
        self.lease_expires_at = None;
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

/// ADR-0074 D1.3（Phase F2）: `next_work_unit` の一般化。工程の中で並列に何本まで起こせるかを
/// 決める（純粋関数。実際に走らせる・lease を取るのは呼び出し側の責務）。
///
/// - 対象は**現在の工程**（有効〈`!is_terminal()`〉な WorkUnit が残っている工程のうち、
///   `seq` が最も小さいもの）だけ。v1（`spec.phase` が常に `None`）は工程が実質 1 つなので
///   全部が対象になる（`limit = 1` と組み合わせると `next_work_unit` と同じ 1 件を返す）。
/// - `needs_continuation` を先に、次に `ready` を `seq` 順で選ぶ（`next_work_unit` と同じ順序）。
/// - `limit` から `in_flight`（呼び出し側がこの Task について現在走らせている run の数）を引いた
///   件数まで。
/// - 現在の工程に `failed`/`blocked` の WorkUnit があれば、**新しい**（`ready` の）WorkUnit は
///   起こさない。ただし既に走ったことのある `needs_continuation` の WorkUnit は続ける
///   （D1.6「走っている WU とその continuation だけは続ける」）。
pub fn runnable_work_units(units: &[WorkUnitRow], in_flight: usize, limit: usize) -> Vec<String> {
    let slots = limit.saturating_sub(in_flight);
    if slots == 0 {
        return Vec::new();
    }
    let in_play: Vec<&WorkUnitRow> = units.iter().filter(|u| !u.status.is_terminal()).collect();
    let Some(current) = in_play.iter().min_by_key(|u| u.seq) else {
        return Vec::new();
    };
    let current_phase = current.spec.phase.as_deref();
    let in_phase: Vec<&WorkUnitRow> = in_play
        .into_iter()
        .filter(|u| u.spec.phase.as_deref() == current_phase)
        .collect();

    let has_failed_or_blocked = in_phase
        .iter()
        .any(|u| matches!(u.status, WorkUnitStatus::Failed | WorkUnitStatus::Blocked));

    let mut candidates: Vec<&WorkUnitRow> = in_phase
        .iter()
        .copied()
        .filter(|u| u.status == WorkUnitStatus::NeedsContinuation)
        .collect();
    candidates.sort_by_key(|u| u.seq);

    if !has_failed_or_blocked {
        let mut ready: Vec<&WorkUnitRow> = in_phase
            .iter()
            .copied()
            .filter(|u| u.status == WorkUnitStatus::Ready)
            .collect();
        ready.sort_by_key(|u| u.seq);
        candidates.extend(ready);
    }

    candidates
        .into_iter()
        .take(slots)
        .map(|u| u.id.clone())
        .collect()
}

/// D15: 依存の解決。`depends_on` が全て `done` になった `pending` の WU を `ready` にする（`id` の集合を返す。
/// 呼び出し側が状態を書き換える）。
///
/// ADR-0074 D1.3（Phase F2b）: v2（`phase` のある行）では「前の工程の統合が done」を追加の条件にする
/// （工程の境は障壁。前の工程の有効な行〈統合 WU を含む〉がすべて `done` になるまで上げない）。
/// 統合 WU（`kind = integrate`）は `ready` にしない（scheduler が工程の完了を見て直接走らせる）。
/// v1（`phase` が無い行）は従来と同じ。
pub fn newly_ready(units: &[WorkUnitRow]) -> Vec<String> {
    let done: BTreeSet<&str> = units
        .iter()
        .filter(|u| u.status == WorkUnitStatus::Done)
        .map(|u| u.key.as_str())
        .collect();
    let ranks = phase_ranks(units);
    units
        .iter()
        .filter(|u| u.status == WorkUnitStatus::Pending)
        .filter(|u| u.kind != WorkUnitKind::Integrate)
        .filter(|u| u.depends_on.iter().all(|d| done.contains(d.as_str())))
        .filter(|u| earlier_phases_done(units, &ranks, u))
        .map(|u| u.id.clone())
        .collect()
}

/// ADR-0074 D1.3（Phase F2b）: 工程の key → 順位（その工程の行の `seq` の最小値。`topo_sort` が
/// 工程順を保証し、統合 WU は工程の末尾に置くので、`seq` の最小値で工程の順が引ける）。
pub fn phase_ranks(units: &[WorkUnitRow]) -> BTreeMap<String, u32> {
    let mut ranks: BTreeMap<String, u32> = BTreeMap::new();
    for u in units.iter().filter(|u| u.status.is_active()) {
        if let Some(p) = &u.phase {
            let e = ranks.entry(p.clone()).or_insert(u.seq);
            if u.seq < *e {
                *e = u.seq;
            }
        }
    }
    ranks
}

fn earlier_phases_done(
    units: &[WorkUnitRow],
    ranks: &BTreeMap<String, u32>,
    u: &WorkUnitRow,
) -> bool {
    let Some(rank) = u.phase.as_ref().and_then(|p| ranks.get(p)) else {
        return true;
    };
    units
        .iter()
        .filter(|o| o.status.is_active())
        .filter(|o| {
            o.phase
                .as_ref()
                .and_then(|p| ranks.get(p))
                .is_some_and(|r| r < rank)
        })
        .all(|o| o.status == WorkUnitStatus::Done)
}

/// ADR-0074 D1.4（Phase F2b）: 統合 WU の key の接頭辞（`integrate-<phase>`）。
pub const INTEGRATE_KEY_PREFIX: &str = "integrate-";

/// `integrate-<phase>`。
pub fn integrate_key(phase: &str) -> String {
    format!("{INTEGRATE_KEY_PREFIX}{phase}")
}

/// ADR-0074 D1.4（Phase F2b）: v2 の工程ごとの統合 WU の spec（`kind = integrate`、依存は
/// その工程のすべての WU）。計画の spec（`ExecutionPlanned.plan`）には入れない（daemon が足す
/// system WU。`max_work_units` にも数えない）。v1 は空。
pub fn integration_work_unit_specs(spec: &ExecutionPlanSpec) -> Vec<WorkUnitSpec> {
    if spec.schema != EXECUTION_PLAN_SCHEMA_V2 {
        return Vec::new();
    }
    spec.phases
        .iter()
        .map(|p| WorkUnitSpec {
            key: integrate_key(&p.key),
            kind: WorkUnitKind::Integrate,
            title: format!("工程 {} の統合", p.title),
            objective: format!(
                "工程 {} の WorkUnit のブランチを Task のブランチへ決定的に merge し、検査を再実行する（daemon が行う。LLM run は起こさない）",
                p.key
            ),
            depends_on: spec
                .work_units
                .iter()
                .filter(|w| w.phase.as_deref() == Some(p.key.as_str()))
                .map(|w| w.key.clone())
                .collect(),
            done_when: Vec::new(),
            checks: Vec::new(),
            context: WorkUnitContext::default(),
            harness: None,
            features: None,
            budget: None,
            outputs: Vec::new(),
            phase: Some(p.key.clone()),
        })
        .collect()
}

/// ADR-0074 D1.1/D1.4（Phase F2b）: 採用する計画の WU の並び（`seq` の順）。v1 はトポロジカル順
/// そのまま（従来どおり）。v2 は工程ごとに「その工程の WU（トポロジカル順）→ `integrate-<phase>`」。
pub fn materialized_order(
    spec: &ExecutionPlanSpec,
    topological_order: &[usize],
) -> Vec<WorkUnitSpec> {
    let in_order: Vec<WorkUnitSpec> = topological_order
        .iter()
        .filter_map(|&i| spec.work_units.get(i).cloned())
        .collect();
    if spec.schema != EXECUTION_PLAN_SCHEMA_V2 {
        return in_order;
    }
    let integrations = integration_work_unit_specs(spec);
    let mut out = Vec::with_capacity(in_order.len() + integrations.len());
    for (phase, integrate) in spec.phases.iter().zip(integrations) {
        out.extend(
            in_order
                .iter()
                .filter(|w| w.phase.as_deref() == Some(phase.key.as_str()))
                .cloned(),
        );
        out.push(integrate);
    }
    out
}

/// ADR-0074 D1.1/D1.4（Phase F2b）: 採用する計画の `work_units` の行を作る（純粋関数）。`id_of` は
/// 行の id を決める（`adopt_plan` は新しい ULID、replay は events から復元した id）。
/// v1 は「依存が無ければ ready、あれば pending」（従来どおり）。v2 は統合 WU を足し、
/// 工程の障壁つきの [`newly_ready`] で ready を決める（最初の工程の依存の無い WU だけが ready）。
pub fn materialize_work_units(
    task_id: &str,
    plan_id: &str,
    spec: &ExecutionPlanSpec,
    topological_order: &[usize],
    created_at: &str,
    id_of: &mut dyn FnMut(&WorkUnitSpec) -> String,
) -> Vec<WorkUnitRow> {
    let v2 = spec.schema == EXECUTION_PLAN_SCHEMA_V2;
    let mut rows: Vec<WorkUnitRow> = materialized_order(spec, topological_order)
        .into_iter()
        .enumerate()
        .map(|(seq, wu_spec)| {
            let status = if !v2 && wu_spec.depends_on.is_empty() {
                WorkUnitStatus::Ready
            } else {
                WorkUnitStatus::Pending
            };
            WorkUnitRow::new(
                id_of(&wu_spec),
                task_id.to_string(),
                plan_id.to_string(),
                seq as u32,
                wu_spec,
                status,
                created_at.to_string(),
            )
        })
        .collect();
    if v2 {
        let ready = newly_ready(&rows);
        for r in rows.iter_mut() {
            if ready.contains(&r.id) {
                r.status = WorkUnitStatus::Ready;
            }
        }
    }
    rows
}

/// ADR-0074 D1.4（Phase F2b）: 工程 `phase` の葉の WU（同じ工程の他の有効な WU に依存されていない、
/// 統合 WU 以外、ブランチを持つもの）を `seq` 順で。積み上げた依存先は葉に含まれる。
pub fn phase_leaves<'a>(units: &'a [WorkUnitRow], phase: &str) -> Vec<&'a WorkUnitRow> {
    let in_phase: Vec<&WorkUnitRow> = units
        .iter()
        .filter(|u| u.status.is_active())
        .filter(|u| u.kind != WorkUnitKind::Integrate)
        .filter(|u| u.phase.as_deref() == Some(phase))
        .collect();
    let mut leaves: Vec<&WorkUnitRow> = in_phase
        .iter()
        .copied()
        .filter(|u| u.branch.is_some())
        .filter(|u| {
            !in_phase
                .iter()
                .any(|o| o.id != u.id && o.depends_on.iter().any(|d| d == &u.key))
        })
        .collect();
    leaves.sort_by_key(|u| u.seq);
    leaves
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
            phase: None,
        }
    }

    fn plan(work_units: Vec<WorkUnitSpec>) -> ExecutionPlanSpec {
        ExecutionPlanSpec {
            schema: EXECUTION_PLAN_SCHEMA.to_string(),
            rationale: "test".to_string(),
            work_units,
            phases: Vec::new(),
            children: Vec::new(),
        }
    }

    // ---- ADR-0074 D1.1（Phase F2）: v2（`phases`）のテスト用ヘルパー ----

    fn phase(key: &str) -> PhaseSpec {
        PhaseSpec {
            key: key.to_string(),
            kind: WorkUnitKind::Implement,
            title: format!("phase {key}"),
        }
    }

    fn spec_v2(key: &str, wu_phase: &str, depends_on: &[&str]) -> WorkUnitSpec {
        let mut s = spec(key, depends_on);
        s.phase = Some(wu_phase.to_string());
        s
    }

    fn plan_v2(phases: Vec<PhaseSpec>, work_units: Vec<WorkUnitSpec>) -> ExecutionPlanSpec {
        ExecutionPlanSpec {
            schema: EXECUTION_PLAN_SCHEMA_V2.to_string(),
            rationale: "test v2".to_string(),
            work_units,
            phases,
            children: Vec::new(),
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

    /// ADR-0074 D5.1（Phase F1）: `features` は `TaskFeatureHints` として読めなければ検証エラー
    /// （黙って `.ok()` で捨てない）。
    #[test]
    fn features_must_parse_as_task_feature_hints() {
        let mut a = spec("a", &[]);
        a.features = Some(serde_json::json!({"judgment": "low", "ambiguity": "low"}));
        let p = plan(vec![a]);
        validate(&p, ExecutionLimits::default(), &[]).expect("valid partial features");

        let mut b = spec("b", &[]);
        // `lane` は `TaskFeatureHints` に無い欄（`deny_unknown_fields`）。
        b.features = Some(serde_json::json!({"lane": "frontier"}));
        let p = plan(vec![b]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter().any(
                |e| matches!(e, PlanValidationError::InvalidFeatures { key, .. } if key == "b")
            ),
            "{errs:?}"
        );

        let mut c = spec("c", &[]);
        // 型が違う（配列であるべき欄が文字列）。
        c.features = Some(serde_json::json!({"judgment": "not-a-level"}));
        let p = plan(vec![c]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter().any(
                |e| matches!(e, PlanValidationError::InvalidFeatures { key, .. } if key == "c")
            ),
            "{errs:?}"
        );
    }

    /// ADR-0074 D5.3（Phase F1）: 計画のサイズ上限（§4）。
    #[test]
    fn plan_size_limits_reject_oversized_rationale() {
        let mut p = plan(vec![spec("a", &[])]);
        p.rationale = "x".repeat(1_501);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter().any(
                |e| matches!(e, PlanValidationError::RationaleTooLong { len, max } if *len == 1_501 && *max == 1_500)
            ),
            "{errs:?}"
        );
    }

    #[test]
    fn plan_size_limits_reject_oversized_work_unit_fields() {
        let mut a = spec("a", &[]);
        a.title = "x".repeat(121);
        a.objective = "y".repeat(2_001);
        a.done_when = (0..9).map(|i| format!("done {i}")).collect();
        a.checks = (0..7)
            .map(|i| WorkUnitCheck {
                cmd: format!("cmd {i}"),
                expect_exit: 0,
            })
            .collect();
        let p = plan(vec![a]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::TitleTooLong { key, .. } if key == "a")),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(
                |e| matches!(e, PlanValidationError::ObjectiveTooLong { key, .. } if key == "a")
            ),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(
                |e| matches!(e, PlanValidationError::TooManyDoneWhen { key, .. } if key == "a")
            ),
            "{errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::TooManyChecks { key, .. } if key == "a")),
            "{errs:?}"
        );
    }

    #[test]
    fn plan_size_limits_reject_the_whole_json_being_too_large() {
        let limits = ExecutionLimits {
            max_plan_json_bytes: 200,
            ..ExecutionLimits::default()
        };
        let p = plan(vec![spec("a", &[]), spec("b", &["a"])]);
        let errs = validate(&p, limits, &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::PlanTooLarge { .. })),
            "{errs:?}"
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

    // ---- ADR-0074 D1.3（Phase F2）: `runnable_work_units` ----

    fn row_v2(
        key: &str,
        wu_phase: &str,
        seq: u32,
        status: WorkUnitStatus,
        depends_on: &[&str],
    ) -> WorkUnitRow {
        WorkUnitRow::new(
            format!("wu-{key}"),
            "task".to_string(),
            "plan".to_string(),
            seq,
            spec_v2(key, wu_phase, depends_on),
            status,
            "2026-09-24T00:00:00Z".to_string(),
        )
    }

    /// v1（`phase` が常に `None`）で `limit = 1` なら `next_work_unit` と同じ 1 件を返す。
    #[test]
    fn runnable_work_units_matches_next_work_unit_for_v1_with_limit_one() {
        let units = vec![
            row("a", 0, WorkUnitStatus::Done, &[]),
            row("c", 2, WorkUnitStatus::Ready, &[]),
            row("b", 1, WorkUnitStatus::NeedsContinuation, &[]),
        ];
        assert_eq!(runnable_work_units(&units, 0, 1), vec!["wu-b".to_string()]);
    }

    #[test]
    fn runnable_work_units_respects_the_parallel_limit() {
        let units = vec![
            row_v2("a", "build", 0, WorkUnitStatus::Ready, &[]),
            row_v2("b", "build", 1, WorkUnitStatus::Ready, &[]),
            row_v2("c", "build", 2, WorkUnitStatus::Ready, &[]),
        ];
        assert_eq!(
            runnable_work_units(&units, 0, 2),
            vec!["wu-a".to_string(), "wu-b".to_string()],
            "2 本まで、seq 順"
        );
        assert_eq!(
            runnable_work_units(&units, 0, 3),
            vec!["wu-a".to_string(), "wu-b".to_string(), "wu-c".to_string()]
        );
        assert!(
            runnable_work_units(&units, 3, 3).is_empty(),
            "in_flight が limit に達していれば何も起こさない"
        );
        assert_eq!(
            runnable_work_units(&units, 1, 3).len(),
            2,
            "in_flight の分だけ枠が減る"
        );
    }

    #[test]
    fn runnable_work_units_prefers_needs_continuation_over_ready() {
        let units = vec![
            row_v2("a", "build", 0, WorkUnitStatus::Ready, &[]),
            row_v2("b", "build", 1, WorkUnitStatus::NeedsContinuation, &[]),
        ];
        assert_eq!(
            runnable_work_units(&units, 0, 1),
            vec!["wu-b".to_string()],
            "needs_continuation を先に選ぶ"
        );
    }

    /// D1.3: 対象は現在の工程だけ（前の工程がまだ終わっていなければ、後の工程の ready な WU は
    /// 対象にしない）。
    #[test]
    fn runnable_work_units_only_considers_the_current_phase() {
        let units = vec![
            row_v2("a", "build", 0, WorkUnitStatus::Ready, &[]),
            // `verify` 工程は `build` に依存していないが、工程の境が障壁になる。
            row_v2("z", "verify", 1, WorkUnitStatus::Ready, &[]),
        ];
        assert_eq!(
            runnable_work_units(&units, 0, 5),
            vec!["wu-a".to_string()],
            "build 工程がまだ終わっていないので verify の WU は対象外"
        );

        // build がすべて終われば（is_terminal）、verify が「現在の工程」になる。
        let mut done = units.clone();
        done[0].status = WorkUnitStatus::Done;
        assert_eq!(runnable_work_units(&done, 0, 5), vec!["wu-z".to_string()]);
    }

    /// D1.6: 兄弟が failed/blocked のとき、新しい（ready の）WU は起こさないが、既に走ったことの
    /// ある needs_continuation の WU は続ける。
    #[test]
    fn runnable_work_units_does_not_start_new_ones_when_a_sibling_failed_but_continues_in_flight() {
        let units = vec![
            row_v2("a", "build", 0, WorkUnitStatus::Failed, &[]),
            row_v2("b", "build", 1, WorkUnitStatus::Ready, &[]),
            row_v2("c", "build", 2, WorkUnitStatus::NeedsContinuation, &[]),
        ];
        assert_eq!(
            runnable_work_units(&units, 0, 5),
            vec!["wu-c".to_string()],
            "ready の b は起こさないが、needs_continuation の c は続ける"
        );
    }

    #[test]
    fn runnable_work_units_returns_empty_when_everything_is_terminal() {
        let units = vec![
            row("a", 0, WorkUnitStatus::Done, &[]),
            row("b", 1, WorkUnitStatus::Superseded, &[]),
        ];
        assert!(runnable_work_units(&units, 0, 3).is_empty());
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

    /// ADR-0074 D5.3（Phase F1）: `execution-plan-delta/1` の生成スキーマとコミット済みファイルの一致。
    #[test]
    fn delta_committed_schema_matches_generated() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/protocol/execution-plan-delta.schema.json"
        );
        let generated = serde_json::to_string_pretty(&delta_schema_value()).unwrap() + "\n";
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

    // ---- ADR-0074 D5.3（Phase F1）: replan の差分 (add/modify/remove) ----

    fn delta(
        base_version: u32,
        add: Vec<WorkUnitSpec>,
        modify: Vec<WorkUnitPatch>,
        remove: Vec<&str>,
    ) -> ExecutionPlanDelta {
        ExecutionPlanDelta {
            schema: EXECUTION_PLAN_DELTA_SCHEMA.to_string(),
            base_version,
            rationale: "delta test".to_string(),
            add,
            modify,
            remove: remove.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn apply_delta_adds_modifies_and_removes_without_restating_untouched_units() {
        let base = plan(vec![spec("a", &[]), spec("b", &["a"]), spec("c", &["b"])]);
        let d = delta(
            1,
            vec![spec("m", &[])],
            vec![WorkUnitPatch {
                key: "b".to_string(),
                depends_on: Some(vec!["a".to_string(), "m".to_string()]),
                ..WorkUnitPatch::default()
            }],
            vec!["c"],
        );
        let applied = apply_delta(&base, &d).expect("delta applies");
        let keys: Vec<&str> = applied.work_units.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(keys, vec!["a", "b", "m"], "{keys:?}");
        let b = applied.work_units.iter().find(|w| w.key == "b").unwrap();
        assert_eq!(b.depends_on, vec!["a".to_string(), "m".to_string()]);
        // `a` は modify/remove の対象ではないので、`base` の spec のまま（書き写していない）。
        let a = applied.work_units.iter().find(|w| w.key == "a").unwrap();
        assert_eq!(a, &spec("a", &[]));
    }

    #[test]
    fn apply_delta_rejects_modifying_an_unknown_or_removed_key() {
        let base = plan(vec![spec("a", &[])]);
        let d = delta(
            1,
            vec![],
            vec![WorkUnitPatch {
                key: "ghost".to_string(),
                title: Some("x".to_string()),
                ..WorkUnitPatch::default()
            }],
            vec![],
        );
        assert!(apply_delta(&base, &d).is_err());

        let d2 = delta(
            1,
            vec![],
            vec![WorkUnitPatch {
                key: "a".to_string(),
                title: Some("x".to_string()),
                ..WorkUnitPatch::default()
            }],
            vec!["a"],
        );
        assert!(apply_delta(&base, &d2).is_err(), "removed then modified");
    }

    #[test]
    fn apply_delta_rejects_adding_a_key_that_still_exists() {
        let base = plan(vec![spec("a", &[])]);
        let d = delta(1, vec![spec("a", &[])], vec![], vec![]);
        assert!(apply_delta(&base, &d).is_err());
    }

    // -------------------------------------------------------------------
    // ADR-0074 D1.1（Phase F2 (a)）: `celeris.execution-plan/2` の検証
    // -------------------------------------------------------------------

    /// v1 の計画は 1 バイトも挙動が変わらない（`phases`/`work_units[].phase` を書かない、
    /// 既定の `ExecutionLimits` で通る）。既存の `valid_three_step_plan_passes_and_orders_topologically`
    /// と合わせて、v1 の後方互換を確かめる。
    #[test]
    fn v1_plan_without_phases_still_validates_exactly_as_before() {
        let p = plan(vec![spec("a", &[]), spec("b", &["a"])]);
        assert!(p.phases.is_empty());
        assert!(p.children.is_empty());
        let validated = validate(&p, ExecutionLimits::default(), &[]).expect("valid");
        assert_eq!(validated.spec.work_units[0].phase, None);
    }

    #[test]
    fn v1_rejects_phases_being_set() {
        let mut p = plan(vec![spec("a", &[])]);
        p.phases = vec![phase("build")];
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::PhasesNotAllowedInV1)),
            "{errs:?}"
        );
    }

    #[test]
    fn v1_rejects_a_work_unit_with_a_phase_set() {
        let p = plan(vec![spec_v2("a", "build", &[])]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                PlanValidationError::WorkUnitPhaseNotAllowedInV1 { key } if key == "a"
            )),
            "{errs:?}"
        );
    }

    #[test]
    fn v2_valid_plan_with_parallel_and_stacked_units_passes() {
        let p = plan_v2(
            vec![phase("build"), phase("verify")],
            vec![
                spec_v2("a", "build", &[]),
                spec_v2("b", "build", &[]),
                // 積み上げ（D1.2）: 同じ工程で a に依存。
                spec_v2("c", "build", &["a"]),
                // 前の工程への依存はいくつでもよい（規則 3）。
                spec_v2("d", "verify", &["a", "b", "c"]),
            ],
        );
        let validated = validate(&p, ExecutionLimits::default(), &[]).expect("valid v2 plan");
        assert_eq!(validated.spec.work_units.len(), 4);
    }

    #[test]
    fn v2_rejects_no_phases() {
        let p = plan_v2(vec![], vec![]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::NoPhases)),
            "{errs:?}"
        );
    }

    #[test]
    fn v2_rejects_too_many_phases() {
        let phases: Vec<PhaseSpec> = (0..6).map(|i| phase(&format!("p{i}"))).collect();
        let p = plan_v2(phases, vec![spec_v2("a", "p0", &[])]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::TooManyPhases { count, max } if *count == 6 && *max == 5)),
            "{errs:?}"
        );
    }

    #[test]
    fn v2_rejects_duplicate_phase_keys() {
        let p = plan_v2(
            vec![phase("build"), phase("build")],
            vec![spec_v2("a", "build", &[])],
        );
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter().any(
                |e| matches!(e, PlanValidationError::DuplicatePhaseKey { key } if key == "build")
            ),
            "{errs:?}"
        );
    }

    #[test]
    fn v2_rejects_a_work_unit_missing_its_phase() {
        let p = plan_v2(vec![phase("build")], vec![spec("a", &[])]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter().any(
                |e| matches!(e, PlanValidationError::WorkUnitMissingPhase { key } if key == "a")
            ),
            "{errs:?}"
        );
    }

    #[test]
    fn v2_rejects_a_work_unit_with_an_unknown_phase() {
        let p = plan_v2(vec![phase("build")], vec![spec_v2("a", "ghost-phase", &[])]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                PlanValidationError::UnknownWorkUnitPhase { key, phase } if key == "a" && phase == "ghost-phase"
            )),
            "{errs:?}"
        );
    }

    /// ADR-0074 D1.1 の依存の規則 1: 後の工程への依存は拒否する。
    #[test]
    fn v2_rejects_a_dependency_on_a_later_phase() {
        let p = plan_v2(
            vec![phase("build"), phase("verify")],
            vec![spec_v2("a", "build", &["b"]), spec_v2("b", "verify", &[])],
        );
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                PlanValidationError::DependencyInLaterPhase { key, depends_on }
                    if key == "a" && depends_on == "b"
            )),
            "{errs:?}"
        );
    }

    /// ADR-0074 D1.1 の依存の規則 2: 同じ工程の中の依存は高々 1 つ（鎖か木のみ）。
    #[test]
    fn v2_rejects_two_intra_phase_dependencies() {
        let p = plan_v2(
            vec![phase("build")],
            vec![
                spec_v2("a", "build", &[]),
                spec_v2("b", "build", &[]),
                spec_v2("c", "build", &["a", "b"]),
            ],
        );
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                PlanValidationError::TooManyIntraPhaseDependencies { key, phase }
                    if key == "c" && phase == "build"
            )),
            "{errs:?}"
        );
    }

    /// 同じ工程で 1 つの WU に複数の WU が依存する「木」は許す（規則 2 は「自分の依存の数」を
    /// 数えるのであって、依存されている側の数ではない）。
    #[test]
    fn v2_allows_a_tree_of_intra_phase_dependencies() {
        let p = plan_v2(
            vec![phase("build")],
            vec![
                spec_v2("a", "build", &[]),
                spec_v2("b", "build", &["a"]),
                spec_v2("c", "build", &["a"]),
            ],
        );
        validate(&p, ExecutionLimits::default(), &[]).expect("tree of dependencies is valid");
    }

    #[test]
    fn v2_uses_the_v2_work_unit_limit_not_the_v1_one() {
        let limits = ExecutionLimits::default();
        assert_eq!(limits.max_work_units, 8);
        assert_eq!(limits.max_work_units_v2, 10);
        let units: Vec<WorkUnitSpec> = (0..9)
            .map(|i| spec_v2(&format!("wu{i}"), "build", &[]))
            .collect();
        let p = plan_v2(vec![phase("build")], units);
        validate(&p, limits, &[]).expect("9 work units fit under the v2 limit of 10");
    }

    #[test]
    fn rejects_children_being_non_empty_in_either_version() {
        let mut v1 = plan(vec![spec("a", &[])]);
        v1.children = vec![serde_json::json!({"key": "child"})];
        let errs = validate(&v1, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::NonEmptyChildren)),
            "{errs:?}"
        );

        let mut v2 = plan_v2(vec![phase("build")], vec![spec_v2("a", "build", &[])]);
        v2.children = vec![serde_json::json!({"key": "child"})];
        let errs = validate(&v2, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::NonEmptyChildren)),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_a_work_unit_with_the_reserved_integrate_kind() {
        let mut a = spec("a", &[]);
        a.kind = WorkUnitKind::Integrate;
        let p = plan(vec![a]);
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, PlanValidationError::ReservedKind { key } if key == "a")),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_a_schema_that_is_neither_v1_nor_v2() {
        let mut p = plan(vec![spec("a", &[])]);
        p.schema = "celeris.execution-plan/99".to_string();
        let errs = validate(&p, ExecutionLimits::default(), &[]).unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                PlanValidationError::WrongSchema { found } if found == "celeris.execution-plan/99"
            )),
            "{errs:?}"
        );
    }

    /// v2 の topological order は工程順を tie-break にする（依存の無い WU 同士が工程をまたいでも、
    /// 後の工程が先に選ばれない）。
    #[test]
    fn v2_topological_order_respects_phase_order_for_independent_units() {
        // `z`（verify 工程、依存なし）と `a`（build 工程、依存なし）は互いに依存が無いが、
        // key の昇順だけで tie-break すると `a` より `z` が先に来てしまう対象にした。
        let p = plan_v2(
            vec![phase("build"), phase("verify")],
            vec![spec_v2("z", "verify", &[]), spec_v2("a", "build", &[])],
        );
        let validated = validate(&p, ExecutionLimits::default(), &[]).expect("valid");
        let order: Vec<&str> = validated
            .topological_order
            .iter()
            .map(|&i| validated.spec.work_units[i].key.as_str())
            .collect();
        assert_eq!(order, vec!["a", "z"], "{order:?}");
    }
}
