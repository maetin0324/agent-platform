//! `celerisctl db` — ADR-0064 D2/D3（Phase 110a）。**DB を通常の経路では開かない**
//! （`backup`/`integrity-check` はどちらも rusqlite の低レベル API を直接、専用の接続で使う。
//! `main.rs` が他の管理系コマンド〈`knowledge`/`mcp client`〉と同じく、主コマンドを dispatch する
//! 前に処理する）。

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Subcommand};

use crate::error::CliError;
use crate::outln;

#[derive(Subcommand, Debug)]
pub enum DbCommand {
    /// rusqlite の backup API で DB ファイルを丸ごと `<dest>` へコピーする。デーモン（他の接続）が
    /// 開いていても安全（WAL のスナップショットとして進む）。`scripts/selfdeploy/relocate-db.sh` が
    /// `sqlite3` コマンドの無い環境でこれを使う。
    Backup(DbBackupArgs),
    /// `<path>` に対して `PRAGMA integrity_check` を実行する。`ok` だけが返れば exit 0、
    /// 開けない・破損していれば exit 1（理由を stderr に出す）。
    IntegrityCheck(DbIntegrityCheckArgs),
}

#[derive(Args, Debug)]
pub struct DbBackupArgs {
    /// コピー先のファイルパス（既存ならエラーにせず上書きする。rusqlite の backup API の挙動）。
    pub dest: PathBuf,
    /// コピー元。省略時は `--db` / `CELERIS_DB` / `./celeris.sqlite3`（他のコマンドと同じ解決規則）。
    #[arg(long)]
    pub src: Option<PathBuf>,
    /// `PRAGMA busy_timeout`（ミリ秒）。既定 15000（relocate 中は他の接続が閉じているはずだが、
    /// 保険として少し長めにしてある）。
    #[arg(long, default_value_t = 15000)]
    pub busy_timeout_ms: u64,
}

#[derive(Args, Debug)]
pub struct DbIntegrityCheckArgs {
    pub path: PathBuf,
}

/// `db backup`。呼び出し側（`main.rs`）が `args.src` を `resolve_db_path` で埋めてから渡す。
pub fn run_backup(args: DbBackupArgs, src: PathBuf) -> Result<ExitCode, CliError> {
    // `rusqlite::Connection::open` は無ければ新規に空の DB を作ってしまう（`SQLITE_OPEN_CREATE` が
    // 既定）。`db backup` は既存の DB を写す操作なので、無言で空の DB を「バックアップ」しないよう
    // 先に存在を確認する。
    if !src.is_file() {
        return Err(CliError::msg(format!(
            "backup source {} does not exist or is not a file",
            src.display()
        )));
    }
    task_core::backup_database(&src, &args.dest, Duration::from_millis(args.busy_timeout_ms))
        .map_err(|e| CliError::msg(format!("backup {} -> {}: {e}", src.display(), args.dest.display())))?;
    outln!("backed up {} -> {}", src.display(), args.dest.display());
    Ok(ExitCode::SUCCESS)
}

/// `db integrity-check`。
pub fn run_integrity_check(args: DbIntegrityCheckArgs) -> Result<ExitCode, CliError> {
    match task_core::integrity_check(&args.path) {
        Ok(true) => {
            outln!("ok: {}", args.path.display());
            Ok(ExitCode::SUCCESS)
        }
        Ok(false) => {
            eprintln!("integrity check failed: {}", args.path.display());
            Ok(ExitCode::FAILURE)
        }
        Err(e) => {
            eprintln!("could not check {}: {e}", args.path.display());
            Ok(ExitCode::FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_then_integrity_check_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.sqlite3");
        drop(task_core::SqliteStore::open(&src).unwrap());
        let dest = dir.path().join("dest.sqlite3");

        let code = run_backup(
            DbBackupArgs {
                dest: dest.clone(),
                src: None,
                busy_timeout_ms: 5000,
            },
            src,
        )
        .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);

        let code = run_integrity_check(DbIntegrityCheckArgs { path: dest }).unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn integrity_check_on_garbage_bytes_is_a_failure_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.sqlite3");
        std::fs::write(&path, b"not a sqlite database").unwrap();
        let code = run_integrity_check(DbIntegrityCheckArgs { path }).unwrap();
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn backup_of_a_missing_source_is_a_clean_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.sqlite3");
        let dest = dir.path().join("dest.sqlite3");
        let err = run_backup(
            DbBackupArgs {
                dest,
                src: None,
                busy_timeout_ms: 5000,
            },
            missing,
        )
        .unwrap_err();
        assert!(matches!(err, CliError::Message(_)));
    }
}
