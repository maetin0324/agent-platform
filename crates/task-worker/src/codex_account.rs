//! codex アカウントの確認（ADR-0025 D4）とログイン中継（D5）。
//!
//! `claude_account.rs` と対になる、codex 版の薄いラッパ。`WorkerAdapter` / ディスパッチャとは独立に、
//! `codex` CLI を直接起動する（アカウント選択・`AccountBook` の更新・cooldown の判断はしない。それは
//! celeris/task-dispatch 側の責務。ADR-0025 D3/D4/D5）。
//!
//! `AccountCheck` / `AccountCheckResult` / `LoginOutcome` / `LoginResult` はアダプタに依存しない語彙なので
//! `claude_account` のものをそのまま再利用する。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use task_core::RateLimitObservation;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::claude_account::{
    AccountCheck, AccountCheckResult, LoginError, LoginOutcome, LoginResult, drain_readers,
    pump_reader, strip_escape_codes,
};
use crate::codex::now_unix_secs;
use crate::protocol::ProviderFailure;
use crate::provider::classify_provider_failure;
use crate::subprocess::{LineOutcome, MAX_LINE_BYTES, read_line_limited, send_signal_to_group};

/// ADR-0049: 推論を使わず app-server のアカウント API で認証と利用枠を確認する。
pub async fn check_account_codex(
    command: &str,
    account_dir: &Path,
    timeout: Duration,
    base_env: &[(String, String)],
) -> AccountCheck {
    let mut builder = Command::new(command);
    builder
        .arg("app-server")
        .envs(base_env.iter().cloned())
        .env("CODEX_HOME", account_dir)
        .current_dir(account_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    builder.process_group(0);
    let child = match builder.spawn() {
        Ok(child) => child,
        Err(_) => {
            return failed_check(
                AccountCheckResult::SpawnFailed,
                "could not start codex app-server",
            );
        }
    };
    let mut process = AccountServer(child);
    let outcome = tokio::time::timeout(timeout, read_account_limits(&mut process.0)).await;
    // 成功・RPC エラー・timeout すべてで終了を待つ。外側のキャンセル時も Drop がグループを止める。
    send_signal_to_group(&process.0, Signal::SIGKILL);
    let _ = process.0.wait().await;
    match outcome {
        Ok(Ok(check)) => check,
        Ok(Err(check)) => check,
        Err(_) => failed_check(AccountCheckResult::SpawnFailed, "timeout"),
    }
}

struct AccountServer(Child);

impl Drop for AccountServer {
    fn drop(&mut self) {
        send_signal_to_group(&self.0, Signal::SIGKILL);
    }
}

fn failed_check(result: AccountCheckResult, detail: &str) -> AccountCheck {
    AccountCheck {
        result,
        detail: Some(detail.into()),
        observation: None,
    }
}

async fn rpc(
    input: &mut tokio::process::ChildStdin,
    output: &mut BufReader<tokio::process::ChildStdout>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, AccountCheck> {
    let msg = serde_json::json!({"id": id, "method": method, "params": params});
    input
        .write_all(format!("{msg}\n").as_bytes())
        .await
        .map_err(|_| failed_check(AccountCheckResult::SpawnFailed, "app-server input closed"))?;
    loop {
        let line = match read_line_limited(output, MAX_LINE_BYTES).await {
            Ok(LineOutcome::Line(bytes)) => bytes,
            _ => {
                return Err(failed_check(
                    AccountCheckResult::SpawnFailed,
                    "app-server response missing or too large",
                ));
            }
        };
        let value: serde_json::Value = serde_json::from_slice(&line).map_err(|_| {
            failed_check(AccountCheckResult::SpawnFailed, "invalid app-server JSON")
        })?;
        if value.get("id").and_then(|v| v.as_u64()) != Some(id) {
            continue; // 通知や別の応答。認証のための server request は実行しない。
        }
        if let Some(error) = value.get("error") {
            let kind = match classify_provider_failure(&error.to_string()) {
                Some(ProviderFailure::AuthFailed) => AccountCheckResult::AuthFailed,
                Some(ProviderFailure::Throttled { .. } | ProviderFailure::Exhausted) => {
                    AccountCheckResult::Throttled
                }
                None => AccountCheckResult::SpawnFailed,
            };
            // サーバの本文にはアカウント情報が含まれうる。GUI/ログへそのまま転送しない。
            return Err(failed_check(kind, &format!("codex {method} failed")));
        }
        return value.get("result").cloned().ok_or_else(|| {
            failed_check(AccountCheckResult::SpawnFailed, "app-server result missing")
        });
    }
}

async fn read_account_limits(child: &mut Child) -> Result<AccountCheck, AccountCheck> {
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| failed_check(AccountCheckResult::SpawnFailed, "app-server stdin missing"))?;
    let mut output = BufReader::new(child.stdout.take().ok_or_else(|| {
        failed_check(AccountCheckResult::SpawnFailed, "app-server stdout missing")
    })?);
    rpc(
        &mut input,
        &mut output,
        1,
        "initialize",
        serde_json::json!({
            "clientInfo": {"name": "celeris", "version": env!("CARGO_PKG_VERSION")}
        }),
    )
    .await?;
    input
        .write_all(b"{\"method\":\"initialized\",\"params\":{}}\n")
        .await
        .map_err(|_| failed_check(AccountCheckResult::SpawnFailed, "app-server input closed"))?;
    let account = rpc(
        &mut input,
        &mut output,
        2,
        "account/read",
        serde_json::json!({"refreshToken": false}),
    )
    .await?;
    let Some(kind) = account.pointer("/account/type").and_then(|v| v.as_str()) else {
        return Ok(failed_check(
            AccountCheckResult::AuthFailed,
            "not logged in",
        ));
    };
    if kind != "chatgpt" {
        return Ok(failed_check(
            AccountCheckResult::Ok,
            "authenticated; ChatGPT usage limits are unavailable for this authentication type",
        ));
    }
    let result = rpc(
        &mut input,
        &mut output,
        3,
        "account/rateLimits/read",
        serde_json::json!({}),
    )
    .await?;
    let observation = RateLimitObservation::from_codex_account_limits(&result, now_unix_secs());
    Ok(AccountCheck {
        result: AccountCheckResult::Ok,
        detail: Some(
            if observation.is_some() {
                "ok"
            } else {
                "authenticated; usage limits unavailable"
            }
            .into(),
        ),
        observation,
    })
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
    let mut out_task = Some(tokio::spawn(pump_reader(stdout, buf.clone())));
    let mut err_task = Some(tokio::spawn(pump_reader(stderr, buf.clone())));

    let find = |bytes: &[u8]| {
        let url = extract_device_url(bytes);
        let user_code = extract_device_code(bytes);
        match (url, user_code) {
            (Some(url), Some(user_code)) => Some((url, user_code)),
            _ => None,
        }
    };

    let start = Instant::now();
    let found = loop {
        {
            let snapshot = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(found) = find(&snapshot) {
                break Some(found);
            }
        }
        if let Ok(Some(_status)) = child.try_wait() {
            // 終了後もパイプに出力が残りうる。読み切ってから最後にもう一度だけ探す（`codex login --device-auth`
            // は URL とコードを出した後すぐ終わることがあり、負荷下では読み取りタスクが追いつかない）。
            drain_readers(&mut out_task, &mut err_task).await;
            let snapshot = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
            break find(&snapshot);
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
            drain_readers(&mut out_task, &mut err_task).await;
            Err(if exited {
                LoginError::ProcessExited
            } else {
                LoginError::Timeout
            })
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
        LoginResult {
            result: if ok {
                LoginOutcome::Ok
            } else {
                LoginOutcome::Failed
            },
            detail: None,
        }
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
                Some(LoginResult {
                    result: if ok {
                        LoginOutcome::Ok
                    } else {
                        LoginOutcome::Failed
                    },
                    detail: None,
                })
            }
            _ => None,
        }
    }

    /// 進行中のログインを止める（ADR-0025 D5: `DELETE /accounts/{id}/login?adapter=codex` / 15 分での打ち切り）。
    pub fn cancel(self) {
        drop(self);
    }
}

impl Drop for CodexLoginSession {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            send_signal_to_group(&child, Signal::SIGKILL);
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = child.wait().await;
                });
            }
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

/// エスケープを除いた出力から、一回限りのコード（実測の形: `ABCD-1EFGH`）を探す。
///
/// **行全体がコードである行**だけを見る。codex は起動時に `OpenAI's command-line coding agent` という
/// バナーを出すので、文中の「英数字-英数字」（`command-line`）を拾ってしまう（実機で発覚）。
/// 本物のコードは大文字と数字だけで、単独の行に出る。
fn extract_device_code(bytes: &[u8]) -> Option<String> {
    let stripped = strip_escape_codes(bytes);
    stripped
        .lines()
        .map(str::trim)
        .find(|line| is_device_code(line))
        .map(str::to_string)
}

/// `ABCD-1EFGH` の形（大文字か数字の塊を `-` でつないだもの。各塊 3〜8 文字、塊は 2〜3 個）。
fn is_device_code(line: &str) -> bool {
    let parts: Vec<&str> = line.split('-').collect();
    if !(2..=3).contains(&parts.len()) {
        return false;
    }
    parts.iter().all(|part| {
        (3..=8).contains(&part.len())
            && part
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    })
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

    // app-server の要求を確認して返す。推論コマンドを起こしたら失敗する。
    fn check_stub(dir: &Path, response: &str) -> PathBuf {
        stub(
            dir,
            &format!(
                r#"
[ "$1" = app-server ] || exit 9
read -r init
case "$init" in *initialize*) ;; *) exit 10 ;; esac
echo '{{"id":1,"result":{{}}}}'
read -r initialized
case "$initialized" in *initialized*) ;; *) exit 11 ;; esac
read -r account
case "$account" in *account/read*) ;; *) exit 12 ;; esac
echo '{{"id":2,"result":{{"account":{{"type":"chatgpt"}}}}}}'
read -r limits
case "$limits" in *account/rateLimits/read*) ;; *) exit 13 ;; esac
{response}
"#
            ),
        )
    }

    #[tokio::test]
    async fn check_account_codex_reads_limits_without_inference() {
        let dir = tempfile::tempdir().unwrap();
        let command = check_stub(
            dir.path(),
            r#"echo '{"method":"account/updated","params":{}}'
echo '{"id":3,"result":{"rateLimits":{"primary":{"usedPercent":14,"windowDurationMins":300,"resetsAt":2000000000},"secondary":{"usedPercent":24,"windowDurationMins":10080,"resetsAt":2000600000}}}}'"#,
        );
        let acct = account_dir(dir.path(), "a");
        let check = check_account_codex(
            command.to_str().unwrap(),
            &acct,
            Duration::from_secs(5),
            &[],
        )
        .await;
        assert_eq!(check.result, AccountCheckResult::Ok);
        let obs = check.observation.unwrap();
        assert_eq!(obs.five_hour.unwrap().utilization, 0.14);
        assert_eq!(obs.five_hour.unwrap().resets_at, 2000000000);
        assert_eq!(obs.seven_day.unwrap().utilization, 0.24);
    }

    #[tokio::test]
    async fn check_account_codex_failures_are_bounded_and_sanitized() {
        let dir = tempfile::tempdir().unwrap();
        let acct = account_dir(dir.path(), "a");
        for (response, expected) in [
            (
                r#"echo '{"id":3,"error":{"message":"401 Unauthorized secret-token"}}'"#,
                AccountCheckResult::AuthFailed,
            ),
            (
                r#"echo '{"id":3,"error":{"message":"usage limit"}}'"#,
                AccountCheckResult::Throttled,
            ),
            ("echo invalid-json", AccountCheckResult::SpawnFailed),
            ("exit 0", AccountCheckResult::SpawnFailed),
            ("sleep 30", AccountCheckResult::SpawnFailed),
        ] {
            let command = check_stub(dir.path(), response);
            let check = check_account_codex(
                command.to_str().unwrap(),
                &acct,
                Duration::from_millis(300),
                &[],
            )
            .await;
            assert_eq!(check.result, expected, "{response}");
            assert!(!check.detail.unwrap().contains("secret-token"));
        }
    }

    #[tokio::test]
    async fn check_account_codex_handles_logged_out_and_api_key_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let acct = account_dir(dir.path(), "a");
        for (account, expected) in [
            ("null", AccountCheckResult::AuthFailed),
            (r#"{"type":"apiKey"}"#, AccountCheckResult::Ok),
        ] {
            let command = stub(
                dir.path(),
                &format!(
                    r#"read -r init
echo '{{"id":1,"result":{{}}}}'
read -r initialized
read -r account
echo '{{"id":2,"result":{{"account":{account}}}}}'
# 次の要求は無い。親が終了させる。
sleep 30"#
                ),
            );
            let check = check_account_codex(
                command.to_str().unwrap(),
                &acct,
                Duration::from_secs(2),
                &[],
            )
            .await;
            assert_eq!(check.result, expected);
            assert!(check.observation.is_none());
        }
    }

    #[tokio::test]
    async fn check_account_codex_spawn_failure_for_nonexistent_command() {
        let dir = tempfile::tempdir().unwrap();
        let acct = account_dir(dir.path(), "a");
        let check =
            check_account_codex("/nonexistent/codex", &acct, Duration::from_secs(1), &[]).await;
        assert_eq!(check.result, AccountCheckResult::SpawnFailed);
    }

    // ---- device login ----

    /// ADR-0025 D5 の実測どおり、ANSI で色付けされた URL とコードを出す（標準入力は使わず、コード入力完了後に
    /// exit 0 する）。
    fn login_stub(dir: &Path, ok: bool) -> PathBuf {
        let exit_code = if ok { 0 } else { 1 };
        let write_auth_json = if ok {
            r#"printf '%s' '{}' > "$CODEX_HOME/auth.json""#
        } else {
            "true"
        };
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
        let session = start_login_codex(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
        .await
        .expect("session");
        assert_eq!(session.url, "https://auth.openai.com/codex/device");
        assert_eq!(session.user_code, "ABCD-EFGHI");
        let result = session.wait(Duration::from_secs(5)).await;
        assert_eq!(result.result, LoginOutcome::Ok);
        assert!(acct.join("auth.json").is_file());
    }

    /// 回帰: 子が URL とコードを出して**すぐに終了**しても、終了検知の前にパイプに残った出力を読み切ってから
    /// 探すので「URL を出す前に終了した」と誤判定しない（accounts_admin の codex ログインテストが CI 負荷下で
    /// `ProcessExited` になった競合）。読み取りタスクが追いつく前に終了させるため、遅延なしの stub を繰り返す。
    #[tokio::test]
    async fn start_login_codex_finds_url_and_code_even_when_the_process_exits_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            r#"printf '%s\n' 'https://auth.openai.com/codex/device' 'ABCD-EFGHI'
printf '%s' '{}' > "$CODEX_HOME/auth.json"
exit 0
"#,
        );
        for i in 0..20 {
            let acct = account_dir(dir.path(), &format!("a{i}"));
            let session = start_login_codex(
                command.to_str().unwrap(),
                &acct,
                &[],
                Duration::from_secs(5),
            )
            .await
            .unwrap_or_else(|e| panic!("iteration {i}: {e}"));
            assert_eq!(session.url, "https://auth.openai.com/codex/device");
            assert_eq!(session.user_code, "ABCD-EFGHI");
            let result = session.wait(Duration::from_secs(5)).await;
            assert_eq!(result.result, LoginOutcome::Ok);
            assert!(acct.join("auth.json").is_file());
        }
    }

    /// 上の回帰の裏: 出力を読み切っても URL が無ければ、従来どおり `ProcessExited`（`Timeout` ではない）。
    #[tokio::test]
    async fn start_login_codex_still_reports_process_exited_when_no_url_was_printed() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            "printf 'codex 0.1.0\nno device flow here\n'
exit 0
",
        );
        let acct = account_dir(dir.path(), "a");
        let err = start_login_codex(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
        .await
        .expect_err("must fail");
        assert!(matches!(err, LoginError::ProcessExited), "{err}");
    }

    #[tokio::test]
    async fn wait_fails_when_process_exits_nonzero_even_without_auth_json() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path(), false);
        let acct = account_dir(dir.path(), "a");
        let session = start_login_codex(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
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
        let session = start_login_codex(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
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
        let session = start_login_codex(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
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
    /// （tick ごとのポーリング用。celeris の実装がこちらを使う）。
    #[tokio::test]
    async fn try_finished_polls_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path(), true);
        let acct = account_dir(dir.path(), "a");
        let mut session = start_login_codex(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
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
        let session = start_login_codex(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
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

    /// 実機（codex-cli 0.154.0）の出力そのまま。バナーの `command-line` を拾ってはいけない（人からの報告で発覚）。
    #[test]
    fn extract_device_code_skips_the_banner_and_takes_the_standalone_code_line() {
        let text = concat!(
            "\n  Welcome to Codex [v0.154.0]\n",
            "  OpenAI's command-line coding agent\n\n",
            "Follow these steps to sign in with ChatGPT using device code authorization:\n\n",
            "1. Open this link in your browser and sign in to your account\n",
            "   https://auth.openai.com/codex/device\n\n",
            "2. Enter this one-time code (expires in 15 minutes)\n",
            "   QWER-1TYUI\n\n",
            "Continue only if you started this login in Codex.\n",
        )
        .as_bytes();
        assert_eq!(extract_device_code(text).as_deref(), Some("QWER-1TYUI"));
    }

    /// 文中の小文字の「英数字-英数字」（`command-line` / `one-time` / パス）は候補にしない。
    #[test]
    fn extract_device_code_ignores_hyphenated_words_in_prose() {
        for text in [
            &b"OpenAI's command-line coding agent\n"[..],
            &b"2. Enter this one-time code (expires in 15 minutes)\n"[..],
            &b"codex_home: /tmp/claude-1001/-home-rmaeda/workspace-agent\n"[..],
        ] {
            assert_eq!(
                extract_device_code(text),
                None,
                "should not match: {}",
                String::from_utf8_lossy(text)
            );
        }
    }

    #[test]
    fn extract_device_code_none_when_absent() {
        let text =
            b"Visit https://auth.openai.com/codex/device and enter the code shown on the page\n";
        assert_eq!(extract_device_code(text), None);
    }
}
