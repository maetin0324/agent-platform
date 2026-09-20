//! ADR-0024（Phase 13）: Claude アカウントのプールを、実バイナリ `celeris`（`[api]` 有効）とスタブの
//! `claude` コマンド（`sh` スクリプト）で再現する。接続先は 127.0.0.1 だけで、外部ネットワークに出ない。
//!
//! 受け入れ条件 1〜4（ADR-0024）:
//! 1. 観測値の異なる 2 アカウントがあれば、スコアの高い方に run が割り当てられ、`WorkerStarted.account` と
//!    ワーカーの `CLAUDE_SECURESTORAGE_CONFIG_DIR` が一致する
//! 2. 片方が throttled で終わると、そのアカウントだけが cooldown になり、次の run はもう片方に行く。
//!    プロバイダは cooldown にならない（`docs/gui/api.md` の cooldown 一覧が空のまま）
//! 3. `rate_limit_event` が run の途中で `AccountBook` と `GET /accounts` に反映され、celeris を再起動しても残る
//! 4. `POST /accounts` → `login` → `login/code` → `logged_in: true` がスタブの `claude auth login` で通る。
//!    管理系はトークン無しで 401

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

#[allow(dead_code)]
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
        v
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    accounts_dir: PathBuf,
    port: u16,
    token: Option<String>,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let accounts_dir = root.join("claude-accounts");
        Self {
            _tmp: tmp,
            root,
            accounts_dir,
            port: free_port(),
            token: None,
        }
    }

    /// スタブの `claude`。`-p ...`（run/check の両方）では `$CLAUDE_SECURESTORAGE_CONFIG_DIR/util`
    /// （無ければ 0）の値で `rate_limit_event` を出し、`artifacts/result.json` を書いて `done` で終わる。
    /// `auth login` では OSC 8 の URL を出し、標準入力からコードを読む（"good-code" だけ成功）。
    fn write_claude_stub(&self) -> PathBuf {
        let path = self.root.join("claude-stub.sh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
set -u
if [ "${1:-}" = "auth" ] && [ "${2:-}" = "login" ]; then
  printf 'Opening browser to sign in...\n'
  printf "If the browser didn't open, visit: \033]8;;https://claude.com/cai/oauth/authorize?x=1\007https://claude.com/cai/oauth/authorize?x=1\033]8;;\007\n"
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
fi

util=0
if [ -n "${CLAUDE_SECURESTORAGE_CONFIG_DIR:-}" ] && [ -f "${CLAUDE_SECURESTORAGE_CONFIG_DIR}/util" ]; then
  util=$(cat "${CLAUDE_SECURESTORAGE_CONFIG_DIR}/util")
fi
resets=$(( $(date +%s) + 90000 ))
printf '{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":%s,"unifiedWindows":{"five_hour":{"utilization":%s,"resetsAt":%s}}}}\n' "$resets" "$util" "$resets"
mkdir -p artifacts
printf '%s' '{"type":"done","summary":"ok","evidence":[]}' > artifacts/result.json
printf '{"type":"result","subtype":"success","is_error":false,"result":"ok"}\n'
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn account(&self, id: &str, util: Option<f64>) {
        let dir = self.accounts_dir.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".credentials.json"), "{}").unwrap();
        if let Some(util) = util {
            std::fs::write(dir.join("util"), util.to_string()).unwrap();
        }
    }

    fn write_config(&self, claude: &Path, token: Option<&str>, extra_provider: &str) -> PathBuf {
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

[accounts]
claude_dir = "claude-accounts"
max_runs_per_account = 2
check_model = "haiku"

[api]
listen = "127.0.0.1:{port}"
{token_line}

[adapters.claude_code]
command = "{claude}"

[[providers]]
id = "pool"
adapter = "claude-code"
tiers = ["frontier", "standard", "cheap"]
concurrency = 1
account_pool = true
{extra_provider}
"#,
            port = self.port,
            claude = claude.display(),
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

    fn add(&self, title: &str) -> String {
        let ws = self.workspace(&format!("ws-{title}"));
        self.celerisctl(&[
            "add",
            "--title",
            title,
            "--objective",
            "phase 13 account pool scenario",
            "--check-cmd",
            "true",
            "--workspace",
            &ws,
        ])
        .trim()
        .to_string()
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
        // `GET /accounts` の観測値（`usage` / `score` / `in_use`）は**ディスパッチャのスナップショット**
        // 由来なので、API が上がっただけでは空になりうる（最初の tick より前は `snapshot: null`）。
        // 起動にかかる時間は環境で動く（Phase 56 で起動時のコンテナ runtime 検出が入った）ため、
        // 「最初の tick がスナップショットを出すまで」を待ちに含める。
        // 認証が要る設定では、この時点でまだトークンを持っていないことがある（そのときは待たない）。
        let ticked = wait_until(Duration::from_secs(20), || {
            let resp = self.request("GET", "/daemon", None, &[]);
            resp.status != 200 || resp.json()["snapshot"].is_object()
        });
        assert!(
            ticked,
            "the dispatcher never published a snapshot\n{}",
            daemon.log_text()
        );
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

    fn delete(&self, path: &str) -> Resp {
        self.request("DELETE", path, None, &[])
    }

    fn replay_is_consistent(&self) {
        let out = self.celerisctl(&["replay"]);
        assert!(out.contains("replay: 0 mismatches"), "{out}");
    }

    fn worker_started_account(&self, task_id: &str) -> Option<String> {
        let store = task_core::SqliteStore::open(&self.root.join("celeris.sqlite3")).unwrap();
        task_core::TaskStore::events_for(&store, task_id.parse().unwrap())
            .unwrap()
            .into_iter()
            .find_map(|(_, e)| match e {
                task_core::Event::WorkerStarted { account, .. } => account,
                _ => None,
            })
    }

    fn task_status(&self, task_id: &str) -> task_core::Status {
        let store = task_core::SqliteStore::open(&self.root.join("celeris.sqlite3")).unwrap();
        task_core::TaskStore::get(&store, task_id.parse().unwrap())
            .unwrap()
            .unwrap()
            .status
    }
}

/// 受け入れ 1・2・3: 残量の高い方が選ばれ、`CLAUDE_SECURESTORAGE_CONFIG_DIR` が一致する。throttled は
/// そのアカウントだけを cooldown にする（プロバイダは cooldown にならない）。観測値は再起動後も残る。
#[test]
fn account_selection_follows_headroom_and_survives_restart_and_throttle_only_cools_the_account() {
    let env = Env::new();
    let claude = env.write_claude_stub();
    // a は残量が少ない（util 0.9）、b は多い（util 0.2）。
    env.account("a", Some(0.9));
    env.account("b", Some(0.2));
    let config = env.write_config(&claude, None, "");
    let mut daemon = env.start_celeris(&config);
    let env_ref = &env;
    env_ref.wait_api(&mut daemon);

    // 1 回目は観測値が無いので id 昇順のタイブレークで "a" に行く。
    let t1 = env.add("t1");
    env.celerisctl(&["approve", &t1]);
    assert!(
        wait_until(Duration::from_secs(10), || env.task_status(&t1)
            == task_core::Status::Done),
        "t1 never completed"
    );
    assert_eq!(env.worker_started_account(&t1).as_deref(), Some("a"));

    // "a" は util 0.9 の観測値がついた。"b" はまだ観測値が無い（score 1.0）ので次はそちらへ行く。
    let t2 = env.add("t2");
    env.celerisctl(&["approve", &t2]);
    assert!(
        wait_until(Duration::from_secs(10), || env.task_status(&t2)
            == task_core::Status::Done),
        "t2 never completed"
    );
    assert_eq!(env.worker_started_account(&t2).as_deref(), Some("b"));

    // どちらも観測値がついた今、以後は残量の多い "b" に行き続ける。
    let t3 = env.add("t3");
    env.celerisctl(&["approve", &t3]);
    assert!(
        wait_until(Duration::from_secs(10), || env.task_status(&t3)
            == task_core::Status::Done),
        "t3 never completed"
    );
    assert_eq!(env.worker_started_account(&t3).as_deref(), Some("b"));

    // GET /accounts に反映されている（受け入れ 3 前半）。
    let accounts = env.get("/accounts").json();
    let items = accounts["items"].as_array().unwrap();
    let a = items.iter().find(|it| it["id"] == "a").unwrap();
    assert_eq!(a["usage"]["five_hour"]["utilization"], json!(0.9));
    let b = items.iter().find(|it| it["id"] == "b").unwrap();
    assert_eq!(b["usage"]["five_hour"]["utilization"], json!(0.2));

    // プールを使う run の cooldown はプロバイダには付かない（daemon.cooldowns は空のまま）。
    assert_eq!(
        env.get("/daemon").json()["snapshot"]["cooldowns"],
        json!([])
    );

    // 再起動しても観測値は残る（受け入れ 3 後半、AccountBook の永続化）。
    drop(daemon);
    let mut daemon = env.start_celeris(&config);
    env.wait_api(&mut daemon);
    let accounts_after_restart = env.get("/accounts").json();
    let items = accounts_after_restart["items"].as_array().unwrap();
    let a = items.iter().find(|it| it["id"] == "a").unwrap();
    assert_eq!(a["usage"]["five_hour"]["utilization"], json!(0.9));

    env.replay_is_consistent();
}

/// 受け入れ 2: throttled で終わったアカウントだけが cooldown になり、次の run はもう片方に行く。
#[test]
fn throttled_account_cools_down_alone_and_the_next_run_uses_the_other_account() {
    let env = Env::new();
    let claude = env.write_claude_stub();
    env.account("a", None);
    env.account("b", None);
    // "a" だけを throttled にする: `claude-conditional.sh` はそのアカウントのディレクトリに `throttle-me` が
    // あれば throttled のエラーを返し、無ければスタブへそのまま委譲する。id 昇順のタイブレークで "a" が最初に
    // 選ばれることを利用する。
    std::fs::write(env.accounts_dir.join("a").join("throttle-me"), "1").unwrap();
    let claude_conditional = env.root.join("claude-conditional.sh");
    std::fs::write(
        &claude_conditional,
        format!(
            r#"#!/bin/sh
set -u
if [ -f "${{CLAUDE_SECURESTORAGE_CONFIG_DIR:-/nonexistent}}/throttle-me" ]; then
  echo '{{"type":"result","subtype":"success","is_error":true,"result":"API Error: 429 rate limit exceeded"}}'
  exit 0
fi
exec "{claude}" "$@"
"#,
            claude = claude.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude_conditional, std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let config = env.write_config(&claude_conditional, None, "");
    let mut daemon = env.start_celeris(&config);
    env.wait_api(&mut daemon);

    let t1 = env.add("throttle1");
    env.celerisctl(&["approve", &t1]);
    assert!(
        wait_until(Duration::from_secs(10), || env
            .worker_started_account(&t1)
            .is_some()),
        "t1 was never dispatched"
    );
    assert_eq!(env.worker_started_account(&t1).as_deref(), Some("a"));

    // "a" が cooldown に入るまで待つ（GET /accounts の cooldown が埋まる）。
    let cooling = wait_until(Duration::from_secs(10), || {
        env.get("/accounts").json()["items"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|it| it["id"] == "a" && !it["cooldown"].is_null())
            })
    });
    assert!(
        cooling,
        "account a never cooled down: {}",
        env.get("/accounts").body
    );
    // プロバイダ自体は cooldown にならない。
    assert_eq!(
        env.get("/daemon").json()["snapshot"]["cooldowns"],
        json!([])
    );

    // 次のタスクは cooldown 中の "a" を避けて "b" に行く。
    let t2 = env.add("throttle2");
    env.celerisctl(&["approve", &t2]);
    assert!(
        wait_until(Duration::from_secs(10), || env
            .worker_started_account(&t2)
            .is_some()),
        "t2 was never dispatched"
    );
    assert_eq!(env.worker_started_account(&t2).as_deref(), Some("b"));

    drop(daemon);
}

/// 受け入れ 4: `POST /accounts` → `login` → `login/code` → `logged_in: true`。管理系はトークン無しで 401。
#[test]
fn http_login_flow_ends_with_logged_in_true_and_management_requires_a_token() {
    // token_file 未設定（loopback 限定構成）: 読み取りは通るが、管理系はトークン無しでは 401（ADR-0017 M3）。
    {
        let env = Env::new();
        let claude = env.write_claude_stub();
        let config = env.write_config(&claude, None, "");
        let mut daemon = env.start_celeris(&config);
        env.wait_api(&mut daemon);

        assert_eq!(env.get("/accounts").status, 200);
        env.post("/accounts", json!({"id": "c"}))
            .assert_problem(401, "unauthorized");
    }

    // token_file 設定あり: 認証を通した後、POST /accounts → login → login/code → logged_in: true の一連が通る。
    let env = Env::new();
    let claude = env.write_claude_stub();
    let config = env.write_config(&claude, Some("s3cret-admin-token"), "");
    let mut daemon = env.start_celeris(&config);
    let mut env = env;
    env.wait_api(&mut daemon);
    env.token = Some("s3cret-admin-token".into());

    let created = env.post("/accounts", json!({"id": "c"}));
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(created.json()["logged_in"], json!(false));

    let started = env.post_empty("/accounts/c/login");
    assert_eq!(started.status, 200, "{}", started.body);
    let url = started.json()["url"].as_str().unwrap().to_string();
    assert_eq!(url, "https://claude.com/cai/oauth/authorize?x=1");
    assert!(started.json()["expires_at"].is_string());

    let coded = env.post("/accounts/c/login/code", json!({"code": "good-code"}));
    assert_eq!(coded.status, 200, "{}", coded.body);
    assert_eq!(coded.json()["result"], "ok", "{}", coded.body);

    let listed = env.get("/accounts").json();
    let c = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| it["id"] == "c")
        .unwrap();
    assert_eq!(c["logged_in"], json!(true));

    // ログもコードも URL も出ない（D5）。
    let log = daemon.log_text();
    assert!(!log.contains("good-code"), "{log}");
    assert!(!log.contains("oauth/authorize"), "{log}");

    // 後片付け: DELETE は .removed へ移す。
    let deleted = env.delete("/accounts/c");
    assert_eq!(deleted.status, 200, "{}", deleted.body);
    assert!(!env.accounts_dir.join("c").exists());

    drop(daemon);
    env.replay_is_consistent();
}
