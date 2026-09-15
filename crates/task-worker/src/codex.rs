//! `codex` アダプタ（DESIGN §5.4, ADR-0008 D3）。
//!
//! `codex exec --json` は taskd 独自のワーカープロトコルを話さない。`--json` が吐く JSON Lines
//! （`thread.started` → `item.*`（進捗）→ `turn.completed`/`turn.failed`）を読み、`claude-code`
//! （ADR-0006）と同じ「結果ファイル規約」（`artifacts/result.json`）で `RunOutcome` を合成する。
//! プロンプト組み立ては `claude_code::build_prompt` をそのまま再利用する（ADR-0008 D3: kind 別の
//! 文面をアダプタごとに複製しない）。生存監視（wall-clock・無出力タイムアウト・SIGTERM→SIGKILL）は
//! `subprocess.rs` の低レベル部分を再利用する。

use std::process::Stdio;
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use task_core::Usage;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
use crate::claude_code::build_prompt;
use crate::delegate_file::{clear_delegate_file, forward_delegate_file};
use crate::protocol::{Evidence, ProviderFailure, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, kill_now, reap_after_terminal, read_line_limited, read_tail, write_result_json,
};

/// `[adapters.codex]`（taskd.toml, ADR-0008 D4）。
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
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: "codex".to_string(),
            extra_args: Vec::new(),
            model: None,
            env: Vec::new(),
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
    let result_path = req.workspace.join("artifacts").join("result.json");
    let _ = tokio::fs::remove_file(&result_path).await;
    clear_delegate_file(&req.workspace).await;

    let prompt = build_prompt(&req.task, &req.context, run_id);

    let mut command = Command::new(&config.command);
    command.arg("exec").arg("--json");
    if let Some(model) = &config.model {
        command.arg("--model").arg(model);
    }
    command.args(&config.extra_args);
    command.arg(&prompt);
    command
        .envs(config.env.iter().cloned())
        .current_dir(&req.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(AdapterError::Spawn)?;

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

        let outcome = match tokio::time::timeout(wait, read_line_limited(&mut reader, MAX_LINE_BYTES)).await {
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

    let (terminal, provider_failure): (Terminal, Option<ProviderFailure>) = match (timeout_terminal, &last_signal) {
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
            let mut pf = last_error_message.as_deref().and_then(classify_provider_failure);
            if pf.is_none() {
                let tail = read_tail(&stderr_log_path, 4096).await;
                pf = classify_provider_failure(&tail);
            }
            (
                Terminal::Error {
                    message: format!("worker exited without a turn.completed/turn.failed message (exit={exit_repr})"),
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
        (None, Some(TurnSignal::Completed { usage })) => (terminal_from_result(&req.workspace, *usage).await, None),
    };

    forward_delegate_file(&req.workspace, sink).await;

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
    if ty.starts_with("item.") {
        sink.progress(&truncate(line, 500));
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
            let message = value.get("error").map(describe_error).unwrap_or_else(|| "turn.failed".to_string());
            *last_signal = Some(TurnSignal::Failed { message });
        }
        _ => {}
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
async fn terminal_from_result(workspace: &std::path::Path, usage: Option<Usage>) -> Terminal {
    let result_path = workspace.join("artifacts").join("result.json");
    let text = match tokio::fs::read_to_string(&result_path).await {
        Ok(t) => t,
        Err(_) => {
            return Terminal::Error {
                message: "codex exited without artifacts/result.json".to_string(),
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
                    message: "artifacts/result.json has neither 'summary' nor 'question'".into(),
                    retryable: true,
                }
            }
        }
        Err(e) => Terminal::Error {
            message: format!("artifacts/result.json is not valid JSON: {e}"),
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
        delegated: Mutex<Vec<Vec<DelegateTask>>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress.lock().unwrap_or_else(|e| e.into_inner()).push(msg.to_string());
        }
        fn artifact(&self, _artifact: &ArtifactRef) {}
        fn delegate(&self, tasks: &[DelegateTask]) {
            self.delegated.lock().unwrap_or_else(|e| e.into_inner()).push(tasks.to_vec());
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
            workspace,
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
        let outcome = adapter.run(req, "run-1", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, evidence, usage } => {
                assert_eq!(summary, "added usage example");
                assert!(evidence.is_empty());
                assert_eq!(usage, Some(Usage { input_tokens: Some(10), output_tokens: Some(20) }));
            }
            other => panic!("expected done, got {other:?}"),
        }
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m.contains("command_execution")));
        assert!(dir.path().join("runs/run-1/stdout.jsonl").is_file());

        // P-26 (ADR-0010 D10): the terminal is also normalized into `runs/<run_id>/result.json`,
        // readable by task-dispatch as a `WorkerMessage::Done`.
        let result_json = std::fs::read_to_string(dir.path().join("runs/run-1/result.json")).unwrap();
        match serde_json::from_str::<crate::protocol::WorkerMessage>(result_json.trim()).unwrap() {
            crate::protocol::WorkerMessage::Done { summary, .. } => assert_eq!(summary, "added usage example"),
            other => panic!("expected done in result.json, got {other:?}"),
        }
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
        let outcome = adapter.run(req, "run-2", default_limits(), &sink).await.unwrap();
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
        let outcome = adapter.run(req, "run-2b", default_limits(), &sink).await.unwrap();
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

    #[tokio::test]
    async fn success_without_result_file_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), r#"echo '{"type":"turn.completed"}'"#);
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-3", default_limits(), &sink).await.unwrap();
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
        let outcome = adapter.run(req, "run-4", default_limits(), &sink).await.unwrap();
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
        let outcome = adapter.run(req, "run-5", default_limits(), &sink).await.unwrap();
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
        let outcome = adapter.run(req, "run-6", default_limits(), &sink).await.unwrap();
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
        let outcome = adapter.run(req, "run-7", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => panic!("expected error (stale file must be cleared, not reused), got {other:?}"),
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
        let outcome = adapter.run(req, "run-10", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, evidence, .. } => {
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
        let outcome = adapter.run(req, "run-11", default_limits(), &sink).await.unwrap();
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
        };
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-12", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Error { .. }));

        let args_log = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
        let args: Vec<&str> = args_log.split('\0').filter(|s| !s.is_empty()).collect();
        assert_eq!(args.len(), 7, "expected exactly one trailing prompt arg, got {args:?}");
        assert_eq!(&args[..4], ["exec", "--json", "--model", "gpt-5-codex"]);
        assert_eq!(&args[4..6], ["--sandbox", "read-only"]);
        let prompt = args[6];
        assert!(prompt.contains("# Task:"), "prompt should be the last arg: {prompt}");
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
        let outcome = adapter.run(req, "run-13", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let delegated = sink.delegated.lock().unwrap();
        assert_eq!(delegated.len(), 1);
        assert_eq!(delegated[0].len(), 2);
    }
}
