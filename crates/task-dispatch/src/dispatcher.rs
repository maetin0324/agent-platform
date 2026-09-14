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
use task_ops::derive::{
    AnswerNote, REVIEWER_REQUEUED_PREFIX, ReviewNote, answers_from_events, approval_decision_note,
    artifacts_for_run, consecutive_requeues, consecutive_reviewer_requeues, human_approval_title, last_run_id,
    prior_review_from_events, retry_backoff,
};
use task_worker::{
    AdapterError, Answer, EventSink, LocalWorkspace, PROTOCOL_VERSION, PriorReview, RunContext, RunLimits,
    RunOutcome, RunRequest, Terminal, WorkerMessage, Workspace, WorkerAdapter,
};
use task_ops::daemon::{CooldownView, DaemonSnapshot, InFlight, InFlightKind, ProviderLive};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::policy::{AdapterId, CooldownReason, ProviderId, ProviderOutcome, ProviderPolicy, Selection};

/// RFC 3339 の文字列（デーモンのスナップショット用）。書式化に失敗することは実質無いが、その場合は空文字列。
fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}
use crate::review::{
    HumanVerdicts, PLAN_FILE, PlanCheck, ReviewExtras, ReviewOutcome, ReviewSubject, ReviewerRun, Verdict,
    needs_reviewer_run, review_task,
};

/// `task_ops::derive::ReviewNote` をワーカープロトコルの `task_worker::PriorReview` に写す
/// （ADR-0013 D7: task-ops は task_worker に依存しないため、この写像は dispatcher 側で行う）。
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
    /// ADR-0010 D6（P-3）: attempts > 0 の ready タスクは `updated_at + min(base·2^(attempts-1), max)` まで dispatch しない。
    /// `base = 0` で無効。
    pub retry_backoff_base: Duration,
    pub retry_backoff_max: Duration,
    /// ADR-0010 D9（P-30）: `Reviewer` run の `pick` と合成 `Review` タスクの `worker_hint`。
    pub reviewer_hint: task_core::WorkerHint,
    /// ADR-0011（P-38）: 同じ試行での連続 requeue の上限。達したら供給側失敗を通常の失敗（attempts 消費）として扱う。
    pub max_requeues: u32,
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
    /// dispatch した時刻（デーモンのスナップショット用。ADR-0013 D4）。
    since: OffsetDateTime,
}

struct ReviewEntry {
    handle: JoinHandle<()>,
    /// `Reviewer` run を起動する場合に選んだプロバイダ（並列度の枠を消費する）。
    provider: Option<ProviderId>,
    /// レビューを延期（Reviewer run の供給側失敗）するときに次 tick へ持ち越す `done` の内容。
    subject: ReviewSubject,
    /// レビュー対象の run（デーモンのスナップショット用）。
    run_id: String,
    since: OffsetDateTime,
}

/// デーモン状態をメモリから公開するための送り口（ADR-0013 D4）。taskd が `[api]` 有効時に `set_snapshot_publisher` で渡す。
pub struct SnapshotPublisher {
    pub tx: tokio::sync::watch::Sender<Option<DaemonSnapshot>>,
    /// 起動ごとの ULID（API の `/health` と同じ値）。
    pub instance_id: String,
    pub hostname: String,
    /// RFC 3339。
    pub started_at: String,
    pub tick_ms: u64,
    /// `[[providers]]` の定義（`in_use` は毎 tick に埋める）。
    pub providers: Vec<ProviderLive>,
}

/// run 途中のイベントをストアに追記するシンク。ワーカーの出力（heartbeat）があればリースを延長する（ADR-0010 D7）。
struct StoreSink {
    store: Arc<dyn TaskStore>,
    task_id: TaskId,
    run_id: String,
    /// 延長後の ttl（`idle_timeout + lease_grace`）。
    lease_ttl: Duration,
    /// 延長の最小間隔（`lease_grace / 2`）。延長後の期限は常にアダプタの無出力タイムアウトより後になる。
    renew_every: Duration,
    last_renew: std::sync::Mutex<Instant>,
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

    fn heartbeat(&self) {
        let Ok(mut last) = self.last_renew.lock() else {
            return;
        };
        if last.elapsed() < self.renew_every {
            return;
        }
        *last = Instant::now();
        match self.store.renew_lease(self.task_id, &self.run_id, self.lease_ttl) {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(task_id = %self.task_id, run_id = %self.run_id, "lease not renewed (no longer running under this run)")
            }
            Err(e) => tracing::warn!(task_id = %self.task_id, error = %e, "failed to renew lease"),
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
    /// プロバイダ（= アカウント）ごとのアダプタのインスタンス（ADR-0012 D1）。
    adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>>,
    config: DispatchConfig,
    running: HashMap<TaskId, RunEntry>,
    reviewing: HashMap<TaskId, ReviewEntry>,
    /// レビューを開始できなかった（`Reviewer` run の枠が無い）タスクの `done` 内容。次 tick で使う。
    pending_subjects: HashMap<TaskId, ReviewSubject>,
    /// 「設定に合うプロバイダが無い」警告を出した（連続 tick で繰り返さない）タスク（ADR-0012 D2）。
    warned_unroutable: std::collections::HashSet<TaskId>,
    /// この tick で `NoMatchingProvider` だった ready タスク（`is_idle` で待ち対象から外す。ADR-0012 D2）。
    unroutable: std::collections::HashSet<TaskId>,
    /// 人間の承認待ちで延期中の reviewing タスク（`is_idle` 判定用。ADR-0010 D8）。
    awaiting_human: std::collections::HashSet<TaskId>,
    tx: mpsc::UnboundedSender<Completion>,
    rx: mpsc::UnboundedReceiver<Completion>,
    /// tick の回数（スナップショット用）。
    ticks: u64,
    publisher: Option<SnapshotPublisher>,
}

impl Dispatcher {
    /// `models` は provider id → `WorkerStarted.model` に記録するモデル名。
    pub fn new(
        store: Arc<dyn TaskStore>,
        policy: Box<dyn ProviderPolicy>,
        models: HashMap<ProviderId, String>,
        adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>>,
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
            warned_unroutable: std::collections::HashSet::new(),
            unroutable: std::collections::HashSet::new(),
            awaiting_human: std::collections::HashSet::new(),
            tx,
            rx,
            ticks: 0,
            publisher: None,
        }
    }

    pub fn config(&self) -> &DispatchConfig {
        &self.config
    }

    /// ADR-0013 D4: tick ごとにデーモンのスナップショットを `watch` に送るようにする。
    pub fn set_snapshot_publisher(&mut self, publisher: SnapshotPublisher) {
        self.publisher = Some(publisher);
    }

    /// 1 tick。tokio ランタイム内から呼ぶ（ワーカーとレビューを `tokio::spawn` する）。
    pub fn tick(&mut self) -> Result<TickReport, DispatchError> {
        self.ticks += 1;
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
        self.publish_snapshot();
        Ok(report)
    }

    /// ADR-0013 D4: メモリ上の状態からスナップショットを作り `watch` に送る（DB には書かない。受け手がいなくても無害）。
    fn publish_snapshot(&self) {
        let Some(publisher) = &self.publisher else {
            return;
        };
        let now_instant = Instant::now();
        let now = OffsetDateTime::now_utc();
        let mut in_flight: Vec<InFlight> = self
            .running
            .iter()
            .map(|(task_id, e)| InFlight {
                task_id: *task_id,
                run_id: e.run_id.clone(),
                provider: e.provider.clone(),
                kind: InFlightKind::Worker,
                since: rfc3339(e.since),
            })
            .collect();
        in_flight.extend(self.reviewing.iter().filter_map(|(task_id, e)| {
            e.provider.as_ref().map(|provider| InFlight {
                task_id: *task_id,
                run_id: e.run_id.clone(),
                provider: provider.clone(),
                kind: InFlightKind::Reviewer,
                since: rfc3339(e.since),
            })
        }));
        in_flight.sort_by(|a, b| a.since.cmp(&b.since).then(a.task_id.cmp(&b.task_id)));
        let cooldowns = self
            .policy
            .cooldowns(now_instant)
            .into_iter()
            .map(|c| CooldownView {
                provider: c.provider,
                until: rfc3339(now + c.until.saturating_duration_since(now_instant)),
                reason: match c.reason {
                    CooldownReason::Throttled => "throttled",
                    CooldownReason::AuthFailed => "auth_failed",
                    CooldownReason::Exhausted => "exhausted",
                }
                .to_string(),
            })
            .collect();
        let mut awaiting_human: Vec<TaskId> = self.awaiting_human.iter().copied().collect();
        awaiting_human.sort();
        let mut unroutable: Vec<TaskId> = self.unroutable.iter().copied().collect();
        unroutable.sort();
        let providers = publisher
            .providers
            .iter()
            .map(|p| ProviderLive {
                in_use: self.provider_in_use(&p.id) as u32,
                ..p.clone()
            })
            .collect();
        let snapshot = DaemonSnapshot {
            instance_id: publisher.instance_id.clone(),
            pid: std::process::id(),
            hostname: publisher.hostname.clone(),
            started_at: publisher.started_at.clone(),
            last_tick_at: rfc3339(now),
            ticks: self.ticks,
            tick_ms: publisher.tick_ms,
            in_flight,
            cooldowns,
            awaiting_human,
            unroutable,
            providers,
        };
        // 受け手（API）がいなければ送信は失敗するが、デーモンの動作には関係ない。
        let _ = publisher.tx.send(Some(snapshot));
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
        // ADR-0013 D9: 供給側失敗なら種別（ProviderThrottled.reason）を、result を消費する前に取っておく。
        let failure_reason = result.as_ref().err().and_then(provider_failure_reason);
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
            Err(e) => match provider_failure_outcome(&e) {
                // ADR-0010 D5（P-21）: 供給側失敗は attempts を消費せず requeue し、プロバイダを cooldown にする。
                Some(po) if consecutive_requeues(&self.store.events_for(task_id)?) < self.config.max_requeues => {
                    (Trigger::Requeue, format!("requeue: adapter: {e}"), None, po)
                }
                // ADR-0011（P-38）: 同じ試行での連続 requeue が上限に達したら、通常の失敗として attempts を消費する。
                Some(po) => (
                    Trigger::WorkerError { retryable: true },
                    format!(
                        "error(retryable=true): requeue limit ({}) reached: adapter: {e}",
                        self.config.max_requeues
                    ),
                    None,
                    po,
                ),
                None => (
                    Trigger::WorkerError { retryable: true },
                    format!("error(retryable=true): adapter: {e}"),
                    None,
                    ProviderOutcome::Ok,
                ),
            },
        };
        self.policy.report(provider.clone(), &provider_outcome);

        let finished = Event::WorkerFinished {
            run_id: run_id.clone(),
            outcome: outcome_str.clone(),
            usage,
        };
        let mut events = vec![finished];
        // ADR-0013 D9: cooldown に入った供給側失敗を、遷移と同じトランザクションで記録する。
        if let Some(reason) = failure_reason
            && let Some(ev) = self.provider_throttled_event(&provider, &provider_outcome, reason)
        {
            events.push(ev);
        }
        match self
            .store
            .apply_transition_with_events(task_id, trigger, events)
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
        mut outcome: ReviewOutcome,
    ) -> Result<(), DispatchError> {
        let entry = self.reviewing.remove(&task_id);
        let Some(task) = self.store.get(task_id)? else {
            return Ok(());
        };
        if task.status != Status::Reviewing {
            tracing::warn!(%task_id, status = ?task.status, "review result discarded (task no longer reviewing)");
            return Ok(());
        }
        let mut throttled_events = Vec::new();
        if let Some(pf) = outcome.provider_failure.take() {
            if let Some(provider) = entry.as_ref().and_then(|e| e.provider.clone()) {
                self.policy.report(provider.clone(), &pf.outcome);
                if let Some(ev) = self.provider_throttled_event(&provider, &pf.outcome, cooldown_reason_name(&pf.outcome)) {
                    throttled_events.push(ev);
                }
            }
            let deferrals = consecutive_reviewer_requeues(&self.store.events_for(task_id)?);
            if deferrals < self.config.max_requeues {
                // ADR-0010 D5（P-29）: Reviewer run の供給側失敗は判定しない。reviewing のまま次 tick に回し、
                // プロバイダを cooldown にする（attempts を消費しない）。
                self.store.append_event(
                    task_id,
                    &Event::WorkerProgress {
                        run_id: run_id.clone(),
                        msg: format!("{REVIEWER_REQUEUED_PREFIX}{}", pf.message),
                    },
                )?;
                for ev in &throttled_events {
                    self.store.append_event(task_id, ev)?;
                }
                if let Some(entry) = entry {
                    self.pending_subjects.insert(task_id, entry.subject);
                }
                tracing::warn!(%task_id, %run_id, reason = %pf.message, "reviewer run hit a provider failure; review deferred");
                return Ok(());
            }
            // ADR-0011（P-38）: 連続延期が上限に達したら、未判定の Reviewer 条件を fail として通常どおり判定を適用する。
            tracing::warn!(%task_id, %run_id, reason = %pf.message, max_requeues = self.config.max_requeues, "reviewer run requeue limit reached; failing reviewer criteria");
            for (idx, criterion) in task.acceptance.iter().enumerate() {
                if matches!(criterion.check, Check::Reviewer) && !outcome.verdicts.iter().any(|v| v.criterion_idx == idx) {
                    outcome.verdicts.push(Verdict {
                        criterion_idx: idx,
                        pass: false,
                        reason: format!("requeue limit ({}) reached: {}", self.config.max_requeues, pf.message),
                    });
                }
            }
            outcome.verdicts.sort_by_key(|v| v.criterion_idx);
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
            .chain(throttled_events)
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
        let reviewing_tasks = self.store.list(Some(Status::Reviewing))?;
        // 承認待ちの記録は、まだ reviewing のタスクだけに保つ（cancel 等で抜けたものをスナップショットに残さない。ADR-0013 D4）。
        self.awaiting_human
            .retain(|id| reviewing_tasks.iter().any(|t| t.id == *id));
        for task in reviewing_tasks {
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
        self.unroutable.clear();
        if self.workers_in_flight() >= self.config.max_concurrency {
            return Ok(0);
        }
        // 上位から見て見送りが続いても後続を試せるよう、窓は広めに取る。
        let window = self.ready_window();
        let candidates = self.store.ready_tasks(window)?;
        let now = Instant::now();
        let mut dispatched = 0;
        // この tick で並列度の上限に達していると分かったプロバイダ（tick 内では空きが増えないので共有する）。
        let mut full: std::collections::HashSet<ProviderId> = std::collections::HashSet::new();
        for task in candidates {
            if self.workers_in_flight() >= self.config.max_concurrency {
                break;
            }
            if self.running.contains_key(&task.id) {
                continue;
            }
            // ADR-0010 D6（P-3）: ready に入った時刻（DB の updated_at）からのバックオフ。
            if task.attempts > 0 {
                let delay = retry_backoff(self.config.retry_backoff_base, self.config.retry_backoff_max, task.attempts);
                if OffsetDateTime::now_utc() < task.updated_at + delay {
                    tracing::debug!(task_id = %task.id, attempts = task.attempts, delay_ms = delay.as_millis() as u64, "retry backoff; not dispatching yet");
                    continue;
                }
            }
            let Some(dir) = self.task_dir(&task) else {
                tracing::warn!(task_id = %task.id, "remote workspace is not supported; task left ready");
                continue;
            };
            let Some((adapter_id, provider_id)) = self.select_provider(&task.worker_hint, now, task.id, &mut full) else {
                continue;
            };
            let Some(adapter) = self.adapters.get(&provider_id).cloned() else {
                tracing::warn!(task_id = %task.id, provider = %provider_id, adapter = %adapter_id, "no adapter instance for provider");
                continue;
            };

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
                    provider: Some(provider_id.clone()),
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
                    since: OffsetDateTime::now_utc(),
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
        let lease = LeaseRenewal {
            ttl: self.config.idle_timeout + self.config.lease_grace,
            every: self.config.lease_grace / 2,
        };
        tokio::spawn(async move {
            let result = run_worker(store, adapter, task_id, dir, &run_id, limits, lease).await;
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
            Some(h) => {
                self.awaiting_human.remove(&task_id);
                h
            }
            None => {
                self.awaiting_human.insert(task_id);
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
        let entry_subject = subject.clone();
        let entry_run_id = run_id.clone();
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
        self.reviewing.insert(
            task_id,
            ReviewEntry {
                handle,
                provider,
                subject: entry_subject,
                run_id: entry_run_id,
                since: OffsetDateTime::now_utc(),
            },
        );
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
        // ADR-0010 D2: 挿入・Created・ApprovalRequested を 1 トランザクションで。
        self.store.create_task(&approval, vec![Event::ApprovalRequested])?;
        tracing::info!(task_id = %task.id, approval_id = %approval.id, criterion_idx = idx, "created approval child for human check");
        Ok(approval)
    }

    /// `Reviewer` run のアダプタ／プロバイダを選ぶ（ADR-0007 D5 1.）。並列度の枠は実行中 run と共有する。
    fn pick_reviewer(&mut self, task: &Task, subject_run_id: &str) -> Option<(ProviderId, ReviewerRun)> {
        if self.workers_in_flight() >= self.config.max_concurrency {
            return None;
        }
        // ADR-0012 D2: ワーカー run と同じ手順（上限のプロバイダを飛ばして次へ、候補なしは warn）で選ぶ。
        let hint = self.config.reviewer_hint.clone();
        let mut full = std::collections::HashSet::new();
        let (adapter_id, provider_id) = self.select_provider(&hint, Instant::now(), task.id, &mut full)?;
        let adapter = match self.adapters.get(&provider_id) {
            Some(a) => a.clone(),
            None => {
                tracing::warn!(task_id = %task.id, provider = %provider_id, adapter = %adapter_id, "no adapter instance for reviewer provider");
                return None;
            }
        };
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
                hint: self.config.reviewer_hint.clone(),
            },
        ))
    }

    /// ADR-0013 D9: cooldown に入った供給側失敗の `ProviderThrottled`。期限はポリシーの `cooldowns()` から取り、
    /// ポリシーが公開しない場合は `Throttled.retry_after` から計算する（どちらも無ければ記録しない）。
    fn provider_throttled_event(&self, provider: &str, outcome: &ProviderOutcome, reason: &str) -> Option<Event> {
        let now = Instant::now();
        let until = self
            .policy
            .cooldowns(now)
            .into_iter()
            .find(|c| c.provider == provider)
            .map(|c| c.until)
            .or(match outcome {
                ProviderOutcome::Throttled { retry_after } => Some(now + *retry_after),
                _ => None,
            })?;
        Some(Event::ProviderThrottled {
            provider: provider.to_string(),
            until: OffsetDateTime::now_utc() + until.saturating_duration_since(now),
            reason: Some(reason.to_string()),
        })
    }

    /// `ready_tasks` の取得件数。経路なしと分かっているタスク（`warned_unroutable`）の分だけ広げ、それらが窓を埋めて
    /// 後ろの実行可能なタスクが dispatch されない・`is_idle` が誤って真になることを防ぐ（ADR-0012 監査）。
    fn ready_window(&self) -> usize {
        self.config.max_concurrency * 4 + 16 + self.warned_unroutable.len()
    }

    /// ADR-0012 D2（P-20 / P-33）: 並列度の上限に達したプロバイダを除外しながら選ぶ（設定表の次の行へフォールバック）。
    /// 条件に合うプロバイダが設定に無ければ、タスクごとに 1 回 warn し `unroutable` に入れる。
    fn select_provider(
        &mut self,
        hint: &task_core::WorkerHint,
        now: Instant,
        task_id: TaskId,
        full: &mut std::collections::HashSet<ProviderId>,
    ) -> Option<(AdapterId, ProviderId)> {
        // 外部の ProviderPolicy が除外集合を無視しても止まるよう、試行回数に上限を置く。
        for _ in 0..64 {
            match self.policy.select(hint, now, full) {
                Selection::Picked { adapter, provider } => {
                    let limit = self.policy.concurrency_limit(provider.clone());
                    if self.provider_in_use(&provider) >= limit {
                        tracing::debug!(%task_id, %provider, limit, "provider at capacity; trying the next one");
                        full.insert(provider);
                        continue;
                    }
                    self.warned_unroutable.remove(&task_id);
                    return Some((adapter, provider));
                }
                Selection::Busy => {
                    tracing::debug!(%task_id, ?hint, "all matching providers are cooling down or at capacity");
                    return None;
                }
                Selection::NoMatchingProvider => {
                    self.unroutable.insert(task_id);
                    if self.warned_unroutable.insert(task_id) {
                        tracing::warn!(%task_id, ?hint, "no provider in the config matches this worker_hint; the task waits until the config changes");
                    }
                    return None;
                }
            }
        }
        tracing::warn!(%task_id, ?hint, "provider policy kept returning excluded providers; giving up for this tick");
        None
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
        // ADR-0010 D8: 人間の承認待ちで延期中の reviewing は、人間が操作しない限り進まないので idle とみなす。
        if self
            .store
            .list(Some(Status::Reviewing))?
            .iter()
            .any(|t| !self.awaiting_human.contains(&t.id))
        {
            return Ok(false);
        }
        // ADR-0012 D2（P-33）: 設定に合うプロバイダが無い ready タスクは、設定を直さない限り進まないので待ち対象から外す。
        // 窓いっぱいに返ってきた場合は窓の外に実行可能なタスクが残りうるので idle にしない（次 tick で窓が広がる）。
        let window = self.ready_window();
        let ready = self.store.ready_tasks(window)?;
        if ready.len() >= window {
            return Ok(false);
        }
        Ok(ready.iter().all(|t| self.unroutable.contains(&t.id)))
    }
}

async fn run_worker(
    store: Arc<dyn TaskStore>,
    adapter: Arc<dyn WorkerAdapter>,
    task_id: TaskId,
    dir: PathBuf,
    run_id: &str,
    limits: RunLimits,
    lease: LeaseRenewal,
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
    let prior_review = to_prior_review(prior_review_from_events(&events));
    let req = RunRequest {
        protocol: PROTOCOL_VERSION,
        task: task.clone(),
        workspace,
        context: RunContext {
            prior_review,
            inputs: task.inputs.clone(),
            answers: to_answers(answers_from_events(&events)),
            review: None,
        },
    };
    let sink = StoreSink {
        store,
        task_id,
        run_id: run_id.to_string(),
        lease_ttl: lease.ttl,
        renew_every: lease.every,
        last_renew: std::sync::Mutex::new(Instant::now()),
    };
    adapter.run(req, run_id, limits, &sink).await
}

/// ワーカー run 中のリース延長パラメータ（ADR-0010 D7）。
#[derive(Debug, Clone, Copy)]
struct LeaseRenewal {
    /// 延長後の ttl（`idle_timeout + lease_grace`）。
    ttl: Duration,
    /// 延長の最小間隔（`lease_grace / 2`）。
    every: Duration,
}

/// `ProviderThrottled.reason` に書く供給側失敗の種別（ADR-0013 D9）。供給側失敗でなければ `None`。
fn provider_failure_reason(e: &AdapterError) -> Option<&'static str> {
    match e {
        AdapterError::Throttled { .. } => Some("throttled"),
        AdapterError::AuthFailed(_) => Some("auth_failed"),
        AdapterError::Exhausted(_) => Some("exhausted"),
        AdapterError::Spawn(_) => Some("spawn"),
        AdapterError::Io(_) | AdapterError::Serde(_) | AdapterError::Other(_) => None,
    }
}

/// Reviewer run の供給側失敗（`ProviderOutcome` しか残っていない）の種別名（ADR-0013 D9）。
fn cooldown_reason_name(outcome: &ProviderOutcome) -> &'static str {
    match outcome {
        ProviderOutcome::Throttled { .. } => "throttled",
        ProviderOutcome::AuthFailed => "auth_failed",
        ProviderOutcome::Exhausted => "exhausted",
        ProviderOutcome::Ok => "ok",
    }
}

/// 供給側失敗（ADR-0010 D5）なら `ProviderPolicy::report` に渡す結果を返す。起動失敗（`Spawn`）も供給側として扱う。
/// `AdapterError`/`ProviderOutcome` は `task-dispatch`/`task-worker` の型なので、`task-ops` には移さない。
pub fn provider_failure_outcome(e: &AdapterError) -> Option<ProviderOutcome> {
    match e {
        AdapterError::Throttled { retry_after } => Some(ProviderOutcome::Throttled {
            retry_after: *retry_after,
        }),
        AdapterError::AuthFailed(_) => Some(ProviderOutcome::AuthFailed),
        AdapterError::Exhausted(_) | AdapterError::Spawn(_) => Some(ProviderOutcome::Exhausted),
        AdapterError::Io(_) | AdapterError::Serde(_) | AdapterError::Other(_) => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ProviderSpec, StaticPolicy};
    use async_trait::async_trait;
    use task_core::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
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
        let mut adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>> = HashMap::new();
        adapters.insert("p1".into(), adapter);
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
                retry_backoff_base: Duration::ZERO,
                retry_backoff_max: Duration::ZERO,
                reviewer_hint: crate::review::reviewer_hint(),
                max_requeues: 5,
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
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![Evidence { criterion: 0, command: Some("x".into()), exit: Some(0), stdout_tail: None }], usage: None },
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

    fn done_outcome() -> RunOutcome {
        RunOutcome {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            exit_code: Some(0),
        }
    }

    /// 1 回目は供給側失敗（Throttled）、2 回目以降は `touched` を作って done を返すアダプタ。
    struct FlakyProviderAdapter {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl WorkerAdapter for FlakyProviderAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(&self, req: RunRequest, _run_id: &str, _limits: RunLimits, _sink: &dyn EventSink) -> Result<RunOutcome, AdapterError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(AdapterError::Throttled { retry_after: Duration::from_millis(200) });
            }
            std::fs::write(req.workspace.join("touched"), "1").unwrap();
            Ok(done_outcome())
        }
    }

    /// ADR-0010 D5（P-21）: 供給側失敗は attempts を消費せず requeue され、cooldown 中は再 dispatch されず、明けたら done。
    #[tokio::test]
    async fn provider_failure_requeues_without_consuming_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Command { cmd: "test -f touched".into(), expect_exit: 0 }, 0);
        store.insert(&task).unwrap();
        let adapter = Arc::new(FlakyProviderAdapter { calls: AtomicUsize::new(0) });
        let mut d = dispatcher(store.clone(), adapter.clone(), 1);
        assert_eq!(d.tick().unwrap().dispatched, 1);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let second = d.tick().unwrap();
        assert_eq!(second.finished, 1);
        assert_eq!(second.dispatched, 0, "provider is cooling down");
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Ready, 0));

        let report = run_until_idle(&mut d, 300).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Done, 0));
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 2);
        let events = store.events_for(task.id).unwrap();
        assert!(events.iter().any(|(_, e)| matches!(e, Event::Transitioned { from: Status::Running, to: Status::Ready, reason } if reason == "requeue")));
        assert!(events.iter().any(|(_, e)| matches!(e, Event::WorkerFinished { outcome, .. } if outcome.starts_with("requeue: "))));
        // ADR-0013 D9: cooldown の開始が期限と種別つきで残る。
        assert!(events.iter().any(|(_, e)| matches!(
            e,
            Event::ProviderThrottled { provider, until, reason } if provider == "p1" && reason.as_deref() == Some("throttled") && *until > OffsetDateTime::now_utc() - time::Duration::seconds(5)
        )), "{events:?}");
    }

    /// ADR-0010 D6（P-3）: attempts > 0 の ready タスクはバックオフが明けるまで dispatch されず、idle にもならない。
    #[tokio::test]
    async fn retry_backoff_delays_redispatch() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Command { cmd: "test -f never".into(), expect_exit: 0 }, 1);
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "claimed".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.retry_backoff_base = Duration::from_secs(3600);
        d.config.retry_backoff_max = Duration::from_secs(3600);
        for _ in 0..100 {
            d.tick().unwrap();
            let t = store.get(task.id).unwrap().unwrap();
            if (t.status, t.attempts) == (Status::Ready, 1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(store.get(task.id).unwrap().unwrap().attempts, 1);
        for _ in 0..5 {
            let r = d.tick().unwrap();
            assert_eq!(r.dispatched, 0);
            assert!(!r.idle, "a task waiting for its backoff is not idle");
        }
        d.config.retry_backoff_base = Duration::ZERO;
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Failed, 2));

        let (base, max) = (Duration::from_secs(10), Duration::from_secs(300));
        assert_eq!(retry_backoff(base, max, 0), Duration::ZERO);
        assert_eq!(retry_backoff(base, max, 1), Duration::from_secs(10));
        assert_eq!(retry_backoff(base, max, 3), Duration::from_secs(40));
        assert_eq!(retry_backoff(base, max, 40), max);
    }

    /// heartbeat を送りながら少し待ってから done を返すアダプタ。
    struct HeartbeatAdapter;

    #[async_trait]
    impl WorkerAdapter for HeartbeatAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(&self, req: RunRequest, _run_id: &str, _limits: RunLimits, sink: &dyn EventSink) -> Result<RunOutcome, AdapterError> {
            for _ in 0..8 {
                sink.heartbeat();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            std::fs::write(req.workspace.join("touched"), "1").unwrap();
            Ok(done_outcome())
        }
    }

    /// ADR-0010 D7（P-7）: ワーカーの heartbeat でリースが `idle_timeout + lease_grace` に更新される
    /// （取得時の `max_wall_secs + grace` より短くなり、デーモン停止時に早く回収できる）。
    #[tokio::test]
    async fn heartbeat_renews_the_lease() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Command { cmd: "test -f touched".into(), expect_exit: 0 }, 0);
        store.insert(&task).unwrap();
        let mut d = dispatcher(store.clone(), Arc::new(HeartbeatAdapter), 1);
        d.config.lease_grace = Duration::from_millis(400);
        let before = OffsetDateTime::now_utc();
        assert_eq!(d.tick().unwrap().dispatched, 1);
        let initial = store.get(task.id).unwrap().unwrap().lease.unwrap().expires_at;
        assert!(initial > before + time::Duration::seconds(25), "acquired with max_wall_secs + grace");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let renewed = store.get(task.id).unwrap().unwrap().lease.expect("still running").expires_at;
        assert!(renewed < initial, "renewed={renewed} initial={initial}");
        assert!(renewed > OffsetDateTime::now_utc() + time::Duration::seconds(4), "ttl = idle_timeout(5s) + grace");
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
    }

    /// 2 回目以降の run でだけ `second` を作るアダプタ。
    struct CountingAdapter {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl WorkerAdapter for CountingAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(&self, req: RunRequest, _run_id: &str, _limits: RunLimits, _sink: &dyn EventSink) -> Result<RunOutcome, AdapterError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) >= 1 {
                std::fs::write(req.workspace.join("second"), "1").unwrap();
            }
            Ok(done_outcome())
        }
    }

    /// ADR-0010 D8（P-35）: Human 条件は再レビュー（attempt が進んだ後）で新しい Approval 子を要求する。
    /// 承認待ちで延期中の reviewing しか無ければ idle になる。
    #[tokio::test]
    async fn human_check_requests_a_new_approval_for_each_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(dir.path(), Check::Human, 1);
        task.acceptance.push(Criterion {
            text: "second run".into(),
            check: Check::Command { cmd: "test -f second".into(), expect_exit: 0 },
        });
        store.insert(&task).unwrap();
        let task_id = task.id;
        let approvals = |store: &Arc<dyn TaskStore>| -> Vec<Task> {
            let mut v: Vec<Task> = store
                .list(None)
                .unwrap()
                .into_iter()
                .filter(|t| t.parent_id == Some(task_id) && t.kind == TaskKind::Approval)
                .collect();
            v.sort_by_key(|t| t.created_at);
            v
        };
        let approve = |store: &Arc<dyn TaskStore>, id: TaskId| {
            store
                .apply_transition(id, Trigger::Approve, Some(Event::ApprovalDecided { by: "human".into(), approved: true, note: None }))
                .unwrap();
        };
        let mut d = dispatcher(store.clone(), Arc::new(CountingAdapter { calls: AtomicUsize::new(0) }), 2);

        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle, "only a human can make progress now");
        let first = approvals(&store);
        assert_eq!(first.len(), 1);
        assert!(first[0].title.ends_with("(attempt 1)"), "{}", first[0].title);
        assert_eq!(store.get(task_id).unwrap().unwrap().status, Status::Reviewing);

        // 承認 → Command 条件が fail → attempts 1 → 2 回目の run → 新しい Approval 子を待って idle。
        approve(&store, first[0].id);
        let report = run_until_idle(&mut d, 400).await;
        assert!(report.idle);
        let all = approvals(&store);
        assert_eq!(all.len(), 2, "{all:?}");
        assert_eq!(all[0].status, Status::Done);
        assert!(all[1].title.ends_with("(attempt 2)"), "{}", all[1].title);
        assert_eq!(all[1].status, Status::Ready);
        let t = store.get(task_id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Reviewing, 1));

        approve(&store, all[1].id);
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(task_id).unwrap().unwrap().status, Status::Done);
    }

    /// Review run の 1 回目だけ供給側失敗を返し、以降は pass の `review.json` を書くアダプタ。
    struct FlakyReviewerAdapter {
        review_calls: AtomicUsize,
    }

    #[async_trait]
    impl WorkerAdapter for FlakyReviewerAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(&self, req: RunRequest, _run_id: &str, _limits: RunLimits, _sink: &dyn EventSink) -> Result<RunOutcome, AdapterError> {
            if req.task.kind == TaskKind::Review {
                if self.review_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(AdapterError::Throttled { retry_after: Duration::from_millis(200) });
                }
                std::fs::create_dir_all(req.workspace.join("artifacts")).unwrap();
                std::fs::write(
                    req.workspace.join("artifacts/review.json"),
                    r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"fine"}]}"#,
                )
                .unwrap();
            }
            Ok(done_outcome())
        }
    }

    /// ADR-0010 D5（P-29）: Reviewer run の供給側失敗は ReviewFail にならず、reviewing のまま延期され後で判定される。
    #[tokio::test]
    async fn reviewer_run_provider_failure_defers_review_without_consuming_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Reviewer, 0);
        store.insert(&task).unwrap();
        let adapter = Arc::new(FlakyReviewerAdapter { review_calls: AtomicUsize::new(0) });
        let mut d = dispatcher(store.clone(), adapter.clone(), 2);
        let report = run_until_idle(&mut d, 400).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Done, 0));
        assert_eq!(adapter.review_calls.load(Ordering::SeqCst), 2);
        let events = store.events_for(task.id).unwrap();
        assert!(events.iter().any(|(_, e)| matches!(e, Event::WorkerProgress { msg, .. } if msg.starts_with("reviewer run requeued"))));
        assert!(events.iter().any(|(_, e)| matches!(
            e,
            Event::ProviderThrottled { provider, reason, .. } if provider == "p1" && reason.as_deref() == Some("throttled")
        )), "{events:?}");
        let verdicts: Vec<bool> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::ReviewVerdict { pass, .. } => Some(*pass),
                _ => None,
            })
            .collect();
        assert_eq!(verdicts, vec![true]);
    }

    /// 常に供給側失敗（短い cooldown）を返すアダプタ。`review_only` なら Review run だけ失敗し、ワーカー run は done。
    struct AlwaysThrottledAdapter {
        calls: AtomicUsize,
        review_only: bool,
    }

    #[async_trait]
    impl WorkerAdapter for AlwaysThrottledAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(&self, req: RunRequest, _run_id: &str, _limits: RunLimits, _sink: &dyn EventSink) -> Result<RunOutcome, AdapterError> {
            if self.review_only && req.task.kind != TaskKind::Review {
                return Ok(done_outcome());
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(AdapterError::Throttled { retry_after: Duration::from_millis(10) })
        }
    }

    fn transition_reasons(store: &Arc<dyn TaskStore>, id: TaskId) -> Vec<String> {
        store
            .events_for(id)
            .unwrap()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::Transitioned { reason, .. } => Some(reason),
                _ => None,
            })
            .collect()
    }

    /// ADR-0011（P-38）: 同じ試行での連続 requeue が max_requeues に達したら通常の失敗として attempts を消費し、
    /// 次の試行ではまた 0 から数える。最悪 (max_retries + 1) × (max_requeues + 1) 回で `failed` になる。
    #[tokio::test]
    async fn requeue_limit_turns_persistent_provider_failures_into_ordinary_failures() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 1);
        store.insert(&task).unwrap();
        let adapter = Arc::new(AlwaysThrottledAdapter { calls: AtomicUsize::new(0), review_only: false });
        let mut d = dispatcher(store.clone(), adapter.clone(), 1);
        d.config.max_requeues = 2;
        let report = run_until_idle(&mut d, 500).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Failed, 2));
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 6);
        let one_attempt = ["dispatch", "requeue", "dispatch", "requeue", "dispatch", "worker_error"];
        let expected: Vec<&str> = one_attempt.iter().chain(one_attempt.iter()).copied().collect();
        assert_eq!(transition_reasons(&store, task.id), expected);
        let events = store.events_for(task.id).unwrap();
        assert!(events.iter().any(|(_, e)| matches!(e, Event::WorkerFinished { outcome, .. } if outcome.contains("requeue limit (2) reached"))));

        // max_requeues = 0 なら最初の供給側失敗から attempts を消費する。
        let task0 = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        store.insert(&task0).unwrap();
        d.config.max_requeues = 0;
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(transition_reasons(&store, task0.id), vec!["dispatch", "worker_error"]);
    }

    /// ADR-0011（P-38）: Reviewer run の供給側失敗による延期も max_requeues までで、超えたら Reviewer 条件を fail にして判定する。
    #[tokio::test]
    async fn reviewer_requeue_limit_fails_reviewer_criteria() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Reviewer, 0);
        store.insert(&task).unwrap();
        let adapter = Arc::new(AlwaysThrottledAdapter { calls: AtomicUsize::new(0), review_only: true });
        let mut d = dispatcher(store.clone(), adapter.clone(), 2);
        d.config.max_requeues = 2;
        let report = run_until_idle(&mut d, 500).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Failed, 1));
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 3);
        let events = store.events_for(task.id).unwrap();
        // 最後の遷移（review_fail）の直前までで数える（その後ろには ReviewVerdict と ProviderThrottled が続く）。
        let last_transition = events
            .iter()
            .rposition(|(_, e)| matches!(e, Event::Transitioned { .. }))
            .unwrap();
        assert_eq!(consecutive_reviewer_requeues(&events[..last_transition]), 2);
        assert!(events.iter().any(|(_, e)| matches!(e, Event::ReviewVerdict { pass: false, reason, .. } if reason.starts_with("requeue limit (2) reached"))));
    }

    /// 監査の指摘（ADR-0012 D2）: 取得窓（max_concurrency*4+16）を優先度の高い経路なしタスクが埋めても、窓の外の実行可能な
    /// タスクが dispatch され、それが終わるまで idle にならない。
    #[tokio::test]
    async fn unroutable_tasks_do_not_starve_or_hide_routable_tasks_outside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut unroutable = Vec::new();
        for _ in 0..25 {
            let mut t = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
            t.priority = 10;
            t.worker_hint.adapter = Some("nonexistent".into());
            store.insert(&t).unwrap();
            unroutable.push(t.id);
        }
        let routable = new_task(dir.path(), Check::Command { cmd: "test -f touched".into(), expect_exit: 0 }, 0);
        store.insert(&routable).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let first = d.tick().unwrap();
        assert!(!first.idle, "a routable task is still waiting beyond the window");
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(routable.id).unwrap().unwrap().status, Status::Done);
        for id in unroutable {
            assert_eq!(store.get(id).unwrap().unwrap().status, Status::Ready);
        }
    }

    /// ADR-0013 D4: tick の最後にメモリ上のスナップショットが `watch` に送られる（実行中の run、プロバイダの使用数、cooldown）。
    #[tokio::test]
    async fn tick_publishes_daemon_snapshot_to_watch() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::from_millis(300),
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let (tx, rx) = tokio::sync::watch::channel(None);
        d.set_snapshot_publisher(SnapshotPublisher {
            tx,
            instance_id: "inst-1".into(),
            hostname: "host-1".into(),
            started_at: "2026-09-14T00:00:00Z".into(),
            tick_ms: 50,
            providers: vec![ProviderLive {
                id: "p1".into(),
                adapter: "instant".into(),
                tiers: vec![Tier::Standard],
                concurrency: 1,
                model: Some("m".into()),
                in_use: 0,
            }],
        });
        assert!(rx.borrow().is_none(), "nothing is published before the first tick");

        d.tick().unwrap();
        let snap = rx.borrow().clone().expect("snapshot after the first tick");
        assert_eq!((snap.ticks, snap.instance_id.as_str(), snap.tick_ms), (1, "inst-1", 50));
        assert_eq!(snap.pid, std::process::id());
        assert_eq!(snap.in_flight.len(), 1);
        assert_eq!(snap.in_flight[0].task_id, task.id);
        assert_eq!(snap.in_flight[0].kind, InFlightKind::Worker);
        assert_eq!(snap.in_flight[0].provider, "p1");
        assert_eq!(snap.providers[0].in_use, 1);
        assert!(snap.cooldowns.is_empty());

        d.policy.report("p1".into(), &ProviderOutcome::Throttled { retry_after: Duration::from_secs(60) });
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        let snap = rx.borrow().clone().unwrap();
        assert!(snap.ticks > 1);
        assert!(snap.in_flight.is_empty());
        assert_eq!(snap.providers[0].in_use, 0);
        assert_eq!(snap.cooldowns.len(), 1);
        assert_eq!((snap.cooldowns[0].provider.as_str(), snap.cooldowns[0].reason.as_str()), ("p1", "throttled"));
        assert!(snap.cooldowns[0].until > snap.last_tick_at, "until is in the future");
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
