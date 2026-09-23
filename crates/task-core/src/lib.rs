//! task-core: ドメインモデル、状態機械、イベント、SQLiteストア。
//! DESIGN.md §4-§5.1 のスコープ。LLM呼び出し・サブプロセス起動は行わない（ADR-0001 D2）。

pub mod delivery;
pub use delivery::{Delivery, DeliveryState, DeliveryStore};
pub mod accounts;
pub mod approval;
pub mod artifacts;
/// ADR-0044 D2（Phase 53）: タスク単位のコメント。
pub mod comment;
pub mod console_action;
pub mod delegate;
/// ADR-0046 D3（Phase 59）: ハーネス = 実行契約（`[[harnesses]]`。旧 `[[genres]]` + `[[roles]]`）。
pub mod harness;
/// ADR-0040 D4（Phase 47）: celeris のインスタンスの役割（`daemon_instances`）。
pub mod instance;
/// ADR-0043 D5（Phase 54）: 変更の取り込みの記録（`task_integrations`）。
pub mod integrations;
/// ADR-0047（Phase 61）: 知識ベース（front matter・索引・検索・マウント。純粋関数だけ）。
pub mod knowledge;
/// ADR-0047 D4（Phase 62）: 知識整理 run の追跡（`knowledge_runs`）。
pub mod knowledge_run;
pub mod message;
/// ADR-0056 D1 / D4（Phase 78）: MCP サーバーの認証（`mcp_clients`）とログ（`mcp_calls`）。
pub mod mcp;
pub mod model;
/// ADR-0064 D1（Phase 110a）: `/proc/self/mountinfo` からマウント点のファイルシステム種別・ソースを
/// 引く純関数（DB がネットワーク越し／loop デバイス上にあることを警告するため）。
pub mod mountinfo;
/// ADR-0054 D1（Phase 67）: ノードごとの継続セッション（`node_sessions`）。
pub mod node_session;
pub mod notify;
pub mod org;
pub mod plan;
/// ADR-0046 D1（Phase 59）: 組織 = Agent Profile の継承木。
pub mod profile;
pub mod report;
/// ADR-0043 D1 / D2（Phase 52）: 案件のリポジトリ（`project_repos`）とタスクの `repos`。
pub mod repos;
pub mod store;
pub mod transition;
/// ADR-0043 D4（Phase 52）: リポジトリの中の設定 `.config/celeris/workspace.toml`。
pub mod workspace_config;

pub use accounts::{AccountAdapter, RateLimitObservation, RateWindow};
pub use approval::{Approval, ApprovalId, ApprovalStore, Decision, StandingRule, StandingRuleId};
pub use artifacts::{
    ARTIFACTS_DIR_NAME, SHARED_ARTIFACTS_PREFIX, artifacts_dir_for, artifacts_rel_for,
    owns_workspace, rel_from,
};
pub use comment::{
    CommentAuthorKind, CommentId, MAX_COMMENT_CHARS, PREAMBLE_COMMENTS, TaskComment,
};
pub use console_action::ConsoleAction;
pub use delegate::{
    DelegateDep, DelegateError, DelegateTask, DelegationLimits, OnChildFailure, WorkspaceContext,
    materialize_delegated, materialize_delegated_logging, validate_each,
};
pub use instance::{DaemonInstance, DaemonMode, InstanceRole, SharedRole};
// ---- ADR-0046 D3（Phase 59）: ハーネスのレジストリ ----
pub use harness::{
    BUILTIN_CONVERSATION, BUILTIN_HARNESSES, BUILTIN_KNOWLEDGE, BUILTIN_PLAN, BUILTIN_REVIEWER,
    BUILTIN_SMOKE, DEFAULT_FALLBACK_TIER, HarnessBudget, HarnessFallback, HarnessFallbackTier,
    HarnessRegistry, HarnessSpec, builtin_harnesses, known_harness_ids,
};
// ---- ADR-0046 D1（Phase 59）: profile の継承木 ----
// `KnowledgeMount` は ADR-0047（Phase 61）の型をそのまま使う（Phase 59 追記）。
pub use profile::resolve as resolve_profile;
pub use profile::{
    CLUSTER_TOOL_PREFIX, COS_ID, COS_NAME, EffectiveProfile, HarnessPrefs, ModelPrefs, Permissions,
    Profile, ProfileError, ProfileRun, ReviewPrefs, TOOL_VOCABULARY, ancestry, is_known_tool,
    is_valid_skill, validate_profile,
};
// ---- ADR-0047（Phase 61）: 知識ベース ----
pub use knowledge::{
    Confidence, Index as KnowledgeIndex, IndexItem as KnowledgeItem, KnowledgeMount, MountKind,
    SearchHit as KnowledgeHit, merge_mounts,
};
// ---- ADR-0047 D4（Phase 62）: 知識整理 run の追跡 ----
pub use knowledge_run::{
    KnowledgeRun, KnowledgeRunState, KnowledgeRunStore, KnowledgeRunSummary, VIA_LANGMEM,
    via_fallback, via_is_fallback,
};
// ---- ADR-0043 D5（Phase 54）: 変更の取り込み ----
pub use integrations::{IntegrationId, IntegrationMethod, IntegrationState, TaskIntegration};
pub use message::{
    CONVERSATION_GENRE, Message, MessageActionFailure, MessageActionResult, MessageId,
    MessageMetadata, MessageRole, conversation_origin, conversation_title, failure_reply,
    is_conversation, is_milestone_review, milestone_review_of,
};
pub use model::{
    ArtifactRef, Budget, Check, Criterion, DEFAULT_PRIORITY, Event, GenreSpec, HARNESS_ADAPTERS,
    Lease, MAX_LABELS, MAX_SKILLS, PRIORITY_LABELS, PROGRESS_DETAIL_MAX_BYTES, ProgressFields,
    ProgressKind, RoleSpec, RunMetrics, RunRole, Status, Task, TaskCategory, TaskId, TaskKind,
    TaskMode, Tier, Usage, WorkerHint, WorkspaceMode, WorkspaceSpec, artifact_entry_description,
    artifact_entry_name, expand_home, home_dir, is_valid_label, normalize_labels, normalize_skills,
    priority_from_label, priority_label, validate_human_checks_have_deliverable,
};
// ---- ADR-0061（Phase 104）: harness routing 基盤（cost 推定・タスク特性ベースの routing）----
pub mod pricing;
pub mod routing;
pub use pricing::estimate_cost_usd;
pub use routing::{RoutingDecision, RoutingPolicy, RoutingSignals, StaticRoutingPolicy};
// ---- ADR-0043 D1 / D2（Phase 52）: 案件のリポジトリ ----
// ---- ADR-0054 D1（Phase 67）: ノードごとの継続セッション ----
pub use node_session::{NodeSession, NodeSessionStore, SessionKind};
// ---- ADR-0056 D1 / D4（Phase 78）: MCP サーバーの認証とログ ----
pub use mcp::{
    McpCall, McpCallStore, McpClient, McpClientStore, McpScope, scopes_from_string,
    scopes_to_string,
};
pub use notify::{
    DEFAULT_WEBHOOK_SECRET_ID, MAX_NOTIFY_ATTEMPTS, Notification, NotificationId, NotificationKind,
    NotificationStore,
};
pub use org::{
    Milestone, MilestoneDecision, MilestoneId, MilestoneStatus, OrgError, OrgKind, OrgNode,
    Project, ProjectId, ProjectStatus, assignee_defaults, department_of, valid_org_id,
    validate_upsert,
};
pub use plan::{
    MAX_PLAN_DEPTH, NewTask, NewTaskKind, PlanError, PlanLimits, PlanOutput, fix_harness_artifacts,
    warn_missing_partial_ok,
};
pub use report::{
    COMPACTION_ROLE, Report, ReportFilter, ReportId, ReportKind, ReportStore, ReportsLive,
    support_kind,
};
pub use repos::{
    ProjectRepo, RepoError, RepoId, RepoKind, RepoRef, RepoRun, RepoSync, default_repo_name,
    resolve_task_repos, valid_repo_name,
};
pub use store::{
    ClusterSettings, EventRow, ListFilter, ListOrder, Page, SCHEMA_VERSION, SqliteStore,
    StoreError, StoreOptions, TaskStore, backup_database, event_row_schema_value, integrity_check,
};
pub use transition::{InvalidTransition, Outcome, StateView, Trigger, transition};

pub mod model_routing;
