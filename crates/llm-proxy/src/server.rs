//! axum サーバ（ADR-0053 D1）: `GET /v1/models`、`POST /v1/chat/completions`（非 stream / stream）、
//! `POST /v1/embeddings`（501）、`GET /healthz`。loopback のみ想定。Bearer は `[api] token` と同じもの。
//!
//! 判断（選択・cooldown・やり直し）はここに集める。`sources::*` は「送る」だけ、`selection` は
//! 「並べる」だけ、という分業をここで束ねる。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Json, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use futures_util::stream::BoxStream;
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};
use task_core::AccountAdapter;
use task_core::SharedRole;
use task_dispatch::accounts::{AccountBook, AccountCooldownReason, AccountDir, cooldown_for_failure, scan_accounts};
use ulid::Ulid;

use crate::config::{LlmProxyConfig, OpenAiCompatibleConfig};
use crate::log::{self, RequestLogRow};
use crate::naming::{self, ModelRequest, SourceKind, SourceScope};
use crate::openai::ChatCompletionRequest;
use crate::selection::{self, PoolInput, SelectedAccount};
use crate::sources::{SendOutcome, SourceError, claude, codex, relay};

const X_SOURCE: &str = "x-celeris-source";
const X_ACCOUNT: &str = "x-celeris-account";

/// プロキシが動くのに要るもの一式。`Arc` で共有する。
pub struct ProxyState {
    pub config: LlmProxyConfig,
    pub client: reqwest::Client,
    /// `[accounts] claude_dir` 直下の `.celeris-usage.json`。CLI ワーカーの dispatcher と**同じ帳簿**
    /// （cooldown・観測値を共有する。ADR-0053: 「既存のアカウントプールを使う」）。
    pub claude_book: Option<Arc<StdMutex<AccountBook>>>,
    pub codex_book: Option<Arc<StdMutex<AccountBook>>>,
    pub token: Option<String>,
    pub role: SharedRole,
    pub db_path: Option<PathBuf>,
    pub busy_timeout: Duration,
    probe_cache: StdMutex<HashMap<String, (Instant, bool)>>,
    in_use: StdMutex<HashMap<String, usize>>,
}

impl ProxyState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: LlmProxyConfig,
        client: reqwest::Client,
        claude_book: Option<Arc<StdMutex<AccountBook>>>,
        codex_book: Option<Arc<StdMutex<AccountBook>>>,
        token: Option<String>,
        role: SharedRole,
        db_path: Option<PathBuf>,
        busy_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            client,
            claude_book,
            codex_book,
            token,
            role,
            db_path,
            busy_timeout,
            probe_cache: StdMutex::new(HashMap::new()),
            in_use: StdMutex::new(HashMap::new()),
        })
    }

    fn in_use_count(&self, key: &str) -> usize {
        self.in_use.lock().unwrap_or_else(|e| e.into_inner()).get(key).copied().unwrap_or(0)
    }

    fn in_use_start(&self, key: &str) {
        let mut guard = self.in_use.lock().unwrap_or_else(|e| e.into_inner());
        *guard.entry(key.to_string()).or_insert(0) += 1;
    }

    fn in_use_end(&self, key: &str) {
        let mut guard = self.in_use.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = guard.get_mut(key) {
            *v = v.saturating_sub(1);
        }
    }

    /// `GET <base_url>/models` の到達性を probe し、`probe_cache_secs` の間はキャッシュする。
    pub(crate) async fn reachable(&self, cfg: &OpenAiCompatibleConfig) -> bool {
        let ttl = Duration::from_secs(self.config.probe_cache_secs);
        if let Some((at, ok)) = self.probe_cache.lock().unwrap_or_else(|e| e.into_inner()).get(&cfg.id).copied()
            && at.elapsed() < ttl
        {
            return ok;
        }
        let ok = relay::probe(&self.client, cfg).await;
        self.probe_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(cfg.id.clone(), (Instant::now(), ok));
        ok
    }

    fn claude_dirs(&self) -> Vec<AccountDir> {
        self.config
            .sources
            .claude_oauth
            .as_ref()
            .filter(|c| c.enabled)
            .map(|c| scan_accounts(&c.accounts_dir, AccountAdapter::ClaudeCode))
            .unwrap_or_default()
    }

    fn codex_dirs(&self) -> Vec<AccountDir> {
        self.config
            .sources
            .codex_oauth
            .as_ref()
            .filter(|c| c.enabled)
            .map(|c| scan_accounts(&c.accounts_dir, AccountAdapter::Codex))
            .unwrap_or_default()
    }

    fn rank_claude(&self, dirs: &[AccountDir], now: i64) -> Vec<SelectedAccount> {
        let Some(book) = &self.claude_book else { return vec![] };
        let guard = book.lock().unwrap_or_else(|e| e.into_inner());
        let input = PoolInput {
            dirs,
            book: &guard,
            in_use: &|id| self.in_use_count(&format!("claude-oauth:{id}")),
            max_concurrent_per_account: self.config.max_concurrent_per_account,
        };
        selection::rank_pool(SourceKind::Claude, &input, now)
    }

    fn rank_codex(&self, dirs: &[AccountDir], now: i64) -> Vec<SelectedAccount> {
        let Some(book) = &self.codex_book else { return vec![] };
        let guard = book.lock().unwrap_or_else(|e| e.into_inner());
        let input = PoolInput {
            dirs,
            book: &guard,
            in_use: &|id| self.in_use_count(&format!("codex-oauth:{id}")),
            max_concurrent_per_account: self.config.max_concurrent_per_account,
        };
        selection::rank_pool(SourceKind::Gpt, &input, now)
    }

    fn rank_cross(&self, claude_dirs: &[AccountDir], codex_dirs: &[AccountDir], now: i64) -> Vec<SelectedAccount> {
        let claude_guard = self.claude_book.as_ref().map(|b| b.lock().unwrap_or_else(|e| e.into_inner()));
        let codex_guard = self.codex_book.as_ref().map(|b| b.lock().unwrap_or_else(|e| e.into_inner()));
        let claude_in_use = |id: &str| self.in_use_count(&format!("claude-oauth:{id}"));
        let codex_in_use = |id: &str| self.in_use_count(&format!("codex-oauth:{id}"));
        let claude_input = claude_guard.as_ref().map(|g| PoolInput {
            dirs: claude_dirs,
            book: g,
            in_use: &claude_in_use,
            max_concurrent_per_account: self.config.max_concurrent_per_account,
        });
        let codex_input = codex_guard.as_ref().map(|g| PoolInput {
            dirs: codex_dirs,
            book: g,
            in_use: &codex_in_use,
            max_concurrent_per_account: self.config.max_concurrent_per_account,
        });
        selection::rank_across_pools(claude_input.as_ref(), codex_input.as_ref(), now)
    }

    async fn rank_relays(&self, sources: &[OpenAiCompatibleConfig]) -> Vec<OpenAiCompatibleConfig> {
        let mut reachability = HashMap::new();
        for s in sources {
            reachability.insert(s.id.clone(), self.reachable(s).await);
        }
        selection::rank_relays(sources, |id| reachability.get(id).copied().unwrap_or(false))
            .into_iter()
            .cloned()
            .collect()
    }

    /// 429/401 を受けたアカウントに cooldown を付ける（既存の観測と同じ表。ADR-0053 D1）。
    fn record_failure(&self, source: SourceKind, account_id: &str, error: &SourceError, now: i64) {
        let reason = match error {
            SourceError::Unauthorized => AccountCooldownReason::AuthFailed,
            SourceError::RateLimited { .. } => AccountCooldownReason::Throttled,
            _ => return,
        };
        let book = match source {
            SourceKind::Claude => self.claude_book.as_ref(),
            SourceKind::Gpt => self.codex_book.as_ref(),
            SourceKind::Qwen => None,
        };
        let Some(book) = book else { return };
        let mut guard = book.lock().unwrap_or_else(|e| e.into_inner());
        let cooldown = cooldown_for_failure(guard.state(account_id), reason, now, self.config.cooldown_fallback_secs);
        guard.set_cooldown(account_id, cooldown, now);
        if let Err(e) = guard.save() {
            tracing::warn!(error = %e, "llm-proxy: could not persist the account cooldown");
        }
    }
}

// ---------------------------------------------------------------------------
// 候補の組み立て
// ---------------------------------------------------------------------------

enum Attempt {
    Claude(SelectedAccount),
    Codex(SelectedAccount),
    Relay(OpenAiCompatibleConfig),
}

impl Attempt {
    fn source_label(&self) -> String {
        match self {
            Attempt::Claude(_) => "claude-oauth".to_string(),
            Attempt::Codex(_) => "codex-oauth".to_string(),
            Attempt::Relay(cfg) => format!("openai-compatible:{}", cfg.id),
        }
    }
    fn account_label(&self) -> Option<String> {
        match self {
            Attempt::Claude(a) | Attempt::Codex(a) => Some(a.account_id.clone()),
            Attempt::Relay(_) => None,
        }
    }
}

fn tier_model(models: &HashMap<task_core::Tier, String>, tier: task_core::Tier) -> Option<String> {
    models.get(&tier).cloned()
}

impl ProxyState {
    async fn attempts_for(&self, parsed: &ModelRequest, now: i64) -> Vec<(Attempt, String)> {
        match parsed {
            ModelRequest::Explicit { source, model } => match source {
                SourceKind::Claude => self
                    .rank_claude(&self.claude_dirs(), now)
                    .into_iter()
                    .map(|a| (Attempt::Claude(a), model.clone()))
                    .collect(),
                SourceKind::Gpt => self
                    .rank_codex(&self.codex_dirs(), now)
                    .into_iter()
                    .map(|a| (Attempt::Codex(a), model.clone()))
                    .collect(),
                SourceKind::Qwen => self
                    .rank_relays(&self.config.sources.openai_compatible)
                    .await
                    .into_iter()
                    .map(|cfg| (Attempt::Relay(cfg), model.clone()))
                    .collect(),
            },
            ModelRequest::Tiered { scope, tier } => match scope {
                SourceScope::Only(SourceKind::Claude) => {
                    let Some(model) = tier_model(&self.config.models.claude, *tier) else { return vec![] };
                    self.rank_claude(&self.claude_dirs(), now).into_iter().map(|a| (Attempt::Claude(a), model.clone())).collect()
                }
                SourceScope::Only(SourceKind::Gpt) => {
                    let Some(model) = tier_model(&self.config.models.gpt, *tier) else { return vec![] };
                    self.rank_codex(&self.codex_dirs(), now).into_iter().map(|a| (Attempt::Codex(a), model.clone())).collect()
                }
                SourceScope::Only(SourceKind::Qwen) => {
                    let Some(model) = tier_model(&self.config.models.qwen, *tier) else { return vec![] };
                    self.rank_relays(&self.config.sources.openai_compatible)
                        .await
                        .into_iter()
                        .map(|cfg| (Attempt::Relay(cfg), model.clone()))
                        .collect()
                }
                SourceScope::Any => {
                    if self.config.prefer_free {
                        let qwen_model = tier_model(&self.config.models.qwen, *tier);
                        if let Some(model) = qwen_model {
                            let relays = self.rank_relays(&self.config.sources.openai_compatible).await;
                            if !relays.is_empty() {
                                return relays.into_iter().map(|cfg| (Attempt::Relay(cfg), model.clone())).collect();
                            }
                        }
                    }
                    let claude_dirs = self.claude_dirs();
                    let codex_dirs = self.codex_dirs();
                    self.rank_cross(&claude_dirs, &codex_dirs, now)
                        .into_iter()
                        .filter_map(|a| match a.source {
                            SourceKind::Claude => tier_model(&self.config.models.claude, *tier).map(|m| (Attempt::Claude(a), m)),
                            SourceKind::Gpt => tier_model(&self.config.models.gpt, *tier).map(|m| (Attempt::Codex(a), m)),
                            SourceKind::Qwen => None,
                        })
                        .collect()
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// 記録
// ---------------------------------------------------------------------------

struct LogHandle {
    id: String,
    ts: i64,
    source: Option<String>,
    account: Option<String>,
    requested_model: String,
    upstream_model: Option<String>,
    started: Instant,
    db_path: Option<PathBuf>,
    busy_timeout: Duration,
}

impl LogHandle {
    fn write(&self, status: &'static str, usage: (Option<u64>, Option<u64>), error_kind: Option<String>) {
        let Some(db_path) = &self.db_path else { return };
        let row = RequestLogRow {
            id: self.id.clone(),
            ts: self.ts,
            source: self.source.clone(),
            account: self.account.clone(),
            requested_model: self.requested_model.clone(),
            upstream_model: self.upstream_model.clone(),
            prompt_tokens: usage.0,
            completion_tokens: usage.1,
            latency_ms: self.started.elapsed().as_millis() as u64,
            status,
            error_kind,
        };
        match log::open(db_path, self.busy_timeout) {
            Ok(conn) => {
                if let Err(e) = log::insert(&conn, &row) {
                    tracing::warn!(error = %e, "llm-proxy: could not record the request log row");
                }
            }
            Err(e) => tracing::warn!(error = %e, "llm-proxy: could not open the request log db"),
        }
    }
}

fn error_kind(e: &SourceError) -> &'static str {
    match e {
        SourceError::Unauthorized => "unauthorized",
        SourceError::RateLimited { .. } => "rate_limited",
        SourceError::Upstream { .. } => "upstream",
        SourceError::Network(_) => "network",
        SourceError::Credentials(_) => "credentials",
        SourceError::Unavailable(_) => "unavailable",
    }
}

fn usage_from_value(v: &Value) -> (Option<u64>, Option<u64>) {
    let usage = v.get("usage");
    let prompt = usage.and_then(|u| u.get("prompt_tokens")).and_then(|x| x.as_u64());
    let completion = usage.and_then(|u| u.get("completion_tokens")).and_then(|x| x.as_u64());
    (prompt, completion)
}

// ---------------------------------------------------------------------------
// 送信 1 回
// ---------------------------------------------------------------------------

enum AttemptOutcome {
    NonStreamJson(Value),
    Stream(BoxStream<'static, Result<Bytes, SourceError>>),
}

async fn run_attempt(
    state: &ProxyState,
    attempt: &Attempt,
    upstream_model: &str,
    req: &ChatCompletionRequest,
) -> Result<AttemptOutcome, SourceError> {
    match attempt {
        Attempt::Claude(a) => {
            let cfg = state
                .config
                .sources
                .claude_oauth
                .as_ref()
                .ok_or_else(|| SourceError::Unavailable("claude-oauth".to_string()))?;
            match claude::send(&state.client, cfg, &a.dir, req, upstream_model).await? {
                SendOutcome::NonStream(resp) => {
                    let mut value = serde_json::to_value(resp).unwrap_or(Value::Null);
                    if let Some(obj) = value.as_object_mut() {
                        obj.insert("model".to_string(), json!(req.model));
                    }
                    Ok(AttemptOutcome::NonStreamJson(value))
                }
                SendOutcome::Stream(s) => Ok(AttemptOutcome::Stream(encode_chunk_stream(s, req.model.clone()))),
            }
        }
        Attempt::Codex(a) => {
            let cfg = state
                .config
                .sources
                .codex_oauth
                .as_ref()
                .ok_or_else(|| SourceError::Unavailable("codex-oauth".to_string()))?;
            match codex::send(&state.client, cfg, &a.dir, req, upstream_model).await? {
                SendOutcome::NonStream(resp) => {
                    let mut value = serde_json::to_value(resp).unwrap_or(Value::Null);
                    if let Some(obj) = value.as_object_mut() {
                        obj.insert("model".to_string(), json!(req.model));
                    }
                    Ok(AttemptOutcome::NonStreamJson(value))
                }
                SendOutcome::Stream(s) => Ok(AttemptOutcome::Stream(encode_chunk_stream(s, req.model.clone()))),
            }
        }
        Attempt::Relay(cfg) => {
            let mut body = serde_json::to_value(req).unwrap_or(Value::Null);
            if let Some(obj) = body.as_object_mut() {
                obj.insert("model".to_string(), json!(upstream_model));
            }
            let raw = relay::send_raw(&state.client, cfg, &body).await?;
            classify_relay_status(raw.status, raw.retry_after)?;
            if req.stream {
                let requested_model = req.model.clone();
                let byte_stream = raw
                    .body
                    .bytes_stream()
                    .map(move |r| r.map_err(|e| SourceError::Network(crate::neterr::safe_reqwest_error(&e))));
                Ok(AttemptOutcome::Stream(rewrite_relay_stream_model(byte_stream, requested_model)))
            } else {
                let mut value: Value = raw.body.json().await.map_err(|e| SourceError::Network(crate::neterr::safe_reqwest_error(&e)))?;
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("model".to_string(), json!(req.model));
                }
                Ok(AttemptOutcome::NonStreamJson(value))
            }
        }
    }
}

fn classify_relay_status(status: u16, retry_after: Option<u64>) -> Result<(), SourceError> {
    if status == 401 {
        return Err(SourceError::Unauthorized);
    }
    if status == 429 {
        return Err(SourceError::RateLimited { retry_after });
    }
    if !(200..300).contains(&status) {
        return Err(SourceError::Upstream {
            status,
            summary: "relay upstream error".to_string(),
        });
    }
    Ok(())
}

/// Claude/Codex の chunk stream を OpenAI SSE の bytes へ写す。末尾に `data: [DONE]` を付ける。
fn encode_chunk_stream(
    inner: BoxStream<'static, Result<crate::openai::ChatCompletionChunk, SourceError>>,
    requested_model: String,
) -> BoxStream<'static, Result<Bytes, SourceError>> {
    let mut inner = inner;
    let mut done = false;
    Box::pin(futures_util::stream::poll_fn(move |cx| {
        if done {
            return std::task::Poll::Ready(None);
        }
        match inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(Some(Ok(mut chunk))) => {
                chunk.model = requested_model.clone();
                let json = serde_json::to_string(&chunk).unwrap_or_else(|_| "{}".to_string());
                std::task::Poll::Ready(Some(Ok(Bytes::from(crate::sse::encode_data(&json)))))
            }
            std::task::Poll::Ready(Some(Err(e))) => std::task::Poll::Ready(Some(Err(e))),
            std::task::Poll::Ready(None) => {
                done = true;
                std::task::Poll::Ready(Some(Ok(Bytes::from(crate::sse::DONE))))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }))
}

/// relay（openai-compatible）の SSE をそのまま中継しつつ、各イベントの `model` だけ書き換える。
/// パースできない行（`[DONE]` 等）はそのまま通す。
fn rewrite_relay_stream_model(
    inner: impl Stream<Item = Result<Bytes, SourceError>> + Send + 'static,
    requested_model: String,
) -> BoxStream<'static, Result<Bytes, SourceError>> {
    let mut inner = Box::pin(inner);
    let mut decoder = crate::sse::SseDecoder::new();
    let mut pending: std::collections::VecDeque<Bytes> = std::collections::VecDeque::new();
    Box::pin(futures_util::stream::poll_fn(move |cx| loop {
        if let Some(b) = pending.pop_front() {
            return std::task::Poll::Ready(Some(Ok(b)));
        }
        match inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(Some(Ok(bytes))) => {
                for ev in decoder.push(&bytes) {
                    if ev.data.trim() == "[DONE]" {
                        pending.push_back(Bytes::from(crate::sse::DONE));
                        continue;
                    }
                    let rewritten = match serde_json::from_str::<Value>(&ev.data) {
                        Ok(mut v) => {
                            if let Some(obj) = v.as_object_mut() {
                                obj.insert("model".to_string(), json!(requested_model));
                            }
                            serde_json::to_string(&v).unwrap_or(ev.data.clone())
                        }
                        Err(_) => ev.data.clone(),
                    };
                    pending.push_back(Bytes::from(crate::sse::encode_data(&rewritten)));
                }
                continue;
            }
            std::task::Poll::Ready(Some(Err(e))) => return std::task::Poll::Ready(Some(Err(e))),
            std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
            std::task::Poll::Pending => return std::task::Poll::Pending,
        }
    }))
}

/// 送信済みバイトの後は切断するだけ（やり直さない。ADR-0053 D1）。ログはここで確定させる。
fn finalize_stream(
    inner: BoxStream<'static, Result<Bytes, SourceError>>,
    log: LogHandle,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    let mut inner = inner;
    let mut logged = false;
    let mut log = Some(log);
    futures_util::stream::poll_fn(move |cx| match inner.as_mut().poll_next(cx) {
        std::task::Poll::Ready(Some(Ok(b))) => std::task::Poll::Ready(Some(Ok(b))),
        std::task::Poll::Ready(Some(Err(e))) => {
            if !logged {
                logged = true;
                if let Some(log) = log.take() {
                    log.write("error", (None, None), Some(error_kind(&e).to_string()));
                }
            }
            tracing::warn!(kind = error_kind(&e), "llm-proxy: stream terminated after bytes were sent");
            std::task::Poll::Ready(None)
        }
        std::task::Poll::Ready(None) => {
            if !logged {
                logged = true;
                if let Some(log) = log.take() {
                    log.write("ok", (None, None), None);
                }
            }
            std::task::Poll::Ready(None)
        }
        std::task::Poll::Pending => std::task::Poll::Pending,
    })
}

// ---------------------------------------------------------------------------
// ハンドラ
// ---------------------------------------------------------------------------

fn problem(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({"error": {"type": code, "message": detail}}))).into_response()
}

async fn healthz() -> Response {
    (StatusCode::OK, Json(json!({"status": "ok"}))).into_response()
}

async fn embeddings_not_implemented() -> Response {
    problem(StatusCode::NOT_IMPLEMENTED, "not_implemented", "POST /v1/embeddings is not implemented by this proxy")
}

async fn list_models(State(state): State<Arc<ProxyState>>) -> Response {
    let created = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut data = Vec::new();
    for tier in ["frontier", "standard", "cheap"] {
        data.push(crate::openai::ModelInfo {
            id: format!("celeris/{tier}"),
            object: "model".to_string(),
            created,
            owned_by: "celeris".to_string(),
        });
    }
    if state.config.sources.claude_oauth.as_ref().is_some_and(|c| c.enabled) {
        for tier in ["frontier", "standard", "cheap"] {
            data.push(crate::openai::ModelInfo {
                id: format!("claude/{tier}"),
                object: "model".to_string(),
                created,
                owned_by: "claude-oauth".to_string(),
            });
        }
    }
    if state.config.sources.codex_oauth.as_ref().is_some_and(|c| c.enabled) {
        for tier in ["frontier", "standard", "cheap"] {
            data.push(crate::openai::ModelInfo {
                id: format!("gpt/{tier}"),
                object: "model".to_string(),
                created,
                owned_by: "codex-oauth".to_string(),
            });
        }
    }
    if state.config.sources.openai_compatible.iter().any(|c| c.enabled) {
        for tier in ["frontier", "standard", "cheap"] {
            data.push(crate::openai::ModelInfo {
                id: format!("qwen/{tier}"),
                object: "model".to_string(),
                created,
                owned_by: "openai-compatible".to_string(),
            });
        }
    }
    (StatusCode::OK, Json(crate::openai::ModelsResponse { object: "list".to_string(), data })).into_response()
}

fn attach_source_headers(headers: &mut HeaderMap, source: &str, account: Option<&str>) {
    if let Ok(v) = HeaderValue::from_str(source) {
        headers.insert(header::HeaderName::from_static(X_SOURCE), v);
    }
    if let Some(account) = account
        && let Ok(v) = HeaderValue::from_str(account)
    {
        headers.insert(header::HeaderName::from_static(X_ACCOUNT), v);
    }
}

fn error_response(e: &SourceError) -> Response {
    match e {
        SourceError::Unauthorized => problem(StatusCode::BAD_GATEWAY, "unauthorized", "upstream rejected the credentials"),
        SourceError::RateLimited { retry_after } => {
            let mut resp = problem(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "upstream rate-limited the request");
            if let Some(secs) = retry_after
                && let Ok(v) = HeaderValue::from_str(&secs.to_string())
            {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
            resp
        }
        SourceError::Upstream { status, summary } => {
            let code = StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY);
            let code = if code.is_client_error() || code.is_server_error() { code } else { StatusCode::BAD_GATEWAY };
            problem(code, "upstream_error", summary)
        }
        SourceError::Network(_) => problem(StatusCode::SERVICE_UNAVAILABLE, "network_error", "could not reach the upstream"),
        SourceError::Credentials(msg) => problem(StatusCode::INTERNAL_SERVER_ERROR, "credentials_error", msg),
        SourceError::Unavailable(id) => problem(StatusCode::SERVICE_UNAVAILABLE, "source_unavailable", &format!("source unavailable: {id}")),
    }
}

async fn chat_completions(State(state): State<Arc<ProxyState>>, Json(req): Json<ChatCompletionRequest>) -> Response {
    let started = Instant::now();
    let request_id = Ulid::new().to_string();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();

    let parsed = match naming::parse_model(&req.model) {
        Ok(p) => p,
        Err(e) => {
            LogHandle {
                id: request_id,
                ts: now,
                source: None,
                account: None,
                requested_model: req.model.clone(),
                upstream_model: None,
                started,
                db_path: state.db_path.clone(),
                busy_timeout: state.busy_timeout,
            }
            .write("error", (None, None), Some("invalid_model".to_string()));
            return problem(StatusCode::UNPROCESSABLE_ENTITY, "invalid_model", &e.to_string());
        }
    };

    let attempts = state.attempts_for(&parsed, now).await;
    if attempts.is_empty() {
        LogHandle {
            id: request_id,
            ts: now,
            source: None,
            account: None,
            requested_model: req.model.clone(),
            upstream_model: None,
            started,
            db_path: state.db_path.clone(),
            busy_timeout: state.busy_timeout,
        }
        .write("unavailable", (None, None), Some("no_source_available".to_string()));
        return problem(StatusCode::SERVICE_UNAVAILABLE, "no_source_available", "no reachable llm source is configured for this model");
    }

    let mut last_error = None;
    for (attempt, upstream_model) in &attempts {
        let source_label = attempt.source_label();
        let account_label = attempt.account_label();
        let in_use_key = account_label.as_ref().map(|a| format!("{source_label}:{a}"));
        if let Some(k) = &in_use_key {
            state.in_use_start(k);
        }
        let result = run_attempt(&state, attempt, upstream_model, &req).await;
        if let Some(k) = &in_use_key {
            state.in_use_end(k);
        }
        match result {
            Ok(AttemptOutcome::NonStreamJson(value)) => {
                let usage = usage_from_value(&value);
                LogHandle {
                    id: request_id,
                    ts: now,
                    source: Some(source_label.clone()),
                    account: account_label.clone(),
                    requested_model: req.model.clone(),
                    upstream_model: Some(upstream_model.clone()),
                    started,
                    db_path: state.db_path.clone(),
                    busy_timeout: state.busy_timeout,
                }
                .write("ok", usage, None);
                let mut resp = (StatusCode::OK, Json(value)).into_response();
                attach_source_headers(resp.headers_mut(), &source_label, account_label.as_deref());
                return resp;
            }
            Ok(AttemptOutcome::Stream(stream)) => {
                let log = LogHandle {
                    id: request_id,
                    ts: now,
                    source: Some(source_label.clone()),
                    account: account_label.clone(),
                    requested_model: req.model.clone(),
                    upstream_model: Some(upstream_model.clone()),
                    started,
                    db_path: state.db_path.clone(),
                    busy_timeout: state.busy_timeout,
                };
                let body = Body::from_stream(finalize_stream(stream, log));
                let mut resp = Response::new(body);
                resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
                resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                attach_source_headers(resp.headers_mut(), &source_label, account_label.as_deref());
                return resp;
            }
            Err(e) => {
                if let (Some(account), true) = (
                    &account_label,
                    matches!(e, SourceError::Unauthorized | SourceError::RateLimited { .. }),
                ) {
                    let source_kind = match attempt {
                        Attempt::Claude(_) => SourceKind::Claude,
                        Attempt::Codex(_) => SourceKind::Gpt,
                        Attempt::Relay(_) => SourceKind::Qwen,
                    };
                    state.record_failure(source_kind, account, &e, now);
                }
                tracing::warn!(source = %source_label, account = ?account_label, kind = error_kind(&e), "llm-proxy: candidate failed before any bytes were sent; trying the next one");
                last_error = Some((source_label, account_label, upstream_model.clone(), e));
            }
        }
    }

    let (source_label, account_label, upstream_model, e) = last_error.unwrap_or((
        "none".to_string(),
        None,
        String::new(),
        SourceError::Unavailable("no candidate succeeded".to_string()),
    ));
    LogHandle {
        id: request_id,
        ts: now,
        source: Some(source_label),
        account: account_label,
        requested_model: req.model.clone(),
        upstream_model: Some(upstream_model),
        started,
        db_path: state.db_path.clone(),
        busy_timeout: state.busy_timeout,
    }
    .write("error", (None, None), Some(error_kind(&e).to_string()));
    error_response(&e)
}

// ---------------------------------------------------------------------------
// ミドルウェア・ルータ
// ---------------------------------------------------------------------------

async fn guard(State(state): State<Arc<ProxyState>>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/healthz" {
        return next.run(req).await;
    }
    if !state.role.accepts_admin() {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "standby", "this instance is not active");
    }
    if let Some(token) = &state.token {
        let digest = crate::auth::token_digest(token);
        if !crate::auth::check_bearer(req.headers(), &digest) {
            return problem(StatusCode::UNAUTHORIZED, "unauthorized", "missing or invalid bearer token");
        }
    }
    next.run(req).await
}

pub fn router(state: Arc<ProxyState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/embeddings", post(embeddings_not_implemented))
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// `listen` に bind して動かす（loopback のみを想定。呼び出し側が bind アドレスを検証する）。
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<ProxyState>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let app = router(state);
    axum::serve(listener, app).with_graceful_shutdown(shutdown).await
}
