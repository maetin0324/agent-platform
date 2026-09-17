//! codex アカウントの確認（ADR-0025 D4）とログイン中継（D5）。
//!
//! `claude_account.rs` と対になる、codex 版の薄いラッパ。`WorkerAdapter` / ディスパッチャとは独立に、
//! `codex` CLI を直接起動する（アカウント選択・`AccountBook` の更新・cooldown の判断はしない。それは
//! taskd/task-dispatch 側の責務。ADR-0025 D3/D4/D5）。
//!
//! `AccountCheck` / `AccountCheckResult` / `LoginOutcome` / `LoginResult` はアダプタに依存しない語彙なので
//! `claude_account` のものをそのまま再利用する。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use task_core::RateLimitObservation;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::claude_account::{
    AccountCheck, AccountCheckResult, LoginError, LoginOutcome, LoginResult, READER_JOIN_TIMEOUT, join_with_timeout,
    pump_reader, strip_escape_codes, truncate_detail,
};
use crate::codex::now_unix_secs;
use crate::protocol::ProviderFailure;
use crate::provider::classify_provider_failure;
use crate::subprocess::{LineOutcome, MAX_LINE_BYTES, read_line_limited, send_signal_to_group};

/// 実機（codex-cli 0.154.0）で観測された未ログイン時の文言（ADR-0025 D4）。再試行を待たずに打ち切るための目印。
const AUTH_401_MARKERS: [&str; 2] = ["401 Unauthorized", "Missing bearer or basic authentication"];

fn looks_like_401(text: &str) -> bool {
    AUTH_401_MARKERS.iter().any(|m| text.contains(m))
}

/// 一時 cwd を確実に消す（`claude_account::TempCwdGuard` と同じ理由）。
struct TempCwdGuard(PathBuf);

impl Drop for TempCwdGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// ADR-0025 D4: `codex exec --json --skip-git-repo-check "Reply with exactly: ok"` を使い捨てディレクトリで
/// `timeout` まで実行する（モデルは指定しない）。出力に 401 の行が出たら再試行を待たずに打ち切って `AuthFailed`
/// にする。`turn.completed` が来れば `Ok`、分類できない失敗は `SpawnFailed`。`token_count` があれば観測値として返す。
pub async fn check_account_codex(
    command: &str,
    account_dir: &Path,
    timeout: Duration,
    base_env: &[(String, String)],
) -> AccountCheck {
    match tokio::time::timeout(timeout, run_check(command, account_dir, base_env)).await {
        Ok(check) => check,
        Err(_elapsed) => AccountCheck {
            result: AccountCheckResult::SpawnFailed,
            detail: Some("timeout".to_string()),
            observation: None,
        },
    }
}

async fn run_check(command: &str, account_dir: &Path, base_env: &[(String, String)]) -> AccountCheck {
    let cwd = std::env::temp_dir().join(format!("taskd-codex-check-{}", task_core::TaskId::new()));
    if let Err(e) = tokio::fs::create_dir_all(&cwd).await {
        return AccountCheck {
            result: AccountCheckResult::SpawnFailed,
            detail: Some(truncate_detail(&format!("failed to create check workspace: {e}"))),
            observation: None,
        };
    }
    let cwd_guard = TempCwdGuard(cwd.clone());

    let mut command_builder = Command::new(command);
    command_builder
        .arg("exec")
        .arg("--json")
        .arg("--skip-git-repo-check")
        .arg("Reply with exactly: ok");
    command_builder
        .envs(base_env.iter().cloned())
        .env("CODEX_HOME", account_dir)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command_builder.process_group(0);

    let mut child = match command_builder.spawn() {
        Ok(child) => child,
        Err(e) => {
            drop(cwd_guard);
            return AccountCheck {
                result: AccountCheckResult::SpawnFailed,
                detail: Some(truncate_detail(&e.to_string())),
                observation: None,
            };
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_end(&mut buf).await;
        }
        buf
    });

    let mut observation: Option<RateLimitObservation> = None;
    let mut last_error_message: Option<String> = None;
    let mut completed = false;
    let mut auth_failed_detail: Option<String> = None;

    if let Some(stdout) = stdout {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_line_limited(&mut reader, MAX_LINE_BYTES).await {
                Ok(LineOutcome::Eof) => break,
                Ok(LineOutcome::TooLong) => continue,
                Ok(LineOutcome::Line(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if looks_like_401(trimmed) {
                        auth_failed_detail = Some(truncate_detail(trimmed));
                        send_signal_to_group(&child, Signal::SIGKILL);
                        break;
                    }
                    handle_check_line(trimmed, &mut observation, &mut last_error_message, &mut completed);
                }
                Err(_) => break,
            }
        }
    }

    let _ = child.wait().await;
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    drop(cwd_guard);

    if let Some(detail) = auth_failed_detail {
        return AccountCheck { result: AccountCheckResult::AuthFailed, detail: Some(detail), observation };
    }
    if completed {
        return AccountCheck { result: AccountCheckResult::Ok, detail: Some("ok".to_string()), observation };
    }
    let stderr_tail = String::from_utf8_lossy(&stderr_bytes).to_string();
    let text_for_classification = last_error_message.clone().unwrap_or_else(|| stderr_tail.clone());
    let (result, detail) = match classify_provider_failure(&text_for_classification) {
        Some(ProviderFailure::AuthFailed) => (AccountCheckResult::AuthFailed, truncate_detail(&text_for_classification)),
        Some(ProviderFailure::Throttled { .. }) | Some(ProviderFailure::Exhausted) => {
            (AccountCheckResult::Throttled, truncate_detail(&text_for_classification))
        }
        None => {
            let detail = if text_for_classification.trim().is_empty() {
                "worker exited without a turn.completed/turn.failed message".to_string()
            } else {
                truncate_detail(&text_for_classification)
            };
            (AccountCheckResult::SpawnFailed, detail)
        }
    };
    AccountCheck { result, detail: Some(detail), observation }
}

fn handle_check_line(
    line: &str,
    observation: &mut Option<RateLimitObservation>,
    last_error_message: &mut Option<String>,
    completed: &mut bool,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let Some(ty) = value.get("type").and_then(|t| t.as_str()) else {
        return;
    };
    if ty == "token_count"
        && let Some(obs) = RateLimitObservation::from_codex_token_count(&value, now_unix_secs())
    {
        *observation = Some(obs);
    }
    match ty {
        "turn.completed" => *completed = true,
        "error" => {
            if let Some(m) = value.get("message").and_then(|m| m.as_str()) {
                *last_error_message = Some(m.to_string());
            }
        }
        "turn.failed" => {
            let message = value.get("error").map(describe_error).unwrap_or_else(|| "turn.failed".to_string());
            *last_error_message = Some(message);
        }
        _ => {}
    }
}

/// `codex::describe_error` と同じ規則（`turn.failed.error` は文字列でもオブジェクトでも読める）。
fn describe_error(value: &serde_json::Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(msg) = value.get("message").and_then(|m| m.as_str()) {
        return msg.to_string();
    }
    value.to_string()
}

/// ADR-0025 D5: `codex login --device-auth` で得た認可 URL と一回限りのコード。標準入力は使わない
/// （人が別デバイスでコードを入力し終わるのを `wait` で待つ）。
pub struct CodexLoginSession {
    pub url: String,
    pub user_code: String,
    pub started_at: Instant,
    account_dir: PathBuf,
    child: Option<Child>,
}

impl std::fmt::Debug for CodexLoginSession {
    /// N3 と同じ理由: 認可 URL・コードはログに出さない。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexLoginSession")
            .field("url", &"<redacted>")
            .field("user_code", &"<redacted>")
            .field("started_at", &self.started_at)
            .finish_non_exhaustive()
    }
}

/// `codex login --device-auth` を子プロセスとして起動し、URL と一回限りのコードを抽出して返す
/// （ADR-0025 D5）。子は標準入力を使わないので `stdin(Stdio::null())`。
pub async fn start_login_codex(
    command: &str,
    account_dir: &Path,
    base_env: &[(String, String)],
    url_timeout: Duration,
) -> Result<CodexLoginSession, LoginError> {
    let mut command_builder = Command::new(command);
    command_builder.arg("login").arg("--device-auth");
    command_builder
        .envs(base_env.iter().cloned())
        .env("CODEX_HOME", account_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command_builder.process_group(0);

    let mut child = command_builder.spawn().map_err(LoginError::Spawn)?;
    let stdout = child.stdout.take().ok_or(LoginError::NotPiped)?;
    let stderr = child.stderr.take().ok_or(LoginError::NotPiped)?;

    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let out_task = tokio::spawn(pump_reader(stdout, buf.clone()));
    let err_task = tokio::spawn(pump_reader(stderr, buf.clone()));

    let start = Instant::now();
    let found = loop {
        {
            let snapshot = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let url = extract_device_url(&snapshot);
            let user_code = extract_device_code(&snapshot);
            if let (Some(url), Some(user_code)) = (url, user_code) {
                break Some((url, user_code));
            }
        }
        if let Ok(Some(_status)) = child.try_wait() {
            break None;
        }
        if start.elapsed() >= url_timeout {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    match found {
        Some((url, user_code)) => Ok(CodexLoginSession {
            url,
            user_code,
            started_at: start,
            account_dir: account_dir.to_path_buf(),
            child: Some(child),
        }),
        None => {
            let exited = child.try_wait().ok().flatten().is_some();
            send_signal_to_group(&child, Signal::SIGKILL);
            let _ = child.wait().await;
            join_with_timeout(out_task, READER_JOIN_TIMEOUT).await;
            join_with_timeout(err_task, READER_JOIN_TIMEOUT).await;
            Err(if exited { LoginError::ProcessExited } else { LoginError::Timeout })
        }
    }
}

impl CodexLoginSession {
    /// 子プロセスの終了を `timeout` まで待つ（ADR-0025 D5: 15 分の上限は呼び出し側が渡す）。
    /// 成功 = exit 0 かつ `<account_dir>/auth.json` ができていること。`detail` は常に `None`
    /// （出力を読み返すと URL・コードを含みうるため、ここでは一切参照しない。D5: ログに出さない）。
    pub async fn wait(mut self, timeout: Duration) -> LoginResult {
        let status = if let Some(mut child) = self.child.take() {
            match tokio::time::timeout(timeout, child.wait()).await {
                Ok(status) => status.ok(),
                Err(_elapsed) => {
                    send_signal_to_group(&child, Signal::SIGKILL);
                    let _ = child.wait().await;
                    None
                }
            }
        } else {
            None
        };
        let auth_exists = self.account_dir.join("auth.json").is_file();
        let ok = status.map(|s| s.success()).unwrap_or(false) && auth_exists;
        LoginResult { result: if ok { LoginOutcome::Ok } else { LoginOutcome::Failed }, detail: None }
    }

    /// 非同期にブロックせず、子が終了していれば結果を返す（tick ごとのポーリング用。ADR-0025 D5）。
    /// まだ実行中なら `None`（セッションはそのまま。呼び出し側が保持し続けられる）。
    pub fn try_finished(&mut self) -> Option<LoginResult> {
        let child = self.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                let auth_exists = self.account_dir.join("auth.json").is_file();
                let ok = status.success() && auth_exists;
                self.child = None;
                Some(LoginResult { result: if ok { LoginOutcome::Ok } else { LoginOutcome::Failed }, detail: None })
            }
            _ => None,
        }
    }

    /// 進行中のログインを止める（ADR-0025 D5: `DELETE /accounts/{id}/login?adapter=codex` / 15 分での打ち切り）。
    pub fn cancel(mut self) {
        if let Some(child) = self.child.take() {
            send_signal_to_group(&child, Signal::SIGKILL);
        }
    }
}

impl Drop for CodexLoginSession {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            send_signal_to_group(&child, Signal::SIGKILL);
        }
    }
}

/// エスケープを除いた上で、最初に見つかる `/codex/device` を含む `https://` URL を返す。
fn extract_device_url(bytes: &[u8]) -> Option<String> {
    let stripped = strip_escape_codes(bytes);
    let mut search_from = 0usize;
    while let Some(rel) = stripped[search_from..].find("https://") {
        let idx = search_from + rel;
        let rest = &stripped[idx..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let candidate = &rest[..end];
        if candidate.contains("/codex/device") {
            return Some(candidate.to_string());
        }
        search_from = idx + "https://".len();
    }
    None
}

/// エスケープを除いた出力から、`ABCD-EFGHI` の形（英数字-英数字、両側とも 3〜8 文字）の一回限りのコードを探す。
fn extract_device_code(bytes: &[u8]) -> Option<String> {
    let stripped = strip_escape_codes(bytes);
    for token in stripped.split(|c: char| c.is_whitespace()) {
        let trimmed = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        let Some((a, b)) = trimmed.split_once('-') else {
            continue;
        };
        if a.is_empty()
            || b.is_empty()
            || b.contains('-')
            || !a.chars().all(|c| c.is_ascii_alphanumeric())
            || !b.chars().all(|c| c.is_ascii_alphanumeric())
            || !(3..=8).contains(&a.len())
            || !(3..=8).contains(&b.len())
        {
            continue;
        }
        return Some(format!("{a}-{b}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("codex_stub.sh");
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        path
    }

    fn account_dir(root: &Path, id: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).expect("account dir");
        dir
    }

    // ---- check_account_codex ----

    #[tokio::test]
    async fn check_account_codex_ok_path_records_observation() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            r#"echo '{"type":"token_count","rate_limits":{"primary":{"used_percent":14.0,"window_minutes":300,"resets_in_seconds":3600},"secondary":{"used_percent":24.0,"window_minutes":10080,"resets_in_seconds":432000}}}'
echo '{"type":"turn.completed"}'
"#,
        );
        let acct = account_dir(dir.path(), "a");
        let check = check_account_codex(command.to_str().unwrap(), &acct, Duration::from_secs(5), &[]).await;
        assert_eq!(check.result, AccountCheckResult::Ok);
        let obs = check.observation.expect("observation");
        assert_eq!(obs.five_hour.map(|w| w.utilization), Some(0.14));
        assert_eq!(obs.seven_day.map(|w| w.utilization), Some(0.24));
    }

    /// ADR-0025 D4: 401 の行が出たら再試行を待たずに打ち切って `AuthFailed` にする（未ログインだと codex は
    /// 10 回ほど再試行して約 40 秒かかるため）。
    #[tokio::test]
    async fn check_account_codex_401_aborts_immediately_without_waiting_for_retries() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            r#"echo 'ERROR: unexpected status 401 Unauthorized: Missing bearer or basic authentication in header'
sleep 30
"#,
        );
        let acct = account_dir(dir.path(), "a");
        let start = Instant::now();
        let check = check_account_codex(command.to_str().unwrap(), &acct, Duration::from_secs(20), &[]).await;
        assert!(start.elapsed() < Duration::from_secs(10), "should not wait for retries: {:?}", start.elapsed());
        assert_eq!(check.result, AccountCheckResult::AuthFailed);
    }

    #[tokio::test]
    async fn check_account_codex_turn_failed_classified() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            r#"echo '{"type":"turn.failed","error":{"message":"You'"'"'ve hit your usage limit"}}'"#,
        );
        let acct = account_dir(dir.path(), "a");
        let check = check_account_codex(command.to_str().unwrap(), &acct, Duration::from_secs(5), &[]).await;
        assert_eq!(check.result, AccountCheckResult::Throttled);
    }

    #[tokio::test]
    async fn check_account_codex_spawn_failure_for_nonexistent_command() {
        let dir = tempfile::tempdir().unwrap();
        let acct = account_dir(dir.path(), "a");
        let missing = dir.path().join("does-not-exist");
        let check = check_account_codex(missing.to_str().unwrap(), &acct, Duration::from_secs(5), &[]).await;
        assert_eq!(check.result, AccountCheckResult::SpawnFailed);
    }

    #[tokio::test]
    async fn check_account_codex_timeout_is_spawn_failed() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(dir.path(), "sleep 30");
        let acct = account_dir(dir.path(), "a");
        let check = check_account_codex(command.to_str().unwrap(), &acct, Duration::from_millis(200), &[]).await;
        assert_eq!(check.result, AccountCheckResult::SpawnFailed);
        assert_eq!(check.detail.as_deref(), Some("timeout"));
    }

    // ---- device login ----

    /// ADR-0025 D5 の実測どおり、ANSI で色付けされた URL とコードを出す（標準入力は使わず、コード入力完了後に
    /// exit 0 する）。
    fn login_stub(dir: &Path, ok: bool) -> PathBuf {
        let exit_code = if ok { 0 } else { 1 };
        let write_auth_json = if ok { r#"printf '%s' '{}' > "$CODEX_HOME/auth.json""# } else { "true" };
        stub(
            dir,
            &format!(
                "printf '\\033[1mVisit\\033[0m https://auth.openai.com/codex/device and enter code:\\n'\n\
                 printf '\\033[32mABCD-EFGHI\\033[0m\\n'\n\
                 sleep 0.2\n\
                 {write_auth_json}\n\
                 exit {exit_code}\n"
            ),
        )
    }

    #[tokio::test]
    async fn start_login_codex_extracts_url_and_code_without_ansi() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path(), true);
        let acct = account_dir(dir.path(), "a");
        let session = start_login_codex(command.to_str().unwrap(), &acct, &[], Duration::from_secs(5))
            .await
            .expect("session");
        assert_eq!(session.url, "https://auth.openai.com/codex/device");
        assert_eq!(session.user_code, "ABCD-EFGHI");
        let result = session.wait(Duration::from_secs(5)).await;
        assert_eq!(result.result, LoginOutcome::Ok);
        assert!(acct.join("auth.json").is_file());
    }

    #[tokio::test]
    async fn wait_fails_when_process_exits_nonzero_even_without_auth_json() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path(), false);
        let acct = account_dir(dir.path(), "a");
        let session = start_login_codex(command.to_str().unwrap(), &acct, &[], Duration::from_secs(5))
            .await
            .expect("session");
        let result = session.wait(Duration::from_secs(5)).await;
        assert_eq!(result.result, LoginOutcome::Failed);
        assert!(!acct.join("auth.json").is_file());
    }

    /// D5: exit 0 でも `auth.json` が無ければ失敗として扱う（成功の判定は両方の条件を要る）。
    #[tokio::test]
    async fn wait_fails_when_exit_zero_but_auth_json_missing() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            r#"printf 'https://auth.openai.com/codex/device\n'
printf 'ABCD-EFGHI\n'
exit 0
"#,
        );
        let acct = account_dir(dir.path(), "a");
        let session = start_login_codex(command.to_str().unwrap(), &acct, &[], Duration::from_secs(5))
            .await
            .expect("session");
        let result = session.wait(Duration::from_secs(5)).await;
        assert_eq!(result.result, LoginOutcome::Failed);
    }

    #[tokio::test]
    async fn cancel_kills_the_login_process() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            r#"printf 'https://auth.openai.com/codex/device\n'
printf 'ABCD-EFGHI\n'
sleep 30
"#,
        );
        let acct = account_dir(dir.path(), "a");
        let session = start_login_codex(command.to_str().unwrap(), &acct, &[], Duration::from_secs(5))
            .await
            .expect("session");
        let pid = session.child.as_ref().and_then(|c| c.id()).expect("pid");
        session.cancel();
        for _ in 0..100 {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("process {pid} is still alive after cancel()");
    }

    /// ADR-0025 D5: `try_finished` はブロックせず、まだ実行中なら `None`、終了していれば結果を返す
    /// （tick ごとのポーリング用。taskd の実装がこちらを使う）。
    #[tokio::test]
    async fn try_finished_polls_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path(), true);
        let acct = account_dir(dir.path(), "a");
        let mut session = start_login_codex(command.to_str().unwrap(), &acct, &[], Duration::from_secs(5))
            .await
            .expect("session");
        // Not finished yet (the stub sleeps 0.2s before exiting).
        assert_eq!(session.try_finished(), None);
        for _ in 0..100 {
            if let Some(result) = session.try_finished() {
                assert_eq!(result.result, LoginOutcome::Ok);
                assert!(acct.join("auth.json").is_file());
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("session never finished");
    }

    /// ADR-0025 D5: 15 分の上限（テストでは短い値を注入する）を超えると `wait` は打ち切って `Failed` を返す。
    #[tokio::test]
    async fn wait_expires_and_kills_the_process_after_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            r#"printf 'https://auth.openai.com/codex/device\n'
printf 'ABCD-EFGHI\n'
sleep 30
"#,
        );
        let acct = account_dir(dir.path(), "a");
        let session = start_login_codex(command.to_str().unwrap(), &acct, &[], Duration::from_secs(5))
            .await
            .expect("session");
        let pid = session.child.as_ref().and_then(|c| c.id()).expect("pid");
        let result = session.wait(Duration::from_millis(200)).await;
        assert_eq!(result.result, LoginOutcome::Failed);
        for _ in 0..100 {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("process {pid} is still alive after wait() timed out");
    }

    // ---- extract_device_code ----

    #[test]
    fn extract_device_code_ignores_the_url_and_finds_the_code() {
        let text = b"Visit https://auth.openai.com/codex/device and enter\nABCD-EFGHI\n";
        assert_eq!(extract_device_code(text).as_deref(), Some("ABCD-EFGHI"));
    }

    #[test]
    fn extract_device_code_none_when_absent() {
        let text = b"Visit https://auth.openai.com/codex/device and enter the code shown on the page\n";
        assert_eq!(extract_device_code(text), None);
    }
}
