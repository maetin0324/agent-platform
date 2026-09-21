//! ADR-0056 D2: `knowledge_list` / `knowledge_search` / `knowledge_get` / `knowledge_propose`。

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::McpScope;
use task_core::knowledge::Confidence;
use task_ops::knowledge as kb;

use super::{ToolDef, ToolError, ToolOutput, clamp_limit, schema};
use crate::auth::AuthedClient;
use crate::state::McpState;

fn kb_root(state: &McpState) -> Result<PathBuf, ToolError> {
    state
        .knowledge_root
        .clone()
        .ok_or_else(|| ToolError::internal("knowledge base is not configured ([knowledge] root)"))
}

// ---- knowledge_list ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListOutput {
    pub items: Vec<task_core::knowledge::IndexItem>,
}

async fn list_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: ListArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let root = kb_root(state)?;
    let limit = clamp_limit(args.limit);
    let scope = args.scope;
    let tag = args.tag;
    let items = state
        .blocking(move |_store| {
            let index = kb::ensure_index(&root);
            let mut items: Vec<_> = index
                .items
                .into_iter()
                .filter(|i| match scope.as_deref() {
                    Some(s) => task_core::knowledge::in_scope(i, s),
                    None => true,
                })
                .filter(|i| match tag.as_deref() {
                    Some(t) => i.tags.iter().any(|x| x == t),
                    None => true,
                })
                .collect();
            items.truncate(limit);
            items
        })
        .await;
    ToolOutput::from_serialize(&ListOutput { items })
}

fn list_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(list_impl(state, client, args))
}

pub fn list_def() -> ToolDef {
    ToolDef {
        name: "knowledge_list",
        description: "知識ベースの索引（index.json）を一覧する（path / title / tags / scope / updated / confidence）。",
        scope: McpScope::KnowledgeRead,
        input_schema: schema::<ListArgs>,
        call: list_call,
    }
}

// ---- knowledge_search ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    pub query: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchOutput {
    pub items: Vec<task_core::knowledge::SearchHit>,
}

async fn search_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: SearchArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let root = kb_root(state)?;
    let limit = clamp_limit(args.limit);
    let items = state
        .blocking(move |_store| kb::search(&root, &args.query, args.scope.as_deref(), limit))
        .await;
    ToolOutput::from_serialize(&SearchOutput { items })
}

fn search_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(search_impl(state, client, args))
}

pub fn search_def() -> ToolDef {
    ToolDef {
        name: "knowledge_search",
        description: "`celerisctl knowledge search` と同じ検索（索引の tags/title と本文の全文一致）。",
        scope: McpScope::KnowledgeRead,
        input_schema: schema::<SearchArgs>,
        call: search_call,
    }
}

// ---- knowledge_get ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetArgs {
    pub path: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetOutput {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    pub body: String,
}

async fn get_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: GetArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let root = kb_root(state)?;
    let path = task_core::knowledge::page_path(&args.path)
        .map_err(|e| ToolError::invalid_params(format!("{:?}: {e}", args.path)))?;
    if task_core::knowledge::RETIRED_DIR == path.as_str()
        || path.starts_with(&format!("{}/", task_core::knowledge::RETIRED_DIR))
    {
        return Err(ToolError::not_found(format!("{path} was not found")));
    }
    let path_for_blocking = path.clone();
    let raw = state
        .blocking(move |_store| kb::read_page(&root, &path_for_blocking))
        .await;
    let Some(raw) = raw else {
        return Err(ToolError::not_found(format!("{path} was not found")));
    };
    let (front, body) = task_core::knowledge::front_matter(&raw);
    ToolOutput::from_serialize(&GetOutput {
        path,
        title: front.title,
        tags: front.tags,
        scope: front.scope,
        sources: front.sources,
        confidence: front.confidence,
        body: body.to_string(),
    })
}

fn get_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(get_impl(state, client, args))
}

pub fn get_def() -> ToolDef {
    ToolDef {
        name: "knowledge_get",
        description: "知識ページ 1 枚の本文（Markdown）とメタを返す。`_retired` は not_found。",
        scope: McpScope::KnowledgeRead,
        input_schema: schema::<GetArgs>,
        call: get_call,
    }
}

// ---- knowledge_propose ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposeArgs {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub scope: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub confidence: Option<Confidence>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProposeOutput {
    pub path: String,
    pub id: String,
}

async fn propose_impl(
    state: &Arc<McpState>,
    client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: ProposeArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let root = kb_root(state)?;
    let mut sources = args.sources;
    let mcp_source = format!("mcp:{}", client.id);
    if !sources.iter().any(|s| s == &mcp_source) {
        sources.push(mcp_source);
    }
    let request = kb::RecordRequest {
        title: args.title,
        scope: args.scope,
        tags: args.tags,
        sources,
        confidence: args.confidence,
        body: args.body,
        path: None,
    };
    let outcome = state.blocking(move |_store| kb::record(&root, &request)).await;
    match outcome {
        Ok(o) => ToolOutput::from_serialize(&ProposeOutput { path: o.path, id: o.id }),
        Err(kb::RecordError::Secret(why)) => {
            Err(ToolError::rejected(format!("秘密が含まれています: {why}")))
        }
        Err(e) => Err(ToolError::invalid_params(e.to_string())),
    }
}

fn propose_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(propose_impl(state, client, args))
}

pub fn propose_def() -> ToolDef {
    ToolDef {
        name: "knowledge_propose",
        description: "`_inbox/` に知識の候補を置く（出典に `mcp:<client_id>` を必ず足す。秘密は拒否。直接コミットはしない）。",
        scope: McpScope::KnowledgePropose,
        input_schema: schema::<ProposeArgs>,
        call: propose_call,
    }
}
