//! 文書（ADR-0044 D7。Phase 57）。**正本は git**（案件の primary リポジトリの `[outputs].docs` の下の
//! `*.md`）で、DB には何も持たない。
//!
//! - [`page_path`] — API が受け取ったパスをリポジトリ相対に正規化する（境界。`..`・絶対パス・`.md` 以外を弾く）
//! - [`list`] / [`grep`] — `git ls-tree` と `git grep -il`（default_branch の内容だけを見る）
//! - [`read_page`] / [`blob_sha`] / [`history`] — ページ 1 枚（`git show` / `rev-parse` / `log`）
//! - [`front_matter`] / [`render`] — front matter（最小の YAML もどき）と Markdown の描画（`pulldown-cmark`、生 HTML は捨てる）
//! - [`commit_page`] — 一時 worktree でコミットし default_branch を fast-forward（ADR-0043 D5 と同じ規則）
//! - [`init_docs_repo`] — 案件に git のリポジトリが無いときの既定の文書リポジトリ（`~/workspace/<slug>/`）
//!
//! ここも `task_ops::changes` と同じく **`git` を起こすだけ**で、判断も LLM も無い（DESIGN 原則 1）。
//! 子プロセスには全部待ち時間の上限がある。

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::changes::{GIT_TIMEOUT, GIT_WRITE_TIMEOUT, git};

/// ADR-0044 D7: 人の編集のコミットの author / committer。
pub const DOCS_AUTHOR_NAME: &str = "Celeris (human)";
pub const DOCS_AUTHOR_EMAIL: &str = "celeris@local";

/// ADR-0044 D7: ページの履歴は直近 20 件。
pub const HISTORY_LIMIT: usize = 20;
/// ツリーに出すページの上限（大きな案件で画面と git が詰まらないように）。
pub const MAX_TREE_ITEMS: usize = 2_000;
/// 1 ページの本文の上限（これを超えるページは `too_large`）。
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
/// 既定の文書リポジトリを作るときの最初のページ。
pub const INITIAL_PAGE: &str = "docs/README.md";

// ---------------------------------------------------------------------------
// パス（境界）
// ---------------------------------------------------------------------------

/// パスの検査の失敗（API は `Forbidden` を 403、`NotMarkdown` を 422 にする）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path must be relative and must not contain `..`")]
    Forbidden,
    #[error("a document page must be a Markdown file (*.md)")]
    NotMarkdown,
    #[error("path is required")]
    Empty,
}

/// 文書の根（`docs`）を `.`・`/` の揺れを取り除いた形にする。ルートそのものは `""`。
pub fn normalize_root(docs: &str) -> String {
    let trimmed = docs.trim().trim_start_matches("./").trim_matches('/');
    if trimmed == "." {
        String::new()
    } else {
        trimmed.to_string()
    }
}

/// API が受け取った `path` を**リポジトリ相対**に正規化する（ADR-0044 D7 の境界）。
///
/// - `..`・絶対パス・Windows の prefix は `Forbidden`（403）
/// - `.md` で終わらないものは `NotMarkdown`（422）
/// - 文書の根（`docs`）で始まっていなければ根の下だと解釈して前に付ける
///   （GUI の `?path=docs/x.md` も、ページの中の `[[x.md]]` も同じ 1 本の表記になる）
pub fn page_path(root: &str, raw: &str) -> Result<String, PathError> {
    let raw = raw.trim().trim_start_matches("./");
    if raw.is_empty() {
        return Err(PathError::Empty);
    }
    let path = Path::new(raw);
    let escapes = path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        });
    if escapes {
        return Err(PathError::Forbidden);
    }
    let cleaned = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if cleaned.is_empty() {
        return Err(PathError::Empty);
    }
    if !cleaned.to_ascii_lowercase().ends_with(".md") {
        return Err(PathError::NotMarkdown);
    }
    let root = normalize_root(root);
    if root.is_empty() || cleaned == root || cleaned.starts_with(&format!("{root}/")) {
        Ok(cleaned)
    } else {
        Ok(format!("{root}/{cleaned}"))
    }
}

/// `a/b/c.md` と `../d.md` → `a/d.md`（ページの中の `[[…]]` の解決。根の外には出ない）。
pub fn resolve_relative(root: &str, from: &str, link: &str) -> Option<String> {
    let base: Vec<&str> = from.split('/').collect();
    let mut parts: Vec<String> = base[..base.len().saturating_sub(1)]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    for part in link.trim().split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other.to_string()),
        }
    }
    let joined = parts.join("/");
    let root = normalize_root(root);
    if root.is_empty() || joined == root || joined.starts_with(&format!("{root}/")) {
        Some(joined)
    } else {
        None
    }
}

/// 題名から `*.md` のファイル名のもとになる slug を作る（ASCII だけ。何も残らなければ `None`）。
pub fn slugify(raw: &str) -> Option<String> {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    let out: String = out.chars().take(48).collect();
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { None } else { Some(out) }
}

/// ADR-0044 D7: 既定の文書リポジトリのディレクトリ名（案件の題名の slug。作れなければ案件の id）。
pub fn project_slug(title: &str, project_id: &str) -> String {
    match slugify(title) {
        Some(slug) => slug,
        None => project_id.to_ascii_lowercase(),
    }
}

// ---------------------------------------------------------------------------
// front matter（最小の YAML もどき）
// ---------------------------------------------------------------------------

/// ページの先頭の `---` … `---`（ADR-0044 D7。`title` / `tags` / `tasks` だけ読む）。
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct FrontMatter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// タスクの id（`celeris:task/<ULID>` ではなく素の ULID）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
}

/// front matter と、その後ろの本文を分ける（純粋関数）。
///
/// `serde_yaml` は入れない（ADR-0044 D7 が求めるのは `title` / `tags` / `tasks` の 3 つだけ）。
/// 読める形:
///
/// ```text
/// ---
/// title: 調べたこと
/// tags: [research, fs]
/// tasks:
///   - 01J...
/// ---
/// ```
pub fn front_matter(raw: &str) -> (FrontMatter, &str) {
    let body = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let Some(rest) = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))
    else {
        return (FrontMatter::default(), body);
    };
    let mut front = FrontMatter::default();
    let mut consumed = 0usize;
    let mut closed = false;
    let mut current: Option<String> = None;
    for line in rest.split_inclusive('\n') {
        consumed += line.len();
        let text = line.trim_end_matches(['\n', '\r']);
        if text.trim_end() == "---" || text.trim_end() == "..." {
            closed = true;
            break;
        }
        if let Some(item) = text.trim().strip_prefix("- ").map(str::trim) {
            match current.as_deref() {
                Some("tags") => front.tags.push(unquote(item)),
                Some("tasks") => front.tasks.push(unquote(item)),
                _ => {}
            }
            continue;
        }
        let Some((key, value)) = text.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "title" => {
                if !value.is_empty() {
                    front.title = Some(unquote(value));
                }
                current = None;
            }
            "tags" | "tasks" => {
                let items = parse_list(value);
                if key == "tags" {
                    front.tags.extend(items);
                } else {
                    front.tasks.extend(items);
                }
                current = Some(key);
            }
            _ => current = None,
        }
    }
    if !closed {
        // 閉じていない `---` は front matter ではない（本文の水平線）。
        return (FrontMatter::default(), body);
    }
    (front, &rest[consumed..])
}

/// `[a, b]` / 空（次の行から `- ` が続く）。
fn parse_list(value: &str) -> Vec<String> {
    let value = value.trim();
    let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) else {
        if value.is_empty() {
            return Vec::new();
        }
        return vec![unquote(value)];
    };
    inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(unquote)
        .collect()
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value);
    value.trim().to_string()
}

/// ページの題名（front matter の `title` → 本文の最初の `# ` → ファイル名）。
pub fn title_of(raw: &str, path: &str) -> String {
    let (front, body) = front_matter(raw);
    if let Some(title) = front.title.filter(|t| !t.trim().is_empty()) {
        return title;
    }
    for line in body.lines().take(50) {
        if let Some(heading) = line.trim_start().strip_prefix("# ") {
            let heading = heading.trim();
            if !heading.is_empty() {
                return heading.to_string();
            }
        }
    }
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// front matter を書き足す（昇格のとき。既にあれば `title` / `tasks` を混ぜる）。
pub fn merge_front_matter(raw: &str, title: Option<&str>, task_id: &str) -> String {
    let (mut front, body) = front_matter(raw);
    if front
        .title
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
        && let Some(title) = title.map(str::trim).filter(|t| !t.is_empty())
    {
        front.title = Some(title.to_string());
    }
    if !front.tasks.iter().any(|t| t == task_id) {
        front.tasks.push(task_id.to_string());
    }
    let mut out = String::from("---\n");
    if let Some(title) = &front.title {
        out.push_str(&format!("title: {}\n", yaml_scalar(title)));
    }
    if !front.tags.is_empty() {
        out.push_str(&format!(
            "tags: [{}]\n",
            front
                .tags
                .iter()
                .map(|t| yaml_scalar(t))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str(&format!(
        "tasks: [{}]\n",
        front
            .tasks
            .iter()
            .map(|t| yaml_scalar(t))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str("---\n");
    let body = body.trim_start_matches(['\n', '\r']);
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
    }
    out
}

/// YAML のスカラ（記号を含むなら引用する。最小限）。
fn yaml_scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && !value.starts_with([
            '-', '[', '{', '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`',
        ])
        && !value.contains([':', ',', '[', ']', '{', '}', '\n', '"']);
    if plain {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

// ---------------------------------------------------------------------------
// 描画（`pulldown-cmark`。生 HTML は捨てる）
// ---------------------------------------------------------------------------

/// Markdown を HTML にする（決定的。ADR-0044 D7: **生 HTML は捨てる**ので、結果はそのまま
/// GUI に渡してよい）。表・脚注・打ち消し線は有効。
///
/// リンクの書き換え（ADR-0044 D7）:
/// - `celeris:task/<ULID>` → `/tasks/<ULID>`
/// - `[[relative/path.md]]` → `<a href="<link_base>?path=…">…</a>`（`link_base` は GUI の文書タブの URL）
pub fn render(body: &str, root: &str, from: &str, link_base: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, Tag};

    let expanded = expand_wiki_links(body, root, from, link_base);
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(&expanded, options).filter_map(|event| match event {
        // 生 HTML は捨てる（`dangerouslySetInnerHTML` で描く側の安全はここが全部）。
        Event::Html(_) | Event::InlineHtml(_) => None,
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Link {
            link_type,
            dest_url: rewrite_link(&dest_url).into(),
            title,
            id,
        })),
        other => Some(other),
    });
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

/// `celeris:task/<ULID>` → `/tasks/<ULID>`。それ以外はそのまま。
fn rewrite_link(dest: &str) -> String {
    match dest.trim().strip_prefix("celeris:task/") {
        Some(id) if !id.is_empty() => format!("/tasks/{id}"),
        _ => dest.to_string(),
    }
}

/// `[[relative/path.md]]` と `[[relative/path.md|題名]]` を普通の Markdown のリンクに開く。
/// 根の外に出るものは開かない（文字のまま残す）。
fn expand_wiki_links(body: &str, root: &str, from: &str, link_base: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        let (before, after) = rest.split_at(start);
        out.push_str(before);
        let Some(end) = after.find("]]") else {
            out.push_str(after);
            return out;
        };
        let inner = &after[2..end];
        let (target, label) = match inner.split_once('|') {
            Some((t, l)) => (t.trim(), l.trim().to_string()),
            None => (inner.trim(), inner.trim().to_string()),
        };
        match resolve_relative(root, from, target)
            .filter(|_| !target.is_empty() && !target.contains(['\n', ']']))
        {
            Some(path) => out.push_str(&format!(
                "[{}]({}?path={})",
                escape_markdown(&label),
                link_base,
                urlencode(&path)
            )),
            None => out.push_str(&after[..end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

fn escape_markdown(text: &str) -> String {
    text.replace('[', "\\[").replace(']', "\\]")
}

/// クエリに載せるための最小の percent encoding（英数字と `-._~/` 以外を逃がす）。
pub fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~' | '/') {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 読み取り（default_branch の中身だけを見る）
// ---------------------------------------------------------------------------

/// コミット 1 件（ツリーの「最終コミット」とページの履歴）。
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct DocCommit {
    pub sha: String,
    /// RFC 3339（`%aI`）。
    pub at: String,
    pub author: String,
    pub subject: String,
}

/// 区切り（`git log --format` の中で使う。パスにもコミットメッセージにも出ない）。
const UNIT: char = '\u{1f}';
const RECORD: char = '\u{1e}';

fn branch_ref(default_branch: &str) -> String {
    format!("refs/heads/{default_branch}")
}

/// その default_branch が存在するか。
pub fn branch_exists(repo: &Path, default_branch: &str) -> bool {
    git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &branch_ref(default_branch),
        ],
        GIT_TIMEOUT,
    )
    .is_some_and(|o| o.ok)
}

/// 文書の根にある `*.md` のパス（リポジトリ相対。並びは git の順 = 名前順）。
pub fn list(repo: &Path, default_branch: &str, root: &str) -> Vec<String> {
    let reference = branch_ref(default_branch);
    let root = normalize_root(root);
    let mut args: Vec<&str> = vec!["ls-tree", "-r", "--name-only", "-z", &reference];
    if !root.is_empty() {
        args.push("--");
        args.push(&root);
    }
    let Some(out) = git(repo, &args, GIT_TIMEOUT).filter(|o| o.ok) else {
        return Vec::new();
    };
    out.stdout
        .split('\0')
        .map(str::trim)
        .filter(|p| !p.is_empty() && p.to_ascii_lowercase().ends_with(".md"))
        .take(MAX_TREE_ITEMS)
        .map(str::to_string)
        .collect()
}

/// ADR-0044 D7 の `?q=`: 文書の根の中を `git grep -il`（大文字小文字を区別しない固定文字列）。
pub fn grep(repo: &Path, default_branch: &str, root: &str, needle: &str) -> Vec<String> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Vec::new();
    }
    let reference = branch_ref(default_branch);
    let root = normalize_root(root);
    let mut args: Vec<&str> = vec!["grep", "-I", "-i", "-l", "-F", "-e", needle, &reference];
    if !root.is_empty() {
        args.push("--");
        args.push(&root);
    }
    // 見つからなければ exit 1（エラーではない）。
    let Some(out) = git(repo, &args, GIT_TIMEOUT) else {
        return Vec::new();
    };
    let prefix = format!("{reference}:");
    out.stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            l.strip_prefix(&prefix)
                .or_else(|| l.split_once(':').map(|(_, p)| p))
        })
        .filter(|p| p.to_ascii_lowercase().ends_with(".md"))
        .take(MAX_TREE_ITEMS)
        .map(str::to_string)
        .collect()
}

/// 文書の根の下のページごとの**最後のコミット**（`git log --name-only` を 1 回だけ起こす）。
pub fn last_commits(repo: &Path, default_branch: &str, root: &str) -> HashMap<String, DocCommit> {
    let reference = branch_ref(default_branch);
    let root = normalize_root(root);
    // `--format=<fmt>` は **1 つの引数**で渡す（`--format <fmt>` は git が revision と読む）。
    let format = format!("--format={RECORD}%H{UNIT}%aI{UNIT}%an{UNIT}%s");
    let mut args: Vec<&str> = vec!["log", "--no-merges", "--name-only", &format, &reference];
    if !root.is_empty() {
        args.push("--");
        args.push(&root);
    }
    let mut out = HashMap::new();
    let Some(result) = git(repo, &args, GIT_TIMEOUT).filter(|o| o.ok) else {
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

/// ページ 1 枚の履歴（新しい順、直近 [`HISTORY_LIMIT`] 件）。
pub fn history(repo: &Path, default_branch: &str, path: &str) -> Vec<DocCommit> {
    let reference = branch_ref(default_branch);
    let format = format!("--format=%H{UNIT}%aI{UNIT}%an{UNIT}%s");
    let limit = format!("-{HISTORY_LIMIT}");
    let Some(out) = git(
        repo,
        &["log", &limit, &format, &reference, "--", path],
        GIT_TIMEOUT,
    )
    .filter(|o| o.ok) else {
        return Vec::new();
    };
    out.stdout.lines().filter_map(parse_commit).collect()
}

/// ページの中身（default_branch の blob）。無ければ `None`。
pub fn read_page(repo: &Path, default_branch: &str, path: &str) -> Option<String> {
    let target = format!("{}:{path}", branch_ref(default_branch));
    let out = git(repo, &["show", &target], GIT_TIMEOUT)?;
    if !out.ok {
        return None;
    }
    Some(out.stdout)
}

/// ページの blob sha（`etag`）。無ければ `None`。
pub fn blob_sha(repo: &Path, default_branch: &str, path: &str) -> Option<String> {
    let target = format!("{}:{path}", branch_ref(default_branch));
    let out = git(
        repo,
        &["rev-parse", "--verify", "--quiet", &target],
        GIT_TIMEOUT,
    )?;
    if !out.ok {
        return None;
    }
    let sha = out.stdout.trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

// ---------------------------------------------------------------------------
// 書き込み（一時 worktree でコミットし default_branch を fast-forward）
// ---------------------------------------------------------------------------

/// 書き込みの結果（ADR-0044 D7）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// コミットした（`sha` は新しいコミット、`etag` は書いた後の blob sha。削除なら `None`）。
    Written {
        sha: String,
        etag: Option<String>,
        /// 人のチェックアウトも早送りした。
        fast_forwarded: bool,
        /// 中身が同じだったのでコミットしなかった。
        unchanged: bool,
    },
    /// `etag` が現在の blob と違う（409）。`etag` はいまの値。
    EtagMismatch { etag: Option<String> },
    /// 人のチェックアウトが default_branch を編集中（409。ADR-0043 D5 と同じ規則）。
    Busy { detail: String },
    /// 消そうとしたページが無い（404）。
    Missing,
    /// git が失敗した。
    Failed { detail: String },
}

/// 1 回の書き込み（作成・更新・削除）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEdit {
    /// リポジトリ相対のパス（[`page_path`] を通したもの）。
    pub path: String,
    /// 本文。`None` なら削除。
    pub body: Option<String>,
    /// 期待する blob sha。新規作成のときだけ `None` でよい。
    pub etag: Option<String>,
    /// コミットメッセージ。
    pub message: String,
    /// `etag` の照合をしない（昇格の `overwrite: true`。宛先があってよいかは呼び出し側が決める）。
    pub overwrite: bool,
}

/// ADR-0044 D7: 人の編集を default_branch に直接コミットする。
///
/// 1. `etag` を照合（違えば 409。ページがあるのに `etag` が無いのも 409）
/// 2. 人のチェックアウトが default_branch を出していて dirty なら 409（ADR-0043 D5 と同じ規則）
/// 3. 一時 worktree（detach）で書き、`Celeris (human)` としてコミット
/// 4. default_branch を進める（人が出していて綺麗なら `merge --ff-only`、そうでなければ `update-ref`）
pub fn commit_page(
    repo: &Path,
    default_branch: &str,
    temp_dir: &Path,
    edit: &PageEdit,
) -> WriteOutcome {
    if !branch_exists(repo, default_branch) {
        return WriteOutcome::Failed {
            detail: format!("既定のブランチ {default_branch} がありません"),
        };
    }
    let current = blob_sha(repo, default_branch, &edit.path);
    if edit.body.is_none() && current.is_none() {
        return WriteOutcome::Missing;
    }
    // `overwrite`（昇格）は照合しない。それ以外は「ページがあるのに `etag` が無い」も 409。
    if !edit.overwrite && edit.etag.as_deref() != current.as_deref() {
        return WriteOutcome::EtagMismatch { etag: current };
    }

    let reference = branch_ref(default_branch);
    let Some(default_sha) = git(
        repo,
        &["rev-parse", "--verify", "--quiet", &reference],
        GIT_TIMEOUT,
    )
    .filter(|o| o.ok)
    .map(|o| o.stdout.trim().to_string())
    .filter(|s| !s.is_empty()) else {
        return WriteOutcome::Failed {
            detail: format!("既定のブランチ {default_branch} を読めませんでした"),
        };
    };
    let checked_out = crate::changes::current_branch(repo).is_some_and(|b| b == default_branch);
    if checked_out && crate::changes::is_dirty(repo) != Some(false) {
        return WriteOutcome::Busy {
            detail: format!("{default_branch} が編集中"),
        };
    }

    if let Some(parent) = temp_dir.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return WriteOutcome::Failed {
            detail: "一時 worktree を作れませんでした".to_string(),
        };
    }
    let temp = temp_dir.to_string_lossy().into_owned();
    match git(
        repo,
        &["worktree", "add", "--detach", &temp, &reference],
        GIT_WRITE_TIMEOUT,
    ) {
        Some(o) if o.ok => {}
        Some(o) => {
            return WriteOutcome::Failed {
                detail: format!("一時 worktree を作れませんでした: {}", o.why()),
            };
        }
        None => {
            return WriteOutcome::Failed {
                detail: "git を起動できませんでした".to_string(),
            };
        }
    }
    let outcome = write_and_advance(
        repo,
        temp_dir,
        default_branch,
        &default_sha,
        checked_out,
        edit,
    );
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

fn write_and_advance(
    repo: &Path,
    temp_dir: &Path,
    default_branch: &str,
    default_sha: &str,
    checked_out: bool,
    edit: &PageEdit,
) -> WriteOutcome {
    let target = temp_dir.join(&edit.path);
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
    if !git(
        temp_dir,
        &["add", "-A", "--", &edit.path],
        GIT_WRITE_TIMEOUT,
    )
    .is_some_and(|o| o.ok)
    {
        return WriteOutcome::Failed {
            detail: format!("{} を git に載せられませんでした", edit.path),
        };
    }
    // 中身が同じなら何もコミットしない（`git commit` は「何も変わっていない」で失敗する）。
    let nothing =
        git(temp_dir, &["diff", "--cached", "--quiet"], GIT_TIMEOUT).is_some_and(|o| o.ok);
    if nothing {
        return WriteOutcome::Written {
            sha: default_sha.to_string(),
            etag: blob_sha(repo, default_branch, &edit.path),
            fast_forwarded: false,
            unchanged: true,
        };
    }
    let author = format!("user.name={DOCS_AUTHOR_NAME}");
    let email = format!("user.email={DOCS_AUTHOR_EMAIL}");
    match git(
        temp_dir,
        &[
            "-c",
            &author,
            "-c",
            &email,
            "commit",
            "-q",
            "-m",
            &edit.message,
        ],
        GIT_WRITE_TIMEOUT,
    ) {
        Some(o) if o.ok => {}
        Some(o) => {
            return WriteOutcome::Failed {
                detail: format!("コミットできませんでした: {}", o.why()),
            };
        }
        None => {
            return WriteOutcome::Failed {
                detail: "git を起動できませんでした".to_string(),
            };
        }
    }
    let Some(new_sha) = git(temp_dir, &["rev-parse", "HEAD"], GIT_TIMEOUT)
        .filter(|o| o.ok)
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return WriteOutcome::Failed {
            detail: "コミットの結果を読めませんでした".to_string(),
        };
    };
    let advanced = if checked_out {
        git(repo, &["merge", "--ff-only", &new_sha], GIT_WRITE_TIMEOUT)
    } else {
        let reference = branch_ref(default_branch);
        git(
            repo,
            &["update-ref", &reference, &new_sha, default_sha],
            GIT_WRITE_TIMEOUT,
        )
    };
    match advanced {
        Some(o) if o.ok => WriteOutcome::Written {
            etag: blob_sha(repo, default_branch, &edit.path),
            sha: new_sha,
            fast_forwarded: checked_out,
            unchanged: false,
        },
        Some(o) => WriteOutcome::Failed {
            detail: format!("{default_branch} を進められませんでした: {}", o.why()),
        },
        None => WriteOutcome::Failed {
            detail: "git を起動できませんでした".to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// 既定の文書リポジトリ（ADR-0044 D7）
// ---------------------------------------------------------------------------

/// `~/workspace/<slug>/` に文書リポジトリを作る（`git init -b main` + `docs/README.md` の最初のコミット）。
/// 既に git のリポジトリならそのまま使う（**中身は触らない**）。
pub fn init_docs_repo(dir: &Path, project_title: &str) -> Result<(), String> {
    if dir.join(".git").exists() {
        return Ok(());
    }
    if dir.exists()
        && std::fs::read_dir(dir)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(true)
    {
        return Err(format!("{} は空ではありません", dir.display()));
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("{} を作れませんでした: {e}", dir.display()))?;
    match git(dir, &["init", "-q", "-b", "main"], GIT_WRITE_TIMEOUT) {
        Some(o) if o.ok => {}
        Some(o) => return Err(format!("git init に失敗しました: {}", o.why())),
        None => return Err("git を起動できませんでした".to_string()),
    }
    let readme = dir.join(INITIAL_PAGE);
    if let Some(parent) = readme.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("docs/ を作れませんでした: {e}"))?;
    }
    let title = project_title.trim();
    let title = if title.is_empty() { "文書" } else { title };
    let body = format!(
        "# {title}\n\nこの案件の文書はここ（`docs/`）に Markdown で置く。\
         題名は 1 行目の `# `、タスクとの紐付けは front matter の `tasks: [<タスク id>]`。\n"
    );
    std::fs::write(&readme, body.as_bytes())
        .map_err(|e| format!("{INITIAL_PAGE} を書けませんでした: {e}"))?;
    if !git(dir, &["add", "-A"], GIT_WRITE_TIMEOUT).is_some_and(|o| o.ok) {
        return Err("git add に失敗しました".to_string());
    }
    let author = format!("user.name={DOCS_AUTHOR_NAME}");
    let email = format!("user.email={DOCS_AUTHOR_EMAIL}");
    match git(
        dir,
        &[
            "-c",
            &author,
            "-c",
            &email,
            "commit",
            "-q",
            "-m",
            "docs: 文書リポジトリを作る",
        ],
        GIT_WRITE_TIMEOUT,
    ) {
        Some(o) if o.ok => Ok(()),
        Some(o) => Err(format!("最初のコミットに失敗しました: {}", o.why())),
        None => Err("git を起動できませんでした".to_string()),
    }
}

/// 既定の文書リポジトリの置き場（`<base>/<slug>`。既にあって git でなければ `<slug>-<id の末尾 8>`）。
pub fn docs_repo_dir(base: &Path, title: &str, project_id: &str) -> PathBuf {
    let slug = project_slug(title, project_id);
    let first = base.join(&slug);
    if !first.exists() || first.join(".git").exists() {
        return first;
    }
    let tail: String = project_id
        .to_ascii_lowercase()
        .chars()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    base.join(format!("{slug}-{tail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_paths_are_confined_to_the_docs_root() {
        assert_eq!(page_path("docs", "docs/a/b.md").expect("ok"), "docs/a/b.md");
        // 根を書かなければ根の下だと解釈する。
        assert_eq!(page_path("docs", "a/b.md").expect("ok"), "docs/a/b.md");
        assert_eq!(page_path("docs", "./a.md").expect("ok"), "docs/a.md");
        // 根が違えばその根に入る。
        assert_eq!(page_path("doc", "docs/a.md").expect("ok"), "doc/docs/a.md");
        // 根がリポジトリのルートなら何でもよい。
        assert_eq!(page_path(".", "a.md").expect("ok"), "a.md");
        for bad in ["../x.md", "/etc/x.md", "docs/../../x.md"] {
            assert_eq!(page_path("docs", bad), Err(PathError::Forbidden), "{bad}");
        }
        assert_eq!(page_path("docs", "a.txt"), Err(PathError::NotMarkdown));
        assert_eq!(page_path("docs", "  "), Err(PathError::Empty));
        // 大文字の拡張子も Markdown。
        assert_eq!(page_path("docs", "A.MD").expect("ok"), "docs/A.MD");
    }

    #[test]
    fn relative_links_stay_inside_the_docs_root() {
        assert_eq!(
            resolve_relative("docs", "docs/a/b.md", "../c.md").as_deref(),
            Some("docs/c.md")
        );
        assert_eq!(
            resolve_relative("docs", "docs/a.md", "b.md").as_deref(),
            Some("docs/b.md")
        );
        assert_eq!(resolve_relative("docs", "docs/a.md", "../../etc.md"), None);
        assert_eq!(resolve_relative("docs", "docs/a.md", "../out.md"), None);
    }

    #[test]
    fn slugs_fall_back_to_the_project_id() {
        assert_eq!(slugify("Pluvio PoC").as_deref(), Some("pluvio-poc"));
        assert_eq!(slugify("調査").as_deref(), None);
        assert_eq!(project_slug("調査", "01JABCDEF"), "01jabcdef");
        assert_eq!(project_slug("BenchFS Paper", "01J"), "benchfs-paper");
    }

    #[test]
    fn front_matter_reads_title_tags_and_tasks() {
        let raw = "---\ntitle: 調べたこと\ntags: [research, fs]\ntasks:\n  - 01J1\n  - \"01J2\"\n---\n\n# 別の題名\n本文\n";
        let (front, body) = front_matter(raw);
        assert_eq!(front.title.as_deref(), Some("調べたこと"));
        assert_eq!(front.tags, vec!["research", "fs"]);
        assert_eq!(front.tasks, vec!["01J1", "01J2"]);
        assert!(body.starts_with("\n# 別の題名"), "{body:?}");
        // front matter の `title` が勝つ。
        assert_eq!(title_of(raw, "docs/x.md"), "調べたこと");
        // 無ければ 1 行目の `# `。
        assert_eq!(title_of("# 題名\n\n本文\n", "docs/x.md"), "題名");
        // それも無ければファイル名。
        assert_eq!(title_of("本文だけ\n", "docs/a/x.md"), "x.md");
        // 閉じていない `---` は front matter ではない。
        let (front, body) = front_matter("---\ntitle: x\n# 本文\n");
        assert_eq!(front, FrontMatter::default());
        assert!(body.starts_with("---"));
    }

    #[test]
    fn merging_front_matter_keeps_what_is_there_and_adds_the_task() {
        let merged = merge_front_matter("# 答え\n\n本文\n", Some("答え"), "01J1");
        assert!(
            merged.starts_with("---\ntitle: 答え\ntasks: [01J1]\n---\n"),
            "{merged}"
        );
        assert!(merged.ends_with("# 答え\n\n本文\n"), "{merged}");
        // 既に front matter があれば混ぜる（同じタスクは 2 回足さない）。
        let again = merge_front_matter(&merged, Some("別"), "01J1");
        assert_eq!(again.matches("01J1").count(), 1, "{again}");
        assert!(again.contains("title: 答え"), "{again}");
        let two = merge_front_matter(&merged, None, "01J2");
        assert!(two.contains("tasks: [01J1, 01J2]"), "{two}");
        // `tags` は残る。
        let tagged = merge_front_matter("---\ntags: [a]\n---\n本文\n", Some("題"), "01J3");
        assert!(
            tagged.contains("tags: [a]") && tagged.contains("tasks: [01J3]"),
            "{tagged}"
        );
    }

    #[test]
    fn rendering_drops_raw_html_and_links_tasks_and_pages() {
        let html = render(
            "# 題\n\n<script>alert(1)</script>\n\n[t](celeris:task/01J1) と [[sub/other.md]] と [[../out.md]]\n\n\
             | a | b |\n| --- | --- |\n| 1 | 2 |\n",
            "docs",
            "docs/index.md",
            "/projects/P1/docs",
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(
            html.contains("alert(1)") || !html.contains("script"),
            "{html}"
        );
        assert!(html.contains("href=\"/tasks/01J1\""), "{html}");
        assert!(
            html.contains("/projects/P1/docs?path=docs/sub/other.md"),
            "{html}"
        );
        // 根の外に出る `[[…]]` はリンクにしない。
        assert!(html.contains("[[../out.md]]"), "{html}");
        assert!(html.contains("<table>"), "{html}");
    }

    /// ここから下は git を起こす（tempdir のリポジトリだけ。ネットワークには出ない）。
    fn repo_with_docs() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_docs_repo(&repo, "Pluvio PoC").expect("init");
        dir
    }

    #[test]
    fn a_new_docs_repository_has_one_page_on_main() {
        let dir = repo_with_docs();
        let repo = dir.path().join("repo");
        assert!(branch_exists(&repo, "main"));
        assert_eq!(list(&repo, "main", "docs"), vec![INITIAL_PAGE.to_string()]);
        let raw = read_page(&repo, "main", INITIAL_PAGE).expect("page");
        assert!(raw.starts_with("# Pluvio PoC"), "{raw}");
        assert!(blob_sha(&repo, "main", INITIAL_PAGE).is_some());
        let log = history(&repo, "main", INITIAL_PAGE);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].author, DOCS_AUTHOR_NAME);
        // 2 回目は何もしない（既にある git リポジトリ）。
        init_docs_repo(&repo, "Pluvio PoC").expect("again");
        assert_eq!(history(&repo, "main", INITIAL_PAGE).len(), 1);
    }

    #[test]
    fn committing_a_page_checks_the_etag_and_moves_the_default_branch() {
        let dir = repo_with_docs();
        let repo = dir.path().join("repo");
        let temp = dir.path().join("tmp/w1");

        // 新規作成（etag なし）。
        let created = commit_page(
            &repo,
            "main",
            &temp,
            &PageEdit {
                path: "docs/a.md".into(),
                body: Some("# A\n".into()),
                etag: None,
                message: "docs: docs/a.md".into(),
                overwrite: false,
            },
        );
        let etag = match created {
            WriteOutcome::Written {
                etag, unchanged, ..
            } => {
                assert!(!unchanged);
                etag.expect("etag")
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(
            read_page(&repo, "main", "docs/a.md").as_deref(),
            Some("# A\n")
        );

        // 既にあるのに etag が無ければ 409。
        assert!(matches!(
            commit_page(
                &repo,
                "main",
                &dir.path().join("tmp/w2"),
                &PageEdit {
                    path: "docs/a.md".into(),
                    body: Some("# B\n".into()),
                    etag: None,
                    message: "docs: docs/a.md".into(),
                    overwrite: false,
                }
            ),
            WriteOutcome::EtagMismatch { .. }
        ));
        // 違う etag も 409。
        assert!(matches!(
            commit_page(
                &repo,
                "main",
                &dir.path().join("tmp/w3"),
                &PageEdit {
                    path: "docs/a.md".into(),
                    body: Some("# B\n".into()),
                    etag: Some("0".repeat(40)),
                    message: "docs: docs/a.md".into(),
                    overwrite: false,
                }
            ),
            WriteOutcome::EtagMismatch { .. }
        ));
        // 正しい etag なら通り、etag は変わる。
        let updated = commit_page(
            &repo,
            "main",
            &dir.path().join("tmp/w4"),
            &PageEdit {
                path: "docs/a.md".into(),
                body: Some("# B\n".into()),
                etag: Some(etag.clone()),
                message: "docs: docs/a.md".into(),
                overwrite: false,
            },
        );
        let next = match updated {
            WriteOutcome::Written { etag, .. } => etag.expect("etag"),
            other => panic!("{other:?}"),
        };
        assert_ne!(next, etag);
        assert_eq!(history(&repo, "main", "docs/a.md").len(), 2);

        // 削除。
        let deleted = commit_page(
            &repo,
            "main",
            &dir.path().join("tmp/w5"),
            &PageEdit {
                path: "docs/a.md".into(),
                body: None,
                etag: Some(next),
                message: "docs: remove docs/a.md".into(),
                overwrite: false,
            },
        );
        assert!(
            matches!(deleted, WriteOutcome::Written { etag: None, .. }),
            "{deleted:?}"
        );
        assert!(read_page(&repo, "main", "docs/a.md").is_none());
        // 無いものは消せない。
        assert_eq!(
            commit_page(
                &repo,
                "main",
                &dir.path().join("tmp/w6"),
                &PageEdit {
                    path: "docs/a.md".into(),
                    body: None,
                    etag: None,
                    message: "docs: remove".into(),
                    overwrite: false,
                }
            ),
            WriteOutcome::Missing
        );
    }

    /// ADR-0043 D5 と同じ規則: 人が default_branch を編集中なら 409（何も触らない）。
    #[test]
    fn writing_while_the_default_branch_is_dirty_is_busy() {
        let dir = repo_with_docs();
        let repo = dir.path().join("repo");
        std::fs::write(repo.join("docs/README.md"), b"human is editing\n").expect("write");
        let outcome = commit_page(
            &repo,
            "main",
            &dir.path().join("tmp/w"),
            &PageEdit {
                path: "docs/a.md".into(),
                body: Some("# A\n".into()),
                etag: None,
                message: "docs: docs/a.md".into(),
                overwrite: false,
            },
        );
        assert!(matches!(outcome, WriteOutcome::Busy { .. }), "{outcome:?}");
        assert!(read_page(&repo, "main", "docs/a.md").is_none());
    }

    #[test]
    fn the_tree_lists_markdown_and_grep_finds_pages() {
        let dir = repo_with_docs();
        let repo = dir.path().join("repo");
        for (path, body) in [
            ("docs/one.md", "# ひとつ\n調査の結果\n"),
            ("docs/sub/two.md", "# ふたつ\nnothing here\n"),
            ("docs/not-a-page.txt", "調査\n"),
        ] {
            let outcome = commit_page(
                &repo,
                "main",
                &dir.path().join(format!("tmp/{}", path.replace('/', "-"))),
                &PageEdit {
                    path: path.into(),
                    body: Some(body.into()),
                    etag: None,
                    message: format!("docs: {path}"),
                    overwrite: false,
                },
            );
            assert!(
                matches!(outcome, WriteOutcome::Written { .. }),
                "{path}: {outcome:?}"
            );
        }
        let mut paths = list(&repo, "main", "docs");
        paths.sort();
        assert_eq!(
            paths,
            vec!["docs/README.md", "docs/one.md", "docs/sub/two.md"]
        );
        assert_eq!(grep(&repo, "main", "docs", "調査"), vec!["docs/one.md"]);
        assert!(grep(&repo, "main", "docs", "見つからない").is_empty());
        let last = last_commits(&repo, "main", "docs");
        assert_eq!(
            last.get("docs/one.md").map(|c| c.subject.as_str()),
            Some("docs: docs/one.md")
        );
        assert!(last.get("docs/one.md").is_some_and(|c| c.at.contains('T')));
    }
}
