//! `paperqa` アダプタ（DESIGN §5.4, ADR-0027 D3、ADR-0035 で「取得」の段を追加、
//! ADR-0063 Phase 109d C1/C2 で `pqa ask` CLI を PaperQA の Python API に置き換え）。
//!
//! PaperQA2 は celeris のワーカープロトコルもストリーム型の進捗形式も話さない、ただの調査エンジンで
//! ある。**アダプタ自身が** ADR-0006 D3 の結果ファイル規約（`artifacts/result.json`）を代わりに書き、
//! `Terminal::Done`/`Terminal::Error` を合成する。委譲（`delegate.json`）は扱わない
//! （ADR-0027 D3: 「委譲はしない」）。生存監視（wall-clock・無出力タイムアウト・SIGTERM→SIGKILL）は
//! `subprocess.rs` の低レベル部分を再利用する。
//!
//! ADR-0035: 1 run は **2 段**になった。
//!
//! 1. **取得** — 埋め込みの Python ランナー（`paperqa_acquire.py`）を `runs/<run_id>/` に書き出して起動し、
//!    arXiv / OpenAlex（鍵無し）で候補論文を集め、open access の PDF を**案件ごとの** corpus
//!    （`paper_directory/<project_id>/`）に落とす。**検索語はランナーの最初の段で LLM が立てる**
//!    （ADR-0035 D5 / Phase 36。PaperQA2 と同じ LLM 先に `chat/completions` を 1 回。答えが壊れて
//!    いれば、このアダプタが `objective` から決定的に抜いた語（`build_search_queries`）に落ちる）。
//! 2. **索引と回答** — ADR-0063 Phase 109d C1: `pqa ask` CLI ではなく、もう 1 つの埋め込み Python
//!    ランナー（`paperqa_ask.py`）が `paperqa.ask`/`Settings.from_name` を呼ぶ。対象（目的文から
//!    決定的に抜いた `research_targets`）が取れていれば対象ごと + 総括の複数回の `ask()`、取れなければ
//!    従来の単一の問い。`paper_directory` / `index_directory` / 索引名は案件ごと。
//!
//! 回答の後に、`ask()` が実際に使った証拠（`PQASession.contexts` の `docname`/`dockey`）と答えの本文
//! （文字列一致、補助）の和で `cited` を決定的に突き合わせ、`artifacts/sources.json` を書き直し、
//! `artifacts/answer.md`（対象が取れていれば対象別の表 + 節、引用された文献の一覧）の末尾に
//! `## 出典` を足し、証拠ゲート（ADR-0035 D3）で候補数・PDF 数・引用数を機械的に判定する。

use std::collections::{BTreeMap, BTreeSet};
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
use crate::progress;
use crate::protocol::{Answer, RunContext, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, kill_now, read_line_limited, read_tail, reap_after_terminal,
    write_result_json,
};

/// run ごとに `runs/<run_id>/paperqa_acquire.py` として書き出す取得ランナー（ADR-0035 D1）。
const ACQUIRE_SCRIPT: &str = include_str!("paperqa_acquire.py");
/// 取得ランナーの最終行の目印（ADR-0035 D1）。
const ACQUIRE_RESULT_PREFIX: &str = "CELERIS_ACQUIRE ";
/// 取得ランナーの進捗行の目印。
const PROGRESS_PREFIX: &str = "progress:";
/// run ごとに `runs/<run_id>/paperqa_ask.py` として書き出す、PaperQA の Python API を呼ぶランナー
/// （ADR-0063 Phase 109d C1。`pqa ask` CLI の置き換え）。
const ASK_SCRIPT: &str = include_str!("paperqa_ask.py");
/// `paperqa_ask.py` が `paperqa` を import できなかったときの exit code（ADR-0063 Phase 109d C1）。
const ASK_EXIT_PAPERQA_NOT_IMPORTABLE: i32 = 3;
/// `paperqa_ask.py` の引数の誤り、または `settings_path`/`Settings.from_name` のどちらでも設定が
/// 見つからなかったときの exit code（ADR-0063 Phase 109e）。どちらも設定の誤りで再試行しても
/// 直らないので `retryable: false` にする。
const ASK_EXIT_BAD_USAGE_OR_SETTINGS_NOT_FOUND: i32 = 2;
/// `artifacts/result.json` の `summary` の上限（ADR-0027 D3）。
const SUMMARY_MAX_CHARS: usize = 1500;
/// `progress` に転送する 1 行あたりの上限（他アダプタと同じ規則。ADR-0026 の `truncate` を踏襲）。
const PROGRESS_LINE_MAX_CHARS: usize = 500;
/// 案件（`project_id`）が無いタスクの corpus / 索引の名前（ADR-0035 D1）。
const SHARED_PROJECT_KEY: &str = "_shared";
/// `objective` から作る検索語の本数の上限（ADR-0035 D1: 「2〜4 本」）。
const MAX_SEARCH_QUERIES: usize = 4;
/// LLM に立てさせる検索語の本数の上限（ADR-0035 D5: 「3〜6 本」）。
const MAX_LLM_SEARCH_QUERIES: u32 = 6;
/// 検索語を立てる `chat/completions` の `max_tokens`（ADR-0035 D5。実機で Qwen3 は
/// 「考える」分を含めるとこれくらい必要）。
const QUERY_LLM_MAX_TOKENS: u32 = 2000;
/// 検索語を立てる LLM に渡す案件の文脈の上限（字数）。
const QUERY_CONTEXT_MAX_CHARS: usize = 2000;

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
    /// ADR-0035 D5（Phase 36）: **検索語を LLM に立てさせる**（既定 `true`）。`false` にすると
    /// Phase 34 までの決定的な抽出（`build_search_queries`）だけを使う。`true` でも LLM の答えが
    /// 壊れていれば決定的な抽出に落ちる（ランナーが `progress:` に出す）。
    #[serde(default = "default_query_llm")]
    pub query_llm: bool,
    /// 検索語を立てる LLM のモデル名。未指定なら `[[providers]] model`（`--llm`）→ PaperQA の
    /// `settings` の `llm` の順に使う（**PaperQA2 と同じ LLM 先**。ADR-0035 D5）。
    /// LiteLLM の `provider/model` 形式の接頭辞（`openai/`）は落として渡す。
    #[serde(default)]
    pub query_model: Option<String>,
    /// 検索語を立てる 1 回の `chat/completions` のタイムアウト（秒）。ローカル LLM は遅い。
    #[serde(default = "default_query_timeout_secs")]
    pub query_timeout_secs: u64,
    /// OpenAlex の `filter=`。未指定ならランナーの既定
    /// `is_oa:true,primary_topic.field.id:17`（open access かつ Computer Science。ADR-0035 D5）。
    /// 計算機科学以外の分野で使うときだけ書き換える。
    #[serde(default)]
    pub openalex_filter: Option<String>,
    /// ADR-0063 D1（Phase 109）: 本文（PDF）が取れない候補は**アブストラクトで妥協する**（既定 `true`）。
    /// OpenAlex / arXiv / Unpaywall / Semantic Scholar のメタデータにある abstract をテキスト文書として
    /// corpus に入れる（`<key>_abstract.txt`。本文ではないことを明記した注記付き）。`false` にすると
    /// Phase 108 までどおり、PDF が無い候補は corpus に入らない。
    #[serde(default = "default_abstract_fallback")]
    pub abstract_fallback: bool,
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
            query_llm: default_query_llm(),
            query_model: None,
            query_timeout_secs: default_query_timeout_secs(),
            openalex_filter: None,
            abstract_fallback: default_abstract_fallback(),
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
fn default_query_llm() -> bool {
    true
}
fn default_query_timeout_secs() -> u64 {
    300
}
fn default_abstract_fallback() -> bool {
    true
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
    /// ADR-0063 D1（Phase 109）: 閾値未達（`min_cited` 等）を hard error にするか（既定 `false`）。
    /// `false`（既定）なら証拠不足でも `Terminal::Done` にし、`research.json` の `evidence` と
    /// `answer.md` の「証拠の質」節に内訳と代替案を書いて reviewer / 受け入れ条件の判断に委ねる。
    /// `true` にすると Phase 108 までどおり `Terminal::Error{retryable: true}`。
    /// 取得が 0 件（検索経路の問題）は、この設定に関係なく常に hard error のまま。
    #[serde(default)]
    pub insufficient_is_error: bool,
}

impl Default for PaperQaEvidence {
    fn default() -> Self {
        Self {
            min_candidates: default_min_candidates(),
            min_pdfs: default_min_pdfs(),
            min_cited: default_min_cited(),
            insufficient_is_error: false,
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

/// `[adapters.paperqa]`（config.toml, ADR-0027 D3）。`[[providers]] adapter = "paperqa"` の行ごとに
/// `settings` / `env` / `model` を上書きできる（ADR-0026 D2 と同じ作り）。
#[derive(Debug, Clone)]
pub struct PaperQaConfig {
    /// ADR-0063 Phase 109d C2: 起動するコマンド。**`pqa` CLI ではなく python インタプリタ**
    /// （`paperqa_ask.py` を `<command> <script> <input.json>` として起動する。既定 `"python"`、
    /// 通常は `paperqa` パッケージが入った venv の `bin/python`）。旧 `pqa`（Phase 108 までの既定、
    /// または運用者が明示的に指している場合）が来たら、同じディレクトリの `python` に自動で
    /// 置き換えて 1 回警告する（`resolve_ask_command`）。
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
    /// `settings.llm` の上書き。設定されているときだけ渡す（PaperQA の設定ファイルの値より優先。
    /// ADR-0027 D3。ADR-0063 Phase 109d C1/C2: CLI の `--llm` から `paperqa_ask.py` への
    /// `input.json` の `model` フィールドに変わった。意味は同じ）。
    pub model: Option<String>,
    /// 追加の環境変数（例: `OPENAI_API_KEY` / `OPENAI_BASE_URL`。LiteLLM 経由の OpenAI 互換エンドポイント向け）。
    pub env: Vec<(String, String)>,
    /// Phase 108 までの `pqa ask` CLI 用の追加引数。ADR-0063 Phase 109d C1/C2 で CLI 呼び出しを
    /// 廃止したため、いまは何もしない（設定ファイルの互換性のためフィールドだけ残す）。
    pub extra_args: Vec<String>,
    /// ADR-0035 D1: 文献の取得。
    pub acquire: AcquireConfig,
    /// ADR-0035 D3: 決定的な証拠ゲートの閾値。
    pub evidence: PaperQaEvidence,
    /// ADR-0063 Phase 109d C3: `ask()` を呼ぶ回数の上限（対象ごとの問い + 総括の問い）。既定 10
    /// （Phase 109h: 8 から引き上げ）。比較先があるときは総括の問いを必ず残し、対象側を後ろから
    /// 詰める（`build_questions_for_targets`、`paperqa_ask.py`）。
    pub max_asks: u32,
    /// ADR-0063 Phase 109g A: `[knowledge] root`（絶対パス）。設定されていれば `paperqa_ask.py` が
    /// 比較先の設計条件（知識ベースのページ本文）をここから直接読む（`ask_input.json` の
    /// `comparison_context.knowledge_root`）。`None` ならページ本文は使わず、目的文の周辺 1 文に
    /// 落ちる（`research_targets::comparison_target_paragraph`）。コンテナで走る run では
    /// `container.knowledge_root` と同じ絶対パスがマウントされているので、そのまま読める。
    pub knowledge_root: Option<PathBuf>,
}

impl Default for PaperQaConfig {
    fn default() -> Self {
        Self {
            command: "python".to_string(),
            settings: None,
            paper_directory: None,
            index_directory: None,
            index_name: None,
            model: None,
            env: Vec::new(),
            extra_args: Vec::new(),
            acquire: AcquireConfig::default(),
            evidence: PaperQaEvidence::default(),
            max_asks: default_max_asks(),
            knowledge_root: None,
        }
    }
}

fn default_max_asks() -> u32 {
    10
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
pub fn build_question(task: &Task, context: &RunContext, artifacts: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", task.title));
    // ADR-0033 D4 / D6（Phase 24）: 前置き（役職と brief・永続の認可・記憶・直近のやり取り・役割の指示文）は
    // `crate::preamble` が 1 か所で組む。`RunContext` が Phase 23 までの中身なら出力は変わらない。
    out.push_str(&crate::preamble::render(context, artifacts));
    out.push_str(&task.objective);
    out.push('\n');
    // ADR-0063 Phase 109c A/C（縮小版。P-109c-1）: 目的文から対象が取れているとき、答えを
    // 「対象ごとの節 + 対象×観点の表」に構造化するよう指示する。対象ごとに `pqa ask` を複数回呼ぶ
    // （元の ADR C2）と PaperQA の Python API への切り替え（C1）は、実機で API 面を確認できず
    // 既存テストへの影響も大きいため Phase 109c では見送った（未解決事項として PROGRESS.md に記載）。
    let targets = crate::research_targets::research_targets(&task.objective);
    if !targets.is_empty() {
        let aspects = crate::research_targets::research_aspects(&task.objective);
        out.push_str(&render_structured_answer_instructions(&targets, &aspects));
    }
    if !context.answers.is_empty() {
        out.push_str("\n## Answers from a human to earlier questions\n");
        for Answer { question, answer } in &context.answers {
            out.push_str(&format!("- Q: {question}\n  A: {answer}\n"));
        }
    }
    out
}

/// ADR-0063 Phase 109c A/C: 対象が取れているときに `pqa ask` へ足す、答えの形式についての指示。
/// 各観点は事実（出典）か「未確認」のどちらかで書かせ、対象ごとの節に加えて対象×観点の表もまとめさせる
/// （観測された失敗: 対象別の整理が無い総論だけの答え、引用の対応不足）。
fn render_structured_answer_instructions(targets: &[String], aspects: &[String]) -> String {
    let mut out = String::from("\n## 回答の形式（必ず守ること）\n\n");
    out.push_str(
        "次の対象それぞれについて `### <対象名>` の見出しを立て、観点ごとに \
         `- <観点>: <文献にある事実（出典）>` または `- <観点>: 未確認` の形で書け。\
         文献に無い観点は推測せず「未確認」と書け。対象ごとの節のあとに、対象を行、観点を列とする \
         Markdown の表でまとめよ（セルは同じく事実（出典）か「未確認」）。\n\n",
    );
    out.push_str(&format!("対象: {}\n", targets.join("、")));
    out.push_str(&format!("観点: {}\n", aspects.join("、")));
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
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.trim_matches('_').is_empty() {
        SHARED_PROJECT_KEY.to_string()
    } else {
        safe
    }
}

/// ADR-0063 D1（Phase 109）: 起点の資料（目的文や `inputs` に含まれる URL）の種類。決定的な分類
/// （LLM は使わない）。`Pdf` / `Doi` / `Arxiv` は取得ランナーに渡して論文として取り込み、`Github` は
/// 「一次情報（実装）」として `answer.md` の参照節に載せる（corpus には入れない）。`Other` は何もしない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedUrlKind {
    Pdf,
    Doi,
    Arxiv,
    Github,
    Other,
}

impl SeedUrlKind {
    fn as_str(self) -> &'static str {
        match self {
            SeedUrlKind::Pdf => "pdf",
            SeedUrlKind::Doi => "doi",
            SeedUrlKind::Arxiv => "arxiv",
            SeedUrlKind::Github => "github",
            SeedUrlKind::Other => "other",
        }
    }
}

/// ADR-0063 D1: 目的文や `inputs` から拾った 1 件の起点 URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedUrl {
    pub url: String,
    pub kind: SeedUrlKind,
}

/// URL を種類ごとに分類する（決定的。ADR-0063 D1）。
pub fn classify_seed_url(url: &str) -> SeedUrlKind {
    let lower = url.to_ascii_lowercase();
    let without_query = lower.split(['?', '#']).next().unwrap_or(&lower);
    if lower.contains("arxiv.org") {
        SeedUrlKind::Arxiv
    } else if lower.contains("github.com") || lower.contains("gitlab.com") {
        SeedUrlKind::Github
    } else if lower.contains("doi.org/") {
        SeedUrlKind::Doi
    } else if without_query.ends_with(".pdf") {
        SeedUrlKind::Pdf
    } else {
        SeedUrlKind::Other
    }
}

/// テキストの中の `http(s)://` で始まるトークンを全て拾う（決定的、正規表現は使わない。ADR-0063 D1）。
/// 前後の日本語の括弧・句読点や引用符は落とす。
pub fn extract_urls(text: &str) -> Vec<String> {
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

/// 起点の資料（目的文 + `inputs` の `path`）から seed URL を集める（重複排除、出現順。ADR-0063 D1）。
pub fn extract_seed_urls(objective: &str, inputs: &[task_core::ArtifactRef]) -> Vec<SeedUrl> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    let mut candidates: Vec<String> = extract_urls(objective);
    for input in inputs {
        candidates.extend(extract_urls(&input.path));
    }
    for url in candidates {
        if seen.insert(url.clone()) {
            let kind = classify_seed_url(&url);
            out.push(SeedUrl { url, kind });
        }
    }
    out
}

/// ADR-0063 Phase 109g B: 知識ベースの索引（`context.knowledge.index`）のうち `primary-sources` /
/// `一次情報` タグを持つページの `sources` から拾う seed URL。`local_deep_research::must_read_urls`
/// と同じタグ判定（大小文字を無視して `primary-source`/`一次情報` を含む）。DOI / arXiv / PDF は
/// `extract_seed_urls` と同じ扱いで取得ランナーの種に、GitHub / GitLab は「一次情報（実装）」に載る
/// （`classify_seed_url` に委ねる。それ以外の URL は捨てる）。重複は落とし、出現順を保つ（決定的、
/// LLM は使わない）。
pub fn kb_primary_source_seed_urls(
    knowledge: Option<&crate::protocol::KnowledgeContext>,
) -> Vec<SeedUrl> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    let Some(k) = knowledge else {
        return out;
    };
    for item in &k.index {
        let is_primary = item.tags.iter().any(|t| {
            let t = t.to_ascii_lowercase();
            t.contains("primary-source") || t.contains("一次情報")
        });
        if !is_primary {
            continue;
        }
        for url in &item.sources {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                continue;
            }
            if seen.insert(url.clone()) {
                out.push(SeedUrl {
                    url: url.clone(),
                    kind: classify_seed_url(url),
                });
            }
        }
    }
    out
}

/// 依頼文（`objective`）から検索語を決定的に作る（ADR-0035 D1 手順 1。**LLM は使わない**）。
///
/// Phase 36（ADR-0035 D5）以降、これは**受け皿**である: 通常は LLM が立てた検索語を使い、その答えが
/// 壊れていたときだけこの語で検索する（`acquire.query_llm = false` にすると常にこちらだけを使う）。
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
        let meaningful: Vec<&str> = words
            .iter()
            .copied()
            .filter(|w| !is_stopword(&w.to_ascii_lowercase()))
            .collect();
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
    let queries: Vec<String> = ordered
        .into_iter()
        .take(MAX_SEARCH_QUERIES)
        .map(|(_, _, p)| p)
        .collect();

    if queries.is_empty() {
        let fallback = objective.split_whitespace().collect::<Vec<_>>().join(" ");
        if fallback.is_empty() {
            Vec::new()
        } else {
            vec![fallback]
        }
    } else {
        queries
    }
}

/// LiteLLM の `provider/model` 形式から供給者の接頭辞を落とす（ADR-0035 D5）。
/// `chat/completions` を直接叩くときに必要なのは、その口が出しているモデル名
/// （実機: settings の `llm` は `openai/qwen3.8-27b`、`/v1/models` は `qwen3.8-27b`）。
/// 接頭辞と見なすのは小文字・数字・`_` だけの最初の 1 区画（`openai/` / `hosted_vllm/`）。
fn strip_provider_prefix(model: &str) -> String {
    let model = model.trim();
    match model.split_once('/') {
        Some((prefix, rest))
            if !rest.is_empty()
                && !prefix.is_empty()
                && prefix
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') =>
        {
            rest.to_string()
        }
        _ => model.to_string(),
    }
}

/// PaperQA の設定ファイル（`-s <settings>` に渡す値 + `.json`）の `llm`（ADR-0035 D5）。
/// 読めない・書かれていなければ `None`。
async fn settings_llm(settings: &str) -> Option<String> {
    let path = if settings.ends_with(".json") {
        PathBuf::from(settings)
    } else {
        PathBuf::from(format!("{settings}.json"))
    };
    let text = tokio::fs::read_to_string(&path).await.ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let llm = value.get("llm").and_then(serde_json::Value::as_str)?.trim();
    if llm.is_empty() {
        None
    } else {
        Some(llm.to_string())
    }
}

/// 検索語を立てる LLM のモデル名（ADR-0035 D5）。`acquire.query_model` → `[[providers]] model`
/// （`--llm`）→ PaperQA の `settings` の `llm` の順。**PaperQA2 と同じ LLM 先**を使うため。
async fn query_llm_model(config: &PaperQaConfig) -> Option<String> {
    for candidate in [config.acquire.query_model.clone(), config.model.clone()]
        .into_iter()
        .flatten()
    {
        let model = strip_provider_prefix(&candidate);
        if !model.is_empty() {
            return Some(model);
        }
    }
    let settings = config.settings.as_deref()?;
    let llm = settings_llm(settings).await?;
    let model = strip_provider_prefix(&llm);
    if model.is_empty() { None } else { Some(model) }
}

/// `config.env` に入っている値（`OPENAI_BASE_URL` / `OPENAI_API_KEY`）。同名キーは後の行が勝つ
/// （`with_env` の規則と同じ）。
fn env_value(config: &PaperQaConfig, key: &str) -> Option<String> {
    config
        .env
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

/// 検索語を立てる LLM に渡すもの（ADR-0035 D5）。タスクの `title` / `objective` と、
/// あればこの案件についての記憶（`context.memory.project`。`RunContext` に案件の依頼文そのものは
/// 無いので、案件単位で人が与えた文脈として最も近いものを渡す）。
fn query_request_block(req: &RunRequest) -> serde_json::Value {
    let context = req
        .context
        .memory
        .as_ref()
        .map(|m| m.project.trim().to_string())
        .filter(|p| !p.is_empty())
        .map(|p| truncate_chars(&p, QUERY_CONTEXT_MAX_CHARS));
    serde_json::json!({
        "title": req.task.title,
        "objective": req.task.objective,
        "context": context,
    })
}

/// 取得ランナーを動かす python（ADR-0035 D1）。設定が無ければ `command`（既定は python インタプリタ
/// 自身。ADR-0063 Phase 109d C2）と同じディレクトリの `python3`（venv の中を指しているのが普通）、
/// ディレクトリが無ければ `python3`。
fn acquire_python(config: &PaperQaConfig) -> String {
    if let Some(command) = &config.acquire.command {
        return command.clone();
    }
    match Path::new(&config.command).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            parent.join("python3").to_string_lossy().into_owned()
        }
        _ => "python3".to_string(),
    }
}

/// ADR-0063 Phase 109d C2: `[adapters.paperqa] command` を実際に起動するコマンドに解決する。
/// 既定・現行の値は python インタプリタそのもの（`paperqa_ask.py` を `<command> <script> <input.json>`
/// として起動する）。旧 `pqa`（Phase 108 までの既定、または運用者がまだ CLI 実行ファイルを指している
/// 場合）が来たら、同じディレクトリの `python` に自動で置き換えて 1 回警告する。
fn resolve_ask_command(config: &PaperQaConfig, run_id: &str) -> String {
    let path = Path::new(&config.command);
    let is_old_pqa_cli = path.file_name().and_then(|n| n.to_str()) == Some("pqa");
    if !is_old_pqa_cli {
        return config.command.clone();
    }
    let replacement = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            parent.join("python").to_string_lossy().into_owned()
        }
        _ => "python".to_string(),
    };
    warn!(
        "run {run_id}: [adapters.paperqa] command={:?} looks like the old `pqa` CLI (ADR-0063 \
         Phase 109d C2 replaced it with the paperqa Python API); using the sibling python \
         interpreter {replacement:?} instead. Update the config to point at python directly.",
        config.command
    );
    replacement
}

/// ADR-0063 Phase 109e: `config.settings` はこのコードベースでは設定ファイルへの**フルパス**
/// （拡張子無し。`settings_llm` と同じ前提。ただし `.json` 付きでも名前だけでも受け付ける）として
/// 扱われてきた。`paperqa_ask.py` に渡す `settings_path`（**直接読む**絶対パス、`.json` 付き）/
/// `settings_dir`/`settings_name`（`settings_path` が組めない、または見つからないときの
/// `Settings.from_name` フォールバック用）に分解する。
///
/// Phase 109d までは `settings_dir` を `PQA_SETTINGS_DIR` として渡し `Settings.from_name` に
/// 探させていたが、本番の `Settings.from_name`（paperqa==2026.8.12）はその環境変数を見ない
/// （`~/.pqa/settings/` 固定）ため、それでは設定ファイルの実際の置き場所（`settings_dir`）を
/// 全く見つけられなかった（本番で観測。Phase 109e）。`settings_path` はその置き場所を
/// `paperqa_ask.py` に直接教える。
///
/// 3 通りの入力を同じ `settings_path` に正規化する:
/// - 拡張子無しのフルパス（`/a/b/name`）→ `/a/b/name.json`
/// - `.json` 付きのフルパス（`/a/b/name.json`）→ そのまま
/// - ディレクトリの無い名前だけ（`name`）→ `settings_path` は組めない（`None`）。
///   `settings_name` だけが残り、`paperqa_ask.py` 側は `Settings.from_name` に委ねる
///   （paperqa 自身の同梱設定名、例 `"high_quality"`、を指すときの唯一の経路）。
fn split_settings_path(settings: &str) -> (Option<String>, Option<String>, Option<String>) {
    let path = Path::new(settings);
    let name = path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned());
    let settings_path = match (&dir, &name) {
        (Some(dir), Some(name)) => Some(format!("{dir}/{name}.json")),
        _ => None,
    };
    (settings_path, dir, name)
}

/// ADR-0063 Phase 109d C2: 答え（全問い）の `contexts` に現れた `docname`/`dockey` の集合
/// （英数字だけに正規化）。
fn context_identifiers(answers: &[AskAnswer]) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for answer in answers {
        for ctx in &answer.contexts {
            if !ctx.docname.is_empty() {
                set.insert(normalize_alnum(&ctx.docname));
            }
            if !ctx.dockey.is_empty() {
                set.insert(normalize_alnum(&ctx.dockey));
            }
        }
    }
    set
}

/// この候補が `context_ids`（`context_identifiers` の戻り値）のどれかと一致するか（ADR-0063 Phase
/// 109d C2）。`parsing.use_doc_details = false`（ADR-0027 の設定）だと PaperQA2 は corpus の
/// **ファイル名**から `docname` を作るので、`answer_cites` と同じくファイル名の語幹で突き合わせる。
fn candidate_in_contexts(context_ids: &BTreeSet<String>, candidate: &Candidate) -> bool {
    if candidate.file.is_empty() {
        return false;
    }
    let stem = candidate
        .file
        .trim_end_matches(".pdf")
        .trim_end_matches(".txt");
    let normalized_stem = normalize_alnum(stem);
    if normalized_stem.is_empty() {
        return false;
    }
    context_ids
        .iter()
        .any(|id| id.contains(&normalized_stem) || normalized_stem.contains(id.as_str()))
}

/// `artifacts/papers.json` の 1 件（ADR-0035 D1 手順 4。Phase 38 で `candidates.json` から改名）。ランナー（Python）が書き、
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
    /// ADR-0063 D1（Phase 109）: メタデータの abstract（OpenAlex / arXiv / Semantic Scholar）。
    #[serde(default, rename = "abstract")]
    pub abstract_text: String,
    /// ADR-0063 D1: `true` なら corpus に入っているのは本文ではなくこの abstract（`file` はその
    /// テキストファイル）。`pdf_downloaded` と排他（両方 true にはならない）。
    #[serde(default)]
    pub abstract_only: bool,
}

/// `paperqa_ask.py` が `output_path` に書く 1 件の証拠（`PQASession.contexts` の 1 件。
/// ADR-0063 Phase 109d C1/C4）。`ask()` に渡した Python の型ではなく、その `docname`/`dockey`/
/// `citation`/`score` だけを写した JSON 表現。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AskContext {
    #[serde(default)]
    pub docname: String,
    #[serde(default)]
    pub dockey: String,
    #[serde(default)]
    pub citation: String,
    #[serde(default)]
    pub score: Option<f64>,
    /// この証拠を使った問いの `id`（`AskAnswer::id`）。
    #[serde(default)]
    pub question: String,
}

/// `paperqa_ask.py` が `output_path` に書く 1 問分の答え（ADR-0063 Phase 109d C1）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AskAnswer {
    #[serde(default)]
    pub id: String,
    /// 対象ごとの問いなら対象名、総括や（対象が取れないときの）単一の問いなら `None`。
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub question: String,
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub has_successful_answer: bool,
    #[serde(default)]
    pub contexts: Vec<AskContext>,
    #[serde(default)]
    pub references: String,
    /// 再試行しても失敗した場合のメッセージ（成功すれば `None`）。
    #[serde(default)]
    pub error: Option<String>,
}

/// `paperqa_ask.py` の `output_path` 全体（ADR-0063 Phase 109d C1）。`error` が `Some` なら
/// `paperqa` そのものが import できなかった（exit 3。`answers` は空）。
#[derive(Debug, Clone, Default, Deserialize)]
struct AskOutput {
    #[serde(default)]
    answers: Vec<AskAnswer>,
    #[serde(default)]
    target_aspect_table: String,
    /// ADR-0063 Phase 109h: `max_asks` に収めるため後ろから削られた対象（比較先があるときだけ。
    /// 総括の問いは必ず残す。`build_questions_for_targets` が返す）。
    #[serde(default)]
    dropped_targets: Vec<String>,
    /// ADR-0063 Phase 109h: 比較先の設計条件に実際に使われた知識ベースのページの path
    /// （`find_comparison_page_path` の一致がページ本文の読み込みまで成功したときだけ。
    /// 見つからなければ `None`、目的文の周辺 1 文にフォールバックしたときも `None`）。
    #[serde(default)]
    comparison_page: Option<String>,
    /// ADR-0063 Phase 109h: 総括の判断の問いに埋め込んだ設計条件の文字数（比較先が無い、または
    /// 設計条件が全く取れなかったときは 0）。
    #[serde(default)]
    comparison_context_chars: u32,
    #[serde(default)]
    error: Option<String>,
}

/// 取得の段の結果（`CELERIS_ACQUIRE` の中身）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AcquireCounts {
    candidates: u32,
    pdfs: u32,
    /// ADR-0063 D1（Phase 109）: 本文が取れず abstract で corpus に入れた件数。
    abstracts: u32,
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
                // pqa / ランナーのフォーマットは celeris が定義したものではないので寛容に無視する
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
    seed_urls: &[SeedUrl],
) -> Result<(AcquireCounts, Option<Terminal>), AdapterError> {
    // ADR-0035 D5（Phase 36）: 検索語はランナーの中で LLM が立てる。ここで作るのは
    // **LLM の答えが壊れていたときの受け皿**（Phase 34 までの決定的な抽出）。
    let queries = build_search_queries(&req.task.objective);
    let model = if config.acquire.query_llm {
        query_llm_model(config).await
    } else {
        None
    };
    match &model {
        Some(model) => progress::emit_status(
            sink,
            &truncate_chars(
                &format!("acquiring literature; the search terms are written by {model}"),
                PROGRESS_LINE_MAX_CHARS,
            ),
        ),
        None => progress::emit_status(
            sink,
            &truncate_chars(
                &format!(
                    "acquiring literature for {} search term(s) (deterministic): {}",
                    queries.len(),
                    queries.join(" | ")
                ),
                PROGRESS_LINE_MAX_CHARS,
            ),
        ),
    }

    let script_path = run_dir.join("paperqa_acquire.py");
    let input_path = run_dir.join("acquire_input.json");
    // ADR-0063 D4（Phase 109）: `acquire_input.json` はワーカー/レビュアーが読める run ディレクトリに
    // 平文で残る（権限 664）。API キーの実際の値はここには書かず、子プロセスの環境変数（`.envs(config.env)`
    // で既に渡っている）だけで受け渡す。JSON にはプレースホルダだけを書く（`paperqa_acquire.py` の
    // `resolve_env_placeholder` が解決する）。
    let api_key_placeholder = env_value(config, "OPENAI_API_KEY").map(|_| "<env:OPENAI_API_KEY>".to_string());
    let seed_urls_json: Vec<serde_json::Value> = seed_urls
        .iter()
        .filter(|s| matches!(s.kind, SeedUrlKind::Pdf | SeedUrlKind::Doi | SeedUrlKind::Arxiv))
        .map(|s| serde_json::json!({"url": s.url, "kind": s.kind.as_str()}))
        .collect();
    let input = serde_json::json!({
        "queries": queries,
        "query_llm": {
            "enabled": model.is_some(),
            "model": model,
            "base_url": env_value(config, "OPENAI_BASE_URL"),
            "api_key": api_key_placeholder,
            "timeout_secs": config.acquire.query_timeout_secs,
            "max_tokens": QUERY_LLM_MAX_TOKENS,
            "max_queries": MAX_LLM_SEARCH_QUERIES,
        },
        "request": query_request_block(req),
        "paper_directory": paper_directory.to_string_lossy(),
        "papers_path": artifacts_dir.join("papers.json").to_string_lossy(),
        "sources_path": artifacts_dir.join("sources.json").to_string_lossy(),
        "queries_path": artifacts_dir.join("queries.json").to_string_lossy(),
        "openalex_filter": config.acquire.openalex_filter,
        "max_candidates": config.acquire.max_candidates,
        "max_pdfs": config.acquire.max_pdfs,
        "per_query": config.acquire.per_query,
        "timeout_secs": config.acquire.timeout_secs,
        "mailto": config.acquire.mailto,
        "abstract_fallback": config.acquire.abstract_fallback,
        "seed_urls": seed_urls_json,
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
        .current_dir(req.cwd())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let child = match command.spawn() {
        // ADR-0044 §5 Phase 53 追記（Phase 55）: 取得ランナーも同じ run の一族（`pqa ask` の前に
        // 順に走るので、表の `run_id` は重ならない）。
        Ok(child) => {
            let _process_group = crate::process_group::ProcessGroup::register(run_id, child.id());
            child
        }
        Err(e) => {
            // 取得ランナーが起動できないのは設定の誤り（python のパス）だが、ここでは run を止めず
            // 0 件として先に進む（ゲートが「取得が 0 件」として人に返す）。
            warn!(
                "run {run_id}: could not start the literature acquisition runner ({python}): {e}"
            );
            progress::emit_status(
                sink,
                &truncate_chars(
                    &format!("literature acquisition could not start ({python}): {e}"),
                    PROGRESS_LINE_MAX_CHARS,
                ),
            );
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
                progress::emit_status(
                    sink,
                    &truncate_chars(
                        &format!("acquire: {}", rest.trim()),
                        PROGRESS_LINE_MAX_CHARS,
                    ),
                );
            } else if let Some(rest) = line.strip_prefix(ACQUIRE_RESULT_PREFIX) {
                match serde_json::from_str::<serde_json::Value>(rest) {
                    Ok(value) => {
                        let number = |key: &str| -> u32 {
                            value
                                .get(key)
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0) as u32
                        };
                        counts = Some(AcquireCounts {
                            candidates: number("candidates"),
                            pdfs: number("pdfs"),
                            abstracts: number("abstracts"),
                        });
                    }
                    Err(e) => warn!("run {run_id}: could not parse CELERIS_ACQUIRE line: {e}"),
                }
            }
        },
    )
    .await?;

    if let Some(terminal) = streamed.timeout {
        return Ok((counts.unwrap_or_default(), Some(terminal)));
    }
    if !streamed.exit.success() {
        let tail = streamed
            .stderr_tail
            .lines()
            .next_back()
            .unwrap_or("")
            .to_string();
        warn!("run {run_id}: the literature acquisition runner failed: {tail}");
        progress::emit_status(
            sink,
            &truncate_chars(
                &format!("literature acquisition failed: {tail}"),
                PROGRESS_LINE_MAX_CHARS,
            ),
        );
    }
    let counts = counts.unwrap_or_default();
    progress::emit_status(
        sink,
        &format!(
            "acquire: {} candidate(s), {} PDF(s), {} abstract(s) in the corpus",
            counts.candidates, counts.pdfs, counts.abstracts
        ),
    );
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
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
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
        Some(pos) if id[pos + 1..].chars().all(|c| c.is_ascii_digit()) && pos + 1 < id.len() => {
            id[..pos].to_string()
        }
        _ => id.to_string(),
    }
}

fn first_author_surname(authors: &[String]) -> String {
    let Some(first) = authors.first() else {
        return String::new();
    };
    first
        .split_whitespace()
        .next_back()
        .map(|s| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .unwrap_or_default()
}

/// `artifacts/answer.md` の末尾に足す `## 出典`（ADR-0035 D4）。引用されたものを先に、
/// `[n] 著者 (年). タイトル. venue. URL` の形で並べる（欠けている項目は飛ばす）。
fn render_sources_section(candidates: &[(Candidate, bool)]) -> String {
    let mut ordered: Vec<&(Candidate, bool)> =
        candidates.iter().filter(|(_, cited)| *cited).collect();
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
        let url = if !candidate.url.is_empty() {
            &candidate.url
        } else {
            &candidate.pdf_url
        };
        if !url.is_empty() {
            parts.push(url.clone());
        }
        let marker = match (*cited, candidate.abstract_only) {
            (true, true) => " (引用・アブストのみ)",
            (true, false) => " (引用)",
            (false, _) => "",
        };
        out.push_str(&format!("[{}] {}{}\n", index + 1, parts.join(". "), marker));
    }
    out
}

/// ADR-0063 Phase 109d C4: `answer.md`/`report.md` の先頭に置く「# 対象別の整理」
/// （対象×観点の表 + 対象ごとの節 + 総括）。対象が 1 件も取れていなければ空文字列
/// （呼び出し側はフォールバックの単一の答えをそのまま先頭に置く）。
///
/// ADR-0063 Phase 109g A: 総括の節の見出しは、比較先（`comparison_target`）が分かっていれば
/// 「## <比較先> との比較分類」、無ければ従来どおり「## 総括」。
fn render_target_sections(
    answers: &[AskAnswer],
    table: &str,
    comparison_target: Option<&str>,
) -> String {
    if !answers.iter().any(|a| a.target.is_some()) {
        return String::new();
    }
    let mut out = String::from("# 対象別の整理\n\n");
    if !table.trim().is_empty() {
        out.push_str(table.trim_end());
        out.push_str("\n\n");
    }
    for answer in answers.iter().filter(|a| a.target.is_some()) {
        out.push_str(&format!(
            "### {}\n\n",
            answer.target.as_deref().unwrap_or_default()
        ));
        if answer.answer.trim().is_empty() {
            out.push_str("(回答なし");
            if let Some(err) = &answer.error {
                out.push_str(&format!(": {err}"));
            }
            out.push_str(")\n\n");
        } else {
            out.push_str(answer.answer.trim());
            out.push_str("\n\n");
        }
    }
    if let Some(summary) = answers.iter().find(|a| a.id == "summary") {
        match comparison_target {
            Some(target) if !target.trim().is_empty() => {
                out.push_str(&format!("## {target} との比較分類\n\n"));
            }
            _ => out.push_str("## 総括\n\n"),
        }
        if summary.answer.trim().is_empty() {
            out.push_str("(回答なし");
            if let Some(err) = &summary.error {
                out.push_str(&format!(": {err}"));
            }
            out.push_str(")\n\n");
        } else {
            out.push_str(summary.answer.trim());
            out.push_str("\n\n");
        }
    }
    out
}

/// ADR-0063 Phase 109d C4: 「## 引用された文献（contexts）」節。`docname`（無ければ `dockey`）で
/// 重複排除し、その文献がどの問い（`AskAnswer::id`）で使われたかを列挙する。
fn render_contexts_section(answers: &[AskAnswer]) -> String {
    let mut by_key: BTreeMap<String, (String, String, Vec<String>)> = BTreeMap::new();
    for answer in answers {
        for ctx in &answer.contexts {
            let key = if !ctx.docname.is_empty() {
                ctx.docname.clone()
            } else {
                ctx.dockey.clone()
            };
            if key.is_empty() {
                continue;
            }
            let entry = by_key
                .entry(key)
                .or_insert_with(|| (ctx.docname.clone(), ctx.citation.clone(), Vec::new()));
            if !entry.2.contains(&answer.id) {
                entry.2.push(answer.id.clone());
            }
        }
    }
    if by_key.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\n## 引用された文献（contexts）\n\n");
    for (key, (docname, citation, ids)) in &by_key {
        let name = if !docname.is_empty() { docname } else { key };
        let label = if !citation.trim().is_empty() {
            citation.trim()
        } else {
            name.as_str()
        };
        out.push_str(&format!("- {name}: {label}（{}）\n", ids.join(", ")));
    }
    out
}

/// `answer.md` に足す「一次情報（実装）」節（ADR-0063 D1）。目的文中の GitHub / GitLab の URL は
/// PaperQA の corpus には入れず（論文ではないため）、参照節に載せるだけ。
fn render_primary_sources_section(urls: &[String]) -> String {
    if urls.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\n## 一次情報（実装）\n\n");
    for url in urls {
        out.push_str(&format!("- {url}\n"));
    }
    out
}

/// ADR-0063 D1: `research.json` の `evidence`（`insufficient_is_error` の値に関わらず、acquire が
/// 動いた run では必ず書く）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct EvidenceSummary {
    cited: u32,
    cited_fulltext: u32,
    cited_abstract_only: u32,
    /// ADR-0063 Phase 109d C2: `cited` のうち、`ask()` が実際に使った証拠（`PQASession.contexts` の
    /// `docname`/`dockey`）と突き合わせられた件数（本文一致だけで数えたものは含まない）。
    cited_from_contexts: u32,
    min_cited: u32,
    insufficient: bool,
}

/// `evidence_gate` の結果。`error` が `Some` なら run は `Terminal::Error{retryable: true}`
/// （検索が 0 件、または `insufficient_is_error = true` で閾値未達）。`summary` は常に埋まる
/// （`research.json` に書く。ADR-0063 D1）。
struct EvidenceGateResult {
    error: Option<String>,
    summary: EvidenceSummary,
}

/// `artifacts/research.json` を書く（ADR-0063 D1。取得の段が動いた run では常に書く）。
/// ADR-0063 Phase 109c A: 目的文から取れた対象・観点も残す（監査・reviewer の参考用）。
/// ADR-0063 Phase 109h: `dropped_targets`/`comparison_page`/`comparison_context_chars`
/// （`ask_output.json` からそのまま写す。観測性）も残す。
#[allow(clippy::too_many_arguments)]
async fn write_research_json(
    path: &Path,
    summary: &EvidenceSummary,
    targets: &[String],
    aspects: &[String],
    dropped_targets: &[String],
    comparison_page: Option<&str>,
    comparison_context_chars: u32,
    run_id: &str,
) {
    let value = serde_json::json!({
        "evidence": summary,
        "targets": targets,
        "aspects": aspects,
        "dropped_targets": dropped_targets,
        "comparison_page": comparison_page,
        "comparison_context_chars": comparison_context_chars,
    });
    match serde_json::to_string_pretty(&value) {
        Ok(text) => {
            if let Err(e) = tokio::fs::write(path, format!("{text}\n")).await {
                warn!("run {run_id}: could not write artifacts/research.json: {e}");
            }
        }
        Err(e) => warn!("run {run_id}: could not serialize artifacts/research.json: {e}"),
    }
}

/// `answer.md` に足す「証拠の質」節（ADR-0063 D1: 本文 / アブストのみの内訳を必ず書く）。
/// ADR-0063 Phase 109h: `dropped_targets` が非空なら「max_asks の制限で問えなかった」対象を書く
/// （総括〈比較分類〉の問いを必ず残すため後ろから詰められた対象。観測性）。
fn render_evidence_section(summary: &EvidenceSummary, dropped_targets: &[String]) -> String {
    let mut out = String::from("\n\n## 証拠の質\n\n");
    out.push_str(&format!(
        "- 引用された出典: {} 件（本文からの引用: {} 件、アブストラクトのみ: {} 件、\
         うち PaperQA の証拠〈contexts〉との突き合わせ: {} 件）\n",
        summary.cited, summary.cited_fulltext, summary.cited_abstract_only, summary.cited_from_contexts
    ));
    if summary.insufficient {
        out.push_str(&format!(
            "- 証拠不足（cited={} < min {}）。代替案: Web 調査（`web-research` / Local Deep Research）に \
             切り替えるか、人が著者版 PDF の URL を与えてください。\n",
            summary.cited, summary.min_cited
        ));
    }
    if !dropped_targets.is_empty() {
        out.push_str(&format!(
            "- 対象 {} 件は max_asks の制限で問えなかった: {}\n",
            dropped_targets.len(),
            dropped_targets.join("、")
        ));
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
    // ADR-0036 D1/D2: 成果物の置き場はディスパッチャが決めた `artifacts_dir`（共有 workspace ではタスクごと）。
    let artifacts_dir = req.artifacts_dir.clone();
    let artifacts_rel = req.artifacts_rel();
    // ADR-0035 D5: `queries.json`（どの検索語で探したか）も前回の残りを消す。
    // ADR-0063 D1（Phase 109）: `research.json`（証拠の内訳）も同様。
    for stale in [
        "result.json",
        "answer.md",
        "papers.json",
        "sources.json",
        "queries.json",
        "research.json",
    ] {
        let _ = tokio::fs::remove_file(artifacts_dir.join(stale)).await;
    }

    let question = build_question(&req.task, &req.context, &artifacts_rel);
    // ADR-0023 D2 / M1: この run で何を渡したかを残す。
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &question, run_id).await;

    // ADR-0063 Phase 109c A: `research.json` に残す（`build_question` が既に同じ関数で問いに反映済み）。
    let research_targets = crate::research_targets::research_targets(&req.task.objective);
    let research_aspects = crate::research_targets::research_aspects(&req.task.objective);

    // ADR-0063 D1: 起点の資料（目的文 + `inputs` の URL）。PDF / DOI / arXiv は取得ランナーへの seed に、
    // GitHub / GitLab は「一次情報（実装）」として答えの参照節に載せる（corpus には入れない）。
    // ADR-0063 Phase 109g B: 知識ベースの `primary-sources` タグのページの `sources` にある URL も
    // 同じ種に足す（既知の論文を種にする。目的文由来の URL が優先、重複は落とす）。
    let mut seed_urls = extract_seed_urls(&req.task.objective, &req.task.inputs);
    let mut seen_seed_urls: std::collections::BTreeSet<String> =
        seed_urls.iter().map(|s| s.url.clone()).collect();
    for kb_seed in kb_primary_source_seed_urls(req.context.knowledge.as_ref()) {
        if seen_seed_urls.insert(kb_seed.url.clone()) {
            seed_urls.push(kb_seed);
        }
    }
    let fetchable_seeds: Vec<SeedUrl> = seed_urls
        .iter()
        .filter(|s| matches!(s.kind, SeedUrlKind::Pdf | SeedUrlKind::Doi | SeedUrlKind::Arxiv))
        .cloned()
        .collect();
    let github_urls: Vec<String> = seed_urls
        .iter()
        .filter(|s| s.kind == SeedUrlKind::Github)
        .map(|s| s.url.clone())
        .collect();

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
            &fetchable_seeds,
        )
        .await?;
        if let Some(terminal) = timeout {
            // タイムアウトは供給側失敗として分類しない（他アダプタと同じ。ADR-0010 D5）。
            write_result_json(&run_dir, &terminal, None).await?;
            return Ok(RunOutcome {
                terminal,
                exit_code: None,
            });
        }
        counts
    } else {
        AcquireCounts::default()
    };

    // ---------------------------------------------------------------- 2 段目: 索引と回答
    // ADR-0063 Phase 109d C1/C2: `pqa ask` CLI ではなく、埋め込みの python ランナー
    // （`paperqa_ask.py`）が PaperQA の Python API（`paperqa.ask`/`Settings`）を呼ぶ。
    let stdout_log_path = run_dir.join("stdout.log");
    let stderr_log_path = run_dir.join("stderr.log");

    let ask_script_path = run_dir.join("paperqa_ask.py");
    let ask_input_path = run_dir.join("ask_input.json");
    let ask_output_path = run_dir.join("ask_output.json");
    let (settings_path, settings_dir, settings_name) = config
        .settings
        .as_deref()
        .map(split_settings_path)
        .unwrap_or((None, None, None));
    // ADR-0063 Phase 109d C3: 対象が複数（比較先込み）でも `max_asks` を超えない。
    let comparison_target = crate::research_targets::comparison_target(&req.task.objective);
    // ADR-0063 Phase 109g A: 総括の問い（比較分類の判断）の材料。マッチングと本文の切り詰めは
    // `paperqa_ask.py` の純関数に任せ、ここでは材料（KB の root と索引、目的文の周辺 1 文）を渡すだけ
    // （知識ベースの本文はハーネス〈task-worker〉自身では読まない。ADR-0047 D2「索引だけ」の境界を守る）。
    let comparison_fallback_paragraph =
        crate::research_targets::comparison_target_paragraph(&req.task.objective);
    let knowledge_index_for_comparison: Vec<serde_json::Value> = req
        .context
        .knowledge
        .as_ref()
        .map(|k| {
            k.index
                .iter()
                .map(|item| {
                    serde_json::json!({
                        "path": item.path,
                        "title": item.title,
                        "tags": item.tags,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let ask_input = serde_json::json!({
        "settings_name": settings_name,
        "settings_dir": settings_dir,
        // ADR-0063 Phase 109e: `paperqa_ask.py` がこれを直接読む（`Settings.from_name` は
        // `PQA_SETTINGS_DIR` を見ないので使えない。あればフォールバックとしてのみ使う）。
        "settings_path": settings_path,
        "paper_directory": paper_directory.to_string_lossy(),
        "index_directory": index_directory.to_string_lossy(),
        "index_name": index_name,
        "model": config.model,
        "targets": research_targets,
        "aspects": research_aspects,
        "comparison_target": comparison_target,
        // ADR-0063 Phase 109g A: `paperqa_ask.py::find_comparison_page_path` /
        // `comparison_design_context` が読む材料。
        "comparison_context": {
            "knowledge_root": config.knowledge_root.as_ref().map(|p| p.to_string_lossy().into_owned()),
            "knowledge_index": knowledge_index_for_comparison,
            "fallback_paragraph": comparison_fallback_paragraph,
        },
        "max_asks": config.max_asks,
        "fallback_question": question,
        "output_path": ask_output_path.to_string_lossy(),
    });
    tokio::fs::write(&ask_script_path, ASK_SCRIPT).await?;
    let ask_input_text = serde_json::to_string_pretty(&ask_input)?;
    tokio::fs::write(&ask_input_path, format!("{ask_input_text}\n")).await?;

    let ask_command = resolve_ask_command(config, run_id);
    let mut command = Command::new(&ask_command);
    command
        .arg(&ask_script_path)
        .arg(&ask_input_path)
        .envs(config.env.iter().cloned())
        .current_dir(req.cwd())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let child = command.spawn().map_err(AdapterError::Spawn)?;
    // ADR-0044 §5 Phase 53 追記（Phase 55）: この run のプロセスグループを覚える（`kill_tree` の入口）。
    let _process_group = crate::process_group::ProcessGroup::register(run_id, child.id());

    let streamed = stream_child(
        child,
        stdout_log_path,
        stderr_log_path,
        start,
        limits,
        run_id,
        sink,
        |line| {
            // `paperqa_ask.py` は `progress: <text>` を出す（ADR-0035 D5 の acquire ランナーと同じ流儀）。
            progress::emit_status(sink, &truncate_chars(line, PROGRESS_LINE_MAX_CHARS));
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
    let classify_text = format!("{stdout_buf}\n{stderr_tail}");
    let ask_output: Option<AskOutput> = match tokio::fs::read_to_string(&ask_output_path).await {
        Ok(text) => serde_json::from_str::<AskOutput>(&text).ok(),
        Err(_) => None,
    };
    // 成功と見なせる出力（`error` なし・答えが 1 件でも非空）だけを次に渡す。
    let usable_ask_output = ask_output.clone().filter(|o| {
        o.error.is_none() && o.answers.iter().any(|a| !a.answer.trim().is_empty())
    });

    let (terminal, provider_failure) = if exit_status.code() == Some(ASK_EXIT_PAPERQA_NOT_IMPORTABLE)
    {
        // ADR-0063 Phase 109d C1: `paperqa` が import できない (テスト環境や未整備の venv)。再試行しても
        // 直らないので `retryable: false` の分かりやすいエラーにする（一般の非 0 終了とは区別する）。
        (
            Terminal::Error {
                message: "paperqa (the Python package) is not importable in the environment \
                          used by [adapters.paperqa] command; install `paperqa` there \
                          (ADR-0063 Phase 109d C1)"
                    .to_string(),
                retryable: false,
            },
            None,
        )
    } else if exit_status.code() == Some(ASK_EXIT_BAD_USAGE_OR_SETTINGS_NOT_FOUND) {
        // ADR-0063 Phase 109e: 引数の誤り、または `settings_path`/`Settings.from_name` のどちらでも
        // 設定ファイルが見つからなかった（`paperqa_ask.py::load_settings`）。どちらも設定の誤りで
        // 再試行しても直らないので `retryable: false`。`output_path` の `error`（探したパスを列挙した
        // メッセージ）を優先し、無ければ stderr の最後の行を使う。
        let message = ask_output
            .as_ref()
            .and_then(|o| o.error.clone())
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| {
                stderr_tail
                    .lines()
                    .next_back()
                    .unwrap_or("paperqa_ask.py: bad usage or settings not found")
                    .to_string()
            });
        (
            Terminal::Error {
                message: format!("paperqa_ask.py could not start (exit=2): {message}"),
                retryable: false,
            },
            None,
        )
    } else if !exit_status.success() {
        let exit_repr = match exit_status.code() {
            Some(code) => code.to_string(),
            None => "signal".to_string(),
        };
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: format!("paperqa_ask.py exited with a non-zero status (exit={exit_repr})"),
                retryable: true,
            },
            pf,
        )
    } else if let Some(ask_output) = usable_ask_output {
        // ADR-0027 D3 手順 4: アダプタが `artifacts/answer.md` と `artifacts/result.json` を書く。
        if let Err(e) = tokio::fs::create_dir_all(&artifacts_dir).await {
            warn!("run {run_id}: could not create artifacts/ directory: {e}");
        }

        // ADR-0063 Phase 109d C2: `cited` は全 `ask()` 呼び出しの `contexts`（実際に使った証拠）の
        // `docname`/`dockey` の和集合が主。本文一致（`answer_cites`、答え全体を連結したもの）は補助として
        // OR する（Phase 109b A2 までの仕組みの名残 -- モデルが引用マーカーを本文に残さない場合の保険）。
        let context_ids = context_identifiers(&ask_output.answers);
        let combined_answer_text: String = ask_output
            .answers
            .iter()
            .map(|a| a.answer.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let candidates = read_candidates(&artifacts_dir.join("papers.json")).await;
        let mut cited_from_contexts: u32 = 0;
        let marked: Vec<(Candidate, bool)> = candidates
            .into_iter()
            .map(|c| {
                let via_contexts = candidate_in_contexts(&context_ids, &c);
                if via_contexts {
                    cited_from_contexts += 1;
                }
                let cited = via_contexts || answer_cites(&combined_answer_text, &c);
                (c, cited)
            })
            .collect();
        // ADR-0063 D1: 本文からの引用とアブストのみの引用を区別する（`research.json` / `answer.md` の
        // 「証拠の質」節に内訳を書くため）。
        let cited_fulltext = marked
            .iter()
            .filter(|(c, cited)| *cited && c.pdf_downloaded)
            .count() as u32;
        let cited_abstract_only = marked
            .iter()
            .filter(|(c, cited)| *cited && c.abstract_only)
            .count() as u32;
        if acquiring {
            write_sources_json(&artifacts_dir.join("sources.json"), &marked, run_id).await;
        }

        // ADR-0063 D1: 決定的な証拠ゲート（LLM には判断させない）。取得の段を行わない構成では見ない。
        let evidence = if acquiring {
            Some(evidence_gate(
                &config.evidence,
                acquired,
                cited_fulltext,
                cited_abstract_only,
                cited_from_contexts,
            ))
        } else {
            None
        };
        if let Some(ev) = &evidence {
            write_research_json(
                &artifacts_dir.join("research.json"),
                &ev.summary,
                &research_targets,
                &research_aspects,
                &ask_output.dropped_targets,
                ask_output.comparison_page.as_deref(),
                ask_output.comparison_context_chars,
                run_id,
            )
            .await;
        }

        // ADR-0063 Phase 109d C4: 対象が取れていれば「# 対象別の整理」（表 + 対象ごとの節 + 総括）、
        // 取れていなければフォールバックの単一の答えをそのまま使う。
        let target_section = render_target_sections(
            &ask_output.answers,
            &ask_output.target_aspect_table,
            comparison_target.as_deref(),
        );
        let body_text = if target_section.is_empty() {
            ask_output
                .answers
                .first()
                .map(|a| a.answer.clone())
                .unwrap_or_default()
        } else {
            target_section
        };
        let contexts_section = render_contexts_section(&ask_output.answers);

        let answer_md = if acquiring {
            let mut body = format!(
                "{body_text}{contexts_section}{}",
                render_sources_section(&marked)
            );
            if let Some(ev) = &evidence {
                body.push_str(&render_evidence_section(&ev.summary, &ask_output.dropped_targets));
            }
            body.push_str(&render_primary_sources_section(&github_urls));
            body
        } else {
            format!("{body_text}{contexts_section}")
        };
        if let Err(e) = tokio::fs::write(artifacts_dir.join("answer.md"), &answer_md).await {
            warn!("run {run_id}: could not write artifacts/answer.md: {e}");
        }
        // ADR-0063 Phase 109b A3: 調査系タスクの成果物名を LDR（`report.md`）と揃える。CoS が
        // 受け入れ条件を `artifact_exists report.md` で書いても通るよう、`answer.md` と同じ内容を
        // `report.md` にも書く（両方残す。`answer.md` が PaperQA 独自の詳しい名前として引き続き主）。
        if let Err(e) = tokio::fs::write(artifacts_dir.join("report.md"), &answer_md).await {
            warn!("run {run_id}: could not write artifacts/report.md: {e}");
        }
        // 書いたものは celeris にも知らせる（run の成果物一覧と `Check::ArtifactExists` の解決に使われる）。
        // 他のアダプタではワーカー自身が `artifact` メッセージで申告するが、pqa は申告しないのでアダプタが行う。
        // ADR-0035 D3 / ADR-0063 D1: ゲートに落ちても成果物は残す（人が読めるように）ので、申告はゲートより前に行う。
        // ADR-0036 D4: 申告する `path` は workspace 相対のまま（`artifacts_dir` 基準で組む）。
        let mut to_register: Vec<(&str, String, &str)> = vec![
            (
                "answer.md",
                format!("{artifacts_rel}/answer.md"),
                "markdown",
            ),
            (
                "report.md",
                format!("{artifacts_rel}/report.md"),
                "markdown",
            ),
        ];
        if acquiring {
            to_register.push((
                "papers.json",
                format!("{artifacts_rel}/papers.json"),
                "json",
            ));
            to_register.push((
                "sources.json",
                format!("{artifacts_rel}/sources.json"),
                "json",
            ));
            // ADR-0035 D5: どの検索語で探したか（LLM の出力そのまま、落ちたなら落ちた理由も）。
            to_register.push((
                "queries.json",
                format!("{artifacts_rel}/queries.json"),
                "json",
            ));
            // ADR-0063 D1: 証拠の質の内訳。
            to_register.push((
                "research.json",
                format!("{artifacts_rel}/research.json"),
                "json",
            ));
        }
        for (name, rel_path, kind) in to_register {
            if !req.workspace.join(&rel_path).is_file() {
                continue;
            }
            match crate::artifact::resolve(&req.workspace, name, &rel_path, Some(kind)) {
                Ok(artifact) => sink.artifact(&artifact),
                Err(e) => warn!("run {run_id}: could not register {rel_path}: {e}"),
            }
        }

        let gate_message = evidence.as_ref().and_then(|ev| ev.error.clone());

        if let Some(message) = gate_message {
            // ADR-0031 D2 と同じ: retryable な `Terminal::Error`。供給側の失敗（`AdapterError`）にはしない。
            (
                Terminal::Error {
                    message,
                    retryable: true,
                },
                None,
            )
        } else {
            // 総括の問い（対象が取れているとき）があればそれを、無ければ最初の答えを要約の元にする。
            let summary_source = ask_output
                .answers
                .iter()
                .find(|a| a.id == "summary")
                .or_else(|| ask_output.answers.first())
                .map(|a| a.answer.as_str())
                .unwrap_or("");
            let summary = single_line_summary(summary_source, SUMMARY_MAX_CHARS);
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
    } else {
        let pf = classify_provider_failure(&classify_text);
        (
            Terminal::Error {
                message: "paperqa_ask.py produced no answer".to_string(),
                retryable: true,
            },
            pf,
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

/// ADR-0035 D3 / ADR-0063 D1: 証拠の内訳を集計し、閾値未達をどう扱うか決める。
///
/// - 取得が 0 件（`acquired.candidates == 0`）は「検索経路の問題」を示す固有のメッセージで**常に**
///   hard error（`insufficient_is_error` に関わらず）。
/// - それ以外の閾値未達（`min_candidates` / `min_pdfs` / `min_cited`）は、`insufficient_is_error`
///   が `true` のときだけ hard error。既定（`false`）では `summary.insufficient = true` のまま
///   `error = None` を返し、呼び出し側は `Terminal::Done` にして reviewer / 受け入れ条件に委ねる
///   （`research.json` の `evidence` と `answer.md` の「証拠の質」節で内訳を残す）。
fn evidence_gate(
    thresholds: &PaperQaEvidence,
    acquired: AcquireCounts,
    cited_fulltext: u32,
    cited_abstract_only: u32,
    cited_from_contexts: u32,
) -> EvidenceGateResult {
    let cited = cited_fulltext + cited_abstract_only;
    let enabled =
        thresholds.min_candidates > 0 || thresholds.min_pdfs > 0 || thresholds.min_cited > 0;
    if !enabled {
        return EvidenceGateResult {
            error: None,
            summary: EvidenceSummary {
                cited,
                cited_fulltext,
                cited_abstract_only,
                cited_from_contexts,
                min_cited: thresholds.min_cited,
                insufficient: false,
            },
        };
    }
    if acquired.candidates == 0 {
        // 「論文が見つからなかった」と「検索経路が壊れている」を運用者が区別できるようにする
        // （ADR-0031 D2 と同じ理由）。
        return EvidenceGateResult {
            error: Some(
                "literature search returned nothing (possible network or API problem)"
                    .to_string(),
            ),
            summary: EvidenceSummary {
                cited,
                cited_fulltext,
                cited_abstract_only,
                cited_from_contexts,
                min_cited: thresholds.min_cited,
                insufficient: true,
            },
        };
    }
    let mut problems = Vec::new();
    if thresholds.min_candidates > 0 && acquired.candidates < thresholds.min_candidates {
        problems.push(format!(
            "candidates={} (min {})",
            acquired.candidates, thresholds.min_candidates
        ));
    }
    if thresholds.min_pdfs > 0 && acquired.pdfs < thresholds.min_pdfs {
        problems.push(format!(
            "pdfs={} (min {})",
            acquired.pdfs, thresholds.min_pdfs
        ));
    }
    if thresholds.min_cited > 0 && cited < thresholds.min_cited {
        problems.push(format!("cited={cited} (min {})", thresholds.min_cited));
    }
    let insufficient = !problems.is_empty();
    let error = if insufficient && thresholds.insufficient_is_error {
        Some(format!(
            "insufficient literature evidence: {}",
            problems.join(", ")
        ))
    } else {
        None
    };
    EvidenceGateResult {
        error,
        summary: EvidenceSummary {
            cited,
            cited_fulltext,
            cited_abstract_only,
            cited_from_contexts,
            min_cited: thresholds.min_cited,
            insufficient,
        },
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
            let url = if !candidate.url.is_empty() {
                &candidate.url
            } else {
                &candidate.pdf_url
            };
            BTreeMap::from([
                ("url", serde_json::Value::String(url.clone())),
                ("title", serde_json::Value::String(candidate.title.clone())),
                (
                    "engine",
                    serde_json::Value::String(candidate.source_engine.clone()),
                ),
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

    /// 取得ランナーのスタブ本体。`papers.json` / `sources.json`（`cited` は全部 false）を書き、
    /// progress と `CELERIS_ACQUIRE` を出す（実物のランナーの動きを最小限まねる）。
    fn acquire_stub_script(candidates: u32, pdfs: u32) -> String {
        format!(
            r#"input="$2"
cand=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['papers_path'])" "$input")
src=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['sources_path'])" "$input")
qry=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['queries_path'])" "$input")
papers=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['paper_directory'])" "$input")
mkdir -p "$(dirname "$cand")" "$papers"
echo 'progress: arxiv: 2 result(s)'
printf '{{"generated_by": "fallback", "queries": [], "exclude_terms": []}}\n' > "$qry"
cat > "$cand" <<'JSON'
{STUB_CANDIDATES}
JSON
python3 - "$cand" "$src" <<'PY'
import json, sys
cands = json.load(open(sys.argv[1]))
out = [{{"url": c["url"], "title": c["title"], "engine": c["source_engine"], "cited": False}} for c in cands]
json.dump(out, open(sys.argv[2], "w"), indent=2)
PY
echo 'CELERIS_ACQUIRE {{"candidates": {candidates}, "pdfs": {pdfs}, "engines": {{"arxiv": 1, "openalex": 2}}}}'
"#
        )
    }

    /// 上の候補のうち 2 件を引用する答え（PaperQA2 のファイル名由来の引用の形）。
    const STUB_ANSWER: &str = "Ad hoc file systems aggregate node-local NVMe \
(brinkmann2020_10-1007-s11390-020-9801-1.pdf), and asynchronous IO runtimes \
reduce server overhead (Roe 2021).";

    /// ADR-0063 Phase 109d C1/C2: `paperqa_ask.py`（実物、`config.command` に渡すのは `$1` にその
    /// パスを受け取るスタブ）の代わりに使うスタブ本体。**実物の `paperqa_ask.py` をそのまま
    /// `python3` で動かし**、`paperqa` パッケージだけ `sys.modules` に偽物を差し込む（LDR の
    /// `sys.modules["local_deep_research"]` 注入と同じ考え方）。これにより `build_questions_for_targets`
    /// / `ask_with_retries` / `flatten_contexts` / `build_target_aspect_table` など、このアダプタが
    /// 書き出す**本物の**問いの組み立て・表の組み立てをテストが経由する（シェルで作った別物の答えを
    /// 返すのではない）。`answer_text` は全ての問いに同じ答えとして、`contexts_json` は全ての問いに
    /// 同じ `contexts` として返す（`[]` なら空）。
    fn ask_stub_script(answer_text: &str, contexts_json: &str) -> String {
        let answer_literal = serde_json::to_string(answer_text).expect("valid json string");
        let contexts_literal = if contexts_json.trim().is_empty() {
            "[]".to_string()
        } else {
            contexts_json.to_string()
        };
        format!(
            r#"python3 - "$1" "$2" <<'PY'
import importlib.util, sys, types

script_path, input_path = sys.argv[1], sys.argv[2]


class FakeIndex:
    def __init__(self):
        self.paper_directory = None
        self.index_directory = None
        self.name = None


class FakeAgent:
    def __init__(self):
        self.index = FakeIndex()


class FakeSettings:
    def __init__(self):
        self.agent = FakeAgent()
        self.llm = None

    @classmethod
    def from_name(cls, name):
        return cls()


ANSWER_TEXT = {answer_literal}
CONTEXTS = {contexts_literal}


class FakeSession:
    def __init__(self):
        self.formatted_answer = ANSWER_TEXT
        self.answer = ANSWER_TEXT
        self.has_successful_answer = True
        self.contexts = CONTEXTS
        self.references = ""
        self.cost = None
        self.token_counts = None


class FakeResponse:
    def __init__(self):
        self.session = FakeSession()


def fake_ask(question_text, settings=None):
    return FakeResponse()


fake_pkg = types.ModuleType("paperqa")
fake_pkg.Settings = FakeSettings
fake_pkg.ask = fake_ask
sys.modules["paperqa"] = fake_pkg

spec = importlib.util.spec_from_file_location("paperqa_ask_stub", script_path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

sys.argv = [script_path, input_path]
sys.exit(mod.main())
PY
"#
        )
    }

    /// `runs/<run_id>/ask_input.json`（ADR-0063 Phase 109d C1）を読む。
    fn read_ask_input(dir: &Path, run_id: &str) -> serde_json::Value {
        let text =
            std::fs::read_to_string(dir.join("runs").join(run_id).join("ask_input.json")).unwrap();
        serde_json::from_str(&text).unwrap()
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

    #[tokio::test]
    async fn happy_path_progress_answer_and_result_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(
            dir.path(),
            &ask_stub_script("PaperQA2 finds no evidence of prior work on X [Doe2020, Roe2021].", ""),
        );
        let adapter = PaperQaAdapter::new(config);
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
                assert!(summary.contains("PaperQA2 finds no evidence"), "{summary}");
                assert!(evidence.is_empty());
                assert!(usage.is_none());
            }
            other => panic!("expected done, got {other:?}"),
        }
        let progress = sink.progress.lock().unwrap();
        // 対象が取れない目的文なので単一のフォールバック問い（`id = "q1"`）になる
        // （`paperqa_ask.py::build_questions_for_targets`）。
        assert!(progress.iter().any(|m| m.contains("asking:")), "{progress:?}");
        assert!(*sink.heartbeat_count.lock().unwrap() >= 1);
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

        // 成果物として申告される（run の一覧と Check::ArtifactExists の解決に使われる）。
        // ADR-0063 Phase 109b A3: `report.md` も `answer.md` と同じ内容で申告される。
        let artifacts = sink.artifacts.lock().unwrap();
        assert_eq!(artifacts.len(), 2, "{artifacts:?}");
        let names: Vec<&str> = artifacts.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"answer.md"), "{names:?}");
        assert!(names.contains(&"report.md"), "{names:?}");
        for a in artifacts.iter() {
            assert!(!a.sha256.is_empty(), "{a:?}");
        }
        drop(artifacts);

        let answer_md = std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap();
        assert!(
            answer_md.starts_with("PaperQA2 finds no evidence"),
            "{answer_md}"
        );
        assert!(
            !answer_md.contains("asking:"),
            "進捗のログは含めない: {answer_md}"
        );
        let report_md = std::fs::read_to_string(dir.path().join("artifacts/report.md")).unwrap();
        assert_eq!(report_md, answer_md, "report.md は answer.md と同じ内容");

        let result_json =
            std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result_json).unwrap();
        assert!(
            parsed["summary"]
                .as_str()
                .unwrap()
                .contains("PaperQA2 finds no evidence")
        );
        assert_eq!(parsed["evidence"], serde_json::json!([]));

        assert!(dir.path().join("runs/run-1/stdout.log").is_file());
        assert!(dir.path().join("runs/run-1/stderr.log").is_file());
        let run_result =
            std::fs::read_to_string(dir.path().join("runs/run-1/result.json")).unwrap();
        match serde_json::from_str::<crate::protocol::WorkerMessage>(run_result.trim()).unwrap() {
            crate::protocol::WorkerMessage::Done { summary, .. } => {
                assert!(summary.contains("PaperQA2 finds no evidence"))
            }
            other => panic!("expected done in runs/<run_id>/result.json, got {other:?}"),
        }
    }

    /// ADR-0036 D1/D2/D4: 共有 workspace のタスクは `.taskd/artifacts/<task_id>/` に答えと結果ファイルを
    /// 置き、申告する `path` は workspace 相対のその形になる（兄弟の `artifacts/` を上書きしない）。
    #[tokio::test]
    async fn a_shared_workspace_task_writes_under_its_own_artifacts_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(dir.path(), &ask_stub_script("no prior work on X [Doe2020].", ""));
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(dir.path().join("artifacts/answer.md"), "sibling").unwrap();
        let adapter = PaperQaAdapter::new(config);
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
                ".taskd/artifacts/T1/answer.md",
                ".taskd/artifacts/T1/report.md",
            ]
        );
        drop(artifacts);
        assert!(dir.path().join(".taskd/artifacts/T1/answer.md").is_file());
        assert!(dir.path().join(".taskd/artifacts/T1/report.md").is_file());
        assert!(dir.path().join(".taskd/artifacts/T1/result.json").is_file());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap(),
            "sibling"
        );
    }

    #[tokio::test]
    async fn non_zero_exit_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(dir.path(), "echo 'boom' 1>&2; exit 7");
        let adapter = PaperQaAdapter::new(config);
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

    /// ADR-0063 Phase 109d C1: `paperqa` が import できない (テスト環境や未整備の venv) ときの exit 3 は
    /// 一般の非 0 終了とは別扱いで、`retryable: false` の分かりやすいメッセージになる（再試行しても
    /// 直らないため）。
    #[tokio::test]
    async fn paperqa_not_importable_is_a_clear_non_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(dir.path(), "echo 'paperqa not importable: no module' 1>&2; exit 3");
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-not-importable", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(!retryable, "installing paperqa fixes this, retrying alone does not");
                assert!(message.contains("paperqa"), "{message}");
                assert!(
                    message.to_ascii_lowercase().contains("not importable")
                        || message.to_ascii_lowercase().contains("install"),
                    "message should say what is wrong, not just that it failed: {message}"
                );
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(!dir.path().join("artifacts/result.json").exists());
    }

    #[tokio::test]
    async fn empty_output_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        // `output_path` を一切書かない（`paperqa` は import できたが、何らかの理由で結果が出なかった
        // 場合の保険。ADR-0063 Phase 109d C1）。
        let config = stub_pqa(dir.path(), "exit 0");
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-3", default_limits(), &sink)
            .await
            .unwrap();
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
                r#"echo $$ > {pid}
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

    /// settings / paper_directory / index_directory / index_name の組み立てを `ask_input.json`
    /// でそのまま確認する（ADR-0063 Phase 109d C1: CLI の argv ではなく JSON になった。ADR-0035
    /// D1 / D2 で corpus と索引が**案件ごと**になったので、案件が無いタスクでは `_shared` が足される）。
    #[tokio::test]
    async fn ask_input_carries_settings_and_index_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_pqa(dir.path(), &ask_stub_script("ok", ""));
        config.settings = Some("/settings/qwen-local".to_string());
        config.paper_directory = Some(PathBuf::from("/papers"));
        config.index_directory = Some(PathBuf::from("/index"));
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req.clone(), "run-5", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));

        let input = read_ask_input(dir.path(), "run-5");
        assert_eq!(input["settings_dir"], "/settings", "{input}");
        assert_eq!(input["settings_name"], "qwen-local", "{input}");
        // ADR-0063 Phase 109e: `settings_path`（直接読む絶対パス）も足される。
        assert_eq!(input["settings_path"], "/settings/qwen-local.json", "{input}");
        assert_eq!(
            input["paper_directory"],
            format!("/papers/{SHARED_PROJECT_KEY}"),
            "{input}"
        );
        assert_eq!(
            input["index_directory"],
            format!("/index/{SHARED_PROJECT_KEY}"),
            "{input}"
        );
        assert_eq!(input["index_name"], SHARED_PROJECT_KEY, "{input}");
        assert_eq!(
            input["fallback_question"],
            build_question(&req.task, &req.context, "artifacts"),
            "{input}"
        );
    }

    /// 設定を省略したときの既定値: `settings_name`/`settings_dir` は `null`、`paper_directory`/
    /// `index_directory`/`index_name` はワークスペース相対・案件ごと（案件が無ければ `_shared`）の
    /// 既定値になる。
    #[tokio::test]
    async fn ask_input_defaults_when_settings_and_index_config_are_absent() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(dir.path(), &ask_stub_script("ok", ""));
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-6", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));

        let input = read_ask_input(dir.path(), "run-6");
        assert!(input["settings_name"].is_null(), "{input}");
        assert!(input["settings_dir"].is_null(), "{input}");
        assert!(input["settings_path"].is_null(), "{input}");
        assert_eq!(
            input["paper_directory"],
            format!("papers/{SHARED_PROJECT_KEY}"),
            "{input}"
        );
        assert_eq!(
            input["index_directory"],
            format!("index/{SHARED_PROJECT_KEY}"),
            "{input}"
        );
        assert_eq!(input["index_name"], SHARED_PROJECT_KEY, "{input}");
    }

    /// `model` は設定されているときだけ `ask_input.json` に載る（`Settings.llm` の上書き。
    /// ADR-0063 Phase 109d C1/C2）。
    #[tokio::test]
    async fn ask_input_carries_model_only_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let config_without = stub_pqa(dir.path(), &ask_stub_script("ok", ""));
        let adapter = PaperQaAdapter::new(config_without);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        adapter
            .run(req, "run-7a", default_limits(), &sink)
            .await
            .unwrap();
        assert!(
            read_ask_input(dir.path(), "run-7a")["model"].is_null(),
            "model なしなら null"
        );

        let dir2 = tempfile::tempdir().unwrap();
        let mut config_with = stub_pqa(dir2.path(), &ask_stub_script("ok", ""));
        config_with.model = Some("qwen3.8-27b".to_string());
        let adapter2 = PaperQaAdapter::new(config_with);
        let req2 = sample_req(dir2.path().to_path_buf());
        let sink2 = RecordingSink::default();
        adapter2
            .run(req2, "run-7b", default_limits(), &sink2)
            .await
            .unwrap();
        assert_eq!(
            read_ask_input(dir2.path(), "run-7b")["model"],
            "qwen3.8-27b"
        );
    }

    /// `with_env` の追加分は既存の同名キーより後に環境を組み立てるので勝つ（claude_code/codex と同じ規則）。
    #[tokio::test]
    async fn with_env_overrides_a_same_name_key_already_in_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("env-seen.txt");
        let mut config = stub_pqa(
            dir.path(),
            &format!(
                "printf '%s' \"$OPENAI_BASE_URL\" > {out}\n{ask}",
                out = out_file.display(),
                ask = ask_stub_script("ok", "")
            ),
        );
        config
            .env
            .push(("OPENAI_BASE_URL".to_string(), "http://old:1".to_string()));
        let base = PaperQaAdapter::new(config);
        let with_env = base
            .with_env(&[("OPENAI_BASE_URL".to_string(), "http://new:2".to_string())])
            .expect("paperqa supports with_env");

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

    /// LLM 供給側のエラー文面（LiteLLM の認証失敗）が `AdapterError::AuthFailed` として分類される（ADR-0010 D5）。
    #[tokio::test]
    async fn llm_auth_failure_is_classified_as_adapter_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa(
            dir.path(),
            "echo 'litellm.AuthenticationError: Invalid API key provided' 1>&2\n\
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

    // ------------------------------------------------------------ ADR-0035（Phase 34）

    /// 依頼文が日本語でも、その中の英数字の名詞句が検索語になる（ADR-0035 D1 手順 1。決定的、LLM 無し）。
    /// 実機の依頼文そのままで確認する。
    #[test]
    fn build_search_queries_takes_the_ascii_noun_phrases_out_of_a_japanese_objective() {
        let objective = "Pluvio（ad-hoc FS の I/O サーバ向け非同期ランタイム）の隣接領域: \
             asynchronous I/O runtime, ad-hoc file system, I/O offload — 直近の研究動向と候補テーマ";
        let queries = build_search_queries(objective);
        assert!(
            queries.len() >= 2 && queries.len() <= MAX_SEARCH_QUERIES,
            "{queries:?}"
        );
        assert!(
            queries.contains(&"asynchronous I/O runtime".to_string()),
            "{queries:?}"
        );
        assert!(
            queries.contains(&"ad-hoc file system".to_string()),
            "{queries:?}"
        );
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
        assert!(
            queries.is_empty() || queries.iter().all(|q| q != "of the"),
            "{queries:?}"
        );
        // 他の句に丸ごと含まれる句は落とす。
        let queries =
            build_search_queries("ad-hoc file system、ad-hoc file system checkpointing について");
        assert_eq!(
            queries,
            vec!["ad-hoc file system checkpointing".to_string()]
        );
    }

    /// 英数字が 1 つも無ければ objective 全文を 1 本の検索語にする（最後の手段）。
    #[test]
    fn build_search_queries_falls_back_to_the_whole_objective() {
        let queries = build_search_queries("非同期ランタイムの\n研究動向");
        assert_eq!(queries, vec!["非同期ランタイムの 研究動向".to_string()]);
        assert!(build_search_queries("   ").is_empty());
    }

    /// ADR-0063 Phase 109c A/C（縮小版）: 目的文から対象が取れるとき、答えの形式を
    /// 「対象ごとの節 + 対象×観点の表」に構造化するよう指示する節が問いに足される。取れなければ
    /// 従来どおり（節そのものが無い）。
    #[test]
    fn build_question_adds_structured_answer_instructions_when_targets_are_found() {
        let mut task = crate::protocol::tests::sample_task();
        task.objective = "CHFS/FINCHFS/GekkoFS のデプロイモデルを比較調査する。".to_string();
        let context = RunContext::default();
        let question = build_question(&task, &context, "artifacts");
        assert!(question.contains("## 回答の形式（必ず守ること）"), "{question}");
        assert!(question.contains("対象: CHFS、FINCHFS、GekkoFS"), "{question}");
        assert!(question.contains("観点:"), "{question}");
        assert!(question.contains("未確認"), "{question}");

        // 対象が取れない目的文では、従来どおり節そのものが付かない。
        let mut plain_task = task.clone();
        plain_task.objective = "BenchFS の設計方針を調べる。".to_string();
        let plain_question = build_question(&plain_task, &context, "artifacts");
        assert!(
            !plain_question.contains("## 回答の形式（必ず守ること）"),
            "{plain_question}"
        );
    }

    /// ADR-0063 D1: URL の種類分け（PDF / DOI / arXiv / GitHub / それ以外）は決定的。
    #[test]
    fn classify_seed_url_recognizes_pdf_doi_arxiv_and_github() {
        assert_eq!(
            classify_seed_url("https://arxiv.org/abs/2101.00001"),
            SeedUrlKind::Arxiv
        );
        assert_eq!(
            classify_seed_url("https://github.com/otatebe/chfs"),
            SeedUrlKind::Github
        );
        assert_eq!(
            classify_seed_url("https://gitlab.com/foo/bar"),
            SeedUrlKind::Github
        );
        assert_eq!(
            classify_seed_url("https://doi.org/10.1109/CHFS"),
            SeedUrlKind::Doi
        );
        assert_eq!(
            classify_seed_url("https://example.org/paper.pdf?download=1"),
            SeedUrlKind::Pdf
        );
        assert_eq!(
            classify_seed_url("https://example.org/blog/post"),
            SeedUrlKind::Other
        );
    }

    /// ADR-0063 Phase 109g B: `primary-sources`（`一次情報` 表記も）タグを持つページの `sources` だけを
    /// 拾う。タグの無いページ・`http` でない値（`human`）は無視する。**取得ランナーに渡る種**（後段で
    /// `Pdf`/`Doi`/`Arxiv` に絞る）は DOI/arXiv だけで、GitHub は分類だけされ「一次情報（実装）」に回る
    /// （`kb_primary_source_urls_are_merged_into_the_seed_urls` が実際の分岐先を確認する）。
    #[test]
    fn kb_primary_source_seed_urls_reads_only_tagged_pages_and_http_sources() {
        let knowledge = crate::protocol::KnowledgeContext {
            mounts: Vec::new(),
            index: vec![
                task_core::KnowledgeItem {
                    path: "projects/benchfs/primary-sources.md".into(),
                    title: "一次情報".into(),
                    tags: vec!["primary-sources".into()],
                    sources: vec![
                        "https://doi.org/10.1145/3492805.3492807".into(),
                        "https://github.com/otatebe/chfs".into(),
                        "human".into(),
                    ],
                    ..Default::default()
                },
                task_core::KnowledgeItem {
                    path: "projects/benchfs/note.md".into(),
                    title: "一次情報（別表記）".into(),
                    tags: vec!["一次情報".into()],
                    sources: vec!["https://arxiv.org/abs/2101.00002".into()],
                    ..Default::default()
                },
                task_core::KnowledgeItem {
                    path: "projects/benchfs/other.md".into(),
                    title: "その他".into(),
                    tags: vec!["misc".into()],
                    sources: vec!["https://arxiv.org/abs/9999.00001".into()],
                    ..Default::default()
                },
            ],
        };
        let seeds = kb_primary_source_seed_urls(Some(&knowledge));
        let urls: Vec<(&str, SeedUrlKind)> =
            seeds.iter().map(|s| (s.url.as_str(), s.kind)).collect();
        assert_eq!(
            urls,
            vec![
                ("https://doi.org/10.1145/3492805.3492807", SeedUrlKind::Doi),
                ("https://github.com/otatebe/chfs", SeedUrlKind::Github),
                ("https://arxiv.org/abs/2101.00002", SeedUrlKind::Arxiv),
            ],
            "タグ無しページ（other.md）と 'human' は拾わない"
        );
        assert!(kb_primary_source_seed_urls(None).is_empty());
    }

    /// ADR-0063 D1: 目的文中の URL をトークナイズして拾う（日本語の括弧・句読点は落とす）。
    #[test]
    fn extract_urls_pulls_http_tokens_out_of_japanese_text() {
        let text = "一次情報は（https://github.com/otatebe/chfs）と \
             https://doi.org/10.1109/CHFS.2022.1 を参照。arXiv は https://arxiv.org/abs/2101.00001v1 。";
        let urls = extract_urls(text);
        assert_eq!(
            urls,
            vec![
                "https://github.com/otatebe/chfs".to_string(),
                "https://doi.org/10.1109/CHFS.2022.1".to_string(),
                "https://arxiv.org/abs/2101.00001v1".to_string(),
            ]
        );
    }

    /// ADR-0063 D1: 起点の資料の集約。重複は落とし、出現順を保つ（目的文 + `inputs` の path）。
    #[test]
    fn extract_seed_urls_dedupes_objective_and_inputs() {
        let inputs = vec![task_core::ArtifactRef {
            name: "primary".into(),
            path: "https://github.com/tsukuba-hpcs/finchfs (see also https://arxiv.org/abs/2101.00001)"
                .into(),
            sha256: String::new(),
            kind: "text".into(),
        }];
        let seeds = extract_seed_urls(
            "CHFS (https://github.com/otatebe/chfs) と https://arxiv.org/abs/2101.00001 を調べる",
            &inputs,
        );
        let urls: Vec<&str> = seeds.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://github.com/otatebe/chfs",
                "https://arxiv.org/abs/2101.00001",
                "https://github.com/tsukuba-hpcs/finchfs",
            ],
            "重複（arxiv の URL）は 1 回だけ"
        );
        assert_eq!(seeds[0].kind, SeedUrlKind::Github);
        assert_eq!(seeds[1].kind, SeedUrlKind::Arxiv);
        assert_eq!(seeds[2].kind, SeedUrlKind::Github);
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
            command: "/home/u/celeris/paperqa/.venv/bin/pqa".to_string(),
            ..PaperQaConfig::default()
        };
        assert_eq!(
            acquire_python(&config),
            "/home/u/celeris/paperqa/.venv/bin/python3"
        );
        config.command = "pqa".to_string();
        assert_eq!(acquire_python(&config), "python3");
        config.acquire.command = Some("/usr/bin/python3.12".to_string());
        assert_eq!(acquire_python(&config), "/usr/bin/python3.12");
    }

    /// ADR-0063 Phase 109d C2: `command` が python インタプリタならそのまま、旧 `pqa` CLI を指して
    /// いれば同じディレクトリの `python` に置き換える（ディレクトリが無ければ `"python"`）。
    #[test]
    fn resolve_ask_command_replaces_the_old_pqa_cli_with_the_sibling_python() {
        let config = PaperQaConfig {
            command: "python".to_string(),
            ..PaperQaConfig::default()
        };
        assert_eq!(resolve_ask_command(&config, "run-x"), "python");

        let config = PaperQaConfig {
            command: "/home/u/celeris/paperqa/.venv/bin/pqa".to_string(),
            ..PaperQaConfig::default()
        };
        assert_eq!(
            resolve_ask_command(&config, "run-x"),
            "/home/u/celeris/paperqa/.venv/bin/python"
        );

        let config = PaperQaConfig {
            command: "pqa".to_string(),
            ..PaperQaConfig::default()
        };
        assert_eq!(resolve_ask_command(&config, "run-x"), "python");
    }

    /// ADR-0063 Phase 109e: `settings`（設定ファイルへのフルパス）を `settings_path`
    /// （`paperqa_ask.py` が**直接読む**絶対パス、`.json` 付き）/`settings_dir`/`settings_name`
    /// （`settings_path` が組めない、または見つからないときの `Settings.from_name` フォールバック用）
    /// に分ける。3 通りの入力（拡張子無しのフルパス・`.json` 付きのフルパス・ディレクトリの無い
    /// 名前だけ）すべてから正しく組まれることを確認する。
    #[test]
    fn split_settings_path_builds_the_settings_path_from_all_three_input_forms() {
        // 拡張子無しのフルパス。
        assert_eq!(
            split_settings_path("/home/u/celeris/paperqa/settings/celeris-proxy"),
            (
                Some("/home/u/celeris/paperqa/settings/celeris-proxy.json".to_string()),
                Some("/home/u/celeris/paperqa/settings".to_string()),
                Some("celeris-proxy".to_string())
            )
        );
        // `.json` が付いていても同じ（`settings_llm` と同じ前提）。
        assert_eq!(
            split_settings_path("/settings/celeris-proxy.json"),
            (
                Some("/settings/celeris-proxy.json".to_string()),
                Some("/settings".to_string()),
                Some("celeris-proxy".to_string())
            )
        );
        // ディレクトリの無い名前だけなら `settings_path`/`settings_dir` は `None`
        // （`paperqa_ask.py` 側は `Settings.from_name` に委ねる。paperqa 自身の同梱設定名を指す
        // 唯一の経路）。
        assert_eq!(
            split_settings_path("celeris-proxy"),
            (None, None, Some("celeris-proxy".to_string()))
        );
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
        assert!(answer_cites(
            "see (brinkmann2020_10-1007-s11390-020-9801-1.pdf)",
            &base
        ));
        // DOI
        assert!(answer_cites("as shown in 10.1007/s11390-020-9801-1", &base));
        // 著者姓 + 年
        assert!(answer_cites("prior work (Brinkmann 2020) shows", &base));
        assert!(answer_cites("prior work (Brinkmann2020) shows", &base));
        // タイトル
        assert!(answer_cites(
            "Ad Hoc File Systems for High-Performance Computing is a survey",
            &base
        ));
        // 何も合わなければ false
        assert!(!answer_cites(
            "no evidence was found in the provided context",
            &base
        ));

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

    /// ゲートの閾値の見方（ADR-0035 D3 / ADR-0063 D1）。既定は `insufficient_is_error = false` なので、
    /// 閾値未達でも `error` は `None`（`summary.insufficient` は `true` になる）。
    #[test]
    fn evidence_gate_counts_candidates_pdfs_and_citations() {
        let thresholds = PaperQaEvidence::default();
        assert_eq!(
            thresholds,
            PaperQaEvidence {
                min_candidates: 5,
                min_pdfs: 3,
                min_cited: 2,
                insufficient_is_error: false,
            }
        );
        let ok = evidence_gate(
            &thresholds,
            AcquireCounts {
                candidates: 6,
                pdfs: 3,
                abstracts: 0,
            },
            2,
            0,
            1,
        );
        assert!(ok.error.is_none());
        assert!(!ok.summary.insufficient);
        assert_eq!(ok.summary.cited, 2);
        // ADR-0063 Phase 109d C2: contexts との突き合わせの内訳も別枠で残る。
        assert_eq!(ok.summary.cited_from_contexts, 1);

        // 既定（insufficient_is_error = false）: 閾値未達でも hard error にはしない。
        let soft = evidence_gate(
            &thresholds,
            AcquireCounts {
                candidates: 4,
                pdfs: 1,
                abstracts: 0,
            },
            0,
            1,
            0,
        );
        assert!(soft.error.is_none(), "{:?}", soft.error);
        assert!(soft.summary.insufficient);
        assert_eq!(soft.summary.cited, 1);
        assert_eq!(soft.summary.cited_fulltext, 0);
        assert_eq!(soft.summary.cited_abstract_only, 1);
        assert_eq!(soft.summary.cited_from_contexts, 0);
        assert_eq!(soft.summary.min_cited, 2);

        // `insufficient_is_error = true`（Phase 108 までの挙動）: 実数と閾値入りのメッセージで hard error。
        let strict = PaperQaEvidence {
            insufficient_is_error: true,
            ..thresholds
        };
        let hard = evidence_gate(
            &strict,
            AcquireCounts {
                candidates: 4,
                pdfs: 1,
                abstracts: 0,
            },
            1,
            0,
            0,
        );
        let message = hard.error.unwrap();
        assert!(message.contains("candidates=4 (min 5)"), "{message}");
        assert!(message.contains("pdfs=1 (min 3)"), "{message}");
        assert!(message.contains("cited=1 (min 2)"), "{message}");

        // 0 件は別メッセージ（検索経路の問題と区別する）。`insufficient_is_error` に関わらず常に hard error。
        let zero = evidence_gate(&thresholds, AcquireCounts::default(), 0, 0, 0)
            .error
            .unwrap();
        assert!(
            zero.contains("literature search returned nothing"),
            "{zero}"
        );
        // 全部 0 ならゲート無し。
        let off = PaperQaEvidence {
            min_candidates: 0,
            min_pdfs: 0,
            min_cited: 0,
            insufficient_is_error: true,
        };
        let disabled = evidence_gate(&off, AcquireCounts::default(), 0, 0, 0);
        assert!(disabled.error.is_none());
        assert!(!disabled.summary.insufficient);
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
                "echo pqa >> {order}\n{ask}",
                order = order_log.display(),
                ask = ask_stub_script(STUB_ANSWER, "")
            ),
            &format!(
                "echo acquire >> {order}\n{script}",
                order = order_log.display(),
                script = acquire_stub_script(6, 3)
            ),
        );
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req.clone(), "run-a1", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => {
                assert!(summary.contains("Ad hoc file systems"), "{summary}")
            }
            other => panic!("expected done, got {other:?}"),
        }

        // 1. 順序（取得 → pqa）
        let order = std::fs::read_to_string(&order_log).unwrap();
        assert_eq!(
            order.lines().collect::<Vec<_>>(),
            vec!["acquire", "pqa"],
            "{order}"
        );

        // 2. 取得ランナーは run ディレクトリに書き出されて起動される（埋め込みの本体そのまま）。
        let written =
            std::fs::read_to_string(dir.path().join("runs/run-a1/paperqa_acquire.py")).unwrap();
        assert_eq!(written, ACQUIRE_SCRIPT);
        let input: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("runs/run-a1/acquire_input.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(input["max_candidates"], 30);
        assert_eq!(input["max_pdfs"], 12);
        assert!(
            input["paper_directory"]
                .as_str()
                .unwrap()
                .ends_with(&format!("papers/{SHARED_PROJECT_KEY}"))
        );
        assert!(!input["queries"].as_array().unwrap().is_empty());
        assert!(
            input["queries_path"]
                .as_str()
                .unwrap()
                .ends_with("artifacts/queries.json"),
            "{input}"
        );
        // ADR-0035 D5: モデルが分からない構成（settings も `--llm` も無い）では LLM の段は動かさず、
        // 決定的な検索語だけで検索する。
        assert_eq!(input["query_llm"]["enabled"], false, "{input}");

        // 3. 成果物 6 つの申告（ADR-0035 D5: `queries.json`、ADR-0063 D1: `research.json`、
        // ADR-0063 Phase 109b A3: `report.md` も）
        let artifacts = sink.artifacts.lock().unwrap();
        let names: Vec<&str> = artifacts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "answer.md",
                "report.md",
                "papers.json",
                "sources.json",
                "queries.json",
                "research.json"
            ],
            "{names:?}"
        );
        drop(artifacts);

        // ADR-0063 D1: 閾値を満たしているので `research.json` の証拠は「不足なし」。
        let research: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/research.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(research["evidence"]["cited"], 2, "{research}");
        assert_eq!(research["evidence"]["insufficient"], false, "{research}");

        // 4. `cited` の突き合わせ（答えが引用した 2 件だけ true）
        let sources: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/sources.json")).unwrap(),
        )
        .unwrap();
        let cited: Vec<bool> = sources
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["cited"].as_bool().unwrap())
            .collect();
        assert_eq!(cited, vec![true, true, false], "{sources}");
        assert_eq!(sources[0]["engine"], "openalex");

        // 5. `## 出典`（引用されたものが先。`[n] 著者 (年). タイトル. venue. URL`）
        let answer_md = std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap();
        assert!(
            answer_md.starts_with("Ad hoc file systems aggregate"),
            "{answer_md}"
        );
        let sources_section = answer_md
            .split("## 出典")
            .nth(1)
            .expect("出典の節があること");
        let lines: Vec<&str> = sources_section
            .lines()
            .filter(|l| l.starts_with('['))
            .collect();
        assert_eq!(lines.len(), 3, "{sources_section}");
        assert!(
            lines[0].contains("Andre Brinkmann, Kathryn Mohror, Weikuan Yu (2020)"),
            "{}",
            lines[0]
        );
        assert!(
            lines[0].contains("Ad Hoc File Systems for High-Performance Computing"),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("JCST"), "{}", lines[0]);
        assert!(
            lines[0].contains("https://doi.org/10.1007/s11390-020-9801-1"),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("(引用)"), "{}", lines[0]);
        assert!(
            !lines[2].contains("(引用)"),
            "引用されていないものは後ろ: {}",
            lines[2]
        );
        // ADR-0063 D1: 「証拠の質」節が付く（閾値を満たしているので「証拠不足」の注記は無い）。
        assert!(answer_md.contains("## 証拠の質"), "{answer_md}");
        assert!(!answer_md.contains("証拠不足"), "{answer_md}");

        assert!(dir.path().join("artifacts/result.json").is_file());
        assert!(dir.path().join("runs/run-a1/acquire.stdout.log").is_file());
    }

    /// ADR-0063 Phase 109d C2: `cited` は `ask()` が実際に使った証拠（`PQASession.contexts` の
    /// `docname`/`dockey`）との突き合わせと、答えの本文一致（`answer_cites`、補助）の**和**。
    /// 1 件目 (Brinkmann) は本文にだけ出てくる、2 件目 (Roe) は本文には出てこず `contexts` にだけ
    /// 出てくる（本文一致だけなら false のはず）、3 件目はどちらにも出ない。
    #[tokio::test]
    async fn cited_counts_the_union_of_context_docnames_and_text_matches() {
        let dir = tempfile::tempdir().unwrap();
        // Roe への言及を含まない答え（本文一致では拾えない）。
        let answer_text =
            "Ad hoc file systems aggregate node-local NVMe (brinkmann2020_10-1007-s11390-020-9801-1.pdf).";
        let contexts_json = r#"[{"text": {"doc": {"docname": "roe2021_arxiv-2101-00001v1", "dockey": "k1", "citation": "Roe (2021)"}}, "score": 5}]"#;
        let config = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script(answer_text, contexts_json),
            &acquire_stub_script(6, 3),
        );
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-refs", default_limits(), &sink)
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { .. }),
            "{:?}",
            outcome.terminal
        );
        let sources: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/sources.json")).unwrap(),
        )
        .unwrap();
        let cited: Vec<bool> = sources
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["cited"].as_bool().unwrap())
            .collect();
        assert_eq!(cited, vec![true, true, false], "{sources}");

        let research: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/research.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(research["evidence"]["cited"], 2, "{research}");
        // Roe だけが contexts 経由（Brinkmann は本文一致だけ）。
        assert_eq!(research["evidence"]["cited_from_contexts"], 1, "{research}");

        let answer_md = std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap();
        assert!(answer_md.contains("## 引用された文献（contexts）"), "{answer_md}");
        assert!(answer_md.contains("roe2021_arxiv-2101-00001v1"), "{answer_md}");
        assert!(answer_md.contains("Roe (2021)"), "{answer_md}");
    }

    /// ADR-0063 D1: 目的文中の起点 URL は種類ごとに扱いが分かれる — PDF / DOI / arXiv は取得ランナーへの
    /// `seed_urls` に渡し、GitHub は corpus に入れず `answer.md` の「一次情報（実装）」節に載せる。
    #[tokio::test]
    async fn seed_urls_split_between_the_acquire_runner_and_the_primary_sources_section() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script(STUB_ANSWER, ""),
            &acquire_stub_script(6, 3),
        );
        let adapter = PaperQaAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.task.objective = "CHFS（https://github.com/otatebe/chfs）の関連研究として \
             https://arxiv.org/abs/2101.00001 と https://doi.org/10.1109/CHFS.2022.1 を調べる"
            .to_string();
        let sink = RecordingSink::default();
        adapter
            .run(req, "run-a1b", default_limits(), &sink)
            .await
            .unwrap();

        let input: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("runs/run-a1b/acquire_input.json")).unwrap(),
        )
        .unwrap();
        let seed_urls = input["seed_urls"].as_array().unwrap();
        let kinds: Vec<&str> = seed_urls
            .iter()
            .map(|s| s["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, vec!["arxiv", "doi"], "GitHub は渡さない: {input}");

        let answer_md = std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap();
        assert!(answer_md.contains("## 一次情報（実装）"), "{answer_md}");
        assert!(
            answer_md.contains("https://github.com/otatebe/chfs"),
            "{answer_md}"
        );
    }

    /// ADR-0063 Phase 109g B: 知識ベースの `primary-sources` タグのページの `sources` にある DOI/GitHub
    /// の URL も、目的文由来の seed と同じ扱いで `acquire_input.json.seed_urls` / 「一次情報（実装）」に
    /// 入る（重複は落とす）。
    #[tokio::test]
    async fn kb_primary_source_urls_are_merged_into_the_seed_urls() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script(STUB_ANSWER, ""),
            &acquire_stub_script(6, 3),
        );
        let adapter = PaperQaAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.task.objective =
            "CHFS の関連研究として https://arxiv.org/abs/2101.00001 を調べる".to_string();
        req.context.knowledge = Some(crate::protocol::KnowledgeContext {
            mounts: Vec::new(),
            index: vec![task_core::KnowledgeItem {
                path: "projects/benchfs/primary-sources.md".into(),
                title: "一次情報".into(),
                tags: vec!["primary-sources".into()],
                sources: vec![
                    "https://doi.org/10.1145/3492805.3492807".into(),
                    "https://github.com/otatebe/chfs".into(),
                    "human".into(),
                    // 目的文とも重複する URL は 1 回だけ数える。
                    "https://arxiv.org/abs/2101.00001".into(),
                ],
                ..Default::default()
            }],
        });
        let sink = RecordingSink::default();
        adapter
            .run(req, "run-kb-seed", default_limits(), &sink)
            .await
            .unwrap();

        let input: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("runs/run-kb-seed/acquire_input.json"))
                .unwrap(),
        )
        .unwrap();
        let seed_urls = input["seed_urls"].as_array().unwrap();
        let urls: Vec<&str> = seed_urls.iter().map(|s| s["url"].as_str().unwrap()).collect();
        assert_eq!(
            urls,
            vec![
                "https://arxiv.org/abs/2101.00001",
                "https://doi.org/10.1145/3492805.3492807",
            ],
            "重複した arxiv URL は 1 回だけ、GitHub と 'human' は seed_urls に入らない: {input}"
        );

        let answer_md = std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap();
        assert!(
            answer_md.contains("https://github.com/otatebe/chfs"),
            "知識ベース由来の GitHub URL も「一次情報（実装）」に載る: {answer_md}"
        );
    }

    /// ADR-0063 Phase 109g A: `ask_input.json` の `comparison_context` に、比較先のページ探索に使う
    /// 材料（`knowledge_root`・`knowledge_index`・目的文の周辺 1 文）が入る。
    #[tokio::test]
    async fn ask_input_carries_comparison_context_ingredients() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_pqa(dir.path(), &ask_stub_script(STUB_ANSWER, ""));
        let kb_root = dir.path().join("kb");
        config.knowledge_root = Some(kb_root.clone());
        let adapter = PaperQaAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.task.objective =
            "CHFS/FINCHFS の学術文献を調査する。BenchFSとの比較が『公平比較可能』か『背景比較のみ』かを\
分類すること。".to_string();
        req.context.knowledge = Some(crate::protocol::KnowledgeContext {
            mounts: Vec::new(),
            index: vec![task_core::KnowledgeItem {
                path: "projects/benchfs/architecture-overview.md".into(),
                title: "BenchFS architecture overview".into(),
                tags: vec!["project:benchfs".into()],
                ..Default::default()
            }],
        });
        let sink = RecordingSink::default();
        adapter
            .run(req, "run-cc1", default_limits(), &sink)
            .await
            .unwrap();

        let input = read_ask_input(dir.path(), "run-cc1");
        assert_eq!(input["comparison_target"], "BenchFS", "{input}");
        let cc = &input["comparison_context"];
        assert_eq!(
            cc["knowledge_root"],
            kb_root.to_string_lossy().into_owned(),
            "{input}"
        );
        assert_eq!(
            cc["knowledge_index"][0]["path"],
            "projects/benchfs/architecture-overview.md",
            "{input}"
        );
        assert_eq!(
            cc["fallback_paragraph"],
            "BenchFSとの比較が『公平比較可能』か『背景比較のみ』かを分類すること。",
            "{input}"
        );
    }

    /// ADR-0063 Phase 109h: Phase 109g の本番 run（`01M37FZRX8GMST4SVDNNQF8NMV`）相当の目的文
    /// （対象 8 件 + 比較先「BenchFS」）で `max_asks = 8` にすると、総括の問いは必ず残り、対象側が
    /// 後ろから 1 件（`io_uring`）詰められる。その `dropped_targets`/`comparison_page`/
    /// `comparison_context_chars`（`ask_output.json`）が `research.json` にそのまま写り、
    /// `report.md` の「## 証拠の質」に「max_asks の制限で問えなかった」対象が出る。
    #[tokio::test]
    async fn dropped_targets_and_comparison_observability_flow_into_research_json_and_report_md() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script(STUB_ANSWER, ""),
            &acquire_stub_script(6, 3),
        );
        config.max_asks = 8;
        let adapter = PaperQaAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        // Phase 109g/109h の本番目的文そのもの（8 対象、比較先 BenchFS）。知識ベースは未設定なので
        // `comparison_page` は必ず `null`（フォールバック段落〈目的文全体〉を使う）になるはず。
        req.task.objective = "対象: CHFS / FINCHFS / GekkoFS / UnifyFS / BeeOND / \
Mochi-Margo-Mercury / UCX / io_uring（観点: 目的、file semantics、deployment model、\
server/core 利用、data path、BenchFS との比較分類）"
            .to_string();
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-dropped", default_limits(), &sink)
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { .. }),
            "{:?}",
            outcome.terminal
        );

        let research: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/research.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            research["dropped_targets"],
            serde_json::json!(["io_uring"]),
            "max_asks=8 では対象 8 件のうち末尾 1 件だけが詰められる: {research}"
        );
        assert!(
            research["comparison_page"].is_null(),
            "知識ベース未設定なのでページは使われない: {research}"
        );
        assert!(
            research["comparison_context_chars"].as_u64().unwrap() > 0,
            "目的文の周辺文（フォールバック段落）が使われるので 0 より大きい: {research}"
        );

        let report_md = std::fs::read_to_string(dir.path().join("artifacts/report.md")).unwrap();
        assert!(
            report_md.contains("対象 1 件は max_asks の制限で問えなかった: io_uring"),
            "{report_md}"
        );
        // 総括の問いが必ず残るので、比較分類の節自体は出る（Phase 109g からの回帰確認）。
        assert!(report_md.contains("## BenchFS との比較分類"), "{report_md}");
    }

    /// ADR-0035 D3: 閾値に足りなければ `Error{retryable}`。**成果物は残す**（`artifacts/result.json` は書かない）。
    #[tokio::test]
    async fn gate_rejects_short_evidence_but_keeps_the_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script(STUB_ANSWER, ""),
            &acquire_stub_script(3, 1),
        );
        // ADR-0063 D1: 既定（`insufficient_is_error = false`）では hard error にならないので、
        // Phase 108 までの挙動（`true`）を明示して確かめる。
        config.evidence.insufficient_is_error = true;
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-a2", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(
                    message.contains("insufficient literature evidence"),
                    "{message}"
                );
                assert!(message.contains("candidates=3 (min 5)"), "{message}");
                assert!(message.contains("pdfs=1 (min 3)"), "{message}");
                // 引用は 2 件あるので、その項目は文面に出ない。
                assert!(!message.contains("cited="), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
        // 人が読めるように残る。
        assert!(dir.path().join("artifacts/answer.md").is_file());
        assert!(dir.path().join("artifacts/report.md").is_file());
        assert!(dir.path().join("artifacts/papers.json").is_file());
        assert!(dir.path().join("artifacts/sources.json").is_file());
        // 成果物の申告はゲートより前に行うので、落ちても 6 件（`report.md`/`queries.json`/`research.json`
        // を含む。ADR-0063 Phase 109b A3）申告される。
        assert!(dir.path().join("artifacts/queries.json").is_file());
        assert!(dir.path().join("artifacts/research.json").is_file());
        assert_eq!(sink.artifacts.lock().unwrap().len(), 6);
        let research: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/research.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(research["evidence"]["insufficient"], true, "{research}");
        // ワーカープロトコル上は done ではないので `artifacts/result.json` は書かない。
        assert!(!dir.path().join("artifacts/result.json").exists());
        // 供給側の失敗にはしない（プロバイダを cooldown にする話ではない）。
        let run_result =
            std::fs::read_to_string(dir.path().join("runs/run-a2/result.json")).unwrap();
        assert!(!run_result.contains("provider_failure"), "{run_result}");
    }

    /// ADR-0063 Phase 109c A: `research.json` に目的文から取れた対象・観点が残る。対象が取れなければ
    /// 空配列。
    #[tokio::test]
    async fn research_json_records_targets_and_aspects_from_the_objective() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script(STUB_ANSWER, ""),
            &acquire_stub_script(5, 3),
        );
        let adapter = PaperQaAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.task.objective = "CHFS/FINCHFS の性能比較調査（cache 方式、file semantics）".to_string();
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-targets", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));

        let research: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/research.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(research["targets"], serde_json::json!(["CHFS", "FINCHFS"]), "{research}");
        assert_eq!(
            research["aspects"],
            serde_json::json!(["cache 方式", "file semantics"]),
            "{research}"
        );
    }

    /// ADR-0035 D3: 取得が 0 件のときだけ別メッセージ（検索経路の問題と区別する）。
    #[tokio::test]
    async fn gate_zero_candidates_uses_the_distinct_message() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script("I could not find any relevant work.", ""),
            "echo 'progress: arxiv: 0 result(s)'\necho 'CELERIS_ACQUIRE {\"candidates\": 0, \"pdfs\": 0, \"engines\": {}}'\n",
        );
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-a3", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert_eq!(
                    message,
                    "literature search returned nothing (possible network or API problem)"
                );
            }
            other => panic!("expected error, got {other:?}"),
        }
        assert!(
            dir.path().join("artifacts/answer.md").is_file(),
            "答えは残す"
        );
    }

    /// 取得ランナーが起動できない／落ちても run はそこで止めず、`pqa` まで進む（判定はゲートが行う）。
    #[tokio::test]
    async fn a_failing_acquire_runner_does_not_stop_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script("answered from the existing corpus.", ""),
            "echo 'boom' 1>&2\nexit 3\n",
        );
        // ゲートを切っておけば（既存 corpus だけで答える運用）取得の失敗でも done になる。
        config.evidence = PaperQaEvidence {
            min_candidates: 0,
            min_pdfs: 0,
            min_cited: 0,
            insufficient_is_error: false,
        };
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-a4", default_limits(), &sink)
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { .. }),
            "{:?}",
            outcome.terminal
        );
        let progress = sink.progress.lock().unwrap();
        assert!(
            progress
                .iter()
                .any(|m| m.contains("literature acquisition failed")),
            "{progress:?}"
        );
    }

    /// `max_candidates = 0` なら取得の段を行わず、ゲートも見ない（従来どおり手元の corpus だけで答える）。
    #[tokio::test]
    async fn acquire_and_the_gate_are_skipped_when_max_candidates_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        // `stub_pqa` は `max_candidates = 0`。
        let config = stub_pqa(dir.path(), &ask_stub_script("from the local corpus only.", ""));
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-a5", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        assert!(!dir.path().join("runs/run-a5/paperqa_acquire.py").exists());
        assert!(!dir.path().join("artifacts/papers.json").exists());
        let answer_md = std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap();
        assert!(!answer_md.contains("## 出典"), "{answer_md}");
        // ADR-0063 Phase 109b A3: `report.md` は取得の段を行わなくても常に書かれる。
        assert_eq!(
            sink.artifacts.lock().unwrap().len(),
            2,
            "answer.md と report.md だけ"
        );
        let report_md = std::fs::read_to_string(dir.path().join("artifacts/report.md")).unwrap();
        assert_eq!(report_md, answer_md);
    }

    // ---------------------------------------------------- 取得ランナー（python3、ネットワーク無し）

    fn python3_available() -> bool {
        match std::process::Command::new("python3")
            .arg("--version")
            .output()
        {
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
    /// `papers.json` / `sources.json` の形・`CELERIS_ACQUIRE` を確認する。
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
            "papers_path": dir.path().join("artifacts/papers.json").to_string_lossy(),
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("progress: arxiv: 2 result(s)"), "{stdout}");

        let result_line = stdout
            .lines()
            .find(|l| l.starts_with(ACQUIRE_RESULT_PREFIX))
            .expect("CELERIS_ACQUIRE");
        let counts: serde_json::Value =
            serde_json::from_str(result_line.trim_start_matches(ACQUIRE_RESULT_PREFIX)).unwrap();
        // 5 件返ってきたうち、DOI 一致とタイトル一致の 2 件が畳まれて 3 件。
        assert_eq!(counts["candidates"], 3, "{counts}");
        assert_eq!(counts["pdfs"], 1, "max_pdfs = 1 なので 1 本だけ: {counts}");
        assert_eq!(counts["engines"]["arxiv"], 1, "{counts}");
        assert_eq!(counts["engines"]["openalex"], 2, "{counts}");

        let candidates: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/papers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0]["title"], "An Asynchronous IO Runtime");
        assert_eq!(candidates[0]["arxiv_id"], "2101.00001v1");
        assert_eq!(candidates[0]["source_engine"], "arxiv");
        assert_eq!(candidates[0]["pdf_downloaded"], true);
        assert_eq!(candidates[0]["file"], "roe2021_arxiv-2101-00001v1.pdf");
        // DOI が同じ arXiv の preprint は OpenAlex 側の 1 件に畳まれ、arXiv id が補われる。
        assert_eq!(
            candidates[1]["title"],
            "Ad Hoc File Systems for High-Performance Computing"
        );
        assert_eq!(
            candidates[1]["doi"],
            "https://doi.org/10.1007/s11390-020-9801-1"
        );
        assert_eq!(candidates[1]["arxiv_id"], "2202.00002v1");
        assert_eq!(
            candidates[1]["venue"],
            "Journal of Computer Science and Technology"
        );
        assert_eq!(candidates[1]["year"], 2020);
        assert_eq!(
            candidates[1]["pdf_downloaded"], false,
            "max_pdfs を超えた分は落とさない"
        );
        // 3 件目は PDF の URL が無い候補（それでも候補としては残る）。
        assert_eq!(candidates[2]["title"], "Something Unrelated");
        assert_eq!(candidates[2]["pdf_url"], "");

        // 案件ごとの corpus にだけ書かれ、PDF は 1 本。
        let mut files: Vec<String> = std::fs::read_dir(&corpus)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(files, vec!["roe2021_arxiv-2101-00001v1.pdf".to_string()]);
        assert!(
            std::fs::read(corpus.join(&files[0]))
                .unwrap()
                .starts_with(b"%PDF")
        );

        // `sources.json` は LDR と同じ 4 つの鍵だけ。`cited` はこの時点では全部 false（ADR-0035 D2）。
        let sources: Vec<serde_json::Map<String, serde_json::Value>> = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/sources.json")).unwrap(),
        )
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
            "papers_path": dir.path().join("artifacts/papers.json").to_string_lossy(),
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            stdout.contains("already in the corpus: roe2021_arxiv-2101-00001v1.pdf"),
            "{stdout}"
        );
        // 上書きされていない（= 取り直していない）。corpus にある分は PDF 数に数える。
        assert_eq!(
            std::fs::read(&existing).unwrap(),
            b"%PDF-1.4 already here\n"
        );
        let result_line = stdout
            .lines()
            .find(|l| l.starts_with(ACQUIRE_RESULT_PREFIX))
            .unwrap();
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
# 引用符で括らない（実機で `all:"ad-hoc file system"` は 0 件だった。ADR-0035）。
# 語は AND で綴じ、カテゴリは `AND (cat:... OR cat:...)`（ADR-0035 D5。実機で確認）。
out["arxiv_url"] = mod.arxiv_url("ad-hoc file system", 20)
out["arxiv_url_cats"] = mod.arxiv_url("ad hoc file system for HPC", 20, ["cs.DC", "cs.OS", "bogus cat"])
out["arxiv_url_or"] = mod.arxiv_url("ad hoc file system", 20, ["cs.DC"], "OR")
out["openalex_url"] = mod.openalex_url("ad-hoc file system", 20, "who@example.org")
out["openalex_url_default_mailto"] = mod.openalex_url("x", 5)
out["openalex_url_filter"] = mod.openalex_url("x", 5, None, "is_oa:true")
out["excluded"] = mod.candidate_excluded({"title": "A Mobile Ad Hoc Network Survey", "abstract": ""}, ["mobile ad hoc network"])
out["not_excluded"] = mod.candidate_excluded({"title": "Ad Hoc File Systems", "abstract": "HPC burst buffers"}, ["mobile ad hoc network"])
out["excluded_by_abstract"] = mod.candidate_excluded({"title": "Runtime Verification", "abstract": "We verify JVM language runtime traces"}, ["language runtime"])
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        let arxiv_url = v["arxiv_url"].as_str().unwrap();
        assert!(
            arxiv_url.starts_with("https://export.arxiv.org/api/query?"),
            "{arxiv_url}"
        );
        // ADR-0035 D5: 語は AND で綴じる（`all:a b c` は API では OR になり、実機で
        // 「モバイル ad hoc ネットワーク」を連れてきた）。
        assert!(
            arxiv_url.contains("search_query=all%3Aad-hoc+AND+all%3Afile+AND+all%3Asystem"),
            "{arxiv_url}"
        );
        assert!(!arxiv_url.contains("%22"), "引用符で括らない: {arxiv_url}");
        assert!(
            !arxiv_url.contains("cat%3A"),
            "カテゴリを渡さなければ cat: は付かない: {arxiv_url}"
        );
        assert!(arxiv_url.contains("sortBy=relevance"), "{arxiv_url}");
        assert!(arxiv_url.contains("max_results=20"), "{arxiv_url}");
        // カテゴリ付き: 語は AND、`for` のような機能語は落ち、形の壊れたカテゴリは無視される。
        let with_cats = v["arxiv_url_cats"].as_str().unwrap();
        assert!(
            with_cats
                .contains("all%3Aad+AND+all%3Ahoc+AND+all%3Afile+AND+all%3Asystem+AND+all%3AHPC"),
            "{with_cats}"
        );
        assert!(
            with_cats.contains("AND+%28cat%3Acs.DC+OR+cat%3Acs.OS%29"),
            "{with_cats}"
        );
        assert!(
            !with_cats.contains("bogus"),
            "形の壊れたカテゴリは渡さない: {with_cats}"
        );
        // 0 件だったときの再検索は同じ語を OR で（カテゴリの縛りは残す）。
        let or_url = v["arxiv_url_or"].as_str().unwrap();
        assert!(
            or_url.contains(
                "%28all%3Aad+OR+all%3Ahoc+OR+all%3Afile+OR+all%3Asystem%29+AND+%28cat%3Acs.DC%29"
            ),
            "{or_url}"
        );
        let openalex_url = v["openalex_url"].as_str().unwrap();
        assert!(
            openalex_url.starts_with("https://api.openalex.org/works?"),
            "{openalex_url}"
        );
        // ADR-0035 D5: open access かつ Computer Science（実機で `GET /fields` で確認した id 17）。
        assert!(
            openalex_url.contains("filter=is_oa%3Atrue%2Cprimary_topic.field.id%3A17"),
            "{openalex_url}"
        );
        assert!(openalex_url.contains("per_page=20"), "{openalex_url}");
        assert!(
            openalex_url.contains("mailto=who%40example.org"),
            "{openalex_url}"
        );
        // ADR-0063 Phase 109b A1: mailto は常に付く（polite pool。設定が無ければ既定値に落ちる）。
        assert!(
            v["openalex_url_default_mailto"]
                .as_str()
                .unwrap()
                .contains("mailto=unknown%40example.org"),
            "{v}"
        );
        assert!(
            v["openalex_url_filter"]
                .as_str()
                .unwrap()
                .contains("filter=is_oa%3Atrue"),
            "設定で filter を差し替えられる: {v}"
        );
        // 除外語は決定的に効く（タイトルでも要旨でも）。
        assert_eq!(v["excluded"], "mobile ad hoc network");
        assert_eq!(v["not_excluded"], "");
        assert_eq!(v["excluded_by_abstract"], "language runtime");
        assert_eq!(v["normalize_title"], "anasynchronousioruntime");
        assert_eq!(v["normalize_doi"], "10.1/abc");
        assert_eq!(v["normalize_arxiv"], "2101.00001");
        assert_eq!(v["interleave"], serde_json::json!(["a1", "b1", "a2", "a3"]));
        // 2 件目はタイトル一致、3 件目は DOI の大文字小文字違いで 1 件目に畳まれる。
        assert_eq!(v["dedup_titles"], serde_json::json!(["T One", "Third"]));
        assert_eq!(
            v["dedup_filled_arxiv"], "2101.1v1",
            "畳んだ側の欠けた項目を補う"
        );
        assert_eq!(v["dedup_limit"], serde_json::json!(["T One"]));
        assert_eq!(
            v["pdf_filename"],
            "brinkmann2020_10-1007-s11390-020-9801-1.pdf"
        );
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
        std::fs::write(
            fixture.join("pdf-2101.00001v1"),
            "<html>login required</html>",
        )
        .unwrap();
        let corpus = dir.path().join("papers/_shared");
        let input_path = dir.path().join("acquire_input.json");
        let input = serde_json::json!({
            "queries": ["asynchronous I/O runtime"],
            "paper_directory": corpus.to_string_lossy(),
            "papers_path": dir.path().join("artifacts/papers.json").to_string_lossy(),
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(stdout.contains("not a PDF, skipped"), "{stdout}");
        assert!(!corpus.join("roe2021_arxiv-2101-00001v1.pdf").exists());
    }

    // ------------------------------------------------ ADR-0063 D1 (Phase 109)

    /// ランナーの純粋な部分（`resolve_env_placeholder` / seed の分類と組み立て / Unpaywall・
    /// Semantic Scholar の応答の読み方 / abstract のテキスト整形）を python3 で直接確認する
    /// （ネットワーク無し）。
    #[test]
    fn runner_abstract_fallback_and_oa_lookup_helpers_are_deterministic() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("paperqa_acquire.py");
        std::fs::write(&script_path, ACQUIRE_SCRIPT).unwrap();
        let checker = r##"
import importlib.util, json, os, sys
spec = importlib.util.spec_from_file_location("acq", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
out = {}

# 秘密は環境変数から解決し、それ以外の文字列はそのまま通す（ADR-0063 D4）。
os.environ["CELERIS_TEST_KEY"] = "real-secret-value"
out["placeholder_resolved"] = mod.resolve_env_placeholder("<env:CELERIS_TEST_KEY>")
out["placeholder_missing"] = mod.resolve_env_placeholder("<env:CELERIS_TEST_NOT_SET>")
out["placeholder_passthrough"] = mod.resolve_env_placeholder("unused")
out["placeholder_none"] = mod.resolve_env_placeholder(None)

# seed の分類と組み立て。
out["seed_arxiv_id"] = mod.seed_arxiv_id("https://arxiv.org/abs/2101.00001v2")
out["seed_doi"] = mod.seed_doi("https://doi.org/10.1109/CHFS.2022.1")
progress = []
out["seed_arxiv"] = mod.candidate_from_seed({"url": "https://arxiv.org/abs/2101.00001", "kind": "arxiv"}, progress.append)
out["seed_doi_candidate"] = mod.candidate_from_seed({"url": "https://doi.org/10.1/x", "kind": "doi"}, progress.append)
out["seed_pdf"] = mod.candidate_from_seed({"url": "https://example.org/paper.pdf", "kind": "pdf"}, progress.append)
out["seed_github_is_none"] = mod.candidate_from_seed({"url": "https://github.com/otatebe/chfs", "kind": "github"}, progress.append) is None
out["seed_no_url_is_none"] = mod.candidate_from_seed({"url": "", "kind": "pdf"}, progress.append) is None
out["seed_progress_count"] = len(progress)

# Unpaywall / Semantic Scholar の応答の読み方（決定的、HTTP は出ない）。
out["unpaywall_pdf"] = mod.parse_unpaywall(json.dumps({"best_oa_location": {"url_for_pdf": "https://good.example/x.pdf"}}))
out["unpaywall_none"] = mod.parse_unpaywall(json.dumps({"best_oa_location": {}}))
s2_pdf, s2_title, s2_abstract, s2_year, s2_authors = mod.parse_semantic_scholar(json.dumps({
    "title": "T", "abstract": "A", "year": 2021, "authors": [{"name": "Jane Roe"}],
    "openAccessPdf": {"url": "https://s2.example/y.pdf"},
}))
out["s2"] = [s2_pdf, s2_title, s2_abstract, s2_year, s2_authors]

# アブストのテキスト整形（本文でないことを明記する）。
text = mod.abstract_document_text({"title": "T", "authors": ["A"], "year": 2020, "doi": "10.1/x", "abstract": "the abstract"})
out["abstract_marks_not_full_text"] = "abstract only" in text
out["abstract_has_text"] = "the abstract" in text
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
        assert_eq!(v["placeholder_resolved"], "real-secret-value");
        assert_eq!(v["placeholder_missing"], "");
        assert_eq!(v["placeholder_passthrough"], "unused");
        assert_eq!(v["placeholder_none"], serde_json::Value::Null);

        assert_eq!(v["seed_arxiv_id"], "2101.00001v2");
        assert_eq!(v["seed_doi"], "10.1109/CHFS.2022.1");
        assert_eq!(v["seed_arxiv"]["arxiv_id"], "2101.00001");
        assert_eq!(v["seed_arxiv"]["pdf_url"], "https://arxiv.org/pdf/2101.00001");
        assert_eq!(v["seed_arxiv"]["source_engine"], "seed:arxiv");
        assert_eq!(v["seed_doi_candidate"]["doi"], "10.1/x");
        assert_eq!(v["seed_doi_candidate"]["url"], "https://doi.org/10.1/x");
        assert_eq!(v["seed_pdf"]["pdf_url"], "https://example.org/paper.pdf");
        assert_eq!(v["seed_github_is_none"], true, "{v}");
        assert_eq!(v["seed_no_url_is_none"], true, "{v}");
        assert_eq!(v["seed_progress_count"], 3, "{v}");

        assert_eq!(v["unpaywall_pdf"], "https://good.example/x.pdf");
        assert_eq!(v["unpaywall_none"], "");
        assert_eq!(
            v["s2"],
            serde_json::json!(["https://s2.example/y.pdf", "T", "A", 2021, ["Jane Roe"]])
        );

        assert_eq!(v["abstract_marks_not_full_text"], true, "{v}");
        assert_eq!(v["abstract_has_text"], true, "{v}");
    }

    /// ADR-0063 Phase 109b A1: OpenAlex/Unpaywall/Semantic Scholar 共通のレート制限器と 429 の
    /// 再試行間隔が決定的に効くこと（`sleep`/`now` は注入するのでネットワークにも実時間にも出ない）。
    #[test]
    fn runner_rate_limiter_and_retry_delay_helpers_are_deterministic() {
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

# レート制限器: 前回から min_interval 秒経っていなければその差分だけ眠り、経っていれば眠らない。
clock = [0.0]
sleeps = []
limiter = mod.RateLimiter(1.0, now=lambda: clock[0], sleep=lambda s: (sleeps.append(s), clock.__setitem__(0, clock[0] + s)))
limiter.wait()  # 初回は待たない
clock[0] += 0.4
limiter.wait()  # 0.6 秒待つはず
clock[0] += 2.0
limiter.wait()  # 1 秒以上経っているので待たない
out["sleeps"] = sleeps

# 429 の再試行間隔: Retry-After があればそれを使い（数値化できなければ既定へ）、無ければ 2/4/8 秒。
out["delay_with_retry_after"] = mod.compute_retry_delay(1, "30")
out["delay_with_bad_retry_after"] = mod.compute_retry_delay(2, "not-a-number")
out["delay_schedule"] = [mod.compute_retry_delay(n) for n in (1, 2, 3, 4)]

# fetch_with_retry: 429 を 2 回受けてから成功すれば、2 回だけ眠って値を返す。
import urllib.error
attempts = {"n": 0}
sleeps2 = []
def flaky():
    attempts["n"] += 1
    if attempts["n"] < 3:
        raise urllib.error.HTTPError("http://x", 429, "too many", {}, None)
    return b"ok"
out["fetch_with_retry_result"] = mod.fetch_with_retry(flaky, sleep=sleeps2.append).decode()
out["fetch_with_retry_attempts"] = attempts["n"]
out["fetch_with_retry_sleeps"] = sleeps2

# 429 が上限まで続けば最後の例外がそのまま伝播する。
def always_429():
    raise urllib.error.HTTPError("http://x", 429, "too many", {}, None)
try:
    mod.fetch_with_retry(always_429, sleep=lambda s: None)
    out["exhausted_raises"] = False
except urllib.error.HTTPError as exc:
    out["exhausted_raises"] = exc.code == 429

# 429 以外は即座に伝播する（再試行しない）。
def not_found():
    raise urllib.error.HTTPError("http://x", 404, "nope", {}, None)
try:
    mod.fetch_with_retry(not_found, sleep=lambda s: None)
    out["non_429_raises_immediately"] = False
except urllib.error.HTTPError as exc:
    out["non_429_raises_immediately"] = exc.code == 404

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
        let sleeps = v["sleeps"].as_array().unwrap();
        assert_eq!(sleeps.len(), 1, "{v}");
        assert!((sleeps[0].as_f64().unwrap() - 0.6).abs() < 1e-9, "{v}");
        assert_eq!(v["delay_with_retry_after"], 30.0);
        assert_eq!(v["delay_with_bad_retry_after"], 4.0);
        assert_eq!(v["delay_schedule"], serde_json::json!([2.0, 4.0, 8.0, 8.0]));
        assert_eq!(v["fetch_with_retry_result"], "ok");
        assert_eq!(v["fetch_with_retry_attempts"], 3);
        assert_eq!(v["fetch_with_retry_sleeps"], serde_json::json!([2.0, 4.0]));
        assert_eq!(v["exhausted_raises"], true, "{v}");
        assert_eq!(v["non_429_raises_immediately"], true, "{v}");
    }

    /// エンドツーエンド（`--fixture`）: 本文が取れない候補は abstract で妥協し（`_abstract.txt` を
    /// corpus に書き、`abstract_only = true`）、DOI がある候補は Unpaywall 経由で OA PDF が見つかれば
    /// それを使う（ADR-0063 D1）。**ネットワークには出ない**。
    #[test]
    fn runner_falls_back_to_the_abstract_and_finds_an_oa_pdf_via_unpaywall() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("fixture");
        std::fs::create_dir_all(&fixture).unwrap();
        // 1 件目: OpenAlex は PDF の URL を持たないが、DOI があるので Unpaywall で見つかる。
        // 2 件目: 見つからず、要旨があるので abstract で妥協する。
        std::fs::write(
            fixture.join("openalex-1.json"),
            r#"{"results": [
  {"id": "https://openalex.org/W1", "doi": "https://doi.org/10.1/found",
   "title": "Found via Unpaywall", "publication_year": 2021,
   "primary_location": {"source": {"display_name": "J"}},
   "abstract_inverted_index": {"Found": [0], "via": [1], "Unpaywall": [2]},
   "best_oa_location": {}, "open_access": {}, "authorships": [{"author": {"display_name": "Jane Roe"}}]},
  {"id": "https://openalex.org/W2", "doi": "https://doi.org/10.1/notfound",
   "title": "Only An Abstract", "publication_year": 2022,
   "primary_location": {"source": {"display_name": "J"}},
   "abstract_inverted_index": {"Only": [0], "an": [1], "abstract": [2]},
   "best_oa_location": {}, "open_access": {}, "authorships": [{"author": {"display_name": "Max Mustermann"}}]}
]}
"#,
        )
        .unwrap();
        std::fs::write(
            fixture.join("unpaywall-1.json"),
            r#"{"best_oa_location": {"url_for_pdf": "https://oa.example/found.pdf"}}"#,
        )
        .unwrap();
        std::fs::write(fixture.join("unpaywall-2.json"), r#"{"best_oa_location": {}}"#).unwrap();
        // Semantic Scholar のフォールバック（`{"results": []}`）は `parse_semantic_scholar` が
        // 空の値として安全に読めるので、専用のフィクスチャは要らない。
        std::fs::write(fixture.join("pdf-found.pdf"), b"%PDF-1.4\nfound\n").unwrap();

        let dir_path = dir.path().to_path_buf();
        let corpus = dir_path.join("papers/_shared");
        let input = serde_json::json!({
            "queries": ["ad hoc file system"],
            "paper_directory": corpus.to_string_lossy(),
            "papers_path": dir_path.join("artifacts/papers.json").to_string_lossy(),
            "sources_path": dir_path.join("artifacts/sources.json").to_string_lossy(),
            "max_candidates": 30,
            "max_pdfs": 5,
            "per_query": 20,
            "mailto": "who@example.org",
            "abstract_fallback": true,
        });
        let stdout = run_acquire_runner(&dir_path, &input, &fixture);
        let counts = acquire_counts(&stdout);
        assert_eq!(counts["pdfs"], 1, "{counts}");
        assert_eq!(counts["abstracts"], 1, "{counts}");

        let candidates: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(dir_path.join("artifacts/papers.json")).unwrap(),
        )
        .unwrap();
        let found = candidates
            .iter()
            .find(|c| c["title"] == "Found via Unpaywall")
            .unwrap();
        assert_eq!(found["pdf_downloaded"], true, "{found}");
        assert_eq!(found["pdf_url"], "https://oa.example/found.pdf", "{found}");
        assert_eq!(found["abstract_only"], false, "{found}");

        let abstract_only = candidates
            .iter()
            .find(|c| c["title"] == "Only An Abstract")
            .unwrap();
        assert_eq!(abstract_only["pdf_downloaded"], false, "{abstract_only}");
        assert_eq!(abstract_only["abstract_only"], true, "{abstract_only}");
        let abstract_file = abstract_only["file"].as_str().unwrap();
        assert!(abstract_file.ends_with("_abstract.txt"), "{abstract_file}");
        let text = std::fs::read_to_string(corpus.join(abstract_file)).unwrap();
        assert!(text.contains("full text could not be retrieved"), "{text}");
        assert!(text.contains("Only an abstract"), "{text}");
    }

    // ------------------------------------------------ Phase 36 / ADR-0035 D5

    /// LiteLLM の `provider/model` の接頭辞だけを落とす（`chat/completions` に渡すのは口が出している名前）。
    #[test]
    fn strip_provider_prefix_drops_only_a_litellm_style_prefix() {
        assert_eq!(strip_provider_prefix("openai/qwen3.8-27b"), "qwen3.8-27b");
        assert_eq!(strip_provider_prefix("hosted_vllm/qwen3"), "qwen3");
        assert_eq!(strip_provider_prefix(" openai/gpt-4o "), "gpt-4o");
        assert_eq!(strip_provider_prefix("qwen3.8-27b"), "qwen3.8-27b");
        // 接頭辞に見えないもの（大文字を含む組織名など）はそのまま残す。
        assert_eq!(
            strip_provider_prefix("Qwen/Qwen3.8-27B-FP8"),
            "Qwen/Qwen3.8-27B-FP8"
        );
        assert_eq!(strip_provider_prefix("openai/"), "openai/");
    }

    /// 検索語を立てるモデルは `acquire.query_model` → `[[providers]] model` → settings の `llm` の順
    /// （PaperQA2 と同じ LLM 先。ADR-0035 D5）。
    #[tokio::test]
    async fn the_query_llm_model_falls_back_from_query_model_to_llm_to_the_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("qwen-local");
        std::fs::write(
            dir.path().join("qwen-local.json"),
            r#"{"llm": "openai/qwen3.8-27b", "embedding": "sparse"}"#,
        )
        .unwrap();

        let mut config = PaperQaConfig {
            settings: Some(settings.to_string_lossy().into_owned()),
            ..PaperQaConfig::default()
        };
        // settings の `llm`（`.json` は付けずに渡す実機の仕様）。
        assert_eq!(
            query_llm_model(&config).await.as_deref(),
            Some("qwen3.8-27b")
        );
        // `[[providers]] model`（`--llm`）が勝つ。
        config.model = Some("openai/other-model".to_string());
        assert_eq!(
            query_llm_model(&config).await.as_deref(),
            Some("other-model")
        );
        // `acquire.query_model` が最も強い。
        config.acquire.query_model = Some("explicit-model".to_string());
        assert_eq!(
            query_llm_model(&config).await.as_deref(),
            Some("explicit-model")
        );
        // 何も分からなければ `None`（= LLM の段を動かさない）。
        let bare = PaperQaConfig::default();
        assert_eq!(query_llm_model(&bare).await, None);
        // settings が指すファイルが無い・`llm` が無い場合も `None`。
        let missing = PaperQaConfig {
            settings: Some(dir.path().join("nope").to_string_lossy().into_owned()),
            ..PaperQaConfig::default()
        };
        assert_eq!(query_llm_model(&missing).await, None);
    }

    /// ADR-0035 D5: 取得ランナーに渡す入力に、LLM の段の設定（PaperQA2 と同じ LLM 先）と依頼文が載る。
    #[tokio::test]
    async fn the_acquire_input_carries_the_query_llm_and_the_request() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("qwen-local.json"),
            r#"{"llm": "openai/qwen3.8-27b"}"#,
        )
        .unwrap();
        let mut config = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script(STUB_ANSWER, ""),
            &acquire_stub_script(6, 3),
        );
        config.settings = Some(dir.path().join("qwen-local").to_string_lossy().into_owned());
        config.env = vec![
            (
                "OPENAI_BASE_URL".to_string(),
                "http://127.0.0.1:18000/v1".to_string(),
            ),
            (
                "OPENAI_API_KEY".to_string(),
                "sk-should-not-leak-in-json".to_string(),
            ),
        ];
        let adapter = PaperQaAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.memory = Some(crate::protocol::MemoryContext {
            notes: "気にしない".to_string(),
            project: "  Pluvio は ad-hoc FS 向けの非同期 I/O ランタイム  ".to_string(),
        });
        let sink = RecordingSink::default();
        let _ = adapter
            .run(req.clone(), "run-a6", default_limits(), &sink)
            .await
            .unwrap();

        let input_path = dir.path().join("runs/run-a6/acquire_input.json");
        let input_text = std::fs::read_to_string(&input_path).unwrap();
        let input: serde_json::Value = serde_json::from_str(&input_text).unwrap();
        let llm = &input["query_llm"];
        assert_eq!(llm["enabled"], true, "{input}");
        assert_eq!(
            llm["model"], "qwen3.8-27b",
            "settings の llm から供給者の接頭辞を落として渡す: {input}"
        );
        assert_eq!(llm["base_url"], "http://127.0.0.1:18000/v1");
        // ADR-0063 D4（Phase 109）: 実際のキーは JSON に書かない。プレースホルダだけ
        // （実際の値は子プロセスの環境変数として渡っている。`paperqa_acquire.py` の
        // `resolve_env_placeholder` が解決する）。
        assert_eq!(llm["api_key"], "<env:OPENAI_API_KEY>");
        assert!(
            !input_text.contains("sk-should-not-leak-in-json"),
            "秘密が acquire_input.json に平文で残ってはいけない: {input_text}"
        );
        assert_eq!(llm["max_queries"], 6);
        assert_eq!(llm["timeout_secs"], 300);
        assert_eq!(input["request"]["title"], req.task.title);
        assert_eq!(input["request"]["objective"], req.task.objective);
        assert_eq!(
            input["request"]["context"],
            "Pluvio は ad-hoc FS 向けの非同期 I/O ランタイム"
        );
        // 決定的な抽出も「受け皿」として一緒に渡す。
        assert!(!input["queries"].as_array().unwrap().is_empty(), "{input}");
        // 進捗に「誰が検索語を立てるか」が出る。
        let progress = sink.progress.lock().unwrap().join("\n");
        assert!(
            progress.contains("the search terms are written by qwen3.8-27b"),
            "{progress}"
        );

        // `acquire.query_llm = false` にすると LLM の段は動かさない（従来の決定的な抽出だけ）。
        let mut off = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script(STUB_ANSWER, ""),
            &acquire_stub_script(6, 3),
        );
        off.settings = Some(dir.path().join("qwen-local").to_string_lossy().into_owned());
        off.acquire.query_llm = false;
        let adapter = PaperQaAdapter::new(off);
        let sink = RecordingSink::default();
        let _ = adapter
            .run(req, "run-a7", default_limits(), &sink)
            .await
            .unwrap();
        let input: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("runs/run-a7/acquire_input.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(input["query_llm"]["enabled"], false, "{input}");
        let progress = sink.progress.lock().unwrap().join("\n");
        assert!(
            progress.contains("search term(s) (deterministic)"),
            "{progress}"
        );
    }

    /// LLM の応答の差し替え（`--fixture` の `llm-1.json`）用の検索結果。1 件目の arXiv の 2 件目と
    /// OpenAlex の 1 件目は**除外語に当たる**（タイトルと要旨のどちらでも効くことを見る）。
    fn write_llm_stage_fixtures(dir: &Path, llm_body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("llm-1.json"), llm_body).unwrap();
        std::fs::write(
            dir.join("arxiv-1.xml"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2404.00004v1</id>
    <title>Ad Hoc File Systems for HPC Burst Buffers</title>
    <summary>We aggregate node-local NVMe into a temporary parallel file system.</summary>
    <published>2024-04-05T00:00:00Z</published>
    <author><name>Jane Roe</name></author>
    <link href="https://arxiv.org/pdf/2404.00004v1" rel="related" type="application/pdf" title="pdf"/>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2405.00005v1</id>
    <title>Routing in Mobile Ad Hoc Networks</title>
    <summary>A survey of routing protocols for wireless nodes.</summary>
    <published>2024-05-06T00:00:00Z</published>
    <author><name>Max Mustermann</name></author>
    <link href="https://arxiv.org/pdf/2405.00005v1" rel="related" type="application/pdf" title="pdf"/>
  </entry>
</feed>
"#,
        )
        .unwrap();
        // 要旨（OpenAlex の逆引き索引）だけに除外語が入っている 1 件。
        std::fs::write(
            dir.join("openalex-1.json"),
            r#"{"results": [
  {"id": "https://openalex.org/W2", "doi": "https://doi.org/10.1/p2p", "title": "Peer-to-Peer Collaboration",
   "publication_year": 2002, "primary_location": {"source": {"display_name": "Old Journal"}},
   "abstract_inverted_index": {"Mobile": [0], "ad": [1], "hoc": [2], "network": [3], "collaboration": [4]},
   "best_oa_location": {}, "open_access": {}, "authorships": [{"author": {"display_name": "Max Mustermann"}}]}
]}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("openalex-2.json"),
            r#"{"results": [
  {"id": "https://openalex.org/W3", "doi": "https://doi.org/10.1/aio", "title": "An Asynchronous IO Runtime for Storage",
   "publication_year": 2023, "primary_location": {"source": {"display_name": "TOS"}},
   "abstract_inverted_index": {"We": [0], "offload": [1], "I/O": [2]},
   "best_oa_location": {"pdf_url": "https://example.org/aio.pdf"}, "open_access": {},
   "authorships": [{"author": {"display_name": "Ada Lovelace"}}]}
]}
"#,
        )
        .unwrap();
    }

    fn run_acquire_runner(dir: &Path, input: &serde_json::Value, fixture: &Path) -> String {
        let script_path = dir.join("paperqa_acquire.py");
        std::fs::write(&script_path, ACQUIRE_SCRIPT).unwrap();
        let input_path = dir.join("acquire_input.json");
        std::fs::write(&input_path, serde_json::to_string_pretty(input).unwrap()).unwrap();
        let output = std::process::Command::new("python3")
            .arg(&script_path)
            .arg(&input_path)
            .arg("--fixture")
            .arg(fixture)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn acquire_counts(stdout: &str) -> serde_json::Value {
        let line = stdout
            .lines()
            .find(|l| l.starts_with(ACQUIRE_RESULT_PREFIX))
            .expect("CELERIS_ACQUIRE");
        serde_json::from_str(line.trim_start_matches(ACQUIRE_RESULT_PREFIX)).expect("valid JSON")
    }

    fn llm_stage_input(dir: &Path, corpus: &Path) -> serde_json::Value {
        serde_json::json!({
            "queries": ["ad-hoc FS", "I/O"],
            "query_llm": {
                "enabled": true,
                "model": "test-model-x",
                "base_url": "http://127.0.0.1:18000/v1",
                "api_key": "unused",
                "timeout_secs": 5,
                "max_tokens": 2000,
                "max_queries": 6,
            },
            "request": {"title": "学術動向調査", "objective": "Pluvio の隣接領域を調べよ", "context": null},
            "paper_directory": corpus.to_string_lossy(),
            "papers_path": dir.join("artifacts/papers.json").to_string_lossy(),
            "sources_path": dir.join("artifacts/sources.json").to_string_lossy(),
            "queries_path": dir.join("artifacts/queries.json").to_string_lossy(),
            "max_candidates": 30,
            "max_pdfs": 2,
            "per_query": 20,
            "timeout_secs": 5,
        })
    }

    /// ADR-0035 D5: ランナーは LLM が立てた検索語で検索し、除外語で候補を落とし、
    /// `queries.json` と `papers.json`（`query_text` 付き）を書く。**HTTP は出ない**。
    #[test]
    fn runner_uses_the_search_terms_the_llm_wrote() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let plan = r#"{"queries": [
             {"text": "ad hoc file system HPC", "engines": ["arxiv", "openalex"], "arxiv_categories": ["cs.DC", "cs.OS"]},
             {"text": "asynchronous I/O runtime storage", "engines": ["openalex"], "arxiv_categories": []}
           ], "exclude_terms": ["Mobile Ad Hoc Network", "no"]}"#;
        // 考える型のモデル（実機の Qwen3）を模して `<think>` と ``` で包む。
        let body = serde_json::json!({
            "choices": [{"message": {"content": format!("<think>I should answer with JSON.</think>\n```json\n{plan}\n```\n")}}]
        });
        let fixture = dir.path().join("fixture");
        write_llm_stage_fixtures(&fixture, &serde_json::to_string(&body).unwrap());
        let corpus = dir.path().join("papers").join("01PROJECT");
        let stdout =
            run_acquire_runner(dir.path(), &llm_stage_input(dir.path(), &corpus), &fixture);

        assert!(
            stdout.contains("asking test-model-x for the search terms"),
            "{stdout}"
        );
        assert!(
            stdout
                .contains("search term: 'ad hoc file system HPC' (arxiv, openalex; cs.DC, cs.OS)"),
            "{stdout}"
        );
        // 2 本目は OpenAlex だけ → arXiv は 1 回しか呼ばれない。
        assert_eq!(stdout.matches("progress: arxiv:").count(), 1, "{stdout}");
        assert!(
            stdout
                .contains("excluded ('mobile ad hoc network'): Routing in Mobile Ad Hoc Networks"),
            "{stdout}"
        );
        assert!(
            stdout.contains("2 result(s) dropped by the exclude terms"),
            "{stdout}"
        );

        let counts = acquire_counts(&stdout);
        assert_eq!(counts["query_source"], "llm", "{counts}");
        assert_eq!(counts["queries"], 2, "{counts}");
        assert_eq!(counts["excluded"], 2, "{counts}");
        assert_eq!(
            counts["candidates"], 2,
            "除外語で落ちた 2 件は候補に入らない: {counts}"
        );

        let plan_file: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/queries.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(plan_file["generated_by"], "llm");
        assert_eq!(plan_file["model"], "test-model-x");
        assert_eq!(plan_file["error"], "");
        assert!(
            plan_file["raw"]
                .as_str()
                .unwrap()
                .contains("ad hoc file system HPC"),
            "{plan_file}"
        );
        assert_eq!(plan_file["queries"][0]["text"], "ad hoc file system HPC");
        assert_eq!(
            plan_file["queries"][1]["engines"],
            serde_json::json!(["openalex"])
        );
        // カテゴリを書かなかった検索語には既定（cs.DC / cs.OS / cs.PF / cs.NI）が入る。
        assert_eq!(
            plan_file["queries"][1]["arxiv_categories"],
            serde_json::json!(["cs.DC", "cs.OS", "cs.PF", "cs.NI"])
        );
        // 除外語は小文字化され、短すぎるもの（"no"）は落ちる。
        assert_eq!(
            plan_file["exclude_terms"],
            serde_json::json!(["mobile ad hoc network"])
        );

        // どの検索語がどの候補を持ってきたか（ADR-0035 D5 手順 4）。
        let candidates: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/papers.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0]["title"],
            "Ad Hoc File Systems for HPC Burst Buffers"
        );
        assert_eq!(candidates[0]["query_text"], "ad hoc file system HPC");
        assert_eq!(candidates[0]["source_engine"], "arxiv");
        assert!(
            candidates[0]["abstract"]
                .as_str()
                .unwrap()
                .contains("node-local NVMe"),
            "{:?}",
            candidates[0]
        );
        assert_eq!(
            candidates[1]["title"],
            "An Asynchronous IO Runtime for Storage"
        );
        assert_eq!(
            candidates[1]["query_text"],
            "asynchronous I/O runtime storage"
        );
        assert_eq!(candidates[1]["source_engine"], "openalex");
        // OpenAlex の逆引き索引から戻した要旨。
        assert_eq!(candidates[1]["abstract"], "We offload I/O");
    }

    /// ADR-0035 D5: LLM の答えが JSON として使えなければ、決定的な抽出（Phase 34 まで）に落ちる。
    #[test]
    fn runner_falls_back_to_the_deterministic_terms_when_the_llm_answer_is_broken() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let body = serde_json::json!({
            "choices": [{"message": {"content": "I'm sorry, I cannot help with that request."}}]
        });
        let fixture = dir.path().join("fixture");
        write_llm_stage_fixtures(&fixture, &serde_json::to_string(&body).unwrap());
        let corpus = dir.path().join("papers").join("01PROJECT");
        let stdout =
            run_acquire_runner(dir.path(), &llm_stage_input(dir.path(), &corpus), &fixture);

        assert!(
            stdout.contains(
                "falling back to the deterministic extraction (no JSON object in the answer)"
            ),
            "{stdout}"
        );
        let counts = acquire_counts(&stdout);
        assert_eq!(counts["query_source"], "fallback", "{counts}");
        // 入力の決定的な検索語で検索する。`I/O` のような「英字が 2 文字続かない」語は
        // 検索語として使えないので落ちる（Phase 34 の実機で `I/O` 単独が PyTorch を連れてきた）。
        assert_eq!(counts["queries"], 1, "{counts}");
        // 除外語は LLM から来るものなので、落ちたときは 0 件（候補は落とさない）。
        assert_eq!(counts["excluded"], 0, "{counts}");
        assert!(counts["candidates"].as_u64().unwrap() >= 3, "{counts}");

        let plan_file: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/queries.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(plan_file["generated_by"], "fallback");
        assert_eq!(plan_file["error"], "no JSON object in the answer");
        assert_eq!(plan_file["queries"][0]["text"], "ad-hoc FS");
        assert_eq!(
            plan_file["queries"][0]["arxiv_categories"],
            serde_json::json!(["cs.DC", "cs.OS", "cs.PF", "cs.NI"])
        );
        assert_eq!(plan_file["exclude_terms"], serde_json::json!([]));
        assert_eq!(
            plan_file["raw"],
            "I'm sorry, I cannot help with that request."
        );
    }

    /// LLM の応答を読む部分（`chat/completions` の URL・本文の取り出し・JSON の切り出し・検査）は
    /// 決定的で、ネットワークには出ない（ADR-0035 D5）。
    #[test]
    fn runner_query_plan_parsing_is_deterministic() {
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
out["url_v1"] = mod.chat_completions_url("http://127.0.0.1:18000/v1")
out["url_slash"] = mod.chat_completions_url("http://h/v1/")
out["url_full"] = mod.chat_completions_url("http://h/v1/chat/completions")
out["url_empty"] = mod.chat_completions_url(None)
# 考える型のモデルは `content` を空にして `reasoning` に書くことがある。
out["text_reasoning"] = mod.message_text(json.dumps(
    {"choices": [{"message": {"content": "", "reasoning": "thinking {\"queries\": []}"}}]}))
try:
    mod.message_text(json.dumps({"choices": []}))
    out["text_no_choices"] = "no error"
except ValueError as exc:
    out["text_no_choices"] = str(exc)
out["json_from_fence"] = mod.extract_json_object("<think>a {b}</think> ```json\n{\"a\": {\"b\": 1}}\n``` tail")
out["json_none"] = mod.extract_json_object("no braces here")
plan = {"queries": [
          {"text": "  ad hoc  file system HPC ", "engines": ["ARXIV", "bogus"], "arxiv_categories": ["cs.DC", "!!"]},
          {"text": "ad hoc file system hpc"},
          "asynchronous I/O runtime storage",
          {"text": "日本語だけ"},
          {"text": "one two three four five six seven eight nine ten"}],
        "exclude_terms": ["Mobile Ad Hoc NETWORK", "ok", "vehicular network", "mobile ad hoc network"]}
queries, excludes = mod.parse_query_plan(json.dumps(plan), ["cs.DC", "cs.NI"], 6)
out["queries"] = queries
out["excludes"] = excludes
out["capped"] = len(mod.parse_query_plan(
    json.dumps({"queries": [{"text": "q%d aa bb" % i} for i in range(9)]}), ["cs.DC"], 6)[0])
for name, text in (("broken", "{\"queries\": [oops}"), ("empty", "{\"queries\": []}"), ("nolist", "{\"queries\": 3}"),
                   ("nojson", "sorry")):
    try:
        mod.parse_query_plan(text, ["cs.DC"], 6)
        out["err_" + name] = "no error"
    except ValueError as exc:
        out["err_" + name] = str(exc)
messages = mod.build_query_messages({"title": "T", "objective": "調べよ", "context": "案件の文脈"})
out["prompt_roles"] = [m["role"] for m in messages]
out["prompt_has_objective"] = "調べよ" in messages[1]["content"]
out["prompt_has_context"] = "案件の文脈" in messages[1]["content"]
out["prompt_has_rules"] = all(s in messages[1]["content"] for s in (
    "English only", "exclude_terms", "arxiv_categories", "Pluvio"))
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

        assert_eq!(v["url_v1"], "http://127.0.0.1:18000/v1/chat/completions");
        assert_eq!(v["url_slash"], "http://h/v1/chat/completions");
        assert_eq!(v["url_full"], "http://h/v1/chat/completions");
        assert_eq!(v["url_empty"], "");
        assert!(
            v["text_reasoning"].as_str().unwrap().contains("thinking"),
            "{v}"
        );
        assert_eq!(v["text_no_choices"], "no choices in the response");
        assert_eq!(v["json_from_fence"], "{\"a\": {\"b\": 1}}");
        assert_eq!(v["json_none"], serde_json::Value::Null);

        // 検索語の検査: 空白を畳み、engines は小文字の既知のものだけ、壊れたカテゴリは既定に差し替え、
        // 同じ語（大文字小文字違い）は 1 本、ASCII の字を持たない語と長すぎる語は切る。
        let queries = v["queries"].as_array().unwrap();
        assert_eq!(queries.len(), 3, "{v}");
        assert_eq!(queries[0]["text"], "ad hoc file system HPC");
        assert_eq!(queries[0]["engines"], serde_json::json!(["arxiv"]));
        assert_eq!(queries[0]["arxiv_categories"], serde_json::json!(["cs.DC"]));
        assert_eq!(queries[1]["text"], "asynchronous I/O runtime storage");
        assert_eq!(
            queries[1]["engines"],
            serde_json::json!(["arxiv", "openalex"]),
            "既定は両方"
        );
        assert_eq!(
            queries[1]["arxiv_categories"],
            serde_json::json!(["cs.DC", "cs.NI"]),
            "既定のカテゴリ"
        );
        assert_eq!(
            queries[2]["text"], "one two three four five six seven eight",
            "8 語で切る"
        );
        assert_eq!(
            v["excludes"],
            serde_json::json!(["mobile ad hoc network", "vehicular network"]),
            "小文字化・重複排除・短すぎるものは落とす: {v}"
        );
        assert_eq!(v["capped"], 6, "上限は 6 本");

        // 壊れた答えは必ず `ValueError`（呼び出し側が決定的な抽出に落ちる）。
        assert_eq!(v["err_nojson"], "no JSON object in the answer");
        assert!(
            v["err_broken"]
                .as_str()
                .unwrap()
                .starts_with("the JSON object is broken"),
            "{v}"
        );
        assert_eq!(v["err_empty"], "no usable query in `queries`");
        assert_eq!(v["err_nolist"], "`queries` is not a list");

        // プロンプト（ADR-0035 D5: 英語・固有名詞・曖昧語・除外語の指示）。
        assert_eq!(v["prompt_roles"], serde_json::json!(["system", "user"]));
        assert_eq!(v["prompt_has_objective"], true);
        assert_eq!(v["prompt_has_context"], true);
        assert_eq!(v["prompt_has_rules"], true);
    }

    // ---------------------------------------------- paperqa_ask.py（python3、ネットワーク無し）

    /// ADR-0063 Phase 109d/109e: `paperqa_ask.py` の純関数（質問の生成・contexts の平坦化・
    /// 表の組み立て・`settings_path` の組み立て）を `python3 -c` から直接呼ぶ
    /// （`paperqa` パッケージ無しで動く）。
    #[test]
    fn ask_script_pure_functions_build_questions_flatten_contexts_and_the_table() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("paperqa_ask.py");
        std::fs::write(&script_path, ASK_SCRIPT).unwrap();
        let checker = r##"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("ask", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

out = {}

# (b) 対象 5 件 + 総括（比較先あり）で 6 問、max_asks=8 でも 5 件とも入って何も削られない。
qs_default = mod.build_questions_for_targets(
    ["A", "B", "C", "D", "E"], ["x", "y"], "BenchFS", 8, "fallback")
out["default_count"] = len(qs_default["questions"])
out["default_ids"] = [q["id"] for q in qs_default["questions"]]
out["default_last_target"] = qs_default["questions"][-1]["target"]
out["default_dropped"] = qs_default["dropped_targets"]

# ADR-0063 Phase 109h: max_asks=4 (< 5 対象 + 総括) でも総括の問いは必ず残り、対象側が後ろから
# 削られる（max_asks - 1 = 3 件まで）。削られた対象は dropped_targets に残る。
qs_capped = mod.build_questions_for_targets(
    ["A", "B", "C", "D", "E"], ["x", "y"], "BenchFS", 4, "fallback")
out["capped_count"] = len(qs_capped["questions"])
out["capped_ids"] = [q["id"] for q in qs_capped["questions"]]
out["capped_dropped"] = qs_capped["dropped_targets"]

# ADR-0063 Phase 109h 実測相当: 対象 8 件 + 比較先あり、max_asks=8 なら対象は前から 7 件、最後は
# summary、8 件目（末尾、io_uring）が dropped_targets に。max_asks=10（新しい既定）なら 9 件すべて
# 入り、何も削られない。
eight_targets = [
    "CHFS", "FINCHFS", "GekkoFS", "UnifyFS", "BeeOND", "Mochi-Margo-Mercury", "UCX", "io_uring",
]
qs_eight_capped = mod.build_questions_for_targets(eight_targets, ["x"], "BenchFS", 8, "fallback")
out["eight_capped_count"] = len(qs_eight_capped["questions"])
out["eight_capped_last_id"] = qs_eight_capped["questions"][-1]["id"]
out["eight_capped_target_count"] = len(
    [q for q in qs_eight_capped["questions"] if q.get("target")]
)
out["eight_capped_dropped"] = qs_eight_capped["dropped_targets"]

qs_eight_default_max = mod.build_questions_for_targets(
    eight_targets, ["x"], "BenchFS", mod.DEFAULT_MAX_ASKS, "fallback"
)
out["eight_default_max_count"] = len(qs_eight_default_max["questions"])
out["eight_default_max_dropped"] = qs_eight_default_max["dropped_targets"]
out["default_max_asks"] = mod.DEFAULT_MAX_ASKS

# 比較先が無ければ総括の問いは無い（対象は全部入るのに max_asks 未満で終わる。dropped_targets も
# 追跡しない -- 従来どおり）。
qs_no_comparison = mod.build_questions_for_targets(["A", "B"], ["x"], None, 8, "fallback")
out["no_comparison_count"] = len(qs_no_comparison["questions"])
out["no_comparison_ids"] = [q["id"] for q in qs_no_comparison["questions"]]
out["no_comparison_dropped"] = qs_no_comparison["dropped_targets"]

# (c) 対象が無ければフォールバックの単一の問い。
qs_none = mod.build_questions_for_targets([], ["x", "y"], "BenchFS", 8, "the fallback question")
out["none_count"] = len(qs_none["questions"])
out["none_question"] = qs_none["questions"][0]["question"]
out["none_target"] = qs_none["questions"][0]["target"]
out["none_dropped"] = qs_none["dropped_targets"]

# contexts の平坦化（dict 形の偽の Context/Text/Doc、paperqa 無しで動く）。
contexts = [
    {"text": {"doc": {"docname": "roe2021", "dockey": "k1", "citation": "Roe 2021"}}, "score": 5},
    {"text": {"doc": {"docname": "", "dockey": "k2", "citation": ""}}, "score": None},
]
out["flat"] = mod.flatten_contexts(contexts, "t1")

# (d) 表の組み立て: 観点が答えに無ければ「未確認」。
answers = [
    {"target": "A", "answer": "- x: fact about A (cite1)\n- y: something else entirely"},
    {"target": "B", "answer": "no useful information here"},
]
out["table"] = mod.build_target_aspect_table(["A", "B"], ["x", "y"], answers)
out["table_empty_without_targets"] = mod.build_target_aspect_table([], ["x"], answers)
out["table_empty_without_aspects"] = mod.build_target_aspect_table(["A"], [], answers)

# (f) ADR-0063 Phase 109f: 答えの先頭に質問がエコーされていても、そのエコー行を観点の値として
# 拾わない（実測の事故 run 01M37AZ129EMB93N50MZ132S8K: 全セルに質問文がそのまま入っていた）。
echoed_question = (
    "Question: C について、次の観点を提示された文献の範囲で答えよ: latency、memory。"
    "文献に無い観点は『未確認』と書け。各事実に引用を付けよ。"
)
answers_with_echo = [
    {
        "target": "C",
        "question": echoed_question,
        "answer": echoed_question + "\n- latency: 5ms observed (cite2)\nno further detail found",
    },
]
out["table_with_echoed_question"] = mod.build_target_aspect_table(
    ["C"], ["latency", "memory"], answers_with_echo
)
# 直接 _extract_aspect_line も確認（エコー行だけの答えは「未確認」になる: 拾えるものが無い）。
out["extract_from_echo_only"] = mod._extract_aspect_line(
    echoed_question, "latency", echoed_question
)

# 再試行の判定（is_transient_error）: 503/429/502/接続断は再試行対象、それ以外は対象外。
out["transient"] = [
    mod.is_transient_error(Exception("HTTP Error 503: Service Unavailable")),
    mod.is_transient_error(Exception("429 Too Many Requests")),
    mod.is_transient_error(Exception("connection reset by peer")),
    mod.is_transient_error(Exception("KeyError: 'missing'")),
]

# (e) ADR-0063 Phase 109e: resolve_settings_path は Rust の split_settings_path と同じ組み方
# （dir + name(.json 無し) -> "<dir>/<name>.json"）。名前だけ・ディレクトリだけでは組めない。
out["settings_path_full"] = mod.resolve_settings_path("/settings", "celeris-proxy")
out["settings_path_name_already_json"] = mod.resolve_settings_path("/settings", "celeris-proxy.json")
out["settings_path_no_dir"] = mod.resolve_settings_path(None, "celeris-proxy")
out["settings_path_no_name"] = mod.resolve_settings_path("/settings", None)

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

        assert_eq!(v["default_count"], 6, "{v}");
        assert_eq!(
            v["default_ids"],
            serde_json::json!(["t1", "t2", "t3", "t4", "t5", "summary"]),
            "{v}"
        );
        assert_eq!(v["default_last_target"], serde_json::Value::Null, "{v}");
        assert_eq!(v["default_dropped"], serde_json::json!([]), "{v}");

        // ADR-0063 Phase 109h: 総括の問いは必ず残る。対象側が後ろから 3 件（max_asks - 1）に詰まる。
        assert_eq!(
            v["capped_count"], 4,
            "max_asks=4 でも総括は必ず残り、対象が 3 件に詰まる: {v}"
        );
        assert_eq!(
            v["capped_ids"],
            serde_json::json!(["t1", "t2", "t3", "summary"]),
            "{v}"
        );
        assert_eq!(v["capped_dropped"], serde_json::json!(["D", "E"]), "{v}");

        // Phase 109g の本番 run 相当（対象 8 件、max_asks=8）: 総括は必ず残り、対象は前から 7 件、
        // 末尾（io_uring）だけが dropped_targets に落ちる。
        assert_eq!(v["eight_capped_count"], 8, "{v}");
        assert_eq!(v["eight_capped_last_id"], "summary", "{v}");
        assert_eq!(v["eight_capped_target_count"], 7, "{v}");
        assert_eq!(
            v["eight_capped_dropped"],
            serde_json::json!(["io_uring"]),
            "{v}"
        );

        // 既定の max_asks（10）なら 8 対象 + 総括の 9 件すべて入り、何も削られない。
        assert_eq!(v["default_max_asks"], 10, "既定は 8 から 10 に引き上げ: {v}");
        assert_eq!(v["eight_default_max_count"], 9, "{v}");
        assert_eq!(v["eight_default_max_dropped"], serde_json::json!([]), "{v}");

        assert_eq!(v["no_comparison_count"], 2, "比較先が無ければ総括は無い: {v}");
        assert_eq!(v["no_comparison_ids"], serde_json::json!(["t1", "t2"]), "{v}");
        assert_eq!(v["no_comparison_dropped"], serde_json::json!([]), "{v}");

        assert_eq!(v["none_count"], 1, "{v}");
        assert_eq!(v["none_question"], "the fallback question", "{v}");
        assert_eq!(v["none_target"], serde_json::Value::Null, "{v}");
        assert_eq!(v["none_dropped"], serde_json::json!([]), "{v}");

        assert_eq!(
            v["flat"],
            serde_json::json!([
                {"docname": "roe2021", "dockey": "k1", "citation": "Roe 2021", "score": 5, "question": "t1"},
                {"docname": "", "dockey": "k2", "citation": "", "score": serde_json::Value::Null, "question": "t1"},
            ]),
            "{v}"
        );

        let table = v["table"].as_str().unwrap();
        assert!(table.contains("| 対象 | x | y |"), "{table}");
        assert!(table.contains("| A | fact about A (cite1) | something else entirely |"), "{table}");
        assert!(
            table.contains("| B | 未確認 | 未確認 |"),
            "B の答えにはどちらの観点も無いので未確認: {table}"
        );
        assert_eq!(v["table_empty_without_targets"], "");
        assert_eq!(v["table_empty_without_aspects"], "");

        let table_with_echo = v["table_with_echoed_question"].as_str().unwrap();
        assert!(
            table_with_echo.contains("| C | 5ms observed (cite2) | 未確認 |"),
            "エコーされた質問行を観点の値として拾ってはいけない: {table_with_echo}"
        );
        assert!(
            !table_with_echo.contains("Question:"),
            "エコーされた質問文がセルに残ってはいけない: {table_with_echo}"
        );
        assert_eq!(
            v["extract_from_echo_only"], "未確認",
            "エコー行しか無い答えは拾えるものが無いので未確認: {v}"
        );

        assert_eq!(
            v["transient"],
            serde_json::json!([true, true, true, false]),
            "{v}"
        );

        assert_eq!(v["settings_path_full"], "/settings/celeris-proxy.json", "{v}");
        assert_eq!(
            v["settings_path_name_already_json"],
            "/settings/celeris-proxy.json",
            "{v}"
        );
        assert_eq!(v["settings_path_no_dir"], serde_json::Value::Null, "{v}");
        assert_eq!(v["settings_path_no_name"], serde_json::Value::Null, "{v}");
    }

    /// ADR-0063 Phase 109g A: `paperqa_ask.py` の比較分類（judgement）純関数を `python3 -c` から直接
    /// 呼ぶ（`paperqa` パッケージ無しで動く）— 設計条件の抽出（knowledge index + 本文から）、総括の問いの
    /// 組み立て（設計条件・対象別の答えが入る、3 KB / 1.5 KB の切り詰め）、総括の答えから分類語と確度を
    /// 抜く。
    #[test]
    fn comparison_judgement_pure_functions_extract_build_and_parse() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("paperqa_ask.py");
        std::fs::write(&script_path, ASK_SCRIPT).unwrap();
        let checker = r##"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("ask", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

out = {}

# find_comparison_page_path: title/tags/path のいずれかに比較先の名前があれば最初の 1 件。
index = [
    {"path": "projects/other/note.md", "title": "別件", "tags": ["misc"]},
    {"path": "projects/benchfs/architecture-overview.md", "title": "BenchFS architecture", "tags": ["project:benchfs"]},
]
out["page_path_found"] = mod.find_comparison_page_path(index, "BenchFS")
out["page_path_no_target"] = mod.find_comparison_page_path(index, None)
out["page_path_no_match"] = mod.find_comparison_page_path(index, "SomethingElse")

# ADR-0063 Phase 109h: 実測（run 01M37FZRX8GMST4SVDNNQF8NMV）相当の索引 -- 25 件、同じ path の
# 重複（projects/README.md、projects/benchfs/architecture-overview.md）と、"benchfs" を含む
# 複数のページ（primary-sources.md / known-issues-inventory.md / architecture-overview.md）。
# 重複は 1 回だけ扱い、"architecture"/"overview"/"design" を含む path を優先する
# （primary-sources.md や known-issues-inventory.md ではなく architecture-overview.md を選ぶ）。
real_index = (
    [{"path": "projects/README.md", "title": "Projects", "tags": []}] * 2
    + [
        {"path": "user/preferences.md", "title": "User preferences", "tags": ["user"]},
        {"path": "user/notes.md", "title": "User notes", "tags": ["user"]},
    ]
    + [
        {"path": "projects/agent-platform/%s.md" % n, "title": "agent-platform %s" % n, "tags": ["project:agent-platform"]}
        for n in ["overview", "roadmap", "adr-index", "glossary", "progress"]
    ]
    + [
        {
            "path": "projects/benchfs/known-issues-inventory.md",
            "title": "BenchFS known issues",
            "tags": ["project:benchfs"],
        },
        {
            "path": "projects/benchfs/primary-sources.md",
            "title": "BenchFS primary sources",
            "tags": ["project:benchfs", "primary-sources"],
        },
    ]
    + [
        {
            "path": "projects/benchfs/architecture-overview.md",
            "title": "BenchFS architecture overview",
            "tags": ["project:benchfs"],
        }
    ]
    * 2
    + [
        {"path": "projects/other-%d/note.md" % n, "title": "Other project %d" % n, "tags": ["misc"]}
        for n in range(12)
    ]
)
out["real_index_len"] = len(real_index)
out["real_index_page"] = mod.find_comparison_page_path(real_index, "BenchFS")

# strip_front_matter_block: front matter を落として本文だけ残す。閉じていない '---' はそのまま。
paged = "---\ntitle: BenchFS\ntags: [project:benchfs]\n---\nBenchFS aggregates node-local NVMe.\n"
out["stripped"] = mod.strip_front_matter_block(paged)
out["stripped_unclosed"] = mod.strip_front_matter_block("---\nnot closed")

# comparison_design_context: ページ本文があればそれを、無ければ objective の周辺文、どちらも無ければ None。
out["design_from_page"] = mod.comparison_design_context(paged, "fallback sentence.")
out["design_from_fallback"] = mod.comparison_design_context(None, "fallback sentence.")
out["design_from_fallback_empty_page"] = mod.comparison_design_context("---\ntitle: x\n---\n   \n", "fallback sentence.")
out["design_none"] = mod.comparison_design_context(None, None)
# 3 KB の切り詰め。
long_body = "x" * 4000
out["design_truncated_len"] = len(mod.comparison_design_context(long_body, None))
out["design_truncated_ends_with_ellipsis"] = mod.comparison_design_context(long_body, None).endswith("…")

# build_comparison_question: comparison_context 無しは旧来の 1 行の問い（未確認を許す）。
q_old = mod.build_comparison_question("BenchFS", ["CHFS", "FINCHFS"], {}, None)
out["old_style_question"] = q_old["question"]
out["old_style_id"] = q_old["id"]
out["old_style_target"] = q_old["target"]

# comparison_context ありは設計条件と対象別の答え（切り詰め）を材料にした判断の問い。
target_answers = {"CHFS": "node-local NVMe cache. " * 200, "FINCHFS": "short answer"}
q_new = mod.build_comparison_question("BenchFS", ["CHFS", "FINCHFS"], target_answers, "BenchFS aggregates node-local NVMe.")
out["new_question"] = q_new["question"]
out["new_question_id"] = q_new["id"]

# extract_comparison_classification: 両方の表記、無ければ未確認。
summary = (
    "- CHFS: 公平比較可能（高） — 両方とも node-local aggregation。\n"
    "- FINCHFS: 背景比較のみ（中） — 直接の設計対応が無い。\n"
    "- UnifyFS: 公平比較可能 — 確度の表記なし。\n"
)
out["cls_chfs"] = mod.extract_comparison_classification(summary, "CHFS")
out["cls_finchfs"] = mod.extract_comparison_classification(summary, "FINCHFS")
out["cls_no_confidence"] = mod.extract_comparison_classification(summary, "UnifyFS")
out["cls_missing"] = mod.extract_comparison_classification(summary, "GekkoFS")
out["cls_no_summary"] = mod.extract_comparison_classification("", "CHFS")

# build_target_aspect_table: 比較分類の列は総括の答えから、それ以外の列は対象ごとの答えから。
answers = [
    {"target": "CHFS", "answer": "- cache: node-local NVMe"},
    {"target": None, "id": "summary", "answer": summary},
]
out["table_with_comparison"] = mod.build_target_aspect_table(
    ["CHFS", "FINCHFS"], ["cache", "BenchFS との比較分類"], answers, "BenchFS"
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

        assert_eq!(
            v["page_path_found"],
            "projects/benchfs/architecture-overview.md",
            "{v}"
        );
        assert_eq!(v["page_path_no_target"], serde_json::Value::Null, "{v}");
        assert_eq!(v["page_path_no_match"], serde_json::Value::Null, "{v}");

        // ADR-0063 Phase 109h: 実測相当の 25 件の索引（重複あり）でも architecture-overview.md を選ぶ
        // （primary-sources.md や known-issues-inventory.md ではなく）。
        assert_eq!(v["real_index_len"], 25, "{v}");
        assert_eq!(
            v["real_index_page"],
            "projects/benchfs/architecture-overview.md",
            "重複や他の benchfs ページより architecture-overview.md を優先: {v}"
        );

        assert_eq!(
            v["stripped"],
            "BenchFS aggregates node-local NVMe.\n",
            "{v}"
        );
        assert_eq!(v["stripped_unclosed"], "---\nnot closed", "{v}");

        assert_eq!(
            v["design_from_page"],
            "BenchFS aggregates node-local NVMe.",
            "{v}"
        );
        assert_eq!(v["design_from_fallback"], "fallback sentence.", "{v}");
        assert_eq!(
            v["design_from_fallback_empty_page"], "fallback sentence.",
            "本文が front matter しか無ければ空とみなし fallback に落ちる: {v}"
        );
        assert_eq!(v["design_none"], serde_json::Value::Null, "{v}");
        assert_eq!(v["design_truncated_len"], 3001, "3000 文字 + 省略記号: {v}");
        assert_eq!(v["design_truncated_ends_with_ellipsis"], true, "{v}");

        assert_eq!(
            v["old_style_question"],
            "対象ごとに BenchFS と『公平比較可能』か『背景比較のみ』かを分類し理由を1行で述べよ。\n\
             対象: CHFS、FINCHFS",
            "{v}"
        );
        assert_eq!(v["old_style_id"], "summary", "{v}");
        assert_eq!(v["old_style_target"], serde_json::Value::Null, "{v}");

        let new_question = v["new_question"].as_str().unwrap();
        assert!(
            new_question.contains("以下は BenchFS の設計条件である"),
            "{new_question}"
        );
        assert!(
            new_question.contains("BenchFS aggregates node-local NVMe."),
            "{new_question}"
        );
        assert!(
            new_question.contains("必ず『公平比較可能』か『背景比較のみ』のどちらかに分類"),
            "{new_question}"
        );
        assert!(
            new_question.contains("文献にBenchFSへの言及が無いことは理由にならない"),
            "{new_question}"
        );
        assert!(
            new_question.contains("判断の確度（高/中/低）も添えよ"),
            "{new_question}"
        );
        assert!(new_question.contains("### CHFS の対象別の答え"), "{new_question}");
        assert!(new_question.contains("### FINCHFS の対象別の答え"), "{new_question}");
        assert!(new_question.contains("short answer"), "{new_question}");
        // CHFS の答えは 1.5 KB に切り詰められる（4600 文字近い繰り返し文字列 -> 1500 + 省略記号）。
        let chfs_material_len = new_question
            .split("### CHFS の対象別の答え\n")
            .nth(1)
            .unwrap()
            .split("\n\n### FINCHFS")
            .next()
            .unwrap()
            .chars()
            .count();
        assert!(
            chfs_material_len <= 1501,
            "CHFS の対象別の答えは 1.5 KB に切り詰められるはず: {chfs_material_len}"
        );
        assert_eq!(v["new_question_id"], "summary", "{v}");

        assert_eq!(v["cls_chfs"], "公平比較可能（高）", "{v}");
        assert_eq!(v["cls_finchfs"], "背景比較のみ（中）", "{v}");
        assert_eq!(
            v["cls_no_confidence"], "公平比較可能",
            "確度の表記が無ければラベルだけ: {v}"
        );
        assert_eq!(v["cls_missing"], "未確認", "{v}");
        assert_eq!(v["cls_no_summary"], "未確認", "{v}");

        let table = v["table_with_comparison"].as_str().unwrap();
        assert!(
            table.contains("| CHFS | node-local NVMe | 公平比較可能（高） |"),
            "比較分類の列は総括の答えから: {table}"
        );
        assert!(
            table.contains("| FINCHFS | 未確認 | 背景比較のみ（中） |"),
            "FINCHFS 自身の対象別の答えは無いので cache 列は未確認、比較分類だけ総括から: {table}"
        );
    }

    /// ADR-0063 Phase 109e: `settings_path` が指す設定ファイルが**実在すれば**、`paperqa_ask.py::main`
    /// は `Settings.from_name` を経由せずそれを直接読んで（`Settings.model_validate_json` →
    /// `model_dump()` → `Settings(**...)`）`ask()` に渡し、`output_path` に答えを書く
    /// （本番で観測した `Settings.from_name` が `PQA_SETTINGS_DIR` を見ない不具合の直し）。
    /// 偽の `paperqa`（`sys.modules` に差し込む最小スタブ）を使い、ネットワークには出ない。
    #[test]
    fn paperqa_ask_main_reads_the_settings_file_directly_when_it_exists() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("paperqa_ask.py");
        std::fs::write(&script_path, ASK_SCRIPT).unwrap();

        let settings_dir = dir.path().join("settings");
        std::fs::create_dir_all(&settings_dir).unwrap();
        let settings_path = settings_dir.join("celeris-proxy.json");
        std::fs::write(&settings_path, r#"{"llm": "qwen3.8-27b", "marker": "from-disk"}"#).unwrap();

        let output_path = dir.path().join("ask_output.json");
        let input = serde_json::json!({
            "settings_name": "celeris-proxy",
            "settings_dir": settings_dir.to_string_lossy(),
            "settings_path": settings_path.to_string_lossy(),
            "paper_directory": dir.path().join("papers").to_string_lossy(),
            "index_directory": dir.path().join("index").to_string_lossy(),
            "index_name": "proj",
            "model": null,
            "targets": [],
            "aspects": [],
            "comparison_target": null,
            "max_asks": 8,
            "fallback_question": "the question",
            "output_path": output_path.to_string_lossy(),
        });
        let input_path = dir.path().join("input.json");
        std::fs::write(&input_path, serde_json::to_string_pretty(&input).unwrap()).unwrap();

        let driver = r##"
import importlib.util, json, sys, types

script_path, input_path = sys.argv[1], sys.argv[2]
calls = {"from_name_called": False, "validated_raw": None, "settings_marker_seen_by_ask": None}


class FakeIndex:
    def __init__(self):
        self.paper_directory = None
        self.index_directory = None
        self.name = None


class FakeAgent:
    def __init__(self):
        self.index = FakeIndex()


class FakeSettings:
    def __init__(self, **kw):
        self.agent = FakeAgent()
        self.llm = kw.get("llm")
        self.marker = kw.get("marker")

    @classmethod
    def model_validate_json(cls, text):
        data = json.loads(text)
        calls["validated_raw"] = data
        inst = cls(**data)
        inst._raw = data
        return inst

    def model_dump(self):
        return dict(getattr(self, "_raw", {}))

    @classmethod
    def from_name(cls, name):
        # settings_path が実在するなら呼ばれてはいけない (ADR-0063 Phase 109e).
        calls["from_name_called"] = True
        raise FileNotFoundError("from_name should not be reached: %s" % name)


def fake_ask(question_text, settings=None):
    calls["settings_marker_seen_by_ask"] = getattr(settings, "marker", None)
    session = types.SimpleNamespace(
        formatted_answer="the answer",
        answer="the answer",
        has_successful_answer=True,
        contexts=[],
        references="",
        cost=0.01,
        token_counts={},
    )
    return types.SimpleNamespace(session=session)


fake_pkg = types.ModuleType("paperqa")
fake_pkg.Settings = FakeSettings
fake_pkg.ask = fake_ask
sys.modules["paperqa"] = fake_pkg

spec = importlib.util.spec_from_file_location("paperqa_ask_direct", script_path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

sys.argv = [script_path, input_path]
calls["exit_code"] = mod.main()
print(json.dumps(calls))
"##;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(driver)
            .arg(&script_path)
            .arg(&input_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // `main()` 自身が `progress: asking: ...` を stdout に出す（実行中の 1 問につき 1 行）ので、
        // 最後の行だけが `calls` の JSON。
        let stdout = String::from_utf8_lossy(&output.stdout);
        let last_line = stdout.lines().next_back().unwrap_or("");
        let v: serde_json::Value =
            serde_json::from_str(last_line).expect("valid JSON on the last stdout line");
        assert_eq!(v["exit_code"], 0, "{v}");
        assert_eq!(
            v["from_name_called"], false,
            "settings_path が実在するので from_name は呼ばれない: {v}"
        );
        assert_eq!(v["settings_marker_seen_by_ask"], "from-disk", "{v}");
        assert_eq!(
            v["validated_raw"],
            serde_json::json!({"llm": "qwen3.8-27b", "marker": "from-disk"}),
            "{v}"
        );

        let ask_output: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&output_path).unwrap()).unwrap();
        assert!(ask_output["error"].is_null(), "{ask_output}");
        assert_eq!(ask_output["answers"][0]["answer"], "the answer", "{ask_output}");
        assert_eq!(
            ask_output["answers"][0]["has_successful_answer"],
            true,
            "{ask_output}"
        );
    }

    /// ADR-0063 Phase 109e: `settings_path` にファイルが無く、`Settings.from_name` も見つけられない
    /// ときは exit 2、`output_path` に探したパスを列挙した `error` を書く（Rust 側は
    /// `retryable: false` にする。`ask_output_signals_settings_not_found_as_a_non_retryable_error`
    /// でアダプタ側の分類も確認する）。
    #[test]
    fn paperqa_ask_main_exits_2_with_a_clear_message_when_settings_are_not_found() {
        if !python3_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("paperqa_ask.py");
        std::fs::write(&script_path, ASK_SCRIPT).unwrap();

        let missing_settings_path = dir.path().join("settings").join("celeris-proxy.json");
        let output_path = dir.path().join("ask_output.json");
        let input = serde_json::json!({
            "settings_name": "celeris-proxy",
            "settings_dir": dir.path().join("settings").to_string_lossy(),
            "settings_path": missing_settings_path.to_string_lossy(),
            "paper_directory": dir.path().join("papers").to_string_lossy(),
            "index_directory": dir.path().join("index").to_string_lossy(),
            "index_name": "proj",
            "model": null,
            "targets": [],
            "aspects": [],
            "comparison_target": null,
            "max_asks": 8,
            "fallback_question": "the question",
            "output_path": output_path.to_string_lossy(),
        });
        let input_path = dir.path().join("input.json");
        std::fs::write(&input_path, serde_json::to_string_pretty(&input).unwrap()).unwrap();

        let driver = r##"
import importlib.util, json, sys, types

script_path, input_path = sys.argv[1], sys.argv[2]


class FakeIndex:
    def __init__(self):
        self.paper_directory = None
        self.index_directory = None
        self.name = None


class FakeAgent:
    def __init__(self):
        self.index = FakeIndex()


class FakeSettings:
    def __init__(self, **kw):
        self.agent = FakeAgent()
        self.llm = None

    @classmethod
    def from_name(cls, name):
        raise FileNotFoundError(
            "No configuration file %r found at user config path "
            "/home/u/.pqa/settings/%s.json or bundled config path ..." % (name, name)
        )


def fake_ask(question_text, settings=None):  # pragma: no cover - should never run
    raise AssertionError("ask() should not be called when settings could not be resolved")


fake_pkg = types.ModuleType("paperqa")
fake_pkg.Settings = FakeSettings
fake_pkg.ask = fake_ask
sys.modules["paperqa"] = fake_pkg

spec = importlib.util.spec_from_file_location("paperqa_ask_direct", script_path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

sys.argv = [script_path, input_path]
rc = mod.main()
print(json.dumps({"exit_code": rc}))
"##;
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(driver)
            .arg(&script_path)
            .arg(&input_path)
            .output()
            .expect("failed to run python3");
        assert!(
            output.status.success(),
            "the python driver itself must not crash: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let v: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("valid JSON on stdout");
        assert_eq!(v["exit_code"], 2, "{v}");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("celeris-proxy"), "{stderr}");

        let ask_output: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&output_path).unwrap()).unwrap();
        let error = ask_output["error"].as_str().unwrap();
        assert!(error.contains("celeris-proxy"), "{error}");
        assert!(
            error.contains(&missing_settings_path.to_string_lossy().into_owned()),
            "探した settings_path を含む: {error}"
        );
        assert_eq!(ask_output["answers"], serde_json::json!([]), "{ask_output}");
    }

    /// ADR-0063 Phase 109e: `paperqa_ask.py` が exit 2（設定/使い方の誤り）で終わったとき、アダプタは
    /// `output_path` の `error` をメッセージに使い、`retryable: false` の `Terminal::Error` にする
    /// （再試行しても直らない設定の誤りのため）。
    #[tokio::test]
    async fn ask_exit_2_becomes_a_non_retryable_terminal_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"input="$2"
out=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['output_path'])" "$input")
cat > "$out" <<'JSON'
{"error": "could not resolve PaperQA settings 'celeris-proxy'; looked at: /settings/celeris-proxy.json", "answers": []}
JSON
echo "could not resolve PaperQA settings 'celeris-proxy'; looked at: /settings/celeris-proxy.json" >&2
exit 2
"#;
        let config = stub_pqa(dir.path(), script);
        let adapter = PaperQaAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-settings-missing", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { message, retryable } => {
                assert!(!retryable, "{message}");
                assert!(message.contains("celeris-proxy"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// ADR-0063 Phase 109d C4: 対象が取れているとき、`answer.md`/`report.md` に「# 対象別の整理」
    /// （表 + 対象ごとの節）と「## 引用された文献（contexts）」が出る。
    #[tokio::test]
    async fn answer_md_gets_target_sections_and_table_when_targets_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let contexts_json = r#"[{"text": {"doc": {"docname": "brinkmann2020_10-1007-s11390-020-9801-1", "dockey": "k1", "citation": "Brinkmann et al. (2020)"}}, "score": 8}]"#;
        let answer_text = "- cache 方式: node-local NVMe cache (brinkmann2020_10-1007-s11390-020-9801-1.pdf)\n- file semantics: 未確認\n";
        let config = stub_pqa_with_acquire(
            dir.path(),
            &ask_stub_script(answer_text, contexts_json),
            &acquire_stub_script(6, 3),
        );
        let adapter = PaperQaAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.task.objective = "CHFS/FINCHFS の性能比較調査（cache 方式、file semantics）".to_string();
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-targets-answer", default_limits(), &sink)
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { .. }),
            "{:?}",
            outcome.terminal
        );

        let answer_md = std::fs::read_to_string(dir.path().join("artifacts/answer.md")).unwrap();
        assert!(answer_md.starts_with("# 対象別の整理"), "{answer_md}");
        assert!(answer_md.contains("| 対象 | cache 方式 | file semantics |"), "{answer_md}");
        assert!(answer_md.contains("### CHFS"), "{answer_md}");
        assert!(answer_md.contains("### FINCHFS"), "{answer_md}");
        assert!(
            answer_md.contains("## 引用された文献（contexts）"),
            "{answer_md}"
        );
        assert!(
            answer_md.contains("brinkmann2020_10-1007-s11390-020-9801-1"),
            "{answer_md}"
        );

        let research: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("artifacts/research.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            research["targets"],
            serde_json::json!(["CHFS", "FINCHFS"]),
            "{research}"
        );
    }

    /// ADR-0063 Phase 109g A: 総括の節の見出しは、比較先が分かっていれば「## <比較先> との比較分類」、
    /// 無ければ従来どおり「## 総括」。
    #[test]
    fn render_target_sections_headings_the_summary_with_the_comparison_target_when_known() {
        let answers = vec![
            AskAnswer {
                id: "t1".into(),
                target: Some("CHFS".into()),
                answer: "- cache: node-local NVMe".into(),
                ..Default::default()
            },
            AskAnswer {
                id: "summary".into(),
                target: None,
                answer: "- CHFS: 公平比較可能（高）".into(),
                ..Default::default()
            },
        ];
        let with_target = render_target_sections(&answers, "", Some("BenchFS"));
        assert!(
            with_target.contains("## BenchFS との比較分類\n\n- CHFS: 公平比較可能（高）"),
            "{with_target}"
        );
        assert!(!with_target.contains("## 総括"), "{with_target}");

        let without_target = render_target_sections(&answers, "", None);
        assert!(without_target.contains("## 総括\n\n"), "{without_target}");
        assert!(!without_target.contains("との比較分類"), "{without_target}");
    }
}
