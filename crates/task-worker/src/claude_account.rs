//! Claude アカウントの確認（ADR-0024 D6）とログイン中継（D7）。
//!
//! `WorkerAdapter` / ディスパッチャとは独立した、`claude` CLI を直接起動する薄いラッパ。ここは
//! celeris（呼び出し側。デーモンの HTTP ハンドラや `celerisctl`）が使う低レベル操作だけを提供し、アカウント選択・
//! `AccountBook` の更新・cooldown の判断は一切行わない（それは celeris/task-dispatch 側の責務。ADR-0024 D3/D4）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::Signal;
use task_core::RateLimitObservation;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::task::JoinHandle;

use crate::claude_code::now_unix_secs;
use crate::protocol::ProviderFailure;
use crate::provider::classify_provider_failure;
use crate::subprocess::{LineOutcome, MAX_LINE_BYTES, read_line_limited, send_signal_to_group};

/// N5: ログイン中継の読み取りタスク（stdout/stderr の `pump_reader`）の join を待つ上限。子プロセスは
/// 既に `wait()` 済みなのでパイプはすぐ閉じるはずだが、何かで詰まっても中継全体を止めないための保険。
pub(crate) const READER_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// `check_account` / `AccountLoginResult` の結果種別（ADR-0024 D5/D6。serde は `snake_case`）。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AccountCheckResult {
    Ok,
    AuthFailed,
    Throttled,
    SpawnFailed,
}

/// `check_account` の結果（ADR-0024 D6）。
#[derive(Debug, Clone, PartialEq)]
pub struct AccountCheck {
    pub result: AccountCheckResult,
    /// 人が読むための一行の手がかり。1 行・200 文字まで。
    pub detail: Option<String>,
    /// 観測できた `rate_limit_event`（あれば）。呼び出し側が `AccountBook` に `source = "check"` で記録する。
    pub observation: Option<RateLimitObservation>,
}

/// `claude` の `result` メッセージのうち、この確認が見る範囲。
#[derive(Debug, Clone, Default)]
struct ResultMeta {
    subtype: String,
    is_error: bool,
    result: Option<String>,
}

/// ADR-0024 D6: そのアカウントの env で `claude -p "Reply with exactly: ok" ...` を 1 回だけ実行し、
/// `rate_limit_event` を観測しつつ結果を分類する。`timeout` を超えたら子プロセスを kill して
/// `SpawnFailed`（`detail = "timeout"`）を返す（`kill_on_drop` により、`timeout` の future が破棄されるときに
/// 子プロセスも kill される）。
pub async fn check_account(
    command: &str,
    account_dir: &Path,
    model: &str,
    timeout: Duration,
    base_env: &[(String, String)],
) -> AccountCheck {
    match tokio::time::timeout(timeout, run_check(command, account_dir, model, base_env)).await {
        Ok(check) => check,
        Err(_elapsed) => AccountCheck {
            result: AccountCheckResult::SpawnFailed,
            detail: Some("timeout".to_string()),
            observation: None,
        },
    }
}

/// N11: 一時 cwd を確実に消す（`check_account` の `tokio::time::timeout` がこの関数の future を
/// 破棄したとき＝タイムアウト時も、Drop でローカル変数が破棄されるのでディレクトリが残らない）。
struct TempCwdGuard(PathBuf);

impl Drop for TempCwdGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn run_check(
    command: &str,
    account_dir: &Path,
    model: &str,
    base_env: &[(String, String)],
) -> AccountCheck {
    let cwd = std::env::temp_dir().join(format!(
        "celeris-account-check-{}",
        task_core::TaskId::new()
    ));
    if let Err(e) = tokio::fs::create_dir_all(&cwd).await {
        return AccountCheck {
            result: AccountCheckResult::SpawnFailed,
            detail: Some(truncate_detail(&format!(
                "failed to create check workspace: {e}"
            ))),
            observation: None,
        };
    }
    let cwd_guard = TempCwdGuard(cwd.clone());

    let mut command_builder = Command::new(command);
    command_builder
        .arg("-p")
        .arg("Reply with exactly: ok")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--max-turns")
        .arg("1")
        .arg("--no-session-persistence")
        .arg("--model")
        .arg(model);
    command_builder
        .envs(base_env.iter().cloned())
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", account_dir)
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

    let stderr_task: JoinHandle<Vec<u8>> = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_end(&mut buf).await;
        }
        buf
    });

    let mut last_result: Option<ResultMeta> = None;
    let mut last_observation: Option<RateLimitObservation> = None;

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
                    handle_check_line(trimmed, &mut last_result, &mut last_observation);
                }
                Err(_) => break,
            }
        }
    }

    let _ = child.wait().await;
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    drop(cwd_guard);

    let (result, detail) = classify(
        last_result.as_ref(),
        &String::from_utf8_lossy(&stderr_bytes),
    );
    AccountCheck {
        result,
        detail,
        observation: last_observation,
    }
}

fn handle_check_line(
    line: &str,
    last_result: &mut Option<ResultMeta>,
    last_observation: &mut Option<RateLimitObservation>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let Some(ty) = value.get("type").and_then(|t| t.as_str()) else {
        return;
    };
    if ty == "rate_limit_event"
        && let Some(obs) = RateLimitObservation::from_stream_json(&value, now_unix_secs())
    {
        *last_observation = Some(obs);
    }
    if ty == "result" {
        let subtype = value
            .get("subtype")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string();
        let is_error = value
            .get("is_error")
            .and_then(|b| b.as_bool())
            .unwrap_or(subtype != "success");
        let result = value
            .get("result")
            .and_then(|r| r.as_str())
            .map(str::to_string);
        *last_result = Some(ResultMeta {
            subtype,
            is_error,
            result,
        });
    }
}

/// D6 の分類規則: `result` が成功なら `Ok`。それ以外（エラー・`result` 行が来なかった）は
/// `classify_provider_failure` に通す。分類できず `result` 行も無ければ `SpawnFailed`、分類できないが
/// `result` 行はあれば（アカウント自体の問題ではないので）`Ok` とする。
fn classify(
    last_result: Option<&ResultMeta>,
    stderr_tail: &str,
) -> (AccountCheckResult, Option<String>) {
    match last_result {
        Some(meta) if !meta.is_error && meta.subtype == "success" => {
            let text = meta.result.clone().unwrap_or_default();
            (AccountCheckResult::Ok, Some(truncate_detail(&text)))
        }
        Some(meta) => {
            let text = meta.result.clone().unwrap_or_else(|| meta.subtype.clone());
            match classify_provider_failure(&text) {
                Some(ProviderFailure::AuthFailed) => {
                    (AccountCheckResult::AuthFailed, Some(truncate_detail(&text)))
                }
                Some(ProviderFailure::Throttled { .. }) | Some(ProviderFailure::Exhausted) => {
                    (AccountCheckResult::Throttled, Some(truncate_detail(&text)))
                }
                None => (AccountCheckResult::Ok, Some(truncate_detail(&text))),
            }
        }
        None => match classify_provider_failure(stderr_tail) {
            Some(ProviderFailure::AuthFailed) => (
                AccountCheckResult::AuthFailed,
                Some(truncate_detail(stderr_tail)),
            ),
            Some(ProviderFailure::Throttled { .. }) | Some(ProviderFailure::Exhausted) => (
                AccountCheckResult::Throttled,
                Some(truncate_detail(stderr_tail)),
            ),
            None => {
                let detail = if stderr_tail.trim().is_empty() {
                    "worker exited without a result message".to_string()
                } else {
                    truncate_detail(stderr_tail)
                };
                (AccountCheckResult::SpawnFailed, Some(detail))
            }
        },
    }
}

/// 人が読むための一行の手がかり（1 行・200 文字まで）。改行・連続空白は 1 個の空白にたたむ。
pub(crate) fn truncate_detail(text: &str) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= 200 {
        return one_line;
    }
    let truncated: String = one_line.chars().take(200).collect();
    format!("{truncated}…")
}

/// ADR-0024 D7 のログイン中継の結果。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LoginOutcome {
    Ok,
    Failed,
}

/// `login/code` の結果（ADR-0024 D5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginResult {
    pub result: LoginOutcome,
    /// 人が読むための一行の手がかり。認可コード・URL は含まない（D5: ログに出さない）。
    pub detail: Option<String>,
}

/// `start_login` の失敗。
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("failed to spawn login process: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("login process exited before showing an authorization url")]
    ProcessExited,
    #[error("timed out waiting for an authorization url")]
    Timeout,
    #[error("login process did not expose a stdin/stdout/stderr pipe")]
    NotPiped,
}

/// ADR-0024 D7: `claude auth login` を子プロセスとして起動し、URL の表示とコードの受け渡しだけを中継する
/// （OAuth は実装しない）。進行中のセッションは celeris 側で `HashMap<AccountId, Mutex<LoginSession>>` のように
/// 1 アカウントごとに 1 つだけ保持することを想定する。
pub struct LoginSession {
    pub url: String,
    pub started_at: Instant,
    account_dir: PathBuf,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    out_task: Option<JoinHandle<()>>,
    err_task: Option<JoinHandle<()>>,
    buf: Arc<Mutex<Vec<u8>>>,
}

impl std::fmt::Debug for LoginSession {
    /// N3: 認可 URL はログに出さない（D5）。`Debug` の既定導出だと `url` がそのまま出てしまうので手で書く。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginSession")
            .field("url", &"<redacted>")
            .field("started_at", &self.started_at)
            .finish_non_exhaustive()
    }
}

/// パイプが詰まらないよう、読めたバイトをそのまま共有バッファへ足し続ける（行区切りに依存しない。
/// `Paste code here if prompted > ` のように改行の無いプロンプトを認識する必要があるため）。
pub(crate) async fn pump_reader<R>(mut reader: R, buf: Arc<Mutex<Vec<u8>>>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend_from_slice(&chunk[..n]);
            }
        }
    }
}

/// N5: `handle` の完了を `timeout` まで待ち、それを超えたら中断する（タスク自体は abort する。呼び出し側の
/// 処理は続ける）。
pub(crate) async fn join_with_timeout(handle: JoinHandle<()>, timeout: Duration) {
    let abort_handle = handle.abort_handle();
    if tokio::time::timeout(timeout, handle).await.is_err() {
        abort_handle.abort();
    }
}

/// 子プロセスが終了した後に、パイプに残っている出力を読み切る（両方の読み取りタスクを `READER_JOIN_TIMEOUT`
/// まで待つ）。`start_login` / `start_login_codex` は「終了を検知した瞬間の共有バッファ」ではなく、読み切った後の
/// バッファで URL を探す。そうしないと、子が URL を出して直ぐに終了したときや、負荷で読み取りタスクが遅れたときに
/// 「URL を出す前に終了した」と誤判定する（accounts_admin の codex ログインテストが CI 負荷下で落ちた原因）。
pub(crate) async fn drain_readers(
    out_task: &mut Option<JoinHandle<()>>,
    err_task: &mut Option<JoinHandle<()>>,
) {
    if let Some(handle) = out_task.take() {
        join_with_timeout(handle, READER_JOIN_TIMEOUT).await;
    }
    if let Some(handle) = err_task.take() {
        join_with_timeout(handle, READER_JOIN_TIMEOUT).await;
    }
}

pub async fn start_login(
    command: &str,
    account_dir: &Path,
    base_env: &[(String, String)],
    url_timeout: Duration,
) -> Result<LoginSession, LoginError> {
    let mut command_builder = Command::new(command);
    command_builder.arg("auth").arg("login");
    command_builder
        .envs(base_env.iter().cloned())
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", account_dir)
        .env("BROWSER", "/bin/true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command_builder.process_group(0);

    let mut child = command_builder.spawn().map_err(LoginError::Spawn)?;
    let stdin = child.stdin.take().ok_or(LoginError::NotPiped)?;
    let stdout = child.stdout.take().ok_or(LoginError::NotPiped)?;
    let stderr = child.stderr.take().ok_or(LoginError::NotPiped)?;

    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let mut out_task = Some(tokio::spawn(pump_reader(stdout, buf.clone())));
    let mut err_task = Some(tokio::spawn(pump_reader(stderr, buf.clone())));

    let start = Instant::now();
    let url = loop {
        {
            let snapshot = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(url) = extract_oauth_url(&snapshot) {
                break Some(url);
            }
        }
        if let Ok(Some(_status)) = child.try_wait() {
            // 終了後もパイプに出力が残りうる。読み切ってから最後にもう一度だけ探す。
            drain_readers(&mut out_task, &mut err_task).await;
            let snapshot = buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
            break extract_oauth_url(&snapshot);
        }
        if start.elapsed() >= url_timeout {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    match url {
        Some(url) => Ok(LoginSession {
            url,
            started_at: start,
            account_dir: account_dir.to_path_buf(),
            child: Some(child),
            stdin: Some(stdin),
            out_task,
            err_task,
            buf,
        }),
        None => {
            let exited = child.try_wait().ok().flatten().is_some();
            // N4: プロセスグループごと kill する（`process_group(0)` で起動しているので、`claude auth login`
            // が起動する孫プロセスも一緒に止める）。
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

impl LoginSession {
    /// コード（+ 改行）を stdin に書き、`wait` 以内の終了を待つ。exit 0 かつ
    /// `<account_dir>/.credentials.json` ができていれば `Ok`。コードはログにも `detail` にも出さない。
    ///
    /// N6: 前後の空白を除いた上で、改行や制御文字を含むコードは stdin に書かずに `failed` を返す
    /// （子プロセスの標準入力への注入を防ぐ。`detail` にもコードは出さない）。この場合セッションはここで
    /// 消費され、`Drop` がプロセスグループを kill する。
    pub async fn submit_code(mut self, code: &str, wait: Duration) -> LoginResult {
        let trimmed = code.trim();
        if trimmed.is_empty() || trimmed.chars().any(|c| c.is_control()) {
            return LoginResult {
                result: LoginOutcome::Failed,
                detail: Some("invalid authorization code".to_string()),
            };
        }

        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(trimmed.as_bytes()).await;
            let _ = stdin.write_all(b"\n").await;
            let _ = stdin.flush().await;
            drop(stdin);
        }

        let status = if let Some(mut child) = self.child.take() {
            match tokio::time::timeout(wait, child.wait()).await {
                Ok(status) => status.ok(),
                Err(_elapsed) => {
                    // N4: プロセスグループごと kill する。
                    send_signal_to_group(&child, Signal::SIGKILL);
                    let _ = child.wait().await;
                    None
                }
            }
        } else {
            None
        };

        // N5: 子は既に wait 済みなのでパイプはすぐ閉じるはずだが、詰まっても中継全体を止めないよう上限を付ける。
        if let Some(t) = self.out_task.take() {
            join_with_timeout(t, READER_JOIN_TIMEOUT).await;
        }
        if let Some(t) = self.err_task.take() {
            join_with_timeout(t, READER_JOIN_TIMEOUT).await;
        }

        let credentials_exist = self.account_dir.join(".credentials.json").is_file();
        let ok = status.map(|s| s.success()).unwrap_or(false) && credentials_exist;

        let snapshot = self.buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let detail = last_line_detail(&snapshot);

        LoginResult {
            result: if ok {
                LoginOutcome::Ok
            } else {
                LoginOutcome::Failed
            },
            detail,
        }
    }

    /// 進行中のログインを止める（ADR-0024 D7: `DELETE /accounts/{id}/login` / 10 分での打ち切り / celeris 終了時）。
    /// N4: プロセスグループごと kill する（`claude auth login` の孫プロセスも一緒に止める）。
    pub fn cancel(mut self) {
        if let Some(child) = self.child.take() {
            send_signal_to_group(&child, Signal::SIGKILL);
        }
    }
}

impl Drop for LoginSession {
    /// N4: プロセスグループごと kill する。`kill_on_drop(true)` は子プロセス自身しか kill しないので、
    /// `claude auth login` が起動した孫プロセスを止めるにはグループへ直接シグナルを送る必要がある。
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            send_signal_to_group(&child, Signal::SIGKILL);
        }
    }
}

/// エスケープを除いた出力の最後の空でない行を `detail` にする（1 行・200 文字まで、URL は伏せる）。
fn last_line_detail(bytes: &[u8]) -> Option<String> {
    let stripped = strip_escape_codes(bytes);
    let last = stripped
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())?;
    let redacted = redact_urls(last);
    Some(truncate_detail(&redacted))
}

/// `https://...` を `<url>` に置き換える（D5: URL はログにも `detail` にも出さない）。
fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find("https://") {
        out.push_str(&rest[..idx]);
        out.push_str("<url>");
        let after = &rest[idx + "https://".len()..];
        let end = after.find(char::is_whitespace).unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// OSC 8 のハイパーリンクや CSI のエスケープシーケンスを取り除く（ADR-0024 D7）。
pub(crate) fn strip_escape_codes(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\u{07}' {
                out.push(c);
            }
            continue;
        }
        match chars.peek() {
            Some(']') => {
                chars.next();
                loop {
                    match chars.next() {
                        None => break,
                        Some('\u{07}') => break,
                        Some('\u{1b}') => {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                        Some(_) => continue,
                    }
                }
            }
            Some('[') => {
                chars.next();
                for ch in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&ch) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// エスケープを除いた上で、最初に見つかる `/oauth/authorize` を含む `https://` URL を返す。
fn extract_oauth_url(bytes: &[u8]) -> Option<String> {
    let stripped = strip_escape_codes(bytes);
    let mut search_from = 0usize;
    while let Some(rel) = stripped[search_from..].find("https://") {
        let idx = search_from + rel;
        let rest = &stripped[idx..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let candidate = &rest[..end];
        if candidate.contains("/oauth/authorize") {
            return Some(candidate.to_string());
        }
        search_from = idx + "https://".len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("claude_stub.sh");
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        path
    }

    fn account_dir(root: &Path, id: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).expect("account dir");
        dir
    }

    // ---- check_account ----

    #[tokio::test]
    async fn check_account_ok_path_records_observation() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            r#"echo '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1789605600,"rateLimitType":"five_hour","overageStatus":"rejected","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.14,"resetsAt":1789605600},"seven_day":{"utilization":0.24,"resetsAt":1790031600}}}}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"ok"}'
"#,
        );
        let acct = account_dir(dir.path(), "a");
        let check = check_account(
            command.to_str().unwrap(),
            &acct,
            "haiku",
            Duration::from_secs(5),
            &[],
        )
        .await;
        assert_eq!(check.result, AccountCheckResult::Ok);
        assert_eq!(check.detail.as_deref(), Some("ok"));
        let obs = check.observation.expect("observation");
        assert_eq!(obs.five_hour.map(|w| w.utilization), Some(0.14));
        assert_eq!(obs.seven_day.map(|w| w.utilization), Some(0.24));
    }

    #[tokio::test]
    async fn check_account_auth_failure_path() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            r#"echo '{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login"}'"#,
        );
        let acct = account_dir(dir.path(), "a");
        let check = check_account(
            command.to_str().unwrap(),
            &acct,
            "haiku",
            Duration::from_secs(5),
            &[],
        )
        .await;
        assert_eq!(check.result, AccountCheckResult::AuthFailed);
        assert!(check.detail.unwrap().contains("login"));
    }

    #[tokio::test]
    async fn check_account_spawn_failure_for_nonexistent_command() {
        let dir = tempfile::tempdir().unwrap();
        let acct = account_dir(dir.path(), "a");
        let missing = dir.path().join("does-not-exist");
        let check = check_account(
            missing.to_str().unwrap(),
            &acct,
            "haiku",
            Duration::from_secs(5),
            &[],
        )
        .await;
        assert_eq!(check.result, AccountCheckResult::SpawnFailed);
    }

    // ---- login ----

    fn login_stub(dir: &Path) -> PathBuf {
        stub(
            dir,
            r#"printf 'Opening browser to sign in\xe2\x80\xa6\n'
printf "If the browser didn't open, visit: \033]8;;https://claude.com/cai/oauth/authorize?code=true&client_id=x&state=abc\007https://claude.com/cai/oauth/authorize?code=true&client_id=x&state=abc\033]8;;\007\n"
printf 'Paste code here if prompted > '
read -r code
if [ "$code" = "good-code" ]; then
  printf '%s' '{}' > "$CLAUDE_SECURESTORAGE_CONFIG_DIR/.credentials.json"
  echo 'Login successful.'
  exit 0
else
  echo 'Login failed: Request failed with status code 400'
  exit 1
fi
"#,
        )
    }

    #[tokio::test]
    async fn start_login_extracts_the_exact_oauth_url_without_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path());
        let acct = account_dir(dir.path(), "a");
        let session = start_login(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
        .await
        .expect("session");
        assert_eq!(
            session.url,
            "https://claude.com/cai/oauth/authorize?code=true&client_id=x&state=abc"
        );
        session.cancel();
    }

    #[tokio::test]
    async fn submit_code_ok_with_the_correct_code() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path());
        let acct = account_dir(dir.path(), "a");
        let session = start_login(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
        .await
        .expect("session");
        let result = session
            .submit_code("good-code", Duration::from_secs(5))
            .await;
        assert_eq!(result.result, LoginOutcome::Ok);
        assert!(acct.join(".credentials.json").is_file());
    }

    #[tokio::test]
    async fn submit_code_failed_with_the_wrong_code_detail_has_no_code() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path());
        let acct = account_dir(dir.path(), "a");
        let session = start_login(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
        .await
        .expect("session");
        let result = session
            .submit_code("bad-code", Duration::from_secs(5))
            .await;
        assert_eq!(result.result, LoginOutcome::Failed);
        let detail = result.detail.expect("detail");
        assert!(detail.contains("400"), "{detail}");
        assert!(!detail.contains("bad-code"), "{detail}");
        assert!(!acct.join(".credentials.json").is_file());
    }

    /// N6: 改行・制御文字を含むコードは stdin に書かずに拒否する（`detail` にもコードは出さない）。
    #[tokio::test]
    async fn submit_code_rejects_control_characters_without_echoing_the_code() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path());
        let acct = account_dir(dir.path(), "a");
        let session = start_login(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
        .await
        .expect("session");
        let result = session
            .submit_code("good-code\nrm -rf /", Duration::from_secs(5))
            .await;
        assert_eq!(result.result, LoginOutcome::Failed);
        let detail = result.detail.expect("detail");
        assert!(!detail.contains("good-code"), "{detail}");
        assert!(!detail.contains("rm -rf"), "{detail}");
        assert!(!acct.join(".credentials.json").is_file());
    }

    /// N6: 空白だけのコードも拒否する。
    /// 回帰（codex 側と同じ競合）: URL を出してすぐ終了する子でも、パイプを読み切ってから探すので URL が取れる。
    #[tokio::test]
    async fn start_login_finds_the_url_even_when_the_process_exits_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let command = stub(
            dir.path(),
            "printf 'visit: https://claude.com/cai/oauth/authorize?code=true&client_id=x&state=abc\n'
exit 0
",
        );
        for i in 0..20 {
            let acct = account_dir(dir.path(), &format!("a{i}"));
            let session = start_login(
                command.to_str().unwrap(),
                &acct,
                &[],
                Duration::from_secs(5),
            )
            .await
            .unwrap_or_else(|e| panic!("iteration {i}: {e}"));
            assert_eq!(
                session.url,
                "https://claude.com/cai/oauth/authorize?code=true&client_id=x&state=abc"
            );
            session.cancel();
        }
    }

    #[tokio::test]
    async fn submit_code_rejects_whitespace_only_code() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path());
        let acct = account_dir(dir.path(), "a");
        let session = start_login(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
        .await
        .expect("session");
        let result = session.submit_code("   ", Duration::from_secs(5)).await;
        assert_eq!(result.result, LoginOutcome::Failed);
        assert!(!acct.join(".credentials.json").is_file());
    }

    #[tokio::test]
    async fn cancel_kills_the_login_process() {
        let dir = tempfile::tempdir().unwrap();
        let command = login_stub(dir.path());
        let acct = account_dir(dir.path(), "a");
        let session = start_login(
            command.to_str().unwrap(),
            &acct,
            &[],
            Duration::from_secs(5),
        )
        .await
        .expect("session");
        let pid = session.child.as_ref().and_then(|c| c.id()).expect("pid");
        session.cancel();
        // The child should exit shortly after being killed.
        for _ in 0..100 {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("process {pid} is still alive after cancel()");
    }
}
