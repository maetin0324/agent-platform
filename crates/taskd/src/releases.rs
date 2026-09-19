//! ADR-0040 D6（Phase 48）: `[selfdeploy] releases_dir` を読む／昇格を起こす。
//!
//! ここにあるのは**ディレクトリに対する純粋な関数**だけで、判断（どれを昇格するか）は人が GUI か
//! shell で行う（ADR-0040 D5）。LLM もワーカーも関与しない。
//!
//! 読むもの（`release.sh` / `verify.sh` / `promote.sh` が書いたもの。`docs/selfdeploy.md`）:
//!
//! ```text
//! <releases_dir>/<sha12>/manifest.json   {sha12, ref, built_at, schema_version, ...}
//! <releases_dir>/<sha12>/gate.json       {ok, failed_step, steps: [...]}
//! <releases_dir>/<sha12>/verify.json     {ok, live_ok, at, checks: [...]}   ← 無ければ未検証
//! <releases_dir>/<sha12>/scripts/*.sh    release.sh が同梱した selfdeploy 一式
//! <releases_dir>/<sha12>/promote.lock    昇格中の pid（この模組が書く）
//! <releases_dir>/<sha12>/promote.log     promote.sh の出力
//! <releases_dir>/../current -> releases/<sha12>
//! <releases_dir>/../previous -> releases/<sha12>
//! ```
//!
//! 壊れた JSON・途中で消えたディレクトリでは**落ちない**（その 1 件が `gate_ok = false` と
//! `problem` を持つだけ）。`.build` / `.cargo-target` のような `.` で始まる名前と `*.partial` は飛ばす。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use task_api::types::{ReleaseItem, ReleasePromoteAccepted, ReleaseVerify};
use task_api::{ReleasePromoteError, ReleaseSource, ReleasesFs};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::instance::pid_alive;

/// `<releases_dir>` を読む `ReleaseSource`（taskd が `ApiSettings` に渡す）。
#[derive(Debug, Clone)]
pub struct FsReleases {
    root: PathBuf,
}

impl FsReleases {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl ReleaseSource for FsReleases {
    fn list(&self) -> ReleasesFs {
        scan(&self.root)
    }

    fn promote(&self, sha12: &str) -> Result<ReleasePromoteAccepted, ReleasePromoteError> {
        start_promote(&self.root, sha12)
    }
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

/// `<releases_dir>` を読んで一覧を作る（純粋。副作用は読み取りだけ）。
pub fn scan(root: &Path) -> ReleasesFs {
    // `current` / `previous` は `releases_dir` の**親**にある（`~/taskd/current -> releases/<sha12>`）。
    let home = root.parent();
    let current = home.and_then(|h| link_target_name(&h.join("current")));
    let previous = home.and_then(|h| link_target_name(&h.join("previous")));

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
        items.push(read_release(&entry.path(), &name, current.as_deref(), previous.as_deref()));
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

fn read_release(dir: &Path, sha12: &str, current: Option<&str>, previous: Option<&str>) -> ReleaseItem {
    let manifest = read_json(&dir.join("manifest.json"));
    let gate = read_json(&dir.join("gate.json"));
    let verify = read_json(&dir.join("verify.json"));

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

/// `<releases_dir>/<sha12>/scripts/promote.sh <sha12>` を detached（`setsid`、stdin は `/dev/null`、
/// stdout/err は `<release>/promote.log`）で起こし、`promote.lock` に pid を書く。
///
/// **この関数を自動で呼ぶ経路は作らない**（ADR-0040 D5: 昇格は人が押す）。呼ぶのは
/// `POST /releases/{sha12}/promote` だけで、そこは管理系（トークン必須）。
///
/// 昇格は**この taskd 自身を drain させうる**（ADR-0040 D4）。だから `promote.sh` は taskd の
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
    if root.parent().and_then(|h| link_target_name(&h.join("current"))).as_deref() == Some(sha12) {
        return Err(ReleasePromoteError::AlreadyCurrent);
    }

    // 3. 既に昇格中か。
    if promoting_pid(&dir).is_some() {
        return Err(ReleasePromoteError::AlreadyPromoting);
    }

    // 4. リリースに同梱された `scripts/promote.sh`（`release.sh` が入れる。ADR-0040 D6 / Phase 48）。
    let script = dir.join("scripts").join("promote.sh");
    if !script.is_file() {
        return Err(ReleasePromoteError::Unavailable(format!(
            "{} is missing — this release was built before the selfdeploy scripts were bundled",
            script.display()
        )));
    }

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

    fn env() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("releases");
        std::fs::create_dir_all(&root).expect("mkdir");
        (dir, root)
    }

    #[test]
    fn scanning_an_empty_or_missing_directory_yields_nothing_and_does_not_fail() {
        let (dir, root) = env();
        let scanned = scan(&root);
        assert!(scanned.items.is_empty());
        assert_eq!(scanned.current, None);
        assert_eq!(scanned.previous, None);
        // ディレクトリごと無いとき（初回）も同じ。
        let missing = dir.path().join("nope");
        assert!(scan(&missing).items.is_empty());
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
        // `~/taskd/current -> releases/aaaaaaaaaaaa`（releases_dir の親に張る）。
        std::os::unix::fs::symlink("releases/aaaaaaaaaaaa", dir.path().join("current")).expect("symlink");
        std::os::unix::fs::symlink("releases/bbbbbbbbbbbb", dir.path().join("previous")).expect("symlink");

        let scanned = scan(&root);
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

        let scanned = scan(&root);
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
        let scanned = scan(&root);
        assert!(scanned.items.iter().any(|i| i.sha12 == "abcdef123456" && i.promoting));
    }
}
