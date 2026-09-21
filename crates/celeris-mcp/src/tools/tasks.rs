//! ADR-0056 D2: `tasks_list` / `tasks_get`（読むだけ。作るのは `console_instruct` 経由）。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{Event, ListFilter, ListOrder, McpScope, ReportStore, Status, TaskId, TaskStore};

use super::{ToolDef, ToolError, ToolOutput, clamp_limit, schema};
use crate::auth::AuthedClient;
use crate::state::McpState;

// ---- tasks_list ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TaskSummary {
    pub id: String,
    pub title: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListOutput {
    pub items: Vec<TaskSummary>,
}

async fn list_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: ListArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let limit = clamp_limit(args.limit);
    let statuses = match args.status.as_deref() {
        Some(s) => vec![
            serde_json::from_value::<Status>(serde_json::Value::String(s.to_string()))
                .map_err(|_| ToolError::invalid_params(format!("unknown status {s:?}")))?,
        ],
        None => Vec::new(),
    };
    let project_id = match args.project_id.as_deref() {
        Some(s) => Some(
            s.parse::<task_core::ProjectId>()
                .map_err(|_| ToolError::invalid_params(format!("{s:?} is not a project id")))?,
        ),
        None => None,
    };
    let filter = ListFilter {
        statuses,
        project_id,
        ..ListFilter::default()
    };
    let page = state
        .blocking(move |store| store.list_page(&filter, ListOrder::UpdatedDesc, None, limit))
        .await
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let items = page
        .items
        .into_iter()
        .map(|t| TaskSummary {
            id: t.id.to_string(),
            title: t.title,
            status: t.status,
            assignee: t.assignee,
            project_id: t.project_id.map(|p| p.to_string()),
            updated_at: t.updated_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        })
        .collect();
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
        name: "tasks_list",
        description: "タスクの一覧（状態・担当・案件で絞れる）。",
        scope: McpScope::TasksRead,
        input_schema: schema::<ListArgs>,
        call: list_call,
    }
}

// ---- tasks_get ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetArgs {
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ArtifactSummary {
    pub name: String,
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetOutput {
    pub id: String,
    pub title: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// 直近の報告の要約（あれば）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_report: Option<String>,
    /// 成果物の一覧（run の生ログは含まない）。
    pub artifacts: Vec<ArtifactSummary>,
}

async fn get_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: GetArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let id: TaskId = args
        .id
        .parse()
        .map_err(|_| ToolError::invalid_params(format!("{:?} is not a task id", args.id)))?;
    let out = state
        .blocking(move |store| -> Result<Option<GetOutput>, ToolError> {
            let Some(task) = store.get(id).map_err(|e| ToolError::internal(e.to_string()))? else {
                return Ok(None);
            };
            let latest_report = store
                .report_list(&task_core::ReportFilter {
                    task_id: Some(id),
                    limit: 1,
                    ..Default::default()
                })
                .map_err(|e| ToolError::internal(e.to_string()))?
                .into_iter()
                .next()
                .map(|r| r.headline);
            let mut artifacts = Vec::new();
            let mut after_seq = None;
            loop {
                let rows = store
                    .event_rows_for(id, after_seq, 500)
                    .map_err(|e| ToolError::internal(e.to_string()))?;
                if rows.is_empty() {
                    break;
                }
                after_seq = rows.last().map(|r| r.seq);
                for row in &rows {
                    if let Event::ArtifactProduced { artifact, .. } = &row.event
                        && !artifacts.iter().any(|a: &ArtifactSummary| a.path == artifact.path)
                    {
                        artifacts.push(ArtifactSummary {
                            name: artifact.name.clone(),
                            path: artifact.path.clone(),
                            kind: artifact.kind.clone(),
                        });
                    }
                }
                if rows.len() < 500 {
                    break;
                }
            }
            Ok(Some(GetOutput {
                id: task.id.to_string(),
                title: task.title,
                status: task.status,
                assignee: task.assignee,
                project_id: task.project_id.map(|p| p.to_string()),
                latest_report,
                artifacts,
            }))
        })
        .await?;
    match out {
        Some(o) => ToolOutput::from_serialize(&o),
        None => Err(ToolError::not_found(format!("task {} was not found", args.id))),
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
        name: "tasks_get",
        description: "タスク 1 件（状態・担当・直近の報告の要約・成果物一覧。run の生ログは含まない）。",
        scope: McpScope::TasksRead,
        input_schema: schema::<GetArgs>,
        call: get_call,
    }
}
