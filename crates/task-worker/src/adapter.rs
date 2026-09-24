//! `WorkerAdapter` / `EventSink`（DESIGN §5.4, ADR-0003, ADR-0005 D1/D4）。
//! アダプタはワーカー固有の事情を閉じ込め、結果を `RunOutcome` に正規化して返す。
//! 状態遷移の判断はアダプタでは行わない（task-dispatch の責務）。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use task_core::{ArtifactRef, ProgressFields, RateLimitObservation, Usage};

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
    /// ADR-0048 D2（Phase 60a）: 構造化した進行（`kind` / `tool` / `summary` / `detail`）。
    /// 既定は `msg` だけを `progress` に流す（この口を実装していないシンクでも従来どおり動く）。
    /// アダプタごとの写像はアダプタ側にあり、ここには何の判断も無い。
    fn progress_with(&self, msg: &str, _fields: &ProgressFields) {
        self.progress(msg);
    }
    /// パス検査と sha256 計算済みの成果物（`crate::artifact::resolve` を通したもの）。
    fn artifact(&self, artifact: &ArtifactRef);
    /// ワーカーの stdout から 1 行読むたびにアダプタが呼ぶ生存通知。ディスパッチャはこれでリースを延長する
    /// （ADR-0010 D7, P-7）。既定は何もしない。
    fn heartbeat(&self) {}
    /// `delegate` メッセージ（LLM アダプタでは `artifacts/delegate.json`）の提案（ADR-0016 D2）。ディスパッチャが検証して
    /// 子タスクを挿入する。既定は何もしない（`celerisctl worker run` など DB を変えない文脈）。
    fn delegate(&self, _tasks: &[task_core::DelegateTask]) {}
    /// ADR-0044 D2（Phase 53）: `{"type":"comment","body":"…"}`（任意回、非終端）。ディスパッチャは
    /// `task_comments` に `author_kind = node` で残す。既定は何もしない（`celerisctl worker run` など
    /// DB を変えない文脈）。
    fn comment(&self, _body: &str) {}
    /// claude-code アダプタが stream-json の `rate_limit_event` を解析するたびに呼ぶ（ADR-0024 D4）。
    /// ディスパッチャはプールのアカウントで走っている run のシンクから、この値を `AccountBook` に記録する。
    /// 既定は何もしない（`fake` アダプタや `celerisctl worker run` の観測用途など）。
    fn rate_limit(&self, _obs: RateLimitObservation) {}
    /// ADR-0054 D1（Phase 67）: アダプタが**実際に使った／割り当てられた**セッション id を報告する
    /// （高々 1 回。claude-code は `--session-id`/`--resume` に渡した値そのもの、codex は
    /// `thread.started` で観測した thread id、acp は `session/new`/`session/load` の応答の
    /// `sessionId`）。ディスパッチャはこれを `node_sessions` に記録する（新規なら作る、確認なら触らない）。
    /// `context.session` が無い run では呼ばれない。既定は何もしない（`fake` アダプタ・
    /// `celerisctl worker run` など DB を変えない文脈）。
    fn session_established(&self, _session_id: &str) {}
    /// ADR-0054 D1（Phase 67）: `context.session` で resume を頼んだのに、アダプタがそのセッションを
    /// 拒否した（見つからない・失効した）ことを報告する（高々 1 回）。ディスパッチャはこれを見て
    /// `node_sessions` の該当行を retire する（次の run からは新しいセッションになる。ADR-0054 D1
    /// 「失敗も同じ経路で作り直す」）。この run 自身の成否には関わらない（`RunOutcome` は通常どおり返す）。
    /// 既定は何もしない。
    fn session_resume_failed(&self, _reason: &str) {}
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
    fn account_id(&self) -> Option<&str> {
        None
    }
    fn model_for_tier(&self, _tier: task_core::Tier) -> Result<Option<String>, String> {
        Ok(None)
    }
    /// ADR-0069 D4（Phase 114）: その lane の設定上の reasoning effort（監査記録用。既定は無し）。
    fn reasoning_effort_for_tier(&self, _tier: task_core::Tier) -> Option<String> {
        None
    }
    fn with_model(&self, _model: &str) -> Option<Arc<dyn WorkerAdapter>> {
        None
    }

    async fn run(
        &self,
        req: RunRequest,
        run_id: &str,
        limits: RunLimits,
        sink: &dyn EventSink,
    ) -> Result<RunOutcome, AdapterError>;

    /// `extra` を環境の末尾に重ねた複製を返す（ADR-0024 D2: celeris の環境 < アダプタの環境 < プロバイダの環境 <
    /// アカウント）。プール（`account_pool = true`）で選んだアカウントの `CLAUDE_SECURESTORAGE_CONFIG_DIR` を
    /// 足すために使う。既定は `None`（この経路をサポートしないアダプタ）。
    fn with_env(&self, _extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        None
    }

    /// ADR-0043 D3（Phase 56）: このアダプタが起こすコマンドを**コンテナの中で**走らせる複製を返す。
    /// 差し込み点は `crate::container::wrap` の 1 関数だけで、アダプタは受け取った計画を
    /// `Command` を組み立てた直後に渡すだけである（stdio 契約は変わらない）。
    ///
    /// 既定は `None` = **この経路を持たないアダプタ**（`paperqa` / `local-deep-research` は道具立てが
    /// ホストの venv にあるので、そもそも `container::decide` がコンテナを選ばない）。
    /// `None` が返ったらディスパッチャはホストで走らせる。
    fn with_container(
        &self,
        _plan: crate::container::SharedPlan,
    ) -> Option<Arc<dyn WorkerAdapter>> {
        None
    }
}
