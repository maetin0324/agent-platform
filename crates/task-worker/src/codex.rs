//! `codex` アダプタ（DESIGN §5.4, ADR-0008 D3）。
//!
//! `codex exec --json` は celeris 独自のワーカープロトコルを話さない。`--json` が吐く JSON Lines
//! （`thread.started` → `item.*`（進捗）→ `turn.completed`/`turn.failed`）を読み、`claude-code`
//! （ADR-0006）と同じ「結果ファイル規約」（`artifacts/result.json`）で `RunOutcome` を合成する。
//! プロンプト組み立ては `claude_code::build_prompt` をそのまま再利用する（ADR-0008 D3: kind 別の
//! 文面をアダプタごとに複製しない）。生存監視（wall-clock・無出力タイムアウト・SIGTERM→SIGKILL）は
//! `subprocess.rs` の低レベル部分を再利用する。

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use task_core::{RateLimitObservation, Usage};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
use crate::claude_code::build_prompt;
use crate::delegate_file::{clear_delegate_file, forward_delegate_file};
use crate::progress;
use crate::protocol::{Evidence, ProviderFailure, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, kill_now, read_line_limited, read_tail, reap_after_terminal,
    write_result_json,
};

/// `[adapters.codex]`（config.toml, ADR-0008 D4）。
#[derive(Debug, Clone)]
pub struct CodexConfig {
    /// 起動するコマンド名／パス。既定 `"codex"`。
    pub command: String,
    /// `exec --json` の後、プロンプトの前に追加する引数（サンドボックス／承認モードの指定など。
    /// 正確なフラグ名は Phase 0 の二次情報では確定していないため、運用側で指定する。ADR-0008 D3）。
    pub extra_args: Vec<String>,
    /// モデル指定（`--model`。省略時は codex の既定モデル）。
    pub model: Option<String>,
    /// 追加の環境変数。
    pub env: Vec<(String, String)>,
    /// ADR-0043 D3（Phase 56）: `Some` なら `codex` をコンテナの中で起こす（`container::wrap`）。
    pub container: Option<crate::container::SharedPlan>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: "codex".to_string(),
            extra_args: Vec::new(),
            model: None,
            env: Vec::new(),
            container: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodexAdapter {
    config: CodexConfig,
}

impl CodexAdapter {
    pub const ID: &'static str = "codex";

    pub fn new(config: CodexConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WorkerAdapter for CodexAdapter {
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
        run_codex(&self.config, &req, run_id, &limits, sink).await
    }

    /// ADR-0025 D2: `extra`（`CODEX_HOME` を含む）を `config.env` の末尾に足した複製を返す
    /// （`claude_code::ClaudeCodeAdapter::with_env` と同じ規則: 同名キーは後勝ち）。
    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.env.extend(extra.iter().cloned());
        Some(Arc::new(CodexAdapter::new(config)))
    }

    /// ADR-0043 D3（Phase 56）: コンテナの中で `codex` を起こす複製。
    fn with_container(&self, plan: crate::container::SharedPlan) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.container = Some(plan);
        Some(Arc::new(CodexAdapter::new(config)))
    }
}

/// 壁時計の Unix 秒（ADR-0025 D3: 観測時刻は celeris の壁時計。`codex_account` からも使う）。
pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `artifacts/result.json`（`claude_code::ResultFile` と同じ規約。ADR-0006 D3, ADR-0008 D3）。
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

/// `turn.completed`/`turn.failed` の一度でも観測できた終端シグナル（ADR-0008 D3）。
#[derive(Debug, Clone)]
enum TurnSignal {
    Completed { usage: Option<Usage> },
    Failed { message: String },
}

async fn run_codex(
    config: &CodexConfig,
    req: &RunRequest,
    run_id: &str,
    limits: &RunLimits,
    sink: &dyn EventSink,
) -> Result<RunOutcome, AdapterError> {
    let run_dir = req.workspace.join("runs").join(run_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    let stdout_log_path = run_dir.join("stdout.jsonl");
    let stderr_log_path = run_dir.join("stderr.log");
    // `stderr_task` (below) moves a copy into its `async move` block; this one stays available for
    // the crash-classification read after the loop (ADR-0010 D5).
    let stderr_log_path_for_task = stderr_log_path.clone();

    // 前回の run（リトライ）が残した結果ファイルを、今回の run の結果と誤読しない（ADR-0006 D3 と同じ理由）。
    // ADR-0036 D1/D2: 置き場はディスパッチャが決めた `artifacts_dir`（共有 workspace ではタスクごと）。
    let artifacts_rel = req.artifacts_rel();
    let result_path = req.artifact_path("result.json");
    let _ = tokio::fs::remove_file(&result_path).await;
    clear_delegate_file(&req.artifacts_dir).await;

    let prompt = build_prompt(&req.task, &req.context, run_id, &artifacts_rel);
    // ADR-0023 D2 / M1: この run で何を渡したかを残す（`request.json` は構造、`prompt.txt` は実際の文面）。
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &prompt, run_id).await;

    let mut command = Command::new(&config.command);
    // CoS and standalone task workspaces need not be Git repositories.
    command
        .arg("exec")
        .arg("--json")
        .arg("--skip-git-repo-check")
        // The result.json contract needs writes; explicit extra_args override this default.
        .arg("-c")
        .arg("sandbox_mode=\"workspace-write\"");
    if let Some(model) = &config.model {
        command.arg("--model").arg(model);
    }
    // Worktrees and shared workspaces keep results outside cwd. Grant only the
    // dispatcher-selected artifact directory, not its parent or other tasks.
    tokio::fs::create_dir_all(&req.artifacts_dir).await?;
    command.arg("--add-dir").arg(&req.artifacts_dir);
    command.args(&config.extra_args);
    command.arg(&prompt);
    command
        .envs(config.env.iter().cloned())
        .current_dir(req.cwd());
    // ★ ADR-0043 D3 の差し込み点（コンテナ実行）。`None` ならそのまま（ホスト実行は変わらない）。
    let mut command = crate::container::wrap(command, config.container.as_deref());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(AdapterError::Spawn)?;
    // ADR-0044 §5 Phase 53 追記（Phase 55）: この run のプロセスグループを覚える（`kill_tree` の入口）。
    let _process_group = crate::process_group::ProcessGroup::register(run_id, child.id());

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

    let start = Instant::now();
    let mut last_activity = Instant::now();
    let mut last_signal: Option<TurnSignal> = None;
    let mut conversation_reply: Option<String> = None;
    // `{"type":"error","message":...}` を観測したら保持する（ADR-0010 D5: `turn.*` を一度も観測できずに
    // exit した場合の分類材料に使う）。
    let mut last_error_message: Option<String> = None;
    let mut force_kill = false;
    let mut timeout_terminal: Option<Terminal> = None;

    loop {
        let wall_elapsed = start.elapsed();
        if wall_elapsed >= limits.wall_clock {
            timeout_terminal = Some(Terminal::Error {
                message: "wall clock exceeded".into(),
                retryable: true,
            });
            force_kill = true;
            break;
        }
        let idle_elapsed = last_activity.elapsed();
        if idle_elapsed >= limits.idle_timeout {
            timeout_terminal = Some(Terminal::Error {
                message: "idle timeout".into(),
                retryable: true,
            });
            force_kill = true;
            break;
        }
        let wait = (limits.wall_clock - wall_elapsed).min(limits.idle_timeout - idle_elapsed);

        let outcome = match tokio::time::timeout(
            wait,
            read_line_limited(&mut reader, MAX_LINE_BYTES),
        )
        .await
        {
            Err(_elapsed) => continue,
            Ok(Err(e)) => return Err(AdapterError::Io(e)),
            Ok(Ok(outcome)) => outcome,
        };

        match outcome {
            LineOutcome::Eof => break,
            LineOutcome::TooLong => {
                sink.heartbeat();
                last_activity = Instant::now();
                warn!("run {run_id}: discarding overlong line from codex stdout");
            }
            LineOutcome::Line(bytes) => {
                sink.heartbeat();
                last_activity = Instant::now();
                stdout_file.write_all(&bytes).await?;
                stdout_file.write_all(b"\n").await?;
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        if event["type"] == "item.completed"
                            && event["item"]["type"] == "agent_message"
                            && event["item"]["phase"] != "commentary"
                        {
                            conversation_reply = event["item"]["text"]
                                .as_str()
                                .filter(|text| !text.trim().is_empty())
                                .map(str::to_owned);
                        } else if event["type"] == "item.started" {
                            conversation_reply = None;
                        }
                    }
                    handle_line(trimmed, sink, &mut last_signal, &mut last_error_message);
                }
            }
        }
    }

    let exit_status = if force_kill {
        kill_now(&mut child, limits.kill_grace).await?
    } else {
        reap_after_terminal(&mut child, limits.kill_grace).await?
    };

    if let Err(e) = stderr_task.await {
        warn!("run {run_id}: stderr capture task failed: {e}");
    }
    stdout_file.flush().await?;

    let (terminal, provider_failure): (Terminal, Option<ProviderFailure>) = match (
        timeout_terminal,
        &last_signal,
    ) {
        // タイムアウト（wall-clock / idle）は分類しない（ADR-0010 D5）。
        (Some(t), _) => (t, None),
        // `turn.completed`/`turn.failed` を一度も観測できずに exit した場合はクラッシュとして扱い、
        // artifacts/result.json を一切信用しない（ADR-0006 D4 と同じ理由。ADR-0008 D3）。
        // 観測できた `{"type":"error",...}` のメッセージ、無ければ stderr.log の末尾を分類する（ADR-0010 D5）。
        (None, None) => {
            let exit_repr = match exit_status.code() {
                Some(code) => code.to_string(),
                None => "signal".to_string(),
            };
            let mut pf = last_error_message
                .as_deref()
                .and_then(classify_provider_failure);
            if pf.is_none() {
                let tail = read_tail(&stderr_log_path, 4096).await;
                pf = classify_provider_failure(&tail);
            }
            (
                Terminal::Error {
                    message: format!(
                        "worker exited without a turn.completed/turn.failed message (exit={exit_repr})"
                    ),
                    retryable: true,
                },
                pf,
            )
        }
        (None, Some(TurnSignal::Failed { message })) => {
            let pf = classify_provider_failure(message);
            (
                Terminal::Error {
                    message: format!("codex turn failed: {message}"),
                    retryable: true,
                },
                pf,
            )
        }
        (None, Some(TurnSignal::Completed { usage })) => {
            // A conversation can answer directly; work orders still require their artifacts.
            let terminal = if exit_status.success()
                && req.task.conversation.is_some()
                && req.task.kind == task_core::TaskKind::Execute
                && matches!(tokio::fs::metadata(&result_path).await,
                    Err(ref error) if error.kind() == std::io::ErrorKind::NotFound)
                && let Some(summary) = conversation_reply
            {
                Terminal::Done {
                    summary,
                    evidence: Vec::new(),
                    usage: *usage,
                }
            } else {
                terminal_from_result(&req.artifacts_dir, &artifacts_rel, *usage).await
            };
            (terminal, None)
        }
    };

    forward_delegate_file(&req.artifacts_dir, sink).await;

    write_result_json(&run_dir, &terminal, provider_failure).await?;

    if let (Terminal::Error { message, .. }, Some(pf)) = (&terminal, provider_failure) {
        return Err(AdapterError::from_provider_failure(pf, message));
    }

    Ok(RunOutcome {
        terminal,
        exit_code: exit_status.code(),
    })
}

/// codex の JSON Lines の 1 行を解釈する。既知でない `type` や JSON として不正な行は無視する
/// （`claude_code::handle_line` と同じ方針。ADR-0008 D3）。
fn handle_line(
    line: &str,
    sink: &dyn EventSink,
    last_signal: &mut Option<TurnSignal>,
    last_error_message: &mut Option<String>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let Some(ty) = value.get("type").and_then(|t| t.as_str()) else {
        return;
    };
    if ty == "token_count"
        && let Some(obs) = RateLimitObservation::from_codex_token_count(&value, now_unix_secs())
    {
        sink.rate_limit(obs);
    }
    if ty.starts_with("item.") {
        // ADR-0048 D2（Phase 60a）: codex の `item.*` を正規化する。`msg` は従来どおり行そのもの
        // （500 バイトで切る）で、構造化フィールドを**足すだけ**。
        let fields = item_progress(ty, value.get("item"));
        sink.progress_with(&truncate(line, 500), &fields);
        return;
    }
    match ty {
        "turn.completed" => {
            let usage = value.get("usage").map(|u| Usage {
                input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()),
                output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()),
            });
            *last_signal = Some(TurnSignal::Completed { usage });
        }
        "error" => {
            if let Some(m) = value.get("message").and_then(|m| m.as_str()) {
                *last_error_message = Some(m.to_string());
            }
        }
        "turn.failed" => {
            // 実機（codex-cli 0.154.0）では `error` はオブジェクト（`{"message":"..."}`）で返る。
            // 将来のバージョンで文字列に変わっても読めるよう両方を受け付ける。
            let message = value
                .get("error")
                .map(describe_error)
                .unwrap_or_else(|| "turn.failed".to_string());
            *last_signal = Some(TurnSignal::Failed { message });
        }
        _ => {}
    }
}

/// ADR-0048 D2（Phase 60a）: codex の `item.*` イベント → 正規化した進行（ここだけがアダプタ固有）。
///
/// - 道具（`command_execution` / `mcp_tool_call` / `web_search` / `file_change` / `patch_apply`）は
///   `item.started` / `item.updated` が `tool_use`、`item.completed` が `tool_result`
///   （`exit_code != 0` は `error`）。
/// - `agent_message` は `text`、`reasoning` は `thinking`（要約だけ）。
/// - それ以外（`todo_list` や知らない item）は `status`（節目）。
fn item_progress(ty: &str, item: Option<&serde_json::Value>) -> task_core::ProgressFields {
    let Some(item) = item else {
        return progress::status();
    };
    // 実機（codex-cli 0.154）は `type`、古い版・別実装は `item_type` を使う。
    let item_type = item
        .get("type")
        .or_else(|| item.get("item_type"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    let completed = ty == "item.completed";
    match item_type {
        "agent_message" => {
            let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
            progress::text(&progress::one_line(text))
        }
        "reasoning" => {
            let text = item
                .get("summary")
                .or_else(|| item.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            progress::thinking(&progress::one_line(text))
        }
        "command_execution" | "mcp_tool_call" | "web_search" | "file_change" | "patch_apply" => {
            if completed {
                let body = item
                    .get("aggregated_output")
                    .or_else(|| item.get("output"))
                    .and_then(|o| o.as_str())
                    .unwrap_or("");
                let error = item
                    .get("exit_code")
                    .and_then(|c| c.as_i64())
                    .is_some_and(|c| c != 0)
                    || item.get("status").and_then(|s| s.as_str()) == Some("failed");
                progress::tool_result(Some(item_type), body, error)
            } else {
                let summary = item
                    .get("command")
                    .or_else(|| item.get("query"))
                    .or_else(|| item.get("path"))
                    .or_else(|| item.get("tool"))
                    .and_then(|v| v.as_str())
                    .map(progress::one_line)
                    .unwrap_or_else(|| progress::one_line(&item.to_string()));
                task_core::ProgressFields::of(task_core::ProgressKind::ToolUse)
                    .with_tool(item_type)
                    .with_summary(progress::truncate_chars(
                        &summary,
                        progress::SUMMARY_MAX_CHARS,
                    ))
                    .with_detail(item.to_string())
            }
        }
        // 知らない item は節目として残す（Console は折り畳んだ見出しに最後の `status` を出す）。
        other => {
            let summary = if other.is_empty() {
                ty.to_string()
            } else {
                format!("{ty} {other}")
            };
            progress::status().with_summary(summary)
        }
    }
}

/// `turn.failed` の `error` フィールドから人間向けの文字列を作る（文字列でもオブジェクトでも読める）。
fn describe_error(value: &serde_json::Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(msg) = value.get("message").and_then(|m| m.as_str()) {
        return msg.to_string();
    }
    value.to_string()
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

/// `turn.completed` と結果ファイルから終端を合成する。呼び出し元は `turn.completed` を一度でも
/// 観測できた場合にのみこれを呼ぶ（`claude_code::terminal_from_result` と同じ構造。ADR-0008 D3）。
async fn terminal_from_result(
    artifacts_dir: &std::path::Path,
    artifacts_rel: &str,
    usage: Option<Usage>,
) -> Terminal {
    let result_path = artifacts_dir.join("result.json");
    let text = match tokio::fs::read_to_string(&result_path).await {
        Ok(t) => t,
        Err(_) => {
            return Terminal::Error {
                message: format!("codex exited without {artifacts_rel}/result.json"),
                retryable: true,
            };
        }
    };

    match serde_json::from_str::<ResultFile>(&text) {
        Ok(rf) => {
            if let Some(question) = rf.question {
                Terminal::Question { text: question }
            } else if let Some(summary) = rf.summary {
                Terminal::Done {
                    summary,
                    evidence: lenient_evidence(rf.evidence),
                    usage,
                }
            } else {
                Terminal::Error {
                    message: format!(
                        "{artifacts_rel}/result.json has neither 'summary' nor 'question'"
                    ),
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use task_core::{ArtifactRef, DelegateTask};

    use super::*;
    use crate::protocol::{PROTOCOL_VERSION, RunContext};

    #[derive(Default)]
    struct RecordingSink {
        progress: Mutex<Vec<String>>,
        /// ADR-0048 D2（Phase 60a）: 構造化した進行（`msg` と一緒に）。
        structured: Mutex<Vec<(String, task_core::ProgressFields)>>,
        delegated: Mutex<Vec<Vec<DelegateTask>>>,
        rate_limits: Mutex<Vec<task_core::RateLimitObservation>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(msg.to_string());
        }
        fn progress_with(&self, msg: &str, fields: &task_core::ProgressFields) {
            self.progress(msg);
            self.structured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((msg.to_string(), fields.clone()));
        }
        fn artifact(&self, _artifact: &ArtifactRef) {}
        fn delegate(&self, tasks: &[DelegateTask]) {
            self.delegated
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(tasks.to_vec());
        }
        fn rate_limit(&self, obs: task_core::RateLimitObservation) {
            self.rate_limits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(obs);
        }
    }

    fn stub_codex(dir: &std::path::Path, script: &str) -> CodexConfig {
        let path = dir.join("codex_stub.sh");
        // ETXTBSY 対策（ADR-0010 D10）: テストプロセス自身が書き込み fd を持たないよう別プロセスで書く。
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        CodexConfig {
            command: path.to_string_lossy().into_owned(),
            ..CodexConfig::default()
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

    /// ADR-0048 D2（Phase 60a）: codex の `item.*` の標本（`tests/fixtures/codex-stream.jsonl`）を
    /// `handle_line` に通し、`tool_use` / `tool_result` / `text` / `thinking`、それ以外は `status` に
    /// なることを確かめる。`msg` は従来どおり行そのもの（500 バイトで切る）。
    #[test]
    fn json_events_map_to_structured_progress() {
        use task_core::ProgressKind;

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/codex-stream.jsonl"
        );
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let sink = RecordingSink::default();
        let (mut signal, mut error) = (None, None);
        for line in text.lines() {
            handle_line(line, &sink, &mut signal, &mut error);
        }
        let items = sink
            .structured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let kinds: Vec<Option<ProgressKind>> = items.iter().map(|(_, f)| f.kind).collect();
        assert_eq!(
            kinds,
            vec![
                Some(ProgressKind::Thinking),
                Some(ProgressKind::ToolUse),
                Some(ProgressKind::ToolResult),
                Some(ProgressKind::ToolResult),
                Some(ProgressKind::Text),
                Some(ProgressKind::Status),
            ],
            "{items:#?}"
        );
        assert_eq!(
            items[0].1.summary.as_deref(),
            Some("テストを回して確かめる")
        );
        assert_eq!(items[1].1.tool.as_deref(), Some("command_execution"));
        assert_eq!(
            items[1].1.summary.as_deref(),
            Some("cargo test --workspace")
        );
        assert_eq!(
            items[2].1.summary.as_deref(),
            Some("test result: ok. 812 passed")
        );
        assert!(!items[2].1.error);
        // `exit_code != 0` は失敗の印。
        assert!(items[3].1.error, "{:?}", items[3]);
        assert_eq!(items[4].1.summary.as_deref(), Some("テストは通りました。"));
        // 知らない item（`todo_list`）は節目として残る。
        assert_eq!(
            items[5].1.summary.as_deref(),
            Some("item.completed todo_list")
        );
        // `msg` は従来どおり行そのもの。
        assert!(items[1].0.contains("command_execution"), "{}", items[1].0);
        assert!(matches!(signal, Some(TurnSignal::Completed { .. })));
        assert!(error.is_none());
    }

    #[tokio::test]
    async fn happy_path_progress_and_done_from_result_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
echo '{"type":"thread.started"}'
echo '{"type":"item.started","item":{"type":"command_execution","command":"cargo test"}}'
printf '%s' '{"summary":"added usage example","evidence":[]}' > artifacts/result.json
echo '{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":20}}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-1", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done {
                summary,
                evidence,
                usage,
            } => {
                assert_eq!(summary, "added usage example");
                assert!(evidence.is_empty());
                assert_eq!(
                    usage,
                    Some(Usage {
                        input_tokens: Some(10),
                        output_tokens: Some(20)
                    })
                );
            }
            other => panic!("expected done, got {other:?}"),
        }
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m.contains("command_execution")));
        assert!(dir.path().join("runs/run-1/stdout.jsonl").is_file());

        // P-26 (ADR-0010 D10): the terminal is also normalized into `runs/<run_id>/result.json`,
        // readable by task-dispatch as a `WorkerMessage::Done`.
        let result_json =
            std::fs::read_to_string(dir.path().join("runs/run-1/result.json")).unwrap();
        match serde_json::from_str::<crate::protocol::WorkerMessage>(result_json.trim()).unwrap() {
            crate::protocol::WorkerMessage::Done { summary, .. } => {
                assert_eq!(summary, "added usage example")
            }
            other => panic!("expected done in result.json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn direct_reply_is_only_accepted_for_successful_conversations() {
        for (conversation, ending, file, done) in [
            (true, "echo '{\"type\":\"turn.completed\"}'", "", true),
            (false, "echo '{\"type\":\"turn.completed\"}'", "", false),
            (
                true,
                "echo '{\"type\":\"turn.failed\",\"error\":\"failed\"}'",
                "",
                false,
            ),
            (
                true,
                "echo '{\"type\":\"turn.completed\"}'; exit 1",
                "",
                false,
            ),
            (
                true,
                "echo '{\"type\":\"turn.completed\"}'",
                "mkdir -p artifacts; echo invalid > artifacts/result.json",
                false,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let config = stub_codex(
                dir.path(),
                &format!(
                    "{file}\necho '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"接続確認OK\"}}}}'\n{ending}"
                ),
            );
            let mut req = sample_req(dir.path().to_path_buf());
            if conversation {
                req.task.conversation = Some(task_core::MessageId::new());
            }
            let outcome = CodexAdapter::new(config)
                .run(req, "reply", default_limits(), &RecordingSink::default())
                .await
                .unwrap();
            assert_eq!(
                matches!(outcome.terminal, Terminal::Done { .. }),
                done,
                "{:?}",
                outcome.terminal
            );
        }
    }

    #[tokio::test]
    async fn worktree_can_write_results_outside_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let work_dir = dir.path().join("repos/code");
        std::fs::create_dir_all(&work_dir).unwrap();
        let config = stub_codex(
            dir.path(),
            r#"
artifact_root=''
while [ "$#" -gt 0 ]; do
    if [ "$1" = '--add-dir' ]; then shift; artifact_root="$1"; fi
    shift
done
[ -n "$artifact_root" ] && [ -d "$artifact_root" ] || exit 10
[ "$PWD" != "$artifact_root" ] || exit 11
printf '%s' '{"summary":"worktree result saved","evidence":[]}' > "$artifact_root/result.json"
echo '{"type":"turn.completed"}'
"#,
        );
        let mut req = sample_req(dir.path().to_path_buf());
        req.work_dir = Some(work_dir.clone());
        // Covers shared task-specific artifact directories as well.
        req.artifacts_dir = dir.path().join(".taskd/artifacts/task-a");
        let result_path = req.artifact_path("result.json");
        let outcome = CodexAdapter::new(config)
            .run(
                req,
                "external-artifacts",
                default_limits(),
                &RecordingSink::default(),
            )
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { summary, .. } if summary == "worktree result saved")
        );
        assert!(result_path.is_file());
        assert!(!work_dir.join("artifacts/result.json").exists());
    }

    #[tokio::test]
    async fn turn_failed_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"turn.failed","error":"sandbox denied write"}'"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-2", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("sandbox denied write"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// 実機（codex-cli 0.154.0, 2026-09-14 確認）では `turn.failed.error` はオブジェクト
    /// （`{"message":"..."}`）で返ってくる。文字列を仮定すると読み落とす回帰テスト。
    #[tokio::test]
    async fn turn_failed_with_object_shaped_error_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"turn.failed","error":{"message":"the model is not supported"}}'"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-2b", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("the model is not supported"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// `turn.failed.error.message` が供給側失敗の文言に一致すれば `AdapterError::Exhausted` として
    /// 返る（ADR-0010 D5）。result.json も書かれる。
    #[tokio::test]
    async fn turn_failed_classified_as_exhausted_surfaces_as_adapter_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"turn.failed","error":{"message":"You'"'"'ve hit your usage limit"}}'"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-2c", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::Exhausted(_)), "{err:?}");
        assert!(dir.path().join("runs/run-2c/result.json").is_file());
    }

    /// `turn.*` を一度も観測できずに exit した場合も `{"type":"error",...}` の直前の行を分類する
    /// （ADR-0010 D5）。
    #[tokio::test]
    async fn crash_with_matching_error_line_is_classified_as_provider_failure() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"error","message":"429 Too Many Requests"}'
exit 9
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-2d", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::Throttled { .. }), "{err:?}");
    }

    /// ADR-0036 D1/D2: 共有 workspace のタスクは `.taskd/artifacts/<task_id>/result.json` を読む。
    #[tokio::test]
    async fn a_shared_workspace_task_uses_its_own_artifacts_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p .taskd/artifacts/T1
printf '%s' '{"summary":"mine","evidence":[]}' > .taskd/artifacts/T1/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/result.json"),
            r#"{"summary":"sibling"}"#,
        )
        .unwrap();
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.artifacts_dir = dir.path().join(".taskd/artifacts/T1");
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-shared", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert_eq!(summary, "mine"),
            other => panic!("expected done, got {other:?}"),
        }
        let prompt =
            std::fs::read_to_string(dir.path().join("runs/run-shared/prompt.txt")).unwrap();
        assert!(
            prompt.contains(".taskd/artifacts/T1/result.json"),
            "{prompt}"
        );
    }

    #[tokio::test]
    async fn success_without_result_file_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), r#"echo '{"type":"turn.completed"}'"#);
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-3", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn question_in_result_file_blocks_task() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"question":"which crate version?"}' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-4", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Question { text } => assert_eq!(text, "which crate version?"),
            other => panic!("expected question, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn crash_without_turn_message_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), "exit 9");
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-5", default_limits(), &sink)
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, Some(9));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=9"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// クラッシュ前に `artifacts/result.json` が存在していても、`turn.completed`/`turn.failed` を
    /// 一度も観測できなければ信用しない（ADR-0006 D4 と同じ回帰、ADR-0008 D3）。
    #[tokio::test]
    async fn stale_result_file_without_turn_message_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"looks done but crashed before saying so","evidence":[]}' > artifacts/result.json
exit 9
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-6", default_limits(), &sink)
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, Some(9));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=9"), "{message}");
            }
            other => panic!("expected error (stale file must not be trusted), got {other:?}"),
        }
    }

    /// 前回の run が残した `artifacts/result.json` は、今回の run 開始時に消される（ADR-0006 D3 と同じ）。
    #[tokio::test]
    async fn stale_result_file_from_previous_run_is_cleared_before_this_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/result.json"),
            r#"{"summary":"stale from a previous attempt","evidence":[]}"#,
        )
        .unwrap();
        let config = stub_codex(dir.path(), r#"echo '{"type":"turn.completed"}'"#);
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-7", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => {
                panic!("expected error (stale file must be cleared, not reused), got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn wall_clock_exceeded_kills_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), "sleep 30");
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_millis(300),
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
    }

    #[tokio::test]
    async fn idle_timeout_kills_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"item.started","item":{"type":"agent_message"}}'
sleep 30
"#,
        );
        let adapter = CodexAdapter::new(config);
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

    #[tokio::test]
    async fn malformed_evidence_in_result_file_does_not_fail_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"all good","evidence":["cargo test: 4 passed",{"criterion":0,"command":"cargo test","exit":0,"stdout_tail":""},42]}' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-10", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done {
                summary, evidence, ..
            } => {
                assert_eq!(summary, "all good");
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].command.as_deref(), Some("cargo test"));
            }
            other => panic!("expected done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_result_file_json_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf 'not json' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-11", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("not valid JSON"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// D3 の核心契約（`exec --json`、`--model` の位置、プロンプトが最終引数）が壊れてもテストが
    /// 緑のままにならないよう、実際に渡された引数をファイルに記録して検証する（監査で指摘）。
    #[tokio::test]
    async fn command_line_has_exec_json_model_then_prompt_as_last_arg() {
        let dir = tempfile::tempdir().unwrap();
        let config = CodexConfig {
            command: {
                let path = dir.path().join("codex_stub.sh");
                // 引数は改行を含みうる（プロンプト）ので NUL 区切りで記録する。
                // ETXTBSY 対策（ADR-0010 D10）: 別プロセスで書く。
                crate::test_support::write_executable(
                    &path,
                    "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\0' \"$a\" >> \"$(dirname \"$0\")/args.log\"; done\necho '{\"type\":\"turn.completed\"}'\n",
                );
                path.to_string_lossy().into_owned()
            },
            extra_args: vec!["--sandbox".into(), "read-only".into()],
            model: Some("gpt-5-codex".into()),
            env: Vec::new(),
            container: None,
        };
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-12", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Error { .. }));

        let args_log = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
        let args: Vec<&str> = args_log.split('\0').filter(|s| !s.is_empty()).collect();
        assert_eq!(
            args.len(),
            12,
            "expected exactly one trailing prompt arg, got {args:?}"
        );
        assert_eq!(
            &args[..7],
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "-c",
                "sandbox_mode=\"workspace-write\"",
                "--model",
                "gpt-5-codex"
            ]
        );
        assert_eq!(args[7], "--add-dir");
        assert_eq!(args[8], dir.path().join("artifacts").to_str().unwrap());
        assert_eq!(&args[9..11], ["--sandbox", "read-only"]);
        let prompt = args[11];
        assert!(
            prompt.contains("# Task:"),
            "prompt should be the last arg: {prompt}"
        );
    }

    /// ADR-0016 M8: `codex` も run の終わりに `artifacts/delegate.json` があれば `sink.delegate` を 1 回呼ぶ。
    #[tokio::test]
    async fn delegate_json_written_by_worker_is_forwarded_to_sink() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"delegated two subtasks","evidence":[]}' > artifacts/result.json
printf '%s' '{"tasks":[{"title":"a","objective":"do a","acceptance":[{"text":"c","check":{"type":"human"}}]},{"title":"b","objective":"do b","acceptance":[{"text":"c","check":{"type":"human"}}]}]}' > artifacts/delegate.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-13", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let delegated = sink.delegated.lock().unwrap();
        assert_eq!(delegated.len(), 1);
        assert_eq!(delegated[0].len(), 2);
    }

    /// ADR-0025 D3: `token_count` の `rate_limits` を解析すると `sink.rate_limit` に観測値が渡る。
    #[tokio::test]
    async fn token_count_event_line_is_forwarded_to_the_sink() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
echo '{"type":"token_count","rate_limits":{"primary":{"used_percent":14.0,"window_minutes":300,"resets_in_seconds":3600},"secondary":{"used_percent":24.0,"window_minutes":10080,"resets_in_seconds":432000}}}'
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-rate-1", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let observed = sink.rate_limits.lock().unwrap();
        assert_eq!(observed.len(), 1);
        let obs = &observed[0];
        assert_eq!(obs.five_hour.map(|w| w.utilization), Some(0.14));
        assert_eq!(obs.seven_day.map(|w| w.utilization), Some(0.24));
    }

    /// ADR-0025 D2: `with_env` の追加分（`CODEX_HOME`）は既存の同名キーより後に環境を組み立てるので勝つ。
    #[tokio::test]
    async fn with_env_overrides_a_same_name_key_already_in_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("env-seen.txt");
        let mut config = CodexConfig {
            command: {
                let path = dir.path().join("codex_stub.sh");
                crate::test_support::write_executable(
                    &path,
                    &format!(
                        "#!/bin/sh\nmkdir -p artifacts\nprintf '%s' \"$CODEX_HOME\" > {out}\nprintf '%s' '{{\"summary\":\"ok\",\"evidence\":[]}}' > artifacts/result.json\necho '{{\"type\":\"turn.completed\"}}'\n",
                        out = out_file.display()
                    ),
                );
                path.to_string_lossy().into_owned()
            },
            ..CodexConfig::default()
        };
        config
            .env
            .push(("CODEX_HOME".to_string(), "old-account-dir".to_string()));
        let base = CodexAdapter::new(config);
        let with_env = base
            .with_env(&[("CODEX_HOME".to_string(), "new-account-dir".to_string())])
            .expect("codex supports with_env");

        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = with_env
            .run(req, "run-env-1", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let seen = std::fs::read_to_string(&out_file).unwrap();
        assert_eq!(seen, "new-account-dir");
    }
}
