//! task-dispatch: 決定的ディスパッチャ、リース管理、リトライ、並列度制御（DESIGN §5.2）、
//! `ProviderPolicy`（§5.5）、Reviewer（§5.7。`Command`/`ArtifactExists`/`Plan` 検証は決定的、`Reviewer` はアダプタ経由の別 run）。
//! **LLM 呼び出しはここに書かない。**

pub mod accounts;
/// ADR-0033 D5（Phase 26）: `Question` 終端から `approvals` に 1 件作る。
pub(crate) mod approvals;
/// ADR-0072 D8（Phase E1）: daemon が決定的に集める mechanical checkpoint（git の読み取りだけ）。
pub mod checkpoint;
pub mod dispatcher;
/// ADR-0072 D6/D11/D12/D15（Phase E2）: WorkUnit の状態遷移の決定（純粋関数）。
pub mod execution_scheduler;
/// ADR-0074 D1.2 / D1.4（Phase F2）: WU の worktree と工程の統合（daemon 側の git 操作だけ。LLM なし）。
pub mod integration;
pub mod policy;
/// ADR-0033 D3（Phase 25）: run の終端から決定的に作る報告。
pub(crate) mod reports;
pub mod review;
/// ADR-0054 D1（Phase 67）: ノードごとの継続セッションの決定的な判断（純粋関数）。
pub mod sessions;
/// ADR-0067 D3: 未申告の成果物（`artifacts/` の外に書かれた `*.md`）を拾う走査。
pub mod undeclared_artifacts;

pub use accounts::{
    AccountBook, AccountCandidate, AccountCheckRecord, AccountCooldown, AccountCooldownReason,
    AccountDir, AccountEvaluation, AccountState, EXHAUSTED_UTILIZATION, ExcludedReason,
    FIVE_HOUR_SECS, IN_USE_PENALTY, MIN_WEEK_FRACTION, ObservationSource, SEVEN_DAY_SECS,
    cooldown_for_failure, evaluate, scan_accounts, select_account, valid_account_id,
};
pub use dispatcher::{
    AccountsRuntimeConfig, ClusterSpec, ContainerDecision, ContainerRun, ContainersRuntimeConfig,
    DispatchConfig, DispatchError, Dispatcher, ExecutionConfig, KnowledgeRuntimeConfig,
    SnapshotPublisher, TaskFilter, TickReport,
};
pub use policy::{
    AdapterId, ProviderId, ProviderOutcome, ProviderPolicy, ProviderSpec, StaticPolicy,
};
pub use review::{
    PLAN_FILE_NAME, PlanCheck, REVIEW_FILE_NAME, ReviewExtras, ReviewOutcome, ReviewSubject,
    ReviewerProviderFailure, ReviewerRun, Verdict, needs_reviewer_run, review_task, reviewer_hint,
};
pub use sessions::{
    FreshReason, SUPPORTED_ADAPTERS, SessionAction, adapter_supports_sessions, decide, diff_lines,
    summary_lines,
};
pub use task_core::AccountAdapter;
