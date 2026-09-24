//! プロバイダ別の run 集計（`docs/gui/api.md` §5.8）。task-api のメモリ内の観測値で、真実ではない（再起動で再計算）。
//!
//! 最初の `GET /providers` で `events_since(0, 5000)` を繰り返して全イベントを 1 回走査し、以後は同じ要求の時点で
//! 前回の続きから増分だけを読む。

use std::collections::{BTreeMap, HashMap};

use task_core::{Event, EventRow, StoreError, TaskStore};
use task_ops::view::RunOutcomeKind;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, UtcOffset};

use crate::types::{DailyUsage, ProviderStats};

const STATS_BATCH: usize = 5_000;
const STATS_DAYS: i64 = 30;
const UNKNOWN_PROVIDER: &str = "unknown";

#[derive(Debug, Default)]
pub(crate) struct StatsState {
    cursor: u64,
    /// `WorkerStarted` 済みで `WorkerFinished` がまだの run → provider。
    open_runs: HashMap<String, String>,
    providers: HashMap<String, Totals>,
}

#[derive(Debug, Default, Clone)]
struct Totals {
    runs: u64,
    done: u64,
    question: u64,
    error: u64,
    requeue: u64,
    lease_expired: u64,
    input_tokens: u64,
    output_tokens: u64,
    by_day: BTreeMap<Date, DayTotals>,
}

#[derive(Debug, Default, Clone, Copy)]
struct DayTotals {
    runs: u64,
    input_tokens: u64,
    output_tokens: u64,
}

/// `WorkerFinished.outcome` の分類（api.md §5.2。ディスパッチャの文字列の接頭辞と対）。
///
/// ADR-0070 D3 / P-E0-3: `infra_requeue: ` も（供給側の `requeue: ` と同じく）attempts を消費しない
/// 再試行なので `Requeue` に数える（これまで分類が無く `Error` に落ちていた不整合を直す）。
/// ADR-0072 D9/D11/D19（Phase E1）: `continue: ` は continuation（失敗ではない）。`end` があれば
/// それを優先する（分類できない古い経路は文字列判定にフォールバック）。
pub fn classify_outcome(outcome: &str, end: Option<&task_core::RunEnd>) -> RunOutcomeKind {
    if let Some(task_core::RunEnd::Yielded | task_core::RunEnd::BudgetExhausted { .. }) = end
        && outcome.starts_with("continue: ")
    {
        return RunOutcomeKind::Continued;
    }
    if outcome.starts_with("done: ") {
        RunOutcomeKind::Done
    } else if outcome.starts_with("question: ") {
        RunOutcomeKind::Question
    } else if outcome.starts_with("continue: ") {
        RunOutcomeKind::Continued
    } else if outcome.starts_with("requeue: ") || outcome.starts_with("infra_requeue: ") {
        RunOutcomeKind::Requeue
    } else if outcome.starts_with("interrupted: ") {
        // ADR-0044 D2/D8（Phase 53）: 人のコメントで止めた run は失敗ではない。
        RunOutcomeKind::Interrupted
    } else if outcome == "lease_expired" {
        RunOutcomeKind::LeaseExpired
    } else {
        RunOutcomeKind::Error
    }
}

impl StatsState {
    /// 前回の続きから最新まで読む。
    pub(crate) fn catch_up(&mut self, store: &dyn TaskStore) -> Result<(), StoreError> {
        loop {
            let rows = store.events_since(self.cursor, STATS_BATCH)?;
            let count = rows.len();
            for row in &rows {
                self.apply(row);
            }
            if count < STATS_BATCH {
                return Ok(());
            }
        }
    }

    pub(crate) fn apply(&mut self, row: &EventRow) {
        if row.id <= self.cursor {
            return;
        }
        self.cursor = row.id;
        match &row.event {
            Event::WorkerStarted {
                run_id, provider, ..
            } => {
                let provider = provider
                    .clone()
                    .unwrap_or_else(|| UNKNOWN_PROVIDER.to_string());
                self.providers.entry(provider.clone()).or_default().runs += 1;
                self.open_runs.insert(run_id.clone(), provider);
            }
            Event::WorkerFinished {
                run_id,
                outcome,
                usage,
                end,
                ..
            } => {
                let provider = self
                    .open_runs
                    .remove(run_id)
                    .unwrap_or_else(|| UNKNOWN_PROVIDER.to_string());
                let totals = self.providers.entry(provider).or_default();
                match classify_outcome(outcome, end.as_ref()) {
                    RunOutcomeKind::Done => totals.done += 1,
                    RunOutcomeKind::Question => totals.question += 1,
                    RunOutcomeKind::Error => totals.error += 1,
                    RunOutcomeKind::Requeue => totals.requeue += 1,
                    RunOutcomeKind::LeaseExpired => totals.lease_expired += 1,
                    // ADR-0044 D8: 割り込みはどの集計にも数えない（run は起きたが失敗でも成功でもない）。
                    // ADR-0072 D9/D11（Phase E1）: continuation も同様（まだ続いている。失敗ではない）。
                    RunOutcomeKind::Interrupted | RunOutcomeKind::Continued => {}
                }
                let input = usage.and_then(|u| u.input_tokens).unwrap_or(0);
                let output = usage.and_then(|u| u.output_tokens).unwrap_or(0);
                totals.input_tokens = totals.input_tokens.saturating_add(input);
                totals.output_tokens = totals.output_tokens.saturating_add(output);
                if let Some(day) = utc_day(&row.ts) {
                    let day_totals = totals.by_day.entry(day).or_default();
                    day_totals.runs += 1;
                    day_totals.input_tokens = day_totals.input_tokens.saturating_add(input);
                    day_totals.output_tokens = day_totals.output_tokens.saturating_add(output);
                }
            }
            _ => {}
        }
    }

    /// `provider` の集計。`by_day` は `today` を含む直近 30 日（UTC）。
    pub(crate) fn view(&self, provider: &str, today: Date) -> ProviderStats {
        let Some(totals) = self.providers.get(provider) else {
            return ProviderStats::default();
        };
        let first = today
            .checked_sub(time::Duration::days(STATS_DAYS - 1))
            .unwrap_or(Date::MIN);
        let by_day = totals
            .by_day
            .range(first..=today)
            .map(|(day, d)| DailyUsage {
                day: format_day(*day),
                runs: d.runs,
                input_tokens: d.input_tokens,
                output_tokens: d.output_tokens,
            })
            .collect();
        ProviderStats {
            runs: totals.runs,
            done: totals.done,
            question: totals.question,
            error: totals.error,
            requeue: totals.requeue,
            lease_expired: totals.lease_expired,
            input_tokens: totals.input_tokens,
            output_tokens: totals.output_tokens,
            by_day,
        }
    }
}

fn utc_day(ts: &str) -> Option<Date> {
    OffsetDateTime::parse(ts, &Rfc3339)
        .ok()
        .map(|t| t.to_offset(UtcOffset::UTC).date())
}

fn format_day(day: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        day.year(),
        u8::from(day.month()),
        day.day()
    )
}

/// ADR-0024/0025: `WorkerStarted.account` と対応する `WorkerFinished` から集計する（`docs/gui/api.md` §3.29 の
/// `stats`）。`StatsState` とは別のカーソルを持つ（アカウント別の集計は `GET /accounts` からしか使わないため）。
/// キーは `"<adapter>:<account id>"`（同じ id でもアダプタが違えば別のアカウントとして集計する。ADR-0025 D1）。
#[derive(Debug, Default)]
pub(crate) struct AccountStatsState {
    cursor: u64,
    /// `WorkerStarted` 済みで `WorkerFinished` がまだの run → `"<adapter>:<account id>"`（プールを使わない run
    /// は登録しない）。
    open_runs: HashMap<String, String>,
    accounts: HashMap<String, AccountTotals>,
}

/// `AccountStatsState` の内部キー（`WorkerStarted.adapter` は `"claude-code"`/`"codex"`/`"fake"` 等の
/// ワーカーアダプタ識別子で、プールのアカウントを持つ run では `AccountAdapter::as_str()` と同じ値になる）。
fn account_key(adapter: &str, account: &str) -> String {
    format!("{adapter}:{account}")
}

#[derive(Debug, Default, Clone)]
struct AccountTotals {
    runs: u64,
    done: u64,
    error: u64,
    input_tokens: u64,
    output_tokens: u64,
}

impl AccountStatsState {
    pub(crate) fn catch_up(&mut self, store: &dyn TaskStore) -> Result<(), StoreError> {
        loop {
            let rows = store.events_since(self.cursor, STATS_BATCH)?;
            let count = rows.len();
            for row in &rows {
                self.apply(row);
            }
            if count < STATS_BATCH {
                return Ok(());
            }
        }
    }

    pub(crate) fn apply(&mut self, row: &EventRow) {
        if row.id <= self.cursor {
            return;
        }
        self.cursor = row.id;
        match &row.event {
            Event::WorkerStarted {
                run_id,
                adapter,
                account: Some(account),
                ..
            } => {
                let key = account_key(adapter, account);
                self.accounts.entry(key.clone()).or_default().runs += 1;
                self.open_runs.insert(run_id.clone(), key);
            }
            Event::WorkerFinished {
                run_id,
                outcome,
                usage,
                end,
                ..
            } => {
                let Some(key) = self.open_runs.remove(run_id) else {
                    return;
                };
                let totals = self.accounts.entry(key).or_default();
                // S5: §5.8 のプロバイダ集計と同じ規則。`error` は `RunOutcomeKind::Error` だけを数える
                // （question/requeue/lease_expired/continue はエラーではない）。
                match classify_outcome(outcome, end.as_ref()) {
                    RunOutcomeKind::Done => totals.done += 1,
                    RunOutcomeKind::Error => totals.error += 1,
                    RunOutcomeKind::Question
                    | RunOutcomeKind::Requeue
                    | RunOutcomeKind::LeaseExpired
                    | RunOutcomeKind::Interrupted
                    | RunOutcomeKind::Continued => {}
                }
                let input = usage.and_then(|u| u.input_tokens).unwrap_or(0);
                let output = usage.and_then(|u| u.output_tokens).unwrap_or(0);
                totals.input_tokens = totals.input_tokens.saturating_add(input);
                totals.output_tokens = totals.output_tokens.saturating_add(output);
            }
            _ => {}
        }
    }

    pub(crate) fn view(&self, adapter: &str, account: &str) -> crate::types::AccountStats {
        let Some(totals) = self.accounts.get(&account_key(adapter, account)) else {
            return crate::types::AccountStats::default();
        };
        crate::types::AccountStats {
            runs: totals.runs,
            done: totals.done,
            error: totals.error,
            input_tokens: totals.input_tokens,
            output_tokens: totals.output_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{TaskId, Usage};

    fn row(id: u64, ts: &str, event: Event) -> EventRow {
        EventRow {
            id,
            task_id: TaskId::new(),
            seq: 0,
            ts: ts.to_string(),
            event,
        }
    }

    fn started(run_id: &str, provider: Option<&str>) -> Event {
        Event::WorkerStarted {
            run_id: run_id.into(),
            adapter: "fake".into(),
            model: "m".into(),
            provider: provider.map(str::to_string),
            account: None,
            role: None,
            task_role: None,
        }
    }

    fn finished(run_id: &str, outcome: &str, usage: Option<Usage>) -> Event {
        Event::WorkerFinished {
            run_id: run_id.into(),
            outcome: outcome.into(),
            usage,
            role: None,
            metrics: None,
            end: None,
        }
    }

    #[test]
    fn outcome_prefixes_are_classified() {
        assert_eq!(classify_outcome("done: ok", None), RunOutcomeKind::Done);
        assert_eq!(
            classify_outcome("question: which?", None),
            RunOutcomeKind::Question
        );
        assert_eq!(
            classify_outcome("requeue: throttled", None),
            RunOutcomeKind::Requeue
        );
        assert_eq!(
            classify_outcome("lease_expired", None),
            RunOutcomeKind::LeaseExpired
        );
        assert_eq!(
            classify_outcome("lease_expired: x", None),
            RunOutcomeKind::Error
        );
        // ADR-0044 D2/D8（Phase 53）: 人のコメントで止めた run は失敗ではない。
        assert_eq!(
            classify_outcome("interrupted: comment", None),
            RunOutcomeKind::Interrupted
        );
        assert_eq!(
            classify_outcome("error(retryable=true): boom", None),
            RunOutcomeKind::Error
        );
        // ADR-0070 D3 / P-E0-3: `infra_requeue: ` も `Requeue` に数える。
        assert_eq!(
            classify_outcome("infra_requeue: adapter: boom", None),
            RunOutcomeKind::Requeue
        );
        // ADR-0072 D9/D11/D19（Phase E1）: `continue: ` は continuation（失敗ではない）。
        assert_eq!(
            classify_outcome(
                "continue: budget_exhausted(turns) の続き（Run #2）",
                Some(&task_core::RunEnd::BudgetExhausted {
                    kind: task_core::BudgetKind::Turns
                })
            ),
            RunOutcomeKind::Continued
        );
        assert_eq!(
            classify_outcome("continue: yielded の続き（Run #2）", None),
            RunOutcomeKind::Continued,
            "end が無くても接頭辞だけで分類できる"
        );
    }

    #[test]
    fn runs_are_attributed_to_providers_with_daily_usage() {
        let mut stats = StatsState::default();
        let usage = |i, o| {
            Some(Usage {
                input_tokens: Some(i),
                output_tokens: o,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                cost_usd: None,
            })
        };
        stats.apply(&row(
            1,
            "2026-09-13T23:00:00Z",
            started("r1", Some("claude-a")),
        ));
        stats.apply(&row(
            2,
            "2026-09-14T00:30:00+09:00",
            finished("r1", "done: ok", usage(10, Some(5))),
        ));
        stats.apply(&row(3, "2026-09-14T01:00:00Z", started("r2", None)));
        stats.apply(&row(
            4,
            "2026-09-14T02:00:00Z",
            finished("r2", "requeue: throttled", usage(1, None)),
        ));
        stats.apply(&row(
            5,
            "2026-09-14T03:00:00Z",
            started("r3", Some("claude-a")),
        ));
        stats.apply(&row(
            2,
            "2026-09-14T03:00:00Z",
            finished("r3", "done: dup", None),
        ));

        let today = Date::from_calendar_date(2026, time::Month::September, 14).unwrap_or(Date::MIN);
        let a = stats.view("claude-a", today);
        assert_eq!(
            (a.runs, a.done, a.input_tokens, a.output_tokens),
            (2, 1, 10, 5)
        );
        assert_eq!(a.by_day.len(), 1);
        assert_eq!(a.by_day[0].day, "2026-09-13");
        let unknown = stats.view("unknown", today);
        assert_eq!(
            (unknown.runs, unknown.requeue, unknown.input_tokens),
            (1, 1, 1)
        );
        assert_eq!(stats.view("nobody", today), ProviderStats::default());

        let later = Date::from_calendar_date(2026, time::Month::November, 1).unwrap_or(Date::MIN);
        assert!(stats.view("claude-a", later).by_day.is_empty());
    }

    /// ADR-0014 D1（P-G14）: Reviewer run（role: reviewer）もプロバイダの集計に入る。
    #[test]
    fn reviewer_runs_are_counted_for_their_provider() {
        let mut stats = StatsState::default();
        let role = Some(task_core::RunRole::Reviewer);
        stats.apply(&row(
            1,
            "2026-09-14T00:00:00Z",
            Event::WorkerStarted {
                run_id: "rev".into(),
                adapter: "fake".into(),
                model: "m".into(),
                provider: Some("claude-b".into()),
                account: None,
                role,
                task_role: None,
            },
        ));
        stats.apply(&row(
            2,
            "2026-09-14T00:01:00Z",
            Event::WorkerFinished {
                run_id: "rev".into(),
                outcome: "done: reviewed".into(),
                usage: Some(Usage {
                    input_tokens: Some(3),
                    output_tokens: Some(4),
                    cache_read_tokens: None,
                    cache_creation_tokens: None,
                    cost_usd: None,
                }),
                role,
                metrics: None,
                end: None,
            },
        ));
        let today = Date::from_calendar_date(2026, time::Month::September, 14).unwrap_or(Date::MIN);
        let b = stats.view("claude-b", today);
        assert_eq!(
            (b.runs, b.done, b.input_tokens, b.output_tokens),
            (1, 1, 3, 4)
        );
    }

    fn started_with_account(run_id: &str, provider: Option<&str>, account: Option<&str>) -> Event {
        Event::WorkerStarted {
            run_id: run_id.into(),
            adapter: "claude-code".into(),
            model: "m".into(),
            provider: provider.map(str::to_string),
            account: account.map(str::to_string),
            role: None,
            task_role: None,
        }
    }

    /// ADR-0024: `WorkerStarted.account` と対応する `WorkerFinished` からアカウント別の集計を作る。
    /// プールを使わない run（`account: None`）は集計に入らない。
    #[test]
    fn account_stats_are_attributed_by_account_and_ignore_pool_less_runs() {
        let mut stats = AccountStatsState::default();
        let usage = |i, o| {
            Some(Usage {
                input_tokens: Some(i),
                output_tokens: o,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                cost_usd: None,
            })
        };
        stats.apply(&row(
            1,
            "2026-09-14T00:00:00Z",
            started_with_account("r1", Some("pool"), Some("b")),
        ));
        stats.apply(&row(
            2,
            "2026-09-14T00:01:00Z",
            finished("r1", "done: ok", usage(10, Some(5))),
        ));
        stats.apply(&row(
            3,
            "2026-09-14T00:02:00Z",
            started_with_account("r2", Some("pool"), Some("b")),
        ));
        stats.apply(&row(
            4,
            "2026-09-14T00:03:00Z",
            finished("r2", "error(retryable=false): boom", None),
        ));
        // プールを使わない run: account が無いので集計に入らない。
        stats.apply(&row(
            5,
            "2026-09-14T00:04:00Z",
            started_with_account("r3", Some("other"), None),
        ));
        stats.apply(&row(
            6,
            "2026-09-14T00:05:00Z",
            finished("r3", "done: ok", None),
        ));

        let b = stats.view("claude-code", "b");
        assert_eq!(
            (b.runs, b.done, b.error, b.input_tokens, b.output_tokens),
            (2, 1, 1, 10, 5)
        );
        assert_eq!(
            stats.view("claude-code", "a"),
            crate::types::AccountStats::default()
        );
        // 違うアダプタの同じ id は別のアカウントとして扱う（ADR-0025 D1）。
        assert_eq!(
            stats.view("codex", "b"),
            crate::types::AccountStats::default()
        );
    }

    /// S5: `error` は `RunOutcomeKind::Error` だけを数える。`question`/`requeue`/`lease_expired` はエラーではない
    /// （§5.8 のプロバイダ集計と同じ規則）。
    #[test]
    fn account_stats_error_only_counts_the_error_outcome_kind() {
        let mut stats = AccountStatsState::default();
        stats.apply(&row(
            1,
            "2026-09-14T00:00:00Z",
            started_with_account("r1", Some("pool"), Some("b")),
        ));
        stats.apply(&row(
            2,
            "2026-09-14T00:01:00Z",
            finished("r1", "question: which?", None),
        ));
        stats.apply(&row(
            3,
            "2026-09-14T00:02:00Z",
            started_with_account("r2", Some("pool"), Some("b")),
        ));
        stats.apply(&row(
            4,
            "2026-09-14T00:03:00Z",
            finished("r2", "requeue: throttled", None),
        ));
        stats.apply(&row(
            5,
            "2026-09-14T00:04:00Z",
            started_with_account("r3", Some("pool"), Some("b")),
        ));
        stats.apply(&row(
            6,
            "2026-09-14T00:05:00Z",
            finished("r3", "lease_expired", None),
        ));
        stats.apply(&row(
            7,
            "2026-09-14T00:06:00Z",
            started_with_account("r4", Some("pool"), Some("b")),
        ));
        stats.apply(&row(
            8,
            "2026-09-14T00:07:00Z",
            finished("r4", "error(retryable=true): boom", None),
        ));

        let b = stats.view("claude-code", "b");
        assert_eq!((b.runs, b.done, b.error), (4, 0, 1));
    }
}
