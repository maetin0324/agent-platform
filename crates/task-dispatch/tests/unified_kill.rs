//! ADR-0044 §5「Phase 53 追記」（Phase 55）: **run の止め方を 1 つにする**。
//!
//! 走っている run を止める道は 5 つある（`cancel` / 人のコメントによる割り込み `Interrupt` /
//! 実時間・無入力のタイムアウト / リース喪失 / ADR-0040 の drain タイムアウト）。どれも
//! **ワーカーのプロセスグループに SIGTERM → `kill_grace_secs` → SIGKILL** で止まり、
//! ハーネスが起こした**孫**（ここでは `sleep 300`）まで消えることを見る。
//!
//! Phase 54 までは `cancel` と `Interrupt` が tokio の `JoinHandle::abort()` だけで、
//! `kill_on_drop` が**直接の子**を SIGKILL するにとどまり、孫は生き残っていた。
//!
//! 外部ネットワークには出ない（`sh` と `sleep` だけ）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use task_core::{
    Budget, Check, Criterion, DelegationLimits, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, Tier, Trigger,
    WorkerHint, WorkspaceSpec,
};
use task_dispatch::dispatcher::{DispatchConfig, Dispatcher};
use task_dispatch::policy::{ProviderSpec, StaticPolicy};
use task_worker::{FakeAdapter, WorkerAdapter};
use time::OffsetDateTime;

/// 孫（`sleep 300`）を起こしてその pid をファイルに書き、自分は終端メッセージを出さずに待ち続ける
/// ワーカー。run を外から止める以外に終わる道は無い。
fn worker_that_spawns_a_grandchild(pidfile: &Path) -> Arc<dyn WorkerAdapter> {
    let script = format!(
        "cat >/dev/null; sleep 300 & echo $! > {pid}; \
         echo '{{\"type\":\"progress\",\"msg\":\"started\"}}'; wait",
        pid = pidfile.display()
    );
    Arc::new(FakeAdapter::new(vec!["sh".into(), "-c".into(), script]))
}

fn new_task(dir: &Path) -> Task {
    let now = OffsetDateTime::now_utc();
    Task {
        repos: Vec::new(),
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: "long run".into(),
        objective: "sleep".into(),
        acceptance: vec![Criterion {
            text: "c".into(),
            check: Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        }],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Ready,
        priority: 0,
        worker_hint: WorkerHint {
            tier: Tier::Standard,
            adapter: None,
        },
        workspace: WorkspaceSpec::Local {
            path: dir.to_path_buf(),
            mode: None,
        },
        // 実時間の上限は、タイムアウトの経路を見るテストだけが短くする（下の `wall_secs`）。
        budget: Budget {
            max_turns: 1,
            max_wall_secs: 300,
            max_retries: 0,
        },
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
        labels: Vec::new(),
        category: Default::default(),
    }
}

fn dispatcher(store: Arc<dyn TaskStore>, adapter: Arc<dyn WorkerAdapter>, workspace_root: PathBuf) -> Dispatcher {
    let policy = StaticPolicy::new(
        vec![ProviderSpec {
            id: "p1".into(),
            adapter: "fake".into(),
            tiers: vec![Tier::Frontier, Tier::Standard, Tier::Cheap],
            concurrency: 2,
            model: "m".into(),
        }],
        Duration::from_secs(1),
    );
    let mut adapters: HashMap<String, Arc<dyn WorkerAdapter>> = HashMap::new();
    adapters.insert("p1".into(), adapter);
    Dispatcher::new(
        store,
        Box::new(policy),
        HashMap::from([("p1".to_string(), "m".to_string())]),
        adapters,
        HashSet::new(),
        DispatchConfig {
            max_concurrency: 2,
            lease_grace: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(30),
            // ADR-0044 Phase 53 追記: SIGTERM から SIGKILL までの猶予。テストは短く。
            kill_grace: Duration::from_millis(200),
            review_timeout: Duration::from_secs(5),
            workspace_root,
            plan_auto_accept: false,
            retry_backoff_base: Duration::ZERO,
            retry_backoff_max: Duration::ZERO,
            reviewer_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            clusters: HashMap::new(),
            cluster_cooldown: Duration::from_secs(1),
            max_requeues: 5,
            roles: Vec::new(),
            genres: Vec::new(),
            delegation: DelegationLimits::default(),
            accounts: None,
            memory_dir: None,
            worktree_branch_prefix: task_worker::DEFAULT_BRANCH_PREFIX.to_string(),
            releases_dir: None,
        },
    )
}

fn pid_alive(pid: i32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// 孫が pid を書くまで待つ（上限付き）。
async fn wait_for_grandchild(path: &Path) -> i32 {
    for _ in 0..400 {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the worker never spawned its grandchild ({})", path.display());
}

async fn wait_until_gone(pid: i32) -> bool {
    for _ in 0..200 {
        if !pid_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// `trigger` でタスクを非 `running` にしてから tick を回し、孫が消えることを見る共通の本体。
async fn stopping_a_run_kills_the_grandchild(trigger: Trigger) {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let pidfile = dir.path().join("grandchild.pid");
    let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap_or_else(|e| panic!("{e}")));
    let task = new_task(dir.path());
    store.insert(&task).unwrap_or_else(|e| panic!("{e}"));

    let mut d = dispatcher(
        store.clone(),
        worker_that_spawns_a_grandchild(&pidfile),
        dir.path().to_path_buf(),
    );
    // 1 tick で dispatch される。
    let report = d.tick().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report.dispatched, 1, "the task should have been dispatched");

    let grandchild = wait_for_grandchild(&pidfile).await;
    assert!(pid_alive(grandchild), "the grandchild should be running");

    // 人が止める（`cancel` / コメントによる `Interrupt`）。API と同じく DB の状態だけを変える。
    store
        .apply_transition(task.id, trigger, None)
        .unwrap_or_else(|e| panic!("{trigger:?}: {e}"));

    // 次の tick で `abort_stale_runs` が気付き、プロセスグループごと止める。
    d.tick().unwrap_or_else(|e| panic!("{e}"));
    assert!(
        wait_until_gone(grandchild).await,
        "the grandchild {grandchild} survived {trigger:?} (process group was not signalled)"
    );
}

/// `POST /tasks/{id}/cancel` の経路。
#[tokio::test]
async fn cancel_kills_the_worker_process_group_including_grandchildren() {
    stopping_a_run_kills_the_grandchild(Trigger::Cancel).await;
}

/// ADR-0044 D2 の人のコメントによる割り込み（`running → ready`）の経路。
#[tokio::test]
async fn interrupt_kills_the_worker_process_group_including_grandchildren() {
    stopping_a_run_kills_the_grandchild(Trigger::Interrupt).await;
}

/// 実時間の上限（`max_wall_secs`）の経路。こちらは `task_worker::subprocess` の中で
/// 同じ手順（グループへ SIGTERM → `kill_grace` → SIGKILL）を踏む。
#[tokio::test]
async fn the_wall_clock_timeout_kills_the_worker_process_group_including_grandchildren() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let pidfile = dir.path().join("grandchild.pid");
    let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap_or_else(|e| panic!("{e}")));
    let mut task = new_task(dir.path());
    task.budget.max_wall_secs = 1;
    store.insert(&task).unwrap_or_else(|e| panic!("{e}"));

    let mut d = dispatcher(
        store.clone(),
        worker_that_spawns_a_grandchild(&pidfile),
        dir.path().to_path_buf(),
    );
    assert_eq!(d.tick().unwrap_or_else(|e| panic!("{e}")).dispatched, 1);
    let grandchild = wait_for_grandchild(&pidfile).await;

    // 上限を過ぎたら run は自分で終わる。孫も一緒に消える。
    for _ in 0..40 {
        d.tick().unwrap_or_else(|e| panic!("{e}"));
        if !pid_alive(grandchild) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        wait_until_gone(grandchild).await,
        "the grandchild {grandchild} survived the wall-clock timeout"
    );
}

/// ADR-0040 の drain タイムアウト（`abort_all_runs`）も同じ止め方。
#[tokio::test]
async fn the_drain_timeout_kills_the_worker_process_group_including_grandchildren() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let pidfile = dir.path().join("grandchild.pid");
    let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap_or_else(|e| panic!("{e}")));
    let task = new_task(dir.path());
    store.insert(&task).unwrap_or_else(|e| panic!("{e}"));

    let mut d = dispatcher(
        store.clone(),
        worker_that_spawns_a_grandchild(&pidfile),
        dir.path().to_path_buf(),
    );
    assert_eq!(d.tick().unwrap_or_else(|e| panic!("{e}")).dispatched, 1);
    let grandchild = wait_for_grandchild(&pidfile).await;

    assert_eq!(d.abort_all_runs(), 1);
    assert!(
        wait_until_gone(grandchild).await,
        "the grandchild {grandchild} survived the drain timeout"
    );
}
