//! DESIGN §6 Phase 9 の受け入れ 4〜8（ADR-0013、`docs/gui/api.md`）のうち、実バイナリ `taskd`（`[api]` 有効）/ `taskctl` と
//! fake ワーカー（`sh` スクリプト）と `curl` で再現するもの。接続先は 127.0.0.1 だけで、外部ネットワークに出ない。
//!
//! 4. `[api]` が無ければリッスンしない。有効なら `/health` が `api_version` と `schema_version` を返す
//! 5. approve / reject / answer / cancel / タスク作成 / plan が状態機械を通る。無効な遷移と `expected_status` の不一致は 409（problem+json）
//! 6. SSE 購読中の `taskctl add` が 2 秒以内に `Created` として届き、`Last-Event-ID` での再接続で取りこぼさない
//! 7. レート制限シナリオで `/daemon` に実行中の run と cooldown が現れ、`ProviderThrottled` がイベントに残る
//! 8. loopback 以外で `token_file` 無しは設定エラー、許可されない `Host` は 400、ワークスペース外の成果物は 403、`env` の値は応答に出ない

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use task_core::{ArtifactRef, Event, SCHEMA_VERSION, SqliteStore, Status, Task, TaskId, TaskStore};

fn bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug_dir = exe.parent().unwrap().parent().unwrap();
    let path = debug_dir.join(name);
    assert!(path.exists(), "{} not found; run `cargo test --workspace`", path.display());
    path
}

/// OS に空きポートを選ばせて閉じる（taskd が bind するまでの僅かな競合は許容する）。
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

/// drop で kill する子プロセス（taskd / SSE の curl）。
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
    /// 応答のヘッダ行（`Name: value`）。
    headers: Vec<String>,
    body: String,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {} {}", self.status, self.body))
    }

    /// ヘッダ名は大文字小文字を区別せず、値はそのまま返す。
    fn header(&self, name: &str) -> Option<String> {
        self.headers.iter().find_map(|h| {
            let (n, v) = h.split_once(':')?;
            n.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }

    /// problem+json の `code` を確かめる。
    fn assert_problem(&self, status: u16, code: &str) -> Value {
        assert_eq!(self.status, status, "{}", self.body);
        let ct = self.header("content-type").unwrap_or_default();
        assert!(ct.starts_with("application/problem+json"), "content-type {ct}: {}", self.body);
        let v = self.json();
        assert_eq!(v["code"], code, "{v}");
        assert_eq!(v["status"], status, "{v}");
        v
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    store: Arc<SqliteStore>,
    port: u16,
    token: Option<String>,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let db = root.join("taskd.sqlite3");
        let store = Arc::new(SqliteStore::open(&db).unwrap());
        Self { _tmp: tmp, root, db, store, port: free_port(), token: None }
    }

    fn write_script(&self, body: &str) -> PathBuf {
        let path = self.root.join("fake-worker.sh");
        std::fs::write(&path, format!("#!/bin/sh\nset -u\n{body}\n")).unwrap();
        path
    }

    /// `api` は `[api]` 節の本体（空なら節を書かない）。`provider_env` は `[[providers]]` の `env` の TOML インライン表。
    fn write_config(&self, script: &Path, api: &str, provider_env: &str) -> PathBuf {
        let path = self.root.join("taskd.toml");
        let api_section = if api.is_empty() { String::new() } else { format!("[api]\n{api}\n") };
        let text = format!(
            r#"db = "taskd.sqlite3"
workspace_root = "workspaces"
tick_ms = 50
max_concurrency = 2
lease_grace_secs = 60
idle_timeout_secs = 30
kill_grace_secs = 1
review_timeout_secs = 30
retry_backoff_base_secs = 0

{api_section}
[adapters.fake]
command = ["sh", "{script}"]
env = {{ ADAPTER_SECRET = "adapter-s3cr3t-value" }}

[[providers]]
id = "fake-local"
adapter = "fake"
tiers = ["frontier", "standard", "cheap"]
concurrency = 2
model = "fake"
env = {provider_env}
"#,
            script = script.display(),
            provider_env = if provider_env.is_empty() { "{}" } else { provider_env },
        );
        std::fs::write(&path, text).unwrap();
        path
    }

    fn api_listen(&self) -> String {
        format!("listen = \"127.0.0.1:{}\"", self.port)
    }

    fn workspace(&self, name: &str) -> String {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn taskctl(&self, args: &[&str]) -> String {
        let out = Command::new(bin("taskctl")).arg("--db").arg(&self.db).args(args).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "taskctl {args:?} failed: {stdout}{}", String::from_utf8_lossy(&out.stderr));
        stdout
    }

    fn add(&self, args: &[&str]) -> TaskId {
        let mut full = vec!["add", "--objective", "phase 9 api scenario"];
        full.extend_from_slice(args);
        self.taskctl(&full).trim().parse().unwrap()
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

    /// `/health` が 200 を返すまで待つ（無認証）。
    fn wait_api(&self, daemon: &mut Proc) {
        let ok = wait_until(Duration::from_secs(20), || {
            if let Ok(Some(status)) = daemon.child.try_wait() {
                panic!("taskd exited early with {status}\n{}", daemon.log_text());
            }
            self.request("GET", "/health", None, &[]).status == 200
        });
        assert!(ok, "API did not come up\n{}", daemon.log_text());
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/api/v1{path}", self.port)
    }

    /// `curl` で 1 要求。接続できなければ `status = 0`。`token` があれば `Authorization` を付ける（`headers` に明示があればそちら）。
    fn request(&self, method: &str, path: &str, body: Option<&str>, headers: &[&str]) -> Resp {
        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-S", "-D", "-", "--max-time", "10", "-H", "Expect:", "-X", method]);
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

    fn task(&self, id: TaskId) -> Task {
        self.store.get(id).unwrap().unwrap()
    }

    fn events(&self, id: TaskId) -> Vec<Event> {
        self.store.events_for(id).unwrap().into_iter().map(|(_, e)| e).collect()
    }

    fn replay_is_consistent(&self) {
        let out = self.taskctl(&["replay"]);
        assert!(out.contains("replay: 0 mismatches"), "{out}");
    }
}

fn id_of(v: &Value) -> TaskId {
    v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}")).parse().unwrap()
}

/// SSE の本文を `(event, id, data)` に分ける。
fn sse_events(text: &str) -> Vec<(String, Option<u64>, Value)> {
    text.split("\n\n")
        .filter_map(|block| {
            let mut event = None;
            let mut id = None;
            let mut data = String::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("id:") {
                    id = v.trim().parse().ok();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push_str(v.trim_start());
                }
            }
            let data = serde_json::from_str(&data).ok()?;
            Some((event?, id, data))
        })
        .collect()
}

/// 受け入れ 4: `[api]` が無ければリッスンしない。有効にすると `/health` が版と版数を返す。
#[test]
fn api_is_off_by_default_and_health_reports_versions_when_enabled() {
    let env = Env::new();
    let script = env.write_script("cat >/dev/null\necho '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'");

    let config = env.write_config(&script, "", "");
    let mut daemon = env.start_taskd(&config);
    std::thread::sleep(Duration::from_millis(800));
    assert!(daemon.child.try_wait().unwrap().is_none(), "taskd should keep running\n{}", daemon.log_text());
    assert_eq!(env.get("/health").status, 0, "nothing listens without [api]");
    drop(daemon);

    let config = env.write_config(&script, &env.api_listen(), "");
    let mut daemon = env.start_taskd(&config);
    env.wait_api(&mut daemon);
    let health = env.get("/health");
    assert_eq!(health.status, 200, "{}", health.body);
    let v = health.json();
    assert_eq!(v["api_version"], "1", "{v}");
    assert_eq!(v["schema_version"], json!(SCHEMA_VERSION), "{v}");
    assert_eq!(v["db"]["journal_mode"], "wal", "{v}");
    assert_eq!(health.header("cache-control").as_deref(), Some("no-store"));
    assert!(health.header("access-control-allow-origin").is_none(), "no CORS headers");

    // 最初の tick の後はメモリ上のスナップショットが見える（DB を読まない。ADR-0013 D4）。
    assert!(wait_until(Duration::from_secs(5), || !env.get("/daemon").json()["snapshot"].is_null()));
    let snap = env.get("/daemon").json()["snapshot"].clone();
    assert_eq!(snap["providers"][0]["id"], "fake-local", "{snap}");
    assert_eq!(snap["tick_ms"], 50, "{snap}");
    assert_eq!(env.get("/config").json()["db"], env.db.to_string_lossy().as_ref());
    assert_eq!(env.get("/no-such-endpoint").status, 404);
}

/// 受け入れ 5: API からの作成・承認・却下・回答・取消・plan が状態機械を通る。無効な遷移と `expected_status` 不一致は 409。
#[test]
fn api_mutations_go_through_the_state_machine() {
    let env = Env::new();
    let script = env.write_script(
        r#"input=$(cat)
case "$input" in
  *'"answers"'*) echo '{"type":"done","summary":"used the answer","evidence":[]}' ;;
  *) echo '{"type":"question","text":"which version should I target?"}' ;;
esac"#,
    );
    let config = env.write_config(&script, &env.api_listen(), "");
    let mut daemon = env.start_taskd(&config);
    env.wait_api(&mut daemon);
    let ws = env.workspace("ws-q");

    // 作成（taskctl add 相当）。
    let created = env.post(
        "/tasks",
        json!({"title": "ask me", "objective": "api", "acceptance": [{"type": "command", "cmd": "true"}], "workspace": ws}),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    let task = created.json();
    let id = id_of(&task);
    assert_eq!(task["status"], "draft");
    assert!(created.header("location").unwrap_or_default().ends_with(&format!("/api/v1/tasks/{id}")));
    env.post("/tasks", json!({"title": "x", "objective": "y", "acceptance": []}))
        .assert_problem(422, "validation");
    // ADR-0014 D3（P-G16）: 空白だけの title と存在しない親は 422（field 付き）で、何も作らない。
    let human = json!([{"type": "human", "text": "t"}]);
    let v = env.post("/tasks", json!({"title": "  ", "objective": "y", "acceptance": human})).assert_problem(422, "validation");
    assert_eq!(v["errors"][0]["field"], "title", "{v}");
    let v = env
        .post("/tasks", json!({"title": "x", "objective": "y", "acceptance": human, "parent": TaskId::new().to_string()}))
        .assert_problem(422, "validation");
    assert_eq!(v["errors"][0]["field"], "parent", "{v}");
    // ADR-0014 D2（P-G15）: q は objective も対象（最初のタスクは title "ask me"、objective "api"）。
    let found = env.get("/tasks?q=api").json();
    assert_eq!((found["total"].as_u64(), found["items"][0]["id"].as_str()), (Some(1), Some(id.to_string().as_str())), "{found}");
    env.post("/tasks", json!({"title": "x", "objective": "y", "acceptance": [{"type": "human", "text": "t"}], "bogus": 1}))
        .assert_problem(400, "bad_request");

    // expected_status の不一致は状態を変えずに 409 conflict。
    let conflict = env.post(&format!("/tasks/{id}/approve"), json!({"expected_status": "ready"})).assert_problem(409, "conflict");
    assert_eq!((conflict["expected"].as_str(), conflict["actual"].as_str()), (Some("ready"), Some("draft")), "{conflict}");
    assert_eq!(env.task(id).status, Status::Draft);

    let accepted = env.post(&format!("/tasks/{id}/approve"), json!({"expected_status": "draft"}));
    assert_eq!(accepted.status, 200, "{}", accepted.body);
    assert_eq!((accepted.json()["from"].clone(), accepted.json()["to"].clone()), (json!("draft"), json!("ready")));

    // 二度目の approve と execute への reject は状態機械が拒否する。
    let invalid = env.post(&format!("/tasks/{id}/approve"), json!({})).assert_problem(409, "invalid_transition");
    assert_eq!(invalid["trigger"], "approve", "{invalid}");
    env.post(&format!("/tasks/{id}/reject"), json!({})).assert_problem(409, "invalid_transition");

    // ワーカーの質問 → API で回答 → done。
    assert!(wait_until(Duration::from_secs(20), || env.task(id).status == Status::Blocked), "{:?}", env.events(id));
    let detail = env.get(&format!("/tasks/{id}")).json();
    assert!(detail["actions"].as_array().unwrap().contains(&json!("answer")), "{detail}");
    env.post(&format!("/tasks/{id}/answer"), json!({"answer": "   "})).assert_problem(422, "validation");
    let answered = env.post(&format!("/tasks/{id}/answer"), json!({"answer": "target v2", "expected_status": "blocked"}));
    assert_eq!(answered.status, 200, "{}", answered.body);
    assert_eq!(answered.json()["to"], "ready");
    assert!(env.events(id).iter().any(|e| matches!(
        e,
        Event::Answered { question, answer } if question == "which version should I target?" && answer == "target v2"
    )));
    assert!(wait_until(Duration::from_secs(20), || env.task(id).status == Status::Done), "{:?}", env.events(id));

    // Approval の承認と却下。
    let approval = |title: &str| {
        let r = env.post(
            "/tasks",
            json!({"title": title, "objective": "decide", "kind": "approval", "acceptance": [{"type": "human", "text": "ok?"}]}),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        assert_eq!(r.json()["status"], "ready");
        id_of(&r.json())
    };
    let a = approval("approve me");
    let r = env.post(&format!("/tasks/{a}/approve"), json!({"note": "lgtm", "expected_status": "ready"}));
    assert_eq!((r.status, r.json()["to"].clone()), (200, json!("done")), "{}", r.body);
    assert!(env.events(a).iter().any(|e| matches!(e, Event::ApprovalDecided { approved: true, .. })));
    let b = approval("reject me");
    let r = env.post(&format!("/tasks/{b}/reject"), json!({"note": "no"}));
    assert_eq!((r.status, r.json()["to"].clone()), (200, json!("failed")), "{}", r.body);

    // cancel は非終端だけ。
    let draft = id_of(&env.post("/tasks", json!({"title": "c", "objective": "o", "acceptance": [{"type": "human", "text": "t"}]})).json());
    let r = env.post(&format!("/tasks/{draft}/cancel"), json!({"expected_status": "draft"}));
    assert_eq!((r.status, r.json()["to"].clone()), (200, json!("cancelled")), "{}", r.body);
    assert!(r.json()["cascaded"].is_array(), "{}", r.body);
    env.post(&format!("/tasks/{draft}/cancel"), json!({})).assert_problem(409, "invalid_transition");
    env.post(&format!("/tasks/{}/cancel", TaskId::new()), json!({})).assert_problem(404, "task_not_found");

    // plan（taskctl plan 相当）。
    let plan = env.post("/plans", json!({"goal": "split the work\nsecond line"}));
    assert_eq!(plan.status, 201, "{}", plan.body);
    let plan = plan.json();
    assert_eq!((plan["kind"].clone(), plan["status"].clone(), plan["title"].clone()), (json!("plan"), json!("draft"), json!("split the work")));
    env.post("/plans", json!({"goal": "  "})).assert_problem(422, "validation");

    // 変更系の前提（Content-Type と Origin）。
    let r = env.request("POST", &format!("/tasks/{draft}/cancel"), Some("{}"), &["Content-Type: text/plain"]);
    r.assert_problem(415, "unsupported_media_type");
    let r = env.request("POST", "/plans", Some(r#"{"goal":"g"}"#), &["Origin: http://evil.example"]);
    r.assert_problem(403, "origin_forbidden");

    let replay = env.post("/replay", json!({}));
    assert_eq!((replay.status, replay.json()["mismatches"].clone()), (200, json!([])), "{}", replay.body);
    env.replay_is_consistent();
}

/// 受け入れ 6: SSE 購読中の `taskctl add` が 2 秒以内に届き、`Last-Event-ID` で再接続しても取りこぼさない。
#[test]
fn sse_delivers_created_quickly_and_resumes_from_last_event_id() {
    let env = Env::new();
    let script = env.write_script("cat >/dev/null\necho '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'");
    let config = env.write_config(&script, &env.api_listen(), "");
    let mut daemon = env.start_taskd(&config);
    env.wait_api(&mut daemon);

    let subscribe = |name: &str, last_event_id: Option<u64>| {
        let log = env.root.join(name);
        let mut cmd = Command::new("curl");
        cmd.args(["-s", "-N", "--max-time", "30"]);
        if let Some(id) = last_event_id {
            cmd.args(["-H", &format!("Last-Event-ID: {id}")]);
        }
        let child = cmd
            .arg(env.url("/stream"))
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let p = Proc { child, log };
        assert!(wait_until(Duration::from_secs(5), || p.log_text().contains("event: hello")), "no hello: {}", p.log_text());
        p
    };
    let created_id = |p: &Proc, task: TaskId| {
        sse_events(&p.log_text()).into_iter().find_map(|(event, id, data)| {
            (event == "task.event" && data["event"]["type"] == "created" && data["task_id"] == task.to_string()).then_some(id)
        })
    };

    let first = subscribe("sse-1.txt", None);
    let started = Instant::now();
    let t1 = env.add(&["--title", "sse one", "--check-cmd", "true"]);
    assert!(wait_until(Duration::from_secs(2), || created_id(&first, t1).is_some()), "Created not delivered: {}", first.log_text());
    let elapsed = started.elapsed();
    assert!(elapsed <= Duration::from_secs(2), "took {elapsed:?}");
    let last_seen = created_id(&first, t1).flatten().expect("task.event carries an id");
    let hello = sse_events(&first.log_text()).into_iter().find(|(e, _, _)| e == "hello").unwrap().2;
    assert!(hello["cursor"].as_u64().unwrap() < last_seen, "{hello}");
    drop(first);

    // 切断中に起きたことを、Last-Event-ID からの再接続で全て受け取る。
    let t2 = env.add(&["--title", "sse two", "--check-cmd", "true"]);
    env.taskctl(&["approve", &t1.to_string()]);
    let second = subscribe("sse-2.txt", Some(last_seen));
    assert!(wait_until(Duration::from_secs(5), || created_id(&second, t2).is_some()), "missed Created: {}", second.log_text());
    assert!(
        wait_until(Duration::from_secs(5), || sse_events(&second.log_text()).iter().any(|(e, _, d)| e == "task.event"
            && d["task_id"] == t1.to_string()
            && d["event"]["type"] == "transitioned")),
        "missed the approval of t1: {}",
        second.log_text()
    );
    let events = sse_events(&second.log_text());
    let hello = &events.iter().find(|(e, _, _)| e == "hello").unwrap().2;
    assert_eq!(hello["cursor"].as_u64(), Some(last_seen), "{hello}");
    let ids: Vec<u64> = events.iter().filter(|(e, _, _)| e == "task.event").filter_map(|(_, id, _)| *id).collect();
    assert!(ids.iter().all(|id| *id > last_seen), "replayed an already-seen event: {ids:?}");
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "ids must increase: {ids:?}");
    drop(second);
    env.replay_is_consistent();
}

/// 受け入れ 7: レート制限で `/daemon` に実行中の run とプロバイダの cooldown が現れ、`ProviderThrottled` が残る。
#[test]
fn daemon_view_shows_in_flight_runs_and_cooldowns_and_throttle_is_recorded() {
    let env = Env::new();
    let script = env.write_script(
        r#"cat >/dev/null
if [ -f throttle-me ]; then
  echo '{"type":"error","message":"429 rate limited","retryable":true,"provider_failure":{"kind":"throttled","retry_after_secs":30}}'
else
  sleep 6
  echo '{"type":"done","summary":"slow ok","evidence":[]}'
fi"#,
    );
    let config = env.write_config(&script, &env.api_listen(), "");
    let ws_slow = env.workspace("ws-slow");
    let ws_throttle = env.workspace("ws-throttle");
    std::fs::write(Path::new(&ws_throttle).join("throttle-me"), "").unwrap();
    let slow = env.add(&["--title", "slow", "--check-cmd", "true", "--workspace", &ws_slow]);
    let throttled = env.add(&["--title", "throttled", "--check-cmd", "true", "--max-retries", "0", "--workspace", &ws_throttle]);

    let mut daemon = env.start_taskd(&config);
    env.wait_api(&mut daemon);
    // 先に slow を走らせてから、同じプロバイダで throttled を走らせる（先に cooldown に入ると slow が dispatch されない）。
    env.taskctl(&["approve", &slow.to_string()]);
    let in_flight_has_slow = |snap: &Value| {
        snap["in_flight"].as_array().is_some_and(|v| {
            v.iter().any(|r| r["task_id"] == slow.to_string() && r["kind"] == "worker" && r["provider"] == "fake-local")
        })
    };
    assert!(
        wait_until(Duration::from_secs(10), || in_flight_has_slow(&env.get("/daemon").json()["snapshot"])),
        "slow run never appeared in /daemon: {}",
        env.get("/daemon").body
    );
    env.taskctl(&["approve", &throttled.to_string()]);

    let mut snap = Value::Null;
    let seen = wait_until(Duration::from_secs(5), || {
        snap = env.get("/daemon").json()["snapshot"].clone();
        snap["cooldowns"].as_array().is_some_and(|c| c.iter().any(|c| c["provider"] == "fake-local" && c["reason"] == "throttled"))
    });
    assert!(seen, "no cooldown in /daemon: {snap}");
    assert!(in_flight_has_slow(&snap), "the slow run is still in flight during the cooldown: {snap}");
    assert!(snap["providers"][0]["in_use"].as_u64().unwrap() >= 1, "{snap}");
    assert!(
        snap["cooldowns"][0]["until"].as_str().unwrap() > snap["last_tick_at"].as_str().unwrap(),
        "cooldown ends in the future: {snap}"
    );

    assert!(env.events(throttled).iter().any(|e| matches!(
        e,
        Event::ProviderThrottled { provider, reason, .. } if provider == "fake-local" && reason.as_deref() == Some("throttled")
    )), "{:?}", env.events(throttled));
    let page = env.get(&format!("/events?task_id={throttled}&types=provider_throttled")).json();
    assert_eq!(page["items"].as_array().map(Vec::len), Some(1), "{page}");
    assert_eq!(env.task(throttled).attempts, 0, "a requeue does not consume attempts");
    drop(daemon);
    env.replay_is_consistent();
}

/// 受け入れ 8: 設定エラー、Bearer、Host 検査、ワークスペース外の成果物、`env` の値を出さないこと。
#[test]
fn api_enforces_token_host_and_workspace_boundaries_without_leaking_env_values() {
    let mut env = Env::new();
    let script = env.write_script("cat >/dev/null\necho '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'");

    // loopback 以外で token_file 無し → 設定エラー（exit 2）。
    let config = env.write_config(&script, &format!("listen = \"0.0.0.0:{}\"", env.port), "");
    let mut bad = env.start_taskd(&config);
    assert!(wait_until(Duration::from_secs(10), || bad.child.try_wait().unwrap().is_some()), "taskd must refuse the config");
    assert_eq!(bad.child.wait().unwrap().code(), Some(2));
    assert!(bad.log_text().contains("token_file is required"), "{}", bad.log_text());
    drop(bad);

    std::fs::write(env.root.join("api.token"), "  tok-9f8e7d\n").unwrap();
    let api = format!("{}\ntoken_file = \"api.token\"", env.api_listen());
    let config = env.write_config(&script, &api, r#"{ SECRET_TOKEN = "s3cr3t-provider-value" }"#);
    let mut daemon = env.start_taskd(&config);
    env.wait_api(&mut daemon);

    // Bearer。
    let r = env.get("/tasks");
    r.assert_problem(401, "unauthorized");
    assert!(r.header("www-authenticate").unwrap_or_default().starts_with("Bearer"), "{:?}", r.headers);
    env.request("GET", "/tasks", None, &["Authorization: Bearer wrong"]).assert_problem(401, "unauthorized");
    env.token = Some("tok-9f8e7d".into());
    assert_eq!(env.get("/tasks").status, 200);

    // Host 検査（/health も対象）。
    env.request("GET", "/tasks", None, &["Host: evil.example"]).assert_problem(400, "host_not_allowed");
    env.request("GET", "/health", None, &["Host: evil.example:80"]).assert_problem(400, "host_not_allowed");
    let localhost = format!("Host: localhost:{}", env.port);
    assert_eq!(env.request("GET", "/health", None, &[localhost.as_str()]).status, 200);

    // env の値・トークン・token_file の場所は応答に出ない（キー名は出る）。
    for path in ["/config", "/providers", "/daemon", "/health"] {
        let r = env.get(path);
        assert_eq!(r.status, 200, "{path}: {}", r.body);
        for secret in ["s3cr3t-provider-value", "adapter-s3cr3t-value", "tok-9f8e7d", "api.token"] {
            assert!(!r.body.contains(secret), "{path} leaks {secret}: {}", r.body);
        }
    }
    let cfg = env.get("/config").json();
    assert!(cfg.to_string().contains("SECRET_TOKEN"), "env keys are listed: {cfg}");
    assert_eq!(cfg["api"]["auth_required"], true, "{cfg}");

    // ワークスペース外を指す成果物は 403（`..` と symlink の両方）。中のものは読める。
    let ws = env.workspace("ws-files");
    std::fs::write(env.root.join("outside.txt"), "top secret").unwrap();
    std::fs::write(Path::new(&ws).join("inside.txt"), "inside").unwrap();
    std::os::unix::fs::symlink(env.root.join("outside.txt"), Path::new(&ws).join("link.txt")).unwrap();
    let created = env.post("/tasks", json!({"title": "files", "objective": "o", "acceptance": [{"type": "human", "text": "t"}], "workspace": ws}));
    assert_eq!(created.status, 201, "{}", created.body);
    let id = id_of(&created.json());
    let run_id = TaskId::new().to_string();
    for path in ["inside.txt", "../outside.txt", "link.txt"] {
        let artifact = ArtifactRef { name: path.into(), path: path.into(), sha256: "0".repeat(64), kind: "file".into() };
        env.store.append_event(id, &Event::ArtifactProduced { run_id: run_id.clone(), artifact }).unwrap();
    }
    let inside = env.get(&format!("/tasks/{id}/artifacts/0"));
    assert_eq!((inside.status, inside.body.as_str()), (200, "inside"));
    for idx in [1, 2] {
        let r = env.get(&format!("/tasks/{id}/artifacts/{idx}"));
        let v = r.assert_problem(403, "path_forbidden");
        assert!(!r.body.contains("top secret"), "{v}");
    }
    let list = env.get(&format!("/tasks/{id}/artifacts")).json();
    assert_eq!(list["items"][1]["forbidden"], true, "{list}");
    env.get(&format!("/tasks/{id}/runs/not-a-ulid/stdout")).assert_problem(403, "path_forbidden");
    env.replay_is_consistent();
}

/// 受け入れ 2（Phase 9 監査の再現手順）: 速い tick で動く taskd に、別プロセスの `taskctl` と API から書き込み続けても、
/// どちらにも `database is locked` が出ず taskd も落ちない（書き込みトランザクションは IMMEDIATE + busy_timeout）。
#[test]
fn writes_from_taskctl_and_api_while_taskd_ticks_fast_never_hit_database_is_locked() {
    let env = Env::new();
    let script = env.write_script("cat >/dev/null\necho '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'");
    let config = env.write_config(&script, &env.api_listen(), "");
    let text = std::fs::read_to_string(&config).unwrap().replace("tick_ms = 50", "tick_ms = 20");
    std::fs::write(&config, text).unwrap();
    let mut daemon = env.start_taskd(&config);
    env.wait_api(&mut daemon);
    let ws = env.workspace("ws-lock");

    let mut ids = Vec::new();
    for i in 0..150 {
        let id = env.add(&["--title", &format!("cli {i}"), "--check-cmd", "true", "--workspace", &ws]);
        env.taskctl(&["approve", &id.to_string()]);
        ids.push(id);
        if i % 5 == 0 {
            let r = env.post(
                "/tasks",
                json!({"title": format!("api {i}"), "objective": "o", "acceptance": [{"type": "command", "cmd": "true"}], "workspace": ws}),
            );
            assert_eq!(r.status, 201, "{}", r.body);
            let id = id_of(&r.json());
            let r = env.post(&format!("/tasks/{id}/approve"), json!({}));
            assert_eq!(r.status, 200, "{}", r.body);
            ids.push(id);
        }
        assert!(daemon.child.try_wait().unwrap().is_none(), "taskd exited at iteration {i}\n{}", daemon.log_text());
    }
    assert!(
        wait_until(Duration::from_secs(120), || ids.iter().all(|id| env.task(*id).status == Status::Done)),
        "not all tasks finished\n{}",
        daemon.log_text()
    );
    assert!(daemon.child.try_wait().unwrap().is_none(), "taskd must still be running\n{}", daemon.log_text());
    assert!(!daemon.log_text().contains("database is locked"), "{}", daemon.log_text());
    env.replay_is_consistent();
}

/// Phase 12 第 2 段階（ADR-0018 実装メモ）8・9・11: `GET /clusters`、受信箱の `attention[].cluster_unavailable`、`TaskDetail.cluster`。
/// 多重接続が**無い**クラスタを使うので、ssh の設定も外部ネットワークも要らない。
#[test]
fn clusters_endpoint_inbox_attention_and_task_detail_show_an_offline_cluster() {
    let env = Env::new();
    let script = env.write_script(r#"cat >/dev/null; echo '{"type":"done","summary":"unused","evidence":[]}'"#);
    let config = env.write_config(&script, &env.api_listen(), "");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(
        "\n[[clusters]]\nid = \"offline\"\nhost = \"taskd-no-such-host-for-tests\"\nconcurrency = 1\n\
         setup = [\"true\"]\nenv = { SECRET_CLUSTER_VALUE = \"cluster-s3cr3t-value\" }\nrsync_excludes = [\".git/\"]\n",
    );
    std::fs::write(&config, text).unwrap();
    let remote = env.workspace("remote-project");
    let id = env.add(&["--title", "offline work", "--check-cmd", "true", "--cluster", "offline", "--workspace", &remote]);

    let mut daemon = env.start_taskd(&config);
    env.wait_api(&mut daemon);
    env.taskctl(&["approve", &id.to_string()]);

    // 8. /clusters: 設定 + 接続の有無 + cooldown。env の値は出ない。
    let mut clusters = Value::Null;
    let seen = wait_until(Duration::from_secs(10), || {
        clusters = env.get("/clusters").json();
        clusters["items"][0]["cooldown_until"].is_string()
    });
    assert!(seen, "the offline cluster never entered cooldown: {clusters}");
    let c = &clusters["items"][0];
    assert_eq!(c["id"], "offline", "{clusters}");
    assert_eq!(c["host"], "taskd-no-such-host-for-tests");
    assert_eq!(c["connected"], false);
    assert_eq!(c["in_use"], 0);
    assert_eq!(c["concurrency"], 1);
    assert_eq!(c["sync"], "rsync");
    assert_eq!(c["delete_on_push"], false);
    assert_eq!(c["has_setup"], true);
    assert_eq!(c["env_keys"], json!(["SECRET_CLUSTER_VALUE"]));
    assert_eq!(c["rsync_excludes"], json!([".git/"]));
    assert!(c["cooldown_remaining_secs"].as_u64().is_some(), "{clusters}");
    assert!(!clusters.to_string().contains("cluster-s3cr3t-value"), "env values must not leak: {clusters}");
    let config_view = env.get("/config");
    assert!(!config_view.body.contains("cluster-s3cr3t-value"), "{}", config_view.body);
    assert!(!config_view.body.contains("\"setup\""), "{}", config_view.body);
    assert_eq!(config_view.json()["clusters"][0]["has_setup"], true, "{}", config_view.body);
    let snap = env.get("/daemon").json()["snapshot"].clone();
    assert_eq!(snap["clusters"][0]["connected"], false, "{snap}");
    assert_eq!(snap["clusters"][0]["host"], "taskd-no-such-host-for-tests", "{snap}");

    // 9. 受信箱の注意: クラスタごとに 1 件、host と対象タスク数。
    let inbox = env.get("/inbox").json();
    let attention = inbox["attention"].as_array().unwrap_or_else(|| panic!("{inbox}"));
    let items: Vec<&Value> = attention.iter().filter(|a| a["type"] == "cluster_unavailable").collect();
    assert_eq!(items.len(), 1, "{inbox}");
    // ADR-0018 M8: 人のログイン待ちは経路なし（unroutable）ではないので、同じタスクが 2 件に出ない。
    assert!(!attention.iter().any(|a| a["type"] == "unroutable"), "{inbox}");
    assert_eq!(items[0]["cluster"], "offline");
    assert_eq!(items[0]["host"], "taskd-no-such-host-for-tests");
    assert_eq!(items[0]["tasks"], 1);
    assert!(items[0]["at"].is_string(), "{inbox}");
    assert_eq!(inbox["counts"]["attention"].as_u64().unwrap() as usize, attention.len());

    // 11. 詳細にクラスタが出る（API と `taskctl show --json` の両方）。`workspace_dir` は手元の写し。
    let detail = env.get(&format!("/tasks/{id}")).json();
    assert_eq!(detail["cluster"], "offline", "{detail}");
    assert_eq!(
        detail["workspace_dir"],
        env.root.join("workspaces").join(id.to_string()).to_string_lossy().as_ref(),
        "{detail}"
    );
    assert_eq!(detail["task"]["workspace"]["path"], remote, "{detail}");
    let show: Value = serde_json::from_str(env.taskctl(&["show", "--json", &id.to_string()]).trim()).unwrap();
    assert_eq!(show["cluster"], "offline", "{show}");

    // タスクは ready のまま（attempts も消費しない）。
    let t = env.task(id);
    assert_eq!((t.status, t.attempts), (Status::Ready, 0));
    drop(daemon);
    env.replay_is_consistent();
}
