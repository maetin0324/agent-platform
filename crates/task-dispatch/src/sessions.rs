//! ADR-0054 D1（Phase 67）: ノードごとの継続セッションの**決定的な判断**（純粋関数、I/O 無し）。
//!
//! 実際の読み書き（`node_sessions` の作成・引退・前回以降の差分の取り出し）は
//! `Dispatcher::resolve_node_session`（`dispatcher.rs`）が行う。ここに置くのは、テストしやすい形の
//! 「続けるか、新しく作るか」の判断と、前置きに出す差分・要約の行の組み立てだけ。

use task_core::{MessageRole, NodeSession};
use time::OffsetDateTime;

/// このアダプタだけが継続セッションを持てる（ADR-0054 D1）。他のアダプタ（`paperqa` /
/// `local-deep-research` / `langmem` 等）は resume の手段が無いので継続しない。
pub const SUPPORTED_ADAPTERS: [&str; 3] = ["claude-code", "codex", "acp"];

pub fn adapter_supports_sessions(adapter_id: &str) -> bool {
    SUPPORTED_ADAPTERS.contains(&adapter_id)
}

/// [`decide`] が返す判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    /// 現役セッションをそのまま続ける（`--resume` 等）。
    Resume,
    /// 新しいセッションを作る（理由付き）。
    Fresh(FreshReason),
}

/// 新しいセッションを作る理由。`NoActive` だけは「初回」で要約を前置きに乗せない
/// （それ以外は継続の断絶なので、ADR-0033 D4 の対話履歴の末尾 20 件を要約として前置きに入れる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshReason {
    /// 現役セッションが無い（このノード・kind・project_id の最初の run）。
    NoActive,
    /// このセッションを持つアダプタが変わった（設定変更等）。
    AdapterChanged,
    /// アカウントプールが別のアカウントに倒れた（枯渇・cooldown）。
    AccountChanged,
    /// `approx_tokens`（run の usage の累計）が `rollover_tokens` を超えた。
    RolloverExceeded,
    /// 直前の run がこのセッションの resume に失敗した（アダプタがセッション不明・拒否を報告した）。
    ResumeFailed,
}

impl FreshReason {
    /// 要約（前回までの対話履歴の末尾）を前置きに乗せるべきか。初回だけは乗せない
    /// （継いでいる前のセッションが無いため）。
    pub fn needs_summary(self) -> bool {
        !matches!(self, FreshReason::NoActive)
    }
}

/// 決定的な判断（純粋関数）。`active` は今の現役セッション（無ければ `None`）。`resume_failed` は、
/// 直前の run がこのセッションの resume に失敗した（アダプタがセッション不明/拒否を報告した）ことを
/// 呼び出し側が検出して渡す。
pub fn decide(
    active: Option<&NodeSession>,
    adapter_id: &str,
    account: Option<&str>,
    rollover_tokens: u64,
    resume_failed: bool,
) -> SessionAction {
    let Some(active) = active else {
        return SessionAction::Fresh(FreshReason::NoActive);
    };
    if resume_failed {
        return SessionAction::Fresh(FreshReason::ResumeFailed);
    }
    if active.adapter != adapter_id {
        return SessionAction::Fresh(FreshReason::AdapterChanged);
    }
    if active.account_id.as_deref() != account {
        return SessionAction::Fresh(FreshReason::AccountChanged);
    }
    if active.approx_tokens as u64 >= rollover_tokens {
        return SessionAction::Fresh(FreshReason::RolloverExceeded);
    }
    SessionAction::Resume
}

/// ADR-0054 D1: 継続中セッションの前置きに出す**差分**（前回の run 以降に起きたこと）。純粋関数:
/// 呼び出し側がストアから読んだ、時刻付きの生データを渡すだけ。`since`（前回の run の時刻。
/// `node_sessions.last_used_at`）より後のものだけを残し、種類ごとに 1 行ずつにする。
#[allow(clippy::too_many_arguments)]
pub fn diff_lines(
    new_messages: &[(OffsetDateTime, MessageRole, String)],
    finished_tasks: &[(OffsetDateTime, String)],
    approval_results: &[(OffsetDateTime, String)],
    new_projects: &[(OffsetDateTime, String)],
    since: OffsetDateTime,
) -> Vec<String> {
    let mut out = Vec::new();
    for (t, role, text) in new_messages {
        if *t > since {
            let who = match role {
                MessageRole::User => "人",
                MessageRole::Node => "あなた",
            };
            out.push(format!("{who}: {text}"));
        }
    }
    for (t, summary) in finished_tasks {
        if *t > since {
            out.push(format!("タスク終了: {summary}"));
        }
    }
    for (t, result) in approval_results {
        if *t > since {
            out.push(format!("認可: {result}"));
        }
    }
    for (t, project) in new_projects {
        if *t > since {
            out.push(format!("新しい案件: {project}"));
        }
    }
    out
}

/// ADR-0033 D4 / ADR-0054 D1: 新しいセッションを継ぐときの「これまでの要約」（対話履歴の末尾
/// `limit` 件。既定 20）。純粋関数。
pub fn summary_lines(history: &[(MessageRole, String)], limit: usize) -> Vec<String> {
    let start = history.len().saturating_sub(limit);
    history[start..]
        .iter()
        .map(|(role, text)| {
            let who = match role {
                MessageRole::User => "人",
                MessageRole::Node => "あなた",
            };
            format!("{who}: {text}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(adapter: &str, account: Option<&str>, tokens: u64) -> NodeSession {
        NodeSession {
            approx_tokens: tokens as i64,
            ..NodeSession::new(
                "cos",
                task_core::SessionKind::Conversation,
                None,
                adapter,
                account.map(str::to_string),
                "sess-1",
                OffsetDateTime::now_utc(),
            )
        }
    }

    #[test]
    fn no_active_session_is_fresh_without_a_summary() {
        let action = decide(None, "claude-code", Some("a"), 400_000, false);
        assert_eq!(action, SessionAction::Fresh(FreshReason::NoActive));
        assert!(!FreshReason::NoActive.needs_summary());
    }

    #[test]
    fn a_matching_session_under_the_rollover_threshold_resumes() {
        let s = session("claude-code", Some("a"), 100);
        let action = decide(Some(&s), "claude-code", Some("a"), 400_000, false);
        assert_eq!(action, SessionAction::Resume);
    }

    #[test]
    fn rollover_exceeded_is_fresh_with_summary() {
        let s = session("claude-code", Some("a"), 400_000);
        let action = decide(Some(&s), "claude-code", Some("a"), 400_000, false);
        assert_eq!(action, SessionAction::Fresh(FreshReason::RolloverExceeded));
        assert!(FreshReason::RolloverExceeded.needs_summary());

        // 境界のすぐ下は resume。
        let under = session("claude-code", Some("a"), 399_999);
        assert_eq!(
            decide(Some(&under), "claude-code", Some("a"), 400_000, false),
            SessionAction::Resume
        );
    }

    #[test]
    fn account_change_is_fresh() {
        let s = session("claude-code", Some("acct-a"), 10);
        assert_eq!(
            decide(Some(&s), "claude-code", Some("acct-b"), 400_000, false),
            SessionAction::Fresh(FreshReason::AccountChanged)
        );
        // プールを使わない → 使う（あるいはその逆）も account change 扱い。
        assert_eq!(
            decide(Some(&s), "claude-code", None, 400_000, false),
            SessionAction::Fresh(FreshReason::AccountChanged)
        );
    }

    #[test]
    fn adapter_change_is_fresh() {
        let s = session("claude-code", Some("a"), 10);
        assert_eq!(
            decide(Some(&s), "codex", Some("a"), 400_000, false),
            SessionAction::Fresh(FreshReason::AdapterChanged)
        );
    }

    #[test]
    fn a_failed_resume_is_fresh_even_if_nothing_else_changed() {
        let s = session("claude-code", Some("a"), 10);
        assert_eq!(
            decide(Some(&s), "claude-code", Some("a"), 400_000, true),
            SessionAction::Fresh(FreshReason::ResumeFailed)
        );
    }

    #[test]
    fn diff_lines_keeps_only_items_strictly_after_since() {
        let since = OffsetDateTime::now_utc();
        let before = since - time::Duration::seconds(10);
        let after = since + time::Duration::seconds(10);
        let lines = diff_lines(
            &[
                (before, MessageRole::User, "old question".into()),
                (after, MessageRole::User, "new question".into()),
            ],
            &[(after, "実装 done: 完了".into())],
            &[(after, "cluster-write once".into())],
            &[(after, "Pluvio".into())],
            since,
        );
        assert_eq!(
            lines,
            vec![
                "人: new question".to_string(),
                "タスク終了: 実装 done: 完了".to_string(),
                "認可: cluster-write once".to_string(),
                "新しい案件: Pluvio".to_string(),
            ]
        );
    }

    #[test]
    fn diff_lines_is_empty_when_nothing_changed() {
        let since = OffsetDateTime::now_utc();
        let before = since - time::Duration::seconds(1);
        let lines = diff_lines(
            &[(before, MessageRole::User, "old".into())],
            &[],
            &[],
            &[],
            since,
        );
        assert!(lines.is_empty());
    }

    #[test]
    fn summary_lines_keeps_only_the_last_n_entries() {
        let history: Vec<(MessageRole, String)> = (0..25)
            .map(|i| (MessageRole::User, format!("msg {i}")))
            .collect();
        let summary = summary_lines(&history, 20);
        assert_eq!(summary.len(), 20);
        assert_eq!(summary[0], "人: msg 5");
        assert_eq!(summary[19], "人: msg 24");
    }

    #[test]
    fn summary_lines_handles_fewer_entries_than_the_limit() {
        let history = vec![(MessageRole::User, "hi".to_string())];
        assert_eq!(summary_lines(&history, 20), vec!["人: hi".to_string()]);
    }
}
