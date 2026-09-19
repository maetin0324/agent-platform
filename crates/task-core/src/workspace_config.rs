//! リポジトリの中の設定 `.config/celeris/workspace.toml`（ADR-0042 D2 / ADR-0043 D4）。
//!
//! **書いてあることだけ使う。言語やファイル構成から推定しない**（ADR-0043 §3）。無ければ全部既定。
//! 不正な TOML・知らないキーは **warn して既定**（設定の間違いで案件が止まらないように）。
//!
//! 使い道:
//! - `[workspace] description` — 計画 run と前置きに出す
//! - `[commands] check` — 前置きの「このリポジトリの検査コマンド」と、レビュー担当の
//!   `Check::Command` の既定（タスクに `acceptance` が明示されていればそれが勝つ）
//! - `[commands] setup` — worktree を作った直後に一度だけ走らせる
//! - `[outputs] docs` / `deliverables` — 成果物の置き場（ADR-0043 D8）
//! - `[run] mode` / `[container]` — **読むだけ**（コンテナ実行は ADR-0043 A3）
//!
//! ここは純粋な型と `parse`（文字列 → 設定）だけ。ファイルを読むのは `load`（薄い I/O）で、
//! 呼ぶのはディスパッチャ側である。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// リポジトリの中の設定ファイルの場所（リポジトリ相対）。ADR-0042 D2:
/// ルートに `.celeris/` や `.taskd/` は作らない。
pub const WORKSPACE_CONFIG_PATH: &str = ".config/celeris/workspace.toml";

/// `[outputs] docs` の既定（ADR-0043 D4）。
pub const DEFAULT_DOCS: &str = "docs";
/// `[outputs] deliverables` の既定（リポジトリのルート）。
pub const DEFAULT_DELIVERABLES: &str = ".";

/// `.config/celeris/workspace.toml` の中身（ADR-0043 D4）。全部の節が任意。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub workspace: WorkspaceSection,
    #[serde(default)]
    pub run: RunSection,
    #[serde(default)]
    pub container: ContainerSection,
    #[serde(default)]
    pub commands: CommandsSection,
    #[serde(default)]
    pub outputs: OutputsSection,
}

/// `[workspace]`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSection {
    /// 案件に登録するときの既定の `name`（任意）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 計画 run と前置きに出す一行（任意）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `[run]`。**この Phase では読むだけ**（コンテナ実行は ADR-0043 A3）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunSection {
    #[serde(default)]
    pub mode: RunMode,
}

/// `[run] mode`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    #[default]
    Host,
    Container,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RunMode::Host => "host",
            RunMode::Container => "container",
        }
    }
}

/// `[container]`。**この Phase では読むだけ**（ADR-0043 A3 が使う）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContainerSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// リポジトリ相対の Dockerfile（既定は `.config/celeris/Dockerfile`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// `[commands]`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandsSection {
    /// worktree 作成直後に一度だけ走らせる（ADR-0043 D3 / D4）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<String>,
    /// 実装者とレビュー担当が使う検査コマンド。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub check: Vec<String>,
}

/// `[outputs]`（ADR-0043 D8）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputsSection {
    /// 文書ページの根（既定 `docs`）。
    #[serde(default = "default_docs")]
    pub docs: String,
    /// コード以外の成果物（図・表・原稿）を置く根（既定はリポジトリのルート）。
    #[serde(default = "default_deliverables")]
    pub deliverables: String,
}

impl Default for OutputsSection {
    fn default() -> Self {
        Self {
            docs: default_docs(),
            deliverables: default_deliverables(),
        }
    }
}

fn default_docs() -> String {
    DEFAULT_DOCS.to_string()
}

fn default_deliverables() -> String {
    DEFAULT_DELIVERABLES.to_string()
}

/// TOML を読む（純粋関数）。`deny_unknown_fields` なので綴り間違いは `Err`。
pub fn parse(text: &str) -> Result<WorkspaceConfig, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// `repo_dir` の `.config/celeris/workspace.toml` を読む。
///
/// - ファイルが無い → `None`（既定を使う。**エラーではない**）
/// - 読めない・TOML として壊れている・知らないキーがある → `Some(Err(理由))`
///   （呼び出し側が warn して既定に倒す。ADR-0043 D4「不正なら warn して既定」）
pub fn load(repo_dir: &Path) -> Option<Result<WorkspaceConfig, String>> {
    let path = config_path(repo_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => return Some(Err(format!("{}: {e}", path.display()))),
    };
    Some(parse(&text).map_err(|e| format!("{}: {e}", path.display())))
}

/// 読めないときは既定（`load` の結果を 1 行にまとめる。警告は呼び出し側が出す）。
pub fn load_or_default(repo_dir: &Path) -> (WorkspaceConfig, Option<String>) {
    match load(repo_dir) {
        None => (WorkspaceConfig::default(), None),
        Some(Ok(cfg)) => (cfg, None),
        Some(Err(e)) => (WorkspaceConfig::default(), Some(e)),
    }
}

/// `<repo_dir>/.config/celeris/workspace.toml`。
pub fn config_path(repo_dir: &Path) -> PathBuf {
    repo_dir.join(WORKSPACE_CONFIG_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
[workspace]
name = "benchfs"
description = "ad-hoc FS のベンチマーク（Rust）"

[run]
mode = "container"

[container]
image = "ghcr.io/x/rust-dev:1.90"
mounts = ["/dev/infiniband:/dev/infiniband"]
env = { CARGO_TARGET_DIR = "/workspaces/.cargo-target" }

[commands]
setup = ["cargo fetch"]
check = ["cargo test --workspace", "cargo clippy --workspace -- -D warnings"]

[outputs]
docs = "doc"
deliverables = "figures"
"#;

    /// ADR-0043 D4: 全部の節が読める。
    #[test]
    fn every_section_of_the_adr_example_parses() {
        let cfg = parse(FULL).expect("parse");
        assert_eq!(cfg.workspace.name.as_deref(), Some("benchfs"));
        assert_eq!(cfg.workspace.description.as_deref(), Some("ad-hoc FS のベンチマーク（Rust）"));
        assert_eq!(cfg.run.mode, RunMode::Container);
        assert_eq!(cfg.container.image.as_deref(), Some("ghcr.io/x/rust-dev:1.90"));
        assert_eq!(cfg.container.mounts, vec!["/dev/infiniband:/dev/infiniband"]);
        assert_eq!(cfg.container.env.get("CARGO_TARGET_DIR").map(String::as_str), Some("/workspaces/.cargo-target"));
        assert_eq!(cfg.commands.setup, vec!["cargo fetch"]);
        assert_eq!(cfg.commands.check.len(), 2);
        assert_eq!(cfg.outputs.docs, "doc");
        assert_eq!(cfg.outputs.deliverables, "figures");
    }

    /// 空のファイル・省略した節は全部既定（`mode = host`、`docs = "docs"`、`deliverables = "."`）。
    #[test]
    fn everything_defaults_when_the_file_is_empty() {
        let cfg = parse("").expect("parse");
        assert_eq!(cfg, WorkspaceConfig::default());
        assert_eq!(cfg.run.mode, RunMode::Host);
        assert_eq!(cfg.outputs.docs, DEFAULT_DOCS);
        assert_eq!(cfg.outputs.deliverables, DEFAULT_DELIVERABLES);
        assert!(cfg.commands.check.is_empty());
        assert!(cfg.commands.setup.is_empty());
    }

    /// `deny_unknown_fields`: 綴り間違いは黙って無視しない。
    #[test]
    fn unknown_fields_are_rejected() {
        assert!(parse("[workspace]\nnaem = \"x\"\n").is_err());
        assert!(parse("[commands]\ntest = [\"x\"]\n").is_err());
        assert!(parse("[bogus]\nx = 1\n").is_err());
        assert!(parse("[run]\nmode = \"vm\"\n").is_err());
    }

    /// ファイルが無ければ `None`（既定）。壊れていれば理由付きの `Err`。
    #[test]
    fn load_returns_none_when_missing_and_an_error_when_broken() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load(dir.path()).is_none());
        let (cfg, warn) = load_or_default(dir.path());
        assert_eq!(cfg, WorkspaceConfig::default());
        assert!(warn.is_none());

        let path = config_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"not = [toml\n").expect("write");
        let (cfg, warn) = load_or_default(dir.path());
        assert_eq!(cfg, WorkspaceConfig::default());
        assert!(warn.expect("warn").contains("workspace.toml"));

        std::fs::write(&path, FULL).expect("write");
        let (cfg, warn) = load_or_default(dir.path());
        assert!(warn.is_none());
        assert_eq!(cfg.commands.check.len(), 2);
    }
}
