//! 決定的ディスパッチャ（DESIGN §5.2, ADR-0005 D4–D6）。
//!
//! 1 tick の手順:
//! 1. 終了したワーカー／レビューの結果を取り込み、状態遷移をストアに書く
//! 2. 期限切れリースを回収（`running → ready|failed`、`LeaseExpired`）
//! 3. 自分が起動した run のうち、ストア上で既に `running` でない／run_id が変わったものを強制終了（cancel 等）
//! 4. `reviewing` なのに判定中でないタスクのレビューを開始（再起動後の復旧）
//! 5. `ready_tasks` を `priority DESC, created_at ASC` で取り、`ProviderPolicy` と並列度上限に従って dispatch
//!
//! **LLM 呼び出しはここに書かない。** 判断は全て設定・状態機械・ストアのクエリで決まる。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use task_core::{ArtifactRef, Event, Status, StoreError, Task, TaskId, TaskStore, Trigger, WorkspaceSpec};
use task_worker::{
    AdapterError, EventSink, LocalWorkspace, PROTOCOL_VERSION, PriorReview, RunContext, RunLimits,
    RunOutcome, RunRequest, Terminal, Workspace, WorkerAdapter,
};
use time::OffsetDateTime;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::policy::{AdapterId, ProviderId, ProviderOutcome, ProviderPolicy};
use crate::review::{Verdict, review_task};

/// ディスパッチャの設定（`taskd.toml` から組み立てる。ADR-0005 D7）。
#[derive(Debug, Clone)]
pub struct DispatchConfig {
    /// 全体の並列度上限。
    pub max_concurrency: usize,
    /// ADR-0002 D7: リース ttl = `max_wall_secs` + この猶予。
    pub lease_grace: Duration,
    /// ADR-0003 D4。
    pub idle_timeout: Duration,
    /// ADR-0003 D4。
    pub kill_grace: Duration,
    /// `Command` チェック 1 件あたりの上限。
    pub review_timeout: Duration,
    /// `WorkspaceSpec::Local` の相対パスの基準。
    pub workspace_root: PathBuf,
}

/// 1 tick の要約（ログとテスト用）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub reclaimed: usize,
    pub dispatched: usize,
    pub finished: usize,
    pub reviewed: usize,
    pub in_flight: usize,
    /// 実行中／判定中が無く、`ready_tasks` も空で、DB に `running`/`reviewing` が無い。
    pub idle: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error(transparent)]
    Store(#[from] StoreError),
}

enum Completion {
    Worker {
        task_id: TaskId,
        run_id: String,
        provider: ProviderId,
        result: Result<RunOutcome, AdapterError>,
    },
    Review {
        task_id: TaskId,
        run_id: String,
        verdicts: Vec<Verdict>,
    },
}

struct RunEntry {
    run_id: String,
    provider: ProviderId,
    handle: JoinHandle<()>,
}

/// run 途中のイベントをストアに追記するシンク。
struct StoreSink {
    store: Arc<dyn TaskStore>,
    task_id: TaskId,
    run_id: String,
}

impl EventSink for StoreSink {
    fn progress(&self, msg: &str) {
        let ev = Event::WorkerProgress {
            run_id: self.run_id.clone(),
            msg: msg.to_string(),
        };
        if let Err(e) = self.store.append_event(self.task_id, &ev) {
            tracing::warn!(task_id = %self.task_id, error = %e, "failed to record progress");
        }
    }

    fn artifact(&self, artifact: &ArtifactRef) {
        let ev = Event::ArtifactProduced {
            run_id: self.run_id.clone(),
            artifact: artifact.clone(),
        };
        if let Err(e) = self.store.append_event(self.task_id, &ev) {
            tracing::warn!(task_id = %self.task_id, error = %e, "failed to record artifact");
        }
    }
}

pub struct Dispatcher {
    store: Arc<dyn TaskStore>,
    policy: Box<dyn ProviderPolicy>,
    models: HashMap<ProviderId, String>,
    adapters: HashMap<AdapterId, Arc<dyn WorkerAdapter>>,
    config: DispatchConfig,
    running: HashMap<TaskId, RunEntry>,
    reviewing: HashMap<TaskId, JoinHandle<()>>,
    tx: mpsc::UnboundedSender<Completion>,
    rx: mpsc::UnboundedReceiver<Completion>,
}

impl Dispatcher {
    /// `models` は provider id → `WorkerStarted.model` に記録するモデル名。
    pub fn new(
        store: Arc<dyn TaskStore>,
        policy: Box<dyn ProviderPolicy>,
        models: HashMap<ProviderId, String>,
        adapters: HashMap<AdapterId, Arc<dyn WorkerAdapter>>,
        config: DispatchConfig,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            store,
            policy,
            models,
            adapters,
            config,
            running: HashMap::new(),
            reviewing: HashMap::new(),
            tx,
            rx,
        }
    }

    pub fn config(&self) -> &DispatchConfig {
        &self.config
    }

    /// 1 tick。tokio ランタイム内から呼ぶ（ワーカーとレビューを `tokio::spawn` する）。
    pub fn tick(&mut self) -> Result<TickReport, DispatchError> {
        let mut report = TickReport::default();
        let (finished, reviewed) = self.drain_completions()?;
        report.finished = finished;
        report.reviewed = reviewed;
        report.reclaimed = self.reclaim_expired_leases()?;
        self.abort_stale_runs()?;
        self.recover_reviews()?;
        report.dispatched = self.dispatch_ready()?;
        report.in_flight = self.running.len() + self.reviewing.len();
        report.idle = self.is_idle()?;
        Ok(report)
    }

    fn drain_completions(&mut self) -> Result<(usize, usize), DispatchError> {
        let mut finished = 0;
        let mut reviewed = 0;
        while let Ok(c) = self.rx.try_recv() {
            match c {
                Completion::Worker {
                    task_id,
                    run_id,
                    provider,
                    result,
                } => {
                    self.on_worker_finished(task_id, run_id, provider, result)?;
                    finished += 1;
                }
                Completion::Review {
                    task_id,
                    run_id,
                    verdicts,
                } => {
                    self.on_review_finished(task_id, run_id, verdicts)?;
                    reviewed += 1;
                }
            }
        }
        Ok((finished, reviewed))
    }

    fn on_worker_finished(
        &mut self,
        task_id: TaskId,
        run_id: String,
        provider: ProviderId,
        result: Result<RunOutcome, AdapterError>,
    ) -> Result<(), DispatchError> {
        self.running.remove(&task_id);
        let Some(task) = self.store.get(task_id)? else {
            tracing::warn!(%task_id, %run_id, "worker finished for unknown task");
            return Ok(());
        };
        let lease_matches = task.status == Status::Running
            && task.lease.as_ref().map(|l| l.worker_run_id.as_str()) == Some(run_id.as_str());
        if !lease_matches {
            // ADR-0002 D9 / ADR-0005 D4: リース回収済み・cancel 済みの古い結果は捨てる。
            tracing::warn!(%task_id, %run_id, status = ?task.status, "stale worker result discarded");
            return Ok(());
        }

        let (trigger, outcome_str, usage, provider_outcome) = match result {
            Ok(RunOutcome {
                terminal: Terminal::Done { summary, usage, .. },
                ..
            }) => (
                Trigger::WorkerDone,
                format!("done: {summary}"),
                usage,
                ProviderOutcome::Ok,
            ),
            Ok(RunOutcome {
                terminal: Terminal::Question { text },
                ..
            }) => (
                Trigger::WorkerQuestion,
                format!("question: {text}"),
                None,
                ProviderOutcome::Ok,
            ),
            Ok(RunOutcome {
                terminal: Terminal::Error { message, retryable },
                ..
            }) => (
                Trigger::WorkerError { retryable },
                format!("error(retryable={retryable}): {message}"),
                None,
                ProviderOutcome::Ok,
            ),
            Err(e) => {
                let po = match &e {
                    AdapterError::Throttled { retry_after } => ProviderOutcome::Throttled {
                        retry_after: *retry_after,
                    },
                    AdapterError::AuthFailed(_) => ProviderOutcome::AuthFailed,
                    AdapterError::Exhausted(_) => ProviderOutcome::Exhausted,
                    _ => ProviderOutcome::Ok,
                };
                (
                    Trigger::WorkerError { retryable: true },
                    format!("error(retryable=true): adapter: {e}"),
                    None,
                    po,
                )
            }
        };
        self.policy.report(provider, &provider_outcome);

        let finished = Event::WorkerFinished {
            run_id: run_id.clone(),
            outcome: outcome_str.clone(),
            usage,
        };
        match self
            .store
            .apply_transition_with_events(task_id, trigger, vec![finished])
        {
            Ok(outcome) => {
                tracing::info!(%task_id, %run_id, next = ?outcome.next, attempts = outcome.attempts, outcome = %outcome_str, "worker finished");
                if outcome.next == Status::Reviewing {
                    self.spawn_review(task_id, run_id)?;
                }
            }
            Err(StoreError::InvalidTransition(e)) => {
                tracing::warn!(%task_id, %run_id, error = %e, "worker result could not be applied");
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    fn on_review_finished(
        &mut self,
        task_id: TaskId,
        run_id: String,
        verdicts: Vec<Verdict>,
    ) -> Result<(), DispatchError> {
        self.reviewing.remove(&task_id);
        let Some(task) = self.store.get(task_id)? else {
            return Ok(());
        };
        if task.status != Status::Reviewing {
            tracing::warn!(%task_id, status = ?task.status, "review result discarded (task no longer reviewing)");
            return Ok(());
        }
        let all_pass = verdicts.iter().all(|v| v.pass);
        let trigger = if all_pass {
            Trigger::ReviewPass
        } else {
            Trigger::ReviewFail
        };
        let events: Vec<Event> = verdicts
            .iter()
            .map(|v| Event::ReviewVerdict {
                run_id: run_id.clone(),
                criterion_idx: v.criterion_idx,
                pass: v.pass,
                reason: v.reason.clone(),
            })
            .collect();
        match self.store.apply_transition_with_events(task_id, trigger, events) {
            Ok(outcome) => {
                tracing::info!(%task_id, %run_id, all_pass, next = ?outcome.next, attempts = outcome.attempts, "review finished");
            }
            Err(StoreError::InvalidTransition(e)) => {
                tracing::warn!(%task_id, error = %e, "review result could not be applied");
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    fn reclaim_expired_leases(&mut self) -> Result<usize, DispatchError> {
        let now = OffsetDateTime::now_utc();
        let mut count = 0;
        for task in self.store.list(Some(Status::Running))? {
            let Some(lease) = &task.lease else { continue };
            if lease.expires_at > now {
                continue;
            }
            if let Some(entry) = self.running.remove(&task.id) {
                entry.handle.abort();
            }
            let finished = Event::WorkerFinished {
                run_id: lease.worker_run_id.clone(),
                outcome: "lease_expired".to_string(),
                usage: None,
            };
            match self
                .store
                .apply_transition_with_events(task.id, Trigger::LeaseExpired, vec![finished])
            {
                Ok(outcome) => {
                    tracing::warn!(task_id = %task.id, run_id = %lease.worker_run_id, next = ?outcome.next, attempts = outcome.attempts, "lease expired; reclaimed");
                    count += 1;
                }
                Err(StoreError::InvalidTransition(e)) => {
                    tracing::warn!(task_id = %task.id, error = %e, "lease reclaim skipped");
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(count)
    }

    /// ADR-0002 D9: ストア上で `running` でなくなった（cancel 等）run を強制終了する。
    fn abort_stale_runs(&mut self) -> Result<(), DispatchError> {
        let ids: Vec<TaskId> = self.running.keys().copied().collect();
        for id in ids {
            let current = self.store.get(id)?;
            let still_ours = match (&current, self.running.get(&id)) {
                (Some(t), Some(entry)) => {
                    t.status == Status::Running
                        && t.lease.as_ref().map(|l| l.worker_run_id.as_str())
                            == Some(entry.run_id.as_str())
                }
                _ => false,
            };
            if !still_ours && let Some(entry) = self.running.remove(&id) {
                tracing::warn!(task_id = %id, run_id = %entry.run_id, "aborting run (task no longer running under this lease)");
                entry.handle.abort();
            }
        }
        Ok(())
    }

    fn recover_reviews(&mut self) -> Result<(), DispatchError> {
        for task in self.store.list(Some(Status::Reviewing))? {
            if self.reviewing.contains_key(&task.id) {
                continue;
            }
            let events = self.store.events_for(task.id)?;
            let run_id = last_run_id(&events).unwrap_or_default();
            self.spawn_review(task.id, run_id)?;
        }
        Ok(())
    }

    fn dispatch_ready(&mut self) -> Result<usize, DispatchError> {
        if self.running.len() >= self.config.max_concurrency {
            return Ok(0);
        }
        // 上位から見て見送りが続いても後続を試せるよう、窓は広めに取る。
        let window = self.config.max_concurrency * 4 + 16;
        let candidates = self.store.ready_tasks(window)?;
        let now = Instant::now();
        let mut dispatched = 0;
        for task in candidates {
            if self.running.len() >= self.config.max_concurrency {
                break;
            }
            if self.running.contains_key(&task.id) {
                continue;
            }
            let Some(dir) = self.task_dir(&task) else {
                tracing::warn!(task_id = %task.id, "remote workspace is not supported; task left ready");
                continue;
            };
            let Some((adapter_id, provider_id)) = self.policy.pick(&task.worker_hint, now) else {
                tracing::debug!(task_id = %task.id, hint = ?task.worker_hint, "no provider available");
                continue;
            };
            let Some(adapter) = self.adapters.get(&adapter_id).cloned() else {
                tracing::warn!(task_id = %task.id, adapter = %adapter_id, "adapter not configured");
                continue;
            };
            let limit = self.policy.concurrency_limit(provider_id.clone());
            let used = self
                .running
                .values()
                .filter(|e| e.provider == provider_id)
                .count();
            if used >= limit {
                tracing::debug!(task_id = %task.id, provider = %provider_id, used, limit, "provider at capacity");
                continue;
            }

            let run_id = ulid::Ulid::new().to_string();
            let wall = Duration::from_secs(task.budget.max_wall_secs);
            let ttl = wall + self.config.lease_grace;
            if !self.store.acquire_lease(task.id, &run_id, ttl)? {
                continue;
            }
            let model = self.models.get(&provider_id).cloned().unwrap_or_default();
            self.store.append_event(
                task.id,
                &Event::WorkerStarted {
                    run_id: run_id.clone(),
                    adapter: adapter_id.clone(),
                    model,
                },
            )?;
            let limits = RunLimits {
                wall_clock: wall,
                idle_timeout: self.config.idle_timeout,
                kill_grace: self.config.kill_grace,
            };
            tracing::info!(task_id = %task.id, %run_id, adapter = %adapter_id, provider = %provider_id, "dispatching");
            let handle = self.spawn_worker(task.id, run_id.clone(), provider_id.clone(), adapter, dir, limits);
            self.running.insert(
                task.id,
                RunEntry {
                    run_id,
                    provider: provider_id,
                    handle,
                },
            );
            dispatched += 1;
        }
        Ok(dispatched)
    }

    fn spawn_worker(
        &self,
        task_id: TaskId,
        run_id: String,
        provider: ProviderId,
        adapter: Arc<dyn WorkerAdapter>,
        dir: PathBuf,
        limits: RunLimits,
    ) -> JoinHandle<()> {
        let store = self.store.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = run_worker(store, adapter, task_id, dir, &run_id, limits).await;
            let _ = tx.send(Completion::Worker {
                task_id,
                run_id,
                provider,
                result,
            });
        })
    }

    fn spawn_review(&mut self, task_id: TaskId, run_id: String) -> Result<(), DispatchError> {
        let Some(task) = self.store.get(task_id)? else {
            return Ok(());
        };
        let Some(dir) = self.task_dir(&task) else {
            tracing::warn!(%task_id, "cannot review task with remote workspace");
            return Ok(());
        };
        let events = self.store.events_for(task_id)?;
        let produced = artifacts_for_run(&events, &run_id);
        let timeout = self.config.review_timeout;
        let tx = self.tx.clone();
        let handle = tokio::spawn(async move {
            let ws = LocalWorkspace::new(&dir);
            let verdicts = review_task(&task, &ws, &dir, &produced, timeout).await;
            let _ = tx.send(Completion::Review {
                task_id,
                run_id,
                verdicts,
            });
        });
        self.reviewing.insert(task_id, handle);
        Ok(())
    }

    /// ADR-0005 D3: `Local{path}` がそのタスクの作業ディレクトリ。相対なら `workspace_root` 基準。
    fn task_dir(&self, task: &Task) -> Option<PathBuf> {
        match &task.workspace {
            WorkspaceSpec::Local { path } => Some(if path.is_absolute() {
                path.clone()
            } else {
                self.config.workspace_root.join(path)
            }),
            WorkspaceSpec::Remote { .. } => None,
        }
    }

    fn is_idle(&self) -> Result<bool, DispatchError> {
        if !self.running.is_empty() || !self.reviewing.is_empty() {
            return Ok(false);
        }
        if !self.store.list(Some(Status::Running))?.is_empty() {
            return Ok(false);
        }
        if !self.store.list(Some(Status::Reviewing))?.is_empty() {
            return Ok(false);
        }
        Ok(self.store.ready_tasks(1)?.is_empty())
    }
}

async fn run_worker(
    store: Arc<dyn TaskStore>,
    adapter: Arc<dyn WorkerAdapter>,
    task_id: TaskId,
    dir: PathBuf,
    run_id: &str,
    limits: RunLimits,
) -> Result<RunOutcome, AdapterError> {
    // リース取得後の状態（running, lease あり）をワーカーに渡す。
    let task = store
        .get(task_id)
        .map_err(|e| AdapterError::Other(format!("store: {e}")))?
        .ok_or_else(|| AdapterError::Other("task vanished".into()))?;
    let ws = LocalWorkspace::new(&dir);
    let workspace = ws
        .prepare(&task)
        .await
        .map_err(|e| AdapterError::Other(format!("workspace prepare: {e}")))?;
    let events = store
        .events_for(task_id)
        .map_err(|e| AdapterError::Other(format!("store: {e}")))?;
    let prior_review = prior_review_from_events(&events);
    let req = RunRequest {
        protocol: PROTOCOL_VERSION,
        task: task.clone(),
        workspace,
        context: RunContext {
            prior_review,
            inputs: task.inputs.clone(),
        },
    };
    let sink = StoreSink {
        store,
        task_id,
        run_id: run_id.to_string(),
    };
    adapter.run(req, run_id, limits, &sink).await
}

/// 直前のレビュー（最後に `ReviewVerdict` を記録した run の全判定）を `context.prior_review` に写す。
pub fn prior_review_from_events(events: &[(u64, Event)]) -> Vec<PriorReview> {
    let mut by_run: HashMap<&str, Vec<PriorReview>> = HashMap::new();
    let mut last_run: Option<&str> = None;
    for (_, ev) in events {
        if let Event::ReviewVerdict {
            run_id,
            criterion_idx,
            pass,
            reason,
        } = ev
        {
            by_run.entry(run_id.as_str()).or_default().push(PriorReview {
                criterion: *criterion_idx,
                pass: *pass,
                reason: reason.clone(),
            });
            last_run = Some(run_id.as_str());
        }
    }
    let mut out = last_run
        .and_then(|r| by_run.remove(r))
        .unwrap_or_default();
    out.sort_by_key(|p| p.criterion);
    out
}

/// その run で `ArtifactProduced` された成果物。
pub fn artifacts_for_run(events: &[(u64, Event)], run_id: &str) -> Vec<ArtifactRef> {
    events
        .iter()
        .filter_map(|(_, ev)| match ev {
            Event::ArtifactProduced { run_id: r, artifact } if r == run_id => Some(artifact.clone()),
            _ => None,
        })
        .collect()
}

/// 最後に `WorkerStarted` した run の id。
pub fn last_run_id(events: &[(u64, Event)]) -> Option<String> {
    events.iter().rev().find_map(|(_, ev)| match ev {
        Event::WorkerStarted { run_id, .. } => Some(run_id.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ProviderSpec, StaticPolicy};
    use async_trait::async_trait;
    use task_core::*;
    use task_worker::Evidence;

    /// 同プロセスで即座に終端を返すテスト用アダプタ（サブプロセスは起動しない）。
    struct InstantAdapter {
        terminal: Terminal,
        delay: Duration,
    }

    #[async_trait]
    impl WorkerAdapter for InstantAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            sink.progress("working");
            std::fs::write(req.workspace.join("touched"), "1").unwrap();
            tokio::time::sleep(self.delay).await;
            Ok(RunOutcome {
                terminal: self.terminal.clone(),
                exit_code: Some(0),
            })
        }
    }

    fn new_task(dir: &std::path::Path, check: Check, max_retries: u32) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion { text: "c".into(), check }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: 0,
            worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
            workspace: WorkspaceSpec::Local { path: dir.to_path_buf() },
            budget: Budget { max_turns: 1, max_wall_secs: 30, max_retries },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn dispatcher(store: Arc<dyn TaskStore>, adapter: Arc<dyn WorkerAdapter>, max_concurrency: usize) -> Dispatcher {
        let policy = StaticPolicy::new(
            vec![ProviderSpec {
                id: "p1".into(),
                adapter: "instant".into(),
                tiers: vec![Tier::Frontier, Tier::Standard, Tier::Cheap],
                concurrency: max_concurrency,
                model: "m".into(),
            }],
            Duration::from_secs(1),
        );
        let mut adapters: HashMap<AdapterId, Arc<dyn WorkerAdapter>> = HashMap::new();
        adapters.insert("instant".into(), adapter);
        Dispatcher::new(
            store,
            Box::new(policy),
            HashMap::from([("p1".to_string(), "m".to_string())]),
            adapters,
            DispatchConfig {
                max_concurrency,
                lease_grace: Duration::from_secs(60),
                idle_timeout: Duration::from_secs(5),
                kill_grace: Duration::from_millis(100),
                review_timeout: Duration::from_secs(5),
                workspace_root: PathBuf::from("/nonexistent"),
            },
        )
    }

    async fn run_until_idle(d: &mut Dispatcher, max_ticks: usize) -> TickReport {
        let mut last = TickReport::default();
        for _ in 0..max_ticks {
            last = d.tick().unwrap();
            if last.idle {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        last
    }

    #[tokio::test]
    async fn done_then_command_review_passes_and_events_are_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Command { cmd: "test -f touched".into(), expect_exit: 0 }, 0);
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![Evidence { criterion: 0, command: "x".into(), exit: 0, stdout_tail: String::new() }], usage: None },
            delay: Duration::from_millis(10),
        });
        let mut d = dispatcher(store.clone(), adapter, 2);
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Done);
        assert_eq!(t.attempts, 0);
        assert!(t.lease.is_none());
        let kinds: Vec<String> = store
            .events_for(task.id)
            .unwrap()
            .into_iter()
            .map(|(_, e)| match e {
                Event::Transitioned { from, to, reason } => format!("{from:?}->{to:?}:{reason}"),
                Event::WorkerStarted { .. } => "started".into(),
                Event::WorkerProgress { .. } => "progress".into(),
                Event::WorkerFinished { outcome, .. } => format!("finished:{outcome}"),
                Event::ReviewVerdict { pass, .. } => format!("verdict:{pass}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "Ready->Running:dispatch",
                "started",
                "progress",
                "Running->Reviewing:worker_done",
                "finished:done: ok",
                "Reviewing->Done:review_pass",
                "verdict:true",
            ]
        );
    }

    #[tokio::test]
    async fn question_blocks_task_and_review_fail_retries_until_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let q = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        store.insert(&q).unwrap();
        let adapter = Arc::new(InstantAdapter { terminal: Terminal::Question { text: "which?".into() }, delay: Duration::ZERO });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let report = run_until_idle(&mut d, 100).await;
        assert!(report.idle);
        assert_eq!(store.get(q.id).unwrap().unwrap().status, Status::Blocked);

        // レビュー失敗（存在しないファイル）は max_retries=1 で 2 回実行して failed。
        let dir2 = tempfile::tempdir().unwrap();
        let store2: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let f = new_task(dir2.path(), Check::Command { cmd: "test -f never".into(), expect_exit: 0 }, 1);
        store2.insert(&f).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "claimed".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d2 = dispatcher(store2.clone(), adapter, 1);
        let report = run_until_idle(&mut d2, 200).await;
        assert!(report.idle);
        let t = store2.get(f.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Failed);
        assert_eq!(t.attempts, 2);
        let events = store2.events_for(f.id).unwrap();
        let starts = events.iter().filter(|(_, e)| matches!(e, Event::WorkerStarted { .. })).count();
        assert_eq!(starts, 2);
        // 2 回目の run には 1 回目のレビュー結果が prior_review として渡る。
        let prior = prior_review_from_events(&events[..events.len() - 2]);
        assert_eq!(prior.len(), 1);
        assert!(!prior[0].pass);
    }

    #[tokio::test]
    async fn concurrency_limit_is_respected() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        for _ in 0..3 {
            store.insert(&new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0)).unwrap();
        }
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::from_millis(200),
        });
        let mut d = dispatcher(store.clone(), adapter, 2);
        let first = d.tick().unwrap();
        assert_eq!(first.dispatched, 2);
        assert_eq!(store.list(Some(Status::Running)).unwrap().len(), 2);
        let second = d.tick().unwrap();
        assert_eq!(second.dispatched, 0);
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.list(Some(Status::Done)).unwrap().len(), 3);
    }
}
