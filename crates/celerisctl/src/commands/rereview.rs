//! `celerisctl rereview` — ADR-0051 / ADR-0054 Phase 113 D3。
//!
//! 引数解析・`task_ops::comment::rereview` の呼び出し・出力整形だけをここで行う（`cancel.rs` と同じ
//! 形）。判断と検証は `task_ops::comment::rereview`（`Reviewer` 条件を持つ通常タスクだけ。`failed` は
//! 直前の遷移が `review_fail` のときだけ）に移した。新しい実装 run は起こさない（既存成果を
//! 部署のレビュアーで再判定するだけ）。

use std::process::ExitCode;

use clap::Args;
use task_core::TaskStore;
use task_ops::comment::rereview as ops_rereview;

use crate::error::CliError;
use crate::outln;

#[derive(Args, Debug)]
pub struct RereviewArgs {
    pub id: String,
}

pub fn run(store: &dyn TaskStore, args: RereviewArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;
    let result = ops_rereview(store, id, None)?;
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

        let result = run(&store, RereviewArgs { id: missing_id });
        assert!(matches!(result, Err(CliError::Message(_))));
    }

    #[test]
    fn run_on_invalid_id_returns_message_error() {
        let store = SqliteStore::open_in_memory().expect("open store");

        let result = run(
            &store,
            RereviewArgs {
                id: "not-a-valid-id".to_string(),
            },
        );
        assert!(matches!(result, Err(CliError::Message(_))));
    }
}
