//! ADR-0056 D2: `tasks_list` / `tasks_get`（読むだけ。作るのは `console_instruct` 経由）。
//!
//! Phase 101 追記: `task_comment` / `task_answer`（scope `tasks:interact`）、`task_retry` /
//! `task_cancel`（scope `tasks:control`）、`task_approve` / `task_reject`（scope `tasks:decide`）。
//! 外部エージェント（ChatGPT、Remote Desktop Commander 経由）が汎用 curl で HTTP API を叩かず、
//! MCP client の scope で許される操作だけをできるようにする（ADR-0056 Phase 101 追記）。
//! いずれも HTTP ハンドラ（`task-api::handlers`）と**同じ** `task-ops` の関数を呼ぶ（ロジックの
//! 二重実装をしない）。書き込みの主体は `mcp:<client_id>`（`knowledge_propose` と同じ流儀。
//! `task_retry` / `task_cancel` は下敷きの `task-ops` 関数がそもそも actor を持たないので、監査は
//! `mcp_calls` に任せる — `docs/mcp.md` §4 参照）。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{Event, ListFilter, ListOrder, McpScope, ReportStore, Status, TaskId, TaskStore};
use task_ops::OpsError;
use time::OffsetDateTime;

use super::{ToolDef, ToolError, ToolOutput, clamp_limit, schema};
use crate::auth::AuthedClient;
use crate::state::McpState;

/// `OpsError` → `ToolError`（Phase 101。`NotFound`/`*NotFound` は not_found、状態や検証の不整合は
/// invalid_params、それ以外（`Store`）は internal。`docs/gui/api.md` の `ops_problem` と同じ分類を
/// JSON-RPC のエラーコードに写しただけ）。
fn map_ops_err(e: OpsError) -> ToolError {
    match e {
        OpsError::NotFound(id) => ToolError::not_found(format!("task {id} was not found")),
        OpsError::ProjectNotFound(id) => ToolError::not_found(format!("project {id} was not found")),
        OpsError::MilestoneNotFound(id) => ToolError::not_found(format!("milestone {id} was not found")),
        OpsError::InvalidState { .. } | OpsError::Validation(_) | OpsError::Conflict { .. } | OpsError::InvalidLifecycle { .. } => {
            ToolError::invalid_params(e.to_string())
        }
        OpsError::Store(_) => ToolError::internal(e.to_string()),
    }
}

fn parse_task_id(raw: &str) -> Result<TaskId, ToolError> {
    raw.parse::<TaskId>()
        .map_err(|_| ToolError::invalid_params(format!("{raw:?} is not a task id")))
}

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

// ---- Phase 101: task_comment (scope tasks:interact) ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentArgs {
    pub id: String,
    pub text: String,
}

async fn comment_impl(
    state: &Arc<McpState>,
    client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: CommentArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let id = parse_task_id(&args.id)?;
    let author = format!("mcp:{}", client.id);
    let result = state
        .blocking(move |store| -> Result<task_ops::comment::CommentResult, ToolError> {
            task_ops::comment::post_human_comment_as(store, id, Some(author), args.text, OffsetDateTime::now_utc())
                .map_err(map_ops_err)
        })
        .await?;
    ToolOutput::from_serialize(&result)
}

fn comment_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(comment_impl(state, client, args))
}

pub fn comment_def() -> ToolDef {
    ToolDef {
        name: "task_comment",
        description: "Post a comment on a task (same effect table as POST /tasks/{id}/comments: interrupts running/reviewing tasks, answers a blocked one, otherwise just records).",
        scope: McpScope::TasksInteract,
        input_schema: schema::<CommentArgs>,
        call: comment_call,
    }
}

// ---- Phase 101: task_answer (scope tasks:interact) ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerArgs {
    pub id: String,
    /// 既存の `AnswerBody.answer` と同じ名前（ADR-0056 Phase 101: 「`answer` の既存の body 形に合わせる」）。
    pub answer: String,
    #[serde(default)]
    pub expected_status: Option<Status>,
}

async fn answer_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: AnswerArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    if args.answer.trim().is_empty() {
        return Err(ToolError::invalid_params("answer must not be blank"));
    }
    let id = parse_task_id(&args.id)?;
    let result = state
        .blocking(move |store| {
            task_ops::gate::answer(store, id, args.answer, args.expected_status).map_err(map_ops_err)
        })
        .await?;
    ToolOutput::from_serialize(&result)
}

fn answer_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(answer_impl(state, client, args))
}

pub fn answer_def() -> ToolDef {
    ToolDef {
        name: "task_answer",
        description: "Answer a blocked task's pending question (same as POST /tasks/{id}/answer).",
        scope: McpScope::TasksInteract,
        input_schema: schema::<AnswerArgs>,
        call: answer_call,
    }
}

// ---- Phase 101: task_retry (scope tasks:control) ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryArgs {
    pub id: String,
}

async fn retry_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: RetryArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let id = parse_task_id(&args.id)?;
    let result = state
        .blocking(move |store| {
            task_ops::retry::retry_task(store, id, false, OffsetDateTime::now_utc()).map_err(map_ops_err)
        })
        .await?;
    ToolOutput::from_serialize(&result)
}

fn retry_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(retry_impl(state, client, args))
}

pub fn retry_def() -> ToolDef {
    ToolDef {
        name: "task_retry",
        description: "Duplicate a failed or cancelled task into a new ready/draft one (same as POST /tasks/{id}/retry, accept=false).",
        scope: McpScope::TasksControl,
        input_schema: schema::<RetryArgs>,
        call: retry_call,
    }
}

// ---- Phase 101: task_cancel (scope tasks:control) ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelArgs {
    pub id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

async fn cancel_impl(
    state: &Arc<McpState>,
    client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: CancelArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let id = parse_task_id(&args.id)?;
    let author = format!("mcp:{}", client.id);
    let reason = args.reason.filter(|r| !r.trim().is_empty());
    let result = state
        .blocking(move |store| -> Result<task_ops::gate::TransitionResult, ToolError> {
            if let Some(reason) = reason {
                // ADR-0044 D2 の `post_node_comment` と同じ「人を起こさない」記録（`reason` の監査）。
                // `gate::cancel` 自体には actor / reason を運ぶ欄が無いので、これで補う。
                task_ops::comment::post_node_comment(
                    store,
                    id,
                    Some(author),
                    None,
                    reason,
                    OffsetDateTime::now_utc(),
                )
                .map_err(map_ops_err)?;
            }
            task_ops::gate::cancel(store, id, None).map_err(map_ops_err)
        })
        .await?;
    ToolOutput::from_serialize(&result)
}

fn cancel_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(cancel_impl(state, client, args))
}

pub fn cancel_def() -> ToolDef {
    ToolDef {
        name: "task_cancel",
        description: "Cancel a non-terminal task (same as POST /tasks/{id}/cancel). An optional reason is recorded as a comment first (author mcp:<client_id>, does not wake anyone).",
        scope: McpScope::TasksControl,
        input_schema: schema::<CancelArgs>,
        call: cancel_call,
    }
}

// ---- Phase 101: task_approve (scope tasks:decide) ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveArgs {
    pub id: String,
    #[serde(default)]
    pub note: Option<String>,
}

async fn approve_impl(
    state: &Arc<McpState>,
    client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: ApproveArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let id = parse_task_id(&args.id)?;
    let by = format!("mcp:{}", client.id);
    let result = state
        .blocking(move |store| task_ops::gate::approve_as(store, id, &by, args.note, None).map_err(map_ops_err))
        .await?;
    ToolOutput::from_serialize(&result)
}

fn approve_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(approve_impl(state, client, args))
}

pub fn approve_def() -> ToolDef {
    ToolDef {
        name: "task_approve",
        description: "Approve a draft task or an approval-kind task waiting for a decision (same as POST /tasks/{id}/approve). Records by=mcp:<client_id>.",
        scope: McpScope::TasksDecide,
        input_schema: schema::<ApproveArgs>,
        call: approve_call,
    }
}

// ---- Phase 101: task_reject (scope tasks:decide) ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RejectArgs {
    pub id: String,
    pub reason: String,
}

async fn reject_impl(
    state: &Arc<McpState>,
    client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: RejectArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    if args.reason.trim().is_empty() {
        return Err(ToolError::invalid_params("reason must not be blank"));
    }
    let id = parse_task_id(&args.id)?;
    let by = format!("mcp:{}", client.id);
    let result = state
        .blocking(move |store| {
            task_ops::gate::reject_as(store, id, &by, Some(args.reason), None).map_err(map_ops_err)
        })
        .await?;
    ToolOutput::from_serialize(&result)
}

fn reject_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(reject_impl(state, client, args))
}

pub fn reject_def() -> ToolDef {
    ToolDef {
        name: "task_reject",
        description: "Reject an approval-kind task waiting for a decision (same as POST /tasks/{id}/reject). reason is required. Records by=mcp:<client_id>.",
        scope: McpScope::TasksDecide,
        input_schema: schema::<RejectArgs>,
        call: reject_call,
    }
}
