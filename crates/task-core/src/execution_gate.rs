//! ADR-0072 D13（Phase E3）: Complexity Gate（atomic / compound の決定的な判定）。
//!
//! 純粋なデータ定義と純粋関数だけを置く（I/O・LLM 呼び出しはしない。ADR-0001 D2）。gate 自体は
//! LLM を使わない（決定的な規則表。ADR-0072 D4 / D13）。`TaskFeatures` は
//! `task_core::model_policy::TaskFeatures::infer_with_hints` の結果をそのまま渡してもらう
//! （既存の 9 軸を再利用し、足りない信号だけ [`ExecutionGateInputs`] で補う）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{Check, Task, TaskKind, WorkspaceMode};
use crate::model_policy::{Level, TaskFeatures};

/// D13: gate の policy 版（監査記録に残す）。
pub const EXECUTION_GATE_POLICY_VERSION: &str = "exec-gate/1";
/// D13: `score >= threshold` なら compound。
pub const EXECUTION_GATE_SCORE_THRESHOLD: i32 = 5;

/// 固定パイプラインの harness（`literature` = paperqa、`web-research` = LDR、`knowledge` = langmem）。
/// D13 の対象外規則。
const FIXED_PIPELINE_GENRES: &[&str] = &["literature", "web-research", "knowledge"];

/// D13: atomic / compound の判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Atomic,
    Compound,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionMode::Atomic => "atomic",
            ExecutionMode::Compound => "compound",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "atomic" => Some(ExecutionMode::Atomic),
            "compound" => Some(ExecutionMode::Compound),
            _ => None,
        }
    }
}

/// D13: `ExecutionGateDecision.source`。人の明示 > 規則表（CoS のヒントが効いたかどうかは
/// `Hint` として残す。`Hint` でも実際に mode を決めるのは規則表のスコアである点に注意
/// — 人の明示だけが規則表そのものをバイパスする）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GateSource {
    Policy,
    Human,
    Hint,
}

/// D13: 当たった信号 1 件（規則表の行、または強制規則・対象外規則）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GateSignal {
    pub name: String,
    pub weight: i32,
    pub detail: String,
}

/// D13: `Event::ExecutionGated` の中身、および `Task.routing.execution`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionGateDecision {
    pub mode: ExecutionMode,
    pub source: GateSource,
    pub score: i32,
    pub threshold: i32,
    pub rule_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<GateSignal>,
    pub policy_version: String,
    /// `[execution] gate = "shadow"` のときの判定なら `true`（記録だけで実行には使わない。D13）。
    pub shadow: bool,
}

/// `NewTaskSpec.execution` / CoS の `create_task.execution` から `Task.routing.execution_hint` に運ぶ値。
/// `explicit = true` は人（API/CLI）の明示（gate をバイパスする）、`false` は CoS のヒント
/// （signal `H` として +2 されるだけ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionHintSpec {
    pub mode: ExecutionMode,
    pub explicit: bool,
}

/// gate が dispatcher/store から読む必要がある信号（S4/S6）。純粋関数のままにするため、
/// 呼び出し側が決定的に計算して渡す。
#[derive(Debug, Clone, Copy, Default)]
pub struct ExecutionGateInputs {
    /// S4: 複数の実行環境（remote workspace と local repos の混在、または skills が 2 部署以上にまたがる）。
    pub multi_environment: bool,
    /// S6: 同じ担当ノード・同じ genre の直近終端タスク（14 日以内、最大 20 件）のうち、
    /// `budget_exhausted` の run/continuation を持ったものの割合（0.0..=1.0）。無ければ `None`。
    pub recent_budget_exhausted_ratio: Option<f64>,
}

/// D13: gate の対象外なら理由（rule_id）を返す。`kind != Execute` / 対話 / support-task /
/// `routing` を持たない旧タスク / 固定パイプラインの harness / `workspace_mode = Shared` の内部タスク。
pub fn out_of_scope_rule(task: &Task) -> Option<&'static str> {
    if task.kind != TaskKind::Execute {
        return Some("atomic/out-of-scope");
    }
    if crate::message::is_conversation(task) {
        return Some("atomic/out-of-scope");
    }
    if crate::report::support_kind(task).is_some() {
        return Some("atomic/out-of-scope");
    }
    if task.routing.is_none() {
        return Some("atomic/out-of-scope");
    }
    if task
        .genre
        .as_deref()
        .is_some_and(|g| FIXED_PIPELINE_GENRES.contains(&g))
    {
        return Some("atomic/out-of-scope");
    }
    if task.workspace.local_mode() == WorkspaceMode::Shared
        || task.workspace.remote_mode() == WorkspaceMode::Shared
    {
        return Some("atomic/out-of-scope");
    }
    None
}

/// 題名・目的に出てくる工程語の種類（S1）。
const PROCESS_WORD_GROUPS: &[&[&str]] = &[
    &["調査", "investigate", "survey"],
    &["設計", "design"],
    &["実装", "implement"],
    &["テスト", "test"],
    &["review", "レビュー"],
    &["release", "リリース", "deploy", "配送"],
];

fn count_process_stages(text: &str) -> usize {
    let lower = text.to_lowercase();
    PROCESS_WORD_GROUPS
        .iter()
        .filter(|group| group.iter().any(|w| lower.contains(&w.to_lowercase())))
        .count()
}

struct Scorer {
    score: i32,
    signals: Vec<GateSignal>,
}

impl Scorer {
    fn new() -> Self {
        Scorer {
            score: 0,
            signals: Vec::new(),
        }
    }

    fn add(&mut self, name: &str, weight: i32, detail: String) {
        if weight != 0 {
            self.score += weight;
            self.signals.push(GateSignal {
                name: name.to_string(),
                weight,
                detail,
            });
        }
    }
}

/// D13: Complexity Gate。`out_of_scope_rule` に当たれば常に atomic。次に人の明示
/// （`human_execution`）があればそれに従う（規則表を評価しない）。それ以外は規則表のスコアで決める。
/// `shadow` はそのまま `ExecutionGateDecision.shadow` に写す（判定のロジックそのものは変えない。
/// `gate = "shadow"` でも同じ判定をし、採用するかどうかは呼び出し側が決める）。
pub fn decide(
    task: &Task,
    features: &TaskFeatures,
    human_execution: Option<ExecutionMode>,
    cos_hint_compound: bool,
    inputs: ExecutionGateInputs,
    shadow: bool,
) -> ExecutionGateDecision {
    if let Some(rule_id) = out_of_scope_rule(task) {
        return ExecutionGateDecision {
            mode: ExecutionMode::Atomic,
            source: GateSource::Policy,
            score: 0,
            threshold: EXECUTION_GATE_SCORE_THRESHOLD,
            rule_id: rule_id.to_string(),
            signals: Vec::new(),
            policy_version: EXECUTION_GATE_POLICY_VERSION.to_string(),
            shadow,
        };
    }
    if let Some(mode) = human_execution {
        return ExecutionGateDecision {
            mode,
            source: GateSource::Human,
            score: 0,
            threshold: EXECUTION_GATE_SCORE_THRESHOLD,
            rule_id: "human/explicit".to_string(),
            signals: Vec::new(),
            policy_version: EXECUTION_GATE_POLICY_VERSION.to_string(),
            shadow,
        };
    }

    let mut s = Scorer::new();
    match features.context_size {
        Level::High => s.add("F1", 2, "context_size=high".to_string()),
        Level::Medium => s.add("F1", 1, "context_size=medium".to_string()),
        Level::Low => {}
    }
    if features.expected_length == Level::High {
        s.add("F2", 2, "expected_length=high".to_string());
    }
    if features.tool_intensity == Level::High {
        s.add("F3", 1, "tool_intensity=high".to_string());
    }
    match features.cross_cutting {
        Level::High => s.add("F4", 2, "cross_cutting=high".to_string()),
        Level::Medium => s.add("F4", 1, "cross_cutting=medium".to_string()),
        Level::Low => {}
    }
    if features.judgment == Level::High && features.tool_intensity >= Level::Medium {
        s.add(
            "F5",
            1,
            "judgment=high and tool_intensity>=medium".to_string(),
        );
    }
    let text = format!("{}\n{}", task.title, task.objective);
    let stages = count_process_stages(&text);
    match stages {
        n if n >= 3 => s.add("S1", 2, format!("{n} process stages mentioned")),
        2 => s.add("S1", 1, "2 process stages mentioned".to_string()),
        _ => {}
    }
    let artifact_exists = task
        .acceptance
        .iter()
        .filter(|c| matches!(c.check, Check::ArtifactExists { .. }))
        .count();
    if task.acceptance.len() >= 6 || artifact_exists >= 3 {
        s.add(
            "S2",
            1,
            format!(
                "acceptance={}, artifact_exists={artifact_exists}",
                task.acceptance.len()
            ),
        );
    }
    let has_human = task.acceptance.iter().any(|c| c.check == Check::Human);
    let has_command = task
        .acceptance
        .iter()
        .any(|c| matches!(c.check, Check::Command { .. }));
    if has_human && has_command {
        s.add(
            "S3",
            1,
            "has both Check::Human and Check::Command".to_string(),
        );
    }
    if inputs.multi_environment {
        s.add("S4", 1, "multiple execution environments".to_string());
    }
    let objective_len = task.objective.chars().count();
    if objective_len > 2000 {
        s.add("S5", 1, format!("objective length {objective_len} > 2000"));
    }
    if let Some(ratio) = inputs.recent_budget_exhausted_ratio
        && ratio >= 0.3
    {
        s.add(
            "S6",
            2,
            format!("recent budget_exhausted ratio {ratio:.2} >= 0.3"),
        );
    }
    if cos_hint_compound {
        s.add("H", 2, "CoS hinted execution=compound".to_string());
    }

    let source = if cos_hint_compound {
        GateSource::Hint
    } else {
        GateSource::Policy
    };

    // 強制規則（D13）: `expected_length=high` かつ `cross_cutting=high` なら compound。
    if features.expected_length == Level::High && features.cross_cutting == Level::High {
        return ExecutionGateDecision {
            mode: ExecutionMode::Compound,
            source,
            score: s.score,
            threshold: EXECUTION_GATE_SCORE_THRESHOLD,
            rule_id: "compound/long-and-broad".to_string(),
            signals: s.signals,
            policy_version: EXECUTION_GATE_POLICY_VERSION.to_string(),
            shadow,
        };
    }
    // 強制規則: `max_turns <= 10` かつ目的が 400 文字未満なら atomic。
    if task.budget.max_turns <= 10 && objective_len < 400 {
        return ExecutionGateDecision {
            mode: ExecutionMode::Atomic,
            source,
            score: s.score,
            threshold: EXECUTION_GATE_SCORE_THRESHOLD,
            rule_id: "atomic/small".to_string(),
            signals: s.signals,
            policy_version: EXECUTION_GATE_POLICY_VERSION.to_string(),
            shadow,
        };
    }

    let (mode, rule_id) = if s.score >= EXECUTION_GATE_SCORE_THRESHOLD {
        (ExecutionMode::Compound, "compound/score")
    } else {
        (ExecutionMode::Atomic, "atomic/score")
    };
    ExecutionGateDecision {
        mode,
        source,
        score: s.score,
        threshold: EXECUTION_GATE_SCORE_THRESHOLD,
        rule_id: rule_id.to_string(),
        signals: s.signals,
        policy_version: EXECUTION_GATE_POLICY_VERSION.to_string(),
        shadow,
    }
}

/// `[execution] gate`。既定は `shadow`（ADR-0072 D13）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GateMode {
    Off,
    #[default]
    Shadow,
    On,
}

impl GateMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GateMode::Off => "off",
            GateMode::Shadow => "shadow",
            GateMode::On => "on",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(GateMode::Off),
            "shadow" => Some(GateMode::Shadow),
            "on" => Some(GateMode::On),
            _ => None,
        }
    }
}

/// `[execution.planner]`（D14）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerConfig {
    pub adapter: String,
    pub permission_mode: String,
    pub max_turns: u32,
    pub max_wall_secs: u64,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        PlannerConfig {
            adapter: "claude-code".to_string(),
            permission_mode: "plan".to_string(),
            max_turns: 40,
            max_wall_secs: 1200,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ArtifactRef, Budget, Criterion, Status, TaskCategory, TaskId, TaskMode, TaskRouting, Tier,
        WorkerHint, WorkspaceSpec,
    };

    fn base_task() -> Task {
        let now = time::OffsetDateTime::UNIX_EPOCH;
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "小さな修正".to_string(),
            objective: "typo を直す".to_string(),
            acceptance: vec![Criterion {
                text: "直っている".to_string(),
                check: Check::Command {
                    cmd: "true".to_string(),
                    expect_exit: 0,
                },
            }],
            inputs: Vec::<ArtifactRef>::new(),
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

    fn features(overrides: impl FnOnce(&mut TaskFeatures)) -> TaskFeatures {
        let mut f = TaskFeatures {
            judgment: Level::Low,
            ambiguity: Level::Low,
            verifiability: Level::High,
            reversibility: Level::High,
            consequence: Level::Low,
            context_size: Level::Low,
            tool_intensity: Level::Low,
            expected_length: Level::Low,
            cross_cutting: Level::Low,
        };
        overrides(&mut f);
        f
    }

    fn no_inputs() -> ExecutionGateInputs {
        ExecutionGateInputs::default()
    }

    #[test]
    fn tiny_task_with_no_signals_is_atomic_by_the_small_rule() {
        let task = base_task();
        let f = features(|_| {});
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert_eq!(d.mode, ExecutionMode::Atomic);
        assert_eq!(d.rule_id, "atomic/small");
        assert_eq!(d.score, 0);
    }

    #[test]
    fn each_weighted_signal_contributes_its_documented_weight() {
        let mut task = base_task();
        task.budget.max_turns = 40; // avoid the atomic/small forced rule
        task.objective = "テスト対象を確かめる".repeat(10); // avoid triggering S1/S5 accidentally beyond expectations

        // F1 high (+2)
        let f = features(|f| f.context_size = Level::High);
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert!(d.signals.iter().any(|s| s.name == "F1" && s.weight == 2));

        // F1 medium (+1)
        let f = features(|f| f.context_size = Level::Medium);
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert!(d.signals.iter().any(|s| s.name == "F1" && s.weight == 1));

        // F2 (+2)
        let f = features(|f| f.expected_length = Level::High);
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert!(d.signals.iter().any(|s| s.name == "F2" && s.weight == 2));

        // F3 (+1)
        let f = features(|f| f.tool_intensity = Level::High);
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert!(d.signals.iter().any(|s| s.name == "F3" && s.weight == 1));

        // F4 high (+2) / medium (+1)
        let f = features(|f| f.cross_cutting = Level::High);
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert!(d.signals.iter().any(|s| s.name == "F4" && s.weight == 2));
        let f = features(|f| f.cross_cutting = Level::Medium);
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert!(d.signals.iter().any(|s| s.name == "F4" && s.weight == 1));

        // F5: judgment high and tool_intensity >= medium (+1)
        let f = features(|f| {
            f.judgment = Level::High;
            f.tool_intensity = Level::Medium;
        });
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert!(d.signals.iter().any(|s| s.name == "F5" && s.weight == 1));
        // judgment high alone (tool_intensity low) must not trigger F5.
        let f = features(|f| f.judgment = Level::High);
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert!(!d.signals.iter().any(|s| s.name == "F5"));

        // S1: process stage words in title/objective.
        let mut three_stages = task.clone();
        three_stages.objective = "調査してから設計し、実装する".to_string();
        let d = decide(&three_stages, &f, None, false, no_inputs(), false);
        assert!(d.signals.iter().any(|s| s.name == "S1" && s.weight == 2));
        let mut two_stages = task.clone();
        two_stages.objective = "調査してから設計する".to_string();
        let d = decide(
            &two_stages,
            &features(|_| {}),
            None,
            false,
            no_inputs(),
            false,
        );
        assert!(d.signals.iter().any(|s| s.name == "S1" && s.weight == 1));

        // S2: acceptance count >= 6.
        let mut many_acceptance = task.clone();
        many_acceptance.acceptance = (0..6)
            .map(|i| Criterion {
                text: format!("cond {i}"),
                check: Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            })
            .collect();
        let d = decide(
            &many_acceptance,
            &features(|_| {}),
            None,
            false,
            no_inputs(),
            false,
        );
        assert!(d.signals.iter().any(|s| s.name == "S2"));

        // S3: both Human and Command checks present.
        let mut mixed = task.clone();
        mixed.acceptance = vec![
            Criterion {
                text: "a".into(),
                check: Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            },
            Criterion {
                text: "b".into(),
                check: Check::Human,
            },
        ];
        let d = decide(&mixed, &features(|_| {}), None, false, no_inputs(), false);
        assert!(d.signals.iter().any(|s| s.name == "S3" && s.weight == 1));

        // S4: multi environment input.
        let d = decide(
            &task,
            &features(|_| {}),
            None,
            false,
            ExecutionGateInputs {
                multi_environment: true,
                recent_budget_exhausted_ratio: None,
            },
            false,
        );
        assert!(d.signals.iter().any(|s| s.name == "S4" && s.weight == 1));

        // S5: objective length > 2000.
        let mut long_objective = task.clone();
        long_objective.objective = "x".repeat(2001);
        let d = decide(
            &long_objective,
            &features(|_| {}),
            None,
            false,
            no_inputs(),
            false,
        );
        assert!(d.signals.iter().any(|s| s.name == "S5" && s.weight == 1));
        let mut short_objective = task.clone();
        short_objective.objective = "x".repeat(2000);
        let d = decide(
            &short_objective,
            &features(|_| {}),
            None,
            false,
            no_inputs(),
            false,
        );
        assert!(!d.signals.iter().any(|s| s.name == "S5"));

        // S6: recent budget_exhausted ratio threshold (0.3).
        let d = decide(
            &task,
            &features(|_| {}),
            None,
            false,
            ExecutionGateInputs {
                multi_environment: false,
                recent_budget_exhausted_ratio: Some(0.3),
            },
            false,
        );
        assert!(d.signals.iter().any(|s| s.name == "S6" && s.weight == 2));
        let d = decide(
            &task,
            &features(|_| {}),
            None,
            false,
            ExecutionGateInputs {
                multi_environment: false,
                recent_budget_exhausted_ratio: Some(0.29),
            },
            false,
        );
        assert!(!d.signals.iter().any(|s| s.name == "S6"));

        // H: CoS hint.
        let d = decide(&task, &features(|_| {}), None, true, no_inputs(), false);
        assert!(d.signals.iter().any(|s| s.name == "H" && s.weight == 2));
        assert_eq!(d.source, GateSource::Hint);
    }

    #[test]
    fn score_threshold_boundary_is_five() {
        let mut task = base_task();
        task.budget.max_turns = 40;
        // Craft exactly score 4 (below threshold): F1 high(+2) + F4 medium(+1) + F3(+1) = 4.
        let f = features(|f| {
            f.context_size = Level::High;
            f.cross_cutting = Level::Medium;
            f.tool_intensity = Level::High;
        });
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert_eq!(d.score, 4);
        assert_eq!(d.mode, ExecutionMode::Atomic);
        assert_eq!(d.rule_id, "atomic/score");

        // Add F2 (+2) to cross 5.
        let f = features(|f| {
            f.context_size = Level::High;
            f.cross_cutting = Level::Medium;
            f.tool_intensity = Level::High;
            f.expected_length = Level::High;
        });
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert_eq!(d.score, 6);
        assert_eq!(d.mode, ExecutionMode::Compound);
        assert_eq!(d.rule_id, "compound/score");
    }

    #[test]
    fn forced_rule_long_and_broad_overrides_low_score() {
        let mut task = base_task();
        task.budget.max_turns = 90;
        task.budget.max_wall_secs = 4000;
        let f = features(|f| {
            f.expected_length = Level::High;
            f.cross_cutting = Level::High;
        });
        let d = decide(&task, &f, None, false, no_inputs(), false);
        assert_eq!(d.mode, ExecutionMode::Compound);
        assert_eq!(d.rule_id, "compound/long-and-broad");
    }

    #[test]
    fn forced_rule_small_overrides_score() {
        let mut task = base_task();
        task.budget.max_turns = 10;
        task.objective = "短い".to_string();
        // Even with a hint that would otherwise push toward compound, the atomic/small
        // forced rule fires first because objective is short and max_turns is tiny.
        let d = decide(&task, &features(|_| {}), None, true, no_inputs(), false);
        assert_eq!(d.mode, ExecutionMode::Atomic);
        assert_eq!(d.rule_id, "atomic/small");
    }

    #[test]
    fn out_of_scope_tasks_are_always_atomic() {
        let mut plan_kind = base_task();
        plan_kind.kind = TaskKind::Plan;
        assert_eq!(out_of_scope_rule(&plan_kind), Some("atomic/out-of-scope"));

        let mut conversation = base_task();
        conversation.conversation = Some(crate::message::MessageId::new());
        assert_eq!(
            out_of_scope_rule(&conversation),
            Some("atomic/out-of-scope")
        );

        let mut no_routing = base_task();
        no_routing.routing = None;
        assert_eq!(out_of_scope_rule(&no_routing), Some("atomic/out-of-scope"));

        let mut fixed_pipeline = base_task();
        fixed_pipeline.genre = Some("literature".to_string());
        assert_eq!(
            out_of_scope_rule(&fixed_pipeline),
            Some("atomic/out-of-scope")
        );

        let mut shared_ws = base_task();
        shared_ws.workspace = WorkspaceSpec::Local {
            path: "/tmp/x".into(),
            mode: Some(WorkspaceMode::Shared),
        };
        assert_eq!(out_of_scope_rule(&shared_ws), Some("atomic/out-of-scope"));

        // An ordinary in-scope task has no out-of-scope rule.
        assert_eq!(out_of_scope_rule(&base_task()), None);

        // out_of_scope always wins even with a very high score / compound hint.
        let d = decide(
            &plan_kind,
            &features(|f| {
                f.expected_length = Level::High;
                f.cross_cutting = Level::High;
            }),
            Some(ExecutionMode::Compound),
            true,
            ExecutionGateInputs {
                multi_environment: true,
                recent_budget_exhausted_ratio: Some(1.0),
            },
            false,
        );
        assert_eq!(d.mode, ExecutionMode::Atomic);
        assert_eq!(d.rule_id, "atomic/out-of-scope");
    }

    #[test]
    fn human_explicit_beats_the_rule_table_and_the_hint() {
        let mut task = base_task();
        task.budget.max_turns = 40;
        // Score would be high (compound by score), but a human explicit atomic wins.
        let f = features(|f| {
            f.expected_length = Level::High;
            f.context_size = Level::High;
        });
        let d = decide(
            &task,
            &f,
            Some(ExecutionMode::Atomic),
            true,
            no_inputs(),
            false,
        );
        assert_eq!(d.mode, ExecutionMode::Atomic);
        assert_eq!(d.source, GateSource::Human);
        assert_eq!(d.rule_id, "human/explicit");
        assert!(d.signals.is_empty());

        task.title = "小さな修正".to_string();
        task.objective = "typo".to_string();
        let d = decide(
            &task,
            &features(|_| {}),
            Some(ExecutionMode::Compound),
            false,
            no_inputs(),
            false,
        );
        assert_eq!(d.mode, ExecutionMode::Compound);
        assert_eq!(d.source, GateSource::Human);
    }

    #[test]
    fn shadow_flag_is_recorded_without_changing_the_decision() {
        let task = base_task();
        let d = decide(&task, &features(|_| {}), None, false, no_inputs(), true);
        assert!(d.shadow);
        let d2 = decide(&task, &features(|_| {}), None, false, no_inputs(), false);
        assert_eq!(d.mode, d2.mode);
        assert_eq!(d.rule_id, d2.rule_id);
        assert!(!d2.shadow);
    }

    #[test]
    fn gate_mode_parses_and_round_trips() {
        assert_eq!(GateMode::parse("off"), Some(GateMode::Off));
        assert_eq!(GateMode::parse("shadow"), Some(GateMode::Shadow));
        assert_eq!(GateMode::parse("on"), Some(GateMode::On));
        assert_eq!(GateMode::parse("bogus"), None);
        assert_eq!(GateMode::default(), GateMode::Shadow);
    }
}
