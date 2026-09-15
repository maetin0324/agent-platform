//! DESIGN §6 Phase 12（ADR-0018）: クラスタでのコマンド実行を、実バイナリの `taskd` / `taskctl` と
//! **localhost への ssh** で確かめる（外部ネットワークに出ない）。多重接続が無い環境では skip する。
//!
//! 1. `WorkspaceSpec::Remote` のタスクが、pull → run → push → クラスタでの判定 → pull を通って done になる
//! 2. 多重接続が無いクラスタのタスクは dispatch されず、`ClusterUnavailable` が残り、`--until-idle` を止めない
//! 3. 設定に無いクラスタのタスクは経路なしとして扱われる

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use task_core::{Event, SqliteStore, Status, Task, TaskId, TaskStore};

const HOST: &str = "taskd-localhost";

fn bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let path = exe.parent().unwrap().parent().unwrap().join(name);
    assert!(path.exists(), "{} not found; run `cargo test --workspace`", path.display());
    path
}

fn control_master_alive(host: &str) -> bool {
    Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-O", "check", host])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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

    /// fake ワーカー: リモートに置かれたファイルを読み、手元の写しに答えを書く（クラスタ側のデータを使う仕事の代役）。
    fn write_script(&self) -> PathBuf {
        let path = self.root.join("fake-worker.sh");
        std::fs::write(
            &path,
            "#!/bin/sh\nset -u\ncat >/dev/null\nif [ -f secret.txt ]; then\n  cp secret.txt answer.txt\n  echo '{\"type\":\"done\",\"summary\":\"used the cluster file\",\"evidence\":[]}'\nelse\n  echo '{\"type\":\"error\",\"message\":\"secret.txt not pulled\",\"retryable\":false}'\nfi\n",
        )
        .unwrap();
        path
    }

    fn write_config(&self, script: &Path, clusters: &str) -> PathBuf {
        let path = self.root.join("taskd.toml");
        std::fs::write(
            &path,
            format!(
                r#"db = "taskd.sqlite3"
workspace_root = "workspaces"
tick_ms = 50
max_concurrency = 2
lease_grace_secs = 60
idle_timeout_secs = 30
kill_grace_secs = 1
review_timeout_secs = 60
retry_backoff_base_secs = 0
error_cooldown_secs = 1

[adapters.fake]
command = ["sh", "{script}"]

[[providers]]
id = "fake-local"
adapter = "fake"
tiers = ["frontier", "standard", "cheap"]
concurrency = 2
model = "fake"

{clusters}
"#,
                script = script.display(),
            ),
        )
        .unwrap();
        path
    }

    fn taskctl(&self, args: &[&str]) -> String {
        let out = Command::new(bin("taskctl")).arg("--db").arg(&self.db).args(args).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "taskctl {args:?}: {stdout}{}", String::from_utf8_lossy(&out.stderr));
        stdout
    }

    /// クラスタ側のパスを指すタスクを作って承認する（ADR-0018: `taskctl add --cluster`）。
    fn add_remote(&self, title: &str, cluster: &str, remote: &Path, check: &str) -> TaskId {
        let id: TaskId = self
            .taskctl(&[
                "add", "--title", title, "--objective", "cluster task", "--check-cmd", check,
                "--cluster", cluster, "--workspace", remote.to_str().unwrap(),
            ])
            .trim()
            .parse()
            .unwrap();
        self.taskctl(&["approve", &id.to_string()]);
        id
    }

    fn run_taskd(&self, config: &Path, timeout: Duration) -> String {
        let log = self.root.join("taskd.log");
        let mut child = Command::new(bin("taskd"))
            .args(["--config", config.to_str().unwrap(), "--until-idle", "--max-ticks", "2000", "--log-format", "text"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
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
}

/// 受け入れ 1: pull → run → push → クラスタでの判定 → pull。クラスタにしか無いファイルを使う条件が通る。
#[test]
fn remote_task_syncs_runs_and_is_checked_on_the_cluster() {
    if !control_master_alive(HOST) {
        eprintln!("skip: {HOST} への多重接続が無い");
        return;
    }
    let env = Env::new();
    let script = env.write_script();
    let remote = env.root.join("cluster-project");
    std::fs::create_dir_all(&remote).unwrap();
    std::fs::write(remote.join("secret.txt"), "cluster-only\n").unwrap();
    std::fs::write(remote.join("keep.txt"), "do not delete\n").unwrap();

    let config = env.write_config(
        &script,
        &format!("[[clusters]]\nid = \"local\"\nhost = \"{HOST}\"\nconcurrency = 2\n"),
    );
    // 判定コマンドはクラスタ側で走る（答えのファイルが push されていることを確かめる）。
    let id = env.add_remote("cluster work", "local", &remote, "grep -q cluster-only answer.txt");

    env.run_taskd(&config, Duration::from_secs(120));

    let t = env.task(id);
    assert_eq!((t.status, t.attempts), (Status::Done, 0), "{:?}", env.events(id));
    assert!(remote.join("answer.txt").exists(), "push でクラスタ側に届く");
    assert!(remote.join("keep.txt").exists(), "既定の push は既存ファイルを消さない");
    let mirror = env.root.join("workspaces").join(id.to_string());
    assert!(mirror.join("secret.txt").exists(), "pull でクラスタの内容が写しに来る");
}

/// 受け入れ 2: 多重接続が無いクラスタは dispatch されず、`ClusterUnavailable` が残り、`--until-idle` は止まる。
#[test]
fn missing_control_master_records_cluster_unavailable_and_does_not_block_idle() {
    let env = Env::new();
    let script = env.write_script();
    let remote = env.root.join("unreachable-project");
    std::fs::create_dir_all(&remote).unwrap();
    let config = env.write_config(
        &script,
        "[[clusters]]\nid = \"offline\"\nhost = \"taskd-no-such-host-for-tests\"\nconcurrency = 1\n",
    );
    let id = env.add_remote("offline work", "offline", &remote, "true");

    env.run_taskd(&config, Duration::from_secs(60));

    let t = env.task(id);
    assert_eq!((t.status, t.attempts), (Status::Ready, 0), "接続が無い間は ready のまま（attempts も消費しない）");
    assert!(
        env.events(id).iter().any(|e| matches!(e, Event::ClusterUnavailable { cluster, .. } if cluster == "offline")),
        "{:?}",
        env.events(id)
    );
}

/// 受け入れ 5: 設定に無いクラスタは経路なし（`--until-idle` を止めない）。
#[test]
fn unknown_cluster_is_unroutable() {
    let env = Env::new();
    let script = env.write_script();
    let remote = env.root.join("nowhere");
    std::fs::create_dir_all(&remote).unwrap();
    let config = env.write_config(&script, "");
    let id = env.add_remote("no cluster", "does-not-exist", &remote, "true");

    let log = env.run_taskd(&config, Duration::from_secs(60));

    assert_eq!(env.task(id).status, Status::Ready);
    assert!(log.contains("no such cluster in the config"), "{log}");
}
