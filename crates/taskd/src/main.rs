//! `taskd` バイナリ。`taskd --config <path> [--until-idle] [--max-ticks N]`（ADR-0005 D7）。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use taskd::{Config, RunOptions};

#[derive(Parser, Debug)]
#[command(name = "taskd", about = "taskd daemon: deterministic task dispatcher (DESIGN.md §5.2)")]
struct Cli {
    /// 設定ファイル（TOML）。
    #[arg(long, default_value = "config/taskd.toml")]
    config: PathBuf,
    /// 実行中・判定中・ready のタスクが無くなったら終了する。
    #[arg(long)]
    until_idle: bool,
    /// tick 数の上限（0 = 無制限）。
    #[arg(long, default_value_t = 0)]
    max_ticks: u64,
    /// ログ形式。
    #[arg(long, default_value = "json")]
    log_format: LogFormat,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum LogFormat {
    Json,
    Text,
}

fn init_tracing(format: LogFormat) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("TASKD_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr);
    match format {
        LogFormat::Json => builder.json().init(),
        LogFormat::Text => builder.init(),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.log_format);
    let config = match Config::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let opts = RunOptions {
        until_idle: cli.until_idle,
        max_ticks: cli.max_ticks,
    };
    match taskd::run(config, opts).await {
        Ok(exit) => {
            tracing::info!(?exit, "taskd stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "taskd failed");
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
