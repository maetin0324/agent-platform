//! 決定的ディスパッチャ（DESIGN §5.2, ADR-0005 D4–D6）。
//!
//! 1 tick の手順:
//! 1. 終了したワーカー／レビューの結果を取り込み、状態遷移をストアに書く
//! 2. 期限切れリースを回収（`running → ready|failed`、`LeaseExpired`）
//! 3. 自分が起動した run のうち、ストア上で既に `running` でない／run_id が変わったものを強制終了（cancel 等）
//! 4. `reviewing` なのに判定中でないタスクのレビューを開始（再起動後の復旧、または前 tick で `Reviewer` run の
//!    枠が無く見送ったもの）
//! 5. `ready_tasks` を `priority DESC, created_at ASC` で取り、`ProviderPolicy` と並列度上限に従って dispatch
//!
//! Phase 5（ADR-0007）: `Reviewer` 条件を持つタスクのレビューは、`Standard` tier のプロバイダをここで選び
//! （並列度の枠も実行中 run と共有する）、`review.rs` がアダプタ経由で別 run を起動する。`Plan` kind の
//! レビューが通れば `TaskStore::complete_plan` で子タスクを挿入する。
//!
//! **LLM 呼び出しはここに書かない。** 判断は全て設定・状態機械・ストアのクエリで決まる。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use task_core::plan::{PlanLimits, materialize};
use task_core::{
    ArtifactRef, Check, Event, Status, StoreError, Task, TaskId, TaskKind, TaskStore, Trigger, WorkspaceSpec,
};
use task_worker::{
    AdapterError, EventSink, LocalWorkspace, PROTOCOL_VERSION, PriorReview, RunContext, RunLimits, RunOutcome,
    RunRequest, Terminal, WorkerMessage, Workspace, WorkerAdapter,
};
use time::OffsetDateTime;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::policy::{AdapterId, ProviderId, ProviderOutcome, ProviderPolicy};
use crate::review::{
    HumanVerdicts, PLAN_FILE, PlanCheck, ReviewExtras, ReviewOutcome, ReviewSubject, ReviewerRun, needs_reviewer_run,
    review_task, reviewer_hint,
};

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
    /// DESIGN §4.2 `plan.auto_accept`: Plan の子を `draft` のまま置く（false）か、親 `done` と同一トランザクションで
    /// `ready` にする（true）か（ADR-0002 D6, ADR-0007 D3）。
    pub plan_auto_accept: bool,
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
        outcome: ReviewOutcome,
    },
}

struct RunEntry {
    run_id: String,
    provider: ProviderId,
    handle: JoinHandle<()>,
}

struct ReviewEntry {
    handle: JoinHandle<()>,
    /// `Reviewer` run を起動する場合に選んだプロバイダ（並列度の枠を消費する）。
    provider: Option<ProviderId>,
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

/// `Reviewer` run のシンク（ADR-0007 D5 6.）。進捗は対象 run の `WorkerProgress` に
/// `reviewer run <review_run_id>: ` を付けて記録し、レビュー run の成果物は記録しない
/// （`artifacts_for_run` が対象 run の成果物だけを返すようにするため）。
struct ReviewerSink {
    store: Arc<dyn TaskStore>,
    task_id: TaskId,
    subject_run_id: String,
    review_run_id: String,
}

impl EventSink for ReviewerSink {
    fn progress(&self, msg: &str) {
        let ev = Event::WorkerProgress {
            run_id: self.subject_run_id.clone(),
            msg: format!("reviewer run {}: {msg}", self.review_run_id),
        };
        if let Err(e) = self.store.append_event(self.task_id, &ev) {
            tracing::warn!(task_id = %self.task_id, error = %e, "failed to record reviewer progress");
        }
    }

    fn artifact(&self, artifact: &ArtifactRef) {
        tracing::debug!(task_id = %self.task_id, review_run_id = %self.review_run_id, name = %artifact.name, "reviewer run artifact ignored");
    }
}

pub struct Dispatcher {
    store: Arc<dyn TaskStore>,
    policy: Box<dyn ProviderPolicy>,
    models: HashMap<ProviderId, String>,
    adapters: HashMap<AdapterId, Arc<dyn WorkerAdapter>>,
    config: DispatchConfig,
    running: HashMap<TaskId, RunEntry>,
    reviewing: HashMap<TaskId, ReviewEntry>,
    /// レビューを開始できなかった（`Reviewer` run の枠が無い）タスクの `done` 内容。次 tick で使う。
    pending_subjects: HashMap<TaskId, ReviewSubject>,
    /// 「`Standard` tier を提供するプロバイダが無い」警告を出した（連続 tick で繰り返さない）タスク。
    warned_no_reviewer: std::collections::HashSet<TaskId>,
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
            pending_subjects: HashMap::new(),
            warned_no_reviewer: std::collections::HashSet::new(),
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
                    outcome,
                } => {
                    self.on_review_finished(task_id, run_id, outcome)?;
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

        let mut subject = ReviewSubject::default();
        let (trigger, outcome_str, usage, provider_outcome) = match result {
            Ok(RunOutcome {
                terminal: Terminal::Done { summary, usage, evidence },
                ..
            }) => {
                subject = ReviewSubject {
                    summary: summary.clone(),
                    evidence,
                };
                (
                    Trigger::WorkerDone,
                    format!("done: {summary}"),
                    usage,
                    ProviderOutcome::Ok,
                )
            }
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
                if outcome.next == Status::Reviewing && !self.spawn_review(task_id, run_id, &subject)? {
                    // Reviewer run の枠が無い: 次 tick の recover_reviews で再試行する。
                    self.pending_subjects.insert(task_id, subject);
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
        outcome: ReviewOutcome,
    ) -> Result<(), DispatchError> {
        self.reviewing.remove(&task_id);
        let Some(task) = self.store.get(task_id)? else {
            return Ok(());
        };
        if task.status != Status::Reviewing {
            tracing::warn!(%task_id, status = ?task.status, "review result discarded (task no longer reviewing)");
            return Ok(());
        }
        let all_pass = outcome.all_pass();
        let events: Vec<Event> = outcome
            .verdicts
            .iter()
            .map(|v| Event::ReviewVerdict {
                run_id: run_id.clone(),
                criterion_idx: v.criterion_idx,
                pass: v.pass,
                reason: v.reason.clone(),
            })
            .collect();
        // ADR-0007 D3/D4: Plan が全 pass なら子タスクの挿入と ReviewPass を同一トランザクションで行う。
        let result = match (all_pass, task.kind, outcome.plan) {
            (true, TaskKind::Plan, Some(plan)) => {
                let children = materialize(&task, &plan, OffsetDateTime::now_utc());
                let n = children.len();
                let r = self
                    .store
                    .complete_plan(task_id, events, children, self.config.plan_auto_accept);
                if r.is_ok() {
                    tracing::info!(%task_id, %run_id, children = n, auto_accept = self.config.plan_auto_accept, "plan completed; children inserted");
                }
                r
            }
            (true, TaskKind::Plan, None) => {
                // review_task は Plan kind に必ず暗黙の判定を付けるので、ここには来ないはず。
                tracing::error!(%task_id, "plan review passed without a parsed plan; treating as failure");
                self.store
                    .apply_transition_with_events(task_id, Trigger::ReviewFail, events)
            }
            (true, _, _) => self
                .store
                .apply_transition_with_events(task_id, Trigger::ReviewPass, events),
            (false, _, _) => self
                .store
                .apply_transition_with_events(task_id, Trigger::ReviewFail, events),
        };
        match result {
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
        // レビュー中に cancel されたタスクの判定（Reviewer run を含む）も中断する。
        let ids: Vec<TaskId> = self.reviewing.keys().copied().collect();
        for id in ids {
            let still_reviewing = matches!(self.store.get(id)?, Some(t) if t.status == Status::Reviewing);
            if !still_reviewing && let Some(entry) = self.reviewing.remove(&id) {
                tracing::warn!(task_id = %id, "aborting review (task no longer reviewing)");
                entry.handle.abort();
                self.pending_subjects.remove(&id);
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
            // 前 tick で見送った場合はメモリ上の done 内容、再起動後は runs/<run_id>/result.json から復元。
            let subject = match self.pending_subjects.remove(&task.id) {
                Some(s) => s,
                None => self
                    .task_dir(&task)
                    .map(|dir| subject_from_run_dir(&dir, &run_id))
                    .unwrap_or_default(),
            };
            if !self.spawn_review(task.id, run_id, &subject)? {
                self.pending_subjects.insert(task.id, subject);
            }
        }
        Ok(())
    }

    /// 実行中の run と、プロバイダを使っているレビュー run の合計（並列度の分母）。
    fn workers_in_flight(&self) -> usize {
        self.running.len() + self.reviewing.values().filter(|e| e.provider.is_some()).count()
    }

    fn provider_in_use(&self, provider: &ProviderId) -> usize {
        self.running.values().filter(|e| &e.provider == provider).count()
            + self
                .reviewing
                .values()
                .filter(|e| e.provider.as_ref() == Some(provider))
                .count()
    }

    /// ADR-0007 D2: その Plan 自身を含む祖先 Plan の数。
    fn plan_depth(&self, task: &Task) -> Result<u32, DispatchError> {
        let mut depth = 0;
        let mut current = Some(task.clone());
        let mut hops = 0;
        while let Some(t) = current {
            if t.kind == TaskKind::Plan {
                depth += 1;
            }
            hops += 1;
            if hops > 64 {
                break;
            }
            current = match t.parent_id {
                Some(p) => self.store.get(p)?,
                None => None,
            };
        }
        Ok(depth)
    }

    fn dispatch_ready(&mut self) -> Result<usize, DispatchError> {
        if self.workers_in_flight() >= self.config.max_concurrency {
            return Ok(0);
        }
        // 上位から見て見送りが続いても後続を試せるよう、窓は広めに取る。
        let window = self.config.max_concurrency * 4 + 16;
        let candidates = self.store.ready_tasks(window)?;
        let now = Instant::now();
        let mut dispatched = 0;
        for task in candidates {
            if self.workers_in_flight() >= self.config.max_concurrency {
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
            let used = self.provider_in_use(&provider_id);
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

    /// レビューを開始する。`Reviewer` 条件があるのにプロバイダ／並列度の枠が無いときは `Ok(false)`
    /// （タスクは `reviewing` のまま。次 tick の `recover_reviews` が再試行する。ADR-0007 D5 1.）。
    fn spawn_review(
        &mut self,
        task_id: TaskId,
        run_id: String,
        subject: &ReviewSubject,
    ) -> Result<bool, DispatchError> {
        let Some(task) = self.store.get(task_id)? else {
            return Ok(true);
        };
        let Some(dir) = self.task_dir(&task) else {
            tracing::warn!(%task_id, "cannot review task with remote workspace");
            return Ok(true);
        };

        let human = match self.resolve_human_approvals(&task)? {
            Some(h) => h,
            None => {
                tracing::debug!(%task_id, "review deferred (waiting for human approval)");
                return Ok(false);
            }
        };

        let reviewer = if needs_reviewer_run(&task) {
            match self.pick_reviewer(&task, &run_id) {
                Some(r) => Some(r),
                None => {
                    tracing::debug!(%task_id, "reviewer run deferred (no provider capacity)");
                    return Ok(false);
                }
            }
        } else {
            None
        };
        let provider = reviewer.as_ref().map(|(p, _)| p.clone());
        let reviewer_run = reviewer.map(|(_, r)| r);

        let plan = if task.kind == TaskKind::Plan {
            Some(PlanCheck {
                depth: self.plan_depth(&task)?,
                limits: PlanLimits::default(),
            })
        } else {
            None
        };

        let events = self.store.events_for(task_id)?;
        let produced = artifacts_for_run(&events, &run_id);
        let timeout = self.config.review_timeout;
        let subject = subject.clone();
        let tx = self.tx.clone();
        let handle = tokio::spawn(async move {
            let ws = LocalWorkspace::new(&dir);
            let extras = ReviewExtras {
                subject,
                plan,
                reviewer: reviewer_run,
                human,
            };
            let outcome = review_task(&task, &ws, &dir, &produced, timeout, extras).await;
            let _ = tx.send(Completion::Review {
                task_id,
                run_id,
                outcome,
            });
        });
        self.reviewing.insert(task_id, ReviewEntry { handle, provider });
        Ok(true)
    }

    /// `task.acceptance` の各 `Check::Human` について `Approval` 子タスクを解決する（ADR-0008 D2）。
    /// 子が無ければ作る。いずれかがまだ未決（`Ready`/`Draft`）なら `Ok(None)`（レビュー全体を延期）。
    /// 全て終端に達していれば `idx -> (pass, reason)` を返す（`Human` criterion が無ければ空の map）。
    fn resolve_human_approvals(&self, task: &Task) -> Result<Option<HumanVerdicts>, DispatchError> {
        let human_indices: Vec<usize> = task
            .acceptance
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(c.check, Check::Human))
            .map(|(idx, _)| idx)
            .collect();
        if human_indices.is_empty() {
            return Ok(Some(HashMap::new()));
        }

        let existing_children = self.store.list(None)?;
        let mut resolved = HashMap::with_capacity(human_indices.len());
        for idx in human_indices {
            let title = human_approval_title(task, idx);
            let child = match existing_children
                .iter()
                .find(|c| c.parent_id == Some(task.id) && c.kind == TaskKind::Approval && c.title == title)
            {
                Some(c) => c.clone(),
                None => self.create_human_approval_child(task, idx, &title)?,
            };
            match child.status {
                Status::Done => {
                    resolved.insert(idx, (true, format!("approved (approval task {})", child.id)));
                }
                Status::Failed => {
                    let note = approval_decision_note(&self.store.events_for(child.id)?);
                    resolved.insert(idx, (false, format!("rejected (approval task {}){note}", child.id)));
                }
                Status::Cancelled => {
                    resolved.insert(idx, (false, format!("approval task {} was cancelled", child.id)));
                }
                _ => return Ok(None),
            }
        }
        Ok(Some(resolved))
    }

    /// `Human` criterion のための `Approval` 子タスクを新規作成する（ADR-0008 D2）。
    fn create_human_approval_child(&self, task: &Task, idx: usize, title: &str) -> Result<Task, DispatchError> {
        let now = OffsetDateTime::now_utc();
        let approval = Task {
            id: TaskId::new(),
            parent_id: Some(task.id),
            kind: TaskKind::Approval,
            title: title.to_string(),
            objective: task.acceptance[idx].text.clone(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: task.priority,
            worker_hint: task.worker_hint.clone(),
            workspace: task.workspace.clone(),
            budget: task.budget,
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
        };
        self.store.insert(&approval)?;
        self.store.append_event(
            approval.id,
            &Event::Created {
                task: Box::new(approval.clone()),
            },
        )?;
        self.store.append_event(approval.id, &Event::ApprovalRequested)?;
        tracing::info!(task_id = %task.id, approval_id = %approval.id, criterion_idx = idx, "created approval child for human check");
        Ok(approval)
    }

    /// `Reviewer` run のアダプタ／プロバイダを選ぶ（ADR-0007 D5 1.）。並列度の枠は実行中 run と共有する。
    fn pick_reviewer(&mut self, task: &Task, subject_run_id: &str) -> Option<(ProviderId, ReviewerRun)> {
        if self.workers_in_flight() >= self.config.max_concurrency {
            return None;
        }
        let Some((adapter_id, provider_id)) = self.policy.pick(&reviewer_hint(), Instant::now()) else {
            // 候補ゼロ（Standard tier を提供するプロバイダが無い、または全て cooldown 中）。設定ミスなら
            // 永久に待つことになるので、一時的な満杯（debug）と区別して warn を 1 回出す（監査指摘）。
            if self.warned_no_reviewer.insert(task.id) {
                tracing::warn!(task_id = %task.id, hint = ?reviewer_hint(), "no provider available for the reviewer run; task stays reviewing until one appears");
            }
            return None;
        };
        self.warned_no_reviewer.remove(&task.id);
        let adapter = match self.adapters.get(&adapter_id) {
            Some(a) => a.clone(),
            None => {
                tracing::warn!(task_id = %task.id, adapter = %adapter_id, "reviewer adapter not configured");
                return None;
            }
        };
        if self.provider_in_use(&provider_id) >= self.policy.concurrency_limit(provider_id.clone()) {
            return None;
        }
        let review_run_id = ulid::Ulid::new().to_string();
        let sink = ReviewerSink {
            store: self.store.clone(),
            task_id: task.id,
            subject_run_id: subject_run_id.to_string(),
            review_run_id: review_run_id.clone(),
        };
        tracing::info!(task_id = %task.id, %review_run_id, adapter = %adapter_id, provider = %provider_id, "starting reviewer run");
        Some((
            provider_id,
            ReviewerRun {
                adapter,
                run_id: review_run_id,
                limits: RunLimits {
                    wall_clock: Duration::from_secs(task.budget.max_wall_secs),
                    idle_timeout: self.config.idle_timeout,
                    kill_grace: self.config.kill_grace,
                },
                sink: Box::new(sink),
            },
        ))
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
    if task.kind == TaskKind::Plan {
        // ADR-0007 D1: 前回の run の plan.json を今回の出力と誤読しない。
        let _ = tokio::fs::remove_file(workspace.join(PLAN_FILE)).await;
    }
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
            review: None,
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

/// デーモン再起動後の復旧用: `runs/<run_id>/result.json`（`fake`/`run_subprocess` が書く終端メッセージ）から
/// `done` の内容を復元する。無ければ空（ADR-0007 D5）。
fn subject_from_run_dir(dir: &std::path::Path, run_id: &str) -> ReviewSubject {
    let path = dir.join("runs").join(run_id).join("result.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return ReviewSubject::default();
    };
    match serde_json::from_str::<WorkerMessage>(&text) {
        Ok(WorkerMessage::Done { summary, evidence, .. }) => ReviewSubject { summary, evidence },
        _ => ReviewSubject::default(),
    }
}

/// `Human` criterion 用の `Approval` 子タスクの `title`（既存子の照合キーにも使う。ADR-0008 D2）。
fn human_approval_title(task: &Task, idx: usize) -> String {
    format!("Approval needed: {} — criterion {idx}", task.title)
}

/// 直近の `Event::ApprovalDecided` の `note` を `": <note>"` の形で返す（無ければ空文字列）。
fn approval_decision_note(events: &[(u64, Event)]) -> String {
    events
        .iter()
        .rev()
        .find_map(|(_, e)| match e {
            Event::ApprovalDecided { note: Some(n), .. } => Some(format!(": {n}")),
            Event::ApprovalDecided { note: None, .. } => Some(String::new()),
            _ => None,
        })
        .unwrap_or_default()
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
                plan_auto_accept: false,
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

    /// `artifacts/plan.json` を書くテスト用アダプタ（Plan kind）、または `artifacts/review.json` を書く（Review kind）。
    struct FileAdapter {
        plan_json: String,
        review_json: String,
        delay: Duration,
    }

    #[async_trait]
    impl WorkerAdapter for FileAdapter {
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
            std::fs::create_dir_all(req.workspace.join("artifacts")).unwrap();
            match req.task.kind {
                TaskKind::Plan => {
                    // 2 回目以降（prior_review あり）は正しい plan を書き、1 回目は plan_json をそのまま書く。
                    let text = if req.context.prior_review.is_empty() {
                        self.plan_json.clone()
                    } else {
                        VALID_PLAN.to_string()
                    };
                    std::fs::write(req.workspace.join("artifacts/plan.json"), text).unwrap();
                }
                TaskKind::Review => {
                    assert!(req.context.review.is_some());
                    std::fs::write(req.workspace.join("artifacts/review.json"), &self.review_json).unwrap();
                }
                _ => {
                    std::fs::write(req.workspace.join("touched"), "1").unwrap();
                }
            }
            sink.progress("working");
            tokio::time::sleep(self.delay).await;
            Ok(RunOutcome {
                terminal: Terminal::Done { summary: format!("{:?}", req.task.kind), evidence: vec![], usage: None },
                exit_code: Some(0),
            })
        }
    }

    const VALID_PLAN: &str = r#"{"tasks":[
        {"title":"a","objective":"do a","acceptance":[{"text":"touched","check":{"type":"command","cmd":"test -f touched","expect_exit":0}}]},
        {"title":"b","objective":"do b","acceptance":[{"text":"touched","check":{"type":"command","cmd":"test -f touched","expect_exit":0}}],"depends_on":[0]},
        {"title":"c","objective":"do c","acceptance":[{"text":"looks good","check":{"type":"reviewer"}}],"depends_on":[0,1],"tier":"cheap"}
    ]}"#;

    fn plan_task(dir: &std::path::Path, max_retries: u32) -> Task {
        let mut t = new_task(dir, Check::Command { cmd: "true".into(), expect_exit: 0 }, max_retries);
        t.kind = TaskKind::Plan;
        t.acceptance.clear();
        t.worker_hint.tier = Tier::Frontier;
        t
    }

    #[tokio::test]
    async fn plan_task_inserts_draft_children_and_they_run_after_accept() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let plan = plan_task(dir.path(), 0);
        store.insert(&plan).unwrap();
        let adapter = Arc::new(FileAdapter {
            plan_json: VALID_PLAN.into(),
            review_json: r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"fine"}]}"#.into(),
            delay: Duration::from_millis(5),
        });
        let mut d = dispatcher(store.clone(), adapter, 2);
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        let p = store.get(plan.id).unwrap().unwrap();
        assert_eq!(p.status, Status::Done);
        let children: Vec<Task> = store.list(Some(Status::Draft)).unwrap();
        assert_eq!(children.len(), 3, "auto_accept=false leaves children in draft");
        for c in &children {
            assert_eq!(c.parent_id, Some(plan.id));
            assert_eq!(c.workspace, plan.workspace);
        }
        let verdicts: Vec<(usize, bool, String)> = store
            .events_for(plan.id)
            .unwrap()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::ReviewVerdict { criterion_idx, pass, reason, .. } => Some((criterion_idx, pass, reason)),
                _ => None,
            })
            .collect();
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].0, 0);
        assert!(verdicts[0].1);
        assert!(verdicts[0].2.contains("3 tasks"));

        // 人間が approve（Accept）すると子が順に実行され、c は Reviewer 条件を LLM run（FileAdapter）で判定して done。
        for c in &children {
            store.apply_transition(c.id, Trigger::Accept, None).unwrap();
        }
        let report = run_until_idle(&mut d, 400).await;
        assert!(report.idle);
        for c in &children {
            let t = store.get(c.id).unwrap().unwrap();
            assert_eq!(t.status, Status::Done, "{}: {:?}", c.title, store.events_for(c.id).unwrap());
        }
        let c = children.iter().find(|c| c.title == "c").unwrap();
        assert_eq!(c.worker_hint.tier, Tier::Cheap);
        let events = store.events_for(c.id).unwrap();
        let run_id = last_run_id(&events).unwrap();
        let reviewer_progress = events
            .iter()
            .filter(|(_, e)| matches!(e, Event::WorkerProgress { run_id: r, msg } if r == &run_id && msg.starts_with("reviewer run ")))
            .count();
        assert!(reviewer_progress >= 2, "{events:?}");
        assert!(events.iter().any(|(_, e)| matches!(e, Event::ReviewVerdict { pass: true, reason, .. } if reason.contains("reviewer(") && reason.contains("fine"))));
        // WorkerStarted はワーカー run の 1 回だけ（Reviewer run は WorkerStarted を使わない）。
        assert_eq!(events.iter().filter(|(_, e)| matches!(e, Event::WorkerStarted { .. })).count(), 1);
    }

    #[tokio::test]
    async fn invalid_plan_is_retried_with_prior_review_then_children_auto_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let plan = plan_task(dir.path(), 1);
        store.insert(&plan).unwrap();
        let adapter = Arc::new(FileAdapter {
            plan_json: r#"{"tasks":[{"title":"a","objective":"o","acceptance":[{"text":"c","check":{"type":"human"}}],"depends_on":[9]}]}"#.into(),
            review_json: String::new(),
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.plan_auto_accept = true;
        let report = run_until_idle(&mut d, 300).await;
        assert!(report.idle);
        let p = store.get(plan.id).unwrap().unwrap();
        assert_eq!(p.status, Status::Done);
        assert_eq!(p.attempts, 1);
        let events = store.events_for(plan.id).unwrap();
        let verdicts: Vec<(bool, String)> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::ReviewVerdict { pass, reason, .. } => Some((*pass, reason.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(verdicts.len(), 2);
        assert!(!verdicts[0].0 && verdicts[0].1.contains("out of range"), "{:?}", verdicts[0]);
        assert!(verdicts[1].0);
        // auto_accept=true: 子は ready で挿入され、その後 done まで進む（a, b は Command、c は Reviewer で review.json 無し→ fail → failed）。
        let children: Vec<Task> = store.list(None).unwrap().into_iter().filter(|t| t.parent_id == Some(plan.id)).collect();
        assert_eq!(children.len(), 3);
        for c in &children {
            let ev = store.events_for(c.id).unwrap();
            assert!(matches!(&ev[0].1, Event::Created { task } if task.status == Status::Draft));
            assert!(matches!(&ev[1].1, Event::Transitioned { from: Status::Draft, to: Status::Ready, reason } if reason == "accept"));
        }
        let by_title = |t: &str| children.iter().find(|c| c.title == t).map(|c| store.get(c.id).unwrap().unwrap()).unwrap();
        assert_eq!(by_title("a").status, Status::Done);
        assert_eq!(by_title("b").status, Status::Done);
        let c = by_title("c");
        assert_eq!(c.status, Status::Failed, "{:?}", store.events_for(c.id).unwrap());
        assert!(store.events_for(c.id).unwrap().iter().any(|(_, e)| matches!(e, Event::ReviewVerdict { pass: false, reason, .. } if reason.contains("review.json"))));
    }

    #[tokio::test]
    async fn reviewer_run_shares_concurrency_and_is_deferred_when_at_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        // 並列度 1: 実行中のワーカーがいる間は Reviewer run を開始できず、reviewing のまま待つ。
        let r = new_task(dir.path(), Check::Reviewer, 0);
        store.insert(&r).unwrap();
        let adapter = Arc::new(FileAdapter {
            plan_json: String::new(),
            review_json: r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"ok"}]}"#.into(),
            delay: Duration::from_millis(150),
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let first = d.tick().unwrap();
        assert_eq!(first.dispatched, 1);
        // ワーカーが終わるのを待ってから、次の tick で reviewing に入る。
        tokio::time::sleep(Duration::from_millis(250)).await;
        // 2 つ目のタスクを ready にしておき、Reviewer run が枠を取っている間は dispatch されないことを見る。
        let other = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        store.insert(&other).unwrap();
        let second = d.tick().unwrap();
        assert_eq!(second.finished, 1);
        assert_eq!(store.get(r.id).unwrap().unwrap().status, Status::Reviewing);
        assert_eq!(second.dispatched, 0, "reviewer run occupies the only slot");
        let report = run_until_idle(&mut d, 300).await;
        assert!(report.idle);
        assert_eq!(store.get(r.id).unwrap().unwrap().status, Status::Done);
        assert_eq!(store.get(other.id).unwrap().unwrap().status, Status::Done);
    }

    /// ADR-0008 D2: `Check::Human` はディスパッチャが `Approval` 子タスクを生成して待つ。承認前は
    /// `reviewing` のまま（`attempts` を消費しない）、承認後に `Done` になる。
    #[tokio::test]
    async fn human_check_creates_approval_child_and_completes_after_approval() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Human, 1);
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 2);

        let approval = wait_for_approval_child(&mut d, &store, task.id).await;
        assert_eq!(approval.status, Status::Ready);
        assert_eq!(approval.parent_id, Some(task.id));
        // 未決の間は reviewing のまま、attempts は消費しない。
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Reviewing);
        assert_eq!(t.attempts, 0);

        store
            .apply_transition(
                approval.id,
                Trigger::Approve,
                Some(Event::ApprovalDecided { by: "human".into(), approved: true, note: Some("looks good".into()) }),
            )
            .unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Done);
        assert_eq!(t.attempts, 0);
        assert!(
            store
                .events_for(task.id)
                .unwrap()
                .iter()
                .any(|(_, e)| matches!(e, Event::ReviewVerdict { pass: true, reason, .. } if reason.contains("approved")))
        );
    }

    /// ADR-0008 D2: 承認児タスクが reject されると、対象タスクの `Human` criterion は fail になる
    /// （`max_retries=0` なので即 `Failed`）。
    #[tokio::test]
    async fn human_check_fails_task_after_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Human, 0);
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 2);

        let approval = wait_for_approval_child(&mut d, &store, task.id).await;
        store
            .apply_transition(
                approval.id,
                Trigger::Reject,
                Some(Event::ApprovalDecided { by: "human".into(), approved: false, note: Some("not ready".into()) }),
            )
            .unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Failed);
        assert!(
            store
                .events_for(task.id)
                .unwrap()
                .iter()
                .any(|(_, e)| matches!(e, Event::ReviewVerdict { pass: false, reason, .. } if reason.contains("rejected") && reason.contains("not ready")))
        );
    }

    /// DESIGN §6 Phase 6 受け入れ: 承認前に子が `ready` にならないこと（dispatch されないこと）、
    /// `reject` で子が `cancelled` になること。`ready_tasks` の除外は task-core 側で検証済みなので、
    /// ここではディスパッチャの実際の tick を通して「dispatch されない」ことまで確認する。
    #[tokio::test]
    async fn approval_gate_blocks_child_dispatch_and_reject_cancels_it() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());

        let now = OffsetDateTime::now_utc();
        let mut approval = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        approval.kind = TaskKind::Approval;
        approval.status = Status::Ready;
        store.insert(&approval).unwrap();

        let mut child = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        child.parent_id = Some(approval.id);
        child.created_at = now;
        store.insert(&child).unwrap();

        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 2);

        // 承認前: 何 tick 回しても子は dispatch されず Ready のまま。
        for _ in 0..5 {
            let report = d.tick().unwrap();
            assert_eq!(report.dispatched, 0, "child must not be dispatched while its Approval parent is pending");
        }
        assert_eq!(store.get(child.id).unwrap().unwrap().status, Status::Ready);

        // reject すると子は cancelled になり、以降も dispatch されない。
        store
            .apply_transition(
                approval.id,
                Trigger::Reject,
                Some(Event::ApprovalDecided { by: "human".into(), approved: false, note: None }),
            )
            .unwrap();
        assert_eq!(store.get(approval.id).unwrap().unwrap().status, Status::Failed);
        assert_eq!(store.get(child.id).unwrap().unwrap().status, Status::Cancelled);
        for _ in 0..5 {
            let report = d.tick().unwrap();
            assert_eq!(report.dispatched, 0);
        }
        assert_eq!(store.get(child.id).unwrap().unwrap().status, Status::Cancelled);
    }

    /// `task_id` の直接の `Approval` 子タスクが現れるまで tick を回す（Human check の生成を待つ）。
    async fn wait_for_approval_child(d: &mut Dispatcher, store: &Arc<dyn TaskStore>, task_id: TaskId) -> Task {
        for _ in 0..100 {
            d.tick().unwrap();
            if let Some(child) = store
                .list(None)
                .unwrap()
                .into_iter()
                .find(|t| t.parent_id == Some(task_id) && t.kind == TaskKind::Approval)
            {
                return child;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("approval child was not created for task {task_id}");
    }
}
