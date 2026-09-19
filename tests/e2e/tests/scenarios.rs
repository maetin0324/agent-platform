//! DESIGN §6 Phase 3 の受け入れ 3 シナリオ（ADR-0005 D8）。fake ワーカー（`sh` スクリプト）と
//! 実バイナリ `taskd` だけで動き、ネットワークに出ない。
//!
//! 1. 3 タスク（うち 1 つは依存あり）を並列度 2 で処理し、全て `done`
//! 2. `Command` チェックが失敗したタスクが 1 回リトライされ 2 回目で `done`
//! 3. リース期限切れタスクが回収される

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use task_core::{
    Budget, Check, Criterion, Event, Lease, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, Tier, Trigger,
    WorkerHint, WorkspaceSpec,
};
use time::OffsetDateTime;

/// `target/debug/<name>`（テスト実行ファイルの 2 つ上）。`cargo test --workspace` で
/// `crates/taskd/tests` と `crates/taskctl/tests` の統合テストがバイナリのビルドを強制する。
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

    fn write_script(&self, body: &str) -> PathBuf {
        let path = self.root.join("fake-worker.sh");
        std::fs::write(&path, format!("#!/bin/sh\nset -u\n{body}\n")).unwrap();
        path
    }

    fn write_config(&self, max_concurrency: usize, script: &Path, extra: &str) -> PathBuf {
        let path = self.root.join("taskd.toml");
        let text = format!(
            r#"db = "taskd.sqlite3"
workspace_root = "workspaces"
tick_ms = 50
max_concurrency = {max_concurrency}
lease_grace_secs = 60
idle_timeout_secs = 30
kill_grace_secs = 1
retry_backoff_base_secs = 0
review_timeout_secs = 30
{extra}
[adapters.fake]
command = ["sh", "{script}"]

[[providers]]
id = "fake-local"
adapter = "fake"
tiers = ["frontier", "standard", "cheap"]
concurrency = {max_concurrency}
model = "fake"
"#,
            script = script.display()
        );
        std::fs::write(&path, text).unwrap();
        path
    }

    fn workspace_for(&self, name: &str) -> PathBuf {
        let dir = self.root.join("workspaces").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `taskctl add` → `approve` と同じ経路（insert + Created、Accept）で ready にする。
    fn add_ready_task(&self, title: &str, dir: &Path, checks: Vec<Check>, depends_on: Vec<TaskId>, max_retries: u32) -> TaskId {
        let now = OffsetDateTime::now_utc();
        let task = Task {
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: title.into(),
            objective: format!("e2e: {title}"),
            acceptance: checks.into_iter().map(|check| Criterion { text: "e2e".into(), check }).collect(),
            inputs: vec![],
            depends_on,
            status: Status::Draft,
            priority: 0,
            worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
            workspace: WorkspaceSpec::Local { path: dir.to_path_buf(), mode: None },
            budget: Budget { max_turns: 5, max_wall_secs: 20, max_retries },
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
        self.store.insert(&task).unwrap();
        self.store.append_event(task.id, &Event::Created { task: Box::new(task.clone()) }).unwrap();
        self.store.apply_transition(task.id, Trigger::Accept, None).unwrap();
        task.id
    }

    fn run_taskd(&self, config: &Path, timeout: Duration) -> String {
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

    fn replay_is_consistent(&self) {
        let out = Command::new(bin("taskctl"))
            .args(["--db", self.db.to_str().unwrap(), "replay"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "taskctl replay failed: {stdout}{}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout.contains("replay: 0 mismatches"), "{stdout}");
    }

    fn task(&self, id: TaskId) -> Task {
        self.store.get(id).unwrap().unwrap()
    }

    fn transitions(&self, id: TaskId) -> Vec<String> {
        self.store
            .events_for(id)
            .unwrap()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::Transitioned { from, to, reason } => Some(format!("{from:?}->{to:?}:{reason}")),
                _ => None,
            })
            .collect()
    }
}

/// シナリオ 1: 3 タスク（C は A に依存）、並列度 2、全て done。
#[test]
fn three_tasks_with_dependency_all_done_at_concurrency_two() {
    let env = Env::new();
    let timeline = env.root.join("timeline.log");
    // 起動・終了時刻を共有ログに記録し、成果物を作って done を返す。
    let script = env.write_script(&format!(
        r#"ID=$(cat | grep -o '"id":"[A-Z0-9]*"' | head -1 | cut -d'"' -f4)
echo "start $ID $(date +%s.%N)" >> {tl}
sleep 0.6
mkdir -p artifacts && echo "$ID" > artifacts/out.txt
echo '{{"type":"progress","msg":"working on '"$ID"'"}}'
echo '{{"type":"artifact","name":"out","path":"artifacts/out.txt"}}'
echo "end $ID $(date +%s.%N)" >> {tl}
echo '{{"type":"done","summary":"fake finished","evidence":[{{"criterion":0,"command":"test -f artifacts/out.txt","exit":0,"stdout_tail":""}}]}}'
"#,
        tl = timeline.display()
    ));
    let config = env.write_config(2, &script, "");
    let checks = || {
        vec![
            Check::Command { cmd: "test -f artifacts/out.txt".into(), expect_exit: 0 },
            Check::ArtifactExists { name: "out".into() },
        ]
    };
    let a = env.add_ready_task("A", &env.workspace_for("a"), checks(), vec![], 0);
    let b = env.add_ready_task("B", &env.workspace_for("b"), checks(), vec![], 0);
    let c = env.add_ready_task("C", &env.workspace_for("c"), checks(), vec![a], 0);

    let log = env.run_taskd(&config, Duration::from_secs(60));

    for id in [a, b, c] {
        let t = env.task(id);
        assert_eq!(t.status, Status::Done, "{id}: {log}");
        assert_eq!(t.attempts, 0);
        assert!(t.lease.is_none());
        assert_eq!(
            env.transitions(id),
            vec!["Draft->Ready:accept", "Ready->Running:dispatch", "Running->Reviewing:worker_done", "Reviewing->Done:review_pass"]
        );
        let events = env.store.events_for(id).unwrap();
        assert!(events.iter().any(|(_, e)| matches!(e, Event::ArtifactProduced { artifact, .. } if artifact.name == "out" && artifact.sha256.len() == 64)));
        assert!(events.iter().any(|(_, e)| matches!(e, Event::WorkerProgress { .. })));
        let verdicts: Vec<bool> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::ReviewVerdict { pass, .. } => Some(*pass),
                _ => None,
            })
            .collect();
        assert_eq!(verdicts, vec![true, true]);
    }

    // 並列度: 同時に走った run は最大 2、かつ実際に 2 並列になった。C は A の終了後に開始。
    let text = std::fs::read_to_string(&timeline).unwrap();
    let mut events: Vec<(f64, i32, String)> = text
        .lines()
        .map(|l| {
            let mut it = l.split_whitespace();
            let kind = it.next().unwrap();
            let id = it.next().unwrap().to_string();
            let ts: f64 = it.next().unwrap().parse().unwrap();
            (ts, if kind == "start" { 1 } else { -1 }, id)
        })
        .collect();
    events.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap().then(x.1.cmp(&y.1)));
    let (mut cur, mut max) = (0, 0);
    for (_, delta, _) in &events {
        cur += delta;
        max = max.max(cur);
    }
    assert_eq!(max, 2, "expected exactly 2 concurrent workers:\n{text}");
    let end_a = events.iter().find(|(_, d, id)| *d == -1 && id == &a.to_string()).unwrap().0;
    let start_c = events.iter().find(|(_, d, id)| *d == 1 && id == &c.to_string()).unwrap().0;
    assert!(start_c >= end_a, "C must start after A finished:\n{text}");
    assert!(log.contains("idle; exiting"));
    env.replay_is_consistent();
}

/// シナリオ 2: Command チェックが 1 回目に失敗し、リトライ 1 回で 2 回目に done。
#[test]
fn command_check_fails_once_then_passes_after_retry() {
    let env = Env::new();
    // 1 回目: stdin を保存して done（ok.txt を作らない → レビュー失敗）。2 回目: ok.txt を作って done。
    let script = env.write_script(
        r#"N=$(ls run-*.json 2>/dev/null | wc -l)
cat > "run-$N.json"
if [ "$N" -ge 1 ]; then touch ok.txt; fi
echo '{"type":"done","summary":"attempt '"$N"'","evidence":[{"criterion":0,"command":"test -f ok.txt","exit":0,"stdout_tail":""}]}'
"#,
    );
    let config = env.write_config(2, &script, "");
    let dir = env.workspace_for("retry");
    let id = env.add_ready_task("retry", &dir, vec![Check::Command { cmd: "test -f ok.txt".into(), expect_exit: 0 }], vec![], 1);

    let log = env.run_taskd(&config, Duration::from_secs(60));

    let t = env.task(id);
    assert_eq!(t.status, Status::Done, "{log}");
    assert_eq!(t.attempts, 1);
    assert_eq!(
        env.transitions(id),
        vec![
            "Draft->Ready:accept",
            "Ready->Running:dispatch",
            "Running->Reviewing:worker_done",
            "Reviewing->Ready:review_fail",
            "Ready->Running:dispatch",
            "Running->Reviewing:worker_done",
            "Reviewing->Done:review_pass",
        ]
    );
    let verdicts: Vec<(bool, String)> = env
        .store
        .events_for(id)
        .unwrap()
        .into_iter()
        .filter_map(|(_, e)| match e {
            Event::ReviewVerdict { pass, reason, .. } => Some((pass, reason)),
            _ => None,
        })
        .collect();
    assert_eq!(verdicts.len(), 2);
    assert!(!verdicts[0].0 && verdicts[0].1.contains("exit=Some(1)"), "{:?}", verdicts[0]);
    assert!(verdicts[1].0);
    // 2 回目の run には 1 回目のレビュー結果が context.prior_review として渡っている。
    let run0 = std::fs::read_to_string(dir.join("run-0.json")).unwrap();
    let run1 = std::fs::read_to_string(dir.join("run-1.json")).unwrap();
    assert!(run0.contains(r#""prior_review":[]"#));
    assert!(run1.contains(r#""prior_review":[{"criterion":0,"pass":false"#), "{run1}");
    assert!(run1.contains(r#""attempts":1"#));
    // ADR-0033 D4/D6: protocol は 4（context.node / memory / conversation / standing_rules /
    // organization、delegate の assignee、結果ファイルの memory を追加）。いずれも追加のみ。
    assert!(run1.starts_with(r#"{"type":"run","protocol":4"#));
    env.replay_is_consistent();
}

/// シナリオ 3: 期限切れリースを持つ running タスクが回収され、再実行されて done。
#[test]
fn expired_lease_is_reclaimed_and_task_completes() {
    let env = Env::new();
    let script = env.write_script(r#"cat >/dev/null; echo '{"type":"done","summary":"ok","evidence":[]}'"#);
    let config = env.write_config(1, &script, "");
    let dir = env.workspace_for("stale");
    // 前世代の taskd が落ちた状態を再現: running + 期限切れリース。
    let now = OffsetDateTime::now_utc();
    let task = Task {
        repos: Vec::new(),
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: "stale".into(),
        objective: "e2e".into(),
        acceptance: vec![Criterion { text: "e2e".into(), check: Check::Command { cmd: "true".into(), expect_exit: 0 } }],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Running,
        priority: 0,
        worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
        workspace: WorkspaceSpec::Local { path: dir.clone(), mode: None },
        budget: Budget { max_turns: 5, max_wall_secs: 20, max_retries: 1 },
        attempts: 0,
        lease: Some(Lease { worker_run_id: "stale-run".into(), expires_at: now - time::Duration::minutes(5) }),
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
    env.store.insert(&task).unwrap();
    env.store.append_event(task.id, &Event::Created { task: Box::new(task.clone()) }).unwrap();

    let log = env.run_taskd(&config, Duration::from_secs(60));

    let t = env.task(task.id);
    assert_eq!(t.status, Status::Done, "{log}");
    assert_eq!(t.attempts, 1, "lease expiry counts as a failed attempt (ADR-0002 D3)");
    assert!(t.lease.is_none());
    assert_eq!(
        env.transitions(task.id),
        vec![
            "Running->Ready:lease_expired",
            "Ready->Running:dispatch",
            "Running->Reviewing:worker_done",
            "Reviewing->Done:review_pass",
        ]
    );
    let events = env.store.events_for(task.id).unwrap();
    assert!(events.iter().any(|(_, e)| matches!(e, Event::WorkerFinished { run_id, outcome, .. } if run_id == "stale-run" && outcome == "lease_expired")));
    assert!(log.contains("lease expired; reclaimed"), "{log}");
    env.replay_is_consistent();
}

/// 補助: 終端メッセージ無しで死ぬワーカーは retryable エラーとして attempts を消費し、
/// max_retries 超過で failed になる（ADR-0003 D3 / ADR-0002 D3）。
#[test]
fn worker_crash_without_terminal_message_fails_after_retries() {
    let env = Env::new();
    let script = env.write_script(r#"cat >/dev/null; echo '{"type":"progress","msg":"about to crash"}'; exit 9"#);
    let config = env.write_config(1, &script, "");
    let dir = env.workspace_for("crash");
    let id = env.add_ready_task("crash", &dir, vec![Check::Command { cmd: "true".into(), expect_exit: 0 }], vec![], 1);
    let log = env.run_taskd(&config, Duration::from_secs(60));
    let t = env.task(id);
    assert_eq!(t.status, Status::Failed, "{log}");
    assert_eq!(t.attempts, 2);
    assert_eq!(
        env.transitions(id),
        vec![
            "Draft->Ready:accept",
            "Ready->Running:dispatch",
            "Running->Ready:worker_error",
            "Ready->Running:dispatch",
            "Running->Failed:worker_error",
        ]
    );
    let events = env.store.events_for(id).unwrap();
    assert!(events.iter().any(|(_, e)| matches!(e, Event::WorkerFinished { outcome, .. } if outcome.contains("exit=9"))));
    env.replay_is_consistent();
}
