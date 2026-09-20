//! テスト専用ヘルパ（`#[cfg(test)]`）。`claude_code`/`codex` のテストが起動する `sh` スタブを
//! 書き込むために使う。
//!
//! ETXTBSY 対策（ADR-0010 D10）: `std::fs::write` でテストプロセス自身がスタブファイルを書き込むと、
//! その書き込みで開いた fd はテストプロセス（tokio マルチスレッドランタイム、複数テストが並行実行）が
//! 保持したままになりうる。並行に走る別のテストが `Command::spawn` で fork するとき、fork は親プロセス
//! （＝テストプロセスそのもの）の全ファイルディスクリプタを継承するため、書き込み直後でまだ閉じられて
//! いないスタブファイルの fd を子が継承してしまうことがあり、その状態でスタブを exec しようとすると
//! `ETXTBSY`（書き込み用に開かれているファイルへの exec は失敗する）が発生しうる。
//! テストプロセス自身がスタブへの書き込み fd を一切保持しなければこの競合は起こらないので、
//! 内容を **別プロセス**（`sh -c 'cat > "$1" && chmod 755 "$1"'`）に標準入力経由で渡して書かせる。

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// `path` に `contents` を実行可能（0o755）として書き込む。書き込みは別プロセスで行う（上記コメント参照）。
pub(crate) fn write_executable(path: &Path, contents: &str) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(r#"cat > "$1" && chmod 755 "$1""#)
        .arg("sh") // "$0"
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn stub writer for {}: {e}", path.display()));

    let mut stdin = child
        .stdin
        .take()
        .unwrap_or_else(|| panic!("stub writer stdin was not piped"));
    stdin
        .write_all(contents.as_bytes())
        .unwrap_or_else(|e| panic!("failed to write stub contents for {}: {e}", path.display()));
    drop(stdin);

    let status = child
        .wait()
        .unwrap_or_else(|e| panic!("failed to wait for stub writer for {}: {e}", path.display()));
    assert!(
        status.success(),
        "stub writer failed for {}: {status}",
        path.display()
    );
}
