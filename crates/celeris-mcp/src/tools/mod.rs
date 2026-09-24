//! ADR-0056 D2: 道具（tools）の一覧・ディスパッチ。各領域の実装は同じディレクトリの各ファイルにある。
//!
//! すべての道具は `(&Arc<McpState>, &AuthedClient, serde_json::Value) -> Result<ToolOutput, ToolError>`
//! の形（非同期）。判断・検証は道具の中に閉じ、JSON-RPC の形への変換は `crate::rpc` が行う。

pub mod console;
pub mod knowledge;
pub mod org;
pub mod projects;
pub mod skills;
pub mod tasks;

use std::sync::Arc;

use serde::Serialize;
use task_core::McpScope;

use crate::auth::AuthedClient;
use crate::state::McpState;

/// 道具 1 件の結果（MCP の `content: [{type: "text", text: <json文字列>}]` に写す。実用のため
/// `structuredContent` も一緒に返す）。
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub value: serde_json::Value,
}

impl ToolOutput {
    pub fn from_value(value: serde_json::Value) -> Self {
        Self { value }
    }

    pub fn from_serialize<T: Serialize>(value: &T) -> Result<Self, ToolError> {
        Ok(Self {
            value: serde_json::to_value(value)
                .map_err(|e| ToolError::internal(format!("シリアライズに失敗しました: {e}")))?,
        })
    }
}

/// 道具の失敗。`crate::rpc` が JSON-RPC のエラーに写す。
#[derive(Debug, Clone)]
pub struct ToolError {
    pub code: ToolErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolErrorCode {
    /// 引数が不正（JSON-RPC `-32602`）。
    InvalidParams,
    /// 探した対象が無い（`-32001`。JSON-RPC の予約外なのでこのサーバー固有）。
    NotFound,
    /// 決定的な検査に落ちた（秘密を含む等。`-32002`）。
    Rejected,
    /// 内部エラー（`-32603`）。
    Internal,
}

impl ToolError {
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: ToolErrorCode::InvalidParams,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: ToolErrorCode::NotFound,
            message: message.into(),
        }
    }

    pub fn rejected(message: impl Into<String>) -> Self {
        Self {
            code: ToolErrorCode::Rejected,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ToolErrorCode::Internal,
            message: message.into(),
        }
    }
}

/// 道具の呼び出し関数の型（`fn(&state, &client, args) -> 結果の future`）。
pub type ToolCallFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>>;
pub type ToolCallFn =
    for<'a> fn(&'a Arc<McpState>, &'a AuthedClient, serde_json::Value) -> ToolCallFuture<'a>;

/// 道具 1 つの定義（`tools/list` に出す形と、呼び出す関数）。
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub scope: McpScope,
    pub input_schema: fn() -> serde_json::Value,
    pub call: ToolCallFn,
}

pub(crate) fn schema<T: schemars::JsonSchema>() -> serde_json::Value {
    let schema = schemars::schema_for!(T);
    serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({}))
}

/// このサーバーが知っている全ての道具（ADR-0056 D2）。`tools/list` はここをスコープで濾す。
pub fn all() -> Vec<ToolDef> {
    vec![
        knowledge::list_def(),
        knowledge::search_def(),
        knowledge::get_def(),
        knowledge::propose_def(),
        tasks::list_def(),
        tasks::get_def(),
        tasks::comment_def(),
        tasks::answer_def(),
        tasks::retry_def(),
        tasks::cancel_def(),
        tasks::approve_def(),
        tasks::reject_def(),
        projects::list_def(),
        projects::get_def(),
        console::instruct_def(),
        console::reply_def(),
        org::list_def(),
        org::get_def(),
        org::create_node_def(),
        org::mount_skill_def(),
        org::unmount_skill_def(),
        skills::list_def(),
        skills::get_def(),
        skills::put_def(),
    ]
}

pub fn find(name: &str) -> Option<ToolDef> {
    all().into_iter().find(|t| t.name == name)
}

/// 既定の `limit`（ADR-0056 D2）。
pub const DEFAULT_LIMIT: usize = 20;
/// `limit` の上限。
pub const MAX_LIMIT: usize = 100;

/// 引数の `limit` を既定・上限にはめる。
pub fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}
