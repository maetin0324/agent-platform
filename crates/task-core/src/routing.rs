//! ADR-0061（Phase 104）: coding worker のハーネス routing の骨格。
//!
//! **純粋関数のみ**（I/O・LLM 呼び出しはしない。ADR-0001 D2、DESIGN 原則 1。ディスパッチャや
//! ストアに LLM を持ち込まない、という CLAUDE.md の禁則とも一致する）。`Task` からタスク特性
//! （[`RoutingSignals`]）を決定的に抽出し、固定ルール（[`StaticRoutingPolicy`]）でハーネス候補の
//! 優先順を決める。呼び出し側（`task-ops::add`）は、返ってきた候補のうち**実際に導入済みの
//! アダプタ**だけを `WorkerHint.adapter` に採用し、未導入のハーネス（例: 元の依頼にある aider・
//! pi）が挙がっても黙って現状の既定（`adapter: None` = 供給層の優先順位）にフォールバックする
//! （ADR-0061「未導入ハーネスへの安全なフォールバック」）。
//!
//! 「将来メトリクスから更新できる構造」は [`RoutingPolicy`] という trait 境界で表す。
//! [`StaticRoutingPolicy`] は固定ルールの実装の 1 つに過ぎず、[`MetricsAwareRoutingPolicy`] は
//! 同じ trait を実装したまま「成功率で候補を並べ替える」薄いラッパーの例（Phase 2 で
//! `task-api::stats` の集計値から成功率関数を作って差し込む想定。このラッパー自体は
//! どこからも呼ばれていない「骨格」）。

use crate::model::{Task, TaskMode, Tier};

/// coding worker が選べるハーネス（アダプタ id）の候補。ADR-0061 の分類表に出てくる名前を
/// そのまま使う（`aider` は Phase 104 時点では未実装。[`RoutingDecision`] の doc を参照）。
pub const HARNESS_AIDER: &str = "aider";
pub const HARNESS_MINI_SWE_AGENT: &str = "mini-swe-agent";
pub const HARNESS_ACP: &str = "acp";
pub const HARNESS_CLAUDE_CODE: &str = "claude-code";
pub const HARNESS_CODEX: &str = "codex";

/// タスクから決定的に抽出したタスク特性。`Task` の全フィールドではなく、routing の判断に使う
/// 部分だけを持つ（`Task` を直接渡さないのは、ルールをテストしやすくするため）。
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingSignals {
    /// 題名・目的・受け入れ条件をつなげた文字数（日本語も1文字=1、複雑さの粗い代理指標）。
    pub description_chars: usize,
    pub acceptance_count: usize,
    pub mode: TaskMode,
    pub tier: Tier,
    /// 題名・目的・受け入れ条件のどこかにファイルパスらしき文字列（`.rs`/`.ts`/`.py` 等の
    /// 既知の拡張子、または `/` を含むトークン）が現れる回数。
    pub file_path_hints: usize,
    /// 「再現」「ログ」「traceback」等、単発の issue 調査・shell 中心の作業を示唆する語の出現回数。
    pub isolated_issue_hints: usize,
}

const CODE_EXTENSIONS: [&str; 10] = [
    ".rs", ".ts", ".tsx", ".py", ".go", ".java", ".rb", ".toml", ".yaml", ".yml",
];
const ISOLATED_ISSUE_KEYWORDS: [&str; 8] = [
    "再現",
    "traceback",
    "stack trace",
    "スタックトレース",
    "ログを確認",
    "reproduce",
    "エラーログ",
    "unit test",
];

impl RoutingSignals {
    /// `Task` から決定的に作る（純粋関数。I/O なし）。
    pub fn from_task(task: &Task) -> Self {
        let acceptance_text = task
            .acceptance
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let combined = format!("{}\n{}\n{acceptance_text}", task.title, task.objective);
        let file_path_hints = combined
            .split_whitespace()
            .filter(|tok| CODE_EXTENSIONS.iter().any(|ext| tok.contains(ext)) || tok.contains('/'))
            .count();
        let isolated_issue_hints = ISOLATED_ISSUE_KEYWORDS
            .iter()
            .filter(|kw| combined.contains(*kw))
            .count();
        Self {
            description_chars: combined.chars().count(),
            acceptance_count: task.acceptance.len(),
            mode: task.mode,
            tier: task.worker_hint.tier,
            file_path_hints,
            isolated_issue_hints,
        }
    }
}

/// routing の判断結果。
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingDecision {
    /// 最有力のハーネス id（未導入の可能性がある。呼び出し側が導入済みかを見る）。
    pub primary: String,
    /// `primary` を含む優先順のフォールバック列（最後は必ず [`HARNESS_CLAUDE_CODE`] のような
    /// 汎用・高自律ハーネスで終わる。「候補が尽きたら結局いつもの汎用ハーネスに倒す」という
    /// 安全側の既定）。
    pub candidates: Vec<String>,
    /// 人が読める判断理由（ログ・comment に残す用）。
    pub reason: String,
}

/// ADR-0061: coding worker のハーネス選択方針。固定ルールでも、将来メトリクスを読む実装でも、
/// この trait さえ満たせば差し替えられる。
pub trait RoutingPolicy: Send + Sync {
    fn decide(&self, signals: &RoutingSignals) -> RoutingDecision;
}

/// 元の依頼にある固定ルール（`docs/adr/0061-*.md` の分類表と対応）:
/// - 明確で局所的な少数ファイル修正 → aider 系
/// - isolated issue solving / shell 中心 → mini-swe-agent
/// - 通常の実装・調査・テスト反復 → acp（Pi/OpenCode 等の汎用ハーネス）
/// - 複雑・長時間・高自律 → claude-code（既定のフォールバック終点）
#[derive(Debug, Clone, Copy, Default)]
pub struct StaticRoutingPolicy;

/// 「局所的な少数ファイル修正」の閾値（説明が短く、受け入れ条件が少なく、具体的なファイル名がある）。
const LOCALIZED_MAX_DESCRIPTION_CHARS: usize = 400;
const LOCALIZED_MAX_ACCEPTANCE: usize = 2;
/// 「複雑・長時間・高自律」の閾値。
const COMPLEX_MIN_DESCRIPTION_CHARS: usize = 2000;
const COMPLEX_MIN_ACCEPTANCE: usize = 5;

impl RoutingPolicy for StaticRoutingPolicy {
    fn decide(&self, signals: &RoutingSignals) -> RoutingDecision {
        // production の重い作業・受け入れ条件が多い・説明が長いタスクは、既存の既定どおり
        // 高自律ハーネスに倒す(現状維持。routing 導入で退行させない)。
        let looks_complex = signals.description_chars >= COMPLEX_MIN_DESCRIPTION_CHARS
            || signals.acceptance_count >= COMPLEX_MIN_ACCEPTANCE
            || signals.tier == Tier::Frontier;
        if looks_complex {
            return RoutingDecision {
                primary: HARNESS_CLAUDE_CODE.into(),
                candidates: vec![HARNESS_CLAUDE_CODE.into(), HARNESS_CODEX.into()],
                reason: format!(
                    "complex/long-running task (description_chars={}, acceptance={}, tier={:?}) \
                     → high-autonomy harness",
                    signals.description_chars, signals.acceptance_count, signals.tier
                ),
            };
        }
        let looks_localized = signals.description_chars <= LOCALIZED_MAX_DESCRIPTION_CHARS
            && signals.acceptance_count <= LOCALIZED_MAX_ACCEPTANCE
            && signals.file_path_hints > 0;
        if looks_localized {
            return RoutingDecision {
                primary: HARNESS_AIDER.into(),
                candidates: vec![
                    HARNESS_AIDER.into(),
                    HARNESS_MINI_SWE_AGENT.into(),
                    HARNESS_CLAUDE_CODE.into(),
                ],
                reason: format!(
                    "short, localized edit with an explicit file hint \
                     (description_chars={}, acceptance={}, file_path_hints={}) → aider-style harness",
                    signals.description_chars, signals.acceptance_count, signals.file_path_hints
                ),
            };
        }
        if signals.isolated_issue_hints > 0 {
            return RoutingDecision {
                primary: HARNESS_MINI_SWE_AGENT.into(),
                candidates: vec![
                    HARNESS_MINI_SWE_AGENT.into(),
                    HARNESS_ACP.into(),
                    HARNESS_CLAUDE_CODE.into(),
                ],
                reason: format!(
                    "isolated issue / shell-centric wording detected (hits={}) → mini-swe-agent",
                    signals.isolated_issue_hints
                ),
            };
        }
        RoutingDecision {
            primary: HARNESS_ACP.into(),
            candidates: vec![
                HARNESS_ACP.into(),
                HARNESS_CLAUDE_CODE.into(),
                HARNESS_CODEX.into(),
            ],
            reason: "ordinary implement/investigate/test loop → general-purpose harness".into(),
        }
    }
}

/// ADR-0061「将来メトリクスから更新できる構造」の骨格。`success_rate(adapter)` が `Some(rate)`
/// (`0.0..=1.0`) を返せば `base` の候補列をその降順で並べ替える。値が無いハーネス(`None`)は
/// 0.5 扱い（判断材料が無いことを「悪い」とみなさない）。Phase 104 時点ではどこからも呼ばれない
/// デモ実装（Phase 2 で `task-api::stats` の集計から `success_rate` を作って差し込む想定）。
pub struct MetricsAwareRoutingPolicy<F>
where
    F: Fn(&str) -> Option<f64> + Send + Sync,
{
    base: StaticRoutingPolicy,
    success_rate: F,
}

impl<F> MetricsAwareRoutingPolicy<F>
where
    F: Fn(&str) -> Option<f64> + Send + Sync,
{
    pub fn new(success_rate: F) -> Self {
        Self {
            base: StaticRoutingPolicy,
            success_rate,
        }
    }
}

impl<F> RoutingPolicy for MetricsAwareRoutingPolicy<F>
where
    F: Fn(&str) -> Option<f64> + Send + Sync,
{
    fn decide(&self, signals: &RoutingSignals) -> RoutingDecision {
        let mut decision = self.base.decide(signals);
        decision.candidates.sort_by(|a, b| {
            let ra = (self.success_rate)(a).unwrap_or(0.5);
            let rb = (self.success_rate)(b).unwrap_or(0.5);
            rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(best) = decision.candidates.first() {
            decision.primary = best.clone();
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Criterion;

    fn signals(
        description_chars: usize,
        acceptance_count: usize,
        file_path_hints: usize,
        isolated_issue_hints: usize,
        tier: Tier,
    ) -> RoutingSignals {
        RoutingSignals {
            description_chars,
            acceptance_count,
            mode: TaskMode::default(),
            tier,
            file_path_hints,
            isolated_issue_hints,
        }
    }

    #[test]
    fn a_short_localized_edit_with_a_file_hint_routes_to_aider() {
        let policy = StaticRoutingPolicy;
        let decision = policy.decide(&signals(120, 1, 1, 0, Tier::Standard));
        assert_eq!(decision.primary, HARNESS_AIDER);
        assert_eq!(decision.candidates.last().unwrap(), HARNESS_CLAUDE_CODE);
    }

    #[test]
    fn isolated_issue_wording_routes_to_mini_swe_agent() {
        let policy = StaticRoutingPolicy;
        // 短くはあるが、ファイルパスの手がかりが無いので localized 判定にはならない。
        let decision = policy.decide(&signals(150, 1, 0, 1, Tier::Standard));
        assert_eq!(decision.primary, HARNESS_MINI_SWE_AGENT);
    }

    #[test]
    fn a_long_or_frontier_task_routes_to_claude_code_regardless_of_other_hints() {
        let policy = StaticRoutingPolicy;
        let decision = policy.decide(&signals(3000, 1, 1, 1, Tier::Standard));
        assert_eq!(decision.primary, HARNESS_CLAUDE_CODE);
        let decision = policy.decide(&signals(50, 1, 1, 0, Tier::Frontier));
        assert_eq!(decision.primary, HARNESS_CLAUDE_CODE);
    }

    #[test]
    fn an_ordinary_task_routes_to_the_general_purpose_harness() {
        let policy = StaticRoutingPolicy;
        let decision = policy.decide(&signals(800, 3, 0, 0, Tier::Standard));
        assert_eq!(decision.primary, HARNESS_ACP);
    }

    #[test]
    fn signals_from_task_extracts_file_hints_and_keywords_deterministically() {
        use crate::model::{Budget, Check, TaskId, TaskKind, WorkerHint, WorkspaceSpec};
        use time::OffsetDateTime;

        let now = OffsetDateTime::now_utc();
        let task = Task {
            mode: TaskMode::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "小さな修正".into(),
            objective: "crates/task-core/src/model.rs の typo を直す".into(),
            acceptance: vec![Criterion {
                text: "typo が直っている".into(),
                check: Check::Human,
            }],
            inputs: vec![],
            depends_on: vec![],
            status: crate::model::Status::Draft,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "/tmp/workspace".into(),
                mode: None,
            },
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
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            labels: vec![],
            category: Default::default(),
            conversation: None,
        };
        let signals = RoutingSignals::from_task(&task);
        assert_eq!(signals.file_path_hints, 1);
        assert_eq!(signals.acceptance_count, 1);
    }

    /// ADR-0061: metrics-aware ラッパーは同じ trait のまま候補の並びだけ変える（骨格の実演）。
    #[test]
    fn metrics_aware_policy_reorders_candidates_by_success_rate() {
        let base_decision = StaticRoutingPolicy.decide(&signals(120, 1, 1, 0, Tier::Standard));
        assert_eq!(base_decision.primary, HARNESS_AIDER);

        let rates = |adapter: &str| -> Option<f64> {
            match adapter {
                "aider" => Some(0.1),
                "mini-swe-agent" => Some(0.95),
                _ => None,
            }
        };
        let policy = MetricsAwareRoutingPolicy::new(rates);
        let decision = policy.decide(&signals(120, 1, 1, 0, Tier::Standard));
        assert_eq!(decision.primary, HARNESS_MINI_SWE_AGENT);
        assert_eq!(decision.candidates[0], HARNESS_MINI_SWE_AGENT);
    }
}
