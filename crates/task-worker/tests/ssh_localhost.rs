//! ADR-0018: `SshWorkspace` を **localhost への ssh** で確かめる（外部ネットワークに出ない）。
//! `celeris-localhost` への多重接続が無い環境では確認できないので、その場合は skip する（失敗させない）。

use std::path::PathBuf;
use std::time::Duration;

use task_core::{
    ArtifactRef, Budget, Check, Criterion, Status, Task, TaskId, TaskKind, Tier, WorkerHint,
    WorkspaceSpec,
};
use task_worker::{SshSettings, SshWorkspace, SyncMode, Workspace};
use time::OffsetDateTime;

const HOST: &str = "celeris-localhost";

fn task(dir: &std::path::Path) -> Task {
    let now = OffsetDateTime::now_utc();
    Task {
        routing: None,
        mode: Default::default(),
        skills: Vec::new(),
        repos: Vec::new(),
        id: TaskId::new(),
        parent_id: None,
        kind: TaskKind::Execute,
        title: "ssh workspace".into(),
        objective: "o".into(),
        acceptance: vec![Criterion {
            text: "c".into(),
            check: Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        }],
        inputs: vec![],
        depends_on: vec![],
        status: Status::Ready,
        priority: 0,
        worker_hint: WorkerHint {
            tier: Tier::Standard,
            adapter: None,
        },
        workspace: WorkspaceSpec::Local {
            path: dir.to_path_buf(),
            mode: None,
        },
        budget: Budget {
            max_turns: 1,
            max_wall_secs: 60,
            max_retries: 0,
        },
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

    // クラスタ側に既にあるプロジェクトを指す（ADR-0018 D1: クラスタが正）。
    std::fs::create_dir_all(&remote_dir).unwrap();
    std::fs::write(remote_dir.join("existing.txt"), "already here\n").unwrap();

    let dir = ws.prepare(&t).await.expect("prepare");
    assert_eq!(dir, local.path().canonicalize().unwrap());
    assert_eq!(
        std::fs::read_to_string(local.path().join("existing.txt")).unwrap(),
        "already here\n",
        "prepare が pull して既存プロジェクトを写す"
    );

    std::fs::write(local.path().join("input.txt"), "hello\n").unwrap();
    ws.push().await.expect("push");
    assert!(
        remote_dir.join("input.txt").exists(),
        "rsync でリモートに届く"
    );
    assert!(
        remote_dir.join("existing.txt").exists(),
        "既定の push は既存ファイルを消さない"
    );

    // コマンドはリモートで動く（リモートにしか無いファイルを読める）。
    std::fs::write(remote_dir.join("only-remote.txt"), "remote\n").unwrap();
    let r = ws
        .exec("cat only-remote.txt", Duration::from_secs(30))
        .await
        .expect("exec");
    assert_eq!((r.exit, r.stdout_tail.trim()), (Some(0), "remote"), "{r:?}");

    // 成果物はリモートで作られ、collect が取り込んでローカルで sha256 を計算する。
    let r = ws
        .exec(
            "mkdir -p artifacts && printf 'from the cluster\\n' > artifacts/report.md",
            Duration::from_secs(30),
        )
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
    let r = ws
        .exec("exit 3", Duration::from_secs(30))
        .await
        .expect("exec exit 3");
    assert_eq!(r.exit, Some(3), "{r:?}");
}

/// ADR-0018 D4: `delete_on_push = true`（celeris 専用の作業ディレクトリ向け）のときだけ、手元に無いものを消す。
#[tokio::test]
async fn push_deletes_only_when_asked() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let remote_dir = remote.path().join("work");
    let mut s = settings(remote_dir.clone());
    s.delete_on_push = true;
    let ws = SshWorkspace::new(local.path(), s);
    if !available(&ws).await {
        return;
    }
    std::fs::create_dir_all(&remote_dir).unwrap();
    std::fs::write(remote_dir.join("stale.txt"), "old\n").unwrap();
    std::fs::write(local.path().join("new.txt"), "new\n").unwrap();
    ws.push().await.expect("push");
    assert!(remote_dir.join("new.txt").exists());
    assert!(
        !remote_dir.join("stale.txt").exists(),
        "delete_on_push = true では消える"
    );
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
    let out = std::process::Command::new(&helper)
        .arg("cat marker.txt")
        .output()
        .expect("run helper");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "cluster side");

    ws.push().await.expect("push");
    assert!(
        !remote_dir.join(".taskd").exists(),
        "ラッパはリモートへ送らない"
    );
}

#[tokio::test]
async fn missing_control_master_is_unreachable() {
    let local = tempfile::tempdir().unwrap();
    let mut s = settings(PathBuf::from("/nonexistent"));
    s.host = "celeris-no-such-host-for-tests".into();
    let ws = SshWorkspace::new(local.path(), s);
    assert!(!ws.control_master_alive().await, "多重接続は無い");
    let err = ws
        .exec("true", Duration::from_secs(10))
        .await
        .expect_err("unreachable");
    assert!(
        matches!(err, task_worker::WorkspaceError::Unreachable(_)),
        "{err:?}"
    );
}

/// ADR-0018 D4（監査の「確認不能」の解消）: `sync = "none"`（共有ファイルシステム）では rsync を一切呼ばない。
/// リモートのパスを手元のディレクトリと同じにして、同期なしでもコマンドの結果が見えることを確かめる。
#[tokio::test]
async fn sync_none_does_not_rsync_and_uses_the_same_directory() {
    let local = tempfile::tempdir().unwrap();
    let mut s = settings(local.path().to_path_buf());
    s.sync = SyncMode::None;
    // rsync が呼ばれたら失敗する（存在しないコマンド）。
    s.rsync_command = vec!["celeris-rsync-must-not-run".to_string()];
    let ws = SshWorkspace::new(local.path(), s);
    if !available(&ws).await {
        return;
    }
    let t = task(local.path());
    ws.prepare(&t).await.expect("prepare（pull を呼ばない）");

    // 共有 FS 前提なので、手元に置いたファイルがそのままリモートのコマンドから見える。
    std::fs::write(local.path().join("shared.txt"), "same filesystem\n").unwrap();
    let r = ws
        .exec("cat shared.txt", Duration::from_secs(30))
        .await
        .expect("exec");
    assert_eq!(
        (r.exit, r.stdout_tail.trim()),
        (Some(0), "same filesystem"),
        "{r:?}"
    );

    // リモートで作った成果物も、同じディレクトリなのでそのまま collect できる。
    let r = ws
        .exec(
            "mkdir -p artifacts && printf 'x\\n' > artifacts/out.txt",
            Duration::from_secs(30),
        )
        .await
        .expect("exec artifact");
    assert_eq!(r.exit, Some(0), "{r:?}");
    let artifacts = ws.collect(&t).await.expect("collect");
    assert_eq!(artifacts.len(), 1, "{artifacts:?}");
}

/// P-46: 同期の両方向で、celeris の管理用ディレクトリ（`runs/` など）はやり取りしない。
#[tokio::test]
async fn management_directories_are_never_synced() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let remote_dir = remote.path().join("project");
    let ws = SshWorkspace::new(local.path(), settings(remote_dir.clone()));
    if !available(&ws).await {
        return;
    }
    std::fs::create_dir_all(&remote_dir).unwrap();
    let t = task(local.path());
    ws.prepare(&t).await.expect("prepare");

    // 手元の run のログはクラスタへ送らない。
    std::fs::create_dir_all(local.path().join("runs/abc")).unwrap();
    std::fs::write(local.path().join("runs/abc/stdout.jsonl"), "{}\n").unwrap();
    ws.push().await.expect("push");
    assert!(
        !remote_dir.join("runs").exists(),
        "runs/ はクラスタへ送らない"
    );

    // pull（--delete 付き）でも手元の run のログは消えない。
    ws.pull().await.expect("pull");
    assert!(
        local.path().join("runs/abc/stdout.jsonl").exists(),
        "pull で手元の runs/ を消さない"
    );
}

/// ADR-0019: `sync = "worktree"` は、クラスタ側で git worktree を切り、その中だけを同期・実行する。
/// 未追跡の巨大データは worktree に入らない（= 手元に来ない）。
#[tokio::test]
async fn worktree_sync_only_brings_tracked_files() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let project = remote.path().join("project");

    // クラスタ側のリポジトリを作る: 追跡ファイル 1 つと、巨大データに見立てた未追跡ファイル。
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/main.rs"),
        "fn main() { println!(\"hi\") }\n",
    )
    .unwrap();
    std::fs::write(project.join("huge-data.bin"), vec![0u8; 1024 * 64]).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&project)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "celeris@example.com"]);
    git(&["config", "user.name", "celeris"]);
    git(&["add", "src/main.rs"]);
    git(&["commit", "-q", "-m", "initial"]);

    let mut s = settings(project.clone());
    s.sync = SyncMode::Worktree;
    s.task_id = "01TESTWORKTREE0000000000AA".to_string();
    let ws = SshWorkspace::new(local.path(), s);
    if !available(&ws).await {
        return;
    }
    let t = task(local.path());
    ws.prepare(&t)
        .await
        .expect("prepare（worktree を作って pull）");

    // 追跡ファイルだけが手元に来る。未追跡の巨大データは来ない。
    assert!(
        local.path().join("src/main.rs").exists(),
        "追跡ファイルは写しに来る"
    );
    assert!(
        !local.path().join("huge-data.bin").exists(),
        "未追跡のデータは持ち込まれない"
    );

    // worktree はブランチ celeris/<task_id> で、元のプロジェクトとは別ディレクトリ。
    let wt = project.join(".celeris-worktrees/01TESTWORKTREE0000000000AA");
    assert!(wt.join("src/main.rs").exists(), "worktree が切られている");
    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(&wt)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&branch.stdout).trim(),
        "celeris/01TESTWORKTREE0000000000AA"
    );

    // 手元の編集は worktree に push され、コマンドは worktree の中で走る。元のプロジェクトは変わらない。
    std::fs::write(
        local.path().join("src/main.rs"),
        "fn main() { println!(\"edited\") }\n",
    )
    .unwrap();
    let r = ws
        .exec("grep -c edited src/main.rs && pwd", Duration::from_secs(60))
        .await
        .expect("exec");
    assert_eq!(r.exit, Some(0), "{r:?}");
    assert!(
        r.stdout_tail.contains(".celeris-worktrees"),
        "worktree の中で実行される: {r:?}"
    );
    assert_eq!(
        std::fs::read_to_string(project.join("src/main.rs")).unwrap(),
        "fn main() { println!(\"hi\") }\n",
        "元のプロジェクトのファイルは変わらない（ADR-0019 D3）"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join("src/main.rs"))
            .unwrap()
            .trim(),
        "fn main() { println!(\"edited\") }"
    );
}

/// ADR-0019 D3: git 管理外のディレクトリに `sync = "worktree"` を指定したら、理由の分かるエラーにする。
#[tokio::test]
async fn worktree_sync_on_a_non_git_directory_explains_itself() {
    let local = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let plain = remote.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();

    let mut s = settings(plain);
    s.sync = SyncMode::Worktree;
    s.task_id = "01TESTNOTGIT000000000000AA".to_string();
    let ws = SshWorkspace::new(local.path(), s);
    if !available(&ws).await {
        return;
    }
    let err = ws.pull().await.expect_err("git リポジトリではない");
    let msg = err.to_string();
    assert!(msg.contains("not a git repository"), "{msg}");
    assert!(
        msg.contains("rsync"),
        "対処（sync = \"rsync\"）を示す: {msg}"
    );
}

/// 同じリポジトリに対して 2 つのタスクが同時に worktree を作っても、両方とも成立する
/// （クラスタの並列度が 1 より大きいときに起きる。git の worktree 管理は共有なので remote 側で flock する）。
#[tokio::test]
async fn two_tasks_can_create_worktrees_of_the_same_repository_at_once() {
    let remote = tempfile::tempdir().unwrap();
    let project = remote.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("f.txt"), "v1\n").unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "celeris@example.com"],
        vec!["config", "user.name", "celeris"],
        vec!["add", "f.txt"],
        vec!["commit", "-q", "-m", "initial"],
    ] {
        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(&project)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let make = |task_id: &str| {
        let local = tempfile::tempdir().unwrap();
        let mut s = settings(project.clone());
        s.sync = SyncMode::Worktree;
        s.task_id = task_id.to_string();
        let ws = SshWorkspace::new(local.path(), s);
        (local, ws)
    };
    let (local_a, a) = make("01TESTCONCURRENT00000000AA");
    let (local_b, b) = make("01TESTCONCURRENT00000000BB");
    if !available(&a).await {
        return;
    }
    let task_a = task(local_a.path());
    let task_b = task(local_b.path());
    let (ra, rb) = tokio::join!(a.prepare(&task_a), b.prepare(&task_b));
    ra.expect("task A の worktree");
    rb.expect("task B の worktree");

    assert!(local_a.path().join("f.txt").exists());
    assert!(local_b.path().join("f.txt").exists());
    let list = std::process::Command::new("git")
        .args(["worktree", "list"])
        .current_dir(&project)
        .output()
        .unwrap();
    let list = String::from_utf8_lossy(&list.stdout);
    assert!(list.contains("01TESTCONCURRENT00000000AA"), "{list}");
    assert!(list.contains("01TESTCONCURRENT00000000BB"), "{list}");
}
