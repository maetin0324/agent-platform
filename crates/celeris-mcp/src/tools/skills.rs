//! ADR-0056 D2 / D3: `skills_list` / `skills_get` / `skills_put`（KB の `skills/<name>/SKILL.md`）。

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::McpScope;
use task_ops::knowledge as kb;

use super::{ToolDef, ToolError, ToolOutput, schema};
use crate::auth::AuthedClient;
use crate::state::McpState;

fn kb_root(state: &McpState) -> Result<PathBuf, ToolError> {
    state
        .knowledge_root
        .clone()
        .ok_or_else(|| ToolError::internal("knowledge base is not configured ([knowledge] root)"))
}

// ---- skills_list ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SkillSummaryView {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListOutput {
    pub items: Vec<SkillSummaryView>,
}

async fn list_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    _args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let root = kb_root(state)?;
    let items = state
        .blocking(move |_store| {
            kb::skills_list(&root)
                .into_iter()
                .map(|s| SkillSummaryView {
                    name: s.name,
                    description: s.description,
                })
                .collect::<Vec<_>>()
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
        name: "skills_list",
        description: "KB の skills/ にある skill の一覧（name / description）。",
        scope: McpScope::SkillsRead,
        input_schema: schema::<ListArgs>,
        call: list_call,
    }
}

// ---- skills_get ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetArgs {
    pub name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetOutput {
    pub name: String,
    pub skill_md: String,
    pub files: Vec<String>,
}

async fn get_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: GetArgs =
        serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let root = kb_root(state)?;
    let detail = state
        .blocking(move |_store| kb::skills_get(&root, &args.name))
        .await;
    match detail {
        Some(d) => ToolOutput::from_serialize(&GetOutput {
            name: d.name,
            skill_md: d.skill_md,
            files: d.files,
        }),
        None => Err(ToolError::not_found("skill was not found")),
    }
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
        name: "skills_get",
        description: "skill 1 件（SKILL.md 本文と付属ファイルの一覧）。",
        scope: McpScope::SkillsRead,
        input_schema: schema::<GetArgs>,
        call: get_call,
    }
}

// ---- skills_put ----

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SkillFileInput {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PutArgs {
    pub name: String,
    pub skill_md: String,
    #[serde(default)]
    pub files: Vec<SkillFileInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PutOutput {
    pub path: String,
}

async fn put_impl(
    state: &Arc<McpState>,
    client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: PutArgs =
        serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let root = kb_root(state)?;
    let source = format!("mcp:{}", client.id);
    let files: Vec<(String, String)> = args
        .files
        .into_iter()
        .map(|f| (f.path, f.content))
        .collect();
    let outcome = state
        .blocking(move |_store| {
            kb::skills_put(&root, &args.name, &args.skill_md, &files, Some(&source))
        })
        .await;
    match outcome {
        Ok(path) => ToolOutput::from_serialize(&PutOutput { path }),
        Err(e) => Err(ToolError::invalid_params(e.to_string())),
    }
}

fn put_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(put_impl(state, client, args))
}

pub fn put_def() -> ToolDef {
    ToolDef {
        name: "skills_put",
        description: "KB の skills/<name>/SKILL.md（＋付属ファイル）を直接書く（frontmatter に name / description 必須。mount されるまで何にも効かない）。",
        scope: McpScope::SkillsWrite,
        input_schema: schema::<PutArgs>,
        call: put_call,
    }
}
