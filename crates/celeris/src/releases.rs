//! ADR-0040 D6（Phase 48）: `[selfdeploy] releases_dir` を読む／昇格を起こす。
//!
//! ここにあるのは**ディレクトリに対する純粋な関数**だけで、判断（どれを昇格するか）は人が GUI か
//! shell で行う（ADR-0040 D5）。LLM もワーカーも関与しない。
//!
//! 読むもの（`release.sh` / `verify.sh` / `promote.sh` が書いたもの。`docs/selfdeploy.md`）:
//!
//! ```text
//! <releases_dir>/<sha12>/manifest.json   {sha, sha12, ref, built_at, schema_version, ...}
//! <releases_dir>/<sha12>/gate.json       {ok, failed_step, steps: [...]}
//! <releases_dir>/<sha12>/verify.json     {ok, live_ok, at, checks: [...]}   ← 無ければ未検証
//! <releases_dir>/<sha12>/changes.json    {base, commits: [...], files: [...], sensitive: [...]}（ADR-0041 D4）
//! <releases_dir>/<sha12>/promoted.json   {promoted_at, mode, from}（ADR-0041 D3。昇格に成功したときだけ）
//! <releases_dir>/<sha12>/scripts/*.sh    release.sh が同梱した selfdeploy 一式
//! <releases_dir>/<sha12>/promote.lock    昇格中の pid（この模組が書く）
//! <releases_dir>/<sha12>/promote.log     promote.sh の出力
//! <releases_dir>/../current -> releases/<sha12>
//! <releases_dir>/../previous -> releases/<sha12>
//! ```
//!
//! 壊れた JSON・途中で消えたディレクトリでは**落ちない**（その 1 件が `gate_ok = false` と
//! `problem` を持つだけ）。`.build` / `.cargo-target` のような `.` で始まる名前と `*.partial` は飛ばす。
//!
//! ADR-0041 D3 で `[selfdeploy] repo`（人の作業チェックアウト）を**読むだけ**使うようになった:
//! `git -C <repo> merge-base --is-ancestor <sha> main` で `on_main` を出す。git が無い・遅い・
//! リポジトリが無い・その sha を知らない、のどれでも `null` を出すだけで、一覧は落とさない
//! （**celeris がリポジトリを書き換えることは無い**。`main` への反映は人がやる）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use task_api::types::{ReleaseChanges, ReleaseCommit, ReleaseItem, ReleasePromoteAccepted, ReleaseVerify};
use task_api::{ReleasePromoteError, ReleaseSource, ReleasesFs};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::instance::pid_alive;

/// `git` を待つ上限。一覧の要求の中で走るので、詰まったら諦めて `null` を出す（ADR-0041 D3）。
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// `<releases_dir>` を読む `ReleaseSource`（celeris が `ApiSettings` に渡す）。
#[derive(Debug, Clone)]
pub struct FsReleases {
    root: PathBuf,
    /// `[selfdeploy] repo`（作業チェックアウト）。`on_main` を出すためだけに読む。
    repo: PathBuf,
}

impl FsReleases {
    pub fn new(root: PathBuf, repo: PathBuf) -> Self {
        Self { root, repo }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn repo(&self) -> &Path {
        &self.repo
    }
}

impl ReleaseSource for FsReleases {
    fn list(&self) -> ReleasesFs {
        scan(&self.root, Some(&self.repo))
    }

    fn promote(&self, sha12: &str) -> Result<ReleasePromoteAccepted, ReleasePromoteError> {
        start_promote(&self.root, sha12)
    }

    /// ADR-0044 D5（Phase 53）: タスクのブランチにだけ載っているコミットの sha。
    /// `rev-list --max-count=<N> [<base>..]<branch>` を上限つきで走らせるだけ（読むだけ。
    /// 壊れていても空を返す）。
    fn branch_commits(&self, repo: &Path, branch: &str, base: Option<&str>) -> Vec<String> {
        branch_commits(repo, branch, base)
    }
}

/// ADR-0044 D5: `branch`（`base` があれば `base..branch`）のコミットの sha を新しい順に返す。
/// git が無い・リポジトリが無い・ブランチが無い・時間切れなら空。
fn branch_commits(repo: &Path, branch: &str, base: Option<&str>) -> Vec<String> {
    // ブランチ名・sha に変な文字が混じっていたら走らせない。
    let safe = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/_-.".contains(c));
    if !repo.is_dir() || !safe(branch) {
        return Vec::new();
    }
    let range = match base.filter(|b| safe(b)) {
        Some(base) => format!("{base}..{branch}"),
        None => branch.to_string(),
    };
    let limit = format!("--max-count={}", task_api::BRANCH_COMMITS_LIMIT);
    let Some(out) = git_output(repo, &["rev-list", &limit, &range]) else {
        return Vec::new();
    };
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// `git_status` と同じ流儀で stdout を取る（失敗・時間切れ・非 0 終了は `None`）。
fn git_output(repo: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + GIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut buf = String::new();
    use std::io::Read;
    child.stdout.as_mut()?.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// ディレクトリ名として安全で、`git rev-parse --short=12` が出す形か（パストラバーサル防止）。
/// 長さは 7〜40（`--short` の幅を変えても通るように）で、16 進数字だけ。
pub fn valid_sha12(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// `link` が指す先の basename（symlink でなければ `None`）。`current` / `previous` 用。
fn link_target_name(link: &Path) -> Option<String> {
    let dest = std::fs::read_link(link).ok()?;
    let name = dest.file_name()?.to_str()?.to_string();
    (!name.is_empty()).then_some(name)
}

/// `<releases_dir>` を読んで一覧を作る（副作用は読み取りだけ。`repo` は `on_main` のためだけに使う）。
pub fn scan(root: &Path, repo: Option<&Path>) -> ReleasesFs {
    // `current` / `previous` は `releases_dir` の**親**にある（`~/.local/celeris/current -> releases/<sha12>`）。
    let home = root.parent();
    let current = home.and_then(|h| link_target_name(&h.join("current")));
    let previous = home.and_then(|h| link_target_name(&h.join("previous")));

    // ADR-0041 D3: `main` が引けるリポジトリのときだけ `on_main` を出す。1 回で見切りをつけて、
    // リリースごとに `git` を起こす無駄（と、リポジトリが無いときの毎回の失敗）を避ける。
    let git_repo = repo.filter(|r| git_has_main(r));

    let mut items = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        // `releases_dir` がまだ無い（初回）。空の一覧を返す — エラーにはしない。
        return ReleasesFs {
            current,
            previous,
            items,
        };
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // `.build` / `.cargo-target` と、`release.sh` が組み立て中に使う `<sha12>.partial` は飛ばす。
        if name.starts_with('.') || name.ends_with(".partial") {
            continue;
        }
        if !entry.path().is_dir() {
            continue;
        }
        items.push(read_release(
            &entry.path(),
            &name,
            current.as_deref(),
            previous.as_deref(),
            git_repo,
        ));
    }
    // 新しい順。`built_at` が読めなかったものは最後（同値は sha12 昇順で安定させる）。
    items.sort_by(|a, b| {
        b.built_at
            .cmp(&a.built_at)
            .then_with(|| a.sha12.cmp(&b.sha12))
    });
    ReleasesFs {
        current,
        previous,
        items,
    }
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// `git -C <repo> <args...>` を**上限つき**で走らせる。終了コードを返す（起こせない・時間切れ・
/// シグナルで死んだ、のどれでも `None`）。`GET /releases` の中で走るので、詰まったら諦める。
fn git_status(repo: &Path, args: &[&str]) -> Option<i32> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + GIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// そのディレクトリが git リポジトリで、`main` という commit を持っているか。
fn git_has_main(repo: &Path) -> bool {
    repo.is_dir() && git_status(repo, &["rev-parse", "--verify", "--quiet", "main^{commit}"]) == Some(0)
}

/// ADR-0041 D3: `<sha>` が `main` の祖先か。分からなければ `None`（一覧は落とさない）。
fn on_main(repo: &Path, sha: &str) -> Option<bool> {
    match git_status(repo, &["merge-base", "--is-ancestor", sha, "main"]) {
        Some(0) => Some(true),
        Some(1) => Some(false),
        // 128 = その sha をこのリポジトリが知らない（別のチェックアウトでビルドした等）。
        _ => None,
    }
}

/// ADR-0041 D4: `changes.json` を `ReleaseChanges` に写す。`stale` はここで決める
/// （`base` が**いまの** `current` と違えば、この一覧は「いま昇格したら何が変わるか」ではない）。
fn read_changes(dir: &Path, current: Option<&str>) -> Option<ReleaseChanges> {
    let raw = read_json(&dir.join("changes.json"))?;
    let base = raw.get("base").and_then(serde_json::Value::as_str).map(str::to_string);
    let files = raw.get("files").and_then(serde_json::Value::as_array);
    let sensitive = raw
        .get("sensitive")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let commits = raw
        .get("commits")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    Some(ReleaseCommit {
                        sha: c.get("sha")?.as_str()?.to_string(),
                        subject: c
                            .get("subject")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(ReleaseChanges {
        stale: base.as_deref() != current,
        base,
        commit_count: commits.len(),
        file_count: files.map(Vec::len).unwrap_or(0),
        sensitive,
        commits,
    })
}

fn read_release(
    dir: &Path,
    sha12: &str,
    current: Option<&str>,
    previous: Option<&str>,
    repo: Option<&Path>,
) -> ReleaseItem {
    let manifest = read_json(&dir.join("manifest.json"));
    let gate = read_json(&dir.join("gate.json"));
    let verify = read_json(&dir.join("verify.json"));
    let promoted = read_json(&dir.join("promoted.json"));

    let mut problems: Vec<&str> = Vec::new();
    if manifest.is_none() {
        problems.push("manifest.json is missing or invalid");
    }
    if gate.is_none() {
        problems.push("gate.json is missing or invalid");
    }

    let field = |v: &Option<serde_json::Value>, key: &str| -> Option<serde_json::Value> {
        v.as_ref().and_then(|m| m.get(key)).cloned()
    };
    let as_string = |v: Option<serde_json::Value>| -> Option<String> {
        v.and_then(|v| v.as_str().map(str::to_string))
    };

    // git に渡すのは完全な sha（`manifest.json`）を優先する。読めなければディレクトリ名（sha12）。
    let full_sha = as_string(field(&manifest, "sha")).unwrap_or_else(|| sha12.to_string());

    ReleaseItem {
        sha12: sha12.to_string(),
        r#ref: as_string(field(&manifest, "ref")),
        built_at: as_string(field(&manifest, "built_at")),
        schema_version: field(&manifest, "schema_version")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok()),
        gate_ok: field(&gate, "ok").and_then(|v| v.as_bool()).unwrap_or(false),
        verify: verify.as_ref().map(|v| ReleaseVerify {
            ok: v.get("ok").and_then(serde_json::Value::as_bool).unwrap_or(false),
            live_ok: v.get("live_ok").and_then(serde_json::Value::as_bool).unwrap_or(false),
            at: v.get("at").and_then(serde_json::Value::as_str).map(str::to_string),
        }),
        promoted_at: as_string(field(&promoted, "promoted_at")),
        on_main: repo.and_then(|r| on_main(r, &full_sha)),
        changes: read_changes(dir, current),
        is_current: current == Some(sha12),
        is_previous: previous == Some(sha12),
        promoting: promoting_pid(dir).is_some(),
        problem: (!problems.is_empty()).then(|| problems.join("; ")),
    }
}

/// `promote.lock` に書かれた pid が**まだ生きていれば** `Some(pid)`。消えていれば `None`
/// （残骸のロックは昇格を塞がない。ADR-0040 D6）。
fn promoting_pid(dir: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(dir.join("promote.lock")).ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    pid_alive(pid).then_some(pid)
}

/// `sh -c` に渡す 1 語を単引用符で囲む。
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `promote.sh <sha12>` を detached（`setsid`、stdin は `/dev/null`、stdout/err は
/// `<release>/promote.log`）で起こし、`promote.lock` に pid を書く。どの `promote.sh` かは
/// ADR-0041 D4: **`<current>/scripts/promote.sh`**（無ければ昇格先のもの）。
///
/// **この関数を自動で呼ぶ経路は作らない**（ADR-0040 D5: 昇格は人が押す）。呼ぶのは
/// `POST /releases/{sha12}/promote` だけで、そこは管理系（トークン必須）。
///
/// 昇格は**この celeris 自身を drain させうる**（ADR-0040 D4）。だから `promote.sh` は celeris の
/// 子プロセスとして待たず、`setsid` で新しいセッションに切り離して孤児にする（親が消えても走り続ける）。
pub fn start_promote(root: &Path, sha12: &str) -> Result<ReleasePromoteAccepted, ReleasePromoteError> {
    if !valid_sha12(sha12) {
        return Err(ReleasePromoteError::NotFound);
    }
    let dir = root.join(sha12);
    if !dir.is_dir() {
        return Err(ReleasePromoteError::NotFound);
    }

    // 1. 検証済みか（`promote.sh` も同じ判定をするが、409 を早く・はっきり返すためここでも見る）。
    let verify = read_json(&dir.join("verify.json"));
    let verified = verify
        .as_ref()
        .and_then(|v| v.get("ok"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !verified {
        return Err(ReleasePromoteError::NotVerified(match verify {
            None => format!("{sha12} has no verify.json — run scripts/selfdeploy/verify.sh first"),
            Some(_) => format!("verify.json of {sha12} is not ok — there is no --force (ADR-0040 D2)"),
        }));
    }

    // 2. 既に current か。
    let current = root.parent().and_then(|h| link_target_name(&h.join("current")));
    if current.as_deref() == Some(sha12) {
        return Err(ReleasePromoteError::AlreadyCurrent);
    }

    // 3. 既に昇格中か。
    if promoting_pid(&dir).is_some() {
        return Err(ReleasePromoteError::AlreadyPromoting);
    }

    // 4. どちらの `promote.sh` で昇格するか（ADR-0041 D4）。
    //
    //    **いま動いている版（`current`）のスクリプト**を使う。昇格は「動いている本番を止めて／
    //    引き継いで新しい版に替える」作業で、その手順を知っているべきなのは**いまの本番**の方だから。
    //    実装者が `scripts/selfdeploy/` を壊したリリースを作っても、その壊れた昇格スクリプトが
    //    走ることは無い（新しい昇格スクリプトは、それ自身が一度昇格されてから次の昇格で使われる）。
    //    `current` に `scripts/` が無い（Phase 48 以前のリリース、または初回）ときだけ、
    //    昇格先に同梱された方を使う。どちらを使ったかは応答の `script_from` に出す。
    let target_script = dir.join("scripts").join("promote.sh");
    let current_script = current
        .as_deref()
        .map(|c| root.join(c).join("scripts").join("promote.sh"))
        .filter(|p| p.is_file());
    let (script, script_from) = match current_script {
        Some(script) => (script, "current"),
        None if target_script.is_file() => (target_script, "target"),
        None => {
            return Err(ReleasePromoteError::Unavailable(format!(
                "{} is missing — this release was built before the selfdeploy scripts were bundled, \
                 and the current release does not carry them either",
                target_script.display()
            )));
        }
    };

    let log = dir.join("promote.log");
    let lock = dir.join("promote.lock");
    // `setsid` で新しいセッションに切り離し、pid を lock に書いてから `sh` は抜ける。
    // `$!` は `setsid` の pid で、`setsid` は（自分がプロセスグループの長でないので）その場で
    // exec する＝そのまま `promote.sh` の pid になる。
    let command = format!(
        "setsid {script} {sha} </dev/null >>{log} 2>&1 & printf '%s\\n' \"$!\" >{lock}",
        script = sh_quote(&script.to_string_lossy()),
        sha = sh_quote(sha12),
        log = sh_quote(&log.to_string_lossy()),
        lock = sh_quote(&lock.to_string_lossy()),
    );
    let status = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| ReleasePromoteError::Unavailable(format!("cannot start promote.sh: {e}")))?;
    if !status.success() {
        return Err(ReleasePromoteError::Unavailable(format!(
            "promote.sh could not be started (sh exited with {status})"
        )));
    }

    Ok(ReleasePromoteAccepted {
        sha12: sha12.to_string(),
        log: log.to_string_lossy().into_owned(),
        started_at: OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::new()),
        script_from: script_from.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<root>/<sha>/` を作り、渡した JSON を置く（`None` はファイルを作らない）。
    fn release(root: &Path, sha: &str, manifest: Option<&str>, gate: Option<&str>, verify: Option<&str>) {
        let dir = root.join(sha);
        std::fs::create_dir_all(&dir).expect("mkdir");
        for (name, body) in [("manifest.json", manifest), ("gate.json", gate), ("verify.json", verify)] {
            if let Some(body) = body {
                std::fs::write(dir.join(name), body).expect("write");
            }
        }
    }

    /// 実行ビット付きの偽 `scripts/promote.sh`（1 行書いて眠るだけ。本物の昇格は起きない）。
    fn fake_script(root: &Path, sha: &str, marker: &str) {
        use std::os::unix::fs::PermissionsExt;
        let scripts = root.join(sha).join("scripts");
        std::fs::create_dir_all(&scripts).expect("mkdir");
        let script = scripts.join("promote.sh");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '{marker} %s\\n' \"$1\"\nsleep 2\n")).expect("write");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// tempdir に `main` を持つ git リポジトリを作り、(main の sha, main に居ない sha) を返す。
    /// git が無い環境では `None`（テストは飛ばす）。
    fn git_repo(dir: &Path) -> Option<(String, String)> {
        let git = |args: &[&str]| -> Option<String> {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.email=t@example.invalid", "-c", "user.name=t"])
                .args(args)
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        std::fs::create_dir_all(dir).expect("mkdir");
        git(&["init", "-q", "-b", "main"])?;
        git(&["commit", "-q", "--allow-empty", "-m", "on main"])?;
        let on_main_sha = git(&["rev-parse", "HEAD"])?;
        git(&["checkout", "-q", "-b", "side"])?;
        git(&["commit", "-q", "--allow-empty", "-m", "not on main"])?;
        let off_main_sha = git(&["rev-parse", "HEAD"])?;
        git(&["checkout", "-q", "main"])?;
        Some((on_main_sha, off_main_sha))
    }

    fn env() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("releases");
        std::fs::create_dir_all(&root).expect("mkdir");
        (dir, root)
    }

    #[test]
    fn scanning_an_empty_or_missing_directory_yields_nothing_and_does_not_fail() {
        let (dir, root) = env();
        let scanned = scan(&root, None);
        assert!(scanned.items.is_empty());
        assert_eq!(scanned.current, None);
        assert_eq!(scanned.previous, None);
        // ディレクトリごと無いとき（初回）も同じ。
        let missing = dir.path().join("nope");
        assert!(scan(&missing, None).items.is_empty());
    }

    #[test]
    fn two_releases_are_sorted_newest_first_with_the_symlinks_applied() {
        let (dir, root) = env();
        release(
            &root,
            "aaaaaaaaaaaa",
            Some(r#"{"ref":"main","built_at":"2026-09-18T00:00:00Z","schema_version":10}"#),
            Some(r#"{"ok":true}"#),
            Some(r#"{"ok":true,"live_ok":false,"at":"2026-09-18T01:00:00Z"}"#),
        );
        release(
            &root,
            "bbbbbbbbbbbb",
            Some(r#"{"ref":"self/01M","built_at":"2026-09-19T00:00:00Z","schema_version":11}"#),
            Some(r#"{"ok":true}"#),
            None,
        );
        // `~/.local/celeris/current -> releases/aaaaaaaaaaaa`（releases_dir の親に張る）。
        std::os::unix::fs::symlink("releases/aaaaaaaaaaaa", dir.path().join("current")).expect("symlink");
        std::os::unix::fs::symlink("releases/bbbbbbbbbbbb", dir.path().join("previous")).expect("symlink");

        let scanned = scan(&root, None);
        assert_eq!(scanned.current.as_deref(), Some("aaaaaaaaaaaa"));
        assert_eq!(scanned.previous.as_deref(), Some("bbbbbbbbbbbb"));
        assert_eq!(
            scanned.items.iter().map(|i| i.sha12.as_str()).collect::<Vec<_>>(),
            ["bbbbbbbbbbbb", "aaaaaaaaaaaa"],
            "built_at の新しい順"
        );

        let newest = &scanned.items[0];
        assert_eq!(newest.r#ref.as_deref(), Some("self/01M"));
        assert_eq!(newest.schema_version, Some(11));
        assert!(newest.gate_ok);
        assert!(newest.verify.is_none(), "verify.json が無ければ未検証");
        assert!(newest.is_previous && !newest.is_current);
        assert!(!newest.promoting);
        assert_eq!(newest.problem, None);

        let older = &scanned.items[1];
        let verify = older.verify.as_ref().expect("verify");
        assert!(verify.ok && !verify.live_ok);
        assert_eq!(verify.at.as_deref(), Some("2026-09-18T01:00:00Z"));
        assert!(older.is_current);
    }

    #[test]
    fn broken_json_and_build_directories_do_not_break_the_scan() {
        let (_dir, root) = env();
        release(&root, "cccccccccccc", Some("{ this is not json"), None, None);
        // `.build` / `.cargo-target` / `<sha>.partial` は一覧に出ない。
        std::fs::create_dir_all(root.join(".build").join("dddddddddddd")).expect("mkdir");
        std::fs::create_dir_all(root.join(".cargo-target")).expect("mkdir");
        std::fs::create_dir_all(root.join("eeeeeeeeeeee.partial")).expect("mkdir");
        std::fs::write(root.join("stray.txt"), "x").expect("write");

        let scanned = scan(&root, None);
        assert_eq!(scanned.items.len(), 1, "{:?}", scanned.items);
        let item = &scanned.items[0];
        assert_eq!(item.sha12, "cccccccccccc");
        assert!(!item.gate_ok);
        assert_eq!(item.built_at, None);
        let problem = item.problem.as_deref().expect("problem");
        assert!(problem.contains("manifest.json"), "{problem}");
        assert!(problem.contains("gate.json"), "{problem}");
    }

    #[test]
    fn sha12_validation_rejects_path_traversal() {
        assert!(valid_sha12("aaaaaaaaaaaa"));
        assert!(valid_sha12("0123456789ab"));
        assert!(!valid_sha12(""));
        assert!(!valid_sha12("../escape"));
        assert!(!valid_sha12("aaaa"));
        assert!(!valid_sha12("zzzzzzzzzzzz"));
        assert!(!valid_sha12(&"a".repeat(41)));
    }

    #[test]
    fn promotion_is_refused_for_unknown_unverified_current_and_running_releases() {
        let (dir, root) = env();
        // 知らない sha / 形が違う sha。
        assert_eq!(start_promote(&root, "aaaaaaaaaaaa"), Err(ReleasePromoteError::NotFound));
        assert_eq!(start_promote(&root, "../etc"), Err(ReleasePromoteError::NotFound));

        // verify.json が無い。
        release(&root, "aaaaaaaaaaaa", Some(r#"{"built_at":"2026-09-18T00:00:00Z"}"#), Some(r#"{"ok":true}"#), None);
        assert!(matches!(
            start_promote(&root, "aaaaaaaaaaaa"),
            Err(ReleasePromoteError::NotVerified(_))
        ));
        // verify.json は在るが ok ではない。
        std::fs::write(root.join("aaaaaaaaaaaa").join("verify.json"), r#"{"ok":false,"live_ok":false}"#)
            .expect("write");
        assert!(matches!(
            start_promote(&root, "aaaaaaaaaaaa"),
            Err(ReleasePromoteError::NotVerified(_))
        ));

        // 検証済みだが既に current。
        std::fs::write(root.join("aaaaaaaaaaaa").join("verify.json"), r#"{"ok":true,"live_ok":true}"#)
            .expect("write");
        std::os::unix::fs::symlink("releases/aaaaaaaaaaaa", dir.path().join("current")).expect("symlink");
        assert_eq!(start_promote(&root, "aaaaaaaaaaaa"), Err(ReleasePromoteError::AlreadyCurrent));

        // current ではないが `scripts/promote.sh` が無い（Phase 48 より前に作られたリリース）。
        release(&root, "bbbbbbbbbbbb", Some(r#"{"built_at":"2026-09-19T00:00:00Z"}"#), Some(r#"{"ok":true}"#),
            Some(r#"{"ok":true,"live_ok":true}"#));
        assert!(matches!(
            start_promote(&root, "bbbbbbbbbbbb"),
            Err(ReleasePromoteError::Unavailable(_))
        ));

        // 昇格中（生きている pid の promote.lock）。自分自身の pid を使う。
        let scripts = root.join("bbbbbbbbbbbb").join("scripts");
        std::fs::create_dir_all(&scripts).expect("mkdir");
        std::fs::write(scripts.join("promote.sh"), "#!/bin/sh\nexit 0\n").expect("write");
        std::fs::write(root.join("bbbbbbbbbbbb").join("promote.lock"), format!("{}\n", std::process::id()))
            .expect("write");
        assert_eq!(start_promote(&root, "bbbbbbbbbbbb"), Err(ReleasePromoteError::AlreadyPromoting));
    }

    /// 偽の `scripts/promote.sh`（ログに 1 行書いて少し眠る）を detached で起こす。
    /// **本物の昇格は起きない**（本番のパスにもプロセスにも触れない）。
    #[test]
    fn promoting_spawns_the_bundled_script_detached_and_writes_the_lock() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, root) = env();
        release(&root, "abcdef123456", Some(r#"{"built_at":"2026-09-19T00:00:00Z"}"#), Some(r#"{"ok":true}"#),
            Some(r#"{"ok":true,"live_ok":true}"#));
        let scripts = root.join("abcdef123456").join("scripts");
        std::fs::create_dir_all(&scripts).expect("mkdir");
        let script = scripts.join("promote.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf 'promote %s\\n' \"$1\"\nsleep 2\n").expect("write");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let accepted = start_promote(&root, "abcdef123456").expect("202");
        assert_eq!(accepted.sha12, "abcdef123456");
        assert!(accepted.log.ends_with("abcdef123456/promote.log"), "{}", accepted.log);
        assert!(accepted.started_at.contains('T'));

        // lock に pid が書かれ、ログに 1 行出る（少し待つ）。
        let lock = root.join("abcdef123456").join("promote.lock");
        let log = root.join("abcdef123456").join("promote.log");
        let mut logged = String::new();
        for _ in 0..100 {
            logged = std::fs::read_to_string(&log).unwrap_or_default();
            if logged.contains("promote abcdef123456") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(logged.contains("promote abcdef123456"), "{logged:?}");
        let pid: u32 = std::fs::read_to_string(&lock)
            .expect("lock")
            .trim()
            .parse()
            .expect("pid");
        assert!(pid > 0);

        // 走っている間は 409（二重に起こさない）。
        assert_eq!(start_promote(&root, "abcdef123456"), Err(ReleasePromoteError::AlreadyPromoting));
        // 一覧にも `promoting = true` で出る。
        let scanned = scan(&root, None);
        assert!(scanned.items.iter().any(|i| i.sha12 == "abcdef123456" && i.promoting));
    }

    /// ADR-0041 D4: 昇格に使うのは **`current` に同梱された** `promote.sh`。
    /// `current` がそれを持っていないとき（Phase 48 以前・初回）だけ昇格先のものを使う。
    #[test]
    fn promotion_runs_the_promote_script_of_the_current_release() {
        let (dir, root) = env();
        release(&root, "aaaaaaaaaaaa", Some(r#"{"built_at":"2026-09-18T00:00:00Z"}"#), Some(r#"{"ok":true}"#),
            Some(r#"{"ok":true,"live_ok":true}"#));
        release(&root, "bbbbbbbbbbbb", Some(r#"{"built_at":"2026-09-19T00:00:00Z"}"#), Some(r#"{"ok":true}"#),
            Some(r#"{"ok":true,"live_ok":true}"#));
        std::os::unix::fs::symlink("releases/aaaaaaaaaaaa", dir.path().join("current")).expect("symlink");

        // (1) current にも昇格先にも `scripts/` が無い → 409。
        assert!(matches!(
            start_promote(&root, "bbbbbbbbbbbb"),
            Err(ReleasePromoteError::Unavailable(_))
        ));

        // (2) 昇格先にだけある（current は Phase 48 以前）→ 昇格先のものを使う。
        fake_script(&root, "bbbbbbbbbbbb", "target-script");
        let accepted = start_promote(&root, "bbbbbbbbbbbb").expect("202");
        assert_eq!(accepted.script_from, "target");
        let log = root.join("bbbbbbbbbbbb").join("promote.log");
        let mut logged = String::new();
        for _ in 0..100 {
            logged = std::fs::read_to_string(&log).unwrap_or_default();
            if logged.contains("target-script") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(logged.contains("target-script bbbbbbbbbbbb"), "{logged:?}");

        // (3) current も持っている → **current のもの**を使う（実装者が昇格先の promote.sh を
        //     壊しても、それは走らない）。前の昇格のロックは消してから。
        std::fs::remove_file(root.join("bbbbbbbbbbbb").join("promote.lock")).expect("rm lock");
        std::fs::remove_file(&log).expect("rm log");
        fake_script(&root, "aaaaaaaaaaaa", "current-script");
        let accepted = start_promote(&root, "bbbbbbbbbbbb").expect("202");
        assert_eq!(accepted.script_from, "current");
        // ログは**昇格先**の promote.log（人が見る場所は変わらない）。
        assert!(accepted.log.ends_with("bbbbbbbbbbbb/promote.log"), "{}", accepted.log);
        for _ in 0..100 {
            logged = std::fs::read_to_string(&log).unwrap_or_default();
            if logged.contains("current-script") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(logged.contains("current-script bbbbbbbbbbbb"), "{logged:?}");
    }

    /// ADR-0041 D3: `promoted.json` が `promoted_at` になる。無ければ `null`。
    #[test]
    fn promoted_json_becomes_promoted_at() {
        let (_dir, root) = env();
        release(&root, "aaaaaaaaaaaa", Some(r#"{"built_at":"2026-09-18T00:00:00Z"}"#), Some(r#"{"ok":true}"#), None);
        std::fs::write(
            root.join("aaaaaaaaaaaa").join("promoted.json"),
            r#"{"promoted_at":"2026-09-19T12:31:36Z","mode":"stop-start","from":null}"#,
        )
        .expect("write");
        release(&root, "bbbbbbbbbbbb", Some(r#"{"built_at":"2026-09-19T00:00:00Z"}"#), Some(r#"{"ok":true}"#), None);

        let scanned = scan(&root, None);
        let by = |sha: &str| scanned.items.iter().find(|i| i.sha12 == sha).expect("item").clone();
        assert_eq!(by("aaaaaaaaaaaa").promoted_at.as_deref(), Some("2026-09-19T12:31:36Z"));
        assert_eq!(by("bbbbbbbbbbbb").promoted_at, None, "昇格していないリリースは null");
        // 壊れた promoted.json でも落ちない。
        std::fs::write(root.join("bbbbbbbbbbbb").join("promoted.json"), "{ broken").expect("write");
        assert_eq!(scan(&root, None).items.len(), 2);
    }

    /// ADR-0041 D4: `changes.json` が `changes` になる。`stale` は**いまの** `current` と比べて決める。
    /// `changes.json` が無いリリース（Phase 48 以前）は `null`。
    #[test]
    fn changes_json_becomes_changes_with_stale_computed_against_current() {
        let (dir, root) = env();
        release(&root, "aaaaaaaaaaaa", Some(r#"{"built_at":"2026-09-18T00:00:00Z"}"#), Some(r#"{"ok":true}"#), None);
        release(&root, "bbbbbbbbbbbb", Some(r#"{"built_at":"2026-09-19T00:00:00Z"}"#), Some(r#"{"ok":true}"#), None);
        std::fs::write(
            root.join("bbbbbbbbbbbb").join("changes.json"),
            r#"{"base":"aaaaaaaaaaaa",
                "commits":[{"sha":"1111111111111111111111111111111111111111","subject":"phase 50"},
                           {"sha":"2222222222222222222222222222222222222222","subject":"adr-0041"}],
                "files":["crates/celeris/src/releases.rs","docs/PROGRESS.md","README.md"],
                "sensitive":["crates/celeris/src/releases.rs"]}"#,
        )
        .expect("write");
        std::os::unix::fs::symlink("releases/aaaaaaaaaaaa", dir.path().join("current")).expect("symlink");

        let scanned = scan(&root, None);
        let newest = &scanned.items[0];
        assert_eq!(newest.sha12, "bbbbbbbbbbbb");
        let changes = newest.changes.as_ref().expect("changes");
        assert_eq!(changes.base.as_deref(), Some("aaaaaaaaaaaa"));
        assert!(!changes.stale, "base == current なら stale ではない");
        assert_eq!(changes.commit_count, 2);
        assert_eq!(changes.file_count, 3);
        assert_eq!(changes.sensitive, ["crates/celeris/src/releases.rs"]);
        assert_eq!(changes.commits[0].sha, "1111111111111111111111111111111111111111");
        assert_eq!(changes.commits[0].subject, "phase 50");
        // `changes.json` が無いリリース（Phase 48 以前）は `null`。
        assert!(scanned.items[1].changes.is_none());

        // `current` が動いたら stale になる（差分の起点がもう「いま」ではない）。
        std::fs::remove_file(dir.path().join("current")).expect("rm");
        std::os::unix::fs::symlink("releases/cccccccccccc", dir.path().join("current")).expect("symlink");
        let stale = scan(&root, None).items[0].changes.clone().expect("changes");
        assert!(stale.stale);
    }

    /// ADR-0041 D3: `on_main` は `git merge-base --is-ancestor <sha> main`。
    /// リポジトリが無い・その sha を知らないときは `null`（一覧は落ちない）。
    #[test]
    fn on_main_is_true_false_or_null() {
        let (dir, root) = env();
        let repo = dir.path().join("repo");
        let Some((merged, unmerged)) = git_repo(&repo) else {
            eprintln!("git is not usable here; skipping");
            return;
        };

        release(&root, "aaaaaaaaaaaa", Some(&format!(r#"{{"sha":"{merged}","built_at":"2026-09-18T00:00:00Z"}}"#)),
            Some(r#"{"ok":true}"#), None);
        release(&root, "bbbbbbbbbbbb", Some(&format!(r#"{{"sha":"{unmerged}","built_at":"2026-09-19T00:00:00Z"}}"#)),
            Some(r#"{"ok":true}"#), None);
        // このリポジトリが知らない sha（別のチェックアウトでビルドした版）。
        release(&root, "cccccccccccc", Some(r#"{"sha":"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef","built_at":"2026-09-17T00:00:00Z"}"#),
            Some(r#"{"ok":true}"#), None);

        let scanned = scan(&root, Some(&repo));
        let by = |sha: &str| scanned.items.iter().find(|i| i.sha12 == sha).expect("item").clone();
        assert_eq!(by("aaaaaaaaaaaa").on_main, Some(true), "main の祖先");
        assert_eq!(by("bbbbbbbbbbbb").on_main, Some(false), "main に入っていない");
        assert_eq!(by("cccccccccccc").on_main, None, "このリポジトリが知らない sha");

        // リポジトリが無ければ全部 null（`git` を 1 回試して諦める）。
        let missing = dir.path().join("no-such-repo");
        assert!(scan(&root, Some(&missing)).items.iter().all(|i| i.on_main.is_none()));
        // `repo` を渡さなければそもそも見ない。
        assert!(scan(&root, None).items.iter().all(|i| i.on_main.is_none()));
    }
}
