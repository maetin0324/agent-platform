//! ADR-0074 D1.2 / D1.4（Phase F2）: WU ごとの worktree と工程末尾の統合（daemon 側の git 操作だけ）。
//!
//! ここは `git` を起こすだけで、判断（どの WU をいつ統合するか、衝突を誰に直させるか）は
//! `dispatcher.rs` と `execution_scheduler.rs` にある。**LLM は呼ばない**（DESIGN 原則 1）。
//!
//! - WU の worktree: `<task_dir>/wu/<key>/repos/<name>/`、ブランチ `celeris-wu/<task_id>/<key>`。
//!   基点は Task ブランチの HEAD（工程の最初の WU）か、同じ工程の依存先の WU ブランチの HEAD（積み上げ）。
//! - WU の完了時の commit: `git add -A && git commit -m "wu/<key>: <title>"`（作者は celeris の固定値。
//!   変更が無ければ commit しない）。
//! - 統合: Task の worktree で、工程の葉の WU のブランチを `seq` 順に `git merge --no-ff --no-edit`。
//!   既に入っている（`merge-base --is-ancestor`）ものは飛ばし、`MERGE_HEAD` が残っていれば
//!   `merge --abort` してからやり直す（再起動後のやり直しを冪等にする）。衝突したら `merge --abort`
//!   して止める（呼び出し側が repair WU を作る）。

use std::path::{Path, PathBuf};
use std::process::Command;

use task_worker::local_worktree::{BaseKind, BaseRef, LocalWorktree};

/// WU の worktree を置くディレクトリ名（`<task_dir>/wu/<key>`）。
pub const WU_DIR_NAME: &str = "wu";
/// 統合・WU の commit の作者（決定的な固定値）。
pub const CELERIS_GIT_NAME: &str = "celeris";
pub const CELERIS_GIT_EMAIL: &str = "celeris@localhost";

/// `celeris-wu/<task_id>/<key>`（ADR-0074 D1.2。`celeris/<task_id>/…` にしないのは ref の D/F 衝突のため）。
pub fn wu_branch(task_id: &str, key: &str) -> String {
    format!("celeris-wu/{task_id}/{key}")
}

/// `<task_dir>/wu/<key>`（その WU の `artifacts/` と `repos/` の親）。
pub fn wu_dir(task_dir: &Path, key: &str) -> PathBuf {
    task_dir.join(WU_DIR_NAME).join(key)
}

/// 統合の merge のメッセージ（固定）。
pub fn merge_message(key: &str, phase: &str) -> String {
    format!("integrate wu/{key} (phase {phase})")
}

/// WU の完了時の commit のメッセージ。
pub fn commit_message(key: &str, title: &str) -> String {
    format!("wu/{key}: {title}")
}

/// `repo`（元のリポジトリ）の上に WU の worktree を表す値を作る（純粋なデータ。作るのは `ensure`）。
/// `task_dir` は Task のディレクトリ（worktree の錠は `task_dir` の親 = workspace_root に置かれる）。
pub fn wu_worktree(
    task_dir: &Path,
    task_id: &str,
    key: &str,
    repo_name: &str,
    repo_source: &Path,
    base_sha: &str,
) -> LocalWorktree {
    LocalWorktree {
        repo: repo_source.to_path_buf(),
        task_dir: task_dir.to_path_buf(),
        dir: wu_dir(task_dir, key)
            .join(task_worker::task_repos::REPOS_DIR_NAME)
            .join(repo_name),
        branch: wu_branch(task_id, key),
        base: BaseRef {
            kind: BaseKind::Head,
            sha: base_sha.to_string(),
        },
    }
}

/// `git -C <dir> <args>` の結果。
#[derive(Debug, Clone)]
struct GitOut {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn git(dir: &Path, args: &[&str]) -> Result<GitOut, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            &format!("user.name={CELERIS_GIT_NAME}"),
            "-c",
            &format!("user.email={CELERIS_GIT_EMAIL}"),
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", CELERIS_GIT_NAME)
        .env("GIT_AUTHOR_EMAIL", CELERIS_GIT_EMAIL)
        .env("GIT_COMMITTER_NAME", CELERIS_GIT_NAME)
        .env("GIT_COMMITTER_EMAIL", CELERIS_GIT_EMAIL)
        .output()
        .map_err(|e| format!("cannot run git in {}: {e}", dir.display()))?;
    Ok(GitOut {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<GitOut, String> {
    let out = git(dir, args)?;
    if out.ok {
        Ok(out)
    } else {
        Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            out.stderr.trim()
        ))
    }
}

/// `rev` の全長 sha（無ければ `None`）。
pub fn rev_parse(dir: &Path, rev: &str) -> Option<String> {
    let out = git(dir, &["rev-parse", "--verify", "--quiet", rev]).ok()?;
    if !out.ok {
        return None;
    }
    let sha = out.stdout.trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

/// `ancestor` が `descendant` の祖先（または同じ）か。
pub fn is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> bool {
    git(dir, &["merge-base", "--is-ancestor", ancestor, descendant]).is_ok_and(|o| o.ok)
}

/// WU の worktree を切る（冪等。既にあれば使い回す。`LocalWorktree::ensure_blocking` と同じ規則）。
pub fn ensure_wu_worktree(worktree: &LocalWorktree) -> Result<(), String> {
    worktree
        .ensure_blocking()
        .map_err(|e| format!("cannot create the work unit worktree: {e}"))
}

/// WU の完了時の commit（`git add -A && git commit`）。変更が無ければ commit しない。
/// 戻り値は `(HEAD の sha, commit したか)`。
pub fn commit_all(dir: &Path, message: &str) -> Result<(String, bool), String> {
    git_ok(dir, &["add", "-A"])?;
    let staged = git(dir, &["diff", "--cached", "--quiet"])?;
    let committed = if staged.ok {
        false
    } else {
        git_ok(dir, &["commit", "--no-verify", "-q", "-m", message])?;
        true
    };
    let head = rev_parse(dir, "HEAD").ok_or_else(|| format!("no HEAD in {}", dir.display()))?;
    Ok((head, committed))
}

/// 工程の統合で merge する 1 件（葉の WU）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeItem {
    pub key: String,
    pub branch: String,
}

/// 1 件の merge の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub key: String,
    /// merge した WU ブランチの HEAD（`PhaseIntegrated.merged[].commit`）。
    pub commit: String,
    /// 既に Task ブランチに入っていたので飛ばした（冪等なやり直し）。
    pub skipped: bool,
}

/// 衝突（`merge --abort` 済み）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub key: String,
    pub branch: String,
    /// 衝突したファイル（`git diff --name-only --diff-filter=U`）。
    pub files: Vec<String>,
}

/// [`integrate`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationOutcome {
    pub merged: Vec<Merged>,
    /// 統合後の Task ブランチの HEAD。
    pub head: String,
    /// 衝突で止まったら `Some`（その WU より後は試していない）。
    pub conflict: Option<Conflict>,
}

/// ADR-0074 D1.4 1〜3: Task の worktree（`task_tree`）で、葉の WU のブランチを順に merge する。
///
/// - `MERGE_HEAD` が残っていれば `merge --abort` してから始める（途中で止まったやり直し）。
/// - 既に入っている WU（`merge-base --is-ancestor <branch> HEAD`）は飛ばす（冪等）。
/// - 衝突したら `merge --abort` して止める（`conflict` に入れて返す。Task ブランチは衝突の前のまま）。
pub fn integrate(
    task_tree: &Path,
    items: &[MergeItem],
    phase: &str,
) -> Result<IntegrationOutcome, String> {
    if rev_parse(task_tree, "MERGE_HEAD").is_some() {
        let _ = git(task_tree, &["merge", "--abort"]);
    }
    let mut merged = Vec::new();
    for item in items {
        let Some(commit) = rev_parse(task_tree, &format!("refs/heads/{}", item.branch)) else {
            return Err(format!(
                "work unit branch {} does not exist in {}",
                item.branch,
                task_tree.display()
            ));
        };
        if is_ancestor(task_tree, &commit, "HEAD") {
            merged.push(Merged {
                key: item.key.clone(),
                commit,
                skipped: true,
            });
            continue;
        }
        let out = git(
            task_tree,
            &[
                "merge",
                "--no-ff",
                "--no-edit",
                "-m",
                &merge_message(&item.key, phase),
                &item.branch,
            ],
        )?;
        if out.ok {
            merged.push(Merged {
                key: item.key.clone(),
                commit,
                skipped: false,
            });
            continue;
        }
        let files = git(task_tree, &["diff", "--name-only", "--diff-filter=U"])
            .map(|o| {
                o.stdout
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let _ = git(task_tree, &["merge", "--abort"]);
        let head = rev_parse(task_tree, "HEAD")
            .ok_or_else(|| "no HEAD after merge --abort".to_string())?;
        return Ok(IntegrationOutcome {
            merged,
            head,
            conflict: Some(Conflict {
                key: item.key.clone(),
                branch: item.branch.clone(),
                files,
            }),
        });
    }
    let head = rev_parse(task_tree, "HEAD")
        .ok_or_else(|| format!("no HEAD in {}", task_tree.display()))?;
    Ok(IntegrationOutcome {
        merged,
        head,
        conflict: None,
    })
}

/// `git diff --stat <base>..<branch>`（repair WU の最小 context 用。失敗は空文字）。
pub fn diff_stat(dir: &Path, base: &str, branch: &str) -> String {
    git(dir, &["diff", "--stat", &format!("{base}...{branch}")])
        .map(|o| o.stdout.trim().to_string())
        .unwrap_or_default()
}

/// 統合が済んだ WU の worktree を消す（ADR-0074 D1.2。ブランチは Task の終端まで残す）。
pub fn remove_wu_worktree(worktree: &LocalWorktree) -> Result<(), String> {
    if !worktree.dir.exists() {
        return Ok(());
    }
    let dir = worktree.dir.to_string_lossy().into_owned();
    let out = git(&worktree.repo, &["worktree", "remove", "--force", &dir])?;
    if !out.ok {
        let _ = git(&worktree.repo, &["worktree", "prune"]);
        if worktree.dir.exists() {
            std::fs::remove_dir_all(&worktree.dir)
                .map_err(|e| format!("cannot remove {}: {e}", worktree.dir.display()))?;
        }
        let _ = git(&worktree.repo, &["worktree", "prune"]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = git(dir, args).expect("git");
        assert!(out.ok, "git {args:?}: {}", out.stderr);
        out.stdout.trim().to_string()
    }

    /// 元のリポジトリ（main に 1 commit）と、Task の worktree（`celeris/<task>`）を作る。
    fn setup(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        std::fs::write(repo.join("shared.txt"), "line1\nline2\nline3\n").unwrap();
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-q", "-m", "init"]);
        let task_dir = root.join("ws").join("T1");
        let task_tree = task_dir.join("repos").join("repo");
        let base = rev_parse(&repo, "main").unwrap();
        let lwt = LocalWorktree {
            repo: repo.clone(),
            task_dir: task_dir.clone(),
            dir: task_tree.clone(),
            branch: "celeris/T1".into(),
            base: BaseRef {
                kind: BaseKind::Main,
                sha: base,
            },
        };
        lwt.ensure_blocking().unwrap();
        (repo, task_dir, task_tree)
    }

    fn make_wu(
        repo: &Path,
        task_dir: &Path,
        task_tree: &Path,
        key: &str,
        base_rev: &str,
    ) -> LocalWorktree {
        let base = rev_parse(task_tree, base_rev).unwrap();
        let wt = wu_worktree(task_dir, "T1", key, "repo", repo, &base);
        ensure_wu_worktree(&wt).unwrap();
        wt
    }

    #[test]
    fn wu_worktree_is_created_on_its_own_branch_from_the_base_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let (repo, task_dir, task_tree) = setup(root.path());
        let wt = make_wu(&repo, &task_dir, &task_tree, "a", "HEAD");
        assert_eq!(wt.dir, task_dir.join("wu/a/repos/repo"));
        assert!(wt.dir.join("README.md").is_file());
        assert_eq!(
            sh(&wt.dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "celeris-wu/T1/a"
        );
        // 2 回目は何もしない（使い回す）。
        ensure_wu_worktree(&wt).unwrap();
        // 変更が無ければ commit しない。
        let (head0, committed) = commit_all(&wt.dir, "wu/a: nothing").unwrap();
        assert!(!committed);
        assert_eq!(head0, wt.base.sha);
        std::fs::write(wt.dir.join("a.txt"), "a\n").unwrap();
        let (head1, committed) = commit_all(&wt.dir, &commit_message("a", "Title A")).unwrap();
        assert!(committed);
        assert_ne!(head1, head0);
        assert_eq!(
            sh(&wt.dir, &["log", "-1", "--format=%an %s"]),
            "celeris wu/a: Title A"
        );
    }

    #[test]
    fn integrate_merges_leaves_deterministically_and_skips_ones_already_in() {
        let root = tempfile::tempdir().unwrap();
        let (repo, task_dir, task_tree) = setup(root.path());
        let mut items = Vec::new();
        for key in ["a", "b", "c"] {
            let wt = make_wu(&repo, &task_dir, &task_tree, key, "HEAD");
            std::fs::write(wt.dir.join(format!("{key}.txt")), key).unwrap();
            commit_all(&wt.dir, &commit_message(key, key)).unwrap();
            items.push(MergeItem {
                key: key.into(),
                branch: wu_branch("T1", key),
            });
        }
        let out = integrate(&task_tree, &items, "build").unwrap();
        assert!(out.conflict.is_none());
        assert_eq!(out.merged.len(), 3);
        assert!(out.merged.iter().all(|m| !m.skipped));
        for key in ["a", "b", "c"] {
            assert!(task_tree.join(format!("{key}.txt")).is_file());
        }
        let subjects = sh(&task_tree, &["log", "--first-parent", "--format=%s", "-3"]);
        assert_eq!(
            subjects.lines().collect::<Vec<_>>(),
            vec![
                "integrate wu/c (phase build)",
                "integrate wu/b (phase build)",
                "integrate wu/a (phase build)"
            ]
        );
        assert_eq!(out.head, rev_parse(&task_tree, "HEAD").unwrap());
    }

    /// ADR-0074 §6 F2 のテスト表: 再起動後のやり直しは冪等（済んだ merge は飛ばし、途中の
    /// `MERGE_HEAD` は abort してからやり直す）。
    #[test]
    fn merge_is_idempotent_after_restart() {
        let root = tempfile::tempdir().unwrap();
        let (repo, task_dir, task_tree) = setup(root.path());
        let mut items = Vec::new();
        for key in ["a", "b"] {
            let wt = make_wu(&repo, &task_dir, &task_tree, key, "HEAD");
            std::fs::write(wt.dir.join(format!("{key}.txt")), key).unwrap();
            commit_all(&wt.dir, &commit_message(key, key)).unwrap();
            items.push(MergeItem {
                key: key.into(),
                branch: wu_branch("T1", key),
            });
        }
        // 1 件目だけ入った状態で「落ちた」ことにする。さらに 2 件目の merge を途中で止める
        // （`--no-commit` で MERGE_HEAD を残す）。
        let first = integrate(&task_tree, &items[..1], "build").unwrap();
        assert_eq!(first.merged.len(), 1);
        sh(
            &task_tree,
            &["merge", "--no-ff", "--no-commit", &wu_branch("T1", "b")],
        );
        assert!(rev_parse(&task_tree, "MERGE_HEAD").is_some());
        // やり直し: a は飛ばし、MERGE_HEAD を abort してから b を入れる。
        let again = integrate(&task_tree, &items, "build").unwrap();
        assert!(again.conflict.is_none());
        assert_eq!(
            again
                .merged
                .iter()
                .map(|m| (m.key.as_str(), m.skipped))
                .collect::<Vec<_>>(),
            vec![("a", true), ("b", false)]
        );
        // 3 回目: 何も変わらない（HEAD 同じ、全部 skipped）。
        let head = rev_parse(&task_tree, "HEAD").unwrap();
        let third = integrate(&task_tree, &items, "build").unwrap();
        assert_eq!(third.head, head);
        assert!(third.merged.iter().all(|m| m.skipped));
        let merges = sh(&task_tree, &["log", "--merges", "--format=%s"]);
        assert_eq!(merges.lines().count(), 2, "{merges}");
    }

    #[test]
    fn a_conflict_is_aborted_and_reported_and_resumes_after_the_repair() {
        let root = tempfile::tempdir().unwrap();
        let (repo, task_dir, task_tree) = setup(root.path());
        let mut items = Vec::new();
        for (key, text) in [("a", "line1\nA\nline3\n"), ("b", "line1\nB\nline3\n")] {
            let wt = make_wu(&repo, &task_dir, &task_tree, key, "HEAD");
            std::fs::write(wt.dir.join("shared.txt"), text).unwrap();
            commit_all(&wt.dir, &commit_message(key, key)).unwrap();
            items.push(MergeItem {
                key: key.into(),
                branch: wu_branch("T1", key),
            });
        }
        let before = rev_parse(&task_tree, "HEAD").unwrap();
        let out = integrate(&task_tree, &items, "build").unwrap();
        let conflict = out.conflict.expect("conflict");
        assert_eq!(conflict.key, "b");
        assert_eq!(conflict.files, vec!["shared.txt".to_string()]);
        assert_eq!(out.merged.len(), 1);
        assert_ne!(out.head, before, "a は入っている");
        assert!(rev_parse(&task_tree, "MERGE_HEAD").is_none(), "abort 済み");
        assert!(status_clean(&task_tree));
        // repair WU の代わり: Task の worktree で b を merge し、衝突を解消して commit する。
        let _ = git(
            &task_tree,
            &["merge", "--no-ff", "--no-edit", &wu_branch("T1", "b")],
        );
        std::fs::write(task_tree.join("shared.txt"), "line1\nA\nB\nline3\n").unwrap();
        sh(&task_tree, &["add", "-A"]);
        sh(&task_tree, &["commit", "-q", "--no-edit"]);
        // 続きから: a も b も既に入っているので飛ばす。
        let resumed = integrate(&task_tree, &items, "build").unwrap();
        assert!(resumed.conflict.is_none());
        assert!(resumed.merged.iter().all(|m| m.skipped));
    }

    #[test]
    fn a_stacked_unit_branches_from_its_dependency_and_only_the_leaf_is_merged() {
        let root = tempfile::tempdir().unwrap();
        let (repo, task_dir, task_tree) = setup(root.path());
        let a = make_wu(&repo, &task_dir, &task_tree, "a", "HEAD");
        std::fs::write(a.dir.join("a.txt"), "a").unwrap();
        let (a_head, _) = commit_all(&a.dir, "wu/a: a").unwrap();
        // b は a のブランチの HEAD から切る（積み上げ）。
        let b_base = rev_parse(&repo, &format!("refs/heads/{}", wu_branch("T1", "a"))).unwrap();
        assert_eq!(b_base, a_head);
        let b = wu_worktree(&task_dir, "T1", "b", "repo", &repo, &b_base);
        ensure_wu_worktree(&b).unwrap();
        assert!(b.dir.join("a.txt").is_file(), "a の成果の上で始まる");
        std::fs::write(b.dir.join("b.txt"), "b").unwrap();
        commit_all(&b.dir, "wu/b: b").unwrap();
        let out = integrate(
            &task_tree,
            &[MergeItem {
                key: "b".into(),
                branch: wu_branch("T1", "b"),
            }],
            "build",
        )
        .unwrap();
        assert!(out.conflict.is_none());
        assert!(task_tree.join("a.txt").is_file() && task_tree.join("b.txt").is_file());
        assert!(is_ancestor(&task_tree, &a_head, "HEAD"));
        // worktree を消してもブランチは残る。
        remove_wu_worktree(&a).unwrap();
        remove_wu_worktree(&b).unwrap();
        assert!(!a.dir.exists() && !b.dir.exists());
        assert!(rev_parse(&repo, &format!("refs/heads/{}", wu_branch("T1", "a"))).is_some());
    }

    fn status_clean(dir: &Path) -> bool {
        sh(dir, &["status", "--porcelain"]).is_empty()
    }
}
