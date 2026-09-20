//! task-worker: ワーカープロトコル（DESIGN §5.3, ADR-0003）、アダプタ（§5.4）、
//! ワークスペース（§5.8）。ディスパッチ判断はここに書かない（それは task-dispatch）。

pub mod acp;
pub mod adapter;
pub mod artifact;
pub mod claude_account;
pub mod claude_code;
pub mod cluster_login;
pub mod codex;
pub mod codex_account;
/// ADR-0043 D3（Phase 56）: ハーネスの CLI をコンテナの中で起こす（runtime 検出・包み方・イメージ）。
pub mod container;
pub mod delegate_file;
pub mod fake;
pub mod local_deep_research;
pub mod local_worktree;
pub mod memory;
pub mod paperqa;
pub mod preamble;
/// ADR-0044 §5 Phase 53 追記（Phase 55）: run の止め方を 1 つにする（プロセスグループごと止める）。
pub mod process_group;
/// ADR-0048 D2（Phase 60a）: 進行の正規化にアダプタが使う共通の小道具（写像はアダプタ側）。
mod progress;
pub mod protocol;
pub mod provider;
pub mod result_report;
pub mod ssh;
pub mod subprocess;
/// ADR-0043 D2 / D4（Phase 52）: タスクの作業場所を複数のリポジトリで組む（worktree とリンク、`setup`）。
pub mod task_repos;
#[cfg(test)]
pub(crate) mod test_support;
pub mod workspace;

pub use acp::{AcpAdapter, AcpConfig, AcpPermission};
pub use adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
pub use claude_account::{
    AccountCheck, AccountCheckResult, LoginError, LoginOutcome, LoginResult, LoginSession,
    check_account, start_login,
};
pub use claude_code::{ClaudeCodeAdapter, ClaudeCodeConfig};
pub use cluster_login::{
    ClusterConnectError, ClusterConnectSession, ClusterConnectStart, ClusterMaster, disconnect,
    start_connect,
};
pub use codex::{CodexAdapter, CodexConfig};
pub use codex_account::{CodexLoginSession, check_account_codex, start_login_codex};
pub use container::{
    ContainerChoice, ContainerPlan, ContainerStop, ContainerStopper, ImageSource, RepoRunInput,
    Runtime, RuntimePreference, RuntimeProbe, SharedPlan,
};
pub use delegate_file::{DELEGATE_FILE_NAME, clear_delegate_file, forward_delegate_file};
pub use fake::FakeAdapter;
pub use local_deep_research::{EvidenceThresholds, LdrAdapter, LdrConfig, LdrMode};
pub use local_worktree::{
    BaseKind, BaseRef, CleanupOutcome, DEFAULT_BRANCH_PREFIX, LocalWorktree, WORKTREE_DIR_NAME,
    current_release_sha, is_git_repo, resolve_base, status_is_clean,
};
pub use memory::{MEMORY_MAX_CHARS, MemoryDir, MemoryUpdate, read_result_memory};
pub use paperqa::{AcquireConfig, PaperQaAdapter, PaperQaConfig, PaperQaEvidence};
pub use process_group::{ProcessGroup, kill_tree, kill_tree_with};
pub use protocol::{
    Answer, ChildSummary, CommentContext, ConversationAddressee, ConversationTurn, Evidence,
    GenreContext, GenreRoleContext, MemoryContext, MilestoneBrief, MilestoneReviewContext,
    MilestoneTaskResult, NodeContext, OrgNodeContext, PROTOCOL_VERSION, PriorReview,
    ProviderFailure, RecentWork, ReviewOutput, ReviewRequest, ReviewVerdictOut, RoleContext,
    RunContext, RunRequest, WorkerMessage,
};
pub use provider::classify_provider_failure;
pub use result_report::{
    MilestoneProposal, ReportDeclaration, milestone_proposal_from_result_json,
    read_result_milestone_proposal, read_result_report_kind, report_kind_from_result_json,
};
pub use ssh::{
    SYNC_ALWAYS_EXCLUDED, SshSettings, SshWorkspace, SyncMode, WorktreeSettings,
    control_master_alive_blocking, remote_exec_instructions,
};
pub use subprocess::{SubprocessSpec, run_subprocess};
pub use task_repos::{
    REPOS_DIR_NAME, SetupOutcome, TaskRepo, TaskWorkspaces, run_setup, run_setup_in,
};
pub use workspace::{ExecResult, LocalWorkspace, RemoteWorkspace, Workspace, WorkspaceError};
