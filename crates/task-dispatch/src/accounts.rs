//! アカウントプールの純粋ロジック（ADR-0024 D1, D3, D4。ADR-0025 でアダプタの次元を追加）。
//!
//! ここは決定的なコードだけで構成する。LLM は呼ばない（DESIGN 原則 1）。`dispatcher.rs` への統合は別ステップで行う。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use task_core::{AccountAdapter, RateLimitObservation, RateWindow};

/// `^[A-Za-z0-9_-]{1,64}$`（先頭 `.` は文字集合に含まれないため自動的に拒否される。ADR-0024 D1）。
pub fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `accounts_dir` 直下の 1 ディレクトリ（ADR-0024 D1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDir {
    pub id: String,
    pub dir: PathBuf,
    pub logged_in: bool,
}

/// `root` の下の、有効な id を持つサブディレクトリを id 昇順で返す。`.` で始まる名前・ファイル・無効な id は飛ばす。
/// `root` が存在しなければ空を返す。ログイン済みの判定に使うファイル（`adapter.credentials_marker()`）の中身は
/// 読まない（存在だけを見る。ADR-0025 D1: claude-code は `.credentials.json`、codex は `auth.json`）。
pub fn scan_accounts(root: &Path, adapter: AccountAdapter) -> Vec<AccountDir> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
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
        out.push(AccountDir {
            id: name,
            dir: path,
            logged_in,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// アカウントを cooldown にした理由（ADR-0024 D4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountCooldownReason {
    AuthFailed,
    Throttled,
    Exhausted,
}

/// アカウントの cooldown（Unix 秒の `until` まで選ばれない）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AccountCooldown {
    pub until: i64,
    pub reason: AccountCooldownReason,
}

/// 観測値の出所（ADR-0024 D4 / D6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    Run,
    Check,
}

/// D6 の手動確認の結果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountCheckRecord {
    pub at: i64,
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 1 アカウント分の観測値・cooldown・最終確認。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<RateLimitObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ObservationSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<AccountCooldown>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<AccountCheckRecord>,
}

/// 永続化するファイルの形（`{"version":1,"accounts":{"<id>":{...}}}`）。
#[derive(Debug, Serialize, Deserialize)]
struct BookFile {
    version: u32,
    accounts: BTreeMap<String, AccountState>,
}

/// アカウントごとの観測値・cooldown・最終確認の帳簿（ADR-0024 D4）。タスクの真実ではないので replay の対象外。
///
/// **どのメソッドも自動では保存しない。** 呼び出し側が変更後に必要なら `save()` を呼ぶこと。
#[derive(Debug, Default)]
pub struct AccountBook {
    states: BTreeMap<String, AccountState>,
    path: Option<PathBuf>,
}

impl AccountBook {
    /// 保存先を持たない帳簿（テストや `celerisctl worker run` 向け）。`save` は no-op。
    pub fn new_in_memory() -> Self {
        Self {
            states: BTreeMap::new(),
            path: None,
        }
    }

    /// `path` から読む。ファイルが無ければ空から始める（`path` は覚えておき、以後の `save` で使う）。
    /// 読めない・壊れているときは warn を出し、空から始める（`path` は覚えたまま）。
    pub fn load(path: &Path) -> Self {
        let mut book = Self {
            states: BTreeMap::new(),
            path: Some(path.to_path_buf()),
        };
        match fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<BookFile>(&text) {
                Ok(file) => book.states = file.accounts,
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "account book: corrupt file, starting empty");
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "account book: unreadable file, starting empty");
            }
        }
        book
    }

    /// 観測値を記録する。`observed_at` が既存以上に新しいときに差し替える（N1: 同時刻なら新しく渡された方が勝つ）。
    pub fn record_observation(
        &mut self,
        id: &str,
        obs: RateLimitObservation,
        source: ObservationSource,
    ) {
        let state = self.states.entry(id.to_string()).or_default();
        let newer = state
            .usage
            .as_ref()
            .is_none_or(|u| obs.observed_at >= u.observed_at);
        if newer {
            state.usage = Some(obs);
            state.source = Some(source);
        }
    }

    /// D6 の確認結果を記録する（常に上書き。呼び出し側が最新の確認だけを渡す想定）。
    pub fn record_check(&mut self, id: &str, record: AccountCheckRecord) {
        let state = self.states.entry(id.to_string()).or_default();
        state.last_check = Some(record);
    }

    /// cooldown を設定する（N2）。`AuthFailed` は理由を最優先で上書きする（`until` は長い方を残す。GUI に
    /// 「再ログインが必要」を出し続けるため）。既存が未失効（`until > now`）の `AuthFailed` なら、
    /// `Throttled`/`Exhausted` を設定しようとしても理由は `AuthFailed` のまま `until` だけ伸ばす。
    /// それ以外は `until` が遅い方を残す。
    pub fn set_cooldown(&mut self, id: &str, cooldown: AccountCooldown, now: i64) {
        let state = self.states.entry(id.to_string()).or_default();
        let next = match state.cooldown {
            None => cooldown,
            Some(existing) if cooldown.reason == AccountCooldownReason::AuthFailed => {
                AccountCooldown {
                    until: cooldown.until.max(existing.until),
                    reason: AccountCooldownReason::AuthFailed,
                }
            }
            Some(existing)
                if existing.reason == AccountCooldownReason::AuthFailed && existing.until > now =>
            {
                AccountCooldown {
                    until: cooldown.until.max(existing.until),
                    reason: AccountCooldownReason::AuthFailed,
                }
            }
            Some(existing) if cooldown.until > existing.until => cooldown,
            Some(existing) => existing,
        };
        state.cooldown = Some(next);
    }

    /// `until <= now` の cooldown を消す（アカウント自体は残す）。
    pub fn clear_expired(&mut self, now: i64) {
        for state in self.states.values_mut() {
            if state.cooldown.as_ref().is_some_and(|c| c.until <= now) {
                state.cooldown = None;
            }
        }
    }

    /// 認証の再確認が成功したときの解除。新しい観測値による枯渇判定は evaluate が行う。
    pub fn clear_cooldown(&mut self, id: &str) {
        if let Some(state) = self.states.get_mut(id) {
            state.cooldown = None;
        }
    }

    pub fn clear_observation(&mut self, id: &str) {
        if let Some(state) = self.states.get_mut(id) {
            state.usage = None;
            state.source = None;
        }
    }

    pub fn state(&self, id: &str) -> Option<&AccountState> {
        self.states.get(id)
    }

    pub fn states(&self) -> &BTreeMap<String, AccountState> {
        &self.states
    }

    pub fn remove(&mut self, id: &str) -> Option<AccountState> {
        self.states.remove(id)
    }

    /// `<path>.tmp` に書いてから `path` へ rename する。in-memory（`path` 無し）なら no-op。
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = BookFile {
            version: 1,
            accounts: self.states.clone(),
        };
        let json = serde_json::to_vec_pretty(&file)?;
        let mut tmp_name = path.as_os_str().to_os_string();
        tmp_name.push(".tmp");
        let tmp_path = PathBuf::from(tmp_name);
        fs::write(&tmp_path, json)?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

/// 窓 1 つの実効使用率（ADR-0024 D3-1）: 観測があり `now < resets_at` ならその `utilization`、それ以外 0。
fn effective_utilization(w: RateWindow, now: i64) -> f64 {
    if now < w.resets_at {
        w.utilization
    } else {
        0.0
    }
}

/// 供給側失敗の cooldown 期限を決める（ADR-0024 D4）。
///
/// `AuthFailed` は常に `now + fallback_secs`。`Throttled` / `Exhausted` は、直近の観測に実効使用率
/// `EXHAUSTED_UTILIZATION` 以上の窓、または `status == "rejected"` かつ `resets_at > now` があれば、
/// それらのうち最も遅い `resets_at` を使う。無ければ `now + fallback_secs`。
pub fn cooldown_for_failure(
    state: Option<&AccountState>,
    reason: AccountCooldownReason,
    now: i64,
    fallback_secs: u64,
) -> AccountCooldown {
    let fallback_until = now + fallback_secs as i64;
    if matches!(reason, AccountCooldownReason::AuthFailed) {
        return AccountCooldown {
            until: fallback_until,
            reason,
        };
    }
    let mut latest: Option<i64> = None;
    let mut consider = |resets_at: i64| {
        latest = Some(latest.map_or(resets_at, |l| l.max(resets_at)));
    };
    if let Some(obs) = state.and_then(|s| s.usage.as_ref()) {
        for w in [obs.five_hour, obs.seven_day].into_iter().flatten() {
            if effective_utilization(w, now) >= EXHAUSTED_UTILIZATION {
                consider(w.resets_at);
            }
        }
        if obs.status.as_deref() == Some("rejected")
            && let Some(r) = obs.resets_at
            && r > now
        {
            consider(r);
        }
    }
    AccountCooldown {
        until: latest.unwrap_or(fallback_until),
        reason,
    }
}

/// 除外理由（ADR-0024 D3-2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcludedReason {
    NotLoggedIn,
    AtCapacity,
    Cooldown,
    FiveHourExhausted,
    SevenDayExhausted,
    Rejected,
}

/// 選択の候補（ディスパッチャが `scan_accounts` と in_use の集計から作る）。
#[derive(Debug, Clone, Copy)]
pub struct AccountCandidate<'a> {
    pub id: &'a str,
    pub logged_in: bool,
    pub in_use: usize,
}

/// 1 アカウントの評価結果。`excluded` が無ければ `score` がある。
#[derive(Debug, Clone, PartialEq)]
pub struct AccountEvaluation {
    pub id: String,
    pub score: Option<f64>,
    pub excluded: Option<ExcludedReason>,
}

pub const FIVE_HOUR_SECS: i64 = 18_000;
pub const SEVEN_DAY_SECS: i64 = 604_800;
pub const EXHAUSTED_UTILIZATION: f64 = 0.97;
pub const IN_USE_PENALTY: f64 = 0.05;
pub const MIN_WEEK_FRACTION: f64 = 0.1;

/// 1 アカウントを評価する（ADR-0024 D3）。除外の判定順: 未ログイン → cooldown → 上限 → rejected →
/// 5 時間枠の枯渇 → 週次枠の枯渇。除外されなければスコアを計算する。
pub fn evaluate(
    c: &AccountCandidate<'_>,
    state: Option<&AccountState>,
    max_runs_per_account: usize,
    now: i64,
) -> AccountEvaluation {
    let id = c.id.to_string();
    let excluded = |reason: ExcludedReason| AccountEvaluation {
        id: id.clone(),
        score: None,
        excluded: Some(reason),
    };

    if !c.logged_in {
        return excluded(ExcludedReason::NotLoggedIn);
    }
    if state
        .and_then(|s| s.cooldown.as_ref())
        .is_some_and(|cd| cd.until > now)
    {
        return excluded(ExcludedReason::Cooldown);
    }
    if c.in_use >= max_runs_per_account {
        return excluded(ExcludedReason::AtCapacity);
    }
    let obs = state.and_then(|s| s.usage.as_ref());
    if let Some(o) = obs
        && o.status.as_deref() == Some("rejected")
    {
        let rejected = match o.resets_at {
            Some(r) => r > now,
            None => now - o.observed_at <= FIVE_HOUR_SECS,
        };
        if rejected {
            return excluded(ExcludedReason::Rejected);
        }
    }

    let five_hour = obs.and_then(|o| o.five_hour);
    let u5 = five_hour.map_or(0.0, |w| effective_utilization(w, now));
    if u5 >= EXHAUSTED_UTILIZATION {
        return excluded(ExcludedReason::FiveHourExhausted);
    }
    let seven_day = obs.and_then(|o| o.seven_day);
    let u7 = seven_day.map_or(0.0, |w| effective_utilization(w, now));
    if u7 >= EXHAUSTED_UTILIZATION {
        return excluded(ExcludedReason::SevenDayExhausted);
    }

    let score = if obs.is_none() {
        1.0 - IN_USE_PENALTY * c.in_use as f64
    } else {
        let h5 = 1.0 - u5;
        let h7 = match seven_day {
            // ADR-0024 D3: `h_7d = min(1, (1 − u_7d) / max(t_7d, 0.1))`。上限はクランプしない
            // （`resets_at` が 7 日を超えていれば `t7 > 1` になり、その分 `h7` は下がる。S4）。
            Some(w) if now < w.resets_at => {
                let t7 =
                    ((w.resets_at - now) as f64 / SEVEN_DAY_SECS as f64).max(MIN_WEEK_FRACTION);
                ((1.0 - u7) / t7).min(1.0)
            }
            _ => 1.0 - u7,
        };
        h5.min(h7) - IN_USE_PENALTY * c.in_use as f64
    };
    AccountEvaluation {
        id,
        score: Some(score),
        excluded: None,
    }
}

/// スコア最大のアカウントを選ぶ。同点は `in_use` の少ない方、次に `id` の昇順（ADR-0024 D3-4）。
/// 選べる候補が無ければ `None`。
pub fn select_account(
    cands: &[AccountCandidate<'_>],
    book: &AccountBook,
    max_runs_per_account: usize,
    now: i64,
) -> Option<String> {
    let mut best: Option<(String, f64, usize)> = None;
    for c in cands {
        let eval = evaluate(c, book.state(c.id), max_runs_per_account, now);
        let Some(score) = eval.score else { continue };
        let take = match &best {
            None => true,
            Some((best_id, best_score, best_in_use)) => match score
                .partial_cmp(best_score)
                .unwrap_or(std::cmp::Ordering::Equal)
            {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => match c.in_use.cmp(best_in_use) {
                    std::cmp::Ordering::Less => true,
                    std::cmp::Ordering::Greater => false,
                    std::cmp::Ordering::Equal => c.id < best_id.as_str(),
                },
            },
        };
        if take {
            best = Some((c.id.to_string(), score, c.in_use));
        }
    }
    best.map(|(id, _, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(utilization: f64, resets_at: i64) -> RateWindow {
        RateWindow {
            utilization,
            resets_at,
        }
    }

    fn obs(
        five_hour: Option<RateWindow>,
        seven_day: Option<RateWindow>,
        observed_at: i64,
    ) -> RateLimitObservation {
        RateLimitObservation {
            five_hour,
            seven_day,
            status: None,
            resets_at: None,
            observed_at,
        }
    }

    fn state_with_usage(usage: RateLimitObservation) -> AccountState {
        AccountState {
            usage: Some(usage),
            ..Default::default()
        }
    }

    fn state_with_cooldown(cooldown: AccountCooldown) -> AccountState {
        AccountState {
            cooldown: Some(cooldown),
            ..Default::default()
        }
    }

    // ---- valid_account_id ----

    #[test]
    fn valid_account_id_table() {
        let cases: &[(&str, bool)] = &[
            ("a", true),
            ("acct-1_ok", true),
            ("A0", true),
            (&"x".repeat(64), true),
            (&"x".repeat(65), false),
            ("", false),
            (".hidden", false),
            ("has space", false),
            ("has/slash", false),
            ("has.dot", false),
            ("emoji-🙂", false),
        ];
        for (id, expected) in cases {
            assert_eq!(valid_account_id(id), *expected, "id={id:?}");
        }
    }

    // ---- scan_accounts ----

    #[test]
    fn scan_accounts_finds_valid_dirs_sorted_with_login_state() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        fs::create_dir(root.join("bravo")).expect("mkdir");
        fs::write(root.join("bravo").join(".credentials.json"), "{}").expect("write");
        fs::create_dir(root.join("alpha")).expect("mkdir");
        fs::create_dir(root.join(".removed")).expect("mkdir"); // dot-prefixed: skipped
        fs::create_dir(root.join("bad name")).expect("mkdir"); // invalid id: skipped
        fs::write(root.join("not-a-dir"), "file").expect("write"); // file: skipped

        let found = scan_accounts(root, AccountAdapter::ClaudeCode);
        assert_eq!(
            found,
            vec![
                AccountDir {
                    id: "alpha".into(),
                    dir: root.join("alpha"),
                    logged_in: false
                },
                AccountDir {
                    id: "bravo".into(),
                    dir: root.join("bravo"),
                    logged_in: true
                },
            ]
        );
    }

    #[test]
    fn scan_accounts_missing_root_is_empty() {
        let tmp = tempfile::tempdir().expect("tmp");
        let missing = tmp.path().join("does-not-exist");
        assert_eq!(
            scan_accounts(&missing, AccountAdapter::ClaudeCode),
            Vec::new()
        );
    }

    /// ADR-0025 D1: codex は `auth.json` の有無でログイン済みを判定する（claude-code は `.credentials.json`）。
    #[test]
    fn scan_accounts_uses_the_adapter_specific_credentials_marker() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        fs::create_dir(root.join("a")).expect("mkdir");
        fs::write(root.join("a").join("auth.json"), "{}").expect("write");
        fs::write(root.join("a").join(".credentials.json"), "{}").expect("write");
        fs::create_dir(root.join("b")).expect("mkdir");
        fs::write(root.join("b").join(".credentials.json"), "{}").expect("write");

        let codex = scan_accounts(root, AccountAdapter::Codex);
        assert!(codex.iter().find(|d| d.id == "a").expect("a").logged_in);
        assert!(!codex.iter().find(|d| d.id == "b").expect("b").logged_in);

        let claude = scan_accounts(root, AccountAdapter::ClaudeCode);
        assert!(claude.iter().find(|d| d.id == "a").expect("a").logged_in);
        assert!(claude.iter().find(|d| d.id == "b").expect("b").logged_in);
    }

    // ---- AccountBook: save/load, corruption, newest-wins, cooldown max ----

    #[test]
    fn book_save_and_load_round_trip() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("usage.json");
        let mut book = AccountBook::load(&path); // missing file -> empty, path remembered
        assert!(book.state("a").is_none());

        book.record_observation(
            "a",
            obs(Some(window(0.5, 100)), None, 10),
            ObservationSource::Run,
        );
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 200,
                reason: AccountCooldownReason::Throttled,
            },
            0,
        );
        book.record_check(
            "b",
            AccountCheckRecord {
                at: 5,
                result: "ok".into(),
                detail: None,
            },
        );
        book.save().expect("save");

        assert!(path.exists());
        assert!(!tmp.path().join("usage.json.tmp").exists());

        let reloaded = AccountBook::load(&path);
        assert_eq!(
            reloaded
                .state("a")
                .expect("a")
                .usage
                .as_ref()
                .expect("usage")
                .observed_at,
            10
        );
        assert_eq!(
            reloaded
                .state("a")
                .expect("a")
                .cooldown
                .as_ref()
                .expect("cd")
                .until,
            200
        );
        assert_eq!(
            reloaded
                .state("b")
                .expect("b")
                .last_check
                .as_ref()
                .expect("chk")
                .result,
            "ok"
        );
    }

    #[test]
    fn book_load_missing_file_starts_empty_but_remembers_path() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("nope.json");
        let mut book = AccountBook::load(&path);
        assert!(book.states().is_empty());
        book.record_check(
            "a",
            AccountCheckRecord {
                at: 1,
                result: "ok".into(),
                detail: None,
            },
        );
        book.save().expect("save"); // path was remembered even though file was missing at load time
        assert!(path.exists());
    }

    #[test]
    fn book_load_corrupt_file_warns_and_starts_empty() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("usage.json");
        fs::write(&path, "{ not json").expect("write");
        let book = AccountBook::load(&path);
        assert!(book.states().is_empty());
    }

    #[test]
    fn book_in_memory_save_is_noop() {
        let mut book = AccountBook::new_in_memory();
        book.record_check(
            "a",
            AccountCheckRecord {
                at: 1,
                result: "ok".into(),
                detail: None,
            },
        );
        assert!(book.save().is_ok());
    }

    #[test]
    fn book_record_observation_keeps_newest_by_observed_at() {
        let mut book = AccountBook::new_in_memory();
        book.record_observation(
            "a",
            obs(Some(window(0.1, 100)), None, 10),
            ObservationSource::Run,
        );
        book.record_observation(
            "a",
            obs(Some(window(0.9, 200)), None, 5),
            ObservationSource::Check,
        ); // older: ignored
        assert_eq!(
            book.state("a")
                .expect("a")
                .usage
                .as_ref()
                .expect("u")
                .observed_at,
            10
        );
        assert_eq!(
            book.state("a").expect("a").source,
            Some(ObservationSource::Run)
        );

        book.record_observation(
            "a",
            obs(Some(window(0.5, 300)), None, 20),
            ObservationSource::Check,
        ); // newer: replaces
        assert_eq!(
            book.state("a")
                .expect("a")
                .usage
                .as_ref()
                .expect("u")
                .observed_at,
            20
        );
        assert_eq!(
            book.state("a").expect("a").source,
            Some(ObservationSource::Check)
        );
    }

    /// N1: 同じ `observed_at` なら、新しく渡された方（後着）が勝つ（`>=`）。
    #[test]
    fn book_record_observation_ties_prefer_the_incoming_one() {
        let mut book = AccountBook::new_in_memory();
        book.record_observation(
            "a",
            obs(Some(window(0.1, 100)), None, 10),
            ObservationSource::Run,
        );
        book.record_observation(
            "a",
            obs(Some(window(0.9, 200)), None, 10),
            ObservationSource::Check,
        ); // same observed_at: wins
        assert_eq!(
            book.state("a")
                .expect("a")
                .usage
                .as_ref()
                .expect("u")
                .five_hour
                .map(|w| w.utilization),
            Some(0.9)
        );
        assert_eq!(
            book.state("a").expect("a").source,
            Some(ObservationSource::Check)
        );
    }

    #[test]
    fn book_set_cooldown_keeps_the_later_until_for_non_auth_failed_reasons() {
        let mut book = AccountBook::new_in_memory();
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 100,
                reason: AccountCooldownReason::Throttled,
            },
            0,
        );
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 50,
                reason: AccountCooldownReason::Exhausted,
            },
            0,
        ); // earlier: ignored
        let cd = book.state("a").expect("a").cooldown.as_ref().expect("cd");
        assert_eq!(
            (cd.until, cd.reason),
            (100, AccountCooldownReason::Throttled)
        );
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 150,
                reason: AccountCooldownReason::Exhausted,
            },
            0,
        ); // later: replaces
        let cd = book.state("a").expect("a").cooldown.as_ref().expect("cd");
        assert_eq!(cd.until, 150);
        assert_eq!(cd.reason, AccountCooldownReason::Exhausted);
    }

    /// N2: `AuthFailed` は理由を最優先で上書きする（`until` は長い方が残る）。
    #[test]
    fn book_set_cooldown_auth_failed_always_overwrites_the_reason() {
        let mut book = AccountBook::new_in_memory();
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 100,
                reason: AccountCooldownReason::Throttled,
            },
            0,
        );
        // AuthFailed の until が既存より短くても、理由は AuthFailed になり until は長い方 (max) を保つ。
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 50,
                reason: AccountCooldownReason::AuthFailed,
            },
            0,
        );
        let cd = book.state("a").expect("a").cooldown.as_ref().expect("cd");
        assert_eq!(cd.reason, AccountCooldownReason::AuthFailed);
        assert_eq!(cd.until, 100);
    }

    /// N2: 未失効の `AuthFailed` があるときに `Throttled`/`Exhausted` を設定しても、理由は `AuthFailed` のまま
    /// （GUI に再ログインが必要と出し続ける）。`until` は長い方。
    #[test]
    fn book_set_cooldown_keeps_unexpired_auth_failed_reason_when_throttled_follows() {
        let mut book = AccountBook::new_in_memory();
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 1_000,
                reason: AccountCooldownReason::AuthFailed,
            },
            0,
        );
        // now(0) < until(1_000): まだ失効していない。
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 2_000,
                reason: AccountCooldownReason::Throttled,
            },
            0,
        );
        let cd = book.state("a").expect("a").cooldown.as_ref().expect("cd");
        assert_eq!(cd.reason, AccountCooldownReason::AuthFailed);
        assert_eq!(cd.until, 2_000);
    }

    /// N2: `AuthFailed` が失効した後は、`Throttled`/`Exhausted` が普通に理由を上書きする。
    #[test]
    fn book_set_cooldown_expired_auth_failed_does_not_block_new_reason() {
        let mut book = AccountBook::new_in_memory();
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 1_000,
                reason: AccountCooldownReason::AuthFailed,
            },
            0,
        );
        // now(2_000) >= until(1_000): 既に失効している。
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 3_000,
                reason: AccountCooldownReason::Throttled,
            },
            2_000,
        );
        let cd = book.state("a").expect("a").cooldown.as_ref().expect("cd");
        assert_eq!(cd.reason, AccountCooldownReason::Throttled);
        assert_eq!(cd.until, 3_000);
    }

    #[test]
    fn book_clear_expired_removes_only_past_cooldowns() {
        let mut book = AccountBook::new_in_memory();
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 100,
                reason: AccountCooldownReason::Throttled,
            },
            0,
        );
        book.set_cooldown(
            "b",
            AccountCooldown {
                until: 300,
                reason: AccountCooldownReason::Throttled,
            },
            0,
        );
        book.clear_expired(200);
        assert!(book.state("a").expect("a").cooldown.is_none());
        assert!(book.state("b").expect("b").cooldown.is_some());
    }

    #[test]
    fn book_remove_drops_the_state() {
        let mut book = AccountBook::new_in_memory();
        book.record_check(
            "a",
            AccountCheckRecord {
                at: 1,
                result: "ok".into(),
                detail: None,
            },
        );
        assert!(book.remove("a").is_some());
        assert!(book.state("a").is_none());
        assert!(book.remove("a").is_none());
    }

    // ---- cooldown_for_failure ----

    #[test]
    fn cooldown_for_failure_auth_failed_uses_fallback() {
        let cd = cooldown_for_failure(None, AccountCooldownReason::AuthFailed, 1_000, 60);
        assert_eq!(
            cd,
            AccountCooldown {
                until: 1_060,
                reason: AccountCooldownReason::AuthFailed
            }
        );
    }

    #[test]
    fn cooldown_for_failure_throttled_no_observation_uses_fallback() {
        let cd = cooldown_for_failure(None, AccountCooldownReason::Throttled, 1_000, 60);
        assert_eq!(cd.until, 1_060);
    }

    #[test]
    fn cooldown_for_failure_uses_latest_exhausted_window_reset() {
        let state = state_with_usage(obs(
            Some(window(0.99, 1_500)),
            Some(window(0.98, 3_000)),
            900,
        ));
        let cd = cooldown_for_failure(Some(&state), AccountCooldownReason::Exhausted, 1_000, 60);
        assert_eq!(cd.until, 3_000); // later of the two exhausted windows
    }

    #[test]
    fn cooldown_for_failure_uses_rejected_resets_at() {
        let mut o = obs(None, None, 900);
        o.status = Some("rejected".into());
        o.resets_at = Some(2_000);
        let state = state_with_usage(o);
        let cd = cooldown_for_failure(Some(&state), AccountCooldownReason::Throttled, 1_000, 60);
        assert_eq!(cd.until, 2_000);
    }

    #[test]
    fn cooldown_for_failure_ignores_rejected_reset_already_passed() {
        let mut o = obs(None, None, 900);
        o.status = Some("rejected".into());
        o.resets_at = Some(500); // already in the past
        let state = state_with_usage(o);
        let cd = cooldown_for_failure(Some(&state), AccountCooldownReason::Throttled, 1_000, 60);
        assert_eq!(cd.until, 1_060); // fallback
    }

    #[test]
    fn cooldown_for_failure_below_threshold_uses_fallback() {
        let state = state_with_usage(obs(Some(window(0.5, 5_000)), None, 900));
        let cd = cooldown_for_failure(Some(&state), AccountCooldownReason::Throttled, 1_000, 60);
        assert_eq!(cd.until, 1_060);
    }

    // ---- evaluate / select_account ----

    fn cand(id: &str, logged_in: bool, in_use: usize) -> AccountCandidate<'_> {
        AccountCandidate {
            id,
            logged_in,
            in_use,
        }
    }

    #[test]
    fn evaluate_no_observation_scores_one_minus_penalty() {
        let e = evaluate(&cand("a", true, 0), None, 2, 1_000);
        assert_eq!(e.excluded, None);
        assert_eq!(e.score, Some(1.0));
        let e2 = evaluate(&cand("a", true, 2), None, 5, 1_000);
        assert_eq!(e2.score, Some(1.0 - IN_USE_PENALTY * 2.0));
    }

    #[test]
    fn evaluate_higher_headroom_wins() {
        let low = state_with_usage(obs(Some(window(0.1, 6_000)), None, 900));
        let high = state_with_usage(obs(Some(window(0.8, 6_000)), None, 900));
        let e_low = evaluate(&cand("low-usage", true, 0), Some(&low), 2, 1_000);
        let e_high = evaluate(&cand("high-usage", true, 0), Some(&high), 2, 1_000);
        assert!(e_low.score.unwrap() > e_high.score.unwrap());
    }

    #[test]
    fn evaluate_window_reset_in_the_past_counts_as_zero() {
        let state = state_with_usage(obs(Some(window(0.99, 500)), None, 100)); // resets_at 500 < now 1000: effective 0
        let e = evaluate(&cand("a", true, 0), Some(&state), 2, 1_000);
        assert_eq!(e.excluded, None); // not exhausted because effective utilization is 0
        assert_eq!(e.score, Some(1.0));
    }

    #[test]
    fn evaluate_exhausted_five_hour_is_excluded() {
        let state = state_with_usage(obs(Some(window(0.97, 5_000)), None, 900));
        let e = evaluate(&cand("a", true, 0), Some(&state), 2, 1_000);
        assert_eq!(e.excluded, Some(ExcludedReason::FiveHourExhausted));
        assert_eq!(e.score, None);
    }

    #[test]
    fn evaluate_exhausted_seven_day_is_excluded() {
        let state = state_with_usage(obs(None, Some(window(0.99, 500_000)), 900));
        let e = evaluate(&cand("a", true, 0), Some(&state), 2, 1_000);
        assert_eq!(e.excluded, Some(ExcludedReason::SevenDayExhausted));
    }

    #[test]
    fn evaluate_rejected_is_excluded_until_reset() {
        let mut o = obs(None, None, 900);
        o.status = Some("rejected".into());
        o.resets_at = Some(5_000);
        let state = state_with_usage(o);
        let excluded = evaluate(&cand("a", true, 0), Some(&state), 2, 1_000);
        assert_eq!(excluded.excluded, Some(ExcludedReason::Rejected));

        let after_reset = evaluate(&cand("a", true, 0), Some(&state), 2, 6_000);
        assert_ne!(after_reset.excluded, Some(ExcludedReason::Rejected));
    }

    #[test]
    fn evaluate_rejected_without_resets_at_uses_five_hour_window_as_fallback() {
        let mut o = obs(None, None, 1_000);
        o.status = Some("rejected".into());
        o.resets_at = None;
        let state = state_with_usage(o);

        // observed recently (within FIVE_HOUR_SECS): still treated as rejected
        let recent = evaluate(
            &cand("a", true, 0),
            Some(&state),
            2,
            1_000 + FIVE_HOUR_SECS - 1,
        );
        assert_eq!(recent.excluded, Some(ExcludedReason::Rejected));

        // observed long ago: no longer treated as rejected
        let stale = evaluate(
            &cand("a", true, 0),
            Some(&state),
            2,
            1_000 + FIVE_HOUR_SECS + 1,
        );
        assert_ne!(stale.excluded, Some(ExcludedReason::Rejected));
    }

    #[test]
    fn evaluate_weekly_pacing_sooner_reset_beats_later_reset_at_same_utilization() {
        let now = 0;
        let soon = state_with_usage(obs(None, Some(window(0.5, 86_400)), now)); // resets in 1 day
        let later = state_with_usage(obs(None, Some(window(0.5, 6 * 86_400)), now)); // resets in 6 days

        let e_soon = evaluate(&cand("soon", true, 0), Some(&soon), 2, now);
        let e_later = evaluate(&cand("later", true, 0), Some(&later), 2, now);
        assert!(e_soon.score.unwrap() > e_later.score.unwrap());
    }

    /// S4: `t7` は `.max(MIN_WEEK_FRACTION)` のみで、上限はクランプしない（ADR-0024 D3）。`resets_at` が
    /// 7 日を超えていれば `t7 > 1` になり、その分 `h7`（ひいては `score`）はさらに下がる。
    #[test]
    fn evaluate_weekly_pacing_is_not_capped_beyond_one_week() {
        let now = 0;
        let within_week = state_with_usage(obs(None, Some(window(0.5, SEVEN_DAY_SECS)), now)); // t7 == 1
        let beyond_week = state_with_usage(obs(None, Some(window(0.5, SEVEN_DAY_SECS * 2)), now)); // t7 == 2 (uncapped)

        let e_within = evaluate(&cand("within", true, 0), Some(&within_week), 2, now);
        let e_beyond = evaluate(&cand("beyond", true, 0), Some(&beyond_week), 2, now);
        assert!(e_within.score.unwrap() > e_beyond.score.unwrap());
        // t7 == 2 なら h7 = (1 - 0.5) / 2 = 0.25 で、それが h5(=1.0) より小さいので min(h5, h7) == 0.25。
        assert!(
            (e_beyond.score.unwrap() - 0.25).abs() < 1e-9,
            "{:?}",
            e_beyond.score
        );
    }

    #[test]
    fn evaluate_unknown_account_preferred_over_forty_percent_used() {
        let used = state_with_usage(obs(Some(window(0.4, 6_000)), None, 900));
        let e_unknown = evaluate(&cand("unknown", true, 0), None, 2, 1_000);
        let e_used = evaluate(&cand("used", true, 0), Some(&used), 2, 1_000);
        assert!(e_unknown.score.unwrap() > e_used.score.unwrap());
    }

    #[test]
    fn evaluate_in_use_penalty_lowers_score() {
        let e_idle = evaluate(&cand("a", true, 0), None, 5, 1_000);
        let e_busy = evaluate(&cand("a", true, 3), None, 5, 1_000);
        assert!(e_idle.score.unwrap() > e_busy.score.unwrap());
        assert!(
            (e_idle.score.unwrap() - e_busy.score.unwrap() - IN_USE_PENALTY * 3.0).abs() < 1e-9
        );
    }

    #[test]
    fn evaluate_at_capacity_is_excluded() {
        let e = evaluate(&cand("a", true, 2), None, 2, 1_000);
        assert_eq!(e.excluded, Some(ExcludedReason::AtCapacity));
    }

    #[test]
    fn evaluate_cooldown_excludes_until_expiry() {
        let state = state_with_cooldown(AccountCooldown {
            until: 2_000,
            reason: AccountCooldownReason::Throttled,
        });
        let during = evaluate(&cand("a", true, 0), Some(&state), 2, 1_000);
        assert_eq!(during.excluded, Some(ExcludedReason::Cooldown));
        let after = evaluate(&cand("a", true, 0), Some(&state), 2, 2_000); // until == now: no longer excluded
        assert_ne!(after.excluded, Some(ExcludedReason::Cooldown));
    }

    #[test]
    fn evaluate_not_logged_in_is_excluded() {
        let e = evaluate(&cand("a", false, 0), None, 2, 1_000);
        assert_eq!(e.excluded, Some(ExcludedReason::NotLoggedIn));
    }

    #[test]
    fn select_account_deterministic_tie_break_by_in_use_then_id() {
        // Equal scores (no observations, same in_use=0) -> id ascending.
        let book = AccountBook::new_in_memory();
        let cands = vec![
            cand("zebra", true, 0),
            cand("alpha", true, 0),
            cand("mid", true, 0),
        ];
        assert_eq!(
            select_account(&cands, &book, 5, 1_000),
            Some("alpha".to_string())
        );

        // Equal scores by observation, but different in_use -> fewer in_use wins.
        let mut book2 = AccountBook::new_in_memory();
        book2.record_observation(
            "a",
            obs(Some(window(0.2, 9_000)), None, 1),
            ObservationSource::Run,
        );
        book2.record_observation(
            "b",
            obs(Some(window(0.2, 9_000)), None, 1),
            ObservationSource::Run,
        );
        let cands2 = vec![cand("a", true, 1), cand("b", true, 0)];
        assert_eq!(
            select_account(&cands2, &book2, 5, 1_000),
            Some("b".to_string())
        );
    }

    #[test]
    fn select_account_all_excluded_returns_none() {
        let mut book = AccountBook::new_in_memory();
        book.set_cooldown(
            "a",
            AccountCooldown {
                until: 5_000,
                reason: AccountCooldownReason::Throttled,
            },
            0,
        );
        let cands = vec![cand("a", true, 0), cand("b", false, 0), cand("c", true, 9)];
        assert_eq!(select_account(&cands, &book, 3, 1_000), None);
    }

    #[test]
    fn select_account_no_candidates_returns_none() {
        let book = AccountBook::new_in_memory();
        assert_eq!(select_account(&[], &book, 3, 1_000), None);
    }

    #[test]
    fn select_account_picks_highest_score_among_mixed_states() {
        let mut book = AccountBook::new_in_memory();
        book.record_observation(
            "nearly-full",
            obs(Some(window(0.9, 9_000)), None, 1),
            ObservationSource::Run,
        );
        book.set_cooldown(
            "cooling",
            AccountCooldown {
                until: 9_999,
                reason: AccountCooldownReason::Throttled,
            },
            0,
        );
        // "fresh" has no observation -> score 1.0, the best.
        let cands = vec![
            cand("nearly-full", true, 0),
            cand("cooling", true, 0),
            cand("fresh", true, 0),
        ];
        assert_eq!(
            select_account(&cands, &book, 5, 1_000),
            Some("fresh".to_string())
        );
    }
}
