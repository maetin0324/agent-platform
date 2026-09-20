//! DESIGN §6 Phase 7 の受け入れ条件（ADR-0010）のうち、実バイナリ `celerisctl` / `celeris` と fake ワーカー
//! （`sh` スクリプト）で再現するもの。ネットワークに出ない。タスクは全て `celerisctl add` で作る。
//!
//! 1/2. `question` → `celerisctl answer` → 次 run の `context.answers` に回答が載り `done`（条件は `--check-cmd` / `--check-artifact`）
//! 3.   `celerisctl cancel` は非終端のみ。先行タスクの `failed` が後続へ推移的に伝播する（`dependency_failed`）
//! 4.   Human check は再レビューで新しい `Approval` 子を要求し、親の cancel で未決の `Approval` 子が `cancelled`
//! 5.   `provider_failure` 付きの `error` は attempts を消費せず `requeue` され、cooldown 明けに `done`

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use task_core::{Check, Event, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, WorkspaceSpec};

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
        let db = root.join("celeris.sqlite3");
        let store = Arc::new(SqliteStore::open(&db).unwrap());
        Self { _tmp: tmp, root, db, store }
    }

    fn write_script(&self, body: &str) -> PathBuf {
        let path = self.root.join("fake-worker.sh");
        std::fs::write(&path, format!("#!/bin/sh\nset -u\n{body}\n")).unwrap();
        path
    }

    fn write_config(&self, script: &Path) -> PathBuf {
        self.write_config_with(script, "")
    }

    /// `extra` はトップレベルに追加する TOML 行。
    fn write_config_with(&self, script: &Path, extra: &str) -> PathBuf {
        let path = self.root.join("config.toml");
        let text = format!(
            r#"db = "celeris.sqlite3"
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

[[providers]]
id = "fake-local"
adapter = "fake"
tiers = ["frontier", "standard", "cheap"]
concurrency = 2
model = "fake"
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

    /// 成功を要求して stdout を返す。
    fn celerisctl(&self, args: &[&str]) -> String {
        let (code, stdout, stderr) = self.celerisctl_raw(args);
        assert_eq!(code, Some(0), "celerisctl {args:?} failed: {stdout}{stderr}");
        stdout
    }

    /// `(exit code, stdout, stderr)`。
    fn celerisctl_raw(&self, args: &[&str]) -> (Option<i32>, String, String) {
        let out = Command::new(bin("celerisctl")).arg("--db").arg(&self.db).args(args).output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// `celerisctl add ...` → `approve` し、ID を返す。
    fn add_approved(&self, args: &[&str]) -> TaskId {
        let id = self.add(args);
        self.celerisctl(&["approve", &id.to_string()]);
        id
    }

    fn add(&self, args: &[&str]) -> TaskId {
        let mut full = vec!["add", "--objective", "phase 7 scenario"];
        full.extend_from_slice(args);
        self.celerisctl(&full).trim().parse().unwrap()
    }

    fn run_celeris(&self, config: &Path, timeout: Duration) -> String {
        let log = self.root.join("celeris.log");
        let mut child = Command::new(bin("celeris"))
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

    fn replay_is_consistent(&self) {
        let out = self.celerisctl(&["replay"]);
        assert!(out.contains("replay: 0 mismatches"), "{out}");
    }

    fn task(&self, id: TaskId) -> Task {
        self.store.get(id).unwrap().unwrap()
    }

    fn events(&self, id: TaskId) -> Vec<Event> {
        self.store.events_for(id).unwrap().into_iter().map(|(_, e)| e).collect()
    }

    fn transitions(&self, id: TaskId) -> Vec<String> {
        self.events(id)
            .into_iter()
            .filter_map(|e| match e {
                Event::Transitioned { from, to, reason } => Some(format!("{from:?}->{to:?}:{reason}")),
                _ => None,
            })
            .collect()
    }

    fn approval_children(&self, parent: TaskId) -> Vec<Task> {
        let mut v: Vec<Task> = self
            .store
            .list(None)
            .unwrap()
            .into_iter()
            .filter(|t| t.parent_id == Some(parent) && t.kind == TaskKind::Approval)
            .collect();
        v.sort_by_key(|t| t.created_at);
        v
    }
}

/// 受け入れ 1 / 2: 回答が次 run に届き、CLI の `--check-cmd` / `--check-artifact` だけで作ったタスクが done になる。
#[test]
fn answer_is_delivered_in_context_answers_and_cli_checks_complete_the_task() {
    let env = Env::new();
    let ws = env.workspace("ws-answer");
    let script = env.write_script(
        r#"input=$(cat)
case "$input" in
  *'"answers"'*)
    printf '%s' "$input" > second-run.json
    mkdir -p artifacts && echo report > artifacts/report.md
    touch answered.txt
    echo '{"type":"done","summary":"used the answer","evidence":[]}'
    ;;
  *)
    echo '{"type":"question","text":"which version should I target?"}'
    ;;
esac"#,
    );
    let config = env.write_config(&script);
    let id = env.add_approved(&[
        "--title", "answer me", "--check-cmd", "test -f answered.txt", "--check-artifact", "report.md", "--workspace", &ws,
    ]);
    let checks: Vec<Check> = env.task(id).acceptance.into_iter().map(|c| c.check).collect();
    assert_eq!(
        checks,
        vec![
            Check::Command { cmd: "test -f answered.txt".into(), expect_exit: 0 },
            Check::ArtifactExists { name: "report.md".into() },
        ]
    );

    env.run_celeris(&config, Duration::from_secs(60));
    assert_eq!(env.task(id).status, Status::Blocked);

    env.celerisctl(&["answer", &id.to_string(), "target v2"]);
    assert!(env.events(id).iter().any(|e| matches!(
        e,
        Event::Answered { question, answer } if question == "which version should I target?" && answer == "target v2"
    )));

    env.run_celeris(&config, Duration::from_secs(60));
    let t = env.task(id);
    assert_eq!((t.status, t.attempts), (Status::Done, 0), "{:?}", env.events(id));
    let second = std::fs::read_to_string(Path::new(&ws).join("second-run.json")).unwrap();
    assert!(
        second.contains(r#""answers":[{"question":"which version should I target?","answer":"target v2"}]"#),
        "{second}"
    );
    assert_eq!(
        env.transitions(id),
        vec![
            "Draft->Ready:accept",
            "Ready->Running:dispatch",
            "Running->Blocked:worker_question",
            "Blocked->Ready:answer",
            "Ready->Running:dispatch",
            "Running->Reviewing:worker_done",
            "Reviewing->Done:review_pass",
        ]
    );
    env.replay_is_consistent();
}

/// 受け入れ 3: cancel は非終端のみ。先行の failed は後続へ推移的に伝播する。workspace 省略時は `<task_id>`。
#[test]
fn cancel_is_limited_to_non_terminal_tasks_and_failures_cancel_dependents() {
    let env = Env::new();
    let script = env.write_script(r#"cat >/dev/null; echo '{"type":"done","summary":"claimed","evidence":[]}'"#);
    let config = env.write_config(&script);
    let ws_a = env.workspace("ws-a");
    let a = env.add_approved(&["--title", "A", "--check-cmd", "false", "--max-retries", "0", "--workspace", &ws_a]);
    let a_str = a.to_string();
    let ws_b = env.workspace("ws-b");
    let b = env.add_approved(&["--title", "B", "--check-cmd", "true", "--depends-on", &a_str, "--workspace", &ws_b]);
    let b_str = b.to_string();
    let ws_c = env.workspace("ws-c");
    let c = env.add_approved(&["--title", "C", "--check-cmd", "true", "--depends-on", &b_str, "--workspace", &ws_c]);
    let d = env.add(&["--title", "D", "--check-cmd", "true"]);
    assert_eq!(env.task(d).workspace, WorkspaceSpec::Local { path: PathBuf::from(d.to_string()), mode: None });

    env.run_celeris(&config, Duration::from_secs(60));
    assert_eq!(env.task(a).status, Status::Failed);
    for id in [b, c] {
        assert_eq!(env.task(id).status, Status::Cancelled);
        assert_eq!(env.transitions(id), vec!["Draft->Ready:accept", "Ready->Cancelled:dependency_failed"]);
    }

    let out = env.celerisctl(&["cancel", &d.to_string()]);
    assert!(out.contains("Cancelled"), "{out}");
    assert_eq!(env.task(d).status, Status::Cancelled);

    let (code, _, stderr) = env.celerisctl_raw(&["cancel", &a_str]);
    assert_eq!(code, Some(1), "cancelling a failed task must exit 1");
    assert!(stderr.contains("cannot be cancelled"), "{stderr}");
    assert_eq!(env.task(a).status, Status::Failed);

    let (code, _, _) = env.celerisctl_raw(&["add", "--objective", "o", "--title", "E", "--check-cmd", "true", "--depends-on", &a_str]);
    assert_eq!(code, Some(1), "depending on a failed task must be rejected");
    env.replay_is_consistent();
}

/// 受け入れ 4: Human check は再レビューで新しい Approval 子を要求し、親を cancel すると未決の Approval 子が cancelled。
#[test]
fn human_check_asks_again_after_a_retry_and_orphaned_approvals_are_cancelled() {
    let env = Env::new();
    let script = env.write_script(
        r#"cat >/dev/null
if [ -f first-run-done ]; then touch second; else touch first-run-done; fi
echo '{"type":"done","summary":"ok","evidence":[]}'"#,
    );
    let config = env.write_config(&script);
    let ws_h = env.workspace("ws-h");
    let h = env.add_approved(&[
        "--title", "H", "--accept", "a human agrees", "--check-cmd", "test -f second", "--max-retries", "1", "--workspace", &ws_h,
    ]);

    env.run_celeris(&config, Duration::from_secs(60));
    let first = env.approval_children(h);
    assert_eq!(first.len(), 1);
    assert!(first[0].title.ends_with("(attempt 1)"), "{}", first[0].title);
    assert_eq!(env.task(h).status, Status::Reviewing);

    env.celerisctl(&["approve", &first[0].id.to_string()]);
    env.run_celeris(&config, Duration::from_secs(60));
    let all = env.approval_children(h);
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(all[1].title.ends_with("(attempt 2)"), "{}", all[1].title);
    let t = env.task(h);
    assert_eq!((t.status, t.attempts), (Status::Reviewing, 1));

    env.celerisctl(&["approve", &all[1].id.to_string()]);
    env.run_celeris(&config, Duration::from_secs(60));
    assert_eq!(env.task(h).status, Status::Done);

    let ws_o = env.workspace("ws-o");
    let o = env.add_approved(&["--title", "O", "--accept", "someone signs off", "--workspace", &ws_o]);
    env.run_celeris(&config, Duration::from_secs(60));
    let pending = env.approval_children(o);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, Status::Ready);
    env.celerisctl(&["cancel", &o.to_string()]);
    assert_eq!(env.task(o).status, Status::Cancelled);
    assert_eq!(env.task(pending[0].id).status, Status::Cancelled);
    env.replay_is_consistent();
}

/// 受け入れ 5: provider_failure 付きの error は attempts を消費せず requeue され、cooldown 明けに done。
#[test]
fn provider_failure_requeues_without_consuming_attempts() {
    let env = Env::new();
    let script = env.write_script(
        r#"cat >/dev/null
if [ -f throttled-once ]; then
  echo '{"type":"done","summary":"ok","evidence":[]}'
else
  touch throttled-once
  echo '{"type":"error","message":"429 rate limited","retryable":false,"provider_failure":{"kind":"throttled","retry_after_secs":1}}'
fi"#,
    );
    let config = env.write_config(&script);
    let ws = env.workspace("ws-p");
    let id = env.add_approved(&["--title", "P", "--check-cmd", "test -f throttled-once", "--max-retries", "0", "--workspace", &ws]);

    env.run_celeris(&config, Duration::from_secs(60));
    let t = env.task(id);
    assert_eq!((t.status, t.attempts), (Status::Done, 0), "{:?}", env.events(id));
    assert_eq!(
        env.transitions(id),
        vec![
            "Draft->Ready:accept",
            "Ready->Running:dispatch",
            "Running->Ready:requeue",
            "Ready->Running:dispatch",
            "Running->Reviewing:worker_done",
            "Reviewing->Done:review_pass",
        ]
    );
    assert!(env.events(id).iter().any(|e| matches!(
        e,
        Event::WorkerFinished { outcome, .. } if outcome.starts_with("requeue: ") && outcome.contains("throttled")
    )));
    env.replay_is_consistent();
}

/// ADR-0011（P-38）: 供給側失敗が続く場合、`max_requeues` 回の requeue の後は通常の失敗として attempts を消費し `failed`。
#[test]
fn persistent_provider_failure_stops_after_max_requeues() {
    let env = Env::new();
    let script = env.write_script(
        r#"cat >/dev/null
echo '{"type":"error","message":"429 rate limited","retryable":true,"provider_failure":{"kind":"throttled","retry_after_secs":1}}'"#,
    );
    let config = env.write_config_with(&script, "max_requeues = 2");
    let ws = env.workspace("ws-limit");
    let id = env.add_approved(&["--title", "L", "--check-cmd", "true", "--max-retries", "0", "--workspace", &ws]);

    env.run_celeris(&config, Duration::from_secs(60));
    let t = env.task(id);
    assert_eq!((t.status, t.attempts), (Status::Failed, 1), "{:?}", env.events(id));
    assert_eq!(
        env.transitions(id),
        vec![
            "Draft->Ready:accept",
            "Ready->Running:dispatch",
            "Running->Ready:requeue",
            "Ready->Running:dispatch",
            "Running->Ready:requeue",
            "Ready->Running:dispatch",
            "Running->Failed:worker_error",
        ]
    );
    assert!(env.events(id).iter().any(|e| matches!(
        e,
        Event::WorkerFinished { outcome, .. } if outcome.contains("requeue limit (2) reached")
    )));
    env.replay_is_consistent();
}
