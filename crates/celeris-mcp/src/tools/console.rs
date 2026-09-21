//! ADR-0056 D2: `console_instruct` / `console_reply`（人の発言と同じ経路で CoS に渡す）。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{COS_ID, MessageRole, ProjectId, TaskId, TaskStore};

use super::{ToolDef, ToolError, ToolOutput, schema};
use crate::auth::AuthedClient;
use crate::state::McpState;

// ---- console_instruct ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstructArgs {
    pub text: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InstructOutput {
    pub message_id: String,
    pub task_id: String,
}

async fn instruct_impl(
    state: &Arc<McpState>,
    client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: InstructArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    if args.text.trim().is_empty() {
        return Err(ToolError::invalid_params("text must not be blank"));
    }
    let project_id = match args.project_id.as_deref() {
        Some(s) => Some(
            s.parse::<ProjectId>()
                .map_err(|_| ToolError::invalid_params(format!("{s:?} is not a project id")))?,
        ),
        None => None,
    };
    let roles = state.roles.clone();
    let genres = state.genres.clone();
    let conversation_genre = state.conversation_genre.clone();
    let author = format!("mcp:{}", client.id);
    let started = state
        .blocking(move |store| {
            task_ops::conversation::start_as(
                store,
                COS_ID,
                project_id,
                &author,
                &args.text,
                &roles,
                &genres,
                &conversation_genre,
                time::OffsetDateTime::now_utc(),
            )
        })
        .await
        .map_err(|e| ToolError::invalid_params(e.to_string()))?;
    ToolOutput::from_serialize(&InstructOutput {
        message_id: started.message.id.to_string(),
        task_id: started.task.id.to_string(),
    })
}

fn instruct_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(instruct_impl(state, client, args))
}

pub fn instruct_def() -> ToolDef {
    ToolDef {
        name: "console_instruct",
        description: "人の発言と同じ経路で CoS に渡す（発言の author は mcp:<client_id>）。案件化・タスク化は CoS が actions で作る。",
        scope: task_core::McpScope::ConsoleInstruct,
        input_schema: schema::<InstructArgs>,
        call: instruct_call,
    }
}

// ---- console_reply ----

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplyArgs {
    pub task_id: String,
    #[serde(default)]
    pub wait_secs: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ActionSummary {
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone_id: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReplyOutput {
    Pending,
    Done {
        reply: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        actions: Vec<ActionSummary>,
    },
    Failed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply: Option<String>,
    },
    Cancelled,
}

/// 上限 60 秒（ADR-0056 D2）。
const MAX_WAIT_SECS: u64 = 60;
const POLL_INTERVAL: Duration = Duration::from_millis(250);

async fn reply_impl(
    state: &Arc<McpState>,
    _client: &AuthedClient,
    args: serde_json::Value,
) -> Result<ToolOutput, ToolError> {
    let args: ReplyArgs = serde_json::from_value(args).map_err(|e| ToolError::invalid_params(e.to_string()))?;
    let task_id: TaskId = args
        .task_id
        .parse()
        .map_err(|_| ToolError::invalid_params(format!("{:?} is not a task id", args.task_id)))?;
    let wait = Duration::from_secs(args.wait_secs.unwrap_or(0).min(MAX_WAIT_SECS));
    let out = state
        .blocking(move |store| -> Result<ReplyOutput, ToolError> {
            let deadline = Instant::now() + wait;
            loop {
                let Some(task) = store.get(task_id).map_err(|e| ToolError::internal(e.to_string()))? else {
                    return Err(ToolError::not_found(format!("task {task_id} was not found")));
                };
                if task.status.is_terminal() {
                    return terminal_reply(store, &task);
                }
                if Instant::now() >= deadline {
                    return Ok(ReplyOutput::Pending);
                }
                std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())).max(Duration::from_millis(1)));
            }
        })
        .await?;
    ToolOutput::from_serialize(&out)
}

fn terminal_reply(store: &dyn TaskStore, task: &task_core::Task) -> Result<ReplyOutput, ToolError> {
    use task_core::Status;
    match task.status {
        Status::Done => {
            let messages = store
                .message_page(Some(COS_ID), task.project_id, None, 4_000)
                .map_err(|e| ToolError::internal(e.to_string()))?;
            let reply_message = messages
                .into_iter()
                .rfind(|m| m.role == MessageRole::Node && m.task_id == Some(task.id));
            let Some(reply_message) = reply_message else {
                return Ok(ReplyOutput::Done {
                    reply: String::new(),
                    actions: Vec::new(),
                });
            };
            let actions = reply_message
                .metadata
                .map(|m| {
                    m.actions_executed
                        .into_iter()
                        .map(|a| ActionSummary {
                            kind: a.kind,
                            summary: a.summary,
                            task_id: a.task_id.map(|t| t.to_string()),
                            project_id: a.project_id.map(|p| p.to_string()),
                            milestone_id: a.milestone_id.map(|m| m.to_string()),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(ReplyOutput::Done {
                reply: reply_message.text,
                actions,
            })
        }
        Status::Failed => {
            let messages = store
                .message_page(Some(COS_ID), task.project_id, None, 4_000)
                .map_err(|e| ToolError::internal(e.to_string()))?;
            let reply = messages
                .into_iter()
                .rfind(|m| m.role == MessageRole::Node && m.task_id == Some(task.id))
                .map(|m| m.text);
            Ok(ReplyOutput::Failed { reply })
        }
        _ => Ok(ReplyOutput::Cancelled),
    }
}

fn reply_call<'a>(
    state: &'a Arc<McpState>,
    client: &'a AuthedClient,
    args: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>> {
    Box::pin(reply_impl(state, client, args))
}

pub fn reply_def() -> ToolDef {
    ToolDef {
        name: "console_reply",
        description: "console_instruct の対話 run の返事と、起きた actions の結果を返す（wait_secs 上限 60）。",
        scope: task_core::McpScope::ConsoleInstruct,
        input_schema: schema::<ReplyArgs>,
        call: reply_call,
    }
}
