//! Read-only protocol smoke check: no inference turn and no credential output.
//! cargo run -p task-worker --example codex_usage -- <CODEX_HOME> [codex-command]

use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!("usage: codex_usage <CODEX_HOME> [codex-command]");
        return std::process::ExitCode::from(2);
    };
    let command = args.next().unwrap_or_else(|| "codex".into());
    let check = task_worker::check_account_codex(
        &command,
        &PathBuf::from(root),
        Duration::from_secs(30),
        &[],
    )
    .await;
    println!(
        "{}",
        serde_json::json!({
            "result": format!("{:?}", check.result),
            "detail": check.detail,
            "usage": check.observation,
        })
    );
    if check.result == task_worker::AccountCheckResult::Ok {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
