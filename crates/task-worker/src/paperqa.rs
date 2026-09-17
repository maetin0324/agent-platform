//! `paperqa` アダプタ（DESIGN §5.4, ADR-0027 D3）。
//!
//! PaperQA2（`pqa` CLI）は taskd のワーカープロトコルもストリーム型の進捗形式も話さない、
//! ただの調査エンジンである。**アダプタ自身が** ADR-0006 D3 の結果ファイル規約（`artifacts/result.json`）を
//! 代わりに書き、`Terminal::Done`/`Terminal::Error` を合成する。委譲（`delegate.json`）は扱わない
//! （ADR-0027 D3: 「委譲はしない」）。生存監視（wall-clock・無出力タイムアウト・SIGTERM→SIGKILL）は
//! `subprocess.rs` の低レベル部分を再利用する。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use task_core::Task;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
use crate::protocol::{Answer, RunContext, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, kill_now, reap_after_terminal, read_line_limited, read_tail, write_result_json,
};

/// `artifacts/result.json` の `summary` の上限（ADR-0027 D3）。
const SUMMARY_MAX_CHARS: usize = 1500;
/// `progress` に転送する 1 行あたりの上限（他アダプタと同じ規則。ADR-0026 の `truncate` を踏襲）。
const PROGRESS_LINE_MAX_CHARS: usize = 500;

/// `[adapters.paperqa]`（taskd.toml, ADR-0027 D3）。`[[providers]] adapter = "paperqa"` の行ごとに
/// `settings` / `env` / `model` を上書きできる（ADR-0026 D2 と同じ作り）。
#[derive(Debug, Clone)]
pub struct PaperQaConfig {
    /// 起動するコマンド名／パス。既定 `"pqa"`。
    pub command: String,
    /// `-s <name>`（拡張子は付けない。`pqa` 自身が `.json` を足す。ADR-0027 の実機の仕様）。未指定なら渡さない。
    pub settings: Option<String>,
    /// `--agent.index.paper_directory`。未指定ならワークスペース相対 `papers`。
    pub paper_directory: Option<PathBuf>,
    /// `--agent.index.index_directory` の親ディレクトリ（実際に渡す値はこの下にタスクごとのサブディレクトリを
    /// 足したもの。ADR-0027 D3: 「index_directory の下にタスクごとの索引を作る」）。未指定ならワークスペース相対 `index`。
    pub index_directory: Option<PathBuf>,
    /// `--agent.index.name`。未指定ならタスク ID を使う。
    pub index_name: Option<String>,
    /// `--llm`。設定されているときだけ渡す（PaperQA の設定ファイルの値より優先。ADR-0027 D3）。
    pub model: Option<String>,
    /// 追加の環境変数（例: `OPENAI_API_KEY` / `OPENAI_BASE_URL`。LiteLLM 経由の OpenAI 互換エンドポイント向け）。
    pub env: Vec<(String, String)>,
    /// 末尾に追加する引数（`ask` の前に挿入する）。
    pub extra_args: Vec<String>,
}

impl Default for PaperQaConfig {
    fn default() -> Self {
        Self {
            command: "pqa".to_string(),
            settings: None,
            paper_directory: None,
            index_directory: None,
            index_name: None,
            model: None,
            env: Vec::new(),
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PaperQaAdapter {
    config: PaperQaConfig,
}

impl PaperQaAdapter {
    pub const ID: &'static str = "paperqa";

    pub fn new(config: PaperQaConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WorkerAdapter for PaperQaAdapter {
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
        run_paperqa(&self.config, &req, run_id, &limits, sink).await
    }

    /// 他のアダプタ（`claude_code`/`codex`）と同じ規則: `extra` は `config.env` の末尾に足すので、
    /// 同名キーは `extra` が勝つ。
    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.env.extend(extra.iter().cloned());
        Some(Arc::new(PaperQaAdapter::new(config)))
    }
}

/// タスクの目的から `pqa ask` に渡す問いを組み立てる（ADR-0027 D3）。`claude_code::build_prompt` は
/// コーディング用の文面（受け入れ条件・`artifacts/result.json` の書式指示・委譲の案内）なので流用せず、
/// 素の目的 + 役割の指示文 + 人間の回答履歴だけを使う（PaperQA2 はワーカープロトコルを話さない調査エンジン
/// であり、結果ファイルの書式やコマンド再実行の話をしても意味がないため）。`request.json`/`prompt.txt` は
/// 他のアダプタと同じ共有ヘルパ（`subprocess::write_run_request`/`write_run_prompt`）で残す。
pub fn build_question(task: &Task, context: &RunContext) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", task.title));
    if let Some(role) = &context.role {
        out.push_str(&format!("## Role: {}\n", role.id));
        if !role.instructions.is_empty() {
            out.push_str(&role.instructions);
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&task.objective);
    out.push('\n');
    if !context.answers.is_empty() {
        out.push_str("\n## Answers from a human to earlier questions\n");
        for Answer { question, answer } in &context.answers {
            out.push_str(&format!("- Q: {question}\n  A: {answer}\n"));
        }
    }
    out
}

async fn run_paperqa(
    config: &PaperQaConfig,
    req: &RunRequest,
    run_id: &str,
    limits: &RunLimits,
    sink: &dyn EventSink,
) -> Result<RunOutcome, AdapterError> {
    let run_dir = req.workspace.join("runs").join(run_id);
    tokio::fs::create_dir_all(&run_dir).await?;
    let stdout_log_path = run_dir.join("stdout.log");
    let stderr_log_path = run_dir.join("stderr.log");
    let stderr_log_path_for_task = stderr_log_path.clone();

    // 前回の run（リトライ）の名残を今回の結果と誤読しない（claude_code/codex と同じ理由。ADR-0006 D3）。
    let artifacts_dir = req.workspace.join("artifacts");
    let _ = tokio::fs::remove_file(artifacts_dir.join("result.json")).await;
    let _ = tokio::fs::remove_file(artifacts_dir.join("answer.md")).await;

    let question = build_question(&req.task, &req.context);
    // ADR-0023 D2 / M1: この run で何を渡したかを残す。
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &question, run_id).await;

    let task_id = req.task.id.to_string();
    let paper_directory = config.paper_directory.clone().unwrap_or_else(|| PathBuf::from("papers"));
    // ADR-0027 D3: 索引はタスクごと（同時実行で索引を壊さないため。run ごとではなくタスクごとにする
    // ことで、同じタスクのリトライ間で索引を使い回せる）。
    let index_directory = config
        .index_directory
        .clone()
        .unwrap_or_else(|| PathBuf::from("index"))
        .join(&task_id);
    let index_name = config.index_name.clone().unwrap_or_else(|| task_id.clone());

    let mut command = Command::new(&config.command);
    if let Some(settings) = &config.settings {
        command.arg("-s").arg(settings);
    }
    command
        .arg("--agent.index.paper_directory")
        .arg(&paper_directory)
        .arg("--agent.index.index_directory")
        .arg(&index_directory)
        .arg("--agent.index.name")
        .arg(&index_name);
    if let Some(model) = &config.model {
        command.arg("--llm").arg(model);
    }
    command.args(&config.extra_args);
    command.arg("ask").arg(&question);
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
    let mut stdout_buf = String::new();
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
                // pqa 自身のフォーマットは taskd が定義したものではないので寛容に無視する（claude_code と同じ考え方）。
                sink.heartbeat();
                last_activity = Instant::now();
                warn!("run {run_id}: discarding overlong line from pqa stdout");
            }
            LineOutcome::Line(bytes) => {
                sink.heartbeat();
                last_activity = Instant::now();
                stdout_file.write_all(&bytes).await?;
                stdout_file.write_all(b"\n").await?;
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim();
                stdout_buf.push_str(&text);
                stdout_buf.push('\n');
                if !trimmed.is_empty() {
                    // PaperQA2 は検索・要約の進捗を出す（ADR-0027 D3 手順 3）。行単位でそのまま progress に写す。
                    sink.progress(&truncate_chars(trimmed, PROGRESS_LINE_MAX_CHARS));
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

    if let Some(terminal) = timeout_terminal {
        // タイムアウトは供給側失敗として分類しない（他アダプタと同じ。ADR-0010 D5）。
        write_result_json(&run_dir, &terminal, None).await?;
        return Ok(RunOutcome {
            terminal,
            exit_code: exit_status.code(),
        });
    }

    let stderr_tail = read_tail(&stderr_log_path, 4096).await;
    let answer = extract_answer(&stdout_buf);

    let (terminal, provider_failure) = if !exit_status.success() {
        let exit_repr = match exit_status.code() {
            Some(code) => code.to_string(),
            None => "signal".to_string(),
        };
        let classify_text = format!("{stdout_buf}\n{stderr_tail}");
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: format!("pqa exited with a non-zero status (exit={exit_repr})"),
                retryable: true,
            },
            pf,
        )
    } else if answer.trim().is_empty() {
        let classify_text = format!("{stdout_buf}\n{stderr_tail}");
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: "pqa produced no answer".to_string(),
                retryable: true,
            },
            pf,
        )
    } else {
        // ADR-0027 D3 手順 4: アダプタが `artifacts/answer.md` と `artifacts/result.json` を書く。
        if let Err(e) = tokio::fs::create_dir_all(&artifacts_dir).await {
            warn!("run {run_id}: could not create artifacts/ directory: {e}");
        }
        if let Err(e) = tokio::fs::write(artifacts_dir.join("answer.md"), &answer).await {
            warn!("run {run_id}: could not write artifacts/answer.md: {e}");
        }
        // 書いたものは taskd にも知らせる（run の成果物一覧と `Check::ArtifactExists` の解決に使われる）。
        // 他のアダプタではワーカー自身が `artifact` メッセージで申告するが、pqa は申告しないのでアダプタが行う。
        match crate::artifact::resolve(&req.workspace, "answer.md", "artifacts/answer.md", Some("markdown")) {
            Ok(artifact) => sink.artifact(&artifact),
            Err(e) => warn!("run {run_id}: could not register artifacts/answer.md: {e}"),
        }
        let summary = single_line_summary(&answer, SUMMARY_MAX_CHARS);
        let result_file = serde_json::json!({ "summary": summary, "evidence": [] });
        match serde_json::to_string_pretty(&result_file) {
            Ok(text) => {
                if let Err(e) = tokio::fs::write(artifacts_dir.join("result.json"), format!("{text}\n")).await {
                    warn!("run {run_id}: could not write artifacts/result.json: {e}");
                }
            }
            Err(e) => warn!("run {run_id}: could not serialize artifacts/result.json: {e}"),
        }
        (
            Terminal::Done {
                summary,
                evidence: Vec::new(),
                usage: None,
            },
            None,
        )
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

/// `pqa ask` の標準出力から回答部分を切り出す。`Answer:` で始まる行が見つかればそこから末尾まで、
/// 見つからなければ標準出力全体を返す（ADR-0027 D3: 「回答本文と引用」、見つからない場合は全体）。
fn extract_answer(stdout: &str) -> String {
    // 実機（pqa 2026.8.12）の出力は rich で整形されていて、各行が `[04:38:30] ` のような時刻と
    // 折り返し用の左詰めと色コードを含む。回答の始まりは `Answer:` の行。
    let clean: Vec<String> = stdout.lines().map(strip_ansi).collect();
    let marker = clean
        .iter()
        .position(|l| strip_log_prefix(l).trim_start().to_ascii_lowercase().starts_with("answer:"));
    let Some(idx) = marker else {
        return stdout.trim_end_matches('\n').to_string();
    };
    let mut out: Vec<String> = Vec::new();
    for (i, line) in clean[idx..].iter().enumerate() {
        let body = strip_log_prefix(line);
        let body = if i == 0 {
            // 先頭行は `Answer:` を落として本文だけにする。
            match body.trim_start().split_once(':') {
                Some((_, rest)) => rest,
                None => body,
            }
        } else {
            body
        };
        out.push(body.trim_end().to_string());
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    while out.first().is_some_and(|l| l.trim().is_empty()) {
        out.remove(0);
    }
    // 折り返しの左詰め（行頭の共通の空白）を落とす。
    let indent = out
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    out.iter().map(|l| if l.len() >= indent { l[indent..].to_string() } else { l.clone() }).collect::<Vec<_>>().join("\n")
}

/// `[04:38:30] ` のような時刻の前置きを落とす（無ければそのまま）。
fn strip_log_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('[') {
        return line;
    }
    let Some(close) = trimmed.find(']') else { return line };
    let inside = &trimmed[1..close];
    let looks_like_time = inside.len() == 8
        && inside.as_bytes().iter().enumerate().all(|(i, b)| if i == 2 || i == 5 { *b == b':' } else { b.is_ascii_digit() });
    if looks_like_time {
        let rest = &trimmed[close + 1..];
        // 時刻の分だけ左詰めを保つ（折り返し行と桁を揃えるため、先頭 1 つの空白だけ落とす）。
        rest.strip_prefix(' ').unwrap_or(rest)
    } else {
        line
    }
}

/// ANSI のエスケープ（色・カーソル制御）を落とす。
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // CSI / OSC を読み飛ばす。
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(']') => {
                for next in chars.by_ref() {
                    if next == '\u{7}' || next == '\u{1b}' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// `artifacts/result.json` の `summary`: 改行・連続空白を単一の空白にたたみ（single-line-safe）、
/// 文字数で上限まで切り詰める。
fn single_line_summary(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&collapsed, max_chars)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;

    use task_core::ArtifactRef;

    use super::*;
    use crate::protocol::PROTOCOL_VERSION;

    #[derive(Default)]
    struct RecordingSink {
        progress: Mutex<Vec<String>>,
        heartbeat_count: Mutex<u32>,
        artifacts: Mutex<Vec<ArtifactRef>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress.lock().unwrap_or_else(|e| e.into_inner()).push(msg.to_string());
        }
        fn artifact(&self, artifact: &ArtifactRef) {
            self.artifacts.lock().unwrap_or_else(|e| e.into_inner()).push(artifact.clone());
        }
        fn heartbeat(&self) {
            *self.heartbeat_count.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        }
    }

    fn stub_pqa(dir: &Path, script: &str) -> PaperQaConfig {
        let path = dir.join("pqa_stub.sh");
        // ETXTBSY 対策（ADR-0010 D10）: 他アダプタのテストと同じ理由で別プロセスに書かせる。
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        PaperQaConfig {
            command: path.to_string_lossy().into_owned(),
            ..PaperQaConfig::default()
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
    async fn happy_path_progress_answer_and_result_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(
            dir.path(),
            r#"cat >/dev/null
echo 'Searching for relevant papers...'
echo 'Gathering evidence from 3 sources...'
echo 'Answer: PaperQA2 finds no evidence of prior work on X [Doe2020, Roe2021].'
"#,
        );
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req.clone(), "run-1", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, evidence, usage } => {
                assert!(summary.contains("PaperQA2 finds no evidence"), "{summary}");
                assert!(evidence.is_empty());
                assert!(usage.is_none());
            }
            other => panic!("expected done, got {other:?}"),
        }
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m.contains("Searching for relevant papers")));
        assert!(progress.iter().any(|m| m.contains("Gathering evidence")));
        assert!(*sink.heartbeat_count.lock().unwrap() >= 3);

        // 成果物として申告される（run の一覧と Check::ArtifactExists の解決に使われる）。
        let artifacts = sink.artifacts.lock().unwrap();
        assert_eq!(artifacts.len(), 1, "{artifacts:?}");
        assert_eq!(artifacts[0].name, "answer.md");
        assert_eq!(artifacts[0].path, "artifacts/answer.md");
        assert!(!artifacts[0].sha256.is_empty());
        drop(artifacts);

        let answer_md = std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap();
        // `Answer:` の見出しは落とし、本文だけを残す（実機の出力は時刻と左詰めが付くため）。
        assert!(answer_md.starts_with("PaperQA2 finds no evidence"), "{answer_md}");
        assert!(!answer_md.contains("Gathering evidence"), "進捗のログは含めない: {answer_md}");

        let result_json = std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result_json).unwrap();
        assert!(parsed["summary"].as_str().unwrap().contains("PaperQA2 finds no evidence"));
        assert_eq!(parsed["evidence"], serde_json::json!([]));

        assert!(dir.path().join("runs/run-1/stdout.log").is_file());
        assert!(dir.path().join("runs/run-1/stderr.log").is_file());
        let run_result = std::fs::read_to_string(dir.path().join("runs/run-1/result.json")).unwrap();
        match serde_json::from_str::<crate::protocol::WorkerMessage>(run_result.trim()).unwrap() {
            crate::protocol::WorkerMessage::Done { summary, .. } => {
                assert!(summary.contains("PaperQA2 finds no evidence"))
            }
            other => panic!("expected done in runs/<run_id>/result.json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_zero_exit_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(dir.path(), "cat >/dev/null; echo 'boom' 1>&2; exit 7");
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-2", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=7"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(!dir.path().join("artifacts/result.json").exists());
    }

    #[tokio::test]
    async fn empty_output_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(dir.path(), "cat >/dev/null");
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-3", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("no answer"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(!dir.path().join("artifacts/result.json").exists());
    }

    /// 壁時計の超過でプロセスグループごと SIGKILL する（acp.rs と同じ確認方法: `/proc/<pid>` の消滅）。
    #[tokio::test]
    async fn wall_clock_exceeded_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid.txt");
        let config = stub_pqa(
            dir.path(),
            &format!(
                r#"cat >/dev/null
echo $$ > {pid}
while true; do sleep 0.1; done
"#,
                pid = pid_file.display()
            ),
        );
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_millis(300),
            idle_timeout: Duration::from_secs(30),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-4", limits, &sink).await.unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("wall clock exceeded"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        let pid_text = std::fs::read_to_string(&pid_file).expect("stub should have recorded its pid before looping");
        let pid: i32 = pid_text.trim().parse().expect("pid.txt should contain a pid");
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists(), "process {pid} should have been killed");
    }

    fn read_argv(path: &Path) -> Vec<String> {
        let bytes = std::fs::read(path).unwrap();
        bytes
            .split(|b| *b == 0)
            .filter(|chunk| !chunk.is_empty())
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect()
    }

    /// settings / paper_directory / index_directory (タスク ID を足したもの) / index_name の組み立てを
    /// argv でそのまま確認する（ADR-0027 D3）。
    #[tokio::test]
    async fn settings_and_index_args_are_composed_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_pqa(
            dir.path(),
            "for a in \"$@\"; do printf '%s\\0' \"$a\" >> \"$(dirname \"$0\")/args.log\"; done\n\
             cat >/dev/null\n\
             echo 'Answer: ok'\n",
        );
        config.settings = Some("/settings/qwen-local".to_string());
        config.paper_directory = Some(PathBuf::from("/papers"));
        config.index_directory = Some(PathBuf::from("/index"));
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let task_id = req.task.id.to_string();
        let sink = RecordingSink::default();
        let outcome = adapter.run(req.clone(), "run-5", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));

        let argv = read_argv(&dir.path().join("args.log"));
        assert_eq!(
            argv,
            vec![
                "-s".to_string(),
                "/settings/qwen-local".to_string(),
                "--agent.index.paper_directory".to_string(),
                "/papers".to_string(),
                "--agent.index.index_directory".to_string(),
                format!("/index/{task_id}"),
                "--agent.index.name".to_string(),
                task_id,
                "ask".to_string(),
                build_question(&req.task, &req.context),
            ]
        );
    }

    /// 設定を省略したときの既定値: `-s` は付かず、`paper_directory`/`index_directory`/`index_name` は
    /// ワークスペース相対・タスク ID の既定値になる。
    #[tokio::test]
    async fn defaults_are_used_when_settings_and_index_config_are_absent() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(
            dir.path(),
            "for a in \"$@\"; do printf '%s\\0' \"$a\" >> \"$(dirname \"$0\")/args.log\"; done\n\
             cat >/dev/null\n\
             echo 'Answer: ok'\n",
        );
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let task_id = req.task.id.to_string();
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-6", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));

        let argv = read_argv(&dir.path().join("args.log"));
        assert!(!argv.contains(&"-s".to_string()));
        assert_eq!(argv[0], "--agent.index.paper_directory");
        assert_eq!(argv[1], "papers");
        assert_eq!(argv[2], "--agent.index.index_directory");
        assert_eq!(argv[3], format!("index/{task_id}"));
        assert_eq!(argv[4], "--agent.index.name");
        assert_eq!(argv[5], task_id);
        assert_eq!(argv[6], "ask");
    }

    /// `--llm` はモデルが設定されているときだけ渡す。
    #[tokio::test]
    async fn llm_flag_is_passed_only_when_model_is_set() {
        let dir = tempfile::tempdir().unwrap();

        // model なし
        let config_without = stub_pqa(
            dir.path(),
            "for a in \"$@\"; do printf '%s\\0' \"$a\" >> \"$(dirname \"$0\")/args-without.log\"; done\n\
             cat >/dev/null\n\
             echo 'Answer: ok'\n",
        );
        let adapter = PaperQaAdapter::new(config_without);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        adapter.run(req, "run-7a", default_limits(), &sink).await.unwrap();
        let argv_without = read_argv(&dir.path().join("args-without.log"));
        assert!(!argv_without.contains(&"--llm".to_string()));

        // model あり
        let dir2 = tempfile::tempdir().unwrap();
        let mut config_with = stub_pqa(
            dir2.path(),
            "for a in \"$@\"; do printf '%s\\0' \"$a\" >> \"$(dirname \"$0\")/args-with.log\"; done\n\
             cat >/dev/null\n\
             echo 'Answer: ok'\n",
        );
        config_with.model = Some("qwen3.8-27b".to_string());
        let adapter2 = PaperQaAdapter::new(config_with);
        let req2 = sample_req(dir2.path().to_path_buf());
        let sink2 = RecordingSink::default();
        adapter2.run(req2, "run-7b", default_limits(), &sink2).await.unwrap();
        let argv_with = read_argv(&dir2.path().join("args-with.log"));
        let llm_idx = argv_with.iter().position(|a| a == "--llm").expect("--llm should be present");
        assert_eq!(argv_with[llm_idx + 1], "qwen3.8-27b");
    }

    /// `with_env` の追加分は既存の同名キーより後に環境を組み立てるので勝つ（claude_code/codex と同じ規則）。
    #[tokio::test]
    async fn with_env_overrides_a_same_name_key_already_in_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("env-seen.txt");
        let mut config = stub_pqa(
            dir.path(),
            &format!(
                "cat >/dev/null\nprintf '%s' \"$OPENAI_BASE_URL\" > {out}\necho 'Answer: ok'\n",
                out = out_file.display()
            ),
        );
        config.env.push(("OPENAI_BASE_URL".to_string(), "http://old:1".to_string()));
        let base = PaperQaAdapter::new(config);
        let with_env = base
            .with_env(&[("OPENAI_BASE_URL".to_string(), "http://new:2".to_string())])
            .expect("paperqa supports with_env");

        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = with_env.run(req, "run-8", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let seen = std::fs::read_to_string(&out_file).unwrap();
        assert_eq!(seen, "http://new:2");
    }

    /// LLM 供給側のエラー文面（LiteLLM の認証失敗）が `AdapterError::AuthFailed` として分類される（ADR-0010 D5）。
    #[tokio::test]
    async fn llm_auth_failure_is_classified_as_adapter_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(
            dir.path(),
            "cat >/dev/null\n\
             echo 'litellm.AuthenticationError: Invalid API key provided' 1>&2\n\
             exit 1\n",
        );
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-9", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::AuthFailed(_)), "{err:?}");
        assert!(dir.path().join("runs/run-9/result.json").is_file());
    }

    /// 要約は空白をたたんで単一行にし、上限文字数で切り詰める。
    #[test]
    fn single_line_summary_collapses_whitespace_and_truncates() {
        let long_answer = format!("Answer: {}", "word ".repeat(2000));
        let summary = single_line_summary(&long_answer, SUMMARY_MAX_CHARS);
        assert!(!summary.contains('\n'));
        assert!(summary.chars().count() <= SUMMARY_MAX_CHARS);

        let with_newlines = "Answer: line one\nline two\n\n  line three  ";
        let collapsed = single_line_summary(with_newlines, SUMMARY_MAX_CHARS);
        assert_eq!(collapsed, "Answer: line one line two line three");
    }

    /// 実機（pqa 2026.8.12）の出力そのままの形: 時刻の前置き・色コード・折り返しの左詰めがある。
    #[test]
    fn extract_answer_handles_timestamped_ansi_wrapped_output() {
        let stdout = concat!(
            "[04:33:21] New file to index: unifyfs.txt...\n",
            "\u{1b}[1;31mProvider List: https://docs.litellm.ai/docs/providers\u{1b}[0m\n",
            "[04:38:30] Answer:                                        \n",
            "                                                          \n",
            "           UnifyFS and GekkoFS aggregate node-local NVMe   \n",
            "           into a job-scoped file system (Unify2020).      \n",
            "\n",
        );
        let answer = extract_answer(stdout);
        assert_eq!(
            answer,
            "UnifyFS and GekkoFS aggregate node-local NVMe\ninto a job-scoped file system (Unify2020)."
        );
        assert!(!answer.contains("New file to index"), "索引のログは含めない: {answer}");
        assert!(!answer.contains('\u{1b}'), "色コードは落とす: {answer:?}");
    }

    #[test]
    fn extract_answer_finds_answer_line_or_falls_back_to_full_output() {
        let stdout = "searching...\nsummarizing...\nAnswer: X causes Y [Doe2020].\n";
        // `Answer:` の見出しは落として本文だけを返す。
        assert_eq!(extract_answer(stdout), "X causes Y [Doe2020].");

        let no_answer_marker = "just some raw output\nwithout the marker\n";
        assert_eq!(extract_answer(no_answer_marker), no_answer_marker.trim_end_matches('\n'));
    }
}
