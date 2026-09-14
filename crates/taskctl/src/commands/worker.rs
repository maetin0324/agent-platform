//! `taskctl worker run` — DESIGN.md §5.9 のデバッグ用コマンド（ADR-0012 D4）。
//!
//! デーモンとディスパッチャを経由せず、1 タスクを 1 つのプロバイダ（アカウント）のアダプタで
//! 1 回だけ実行する。**状態は変えない**: リースを取らず、遷移もイベントの追記もしない
//! （DB は読むだけ）。レビューも行わない。`context.prior_review` / `context.answers` は
//! ディスパッチャと同じ関数（`task_dispatch::dispatcher::{prior_review_from_events,
//! answers_from_events}`）で events から組み立てる。
//!
//! プロバイダは `--provider`（ID 指定）/ `--adapter`（種別の先頭行）/ どちらも省略
//! （`StaticPolicy::select` に `worker_hint` を渡す、cooldown なし）の順で決める。
//! 作業ディレクトリは `--workspace` があればそれ、無ければタスクの `WorkspaceSpec::Local`
//! （相対なら `workspace_root` 基準）。タスクが `running`/`reviewing` のときは、デーモンの
//! run と作業ディレクトリを取り合うため `--workspace` 指定なしでは拒否する。

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use task_core::{ArtifactRef, Event, Status, Task, TaskId, TaskStore, WorkspaceSpec};
use task_dispatch::dispatcher::{answers_from_events, prior_review_from_events};
use task_dispatch::policy::Selection;
use task_dispatch::{ProviderPolicy, StaticPolicy};
use task_worker::{
    AdapterError, EventSink, LocalWorkspace, PROTOCOL_VERSION, ProviderFailure, RunContext, RunLimits, RunOutcome,
    RunRequest, Terminal, WorkerAdapter, WorkerMessage, Workspace,
};
use taskd::Config;

use crate::error::CliError;
use crate::outln;

#[derive(Subcommand, Debug)]
pub enum WorkerCommand {
    /// 1 タスクを 1 つのプロバイダのアダプタで 1 回だけ実行する（ADR-0012 D4）。
    Run(WorkerRunArgs),
}

#[derive(Args, Debug)]
pub struct WorkerRunArgs {
    /// `taskd.toml`。
    #[arg(long)]
    pub config: PathBuf,

    /// 実行するタスクの ID。
    #[arg(long)]
    pub task: String,

    /// 使うプロバイダ ID（`[[providers]].id`）。`--adapter` と同時指定不可。
    #[arg(long, conflicts_with = "adapter")]
    pub provider: Option<String>,

    /// 使うアダプタ種別（`[[providers]].adapter` に合う最初の行）。`--provider` と同時指定不可。
    #[arg(long)]
    pub adapter: Option<String>,

    /// 作業ディレクトリ。省略時はタスクの workspace（相対なら `workspace_root` 基準）。
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}

/// 選ばれたプロバイダ（`--provider`/`--adapter`/`select` のいずれか）。
struct Selected {
    provider_id: String,
    adapter_kind: String,
    model: String,
}

pub fn run_run(store: &dyn TaskStore, args: WorkerRunArgs) -> Result<ExitCode, CliError> {
    let task_id = crate::error::parse_task_id(&args.task)?;
    let config = Config::load(&args.config)
        .map_err(|e| CliError::msg(format!("failed to load config {}: {e}", args.config.display())))?;

    let task = store.get(task_id)?.ok_or_else(|| {
        CliError::msg(format!(
            "task not found: {task_id} (check that --db points at the same db as `db` in {})",
            args.config.display()
        ))
    })?;
    let events = store.events_for(task_id)?;

    let (provider_id, adapter_kind) = select_provider(&config, &task, &args)?;

    if args.workspace.is_none() && matches!(task.status, Status::Running | Status::Reviewing) {
        return Err(CliError::msg(format!(
            "task {task_id} is {:?}; pass --workspace to avoid racing the daemon's own run",
            task.status
        )));
    }

    let workspace_dir = match &args.workspace {
        Some(dir) => dir.clone(),
        None => match &task.workspace {
            WorkspaceSpec::Local { path } if path.is_relative() => config.workspace_root.join(path),
            WorkspaceSpec::Local { path } => path.clone(),
            WorkspaceSpec::Remote { .. } => {
                return Err(CliError::msg("task workspace is remote; pass --workspace to run it locally"));
            }
        },
    };

    let adapters = taskd::build_adapters(&config);
    let adapter = adapters
        .get(&provider_id)
        .ok_or_else(|| CliError::msg(format!("provider {provider_id} has no adapter instance")))?;
    let model = taskd::effective_models(&config).get(&provider_id).cloned().unwrap_or_default();

    let selected = Selected {
        provider_id,
        adapter_kind,
        model,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::msg(format!("failed to start tokio runtime: {e}")))?;

    rt.block_on(execute(&task, &events, workspace_dir, &config, &selected, adapter.as_ref()))
}

/// `--provider` / `--adapter` / どちらも省略（`select`、cooldown なし）の順でプロバイダを決める
/// （ADR-0012 D4）。
fn select_provider(config: &Config, task: &Task, args: &WorkerRunArgs) -> Result<(String, String), CliError> {
    let specs = config.provider_specs();

    if let Some(provider) = &args.provider {
        return specs
            .iter()
            .find(|p| &p.id == provider)
            .map(|p| (p.id.clone(), p.adapter.clone()))
            .ok_or_else(|| CliError::msg(format!("provider not found in config: {provider}")));
    }

    if let Some(adapter) = &args.adapter {
        return specs
            .iter()
            .find(|p| &p.adapter == adapter)
            .map(|p| (p.id.clone(), p.adapter.clone()))
            .ok_or_else(|| CliError::msg(format!("no provider configured for adapter: {adapter}")));
    }

    let policy = StaticPolicy::new(specs, Duration::from_secs(config.error_cooldown_secs));
    match policy.select(&task.worker_hint, Instant::now(), &HashSet::new()) {
        Selection::Picked { adapter, provider } => Ok((provider, adapter)),
        Selection::Busy => Err(CliError::msg(
            "no provider available right now (matching providers are all cooling down)",
        )),
        Selection::NoMatchingProvider => {
            Err(CliError::msg(format!("no provider configured for worker_hint {:?}", task.worker_hint)))
        }
    }
}

async fn execute(
    task: &Task,
    events: &[(u64, Event)],
    workspace_dir: PathBuf,
    config: &Config,
    selected: &Selected,
    adapter: &dyn WorkerAdapter,
) -> Result<ExitCode, CliError> {
    let prepared = LocalWorkspace::new(workspace_dir)
        .prepare(task)
        .await
        .map_err(|e| CliError::msg(format!("failed to prepare workspace: {e}")))?;

    let run_id = TaskId::new().to_string();
    let req = RunRequest {
        protocol: PROTOCOL_VERSION,
        task: task.clone(),
        workspace: prepared.clone(),
        context: RunContext {
            prior_review: prior_review_from_events(events),
            inputs: task.inputs.clone(),
            answers: answers_from_events(events),
            review: None,
        },
    };

    outln!(
        "worker run: task={} provider={} adapter={} model={} workspace={} run_id={run_id}",
        task.id,
        selected.provider_id,
        selected.adapter_kind,
        selected.model,
        prepared.display(),
    );

    let limits = RunLimits {
        wall_clock: Duration::from_secs(task.budget.max_wall_secs),
        idle_timeout: Duration::from_secs(config.idle_timeout_secs),
        kill_grace: Duration::from_secs(config.kill_grace_secs),
    };

    let sink = PrintSink;
    // 監査の指摘: シグナルの既定動作で taskctl が死ぬと drop が走らず、アダプタの子プロセス（別プロセスグループなので端末の
    // Ctrl-C も届かない）が kill されずに作業ディレクトリを編集し続ける。SIGINT / SIGTERM を受けたら run の future を drop して
    // 子を kill（`kill_on_drop`）し、exit 130 で終わる。
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| CliError::msg(format!("failed to install SIGTERM handler: {e}")))?;
    let finished = tokio::select! {
        result = adapter.run(req, &run_id, limits, &sink) => Some(result),
        _ = tokio::signal::ctrl_c() => None,
        _ = sigterm.recv() => None,
    };
    let Some(result) = finished else {
        eprintln!("worker run interrupted; the worker process was killed");
        return Ok(ExitCode::from(130));
    };
    let (message, exit) = normalize_outcome(result);
    let json =
        serde_json::to_string(&message).map_err(|e| CliError::msg(format!("failed to encode result: {e}")))?;
    outln!("result: {json}");
    Ok(ExitCode::from(exit))
}

/// アダプタの結果を、出す `WorkerMessage` と exit code に正規化する（ADR-0012 D4 の表）。
fn normalize_outcome(result: Result<RunOutcome, AdapterError>) -> (WorkerMessage, u8) {
    match result {
        Ok(outcome) => match outcome.terminal {
            Terminal::Done { summary, evidence, usage } => (WorkerMessage::Done { summary, evidence, usage }, 0),
            Terminal::Question { text } => (WorkerMessage::Question { text }, 3),
            Terminal::Error { message, retryable } => {
                (WorkerMessage::Error { message, retryable, provider_failure: None }, 4)
            }
        },
        Err(e) => {
            let provider_failure = match &e {
                AdapterError::Throttled { retry_after } => {
                    Some(ProviderFailure::Throttled { retry_after_secs: retry_after.as_secs() })
                }
                AdapterError::AuthFailed(_) => Some(ProviderFailure::AuthFailed),
                AdapterError::Exhausted(_) => Some(ProviderFailure::Exhausted),
                _ => None,
            };
            (
                WorkerMessage::Error {
                    message: format!("adapter: {e}"),
                    retryable: true,
                    provider_failure,
                },
                4,
            )
        }
    }
}

/// `progress` / `artifact` を逐次標準出力に出すシンク（ADR-0012 D4）。ストアには何も書かない。
#[derive(Debug, Default, Clone, Copy)]
struct PrintSink;

impl EventSink for PrintSink {
    fn progress(&self, msg: &str) {
        outln!("progress: {msg}");
    }

    fn artifact(&self, artifact: &ArtifactRef) {
        outln!("artifact: {} {} sha256={}", artifact.name, artifact.path, artifact.sha256);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_outcome_maps_done_question_and_error_to_exit_codes() {
        let (msg, exit) = normalize_outcome(Ok(RunOutcome {
            terminal: Terminal::Done { summary: "s".into(), evidence: vec![], usage: None },
            exit_code: Some(0),
        }));
        assert!(matches!(msg, WorkerMessage::Done { .. }));
        assert_eq!(exit, 0);

        let (msg, exit) = normalize_outcome(Ok(RunOutcome {
            terminal: Terminal::Question { text: "q?".into() },
            exit_code: Some(0),
        }));
        assert!(matches!(msg, WorkerMessage::Question { .. }));
        assert_eq!(exit, 3);

        let (msg, exit) = normalize_outcome(Ok(RunOutcome {
            terminal: Terminal::Error { message: "m".into(), retryable: false },
            exit_code: Some(1),
        }));
        assert!(matches!(msg, WorkerMessage::Error { provider_failure: None, .. }));
        assert_eq!(exit, 4);
    }

    #[test]
    fn normalize_outcome_maps_adapter_errors_to_provider_failure() {
        let (msg, exit) = normalize_outcome(Err(AdapterError::Throttled { retry_after: Duration::from_secs(7) }));
        assert!(matches!(
            msg,
            WorkerMessage::Error {
                provider_failure: Some(ProviderFailure::Throttled { retry_after_secs: 7 }),
                retryable: true,
                ..
            }
        ));
        assert_eq!(exit, 4);

        let (msg, _) = normalize_outcome(Err(AdapterError::AuthFailed("nope".into())));
        assert!(matches!(msg, WorkerMessage::Error { provider_failure: Some(ProviderFailure::AuthFailed), .. }));

        let (msg, _) = normalize_outcome(Err(AdapterError::Exhausted("nope".into())));
        assert!(matches!(msg, WorkerMessage::Error { provider_failure: Some(ProviderFailure::Exhausted), .. }));

        let (msg, _) = normalize_outcome(Err(AdapterError::Other("boom".into())));
        assert!(matches!(msg, WorkerMessage::Error { provider_failure: None, .. }));
    }
}
