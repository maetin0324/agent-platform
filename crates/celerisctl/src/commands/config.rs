//! `celerisctl config to-harnesses`（ADR-0046 D3）。
//!
//! 旧い `[[genres]]` + `[[roles]]` を `[[harnesses]]` の形で標準出力に書き出す（決定的。LLM は使わない）。
//! DB には触らない（設定ファイルしか読まない）。

use std::path::PathBuf;
use std::process::ExitCode;

use celeris::config::Config;
use clap::Subcommand;

use crate::error::CliError;
use crate::outln;

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// 旧い `[[genres]]` + `[[roles]]` を `[[harnesses]]` の形に書き出す（ADR-0046 D3）。
    ToHarnesses(ToHarnessesArgs),
}

#[derive(clap::Args, Debug)]
pub struct ToHarnessesArgs {
    /// 読み込む設定ファイル（`~/.config/celeris/config.toml` など）。
    #[arg(long)]
    pub config: PathBuf,
}

pub fn run(command: ConfigCommand) -> Result<ExitCode, CliError> {
    match command {
        ConfigCommand::ToHarnesses(args) => {
            let config = Config::load(&args.config)
                .map_err(|e| CliError::msg(format!("config {}: {e}", args.config.display())))?;
            outln!("{}", config.to_harnesses_toml().trim_end());
            Ok(ExitCode::SUCCESS)
        }
    }
}
