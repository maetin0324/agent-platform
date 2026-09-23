//! DESIGN §6 Phase 10 の受け入れ条件（ADR-0016）。実バイナリ `celerisctl` / `celeris`（`[api]` 有効）と fake ワーカー
//! （`sh` スクリプト）と `curl` だけで動き、外部ネットワークに出ない。
//!
//! 1. `[[roles]]` の既定（tier / max_turns）が `celerisctl add --role lead --config` で効き、`WorkerStarted.task_role` から役割が追える。
//!    役割の指示文は `context.role.instructions` としてワーカーに届く（fake が progress に書き戻す）。
//! 2. fake が `delegate` で 4 件提案 → 検証を通った 2 件だけが子として挿入され `Event::Delegated` が残る。空欄・自己参照は拒否され、
//!    理由が `WorkerProgress` に残り、親は失敗しない。
//! 3. 子が全て終端になるまで親は `reviewing` のまま。`--aggregate` の親は最後に 1 回だけ集約 run をして `artifacts/summary.md` が
//!    暗黙の条件で判定される。`--aggregate` 無しの親は run を増やさず `done`。
//! 5. `celerisctl show --json` と `GET /api/v1/tasks/{id}` に `role` と `delegated` が出る。
//! 6. `celerisctl replay` の差分ゼロ。

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use task_core::{Event, SqliteStore, Status, Task, TaskId, TaskStore, Tier};

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

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    store: Arc<SqliteStore>,
    port: u16,
}

const LEAD_INSTRUCTIONS: &str = "You are the lead: split the work and delegate implementation";

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let db = root.join("celeris.sqlite3");
        let store = Arc::new(SqliteStore::open(&db).unwrap());
        Self {
            _tmp: tmp,
            root,
            db,
            store,
            port: free_port(),
        }
    }

    fn write_script(&self, body: &str) -> PathBuf {
        let path = self.root.join("fake-worker.sh");
        std::fs::write(&path, format!("#!/bin/sh\nset -u\n{body}\n")).unwrap();
        path
    }

    fn write_config(&self, script: &Path) -> PathBuf {
        self.write_config_with(script, "")
    }

    /// `extra_delegation` は `[delegation]` に足す行（ADR-0021 D4 の `on_child_failure` など）。
    fn write_config_with(&self, script: &Path, extra_delegation: &str) -> PathBuf {
        let path = self.root.join("config.toml");
        let text = format!(
            r#"db = "celeris.sqlite3"
workspace_root = "workspaces"
tick_ms = 50
max_concurrency = 3
lease_grace_secs = 60
idle_timeout_secs = 30
kill_grace_secs = 1
review_timeout_secs = 30
retry_backoff_base_secs = 0

[api]
listen = "127.0.0.1:{port}"

[delegation]
max_delegate_per_run = 8
max_tree_depth = 5
max_tree_runs = 100
{extra_delegation}

[[roles]]
id = "lead"
tier = "frontier"
max_turns = 40
instructions = "{instructions}"

[[roles]]
id = "implementer"
tier = "cheap"
max_wall_secs = 120

[adapters.fake]
command = ["sh", "{script}"]

[[providers]]
id = "fake-local"
adapter = "fake"
tiers = ["frontier", "standard", "cheap"]
concurrency = 3
model = "fake"
"#,
            port = self.port,
            instructions = LEAD_INSTRUCTIONS,
            extra_delegation = extra_delegation,
            script = script.display()
        );
        std::fs::write(&path, text).unwrap();
        path
    }

    fn celerisctl(&self, args: &[&str]) -> String {
        let out = Command::new(bin("celerisctl"))
            .arg("--db")
            .arg(&self.db)
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

    fn start_celeris(&self, config: &Path) -> Proc {
        let log = self.root.join("celeris.log");
        let child = Command::new(bin("celeris"))
            .args(["--config", config.to_str().unwrap(), "--log-format", "text"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        Proc { child, log }
    }

    fn get(&self, path: &str) -> (u16, Value) {
        let out = Command::new("curl")
            .args([
                "-s",
                "-S",
                "-o",
                "-",
                "-w",
                "\n%{http_code}",
                "--max-time",
                "10",
            ])
            .arg(format!("http://127.0.0.1:{}/api/v1{path}", self.port))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", "0"));
        let status: u16 = status.trim().parse().unwrap_or(0);
        let json = serde_json::from_str(body).unwrap_or(Value::Null);
        (status, json)
    }

    fn task(&self, id: TaskId) -> Task {
        self.store.get(id).unwrap().unwrap()
    }

    fn events(&self, id: TaskId) -> Vec<Event> {
        self.store
            .events_for(id)
            .unwrap()
            .into_iter()
            .map(|(_, e)| e)
            .collect()
    }

    fn transitions(&self, id: TaskId) -> Vec<String> {
        self.events(id)
            .into_iter()
            .filter_map(|e| match e {
                Event::Transitioned { from, to, reason } => {
                    Some(format!("{from:?}->{to:?}:{reason}"))
                }
                _ => None,
            })
            .collect()
    }

    fn progress(&self, id: TaskId) -> Vec<String> {
        self.events(id)
            .into_iter()
            .filter_map(|e| match e {
                Event::WorkerProgress { msg, .. } => Some(msg),
                _ => None,
            })
            .collect()
    }

    fn children_of(&self, parent: TaskId) -> Vec<Task> {
        self.store.children(parent).unwrap()
    }

    fn replay_is_consistent(&self) {
        let out = self.celerisctl(&["replay"]);
        assert!(out.contains("replay: 0 mismatches"), "{out}");
    }
}

/// stdin の `run` 行から `task.role`（文字列の `"role":"…"`）、`context.role.instructions`、`context.children` の有無、
/// `task.id` を取り、役割ごとに振る舞う fake ワーカー。
/// - lead（子の一覧なし）: 指示文を progress に書き戻し、4 件（有効 2、空欄 1、自己参照 1）を `delegate` して done
/// - lead（子の一覧あり = 集約 run）: 子の件数を progress に書き、`artifacts/summary.md` を作って done
/// - implementer: `<title>.txt` を作って done
fn worker_script() -> String {
    r##"RUN=$(mktemp)
cat > "$RUN"
TASK_ID=$(grep -o '"task":{"id":"[0-9A-Z]*"' "$RUN" | head -1 | grep -o '[0-9A-Z]\{26\}')
ROLE=$(grep -o '"role":"[a-z]*"' "$RUN" | head -1 | cut -d'"' -f4)
TITLE=$(grep -o '"title":"[^"]*"' "$RUN" | head -1 | cut -d'"' -f4)
INSTR=$(grep -o '"instructions":"[^"]*"' "$RUN" | head -1 | cut -d'"' -f4)
mkdir -p artifacts
case "$ROLE" in
  lead)
    if grep -q '"children":\[{' "$RUN"; then
      N=$(grep -o '"children":\[.*' "$RUN" | grep -o '"outcome":"[^"]*"' | wc -l)
      echo '{"type":"progress","msg":"aggregate run sees '"$N"' children"}'
      printf '# Summary\n\n%s children done.\n' "$N" > artifacts/summary.md
      echo '{"type":"artifact","name":"summary.md","path":"artifacts/summary.md"}'
      echo '{"type":"done","summary":"aggregated","evidence":[]}'
    else
      echo '{"type":"progress","msg":"instructions: '"$INSTR"'"}'
      echo '{"type":"delegate","tasks":['\
'{"title":"impl-a","objective":"implement a","acceptance":[{"text":"a.txt exists","check":{"type":"command","cmd":"test -f impl-a.txt","expect_exit":0}}],"role":"implementer"},'\
'{"title":"impl-b","objective":"implement b","acceptance":[{"text":"b.txt exists","check":{"type":"command","cmd":"test -f impl-b.txt","expect_exit":0}}],"role":"implementer","depends_on":[0]},'\
'{"title":"  ","objective":"blank title","acceptance":[{"text":"c","check":{"type":"human"}}]},'\
'{"title":"self-ref","objective":"depends on the lead itself","acceptance":[{"text":"c","check":{"type":"human"}},{"text":"d","check":{"type":"artifact_exists","name":"result.md"}}],"depends_on":["'"$TASK_ID"'"]}'\
']}'
      echo '{"type":"done","summary":"delegated","evidence":[]}'
    fi
    ;;
  implementer)
    sleep 0.2
    touch "$TITLE.txt"
    echo '{"type":"done","summary":"did '"$TITLE"'","evidence":[]}'
    ;;
  *)
    echo '{"type":"error","message":"unexpected role '"$ROLE"'","retryable":false}'
    ;;
esac
rm -f "$RUN"
"##
    .to_string()
}

fn wait_done(env: &Env, daemon: &Proc, id: TaskId) {
    let ok = wait_until(Duration::from_secs(60), || {
        env.task(id).status.is_terminal()
    });
    assert!(
        ok,
        "task {id} did not reach a terminal state\n{}",
        daemon.log_text()
    );
}

/// 受け入れ 1・2・3（aggregate = true）・5・6。
#[test]
fn lead_delegates_children_waits_for_them_and_aggregates_once() {
    let env = Env::new();
    let script = env.write_script(&worker_script());
    let config = env.write_config(&script);
    let dir = env.root.join("workspaces").join("lead");
    std::fs::create_dir_all(&dir).unwrap();

    // 1. --role lead --config: tier / max_turns は役割の既定、max_wall_secs は全体の既定。
    let id: TaskId = env
        .celerisctl(&[
            "add",
            "--config",
            config.to_str().unwrap(),
            "--role",
            "lead",
            "--aggregate",
            "--title",
            "lead the feature",
            "--objective",
            "split the feature into implementation tasks and summarize",
            "--check-cmd",
            "true",
            "--workspace",
            dir.to_str().unwrap(),
        ])
        .trim()
        .parse()
        .unwrap();
    let lead = env.task(id);
    assert_eq!(lead.role.as_deref(), Some("lead"));
    assert!(lead.aggregate);
    assert_eq!(lead.worker_hint.tier, Tier::Frontier, "role default");
    assert_eq!(lead.budget.max_turns, 40, "role default");
    assert_eq!(lead.budget.max_wall_secs, 600, "global default");
    assert_eq!(
        env.celerisctl(&["approve", &id.to_string()]).trim(),
        "Ready"
    );

    let daemon = env.start_celeris(&config);
    wait_done(&env, &daemon, id);
    let lead = env.task(id);
    assert_eq!(lead.status, Status::Done, "{}", daemon.log_text());
    assert_eq!(lead.attempts, 0);

    // 2. 検証を通った 2 件だけが子。役割の既定（cheap / max_wall_secs 120）が子に効く。
    let children = env.children_of(id);
    assert_eq!(children.len(), 2, "{children:?}");
    assert_eq!(children[0].title, "impl-a");
    assert_eq!(children[1].title, "impl-b");
    assert_eq!(children[1].depends_on, vec![children[0].id]);
    for c in &children {
        assert_eq!(c.status, Status::Done, "{:?}", env.events(c.id));
        assert_eq!(c.role.as_deref(), Some("implementer"));
        assert_eq!(c.worker_hint.tier, Tier::Cheap);
        assert_eq!(c.budget.max_wall_secs, 120);
        assert_eq!(c.parent_id, Some(id));
        assert_eq!(env.transitions(c.id)[0], "Draft->Ready:accept");
    }
    let events = env.events(id);
    let delegated: Vec<Vec<TaskId>> = events
        .iter()
        .filter_map(|e| match e {
            Event::Delegated { task_ids, .. } => Some(task_ids.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(delegated, vec![vec![children[0].id, children[1].id]]);
    let progress = env.progress(id);
    assert!(
        progress
            .iter()
            .any(|m| m == &format!("instructions: {LEAD_INSTRUCTIONS}")),
        "{progress:?}"
    );
    assert!(
        progress
            .iter()
            .any(|m| m.starts_with("delegate rejected: tasks[2]")
                && m.contains("title must not be empty")),
        "{progress:?}"
    );
    assert!(
        progress
            .iter()
            .any(|m| m.starts_with("delegate rejected: tasks[3]")
                && m.contains("delegating task itself")),
        "{progress:?}"
    );
    assert!(
        progress
            .iter()
            .any(|m| m.starts_with("waiting for 2 delegated child task(s)")),
        "{progress:?}"
    );
    assert!(
        progress
            .iter()
            .any(|m| m == "aggregate run sees 2 children"),
        "{progress:?}"
    );

    // 3. 親は子待ちの間 reviewing のまま → 集約遷移 → 集約 run → done。run は 2 回、役割が WorkerStarted に残る。
    assert_eq!(
        env.transitions(id),
        vec![
            "Draft->Ready:accept",
            "Ready->Running:dispatch",
            "Running->Reviewing:worker_done",
            "Reviewing->Ready:aggregate",
            "Ready->Running:dispatch",
            "Running->Reviewing:worker_done",
            "Reviewing->Done:review_pass",
        ]
    );
    let task_roles: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match e {
            Event::WorkerStarted {
                role: None,
                task_role,
                ..
            } => Some(task_role.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        task_roles,
        vec![Some("lead".to_string()), Some("lead".to_string())]
    );
    assert!(
        events.iter().any(|e| matches!(e, Event::ReviewVerdict { criterion_idx: 1, pass: true, reason, .. } if reason.contains("summary.md"))),
        "{events:?}"
    );
    assert!(dir.join("artifacts/summary.md").is_file());

    // 5. celerisctl show --json と GET /tasks/{id} の role / delegated。
    let shown: Value =
        serde_json::from_str(env.celerisctl(&["show", "--json", &id.to_string()]).trim()).unwrap();
    assert_eq!(shown["role"], "lead", "{shown}");
    assert_eq!(shown["task"]["aggregate"], true);
    assert_eq!(
        shown["delegated"].as_array().map(Vec::len),
        Some(1),
        "{shown}"
    );
    assert_eq!(
        shown["delegated"][0]["tasks"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(shown["delegated"][0]["tasks"][0]["title"], "impl-a");
    let (status, detail) = env.get(&format!("/tasks/{id}"));
    assert_eq!(status, 200, "{detail}\n{}", daemon.log_text());
    assert_eq!(detail["role"], "lead");
    assert_eq!(detail["delegated"][0]["tasks"][1]["title"], "impl-b");
    assert_eq!(detail["delegated"][0]["tasks"][1]["status"], "done");
    let (status, cfg) = env.get("/config");
    assert_eq!(status, 200);
    assert_eq!(cfg["roles"][0]["id"], "lead", "{cfg}");
    assert!(cfg.to_string().contains("has_instructions"), "{cfg}");
    assert!(
        !cfg.to_string().contains(LEAD_INSTRUCTIONS),
        "instructions body must not leak: {cfg}"
    );
    drop(daemon);

    // 6. replay 差分ゼロ（Delegated と aggregate は状態・attempts を変えない）。
    env.replay_is_consistent();
}

/// 受け入れ 3（aggregate = false）: 親は子が終わるまで reviewing のまま、終わったら run を増やさず done。
#[test]
fn non_aggregate_lead_completes_after_children_without_another_run() {
    let env = Env::new();
    let script = env.write_script(&worker_script());
    let config = env.write_config(&script);
    let dir = env.root.join("workspaces").join("lead2");
    std::fs::create_dir_all(&dir).unwrap();
    let id: TaskId = env
        .celerisctl(&[
            "add",
            "--config",
            config.to_str().unwrap(),
            "--role",
            "lead",
            "--title",
            "lead without aggregate",
            "--objective",
            "delegate and finish",
            "--check-cmd",
            "true",
            "--workspace",
            dir.to_str().unwrap(),
        ])
        .trim()
        .parse()
        .unwrap();
    assert!(!env.task(id).aggregate);
    env.celerisctl(&["approve", &id.to_string()]);

    let daemon = env.start_celeris(&config);
    // 親の run と判定が終わっても、子が終わるまで reviewing のまま。
    let saw_waiting = wait_until(Duration::from_secs(30), || {
        let parent = env.task(id);
        parent.status == Status::Reviewing
            && env.children_of(id).iter().any(|c| !c.status.is_terminal())
    });
    assert!(
        saw_waiting,
        "parent should wait in reviewing while children run\n{}",
        daemon.log_text()
    );
    wait_done(&env, &daemon, id);
    drop(daemon);

    assert_eq!(env.task(id).status, Status::Done);
    assert_eq!(
        env.transitions(id),
        vec![
            "Draft->Ready:accept",
            "Ready->Running:dispatch",
            "Running->Reviewing:worker_done",
            "Reviewing->Done:review_pass",
        ]
    );
    assert_eq!(
        env.events(id)
            .iter()
            .filter(|e| matches!(e, Event::WorkerStarted { role: None, .. }))
            .count(),
        1,
        "no aggregate run without --aggregate"
    );
    for c in env.children_of(id) {
        assert_eq!(c.status, Status::Done);
    }
    assert!(!dir.join("artifacts/summary.md").exists());
    env.replay_is_consistent();
}

/// ADR-0021: 委譲した子が失敗したときの親の fake ワーカー。
/// - lead（子の一覧なし）: `impl-ok` と `impl-broken` を委譲して done
/// - lead（子の一覧に failed があり、まだ代わりを立てていない = やり直し run）: 代わりの子 `impl-fixed` を委譲して done
/// - lead（子の一覧あり、代わりを立てた後 = 集約 run）: `artifacts/summary.md` を書いて done
/// - implementer: `impl-broken` だけ retryable=false で失敗。ほかは `<title>.txt` を作って done
fn worker_script_with_a_failing_child() -> String {
    r##"RUN=$(mktemp)
cat > "$RUN"
ROLE=$(grep -o '"role":"[a-z]*"' "$RUN" | head -1 | cut -d'"' -f4)
TITLE=$(grep -o '"title":"[^"]*"' "$RUN" | head -1 | cut -d'"' -f4)
HAS_CHILDREN=$(grep -c '"children":\[[^]]' "$RUN" || true)
HAS_FAILED_CHILD=$(grep -c '"status":"failed"' "$RUN" || true)
HAS_REPLACEMENT=$(grep -c '"title":"impl-fixed"' "$RUN" || true)
mkdir -p artifacts
case "$ROLE" in
  lead)
    if [ "$HAS_CHILDREN" != "0" ] && [ "$HAS_FAILED_CHILD" != "0" ] && [ "$HAS_REPLACEMENT" = "0" ]; then
      echo '{"type":"progress","msg":"a delegated child failed; delegating a replacement"}'
      echo '{"type":"delegate","tasks":[{"title":"impl-fixed","objective":"redo the failed unit","acceptance":[{"text":"file exists","check":{"type":"command","cmd":"test -f impl-fixed.txt","expect_exit":0}}],"role":"implementer"}]}'
      echo '{"type":"done","summary":"re-delegated","evidence":[]}'
    elif [ "$HAS_CHILDREN" != "0" ]; then
      printf '# Summary\n\nall children done.\n' > artifacts/summary.md
      echo '{"type":"artifact","name":"summary.md","path":"artifacts/summary.md"}'
      echo '{"type":"done","summary":"aggregated","evidence":[]}'
    else
      echo '{"type":"delegate","tasks":['\
'{"title":"impl-ok","objective":"this one works","acceptance":[{"text":"file exists","check":{"type":"command","cmd":"test -f impl-ok.txt","expect_exit":0}}],"role":"implementer"},'\
'{"title":"impl-broken","objective":"this one fails","acceptance":[{"text":"file exists","check":{"type":"command","cmd":"test -f impl-broken.txt","expect_exit":0}}],"role":"implementer"}'\
']}'
      echo '{"type":"done","summary":"delegated","evidence":[]}'
    fi
    ;;
  implementer)
    if [ "$TITLE" = "impl-broken" ]; then
      echo '{"type":"error","message":"cannot do it","retryable":false}'
    else
      touch "$TITLE.txt"
      echo '{"type":"done","summary":"did '"$TITLE"'","evidence":[]}'
    fi
    ;;
  *)
    echo '{"type":"error","message":"unexpected role '"$ROLE"'","retryable":false}'
    ;;
esac
rm -f "$RUN"
"##
    .to_string()
}

/// ADR-0021 D1/D3: 委譲した子が失敗したら、親は**失敗を引き継がず**やり直す（attempts 消費、`reviewing → ready`）。
/// やり直しの run は `context.children` で失敗した子を見られる。一度扱った失敗は数え直さないので、
/// 親が代わりの子を立てて成功すれば、古い失敗があっても親は完了できる。
#[test]
fn a_failed_child_makes_the_parent_retry_instead_of_inheriting_the_failure() {
    let env = Env::new();
    let script = env.write_script(&worker_script_with_a_failing_child());
    let config = env.write_config(&script);
    let dir = env.root.join("workspaces").join("lead-retry");
    std::fs::create_dir_all(&dir).unwrap();

    let id: TaskId = env
        .celerisctl(&[
            "add",
            "--config",
            config.to_str().unwrap(),
            "--role",
            "lead",
            "--aggregate",
            "--max-retries",
            "1",
            "--title",
            "lead with a failing child",
            "--objective",
            "delegate two units; one of them fails",
            "--check-cmd",
            "true",
            "--workspace",
            dir.to_str().unwrap(),
        ])
        .trim()
        .parse()
        .unwrap();
    env.celerisctl(&["approve", &id.to_string()]);

    let daemon = env.start_celeris(&config);
    wait_done(&env, &daemon, id);
    drop(daemon);

    let parent = env.task(id);
    assert_eq!(
        parent.status,
        Status::Done,
        "親は子の失敗を引き継がない: {:?}\n{:?}",
        env.transitions(id),
        env.progress(id)
    );
    assert_eq!(parent.attempts, 1, "やり直しで attempts を 1 つ使う");

    // reviewing → ready(child_failed) → もう一度 run → 代わりの子 → 集約 run → done。
    let transitions = env.transitions(id);
    assert!(
        transitions.contains(&"Reviewing->Ready:child_failed".to_string()),
        "{transitions:?}"
    );
    assert!(
        !transitions
            .iter()
            .any(|t| t.ends_with("->Failed:child_failed")),
        "{transitions:?}"
    );
    assert_eq!(
        transitions.last().map(String::as_str),
        Some("Reviewing->Done:review_pass"),
        "{transitions:?}"
    );

    // やり直しの run は子の結果を見ている（fake が failed を見つけて代わりを委譲した）。
    let progress = env.progress(id);
    assert!(
        progress
            .iter()
            .any(|m| m.starts_with("a delegated child failed; delegating a replacement")),
        "{progress:?}"
    );
    assert!(
        progress.iter().any(|m| m.contains("delegated child task(s) failed; retrying this task (attempt 1/1)")),
        "{progress:?}"
    );

    // 子は 3 件（impl-ok / impl-broken=failed / impl-fixed）。古い失敗は 2 度目の判定には出てこない。
    let children = env.children_of(id);
    let failed: Vec<&str> = children
        .iter()
        .filter(|c| c.status == Status::Failed)
        .map(|c| c.title.as_str())
        .collect();
    assert_eq!(failed, vec!["impl-broken"], "{children:?}");
    assert_eq!(children.len(), 3, "{children:?}");
    assert!(
        dir.join("artifacts/summary.md").is_file(),
        "集約 run まで進む"
    );
    env.replay_is_consistent();
}

/// ADR-0021 D1/D2: やり直せない（`max_retries = 0`）なら、親は `failed` ではなく `blocked` になり、
/// 受信箱に質問が出る。人が `celerisctl answer` すると再開する。
#[test]
fn when_the_parent_cannot_retry_it_asks_a_human_instead_of_failing() {
    let env = Env::new();
    let script = env.write_script(&worker_script_with_a_failing_child());
    let config = env.write_config(&script);
    let dir = env.root.join("workspaces").join("lead-ask");
    std::fs::create_dir_all(&dir).unwrap();

    let id: TaskId = env
        .celerisctl(&[
            "add",
            "--config",
            config.to_str().unwrap(),
            "--role",
            "lead",
            "--aggregate",
            "--max-retries",
            "0",
            "--title",
            "lead that cannot retry",
            "--objective",
            "delegate two units; one of them fails",
            "--check-cmd",
            "true",
            "--workspace",
            dir.to_str().unwrap(),
        ])
        .trim()
        .parse()
        .unwrap();
    env.celerisctl(&["approve", &id.to_string()]);

    let daemon = env.start_celeris(&config);
    let blocked = wait_until(Duration::from_secs(60), || {
        env.task(id).status == Status::Blocked
    });
    assert!(blocked, "親は人の判断待ちになる\n{}", daemon.log_text());

    let parent = env.task(id);
    assert_eq!(
        parent.attempts, 0,
        "人の回答を待つ間は attempts を増やさない"
    );
    let transitions = env.transitions(id);
    assert!(
        transitions.contains(&"Reviewing->Blocked:child_failed".to_string()),
        "{transitions:?}"
    );

    // 質問が残り、受信箱と詳細に出る。
    let question = env.events(id).into_iter().find_map(|e| match e {
        Event::QuestionRaised { text, .. } => Some(text),
        _ => None,
    });
    let question = question.expect("QuestionRaised");
    assert!(question.contains("impl-broken"), "{question}");
    assert!(
        question.contains(&format!("celerisctl answer {id}")),
        "{question}"
    );

    let (status, inbox) = env.get("/inbox");
    assert_eq!(status, 200);
    let questions = inbox["questions"].as_array().expect("questions");
    let item = questions
        .iter()
        .find(|q| q["task"]["id"] == id.to_string())
        .unwrap_or_else(|| panic!("parent should be in the inbox questions: {inbox}"));
    assert!(
        item["question"]
            .as_str()
            .unwrap_or_default()
            .contains("impl-broken"),
        "{item}"
    );
    assert!(item["asked_at"].is_string(), "{item}");

    // 人が答えると再開し、代わりの子を立てて完了する（失敗のまま終わらない）。
    env.celerisctl(&[
        "answer",
        &id.to_string(),
        "impl-broken は別の分け方でやり直して",
    ]);
    wait_done(&env, &daemon, id);
    drop(daemon);

    let parent = env.task(id);
    assert_eq!(parent.status, Status::Done, "回答の後は進める");
    assert!(
        env.children_of(id).iter().any(|c| c.title == "impl-fixed"),
        "{:?}",
        env.children_of(id)
    );
    env.replay_is_consistent();
}

/// ADR-0021 D4: `on_child_failure = "ignore"` なら ADR-0016 M5 までの挙動（子の失敗を見ずに親を完了させる）。
#[test]
fn on_child_failure_ignore_keeps_the_old_behaviour() {
    let env = Env::new();
    // 子の一覧があれば必ず集約する（やり直しの分岐を持たない）単純な lead。
    let script = env.write_script(
        r##"RUN=$(mktemp)
cat > "$RUN"
ROLE=$(grep -o '"role":"[a-z]*"' "$RUN" | head -1 | cut -d'"' -f4)
TITLE=$(grep -o '"title":"[^"]*"' "$RUN" | head -1 | cut -d'"' -f4)
HAS_CHILDREN=$(grep -c '"children":\[[^]]' "$RUN" || true)
mkdir -p artifacts
case "$ROLE" in
  lead)
    if [ "$HAS_CHILDREN" != "0" ]; then
      printf '# Summary\n\nchildren finished.\n' > artifacts/summary.md
      echo '{"type":"artifact","name":"summary.md","path":"artifacts/summary.md"}'
      echo '{"type":"done","summary":"aggregated","evidence":[]}'
    else
      echo '{"type":"delegate","tasks":[{"title":"impl-broken","objective":"this one fails","acceptance":[{"text":"file exists","check":{"type":"command","cmd":"test -f impl-broken.txt","expect_exit":0}}],"role":"implementer"}]}'
      echo '{"type":"done","summary":"delegated","evidence":[]}'
    fi
    ;;
  implementer)
    echo '{"type":"error","message":"cannot do it","retryable":false}'
    ;;
esac
rm -f "$RUN"
"##,
    );
    let config = env.write_config_with(&script, "on_child_failure = \"ignore\"");
    let dir = env.root.join("workspaces").join("lead-ignore");
    std::fs::create_dir_all(&dir).unwrap();

    let id: TaskId = env
        .celerisctl(&[
            "add",
            "--config",
            config.to_str().unwrap(),
            "--role",
            "lead",
            "--aggregate",
            "--title",
            "lead that ignores child failures",
            "--objective",
            "delegate one unit that fails",
            "--check-cmd",
            "true",
            "--workspace",
            dir.to_str().unwrap(),
        ])
        .trim()
        .parse()
        .unwrap();
    env.celerisctl(&["approve", &id.to_string()]);

    let daemon = env.start_celeris(&config);
    wait_done(&env, &daemon, id);
    drop(daemon);

    let parent = env.task(id);
    assert_eq!(parent.status, Status::Done);
    assert_eq!(parent.attempts, 0);
    let transitions = env.transitions(id);
    assert!(
        !transitions.iter().any(|t| t.contains("child_failed")),
        "{transitions:?}"
    );
    assert!(
        env.children_of(id)
            .iter()
            .any(|c| c.status == Status::Failed),
        "子は失敗したまま"
    );
    env.replay_is_consistent();
}
