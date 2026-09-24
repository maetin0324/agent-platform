//! `codex` アダプタ（DESIGN §5.4, ADR-0008 D3）。
//!
//! `codex exec --json` は celeris 独自のワーカープロトコルを話さない。`--json` が吐く JSON Lines
//! （`thread.started` → `item.*`（進捗）→ `turn.completed`/`turn.failed`）を読み、`claude-code`
//! （ADR-0006）と同じ「結果ファイル規約」（`artifacts/result.json`）で `RunOutcome` を合成する。
//! プロンプト組み立ては `claude_code::build_prompt` をそのまま再利用する（ADR-0008 D3: kind 別の
//! 文面をアダプタごとに複製しない）。生存監視（wall-clock・無出力タイムアウト・SIGTERM→SIGKILL）は
//! `subprocess.rs` の低レベル部分を再利用する。

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use task_core::{RateLimitObservation, Usage};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::warn;

use crate::adapter::{AdapterError, EventSink, RunLimits, RunOutcome, Terminal, WorkerAdapter};
use crate::claude_code::{build_prompt, work_dir_note};
use crate::delegate_file::{clear_delegate_file, forward_delegate_file};
use crate::progress;
use crate::protocol::{Evidence, ProviderFailure, RunRequest};
use crate::provider::classify_provider_failure;
use crate::subprocess::{
    LineOutcome, MAX_LINE_BYTES, adopt_result_json_written_under_work_dir, kill_now,
    read_line_limited, read_tail, reap_after_terminal, write_result_json,
};

/// `[adapters.codex] resume_mode`（ADR-0054 D1。Phase 67）: このインストールの `codex` が
/// `exec resume <id>` サブコマンドを受け付けるかの**決定的な**判定。実機のバージョンを毎回 probe する
/// のではなく、設定で固定する（instructions: 「a config flag or a version probe done once, cached」の
/// うち前者。celeris はこのサンドボックスから実 CLI を起こせないため、実機で `codex exec resume --help`
/// 等を確認した上で運用側が設定すること。既定は `ExecResume`（新しめの codex-cli を想定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodexResumeMode {
    /// `codex exec resume <id> --json …`。
    #[default]
    ExecResume,
    /// `codex exec --json -c experimental_resume=<id> …`（`resume` サブコマンドの無い古い版）。
    ExperimentalResume,
}

/// `[adapters.codex] resume_bypass`（ADR-0054 Phase 112 D1）: `exec resume` の翻訳表
/// （`translate_resume_extra_args`）で `-c key=value` に翻訳できなかった operator `extra_args` が
/// 残ったときの扱い。既定は `Off`（従来どおり WARN で落とす）。`Dangerous` を明示設定した運用でだけ、
/// 落とす代わりに `--dangerously-bypass-approvals-and-sandbox` を 1 回足す（意味が広すぎるので既定にはしない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodexResumeBypass {
    #[default]
    Off,
    Dangerous,
}

/// `[adapters.codex]`（config.toml, ADR-0008 D4）。
#[derive(Debug, Clone)]
pub struct CodexConfig {
    /// 起動するコマンド名／パス。既定 `"codex"`。
    pub command: String,
    /// `exec --json` の後、プロンプトの前に追加する引数（サンドボックス／承認モードの指定など。
    /// 正確なフラグ名は Phase 0 の二次情報では確定していないため、運用側で指定する。ADR-0008 D3）。
    pub extra_args: Vec<String>,
    /// モデル指定（`--model`。省略時は codex の既定モデル）。
    pub model: Option<String>,
    /// 追加の環境変数。
    pub env: Vec<(String, String)>,
    /// ADR-0043 D3（Phase 56）: `Some` なら `codex` をコンテナの中で起こす（`container::wrap`）。
    pub container: Option<crate::container::SharedPlan>,
    /// ADR-0054 D1（Phase 67）: `context.session` が resume を求めたときの継続手段。
    pub resume_mode: CodexResumeMode,
    /// ADR-0054 Phase 112 D1: 翻訳しきれない `extra_args` の resume での扱い。
    pub resume_bypass: CodexResumeBypass,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: "codex".to_string(),
            extra_args: Vec::new(),
            model: None,
            env: Vec::new(),
            container: None,
            resume_mode: CodexResumeMode::default(),
            resume_bypass: CodexResumeBypass::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodexAdapter {
    config: CodexConfig,
}

impl CodexAdapter {
    pub const ID: &'static str = "codex";

    pub fn new(config: CodexConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WorkerAdapter for CodexAdapter {
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
        run_codex(&self.config, &req, run_id, &limits, sink).await
    }

    /// ADR-0025 D2: `extra`（`CODEX_HOME` を含む）を `config.env` の末尾に足した複製を返す
    /// （`claude_code::ClaudeCodeAdapter::with_env` と同じ規則: 同名キーは後勝ち）。
    fn with_model(&self, model: &str) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.model = Some(model.to_owned());
        Some(Arc::new(Self::new(config)))
    }
    fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.env.extend(extra.iter().cloned());
        Some(Arc::new(CodexAdapter::new(config)))
    }

    /// ADR-0043 D3（Phase 56）: コンテナの中で `codex` を起こす複製。
    fn with_container(&self, plan: crate::container::SharedPlan) -> Option<Arc<dyn WorkerAdapter>> {
        let mut config = self.config.clone();
        config.container = Some(plan);
        Some(Arc::new(CodexAdapter::new(config)))
    }
}

/// 壁時計の Unix 秒（ADR-0025 D3: 観測時刻は celeris の壁時計。`codex_account` からも使う）。
pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `artifacts/result.json`（`claude_code::ResultFile` と同じ規約。ADR-0006 D3, ADR-0008 D3）。
#[derive(Debug, Deserialize)]
struct ResultFile {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    evidence: serde_json::Value,
}

/// ADR-0061（Phase 104）: model が分かる時だけ静的単価表から USD を推定して埋める
/// （`claude_code::terminal_from_result` と同じやり方。不明なモデル・token 欠落は `None` のまま）。
fn with_estimated_cost(usage: Option<Usage>, model: Option<&str>) -> Option<Usage> {
    usage.map(|mut u| {
        if let Some(model) = model {
            u.cost_usd = task_core::estimate_cost_usd(model, &u);
        }
        u
    })
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

/// `translate_resume_extra_args` の結果（ADR-0054 Phase 112 D1）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ResumeArgTranslation {
    /// `-c key=value` として `exec resume` の argv に足す値（`value` は既に quote 済み）。
    config_overrides: Vec<String>,
    /// 翻訳できた operator の元のフラグ表記（INFO ログ用）。
    translated: Vec<String>,
    /// 翻訳できなかった operator の元のフラグ表記（WARN で落とすか、`resume_bypass` があれば
    /// `--dangerously-bypass-approvals-and-sandbox` に置き換える対象）。
    untranslatable: Vec<String>,
}

/// `[adapters.codex] extra_args` のうち `exec resume`（ADR-0054 Phase 68c のホワイトリスト）に直接は
/// 乗せられないフラグを、可能な限り意味を保った `-c key=value` に書き換える（ADR-0054 Phase 112 D1）。
/// 純粋関数（I/O・ログをしない。呼び出し側がログを出す）。
///
/// - `--approve-for-me` → `approval_policy="never"` + `sandbox_mode="workspace-write"`
/// - `--full-auto` → `approval_policy="on-failure"` + `sandbox_mode="workspace-write"`
/// - `--sandbox <MODE>` / `-s <MODE>` → `sandbox_mode="<MODE>"`
/// - `--ask-for-approval <POLICY>` / `-a <POLICY>` → `approval_policy="<POLICY>"`
/// - それ以外（値を取るはずのフラグの値欠落を含む）は `untranslatable` に残る。
fn translate_resume_extra_args(extra_args: &[String]) -> ResumeArgTranslation {
    let mut out = ResumeArgTranslation::default();
    let mut iter = extra_args.iter().peekable();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--approve-for-me" => {
                out.config_overrides
                    .push("approval_policy=\"never\"".to_string());
                out.config_overrides
                    .push("sandbox_mode=\"workspace-write\"".to_string());
                out.translated.push(flag.clone());
            }
            "--full-auto" => {
                out.config_overrides
                    .push("approval_policy=\"on-failure\"".to_string());
                out.config_overrides
                    .push("sandbox_mode=\"workspace-write\"".to_string());
                out.translated.push(flag.clone());
            }
            "--sandbox" | "-s" => match iter.next() {
                Some(mode) => {
                    out.config_overrides
                        .push(format!("sandbox_mode=\"{mode}\""));
                    out.translated.push(format!("{flag} {mode}"));
                }
                None => out.untranslatable.push(flag.clone()),
            },
            "--ask-for-approval" | "-a" => match iter.next() {
                Some(policy) => {
                    out.config_overrides
                        .push(format!("approval_policy=\"{policy}\""));
                    out.translated.push(format!("{flag} {policy}"));
                }
                None => out.untranslatable.push(flag.clone()),
            },
            other => out.untranslatable.push(other.to_string()),
        }
    }
    out
}

/// `turn.completed`/`turn.failed` の一度でも観測できた終端シグナル（ADR-0008 D3）。
#[derive(Debug, Clone)]
enum TurnSignal {
    Completed { usage: Option<Usage> },
    Failed { message: String },
}

/// 1 回の `codex exec` 起動（fresh でも resume でも）が読み取った生の観測結果。分類
/// （`Terminal`/`ProviderFailure` への変換）は呼び出し側（`run_codex`）が行う。Phase 98（ADR-0054
/// 追記）: この構造体に分けたのは、resume の RPC 失敗を**同じ run の中で** fresh 起動としてやり直す
/// （`run_codex_once` をもう一度呼ぶ）ために、読み取りループを 1 回分だけの単位に切り出す必要があった
/// ため。
struct CodexAttempt {
    exit_status: std::process::ExitStatus,
    last_signal: Option<TurnSignal>,
    last_error_message: Option<String>,
    conversation_reply: Option<String>,
    timeout_terminal: Option<Terminal>,
    /// Phase 98: stdout に JSON Lines が 1 行でも来たか（パースの成否は問わない）。「イベントを
    /// 一つも出さずに終わった」判定に使う。
    saw_any_stdout_line: bool,
}

/// `codex exec`（または `codex exec resume <id>`）を 1 回起動し、終了まで読み切る（ADR-0008 D3）。
/// `resume_id` が `Some` なら resume、`None` なら fresh（`--add-dir` 付き）。プロンプトは呼び出し側が
/// 一度だけ組んで渡す（Phase 98 のリトライでも同じ文面を使う。ADR-0054 D1 のとおり fresh 側の前置きは
/// 本来「差分でなく全量」だが、intra-run のやり直しでは前置きを組み直さない — 動くことを優先する）。
#[allow(clippy::too_many_arguments)]
async fn run_codex_once(
    config: &CodexConfig,
    req: &RunRequest,
    run_id: &str,
    limits: &RunLimits,
    sink: &dyn EventSink,
    prompt: &str,
    resume_id: Option<&str>,
    stdout_log_path: &std::path::Path,
    stderr_log_path: &std::path::Path,
) -> Result<CodexAttempt, AdapterError> {
    // `stderr_task` (below) moves a copy into its `async move` block; this one stays available for
    // the crash-classification read after the loop (ADR-0010 D5).
    let stderr_log_path_for_task = stderr_log_path.to_path_buf();
    let resume_id = resume_id.map(|s| s.to_string());

    let mut command = Command::new(&config.command);
    // CoS and standalone task workspaces need not be Git repositories.
    command.arg("exec");
    // ADR-0054 Phase 68b/68c: `codex exec resume` is a distinct clap subcommand from `codex exec`,
    // and does not accept every flag the parent does — production hit this twice:
    //   Phase 68b (2026-09-21 15:04 UTC, release b24bae9a796a): `--add-dir` rejected on resume.
    //   Phase 68c (2026-09-21 15:44 UTC, release a2942d5d8a94, this worktree's 68b already live):
    //     `--approve-for-me` (from `[adapters.codex] extra_args`) rejected on resume too, with a
    //     *narrower* usage line than `exec resume --help`'s OPTIONS list actually documents:
    //       error: unexpected argument '--approve-for-me' found
    //       Usage: codex exec resume --json --skip-git-repo-check --config <key=value> <SESSION_ID> [PROMPT]
    //     `~/.local/bin/codex exec resume --help` (codex-cli 0.155.1, re-checked for Phase 68c) still
    //     lists a broader OPTIONS set (-c/--config, --last, --all, --enable, --disable, -i/--image,
    //     --strict-config, -m/--model, --dangerously-bypass-approvals-and-sandbox,
    //     --dangerously-bypass-hook-trust, --worktree, --thread-source, --skip-git-repo-check,
    //     --ephemeral, --ignore-user-config, --ignore-rules, --output-schema, --json,
    //     -o/--output-last-message, -h/--help — no `--add-dir`, no `-s/--sandbox`, no
    //     `--approve-for-me`), same as it did for Phase 68b. Since the deployed binary's actual
    //     accepted set is narrower than what `--help` documents (and we cannot exec `codex exec
    //     resume` itself to verify further — only `--help` is allowed), we now treat resume as
    //     **whitelist-based** rather than "subtract the flags we know are missing": for the `resume`
    //     subcommand form we emit only `--json`, `--skip-git-repo-check`, and any number of
    //     `-c/--config <key=value>` pairs, plus the session id and prompt positionals — i.e. exactly
    //     the flags the production usage line above shows. Everything else the fresh form uses
    //     (`--add-dir`, `--model`, operator `extra_args` such as `--approve-for-me`/`--sandbox`) is
    //     either routed through an equivalent `-c key=value` (only `--model` has a documented one:
    //     `codex exec --help`'s own example is `-c model="o3"`) or dropped for resume with a comment
    //     below explaining why (the resumed thread already carries whatever approval/sandbox/
    //     writable-roots settings were set on the *first*, non-resume `exec` invocation that created
    //     it — unverified against the real CLI beyond `--help`, see PROGRESS.md Phase 68c 未解決事項).
    let mut is_exec_resume_subcommand = false;
    if let (Some(id), CodexResumeMode::ExecResume) = (&resume_id, config.resume_mode) {
        command.arg("resume").arg(id);
        is_exec_resume_subcommand = true;
    }
    // ADR-0054 D2（Phase 68）: CoS の対話 run だけ read-only sandbox（読み取りの道具の代わり。codex には
    // claude-code の `--allowedTools` に相当する道具単位の許可リストが無いため、書き込みそのものを
    // 塞ぐ）。`--add-dir` した `artifacts_dir`（下。fresh run のみ。Phase 68b/68c 参照）は read-only でも
    // 書ける（celeris が渡す「結果ファイルを書く場所」の明示的な例外。codex の writable_roots の扱いに依る）。
    // それ以外の run は従来どおり `workspace-write`（result.json の契約に書き込みが要る）。
    let sandbox_mode = if req.context.conversation_addressee
        == Some(crate::protocol::ConversationAddressee::Secretary)
    {
        "read-only"
    } else {
        "workspace-write"
    };
    // `--json` / `--skip-git-repo-check` / `-c key=value` are on the `exec resume` whitelist above,
    // so this trio is identical for fresh and resume runs.
    command
        .arg("--json")
        .arg("--skip-git-repo-check")
        // The result.json contract needs writes; explicit extra_args override this default.
        .arg("-c")
        .arg(format!("sandbox_mode=\"{sandbox_mode}\""));
    if let (Some(id), CodexResumeMode::ExperimentalResume) = (&resume_id, config.resume_mode) {
        command.arg("-c").arg(format!("experimental_resume={id}"));
    }
    if let Some(model) = &config.model {
        if is_exec_resume_subcommand {
            // `--model` isn't on the Phase 68c whitelist; `-c model="..."` is the documented
            // equivalent (`codex exec --help`'s own `-c model="o3"` example).
            command.arg("-c").arg(format!("model=\"{model}\""));
        } else {
            command.arg("--model").arg(model);
        }
    }
    // Worktrees and shared workspaces keep results outside cwd. Grant only the
    // dispatcher-selected artifact directory, not its parent or other tasks — but only on the forms
    // of `codex exec` that accept `--add-dir` (see the usage-line comment above; `exec resume` does
    // not). A resumed thread already carries the writable-roots grant it received on the *first*
    // (non-resume) `exec` invocation that created it, since that one goes through plain `codex exec`
    // and does pass `--add-dir`; `-c experimental_resume=<id>` runs also go through plain `codex exec`
    // and keep getting `--add-dir` here.
    tokio::fs::create_dir_all(&req.artifacts_dir).await?;
    if is_exec_resume_subcommand {
        // ADR-0054 Phase 112 D1: operator-configured `extra_args` are arbitrary strings celeris does
        // not validate, and Phase 68c's production incident shows `exec resume` rejects at least one
        // of them (`--approve-for-me`) outright. Rather than dropping every flag wholesale (Phase
        // 68c), translate the ones with a documented `-c key=value` equivalent and only drop what's
        // left (see `translate_resume_extra_args` for the table and reasoning).
        let translation = translate_resume_extra_args(&config.extra_args);
        for pair in &translation.config_overrides {
            command.arg("-c").arg(pair);
        }
        if !translation.translated.is_empty() {
            tracing::info!(
                "run {run_id}: translated codex extra_args for `exec resume` (ADR-0054 Phase 112 \
                 D1): {:?} -> {:?}",
                translation.translated,
                translation.config_overrides
            );
        }
        if !translation.untranslatable.is_empty() {
            if config.resume_bypass == CodexResumeBypass::Dangerous {
                command.arg("--dangerously-bypass-approvals-and-sandbox");
                tracing::info!(
                    "run {run_id}: using --dangerously-bypass-approvals-and-sandbox on `exec \
                     resume` for extra_args with no -c translation (resume_bypass = \"dangerous\"; \
                     ADR-0054 Phase 112 D1): {:?}",
                    translation.untranslatable
                );
            } else {
                warn!(
                    "run {run_id}: dropping codex extra_args on `exec resume` (no -c translation \
                     known and resume_bypass is not configured; ADR-0054 Phase 112 D1): {:?}",
                    translation.untranslatable
                );
            }
        }
    } else {
        command.arg("--add-dir").arg(&req.artifacts_dir);
        command.args(&config.extra_args);
    }
    command.arg(prompt);
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

    let mut stdout_file = tokio::fs::File::create(stdout_log_path).await?;
    let mut reader = BufReader::new(stdout);

    let start = Instant::now();
    let mut last_activity = Instant::now();
    let mut last_signal: Option<TurnSignal> = None;
    let mut conversation_reply: Option<String> = None;
    // `{"type":"error","message":...}` を観測したら保持する（ADR-0010 D5: `turn.*` を一度も観測できずに
    // exit した場合の分類材料に使う）。
    let mut last_error_message: Option<String> = None;
    let mut force_kill = false;
    let mut timeout_terminal: Option<Terminal> = None;
    // Phase 98: 少なくとも 1 行の stdout（イベント）を見たか。
    let mut saw_any_stdout_line = false;

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
                saw_any_stdout_line = true;
                warn!("run {run_id}: discarding overlong line from codex stdout");
            }
            LineOutcome::Line(bytes) => {
                sink.heartbeat();
                last_activity = Instant::now();
                saw_any_stdout_line = true;
                stdout_file.write_all(&bytes).await?;
                stdout_file.write_all(b"\n").await?;
                let text = String::from_utf8_lossy(&bytes);
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        if event["type"] == "item.completed"
                            && event["item"]["type"] == "agent_message"
                            && event["item"]["phase"] != "commentary"
                        {
                            conversation_reply = event["item"]["text"]
                                .as_str()
                                .filter(|text| !text.trim().is_empty())
                                .map(str::to_owned);
                        } else if event["type"] == "item.started" {
                            conversation_reply = None;
                        }
                    }
                    handle_line(trimmed, sink, &mut last_signal, &mut last_error_message);
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

    Ok(CodexAttempt {
        exit_status,
        last_signal,
        last_error_message,
        conversation_reply,
        timeout_terminal,
        saw_any_stdout_line,
    })
}

async fn run_codex(
    config: &CodexConfig,
    req: &RunRequest,
    run_id: &str,
    limits: &RunLimits,
    sink: &dyn EventSink,
) -> Result<RunOutcome, AdapterError> {
    let run_dir = req.workspace.join("runs").join(run_id);
    tokio::fs::create_dir_all(&run_dir).await?;

    // 前回の run（リトライ）が残した結果ファイルを、今回の run の結果と誤読しない（ADR-0006 D3 と同じ理由）。
    // ADR-0036 D1/D2: 置き場はディスパッチャが決めた `artifacts_dir`（共有 workspace ではタスクごと）。
    let artifacts_rel = req.artifacts_rel();
    let result_path = req.artifact_path("result.json");
    let _ = tokio::fs::remove_file(&result_path).await;
    clear_delegate_file(&req.artifacts_dir).await;
    // ADR-0006 Phase 115 D2: 同じ理由で、work_dir 側の名残候補も消す（`work_dir != workspace` の
    // ときだけ意味がある。無ければ何もしない）。
    if let Some(work_dir) = req.work_dir.as_deref()
        && work_dir != req.workspace
    {
        let _ = tokio::fs::remove_file(work_dir.join("artifacts").join("result.json")).await;
    }

    let prompt = format!(
        "{}{}",
        work_dir_note(req.work_dir.as_deref(), &req.workspace, &req.artifacts_dir),
        build_prompt(&req.task, &req.context, run_id, &artifacts_rel)
    );
    // ADR-0023 D2 / M1: この run で何を渡したかを残す（`request.json` は構造、`prompt.txt` は実際の文面）。
    crate::subprocess::write_run_request(&run_dir, req, run_id).await;
    crate::subprocess::write_run_prompt(&run_dir, &prompt, run_id).await;
    // ADR-0056 D3（Phase 79）: mount された skills を `AGENTS.md` の節として書く（codex はこのファイルを
    // 自動で読む。既存の内容は壊さない。run は落とさない）。
    if let Err(e) = crate::skills::deliver_agents_md(req.cwd(), &req.context.skills).await {
        warn!("run {run_id}: failed to update AGENTS.md with skills: {e}");
    }

    // ADR-0054 D1（Phase 67）: `context.session` がこのアダプタ宛て（`adapter == "codex"`）で
    // `resume: true` のときだけ継続する。手段は `config.resume_mode` で決定的に選ぶ（実機 probe はしない）。
    let codex_session = req
        .context
        .session
        .as_ref()
        .filter(|s| s.adapter == CodexAdapter::ID);
    let resume_id = codex_session
        .filter(|s| s.resume)
        .map(|s| s.session_id.clone());
    // ADR-0054 Phase 112 D2: if this would go through the `exec resume` whitelist (Phase 68c) and
    // the operator's `extra_args` have flags D1 can't translate to `-c key=value` (and no
    // `resume_bypass` is configured to cover them), don't even attempt resume — a resumed thread
    // that silently loses its approval/sandbox settings can end up permanently read-only. Give up on
    // resume and run fresh instead (fresh gets the full, untranslated `extra_args`; D1 is preferred
    // whenever its translation is complete).
    let resume_id = if resume_id.is_some() && config.resume_mode == CodexResumeMode::ExecResume {
        let translation = translate_resume_extra_args(&config.extra_args);
        if !translation.untranslatable.is_empty()
            && config.resume_bypass != CodexResumeBypass::Dangerous
        {
            warn!(
                "run {run_id}: skipping `exec resume` and starting a fresh codex session instead \
                 (extra_args {:?} have no -c translation for `exec resume` and resume_bypass is \
                 not configured; ADR-0054 Phase 112 D2)",
                translation.untranslatable
            );
            None
        } else {
            resume_id
        }
    } else {
        resume_id
    };

    let stdout_log_path = run_dir.join("stdout.jsonl");
    let mut stderr_log_path = run_dir.join("stderr.log");
    let mut attempt = run_codex_once(
        config,
        req,
        run_id,
        limits,
        sink,
        &prompt,
        resume_id.as_deref(),
        &stdout_log_path,
        &stderr_log_path,
    )
    .await?;

    // Phase 98（ADR-0054 追記。実機障害 2026-09-22 00:18 UTC、task 01M337NT3QT1FR1G6WHS9G6NDA）:
    // `codex exec resume` が「イベントを一つも出さずに」非 0 で終わり、stderr に
    // `thread/resume`/`-32601`/`resume` を含む失敗行があるなら、これは通常の resume 拒否
    // （`looks_like_resume_rejection` が既に検出する `session not found` 等）ではなく、codex-cli 自身が
    // `exec resume` の JSON-RPC を実装していない（`list_turns is not supported yet`）という、**同じ
    // インストールでは毎回起きる**壊れ方。次 run を待たずに、**この run の中で** fresh セッション
    // （resume 無しの通常の `codex exec`）として 1 回だけやり直す（claude-code 側の Phase 67b の
    // 自己修復と同じ「壊れたら retire して仕切り直す」考え方を、run をまたがずに行うもの）。
    let mut resume_id_for_classification = resume_id.clone();
    if resume_id.is_some()
        && !attempt.saw_any_stdout_line
        && attempt.timeout_terminal.is_none()
        && attempt.last_signal.is_none()
        && !attempt.exit_status.success()
    {
        let tail = read_tail(&stderr_log_path, 4096).await;
        let combined = format!(
            "{} {tail}",
            attempt.last_error_message.as_deref().unwrap_or("")
        );
        if crate::provider::looks_like_resume_rpc_failure(&combined) {
            sink.session_resume_failed(&combined);
            let retry_stdout_log_path = run_dir.join("stdout.resume_retry.jsonl");
            let retry_stderr_log_path = run_dir.join("stderr.resume_retry.log");
            let _ = tokio::fs::remove_file(&result_path).await;
            clear_delegate_file(&req.artifacts_dir).await;
            attempt = run_codex_once(
                config,
                req,
                run_id,
                limits,
                sink,
                &prompt,
                None,
                &retry_stdout_log_path,
                &retry_stderr_log_path,
            )
            .await?;
            stderr_log_path = retry_stderr_log_path;
            resume_id_for_classification = None;
        }
    }

    let CodexAttempt {
        exit_status,
        last_signal,
        last_error_message,
        conversation_reply,
        timeout_terminal,
        saw_any_stdout_line: _,
    } = attempt;
    let resume_id = resume_id_for_classification;

    let (terminal, provider_failure): (Terminal, Option<ProviderFailure>) = match (
        timeout_terminal,
        &last_signal,
    ) {
        // タイムアウト（wall-clock / idle）は分類しない（ADR-0010 D5）。
        (Some(t), _) => (t, None),
        // `turn.completed`/`turn.failed` を一度も観測できずに exit した場合はクラッシュとして扱い、
        // artifacts/result.json を一切信用しない（ADR-0006 D4 と同じ理由。ADR-0008 D3）。
        // 観測できた `{"type":"error",...}` のメッセージ、無ければ stderr.log の末尾を分類する（ADR-0010 D5）。
        (None, None) => {
            let exit_repr = match exit_status.code() {
                Some(code) => code.to_string(),
                None => "signal".to_string(),
            };
            let mut pf = last_error_message
                .as_deref()
                .and_then(classify_provider_failure);
            let tail = read_tail(&stderr_log_path, 4096).await;
            if pf.is_none() {
                pf = classify_provider_failure(&tail);
            }
            // ADR-0054 D1（Phase 67）: resume を頼んだ run が、セッションを拒否されたように見える crash
            // なら報告する。文言は実機で確認していない（`provider::looks_like_resume_rejection` 参照）。
            if resume_id.is_some() {
                let combined = format!("{} {tail}", last_error_message.as_deref().unwrap_or(""));
                if crate::provider::looks_like_resume_rejection(&combined) {
                    sink.session_resume_failed(&combined);
                }
            }
            (
                Terminal::Error {
                    message: format!(
                        "worker exited without a turn.completed/turn.failed message (exit={exit_repr})"
                    ),
                    retryable: true,
                },
                pf,
            )
        }
        (None, Some(TurnSignal::Failed { message })) => {
            let pf = classify_provider_failure(message);
            if resume_id.is_some() && crate::provider::looks_like_resume_rejection(message) {
                sink.session_resume_failed(message);
            }
            (
                Terminal::Error {
                    message: format!("codex turn failed: {message}"),
                    retryable: true,
                },
                pf,
            )
        }
        (None, Some(TurnSignal::Completed { usage })) => {
            // ADR-0006 Phase 115 D2: 結果ファイルを読む（`result_path` の有無を見る）前に、work_dir 側の
            // 名残を採用する。Phase 112 D3 の「最終メッセージから回収」より必ず先（`result_path` の
            // `NotFound` 判定がこの後にあるため、先に採用しておかないと本物の結果を無視して最終メッセージ
            // から再構成してしまう）。
            adopt_result_json_written_under_work_dir(
                &req.artifacts_dir,
                req.work_dir.as_deref(),
                &req.workspace,
                run_id,
            )
            .await;
            // A conversation can answer directly; work orders still require their artifacts.
            let terminal = if exit_status.success()
                && req.task.conversation.is_some()
                && req.task.kind == task_core::TaskKind::Execute
                && matches!(tokio::fs::metadata(&result_path).await,
                    Err(ref error) if error.kind() == std::io::ErrorKind::NotFound)
                && let Some(summary) = conversation_reply
            {
                // ADR-0054 Phase 112 D3: the model may have meant `summary` to be the whole
                // `result.json` body (e.g. it could not write into `artifacts_dir`, as happens when
                // `exec resume` drops the `--add-dir` grant it would otherwise need). If it parses
                // as that shape (a non-empty `summary` string and an `actions` array), recover it:
                // write it to `result.json` as if the model had written it itself, and re-derive the
                // terminal from disk through the normal path (`terminal_from_result`) so
                // `actions`/`memory`/`milestone_proposal` all pick it up via the existing,
                // disk-reading machinery instead of the raw JSON text being treated as inert prose.
                if crate::result_report::final_message_is_recoverable_result(&summary) {
                    match tokio::fs::write(&result_path, &summary).await {
                        Ok(()) => {
                            crate::progress::emit_status(
                                sink,
                                "result recovered from the final message (result.json was \
                                 missing; ADR-0054 Phase 112 D3)",
                            );
                            terminal_from_result(
                                &req.artifacts_dir,
                                &artifacts_rel,
                                *usage,
                                config.model.as_deref(),
                            )
                            .await
                        }
                        Err(e) => {
                            warn!(
                                "run {run_id}: failed to write the result recovered from the \
                                 final message: {e}"
                            );
                            Terminal::Done {
                                summary,
                                evidence: Vec::new(),
                                usage: with_estimated_cost(*usage, config.model.as_deref()),
                            }
                        }
                    }
                } else {
                    Terminal::Done {
                        summary,
                        evidence: Vec::new(),
                        usage: with_estimated_cost(*usage, config.model.as_deref()),
                    }
                }
            } else {
                terminal_from_result(
                    &req.artifacts_dir,
                    &artifacts_rel,
                    *usage,
                    config.model.as_deref(),
                )
                .await
            };
            (terminal, None)
        }
    };

    forward_delegate_file(&req.artifacts_dir, sink).await;

    write_result_json(&run_dir, &terminal, provider_failure).await?;

    if let (Terminal::Error { message, .. }, Some(pf)) = (&terminal, provider_failure) {
        return Err(AdapterError::from_provider_failure(pf, message));
    }

    Ok(RunOutcome {
        terminal,
        exit_code: exit_status.code(),
    })
}

/// codex の JSON Lines の 1 行を解釈する。既知でない `type` や JSON として不正な行は無視する
/// （`claude_code::handle_line` と同じ方針。ADR-0008 D3）。
fn handle_line(
    line: &str,
    sink: &dyn EventSink,
    last_signal: &mut Option<TurnSignal>,
    last_error_message: &mut Option<String>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    let Some(ty) = value.get("type").and_then(|t| t.as_str()) else {
        return;
    };
    if ty == "token_count"
        && let Some(obs) = RateLimitObservation::from_codex_token_count(&value, now_unix_secs())
    {
        sink.rate_limit(obs);
    }
    if ty.starts_with("item.") {
        // ADR-0048 D2（Phase 60a）: codex の `item.*` を正規化する。`msg` は従来どおり行そのもの
        // （500 バイトで切る）で、構造化フィールドを**足すだけ**。
        let fields = item_progress(ty, value.get("item"));
        sink.progress_with(&truncate(line, 500), &fields);
        return;
    }
    match ty {
        // ADR-0054 D1（Phase 67）/ Phase 67c 追記: 新規セッション（`context.session.resume == false`）で
        // codex 自身が割り当てた thread/session id を報告する。**フィールド名は実機で確認していない**
        // （`thread.started` イベント自体は ADR-0008 の実機確認で観測済みだが、その本文の形は未確認）。
        // `codex exec resume <id>` は celeris が作った id ではなく codex 自身が報告した id しか受け付け
        // ないので、ここで読み損なうと（フィールド名が実機と違う等）その run は id を持たないまま
        // 終わる。celeris 側は空文字を「まだ確定していない」として扱い（
        // `crate::sessions::session_id_is_valid_for_adapter`。Phase 67c）、次の run を resume させずに
        // 新規セッションとして仕切り直すので、誤って読めなくても run 自体は失敗しない。`thread_id` /
        // `threadId` / `session_id`（`session_configured` 系の実装がこの名前を使うことがある）の
        // どれかで受け取る。
        "thread.started" | "session_configured" => {
            if let Some(id) = value
                .get("thread_id")
                .or_else(|| value.get("threadId"))
                .or_else(|| value.get("session_id"))
                .and_then(|v| v.as_str())
            {
                sink.session_established(id);
            }
        }
        "turn.completed" => {
            // ADR-0061（Phase 104）: cache tokens のフィールド名は実機で確認していない
            // （OpenAI Responses API の `input_tokens_details.cached_tokens` を想定した best-effort。
            // 無ければ `None` のまま。`cost_usd` はここでは埋めない。model 文字列は呼び出し元でしか
            // 分からない — `claude_code::terminal_from_result` と同じ構造。ADR-0008 D3）。
            let usage = value.get("usage").map(|u| Usage {
                input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()),
                output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()),
                cache_read_tokens: u
                    .pointer("/input_tokens_details/cached_tokens")
                    .and_then(|v| v.as_u64()),
                cache_creation_tokens: None,
                cost_usd: None,
            });
            *last_signal = Some(TurnSignal::Completed { usage });
        }
        "error" => {
            if let Some(m) = value.get("message").and_then(|m| m.as_str()) {
                *last_error_message = Some(m.to_string());
            }
        }
        "turn.failed" => {
            // 実機（codex-cli 0.154.0）では `error` はオブジェクト（`{"message":"..."}`）で返る。
            // 将来のバージョンで文字列に変わっても読めるよう両方を受け付ける。
            let message = value
                .get("error")
                .map(describe_error)
                .unwrap_or_else(|| "turn.failed".to_string());
            *last_signal = Some(TurnSignal::Failed { message });
        }
        _ => {}
    }
}

/// ADR-0048 D2（Phase 60a）: codex の `item.*` イベント → 正規化した進行（ここだけがアダプタ固有）。
///
/// - 道具（`command_execution` / `mcp_tool_call` / `web_search` / `file_change` / `patch_apply`）は
///   `item.started` / `item.updated` が `tool_use`、`item.completed` が `tool_result`
///   （`exit_code != 0` は `error`）。
/// - `agent_message` は `text`、`reasoning` は `thinking`（要約だけ）。
/// - それ以外（`todo_list` や知らない item）は `status`（節目）。
fn item_progress(ty: &str, item: Option<&serde_json::Value>) -> task_core::ProgressFields {
    let Some(item) = item else {
        return progress::status();
    };
    // 実機（codex-cli 0.154）は `type`、古い版・別実装は `item_type` を使う。
    let item_type = item
        .get("type")
        .or_else(|| item.get("item_type"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    let completed = ty == "item.completed";
    match item_type {
        "agent_message" => {
            let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
            progress::text(&progress::one_line(text))
        }
        "reasoning" => {
            let text = item
                .get("summary")
                .or_else(|| item.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            progress::thinking(&progress::one_line(text))
        }
        "command_execution" | "mcp_tool_call" | "web_search" | "file_change" | "patch_apply" => {
            if completed {
                let body = item
                    .get("aggregated_output")
                    .or_else(|| item.get("output"))
                    .and_then(|o| o.as_str())
                    .unwrap_or("");
                let error = item
                    .get("exit_code")
                    .and_then(|c| c.as_i64())
                    .is_some_and(|c| c != 0)
                    || item.get("status").and_then(|s| s.as_str()) == Some("failed");
                progress::tool_result(Some(item_type), body, error)
            } else {
                let summary = item
                    .get("command")
                    .or_else(|| item.get("query"))
                    .or_else(|| item.get("path"))
                    .or_else(|| item.get("tool"))
                    .and_then(|v| v.as_str())
                    .map(progress::one_line)
                    .unwrap_or_else(|| progress::one_line(&item.to_string()));
                task_core::ProgressFields::of(task_core::ProgressKind::ToolUse)
                    .with_tool(item_type)
                    .with_summary(progress::truncate_chars(
                        &summary,
                        progress::SUMMARY_MAX_CHARS,
                    ))
                    .with_detail(item.to_string())
            }
        }
        // 知らない item は節目として残す（Console は折り畳んだ見出しに最後の `status` を出す）。
        other => {
            let summary = if other.is_empty() {
                ty.to_string()
            } else {
                format!("{ty} {other}")
            };
            progress::status().with_summary(summary)
        }
    }
}

/// `turn.failed` の `error` フィールドから人間向けの文字列を作る（文字列でもオブジェクトでも読める）。
fn describe_error(value: &serde_json::Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(msg) = value.get("message").and_then(|m| m.as_str()) {
        return msg.to_string();
    }
    value.to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// `turn.completed` と結果ファイルから終端を合成する。呼び出し元は `turn.completed` を一度でも
/// 観測できた場合にのみこれを呼ぶ（`claude_code::terminal_from_result` と同じ構造。ADR-0008 D3）。
async fn terminal_from_result(
    artifacts_dir: &std::path::Path,
    artifacts_rel: &str,
    usage: Option<Usage>,
    model: Option<&str>,
) -> Terminal {
    let usage = with_estimated_cost(usage, model);
    let result_path = artifacts_dir.join("result.json");
    let text = match tokio::fs::read_to_string(&result_path).await {
        Ok(t) => t,
        Err(_) => {
            return Terminal::Error {
                message: format!("codex exited without {artifacts_rel}/result.json"),
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
        /// ADR-0048 D2（Phase 60a）: 構造化した進行（`msg` と一緒に）。
        structured: Mutex<Vec<(String, task_core::ProgressFields)>>,
        delegated: Mutex<Vec<Vec<DelegateTask>>>,
        rate_limits: Mutex<Vec<task_core::RateLimitObservation>>,
        /// ADR-0054 D1（Phase 67）: `session_established` の呼び出し。
        sessions: Mutex<Vec<String>>,
        /// ADR-0054 D1（Phase 67）: `session_resume_failed` の呼び出し。
        resume_failed: Mutex<Vec<String>>,
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
        fn artifact(&self, _artifact: &ArtifactRef) {}
        fn delegate(&self, tasks: &[DelegateTask]) {
            self.delegated
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(tasks.to_vec());
        }
        fn rate_limit(&self, obs: task_core::RateLimitObservation) {
            self.rate_limits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(obs);
        }
        fn session_established(&self, session_id: &str) {
            self.sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(session_id.to_string());
        }
        fn session_resume_failed(&self, reason: &str) {
            self.resume_failed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(reason.to_string());
        }
    }

    fn stub_codex(dir: &std::path::Path, script: &str) -> CodexConfig {
        let path = dir.join("codex_stub.sh");
        // ETXTBSY 対策（ADR-0010 D10）: テストプロセス自身が書き込み fd を持たないよう別プロセスで書く。
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{script}\n"));
        CodexConfig {
            command: path.to_string_lossy().into_owned(),
            ..CodexConfig::default()
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

    /// ADR-0048 D2（Phase 60a）: codex の `item.*` の標本（`tests/fixtures/codex-stream.jsonl`）を
    /// `handle_line` に通し、`tool_use` / `tool_result` / `text` / `thinking`、それ以外は `status` に
    /// なることを確かめる。`msg` は従来どおり行そのもの（500 バイトで切る）。
    #[test]
    fn json_events_map_to_structured_progress() {
        use task_core::ProgressKind;

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/codex-stream.jsonl"
        );
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let sink = RecordingSink::default();
        let (mut signal, mut error) = (None, None);
        for line in text.lines() {
            handle_line(line, &sink, &mut signal, &mut error);
        }
        let items = sink
            .structured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let kinds: Vec<Option<ProgressKind>> = items.iter().map(|(_, f)| f.kind).collect();
        assert_eq!(
            kinds,
            vec![
                Some(ProgressKind::Thinking),
                Some(ProgressKind::ToolUse),
                Some(ProgressKind::ToolResult),
                Some(ProgressKind::ToolResult),
                Some(ProgressKind::Text),
                Some(ProgressKind::Status),
            ],
            "{items:#?}"
        );
        assert_eq!(
            items[0].1.summary.as_deref(),
            Some("テストを回して確かめる")
        );
        assert_eq!(items[1].1.tool.as_deref(), Some("command_execution"));
        assert_eq!(
            items[1].1.summary.as_deref(),
            Some("cargo test --workspace")
        );
        assert_eq!(
            items[2].1.summary.as_deref(),
            Some("test result: ok. 812 passed")
        );
        assert!(!items[2].1.error);
        // `exit_code != 0` は失敗の印。
        assert!(items[3].1.error, "{:?}", items[3]);
        assert_eq!(items[4].1.summary.as_deref(), Some("テストは通りました。"));
        // 知らない item（`todo_list`）は節目として残る。
        assert_eq!(
            items[5].1.summary.as_deref(),
            Some("item.completed todo_list")
        );
        // `msg` は従来どおり行そのもの。
        assert!(items[1].0.contains("command_execution"), "{}", items[1].0);
        assert!(matches!(signal, Some(TurnSignal::Completed { .. })));
        assert!(error.is_none());
    }

    #[tokio::test]
    async fn happy_path_progress_and_done_from_result_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
echo '{"type":"thread.started"}'
echo '{"type":"item.started","item":{"type":"command_execution","command":"cargo test"}}'
printf '%s' '{"summary":"added usage example","evidence":[]}' > artifacts/result.json
echo '{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":20}}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-1", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done {
                summary,
                evidence,
                usage,
            } => {
                assert_eq!(summary, "added usage example");
                assert!(evidence.is_empty());
                assert_eq!(
                    usage,
                    Some(Usage {
                        input_tokens: Some(10),
                        output_tokens: Some(20),
                        cache_read_tokens: None,
                        cache_creation_tokens: None,
                        cost_usd: None,
                    })
                );
            }
            other => panic!("expected done, got {other:?}"),
        }
        let progress = sink.progress.lock().unwrap();
        assert!(progress.iter().any(|m| m.contains("command_execution")));
        assert!(dir.path().join("runs/run-1/stdout.jsonl").is_file());

        // P-26 (ADR-0010 D10): the terminal is also normalized into `runs/<run_id>/result.json`,
        // readable by task-dispatch as a `WorkerMessage::Done`.
        let result_json =
            std::fs::read_to_string(dir.path().join("runs/run-1/result.json")).unwrap();
        match serde_json::from_str::<crate::protocol::WorkerMessage>(result_json.trim()).unwrap() {
            crate::protocol::WorkerMessage::Done { summary, .. } => {
                assert_eq!(summary, "added usage example")
            }
            other => panic!("expected done in result.json, got {other:?}"),
        }
    }

    /// ADR-0056 D3（Phase 79）: `context.skills` に乗った skill は、run 開始時に `AGENTS.md` の
    /// `<!-- celeris:skills:start -->` 〜 `end` の節として作業場所に書かれる。既存の内容は保つ。
    #[tokio::test]
    async fn mounted_skills_are_written_into_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# Notes\n\nBuild with cargo.\n").unwrap();
        let kb = tempfile::tempdir().unwrap();
        let skill_dir = kb.path().join("rust-review");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: rust-review\ndescription: d\n---\n\nレビューの手順\n",
        )
        .unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.skills = vec![crate::protocol::SkillMount {
            name: "rust-review".into(),
            path: skill_dir.display().to_string(),
            description: "d".into(),
        }];
        let sink = RecordingSink::default();
        adapter
            .run(req, "run-skills", default_limits(), &sink)
            .await
            .unwrap();
        let agents_md = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(agents_md.contains("# Notes"), "{agents_md}");
        assert!(agents_md.contains("Build with cargo."), "{agents_md}");
        assert!(agents_md.contains("## Skills（celeris）"), "{agents_md}");
        assert!(agents_md.contains("### rust-review"), "{agents_md}");
        assert!(agents_md.contains("レビューの手順"), "{agents_md}");
    }

    #[tokio::test]
    async fn direct_reply_is_only_accepted_for_successful_conversations() {
        for (conversation, ending, file, done) in [
            (true, "echo '{\"type\":\"turn.completed\"}'", "", true),
            (false, "echo '{\"type\":\"turn.completed\"}'", "", false),
            (
                true,
                "echo '{\"type\":\"turn.failed\",\"error\":\"failed\"}'",
                "",
                false,
            ),
            (
                true,
                "echo '{\"type\":\"turn.completed\"}'; exit 1",
                "",
                false,
            ),
            (
                true,
                "echo '{\"type\":\"turn.completed\"}'",
                "mkdir -p artifacts; echo invalid > artifacts/result.json",
                false,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let config = stub_codex(
                dir.path(),
                &format!(
                    "{file}\necho '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"接続確認OK\"}}}}'\n{ending}"
                ),
            );
            let mut req = sample_req(dir.path().to_path_buf());
            if conversation {
                req.task.conversation = Some(task_core::MessageId::new());
            }
            let outcome = CodexAdapter::new(config)
                .run(req, "reply", default_limits(), &RecordingSink::default())
                .await
                .unwrap();
            assert_eq!(
                matches!(outcome.terminal, Terminal::Done { .. }),
                done,
                "{:?}",
                outcome.terminal
            );
        }
    }

    #[tokio::test]
    async fn worktree_can_write_results_outside_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let work_dir = dir.path().join("repos/code");
        std::fs::create_dir_all(&work_dir).unwrap();
        let config = stub_codex(
            dir.path(),
            r#"
artifact_root=''
while [ "$#" -gt 0 ]; do
    if [ "$1" = '--add-dir' ]; then shift; artifact_root="$1"; fi
    shift
done
[ -n "$artifact_root" ] && [ -d "$artifact_root" ] || exit 10
[ "$PWD" != "$artifact_root" ] || exit 11
printf '%s' '{"summary":"worktree result saved","evidence":[]}' > "$artifact_root/result.json"
echo '{"type":"turn.completed"}'
"#,
        );
        let mut req = sample_req(dir.path().to_path_buf());
        req.work_dir = Some(work_dir.clone());
        // Covers shared task-specific artifact directories as well.
        req.artifacts_dir = dir.path().join(".taskd/artifacts/task-a");
        let result_path = req.artifact_path("result.json");
        let outcome = CodexAdapter::new(config)
            .run(
                req,
                "external-artifacts",
                default_limits(),
                &RecordingSink::default(),
            )
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { summary, .. } if summary == "worktree result saved")
        );
        assert!(result_path.is_file());
        assert!(!work_dir.join("artifacts/result.json").exists());
    }

    /// ADR-0006 Phase 115 D1（本番障害 01M3915FARENW8M0JM11XVF6W0）: `work_dir != workspace` の run
    /// では、プロンプト冒頭に cwd と成果物ディレクトリの絶対パスの注意が出る（`claude_code::build_prompt`
    /// を再利用する `codex` アダプタでも同じ。D4(a)）。
    #[tokio::test]
    async fn work_dir_note_appears_in_the_prompt_when_work_dir_differs_from_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let work_dir = dir.path().join("repos/agent-platform");
        std::fs::create_dir_all(&work_dir).unwrap();
        let config = stub_codex(
            dir.path(),
            r#"
artifact_root=''
while [ "$#" -gt 0 ]; do
    if [ "$1" = '--add-dir' ]; then shift; artifact_root="$1"; fi
    shift
done
printf '%s' '{"summary":"ok","evidence":[]}' > "$artifact_root/result.json"
echo '{"type":"turn.completed"}'
"#,
        );
        let mut req = sample_req(dir.path().to_path_buf());
        req.work_dir = Some(work_dir.clone());
        let outcome = CodexAdapter::new(config)
            .run(req, "run-wd-1", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        assert!(
            matches!(outcome.terminal, Terminal::Done { .. }),
            "{:?}",
            outcome.terminal
        );
        let prompt = std::fs::read_to_string(dir.path().join("runs/run-wd-1/prompt.txt")).unwrap();
        assert!(
            prompt.contains(&format!("cwd は `{}`", work_dir.display())),
            "{prompt}"
        );
        assert!(
            prompt.contains(&format!(
                "成果物ディレクトリは `{}`",
                dir.path().join("artifacts").display()
            )),
            "{prompt}"
        );
        assert!(
            prompt.contains("相対 `artifacts/` はリポジトリの中を指すので使わない"),
            "{prompt}"
        );
    }

    /// ADR-0006 Phase 115 D2（本番障害 01M3915FARENW8M0JM11XVF6W0 / 01M38T8N17MEWPTJQXGX1TNYJD）:
    /// `codex` が `--add-dir` を無視して（あるいは resume で落として）cwd 相対の `artifacts/result.json`
    /// に書いてしまっても、正しい置き場へ移して採用し `Done` になる。worktree 側には残らない（D4(b)）。
    #[tokio::test]
    async fn a_result_json_written_under_work_dir_is_adopted_and_not_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let work_dir = dir.path().join("repos/agent-platform");
        std::fs::create_dir_all(&work_dir).unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"wrote to the worktree by mistake","evidence":[]}' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        let mut req = sample_req(dir.path().to_path_buf());
        req.work_dir = Some(work_dir.clone());
        let outcome = CodexAdapter::new(config)
            .run(req, "run-wd-2", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => {
                assert_eq!(summary, "wrote to the worktree by mistake")
            }
            other => panic!("expected done, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap(),
            r#"{"summary":"wrote to the worktree by mistake","evidence":[]}"#
        );
        assert!(
            !work_dir.join("artifacts").exists(),
            "the stray artifacts/ dir under work_dir should be gone"
        );
    }

    #[tokio::test]
    async fn turn_failed_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"turn.failed","error":"sandbox denied write"}'"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-2", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("sandbox denied write"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// 実機（codex-cli 0.154.0, 2026-09-14 確認）では `turn.failed.error` はオブジェクト
    /// （`{"message":"..."}`）で返ってくる。文字列を仮定すると読み落とす回帰テスト。
    #[tokio::test]
    async fn turn_failed_with_object_shaped_error_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"turn.failed","error":{"message":"the model is not supported"}}'"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-2b", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("the model is not supported"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// `turn.failed.error.message` が供給側失敗の文言に一致すれば `AdapterError::Exhausted` として
    /// 返る（ADR-0010 D5）。result.json も書かれる。
    #[tokio::test]
    async fn turn_failed_classified_as_exhausted_surfaces_as_adapter_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"turn.failed","error":{"message":"You'"'"'ve hit your usage limit"}}'"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-2c", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::Exhausted(_)), "{err:?}");
        assert!(dir.path().join("runs/run-2c/result.json").is_file());
    }

    /// `turn.*` を一度も観測できずに exit した場合も `{"type":"error",...}` の直前の行を分類する
    /// （ADR-0010 D5）。
    #[tokio::test]
    async fn crash_with_matching_error_line_is_classified_as_provider_failure() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"error","message":"429 Too Many Requests"}'
exit 9
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let err = adapter
            .run(req, "run-2d", default_limits(), &sink)
            .await
            .expect_err("expected a provider failure");
        assert!(matches!(err, AdapterError::Throttled { .. }), "{err:?}");
    }

    /// ADR-0036 D1/D2: 共有 workspace のタスクは `.taskd/artifacts/<task_id>/result.json` を読む。
    #[tokio::test]
    async fn a_shared_workspace_task_uses_its_own_artifacts_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p .taskd/artifacts/T1
printf '%s' '{"summary":"mine","evidence":[]}' > .taskd/artifacts/T1/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/result.json"),
            r#"{"summary":"sibling"}"#,
        )
        .unwrap();
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.artifacts_dir = dir.path().join(".taskd/artifacts/T1");
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-shared", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert_eq!(summary, "mine"),
            other => panic!("expected done, got {other:?}"),
        }
        let prompt =
            std::fs::read_to_string(dir.path().join("runs/run-shared/prompt.txt")).unwrap();
        assert!(
            prompt.contains(".taskd/artifacts/T1/result.json"),
            "{prompt}"
        );
    }

    #[tokio::test]
    async fn success_without_result_file_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), r#"echo '{"type":"turn.completed"}'"#);
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-3", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn question_in_result_file_blocks_task() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"question":"which crate version?"}' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-4", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Question { text } => assert_eq!(text, "which crate version?"),
            other => panic!("expected question, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn crash_without_turn_message_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), "exit 9");
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-5", default_limits(), &sink)
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, Some(9));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=9"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// クラッシュ前に `artifacts/result.json` が存在していても、`turn.completed`/`turn.failed` を
    /// 一度も観測できなければ信用しない（ADR-0006 D4 と同じ回帰、ADR-0008 D3）。
    #[tokio::test]
    async fn stale_result_file_without_turn_message_is_not_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"looks done but crashed before saying so","evidence":[]}' > artifacts/result.json
exit 9
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-6", default_limits(), &sink)
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, Some(9));
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("exit=9"), "{message}");
            }
            other => panic!("expected error (stale file must not be trusted), got {other:?}"),
        }
    }

    /// 前回の run が残した `artifacts/result.json` は、今回の run 開始時に消される（ADR-0006 D3 と同じ）。
    #[tokio::test]
    async fn stale_result_file_from_previous_run_is_cleared_before_this_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
        std::fs::write(
            dir.path().join("artifacts/result.json"),
            r#"{"summary":"stale from a previous attempt","evidence":[]}"#,
        )
        .unwrap();
        let config = stub_codex(dir.path(), r#"echo '{"type":"turn.completed"}'"#);
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-7", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("artifacts/result.json"), "{message}");
            }
            other => {
                panic!("expected error (stale file must be cleared, not reused), got {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn wall_clock_exceeded_kills_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), "sleep 30");
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_millis(300),
            idle_timeout: Duration::from_secs(30),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-8", limits, &sink).await.unwrap();
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
    async fn idle_timeout_kills_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"item.started","item":{"type":"agent_message"}}'
sleep 30
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let limits = RunLimits {
            wall_clock: Duration::from_secs(30),
            idle_timeout: Duration::from_millis(300),
            kill_grace: Duration::from_millis(200),
        };
        let start = Instant::now();
        let outcome = adapter.run(req, "run-9", limits, &sink).await.unwrap();
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
    async fn malformed_evidence_in_result_file_does_not_fail_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"all good","evidence":["cargo test: 4 passed",{"criterion":0,"command":"cargo test","exit":0,"stdout_tail":""},42]}' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-10", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done {
                summary, evidence, ..
            } => {
                assert_eq!(summary, "all good");
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].command.as_deref(), Some("cargo test"));
            }
            other => panic!("expected done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_result_file_json_is_retryable_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf 'not json' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-11", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Error { retryable, message } => {
                assert!(retryable);
                assert!(message.contains("not valid JSON"), "{message}");
            }
            other => panic!("expected error, got {other:?}"),
        }
    }

    /// D3 の核心契約（`exec --json`、`--model` の位置、プロンプトが最終引数）が壊れてもテストが
    /// 緑のままにならないよう、実際に渡された引数をファイルに記録して検証する（監査で指摘）。
    #[tokio::test]
    async fn command_line_has_exec_json_model_then_prompt_as_last_arg() {
        let dir = tempfile::tempdir().unwrap();
        let config = CodexConfig {
            command: {
                let path = dir.path().join("codex_stub.sh");
                // 引数は改行を含みうる（プロンプト）ので NUL 区切りで記録する。
                // ETXTBSY 対策（ADR-0010 D10）: 別プロセスで書く。
                crate::test_support::write_executable(
                    &path,
                    "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\0' \"$a\" >> \"$(dirname \"$0\")/args.log\"; done\necho '{\"type\":\"turn.completed\"}'\n",
                );
                path.to_string_lossy().into_owned()
            },
            extra_args: vec!["--sandbox".into(), "read-only".into()],
            model: Some("gpt-5-codex".into()),
            env: Vec::new(),
            container: None,
            resume_mode: CodexResumeMode::default(),
            resume_bypass: CodexResumeBypass::default(),
        };
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-12", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Error { .. }));

        let args_log = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
        let args: Vec<&str> = args_log.split('\0').filter(|s| !s.is_empty()).collect();
        assert_eq!(
            args.len(),
            12,
            "expected exactly one trailing prompt arg, got {args:?}"
        );
        assert_eq!(
            &args[..7],
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "-c",
                "sandbox_mode=\"workspace-write\"",
                "--model",
                "gpt-5-codex"
            ]
        );
        assert_eq!(args[7], "--add-dir");
        assert_eq!(args[8], dir.path().join("artifacts").to_str().unwrap());
        assert_eq!(&args[9..11], ["--sandbox", "read-only"]);
        let prompt = args[11];
        assert!(
            prompt.contains("# Task:"),
            "prompt should be the last arg: {prompt}"
        );
    }

    /// ADR-0016 M8: `codex` も run の終わりに `artifacts/delegate.json` があれば `sink.delegate` を 1 回呼ぶ。
    #[tokio::test]
    async fn delegate_json_written_by_worker_is_forwarded_to_sink() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
printf '%s' '{"summary":"delegated two subtasks","evidence":[]}' > artifacts/result.json
printf '%s' '{"tasks":[{"title":"a","objective":"do a","acceptance":[{"text":"c","check":{"type":"human"}}]},{"title":"b","objective":"do b","acceptance":[{"text":"c","check":{"type":"human"}}]}]}' > artifacts/delegate.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-13", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let delegated = sink.delegated.lock().unwrap();
        assert_eq!(delegated.len(), 1);
        assert_eq!(delegated[0].len(), 2);
    }

    /// ADR-0025 D3: `token_count` の `rate_limits` を解析すると `sink.rate_limit` に観測値が渡る。
    #[tokio::test]
    async fn token_count_event_line_is_forwarded_to_the_sink() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"mkdir -p artifacts
echo '{"type":"token_count","rate_limits":{"primary":{"used_percent":14.0,"window_minutes":300,"resets_in_seconds":3600},"secondary":{"used_percent":24.0,"window_minutes":10080,"resets_in_seconds":432000}}}'
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-rate-1", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let observed = sink.rate_limits.lock().unwrap();
        assert_eq!(observed.len(), 1);
        let obs = &observed[0];
        assert_eq!(obs.five_hour.map(|w| w.utilization), Some(0.14));
        assert_eq!(obs.seven_day.map(|w| w.utilization), Some(0.24));
    }

    /// ADR-0025 D2: `with_env` の追加分（`CODEX_HOME`）は既存の同名キーより後に環境を組み立てるので勝つ。
    #[tokio::test]
    async fn with_env_overrides_a_same_name_key_already_in_config_env() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("env-seen.txt");
        let mut config = CodexConfig {
            command: {
                let path = dir.path().join("codex_stub.sh");
                crate::test_support::write_executable(
                    &path,
                    &format!(
                        "#!/bin/sh\nmkdir -p artifacts\nprintf '%s' \"$CODEX_HOME\" > {out}\nprintf '%s' '{{\"summary\":\"ok\",\"evidence\":[]}}' > artifacts/result.json\necho '{{\"type\":\"turn.completed\"}}'\n",
                        out = out_file.display()
                    ),
                );
                path.to_string_lossy().into_owned()
            },
            ..CodexConfig::default()
        };
        config
            .env
            .push(("CODEX_HOME".to_string(), "old-account-dir".to_string()));
        let base = CodexAdapter::new(config);
        let with_env = base
            .with_env(&[("CODEX_HOME".to_string(), "new-account-dir".to_string())])
            .expect("codex supports with_env");

        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = with_env
            .run(req, "run-env-1", default_limits(), &sink)
            .await
            .unwrap();
        assert!(matches!(outcome.terminal, Terminal::Done { .. }));
        let seen = std::fs::read_to_string(&out_file).unwrap();
        assert_eq!(seen, "new-account-dir");
    }
    #[tokio::test]
    async fn tier_binding_reaches_cli_model_argument_and_preserves_account_env() {
        use task_core::{Tier, model_routing::ModelBinding};
        for tier in [Tier::Frontier, Tier::Standard, Tier::Cheap] {
            let dir = tempfile::tempdir().unwrap();
            let config = stub_codex(
                dir.path(),
                r#"
for a in "$@"; do printf '%s\0' "$a" >> args.log; done
printf '%s' "$ROUTING_ACCOUNT" > account.log
mkdir -p artifacts
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
printf '%s\n' '{"type":"turn.completed"}'
"#,
            );
            let expected = format!("explicit-{tier:?}");
            let adapter = crate::tiered::TieredAdapter {
                base: Arc::new(CodexAdapter::new(config)),
                account_id: Some("account-a".into()),
                credential_error: None,
                models: [(
                    tier,
                    ModelBinding {
                        name: "requested-name".into(),
                        model_id: Some(expected.clone()),
                        unavailable_reason: None,
                    },
                )]
                .into(),
            };
            let adapter = adapter
                .with_env(&[("ROUTING_ACCOUNT".into(), "account-a".into())])
                .unwrap();
            assert_eq!(adapter.account_id(), Some("account-a"));
            let mut req = sample_req(dir.path().to_path_buf());
            req.task.worker_hint.tier = tier;
            let _ = adapter
                .run(req, "tier-run", default_limits(), &RecordingSink::default())
                .await
                .unwrap();
            let args = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
            let args: Vec<_> = args.split('\0').collect();
            let model = args.windows(2).find(|pair| pair[0] == "--model").unwrap()[1];
            assert_eq!(model, expected);
            assert_eq!(
                std::fs::read_to_string(dir.path().join("account.log")).unwrap(),
                "account-a"
            );
        }
    }

    fn args_log_script() -> &'static str {
        r#"
for a in "$@"; do printf '%s\0' "$a" >> args.log; done
mkdir -p artifacts
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
printf '%s\n' '{"type":"turn.completed"}'
"#
    }

    fn captured_args(dir: &std::path::Path) -> Vec<String> {
        let args = std::fs::read_to_string(dir.join("args.log")).unwrap();
        args.split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// ADR-0054 D1（Phase 67）: `context.session` が無ければ Phase 66 までと同じ（resume の引数は付かない）。
    #[tokio::test]
    async fn without_a_session_no_resume_flags_are_added() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), args_log_script());
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let _ = adapter
            .run(req, "run-1", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(!args.contains(&"resume".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("experimental_resume=")));
    }

    /// ADR-0054 D2（Phase 68）: CoS の対話 run（`conversation_addressee = Secretary`）だけ
    /// `sandbox_mode="read-only"`。それ以外は従来どおり `workspace-write`。
    #[tokio::test]
    async fn the_cos_conversation_run_gets_a_readonly_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), args_log_script());
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.conversation_addressee =
            Some(crate::protocol::ConversationAddressee::Secretary);
        let _ = adapter
            .run(req, "run-cos", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(
            args.contains(&"sandbox_mode=\"read-only\"".to_string()),
            "{args:?}"
        );
        assert!(!args.contains(&"sandbox_mode=\"workspace-write\"".to_string()));
    }

    /// 対話でない run・CoS 以外の対話には従来どおり `workspace-write`。
    #[tokio::test]
    async fn non_cos_runs_keep_the_workspace_write_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), args_log_script());
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let _ = adapter
            .run(req, "run-plain", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(
            args.contains(&"sandbox_mode=\"workspace-write\"".to_string()),
            "{args:?}"
        );
    }

    // ADR-0054 Phase 68b（本番障害 2026-09-21 15:04 UTC、release b24bae9a796a: CoS の対話が codex に
    // 割り当たり、`session.resume=true` の run が `error: unexpected argument '--add-dir' found` で
    // exit 2 を 2 回連発した）。
    //
    // Usage 行は実機（`~/.local/bin/codex exec --help` / `~/.local/bin/codex exec resume --help`、
    // codex-cli 0.155.1、2026-09-21）で確認したものをそのまま貼る:
    //
    //   $ codex exec --help
    //   Usage: codex exec [OPTIONS] [PROMPT]
    //          codex exec [OPTIONS] <COMMAND> [ARGS]
    //   OPTIONS（抜粋）: -c/--config <key=value>, -m/--model <MODEL>, --add-dir <DIR>, --json,
    //   --skip-git-repo-check, -s/--sandbox <SANDBOX_MODE> ...
    //
    //   $ codex exec resume --help
    //   Usage: codex exec resume [OPTIONS] [SESSION_ID] [PROMPT]
    //   OPTIONS（抜粋）: -c/--config <key=value>, --last, --all, -m/--model <MODEL>, --json,
    //   --skip-git-repo-check ... — `--add-dir` も `-s/--sandbox` も無い。
    //
    // つまり `-c/--config`（sandbox_mode の指定に使う）は両方で通るが、`--add-dir` は `exec resume` では
    // 拒否される。celeris 側の対応: `exec resume` のときだけ `--add-dir` を落とす（resume 先のスレッドは
    // 最初の（非 resume の）`exec` 呼び出しで受け取った writable-roots をそのまま引き継ぐ前提。ADR-0054
    // Phase 68b 追記参照）。

    /// (a) CoS の新規（fresh）run: `codex exec --json --skip-git-repo-check -c sandbox_mode="read-only"
    /// --add-dir <artifacts_dir> <prompt>`。read-only sandbox と `--add-dir` が両方乗ることを確認する
    /// （`exec` の usage 行に `--add-dir` があることに対応。上のコメント参照）。
    #[tokio::test]
    async fn phase_68b_fresh_cos_run_argv_has_readonly_sandbox_and_add_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), args_log_script());
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.conversation_addressee =
            Some(crate::protocol::ConversationAddressee::Secretary);
        let _ = adapter
            .run(req, "run-68b-fresh", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        let artifacts_dir = dir.path().join("artifacts").to_str().unwrap().to_string();
        assert_eq!(args.len(), 8, "{args:?}");
        assert_eq!(
            &args[..7],
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "-c",
                "sandbox_mode=\"read-only\"",
                "--add-dir",
                artifacts_dir.as_str(),
            ],
            "{args:?}"
        );
        assert!(args[7].contains("# Task:"), "prompt is last: {args:?}");
    }

    /// (b) CoS の継続（resume）run: production の再現。`--add-dir` を落とし、`-c sandbox_mode="read-only"`
    /// は維持したまま `codex exec resume <id> --json --skip-git-repo-check -c sandbox_mode="read-only"
    /// <prompt>` になることを確認する（`exec resume` の usage 行に `--add-dir` が無いことに対応）。
    #[tokio::test]
    async fn phase_68b_resume_cos_run_argv_drops_add_dir_keeps_readonly_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), args_log_script());
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.conversation_addressee =
            Some(crate::protocol::ConversationAddressee::Secretary);
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: CodexAdapter::ID.to_string(),
            session_id: "thread-68b".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-68b-resume", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert_eq!(args.len(), 8, "{args:?}");
        assert_eq!(
            &args[..7],
            [
                "exec",
                "resume",
                "thread-68b",
                "--json",
                "--skip-git-repo-check",
                "-c",
                "sandbox_mode=\"read-only\"",
            ],
            "{args:?}"
        );
        assert!(args[7].contains("# Task:"), "prompt is last: {args:?}");
        assert!(
            !args.contains(&"--add-dir".to_string()),
            "exec resume must not receive --add-dir (see usage-line comment above): {args:?}"
        );
    }

    /// (c) 通常（非 CoS）の run: 従来どおり `workspace-write` + `--add-dir` を維持し、Phase 68b の変更が
    /// 対話以外の run に影響しないことを確認する。
    #[tokio::test]
    async fn phase_68b_normal_run_argv_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), args_log_script());
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let _ = adapter
            .run(req, "run-68b-normal", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        let artifacts_dir = dir.path().join("artifacts").to_str().unwrap().to_string();
        assert_eq!(args.len(), 8, "{args:?}");
        assert_eq!(
            &args[..7],
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "-c",
                "sandbox_mode=\"workspace-write\"",
                "--add-dir",
                artifacts_dir.as_str(),
            ],
            "{args:?}"
        );
        assert!(args[7].contains("# Task:"), "prompt is last: {args:?}");
    }

    // ADR-0054 Phase 68c（本番障害 2026-09-21 15:44 UTC、release a2942d5d8a94。68b 配備後、fresh run は
    // 成功したが resume run が `--approve-for-me`（`[adapters.codex] extra_args` 由来）で exit 2）:
    //
    //   error: unexpected argument '--approve-for-me' found
    //   Usage: codex exec resume --json --skip-git-repo-check --config <key=value> <SESSION_ID> [PROMPT]
    //
    // `~/.local/bin/codex exec resume --help`（codex-cli 0.155.1。Phase 68c で再確認。テキストは
    // Phase 68b の確認と同一）:
    //
    //   Usage: codex exec resume [OPTIONS] [SESSION_ID] [PROMPT]
    //   Options（全量）: -c/--config <key=value>, --last, --all, --enable <FEATURE>,
    //   --disable <FEATURE>, -i/--image <FILE>, --strict-config, -m/--model <MODEL>,
    //   --dangerously-bypass-approvals-and-sandbox, --dangerously-bypass-hook-trust, --worktree,
    //   --thread-source <SOURCE>, --skip-git-repo-check, --ephemeral, --ignore-user-config,
    //   --ignore-rules, --output-schema <FILE>, --json, -o/--output-last-message <FILE>, -h/--help
    //   （`--add-dir`・`-s/--sandbox`・`--approve-for-me` は無い）。
    //
    // `--help` の OPTIONS 一覧は `-m/--model` を含むが、本番の実際のエラーが示した usage 行は
    // `--json`・`--skip-git-repo-check`・`--config`（＋位置引数）だけに絞られていた。`--help` の記載と
    // 実際に受け付けられる集合が一致しない疑いがあるため、`exec resume` の argv はここから
    // **ホワイトリスト方式**にした: `--json`・`--skip-git-repo-check`・`-c/--config`（複数可）＋
    // session id ＋ prompt 以外は一切乗せない。

    // ADR-0054 Phase 112 D1（本番障害 2026-09-23。Phase 68c 配備後、resume run が `--approve-for-me` を
    // 丸ごと落とした結果、CoS の対話が result.json を書けないまま最終メッセージに生の JSON を吐いていた）:
    // `exec resume` のホワイトリスト（`--json`・`--skip-git-repo-check`・`-c/--config`・位置引数）は
    // 維持したまま、`extra_args` の個々のフラグを `-c key=value` に翻訳できるものは翻訳し、翻訳できない
    // ものだけを落とす（`translate_resume_extra_args` 参照）。

    /// D4(a): resume run の argv に `--approve-for-me` がそのまま乗らないこと、`-c
    /// approval_policy="never"`・`-c sandbox_mode="workspace-write"` に翻訳されて乗ること、ホワイト
    /// リスト外のフラグが無いこと（production の `--approve-for-me` 拒否の回帰、かつ Phase 68c の
    /// 「丸ごと落とす」を「翻訳する」に変えたことの確認）。
    #[tokio::test]
    async fn phase_112_resume_translates_approve_for_me_to_config_overrides_and_drops_the_raw_flag()
     {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_codex(dir.path(), args_log_script());
        config.model = Some("gpt-5-codex".into());
        config.extra_args = vec!["--approve-for-me".into()];
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.conversation_addressee =
            Some(crate::protocol::ConversationAddressee::Secretary);
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: CodexAdapter::ID.to_string(),
            session_id: "thread-112".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-112-resume", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        // The whitelist pasted in the module comment above: --json, --skip-git-repo-check,
        // -c/--config (repeatable). Every other token that looks like a flag (starts with '-') is a
        // regression.
        const WHITELIST: &[&str] = &["--json", "--skip-git-repo-check", "-c"];
        for arg in &args {
            if arg.starts_with('-') {
                assert!(
                    WHITELIST.contains(&arg.as_str()),
                    "flag {arg:?} is not on the `exec resume` whitelist: {args:?}"
                );
            }
        }
        assert!(
            !args.contains(&"--approve-for-me".to_string()),
            "the raw operator flag must not reach `exec resume`: {args:?}"
        );
        assert!(!args.contains(&"--add-dir".to_string()), "{args:?}");
        assert!(!args.contains(&"--model".to_string()), "{args:?}");
        // The model still reaches codex, just via `-c model="..."` instead of `--model`.
        assert!(
            args.contains(&"model=\"gpt-5-codex\"".to_string()),
            "{args:?}"
        );
        // The unconditional CoS `sandbox_mode="read-only"` (ADR-0054 D2/Phase 68) is still there,
        // but the translation of `--approve-for-me` appends its own `-c` pairs after it.
        assert!(
            args.contains(&"sandbox_mode=\"read-only\"".to_string()),
            "{args:?}"
        );
        assert!(
            args.contains(&"approval_policy=\"never\"".to_string()),
            "{args:?}"
        );
        assert!(
            args.contains(&"sandbox_mode=\"workspace-write\"".to_string()),
            "{args:?}"
        );
    }

    /// D4(b): 新規スレッド（resume なし）の run では `--approve-for-me` が翻訳されず、従来どおりそのまま
    /// argv に乗ること（fresh は `exec resume` のホワイトリストの対象外なので、D1 の翻訳は resume だけの
    /// 話であることの回帰）。
    #[tokio::test]
    async fn phase_112_fresh_session_keeps_operator_extra_args_unmodified() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_codex(dir.path(), args_log_script());
        config.extra_args = vec!["--approve-for-me".into()];
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.conversation_addressee =
            Some(crate::protocol::ConversationAddressee::Secretary);
        // No `context.session`: this is a fresh (non-resuming) run.
        let _ = adapter
            .run(req, "run-112-fresh", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(!args.contains(&"resume".to_string()), "{args:?}");
        assert!(
            args.contains(&"--approve-for-me".to_string()),
            "fresh runs pass extra_args through unmodified: {args:?}"
        );
        assert!(
            !args.contains(&"approval_policy=\"never\"".to_string()),
            "fresh runs don't need translation: {args:?}"
        );
    }

    /// D1: `--full-auto`・`--sandbox <mode>`・`--ask-for-approval <policy>` も `-c` に翻訳される。
    #[tokio::test]
    async fn phase_112_full_auto_and_sandbox_and_ask_for_approval_translate_on_resume() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_codex(dir.path(), args_log_script());
        config.extra_args = vec![
            "--full-auto".into(),
            "--sandbox".into(),
            "danger-full-access".into(),
            "--ask-for-approval".into(),
            "on-request".into(),
        ];
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: CodexAdapter::ID.to_string(),
            session_id: "thread-112b".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-112-translate", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        for raw in ["--full-auto", "--sandbox", "--ask-for-approval"] {
            assert!(!args.contains(&raw.to_string()), "{raw} leaked: {args:?}");
        }
        assert!(
            args.contains(&"approval_policy=\"on-failure\"".to_string()),
            "--full-auto: {args:?}"
        );
        assert!(
            args.contains(&"sandbox_mode=\"danger-full-access\"".to_string()),
            "--sandbox danger-full-access: {args:?}"
        );
        assert!(
            args.contains(&"approval_policy=\"on-request\"".to_string()),
            "--ask-for-approval on-request: {args:?}"
        );
    }

    /// D2: 翻訳できないフラグが残り、`resume_bypass` も設定されていないときは resume 自体を諦め、
    /// 新規スレッド（`codex exec`、`resume` サブコマンド無し）として走る。新規スレッドには untranslatable
    /// なフラグを含め `extra_args` が丸ごと乗る。
    #[tokio::test]
    async fn phase_112_untranslatable_extra_args_without_bypass_skip_resume_and_run_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_codex(dir.path(), args_log_script());
        config.extra_args = vec!["--unknown-flag".into()];
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: CodexAdapter::ID.to_string(),
            session_id: "thread-112c".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-112-skip-resume", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(
            !args.contains(&"resume".to_string()),
            "an untranslatable extra_arg without resume_bypass must not attempt resume: {args:?}"
        );
        assert!(
            !args.contains(&"thread-112c".to_string()),
            "{args:?}"
        );
        assert!(
            args.contains(&"--unknown-flag".to_string()),
            "the fresh run gets the full, untranslated extra_args: {args:?}"
        );
    }

    /// D1: `resume_bypass = Dangerous` かつ untranslatable なフラグが残るときは、resume はそのまま
    /// 行われ、落とす代わりに `--dangerously-bypass-approvals-and-sandbox` を 1 回だけ足す。
    #[tokio::test]
    async fn phase_112_resume_bypass_dangerous_uses_the_bypass_flag_for_untranslatable_extra_args()
     {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_codex(dir.path(), args_log_script());
        config.extra_args = vec!["--unknown-flag".into()];
        config.resume_bypass = CodexResumeBypass::Dangerous;
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: CodexAdapter::ID.to_string(),
            session_id: "thread-112d".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-112-bypass", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(args.contains(&"resume".to_string()), "{args:?}");
        assert!(
            args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()),
            "{args:?}"
        );
        assert!(
            !args.contains(&"--unknown-flag".to_string()),
            "the untranslatable flag itself must not reach `exec resume`: {args:?}"
        );
    }

    /// D3: `result.json` を書かない（書けない）run が、最終メッセージに `result.json` と同じ形の JSON
    /// （`summary` + `actions`）を吐いたときは、それを `result.json` として回収し、`Done.summary` には
    /// 生の JSON 文字列ではなく JSON の `summary` を使う。回収したことが `sink.progress` に残る。
    #[tokio::test]
    async fn phase_112_a_recoverable_final_message_is_written_as_result_json_and_its_summary_is_used()
     {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo '{"type":"item.completed","item":{"type":"agent_message","text":"{\"summary\":\"直すタスクを作りました\",\"actions\":[{\"type\":\"create_task\",\"title\":\"直す\",\"objective\":\"直して\"}]}"}}'
echo '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.task.conversation = Some(task_core::MessageId::new());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-112-recover", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => {
                assert_eq!(summary, "直すタスクを作りました");
            }
            other => panic!("expected done, got {other:?}"),
        }
        let result_json = std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&result_json).unwrap();
        assert!(value.get("actions").is_some_and(|a| a.is_array()), "{value:?}");
        let progress = sink.progress.lock().unwrap();
        assert!(
            progress.iter().any(|m| m.contains("result recovered")),
            "{progress:?}"
        );
    }

    /// ADR-0054 D1（Phase 67）: 継続セッション（`resume: true`）かつ `resume_mode = ExecResume`（既定）
    /// なら `codex exec resume <id> …`。
    #[tokio::test]
    async fn a_continuing_session_uses_exec_resume_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), args_log_script());
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: CodexAdapter::ID.to_string(),
            session_id: "thread-123".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-2", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "resume");
        assert_eq!(args[2], "thread-123");
        assert!(!args.iter().any(|a| a.starts_with("experimental_resume=")));
    }

    /// ADR-0054 D1（Phase 67）: `resume_mode = ExperimentalResume`（`resume` サブコマンドの無い古い版）
    /// なら `-c experimental_resume=<id>` に切り替える。
    #[tokio::test]
    async fn a_continuing_session_uses_experimental_resume_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = stub_codex(dir.path(), args_log_script());
        config.resume_mode = CodexResumeMode::ExperimentalResume;
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: CodexAdapter::ID.to_string(),
            session_id: "thread-123".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-3", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(!args.contains(&"resume".to_string()));
        assert!(
            args.contains(&"experimental_resume=thread-123".to_string()),
            "{args:?}"
        );
    }

    /// ADR-0054 D1（Phase 67）: 新規セッション（`resume: false`）は resume の引数を付けない
    /// （codex は `--session-id` 相当の「これから使う id を固定する」手段を持たないため）。
    #[tokio::test]
    async fn a_fresh_session_adds_no_resume_flags() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), args_log_script());
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: CodexAdapter::ID.to_string(),
            session_id: "placeholder".to_string(),
            resume: false,
        });
        let _ = adapter
            .run(req, "run-4", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(!args.contains(&"resume".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("experimental_resume=")));
    }

    /// `context.session` が別アダプタ向けなら無視する。
    #[tokio::test]
    async fn a_session_for_another_adapter_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(dir.path(), args_log_script());
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: "claude-code".to_string(),
            session_id: "cc-session".to_string(),
            resume: true,
        });
        let _ = adapter
            .run(req, "run-5", default_limits(), &RecordingSink::default())
            .await
            .unwrap();
        let args = captured_args(dir.path());
        assert!(!args.contains(&"resume".to_string()));
    }

    /// ADR-0054 D1（Phase 67）: `thread.started` に `thread_id` があれば `session_established` へ報告する。
    /// フィールド名は未検証（コメント参照）だが、パーサの挙動そのものはこの偽の CLI で確認できる。
    #[tokio::test]
    async fn thread_started_with_a_thread_id_reports_session_established() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"
mkdir -p artifacts
printf '%s\n' '{"type":"thread.started","thread_id":"thread-xyz"}'
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
printf '%s\n' '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let _ = adapter
            .run(req, "run-6", default_limits(), &sink)
            .await
            .unwrap();
        assert_eq!(
            sink.sessions.lock().unwrap().as_slice(),
            &["thread-xyz".to_string()]
        );
    }

    /// ADR-0054 Phase 67c: `session_configured`（実装によっては `thread.started` の代わりにこの type
    /// 名を使うことがある）の `session_id` からも id を拾う。
    #[tokio::test]
    async fn session_configured_with_a_session_id_reports_session_established() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"
mkdir -p artifacts
printf '%s\n' '{"type":"session_configured","session_id":"sess-abc"}'
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
printf '%s\n' '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let _ = adapter
            .run(req, "run-6b", default_limits(), &sink)
            .await
            .unwrap();
        assert_eq!(
            sink.sessions.lock().unwrap().as_slice(),
            &["sess-abc".to_string()]
        );
    }

    /// ADR-0054 Phase 67c: `thread.started` に id が無ければ `session_established` は呼ばない
    /// （`node_sessions.session_id` は空文字のまま残り、Phase 67c の自己修復
    /// — `crate::sessions::session_id_is_valid_for_adapter` が空文字を無効扱いする — に委ねる）。
    #[tokio::test]
    async fn thread_started_without_an_id_does_not_report_session_established() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"
mkdir -p artifacts
printf '%s\n' '{"type":"thread.started"}'
printf '%s' '{"summary":"ok","evidence":[]}' > artifacts/result.json
printf '%s\n' '{"type":"turn.completed"}'
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let _ = adapter
            .run(req, "run-6c", default_limits(), &sink)
            .await
            .unwrap();
        assert!(sink.sessions.lock().unwrap().is_empty());
    }

    /// ADR-0054 D1（Phase 67）: `resume` を頼んだ run が turn.failed の `message` に「セッションが
    /// 見つからない」旨の文言を含んで終わったら `session_resume_failed` を報告する（文言は未検証）。
    #[tokio::test]
    async fn a_rejected_resume_reports_session_resume_failed() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"printf '%s\n' '{"type":"turn.failed","error":{"message":"session not found: 01ARZ3"}}'"#,
        );
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: CodexAdapter::ID.to_string(),
            session_id: "01ARZ3".to_string(),
            resume: true,
        });
        let sink = RecordingSink::default();
        let outcome = adapter.run(req, "run-7", default_limits(), &sink).await;
        match outcome {
            Ok(o) => assert!(matches!(o.terminal, Terminal::Error { retryable: true, .. })),
            Err(e) => panic!("expected Ok(Terminal::Error), got {e:?}"),
        }
        let failed = sink.resume_failed.lock().unwrap();
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].contains("session not found"), "{failed:?}");
    }

    /// resume していない run が同じ文言で失敗しても `session_resume_failed` は報告しない。
    #[tokio::test]
    async fn a_failure_without_resuming_does_not_report_session_resume_failed() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"printf '%s\n' '{"type":"turn.failed","error":{"message":"session not found: 01ARZ3"}}'"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let _ = adapter.run(req, "run-8", default_limits(), &sink).await;
        assert!(sink.resume_failed.lock().unwrap().is_empty());
    }

    /// Phase 98（ADR-0054 追記。実機障害 2026-09-22 00:18 UTC、task 01M337NT3QT1FR1G6WHS9G6NDA）:
    /// `codex exec resume` が**イベントを一つも出さずに** stderr に `list_turns is not supported yet
    /// (code -32601)` を出して exit 1 したら、`session_resume_failed` を 1 回報告した上で、**同じ run の
    /// 中で** resume 無しの fresh `codex exec` としてやり直し、run は done になる（stub は argv の
    /// `$2` が `resume` かどうかで振る舞いを変える: resume 呼び出しは失敗を模し、resume 無しの呼び出しは
    /// 正常な `thread.started`/`turn.completed` を出す）。
    #[tokio::test]
    async fn a_resume_rpc_failure_self_heals_within_the_same_run() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"if [ "$2" = "resume" ]; then
  echo 'Error: thread/resume: thread/resume failed: list_turns is not supported yet (code -32601)' >&2
  exit 1
else
  mkdir -p artifacts
  echo '{"type":"thread.started","thread_id":"thread-fresh-1"}'
  printf '%s' '{"summary":"done after fresh retry","evidence":[]}' > artifacts/result.json
  echo '{"type":"turn.completed","usage":{"input_tokens":5,"output_tokens":7}}'
fi
"#,
        );
        let adapter = CodexAdapter::new(config);
        let mut req = sample_req(dir.path().to_path_buf());
        req.context.session = Some(crate::protocol::SessionHandle {
            adapter: CodexAdapter::ID.to_string(),
            session_id: "01ARZ3".to_string(),
            resume: true,
        });
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-resume-heal", default_limits(), &sink)
            .await
            .unwrap();
        match outcome.terminal {
            Terminal::Done { summary, .. } => assert_eq!(summary, "done after fresh retry"),
            other => panic!("expected done after self-heal, got {other:?}"),
        }
        let failed = sink.resume_failed.lock().unwrap();
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(
            failed[0].contains("thread/resume") || failed[0].contains("-32601"),
            "{failed:?}"
        );
        // やり直しは resume していない（`thread.started` からの id）ので、新しい id だけが 1 回報告される。
        let sessions = sink.sessions.lock().unwrap();
        assert_eq!(sessions.as_slice(), ["thread-fresh-1".to_string()]);
        // どちらの試行のログも残る（デバッグ用。1 回目は resume の argv、2 回目は fresh の argv）。
        assert!(
            dir.path()
                .join("runs/run-resume-heal/stdout.jsonl")
                .is_file()
        );
        assert!(
            dir.path()
                .join("runs/run-resume-heal/stdout.resume_retry.jsonl")
                .is_file()
        );
    }

    /// resume していない run は、この自己回復の対象にならない（同じ文言で失敗しても 1 回で終わる。
    /// `session_resume_failed` も呼ばれない）。
    #[tokio::test]
    async fn a_non_resuming_run_is_unaffected_by_the_resume_rpc_self_heal() {
        let dir = tempfile::tempdir().unwrap();
        let config = stub_codex(
            dir.path(),
            r#"echo 'Error: thread/resume: thread/resume failed: list_turns is not supported yet (code -32601)' >&2
exit 1
"#,
        );
        let adapter = CodexAdapter::new(config);
        let req = sample_req(dir.path().to_path_buf());
        let sink = RecordingSink::default();
        let outcome = adapter
            .run(req, "run-no-resume", default_limits(), &sink)
            .await;
        match outcome {
            Ok(o) => assert!(matches!(o.terminal, Terminal::Error { retryable: true, .. })),
            Err(e) => panic!("expected Ok(Terminal::Error), got {e:?}"),
        }
        assert!(sink.resume_failed.lock().unwrap().is_empty());
        assert!(sink.sessions.lock().unwrap().is_empty());
        assert!(
            !dir.path()
                .join("runs/run-no-resume/stdout.resume_retry.jsonl")
                .exists(),
            "resume していない run はやり直さない"
        );
    }
}
