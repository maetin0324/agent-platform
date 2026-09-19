//! task-dispatch: 決定的ディスパッチャ、リース管理、リトライ、並列度制御（DESIGN §5.2）、
//! `ProviderPolicy`（§5.5）、Reviewer（§5.7。`Command`/`ArtifactExists`/`Plan` 検証は決定的、`Reviewer` はアダプタ経由の別 run）。
//! **LLM 呼び出しはここに書かない。**

pub mod accounts;
/// ADR-0033 D5（Phase 26）: `Question` 終端から `approvals` に 1 件作る。
pub(crate) mod approvals;
pub mod dispatcher;
pub mod policy;
/// ADR-0033 D3（Phase 25）: run の終端から決定的に作る報告。
pub(crate) mod reports;
pub mod review;

pub use accounts::{
    AccountBook, AccountCandidate, AccountCheckRecord, AccountCooldown, AccountCooldownReason, AccountDir,
    AccountEvaluation, AccountState, EXHAUSTED_UTILIZATION, ExcludedReason, FIVE_HOUR_SECS, IN_USE_PENALTY,
    MIN_WEEK_FRACTION, ObservationSource, SEVEN_DAY_SECS, cooldown_for_failure, evaluate, scan_accounts,
    select_account, valid_account_id,
};
pub use task_core::AccountAdapter;
pub use dispatcher::{
    AccountsRuntimeConfig, ClusterSpec, DispatchConfig, DispatchError, Dispatcher, SnapshotPublisher, TaskFilter,
    TickReport,
};
pub use policy::{AdapterId, ProviderId, ProviderOutcome, ProviderPolicy, ProviderSpec, StaticPolicy};
pub use review::{
    PLAN_FILE_NAME, PlanCheck, REVIEW_FILE_NAME, ReviewExtras, ReviewOutcome, ReviewSubject, ReviewerProviderFailure, ReviewerRun,
    Verdict, needs_reviewer_run, review_task, reviewer_hint,
};
