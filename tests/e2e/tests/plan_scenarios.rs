//! DESIGN §6 Phase 5 の受け入れ条件（ADR-0007）。fake ワーカー（`sh` スクリプト）と実バイナリ
//! `taskctl` / `taskd` だけで動き、ネットワークに出ない。
//!
//! 1. `taskctl plan "<大目標>"` → `taskctl approve` → `taskd` がプランナー（fake）を走らせ、`artifacts/plan.json` から
//!    4 個の子タスクを `draft` で生成する（`plan.auto_accept = false`）。子は人間が `taskctl approve` するまで
//!    dispatch されない。承認後に `taskd` を再実行すると全て `done`（うち 1 つは `Reviewer` 条件を fake の
//!    レビュー run で判定）。`taskctl replay` 差分ゼロ。
//! 2. 1 回目の `plan.json` が不正（依存が範囲外）→ `review_fail` → 2 回目の run の `prior_review` に理由が渡り、
//!    正しい plan を書いて `done`（`max_retries = 1`）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use task_core::{Event, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, Tier};

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

    fn write_config(&self, max_concurrency: usize, script: &Path) -> PathBuf {
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

[plan]
auto_accept = false

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

    fn taskctl(&self, args: &[&str]) -> String {
        let out = Command::new(bin("taskctl"))
            .arg("--db")
            .arg(&self.db)
            .args(args)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "taskctl {args:?} failed: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
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
        let out = self.taskctl(&["replay"]);
        assert!(out.contains("replay: 0 mismatches"), "{out}");
    }

    fn task(&self, id: TaskId) -> Task {
        self.store.get(id).unwrap().unwrap()
    }

    fn children_of(&self, parent: TaskId) -> Vec<Task> {
        let mut v: Vec<Task> = self
            .store
            .list(None)
            .unwrap()
            .into_iter()
            .filter(|t| t.parent_id == Some(parent))
            .collect();
        v.sort_by(|a, b| a.title.cmp(&b.title));
        v
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

/// プランナー用の plan.json（4 タスク。B は A に、C は A に依存し `reviewer` 条件、D は B と C に依存し `artifact_exists`）。
const PLAN_JSON: &str = r#"{"tasks":[
 {"title":"A","objective":"add clap dependency and a main.rs skeleton","acceptance":[{"text":"A ran","check":{"type":"command","cmd":"test -f A.txt","expect_exit":0}}]},
 {"title":"B","objective":"parse --name argument","acceptance":[{"text":"B ran","check":{"type":"command","cmd":"test -f B.txt","expect_exit":0}}],"depends_on":[0]},
 {"title":"C","objective":"write tests for the CLI","acceptance":[{"text":"tests are meaningful","check":{"type":"reviewer"}}],"depends_on":[0],"tier":"cheap"},
 {"title":"D","objective":"document usage in README","acceptance":[{"text":"report exists","check":{"type":"artifact_exists","name":"D-report.md"}}],"depends_on":[1,2]}
]}"#;

/// stdin の `run` 行から `task.kind` と `task.title` を取り、kind ごとに振る舞う fake ワーカー。
/// `kind = plan` は `$PLAN_FILE`（テストが用意）を `artifacts/plan.json` にコピー、`kind = review` は全条件 pass の
/// `artifacts/review.json`、それ以外は `<title>.txt` と `artifacts/<title>-report.md` を作る。
fn worker_script(plan_source: &str, invalid_first: bool) -> String {
    let plan_step = if invalid_first {
        format!(
            r#"N=$(ls plan-run-*.marker 2>/dev/null | wc -l)
    cp "$RUN" "plan-run-$N.marker"
    if [ "$N" -eq 0 ]; then
      printf '%s' '{{"tasks":[{{"title":"bad","objective":"o","acceptance":[{{"text":"c","check":{{"type":"human"}}}}],"depends_on":[9]}}]}}' > artifacts/plan.json
    else
      cp "{plan_source}" artifacts/plan.json
    fi"#
        )
    } else {
        format!(r#"cp "{plan_source}" artifacts/plan.json"#)
    };
    format!(
        r##"RUN=$(mktemp)
cat > "$RUN"
KIND=$(grep -o '"kind":"[a-z]*"' "$RUN" | head -1 | cut -d'"' -f4)
TITLE=$(grep -o '"title":"[^"]*"' "$RUN" | head -1 | cut -d'"' -f4)
mkdir -p artifacts
echo "$KIND $TITLE $(date +%s.%N)" >> timeline.log
case "$KIND" in
  plan)
    {plan_step}
    echo '{{"type":"artifact","name":"plan.json","path":"artifacts/plan.json"}}'
    echo '{{"type":"done","summary":"planned","evidence":[]}}'
    ;;
  review)
    cp "$RUN" "review-run.json"
    printf '%s' '{{"verdicts":[{{"criterion":0,"pass":true,"reason":"tests cover the parser"}}]}}' > artifacts/review.json
    echo '{{"type":"done","summary":"reviewed","evidence":[]}}'
    ;;
  *)
    sleep 0.2
    touch "$TITLE.txt"
    echo "# $TITLE" > "artifacts/$TITLE-report.md"
    echo '{{"type":"artifact","name":"'"$TITLE"'-report.md","path":"artifacts/'"$TITLE"'-report.md"}}'
    echo '{{"type":"done","summary":"did '"$TITLE"'","evidence":[]}}'
    ;;
esac
rm -f "$RUN"
"##
    )
}

const GOAL: &str = "examples/hello-crate に CLI 引数パースを追加し、テストとREADMEを整備";

/// シナリオ 1: plan → approve → 子 4 件が draft → 人間承認 → 全て done。
#[test]
fn taskctl_plan_generates_children_that_complete_after_human_approval() {
    let env = Env::new();
    let plan_source = env.root.join("plan-source.json");
    std::fs::write(&plan_source, PLAN_JSON).unwrap();
    let script = env.write_script(&worker_script(plan_source.to_str().unwrap(), false));
    let config = env.write_config(2, &script);
    let dir = env.root.join("workspaces").join("hello-crate");
    std::fs::create_dir_all(&dir).unwrap();

    // taskctl plan "<大目標>" → Plan タスク（draft、Frontier、acceptance 空）。
    let id: TaskId = env
        .taskctl(&["plan", GOAL, "--workspace", dir.to_str().unwrap()])
        .trim()
        .parse()
        .unwrap();
    let plan = env.task(id);
    assert_eq!(plan.kind, TaskKind::Plan);
    assert_eq!(plan.status, Status::Draft);
    assert_eq!(plan.objective, GOAL);
    assert!(plan.acceptance.is_empty());
    assert_eq!(plan.worker_hint.tier, Tier::Frontier);
    assert_eq!(plan.budget.max_retries, 1);

    // taskctl approve → ready。taskd がプランナーを走らせ、子を draft で挿入して plan は done。
    assert_eq!(env.taskctl(&["approve", &id.to_string()]).trim(), "Ready");
    let log1 = env.run_taskd(&config, Duration::from_secs(60));
    let plan = env.task(id);
    assert_eq!(plan.status, Status::Done, "{log1}");
    assert_eq!(plan.attempts, 0);
    assert_eq!(
        env.transitions(id),
        vec![
            "Draft->Ready:accept",
            "Ready->Running:dispatch",
            "Running->Reviewing:worker_done",
            "Reviewing->Done:review_pass",
        ]
    );
    let plan_verdicts: Vec<(usize, bool, String)> = env
        .store
        .events_for(id)
        .unwrap()
        .into_iter()
        .filter_map(|(_, e)| match e {
            Event::ReviewVerdict { criterion_idx, pass, reason, .. } => Some((criterion_idx, pass, reason)),
            _ => None,
        })
        .collect();
    assert_eq!(plan_verdicts.len(), 1, "{plan_verdicts:?}");
    assert_eq!(plan_verdicts[0].0, 0);
    assert!(plan_verdicts[0].1);
    assert!(plan_verdicts[0].2.contains("valid PlanOutput with 4 tasks"), "{}", plan_verdicts[0].2);
    assert!(log1.contains("plan completed; children inserted"), "{log1}");

    // 子 4 件（3〜6 件の範囲）が draft で、plan.auto_accept = false なので dispatch されていない。
    let children = env.children_of(id);
    assert_eq!(children.len(), 4);
    assert!((3..=6).contains(&children.len()));
    let by_title = |t: &str| children.iter().find(|c| c.title == t).unwrap().clone();
    for c in &children {
        assert_eq!(c.status, Status::Draft);
        assert_eq!(c.kind, TaskKind::Execute);
        assert_eq!(c.workspace, plan.workspace);
        assert_eq!(c.budget, plan.budget);
        let ev = env.store.events_for(c.id).unwrap();
        assert_eq!(ev.len(), 1, "child must have only Created: {ev:?}");
        assert!(matches!(&ev[0].1, Event::Created { .. }));
    }
    assert_eq!(by_title("B").depends_on, vec![by_title("A").id]);
    assert_eq!(by_title("C").depends_on, vec![by_title("A").id]);
    assert_eq!(by_title("D").depends_on, vec![by_title("B").id, by_title("C").id]);
    assert_eq!(by_title("C").worker_hint.tier, Tier::Cheap);
    assert_eq!(by_title("A").worker_hint.tier, Tier::Standard);
    let ls = env.taskctl(&["ls", "--status", "draft"]);
    assert_eq!(ls.lines().count(), 4, "{ls}");
    let tree = env.taskctl(&["ls", "--tree"]);
    assert!(tree.contains(&id.to_string()));
    assert!(tree.lines().filter(|l| l.starts_with("  ")).count() >= 4, "{tree}");

    // 人間が子を承認 → taskd 再実行 → 全て done。
    for c in &children {
        assert_eq!(env.taskctl(&["approve", &c.id.to_string()]).trim(), "Ready");
    }
    let log2 = env.run_taskd(&config, Duration::from_secs(60));
    for c in &children {
        let t = env.task(c.id);
        assert_eq!(t.status, Status::Done, "{}: {:?}\n{log2}", c.title, env.transitions(c.id));
        assert_eq!(t.attempts, 0);
        assert!(t.lease.is_none());
        assert_eq!(
            env.transitions(c.id),
            vec![
                "Draft->Ready:accept",
                "Ready->Running:dispatch",
                "Running->Reviewing:worker_done",
                "Reviewing->Done:review_pass",
            ]
        );
    }
    // C の Reviewer 条件は fake のレビュー run（合成 Review タスク + context.review）で判定された。
    let c = by_title("C");
    let c_events = env.store.events_for(c.id).unwrap();
    assert!(c_events.iter().any(|(_, e)| matches!(e, Event::ReviewVerdict { criterion_idx: 0, pass: true, reason, .. } if reason.contains("reviewer(") && reason.contains("tests cover the parser"))), "{c_events:?}");
    assert!(c_events.iter().any(|(_, e)| matches!(e, Event::WorkerProgress { msg, .. } if msg.starts_with("reviewer run ") && msg.contains("started"))), "{c_events:?}");
    // ADR-0014 D1（P-G14）: WorkerStarted はワーカー run の 1 回と、provider 付きで記録する Reviewer run の 1 回。
    assert_eq!(c_events.iter().filter(|(_, e)| matches!(e, Event::WorkerStarted { role: None, .. })).count(), 1);
    assert_eq!(
        c_events
            .iter()
            .filter(|(_, e)| matches!(e, Event::WorkerStarted { role: Some(task_core::RunRole::Reviewer), provider: Some(_), .. }))
            .count(),
        1,
        "{c_events:?}"
    );
    assert!(c_events.iter().any(|(_, e)| matches!(e, Event::WorkerFinished { role: Some(task_core::RunRole::Reviewer), outcome, .. } if outcome.starts_with("done: "))), "{c_events:?}");
    let review_run = std::fs::read_to_string(dir.join("review-run.json")).unwrap();
    assert!(review_run.contains(r#""kind":"review""#), "{review_run}");
    assert!(review_run.contains(r#""title":"Review: C""#), "{review_run}");
    assert!(review_run.contains(r#""review":{"summary":"did C","evidence":[],"criteria":[0]}"#), "{review_run}");
    assert!(review_run.contains(r#""inputs":[{"name":"C-report.md""#), "{review_run}");
    assert!(log2.contains("starting reviewer run"), "{log2}");

    // 依存順: D は B と C の後、B/C は A の後（timeline.log は fake が書く）。
    let timeline = std::fs::read_to_string(dir.join("timeline.log")).unwrap();
    let start_of = |kind: &str, title: &str| -> f64 {
        timeline
            .lines()
            .find(|l| l.starts_with(&format!("{kind} {title} ")))
            .unwrap_or_else(|| panic!("no {kind} {title} in {timeline}"))
            .rsplit(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    assert!(start_of("execute", "A") < start_of("execute", "B"));
    assert!(start_of("execute", "A") < start_of("execute", "C"));
    assert!(start_of("execute", "B") < start_of("execute", "D"));
    assert!(start_of("review", "Review: C") < start_of("execute", "D"));

    env.replay_is_consistent();
}

/// シナリオ 2: 不正な plan.json は 1 回だけリトライされ、2 回目に prior_review を受けて done。
#[test]
fn invalid_plan_is_retried_once_with_prior_review() {
    let env = Env::new();
    let plan_source = env.root.join("plan-source.json");
    std::fs::write(&plan_source, PLAN_JSON).unwrap();
    let script = env.write_script(&worker_script(plan_source.to_str().unwrap(), true));
    let config = env.write_config(1, &script);
    let dir = env.root.join("workspaces").join("retry");
    std::fs::create_dir_all(&dir).unwrap();

    let id: TaskId = env
        .taskctl(&["plan", "retry goal", "--workspace", dir.to_str().unwrap()])
        .trim()
        .parse()
        .unwrap();
    env.taskctl(&["approve", &id.to_string()]);
    let log = env.run_taskd(&config, Duration::from_secs(60));

    let plan = env.task(id);
    assert_eq!(plan.status, Status::Done, "{log}");
    assert_eq!(plan.attempts, 1);
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
    assert!(!verdicts[0].0 && verdicts[0].1.contains("out of range"), "{:?}", verdicts[0]);
    assert!(verdicts[1].0);
    // 2 回目の run の stdin に 1 回目の検証エラーが prior_review として入っている。
    let run1 = std::fs::read_to_string(dir.join("plan-run-1.marker")).unwrap();
    assert!(run1.contains(r#""prior_review":[{"criterion":0,"pass":false"#), "{run1}");
    assert!(run1.contains("out of range"), "{run1}");
    assert!(run1.contains(r#""attempts":1"#));
    // 1 回目の不正な plan からは子が作られていない。
    let children = env.children_of(id);
    assert_eq!(children.len(), 4);
    assert!(children.iter().all(|c| c.status == Status::Draft));
    env.replay_is_consistent();
}
