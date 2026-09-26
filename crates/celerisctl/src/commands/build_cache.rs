//! `celerisctl build-cache prune`: 設定した共有 Cargo キャッシュの古い repo-key だけを消す。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use celeris::Config;
use clap::{Args, Subcommand};

use crate::error::CliError;
use crate::outln;

#[derive(Debug, Subcommand)]
pub enum BuildCacheCommand {
    Prune(PruneArgs),
}

#[derive(Debug, Args)]
pub struct PruneArgs {
    /// `config.toml`。省略時は `CELERIS_CONFIG`。
    #[arg(long, env = "CELERIS_CONFIG")]
    pub config: PathBuf,
    /// 何日間更新が無い repo-key を刈るか（既定 30 日）。
    #[arg(long, default_value_t = 30)]
    pub older_than: u64,
    /// 候補を表示するだけ。
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(command: BuildCacheCommand) -> Result<ExitCode, CliError> {
    match command {
        BuildCacheCommand::Prune(args) => {
            let config =
                Config::load(&args.config).map_err(|e| CliError::msg(format!("config: {e}")))?;
            let cargo = config.workspace.build_cache_dir.join("cargo");
            let cutoff = SystemTime::now()
                .checked_sub(Duration::from_secs(args.older_than.saturating_mul(86400)))
                .ok_or_else(|| CliError::msg("older-than is too large"))?;
            let candidates = candidates(&cargo, cutoff)
                .map_err(|e| CliError::msg(format!("{}: {e}", cargo.display())))?;
            for path in &candidates {
                if args.dry_run {
                    outln!("would prune {}", path.display());
                } else {
                    std::fs::remove_dir_all(path)
                        .map_err(|e| CliError::msg(format!("{}: {e}", path.display())))?;
                    outln!("pruned {}", path.display());
                }
            }
            outln!("{} candidate(s)", candidates.len());
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn candidates(cargo: &Path, cutoff: SystemTime) -> std::io::Result<Vec<PathBuf>> {
    if !cargo.exists() {
        return Ok(Vec::new());
    }
    if !std::fs::symlink_metadata(cargo)?.file_type().is_dir() {
        return Err(std::io::Error::other("cargo must be a real directory"));
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(cargo)? {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        // シンボリックリンクや通常ファイルは対象にしない。`cargo/*` 直下の実ディレクトリだけ。
        if meta.file_type().is_dir() && last_modified(&entry.path())? < cutoff {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// root 以下の実ファイル・実ディレクトリの最新更新時刻。symlink の先は辿らない。
fn last_modified(root: &Path) -> std::io::Result<SystemTime> {
    let mut latest = SystemTime::UNIX_EPOCH;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = std::fs::symlink_metadata(&path)?;
        latest = latest.max(meta.modified()?);
        if meta.file_type().is_dir() {
            for entry in std::fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_old_directories_directly_under_cargo_are_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo = tmp.path().join("cargo");
        std::fs::create_dir(&cargo).unwrap();
        let old = cargo.join("old");
        let recent = cargo.join("recent");
        std::fs::create_dir(&old).unwrap();
        std::fs::create_dir(&recent).unwrap();
        std::fs::write(cargo.join("file"), "keep").unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(40 * 86400);
        std::fs::File::open(&old)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&old, cargo.join("link")).unwrap();
        let found =
            candidates(&cargo, SystemTime::now() - Duration::from_secs(30 * 86400)).unwrap();
        assert_eq!(found, vec![old.clone()]);
        assert!(recent.exists());
        assert!(cargo.join("link").exists());
        std::fs::write(old.join("active-build"), "new").unwrap();
        assert!(
            candidates(&cargo, SystemTime::now() - Duration::from_secs(30 * 86400))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn dry_run_preserves_cache_and_prune_removes_only_old_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let cargo = cache.join("cargo");
        let old = cargo.join("old");
        let fresh = cargo.join("fresh");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir(&fresh).unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(40 * 86400);
        std::fs::File::open(&old)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_time))
            .unwrap();
        let config = tmp.path().join("config.toml");
        std::fs::write(&config, format!("[workspace]\nbuild_cache_dir = \"{}\"\n[[providers]]\nid = \"x\"\nadapter = \"fake\"\n", cache.display())).unwrap();
        let args = |dry_run| PruneArgs {
            config: config.clone(),
            older_than: 30,
            dry_run,
        };
        run(BuildCacheCommand::Prune(args(true))).unwrap();
        assert!(old.exists());
        run(BuildCacheCommand::Prune(args(false))).unwrap();
        assert!(!old.exists());
        assert!(fresh.exists());
    }
}
