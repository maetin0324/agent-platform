//! ドメインモデル型。DESIGN.md §4 を実装する。純粋なデータ定義のみで、
//! I/O・LLM呼び出し・プロセス起動を行うロジックはここに置かない（ADR-0001 D2）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use ulid::Ulid;

/// タスクの一意識別子（ULID）。DESIGN §4.1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaskId(pub Ulid);

impl TaskId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for TaskId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_string(s)?))
    }
}

/// DESIGN §4.1 の `TaskKind`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Plan,
    Execute,
    Review,
    Approval,
}

/// ADR-0002 D1 の状態集合。終端は `done | failed | cancelled`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Draft,
    Ready,
    Running,
    Blocked,
    Reviewing,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    /// ADR-0002 D1: 終端状態 = `done | failed | cancelled`。
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Done | Status::Failed | Status::Cancelled)
    }
}

/// DESIGN §5.4 の `WorkerHint`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Frontier,
    Standard,
    Cheap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerHint {
    pub tier: Tier,
    pub adapter: Option<String>,
}

/// DESIGN §5.8 の境界。`Remote` は接続層プロジェクトが実装するまで型のみ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceSpec {
    Local { path: PathBuf },
    Remote { cluster: String, path: PathBuf },
}

/// DESIGN §4.1 の `Budget`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub max_turns: u32,
    pub max_wall_secs: u64,
    pub max_retries: u32,
}

/// DESIGN §4.1 の `Lease`。ADR-0002 D7: `expires_at` は
/// `budget.max_wall_secs + 猶予` から `acquire_lease` 呼び出し側が計算する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub worker_run_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// DESIGN §5.3/§5.7 の `Check` 種別。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Check {
    Command { cmd: String, expect_exit: i32 },
    ArtifactExists { name: String },
    Reviewer,
    Human,
}

/// DESIGN §4.1 の `Criterion`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Criterion {
    pub text: String,
    pub check: Check,
}

/// DESIGN §4.4 の `ArtifactRef`。実体は `workspace/<task_id>/artifacts/` 配下。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub name: String,
    pub path: String,
    pub sha256: String,
    pub kind: String,
}

/// DESIGN §4.1 の `Task`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub parent_id: Option<TaskId>,
    pub kind: TaskKind,
    pub title: String,
    pub objective: String,
    pub acceptance: Vec<Criterion>,
    pub inputs: Vec<ArtifactRef>,
    pub depends_on: Vec<TaskId>,
    pub status: Status,
    pub priority: i32,
    pub worker_hint: WorkerHint,
    pub workspace: WorkspaceSpec,
    pub budget: Budget,
    pub attempts: u32,
    pub lease: Option<Lease>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// DESIGN §5.3 の `usage`。取れない項目は省略可。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// DESIGN §4.3 の `Event`（追記専用）。ADR-0002 D2: `Transitioned` は遷移の
/// *結果* を記録するものであり、`transition()` の入力（`Trigger`）とは別物。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Created {
        task: Box<Task>,
    },
    Transitioned {
        from: Status,
        to: Status,
        reason: String,
    },
    WorkerStarted {
        run_id: String,
        adapter: String,
        model: String,
    },
    WorkerProgress {
        run_id: String,
        msg: String,
    },
    ArtifactProduced {
        run_id: String,
        artifact: ArtifactRef,
    },
    WorkerFinished {
        run_id: String,
        outcome: String,
        usage: Option<Usage>,
    },
    ReviewVerdict {
        run_id: String,
        criterion_idx: usize,
        pass: bool,
        reason: String,
    },
    ApprovalRequested,
    ApprovalDecided {
        by: String,
        approved: bool,
        note: Option<String>,
    },
    ProviderThrottled {
        provider: String,
        #[serde(with = "time::serde::rfc3339")]
        until: OffsetDateTime,
    },
}
