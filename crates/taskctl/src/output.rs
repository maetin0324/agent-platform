//! 標準出力への書き込みヘルパ（ADR-0010 D4, DESIGN.md §5.9「出力を閉じたパイプに
//! 流しても異常終了しないこと」）。
//!
//! `println!` は書き込みに失敗すると panic する。`taskctl ls | head` のように
//! 出力先のパイプが先に閉じられると書き込みは `BrokenPipe` エラーになるため、
//! 各コマンドは `println!` の代わりにこのモジュールの `outln!` マクロを使う。
//! `BrokenPipe` は「読み手が十分読んで終了した」正常なケースとみなし、
//! `std::process::exit(0)` で静かに終了する。それ以外の書き込みエラーは
//! 復旧できないため `exit(1)` する（panic はしない）。

use std::io::Write;

/// `outln!` マクロの実体。テストからは直接使わない（マクロ経由で使う）。
pub fn write_line(args: std::fmt::Arguments<'_>) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    if let Err(e) = writeln!(lock, "{args}") {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        std::process::exit(1);
    }
}

/// `println!` の代替。標準出力への書き込みが `BrokenPipe` で失敗しても panic せず、
/// 静かに `exit(0)` する（ADR-0010 D4）。
#[macro_export]
macro_rules! outln {
    () => {
        $crate::output::write_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::output::write_line(format_args!($($arg)*))
    };
}
