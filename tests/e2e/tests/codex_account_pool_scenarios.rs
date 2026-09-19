//! ADR-0025（Phase 14）: codex アカウントのプールを、実バイナリ `taskd`（`[api]` 有効）とスタブの `codex`
//! コマンド（`sh` スクリプト）で再現する。`tests/e2e/tests/account_pool_scenarios.rs`（claude-code, ADR-0024）
//! の codex 版。接続先は 127.0.0.1 だけで、外部ネットワークに出ない。
//!
//! 受け入れ条件（ADR-0025）:
//! 1. `[accounts] codex_dir` と `adapter = "codex"` の `account_pool` プロバイダで、残量の多い codex アカウントが
//!    選ばれ、`CODEX_HOME` がその値になる
//! 2. codex の `token_count` の `rate_limits` が観測値になり、`GET /accounts` に出る（短い枠/長い枠が
//!    `window_minutes` に従う）
//! 3. `POST /accounts {"id":…,"adapter":"codex"}` → `login` が `kind: "device_code"` と `user_code` を返し、
//!    スタブの `codex login --device-auth` の完了後に `logged_in: true` になる。`login/code` は 409

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
    assert!(path.exists(), "{} not found; run `cargo test --workspace`", path.display());
    path
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
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
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {} {}", self.status, self.body))
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
        let accounts_dir = root.join("codex-accounts");
        Self { _tmp: tmp, root, accounts_dir, port: free_port(), token: None }
    }

    /// スタブの `codex`。`exec --json --skip-git-repo-check "..."`（check）と `exec --json <prompt>`（run）の
    /// どちらも `$CODEX_HOME/util`（無ければ 0）の値で `token_count` の `rate_limits` を出す。`login --device-auth`
    /// は URL と一回限りのコードを出し、標準入力は読まずに完了後 `auth.json` を書いて exit 0 する。
    fn write_codex_stub(&self) -> PathBuf {
        let path = self.root.join("codex-stub.sh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
set -u
if [ "${1:-}" = "login" ] && [ "${2:-}" = "--device-auth" ]; then
  # 実機（codex-cli 0.154.0）と同じ形: バナー（`command-line` を含む）、URL の行、コードは単独の行。
  printf '\n  Welcome to Codex [v0.154.0]\n'
  printf "  OpenAI's command-line coding agent\n\n"
  printf '1. Open this link in your browser and sign in to your account\n'
  printf '   https://auth.openai.com/codex/device\n\n'
  printf '2. Enter this one-time code (expires in 15 minutes)\n'
  printf '   ABCD-EFGHI\n\n'
  sleep 0.2
  printf '%s' '{}' > "$CODEX_HOME/auth.json"
  exit 0
fi

util=0
if [ -n "${CODEX_HOME:-}" ] && [ -f "${CODEX_HOME}/util" ]; then
  util=$(cat "${CODEX_HOME}/util")
fi
percent=$(awk "BEGIN { printf \"%.1f\", $util * 100 }")
printf '{"type":"token_count","rate_limits":{"primary":{"used_percent":%s,"window_minutes":300},"secondary":{"used_percent":10.0,"window_minutes":10080}}}\n' "$percent"

case " $* " in
  *" --skip-git-repo-check "*)
    echo '{"type":"turn.completed"}'
    ;;
  *)
    mkdir -p artifacts
    printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
    echo '{"type":"turn.completed"}'
    ;;
esac
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
        std::fs::write(dir.join("auth.json"), "{}").unwrap();
        if let Some(util) = util {
            std::fs::write(dir.join("util"), util.to_string()).unwrap();
        }
    }

    fn write_config(&self, codex: &Path, token: Option<&str>) -> PathBuf {
        let path = self.root.join("taskd.toml");
        let token_line = if let Some(token) = token {
            std::fs::write(self.root.join("api.token"), token).unwrap();
            "token_file = \"api.token\"\n"
        } else {
            ""
        };
        let text = format!(
            r#"db = "taskd.sqlite3"
workspace_root = "workspaces"
tick_ms = 50
max_concurrency = 4
lease_grace_secs = 60
idle_timeout_secs = 30
kill_grace_secs = 1
review_timeout_secs = 30
retry_backoff_base_secs = 0

[accounts]
codex_dir = "codex-accounts"
max_runs_per_account = 2

[api]
listen = "127.0.0.1:{port}"
{token_line}

[adapters.codex]
command = "{codex}"

[[providers]]
id = "pool"
adapter = "codex"
tiers = ["frontier", "standard", "cheap"]
concurrency = 1
account_pool = true
"#,
            port = self.port,
            codex = codex.display(),
        );
        std::fs::write(&path, text).unwrap();
        path
    }

    fn workspace(&self, name: &str) -> String {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn taskctl(&self, args: &[&str]) -> String {
        let out = Command::new(bin("taskctl")).arg("--db").arg(self.root.join("taskd.sqlite3")).args(args).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "taskctl {args:?} failed: {stdout}{}", String::from_utf8_lossy(&out.stderr));
        stdout
    }

    fn add(&self, title: &str) -> String {
        let ws = self.workspace(&format!("ws-{title}"));
        self.taskctl(&[
            "add",
            "--title",
            title,
            "--objective",
            "phase 14 codex account pool scenario",
            "--check-cmd",
            "true",
            "--workspace",
            &ws,
        ])
        .trim()
        .to_string()
    }

    fn start_taskd(&self, config: &Path) -> Proc {
        static STARTS: AtomicUsize = AtomicUsize::new(0);
        let log = self.root.join(format!("taskd-{}.log", STARTS.fetch_add(1, Ordering::Relaxed)));
        let child = Command::new(bin("taskd"))
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
                panic!("taskd exited early with {status}\n{}", daemon.log_text());
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
        assert!(ticked, "the dispatcher never published a snapshot\n{}", daemon.log_text());
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/api/v1{path}", self.port)
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>, headers: &[&str]) -> Resp {
        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-S", "-D", "-", "--max-time", "35", "-H", "Expect:", "-X", method]);
        let has = |name: &str| headers.iter().any(|h| h.to_ascii_lowercase().starts_with(&format!("{name}:")));
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
            return Resp { status: 0, headers: vec![], body: String::from_utf8_lossy(&out.stderr).into_owned() };
        };
        let mut lines = head.lines();
        let status = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).unwrap_or(0);
        Resp { status, headers: lines.map(str::to_string).collect(), body: body.to_string() }
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
        let out = self.taskctl(&["replay"]);
        assert!(out.contains("replay: 0 mismatches"), "{out}");
    }

    fn worker_started(&self, task_id: &str) -> Option<(String, Option<String>)> {
        let store = task_core::SqliteStore::open(&self.root.join("taskd.sqlite3")).unwrap();
        task_core::TaskStore::events_for(&store, task_id.parse().unwrap())
            .unwrap()
            .into_iter()
            .find_map(|(_, e)| match e {
                task_core::Event::WorkerStarted { adapter, account, .. } => Some((adapter, account)),
                _ => None,
            })
    }

    fn task_status(&self, task_id: &str) -> task_core::Status {
        let store = task_core::SqliteStore::open(&self.root.join("taskd.sqlite3")).unwrap();
        task_core::TaskStore::get(&store, task_id.parse().unwrap()).unwrap().unwrap().status
    }
}

/// 受け入れ 1・2: 残量の高い codex アカウントが選ばれ、`CODEX_HOME` が一致する。`token_count` の
/// `rate_limits` が `GET /accounts` に反映され（短い枠/長い枠が `window_minutes` に従う）、再起動しても残る。
#[test]
fn codex_account_selection_follows_headroom_and_sets_codex_home_and_records_rate_limits() {
    let env = Env::new();
    let codex = env.write_codex_stub();
    // a は残量が少ない（util 0.9）、b は多い（util 0.2）。
    env.account("a", Some(0.9));
    env.account("b", Some(0.2));
    let config = env.write_config(&codex, None);
    let mut daemon = env.start_taskd(&config);
    env.wait_api(&mut daemon);

    // 1 回目は観測値が無いので id 昇順のタイブレークで "a" に行く。
    let t1 = env.add("t1");
    env.taskctl(&["approve", &t1]);
    assert!(wait_until(Duration::from_secs(10), || env.task_status(&t1) == task_core::Status::Done), "t1 never completed");
    let (adapter1, account1) = env.worker_started(&t1).expect("worker started");
    assert_eq!(adapter1, "codex");
    assert_eq!(account1.as_deref(), Some("a"));

    // "a" は util 0.9 の観測値がついた。"b" はまだ観測値が無い（score 1.0）ので次はそちらへ行く。
    let t2 = env.add("t2");
    env.taskctl(&["approve", &t2]);
    assert!(wait_until(Duration::from_secs(10), || env.task_status(&t2) == task_core::Status::Done), "t2 never completed");
    assert_eq!(env.worker_started(&t2).and_then(|(_, a)| a).as_deref(), Some("b"));

    // GET /accounts に反映されている: short window (300 min <= 1440) -> five_hour, long window (10080 min) -> seven_day。
    let accounts = env.get("/accounts").json();
    assert_eq!(accounts["roots"]["codex"], json!(env.accounts_dir.display().to_string()));
    let items = accounts["items"].as_array().unwrap();
    let a = items.iter().find(|it| it["id"] == "a" && it["adapter"] == "codex").unwrap();
    assert_eq!(a["usage"]["five_hour"]["utilization"], json!(0.9));
    assert_eq!(a["usage"]["seven_day"]["utilization"], json!(0.1));
    let b = items.iter().find(|it| it["id"] == "b" && it["adapter"] == "codex").unwrap();
    assert_eq!(b["usage"]["five_hour"]["utilization"], json!(0.2));

    // プールを使う run の cooldown はプロバイダには付かない。
    assert_eq!(env.get("/daemon").json()["snapshot"]["cooldowns"], json!([]));

    // 再起動しても観測値は残る。
    drop(daemon);
    let mut daemon = env.start_taskd(&config);
    env.wait_api(&mut daemon);
    let accounts_after_restart = env.get("/accounts").json();
    let items = accounts_after_restart["items"].as_array().unwrap();
    let a = items.iter().find(|it| it["id"] == "a").unwrap();
    assert_eq!(a["usage"]["five_hour"]["utilization"], json!(0.9));

    env.replay_is_consistent();
}

/// 受け入れ 3: `POST /accounts {"adapter":"codex"}` → `login` は `kind: "device_code"` と `user_code` を返す。
/// スタブの `codex login --device-auth` の完了後（`auth.json` ができて exit 0）に `logged_in: true` になる。
/// `login/code` は 409 `login_code_not_supported`。
#[test]
fn codex_device_login_flow_completes_without_login_code_and_logs_no_secrets() {
    let env = Env::new();
    let codex = env.write_codex_stub();
    let config = env.write_config(&codex, Some("s3cret-admin-token"));
    let mut daemon = env.start_taskd(&config);
    let mut env = env;
    env.wait_api(&mut daemon);
    env.token = Some("s3cret-admin-token".into());

    let created = env.post("/accounts", json!({"id": "c", "adapter": "codex"}));
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(created.json()["adapter"], json!("codex"));
    assert_eq!(created.json()["logged_in"], json!(false));

    let started = env.post_empty("/accounts/c/login?adapter=codex");
    assert_eq!(started.status, 200, "{}", started.body);
    let body = started.json();
    assert_eq!(body["kind"], json!("device_code"));
    assert_eq!(body["url"], json!("https://auth.openai.com/codex/device"));
    assert_eq!(body["user_code"], json!("ABCD-EFGHI"));
    assert!(body["expires_at"].is_string());

    // codex は login/code を使わない: 409。
    env.post("/accounts/c/login/code?adapter=codex", json!({"code": "x"}))
        .assert_problem(409, "login_code_not_supported");

    // taskd のポーリングが完了を検知するまで待つ（人が別デバイスで入力し終わった、のスタブ側の代わり:
    // スタブは待たずに自分で auth.json を書いて exit 0 する）。
    let logged_in = wait_until(Duration::from_secs(10), || {
        env.get("/accounts").json()["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|it| it["id"] == "c" && it["logged_in"] == true))
    });
    assert!(logged_in, "codex account never became logged_in: {}", env.get("/accounts").body);
    assert!(env.accounts_dir.join("c").join("auth.json").is_file());

    // ログもコードも URL も出ない（D5）。
    let log = daemon.log_text();
    assert!(!log.contains("ABCD-EFGHI"), "{log}");
    assert!(!log.contains("codex/device"), "{log}");

    // 後片付け: DELETE は .removed へ移す。
    let deleted = env.delete("/accounts/c?adapter=codex");
    assert_eq!(deleted.status, 200, "{}", deleted.body);
    assert!(!env.accounts_dir.join("c").exists());

    drop(daemon);
    env.replay_is_consistent();
}
