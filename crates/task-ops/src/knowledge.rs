//! 知識ベースのファイル操作（ADR-0047 D1〜D3。Phase 61）。
//!
//! **正本は `~/knowledge/` の Markdown**（`[knowledge] root` で変えられる）で、DB には何も持たない。
//! `task_ops::docs`（ADR-0044 D7）と同じ流儀で **`git` を起こすだけ**（判断も LLM も無い。DESIGN 原則 1）。
//! 文書との違いは「正本が作業ツリーそのもの」という一点で、読み取りは常にファイルを読み、書き込みは
//! 作業ツリーに書いてから 1 件ずつコミットする（一時 worktree は要らない。人も同じファイルを直接編集する）。
//!
//! - [`init`] — `git init` + 骨組み + 雛形 + `README.md` + `_inbox/` + `index.json`（冪等）
//! - [`reindex`] / [`load_index`] / [`ensure_index`] — `index.json`（派生物。`_inbox` は入れない）
//! - [`read_page`] / [`etag`] / [`history`] — ページ 1 枚
//! - [`commit_page`] — 1 件 1 コミット（`etag` で衝突を見る）
//! - [`search`] — 索引 ＋ `git grep -il`（無ければ `grep -ril`）→ [`task_core::knowledge::search`] で順位付け
//! - [`record`] — 候補を `_inbox/<ts>-<slug>.md` に書く（`sources` 必須、秘密は拒否）
//! - [`inbox_list`] / [`inbox_accept`] / [`inbox_reject`] — 候補の一覧と取り込み・破棄
//!
//! 子プロセス（`git` / `grep`）には全部待ち時間の上限がある。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use task_core::knowledge::{
    self as kb, Confidence, FrontMatter, INBOX_DIR, INDEX_FILE, Index, IndexItem, PathError,
    SearchHit,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::changes::{CmdOutput, GIT_TIMEOUT, GIT_WRITE_TIMEOUT, git};
use crate::docs::DocCommit;

/// 索引が古いと見なす閾値（daemon は起動時にこれを超えていれば作り直す）。
pub const INDEX_STALE_SECS: i64 = 6 * 60 * 60;
/// `grep` にかける上限（`git` が使えないときのフォールバック）。
const GREP_TIMEOUT: std::time::Duration = GIT_TIMEOUT;
/// ADR-0047 D4（Phase 62）: `op = retire` の取り込み先（`_inbox` accept が動かす。P-61-k の答え:
/// `DELETE /knowledge/page` は足さず、「捨てる」は retire 一本にする。`docs/knowledge.md` に明記）。
/// `_inbox` と同じく索引にも検索にも出ない。
pub const RETIRED_DIR: &str = "_retired";

fn is_retired(path: &str) -> bool {
    path == RETIRED_DIR || path.starts_with(&format!("{RETIRED_DIR}/"))
}

// ---------------------------------------------------------------------------
// 根の解決（ADR-0047 D3: `--root` > `CELERIS_KNOWLEDGE_ROOT` > `[knowledge] root` > `~/knowledge`）
// ---------------------------------------------------------------------------

/// 知識ベースの根を決める。`~` は展開する。
pub fn resolve_root(explicit: Option<&Path>, configured: Option<&Path>) -> PathBuf {
    let home = task_core::home_dir();
    let raw = explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("CELERIS_KNOWLEDGE_ROOT").map(PathBuf::from))
        .or_else(|| configured.map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from(kb::DEFAULT_ROOT));
    task_core::expand_home(&raw, home.as_deref())
}

/// KB がそこにあるか（`init` 済みか）。
pub fn exists(root: &Path) -> bool {
    root.join(".git").exists()
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn today() -> String {
    now_rfc3339()
        .split('T')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn author_args(name: &str, email: &str) -> (String, String) {
    (format!("user.name={name}"), format!("user.email={email}"))
}

fn commit_paths(
    root: &Path,
    message: &str,
    author: (&str, &str),
    paths: &[&str],
) -> Result<String, String> {
    let mut add: Vec<&str> = vec!["add", "-A", "--"];
    add.extend_from_slice(paths);
    if !git(root, &add, GIT_WRITE_TIMEOUT).is_some_and(|o| o.ok) {
        return Err("git add に失敗しました".to_string());
    }
    let (name, email) = author_args(author.0, author.1);
    let mut args: Vec<&str> = vec![
        "-c", &name, "-c", &email, "commit", "-q", "-m", message, "--",
    ];
    args.extend_from_slice(paths);
    match git(root, &args, GIT_WRITE_TIMEOUT) {
        Some(o) if o.ok => {}
        Some(o) => return Err(format!("コミットできませんでした: {}", o.why())),
        None => return Err("git を起動できませんでした".to_string()),
    }
    Ok(head(root).unwrap_or_default())
}

fn head(root: &Path) -> Option<String> {
    git(root, &["rev-parse", "HEAD"], GIT_TIMEOUT)
        .filter(|o| o.ok)
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// 初期化（ADR-0047 D1）
// ---------------------------------------------------------------------------

/// `init` の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    pub root: PathBuf,
    /// この呼び出しで `git init` した。
    pub created: bool,
    /// この呼び出しで新しく書いた雛形（KB 相対）。既にあるページは**触らない**。
    pub added: Vec<String>,
}

/// ADR-0047 D1: `~/knowledge` を git のリポジトリとして用意する（**冪等**）。
///
/// 既にあるファイルは 1 バイトも触らない。足りないディレクトリ・雛形・`README.md`・`.gitignore`・
/// `_inbox/.gitkeep` だけを書き、変わったものがあれば 1 回コミットして `index.json` を作り直す。
pub fn init(root: &Path) -> Result<InitOutcome, String> {
    std::fs::create_dir_all(root)
        .map_err(|e| format!("{} を作れませんでした: {e}", root.display()))?;
    let created = !root.join(".git").exists();
    if created {
        match git(root, &["init", "-q", "-b", "main"], GIT_WRITE_TIMEOUT) {
            Some(o) if o.ok => {}
            Some(o) => return Err(format!("git init に失敗しました: {}", o.why())),
            None => return Err("git を起動できませんでした".to_string()),
        }
    }
    for dir in kb::SKELETON_DIRS.iter().chain([INBOX_DIR].iter()) {
        std::fs::create_dir_all(root.join(dir))
            .map_err(|e| format!("{dir} を作れませんでした: {e}"))?;
    }
    let mut added = Vec::new();
    for (path, body) in seed_files() {
        let target = root.join(&path);
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("{} を作れませんでした: {e}", parent.display()))?;
        }
        std::fs::write(&target, body.as_bytes())
            .map_err(|e| format!("{path} を書けませんでした: {e}"))?;
        added.push(path);
    }
    if !added.is_empty() || created {
        let message = if created {
            "knowledge: 知識ベースを作る（ADR-0047 D1）".to_string()
        } else {
            format!("knowledge: 雛形を追加（{} 件）", added.len())
        };
        commit_paths(
            root,
            &message,
            (kb::HUMAN_AUTHOR_NAME, kb::HUMAN_AUTHOR_EMAIL),
            &["."],
        )?;
    }
    reindex(root)?;
    Ok(InitOutcome {
        root: root.to_path_buf(),
        created,
        added,
    })
}

/// ADR-0047 D1 の雛形（**人が埋める前提のテンプレート**。中身は空欄と書き方の説明だけ）。
fn seed_files() -> Vec<(String, String)> {
    let today = today();
    let page = |title: &str, tags: &[&str], scope: &str, body: &str| {
        kb::render_page(
            &FrontMatter {
                title: Some(title.to_string()),
                tags: tags.iter().map(|t| (*t).to_string()).collect(),
                scope: Some(scope.to_string()),
                sources: vec!["human".to_string()],
                created: Some(today.clone()),
                updated: Some(today.clone()),
                confidence: Some(Confidence::Medium),
                path: None,
                op: None,
            },
            body,
        )
    };
    vec![
        (".gitignore".to_string(), GITIGNORE.to_string()),
        ("README.md".to_string(), readme()),
        (format!("{INBOX_DIR}/.gitkeep"), String::new()),
        (
            "user/profile.md".to_string(),
            page(
                "人のプロフィール",
                &["user"],
                "user",
                "# 人のプロフィール\n\n- 所属・役割:\n- 呼び方・言語:\n- 連絡の好み:\n\n\
                 （celeris が人のことで繰り返し確かめている事実をここに書く。一時的な予定は書かない）\n",
            ),
        ),
        (
            "user/expertise.md".to_string(),
            page(
                "人の専門",
                &["user"],
                "user",
                "# 人の専門\n\n- 得意:\n- 前提として説明が要らないこと:\n- 説明が要ること:\n",
            ),
        ),
        (
            "user/preferences.md".to_string(),
            page(
                "人の好み",
                &["user"],
                "user",
                "# 人の好み\n\n- 文書の書き方:\n- 実装の進め方:\n- 確認を取ってほしい場面:\n",
            ),
        ),
        (
            "user/goals.md".to_string(),
            page(
                "人の目標",
                &["user"],
                "user",
                "# 人の目標\n\n- いま追っていること:\n- 中期の目標:\n",
            ),
        ),
        (
            "environment/clusters/pegasus.md".to_string(),
            cluster_page(&today, "pegasus"),
        ),
        (
            "environment/clusters/sirius.md".to_string(),
            cluster_page(&today, "sirius"),
        ),
        (
            "environment/clusters/fern03.md".to_string(),
            cluster_page(&today, "fern03"),
        ),
        (
            "experience/README.md".to_string(),
            page(
                "経験",
                &["experience"],
                "experience",
                "# 経験\n\n`YYYY/MM/<slug>.md` に 1 件ずつ。**問題・解法・結果・採らなかった案と理由**を書く。\n",
            ),
        ),
        (
            "projects/README.md".to_string(),
            page(
                "案件の知識",
                &["projects"],
                "user",
                "# 案件の知識\n\n`projects/<slug>/` に 1 案件ずつ（`design.md` / `decisions.md` / `status.md` …）。\n\
                 `<slug>` は案件の文書リポジトリと同じ slug（ADR-0044 D7）。案件のタスクの前置きには\n\
                 この `projects/<slug>` が自動でマウントされる。\n",
            ),
        ),
    ]
}

/// クラスタ 1 台分の雛形（**人が埋める**。`docs/` と設定に書いてあることを書き写す場所）。
fn cluster_page(today: &str, id: &str) -> String {
    kb::render_page(
        &FrontMatter {
            title: Some(format!("{id} の使い方")),
            tags: vec!["environment".into(), "cluster".into(), id.to_string()],
            scope: Some("environment".into()),
            sources: vec!["human".into()],
            created: Some(today.to_string()),
            updated: Some(today.to_string()),
            confidence: Some(Confidence::Low),
            path: None,
            op: None,
        },
        &format!(
            "# {id} の使い方\n\n\
             > 雛形。**人が埋める**（`docs/` と `~/.config/celeris/config.toml` の `[[clusters]]` に\n\
             > 既に書いてあることを書き写す。埋めたら `confidence: high` にする）。\n\n\
             ## 接続\n\n- ホスト（`ssh` の別名）:\n- 踏み台:\n- 認証:\n\n\
             ## 作業場所\n\n- 作業ツリーの根:\n- 共有ディレクトリ:\n\n\
             ## ジョブ\n\n- 投げ方:\n- 待ち行列 / 資源の単位:\n- よくある落とし穴:\n\n\
             ## 環境\n\n- module / コンパイラ:\n- 使えるネットワーク・ストレージ:\n"
        ),
    )
}

const GITIGNORE: &str =
    "# ADR-0047 D1 / D6: 索引は再生成できる派生物（正本は Markdown）。\nindex.json\nindex.*/\n";

fn readme() -> String {
    format!(
        "# 知識ベース（Celeris）\n\n\
         正本はこのディレクトリの **Markdown**（ADR-0047）。DB も外部サービスも正本ではない。\n\
         `index.json` は再生成できる派生物なので git には入れない。\n\n\
         ```\n\
         user/                 人のこと（profile / expertise / preferences / goals）\n\
         environment/          環境（clusters/ servers/ tools/）\n\
         projects/<slug>/      案件の知識（design.md / decisions.md / status.md …）\n\
         experience/           経験（YYYY/MM/<slug>.md。問題・解法・結果・採らなかった案）\n\
         {INBOX_DIR}/               抽出された候補。まだ索引に入らない（人が accept / reject する）\n\
         {INDEX_FILE}            派生物\n\
         ```\n\n\
         1 ファイル = 1 トピック。先頭に front matter を付ける:\n\n\
         ```\n\
         ---\n\
         title: pegasus の使い方\n\
         tags: [hpc, cluster, pegasus]\n\
         scope: environment\n\
         sources: [human, \"task:01J…\"]\n\
         created: 2026-09-20\n\
         updated: 2026-09-20\n\
         confidence: high\n\
         ---\n\
         ```\n\n\
         `scope` は `user` / `environment` / `project:<slug>` / `experience`。\n\
         `sources` は `task:<id>` / `message:<id>` / `human` / `url:<…>`。\n\n\
         ## 道具\n\n\
         ```\n\
         celerisctl knowledge search <語> [--scope …] [--limit N] [--json]\n\
         celerisctl knowledge get <path>\n\
         celerisctl knowledge record --title … --scope … --tags … --source task:<id> < body.md\n\
         celerisctl knowledge reindex\n\
         ```\n\n\
         `record` は**候補**を `{INBOX_DIR}/` に書く（正本には直接書かない）。人が GUI の「知識」画面で\n\
         accept / reject する。秘密（API キー・トークン・秘密鍵）は決定的な検査で弾かれる。\n"
    )
}

// ---------------------------------------------------------------------------
// 索引（`index.json`。派生物）
// ---------------------------------------------------------------------------

/// KB の下の `*.md`（`_inbox` と `.git` は除く。名前順、最大 [`kb::MAX_INDEX_ITEMS`] 件）。
pub fn list_pages(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out.truncate(kb::MAX_INDEX_ITEMS);
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.starts_with('.') || kb::is_inbox(&rel) {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, out);
        } else if rel.to_ascii_lowercase().ends_with(".md") {
            out.push(rel);
        }
    }
}

/// ADR-0047 D1 / D3: `index.json` を作り直す（`_inbox` は入れない）。
pub fn reindex(root: &Path) -> Result<Index, String> {
    let last = last_commits(root);
    let mut items = Vec::new();
    for path in list_pages(root) {
        let Ok(raw) = std::fs::read_to_string(root.join(&path)) else {
            continue;
        };
        items.push(index_item(&path, &raw, last.get(&path)));
    }
    let index = Index {
        generated_at: now_rfc3339(),
        items,
    };
    write_index(root, &index)?;
    Ok(index)
}

fn index_item(path: &str, raw: &str, commit: Option<&DocCommit>) -> IndexItem {
    let (front, _) = kb::front_matter(raw);
    IndexItem {
        title: kb::title_of(raw, path),
        tags: front.tags,
        scope: front.scope.or_else(|| default_scope(path)),
        sources: front.sources,
        updated: front.updated.or_else(|| commit.map(|c| c.at.clone())),
        confidence: front.confidence,
        path: path.to_string(),
    }
}

/// front matter に `scope` が無いページの既定（置き場から決める）。
fn default_scope(path: &str) -> Option<String> {
    let top = path.split('/').next()?;
    match top {
        "user" | "environment" | "experience" => Some(top.to_string()),
        "projects" => path.split('/').nth(1).map(|slug| format!("project:{slug}")),
        _ => None,
    }
}

fn write_index(root: &Path, index: &Index) -> Result<(), String> {
    let json = serde_json::to_string_pretty(index)
        .map_err(|e| format!("索引を組み立てられませんでした: {e}"))?;
    std::fs::write(root.join(INDEX_FILE), format!("{json}\n").as_bytes())
        .map_err(|e| format!("{INDEX_FILE} を書けませんでした: {e}"))
}

/// `index.json` を読む（無い・壊れていれば `None`）。
pub fn load_index(root: &Path) -> Option<Index> {
    let raw = std::fs::read_to_string(root.join(INDEX_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 索引が無い・古い（[`INDEX_STALE_SECS`] より前）か。
pub fn index_is_stale(root: &Path) -> bool {
    let Some(index) = load_index(root) else {
        return true;
    };
    let Ok(at) = OffsetDateTime::parse(&index.generated_at, &Rfc3339) else {
        return true;
    };
    (OffsetDateTime::now_utc() - at).whole_seconds() > INDEX_STALE_SECS
}

/// 索引を読む。無い・古ければ作り直す（daemon が起動時に呼ぶ。ADR-0047 D3）。
pub fn ensure_index(root: &Path) -> Index {
    if index_is_stale(root)
        && let Ok(index) = reindex(root)
    {
        return index;
    }
    load_index(root).unwrap_or_default()
}

/// ページごとの**最後のコミット**（`git log --name-only` を 1 回だけ起こす）。
pub fn last_commits(root: &Path) -> BTreeMap<String, DocCommit> {
    let format = format!("--format={RECORD}%H{UNIT}%aI{UNIT}%an{UNIT}%s");
    let mut out = BTreeMap::new();
    let Some(result) = git(
        root,
        &["log", "--no-merges", "--name-only", &format],
        GIT_TIMEOUT,
    )
    .filter(|o| o.ok) else {
        return out;
    };
    for record in result.stdout.split(RECORD).skip(1) {
        let mut lines = record.lines();
        let Some(header) = lines.next() else { continue };
        let Some(commit) = parse_commit(header) else {
            continue;
        };
        for path in lines.map(str::trim).filter(|p| !p.is_empty()) {
            out.entry(path.to_string())
                .or_insert_with(|| commit.clone());
        }
    }
    out
}

const UNIT: char = '\u{1f}';
const RECORD: char = '\u{1e}';

fn parse_commit(line: &str) -> Option<DocCommit> {
    let mut parts = line.split(UNIT);
    let sha = parts.next()?.trim().to_string();
    if sha.is_empty() {
        return None;
    }
    Some(DocCommit {
        sha,
        at: parts.next().unwrap_or_default().trim().to_string(),
        author: parts.next().unwrap_or_default().trim().to_string(),
        subject: parts.next().unwrap_or_default().trim().to_string(),
    })
}

// ---------------------------------------------------------------------------
// ページ 1 枚
// ---------------------------------------------------------------------------

/// KB のファイルの絶対パス（境界の検査を通したものだけ）。
pub fn page_file(root: &Path, path: &str) -> Result<PathBuf, PathError> {
    let path = kb::page_path(path)?;
    Ok(root.join(path))
}

/// ページの中身（**作業ツリーのファイル**。人が編集中のものもそのまま見える）。
pub fn read_page(root: &Path, path: &str) -> Option<String> {
    std::fs::read_to_string(root.join(path)).ok()
}

/// ページの `etag`（中身の sha256。作業ツリーのファイルに対して決まる）。無ければ `None`。
pub fn etag(root: &Path, path: &str) -> Option<String> {
    let bytes = std::fs::read(root.join(path)).ok()?;
    Some(content_etag(&bytes))
}

fn content_etag(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// ページ 1 枚の履歴（新しい順、直近 [`kb::HISTORY_LIMIT`] 件）。
pub fn history(root: &Path, path: &str) -> Vec<DocCommit> {
    let format = format!("--format=%H{UNIT}%aI{UNIT}%an{UNIT}%s");
    let limit = format!("-{}", kb::HISTORY_LIMIT);
    let Some(out) = git(root, &["log", &limit, &format, "--", path], GIT_TIMEOUT).filter(|o| o.ok)
    else {
        return Vec::new();
    };
    out.stdout.lines().filter_map(parse_commit).collect()
}

/// 1 回の書き込み。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEdit {
    /// KB 相対のパス（[`kb::page_path`] を通したもの）。
    pub path: String,
    /// 本文。`None` なら削除。
    pub body: Option<String>,
    /// 期待する `etag`。新規作成のときだけ `None` でよい。
    pub etag: Option<String>,
    pub message: String,
    /// 作った人（`Celeris (human)` / `Celeris (knowledge)`）。
    pub author: (String, String),
}

/// 書き込みの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    Written {
        sha: String,
        etag: Option<String>,
        unchanged: bool,
    },
    /// `etag` が現在の中身と違う（409）。`etag` はいまの値。
    EtagMismatch {
        etag: Option<String>,
    },
    /// 消そうとしたページが無い（404）。
    Missing,
    Failed {
        detail: String,
    },
}

/// ADR-0047 D1: **1 件 1 コミット**。作業ツリーに書いてからそのパスだけをコミットする。
pub fn commit_page(root: &Path, edit: &PageEdit) -> WriteOutcome {
    if !exists(root) {
        return WriteOutcome::Failed {
            detail: format!(
                "{} は知識ベースではありません（celerisctl knowledge init）",
                root.display()
            ),
        };
    }
    let current = etag(root, &edit.path);
    if edit.body.is_none() && current.is_none() {
        return WriteOutcome::Missing;
    }
    if edit.etag.as_deref() != current.as_deref() {
        return WriteOutcome::EtagMismatch { etag: current };
    }
    let target = root.join(&edit.path);
    match &edit.body {
        Some(body) => {
            if let Some(parent) = target.parent()
                && std::fs::create_dir_all(parent).is_err()
            {
                return WriteOutcome::Failed {
                    detail: format!("{} を作れませんでした", edit.path),
                };
            }
            let mut text = body.replace("\r\n", "\n");
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            if current.as_deref() == Some(content_etag(text.as_bytes()).as_str()) {
                return WriteOutcome::Written {
                    sha: head(root).unwrap_or_default(),
                    etag: current,
                    unchanged: true,
                };
            }
            if std::fs::write(&target, text.as_bytes()).is_err() {
                return WriteOutcome::Failed {
                    detail: format!("{} を書けませんでした", edit.path),
                };
            }
        }
        None => {
            if std::fs::remove_file(&target).is_err() {
                return WriteOutcome::Failed {
                    detail: format!("{} を消せませんでした", edit.path),
                };
            }
        }
    }
    match commit_paths(
        root,
        &edit.message,
        (edit.author.0.as_str(), edit.author.1.as_str()),
        &[edit.path.as_str()],
    ) {
        Ok(sha) => WriteOutcome::Written {
            sha,
            etag: etag(root, &edit.path),
            unchanged: false,
        },
        Err(detail) => WriteOutcome::Failed { detail },
    }
}

// ---------------------------------------------------------------------------
// 検索（ADR-0047 D3）
// ---------------------------------------------------------------------------

/// 本文の全文一致（`git grep -il`、無ければ `grep -ril`）。当たった KB 相対パス（`_inbox` は除く）。
pub fn grep(root: &Path, needle: &str) -> Vec<String> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Vec::new();
    }
    // `--untracked` は「人がまだコミットしていないページ」も見る（正本は作業ツリーなので）。
    let out = git(
        root,
        &[
            "grep",
            "-I",
            "-i",
            "-l",
            "-F",
            "--untracked",
            "-e",
            needle,
            "--",
            "*.md",
        ],
        GIT_TIMEOUT,
    );
    let text = match out {
        // 「見つからない」は exit 1（エラーではない）。
        Some(o) => o.stdout,
        // `git` そのものが無い環境では `grep -ril`（ADR-0047 D3）。
        None => plain_grep(root, needle),
    };
    let mut paths: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.trim_start_matches("./").to_string())
        .filter(|p| p.to_ascii_lowercase().ends_with(".md") && !kb::is_inbox(p))
        .collect();
    paths.sort();
    paths.dedup();
    paths.truncate(kb::MAX_INDEX_ITEMS);
    paths
}

/// git が動かない（KB がまだ git でない）ときの `grep -ril`。
fn plain_grep(root: &Path, needle: &str) -> String {
    let out = run(
        root,
        "grep",
        &["-r", "-i", "-l", "-F", "--include=*.md", "-e", needle, "."],
        GREP_TIMEOUT,
    );
    out.map(|o| o.stdout).unwrap_or_default()
}

/// 子プロセスを 1 つ起こす（`crate::changes::git` と同じ上限の付け方）。
fn run(
    dir: &Path,
    program: &str,
    args: &[&str],
    timeout: std::time::Duration,
) -> Option<CmdOutput> {
    crate::changes::run_with_timeout(dir, program, args, timeout)
}

/// ADR-0047 D3 の検索（索引 ＋ 本文一致 → [`kb::search`] の順位付け）。
pub fn search(root: &Path, query: &str, scope: Option<&str>, limit: usize) -> Vec<SearchHit> {
    let index = ensure_index(root);
    let body_hits = if query.trim().is_empty() {
        Vec::new()
    } else {
        grep(root, query.trim())
    };
    kb::search(&index, query, scope, limit, &body_hits)
}

// ---------------------------------------------------------------------------
// 候補（`_inbox/`。ADR-0047 D3 の `record` と D4 の適用）
// ---------------------------------------------------------------------------

/// `record` の入力（CLI と、Phase 62 の知識整理 run が使う）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordRequest {
    pub title: String,
    pub scope: String,
    pub tags: Vec<String>,
    /// **1 件以上必須**（ADR-0047 D3）。
    pub sources: Vec<String>,
    pub confidence: Option<Confidence>,
    pub body: String,
    /// 取り込む先の KB 相対パス（省略なら accept のときに `scope` と `title` から決める）。
    pub path: Option<String>,
}

/// `record` の失敗（CLI は 1、API は 422）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    #[error("title must not be blank")]
    NoTitle,
    #[error("scope must not be blank (user | environment | project:<slug> | experience)")]
    NoScope,
    #[error("at least one --source is required (task:<id> / message:<id> / human / url:<…>)")]
    NoSources,
    #[error("body must not be blank")]
    NoBody,
    /// ADR-0047 D4: 秘密は保存しない。
    #[error("refused: the candidate contains a secret（{0}）")]
    Secret(&'static str),
    #[error("{0}")]
    Failed(String),
}

/// `record` の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordOutcome {
    /// `_inbox/<ts>-<slug>.md`。
    pub path: String,
    /// `_inbox` の中での id（ファイル名から `.md` を取ったもの）。
    pub id: String,
    pub sha: String,
}

/// ADR-0047 D3: **候補**を `_inbox/` に書く（正本には直接書かない）。
pub fn record(root: &Path, request: &RecordRequest) -> Result<RecordOutcome, RecordError> {
    let title = request.title.trim();
    let scope = request.scope.trim();
    let body = request.body.trim();
    if title.is_empty() {
        return Err(RecordError::NoTitle);
    }
    if scope.is_empty() {
        return Err(RecordError::NoScope);
    }
    let sources: Vec<String> = request
        .sources
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if sources.is_empty() {
        return Err(RecordError::NoSources);
    }
    if body.is_empty() {
        return Err(RecordError::NoBody);
    }
    // ADR-0047 D4: 秘密（API キー・トークン・秘密鍵）は決定的な検査で弾く。
    let haystack = format!("{title}\n{body}\n{}", sources.join("\n"));
    if let Some(why) = kb::secret_finding(&haystack) {
        return Err(RecordError::Secret(why));
    }
    if !exists(root) {
        return Err(RecordError::Failed(format!(
            "{} は知識ベースではありません（celerisctl knowledge init）",
            root.display()
        )));
    }
    let target = request
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(kb::page_path)
        .transpose()
        .map_err(|e| RecordError::Failed(e.to_string()))?;
    let now = OffsetDateTime::now_utc();
    let stamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    let slug = kb::slugify(title).unwrap_or_else(|| "note".to_string());
    let mut id = format!("{stamp}-{slug}");
    let mut n = 2;
    while root.join(INBOX_DIR).join(format!("{id}.md")).exists() {
        id = format!("{stamp}-{slug}-{n}");
        n += 1;
    }
    let path = format!("{INBOX_DIR}/{id}.md");
    let page = kb::render_page(
        &FrontMatter {
            title: Some(title.to_string()),
            tags: request
                .tags
                .iter()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect(),
            scope: Some(scope.to_string()),
            sources,
            created: Some(today()),
            updated: Some(today()),
            confidence: request.confidence,
            path: target,
            op: None,
        },
        body,
    );
    let dir = root.join(INBOX_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| RecordError::Failed(format!("{INBOX_DIR} を作れませんでした: {e}")))?;
    std::fs::write(dir.join(format!("{id}.md")), page.as_bytes())
        .map_err(|e| RecordError::Failed(format!("{path} を書けませんでした: {e}")))?;
    let sha = commit_paths(
        root,
        &format!("knowledge: 候補 {path}"),
        (kb::AGENT_AUTHOR_NAME, kb::AGENT_AUTHOR_EMAIL),
        &[path.as_str()],
    )
    .map_err(RecordError::Failed)?;
    Ok(RecordOutcome { path, id, sha })
}

/// `_inbox/` の候補 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxItem {
    /// ファイル名から `.md` を取ったもの（API のパスに使う）。
    pub id: String,
    /// `_inbox/<id>.md`。
    pub path: String,
    pub title: String,
    pub tags: Vec<String>,
    pub scope: Option<String>,
    pub sources: Vec<String>,
    pub confidence: Option<Confidence>,
    /// 取り込む先（front matter の `path`、無ければ `scope` と `title` からの既定）。
    pub target: String,
    pub created: Option<String>,
    /// 本文（front matter を除く）。
    pub body: String,
}

/// `_inbox/` の候補（新しい順 = id の降順）。
pub fn inbox_list(root: &Path) -> Vec<InboxItem> {
    let dir = root.join(INBOX_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".md").map(str::to_string)
        })
        .collect();
    ids.sort();
    ids.reverse();
    ids.iter().filter_map(|id| inbox_get(root, id)).collect()
}

/// `_inbox` の id の検査（`/`・`..`・空は通さない）。
pub fn inbox_path(id: &str) -> Result<String, PathError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(PathError::Empty);
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err(PathError::Forbidden);
    }
    kb::page_path(&format!("{INBOX_DIR}/{id}.md"))
}

/// 候補 1 件。
pub fn inbox_get(root: &Path, id: &str) -> Option<InboxItem> {
    let path = inbox_path(id).ok()?;
    let raw = std::fs::read_to_string(root.join(&path)).ok()?;
    let (front, body) = kb::front_matter(&raw);
    let title = kb::title_of(&raw, &path);
    let target = front
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .and_then(|p| kb::page_path(p).ok())
        .unwrap_or_else(|| default_target(front.scope.as_deref(), &title, id));
    Some(InboxItem {
        id: id.trim().to_string(),
        path,
        title,
        tags: front.tags,
        scope: front.scope,
        sources: front.sources,
        confidence: front.confidence,
        target,
        created: front.created,
        body: body.trim_start_matches(['\n', '\r']).to_string(),
    })
}

/// 取り込み先の既定（`scope` のディレクトリ ＋ 題名の slug）。
fn default_target(scope: Option<&str>, title: &str, id: &str) -> String {
    let dir = scope
        .and_then(kb::scope_dir)
        .unwrap_or_else(|| "experience".to_string());
    let slug = kb::slugify(title).unwrap_or_else(|| id.to_ascii_lowercase());
    format!("{dir}/{slug}.md")
}

/// accept / reject の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxOutcome {
    /// 取り込んだ（`path` は正本の中での置き場）。
    Accepted {
        path: String,
        sha: String,
        etag: Option<String>,
    },
    /// 破棄した。
    Rejected {
        sha: String,
    },
    /// その id が無い（404）。
    Missing,
    /// 宛先が既にある（409）。
    Exists {
        path: String,
    },
    Failed {
        detail: String,
    },
}

/// ADR-0047 D3 / D5: 候補を正本に取り込む（`_inbox` から消して、`path` にコミットする）。
///
/// `path` を渡せばそこへ、渡さなければ front matter の `path`（無ければ `scope` と題名からの既定）へ。
/// 宛先が既にあるときは `overwrite` が無ければ 409。
pub fn inbox_accept(root: &Path, id: &str, path: Option<&str>, overwrite: bool) -> InboxOutcome {
    let Some(item) = inbox_get(root, id) else {
        return InboxOutcome::Missing;
    };
    // ADR-0047 D4（Phase 62）: `op = retire` は候補の中身を書くのではなく、`target`（対象の既存ページ）を
    // `_retired/` へ動かす。`op = merge` は候補の本文（= 書き直した完全な版）で `target` を**必ず上書き**する
    // （P-61-k: `DELETE /knowledge/page` は足さず、捨てるのは retire に一本化。`docs/knowledge.md` に明記）。
    if item.op == Some(kb::CandidateOp::Retire) {
        return inbox_accept_retire(root, &item);
    }
    let target = match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => match kb::page_path(p) {
            Ok(p) => p,
            Err(e) => {
                return InboxOutcome::Failed {
                    detail: e.to_string(),
                };
            }
        },
        None => item.target.clone(),
    };
    if kb::is_inbox(&target) || is_retired(&target) {
        return InboxOutcome::Failed {
            detail: "取り込み先を `_inbox/`・`_retired/` にはできません".to_string(),
        };
    }
    let overwrite = overwrite || item.op == Some(kb::CandidateOp::Merge);
    if !overwrite && root.join(&target).exists() {
        return InboxOutcome::Exists { path: target };
    }
    // 取り込んだページは `_inbox` 専用の `path:` を落とし、`updated` を今日にする。
    let Some(raw) = std::fs::read_to_string(root.join(&item.path)).ok() else {
        return InboxOutcome::Missing;
    };
    let (mut front, body) = kb::front_matter(&raw);
    front.path = None;
    front.updated = Some(today());
    let page = kb::render_page(&front, body);
    let target_file = root.join(&target);
    if let Some(parent) = target_file.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return InboxOutcome::Failed {
            detail: format!("{target} を作れませんでした"),
        };
    }
    if std::fs::write(&target_file, page.as_bytes()).is_err() {
        return InboxOutcome::Failed {
            detail: format!("{target} を書けませんでした"),
        };
    }
    if std::fs::remove_file(root.join(&item.path)).is_err() {
        return InboxOutcome::Failed {
            detail: format!("{} を消せませんでした", item.path),
        };
    }
    match commit_paths(
        root,
        &format!("knowledge: {target}（候補 {} を取り込む）", item.id),
        (kb::HUMAN_AUTHOR_NAME, kb::HUMAN_AUTHOR_EMAIL),
        &[target.as_str(), item.path.as_str()],
    ) {
        Ok(sha) => {
            let _ = reindex(root);
            InboxOutcome::Accepted {
                etag: etag(root, &target),
                path: target,
                sha,
            }
        }
        Err(detail) => InboxOutcome::Failed { detail },
    }
}

/// ADR-0047 D4（Phase 62）: `op = retire` の accept。候補の本文は書かず、`item.target`
/// （退役させる既存ページ）を `_retired/<target>` へ動かす。`target` が無ければ何もできない。
fn inbox_accept_retire(root: &Path, item: &InboxItem) -> InboxOutcome {
    if !root.join(&item.target).exists() {
        return InboxOutcome::Failed {
            detail: format!("退役させるページがありません: {}", item.target),
        };
    }
    let Some(raw) = std::fs::read_to_string(root.join(&item.target)).ok() else {
        return InboxOutcome::Failed {
            detail: format!("{} を読めませんでした", item.target),
        };
    };
    let retired_path = format!("{RETIRED_DIR}/{}", item.target);
    let retired_file = root.join(&retired_path);
    if let Some(parent) = retired_file.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return InboxOutcome::Failed {
            detail: format!("{retired_path} を作れませんでした"),
        };
    }
    if std::fs::write(&retired_file, raw.as_bytes()).is_err() {
        return InboxOutcome::Failed {
            detail: format!("{retired_path} を書けませんでした"),
        };
    }
    if std::fs::remove_file(root.join(&item.target)).is_err() {
        return InboxOutcome::Failed {
            detail: format!("{} を消せませんでした", item.target),
        };
    }
    if std::fs::remove_file(root.join(&item.path)).is_err() {
        return InboxOutcome::Failed {
            detail: format!("{} を消せませんでした", item.path),
        };
    }
    match commit_paths(
        root,
        &format!(
            "knowledge: retire {}（候補 {} を取り込む）",
            item.target, item.id
        ),
        (kb::HUMAN_AUTHOR_NAME, kb::HUMAN_AUTHOR_EMAIL),
        &[
            retired_path.as_str(),
            item.target.as_str(),
            item.path.as_str(),
        ],
    ) {
        Ok(sha) => {
            let _ = reindex(root);
            InboxOutcome::Accepted {
                etag: None,
                path: retired_path,
                sha,
            }
        }
        Err(detail) => InboxOutcome::Failed { detail },
    }
}

/// ADR-0047 D5: 候補を捨てる（`_inbox` から消してコミットする。git に履歴は残る）。
pub fn inbox_reject(root: &Path, id: &str) -> InboxOutcome {
    let Ok(path) = inbox_path(id) else {
        return InboxOutcome::Missing;
    };
    if !root.join(&path).exists() {
        return InboxOutcome::Missing;
    }
    if std::fs::remove_file(root.join(&path)).is_err() {
        return InboxOutcome::Failed {
            detail: format!("{path} を消せませんでした"),
        };
    }
    match commit_paths(
        root,
        &format!("knowledge: 候補 {path} を捨てる"),
        (kb::HUMAN_AUTHOR_NAME, kb::HUMAN_AUTHOR_EMAIL),
        &[path.as_str()],
    ) {
        Ok(sha) => InboxOutcome::Rejected { sha },
        Err(detail) => InboxOutcome::Failed { detail },
    }
}

// ---------------------------------------------------------------------------
// 候補の適用（ADR-0047 D4。Phase 62）。`crates/celeris` が知識整理 run の終端で 1 度だけ呼ぶ。
// ---------------------------------------------------------------------------

/// [`apply_candidates`] の結果。`task_core::KnowledgeRunSummary` と対になる件数を持つ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// KB へ直接コミットした KB 相対パス。
    pub committed: Vec<String>,
    /// `_inbox/` へ送った候補の KB 相対パス（`_inbox/<id>.md`）。
    pub inboxed: Vec<String>,
    /// 検査で落とした候補（元の `candidate.path` と理由）。
    pub dropped: Vec<(String, String)>,
}

impl ApplyOutcome {
    /// `knowledge_runs.summary_json` に書く形（Console・タイムラインが読む）。
    pub fn summary(&self) -> task_core::KnowledgeRunSummary {
        task_core::KnowledgeRunSummary {
            candidates: (self.committed.len() + self.inboxed.len() + self.dropped.len()) as u32,
            ingested: self.committed.len() as u32,
            inbox: self.inboxed.len() as u32,
            discarded: self.dropped.len() as u32,
        }
    }
}

/// ADR-0047 D4: 知識整理 run（`langmem` アダプタ）が書いた候補を適用する。
///
/// - 検査を通らない候補（path 境界・`.md`・題名/出典/本文なし・サイズ超過・秘密。
///   [`kb::validate_candidate`]）は**落とす**（どこにも書かない。`dropped` に理由を残す）。
/// - `confidence = high` かつ `op ∈ {create, update}` で、対象に人の未コミット編集が無ければ
///   KB へ**直接**コミットする（author [`kb::AGENT_AUTHOR_NAME`]、message
///   `knowledge: <op> <path> (task <task_id>)`。`update` は既存の `sources`/`created` を引き継ぐ）。
/// - それ以外（`merge`/`retire`/`medium`/`low`/人の編集と衝突/`create` なのに既にある/`update` なのに
///   まだ無い）は `_inbox/` へ（front matter に取り込み先 `path` と `op` を持たせる。人が GUI で
///   accept/reject する）。
///
/// 適用のあとに 1 度だけ [`reindex`] する。
pub fn apply_candidates(root: &Path, task_id: &str, candidates: &[kb::Candidate]) -> ApplyOutcome {
    let mut out = ApplyOutcome::default();
    for candidate in candidates {
        let path = match kb::validate_candidate(candidate) {
            Ok(p) => p,
            Err(e) => {
                out.dropped.push((candidate.path.clone(), e.to_string()));
                continue;
            }
        };
        let eligible = candidate.confidence == Confidence::High
            && candidate.op.direct_commit_eligible()
            && direct_commit_fits(root, &path, candidate.op);
        if eligible && commit_candidate_directly(root, task_id, candidate, &path).is_ok() {
            out.committed.push(path);
            continue;
        }
        match write_inbox_candidate(root, task_id, candidate, &path) {
            Ok(inbox_path) => out.inboxed.push(inbox_path),
            Err(detail) => out.dropped.push((candidate.path.clone(), detail)),
        }
    }
    let _ = reindex(root);
    out
}

/// 直接コミットしてよい形か: `create` は対象がまだ無いこと、`update` は対象があって
/// 人の未コミット編集が無いこと（[`has_uncommitted_changes`]）。それ以外の組は `_inbox/` へ。
fn direct_commit_fits(root: &Path, path: &str, op: kb::CandidateOp) -> bool {
    let exists = root.join(path).exists();
    match op {
        kb::CandidateOp::Create => !exists,
        kb::CandidateOp::Update => exists && !has_uncommitted_changes(root, path),
        kb::CandidateOp::Merge | kb::CandidateOp::Retire => false,
    }
}

/// 対象ページに人の未コミット編集（変更・未追跡）があるか。無ければ `false`（`git` が使えない
/// 環境でも安全側＝直接コミットを妨げない）。
fn has_uncommitted_changes(root: &Path, path: &str) -> bool {
    match git(root, &["status", "--porcelain", "--", path], GIT_TIMEOUT) {
        Some(o) if o.ok => !o.stdout.trim().is_empty(),
        _ => false,
    }
}

/// `confidence = high` の `create`/`update` を KB へ直接コミットする。
fn commit_candidate_directly(
    root: &Path,
    task_id: &str,
    candidate: &kb::Candidate,
    path: &str,
) -> Result<(), ()> {
    let current_etag = etag(root, path);
    let existing_front = read_page(root, path).map(|raw| kb::front_matter(&raw).0);
    let mut sources: Vec<String> = existing_front
        .as_ref()
        .map(|f| f.sources.clone())
        .unwrap_or_default();
    for s in &candidate.sources {
        let s = s.trim().to_string();
        if !s.is_empty() && !sources.contains(&s) {
            sources.push(s);
        }
    }
    let created = existing_front
        .as_ref()
        .and_then(|f| f.created.clone())
        .unwrap_or_else(today);
    let front = FrontMatter {
        title: Some(candidate.title.trim().to_string()),
        tags: candidate.tags.clone(),
        scope: Some(candidate.scope.trim().to_string()).filter(|s| !s.is_empty()),
        sources,
        created: Some(created),
        updated: Some(today()),
        confidence: Some(candidate.confidence),
        path: None,
        op: None,
    };
    let page = kb::render_page(&front, candidate.body.trim());
    let edit = PageEdit {
        path: path.to_string(),
        body: Some(page),
        etag: current_etag,
        message: format!("knowledge: {} {path} (task {task_id})", candidate.op),
        author: (
            kb::AGENT_AUTHOR_NAME.to_string(),
            kb::AGENT_AUTHOR_EMAIL.to_string(),
        ),
    };
    match commit_page(root, &edit) {
        WriteOutcome::Written { .. } => Ok(()),
        _ => Err(()),
    }
}

/// `_inbox/` へ候補を書く（[`record`] と同じファイル名の作り方。front matter に取り込み先 `path` と
/// `op` を持たせる）。
fn write_inbox_candidate(
    root: &Path,
    task_id: &str,
    candidate: &kb::Candidate,
    target_path: &str,
) -> Result<String, String> {
    let now = OffsetDateTime::now_utc();
    let stamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    let slug = kb::slugify(&candidate.title).unwrap_or_else(|| "candidate".to_string());
    let mut id = format!("{stamp}-{slug}");
    let mut n = 2;
    while root.join(INBOX_DIR).join(format!("{id}.md")).exists() {
        id = format!("{stamp}-{slug}-{n}");
        n += 1;
    }
    let path = format!("{INBOX_DIR}/{id}.md");
    let mut sources = candidate.sources.clone();
    let task_source = format!("task:{task_id}");
    if !sources.iter().any(|s| s == &task_source) {
        sources.push(task_source);
    }
    let front = FrontMatter {
        title: Some(candidate.title.trim().to_string()),
        tags: candidate.tags.clone(),
        scope: Some(candidate.scope.trim().to_string()).filter(|s| !s.is_empty()),
        sources,
        created: Some(today()),
        updated: Some(today()),
        confidence: Some(candidate.confidence),
        path: Some(target_path.to_string()),
        op: Some(candidate.op.as_str().to_string()),
    };
    let page = kb::render_page(&front, candidate.body.trim());
    let dir = root.join(INBOX_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{INBOX_DIR} を作れませんでした: {e}"))?;
    std::fs::write(dir.join(format!("{id}.md")), page.as_bytes())
        .map_err(|e| format!("{path} を書けませんでした: {e}"))?;
    commit_paths(
        root,
        &format!("knowledge: 候補 {path}（task {task_id}）"),
        (kb::AGENT_AUTHOR_NAME, kb::AGENT_AUTHOR_EMAIL),
        &[path.as_str()],
    )
    .map(|_| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("knowledge");
        init(&root).expect("init");
        (dir, root)
    }

    /// ADR-0047 D1: `init` は git のリポジトリ・骨組み・雛形・`index.json` を作り、**2 回目は何もしない**。
    #[test]
    fn init_creates_the_skeleton_and_is_idempotent() {
        let (_dir, root) = kb_dir();
        assert!(root.join(".git").exists());
        for d in kb::SKELETON_DIRS {
            assert!(root.join(d).is_dir(), "{d}");
        }
        assert!(root.join(INBOX_DIR).is_dir());
        assert!(root.join("README.md").exists());
        assert!(root.join("user/profile.md").exists());
        assert!(root.join("environment/clusters/pegasus.md").exists());
        assert!(root.join(INDEX_FILE).exists());
        // 索引は雛形を拾い、`_inbox` は入らない。
        let index = load_index(&root).expect("index");
        assert!(index.items.iter().any(|i| i.path == "user/profile.md"));
        assert!(!index.items.iter().any(|i| kb::is_inbox(&i.path)));
        assert!(
            index
                .items
                .iter()
                .any(|i| i.path == "environment/clusters/pegasus.md")
        );
        // `index.json` は派生物なので git には入れない。
        let tracked = git(&root, &["ls-files"], GIT_TIMEOUT)
            .expect("ls-files")
            .stdout;
        assert!(!tracked.contains(INDEX_FILE), "{tracked}");
        let commits_before = history(&root, "README.md").len();

        // 人が書き換えたページは 2 回目の `init` で上書きされない。
        std::fs::write(
            root.join("user/profile.md"),
            "---\ntitle: 私\n---\n\n# 私\n".as_bytes(),
        )
        .expect("write");
        let again = init(&root).expect("again");
        assert!(!again.created);
        assert!(again.added.is_empty(), "{again:?}");
        assert!(
            std::fs::read_to_string(root.join("user/profile.md"))
                .expect("read")
                .contains("# 私"),
        );
        assert_eq!(history(&root, "README.md").len(), commits_before);
    }

    /// 索引は front matter を読み、`scope` が無いページは置き場から決める。
    #[test]
    fn reindex_reads_front_matter_and_defaults_the_scope() {
        let (_dir, root) = kb_dir();
        std::fs::create_dir_all(root.join("projects/pluvio")).expect("mkdir");
        std::fs::write(
            root.join("projects/pluvio/design.md"),
            "# Pluvio の設計\n".as_bytes(),
        )
        .expect("write");
        std::fs::write(
            root.join("environment/tools/git.md"),
            "---\ntitle: git\ntags: [tool, vcs]\nconfidence: high\n---\n\n本文\n".as_bytes(),
        )
        .expect("write");
        let index = reindex(&root).expect("reindex");
        let design = index.get("projects/pluvio/design.md").expect("design");
        assert_eq!(design.scope.as_deref(), Some("project:pluvio"));
        assert_eq!(design.title, "Pluvio の設計");
        let tool = index.get("environment/tools/git.md").expect("tool");
        assert_eq!(tool.tags, vec!["tool", "vcs"]);
        assert_eq!(tool.scope.as_deref(), Some("environment"));
        assert_eq!(tool.confidence, Some(Confidence::High));
        assert!(!index.generated_at.is_empty());
        assert!(!index_is_stale(&root));
    }

    /// ADR-0047 D3: 検索は索引の tag / title と、本文の `git grep` を合わせる。
    #[test]
    fn search_finds_pages_by_tag_title_and_body() {
        let (_dir, root) = kb_dir();
        std::fs::write(
            root.join("environment/clusters/pegasus.md"),
            "---\ntitle: pegasus\ntags: [hpc, cluster]\nscope: environment\n---\n\npjsub で投げる。\n".as_bytes(),
        )
        .expect("write");
        std::fs::write(
            root.join("experience/2026-pjsub.md"),
            "---\ntitle: 計測の記録\nscope: experience\n---\n\npjsub の待ち行列が長い。\n"
                .as_bytes(),
        )
        .expect("write");
        reindex(&root).expect("reindex");

        let hits = search(&root, "cluster", None, 10);
        assert_eq!(
            hits.first().map(|h| h.item.path.as_str()),
            Some("environment/clusters/pegasus.md")
        );
        // 本文にしか無い語も当たる（`git grep`）。
        let body = search(&root, "pjsub", None, 10);
        let paths: Vec<&str> = body.iter().map(|h| h.item.path.as_str()).collect();
        assert!(paths.contains(&"experience/2026-pjsub.md"), "{paths:?}");
        assert!(
            paths.contains(&"environment/clusters/pegasus.md"),
            "{paths:?}"
        );
        // scope で絞る。
        let only = search(&root, "pjsub", Some("experience"), 10);
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].item.path, "experience/2026-pjsub.md");
        // limit。
        assert_eq!(search(&root, "pjsub", None, 1).len(), 1);
        assert!(grep(&root, "みつからないはず").is_empty());
    }

    /// ADR-0047 D3 / D4: `record` は `_inbox` に書き、`sources` が無ければ拒否、秘密も拒否。
    #[test]
    fn record_writes_a_candidate_and_refuses_secrets_and_missing_sources() {
        let (_dir, root) = kb_dir();
        let ok = record(
            &root,
            &RecordRequest {
                title: "pegasus の投げ方".into(),
                scope: "environment".into(),
                tags: vec!["hpc".into()],
                sources: vec!["task:01J1".into()],
                confidence: Some(Confidence::High),
                body: "pjsub -L node=1 で投げる。".into(),
                path: None,
            },
        )
        .expect("record");
        assert!(ok.path.starts_with("_inbox/"), "{ok:?}");
        let raw = std::fs::read_to_string(root.join(&ok.path)).expect("read");
        assert!(raw.contains("title: pegasus の投げ方"), "{raw}");
        assert!(raw.contains("sources: [\"task:01J1\"]"), "{raw}");
        assert!(raw.contains("confidence: high"), "{raw}");
        // 候補は索引に入らない。
        let index = reindex(&root).expect("reindex");
        assert!(!index.items.iter().any(|i| i.path == ok.path));
        // コミットされている。
        assert_eq!(history(&root, &ok.path).len(), 1);
        assert_eq!(history(&root, &ok.path)[0].author, kb::AGENT_AUTHOR_NAME);

        // `sources` が無ければエラー。
        let no_source = record(
            &root,
            &RecordRequest {
                title: "x".into(),
                scope: "user".into(),
                sources: vec![],
                body: "y".into(),
                ..RecordRequest::default()
            },
        );
        assert_eq!(no_source, Err(RecordError::NoSources));
        // 秘密は拒否（ADR-0047 D4）。
        let secret = record(
            &root,
            &RecordRequest {
                title: "鍵".into(),
                scope: "user".into(),
                sources: vec!["human".into()],
                body: "API キーは sk-abc123def456 です".into(),
                ..RecordRequest::default()
            },
        );
        assert!(matches!(secret, Err(RecordError::Secret(_))), "{secret:?}");
        assert_eq!(inbox_list(&root).len(), 1);
    }

    /// ADR-0047 D5: accept は正本へ移してコミットし、reject は捨てる。
    #[test]
    fn inbox_accept_and_reject_commit_to_git() {
        let (_dir, root) = kb_dir();
        let one = record(
            &root,
            &RecordRequest {
                title: "fern03 の使い方".into(),
                scope: "environment".into(),
                sources: vec!["human".into()],
                body: "ssh fern03。".into(),
                path: Some("environment/servers/fern03.md".into()),
                ..RecordRequest::default()
            },
        )
        .expect("record");
        let listed = inbox_list(&root);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, one.id);
        assert_eq!(listed[0].target, "environment/servers/fern03.md");
        assert_eq!(listed[0].sources, vec!["human"]);

        let accepted = inbox_accept(&root, &one.id, None, false);
        let path = match &accepted {
            InboxOutcome::Accepted { path, etag, .. } => {
                assert!(etag.is_some());
                path.clone()
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(path, "environment/servers/fern03.md");
        assert!(root.join(&path).exists());
        assert!(!root.join(&one.path).exists());
        let raw = std::fs::read_to_string(root.join(&path)).expect("read");
        // `_inbox` 専用の `path:` は落ちる。
        assert!(!raw.contains("\npath:"), "{raw}");
        assert!(raw.contains("title: fern03 の使い方"), "{raw}");
        assert!(load_index(&root).expect("index").get(&path).is_some());
        assert_eq!(history(&root, &path).len(), 1);
        assert_eq!(history(&root, &path)[0].author, kb::HUMAN_AUTHOR_NAME);
        // 同じ id はもう無い。
        assert_eq!(
            inbox_accept(&root, &one.id, None, false),
            InboxOutcome::Missing
        );

        // 宛先が既にあれば 409。
        let two = record(
            &root,
            &RecordRequest {
                title: "fern03 の使い方".into(),
                scope: "environment".into(),
                sources: vec!["human".into()],
                body: "別の版。".into(),
                path: Some("environment/servers/fern03.md".into()),
                ..RecordRequest::default()
            },
        )
        .expect("record");
        assert_eq!(
            inbox_accept(&root, &two.id, None, false),
            InboxOutcome::Exists {
                path: "environment/servers/fern03.md".into()
            }
        );
        // reject は捨てる。
        assert!(matches!(
            inbox_reject(&root, &two.id),
            InboxOutcome::Rejected { .. }
        ));
        assert!(inbox_list(&root).is_empty());
        assert_eq!(inbox_reject(&root, &two.id), InboxOutcome::Missing);
        // id の境界。
        assert_eq!(inbox_path("../../etc/passwd"), Err(PathError::Forbidden));
        assert_eq!(inbox_path("  "), Err(PathError::Empty));
    }

    /// 人の編集は `etag` で衝突を見て、1 件 1 コミットになる。
    #[test]
    fn commit_page_checks_the_etag_and_makes_one_commit_per_change() {
        let (_dir, root) = kb_dir();
        let human = (
            kb::HUMAN_AUTHOR_NAME.to_string(),
            kb::HUMAN_AUTHOR_EMAIL.to_string(),
        );
        let created = commit_page(
            &root,
            &PageEdit {
                path: "user/notes.md".into(),
                body: Some("# メモ\n".into()),
                etag: None,
                message: "knowledge: user/notes.md".into(),
                author: human.clone(),
            },
        );
        let tag = match created {
            WriteOutcome::Written {
                etag, unchanged, ..
            } => {
                assert!(!unchanged);
                etag.expect("etag")
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(
            read_page(&root, "user/notes.md").as_deref(),
            Some("# メモ\n")
        );
        // 既にあるのに etag 無しは 409。
        assert!(matches!(
            commit_page(
                &root,
                &PageEdit {
                    path: "user/notes.md".into(),
                    body: Some("# 別\n".into()),
                    etag: None,
                    message: "x".into(),
                    author: human.clone(),
                }
            ),
            WriteOutcome::EtagMismatch { .. }
        ));
        // 同じ中身なら新しいコミットは作らない。
        let same = commit_page(
            &root,
            &PageEdit {
                path: "user/notes.md".into(),
                body: Some("# メモ\n".into()),
                etag: Some(tag.clone()),
                message: "x".into(),
                author: human.clone(),
            },
        );
        assert!(
            matches!(
                same,
                WriteOutcome::Written {
                    unchanged: true,
                    ..
                }
            ),
            "{same:?}"
        );
        assert_eq!(history(&root, "user/notes.md").len(), 1);
        // 正しい etag なら通る。
        let updated = commit_page(
            &root,
            &PageEdit {
                path: "user/notes.md".into(),
                body: Some("# メモ\n\n続き\n".into()),
                etag: Some(tag),
                message: "knowledge: user/notes.md".into(),
                author: human.clone(),
            },
        );
        let next = match updated {
            WriteOutcome::Written { etag, .. } => etag.expect("etag"),
            other => panic!("{other:?}"),
        };
        assert_eq!(history(&root, "user/notes.md").len(), 2);
        // 削除。
        let deleted = commit_page(
            &root,
            &PageEdit {
                path: "user/notes.md".into(),
                body: None,
                etag: Some(next),
                message: "knowledge: remove".into(),
                author: human.clone(),
            },
        );
        assert!(
            matches!(deleted, WriteOutcome::Written { etag: None, .. }),
            "{deleted:?}"
        );
        assert!(read_page(&root, "user/notes.md").is_none());
        assert_eq!(
            commit_page(
                &root,
                &PageEdit {
                    path: "user/notes.md".into(),
                    body: None,
                    etag: None,
                    message: "x".into(),
                    author: human,
                }
            ),
            WriteOutcome::Missing
        );
    }

    /// 根の決め方（`--root` > `CELERIS_KNOWLEDGE_ROOT` > `[knowledge] root` > `~/knowledge`）。
    /// 環境変数は他のテストと干渉しないよう、この 1 本の中だけで設定する。
    #[test]
    fn the_root_comes_from_the_flag_then_the_env_then_the_config() {
        let explicit = PathBuf::from("/tmp/kb-explicit");
        let configured = PathBuf::from("/tmp/kb-config");
        assert_eq!(resolve_root(Some(&explicit), Some(&configured)), explicit);
        assert_eq!(resolve_root(None, Some(&configured)), configured);
        // 既定は `~/knowledge`（`~` は展開される）。
        let fallback = resolve_root(None, None);
        assert!(fallback.ends_with("knowledge"), "{}", fallback.display());
        assert!(
            !fallback.to_string_lossy().starts_with('~'),
            "{}",
            fallback.display()
        );
    }
}
