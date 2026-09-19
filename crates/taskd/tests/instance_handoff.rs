//! ADR-0040 D4（Phase 47）: 1 つの SQLite に対して 2 つの taskd を**同じプロセスの中で**起こし、
//! ライブ引き継ぎ（新 standby → 旧 draining → 新 active）が実際に起きることを見る。
//!
//! 見るもの（ADR-0040 §4 の受け入れ条件）:
//! - (a) 新しいリリースは `standby` で起き、旧が `draining` になり、新が `active` になる。
//! - (b) `draining` の手元の run は**旧が最後まで面倒を見**、新は二重に dispatch しない
//!   （`WorkerStarted` はそのタスクに 1 回だけ）。旧は run が 0 になったら `drained_at` を書いて exit 0。
//! - (c) `--mode verify` は ready なタスクを dispatch せず、`daemon_instances` にも書かない。
//! - (d) 同じ `release` の二重起動は exit 3（バイナリごと確かめる）。
//! - (e) `active` の heartbeat が止まれば `standby` が `active` になる。
//! - (g) `SO_REUSEPORT`: 同じポートに 2 つの listener が bind できる。
//!
//! 外部ネットワークには出ない（ワーカーは `sh` の偽アダプタ、API は loopback）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use task_core::{
    Budget, Check, Criterion, Event, InstanceRole, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, Tier,
    WorkerHint, WorkspaceSpec,
};
use taskd::{Config, Exit, RunOptions};
use time::OffsetDateTime;

/// 偽のワーカー: 2 秒眠ってから `done` を 1 行返す（引き継ぎの間ずっと走っている run を作るため）。
/// TOML には**リテラル文字列**（`'...'`）として書くので、ここの `\"` はそのまま `sh` に渡る。
const SLOW_FAKE: &str =
    r#"cat >/dev/null; sleep 2; printf "{\"type\":\"done\",\"summary\":\"fake\",\"evidence\":[]}\n""#;

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    config_path: PathBuf,
    db: PathBuf,
}

impl Env {
    fn new() -> Self {
        Self::with_extra("")
    }

    fn with_extra(extra: &str) -> Self {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("ws")).unwrap_or_else(|e| panic!("ws: {e}"));
        let config_path = root.join("taskd.toml");
        std::fs::write(
            &config_path,
            format!(
                r#"
db = "taskd.sqlite3"
workspace_root = "ws"
tick_ms = 100
max_concurrency = 2
lease_grace_secs = 4
idle_timeout_secs = 30
kill_grace_secs = 1

[handoff]
drain_timeout_secs = 60

[adapters.fake]
command = ["sh", "-c", '{SLOW_FAKE}']

[[providers]]
id = "p1"
adapter = "fake"
{extra}
"#
            ),
        )
        .unwrap_or_else(|e| panic!("config: {e}"));
        let db = root.join("taskd.sqlite3");
        Self { _dir: dir, root, config_path, db }
    }

    fn config(&self) -> Config {
        Config::load(&self.config_path).unwrap_or_else(|e| panic!("load: {e}"))
    }

    fn store(&self) -> SqliteStore {
        SqliteStore::open(&self.db).unwrap_or_else(|e| panic!("open: {e}"))
    }

    /// ready なタスクを 1 件置く（受け入れ条件はコマンドなので、レビューに LLM は要らない）。
    fn ready_task(&self, title: &str) -> TaskId {
        let ws = self.root.join("ws").join(title);
        std::fs::create_dir_all(&ws).unwrap_or_else(|e| panic!("ws: {e}"));
        let now = OffsetDateTime::now_utc();
        let task = Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: title.into(),
            objective: "phase 47 handoff".into(),
            acceptance: vec![Criterion {
                text: "true".into(),
                check: Check::Command { cmd: "true".into(), expect_exit: 0 },
            }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: 0,
            worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
            workspace: WorkspaceSpec::Local { path: ws, mode: None },
            budget: Budget { max_turns: 4, max_wall_secs: 60, max_retries: 0 },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            conversation: None,
        };
        let store = self.store();
        store.insert(&task).unwrap_or_else(|e| panic!("insert: {e}"));
        task.id
    }
}

fn options(release: &str, max_ticks: u64, until_idle: bool) -> RunOptions {
    RunOptions {
        until_idle,
        max_ticks,
        mode: task_core::DaemonMode::Normal,
        release: Some(release.to_string()),
    }
}

/// `check` が真になるまで（最大 `limit`）待つ。
async fn wait_until(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if check() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn worker_starts(store: &SqliteStore, task_id: TaskId) -> usize {
    store
        .events_for(task_id)
        .unwrap_or_else(|e| panic!("events: {e}"))
        .iter()
        .filter(|(_, e)| matches!(e, Event::WorkerStarted { role: None, .. }))
        .count()
}

fn role_of(store: &SqliteStore, release: &str) -> Option<InstanceRole> {
    store
        .instance_list()
        .unwrap_or_else(|e| panic!("instances: {e}"))
        .into_iter()
        .find(|i| i.release == release)
        .map(|i| i.role)
}

/// (a) 新しいリリースが standby で起き、旧が draining になり、新が active になる。
/// (b) draining の手元の run は旧が完了させ、新は二重に dispatch しない。旧は exit 0（`Exit::Drained`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_newer_release_takes_over_while_the_old_one_finishes_its_run() {
    let env = Env::new();
    let task_id = env.ready_task("handoff");
    let store = env.store();

    // 旧（release = "old"）を起こす。ready なタスクを 1 件 dispatch する。
    let old = tokio::spawn(taskd::run(env.config(), options("old", 400, false)));
    assert!(
        wait_until(Duration::from_secs(10), || {
            store.get(task_id).ok().flatten().map(|t| t.status) == Some(Status::Running)
        })
        .await,
        "旧インスタンスがタスクを dispatch しない"
    );
    assert_eq!(role_of(&store, "old"), Some(InstanceRole::Active));
    let lease_before = store
        .get(task_id)
        .unwrap_or_else(|e| panic!("get: {e}"))
        .and_then(|t| t.lease)
        .unwrap_or_else(|| panic!("running task must hold a lease"));

    // 新（release = "new"）を起こす。idle になったら自分で止まる。
    let new = tokio::spawn(taskd::run(env.config(), options("new", 400, true)));

    // (a) 新は standby → 旧が draining → 新が active。
    assert!(
        wait_until(Duration::from_secs(10), || role_of(&store, "old") == Some(InstanceRole::Draining)).await,
        "旧が draining にならない"
    );
    assert!(
        wait_until(Duration::from_secs(10), || role_of(&store, "new") == Some(InstanceRole::Active)).await,
        "新が active にならない"
    );

    // (b) 旧は手元の run を最後まで面倒を見て、drained_at を書いて exit 0 する。
    let old_exit = tokio::time::timeout(Duration::from_secs(30), old)
        .await
        .unwrap_or_else(|_| panic!("旧インスタンスが drain で終わらない"))
        .unwrap_or_else(|e| panic!("join: {e}"))
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_eq!(old_exit, Exit::Drained);

    let new_exit = tokio::time::timeout(Duration::from_secs(30), new)
        .await
        .unwrap_or_else(|_| panic!("新インスタンスが idle で終わらない"))
        .unwrap_or_else(|e| panic!("join: {e}"))
        .unwrap_or_else(|e| panic!("run: {e}"));
    assert_eq!(new_exit, Exit::Idle);

    // run を起こしたのは 1 回だけ（新 active はリースが生きている running を二重に dispatch しない）。
    assert_eq!(worker_starts(&store, task_id), 1, "run が二重に起きた");
    let finished = store
        .events_for(task_id)
        .unwrap_or_else(|e| panic!("events: {e}"))
        .into_iter()
        .filter_map(|(_, e)| match e {
            Event::WorkerFinished { run_id, outcome, role: None, .. } => Some((run_id, outcome)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(finished.len(), 1, "{finished:?}");
    assert_eq!(finished[0].0, lease_before.worker_run_id, "旧が起こした run が旧の手で終わっている");
    assert!(finished[0].1.starts_with("done"), "{finished:?}");
    let status = store.get(task_id).unwrap_or_else(|e| panic!("get: {e}")).map(|t| t.status);
    assert_eq!(status, Some(Status::Done));
}

/// (c) `--mode verify` は ready なタスクを dispatch せず、`daemon_instances` にも書かない
/// （マイグレーションは適用される）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_mode_never_dispatches_and_never_touches_daemon_instances() {
    let env = Env::new();
    let task_id = env.ready_task("verify");
    let store = env.store();

    let exit = taskd::run(
        env.config(),
        RunOptions {
            until_idle: false,
            max_ticks: 5,
            mode: task_core::DaemonMode::Verify,
            release: Some("verify-release".into()),
        },
    )
    .await
    .unwrap_or_else(|e| panic!("run: {e}"));
    assert_eq!(exit, Exit::MaxTicks);

    assert_eq!(
        store.get(task_id).unwrap_or_else(|e| panic!("get: {e}")).map(|t| t.status),
        Some(Status::Ready),
        "verify は ready なタスクに触れない"
    );
    assert_eq!(worker_starts(&store, task_id), 0, "verify はワーカーを起こさない");
    assert!(
        store.instance_list().unwrap_or_else(|e| panic!("instances: {e}")).is_empty(),
        "verify は daemon_instances に行を書かない"
    );
    // マイグレーションは適用されている（`SCHEMA_VERSION` まで上がっている）。
    assert_eq!(store.schema_version().unwrap_or_else(|e| panic!("schema: {e}")), task_core::SCHEMA_VERSION);
}

/// (e) `active` の heartbeat が止まれば `standby` が `active` になる（旧の行も消える）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_heartbeat_promotes_the_standby() {
    let env = Env::new();
    let store = env.store();
    // 「生きているが heartbeat を打たない active」を手で置く（pid はこのテストプロセス自身なので
    // `pid_alive` は真になり、standby になる経路を通る）。
    let now = OffsetDateTime::now_utc();
    store
        .instance_register(&task_core::DaemonInstance {
            instance_id: "ghost".into(),
            release: "ghost-release".into(),
            pid: std::process::id(),
            role: InstanceRole::Active,
            started_at: now,
            heartbeat_at: now,
            handoff_requested_at: None,
            drained_at: None,
        })
        .unwrap_or_else(|e| panic!("register: {e}"));

    let new = tokio::spawn(taskd::run(env.config(), options("fresh", 200, false)));
    // 最初は standby（`ghost` の heartbeat がまだ新しい）。
    assert!(
        wait_until(Duration::from_secs(5), || role_of(&store, "fresh") == Some(InstanceRole::Standby)).await,
        "新しいインスタンスが standby にならない"
    );
    assert!(
        store
            .instance_list()
            .unwrap_or_else(|e| panic!("instances: {e}"))
            .iter()
            .find(|i| i.instance_id == "ghost")
            .and_then(|i| i.handoff_requested_at)
            .is_some(),
        "standby は active に引き継ぎを要求する"
    );
    // `ghost` は heartbeat を打たないので、3 × tick + lease_grace（= 4.3 秒）で古くなる。
    assert!(
        wait_until(Duration::from_secs(15), || role_of(&store, "fresh") == Some(InstanceRole::Active)).await,
        "heartbeat が止まった active を置き換えられない"
    );
    assert!(
        store
            .instance_list()
            .unwrap_or_else(|e| panic!("instances: {e}"))
            .iter()
            .all(|i| i.instance_id != "ghost"),
        "死んだ行は消える"
    );
    new.abort();
}

/// (d) 同じ `release` の二重起動は exit 3（バイナリで確かめる）。違う `release` なら standby として上がる。
#[test]
fn starting_the_same_release_twice_exits_three() {
    let env = Env::new();
    let store = env.store();
    let now = OffsetDateTime::now_utc();
    store
        .instance_register(&task_core::DaemonInstance {
            instance_id: "already".into(),
            release: "sha12sha12ab".into(),
            pid: std::process::id(),
            role: InstanceRole::Active,
            started_at: now,
            heartbeat_at: now,
            handoff_requested_at: None,
            drained_at: None,
        })
        .unwrap_or_else(|e| panic!("register: {e}"));

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_taskd"))
        .args([
            "--config",
            env.config_path.to_str().unwrap_or_default(),
            "--release",
            "sha12sha12ab",
            "--max-ticks",
            "1",
            "--log-format",
            "text",
        ])
        .output()
        .unwrap_or_else(|e| panic!("spawn: {e}"));
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        store.instance_list().unwrap_or_else(|e| panic!("instances: {e}")).len(),
        1,
        "二重起動は行を増やさない"
    );
}

/// ADR-0040 D3: CLI の上書き（`--db` / `--listen` / `--workspace-root` / `--token-file`）は設定の後に効き、
/// 相対パスは設定ファイルのディレクトリ基準で解決される。
#[test]
fn the_cli_overrides_replace_the_config_values() {
    let env = Env::new();
    let mut config = env.config();
    assert_eq!(config.db, env.root.join("taskd.sqlite3"));
    assert_eq!(config.api.listen, None);
    std::fs::write(env.root.join("api.token"), "s3cret").unwrap_or_else(|e| panic!("token: {e}"));
    config.apply_overrides(&taskd::Overrides {
        db: Some(PathBuf::from("staging.sqlite3")),
        listen: Some("127.0.0.1:7711".parse().unwrap_or_else(|e| panic!("{e}"))),
        workspace_root: Some(PathBuf::from("staging-ws")),
        token_file: Some(PathBuf::from("api.token")),
    });
    assert_eq!(config.db, env.root.join("staging.sqlite3"));
    assert_eq!(config.workspace_root, env.root.join("staging-ws"));
    assert_eq!(config.api.listen.map(|a| a.to_string()).as_deref(), Some("127.0.0.1:7711"));
    assert_eq!(config.api.token_file, Some(env.root.join("api.token")));
    assert_eq!(config.api.read_token().unwrap_or_else(|e| panic!("{e}")).as_deref(), Some("s3cret"));
    // 絶対パスはそのまま。
    let elsewhere = Path::new("/tmp/taskd-absolute.sqlite3");
    config.apply_overrides(&taskd::Overrides { db: Some(elsewhere.to_path_buf()), ..Default::default() });
    assert_eq!(config.db, elsewhere);
    // 既定は `[handoff] drain_timeout_secs = 3600`（この設定では 60 に上書きしてある）。
    assert_eq!(config.drain_timeout(), Duration::from_secs(60));
    assert_eq!(taskd::config::HandoffConfig::default().drain_timeout_secs, 3600);
}

/// (g) `SO_REUSEPORT`: 同じポートに 2 つの listener が bind できる（新旧が並ぶ間、カーネルが振り分ける）。
#[tokio::test]
async fn two_listeners_bind_the_same_port_with_so_reuseport() {
    let first = taskd::bind_reuseport("127.0.0.1:0".parse().unwrap_or_else(|e| panic!("{e}")))
        .unwrap_or_else(|e| panic!("first bind: {e}"));
    let addr = first.local_addr().unwrap_or_else(|e| panic!("addr: {e}"));
    let second = taskd::bind_reuseport(addr).unwrap_or_else(|e| panic!("second bind on {addr}: {e}"));
    assert_eq!(second.local_addr().unwrap_or_else(|e| panic!("addr: {e}")), addr);
    // 従来の bind は同じポートを取れない（`SO_REUSEPORT` があってこそ並べる）。
    assert!(
        tokio::net::TcpListener::bind(addr).await.is_err(),
        "REUSEPORT 無しの bind は既に使われているポートを取れない"
    );
}
