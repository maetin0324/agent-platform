//! ADR-0024（Phase 13）: Claude アカウントのプール。`check`/`login`/`login/code`/`DELETE .../login` は
//! taskd 側の実行（`AdminRequest` 経由）が要るので、ここでは task-api だけで完結する部分だけを確認する:
//! 認証ガード（管理系はすべて token 必須）、ディレクトリ操作（作成・削除）、`GET /accounts` のマージ
//! （ディレクトリのスキャン + スナップショット + 集計）、プロバイダの `account_pool` の往復。
//! 実際の確認・ログイン中継は `tests/e2e/tests/account_pool_scenarios.rs` で実バイナリを使って検証する。

mod common;

use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};

use common::*;
use serde_json::json;
use task_api::{AccountAdminError, AdminRequest};
use task_core::{Event, Status, TaskKind};
use task_ops::daemon::{AccountCooldownLive, AccountLive, AccountUsageLive};
use tokio::sync::mpsc;

fn auth() -> String {
    format!("Bearer {TOKEN}")
}

/// `[accounts] claude_dir` に使う一時ディレクトリを持つ環境。
fn env_with_accounts_root() -> (TestEnv, tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("claude-accounts");
    std::fs::create_dir_all(&root).unwrap();
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        accounts_root: Some(root.clone()),
        max_runs_per_account: 2,
        ..Default::default()
    });
    (env, tmp, root)
}

/// S2+S8: `DELETE /accounts/{id}` は taskd 側（`AdminRequest::AccountRemove`）へ委譲される。ここでは実際の
/// taskd の代わりに、同じ fs 操作（`.removed/<id>-<unix>` への move）と `in_use` の判定を行うダブルを立てる
/// （taskd 側の本物の実装とその単体テストは `crates/taskd/src/accounts_admin.rs`）。`in_use` はこのダブルが
/// 見る集合で、ディスパッチャの `account_in_use` の代わり。
fn spawn_account_remove_double(root: std::path::PathBuf, in_use: Arc<StdMutex<HashSet<String>>>) -> mpsc::Sender<AdminRequest> {
    let (tx, mut rx) = mpsc::channel::<AdminRequest>(8);
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            if let AdminRequest::AccountRemove { id, reply } = req {
                let result: Result<(), AccountAdminError> = (|| {
                    let dir = root.join(&id);
                    if !dir.is_dir() {
                        return Err(AccountAdminError::NotFound);
                    }
                    if in_use.lock().unwrap_or_else(|e| e.into_inner()).contains(&id) {
                        return Err(AccountAdminError::InUse);
                    }
                    let removed_dir = root.join(".removed");
                    std::fs::create_dir_all(&removed_dir).map_err(|e| AccountAdminError::Unavailable(e.to_string()))?;
                    let dest = removed_dir.join(format!("{id}-{}", time::OffsetDateTime::now_utc().unix_timestamp()));
                    std::fs::rename(&dir, &dest).map_err(|e| AccountAdminError::Unavailable(e.to_string()))?;
                    Ok(())
                })();
                let _ = reply.send(result);
            }
        }
    });
    tx
}

/// `env_with_accounts_root` に `spawn_account_remove_double` を配線したもの。
fn env_with_accounts_root_and_remove_double() -> (TestEnv, tempfile::TempDir, std::path::PathBuf, Arc<StdMutex<HashSet<String>>>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("claude-accounts");
    std::fs::create_dir_all(&root).unwrap();
    let in_use = Arc::new(StdMutex::new(HashSet::new()));
    let admin_tx = spawn_account_remove_double(root.clone(), in_use.clone());
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        accounts_root: Some(root.clone()),
        max_runs_per_account: 2,
        admin_tx: Some(admin_tx),
        ..Default::default()
    });
    (env, tmp, root, in_use)
}

/// 管理系エンドポイントは `token_file` が無くても常に 401（ADR-0017 M3 / api.md §3.29）。`GET /accounts` は
/// 読み取りなので、token を設定していない（loopback 限定）構成でも 200 になる。
#[tokio::test]
async fn management_routes_all_require_a_token() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("claude-accounts");
    std::fs::create_dir_all(&root).unwrap();
    let env = TestEnv::with(EnvOptions {
        token: None,
        accounts_root: Some(root),
        max_runs_per_account: 2,
        ..Default::default()
    });
    let app = env.router();

    let resp = send(&app, post_json("/api/v1/accounts", &json!({"id": "a"}))).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, delete_with("/api/v1/accounts/a", &[])).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, post_json("/api/v1/accounts/a/check", &json!({}))).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, post_json("/api/v1/accounts/a/login", &json!({}))).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, delete_with("/api/v1/accounts/a/login", &[])).await;
    assert_problem(&resp, 401, "unauthorized");

    let resp = send(&app, post_json("/api/v1/accounts/a/login/code", &json!({"code": "x"}))).await;
    assert_problem(&resp, 401, "unauthorized");

    // GET /accounts は読み取りなので token 不要（他のトークン無しの読み取りエンドポイントと同じ扱い）。
    let resp = send(&app, get("/api/v1/accounts")).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
}

/// `[accounts]` 無しの構成では `GET /accounts` は `{root: null, items: []}`、管理系は 409。
#[tokio::test]
async fn no_accounts_section_yields_null_root_and_unavailable_management() {
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        ..Default::default()
    });
    let app = env.router();
    let auth = auth();

    // GET は認証ありでも普通に読める（他の読み取りエンドポイントと同じ）。
    let resp = send(&app, get_with("/api/v1/accounts", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let v = resp.json();
    assert_eq!(v["root"], json!(null));
    assert_eq!(v["items"], json!([]));

    let resp = send(&app, post_json_with("/api/v1/accounts", &json!({"id": "a"}), &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "accounts_unavailable");

    let resp = send(&app, delete_with("/api/v1/accounts/a", &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "accounts_unavailable");

    let resp = send(&app, post_json_with("/api/v1/accounts/a/check", &json!({}), &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "accounts_unavailable");
}

/// `POST /accounts` はディレクトリを作る。既にあれば 409 `account_exists`。不正な id は 400。
#[tokio::test]
async fn create_account_makes_a_directory_and_rejects_duplicates_and_bad_ids() {
    let (env, _tmp, root) = env_with_accounts_root();
    let app = env.router();
    let auth = auth();

    let resp = send(&app, post_json_with("/api/v1/accounts", &json!({"id": "../escape"}), &[("authorization", &auth)])).await;
    assert_problem(&resp, 400, "bad_request");

    let resp = send(&app, post_json_with("/api/v1/accounts", &json!({"id": "acct-b"}), &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    assert_eq!(resp.header("location"), Some("/api/v1/accounts/acct-b"));
    let v = resp.json();
    assert_eq!(v["id"], json!("acct-b"));
    assert_eq!(v["logged_in"], json!(false));
    assert_eq!(v["in_use"], json!(0));
    assert!(root.join("acct-b").is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join("acct-b")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    let resp = send(&app, post_json_with("/api/v1/accounts", &json!({"id": "acct-b"}), &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "account_exists");
}

/// `DELETE /accounts/{id}` は taskd 側（`AdminRequest::AccountRemove`）へ委譲され、ディレクトリを
/// `.removed/<id>-<unix>` へ移す（S2+S8）。無ければ 404。`admin_tx` が無い（taskd に届かない）構成は
/// 409 `accounts_unavailable`。
#[tokio::test]
async fn delete_account_moves_the_directory_to_removed_and_missing_is_404() {
    let (env, _tmp, root, _in_use) = env_with_accounts_root_and_remove_double();
    let app = env.router();
    let auth = auth();
    std::fs::create_dir_all(root.join("acct-b")).unwrap();
    std::fs::write(root.join("acct-b").join(".credentials.json"), "{}").unwrap();

    let resp = send(&app, delete_with("/api/v1/accounts/does-not-exist", &[("authorization", &auth)])).await;
    assert_problem(&resp, 404, "account_not_found");

    let resp = send(&app, delete_with("/api/v1/accounts/acct-b", &[("authorization", &auth)])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert!(!root.join("acct-b").exists());
    let removed_entries: Vec<_> = std::fs::read_dir(root.join(".removed")).unwrap().collect();
    assert_eq!(removed_entries.len(), 1);
    let moved = removed_entries.into_iter().next().unwrap().unwrap().path();
    assert!(moved.file_name().unwrap().to_string_lossy().starts_with("acct-b-"));
    assert!(moved.join(".credentials.json").is_file(), "credentials are not deleted, just moved");
}

/// `[accounts]` はあるが `admin_tx`（taskd への経路）が無い構成は 409 `accounts_unavailable`（taskd 側で
/// 実行するしかない操作なので、届かなければ「使えない」）。
#[tokio::test]
async fn delete_account_without_admin_tx_is_accounts_unavailable() {
    let (env, _tmp, root) = env_with_accounts_root();
    let app = env.router();
    let auth = auth();
    std::fs::create_dir_all(root.join("acct-b")).unwrap();

    let resp = send(&app, delete_with("/api/v1/accounts/acct-b", &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "accounts_unavailable");
    assert!(root.join("acct-b").exists());
}

/// `in_use > 0`（taskd 側のディスパッチャの権威ある値）のアカウントは削除できない（S2+S8）。
#[tokio::test]
async fn delete_account_in_use_is_conflict() {
    let (env, _tmp, root, in_use) = env_with_accounts_root_and_remove_double();
    let app = env.router();
    let auth = auth();
    std::fs::create_dir_all(root.join("acct-b")).unwrap();
    in_use.lock().unwrap().insert("acct-b".to_string());

    let resp = send(&app, delete_with("/api/v1/accounts/acct-b", &[("authorization", &auth)])).await;
    assert_problem(&resp, 409, "account_in_use");
    assert!(root.join("acct-b").exists());
}

/// `GET /accounts` はディレクトリのスキャン（`logged_in`/`dir`）とスナップショットの観測値（`score`/`cooldown`/
/// `usage`/`login_pending`）、`WorkerStarted.account` からの集計を合わせる。
#[tokio::test]
async fn list_accounts_merges_filesystem_snapshot_and_stats() {
    let (env, _tmp, root) = env_with_accounts_root();
    let app = env.router();

    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::write(root.join("b").join(".credentials.json"), "{}").unwrap();

    // b は WorkerStarted/WorkerFinished を通じて集計に現れる。
    let task = new_task(TaskKind::Execute, Status::Done);
    env.seed_with(
        &task,
        vec![
            Event::WorkerStarted {
                run_id: "r1".into(),
                adapter: "claude-code".into(),
                model: "m".into(),
                provider: Some("pool".into()),
                account: Some("b".into()),
                role: None,
                task_role: None,
            },
            Event::WorkerFinished {
                run_id: "r1".into(),
                outcome: "done: ok".into(),
                usage: Some(task_core::Usage { input_tokens: Some(10), output_tokens: Some(4) }),
                role: None,
            },
        ],
    );

    let mut snap = snapshot(1);
    snap.accounts_root = Some(root.display().to_string());
    snap.max_runs_per_account = Some(2);
    snap.accounts = vec![AccountLive {
        id: "b".into(),
        logged_in: true,
        in_use: 1,
        usage: Some(AccountUsageLive {
            five_hour: Some(task_core::RateWindow { utilization: 0.2, resets_at: 2_000_000_000 }),
            seven_day: None,
            status: Some("allowed".into()),
            observed_at: 1_900_000_000,
            source: "run".into(),
        }),
        score: Some(0.8),
        excluded_reason: None,
        cooldown: Some(AccountCooldownLive { until: 1_900_000_100, reason: "throttled".into() }),
        last_check: None,
        login_pending: true,
    }];
    env.daemon_tx.send(Some(snap)).unwrap();

    let resp = send(&app, get_with("/api/v1/accounts", &[("authorization", &auth())])).await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    let v = resp.json();
    assert_eq!(v["root"], json!(root.display().to_string()));
    assert_eq!(v["max_runs_per_account"], json!(2));
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{items:?}");

    let a = items.iter().find(|it| it["id"] == "a").unwrap();
    assert_eq!(a["logged_in"], json!(false));
    assert_eq!(a["in_use"], json!(0));
    assert_eq!(a["usage"], json!(null));
    assert_eq!(a["login_pending"], json!(false));

    let b = items.iter().find(|it| it["id"] == "b").unwrap();
    assert_eq!(b["logged_in"], json!(true));
    assert_eq!(b["in_use"], json!(1));
    assert_eq!(b["score"], json!(0.8));
    assert_eq!(b["login_pending"], json!(true));
    assert_eq!(b["usage"]["five_hour"]["utilization"], json!(0.2));
    assert_eq!(b["usage"]["source"], json!("run"));
    assert_eq!(b["cooldown"]["reason"], json!("throttled"));
    assert_eq!(b["stats"]["runs"], json!(1));
    assert_eq!(b["stats"]["done"], json!(1));
    assert_eq!(b["stats"]["input_tokens"], json!(10));
    assert_eq!(b["stats"]["output_tokens"], json!(4));
}

/// S6: taskd 側が `AccountAdminError::Unavailable` を返したら 409 `accounts_unavailable`（500 `internal` では
/// ない）。`taskd` が `[accounts]` 消滅などの理由でその場で断ったケースを模す。
#[tokio::test]
async fn account_admin_unavailable_error_maps_to_409_not_500() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("claude-accounts");
    std::fs::create_dir_all(root.join("a")).unwrap();
    // taskd 側のダブル: 何を要求されても Unavailable で応える。
    let (admin_tx, mut admin_rx) = mpsc::channel::<AdminRequest>(4);
    tokio::spawn(async move {
        while let Some(req) = admin_rx.recv().await {
            if let AdminRequest::AccountCheck { reply, .. } = req {
                let _ = reply.send(Err(AccountAdminError::Unavailable("taskd could not do this right now".into())));
            }
        }
    });
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        accounts_root: Some(root),
        max_runs_per_account: 2,
        admin_tx: Some(admin_tx),
        ..Default::default()
    });
    let app = env.router();

    let resp = send(&app, post_json_with("/api/v1/accounts/a/check", &json!({}), &[("authorization", &auth())])).await;
    assert_problem(&resp, 409, "accounts_unavailable");
}

/// `POST /providers` / `PATCH /providers/{id}` は `account_pool` を受け取り、`providers.d/<id>.toml` に
/// 往復する。`account_pool = true` は claude-code だけ（それ以外は 422 `invalid_provider`）。`[accounts]` が
/// 設定されているので、claude-code + account_pool の組み合わせは通る（S1 は別テストで確認する）。
#[tokio::test]
async fn provider_admin_round_trips_account_pool_and_validates_the_adapter() {
    let providers_tmp = tempfile::tempdir().expect("tempdir");
    let dir = providers_tmp.path().join("providers.d");
    std::fs::create_dir_all(&dir).unwrap();
    let accounts_root = providers_tmp.path().join("claude-accounts");
    std::fs::create_dir_all(&accounts_root).unwrap();
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        providers_dir: Some(dir.clone()),
        accounts_root: Some(accounts_root),
        max_runs_per_account: 2,
        ..Default::default()
    });
    let app = env.router();
    let auth = auth();

    // account_pool = true と adapter = "fake" の組み合わせは 422。
    let resp = send(
        &app,
        post_json_with(
            "/api/v1/providers",
            &json!({"id": "pool", "adapter": "fake", "account_pool": true}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");

    // claude-code なら通り、応答と providers.d のファイルの両方に account_pool が残る。
    let resp = send(
        &app,
        post_json_with(
            "/api/v1/providers",
            &json!({"id": "pool", "adapter": "claude-code", "account_pool": true}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    assert_eq!(resp.json()["account_pool"], json!(true));
    let text = std::fs::read_to_string(dir.join("pool.toml")).unwrap();
    assert!(text.contains("account_pool = true"), "{text}");

    // PATCH で false に戻せる。
    let resp = send(
        &app,
        patch_json_with("/api/v1/providers/pool", &json!({"account_pool": false}), &[("authorization", &auth)]),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.json()["account_pool"], json!(false));

    // PATCH で adapter と矛盾する組み合わせに戻すことはできない（adapter は変更不可なので、
    // account_pool だけを true に戻すと claude-code のままなので通る。fake のプロバイダで確かめる）。
    let resp = send(
        &app,
        post_json_with("/api/v1/providers", &json!({"id": "plain", "adapter": "fake"}), &[("authorization", &auth)]),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let resp = send(
        &app,
        patch_json_with("/api/v1/providers/plain", &json!({"account_pool": true}), &[("authorization", &auth)]),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");
}

/// S1: `account_pool = true` は `adapter = "claude-code"` に加えて `[accounts]` が設定されていることも要る。
/// `[accounts]` が無い構成では `POST`/`PATCH /providers` のどちらも 422 `invalid_provider`（`reload` を待たず
/// 作成・変更の時点で拒否する）。
#[tokio::test]
async fn provider_admin_account_pool_requires_the_accounts_section() {
    let providers_tmp = tempfile::tempdir().expect("tempdir");
    let dir = providers_tmp.path().join("providers.d");
    std::fs::create_dir_all(&dir).unwrap();
    let env = TestEnv::with(EnvOptions {
        token: Some(TOKEN.into()),
        providers_dir: Some(dir.clone()),
        // accounts_root: None（既定）→ `[accounts]` 未設定の構成。
        ..Default::default()
    });
    let app = env.router();
    let auth = auth();

    let resp = send(
        &app,
        post_json_with(
            "/api/v1/providers",
            &json!({"id": "pool", "adapter": "claude-code", "account_pool": true}),
            &[("authorization", &auth)],
        ),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");
    assert!(!dir.join("pool.toml").exists());

    // 既存のプロバイダを PATCH で account_pool = true にする場合も同様に拒否する。
    let resp = send(
        &app,
        post_json_with("/api/v1/providers", &json!({"id": "plain", "adapter": "claude-code"}), &[("authorization", &auth)]),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", resp.text());
    let resp = send(
        &app,
        patch_json_with("/api/v1/providers/plain", &json!({"account_pool": true}), &[("authorization", &auth)]),
    )
    .await;
    assert_problem(&resp, 422, "invalid_provider");
}
