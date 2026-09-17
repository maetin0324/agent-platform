//! `local-deep-research` アダプタ（DESIGN §5.4, ADR-0029 D1）。
//!
//! Local Deep Research（LDR）は `paperqa`（ADR-0027 D3）と同じ「調査エンジンを包む」形。LDR には
//! 一発実行の CLI が無く（`ldr-web`/`ldr-mcp` は常駐プロセス）、プログラム的な API
//! （`local_deep_research.api.{quick_summary,detailed_research,generate_report}`）だけがある。
//! そのため taskd 側が実行用の Python スクリプトを持ち（`include_str!`）、run ごとに
//! `runs/<run_id>/ldr_run.py` として書き出して `<command> <その場所> <run_dir>/ldr_input.json` で起動する。
//! taskd の外に置くファイルは venv（`command` が指す python）だけで、スクリプト自体は taskd のバイナリと
//! 一緒に版が進む。
//!
//! ワーカープロトコル（`artifacts/result.json`）は PaperQA2 アダプタと同じく**アダプタが代わりに書く**
//! （ADR-0006 D3 の規約は保つ）。委譲（`delegate.json`）は扱わない（ADR-0029 D1: 「委譲はしない」）。

use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
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

/// run ごとに `runs/<run_id>/ldr_run.py` として書き出す本体（ADR-0029 D1）。
const RUNNER_SCRIPT: &str = include_str!("local_deep_research_run.py");

/// `artifacts/result.json` の `summary` の上限（`paperqa` と同じ規則。ADR-0029 D1）。
const SUMMARY_MAX_CHARS: usize = 1500;
/// `progress:` 行を `progress` に転送するときの 1 行あたりの上限。
const PROGRESS_LINE_MAX_CHARS: usize = 500;
/// ランナーの最終行の目印（ADR-0029 D1）。
const RESULT_PREFIX: &str = "TASKD_RESULT ";
/// `progress:` 行の目印。
const PROGRESS_PREFIX: &str = "progress:";

/// `[adapters.local_deep_research].mode`（ADR-0029 D1）。既定 `Quick`。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LdrMode {
    #[default]
    Quick,
    Detailed,
    Report,
}

impl LdrMode {
    fn as_str(self) -> &'static str {
        match self {
            LdrMode::Quick => "quick",
            LdrMode::Detailed => "detailed",
            LdrMode::Report => "report",
        }
    }
}

/// `[adapters.local_deep_research]`（taskd.toml, ADR-0029 D1）。`[[providers]] adapter =
/// "local-deep-research"` の行ごとに `model`（= `settings` の `llm.model` を上書き）と `env` を上書きできる
/// （`paperqa`/`acp` と同じ作り）。行の `settings` の上書きは無い（`ProviderConfig.settings` は `paperqa` 専用
/// のフィールドで、LDR では再利用しない。taskd 側の実装判断）。
#[derive(Debug, Clone)]
pub struct LdrConfig {
    /// 起動するコマンド（LDR を入れた venv の python）。
    pub command: String,
    pub mode: LdrMode,
    /// `quick_summary`/`detailed_research` の `iterations`。未指定なら渡さない。
    pub iterations: Option<u32>,
    /// `quick_summary`/`detailed_research` の `questions_per_iteration`。未指定なら渡さない。
    pub questions_per_iteration: Option<u32>,
    /// `settings_override` に渡すキー。値は文字列で持ち、数値・真偽値・JSON 配列/オブジェクトに見えるものは
    /// ランナー（Python）側で変換する（ADR-0029 D1/D3: TOML の型を混ぜない）。
    pub settings: Vec<(String, String)>,
    /// 設定されていれば `settings` の `llm.model` を上書きする（`paperqa` の `--llm` と同じ考え方）。
    pub model: Option<String>,
    /// 追加の環境変数。
    pub env: Vec<(String, String)>,
}

impl Default for LdrConfig {
    fn default() -> Self {
        Self {
            command: "python3".to_string(),
            mode: LdrMode::Quick,
            iterations: None,
            questions_per_iteration: None,
            settings: Vec::new(),
            model: None,
            env: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LdrAdapter {
    config: LdrConfig,
}

impl LdrAdapter {
    pub const ID: &'static str = "local-deep-research";

    pub fn new(config: LdrConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WorkerAdapter for LdrAdapter {
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
        run_ldr(&self.config, &req, run_id, &limits, sink).await
    }

    /// 他のアダプタ（`paperqa`/`claude_code`/`codex`）と同じ規則: `extra` は `config.env` の末尾に足すので、
    /// 同名キーは `extra` が勝つ（taskd の環境 < アダプタの環境 < `with_env` の追加分）。
    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.env.extend(extra.iter().cloned());
        Some(Arc::new(LdrAdapter::new(config)))
    }
}

/// タスクの目的から LDR に渡す問いを組み立てる（`paperqa::build_question` と同じ考え方: LDR はワーカー
/// プロトコルを話さない調査エンジンなので、結果ファイルの書式やコマンド再実行の話をしても意味が無く、
/// 素の目的 + 役割の指示文 + 人間の回答履歴だけを使う）。
pub fn build_query(task: &Task, context: &RunContext) -> String {
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

async fn run_ldr(
    config: &LdrConfig,
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

    let artifacts_dir = req.workspace.join("artifacts");
    tokio::fs::create_dir_all(&artifacts_dir).await?;
    let report_path = artifacts_dir.join("report.md");
    // 前回の run（リトライ）の名残を今回の結果と誤読しない（paperqa/claude_code/codex と同じ理由。ADR-0006 D3）。
    let _ = tokio::fs::remove_file(&report_path).await;
    let _ = tokio::fs::remove_file(artifacts_dir.join("result.json")).await;

    let query = build_query(&req.task, &req.context);
    // ADR-0023 D2 / M1: この run で何を渡したかを残す。
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &query, run_id).await;

    // `model` は `settings` の `llm.model` より優先する（ADR-0029 D1）。`BTreeMap` で決定的な順序にする。
    let mut settings: std::collections::BTreeMap<String, String> = config.settings.iter().cloned().collect();
    if let Some(model) = &config.model {
        settings.insert("llm.model".to_string(), model.clone());
    }

    let input = serde_json::json!({
        "query": query,
        "mode": config.mode.as_str(),
        "settings": settings,
        "iterations": config.iterations,
        "questions_per_iteration": config.questions_per_iteration,
        "report_path": report_path.to_string_lossy(),
    });
    let script_path = run_dir.join("ldr_run.py");
    let input_path = run_dir.join("ldr_input.json");
    tokio::fs::write(&script_path, RUNNER_SCRIPT).await?;
    let input_text = serde_json::to_string_pretty(&input)?;
    tokio::fs::write(&input_path, format!("{input_text}\n")).await?;

    let mut command = Command::new(&config.command);
    command
        .arg(&script_path)
        .arg(&input_path)
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
    let mut task_result: Option<serde_json::Value> = None;
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
                // ランナーの出力形式は taskd が定義したものではないので寛容に無視する（paperqa と同じ考え方）。
                sink.heartbeat();
                last_activity = Instant::now();
                warn!("run {run_id}: discarding overlong line from the local-deep-research runner");
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
                if let Some(rest) = trimmed.strip_prefix(PROGRESS_PREFIX) {
                    // ADR-0029 D1 手順 3: ランナーは検索・要約の進捗を `progress: <text>` の形で出す。
                    sink.progress(&truncate_chars(rest.trim(), PROGRESS_LINE_MAX_CHARS));
                } else if let Some(rest) = trimmed.strip_prefix(RESULT_PREFIX) {
                    match serde_json::from_str::<serde_json::Value>(rest) {
                        Ok(value) => task_result = Some(value),
                        Err(e) => warn!("run {run_id}: could not parse TASKD_RESULT line: {e}"),
                    }
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
    let classify_text = format!("{stdout_buf}\n{stderr_tail}");

    let report_len = tokio::fs::metadata(&report_path).await.map(|m| m.len()).unwrap_or(0);

    let (terminal, provider_failure) = if !exit_status.success() {
        let exit_repr = match exit_status.code() {
            Some(code) => code.to_string(),
            None => "signal".to_string(),
        };
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: format!("local-deep-research runner exited with a non-zero status (exit={exit_repr})"),
                retryable: true,
            },
            pf,
        )
    } else if task_result.is_none() {
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: "local-deep-research runner did not print a TASKD_RESULT line".to_string(),
                retryable: true,
            },
            pf,
        )
    } else if report_len == 0 {
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: "local-deep-research runner produced an empty report".to_string(),
                retryable: true,
            },
            pf,
        )
    } else {
        // ADR-0029 D1: アダプタが `artifacts/report.md`（ランナーが直接書いた）を成果物として申告し、
        // `artifacts/result.json` を書く。exit=0 かつ `report_len > 0` の枝で `task_result` は必ず
        // `Some`（上の 2 つの分岐で `None`/空報告は既に処理済み）なので `unwrap_or_default` で十分。
        let value = task_result.unwrap_or(serde_json::Value::Null);
        let raw_summary = value.get("summary").and_then(|v| v.as_str()).unwrap_or_default();
        let summary = single_line_summary(raw_summary, SUMMARY_MAX_CHARS);
        match crate::artifact::resolve(&req.workspace, "report.md", "artifacts/report.md", Some("markdown")) {
            Ok(artifact) => sink.artifact(&artifact),
            Err(e) => warn!("run {run_id}: could not register artifacts/report.md: {e}"),
        }
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

/// `artifacts/result.json` の `summary`: 改行・連続空白を単一の空白にたたみ（single-line-safe）、
/// 文字数で上限まで切り詰める（ランナー側でも行うが、二重に安全側へ倒す）。
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

    fn stub_ldr(dir: &Path, script: &str) -> LdrConfig {
        let path = dir.join("ldr_stub.sh");
        // ETXTBSY 対策（ADR-0010 D10）: 他アダプタのテストと同じ理由で別プロセスに書かせる。
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        LdrConfig {
            command: path.to_string_lossy().into_owned(),
            ..LdrConfig::default()
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

    /// スタブは argv[2]（`ldr_input.json` のパス）に成功時の `report.md` を書き、progress と
    /// TASKD_RESULT を出す（実際のランナーの動きを最小限まねる）。
    fn success_script() -> &'static str {
        r#"input="$2"
report_path=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['report_path'])" "$input")
echo 'progress: searching the web...'
echo 'progress: reading 3 pages...'
mkdir -p "$(dirname "$report_path")"
printf '# Report\n\nfound X and Y with sources\n' > "$report_path"
echo 'TASKD_RESULT {"summary": "found X and Y with sources", "sources": 2}'
"#
    }

    #[tokio::test]
    async fn happy_path_progress_report_and_result_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_ldr(dir.path(), success_script());
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req.clone(), "run-1", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, evidence, usage } => {
                assert_eq!(summary, "found X and Y with sources");
                assert!(evidence.is_empty());
                assert!(usage.is_none());
            }
            other => panic!("expected done, got {other:?}"),
        }
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m.contains("searching the web")));
        assert!(progress.iter().any(|m| m.contains("reading 3 pages")));
        assert!(*sink.heartbeat_count.lock().unwrap() >= 3);

        let artifacts = sink.artifacts.lock().unwrap();
        assert_eq!(artifacts.len(), 1, "{artifacts:?}");
        assert_eq!(artifacts[0].name, "report.md");
        assert_eq!(artifacts[0].path, "artifacts/report.md");
        assert!(!artifacts[0].sha256.is_empty());
        drop(artifacts);

        let report_md = std::fs::read_to_string(dir.path().join("artifacts/report.md")).unwrap();
        assert!(report_md.contains("found X and Y with sources"));

        let result_json = std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result_json).unwrap();
        assert_eq!(parsed["summary"], "found X and Y with sources");
        assert_eq!(parsed["evidence"], serde_json::json!([]));

        assert!(dir.path().join("runs/run-1/stdout.log").is_file());
        assert!(dir.path().join("runs/run-1/stderr.log").is_file());
        assert!(dir.path().join("runs/run-1/ldr_run.py").is_file());
        assert!(dir.path().join("runs/run-1/ldr_input.json").is_file());
        let run_result = std::fs::read_to_string(dir.path().join("runs/run-1/result.json")).unwrap();
        match serde_json::from_str::<crate::protocol::WorkerMessage>(run_result.trim()).unwrap() {
            crate::protocol::WorkerMessage::Done { summary, .. } => assert_eq!(summary, "found X and Y with sources"),
            other => panic!("expected done in runs/<run_id>/result.json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_zero_exit_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_ldr(dir.path(), "cat >/dev/null; echo 'boom' 1>&2; exit 7");
        let adapter = LdrAdapter::new(config);
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
    async fn missing_taskd_result_line_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        // report.md は書くが TASKD_RESULT を出さずに終わる。
        let config = stub_ldr(
            dir.path(),
            "mkdir -p artifacts && echo hi > artifacts/report.md\necho 'progress: working'\n",
        );
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-3", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("TASKD_RESULT"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(!dir.path().join("artifacts/result.json").exists());
    }

    #[tokio::test]
    async fn empty_report_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_ldr(
            dir.path(),
            "mkdir -p artifacts && : > artifacts/report.md\necho 'TASKD_RESULT {\"summary\": \"x\", \"sources\": 0}'\n",
        );
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-4", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("empty report"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(!dir.path().join("artifacts/result.json").exists());
    }

    /// 壁時計の超過でプロセスグループごと SIGKILL する（paperqa/acp と同じ確認方法: `/proc/<pid>` の消滅）。
    #[tokio::test]
    async fn wall_clock_exceeded_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid.txt");
        let config = stub_ldr(
            dir.path(),
            &format!(
                r#"echo $$ > {pid}
while true; do sleep 0.1; done
"#,
                pid = pid_file.display()
            ),
        );
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_millis(300),
            idle_timeout: Duration::from_secs(30),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-5", limits, &sink).await.unwrap();
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

    fn read_json(path: &Path) -> serde_json::Value {
        let text = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// 入力 JSON の組み立てを argv 経由で確認する: `query`/`mode`/`settings`（`model` が `llm.model` を
    /// 上書き）/`iterations`/`questions_per_iteration`/`report_path`（ADR-0029 D1）。
    #[tokio::test]
    async fn input_json_is_composed_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_ldr(
            dir.path(),
            "cp \"$2\" \"$(dirname \"$0\")/seen_input.json\"\n\
             mkdir -p artifacts && echo hi > artifacts/report.md\n\
             echo 'TASKD_RESULT {\"summary\": \"ok\", \"sources\": 0}'\n",
        );
        config.mode = LdrMode::Detailed;
        config.iterations = Some(3);
        config.questions_per_iteration = Some(2);
        config.settings = vec![
            ("llm.provider".to_string(), "openai_endpoint".to_string()),
            ("llm.model".to_string(), "should-be-overridden".to_string()),
            ("search.tool".to_string(), "searxng".to_string()),
        ];
        config.model = Some("qwen3.8-27b".to_string());
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req.clone(), "run-6", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));

        let seen = read_json(&dir.path().join("seen_input.json"));
        assert_eq!(seen["query"], serde_json::Value::String(build_query(&req.task, &req.context)));
        assert_eq!(seen["mode"], "detailed");
        assert_eq!(seen["iterations"], 3);
        assert_eq!(seen["questions_per_iteration"], 2);
        assert_eq!(seen["settings"]["llm.provider"], "openai_endpoint");
        assert_eq!(seen["settings"]["search.tool"], "searxng");
        // `model` が `settings` の `llm.model` を上書きする。
        assert_eq!(seen["settings"]["llm.model"], "qwen3.8-27b");
        assert!(seen["report_path"].as_str().unwrap().ends_with("artifacts/report.md"));

        // `runs/<run_id>/ldr_run.py` は埋め込みランナーそのもの。
        let script = std::fs::read_to_string(dir.path().join("runs/run-6/ldr_run.py")).unwrap();
        assert_eq!(script, RUNNER_SCRIPT);
    }

    /// `iterations`/`questions_per_iteration` を設定しなければ `null` のまま渡す。
    #[tokio::test]
    async fn iterations_and_questions_are_null_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_ldr(
            dir.path(),
            "cp \"$2\" \"$(dirname \"$0\")/seen_input.json\"\n\
             mkdir -p artifacts && echo hi > artifacts/report.md\n\
             echo 'TASKD_RESULT {\"summary\": \"ok\", \"sources\": 0}'\n",
        );
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        adapter.run(req, "run-7", default_limits(), &sink).await.unwrap();
        let seen = read_json(&dir.path().join("seen_input.json"));
        assert_eq!(seen["mode"], "quick");
        assert!(seen["iterations"].is_null());
        assert!(seen["questions_per_iteration"].is_null());
    }

    /// `with_env` の追加分は既存の同名キーより後に環境を組み立てるので勝つ（他アダプタと同じ規則）。
    #[tokio::test]
    async fn with_env_overrides_a_same_name_key_already_in_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("env-seen.txt");
        let mut config = stub_ldr(
            dir.path(),
            &format!(
                "printf '%s' \"$OPENAI_BASE_URL\" > {out}\n\
                 mkdir -p artifacts && echo hi > artifacts/report.md\n\
                 echo 'TASKD_RESULT {{\"summary\": \"ok\", \"sources\": 0}}'\n",
                out = out_file.display()
            ),
        );
        config.env.push(("OPENAI_BASE_URL".to_string(), "http://old:1".to_string()));
        let base = LdrAdapter::new(config);
        let with_env = base
            .with_env(&[("OPENAI_BASE_URL".to_string(), "http://new:2".to_string())])
            .expect("local-deep-research supports with_env");

        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = with_env.run(req, "run-8", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let seen = std::fs::read_to_string(&out_file).unwrap();
        assert_eq!(seen, "http://new:2");
    }

    /// LLM 供給側のエラー文面（認証失敗）が `AdapterError::AuthFailed` として分類される（ADR-0010 D5）。
    /// タイムアウトでは分類しない（別テストで確認済みの `wall_clock_exceeded` はプレーンな Error）。
    #[tokio::test]
    async fn llm_auth_failure_is_classified_as_adapter_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_ldr(
            dir.path(),
            "echo 'AuthenticationError: Invalid API key provided' 1>&2\nexit 1\n",
        );
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-9", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::AuthFailed(_)), "{err:?}");
        assert!(dir.path().join("runs/run-9/result.json").is_file());
    }

    /// 要約は空白をたたんで単一行にし、上限文字数で切り詰める（ランナーが既に行うが、アダプタ側も二重に守る）。
    #[test]
    fn single_line_summary_collapses_whitespace_and_truncates() {
        let long_answer = "word ".repeat(2000);
        let summary = single_line_summary(&long_answer, SUMMARY_MAX_CHARS);
        assert!(!summary.contains('\n'));
        assert!(summary.chars().count() <= SUMMARY_MAX_CHARS);

        let with_newlines = "line one\nline two\n\n  line three  ";
        let collapsed = single_line_summary(with_newlines, SUMMARY_MAX_CHARS);
        assert_eq!(collapsed, "line one line two line three");
    }

    /// 実機で見つかった不具合の回帰: アダプタが作る問いはタスクのタイトルを `# ...` の見出しとして含むので、
    /// ランナーがさらに `# ` を足すと `# # タイトル` になる。見出しで始まっていればそのまま使う。
    #[test]
    fn runner_report_does_not_double_the_markdown_heading() {
        let Ok(python) = std::process::Command::new("python3").arg("--version").output() else {
            eprintln!("skipping: python3 not available");
            return;
        };
        if !python.status.success() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("local_deep_research_run.py");
        std::fs::write(&script_path, RUNNER_SCRIPT).unwrap();
        let checker = r##"
import importlib.util, json, os, sys, tempfile
spec = importlib.util.spec_from_file_location("ldr_run", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
out = []
with tempfile.TemporaryDirectory() as d:
    for query in ["# Title" + chr(10) + chr(10) + "body", "plain question"]:
        path = os.path.join(d, "report.md")
        mod.write_report_from_result(path, query, {"summary": "s", "sources": []})
        with open(path) as f:
            out.append(f.read().splitlines()[0])
print(json.dumps(out))
"##;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let values: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(values, serde_json::json!(["# Title", "# plain question"]));
    }

    /// ランナーの `convert_setting_value`（int/float/bool/JSON 配列・オブジェクトへの変換）を、実際に
    /// 埋め込んだスクリプトに対して python3 で直接確認する（`local_deep_research` の import は
    /// `main()` の中だけにあるので、パッケージ未導入でもモジュールとして読み込める。ネットワークには出ない）。
    #[test]
    fn runner_convert_setting_value_handles_bool_int_float_and_json() {
        let Ok(python) = std::process::Command::new("python3").arg("--version").output() else {
            eprintln!("skipping: python3 not available");
            return;
        };
        if !python.status.success() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("local_deep_research_run.py");
        std::fs::write(&script_path, RUNNER_SCRIPT).unwrap();
        let checker = r#"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("ldr_run", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
cases = [
    "true", "FALSE", "42", "-3", "3.5",
    '["bing"]', '{"a": 1}', "not json but starts with [", "plain",
]
print(json.dumps([mod.convert_setting_value(c) for c in cases]))
"#;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let values: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(
            values,
            serde_json::json!([
                true,
                false,
                42,
                -3,
                3.5,
                ["bing"],
                {"a": 1},
                "not json but starts with [",
                "plain",
            ])
        );
    }
}
