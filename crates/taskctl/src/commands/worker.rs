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
    /// P-54: 省略時は環境変数 `TASKD_CONFIG` を見る。
    #[arg(long, env = "TASKD_CONFIG")]
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

    /// ADR-0024 D2 / ADR-0025 D2: `account_pool = true` のプロバイダで使うアカウント id（プロバイダの
    /// `adapter` に応じて `[accounts] claude_dir`/`codex_dir` の下のディレクトリ）。省略時は
    /// `<root>/.taskd-usage.json`（永続化された観測値。taskd が更新するのと同じファイル）を読むだけで選ぶ
    /// （in_use は分からないので 0 として扱う）。`account_pool` でないプロバイダに指定するとエラー。
    #[arg(long)]
    pub account: Option<String>,
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
    /// ADR-0024 D2: `account_pool` のプロバイダで使うアカウント（プールを使わなければ `None`）。
    account: Option<String>,
}

/// ADR-0024 D2 / ADR-0025 D2: `account_pool = true` のプロバイダのアカウントを決める。`--account` があれば
/// それ、無ければ `<root>/.taskd-usage.json`（永続化された観測値）を読み取り専用で見て選ぶ（`in_use` は 0
/// 扱い）。`account_pool` でないプロバイダに `--account` を渡したらエラー。アダプタ（claude-code/codex）は
/// プロバイダの `adapter` から決まる。
fn resolve_account(
    config: &Config,
    provider_id: &str,
    adapter_kind: &str,
    args: &WorkerRunArgs,
) -> Result<Option<String>, CliError> {
    let is_pool = config.providers.iter().any(|p| p.id == provider_id && p.account_pool);
    if !is_pool {
        if args.account.is_some() {
            return Err(CliError::msg(format!("provider {provider_id} does not have account_pool = true; --account is not applicable")));
        }
        return Ok(None);
    }
    let account_adapter = task_core::AccountAdapter::parse(adapter_kind).ok_or_else(|| {
        CliError::msg(format!("provider {provider_id} has account_pool = true but adapter {adapter_kind:?} is not a pool adapter"))
    })?;
    let accounts = config
        .accounts
        .as_ref()
        .ok_or_else(|| CliError::msg(format!("provider {provider_id} has account_pool = true but [accounts] is not configured")))?;
    let root = accounts.root_for(account_adapter).ok_or_else(|| {
        CliError::msg(format!(
            "provider {provider_id} has account_pool = true but [accounts] has no root configured for adapter {adapter_kind}"
        ))
    })?;
    if let Some(id) = &args.account {
        if !task_dispatch::valid_account_id(id) {
            return Err(CliError::msg(format!("invalid --account id: {id}")));
        }
        if !root.join(id).is_dir() {
            return Err(CliError::msg(format!("account not found: {id} (looked in {})", root.display())));
        }
        return Ok(Some(id.clone()));
    }
    let dirs = task_dispatch::scan_accounts(root, account_adapter);
    let book = task_dispatch::AccountBook::load(&root.join(".taskd-usage.json"));
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let candidates: Vec<task_dispatch::AccountCandidate<'_>> = dirs
        .iter()
        .map(|d| task_dispatch::AccountCandidate { id: d.id.as_str(), logged_in: d.logged_in, in_use: 0 })
        .collect();
    task_dispatch::select_account(&candidates, &book, accounts.max_runs_per_account, now)
        .map(Some)
        .ok_or_else(|| CliError::msg(format!("no eligible account in the pool for provider {provider_id}")))
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
    let account = resolve_account(&config, &provider_id, &adapter_kind, &args)?;

    let adapters = taskd::build_adapters(&config);
    let base_adapter = adapters
        .get(&provider_id)
        .ok_or_else(|| CliError::msg(format!("provider {provider_id} has no adapter instance")))?;
    // ADR-0024 D2 / ADR-0025 D2: アカウントが決まっていれば、taskd の dispatch と同じように env の末尾に重ねる。
    let effective_adapter: std::sync::Arc<dyn WorkerAdapter> = match &account {
        Some(account_id) => {
            // B3: `resolve_account` は `account_pool` のプロバイダでしか `Some` を返さない（そのときは
            // `[accounts]` に対応する根ディレクトリが要る、と検証済み）はずだが、`.expect()` は使わず、
            // 万一の不整合はエラーとして返す。
            let account_adapter = task_core::AccountAdapter::parse(&adapter_kind).ok_or_else(|| {
                CliError::msg(format!("provider {provider_id} resolved an account but adapter {adapter_kind:?} is not a pool adapter"))
            })?;
            let accounts = config
                .accounts
                .as_ref()
                .ok_or_else(|| CliError::msg(format!("provider {provider_id} resolved an account but [accounts] is not configured")))?;
            let root = accounts.root_for(account_adapter).ok_or_else(|| {
                CliError::msg(format!("provider {provider_id} resolved an account but [accounts] has no root for adapter {adapter_kind}"))
            })?;
            let dir = root.join(account_id);
            base_adapter
                .with_env(&[(account_adapter.env_var().to_string(), dir.display().to_string())])
                .ok_or_else(|| CliError::msg(format!("adapter for provider {provider_id} does not support account pools")))?
        }
        None => base_adapter.clone(),
    };
    let adapter = effective_adapter.as_ref();
    let model = taskd::effective_models(&config).get(&provider_id).cloned().unwrap_or_default();

    let selected = Selected {
        provider_id,
        adapter_kind,
        model,
        account,
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
        return rt.block_on(execute_on_cluster(&task, &events, target, &config, &selected, adapter));
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
            WorkspaceSpec::Local { path, .. } if path.is_relative() => config.workspace_root.join(path),
            WorkspaceSpec::Local { path, .. } => path.clone(),
            WorkspaceSpec::Remote { .. } => {
                return Err(CliError::msg("task workspace is remote; pass --workspace to run it locally"));
            }
        },
    };

    rt.block_on(execute(&task, &events, workspace_dir, &config, &selected, adapter))
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
    // ADR-0036 D1: 成果物ディレクトリはタスクごと（共有 workspace の子は `.taskd/artifacts/<task_id>/`）。
    let artifacts_dir = task_core::artifacts::artifacts_dir_for(task, &prepared);
    let req = RunRequest {
        protocol: PROTOCOL_VERSION,
        task: task.clone(),
        workspace: prepared.clone(),
        // `taskctl worker run` は人が指定した（か DB の）ディレクトリでそのまま動かす（ADR-0041 D1 の worktree は
        // ディスパッチャが用意するもので、手動 run では切らない）。
        work_dir: None,
        artifacts_dir,
        context: RunContext {
            prior_review: to_prior_review(prior_review_from_events(events)),
            inputs: task.inputs.clone(),
            answers: to_answers(answers_from_events(events)),
            review: None,
            role: None,
            children: Vec::new(),
            available_genres: Vec::new(),
            // `taskctl worker run` は DB を変えない手動実行なので、役職・記憶・やり取り・組織図は渡さない
            // （ADR-0033 D4 / D6: 記憶の追記はディスパッチャの仕事）。
            ..RunContext::default()
        },
    };

    outln!(
        "worker run: task={} provider={} adapter={} model={} account={} workspace={} run_id={run_id}",
        task.id,
        selected.provider_id,
        selected.adapter_kind,
        selected.model,
        selected.account.as_deref().unwrap_or("-"),
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
    let settings = target.spec.ssh_settings(&target.remote_path, task.id);
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
    // ADR-0036 D1: Remote の写しは `workspace_root/<task_id>` でタスクごとなので `<写し>/artifacts`。
    let artifacts_dir = task_core::artifacts::artifacts_dir_for(&run_task, &prepared);
    let req = RunRequest {
        protocol: PROTOCOL_VERSION,
        task: run_task,
        workspace: prepared.clone(),
        // `taskctl worker run` は人が指定した（か DB の）ディレクトリでそのまま動かす（ADR-0041 D1 の worktree は
        // ディスパッチャが用意するもので、手動 run では切らない）。
        work_dir: None,
        artifacts_dir,
        context: RunContext {
            prior_review: to_prior_review(prior_review_from_events(events)),
            inputs: task.inputs.clone(),
            answers: to_answers(answers_from_events(events)),
            review: None,
            role: None,
            children: Vec::new(),
            available_genres: Vec::new(),
            // `taskctl worker run` は DB を変えない手動実行なので、役職・記憶・やり取り・組織図は渡さない
            // （ADR-0033 D4 / D6: 記憶の追記はディスパッチャの仕事）。
            ..RunContext::default()
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
            providers_include: None,
            providers_dir: None,
            clusters,
            roles: vec![],
            genres: vec![],
            conversation: None,
            org_include: None,
            org: vec![],
            delegation: Default::default(),
            reports: Default::default(),
            notify: Default::default(),
            accounts: None,
            secrets: None,
            memory: None,
            handoff: Default::default(),
            selfdeploy: Default::default(),
            workspace: Default::default(),
            source_path: None,
        }
    }

    fn cluster(id: &str, host: &str) -> taskd::config::ClusterConfig {
        taskd::config::ClusterConfig {
            id: id.into(),
            host: host.into(),
            concurrency: 1,
            sync: "rsync".into(),
            auth: "manual".into(),
            delete_on_push: false,
            setup: vec![],
            env: std::collections::HashMap::new(),
            rsync_excludes: vec![],
            worktree_root: None,
            worktree_base: "HEAD".into(),
            worktree_paths: vec![],
            remove_worktree_when: "never".into(),
        }
    }

    fn task_fixture(status: Status, workspace: WorkspaceSpec) -> Task {
        let now = time::OffsetDateTime::now_utc();
        Task {
            repos: Vec::new(),
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
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
        }
    }

    #[test]
    fn resolve_cluster_target_errors_when_cluster_missing() {
        let config = cluster_config(vec![]);
        let task = task_fixture(Status::Ready, WorkspaceSpec::Local { path: "/tmp/x".into(), mode: None });
        let err = resolve_cluster_target(&config, &task, "local", None).unwrap_err();
        assert!(err.to_string().contains("cluster not found in config: local"), "{err}");
    }

    #[test]
    fn resolve_cluster_target_requires_workspace_arg_for_local_task() {
        let config = cluster_config(vec![cluster("local", "h")]);
        let task = task_fixture(Status::Ready, WorkspaceSpec::Local { path: "/tmp/x".into(), mode: None });
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
        let task = task_fixture(Status::Ready, WorkspaceSpec::Local { path: "/tmp/x".into(), mode: None });
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
            let task = task_fixture(status, WorkspaceSpec::Local { path: "/tmp/x".into(), mode: None });
            let err = resolve_cluster_target(&config, &task, "local", Some(Path::new("/remote/x"))).unwrap_err();
            assert!(err.to_string().contains("stop taskd or wait"), "{err}");
        }
    }

    // ---- ADR-0024 D2: resolve_account ----

    fn args_fixture(account: Option<&str>) -> WorkerRunArgs {
        WorkerRunArgs {
            config: PathBuf::from("taskd.toml"),
            task: "01J000000000000000000000AA".into(),
            provider: None,
            adapter: None,
            workspace: None,
            cluster: None,
            account: account.map(str::to_string),
        }
    }

    fn pool_provider_config(accounts_dir: &Path, max_runs_per_account: usize) -> Config {
        let mut config = cluster_config(vec![]);
        config.providers = vec![taskd::config::ProviderConfig {
            id: "pool".into(),
            adapter: "claude-code".into(),
            tiers: vec![task_core::Tier::Standard],
            concurrency: 1,
            model: String::new(),
            env: Default::default(),
            env_from_secrets: Default::default(),
            account_pool: true,
            command: None,
            args: None,
            settings: None,
        }];
        config.accounts = Some(taskd::config::AccountsConfig {
            claude_dir: Some(accounts_dir.to_path_buf()),
            codex_dir: None,
            max_runs_per_account,
            check_model: "haiku".into(),
        });
        config
    }

    #[test]
    fn resolve_account_non_pool_provider_ignores_missing_account_and_rejects_explicit_one() {
        let config = cluster_config(vec![]);
        assert_eq!(resolve_account(&config, "p1", "claude-code", &args_fixture(None)).unwrap(), None);
        let err = resolve_account(&config, "p1", "claude-code", &args_fixture(Some("a"))).unwrap_err();
        assert!(err.to_string().contains("does not have account_pool"), "{err}");
    }

    /// ADR-0026 D5: acp はアカウントのプールを使わない（`Config::validate` が `account_pool = true` を
    /// claude-code/codex 以外で拒否しているので、acp プロバイダは常に `is_pool = false` になる）。
    /// `--account` 無しなら無視され、`--account` を付けたらエラーになる（他の非プールプロバイダと同じ扱い）。
    #[test]
    fn resolve_account_ignores_or_rejects_account_flag_for_acp_provider() {
        let mut config = cluster_config(vec![]);
        config.providers = vec![taskd::config::ProviderConfig {
            id: "opencode-qwen".into(),
            adapter: "acp".into(),
            tiers: vec![task_core::Tier::Standard],
            concurrency: 1,
            model: "qwen-local/qwen3.8-27b".into(),
            env: Default::default(),
            env_from_secrets: Default::default(),
            account_pool: false,
            command: None,
            args: None,
            settings: None,
        }];
        assert_eq!(resolve_account(&config, "opencode-qwen", "acp", &args_fixture(None)).unwrap(), None);
        let err = resolve_account(&config, "opencode-qwen", "acp", &args_fixture(Some("a"))).unwrap_err();
        assert!(err.to_string().contains("does not have account_pool"), "{err}");
    }

    /// ADR-0026 D2: `taskctl worker run` の `build_adapters` 経由でも acp プロバイダのアダプタが引ける
    /// （`select_provider` / `resolve_account` を通した後の配線が壊れていないことの確認）。
    #[test]
    fn build_adapters_resolves_an_instance_for_an_acp_provider_selected_by_worker_run() {
        let mut config = cluster_config(vec![]);
        config.providers = vec![taskd::config::ProviderConfig {
            id: "opencode-qwen".into(),
            adapter: "acp".into(),
            tiers: vec![task_core::Tier::Standard],
            concurrency: 1,
            model: "qwen-local/qwen3.8-27b".into(),
            env: Default::default(),
            env_from_secrets: Default::default(),
            account_pool: false,
            command: None,
            args: None,
            settings: None,
        }];
        let adapters = taskd::build_adapters(&config);
        let adapter = adapters.get("opencode-qwen").expect("acp provider has an adapter instance");
        assert_eq!(adapter.id(), "acp");
    }

    #[test]
    fn resolve_account_pool_provider_without_accounts_section_errors() {
        let mut config = cluster_config(vec![]);
        config.providers = vec![taskd::config::ProviderConfig {
            id: "pool".into(),
            adapter: "claude-code".into(),
            tiers: vec![task_core::Tier::Standard],
            concurrency: 1,
            model: String::new(),
            env: Default::default(),
            env_from_secrets: Default::default(),
            account_pool: true,
            command: None,
            args: None,
            settings: None,
        }];
        let err = resolve_account(&config, "pool", "claude-code", &args_fixture(None)).unwrap_err();
        assert!(err.to_string().contains("[accounts] is not configured"), "{err}");
    }

    #[test]
    fn resolve_account_explicit_flag_validates_the_id_and_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("b")).unwrap();
        let config = pool_provider_config(tmp.path(), 2);

        assert_eq!(resolve_account(&config, "pool", "claude-code", &args_fixture(Some("b"))).unwrap(), Some("b".into()));

        let err = resolve_account(&config, "pool", "claude-code", &args_fixture(Some("missing"))).unwrap_err();
        assert!(err.to_string().contains("account not found"), "{err}");

        let err = resolve_account(&config, "pool", "claude-code", &args_fixture(Some("../x"))).unwrap_err();
        assert!(err.to_string().contains("invalid --account id"), "{err}");
    }

    #[test]
    fn resolve_account_without_flag_picks_the_account_with_more_headroom_from_the_persisted_book() {
        let tmp = tempfile::tempdir().unwrap();
        for id in ["a", "b"] {
            std::fs::create_dir_all(tmp.path().join(id)).unwrap();
            std::fs::write(tmp.path().join(id).join(".credentials.json"), "{}").unwrap();
        }
        let mut book = task_dispatch::AccountBook::load(&tmp.path().join(".taskd-usage.json"));
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let window = |u: f64| task_core::RateLimitObservation {
            five_hour: Some(task_core::RateWindow { utilization: u, resets_at: now + 90_000 }),
            seven_day: None,
            status: None,
            resets_at: None,
            observed_at: now,
        };
        book.record_observation("a", window(0.9), task_dispatch::ObservationSource::Run);
        book.record_observation("b", window(0.1), task_dispatch::ObservationSource::Run);
        book.save().unwrap();

        let config = pool_provider_config(tmp.path(), 2);
        assert_eq!(resolve_account(&config, "pool", "claude-code", &args_fixture(None)).unwrap(), Some("b".into()));
    }

    #[test]
    fn resolve_account_no_eligible_account_errors() {
        let tmp = tempfile::tempdir().unwrap();
        // ディレクトリはあるがログインしていない（.credentials.json が無い）ので選べない。
        std::fs::create_dir_all(tmp.path().join("a")).unwrap();
        let config = pool_provider_config(tmp.path(), 2);
        let err = resolve_account(&config, "pool", "claude-code", &args_fixture(None)).unwrap_err();
        assert!(err.to_string().contains("no eligible account"), "{err}");
    }

    /// ADR-0025 D2: codex の `account_pool` プロバイダは `[accounts] codex_dir` の下から選び、`auth.json` を
    /// ログイン済みの目印にする（claude-code の `.credentials.json` とは別物）。
    #[test]
    fn resolve_account_codex_pool_provider_uses_codex_dir_and_auth_json_marker() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("c")).unwrap();
        std::fs::write(tmp.path().join("c").join("auth.json"), "{}").unwrap();
        let mut config = cluster_config(vec![]);
        config.providers = vec![taskd::config::ProviderConfig {
            id: "pool".into(),
            adapter: "codex".into(),
            tiers: vec![task_core::Tier::Standard],
            concurrency: 1,
            model: String::new(),
            env: Default::default(),
            env_from_secrets: Default::default(),
            account_pool: true,
            command: None,
            args: None,
            settings: None,
        }];
        config.accounts = Some(taskd::config::AccountsConfig {
            claude_dir: None,
            codex_dir: Some(tmp.path().to_path_buf()),
            max_runs_per_account: 2,
            check_model: "haiku".into(),
        });

        assert_eq!(resolve_account(&config, "pool", "codex", &args_fixture(Some("c"))).unwrap(), Some("c".into()));
        assert_eq!(resolve_account(&config, "pool", "codex", &args_fixture(None)).unwrap(), Some("c".into()));

        let err = resolve_account(&config, "pool", "claude-code", &args_fixture(None)).unwrap_err();
        assert!(err.to_string().contains("not a pool adapter") || err.to_string().contains("no root configured"), "{err}");
    }
}
