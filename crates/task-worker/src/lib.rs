//! task-worker: ワーカープロトコル（DESIGN §5.3, ADR-0003）、アダプタ（§5.4）、
//! ワークスペース（§5.8）。ディスパッチ判断はここに書かない（それは task-dispatch）。

pub mod acp;
pub mod adapter;
/// ADR-0061（Phase 104）: `aider` CLI アダプタ（明確で局所的な少数ファイル修正向け）。
pub mod aider;
pub mod artifact;
/// ADR-0066 D1（Phase 110b）: 同一リポジトリの worktree 間で cargo のビルドキャッシュを共有する。
pub mod build_cache;
pub mod claude_account;
pub mod claude_code;
pub mod cluster_login;
pub mod codex;
pub mod codex_account;
/// ADR-0043 D3（Phase 56）: ハーネスの CLI をコンテナの中で起こす（runtime 検出・包み方・イメージ）。
pub mod container;
pub mod delegate_file;
/// ADR-0060 D1 / Phase 105: celeris の cgroup の外で子プロセスを起こす共通の小道具
/// （`cluster_login.rs` の ssh master と `celeris::releases::start_promote` の両方が使う）。
pub mod detach;
pub mod fake;
/// ADR-0047 D4（Phase 62）: 知識整理 run（`langmem` の memory manager を包む）。
pub mod langmem;
pub mod local_deep_research;
pub mod local_worktree;
pub mod memory;
pub mod paperqa;
pub mod preamble;
/// ADR-0052 D1（Phase 64）: OpenAI 互換エンドポイントの到達性の検査（LLM は呼ばない）。
pub mod probe;
/// ADR-0044 §5 Phase 53 追記（Phase 55）: run の止め方を 1 つにする（プロセスグループごと止める）。
pub mod process_group;
/// ADR-0048 D2（Phase 60a）: 進行の正規化にアダプタが使う共通の小道具（写像はアダプタ側）。
mod progress;
pub mod protocol;
pub mod provider;
pub mod research_targets;
pub mod result_report;
/// ADR-0056 D3（Phase 79）: mount された skills を run にアダプタごとに届ける。
pub mod skills;
pub mod ssh;
pub mod subprocess;
/// ADR-0043 D2 / D4（Phase 52）: タスクの作業場所を複数のリポジトリで組む（worktree とリンク、`setup`）。
pub mod task_repos;
#[cfg(test)]
pub(crate) mod test_support;
pub mod workspace;
/// ADR-0066 D2（Phase 110b）: 終端タスクの作業場所から、ビルド生成物だけを自動で刈る。
pub mod workspace_prune;

pub use acp::{AcpAdapter, AcpConfig, AcpPermission};
pub use adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
pub use aider::{AiderAdapter, AiderConfig};
pub use claude_account::{
    AccountCheck, AccountCheckResult, LoginError, LoginOutcome, LoginResult, LoginSession,
    check_account, start_login,
};
pub use claude_code::{ClaudeCodeAdapter, ClaudeCodeConfig};
pub use cluster_login::{
    ClusterConnectError, ClusterConnectSession, ClusterConnectStart, ClusterMaster, disconnect,
    start_connect,
};
pub use codex::{CodexAdapter, CodexConfig, CodexResumeBypass, CodexResumeMode};
pub use codex_account::{CodexLoginSession, check_account_codex, start_login_codex};
pub use container::{
    ContainerChoice, ContainerPlan, ContainerStop, ContainerStopper, ImageSource, RepoRunInput,
    Runtime, RuntimePreference, RuntimeProbe, SharedPlan,
};
pub use delegate_file::{DELEGATE_FILE_NAME, clear_delegate_file, forward_delegate_file};
pub use fake::FakeAdapter;
pub use langmem::{
    LANGMEM_MISSING_MARKER, LangMemAdapter, LangMemConfig, LangMemProvider,
    knowledge_fallback_instructions,
};
pub use local_deep_research::{EvidenceThresholds, LdrAdapter, LdrConfig, LdrMode};
pub use local_worktree::{
    BaseKind, BaseRef, CleanupOutcome, DEFAULT_BRANCH_PREFIX, LocalWorktree, WORKTREE_DIR_NAME,
    current_release_sha, is_git_repo, resolve_base, status_is_clean,
};
pub use memory::{MEMORY_MAX_CHARS, MemoryDir, MemoryUpdate, read_result_memory};
pub use paperqa::{AcquireConfig, PaperQaAdapter, PaperQaConfig, PaperQaEvidence};
pub use probe::{PROBE_CACHE_TTL, PROBE_TIMEOUT, Reachability, probe_models};
pub use process_group::{ProcessGroup, kill_tree, kill_tree_with};
pub use protocol::{
    ActiveMilestoneContext, ActiveProjectContext, Answer, ChildSummary, ClusterContext,
    CommentContext, ConversationAddressee, ConversationTurn, Evidence, GenreContext,
    GenreRoleContext, MemoryContext, MilestoneBrief, MilestoneReviewContext, MilestoneTaskResult,
    NodeContext, OrgNodeContext, PROTOCOL_VERSION, PriorReview, ProviderFailure, RecentWork,
    ReviewOutput, ReviewRequest, ReviewVerdictOut, RoleContext, RunContext, RunRequest,
    SessionHandle, WorkerMessage,
};
pub use provider::classify_provider_failure;
pub use result_report::{
    ConsoleAction, MilestoneProposal, ParsedActions, ReportDeclaration, actions_from_result_json,
    milestone_proposal_from_result_json, read_result_actions, read_result_milestone_proposal,
    read_result_report_kind, report_kind_from_result_json,
};
pub use ssh::{
    SYNC_ALWAYS_EXCLUDED, SshSettings, SshWorkspace, SyncMode, WorktreeSettings,
    control_master_alive_blocking, remote_dir_is_resolved, remote_exec_instructions,
    resolve_remote_dir,
};
pub use subprocess::{SubprocessSpec, run_subprocess};
pub use task_repos::{
    REPOS_DIR_NAME, SetupOutcome, TaskRepo, TaskWorkspaces, run_setup, run_setup_in,
};
pub use workspace::{ExecResult, LocalWorkspace, RemoteWorkspace, Workspace, WorkspaceError};

pub mod tiered;
