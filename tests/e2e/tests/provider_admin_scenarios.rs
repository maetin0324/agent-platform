//! DESIGN §6 Phase 11 の受け入れ 1〜6（ADR-0017: GUI からのアカウント管理）を、実バイナリ `celeris`（`[api]` 有効、
//! `providers_include` 有効）と fake ワーカー（`sh` スクリプト）と `curl` で再現する。接続先は 127.0.0.1 だけで、
//! 外部ネットワークに出ない。
//!
//! 1. `POST /api/v1/providers` が `providers.d/<id>.toml` を作り、`POST /api/v1/reload` の後の tick から新しいアカウントが
//!    使われる。実行中の run は影響を受けない
//! 2. 管理系はトークン無しで 401（loopback でも）。読み取り系は従来どおり
//! 3. `env` の値・`token_file` の中身は、応答にもログにも出ない
//! 4. `POST /api/v1/providers/{id}/check` が 4 種類の結果を返す（fake アダプタで `ok` と `auth_failed` を再現）
//! 5. 重複 id の追加は 409、存在しない id の変更・削除は 404、`reload` で cooldown が消える
//! 6. `cargo test --workspace` と clippy は CI 側（本ファイルはそのテストの 1 つ）。スキーマは別途 `task-api` 側で検証

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug_dir = exe.parent().unwrap().parent().unwrap();
    let path = debug_dir.join(name);
    assert!(
        path.exists(),
        "{} not found; run `cargo test --workspace`",
        path.display()
    );
    path
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

struct Proc {
    child: Child,
    log: PathBuf,
}

impl Proc {
    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Resp {
    status: u16,
    headers: Vec<String>,
    body: String,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {} {}", self.status, self.body))
    }

    #[track_caller]
    fn assert_problem(&self, status: u16, code: &str) -> Value {
        assert_eq!(self.status, status, "{}", self.body);
        let v = self.json();
        assert_eq!(v["code"], code, "{v}");
        assert_eq!(v["status"], status, "{v}");
        v
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    port: u16,
    token: Option<String>,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        Self {
            _tmp: tmp,
            root,
            port: free_port(),
            token: None,
        }
    }

    fn write_script(&self, body: &str) -> PathBuf {
        let path = self.root.join("fake-worker.sh");
        std::fs::write(&path, format!("#!/bin/sh\nset -u\n{body}\n")).unwrap();
        path
    }

    /// `extra` は `[[providers]]`（inline）以外に足す設定本文（`providers_include` 等）。`token` が `Some` なら
    /// `[api] token_file` を書く。
    fn write_config(&self, script: &Path, token: Option<&str>, extra: &str) -> PathBuf {
        let path = self.root.join("config.toml");
        let token_line = if let Some(token) = token {
            std::fs::write(self.root.join("api.token"), token).unwrap();
            "token_file = \"api.token\"\n"
        } else {
            ""
        };
        let text = format!(
            r#"db = "celeris.sqlite3"
workspace_root = "workspaces"
tick_ms = 50
max_concurrency = 4
lease_grace_secs = 60
idle_timeout_secs = 30
kill_grace_secs = 1
review_timeout_secs = 30
retry_backoff_base_secs = 0

{extra}

[api]
listen = "127.0.0.1:{port}"
{token_line}

[adapters.fake]
command = ["sh", "{script}"]
env = {{ ADAPTER_SECRET = "adapter-s3cr3t-value" }}

[[providers]]
id = "acct-a"
adapter = "fake"
tiers = ["frontier", "standard", "cheap"]
concurrency = 1
model = "fake"
"#,
            port = self.port,
            script = script.display(),
        );
        std::fs::write(&path, text).unwrap();
        path
    }

    fn workspace(&self, name: &str) -> String {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn celerisctl(&self, args: &[&str]) -> String {
        let out = Command::new(bin("celerisctl"))
            .arg("--db")
            .arg(self.root.join("celeris.sqlite3"))
            .args(args)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "celerisctl {args:?} failed: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    }

    fn add(&self, args: &[&str]) -> String {
        let mut full = vec!["add", "--objective", "phase 11 provider admin scenario"];
        full.extend_from_slice(args);
        self.celerisctl(&full).trim().to_string()
    }

    fn start_celeris(&self, config: &Path) -> Proc {
        static STARTS: AtomicUsize = AtomicUsize::new(0);
        let log = self.root.join(format!(
            "celeris-{}.log",
            STARTS.fetch_add(1, Ordering::Relaxed)
        ));
        let child = Command::new(bin("celeris"))
            .args(["--config", config.to_str().unwrap(), "--log-format", "text"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        Proc { child, log }
    }

    fn wait_api(&self, daemon: &mut Proc) {
        let ok = wait_until(Duration::from_secs(20), || {
            if let Ok(Some(status)) = daemon.child.try_wait() {
                panic!("celeris exited early with {status}\n{}", daemon.log_text());
            }
            self.request("GET", "/health", None, &[]).status == 200
        });
        assert!(ok, "API did not come up\n{}", daemon.log_text());
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/api/v1{path}", self.port)
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>, headers: &[&str]) -> Resp {
        let mut cmd = Command::new("curl");
        cmd.args([
            "-s",
            "-S",
            "-D",
            "-",
            "--max-time",
            "35",
            "-H",
            "Expect:",
            "-X",
            method,
        ]);
        let has = |name: &str| {
            headers
                .iter()
                .any(|h| h.to_ascii_lowercase().starts_with(&format!("{name}:")))
        };
        if let Some(token) = &self.token
            && !has("authorization")
        {
            cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
        }
        for h in headers {
            cmd.args(["-H", h]);
        }
        if let Some(body) = body {
            if !has("content-type") {
                cmd.args(["-H", "Content-Type: application/json"]);
            }
            cmd.args(["--data-binary", body]);
        }
        let out = cmd.arg(self.url(path)).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            return Resp {
                status: 0,
                headers: vec![],
                body: String::from_utf8_lossy(&out.stderr).into_owned(),
            };
        };
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        Resp {
            status,
            headers: lines.map(str::to_string).collect(),
            body: body.to_string(),
        }
    }

    fn get(&self, path: &str) -> Resp {
        self.request("GET", path, None, &[])
    }

    fn post(&self, path: &str, body: Value) -> Resp {
        self.request("POST", path, Some(&body.to_string()), &[])
    }

    fn post_empty(&self, path: &str) -> Resp {
        self.request("POST", path, Some("{}"), &[])
    }

    fn patch(&self, path: &str, body: Value) -> Resp {
        self.request("PATCH", path, Some(&body.to_string()), &[])
    }

    fn delete(&self, path: &str) -> Resp {
        self.request("DELETE", path, None, &[])
    }

    fn header(&self, resp: &Resp, name: &str) -> Option<String> {
        resp.headers.iter().find_map(|h| {
            let (n, v) = h.split_once(':')?;
            n.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    }

    fn replay_is_consistent(&self) {
        let out = self.celerisctl(&["replay"]);
        assert!(out.contains("replay: 0 mismatches"), "{out}");
    }
}

/// 受け入れ 2: 管理系エンドポイントは `token_file` 未設定（loopback 限定構成）でもトークン無しでは 401。
/// 読み取り系（`GET /providers`）は従来どおりトークン無しで 200。
#[test]
fn admin_endpoints_require_token_even_without_token_file_on_loopback() {
    let env = Env::new();
    let script = env.write_script(
        "cat >/dev/null\necho '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'",
    );
    let config = env.write_config(&script, None, "");
    let mut daemon = env.start_celeris(&config);
    env.wait_api(&mut daemon);

    assert_eq!(env.get("/providers").status, 200);

    env.post("/providers", json!({"id": "x", "adapter": "fake"}))
        .assert_problem(401, "unauthorized");
    env.post_empty("/reload")
        .assert_problem(401, "unauthorized");
    env.post_empty("/providers/x/check")
        .assert_problem(401, "unauthorized");
    env.patch("/providers/x", json!({"concurrency": 2}))
        .assert_problem(401, "unauthorized");
    env.delete("/providers/x")
        .assert_problem(401, "unauthorized");
}

/// 受け入れ 1・3・4・5: `providers.d/` への作成・変更・削除・重複/未知 id のエラー、`check` の `ok`/`auth_failed`、
/// `reload` 後の次 tick からの新アカウント利用（実行中の run は影響を受けない）、`env` の値が応答に出ないこと。
#[test]
fn provider_lifecycle_create_check_patch_delete_and_reload_routes_new_account() {
    let env = Env::new();
    // 新規アカウントの env に `AUTH_FAIL=1` があれば auth_failed、タスクの title が "slow" なら少し待ってから done、
    // それ以外は即 done（`SLOW` は env ではなく stdin の RunRequest.task.title で判定する。env はプロバイダ単位で
    // タスク単位ではないため）。
    let script = env.write_script(
        r#"input=$(cat)
if [ -n "${AUTH_FAIL:-}" ]; then
  echo '{"type":"error","message":"401 unauthorized","retryable":false,"provider_failure":{"kind":"auth_failed"}}'
elif printf '%s' "$input" | grep -q '"title":"slow"'; then
  sleep 8
  echo '{"type":"done","summary":"slow ok","evidence":[]}'
else
  echo '{"type":"done","summary":"ok","evidence":[]}'
fi"#,
    );
    let config = env.write_config(
        &script,
        Some("s3cret-admin-token"),
        "providers_include = \"providers.d/*.toml\"",
    );
    let mut daemon = env.start_celeris(&config);
    let mut env = env;
    env.token = Some("s3cret-admin-token".into());
    env.wait_api(&mut daemon);

    // --- 受け入れ 5: 未知 id の変更・削除は 404 ---
    env.patch("/providers/does-not-exist", json!({"concurrency": 2}))
        .assert_problem(404, "provider_not_found");
    env.delete("/providers/does-not-exist")
        .assert_problem(404, "provider_not_found");

    // --- 作成: acct-c-authfail（env にトークンらしき値を含める。応答にもログにも出てはいけない） ---
    let created = env.post(
        "/providers",
        json!({
            "id": "acct-c-authfail",
            "adapter": "fake",
            "env": {"AUTH_FAIL": "1", "CLAUDE_CONFIG_DIR": "/home/x/.config/claude-super-secret-path"},
        }),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(
        env.header(&created, "location").as_deref(),
        Some("/api/v1/providers/acct-c-authfail")
    );
    let created_json = created.json();
    let mut env_keys = created_json["env_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect::<Vec<_>>();
    env_keys.sort();
    assert_eq!(env_keys, vec!["AUTH_FAIL", "CLAUDE_CONFIG_DIR"]);
    assert!(created_json.get("env").is_none(), "{}", created.body);
    assert!(
        !created.body.contains("claude-super-secret-path"),
        "{}",
        created.body
    );
    assert!(
        std::fs::read_to_string(env.root.join("providers.d/acct-c-authfail.toml"))
            .unwrap()
            .contains("claude-super-secret-path")
    );

    // --- 受け入れ 5: 重複 id は 409 ---
    env.post(
        "/providers",
        json!({"id": "acct-c-authfail", "adapter": "fake"}),
    )
    .assert_problem(409, "provider_exists");

    // --- 受け入れ 4: check は `auth_failed` を返す（タスクにもイベントにも残らない） ---
    let checked = env.post_empty("/providers/acct-c-authfail/check");
    assert_eq!(checked.status, 200, "{}", checked.body);
    assert_eq!(checked.json()["result"], "auth_failed", "{}", checked.body);
    assert!(checked.json()["checked_at"].is_string());

    // --- 変更: concurrency を PATCH ---
    let patched = env.patch("/providers/acct-c-authfail", json!({"concurrency": 5}));
    assert_eq!(patched.status, 200, "{}", patched.body);
    assert_eq!(patched.json()["concurrency"], 5, "{}", patched.body);

    // --- 作成: acct-b（正常。dispatch の overflow 先として使う） ---
    let good = env.post("/providers", json!({"id": "acct-b", "adapter": "fake"}));
    assert_eq!(good.status, 201, "{}", good.body);

    // --- 受け入れ 4: check は `ok` も返す ---
    let checked = env.post_empty("/providers/acct-b/check");
    assert_eq!(checked.status, 200, "{}", checked.body);
    assert_eq!(checked.json()["result"], "ok", "{}", checked.body);

    // reload 前は acct-b がまだ稼働中のプロバイダ一覧に出ない（`providers.d/` に書いただけ）。
    let before = env.get("/providers").json();
    assert!(
        !before["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "acct-b"),
        "acct-b must not be in rotation before reload: {before}"
    );

    // --- 受け入れ 1: acct-a を長時間タスクで埋めてから acct-b を追加・reload し、次の tick から使われることを確かめる。
    // 実行中の run（acct-a 側）は影響を受けない。
    let ws_slow = env.workspace("ws-slow");
    let slow = env.add(&[
        "--title",
        "slow",
        "--check-cmd",
        "true",
        "--workspace",
        &ws_slow,
    ]);
    env.celerisctl(&["approve", &slow]);
    assert!(
        wait_until(Duration::from_secs(10), || {
            env.get("/daemon").json()["snapshot"]["in_flight"]
                .as_array()
                .is_some_and(|v| {
                    v.iter()
                        .any(|r| r["task_id"] == slow && r["provider"] == "acct-a")
                })
        }),
        "slow run never started on acct-a: {}",
        env.get("/daemon").body
    );

    let reloaded = env.post_empty("/reload");
    assert_eq!(reloaded.status, 200, "{}", reloaded.body);
    assert_eq!(reloaded.json()["reloaded"], true);

    let after = env.get("/providers").json();
    let ids: Vec<&str> = after["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"acct-b") && ids.contains(&"acct-c-authfail"),
        "{after}"
    );

    let ws_quick = env.workspace("ws-quick");
    let quick = env.add(&[
        "--title",
        "quick",
        "--check-cmd",
        "true",
        "--workspace",
        &ws_quick,
    ]);
    env.celerisctl(&["approve", &quick]);
    let quick_dispatched_to_b = wait_until(Duration::from_secs(10), || {
        let store = task_core::SqliteStore::open(&env.root.join("celeris.sqlite3")).unwrap();
        task_core::TaskStore::events_for(&store, quick.parse().unwrap())
            .unwrap()
            .iter()
            .any(|(_, e)| matches!(e, task_core::Event::WorkerStarted { provider, .. } if provider.as_deref() == Some("acct-b")))
    });
    if !quick_dispatched_to_b {
        let store = task_core::SqliteStore::open(&env.root.join("celeris.sqlite3")).unwrap();
        let task = task_core::TaskStore::get(&store, quick.parse().unwrap())
            .unwrap()
            .unwrap();
        let events = task_core::TaskStore::events_for(&store, quick.parse().unwrap()).unwrap();
        panic!(
            "quick task never dispatched to acct-b (busy acct-a should overflow to it); status={:?} events={:#?}\ndaemon={}",
            task.status,
            events,
            env.get("/daemon").body
        );
    }

    // acct-a の run はそのまま完了する（reload / 新アカウント追加の影響を受けない）。
    assert!(
        wait_until(Duration::from_secs(20), || {
            let store = task_core::SqliteStore::open(&env.root.join("celeris.sqlite3")).unwrap();
            task_core::TaskStore::get(&store, slow.parse().unwrap())
                .unwrap()
                .unwrap()
                .status
                == task_core::Status::Done
        }),
        "slow task on acct-a never completed"
    );
    {
        let store = task_core::SqliteStore::open(&env.root.join("celeris.sqlite3")).unwrap();
        let events = task_core::TaskStore::events_for(&store, slow.parse().unwrap()).unwrap();
        assert!(events.iter().any(|(_, e)| matches!(e, task_core::Event::WorkerStarted { provider, .. } if provider.as_deref() == Some("acct-a"))));
    }

    // --- 削除 ---
    let deleted = env.delete("/providers/acct-c-authfail");
    assert_eq!(deleted.status, 200, "{}", deleted.body);
    assert!(!std::fs::exists(env.root.join("providers.d/acct-c-authfail.toml")).unwrap());
    env.delete("/providers/acct-c-authfail")
        .assert_problem(404, "provider_not_found");

    // --- 受け入れ 3: env の値も token_file の中身もログに出ない ---
    let log = daemon.log_text();
    assert!(!log.contains("claude-super-secret-path"), "{log}");
    assert!(!log.contains("s3cret-admin-token"), "{log}");

    drop(daemon);
    env.replay_is_consistent();
}

/// 受け入れ 5: `reload` で cooldown が消える（`StaticPolicy` を作り直すため）。
#[test]
fn reload_clears_provider_cooldown() {
    let env = Env::new();
    let script = env.write_script(
        r#"cat >/dev/null
echo '{"type":"error","message":"429 rate limited","retryable":true,"provider_failure":{"kind":"throttled","retry_after_secs":120}}'"#,
    );
    let config = env.write_config(
        &script,
        Some("s3cret-admin-token"),
        "providers_include = \"providers.d/*.toml\"",
    );
    let mut daemon = env.start_celeris(&config);
    let mut env = env;
    env.token = Some("s3cret-admin-token".into());
    env.wait_api(&mut daemon);

    let ws = env.workspace("ws-throttle");
    let task = env.add(&[
        "--title",
        "t",
        "--check-cmd",
        "true",
        "--max-retries",
        "0",
        "--workspace",
        &ws,
    ]);
    env.celerisctl(&["approve", &task]);

    let cooling = wait_until(Duration::from_secs(10), || {
        env.get("/providers").json()["items"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|p| p["id"] == "acct-a" && !p["cooldown"].is_null())
            })
    });
    assert!(
        cooling,
        "acct-a never entered cooldown: {}",
        env.get("/providers").body
    );

    let reloaded = env.post_empty("/reload");
    assert_eq!(reloaded.status, 200, "{}", reloaded.body);

    let cleared = wait_until(Duration::from_secs(5), || {
        env.get("/providers").json()["items"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|p| p["id"] == "acct-a" && p["cooldown"].is_null())
            })
    });
    assert!(
        cleared,
        "cooldown was not cleared by reload: {}",
        env.get("/providers").body
    );

    drop(daemon);
    env.replay_is_consistent();
}
