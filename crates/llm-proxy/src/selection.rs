//! 決定的な選択（ADR-0053 D1 / ADR-0049 の規則の再利用）。
//!
//! `celeris/<tier>`: (a) `prefer_free` なら到達可能な `openai-compatible` を最優先、(b) 無ければ
//! アカウントプール（Claude / Codex を跨いで残量スコアを比較。同点は設定順＝ claude を先に見る）。
//! `claude/<tier>` / `gpt/<tier>` はそのプールだけ、`qwen/<tier>` は `openai-compatible` だけを見る。
//!
//! 429/401 を受けて次の候補へやり直せるよう（ADR-0053 D1）、選択は**順位付きの列**を返す
//! （1 位が failed candidate/account_book 更新を受けても、この列は要求の最初に決めたまま進む。
//! 同じ要求の中で選び直しはしない）。
//!
//! ここは判断（決定的）だけを持ち、I/O（ファイル走査・到達性 probe）は呼び出し側が済ませてから渡す
//! （テストしやすくするため。DESIGN 原則「判断は 1 か所」をこのクレート内でも守る）。

use std::cmp::Ordering;
use std::path::PathBuf;

use task_dispatch::accounts::{AccountBook, AccountCandidate, AccountDir, evaluate};

use crate::config::OpenAiCompatibleConfig;
use crate::naming::SourceKind;

/// 1 プール分の入力（`scan_accounts` 済みの一覧・帳簿・使用中カウント）。
pub struct PoolInput<'a> {
    pub dirs: &'a [AccountDir],
    pub book: &'a AccountBook,
    pub in_use: &'a dyn Fn(&str) -> usize,
    pub max_concurrent_per_account: usize,
}

/// 選ばれたアカウント（供給元の種類・id・ディレクトリ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedAccount {
    pub source: SourceKind,
    pub account_id: String,
    pub dir: PathBuf,
}

fn dir_for(dirs: &[AccountDir], id: &str) -> PathBuf {
    dirs.iter()
        .find(|d| d.id == id)
        .map(|d| d.dir.clone())
        .unwrap_or_default()
}

/// スコア降順、同点は in_use 昇順→id 昇順（ADR-0024 D3-4 と同じ規律）。除外は含めない。
fn rank_in_pool(input: &PoolInput<'_>, now: i64) -> Vec<(String, f64, usize)> {
    let mut scored: Vec<(String, f64, usize)> = input
        .dirs
        .iter()
        .filter_map(|d| {
            let in_use = (input.in_use)(&d.id);
            let cand = AccountCandidate {
                id: &d.id,
                logged_in: d.logged_in,
                in_use,
            };
            evaluate(
                &cand,
                input.book.state(&d.id),
                input.max_concurrent_per_account,
                now,
            )
            .score
            .map(|s| (d.id.clone(), s, in_use))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then(a.2.cmp(&b.2))
            .then(a.0.cmp(&b.0))
    });
    scored
}

/// `claude/<tier>` / `gpt/<tier>` 用: 1 プールを順位付きで返す。
pub fn rank_pool(source: SourceKind, input: &PoolInput<'_>, now: i64) -> Vec<SelectedAccount> {
    rank_in_pool(input, now)
        .into_iter()
        .map(|(id, _, _)| SelectedAccount {
            source,
            account_id: id.clone(),
            dir: dir_for(input.dirs, &id),
        })
        .collect()
}

pub fn select_from_pool(
    source: SourceKind,
    input: &PoolInput<'_>,
    now: i64,
) -> Option<SelectedAccount> {
    rank_pool(source, input, now).into_iter().next()
}

/// `celeris/<tier>` の (b): Claude と Codex を跨いで残量スコアを比較し、順位付きの列にする。
/// 同点は設定順（claude を先に見る）→ in_use 少ない方 → id 昇順。
pub fn rank_across_pools(
    claude: Option<&PoolInput<'_>>,
    codex: Option<&PoolInput<'_>>,
    now: i64,
) -> Vec<SelectedAccount> {
    let mut combined: Vec<(SourceKind, String, f64, usize)> = Vec::new();
    if let Some(input) = claude {
        combined.extend(
            rank_in_pool(input, now)
                .into_iter()
                .map(|(id, score, in_use)| (SourceKind::Claude, id, score, in_use)),
        );
    }
    if let Some(input) = codex {
        combined.extend(
            rank_in_pool(input, now)
                .into_iter()
                .map(|(id, score, in_use)| (SourceKind::Gpt, id, score, in_use)),
        );
    }
    // claude(0) を codex(1) より先に見る（同点タイブレークの「設定順」）。
    let source_priority = |s: SourceKind| if s == SourceKind::Claude { 0u8 } else { 1u8 };
    combined.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(Ordering::Equal)
            .then(source_priority(a.0).cmp(&source_priority(b.0)))
            .then(a.3.cmp(&b.3))
            .then(a.1.cmp(&b.1))
    });
    combined
        .into_iter()
        .map(|(source, id, _, _)| {
            let dirs = match source {
                SourceKind::Claude => claude.map(|p| p.dirs).unwrap_or(&[]),
                _ => codex.map(|p| p.dirs).unwrap_or(&[]),
            };
            SelectedAccount {
                source,
                account_id: id.clone(),
                dir: dir_for(dirs, &id),
            }
        })
        .collect()
}

pub fn select_across_pools(
    claude: Option<&PoolInput<'_>>,
    codex: Option<&PoolInput<'_>>,
    now: i64,
) -> Option<SelectedAccount> {
    rank_across_pools(claude, codex, now).into_iter().next()
}

/// `qwen/<tier>` / `celeris/<tier>` の (a): 設定順で到達可能な relay を順位付きで返す。
/// `reachable` は呼び出し側が probe 済みの結果を返す（副作用なし。テストしやすくするため）。
pub fn rank_relays(
    sources: &[OpenAiCompatibleConfig],
    reachable: impl Fn(&str) -> bool,
) -> Vec<&OpenAiCompatibleConfig> {
    sources
        .iter()
        .filter(|s| s.enabled && reachable(&s.id))
        .collect()
}

pub fn pick_relay(
    sources: &[OpenAiCompatibleConfig],
    reachable: impl Fn(&str) -> bool,
) -> Option<&OpenAiCompatibleConfig> {
    rank_relays(sources, reachable).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_dispatch::accounts::{AccountBook, AccountCooldown, AccountCooldownReason};

    fn dirs(ids: &[&str]) -> Vec<AccountDir> {
        ids.iter()
            .map(|id| AccountDir {
                id: id.to_string(),
                dir: PathBuf::from(format!("/accounts/{id}")),
                logged_in: true,
            })
            .collect()
    }

    fn no_in_use(_: &str) -> usize {
        0
    }

    #[test]
    fn select_from_pool_prefers_the_unused_account_and_ties_break_by_id() {
        let dirs = dirs(&["bravo", "alpha"]);
        let book = AccountBook::new_in_memory();
        let input = PoolInput {
            dirs: &dirs,
            book: &book,
            in_use: &no_in_use,
            max_concurrent_per_account: 4,
        };
        let picked = select_from_pool(SourceKind::Claude, &input, 1_000).unwrap();
        assert_eq!(picked.account_id, "alpha");
        assert_eq!(picked.source, SourceKind::Claude);
    }

    #[test]
    fn rank_pool_orders_all_eligible_candidates_and_drops_excluded_ones() {
        let mut dirs = dirs(&["a", "b", "c"]);
        dirs[2].logged_in = false; // c: excluded
        let mut book = AccountBook::new_in_memory();
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 9_999,
                reason: AccountCooldownReason::Throttled,
            },
            0,
        ); // a: excluded (cooldown)
        let input = PoolInput {
            dirs: &dirs,
            book: &book,
            in_use: &no_in_use,
            max_concurrent_per_account: 4,
        };
        let ranked = rank_pool(SourceKind::Claude, &input, 1_000);
        assert_eq!(
            ranked.into_iter().map(|s| s.account_id).collect::<Vec<_>>(),
            vec!["b"]
        );
    }

    #[test]
    fn select_from_pool_skips_cooldown_and_not_logged_in() {
        let mut dirs = dirs(&["a", "b"]);
        dirs[1].logged_in = false;
        let mut book = AccountBook::new_in_memory();
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 9_999,
                reason: AccountCooldownReason::Throttled,
            },
            0,
        );
        let input = PoolInput {
            dirs: &dirs,
            book: &book,
            in_use: &no_in_use,
            max_concurrent_per_account: 4,
        };
        assert_eq!(select_from_pool(SourceKind::Claude, &input, 1_000), None);
    }

    #[test]
    fn select_across_pools_picks_the_higher_scoring_pool() {
        let claude_dirs = dirs(&["c1"]);
        let codex_dirs = dirs(&["g1"]);
        let claude_book = AccountBook::new_in_memory();
        let codex_book = AccountBook::new_in_memory();
        let claude_input = PoolInput {
            dirs: &claude_dirs,
            book: &claude_book,
            in_use: &|_| 3usize, // in_use penalty で claude を不利にする
            max_concurrent_per_account: 4,
        };
        let codex_input = PoolInput {
            dirs: &codex_dirs,
            book: &codex_book,
            in_use: &no_in_use,
            max_concurrent_per_account: 4,
        };
        let picked = select_across_pools(Some(&claude_input), Some(&codex_input), 1_000).unwrap();
        assert_eq!(picked.source, SourceKind::Gpt);
        assert_eq!(picked.account_id, "g1");
    }

    #[test]
    fn select_across_pools_ties_prefer_claude_by_config_order() {
        let claude_dirs = dirs(&["c1"]);
        let codex_dirs = dirs(&["g1"]);
        let claude_book = AccountBook::new_in_memory();
        let codex_book = AccountBook::new_in_memory();
        let claude_input = PoolInput {
            dirs: &claude_dirs,
            book: &claude_book,
            in_use: &no_in_use,
            max_concurrent_per_account: 4,
        };
        let codex_input = PoolInput {
            dirs: &codex_dirs,
            book: &codex_book,
            in_use: &no_in_use,
            max_concurrent_per_account: 4,
        };
        let picked = select_across_pools(Some(&claude_input), Some(&codex_input), 1_000).unwrap();
        assert_eq!(picked.source, SourceKind::Claude);
    }

    #[test]
    fn select_across_pools_falls_back_to_the_only_available_pool() {
        let codex_dirs = dirs(&["g1"]);
        let codex_book = AccountBook::new_in_memory();
        let codex_input = PoolInput {
            dirs: &codex_dirs,
            book: &codex_book,
            in_use: &no_in_use,
            max_concurrent_per_account: 4,
        };
        let picked = select_across_pools(None, Some(&codex_input), 1_000).unwrap();
        assert_eq!(picked.source, SourceKind::Gpt);
    }

    #[test]
    fn rank_across_pools_returns_a_full_fallback_order() {
        let claude_dirs = dirs(&["c1", "c2"]);
        let codex_dirs = dirs(&["g1"]);
        let claude_book = AccountBook::new_in_memory();
        let codex_book = AccountBook::new_in_memory();
        let claude_input = PoolInput {
            dirs: &claude_dirs,
            book: &claude_book,
            in_use: &no_in_use,
            max_concurrent_per_account: 4,
        };
        let codex_input = PoolInput {
            dirs: &codex_dirs,
            book: &codex_book,
            in_use: &no_in_use,
            max_concurrent_per_account: 4,
        };
        let ranked = rank_across_pools(Some(&claude_input), Some(&codex_input), 1_000);
        let ids: Vec<_> = ranked.iter().map(|s| s.account_id.as_str()).collect();
        assert_eq!(ids, vec!["c1", "c2", "g1"]);
    }

    fn relay(id: &str) -> OpenAiCompatibleConfig {
        OpenAiCompatibleConfig {
            id: id.to_string(),
            base_url: format!("http://127.0.0.1:0/{id}"),
            api_key: None,
            enabled: true,
        }
    }

    #[test]
    fn pick_relay_returns_the_first_reachable_in_config_order() {
        let sources = vec![relay("a"), relay("b")];
        let picked = pick_relay(&sources, |id| id == "b");
        assert_eq!(picked.unwrap().id, "b");
        let none = pick_relay(&sources, |_| false);
        assert!(none.is_none());
    }

    #[test]
    fn pick_relay_skips_disabled_sources() {
        let mut sources = vec![relay("a"), relay("b")];
        sources[0].enabled = false;
        let picked = pick_relay(&sources, |_| true);
        assert_eq!(picked.unwrap().id, "b");
    }

    #[test]
    fn rank_relays_keeps_config_order_among_reachable_sources() {
        let sources = vec![relay("a"), relay("b"), relay("c")];
        let ranked = rank_relays(&sources, |id| id != "b");
        assert_eq!(
            ranked
                .into_iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "c"]
        );
    }
}
