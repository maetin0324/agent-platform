//! `langmem` アダプタ（ADR-0047 D4。Phase 62）— 知識整理 run（`knowledge` harness）を起こす。
//!
//! `paperqa`/`local-deep-research` と同じ「調査・抽出エンジンを包む」形。LangMem
//! （`langmem.create_memory_manager`）は celeris のワーカープロトコルもストリーム型の進捗形式も話さない
//! ただの python ライブラリなので、**アダプタ自身が** ADR-0006 D3 の結果ファイル規約
//! （`artifacts/result.json`）を代わりに書き、`Terminal::Done`/`Terminal::Error` を合成する。
//! 委譲（`delegate.json`）は扱わない（知識整理 run は裏方の支援タスクで、子タスクを作らない）。
//!
//! run ごとに `runs/<run_id>/langmem_run.py` として埋め込みの python ランナーを書き出し、
//! `<command> <その場所> <run_dir>/langmem_input.json` で起動する（`local-deep-research` と同じ配線）。
//! **LLM はこの python プロセスの中だけ**で起きる（CLAUDE.md「ディスパッチャやストアに LLM 呼び出しを
//! 入れない」。dispatch の判断もトリガの決定も celeris 側は決定的なまま）。
//!
//! 入力は `req.task.objective` そのもの — `crates/celeris/src/knowledge_maint.rs` が
//! `task_core::knowledge::maintenance_objective` で、タスクの報告・結果・コメント・関連する既存の
//! KB ページ・担当ノードの手帳・既存の索引の題名を**あらかじめ** 1 本のテキストへ組んである
//! （`local-deep-research` の `build_query` と同じ考え方: アダプタ・ランナーは余計なファイルを
//! 読み書きしない）。出力は `artifacts/knowledge-candidates.json` = `{candidates: [...]}`
//! （`task_core::knowledge::Candidate` の配列。検査・適用は `task_ops::knowledge::apply_candidates`）。

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
use crate::progress;
use crate::protocol::RunRequest;
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, kill_now, read_line_limited, read_tail, reap_after_terminal,
    write_result_json,
};

/// run ごとに `runs/<run_id>/langmem_run.py` として書き出すランナー（ADR-0047 D4）。
const RUNNER_SCRIPT: &str = include_str!("langmem_run.py");

/// `artifacts/result.json` の `summary` の上限（他の python アダプタと同じ規則）。
const SUMMARY_MAX_CHARS: usize = 1500;
/// `progress:` 行を `progress` に転送するときの 1 行あたりの上限。
const PROGRESS_LINE_MAX_CHARS: usize = 500;
/// ランナーの最終行の目印。
const RESULT_PREFIX: &str = "CELERIS_RESULT ";
/// `progress:` 行の目印。
const PROGRESS_PREFIX: &str = "progress:";
/// `langmem` が import できないときにランナーが stderr に出す目印（ADR-0047 D4「clear error」）。
/// これが出ていれば `retryable = false`（venv のセットアップが要る。再試行しても直らない）。
pub const LANGMEM_MISSING_MARKER: &str = "CELERIS_LANGMEM_MISSING";

/// `[knowledge.langmem].provider`。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LangMemProvider {
    #[default]
    OpenaiCompatible,
    Anthropic,
}

impl LangMemProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            LangMemProvider::OpenaiCompatible => "openai-compatible",
            LangMemProvider::Anthropic => "anthropic",
        }
    }
}

/// `[adapters.langmem]`（config.toml）＋ `[knowledge.langmem]` の LLM 接続先をまとめた設定
/// （ADR-0047 D4）。celeris 側（`crates/celeris/src/lib.rs::build_adapters`）が 2 つの節を合わせて作る。
#[derive(Debug, Clone)]
pub struct LangMemConfig {
    /// 起動する python（`tools/langmem/.venv/bin/python` のような venv の python）。
    pub command: String,
    /// 無出力タイムアウト（`RunLimits.idle_timeout` を上回らない範囲で使う。既定はハーネスの予算のまま）。
    pub idle_timeout_secs: Option<u64>,
    pub provider: LangMemProvider,
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// `[knowledge.langmem].api_key_secret` を celeris が解決した値（無ければ渡さない）。
    pub api_key: Option<String>,
    pub env: Vec<(String, String)>,
}

impl Default for LangMemConfig {
    fn default() -> Self {
        Self {
            command: "python3".to_string(),
            idle_timeout_secs: None,
            provider: LangMemProvider::OpenaiCompatible,
            base_url: None,
            model: None,
            api_key: None,
            env: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LangMemAdapter {
    config: LangMemConfig,
}

impl LangMemAdapter {
    pub const ID: &'static str = "langmem";

    pub fn new(config: LangMemConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WorkerAdapter for LangMemAdapter {
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
        run_langmem(&self.config, &req, run_id, &limits, sink).await
    }

    /// 他のアダプタと同じ規則: `extra` は `config.env` の末尾に足す（同名キーは `extra` が勝つ）。
    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.env.extend(extra.iter().cloned());
        Some(Arc::new(LangMemAdapter::new(config)))
    }
}

async fn run_langmem(
    config: &LangMemConfig,
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

    // ADR-0036 D1/D2: 成果物の置き場はディスパッチャが決めた `artifacts_dir`（共有 workspace ではタスクごと）。
    let artifacts_dir = req.artifacts_dir.clone();
    let artifacts_rel = req.artifacts_rel();
    tokio::fs::create_dir_all(&artifacts_dir).await?;
    let candidates_path = artifacts_dir.join("knowledge-candidates.json");
    // 前回の run（リトライ）の名残を今回の結果と誤読しない（他アダプタと同じ理由。ADR-0006 D3）。
    let _ = tokio::fs::remove_file(&candidates_path).await;
    let _ = tokio::fs::remove_file(artifacts_dir.join("result.json")).await;

    // ADR-0047 D4: 依頼文（`objective`）は `crates/celeris/src/knowledge_maint.rs` が
    // `task_core::knowledge::maintenance_objective` で組み立て済み。アダプタは前置きを足さず
    // そのまま渡す（`local-deep-research` の `build_query` と同じ考え方）。
    let objective = req.task.objective.clone();
    // ADR-0023 D2 / M1: この run で何を渡したかを残す。
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &objective, run_id).await;

    let input = serde_json::json!({
        "task_id": req.task.id.to_string(),
        "objective": objective,
        "llm": {
            "provider": config.provider.as_str(),
            "base_url": config.base_url,
            "model": config.model,
            "api_key": config.api_key,
        },
        "candidates_path": candidates_path.to_string_lossy(),
    });
    let script_path = run_dir.join("langmem_run.py");
    let input_path = run_dir.join("langmem_input.json");
    tokio::fs::write(&script_path, RUNNER_SCRIPT).await?;
    let input_text = serde_json::to_string_pretty(&input)?;
    tokio::fs::write(&input_path, format!("{input_text}\n")).await?;

    let mut command = Command::new(&config.command);
    command
        .arg(&script_path)
        .arg(&input_path)
        .envs(config.env.iter().cloned())
        .current_dir(req.cwd())
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
    // ADR-0047 D4: `[adapters.langmem].idle_timeout_secs` はハーネスの予算（`limits.idle_timeout`）を
    // 上回らない範囲でだけ効く（config が長い秒数を書いても予算を突破できない）。
    let idle_timeout = config
        .idle_timeout_secs
        .map(Duration::from_secs)
        .map(|d| d.min(limits.idle_timeout))
        .unwrap_or(limits.idle_timeout);
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
        if idle_elapsed >= idle_timeout {
            timeout_terminal = Some(Terminal::Error {
                message: "idle timeout".into(),
                retryable: true,
            });
            force_kill = true;
            break;
        }
        let wait = (limits.wall_clock - wall_elapsed).min(idle_timeout - idle_elapsed);

        let outcome = match tokio::time::timeout(
            wait,
            read_line_limited(&mut reader, MAX_LINE_BYTES),
        )
        .await
        {
            Err(_elapsed) => continue, // タイムアウト。ループ先頭で上限超過を検知する。
            Ok(Err(e)) => return Err(AdapterError::Io(e)),
            Ok(Ok(outcome)) => outcome,
        };

        match outcome {
            LineOutcome::Eof => break,
            LineOutcome::TooLong => {
                sink.heartbeat();
                last_activity = Instant::now();
                warn!("run {run_id}: discarding overlong line from the langmem runner");
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
                    progress::emit_status(
                        sink,
                        &truncate_chars(rest.trim(), PROGRESS_LINE_MAX_CHARS),
                    );
                } else if let Some(rest) = trimmed.strip_prefix(RESULT_PREFIX) {
                    match serde_json::from_str::<serde_json::Value>(rest) {
                        Ok(value) => task_result = Some(value),
                        Err(e) => warn!("run {run_id}: could not parse CELERIS_RESULT line: {e}"),
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

    // ADR-0047 D4: `langmem` が import できないのは venv のセットアップが要る設定の誤りで、
    // 再試行しても直らない（`retryable = false`）。
    if classify_text.contains(LANGMEM_MISSING_MARKER) {
        let terminal = Terminal::Error {
            message: format!(
                "langmem is not importable (set up the venv: scripts/knowledge/setup-langmem.sh): {}",
                stderr_tail.lines().next_back().unwrap_or("").trim()
            ),
            retryable: false,
        };
        write_result_json(&run_dir, &terminal, None).await?;
        return Ok(RunOutcome {
            terminal,
            exit_code: exit_status.code(),
        });
    }

    let candidates_len = tokio::fs::read_to_string(&candidates_path)
        .await
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("candidates").and_then(|c| c.as_array().map(|a| a.len())));

    let (terminal, provider_failure) = if !exit_status.success() {
        let exit_repr = match exit_status.code() {
            Some(code) => code.to_string(),
            None => "signal".to_string(),
        };
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: format!(
                    "langmem runner exited with a non-zero status (exit={exit_repr})"
                ),
                retryable: true,
            },
            pf,
        )
    } else if task_result.is_none() {
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: "langmem runner did not print a CELERIS_RESULT line".to_string(),
                retryable: true,
            },
            pf,
        )
    } else if candidates_len.is_none() {
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: "langmem runner did not write artifacts/knowledge-candidates.json"
                    .to_string(),
                retryable: true,
            },
            pf,
        )
    } else {
        let value = task_result.unwrap_or(serde_json::Value::Null);
        let raw_summary = value
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let count = candidates_len.unwrap_or(0);
        let summary = if raw_summary.trim().is_empty() {
            format!("知識の候補 {count} 件を抽出した")
        } else {
            single_line_summary(raw_summary, SUMMARY_MAX_CHARS)
        };
        // ADR-0047 D4: 成果物として申告する（celeris が終端の run 一覧・`Check::ArtifactExists` に使う）。
        match crate::artifact::resolve(
            &req.workspace,
            "knowledge-candidates.json",
            &format!("{artifacts_rel}/knowledge-candidates.json"),
            Some("json"),
        ) {
            Ok(artifact) => sink.artifact(&artifact),
            Err(e) => warn!("run {run_id}: could not register knowledge-candidates.json: {e}"),
        }
        let result_file = serde_json::json!({ "summary": summary, "evidence": [] });
        match serde_json::to_string_pretty(&result_file) {
            Ok(text) => {
                if let Err(e) =
                    tokio::fs::write(artifacts_dir.join("result.json"), format!("{text}\n")).await
                {
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

    use task_core::ArtifactRef;

    use super::*;
    use crate::protocol::{PROTOCOL_VERSION, RunContext};

    #[derive(Default)]
    struct RecordingSink {
        progress: Mutex<Vec<String>>,
        heartbeat_count: Mutex<u32>,
        artifacts: Mutex<Vec<ArtifactRef>>,
    }

    impl EventSink for RecordingSink {
        fn progress(&self, msg: &str) {
            self.progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(msg.to_string());
        }
        fn artifact(&self, artifact: &ArtifactRef) {
            self.artifacts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(artifact.clone());
        }
        fn heartbeat(&self) {
            *self
                .heartbeat_count
                .lock()
                .unwrap_or_else(|e| e.into_inner()) += 1;
        }
    }

    /// `local-deep-research` のテストと同じ理由（ADR-0010 D10 の ETXTBSY 対策）で、スタブは別プロセスに
    /// 書かせる。**実物の `langmem_run.py` は使わない**（ネットワーク・本物の LLM 無しで検証できるのは
    /// アダプタ ↔ python のプロトコルだけ。§7「adapter test with a fake extractor」）。
    fn stub_langmem(dir: &Path, script: &str) -> LangMemConfig {
        let path = dir.join("langmem_stub.sh");
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        LangMemConfig {
            command: path.to_string_lossy().into_owned(),
            ..LangMemConfig::default()
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
            wall_clock: std::time::Duration::from_secs(30),
            idle_timeout: std::time::Duration::from_secs(30),
            kill_grace: std::time::Duration::from_millis(200),
        }
    }

    /// 固定の候補ファイルを書き、`CELERIS_RESULT` を出す偽の抽出器（本物の langmem_run.py の代わり）。
    fn fake_extractor_script() -> String {
        r#"input="$2"
candidates_path=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['candidates_path'])" "$input")
echo 'progress: reading the task objective...'
echo 'progress: extracting candidates...'
mkdir -p "$(dirname "$candidates_path")"
cat > "$candidates_path" <<'JSON'
{"candidates": [
  {"op": "create", "path": "environment/tools/newtool.md", "title": "newtool",
   "tags": ["tool"], "scope": "environment", "body": "newtool の使い方。",
   "sources": ["task:01J1"], "confidence": "high"}
]}
JSON
echo 'CELERIS_RESULT {"summary": "1 件の知識の候補を抽出した", "candidates": 1}'
"#
        .to_string()
    }

    #[tokio::test]
    async fn happy_path_writes_candidates_and_result_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_langmem(dir.path(), &fake_extractor_script());
        let adapter = LangMemAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req.clone(), "run-1", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => {
                assert_eq!(summary, "1 件の知識の候補を抽出した")
            }
            other => panic!("expected done, got {other:?}"),
        }
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m.contains("extracting candidates")));
        assert!(*sink.heartbeat_count.lock().unwrap() >= 2);
        let artifacts = sink.artifacts.lock().unwrap();
        assert_eq!(artifacts.len(), 1, "{artifacts:?}");
        assert_eq!(artifacts[0].name, "knowledge-candidates.json");
        assert_eq!(artifacts[0].path, "artifacts/knowledge-candidates.json");
        drop(artifacts);

        let candidates_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/knowledge-candidates.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(candidates_json["candidates"].as_array().unwrap().len(), 1);

        let result_json =
            std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result_json).unwrap();
        assert_eq!(parsed["summary"], "1 件の知識の候補を抽出した");

        assert!(dir.path().join("runs/run-1/langmem_run.py").is_file());
        let script = std::fs::read_to_string(dir.path().join("runs/run-1/langmem_run.py")).unwrap();
        assert_eq!(script, RUNNER_SCRIPT);
        let seen_input: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("runs/run-1/langmem_input.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(seen_input["objective"], req.task.objective);
    }

    /// `langmem` が import できない（venv 未セットアップ）は `retryable = false`（ADR-0047 D4）。
    #[tokio::test]
    async fn missing_langmem_package_is_not_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_langmem(
            dir.path(),
            "echo 'CELERIS_LANGMEM_MISSING: No module named langmem' 1>&2\nexit 1\n",
        );
        let adapter = LangMemAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-2", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(!retryable, "{message}");
                assert!(message.contains("langmem is not importable"), "{message}");
                assert!(message.contains("setup-langmem.sh"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(!dir.path().join("artifacts/result.json").exists());
    }

    #[tokio::test]
    async fn a_non_zero_exit_without_the_missing_marker_is_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_langmem(dir.path(), "echo 'boom' 1>&2\nexit 7\n");
        let adapter = LangMemAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-3", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=7"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// 候補ファイルを書かずに終わるのは（`CELERIS_RESULT` があっても）retryable エラー。
    #[tokio::test]
    async fn missing_candidates_file_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_langmem(
            dir.path(),
            "echo 'CELERIS_RESULT {\"summary\": \"x\", \"candidates\": 0}'\n",
        );
        let adapter = LangMemAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-4", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("knowledge-candidates.json"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// 候補が 0 件でも（空の `candidates: []`）正常終了になる（ADR-0047 D4「何も抽出するものが無ければ
    /// 空でよい」）。
    #[tokio::test]
    async fn zero_candidates_is_still_done() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"input="$2"
candidates_path=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['candidates_path'])" "$input")
mkdir -p "$(dirname "$candidates_path")"
printf '{"candidates": []}' > "$candidates_path"
echo 'CELERIS_RESULT {"summary": "", "candidates": 0}'
"#;
        let config = stub_langmem(dir.path(), script);
        let adapter = LangMemAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-5", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert!(summary.contains('0'), "{summary}"),
            other => panic!("expected done, got {other:?}"),
        }
    }

    /// 壁時計の超過でプロセスグループごと SIGKILL する（他の python サイドカーと同じ確認方法）。
    #[tokio::test]
    async fn wall_clock_exceeded_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid.txt");
        let config = stub_langmem(
            dir.path(),
            &format!(
                "echo $$ > {pid}\nwhile true; do sleep 0.1; done\n",
                pid = pid_file.display()
            ),
        );
        let adapter = LangMemAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: std::time::Duration::from_millis(300),
            idle_timeout: std::time::Duration::from_secs(30),
            kill_grace: std::time::Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-6", limits, &sink).await.unwrap();
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("wall clock exceeded"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        let pid_text = std::fs::read_to_string(&pid_file)
            .expect("stub should have recorded its pid before looping");
        let pid: i32 = pid_text.trim().parse().expect("pid.txt should contain a pid");
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "process {pid} should have been killed"
        );
    }

    /// `[adapters.langmem].idle_timeout_secs` はハーネスの予算を上回れない（min を取る）。
    #[tokio::test]
    async fn idle_timeout_secs_cannot_exceed_the_harness_budget() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid.txt");
        let mut config = stub_langmem(
            dir.path(),
            &format!(
                "echo $$ > {pid}\nwhile true; do sleep 0.1; done\n",
                pid = pid_file.display()
            ),
        );
        config.idle_timeout_secs = Some(3600); // 大きすぎる値。
        let adapter = LangMemAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: std::time::Duration::from_secs(30),
            idle_timeout: std::time::Duration::from_millis(300),
            kill_grace: std::time::Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-7", limits, &sink).await.unwrap();
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert!(matches!(outcome.terminal, Terminal::Error { .. }));
    }

    /// `with_env` の追加分は既存の同名キーより後に環境を組み立てるので勝つ（他アダプタと同じ規則）。
    #[tokio::test]
    async fn with_env_overrides_a_same_name_key_already_in_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("env-seen.txt");
        let script = format!(
            "printf '%s' \"$OPENAI_API_KEY\" > {out}\n\
             echo 'CELERIS_RESULT {{\"summary\": \"ok\", \"candidates\": 0}}'\n\
             input=\"$2\"\n\
             candidates_path=$(python3 -c \"import json,sys; print(json.load(open(sys.argv[1]))['candidates_path'])\" \"$input\")\n\
             mkdir -p \"$(dirname \"$candidates_path\")\"\n\
             printf '{{\"candidates\": []}}' > \"$candidates_path\"\n",
            out = out_file.display()
        );
        let mut config = stub_langmem(dir.path(), &script);
        config
            .env
            .push(("OPENAI_API_KEY".to_string(), "old-key".to_string()));
        let base = LangMemAdapter::new(config);
        let with_env = base
            .with_env(&[("OPENAI_API_KEY".to_string(), "new-key".to_string())])
            .expect("langmem supports with_env");

        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = with_env
            .run(req, "run-8", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let seen = std::fs::read_to_string(&out_file).unwrap();
        assert_eq!(seen, "new-key");
    }
}
