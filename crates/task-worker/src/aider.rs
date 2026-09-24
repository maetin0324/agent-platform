//! `aider` アダプタ（ADR-0061, Phase 104）。
//!
//! ADR-0061 のハーネス分類表の「明確で局所的な少数ファイル修正 → aider 系」の実装。`aider` CLI は
//! celeris 独自のワーカープロトコルもストリーム JSON も話さない、プレーンテキストの対話ツールなので、
//! `claude-code`/`codex`（ADR-0006/ADR-0008）と同じ「結果ファイル規約」（`artifacts/result.json`）で
//! `RunOutcome` を合成する。プロンプト組み立ては `claude_code::build_prompt` をそのまま再利用する
//! （kind 別の文面をアダプタごとに複製しない、という既存の方針を踏襲）。
//!
//! `codex.rs`/`claude_code.rs` と違い、`aider` の標準出力は 1 行 1 JSON ではなく人間向けの文章なので、
//! この実装はそれらより単純: JSON Lines の逐次解釈をせず、各行をそのまま `sink.progress` に流し、
//! プロセスの終了後に `artifacts/result.json` を読むだけである（`fake` アダプタの「起動して待つだけ」
//! に近い。生存監視〈wall-clock・無出力タイムアウト・SIGTERM→SIGKILL〉は `subprocess.rs` を再利用）。
//!
//! トークン使用量は aider 自身が終了直前に stdout へ書く `Tokens: N sent, M received.` /
//! `Cost: $X message, $Y session.` を最良努力で拾う（無ければ `None`。ADR-0061「取得可能な範囲」）。
//! 実機（`aider-chat` 0.86.2、モック OpenAI 互換エンドポイント）で書式を確認済み（`docs/PROGRESS.md`
//! Phase 104 参照）。`Cost:` 行があればそれをそのまま使い、無ければ `model` が分かるときだけ
//! `task_core::estimate_cost_usd` の静的単価表で推定する（aider 自身の実測値を優先する）。

use std::process::Stdio;
use std::sync::Arc;
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
use crate::protocol::{Evidence, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, kill_now, read_line_limited, read_tail, reap_after_terminal,
    write_result_json,
};

/// `[adapters.aider]`（config.toml, ADR-0061）。
#[derive(Debug, Clone)]
pub struct AiderConfig {
    /// 起動するコマンド。既定 `"aider"`。
    pub command: String,
    /// `--message` の前に追加する引数（例: `["--architect"]`）。
    pub extra_args: Vec<String>,
    /// モデル指定（`--model`。省略時は aider 自身の既定モデル）。
    pub model: Option<String>,
    /// 追加の環境変数（`OPENAI_API_KEY` 等）。
    pub env: Vec<(String, String)>,
    /// ADR-0043 D3（Phase 56）: `Some` なら `aider` をコンテナの中で起こす（`container::wrap`）。
    pub container: Option<crate::container::SharedPlan>,
}

impl Default for AiderConfig {
    fn default() -> Self {
        Self {
            command: "aider".to_string(),
            extra_args: Vec::new(),
            model: None,
            env: Vec::new(),
            container: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiderAdapter {
    config: AiderConfig,
}

impl AiderAdapter {
    pub const ID: &'static str = "aider";

    pub fn new(config: AiderConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WorkerAdapter for AiderAdapter {
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
        run_aider(&self.config, &req, run_id, &limits, sink).await
    }

    fn with_model(&self, model: &str) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.model = Some(model.to_owned());
        Some(Arc::new(Self::new(config)))
    }

    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.env.extend(extra.iter().cloned());
        Some(Arc::new(Self::new(config)))
    }

    fn with_container(&self, plan: crate::container::SharedPlan) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.container = Some(plan);
        Some(Arc::new(Self::new(config)))
    }
}

/// `artifacts/result.json`（`claude_code::ResultFile`/`codex::ResultFile` と同じ規約）。
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

async fn run_aider(
    config: &AiderConfig,
    req: &RunRequest,
    run_id: &str,
    limits: &RunLimits,
    sink: &dyn EventSink,
) -> Result<RunOutcome, AdapterError> {
    let run_dir = req.workspace.join("runs").join(run_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    let artifacts_rel = req.artifacts_rel();
    let result_path = req.artifact_path("result.json");
    // 前回の run（リトライ）が残した結果ファイルを今回のものと誤読しない（ADR-0006 D3 と同じ理由）。
    let _ = tokio::fs::remove_file(&result_path).await;
    clear_delegate_file(&req.artifacts_dir).await;
    tokio::fs::create_dir_all(&req.artifacts_dir).await?;

    let prompt = build_prompt(&req.task, &req.context, run_id, &artifacts_rel);
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &prompt, run_id).await;

    let mut command = Command::new(&config.command);
    command
        // 対話プロンプトを一切出さない（一度限りの `--message` 実行。ADR-0061）。
        .arg("--yes-always")
        .arg("--no-check-update")
        .arg("--no-show-model-warnings")
        // celeris のタスクライフサイクルが commit を管理する（Phase 27 の「提案 vs 結果」規約と同じ理由で、
        // アダプタが黙って git へコミットしない）。
        .arg("--no-auto-commits")
        .arg("--no-gitignore");
    if let Some(model) = &config.model {
        command.arg("--model").arg(model);
    }
    command.args(&config.extra_args);
    command.arg("--message").arg(&prompt);
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
    let _process_group = crate::process_group::ProcessGroup::register(run_id, child.id());

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AdapterError::Other("worker stdout was not piped".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AdapterError::Other("worker stderr was not piped".into()))?;

    let stdout_log_path = run_dir.join("stdout.log");
    let stderr_log_path = run_dir.join("stderr.log");
    let stderr_log_path_for_task = stderr_log_path.clone();
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
    let mut force_kill = false;
    let mut timeout_terminal: Option<Terminal> = None;
    let mut stdout_text = String::new();

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
                warn!("run {run_id}: discarding overlong line from aider stdout");
            }
            LineOutcome::Line(bytes) => {
                sink.heartbeat();
                last_activity = Instant::now();
                stdout_file.write_all(&bytes).await?;
                stdout_file.write_all(b"\n").await?;
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    stdout_text.push_str(trimmed);
                    stdout_text.push('\n');
                    // aider は 1 行 1 JSON を話さない（ADR-0061）ので、行をそのまま節目として流す。
                    sink.progress(trimmed);
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

    forward_delegate_file(&req.artifacts_dir, sink).await;

    let (terminal, provider_failure) = if let Some(t) = timeout_terminal {
        (t, None)
    } else {
        let usage = usage_with_cost(parse_aider_usage(&stdout_text), config.model.as_deref());
        let terminal = terminal_from_result(&req.artifacts_dir, &artifacts_rel, usage).await;
        let provider_failure = if matches!(terminal, Terminal::Error { .. }) {
            let tail = read_tail(&stderr_log_path, 4096).await;
            classify_provider_failure(&tail).or_else(|| classify_provider_failure(&stdout_text))
        } else {
            None
        };
        (terminal, provider_failure)
    };

    write_result_json(&run_dir, &terminal, provider_failure).await?;

    if let (Terminal::Error { message, .. }, Some(pf)) = (&terminal, provider_failure) {
        return Err(AdapterError::from_provider_failure(pf, message));
    }

    Ok(RunOutcome {
        terminal,
        exit_code: exit_status.code(),
    })
}

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
                message: format!("aider exited without {artifacts_rel}/result.json"),
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

/// aider が stdout の最後に書く `Tokens: N sent, M received.`（`--no-stream` でも `--stream` でも
/// 出す。実機確認: aider-chat 0.86.2）を拾う。桁が大きいと `1.2k` のような省略形になることがあり、
/// 誤ったトークン数を記録しないためそのときは `None` のまま無視する（取れる範囲だけ埋める、
/// という ADR-0061 の方針）。同じ行の直後に出ることがある
/// `Cost: $0.0132 message, $0.0132 session.` の `session` 側の金額も拾う。
fn parse_aider_usage(stdout: &str) -> Option<Usage> {
    let mut input_tokens = None;
    let mut output_tokens = None;
    let mut session_cost = None;
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Tokens:") {
            let mut parts = rest.trim_end_matches('.').split(',');
            input_tokens = parts.next().and_then(|p| exact_token_count(p, "sent"));
            output_tokens = parts.next().and_then(|p| exact_token_count(p, "received"));
        } else if let Some(rest) = line.strip_prefix("Cost:") {
            session_cost = dollar_amount_before(rest, "session");
        }
    }
    if input_tokens.is_none() && output_tokens.is_none() && session_cost.is_none() {
        return None;
    }
    Some(Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens: None,
        cache_creation_tokens: None,
        cost_usd: session_cost,
    })
}

/// `" 123 sent"` のような区間から、`label` を含み、省略形（`k`/`M`/小数点）でない整数だけを拾う。
fn exact_token_count(segment: &str, label: &str) -> Option<u64> {
    let segment = segment.trim();
    if !segment.contains(label) {
        return None;
    }
    let digits: String = segment.chars().take_while(|c| c.is_ascii_digit()).collect();
    let rest_after_digits = &segment[digits.len()..];
    // 数字の直後が空白のみ（そのあと label が続く）なら省略形でない整数とみなす。
    if digits.is_empty() || !rest_after_digits.trim_start().starts_with(label) {
        return None;
    }
    digits.parse().ok()
}

/// `"$0.0132 message, $0.0132 session"` のような文字列から、`label` の直前にある `$` 金額を拾う。
fn dollar_amount_before(text: &str, label: &str) -> Option<f64> {
    let idx = text.find(label)?;
    let before = &text[..idx];
    let dollar = before.rfind('$')?;
    let amount: String = before[dollar + 1..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    amount.parse().ok()
}

/// `Cost:` 行が取れなかったときだけ、`model` が分かる場合に静的単価表（`task_core::estimate_cost_usd`）
/// で埋める。aider 自身が報告した実測コストがあればそれを常に優先する（ADR-0061）。
fn usage_with_cost(usage: Option<Usage>, model: Option<&str>) -> Option<Usage> {
    usage.map(|mut u| {
        if u.cost_usd.is_none()
            && let Some(model) = model
        {
            u.cost_usd = task_core::estimate_cost_usd(model, &u);
        }
        u
    })
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
            self.progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(msg.to_string());
        }
        fn artifact(&self, _artifact: &ArtifactRef) {}
        fn delegate(&self, tasks: &[DelegateTask]) {
            self.delegated
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(tasks.to_vec());
        }
    }

    fn stub_aider(dir: &std::path::Path, script: &str) -> AiderConfig {
        let path = dir.join("aider_stub.sh");
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        AiderConfig {
            command: path.to_string_lossy().into_owned(),
            ..AiderConfig::default()
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
            wall_clock: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(5),
            kill_grace: Duration::from_millis(200),
        }
    }

    #[tokio::test]
    async fn a_successful_edit_produces_done_with_parsed_token_usage() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_aider(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary": "fixed the typo", "evidence": []}' > artifacts/result.json
echo 'Applied edit to README.md'
echo 'Tokens: 120 sent, 34 received.'
echo 'Cost: $0.0011 message, $0.0011 session.'
"#,
        );
        let adapter = AiderAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-1", default_limits(), &sink)
            .await
            .expect("run succeeds");
        match outcome.terminal {
            Terminal::Done { summary, usage, .. } => {
                assert_eq!(summary, "fixed the typo");
                let usage = usage.expect("usage parsed from aider's own token line");
                assert_eq!(usage.input_tokens, Some(120));
                assert_eq!(usage.output_tokens, Some(34));
                assert!((usage.cost_usd.unwrap() - 0.0011).abs() < 1e-9);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        assert!(
            sink.progress
                .lock()
                .unwrap()
                .iter()
                .any(|m| m.contains("Applied edit"))
        );
        assert!(dir.path().join("runs/run-1/stdout.log").is_file());
    }

    #[tokio::test]
    async fn missing_result_json_is_a_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_aider(dir.path(), "echo 'nothing to do here'\n");
        let adapter = AiderAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-2", default_limits(), &sink)
            .await
            .expect("run itself does not error (no provider-failure text)");
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("result.json"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_question_in_result_json_becomes_terminal_question() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_aider(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"question": "which file?"}' > artifacts/result.json
"#,
        );
        let adapter = AiderAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-3", default_limits(), &sink)
            .await
            .expect("run succeeds");
        assert_eq!(
            outcome.terminal,
            Terminal::Question {
                text: "which file?".into()
            }
        );
    }

    #[tokio::test]
    async fn wall_clock_timeout_kills_the_process_and_is_not_classified() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_aider(dir.path(), "sleep 30\n");
        let adapter = AiderAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let mut short_limits = default_limits();
        short_limits.wall_clock = Duration::from_millis(200);
        short_limits.idle_timeout = Duration::from_secs(5);
        let outcome = adapter
            .run(req, "run-4", short_limits, &sink)
            .await
            .expect("timeout is not a provider failure");
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert_eq!(message, "wall clock exceeded");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_aider_usage_ignores_abbreviated_token_counts() {
        let usage = parse_aider_usage("blah\nTokens: 1.2k sent, 340 received.\n");
        let usage = usage.expect("the exact 'received' count is still recovered");
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, Some(340));
    }

    #[test]
    fn parse_aider_usage_returns_none_when_nothing_matches() {
        assert_eq!(parse_aider_usage("nothing interesting here"), None);
    }

    #[test]
    fn with_model_returns_a_new_adapter_carrying_the_model() {
        let adapter = AiderAdapter::new(AiderConfig::default());
        let with_model = adapter.with_model("claude-sonnet-5").unwrap();
        assert_eq!(with_model.id(), AiderAdapter::ID);
    }

    /// 実バイナリでの動作確認（ADR-0061, Phase 104。`docs/PROGRESS.md` Phase 104 参照）。`cargo test
    /// --workspace` の既定では走らない（`aider` バイナリが要る。テストで外部ネットワークに出ない、
    /// という CLAUDE.md の方針どおり、接続先はこのテストが自分で起こすローカルの HTTP モックだけ）。
    /// `AIDER_TEST_BIN`（既定 `"aider"`）で使う実行ファイルを指定できる:
    /// `AIDER_TEST_BIN=/path/to/aider cargo test -p task-worker aider::tests::real_aider_binary_end_to_end -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn real_aider_binary_end_to_end() {
        use std::io::{Read, Write};

        let bin = std::env::var("AIDER_TEST_BIN").unwrap_or_else(|_| "aider".into());
        if std::process::Command::new(&bin)
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!(
                "skipping real_aider_binary_end_to_end: {bin} is not runnable (set AIDER_TEST_BIN)"
            );
            return;
        }

        // 最小の OpenAI 互換モック（127.0.0.1 だけを聞く。外部ネットワークには出ない）。1 リクエストだけ
        // 相手にして、`artifacts/result.json` を作る SEARCH/REPLACE diff を返す。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 8192];
            let mut received = Vec::new();
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n]);
                let Some(header_end) = received
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4)
                else {
                    continue;
                };
                let header = String::from_utf8_lossy(&received[..header_end]);
                let content_length: usize = header
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().to_string())
                    })
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                if received.len() >= header_end + content_length {
                    break;
                }
            }
            let body = br#"{"id":"mock","object":"chat.completion","created":0,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"artifacts/result.json\n```json\n<<<<<<< SEARCH\n=======\n{\"summary\": \"real aider binary smoke test\", \"evidence\": []}\n>>>>>>> REPLACE\n```\n"},"finish_reason":"stop"}],"usage":{"prompt_tokens":50,"completion_tokens":20,"total_tokens":70}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
            let _ = stream.flush();
        });

        let dir = tempfile::tempdir().unwrap();
        // aider は git リポジトリを期待する（repo-map 機能。commit はしない: `--no-auto-commits`）。
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "a@b.c"],
            vec!["config", "user.name", "test"],
        ] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(dir.path())
                .status()
                .expect("git available for the smoke test");
        }
        std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(dir.path())
            .status()
            .unwrap();

        let config = AiderConfig {
            command: bin,
            extra_args: vec!["--edit-format".into(), "diff".into(), "--no-stream".into()],
            model: Some("openai/mock-model".into()),
            env: vec![
                ("OPENAI_API_BASE".into(), format!("http://{addr}/v1")),
                ("OPENAI_API_KEY".into(), "dummy".into()),
            ],
            container: None,
        };
        let adapter = AiderAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "real-run", default_limits(), &sink)
            .await
            .expect("real aider binary run succeeds against the local mock");
        match outcome.terminal {
            Terminal::Done { summary, .. } => {
                assert_eq!(summary, "real aider binary smoke test");
            }
            other => panic!("expected Done from the real aider binary, got {other:?}"),
        }
        server.join().expect("mock server thread");
    }
}
