//! 知識ベース（ADR-0047。Phase 61）— **正本はローカルの Markdown**。
//!
//! ここは**純粋関数とデータだけ**（I/O も git も LLM も無い。DESIGN 原則 1 / ADR-0001 D2）。
//! ファイルを触るのは [`task_ops::knowledge`]、HTTP は `task_api::knowledge`、CLI は
//! `celerisctl knowledge`。
//!
//! - [`front_matter`] / [`render_page`] — front matter（最小の YAML もどき。`title` / `tags` /
//!   `scope` / `sources` / `created` / `updated` / `confidence`）の読み書き。往復できる
//! - [`page_path`] — KB の根の外に出られないパスの正規化（境界。`..`・絶対パス・`.md` 以外を弾く）
//! - [`Index`] / [`IndexItem`] — `index.json`（**再生成できる派生物**。正本はファイル）
//! - [`search`] — 順位は「tag 一致数 → title 一致 → `updated` の新しさ」（ADR-0047 D3）
//! - [`KnowledgeMount`] — ADR-0046 D1 の `profile.knowledge`（`kb` / `repo` / `dir` / `memory`）
//! - [`secret_finding`] — ADR-0047 D4 の「秘密を保存しない」決定的な検査（Phase 61 で先に入れる）

use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// ADR-0047 D1: `[knowledge] root` を設定しなかったときの正本の置き場。
pub const DEFAULT_ROOT: &str = "~/.local/share/celeris/knowledge";

/// 既定の根。XDG Base Directory に従う: `$XDG_DATA_HOME/celeris/knowledge`（絶対パスのときだけ採る）、無ければ
/// `~/.local/share/celeris/knowledge`（人の指示 2026-09-20: ホームの直下に `knowledge/` を置かない。知識は人が持つ
/// **データ**なので XDG_DATA_HOME）。
pub fn default_root() -> std::path::PathBuf {
    match std::env::var_os("XDG_DATA_HOME").map(std::path::PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir.join("celeris").join("knowledge"),
        _ => std::path::PathBuf::from(DEFAULT_ROOT),
    }
}
/// ADR-0047 D1: 候補の置き場（索引には入らない）。
pub const INBOX_DIR: &str = "_inbox";
/// ADR-0047 D4（Phase 62）: `op = retire` を accept したときの行き先（索引にも検索にも入らない）。
pub const RETIRED_DIR: &str = "_retired";
/// ADR-0047 D1: 派生物の索引。
pub const INDEX_FILE: &str = "index.json";
/// ADR-0047 D2: 前置きに出す索引の上限。
pub const MAX_PREAMBLE_ITEMS: usize = 200;
/// 索引に入れるページ数の上限（KB が壊れていても画面と前置きが詰まらないように）。
pub const MAX_INDEX_ITEMS: usize = 5_000;
/// 1 ページの本文の上限（これを超えるページは `too_large`。`task_ops::docs` と同じ）。
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
/// ページの履歴は直近 20 件（`task_ops::docs::HISTORY_LIMIT` と同じ）。
pub const HISTORY_LIMIT: usize = 20;
/// 人の編集のコミットの author / committer（ADR-0044 D7 と同じ）。
pub const HUMAN_AUTHOR_NAME: &str = "Celeris (human)";
pub const HUMAN_AUTHOR_EMAIL: &str = "celeris@local";
/// ADR-0047 D4: 知識整理 run が直接コミットするときの author（Phase 62 で使う。ここでは `record` の
/// 候補にだけ使われる）。
pub const AGENT_AUTHOR_NAME: &str = "Celeris (knowledge)";
pub const AGENT_AUTHOR_EMAIL: &str = "celeris@local";

/// ADR-0047 D1 の骨組み（`init` が作るディレクトリ）。
pub const SKELETON_DIRS: [&str; 7] = [
    "user",
    "environment",
    "environment/clusters",
    "environment/servers",
    "environment/tools",
    "projects",
    "experience",
];

// ---------------------------------------------------------------------------
// front matter（最小の YAML もどき。`serde_yaml` は入れない）
// ---------------------------------------------------------------------------

/// ADR-0047 D1 の front matter。知らない鍵は読まない（書くときも落ちる）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FrontMatter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// `user` / `environment` / `project:<slug>` / `experience`（ADR-0047 D1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `task:<id>` / `message:<id>` / `human` / `url:<…>`。`record` では必須。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    /// **`_inbox/` の候補にだけ意味がある**（ADR-0047 D4）: 取り込む先の KB 相対パス。
    /// accept はこれ（無ければ `scope` と `title` から決めた既定）へ移す。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// **`_inbox/` の候補にだけ意味がある**（ADR-0047 D4。Phase 62）: `create` / `update` / `merge` /
    /// `retire`。`record`（Phase 61）が書く候補には無い（`None` は「素の accept/reject」= 従来どおり）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
}

/// ADR-0047 D1 / D4。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Confidence {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Ok(Confidence::High),
            "medium" | "med" => Ok(Confidence::Medium),
            "low" => Ok(Confidence::Low),
            other => Err(format!(
                "confidence must be high | medium | low (got {other:?})"
            )),
        }
    }
}

/// ページの先頭の `---` … `---` と、その後ろの本文を分ける（純粋関数）。
///
/// 閉じていない `---` は front matter ではない（本文の水平線）。読める形:
///
/// ```text
/// ---
/// title: pegasus の使い方
/// tags: [hpc, pegasus]
/// scope: environment
/// sources:
///   - task:01J...
///   - human
/// confidence: high
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
                Some("sources") => front.sources.push(unquote(item)),
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
                front.title = non_empty(unquote(value));
                current = None;
            }
            "scope" => {
                front.scope = non_empty(unquote(value));
                current = None;
            }
            "path" => {
                front.path = non_empty(unquote(value));
                current = None;
            }
            "op" => {
                front.op = non_empty(unquote(value));
                current = None;
            }
            "created" => {
                front.created = non_empty(unquote(value));
                current = None;
            }
            "updated" => {
                front.updated = non_empty(unquote(value));
                current = None;
            }
            "confidence" => {
                front.confidence = unquote(value).parse().ok();
                current = None;
            }
            "tags" => {
                front.tags.extend(parse_list(value));
                current = Some(key);
            }
            "sources" => {
                front.sources.extend(parse_list(value));
                current = Some(key);
            }
            _ => current = None,
        }
    }
    if !closed {
        return (FrontMatter::default(), body);
    }
    (front, &rest[consumed..])
}

/// front matter と本文から 1 ページを組む（[`front_matter`] と往復する）。
pub fn render_page(front: &FrontMatter, body: &str) -> String {
    let mut out = String::from("---\n");
    if let Some(title) = front
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        out.push_str(&format!("title: {}\n", yaml_scalar(title)));
    }
    if !front.tags.is_empty() {
        out.push_str(&format!("tags: [{}]\n", yaml_list(&front.tags)));
    }
    if let Some(scope) = front
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        out.push_str(&format!("scope: {}\n", yaml_scalar(scope)));
    }
    if !front.sources.is_empty() {
        out.push_str(&format!("sources: [{}]\n", yaml_list(&front.sources)));
    }
    if let Some(created) = front
        .created
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        out.push_str(&format!("created: {}\n", yaml_scalar(created)));
    }
    if let Some(updated) = front
        .updated
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        out.push_str(&format!("updated: {}\n", yaml_scalar(updated)));
    }
    if let Some(confidence) = front.confidence {
        out.push_str(&format!("confidence: {confidence}\n"));
    }
    if let Some(path) = front
        .path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        out.push_str(&format!("path: {}\n", yaml_scalar(path)));
    }
    if let Some(op) = front.op.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str(&format!("op: {}\n", yaml_scalar(op)));
    }
    out.push_str("---\n");
    let body = body.trim_start_matches(['\n', '\r']);
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
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

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
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

/// YAML のスカラ（記号を含むなら引用する。最小限。`task_ops::docs` と同じ規則）。
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

fn yaml_list(items: &[String]) -> String {
    items
        .iter()
        .map(|t| yaml_scalar(t))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// パス（境界）
// ---------------------------------------------------------------------------

/// パスの検査の失敗（API は `Forbidden` を 403、`NotMarkdown` を 422 にする）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path must be relative to the knowledge base and must not contain `..`")]
    Forbidden,
    #[error("a knowledge page must be a Markdown file (*.md)")]
    NotMarkdown,
    #[error("path is required")]
    Empty,
}

/// 受け取った `path` を**KB の根からの相対**に正規化する（ADR-0047 D1 の境界）。
///
/// - `..`・絶対パス・Windows の prefix は `Forbidden`（403）
/// - `.md` で終わらないものは `NotMarkdown`（422）
pub fn page_path(raw: &str) -> Result<String, PathError> {
    let cleaned = clean_relative(raw)?;
    if !cleaned.to_ascii_lowercase().ends_with(".md") {
        return Err(PathError::NotMarkdown);
    }
    Ok(cleaned)
}

/// `page_path` の拡張子を見ない版（scope の正規化に使う）。
pub fn scope_path(raw: &str) -> Result<String, PathError> {
    clean_relative(raw)
}

fn clean_relative(raw: &str) -> Result<String, PathError> {
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
    Ok(cleaned)
}

/// `_inbox/` の下か（候補は索引に入らない）。
pub fn is_inbox(path: &str) -> bool {
    path == INBOX_DIR || path.starts_with(&format!("{INBOX_DIR}/"))
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
    let out: String = out.trim_matches('-').chars().take(48).collect();
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { None } else { Some(out) }
}

/// front matter の `scope`（`user` / `project:<slug>` …）から KB の相対ディレクトリを当てる。
/// 当たらなければ `None`（その scope は「ページのラベル」としてだけ使う）。
pub fn scope_dir(scope: &str) -> Option<String> {
    let scope = scope.trim();
    if scope.is_empty() {
        return None;
    }
    if let Some(slug) = scope.strip_prefix("project:") {
        let slug = slug.trim();
        return if slug.is_empty() {
            None
        } else {
            Some(format!("projects/{slug}"))
        };
    }
    scope_path(scope).ok()
}

// ---------------------------------------------------------------------------
// 索引（`index.json`。派生物）
// ---------------------------------------------------------------------------

/// `index.json` の 1 件。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IndexItem {
    /// KB の根からの相対パス（`environment/clusters/pegasus.md`）。
    pub path: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// front matter の `updated` → 最後のコミットの時刻（RFC 3339）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
}

/// `index.json`（**再生成できる派生物**。正本は Markdown のファイル）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Index {
    /// RFC 3339。
    pub generated_at: String,
    #[serde(default)]
    pub items: Vec<IndexItem>,
}

impl Index {
    /// そのパスの 1 件。
    pub fn get(&self, path: &str) -> Option<&IndexItem> {
        self.items.iter().find(|i| i.path == path)
    }

    /// その scope（KB の相対パスの接頭辞、または front matter の `scope`）に入る件（並びは索引の順）。
    pub fn in_scope<'a>(&'a self, scope: &'a str) -> impl Iterator<Item = &'a IndexItem> + 'a {
        self.items.iter().filter(move |item| in_scope(item, scope))
    }
}

/// その 1 件が `scope` に入るか。`scope` は **KB の相対パスの接頭辞**（`user`、`environment/clusters`、
/// `projects/pluvio`）か、front matter の `scope` の値そのもの（`project:pluvio`）。
pub fn in_scope(item: &IndexItem, scope: &str) -> bool {
    let scope = scope.trim().trim_matches('/');
    if scope.is_empty() {
        return true;
    }
    if item.path == scope || item.path.starts_with(&format!("{scope}/")) {
        return true;
    }
    if item.scope.as_deref().map(str::trim) == Some(scope) {
        return true;
    }
    // `scope = "project:pluvio"` は `projects/pluvio/` のページも指す。
    match scope_dir(scope) {
        Some(dir) if dir != scope => item.path == dir || item.path.starts_with(&format!("{dir}/")),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 検索（ADR-0047 D3。決定的。LLM 不在）
// ---------------------------------------------------------------------------

/// 検索の 1 件（順位付けの内訳も返す。GUI と CLI が「なぜ当たったか」を出せる）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchHit {
    #[serde(flatten)]
    pub item: IndexItem,
    /// 当たった `tags` の語数。
    pub tag_matches: usize,
    /// 当たった `title` の語数。
    pub title_matches: usize,
    /// 本文の全文一致（`git grep -il`）で当たった。
    pub body_match: bool,
}

/// ADR-0047 D3 の検索。**索引（`index.json`）と、本文一致のパスの集合だけ**を見る純粋関数。
///
/// 順位: tag 一致数（多い順）→ title 一致数（多い順）→ 本文一致 → `updated` の新しい順 → パスの辞書順。
/// 語は空白で切り、大文字小文字を区別しない部分一致。`query` が空なら `scope` の全件を
/// `updated` の新しい順で返す。
pub fn search(
    index: &Index,
    query: &str,
    scope: Option<&str>,
    limit: usize,
    body_hits: &[String],
) -> Vec<SearchHit> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let mut hits: Vec<SearchHit> = index
        .items
        .iter()
        .filter(|item| !is_inbox(&item.path))
        .filter(|item| scope.is_none_or(|s| in_scope(item, s)))
        .filter_map(|item| {
            let body_match = body_hits.iter().any(|p| p == &item.path);
            if terms.is_empty() {
                return Some(SearchHit {
                    item: item.clone(),
                    tag_matches: 0,
                    title_matches: 0,
                    body_match,
                });
            }
            let title = item.title.to_ascii_lowercase();
            let path = item.path.to_ascii_lowercase();
            let tag_matches = terms
                .iter()
                .filter(|term| {
                    item.tags
                        .iter()
                        .any(|tag| tag.to_ascii_lowercase().contains(term.as_str()))
                })
                .count();
            let title_matches = terms
                .iter()
                .filter(|term| title.contains(term.as_str()) || path.contains(term.as_str()))
                .count();
            if tag_matches == 0 && title_matches == 0 && !body_match {
                return None;
            }
            Some(SearchHit {
                item: item.clone(),
                tag_matches,
                title_matches,
                body_match,
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.tag_matches
            .cmp(&a.tag_matches)
            .then(b.title_matches.cmp(&a.title_matches))
            .then(b.body_match.cmp(&a.body_match))
            .then(b.item.updated.cmp(&a.item.updated))
            .then(a.item.path.cmp(&b.item.path))
    });
    if limit > 0 {
        hits.truncate(limit);
    }
    hits
}

// ---------------------------------------------------------------------------
// マウント（ADR-0046 D1 の `profile.knowledge` / ADR-0047 D2）
// ---------------------------------------------------------------------------

/// ADR-0047 D2: 何をマウントするか。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MountKind {
    /// KB の一部（`scope` = KB の相対パス。`user`、`environment/clusters`、`projects/<slug>`）。
    Kb,
    /// 案件のリポジトリの `docs/`（ADR-0043）。
    Repo,
    /// 任意のローカルディレクトリ（読み取り）。
    Dir,
    /// そのノードの長期記憶（`memory/<node>/notes.md`。ADR-0033 D3）。
    Memory,
}

impl MountKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MountKind::Kb => "kb",
            MountKind::Repo => "repo",
            MountKind::Dir => "dir",
            MountKind::Memory => "memory",
        }
    }
}

impl std::fmt::Display for MountKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// ADR-0046 D1 の `knowledge = [ … ]` の 1 件。**Phase 59 の `Profile.knowledge` はこの型を持つ**
/// （Phase 61 がここで定義し、Phase 59 が使う）。
///
/// TOML での形（ADR-0046 D1 のまま）:
///
/// ```toml
/// knowledge = [ { kind = "kb", scope = "environment/clusters" },
///               { kind = "repo", name = "pluvio", docs = "docs" },
///               { kind = "dir", path = "/opt/share/notes" },
///               { kind = "memory" } ]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct KnowledgeMount {
    pub kind: MountKind,
    /// `kb`: KB の相対パス（`user` / `environment/clusters` / `projects/<slug>`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `repo`: 案件のリポジトリの名前。`memory`: ノードの id（省略なら担当のノード）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `repo`: そのリポジトリの文書の根（既定 `docs`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    /// `dir`: ローカルのディレクトリ（読み取り）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl KnowledgeMount {
    pub fn kb(scope: impl Into<String>) -> Self {
        Self {
            kind: MountKind::Kb,
            scope: Some(scope.into()),
            name: None,
            docs: None,
            path: None,
        }
    }

    pub fn repo(name: impl Into<String>, docs: Option<String>) -> Self {
        Self {
            kind: MountKind::Repo,
            scope: None,
            name: Some(name.into()),
            docs,
            path: None,
        }
    }

    pub fn dir(path: impl Into<PathBuf>) -> Self {
        Self {
            kind: MountKind::Dir,
            scope: None,
            name: None,
            docs: None,
            path: Some(path.into()),
        }
    }

    pub fn memory(node: Option<String>) -> Self {
        Self {
            kind: MountKind::Memory,
            scope: None,
            name: node,
            docs: None,
            path: None,
        }
    }

    /// 前置きと GUI に出す 1 行の見出し。
    pub fn label(&self) -> String {
        match self.kind {
            MountKind::Kb => format!("kb:{}", self.scope.as_deref().unwrap_or("")),
            MountKind::Repo => format!("repo:{}", self.name.as_deref().unwrap_or("")),
            MountKind::Dir => format!(
                "dir:{}",
                self.path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
            MountKind::Memory => match self.name.as_deref() {
                Some(node) => format!("memory:{node}"),
                None => "memory".to_string(),
            },
        }
    }
}

/// 設定（`[knowledge] default_mounts`）とテストで使う短い表記。
///
/// `kb:<scope>` / `repo:<name>[:<docs>]` / `dir:<path>` / `memory[:<node>]`。
impl FromStr for KnowledgeMount {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("mount must not be blank".to_string());
        }
        let (kind, rest) = match s.split_once(':') {
            Some((kind, rest)) => (kind.trim(), rest.trim()),
            None => (s, ""),
        };
        match kind.to_ascii_lowercase().as_str() {
            "kb" => {
                let scope = scope_path(rest).map_err(|e| e.to_string())?;
                Ok(KnowledgeMount::kb(scope))
            }
            "repo" => {
                let (name, docs) = match rest.split_once(':') {
                    Some((name, docs)) => (name.trim(), Some(docs.trim().to_string())),
                    None => (rest, None),
                };
                if name.is_empty() {
                    return Err("repo mount needs a repository name (repo:<name>)".to_string());
                }
                Ok(KnowledgeMount::repo(name, docs.filter(|d| !d.is_empty())))
            }
            "dir" => {
                if rest.is_empty() {
                    return Err("dir mount needs a path (dir:<path>)".to_string());
                }
                Ok(KnowledgeMount::dir(PathBuf::from(rest)))
            }
            "memory" => Ok(KnowledgeMount::memory(
                Some(rest.to_string()).filter(|n| !n.is_empty()),
            )),
            other => Err(format!(
                "unknown knowledge mount kind {other:?} (kb | repo | dir | memory)"
            )),
        }
    }
}

/// その索引の 1 件がそのマウントに入るか（ADR-0047 D2。前置きの索引をマウントごとに並べるのに使う）。
///
/// `kb` は [`in_scope`]。`repo` / `dir` / `memory` の項目は、集める側（ディスパッチャ）が
/// `scope` に [`KnowledgeMount::label`] と同じ文字列（`repo:<name>` / `dir:<path>` / `memory:<node>`）を
/// 入れる約束にしてある（KB の外のページを、KB の索引と同じ形で 1 本に並べるため）。
pub fn mount_matches(mount: &KnowledgeMount, item: &IndexItem) -> bool {
    match mount.kind {
        MountKind::Kb => match mount.scope.as_deref() {
            Some(scope) => in_scope(item, scope),
            None => false,
        },
        MountKind::Repo | MountKind::Dir => item.scope.as_deref() == Some(mount.label().as_str()),
        // `memory` は担当のノードが決まってから label が定まるので、接頭辞で見る。
        MountKind::Memory => item
            .scope
            .as_deref()
            .is_some_and(|s| s.starts_with("memory")),
    }
}

/// 実効マウント（ADR-0047 D2: 組織の和 ＋ 案件 ＋ タスクの明示）。重複は落とし、順は最初に出た順。
pub fn merge_mounts(parts: &[&[KnowledgeMount]]) -> Vec<KnowledgeMount> {
    let mut out: Vec<KnowledgeMount> = Vec::new();
    for part in parts {
        for mount in part.iter() {
            if !out.contains(mount) {
                out.push(mount.clone());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 秘密の検査（ADR-0047 D4。Phase 61 で先に入れる）
// ---------------------------------------------------------------------------

/// ADR-0047 D4 が「保存しない」と決めているもののうち、**決定的に判定できるもの**。
/// 当たったらそのパターンの名前を返す（`record` は拒否し、Phase 62 の適用も弾く）。
pub const SECRET_PATTERNS: [(&str, &str); 10] = [
    ("sk-", "OpenAI 風の API キー（sk-…）"),
    ("ghp_", "GitHub の個人アクセストークン（ghp_…）"),
    ("gho_", "GitHub の OAuth トークン（gho_…）"),
    ("ghs_", "GitHub のサーバトークン（ghs_…）"),
    ("github_pat_", "GitHub のきめ細かいトークン（github_pat_…）"),
    ("xoxb-", "Slack のボットトークン（xoxb-…）"),
    ("AKIA", "AWS のアクセスキー ID（AKIA…）"),
    ("AIza", "Google の API キー（AIza…）"),
    ("-----BEGIN", "PEM の秘密鍵（-----BEGIN …）"),
    ("ANTHROPIC_API_KEY=", "環境変数に書かれた API キー"),
];

/// 秘密が含まれていればその説明（ADR-0047 D4。決定的。大文字小文字は `sk-` などの前置きだけ
/// 区別しない）。
pub fn secret_finding(text: &str) -> Option<&'static str> {
    for (needle, why) in SECRET_PATTERNS {
        if needle.chars().any(|c| c.is_ascii_uppercase()) {
            if text.contains(needle) {
                return Some(why);
            }
        } else if text.to_ascii_lowercase().contains(needle) {
            return Some(why);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 知識整理 run の候補（ADR-0047 D4。Phase 62）。純粋なデータと検証だけ（I/O・git・LLM 無し）。
// git を起こす適用は `task_ops::knowledge::apply_candidates`。
// ---------------------------------------------------------------------------

/// `artifacts/knowledge-candidates.json` の候補 1 件がとる操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CandidateOp {
    Create,
    Update,
    Merge,
    Retire,
}

impl CandidateOp {
    pub fn as_str(self) -> &'static str {
        match self {
            CandidateOp::Create => "create",
            CandidateOp::Update => "update",
            CandidateOp::Merge => "merge",
            CandidateOp::Retire => "retire",
        }
    }

    /// `confidence = high` のとき、KB へ**直接**コミットしてよい操作か（ADR-0047 D4）。
    /// `merge` / `retire` は常に `_inbox/` に置く。
    pub fn direct_commit_eligible(self) -> bool {
        matches!(self, CandidateOp::Create | CandidateOp::Update)
    }
}

impl std::fmt::Display for CandidateOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CandidateOp {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "create" => Ok(CandidateOp::Create),
            "update" => Ok(CandidateOp::Update),
            "merge" => Ok(CandidateOp::Merge),
            "retire" => Ok(CandidateOp::Retire),
            other => Err(format!(
                "op must be create | update | merge | retire (got {other:?})"
            )),
        }
    }
}

/// `langmem` アダプタが `artifacts/knowledge-candidates.json` に書く候補 1 件
/// （ADR-0047 D4: `{op, path, title, tags, scope, body, sources, confidence}`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Candidate {
    pub op: CandidateOp,
    /// KB の根からの相対パス（`retire`/`merge` は対象ページ、`create`/`update` は書き込み先）。
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// `user` / `environment` / `project:<slug>` / `experience`。
    #[serde(default)]
    pub scope: String,
    /// `create`/`update`/`merge` の本文。`retire` は空でもよい（理由を書いてもよい）。
    #[serde(default)]
    pub body: String,
    /// `task:<id>` / `message:<id>` / `human` / `url:<…>`。空は許さない（出典の無い知識は入れない）。
    #[serde(default)]
    pub sources: Vec<String>,
    pub confidence: Confidence,
}

/// [`validate_candidate`] が弾く「保存に値しない」理由（ADR-0047 D4「保存しないもの」＋ D1 の境界）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CandidateProblem {
    #[error("title must not be blank")]
    NoTitle,
    #[error("at least one source is required")]
    NoSources,
    #[error("body must not be blank")]
    NoBody,
    #[error("path: {0}")]
    Path(#[from] PathError),
    #[error("body exceeds {MAX_PAGE_BYTES} bytes")]
    TooLarge,
    #[error("refused: the candidate contains a secret（{0}）")]
    Secret(&'static str),
}

/// ADR-0047 D4: 候補の決定的な検査（LLM の判断は経ない）。通れば KB 相対の正規化パスを返す。
///
/// - パスは KB の根に収まる `*.md`（[`page_path`]）
/// - `title` は空でない
/// - `sources` は 1 件以上（`retire` も対象ページの理由づけとして要る）
/// - `body` は `retire` を除き空でない（対象ページを退役させるだけなら理由は必須ではない）
/// - `body` は [`MAX_PAGE_BYTES`] 以下
/// - `title` / `body` / `sources` に秘密が無い（[`secret_finding`]）
pub fn validate_candidate(candidate: &Candidate) -> Result<String, CandidateProblem> {
    let path = page_path(&candidate.path)?;
    if candidate.title.trim().is_empty() {
        return Err(CandidateProblem::NoTitle);
    }
    let sources: Vec<&str> = candidate
        .sources
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if sources.is_empty() {
        return Err(CandidateProblem::NoSources);
    }
    if candidate.op != CandidateOp::Retire && candidate.body.trim().is_empty() {
        return Err(CandidateProblem::NoBody);
    }
    if candidate.body.len() > MAX_PAGE_BYTES {
        return Err(CandidateProblem::TooLarge);
    }
    let haystack = format!(
        "{}\n{}\n{}",
        candidate.title,
        candidate.body,
        sources.join("\n")
    );
    if let Some(why) = secret_finding(&haystack) {
        return Err(CandidateProblem::Secret(why));
    }
    Ok(path)
}

// ---------------------------------------------------------------------------
// 知識整理 run の依頼文（ADR-0047 D4。Phase 62）。決定的な組み立て（LLM は呼ばない）。
// `crates/celeris/src/knowledge_maint.rs` が集めた入力から、langmem アダプタに渡す
// タスクの `objective` を組む（`task_core::report::compaction_objective` と同じ考え方）。
// ---------------------------------------------------------------------------

/// 関連する既存の KB ページ 1 件（決定的な検索の上位。ADR-0047 D3 の `search`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedPage {
    pub path: String,
    pub title: String,
    /// 本文の抜粋（front matter を除く）。
    pub excerpt: String,
}

/// 知識整理 run の入力（`crates/celeris/src/knowledge_maint.rs` が集める）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaintenanceInput {
    pub task_id: String,
    pub task_title: String,
    pub task_objective: String,
    pub report_headline: String,
    pub report_body: String,
    pub result_summary: String,
    /// 人・ワーカーのコメント（`"<誰か>: <本文>"` の形に整形済み）。
    pub comments: Vec<String>,
    pub related_pages: Vec<RelatedPage>,
    /// そのノードの手帳（`memory/<node>/notes.md`）の抜粋。
    pub notes_excerpt: String,
    /// 既存の索引の題名（`"<path> — <title>"`）。近い重複を作らないための手掛かり。
    pub existing_titles: Vec<String>,
}

/// ADR-0047 D4 の抽出指示（決定的な文面。langmem 自身の抽出プロンプトはこれを踏まえて python 側が組む）。
pub fn maintenance_objective(input: &MaintenanceInput) -> String {
    let mut out = format!(
        "あなたは知識ベース（ADR-0047）の整理役です。次の 1 タスクの終端から、\
         将来も使える事実だけを抽出し、候補として `artifacts/knowledge-candidates.json` に書いてください。\n\n\
         タスク: {}（id: {}）\n目的: {}\n",
        input.task_title,
        input.task_id,
        input.task_objective.trim()
    );
    if !input.report_headline.trim().is_empty() || !input.report_body.trim().is_empty() {
        out.push_str("\n---- 報告 ----\n");
        if !input.report_headline.trim().is_empty() {
            out.push_str(&format!("{}\n", input.report_headline.trim()));
        }
        if !input.report_body.trim().is_empty() {
            out.push_str(&format!("{}\n", input.report_body.trim()));
        }
    }
    if !input.result_summary.trim().is_empty() {
        out.push_str(&format!(
            "\n---- 結果（result.json summary）----\n{}\n",
            input.result_summary.trim()
        ));
    }
    if !input.comments.is_empty() {
        out.push_str("\n---- コメント ----\n");
        for c in &input.comments {
            out.push_str(&format!("- {c}\n"));
        }
    }
    if !input.notes_excerpt.trim().is_empty() {
        out.push_str(&format!(
            "\n---- 担当ノードの手帳の抜粋 ----\n{}\n",
            input.notes_excerpt.trim()
        ));
    }
    if !input.related_pages.is_empty() {
        out.push_str("\n---- 関連する既存の知識ベースのページ（検索の上位）----\n");
        for p in &input.related_pages {
            out.push_str(&format!(
                "\n[{}] {}\n{}\n",
                p.path,
                p.title,
                p.excerpt.trim()
            ));
        }
    }
    if !input.existing_titles.is_empty() {
        out.push_str("\n---- 既存の索引（重複を避ける手掛かり）----\n");
        for t in &input.existing_titles {
            out.push_str(&format!("- {t}\n"));
        }
    }
    out.push_str(
        "\n---- 抽出の規則（ADR-0047 D4）----\n\
         - 将来も使える事実だけを候補にする。一時的な情報・雑談・重複・信頼性の低い推測は候補にしない\n\
         - 秘密（API キー・パスワード・トークン・秘密鍵）は絶対に候補に含めない\n\
         - 出典（`sources`。`task:{task_id}` を少なくとも 1 つ）を必ず付ける\n\
         - 近い既存ページがあれば `update`（`op = update`、対象の `path`）を優先し、`create` で近い重複を作らない\n\
         - 既存ページが古い・誤っていると分かったら `merge`（本文をあなたが書き直した完全な版にする）か\n\
           `retire`（そのページはもう使えない）を使う\n\
         - 確信度は `confidence`（`high` / `medium` / `low`）で正直に書く。`high` の `create`/`update` は\n\
           そのまま知識ベースに入る（他は人が確認してから入る）\n\
         - 何も抽出するものが無ければ、空の `candidates` を書いてよい\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_round_trips() {
        let front = FrontMatter {
            title: Some("pegasus の使い方".into()),
            tags: vec!["hpc".into(), "pegasus".into()],
            scope: Some("environment".into()),
            sources: vec!["task:01J1".into(), "human".into()],
            created: Some("2026-09-20".into()),
            updated: Some("2026-09-20".into()),
            confidence: Some(Confidence::High),
            path: None,
            op: None,
        };
        let page = render_page(&front, "# pegasus\n\npjsub で投げる。\n");
        let (again, body) = front_matter(&page);
        assert_eq!(again, front, "{page}");
        assert_eq!(body, "\n# pegasus\n\npjsub で投げる。\n");
        // 2 回目も同じ（冪等）。
        assert_eq!(render_page(&again, body), page);
        // 題名は front matter が勝つ。
        assert_eq!(
            title_of(&page, "environment/clusters/pegasus.md"),
            "pegasus の使い方"
        );
    }

    /// ADR-0047 D4（Phase 62）: `_inbox` 専用の `op` も往復する。
    #[test]
    fn front_matter_round_trips_the_op_key() {
        let front = FrontMatter {
            title: Some("退役候補".into()),
            scope: Some("environment".into()),
            sources: vec!["task:01J2".into()],
            path: Some("environment/tools/old.md".into()),
            op: Some("retire".into()),
            ..FrontMatter::default()
        };
        let page = render_page(&front, "古くなった。\n");
        assert!(page.contains("op: retire"), "{page}");
        let (again, _) = front_matter(&page);
        assert_eq!(again, front);
    }

    #[test]
    fn front_matter_reads_block_lists_and_ignores_unclosed_blocks() {
        let raw = "---\ntitle: x\ntags:\n  - a\n  - \"b\"\nsources:\n  - url:https://e.com\nconfidence: medium\n---\n本文\n";
        let (front, body) = front_matter(raw);
        assert_eq!(front.tags, vec!["a", "b"]);
        assert_eq!(front.sources, vec!["url:https://e.com"]);
        assert_eq!(front.confidence, Some(Confidence::Medium));
        assert_eq!(body, "本文\n");
        // 閉じていない `---` は front matter ではない。
        let (front, body) = front_matter("---\ntitle: x\n# 本文\n");
        assert_eq!(front, FrontMatter::default());
        assert!(body.starts_with("---"));
        // front matter が無いページは題名を本文から取る。
        assert_eq!(title_of("# 題名\n", "user/profile.md"), "題名");
        assert_eq!(title_of("本文\n", "user/profile.md"), "profile.md");
    }

    #[test]
    fn paths_are_confined_to_the_knowledge_root() {
        assert_eq!(page_path("user/profile.md").expect("ok"), "user/profile.md");
        assert_eq!(page_path("./user/a.md").expect("ok"), "user/a.md");
        for bad in ["../x.md", "/etc/x.md", "user/../../x.md"] {
            assert_eq!(page_path(bad), Err(PathError::Forbidden), "{bad}");
        }
        assert_eq!(page_path("a.txt"), Err(PathError::NotMarkdown));
        assert_eq!(page_path("  "), Err(PathError::Empty));
        assert!(is_inbox("_inbox/2026-x.md"));
        assert!(!is_inbox("_inboxed/x.md"));
        assert_eq!(
            scope_dir("project:pluvio").as_deref(),
            Some("projects/pluvio")
        );
        assert_eq!(scope_dir("user").as_deref(), Some("user"));
        assert_eq!(scope_dir("  "), None);
    }

    fn sample_index() -> Index {
        Index {
            generated_at: "2026-09-20T00:00:00Z".into(),
            items: vec![
                IndexItem {
                    path: "environment/clusters/pegasus.md".into(),
                    title: "pegasus".into(),
                    tags: vec!["hpc".into(), "cluster".into()],
                    scope: Some("environment".into()),
                    updated: Some("2026-09-01T00:00:00Z".into()),
                    ..IndexItem::default()
                },
                IndexItem {
                    path: "user/profile.md".into(),
                    title: "cluster の好み".into(),
                    tags: vec![],
                    scope: Some("user".into()),
                    updated: Some("2026-09-10T00:00:00Z".into()),
                    ..IndexItem::default()
                },
                IndexItem {
                    path: "experience/2026/09/hpc-run.md".into(),
                    title: "計測".into(),
                    tags: vec![],
                    scope: Some("experience".into()),
                    updated: Some("2026-09-19T00:00:00Z".into()),
                    ..IndexItem::default()
                },
                IndexItem {
                    path: "_inbox/2026-09-20-x.md".into(),
                    title: "cluster の候補".into(),
                    tags: vec!["cluster".into()],
                    ..IndexItem::default()
                },
            ],
        }
    }

    /// ADR-0047 D3: tag 一致 → title 一致 → 本文一致 → `updated` の新しさ。`_inbox` は出ない。
    #[test]
    fn search_ranks_tags_above_titles_above_bodies() {
        let index = sample_index();
        let hits = search(
            &index,
            "cluster",
            None,
            10,
            &["experience/2026/09/hpc-run.md".to_string()],
        );
        let paths: Vec<&str> = hits.iter().map(|h| h.item.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "environment/clusters/pegasus.md", // tag 一致
                "user/profile.md",                 // title 一致
                "experience/2026/09/hpc-run.md",   // 本文一致だけ
            ],
            "{hits:#?}"
        );
        assert_eq!(hits[0].tag_matches, 1);
        assert!(hits[2].body_match && hits[2].tag_matches == 0);
        // `_inbox` は検索に出ない（候補は正本ではない）。
        assert!(!paths.contains(&"_inbox/2026-09-20-x.md"));

        // scope で絞る（KB の相対パスの接頭辞でも front matter の `scope` でも当たる）。
        let only_env = search(&index, "cluster", Some("environment"), 10, &[]);
        assert_eq!(only_env.len(), 1);
        assert_eq!(only_env[0].item.path, "environment/clusters/pegasus.md");
        assert_eq!(
            search(&index, "cluster", Some("environment/clusters"), 10, &[]).len(),
            1
        );
        assert!(search(&index, "cluster", Some("projects/pluvio"), 10, &[]).is_empty());

        // limit で切る。
        assert_eq!(search(&index, "cluster", None, 1, &[]).len(), 1);
        // 当たらない語は 0 件。
        assert!(search(&index, "みつからない", None, 10, &[]).is_empty());
        // 空の query は scope の全件を `updated` の新しい順で返す。
        let all = search(&index, "  ", None, 10, &[]);
        assert_eq!(
            all.iter().map(|h| h.item.path.as_str()).collect::<Vec<_>>(),
            vec![
                "experience/2026/09/hpc-run.md",
                "user/profile.md",
                "environment/clusters/pegasus.md"
            ]
        );
    }

    #[test]
    fn mounts_parse_from_the_short_form_and_merge_without_duplicates() {
        assert_eq!(
            "kb:user".parse::<KnowledgeMount>().expect("kb"),
            KnowledgeMount::kb("user")
        );
        assert_eq!(
            "kb:environment/clusters"
                .parse::<KnowledgeMount>()
                .expect("kb"),
            KnowledgeMount::kb("environment/clusters")
        );
        assert_eq!(
            "repo:pluvio:doc".parse::<KnowledgeMount>().expect("repo"),
            KnowledgeMount::repo("pluvio", Some("doc".into()))
        );
        assert_eq!(
            "dir:/opt/notes".parse::<KnowledgeMount>().expect("dir"),
            KnowledgeMount::dir("/opt/notes")
        );
        assert_eq!(
            "memory".parse::<KnowledgeMount>().expect("memory"),
            KnowledgeMount::memory(None)
        );
        assert_eq!(
            "memory:cos".parse::<KnowledgeMount>().expect("memory"),
            KnowledgeMount::memory(Some("cos".into()))
        );
        assert!("kb:../etc".parse::<KnowledgeMount>().is_err());
        assert!("nope:x".parse::<KnowledgeMount>().is_err());
        assert_eq!(KnowledgeMount::kb("user").label(), "kb:user");

        let org = vec![KnowledgeMount::kb("user"), KnowledgeMount::memory(None)];
        let project = vec![
            KnowledgeMount::kb("projects/pluvio"),
            KnowledgeMount::kb("user"),
        ];
        let merged = merge_mounts(&[&org, &project]);
        assert_eq!(
            merged,
            vec![
                KnowledgeMount::kb("user"),
                KnowledgeMount::memory(None),
                KnowledgeMount::kb("projects/pluvio"),
            ]
        );
    }

    /// ADR-0047 D4: 秘密は決定的なパターンで弾く。
    #[test]
    fn secrets_are_refused_by_pattern() {
        assert!(secret_finding("api key: sk-abcdef123").is_some());
        assert!(secret_finding("token ghp_0123456789").is_some());
        assert!(secret_finding("-----BEGIN OPENSSH PRIVATE KEY-----").is_some());
        assert!(secret_finding("AKIAIOSFODNN7EXAMPLE").is_some());
        assert!(secret_finding("AIzaSyA-0123").is_some());
        // 大文字の AKIA は小文字の `akia` とは違う（誤検知を避ける）。
        assert!(secret_finding("akiaiosfodnn7example").is_none());
        assert!(secret_finding("pegasus は pjsub で投げる").is_none());
    }

    fn ok_candidate(op: CandidateOp) -> Candidate {
        Candidate {
            op,
            path: "environment/clusters/pegasus.md".into(),
            title: "pegasus の使い方".into(),
            tags: vec!["hpc".into()],
            scope: "environment".into(),
            body: "pjsub -L node=1 で投げる。".into(),
            sources: vec!["task:01J1".into()],
            confidence: Confidence::High,
        }
    }

    /// ADR-0047 D4: 候補の決定的な検査。`retire` だけ本文が空でもよい。
    #[test]
    fn validate_candidate_checks_path_title_sources_body_size_and_secrets() {
        assert_eq!(
            validate_candidate(&ok_candidate(CandidateOp::Create)).expect("ok"),
            "environment/clusters/pegasus.md"
        );

        let mut retiring = ok_candidate(CandidateOp::Retire);
        retiring.body = String::new();
        assert_eq!(
            validate_candidate(&retiring).expect("retire may have an empty body"),
            "environment/clusters/pegasus.md"
        );

        let mut escapes = ok_candidate(CandidateOp::Create);
        escapes.path = "../../etc/passwd".into();
        assert!(matches!(
            validate_candidate(&escapes),
            Err(CandidateProblem::Path(PathError::Forbidden))
        ));

        let mut no_title = ok_candidate(CandidateOp::Create);
        no_title.title = "  ".into();
        assert_eq!(
            validate_candidate(&no_title),
            Err(CandidateProblem::NoTitle)
        );

        let mut no_sources = ok_candidate(CandidateOp::Create);
        no_sources.sources = vec![];
        assert_eq!(
            validate_candidate(&no_sources),
            Err(CandidateProblem::NoSources)
        );

        let mut no_body = ok_candidate(CandidateOp::Update);
        no_body.body = "   ".into();
        assert_eq!(validate_candidate(&no_body), Err(CandidateProblem::NoBody));

        let mut too_large = ok_candidate(CandidateOp::Create);
        too_large.body = "x".repeat(MAX_PAGE_BYTES + 1);
        assert_eq!(
            validate_candidate(&too_large),
            Err(CandidateProblem::TooLarge)
        );

        let mut secret = ok_candidate(CandidateOp::Create);
        secret.body = "API キーは sk-abc123def456 です".into();
        assert!(matches!(
            validate_candidate(&secret),
            Err(CandidateProblem::Secret(_))
        ));

        assert!(CandidateOp::Create.direct_commit_eligible());
        assert!(CandidateOp::Update.direct_commit_eligible());
        assert!(!CandidateOp::Merge.direct_commit_eligible());
        assert!(!CandidateOp::Retire.direct_commit_eligible());
        assert_eq!("update".parse::<CandidateOp>(), Ok(CandidateOp::Update));
        assert!("bogus".parse::<CandidateOp>().is_err());
    }

    /// ADR-0047 D4: 依頼文は入力の各節を含み、出典に `task:<id>` を促す（決定的。LLM は呼ばない）。
    #[test]
    fn maintenance_objective_includes_every_section() {
        let input = MaintenanceInput {
            task_id: "01J1".into(),
            task_title: "pegasus の初期セットアップ".into(),
            task_objective: "pegasus に pjsub の使い方を確認する".into(),
            report_headline: "pjsub の投げ方を確認した".into(),
            report_body: "pjsub -L node=1 で投げられる。".into(),
            result_summary: "pjsub -L node=1 -L elapse=01:00 で動作確認済み".into(),
            comments: vec!["human: 良さそう".into()],
            related_pages: vec![RelatedPage {
                path: "environment/clusters/pegasus.md".into(),
                title: "pegasus の使い方".into(),
                excerpt: "既存の使い方メモ".into(),
            }],
            notes_excerpt: "- 2026-09-10: pegasus は pjsub".into(),
            existing_titles: vec!["environment/clusters/pegasus.md — pegasus の使い方".into()],
        };
        let text = maintenance_objective(&input);
        assert!(text.contains("01J1"), "{text}");
        assert!(text.contains("pjsub の投げ方を確認した"), "{text}");
        assert!(text.contains("pjsub -L node=1 -L elapse=01:00"), "{text}");
        assert!(text.contains("human: 良さそう"), "{text}");
        assert!(text.contains("既存の使い方メモ"), "{text}");
        assert!(text.contains("2026-09-10: pegasus は pjsub"), "{text}");
        assert!(text.contains("pegasus の使い方"), "{text}");
        assert!(text.contains("sources"), "{text}");

        let minimal = maintenance_objective(&MaintenanceInput {
            task_id: "01J2".into(),
            task_title: "x".into(),
            task_objective: "y".into(),
            ..MaintenanceInput::default()
        });
        assert!(!minimal.contains("---- 報告"), "{minimal}");
        assert!(!minimal.contains("---- コメント"), "{minimal}");
    }
}
