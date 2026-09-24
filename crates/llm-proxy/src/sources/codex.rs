//! `codex-oauth`（ADR-0053 D1-2、Phase 65b で要求形を修正）: Codex CLI の `auth.json` を使って
//! ChatGPT の Codex backend（Responses API、`.../backend-api/codex/responses`）を叩き、OpenAI 互換の
//! 要求/応答へ双方向に写す。
//!
//! `auth.json` に `expires_at` 相当が無いため（ADR-0053 のフィクスチャどおり）、更新は**事後（401 を
//! 受けてから）だけ**行う（`docs/llm-source.md` に明記。claude-oauth の事前更新とはこの点だけ違う）。
//!
//! **Phase 65b 追記**: 本番で `codex-oauth` 経由の要求が全て `400` で落ちていた（`docs/adr/0053-llm-source-proxy.md`
//! の Phase 65b 追記、`docs/llm-source.md` §2 参照）。ChatGPT の Codex backend は Codex CLI
//! （`codex-rs`）が送る形以外を拒否することがあるため、ここでは Codex CLI と同じ形で送る:
//! `store: false`、`stream: true`（**非 stream の応答を受け付けないので、常に stream で要求し、
//! クライアントが非 stream を求めたときはこの層で SSE を集約する**）、`instructions` は常に入れる
//! （system メッセージが無ければ既定の一文）、`temperature`/`max_output_tokens` は既定では送らない
//! （opt-in）、`User-Agent: codex_cli_rs/<version>` を付ける。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde_json::{Value, json};
use ulid::Ulid;

use crate::config::CodexOauthConfig;
use crate::credentials::{
    self, CODEX_CREDENTIALS_FILE, CodexTokens, apply_codex_tokens, parse_codex_tokens,
};
use crate::neterr::safe_reqwest_error;
use crate::openai::{
    ChatCompletionResponse, ChatMessage, Delta, FunctionCall, FunctionCallDelta, MessageContent,
    ToolCall, ToolCallDelta, Usage,
};
use crate::sse::SseDecoder;

use super::{SendOutcome, SourceError};

const ORIGINATOR: &str = "codex_cli_rs";

pub fn account_dir(cfg: &CodexOauthConfig, account_id: &str) -> PathBuf {
    cfg.accounts_dir.join(account_id)
}

fn credentials_path(dir: &Path) -> PathBuf {
    dir.join(CODEX_CREDENTIALS_FILE)
}

fn load_tokens(dir: &Path) -> Result<CodexTokens, SourceError> {
    let value = credentials::read_json(&credentials_path(dir))
        .map_err(|e| SourceError::Credentials(e.to_string()))?;
    parse_codex_tokens(&value).map_err(|e| SourceError::Credentials(e.to_string()))
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

async fn refresh_tokens(
    client: &reqwest::Client,
    cfg: &CodexOauthConfig,
    dir: &Path,
    tokens: &CodexTokens,
) -> Result<CodexTokens, SourceError> {
    let resp = client
        .post(&cfg.token_url)
        .json(&json!({
            "grant_type": "refresh_token",
            "refresh_token": tokens.refresh_token,
            "client_id": cfg.client_id,
            "scope": "openid profile email",
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
    let id_token = body
        .get("id_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| tokens.id_token.clone());
    let new_tokens = CodexTokens {
        access_token,
        refresh_token,
        id_token,
        account_id: tokens.account_id.clone(),
    };
    let mut value = credentials::read_json(&credentials_path(dir))
        .map_err(|e| SourceError::Credentials(e.to_string()))?;
    apply_codex_tokens(&mut value, &new_tokens, &now_rfc3339());
    credentials::write_json_atomic(&credentials_path(dir), &value)
        .map_err(|e| SourceError::Credentials(e.to_string()))?;
    tracing::info!(dir = %dir.display(), "llm-proxy: refreshed codex-oauth tokens");
    Ok(new_tokens)
}

// ---------------------------------------------------------------------------
// 要求の写し（OpenAI → Codex Responses）
// ---------------------------------------------------------------------------

fn message_to_input_items(msg: &ChatMessage) -> Vec<Value> {
    match msg.role.as_str() {
        "system" => vec![],
        "tool" => vec![json!({
            "type": "function_call_output",
            "call_id": msg.tool_call_id.clone().unwrap_or_default(),
            "output": msg.content.as_ref().map(MessageContent::as_text).unwrap_or_default(),
        })],
        "assistant" => {
            let mut items = Vec::new();
            if let Some(content) = &msg.content {
                let text = content.as_text();
                if !text.is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}]
                    }));
                }
            }
            for call in msg.tool_calls.iter().flatten() {
                items.push(json!({
                    "type": "function_call",
                    "call_id": call.id,
                    "name": call.function.name,
                    "arguments": call.function.arguments,
                }));
            }
            items
        }
        _ => vec![json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": msg.content.as_ref().map(MessageContent::as_text).unwrap_or_default()}]
        })],
    }
}

/// クライアントに system メッセージが無いときに送る既定の `instructions`（Codex CLI は常に
/// 何らかの instructions を送るため、空にしない。ADR-0053 Phase 65b 追記）。
const DEFAULT_INSTRUCTIONS: &str =
    "You are a helpful assistant, accessed through the celeris LLM proxy.";

pub fn to_responses_body(
    req: &crate::openai::ChatCompletionRequest,
    model: &str,
    cfg: &CodexOauthConfig,
) -> Value {
    let instructions: Vec<String> = req
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
    let input: Vec<Value> = req
        .messages
        .iter()
        .flat_map(message_to_input_items)
        .collect();
    let instructions_text = if instructions.is_empty() {
        DEFAULT_INSTRUCTIONS.to_string()
    } else {
        instructions.join("\n\n")
    };

    let mut body = json!({
        "model": model,
        "input": input,
        // Codex backend は非 stream の応答を受け付けない（Codex CLI は常に stream:true / store:false
        // で送る）。クライアントが非 stream を求めたときは `send()` が SSE を集約して 1 つの応答に
        // 組み立てる（ADR-0053 Phase 65b 追記）。
        "stream": true,
        "store": false,
        "instructions": instructions_text,
        "parallel_tool_calls": true,
    });
    let obj = body
        .as_object_mut()
        .unwrap_or_else(|| unreachable!("body is always an object"));
    // Codex CLI は temperature / max_output_tokens を送らない（送ると拒否されることがある）。
    // 既定では省略し、`send_sampling_params = true` で明示的に opt-in したときだけ転送する。
    if cfg.send_sampling_params {
        if let Some(max) = req.max_tokens {
            obj.insert("max_output_tokens".to_string(), json!(max));
        }
        if let Some(t) = req.temperature {
            obj.insert("temperature".to_string(), json!(t));
        }
    }
    if let Some(effort) = &cfg.reasoning_effort {
        obj.insert(
            "reasoning".to_string(),
            json!({"effort": effort, "summary": "auto"}),
        );
        obj.insert(
            "include".to_string(),
            json!(["reasoning.encrypted_content"]),
        );
    }
    if let Some(tools) = &req.tools {
        let mapped: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.function.name,
                    "description": t.function.description.clone().unwrap_or_default(),
                    "parameters": t.function.parameters,
                })
            })
            .collect();
        obj.insert("tools".to_string(), json!(mapped));
    }
    if let Some(choice) = &req.tool_choice {
        obj.insert(
            "tool_choice".to_string(),
            match choice {
                crate::openai::ToolChoice::Mode(m) => json!(m),
                crate::openai::ToolChoice::Named { function, .. } => {
                    json!({"type": "function", "name": function.name})
                }
            },
        );
    } else if req.tools.as_ref().is_some_and(|t| !t.is_empty()) {
        // Codex CLI は tools がある要求に `tool_choice: "auto"` を付ける。
        obj.insert("tool_choice".to_string(), json!("auto"));
    }
    body
}

// ---------------------------------------------------------------------------
// 応答の写し（Codex Responses → OpenAI、非 stream）
// ---------------------------------------------------------------------------

pub fn from_responses_body(
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
        .unwrap_or_else(|| format!("chatcmpl-{}", Ulid::new()));
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for item in body
        .get("output")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(|v| v.as_str()) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    if part.get("type").and_then(|v| v.as_str()) == Some("output_text") {
                        text.push_str(
                            part.get("text")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default(),
                        );
                    }
                }
            }
            Some("function_call") => {
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}")
                    .to_string();
                tool_calls.push(ToolCall {
                    id: call_id,
                    kind: "function".to_string(),
                    function: FunctionCall { name, arguments },
                });
            }
            _ => {}
        }
    }
    let has_tool_calls = !tool_calls.is_empty();
    let status = body
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("completed");
    let incomplete_reason = body
        .get("incomplete_details")
        .and_then(|d| d.get("reason"))
        .and_then(|v| v.as_str());
    let finish_reason = match (status, incomplete_reason) {
        (_, Some("max_output_tokens")) => "length",
        _ if has_tool_calls => "tool_calls",
        _ => "stop",
    }
    .to_string();
    let usage = body.get("usage").map(|u| {
        let prompt = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let completion = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
        }
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
// stream（Responses SSE → OpenAI chunk）
// ---------------------------------------------------------------------------

pub struct ResponsesStreamMapper {
    id: String,
    model: String,
    created: i64,
    next_tool_index: u32,
    tool_index_by_item: HashMap<String, u32>,
    saw_tool_call: bool,
    usage: Option<Usage>,
}

impl ResponsesStreamMapper {
    pub fn new(model: &str) -> Self {
        Self {
            id: format!("chatcmpl-{}", Ulid::new()),
            model: model.to_string(),
            created: time::OffsetDateTime::now_utc().unix_timestamp(),
            next_tool_index: 0,
            tool_index_by_item: HashMap::new(),
            saw_tool_call: false,
            usage: None,
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
        {
            c.usage = self.usage;
        }
        c
    }

    fn item_key(data: &Value) -> String {
        data.get("item_id")
            .and_then(|v| v.as_str())
            .or_else(|| {
                data.get("item")
                    .and_then(|i| i.get("id"))
                    .and_then(|v| v.as_str())
            })
            .map(str::to_string)
            .unwrap_or_else(|| {
                data.get("output_index")
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            })
    }

    pub fn feed(&mut self, event: &str, data: &Value) -> Vec<crate::openai::ChatCompletionChunk> {
        match event {
            "response.created" => {
                if let Some(id) = data
                    .get("response")
                    .and_then(|r| r.get("id"))
                    .and_then(|v| v.as_str())
                {
                    self.id = id.to_string();
                }
                vec![self.chunk(
                    Delta {
                        role: Some("assistant".to_string()),
                        ..Delta::default()
                    },
                    None,
                )]
            }
            "response.output_text.delta" => {
                let text = data
                    .get("delta")
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
            "response.output_item.added" => {
                let item = data.get("item").cloned().unwrap_or_default();
                if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                    self.saw_tool_call = true;
                    let key = Self::item_key(data);
                    let idx = self.next_tool_index;
                    self.next_tool_index += 1;
                    self.tool_index_by_item.insert(key, idx);
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    vec![self.chunk(
                        Delta {
                            tool_calls: Some(vec![ToolCallDelta {
                                index: idx,
                                id: Some(call_id),
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
                } else {
                    vec![]
                }
            }
            "response.function_call_arguments.delta" => {
                let key = Self::item_key(data);
                let Some(idx) = self.tool_index_by_item.get(&key).copied() else {
                    return vec![];
                };
                let partial = data
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                vec![self.chunk(
                    Delta {
                        tool_calls: Some(vec![ToolCallDelta {
                            index: idx,
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
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                let response = data.get("response").cloned().unwrap_or(data.clone());
                self.usage = response.get("usage").map(|u| {
                    let prompt = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    let completion = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    Usage {
                        prompt_tokens: prompt,
                        completion_tokens: completion,
                        total_tokens: prompt + completion,
                    }
                });
                let incomplete_reason = response
                    .get("incomplete_details")
                    .and_then(|d| d.get("reason"))
                    .and_then(|v| v.as_str());
                let finish_reason = if incomplete_reason == Some("max_output_tokens") {
                    "length"
                } else if self.saw_tool_call {
                    "tool_calls"
                } else {
                    "stop"
                };
                vec![self.chunk(Delta::default(), Some(finish_reason.to_string()))]
            }
            _ => vec![],
        }
    }
}

/// 1 回分の要求を送る。`dir` はそのアカウントの `CODEX_HOME`。
pub async fn send(
    client: &reqwest::Client,
    cfg: &CodexOauthConfig,
    dir: &Path,
    req: &crate::openai::ChatCompletionRequest,
    upstream_model: &str,
) -> Result<SendOutcome, SourceError> {
    let mut tokens = load_tokens(dir)?;
    let body = to_responses_body(req, upstream_model, cfg);

    let do_request = |access_token: String, account_id: String| {
        client
            .post(&cfg.responses_url)
            .header("authorization", format!("Bearer {access_token}"))
            .header("chatgpt-account-id", account_id)
            .header("openai-beta", "responses=experimental")
            .header("originator", ORIGINATOR)
            .header("session_id", Ulid::new().to_string())
            .header("user-agent", cfg.user_agent.clone())
            // 上流はいつも stream:true で要求する（Codex backend は非 stream を受け付けない）。
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
    };

    let mut resp = do_request(tokens.access_token.clone(), tokens.account_id.clone())
        .await
        .map_err(|e| SourceError::Network(safe_reqwest_error(&e)))?;

    if resp.status().as_u16() == 401 {
        tokens = refresh_tokens(client, cfg, dir, &tokens).await?;
        resp = do_request(tokens.access_token.clone(), tokens.account_id.clone())
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
        // ADR-0053 Phase 65b 追記: 上流の本文（`error.message` / 上位の `detail`・`message`）を
        // 秘密の値を含まない範囲で要約し、WARN ログと応答の両方に出す（トークン・ヘッダは出さない）。
        let text = resp.text().await.unwrap_or_default();
        let summary = extract_error_summary(&text);
        tracing::warn!(status = status.as_u16(), summary = %summary, "llm-proxy: codex-oauth upstream returned a non-success status");
        return Err(SourceError::Upstream {
            status: status.as_u16(),
            summary,
        });
    }

    // 上流はいつも SSE で返す。クライアントが非 stream を求めていたときはここで集約する
    // （ADR-0053 Phase 65b 追記）。
    if !req.stream {
        let requested_model = req.model.clone();
        return aggregate_stream(resp.bytes_stream(), &requested_model)
            .await
            .map(SendOutcome::NonStream);
    }

    let model = req.model.clone();
    let chunk_stream = build_chunk_stream(resp.bytes_stream(), model);
    Ok(SendOutcome::Stream(chunk_stream))
}

/// クライアントが非 stream を求めたときに、上流の SSE（常に stream:true で要求している）を集約して
/// 1 つの `ChatCompletionResponse` を組み立てる（ADR-0053 Phase 65b 追記）。
/// `response.output_text.delta` でテキストを、`response.output_item.done` の `function_call` で
/// tool call を、`response.completed`（`incomplete`/`failed` も同様に扱う）で usage / finish_reason
/// を集める。
async fn aggregate_stream(
    byte_stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    requested_model: &str,
) -> Result<ChatCompletionResponse, SourceError> {
    let mut byte_stream = Box::pin(byte_stream);
    let mut decoder = SseDecoder::new();
    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut usage: Option<Usage> = None;
    let mut finish_reason = "stop".to_string();
    let mut response_id: Option<String> = None;

    while let Some(item) = byte_stream.next().await {
        let bytes = item.map_err(|e| SourceError::Network(safe_reqwest_error(&e)))?;
        for ev in decoder.push(&bytes) {
            let event_name = ev.event.clone().unwrap_or_default();
            let data: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
            match event_name.as_str() {
                "response.created" => {
                    if let Some(id) = data
                        .get("response")
                        .and_then(|r| r.get("id"))
                        .and_then(|v| v.as_str())
                    {
                        response_id = Some(id.to_string());
                    }
                }
                "response.output_text.delta" => {
                    text.push_str(
                        data.get("delta")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default(),
                    );
                }
                "response.output_item.done" => {
                    let item = data.get("item").cloned().unwrap_or_default();
                    if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let arguments = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}")
                            .to_string();
                        tool_calls.push(ToolCall {
                            id: call_id,
                            kind: "function".to_string(),
                            function: FunctionCall { name, arguments },
                        });
                    }
                }
                "response.completed" | "response.incomplete" | "response.failed" => {
                    let response = data
                        .get("response")
                        .cloned()
                        .unwrap_or_else(|| data.clone());
                    if let Some(id) = response.get("id").and_then(|v| v.as_str()) {
                        response_id = Some(id.to_string());
                    }
                    usage = response.get("usage").map(|u| {
                        let prompt = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        let completion =
                            u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        Usage {
                            prompt_tokens: prompt,
                            completion_tokens: completion,
                            total_tokens: prompt + completion,
                        }
                    });
                    let incomplete_reason = response
                        .get("incomplete_details")
                        .and_then(|d| d.get("reason"))
                        .and_then(|v| v.as_str());
                    if incomplete_reason == Some("max_output_tokens") {
                        finish_reason = "length".to_string();
                    } else if !tool_calls.is_empty() {
                        finish_reason = "tool_calls".to_string();
                    }
                    if event_name == "response.failed" {
                        let msg = response
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("upstream error");
                        return Err(SourceError::Upstream {
                            status: 200,
                            summary: msg.chars().take(300).collect(),
                        });
                    }
                }
                "error" => {
                    let msg = data
                        .get("message")
                        .or_else(|| data.get("error").and_then(|e| e.get("message")))
                        .and_then(|v| v.as_str())
                        .unwrap_or("upstream error");
                    return Err(SourceError::Upstream {
                        status: 200,
                        summary: msg.chars().take(300).collect(),
                    });
                }
                _ => {}
            }
        }
    }

    let id = response_id.unwrap_or_else(|| format!("chatcmpl-{}", Ulid::new()));
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

fn build_chunk_stream(
    byte_stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    model: String,
) -> futures_util::stream::BoxStream<'static, Result<crate::openai::ChatCompletionChunk, SourceError>>
{
    let mut byte_stream = Box::pin(byte_stream);
    let mut decoder = SseDecoder::new();
    let mut mapper = ResponsesStreamMapper::new(&model);
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

/// ADR-0053 Phase 65b 追記: `error.message`（OpenAI 互換の形）と、ChatGPT backend がよく返す
/// 上位の `detail` / `message` フィールドの両方を見る（どちらもあれば両方を残す）。秘密の値は
/// 含まない前提（本文はそもそも呼び出し側の資格情報を含まない）が、念のため 300 文字で切る。
fn extract_error_summary(body: &str) -> String {
    let value = serde_json::from_str::<Value>(body).ok();
    let mut parts: Vec<String> = Vec::new();
    if let Some(v) = &value {
        if let Some(msg) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            parts.push(msg.to_string());
        }
        if let Some(detail) = v.get("detail").and_then(|d| d.as_str()) {
            parts.push(detail.to_string());
        }
        if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
            parts.push(msg.to_string());
        }
    }
    let summary = if !parts.is_empty() {
        parts.join("; ")
    } else if !body.trim().is_empty() {
        body.trim().to_string()
    } else {
        "upstream error".to_string()
    };
    summary.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::{ChatCompletionRequest, ChatMessage};

    fn test_config() -> CodexOauthConfig {
        CodexOauthConfig {
            accounts_dir: std::path::PathBuf::new(),
            responses_url: "http://localhost/responses".to_string(),
            token_url: "http://localhost/token".to_string(),
            client_id: "test-client".to_string(),
            enabled: true,
            user_agent: "codex_cli_rs/0.45.0".to_string(),
            send_sampling_params: false,
            reasoning_effort: None,
        }
    }

    fn user_message(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: Some(MessageContent::Text(text.to_string())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn base_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-5".to_string(),
            messages: vec![user_message("hi")],
            temperature: Some(0.7),
            top_p: None,
            max_tokens: Some(512),
            stop: None,
            stream: false,
            tools: None,
            tool_choice: None,
        }
    }

    /// Phase 65b 受け入れ条件 (a): 既定では `store:false` / `stream:true` / `instructions` あり /
    /// `temperature`・`max_output_tokens` は無し（Codex CLI が送る形。ADR-0053 Phase 65b 追記）。
    #[test]
    fn to_responses_body_matches_the_codex_cli_shape_by_default() {
        let req = base_request();
        let cfg = test_config();
        let body = to_responses_body(&req, "gpt-5", &cfg);
        assert_eq!(body["store"], false);
        assert_eq!(
            body["stream"], true,
            "クライアントの要求(stream:false)に関わらず上流へはいつも stream:true"
        );
        assert!(
            body["instructions"].as_str().is_some_and(|s| !s.is_empty()),
            "system が無くても既定の instructions が入る: {body}"
        );
        assert!(
            body.get("temperature").is_none(),
            "既定では temperature を送らない: {body}"
        );
        assert!(
            body.get("max_output_tokens").is_none(),
            "既定では max_output_tokens を送らない: {body}"
        );
    }

    #[test]
    fn to_responses_body_uses_the_client_system_message_as_instructions() {
        let mut req = base_request();
        req.messages.insert(
            0,
            ChatMessage {
                role: "system".to_string(),
                content: Some(MessageContent::Text("You are terse.".to_string())),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        );
        let body = to_responses_body(&req, "gpt-5", &test_config());
        assert_eq!(body["instructions"], "You are terse.");
    }

    #[test]
    fn to_responses_body_sends_sampling_params_only_when_opted_in() {
        let req = base_request();
        let mut cfg = test_config();
        cfg.send_sampling_params = true;
        let body = to_responses_body(&req, "gpt-5", &cfg);
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_output_tokens"], 512);
    }

    #[test]
    fn to_responses_body_adds_reasoning_and_include_only_when_configured() {
        let req = base_request();
        let mut cfg = test_config();
        cfg.reasoning_effort = Some("medium".to_string());
        let body = to_responses_body(&req, "gpt-5", &cfg);
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["include"][0], "reasoning.encrypted_content");

        let without = to_responses_body(&req, "gpt-5", &test_config());
        assert!(without.get("reasoning").is_none());
        assert!(without.get("include").is_none());
    }

    /// Phase 65b 受け入れ条件 (c): ChatGPT backend がよく返す `{"detail": ...}` を要約に反映する。
    #[test]
    fn extract_error_summary_prefers_detail_and_message_fields() {
        assert_eq!(
            extract_error_summary(r#"{"detail":"Store must be set to false"}"#),
            "Store must be set to false"
        );
        assert_eq!(
            extract_error_summary(r#"{"error":{"message":"bad request"}}"#),
            "bad request"
        );
        assert_eq!(
            extract_error_summary(r#"{"message":"plain message field"}"#),
            "plain message field"
        );
        assert_eq!(extract_error_summary(""), "upstream error");
    }

    #[test]
    fn extract_error_summary_is_truncated_to_300_chars() {
        let long = "x".repeat(500);
        let body = format!(r#"{{"detail":"{long}"}}"#);
        let summary = extract_error_summary(&body);
        assert_eq!(summary.chars().count(), 300);
    }
}
