//! `celerisctl accept` — ADR-0070 D2 追記（Phase 116）。
//!
//! 引数解析・`task_ops::gate::accept` の呼び出し・出力整形だけをここで行う（`cancel.rs` と同じ形）。
//! `draft` だけを `ready` にする（`approve` は `Approval` タスクの承認とも兼用でわかりにくいので、
//! こちらは名前で意図を明確にする専用の道具）。

use std::process::ExitCode;

use clap::Args;
use task_core::TaskStore;
use task_ops::gate::accept as ops_accept;

use crate::error::CliError;
use crate::outln;

#[derive(Args, Debug)]
pub struct AcceptArgs {
    pub id: String,
}

pub fn run(store: &dyn TaskStore, args: AcceptArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;
    let result = ops_accept(store, id, None)?;
    outln!("{:?}", result.to);
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

        let result = run(&store, AcceptArgs { id: missing_id });
        assert!(matches!(result, Err(CliError::Message(_))));
    }

    #[test]
    fn run_on_invalid_id_returns_message_error() {
        let store = SqliteStore::open_in_memory().expect("open store");

        let result = run(
            &store,
            AcceptArgs {
                id: "not-a-valid-id".to_string(),
            },
        );
        assert!(matches!(result, Err(CliError::Message(_))));
    }
}
