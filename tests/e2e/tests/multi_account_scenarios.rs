//! 複数アカウント運用（ADR-0012 D1/D2）。fake ワーカー（`sh` スクリプト）と実バイナリ `taskctl` / `taskd` だけで動き、
//! ネットワークに出ない。プロバイダ（= アカウント）ごとの `env` はスクリプトから `$ACCOUNT` として見える。
//!
//! 1. 先頭アカウントが並列度の上限に達すると、2 つ目のアカウントがあふれた分を実行する（P-20）
//! 2. レート制限を返したアカウントは cooldown になり、同じタスクは attempts を消費せず次のアカウントで実行される
//! 3. 設定に合うプロバイダが無いタスクは `taskd --until-idle` を止めない（P-33）

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use task_core::{Event, SqliteStore, Status, Task, TaskId, TaskStore};

fn bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug_dir = exe.parent().unwrap().parent().unwrap();
    let path = debug_dir.join(name);
    assert!(path.exists(), "{} not found; run `cargo test --workspace`", path.display());
    path
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    store: Arc<SqliteStore>,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let db = root.join("taskd.sqlite3");
        let store = Arc::new(SqliteStore::open(&db).unwrap());
        Self { _tmp: tmp, root, db, store }
    }

    /// `script` を fake ワーカーにし、`providers`（TOML の `[[providers]]` 群）を持つ設定を書く。
    fn write_config(&self, script_body: &str, providers: &str, extra: &str) -> PathBuf {
        let script = self.root.join("fake-worker.sh");
        std::fs::write(&script, format!("#!/bin/sh\nset -u\n{script_body}\n")).unwrap();
        let path = self.root.join("taskd.toml");
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
{extra}

[adapters.fake]
command = ["sh", "{script}"]

{providers}
"#,
            script = script.display()
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
        let out = Command::new(bin("taskctl")).arg("--db").arg(&self.db).args(args).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "taskctl {args:?} failed: {stdout}{}", String::from_utf8_lossy(&out.stderr));
        stdout
    }

    fn add_approved(&self, args: &[&str]) -> TaskId {
        let mut full = vec!["add", "--objective", "multi-account scenario"];
        full.extend_from_slice(args);
        let id: TaskId = self.taskctl(&full).trim().parse().unwrap();
        self.taskctl(&["approve", &id.to_string()]);
        id
    }

    fn run_taskd(&self, config: &std::path::Path, timeout: Duration) -> String {
        let log = self.root.join("taskd.log");
        let mut child = Command::new(bin("taskd"))
            .args(["--config", config.to_str().unwrap(), "--until-idle", "--max-ticks", "2000", "--log-format", "text"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                assert!(status.success(), "taskd exited with {status}\n{text}");
                return text;
            }
            if start.elapsed() > timeout {
                let _ = child.kill();
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                panic!("taskd did not reach idle within {timeout:?}\n{text}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn task(&self, id: TaskId) -> Task {
        self.store.get(id).unwrap().unwrap()
    }

    fn events(&self, id: TaskId) -> Vec<Event> {
        self.store.events_for(id).unwrap().into_iter().map(|(_, e)| e).collect()
    }

    fn started_models(&self, id: TaskId) -> Vec<String> {
        self.events(id)
            .into_iter()
            .filter_map(|e| match e {
                Event::WorkerStarted { model, .. } => Some(model),
                _ => None,
            })
            .collect()
    }

    /// `WorkerStarted.provider`（どのアカウントで実行したか）。
    fn started_providers(&self, id: TaskId) -> Vec<String> {
        self.events(id)
            .into_iter()
            .filter_map(|e| match e {
                Event::WorkerStarted { provider, .. } => Some(provider.unwrap_or_default()),
                _ => None,
            })
            .collect()
    }

    fn transition_reasons(&self, id: TaskId) -> Vec<String> {
        self.events(id)
            .into_iter()
            .filter_map(|e| match e {
                Event::Transitioned { reason, .. } => Some(reason),
                _ => None,
            })
            .collect()
    }

    fn replay_is_consistent(&self) {
        let out = self.taskctl(&["replay"]);
        assert!(out.contains("replay: 0 mismatches"), "{out}");
    }
}

const TWO_ACCOUNTS: &str = r#"[[providers]]
id = "acct-a"
adapter = "fake"
concurrency = 1
model = "model-a"
env = { ACCOUNT = "a" }

[[providers]]
id = "acct-b"
adapter = "fake"
concurrency = 1
model = "model-b"
env = { ACCOUNT = "b" }
"#;

/// 1: 先頭アカウント（並列度 1）が埋まると、同じ tick で 2 つ目のアカウントがもう 1 件を実行する。
#[test]
fn second_account_runs_the_overflow_when_the_first_is_at_capacity() {
    let env = Env::new();
    let config = env.write_config(
        r#"cat >/dev/null
printf '%s' "$ACCOUNT" > account.txt
sleep 1
echo '{"type":"done","summary":"ok","evidence":[]}'"#,
        TWO_ACCOUNTS,
        "",
    );
    let ws1 = env.workspace("ws-1");
    let ws2 = env.workspace("ws-2");
    let t1 = env.add_approved(&["--title", "one", "--check-cmd", "test -f account.txt", "--workspace", &ws1]);
    let t2 = env.add_approved(&["--title", "two", "--check-cmd", "test -f account.txt", "--workspace", &ws2]);

    let started = Instant::now();
    env.run_taskd(&config, Duration::from_secs(60));
    // 直列（2 秒以上）ではなく並列に動いたこと。
    assert!(started.elapsed() < Duration::from_secs(10));
    for id in [t1, t2] {
        assert_eq!(env.task(id).status, Status::Done, "{:?}", env.events(id));
    }
    let accounts: BTreeSet<String> = [&ws1, &ws2]
        .iter()
        .map(|ws| std::fs::read_to_string(PathBuf::from(ws).join("account.txt")).unwrap())
        .collect();
    assert_eq!(accounts, BTreeSet::from(["a".to_string(), "b".to_string()]));
    let models: BTreeSet<String> = [t1, t2].iter().flat_map(|id| env.started_models(*id)).collect();
    assert_eq!(models, BTreeSet::from(["model-a".to_string(), "model-b".to_string()]));
    let providers: BTreeSet<String> = [t1, t2].iter().flat_map(|id| env.started_providers(*id)).collect();
    assert_eq!(providers, BTreeSet::from(["acct-a".to_string(), "acct-b".to_string()]));
    env.replay_is_consistent();
}

/// 2: アカウント A がレート制限を返すと A は cooldown になり、同じタスクは attempts を消費せずアカウント B で実行される。
#[test]
fn throttled_account_falls_back_to_the_next_account() {
    let env = Env::new();
    let config = env.write_config(
        r#"cat >/dev/null
if [ "$ACCOUNT" = "a" ]; then
  echo '{"type":"error","message":"429 rate limited","retryable":true,"provider_failure":{"kind":"throttled","retry_after_secs":300}}'
else
  printf '%s' "$ACCOUNT" > account.txt
  echo '{"type":"done","summary":"ok","evidence":[]}'
fi"#,
        TWO_ACCOUNTS,
        "",
    );
    let ws = env.workspace("ws-fallback");
    let id = env.add_approved(&["--title", "fallback", "--check-cmd", "test -f account.txt", "--max-retries", "0", "--workspace", &ws]);

    env.run_taskd(&config, Duration::from_secs(60));
    let t = env.task(id);
    assert_eq!((t.status, t.attempts), (Status::Done, 0), "{:?}", env.events(id));
    assert_eq!(env.transition_reasons(id), vec!["accept", "dispatch", "requeue", "dispatch", "worker_done", "review_pass"]);
    assert_eq!(env.started_models(id), vec!["model-a", "model-b"]);
    assert_eq!(env.started_providers(id), vec!["acct-a", "acct-b"]);
    assert_eq!(std::fs::read_to_string(PathBuf::from(&ws).join("account.txt")).unwrap(), "b");
    env.replay_is_consistent();
}

/// 3: worker_hint に合うプロバイダが設定に無いタスクは ready のまま残り、`--until-idle` は warn を出して終わる。
#[test]
fn task_without_a_matching_provider_does_not_block_until_idle() {
    let env = Env::new();
    let config = env.write_config(
        r#"cat >/dev/null; echo '{"type":"done","summary":"ok","evidence":[]}'"#,
        "[[providers]]\nid = \"frontier-only\"\nadapter = \"fake\"\ntiers = [\"frontier\"]\n",
        "[reviewer]\ntier = \"frontier\"",
    );
    let ws = env.workspace("ws-unroutable");
    let id = env.add_approved(&["--title", "cheap", "--tier", "cheap", "--check-cmd", "true", "--workspace", &ws]);

    let log = env.run_taskd(&config, Duration::from_secs(30));
    assert_eq!(env.task(id).status, Status::Ready);
    assert!(log.contains("no provider in the config matches"), "{log}");
    env.replay_is_consistent();
}
