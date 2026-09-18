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
pub mod delegate_file;
pub mod fake;
pub mod local_deep_research;
pub mod memory;
pub mod paperqa;
pub mod preamble;
pub mod protocol;
pub mod provider;
pub mod result_report;
pub mod subprocess;
#[cfg(test)]
pub(crate) mod test_support;
pub mod ssh;
pub mod workspace;

pub use acp::{AcpAdapter, AcpConfig, AcpPermission};
pub use adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
pub use claude_account::{
    AccountCheck, AccountCheckResult, LoginError, LoginOutcome, LoginResult, LoginSession, check_account, start_login,
};
pub use claude_code::{ClaudeCodeAdapter, ClaudeCodeConfig};
pub use cluster_login::{
    ClusterConnectError, ClusterConnectSession, ClusterConnectStart, ClusterMaster, disconnect, start_connect,
};
pub use codex::{CodexAdapter, CodexConfig};
pub use codex_account::{CodexLoginSession, check_account_codex, start_login_codex};
pub use delegate_file::{DELEGATE_FILE, clear_delegate_file, forward_delegate_file};
pub use fake::FakeAdapter;
pub use local_deep_research::{EvidenceThresholds, LdrAdapter, LdrConfig, LdrMode};
pub use memory::{MEMORY_MAX_CHARS, MemoryDir, MemoryUpdate, read_result_memory};
pub use paperqa::{AcquireConfig, PaperQaAdapter, PaperQaConfig, PaperQaEvidence};
pub use protocol::{
    Answer, ChildSummary, ConversationAddressee, ConversationTurn, Evidence, GenreContext, GenreRoleContext,
    MemoryContext, NodeContext, OrgNodeContext, PROTOCOL_VERSION, PriorReview, ProviderFailure, RecentWork,
    ReviewOutput, ReviewRequest, ReviewVerdictOut, RoleContext, RunContext, RunRequest, WorkerMessage,
};
pub use provider::classify_provider_failure;
pub use result_report::{ReportDeclaration, read_result_report_kind, report_kind_from_result_json};
pub use subprocess::{SubprocessSpec, run_subprocess};
pub use ssh::{
    SYNC_ALWAYS_EXCLUDED, SshSettings, SshWorkspace, SyncMode, WorktreeSettings, control_master_alive_blocking,
    remote_exec_instructions,
};
pub use workspace::{ExecResult, LocalWorkspace, RemoteWorkspace, Workspace, WorkspaceError};
