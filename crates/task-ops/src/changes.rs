//! 変更の取り込み（intake）の足回り（ADR-0043 D5。Phase 54）。
//!
//! タスクが `celeris/<task_id>` ブランチに積んだ変更を、**人が**見て取り込むための道具:
//!
//! - [`changes`] — リポジトリ 1 つ分の差分の要約（base / head / ahead / ファイル一覧 / 汚れ）
//! - [`file_diff`] — 1 ファイルの unified diff（200 KiB で切る）
//! - [`merge_into_default_branch`] — 一時 worktree で rebase → default_branch を fast-forward
//! - [`discard`] / [`remove_worktree_and_branch`] — worktree とブランチを消す
//! - [`push_branch`] / [`gh_*`] — `origin` へ push して `gh` で PR を作る・見る・merge する
//!
//! ここは **`git` と `gh` を起こすだけ**で、判断も LLM も無い（DESIGN 原則 1）。全ての子プロセスに
//! 待ち時間の上限があり、超えたら殺して「失敗」を返す（API のハンドラが握りっぱなしにならないように）。
//!
//! **押すのは人だけ**（SPEC §3.6 / ADR-0043 D5）。ワーカーのプロトコルにはこの経路を出さない。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 読み取り（`status` / `diff` / `rev-list`）の待ち時間。
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// 書き込み（`worktree add` / `rebase` / `push`）の待ち時間。
pub const GIT_WRITE_TIMEOUT: Duration = Duration::from_secs(300);
/// ADR-0043 D5: PR の状態を見るのは 10 秒で諦める（画面を開くたびに走るため）。
pub const GH_VIEW_TIMEOUT: Duration = Duration::from_secs(10);
/// PR を作る・merge するのは人が押したときだけなので少し長い。
pub const GH_WRITE_TIMEOUT: Duration = Duration::from_secs(120);
/// ADR-0043 D5: 1 ファイルの diff は 200 KiB で切る。
pub const MAX_DIFF_BYTES: usize = 200 * 1024;
/// `gh auth status` の結果をプロセス内で使い回す時間（ADR-0043 D5）。
pub const GH_AUTH_CACHE: Duration = Duration::from_secs(60);
/// 追跡外のファイルの行数を数えるときに読む上限（これより大きければ `additions = 0`）。
const MAX_UNTRACKED_BYTES: u64 = 1024 * 1024;

// ---------------------------------------------------------------------------
// 子プロセス（決定的。全部に待ち時間の上限がある）
// ---------------------------------------------------------------------------

/// 子プロセスの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub ok: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl CmdOutput {
    /// 失敗の 1 行説明（人に見せる。`stderr` の最初の非空行）。
    pub fn why(&self) -> String {
        if self.timed_out {
            return "コマンドが時間内に終わりませんでした".to_string();
        }
        let line = self
            .stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .or_else(|| self.stdout.lines().map(str::trim).find(|l| !l.is_empty()))
            .unwrap_or("");
        if line.is_empty() {
            format!("exit {:?}", self.code)
        } else {
            format!("{line}（exit {:?}）", self.code)
        }
    }
}

/// 起動して、`timeout` を過ぎたら殺す。パイプは別スレッドで読み切る（詰まらせない）。
/// 起動そのものに失敗したら `None`（`git` / `gh` が無い）。
fn run(mut cmd: Command, timeout: Duration) -> Option<CmdOutput> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let out_thread = std::thread::spawn(move || read_all(out_pipe));
    let err_thread = std::thread::spawn(move || read_all(err_pipe));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Err(_) => break None,
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().ok();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    let code = status.and_then(|s| s.code());
    Some(CmdOutput {
        ok: !timed_out && code == Some(0),
        code,
        stdout,
        stderr,
        timed_out,
    })
}

fn read_all(pipe: Option<impl std::io::Read>) -> String {
    let mut buf = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// `git -C <dir> <args...>`（人の設定と対話的な認証に引きずられない）。
pub fn git(dir: &Path, args: &[&str], timeout: Duration) -> Option<CmdOutput> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        // 引用されたパス（`"src/\346\227\245"`）を避け、人の hooks と対話的な認証を止める。
        .args(["-c", "core.quotePath=false"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args);
    run(cmd, timeout)
}

/// 任意の子プロセスを 1 つ起こす（上限つき）。`git` が無い環境での `grep` の代わりなど、
/// **`git` 以外の決定的な道具**を呼ぶときだけ使う（ADR-0047 D3 の全文検索のフォールバック）。
pub fn run_with_timeout(
    dir: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Option<CmdOutput> {
    let mut cmd = Command::new(program);
    cmd.current_dir(dir).args(args);
    run(cmd, timeout)
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    git(dir, args, GIT_TIMEOUT).is_some_and(|o| o.ok)
}

fn git_line(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git(dir, args, GIT_TIMEOUT)?;
    if !out.ok {
        return None;
    }
    let line = out.stdout.trim().to_string();
    if line.is_empty() { None } else { Some(line) }
}

// ---------------------------------------------------------------------------
// 読み取り（`GET /tasks/{id}/changes`）
// ---------------------------------------------------------------------------

/// 変わったファイル 1 件。
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ChangedFile {
    /// リポジトリの根からの相対パス。
    pub path: String,
    /// `A`（追加）/ `M`（変更）/ `D`（削除）/ `?`（git の管理外）/ `T`（種類が変わった）。
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    /// バイナリ（git が行数を出さなかった）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
}

/// ファイル数と ± の合計。
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct DiffStat {
    pub files: u64,
    pub additions: u64,
    pub deletions: u64,
}

/// リポジトリ 1 つ分の「このタスクが変えたもの」（ADR-0043 D5）。
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct RepoChanges {
    /// 分岐した地点の sha（`merge-base(default_branch, head)`。取れなければ目印の base）。
    pub base: String,
    /// いまのブランチの先端の sha。
    pub head: String,
    /// `base..head` のコミットの数（**コミットが無ければ 0**。ADR-0043 D5）。
    pub ahead: u64,
    pub files: Vec<ChangedFile>,
    pub stat: DiffStat,
    /// 作業ツリーに未コミットの変更がある。
    pub dirty: bool,
    /// worktree もブランチも無い（取り込み済み・中止済み・そもそも切っていない）。
    pub missing: bool,
}

/// どこを見て差分を出したか（`file_diff` に同じものを渡す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesSource {
    /// worktree がある（未コミットの変更も見える）。
    Worktree(PathBuf),
    /// worktree は無いがブランチはある（元のリポジトリで `base..branch` を見る）。
    Branch { repo: PathBuf, branch: String },
    /// どちらも無い。
    Missing,
}

/// 見るべき場所を決める（worktree → ブランチ → 無い）。
pub fn source_for(repo: &Path, worktree: Option<&Path>, branch: &str) -> ChangesSource {
    if let Some(dir) = worktree
        && dir.join(".git").exists()
        && git_ok(dir, &["rev-parse", "--git-dir"])
    {
        return ChangesSource::Worktree(dir.to_path_buf());
    }
    if !branch.is_empty()
        && git_ok(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
    {
        return ChangesSource::Branch {
            repo: repo.to_path_buf(),
            branch: branch.to_string(),
        };
    }
    ChangesSource::Missing
}

/// ADR-0043 D1: git の既定のブランチ。設定（`project_repos.default_branch`）が無ければ
/// `origin/HEAD` → `main` → `master` の順で検出する。何も無ければ `"main"`。
pub fn default_branch(repo: &Path, configured: Option<&str>) -> String {
    if let Some(name) = configured.map(str::trim).filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    if let Some(head) = git_line(
        repo,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    ) && let Some(name) = head.strip_prefix("origin/")
        && !name.is_empty()
    {
        return name.to_string();
    }
    for name in ["main", "master"] {
        if git_ok(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ],
        ) {
            return name.to_string();
        }
    }
    "main".to_string()
}

/// リポジトリ 1 つ分の差分の要約（ADR-0043 D5）。
///
/// - worktree があれば**未コミットの変更も含めて** base からの差分を出す（人が見たいのはそれ）。
///   `ahead` は `base..HEAD` のコミット数なので、まだコミットしていなければ 0 になる。
/// - worktree が無くブランチだけあれば、元のリポジトリで `base..branch` を見る（`dirty = false`）。
/// - どちらも無ければ `missing = true`、`ahead = 0`。
pub fn changes(
    repo: &Path,
    worktree: Option<&Path>,
    branch: &str,
    default_branch: &str,
    fallback_base: Option<&str>,
) -> RepoChanges {
    match source_for(repo, worktree, branch) {
        ChangesSource::Missing => RepoChanges {
            missing: true,
            ..Default::default()
        },
        ChangesSource::Worktree(dir) => {
            let head = git_line(&dir, &["rev-parse", "HEAD"]).unwrap_or_default();
            let base = base_of(&dir, default_branch, &head, fallback_base);
            let dirty = git(&dir, &["status", "--porcelain"], GIT_TIMEOUT)
                .is_some_and(|o| o.ok && !o.stdout.trim().is_empty());
            let mut files = diff_files(&dir, &[&base]);
            files.extend(untracked_files(&dir));
            files.sort_by(|a, b| a.path.cmp(&b.path));
            RepoChanges {
                ahead: commit_count(&dir, &base, "HEAD"),
                stat: stat_of(&files),
                base,
                head,
                files,
                dirty,
                missing: false,
            }
        }
        ChangesSource::Branch { repo, branch } => {
            let reference = format!("refs/heads/{branch}");
            let head = git_line(&repo, &["rev-parse", &reference]).unwrap_or_default();
            let base = base_of(&repo, default_branch, &head, fallback_base);
            let range = format!("{base}..{reference}");
            let files = diff_files(&repo, &[&range]);
            RepoChanges {
                ahead: commit_count(&repo, &base, &reference),
                stat: stat_of(&files),
                base,
                head,
                files,
                dirty: false,
                missing: false,
            }
        }
    }
}

fn stat_of(files: &[ChangedFile]) -> DiffStat {
    DiffStat {
        files: files.len() as u64,
        additions: files.iter().map(|f| f.additions).sum(),
        deletions: files.iter().map(|f| f.deletions).sum(),
    }
}

/// 分岐点。`merge-base(default_branch, head)` が取れなければ目印の base、それも無ければ head。
fn base_of(dir: &Path, default_branch: &str, head: &str, fallback: Option<&str>) -> String {
    if !head.is_empty()
        && let Some(sha) = git_line(dir, &["merge-base", default_branch, head])
    {
        return sha;
    }
    if let Some(fallback) = fallback.map(str::trim).filter(|b| !b.is_empty())
        && let Some(sha) = git_line(
            dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{fallback}^{{commit}}"),
            ],
        )
    {
        return sha;
    }
    head.to_string()
}

fn commit_count(dir: &Path, base: &str, head: &str) -> u64 {
    if base.is_empty() || head.is_empty() {
        return 0;
    }
    git_line(dir, &["rev-list", "--count", &format!("{base}..{head}")])
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

/// `git diff --numstat` + `--name-status` を突き合わせる（`--no-renames` で `a => b` を避ける）。
fn diff_files(dir: &Path, rev: &[&str]) -> Vec<ChangedFile> {
    let mut args: Vec<&str> = vec!["diff", "--numstat", "--no-renames"];
    args.extend_from_slice(rev);
    let Some(numstat) = git(dir, &args, GIT_TIMEOUT).filter(|o| o.ok) else {
        return Vec::new();
    };
    let mut args: Vec<&str> = vec!["diff", "--name-status", "--no-renames"];
    args.extend_from_slice(rev);
    let statuses = git(dir, &args, GIT_TIMEOUT)
        .filter(|o| o.ok)
        .map(|o| parse_name_status(&o.stdout))
        .unwrap_or_default();
    parse_numstat(&numstat.stdout)
        .into_iter()
        .map(|(path, additions, deletions, binary)| {
            let status = statuses
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| "M".to_string());
            ChangedFile {
                path,
                status,
                additions,
                deletions,
                binary,
            }
        })
        .collect()
}

/// `--numstat` の 1 行は `<+>\t<->\t<path>`（バイナリは `-\t-\t<path>`）。
pub fn parse_numstat(text: &str) -> Vec<(String, u64, u64, bool)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(add), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        let binary = add == "-" || del == "-";
        out.push((
            path.to_string(),
            add.parse::<u64>().unwrap_or(0),
            del.parse::<u64>().unwrap_or(0),
            binary,
        ));
    }
    out
}

/// `--name-status` の 1 行は `<status>\t<path>`。
pub fn parse_name_status(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(2, '\t');
        let (Some(status), Some(path)) = (parts.next(), parts.next()) else {
            continue;
        };
        let status = status.trim();
        let path = path.trim();
        if status.is_empty() || path.is_empty() {
            continue;
        }
        // `M100` のような形（`--find-copies` 等）は先頭の 1 文字だけ使う。
        out.push((path.to_string(), status.chars().take(1).collect::<String>()));
    }
    out
}

/// git の管理外のファイル（`.gitignore` は尊重する）。人が見たいのは「このタスクが置いたもの」なので
/// 追加行だけ数える（大きすぎる・バイナリは 0）。
fn untracked_files(dir: &Path) -> Vec<ChangedFile> {
    let Some(out) = git(
        dir,
        &["ls-files", "--others", "--exclude-standard"],
        GIT_TIMEOUT,
    )
    .filter(|o| o.ok) else {
        return Vec::new();
    };
    out.stdout
        .lines()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|path| {
            let (additions, binary) = count_added_lines(&dir.join(path));
            ChangedFile {
                path: path.to_string(),
                status: "?".to_string(),
                additions,
                deletions: 0,
                binary,
            }
        })
        .collect()
}

fn count_added_lines(path: &Path) -> (u64, bool) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (0, false);
    };
    if !meta.is_file() || meta.len() > MAX_UNTRACKED_BYTES {
        return (0, false);
    }
    let Ok(bytes) = std::fs::read(path) else {
        return (0, false);
    };
    if bytes.contains(&0) {
        return (0, true);
    }
    let lines = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    // 末尾に改行が無いファイルの最後の行も 1 行と数える。
    let lines = if bytes.is_empty() || bytes.ends_with(b"\n") {
        lines
    } else {
        lines + 1
    };
    (lines, false)
}

/// 1 ファイルの unified diff（ADR-0043 D5。200 KiB で切る）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub diff: String,
    /// 200 KiB を超えたので途中で切った。
    pub truncated: bool,
}

/// `path` 1 件の unified diff。git が起こせない・そのファイルに差分が無いときは空の diff を返す。
pub fn file_diff(
    repo: &Path,
    worktree: Option<&Path>,
    branch: &str,
    default_branch: &str,
    fallback_base: Option<&str>,
    path: &str,
) -> Option<FileDiff> {
    let source = source_for(repo, worktree, branch);
    let (dir, rev) = match &source {
        ChangesSource::Missing => return None,
        ChangesSource::Worktree(dir) => {
            let head = git_line(dir, &["rev-parse", "HEAD"]).unwrap_or_default();
            (
                dir.clone(),
                base_of(dir, default_branch, &head, fallback_base),
            )
        }
        ChangesSource::Branch { repo, branch } => {
            let reference = format!("refs/heads/{branch}");
            let head = git_line(repo, &["rev-parse", &reference]).unwrap_or_default();
            let base = base_of(repo, default_branch, &head, fallback_base);
            (repo.clone(), format!("{base}..{reference}"))
        }
    };
    let out = git(
        &dir,
        &["diff", "--no-renames", &rev, "--", path],
        GIT_TIMEOUT,
    )?;
    let mut text = if out.ok { out.stdout } else { String::new() };
    // 追跡外のファイル（`git diff` には出ない）は `--no-index` で「空 → いまの中身」を出す。
    if text.trim().is_empty()
        && matches!(source, ChangesSource::Worktree(_))
        && dir.join(path).is_file()
        && let Some(untracked) = git(
            &dir,
            &["diff", "--no-index", "--", "/dev/null", path],
            GIT_TIMEOUT,
        )
    {
        // `--no-index` は差分があると exit 1 なので `ok` は見ない。
        text = untracked.stdout;
    }
    Some(truncate_diff(&text, path))
}

/// 200 KiB で切る（文字の境界で切る。切ったら印を立てる）。
pub fn truncate_diff(text: &str, path: &str) -> FileDiff {
    if text.len() <= MAX_DIFF_BYTES {
        return FileDiff {
            path: path.to_string(),
            diff: text.to_string(),
            truncated: false,
        };
    }
    let mut end = MAX_DIFF_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    FileDiff {
        path: path.to_string(),
        diff: text[..end].to_string(),
        truncated: true,
    }
}

// ---------------------------------------------------------------------------
// 取り込み（`POST /tasks/{id}/changes/{repo}/integrate`）
// ---------------------------------------------------------------------------

/// `merge` の結果（ADR-0043 D5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// default_branch を `sha` まで進めた。
    Merged { sha: String, fast_forwarded: bool },
    /// rebase が衝突した（worktree はそのまま。衝突したファイルの一覧を返す）。
    Conflict { files: Vec<String> },
    /// 人のチェックアウトが default_branch を編集中（409）。
    Busy { detail: String },
    /// git が失敗した（`detail` に理由）。
    Failed { detail: String },
}

/// 人のチェックアウト（`local.path`）がいま出しているブランチ。detached なら `None`。
pub fn current_branch(repo: &Path) -> Option<String> {
    let name = git_line(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if name == "HEAD" { None } else { Some(name) }
}

/// 作業ツリーに未コミットの変更があるか（git が動かせなければ `None`）。
pub fn is_dirty(dir: &Path) -> Option<bool> {
    let out = git(dir, &["status", "--porcelain"], GIT_TIMEOUT)?;
    if !out.ok {
        return None;
    }
    Some(!out.stdout.trim().is_empty())
}

/// ADR-0043 D5 の `merge`: 一時 worktree でブランチを default_branch に rebase し、成功したら
/// **default_branch を fast-forward** する。`origin` には push しない。
///
/// - 人のチェックアウトが default_branch を出していて未コミットの変更があれば `Busy`（409）
/// - 出していて綺麗なら `git -C <repo> merge --ff-only <sha>`（作業ツリーもそのまま進む）
/// - 別のブランチを出していれば `git -C <repo> update-ref`（作業ツリーには触らない）
/// - rebase が衝突したら `rebase --abort` して `Conflict`（worktree もブランチも残す）
pub fn merge_into_default_branch(
    repo: &Path,
    branch: &str,
    default_branch: &str,
    temp_dir: &Path,
) -> MergeOutcome {
    let branch_ref = format!("refs/heads/{branch}");
    if !git_ok(repo, &["rev-parse", "--verify", "--quiet", &branch_ref]) {
        return MergeOutcome::Failed {
            detail: format!("ブランチ {branch} がありません"),
        };
    }
    let default_ref = format!("refs/heads/{default_branch}");
    let Some(default_sha) = git_line(repo, &["rev-parse", "--verify", "--quiet", &default_ref])
    else {
        return MergeOutcome::Failed {
            detail: format!("既定のブランチ {default_branch} がありません"),
        };
    };
    // 人のチェックアウトが default_branch を編集中なら、何も触らずに 409（ADR-0043 D5）。
    let checked_out = current_branch(repo).is_some_and(|b| b == default_branch);
    if checked_out && is_dirty(repo) != Some(false) {
        return MergeOutcome::Busy {
            detail: format!("{default_branch} が編集中"),
        };
    }

    if let Some(parent) = temp_dir.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return MergeOutcome::Failed {
            detail: "一時 worktree を作れませんでした".to_string(),
        };
    }
    let temp = temp_dir.to_string_lossy().into_owned();
    let add = git(
        repo,
        &["worktree", "add", "--detach", &temp, &branch_ref],
        GIT_WRITE_TIMEOUT,
    );
    match add {
        Some(o) if o.ok => {}
        Some(o) => {
            return MergeOutcome::Failed {
                detail: format!("一時 worktree を作れませんでした: {}", o.why()),
            };
        }
        None => {
            return MergeOutcome::Failed {
                detail: "git を起動できませんでした".to_string(),
            };
        }
    }

    let outcome = rebase_and_advance(repo, temp_dir, default_branch, &default_sha, checked_out);
    let _ = git(
        repo,
        &["worktree", "remove", "--force", &temp],
        GIT_WRITE_TIMEOUT,
    );
    let _ = git(repo, &["worktree", "prune"], GIT_TIMEOUT);
    if temp_dir.exists() {
        let _ = std::fs::remove_dir_all(temp_dir);
    }
    outcome
}

fn rebase_and_advance(
    repo: &Path,
    temp_dir: &Path,
    default_branch: &str,
    default_sha: &str,
    checked_out: bool,
) -> MergeOutcome {
    let rebase = git(temp_dir, &["rebase", default_sha], GIT_WRITE_TIMEOUT);
    match rebase {
        Some(o) if o.ok => {}
        Some(o) => {
            let files = git(
                temp_dir,
                &["diff", "--name-only", "--diff-filter=U"],
                GIT_TIMEOUT,
            )
            .filter(|o| o.ok)
            .map(|o| {
                o.stdout
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
            let _ = git(temp_dir, &["rebase", "--abort"], GIT_WRITE_TIMEOUT);
            if files.is_empty() && !o.stdout.contains("CONFLICT") && !o.stderr.contains("CONFLICT")
            {
                return MergeOutcome::Failed {
                    detail: format!("rebase に失敗しました: {}", o.why()),
                };
            }
            return MergeOutcome::Conflict { files };
        }
        None => {
            return MergeOutcome::Failed {
                detail: "git を起動できませんでした".to_string(),
            };
        }
    }
    let Some(new_sha) = git_line(temp_dir, &["rev-parse", "HEAD"]) else {
        return MergeOutcome::Failed {
            detail: "rebase の結果を読めませんでした".to_string(),
        };
    };
    if checked_out {
        // 人の作業ツリーが default_branch を出していて綺麗なので、そのまま早送りする。
        match git(repo, &["merge", "--ff-only", &new_sha], GIT_WRITE_TIMEOUT) {
            Some(o) if o.ok => MergeOutcome::Merged {
                sha: new_sha,
                fast_forwarded: true,
            },
            Some(o) => MergeOutcome::Failed {
                detail: format!("{default_branch} を早送りできませんでした: {}", o.why()),
            },
            None => MergeOutcome::Failed {
                detail: "git を起動できませんでした".to_string(),
            },
        }
    } else {
        // 別のブランチ（または detached）を出しているので、ref だけ動かす。作業ツリーには触らない。
        let reference = format!("refs/heads/{default_branch}");
        match git(
            repo,
            &["update-ref", &reference, &new_sha, default_sha],
            GIT_WRITE_TIMEOUT,
        ) {
            Some(o) if o.ok => MergeOutcome::Merged {
                sha: new_sha,
                fast_forwarded: false,
            },
            Some(o) => MergeOutcome::Failed {
                detail: format!("{default_branch} を動かせませんでした: {}", o.why()),
            },
            None => MergeOutcome::Failed {
                detail: "git を起動できませんでした".to_string(),
            },
        }
    }
}

/// worktree を消してブランチも消す（ADR-0043 D5 の `merge` 成功後と `discard`）。
/// 人が手で消していても落ちない（`prune` してから `branch -D`）。
pub fn remove_worktree_and_branch(
    repo: &Path,
    worktree: Option<&Path>,
    branch: &str,
) -> Result<(), String> {
    if let Some(dir) = worktree
        && dir.exists()
    {
        let path = dir.to_string_lossy().into_owned();
        let removed = git(
            repo,
            &["worktree", "remove", "--force", &path],
            GIT_WRITE_TIMEOUT,
        )
        .is_some_and(|o| o.ok);
        if !removed {
            let _ = git(repo, &["worktree", "prune"], GIT_TIMEOUT);
            if dir.exists() && std::fs::remove_dir_all(dir).is_err() {
                return Err(format!("作業ツリー {path} を消せませんでした"));
            }
        }
        let _ = git(repo, &["worktree", "prune"], GIT_TIMEOUT);
    }
    if branch.is_empty() {
        return Ok(());
    }
    let reference = format!("refs/heads/{branch}");
    if !git_ok(repo, &["rev-parse", "--verify", "--quiet", &reference]) {
        return Ok(());
    }
    match git(repo, &["branch", "-D", branch], GIT_WRITE_TIMEOUT) {
        Some(o) if o.ok => Ok(()),
        Some(o) => Err(format!("ブランチ {branch} を消せませんでした: {}", o.why())),
        None => Err("git を起動できませんでした".to_string()),
    }
}

/// ADR-0043 D5 の `discard`: worktree とブランチを消す（確認は呼び出し側で取る）。
pub fn discard(repo: &Path, worktree: Option<&Path>, branch: &str) -> Result<(), String> {
    remove_worktree_and_branch(repo, worktree, branch)
}

// ---------------------------------------------------------------------------
// GitHub（`gh`）
// ---------------------------------------------------------------------------

/// `origin` リモートがあるか（ADR-0043 D5: `pr` の前提）。
pub fn has_origin(repo: &Path) -> bool {
    git_line(repo, &["remote", "get-url", "origin"]).is_some()
}

/// `git push -u origin <branch>`。
pub fn push_branch(repo: &Path, branch: &str) -> Result<(), String> {
    match git(repo, &["push", "-u", "origin", branch], GIT_WRITE_TIMEOUT) {
        Some(o) if o.ok => Ok(()),
        Some(o) => Err(format!("push に失敗しました: {}", o.why())),
        None => Err("git を起動できませんでした".to_string()),
    }
}

fn gh(gh_path: &str, repo: &Path, args: &[&str], timeout: Duration) -> Option<CmdOutput> {
    let mut cmd = Command::new(gh_path);
    // `auth status` はリポジトリが要らないので、消えた作業場所でも「gh が無い」と誤判定しないように、
    // 実在するディレクトリのときだけ cwd を移す（`pr create` / `pr view` は必ず実在する）。
    if repo.is_dir() {
        cmd.current_dir(repo);
    }
    cmd.env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("NO_COLOR", "1")
        .args(args);
    run(cmd, timeout)
}

static GH_AUTH: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

/// `gh` が PATH にあって認証済みか（ADR-0043 D5: **プロセス内で 60 秒だけ**使い回す）。
pub fn gh_authenticated(gh_path: &str, repo: &Path) -> bool {
    if let Ok(cache) = GH_AUTH.lock()
        && let Some((at, ok)) = *cache
        && at.elapsed() < GH_AUTH_CACHE
    {
        return ok;
    }
    let ok = gh(gh_path, repo, &["auth", "status"], GH_VIEW_TIMEOUT).is_some_and(|o| o.ok);
    if let Ok(mut cache) = GH_AUTH.lock() {
        *cache = Some((Instant::now(), ok));
    }
    ok
}

/// テスト専用: `gh auth status` の記憶を捨てる（偽の `gh` を差し替えるたびに呼ぶ）。
pub fn forget_gh_auth() {
    if let Ok(mut cache) = GH_AUTH.lock() {
        *cache = None;
    }
}

/// 作った PR（URL と番号）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub number: i64,
    pub url: String,
}

/// `gh pr create` の出力から URL と番号を拾う（出力の最後の `https://…/pull/<n>`）。
pub fn parse_pr_url(text: &str) -> Option<PrRef> {
    let mut found: Option<PrRef> = None;
    for token in text.split_whitespace() {
        let token = token.trim_end_matches(['.', ',', ')']);
        if !token.starts_with("https://") {
            continue;
        }
        let Some((_, tail)) = token.split_once("/pull/") else {
            continue;
        };
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(number) = digits.parse::<i64>() else {
            continue;
        };
        found = Some(PrRef {
            number,
            url: token.to_string(),
        });
    }
    found
}

/// `gh pr create --base <base> --head <branch> --title … --body …`。
pub fn gh_pr_create(
    gh_path: &str,
    repo: &Path,
    base: &str,
    branch: &str,
    title: &str,
    body: &str,
) -> Result<PrRef, String> {
    let out = gh(
        gh_path,
        repo,
        &[
            "pr", "create", "--base", base, "--head", branch, "--title", title, "--body", body,
        ],
        GH_WRITE_TIMEOUT,
    )
    .ok_or_else(|| format!("{gh_path} を起動できませんでした"))?;
    if !out.ok {
        return Err(format!("gh pr create に失敗しました: {}", out.why()));
    }
    parse_pr_url(&format!("{}\n{}", out.stdout, out.stderr))
        .ok_or_else(|| "gh pr create の出力から PR の URL を読めませんでした".to_string())
}

/// `gh pr view --json state,mergedAt,mergeable,reviewDecision,url` の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrView {
    pub state: String,
    pub merged_at: Option<String>,
    pub mergeable: Option<String>,
    pub review_decision: Option<String>,
    pub url: Option<String>,
}

/// PR の状態を見る（ADR-0043 D5: 画面を開いたときだけ。10 秒で諦める）。
pub fn gh_pr_view(gh_path: &str, repo: &Path, number: i64) -> Result<PrView, String> {
    let number = number.to_string();
    let out = gh(
        gh_path,
        repo,
        &[
            "pr",
            "view",
            &number,
            "--json",
            "state,mergedAt,mergeable,reviewDecision,url",
        ],
        GH_VIEW_TIMEOUT,
    )
    .ok_or_else(|| format!("{gh_path} を起動できませんでした"))?;
    if !out.ok {
        return Err(format!("gh pr view に失敗しました: {}", out.why()));
    }
    parse_pr_view(&out.stdout)
}

/// `gh pr view --json …` の JSON を読む（知らないキーは無視する）。
pub fn parse_pr_view(text: &str) -> Result<PrView, String> {
    let value: serde_json::Value = serde_json::from_str(text.trim())
        .map_err(|e| format!("gh pr view の JSON を読めませんでした: {e}"))?;
    let string = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Ok(PrView {
        state: string("state").unwrap_or_default(),
        merged_at: string("mergedAt"),
        mergeable: string("mergeable"),
        review_decision: string("reviewDecision"),
        url: string("url"),
    })
}

/// `gh pr merge <n> --<method> --delete-branch`（ADR-0043 D5。`method` は `[github] merge_method`）。
pub fn gh_pr_merge(gh_path: &str, repo: &Path, number: i64, method: &str) -> Result<(), String> {
    let number = number.to_string();
    let flag = format!("--{method}");
    let out = gh(
        gh_path,
        repo,
        &["pr", "merge", &number, &flag, "--delete-branch"],
        GH_WRITE_TIMEOUT,
    )
    .ok_or_else(|| format!("{gh_path} を起動できませんでした"))?;
    if !out.ok {
        return Err(format!("gh pr merge に失敗しました: {}", out.why()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 衝突の解消タスク（ADR-0043 D5）
// ---------------------------------------------------------------------------

/// ADR-0043 D5: rebase が衝突したときに自動で作る「衝突の解消: <題名>」タスク（純粋な組み立て。
/// 保存は呼び出し側）。親と同じ担当（`assignee` / `role` / `genre` / `tier` / 予算）で、
/// **親のブランチの上で**作業する。
///
/// **ADR-0043 D5 からの逸脱（Phase 54 の実装判断。PROGRESS の P54-2）**: 子に `repos` を持たせて
/// `task_repos` に親の worktree を使い回させる（`worktree_of` の目印）のではなく、
/// 子の `workspace` を **親の worktree そのもの + `mode = "shared"`**（ADR-0041 D1 の逃げ道）にする。
/// こうすると既存のディスパッチャがそのまま「そのディレクトリで走る」ので、ワーカーのプロトコルにも
/// `worktree.json` にも新しい概念を足さずに済む。子の cwd は親の `repos/<name>/`、ブランチは
/// 親の `celeris/<parent_id>` のまま。`repos` は**空**にする（空でないと子が自分の worktree を切ってしまう）。
///
/// 受け入れ条件は既存の `Check::Command` だけで書く:
/// 1. 作業ツリーが clean（`status --porcelain` が空）
/// 2. rebase が進行中でない（`rebase-merge` / `rebase-apply` が無い）
/// 3. rebase が済んでいる（`<default_branch>` が `HEAD` の祖先）
pub fn conflict_child_task(
    parent: &task_core::Task,
    repo: &str,
    worktree: &Path,
    default_branch: &str,
    conflicts: &[String],
    now: time::OffsetDateTime,
) -> task_core::Task {
    use task_core::{Check, Criterion, Status, TaskId, WorkspaceMode, WorkspaceSpec};

    let files = if conflicts.is_empty() {
        "（一覧が取れませんでした。作業ツリーの状態を見てください）".to_string()
    } else {
        conflicts
            .iter()
            .map(|f| format!("- `{f}`"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let objective = format!(
        "リポジトリ `{repo}` のブランチを `{default_branch}` に rebase しようとしたところ衝突しました。\n\
         この作業ツリー（`{dir}`。ブランチはこのまま）で rebase をやり直し、衝突を解消して\n\
         完了させてください。コードの意図は元のタスク「{title}」のとおりです。勝手に内容を変えないこと。\n\n\
         衝突したファイル:\n{files}\n\n\
         終わったら人が改めて「{default_branch} に取り込む」を押します。",
        dir = worktree.display(),
        title = parent.title,
    );
    let acceptance = vec![
        Criterion {
            text: "作業ツリーに未コミットの変更が無い".to_string(),
            check: Check::Command {
                cmd: "test -z \"$(git status --porcelain)\"".to_string(),
                expect_exit: 0,
            },
        },
        Criterion {
            text: "rebase が進行中でない".to_string(),
            check: Check::Command {
                cmd: "test ! -e \"$(git rev-parse --git-path rebase-merge)\" && \
                      test ! -e \"$(git rev-parse --git-path rebase-apply)\""
                    .to_string(),
                expect_exit: 0,
            },
        },
        Criterion {
            text: format!("{default_branch} の上に乗っている（rebase が完了している）"),
            check: Check::Command {
                cmd: format!("git merge-base --is-ancestor {default_branch} HEAD"),
                expect_exit: 0,
            },
        },
    ];

    // 親を写してから、子として要るところだけ書き換える（親に列が増えても写し漏れない）。
    let mut child = parent.clone();
    child.id = TaskId::new();
    child.parent_id = Some(parent.id);
    child.title = format!("衝突の解消: {}", parent.title);
    child.objective = objective;
    child.acceptance = acceptance;
    child.inputs = Vec::new();
    child.depends_on = Vec::new();
    child.status = Status::Ready;
    child.aggregate = false;
    child.attempts = 0;
    child.lease = None;
    child.conversation = None;
    child.created_at = now;
    child.updated_at = now;
    // 親の worktree の上で働く（自分の worktree を切らせない）。
    child.repos = Vec::new();
    child.workspace = WorkspaceSpec::Local {
        path: worktree.to_path_buf(),
        mode: Some(WorkspaceMode::Shared),
    };
    child
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_must(dir: &Path, args: &[&str]) {
        let out =
            git(dir, args, GIT_TIMEOUT).unwrap_or_else(|| panic!("git {args:?} did not start"));
        assert!(out.ok, "git {args:?}: {}", out.stderr);
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
        git_must(dir, &["init", "-q", "-b", "main"]);
        git_must(dir, &["config", "user.email", "t@example.com"]);
        git_must(dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("README.md"), b"hello\n").unwrap_or_else(|e| panic!("write: {e}"));
        git_must(dir, &["add", "-A"]);
        git_must(dir, &["commit", "-q", "-m", "first"]);
    }

    fn add_worktree(repo: &Path, dir: &Path, branch: &str) {
        let path = dir.to_string_lossy().into_owned();
        git_must(repo, &["worktree", "add", "-b", branch, &path, "main"]);
        git_must(dir, &["config", "user.email", "t@example.com"]);
        git_must(dir, &["config", "user.name", "t"]);
    }

    /// ADR-0043 D5: コミットとコミットしていない変更、追跡外のファイルが 1 つの一覧になる。
    #[test]
    fn the_changes_of_a_worktree_cover_commits_working_tree_and_untracked_files() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let repo = root.path().join("code");
        init_repo(&repo);
        let tree = root.path().join("ws/01TASK/repos/code");
        add_worktree(&repo, &tree, "celeris/01TASK");

        // 1 コミット + 未コミットの変更 + 追跡外のファイル。
        std::fs::write(tree.join("src.txt"), b"a\nb\nc\n").unwrap_or_else(|e| panic!("{e}"));
        git_must(&tree, &["add", "-A"]);
        git_must(&tree, &["commit", "-q", "-m", "add src"]);
        std::fs::write(tree.join("README.md"), b"hello\nmore\n").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(tree.join("new.txt"), b"x\ny\n").unwrap_or_else(|e| panic!("{e}"));

        let c = changes(&repo, Some(&tree), "celeris/01TASK", "main", None);
        assert!(!c.missing);
        assert_eq!(c.ahead, 1, "コミットは 1 つ");
        assert!(c.dirty, "未コミットの変更がある");
        let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["README.md", "new.txt", "src.txt"],
            "{:?}",
            c.files
        );
        let src = c
            .files
            .iter()
            .find(|f| f.path == "src.txt")
            .unwrap_or_else(|| panic!("src"));
        assert_eq!(
            (src.status.as_str(), src.additions, src.deletions),
            ("A", 3, 0)
        );
        let untracked = c
            .files
            .iter()
            .find(|f| f.path == "new.txt")
            .unwrap_or_else(|| panic!("new"));
        assert_eq!((untracked.status.as_str(), untracked.additions), ("?", 2));
        assert_eq!(
            c.stat,
            DiffStat {
                files: 3,
                additions: 6,
                deletions: 0
            }
        );
        assert_eq!(c.base.len(), 40, "base は完全な sha");

        // 1 ファイルの diff。
        let diff = file_diff(
            &repo,
            Some(&tree),
            "celeris/01TASK",
            "main",
            None,
            "src.txt",
        )
        .unwrap_or_else(|| panic!("diff"));
        assert!(diff.diff.contains("+a"), "{}", diff.diff);
        assert!(!diff.truncated);
        // 追跡外のファイルも `--no-index` で出る。
        let untracked_diff = file_diff(
            &repo,
            Some(&tree),
            "celeris/01TASK",
            "main",
            None,
            "new.txt",
        )
        .unwrap_or_else(|| panic!("diff"));
        assert!(
            untracked_diff.diff.contains("+x"),
            "{}",
            untracked_diff.diff
        );
    }

    /// worktree を消してもブランチが残っていれば、元のリポジトリから差分が引ける。無ければ `missing`。
    #[test]
    fn changes_fall_back_to_the_branch_and_then_report_missing() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let repo = root.path().join("code");
        init_repo(&repo);
        let tree = root.path().join("ws/01TASK/repos/code");
        add_worktree(&repo, &tree, "celeris/01TASK");
        std::fs::write(tree.join("src.txt"), b"a\n").unwrap_or_else(|e| panic!("{e}"));
        git_must(&tree, &["add", "-A"]);
        git_must(&tree, &["commit", "-q", "-m", "add src"]);

        let path = tree.to_string_lossy().into_owned();
        git_must(&repo, &["worktree", "remove", "--force", &path]);
        let c = changes(&repo, Some(&tree), "celeris/01TASK", "main", None);
        assert!(!c.missing, "ブランチは残っている");
        assert!(!c.dirty, "作業ツリーが無いので汚れようがない");
        assert_eq!(c.ahead, 1);
        assert_eq!(c.files.len(), 1);
        assert_eq!(c.files[0].path, "src.txt");
        assert!(
            file_diff(
                &repo,
                Some(&tree),
                "celeris/01TASK",
                "main",
                None,
                "src.txt"
            )
            .is_some_and(|d| d.diff.contains("+a"))
        );

        git_must(&repo, &["branch", "-D", "celeris/01TASK"]);
        let gone = changes(&repo, Some(&tree), "celeris/01TASK", "main", None);
        assert_eq!(
            gone,
            RepoChanges {
                missing: true,
                ..Default::default()
            }
        );
        assert_eq!(gone.ahead, 0);
        assert!(
            file_diff(
                &repo,
                Some(&tree),
                "celeris/01TASK",
                "main",
                None,
                "src.txt"
            )
            .is_none()
        );
    }

    /// コミットが 1 つも無いタスク（調査など）は `ahead = 0` で、ファイルも出ない。
    #[test]
    fn a_task_without_commits_is_zero_ahead() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let repo = root.path().join("code");
        init_repo(&repo);
        let tree = root.path().join("ws/01TASK/repos/code");
        add_worktree(&repo, &tree, "celeris/01TASK");
        let c = changes(&repo, Some(&tree), "celeris/01TASK", "main", None);
        assert_eq!(c.ahead, 0);
        assert!(c.files.is_empty());
        assert!(!c.dirty);
        assert!(!c.missing);
    }

    /// 200 KiB で切る（切ったら印が立つ）。
    #[test]
    fn a_huge_diff_is_truncated_at_200_kib() {
        let small = truncate_diff("@@\n+a\n", "a.txt");
        assert!(!small.truncated);
        let huge = "x".repeat(MAX_DIFF_BYTES + 10);
        let cut = truncate_diff(&huge, "a.txt");
        assert!(cut.truncated);
        assert_eq!(cut.diff.len(), MAX_DIFF_BYTES);
        // 文字の境界で切る（多バイト文字を壊さない）。
        let multibyte = "あ".repeat(MAX_DIFF_BYTES);
        let cut = truncate_diff(&multibyte, "a.txt");
        assert!(cut.truncated);
        assert!(cut.diff.len() <= MAX_DIFF_BYTES);
    }

    #[test]
    fn numstat_and_name_status_are_parsed() {
        assert_eq!(
            parse_numstat("3\t1\tsrc/a.rs\n-\t-\tlogo.png\nbroken\n"),
            vec![
                ("src/a.rs".to_string(), 3, 1, false),
                ("logo.png".to_string(), 0, 0, true)
            ]
        );
        assert_eq!(
            parse_name_status("M\tsrc/a.rs\nA\tnew.rs\nD\told.rs\n\n"),
            vec![
                ("src/a.rs".to_string(), "M".to_string()),
                ("new.rs".to_string(), "A".to_string()),
                ("old.rs".to_string(), "D".to_string()),
            ]
        );
    }

    #[test]
    fn the_pr_url_and_view_json_are_parsed() {
        assert_eq!(
            parse_pr_url("https://github.com/o/r/pull/12\n"),
            Some(PrRef {
                number: 12,
                url: "https://github.com/o/r/pull/12".into()
            })
        );
        assert_eq!(
            parse_pr_url("Creating pull request...\nhttps://github.com/o/r/pull/7."),
            Some(PrRef {
                number: 7,
                url: "https://github.com/o/r/pull/7".into()
            })
        );
        assert_eq!(parse_pr_url("no url here"), None);
        let view = parse_pr_view(
            r#"{"state":"MERGED","mergedAt":"2026-09-19T00:00:00Z","url":"u","mergeable":null}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(view.state, "MERGED");
        assert_eq!(view.merged_at.as_deref(), Some("2026-09-19T00:00:00Z"));
        assert_eq!(view.mergeable, None);
        assert!(parse_pr_view("not json").is_err());
    }

    /// ADR-0043 D5 の `merge`: 人のチェックアウトが `main` を出していて綺麗なら fast-forward する。
    #[test]
    fn merge_fast_forwards_the_default_branch_when_it_is_checked_out_and_clean() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let repo = root.path().join("code");
        init_repo(&repo);
        let tree = root.path().join("ws/01TASK/repos/code");
        add_worktree(&repo, &tree, "celeris/01TASK");
        std::fs::write(tree.join("src.txt"), b"a\n").unwrap_or_else(|e| panic!("{e}"));
        git_must(&tree, &["add", "-A"]);
        git_must(&tree, &["commit", "-q", "-m", "add src"]);

        let outcome = merge_into_default_branch(
            &repo,
            "celeris/01TASK",
            "main",
            &root.path().join("tmp/one"),
        );
        let sha = match outcome {
            MergeOutcome::Merged {
                sha,
                fast_forwarded,
            } => {
                assert!(fast_forwarded, "作業ツリーごと早送りする");
                sha
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(
            git_line(&repo, &["rev-parse", "refs/heads/main"]).as_deref(),
            Some(sha.as_str())
        );
        assert!(
            repo.join("src.txt").is_file(),
            "人の作業ツリーにも反映される"
        );
        assert!(
            !root.path().join("tmp/one").exists(),
            "一時 worktree は片付ける"
        );

        remove_worktree_and_branch(&repo, Some(&tree), "celeris/01TASK")
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!tree.exists());
        assert!(!git_ok(
            &repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/celeris/01TASK"
            ]
        ));
        // 2 回目も落ちない。
        remove_worktree_and_branch(&repo, Some(&tree), "celeris/01TASK")
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// 人が別のブランチを出していれば ref だけ動かす（作業ツリーには触らない）。
    #[test]
    fn merge_updates_the_ref_when_another_branch_is_checked_out() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let repo = root.path().join("code");
        init_repo(&repo);
        let tree = root.path().join("ws/01TASK/repos/code");
        add_worktree(&repo, &tree, "celeris/01TASK");
        std::fs::write(tree.join("src.txt"), b"a\n").unwrap_or_else(|e| panic!("{e}"));
        git_must(&tree, &["add", "-A"]);
        git_must(&tree, &["commit", "-q", "-m", "add src"]);
        // 人は別のブランチで作業していて、しかも汚れている（それでも `main` は動かせる）。
        git_must(&repo, &["checkout", "-q", "-b", "wip"]);
        std::fs::write(repo.join("scratch.txt"), b"mine\n").unwrap_or_else(|e| panic!("{e}"));

        let before = git_line(&repo, &["rev-parse", "refs/heads/main"]).unwrap_or_default();
        let outcome = merge_into_default_branch(
            &repo,
            "celeris/01TASK",
            "main",
            &root.path().join("tmp/two"),
        );
        match outcome {
            MergeOutcome::Merged {
                sha,
                fast_forwarded,
            } => {
                assert!(!fast_forwarded, "ref だけ動かす");
                assert_ne!(sha, before);
                assert_eq!(
                    git_line(&repo, &["rev-parse", "refs/heads/main"]).as_deref(),
                    Some(sha.as_str())
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            current_branch(&repo).as_deref(),
            Some("wip"),
            "人のチェックアウトは動かさない"
        );
        assert!(!repo.join("src.txt").exists(), "人の作業ツリーには触らない");
        assert!(repo.join("scratch.txt").is_file());
    }

    /// 人が `main` を編集中なら 409（何も触らない）。
    #[test]
    fn merge_refuses_while_the_default_branch_is_being_edited() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let repo = root.path().join("code");
        init_repo(&repo);
        let tree = root.path().join("ws/01TASK/repos/code");
        add_worktree(&repo, &tree, "celeris/01TASK");
        std::fs::write(tree.join("src.txt"), b"a\n").unwrap_or_else(|e| panic!("{e}"));
        git_must(&tree, &["add", "-A"]);
        git_must(&tree, &["commit", "-q", "-m", "add src"]);
        std::fs::write(repo.join("README.md"), b"human is editing\n")
            .unwrap_or_else(|e| panic!("{e}"));

        let before = git_line(&repo, &["rev-parse", "refs/heads/main"]).unwrap_or_default();
        match merge_into_default_branch(
            &repo,
            "celeris/01TASK",
            "main",
            &root.path().join("tmp/three"),
        ) {
            MergeOutcome::Busy { detail } => assert_eq!(detail, "main が編集中"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            git_line(&repo, &["rev-parse", "refs/heads/main"]).as_deref(),
            Some(before.as_str())
        );
        assert!(tree.join("src.txt").is_file(), "worktree は残る");
    }

    /// rebase が衝突したら `--abort` して衝突したファイルを返す（worktree もブランチも残す）。
    #[test]
    fn a_conflicting_rebase_is_aborted_and_reports_the_files() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let repo = root.path().join("code");
        init_repo(&repo);
        let tree = root.path().join("ws/01TASK/repos/code");
        add_worktree(&repo, &tree, "celeris/01TASK");
        std::fs::write(tree.join("README.md"), b"task side\n").unwrap_or_else(|e| panic!("{e}"));
        git_must(&tree, &["add", "-A"]);
        git_must(&tree, &["commit", "-q", "-m", "task"]);
        // `main` 側も同じ行を動かす。
        std::fs::write(repo.join("README.md"), b"human side\n").unwrap_or_else(|e| panic!("{e}"));
        git_must(&repo, &["add", "-A"]);
        git_must(&repo, &["commit", "-q", "-m", "human"]);

        match merge_into_default_branch(
            &repo,
            "celeris/01TASK",
            "main",
            &root.path().join("tmp/four"),
        ) {
            MergeOutcome::Conflict { files } => assert_eq!(files, vec!["README.md".to_string()]),
            other => panic!("{other:?}"),
        }
        assert!(tree.join("README.md").is_file(), "worktree はそのまま");
        assert!(git_ok(
            &repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/celeris/01TASK"
            ]
        ));
        assert!(
            !root.path().join("tmp/four").exists(),
            "一時 worktree は片付ける"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("README.md")).unwrap_or_default(),
            "human side\n",
            "人の作業ツリーには触らない"
        );
    }

    /// `discard` は worktree もブランチも消す。
    #[test]
    fn discard_removes_the_worktree_and_the_branch() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let repo = root.path().join("code");
        init_repo(&repo);
        let tree = root.path().join("ws/01TASK/repos/code");
        add_worktree(&repo, &tree, "celeris/01TASK");
        std::fs::write(tree.join("wip.txt"), b"x\n").unwrap_or_else(|e| panic!("{e}"));

        discard(&repo, Some(&tree), "celeris/01TASK").unwrap_or_else(|e| panic!("{e}"));
        assert!(!tree.exists());
        assert!(!git_ok(
            &repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/celeris/01TASK"
            ]
        ));
        assert!(repo.join("README.md").is_file(), "元のリポジトリは無事");
    }

    /// `default_branch` は設定 → `origin/HEAD` → `main` → `master` の順。
    #[test]
    fn the_default_branch_is_configured_then_detected() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let repo = root.path().join("code");
        init_repo(&repo);
        assert_eq!(default_branch(&repo, Some("trunk")), "trunk");
        assert_eq!(default_branch(&repo, Some("  ")), "main");
        assert_eq!(default_branch(&repo, None), "main");
        git_must(&repo, &["branch", "-m", "main", "master"]);
        assert_eq!(default_branch(&repo, None), "master");
        assert!(!has_origin(&repo));
    }

    /// 子プロセスの待ち時間: 終わらないコマンドは殺して `timed_out` を立てる。
    #[test]
    fn a_command_that_never_finishes_is_killed() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        let out = run(cmd, Duration::from_millis(200)).unwrap_or_else(|| panic!("spawn"));
        assert!(out.timed_out);
        assert!(!out.ok);
        assert_eq!(out.why(), "コマンドが時間内に終わりませんでした");
    }
}
