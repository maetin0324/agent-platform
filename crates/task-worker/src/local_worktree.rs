//! ローカルの作業場所をタスクごとの `git worktree` にする（ADR-0041 D1）。
//!
//! ADR-0019 がリモート（`sync = "worktree"`）に対して決めたことを、ローカルにも同じ形で持ち込む。
//! 実機の問題: 案件の作業場所 `local: ~/workspace/agent-platform` を全タスクが同じ作業ツリーで共有すると、
//! 並列の実装者が別のブランチを `checkout` して互いの未コミット変更を壊す（人も同じチェックアウトで作業している）。
//!
//! ここがやること（`git` を起こすだけ。LLM も判断も無い）:
//!
//! - `path` が git リポジトリか（`git -C <path> rev-parse --git-dir`）
//! - base を決める（`main` / 本番の `current` / `HEAD`。ADR-0041 D1 の決定的な規則）
//! - `git -C <path> worktree add -b <branch_prefix><task_id> <dir> <base>`（再試行では作り直さず使い回す）
//! - 終端で `git status --porcelain` が空なら `git -C <path> worktree remove <dir>`
//!
//! **taskd はコミットしない**（ADR-0019 D2）。ブランチも消さない。元のリポジトリの作業ツリーには触らない
//! （ADR-0019 D3）。

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::workspace::WorkspaceError;

/// worktree の親（`<workspace_root>/<task_id>`）の下に切る作業ツリーの名前。
pub const WORKTREE_DIR_NAME: &str = "tree";

/// ローカルの worktree のブランチ接頭辞の既定（ADR-0042 D3 でこの基盤の名前 Celeris に合わせた。
/// Phase 49 までは ADR-0019 のクラスタ側と同じ `taskd/` だった）。`[workspace] worktree_branch_prefix` で変えられる。
/// **クラスタ側**（ADR-0019 の `WorktreeSettings::branch_prefix`）は `taskd/` のまま（既存のクラスタに
/// 残っているブランチの名前を変えないため）。
pub const DEFAULT_BRANCH_PREFIX: &str = "celeris/";

/// `git worktree add` の直列化に使う錠（`workspace_root` 直下。元のリポジトリには置かない）。
const LOCK_FILE: &str = ".taskd-worktree.lock";

/// base をどこから取ったか（前置きに書く。ADR-0041 D1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseKind {
    /// リポジトリの `main`。
    Main,
    /// 本番の `current` リリースの sha（`main` がまだ本番に追いついていない）。
    Current,
    /// `main` が無いリポジトリの `HEAD`。
    Head,
}

impl BaseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BaseKind::Main => "main",
            BaseKind::Current => "current",
            BaseKind::Head => "head",
        }
    }
}

/// 切り出す base（全長の sha と、その出どころ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseRef {
    pub kind: BaseKind,
    pub sha: String,
}

impl BaseRef {
    /// 前置きに出す短縮 sha（12 桁）。
    pub fn sha12(&self) -> String {
        self.sha.chars().take(12).collect()
    }
}

/// 1 タスク分の worktree（ディスパッチャが dispatch のたびに組み立てる。純粋なデータ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorktree {
    /// 元のリポジトリ（`WorkspaceSpec::Local.path`）。ここには書かない。
    pub repo: PathBuf,
    /// taskd が持つタスクのディレクトリ（`<workspace_root>/<task_id>`）。`runs/` `inputs/` `artifacts/` はここ。
    pub task_dir: PathBuf,
    /// 作業ツリー（`<task_dir>/tree`）。ワーカーのカレントディレクトリになる。
    pub dir: PathBuf,
    /// ブランチ（`<branch_prefix><task_id>`）。taskd は作るだけで、消さない。
    pub branch: String,
    /// 切り出した base。
    pub base: BaseRef,
}

impl LocalWorktree {
    /// `git worktree add`（既にあれば使い回す。ADR-0041 D1 の「再試行では作り直さない」）。
    ///
    /// - 作業ツリーがある（`<dir>/.git` がある）→ 何もしない
    /// - ブランチだけある（前の run で作って worktree を消した）→ `worktree add <dir> <branch>`
    /// - どちらも無い → `worktree add -b <branch> <dir> <base>`
    pub async fn ensure(&self) -> Result<(), WorkspaceError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.ensure_blocking())
            .await
            .map_err(|e| WorkspaceError::Io(std::io::Error::other(format!("worktree task: {e}"))))?
    }

    fn ensure_blocking(&self) -> Result<(), WorkspaceError> {
        std::fs::create_dir_all(&self.task_dir)?;
        if self.dir.join(".git").exists() {
            return Ok(());
        }
        // 同じリポジトリに複数のタスクが同時に worktree を作ることがある（`max_concurrency > 1`）。
        // git の worktree 管理はリポジトリで共有なので、taskd 側の錠で直列化する。
        let _lock = WorktreeLock::acquire(self.task_dir.parent().unwrap_or(&self.task_dir));
        // 手で消された作業ツリーの登録が残っていると `worktree add` が「既に登録済み」で失敗する。
        let _ = git(&self.repo, &["worktree", "prune"]);
        if self.dir.join(".git").exists() {
            return Ok(());
        }
        let dir = self.dir.to_string_lossy().into_owned();
        let out = if branch_exists(&self.repo, &self.branch) {
            git(&self.repo, &["worktree", "add", &dir, &self.branch])
        } else {
            git(&self.repo, &["worktree", "add", "-b", &self.branch, &dir, &self.base.sha])
        };
        match out {
            Some(o) if o.ok => Ok(()),
            Some(o) => Err(WorkspaceError::Remote(format!(
                "cannot create the git worktree {} of {} (exit {:?}): {}",
                dir,
                self.repo.display(),
                o.code,
                o.stderr.trim()
            ))),
            None => Err(WorkspaceError::Remote(format!(
                "cannot run git for the worktree of {}",
                self.repo.display()
            ))),
        }
    }

    /// 終端での後片付け（ADR-0041 D1）。`git status --porcelain` が空なら worktree を消す
    /// （**ブランチは消さない**。コミットはリポジトリに残る）。空でなければ残して `Err(dir)` を返す
    /// （呼び出し側が `WorkerProgress` を 1 行積む）。
    pub fn remove_if_clean(&self) -> CleanupOutcome {
        if !self.dir.exists() {
            return CleanupOutcome::AlreadyGone;
        }
        match status_is_clean(&self.dir) {
            None => CleanupOutcome::Unknown,
            Some(false) => CleanupOutcome::Dirty,
            Some(true) => {
                let dir = self.dir.to_string_lossy().into_owned();
                match git(&self.repo, &["worktree", "remove", &dir]) {
                    Some(o) if o.ok => CleanupOutcome::Removed,
                    _ => CleanupOutcome::Unknown,
                }
            }
        }
    }
    /// ADR-0043 D2 の**中止（cancel）**の後片付け: 未コミットの変更があっても worktree を消し、
    /// **ブランチも消す**（`git branch -D`）。人が「このタスクは中止」と決めたときだけ呼ぶ。
    /// 終端（`done` / `failed`）では呼ばない（差分を見るために残す）。
    pub fn remove_with_branch(&self) -> CleanupOutcome {
        let existed = self.dir.exists();
        let dir = self.dir.to_string_lossy().into_owned();
        if existed {
            // `--force` は未コミットの変更ごと消す（cancel は人の指示）。
            let removed = git(&self.repo, &["worktree", "remove", "--force", &dir]).is_some_and(|o| o.ok);
            if !removed {
                // 登録が壊れている（人が手で消した等）なら prune してからディレクトリを落とす。
                let _ = git(&self.repo, &["worktree", "prune"]);
                if self.dir.exists() && std::fs::remove_dir_all(&self.dir).is_err() {
                    return CleanupOutcome::Unknown;
                }
                let _ = git(&self.repo, &["worktree", "prune"]);
            }
        }
        // ブランチは worktree が無くなってからでないと消せない。
        let had_branch = branch_exists(&self.repo, &self.branch);
        if had_branch && !git(&self.repo, &["branch", "-D", &self.branch]).is_some_and(|o| o.ok) {
            return CleanupOutcome::Unknown;
        }
        if existed || had_branch {
            CleanupOutcome::Removed
        } else {
            CleanupOutcome::AlreadyGone
        }
    }
}

/// `remove_if_clean` の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// 消した（クリーンだった）。
    Removed,
    /// 未コミットの変更が残っているので残した。
    Dirty,
    /// もう無い（人が消した・作られなかった）。
    AlreadyGone,
    /// git が動かせなかった。残す。
    Unknown,
}

/// `path` が git リポジトリか（ADR-0019 D3 と同じ判定）。
pub fn is_git_repo(path: &Path) -> bool {
    path.is_dir() && git(path, &["rev-parse", "--git-dir"]).is_some_and(|o| o.ok)
}

/// ADR-0041 D1 の base の規則（決定的）:
///
/// 1. `main` があればその sha。無ければ `HEAD`。
/// 2. ただし本番の `current` リリースの sha が分かり、それが `main` の**子孫**（`main` が本番に
///    追いついていない）なら、`current` の sha。本番より古いコードから分岐させない。
///
/// commit が 1 つも無いリポジトリでは `None`（worktree を切れない → 従来どおりの共有に倒す）。
pub fn resolve_base(repo: &Path, current_sha: Option<&str>) -> Option<BaseRef> {
    let main = rev_parse(repo, "refs/heads/main");
    let Some(main_sha) = main else {
        let head = rev_parse(repo, "HEAD")?;
        return Some(BaseRef { kind: BaseKind::Head, sha: head });
    };
    if let Some(current) = current_sha
        && let Some(current_full) = rev_parse(repo, &format!("{current}^{{commit}}"))
        && current_full != main_sha
        && git(repo, &["merge-base", "--is-ancestor", &main_sha, &current_full]).is_some_and(|o| o.ok)
    {
        return Some(BaseRef { kind: BaseKind::Current, sha: current_full });
    }
    Some(BaseRef { kind: BaseKind::Main, sha: main_sha })
}

/// 本番の `current` リリースの sha（ADR-0040 D6: `current` は `releases_dir` の**親**にある）。
/// 読めない・壊れている・`sha` が無いときは `None`（base は `main` に倒れる）。
pub fn current_release_sha(releases_dir: &Path) -> Option<String> {
    let manifest = releases_dir.parent()?.join("current").join("manifest.json");
    let text = std::fs::read_to_string(manifest).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let sha = value
        .get("sha")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("sha12").and_then(|v| v.as_str()))?;
    let sha = sha.trim();
    if sha.is_empty() || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(sha.to_string())
}

/// `git -C <dir> status --porcelain` が空か。git が動かせなければ `None`。
pub fn status_is_clean(dir: &Path) -> Option<bool> {
    let out = git(dir, &["status", "--porcelain"])?;
    if !out.ok {
        return None;
    }
    Some(out.stdout.trim().is_empty())
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_some_and(|o| o.ok)
}

fn rev_parse(repo: &Path, rev: &str) -> Option<String> {
    let out = git(repo, &["rev-parse", "--verify", "--quiet", rev])?;
    if !out.ok {
        return None;
    }
    let sha = out.stdout.trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

struct GitOutput {
    ok: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// `git -C <dir> <args...>`。git が無い・起動できないときは `None`。
fn git(dir: &Path, args: &[&str]) -> Option<GitOutput> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        // 人の設定（`core.hooksPath`、対話的な認証）に引きずられないようにする。
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .output()
        .ok()?;
    Some(GitOutput {
        ok: out.status.success(),
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// `workspace_root/.taskd-worktree.lock` の flock（取れなければ錠なしで進む。止めない）。
struct WorktreeLock(#[allow(dead_code)] Option<nix::fcntl::Flock<std::fs::File>>);

impl WorktreeLock {
    fn acquire(root: &Path) -> WorktreeLock {
        let _ = std::fs::create_dir_all(root);
        let file = match std::fs::OpenOptions::new().create(true).append(true).open(root.join(LOCK_FILE)) {
            Ok(f) => f,
            Err(_) => return WorktreeLock(None),
        };
        match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive) {
            Ok(lock) => WorktreeLock(Some(lock)),
            Err(_) => WorktreeLock(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).expect("mkdir");
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
        ] {
            let out = git(dir, &args).expect("git");
            assert!(out.ok, "git {args:?}: {}", out.stderr);
        }
        std::fs::write(dir.join("README.md"), b"hello\n").expect("write");
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", "first"]] {
            let out = git(dir, &args).expect("git");
            assert!(out.ok, "git {args:?}: {}", out.stderr);
        }
    }

    pub(crate) fn commit(dir: &Path, name: &str) -> String {
        std::fs::write(dir.join(name), name.as_bytes()).expect("write");
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", name]] {
            let out = git(dir, &args).expect("git");
            assert!(out.ok, "git {args:?}: {}", out.stderr);
        }
        rev_parse(dir, "HEAD").expect("head")
    }

    #[test]
    fn a_plain_directory_is_not_a_git_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!is_git_repo(dir.path()));
        init_repo(dir.path());
        assert!(is_git_repo(dir.path()));
    }

    /// base の既定は `main`（ADR-0041 D1）。
    #[test]
    fn the_base_is_main_when_there_is_no_current_release() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        let base = resolve_base(dir.path(), None).expect("base");
        assert_eq!(base.kind, BaseKind::Main);
        assert_eq!(base.sha, rev_parse(dir.path(), "refs/heads/main").expect("main"));
    }

    /// `main` が無いリポジトリでは `HEAD`。
    #[test]
    fn the_base_is_head_when_there_is_no_main() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        let out = git(dir.path(), &["checkout", "-q", "-b", "trunk"]).expect("git");
        assert!(out.ok, "{}", out.stderr);
        let out = git(dir.path(), &["branch", "-q", "-D", "main"]).expect("git");
        assert!(out.ok, "{}", out.stderr);
        let base = resolve_base(dir.path(), None).expect("base");
        assert_eq!(base.kind, BaseKind::Head);
    }

    /// 本番の `current` が `main` の子孫なら、そちらを base にする（本番より古いコードから分岐させない）。
    #[test]
    fn the_base_is_the_current_release_when_it_is_a_descendant_of_main() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        let main_sha = rev_parse(dir.path(), "refs/heads/main").expect("main");
        let out = git(dir.path(), &["checkout", "-q", "-b", "ahead"]).expect("git");
        assert!(out.ok, "{}", out.stderr);
        let ahead = commit(dir.path(), "ahead.txt");
        let base = resolve_base(dir.path(), Some(&ahead)).expect("base");
        assert_eq!(base.kind, BaseKind::Current);
        assert_eq!(base.sha, ahead);
        assert_ne!(base.sha, main_sha);
    }

    /// `current` が `main` の祖先（本番が古い）なら `main` のまま。
    #[test]
    fn the_base_stays_main_when_the_current_release_is_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        let first = rev_parse(dir.path(), "HEAD").expect("head");
        let second = commit(dir.path(), "second.txt");
        let base = resolve_base(dir.path(), Some(&first)).expect("base");
        assert_eq!(base.kind, BaseKind::Main);
        assert_eq!(base.sha, second);
    }

    fn worktree_for(repo: &Path, root: &Path, id: &str) -> LocalWorktree {
        let task_dir = root.join(id);
        LocalWorktree {
            dir: task_dir.join(WORKTREE_DIR_NAME),
            task_dir,
            repo: repo.to_path_buf(),
            branch: format!("{DEFAULT_BRANCH_PREFIX}{id}"),
            base: resolve_base(repo, None).expect("base"),
        }
    }

    /// ADR-0041 D1: やり直しの run は worktree を**作り直さない**（未コミットの作業を消さない）。
    #[tokio::test]
    async fn a_retry_reuses_the_existing_worktree() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        let root = tempfile::tempdir().expect("tempdir");
        let wt = worktree_for(repo.path(), root.path(), "01TASK");
        wt.ensure().await.expect("first");
        std::fs::write(wt.dir.join("work-in-progress"), b"x").expect("write");
        wt.ensure().await.expect("retry");
        assert!(wt.dir.join("work-in-progress").is_file(), "やり直しで作業を消さない");
        assert_eq!(git(repo.path(), &["worktree", "list"]).expect("git").stdout.lines().count(), 2);
    }

    /// ブランチだけ残っている（前の run の後で worktree を消した）ときは、そのブランチで作り直す。
    #[tokio::test]
    async fn a_removed_worktree_is_recreated_on_the_same_branch() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        let root = tempfile::tempdir().expect("tempdir");
        let wt = worktree_for(repo.path(), root.path(), "01TASK");
        wt.ensure().await.expect("first");
        let committed = commit(&wt.dir, "done.txt");
        assert_eq!(wt.remove_if_clean(), CleanupOutcome::Removed);
        assert!(!wt.dir.exists());
        wt.ensure().await.expect("again");
        // ブランチの先端（前の run のコミット）から再開する。base には戻らない。
        assert_eq!(rev_parse(&wt.dir, "HEAD").expect("head"), committed);
        assert!(wt.dir.join("done.txt").is_file());
    }

    /// 終端の後片付け: クリーンなら消す、汚れていれば残す。ブランチは消さない。
    #[tokio::test]
    async fn cleanup_removes_a_clean_worktree_and_keeps_a_dirty_one() {
        let repo = tempfile::tempdir().expect("tempdir");
        init_repo(repo.path());
        let root = tempfile::tempdir().expect("tempdir");
        let dirty = worktree_for(repo.path(), root.path(), "01DIRTY");
        dirty.ensure().await.expect("ensure");
        std::fs::write(dirty.dir.join("untracked"), b"x").expect("write");
        assert_eq!(dirty.remove_if_clean(), CleanupOutcome::Dirty);
        assert!(dirty.dir.join("untracked").is_file());

        let clean = worktree_for(repo.path(), root.path(), "01CLEAN");
        clean.ensure().await.expect("ensure");
        assert_eq!(clean.remove_if_clean(), CleanupOutcome::Removed);
        assert!(!clean.dir.exists());
        assert!(branch_exists(repo.path(), &clean.branch), "ブランチは消さない");
        assert_eq!(clean.remove_if_clean(), CleanupOutcome::AlreadyGone);
    }

    /// `current/manifest.json` の `sha` を読む（`releases_dir` の**親**にある。ADR-0040 D6）。
    #[test]
    fn the_current_release_sha_comes_from_the_manifest_next_to_releases() {
        let home = tempfile::tempdir().expect("tempdir");
        let releases = home.path().join("releases");
        std::fs::create_dir_all(&releases).expect("mkdir");
        assert_eq!(current_release_sha(&releases), None);
        let current = home.path().join("current");
        std::fs::create_dir_all(&current).expect("mkdir");
        std::fs::write(current.join("manifest.json"), br#"{"sha":"abc123def456789","sha12":"abc123def456"}"#)
            .expect("write");
        assert_eq!(current_release_sha(&releases).as_deref(), Some("abc123def456789"));
        std::fs::write(current.join("manifest.json"), b"not json").expect("write");
        assert_eq!(current_release_sha(&releases), None);
    }
}
