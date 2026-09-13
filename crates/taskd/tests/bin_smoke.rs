//! `taskd` バイナリの最小テスト。`cargo test --workspace` でバイナリが必ずビルドされるようにする
//! （`tests/e2e` が `target/debug/taskd` を使う。ADR-0005 D8）。

use std::process::Command;

#[test]
fn help_exits_zero_and_missing_config_exits_two() {
    let out = Command::new(env!("CARGO_BIN_EXE_taskd")).arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("--until-idle"));

    let out = Command::new(env!("CARGO_BIN_EXE_taskd"))
        .args(["--config", "/nonexistent/taskd.toml"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
