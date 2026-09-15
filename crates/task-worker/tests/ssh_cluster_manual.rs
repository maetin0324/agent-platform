//! 実クラスタ（pegasus / sirius）での確認。既定では走らせない（`#[ignore]`）。
//! 実行: `TASKD_CLUSTER_HOST=pegasus TASKD_CLUSTER_WORKDIR=/work/NBB/rmaeda/taskd-test \
//!        cargo test -p task-worker --test ssh_cluster_manual -- --ignored --nocapture`
//! 事前に `scripts/cluster-login.sh <host>` で多重接続を張っておくこと（2 要素認証は人が通す）。

use std::path::PathBuf;
use std::time::Duration;

use task_core::{Budget, Check, Criterion, Status, Task, TaskId, TaskKind, Tier, WorkerHint, WorkspaceSpec};
use task_worker::{SshSettings, SshWorkspace, SyncMode, Workspace};
use time::OffsetDateTime;

fn task(dir: &std::path::Path) -> Task {
    let now = OffsetDateTime::now_utc();
    Task {
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: "cluster smoke".into(),
        objective: "o".into(),
        acceptance: vec![Criterion { text: "c".into(), check: Check::Command { cmd: "true".into(), expect_exit: 0 } }],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Ready,
        priority: 0,
        worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
        workspace: WorkspaceSpec::Local { path: dir.to_path_buf() },
        budget: Budget { max_turns: 1, max_wall_secs: 120, max_retries: 0 },
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
        role: None,
        aggregate: false,
    }
}

#[tokio::test]
#[ignore = "実クラスタが要る（TASKD_CLUSTER_HOST / TASKD_CLUSTER_WORKDIR と多重接続）"]
async fn cluster_round_trip() {
    let host = std::env::var("TASKD_CLUSTER_HOST").expect("TASKD_CLUSTER_HOST");
    let workdir = std::env::var("TASKD_CLUSTER_WORKDIR").expect("TASKD_CLUSTER_WORKDIR");
    let local = tempfile::tempdir().unwrap();
    let remote_dir = PathBuf::from(&workdir).join(format!("smoke-{}", TaskId::new()));
    let mut settings = SshSettings::new(host.clone(), host.clone(), remote_dir.clone());
    settings.sync = SyncMode::Rsync;
    let ws = SshWorkspace::new(local.path(), settings);

    assert!(ws.control_master_alive().await, "多重接続が要る: scripts/cluster-login.sh {host}");
    let t = task(local.path());
    ws.prepare(&t).await.expect("prepare");

    // クラスタ側でだけ分かること（ホスト名）を確かめる。
    let r = ws.exec("hostname; pwd", Duration::from_secs(60)).await.expect("exec");
    println!("remote hostname/pwd:\n{}", r.stdout_tail);
    assert_eq!(r.exit, Some(0), "{r:?}");
    assert!(r.stdout_tail.contains(remote_dir.to_string_lossy().as_ref()), "作業ディレクトリで動く: {r:?}");

    // 成果物をクラスタで作り、取り込む。
    let r = ws
        .exec("mkdir -p artifacts && hostname > artifacts/where.txt", Duration::from_secs(60))
        .await
        .expect("exec artifact");
    assert_eq!(r.exit, Some(0), "{r:?}");
    let artifacts = ws.collect(&t).await.expect("collect");
    assert_eq!(artifacts.len(), 1, "{artifacts:?}");
    let body = std::fs::read_to_string(local.path().join("artifacts/where.txt")).unwrap();
    println!("artifact from the cluster: {}", body.trim());
    assert!(!body.trim().is_empty());

    // 後始末（リモートの一時ディレクトリを消す）。
    let r = ws
        .exec(&format!("cd / && rm -rf {}", remote_dir.to_string_lossy()), Duration::from_secs(60))
        .await
        .expect("cleanup");
    assert_eq!(r.exit, Some(0), "{r:?}");
}
