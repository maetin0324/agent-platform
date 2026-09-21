//! 知識ベース（ADR-0047 D3 / D5。Phase 61）。**正本は `[knowledge] root` の Markdown**:
//!
//! - `GET  /knowledge/tree?scope=&q=` — ツリー（`q` は索引 ＋ `git grep -il`）。読み取り
//! - `GET  /knowledge/page?path=` — 1 ページ（raw / html / front matter / 履歴 / etag）。読み取り
//! - `PUT  /knowledge/page` — **管理系**。人の編集を 1 件 1 コミット（`etag` 必須）
//! - `GET  /knowledge/inbox` — `_inbox/` の候補。読み取り
//! - `POST /knowledge/inbox/{id}/accept` / `reject` — **管理系**。取り込み・破棄
//!
//! 境界（ADR-0044 D7 / ADR-0047 D1 と同じ規則）:
//!
//! - パスは**KB の根からの相対**で、根の外に出られない（`..`・絶対パスは 403）。`.md` 以外は 422
//! - 人の編集は `etag`（中身の sha256）で衝突を見る。違えば 409 `etag_mismatch`
//! - 書き込みのあとは必ず索引（`index.json`）を作り直す
//! - 読み取りは**何も作らない**（KB が無ければ `initialized: false` を返すだけ）
//!
//! ADR-0013 は「API はコマンドを実行しない」と決めているが、ここも `task_ops::knowledge` を通して
//! **`git` だけ**を上限付きで起こす（`crate::docs` と同じ逸脱。ワーカーも LLM も起こさない）。

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::knowledge::{self as kb, Confidence, PathError};
use task_ops::docs::{self as ops_docs, DocCommit};
use task_ops::knowledge::{self as ops_kb, InboxOutcome, PageEdit, WriteOutcome};

use crate::handlers::{ApiResult, Params, json_response, no_query, read_json};
use crate::middleware::require_admin;
use crate::problem::ApiProblem;
use crate::query::QueryParams;
use crate::state::ApiState;
use crate::types::ValidationError;

/// ツリーに出すページの上限（`crate::docs::MAX_TREE_PAGES` と同じ）。
pub const MAX_TREE_PAGES: usize = 500;

pub(crate) fn routes() -> axum::Router<ApiState> {
    axum::Router::new()
        .route("/api/v1/knowledge/tree", get(tree))
        .route("/api/v1/knowledge/page", get(page).put(put_page))
        .route("/api/v1/knowledge/inbox", get(inbox))
        .route("/api/v1/knowledge/inbox/{id}/accept", post(accept))
        .route("/api/v1/knowledge/inbox/{id}/reject", post(reject))
}

// ---------------------------------------------------------------------------
// 応答の型（`docs/gui/api.md` §3.98〜3.102）
// ---------------------------------------------------------------------------

/// ツリーの 1 件（`GET /knowledge/tree`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgeItem {
    /// KB の根からの相対パス（`environment/clusters/pegasus.md`）。
    pub path: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// front matter の `scope`（無ければ置き場から決めた既定）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// RFC 3339 か `YYYY-MM-DD`（front matter の `updated` → 最後のコミット）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
}

impl From<kb::IndexItem> for KnowledgeItem {
    fn from(item: kb::IndexItem) -> Self {
        Self {
            path: item.path,
            title: item.title,
            tags: item.tags,
            scope: item.scope,
            sources: item.sources,
            updated: item.updated,
            confidence: item.confidence,
        }
    }
}

/// `GET /knowledge/tree`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgeTree {
    /// KB の根（絶対パス）。
    pub root: String,
    /// `celerisctl knowledge init` が済んでいるか。偽なら `items` は空。
    pub initialized: bool,
    /// `?scope=` で絞ったならその文字列。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `?q=` で絞ったならその文字列。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    /// KB にある scope の一覧（画面のツリーの見出し。`user` / `environment/clusters` / `projects/<slug>` …）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    pub items: Vec<KnowledgeItem>,
    /// `_inbox/` にある候補の数（画面のバッジ）。
    pub inbox_count: usize,
    /// [`MAX_TREE_PAGES`] で切った。
    pub truncated: bool,
    /// `index.json` を作った時刻（RFC 3339）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
}

/// `GET /knowledge/page`。`crate::docs::DocPage` と同じ形（描画・履歴・etag）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgePage {
    pub root: String,
    pub path: String,
    pub title: String,
    /// Markdown のもと（front matter を含む）。`too_large` なら空。
    pub raw: String,
    /// サーバで描画した HTML（生 HTML は捨ててある）。
    pub html: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// 直近 20 件（新しい順）。
    pub history: Vec<DocCommit>,
    /// いまの中身の sha256。`PUT` にそのまま渡す。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    pub too_large: bool,
}

/// `PUT /knowledge/page` の本文。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgePagePutBody {
    pub path: String,
    pub body: String,
    /// 既にあるページを直すときは必須（無ければ 409 `etag_mismatch`）。
    #[serde(default)]
    pub etag: Option<String>,
    /// コミットメッセージ（既定 `knowledge: <path>`）。
    #[serde(default)]
    pub message: Option<String>,
}

/// `PUT /knowledge/page` と `inbox/{id}/accept` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgePageResult {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// 新しいコミットの sha。
    pub sha: String,
    /// 中身が同じだったので新しいコミットは作らなかった。
    pub unchanged: bool,
}

/// `_inbox/` の候補 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgeCandidate {
    pub id: String,
    /// `_inbox/<id>.md`。
    pub path: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `task:<id>` / `message:<id>` / `human` / `url:<…>`（GUI は出典へのリンクにする）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    /// 取り込む先（front matter の `path`、無ければ `scope` と題名からの既定）。
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// 本文（front matter を除く）。
    pub body: String,
    /// 本文を描画した HTML（生 HTML は捨ててある）。
    pub html: String,
    /// 取り込み先に既にページがある（accept は `overwrite` が要る）。
    pub target_exists: bool,
    /// ADR-0047 D4（Phase 62）: `create` / `update` / `merge` / `retire`。`celerisctl knowledge record`
    /// が書いた候補（Phase 61）には無い（`null`）。`retire` の accept は `target` を `_retired/` へ動かし、
    /// `merge` の accept は `target` を必ず上書きする（P-61-k。`docs/knowledge.md` 参照）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
}

/// `GET /knowledge/inbox`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgeInbox {
    pub root: String,
    pub initialized: bool,
    pub items: Vec<KnowledgeCandidate>,
}

/// `POST /knowledge/inbox/{id}/accept` の本文（省略してよい）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeAcceptBody {
    /// 取り込む先（省略なら候補の front matter の `path`）。
    #[serde(default)]
    pub path: Option<String>,
    /// 宛先が既にあっても上書きする。
    #[serde(default)]
    pub overwrite: bool,
}

/// `POST /knowledge/inbox/{id}/reject` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KnowledgeRejectResult {
    pub id: String,
    pub sha: String,
}

// ---------------------------------------------------------------------------
// 共通
// ---------------------------------------------------------------------------

fn knowledge_unavailable(detail: impl Into<String>) -> ApiProblem {
    ApiProblem::new(StatusCode::CONFLICT, "knowledge_unavailable", detail)
}

fn page_not_found(path: &str) -> ApiProblem {
    ApiProblem::new(
        StatusCode::NOT_FOUND,
        "page_not_found",
        format!("knowledge page not found: {path}"),
    )
}

fn candidate_not_found(id: &str) -> ApiProblem {
    ApiProblem::new(
        StatusCode::NOT_FOUND,
        "candidate_not_found",
        format!("knowledge candidate not found: {id}"),
    )
}

fn etag_mismatch(etag: Option<String>) -> ApiProblem {
    let problem = ApiProblem::new(
        StatusCode::CONFLICT,
        "etag_mismatch",
        "the page changed since it was read (reload and edit again)",
    );
    match etag {
        Some(etag) => problem.with_extra("etag", etag),
        None => problem,
    }
}

/// パスの検査（`..`・絶対パスは 403、`.md` 以外は 422）。
fn page_path(raw: &str) -> Result<String, ApiProblem> {
    kb::page_path(raw).map_err(path_problem)
}

fn path_problem(e: PathError) -> ApiProblem {
    match e {
        PathError::Forbidden => ApiProblem::path_forbidden(e.to_string()),
        PathError::NotMarkdown => ApiProblem::validation(vec![ValidationError {
            field: Some("path".into()),
            message: e.to_string(),
        }]),
        PathError::Empty => ApiProblem::bad_request("path is required"),
    }
}

/// KB の根（設定していなければ 409）。Phase 82: `crate::skills` も同じ KB を見るので `pub(crate)`。
pub(crate) fn root_of(state: &ApiState) -> Result<PathBuf, ApiProblem> {
    state.inner.knowledge_root.clone().ok_or_else(|| {
        knowledge_unavailable(
            "`[knowledge] root` が設定されていません（config.toml に `[knowledge]` を足す）",
        )
    })
}

/// 書き込み系は KB が用意されていることを要求する。Phase 82: `crate::skills` も使う。
pub(crate) fn require_kb(root: &std::path::Path) -> Result<(), ApiProblem> {
    if ops_kb::exists(root) {
        Ok(())
    } else {
        Err(knowledge_unavailable(format!(
            "{} に知識ベースがありません（`celerisctl knowledge init` で用意する）",
            root.display()
        )))
    }
}

/// ページの中の `[[…]]` のリンク先（GUI の「知識」画面）。
const LINK_BASE: &str = "/knowledge";

fn render_markdown(body: &str, from: &str) -> String {
    ops_docs::render(body, "", from, LINK_BASE)
}

// ---------------------------------------------------------------------------
// GET /knowledge/tree
// ---------------------------------------------------------------------------

async fn tree(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    let query = QueryParams::parse(raw.as_deref(), &["scope", "q", "limit"])?;
    let scope = query
        .single("scope")?
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let q = query
        .single("q")?
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let root = root_of(&state)?;
    let view = state
        .blocking(move |_| {
            let initialized = ops_kb::exists(&root);
            if !initialized {
                return Ok(KnowledgeTree {
                    root: root.display().to_string(),
                    initialized,
                    scope,
                    q,
                    scopes: Vec::new(),
                    items: Vec::new(),
                    inbox_count: 0,
                    truncated: false,
                    generated_at: None,
                });
            }
            let index = ops_kb::ensure_index(&root);
            let mut items: Vec<KnowledgeItem> = match &q {
                Some(q) => ops_kb::search(&root, q, scope.as_deref(), 0)
                    .into_iter()
                    .map(|h| h.item.into())
                    .collect(),
                None => index
                    .items
                    .iter()
                    .filter(|i| scope.as_deref().is_none_or(|s| kb::in_scope(i, s)))
                    .cloned()
                    .map(KnowledgeItem::from)
                    .collect(),
            };
            if q.is_none() {
                items.sort_by(|a, b| a.path.cmp(&b.path));
            }
            let truncated = items.len() > MAX_TREE_PAGES;
            items.truncate(MAX_TREE_PAGES);
            // 画面のツリーの見出し（ページの置き場のディレクトリ。名前順で重複無し）。
            let mut scopes: Vec<String> = index
                .items
                .iter()
                .filter_map(|i| i.path.rsplit_once('/').map(|(dir, _)| dir.to_string()))
                .collect();
            scopes.sort();
            scopes.dedup();
            Ok(KnowledgeTree {
                root: root.display().to_string(),
                initialized,
                scope,
                q,
                scopes,
                items,
                inbox_count: ops_kb::inbox_list(&root).len(),
                truncated,
                generated_at: Some(index.generated_at),
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

// ---------------------------------------------------------------------------
// GET /knowledge/page
// ---------------------------------------------------------------------------

async fn page(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    let query = QueryParams::parse(raw.as_deref(), &["path"])?;
    let requested = query.single("path")?.unwrap_or_default().to_string();
    let root = root_of(&state)?;
    let view = state
        .blocking(move |_| {
            let path = page_path(&requested)?;
            let raw = ops_kb::read_page(&root, &path).ok_or_else(|| page_not_found(&path))?;
            let too_large = raw.len() > kb::MAX_PAGE_BYTES;
            let (front, body) = kb::front_matter(&raw);
            let html = if too_large {
                String::new()
            } else {
                render_markdown(body, &path)
            };
            Ok(KnowledgePage {
                root: root.display().to_string(),
                title: kb::title_of(&raw, &path),
                tags: front.tags,
                scope: front.scope,
                sources: front.sources,
                confidence: front.confidence,
                updated: front.updated,
                history: ops_kb::history(&root, &path),
                etag: ops_kb::etag(&root, &path),
                raw: if too_large { String::new() } else { raw },
                html,
                path,
                too_large,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

// ---------------------------------------------------------------------------
// PUT /knowledge/page（管理系）
// ---------------------------------------------------------------------------

async fn put_page(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let request: KnowledgePagePutBody = read_json(body, false).await?;
    let root = root_of(&state)?;
    let result = state
        .blocking(move |_| {
            require_kb(&root)?;
            let path = page_path(&request.path)?;
            if kb::is_inbox(&path) {
                return Err(ApiProblem::path_forbidden(
                    "`_inbox/` の候補は編集できません（accept か reject を使う）",
                ));
            }
            let message = request
                .message
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("knowledge: {path}"));
            let edit = PageEdit {
                path,
                body: Some(request.body.clone()),
                etag: request.etag.clone().filter(|e| !e.trim().is_empty()),
                message,
                author: (
                    kb::HUMAN_AUTHOR_NAME.to_string(),
                    kb::HUMAN_AUTHOR_EMAIL.to_string(),
                ),
            };
            match ops_kb::commit_page(&root, &edit) {
                WriteOutcome::Written {
                    sha,
                    etag,
                    unchanged,
                } => {
                    // ADR-0047 D3: 書いたら必ず索引を作り直す。
                    let _ = ops_kb::reindex(&root);
                    tracing::info!(
                        who = "admin",
                        op = "knowledge_put",
                        path = %edit.path,
                        unchanged,
                        "admin: knowledge page committed"
                    );
                    Ok(KnowledgePageResult {
                        path: edit.path,
                        etag,
                        sha,
                        unchanged,
                    })
                }
                WriteOutcome::EtagMismatch { etag } => Err(etag_mismatch(etag)),
                WriteOutcome::Missing => Err(page_not_found(&edit.path)),
                WriteOutcome::Failed { detail } => Err(knowledge_unavailable(detail)),
            }
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

// ---------------------------------------------------------------------------
// GET /knowledge/inbox
// ---------------------------------------------------------------------------

async fn inbox(
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    let root = root_of(&state)?;
    let view = state
        .blocking(move |_| {
            let initialized = ops_kb::exists(&root);
            let items = if initialized {
                ops_kb::inbox_list(&root)
                    .into_iter()
                    .map(|item| KnowledgeCandidate {
                        html: render_markdown(&item.body, &item.path),
                        target_exists: root.join(&item.target).exists(),
                        id: item.id,
                        path: item.path,
                        title: item.title,
                        tags: item.tags,
                        scope: item.scope,
                        sources: item.sources,
                        confidence: item.confidence,
                        target: item.target,
                        created: item.created,
                        body: item.body,
                        op: item.op.map(|o| o.as_str().to_string()),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            Ok(KnowledgeInbox {
                root: root.display().to_string(),
                initialized,
                items,
            })
        })
        .await?;
    Ok(json_response(StatusCode::OK, &view))
}

// ---------------------------------------------------------------------------
// POST /knowledge/inbox/{id}/{accept,reject}（管理系）
// ---------------------------------------------------------------------------

async fn accept(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    body: Body,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let request: KnowledgeAcceptBody = read_json(body, true).await?;
    let root = root_of(&state)?;
    let result = state
        .blocking(move |_| {
            require_kb(&root)?;
            // id の境界（`/`・`..` は 403）。
            ops_kb::inbox_path(&id).map_err(path_problem)?;
            if let Some(path) = request.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
                page_path(path)?;
            }
            match ops_kb::inbox_accept(&root, &id, request.path.as_deref(), request.overwrite) {
                InboxOutcome::Accepted { path, sha, etag } => {
                    tracing::info!(who = "admin", op = "knowledge_accept", id = %id, path = %path, "admin: knowledge candidate accepted");
                    Ok(KnowledgePageResult {
                        path,
                        etag,
                        sha,
                        unchanged: false,
                    })
                }
                InboxOutcome::Missing => Err(candidate_not_found(&id)),
                InboxOutcome::Exists { path } => Err(ApiProblem::new(
                    StatusCode::CONFLICT,
                    "page_exists",
                    format!("page already exists: {path}（上書きするなら overwrite: true）"),
                )),
                InboxOutcome::Failed { detail } => Err(knowledge_unavailable(detail)),
                InboxOutcome::Rejected { .. } => Err(knowledge_unavailable("unexpected outcome")),
            }
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

async fn reject(
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Params(id): Params<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> ApiResult {
    no_query(&raw)?;
    require_admin(&state, &headers)?;
    let root = root_of(&state)?;
    let result = state
        .blocking(move |_| {
            require_kb(&root)?;
            ops_kb::inbox_path(&id).map_err(path_problem)?;
            match ops_kb::inbox_reject(&root, &id) {
                InboxOutcome::Rejected { sha } => {
                    tracing::info!(who = "admin", op = "knowledge_reject", id = %id, "admin: knowledge candidate rejected");
                    Ok(KnowledgeRejectResult { id, sha })
                }
                InboxOutcome::Missing => Err(candidate_not_found(&id)),
                InboxOutcome::Failed { detail } => Err(knowledge_unavailable(detail)),
                other => Err(knowledge_unavailable(format!("unexpected outcome: {other:?}"))),
            }
        })
        .await?;
    Ok(json_response(StatusCode::OK, &result))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// パスの境界は `task_core::knowledge` と同じ（403 / 422 / 400 に写す）。
    #[test]
    fn page_paths_map_to_the_right_problem() {
        assert_eq!(page_path("user/a.md").expect("ok"), "user/a.md");
        assert_eq!(
            page_path("../escape.md").err().map(|p| p.code()),
            Some("path_forbidden")
        );
        assert_eq!(
            page_path("a.txt").err().map(|p| p.code()),
            Some("validation")
        );
        assert_eq!(page_path(" ").err().map(|p| p.code()), Some("bad_request"));
        assert_eq!(
            ops_kb::inbox_path("../../etc/passwd")
                .err()
                .map(path_problem)
                .map(|p| p.code()),
            Some("path_forbidden")
        );
    }
}
