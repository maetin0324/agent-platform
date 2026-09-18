//! `paperqa` アダプタ（DESIGN §5.4, ADR-0027 D3、ADR-0035 で「取得」の段を追加）。
//!
//! PaperQA2（`pqa` CLI）は taskd のワーカープロトコルもストリーム型の進捗形式も話さない、
//! ただの調査エンジンである。**アダプタ自身が** ADR-0006 D3 の結果ファイル規約（`artifacts/result.json`）を
//! 代わりに書き、`Terminal::Done`/`Terminal::Error` を合成する。委譲（`delegate.json`）は扱わない
//! （ADR-0027 D3: 「委譲はしない」）。生存監視（wall-clock・無出力タイムアウト・SIGTERM→SIGKILL）は
//! `subprocess.rs` の低レベル部分を再利用する。
//!
//! ADR-0035: 1 run は **2 段**になった。
//!
//! 1. **取得** — 埋め込みの Python ランナー（`paperqa_acquire.py`）を `runs/<run_id>/` に書き出して起動し、
//!    arXiv / OpenAlex（鍵無し）で候補論文を集め、open access の PDF を**案件ごとの** corpus
//!    （`paper_directory/<project_id>/`）に落とす。検索語はこのアダプタが `objective` から決定的に作る
//!    （LLM は使わない。`build_search_queries`）。
//! 2. **索引と回答** — 従来どおり `pqa ask`。`paper_directory` / `index_directory` / 索引名は案件ごと。
//!
//! 回答の後に、答えが実際に引用した出典（`cited`）を決定的に突き合わせ（`answer_cites`）、
//! `artifacts/sources.json` を書き直し、`artifacts/answer.md` の末尾に `## 出典` を足し、
//! 証拠ゲート（ADR-0035 D3）で候補数・PDF 数・引用数を機械的に判定する。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use task_core::Task;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
use crate::protocol::{Answer, RunContext, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, kill_now, reap_after_terminal, read_line_limited, read_tail, write_result_json,
};

/// run ごとに `runs/<run_id>/paperqa_acquire.py` として書き出す取得ランナー（ADR-0035 D1）。
const ACQUIRE_SCRIPT: &str = include_str!("paperqa_acquire.py");
/// 取得ランナーの最終行の目印（ADR-0035 D1）。
const ACQUIRE_RESULT_PREFIX: &str = "TASKD_ACQUIRE ";
/// 取得ランナーの進捗行の目印。
const PROGRESS_PREFIX: &str = "progress:";
/// `artifacts/result.json` の `summary` の上限（ADR-0027 D3）。
const SUMMARY_MAX_CHARS: usize = 1500;
/// `progress` に転送する 1 行あたりの上限（他アダプタと同じ規則。ADR-0026 の `truncate` を踏襲）。
const PROGRESS_LINE_MAX_CHARS: usize = 500;
/// 案件（`project_id`）が無いタスクの corpus / 索引の名前（ADR-0035 D1）。
const SHARED_PROJECT_KEY: &str = "_shared";
/// `objective` から作る検索語の本数の上限（ADR-0035 D1: 「2〜4 本」）。
const MAX_SEARCH_QUERIES: usize = 4;

/// `[adapters.paperqa.acquire]`（ADR-0035 D1）: 文献の取得の設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquireConfig {
    /// 取得ランナーを動かす python（標準ライブラリしか使わないので任意の python3 でよい）。
    /// 未指定なら `command`（`pqa`）と同じディレクトリの `python3`、`command` にディレクトリが
    /// 無ければ `python3`。
    #[serde(default)]
    pub command: Option<String>,
    /// 重複排除後に残す候補の上限。**`0` なら取得の段そのものを行わない**（従来どおり手元の corpus
    /// だけで答える。このとき証拠ゲート（`evidence`）も見ない）。
    #[serde(default = "default_max_candidates")]
    pub max_candidates: u32,
    /// 1 run で corpus に入れる PDF の上限。
    #[serde(default = "default_max_pdfs")]
    pub max_pdfs: u32,
    /// 検索語 1 本・エンジン 1 つあたりの取得件数。
    #[serde(default = "default_per_query")]
    pub per_query: u32,
    /// 1 回の HTTP 要求のタイムアウト（秒）。
    #[serde(default = "default_acquire_timeout_secs")]
    pub timeout_secs: u64,
    /// OpenAlex の polite pool に付ける連絡先（`mailto=`）。未指定なら付けない。
    #[serde(default)]
    pub mailto: Option<String>,
}

impl Default for AcquireConfig {
    fn default() -> Self {
        Self {
            command: None,
            max_candidates: default_max_candidates(),
            max_pdfs: default_max_pdfs(),
            per_query: default_per_query(),
            timeout_secs: default_acquire_timeout_secs(),
            mailto: None,
        }
    }
}

fn default_max_candidates() -> u32 {
    30
}
fn default_max_pdfs() -> u32 {
    12
}
fn default_per_query() -> u32 {
    20
}
fn default_acquire_timeout_secs() -> u64 {
    30
}

/// `[adapters.paperqa.evidence]`（ADR-0035 D3）: 決定的な証拠ゲートの閾値（ADR-0031 D2 の literature 版）。
/// ハーネス（このアダプタ）が取得の結果と答えの引用を機械的に見る（LLM に判断させない）。
/// `0` を書けばその項目は見ない。全部 0 ならゲート無し。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaperQaEvidence {
    /// 検索が返した候補論文（重複排除後）の数の下限。
    #[serde(default = "default_min_candidates")]
    pub min_candidates: u32,
    /// corpus に入った PDF の数の下限（既にあったものを含む）。
    #[serde(default = "default_min_pdfs")]
    pub min_pdfs: u32,
    /// 答えが引用した出典の数の下限。
    #[serde(default = "default_min_cited")]
    pub min_cited: u32,
}

impl Default for PaperQaEvidence {
    fn default() -> Self {
        Self {
            min_candidates: default_min_candidates(),
            min_pdfs: default_min_pdfs(),
            min_cited: default_min_cited(),
        }
    }
}

fn default_min_candidates() -> u32 {
    5
}
fn default_min_pdfs() -> u32 {
    3
}
fn default_min_cited() -> u32 {
    2
}

/// `[adapters.paperqa]`（taskd.toml, ADR-0027 D3）。`[[providers]] adapter = "paperqa"` の行ごとに
/// `settings` / `env` / `model` を上書きできる（ADR-0026 D2 と同じ作り）。
#[derive(Debug, Clone)]
pub struct PaperQaConfig {
    /// 起動するコマンド名／パス。既定 `"pqa"`。
    pub command: String,
    /// `-s <name>`（拡張子は付けない。`pqa` 自身が `.json` を足す。ADR-0027 の実機の仕様）。未指定なら渡さない。
    pub settings: Option<String>,
    /// `--agent.index.paper_directory` の親ディレクトリ（実際に渡す値はこの下に**案件ごと**の
    /// サブディレクトリを足したもの。ADR-0035 D1）。未指定ならワークスペース相対 `papers`。
    pub paper_directory: Option<PathBuf>,
    /// `--agent.index.index_directory` の親ディレクトリ（実際に渡す値はこの下に**案件ごと**の
    /// サブディレクトリを足したもの。ADR-0035 D2。ADR-0027 D3 の「タスクごと」からの変更）。
    /// 未指定ならワークスペース相対 `index`。
    pub index_directory: Option<PathBuf>,
    /// `--agent.index.name`。未指定なら案件の鍵（`project_id`、無ければ `_shared`）を使う。
    pub index_name: Option<String>,
    /// `--llm`。設定されているときだけ渡す（PaperQA の設定ファイルの値より優先。ADR-0027 D3）。
    pub model: Option<String>,
    /// 追加の環境変数（例: `OPENAI_API_KEY` / `OPENAI_BASE_URL`。LiteLLM 経由の OpenAI 互換エンドポイント向け）。
    pub env: Vec<(String, String)>,
    /// 末尾に追加する引数（`ask` の前に挿入する）。
    pub extra_args: Vec<String>,
    /// ADR-0035 D1: 文献の取得。
    pub acquire: AcquireConfig,
    /// ADR-0035 D3: 決定的な証拠ゲートの閾値。
    pub evidence: PaperQaEvidence,
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
            acquire: AcquireConfig::default(),
            evidence: PaperQaEvidence::default(),
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
/// 素の目的 + 前置き（役割の指示文・記憶・直近のやり取り）+ 人間の回答履歴だけを使う（PaperQA2 は
/// ワーカープロトコルを話さない調査エンジン
/// であり、結果ファイルの書式やコマンド再実行の話をしても意味がないため）。`request.json`/`prompt.txt` は
/// 他のアダプタと同じ共有ヘルパ（`subprocess::write_run_request`/`write_run_prompt`）で残す。
pub fn build_question(task: &Task, context: &RunContext) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", task.title));
    // ADR-0033 D4 / D6（Phase 24）: 前置き（役職と brief・永続の認可・記憶・直近のやり取り・役割の指示文）は
    // `crate::preamble` が 1 か所で組む。`RunContext` が Phase 23 までの中身なら出力は変わらない。
    out.push_str(&crate::preamble::render(context));
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

/// 案件の鍵（corpus と索引のサブディレクトリ名。ADR-0035 D1）。案件が無いタスクは `_shared`。
/// パス要素として安全な文字（英数字・`-`・`_`）だけを残す（ULID はもともと英数字だが、
/// 将来 id の形が変わってもディレクトリを抜け出さないため）。
pub fn project_key(task: &Task) -> String {
    let raw = match &task.project_id {
        Some(id) => id.to_string(),
        None => return SHARED_PROJECT_KEY.to_string(),
    };
    let safe: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if safe.trim_matches('_').is_empty() { SHARED_PROJECT_KEY.to_string() } else { safe }
}

/// 依頼文（`objective`）から検索語を決定的に作る（ADR-0035 D1 手順 1。**LLM は使わない**）。
///
/// 依頼文が日本語でも、その中の**英数字の名詞句**（`Pluvio` / `ad-hoc FS` /
/// `asynchronous I/O runtime` のように、日本語や句読点で区切られた ASCII の連なり）は英語の検索語に
/// なる。語数の多い順・出現順で最大 `MAX_SEARCH_QUERIES` 本を選ぶ。1 本も取れなければ
/// `objective` 全文を 1 本の検索語にする（検索エンジンに丸投げする最後の手段）。
pub fn build_search_queries(objective: &str) -> Vec<String> {
    /// 英字・数字とその間に入りうる記号（`I/O`、`ad-hoc`、`C++`、`.NET` 等）だけを句の材料にする。
    fn is_phrase_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | '+' | '#' | '.' | '_' | '\'')
    }
    fn is_stopword(word: &str) -> bool {
        matches!(
            word,
            "a" | "an"
                | "and"
                | "are"
                | "as"
                | "at"
                | "be"
                | "by"
                | "for"
                | "from"
                | "in"
                | "is"
                | "of"
                | "on"
                | "or"
                | "that"
                | "the"
                | "to"
                | "via"
                | "vs"
                | "with"
        )
    }

    // 1. ASCII の句に切り出す（区切りは日本語・句読点・改行）。句の中の単語は空白で区切られる。
    let mut phrases: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut word = String::new();
    let flush_word = |word: &mut String, current: &mut Vec<String>| {
        let trimmed = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if !trimmed.is_empty() {
            current.push(trimmed.to_string());
        }
        word.clear();
    };
    for c in objective.chars() {
        if is_phrase_char(c) {
            word.push(c);
        } else if c == ' ' || c == '\t' {
            flush_word(&mut word, &mut current);
        } else {
            flush_word(&mut word, &mut current);
            if !current.is_empty() {
                phrases.push(current.join(" "));
                current.clear();
            }
        }
    }
    flush_word(&mut word, &mut current);
    if !current.is_empty() {
        phrases.push(current.join(" "));
    }

    // 2. 検索語として意味の無いものを落とす。
    let mut kept: Vec<String> = Vec::new();
    for phrase in phrases {
        let words: Vec<&str> = phrase.split(' ').filter(|w| !w.is_empty()).collect();
        let meaningful: Vec<&str> = words.iter().copied().filter(|w| !is_stopword(&w.to_ascii_lowercase())).collect();
        if meaningful.is_empty() {
            continue;
        }
        // 1 語だけの句は、大文字を含む（固有名詞・略語。`Pluvio` / `FS`）か 4 文字以上のときだけ残す。
        if meaningful.len() == 1 {
            let only = meaningful[0];
            let has_upper = only.chars().any(|c| c.is_ascii_uppercase());
            if !has_upper && only.chars().count() < 4 {
                continue;
            }
            if only.chars().count() < 2 {
                continue;
            }
        }
        let text = meaningful.join(" ");
        let lower = text.to_ascii_lowercase();
        if kept.iter().any(|k| k.to_ascii_lowercase() == lower) {
            continue;
        }
        kept.push(text);
    }

    // 3. 他の句に丸ごと含まれる句は落とす（`ad-hoc FS` ⊂ `ad-hoc FS server` のような場合）。
    let mut unique: Vec<String> = Vec::new();
    for (i, phrase) in kept.iter().enumerate() {
        let lower = phrase.to_ascii_lowercase();
        let contained = kept.iter().enumerate().any(|(j, other)| {
            j != i && other.len() > phrase.len() && other.to_ascii_lowercase().contains(&lower)
        });
        if !contained {
            unique.push(phrase.clone());
        }
    }

    // 4. 語数の多い順（同数なら出現順）で上限まで。
    let mut ordered: Vec<(usize, usize, String)> = unique
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p.split(' ').count(), i, p))
        .collect();
    ordered.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let queries: Vec<String> = ordered.into_iter().take(MAX_SEARCH_QUERIES).map(|(_, _, p)| p).collect();

    if queries.is_empty() {
        let fallback = objective.split_whitespace().collect::<Vec<_>>().join(" ");
        if fallback.is_empty() { Vec::new() } else { vec![fallback] }
    } else {
        queries
    }
}

/// 取得ランナーを動かす python（ADR-0035 D1）。設定が無ければ `command`（`pqa`）と同じ
/// ディレクトリの `python3`（venv の中を指しているのが普通）、ディレクトリが無ければ `python3`。
fn acquire_python(config: &PaperQaConfig) -> String {
    if let Some(command) = &config.acquire.command {
        return command.clone();
    }
    match Path::new(&config.command).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("python3").to_string_lossy().into_owned(),
        _ => "python3".to_string(),
    }
}

/// `artifacts/candidates.json` の 1 件（ADR-0035 D1 手順 4）。ランナー（Python）が書き、
/// アダプタが `cited` の突き合わせと `## 出典` の組み立てに読む。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Candidate {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub year: Option<i64>,
    #[serde(default)]
    pub venue: String,
    #[serde(default)]
    pub doi: String,
    #[serde(default)]
    pub arxiv_id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub pdf_url: String,
    /// corpus 内のファイル名（落とせなかった候補では空のこともある）。
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub pdf_downloaded: bool,
    #[serde(default)]
    pub source_engine: String,
}

/// 取得の段の結果（`TASKD_ACQUIRE` の中身）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AcquireCounts {
    candidates: u32,
    pdfs: u32,
}

/// 子プロセスの標準出力を 1 行ずつ読み、生存監視（壁時計・無出力）を行う（両方の段で共用）。
struct StreamedRun {
    stdout: String,
    stderr_tail: String,
    exit: std::process::ExitStatus,
    /// 上限を超えて強制終了したときの終端（超えていなければ `None`）。
    timeout: Option<Terminal>,
}

#[allow(clippy::too_many_arguments)]
async fn stream_child(
    mut child: Child,
    stdout_log_path: PathBuf,
    stderr_log_path: PathBuf,
    start: Instant,
    limits: &RunLimits,
    run_id: &str,
    sink: &dyn EventSink,
    mut on_line: impl FnMut(&str),
) -> Result<StreamedRun, AdapterError> {
    let stderr_log_path_for_task = stderr_log_path.clone();
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

    let mut last_activity = Instant::now();
    let mut stdout_buf = String::new();
    let mut force_kill = false;
    let mut timeout_terminal: Option<Terminal> = None;

    loop {
        // 壁時計は run 全体（取得 + pqa）で数える。2 段目に入る時点で残りが無ければすぐ打ち切る。
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
                // pqa / ランナーのフォーマットは taskd が定義したものではないので寛容に無視する
                // （claude_code と同じ考え方）。
                sink.heartbeat();
                last_activity = Instant::now();
                warn!("run {run_id}: discarding overlong line from the paperqa stage");
            }
            LineOutcome::Line(bytes) => {
                sink.heartbeat();
                last_activity = Instant::now();
                stdout_file.write_all(&bytes).await?;
                stdout_file.write_all(b"\n").await?;
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim().to_string();
                stdout_buf.push_str(&text);
                stdout_buf.push('\n');
                if !trimmed.is_empty() {
                    on_line(&trimmed);
                }
            }
        }
    }

    let exit = if force_kill {
        kill_now(&mut child, limits.kill_grace).await?
    } else {
        reap_after_terminal(&mut child, limits.kill_grace).await?
    };
    if let Err(e) = stderr_task.await {
        warn!("run {run_id}: stderr capture task failed: {e}");
    }
    stdout_file.flush().await?;
    let stderr_tail = read_tail(&stderr_log_path, 4096).await;

    Ok(StreamedRun {
        stdout: stdout_buf,
        stderr_tail,
        exit,
        timeout: timeout_terminal,
    })
}

/// 取得の段（ADR-0035 D1 / D2 手順 1）。**失敗しても run は止めない**（`progress` に残して `pqa` に進み、
/// 判定は証拠ゲートに任せる）。壁時計・無出力の上限に当たったときだけ終端を返す。
#[allow(clippy::too_many_arguments)]
async fn run_acquire(
    config: &PaperQaConfig,
    req: &RunRequest,
    run_id: &str,
    limits: &RunLimits,
    start: Instant,
    sink: &dyn EventSink,
    run_dir: &Path,
    artifacts_dir: &Path,
    paper_directory: &Path,
) -> Result<(AcquireCounts, Option<Terminal>), AdapterError> {
    let queries = build_search_queries(&req.task.objective);
    sink.progress(&truncate_chars(
        &format!("acquiring literature for {} search term(s): {}", queries.len(), queries.join(" | ")),
        PROGRESS_LINE_MAX_CHARS,
    ));

    let script_path = run_dir.join("paperqa_acquire.py");
    let input_path = run_dir.join("acquire_input.json");
    let input = serde_json::json!({
        "queries": queries,
        "paper_directory": paper_directory.to_string_lossy(),
        "candidates_path": artifacts_dir.join("candidates.json").to_string_lossy(),
        "sources_path": artifacts_dir.join("sources.json").to_string_lossy(),
        "max_candidates": config.acquire.max_candidates,
        "max_pdfs": config.acquire.max_pdfs,
        "per_query": config.acquire.per_query,
        "timeout_secs": config.acquire.timeout_secs,
        "mailto": config.acquire.mailto,
    });
    tokio::fs::write(&script_path, ACQUIRE_SCRIPT).await?;
    let input_text = serde_json::to_string_pretty(&input)?;
    tokio::fs::write(&input_path, format!("{input_text}\n")).await?;

    let python = acquire_python(config);
    let mut command = Command::new(&python);
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

    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            // 取得ランナーが起動できないのは設定の誤り（python のパス）だが、ここでは run を止めず
            // 0 件として先に進む（ゲートが「取得が 0 件」として人に返す）。
            warn!("run {run_id}: could not start the literature acquisition runner ({python}): {e}");
            sink.progress(&truncate_chars(
                &format!("literature acquisition could not start ({python}): {e}"),
                PROGRESS_LINE_MAX_CHARS,
            ));
            return Ok((AcquireCounts::default(), None));
        }
    };

    let mut counts: Option<AcquireCounts> = None;
    let streamed = stream_child(
        child,
        run_dir.join("acquire.stdout.log"),
        run_dir.join("acquire.stderr.log"),
        start,
        limits,
        run_id,
        sink,
        |line| {
            if let Some(rest) = line.strip_prefix(PROGRESS_PREFIX) {
                sink.progress(&truncate_chars(&format!("acquire: {}", rest.trim()), PROGRESS_LINE_MAX_CHARS));
            } else if let Some(rest) = line.strip_prefix(ACQUIRE_RESULT_PREFIX) {
                match serde_json::from_str::<serde_json::Value>(rest) {
                    Ok(value) => {
                        let number = |key: &str| -> u32 {
                            value.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0) as u32
                        };
                        counts = Some(AcquireCounts {
                            candidates: number("candidates"),
                            pdfs: number("pdfs"),
                        });
                    }
                    Err(e) => warn!("run {run_id}: could not parse TASKD_ACQUIRE line: {e}"),
                }
            }
        },
    )
    .await?;

    if let Some(terminal) = streamed.timeout {
        return Ok((counts.unwrap_or_default(), Some(terminal)));
    }
    if !streamed.exit.success() {
        let tail = streamed.stderr_tail.lines().next_back().unwrap_or("").to_string();
        warn!("run {run_id}: the literature acquisition runner failed: {tail}");
        sink.progress(&truncate_chars(
            &format!("literature acquisition failed: {tail}"),
            PROGRESS_LINE_MAX_CHARS,
        ));
    }
    let counts = counts.unwrap_or_default();
    sink.progress(&format!(
        "acquire: {} candidate(s), {} PDF(s) in the corpus",
        counts.candidates, counts.pdfs
    ));
    Ok((counts, None))
}

/// 答えがこの候補を引用しているか（ADR-0035 D2。**決定的**。合わなければ `false`）。
///
/// PaperQA2 は `parsing.use_doc_details = false`（ADR-0027 の設定）だと**ファイル名**から引用の鍵を作るので、
/// 次のどれかが答えの中に現れれば引用とみなす: DOI / arXiv id（版番号なし）/ corpus のファイル名（拡張子なし）/
/// `著者姓+年`（`brinkmann2020`）/ 著者姓と年の両方 / 正規化したタイトル。
pub fn answer_cites(answer: &str, candidate: &Candidate) -> bool {
    let lower = answer.to_ascii_lowercase();
    let alnum = normalize_alnum(answer);

    let doi = normalize_doi(&candidate.doi);
    if !doi.is_empty() && lower.contains(&doi) {
        return true;
    }
    let arxiv = strip_arxiv_version(&candidate.arxiv_id.to_ascii_lowercase());
    if arxiv.len() >= 6 && lower.contains(&arxiv) {
        return true;
    }
    if !candidate.file.is_empty() {
        let stem = candidate.file.trim_end_matches(".pdf").to_ascii_lowercase();
        if stem.len() >= 4 && lower.contains(&stem) {
            return true;
        }
    }
    let surname = first_author_surname(&candidate.authors);
    if let (false, Some(year)) = (surname.is_empty(), candidate.year) {
        let year = year.to_string();
        if alnum.contains(&format!("{surname}{year}")) {
            return true;
        }
        if lower.contains(&surname) && lower.contains(&year) {
            return true;
        }
    }
    let title = normalize_alnum(&candidate.title);
    if title.len() >= 12 && alnum.contains(&title) {
        return true;
    }
    false
}

fn normalize_alnum(text: &str) -> String {
    text.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase()
}

fn normalize_doi(doi: &str) -> String {
    let mut text = doi.trim().to_ascii_lowercase();
    for prefix in ["https://doi.org/", "http://doi.org/", "doi:"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.to_string();
        }
    }
    text.trim_matches('/').to_string()
}

fn strip_arxiv_version(id: &str) -> String {
    match id.rfind('v') {
        Some(pos) if id[pos + 1..].chars().all(|c| c.is_ascii_digit()) && pos + 1 < id.len() => id[..pos].to_string(),
        _ => id.to_string(),
    }
}

fn first_author_surname(authors: &[String]) -> String {
    let Some(first) = authors.first() else { return String::new() };
    first
        .split_whitespace()
        .next_back()
        .map(|s| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase())
        .unwrap_or_default()
}

/// `artifacts/answer.md` の末尾に足す `## 出典`（ADR-0035 D4）。引用されたものを先に、
/// `[n] 著者 (年). タイトル. venue. URL` の形で並べる（欠けている項目は飛ばす）。
fn render_sources_section(candidates: &[(Candidate, bool)]) -> String {
    let mut ordered: Vec<&(Candidate, bool)> = candidates.iter().filter(|(_, cited)| *cited).collect();
    ordered.extend(candidates.iter().filter(|(_, cited)| !*cited));

    let mut out = String::from("\n\n## 出典\n\n");
    if ordered.is_empty() {
        out.push_str("(取得できた文献なし)\n");
        return out;
    }
    for (index, (candidate, cited)) in ordered.iter().enumerate() {
        let mut parts: Vec<String> = Vec::new();
        let authors = match candidate.authors.len() {
            0 => String::new(),
            1..=3 => candidate.authors.join(", "),
            _ => format!("{} et al.", candidate.authors[0]),
        };
        if !authors.is_empty() {
            parts.push(match candidate.year {
                Some(year) => format!("{authors} ({year})"),
                None => authors,
            });
        } else if let Some(year) = candidate.year {
            parts.push(format!("({year})"));
        }
        if !candidate.title.is_empty() {
            parts.push(candidate.title.clone());
        }
        if !candidate.venue.is_empty() {
            parts.push(candidate.venue.clone());
        }
        let url = if !candidate.url.is_empty() { &candidate.url } else { &candidate.pdf_url };
        if !url.is_empty() {
            parts.push(url.clone());
        }
        let marker = if *cited { " (引用)" } else { "" };
        out.push_str(&format!("[{}] {}{}\n", index + 1, parts.join(". "), marker));
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
    let start = Instant::now();
    let run_dir = req.workspace.join("runs").join(run_id);
    tokio::fs::create_dir_all(&run_dir).await?;

    // 前回の run（リトライ）の名残を今回の結果と誤読しない（claude_code/codex と同じ理由。ADR-0006 D3）。
    let artifacts_dir = req.workspace.join("artifacts");
    for stale in ["result.json", "answer.md", "candidates.json", "sources.json"] {
        let _ = tokio::fs::remove_file(artifacts_dir.join(stale)).await;
    }

    let question = build_question(&req.task, &req.context);
    // ADR-0023 D2 / M1: この run で何を渡したかを残す。
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &question, run_id).await;

    // ADR-0035 D1 / D2: corpus も索引も**案件ごと**（同じ案件の別タスク・リトライで使い回せる。
    // ADR-0027 D3 の「索引はタスクごと」からの変更）。
    let project = project_key(&req.task);
    let paper_directory = config
        .paper_directory
        .clone()
        .unwrap_or_else(|| PathBuf::from("papers"))
        .join(&project);
    let index_directory = config
        .index_directory
        .clone()
        .unwrap_or_else(|| PathBuf::from("index"))
        .join(&project);
    let index_name = config.index_name.clone().unwrap_or_else(|| project.clone());

    // ---------------------------------------------------------------- 1 段目: 取得
    let acquiring = config.acquire.max_candidates > 0;
    let acquired = if acquiring {
        let absolute_papers = if paper_directory.is_absolute() {
            paper_directory.clone()
        } else {
            req.workspace.join(&paper_directory)
        };
        if let Err(e) = tokio::fs::create_dir_all(&artifacts_dir).await {
            warn!("run {run_id}: could not create artifacts/ directory: {e}");
        }
        let (counts, timeout) = run_acquire(
            config,
            req,
            run_id,
            limits,
            start,
            sink,
            &run_dir,
            &artifacts_dir,
            &absolute_papers,
        )
        .await?;
        if let Some(terminal) = timeout {
            // タイムアウトは供給側失敗として分類しない（他アダプタと同じ。ADR-0010 D5）。
            write_result_json(&run_dir, &terminal, None).await?;
            return Ok(RunOutcome { terminal, exit_code: None });
        }
        counts
    } else {
        AcquireCounts::default()
    };

    // ---------------------------------------------------------------- 2 段目: 索引と回答
    let stdout_log_path = run_dir.join("stdout.log");
    let stderr_log_path = run_dir.join("stderr.log");

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

    let child = command.spawn().map_err(AdapterError::Spawn)?;

    let streamed = stream_child(
        child,
        stdout_log_path,
        stderr_log_path,
        start,
        limits,
        run_id,
        sink,
        |line| {
            // PaperQA2 は検索・要約の進捗を出す（ADR-0027 D3 手順 3）。行単位でそのまま progress に写す。
            sink.progress(&truncate_chars(line, PROGRESS_LINE_MAX_CHARS));
        },
    )
    .await?;

    if let Some(terminal) = streamed.timeout {
        // タイムアウトは供給側失敗として分類しない（他アダプタと同じ。ADR-0010 D5）。
        write_result_json(&run_dir, &terminal, None).await?;
        return Ok(RunOutcome {
            terminal,
            exit_code: streamed.exit.code(),
        });
    }

    let exit_status = streamed.exit;
    let stdout_buf = streamed.stdout;
    let stderr_tail = streamed.stderr_tail;
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

        // ADR-0035 D2 / D4: 取得した候補と答えを決定的に突き合わせ、`sources.json` の `cited` を決め、
        // `answer.md` の末尾に `## 出典` を足す。
        let candidates = read_candidates(&artifacts_dir.join("candidates.json")).await;
        let marked: Vec<(Candidate, bool)> = candidates
            .into_iter()
            .map(|c| {
                let cited = answer_cites(&answer, &c);
                (c, cited)
            })
            .collect();
        let cited_count = marked.iter().filter(|(_, cited)| *cited).count() as u32;
        if acquiring {
            write_sources_json(&artifacts_dir.join("sources.json"), &marked, run_id).await;
        }

        let answer_md = if acquiring {
            format!("{answer}{}", render_sources_section(&marked))
        } else {
            answer.clone()
        };
        if let Err(e) = tokio::fs::write(artifacts_dir.join("answer.md"), &answer_md).await {
            warn!("run {run_id}: could not write artifacts/answer.md: {e}");
        }
        // 書いたものは taskd にも知らせる（run の成果物一覧と `Check::ArtifactExists` の解決に使われる）。
        // 他のアダプタではワーカー自身が `artifact` メッセージで申告するが、pqa は申告しないのでアダプタが行う。
        // ADR-0035 D3: ゲートに落ちても成果物は残す（人が読めるように）ので、申告はゲートより前に行う。
        let mut to_register: Vec<(&str, &str, &str)> = vec![("answer.md", "artifacts/answer.md", "markdown")];
        if acquiring {
            to_register.push(("candidates.json", "artifacts/candidates.json", "json"));
            to_register.push(("sources.json", "artifacts/sources.json", "json"));
        }
        for (name, rel_path, kind) in to_register {
            if !req.workspace.join(rel_path).is_file() {
                continue;
            }
            match crate::artifact::resolve(&req.workspace, name, rel_path, Some(kind)) {
                Ok(artifact) => sink.artifact(&artifact),
                Err(e) => warn!("run {run_id}: could not register {rel_path}: {e}"),
            }
        }

        // ADR-0035 D3: 決定的な証拠ゲート（LLM には判断させない）。取得の段を行わない構成では見ない。
        let gate_message =
            if acquiring { evidence_gate(&config.evidence, acquired, cited_count) } else { None };

        if let Some(message) = gate_message {
            // ADR-0031 D2 と同じ: retryable な `Terminal::Error`。供給側の失敗（`AdapterError`）にはしない。
            (Terminal::Error { message, retryable: true }, None)
        } else {
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

/// ADR-0035 D3: 閾値を満たさなければメッセージを返す（満たせば `None`）。
fn evidence_gate(thresholds: &PaperQaEvidence, acquired: AcquireCounts, cited: u32) -> Option<String> {
    let enabled = thresholds.min_candidates > 0 || thresholds.min_pdfs > 0 || thresholds.min_cited > 0;
    if !enabled {
        return None;
    }
    if acquired.candidates == 0 {
        // 「論文が見つからなかった」と「検索経路が壊れている」を運用者が区別できるようにする
        // （ADR-0031 D2 と同じ理由）。
        return Some(
            "literature search returned nothing (possible network or API problem)".to_string(),
        );
    }
    let mut problems = Vec::new();
    if thresholds.min_candidates > 0 && acquired.candidates < thresholds.min_candidates {
        problems.push(format!("candidates={} (min {})", acquired.candidates, thresholds.min_candidates));
    }
    if thresholds.min_pdfs > 0 && acquired.pdfs < thresholds.min_pdfs {
        problems.push(format!("pdfs={} (min {})", acquired.pdfs, thresholds.min_pdfs));
    }
    if thresholds.min_cited > 0 && cited < thresholds.min_cited {
        problems.push(format!("cited={cited} (min {})", thresholds.min_cited));
    }
    if problems.is_empty() {
        None
    } else {
        Some(format!("insufficient literature evidence: {}", problems.join(", ")))
    }
}

async fn read_candidates(path: &Path) -> Vec<Candidate> {
    let Ok(text) = tokio::fs::read_to_string(path).await else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<Candidate>>(&text).unwrap_or_default()
}

/// `artifacts/sources.json` を `cited` を決めた後の値で書き直す（LDR と同じ形。ADR-0031 D1 / ADR-0035 D2）。
async fn write_sources_json(path: &Path, marked: &[(Candidate, bool)], run_id: &str) {
    let sources: Vec<BTreeMap<&str, serde_json::Value>> = marked
        .iter()
        .map(|(candidate, cited)| {
            let url = if !candidate.url.is_empty() { &candidate.url } else { &candidate.pdf_url };
            BTreeMap::from([
                ("url", serde_json::Value::String(url.clone())),
                ("title", serde_json::Value::String(candidate.title.clone())),
                ("engine", serde_json::Value::String(candidate.source_engine.clone())),
                ("cited", serde_json::Value::Bool(*cited)),
            ])
        })
        .collect();
    match serde_json::to_string_pretty(&sources) {
        Ok(text) => {
            if let Err(e) = tokio::fs::write(path, format!("{text}\n")).await {
                warn!("run {run_id}: could not write artifacts/sources.json: {e}");
            }
        }
        Err(e) => warn!("run {run_id}: could not serialize artifacts/sources.json: {e}"),
    }
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

    /// 取得の段（ADR-0035 D1）を**行わない** `pqa` スタブ（Phase 17〜18 のテストはこれ。
    /// `max_candidates = 0` なので外部ネットワークに出ず、証拠ゲートも見ない = 従来どおりの挙動）。
    fn stub_pqa(dir: &Path, script: &str) -> PaperQaConfig {
        let path = dir.join("pqa_stub.sh");
        // ETXTBSY 対策（ADR-0010 D10）: 他アダプタのテストと同じ理由で別プロセスに書かせる。
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        PaperQaConfig {
            command: path.to_string_lossy().into_owned(),
            acquire: AcquireConfig {
                max_candidates: 0,
                ..AcquireConfig::default()
            },
            ..PaperQaConfig::default()
        }
    }

    /// 取得の段も含む 2 段のスタブ（ADR-0035 D1 / D2）。取得ランナーの代わりに sh スクリプトを起動する
    /// （**本物の API は叩かない**。ランナー自身は `--fixture` を使う別のテストで確かめる）。
    /// `acquire_script` は `$1` に書き出されたランナー、`$2` に `acquire_input.json` を受け取る。
    fn stub_pqa_with_acquire(dir: &Path, pqa_script: &str, acquire_script: &str) -> PaperQaConfig {
        let mut config = stub_pqa(dir, pqa_script);
        let path = dir.join("acquire_stub.sh");
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{acquire_script}\n"));
        config.acquire = AcquireConfig {
            command: Some(path.to_string_lossy().into_owned()),
            ..AcquireConfig::default()
        };
        config
    }

    /// 取得ランナーのスタブが書く候補 3 件（うち 2 件は答えの中で引用される）。
    const STUB_CANDIDATES: &str = r#"[
      {"title": "Ad Hoc File Systems for High-Performance Computing", "authors": ["Andre Brinkmann", "Kathryn Mohror", "Weikuan Yu"],
       "year": 2020, "venue": "JCST", "doi": "https://doi.org/10.1007/s11390-020-9801-1", "arxiv_id": "",
       "url": "https://doi.org/10.1007/s11390-020-9801-1", "pdf_url": "https://upc.example/AdHocFileSystems.pdf",
       "file": "brinkmann2020_10-1007-s11390-020-9801-1.pdf", "pdf_downloaded": true, "source_engine": "openalex"},
      {"title": "An Asynchronous IO Runtime for Burst Buffers", "authors": ["Jane Roe"], "year": 2021, "venue": "arXiv",
       "doi": "", "arxiv_id": "2101.00001v1", "url": "https://arxiv.org/abs/2101.00001v1",
       "pdf_url": "https://arxiv.org/pdf/2101.00001v1", "file": "roe2021_arxiv-2101-00001v1.pdf",
       "pdf_downloaded": true, "source_engine": "arxiv"},
      {"title": "Something Entirely Unrelated", "authors": ["Max Mustermann"], "year": 1999, "venue": "",
       "doi": "", "arxiv_id": "", "url": "https://example.org/unrelated", "pdf_url": "",
       "file": "", "pdf_downloaded": false, "source_engine": "openalex"}
    ]"#;

    /// 取得ランナーのスタブ本体。`candidates.json` / `sources.json`（`cited` は全部 false）を書き、
    /// progress と `TASKD_ACQUIRE` を出す（実物のランナーの動きを最小限まねる）。
    fn acquire_stub_script(candidates: u32, pdfs: u32) -> String {
        format!(
            r#"input="$2"
cand=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['candidates_path'])" "$input")
src=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['sources_path'])" "$input")
papers=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['paper_directory'])" "$input")
mkdir -p "$(dirname "$cand")" "$papers"
echo 'progress: arxiv: 2 result(s)'
cat > "$cand" <<'JSON'
{STUB_CANDIDATES}
JSON
python3 - "$cand" "$src" <<'PY'
import json, sys
cands = json.load(open(sys.argv[1]))
out = [{{"url": c["url"], "title": c["title"], "engine": c["source_engine"], "cited": False}} for c in cands]
json.dump(out, open(sys.argv[2], "w"), indent=2)
PY
echo 'TASKD_ACQUIRE {{"candidates": {candidates}, "pdfs": {pdfs}, "engines": {{"arxiv": 1, "openalex": 2}}}}'
"#
        )
    }

    /// 上の候補のうち 2 件を引用する答え（PaperQA2 のファイル名由来の引用の形）。
    const STUB_ANSWER: &str = "Ad hoc file systems aggregate node-local NVMe \
(brinkmann2020_10-1007-s11390-020-9801-1.pdf), and asynchronous IO runtimes \
reduce server overhead (Roe 2021).";

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

    /// settings / paper_directory / index_directory / index_name の組み立てを argv でそのまま確認する
    /// （ADR-0027 D3。ADR-0035 D1 / D2 で corpus と索引が**案件ごと**になったので、案件が無い
    /// タスクでは `_shared` が足される）。
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
                format!("/papers/{SHARED_PROJECT_KEY}"),
                "--agent.index.index_directory".to_string(),
                format!("/index/{SHARED_PROJECT_KEY}"),
                "--agent.index.name".to_string(),
                SHARED_PROJECT_KEY.to_string(),
                "ask".to_string(),
                build_question(&req.task, &req.context),
            ]
        );
    }

    /// 設定を省略したときの既定値: `-s` は付かず、`paper_directory`/`index_directory`/`index_name` は
    /// ワークスペース相対・案件ごと（案件が無ければ `_shared`）の既定値になる。
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
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-6", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));

        let argv = read_argv(&dir.path().join("args.log"));
        assert!(!argv.contains(&"-s".to_string()));
        assert_eq!(argv[0], "--agent.index.paper_directory");
        assert_eq!(argv[1], format!("papers/{SHARED_PROJECT_KEY}"));
        assert_eq!(argv[2], "--agent.index.index_directory");
        assert_eq!(argv[3], format!("index/{SHARED_PROJECT_KEY}"));
        assert_eq!(argv[4], "--agent.index.name");
        assert_eq!(argv[5], SHARED_PROJECT_KEY.to_string());
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
    // ------------------------------------------------------------ ADR-0035（Phase 34）

    /// 依頼文が日本語でも、その中の英数字の名詞句が検索語になる（ADR-0035 D1 手順 1。決定的、LLM 無し）。
    /// 実機の依頼文そのままで確認する。
    #[test]
    fn build_search_queries_takes_the_ascii_noun_phrases_out_of_a_japanese_objective() {
        let objective = "Pluvio（ad-hoc FS の I/O サーバ向け非同期ランタイム）の隣接領域: \
             asynchronous I/O runtime, ad-hoc file system, I/O offload — 直近の研究動向と候補テーマ";
        let queries = build_search_queries(objective);
        assert!(queries.len() >= 2 && queries.len() <= MAX_SEARCH_QUERIES, "{queries:?}");
        assert!(queries.contains(&"asynchronous I/O runtime".to_string()), "{queries:?}");
        assert!(queries.contains(&"ad-hoc file system".to_string()), "{queries:?}");
        // 語数の多い句が先（relevance の当たりが良い順）。
        assert_eq!(queries[0].split(' ').count(), 3, "{queries:?}");
        // 日本語はそのまま検索語にしない（英語の検索 API に投げるため）。
        assert!(queries.iter().all(|q| q.is_ascii()), "{queries:?}");
    }

    #[test]
    fn build_search_queries_keeps_proper_nouns_and_drops_noise() {
        // 固有名詞 1 語（大文字を含む）は残す。
        let queries = build_search_queries("Pluvio を調べる");
        assert_eq!(queries, vec!["Pluvio".to_string()]);
        // ストップワードだけ・短い小文字 1 語は落とす。
        let queries = build_search_queries("of the あれ、to be な話");
        assert!(queries.is_empty() || queries.iter().all(|q| q != "of the"), "{queries:?}");
        // 他の句に丸ごと含まれる句は落とす。
        let queries = build_search_queries("ad-hoc file system、ad-hoc file system checkpointing について");
        assert_eq!(queries, vec!["ad-hoc file system checkpointing".to_string()]);
    }

    /// 英数字が 1 つも無ければ objective 全文を 1 本の検索語にする（最後の手段）。
    #[test]
    fn build_search_queries_falls_back_to_the_whole_objective() {
        let queries = build_search_queries("非同期ランタイムの\n研究動向");
        assert_eq!(queries, vec!["非同期ランタイムの 研究動向".to_string()]);
        assert!(build_search_queries("   ").is_empty());
    }

    /// corpus と索引の鍵は案件（`project_id`）。案件が無ければ `_shared`（ADR-0035 D1）。
    #[test]
    fn project_key_uses_the_project_id_or_shared() {
        let mut task = crate::protocol::tests::sample_task();
        assert_eq!(project_key(&task), SHARED_PROJECT_KEY);
        let project = task_core::org::ProjectId::new();
        task.project_id = Some(project);
        assert_eq!(project_key(&task), project.to_string());
    }

    /// 取得ランナーの python は、既定では `pqa` と同じディレクトリのもの（venv の中）。
    #[test]
    fn acquire_python_defaults_to_the_sibling_of_the_pqa_command() {
        let mut config = PaperQaConfig {
            command: "/home/u/taskd/paperqa/.venv/bin/pqa".to_string(),
            ..PaperQaConfig::default()
        };
        assert_eq!(acquire_python(&config), "/home/u/taskd/paperqa/.venv/bin/python3");
        config.command = "pqa".to_string();
        assert_eq!(acquire_python(&config), "python3");
        config.acquire.command = Some("/usr/bin/python3.12".to_string());
        assert_eq!(acquire_python(&config), "/usr/bin/python3.12");
    }

    /// `cited` の判定（ADR-0035 D2。決定的。合わなければ false）。
    #[test]
    fn answer_cites_matches_the_file_name_doi_arxiv_id_or_author_year() {
        let base = Candidate {
            title: "Ad Hoc File Systems for High-Performance Computing".to_string(),
            authors: vec!["Andre Brinkmann".to_string()],
            year: Some(2020),
            doi: "https://doi.org/10.1007/s11390-020-9801-1".to_string(),
            arxiv_id: String::new(),
            file: "brinkmann2020_10-1007-s11390-020-9801-1.pdf".to_string(),
            ..Candidate::default()
        };
        // ファイル名（PaperQA2 は use_doc_details = false でファイル名から引用の鍵を作る）
        assert!(answer_cites("see (brinkmann2020_10-1007-s11390-020-9801-1.pdf)", &base));
        // DOI
        assert!(answer_cites("as shown in 10.1007/s11390-020-9801-1", &base));
        // 著者姓 + 年
        assert!(answer_cites("prior work (Brinkmann 2020) shows", &base));
        assert!(answer_cites("prior work (Brinkmann2020) shows", &base));
        // タイトル
        assert!(answer_cites("Ad Hoc File Systems for High-Performance Computing is a survey", &base));
        // 何も合わなければ false
        assert!(!answer_cites("no evidence was found in the provided context", &base));

        let arxiv = Candidate {
            title: "An Asynchronous IO Runtime".to_string(),
            authors: vec!["Jane Roe".to_string()],
            year: Some(2021),
            arxiv_id: "2101.00001v2".to_string(),
            ..Candidate::default()
        };
        // 版番号の違いは無視する。
        assert!(answer_cites("see arXiv:2101.00001 for the runtime", &arxiv));
        assert!(!answer_cites("unrelated text", &arxiv));
    }

    /// ゲートの閾値の見方（ADR-0035 D3）。
    #[test]
    fn evidence_gate_counts_candidates_pdfs_and_citations() {
        let thresholds = PaperQaEvidence::default();
        assert_eq!(thresholds, PaperQaEvidence { min_candidates: 5, min_pdfs: 3, min_cited: 2 });
        assert!(evidence_gate(&thresholds, AcquireCounts { candidates: 6, pdfs: 3 }, 2).is_none());
        let message = evidence_gate(&thresholds, AcquireCounts { candidates: 4, pdfs: 1 }, 1).unwrap();
        assert!(message.contains("candidates=4 (min 5)"), "{message}");
        assert!(message.contains("pdfs=1 (min 3)"), "{message}");
        assert!(message.contains("cited=1 (min 2)"), "{message}");
        // 0 件は別メッセージ（検索経路の問題と区別する）。
        let zero = evidence_gate(&thresholds, AcquireCounts::default(), 0).unwrap();
        assert!(zero.contains("literature search returned nothing"), "{zero}");
        // 全部 0 ならゲート無し。
        let off = PaperQaEvidence { min_candidates: 0, min_pdfs: 0, min_cited: 0 };
        assert!(evidence_gate(&off, AcquireCounts::default(), 0).is_none());
    }

    /// ADR-0035 D2 / D4: 取得 → pqa の順に起動し、成果物 3 つを申告し、`answer.md` の末尾に `## 出典` が付き、
    /// `sources.json` の `cited` が答えとの突き合わせで決まる（ゲートは通る側）。
    #[tokio::test]
    async fn acquire_runs_before_pqa_and_the_answer_gets_a_sources_section() {
        let dir = tempfile::tempdir().unwrap();
        let order_log = dir.path().join("order.log");
        let config = stub_pqa_with_acquire(
            dir.path(),
            &format!(
                "echo pqa >> {order}\ncat >/dev/null\nprintf 'Answer: {answer}\\n'\n",
                order = order_log.display(),
                answer = STUB_ANSWER
            ),
            &format!("echo acquire >> {order}\n{script}", order = order_log.display(), script = acquire_stub_script(6, 3)),
        );
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req.clone(), "run-a1", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert!(summary.contains("Ad hoc file systems"), "{summary}"),
            other => panic!("expected done, got {other:?}"),
        }

        // 1. 順序（取得 → pqa）
        let order = std::fs::read_to_string(&order_log).unwrap();
        assert_eq!(order.lines().collect::<Vec<_>>(), vec!["acquire", "pqa"], "{order}");

        // 2. 取得ランナーは run ディレクトリに書き出されて起動される（埋め込みの本体そのまま）。
        let written = std::fs::read_to_string(dir.path().join("runs/run-a1/paperqa_acquire.py")).unwrap();
        assert_eq!(written, ACQUIRE_SCRIPT);
        let input: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("runs/run-a1/acquire_input.json")).unwrap())
                .unwrap();
        assert_eq!(input["max_candidates"], 30);
        assert_eq!(input["max_pdfs"], 12);
        assert!(input["paper_directory"].as_str().unwrap().ends_with(&format!("papers/{SHARED_PROJECT_KEY}")));
        assert!(!input["queries"].as_array().unwrap().is_empty());

        // 3. 成果物 3 つの申告
        let artifacts = sink.artifacts.lock().unwrap();
        let names: Vec<&str> = artifacts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["answer.md", "candidates.json", "sources.json"], "{names:?}");
        drop(artifacts);

        // 4. `cited` の突き合わせ（答えが引用した 2 件だけ true）
        let sources: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("artifacts/sources.json")).unwrap()).unwrap();
        let cited: Vec<bool> =
            sources.as_array().unwrap().iter().map(|s| s["cited"].as_bool().unwrap()).collect();
        assert_eq!(cited, vec![true, true, false], "{sources}");
        assert_eq!(sources[0]["engine"], "openalex");

        // 5. `## 出典`（引用されたものが先。`[n] 著者 (年). タイトル. venue. URL`）
        let answer_md = std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap();
        assert!(answer_md.starts_with("Ad hoc file systems aggregate"), "{answer_md}");
        let sources_section = answer_md.split("## 出典").nth(1).expect("出典の節があること");
        let lines: Vec<&str> = sources_section.lines().filter(|l| l.starts_with('[')).collect();
        assert_eq!(lines.len(), 3, "{sources_section}");
        assert!(lines[0].contains("Andre Brinkmann, Kathryn Mohror, Weikuan Yu (2020)"), "{}", lines[0]);
        assert!(lines[0].contains("Ad Hoc File Systems for High-Performance Computing"), "{}", lines[0]);
        assert!(lines[0].contains("JCST"), "{}", lines[0]);
        assert!(lines[0].contains("https://doi.org/10.1007/s11390-020-9801-1"), "{}", lines[0]);
        assert!(lines[0].contains("(引用)"), "{}", lines[0]);
        assert!(!lines[2].contains("(引用)"), "引用されていないものは後ろ: {}", lines[2]);

        assert!(dir.path().join("artifacts/result.json").is_file());
        assert!(dir.path().join("runs/run-a1/acquire.stdout.log").is_file());
    }

    /// ADR-0035 D3: 閾値に足りなければ `Error{retryable}`。**成果物は残す**（`artifacts/result.json` は書かない）。
    #[tokio::test]
    async fn gate_rejects_short_evidence_but_keeps_the_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa_with_acquire(
            dir.path(),
            &format!("cat >/dev/null\nprintf 'Answer: {STUB_ANSWER}\\n'\n"),
            &acquire_stub_script(3, 1),
        );
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-a2", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("insufficient literature evidence"), "{message}");
                assert!(message.contains("candidates=3 (min 5)"), "{message}");
                assert!(message.contains("pdfs=1 (min 3)"), "{message}");
                // 引用は 2 件あるので、その項目は文面に出ない。
                assert!(!message.contains("cited="), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        // 人が読めるように残る。
        assert!(dir.path().join("artifacts/answer.md").is_file());
        assert!(dir.path().join("artifacts/candidates.json").is_file());
        assert!(dir.path().join("artifacts/sources.json").is_file());
        // 成果物の申告はゲートより前に行うので、落ちても 3 件申告される。
        assert_eq!(sink.artifacts.lock().unwrap().len(), 3);
        // ワーカープロトコル上は done ではないので `artifacts/result.json` は書かない。
        assert!(!dir.path().join("artifacts/result.json").exists());
        // 供給側の失敗にはしない（プロバイダを cooldown にする話ではない）。
        let run_result = std::fs::read_to_string(dir.path().join("runs/run-a2/result.json")).unwrap();
        assert!(!run_result.contains("provider_failure"), "{run_result}");
    }

    /// ADR-0035 D3: 取得が 0 件のときだけ別メッセージ（検索経路の問題と区別する）。
    #[tokio::test]
    async fn gate_zero_candidates_uses_the_distinct_message() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa_with_acquire(
            dir.path(),
            "cat >/dev/null\necho 'Answer: I could not find any relevant work.'\n",
            "echo 'progress: arxiv: 0 result(s)'\necho 'TASKD_ACQUIRE {\"candidates\": 0, \"pdfs\": 0, \"engines\": {}}'\n",
        );
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-a3", default_limits(), &sink).await.unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert_eq!(message, "literature search returned nothing (possible network or API problem)");
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(dir.path().join("artifacts/answer.md").is_file(), "答えは残す");
    }

    /// 取得ランナーが起動できない／落ちても run はそこで止めず、`pqa` まで進む（判定はゲートが行う）。
    #[tokio::test]
    async fn a_failing_acquire_runner_does_not_stop_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_pqa_with_acquire(
            dir.path(),
            "cat >/dev/null\necho 'Answer: answered from the existing corpus.'\n",
            "echo 'boom' 1>&2\nexit 3\n",
        );
        // ゲートを切っておけば（既存 corpus だけで答える運用）取得の失敗でも done になる。
        config.evidence = PaperQaEvidence { min_candidates: 0, min_pdfs: 0, min_cited: 0 };
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-a4", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }), "{:?}", outcome.terminal);
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m.contains("literature acquisition failed")), "{progress:?}");
    }

    /// `max_candidates = 0` なら取得の段を行わず、ゲートも見ない（従来どおり手元の corpus だけで答える）。
    #[tokio::test]
    async fn acquire_and_the_gate_are_skipped_when_max_candidates_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        // `stub_pqa` は `max_candidates = 0`。
        let config = stub_pqa(dir.path(), "cat >/dev/null\necho 'Answer: from the local corpus only.'\n");
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-a5", default_limits(), &sink).await.unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        assert!(!dir.path().join("runs/run-a5/paperqa_acquire.py").exists());
        assert!(!dir.path().join("artifacts/candidates.json").exists());
        let answer_md = std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap();
        assert!(!answer_md.contains("## 出典"), "{answer_md}");
        assert_eq!(sink.artifacts.lock().unwrap().len(), 1, "answer.md だけ");
    }

    // ---------------------------------------------------- 取得ランナー（python3、ネットワーク無し）

    fn python3_available() -> bool {
        match std::process::Command::new("python3").arg("--version").output() {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
    }

    /// `--fixture <dir>` 用の応答（本物の API の形をそのまま小さくしたもの。ADR-0035 の実機確認に基づく）。
    /// arXiv の 2 件目は OpenAlex の 1 件目と **DOI が同じ**、2 本目の検索語の結果は 1 件目と
    /// **タイトルが同じ**（記号違い）ので、DOI とタイトル正規化の両方の重複排除が効く。
    fn write_fixtures(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("arxiv-1.xml"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2101.00001v1</id>
    <title>An Asynchronous IO Runtime</title>
    <published>2021-01-02T00:00:00Z</published>
    <author><name>Jane Roe</name></author>
    <link href="https://arxiv.org/abs/2101.00001v1" rel="alternate" type="text/html"/>
    <link href="https://arxiv.org/pdf/2101.00001v1" rel="related" type="application/pdf" title="pdf"/>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2202.00002v1</id>
    <title>Ad Hoc File Systems (preprint)</title>
    <published>2022-02-03T00:00:00Z</published>
    <author><name>Andre Brinkmann</name></author>
    <arxiv:doi>10.1007/s11390-020-9801-1</arxiv:doi>
    <link href="https://arxiv.org/pdf/2202.00002v1" rel="related" type="application/pdf" title="pdf"/>
  </entry>
</feed>
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("arxiv-2.xml"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2303.00003v1</id>
    <title>An  Asynchronous  IO Runtime!</title>
    <published>2023-03-04T00:00:00Z</published>
    <author><name>Jane Roe</name></author>
    <link href="https://arxiv.org/pdf/2303.00003v1" rel="related" type="application/pdf" title="pdf"/>
  </entry>
</feed>
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("openalex-1.json"),
            r#"{"results": [
  {"id": "https://openalex.org/W3004116193", "doi": "https://doi.org/10.1007/s11390-020-9801-1",
   "title": "Ad Hoc File Systems for High-Performance Computing", "publication_year": 2020,
   "primary_location": {"source": {"display_name": "Journal of Computer Science and Technology"}, "pdf_url": null},
   "best_oa_location": {"pdf_url": "https://upc.example/AdHocFileSystems.pdf"},
   "open_access": {"oa_url": "https://upc.example/AdHocFileSystems.pdf"},
   "authorships": [{"author": {"display_name": "Andre Brinkmann"}}, {"author": {"display_name": "Kathryn Mohror"}}]},
  {"id": "https://openalex.org/W1", "doi": "https://doi.org/10.1/zzz", "title": "Something Unrelated",
   "publication_year": 1999, "primary_location": {"source": {"display_name": "Old Journal"}},
   "best_oa_location": {}, "open_access": {}, "authorships": [{"author": {"display_name": "Max Mustermann"}}]}
]}
"#,
        )
        .unwrap();
        // 2 本目の検索語の OpenAlex は用意しない → ランナーは空の結果として扱う。
    }

    /// ADR-0035 §4.1: 本物の API を叩かずに（`--fixture`）、重複排除・上限・案件ごとの corpus・
    /// `candidates.json` / `sources.json` の形・`TASKD_ACQUIRE` を確認する。
    #[test]
    fn runner_acquires_from_fixtures_with_dedup_limits_and_the_project_corpus() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("paperqa_acquire.py");
        std::fs::write(&script_path, ACQUIRE_SCRIPT).unwrap();
        let fixture = dir.path().join("fixture");
        write_fixtures(&fixture);
        let corpus = dir.path().join("papers").join("01PROJECT");
        let input_path = dir.path().join("acquire_input.json");
        let input = serde_json::json!({
            "queries": ["asynchronous I/O runtime", "ad-hoc file system"],
            "paper_directory": corpus.to_string_lossy(),
            "candidates_path": dir.path().join("artifacts/candidates.json").to_string_lossy(),
            "sources_path": dir.path().join("artifacts/sources.json").to_string_lossy(),
            "max_candidates": 3,
            "max_pdfs": 1,
            "per_query": 20,
            "timeout_secs": 5,
            "mailto": "who@example.org",
        });
        std::fs::write(&input_path, serde_json::to_string_pretty(&input).unwrap()).unwrap();

        let output = std::process::Command::new("python3")
            .arg(&script_path)
            .arg(&input_path)
            .arg("--fixture")
            .arg(&fixture)
            .output()
            .expect("failed to run python3");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("progress: arxiv: 2 result(s)"), "{stdout}");

        let result_line = stdout.lines().find(|l| l.starts_with(ACQUIRE_RESULT_PREFIX)).expect("TASKD_ACQUIRE");
        let counts: serde_json::Value =
            serde_json::from_str(result_line.trim_start_matches(ACQUIRE_RESULT_PREFIX)).unwrap();
        // 5 件返ってきたうち、DOI 一致とタイトル一致の 2 件が畳まれて 3 件。
        assert_eq!(counts["candidates"], 3, "{counts}");
        assert_eq!(counts["pdfs"], 1, "max_pdfs = 1 なので 1 本だけ: {counts}");
        assert_eq!(counts["engines"]["arxiv"], 1, "{counts}");
        assert_eq!(counts["engines"]["openalex"], 2, "{counts}");

        let candidates: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("artifacts/candidates.json")).unwrap())
                .unwrap();
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0]["title"], "An Asynchronous IO Runtime");
        assert_eq!(candidates[0]["arxiv_id"], "2101.00001v1");
        assert_eq!(candidates[0]["source_engine"], "arxiv");
        assert_eq!(candidates[0]["pdf_downloaded"], true);
        assert_eq!(candidates[0]["file"], "roe2021_arxiv-2101-00001v1.pdf");
        // DOI が同じ arXiv の preprint は OpenAlex 側の 1 件に畳まれ、arXiv id が補われる。
        assert_eq!(candidates[1]["title"], "Ad Hoc File Systems for High-Performance Computing");
        assert_eq!(candidates[1]["doi"], "https://doi.org/10.1007/s11390-020-9801-1");
        assert_eq!(candidates[1]["arxiv_id"], "2202.00002v1");
        assert_eq!(candidates[1]["venue"], "Journal of Computer Science and Technology");
        assert_eq!(candidates[1]["year"], 2020);
        assert_eq!(candidates[1]["pdf_downloaded"], false, "max_pdfs を超えた分は落とさない");
        // 3 件目は PDF の URL が無い候補（それでも候補としては残る）。
        assert_eq!(candidates[2]["title"], "Something Unrelated");
        assert_eq!(candidates[2]["pdf_url"], "");

        // 案件ごとの corpus にだけ書かれ、PDF は 1 本。
        let mut files: Vec<String> =
            std::fs::read_dir(&corpus).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        files.sort();
        assert_eq!(files, vec!["roe2021_arxiv-2101-00001v1.pdf".to_string()]);
        assert!(std::fs::read(corpus.join(&files[0])).unwrap().starts_with(b"%PDF"));

        // `sources.json` は LDR と同じ 4 つの鍵だけ。`cited` はこの時点では全部 false（ADR-0035 D2）。
        let sources: Vec<serde_json::Map<String, serde_json::Value>> =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("artifacts/sources.json")).unwrap())
                .unwrap();
        assert_eq!(sources.len(), 3);
        for source in &sources {
            let mut keys: Vec<&str> = source.keys().map(|k| k.as_str()).collect();
            keys.sort();
            assert_eq!(keys, vec!["cited", "engine", "title", "url"], "{source:?}");
            assert_eq!(source["cited"], false);
        }
        assert_eq!(sources[0]["url"], "https://arxiv.org/abs/2101.00001v1");
    }

    /// 既に corpus にある PDF は取り直さない（ADR-0035 D1 手順 3）。
    #[test]
    fn runner_does_not_re_download_a_pdf_that_is_already_in_the_corpus() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("paperqa_acquire.py");
        std::fs::write(&script_path, ACQUIRE_SCRIPT).unwrap();
        let fixture = dir.path().join("fixture");
        write_fixtures(&fixture);
        let corpus = dir.path().join("papers").join("01PROJECT");
        std::fs::create_dir_all(&corpus).unwrap();
        let existing = corpus.join("roe2021_arxiv-2101-00001v1.pdf");
        std::fs::write(&existing, b"%PDF-1.4 already here\n").unwrap();

        let input_path = dir.path().join("acquire_input.json");
        let input = serde_json::json!({
            "queries": ["asynchronous I/O runtime"],
            "paper_directory": corpus.to_string_lossy(),
            "candidates_path": dir.path().join("artifacts/candidates.json").to_string_lossy(),
            "sources_path": dir.path().join("artifacts/sources.json").to_string_lossy(),
            "max_candidates": 30,
            "max_pdfs": 1,
            "per_query": 20,
        });
        std::fs::write(&input_path, serde_json::to_string_pretty(&input).unwrap()).unwrap();
        let output = std::process::Command::new("python3")
            .arg(&script_path)
            .arg(&input_path)
            .arg("--fixture")
            .arg(&fixture)
            .output()
            .expect("failed to run python3");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(stdout.contains("already in the corpus: roe2021_arxiv-2101-00001v1.pdf"), "{stdout}");
        // 上書きされていない（= 取り直していない）。corpus にある分は PDF 数に数える。
        assert_eq!(std::fs::read(&existing).unwrap(), b"%PDF-1.4 already here\n");
        let result_line = stdout.lines().find(|l| l.starts_with(ACQUIRE_RESULT_PREFIX)).unwrap();
        let counts: serde_json::Value =
            serde_json::from_str(result_line.trim_start_matches(ACQUIRE_RESULT_PREFIX)).unwrap();
        assert_eq!(counts["pdfs"], 1, "{counts}");
    }

    /// ランナーの純粋な部分（検索 URL の組み立て・正規化・重複排除・ファイル名）を python3 で直接確認する
    /// （LDR の `runner_*` テストと同じ作り。ネットワークには出ない）。
    #[test]
    fn runner_url_building_normalization_and_dedup_are_deterministic() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("paperqa_acquire.py");
        std::fs::write(&script_path, ACQUIRE_SCRIPT).unwrap();
        let checker = r##"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("acq", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
out = {}
# 引用符で括らない（実機で `all:"ad-hoc file system"` は 0 件だった。ADR-0035）
out["arxiv_url"] = mod.arxiv_url("ad-hoc file system", 20)
out["openalex_url"] = mod.openalex_url("ad-hoc file system", 20, "who@example.org")
out["openalex_url_no_mailto"] = mod.openalex_url("x", 5)
out["normalize_title"] = mod.normalize_title("An  Asynchronous, IO Runtime!")
out["normalize_doi"] = mod.normalize_doi("HTTPS://doi.org/10.1/AbC/")
out["normalize_arxiv"] = mod.normalize_arxiv_id("2101.00001v3")
out["interleave"] = mod.interleave([["a1", "a2", "a3"], ["b1"], []])
docs = [
  {"title": "T One", "doi": "10.1/x", "arxiv_id": "", "pdf_url": ""},
  {"title": "T  one!", "doi": "", "arxiv_id": "2101.1v1", "pdf_url": "p"},
  {"title": "Other", "doi": "https://doi.org/10.1/X", "arxiv_id": "", "pdf_url": ""},
  {"title": "Third", "doi": "", "arxiv_id": "", "pdf_url": ""},
]
deduped = mod.dedupe_candidates(docs, 10)
out["dedup_titles"] = [d["title"] for d in deduped]
out["dedup_filled_arxiv"] = deduped[0]["arxiv_id"]
out["dedup_limit"] = [d["title"] for d in mod.dedupe_candidates(docs, 1)]
out["pdf_filename"] = mod.pdf_filename({"authors": ["Andre Brinkmann"], "year": 2020, "doi": "10.1007/s11390-020-9801-1", "title": "t"})
out["pdf_filename_anon"] = mod.pdf_filename({"authors": [], "year": None, "title": "A Title Here"})
print(json.dumps(out))
"##;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(checker)
            .arg(&script_path)
            .output()
            .expect("failed to run python3");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let v: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        let arxiv_url = v["arxiv_url"].as_str().unwrap();
        assert!(arxiv_url.starts_with("https://export.arxiv.org/api/query?"), "{arxiv_url}");
        assert!(arxiv_url.contains("search_query=all%3Aad-hoc+file+system"), "{arxiv_url}");
        assert!(!arxiv_url.contains("%22"), "引用符で括らない: {arxiv_url}");
        assert!(arxiv_url.contains("sortBy=relevance"), "{arxiv_url}");
        assert!(arxiv_url.contains("max_results=20"), "{arxiv_url}");
        let openalex_url = v["openalex_url"].as_str().unwrap();
        assert!(openalex_url.starts_with("https://api.openalex.org/works?"), "{openalex_url}");
        assert!(openalex_url.contains("filter=is_oa%3Atrue"), "{openalex_url}");
        assert!(openalex_url.contains("per_page=20"), "{openalex_url}");
        assert!(openalex_url.contains("mailto=who%40example.org"), "{openalex_url}");
        assert!(!v["openalex_url_no_mailto"].as_str().unwrap().contains("mailto"), "{v}");
        assert_eq!(v["normalize_title"], "anasynchronousioruntime");
        assert_eq!(v["normalize_doi"], "10.1/abc");
        assert_eq!(v["normalize_arxiv"], "2101.00001");
        assert_eq!(v["interleave"], serde_json::json!(["a1", "b1", "a2", "a3"]));
        // 2 件目はタイトル一致、3 件目は DOI の大文字小文字違いで 1 件目に畳まれる。
        assert_eq!(v["dedup_titles"], serde_json::json!(["T One", "Third"]));
        assert_eq!(v["dedup_filled_arxiv"], "2101.1v1", "畳んだ側の欠けた項目を補う");
        assert_eq!(v["dedup_limit"], serde_json::json!(["T One"]));
        assert_eq!(v["pdf_filename"], "brinkmann2020_10-1007-s11390-020-9801-1.pdf");
        assert_eq!(v["pdf_filename_anon"], "anonnd_a-title-here.pdf");
    }

    /// `%PDF` で始まらない応答（HTML のログインページ等）は corpus に入れない。
    #[test]
    fn runner_rejects_a_response_that_is_not_a_pdf() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("paperqa_acquire.py");
        std::fs::write(&script_path, ACQUIRE_SCRIPT).unwrap();
        let fixture = dir.path().join("fixture");
        write_fixtures(&fixture);
        // arXiv の PDF の URL に HTML を返させる。
        std::fs::write(fixture.join("pdf-2101.00001v1"), "<html>login required</html>").unwrap();
        let corpus = dir.path().join("papers/_shared");
        let input_path = dir.path().join("acquire_input.json");
        let input = serde_json::json!({
            "queries": ["asynchronous I/O runtime"],
            "paper_directory": corpus.to_string_lossy(),
            "candidates_path": dir.path().join("artifacts/candidates.json").to_string_lossy(),
            "sources_path": dir.path().join("artifacts/sources.json").to_string_lossy(),
            "max_candidates": 30,
            "max_pdfs": 1,
            "per_query": 20,
        });
        std::fs::write(&input_path, serde_json::to_string_pretty(&input).unwrap()).unwrap();
        let output = std::process::Command::new("python3")
            .arg(&script_path)
            .arg(&input_path)
            .arg("--fixture")
            .arg(&fixture)
            .output()
            .expect("failed to run python3");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(stdout.contains("not a PDF, skipped"), "{stdout}");
        assert!(!corpus.join("roe2021_arxiv-2101-00001v1.pdf").exists());
    }
}
