//! `claude-oauth`（ADR-0053 D1-1）: Claude Code の `.credentials.json` を使って Anthropic Messages API
//! を叩き、OpenAI 互換の要求/応答へ双方向に写す。
//!
//! - 要求: `messages`（`system` は分離、`tool`/`tool_calls` を `tool_result`/`tool_use` ブロックへ）、
//!   `tools`、`tool_choice`、`stream`。
//! - 応答: `content` ブロックの連結と `tool_use`、`stop_reason`、`usage`。
//! - stream: `content_block_delta` の `text_delta`/`input_json_delta` を OpenAI の delta chunk に写す。
//! - トークン更新: `expiresAt` を過ぎていれば送信前に、401 を受ければ 1 回だけ事後に。どちらも同じファイルへ書き戻す。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::config::ClaudeOauthConfig;
use crate::credentials::{
    self, CLAUDE_CREDENTIALS_FILE, ClaudeTokens, apply_claude_tokens, parse_claude_tokens,
};
use crate::neterr::safe_reqwest_error;
use crate::openai::{
    ChatCompletionResponse, ChatMessage, Delta, FunctionCall, FunctionCallDelta, MessageContent,
    ToolCall, ToolCallDelta, Usage,
};
use crate::sse::SseDecoder;

use super::{SendOutcome, SourceError};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
/// 送信前に更新する猶予（期限まで 60 秒を切っていたら先に更新する）。
const REFRESH_BUFFER_MS: i64 = 60_000;

pub fn account_dir(cfg: &ClaudeOauthConfig, account_id: &str) -> PathBuf {
    cfg.accounts_dir.join(account_id)
}

fn now_unix_ms() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp() * 1000
}

// ---------------------------------------------------------------------------
// 要求の写し（OpenAI → Anthropic）
// ---------------------------------------------------------------------------

fn map_tool_choice(choice: &crate::openai::ToolChoice) -> Value {
    match choice {
        crate::openai::ToolChoice::Mode(m) => match m.as_str() {
            "none" => json!({"type": "none"}),
            "required" => json!({"type": "any"}),
            _ => json!({"type": "auto"}),
        },
        crate::openai::ToolChoice::Named { function, .. } => {
            json!({"type": "tool", "name": function.name})
        }
    }
}

fn message_to_anthropic(msg: &ChatMessage) -> Option<Value> {
    match msg.role.as_str() {
        "system" => None, // 呼び出し側が別に集める
        "tool" => {
            let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
            let content = msg
                .content
                .as_ref()
                .map(MessageContent::as_text)
                .unwrap_or_default();
            Some(json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": tool_use_id, "content": content}]
            }))
        }
        "assistant" => {
            let mut blocks = Vec::new();
            if let Some(content) = &msg.content {
                let text = content.as_text();
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
            }
            for call in msg.tool_calls.iter().flatten() {
                let input: Value =
                    serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| json!({}));
                blocks.push(json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.function.name,
                    "input": input,
                }));
            }
            Some(json!({"role": "assistant", "content": blocks}))
        }
        // "user" とその他は user として扱う（Anthropic は user/assistant の 2 種）。
        _ => {
            let text = msg
                .content
                .as_ref()
                .map(MessageContent::as_text)
                .unwrap_or_default();
            Some(json!({"role": "user", "content": [{"type": "text", "text": text}]}))
        }
    }
}

/// OpenAI の `ChatCompletionRequest` を Anthropic Messages API の本文へ写す。
pub fn to_anthropic_body(req: &crate::openai::ChatCompletionRequest, model: &str) -> Value {
    let system: Vec<String> = req
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| {
            m.content
                .as_ref()
                .map(MessageContent::as_text)
                .unwrap_or_default()
        })
        .collect();
    let messages: Vec<Value> = req
        .messages
        .iter()
        .filter_map(message_to_anthropic)
        .collect();

    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": req.max_tokens.unwrap_or(4096),
        "stream": req.stream,
    });
    let obj = body
        .as_object_mut()
        .unwrap_or_else(|| unreachable!("body is always an object"));
    if !system.is_empty() {
        obj.insert("system".to_string(), json!(system.join("\n\n")));
    }
    if let Some(t) = req.temperature {
        obj.insert("temperature".to_string(), json!(t));
    }
    if let Some(t) = req.top_p {
        obj.insert("top_p".to_string(), json!(t));
    }
    if let Some(stop) = &req.stop {
        obj.insert("stop_sequences".to_string(), json!(stop));
    }
    if let Some(tools) = &req.tools {
        let mapped: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.function.name,
                    "description": t.function.description.clone().unwrap_or_default(),
                    "input_schema": t.function.parameters,
                })
            })
            .collect();
        obj.insert("tools".to_string(), json!(mapped));
    }
    if let Some(choice) = &req.tool_choice {
        obj.insert("tool_choice".to_string(), map_tool_choice(choice));
    }
    body
}

// ---------------------------------------------------------------------------
// 応答の写し（Anthropic → OpenAI、非 stream）
// ---------------------------------------------------------------------------

fn map_stop_reason(reason: &str) -> String {
    match reason {
        "end_turn" | "stop_sequence" => "stop",
        "tool_use" => "tool_calls",
        "max_tokens" => "length",
        other => other,
    }
    .to_string()
}

pub fn from_anthropic_response(
    body: &Value,
    requested_model: &str,
) -> Result<ChatCompletionResponse, SourceError> {
    if let Some(err) = body.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("upstream error");
        return Err(SourceError::Upstream {
            status: 200,
            summary: msg.chars().take(200).collect(),
        });
    }
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("chatcmpl-{}", ulid::Ulid::new()));
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in body
        .get("content")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                text.push_str(
                    block
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default(),
                );
            }
            Some("tool_use") => {
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let input = block.get("input").cloned().unwrap_or(json!({}));
                let call_id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                tool_calls.push(ToolCall {
                    id: call_id,
                    kind: "function".to_string(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::to_string(&input)
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                });
            }
            _ => {}
        }
    }
    let stop_reason = body
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("end_turn");
    let finish_reason = map_stop_reason(stop_reason);
    let usage = body.get("usage").map(|u| Usage {
        prompt_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        completion_tokens: u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        total_tokens: u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
            + u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
    });
    let message = ChatMessage {
        role: "assistant".to_string(),
        content: if text.is_empty() {
            None
        } else {
            Some(MessageContent::Text(text))
        },
        name: None,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
    };
    Ok(ChatCompletionResponse::new(
        id,
        requested_model.to_string(),
        message,
        Some(finish_reason),
        usage,
        time::OffsetDateTime::now_utc().unix_timestamp(),
    ))
}

// ---------------------------------------------------------------------------
// stream（Anthropic SSE → OpenAI chunk）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    ToolUse,
}

pub struct AnthropicStreamMapper {
    id: String,
    model: String,
    created: i64,
    next_tool_index: u32,
    block_kind: HashMap<u32, BlockKind>,
    tool_index_by_block: HashMap<u32, u32>,
    finish_reason: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl AnthropicStreamMapper {
    pub fn new(model: &str) -> Self {
        Self {
            id: format!("chatcmpl-{}", ulid::Ulid::new()),
            model: model.to_string(),
            created: time::OffsetDateTime::now_utc().unix_timestamp(),
            next_tool_index: 0,
            block_kind: HashMap::new(),
            tool_index_by_block: HashMap::new(),
            finish_reason: None,
            input_tokens: None,
            output_tokens: None,
        }
    }

    fn chunk(
        &self,
        delta: Delta,
        finish_reason: Option<String>,
    ) -> crate::openai::ChatCompletionChunk {
        let mut c = crate::openai::ChatCompletionChunk::new(
            &self.id,
            &self.model,
            self.created,
            delta,
            finish_reason,
        );
        if c.choices
            .first()
            .map(|ch| ch.finish_reason.is_some())
            .unwrap_or(false)
            && let (Some(i), Some(o)) = (self.input_tokens, self.output_tokens)
        {
            c.usage = Some(Usage {
                prompt_tokens: i,
                completion_tokens: o,
                total_tokens: i + o,
            });
        }
        c
    }

    /// 1 つの SSE イベントから 0 件以上の OpenAI chunk を作る。
    pub fn feed(&mut self, event: &str, data: &Value) -> Vec<crate::openai::ChatCompletionChunk> {
        match event {
            "message_start" => {
                if let Some(id) = data
                    .get("message")
                    .and_then(|m| m.get("id"))
                    .and_then(|v| v.as_str())
                {
                    self.id = id.to_string();
                }
                self.input_tokens = data
                    .get("message")
                    .and_then(|m| m.get("usage"))
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(|v| v.as_u64());
                vec![self.chunk(
                    Delta {
                        role: Some("assistant".to_string()),
                        ..Delta::default()
                    },
                    None,
                )]
            }
            "content_block_start" => {
                let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let block = data.get("content_block").cloned().unwrap_or_default();
                match block.get("type").and_then(|v| v.as_str()) {
                    Some("tool_use") => {
                        let tool_idx = self.next_tool_index;
                        self.next_tool_index += 1;
                        self.tool_index_by_block.insert(idx, tool_idx);
                        self.block_kind.insert(idx, BlockKind::ToolUse);
                        let id = block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        vec![self.chunk(
                            Delta {
                                tool_calls: Some(vec![ToolCallDelta {
                                    index: tool_idx,
                                    id: Some(id),
                                    kind: Some("function".to_string()),
                                    function: Some(FunctionCallDelta {
                                        name: Some(name),
                                        arguments: Some(String::new()),
                                    }),
                                }]),
                                ..Delta::default()
                            },
                            None,
                        )]
                    }
                    _ => {
                        self.block_kind.insert(idx, BlockKind::Text);
                        vec![]
                    }
                }
            }
            "content_block_delta" => {
                let idx = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let delta = data.get("delta").cloned().unwrap_or_default();
                match delta.get("type").and_then(|v| v.as_str()) {
                    Some("text_delta") => {
                        let text = delta
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        vec![self.chunk(
                            Delta {
                                content: Some(text),
                                ..Delta::default()
                            },
                            None,
                        )]
                    }
                    Some("input_json_delta") => {
                        let partial = delta
                            .get("partial_json")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let tool_idx = self.tool_index_by_block.get(&idx).copied().unwrap_or(0);
                        if self.block_kind.get(&idx) == Some(&BlockKind::ToolUse) {
                            vec![self.chunk(
                                Delta {
                                    tool_calls: Some(vec![ToolCallDelta {
                                        index: tool_idx,
                                        function: Some(FunctionCallDelta {
                                            arguments: Some(partial),
                                            name: None,
                                        }),
                                        ..ToolCallDelta::default()
                                    }]),
                                    ..Delta::default()
                                },
                                None,
                            )]
                        } else {
                            vec![]
                        }
                    }
                    _ => vec![],
                }
            }
            "message_delta" => {
                if let Some(reason) = data
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|v| v.as_str())
                {
                    self.finish_reason = Some(map_stop_reason(reason));
                }
                if let Some(out) = data
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(|v| v.as_u64())
                {
                    self.output_tokens = Some(out);
                }
                vec![]
            }
            "message_stop" => {
                vec![
                    self.chunk(
                        Delta::default(),
                        Some(
                            self.finish_reason
                                .clone()
                                .unwrap_or_else(|| "stop".to_string()),
                        ),
                    ),
                ]
            }
            "error" => {
                vec![]
            }
            _ => vec![],
        }
    }
}

// ---------------------------------------------------------------------------
// 資格情報・送信
// ---------------------------------------------------------------------------

fn credentials_path(dir: &Path) -> PathBuf {
    dir.join(CLAUDE_CREDENTIALS_FILE)
}

fn load_tokens(dir: &Path) -> Result<ClaudeTokens, SourceError> {
    let value = credentials::read_json(&credentials_path(dir))
        .map_err(|e| SourceError::Credentials(e.to_string()))?;
    parse_claude_tokens(&value).map_err(|e| SourceError::Credentials(e.to_string()))
}

async fn refresh_tokens(
    client: &reqwest::Client,
    cfg: &ClaudeOauthConfig,
    dir: &Path,
    tokens: &ClaudeTokens,
) -> Result<ClaudeTokens, SourceError> {
    let resp = client
        .post(&cfg.token_url)
        .json(&json!({
            "grant_type": "refresh_token",
            "refresh_token": tokens.refresh_token,
            "client_id": cfg.client_id,
        }))
        .send()
        .await
        .map_err(|e| SourceError::Network(safe_reqwest_error(&e)))?;
    if !resp.status().is_success() {
        return Err(SourceError::Unauthorized);
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|_| SourceError::Credentials("refresh response was not valid JSON".to_string()))?;
    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            SourceError::Credentials("refresh response missing access_token".to_string())
        })?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| tokens.refresh_token.clone());
    let expires_in = body
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(3600);
    let new_tokens = ClaudeTokens {
        access_token,
        refresh_token,
        expires_at_ms: now_unix_ms() + expires_in * 1000,
    };
    let mut value = credentials::read_json(&credentials_path(dir))
        .map_err(|e| SourceError::Credentials(e.to_string()))?;
    apply_claude_tokens(&mut value, &new_tokens);
    credentials::write_json_atomic(&credentials_path(dir), &value)
        .map_err(|e| SourceError::Credentials(e.to_string()))?;
    tracing::info!(dir = %dir.display(), "llm-proxy: refreshed claude-oauth tokens");
    Ok(new_tokens)
}

/// 1 回分の要求を送る。`dir` はそのアカウントの `CLAUDE_CONFIG_DIR`。
pub async fn send(
    client: &reqwest::Client,
    cfg: &ClaudeOauthConfig,
    dir: &Path,
    req: &crate::openai::ChatCompletionRequest,
    upstream_model: &str,
) -> Result<SendOutcome, SourceError> {
    let mut tokens = load_tokens(dir)?;
    if tokens.expires_at_ms <= now_unix_ms() + REFRESH_BUFFER_MS {
        tokens = refresh_tokens(client, cfg, dir, &tokens).await?;
    }
    let body = to_anthropic_body(req, upstream_model);
    let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));

    let do_request = |access_token: String| {
        let mut builder = client
            .post(&url)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", ANTHROPIC_BETA)
            .header("authorization", format!("Bearer {access_token}"))
            .json(&body);
        if req.stream {
            builder = builder.header("accept", "text/event-stream");
        }
        builder.send()
    };

    let mut resp = do_request(tokens.access_token.clone())
        .await
        .map_err(|e| SourceError::Network(safe_reqwest_error(&e)))?;

    if resp.status().as_u16() == 401 {
        // 事後の 1 回だけ更新して再試行する（ADR-0053 D1）。
        tokens = refresh_tokens(client, cfg, dir, &tokens).await?;
        resp = do_request(tokens.access_token.clone())
            .await
            .map_err(|e| SourceError::Network(safe_reqwest_error(&e)))?;
    }
    let status = resp.status();
    if status.as_u16() == 401 {
        return Err(SourceError::Unauthorized);
    }
    if status.as_u16() == 429 {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        return Err(SourceError::RateLimited { retry_after });
    }
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let summary = extract_error_summary(&text);
        return Err(SourceError::Upstream {
            status: status.as_u16(),
            summary,
        });
    }

    if !req.stream {
        let value: Value = resp
            .json()
            .await
            .map_err(|e| SourceError::Network(safe_reqwest_error(&e)))?;
        return from_anthropic_response(&value, &req.model).map(SendOutcome::NonStream);
    }

    let model = req.model.clone();
    let chunk_stream = build_chunk_stream(resp.bytes_stream(), model);
    Ok(SendOutcome::Stream(chunk_stream))
}

/// `resp.bytes_stream()` を消費して OpenAI chunk の stream に写す（1 poll で複数件出た分は次回に回す）。
fn build_chunk_stream(
    byte_stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    model: String,
) -> futures_util::stream::BoxStream<'static, Result<crate::openai::ChatCompletionChunk, SourceError>>
{
    let mut byte_stream = Box::pin(byte_stream);
    let mut decoder = SseDecoder::new();
    let mut mapper = AnthropicStreamMapper::new(&model);
    let mut pending: std::collections::VecDeque<crate::openai::ChatCompletionChunk> =
        std::collections::VecDeque::new();
    Box::pin(futures_util::stream::poll_fn(move |cx| {
        loop {
            if let Some(chunk) = pending.pop_front() {
                return std::task::Poll::Ready(Some(Ok(chunk)));
            }
            match byte_stream.poll_next_unpin(cx) {
                std::task::Poll::Ready(Some(Ok(bytes))) => {
                    let events = decoder.push(&bytes);
                    for ev in events {
                        let event_name = ev.event.clone().unwrap_or_default();
                        let data: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
                        pending.extend(mapper.feed(&event_name, &data));
                    }
                    continue;
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    return std::task::Poll::Ready(Some(Err(SourceError::Network(
                        safe_reqwest_error(&e),
                    ))));
                }
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }))
}

fn extract_error_summary(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "upstream error".to_string())
        .chars()
        .take(200)
        .collect()
}
