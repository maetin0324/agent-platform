//! `taskctl worker run` — DESIGN.md §5.9 のデバッグ用コマンド（ADR-0012 D4）。
//!
//! デーモンとディスパッチャを経由せず、1 タスクを 1 つのプロバイダ（アカウント）のアダプタで
//! 1 回だけ実行する。**状態は変えない**: リースを取らず、遷移もイベントの追記もしない
//! （DB は読むだけ）。レビューも行わない。`context.prior_review` / `context.answers` は
//! ディスパッチャと同じ派生関数（`task_ops::derive::{prior_review_from_events,
//! answers_from_events}`）で events から組み立て、ワーカープロトコルの型
//! （`task_worker::{PriorReview, Answer}`）へ写す（ADR-0013 D7）。
//!
//! プロバイダは `--provider`（ID 指定）/ `--adapter`（種別の先頭行）/ どちらも省略
//! （`StaticPolicy::select` に `worker_hint` を渡す、cooldown なし）の順で決める。
//! 作業ディレクトリは `--workspace` があればそれ、無ければタスクの `WorkspaceSpec::Local`
//! （相対なら `workspace_root` 基準）。タスクが `running`/`reviewing` のときは、デーモンの
//! run と作業ディレクトリを取り合うため `--workspace` 指定なしでは拒否する。
//!
//! `--cluster <id>`（ADR-0018 受け入れ 10）: `[[clusters]] id` を指定すると、そのクラスタ側の
//! ディレクトリに対して 1 回 run する（デーモンを介さない動作確認）。DB はここでも変えない。
//! 多重接続（`ControlMaster`）が無ければ何も実行せず exit 4 で理由を返す。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use task_core::{ArtifactRef, Event, Status, Task, TaskId, TaskStore, WorkspaceSpec};
use task_dispatch::policy::Selection;
use task_dispatch::{ClusterSpec, ProviderPolicy, StaticPolicy};
use task_ops::derive::{AnswerNote, ReviewNote, answers_from_events, prior_review_from_events};
use task_worker::{
    Answer, AdapterError, EventSink, LocalWorkspace, PROTOCOL_VERSION, PriorReview, ProviderFailure, RunContext,
    RunLimits, RunOutcome, RunRequest, SshWorkspace, Terminal, WorkerAdapter, WorkerMessage, Workspace,
    WorkspaceError, remote_exec_instructions,
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
    /// `--cluster` と併用時は「クラスタ側」のパス（`taskctl add --cluster` と同じ意味）。
    #[arg(long)]
    pub workspace: Option<PathBuf>,

    /// クラスタ ID（`[[clusters]] id`）。指定すると、クラスタ側のディレクトリに対して 1 回だけ
    /// run する（pull → run → push。ADR-0018 D1/D4）。デーモンを介さない動作確認で、DB は変えない。
    /// 多重接続（`ControlMaster`）が無ければ何も実行せず exit 4 で理由を返す。
    #[arg(long)]
    pub cluster: Option<String>,
}

/// `--cluster` を解決した先（ADR-0018 受け入れ 10）。
#[derive(Debug)]
struct ClusterTarget {
    spec: ClusterSpec,
    /// クラスタ側の作業ディレクトリ。
    remote_path: PathBuf,
    /// 手元の写し（デーモンと同じ `workspace_root/<task_id>`）。
    mirror_dir: PathBuf,
    /// タスクの `WorkspaceSpec::Remote{cluster}` と `--cluster` が食い違うときの警告（呼び出し側が stderr に出す）。
    warning: Option<String>,
}

/// `--cluster <id>` の解決（純関数）。`config.cluster_specs()` に無ければエラー。クラスタ側パスは
/// `--workspace`（あれば）かタスクの `WorkspaceSpec::Remote{path}`。`Local` タスクに `--workspace` が
/// 無ければエラー。手元の写しはデーモンと共有するため、タスクが `running`/`reviewing` なら
/// `--workspace` の有無に関係なく拒否する。
fn resolve_cluster_target(
    config: &Config,
    task: &Task,
    cluster_id: &str,
    workspace_arg: Option<&Path>,
) -> Result<ClusterTarget, CliError> {
    let mut specs = config.cluster_specs();
    let spec = specs
        .remove(cluster_id)
        .ok_or_else(|| CliError::msg(format!("cluster not found in config: {cluster_id}")))?;

    if matches!(task.status, Status::Running | Status::Reviewing) {
        return Err(CliError::msg(format!(
            "task {} is {:?}; stop taskd or wait before `worker run --cluster`",
            task.id, task.status
        )));
    }

    let mut warning = None;
    let remote_path = if let Some(path) = workspace_arg {
        if let WorkspaceSpec::Remote { cluster, .. } = &task.workspace
            && cluster != cluster_id
        {
            warning = Some(format!(
                "task {} workspace targets cluster {cluster:?}; overriding with --cluster {cluster_id:?}",
                task.id
            ));
        }
        path.to_path_buf()
    } else {
        match &task.workspace {
            WorkspaceSpec::Remote { cluster, path } => {
                if cluster != cluster_id {
                    warning = Some(format!(
                        "task {} workspace targets cluster {cluster:?}; overriding with --cluster {cluster_id:?}",
                        task.id
                    ));
                }
                path.clone()
            }
            WorkspaceSpec::Local { .. } => {
                return Err(CliError::msg(format!(
                    "task {} has a local workspace; pass --workspace <cluster-side path> with --cluster",
                    task.id
                )));
            }
        }
    };

    let mirror_dir = config.workspace_root.join(task.id.to_string());
    Ok(ClusterTarget { spec, remote_path, mirror_dir, warning })
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

    if let Some(cluster_id) = &args.cluster {
        let target = resolve_cluster_target(&config, &task, cluster_id, args.workspace.as_deref())?;
        if let Some(warning) = &target.warning {
            eprintln!("warning: {warning}");
        }
        return rt.block_on(execute_on_cluster(&task, &events, target, &config, &selected, adapter.as_ref()));
    }

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
            prior_review: to_prior_review(prior_review_from_events(events)),
            inputs: task.inputs.clone(),
            answers: to_answers(answers_from_events(events)),
            review: None,
            role: None,
            children: Vec::new(),
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

/// `--cluster <id>`: クラスタ側の作業ディレクトリに対して 1 回だけ run する（ADR-0018 受け入れ 10）。
/// `SshWorkspace` を経由すること以外は `execute` と同じ（DB は変えない。リースも取らない）。
async fn execute_on_cluster(
    task: &Task,
    events: &[(u64, Event)],
    target: ClusterTarget,
    config: &Config,
    selected: &Selected,
    adapter: &dyn WorkerAdapter,
) -> Result<ExitCode, CliError> {
    let settings = target.spec.ssh_settings(&target.remote_path);
    let ws = SshWorkspace::new(&target.mirror_dir, settings.clone());

    if !ws.control_master_alive().await {
        return unreachable_result(format!(
            "no ssh ControlMaster connection to {} (host {}); run scripts/cluster-login.sh {}",
            target.spec.id, target.spec.host, target.spec.host
        ));
    }

    let prepared = match ws.prepare(task).await {
        Ok(p) => p,
        Err(WorkspaceError::Unreachable(msg)) => return unreachable_result(msg),
        Err(e) => return Err(CliError::msg(format!("failed to prepare workspace: {e}"))),
    };
    ws.write_remote_exec_helper()
        .await
        .map_err(|e| CliError::msg(format!("failed to write .taskd/remote-exec: {e}")))?;

    // ADR-0018 D3: DB のタスクは変えない。渡す写しの `objective` だけにラッパの使い方を足す。
    let mut run_task = task.clone();
    run_task.objective.push_str(&remote_exec_instructions(&settings));

    let run_id = TaskId::new().to_string();
    let req = RunRequest {
        protocol: PROTOCOL_VERSION,
        task: run_task,
        workspace: prepared.clone(),
        context: RunContext {
            prior_review: to_prior_review(prior_review_from_events(events)),
            inputs: task.inputs.clone(),
            answers: to_answers(answers_from_events(events)),
            review: None,
            role: None,
            children: Vec::new(),
        },
    };

    outln!(
        "worker run: task={} provider={} adapter={} model={} workspace={} run_id={run_id} cluster={} remote={}:{}",
        task.id,
        selected.provider_id,
        selected.adapter_kind,
        selected.model,
        prepared.display(),
        target.spec.id,
        target.spec.host,
        target.remote_path.display(),
    );

    let limits = RunLimits {
        wall_clock: Duration::from_secs(task.budget.max_wall_secs),
        idle_timeout: Duration::from_secs(config.idle_timeout_secs),
        kill_grace: Duration::from_secs(config.kill_grace_secs),
    };

    let sink = PrintSink;
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

    // ADR-0018 D4: run が終わった（アダプタ自体が Ok を返した）ら、手元の編集をクラスタへ push する。
    if result.is_ok() {
        match ws.push().await {
            Ok(()) => outln!("progress: pushed to cluster {}:{}", target.spec.id, target.remote_path.display()),
            Err(WorkspaceError::Unreachable(msg)) => return unreachable_result(msg),
            Err(e) => return Err(CliError::msg(format!("failed to push to cluster: {e}"))),
        }
    }

    let (message, exit) = normalize_outcome(result);
    let json =
        serde_json::to_string(&message).map_err(|e| CliError::msg(format!("failed to encode result: {e}")))?;
    outln!("result: {json}");
    Ok(ExitCode::from(exit))
}

/// ssh / rsync 自体が失敗した（多重接続が無い等）ときの表示（ADR-0018 D5: 供給側失敗と同じ形）。
/// アダプタは起動しない／起動できなかったので、`WorkerMessage::Error{retryable: true}` を出して exit 4。
fn unreachable_result(message: String) -> Result<ExitCode, CliError> {
    let msg = WorkerMessage::Error {
        message,
        retryable: true,
        provider_failure: None,
    };
    let json = serde_json::to_string(&msg).map_err(|e| CliError::msg(format!("failed to encode result: {e}")))?;
    outln!("result: {json}");
    Ok(ExitCode::from(4))
}

/// `task_ops::derive::ReviewNote` をワーカープロトコルの `task_worker::PriorReview` に写す
/// （ADR-0013 D7: task-ops は task_worker に依存しないため、この写像は呼び出し側で行う）。
fn to_prior_review(notes: Vec<ReviewNote>) -> Vec<PriorReview> {
    notes
        .into_iter()
        .map(|n| PriorReview {
            criterion: n.criterion,
            pass: n.pass,
            reason: n.reason,
        })
        .collect()
}

/// `task_ops::derive::AnswerNote` をワーカープロトコルの `task_worker::Answer` に写す。
fn to_answers(notes: Vec<AnswerNote>) -> Vec<Answer> {
    notes
        .into_iter()
        .map(|n| Answer {
            question: n.question,
            answer: n.answer,
        })
        .collect()
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

    /// ADR-0016 D2: `worker run` は DB を変えないので、提案は表示するだけで子タスクは挿入しない。
    fn delegate(&self, tasks: &[task_core::DelegateTask]) {
        let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
        outln!(
            "delegate: {} task(s) proposed (not inserted; worker run does not write the DB): {}",
            tasks.len(),
            titles.join(", ")
        );
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

    /// `Config` を toml を経由せず直接組み立てる（taskctl は `toml` crate に依存していないため）。
    fn cluster_config(clusters: Vec<taskd::config::ClusterConfig>) -> Config {
        Config {
            db: PathBuf::from("taskd.sqlite3"),
            workspace_root: PathBuf::from("workspaces"),
            tick_ms: 2000,
            max_concurrency: 2,
            lease_grace_secs: 60,
            idle_timeout_secs: 30,
            kill_grace_secs: 5,
            review_timeout_secs: 60,
            error_cooldown_secs: 30,
            retry_backoff_base_secs: 10,
            retry_backoff_max_secs: 300,
            max_requeues: 3,
            adapters: Default::default(),
            plan: Default::default(),
            reviewer: Default::default(),
            api: Default::default(),
            providers: vec![],
            clusters,
            roles: vec![],
            delegation: Default::default(),
            source_path: None,
        }
    }

    fn cluster(id: &str, host: &str) -> taskd::config::ClusterConfig {
        taskd::config::ClusterConfig {
            id: id.into(),
            host: host.into(),
            concurrency: 1,
            sync: "rsync".into(),
            delete_on_push: false,
            setup: vec![],
            env: std::collections::HashMap::new(),
            rsync_excludes: vec![],
        }
    }

    fn task_fixture(status: Status, workspace: WorkspaceSpec) -> Task {
        let now = time::OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: task_core::TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status,
            priority: 0,
            worker_hint: task_core::WorkerHint { tier: task_core::Tier::Standard, adapter: None },
            workspace,
            budget: task_core::Budget { max_turns: 1, max_wall_secs: 30, max_retries: 0 },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: None,
            aggregate: false,
        }
    }

    #[test]
    fn resolve_cluster_target_errors_when_cluster_missing() {
        let config = cluster_config(vec![]);
        let task = task_fixture(Status::Ready, WorkspaceSpec::Local { path: "/tmp/x".into() });
        let err = resolve_cluster_target(&config, &task, "local", None).unwrap_err();
        assert!(err.to_string().contains("cluster not found in config: local"), "{err}");
    }

    #[test]
    fn resolve_cluster_target_requires_workspace_arg_for_local_task() {
        let config = cluster_config(vec![cluster("local", "h")]);
        let task = task_fixture(Status::Ready, WorkspaceSpec::Local { path: "/tmp/x".into() });
        let err = resolve_cluster_target(&config, &task, "local", None).unwrap_err();
        assert!(err.to_string().contains("has a local workspace"), "{err}");
    }

    #[test]
    fn resolve_cluster_target_uses_task_remote_path_without_workspace_arg() {
        let config = cluster_config(vec![cluster("local", "h")]);
        let task = task_fixture(
            Status::Ready,
            WorkspaceSpec::Remote { cluster: "local".into(), path: "/remote/proj".into() },
        );
        let target = resolve_cluster_target(&config, &task, "local", None).unwrap();
        assert_eq!(target.remote_path, PathBuf::from("/remote/proj"));
        assert_eq!(target.mirror_dir, config.workspace_root.join(task.id.to_string()));
        assert!(target.warning.is_none());
        assert_eq!(target.spec.host, "h");
    }

    #[test]
    fn resolve_cluster_target_workspace_arg_overrides_task_path() {
        let config = cluster_config(vec![cluster("local", "h")]);
        let task = task_fixture(Status::Ready, WorkspaceSpec::Local { path: "/tmp/x".into() });
        let target = resolve_cluster_target(&config, &task, "local", Some(Path::new("/remote/other"))).unwrap();
        assert_eq!(target.remote_path, PathBuf::from("/remote/other"));
        assert!(target.warning.is_none());
    }

    #[test]
    fn resolve_cluster_target_warns_when_task_targets_a_different_cluster() {
        let config = cluster_config(vec![cluster("local", "h")]);
        let task = task_fixture(
            Status::Ready,
            WorkspaceSpec::Remote { cluster: "other".into(), path: "/remote/proj".into() },
        );
        let target = resolve_cluster_target(&config, &task, "local", None).unwrap();
        assert_eq!(target.remote_path, PathBuf::from("/remote/proj"));
        assert!(target.warning.as_deref().unwrap().contains("other"));
    }

    #[test]
    fn resolve_cluster_target_rejects_running_or_reviewing_task_even_with_workspace_arg() {
        let config = cluster_config(vec![cluster("local", "h")]);
        for status in [Status::Running, Status::Reviewing] {
            let task = task_fixture(status, WorkspaceSpec::Local { path: "/tmp/x".into() });
            let err = resolve_cluster_target(&config, &task, "local", Some(Path::new("/remote/x"))).unwrap_err();
            assert!(err.to_string().contains("stop taskd or wait"), "{err}");
        }
    }
}
