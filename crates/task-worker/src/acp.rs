//! 汎用 ACP（Agent Client Protocol）ワーカーアダプタ（DESIGN §5.4 提案 P-63, ADR-0026）。
//!
//! ACP はここでは「運搬・観測・生存管理」だけを担う（ADR-0026 D1）。エージェントの最終回答は成功判定に
//! 使わない。終端は `claude-code`/`codex` と同じく `artifacts/result.json`（ADR-0006 D3）から合成し、
//! 成果物ディレクトリの `delegate.json` も共有ヘルパで転送する。プロンプト組み立ても `claude_code::build_prompt`
//! をそのまま再利用する（kind 別の文面をアダプタごとに複製しない）。
//!
//! # 実装メモ: 公式 SDK ではなく自前の JSON-RPC クライアント
//!
//! `agent-client-protocol` crate（2.1.0）は試したが採用しなかった。理由:
//! - プロセス生成に `async-process` / `blocking`（別スレッドプール経由の同期呼び出し）を使っており、
//!   本クレートが一貫して使っている `tokio::process::Command`（`process_group(0)` / `kill_on_drop` /
//!   `subprocess.rs` の `send_signal_to_group` によるプロセスグループ単位のシグナル送信）と噛み合わない。
//!   taskd 側でプロセスの生死とシグナルを直接握っておく必要がある（ADR-0003 D4 の生存監視）ため、
//!   SDK 側にプロセス起動を委ねる `AcpAgent::from_str` 系の入口は使いにくい。
//! - SDK の `Client::builder()...connect_with(...)` は「コネクション全体を 1 つの async ブロックに
//!   閉じ込める」設計で、`claude_code.rs`/`codex.rs` が使っている「1 行読むたびに wall-clock / idle
//!   タイムアウトを判定してから次を読む」ループ（`subprocess::read_line_limited` + `tokio::time::timeout`）
//!   や、受信した生の行をそのまま `runs/<run_id>/stdout.jsonl` にミラーする既存の作法にはめ込みにくい。
//! - ワーカー → taskd 方向は結局「1 行 1 JSON」を読むだけなので、`serde_json::Value` を手で組み立てる
//!   方が本クレートの他のアダプタ（`claude_code.rs`/`codex.rs` も CLI 独自の JSON Lines を手でパースして
//!   いる）と一貫する。
//!
//! そのため JSON-RPC 2.0 のメッセージは `serde_json::Value` で直接組み立て・パースする。ACP のフィールド名
//! （`protocolVersion`, `sessionId`, `optionId` 等）は `agent-client-protocol-schema` クレートのソースと
//! ADR-0026 の実機確認（opencode 1.18.31）を突き合わせて決めた。

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nix::sys::signal::Signal;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
use crate::claude_code::build_prompt;
use crate::delegate_file::{clear_delegate_file, forward_delegate_file};
use crate::protocol::{Evidence, ProviderFailure, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, kill_now, reap_after_terminal, read_line_limited, read_tail, send_signal_to_group,
    write_result_json,
};

/// `session/request_permission` への即答（ADR-0026 D4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpPermission {
    Allow,
    Deny,
}

/// `[adapters.acp]` / `[[providers]]` 行の上書き分（taskd.toml, ADR-0026 D2）。
#[derive(Debug, Clone)]
pub struct AcpConfig {
    /// 起動する ACP エージェントの実行ファイル。既定 `"opencode"`。
    pub command: String,
    /// コマンドへの引数。既定 `["acp"]`。
    pub args: Vec<String>,
    /// 追加の環境変数。
    pub env: Vec<(String, String)>,
    /// `session/request_permission` への即答（既定 `Allow`）。
    pub permission: AcpPermission,
    /// `session/set_config_option` で設定するモデル（空/`None` なら送らない）。
    pub model: Option<String>,
    /// `session/set_config_option` の `configId`（`session/new` の `configOptions[].id`）。既定 `"model"`。
    pub model_option_id: String,
    /// `initialize` の応答を待つ上限（初回はエージェント側のプロバイダ取得で数分かかりうる。既定 300 秒）。
    pub startup_timeout: Duration,
    /// ADR-0043 D3（Phase 56）: `Some` なら ACP エージェントをコンテナの中で起こす（`container::wrap`）。
    pub container: Option<crate::container::SharedPlan>,
}

impl Default for AcpConfig {
    fn default() -> Self {
        Self {
            command: "opencode".to_string(),
            args: vec!["acp".to_string()],
            env: Vec::new(),
            permission: AcpPermission::Allow,
            model: None,
            model_option_id: "model".to_string(),
            startup_timeout: Duration::from_secs(300),
            container: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AcpAdapter {
    config: AcpConfig,
}

impl AcpAdapter {
    pub const ID: &'static str = "acp";

    pub fn new(config: AcpConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WorkerAdapter for AcpAdapter {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn run(
        &self,
        req: RunRequest,
        run_id: &str,
        limits: RunLimits,
        sink: &dyn EventSink,
    ) -> Result<RunOutcome, AdapterError> {
        run_acp(&self.config, &req, run_id, &limits, sink).await
    }

    /// `claude_code::ClaudeCodeAdapter::with_env` と同じ規則（同名キーは後勝ち）。ADR-0026 D5:
    /// acp はアカウントプールを使わないが、`env` の上書き自体は他アダプタと同じ形で提供しておく。
    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.env.extend(extra.iter().cloned());
        Some(Arc::new(AcpAdapter::new(config)))
    }

    /// ADR-0043 D3（Phase 56）: コンテナの中で ACP エージェントを起こす複製。
    fn with_container(&self, plan: crate::container::SharedPlan) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.container = Some(plan);
        Some(Arc::new(AcpAdapter::new(config)))
    }
}

/// `artifacts/result.json`（`claude_code::ResultFile` と同じ規約。ADR-0006 D3, ADR-0026 D3）。
#[derive(Debug, Deserialize)]
struct ResultFile {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    evidence: serde_json::Value,
}

fn lenient_evidence(value: serde_json::Value) -> Vec<Evidence> {
    match value {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| serde_json::from_value::<Evidence>(item).ok())
            .collect(),
        _ => Vec::new(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

// --- JSON-RPC 2.0 の組み立て（ADR-0026 の実装メモ参照） ---

fn jsonrpc_request(id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn jsonrpc_notification(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params})
}

fn jsonrpc_response(id: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn jsonrpc_error_response(id: serde_json::Value, code: i64, message: String) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// エラーオブジェクト（`{"code":.., "message":..}`）を分類・ログ用の 1 行にする。
fn jsonrpc_error_text(error: &serde_json::Value) -> String {
    let message = error.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error");
    match error.get("code").and_then(|c| c.as_i64()) {
        Some(code) => format!("{message} (code {code})"),
        None => message.to_string(),
    }
}

async fn write_line(stdin: &mut ChildStdin, value: &serde_json::Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value).map_err(std::io::Error::other)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await
}

/// `pump_one_line` の結果。
enum Pumped {
    Eof,
    /// 通知・エージェントからのリクエスト・空行・非 JSON 行など、呼び出し元が気にする必要のないもの。
    Handled,
    /// 呼び出し元が送った何らかのリクエストへの応答（`id` と `result`/`error` を持つ行）。
    Response(serde_json::Value),
}

/// stdout から 1 行読み、通知（`session/update`）とエージェントからのリクエスト（`session/request_permission`
/// 等）はここで処理してしまい、レスポンス行だけを呼び出し元に返す（ADR-0026 D3/D4）。
async fn pump_one_line(
    reader: &mut BufReader<ChildStdout>,
    stdin: &mut ChildStdin,
    stdout_file: &mut tokio::fs::File,
    sink: &dyn EventSink,
    config: &AcpConfig,
    run_id: &str,
    chunks: &mut ChunkBuffer,
) -> std::io::Result<Pumped> {
    match read_line_limited(reader, MAX_LINE_BYTES).await? {
        LineOutcome::Eof => Ok(Pumped::Eof),
        LineOutcome::TooLong => {
            // ACP エージェント自身のフォーマットは taskd が定義したものではないため寛容に扱う
            // （ADR-0006 D5 と同じ方針）。
            sink.heartbeat();
            warn!("run {run_id}: discarding overlong line from acp agent stdout");
            Ok(Pumped::Handled)
        }
        LineOutcome::Line(bytes) => {
            sink.heartbeat();
            stdout_file.write_all(&bytes).await?;
            stdout_file.write_all(b"\n").await?;
            let text = String::from_utf8_lossy(&bytes);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(Pumped::Handled);
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                warn!("run {run_id}: discarding non-json line from acp agent stdout");
                return Ok(Pumped::Handled);
            };
            if value.get("method").and_then(|m| m.as_str()).is_some() {
                if value.get("id").is_some() {
                    handle_incoming_request(stdin, &value, config, run_id).await?;
                } else {
                    handle_notification(&value, sink, chunks);
                }
                return Ok(Pumped::Handled);
            }
            if value.get("id").is_some() {
                return Ok(Pumped::Response(value));
            }
            warn!("run {run_id}: discarding unrecognized line from acp agent stdout: {trimmed}");
            Ok(Pumped::Handled)
        }
    }
}

/// `session/request_permission`（およびクライアント能力を false にしているため本来来ないはずの
/// `fs/*`/`terminal/*`）に即答する（ADR-0026 D4）。
async fn handle_incoming_request(
    stdin: &mut ChildStdin,
    req: &serde_json::Value,
    config: &AcpConfig,
    run_id: &str,
) -> std::io::Result<()> {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    match method {
        "session/request_permission" => {
            let options = req
                .pointer("/params/options")
                .and_then(|o| o.as_array())
                .cloned()
                .unwrap_or_default();
            let outcome = match choose_permission_option(&options, config.permission, run_id) {
                Some(option_id) => serde_json::json!({"outcome": "selected", "optionId": option_id}),
                None => serde_json::json!({"outcome": "cancelled"}),
            };
            write_line(stdin, &jsonrpc_response(id, serde_json::json!({"outcome": outcome}))).await
        }
        other => {
            warn!("run {run_id}: acp agent sent an unsupported request {other:?}; responding with method not found");
            write_line(stdin, &jsonrpc_error_response(id, -32601, format!("unsupported method: {other}"))).await
        }
    }
}

/// `permission` に沿って選択肢を選ぶ。Allow は "allow always" → "allow once"、Deny は "reject always" →
/// "reject once" の優先順（task 指示: 要求が一致する選択肢を提供しなければ最初の選択肢を選び warn する）。
fn choose_permission_option(options: &[serde_json::Value], permission: AcpPermission, run_id: &str) -> Option<String> {
    let preferred_kinds: &[&str] = match permission {
        AcpPermission::Allow => &["allow_always", "allow_once"],
        AcpPermission::Deny => &["reject_always", "reject_once"],
    };
    for kind in preferred_kinds {
        if let Some(option_id) = options
            .iter()
            .find(|o| o.get("kind").and_then(|k| k.as_str()) == Some(*kind))
            .and_then(|o| o.get("optionId").and_then(|v| v.as_str()))
        {
            return Some(option_id.to_string());
        }
    }
    if let Some(option_id) = options.first().and_then(|o| o.get("optionId").and_then(|v| v.as_str())) {
        warn!("run {run_id}: acp permission request offered no option matching {permission:?}; choosing the first offered option");
        return Some(option_id.to_string());
    }
    warn!("run {run_id}: acp permission request offered no options at all");
    None
}

/// エージェントの本文はトークン単位の細切れで届く（実機の opencode + Qwen3.8-27B では 1 タスクで 259 件・
/// 平均 9 文字だった）。そのまま `progress` にすると `WorkerProgress` イベントが膨れるので、改行が来るか
/// 一定量たまるまで溜めてから出す。`heartbeat()` は溜めずに毎行呼ぶので、無出力タイムアウトの判定は変わらない。
#[derive(Default)]
struct ChunkBuffer {
    text: String,
}

impl ChunkBuffer {
    /// 改行が来なくてもこの文字数でいったん出す。
    const FLUSH_AT: usize = 400;

    fn push(&mut self, chunk: &str, sink: &dyn EventSink) {
        self.text.push_str(chunk);
        while let Some(idx) = self.text.find('\n') {
            let line: String = self.text.drain(..=idx).collect();
            Self::emit(line.trim(), sink);
        }
        if self.text.chars().count() >= Self::FLUSH_AT {
            self.flush(sink);
        }
    }

    fn flush(&mut self, sink: &dyn EventSink) {
        let text = std::mem::take(&mut self.text);
        Self::emit(text.trim(), sink);
    }

    fn emit(text: &str, sink: &dyn EventSink) {
        if !text.is_empty() {
            sink.progress(&truncate(text, 500));
        }
    }
}

/// `session/update` 通知を `EventSink` に写す（ADR-0026 D3 の表）。すべての通知で `pump_one_line` が既に
/// `heartbeat()` を呼んでいる。本文は `chunks` にまとめてから出す。
fn handle_notification(value: &serde_json::Value, sink: &dyn EventSink, chunks: &mut ChunkBuffer) {
    let Some(method) = value.get("method").and_then(|m| m.as_str()) else {
        return;
    };
    if method != "session/update" {
        return;
    }
    let Some(update) = value.pointer("/params/update") else {
        return;
    };
    match update.get("sessionUpdate").and_then(|k| k.as_str()).unwrap_or("") {
        "agent_message_chunk" | "agent_thought_chunk" => {
            if let Some(text) = update.pointer("/content/text").and_then(|t| t.as_str()) {
                chunks.push(text, sink);
            }
        }
        "tool_call" | "tool_call_update" => {
            // 本文の途中でツールが動いたら、溜めていた本文を先に出して順序を保つ。
            chunks.flush(sink);
            let name = update.get("title").and_then(|t| t.as_str()).unwrap_or("tool");
            let status = update.get("status").and_then(|s| s.as_str()).unwrap_or("pending");
            sink.progress(&format!("tool: {name} {status}"));
        }
        // `plan` やそれ以外の通知は heartbeat のみ（ADR-0026 D3 の表）。
        _ => {}
    }
}

/// `initialize` の応答を `startup_timeout` まで待つ（ADR-0026 D4）。`None` は EOF またはタイムアウト。
#[allow(clippy::too_many_arguments)]
async fn wait_for_initialize_response(
    reader: &mut BufReader<ChildStdout>,
    stdin: &mut ChildStdin,
    stdout_file: &mut tokio::fs::File,
    sink: &dyn EventSink,
    config: &AcpConfig,
    run_id: &str,
    startup_timeout: Duration,
    chunks: &mut ChunkBuffer,
) -> Result<Option<serde_json::Value>, AdapterError> {
    let deadline = Instant::now() + startup_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let pumped = match tokio::time::timeout(remaining, pump_one_line(reader, stdin, stdout_file, sink, config, run_id, chunks)).await
        {
            Err(_elapsed) => return Ok(None),
            Ok(Err(e)) => return Err(AdapterError::Io(e)),
            Ok(Ok(p)) => p,
        };
        match pumped {
            Pumped::Eof => return Ok(None),
            Pumped::Handled => continue,
            Pumped::Response(value) => {
                if value.get("id").and_then(|v| v.as_u64()) == Some(1) {
                    return Ok(Some(value));
                }
                continue;
            }
        }
    }
}

/// `initialize` 以外のリクエストへの応答を、通常の壁時計・アイドルタイムアウトの下で待つ（ADR-0026 D4）。
enum WaitOutcome {
    Response(serde_json::Value),
    Eof,
    TimedOut(Terminal),
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_response(
    reader: &mut BufReader<ChildStdout>,
    stdin: &mut ChildStdin,
    stdout_file: &mut tokio::fs::File,
    sink: &dyn EventSink,
    config: &AcpConfig,
    run_id: &str,
    expected_id: u64,
    start: Instant,
    last_activity: &mut Instant,
    limits: &RunLimits,
    chunks: &mut ChunkBuffer,
) -> Result<WaitOutcome, AdapterError> {
    loop {
        let wall_elapsed = start.elapsed();
        if wall_elapsed >= limits.wall_clock {
            return Ok(WaitOutcome::TimedOut(Terminal::Error {
                message: "wall clock exceeded".into(),
                retryable: true,
            }));
        }
        let idle_elapsed = last_activity.elapsed();
        if idle_elapsed >= limits.idle_timeout {
            return Ok(WaitOutcome::TimedOut(Terminal::Error {
                message: "idle timeout".into(),
                retryable: true,
            }));
        }
        let wait = (limits.wall_clock - wall_elapsed).min(limits.idle_timeout - idle_elapsed);
        let pumped = match tokio::time::timeout(wait, pump_one_line(reader, stdin, stdout_file, sink, config, run_id, chunks)).await {
            Err(_elapsed) => continue, // タイムアウト。ループ先頭で上限超過を検知する。
            Ok(Err(e)) => return Err(AdapterError::Io(e)),
            Ok(Ok(p)) => p,
        };
        match pumped {
            Pumped::Eof => return Ok(WaitOutcome::Eof),
            Pumped::Handled => {
                *last_activity = Instant::now();
                continue;
            }
            Pumped::Response(value) => {
                *last_activity = Instant::now();
                if value.get("id").and_then(|v| v.as_u64()) == Some(expected_id) {
                    return Ok(WaitOutcome::Response(value));
                }
                warn!("run {run_id}: discarding response with unexpected id: {value}");
                continue;
            }
        }
    }
}

/// Phase A（`initialize`・`session/new`。まだセッションが無い）の失敗をまとめて処理する: プロセスを
/// 止め、JSON-RPC のエラー本文と stderr の末尾を分類する（ADR-0026 D5）。分類できなければ
/// `AdapterError::Spawn`（ADR-0026 D3: 版が V1 でなければ「spawn_failed として終える」の実装）。
async fn kill_and_classify(
    child: &mut Child,
    stderr_task: tokio::task::JoinHandle<()>,
    stderr_log_path: &Path,
    grace: Duration,
    jsonrpc_error_text: Option<String>,
    fallback_message: String,
) -> AdapterError {
    let _ = kill_now(child, grace).await;
    if let Err(e) = stderr_task.await {
        warn!("stderr capture task failed while classifying an acp spawn failure: {e}");
    }
    let tail = read_tail(stderr_log_path, 4096).await;
    let combined = match &jsonrpc_error_text {
        Some(t) => format!("{t}\n{tail}"),
        None => tail,
    };
    match classify_provider_failure(&combined) {
        Some(pf) => AdapterError::from_provider_failure(pf, jsonrpc_error_text.as_deref().unwrap_or(&fallback_message)),
        None => AdapterError::Spawn(std::io::Error::other(fallback_message)),
    }
}

/// Phase B（セッションが存在する状態）で run を終えるときの生データ。
enum RawOutcome {
    TimedOut(Terminal),
    Eof { context: &'static str },
    RpcError { context: &'static str, error_text: String },
    Success { stop_reason: String },
}

/// プロセスを止め（タイムアウトなら `session/cancel` → `kill_grace` 後 SIGKILL、そうでなければ穏やかな
/// 刈り取り。ADR-0026 D4）、結果ファイル（`<artifacts_dir>/result.json`）から終端を合成し、`runs/<run_id>/result.json` に書く
/// （ADR-0006 D3, ADR-0010 D10）。
#[allow(clippy::too_many_arguments)]
async fn finish_run(
    child: &mut Child,
    mut stdin: ChildStdin,
    session_id: &str,
    limits: &RunLimits,
    stderr_task: tokio::task::JoinHandle<()>,
    mut stdout_file: tokio::fs::File,
    run_dir: &Path,
    // ADR-0036 D1/D2: 成果物と結果ファイルの置き場（`RunRequest.artifacts_dir`）と、その workspace 相対表記。
    artifacts_dir: &Path,
    artifacts_rel: &str,
    stderr_log_path: &Path,
    sink: &dyn EventSink,
    outcome: RawOutcome,
    run_id: &str,
) -> Result<RunOutcome, AdapterError> {
    let force_kill = matches!(outcome, RawOutcome::TimedOut(_));
    let exit_status = if force_kill {
        // ADR-0026 D4: まず `session/cancel` を送り、`kill_grace` 待ってからプロセスグループへ SIGKILL。
        let _ = write_line(&mut stdin, &jsonrpc_notification("session/cancel", serde_json::json!({"sessionId": session_id}))).await;
        match tokio::time::timeout(limits.kill_grace, child.wait()).await {
            Ok(status) => status?,
            Err(_elapsed) => {
                send_signal_to_group(child, Signal::SIGKILL);
                child.wait().await?
            }
        }
    } else {
        reap_after_terminal(child, limits.kill_grace).await?
    };
    if let Err(e) = stderr_task.await {
        warn!("run {run_id}: stderr capture task failed: {e}");
    }
    stdout_file.flush().await?;

    let (terminal, provider_failure): (Terminal, Option<ProviderFailure>) = match outcome {
        // タイムアウトは分類しない（ADR-0010 D5 と同じ方針）。
        RawOutcome::TimedOut(t) => (t, None),
        RawOutcome::Eof { context } => {
            let exit_repr = match exit_status.code() {
                Some(code) => code.to_string(),
                None => "signal".to_string(),
            };
            let tail = read_tail(stderr_log_path, 4096).await;
            let pf = classify_provider_failure(&tail);
            (
                Terminal::Error {
                    message: format!("acp agent exited before responding to {context} (exit={exit_repr})"),
                    retryable: true,
                },
                pf,
            )
        }
        RawOutcome::RpcError { context, error_text } => {
            let tail = read_tail(stderr_log_path, 4096).await;
            let combined = format!("{error_text}\n{tail}");
            let pf = classify_provider_failure(&combined);
            (
                Terminal::Error {
                    message: format!("acp agent returned an error for {context}: {error_text}"),
                    retryable: true,
                },
                pf,
            )
        }
        RawOutcome::Success { stop_reason } => {
            (terminal_from_result_file(artifacts_dir, artifacts_rel, &stop_reason).await, None)
        }
    };

    forward_delegate_file(artifacts_dir, sink).await;
    write_result_json(run_dir, &terminal, provider_failure).await?;

    if let (Terminal::Error { message, .. }, Some(pf)) = (&terminal, provider_failure) {
        return Err(AdapterError::from_provider_failure(pf, message));
    }

    Ok(RunOutcome {
        terminal,
        exit_code: exit_status.code(),
    })
}

/// 結果ファイル（`<artifacts_dir>/result.json`）から終端を合成する（ADR-0026 D3 手順 7、ADR-0036 D2）。
/// ACP 自体には成功/失敗の強い意味論が無いので（D1: 運搬・観測・生存管理だけ）、`stopReason` に関わらず
/// 常にこのファイルを見る。ファイルが無ければ `stopReason` を理由として `Error{retryable:true}` にする。
async fn terminal_from_result_file(artifacts_dir: &Path, artifacts_rel: &str, stop_reason: &str) -> Terminal {
    let result_path = artifacts_dir.join("result.json");
    let text = match tokio::fs::read_to_string(&result_path).await {
        Ok(t) => t,
        Err(_) => {
            return Terminal::Error {
                message: format!("acp agent stopped ({stop_reason}) without {artifacts_rel}/result.json"),
                retryable: true,
            };
        }
    };
    match serde_json::from_str::<ResultFile>(&text) {
        Ok(rf) => {
            if let Some(question) = rf.question {
                Terminal::Question { text: question }
            } else if let Some(summary) = rf.summary {
                // ADR-0026: PromptResponse の token 使用量は `unstable_end_turn_token_usage`
                // （安定版 ACP には無い）でしか運ばれず、opencode の実機確認でも観測されていないため
                // 常に `None` にする（`usage` を埋める標準フィールドが無い）。
                Terminal::Done {
                    summary,
                    evidence: lenient_evidence(rf.evidence),
                    usage: None,
                }
            } else {
                Terminal::Error {
                    message: format!("{artifacts_rel}/result.json has neither 'summary' nor 'question'"),
                    retryable: true,
                }
            }
        }
        Err(e) => Terminal::Error {
            message: format!("{artifacts_rel}/result.json is not valid JSON: {e}"),
            retryable: true,
        },
    }
}

async fn run_acp(
    config: &AcpConfig,
    req: &RunRequest,
    run_id: &str,
    limits: &RunLimits,
    sink: &dyn EventSink,
) -> Result<RunOutcome, AdapterError> {
    let run_dir = req.workspace.join("runs").join(run_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    let stdout_log_path = run_dir.join("stdout.jsonl");
    let stderr_log_path = run_dir.join("stderr.log");
    let stderr_log_path_for_task = stderr_log_path.clone();

    // 前回の run（リトライ）が残した結果ファイルを、今回の run の結果と誤読しないよう先に消す
    // （claude_code / codex と同じ。ADR-0006 D3）。
    // ADR-0036 D1/D2: 置き場はディスパッチャが決めた `artifacts_dir`（共有 workspace ではタスクごと）。
    let artifacts_rel = req.artifacts_rel();
    let result_path = req.artifact_path("result.json");
    let _ = tokio::fs::remove_file(&result_path).await;
    clear_delegate_file(&req.artifacts_dir).await;

    let prompt = build_prompt(&req.task, &req.context, run_id, &artifacts_rel);
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &prompt, run_id).await;

    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .envs(config.env.iter().cloned())
        .current_dir(req.cwd());
    // ★ ADR-0043 D3 の差し込み点（コンテナ実行）。`None` ならそのまま（ホスト実行は変わらない）。
    let mut command = crate::container::wrap(command, config.container.as_deref());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(AdapterError::Spawn)?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AdapterError::Other("worker stdin was not piped".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AdapterError::Other("worker stdout was not piped".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AdapterError::Other("worker stderr was not piped".into()))?;

    let stderr_task = tokio::spawn(async move {
        let mut reader = stderr;
        match tokio::fs::File::create(&stderr_log_path_for_task).await {
            Ok(mut file) => {
                if let Err(e) = tokio::io::copy(&mut reader, &mut file).await {
                    warn!("failed to write worker stderr.log: {e}");
                }
            }
            Err(e) => warn!("failed to create worker stderr.log: {e}"),
        }
    });

    let mut stdout_file = tokio::fs::File::create(&stdout_log_path).await?;
    let mut reader = BufReader::new(stdout);
    // 本文のチャンクをまとめる入れ物（run 全体で 1 つ。initialize の前から使う）。
    let mut chunks = ChunkBuffer::default();

    // --- Phase A: initialize（ADR-0026 D3 手順 2, D4: startup_timeout） ---
    let init_params = serde_json::json!({
        "protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
    });
    if write_line(&mut stdin, &jsonrpc_request(1, "initialize", init_params)).await.is_err() {
        return Err(
            kill_and_classify(
                &mut child,
                stderr_task,
                &stderr_log_path,
                limits.kill_grace,
                None,
                "failed to write the initialize request to the acp agent's stdin".into(),
            )
            .await,
        );
    }

    let init_response =
        match wait_for_initialize_response(
            &mut reader,
            &mut stdin,
            &mut stdout_file,
            sink,
            config,
            run_id,
            config.startup_timeout,
            &mut chunks,
        )
            .await
        {
            Ok(Some(v)) => v,
            Ok(None) => {
                return Err(
                    kill_and_classify(
                        &mut child,
                        stderr_task,
                        &stderr_log_path,
                        limits.kill_grace,
                        None,
                        "acp agent did not respond to initialize within the startup timeout".into(),
                    )
                    .await,
                );
            }
            Err(e) => {
                let _ = kill_now(&mut child, limits.kill_grace).await;
                let _ = stderr_task.await;
                return Err(e);
            }
        };

    if let Some(error) = init_response.get("error") {
        let msg = jsonrpc_error_text(error);
        return Err(
            kill_and_classify(
                &mut child,
                stderr_task,
                &stderr_log_path,
                limits.kill_grace,
                Some(msg.clone()),
                format!("acp agent rejected initialize: {msg}"),
            )
            .await,
        );
    }
    let negotiated = init_response.pointer("/result/protocolVersion").and_then(|v| v.as_u64());
    if negotiated != Some(1) {
        // ADR-0026 D3: 版の交渉はアダプタの中に閉じる。V1 でなければ spawn_failed として終える。
        return Err(
            kill_and_classify(
                &mut child,
                stderr_task,
                &stderr_log_path,
                limits.kill_grace,
                None,
                format!("acp agent negotiated protocol version {negotiated:?}, expected 1"),
            )
            .await,
        );
    }

    // ここから先は通常の壁時計・アイドルタイムアウトを使う（ADR-0026 D4。`startup_timeout` は
    // initialize の応答待ちだけに使う別枠）。
    let start = Instant::now();
    let mut last_activity = Instant::now();

    // --- Phase A 続き: session/new（ADR-0026 D3 手順 3） ---
    let new_session_params = serde_json::json!({
        "cwd": req.cwd().to_string_lossy(),
        "mcpServers": [],
    });
    if write_line(&mut stdin, &jsonrpc_request(2, "session/new", new_session_params)).await.is_err() {
        return Err(
            kill_and_classify(
                &mut child,
                stderr_task,
                &stderr_log_path,
                limits.kill_grace,
                None,
                "failed to write the session/new request to the acp agent's stdin".into(),
            )
            .await,
        );
    }
    let new_session_response = match wait_for_response(
        &mut reader,
        &mut stdin,
        &mut stdout_file,
        sink,
        config,
        run_id,
        2,
        start,
        &mut last_activity,
        limits,
        &mut chunks,
    )
    .await
    {
        Ok(WaitOutcome::Response(v)) => v,
        Ok(WaitOutcome::Eof) => {
            return Err(
                kill_and_classify(
                    &mut child,
                    stderr_task,
                    &stderr_log_path,
                    limits.kill_grace,
                    None,
                    "acp agent exited before responding to session/new".into(),
                )
                .await,
            );
        }
        Ok(WaitOutcome::TimedOut(terminal)) => {
            let message = match &terminal {
                Terminal::Error { message, .. } => message.clone(),
                _ => "timeout waiting for session/new".to_string(),
            };
            return Err(
                kill_and_classify(&mut child, stderr_task, &stderr_log_path, limits.kill_grace, None, message).await,
            );
        }
        Err(e) => {
            let _ = kill_now(&mut child, limits.kill_grace).await;
            let _ = stderr_task.await;
            return Err(e);
        }
    };
    if let Some(error) = new_session_response.get("error") {
        let msg = jsonrpc_error_text(error);
        return Err(
            kill_and_classify(
                &mut child,
                stderr_task,
                &stderr_log_path,
                limits.kill_grace,
                Some(msg.clone()),
                format!("acp agent rejected session/new: {msg}"),
            )
            .await,
        );
    }
    let Some(session_id) = new_session_response
        .pointer("/result/sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
    else {
        return Err(
            kill_and_classify(
                &mut child,
                stderr_task,
                &stderr_log_path,
                limits.kill_grace,
                None,
                "acp agent session/new response had no sessionId".into(),
            )
            .await,
        );
    };

    // --- Phase B: セッションが存在する。以降の失敗は artifacts/result.json 経由の終端として扱う。 ---
    let mut next_id = 3u64;

    // 手順 4: モデル指定があれば session/set_config_option（ADR-0026 D3）。
    if let Some(model) = config.model.as_deref().filter(|m| !m.is_empty()) {
        let has_option = new_session_response
            .pointer("/result/configOptions")
            .and_then(|v| v.as_array())
            .map(|opts| opts.iter().any(|o| o.get("id").and_then(|i| i.as_str()) == Some(config.model_option_id.as_str())))
            .unwrap_or(false);
        if has_option {
            let id = next_id;
            next_id += 1;
            // 実機（opencode 1.18.31）は `configId` を要求する（`optionId` は -32602 Invalid params）。
            // `session/new` の `configOptions[].id` と対になる名前で、権限要求の `optionId` とは別物。
            let params = serde_json::json!({
                "sessionId": session_id,
                "configId": config.model_option_id,
                "value": model,
            });
            if write_line(&mut stdin, &jsonrpc_request(id, "session/set_config_option", params)).await.is_ok() {
                match wait_for_response(
                    &mut reader,
                    &mut stdin,
                    &mut stdout_file,
                    sink,
                    config,
                    run_id,
                    id,
                    start,
                    &mut last_activity,
                    limits,
                    &mut chunks,
                )
                .await
                {
                    Ok(WaitOutcome::Response(v)) => {
                        if let Some(error) = v.get("error") {
                            warn!(
                                "run {run_id}: acp agent rejected session/set_config_option: {}",
                                jsonrpc_error_text(error)
                            );
                        }
                    }
                    Ok(WaitOutcome::Eof) => {
                        return finish_run(
                            &mut child,
                            stdin,
                            &session_id,
                            limits,
                            stderr_task,
                            stdout_file,
                            &run_dir,
                            &req.artifacts_dir,
                            &artifacts_rel,
                            &stderr_log_path,
                            sink,
                            RawOutcome::Eof { context: "session/set_config_option" },
                            run_id,
                        )
                        .await;
                    }
                    Ok(WaitOutcome::TimedOut(terminal)) => {
                        return finish_run(
                            &mut child,
                            stdin,
                            &session_id,
                            limits,
                            stderr_task,
                            stdout_file,
                            &run_dir,
                            &req.artifacts_dir,
                            &artifacts_rel,
                            &stderr_log_path,
                            sink,
                            RawOutcome::TimedOut(terminal),
                            run_id,
                        )
                        .await;
                    }
                    Err(e) => {
                        let _ = kill_now(&mut child, limits.kill_grace).await;
                        let _ = stderr_task.await;
                        return Err(e);
                    }
                }
            } else {
                warn!("run {run_id}: failed to write session/set_config_option to the acp agent's stdin");
            }
        } else {
            warn!(
                "run {run_id}: acp model_option_id {:?} was not found in session/new's configOptions; skipping session/set_config_option",
                config.model_option_id
            );
        }
    }

    // 手順 5: session/prompt（ADR-0026 D3）。
    let prompt_id = next_id;
    let prompt_params = serde_json::json!({
        "sessionId": session_id,
        "prompt": [{"type": "text", "text": prompt}],
    });
    if let Err(e) = write_line(&mut stdin, &jsonrpc_request(prompt_id, "session/prompt", prompt_params)).await {
        let _ = kill_now(&mut child, limits.kill_grace).await;
        let _ = stderr_task.await;
        return Err(AdapterError::Io(e));
    }

    let prompt_wait = match wait_for_response(
        &mut reader,
        &mut stdin,
        &mut stdout_file,
        sink,
        config,
        run_id,
        prompt_id,
        start,
        &mut last_activity,
        limits,
        &mut chunks,
    )
    .await
    {
        Ok(w) => w,
        Err(e) => {
            let _ = kill_now(&mut child, limits.kill_grace).await;
            let _ = stderr_task.await;
            return Err(e);
        }
    };

    // 溜めたままの本文を最後に出し切る（改行で終わらない返答を落とさない）。
    chunks.flush(sink);

    let outcome = match prompt_wait {
        WaitOutcome::TimedOut(terminal) => RawOutcome::TimedOut(terminal),
        WaitOutcome::Eof => RawOutcome::Eof { context: "session/prompt" },
        WaitOutcome::Response(value) => match value.get("error") {
            Some(error) => RawOutcome::RpcError {
                context: "session/prompt",
                error_text: jsonrpc_error_text(error),
            },
            None => {
                let stop_reason = value
                    .pointer("/result/stopReason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                RawOutcome::Success { stop_reason }
            }
        },
    };

    finish_run(
        &mut child,
        stdin,
        &session_id,
        limits,
        stderr_task,
        stdout_file,
        &run_dir,
        &req.artifacts_dir,
        &artifacts_rel,
        &stderr_log_path,
        sink,
        outcome,
        run_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use task_core::{ArtifactRef, DelegateTask, RateLimitObservation};

    use super::*;
    use crate::protocol::{PROTOCOL_VERSION, RunContext};

    #[derive(Default)]
    struct RecordingSink {
        progress: Mutex<Vec<String>>,
        delegated: Mutex<Vec<Vec<DelegateTask>>>,
        rate_limits: Mutex<Vec<RateLimitObservation>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress.lock().unwrap_or_else(|e| e.into_inner()).push(msg.to_string());
        }
        fn artifact(&self, _artifact: &ArtifactRef) {}
        fn delegate(&self, tasks: &[DelegateTask]) {
            self.delegated.lock().unwrap_or_else(|e| e.into_inner()).push(tasks.to_vec());
        }
        fn rate_limit(&self, obs: RateLimitObservation) {
            self.rate_limits.lock().unwrap_or_else(|e| e.into_inner()).push(obs);
        }
    }

    fn progress_of(sink: &RecordingSink) -> Vec<String> {
        sink.progress.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn stub_acp(dir: &Path, script: &str) -> AcpConfig {
        let path = dir.join("acp_stub.sh");
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        AcpConfig {
            command: path.to_string_lossy().into_owned(),
            args: Vec::new(),
            startup_timeout: Duration::from_secs(5),
            ..AcpConfig::default()
        }
    }

    fn sample_req(workspace: std::path::PathBuf) -> RunRequest {
        RunRequest {
            protocol: PROTOCOL_VERSION,
            task: crate::protocol::tests::sample_task(),
            artifacts_dir: workspace.join("artifacts"),
            workspace,
            work_dir: None,
            context: RunContext::default(),
        }
    }

    fn default_limits() -> RunLimits {
        RunLimits {
            wall_clock: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
            kill_grace: Duration::from_millis(200),
        }
    }

    /// 基本のハンドシェイク（`initialize` → `session/new` → `session/prompt`）に応じる雛形。呼び出し側が
    /// prompt を受け取ったあとの反応（3 番目の `read` 以降）を追加する。
    const HANDSHAKE: &str = r#"
mkdir -p artifacts
read -r _init
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}}'
read -r _new
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1","configOptions":[]}}'
"#;

    #[tokio::test]
    async fn happy_path_progress_and_done_from_result_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            &format!(
                r#"{HANDSHAKE}
read -r _prompt
printf '%s\n' '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"sess-1","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"working on it"}}}}}}}}'
printf '%s\n' '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"sess-1","update":{{"sessionUpdate":"tool_call","title":"Bash","status":"in_progress"}}}}}}'
printf '%s' '{{"summary":"added usage example","evidence":[]}}' > artifacts/result.json
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"end_turn"}}}}'
"#
            ),
        );
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-1", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, evidence, usage } => {
                assert_eq!(summary, "added usage example");
                assert!(evidence.is_empty());
                assert_eq!(usage, None);
            }
            other => panic!("expected done, got {other:?}"),
        }
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m == "working on it"));
        assert!(progress.iter().any(|m| m.starts_with("tool: Bash")));
        assert!(dir.path().join("runs/run-1/stdout.jsonl").is_file());

        let result_json = std::fs::read_to_string(dir.path().join("runs/run-1/result.json")).unwrap();
        match serde_json::from_str::<crate::protocol::WorkerMessage>(result_json.trim()).unwrap() {
            crate::protocol::WorkerMessage::Done { summary, .. } => assert_eq!(summary, "added usage example"),
            other => panic!("expected done in result.json, got {other:?}"),
        }
    }

    /// ADR-0036 D1/D2: 共有 workspace のタスクは `.taskd/artifacts/<task_id>/result.json` を読む。
    #[tokio::test]
    async fn a_shared_workspace_task_uses_its_own_artifacts_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            &format!(
                r#"{HANDSHAKE}
read -r _prompt
mkdir -p .taskd/artifacts/T1
printf '%s' '{{"summary":"mine","evidence":[]}}' > .taskd/artifacts/T1/result.json
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"end_turn"}}}}'
"#
            ),
        );
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(dir.path().join("artifacts/result.json"), r#"{"summary":"sibling"}"#).unwrap();
        let adapter = AcpAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.artifacts_dir = dir.path().join(".taskd/artifacts/T1");
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-shared", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert_eq!(summary, "mine"),
            other => panic!("expected done, got {other:?}"),
        }
        let prompt = std::fs::read_to_string(dir.path().join("runs/run-shared/prompt.txt")).unwrap();
        assert!(prompt.contains(".taskd/artifacts/T1/result.json"), "{prompt}");
    }

    #[tokio::test]
    async fn question_in_result_file_blocks_task() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            &format!(
                r#"{HANDSHAKE}
read -r _prompt
printf '%s' '{{"question":"which crate version?"}}' > artifacts/result.json
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"end_turn"}}}}'
"#
            ),
        );
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-2", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Question { text } => assert_eq!(text, "which crate version?"),
            other => panic!("expected question, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_result_file_after_stop_reason_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            &format!(
                r#"{HANDSHAKE}
read -r _prompt
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"refusal"}}}}'
"#
            ),
        );
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-3", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("refusal"), "{message}");
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn protocol_version_mismatch_is_a_spawn_failure() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            r#"
read -r _init
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":2,"agentCapabilities":{}}}'
sleep 5
"#,
        );
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-4", default_limits(), &sink)
            .await
            .expect_err("expected a spawn failure");
        match err {
            AdapterError::Spawn(e) => assert!(e.to_string().contains("protocol version"), "{e}"),
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn startup_timeout_without_any_response_is_a_spawn_failure() {
        let dir = tempfile::tempdir().unwrap();
        let config = AcpConfig {
            startup_timeout: Duration::from_millis(200),
            ..stub_acp(dir.path(), "read -r _init\nsleep 5\n")
        };
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let start = Instant::now();
        let err = adapter
            .run(req, "run-5", default_limits(), &sink)
            .await
            .expect_err("expected a spawn failure");
        assert!(start.elapsed() < Duration::from_secs(5));
        match err {
            AdapterError::Spawn(e) => assert!(e.to_string().contains("startup timeout"), "{e}"),
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    /// ADR-0026 D4: `permission = allow` なら "allow" 系の選択肢を選ぶ。
    #[tokio::test]
    async fn permission_allow_selects_an_allow_option() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            &format!(
                r#"{HANDSHAKE}
read -r _prompt
printf '%s\n' '{{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{{"sessionId":"sess-1","options":[{{"optionId":"reject-once","name":"Reject","kind":"reject_once"}},{{"optionId":"allow-once","name":"Allow once","kind":"allow_once"}},{{"optionId":"allow-always","name":"Allow always","kind":"allow_always"}}]}}}}'
read -r permresp
echo "$permresp" > permission_response.json
printf '%s' '{{"summary":"ok","evidence":[]}}' > artifacts/result.json
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"end_turn"}}}}'
"#
            ),
        );
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-6", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let response = std::fs::read_to_string(dir.path().join("permission_response.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"]["outcome"]["optionId"], "allow-always");
    }

    /// ADR-0026 D4: `permission = deny` なら "reject" 系の選択肢を選ぶ。
    #[tokio::test]
    async fn permission_deny_selects_a_reject_option() {
        let dir = tempfile::tempdir().unwrap();
        let config = AcpConfig {
            permission: AcpPermission::Deny,
            ..stub_acp(
                dir.path(),
                &format!(
                    r#"{HANDSHAKE}
read -r _prompt
printf '%s\n' '{{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{{"sessionId":"sess-1","options":[{{"optionId":"allow-once","name":"Allow once","kind":"allow_once"}},{{"optionId":"reject-always","name":"Reject always","kind":"reject_always"}}]}}}}'
read -r permresp
echo "$permresp" > permission_response.json
printf '%s' '{{"summary":"ok","evidence":[]}}' > artifacts/result.json
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"end_turn"}}}}'
"#
                ),
            )
        };
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-7", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let response = std::fs::read_to_string(dir.path().join("permission_response.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"]["outcome"]["optionId"], "reject-always");
    }

    /// 壁時計の超過で `session/cancel` を送ってから、応答が無ければプロセスグループごと SIGKILL する
    /// （ADR-0026 D4）。子プロセスが本当に居なくなることを `/proc/<pid>` の消滅で確認する。
    #[tokio::test]
    async fn wall_clock_exceeded_cancels_then_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid.txt");
        let config = stub_acp(
            dir.path(),
            &format!(
                r#"{HANDSHAKE}
read -r _prompt
echo $$ > {pid}
while true; do sleep 0.1; done
"#,
                pid = pid_file.display()
            ),
        );
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_millis(500),
            idle_timeout: Duration::from_secs(30),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-8", limits, &sink).await.unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("wall clock exceeded"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        // stub は 3 番目の read の直後に自分の pid を書く。読めなかった (=タイムアウト前に到達できなかった)
        // 場合はテストの前提が崩れているのでそこで失敗させる。
        let pid_text = std::fs::read_to_string(&pid_file).expect("stub should have recorded its pid before looping");
        let pid: i32 = pid_text.trim().parse().expect("pid.txt should contain a pid");
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists(), "process {pid} should have been killed");
    }

    #[tokio::test]
    async fn idle_timeout_kills_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            &format!(
                r#"{HANDSHAKE}
read -r _prompt
printf '%s\n' '{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"sess-1","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"start"}}}}}}}}'
while true; do sleep 0.1; done
"#
            ),
        );
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_secs(30),
            idle_timeout: Duration::from_millis(300),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-9", limits, &sink).await.unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("idle timeout"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// ADR-0016 M8: run の終わりに `artifacts/delegate.json` があれば `sink.delegate` が呼ばれる。
    #[tokio::test]
    async fn delegate_json_written_by_worker_is_forwarded_to_sink() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            &format!(
                r#"{HANDSHAKE}
read -r _prompt
printf '%s' '{{"summary":"delegated two subtasks","evidence":[]}}' > artifacts/result.json
printf '%s' '{{"tasks":[{{"title":"a","objective":"do a","acceptance":[{{"text":"c","check":{{"type":"human"}}}}]}},{{"title":"b","objective":"do b","acceptance":[{{"text":"c","check":{{"type":"human"}}}}]}}]}}' > artifacts/delegate.json
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"end_turn"}}}}'
"#
            ),
        );
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-10", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let delegated = sink.delegated.lock().unwrap();
        assert_eq!(delegated.len(), 1);
        assert_eq!(delegated[0].len(), 2);
    }

    /// 本文のチャンクはまとめて `progress` にする（実機の opencode は 1 タスクで 259 件・平均 9 文字を出した）。
    /// 改行が来たらそこで区切り、ツール呼び出しの前には溜め分を先に出し、最後に残りを出し切る。
    #[tokio::test]
    async fn agent_message_chunks_are_coalesced_into_few_progress_lines() {
        let sink = RecordingSink::default();
        let mut buffer = ChunkBuffer::default();
        for chunk in ["Cre", "ated ", "artifacts", "/ok.txt"] {
            buffer.push(chunk, &sink);
        }
        assert!(progress_of(&sink).is_empty(), "改行も上限も来ていないので、まだ出さない");

        buffer.push(" done\nnext line", &sink);
        assert_eq!(progress_of(&sink), vec!["Created artifacts/ok.txt done".to_string()]);

        buffer.flush(&sink);
        assert_eq!(
            progress_of(&sink),
            vec!["Created artifacts/ok.txt done".to_string(), "next line".to_string()]
        );

        // 上限（FLUSH_AT）を超えたら改行が無くても出す。
        let sink2 = RecordingSink::default();
        let mut buffer2 = ChunkBuffer::default();
        for _ in 0..ChunkBuffer::FLUSH_AT {
            buffer2.push("x", &sink2);
        }
        assert_eq!(progress_of(&sink2).len(), 1);
    }

    /// `model` を指定すると `session/set_config_option` が送られ、値がそのまま渡る。
    #[tokio::test]
    async fn model_option_is_set_when_configured_and_offered() {
        let dir = tempfile::tempdir().unwrap();
        let config = AcpConfig {
            model: Some("qwen-local/qwen3.8-27b".to_string()),
            ..stub_acp(
                dir.path(),
                r#"
mkdir -p artifacts
read -r _init
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
read -r _new
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1","configOptions":[{"id":"model","type":"select","currentValue":"a","options":["a","b"]}]}}'
read -r l3
echo "$l3" >> received.log
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}'
read -r l4
echo "$l4" >> received.log
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}'
"#,
            )
        };
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-11", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let received = std::fs::read_to_string(dir.path().join("received.log")).unwrap();
        assert!(received.contains("session/set_config_option"), "{received}");
        assert!(received.contains("qwen-local/qwen3.8-27b"), "{received}");
        assert!(received.contains("\"configId\":\"model\""), "{received}");
    }

    /// `model` が空ならば `session/set_config_option` は送らない。
    #[tokio::test]
    async fn model_option_is_not_set_when_model_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            r#"
mkdir -p artifacts
read -r _init
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
read -r _new
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1","configOptions":[{"id":"model","type":"select","currentValue":"a","options":["a","b"]}]}}'
read -r l3
echo "$l3" >> received.log
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
"#,
        );
        assert!(config.model.is_none());
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-12", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let received = std::fs::read_to_string(dir.path().join("received.log")).unwrap();
        assert!(!received.contains("session/set_config_option"), "{received}");
        assert!(received.contains("session/prompt"), "{received}");
    }

    /// ADR-0026 D5: `session/prompt` の JSON-RPC エラーを分類し `AdapterError::Throttled` として返す。
    #[tokio::test]
    async fn prompt_error_classified_as_throttled_surfaces_as_adapter_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            &format!(
                r#"{HANDSHAKE}
read -r _prompt
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"error":{{"code":-32000,"message":"429 rate limit exceeded"}}}}'
"#
            ),
        );
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-13", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::Throttled { .. }), "{err:?}");
        assert!(dir.path().join("runs/run-13/result.json").is_file());
    }

    /// 同じく `AuthFailed` の分類（ADR-0026 D5）。
    #[tokio::test]
    async fn prompt_error_classified_as_auth_failed_surfaces_as_adapter_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_acp(
            dir.path(),
            &format!(
                r#"{HANDSHAKE}
read -r _prompt
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"error":{{"code":-32000,"message":"401 Unauthorized: not logged in"}}}}'
"#
            ),
        );
        let adapter = AcpAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-14", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::AuthFailed(_)), "{err:?}");
        assert!(dir.path().join("runs/run-14/result.json").is_file());
    }

    /// ADR-0024/0026 と同じ規則: `with_env` の追加分は既存の同名キーより後に環境を組み立てるので勝つ。
    #[tokio::test]
    async fn with_env_overrides_a_same_name_key_already_in_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("env-seen.txt");
        let mut config = stub_acp(
            dir.path(),
            &format!(
                r#"printf '%s' "$ACP_TEST_VAR" > {out}
{HANDSHAKE}
read -r _prompt
printf '%s' '{{"summary":"ok","evidence":[]}}' > artifacts/result.json
printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"end_turn"}}}}'
"#,
                out = out_file.display()
            ),
        );
        config.env.push(("ACP_TEST_VAR".to_string(), "old".to_string()));
        let base = AcpAdapter::new(config);
        let with_env = base
            .with_env(&[("ACP_TEST_VAR".to_string(), "new".to_string())])
            .expect("acp supports with_env");

        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = with_env.run(req, "run-15", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let seen = std::fs::read_to_string(&out_file).unwrap();
        assert_eq!(seen, "new");
    }
}
