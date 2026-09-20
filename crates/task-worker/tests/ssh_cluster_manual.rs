//! 実クラスタ（pegasus / sirius）での確認。既定では走らせない（`#[ignore]`）。
//! 実行: `CELERIS_CLUSTER_HOST=pegasus CELERIS_CLUSTER_WORKDIR=/work/NBB/rmaeda/celeris-test \
//!        cargo test -p task-worker --test ssh_cluster_manual -- --ignored --nocapture`
//! 事前に `scripts/cluster-login.sh <host>` で多重接続を張っておくこと（2 要素認証は人が通す）。

use std::path::PathBuf;
use std::time::Duration;

use task_core::{Budget, Check, Criterion, Status, Task, TaskId, TaskKind, Tier, WorkerHint, WorkspaceSpec};
use task_worker::{SshSettings, SshWorkspace, SyncMode, Workspace};
use time::OffsetDateTime;

fn task(dir: &std::path::Path) -> Task {
    let now = OffsetDateTime::now_utc();
    Task { mode: Default::default(), skills: Vec::new(),
        repos: Vec::new(),
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
        workspace: WorkspaceSpec::Local { path: dir.to_path_buf(), mode: None },
        budget: Budget { max_turns: 1, max_wall_secs: 120, max_retries: 0 },
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

#[tokio::test]
#[ignore = "実クラスタが要る（CELERIS_CLUSTER_HOST / CELERIS_CLUSTER_WORKDIR と多重接続）"]
async fn cluster_round_trip() {
    let host = std::env::var("CELERIS_CLUSTER_HOST").expect("CELERIS_CLUSTER_HOST");
    let workdir = std::env::var("CELERIS_CLUSTER_WORKDIR").expect("CELERIS_CLUSTER_WORKDIR");
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

/// ADR-0019: 実クラスタの既存リポジトリ（既定 benchfs）で `sync = "worktree"` を確かめる。
/// 実行:
/// ```sh
/// CELERIS_CLUSTER_HOST=pegasus CELERIS_CLUSTER_PROJECT=/work/NBB/rmaeda/workspace/rust/benchfs \
///   cargo test -p task-worker --test ssh_cluster_manual -- --ignored --nocapture worktree
/// ```
/// 後片付け（`git worktree remove`）はテストの最後に行う（ADR-0019 D2 の運用では人が行うが、確認用の worktree は残さない）。
#[tokio::test]
#[ignore = "実クラスタが要る（CELERIS_CLUSTER_HOST / CELERIS_CLUSTER_PROJECT と多重接続）"]
async fn cluster_worktree_brings_only_the_sparse_paths() {
    let host = std::env::var("CELERIS_CLUSTER_HOST").expect("CELERIS_CLUSTER_HOST");
    let project = PathBuf::from(std::env::var("CELERIS_CLUSTER_PROJECT").expect("CELERIS_CLUSTER_PROJECT"));
    let local = tempfile::tempdir().unwrap();
    let task_id = TaskId::new();

    let mut settings = SshSettings::new(host.clone(), host.clone(), project.clone());
    settings.sync = SyncMode::Worktree;
    settings.task_id = task_id.to_string();
    settings.worktree.paths = vec!["src".to_string(), "Cargo.toml".to_string()];
    let ws = SshWorkspace::new(local.path(), settings.clone());

    assert!(ws.control_master_alive().await, "多重接続が要る: scripts/cluster-login.sh {host}");
    let mut t = task(local.path());
    t.id = task_id;
    let started = std::time::Instant::now();
    ws.prepare(&t).await.expect("prepare（worktree を切って pull）");
    println!("prepare took {:?}", started.elapsed());

    // sparse-checkout で指定した分だけが手元に来る。巨大なベンチ結果は来ない。
    assert!(local.path().join("src").is_dir(), "src が来る");
    assert!(local.path().join("Cargo.toml").is_file(), "Cargo.toml が来る");
    assert!(
        !local.path().join("lib/pluvio/examples/mpi_example/results").exists(),
        "sparse-checkout の外（39 GB のベンチ結果）は来ない"
    );
    let size = std::process::Command::new("du").args(["-sm", &local.path().to_string_lossy()]).output().unwrap();
    println!("mirror size: {}", String::from_utf8_lossy(&size.stdout).trim());

    // コマンドは worktree の中で走る。元のリポジトリの作業ツリーには触らない。
    let r = ws.exec("pwd && git rev-parse --abbrev-ref HEAD", Duration::from_secs(120)).await.expect("exec");
    println!("remote pwd/branch:\n{}", r.stdout_tail);
    assert_eq!(r.exit, Some(0), "{r:?}");
    assert!(r.stdout_tail.contains(&format!("celeris/{task_id}")), "ブランチは celeris/<task_id>: {r:?}");

    // 後片付け: 確認用の worktree とブランチを消す（本番の運用では人が行う。ADR-0019 D2）。
    let wt = settings.worktree_dir();
    let cleanup = std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            &host,
            &format!(
                "git -C {} worktree remove --force {} && git -C {} branch -D celeris/{}",
                project.display(),
                wt.display(),
                project.display(),
                task_id
            ),
        ])
        .output()
        .unwrap();
    println!("cleanup: {}{}", String::from_utf8_lossy(&cleanup.stdout), String::from_utf8_lossy(&cleanup.stderr));
    assert!(cleanup.status.success(), "worktree の後片付けに失敗した");
}
