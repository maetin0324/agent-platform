//! Phase 4 ドッグフード用のシード実行ファイル（ADR-0006 D7）。Phase 6 で `--adapter` を追加し、
//! `codex` アダプタでも同じドッグフードタスクを投入できるようにした（ADR-0008、DESIGN §6 Phase 6:
//! 「codex アダプタで Phase 4 と同じドッグフードタスクが通る」）。
//!
//! `taskctl add` は `Check::Human` しか作れない（ADR-0004 D3）ため、`examples/hello-crate` に対する
//! `Check::Command` 付きの実タスクを、Phase 3 の `tests/e2e` と同じ方法（`TaskStore` API を直接呼ぶ）で
//! 1 件だけ `ready` として投入する。人間が `docs/DESIGN.md` §6 Phase 4/6 の受け入れ条件を確認するための道具。
//!
//! 使い方:
//! ```text
//! cargo run -p taskd --example seed_hello_crate_task -- \
//!   --db /path/to/taskd-demo.sqlite3 --workspace /path/to/agent-platform/examples/hello-crate \
//!   --adapter claude-code   # または --adapter codex（省略時は claude-code）
//! taskctl --db /path/to/taskd-demo.sqlite3 show <id を上の出力から>
//! ```

use std::path::PathBuf;

use clap::Parser;
use task_core::{
    Budget, Check, Criterion, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore, Tier,
    WorkerHint, WorkspaceSpec,
};

#[derive(Parser, Debug)]
struct Cli {
    /// `taskctl --db` / `taskd.toml` の `db` と同じファイルを指すこと。
    #[arg(long)]
    db: PathBuf,
    /// `examples/hello-crate` の絶対パス。`taskd.toml` の provider が `--adapter` に対応する設定である前提。
    #[arg(long)]
    workspace: PathBuf,
    /// `worker_hint.adapter`（`taskd.toml` の `[[providers]]` の `adapter` と一致させること）。
    #[arg(long, default_value = "claude-code")]
    adapter: String,
}

fn main() {
    let cli = Cli::parse();
    let store = SqliteStore::open(&cli.db).expect("open store");

    let now = time::OffsetDateTime::now_utc();
    let task = Task {
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: "Add a usage example to hello-crate's README".to_string(),
        objective: "In this workspace there is a small Rust crate (hello-crate) with a single \
            public function `greet(name: &str) -> String` in src/lib.rs. README.md currently has \
            no usage example (see the TODO comment in it). Add a short \"Usage\" section to \
            README.md that shows how to call `hello_crate::greet` (a short Rust code block is \
            fine), and make sure `cargo test` still passes."
            .to_string(),
        acceptance: vec![
            Criterion {
                text: "cargo test exits 0".into(),
                check: Check::Command {
                    cmd: "cargo test".into(),
                    expect_exit: 0,
                },
            },
            Criterion {
                text: "README.md mentions the `greet` function".into(),
                check: Check::Command {
                    cmd: "grep -q 'greet' README.md".into(),
                    expect_exit: 0,
                },
            },
        ],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Ready,
        priority: 0,
        worker_hint: WorkerHint {
            tier: Tier::Standard,
            adapter: Some(cli.adapter.clone()),
        },
        workspace: WorkspaceSpec::Local { path: cli.workspace },
        budget: Budget {
            max_turns: 30,
            max_wall_secs: 600,
            max_retries: 0,
        },
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
        role: None,
        aggregate: false,
    };

    let WorkspaceSpec::Local { path: workspace_path } = &task.workspace else {
        unreachable!("this seed always creates a Local workspace")
    };
    let workspace_display = workspace_path.display().to_string();

    // ADR-0010 D2: insert と Created を 1 トランザクションで。
    store.create_task(&task, vec![]).expect("create task");

    println!("seeded task {} (status=ready, workspace={workspace_display})", task.id);
}
