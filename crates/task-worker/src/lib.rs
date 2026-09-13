//! task-worker: ワーカープロトコル（DESIGN §5.3, ADR-0003）、アダプタ（§5.4）、
//! ワークスペース（§5.8）。ディスパッチ判断はここに書かない（それは task-dispatch）。

pub mod adapter;
pub mod artifact;
pub mod claude_code;
pub mod fake;
pub mod protocol;
pub mod subprocess;
pub mod workspace;

pub use adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
pub use claude_code::{ClaudeCodeAdapter, ClaudeCodeConfig};
pub use fake::FakeAdapter;
pub use protocol::{Evidence, PROTOCOL_VERSION, PriorReview, RunContext, RunRequest, WorkerMessage};
pub use subprocess::{SubprocessSpec, run_subprocess};
pub use workspace::{ExecResult, LocalWorkspace, RemoteWorkspace, Workspace, WorkspaceError};
