//! `celerisctl` — DESIGN.md §5.9 の CLI。ADR-0004 参照。

mod commands;
mod error;
mod output;

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use task_core::SqliteStore;

use commands::add::{self, AddArgs};
use commands::cancel::{self, CancelArgs};
use commands::config::{self as config_cmd, ConfigCommand};
use commands::gate::{self, AnswerArgs, ApproveArgs, RejectArgs};
use commands::org::{self as org_cmd, OrgCommand};
use commands::plan::{self, PlanArgs};
use commands::query::{self, LogArgs, LsArgs, ShowArgs};
use commands::replay::{self, ReplayArgs};
use commands::worker::{self, WorkerCommand};
use error::CliError;

#[derive(Parser, Debug)]
#[command(name = "celerisctl", about = "celeris task control CLI (DESIGN.md §5.9)")]
struct Cli {
    /// SQLite データベースファイルのパス（ADR-0004 D5）。
    /// 優先順位: --db > 環境変数 CELERIS_DB > ./celeris.sqlite3
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
    Cancel(CancelArgs),
    Answer(AnswerArgs),
    Log(LogArgs),
    Replay(ReplayArgs),
    /// `celerisctl worker run` 等（デバッグ用。ADR-0012 D4）。
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
    },
    /// ADR-0046 D3: 設定の変換（`config to-harnesses`）。DB には触らない。
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// ADR-0046 D7: 組織の移行（`org migrate-v2`）。
    Org {
        #[command(subcommand)]
        command: OrgCommand,
    },
}

fn resolve_db_path(cli_db: Option<PathBuf>) -> PathBuf {
    cli_db
        .or_else(|| env::var_os("CELERIS_DB").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("celeris.sqlite3"))
}

fn dispatch(store: &SqliteStore, db_path: &Path, command: Command) -> Result<ExitCode, CliError> {
    match command {
        Command::Org { command } => return org_cmd::run(store, db_path, command),
        // `Config` は DB を開く前に処理される（`main` を見よ）。
        Command::Config { command } => return config_cmd::run(command),
        Command::Add(args) => add::run(store, args),
        Command::Plan(args) => plan::run(store, args),
        Command::Ls(args) => query::run_ls(store, args),
        Command::Show(args) => query::run_show(store, args),
        Command::Approve(args) => gate::run_approve(store, args),
        Command::Reject(args) => gate::run_reject(store, args),
        Command::Cancel(args) => cancel::run(store, args),
        Command::Answer(args) => gate::run_answer(store, args),
        Command::Log(args) => query::run_log(store, args),
        Command::Replay(args) => replay::run(store, args),
        Command::Worker { command } => match command {
            WorkerCommand::Run(args) => worker::run_run(store, args),
        },
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // ADR-0046 D3: `config to-harnesses` は設定ファイルしか読まない（DB を開かない）。
    if let Command::Config { command } = cli.command {
        return match config_cmd::run(command) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let db_path = resolve_db_path(cli.db);

    let store = match SqliteStore::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to open db {}: {e}", db_path.display());
            return ExitCode::FAILURE;
        }
    };

    match dispatch(&store, &db_path, cli.command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
