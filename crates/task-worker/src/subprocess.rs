//! サブプロセス + JSON Lines の実行器（ADR-0003 D1/D3/D4/D5）。

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal};
use crate::protocol::{ProviderFailure, RunRequest, WorkerMessage};

/// 1 行の上限（ADR-0003 D1）。
pub(crate) const MAX_LINE_BYTES: usize = 1024 * 1024;

/// 起動するコマンド。`program` と `args` は設定からそのまま渡す。cwd は `req.workspace`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubprocessSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// ADR-0023 D2: ワーカーに渡した指示そのものを `runs/<run_id>/` に残す（後から「何を言われて何をしたか」を追える）。
/// **全アダプタから呼ぶ**（fake は `RunRequest` をそのまま stdin に渡し、claude-code / codex は
/// そこから組み立てたプロンプトを渡すので、後者は `prompt.txt` も書く。ADR-0023 M1）。
/// 秘密は入らない（`RunRequest` に `env` の値やトークンは含まれない。ADR-0013 D11）。書けなくても run は続ける。
pub(crate) async fn write_run_request(run_dir: &Path, req: &RunRequest, run_id: &str) {
    match serde_json::to_string_pretty(req) {
        Ok(pretty) => {
            if let Err(e) = tokio::fs::write(run_dir.join("request.json"), format!("{pretty}\n")).await {
                tracing::warn!(%run_id, error = %e, "could not write runs/<run_id>/request.json");
            }
        }
        Err(e) => tracing::warn!(%run_id, error = %e, "could not serialize the run request"),
    }
}

/// ADR-0023 M1: claude-code / codex が実際に渡した文面（`build_prompt` の結果）を残す。
pub(crate) async fn write_run_prompt(run_dir: &Path, prompt: &str, run_id: &str) {
    if let Err(e) = tokio::fs::write(run_dir.join("prompt.txt"), prompt).await {
        tracing::warn!(%run_id, error = %e, "could not write runs/<run_id>/prompt.txt");
    }
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
    write_run_request(&run_dir, req, run_id).await;

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .envs(spec.env.iter().cloned())
        .current_dir(req.cwd())
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
    // `error.provider_failure`（ADR-0010 D5）。終端が `error` でこれが付いていれば、result.json を
    // 書いた後に `AdapterError` として返す（ディスパッチャに requeue 判断をさせるため）。
    let mut provider_failure: Option<ProviderFailure> = None;
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
                // ADR-0010 D7: 1 行読むたび（読めなかった超過行も含む）に生存通知する。
                sink.heartbeat();
                terminal = Some(Terminal::Error {
                    message: "protocol violation: stdout line exceeds 1 MiB".into(),
                    retryable: false,
                });
                force_kill = true;
                break;
            }
            LineOutcome::Line(bytes) => {
                sink.heartbeat();
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
                        // ADR-0044 D2（Phase 53）: 残る記録。run は止まらない。
                        WorkerMessage::Comment { body } => sink.comment(&body),
                        WorkerMessage::Delegate { tasks } => sink.delegate(&tasks),
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
                        WorkerMessage::Error { message, retryable, provider_failure: pf } => {
                            terminal_raw = Some(trimmed.to_string());
                            provider_failure = pf;
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

    // ADR-0010 D5: 供給側失敗は result.json を書いた後（上記）に `AdapterError` として返す。
    // 遷移の判断はここでは行わない（ディスパッチャの責務）。
    if let (Some(Terminal::Error { message, .. }), Some(pf)) = (&terminal, provider_failure) {
        return Err(AdapterError::from_provider_failure(pf, message));
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

/// `Terminal` を `WorkerMessage` に正規化して `runs/<run_id>/result.json` に 1 行 JSON として書く
/// （ADR-0010 D10 P-26）。`run_subprocess` はワーカーから受信した生の行をそのまま書くので使わないが、
/// `claude-code`/`codex` は自前で終端を合成するのでこの共通ヘルパを使う。
pub(crate) async fn write_result_json(
    run_dir: &Path,
    terminal: &Terminal,
    provider_failure: Option<ProviderFailure>,
) -> std::io::Result<()> {
    let msg = match terminal {
        Terminal::Done { summary, evidence, usage } => WorkerMessage::Done {
            summary: summary.clone(),
            evidence: evidence.clone(),
            usage: *usage,
        },
        Terminal::Question { text } => WorkerMessage::Question { text: text.clone() },
        Terminal::Error { message, retryable } => WorkerMessage::Error {
            message: message.clone(),
            retryable: *retryable,
            provider_failure,
        },
    };
    let text = serde_json::to_string(&msg).map_err(std::io::Error::other)?;
    tokio::fs::write(run_dir.join("result.json"), format!("{text}\n")).await
}

/// ファイル末尾 `max` バイトを文字列として読む（供給側失敗の分類用。ADR-0010 D5）。
/// ファイルが無い・読めない場合は空文字列を返す（分類対象が無ければ `None` になるだけで、run の
/// 結果全体には影響させない）。
pub(crate) async fn read_tail(path: &Path, max: usize) -> String {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let start = bytes.len().saturating_sub(max);
            String::from_utf8_lossy(&bytes[start..]).into_owned()
        }
        Err(_) => String::new(),
    }
}

pub(crate) enum LineOutcome {
    Eof,
    Line(Vec<u8>),
    TooLong,
}

/// `max` バイトを超える行は `TooLong` を返す（末尾の `\n` は含まない基準）。
pub(crate) async fn read_line_limited<R>(reader: &mut R, max: usize) -> std::io::Result<LineOutcome>
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
pub(crate) fn send_signal_to_group(child: &Child, sig: Signal) {
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
pub(crate) async fn kill_now(child: &mut Child, grace: Duration) -> std::io::Result<ExitStatus> {
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
pub(crate) async fn reap_after_terminal(child: &mut Child, grace: Duration) -> std::io::Result<ExitStatus> {
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
        heartbeat_count: Mutex<u32>,
        /// ADR-0044 D2（Phase 53）: `{"type":"comment"}` の本文。
        comments: Mutex<Vec<String>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress.lock().unwrap_or_else(|e| e.into_inner()).push(msg.to_string());
        }
        fn comment(&self, body: &str) {
            self.comments.lock().unwrap_or_else(|e| e.into_inner()).push(body.to_string());
        }
        fn artifact(&self, artifact: &ArtifactRef) {
            self.artifacts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(artifact.clone());
        }
        fn heartbeat(&self) {
            *self.heartbeat_count.lock().unwrap_or_else(|e| e.into_inner()) += 1;
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
        // progress・artifact・done の 3 行分は少なくとも heartbeat が呼ばれている（ADR-0010 D7）。
        assert!(*sink.heartbeat_count.lock().unwrap() >= 3);

        let received = std::fs::read_to_string(dir.path().join("received.json")).unwrap();
        assert!(received.contains(r#""type":"run""#));

        let run_dir = dir.path().join("runs/run-1");
        assert!(run_dir.join("stdout.jsonl").is_file());
        assert!(run_dir.join("stderr.log").is_file());
        assert!(run_dir.join("result.json").is_file());

        // ADR-0023 D2: ワーカーに渡した指示そのものが残る（stdin に書いたものと同じ内容）。
        let request = std::fs::read_to_string(run_dir.join("request.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&request).expect("request.json は JSON");
        assert_eq!(parsed["type"], "run");
        assert_eq!(parsed["task"]["id"], req.task.id.to_string());
        assert!(request.contains('\n'), "人が読めるよう整形して書く");
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

    /// ADR-0044 D2（Phase 53）: `{"type":"comment","body":"…"}` は**非終端**で、シンクの
    /// `comment` に渡る（`progress` とは別の口）。run はそのまま続いて `done` で終わる。
    /// `PROTOCOL_VERSION` は上げない（追加のみ。この行を出さないワーカーはそのまま動く）。
    #[tokio::test]
    async fn a_comment_line_is_passed_to_the_sink_and_does_not_end_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec("cat >/dev/null\necho '{\"type\":\"comment\",\"body\":\"ビルドが通った\"}'\necho '{\"type\":\"progress\",\"msg\":\"next\"}'\necho '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'");
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = run_subprocess(&spec, &req, "run-comment", &default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }), "{:?}", outcome.terminal);
        assert_eq!(
            sink.comments.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            vec!["ビルドが通った".to_string()]
        );
        assert_eq!(
            sink.progress.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            vec!["next".to_string()],
            "コメントは progress には入らない"
        );
        assert_eq!(PROTOCOL_VERSION, 4, "追加のみなので版数は上げない");
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

    /// `error.provider_failure` が付いていれば、result.json を書いた上で `AdapterError` として返す
    /// （ADR-0010 D5）。heartbeat は observed 行数（progress + error）以上呼ばれる（D7）。
    #[tokio::test]
    async fn provider_failure_is_classified_and_result_json_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec(
            "cat >/dev/null; \
             echo '{\"type\":\"progress\",\"msg\":\"a\"}'; \
             echo '{\"type\":\"error\",\"message\":\"boom\",\"retryable\":true,\
             \"provider_failure\":{\"kind\":\"throttled\",\"retry_after_secs\":7}}'",
        );
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = run_subprocess(&spec, &req, "run-9", &default_limits(), &sink)
            .await
            .expect_err("expected provider failure to surface as an AdapterError");
        match err {
            AdapterError::Throttled { retry_after } => assert_eq!(retry_after, Duration::from_secs(7)),
            other => panic!("expected Throttled, got {other:?}"),
        }
        assert!(dir.path().join("runs/run-9/result.json").is_file());
        assert!(*sink.heartbeat_count.lock().unwrap() >= 2);
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
