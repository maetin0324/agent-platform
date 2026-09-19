//! `taskctl worker run` の中断（ADR-0012 D4、監査の指摘）: SIGTERM を受けたらワーカーのプロセスを kill し、exit 130 で終わる。
//! fake ワーカー（`sh` スクリプト）とローカル SQLite だけで動き、ネットワークに出ない。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use task_core::{
    Budget, Check, Criterion, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, Tier, WorkerHint, WorkspaceSpec,
};
use time::OffsetDateTime;

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

fn process_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn write_config(root: &Path, script: &Path) -> std::path::PathBuf {
    let path = root.join("taskd.toml");
    std::fs::write(
        &path,
        format!(
            r#"workspace_root = "workspaces"
lease_grace_secs = 60
kill_grace_secs = 1
idle_timeout_secs = 120

[adapters.fake]
command = ["sh", "{}"]

[[providers]]
id = "local"
adapter = "fake"
"#,
            script.display()
        ),
    )
    .unwrap();
    path
}

#[test]
fn sigterm_kills_the_worker_process_and_exits_130() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let db = root.join("taskd.sqlite3");
    let store = SqliteStore::open(&db).unwrap();
    let now = OffsetDateTime::now_utc();
    let task = Task {
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: "long".into(),
        objective: "sleep".into(),
        acceptance: vec![Criterion { text: "c".into(), check: Check::Command { cmd: "true".into(), expect_exit: 0 } }],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Ready,
        priority: 0,
        worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
        workspace: WorkspaceSpec::Local { path: ws.clone(), mode: None },
        budget: Budget { max_turns: 1, max_wall_secs: 600, max_retries: 0 },
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
    store.create_task(&task, vec![]).unwrap();
    let script = root.join("worker.sh");
    std::fs::write(&script, "cat >/dev/null\necho $$ > worker.pid\nexec sleep 60\n").unwrap();
    let config = write_config(&root, &script);

    let mut child = Command::new(env!("CARGO_BIN_EXE_taskctl"))
        .arg("--db")
        .arg(&db)
        .args(["worker", "run", "--config"])
        .arg(&config)
        .args(["--task", &task.id.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let pid_file = ws.join("worker.pid");
    assert!(wait_until(Duration::from_secs(10), || pid_file.is_file()), "worker did not start");
    let worker_pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_string();
    assert!(process_alive(&worker_pid));

    let status = Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    assert!(status.success());
    let mut exit = None;
    assert!(
        wait_until(Duration::from_secs(10), || {
            exit = child.try_wait().unwrap();
            exit.is_some()
        }),
        "taskctl did not exit after SIGTERM"
    );
    assert_eq!(exit.and_then(|s| s.code()), Some(130));
    assert!(
        wait_until(Duration::from_secs(5), || !process_alive(&worker_pid)),
        "worker process {worker_pid} survived taskctl's SIGTERM"
    );
    // DB は変えない。
    assert!(store.events_for(task.id).unwrap().len() == 1);
    assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Ready);
}
