//! `claude-code` アダプタ（DESIGN §5.4, ADR-0003 D7, ADR-0006）。
//!
//! `claude` CLI は taskd 独自のワーカープロトコルを話さない。`--output-format stream-json` が吐く
//! Claude Code 自身のイベント（`system`/`assistant`/`user`/`result`）を読み、結果ファイル規約
//! （ADR-0006 D3: `artifacts/result.json`）と `result` メッセージ（D4）から `RunOutcome` を合成する。
//! 生存監視（wall-clock・無出力タイムアウト・SIGTERM→SIGKILL）は `subprocess.rs` の低レベル部分を再利用する。

use std::path::Path;
use std::process::Stdio;
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use task_core::{Check, Task, Usage};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
use crate::protocol::{Evidence, RunContext, RunRequest};
use crate::subprocess::{LineOutcome, MAX_LINE_BYTES, kill_now, reap_after_terminal, read_line_limited};

/// `[adapters.claude_code]`（taskd.toml, ADR-0006 D6）。
#[derive(Debug, Clone)]
pub struct ClaudeCodeConfig {
    /// 起動するコマンド名／パス。既定 `"claude"`。
    pub command: String,
    /// 末尾に追加する引数。
    pub extra_args: Vec<String>,
    /// `--permission-mode`。既定 `"bypassPermissions"`（ADR-0006 D6: taskd は許可プロンプトに応答できない）。
    pub permission_mode: String,
    /// `--model`（省略時は claude の既定モデル）。
    pub model: Option<String>,
    /// 追加の環境変数（例: `CLAUDE_CONFIG_DIR`）。
    pub env: Vec<(String, String)>,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            command: "claude".to_string(),
            extra_args: Vec::new(),
            permission_mode: "bypassPermissions".to_string(),
            model: None,
            env: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeCodeAdapter {
    config: ClaudeCodeConfig,
}

impl ClaudeCodeAdapter {
    pub const ID: &'static str = "claude-code";

    pub fn new(config: ClaudeCodeConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WorkerAdapter for ClaudeCodeAdapter {
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
        run_claude_code(&self.config, &req, run_id, &limits, sink).await
    }
}

/// タスクからワーカーへのプロンプトを組み立てる（ADR-0006 D2, 純粋関数）。`run_id` は
/// スキーマ変更を避けてプロンプト文面にのみ埋め込む（旧 P-11。ADR-0006 D2 参照）。
pub fn build_prompt(task: &Task, context: &RunContext, run_id: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Task: {}\n\n", task.title));
    out.push_str(&format!(
        "(run {run_id}, attempt {} of {})\n\n",
        task.attempts + 1,
        task.budget.max_retries + 1
    ));
    out.push_str(&format!("## Objective\n{}\n\n", task.objective));
    out.push_str("## Acceptance criteria\n");
    for (i, c) in task.acceptance.iter().enumerate() {
        let detail = match &c.check {
            Check::Command { cmd, expect_exit } => format!(
                " (a reviewer will independently re-run `{cmd}` in this directory afterwards and \
                 requires exit code {expect_exit}; your own claim of success is not trusted)"
            ),
            Check::ArtifactExists { name } => {
                format!(" (a reviewer will check that the file `artifacts/{name}` exists)")
            }
            Check::Reviewer | Check::Human => String::new(),
        };
        out.push_str(&format!("{}. {}{}\n", i, c.text, detail));
    }
    out.push('\n');
    if !context.prior_review.is_empty() {
        out.push_str("## Previous attempt's review result (this is a retry)\n");
        for pr in &context.prior_review {
            let verdict = if pr.pass { "pass" } else { "fail" };
            out.push_str(&format!("- criterion {}: {verdict} ({})\n", pr.criterion, pr.reason));
        }
        out.push('\n');
    }
    out.push_str(
        "## Instructions\n\
         Work in the current directory (it is a dedicated workspace for this task). \
         When you are done, write your result to `artifacts/result.json` (create the \
         `artifacts/` directory if it does not exist yet) as a single JSON object of the form \
         `{\"summary\": \"<what you did>\", \"evidence\": []}`. If you cannot proceed and need a \
         decision from a human, instead write `{\"question\": \"<your question>\"}` to \
         `artifacts/result.json` and stop there. This is a non-interactive run: you cannot ask a \
         question any other way, and no one will read your final chat message directly.\n",
    );
    out
}

/// `artifacts/result.json`（ADR-0006 D3）。
#[derive(Debug, Deserialize)]
struct ResultFile {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    evidence: Vec<Evidence>,
}

/// stream-json の最後に観測した `{"type":"result",...}`（ADR-0006 D4）。
#[derive(Debug, Clone)]
struct ResultMeta {
    subtype: String,
    is_error: bool,
    usage: Option<Usage>,
}

async fn run_claude_code(
    config: &ClaudeCodeConfig,
    req: &RunRequest,
    run_id: &str,
    limits: &RunLimits,
    sink: &dyn EventSink,
) -> Result<RunOutcome, AdapterError> {
    let run_dir = req.workspace.join("runs").join(run_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    let stdout_log_path = run_dir.join("stdout.jsonl");
    let stderr_log_path = run_dir.join("stderr.log");

    // 前回の run（リトライ）が残した結果ファイルを、今回の run の結果と誤読しないよう先に消す
    // （監査で指摘。ADR-0006 D3 は「この run が書いたファイル」を前提にしている）。
    let result_path = req.workspace.join("artifacts").join("result.json");
    let _ = tokio::fs::remove_file(&result_path).await;

    let prompt = build_prompt(&req.task, &req.context, run_id);

    let mut command = Command::new(&config.command);
    command
        .arg("-p")
        .arg(&prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--permission-mode")
        .arg(&config.permission_mode)
        .arg("--max-turns")
        .arg(req.task.budget.max_turns.to_string())
        .arg("--no-session-persistence");
    if let Some(model) = &config.model {
        command.arg("--model").arg(model);
    }
    command.args(&config.extra_args);
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
        match tokio::fs::File::create(&stderr_log_path).await {
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
    let mut last_result: Option<ResultMeta> = None;
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
            Err(_elapsed) => continue, // タイムアウト。ループ先頭で上限超過を検知する。
            Ok(Err(e)) => return Err(AdapterError::Io(e)),
            Ok(Ok(outcome)) => outcome,
        };

        match outcome {
            LineOutcome::Eof => break,
            LineOutcome::TooLong => {
                // claude 自身のフォーマットは taskd が定義したものではないため、寛容に無視する（ADR-0006 D5）。
                last_activity = Instant::now();
                warn!("run {run_id}: discarding overlong line from claude stdout");
            }
            LineOutcome::Line(bytes) => {
                last_activity = Instant::now();
                stdout_file.write_all(&bytes).await?;
                stdout_file.write_all(b"\n").await?;
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    handle_line(trimmed, sink, &mut last_result);
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

    let terminal = match (timeout_terminal, &last_result) {
        (Some(t), _) => t,
        // `result` メッセージを一度も観測できずに exit した場合はクラッシュとして扱い、
        // artifacts/result.json（前回の run の名残や書きかけの内容）を一切信用しない（ADR-0006 D4）。
        (None, None) => {
            let exit_repr = match exit_status.code() {
                Some(code) => code.to_string(),
                None => "signal".to_string(),
            };
            Terminal::Error {
                message: format!("worker exited without a result message (exit={exit_repr})"),
                retryable: true,
            }
        }
        (None, Some(meta)) => terminal_from_result(&req.workspace, meta).await,
    };

    Ok(RunOutcome {
        terminal,
        exit_code: exit_status.code(),
    })
}

/// stream-json の 1 行を解釈する。既知でない `type` や JSON として不正な行は無視する（ADR-0006 D5）。
fn handle_line(line: &str, sink: &dyn EventSink, last_result: &mut Option<ResultMeta>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let Some(ty) = value.get("type").and_then(|t| t.as_str()) else {
        return;
    };
    match ty {
        "assistant" => {
            if let Some(content) = value.pointer("/message/content").and_then(|c| c.as_array()) {
                for item in content {
                    match item.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                sink.progress(&truncate(text, 500));
                            }
                        }
                        Some("tool_use") => {
                            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                            let input = item
                                .get("input")
                                .map(|v| truncate(&v.to_string(), 200))
                                .unwrap_or_default();
                            sink.progress(&format!("tool_use: {name} {input}"));
                        }
                        _ => {}
                    }
                }
            }
        }
        "result" => {
            let subtype = value
                .get("subtype")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let is_error = value
                .get("is_error")
                .and_then(|b| b.as_bool())
                .unwrap_or(subtype != "success");
            let usage = value.get("usage").map(|u| Usage {
                input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()),
                output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()),
            });
            *last_result = Some(ResultMeta {
                subtype,
                is_error,
                usage,
            });
        }
        _ => {}
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

/// `result` メッセージと結果ファイルから終端を合成する（ADR-0006 D3/D4）。呼び出し元は
/// `result` メッセージを一度でも観測できた場合にのみこれを呼ぶ（観測できなかった場合は
/// クラッシュとして扱い、この関数を呼ばずに `Error` にする。ADR-0006 D4）。
async fn terminal_from_result(workspace: &Path, last_result: &ResultMeta) -> Terminal {
    if last_result.is_error || last_result.subtype != "success" {
        return Terminal::Error {
            message: format!("claude result: {}", last_result.subtype),
            retryable: true,
        };
    }

    let result_path = workspace.join("artifacts").join("result.json");
    let text = match tokio::fs::read_to_string(&result_path).await {
        Ok(t) => t,
        Err(_) => {
            return Terminal::Error {
                message: "claude exited without artifacts/result.json".to_string(),
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
                    evidence: rf.evidence,
                    usage: last_result.usage,
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
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use std::time::Duration;

    use task_core::ArtifactRef;

    use super::*;
    use crate::protocol::{PROTOCOL_VERSION, RunContext};

    #[derive(Default)]
    struct RecordingSink {
        progress: Mutex<Vec<String>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress.lock().unwrap_or_else(|e| e.into_inner()).push(msg.to_string());
        }
        fn artifact(&self, _artifact: &ArtifactRef) {}
    }

    fn stub_claude(dir: &Path, script: &str) -> ClaudeCodeConfig {
        let path = dir.join("claude_stub.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        ClaudeCodeConfig {
            command: path.to_string_lossy().into_owned(),
            ..ClaudeCodeConfig::default()
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

    #[test]
    fn build_prompt_includes_objective_criteria_and_result_file_instructions() {
        let task = crate::protocol::tests::sample_task();
        let mut context = RunContext::default();
        context.prior_review.push(crate::protocol::PriorReview {
            criterion: 0,
            pass: false,
            reason: "cargo test exit 101".into(),
        });
        let prompt = build_prompt(&task, &context, "run-xyz");
        assert!(prompt.contains(&task.objective));
        assert!(prompt.contains("cargo test exit 101"));
        assert!(prompt.contains("artifacts/result.json"));
        assert!(prompt.contains("reviewer will independently re-run"));
        assert!(prompt.contains("run-xyz"));
        assert!(prompt.contains("attempt 1 of"));
    }

    #[tokio::test]
    async fn happy_path_progress_and_done_from_result_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"working on it"}]}}'
echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}'
printf '%s' '{"summary":"added usage example","evidence":[]}' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":10,"output_tokens":20}}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
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
        assert!(progress.iter().any(|m| m == "working on it"));
        assert!(progress.iter().any(|m| m.starts_with("tool_use: Bash")));
        assert!(dir.path().join("runs/run-1/stdout.jsonl").is_file());
    }

    #[tokio::test]
    async fn success_without_result_file_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"echo '{"type":"result","subtype":"success","is_error":false}'"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-2", default_limits(), &sink).await.unwrap();
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
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"question":"which crate version?"}' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-3", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Question { text } => assert_eq!(text, "which crate version?"),
            other => panic!("expected question, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn error_subtype_wins_even_if_result_file_claims_done() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"claimed done","evidence":[]}' > artifacts/result.json
echo '{"type":"result","subtype":"error_max_turns","is_error":true}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-4", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("error_max_turns"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_result_file_json_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf 'not json' > artifacts/result.json
echo '{"type":"result","subtype":"success","is_error":false}'
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-5", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("not valid JSON"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wall_clock_exceeded_kills_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), "sleep 30");
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_millis(300),
            idle_timeout: Duration::from_secs(30),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-6", limits, &sink).await.unwrap();
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
        let config = stub_claude(
            dir.path(),
            r#"echo '{"type":"assistant","message":{"content":[{"type":"text","text":"start"}]}}'
sleep 30
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_secs(30),
            idle_timeout: Duration::from_millis(300),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-7", limits, &sink).await.unwrap();
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
    async fn crash_without_result_message_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(dir.path(), "exit 9");
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-8", default_limits(), &sink).await.unwrap();
        assert_eq!(outcome.exit_code, Some(9));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=9"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// クラッシュ前に（あるいは前回の run の名残として）`artifacts/result.json` が存在していても、
    /// `result` メッセージを一度も観測できなければ絶対に信用しない（監査で発見した不具合の回帰テスト。
    /// ADR-0006 D4）。
    #[tokio::test]
    async fn stale_result_file_without_result_message_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_claude(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"looks done but crashed before saying so","evidence":[]}' > artifacts/result.json
exit 9
"#,
        );
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-9", default_limits(), &sink).await.unwrap();
        assert_eq!(outcome.exit_code, Some(9));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=9"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// 前回の run が残した `artifacts/result.json` は、今回の run 開始時に消される
    /// （監査で発見した不具合の回帰テスト。ADR-0006 D3）。
    #[tokio::test]
    async fn stale_result_file_from_previous_run_is_cleared_before_this_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/result.json"),
            r#"{"summary":"stale from a previous attempt","evidence":[]}"#,
        )
        .unwrap();
        let config = stub_claude(dir.path(), r#"echo '{"type":"result","subtype":"success","is_error":false}'"#);
        let adapter = ClaudeCodeAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-10", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => panic!("expected error (stale file must be cleared, not reused), got {other:?}"),
        }
    }
}
