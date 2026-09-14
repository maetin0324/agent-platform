//! `WorkerAdapter` / `EventSink`（DESIGN §5.4, ADR-0003, ADR-0005 D1/D4）。
//! アダプタはワーカー固有の事情を閉じ込め、結果を `RunOutcome` に正規化して返す。
//! 状態遷移の判断はアダプタでは行わない（task-dispatch の責務）。

use std::time::Duration;

use async_trait::async_trait;
use task_core::{ArtifactRef, Usage};

use crate::protocol::{Evidence, ProviderFailure, RunRequest};

/// run の終端結果（ADR-0003 D3）。タイムアウト・終端無し exit も `Error{retryable:true}` に正規化する（D4）。
#[derive(Debug, Clone, PartialEq)]
pub enum Terminal {
    Done {
        summary: String,
        evidence: Vec<Evidence>,
        usage: Option<Usage>,
    },
    Question {
        text: String,
    },
    Error {
        message: String,
        retryable: bool,
    },
}

/// アダプタが返す run の結果。
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    pub terminal: Terminal,
    /// サブプロセスの exit code（取れた場合）。状態遷移には使わない（ADR-0003 D3）。
    pub exit_code: Option<i32>,
}

/// 生存監視の上限（ADR-0003 D4）。`wall_clock` は `task.budget.max_wall_secs`、
/// `idle_timeout` / `kill_grace` はアダプタ設定から。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunLimits {
    pub wall_clock: Duration,
    pub idle_timeout: Duration,
    pub kill_grace: Duration,
}

/// アダプタ内部の失敗（プロトコル上の `error` ではなく、起動不能など）。
/// ディスパッチャは `WorkerError{retryable:true}` に写し、`Throttled`/`AuthFailed`/`Exhausted` は
/// `ProviderPolicy::report` にも渡す（ADR-0005 D4/D6）。
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("failed to spawn worker: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("provider throttled (retry after {retry_after:?})")]
    Throttled { retry_after: Duration },
    #[error("provider auth failed: {0}")]
    AuthFailed(String),
    #[error("provider exhausted: {0}")]
    Exhausted(String),
    #[error("{0}")]
    Other(String),
}

impl AdapterError {
    /// `error.provider_failure`（プロトコル）や、CLI 系アダプタのエラー文面の分類結果を `AdapterError` に写す
    /// （ADR-0010 D5）。遷移の判断はディスパッチャが行う。
    pub fn from_provider_failure(failure: ProviderFailure, message: &str) -> Self {
        match failure {
            // `retry_after_secs: 0` で毎 tick 再 dispatch されるホットループを避けるため最低 1 秒（Phase 7 監査）。
            ProviderFailure::Throttled { retry_after_secs } => AdapterError::Throttled {
                retry_after: Duration::from_secs(retry_after_secs.max(1)),
            },
            ProviderFailure::AuthFailed => AdapterError::AuthFailed(message.to_string()),
            ProviderFailure::Exhausted => AdapterError::Exhausted(message.to_string()),
        }
    }
}

/// run 途中のイベント受け口。ディスパッチャがストアへ `WorkerProgress` / `ArtifactProduced` を追記する。
/// 同期 API（ストアは `Mutex<Connection>` で直列化されるため）。
pub trait EventSink: Send + Sync {
    fn progress(&self, msg: &str);
    /// パス検査と sha256 計算済みの成果物（`crate::artifact::resolve` を通したもの）。
    fn artifact(&self, artifact: &ArtifactRef);
    /// ワーカーの stdout から 1 行読むたびにアダプタが呼ぶ生存通知。ディスパッチャはこれでリースを延長する
    /// （ADR-0010 D7, P-7）。既定は何もしない。
    fn heartbeat(&self) {}
}

/// 何もしないシンク（テスト・デバッグ用）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl EventSink for NullSink {
    fn progress(&self, _msg: &str) {}
    fn artifact(&self, _artifact: &ArtifactRef) {}
}

/// DESIGN §5.4 `trait WorkerAdapter`。`run_id` は成果物・ログのひも付け用（`runs/<run_id>/`）。
#[async_trait]
pub trait WorkerAdapter: Send + Sync {
    /// アダプタ識別子（設定の `adapter` と一致。例: `"fake"`）。
    fn id(&self) -> &str;
    async fn run(
        &self,
        req: RunRequest,
        run_id: &str,
        limits: RunLimits,
        sink: &dyn EventSink,
    ) -> Result<RunOutcome, AdapterError>;
}
