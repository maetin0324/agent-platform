//! タスクの作業場所を**複数のリポジトリ**で組む（ADR-0043 D2）。
//!
//! ADR-0041 D1（Phase 49）はローカルの作業場所 1 つに対してタスクごとの `git worktree` を切った。
//! ADR-0043 D2 はそれを「案件が持つ複数のリポジトリ」へ広げる:
//!
//! ```text
//! <workspace_root>/<task_id>/
//!   repos/<name>/     # git: worktree（ブランチ <prefix><task_id>）。dir: 実体へのシンボリックリンク
//!   artifacts/ inputs/ runs/ worktree.json
//! ```
//!
//! - cwd は**タスクの最初のリポジトリ**（`repos[0]`）。
//! - `dir` のリポジトリは**シンボリックリンク**で見せる（コピーしない。大きいデータを想定）。
//! - 後片付けは ADR-0043 D2 の改定に従う: **終端では消さない**。消えるのは**中止**（cancel）のときだけで、
//!   そのとき worktree を消し、ブランチも `git branch -D` する。
//!
//! ここは `git` とファイルシステムを起こすだけで、判断は無い（LLM も無い。DESIGN 原則 1）。

use std::path::{Path, PathBuf};

use crate::local_worktree::{CleanupOutcome, LocalWorktree};
use crate::workspace::WorkspaceError;

/// タスクのディレクトリの下でリポジトリを並べる場所（ADR-0043 D2）。
pub const REPOS_DIR_NAME: &str = "repos";

/// タスクが使うリポジトリ 1 件の「タスクの中での姿」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRepo {
    /// 案件の中での名前（`project_repos.name`）。ディレクトリ名にもなる。
    pub name: String,
    /// 実体（案件のリポジトリの場所）。ここには書かない（worktree の場合）。
    pub source: PathBuf,
    /// タスクの中での場所（`<task_dir>/repos/<name>`。Phase 49 の 1 リポジトリだけのときは `<task_dir>/tree`）。
    pub dir: PathBuf,
    /// git のリポジトリのときだけ（worktree を切る）。`None` はシンボリックリンクで見せるもの
    /// （`kind = dir`、`mode = shared`、または「git と登録されているが実際は git ではない」）。
    pub worktree: Option<LocalWorktree>,
}

impl TaskRepo {
    /// git の worktree を切るリポジトリ。
    pub fn git(name: impl Into<String>, worktree: LocalWorktree) -> Self {
        Self {
            name: name.into(),
            source: worktree.repo.clone(),
            dir: worktree.dir.clone(),
            worktree: Some(worktree),
        }
    }

    /// シンボリックリンクで見せるリポジトリ（`kind = dir` など）。
    pub fn link(name: impl Into<String>, source: impl Into<PathBuf>, dir: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            dir: dir.into(),
            worktree: None,
        }
    }

    pub fn is_git(&self) -> bool {
        self.worktree.is_some()
    }

    /// 前置きと目印に出すブランチ名（git のときだけ）。
    pub fn branch(&self) -> Option<&str> {
        self.worktree.as_ref().map(|w| w.branch.as_str())
    }
}

/// 1 タスク分の作業場所（ADR-0043 D2）。ディスパッチャが dispatch のたびに組み立てる純粋なデータ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskWorkspaces {
    /// celeris が持つタスクのディレクトリ（`<workspace_root>/<task_id>`）。`runs/` `inputs/` `artifacts/` はここ。
    pub task_dir: PathBuf,
    /// 使うリポジトリ。**先頭がワーカーのカレントディレクトリ**になる。
    pub repos: Vec<TaskRepo>,
}

impl TaskWorkspaces {
    /// ワーカーのカレントディレクトリ（`repos[0]`）。リポジトリが 1 つも無ければ `None`。
    pub fn cwd(&self) -> Option<&Path> {
        self.repos.first().map(|r| r.dir.as_path())
    }

    /// 全リポジトリを用意する（冪等。既にあれば使い回す。ADR-0041 D1 の「再試行では作り直さない」）。
    pub async fn ensure(&self) -> Result<(), WorkspaceError> {
        tokio::fs::create_dir_all(&self.task_dir).await?;
        for repo in &self.repos {
            match &repo.worktree {
                Some(worktree) => worktree.ensure().await?,
                None => {
                    let (source, dir) = (repo.source.clone(), repo.dir.clone());
                    tokio::task::spawn_blocking(move || ensure_link(&source, &dir))
                        .await
                        .map_err(|e| WorkspaceError::Io(std::io::Error::other(format!("link task: {e}"))))??;
                }
            }
        }
        Ok(())
    }

    /// **中止（cancel）**の後片付け（ADR-0043 D2）: worktree を消し、ブランチも `git branch -D` する。
    /// シンボリックリンクは外す（リンク先の実体には触らない）。終端（`done` / `failed`）では**呼ばない**。
    pub fn remove_for_cancel(&self) -> Vec<(String, CleanupOutcome)> {
        self.repos
            .iter()
            .map(|repo| {
                let outcome = match &repo.worktree {
                    Some(worktree) => worktree.remove_with_branch(),
                    None => remove_link(&repo.dir),
                };
                (repo.name.clone(), outcome)
            })
            .collect()
    }
}

/// Phase 49 の 1 リポジトリだけの作業ツリーに付ける表示用の名前（ディレクトリ名の slug）。
/// 目印（`worktree.json`）の `repos[].name` とファイル閲覧 API の `?repo=` に使う。
pub fn repo_display_name(repo: &Path) -> String {
    let raw = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    task_core::repos::slugify_repo_name(&raw)
}

/// `setup` の記録を残すファイル（ADR-0043 D3: 「結果は `runs/setup.log`」）。
/// このファイルがあれば `setup` は済んでいるとみなす（worktree を作った直後に一度だけ）。
pub const SETUP_LOG: &str = "runs/setup.log";

/// `[commands] setup` を流した結果（ADR-0043 D3 / D4）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupOutcome {
    /// 全部のコマンドが exit 0 だったか。
    pub ok: bool,
    /// 実際に走ったコマンドの数（0 なら `setup` を書いたリポジトリが無かった）。
    pub ran: usize,
    /// 落ちたコマンドの一行説明（人への質問文に入れる）。
    pub failures: Vec<String>,
}

/// リポジトリごとの `[commands] setup` を一度だけ流す（ADR-0043 D3）。ホストで流す従来の入口。
pub async fn run_setup(
    repos: &[TaskRepo],
    task_dir: &Path,
    timeout: std::time::Duration,
) -> Result<SetupOutcome, WorkspaceError> {
    run_setup_in(repos, task_dir, timeout, None).await
}

/// リポジトリごとの `[commands] setup` を**そのタスクの実行環境で**一度だけ流す（ADR-0043 D3 / D4）。
///
/// - 走らせる場所はそのリポジトリの作業ツリー（`<task_dir>/repos/<name>`）
/// - `plan` が `Some` なら**コンテナの中**（Phase 56 = A3。`None` ならホスト）
/// - 記録は `<task_dir>/runs/setup.log`（追記。このファイルがあるかどうかは呼び出し側が見る）
/// - 1 つでも落ちたら `ok = false`（呼び出し側は run を始めずタスクを `blocked` にして人に聞く）
///
/// `workspace.toml` が読めない・壊れているリポジトリは既定（`setup` 無し）として飛ばす。
pub async fn run_setup_in(
    repos: &[TaskRepo],
    task_dir: &Path,
    timeout: std::time::Duration,
    plan: Option<&crate::container::SharedPlan>,
) -> Result<SetupOutcome, WorkspaceError> {
    use std::fmt::Write as _;

    let mut log = String::new();
    if let Some(plan) = plan {
        let _ = writeln!(log, "# 実行環境: コンテナ {} （{}）", plan.image, plan.runtime.as_str());
    }
    let mut out = SetupOutcome { ok: true, ran: 0, failures: Vec::new() };
    for repo in repos {
        let (config, warning) = task_core::workspace_config::load_or_default(&repo.dir);
        if let Some(warning) = warning {
            let _ = writeln!(log, "# {}: workspace.toml が読めないので既定にした: {warning}", repo.name);
            tracing::warn!(repo = %repo.name, %warning, "cannot read workspace.toml; using the defaults");
        }
        for cmd in &config.commands.setup {
            out.ran += 1;
            let _ = writeln!(log, "$ ({}) {cmd}", repo.name);
            let ws = crate::workspace::LocalWorkspace::new(task_dir)
                .with_work_dir(&repo.dir)
                .with_container(plan.map(std::sync::Arc::clone));
            let result = crate::workspace::Workspace::exec(&ws, cmd, timeout).await?;
            if !result.stdout_tail.is_empty() {
                let _ = writeln!(log, "{}", result.stdout_tail.trim_end());
            }
            if !result.stderr_tail.is_empty() {
                let _ = writeln!(log, "[stderr] {}", result.stderr_tail.trim_end());
            }
            let detail = if result.timed_out {
                Some(format!("`{cmd}`（{}）が {} 秒で終わらなかった", repo.name, timeout.as_secs()))
            } else if result.exit != Some(0) {
                Some(format!("`{cmd}`（{}）が exit {:?} で落ちた", repo.name, result.exit))
            } else {
                None
            };
            match detail {
                Some(detail) => {
                    let _ = writeln!(log, "=> 失敗: {detail}");
                    out.ok = false;
                    out.failures.push(detail);
                }
                None => {
                    let _ = writeln!(log, "=> exit 0");
                }
            }
        }
    }
    if out.ran > 0 || !log.is_empty() {
        let path = task_dir.join(SETUP_LOG);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, log.as_bytes()).await?;
    }
    Ok(out)
}

/// `dir` を `source` へのシンボリックリンクにする（既に同じ先を指していれば何もしない）。
/// `dir` に実体のディレクトリがあれば**触らない**（人が置いたものを消さない）。
fn ensure_link(source: &Path, dir: &Path) -> Result<(), WorkspaceError> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::read_link(dir) {
        Ok(current) if current == source => return Ok(()),
        // 別の場所を指しているリンクは張り替える（案件のリポジトリの場所が変わったとき）。
        Ok(_) => std::fs::remove_file(dir)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // シンボリックリンクではない実体がある。人が置いたものなので消さずにそのまま使う。
        Err(_) => return Ok(()),
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, dir)?;
    #[cfg(not(unix))]
    return Err(WorkspaceError::Io(std::io::Error::other("symlinks are only supported on unix")));
    #[cfg(unix)]
    Ok(())
}

/// シンボリックリンクだけを外す（実体には触らない）。
fn remove_link(dir: &Path) -> CleanupOutcome {
    match std::fs::symlink_metadata(dir) {
        Err(_) => CleanupOutcome::AlreadyGone,
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::remove_file(dir) {
            Ok(()) => CleanupOutcome::Removed,
            Err(_) => CleanupOutcome::Unknown,
        },
        // 実体のディレクトリ（人が置いた）は消さない。
        Ok(_) => CleanupOutcome::Dirty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_worktree::{DEFAULT_BRANCH_PREFIX, resolve_base};

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).expect("mkdir");
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("README.md"), b"hello\n").expect("write");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "first"]);
    }

    fn workspaces(task_dir: &Path, gits: &[(&str, &Path)], dirs: &[(&str, &Path)]) -> TaskWorkspaces {
        let id = "01TASK";
        let mut repos = Vec::new();
        for (name, repo) in gits {
            let dir = task_dir.join(REPOS_DIR_NAME).join(name);
            repos.push(TaskRepo::git(
                *name,
                LocalWorktree {
                    repo: repo.to_path_buf(),
                    task_dir: task_dir.to_path_buf(),
                    dir,
                    branch: format!("{DEFAULT_BRANCH_PREFIX}{id}"),
                    base: resolve_base(repo, None).expect("base"),
                },
            ));
        }
        for (name, target) in dirs {
            repos.push(TaskRepo::link(*name, *target, task_dir.join(REPOS_DIR_NAME).join(name)));
        }
        TaskWorkspaces { task_dir: task_dir.to_path_buf(), repos }
    }

    /// ADR-0043 D2: git 2 つ + `dir` 1 つ → worktree 2 つ + シンボリックリンク 1 つ。cwd は先頭。
    #[tokio::test]
    async fn two_git_repos_and_one_directory_become_two_worktrees_and_a_symlink() {
        let root = tempfile::tempdir().expect("tempdir");
        let code = root.path().join("benchfs");
        let paper = root.path().join("benchfs-paper");
        init_repo(&code);
        init_repo(&paper);
        let data = root.path().join("data");
        std::fs::create_dir_all(data.join("runs")).expect("mkdir");
        std::fs::write(data.join("runs/one.csv"), b"1\n").expect("write");

        let task_dir = root.path().join("ws").join("01TASK");
        let ws = workspaces(&task_dir, &[("benchfs", &code), ("benchfs-paper", &paper)], &[("data", &data)]);
        ws.ensure().await.expect("ensure");

        assert_eq!(ws.cwd(), Some(task_dir.join("repos/benchfs").as_path()), "cwd は先頭のリポジトリ");
        assert!(task_dir.join("repos/benchfs/README.md").is_file());
        assert!(task_dir.join("repos/benchfs-paper/README.md").is_file());
        // `dir` はシンボリックリンク（コピーしない）。
        let link = task_dir.join("repos/data");
        assert!(std::fs::symlink_metadata(&link).expect("meta").file_type().is_symlink());
        assert_eq!(std::fs::read_link(&link).expect("readlink"), data);
        assert!(link.join("runs/one.csv").is_file(), "リンク越しに読める");

        // 冪等: もう一度 ensure しても作業は消えない。
        std::fs::write(task_dir.join("repos/benchfs/wip.txt"), b"x").expect("write");
        ws.ensure().await.expect("again");
        assert!(task_dir.join("repos/benchfs/wip.txt").is_file());
    }

    /// ADR-0043 D2: 中止したタスクは worktree とブランチを消し、リンクを外す（実体は残る）。
    #[tokio::test]
    async fn cancel_removes_every_worktree_its_branch_and_the_symlink() {
        let root = tempfile::tempdir().expect("tempdir");
        let code = root.path().join("benchfs");
        init_repo(&code);
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).expect("mkdir");
        std::fs::write(data.join("keep.txt"), b"keep").expect("write");

        let task_dir = root.path().join("ws").join("01TASK");
        let ws = workspaces(&task_dir, &[("benchfs", &code)], &[("data", &data)]);
        ws.ensure().await.expect("ensure");
        // 未コミットの変更があっても cancel は消す（人の指示なので）。
        std::fs::write(task_dir.join("repos/benchfs/dirty.txt"), b"x").expect("write");

        let outcomes = ws.remove_for_cancel();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|(_, o)| *o == CleanupOutcome::Removed), "{outcomes:?}");
        assert!(!task_dir.join("repos/benchfs").exists());
        assert!(!task_dir.join("repos/data").exists());
        assert!(data.join("keep.txt").is_file(), "リンク先の実体は消さない");
        let branches = std::process::Command::new("git")
            .arg("-C")
            .arg(&code)
            .args(["branch", "--list"])
            .output()
            .expect("git");
        assert!(
            !String::from_utf8_lossy(&branches.stdout).contains(DEFAULT_BRANCH_PREFIX),
            "ブランチも消える: {}",
            String::from_utf8_lossy(&branches.stdout)
        );
        // 2 回目は「もう無い」。
        assert!(ws.remove_for_cancel().iter().all(|(_, o)| *o == CleanupOutcome::AlreadyGone));
    }

    /// ADR-0043 D3 / D4: `[commands] setup` は worktree の中で走り、`runs/setup.log` に残る。
    #[tokio::test]
    async fn setup_commands_run_in_the_repo_and_are_logged() {
        let root = tempfile::tempdir().expect("tempdir");
        let code = root.path().join("benchfs");
        init_repo(&code);
        std::fs::create_dir_all(code.join(".config/celeris")).expect("mkdir");
        std::fs::write(
            code.join(".config/celeris/workspace.toml"),
            b"[commands]\nsetup = [\"pwd > setup-ran.txt\"]\n",
        )
        .expect("write");
        git(&code, &["add", "-A"]);
        git(&code, &["commit", "-q", "-m", "workspace.toml"]);

        let task_dir = root.path().join("ws").join("01TASK");
        let ws = workspaces(&task_dir, &[("benchfs", &code)], &[]);
        ws.ensure().await.expect("ensure");
        let outcome = run_setup(&ws.repos, &task_dir, std::time::Duration::from_secs(30))
            .await
            .expect("setup");
        assert_eq!(outcome, SetupOutcome { ok: true, ran: 1, failures: Vec::new() });
        let ran = task_dir.join("repos/benchfs/setup-ran.txt");
        assert!(ran.is_file(), "setup は worktree の中で走る");
        let log = std::fs::read_to_string(task_dir.join(SETUP_LOG)).expect("log");
        assert!(log.contains("$ (benchfs) pwd > setup-ran.txt"), "{log}");
        assert!(log.contains("=> exit 0"), "{log}");
    }

    /// 落ちた `setup` は `ok = false` と理由を返す（呼び出し側が run を始めずに人へ聞く）。
    #[tokio::test]
    async fn a_failing_setup_reports_the_reason() {
        let root = tempfile::tempdir().expect("tempdir");
        let code = root.path().join("benchfs");
        init_repo(&code);
        std::fs::create_dir_all(code.join(".config/celeris")).expect("mkdir");
        std::fs::write(
            code.join(".config/celeris/workspace.toml"),
            b"[commands]\nsetup = [\"echo boom 1>&2; exit 7\"]\n",
        )
        .expect("write");
        git(&code, &["add", "-A"]);
        git(&code, &["commit", "-q", "-m", "workspace.toml"]);

        let task_dir = root.path().join("ws").join("01TASK");
        let ws = workspaces(&task_dir, &[("benchfs", &code)], &[]);
        ws.ensure().await.expect("ensure");
        let outcome = run_setup(&ws.repos, &task_dir, std::time::Duration::from_secs(30))
            .await
            .expect("setup");
        assert!(!outcome.ok);
        assert_eq!(outcome.ran, 1);
        assert_eq!(outcome.failures.len(), 1);
        assert!(outcome.failures[0].contains("exit Some(7)"), "{:?}", outcome.failures);
        let log = std::fs::read_to_string(task_dir.join(SETUP_LOG)).expect("log");
        assert!(log.contains("[stderr] boom"), "{log}");
    }

    /// ADR-0043 D3（Phase 56）: コンテナのタスクでは `setup` も**コンテナの中で**走る。
    ///
    /// 偽の runtime（argv を記録して、`-w` の場所で本体のコマンドをそのまま実行する sh スクリプト）を
    /// 使う。**ネットワークにも本物の podman / docker にも触らない**。
    #[tokio::test]
    async fn setup_runs_inside_the_container_when_the_task_is_containerized() {
        let root = tempfile::tempdir().expect("tempdir");
        let code = root.path().join("benchfs");
        init_repo(&code);
        std::fs::create_dir_all(code.join(".config/celeris")).expect("mkdir");
        std::fs::write(
            code.join(".config/celeris/workspace.toml"),
            b"[run]\nmode = \"container\"\n\n[commands]\nsetup = [\"pwd > setup-ran.txt\"]\n",
        )
        .expect("write");
        git(&code, &["add", "-A"]);
        git(&code, &["commit", "-q", "-m", "workspace.toml"]);

        // 偽の runtime: argv を NUL 区切りで記録し、`-w` のディレクトリでイメージ以降を実行する。
        let argv_log = root.path().join("argv.log");
        let fake = root.path().join("fake-runtime");
        crate::test_support::write_executable(
            &fake,
            &format!(
                r#"#!/bin/sh
for a in "$@"; do printf '%s\0' "$a" >> "{log}"; done
cwd=""
while [ $# -gt 0 ]; do
  case "$1" in
    -w) cwd="$2"; shift 2 ;;
    -v|--env|--label|--user) shift 2 ;;
    run|--rm|-i|--network|host|--userns=keep-id) shift ;;
    *) break ;;
  esac
done
shift
cd "$cwd" || exit 1
exec "$@"
"#,
                log = argv_log.display()
            ),
        );

        let task_dir = root.path().join("ws").join("01TASK");
        let ws = workspaces(&task_dir, &[("benchfs", &code)], &[]);
        ws.ensure().await.expect("ensure");

        let plan: crate::container::SharedPlan = std::sync::Arc::new(crate::container::ContainerPlan {
            runtime: crate::container::Runtime::Podman,
            program: fake.display().to_string(),
            image: "celeris-worker:latest".to_string(),
            task_dir: task_dir.clone(),
            dir_repos: vec![],
            creds: vec![],
            extra_mounts: vec![],
            env: vec![],
            task_id: "01TASK".to_string(),
            uid: 1000,
            gid: 1000,
        });
        let outcome = run_setup_in(&ws.repos, &task_dir, std::time::Duration::from_secs(60), Some(&plan))
            .await
            .expect("setup");
        assert_eq!(outcome, SetupOutcome { ok: true, ran: 1, failures: Vec::new() });

        // コマンドは runtime 越しに渡っている（ホストで直接 `sh -c` していない）。
        let argv = std::fs::read(&argv_log).expect("argv.log");
        let argv: Vec<String> = String::from_utf8_lossy(&argv)
            .split('\0')
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect();
        let line = argv.join(" ");
        assert!(line.starts_with("run --rm -i --network host --userns=keep-id"), "{line}");
        assert!(line.contains(&format!("-w {}", task_dir.join("repos/benchfs").display())), "{line}");
        assert!(line.contains(&format!("-v {0}:{0}", task_dir.display())), "{line}");
        assert!(line.contains("--label celeris.task=01TASK"), "{line}");
        assert!(line.contains("celeris-worker:latest sh -c pwd > setup-ran.txt"), "{line}");

        // 中身も本当に走っている（worktree の中に結果が残る）。
        assert!(task_dir.join("repos/benchfs/setup-ran.txt").is_file());
        let log = std::fs::read_to_string(task_dir.join(SETUP_LOG)).expect("log");
        assert!(log.contains("# 実行環境: コンテナ celeris-worker:latest （podman）"), "{log}");
        assert!(log.contains("=> exit 0"), "{log}");
    }

    /// 人が実体のディレクトリを置いていたら、リンクで上書きしない。
    #[tokio::test]
    async fn an_existing_real_directory_is_left_alone() {
        let root = tempfile::tempdir().expect("tempdir");
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).expect("mkdir");
        let task_dir = root.path().join("ws").join("01TASK");
        let real = task_dir.join(REPOS_DIR_NAME).join("data");
        std::fs::create_dir_all(&real).expect("mkdir");
        std::fs::write(real.join("mine.txt"), b"mine").expect("write");

        let ws = workspaces(&task_dir, &[], &[("data", &data)]);
        ws.ensure().await.expect("ensure");
        assert!(real.join("mine.txt").is_file());
        assert!(!std::fs::symlink_metadata(&real).expect("meta").file_type().is_symlink());
        assert_eq!(ws.remove_for_cancel(), vec![("data".to_string(), CleanupOutcome::Dirty)]);
        assert!(real.join("mine.txt").is_file());
    }
}
