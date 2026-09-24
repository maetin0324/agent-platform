//! `celerisctl retry` — Phase 31（実機の事故、2026-09-18）/ ADR-0070 D2（Phase 116）。
//!
//! 引数解析・`task_ops::retry::retry_task` の呼び出し・出力整形だけをここで行う（`cancel.rs` /
//! `rereview.rs` と同じ形）。判断と検証は `task_ops::retry::retry_task`（`failed`/`cancelled` から
//! だけ許す）に移した。`failed`/`cancelled` を複製した**新しいタスク**を作る（`reset_attempts: true`
//! が既定 — 複製先の `attempts` は常に `0`。既存の状態機械は変えない）。
//!
//! ADR-0070 D2 追記（Phase 116。本番で確認: `--accept` を付け忘れると `draft` のまま止まり、
//! 「やり直したのに動かない」状態になった）: **既定で `ready`** から始まる。`draft` のまま
//! 始めたいときだけ `--draft` を付ける（`POST /tasks/{id}/retry` の `accept` の既定と揃える）。

use std::process::ExitCode;

use clap::Args;
use task_core::TaskStore;
use task_ops::retry::retry_task as ops_retry;
use time::OffsetDateTime;

use crate::error::CliError;
use crate::outln;

#[derive(Args, Debug)]
pub struct RetryArgs {
    pub id: String,
    /// 新しいタスクを `draft` のまま始める（既定は `ready`）。
    #[arg(long)]
    pub draft: bool,
}

pub fn run(store: &dyn TaskStore, args: RetryArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;
    let result = ops_retry(store, id, !args.draft, None, OffsetDateTime::now_utc())?;
    outln!("{}", result.task_id);
    if !result.rewired.is_empty() {
        outln!("rewired: {:?}", result.rewired);
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{SqliteStore, TaskId};

    #[test]
    fn run_on_missing_task_returns_message_error() {
        let store = SqliteStore::open_in_memory().expect("open store");
        let missing_id = TaskId::new().to_string();

        let result = run(
            &store,
            RetryArgs {
                id: missing_id,
                draft: false,
            },
        );
        assert!(matches!(result, Err(CliError::Message(_))));
    }

    #[test]
    fn run_on_invalid_id_returns_message_error() {
        let store = SqliteStore::open_in_memory().expect("open store");

        let result = run(
            &store,
            RetryArgs {
                id: "not-a-valid-id".to_string(),
                draft: false,
            },
        );
        assert!(matches!(result, Err(CliError::Message(_))));
    }
}
