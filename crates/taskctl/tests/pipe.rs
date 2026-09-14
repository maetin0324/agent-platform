//! `taskctl` が閉じたパイプへの出力で panic しないことを確認する統合テスト
//! （DESIGN.md §5.9「出力を閉じたパイプに流しても異常終了しないこと」、ADR-0010 D4）。
//!
//! 実バイナリを `taskctl --db <db> ls` として起動し、標準出力をパイプでつなぐ。少し読んで
//! からパイプを閉じ、プロセスが panic せず正常終了する（exit 0、stderr に "panicked" を
//! 含まない）ことを確認する。

use std::io::Read;
use std::process::{Command, Stdio};

use task_core::{
    ArtifactRef, Budget, Check, Criterion, SqliteStore, Status, Task, TaskId, TaskKind, TaskStore,
    Tier, WorkerHint, WorkspaceSpec,
};
use time::OffsetDateTime;

fn sample_task(i: usize) -> Task {
    let now = OffsetDateTime::now_utc();
    Task {
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: format!("task number {i} with a moderately long title for line length"),
        objective: "do some work".to_string(),
        acceptance: vec![Criterion {
            text: "tests pass".to_string(),
            check: Check::Command {
                cmd: "true".to_string(),
                expect_exit: 0,
            },
        }],
        inputs: vec![ArtifactRef {
            name: "spec".to_string(),
            path: "spec.md".to_string(),
            sha256: "abc".to_string(),
            kind: "doc".to_string(),
        }],
        depends_on: vec![],
        status: Status::Draft,
        priority: 0,
        worker_hint: WorkerHint {
            tier: Tier::Standard,
            adapter: None,
        },
        workspace: WorkspaceSpec::Local {
            path: format!("ws-{i}").into(),
        },
        budget: Budget {
            max_turns: 10,
            max_wall_secs: 600,
            max_retries: 2,
        },
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn ls_on_a_closed_pipe_exits_cleanly_without_panicking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("taskd.sqlite3");

    {
        let store = SqliteStore::open(&db_path).expect("open store");
        for i in 0..3000 {
            store.insert(&sample_task(i)).expect("insert task");
        }
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_taskctl"))
        .arg("--db")
        .arg(&db_path)
        .arg("ls")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn taskctl");

    // Read a little of stdout (much less than the several-thousand-line output),
    // then drop the handle to close our end of the pipe while the child is
    // presumably still writing more lines.
    {
        let mut stdout = child.stdout.take().expect("child stdout");
        let mut buf = [0u8; 256];
        let _ = stdout.read(&mut buf);
        // `stdout` is dropped here, closing the read end of the pipe.
    }

    let output = child.wait_with_output().expect("wait for taskctl");

    assert!(
        output.status.success(),
        "taskctl ls should exit successfully even if stdout is closed early: {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "taskctl should not panic on a closed stdout pipe, stderr: {stderr}"
    );
}
