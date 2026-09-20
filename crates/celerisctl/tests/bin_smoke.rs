//! `celerisctl` バイナリの最小テスト（`tests/e2e` が `target/debug/celerisctl replay` を使うため、
//! `cargo test --workspace` でバイナリを必ずビルドさせる）。

use std::process::Command;

#[test]
fn help_exits_zero() {
    let out = Command::new(env!("CARGO_BIN_EXE_celerisctl"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("replay"));
}
