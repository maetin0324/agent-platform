//! サブプロセス + JSON Lines の実行器（ADR-0003 D1/D3/D4/D5）。

use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal};
use crate::protocol::{RunRequest, WorkerMessage};

/// 1 行の上限（ADR-0003 D1）。
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// 起動するコマンド。`program` と `args` は設定からそのまま渡す。cwd は `req.workspace`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubprocessSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

pub async fn run_subprocess(
    spec: &SubprocessSpec,
    req: &RunRequest,
    run_id: &str,
    limits: &RunLimits,
    sink: &dyn EventSink,
) -> Result<RunOutcome, AdapterError> {
    let run_dir = req.workspace.join("runs").join(run_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    let stdout_log_path = run_dir.join("stdout.jsonl");
    let stderr_log_path = run_dir.join("stderr.log");
    let result_log_path = run_dir.join("result.json");

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .envs(spec.env.iter().cloned())
        .current_dir(&req.workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(AdapterError::Spawn)?;

    let payload = serde_json::to_string(req)?;
    if let Some(mut stdin) = child.stdin.take() {
        // 子が先に stdin を閉じて死んだ場合の書き込みエラーは無視してよい（仕様どおり）。
        let _ = stdin.write_all(payload.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        drop(stdin);
    }

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
    let mut terminal: Option<Terminal> = None;
    let mut terminal_raw: Option<String> = None;
    // true なら終端後すぐに SIGTERM→SIGKILL（タイムアウト・プロトコル違反）。
    // false なら「終端メッセージを受け取った後」の穏やかな刈り取り（仕様 6）。
    let mut force_kill = false;

    loop {
        let wall_elapsed = start.elapsed();
        if wall_elapsed >= limits.wall_clock {
            terminal = Some(Terminal::Error {
                message: "wall clock exceeded".into(),
                retryable: true,
            });
            force_kill = true;
            break;
        }
        let idle_elapsed = last_activity.elapsed();
        if idle_elapsed >= limits.idle_timeout {
            terminal = Some(Terminal::Error {
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
                terminal = Some(Terminal::Error {
                    message: "protocol violation: stdout line exceeds 1 MiB".into(),
                    retryable: false,
                });
                force_kill = true;
                break;
            }
            LineOutcome::Line(bytes) => {
                last_activity = Instant::now();
                stdout_file.write_all(&bytes).await?;
                stdout_file.write_all(b"\n").await?;

                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    warn!("run {run_id}: discarding blank line from worker stdout");
                    continue;
                }
                match serde_json::from_str::<WorkerMessage>(trimmed) {
                    Ok(msg) => match msg {
                        WorkerMessage::Progress { msg } => sink.progress(&msg),
                        WorkerMessage::Artifact { name, path, kind } => {
                            match crate::artifact::resolve(&req.workspace, &name, &path, kind.as_deref()) {
                                Ok(aref) => sink.artifact(&aref),
                                Err(e) => warn!("run {run_id}: discarding invalid artifact {name:?}: {e}"),
                            }
                        }
                        WorkerMessage::Question { text } => {
                            terminal_raw = Some(trimmed.to_string());
                            terminal = Some(Terminal::Question { text });
                        }
                        WorkerMessage::Done { summary, evidence, usage } => {
                            terminal_raw = Some(trimmed.to_string());
                            terminal = Some(Terminal::Done { summary, evidence, usage });
                        }
                        WorkerMessage::Error { message, retryable } => {
                            terminal_raw = Some(trimmed.to_string());
                            terminal = Some(Terminal::Error { message, retryable });
                        }
                    },
                    Err(parse_err) => {
                        if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
                            // JSON としては妥当だが WorkerMessage に解析できない: プロトコル違反。
                            terminal = Some(Terminal::Error {
                                message: format!("protocol violation: {parse_err}"),
                                retryable: false,
                            });
                            force_kill = true;
                        } else {
                            warn!("run {run_id}: discarding non-json line from worker stdout: {trimmed}");
                        }
                    }
                }
                if terminal.is_some() {
                    break;
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

    if let Some(raw) = &terminal_raw {
        tokio::fs::write(&result_log_path, format!("{raw}\n")).await?;
    }

    let terminal = terminal.unwrap_or_else(|| {
        let exit_repr = match exit_status.code() {
            Some(code) => code.to_string(),
            None => "signal".to_string(),
        };
        Terminal::Error {
            message: format!("worker exited without terminal message (exit={exit_repr})"),
            retryable: true,
        }
    });

    Ok(RunOutcome {
        terminal,
        exit_code: exit_status.code(),
    })
}

enum LineOutcome {
    Eof,
    Line(Vec<u8>),
    TooLong,
}

/// `max` バイトを超える行は `TooLong` を返す（末尾の `\n` は含まない基準）。
async fn read_line_limited<R>(reader: &mut R, max: usize) -> std::io::Result<LineOutcome>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::new();
    let n = reader.take(max as u64 + 1).read_until(b'\n', &mut buf).await?;
    if n == 0 {
        return Ok(LineOutcome::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        return Ok(LineOutcome::Line(buf));
    }
    if buf.len() as u64 > max as u64 {
        return Ok(LineOutcome::TooLong);
    }
    // 上限内で改行の無いまま EOF に達した最終行。
    Ok(LineOutcome::Line(buf))
}

/// プロセスグループへ signal を送る。既に居なければ (`ESRCH`) 無視する。
fn send_signal_to_group(child: &Child, sig: Signal) {
    if let Some(pid) = child.id() {
        let pgid = Pid::from_raw(pid as i32);
        if let Err(e) = signal::killpg(pgid, sig)
            && e != Errno::ESRCH
        {
            warn!("failed to send {sig:?} to worker process group {pid}: {e}");
        }
    }
}

/// SIGTERM を直ちに送り、`grace` 待って生きていれば SIGKILL する（ADR-0003 D4）。
async fn kill_now(child: &mut Child, grace: Duration) -> std::io::Result<ExitStatus> {
    send_signal_to_group(child, Signal::SIGTERM);
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(status) => status,
        Err(_elapsed) => {
            send_signal_to_group(child, Signal::SIGKILL);
            child.wait().await
        }
    }
}

/// 終端メッセージ受信後の後始末: 最大 `grace` 待ち、まだ生きていれば kill する（仕様 6）。
async fn reap_after_terminal(child: &mut Child, grace: Duration) -> std::io::Result<ExitStatus> {
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(status) => status,
        Err(_elapsed) => kill_now(child, grace).await,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use task_core::ArtifactRef;

    use super::*;
    use crate::protocol::{RunContext, PROTOCOL_VERSION};

    #[derive(Default)]
    struct RecordingSink {
        progress: Mutex<Vec<String>>,
        artifacts: Mutex<Vec<ArtifactRef>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress.lock().unwrap_or_else(|e| e.into_inner()).push(msg.to_string());
        }
        fn artifact(&self, artifact: &ArtifactRef) {
            self.artifacts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(artifact.clone());
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

    fn sh_spec(script: &str) -> SubprocessSpec {
        SubprocessSpec {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            env: vec![],
        }
    }

    #[tokio::test]
    async fn happy_path_progress_artifact_done() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec(
            "cat > received.json; \
             mkdir -p artifacts && echo hi > artifacts/out.txt; \
             echo '{\"type\":\"progress\",\"msg\":\"working\"}'; \
             echo '{\"type\":\"artifact\",\"name\":\"out\",\"path\":\"artifacts/out.txt\",\"kind\":\"txt\"}'; \
             echo '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'",
        );
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = run_subprocess(&spec, &req, "run-1", &default_limits(), &sink).await.unwrap();

        assert_eq!(outcome.exit_code, Some(0));
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert_eq!(summary, "ok"),
            other => panic!("expected done, got {other:?}"),
        }
        assert_eq!(sink.progress.lock().unwrap().len(), 1);
        let artifacts = sink.artifacts.lock().unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].name, "out");
        assert_eq!(artifacts[0].sha256.len(), 64);

        let received = std::fs::read_to_string(dir.path().join("received.json")).unwrap();
        assert!(received.contains(r#""type":"run""#));

        let run_dir = dir.path().join("runs/run-1");
        assert!(run_dir.join("stdout.jsonl").is_file());
        assert!(run_dir.join("stderr.log").is_file());
        assert!(run_dir.join("result.json").is_file());
    }

    #[tokio::test]
    async fn non_json_lines_are_ignored_and_subsequent_done_wins() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec(
            "cat >/dev/null; \
             echo 'not json at all'; \
             echo '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'",
        );
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = run_subprocess(&spec, &req, "run-2", &default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert_eq!(summary, "ok"),
            other => panic!("expected done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_type_is_protocol_violation() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec("cat >/dev/null; echo '{\"type\":\"bogus\"}'");
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = run_subprocess(&spec, &req, "run-3", &default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(!retryable);
                assert!(message.contains("protocol violation"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exit_without_terminal_message_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec("cat >/dev/null; exit 3");
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = run_subprocess(&spec, &req, "run-4", &default_limits(), &sink).await.unwrap();
        assert_eq!(outcome.exit_code, Some(3));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=3"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn idle_timeout_kills_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec("cat >/dev/null; sleep 30");
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_secs(30),
            idle_timeout: Duration::from_millis(300),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = run_subprocess(&spec, &req, "run-5", &limits, &sink).await.unwrap();
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
    async fn wall_clock_exceeded_kills_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec(
            "cat >/dev/null; \
             while true; do echo '{\"type\":\"progress\",\"msg\":\"tick\"}'; sleep 0.1; done",
        );
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_millis(300),
            idle_timeout: Duration::from_secs(30),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = run_subprocess(&spec, &req, "run-6", &limits, &sink).await.unwrap();
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
    async fn lines_after_terminal_are_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec(
            "cat >/dev/null; \
             echo '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'; \
             echo '{\"type\":\"error\",\"message\":\"late\",\"retryable\":false}'",
        );
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = run_subprocess(&spec, &req, "run-7", &default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert_eq!(summary, "ok"),
            other => panic!("expected done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn artifact_path_escaping_workspace_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec(
            "cat >/dev/null; \
             echo '{\"type\":\"artifact\",\"name\":\"bad\",\"path\":\"../x\"}'; \
             echo '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'",
        );
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = run_subprocess(&spec, &req, "run-8", &default_limits(), &sink).await.unwrap();
        assert!(sink.artifacts.lock().unwrap().is_empty());
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert_eq!(summary, "ok"),
            other => panic!("expected done, got {other:?}"),
        }
    }
}
