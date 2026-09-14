//! `taskctl` — DESIGN.md §5.9 の CLI。ADR-0004 参照。

mod commands;
mod error;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use task_core::{SqliteStore, TaskStore};

use commands::add::{self, AddArgs};
use commands::gate::{self, AnswerArgs, ApproveArgs, RejectArgs};
use commands::plan::{self, PlanArgs};
use commands::query::{self, LogArgs, LsArgs, ShowArgs};
use commands::replay::{self, ReplayArgs};
use error::CliError;

#[derive(Parser, Debug)]
#[command(name = "taskctl", about = "taskd task control CLI (DESIGN.md §5.9)")]
struct Cli {
    /// SQLite データベースファイルのパス（ADR-0004 D5）。
    /// 優先順位: --db > 環境変数 TASKD_DB > ./taskd.sqlite3
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Add(AddArgs),
    Plan(PlanArgs),
    Ls(LsArgs),
    Show(ShowArgs),
    Approve(ApproveArgs),
    Reject(RejectArgs),
    Answer(AnswerArgs),
    Log(LogArgs),
    Replay(ReplayArgs),
}

fn resolve_db_path(cli_db: Option<PathBuf>) -> PathBuf {
    cli_db
        .or_else(|| env::var_os("TASKD_DB").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("taskd.sqlite3"))
}

fn dispatch(store: &dyn TaskStore, command: Command) -> Result<ExitCode, CliError> {
    match command {
        Command::Add(args) => add::run(store, args),
        Command::Plan(args) => plan::run(store, args),
        Command::Ls(args) => query::run_ls(store, args),
        Command::Show(args) => query::run_show(store, args),
        Command::Approve(args) => gate::run_approve(store, args),
        Command::Reject(args) => gate::run_reject(store, args),
        Command::Answer(args) => gate::run_answer(store, args),
        Command::Log(args) => query::run_log(store, args),
        Command::Replay(args) => replay::run(store, args),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let db_path = resolve_db_path(cli.db);

    let store = match SqliteStore::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to open db {}: {e}", db_path.display());
            return ExitCode::FAILURE;
        }
    };

    match dispatch(&store, cli.command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
