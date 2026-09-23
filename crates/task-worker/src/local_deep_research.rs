//! `local-deep-research` アダプタ（DESIGN §5.4, ADR-0029 D1）。
//!
//! Local Deep Research（LDR）は `paperqa`（ADR-0027 D3）と同じ「調査エンジンを包む」形。LDR には
//! 一発実行の CLI が無く（`ldr-web`/`ldr-mcp` は常駐プロセス）、プログラム的な API
//! （`local_deep_research.api.{quick_summary,detailed_research,generate_report}`）だけがある。
//! そのため celeris 側が実行用の Python スクリプトを持ち（`include_str!`）、run ごとに
//! `runs/<run_id>/ldr_run.py` として書き出して `<command> <その場所> <run_dir>/ldr_input.json` で起動する。
//! celeris の外に置くファイルは venv（`command` が指す python）だけで、スクリプト自体は celeris のバイナリと
//! 一緒に版が進む。
//!
//! ワーカープロトコル（`artifacts/result.json`）は PaperQA2 アダプタと同じく**アダプタが代わりに書く**
//! （ADR-0006 D3 の規約は保つ）。委譲（`delegate.json`）は扱わない（ADR-0029 D1: 「委譲はしない」）。

use std::path::Path;
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
use crate::progress;
use crate::protocol::{Answer, RunContext, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, kill_now, read_line_limited, read_tail, reap_after_terminal,
    write_result_json,
};

/// run ごとに `runs/<run_id>/ldr_run.py` として書き出す本体（ADR-0029 D1）。
const RUNNER_SCRIPT: &str = include_str!("local_deep_research_run.py");

/// `artifacts/result.json` の `summary` の上限（`paperqa` と同じ規則。ADR-0029 D1）。
const SUMMARY_MAX_CHARS: usize = 1500;
/// `progress:` 行を `progress` に転送するときの 1 行あたりの上限。
const PROGRESS_LINE_MAX_CHARS: usize = 500;
/// ランナーの最終行の目印（ADR-0029 D1）。
const RESULT_PREFIX: &str = "CELERIS_RESULT ";
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
/// （このアダプタ）が `CELERIS_RESULT` の `counts` を見て機械的に判定する（LLM に判断させない）。
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
    /// ADR-0063 Phase 109b B4（`task_worker::PaperQaEvidence::insufficient_is_error` と同じ考え方）:
    /// 検索が 0 件（検索経路の問題）は、この設定に関わらず常に hard error。それ以外の閾値未達
    /// （`min_sources`/`min_cited`/`min_domains`）を hard error にするか（既定 `false`）。`false`
    /// （既定）なら証拠不足でも `Terminal::Done` にし、`report.md` の「## 証拠の質」節に内訳を書いて
    /// reviewer / 受け入れ条件の判断に委ねる。`true` にすると Phase 109 までどおり
    /// `Terminal::Error{retryable: true}`。
    #[serde(default)]
    pub insufficient_is_error: bool,
}

impl Default for EvidenceThresholds {
    fn default() -> Self {
        Self {
            min_search_results: default_min_search_results(),
            min_sources: default_min_sources(),
            min_cited: default_min_cited(),
            min_domains: default_min_domains(),
            insufficient_is_error: false,
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

/// `[adapters.local_deep_research]`（config.toml, ADR-0029 D1）。`[[providers]] adapter =
/// "local-deep-research"` の行ごとに `model`（= `settings` の `llm.model` を上書き）と `env` を上書きできる
/// （`paperqa`/`acp` と同じ作り）。行の `settings` の上書きは無い（`ProviderConfig.settings` は `paperqa` 専用
/// のフィールドで、LDR では再利用しない。celeris 側の実装判断）。
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
    /// ADR-0063 D2（Phase 109）: `attempts >= 1`（前回が reviewer 不合格）の run で使う `mode`
    /// （既定 `Detailed`）。
    pub retry_mode: LdrMode,
    /// ADR-0063 D2: 同じく再挑戦の run で使う `iterations`（既定 `Some(5)`）。`mode`/`iterations` の
    /// 通常値より優先する。
    pub retry_iterations: Option<u32>,
    /// ADR-0063 Phase 109c B3: 目的文から対象（`research_targets`）が取れたとき、LDR の答えと必読の
    /// 一次情報の抜粋を材料に、プロキシの LLM（LDR と同じ `settings` の `llm.model` /
    /// `llm.openai_endpoint`）で対象×観点の表と対象ごとの節を合成し、`report.md` の先頭に置く
    /// （既定 `true`）。`false` にすると Phase 109b までどおり LDR の生の findings だけ。
    pub structured_synthesis: bool,
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
            retry_mode: LdrMode::Detailed,
            retry_iterations: Some(5),
            structured_synthesis: true,
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
    /// 同名キーは `extra` が勝つ（celeris の環境 < アダプタの環境 < `with_env` の追加分）。
    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.env.extend(extra.iter().cloned());
        Some(Arc::new(LdrAdapter::new(config)))
    }
}

/// ADR-0063 D2（Phase 109）: 前回 reviewer に不合格にされた条件の理由（`context.prior_review` で
/// `pass = false` のもの）。空欄・空白だけの理由は落とす。
fn must_cover_items(prior_review: &[crate::protocol::PriorReview]) -> Vec<String> {
    prior_review
        .iter()
        .filter(|p| !p.pass)
        .map(|p| p.reason.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect()
}

/// ADR-0063 Phase 109c B1: 「前回からの改善点」節。空なら空文字（従来どおりの問いのまま）。
/// Phase 109 まではこの節を問いの**先頭**に置いていたが、本番観測（2026-09-23）で LDR が
/// この節だけに検索・合成を引きずられ、目的文の他の対象を落とすことが分かった。以後は
/// `build_query` が目的文の**後ろ**に置く（置き換えない）。
fn build_must_cover_section(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut out = String::from("## 前回からの改善点（必ず埋める）\n");
    for item in items {
        out.push_str(&format!("- {item}\n"));
    }
    out.trim_end().to_string()
}

/// テキストの中の `http(s)://` で始まるトークンを全て拾う（`paperqa.rs::extract_urls` と同じ決定的な
/// トークナイズ。正規表現は使わない）。
fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in text.split(|c: char| {
        c.is_whitespace() || matches!(c, '「' | '」' | '（' | '）' | '(' | ')' | '<' | '>' | '"' | '\'' | '　')
    }) {
        let trimmed = token.trim_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?'));
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// ADR-0063 D2: 必読の一次情報の URL。目的文中の URL に加え、知識ベースの索引
/// （`context.knowledge.index`）のうち `primary-sources` / `一次情報` タグを持つページの
/// `sources`（前置きの出典。`celerisctl knowledge record --source` で人が付けたもの）を拾う。
/// 重複は落とし、出現順を保つ（決定的。LLM は使わない）。
pub fn must_read_urls(
    objective: &str,
    knowledge: Option<&crate::protocol::KnowledgeContext>,
) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for url in extract_urls(objective) {
        if seen.insert(url.clone()) {
            out.push(url);
        }
    }
    if let Some(k) = knowledge {
        for item in &k.index {
            let is_primary = item.tags.iter().any(|t| {
                let t = t.to_ascii_lowercase();
                t.contains("primary-source") || t.contains("一次情報")
            });
            if !is_primary {
                continue;
            }
            for url in &item.sources {
                // ADR-0063 Phase 109b B1: 知識ベースの `sources` は前置きの出典（人が
                // `celerisctl knowledge record --source` で付けたもの）で、`human` のような
                // URL でない値が混じることがある（本番の観測、2026-09-23）。`http(s)://` で
                // 始まるものだけを拾う。
                let is_url = url.starts_with("http://") || url.starts_with("https://");
                if is_url && seen.insert(url.clone()) {
                    out.push(url.clone());
                }
            }
        }
    }
    out
}

/// LDR に渡す**問い**を組み立てる。
///
/// 実機で分かったこと（2026-09-17）: ここにタスクのタイトルの見出し（`# ...`）や役割の指示文まで入れると、
/// 検索エンジンがその文字列ごと検索して**何も返さない**（同じ問いを素で投げれば出典が取れる）。
/// LDR は受け取った問いをそのまま検索にも使うので、**素の目的だけ**を渡す。
/// 人間の回答履歴は短い補足として後ろに付ける（検索語としての邪魔が少ない）。
/// 役割の指示文とタイトルは `runs/<run_id>/request.json` に残るので記録は失われない。
///
/// ADR-0063 D2（Phase 109）: 前回 reviewer に不合格にされた run（`context.prior_review` に
/// `pass = false` の条件がある）では、その理由を「前回からの改善点」として問いに足す。
///
/// ADR-0063 Phase 109c B1（本番観測 2026-09-23）: この節を目的文の**先頭**に置くと、LDR がそこだけに
/// 検索・合成を引きずられ、目的文が挙げる他の対象を落とすことが分かった（BeeOND のみの報告になった
/// 事故）。以後は**目的文をそのまま先に置き、置き換えない**。前回の `report.md`（`is_retry` のときだけ、
/// 先頭 20 KB）があれば、それを改善する材料として続けて足す。
pub fn build_query(task: &Task, context: &RunContext, prior_report: Option<&str>) -> String {
    // ADR-0029 / Phase 19 / ADR-0033 D6（Phase 27 の監査 M-2）: 検索ハーネスに渡すのは**素の目的だけ**。
    // 役職・記憶・直近のやり取り・記憶の書式指示は載せない（問いを濁すと検索が何も返さない）。
    let mut out = String::new();
    out.push_str(task.objective.trim());
    let must_cover = build_must_cover_section(&must_cover_items(&context.prior_review));
    if !must_cover.is_empty() {
        out.push_str("\n\n");
        out.push_str(&must_cover);
    }
    if let Some(report) = prior_report {
        let trimmed = report.trim();
        if !trimmed.is_empty() {
            out.push_str("\n\n## 前回の報告（これを改善する。削らない）\n");
            out.push_str(trimmed);
        }
    }
    if !context.answers.is_empty() {
        out.push_str("\n\n補足（人間の回答）:");
        for Answer { question, answer } in &context.answers {
            out.push_str(&format!("\n- {question} → {answer}"));
        }
    }
    out
}

/// ADR-0063 D4（Phase 109）: `settings` の値のうち秘密らしいもの（キーが `api_key` / `token` /
/// `password` / `secret` で終わる）は `ldr_input.json` に平文で書かない。実際の値は、LDR 自身が
/// `LDR_` + 設定キーの大文字化（`.` は `_`）で環境変数から読む規則（ADR-0031 の実測）に沿った名前の
/// 環境変数として子プロセスに渡し、JSON にはその環境変数名へのプレースホルダ（`"<env:NAME>"`）だけを
/// 書く。戻り値は `(JSON に書く settings, 子プロセスへ追加する env)`。
fn redact_secret_settings(
    settings: &std::collections::BTreeMap<String, String>,
) -> (
    std::collections::BTreeMap<String, String>,
    Vec<(String, String)>,
) {
    fn is_secret_key(key: &str) -> bool {
        let lower = key.to_ascii_lowercase();
        ["api_key", "token", "password", "secret"]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
    }
    fn env_name(key: &str) -> String {
        format!("LDR_{}", key.to_ascii_uppercase().replace('.', "_"))
    }
    let mut redacted = std::collections::BTreeMap::new();
    let mut extra_env = Vec::new();
    for (key, value) in settings {
        if is_secret_key(key) && !value.is_empty() {
            let name = env_name(key);
            redacted.insert(key.clone(), format!("<env:{name}>"));
            extra_env.push((name, value.clone()));
        } else {
            redacted.insert(key.clone(), value.clone());
        }
    }
    (redacted, extra_env)
}

/// ADR-0063 Phase 109c B1: 前回の run の `report.md`（先頭 20 KB。文字境界で安全に切る）を読む。
/// 無い・空・読めない場合は `None`（run は止めない。あくまで再挑戦の材料）。
const PRIOR_REPORT_MAX_BYTES: usize = 20 * 1024;

async fn read_prior_report(report_path: &Path) -> Option<String> {
    let text = tokio::fs::read_to_string(report_path).await.ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= PRIOR_REPORT_MAX_BYTES {
        return Some(trimmed.to_string());
    }
    let mut end = PRIOR_REPORT_MAX_BYTES;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!("{}\n\n…（以下省略）", &trimmed[..end]))
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

    // ADR-0036 D1/D2: 成果物の置き場はディスパッチャが決めた `artifacts_dir`（共有 workspace ではタスクごと）。
    let artifacts_dir = req.artifacts_dir.clone();
    let artifacts_rel = req.artifacts_rel();
    tokio::fs::create_dir_all(&artifacts_dir).await?;
    let report_path = artifacts_dir.join("report.md");
    // ADR-0063 D2（Phase 109）: 前回 reviewer に不合格にされた run（`attempts >= 1`）は、mode /
    // iterations を強く（既定 `detailed` / 5 周）する。
    let is_retry = req.task.attempts >= 1;
    // ADR-0063 Phase 109c B1: 前回の run の `report.md` は、消す前に読んでおく（再挑戦のときだけ。
    // 「削らない」ための材料として問いに足す）。
    let prior_report = if is_retry {
        read_prior_report(&report_path).await
    } else {
        None
    };
    // 前回の run（リトライ）の名残を今回の結果と誤読しない（paperqa/claude_code/codex と同じ理由。ADR-0006 D3）。
    let _ = tokio::fs::remove_file(&report_path).await;
    let _ = tokio::fs::remove_file(artifacts_dir.join("result.json")).await;

    let query = build_query(&req.task, &req.context, prior_report.as_deref());
    // ADR-0023 D2 / M1: この run で何を渡したかを残す。
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &query, run_id).await;
    let mode = if is_retry { config.retry_mode } else { config.mode };
    let iterations = if is_retry {
        config.retry_iterations.or(config.iterations)
    } else {
        config.iterations
    };
    if is_retry {
        progress::emit_status(
            sink,
            &format!(
                "retrying (attempt {}); escalating to mode={} iterations={:?}",
                req.task.attempts + 1,
                mode.as_str(),
                iterations
            ),
        );
    }

    // ADR-0063 D2: 必読の一次情報（目的文中の URL + 知識ベースの primary-sources / 一次情報 タグ）。
    // `ldr_run.py` が LDR の検索の前に直接 fetch して sources に含める。
    let must_read = must_read_urls(&req.task.objective, req.context.knowledge.as_ref());
    if !must_read.is_empty() {
        progress::emit_status(
            sink,
            &format!("{} must-read primary source(s)", must_read.len()),
        );
    }

    // `model` は `settings` の `llm.model` より優先する（ADR-0029 D1）。`BTreeMap` で決定的な順序にする。
    let mut settings: std::collections::BTreeMap<String, String> =
        config.settings.iter().cloned().collect();
    if let Some(model) = &config.model {
        settings.insert("llm.model".to_string(), model.clone());
    }
    // ADR-0063 D4（Phase 109）: 秘密らしい値（`api_key`/`token`/`password`/`secret` で終わるキー）は
    // `ldr_input.json` に平文で書かない。実際の値は子プロセスの環境変数として渡し、JSON にはその
    // 環境変数名へのプレースホルダだけを書く（`ldr_run.py::convert_setting_value` が解決する）。
    let (json_settings, secret_env) = redact_secret_settings(&settings);

    // ADR-0063 Phase 109c A/B3: 目的文から取れた対象・観点。`targets` が空なら `ldr_run.py` は
    // 構造化合成をせず、従来どおり LDR の生の findings だけを report.md に書く。
    let targets = crate::research_targets::research_targets(&req.task.objective);
    let aspects = crate::research_targets::research_aspects(&req.task.objective);
    if !targets.is_empty() {
        progress::emit_status(
            sink,
            &format!(
                "{} target(s), {} aspect(s) for structured synthesis: {}",
                targets.len(),
                aspects.len(),
                targets.join(" / ")
            ),
        );
    }

    let input = serde_json::json!({
        "query": query,
        "mode": mode.as_str(),
        "settings": json_settings,
        "iterations": iterations,
        "questions_per_iteration": config.questions_per_iteration,
        "report_path": report_path.to_string_lossy(),
        "must_read_urls": must_read,
        "targets": targets,
        "aspects": aspects,
        "structured_synthesis": config.structured_synthesis,
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
        .envs(secret_env)
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
                // ランナーの出力形式は celeris が定義したものではないので寛容に無視する（paperqa と同じ考え方）。
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

    let report_len = tokio::fs::metadata(&report_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let (terminal, provider_failure) = if !exit_status.success() {
        let exit_repr = match exit_status.code() {
            Some(code) => code.to_string(),
            None => "signal".to_string(),
        };
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: format!(
                    "local-deep-research runner exited with a non-zero status (exit={exit_repr})"
                ),
                retryable: true,
            },
            pf,
        )
    } else if task_result.is_none() {
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: "local-deep-research runner did not print a CELERIS_RESULT line"
                    .to_string(),
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
        let raw_summary = value
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let summary = single_line_summary(raw_summary, SUMMARY_MAX_CHARS);

        // ADR-0031 D1: `report.md` に加えて、ランナーが機械的に作った証拠の記録
        // （`sources.json` / `research.json`）も成果物として申告する。ゲート（下）に落ちても
        // **消さずに残す**（D2: 人が読めるように）ので、この申告はゲートの判定より前に行う。
        // ADR-0036 D4: 申告する `path` は workspace 相対のまま（`artifacts_dir` 基準で組む）。
        for (name, rel_path, kind) in [
            (
                "report.md",
                format!("{artifacts_rel}/report.md"),
                "markdown",
            ),
            (
                "sources.json",
                format!("{artifacts_rel}/sources.json"),
                "json",
            ),
            (
                "research.json",
                format!("{artifacts_rel}/research.json"),
                "json",
            ),
        ] {
            match crate::artifact::resolve(&req.workspace, name, &rel_path, Some(kind)) {
                Ok(artifact) => sink.artifact(&artifact),
                Err(e) => warn!("run {run_id}: could not register {rel_path}: {e}"),
            }
        }

        // ADR-0031 D2: 決定的な証拠ゲート。`CELERIS_RESULT` の `counts` を見る（古いランナー/スタブで
        // 無ければ全 0 扱い＝閾値を全部 0 にしないと落ちる）。LLM には判断させない。
        let counts = value.get("counts");
        let count_of = |key: &str| -> u32 {
            counts
                .and_then(|c| c.get(key))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as u32
        };
        let search_results = count_of("search_results");
        let sources = count_of("sources");
        let sources_cited = count_of("sources_cited");
        let unique_domains = count_of("unique_domains");
        let ev = &config.evidence;

        // どれか 1 つでも閾値が立っているか（全部 0 なら ADR-0031 D2 どおりゲートを見ない）。
        let gate_enabled = ev.min_search_results > 0
            || ev.min_sources > 0
            || ev.min_cited > 0
            || ev.min_domains > 0;
        // 検索経路そのものの問題（鍵切れ・CAPTCHA・ネットワーク遮断）を、調べた結果情報が無かった
        // ケースと区別できるように、別メッセージにする（ADR-0031 D2）。`min_search_results = 0` に
        // していてもこの区別は要る（他の項目で落ちるので、運用者が原因を知りたいのは同じ）。これは
        // `insufficient_is_error` に関わらず常に hard error（ADR-0063 Phase 109b B4。PaperQA の
        // 「取得 0 件」と同じ考え方）。
        let zero_search_results = gate_enabled && search_results == 0;
        let mut problems = Vec::new();
        if !zero_search_results {
            if ev.min_search_results > 0 && search_results < ev.min_search_results {
                problems.push(format!(
                    "search_results={search_results} (min {})",
                    ev.min_search_results
                ));
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
        }
        let insufficient = zero_search_results || !problems.is_empty();

        // ADR-0063 Phase 109b B4: 閾値未達（`search_results == 0` を除く）は、`insufficient_is_error`
        // （既定 false）が true のときだけ hard error。既定では `Terminal::Done` にし、`report.md` の
        // 「## 証拠の質」節に内訳を書いて reviewer / 受け入れ条件の判断に委ねる。
        let hard_error_message = if zero_search_results {
            Some(
                "web search returned nothing (possible search path failure: expired key, CAPTCHA, or network block)"
                    .to_string(),
            )
        } else if !problems.is_empty() && ev.insufficient_is_error {
            Some(format!(
                "insufficient web evidence: {}",
                problems.join(", ")
            ))
        } else {
            None
        };

        // ADR-0063 Phase 109b B4: ゲートを見た run では常に「## 証拠の質」節を足す（合否に関わらず。
        // hard error でも report.md 自体は残るので、人が読めるようにしておく）。
        if gate_enabled {
            append_evidence_quality_section(
                &report_path,
                sources,
                sources_cited,
                unique_domains,
                &problems,
                insufficient,
                run_id,
            )
            .await;
        }

        if let Some(message) = hard_error_message {
            // ADR-0031 D2: retryable な `Terminal::Error`。供給側の失敗（`AdapterError`）にはしない
            // （プロバイダを cooldown にする話ではない）ので `provider_failure` は `None` のまま。
            (
                Terminal::Error {
                    message,
                    retryable: true,
                },
                None,
            )
        } else {
            let result_file = serde_json::json!({ "summary": summary, "evidence": [] });
            match serde_json::to_string_pretty(&result_file) {
                Ok(text) => {
                    if let Err(e) =
                        tokio::fs::write(artifacts_dir.join("result.json"), format!("{text}\n"))
                            .await
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

/// `sources.json`（`report_path` の隣。ランナーが書く）から `primary: true` の件数と、そのうち
/// `cited: true` の件数を数える（ADR-0063 Phase 109b B1/B4: 必読の一次情報が実際に report に
/// 反映されたか）。読めない・壊れていれば `(0, 0)`（この節は補足情報であって、失敗しても run は
/// 止めない）。
async fn read_primary_source_counts(report_path: &Path) -> (u32, u32) {
    let Some(dir) = report_path.parent() else {
        return (0, 0);
    };
    let Ok(text) = tokio::fs::read_to_string(dir.join("sources.json")).await else {
        return (0, 0);
    };
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str::<serde_json::Value>(&text)
    else {
        return (0, 0);
    };
    let mut total = 0u32;
    let mut cited = 0u32;
    for entry in &entries {
        if entry.get("primary").and_then(serde_json::Value::as_bool) == Some(true) {
            total += 1;
            if entry.get("cited").and_then(serde_json::Value::as_bool) == Some(true) {
                cited += 1;
            }
        }
    }
    (total, cited)
}

/// `report.md` の末尾に「## 証拠の質」節を足す（ADR-0063 Phase 109b B4。決定的の証拠ゲートを見た
/// run では常に。PaperQA の `render_evidence_section` と同じ考え方）。出典/引用/ドメイン数と、
/// 必読の一次情報のうち report に反映された件数、証拠不足ならその理由を書く。合否は reviewer に
/// 委ねる（`insufficient` でも run 自体は Done になり得る）。読み書きに失敗しても run は止めない。
#[allow(clippy::too_many_arguments)]
async fn append_evidence_quality_section(
    report_path: &Path,
    sources: u32,
    sources_cited: u32,
    unique_domains: u32,
    problems: &[String],
    insufficient: bool,
    run_id: &str,
) {
    let (primary_total, primary_cited) = read_primary_source_counts(report_path).await;
    let mut section = String::from("\n\n## 証拠の質\n\n");
    section.push_str(&format!(
        "- 出典: {sources} 件（引用: {sources_cited} 件、異なるドメイン: {unique_domains} 件）\n"
    ));
    if primary_total > 0 {
        section.push_str(&format!(
            "- 必読の一次情報: {primary_total} 件中 {primary_cited} 件が report に反映（引用）された\n"
        ));
    }
    if insufficient {
        let reason = if problems.is_empty() {
            "web search returned nothing".to_string()
        } else {
            problems.join(", ")
        };
        section.push_str(&format!(
            "- 証拠不足（{reason}）。合否は reviewer の判断に委ねる。\n"
        ));
    }
    match tokio::fs::read_to_string(report_path).await {
        Ok(mut body) => {
            body.push_str(&section);
            if let Err(e) = tokio::fs::write(report_path, body).await {
                warn!(
                    "run {run_id}: could not append the evidence-quality section to report.md: {e}"
                );
            }
        }
        Err(e) => warn!(
            "run {run_id}: could not read report.md to append the evidence-quality section: {e}"
        ),
    }
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
        /// ADR-0048 D2（Phase 60a）: 構造化した進行（このアダプタは `status` だけ）。
        structured: Mutex<Vec<(String, task_core::ProgressFields)>>,
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
        fn progress_with(&self, msg: &str, fields: &task_core::ProgressFields) {
            self.progress(msg);
            self.structured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((msg.to_string(), fields.clone()));
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

    /// counts が満たす閾値（既定: min_search_results=5, min_sources=3, min_cited=2, min_domains=2。ADR-0031 D2）。
    const PASSING_COUNTS: &str = r#"{"queries": 1, "search_results": 5, "sources": 3, "sources_cited": 2, "unique_domains": 3}"#;

    /// スタブは argv[2]（`ldr_input.json` のパス）に成功時の `report.md`/`sources.json`/`research.json` を
    /// 書き、progress と `CELERIS_RESULT`（`counts` 込み）を出す（実際のランナーの動きを最小限まねる。ADR-0031 D1）。
    /// `counts_json` が `None` なら `CELERIS_RESULT` に `counts` を含めない（古いランナー/スタブの再現）。
    fn script_with_counts(counts_json: Option<&str>) -> String {
        let counts = counts_json.unwrap_or(r#"{"queries": 0, "search_results": 0, "sources": 0, "sources_cited": 0, "unique_domains": 0}"#);
        let result_line = match counts_json {
            Some(counts) => format!(
                r#"CELERIS_RESULT {{"summary": "found X and Y with sources", "sources": 3, "counts": {counts}}}"#
            ),
            None => r#"CELERIS_RESULT {"summary": "found X and Y with sources", "sources": 3}"#
                .to_string(),
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
        let outcome = adapter
            .run(req.clone(), "run-1", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done {
                summary,
                evidence,
                usage,
            } => {
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
        // ADR-0048 D2（Phase 60a）: このアダプタが出せる進行は節目（`status`）だけで、
        // すべての行が構造化されている（`msg` は従来どおり）。
        let structured = sink.structured.lock().unwrap().clone();
        assert_eq!(structured.len(), progress.len(), "{structured:#?}");
        assert!(
            structured
                .iter()
                .all(|(_, f)| f.kind == Some(task_core::ProgressKind::Status)),
            "{structured:#?}"
        );
        assert!(
            structured.iter().all(|(_, f)| f.summary.is_some()),
            "{structured:#?}"
        );

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
        let research_artifact = artifacts
            .iter()
            .find(|a| a.name == "research.json")
            .unwrap();
        assert_eq!(research_artifact.path, "artifacts/research.json");
        drop(artifacts);

        let sources_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/sources.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(sources_json.as_array().unwrap().len(), 3);
        let research_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/research.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(research_json["counts"]["sources"], 3);

        let report_md = std::fs::read_to_string(dir.path().join("artifacts/report.md")).unwrap();
        assert!(report_md.contains("found X and Y with sources"));

        let result_json =
            std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result_json).unwrap();
        assert_eq!(parsed["summary"], "found X and Y with sources");
        assert_eq!(parsed["evidence"], serde_json::json!([]));

        assert!(dir.path().join("runs/run-1/stdout.log").is_file());
        assert!(dir.path().join("runs/run-1/stderr.log").is_file());
        assert!(dir.path().join("runs/run-1/ldr_run.py").is_file());
        assert!(dir.path().join("runs/run-1/ldr_input.json").is_file());
        let run_result =
            std::fs::read_to_string(dir.path().join("runs/run-1/result.json")).unwrap();
        match serde_json::from_str::<crate::protocol::WorkerMessage>(run_result.trim()).unwrap() {
            crate::protocol::WorkerMessage::Done { summary, .. } => {
                assert_eq!(summary, "found X and Y with sources")
            }
            other => panic!("expected done in runs/<run_id>/result.json, got {other:?}"),
        }
    }

    /// ADR-0036 D1/D2/D4: 共有 workspace のタスクでは、ランナーに渡す `report_path` も、申告する成果物の
    /// `path` も、結果ファイルもタスクごとの `.taskd/artifacts/<task_id>/` になる（実機の事故: 兄弟の
    /// PaperQA2 の `sources.json` を LDR が上書きした）。
    #[tokio::test]
    async fn a_shared_workspace_task_writes_under_its_own_artifacts_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_ldr(dir.path(), &success_script());
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(dir.path().join("artifacts/sources.json"), "sibling").unwrap();
        let adapter = LdrAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.artifacts_dir = dir.path().join(".taskd/artifacts/T1");
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-shared", default_limits(), &sink)
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { .. }),
            "{:?}",
            outcome.terminal
        );
        let artifacts = sink.artifacts.lock().unwrap();
        let mut paths: Vec<&str> = artifacts.iter().map(|a| a.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![
                ".taskd/artifacts/T1/report.md",
                ".taskd/artifacts/T1/research.json",
                ".taskd/artifacts/T1/sources.json",
            ]
        );
        drop(artifacts);
        assert!(dir.path().join(".taskd/artifacts/T1/result.json").is_file());
        // 兄弟の `artifacts/sources.json` は触らない（実機の上書き事故の再発防止）。
        assert_eq!(
            std::fs::read_to_string(dir.path().join("artifacts/sources.json")).unwrap(),
            "sibling"
        );
    }

    #[tokio::test]
    async fn non_zero_exit_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_ldr(dir.path(), "cat >/dev/null; echo 'boom' 1>&2; exit 7");
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-2", default_limits(), &sink)
            .await
            .unwrap();
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
    async fn missing_celeris_result_line_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        // report.md は書くが CELERIS_RESULT を出さずに終わる。
        let config = stub_ldr(
            dir.path(),
            "mkdir -p artifacts && echo hi > artifacts/report.md\necho 'progress: working'\n",
        );
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-3", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("CELERIS_RESULT"), "{message}");
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
            "mkdir -p artifacts && : > artifacts/report.md\necho 'CELERIS_RESULT {\"summary\": \"x\", \"sources\": 0}'\n",
        );
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-4", default_limits(), &sink)
            .await
            .unwrap();
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
        let pid_text = std::fs::read_to_string(&pid_file)
            .expect("stub should have recorded its pid before looping");
        let pid: i32 = pid_text
            .trim()
            .parse()
            .expect("pid.txt should contain a pid");
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "process {pid} should have been killed"
        );
    }

    fn read_json(path: &Path) -> serde_json::Value {
        let text = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn python3_available() -> bool {
        match std::process::Command::new("python3").arg("--version").output() {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
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
        let query = build_query(&task, &context, None);
        assert_eq!(query, "What is Kubernetes and what problem does it solve?");
        assert!(!query.contains("gate pass check"), "{query}");
        assert!(!query.contains("Web 調査担当"), "{query}");
        assert!(!query.contains("関連研究調査課"), "{query}");
        assert!(!query.contains("pjsub"), "{query}");
        assert!(!query.contains("先週の続きを"), "{query}");
        assert!(!query.contains("覚えておくこと"), "{query}");

        context.answers = vec![Answer {
            question: "対象は?".into(),
            answer: "v1.31".into(),
        }];
        let with_answers = build_query(&task, &context, None);
        assert!(
            with_answers.starts_with("What is Kubernetes"),
            "{with_answers}"
        );
        assert!(with_answers.contains("対象は? → v1.31"), "{with_answers}");
    }

    /// ADR-0063 Phase 109c B1（本番観測 2026-09-23）: 前回不合格になった条件の理由が「前回からの
    /// 改善点」として**目的文の後ろ**に付く（先頭に置いて目的文を実質置き換えると、LDR がそこだけに
    /// 引きずられ他の対象を落とす事故があった）。合格した条件（`pass = true`）や空の理由は載せない。
    #[test]
    fn build_query_appends_a_must_cover_section_after_the_objective_from_a_failed_prior_review() {
        let mut task = crate::protocol::tests::sample_task();
        task.objective = "CHFS と FinchFS を比べよ".into();
        let context = RunContext {
            prior_review: vec![
                crate::protocol::PriorReview {
                    criterion: 0,
                    pass: false,
                    reason: "CHFS の一次情報（GitHub）が無い".into(),
                },
                crate::protocol::PriorReview {
                    criterion: 1,
                    pass: true,
                    reason: "満たしている".into(),
                },
                crate::protocol::PriorReview {
                    criterion: 2,
                    pass: false,
                    reason: "  ".into(),
                },
            ],
            ..Default::default()
        };
        let query = build_query(&task, &context, None);
        assert!(query.starts_with("CHFS と FinchFS を比べよ"), "{query}");
        assert!(query.contains("## 前回からの改善点（必ず埋める）"), "{query}");
        assert!(query.contains("CHFS の一次情報（GitHub）が無い"), "{query}");
        assert!(!query.contains("満たしている"), "{query}");

        // prior_review が全部合格、または空なら従来どおり素の目的だけ。
        let all_passed = RunContext {
            prior_review: vec![crate::protocol::PriorReview {
                criterion: 0,
                pass: true,
                reason: "ok".into(),
            }],
            ..Default::default()
        };
        assert_eq!(build_query(&task, &all_passed, None), task.objective);
    }

    /// ADR-0063 Phase 109c B1: 再挑戦のときの前回の報告（先頭 20 KB）は「これを改善する。削らない」
    /// 節として目的文の後ろに足す。前回の報告が無ければこの節は付かない。
    #[test]
    fn build_query_appends_the_previous_report_when_given_one() {
        let mut task = crate::protocol::tests::sample_task();
        task.objective = "CHFS を調べよ".into();
        let context = RunContext::default();

        let without_prior = build_query(&task, &context, None);
        assert_eq!(without_prior, "CHFS を調べよ");

        let with_prior = build_query(&task, &context, Some("# 前回の報告\n本文の抜粋"));
        assert!(with_prior.starts_with("CHFS を調べよ"), "{with_prior}");
        assert!(
            with_prior.contains("## 前回の報告（これを改善する。削らない）"),
            "{with_prior}"
        );
        assert!(with_prior.contains("本文の抜粋"), "{with_prior}");
    }

    /// ADR-0063 D2: 必読の一次情報 = 目的文中の URL + 知識ベースの `primary-sources` / `一次情報`
    /// タグを持つページの `sources`（そのタグを持たないページや通常の URL 以外は拾わない）。
    #[test]
    fn must_read_urls_collects_objective_urls_and_primary_source_tagged_knowledge_sources() {
        let objective = "CHFS（https://github.com/otatebe/chfs）と一般的な比較資料を調べる";
        assert_eq!(
            must_read_urls(objective, None),
            vec!["https://github.com/otatebe/chfs".to_string()]
        );

        let knowledge = crate::protocol::KnowledgeContext {
            mounts: vec![],
            index: vec![
                task_core::KnowledgeItem {
                    path: "projects/benchfs/primary-sources.md".into(),
                    title: "一次情報".into(),
                    tags: vec!["primary-sources".into()],
                    scope: None,
                    sources: vec![
                        "https://github.com/tsukuba-hpcs/finchfs".into(),
                        // 目的文の URL と重複するものは 1 回だけ。
                        "https://github.com/otatebe/chfs".into(),
                        // ADR-0063 Phase 109b B1: URL でない前置きの出典（本番の観測、
                        // 2026-09-23: `sources: ["human", ...]`）は捨てる。
                        "human".into(),
                    ],
                    updated: None,
                    confidence: None,
                },
                task_core::KnowledgeItem {
                    path: "environment/notes.md".into(),
                    title: "雑記".into(),
                    tags: vec!["environment".into()],
                    scope: None,
                    sources: vec!["https://example.org/should-not-appear".into()],
                    updated: None,
                    confidence: None,
                },
            ],
        };
        let urls = must_read_urls(objective, Some(&knowledge));
        assert_eq!(
            urls,
            vec![
                "https://github.com/otatebe/chfs".to_string(),
                "https://github.com/tsukuba-hpcs/finchfs".to_string(),
            ],
            "{urls:?}"
        );
    }

    /// ADR-0063 D4: 秘密らしいキー（`api_key`/`token`/`password`/`secret` で終わる）は
    /// `LDR_<KEY>` の環境変数名に変換され、JSON にはプレースホルダだけが残る。それ以外の設定はそのまま。
    #[test]
    fn redact_secret_settings_replaces_secret_looking_values_with_env_placeholders() {
        let mut settings = std::collections::BTreeMap::new();
        settings.insert(
            "llm.openai_endpoint.api_key".to_string(),
            "sk-should-not-leak".to_string(),
        );
        settings.insert("llm.provider".to_string(), "openai_endpoint".to_string());
        settings.insert(
            "search.engine.web.tavily.api_key".to_string(),
            "tvly-secret".to_string(),
        );
        let (redacted, env) = redact_secret_settings(&settings);
        assert_eq!(
            redacted["llm.openai_endpoint.api_key"],
            "<env:LDR_LLM_OPENAI_ENDPOINT_API_KEY>"
        );
        assert_eq!(
            redacted["search.engine.web.tavily.api_key"],
            "<env:LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY>"
        );
        assert_eq!(redacted["llm.provider"], "openai_endpoint", "秘密でない値はそのまま");
        let env_map: std::collections::BTreeMap<&str, &str> = env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            env_map["LDR_LLM_OPENAI_ENDPOINT_API_KEY"],
            "sk-should-not-leak"
        );
        assert_eq!(
            env_map["LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY"],
            "tvly-secret"
        );
        assert_eq!(env.len(), 2, "秘密でない設定は env に足さない");
    }

    /// ADR-0063 D2: `attempts >= 1` の run は `mode`/`iterations` を再挑戦用の値に上げる。
    /// `ldr_input.json` にも `must_read_urls` が入る。秘密（`api_key`）は JSON に平文で残らない。
    #[tokio::test]
    async fn retry_run_escalates_mode_and_iterations_and_redacts_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_ldr(
            dir.path(),
            "cp \"$2\" \"$(dirname \"$0\")/seen_input.json\"\n\
             mkdir -p artifacts && echo hi > artifacts/report.md\n\
             echo 'CELERIS_RESULT {\"summary\": \"ok\", \"sources\": 0}'\n",
        );
        config.settings = vec![(
            "llm.openai_endpoint.api_key".to_string(),
            "sk-should-not-leak".to_string(),
        )];
        config.evidence = EvidenceThresholds {
            min_search_results: 0,
            min_sources: 0,
            min_cited: 0,
            min_domains: 0,
            insufficient_is_error: false,
        };
        let adapter = LdrAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.task.objective = "CHFS（https://github.com/otatebe/chfs）を調べる".to_string();
        req.task.attempts = 1;
        req.context.prior_review.push(crate::protocol::PriorReview {
            criterion: 0,
            pass: false,
            reason: "CHFS の一次情報が出典に無い".to_string(),
        });
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-retry", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));

        let input_path = dir.path().join("seen_input.json");
        let input_text = std::fs::read_to_string(&input_path).unwrap();
        let seen: serde_json::Value = serde_json::from_str(&input_text).unwrap();
        assert_eq!(seen["mode"], "detailed");
        assert_eq!(seen["iterations"], 5);
        assert_eq!(
            seen["must_read_urls"],
            serde_json::json!(["https://github.com/otatebe/chfs"])
        );
        let query = seen["query"].as_str().unwrap();
        // ADR-0063 Phase 109c B1: 目的文を置き換えず、後ろに「前回からの改善点」を足す。
        assert!(
            query.starts_with("CHFS（https://github.com/otatebe/chfs）を調べる"),
            "{seen}"
        );
        assert!(query.contains("## 前回からの改善点（必ず埋める）"), "{seen}");
        assert!(query.contains("CHFS の一次情報が出典に無い"), "{seen}");
        assert_eq!(
            seen["settings"]["llm.openai_endpoint.api_key"],
            "<env:LDR_LLM_OPENAI_ENDPOINT_API_KEY>"
        );
        assert!(
            !input_text.contains("sk-should-not-leak"),
            "秘密が ldr_input.json に平文で残ってはいけない: {input_text}"
        );
        let progress = sink.progress.lock().unwrap().join("\n");
        assert!(progress.contains("retrying (attempt 2)"), "{progress}");
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
             echo 'CELERIS_RESULT {\"summary\": \"ok\", \"sources\": 0}'\n",
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
        // （スタブの CELERIS_RESULT に counts が無い）。
        config.evidence = EvidenceThresholds {
            min_search_results: 0,
            min_sources: 0,
            min_cited: 0,
            min_domains: 0,
            insufficient_is_error: false,
        };
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req.clone(), "run-6", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));

        let seen = read_json(&dir.path().join("seen_input.json"));
        assert_eq!(
            seen["query"],
            serde_json::Value::String(build_query(&req.task, &req.context, None))
        );
        assert_eq!(seen["mode"], "detailed");
        assert_eq!(seen["iterations"], 3);
        assert_eq!(seen["questions_per_iteration"], 2);
        assert_eq!(seen["settings"]["llm.provider"], "openai_endpoint");
        assert_eq!(seen["settings"]["search.tool"], "searxng");
        // `model` が `settings` の `llm.model` を上書きする。
        assert_eq!(seen["settings"]["llm.model"], "qwen3.8-27b");
        assert!(
            seen["report_path"]
                .as_str()
                .unwrap()
                .ends_with("artifacts/report.md")
        );

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
             echo 'CELERIS_RESULT {\"summary\": \"ok\", \"sources\": 0}'\n",
        );
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        adapter
            .run(req, "run-7", default_limits(), &sink)
            .await
            .unwrap();
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
                 echo 'CELERIS_RESULT {{\"summary\": \"ok\", \"sources\": 0}}'\n",
                out = out_file.display()
            ),
        );
        config
            .env
            .push(("OPENAI_BASE_URL".to_string(), "http://old:1".to_string()));
        // このテストは環境変数の上書きを見るだけで、証拠ゲート（ADR-0031 D2）とは無関係なので無効にする
        // （スタブの CELERIS_RESULT に counts が無い）。
        config.evidence = EvidenceThresholds {
            min_search_results: 0,
            min_sources: 0,
            min_cited: 0,
            min_domains: 0,
            insufficient_is_error: false,
        };
        let base = LdrAdapter::new(config);
        let with_env = base
            .with_env(&[("OPENAI_BASE_URL".to_string(), "http://new:2".to_string())])
            .expect("local-deep-research supports with_env");

        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = with_env
            .run(req, "run-8", default_limits(), &sink)
            .await
            .unwrap();
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
        let Ok(python) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        else {
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let values: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(values, serde_json::json!(["# Title", "# plain question"]));
    }

    /// Phase 32: 実機で起きたレビュー不合格（`report.md` が summary/formatted_findings/findings の
    /// 全文を 3 回以上重複させ、出典も同じ URL を 4 回書いていた）の回帰。実物と同じ構造（`summary` ==
    /// `formatted_findings` == 全文、`findings` にも同じ本文、`sources` に同じ URL が 4 回）の偽の戻り値で:
    /// 本文が 1 回だけ書かれ、`## Summary`/`## Findings` のような区画見出しが付かず、題名は LDR 自身の
    /// `#` 見出しを使い、出典は URL で重複排除されつつ元の引用番号（`[n]`）との対応が保たれる。
    #[test]
    fn runner_report_deduplicates_the_body_and_sources_like_the_real_incident() {
        let Ok(python) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        else {
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(
            value["title"],
            "# Pluvioの隣接領域に関する研究動向と研究テーマ候補"
        );
        assert_eq!(value["body_occurrences"], serde_json::json!(1));
        assert_eq!(value["has_summary_heading"], serde_json::json!(false));
        assert_eq!(value["has_findings_heading"], serde_json::json!(false));
        assert_eq!(
            value["has_final_synthesis_heading"],
            serde_json::json!(false)
        );
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
        let Ok(python) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        else {
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        // 長い objective: 最初の「。」より前に 80 字上限に達するので、そこで切り詰められる
        // （objective 全文を見出しにしない。実機の回帰の是正）。
        let long_title = value["long_title"].as_str().expect("long_title string");
        assert!(long_title.starts_with("# Pluvio"), "{long_title}");
        assert!(
            !long_title.contains("優先すること"),
            "{long_title} should not include the whole objective"
        );
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
        let Ok(python) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        else {
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let values: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
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

    /// ADR-0063 D4（Phase 109）: `convert_setting_value` は `"<env:...>"` プレースホルダを環境変数
    /// から解決してから型変換する。未設定なら空文字（値としては安全側）。
    #[test]
    fn runner_convert_setting_value_resolves_env_placeholders() {
        let Ok(python) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        else {
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
import importlib.util, json, os, sys
spec = importlib.util.spec_from_file_location("ldr_run", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
os.environ["CELERIS_TEST_LDR_KEY"] = "real-secret-value"
print(json.dumps([
    mod.convert_setting_value("<env:CELERIS_TEST_LDR_KEY>"),
    mod.convert_setting_value("<env:CELERIS_TEST_LDR_MISSING>"),
    mod.resolve_env_placeholder("plain-string"),
    mod.resolve_env_placeholder(42),
]))
"#;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let values: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(
            values,
            serde_json::json!(["real-secret-value", "", "plain-string", 42])
        );
    }

    /// ADR-0063 D2/D3（Phase 109）: 必読の一次情報を `sources` の末尾に足す（既存の URL は重複させ
    /// ない。既存の `[n]` 引用番号を崩さないよう先頭には差し込まない）。`fetch_title` は注入するので
    /// ネットワークには出ない。
    #[test]
    fn runner_add_must_read_sources_appends_new_urls_without_touching_existing_citation_numbers() {
        let Ok(python) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        else {
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
result = {"summary": "A [1].", "sources": [{"link": "https://a.example.com/x", "title": "A"}]}
added = mod.add_must_read_sources(
    result,
    ["https://github.com/otatebe/chfs", "https://a.example.com/x", ""],
    lambda u: "T:" + u,
)
print(json.dumps({"added": added, "sources": result["sources"]}))
"#;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(v["added"], 1, "既存の URL と空文字は数えない: {v}");
        let sources = v["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2, "{v}");
        assert_eq!(sources[0]["link"], "https://a.example.com/x", "元の [1] のまま先頭: {v}");
        assert_eq!(sources[1]["link"], "https://github.com/otatebe/chfs", "{v}");
        assert_eq!(sources[1]["title"], "T:https://github.com/otatebe/chfs", "{v}");
    }

    /// ADR-0063 Phase 109b B1: `add_must_read_sources` は python 側でも `http(s)://` 以外
    /// （`human` のような前置きの出典）を捨て、足した各エントリに `primary: true` を付ける。
    #[test]
    fn runner_add_must_read_sources_drops_non_urls_and_marks_primary() {
        if !python3_available() {
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
result = {"summary": "no citations", "sources": []}
added = mod.add_must_read_sources(result, ["human", "https://github.com/otatebe/chfs"], lambda u: u)
print(json.dumps({"added": added, "sources": result["sources"], "is_http_url": [mod.is_http_url("human"), mod.is_http_url("https://x")]}))
"#;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(v["added"], 1, "human は URL でないので足さない: {v}");
        let sources = v["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1, "{v}");
        assert_eq!(sources[0]["link"], "https://github.com/otatebe/chfs");
        assert_eq!(sources[0]["primary"], true, "{v}");
        assert_eq!(v["is_http_url"], serde_json::json!([false, true]));
    }

    /// ADR-0063 Phase 109b B1: 必読の一次情報の本文抜粋 — GitHub のリポジトリ URL は README の raw
    /// テキストを、それ以外は HTML → テキストに変換して取る（`fetch` は注入するのでネットワークに
    /// 出ない）。取得に失敗した URL は `fetch_error` を残して続行する。
    #[test]
    fn runner_builds_primary_source_excerpts_from_readme_and_html() {
        if !python3_available() {
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

def fetch(url, timeout):
    if url == "https://raw.githubusercontent.com/otatebe/chfs/HEAD/README.md":
        return b"# CHFS\n\nA persistent memory ad-hoc file system for HPC clusters."
    if url == "https://example.org/paper":
        return b"<html><head><title>A Paper</title></head><body><script>bad()</script><p>Hello &amp; world</p></body></html>"
    if url == "https://broken.example/x":
        raise OSError("boom")
    raise AssertionError("unexpected url: " + url)

urls = [
    "https://github.com/otatebe/chfs",
    "https://example.org/paper",
    "https://broken.example/x",
    "human",
    "https://github.com/otatebe/chfs",
]
section, entries = mod.build_primary_source_entries(urls, fetch=fetch)
print(json.dumps({
    "section": section,
    "entries": entries,
    "readme_url": mod.github_readme_url("https://github.com/otatebe/chfs"),
    "readme_url_subpath": mod.github_readme_url("https://github.com/otatebe/chfs/issues/1"),
    "gitlab_readme_url": mod.github_readme_url("https://gitlab.com/foo/bar"),
    "html_to_text": mod.html_to_text("<p>Hello <b>world</b></p><script>evil()</script>"),
}))
"##;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        let entries = v["entries"].as_array().unwrap();
        // `human` は捨てる。重複した chfs は 1 回だけ。
        assert_eq!(entries.len(), 3, "{v}");
        assert_eq!(entries[0]["link"], "https://github.com/otatebe/chfs");
        assert!(
            entries[0]["excerpt"]
                .as_str()
                .unwrap()
                .contains("persistent memory"),
            "{v}"
        );
        assert_eq!(entries[0]["primary"], true);
        assert!(entries[0]["fetch_error"].is_null(), "{v}");
        assert_eq!(entries[1]["link"], "https://example.org/paper");
        assert_eq!(entries[1]["title"], "A Paper");
        assert!(
            entries[1]["excerpt"].as_str().unwrap().contains("Hello & world"),
            "script は落ち、実体参照は戻る: {v}"
        );
        assert!(
            !entries[1]["excerpt"].as_str().unwrap().contains("bad()"),
            "{v}"
        );
        assert_eq!(entries[2]["link"], "https://broken.example/x");
        assert!(entries[2]["fetch_error"].as_str().unwrap().contains("boom"), "{v}");
        assert_eq!(entries[2]["excerpt"], "");

        let section = v["section"].as_str().unwrap();
        assert!(section.starts_with("## 必読の一次情報（本文抜粋）"), "{section}");
        assert!(section.contains("CHFS"), "{section}");
        assert!(section.contains("取得失敗: OSError: boom"), "{section}");

        assert_eq!(
            v["readme_url"],
            "https://raw.githubusercontent.com/otatebe/chfs/HEAD/README.md"
        );
        assert!(v["readme_url_subpath"].is_null(), "サブパスは README にしない: {v}");
        assert_eq!(
            v["gitlab_readme_url"],
            "https://gitlab.com/foo/bar/-/raw/HEAD/README.md"
        );
        assert_eq!(v["html_to_text"], "Hello world");
    }

    /// ADR-0063 Phase 109b B1: 必読の一次情報の抜粋が最終的な report の本文に現れれば `cited` になる
    /// （URL でも一致する。`apply_primary_source_citations` が `sources_list`/`counts` を書き換える）。
    #[test]
    fn runner_excerpt_citation_marks_primary_sources_used_in_the_body() {
        if !python3_available() {
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

out = {}
out["not_cited_when_absent"] = mod.excerpt_is_cited("https://x", "a line that is long enough to count as a real match", "totally unrelated body")
out["cited_by_excerpt_line"] = mod.excerpt_is_cited("https://x", "CHFS uses node-local persistent memory for burst buffering", "The system, CHFS uses node-local persistent memory for burst buffering, is fast.")
out["cited_by_url"] = mod.excerpt_is_cited("https://github.com/otatebe/chfs", "short", "see https://github.com/otatebe/chfs for the implementation")
out["too_short_to_count"] = mod.excerpt_is_cited("https://x", "short line", "a body that happens to contain short line too")

sources_list = [
    {"url": "https://a.example.com/1", "title": "A", "engine": None, "cited": True},
    {"url": "https://github.com/otatebe/chfs", "title": "CHFS", "engine": None, "cited": False, "primary": True},
    {"url": "https://github.com/tsukuba-hpcs/finchfs", "title": "FinchFS", "engine": None, "cited": False, "primary": True},
]
research = {"queries": [], "iterations": 0, "counts": {"sources_cited": 1}}
primary_excerpts = {
    "https://github.com/otatebe/chfs": "CHFS is a persistent-memory ad-hoc file system for HPC.",
    "https://github.com/tsukuba-hpcs/finchfs": "FinchFS is a different system entirely.",
}
body = "This report discusses: CHFS is a persistent-memory ad-hoc file system for HPC. That is all."
mod.apply_primary_source_citations(sources_list, research, primary_excerpts, body)
out["sources_list"] = sources_list
out["sources_cited_count"] = research["counts"]["sources_cited"]
print(json.dumps(out))
"#;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(v["not_cited_when_absent"], false, "{v}");
        assert_eq!(v["cited_by_excerpt_line"], true, "{v}");
        assert_eq!(v["cited_by_url"], true, "{v}");
        assert_eq!(v["too_short_to_count"], false, "{v}");

        let sources = v["sources_list"].as_array().unwrap();
        assert_eq!(sources[0]["cited"], true, "既に true のものはそのまま: {v}");
        assert_eq!(sources[1]["cited"], true, "CHFS の抜粋が本文に現れる: {v}");
        assert_eq!(
            sources[2]["cited"], false,
            "FinchFS の抜粋は本文に現れない: {v}"
        );
        assert_eq!(v["sources_cited_count"], 2, "{v}");
    }

    /// ADR-0063 Phase 109b B3: `detect_upstream_llm_error` はプロキシのエラー文面をそれと見抜き、
    /// 普通の答えは通す。`run_with_retries`/`call_ldr_stage` は例外・偽装エラーのどちらも
    /// バックオフ付きで再試行し、最終的に成功すれば返し、尽きれば最後のエラーを伝える。
    #[test]
    fn runner_upstream_llm_error_detection_and_retry_are_deterministic() {
        if !python3_available() {
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

out = {}
out["real_answer_is_not_an_error"] = mod.detect_upstream_llm_error("CHFS uses persistent memory.") is None
out["503_body_is_detected"] = mod.detect_upstream_llm_error(
    "Error: Error code: 503 - {'error': {'message': 'no reachable llm source is configured for this model', 'type': 'no_source_available'}}"
) is not None
out["no_source_available_is_detected"] = mod.detect_upstream_llm_error("something something no_source_available") is not None

# run_with_retries: 2 回失敗してから成功。
calls = {"n": 0}
sleeps = []
def flaky():
    calls["n"] += 1
    if calls["n"] < 3:
        raise mod.UpstreamLlmError("boom %d" % calls["n"])
    return "ok"
out["retry_result"] = mod.run_with_retries(flaky, sleep=sleeps.append)
out["retry_attempts"] = calls["n"]
out["retry_sleeps"] = sleeps

# 尽きれば最後のエラーが伝播する。
def always_fails():
    raise mod.UpstreamLlmError("always boom")
try:
    mod.run_with_retries(always_fails, sleep=lambda s: None)
    out["exhausted_raises"] = False
except mod.UpstreamLlmError as exc:
    out["exhausted_raises"] = True
    out["exhausted_message"] = str(exc)

# call_ldr_stage: 例外はそのまま UpstreamLlmError になる。
try:
    mod.call_ldr_stage(lambda: (_ for _ in ()).throw(RuntimeError("network down")), lambda r: "")
    out["stage_exception_wrapped"] = False
except mod.UpstreamLlmError as exc:
    out["stage_exception_wrapped"] = "network down" in str(exc)

# call_ldr_stage: 偽装エラー（成功したように見えて実は 503 の文面）は on_bad_result を呼んでから raise。
cleanup_calls = []
try:
    mod.call_ldr_stage(
        lambda: {"summary": "Error: Error code: 503 - boom"},
        lambda r: r["summary"],
        on_bad_result=lambda r: cleanup_calls.append(r),
    )
    out["disguised_error_raises"] = False
except mod.UpstreamLlmError:
    out["disguised_error_raises"] = True
out["disguised_error_cleanup_called"] = len(cleanup_calls) == 1

# call_ldr_stage: 普通の結果はそのまま返る。
out["stage_passthrough"] = mod.call_ldr_stage(lambda: {"summary": "a real answer"}, lambda r: r["summary"])

print(json.dumps(out))
"#;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(v["real_answer_is_not_an_error"], true, "{v}");
        assert_eq!(v["503_body_is_detected"], true, "{v}");
        assert_eq!(v["no_source_available_is_detected"], true, "{v}");
        assert_eq!(v["retry_result"], "ok");
        assert_eq!(v["retry_attempts"], 3);
        assert_eq!(v["retry_sleeps"], serde_json::json!([2.0, 4.0]));
        assert_eq!(v["exhausted_raises"], true, "{v}");
        assert_eq!(v["exhausted_message"], "always boom");
        assert_eq!(v["stage_exception_wrapped"], true, "{v}");
        assert_eq!(v["disguised_error_raises"], true, "{v}");
        assert_eq!(v["disguised_error_cleanup_called"], true, "{v}");
        assert_eq!(v["stage_passthrough"], serde_json::json!({"summary": "a real answer"}));
    }

    // --- ADR-0031 D2: 決定的な証拠ゲート ---

    /// 検索が 1 件も返らなかった（`search_results == 0`）ときは別メッセージになり、`report.md` /
    /// `sources.json` / `research.json` は消さずに残る（ADR-0031 D2 / 受け入れ条件 2）。
    /// ADR-0031 D2 の監査指摘（D-5）: `min_search_results = 0` にしていても、検索が 0 件なら
    /// 「検索経路の問題かもしれない」側のメッセージを出す（他の項目でゲートに落ちる場合でも、
    /// 運用者が知りたい原因は同じ）。閾値が全部 0 のときだけゲート自体を見ない。
    #[tokio::test]
    async fn gate_zero_search_results_keeps_the_distinct_message_even_when_that_threshold_is_zero()
    {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 1, "search_results": 0, "sources": 0, "sources_cited": 0, "unique_domains": 0}"#;
        let mut config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        config.evidence = EvidenceThresholds {
            min_search_results: 0,
            min_sources: 3,
            min_cited: 2,
            min_domains: 2,
            insufficient_is_error: false,
        };
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-gate-0sr", default_limits(), &sink)
            .await
            .unwrap();
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
        let outcome = adapter
            .run(req, "run-gate-1", default_limits(), &sink)
            .await
            .unwrap();
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

    /// 出典が閾値未満（他は満たす）→ `insufficient_is_error = true` なら実数と閾値入りのメッセージで
    /// retryable。`report.md` は残る（ADR-0063 Phase 109b B4: 既定は soft。この Phase 108 までの
    /// 挙動は明示的に有効化して確かめる）。
    #[tokio::test]
    async fn gate_sources_below_minimum_is_retryable_with_actual_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 1, "search_results": 5, "sources": 1, "sources_cited": 2, "unique_domains": 2}"#;
        let mut config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        config.evidence.insufficient_is_error = true;
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-gate-2", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(
                    message.starts_with("insufficient web evidence:"),
                    "{message}"
                );
                assert!(message.contains("sources=1 (min 3)"), "{message}");
                assert!(!message.contains("cited="), "{message}");
                assert!(!message.contains("domains="), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(dir.path().join("artifacts/report.md").exists());
        assert!(!dir.path().join("artifacts/result.json").exists());
    }

    /// 引用数が閾値未満（他は満たす）→ `insufficient_is_error = true` なら実数と閾値入りのメッセージで
    /// retryable（ADR-0063 Phase 109b B4）。
    #[tokio::test]
    async fn gate_cited_below_minimum_is_retryable_with_actual_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 1, "search_results": 5, "sources": 3, "sources_cited": 1, "unique_domains": 2}"#;
        let mut config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        config.evidence.insufficient_is_error = true;
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-gate-3", default_limits(), &sink)
            .await
            .unwrap();
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

    /// 出典が同一ドメインのみ（他は満たす）→ `insufficient_is_error = true` なら実数と閾値入りの
    /// メッセージで retryable（ADR-0063 Phase 109b B4）。
    #[tokio::test]
    async fn gate_single_domain_is_retryable_with_actual_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 1, "search_results": 5, "sources": 3, "sources_cited": 2, "unique_domains": 1}"#;
        let mut config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        config.evidence.insufficient_is_error = true;
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-gate-4", default_limits(), &sink)
            .await
            .unwrap();
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

    /// ADR-0063 Phase 109b B4: `insufficient_is_error` の既定 `false` では、閾値未達でも
    /// `Terminal::Done` になり、`report.md` の末尾に「## 証拠の質」節（出典/引用/ドメイン数、証拠
    /// 不足の理由）が付く。合否は reviewer に委ねる。
    #[tokio::test]
    async fn gate_insufficient_evidence_defaults_to_done_with_an_evidence_quality_section() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 1, "search_results": 5, "sources": 1, "sources_cited": 1, "unique_domains": 1}"#;
        let config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-gate-soft", default_limits(), &sink)
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { .. }),
            "{:?}",
            outcome.terminal
        );
        assert!(dir.path().join("artifacts/result.json").exists());
        let report_md = std::fs::read_to_string(dir.path().join("artifacts/report.md")).unwrap();
        assert!(report_md.contains("## 証拠の質"), "{report_md}");
        assert!(report_md.contains("証拠不足"), "{report_md}");
        assert!(report_md.contains("sources=1 (min 3)"), "{report_md}");
        assert!(report_md.contains("cited=1 (min 2)"), "{report_md}");
        assert!(report_md.contains("domains=1 (min 2)"), "{report_md}");
    }

    /// 閾値を全部 0 にすると、`counts` が全 0 でも従来どおり `done`（受け入れ条件 3）。
    #[tokio::test]
    async fn gate_all_zero_thresholds_still_done_even_with_empty_counts() {
        let dir = tempfile::tempdir().unwrap();
        let counts = r#"{"queries": 0, "search_results": 0, "sources": 0, "sources_cited": 0, "unique_domains": 0}"#;
        let mut config = stub_ldr(dir.path(), &script_with_counts(Some(counts)));
        config.evidence = EvidenceThresholds {
            min_search_results: 0,
            min_sources: 0,
            min_cited: 0,
            min_domains: 0,
            insufficient_is_error: false,
        };
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-gate-5", default_limits(), &sink)
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { .. }),
            "{:?}",
            outcome.terminal
        );
        assert!(dir.path().join("artifacts/result.json").exists());
    }

    /// `CELERIS_RESULT` に `counts` が無い（古いランナー/スタブ）場合は全 0 扱いになるので、既定の閾値
    /// （全部 0 より大きい）では落ちる。
    #[tokio::test]
    async fn gate_missing_counts_is_treated_as_all_zero_and_fails_with_default_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_ldr(dir.path(), &script_with_counts(None));
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-gate-6", default_limits(), &sink)
            .await
            .unwrap();
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
        config.evidence = EvidenceThresholds {
            min_search_results: 0,
            min_sources: 0,
            min_cited: 0,
            min_domains: 0,
            insufficient_is_error: false,
        };
        let adapter = LdrAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-gate-7", default_limits(), &sink)
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { .. }),
            "{:?}",
            outcome.terminal
        );
    }

    /// ランナーの `build_evidence_manifest`（URL での重複排除、`[n]` からの `cited` 判定、ドメイン数、
    /// 実機（LDR 1.10.7）の形の回帰: `questions` が空でも `findings[].question` から問いを拾い、
    /// 出典のエンジン名は `source` から取る（ADR-0031 D1 の記録が 0 件のままにならないように）。
    #[test]
    fn runner_manifest_uses_findings_questions_and_source_engine() {
        let Ok(python) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        else {
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(
            value["queries"],
            serde_json::json!(["what is k8s?", "what is etcd?"])
        );
        assert_eq!(value["counts"]["queries"], serde_json::json!(2));
        assert_eq!(value["counts"]["unique_domains"], serde_json::json!(2));
        assert_eq!(
            value["engines"],
            serde_json::json!(["wikipedia", "wikipedia"])
        );
    }

    /// `questions` が dict/list どちらでも扱えること）を python3 で直接確認する（ADR-0031 D1）。
    #[test]
    fn runner_build_evidence_manifest_dedupes_cites_and_flattens_questions() {
        let Ok(python) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        else {
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let values: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");

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
        assert_eq!(
            values["queries_from_list_questions"],
            serde_json::json!(["qa", "qb"])
        );
    }

    /// 実機の不具合（本番、2026-09-18）の回帰: `detailed_research` は `settings_override` という
    /// 引数を持たず（`quick_summary`/`generate_report` と違う）、渡した設定は `**kwargs` に落ちて
    /// 無視され、`llm.model` が未設定のまま "Ollama model not configured" で落ちていた。実機
    /// （LDR 1.10.7）で確かめた通り、正しい渡し方は `settings_snapshot=create_settings_snapshot(overrides=settings)`。
    /// このテストは偽の `local_deep_research` パッケージを `sys.modules` に入れてランナーの `main()` を
    /// 実際に呼び、`detailed` モードでは `create_settings_snapshot` が `[adapters.local_deep_research].settings`
    /// を `overrides` として受け取り、その戻り値が `detailed_research` に `settings_snapshot` として渡ること
    /// （`settings_override` としては渡らないこと）を検証する。ADR-0029 参照。
    #[test]
    fn runner_detailed_mode_passes_settings_via_settings_snapshot() {
        let Ok(python) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        else {
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
import contextlib, importlib.util, io, json, os, sys, tempfile, types

spec = importlib.util.spec_from_file_location("ldr_run", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

calls = {}

def fake_create_settings_snapshot(overrides=None, base_settings=None, **kwargs):
    calls["create_settings_snapshot_overrides"] = overrides
    return {"snapshot": True, "from_overrides": overrides}

def fake_detailed_research(query, **kwargs):
    calls["detailed_research_kwargs"] = kwargs
    return {"summary": "s", "sources": [], "findings": [], "iterations": 1, "questions": {}}

def fake_quick_summary(*a, **kw):
    raise AssertionError("quick_summary must not be called for mode=detailed")

def fake_generate_report(*a, **kw):
    raise AssertionError("generate_report must not be called for mode=detailed")

fake_api = types.ModuleType("local_deep_research.api")
fake_api.detailed_research = fake_detailed_research
fake_api.quick_summary = fake_quick_summary
fake_api.generate_report = fake_generate_report
fake_api.create_settings_snapshot = fake_create_settings_snapshot
fake_pkg = types.ModuleType("local_deep_research")
fake_pkg.api = fake_api
sys.modules["local_deep_research"] = fake_pkg
sys.modules["local_deep_research.api"] = fake_api

with tempfile.TemporaryDirectory() as d:
    report_path = os.path.join(d, "artifacts", "report.md")
    input_path = os.path.join(d, "input.json")
    payload = {
        "query": "what is the capital of France?",
        "mode": "detailed",
        "settings": {"llm.provider": "openai_endpoint", "llm.model": "qwen3.8-27b"},
        "iterations": 1,
        "questions_per_iteration": 1,
        "report_path": report_path,
    }
    with open(input_path, "w") as f:
        json.dump(payload, f)
    sys.argv = ["local_deep_research_run.py", input_path]
    # `main()` prints its own progress/`CELERIS_RESULT` lines to stdout; swallow those so
    # only the checker's own JSON summary line reaches the Rust test's stdout capture.
    with contextlib.redirect_stdout(io.StringIO()):
        rc = mod.main()

kwargs = calls.get("detailed_research_kwargs", {})
print(json.dumps({
    "rc": rc,
    "overrides_passed_to_snapshot": calls.get("create_settings_snapshot_overrides"),
    "has_settings_snapshot_kwarg": "settings_snapshot" in kwargs,
    "has_settings_override_kwarg": "settings_override" in kwargs,
    "settings_snapshot_value": kwargs.get("settings_snapshot"),
    "has_iterations_kwarg": "iterations" in kwargs,
    "has_questions_per_iteration_kwarg": "questions_per_iteration" in kwargs,
}))
"#;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let values: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(values["rc"], serde_json::json!(0));
        // ADR-0063 Phase 109b B2: `fake_detailed_research(query, **kwargs)` declares neither
        // `iterations` nor `questions_per_iteration` by name (exactly the real bug: they would be
        // silently swallowed by `**kwargs`), so `iteration_setting_overrides` routes both into the
        // settings snapshot instead of passing them as direct kwargs.
        assert_eq!(
            values["overrides_passed_to_snapshot"],
            serde_json::json!({
                "llm.provider": "openai_endpoint",
                "llm.model": "qwen3.8-27b",
                "search.iterations": 1,
                "search.questions_per_iteration": 1,
            })
        );
        assert_eq!(
            values["has_settings_snapshot_kwarg"],
            serde_json::json!(true)
        );
        assert_eq!(
            values["has_settings_override_kwarg"],
            serde_json::json!(false)
        );
        assert_eq!(values["has_iterations_kwarg"], serde_json::json!(false));
        assert_eq!(
            values["has_questions_per_iteration_kwarg"],
            serde_json::json!(false)
        );
        assert_eq!(
            values["settings_snapshot_value"],
            serde_json::json!({"snapshot": true, "from_overrides": {
                "llm.provider": "openai_endpoint",
                "llm.model": "qwen3.8-27b",
                "search.iterations": 1,
                "search.questions_per_iteration": 1,
            }})
        );
    }

    /// ADR-0063 Phase 109b B2: `detailed_research` が `iterations`/`questions_per_iteration` を
    /// 実際に名前付き引数として宣言していれば、そちらへ直接渡る（設定へは回さない）。
    #[test]
    fn runner_iteration_kwargs_go_direct_when_the_function_actually_declares_them() {
        let Ok(python) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        else {
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

def declares_both(query, iterations=None, questions_per_iteration=None, **kwargs):
    return None

def declares_neither(query, **kwargs):
    return None

overrides_a, direct_a = mod.iteration_setting_overrides(declares_both, 5, 2)
overrides_b, direct_b = mod.iteration_setting_overrides(declares_neither, 5, 2)
overrides_c, direct_c = mod.iteration_setting_overrides(declares_both, None, None)

print(json.dumps({
    "declares_both": {"overrides": overrides_a, "direct": dict(direct_a)},
    "declares_neither": {"overrides": overrides_b, "direct": dict(direct_b)},
    "both_none": {"overrides": overrides_c, "direct": dict(direct_c)},
}))
"#;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(
            v["declares_both"],
            serde_json::json!({
                "overrides": {},
                "direct": {"iterations": 5, "questions_per_iteration": 2},
            }),
            "{v}"
        );
        assert_eq!(
            v["declares_neither"],
            serde_json::json!({
                "overrides": {"search.iterations": 5, "search.questions_per_iteration": 2},
                "direct": {},
            }),
            "{v}"
        );
        assert_eq!(
            v["both_none"],
            serde_json::json!({"overrides": {}, "direct": {}}),
            "None は両方とも渡さない: {v}"
        );
    }

    /// エンドツーエンド（`main()` を偽の `local_deep_research` で実際に呼ぶ）: ADR-0063 Phase 109b
    /// B1 + B3。1 回目の呼び出しは llm-proxy の 503 が答えに化けた結果（`detect_upstream_llm_error`
    /// が見抜く）を返し、`run_with_retries`（`time.sleep` を注入して実時間は待たない）が 1 回だけ
    /// 待って再試行、2 回目で本物の答えが返る。必読の一次情報（GitHub の README）は問いに追記され、
    /// その抜粋が答えの本文に現れるので `sources.json` で `cited: true` になる。
    #[test]
    fn main_retries_a_disguised_upstream_error_and_marks_the_must_read_source_cited() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("local_deep_research_run.py");
        std::fs::write(&script_path, RUNNER_SCRIPT).unwrap();
        let checker = r###"
import contextlib, importlib.util, io, json, os, sys, tempfile, types

spec = importlib.util.spec_from_file_location("ldr_run", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

# `run_with_retries`/`build_primary_source_entries` resolve their `sleep`/`fetch` at call time
# (ADR-0063 Phase 109b), so `main()` -- which never passes either -- honors these fakes.
sleep_calls = []
mod.time.sleep = lambda s: sleep_calls.append(s)
mod.default_primary_source_fetch = lambda url, timeout: b"# CHFS\n\nCHFS is a persistent-memory ad-hoc file system for HPC."

calls = {"n": 0}
captured_queries = []

def fake_quick_summary(query, **kwargs):
    calls["n"] += 1
    captured_queries.append(query)
    if calls["n"] == 1:
        return {
            "summary": "Error: Error code: 503 - {'error': {'message': "
                       "'no reachable llm source is configured for this model', "
                       "'type': 'no_source_available'}}",
            "sources": [],
        }
    return {
        "summary": "CHFS is a persistent-memory ad-hoc file system for HPC. [1]",
        "sources": [{"link": "https://example.org/blog", "title": "Blog", "engine": "tavily"}],
        "findings": [],
        "iterations": 1,
        "questions": {},
    }

fake_api = types.ModuleType("local_deep_research.api")
fake_api.quick_summary = fake_quick_summary
fake_api.detailed_research = lambda *a, **k: (_ for _ in ()).throw(AssertionError("not used"))
fake_api.generate_report = lambda *a, **k: (_ for _ in ()).throw(AssertionError("not used"))
fake_api.create_settings_snapshot = lambda **k: {}
fake_pkg = types.ModuleType("local_deep_research")
fake_pkg.api = fake_api
sys.modules["local_deep_research"] = fake_pkg
sys.modules["local_deep_research.api"] = fake_api

with tempfile.TemporaryDirectory() as d:
    report_path = os.path.join(d, "artifacts", "report.md")
    input_path = os.path.join(d, "input.json")
    payload = {
        "query": "CHFS について調べる",
        "mode": "quick",
        "settings": {},
        "report_path": report_path,
        "must_read_urls": ["https://github.com/otatebe/chfs", "human"],
    }
    with open(input_path, "w") as f:
        json.dump(payload, f)
    sys.argv = ["local_deep_research_run.py", input_path]
    with contextlib.redirect_stdout(io.StringIO()) as captured_stdout:
        rc = mod.main()
    stdout_text = captured_stdout.getvalue()
    report_text = open(report_path, "r", encoding="utf-8").read()
    sources_text = open(os.path.join(d, "artifacts", "sources.json"), "r", encoding="utf-8").read()

print(json.dumps({
    "rc": rc,
    "attempts": calls["n"],
    "sleeps": sleep_calls,
    "first_query_has_excerpt_section": "## 必読の一次情報" in captured_queries[0],
    "first_query_has_readme_text": "persistent-memory ad-hoc file system" in captured_queries[0],
    "second_query_is_the_same_as_first": captured_queries[0] == captured_queries[1],
    "celeris_result_line": "CELERIS_RESULT" in stdout_text,
    "report_text": report_text,
    "sources": json.loads(sources_text),
}))
"###;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(v["rc"], 0, "{v}");
        assert_eq!(v["attempts"], 2, "2 回目で成功するはず: {v}");
        assert_eq!(v["sleeps"], serde_json::json!([2.0]), "1 回だけ待つ: {v}");
        assert_eq!(v["first_query_has_excerpt_section"], true, "{v}");
        assert_eq!(v["first_query_has_readme_text"], true, "{v}");
        assert_eq!(v["second_query_is_the_same_as_first"], true, "{v}");
        assert_eq!(v["celeris_result_line"], true, "{v}");
        let report_text = v["report_text"].as_str().unwrap();
        assert!(
            report_text.contains("CHFS is a persistent-memory"),
            "{report_text}"
        );
        assert!(
            !report_text.contains("no_source_available"),
            "偽装エラーの本文は残らない: {report_text}"
        );
        let sources = v["sources"].as_array().unwrap();
        let chfs = sources
            .iter()
            .find(|s| s["url"] == "https://github.com/otatebe/chfs")
            .expect("必読の一次情報が sources.json に入っている");
        assert_eq!(chfs["primary"], true, "{chfs}");
        assert_eq!(
            chfs["cited"], true,
            "README の抜粋が答えの本文に現れるので cited になる: {chfs}"
        );
        // `human`（URL でない）は落ちて sources には現れない。
        assert!(
            !sources.iter().any(|s| s["url"] == "human"),
            "{sources:?}"
        );
    }

    /// エンドツーエンド: ADR-0063 Phase 109c B3。目的文から対象が取れた（`targets` が非空の）run は
    /// `main()` の中で構造化合成を呼び、`report.md` の先頭が「# 対象別の整理」になる。`targets` が
    /// 空なら（従来どおり）合成を呼ばず、LDR の生の findings だけが残る。
    #[test]
    fn main_applies_structured_synthesis_when_targets_are_present() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("local_deep_research_run.py");
        std::fs::write(&script_path, RUNNER_SCRIPT).unwrap();
        let checker = r###"
import contextlib, importlib.util, io, json, os, sys, tempfile, types

spec = importlib.util.spec_from_file_location("ldr_run", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

mod.time.sleep = lambda s: None
synth_calls = []

def fake_synthesize(messages, base_url, api_key, model, timeout=120):
    synth_calls.append({"base_url": base_url, "model": model})
    return "| 対象 | 配置 |\n|---|---|\n| CHFS | 事実(readme) |\n\n### CHFS\n配置: 事実"

mod.default_synthesize = fake_synthesize

def fake_quick_summary(query, **kwargs):
    return {
        "summary": "CHFS is a persistent-memory ad-hoc file system for HPC. [1]",
        "sources": [{"link": "https://example.org/blog", "title": "Blog", "engine": "tavily"}],
        "findings": [],
        "iterations": 1,
        "questions": {},
    }

fake_api = types.ModuleType("local_deep_research.api")
fake_api.quick_summary = fake_quick_summary
fake_api.detailed_research = lambda *a, **k: (_ for _ in ()).throw(AssertionError("not used"))
fake_api.generate_report = lambda *a, **k: (_ for _ in ()).throw(AssertionError("not used"))
fake_api.create_settings_snapshot = lambda **k: {}
fake_pkg = types.ModuleType("local_deep_research")
fake_pkg.api = fake_api
sys.modules["local_deep_research"] = fake_pkg
sys.modules["local_deep_research.api"] = fake_api


def run_once(targets, aspects):
    with tempfile.TemporaryDirectory() as d:
        report_path = os.path.join(d, "artifacts", "report.md")
        input_path = os.path.join(d, "input.json")
        payload = {
            "query": "CHFS について調べる",
            "mode": "quick",
            "settings": {
                "llm.model": "celeris/cheap",
                "llm.openai_endpoint.url": "http://127.0.0.1:18100/v1",
            },
            "report_path": report_path,
            "targets": targets,
            "aspects": aspects,
        }
        with open(input_path, "w") as f:
            json.dump(payload, f)
        sys.argv = ["local_deep_research_run.py", input_path]
        with contextlib.redirect_stdout(io.StringIO()):
            rc = mod.main()
        report_text = open(report_path, "r", encoding="utf-8").read()
        return rc, report_text


rc_with_targets, report_with_targets = run_once(["CHFS"], ["配置"])
synth_calls_with_targets = len(synth_calls)
rc_without_targets, report_without_targets = run_once([], [])

print(json.dumps({
    "rc_with_targets": rc_with_targets,
    "report_with_targets": report_with_targets,
    "synth_calls_with_targets": synth_calls_with_targets,
    "rc_without_targets": rc_without_targets,
    "report_without_targets": report_without_targets,
    "synth_calls_total": len(synth_calls),
}))
"###;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(v["rc_with_targets"], 0, "{v}");
        assert_eq!(v["synth_calls_with_targets"], 1, "{v}");
        let report_with_targets = v["report_with_targets"].as_str().unwrap();
        assert!(
            report_with_targets.starts_with("# 対象別の整理"),
            "{report_with_targets}"
        );
        assert!(
            report_with_targets.contains("CHFS is a persistent-memory"),
            "生の findings も後段に残る: {report_with_targets}"
        );

        assert_eq!(v["rc_without_targets"], 0, "{v}");
        // `targets` が無い run では合成を呼ばない（呼び出し回数は前段の 1 回のままで増えない）。
        assert_eq!(v["synth_calls_total"], 1, "{v}");
        let report_without_targets = v["report_without_targets"].as_str().unwrap();
        assert!(
            !report_without_targets.starts_with("# 対象別の整理"),
            "{report_without_targets}"
        );
    }

    /// エンドツーエンド: ADR-0063 Phase 109b B3 の再試行が尽きた場合、`report.md` を書かず（残さず）、
    /// exit code 1 と `llm: ...` のメッセージで終わる（`run_ldr` はこれを retryable な
    /// `Terminal::Error` にする）。バックオフは全て偽の `sleep` なので実時間は待たない。
    #[test]
    fn main_gives_up_without_writing_report_md_when_the_upstream_error_never_clears() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("local_deep_research_run.py");
        std::fs::write(&script_path, RUNNER_SCRIPT).unwrap();
        let checker = r#"
import contextlib, importlib.util, io, json, os, sys, tempfile, types

spec = importlib.util.spec_from_file_location("ldr_run", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

sleep_calls = []
mod.time.sleep = lambda s: sleep_calls.append(s)

calls = {"n": 0}

def fake_quick_summary(query, **kwargs):
    calls["n"] += 1
    return {"summary": "Error: Error code: 503 - no_source_available forever", "sources": []}

fake_api = types.ModuleType("local_deep_research.api")
fake_api.quick_summary = fake_quick_summary
fake_api.detailed_research = lambda *a, **k: (_ for _ in ()).throw(AssertionError("not used"))
fake_api.generate_report = lambda *a, **k: (_ for _ in ()).throw(AssertionError("not used"))
fake_api.create_settings_snapshot = lambda **k: {}
fake_pkg = types.ModuleType("local_deep_research")
fake_pkg.api = fake_api
sys.modules["local_deep_research"] = fake_pkg
sys.modules["local_deep_research.api"] = fake_api

with tempfile.TemporaryDirectory() as d:
    report_path = os.path.join(d, "artifacts", "report.md")
    input_path = os.path.join(d, "input.json")
    payload = {"query": "CHFS について調べる", "mode": "quick", "settings": {}, "report_path": report_path}
    with open(input_path, "w") as f:
        json.dump(payload, f)
    sys.argv = ["local_deep_research_run.py", input_path]
    stderr_capture = io.StringIO()
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(stderr_capture):
        rc = mod.main()
    report_exists = os.path.exists(report_path)

print(json.dumps({
    "rc": rc,
    "attempts": calls["n"],
    "sleeps": sleep_calls,
    "report_exists": report_exists,
    "stderr": stderr_capture.getvalue(),
}))
"#;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(v["rc"], 1, "{v}");
        assert_eq!(v["attempts"], 4, "1 回 + 再試行 3 回 = 4 回: {v}");
        assert_eq!(v["sleeps"], serde_json::json!([2.0, 4.0, 8.0]), "{v}");
        assert_eq!(v["report_exists"], false, "{v}");
        assert!(
            v["stderr"].as_str().unwrap().starts_with("llm: "),
            "{v}"
        );
    }

    /// ADR-0063 Phase 109c B2: `is_docs_site` recognizes readthedocs/`doc.`/`docs.`/`/docs/`;
    /// `select_docs_subpages` follows only same-host, config/deploy-ish-keyword links, capped,
    /// de-duplicated, and never the page itself.
    #[test]
    fn runner_docs_site_detection_and_subpage_selection_are_deterministic() {
        if !python3_available() {
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

out = {}
out["readthedocs"] = mod.is_docs_site("https://finchfs.readthedocs.io/en/latest/")
out["doc_subdomain"] = mod.is_docs_site("https://doc.beegfs.io/latest/index.html")
out["docs_path"] = mod.is_docs_site("https://example.org/docs/x")
out["not_a_docs_site"] = mod.is_docs_site("https://example.org/x")

html = (
    '<a href="/en/latest/configuration.html">Configuration</a>'
    '<a href="/en/latest/other.html">Other page</a>'
    '<a href="https://another.example/deploy">External deploy</a>'
    '<a href="/en/latest/install.html">Install</a>'
    '<a href="/en/latest/usage.html">Usage</a>'
    '<a href="/en/latest/faq.html">FAQ</a>'
    '<a href="/en/latest/index.html">Home</a>'
)
base = "https://finchfs.readthedocs.io/en/latest/index.html"
out["subpages"] = mod.select_docs_subpages(html, base)
out["subpages_capped"] = mod.select_docs_subpages(html, base, max_pages=2)
print(json.dumps(out))
"##;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(v["readthedocs"], true, "{v}");
        assert_eq!(v["doc_subdomain"], true, "{v}");
        assert_eq!(v["docs_path"], true, "{v}");
        assert_eq!(v["not_a_docs_site"], false, "{v}");

        let subpages: Vec<String> = v["subpages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            subpages,
            vec![
                "https://finchfs.readthedocs.io/en/latest/configuration.html",
                "https://finchfs.readthedocs.io/en/latest/install.html",
                "https://finchfs.readthedocs.io/en/latest/usage.html",
            ],
            "外部ホスト・キーワード無し・自分自身は落ちる: {v}"
        );
        assert_eq!(
            v["subpages_capped"].as_array().unwrap().len(),
            2,
            "max_pages で打ち切る: {v}"
        );
    }

    /// ADR-0063 Phase 109c B2: docs サイトのトップページを取ったら、同一ホストの config/deploy 系
    /// サブページも 1 階層分取り込む（`source: "must-read-docs"`）。合計文字数の予算を使い切ったら
    /// それ以上は追加しない。
    #[test]
    fn runner_builds_primary_source_excerpts_follow_docs_subpages_within_a_budget() {
        if !python3_available() {
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

# 予算を小さくして、2 ページ目で使い切ることを確認する（本番の 48 KB は大きすぎてテストできない）。
mod.DOCS_SUBPAGES_TOTAL_MAX_CHARS = 10

top_html = (
    "<html><body>"
    '<a href="/en/latest/configuration.html">Configuration</a>'
    '<a href="/en/latest/install.html">Install</a>'
    "</body></html>"
)

def fetch(url, timeout):
    if url.endswith("index.html"):
        return top_html.encode()
    if url.endswith("configuration.html"):
        return b"<html><head><title>Config</title></head><body>0123456789ABCDEF</body></html>"
    if url.endswith("install.html"):
        raise AssertionError("budget should already be exhausted before fetching install.html")
    raise AssertionError("unexpected url: " + url)

section, entries = mod.build_primary_source_entries(
    ["https://finchfs.readthedocs.io/en/latest/index.html"], fetch=fetch
)
print(json.dumps({"section": section, "entries": entries}))
"##;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2, "トップページ + config サブページのみ: {v}");
        assert_eq!(entries[0]["source"], "must-read");
        assert_eq!(entries[1]["source"], "must-read-docs");
        assert_eq!(
            entries[1]["link"],
            "https://finchfs.readthedocs.io/en/latest/configuration.html"
        );
        assert!(entries[1]["primary"].as_bool().unwrap());
        assert!(
            entries[1]["excerpt"].as_str().unwrap().len() <= 10,
            "予算でセルフキャップされる: {v}"
        );
    }

    /// ADR-0063 Phase 109c B3: 対象が取れているとき、`apply_structured_synthesis` は合成 LLM の答え
    /// （表 + 対象ごとの節）を report.md の先頭に足し、生の findings は後段に残す。合成が最後まで
    /// 失敗すれば `report.md` はそのまま（best-effort、run 自体は失敗にしない）。
    #[test]
    fn runner_structured_synthesis_prepends_a_table_and_is_best_effort() {
        if !python3_available() {
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
mod.time.sleep = lambda s: None  # never really wait in a test

out = {}
messages = mod.build_synthesis_messages(["CHFS", "FinchFS"], ["配置", "cache"], "資料本文")
out["system_role"] = messages[0]["role"]
out["mentions_targets"] = "CHFS" in messages[1]["content"] and "FinchFS" in messages[1]["content"]
out["mentions_aspects"] = "配置" in messages[1]["content"] and "cache" in messages[1]["content"]
out["material"] = mod.structured_synthesis_material(
    "raw findings", [{"title": "CHFS README", "link": "https://x", "excerpt": "CHFS excerpt"}]
)

with tempfile.TemporaryDirectory() as d:
    path = os.path.join(d, "report.md")
    with open(path, "w", encoding="utf-8") as f:
        f.write("# raw findings\n\nCHFS is ...\n")

    calls = {"n": 0}
    def flaky_synth(messages, base_url, api_key, model, timeout=120):
        calls["n"] += 1
        if calls["n"] < 2:
            raise RuntimeError("transient")
        return "| 対象 | 配置 |\n|---|---|\n| CHFS | 事実(readme) |\n\n### CHFS\n配置: 事実"

    applied = mod.apply_structured_synthesis(
        path, ["CHFS"], ["配置"], [], flaky_synth, "celeris/cheap", "http://127.0.0.1:18100/v1", "k"
    )
    with open(path, encoding="utf-8") as f:
        rewritten = f.read()
    out["applied"] = applied
    out["retried_once"] = calls["n"] == 2
    out["table_comes_first"] = rewritten.startswith("# 対象別の整理")
    out["raw_findings_kept_after"] = "raw findings" in rewritten and rewritten.index(
        "raw findings"
    ) > rewritten.index("対象別の整理")

    with open(path, "w", encoding="utf-8") as f:
        f.write("untouched")

    def always_fails(*a, **k):
        raise RuntimeError("nope")

    out["gives_up_without_touching_the_file"] = (
        mod.apply_structured_synthesis(
            path, ["CHFS"], ["配置"], [], always_fails, "m", "http://x", "k"
        )
        is False
    )
    with open(path, encoding="utf-8") as f:
        out["file_after_giving_up"] = f.read()

    out["skips_without_targets"] = (
        mod.apply_structured_synthesis(path, [], ["配置"], [], always_fails, "m", "http://x", "k")
        is False
    )

print(json.dumps(out, ensure_ascii=False))
"##;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(v["system_role"], "system", "{v}");
        assert_eq!(v["mentions_targets"], true, "{v}");
        assert_eq!(v["mentions_aspects"], true, "{v}");
        assert!(
            v["material"].as_str().unwrap().contains("raw findings")
                && v["material"].as_str().unwrap().contains("CHFS excerpt"),
            "{v}"
        );
        assert_eq!(v["applied"], true, "{v}");
        assert_eq!(v["retried_once"], true, "2/4/8 秒バックオフで再試行する: {v}");
        assert_eq!(v["table_comes_first"], true, "{v}");
        assert_eq!(v["raw_findings_kept_after"], true, "{v}");
        assert_eq!(v["gives_up_without_touching_the_file"], true, "{v}");
        assert_eq!(v["file_after_giving_up"], "untouched", "{v}");
        assert_eq!(v["skips_without_targets"], true, "{v}");
    }
}
