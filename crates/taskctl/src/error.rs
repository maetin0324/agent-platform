//! taskctl 全コマンド共通のエラー型とヘルパ。

use task_core::{StoreError, TaskId};

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

/// CLI から受け取った文字列を `TaskId` にパースする。失敗はユーザー向けメッセージにする。
pub fn parse_task_id(s: &str) -> Result<TaskId, CliError> {
    s.parse::<TaskId>()
        .map_err(|e| CliError::msg(format!("invalid task id '{s}': {e}")))
}
