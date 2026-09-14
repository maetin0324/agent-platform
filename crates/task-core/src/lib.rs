//! task-core: ドメインモデル、状態機械、イベント、SQLiteストア。
//! DESIGN.md §4-§5.1 のスコープ。LLM呼び出し・サブプロセス起動は行わない（ADR-0001 D2）。

pub mod model;
pub mod plan;
pub mod store;
pub mod transition;

pub use model::{
    ArtifactRef, Budget, Check, Criterion, Event, Lease, Status, Task, TaskId, TaskKind, Tier,
    Usage, WorkerHint, WorkspaceSpec,
};
pub use plan::{MAX_PLAN_DEPTH, NewTask, NewTaskKind, PlanError, PlanLimits, PlanOutput};
pub use store::{SqliteStore, StoreError, TaskStore};
pub use transition::{InvalidTransition, Outcome, StateView, Trigger, transition};
