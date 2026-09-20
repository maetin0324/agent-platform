//! celerisctl 全コマンド共通のエラー型とヘルパ。

use task_core::{StoreError, TaskId};
use task_ops::OpsError;

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Message(String),
}

impl CliError {
    pub fn msg(s: impl Into<String>) -> Self {
        CliError::Message(s.into())
    }
}

/// `task-ops` のエラーを `CliError` に写す（ADR-0013 D7）。`OpsError::Store` は
/// `CliError::Store` に、それ以外は `Display` の文面をそのまま `CliError::Message` にする
/// （stderr の文面と exit code を変えない）。
impl From<OpsError> for CliError {
    fn from(e: OpsError) -> Self {
        match e {
            OpsError::Store(se) => CliError::Store(se),
            other => CliError::Message(other.to_string()),
        }
    }
}

/// CLI から受け取った文字列を `TaskId` にパースする。失敗はユーザー向けメッセージにする。
pub fn parse_task_id(s: &str) -> Result<TaskId, CliError> {
    s.parse::<TaskId>()
        .map_err(|e| CliError::msg(format!("invalid task id '{s}': {e}")))
}
