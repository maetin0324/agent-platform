//! ADR-0024 D5〜D7 / ADR-0025 D4〜D5: taskd 側のアカウント管理（手動確認・ログインの中継）。
//!
//! task-api からは `task_api::AdminRequest` 経由でだけ触れる（DESIGN §5.10 の境界。ワーカー・子プロセスの
//! 起動は taskd 側）。長くかかる操作（確認・ログイン）は `tick_loop` をブロックしないよう `tokio::spawn` する。
//! 結果は 2 つの経路で返る: 呼び出し元（task-api）への `oneshot` 応答と、`Dispatcher`（`AccountBook`・
//! `login_pending`）へ反映するための `AccountAdminEvent` チャネル。
//!
//! claude-code（ADR-0024 D6/D7）と codex（ADR-0025 D4/D5）はログインの流儀が違うので、`LoginSessions` を
//! アダプタごとに別のマップに分ける（claude-code はコード貼り付け式、codex は端末を跨いだデバイス認証で
//! `login/code` を使わない）。確認は共通の語彙（`AccountCheckResult` 等）に写して扱う。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use task_api::{AccountAdminError, AccountCheckOutcome, AccountLoginCodeOutcome, AccountLoginStartOutcome};
use task_core::{AccountAdapter, RateLimitObservation};
use task_worker::{
    AccountCheckResult, CodexLoginSession, LoginOutcome, LoginSession, check_account, check_account_codex,
    start_login, start_login_codex,
};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::config::Config;

/// D6 の確認 1 回あたりの上限（`AdapterError` 等の分類は `task_worker::claude_account`/`codex_account` 側で行う）。
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
/// D7: 認可 URL が出るまでの上限（claude-code）。
pub const LOGIN_URL_TIMEOUT: Duration = Duration::from_secs(15);
/// D7: コード送信後、終了を待つ上限（claude-code）。
pub const LOGIN_CODE_WAIT: Duration = Duration::from_secs(30);
/// D7: 進行中のログインを打ち切るまでの時間（claude-code）。
pub const LOGIN_EXPIRY: Duration = Duration::from_secs(600);
/// ADR-0025 D5: 認可 URL とコードが出るまでの上限（codex）。
pub const LOGIN_URL_TIMEOUT_CODEX: Duration = Duration::from_secs(15);
/// ADR-0025 D5: 人が別デバイスでコードを入力し終わるのを待つ上限（codex の一回限りのコードの実際の有効期限）。
pub const LOGIN_EXPIRY_CODEX: Duration = Duration::from_secs(900);

/// 進行中のログイン中継（アカウントごとに高々 1 つ）。taskd の生存期間だけ持つ（プロセス終了で Drop → kill）。
pub type LoginSessions = Arc<Mutex<HashMap<String, LoginSession>>>;
/// ADR-0025 D5: codex 版（別の型・別の流儀なので別のマップに分ける）。
pub type CodexLoginSessions = Arc<Mutex<HashMap<String, CodexLoginSession>>>;

pub fn new_sessions() -> LoginSessions {
    Arc::new(Mutex::new(HashMap::new()))
}

pub fn new_codex_sessions() -> CodexLoginSessions {
    Arc::new(Mutex::new(HashMap::new()))
}

/// `handle_account_admin_request` の結果を `Dispatcher` に反映するための通知（`tick_loop` が受け取る）。
pub enum AccountAdminEvent {
    /// D6: 確認結果を `AccountBook` に記録する（`source = "check"`）。
    Checked {
        adapter: AccountAdapter,
        id: String,
        result: String,
        detail: Option<String>,
        observation: Option<RateLimitObservation>,
    },
    /// D7: 進行中のログインの有無（開始・コード送信・打ち切りのたびに送る）。
    LoginPending { adapter: AccountAdapter, id: String, pending: bool },
}

/// 10 分を超えたログイン中継を打ち切り、打ち切ったアカウント id を返す（`tick_loop` が毎 tick 呼ぶ。claude-code）。
///
/// B1: `tick_loop` 自身が drain する `account_tx`（容量 16）へ、この関数の中から `await` で送ってはいけない
/// （`tick_loop` は `select!` に戻るまでチャネルを読まないので、詰まると `tick_loop` 自身がここで永遠に
/// ブロックしてデッドロックする）。そのため、ここではチャネルを一切使わず、打ち切った id を呼び出し側
/// （`tick_loop`）に返すだけにし、`Dispatcher::set_account_login_pending` は呼び出し側が直接呼ぶ。
///
/// `expiry` は N12（テストで注入できるように 10 分固定にしない）: 本番は `LOGIN_EXPIRY` を渡す。
pub async fn expire_stale_logins(sessions: &LoginSessions, expiry: Duration) -> Vec<String> {
    let mut guard = sessions.lock().await;
    let expired: Vec<String> = guard
        .iter()
        .filter(|(_, s)| s.started_at.elapsed() >= expiry)
        .map(|(id, _)| id.clone())
        .collect();
    for id in &expired {
        if let Some(session) = guard.remove(id) {
            tracing::info!(who = "admin", op = "account_login_expire", account_id = %id, adapter = "claude-code", "admin: login session expired");
            session.cancel();
        }
    }
    drop(guard);
    expired
}

/// ADR-0025 D5 / B1 と同じ理由: codex の進行中のログインを 15 分で打ち切る（`tick_loop` が毎 tick 呼ぶ）。
pub async fn expire_stale_codex_logins(sessions: &CodexLoginSessions, expiry: Duration) -> Vec<String> {
    let mut guard = sessions.lock().await;
    let expired: Vec<String> = guard
        .iter()
        .filter(|(_, s)| s.started_at.elapsed() >= expiry)
        .map(|(id, _)| id.clone())
        .collect();
    for id in &expired {
        if let Some(session) = guard.remove(id) {
            tracing::info!(who = "admin", op = "account_login_expire", account_id = %id, adapter = "codex", "admin: login session expired");
            session.cancel();
        }
    }
    drop(guard);
    expired
}

/// ADR-0025 D5: codex は標準入力を使わず、人が別デバイスでコードを入力し終わるのを待つだけ。子プロセスの
/// 終了を非同期にブロックせず毎 tick ポーリングし、終わっていたセッションを取り除いて `(id, ok)` を返す
/// （`tick_loop` が毎 tick 呼ぶ。`expire_stale_codex_logins` と同じ理由でチャネルを使わない）。
pub async fn poll_codex_logins(sessions: &CodexLoginSessions) -> Vec<(String, bool)> {
    let mut guard = sessions.lock().await;
    let mut finished = Vec::new();
    let ids: Vec<String> = guard.keys().cloned().collect();
    for id in ids {
        let Some(session) = guard.get_mut(&id) else { continue };
        if let Some(result) = session.try_finished() {
            guard.remove(&id);
            let ok = result.result == LoginOutcome::Ok;
            tracing::info!(who = "admin", op = "account_login_finished", account_id = %id, adapter = "codex", ok, "admin: codex device login finished");
            finished.push((id, ok));
        }
    }
    finished
}

/// `[adapters.claude_code].command` と、そのアダプタの env（**プロバイダの env ではない**。ADR-0024 D6/D7）。
fn claude_command_and_env(config: &Config) -> (String, Vec<(String, String)>) {
    let base = &config.adapters.claude_code;
    let mut env: Vec<(String, String)> = base.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    env.sort();
    (base.command.clone(), env)
}

/// `[adapters.codex].command` と、そのアダプタの env（ADR-0025 D4/D5）。
fn codex_command_and_env(config: &Config) -> (String, Vec<(String, String)>) {
    let base = &config.adapters.codex;
    let mut env: Vec<(String, String)> = base.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    env.sort();
    (base.command.clone(), env)
}

/// アカウントディレクトリを解決する。`id` が無効、`[accounts]` にそのアダプタの根が無い、ディレクトリが
/// 無ければエラー（ADR-0025 D1）。
fn account_dir(config: &Config, adapter: AccountAdapter, id: &str) -> Result<PathBuf, AccountAdminError> {
    let accounts = config
        .accounts
        .as_ref()
        .ok_or_else(|| AccountAdminError::Unavailable("the [accounts] section is not configured".to_string()))?;
    if !task_dispatch::valid_account_id(id) {
        return Err(AccountAdminError::NotFound);
    }
    let root = accounts.root_for(adapter).ok_or_else(|| {
        AccountAdminError::Unavailable(format!("the [accounts] section has no root configured for adapter {adapter}"))
    })?;
    let dir = root.join(id);
    if !dir.is_dir() {
        return Err(AccountAdminError::NotFound);
    }
    Ok(dir)
}

/// S2+S8 (ADR-0024 D5 / ADR-0025 D5): `DELETE /accounts/{id}`。taskd 側で行う（スナップショットではなく、
/// ディスパッチャの権威ある `account_in_use`（running/reviewing を直接見る）でレースなく判定できるため）。
/// cheap な fs 操作なので `tick_loop` からは spawn せずそのまま呼ぶ。進行中のログインがあれば止め、
/// `.removed/<id>-<unix>` へ移し、`AccountBook` からもこのアカウントの記録を消す。
pub async fn remove_account(
    config: &Config,
    dispatcher: &mut task_dispatch::Dispatcher,
    sessions: &LoginSessions,
    codex_sessions: &CodexLoginSessions,
    adapter: AccountAdapter,
    id: &str,
) -> Result<(), AccountAdminError> {
    let dir = account_dir(config, adapter, id)?;
    if dispatcher.account_in_use(adapter, id) > 0 {
        return Err(AccountAdminError::InUse);
    }
    match adapter {
        AccountAdapter::ClaudeCode => {
            if let Some(session) = sessions.lock().await.remove(id) {
                session.cancel();
            }
        }
        AccountAdapter::Codex => {
            if let Some(session) = codex_sessions.lock().await.remove(id) {
                session.cancel();
            }
        }
    }
    dispatcher.set_account_login_pending(adapter, id, false);
    // `account_dir` は `[accounts]` とそのアダプタの根の存在を既に確かめている。
    let accounts = config
        .accounts
        .as_ref()
        .ok_or_else(|| AccountAdminError::Unavailable("the [accounts] section is not configured".to_string()))?;
    let root = accounts
        .root_for(adapter)
        .ok_or_else(|| AccountAdminError::Unavailable(format!("no root configured for adapter {adapter}")))?;
    let removed_dir = root.join(".removed");
    // 移した先にも認証ファイルが残るので、アカウントのディレクトリと同じく本人だけが読める権限にする。
    {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new().mode(0o700).create(&removed_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(AccountAdminError::Unavailable(format!("failed to create .removed dir: {e}"))),
        }
    }
    let dest = removed_dir.join(format!("{id}-{}", unix_now()));
    std::fs::rename(&dir, &dest).map_err(|e| AccountAdminError::Unavailable(format!("failed to move account dir: {e}")))?;
    dispatcher.remove_account_book_entry(adapter, id);
    Ok(())
}

/// D6 / ADR-0025 D4: `POST /accounts/{id}/check`。設定は呼び出しごとに読み直さない（`config` は tick_loop が
/// 持つ最新の写し）。claude-code は `[accounts].check_model` を使い、codex はモデルを指定しない。
pub fn spawn_check(
    config: &Config,
    adapter: AccountAdapter,
    id: String,
    events: mpsc::Sender<AccountAdminEvent>,
    reply: oneshot::Sender<Result<AccountCheckOutcome, AccountAdminError>>,
) {
    let dir = match account_dir(config, adapter, &id) {
        Ok(dir) => dir,
        Err(e) => {
            let _ = reply.send(Err(e));
            return;
        }
    };
    match adapter {
        AccountAdapter::ClaudeCode => {
            let check_model = config.accounts.as_ref().map(|a| a.check_model.clone()).unwrap_or_default();
            let (command, env) = claude_command_and_env(config);
            tokio::spawn(async move {
                let check = check_account(&command, &dir, &check_model, CHECK_TIMEOUT, &env).await;
                finish_check(adapter, id, check.result, check.detail, check.observation, events, reply).await;
            });
        }
        AccountAdapter::Codex => {
            let (command, env) = codex_command_and_env(config);
            tokio::spawn(async move {
                let check = check_account_codex(&command, &dir, CHECK_TIMEOUT, &env).await;
                finish_check(adapter, id, check.result, check.detail, check.observation, events, reply).await;
            });
        }
    }
}

async fn finish_check(
    adapter: AccountAdapter,
    id: String,
    result: AccountCheckResult,
    detail: Option<String>,
    observation: Option<RateLimitObservation>,
    events: mpsc::Sender<AccountAdminEvent>,
    reply: oneshot::Sender<Result<AccountCheckOutcome, AccountAdminError>>,
) {
    let result_name = account_check_result_name(&result).to_string();
    let outcome = AccountCheckOutcome { result: map_check_result(result), detail: detail.clone(), observation: observation.clone() };
    let _ = events
        .send(AccountAdminEvent::Checked { adapter, id, result: result_name, detail, observation })
        .await;
    let _ = reply.send(Ok(outcome));
}

/// D7 / ADR-0025 D5: `POST /accounts/{id}/login`。既に進行中のセッションがあれば止めてから新しく始める。
/// claude-code は `claude auth login`（コード貼り付け式）、codex は `codex login --device-auth`
/// （デバイス認証。`user_code` を返す）。
#[allow(clippy::too_many_arguments)]
pub fn spawn_login_start(
    config: &Config,
    sessions: LoginSessions,
    codex_sessions: CodexLoginSessions,
    adapter: AccountAdapter,
    id: String,
    events: mpsc::Sender<AccountAdminEvent>,
    reply: oneshot::Sender<Result<AccountLoginStartOutcome, AccountAdminError>>,
) {
    let dir = match account_dir(config, adapter, &id) {
        Ok(dir) => dir,
        Err(e) => {
            let _ = reply.send(Err(e));
            return;
        }
    };
    match adapter {
        AccountAdapter::ClaudeCode => {
            let (command, env) = claude_command_and_env(config);
            tokio::spawn(async move {
                let had_old = {
                    let mut guard = sessions.lock().await;
                    match guard.remove(&id) {
                        Some(old) => {
                            old.cancel();
                            true
                        }
                        None => false,
                    }
                };
                match start_login(&command, &dir, &env, LOGIN_URL_TIMEOUT).await {
                    Ok(session) => {
                        let url = session.url.clone();
                        let expires_at_unix = unix_now() + LOGIN_EXPIRY.as_secs() as i64;
                        sessions.lock().await.insert(id.clone(), session);
                        let _ = events
                            .send(AccountAdminEvent::LoginPending { adapter, id: id.clone(), pending: true })
                            .await;
                        let _ = reply.send(Ok(AccountLoginStartOutcome { url, expires_at_unix, user_code: None }));
                    }
                    Err(e) => {
                        // B2: 古いセッションを止めた後に新しい start_login 自体が失敗したら、login_pending を false に
                        // 戻す（そのままだと `sessions` には無いのに GUI には「進行中」が残り続ける）。
                        if had_old {
                            let _ = events.send(AccountAdminEvent::LoginPending { adapter, id: id.clone(), pending: false }).await;
                        }
                        // D5: URL・認可コードは失敗時もログには出さない（アカウント id と操作名だけ）。
                        let _ = reply.send(Err(AccountAdminError::LoginFailed(e.to_string())));
                    }
                }
            });
        }
        AccountAdapter::Codex => {
            let (command, env) = codex_command_and_env(config);
            tokio::spawn(async move {
                let had_old = {
                    let mut guard = codex_sessions.lock().await;
                    match guard.remove(&id) {
                        Some(old) => {
                            old.cancel();
                            true
                        }
                        None => false,
                    }
                };
                match start_login_codex(&command, &dir, &env, LOGIN_URL_TIMEOUT_CODEX).await {
                    Ok(session) => {
                        let url = session.url.clone();
                        let user_code = session.user_code.clone();
                        let expires_at_unix = unix_now() + LOGIN_EXPIRY_CODEX.as_secs() as i64;
                        codex_sessions.lock().await.insert(id.clone(), session);
                        let _ = events
                            .send(AccountAdminEvent::LoginPending { adapter, id: id.clone(), pending: true })
                            .await;
                        let _ = reply.send(Ok(AccountLoginStartOutcome { url, expires_at_unix, user_code: Some(user_code) }));
                    }
                    Err(e) => {
                        if had_old {
                            let _ = events.send(AccountAdminEvent::LoginPending { adapter, id: id.clone(), pending: false }).await;
                        }
                        let _ = reply.send(Err(AccountAdminError::LoginFailed(e.to_string())));
                    }
                }
            });
        }
    }
}

/// D7: `POST /accounts/{id}/login/code`。claude-code のみ（task-api が codex を 409 で弾く）。進行中の
/// セッションが無ければ `LoginNotStarted`。
pub fn spawn_login_code(
    sessions: LoginSessions,
    id: String,
    code: String,
    events: mpsc::Sender<AccountAdminEvent>,
    reply: oneshot::Sender<Result<AccountLoginCodeOutcome, AccountAdminError>>,
) {
    tokio::spawn(async move {
        let session = sessions.lock().await.remove(&id);
        let Some(session) = session else {
            let _ = reply.send(Err(AccountAdminError::LoginNotStarted));
            return;
        };
        let result = session.submit_code(&code, LOGIN_CODE_WAIT).await;
        let _ = events
            .send(AccountAdminEvent::LoginPending { adapter: AccountAdapter::ClaudeCode, id, pending: false })
            .await;
        let _ = reply.send(Ok(AccountLoginCodeOutcome {
            ok: result.result == LoginOutcome::Ok,
            detail: result.detail,
        }));
    });
}

/// D7 / 3.35 / ADR-0025 D5: `DELETE /accounts/{id}/login`。進行中でなければ何もしない（エラーにしない）。
pub fn spawn_login_cancel(
    sessions: LoginSessions,
    codex_sessions: CodexLoginSessions,
    adapter: AccountAdapter,
    id: String,
    events: mpsc::Sender<AccountAdminEvent>,
    reply: oneshot::Sender<Result<(), AccountAdminError>>,
) {
    tokio::spawn(async move {
        let cancelled = match adapter {
            AccountAdapter::ClaudeCode => sessions.lock().await.remove(&id).is_some_and(|s| {
                s.cancel();
                true
            }),
            AccountAdapter::Codex => codex_sessions.lock().await.remove(&id).is_some_and(|s| {
                s.cancel();
                true
            }),
        };
        if cancelled {
            let _ = events.send(AccountAdminEvent::LoginPending { adapter, id, pending: false }).await;
        }
        let _ = reply.send(Ok(()));
    });
}

fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn account_check_result_name(result: &AccountCheckResult) -> &'static str {
    match result {
        AccountCheckResult::Ok => "ok",
        AccountCheckResult::AuthFailed => "auth_failed",
        AccountCheckResult::Throttled => "throttled",
        AccountCheckResult::SpawnFailed => "spawn_failed",
    }
}

fn map_check_result(result: AccountCheckResult) -> task_api::ProviderCheckResult {
    match result {
        AccountCheckResult::Ok => task_api::ProviderCheckResult::Ok,
        AccountCheckResult::AuthFailed => task_api::ProviderCheckResult::AuthFailed,
        AccountCheckResult::Throttled => task_api::ProviderCheckResult::Throttled,
        AccountCheckResult::SpawnFailed => task_api::ProviderCheckResult::SpawnFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_command(dir: &std::path::Path, script: &str) -> String {
        let path = dir.join("claude_stub.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path.to_string_lossy().into_owned()
    }

    fn config_with_accounts(claude_dir: PathBuf, command: String) -> Config {
        let text = format!(
            "[accounts]\nclaude_dir = {claude_dir:?}\n[adapters.claude_code]\ncommand = {command:?}\n[[providers]]\nid = \"x\"\nadapter = \"claude-code\"\n"
        );
        let cfg: Config = toml::from_str(&text).unwrap();
        cfg.validate().unwrap();
        cfg
    }

    fn config_with_codex_accounts(codex_dir: PathBuf, command: String) -> Config {
        let text = format!(
            "[accounts]\ncodex_dir = {codex_dir:?}\n[adapters.codex]\ncommand = {command:?}\n[[providers]]\nid = \"x\"\nadapter = \"codex\"\n"
        );
        let cfg: Config = toml::from_str(&text).unwrap();
        cfg.validate().unwrap();
        cfg
    }

    #[tokio::test]
    async fn spawn_check_reports_ok_and_records_observation_via_event() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("claude-accounts");
        std::fs::create_dir_all(acct.join("a")).unwrap();
        let command = stub_command(
            tmp.path(),
            r#"echo '{"type":"result","subtype":"success","is_error":false,"result":"ok"}'"#,
        );
        let config = config_with_accounts(acct, command);
        let (tx, mut rx) = mpsc::channel(4);
        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_check(&config, AccountAdapter::ClaudeCode, "a".to_string(), tx, reply_tx);
        let outcome = reply_rx.await.unwrap().unwrap();
        assert_eq!(outcome.result, task_api::ProviderCheckResult::Ok);
        let event = rx.recv().await.unwrap();
        match event {
            AccountAdminEvent::Checked { adapter, id, result, .. } => {
                assert_eq!(adapter, AccountAdapter::ClaudeCode);
                assert_eq!(id, "a");
                assert_eq!(result, "ok");
            }
            _ => panic!("expected Checked event"),
        }
    }

    #[tokio::test]
    async fn spawn_check_missing_account_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("claude-accounts");
        std::fs::create_dir_all(&acct).unwrap();
        let config = config_with_accounts(acct, "claude".into());
        let (tx, _rx) = mpsc::channel(4);
        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_check(&config, AccountAdapter::ClaudeCode, "missing".to_string(), tx, reply_tx);
        assert!(matches!(reply_rx.await.unwrap(), Err(AccountAdminError::NotFound)));
    }

    /// ADR-0025 D4: codex の確認は `[accounts].check_model` を使わず、`check_account_codex` に委譲する。
    #[tokio::test]
    async fn spawn_check_codex_reports_ok_and_records_observation() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("codex-accounts");
        std::fs::create_dir_all(acct.join("a")).unwrap();
        let command = stub_command(tmp.path(), r#"echo '{"type":"turn.completed"}'"#);
        let config = config_with_codex_accounts(acct, command);
        let (tx, mut rx) = mpsc::channel(4);
        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_check(&config, AccountAdapter::Codex, "a".to_string(), tx, reply_tx);
        let outcome = reply_rx.await.unwrap().unwrap();
        assert_eq!(outcome.result, task_api::ProviderCheckResult::Ok);
        let event = rx.recv().await.unwrap();
        match event {
            AccountAdminEvent::Checked { adapter, id, result, .. } => {
                assert_eq!(adapter, AccountAdapter::Codex);
                assert_eq!(id, "a");
                assert_eq!(result, "ok");
            }
            _ => panic!("expected Checked event"),
        }
    }

    /// ADR-0025 D4: 未ログイン（401）なら `spawn_check` の結果は `auth_failed`。
    #[tokio::test]
    async fn spawn_check_codex_auth_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("codex-accounts");
        std::fs::create_dir_all(acct.join("a")).unwrap();
        let command = stub_command(
            tmp.path(),
            "echo 'unexpected status 401 Unauthorized: Missing bearer or basic authentication in header'",
        );
        let config = config_with_codex_accounts(acct, command);
        let (tx, _rx) = mpsc::channel(4);
        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_check(&config, AccountAdapter::Codex, "a".to_string(), tx, reply_tx);
        let outcome = reply_rx.await.unwrap().unwrap();
        assert_eq!(outcome.result, task_api::ProviderCheckResult::AuthFailed);
    }

    #[tokio::test]
    async fn login_start_code_cancel_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("claude-accounts");
        std::fs::create_dir_all(acct.join("a")).unwrap();
        let command = stub_command(
            tmp.path(),
            r#"printf "visit: \033]8;;https://claude.com/cai/oauth/authorize?x=1\007https://claude.com/cai/oauth/authorize?x=1\033]8;;\007\n"
printf 'code> '
read -r code
if [ "$code" = "good-code" ]; then
  printf '%s' '{}' > "$CLAUDE_SECURESTORAGE_CONFIG_DIR/.credentials.json"
  exit 0
fi
exit 1
"#,
        );
        let config = config_with_accounts(acct.clone(), command);
        let sessions = new_sessions();
        let codex_sessions = new_codex_sessions();
        let (tx, mut rx) = mpsc::channel(8);

        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(
            &config,
            sessions.clone(),
            codex_sessions.clone(),
            AccountAdapter::ClaudeCode,
            "a".to_string(),
            tx.clone(),
            reply_tx,
        );
        let started = reply_rx.await.unwrap().unwrap();
        assert_eq!(started.url, "https://claude.com/cai/oauth/authorize?x=1");
        assert_eq!(started.user_code, None);
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { pending: true, .. })));
        assert!(sessions.lock().await.contains_key("a"));

        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_code(sessions.clone(), "a".to_string(), "good-code".to_string(), tx.clone(), reply_tx);
        let result = reply_rx.await.unwrap().unwrap();
        assert!(result.ok);
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { pending: false, .. })));
        assert!(acct.join("a").join(".credentials.json").is_file());
        assert!(!sessions.lock().await.contains_key("a"));

        // 進行中でないときの login/code は LoginNotStarted。
        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_code(sessions.clone(), "a".to_string(), "x".to_string(), tx.clone(), reply_tx);
        assert!(matches!(reply_rx.await.unwrap(), Err(AccountAdminError::LoginNotStarted)));

        // cancel は進行中でなくても成功する（no-op）。
        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_cancel(sessions.clone(), codex_sessions.clone(), AccountAdapter::ClaudeCode, "a".to_string(), tx, reply_tx);
        assert!(reply_rx.await.unwrap().is_ok());
    }

    /// ADR-0025 D5: codex のログインは `login_start` が `user_code` を返し、`login/code` を使わない
    /// （taskd 側はそのメッセージを扱わない。task-api が 409 を返す）。完了はポーリング（`poll_codex_logins`）で
    /// 検知する。
    #[tokio::test]
    async fn codex_login_start_returns_user_code_and_poll_detects_completion() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("codex-accounts");
        std::fs::create_dir_all(acct.join("a")).unwrap();
        let command = stub_command(
            tmp.path(),
            r#"printf 'https://auth.openai.com/codex/device\n'
printf 'ABCD-EFGHI\n'
sleep 0.2
printf '%s' '{}' > "$CODEX_HOME/auth.json"
exit 0
"#,
        );
        let config = config_with_codex_accounts(acct.clone(), command);
        let sessions = new_sessions();
        let codex_sessions = new_codex_sessions();
        let (tx, mut rx) = mpsc::channel(8);

        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(
            &config,
            sessions,
            codex_sessions.clone(),
            AccountAdapter::Codex,
            "a".to_string(),
            tx,
            reply_tx,
        );
        let started = reply_rx.await.unwrap().unwrap();
        assert_eq!(started.url, "https://auth.openai.com/codex/device");
        assert_eq!(started.user_code.as_deref(), Some("ABCD-EFGHI"));
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { adapter: AccountAdapter::Codex, pending: true, .. })));
        assert!(codex_sessions.lock().await.contains_key("a"));

        for _ in 0..100 {
            let finished = poll_codex_logins(&codex_sessions).await;
            if !finished.is_empty() {
                assert_eq!(finished, vec![("a".to_string(), true)]);
                assert!(!codex_sessions.lock().await.contains_key("a"));
                assert!(acct.join("a").join("auth.json").is_file());
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("codex login never finished");
    }

    /// N12 相当: `expire_stale_codex_logins` の期限は呼び出し側が注入できる。
    #[tokio::test]
    async fn expire_stale_codex_logins_cancels_sessions_older_than_the_injected_expiry() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("codex-accounts");
        std::fs::create_dir_all(acct.join("a")).unwrap();
        let command = stub_command(
            tmp.path(),
            r#"printf 'https://auth.openai.com/codex/device\n'
printf 'ABCD-EFGHI\n'
sleep 30
"#,
        );
        let config = config_with_codex_accounts(acct, command);
        let sessions = new_sessions();
        let codex_sessions = new_codex_sessions();
        let (tx, mut rx) = mpsc::channel(8);

        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(&config, sessions, codex_sessions.clone(), AccountAdapter::Codex, "a".to_string(), tx, reply_tx);
        reply_rx.await.unwrap().unwrap();
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { pending: true, .. })));
        assert!(codex_sessions.lock().await.contains_key("a"));

        let expired = expire_stale_codex_logins(&codex_sessions, Duration::from_secs(600)).await;
        assert!(expired.is_empty());

        tokio::time::sleep(Duration::from_millis(20)).await;
        let expired = expire_stale_codex_logins(&codex_sessions, Duration::from_millis(10)).await;
        assert_eq!(expired, vec!["a".to_string()]);
        assert!(!codex_sessions.lock().await.contains_key("a"));
    }

    /// N12: `expire_stale_logins` の期限は呼び出し側が注入できる（実際に 10 分待たずにテストできる）。
    #[tokio::test]
    async fn expire_stale_logins_cancels_sessions_older_than_the_injected_expiry() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("claude-accounts");
        std::fs::create_dir_all(acct.join("a")).unwrap();
        let command = stub_command(
            tmp.path(),
            r#"printf "visit: \033]8;;https://claude.com/cai/oauth/authorize?x=1\007https://claude.com/cai/oauth/authorize?x=1\033]8;;\007\n"
sleep 30
"#,
        );
        let config = config_with_accounts(acct.clone(), command);
        let sessions = new_sessions();
        let codex_sessions = new_codex_sessions();
        let (tx, mut rx) = mpsc::channel(8);

        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(&config, sessions.clone(), codex_sessions, AccountAdapter::ClaudeCode, "a".to_string(), tx, reply_tx);
        reply_rx.await.unwrap().unwrap();
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { pending: true, .. })));
        assert!(sessions.lock().await.contains_key("a"));

        // 期限内なら何も打ち切らない。
        let expired = expire_stale_logins(&sessions, Duration::from_secs(600)).await;
        assert!(expired.is_empty());
        assert!(sessions.lock().await.contains_key("a"));

        // 短い expiry を注入すれば、実際に 10 分待たなくても打ち切りを確認できる。
        tokio::time::sleep(Duration::from_millis(20)).await;
        let expired = expire_stale_logins(&sessions, Duration::from_millis(10)).await;
        assert_eq!(expired, vec!["a".to_string()]);
        assert!(!sessions.lock().await.contains_key("a"));
    }

    // ---- S2+S8: remove_account ----

    fn config_for_dispatcher(tmp: &std::path::Path, claude_dir: PathBuf) -> Config {
        let mut config = config_with_accounts(claude_dir, "claude".into());
        config.db = tmp.join("taskd.db");
        config.workspace_root = tmp.join("ws");
        config
    }

    #[tokio::test]
    async fn remove_account_moves_the_directory_cancels_login_and_clears_the_book() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("claude-accounts");
        std::fs::create_dir_all(acct.join("a")).unwrap();
        std::fs::write(acct.join("a").join(".credentials.json"), "{}").unwrap();
        // 既存の観測値がある帳簿を用意しておき、削除後に消えることを確かめる。
        {
            let mut book = task_dispatch::AccountBook::load(&acct.join(".taskd-usage.json"));
            book.record_check("a", task_dispatch::AccountCheckRecord { at: 1, result: "ok".into(), detail: None });
            book.save().unwrap();
        }
        let config = config_for_dispatcher(tmp.path(), acct.clone());
        let mut dispatcher = crate::build_dispatcher(&config, Default::default()).unwrap();
        let sessions = new_sessions();
        let codex_sessions = new_codex_sessions();
        // 進行中のログインがあれば、削除で止められる。
        let login_command = stub_command(
            tmp.path(),
            r#"printf "visit: \033]8;;https://claude.com/cai/oauth/authorize?x=1\007https://claude.com/cai/oauth/authorize?x=1\033]8;;\007\n"
sleep 30
"#,
        );
        let login_config = config_with_accounts(acct.clone(), login_command);
        let (tx, mut rx) = mpsc::channel(8);
        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(
            &login_config,
            sessions.clone(),
            codex_sessions.clone(),
            AccountAdapter::ClaudeCode,
            "a".to_string(),
            tx.clone(),
            reply_tx,
        );
        reply_rx.await.unwrap().unwrap();
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { pending: true, .. })));

        let result = remove_account(&config, &mut dispatcher, &sessions, &codex_sessions, AccountAdapter::ClaudeCode, "a").await;
        assert!(result.is_ok(), "{result:?}");
        assert!(!acct.join("a").exists());
        assert!(!sessions.lock().await.contains_key("a"));
        let removed: Vec<_> = std::fs::read_dir(acct.join(".removed")).unwrap().collect();
        assert_eq!(removed.len(), 1);
        let moved = removed.into_iter().next().unwrap().unwrap().path();
        assert!(moved.file_name().unwrap().to_string_lossy().starts_with("a-"));
        assert!(moved.join(".credentials.json").is_file(), "credentials are not deleted, just moved");

        let reloaded = task_dispatch::AccountBook::load(&acct.join(".taskd-usage.json"));
        assert!(reloaded.state("a").is_none(), "account book entry should be cleared");
    }

    #[tokio::test]
    async fn remove_account_missing_directory_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("claude-accounts");
        std::fs::create_dir_all(&acct).unwrap();
        let config = config_for_dispatcher(tmp.path(), acct);
        let mut dispatcher = crate::build_dispatcher(&config, Default::default()).unwrap();
        let sessions = new_sessions();
        let codex_sessions = new_codex_sessions();
        let result = remove_account(&config, &mut dispatcher, &sessions, &codex_sessions, AccountAdapter::ClaudeCode, "missing").await;
        assert!(matches!(result, Err(AccountAdminError::NotFound)));
    }

    /// ADR-0025 D1: codex の `remove_account` は codex の根ディレクトリだけを見る（claude と混同しない）。
    #[tokio::test]
    async fn remove_account_codex_uses_the_codex_root() {
        let tmp = tempfile::tempdir().unwrap();
        let codex_dir = tmp.path().join("codex-accounts");
        std::fs::create_dir_all(codex_dir.join("a")).unwrap();
        std::fs::write(codex_dir.join("a").join("auth.json"), "{}").unwrap();
        let mut config = config_with_codex_accounts(codex_dir.clone(), "codex".into());
        config.db = tmp.path().join("taskd.db");
        config.workspace_root = tmp.path().join("ws");
        let mut dispatcher = crate::build_dispatcher(&config, Default::default()).unwrap();
        let sessions = new_sessions();
        let codex_sessions = new_codex_sessions();
        let result = remove_account(&config, &mut dispatcher, &sessions, &codex_sessions, AccountAdapter::Codex, "a").await;
        assert!(result.is_ok(), "{result:?}");
        assert!(!codex_dir.join("a").exists());
        let removed: Vec<_> = std::fs::read_dir(codex_dir.join(".removed")).unwrap().collect();
        assert_eq!(removed.len(), 1);
    }
}
