//! task-worker: ワーカープロトコル（DESIGN §5.3, ADR-0003）、アダプタ（§5.4）、
//! ワークスペース（§5.8）。ディスパッチ判断はここに書かない（それは task-dispatch）。

pub mod adapter;
pub mod artifact;
pub mod claude_code;
pub mod codex;
pub mod fake;
pub mod protocol;
pub mod provider;
pub mod subprocess;
#[cfg(test)]
pub(crate) mod test_support;
pub mod ssh;
pub mod workspace;

pub use adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
pub use claude_code::{ClaudeCodeAdapter, ClaudeCodeConfig};
pub use codex::{CodexAdapter, CodexConfig};
pub use fake::FakeAdapter;
pub use protocol::{
    Answer, Evidence, PROTOCOL_VERSION, PriorReview, ProviderFailure, ReviewOutput, ReviewRequest, ReviewVerdictOut,
    RunContext, RunRequest, WorkerMessage,
};
pub use provider::classify_provider_failure;
pub use subprocess::{SubprocessSpec, run_subprocess};
pub use ssh::{SshSettings, SshWorkspace, SyncMode, control_master_alive_blocking};
pub use workspace::{ExecResult, LocalWorkspace, RemoteWorkspace, Workspace, WorkspaceError};
