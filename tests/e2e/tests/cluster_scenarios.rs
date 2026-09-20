//! DESIGN §6 Phase 12（ADR-0018）: クラスタでのコマンド実行を、実バイナリの `celeris` / `celerisctl` と
//! **localhost への ssh** で確かめる（外部ネットワークに出ない）。多重接続が無い環境では skip する。
//!
//! 1. `WorkspaceSpec::Remote` のタスクが、pull → run → push → クラスタでの判定 → pull を通って done になる
//! 2. 多重接続が無いクラスタのタスクは dispatch されず、`ClusterUnavailable` が残り、`--until-idle` を止めない
//! 3. 設定に無いクラスタのタスクは経路なしとして扱われる
//! 10. `celerisctl worker run --cluster <id>` が、デーモン無しでクラスタ側の作業ディレクトリに対して
//!     1 回 run する（DB は変えない）。多重接続が無ければ exit 4 と理由。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use task_core::{Event, SqliteStore, Status, Task, TaskId, TaskStore};

const HOST: &str = "celeris-localhost";

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
        let db = root.join("celeris.sqlite3");
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
        let path = self.root.join("config.toml");
        std::fs::write(
            &path,
            format!(
                r#"db = "celeris.sqlite3"
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

    fn celerisctl(&self, args: &[&str]) -> String {
        let out = Command::new(bin("celerisctl")).arg("--db").arg(&self.db).args(args).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "celerisctl {args:?}: {stdout}{}", String::from_utf8_lossy(&out.stderr));
        stdout
    }

    /// クラスタ側のパスを指すタスクを作って承認する（ADR-0018: `celerisctl add --cluster`）。
    fn add_remote(&self, title: &str, cluster: &str, remote: &Path, check: &str) -> TaskId {
        let id: TaskId = self
            .celerisctl(&[
                "add", "--title", title, "--objective", "cluster task", "--check-cmd", check,
                "--cluster", cluster, "--workspace", remote.to_str().unwrap(),
            ])
            .trim()
            .parse()
            .unwrap();
        self.celerisctl(&["approve", &id.to_string()]);
        id
    }

    fn run_celeris(&self, config: &Path, timeout: Duration) -> String {
        let log = self.root.join("celeris.log");
        let mut child = Command::new(bin("celeris"))
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
                assert!(status.success(), "celeris exited with {status}\n{text}");
                return text;
            }
            if start.elapsed() > timeout {
                let _ = child.kill();
                let text = std::fs::read_to_string(&log).unwrap_or_default();
                panic!("celeris did not reach idle within {timeout:?}\n{text}");
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

    env.run_celeris(&config, Duration::from_secs(120));

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
        "[[clusters]]\nid = \"offline\"\nhost = \"celeris-no-such-host-for-tests\"\nconcurrency = 1\n",
    );
    let id = env.add_remote("offline work", "offline", &remote, "true");

    env.run_celeris(&config, Duration::from_secs(60));

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

    let log = env.run_celeris(&config, Duration::from_secs(60));

    assert_eq!(env.task(id).status, Status::Ready);
    assert!(log.contains("no such cluster in the config"), "{log}");
}

/// 受け入れ 10: `celerisctl worker run --cluster` はデーモン無しでクラスタ側のディレクトリに対して
/// 1 回だけ run する。pull でクラスタのファイルが写しに来て、push で編集結果がクラスタに届く。
/// DB は一切変わらない（status/attempts/events の件数が実行前後で不変）。
#[test]
fn worker_run_cluster_executes_one_run_against_the_cluster_dir() {
    if !control_master_alive(HOST) {
        eprintln!("skip: {HOST} への多重接続が無い");
        return;
    }
    let env = Env::new();
    let script = env.write_script();
    let remote = env.root.join("worker-run-cluster-project");
    std::fs::create_dir_all(&remote).unwrap();
    std::fs::write(remote.join("secret.txt"), "cluster-only\n").unwrap();

    let config = env.write_config(
        &script,
        &format!("[[clusters]]\nid = \"local\"\nhost = \"{HOST}\"\nconcurrency = 2\n"),
    );
    let id = env.add_remote("worker run cluster", "local", &remote, "true");

    let task_before = env.task(id);
    let events_before = env.events(id);

    let out = Command::new(bin("celerisctl"))
        .arg("--db")
        .arg(&env.db)
        .args([
            "worker",
            "run",
            "--config",
            config.to_str().unwrap(),
            "--task",
            &id.to_string(),
            "--cluster",
            "local",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "stdout: {stdout}\nstderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("\"type\":\"done\""), "{stdout}");
    assert!(stdout.contains("cluster=local"), "{stdout}");
    assert!(remote.join("answer.txt").exists(), "push でクラスタ側に answer.txt が届く");

    let mirror = env.root.join("workspaces").join(id.to_string());
    assert!(mirror.join("secret.txt").exists(), "pull でクラスタの内容が写しに来る");

    let task_after = env.task(id);
    let events_after = env.events(id);
    assert_eq!((task_after.status, task_after.attempts), (task_before.status, task_before.attempts));
    assert_eq!(events_after.len(), events_before.len(), "DB のイベントは増えない");
}

/// 受け入れ 10: 多重接続が無いクラスタでは、アダプタを起動せず exit 4 と理由（ssh 接続は不要なので常に走る）。
#[test]
fn worker_run_cluster_without_control_master_exits_4() {
    let env = Env::new();
    let script = env.write_script();
    let remote = env.root.join("offline-worker-run-project");
    std::fs::create_dir_all(&remote).unwrap();
    let config = env.write_config(
        &script,
        "[[clusters]]\nid = \"offline\"\nhost = \"celeris-no-such-host-for-tests\"\nconcurrency = 1\n",
    );
    let id = env.add_remote("offline worker run", "offline", &remote, "true");

    let out = Command::new(bin("celerisctl"))
        .arg("--db")
        .arg(&env.db)
        .args([
            "worker",
            "run",
            "--config",
            config.to_str().unwrap(),
            "--task",
            &id.to_string(),
            "--cluster",
            "offline",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(4), "stdout: {stdout}\nstderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("ControlMaster"), "{stdout}");
}

/// ADR-0019: `sync = "worktree"` を実バイナリで通す。クラスタ側のリポジトリから worktree を切り、
/// **追跡ファイルだけ**を写して run し、判定は worktree の中で走り、元のリポジトリの作業ツリーは変わらない。
#[test]
fn worktree_cluster_runs_in_a_worktree_and_leaves_the_repository_alone() {
    if !control_master_alive(HOST) {
        eprintln!("skip: {HOST} への多重接続が無い");
        return;
    }
    let env = Env::new();
    // fake ワーカー: 追跡ファイルを編集する（判定はクラスタ側の worktree でこの編集を見る）。
    let script = env.root.join("fake-editor.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nset -u\ncat >/dev/null\necho edited > tracked.txt\necho '{\"type\":\"done\",\"summary\":\"edited\",\"evidence\":[]}'\n",
    )
    .unwrap();

    // クラスタ側のリポジトリ: 追跡ファイル 1 件と、巨大データに見立てた未追跡ファイル 1 件。
    let repo = env.root.join("cluster-repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("tracked.txt"), "original\n").unwrap();
    std::fs::write(repo.join("untracked-huge.bin"), "x".repeat(4096)).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "celeris@example.com"],
        vec!["config", "user.name", "celeris"],
        vec!["add", "tracked.txt"],
        vec!["commit", "-q", "-m", "initial"],
    ] {
        let out = Command::new("git").args(&args).current_dir(&repo).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    let config = env.write_config(
        &script,
        &format!("[[clusters]]\nid = \"local\"\nhost = \"{HOST}\"\nconcurrency = 1\nsync = \"worktree\"\n"),
    );
    let id = env.add_remote("worktree work", "local", &repo, "grep -q edited tracked.txt");

    env.run_celeris(&config, Duration::from_secs(120));

    let t = env.task(id);
    assert_eq!((t.status, t.attempts), (Status::Done, 0), "{:?}", env.events(id));

    // 手元の写しには追跡ファイルだけが来る。
    let mirror = env.root.join("workspaces").join(id.to_string());
    assert!(mirror.join("tracked.txt").exists(), "追跡ファイルは写しに来る");
    assert!(!mirror.join("untracked-huge.bin").exists(), "未追跡の巨大データは持ち込まれない");

    // 編集は worktree のブランチにだけ入る。元のリポジトリの作業ツリーは変わらない（ADR-0019 D3）。
    let worktree = repo.join(".celeris-worktrees").join(id.to_string());
    assert_eq!(std::fs::read_to_string(worktree.join("tracked.txt")).unwrap().trim(), "edited");
    assert_eq!(std::fs::read_to_string(repo.join("tracked.txt")).unwrap(), "original\n", "元のリポジトリは触らない");
    let branch = Command::new("git").args(["rev-parse", "--abbrev-ref", "HEAD"]).current_dir(&worktree).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), format!("celeris/{id}"));
    // celeris は commit しない（変更は作業ツリーに残る。ADR-0019 D2）。
    let status = Command::new("git").args(["status", "--porcelain"]).current_dir(&worktree).output().unwrap();
    assert!(String::from_utf8_lossy(&status.stdout).contains("tracked.txt"), "commit せず作業ツリーに残す");

    // 後片付け（本番では人の操作。テストの tempdir は消えるが、worktree の登録を残さないため）。
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force", worktree.to_str().unwrap()])
        .current_dir(&repo)
        .output();
}
