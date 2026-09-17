//! task-worker: ワーカープロトコル（DESIGN §5.3, ADR-0003）、アダプタ（§5.4）、
//! ワークスペース（§5.8）。ディスパッチ判断はここに書かない（それは task-dispatch）。

pub mod adapter;
pub mod artifact;
pub mod claude_account;
pub mod claude_code;
pub mod codex;
pub mod codex_account;
pub mod delegate_file;
pub mod fake;
pub mod protocol;
pub mod provider;
pub mod subprocess;
#[cfg(test)]
pub(crate) mod test_support;
pub mod ssh;
pub mod workspace;

pub use adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
pub use claude_account::{
    AccountCheck, AccountCheckResult, LoginError, LoginOutcome, LoginResult, LoginSession, check_account, start_login,
};
pub use claude_code::{ClaudeCodeAdapter, ClaudeCodeConfig};
pub use codex::{CodexAdapter, CodexConfig};
pub use codex_account::{CodexLoginSession, check_account_codex, start_login_codex};
pub use delegate_file::{DELEGATE_FILE, clear_delegate_file, forward_delegate_file};
pub use fake::FakeAdapter;
pub use protocol::{
    Answer, ChildSummary, Evidence, PROTOCOL_VERSION, PriorReview, ProviderFailure, ReviewOutput, ReviewRequest,
    ReviewVerdictOut, RoleContext, RunContext, RunRequest, WorkerMessage,
};
pub use provider::classify_provider_failure;
pub use subprocess::{SubprocessSpec, run_subprocess};
pub use ssh::{
    SYNC_ALWAYS_EXCLUDED, SshSettings, SshWorkspace, SyncMode, WorktreeSettings, control_master_alive_blocking,
    remote_exec_instructions,
};
pub use workspace::{ExecResult, LocalWorkspace, RemoteWorkspace, Workspace, WorkspaceError};
