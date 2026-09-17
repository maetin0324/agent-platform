//! task-core: ドメインモデル、状態機械、イベント、SQLiteストア。
//! DESIGN.md §4-§5.1 のスコープ。LLM呼び出し・サブプロセス起動は行わない（ADR-0001 D2）。

pub mod accounts;
pub mod delegate;
pub mod model;
pub mod plan;
pub mod store;
pub mod transition;

pub use accounts::{AccountAdapter, RateLimitObservation, RateWindow};
pub use delegate::{
    DelegateDep, DelegateError, DelegateTask, DelegationLimits, OnChildFailure, materialize_delegated, validate_each,
};
pub use model::{
    ArtifactRef, Budget, Check, Criterion, Event, Lease, RoleSpec, RunRole, Status, Task, TaskId, TaskKind,
    Tier, Usage, WorkerHint, WorkspaceSpec,
};
pub use plan::{MAX_PLAN_DEPTH, NewTask, NewTaskKind, PlanError, PlanLimits, PlanOutput};
pub use store::{
    EventRow, ListFilter, ListOrder, Page, SCHEMA_VERSION, SqliteStore, StoreError, StoreOptions,
    TaskStore, event_row_schema_value,
};
pub use transition::{InvalidTransition, Outcome, StateView, Trigger, transition};
