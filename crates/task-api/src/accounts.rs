//! アカウントのプールの管理系エンドポイント（ADR-0024, ADR-0025）。
//!
//! task-api は task-dispatch に依存しないので（循環依存になる）、ディレクトリのスキャン（存在とログイン済みの
//! 判定だけ。中身は読まない）はここに独自に持つ。ロジックは `task_dispatch::accounts::{valid_account_id,
//! scan_accounts}` と同じ規則（ADR-0024 D1、ADR-0025 D1）。

use std::path::{Path, PathBuf};

use task_core::AccountAdapter;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::types::{AccountCooldownView, AccountUsageView, RateWindowView};

/// `^[A-Za-z0-9_-]{1,64}$`（プロバイダ id と同じ規則。ADR-0024 D1）。
pub fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `claude_dir` 直下の 1 ディレクトリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDirEntry {
    pub id: String,
    pub dir: PathBuf,
    pub logged_in: bool,
}

/// `root` の下の、有効な id を持つサブディレクトリを id 昇順で返す。`.` で始まる名前（`.removed/` 等）・
/// ファイル・無効な id は飛ばす。`root` が存在しなければ空。ログイン済みの判定は `adapter.credentials_marker()`
/// の有無（ADR-0025 D1: claude-code は `.credentials.json`、codex は `auth.json`）。
pub fn scan_accounts(root: &Path, adapter: AccountAdapter) -> Vec<AccountDirEntry> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    let marker = adapter.credentials_marker();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !valid_account_id(&name) {
            continue;
        }
        let logged_in = path.join(marker).exists();
        out.push(AccountDirEntry {
            id: name,
            dir: path,
            logged_in,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Unix 秒を RFC 3339 に直す（変換できなければ空文字列。実運用では起きない範囲の値のみ扱う）。
pub(crate) fn rfc3339_unix(secs: i64) -> String {
    OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_default()
}

/// `RateLimitObservation`（task-core、run/check の直後）を `AccountUsageView` に写す。
pub(crate) fn usage_view_from_observation(
    obs: &task_core::RateLimitObservation,
    source: &str,
) -> AccountUsageView {
    AccountUsageView {
        five_hour: obs.five_hour.map(|w| RateWindowView {
            utilization: w.utilization,
            resets_at: rfc3339_unix(w.resets_at),
        }),
        seven_day: obs.seven_day.map(|w| RateWindowView {
            utilization: w.utilization,
            resets_at: rfc3339_unix(w.resets_at),
        }),
        status: obs.status.clone(),
        observed_at: rfc3339_unix(obs.observed_at),
        source: source.to_string(),
    }
}

/// `AccountUsageLive`（task-ops、スナップショット）を `AccountUsageView` に写す。
pub(crate) fn usage_view_from_live(live: &task_ops::daemon::AccountUsageLive) -> AccountUsageView {
    AccountUsageView {
        five_hour: live.five_hour.map(|w| RateWindowView {
            utilization: w.utilization,
            resets_at: rfc3339_unix(w.resets_at),
        }),
        seven_day: live.seven_day.map(|w| RateWindowView {
            utilization: w.utilization,
            resets_at: rfc3339_unix(w.resets_at),
        }),
        status: live.status.clone(),
        observed_at: rfc3339_unix(live.observed_at),
        source: live.source.clone(),
    }
}

pub(crate) fn cooldown_view_from_live(
    live: &task_ops::daemon::AccountCooldownLive,
) -> AccountCooldownView {
    AccountCooldownView {
        until: rfc3339_unix(live.until),
        reason: live.reason.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_account_id_rejects_path_traversal_and_dots() {
        assert!(valid_account_id("a"));
        assert!(valid_account_id("acct-1_ok"));
        assert!(!valid_account_id(""));
        assert!(!valid_account_id(".hidden"));
        assert!(!valid_account_id("../escape"));
        assert!(!valid_account_id("a/b"));
        assert!(!valid_account_id(&"x".repeat(65)));
    }

    #[test]
    fn scan_accounts_finds_valid_dirs_sorted_with_login_state() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("tmp: {e}"));
        let root = tmp.path();
        std::fs::create_dir(root.join("bravo")).unwrap_or_else(|e| panic!("mkdir: {e}"));
        std::fs::write(root.join("bravo").join(".credentials.json"), "{}")
            .unwrap_or_else(|e| panic!("write: {e}"));
        std::fs::create_dir(root.join("alpha")).unwrap_or_else(|e| panic!("mkdir: {e}"));
        std::fs::create_dir(root.join(".removed")).unwrap_or_else(|e| panic!("mkdir: {e}"));
        std::fs::write(root.join("not-a-dir"), "x").unwrap_or_else(|e| panic!("write: {e}"));

        let found = scan_accounts(root, AccountAdapter::ClaudeCode);
        assert_eq!(
            found,
            vec![
                AccountDirEntry {
                    id: "alpha".into(),
                    dir: root.join("alpha"),
                    logged_in: false
                },
                AccountDirEntry {
                    id: "bravo".into(),
                    dir: root.join("bravo"),
                    logged_in: true
                },
            ]
        );
    }

    #[test]
    fn scan_accounts_missing_root_is_empty() {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("tmp: {e}"));
        assert_eq!(
            scan_accounts(&tmp.path().join("nope"), AccountAdapter::ClaudeCode),
            Vec::new()
        );
    }
}
