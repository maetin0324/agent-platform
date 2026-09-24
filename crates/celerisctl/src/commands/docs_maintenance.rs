//! Repository documentation lifecycle CLI. Canonical changes remain isolated.
use crate::error::CliError;
use clap::Subcommand;
use std::{path::PathBuf, process::ExitCode};
use task_ops::docs_maintenance as docs;

#[derive(Debug, Subcommand)]
pub enum DocsMaintenanceCommand {
    /// Read-only committed documentation inventory and reconciliation proposal.
    Audit {
        repo: PathBuf,
        #[arg(long, default_value = "HEAD")]
        reference: String,
    },
    /// Save a repository policy in Celeris's overlay.
    Adopt { repo_id: String, policy: PathBuf },
    /// Human approval of this exact concrete plan.
    Approve { repo_id: String, plan: PathBuf },
    /// Apply an approved plan to a new isolated worktree (never merge).
    Apply {
        repo: PathBuf,
        repo_id: String,
        plan: PathBuf,
        worktree: PathBuf,
        #[arg(long, default_value = "main")]
        default_branch: String,
    },
}
fn read<T: serde::de::DeserializeOwned>(path: PathBuf) -> Result<T, CliError> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| CliError::msg(e.to_string()))?)
        .map_err(|e| CliError::msg(e.to_string()))
}
pub fn run(command: DocsMaintenanceCommand) -> Result<ExitCode, CliError> {
    let state = docs::state_root();
    let result = match command {
        DocsMaintenanceCommand::Audit { repo, reference } => {
            let audit = docs::audit(&repo, &reference).map_err(CliError::msg)?;
            serde_json::json!({"audit":audit,"proposal":docs::proposal(&audit)})
        }
        DocsMaintenanceCommand::Adopt { repo_id, policy } => {
            docs::save_policy(&state, &repo_id, &read(policy)?).map_err(CliError::msg)?;
            serde_json::json!({"saved":true})
        }
        DocsMaintenanceCommand::Approve { repo_id, plan } => {
            docs::approve_plan(&state, &repo_id, &read(plan)?).map_err(CliError::msg)?;
            serde_json::json!({"approved":true})
        }
        DocsMaintenanceCommand::Apply {
            repo,
            repo_id,
            plan,
            worktree,
            default_branch,
        } => {
            let sha = docs::apply_plan(
                &repo,
                &default_branch,
                &worktree,
                &read(plan)?,
                &state,
                &repo_id,
            )
            .map_err(CliError::msg)?;
            serde_json::json!({"sha":sha,"worktree":worktree,"merged":false})
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|e| CliError::msg(e.to_string()))?
    );
    Ok(ExitCode::SUCCESS)
}
