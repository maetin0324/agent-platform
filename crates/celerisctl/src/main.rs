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
use commands::knowledge::{self, KnowledgeCommand};
use commands::mcp::{self, McpCommand};
use commands::org::{self as org_cmd, OrgCommand};
use commands::plan::{self, PlanArgs};
use commands::projects::{self, ProjectsCommand};
use commands::query::{self, LogArgs, LsArgs, ShowArgs};
use commands::replay::{self, ReplayArgs};
use commands::worker::{self, WorkerCommand};
use error::CliError;

#[derive(Parser, Debug)]
#[command(
    name = "celerisctl",
    about = "celeris task control CLI (DESIGN.md §5.9)"
)]
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
    /// ADR-0047 D3（Phase 61）: 知識ベース（`init` / `search` / `get` / `record` / `reindex`）。
    /// **DB を開かない**ので、コンテナの中でも KB さえマウントされていれば動く。
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommand,
    },
    /// ADR-0056 D1（Phase 78）: MCP クライアントの発行・一覧・失効（`client`）、stdio 橋（`stdio`）。
    /// `stdio` 以外は DB を直接開く。
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
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
    /// ADR-0054 D2（Phase 68）: 案件の一覧・詳細（`ls`/`show` のタスク版）。読み取り専用。
    Projects {
        #[command(subcommand)]
        command: ProjectsCommand,
    },
}

fn resolve_db_path(cli_db: Option<PathBuf>) -> PathBuf {
    cli_db
        .or_else(|| env::var_os("CELERIS_DB").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("celeris.sqlite3"))
}

fn dispatch(store: &SqliteStore, db_path: &Path, command: Command) -> Result<ExitCode, CliError> {
    match command {
        Command::Org { command } => org_cmd::run(store, db_path, command),
        // `Config` は DB を開く前に処理される（`main` を見よ）。
        Command::Config { command } => config_cmd::run(command),
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
        Command::Projects { command } => projects::run(store, command),
        // `main` が先に処理する（DB を開かない場合があるため）。
        Command::Knowledge { .. } => unreachable!("handled before the store is opened"),
        Command::Mcp { .. } => unreachable!("handled before the store is opened"),
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
    // ADR-0047 D3: 知識ベースの道具は **DB を開かない**（ワーカーのコンテナには DB が無い）。
    // ADR-0052 D3 の例外は `knowledge rerun` だけ（管理系。`org migrate-v2` と同じく DB を直接開く）。
    if let Command::Knowledge { command } = cli.command {
        if !command.needs_db() {
            return match knowledge::run(command) {
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
        return match knowledge::run_with_store(&store, command) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    // ADR-0056 D1: `mcp stdio` は DB を開かない（手元の HTTP に橋を架けるだけ）。
    // Phase 101: `mcp call` も同じ（DB は開かない）。
    // `mcp client …` は `knowledge rerun` と同じ管理系（DB を直接開く）。
    if let Command::Mcp { command } = cli.command {
        return match command {
            McpCommand::Stdio(args) => match mcp::run_stdio(args) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            },
            McpCommand::Call(args) => match mcp::run_call(args) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            },
            McpCommand::Client { command } => {
                let db_path = resolve_db_path(cli.db);
                let store = match SqliteStore::open(&db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("error: failed to open db {}: {e}", db_path.display());
                        return ExitCode::FAILURE;
                    }
                };
                match mcp::run_client(&store, command) {
                    Ok(code) => code,
                    Err(e) => {
                        eprintln!("error: {e}");
                        ExitCode::FAILURE
                    }
                }
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
