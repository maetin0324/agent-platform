//! `celerisctl approve` / `celerisctl reject` / `celerisctl answer` — DESIGN.md §5.9 / ADR-0002 D4 /
//! ADR-0004 D1-D3 / ADR-0010 D3。
//!
//! 引数解析・`task_ops::gate` の呼び出し・出力整形だけをここで行う。判断と検証は
//! `task_ops::gate`（ADR-0013 D7）に移した。CLI にはまだ `--expected` は無いので、常に
//! `expected = None` で呼ぶ（挙動は変えない）。

use std::process::ExitCode;

use clap::Args;
use task_core::TaskStore;
use task_ops::gate::{answer as ops_answer, approve as ops_approve, reject as ops_reject};

use crate::error::CliError;
use crate::outln;

#[derive(Args, Debug)]
pub struct ApproveArgs {
    pub id: String,

    #[arg(long)]
    pub note: Option<String>,
}

#[derive(Args, Debug)]
pub struct RejectArgs {
    pub id: String,

    #[arg(long)]
    pub note: Option<String>,
}

#[derive(Args, Debug)]
pub struct AnswerArgs {
    pub id: String,

    pub answer: String,
}

pub fn run_approve(store: &dyn TaskStore, args: ApproveArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;
    let result = ops_approve(store, id, args.note, None)?;
    outln!("{:?}", result.to);
    Ok(ExitCode::SUCCESS)
}

pub fn run_reject(store: &dyn TaskStore, args: RejectArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;
    let result = ops_reject(store, id, args.note, None)?;
    outln!("{:?}", result.to);
    Ok(ExitCode::SUCCESS)
}

pub fn run_answer(store: &dyn TaskStore, args: AnswerArgs) -> Result<ExitCode, CliError> {
    let id = crate::error::parse_task_id(&args.id)?;
    let result = ops_answer(store, id, args.answer, None)?;
    outln!(
        "answer recorded; task {} moved to {:?}",
        result.id,
        result.to
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_approve_on_missing_task_returns_message_error() {
        let store = task_core::SqliteStore::open_in_memory().expect("open store");
        let missing_id = task_core::TaskId::new().to_string();

        let result = run_approve(
            &store,
            ApproveArgs {
                id: missing_id,
                note: None,
            },
        );
        assert!(matches!(result, Err(CliError::Message(_))));
    }

    #[test]
    fn run_answer_on_missing_task_returns_message_error() {
        let store = task_core::SqliteStore::open_in_memory().expect("open store");
        let missing_id = task_core::TaskId::new().to_string();

        let result = run_answer(
            &store,
            AnswerArgs {
                id: missing_id,
                answer: "irrelevant".to_string(),
            },
        );
        assert!(matches!(result, Err(CliError::Message(_))));
    }
}
