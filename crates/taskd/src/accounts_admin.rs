//! ADR-0024 D5〜D7: taskd 側のアカウント管理（手動確認・`claude auth login` の中継）。
//!
//! task-api からは `task_api::AdminRequest` 経由でだけ触れる（DESIGN §5.10 の境界。ワーカー・子プロセスの
//! 起動は taskd 側）。長くかかる操作（確認・ログイン）は `tick_loop` をブロックしないよう `tokio::spawn` する。
//! 結果は 2 つの経路で返る: 呼び出し元（task-api）への `oneshot` 応答と、`Dispatcher`（`AccountBook`・
//! `login_pending`）へ反映するための `AccountAdminEvent` チャネル。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use task_api::{AccountAdminError, AccountCheckOutcome, AccountLoginCodeOutcome, AccountLoginStartOutcome};
use task_core::RateLimitObservation;
use task_worker::{AccountCheckResult, LoginOutcome, LoginSession, check_account, start_login};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::config::Config;

/// D6 の確認 1 回あたりの上限（`AdapterError` 等の分類は `task_worker::claude_account` 側で行う）。
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
/// D7: 認可 URL が出るまでの上限。
pub const LOGIN_URL_TIMEOUT: Duration = Duration::from_secs(15);
/// D7: コード送信後、終了を待つ上限。
pub const LOGIN_CODE_WAIT: Duration = Duration::from_secs(30);
/// D7: 進行中のログインを打ち切るまでの時間。
pub const LOGIN_EXPIRY: Duration = Duration::from_secs(600);

/// 進行中のログイン中継（アカウントごとに高々 1 つ）。taskd の生存期間だけ持つ（プロセス終了で Drop → kill）。
pub type LoginSessions = Arc<Mutex<HashMap<String, LoginSession>>>;

pub fn new_sessions() -> LoginSessions {
    Arc::new(Mutex::new(HashMap::new()))
}

/// `handle_account_admin_request` の結果を `Dispatcher` に反映するための通知（`tick_loop` が受け取る）。
pub enum AccountAdminEvent {
    /// D6: 確認結果を `AccountBook` に記録する（`source = "check"`）。
    Checked {
        id: String,
        result: String,
        detail: Option<String>,
        observation: Option<RateLimitObservation>,
    },
    /// D7: 進行中のログインの有無（開始・コード送信・打ち切りのたびに送る）。
    LoginPending { id: String, pending: bool },
}

/// 10 分を超えたログイン中継を打ち切り、打ち切ったアカウント id を返す（`tick_loop` が毎 tick 呼ぶ）。
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
            tracing::info!(who = "admin", op = "account_login_expire", account_id = %id, "admin: login session expired");
            session.cancel();
        }
    }
    drop(guard);
    expired
}

/// `[adapters.claude_code].command` と、そのアダプタの env（**プロバイダの env ではない**。ADR-0024 D6/D7）。
fn claude_command_and_env(config: &Config) -> (String, Vec<(String, String)>) {
    let base = &config.adapters.claude_code;
    let mut env: Vec<(String, String)> = base.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    env.sort();
    (base.command.clone(), env)
}

/// アカウントディレクトリを解決する。`id` が無効、`[accounts]` が無い、ディレクトリが無ければエラー。
fn account_dir(config: &Config, id: &str) -> Result<PathBuf, AccountAdminError> {
    let accounts = config
        .accounts
        .as_ref()
        .ok_or_else(|| AccountAdminError::Unavailable("the [accounts] section is not configured".to_string()))?;
    if !task_dispatch::valid_account_id(id) {
        return Err(AccountAdminError::NotFound);
    }
    let dir = accounts.claude_dir.join(id);
    if !dir.is_dir() {
        return Err(AccountAdminError::NotFound);
    }
    Ok(dir)
}

/// S2+S8 (ADR-0024 D5): `DELETE /accounts/{id}`。taskd 側で行う（スナップショットではなく、ディスパッチャの
/// 権威ある `account_in_use`（running/reviewing を直接見る）でレースなく判定できるため）。cheap な fs 操作
/// なので `tick_loop` からは spawn せずそのまま呼ぶ。進行中のログインがあれば止め、`.removed/<id>-<unix>` へ
/// 移し、`AccountBook` からもこのアカウントの記録を消す。
pub async fn remove_account(
    config: &Config,
    dispatcher: &mut task_dispatch::Dispatcher,
    sessions: &LoginSessions,
    id: &str,
) -> Result<(), AccountAdminError> {
    let dir = account_dir(config, id)?;
    if dispatcher.account_in_use(id) > 0 {
        return Err(AccountAdminError::InUse);
    }
    if let Some(session) = sessions.lock().await.remove(id) {
        session.cancel();
    }
    dispatcher.set_account_login_pending(id, false);
    // `account_dir` は `[accounts]` の有無を既に確かめている。
    let accounts = config
        .accounts
        .as_ref()
        .ok_or_else(|| AccountAdminError::Unavailable("the [accounts] section is not configured".to_string()))?;
    let removed_dir = accounts.claude_dir.join(".removed");
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
    dispatcher.remove_account_book_entry(id);
    Ok(())
}

/// D6: `POST /accounts/{id}/check`。設定は呼び出しごとに読み直さない（`config` は tick_loop が持つ最新の写し）。
pub fn spawn_check(
    config: &Config,
    id: String,
    events: mpsc::Sender<AccountAdminEvent>,
    reply: oneshot::Sender<Result<AccountCheckOutcome, AccountAdminError>>,
) {
    let dir = match account_dir(config, &id) {
        Ok(dir) => dir,
        Err(e) => {
            let _ = reply.send(Err(e));
            return;
        }
    };
    let check_model = config.accounts.as_ref().map(|a| a.check_model.clone()).unwrap_or_default();
    let (command, env) = claude_command_and_env(config);
    tokio::spawn(async move {
        let check = check_account(&command, &dir, &check_model, CHECK_TIMEOUT, &env).await;
        let result_name = account_check_result_name(&check.result).to_string();
        let outcome = AccountCheckOutcome {
            result: map_check_result(check.result),
            detail: check.detail.clone(),
            observation: check.observation.clone(),
        };
        let _ = events
            .send(AccountAdminEvent::Checked {
                id: id.clone(),
                result: result_name,
                detail: check.detail,
                observation: check.observation,
            })
            .await;
        let _ = reply.send(Ok(outcome));
    });
}

/// D7: `POST /accounts/{id}/login`。既に進行中のセッションがあれば止めてから新しく始める。
pub fn spawn_login_start(
    config: &Config,
    sessions: LoginSessions,
    id: String,
    events: mpsc::Sender<AccountAdminEvent>,
    reply: oneshot::Sender<Result<AccountLoginStartOutcome, AccountAdminError>>,
) {
    let dir = match account_dir(config, &id) {
        Ok(dir) => dir,
        Err(e) => {
            let _ = reply.send(Err(e));
            return;
        }
    };
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
                    .send(AccountAdminEvent::LoginPending { id: id.clone(), pending: true })
                    .await;
                let _ = reply.send(Ok(AccountLoginStartOutcome { url, expires_at_unix }));
            }
            Err(e) => {
                // B2: 古いセッションを止めた後に新しい start_login 自体が失敗したら、login_pending を false に
                // 戻す（そのままだと `sessions` には無いのに GUI には「進行中」が残り続ける）。
                if had_old {
                    let _ = events.send(AccountAdminEvent::LoginPending { id: id.clone(), pending: false }).await;
                }
                // D5: URL・詳細な理由は失敗時もログには出さない（アカウント id と操作名だけ）。
                let _ = reply.send(Err(AccountAdminError::LoginFailed(e.to_string())));
            }
        }
    });
}

/// D7: `POST /accounts/{id}/login/code`。進行中のセッションが無ければ `LoginNotStarted`。
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
        let _ = events.send(AccountAdminEvent::LoginPending { id, pending: false }).await;
        let _ = reply.send(Ok(AccountLoginCodeOutcome {
            ok: result.result == LoginOutcome::Ok,
            detail: result.detail,
        }));
    });
}

/// D7 / 3.35: `DELETE /accounts/{id}/login`。進行中でなければ何もしない（エラーにしない）。
pub fn spawn_login_cancel(
    sessions: LoginSessions,
    id: String,
    events: mpsc::Sender<AccountAdminEvent>,
    reply: oneshot::Sender<Result<(), AccountAdminError>>,
) {
    tokio::spawn(async move {
        let session = sessions.lock().await.remove(&id);
        if let Some(session) = session {
            session.cancel();
            let _ = events.send(AccountAdminEvent::LoginPending { id, pending: false }).await;
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
        spawn_check(&config, "a".to_string(), tx, reply_tx);
        let outcome = reply_rx.await.unwrap().unwrap();
        assert_eq!(outcome.result, task_api::ProviderCheckResult::Ok);
        let event = rx.recv().await.unwrap();
        match event {
            AccountAdminEvent::Checked { id, result, .. } => {
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
        spawn_check(&config, "missing".to_string(), tx, reply_tx);
        assert!(matches!(reply_rx.await.unwrap(), Err(AccountAdminError::NotFound)));
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
        let (tx, mut rx) = mpsc::channel(8);

        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(&config, sessions.clone(), "a".to_string(), tx.clone(), reply_tx);
        let started = reply_rx.await.unwrap().unwrap();
        assert_eq!(started.url, "https://claude.com/cai/oauth/authorize?x=1");
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
        spawn_login_cancel(sessions.clone(), "a".to_string(), tx, reply_tx);
        assert!(reply_rx.await.unwrap().is_ok());
    }

    /// N12: 同じアカウントで 2 回目の `login` を始めると、古いセッション（プロセス）は kill され、
    /// 返る URL は新しいセッションのもの（キャッシュされた古い URL ではない）。
    #[tokio::test]
    async fn login_start_replaces_and_kills_the_old_session_returning_a_fresh_url() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("claude-accounts");
        std::fs::create_dir_all(acct.join("a")).unwrap();
        let command = stub_command(
            tmp.path(),
            r#"count_file="$CLAUDE_SECURESTORAGE_CONFIG_DIR/count"
count=$(( $(cat "$count_file" 2>/dev/null || echo 0) + 1 ))
echo "$count" > "$count_file"
echo $$ > "$CLAUDE_SECURESTORAGE_CONFIG_DIR/pid.$count"
printf "visit: \033]8;;https://claude.com/cai/oauth/authorize?x=$count\007https://claude.com/cai/oauth/authorize?x=$count\033]8;;\007\n"
sleep 30
"#,
        );
        let config = config_with_accounts(acct.clone(), command);
        let sessions = new_sessions();
        let (tx, mut rx) = mpsc::channel(8);

        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(&config, sessions.clone(), "a".to_string(), tx.clone(), reply_tx);
        let first = reply_rx.await.unwrap().unwrap();
        assert_eq!(first.url, "https://claude.com/cai/oauth/authorize?x=1");
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { pending: true, .. })));
        let first_pid: u32 = std::fs::read_to_string(acct.join("a").join("pid.1"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(std::path::Path::new(&format!("/proc/{first_pid}")).exists());

        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(&config, sessions.clone(), "a".to_string(), tx.clone(), reply_tx);
        let second = reply_rx.await.unwrap().unwrap();
        assert_eq!(second.url, "https://claude.com/cai/oauth/authorize?x=2");
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { pending: true, .. })));

        // 古いプロセスは kill されている（プロセスグループごと。N4）。
        for _ in 0..100 {
            if !std::path::Path::new(&format!("/proc/{first_pid}")).exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("old login process {first_pid} is still alive after replacement");
    }

    /// B2: 古いセッションを止めた後、新しい `start_login` 自体が失敗したら `login_pending` を false に戻す
    /// （そのままだと GUI に「進行中」が残り続ける）。
    #[tokio::test]
    async fn login_start_failure_after_replacing_old_session_clears_login_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let acct = tmp.path().join("claude-accounts");
        std::fs::create_dir_all(acct.join("a")).unwrap();
        let command = stub_command(
            tmp.path(),
            r#"count_file="$CLAUDE_SECURESTORAGE_CONFIG_DIR/count"
count=$(( $(cat "$count_file" 2>/dev/null || echo 0) + 1 ))
echo "$count" > "$count_file"
if [ "$count" = "1" ]; then
  printf "visit: \033]8;;https://claude.com/cai/oauth/authorize?x=1\007https://claude.com/cai/oauth/authorize?x=1\033]8;;\007\n"
  sleep 30
else
  exit 1
fi
"#,
        );
        let config = config_with_accounts(acct.clone(), command);
        let sessions = new_sessions();
        let (tx, mut rx) = mpsc::channel(8);

        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(&config, sessions.clone(), "a".to_string(), tx.clone(), reply_tx);
        reply_rx.await.unwrap().unwrap();
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { pending: true, .. })));
        assert!(sessions.lock().await.contains_key("a"));

        // 2 回目は起動そのものが失敗する（stub がすぐ exit 1 する。URL を出さない）。
        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(&config, sessions.clone(), "a".to_string(), tx.clone(), reply_tx);
        assert!(matches!(reply_rx.await.unwrap(), Err(AccountAdminError::LoginFailed(_))));
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { pending: false, .. })));
        assert!(!sessions.lock().await.contains_key("a"));
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
        let (tx, mut rx) = mpsc::channel(8);

        let (reply_tx, reply_rx) = oneshot::channel();
        spawn_login_start(&config, sessions.clone(), "a".to_string(), tx.clone(), reply_tx);
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
        let mut dispatcher = crate::build_dispatcher(&config).unwrap();
        let sessions = new_sessions();
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
        spawn_login_start(&login_config, sessions.clone(), "a".to_string(), tx.clone(), reply_tx);
        reply_rx.await.unwrap().unwrap();
        assert!(matches!(rx.recv().await, Some(AccountAdminEvent::LoginPending { pending: true, .. })));

        let result = remove_account(&config, &mut dispatcher, &sessions, "a").await;
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
        let mut dispatcher = crate::build_dispatcher(&config).unwrap();
        let sessions = new_sessions();
        let result = remove_account(&config, &mut dispatcher, &sessions, "missing").await;
        assert!(matches!(result, Err(AccountAdminError::NotFound)));
    }
}
