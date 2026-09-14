//! `task-ops` 全体で使うエラー型（ADR-0013 D7）。
//!
//! `Display` は現在の `taskctl` の各コマンドのエラー文面をそのまま保つ（挙動を変えない）。
//! `taskctl` 側は `OpsError` を `CliError` に写し、stderr の文面と exit code を変えない。

use task_core::{Status, StoreError, TaskId};

#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    /// 指定した `TaskId` が存在しない。
    #[error("task not found: {0}")]
    NotFound(TaskId),

    /// 現在の状態では要求された操作ができない。
    /// `context` は `kind=.., status=..` や `status=..`、`action` は `approved` や
    /// `answered; only blocked tasks accept an answer` のように、元のメッセージの
    /// `cannot be <action>` 部分をそのまま埋め込む。
    #[error("task {id} ({context}) cannot be {action}")]
    InvalidState {
        id: TaskId,
        context: String,
        action: String,
    },

    /// 入力の検証エラー（受け入れ条件が無い、依存先が不正など）。
    #[error("{0}")]
    Validation(String),

    /// 呼び出し側が期待した `Status` と現在の `Status` が食い違う（`expected` 引数付き呼び出し）。
    #[error("expected status {expected:?} but task has status {actual:?}")]
    Conflict { expected: Status, actual: Status },

    #[error(transparent)]
    Store(#[from] StoreError),
}
