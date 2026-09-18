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

/// `[adapters.local_deep_research.evidence]`（ADR-0031 D2）: 決定的な証拠ゲートの閾値。ハーネス
/// （このアダプタ）が `TASKD_RESULT` の `counts` を見て機械的に判定する（LLM に判断させない）。
/// `0` を書けばその項目は見ない。全部 0 なら従来どおり（ゲート無し）の挙動になる（受け入れ条件 3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceThresholds {
    /// 検索が返した件数の合計の下限（`counts.search_results`。ランナーは重複排除前の出典件数を使う）。
    #[serde(default = "default_min_search_results")]
    pub min_search_results: u32,
    /// 実際に証拠として集まった出典（URL で重複排除後）の数の下限（`counts.sources`）。
    #[serde(default = "default_min_sources")]
    pub min_sources: u32,
    /// 報告が `[n]` で引用した出典の数の下限（`counts.sources_cited`）。
    #[serde(default = "default_min_cited")]
    pub min_cited: u32,
    /// 出典の異なるドメイン数の下限（`counts.unique_domains`）。
    #[serde(default = "default_min_domains")]
    pub min_domains: u32,
}

impl Default for EvidenceThresholds {
    fn default() -> Self {
        Self {
            min_search_results: default_min_search_results(),
            min_sources: default_min_sources(),
            min_cited: default_min_cited(),
            min_domains: default_min_domains(),
        }
    }
}

fn default_min_search_results() -> u32 {
    5
}
fn default_min_sources() -> u32 {
    3
}
fn default_min_cited() -> u32 {
    2
}
fn default_min_domains() -> u32 {
    2
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
    /// ADR-0031 D2: 決定的な証拠ゲートの閾値。
    pub evidence: EvidenceThresholds,
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
            evidence: EvidenceThresholds::default(),
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

/// LDR に渡す**問い**を組み立てる。
///
/// 実機で分かったこと（2026-09-17）: ここにタスクのタイトルの見出し（`# ...`）や役割の指示文まで入れると、
/// 検索エンジンがその文字列ごと検索して**何も返さない**（同じ問いを素で投げれば出典が取れる）。
/// LDR は受け取った問いをそのまま検索にも使うので、**素の目的だけ**を渡す。
/// 人間の回答履歴は短い補足として後ろに付ける（検索語としての邪魔が少ない）。
/// 役割の指示文とタイトルは `runs/<run_id>/request.json` に残るので記録は失われない。
pub fn build_query(task: &Task, context: &RunContext) -> String {
    // ADR-0029 / Phase 19 / ADR-0033 D6（Phase 27 の監査 M-2）: 検索ハーネスに渡すのは**素の目的だけ**。
    // 役職・記憶・直近のやり取り・記憶の書式指示は載せない（問いを濁すと検索が何も返さない）。
    let mut out = String::new();
    out.push_str(task.objective.trim());
    if !context.answers.is_empty() {
        out.push_str("\n\n補足（人間の回答）:");
        for Answer { question, answer } in &context.answers {
            out.push_str(&format!("\n- {question} → {answer}"));
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

        // ADR-0031 D1: `report.md` に加えて、ランナーが機械的に作った証拠の記録
        // （`sources.json` / `research.json`）も成果物として申告する。ゲート（下）に落ちても
        // **消さずに残す**（D2: 人が読めるように）ので、この申告はゲートの判定より前に行う。
        for (name, rel_path, kind) in [
            ("report.md", "artifacts/report.md", "markdown"),
            ("sources.json", "artifacts/sources.json", "json"),
            ("research.json", "artifacts/research.json", "json"),
        ] {
            match crate::artifact::resolve(&req.workspace, name, rel_path, Some(kind)) {
                Ok(artifact) => sink.artifact(&artifact),
                Err(e) => warn!("run {run_id}: could not register {rel_path}: {e}"),
            }
        }

        // ADR-0031 D2: 決定的な証拠ゲート。`TASKD_RESULT` の `counts` を見る（古いランナー/スタブで
        // 無ければ全 0 扱い＝閾値を全部 0 にしないと落ちる）。LLM には判断させない。
        let counts = value.get("counts");
        let count_of = |key: &str| -> u32 {
            counts.and_then(|c| c.get(key)).and_then(serde_json::Value::as_u64).unwrap_or(0) as u32
        };
        let search_results = count_of("search_results");
        let sources = count_of("sources");
        let sources_cited = count_of("sources_cited");
        let unique_domains = count_of("unique_domains");
        let ev = &config.evidence;

        // どれか 1 つでも閾値が立っているか（全部 0 なら ADR-0031 D2 どおりゲートを見ない）。
        let gate_enabled =
            ev.min_search_results > 0 || ev.min_sources > 0 || ev.min_cited > 0 || ev.min_domains > 0;
        let gate_message = if gate_enabled && search_results == 0 {
            // 検索経路そのものの問題（鍵切れ・CAPTCHA・ネットワーク遮断）を、調べた結果情報が無かった
            // ケースと区別できるように、別メッセージにする（ADR-0031 D2）。`min_search_results = 0` に
            // していてもこの区別は要る（他の項目で落ちるので、運用者が原因を知りたいのは同じ）。
            Some(
                "web search returned nothing (possible search path failure: expired key, CAPTCHA, or network block)"
                    .to_string(),
            )
        } else {
            let mut problems = Vec::new();
            if ev.min_search_results > 0 && search_results < ev.min_search_results {
                problems.push(format!("search_results={search_results} (min {})", ev.min_search_results));
            }
            if ev.min_sources > 0 && sources < ev.min_sources {
                problems.push(format!("sources={sources} (min {})", ev.min_sources));
            }
            if ev.min_cited > 0 && sources_cited < ev.min_cited {
                problems.push(format!("cited={sources_cited} (min {})", ev.min_cited));
            }
            if ev.min_domains > 0 && unique_domains < ev.min_domains {
                problems.push(format!("domains={unique_domains} (min {})", ev.min_domains));
            }
            if problems.is_empty() {
                None
            } else {
                Some(format!("insufficient web evidence: {}", problems.join(", ")))
            }
        };

        if let Some(message) = gate_message {
            // ADR-0031 D2: retryable な `Terminal::Error`。供給側の失敗（`AdapterError`）にはしない
            // （プロバイダを cooldown にする話ではない）ので `provider_failure` は `None` のまま。
            (Terminal::Error { message, retryable: true }, None)
        } else {
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
        }
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

    /// counts が満たす閾値（既定: min_search_results=5, min_sources=3, min_cited=2, min_domains=2。ADR-0031 D2）。
    const PASSING_COUNTS: &str =
        r#"{"queries": 1, "search_results": 5, "sources": 3, "sources_cited": 2, "unique_domains": 3}"#;

    /// スタブは argv[2]（`ldr_input.json` のパス）に成功時の `report.md`/`sources.json`/`research.json` を
    /// 書き、progress と `TASKD_RESULT`（`counts` 込み）を出す（実際のランナーの動きを最小限まねる。ADR-0031 D1）。
    /// `counts_json` が `None` なら `TASKD_RESULT` に `counts` を含めない（古いランナー/スタブの再現）。
    fn script_with_counts(counts_json: Option<&str>) -> String {
        let counts = counts_json.unwrap_or(r#"{"queries": 0, "search_results": 0, "sources": 0, "sources_cited": 0, "unique_domains": 0}"#);
        let result_line = match counts_json {
            Some(counts) => format!(r#"TASKD_RESULT {{"summary": "found X and Y with sources", "sources": 3, "counts": {counts}}}"#),
            None => r#"TASKD_RESULT {"summary": "found X and Y with sources", "sources": 3}"#.to_string(),
        };
        format!(
            r#"input="$2"
report_path=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['report_path'])" "$input")
artifacts_dir=$(dirname "$report_path")
echo 'progress: searching the web...'
echo 'progress: reading 3 pages...'
mkdir -p "$artifacts_dir"
printf '# Report\n\nfound X and Y with sources\n' > "$report_path"
printf '[{{"url": "https://a.example.com/1", "title": "A", "engine": "tavily", "cited": true}}, {{"url": "https://b.example.com/2", "title": "B", "engine": null, "cited": true}}, {{"url": "https://c.example.org/3", "title": "C", "engine": null, "cited": false}}]' > "$artifacts_dir/sources.json"
printf '{{"queries": [{{"query": "q1", "engine": null, "result_count": null}}], "iterations": 1, "counts": {counts}}}' > "$artifacts_dir/research.json"
echo '{result_line}'
"#
        )
    }

    /// counts が既定の閾値を満たすスタブ（happy path 用）。
    fn success_script() -> String {
        script_with_counts(Some(PASSING_COUNTS))
    }

    #[tokio::test]
    async fn happy_path_progress_report_and_result_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_ldr(dir.path(), &success_script());
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

        // ADR-0031 受け入れ条件 1: report.md / sources.json / research.json の 3 つが成果物として申告される。
        let artifacts = sink.artifacts.lock().unwrap();
        assert_eq!(artifacts.len(), 3, "{artifacts:?}");
        let names: Vec<&str> = artifacts.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"report.md"), "{names:?}");
        assert!(names.contains(&"sources.json"), "{names:?}");
        assert!(names.contains(&"research.json"), "{names:?}");
        for a in artifacts.iter() {
            assert!(!a.sha256.is_empty(), "{a:?}");
        }
        let sources_artifact = artifacts.iter().find(|a| a.name == "sources.json").unwrap();
        assert_eq!(sources_artifact.path, "artifacts/sources.json");
        let research_artifact = artifacts.iter().find(|a| a.name == "research.json").unwrap();
        assert_eq!(research_artifact.path, "artifacts/research.json");
        drop(artifacts);

        let sources_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("artifacts/sources.json")).unwrap()).unwrap();
        assert_eq!(sources_json.as_array().unwrap().len(), 3);
        let research_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("artifacts/research.json")).unwrap()).unwrap();
        assert_eq!(research_json["counts"]["sources"], 3);

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

    /// 実機の回帰（2026-09-17）: 検索に渡す問いにタイトルの見出しや役割の指示文を入れると、検索が
    /// 何も返さなくなる。素の目的だけを渡す（人間の回答があれば短い補足として足す）。
    /// Phase 27（監査 M-2）: 役職・brief・記憶・直近のやり取りも載せない（ADR-0033 D6 に追記）。
    #[test]
    fn build_query_sends_only_the_objective_not_the_title_role_memory_or_conversation() {
        let mut task = crate::protocol::tests::sample_task();
        task.title = "gate pass check".into();
        task.objective = "What is Kubernetes and what problem does it solve?".into();
        let mut context = RunContext {
            role: Some(crate::protocol::RoleContext {
                id: "web-scout".into(),
                instructions: "あなたは Web 調査担当。出典 URL を付ける。".into(),
            }),
            node: Some(crate::protocol::NodeContext {
                id: "research-survey".into(),
                name: "関連研究調査課".into(),
                brief: "関連研究を洗う。".into(),
            }),
            memory: Some(crate::protocol::MemoryContext {
                notes: "- 2026-09-10: pegasus は pjsub で投げる".into(),
                project: "- 2026-09-16: Pluvio は非同期ランタイム基盤".into(),
            }),
            conversation: vec![crate::protocol::ConversationTurn {
                role: task_core::MessageRole::User,
                text: "先週の続きを".into(),
            }],
            standing_rules: vec!["1 ノードで始めてよい".into()],
            ..Default::default()
        };
        let query = build_query(&task, &context);
        assert_eq!(query, "What is Kubernetes and what problem does it solve?");
        assert!(!query.contains("gate pass check"), "{query}");
        assert!(!query.contains("Web 調査担当"), "{query}");
        assert!(!query.contains("関連研究調査課"), "{query}");
        assert!(!query.contains("pjsub"), "{query}");
        assert!(!query.contains("先週の続きを"), "{query}");
        assert!(!query.contains("覚えておくこと"), "{query}");

        context.answers = vec![Answer { question: "対象は?".into(), answer: "v1.31".into() }];
        let with_answers = build_query(&task, &context);
        assert!(with_answers.starts_with("What is Kubernetes"), "{with_answers}");
        assert!(with_answers.contains("対象は? → v1.31"), "{with_answers}");
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
        // このテストは入力 JSON の組み立てを見るだけで、証拠ゲート（ADR-0031 D2）とは無関係なので無効にする
        // （スタブの TASKD_RESULT に counts が無い）。
        config.evidence = EvidenceThresholds { min_search_results: 0, min_sources: 0, min_cited: 0, min_domains: 0 };
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
        // このテストは環境変数の上書きを見るだけで、証拠ゲート（ADR-0031 D2）とは無関係なので無効にする
        // （スタブの TASKD_RESULT に counts が無い）。
        config.evidence = EvidenceThresholds { min_search_results: 0, min_sources: 0, min_cited: 0, min_domains: 0 };
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

    /// Phase 32: 実機で起きたレビュー不合格（`report.md` が summary/formatted_findings/findings の
    /// 全文を 3 回以上重複させ、出典も同じ URL を 4 回書いていた）の回帰。実物と同じ構造（`summary` ==
    /// `formatted_findings` == 全文、`findings` にも同じ本文、`sources` に同じ URL が 4 回）の偽の戻り値で:
    /// 本文が 1 回だけ書かれ、`## Summary`/`## Findings` のような区画見出しが付かず、題名は LDR 自身の
    /// `#` 見出しを使い、出典は URL で重複排除されつつ元の引用番号（`[n]`）との対応が保たれる。
    #[test]
    fn runner_report_deduplicates_the_body_and_sources_like_the_real_incident() {
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
        let checker = r###"
import importlib.util, json, os, sys, tempfile
spec = importlib.util.spec_from_file_location("ldr_run", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

full_body = (
    "# Pluvioの隣接領域に関する研究動向と研究テーマ候補\n\n"
    "## 0. 調査の前提と対象\n\n本文の中身はここに詳しく書かれる [1][2]。"
)
result = {
    "summary": full_body,
    "formatted_findings": full_body,
    "findings": [
        {"content": full_body},
        {"content": full_body},
    ],
    "sources": [
        {"link": "https://a.example.com/paper", "title": "Paper A"},
        {"link": "https://a.example.com/paper", "title": "Paper A (dup)"},
        {"link": "https://a.example.com/paper", "title": "Paper A (dup2)"},
        {"link": "https://a.example.com/paper", "title": "Paper A (dup3)"},
        {"link": "https://b.example.org/other", "title": "Other B"},
    ],
}
with tempfile.TemporaryDirectory() as d:
    path = os.path.join(d, "report.md")
    mod.write_report_from_result(path, "この objective は無視され、LDR 自身の見出しが優先される。", result)
    with open(path) as f:
        text = f.read()
print(json.dumps({
    "title": text.splitlines()[0],
    "body_occurrences": text.count("本文の中身はここに詳しく書かれる"),
    "has_summary_heading": "## Summary" in text,
    "has_findings_heading": "## Findings" in text,
    "has_final_synthesis_heading": "## Final synthesis" in text,
    "sources_section": [line for line in text.splitlines() if line.startswith("[")],
}))
"###;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(value["title"], "# Pluvioの隣接領域に関する研究動向と研究テーマ候補");
        assert_eq!(value["body_occurrences"], serde_json::json!(1));
        assert_eq!(value["has_summary_heading"], serde_json::json!(false));
        assert_eq!(value["has_findings_heading"], serde_json::json!(false));
        assert_eq!(value["has_final_synthesis_heading"], serde_json::json!(false));
        assert_eq!(
            value["sources_section"],
            serde_json::json!([
                "[1] Paper A — https://a.example.com/paper",
                "[2] (= [1])",
                "[3] (= [1])",
                "[4] (= [1])",
                "[5] Other B — https://b.example.org/other",
            ])
        );
    }

    /// Phase 32: LDR の統合結果が `#` 見出しで始まらないときは、objective（`query`）の先頭 1 文
    /// （最初の「。」まで、最大 80 字）を題名にする。objective 全文をそのまま見出しにしない
    /// （実機の回帰: 1 行目が objective 丸ごとになっていた）。
    #[test]
    fn runner_report_title_falls_back_to_the_objectives_first_sentence_when_capped() {
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

long_objective = (
    "Pluvio（ad-hoc FSのI/Oサーバ向け非同期ランタイム、IEEE Cluster 2026 Best Paper Finalist）の"
    "隣接領域について直近の研究動向をWeb調査し、次の研究テーマ候補を3〜5件まとめる。"
    "探索範囲は特定の学会・締切に絞らず自由でよいが、Pluvioの非同期I/Oランタイムという資産を活かせる方向を優先すること。"
)
short_objective = "Kubernetesの最新動向を調べる。詳細な補足がここに続くがタイトルには含まれない。"
result = {"summary": "以下にまとめます。中身はここに続く。", "sources": []}

def title_of(objective):
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "report.md")
        mod.write_report_from_result(path, objective, result)
        with open(path) as f:
            return f.read().splitlines()[0]

long_title = title_of(long_objective)
short_title = title_of(short_objective)
print(json.dumps({
    "long_title": long_title,
    "long_title_len": len(long_title) - 2,
    "short_title": short_title,
}))
"##;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        // 長い objective: 最初の「。」より前に 80 字上限に達するので、そこで切り詰められる
        // （objective 全文を見出しにしない。実機の回帰の是正）。
        let long_title = value["long_title"].as_str().expect("long_title string");
        assert!(long_title.starts_with("# Pluvio"), "{long_title}");
        assert!(!long_title.contains("優先すること"), "{long_title} should not include the whole objective");
        let long_title_len = value["long_title_len"].as_u64().expect("long_title_len");
        assert!(long_title_len <= 80, "{long_title_len}");
        // 短い objective: 最初の「。」が 80 字より前にあるので、そこで文が終わる（それ以降の
        // 補足文は含まない）。
        assert_eq!(value["short_title"], "# Kubernetesの最新動向を調べる。");
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

    // --- ADR-0031 D2: 決定的な証拠ゲート ---

    /// 検索が 1 件も返らなかった（`search_results == 0`）ときは別メッセージになり、`report.md` /
    /// `sources.json` / `research.json` は消さずに残る（ADR-0031 D2 / 受け入れ条件 2）。
        /// ADR-0031 D2 の監査指摘（D-5）: `min_search_results = 0` にしていても、検索が 0 件なら
    /// 「検索経路の問題かもしれない」側のメッセージを出す（他の項目でゲートに落ちる場合でも、
    /// 運用者が知りたい原因は同じ）。閾値が全部 0 のときだけゲート自体を見ない。
    #[tokio::test]
    async fn gate_zero_search_results_keeps_the_distinct_message_even_when_that_threshold_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 1, "search_results": 0, "sources": 0, "sources_cited": 0, "unique_domains": 0}"#;
        let mut config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        config.evidence = EvidenceThresholds { min_search_results: 0, min_sources: 3, min_cited: 2, min_domains: 2 };
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-gate-0sr", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("web search returned nothing"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

#[tokio::test]
    async fn gate_zero_search_results_uses_the_distinct_message_and_keeps_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 0, "search_results": 0, "sources": 0, "sources_cited": 0, "unique_domains": 0}"#;
        let config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-gate-1", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("web search returned nothing"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(dir.path().join("artifacts/report.md").exists());
        assert!(dir.path().join("artifacts/sources.json").exists());
        assert!(dir.path().join("artifacts/research.json").exists());
        assert!(!dir.path().join("artifacts/result.json").exists());
        let artifacts = sink.artifacts.lock().unwrap();
        assert_eq!(artifacts.len(), 3, "{artifacts:?}");
    }

    /// 出典が閾値未満（他は満たす）→ 実数と閾値入りのメッセージで retryable。`report.md` は残る。
    #[tokio::test]
    async fn gate_sources_below_minimum_is_retryable_with_actual_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 1, "search_results": 5, "sources": 1, "sources_cited": 2, "unique_domains": 2}"#;
        let config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-gate-2", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.starts_with("insufficient web evidence:"), "{message}");
                assert!(message.contains("sources=1 (min 3)"), "{message}");
                assert!(!message.contains("cited="), "{message}");
                assert!(!message.contains("domains="), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(dir.path().join("artifacts/report.md").exists());
        assert!(!dir.path().join("artifacts/result.json").exists());
    }

    /// 引用数が閾値未満（他は満たす）→ 実数と閾値入りのメッセージで retryable。
    #[tokio::test]
    async fn gate_cited_below_minimum_is_retryable_with_actual_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 1, "search_results": 5, "sources": 3, "sources_cited": 1, "unique_domains": 2}"#;
        let config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-gate-3", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("cited=1 (min 2)"), "{message}");
                assert!(!message.contains("sources="), "{message}");
                assert!(!message.contains("domains="), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(dir.path().join("artifacts/report.md").exists());
    }

    /// 出典が同一ドメインのみ（他は満たす）→ 実数と閾値入りのメッセージで retryable。
    #[tokio::test]
    async fn gate_single_domain_is_retryable_with_actual_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 1, "search_results": 5, "sources": 3, "sources_cited": 2, "unique_domains": 1}"#;
        let config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-gate-4", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("domains=1 (min 2)"), "{message}");
                assert!(!message.contains("sources="), "{message}");
                assert!(!message.contains("cited="), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(dir.path().join("artifacts/report.md").exists());
    }

    /// 閾値を全部 0 にすると、`counts` が全 0 でも従来どおり `done`（受け入れ条件 3）。
    #[tokio::test]
    async fn gate_all_zero_thresholds_still_done_even_with_empty_counts() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 0, "search_results": 0, "sources": 0, "sources_cited": 0, "unique_domains": 0}"#;
        let mut config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        config.evidence = EvidenceThresholds { min_search_results: 0, min_sources: 0, min_cited: 0, min_domains: 0 };
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-gate-5", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }), "{:?}", outcome.terminal);
        assert!(dir.path().join("artifacts/result.json").exists());
    }

    /// `TASKD_RESULT` に `counts` が無い（古いランナー/スタブ）場合は全 0 扱いになるので、既定の閾値
    /// （全部 0 より大きい）では落ちる。
    #[tokio::test]
    async fn gate_missing_counts_is_treated_as_all_zero_and_fails_with_default_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_ldr(dir.path(), &script_with_counts(None));
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-gate-6", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("web search returned nothing"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// `counts` が無くても、閾値を全部 0 にすれば `done`（"全部 0 でないと落ちる" の裏取り）。
    #[tokio::test]
    async fn gate_missing_counts_is_done_when_all_thresholds_are_zero() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_ldr(dir.path(), &script_with_counts(None));
        config.evidence = EvidenceThresholds { min_search_results: 0, min_sources: 0, min_cited: 0, min_domains: 0 };
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-gate-7", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }), "{:?}", outcome.terminal);
    }

    /// ランナーの `build_evidence_manifest`（URL での重複排除、`[n]` からの `cited` 判定、ドメイン数、
    /// 実機（LDR 1.10.7）の形の回帰: `questions` が空でも `findings[].question` から問いを拾い、
    /// 出典のエンジン名は `source` から取る（ADR-0031 D1 の記録が 0 件のままにならないように）。
    #[test]
    fn runner_manifest_uses_findings_questions_and_source_engine() {
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
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("ldr_run", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
result = {
    "summary": "A [1] and B [2].",
    "questions": {},
    "findings": [
        {"question": "what is k8s?"},
        {"question": "what is etcd?"},
        {"question": "what is k8s?"},
    ],
    "sources": [
        {"link": "https://en.wikipedia.org/wiki/Kubernetes", "title": "K8s", "source": "wikipedia"},
        {"link": "https://example.org/etcd", "title": "etcd", "source": "wikipedia"},
    ],
}
sources, research = mod.build_evidence_manifest(result)
print(json.dumps({
    "queries": [q["query"] for q in research["queries"]],
    "counts": research["counts"],
    "engines": [s.get("engine") for s in sources],
}))
"##;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(value["queries"], serde_json::json!(["what is k8s?", "what is etcd?"]));
        assert_eq!(value["counts"]["queries"], serde_json::json!(2));
        assert_eq!(value["counts"]["unique_domains"], serde_json::json!(2));
        assert_eq!(value["engines"], serde_json::json!(["wikipedia", "wikipedia"]));
    }

    /// `questions` が dict/list どちらでも扱えること）を python3 で直接確認する（ADR-0031 D1）。
    #[test]
    fn runner_build_evidence_manifest_dedupes_cites_and_flattens_questions() {
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

result = {
    "summary": "A is confirmed [1]. C is also seen [3].",
    "sources": [
        {"link": "https://a.example.com/x", "title": "A"},
        {"link": "https://b.example.org/y", "title": "B", "engine": "tavily"},
        {"link": "https://a.example.com/x", "title": "A dup"},
    ],
    "questions": {"1": ["q-second"], "0": ["q-first", "q-first-2"]},
    "iterations": 2,
}
sources_list, research = mod.build_evidence_manifest(result)

result_list_questions = dict(result)
result_list_questions["questions"] = [["qa"], "qb"]
_, research_list = mod.build_evidence_manifest(result_list_questions)

print(json.dumps({
    "sources_list": sources_list,
    "research": research,
    "queries_from_list_questions": [q["query"] for q in research_list["queries"]],
}))
"#;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let values: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");

        // 重複排除: 2 件（a.example.com/x は 2 回出るが 1 件に）。`[1]` と `[3]` は両方 a.example.com/x を指す
        // ので cited=true、`b.example.org/y` は引用されていないので cited=false。
        assert_eq!(
            values["sources_list"],
            serde_json::json!([
                {"url": "https://a.example.com/x", "title": "A", "engine": null, "cited": true},
                {"url": "https://b.example.org/y", "title": "B", "engine": "tavily", "cited": false},
            ])
        );
        assert_eq!(values["research"]["iterations"], 2);
        assert_eq!(values["research"]["counts"]["search_results"], 3);
        assert_eq!(values["research"]["counts"]["sources"], 2);
        assert_eq!(values["research"]["counts"]["sources_cited"], 1);
        assert_eq!(values["research"]["counts"]["unique_domains"], 2);
        // `questions` が dict のときは反復順（キーの数値昇順）で並ぶ。
        assert_eq!(
            values["research"]["queries"],
            serde_json::json!([
                {"query": "q-first", "engine": null, "result_count": null},
                {"query": "q-first-2", "engine": null, "result_count": null},
                {"query": "q-second", "engine": null, "result_count": null},
            ])
        );
        // `questions` が list（要素がリストまたは文字列）のときも同じように平らにする。
        assert_eq!(values["queries_from_list_questions"], serde_json::json!(["qa", "qb"]));
    }
}
