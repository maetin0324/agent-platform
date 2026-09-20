//! task-core: ドメインモデル、状態機械、イベント、SQLiteストア。
//! DESIGN.md §4-§5.1 のスコープ。LLM呼び出し・サブプロセス起動は行わない（ADR-0001 D2）。

pub mod accounts;
pub mod approval;
pub mod artifacts;
/// ADR-0044 D2（Phase 53）: タスク単位のコメント。
pub mod comment;
pub mod delegate;
/// ADR-0040 D4（Phase 47）: celeris のインスタンスの役割（`daemon_instances`）。
pub mod instance;
/// ADR-0043 D5（Phase 54）: 変更の取り込みの記録（`task_integrations`）。
pub mod integrations;
pub mod message;
pub mod model;
pub mod notify;
pub mod org;
pub mod plan;
pub mod report;
/// ADR-0043 D1 / D2（Phase 52）: 案件のリポジトリ（`project_repos`）とタスクの `repos`。
pub mod repos;
pub mod store;
pub mod transition;
/// ADR-0043 D4（Phase 52）: リポジトリの中の設定 `.config/celeris/workspace.toml`。
pub mod workspace_config;

pub use accounts::{AccountAdapter, RateLimitObservation, RateWindow};
pub use artifacts::{ARTIFACTS_DIR_NAME, SHARED_ARTIFACTS_PREFIX, artifacts_dir_for, artifacts_rel_for, owns_workspace, rel_from};
pub use approval::{Approval, ApprovalId, ApprovalStore, Decision, StandingRule, StandingRuleId};
pub use delegate::{
    DelegateDep, DelegateError, DelegateTask, DelegationLimits, OnChildFailure, WorkspaceContext,
    materialize_delegated, validate_each,
};
pub use comment::{
    CommentAuthorKind, CommentId, MAX_COMMENT_CHARS, PREAMBLE_COMMENTS, TaskComment,
};
pub use instance::{DaemonInstance, DaemonMode, InstanceRole, SharedRole};
// ---- ADR-0043 D5（Phase 54）: 変更の取り込み ----
pub use integrations::{IntegrationId, IntegrationMethod, IntegrationState, TaskIntegration};
pub use message::{
    CONVERSATION_GENRE, Message, MessageId, MessageRole, conversation_origin, conversation_title, failure_reply,
    is_conversation, is_milestone_review, milestone_review_of,
};
pub use model::{
    ArtifactRef, Budget, Check, Criterion, DEFAULT_PRIORITY, Event, GenreSpec, HARNESS_ADAPTERS, Lease, MAX_LABELS,
    PRIORITY_LABELS, RoleSpec, RunRole, Status, Task, TaskCategory, TaskId, TaskKind, Tier, Usage, WorkerHint,
    WorkspaceMode, WorkspaceSpec, artifact_entry_description, artifact_entry_name, expand_home, home_dir,
    is_valid_label, normalize_labels, priority_from_label, priority_label,
};
// ---- ADR-0043 D1 / D2（Phase 52）: 案件のリポジトリ ----
pub use repos::{
    ProjectRepo, RepoError, RepoId, RepoKind, RepoRef, RepoRun, RepoSync, default_repo_name, resolve_task_repos,
    valid_repo_name,
};
pub use notify::{
    DEFAULT_WEBHOOK_SECRET_ID, MAX_NOTIFY_ATTEMPTS, Notification, NotificationId, NotificationKind,
    NotificationStore,
};
pub use org::{
    Milestone, MilestoneDecision, MilestoneId, MilestoneStatus, OrgError, OrgKind, OrgNode, Project, ProjectId,
    ProjectStatus, assignee_defaults, department_of, valid_org_id, validate_upsert,
};
pub use report::{
    COMPACTION_ROLE, Report, ReportFilter, ReportId, ReportKind, ReportStore, ReportsLive, support_kind,
};
pub use plan::{
    MAX_PLAN_DEPTH, NewTask, NewTaskKind, PlanError, PlanLimits, PlanOutput, fix_harness_artifacts,
};
pub use store::{
    EventRow, ListFilter, ListOrder, Page, SCHEMA_VERSION, SqliteStore, StoreError, StoreOptions,
    TaskStore, event_row_schema_value,
};
pub use transition::{InvalidTransition, Outcome, StateView, Trigger, transition};
