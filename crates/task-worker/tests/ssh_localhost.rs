//! ADR-0018: `SshWorkspace` を **localhost への ssh** で確かめる（外部ネットワークに出ない）。
//! `taskd-localhost` への多重接続が無い環境では確認できないので、その場合は skip する（失敗させない）。

use std::path::PathBuf;
use std::time::Duration;

use task_core::{ArtifactRef, Budget, Check, Criterion, Status, Task, TaskId, TaskKind, Tier, WorkerHint, WorkspaceSpec};
use task_worker::{SshSettings, SshWorkspace, SyncMode, Workspace};
use time::OffsetDateTime;

const HOST: &str = "taskd-localhost";

fn task(dir: &std::path::Path) -> Task {
    let now = OffsetDateTime::now_utc();
    Task {
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: "ssh workspace".into(),
        objective: "o".into(),
        acceptance: vec![Criterion { text: "c".into(), check: Check::Command { cmd: "true".into(), expect_exit: 0 } }],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Ready,
        priority: 0,
        worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
        workspace: WorkspaceSpec::Local { path: dir.to_path_buf() },
        budget: Budget { max_turns: 1, max_wall_secs: 60, max_retries: 0 },
        attempts: 0,
        lease: None,
        created_at: now,
        updated_at: now,
    }
}

fn settings(remote: PathBuf) -> SshSettings {
    let mut s = SshSettings::new("localtest", HOST, remote);
    s.sync = SyncMode::Rsync;
    s
}

async fn available(ws: &SshWorkspace) -> bool {
    if ws.control_master_alive().await {
        return true;
    }
    eprintln!("skip: {HOST} への多重接続が無い（scripts/cluster-login.sh {HOST} で張れる）");
    false
}

#[tokio::test]
async fn pushes_runs_and_pulls_over_ssh() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let remote_dir = remote.path().join("work");
    let ws = SshWorkspace::new(local.path(), settings(remote_dir.clone()));
    if !available(&ws).await {
        return;
    }
    let t = task(local.path());

    let dir = ws.prepare(&t).await.expect("prepare");
    assert_eq!(dir, local.path().canonicalize().unwrap());
    std::fs::write(local.path().join("input.txt"), "hello\n").unwrap();
    ws.push().await.expect("push");
    assert!(remote_dir.join("input.txt").exists(), "rsync でリモートに届く");

    // コマンドはリモートで動く（リモートにしか無いファイルを読める）。
    std::fs::write(remote_dir.join("only-remote.txt"), "remote\n").unwrap();
    let r = ws.exec("cat only-remote.txt", Duration::from_secs(30)).await.expect("exec");
    assert_eq!((r.exit, r.stdout_tail.trim()), (Some(0), "remote"), "{r:?}");

    // 成果物はリモートで作られ、collect が取り込んでローカルで sha256 を計算する。
    let r = ws
        .exec("mkdir -p artifacts && printf 'from the cluster\\n' > artifacts/report.md", Duration::from_secs(30))
        .await
        .expect("exec artifact");
    assert_eq!(r.exit, Some(0), "{r:?}");
    let artifacts: Vec<ArtifactRef> = ws.collect(&t).await.expect("collect");
    assert_eq!(artifacts.len(), 1, "{artifacts:?}");
    assert_eq!(artifacts[0].name, "report.md");
    assert_eq!(
        std::fs::read_to_string(local.path().join("artifacts/report.md")).unwrap(),
        "from the cluster\n",
        "pull でローカルに戻る"
    );

    // 失敗するコマンドの終了コードはそのまま返る（接続の問題ではなく判定の失敗）。
    let r = ws.exec("exit 3", Duration::from_secs(30)).await.expect("exec exit 3");
    assert_eq!(r.exit, Some(3), "{r:?}");
}

#[tokio::test]
async fn writes_remote_exec_helper_that_runs_on_the_cluster() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let remote_dir = remote.path().join("work");
    let ws = SshWorkspace::new(local.path(), settings(remote_dir.clone()));
    if !available(&ws).await {
        return;
    }
    let t = task(local.path());
    ws.prepare(&t).await.expect("prepare");
    let helper = ws.write_remote_exec_helper().await.expect("helper");
    assert!(helper.ends_with(".taskd/remote-exec"));

    std::fs::write(remote_dir.join("marker.txt"), "cluster side\n").unwrap();
    let out = std::process::Command::new(&helper).arg("cat marker.txt").output().expect("run helper");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "cluster side");

    ws.push().await.expect("push");
    assert!(!remote_dir.join(".taskd").exists(), "ラッパはリモートへ送らない");
}

#[tokio::test]
async fn missing_control_master_is_unreachable() {
    let local = tempfile::tempdir().unwrap();
    let mut s = settings(PathBuf::from("/nonexistent"));
    s.host = "taskd-no-such-host-for-tests".into();
    let ws = SshWorkspace::new(local.path(), s);
    assert!(!ws.control_master_alive().await, "多重接続は無い");
    let err = ws.exec("true", Duration::from_secs(10)).await.expect_err("unreachable");
    assert!(matches!(err, task_worker::WorkspaceError::Unreachable(_)), "{err:?}");
}
