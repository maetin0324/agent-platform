//! `taskd` バイナリ。`taskd --config <path> [--until-idle] [--max-ticks N]`（ADR-0005 D7）。
//! ADR-0040 D3 / D4（Phase 47）: `--mode` / `--db` / `--listen` / `--workspace-root` / `--token-file` /
//! `--release` の上書き。終了コードは 0（正常・drain 完了）/ 2（設定・スキーマ）/ 3（同じ版の二重起動）。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use taskd::{Config, Overrides, RunOptions};

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
    /// ADR-0040 D3: `verify` は本番のデータのコピーに対する検証専用（dispatch しない、ワーカーを
    /// 起こさない、tick の裏方を動かさない、Discord に送らない、`daemon_instances` に書かない）。
    #[arg(long, default_value = "normal")]
    mode: Mode,
    /// ADR-0040 D3: `db` の上書き（相対パスは設定ファイルのディレクトリ基準）。
    #[arg(long)]
    db: Option<PathBuf>,
    /// ADR-0040 D3: `[api] listen` の上書き（例 `127.0.0.1:7711`）。
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// ADR-0040 D3: `workspace_root` の上書き（相対パスは設定ファイルのディレクトリ基準）。
    #[arg(long)]
    workspace_root: Option<PathBuf>,
    /// ADR-0040 D3: `[api] token_file` の上書き（相対パスは設定ファイルのディレクトリ基準）。
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// ADR-0040 D4: このプロセスのリリース（`sha12`）。省略時は環境変数 `TASKD_RELEASE`、
    /// それも無ければ `dev`。同じ `release` の `active` が既にいたら何もせず exit 3。
    #[arg(long)]
    release: Option<String>,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Mode {
    Normal,
    Verify,
}

impl From<Mode> for task_core::DaemonMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Normal => Self::Normal,
            Mode::Verify => Self::Verify,
        }
    }
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
    let mut config = match Config::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    // ADR-0040 D3: 設定は本番のものをそのまま読み、**上書きは CLI だけ**。
    config.apply_overrides(&Overrides {
        db: cli.db,
        listen: cli.listen,
        workspace_root: cli.workspace_root,
        token_file: cli.token_file,
    });
    let opts = RunOptions {
        until_idle: cli.until_idle,
        max_ticks: cli.max_ticks,
        mode: cli.mode.into(),
        release: cli.release,
    };
    match taskd::run(config, opts).await {
        // ADR-0040 D4: 同じ版の `active` が既にいた。何も変えずに exit 3（promote.sh が見る）。
        Ok(taskd::Exit::DuplicateRelease) => {
            eprintln!("error: another instance of the same release is already active");
            ExitCode::from(3)
        }
        Ok(exit) => {
            tracing::info!(?exit, "taskd stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "taskd failed");
            eprintln!("error: {e}");
            // docs/gui/api.md §1.5: 知らない新しいスキーマ版数の DB は、設定エラーと同じく起動時の exit 2。
            if matches!(e, taskd::DaemonError::Store(task_core::StoreError::SchemaTooNew { .. })) {
                return ExitCode::from(2);
            }
            ExitCode::FAILURE
        }
    }
}
