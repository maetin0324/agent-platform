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
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use task_core::plan::{PlanLimits, PlanOutput, materialize};
use task_core::{
    AccountAdapter, ArtifactRef, Check, DelegateTask, DelegationLimits, Event, GenreSpec, OnChildFailure,
    RateLimitObservation, RoleSpec, RunRole, Status, StoreError, Task, TaskId, TaskKind, TaskStore, Trigger,
    WorkspaceSpec,
};
use task_ops::delegate::{pending_children, plan_delegation};
use task_ops::derive::{
    AnswerNote, REVIEWER_REQUEUED_PREFIX, ReviewNote, answers_from_events, approval_decision_note,
    artifacts_for_run, consecutive_requeues, consecutive_reviewer_requeues, human_approval_title, last_run_id,
    prior_review_from_events, retry_backoff,
};
use task_worker::{
    AdapterError, Answer, ChildSummary, EventSink, GenreContext, LocalWorkspace, PROTOCOL_VERSION, PriorReview,
    RoleContext, RunContext, RunLimits, RunOutcome, RunRequest, SshSettings, SshWorkspace, SyncMode, Terminal,
    WorkerMessage, Workspace, WorkerAdapter, control_master_alive_blocking, remote_exec_instructions,
};
use task_ops::daemon::{
    AccountCooldownLive, AccountLive, AccountUsageLive, ClusterLive, CooldownView, DaemonSnapshot, InFlight,
    InFlightKind, ProviderCheckView, ProviderLive,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::accounts::{
    AccountBook, AccountCandidate, AccountCheckRecord, AccountCooldownReason, AccountDir, ExcludedReason,
    ObservationSource, cooldown_for_failure, evaluate, scan_accounts, select_account,
};
use crate::policy::{AdapterId, CooldownReason, ProviderId, ProviderOutcome, ProviderPolicy, Selection};

/// これを超えた tick は段階ごとの所要時間を `warn` で出す（ADR-0015 D2）。
const SLOW_TICK: Duration = Duration::from_secs(1);

/// tick の中の 1 段階がこれを超えたら `warn`（ADR-0015 D2。遅いのが DB かファイルかを切り分ける）。
const SLOW_STEP: Duration = Duration::from_millis(500);

fn log_slow_step(step: &'static str, started: Instant) {
    let elapsed = started.elapsed();
    if elapsed >= SLOW_STEP {
        tracing::warn!(step, duration_ms = elapsed.as_millis() as u64, "slow dispatcher step");
    }
}

/// ADR-0018: コマンドを実行するクラスタ 1 つ分の設定（`taskd::config::ClusterConfig` の写し。task-dispatch は taskd に依存しない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterSpec {
    pub id: String,
    /// `~/.ssh/config` の `Host` 名。
    pub host: String,
    /// このクラスタで同時に走らせる run の上限。
    pub concurrency: usize,
    pub sync: SyncMode,
    pub delete_on_push: bool,
    pub setup: Vec<String>,
    /// 決定的な順に並べた環境変数。
    pub env: Vec<(String, String)>,
    pub rsync_excludes: Vec<String>,
    /// ADR-0019 D1: `sync = "worktree"` のときの設定。
    pub worktree: task_worker::WorktreeSettings,
    /// ADR-0032 D1: `"manual"`（既定） / `"publickey"` / `"totp"`。`"publickey"` のときだけディスパッチャが
    /// 自動で接続を試みる（D3）。
    pub auth: String,
}

/// ADR-0032 D3: `auth = "publickey"` のクラスタに未接続なら、cooldown にする前にディスパッチャが 1 回だけ
/// 接続を試みるためのフック。引数は `(cluster_id, host)`。同期でブロックしてよい（`control_master_alive_blocking`
/// と同じ扱い）。本番では taskd が `task_worker::cluster_login::start_connect` 相当の実装を挿す。テストでは
/// 偽物を挿す。未設定（`None`）なら自動接続はせず、従来どおり cooldown に落ちる。
pub type ClusterConnector = Arc<dyn Fn(&str, &str) -> Result<(), String> + Send + Sync>;

impl ClusterSpec {
    /// このタスクの写し（ローカル）とリモートのパスから、ワーカー用の設定を作る。
    /// `task_id` は worktree のディレクトリ名とブランチ名に使う（ADR-0019 D2）。
    pub fn ssh_settings(&self, remote_path: &std::path::Path, task_id: task_core::TaskId) -> SshSettings {
        let mut settings = SshSettings::new(self.id.clone(), self.host.clone(), remote_path);
        settings.sync = self.sync;
        settings.delete_on_push = self.delete_on_push;
        settings.setup = self.setup.clone();
        settings.env = self.env.clone();
        settings.rsync_excludes = self.rsync_excludes.clone();
        settings.worktree = self.worktree.clone();
        settings.task_id = task_id.to_string();
        settings
    }
}

/// ADR-0023 D1: クラスタの多重接続を確認する間隔（`ssh -O check`）。tick がこれより長ければ毎 tick になる。
const CLUSTER_LIVENESS_INTERVAL: Duration = Duration::from_secs(5);

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

/// ADR-0024/0025: `[accounts]` があるときのプール実行時設定（`taskd::config::AccountsConfig` の写し）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountsRuntimeConfig {
    /// ADR-0025 D1: アダプタごとの根ディレクトリ。`<root>/<id>/` が 1 アカウント。どちらか一方だけでもよい。
    pub roots: HashMap<AccountAdapter, PathBuf>,
    pub max_runs_per_account: usize,
    /// D6 の確認に使うモデル（taskd 側が使う。ディスパッチャ自身は確認を行わない。claude-code のみ）。
    pub check_model: String,
    /// 供給側失敗でアカウントを cooldown にするときのフォールバック秒数（= `error_cooldown_secs`）。
    pub fallback_cooldown_secs: u64,
}

impl AccountsRuntimeConfig {
    pub fn root_for(&self, adapter: AccountAdapter) -> Option<&PathBuf> {
        self.roots.get(&adapter)
    }
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
    /// ADR-0018: `WorkspaceSpec::Remote{cluster}` が指すクラスタ。キーは `cluster` の名前。
    pub clusters: HashMap<String, ClusterSpec>,
    /// ADR-0018 D2: 多重接続が無いクラスタを、この時間だけ dispatch の対象から外す。
    pub cluster_cooldown: Duration,
    /// ADR-0011（P-38）: 同じ試行での連続 requeue の上限。達したら供給側失敗を通常の失敗（attempts 消費）として扱う。
    pub max_requeues: u32,
    /// ADR-0016 D1: `[[roles]]`。run 開始時に `RunContext.role`（指示文）を載せ、委譲された子の既定に使う。
    pub roles: Vec<RoleSpec>,
    /// ADR-0027 D1: `[[genres]]`。委譲の分野解決（`default_role` の既定の穴埋め）と、委譲できる run に渡す
    /// `RunContext.available_genres` に使う。
    pub genres: Vec<GenreSpec>,
    /// ADR-0016 D2: 委譲の上限（1 run の件数・木の深さ・木の run 数）。
    pub delegation: DelegationLimits,
    /// ADR-0024: `[accounts]` が設定されていればプール選択を有効にする。
    pub accounts: Option<AccountsRuntimeConfig>,
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
    /// ADR-0018: コマンドを実行するクラスタ（ローカル実行なら `None`）。並列度の会計に使う。
    cluster: Option<String>,
    /// ADR-0024 D2/D3: プールから選んだアカウント（プールを使わないプロバイダなら `None`）。
    account: Option<String>,
    /// ADR-0025 D1: `account` が属するアダプタ（`account` が `None` なら `None`）。
    account_adapter: Option<AccountAdapter>,
}

/// ADR-0016 M5: 子待ちの親について覚えておくもの。
struct AwaitingChildren {
    run_id: String,
    plan: Option<PlanOutput>,
}

/// run 開始時に決める、ワーカーに渡す追加の文脈（ADR-0016 D1 / D3, ADR-0027 D1）。
#[derive(Default)]
struct RunExtras {
    role: Option<RoleContext>,
    children: Vec<ChildSummary>,
    /// ADR-0027 D1: 委譲できる run（`build_execute_prompt` を使う run）にだけ非空。
    available_genres: Vec<GenreContext>,
}

struct ReviewEntry {
    handle: JoinHandle<()>,
    /// `Reviewer` run を起動する場合に選んだプロバイダ（並列度の枠を消費する）。
    provider: Option<ProviderId>,
    /// レビューを延期（Reviewer run の供給側失敗）するときに次 tick へ持ち越す `done` の内容。
    subject: ReviewSubject,
    /// レビュー対象の run（デーモンのスナップショット用）。
    run_id: String,
    /// ADR-0018: 判定コマンドを実行するクラスタ（ローカルなら `None`）。
    cluster: Option<String>,
    /// Reviewer run を起動した場合のその run の id（スナップショットの `in_flight` 用。ADR-0014 D1）。
    review_run_id: Option<String>,
    since: OffsetDateTime,
    /// ADR-0024 D2/D3: プールから選んだアカウント（プールを使わない、または Reviewer run 自体を起動しない場合は `None`）。
    account: Option<String>,
    /// ADR-0025 D1: `account` が属するアダプタ（`account` が `None` なら `None`）。
    account_adapter: Option<AccountAdapter>,
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
    /// ADR-0022 D2: プロバイダ id → 直近の疎通確認。`reload` でプロバイダ表を差し替えても保持する
    /// （確認した事実は設定の書き換えでは古くならない）。taskd を再起動すると消える。
    pub provider_checks: HashMap<String, ProviderCheckView>,
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
    /// ADR-0016 D2: 委譲の検証に使う `[[roles]]` と上限、この run で既に受け入れた件数。
    roles: Vec<RoleSpec>,
    /// ADR-0027 D1: 委譲の分野解決・検証に使う `[[genres]]`。
    genres: Vec<GenreSpec>,
    delegation: DelegationLimits,
    delegated_this_run: std::sync::atomic::AtomicUsize,
    /// ADR-0024 D4 / ADR-0025 D1: このアカウント（プールを使わなければ `None`）と、そのアダプタの観測値を記録する帳簿
    /// （呼び出し側があらかじめアダプタで解決して渡す）。
    account: Option<String>,
    account_book: Option<Arc<StdMutex<AccountBook>>>,
}

impl StoreSink {
    fn note(&self, msg: String) {
        let ev = Event::WorkerProgress {
            run_id: self.run_id.clone(),
            msg,
        };
        if let Err(e) = self.store.append_event(self.task_id, &ev) {
            tracing::warn!(task_id = %self.task_id, error = %e, "failed to record delegation note");
        }
    }

    /// ADR-0016 D2 / M2 / M6: 提案を検証し、通ったものだけ子として挿入する。拒否理由は `WorkerProgress` に残し、run は失敗させない。
    fn delegate_impl(&self, tasks: &[DelegateTask]) -> Result<(), String> {
        let parent = self
            .store
            .get(self.task_id)
            .map_err(|e| format!("store: {e}"))?
            .ok_or_else(|| "task vanished".to_string())?;
        let ours = parent.status == Status::Running
            && parent.lease.as_ref().map(|l| l.worker_run_id.as_str()) == Some(self.run_id.as_str());
        if !ours {
            return Err("task is no longer running under this run".to_string());
        }
        let already = self.delegated_this_run.load(std::sync::atomic::Ordering::SeqCst);
        let outcome = plan_delegation(
            self.store.as_ref(),
            &parent,
            tasks,
            already,
            &self.roles,
            &self.genres,
            &self.delegation,
            OffsetDateTime::now_utc(),
        )
        .map_err(|e| format!("validation: {e}"))?;
        for reason in &outcome.rejected {
            self.note(format!("delegate rejected: {reason}"));
        }
        if outcome.accepted.is_empty() {
            return Ok(());
        }
        let n = outcome.accepted.len();
        let ids = self
            .store
            .delegate_children(self.task_id, &self.run_id, outcome.accepted)
            .map_err(|e| format!("insert: {e}"))?;
        self.delegated_this_run
            .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
        let listed: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        self.note(format!("delegated {n} child task(s): {}", listed.join(", ")));
        tracing::info!(task_id = %self.task_id, run_id = %self.run_id, children = n, "delegated child tasks inserted");
        Ok(())
    }
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

    fn delegate(&self, tasks: &[DelegateTask]) {
        if let Err(reason) = self.delegate_impl(tasks) {
            tracing::warn!(task_id = %self.task_id, run_id = %self.run_id, %reason, "delegate proposal ignored");
            self.note(format!("delegate ignored: {reason}"));
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

    /// ADR-0024 D4: run の途中でも観測値を `AccountBook` に記録する（`source = "run"`）。プールを使わない run では
    /// `account` が `None` なので no-op。
    fn rate_limit(&self, obs: RateLimitObservation) {
        let Some(account) = &self.account else { return };
        let Some(book) = &self.account_book else { return };
        let Ok(mut book) = book.lock() else { return };
        book.record_observation(account, obs, ObservationSource::Run);
        if let Err(e) = book.save() {
            tracing::warn!(task_id = %self.task_id, %account, error = %e, "failed to save account book after rate_limit observation");
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
    /// ADR-0024 D4: Reviewer run もプールのアカウントで走ることがあるので、同じ帳簿に観測値を記録する
    /// （呼び出し側があらかじめアダプタで解決して渡す。ADR-0025 D1）。
    account: Option<String>,
    account_book: Option<Arc<StdMutex<AccountBook>>>,
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

    fn rate_limit(&self, obs: RateLimitObservation) {
        let Some(account) = &self.account else { return };
        let Some(book) = &self.account_book else { return };
        let Ok(mut book) = book.lock() else { return };
        book.record_observation(account, obs, ObservationSource::Run);
        if let Err(e) = book.save() {
            tracing::warn!(task_id = %self.task_id, %account, error = %e, "failed to save account book after reviewer rate_limit observation");
        }
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
    /// ADR-0018 D2（監査 4-1）: この tick でクラスタの多重接続が無い／cooldown 中のため待っている ready タスク。「人のログイン待ち」で
    /// 経路なし（`unroutable`）とは別物。`is_idle` の待ち対象から外すだけで、スナップショットには出さない（受信箱の (d) が知らせる）。
    cluster_waiting: std::collections::HashSet<TaskId>,
    /// 人間の承認待ちで延期中の reviewing タスク（`is_idle` 判定用。ADR-0010 D8）。
    awaiting_human: std::collections::HashSet<TaskId>,
    /// ADR-0016 D2 / M5: レビューは全 pass だが、委譲した子が終端になるのを待っている reviewing タスク。
    /// 値はその run の id と、Plan kind なら検証済みの plan（子が終わってから `complete_plan` する）。
    awaiting_children: HashMap<TaskId, AwaitingChildren>,
    tx: mpsc::UnboundedSender<Completion>,
    rx: mpsc::UnboundedReceiver<Completion>,
    /// ADR-0018 D2: 多重接続が無いクラスタの cooldown（この時刻まで dispatch しない）。
    cluster_cooldown: HashMap<String, Instant>,
    /// ADR-0018 D2: 直近の `ssh -O check` の結果（クラスタ id → 多重接続があるか）。`refresh_cluster_liveness` が埋める。
    cluster_connected: HashMap<String, bool>,
    /// ADR-0023 D1: 最後に `ssh -O check` を回した時刻（`CLUSTER_LIVENESS_INTERVAL` に 1 回だけ回す）。
    last_cluster_liveness: Option<Instant>,
    /// tick の回数（スナップショット用）。
    ticks: u64,
    publisher: Option<SnapshotPublisher>,
    /// ADR-0024 D2: `account_pool = true` のプロバイダ id。`reload_providers` で差し替える。
    account_pool_providers: std::collections::HashSet<ProviderId>,
    /// ADR-0024 D4 / ADR-0025 D1: アダプタごとのアカウントの観測値・cooldown・確認の帳簿（設定された根ディレクトリの
    /// アダプタだけキーを持つ）。実行中の run のシンクとも共有する。reload では差し替えない（設定ファイルの
    /// 再読込では消えない観測値）。
    account_books: HashMap<AccountAdapter, Arc<StdMutex<AccountBook>>>,
    /// ADR-0024 D1: 選択のたびにディレクトリを読み直さないよう、tick につき高々 1 回だけスキャンする（アダプタごと）。
    accounts_scan_cache: HashMap<AccountAdapter, Vec<AccountDir>>,
    /// ADR-0024 D5/D7 / ADR-0025 D5: taskd（GUI の管理 API）が進行中のログイン中継を持っているアカウント
    /// （キーは `"<adapter>:<id>"`。同じ id でもアダプタが違えば別のログインとして扱う）。
    login_pending_accounts: std::collections::HashSet<String>,
    /// ADR-0032 D3: `auth = "publickey"` のクラスタに自動で接続を張るフック。`None` なら自動接続しない
    /// （taskd 側が `set_cluster_connector` で挿す。未設定＝従来どおりの挙動）。
    cluster_connector: Option<ClusterConnector>,
    /// ADR-0032 D4/D5: GUI 発の接続（`POST /clusters/{id}/connect`）が進行中のクラスタ id
    /// （taskd が `set_cluster_connect_pending` で反映する。D3 の自動接続とは別物）。
    connect_pending_clusters: std::collections::HashSet<String>,
    /// 壁時計の Unix 秒（テストで差し替えられるようにした関数。既定は実時刻）。
    now_unix_fn: Arc<dyn Fn() -> i64 + Send + Sync>,
}

fn real_now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

impl Dispatcher {
    /// `models` は provider id → `WorkerStarted.model` に記録するモデル名。`account_pool_providers` は
    /// `account_pool = true` のプロバイダ id（ADR-0024 D2）。
    pub fn new(
        store: Arc<dyn TaskStore>,
        policy: Box<dyn ProviderPolicy>,
        models: HashMap<ProviderId, String>,
        adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>>,
        account_pool_providers: std::collections::HashSet<ProviderId>,
        config: DispatchConfig,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        // ADR-0024 D4 / ADR-0025 D1: `<root>/.taskd-usage.json` から観測値・cooldown を読む（無ければ空から始める）。
        // アダプタごとに別の根ディレクトリ・別の帳簿（アカウントの記録はそのアダプタの中で閉じる）。
        let account_books: HashMap<AccountAdapter, Arc<StdMutex<AccountBook>>> = match &config.accounts {
            Some(accounts) => accounts
                .roots
                .iter()
                .map(|(adapter, root)| {
                    (*adapter, Arc::new(StdMutex::new(AccountBook::load(&root.join(".taskd-usage.json")))))
                })
                .collect(),
            None => HashMap::new(),
        };
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
            cluster_waiting: std::collections::HashSet::new(),
            awaiting_human: std::collections::HashSet::new(),
            awaiting_children: HashMap::new(),
            tx,
            rx,
            cluster_cooldown: HashMap::new(),
            cluster_connected: HashMap::new(),
            last_cluster_liveness: None,
            ticks: 0,
            publisher: None,
            account_pool_providers,
            account_books,
            accounts_scan_cache: HashMap::new(),
            login_pending_accounts: std::collections::HashSet::new(),
            cluster_connector: None,
            connect_pending_clusters: std::collections::HashSet::new(),
            now_unix_fn: Arc::new(real_now_unix),
        }
    }

    /// ADR-0032 D3: `auth = "publickey"` のクラスタへの自動接続を有効にする（taskd 側が本番の実装を挿す）。
    /// 呼ばなければ従来どおり自動接続しない。
    pub fn set_cluster_connector(&mut self, connector: ClusterConnector) {
        self.cluster_connector = Some(connector);
    }

    /// ADR-0032 D4/D5: GUI 発の接続が進行中かを記録する（taskd の管理 API が呼ぶ。呼び出しは taskd 側の配線）。
    pub fn set_cluster_connect_pending(&mut self, id: &str, pending: bool) {
        if pending {
            self.connect_pending_clusters.insert(id.to_string());
        } else {
            self.connect_pending_clusters.remove(id);
        }
    }

    pub fn config(&self) -> &DispatchConfig {
        &self.config
    }

    /// テスト用: 壁時計の Unix 秒を差し替える（ADR-0024 D3 のスコア計算・cooldown の期限に使う）。
    pub fn set_now_unix_fn(&mut self, f: Arc<dyn Fn() -> i64 + Send + Sync>) {
        self.now_unix_fn = f;
    }

    /// ADR-0013 D4: tick ごとにデーモンのスナップショットを `watch` に送るようにする。
    pub fn set_snapshot_publisher(&mut self, publisher: SnapshotPublisher) {
        self.publisher = Some(publisher);
    }

    /// ADR-0017 M2: `POST /api/v1/reload` — 稼働中のプロバイダ選定・アダプタ一式を丸ごと差し替える。
    /// 実行中の run はそれぞれ差し替え前のアダプタの `Arc` を既に掴んでいるので影響を受けない（D1）。
    /// `AccountBook`（観測値・cooldown）は reload では差し替えない（ADR-0024 D4: 設定ではなく観測値なので）。
    pub fn reload_providers(
        &mut self,
        policy: Box<dyn ProviderPolicy>,
        models: HashMap<ProviderId, String>,
        adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>>,
        account_pool_providers: std::collections::HashSet<ProviderId>,
    ) {
        self.policy = policy;
        self.models = models;
        self.adapters = adapters;
        self.account_pool_providers = account_pool_providers;
    }

    // ---- ADR-0024/0025: taskd（GUI の管理 API）が使うアカウント操作 ----

    /// そのアダプタ・アカウントで走っている run（ワーカー run + Reviewer run）の数。
    pub fn account_in_use(&self, adapter: AccountAdapter, id: &str) -> usize {
        let matches = |a: &Option<AccountAdapter>, acct: &Option<String>| {
            *a == Some(adapter) && acct.as_deref() == Some(id)
        };
        self.running.values().filter(|e| matches(&e.account_adapter, &e.account)).count()
            + self.reviewing.values().filter(|e| matches(&e.account_adapter, &e.account)).count()
    }

    /// ADR-0025 D1: `login_pending_accounts` のキー（同じ id でもアダプタが違えば別のログインとして扱う）。
    fn login_pending_key(adapter: AccountAdapter, id: &str) -> String {
        format!("{adapter}:{id}")
    }

    /// D7: 進行中のログイン中継の有無を記録する（taskd の `HashMap<String, LoginSession>` と対）。
    pub fn set_account_login_pending(&mut self, adapter: AccountAdapter, id: &str, pending: bool) {
        let key = Self::login_pending_key(adapter, id);
        if pending {
            self.login_pending_accounts.insert(key);
        } else {
            self.login_pending_accounts.remove(&key);
        }
    }

    /// このアダプタの帳簿（設定されていなければ `None`）。
    fn account_book(&self, adapter: AccountAdapter) -> Option<Arc<StdMutex<AccountBook>>> {
        self.account_books.get(&adapter).cloned()
    }

    /// D6: 手動確認の結果を `AccountBook` に記録して保存する（`source = "check"`）。
    pub fn record_account_check(
        &mut self,
        adapter: AccountAdapter,
        id: &str,
        result: &str,
        detail: Option<String>,
        observation: Option<RateLimitObservation>,
    ) {
        let now = (self.now_unix_fn)();
        let Some(book) = self.account_book(adapter) else { return };
        let Ok(mut book) = book.lock() else { return };
        if let Some(obs) = observation {
            book.record_observation(id, obs, ObservationSource::Check);
        }
        book.record_check(id, AccountCheckRecord { at: now, result: result.to_string(), detail });
        if let Err(e) = book.save() {
            tracing::warn!(account_id = %id, %adapter, error = %e, "failed to save account book after check");
        }
    }

    /// D5 `DELETE /accounts/{id}`: 帳簿からもこのアカウントの記録を消す（ディレクトリの移動は taskd/task-api が行う）。
    pub fn remove_account_book_entry(&mut self, adapter: AccountAdapter, id: &str) {
        let Some(book) = self.account_book(adapter) else { return };
        let Ok(mut book) = book.lock() else { return };
        book.remove(id);
        if let Err(e) = book.save() {
            tracing::warn!(account_id = %id, %adapter, error = %e, "failed to save account book after removal");
        }
    }

    /// ADR-0017 M4: 次 tick のスナップショットに乗るプロバイダ一覧を差し替える（`reload_providers` とあわせて呼ぶ）。
    /// `set_snapshot_publisher` より前（`publisher` が無い状態）で呼んでも無害（何もしない）。
    pub fn set_snapshot_providers(&mut self, providers: Vec<ProviderLive>) {
        if let Some(publisher) = self.publisher.as_mut() {
            // ADR-0022 D2: 消えた id の確認記録は落とし、残った id の記録は保つ。
            let ids: std::collections::HashSet<&str> = providers.iter().map(|p| p.id.as_str()).collect();
            publisher.provider_checks.retain(|id, _| ids.contains(id.as_str()));
            publisher.providers = providers;
        }
    }

    /// ADR-0022 D2: 疎通確認の結果をスナップショットに載せる（DB には書かない）。次の tick から `GET /providers` に出る。
    pub fn set_provider_check(&mut self, provider_id: &str, check: ProviderCheckView) {
        if let Some(publisher) = self.publisher.as_mut() {
            publisher.provider_checks.insert(provider_id.to_string(), check);
        }
    }

    /// 1 tick。tokio ランタイム内から呼ぶ（ワーカーとレビューを `tokio::spawn` する）。
    pub fn tick(&mut self) -> Result<TickReport, DispatchError> {
        self.ticks += 1;
        // ADR-0024 D1: 選択のたびに読み直さないよう、スキャンは tick ごとに高々 1 回（このキャッシュを毎 tick 捨てる）。
        self.accounts_scan_cache.clear();
        let now = (self.now_unix_fn)();
        for book in self.account_books.values() {
            if let Ok(mut book) = book.lock() {
                book.clear_expired(now);
            }
        }
        let mut report = TickReport::default();
        // ADR-0015 D2: 遅い tick の内訳を出せるよう、段階ごとに所要時間を測る。
        let started = Instant::now();
        let mut at = Instant::now();
        let lap = |at: &mut Instant| {
            let d = at.elapsed().as_millis() as u64;
            *at = Instant::now();
            d
        };
        let (finished, reviewed) = self.drain_completions()?;
        let drain_ms = lap(&mut at);
        report.finished = finished;
        report.reviewed = reviewed;
        self.settle_awaiting_children()?;
        report.reclaimed = self.reclaim_expired_leases()?;
        let reclaim_ms = lap(&mut at);
        self.abort_stale_runs()?;
        let abort_ms = lap(&mut at);
        self.recover_reviews()?;
        let recover_ms = lap(&mut at);
        self.refresh_cluster_liveness();
        let cluster_ms = lap(&mut at);
        report.dispatched = self.dispatch_ready()?;
        let dispatch_ms = lap(&mut at);
        report.in_flight = self.running.len() + self.reviewing.len();
        report.idle = self.is_idle()?;
        let idle_ms = lap(&mut at);
        self.publish_snapshot();
        if started.elapsed() >= SLOW_TICK {
            tracing::warn!(
                total_ms = started.elapsed().as_millis() as u64,
                drain_ms,
                reclaim_ms,
                abort_ms,
                recover_ms,
                cluster_ms,
                dispatch_ms,
                idle_ms,
                "slow tick phases"
            );
        }
        Ok(report)
    }

    /// ADR-0018 D2 / ADR-0032 D3: 多重接続が無い（または自動接続を試みて失敗した）クラスタを cooldown にし、
    /// 理由をタスクのイベントに残す（人にログインを促すため）。`reason` は呼び出し側が組み立てる
    /// （自動接続を試みて失敗した場合は `"auto-connect failed: ..."` を含め、従来の「接続が無い」だけの文言と区別する）。
    fn mark_cluster_unavailable(
        &mut self,
        task_id: TaskId,
        spec: &ClusterSpec,
        reason: String,
    ) -> Result<(), DispatchError> {
        let first = self
            .cluster_cooldown
            .insert(spec.id.clone(), Instant::now() + self.config.cluster_cooldown)
            .is_none();
        if first {
            tracing::warn!(
                cluster = %spec.id, host = %spec.host, %reason,
                "no ssh ControlMaster connection; run `scripts/cluster-login.sh {}` to log in again", spec.host
            );
        }
        self.store.append_event(
            task_id,
            &Event::ClusterUnavailable { cluster: spec.id.clone(), host: spec.host.clone(), reason },
        )?;
        Ok(())
    }

    /// ADR-0032 D3: `auth = "publickey"` のクラスタに接続フックが刺さっていれば 1 回だけ接続を試みる。
    /// フックが無い、または `auth` が `"publickey"` でなければ `None`（＝試みなかった。呼び出し側は従来どおり
    /// cooldown に落とす）。試みた場合は結果（`Ok(())` = 成功、`Err(detail)` = 失敗の理由）を返す。
    fn try_auto_connect_cluster(&self, spec: &ClusterSpec) -> Option<Result<(), String>> {
        if spec.auth != "publickey" {
            return None;
        }
        let connector = self.cluster_connector.as_ref()?;
        Some(connector(&spec.id, &spec.host))
    }

    /// ADR-0018 D2: 設定の全クラスタについて、多重接続の有無を 1 tick に 1 回調べる（`ssh -O check` は unix ソケットを
    /// 見るだけで即座に返る。ネットワークにも認証にも触れない）。結果は dispatch の判断とスナップショットの `connected` に使う。
    /// 接続が戻っていれば cooldown を解く（人がログインし直したら、次の tick から再開できるように）。
    fn refresh_cluster_liveness(&mut self) {
        if self.config.clusters.is_empty() {
            return;
        }
        // ADR-0023 D1: tick ごとではなく 5 秒に 1 回。外れた判定で dispatch しても、ssh が 255 を返して
        // 供給側失敗（attempts を消費しない cooldown）になるだけなので、多少古くても困らない。
        let now = Instant::now();
        if let Some(last) = self.last_cluster_liveness
            && now.duration_since(last) < CLUSTER_LIVENESS_INTERVAL
        {
            return;
        }
        self.last_cluster_liveness = Some(now);
        let ssh_command = SshSettings::new("", "", "/").ssh_command;
        let mut specs: Vec<(String, String)> =
            self.config.clusters.values().map(|c| (c.id.clone(), c.host.clone())).collect();
        specs.sort();
        for (id, host) in specs {
            let alive = control_master_alive_blocking(&ssh_command, &host);
            self.cluster_connected.insert(id.clone(), alive);
            if alive && self.cluster_cooldown.remove(&id).is_some() {
                tracing::info!(cluster = %id, %host, "ssh ControlMaster connection is back; cluster cooldown cleared");
            }
        }
    }

    /// ADR-0013 D4: メモリ上の状態からスナップショットを作り `watch` に送る（DB には書かない。受け手がいなくても無害）。
    fn publish_snapshot(&mut self) {
        if self.publisher.is_none() {
            return;
        }
        // `&mut self` が要る（アカウントのスキャンキャッシュを埋める）ので、`self.publisher` を借りる前に計算する。
        let (accounts_root, accounts_roots, max_runs_per_account, accounts) = self.accounts_snapshot();
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
                run_id: e.review_run_id.clone().unwrap_or_else(|| e.run_id.clone()),
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
        // ADR-0023 D3: 委譲した子を待っている親（`reviewing` のまま）。GUI が「判定中」と区別して出せるように。
        let mut awaiting_children: Vec<TaskId> = self.awaiting_children.keys().copied().collect();
        awaiting_children.sort();
        let mut unroutable: Vec<TaskId> = self.unroutable.iter().copied().collect();
        unroutable.sort();
        let providers = publisher
            .providers
            .iter()
            .map(|p| ProviderLive {
                in_use: self.provider_in_use(&p.id) as u32,
                last_check: publisher.provider_checks.get(&p.id).cloned(),
                ..p.clone()
            })
            .collect();
        // ADR-0018: クラスタの稼働状況（id 昇順）。`env` の値や `setup` は含めない。
        let mut clusters: Vec<ClusterLive> = self
            .config
            .clusters
            .values()
            .map(|spec| ClusterLive {
                id: spec.id.clone(),
                host: spec.host.clone(),
                concurrency: spec.concurrency,
                in_use: self.cluster_in_use(&spec.id) as u32,
                connected: self.cluster_connected.get(&spec.id).copied().unwrap_or(false),
                cooldown_until: self
                    .cluster_cooldown
                    .get(&spec.id)
                    .filter(|until| **until > now_instant)
                    .map(|until| rfc3339(now + until.saturating_duration_since(now_instant))),
                auth: spec.auth.clone(),
                connect_pending: self.connect_pending_clusters.contains(&spec.id),
            })
            .collect();
        clusters.sort_by(|a, b| a.id.cmp(&b.id));
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
            awaiting_children,
            unroutable,
            providers,
            clusters,
            accounts_root,
            max_runs_per_account,
            accounts_roots,
            accounts,
        };
        // 受け手（API）がいなければ送信は失敗するが、デーモンの動作には関係ない。
        let _ = publisher.tx.send(Some(snapshot));
    }

    /// ADR-0024 D5 / ADR-0025 D6: スナップショットに載せる `accounts_root`（claude-code の別名）/ `accounts_roots`
    /// （アダプタ → 根ディレクトリ）/ `max_runs_per_account` / `accounts[]`（`adapter` → `id` の順）。
    /// `[accounts]` が無ければ全て空。
    #[allow(clippy::type_complexity)]
    fn accounts_snapshot(&mut self) -> (Option<String>, HashMap<String, String>, Option<usize>, Vec<AccountLive>) {
        let Some(cfg) = self.config.accounts.clone() else {
            return (None, HashMap::new(), None, Vec::new());
        };
        let now = (self.now_unix_fn)();
        let mut items = Vec::new();
        let mut roots = HashMap::new();
        for adapter in AccountAdapter::ALL {
            let Some(root) = cfg.root_for(adapter) else { continue };
            roots.insert(adapter.as_str().to_string(), root.display().to_string());
            let dirs = self
                .accounts_scan_cache
                .entry(adapter)
                .or_insert_with(|| scan_accounts(root, adapter))
                .clone();
            let Some(book) = self.account_book(adapter) else { continue };
            let book = book.lock().unwrap_or_else(|e| e.into_inner());
            for d in &dirs {
                let in_use = self.account_in_use(adapter, &d.id);
                let state = book.state(&d.id);
                let eval = evaluate(
                    &AccountCandidate { id: &d.id, logged_in: d.logged_in, in_use },
                    state,
                    cfg.max_runs_per_account,
                    now,
                );
                items.push(AccountLive {
                    adapter: adapter.as_str().to_string(),
                    id: d.id.clone(),
                    logged_in: d.logged_in,
                    in_use: in_use as u32,
                    usage: state.and_then(|s| s.usage.as_ref()).map(|u| AccountUsageLive {
                        five_hour: u.five_hour,
                        seven_day: u.seven_day,
                        status: u.status.clone(),
                        observed_at: u.observed_at,
                        source: match state.and_then(|s| s.source) {
                            Some(ObservationSource::Check) => "check",
                            _ => "run",
                        }
                        .to_string(),
                    }),
                    score: eval.score,
                    excluded_reason: eval.excluded.map(excluded_reason_name).map(str::to_string),
                    cooldown: state.and_then(|s| s.cooldown.as_ref()).map(|c| AccountCooldownLive {
                        until: c.until,
                        reason: account_cooldown_reason_name(c.reason).to_string(),
                    }),
                    last_check: state.and_then(|s| s.last_check.as_ref()).map(|c| ProviderCheckView {
                        at: rfc3339(OffsetDateTime::from_unix_timestamp(c.at).unwrap_or(OffsetDateTime::UNIX_EPOCH)),
                        result: c.result.clone(),
                        detail: c.detail.clone(),
                    }),
                    login_pending: self.login_pending_accounts.contains(&Self::login_pending_key(adapter, &d.id)),
                });
            }
        }
        let accounts_root = roots.get(AccountAdapter::ClaudeCode.as_str()).cloned();
        (accounts_root, roots, Some(cfg.max_runs_per_account), items)
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
        // ADR-0024 D2/D4: プールで選んだアカウント（無ければ `None`）。失敗の cooldown をプロバイダかアカウントか
        // どちらに向けるかを後で決める。
        let (account, account_adapter) = self
            .running
            .remove(&task_id)
            .map(|e| (e.account, e.account_adapter))
            .unwrap_or((None, None));
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
        // ADR-0024 D4 / S10: プール経由の run の失敗は、原因がアカウント側（throttled/auth_failed/exhausted）なら
        // アカウントを cooldown にしプロバイダは cooldown にしない。`Spawn` 失敗（起動できない）はアカウントの
        // 責任ではないので、通常どおりプロバイダを cooldown にする（`failure_reason == Some("spawn")`）。
        let account_at_fault = account.is_some() && failure_reason != Some("spawn");
        let policy_outcome = if account_at_fault { ProviderOutcome::Ok } else { provider_outcome.clone() };
        self.policy.report(provider.clone(), &policy_outcome);

        let finished = Event::WorkerFinished {
            run_id: run_id.clone(),
            outcome: outcome_str.clone(),
            usage,
            role: None,
        };
        let mut events = vec![finished];
        if let Some(reason) = failure_reason {
            match (&account, account_adapter) {
                // ADR-0024 D4: アカウントの cooldown として記録する（`ProviderThrottled` イベントは出さない）。
                (Some(acct), Some(adapter)) if reason != "spawn" => {
                    self.record_account_failure(adapter, acct, reason, &provider_outcome)
                }
                // ADR-0013 D9 / S10: プールを使わない、または Spawn 失敗（アカウント非依存）はプロバイダの
                // cooldown として、遷移と同じトランザクションで記録する。
                _ => {
                    if let Some(ev) = self.provider_throttled_event(&provider, &provider_outcome, reason) {
                        events.push(ev);
                    }
                }
            }
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
        // ADR-0014 D1: Reviewer run の終わりを WorkerFinished{role: reviewer} として残す（判定の適用・延期・破棄のどれでも）。
        let mut reviewer_finished = outcome.reviewer_run.take().map(|r| Event::WorkerFinished {
            run_id: r.run_id,
            outcome: r.outcome,
            usage: r.usage,
            role: Some(RunRole::Reviewer),
        });
        let Some(task) = self.store.get(task_id)? else {
            return Ok(());
        };
        if task.status != Status::Reviewing {
            tracing::warn!(%task_id, status = ?task.status, "review result discarded (task no longer reviewing)");
            if let Some(ev) = &reviewer_finished {
                self.store.append_event(task_id, ev)?;
            }
            return Ok(());
        }
        let mut throttled_events = Vec::new();
        if let Some(pf) = outcome.provider_failure.take() {
            let review_account = entry.as_ref().and_then(|e| e.account.clone());
            let review_account_adapter = entry.as_ref().and_then(|e| e.account_adapter);
            if let Some(provider) = entry.as_ref().and_then(|e| e.provider.clone()) {
                // ADR-0024 D4: プール経由の Reviewer run の失敗もプロバイダを cooldown にせず、アカウントに向ける。
                let policy_outcome = if review_account.is_some() { ProviderOutcome::Ok } else { pf.outcome.clone() };
                self.policy.report(provider.clone(), &policy_outcome);
                match (&review_account, review_account_adapter) {
                    (Some(acct), Some(adapter)) => {
                        self.record_account_failure(adapter, acct, cooldown_reason_name(&pf.outcome), &pf.outcome)
                    }
                    _ => {
                        if let Some(ev) = self.provider_throttled_event(&provider, &pf.outcome, cooldown_reason_name(&pf.outcome)) {
                            throttled_events.push(ev);
                        }
                    }
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
                if let Some(ev) = &reviewer_finished {
                    self.store.append_event(task_id, ev)?;
                }
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
            if let Some(Event::WorkerFinished { outcome: finished_outcome, .. }) = reviewer_finished.as_mut() {
                *finished_outcome = format!(
                    "error(retryable=false): requeue limit ({}) reached: {}",
                    self.config.max_requeues, pf.message
                );
            }
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
        let mut events: Vec<Event> = reviewer_finished
            .into_iter()
            .chain(outcome.verdicts.iter().map(|v| Event::ReviewVerdict {
                run_id: run_id.clone(),
                criterion_idx: v.criterion_idx,
                pass: v.pass,
                reason: v.reason.clone(),
            }))
            .chain(throttled_events)
            .collect();
        // ADR-0016 D2 / M5: 全 pass でも委譲した子が終端でなければ、判定だけ記録して reviewing のまま待つ。
        if all_pass {
            let pending = pending_children(self.store.as_ref(), task_id).map_err(ops_to_store)?;
            if pending > 0 {
                for ev in &events {
                    self.store.append_event(task_id, ev)?;
                }
                self.store.append_event(
                    task_id,
                    &Event::WorkerProgress {
                        run_id: run_id.clone(),
                        msg: format!("waiting for {pending} delegated child task(s) before completing"),
                    },
                )?;
                self.awaiting_children.insert(
                    task_id,
                    AwaitingChildren {
                        run_id: run_id.clone(),
                        plan: outcome.plan,
                    },
                );
                tracing::info!(%task_id, %run_id, pending, "review passed; waiting for delegated children");
                return Ok(());
            }
            // ADR-0021 D1: 子が失敗していたら、集約・完了より先に「やり直す or 人に聞く」。
            if self.escalate_failed_children(&task, &run_id, &mut events)? {
                return Ok(());
            }
            // ADR-0016 D3 / M4: 子が全て終端で、まだ集約 run をしていなければ集約 run を予約する。
            if self.needs_aggregate_run(&task)? {
                return self.schedule_aggregate_run(task_id, &run_id, events);
            }
        }
        // ADR-0007 D3/D4: Plan が全 pass なら子タスクの挿入と ReviewPass を同一トランザクションで行う。
        let result = match (all_pass, task.kind, outcome.plan) {
            (true, TaskKind::Plan, Some(plan)) => {
                let children = materialize(&task, &plan, &self.config.roles, &self.config.genres, OffsetDateTime::now_utc());
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

    /// ADR-0016 M4: `aggregate = true` で、Approval 以外の子が 1 件以上あり、まだ集約遷移をしていないか。
    fn needs_aggregate_run(&self, task: &Task) -> Result<bool, DispatchError> {
        if !task.aggregate {
            return Ok(false);
        }
        let has_children = self
            .store
            .children(task.id)?
            .iter()
            .any(|c| c.kind != TaskKind::Approval);
        if !has_children {
            return Ok(false);
        }
        let events = self.store.events_for(task.id)?;
        Ok(!has_aggregate_transition(&events))
    }

    /// ADR-0021 D1/D3: 委譲した子（`Event::Delegated`）のうち、**前回この親が子の失敗を扱ってから後に** `failed` に
    /// なったもの。一度扱った失敗は数え直さない（親が別の子に割り当て直して成功したのに、古い失敗で止まらないため）。
    /// 返り値は id 順の `(子, 直近のワーカー run の outcome)`。
    fn newly_failed_delegated_children(&self, task_id: TaskId) -> Result<Vec<(Task, Option<String>)>, DispatchError> {
        let events = self.store.events_for(task_id)?;
        // 直近に子の失敗を扱った時点（グローバル id。イベント id は単調増加）。
        let handled_at = events
            .iter()
            .rev()
            .find_map(|(id, e)| match e {
                Event::Transitioned { reason, .. } if reason == Trigger::ChildFailed.name() => Some(*id),
                _ => None,
            })
            .unwrap_or(0);
        let mut delegated: Vec<TaskId> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::Delegated { task_ids, .. } => Some(task_ids.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        delegated.sort();
        delegated.dedup();

        let mut out = Vec::new();
        for id in delegated {
            let Some(child) = self.store.get(id)? else { continue };
            if child.status != Status::Failed {
                continue;
            }
            let child_events = self.store.events_for(id)?;
            let failed_at = child_events.iter().rev().find_map(|(eid, e)| match e {
                Event::Transitioned { to: Status::Failed, .. } => Some(*eid),
                _ => None,
            });
            // 既に扱った失敗（id が前回の child_failed より前）は数えない。
            if failed_at.is_some_and(|at| at <= handled_at) {
                continue;
            }
            let outcome = child_events.iter().rev().find_map(|(_, e)| match e {
                Event::WorkerFinished { outcome, role: None, .. } => Some(outcome.clone()),
                _ => None,
            });
            out.push((child, outcome));
        }
        Ok(out)
    }

    /// ADR-0021 D1/D2: 委譲した子が失敗していたら、親をやり直す（attempts に余裕があるとき）か、
    /// 人間に質問して待つ（`blocked`）。**親を `failed` にはしない。** 扱ったら `true`（`events` も記録済み）。
    /// `events`（判定結果など、まだ記録していないもの）は、**扱ったときだけ**同じトランザクションで一緒に記録する。
    /// 扱わなかったときは触らない（呼び出し側がそのまま使う）。
    fn escalate_failed_children(
        &mut self,
        task: &Task,
        run_id: &str,
        events: &mut Vec<Event>,
    ) -> Result<bool, DispatchError> {
        if self.config.delegation.on_child_failure == OnChildFailure::Ignore {
            return Ok(false);
        }
        let failed = self.newly_failed_delegated_children(task.id)?;
        if failed.is_empty() {
            return Ok(false);
        }
        let mut events = std::mem::take(events);
        let listed = failed
            .iter()
            .map(|(child, outcome)| {
                format!("{} ({}): {}", child.title, child.id, outcome.as_deref().unwrap_or("(no outcome recorded)"))
            })
            .collect::<Vec<_>>()
            .join("; ");
        // 状態機械と同じ判定（ADR-0021 D1）。ここで分かるのは「やり直せるか」だけ。
        let will_retry = task.attempts < task.budget.max_retries;
        if will_retry {
            events.push(Event::WorkerProgress {
                run_id: run_id.to_string(),
                msg: format!(
                    "{} delegated child task(s) failed; retrying this task (attempt {}/{}): {listed}",
                    failed.len(),
                    task.attempts + 1,
                    task.budget.max_retries,
                ),
            });
        } else {
            let text = format!(
                "委譲した子タスクが失敗し、やり直し（max_retries = {}）でも解決しませんでした。どうしますか。\n\
                 失敗した子: {listed}\n\
                 回答するとこのタスクは指示を持って再開します: taskctl answer {} \"…\"",
                task.budget.max_retries, task.id,
            );
            events.push(Event::QuestionRaised { run_id: run_id.to_string(), text });
        }
        match self.store.apply_transition_with_events(task.id, Trigger::ChildFailed, events) {
            Ok(outcome) => {
                tracing::info!(
                    task_id = %task.id, %run_id, next = ?outcome.next, failed = failed.len(),
                    "delegated child task(s) failed; parent retries or asks a human (ADR-0021)"
                );
            }
            Err(StoreError::InvalidTransition(e)) => {
                tracing::warn!(task_id = %task.id, error = %e, "child_failed transition could not be applied");
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        }
        Ok(true)
    }

    /// ADR-0016 M1 / M4: `Aggregate`（reviewing → ready、attempts 据え置き）を適用し、次の dispatch を集約 run にする。
    fn schedule_aggregate_run(&mut self, task_id: TaskId, run_id: &str, mut events: Vec<Event>) -> Result<(), DispatchError> {
        let children = self.store.children(task_id)?.len();
        events.push(Event::WorkerProgress {
            run_id: run_id.to_string(),
            msg: format!("all {children} delegated child task(s) finished; scheduling the aggregate run"),
        });
        match self.store.apply_transition_with_events(task_id, Trigger::Aggregate, events) {
            Ok(outcome) => {
                tracing::info!(%task_id, %run_id, next = ?outcome.next, "aggregate run scheduled");
            }
            Err(StoreError::InvalidTransition(e)) => {
                tracing::warn!(%task_id, error = %e, "aggregate transition could not be applied");
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    /// ADR-0016 M5: 子待ちの親を毎 tick 数え直し、全て終端になったら集約 run（M4）か `ReviewPass`（Plan なら `complete_plan`）。
    fn settle_awaiting_children(&mut self) -> Result<(), DispatchError> {
        let ids: Vec<TaskId> = self.awaiting_children.keys().copied().collect();
        for task_id in ids {
            let Some(task) = self.store.get(task_id)? else {
                self.awaiting_children.remove(&task_id);
                continue;
            };
            if task.status != Status::Reviewing {
                self.awaiting_children.remove(&task_id);
                continue;
            }
            let pending = pending_children(self.store.as_ref(), task_id).map_err(ops_to_store)?;
            if pending > 0 {
                continue;
            }
            let Some(waiting) = self.awaiting_children.remove(&task_id) else {
                continue;
            };
            // ADR-0021 D1: 子が失敗していたら、集約・完了より先に「やり直す or 人に聞く」。
            if self.escalate_failed_children(&task, &waiting.run_id, &mut Vec::new())? {
                continue;
            }
            if self.needs_aggregate_run(&task)? {
                self.schedule_aggregate_run(task_id, &waiting.run_id, Vec::new())?;
                continue;
            }
            let result = match (task.kind, waiting.plan) {
                (TaskKind::Plan, Some(plan)) => {
                    let children = materialize(&task, &plan, &self.config.roles, &self.config.genres, OffsetDateTime::now_utc());
                    self.store
                        .complete_plan(task_id, Vec::new(), children, self.config.plan_auto_accept)
                }
                _ => self
                    .store
                    .apply_transition_with_events(task_id, Trigger::ReviewPass, Vec::new()),
            };
            match result {
                Ok(outcome) => {
                    tracing::info!(%task_id, run_id = %waiting.run_id, next = ?outcome.next, "delegated children finished; parent completed");
                }
                Err(StoreError::InvalidTransition(e)) => {
                    tracing::warn!(%task_id, error = %e, "parent completion could not be applied");
                }
                Err(e) => return Err(e.into()),
            }
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
                role: None,
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
            if self.reviewing.contains_key(&task.id) || self.awaiting_children.contains_key(&task.id) {
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
        self.cluster_waiting.clear();
        if self.workers_in_flight() >= self.config.max_concurrency {
            return Ok(0);
        }
        // 上位から見て見送りが続いても後続を試せるよう、窓は広めに取る。
        let window = self.ready_window();
        let ready_started = Instant::now();
        let candidates = self.store.ready_tasks(window)?;
        log_slow_step("ready_tasks", ready_started);
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
            // ADR-0018: リモート実行のタスクは、クラスタの設定・cooldown・並列度・多重接続を先に確かめる。
            let cluster = match &task.workspace {
                WorkspaceSpec::Local { .. } => None,
                WorkspaceSpec::Remote { cluster, .. } => match self.cluster_of(&task) {
                    Some(resolved) => Some(resolved),
                    None => {
                        if self.warned_unroutable.insert(task.id) {
                            tracing::warn!(task_id = %task.id, %cluster, "no such cluster in the config; task left ready");
                        }
                        self.unroutable.insert(task.id);
                        continue;
                    }
                },
            };
            if let Some((spec, _)) = &cluster {
                if self.cluster_cooldown.get(&spec.id).is_some_and(|until| *until > now) {
                    // ADR-0018 D2: 人がログインするまで進まないので、待ち対象には数えない（`--until-idle` を止めない）。
                    self.cluster_waiting.insert(task.id);
                    continue;
                }
                if self.cluster_in_use(&spec.id) >= spec.concurrency {
                    continue;
                }
                // この tick の `refresh_cluster_liveness` の結果を使う（1 tick に 1 回だけ `ssh -O check` を呼ぶ）。
                let alive = self.cluster_connected.get(&spec.id).copied().unwrap_or(false);
                if !alive {
                    let spec = spec.clone();
                    // ADR-0032 D3: `auth = "publickey"` かつ接続フックがあれば、cooldown にする前に
                    // 1 回だけ接続を試みる（cooldown 中はここに来ないので、tick ごとに ssh は湧かない。
                    // 同じ tick の別タスクが同じクラスタを指していても、成功時は `cluster_connected` の
                    // キャッシュが true になり、失敗時は下で cooldown が立つので、2 本目は走らない）。
                    match self.try_auto_connect_cluster(&spec) {
                        Some(Ok(())) => {
                            self.cluster_connected.insert(spec.id.clone(), true);
                        }
                        Some(Err(detail)) => {
                            self.mark_cluster_unavailable(
                                task.id,
                                &spec,
                                format!("auto-connect failed: {detail}"),
                            )?;
                            self.cluster_waiting.insert(task.id);
                            continue;
                        }
                        None => {
                            self.mark_cluster_unavailable(
                                task.id,
                                &spec,
                                format!("no ssh ControlMaster connection to {} (host {})", spec.id, spec.host),
                            )?;
                            self.cluster_waiting.insert(task.id);
                            continue;
                        }
                    }
                }
            }
            let dir_started = Instant::now();
            let Some(dir) = self.task_dir(&task) else {
                tracing::warn!(task_id = %task.id, "cannot resolve the workspace directory; task left ready");
                continue;
            };
            log_slow_step("task_dir", dir_started);
            let Some((adapter_id, provider_id, selected_account)) =
                self.select_provider(&task.worker_hint, now, task.id, &mut full)
            else {
                continue;
            };
            let Some(base_adapter) = self.adapters.get(&provider_id).cloned() else {
                tracing::warn!(task_id = %task.id, provider = %provider_id, adapter = %adapter_id, "no adapter instance for provider");
                continue;
            };
            // ADR-0024 D2 / ADR-0025 D2: プールで選んだアカウントの env を重ねる。`with_env` が `None` を返すのは
            // アダプタの実装漏れ（設定検証で account_pool は claude-code/codex 限定にしているため通常は起きない）
            // なので、このタスクは今回見送る。
            let adapter = match &selected_account {
                Some((account_adapter, account_id)) => match self.adapter_for_account(&base_adapter, *account_adapter, account_id) {
                    Some(a) => a,
                    None => {
                        tracing::warn!(task_id = %task.id, provider = %provider_id, account_id, "adapter does not support account pools (with_env returned None); skipping this tick");
                        continue;
                    }
                },
                None => base_adapter,
            };
            let account = selected_account.as_ref().map(|(_, id)| id.clone());
            let account_adapter = selected_account.as_ref().map(|(a, _)| *a);

            let run_id = ulid::Ulid::new().to_string();
            let wall = Duration::from_secs(task.budget.max_wall_secs);
            let ttl = wall + self.config.lease_grace;
            let lease_started = Instant::now();
            let acquired = self.store.acquire_lease(task.id, &run_id, ttl)?;
            log_slow_step("acquire_lease", lease_started);
            if !acquired {
                continue;
            }
            let model = self.models.get(&provider_id).cloned().unwrap_or_default();
            let event_started = Instant::now();
            self.store.append_event(
                task.id,
                &Event::WorkerStarted {
                    run_id: run_id.clone(),
                    adapter: adapter_id.clone(),
                    model,
                    provider: Some(provider_id.clone()),
                    // ADR-0024 D4: `account_pool` のプロバイダで選んだアカウント（プールを使わなければ `None`）。
                    account: account.clone(),
                    role: None,
                    task_role: task.role.clone(),
                },
            )?;
            log_slow_step("append_worker_started", event_started);
            let limits = RunLimits {
                wall_clock: wall,
                idle_timeout: self.config.idle_timeout,
                kill_grace: self.config.kill_grace,
            };
            tracing::info!(task_id = %task.id, %run_id, adapter = %adapter_id, provider = %provider_id, account = account.as_deref(), "dispatching");
            let remote = cluster.as_ref().map(|(spec, path)| spec.ssh_settings(path, task.id));
            let extras = self.run_extras(&task)?;
            let handle = self.spawn_worker(
                task.id,
                run_id.clone(),
                provider_id.clone(),
                account.clone(),
                account_adapter,
                adapter,
                dir,
                limits,
                remote,
                extras,
            );
            self.running.insert(
                task.id,
                RunEntry {
                    run_id,
                    provider: provider_id,
                    handle,
                    since: OffsetDateTime::now_utc(),
                    cluster: cluster.map(|(spec, _)| spec.id),
                    account,
                    account_adapter,
                },
            );
            dispatched += 1;
        }
        Ok(dispatched)
    }

    /// ADR-0016 D1 / D3, ADR-0027 D1: run 開始時にワーカーへ渡す役割の指示文、委譲できる run なら使える
    /// 分野の一覧、集約 run なら子の要約。
    fn run_extras(&self, task: &Task) -> Result<RunExtras, DispatchError> {
        let role = task.role.as_deref().map(|id| RoleContext {
            id: id.to_string(),
            instructions: RoleSpec::find(&self.config.roles, id)
                .and_then(|r| r.instructions.clone())
                .unwrap_or_default(),
        });
        // ADR-0027 D1 / ADR-0028 D3: 委譲の指示文を出す run（Execute/Approval）と、子の分野を選べる
        // Plan run（`build_plan_prompt` も同じ節を出す）にだけ使える分野の一覧を渡す
        // （プロンプト側の条件と同じ。`claude_code::build_prompt` 参照）。
        let available_genres = if matches!(task.kind, TaskKind::Execute | TaskKind::Approval | TaskKind::Plan) {
            self.config.genres.iter().map(GenreContext::from).collect()
        } else {
            Vec::new()
        };
        let events = self.store.events_for(task.id)?;
        // 集約 run（ADR-0016 D3）と、子の失敗によるやり直し run（ADR-0021 D1）は、子の結果を見て判断する。
        let children = if (task.aggregate && has_aggregate_transition(&events)) || has_child_failed_transition(&events) {
            let mut out = Vec::new();
            for child in self.store.children(task.id)? {
                if child.kind == TaskKind::Approval {
                    continue;
                }
                let child_events = self.store.events_for(child.id)?;
                let outcome = child_events.iter().rev().find_map(|(_, e)| match e {
                    Event::WorkerFinished { outcome, role: None, .. } => Some(outcome.clone()),
                    _ => None,
                });
                let artifacts = child_events
                    .iter()
                    .filter_map(|(_, e)| match e {
                        Event::ArtifactProduced { artifact, .. } => Some(artifact.clone()),
                        _ => None,
                    })
                    .collect();
                out.push(ChildSummary {
                    id: child.id,
                    title: child.title.clone(),
                    role: child.role.clone(),
                    status: child.status,
                    outcome,
                    artifacts,
                    workspace: self.task_dir(&child),
                });
            }
            out
        } else {
            Vec::new()
        };
        Ok(RunExtras { role, children, available_genres })
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_worker(
        &self,
        task_id: TaskId,
        run_id: String,
        provider: ProviderId,
        account: Option<String>,
        account_adapter: Option<AccountAdapter>,
        adapter: Arc<dyn WorkerAdapter>,
        dir: PathBuf,
        limits: RunLimits,
        remote: Option<SshSettings>,
        extras: RunExtras,
    ) -> JoinHandle<()> {
        let store = self.store.clone();
        let tx = self.tx.clone();
        let lease = LeaseRenewal {
            ttl: self.config.idle_timeout + self.config.lease_grace,
            every: self.config.lease_grace / 2,
        };
        let roles = self.config.roles.clone();
        let genres = self.config.genres.clone();
        let delegation = self.config.delegation;
        let account_book = account_adapter.and_then(|a| self.account_book(a));
        tokio::spawn(async move {
            let result = run_worker(
                store,
                adapter,
                task_id,
                dir,
                &run_id,
                limits,
                lease,
                remote,
                extras,
                roles,
                genres,
                delegation,
                account,
                account_book,
            )
            .await;
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
        let provider = reviewer.as_ref().map(|(p, _, _)| p.clone());
        // ADR-0024 D2: `account_pool` で選んだアカウント（プールを使わない、または Reviewer run を起動しない場合は `None`）。
        let selected_account = reviewer.as_ref().and_then(|(_, a, _)| a.clone());
        let account = selected_account.as_ref().map(|(_, id)| id.clone());
        let account_adapter = selected_account.as_ref().map(|(a, _)| *a);
        // ADR-0014 D1: (provider, Reviewer run の id, adapter) — WorkerStarted の記録と in_flight に使う。
        let review_run = reviewer.as_ref().map(|(p, _, r)| (p.clone(), r.run_id.clone(), r.adapter.id().to_string()));
        let reviewer_run = reviewer.map(|(_, _, r)| r);

        let plan = if task.kind == TaskKind::Plan {
            Some(PlanCheck {
                depth: self.plan_depth(&task)?,
                limits: PlanLimits::default(),
                genres: self.config.genres.clone(),
            })
        } else {
            None
        };
        // ADR-0016 M4: 集約 run のレビューには暗黙の条件「artifacts/summary.md がある」が加わる。
        let aggregate = task.aggregate && has_aggregate_transition(&self.store.events_for(task_id)?);

        // ADR-0018: 判定コマンドもクラスタで実行する。
        let cluster = self.cluster_of(&task);
        let remote_settings = cluster.as_ref().map(|(spec, path)| spec.ssh_settings(path, task.id));
        let cluster_id = cluster.as_ref().map(|(spec, _)| spec.id.clone());
        let events = self.store.events_for(task_id)?;
        let produced = artifacts_for_run(&events, &run_id);
        // ADR-0014 D1: Reviewer run も対象タスクに WorkerStarted（role: reviewer）を残す（アカウント別の集計に含めるため）。
        if let Some((provider_id, review_run_id, adapter_id)) = &review_run {
            let model = self.models.get(provider_id).cloned().unwrap_or_default();
            self.store.append_event(
                task_id,
                &Event::WorkerStarted {
                    run_id: review_run_id.clone(),
                    adapter: adapter_id.clone(),
                    model,
                    provider: Some(provider_id.clone()),
                    account: account.clone(),
                    role: Some(RunRole::Reviewer),
                    task_role: None,
                },
            )?;
        }
        let timeout = self.config.review_timeout;
        let entry_subject = subject.clone();
        let entry_run_id = run_id.clone();
        let subject = subject.clone();
        let tx = self.tx.clone();
        let remote_review = remote_settings.clone();
        let handle = tokio::spawn(async move {
            let ws: Box<dyn Workspace> = match remote_review {
                Some(settings) => Box::new(SshWorkspace::new(&dir, settings)),
                None => Box::new(LocalWorkspace::new(&dir)),
            };
            let extras = ReviewExtras {
                subject,
                plan,
                reviewer: reviewer_run,
                human,
                aggregate,
            };
            let outcome = review_task(&task, ws.as_ref(), &dir, &produced, timeout, extras).await;
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
                review_run_id: review_run.map(|(_, id, _)| id),
                since: OffsetDateTime::now_utc(),
                cluster: cluster_id,
                account,
                account_adapter,
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
            role: None,
            genre: None,
            aggregate: false,
        };
        // ADR-0010 D2: 挿入・Created・ApprovalRequested を 1 トランザクションで。
        self.store.create_task(&approval, vec![Event::ApprovalRequested])?;
        tracing::info!(task_id = %task.id, approval_id = %approval.id, criterion_idx = idx, "created approval child for human check");
        Ok(approval)
    }

    /// `Reviewer` run のアダプタ／プロバイダを選ぶ（ADR-0007 D5 1.）。並列度の枠は実行中 run と共有する。
    /// ADR-0024 D2: 選んだプロバイダが `account_pool` ならアカウントも選ぶ（戻り値の第 2 要素）。
    #[allow(clippy::type_complexity)]
    fn pick_reviewer(
        &mut self,
        task: &Task,
        subject_run_id: &str,
    ) -> Option<(ProviderId, Option<(AccountAdapter, String)>, ReviewerRun)> {
        if self.workers_in_flight() >= self.config.max_concurrency {
            return None;
        }
        // ADR-0012 D2: ワーカー run と同じ手順（上限のプロバイダを飛ばして次へ、候補なしは warn）で選ぶ。
        let hint = self.config.reviewer_hint.clone();
        let mut full = std::collections::HashSet::new();
        let (adapter_id, provider_id, selected_account) = self.select_provider(&hint, Instant::now(), task.id, &mut full)?;
        let base_adapter = match self.adapters.get(&provider_id) {
            Some(a) => a.clone(),
            None => {
                tracing::warn!(task_id = %task.id, provider = %provider_id, adapter = %adapter_id, "no adapter instance for reviewer provider");
                return None;
            }
        };
        let adapter = match &selected_account {
            Some((account_adapter, account_id)) => match self.adapter_for_account(&base_adapter, *account_adapter, account_id) {
                Some(a) => a,
                None => {
                    tracing::warn!(task_id = %task.id, provider = %provider_id, account_id, "adapter does not support account pools for reviewer run; deferring");
                    return None;
                }
            },
            None => base_adapter,
        };
        let account = selected_account.as_ref().map(|(_, id)| id.clone());
        let account_adapter = selected_account.as_ref().map(|(a, _)| *a);
        let review_run_id = ulid::Ulid::new().to_string();
        let sink = ReviewerSink {
            store: self.store.clone(),
            task_id: task.id,
            subject_run_id: subject_run_id.to_string(),
            review_run_id: review_run_id.clone(),
            account: account.clone(),
            account_book: account_adapter.and_then(|a| self.account_book(a)),
        };
        tracing::info!(task_id = %task.id, %review_run_id, adapter = %adapter_id, provider = %provider_id, account = account.as_deref(), "starting reviewer run");
        Some((
            provider_id,
            selected_account,
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
    /// ADR-0024 D2: 選んだプロバイダが `account_pool = true` なら、続けて D3 でアカウントを選ぶ。選べるアカウントが
    /// 無ければそのプロバイダを満杯として扱い（除外集合に入れて）次の候補へ進む。戻り値の第 3 要素が選んだアカウント
    /// （プールを使わないプロバイダなら `None`）。
    #[allow(clippy::type_complexity)]
    fn select_provider(
        &mut self,
        hint: &task_core::WorkerHint,
        now: Instant,
        task_id: TaskId,
        full: &mut std::collections::HashSet<ProviderId>,
    ) -> Option<(AdapterId, ProviderId, Option<(AccountAdapter, String)>)> {
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
                    if self.account_pool_providers.contains(&provider) {
                        // ADR-0025 D1: プールのアダプタは、そのプロバイダ自身のワーカーアダプタと同じ
                        // （`account_pool = true` は claude-code/codex 限定。設定検証済み）。
                        let Some(account_adapter) = AccountAdapter::parse(&adapter) else {
                            tracing::warn!(%task_id, %provider, %adapter, "account_pool provider has an adapter that is not a pool adapter; treating as full");
                            full.insert(provider);
                            continue;
                        };
                        match self.pick_account(account_adapter) {
                            Some(account_id) => {
                                self.warned_unroutable.remove(&task_id);
                                return Some((adapter, provider, Some((account_adapter, account_id))));
                            }
                            None => {
                                tracing::debug!(%task_id, %provider, "no eligible account in the pool; trying the next provider");
                                full.insert(provider);
                                continue;
                            }
                        }
                    }
                    self.warned_unroutable.remove(&task_id);
                    return Some((adapter, provider, None));
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

    /// ADR-0024 D3 / ADR-0025 D2: `[accounts]` の指定アダプタのプールから 1 アカウントを選ぶ（残量に基づく決定的な
    /// 選択）。そのアダプタの根ディレクトリが無い、または選べるアカウントが無ければ `None`。
    /// ディレクトリのスキャンは tick につき高々 1 回（アダプタごと）。
    fn pick_account(&mut self, adapter: AccountAdapter) -> Option<String> {
        let cfg = self.config.accounts.clone()?;
        let root = cfg.root_for(adapter)?;
        let dirs = self.accounts_scan_cache.entry(adapter).or_insert_with(|| scan_accounts(root, adapter)).clone();
        let now = (self.now_unix_fn)();
        let book = self.account_book(adapter)?;
        let book = book.lock().unwrap_or_else(|e| e.into_inner());
        let candidates: Vec<AccountCandidate<'_>> = dirs
            .iter()
            .map(|d| AccountCandidate { id: d.id.as_str(), logged_in: d.logged_in, in_use: self.account_in_use(adapter, &d.id) })
            .collect();
        select_account(&candidates, &book, cfg.max_runs_per_account, now)
    }

    /// ADR-0024 D4: プール run の供給側失敗をアカウントの cooldown として記録する（プロバイダは cooldown にしない）。
    /// `reason` は `provider_failure_reason` と同じ語彙（`throttled` / `auth_failed` / `exhausted` / `spawn`）。
    fn record_account_failure(&self, adapter: AccountAdapter, account_id: &str, reason: &str, outcome: &ProviderOutcome) {
        let Some(cfg) = &self.config.accounts else { return };
        let now = (self.now_unix_fn)();
        let fallback_secs = match outcome {
            ProviderOutcome::Throttled { retry_after } if retry_after.as_secs() > 0 => retry_after.as_secs(),
            _ => cfg.fallback_cooldown_secs,
        };
        let cooldown_reason = account_cooldown_reason_from_failure(reason);
        let Some(book) = self.account_book(adapter) else { return };
        let Ok(mut book) = book.lock() else { return };
        let cooldown = {
            let state = book.state(account_id);
            cooldown_for_failure(state, cooldown_reason, now, fallback_secs)
        };
        book.set_cooldown(account_id, cooldown, now);
        if let Err(e) = book.save() {
            tracing::warn!(%account_id, %adapter, error = %e, "failed to save account book after cooldown");
        }
    }

    /// ADR-0024 D2 / ADR-0025 D2: プールから選んだアカウントの環境変数（claude-code は
    /// `CLAUDE_SECURESTORAGE_CONFIG_DIR`、codex は `CODEX_HOME`）を末尾に重ねたアダプタを返す。`with_env` が
    /// `None`（アダプタがこの経路を実装していない）なら `None`（呼び出し側は満杯として扱う）。
    fn adapter_for_account(
        &self,
        base: &Arc<dyn WorkerAdapter>,
        account_adapter: AccountAdapter,
        account_id: &str,
    ) -> Option<Arc<dyn WorkerAdapter>> {
        let cfg = self.config.accounts.as_ref()?;
        let root = cfg.root_for(account_adapter)?;
        let dir = root.join(account_id);
        base.with_env(&[(account_adapter.env_var().to_string(), dir.display().to_string())])
    }

    /// ADR-0005 D3: `Local{path}` がそのタスクの作業ディレクトリ。相対なら `workspace_root` 基準。
    fn task_dir(&self, task: &Task) -> Option<PathBuf> {
        match &task.workspace {
            WorkspaceSpec::Local { path } => Some(if path.is_absolute() {
                path.clone()
            } else {
                self.config.workspace_root.join(path)
            }),
            // ADR-0018 D1: クラスタ側が正で、手元は写し（`workspace_root/<task_id>`）。
            WorkspaceSpec::Remote { .. } => Some(self.config.workspace_root.join(task.id.to_string())),
        }
    }

    /// ADR-0018: `WorkspaceSpec::Remote` のタスクのクラスタ設定とリモートのパス。ローカルのタスクは `None`。
    fn cluster_of(&self, task: &Task) -> Option<(ClusterSpec, PathBuf)> {
        match &task.workspace {
            WorkspaceSpec::Local { .. } => None,
            WorkspaceSpec::Remote { cluster, path } => self
                .config
                .clusters
                .get(cluster)
                .map(|spec| (spec.clone(), path.clone())),
        }
    }

    /// そのクラスタで走っている run の数（ADR-0018 D5: プロバイダとクラスタの二次元）。
    fn cluster_in_use(&self, cluster: &str) -> usize {
        self.running.values().filter(|e| e.cluster.as_deref() == Some(cluster)).count()
            + self.reviewing.values().filter(|e| e.cluster.as_deref() == Some(cluster)).count()
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
            .any(|t| !self.awaiting_human.contains(&t.id) && !self.awaiting_children.contains_key(&t.id))
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
        Ok(ready
            .iter()
            .all(|t| self.unroutable.contains(&t.id) || self.cluster_waiting.contains(&t.id)))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_worker(
    store: Arc<dyn TaskStore>,
    adapter: Arc<dyn WorkerAdapter>,
    task_id: TaskId,
    dir: PathBuf,
    run_id: &str,
    limits: RunLimits,
    lease: LeaseRenewal,
    remote: Option<SshSettings>,
    extras: RunExtras,
    roles: Vec<RoleSpec>,
    genres: Vec<GenreSpec>,
    delegation: DelegationLimits,
    account: Option<String>,
    account_book: Option<Arc<StdMutex<AccountBook>>>,
) -> Result<RunOutcome, AdapterError> {
    // リース取得後の状態（running, lease あり）をワーカーに渡す。
    let mut task = store
        .get(task_id)
        .map_err(|e| AdapterError::Other(format!("store: {e}")))?
        .ok_or_else(|| AdapterError::Other("task vanished".into()))?;
    // ADR-0018 D1/D3: リモート実行のタスクは、クラスタの内容を写しに取り込み、ラッパを置き、その使い方を指示文に足す
    // （DB のタスクは変えない。ワーカーに渡す写しだけ）。
    let workspace = match &remote {
        Some(settings) => {
            let ws = SshWorkspace::new(&dir, settings.clone());
            let prepared = ws
                .prepare(&task)
                .await
                .map_err(|e| workspace_error_to_adapter(e, "workspace prepare"))?;
            ws.write_remote_exec_helper()
                .await
                .map_err(|e| workspace_error_to_adapter(e, "remote-exec helper"))?;
            task.objective.push_str(&remote_exec_instructions(settings));
            prepared
        }
        None => LocalWorkspace::new(&dir)
            .prepare(&task)
            .await
            .map_err(|e| AdapterError::Other(format!("workspace prepare: {e}")))?,
    };
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
            role: extras.role,
            children: extras.children,
            available_genres: extras.available_genres,
        },
    };
    let sink = StoreSink {
        store,
        task_id,
        run_id: run_id.to_string(),
        lease_ttl: lease.ttl,
        renew_every: lease.every,
        last_renew: std::sync::Mutex::new(Instant::now()),
        roles,
        genres,
        delegation,
        delegated_this_run: std::sync::atomic::AtomicUsize::new(0),
        account,
        account_book,
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

/// ADR-0018 D5: ワークスペースの失敗をアダプタの失敗に写す。`Unreachable`（ssh / rsync 自体の失敗）は
/// 供給側失敗（`Spawn`）にして、attempts を消費せず requeue されるようにする。
fn workspace_error_to_adapter(e: task_worker::WorkspaceError, context: &str) -> AdapterError {
    match e {
        task_worker::WorkspaceError::Unreachable(msg) => {
            AdapterError::Spawn(std::io::Error::other(format!("{context}: {msg}")))
        }
        other => AdapterError::Other(format!("{context}: {other}")),
    }
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

/// ADR-0024 D3: `AccountView.excluded_reason` の語彙（`docs/gui/api.md` §3.29）。
fn excluded_reason_name(reason: ExcludedReason) -> &'static str {
    match reason {
        ExcludedReason::NotLoggedIn => "not_logged_in",
        ExcludedReason::AtCapacity => "at_capacity",
        ExcludedReason::Cooldown => "cooldown",
        ExcludedReason::FiveHourExhausted => "five_hour_exhausted",
        ExcludedReason::SevenDayExhausted => "seven_day_exhausted",
        ExcludedReason::Rejected => "rejected",
    }
}

/// ADR-0024 D4/D5: `AccountCooldownView.reason` の語彙。
fn account_cooldown_reason_name(reason: AccountCooldownReason) -> &'static str {
    match reason {
        AccountCooldownReason::AuthFailed => "auth_failed",
        AccountCooldownReason::Throttled => "throttled",
        AccountCooldownReason::Exhausted => "exhausted",
    }
}

/// ADR-0024 D4: 供給側失敗の種別名（`provider_failure_reason` と同じ語彙）を `AccountCooldownReason` に写す。
fn account_cooldown_reason_from_failure(reason: &str) -> AccountCooldownReason {
    match reason {
        "auth_failed" => AccountCooldownReason::AuthFailed,
        "throttled" => AccountCooldownReason::Throttled,
        // "exhausted" | "spawn"
        _ => AccountCooldownReason::Exhausted,
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

/// ADR-0016 M4: イベント列に集約遷移（`Transitioned{reason: "aggregate"}`）があるか。以後の run は集約 run。
fn has_aggregate_transition(events: &[(u64, Event)]) -> bool {
    events
        .iter()
        .any(|(_, e)| matches!(e, Event::Transitioned { reason, .. } if reason == "aggregate"))
}

/// ADR-0021 D1: イベント列に子の失敗による遷移（`Transitioned{reason: "child_failed"}`）があるか。
/// 以後の run は「子が失敗した後のやり直し」なので、子の結果（`context.children`）を渡す。
fn has_child_failed_transition(events: &[(u64, Event)]) -> bool {
    events
        .iter()
        .any(|(_, e)| matches!(e, Event::Transitioned { reason, .. } if reason == Trigger::ChildFailed.name()))
}

/// task-ops の読み取りエラーをディスパッチャのエラーに写す（検証以外の失敗は来ない想定）。
fn ops_to_store(e: task_ops::OpsError) -> DispatchError {
    match e {
        task_ops::OpsError::Store(inner) => DispatchError::Store(inner),
        other => DispatchError::Store(StoreError::Invalid(other.to_string())),
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
            role: None,
            genre: None,
            aggregate: false,
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
            std::collections::HashSet::new(),
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
                clusters: HashMap::new(),
                cluster_cooldown: Duration::from_secs(1),
                max_requeues: 5,
                roles: Vec::new(),
                genres: Vec::new(),
                delegation: DelegationLimits::default(),
                accounts: None,
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
        // WorkerStarted はワーカー run の 1 回と、ADR-0014 D1 で記録する Reviewer run の 1 回。
        assert_eq!(events.iter().filter(|(_, e)| matches!(e, Event::WorkerStarted { role: None, .. })).count(), 1);
        assert_eq!(
            events.iter().filter(|(_, e)| matches!(e, Event::WorkerStarted { role: Some(RunRole::Reviewer), .. })).count(),
            1
        );
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
        // ADR-0014 D1: 延期した Reviewer run も、成功した Reviewer run も WorkerFinished{role: reviewer} を残す。
        let reviewer_outcomes: Vec<&str> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerFinished { outcome, role: Some(RunRole::Reviewer), .. } => Some(outcome.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(reviewer_outcomes.len(), 2, "{events:?}");
        assert!(reviewer_outcomes[0].starts_with("requeue: "), "{reviewer_outcomes:?}");
        assert!(reviewer_outcomes[1].starts_with("done: "), "{reviewer_outcomes:?}");
    }

    /// ADR-0014 D1（P-G14）: Reviewer run も対象タスクに WorkerStarted / WorkerFinished（role: reviewer、provider つき）を残す。
    /// ReviewVerdict はワーカー run に付き、ワーカー run を前提にする `last_run_id` は Reviewer run を見ない。
    #[tokio::test]
    async fn reviewer_run_records_worker_started_and_finished_with_reviewer_role() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let r = new_task(dir.path(), Check::Reviewer, 0);
        store.insert(&r).unwrap();
        let adapter = Arc::new(FileAdapter {
            plan_json: String::new(),
            review_json: r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"ok"}]}"#.into(),
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 2);
        let report = run_until_idle(&mut d, 300).await;
        assert!(report.idle);
        assert_eq!(store.get(r.id).unwrap().unwrap().status, Status::Done);
        let events = store.events_for(r.id).unwrap();
        let started: Vec<(String, Option<RunRole>, Option<String>)> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerStarted { run_id, role, provider, .. } => Some((run_id.clone(), *role, provider.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), 2, "{events:?}");
        assert_eq!((started[0].1, started[1].1), (None, Some(RunRole::Reviewer)));
        assert_eq!(started[1].2.as_deref(), Some("p1"));
        let reviewer_finished: Vec<&str> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerFinished { run_id, outcome, role: Some(RunRole::Reviewer), .. } if *run_id == started[1].0 => {
                    Some(outcome.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(reviewer_finished.len(), 1, "{events:?}");
        assert!(reviewer_finished[0].starts_with("done: "), "{reviewer_finished:?}");
        assert_eq!(last_run_id(&events).as_deref(), Some(started[0].0.as_str()));
        assert!(events.iter().any(|(_, e)| matches!(e, Event::ReviewVerdict { run_id, pass: true, .. } if *run_id == started[0].0)));
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

    /// ADR-0018 D5（監査の「確認不能」の解消）: 並列度は「プロバイダ」と「クラスタ」の両方で守る。
    /// クラスタの上限（1）が全体の上限（3）とプロバイダの上限（3）より小さいとき、そのクラスタのタスクは
    /// 1 件ずつしか走らない。ローカル実行のタスクはクラスタの枠を消費しない。
    #[tokio::test]
    async fn cluster_and_provider_concurrency_are_both_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        // 同じクラスタを指すリモートのタスク 2 件と、ローカルのタスク 1 件。
        let mut remote_ids = Vec::new();
        for _ in 0..2 {
            let mut t = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
            t.workspace = WorkspaceSpec::Remote { cluster: "slow".into(), path: dir.path().to_path_buf() };
            store.insert(&t).unwrap();
            remote_ids.push(t.id);
        }
        let local = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        store.insert(&local).unwrap();

        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::from_millis(400),
        });
        let mut d = dispatcher(store.clone(), adapter, 3);
        // Remote のタスクの写しは `workspace_root/<task_id>`（ADR-0018 D1）。テストでは実体のある場所にする。
        d.config.workspace_root = dir.path().to_path_buf();
        d.config.clusters.insert(
            "slow".into(),
            ClusterSpec {
                id: "slow".into(),
                // 実際に ssh はせず、多重接続の確認だけが通ればよいので localhost 向けの Host 名を使う。
                host: "taskd-localhost".into(),
                concurrency: 1,
                sync: SyncMode::None,
                delete_on_push: false,
                setup: vec![],
                env: vec![],
                rsync_excludes: vec![],
                worktree: Default::default(),
                auth: "manual".into(),
            },
        );
        if !control_master_alive_blocking(&["ssh".to_string()], "taskd-localhost") {
            eprintln!("skip: taskd-localhost への多重接続が無い");
            return;
        }

        let report = d.tick().unwrap();
        // クラスタの上限が 1 なので、リモートは 1 件だけ。ローカルの 1 件は別枠で走る。
        assert_eq!(report.dispatched, 2, "{report:?}");
        let running_remote = remote_ids
            .iter()
            .filter(|id| store.get(**id).unwrap().unwrap().status == Status::Running)
            .count();
        assert_eq!(running_remote, 1, "クラスタの上限 1 を超えない");
        assert_eq!(store.get(local.id).unwrap().unwrap().status, Status::Running, "ローカルはクラスタの枠を使わない");

        let report = run_until_idle(&mut d, 400).await;
        assert!(report.idle);
        for id in &remote_ids {
            assert_eq!(
                store.get(*id).unwrap().unwrap().status,
                Status::Done,
                "{:?}",
                store.events_for(*id).unwrap()
            );
        }
    }

    /// ADR-0018 実装メモ M1〜M3: 多重接続の無いクラスタは 1 tick に 1 回の `ssh -O check` で分かり、スナップショットの `clusters[]` に
    /// `connected: false` と `cooldown_until` で現れる。タスクは ready のまま（attempts 不変）、`ClusterUnavailable` に host が入り、
    /// 人待ちなので idle を止めない。ssh 先が無いことを使うので外部ネットワークには出ない。
    #[tokio::test]
    async fn offline_cluster_is_reported_in_the_snapshot_and_the_event_carries_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        task.workspace = WorkspaceSpec::Remote { cluster: "offline".into(), path: PathBuf::from("/remote/project") };
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.clusters.insert(
            "offline".into(),
            ClusterSpec {
                id: "offline".into(),
                host: "taskd-no-such-host-for-tests".into(),
                concurrency: 1,
                sync: SyncMode::Rsync,
                delete_on_push: false,
                setup: vec![],
                env: vec![],
                rsync_excludes: vec![],
                worktree: Default::default(),
                auth: "manual".into(),
            },
        );
        let (tx, rx) = tokio::sync::watch::channel(None);
        d.set_snapshot_publisher(SnapshotPublisher {
            tx,
            instance_id: "inst-1".into(),
            hostname: "host-1".into(),
            started_at: "2026-09-15T00:00:00Z".into(),
            tick_ms: 50,
            providers: vec![],
            provider_checks: Default::default(),
        });

        let report = d.tick().unwrap();
        assert_eq!(report.dispatched, 0);
        assert!(report.idle, "a task waiting for a human login does not keep the daemon from going idle");

        let snap = rx.borrow().clone().expect("snapshot published");
        assert_eq!(snap.clusters.len(), 1, "{snap:?}");
        let live = &snap.clusters[0];
        assert_eq!(
            (live.id.as_str(), live.host.as_str(), live.concurrency, live.in_use, live.connected),
            ("offline", "taskd-no-such-host-for-tests", 1, 0, false)
        );
        let until = live.cooldown_until.clone().expect("cooldown_until");
        assert!(until > snap.last_tick_at, "cooldown ends after the tick: {until} vs {}", snap.last_tick_at);
        assert!(!snap.unroutable.contains(&task.id), "人待ちは経路なしではない（監査 4-1）: {snap:?}");
        assert!(d.cluster_waiting.contains(&task.id));

        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Ready, 0));
        let events = store.events_for(task.id).unwrap();
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                Event::ClusterUnavailable { cluster, host, .. } if cluster == "offline" && host == "taskd-no-such-host-for-tests"
            )),
            "{events:?}"
        );
        // 2 tick 目: cooldown 中は再度イベントを足さない（1 件のまま）。
        d.tick().unwrap();
        let again = store.events_for(task.id).unwrap();
        assert_eq!(
            again.iter().filter(|(_, e)| matches!(e, Event::ClusterUnavailable { .. })).count(),
            1,
            "{again:?}"
        );
    }

    fn cluster_spec_with_auth(id: &str, host: &str, auth: &str) -> ClusterSpec {
        ClusterSpec {
            id: id.into(),
            host: host.into(),
            concurrency: 1,
            sync: SyncMode::Rsync,
            delete_on_push: false,
            setup: vec![],
            env: vec![],
            rsync_excludes: vec![],
            worktree: Default::default(),
            auth: auth.into(),
        }
    }

    /// ADR-0032 D3: `auth = "publickey"` かつ接続フックが刺さっていれば、未接続のクラスタは cooldown にする前に
    /// 1 回だけ自動接続を試みる。成功したら `cluster_connected` が true になり、そのまま dispatch が続く
    /// （`ClusterUnavailable` は残らない）。
    #[tokio::test]
    async fn publickey_cluster_auto_connects_and_dispatch_continues_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        task.workspace = WorkspaceSpec::Remote { cluster: "auto".into(), path: PathBuf::from("/remote/project") };
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.clusters.insert(
            "auto".into(),
            cluster_spec_with_auth("auto", "taskd-no-such-host-for-tests-auto-ok", "publickey"),
        );
        let calls: Arc<StdMutex<Vec<(String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
        let calls_for_hook = calls.clone();
        d.set_cluster_connector(Arc::new(move |id: &str, host: &str| {
            calls_for_hook.lock().unwrap().push((id.to_string(), host.to_string()));
            Ok(())
        }));

        let report = d.tick().unwrap();
        assert_eq!(report.dispatched, 1, "{report:?}");
        assert_eq!(
            *calls.lock().unwrap(),
            vec![("auto".to_string(), "taskd-no-such-host-for-tests-auto-ok".to_string())]
        );
        assert_eq!(d.cluster_connected.get("auto"), Some(&true));
        assert!(!d.cluster_cooldown.contains_key("auto"), "success does not cool the cluster down");
        let events = store.events_for(task.id).unwrap();
        assert!(
            !events.iter().any(|(_, e)| matches!(e, Event::ClusterUnavailable { .. })),
            "no ClusterUnavailable when the auto-connect succeeded: {events:?}"
        );
    }

    /// ADR-0032 D3: 自動接続が失敗したら、従来どおり cooldown + `Event::ClusterUnavailable` に落ちるが、
    /// `reason` は「自動接続を試みて失敗した」と分かる文字列になる（人が受信箱で区別できるように）。
    /// 接続の試行はクラスタごとに 1 回だけ（cooldown 中の 2 tick 目では呼ばれない）。
    #[tokio::test]
    async fn publickey_cluster_auto_connect_failure_gets_a_distinguishable_reason() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        task.workspace = WorkspaceSpec::Remote { cluster: "auto".into(), path: PathBuf::from("/remote/project") };
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.clusters.insert(
            "auto".into(),
            cluster_spec_with_auth("auto", "taskd-no-such-host-for-tests-auto-fail", "publickey"),
        );
        let call_count = Arc::new(StdMutex::new(0u32));
        let call_count_for_hook = call_count.clone();
        d.set_cluster_connector(Arc::new(move |_id: &str, _host: &str| {
            *call_count_for_hook.lock().unwrap() += 1;
            Err("permission denied (publickey)".to_string())
        }));

        let report = d.tick().unwrap();
        assert_eq!(report.dispatched, 0, "{report:?}");
        assert_eq!(*call_count.lock().unwrap(), 1);
        let events = store.events_for(task.id).unwrap();
        let reason = events
            .iter()
            .find_map(|(_, e)| match e {
                Event::ClusterUnavailable { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .expect("ClusterUnavailable event");
        assert!(reason.contains("auto-connect failed"), "{reason}");
        assert!(reason.contains("permission denied (publickey)"), "{reason}");

        // 2 tick 目: cooldown 中なので自動接続は再試行しない（tick ごとに ssh が湧かない）。
        d.tick().unwrap();
        assert_eq!(*call_count.lock().unwrap(), 1, "cooldown 中は 1 回だけ");
    }

    /// ADR-0032 D3: `auth = "manual"` / `"totp"` は自動接続の対象外。接続フックが刺さっていても呼ばれず、
    /// `reason` は従来どおりの文言のまま（自動接続を試みたとは分からない）。
    #[tokio::test]
    async fn manual_and_totp_clusters_are_not_auto_connected_even_with_a_hook() {
        for auth in ["manual", "totp"] {
            let dir = tempfile::tempdir().unwrap();
            let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
            let mut task = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
            task.workspace = WorkspaceSpec::Remote { cluster: "auto".into(), path: PathBuf::from("/remote/project") };
            store.insert(&task).unwrap();
            let adapter = Arc::new(InstantAdapter {
                terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
                delay: Duration::ZERO,
            });
            let mut d = dispatcher(store.clone(), adapter, 1);
            d.config.clusters.insert(
                "auto".into(),
                cluster_spec_with_auth("auto", "taskd-no-such-host-for-tests-not-auto", auth),
            );
            let call_count = Arc::new(StdMutex::new(0u32));
            let call_count_for_hook = call_count.clone();
            d.set_cluster_connector(Arc::new(move |_id: &str, _host: &str| {
                *call_count_for_hook.lock().unwrap() += 1;
                Ok(())
            }));

            let report = d.tick().unwrap();
            assert_eq!(report.dispatched, 0, "{auth}: {report:?}");
            assert_eq!(*call_count.lock().unwrap(), 0, "{auth}: hook must not run for auth={auth:?}");
            let events = store.events_for(task.id).unwrap();
            let reason = events
                .iter()
                .find_map(|(_, e)| match e {
                    Event::ClusterUnavailable { reason, .. } => Some(reason.clone()),
                    _ => None,
                })
                .expect("ClusterUnavailable event");
            assert!(!reason.contains("auto-connect"), "{auth}: {reason}");
        }
    }

    /// ADR-0032 D1/D4: `ClusterSpec.auth` がスナップショットの `ClusterLive.auth` に写り、
    /// `set_cluster_connect_pending` が `ClusterLive.connect_pending` を立てる/降ろす。
    #[tokio::test]
    async fn cluster_live_carries_auth_and_connect_pending() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "fern03".into(),
            cluster_spec_with_auth("fern03", "taskd-no-such-host-for-tests-live", "publickey"),
        );
        let (tx, rx) = tokio::sync::watch::channel(None);
        d.set_snapshot_publisher(SnapshotPublisher {
            tx,
            instance_id: "inst-1".into(),
            hostname: "host-1".into(),
            started_at: "2026-09-17T00:00:00Z".into(),
            tick_ms: 50,
            providers: vec![],
            provider_checks: Default::default(),
        });

        d.tick().unwrap();
        let snap = rx.borrow().clone().expect("snapshot published");
        let live = snap.clusters.iter().find(|c| c.id == "fern03").expect("fern03 in snapshot");
        assert_eq!((live.auth.as_str(), live.connect_pending), ("publickey", false));

        d.set_cluster_connect_pending("fern03", true);
        d.tick().unwrap();
        let snap = rx.borrow().clone().expect("snapshot published");
        let live = snap.clusters.iter().find(|c| c.id == "fern03").expect("fern03 in snapshot");
        assert!(live.connect_pending, "connect_pending set");

        d.set_cluster_connect_pending("fern03", false);
        d.tick().unwrap();
        let snap = rx.borrow().clone().expect("snapshot published");
        let live = snap.clusters.iter().find(|c| c.id == "fern03").expect("fern03 in snapshot");
        assert!(!live.connect_pending, "connect_pending cleared");
    }

    /// ADR-0018 実装メモ M1: 接続が戻っていれば、その tick で cooldown が解ける。`taskd-localhost` への多重接続が無い環境では skip。
    #[tokio::test]
    async fn cluster_cooldown_is_cleared_once_the_control_master_is_back() {
        if !control_master_alive_blocking(&["ssh".to_string()], "taskd-localhost") {
            eprintln!("skip: taskd-localhost への多重接続が無い");
            return;
        }
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "local".into(),
            ClusterSpec {
                id: "local".into(),
                host: "taskd-localhost".into(),
                concurrency: 1,
                sync: SyncMode::Rsync,
                delete_on_push: false,
                setup: vec![],
                env: vec![],
                rsync_excludes: vec![],
                worktree: Default::default(),
                auth: "manual".into(),
            },
        );
        d.cluster_cooldown.insert("local".into(), Instant::now() + Duration::from_secs(3600));
        d.refresh_cluster_liveness();
        assert_eq!(d.cluster_connected.get("local"), Some(&true));
        assert!(!d.cluster_cooldown.contains_key("local"), "cooldown is cleared when the connection is back");

        // ADR-0023 D1: 5 秒以内の 2 回目は `ssh -O check` を回さず、前回の結果をそのまま使う。
        d.cluster_connected.insert("local".into(), false);
        d.refresh_cluster_liveness();
        assert_eq!(d.cluster_connected.get("local"), Some(&false), "間引いた回は確認し直さない");
        // 前回の確認を古くすると、次の呼び出しで確認し直す。
        d.last_cluster_liveness = Some(Instant::now() - CLUSTER_LIVENESS_INTERVAL - Duration::from_millis(1));
        d.refresh_cluster_liveness();
        assert_eq!(d.cluster_connected.get("local"), Some(&true), "間隔を過ぎたら確認し直す");
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
                env_keys: vec![],
                in_use: 0,
                last_check: None,
                account_pool: false,
            }],
            provider_checks: Default::default(),
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

        // ADR-0022 D2: 疎通確認の結果はスナップショットにだけ載る（DB には書かない）。
        assert!(snap.providers[0].last_check.is_none(), "確認する前は空");
        d.set_provider_check("p1", ProviderCheckView { at: "2026-09-16T02:00:00Z".into(), result: "ok".into(), detail: None });
        d.tick().unwrap();
        let snap = rx.borrow().clone().unwrap();
        assert_eq!(
            snap.providers[0].last_check,
            Some(ProviderCheckView { at: "2026-09-16T02:00:00Z".into(), result: "ok".into(), detail: None })
        );

        // reload でプロバイダ表を差し替えても、残った id の記録は保つ。消えた id の記録は落とす。
        d.set_snapshot_providers(vec![
            ProviderLive {
                id: "p1".into(),
                adapter: "instant".into(),
                tiers: vec![Tier::Standard],
                concurrency: 2,
                model: Some("m2".into()),
                env_keys: vec![],
                in_use: 0,
                last_check: None,
                account_pool: false,
            },
            ProviderLive {
                id: "p2".into(),
                adapter: "instant".into(),
                tiers: vec![Tier::Standard],
                concurrency: 1,
                model: None,
                env_keys: vec![],
                in_use: 0,
                last_check: None,
                account_pool: false,
            },
        ]);
        d.tick().unwrap();
        let snap = rx.borrow().clone().unwrap();
        assert_eq!(snap.providers[0].last_check.as_ref().map(|c| c.result.as_str()), Some("ok"), "p1 の記録は残る");
        assert!(snap.providers[1].last_check.is_none(), "p2 はまだ確認していない");

        d.set_snapshot_providers(vec![ProviderLive {
            id: "p2".into(),
            adapter: "instant".into(),
            tiers: vec![Tier::Standard],
            concurrency: 1,
            model: None,
            env_keys: vec![],
            in_use: 0,
            last_check: None,
            account_pool: false,
        }]);
        d.tick().unwrap();
        let snap = rx.borrow().clone().unwrap();
        assert_eq!(snap.providers.len(), 1);
        assert!(snap.providers[0].last_check.is_none(), "消えた p1 の記録は残さない");
    }

    /// `task_id` の直接の `Approval` 子タスクが現れるまで tick を回す（Human check の生成を待つ）。
    /// ADR-0016: 親 run で `delegate` を出し、子 run は少し待って done、集約 run は `artifacts/summary.md` を書くアダプタ。
    struct DelegatingAdapter {
        proposals: Vec<DelegateTask>,
        child_delay: Duration,
        seen_role: std::sync::Mutex<Option<RoleContext>>,
        aggregate_children: AtomicUsize,
        write_summary: bool,
    }

    #[async_trait]
    impl WorkerAdapter for DelegatingAdapter {
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
            let done = |summary: &str| {
                Ok(RunOutcome {
                    terminal: Terminal::Done { summary: summary.into(), evidence: vec![], usage: None },
                    exit_code: Some(0),
                })
            };
            // 提案した子は role = implementer。親（lead / 役割なし）と区別する。
            if req.task.role.as_deref() == Some("implementer") {
                tokio::time::sleep(self.child_delay).await;
                return done("child");
            }
            *self.seen_role.lock().unwrap() = req.context.role.clone();
            if !req.context.children.is_empty() {
                self.aggregate_children.store(req.context.children.len(), Ordering::SeqCst);
                if self.write_summary {
                    std::fs::create_dir_all(req.workspace.join("artifacts")).unwrap();
                    std::fs::write(req.workspace.join("artifacts/summary.md"), "# summary\n").unwrap();
                }
                return done("aggregated");
            }
            sink.delegate(&self.proposals);
            done("delegated")
        }
    }

    fn proposal(title: &str, deps: Vec<task_core::DelegateDep>) -> DelegateTask {
        DelegateTask {
            title: title.into(),
            objective: format!("do {title}"),
            acceptance: vec![Criterion { text: "c".into(), check: Check::Command { cmd: "true".into(), expect_exit: 0 } }],
            role: Some("implementer".into()),
            genre: None,
            depends_on: deps,
            tier: None,
        }
    }

    fn roles() -> Vec<RoleSpec> {
        vec![
            RoleSpec {
                id: "lead".into(),
                instructions: Some("You lead; delegate implementation.".into()),
                ..RoleSpec::default()
            },
            RoleSpec { id: "implementer".into(), tier: Some(Tier::Cheap), ..RoleSpec::default() },
        ]
    }

    fn progress_msgs(store: &Arc<dyn TaskStore>, id: TaskId) -> Vec<String> {
        store
            .events_for(id)
            .unwrap()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerProgress { msg, .. } => Some(msg),
                _ => None,
            })
            .collect()
    }

    /// 受け入れ 1〜3・5: 役割の指示文が run に載り、`delegate` の検証を通った 2 件だけが子になり、親は子が終わるまで
    /// reviewing のまま、`aggregate = true` なら最後に 1 回だけ集約 run が走って summary.md が暗黙の条件で判定される。
    #[tokio::test]
    async fn delegate_inserts_validated_children_and_aggregate_parent_runs_once_more() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut parent = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 1);
        parent.role = Some("lead".into());
        parent.aggregate = true;
        store.insert(&parent).unwrap();
        let mut bad_title = proposal("", vec![]);
        bad_title.title = "  ".into();
        let adapter = Arc::new(DelegatingAdapter {
            proposals: vec![
                proposal("a", vec![]),
                proposal("b", vec![task_core::DelegateDep::Index(0)]),
                bad_title,
                proposal("self", vec![task_core::DelegateDep::Id(parent.id.to_string())]),
            ],
            child_delay: Duration::from_millis(30),
            seen_role: std::sync::Mutex::new(None),
            aggregate_children: AtomicUsize::new(0),
            write_summary: true,
        });
        let mut d = dispatcher(store.clone(), adapter.clone(), 4);
        d.config.roles = roles();
        let report = run_until_idle(&mut d, 400).await;
        assert!(report.idle);

        let p = store.get(parent.id).unwrap().unwrap();
        assert_eq!(p.status, Status::Done, "{:?}", store.events_for(parent.id).unwrap());
        assert_eq!(p.attempts, 0, "aggregate does not consume attempts");
        let children = store.children(parent.id).unwrap();
        assert_eq!(children.len(), 2, "only the two valid proposals were inserted");
        assert_eq!(children[0].title, "a");
        assert_eq!(children[1].title, "b");
        assert_eq!(children[1].depends_on, vec![children[0].id]);
        assert_eq!(children[0].role.as_deref(), Some("implementer"));
        assert_eq!(children[0].worker_hint.tier, Tier::Cheap, "role default applied to the child");
        for c in &children {
            assert_eq!(store.get(c.id).unwrap().unwrap().status, Status::Done);
        }

        let events = store.events_for(parent.id).unwrap();
        let delegated: Vec<Vec<TaskId>> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::Delegated { task_ids, .. } => Some(task_ids.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(delegated, vec![vec![children[0].id, children[1].id]]);
        let msgs = progress_msgs(&store, parent.id);
        assert!(msgs.iter().any(|m| m.starts_with("delegate rejected: tasks[2]") && m.contains("title")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.starts_with("delegate rejected: tasks[3]") && m.contains("delegating task itself")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.starts_with("waiting for ") && m.contains("delegated child task")), "{msgs:?}");
        assert_eq!(
            transition_reasons(&store, parent.id),
            vec!["dispatch", "worker_done", "aggregate", "dispatch", "worker_done", "review_pass"]
        );
        // 親の run は 2 回（最初 + 集約）。役割名が WorkerStarted に残り、指示文が RunContext に載る。
        let started: Vec<Option<String>> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerStarted { role: None, task_role, .. } => Some(task_role.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(started, vec![Some("lead".to_string()), Some("lead".to_string())]);
        let role = adapter.seen_role.lock().unwrap().clone().expect("role context");
        assert_eq!(role.id, "lead");
        assert_eq!(role.instructions, "You lead; delegate implementation.");
        assert_eq!(adapter.aggregate_children.load(Ordering::SeqCst), 2, "aggregate run saw both children");
        // 集約 run のレビューには暗黙の summary.md 条件（idx = acceptance.len()）が入る。
        assert!(events.iter().any(|(_, e)| matches!(e, Event::ReviewVerdict { criterion_idx: 1, pass: true, reason, .. } if reason.contains("summary.md"))), "{events:?}");
    }

    /// ADR-0027 D1 / 受け入れ 3: 委譲できる run（lead, genre=coding）のプロンプト文脈に設定済みの
    /// 全分野（`available_genres`）が渡り、`DelegateTask.genre` で子を別分野（literature）に委譲できる。
    struct GenreDelegatingAdapter {
        proposal: DelegateTask,
        seen_available_genres: std::sync::Mutex<Option<Vec<task_worker::GenreContext>>>,
    }

    #[async_trait]
    impl WorkerAdapter for GenreDelegatingAdapter {
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
            let done = |summary: &str| {
                Ok(RunOutcome {
                    terminal: Terminal::Done { summary: summary.into(), evidence: vec![], usage: None },
                    exit_code: Some(0),
                })
            };
            if req.task.role.as_deref() == Some("literature-reader") {
                return done("child");
            }
            *self.seen_available_genres.lock().unwrap() = Some(req.context.available_genres.clone());
            sink.delegate(std::slice::from_ref(&self.proposal));
            done("delegated")
        }
    }

    #[tokio::test]
    async fn delegate_can_select_a_different_genre_and_available_genres_reach_the_prompt_context() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut parent = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        parent.role = Some("lead".into());
        parent.genre = Some("coding".into());
        store.insert(&parent).unwrap();

        let mut child_proposal = proposal("investigate prior art", vec![]);
        child_proposal.role = Some("literature-reader".into());
        child_proposal.genre = Some("literature".into());
        let adapter = Arc::new(GenreDelegatingAdapter {
            proposal: child_proposal,
            seen_available_genres: std::sync::Mutex::new(None),
        });
        let mut d = dispatcher(store.clone(), adapter.clone(), 4);
        d.config.roles = vec![
            RoleSpec { id: "lead".into(), ..RoleSpec::default() },
            RoleSpec { id: "literature-reader".into(), ..RoleSpec::default() },
        ];
        d.config.genres = vec![
            task_core::GenreSpec {
                id: "coding".into(),
                description: "write and fix code".into(),
                default_role: Some("lead".into()),
                roles: vec!["lead".into()],
                ..task_core::GenreSpec::default()
            },
            task_core::GenreSpec {
                id: "literature".into(),
                description: "related work survey".into(),
                default_role: Some("literature-reader".into()),
                roles: vec!["literature-reader".into()],
                ..task_core::GenreSpec::default()
            },
        ];
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);

        let p = store.get(parent.id).unwrap().unwrap();
        assert_eq!(p.status, Status::Done, "{:?}", store.events_for(parent.id).unwrap());
        let children = store.children(parent.id).unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].role.as_deref(), Some("literature-reader"));
        assert_eq!(children[0].genre.as_deref(), Some("literature"), "explicit genre wins");

        let available = adapter.seen_available_genres.lock().unwrap().clone().expect("available_genres seen");
        let ids: Vec<&str> = available.iter().map(|g| g.id.as_str()).collect();
        assert!(ids.contains(&"coding"), "{ids:?}");
        assert!(ids.contains(&"literature"), "{ids:?}");
    }

    /// ADR-0028 D3: `run_extras` は Plan run にも `available_genres` を渡す（今までは Execute/Approval だけ）。
    /// これで `build_plan_prompt` にも「使える専門家」節が出る（`claude_code` 側のテストで確認済み）。
    #[test]
    fn run_extras_fills_available_genres_for_plan_runs() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let plan = plan_task(dir.path(), 0);
        store.insert(&plan).unwrap();
        let adapter: Arc<dyn WorkerAdapter> = Arc::new(FileAdapter {
            plan_json: VALID_PLAN.into(),
            review_json: r#"{"verdicts":[]}"#.into(),
            delay: Duration::from_millis(0),
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.genres = vec![task_core::GenreSpec {
            id: "coding".into(),
            description: "write and fix code".into(),
            ..task_core::GenreSpec::default()
        }];
        let extras = d.run_extras(&plan).unwrap();
        let ids: Vec<&str> = extras.available_genres.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, vec!["coding"]);

        // 分野が無い設定では空のまま。
        d.config.genres = Vec::new();
        let extras = d.run_extras(&plan).unwrap();
        assert!(extras.available_genres.is_empty());
    }

    /// 受け入れ 3: `aggregate = false` の親は子が終わるまで reviewing のまま、終わったら run を増やさず done。
    #[tokio::test]
    async fn non_aggregate_parent_stays_reviewing_until_children_finish_then_completes() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let parent = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        store.insert(&parent).unwrap();
        let adapter = Arc::new(DelegatingAdapter {
            proposals: vec![proposal("slow", vec![])],
            child_delay: Duration::from_millis(400),
            seen_role: std::sync::Mutex::new(None),
            aggregate_children: AtomicUsize::new(0),
            write_summary: false,
        });
        let mut d = dispatcher(store.clone(), adapter, 4);
        // ADR-0023 D3: 子待ちの親はスナップショットの `awaiting_children` にも出る。
        let (tx, rx) = tokio::sync::watch::channel(None);
        d.set_snapshot_publisher(SnapshotPublisher {
            tx,
            instance_id: "inst-1".into(),
            hostname: "host-1".into(),
            started_at: "2026-09-16T00:00:00Z".into(),
            tick_ms: 10,
            providers: vec![],
            provider_checks: Default::default(),
        });
        // 親の run と判定が終わり、子がまだ走っている間に観察する。
        let mut observed_waiting = false;
        for _ in 0..200 {
            let r = d.tick().unwrap();
            let p = store.get(parent.id).unwrap().unwrap();
            let child_running = store.children(parent.id).unwrap().iter().any(|c| c.status == Status::Running);
            if p.status == Status::Reviewing && child_running && d.awaiting_children.contains_key(&parent.id) {
                observed_waiting = true;
                break;
            }
            if r.idle {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(observed_waiting, "parent should be reviewing while its delegated child runs");
        assert_eq!(
            rx.borrow().as_ref().map(|s| s.awaiting_children.clone()),
            Some(vec![parent.id]),
            "ADR-0023 D3: 子待ちの親がスナップショットに出る（GUI が「判定中」と区別できる）"
        );
        let report = run_until_idle(&mut d, 400).await;
        assert!(report.idle);
        let p = store.get(parent.id).unwrap().unwrap();
        assert_eq!(p.status, Status::Done);
        assert_eq!(
            rx.borrow().as_ref().map(|s| s.awaiting_children.clone()),
            Some(vec![]),
            "子が終われば待ちも消える"
        );
        assert_eq!(transition_reasons(&store, parent.id), vec!["dispatch", "worker_done", "review_pass"]);
        let events = store.events_for(parent.id).unwrap();
        assert_eq!(events.iter().filter(|(_, e)| matches!(e, Event::WorkerStarted { role: None, .. })).count(), 1);
        assert!(events.iter().any(|(_, e)| matches!(e, Event::Delegated { .. })));
    }

    /// 受け入れ 2: 上限（1 run の件数・木の深さ・木の run 数）を超える提案は拒否され、理由が WorkerProgress に残り、親は失敗しない。
    #[tokio::test]
    async fn delegation_limits_reject_with_reasons_and_do_not_fail_the_run() {
        async fn run_with(limits: DelegationLimits, proposals: Vec<DelegateTask>, depth: u32) -> (Vec<String>, usize, Status) {
            let dir = tempfile::tempdir().unwrap();
            let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
            // depth 個の祖先の下に親を置く（根 = 深さ 1）。
            let mut ancestor: Option<TaskId> = None;
            for _ in 1..depth {
                let mut a = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
                a.parent_id = ancestor;
                a.status = Status::Done;
                store.insert(&a).unwrap();
                ancestor = Some(a.id);
            }
            let mut parent = new_task(dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
            parent.parent_id = ancestor;
            store.insert(&parent).unwrap();
            let adapter = Arc::new(DelegatingAdapter {
                proposals,
                child_delay: Duration::from_millis(1),
                seen_role: std::sync::Mutex::new(None),
                aggregate_children: AtomicUsize::new(0),
                write_summary: false,
            });
            let mut d = dispatcher(store.clone(), adapter, 4);
            d.config.delegation = limits;
            let report = run_until_idle(&mut d, 400).await;
            assert!(report.idle);
            let p = store.get(parent.id).unwrap().unwrap();
            (progress_msgs(&store, parent.id), store.children(parent.id).unwrap().len(), p.status)
        }

        // 1 run の件数: 2 件のうち 1 件だけ。
        let (msgs, n, status) = run_with(
            DelegationLimits { max_delegate_per_run: 1, ..DelegationLimits::default() },
            vec![proposal("a", vec![]), proposal("b", vec![])],
            1,
        )
        .await;
        assert_eq!(n, 1, "{msgs:?}");
        assert_eq!(status, Status::Done);
        assert!(msgs.iter().any(|m| m.contains("delegate rejected: tasks[1]") && m.contains("per-run delegation limit (1)")), "{msgs:?}");

        // 木の深さ: 深さ 2 の親は max_tree_depth = 2 で子を作れない。
        let (msgs, n, status) = run_with(
            DelegationLimits { max_tree_depth: 2, ..DelegationLimits::default() },
            vec![proposal("a", vec![])],
            2,
        )
        .await;
        assert_eq!(n, 0, "{msgs:?}");
        assert_eq!(status, Status::Done);
        assert!(msgs.iter().any(|m| m.contains("delegate rejected") && m.contains("tree depth would become 3 (max 2)")), "{msgs:?}");

        // 木の run 数: 親自身の run が 1 回目なので max_tree_runs = 1 で拒否。
        let (msgs, n, status) = run_with(
            DelegationLimits { max_tree_runs: 1, ..DelegationLimits::default() },
            vec![proposal("a", vec![])],
            1,
        )
        .await;
        assert_eq!(n, 0, "{msgs:?}");
        assert_eq!(status, Status::Done);
        assert!(msgs.iter().any(|m| m.contains("delegate rejected") && m.contains("worker runs (max 1)")), "{msgs:?}");
    }

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

    // ---- ADR-0024: account pool ----

    /// 走った run の env を記録し、`with_env` を実装するテスト用アダプタ（ADR-0024 D2）。
    type CapturedEnvs = Arc<StdMutex<Vec<Vec<(String, String)>>>>;

    #[derive(Clone)]
    struct PoolAdapter {
        terminal_or_throttled: Result<Terminal, Duration>,
        delay: Duration,
        observation: Option<RateLimitObservation>,
        env: Vec<(String, String)>,
        captured: CapturedEnvs,
        /// S10: `true` なら `AdapterError::Spawn` を返す（`terminal_or_throttled` より優先）。
        spawn_failure: bool,
    }

    #[async_trait]
    impl WorkerAdapter for PoolAdapter {
        fn id(&self) -> &str {
            "claude-code"
        }
        async fn run(&self, req: RunRequest, _run_id: &str, _limits: RunLimits, sink: &dyn EventSink) -> Result<RunOutcome, AdapterError> {
            self.captured.lock().unwrap().push(self.env.clone());
            if let Some(obs) = self.observation.clone() {
                sink.rate_limit(obs);
            }
            std::fs::write(req.workspace.join("touched"), "1").unwrap();
            tokio::time::sleep(self.delay).await;
            if self.spawn_failure {
                return Err(AdapterError::Spawn(std::io::Error::other("boom")));
            }
            match &self.terminal_or_throttled {
                Ok(terminal) => Ok(RunOutcome { terminal: terminal.clone(), exit_code: Some(0) }),
                Err(retry_after) => Err(AdapterError::Throttled { retry_after: *retry_after }),
            }
        }
        fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
            let mut env = self.env.clone();
            env.extend(extra.iter().cloned());
            Some(Arc::new(PoolAdapter { env, ..self.clone() }))
        }
    }

    /// 2 アカウント（`a`, `b`。両方ログイン済み）を持つ一時ディレクトリを作る。
    fn accounts_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for id in ["a", "b"] {
            let acct = dir.path().join(id);
            std::fs::create_dir_all(&acct).unwrap();
            std::fs::write(acct.join(".credentials.json"), "{}").unwrap();
        }
        dir
    }

    #[allow(clippy::too_many_arguments)]
    fn pool_dispatcher(
        store: Arc<dyn TaskStore>,
        adapter: Arc<dyn WorkerAdapter>,
        second_provider: Option<(&str, Arc<dyn WorkerAdapter>)>,
        accounts_root: PathBuf,
        max_runs_per_account: usize,
        max_concurrency: usize,
    ) -> Dispatcher {
        pool_dispatcher_with_requeues(store, adapter, second_provider, accounts_root, max_runs_per_account, max_concurrency, 5)
    }

    #[allow(clippy::too_many_arguments)]
    fn pool_dispatcher_with_requeues(
        store: Arc<dyn TaskStore>,
        adapter: Arc<dyn WorkerAdapter>,
        second_provider: Option<(&str, Arc<dyn WorkerAdapter>)>,
        accounts_root: PathBuf,
        max_runs_per_account: usize,
        max_concurrency: usize,
        max_requeues: u32,
    ) -> Dispatcher {
        let mut providers = vec![ProviderSpec {
            id: "p1".into(),
            adapter: "claude-code".into(),
            tiers: vec![Tier::Frontier, Tier::Standard, Tier::Cheap],
            concurrency: max_concurrency,
            model: "m".into(),
        }];
        let mut adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>> = HashMap::new();
        adapters.insert("p1".into(), adapter);
        if let Some((id, a)) = second_provider {
            providers.push(ProviderSpec {
                id: id.into(),
                adapter: "instant".into(),
                tiers: vec![Tier::Frontier, Tier::Standard, Tier::Cheap],
                concurrency: max_concurrency,
                model: "m".into(),
            });
            adapters.insert(id.into(), a);
        }
        let policy = StaticPolicy::new(providers, Duration::from_secs(1));
        let account_pool_providers: std::collections::HashSet<ProviderId> = ["p1".to_string()].into();
        Dispatcher::new(
            store,
            Box::new(policy),
            HashMap::from([("p1".to_string(), "m".to_string())]),
            adapters,
            account_pool_providers,
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
                clusters: HashMap::new(),
                cluster_cooldown: Duration::from_secs(1),
                max_requeues,
                roles: Vec::new(),
                genres: Vec::new(),
                delegation: DelegationLimits::default(),
                accounts: Some(AccountsRuntimeConfig {
                    roots: HashMap::from([(AccountAdapter::ClaudeCode, accounts_root)]),
                    max_runs_per_account,
                    check_model: "haiku".into(),
                    fallback_cooldown_secs: 300,
                }),
            },
        )
    }

    fn usage_window(utilization: f64, resets_at_secs_from_now: i64) -> RateLimitObservation {
        RateLimitObservation {
            five_hour: Some(task_core::RateWindow { utilization, resets_at: 10_000 + resets_at_secs_from_now }),
            seven_day: None,
            status: None,
            resets_at: None,
            observed_at: 10_000,
        }
    }

    /// (a) 観測値の異なる 2 アカウントがあれば、スコアの高い方（残量が多い方）に run が割り当てられ、
    /// `WorkerStarted.account` とアダプタが実際に受け取った env が一致する（ADR-0024 D2/D3、受け入れ条件 1）。
    #[tokio::test]
    async fn pool_run_goes_to_the_account_with_more_headroom_and_sets_the_env() {
        let dir = accounts_fixture();
        let book_path = dir.path().join(".taskd-usage.json");
        {
            let mut book = AccountBook::load(&book_path);
            book.record_observation("a", usage_window(0.8, 90_000), ObservationSource::Run);
            book.record_observation("b", usage_window(0.1, 90_000), ObservationSource::Run);
            book.save().unwrap();
        }

        let ws_dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(ws_dir.path(), Check::Command { cmd: "test -f touched".into(), expect_exit: 0 }, 0);
        store.insert(&task).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None }),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured: captured.clone(),
            spawn_failure: false,
        });
        let mut d = pool_dispatcher(store.clone(), adapter, None, dir.path().to_path_buf(), 2, 2);
        // `usage_window` は `observed_at = 10_000` 基準なので、評価もその時刻で行う（実時計だと `resets_at` が
        // とっくに過ぎていて両方とも実効使用率 0 になってしまうため）。
        d.set_now_unix_fn(Arc::new(|| 10_000));
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);

        let events = store.events_for(task.id).unwrap();
        let account = events.iter().find_map(|(_, e)| match e {
            Event::WorkerStarted { account, .. } => account.clone(),
            _ => None,
        });
        assert_eq!(account.as_deref(), Some("b"));

        let envs = captured.lock().unwrap();
        assert_eq!(envs.len(), 1);
        let dir_value = envs[0]
            .iter()
            .find(|(k, _)| k == "CLAUDE_SECURESTORAGE_CONFIG_DIR")
            .map(|(_, v)| v.clone());
        assert_eq!(dir_value.as_deref(), Some(dir.path().join("b").to_string_lossy().as_ref()));
    }

    /// (b) 片方が throttled で終わると、そのアカウントだけが cooldown になり、次の run はもう片方に行く。
    /// プロバイダ自体は cooldown にならない（受け入れ条件 2）。
    #[tokio::test]
    async fn throttled_account_cools_down_without_cooling_the_provider() {
        let dir = accounts_fixture();
        let ws_dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        // `max_requeues = 0` にして、1 回失敗したらすぐ通常の失敗（`max_retries = 0` で即 `failed`）にする。
        // そうしないと供給側失敗は requeue され続け、"a" だけでなく "b" も使い切って cooldown にしてしまう。
        let task1 = new_task(ws_dir.path(), Check::Command { cmd: "test -f touched".into(), expect_exit: 0 }, 0);
        store.insert(&task1).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        // "a" は id の昇順タイブレークで最初に選ばれ、throttled で失敗する。
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Err(Duration::from_secs(120)),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured: captured.clone(),
            spawn_failure: false,
        });
        let mut d = pool_dispatcher_with_requeues(store.clone(), adapter, None, dir.path().to_path_buf(), 1, 1, 0);
        // 1 tick で dispatch → 完了まで待つ。
        for _ in 0..50 {
            d.tick().unwrap();
            if !store.events_for(task1.id).unwrap().is_empty()
                && store
                    .events_for(task1.id)
                    .unwrap()
                    .iter()
                    .any(|(_, e)| matches!(e, Event::WorkerFinished { .. }))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let events = store.events_for(task1.id).unwrap();
        let first_account = events.iter().find_map(|(_, e)| match e {
            Event::WorkerStarted { account, .. } => account.clone(),
            _ => None,
        });
        assert_eq!(first_account.as_deref(), Some("a"));
        // プロバイダ自体は cooldown にならない（ADR-0024 D4）。
        assert!(d.policy.cooldowns(Instant::now()).is_empty());
        // `ProviderThrottled` イベントは記録されない（アカウントの cooldown として扱われるため）。
        assert!(!events.iter().any(|(_, e)| matches!(e, Event::ProviderThrottled { .. })));

        // 次に投入したタスクは、cooldown 中の "a" を避けて "b" に行く。
        let task2 = new_task(ws_dir.path(), Check::Command { cmd: "true".into(), expect_exit: 0 }, 0);
        store.insert(&task2).unwrap();
        for _ in 0..50 {
            d.tick().unwrap();
            let events2 = store.events_for(task2.id).unwrap();
            if events2.iter().any(|(_, e)| matches!(e, Event::WorkerStarted { .. })) {
                let acct = events2.iter().find_map(|(_, e)| match e {
                    Event::WorkerStarted { account, .. } => account.clone(),
                    _ => None,
                });
                assert_eq!(acct.as_deref(), Some("b"));
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("task2 was never dispatched to account b");
    }

    /// S10: `Spawn` 失敗（起動できない）はアカウントの責任ではないので、プールの run でもプロバイダを
    /// cooldown にする（アカウントは cooldown にしない）。
    #[tokio::test]
    async fn spawn_failure_on_pool_run_cools_the_provider_not_the_account() {
        let dir = accounts_fixture();
        let ws_dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(ws_dir.path(), Check::Command { cmd: "test -f touched".into(), expect_exit: 0 }, 0);
        store.insert(&task).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done { summary: "unused".into(), evidence: vec![], usage: None }),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured: captured.clone(),
            spawn_failure: true,
        });
        let mut d = pool_dispatcher_with_requeues(store.clone(), adapter, None, dir.path().to_path_buf(), 1, 1, 0);
        let (tx, mut rx) = tokio::sync::watch::channel(None);
        d.set_snapshot_publisher(SnapshotPublisher {
            tx,
            instance_id: "inst".into(),
            hostname: "h".into(),
            started_at: "t".into(),
            tick_ms: 1,
            providers: Vec::new(),
            provider_checks: HashMap::new(),
        });
        for _ in 0..50 {
            d.tick().unwrap();
            if store.events_for(task.id).unwrap().iter().any(|(_, e)| matches!(e, Event::WorkerFinished { .. })) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let events = store.events_for(task.id).unwrap();
        let account = events.iter().find_map(|(_, e)| match e {
            Event::WorkerStarted { account, .. } => account.clone(),
            _ => None,
        });
        assert!(account.is_some(), "run should have used a pooled account: {events:?}");
        // `ProviderThrottled` イベントが記録される（アカウントの cooldown としては扱わない）。
        assert!(events.iter().any(|(_, e)| matches!(e, Event::ProviderThrottled { .. })));

        // プロバイダは cooldown になる。アカウント自体は cooldown にならない。
        d.tick().unwrap(); // もう 1 tick 回し、最新のスナップショットを送らせる。
        rx.changed().await.ok();
        let snapshot = rx.borrow().clone().unwrap();
        assert!(!snapshot.cooldowns.is_empty(), "provider should be cooling down: {snapshot:?}");
        assert!(
            snapshot.accounts.iter().all(|a| a.cooldown.is_none()),
            "no account should be cooling down: {:?}",
            snapshot.accounts
        );
    }

    /// (c) プールに選べるアカウントが無ければ、プールのプロバイダは満杯として扱われ、
    /// 非プールのプロバイダにフォールバックする（ADR-0012 D2、ADR-0024 D2）。
    #[tokio::test]
    async fn no_eligible_account_falls_back_to_a_non_pool_provider() {
        // アカウントは 1 つも作らない（ディレクトリはあるが空 = 選べるアカウント無し）。
        let dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(ws_dir.path(), Check::Command { cmd: "test -f touched".into(), expect_exit: 0 }, 0);
        store.insert(&task).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let pool_adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done { summary: "should not run".into(), evidence: vec![], usage: None }),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured,
            spawn_failure: false,
        });
        let fallback_adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None },
            delay: Duration::ZERO,
        });
        let mut d = pool_dispatcher(
            store.clone(),
            pool_adapter,
            Some(("p2", fallback_adapter)),
            dir.path().to_path_buf(),
            2,
            2,
        );
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Done);
        let events = store.events_for(task.id).unwrap();
        let (provider, account) = events
            .iter()
            .find_map(|(_, e)| match e {
                Event::WorkerStarted { provider, account, .. } => Some((provider.clone(), account.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(provider.as_deref(), Some("p2"));
        assert_eq!(account, None);
    }

    /// (d) run の途中で受け取った `rate_limit_event` の観測値が `AccountBook` とスナップショットに反映される。
    #[tokio::test]
    async fn mid_run_rate_limit_observation_lands_in_the_book_and_the_snapshot() {
        let dir = accounts_fixture();
        let ws_dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(ws_dir.path(), Check::Command { cmd: "test -f touched".into(), expect_exit: 0 }, 0);
        store.insert(&task).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None }),
            delay: Duration::from_millis(20),
            observation: Some(usage_window(0.42, 90_000)),
            env: Vec::new(),
            captured,
            spawn_failure: false,
        });
        let mut d = pool_dispatcher(store.clone(), adapter, None, dir.path().to_path_buf(), 2, 2);
        let (tx, mut rx) = tokio::sync::watch::channel(None);
        d.set_snapshot_publisher(SnapshotPublisher {
            tx,
            instance_id: "inst".into(),
            hostname: "h".into(),
            started_at: "t".into(),
            tick_ms: 1,
            providers: Vec::new(),
            provider_checks: HashMap::new(),
        });
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        d.tick().unwrap(); // 完了後もう 1 tick 回し、最新のスナップショットを送らせる。
        rx.changed().await.ok();
        let snapshot = rx.borrow().clone().unwrap();
        assert_eq!(snapshot.accounts_root.as_deref(), Some(dir.path().to_string_lossy().as_ref()));
        assert_eq!(snapshot.max_runs_per_account, Some(2));
        let a_or_b = snapshot
            .accounts
            .iter()
            .find(|a| a.usage.is_some())
            .unwrap_or_else(|| panic!("no account carries the observation: {:?}", snapshot.accounts));
        let usage = a_or_b.usage.as_ref().unwrap();
        assert_eq!(usage.five_hour.map(|w| w.utilization), Some(0.42));
        assert_eq!(usage.source, "run");
    }

    /// (e) 帳簿（`AccountBook`）はファイルに保存され、taskd の再起動（新しい `Dispatcher`）後も残る。
    #[tokio::test]
    async fn account_book_is_persisted_and_reloaded_after_restart() {
        let dir = accounts_fixture();
        let ws_dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(ws_dir.path(), Check::Command { cmd: "test -f touched".into(), expect_exit: 0 }, 0);
        store.insert(&task).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done { summary: "ok".into(), evidence: vec![], usage: None }),
            delay: Duration::ZERO,
            observation: Some(usage_window(0.33, 90_000)),
            env: Vec::new(),
            captured,
            spawn_failure: false,
        });
        let mut d1 = pool_dispatcher(store.clone(), adapter, None, dir.path().to_path_buf(), 2, 2);
        let report = run_until_idle(&mut d1, 200).await;
        assert!(report.idle);
        assert!(dir.path().join(".taskd-usage.json").exists());
        drop(d1);

        // "taskd を再起動" = 新しい Dispatcher（同じ store・同じ accounts root）を作る。
        let never_used = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done { summary: "unused".into(), evidence: vec![], usage: None }),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured: Arc::new(StdMutex::new(Vec::new())),
            spawn_failure: false,
        });
        let mut d2 = pool_dispatcher(store.clone(), never_used, None, dir.path().to_path_buf(), 2, 2);
        let (tx, mut rx) = tokio::sync::watch::channel(None);
        d2.set_snapshot_publisher(SnapshotPublisher {
            tx,
            instance_id: "inst2".into(),
            hostname: "h".into(),
            started_at: "t".into(),
            tick_ms: 1,
            providers: Vec::new(),
            provider_checks: HashMap::new(),
        });
        d2.tick().unwrap();
        rx.changed().await.ok();
        let snapshot = rx.borrow().clone().unwrap();
        let used_account = snapshot.accounts.iter().find(|a| a.usage.is_some());
        let usage = used_account.expect("observation survives restart").usage.as_ref().unwrap();
        assert_eq!(usage.five_hour.map(|w| w.utilization), Some(0.33));
    }
}
