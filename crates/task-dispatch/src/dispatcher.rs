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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use task_core::plan::{PlanLimits, PlanOutput, materialize_logging};
use task_core::report::{HEADLINE_MAX_CHARS, first_line, truncate_chars};
use task_core::{
    AccountAdapter, ArtifactRef, Check, DelegateTask, DelegationLimits, Event, GenreSpec,
    ListFilter, ListOrder, NodeSession, OnChildFailure, OrgKind, ProjectId, ProjectStatus,
    RateLimitObservation, RoleSpec, RunRole, SessionKind, Status, StoreError, Task, TaskId,
    TaskKind, TaskStore, Tier, Trigger, WorkspaceMode, WorkspaceSpec, support_kind,
};
use task_ops::daemon::{
    AccountCooldownLive, AccountLive, AccountUsageLive, ClusterLive, CooldownView, DaemonSnapshot,
    InFlight, InFlightKind, ProviderCheckView, ProviderLive, TunnelForwardLive,
};
use task_ops::delegate::{pending_children, plan_delegation};
use task_ops::derive::{
    AnswerNote, INFRA_FAILURE_MARKER, REVIEWER_INFRA_FAILURE_PREFIX, REVIEWER_REQUEUED_PREFIX,
    ReviewNote, answers_from_events, approval_decision_note, artifacts_for_run,
    consecutive_continuations, consecutive_infra_requeues, consecutive_requeues,
    consecutive_reviewer_infra_failures, consecutive_reviewer_requeues, current_run_seq,
    human_approval_title, infra_backoff_delay, last_run_id, latest_checkpoint, no_progress_streak,
    prior_review_from_events, retry_backoff,
};
use task_worker::{
    ActiveMilestoneContext, ActiveProjectContext, AdapterError, Answer, ChildSummary,
    CommentContext, ConversationAddressee, ConversationTurn, EventSink, GenreContext,
    LocalWorkspace, MemoryContext, MemoryDir, MilestoneBrief, MilestoneReviewContext,
    MilestoneTaskResult, NodeContext, OrgNodeContext, PROTOCOL_VERSION, PriorReview, Reachability,
    RecentWork, RoleContext, RunContext, RunLimits, RunOutcome, RunRequest, SshSettings,
    SshWorkspace, SyncMode, Terminal, WorkerAdapter, WorkerMessage, Workspace, WorkspaceError,
    control_master_alive_blocking, remote_dir_is_resolved, remote_exec_instructions,
    resolve_remote_dir,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::accounts::{
    AccountBook, AccountCandidate, AccountCheckRecord, AccountCooldownReason, AccountDir,
    ExcludedReason, ObservationSource, cooldown_for_failure, evaluate, scan_accounts,
    select_account,
};
use crate::policy::{
    AdapterId, CooldownReason, ProviderId, ProviderOutcome, ProviderPolicy, Selection,
};

/// これを超えた tick は段階ごとの所要時間を `warn` で出す（ADR-0015 D2）。
const SLOW_TICK: Duration = Duration::from_secs(1);

/// tick の中の 1 段階がこれを超えたら `warn`（ADR-0015 D2。遅いのが DB かファイルかを切り分ける）。
const SLOW_STEP: Duration = Duration::from_millis(500);

/// ADR-0070 D5（Phase 116）: `StoreSink::heartbeat` が `renew_lease` の DB busy/locked をリトライする回数。
const RENEW_LEASE_RETRIES: u32 = 3;
/// ADR-0070 D5: 上のリトライの間隔。
const RENEW_LEASE_RETRY_DELAY: Duration = Duration::from_millis(100);

fn log_slow_step(step: &'static str, started: Instant) {
    let elapsed = started.elapsed();
    if elapsed >= SLOW_STEP {
        tracing::warn!(
            step,
            duration_ms = elapsed.as_millis() as u64,
            "slow dispatcher step"
        );
    }
}

/// ADR-0018: コマンドを実行するクラスタ 1 つ分の設定（`celeris::config::ClusterConfig` の写し。task-dispatch は celeris に依存しない）。
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
    /// ADR-0053 D3（Phase 66）: このクラスタの ssh master に張る port forward（Qwen トンネル等）。
    /// 空なら `refresh_cluster_tunnels` は何もしない（従来どおり）。
    pub forwards: Vec<ClusterForwardSpec>,
    /// ADR-0059 D6: 設定ファイルの `work_dir`（`[[clusters]] work_dir`）。DB の上書き
    /// （`cluster_settings`）があればそちらが勝つ（`Dispatcher::effective_work_dir` が決める）。
    pub work_dir: Option<PathBuf>,
    /// ADR-0062 A（Phase 107）: master の argv に足す `-o ServerAliveInterval=<n> -o
    /// ServerAliveCountMax=3 -o TCPKeepAlive=yes`（`0` なら keepalive を付けない）。既定 30 秒。
    /// コマンドラインの `-o` は `~/.ssh/config` より優先されるので、人の設定を変えずに効く。
    pub keepalive_secs: u64,
    /// ADR-0062 A: master 越しの実通信（`ssh -o BatchMode=yes <host> -- true`）による生存確認を
    /// この秒数ごとに行う（`0` で無効）。既定 300 秒。NAT / ファイアウォールの idle timeout で
    /// TCP が黙って死んでも、`-O check`（unix socket を見るだけ）は気づかないので、実通信で確定させる。
    pub liveness_probe_secs: u64,
}

/// ADR-0062 B1（Phase 107）: `resolve_cluster` の結果。`cluster_of`（従来の `Option` 契約）はこれを
/// 畳んだだけだが、`dispatch_ready` は `NotConfigured`（設定にクラスタが無い）と
/// `AssigneeLacksTool`（担当が `cluster:<id>` を持たない）を区別して扱う（前者は従来どおり
/// `unroutable` の警告、後者は `blocked` にして人に聞く）。
enum ClusterResolution {
    /// `WorkspaceSpec::Local` のタスク。
    Local,
    /// `clippy::large_enum_variant`: `ClusterSpec` は大きいので `Box` に入れる。
    Resolved(Box<ClusterSpec>, PathBuf, WorkspaceMode),
    /// `[[clusters]]` にそのクラスタ id が無い。
    NotConfigured,
    /// クラスタは設定にあるが、担当が `cluster:<id>` を持たない（ADR-0062 B1）。
    AssigneeLacksTool { cluster: String },
}

/// ADR-0053 D3: 1 本の port forward（`ssh -O forward -L <listen>:<target> <host>` 相当）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterForwardSpec {
    /// ローカル（celeris が bind する側）。`"127.0.0.1:18000"` の形。
    pub listen: String,
    /// master のホスト側から見た転送先。`"bnode150:18000"` の形。
    pub target: String,
    /// ADR-0053 Phase 85: target（`/v1/models`）の健康 probe をこの秒数より短い間隔では行わない
    /// （`[[clusters.forwards]] probe_interval_secs`。既定 30 秒）。リスナーの有無の確認（軽い TCP
    /// connect）はこの間隔に縛られない — 毎回の `refresh_cluster_tunnels` で見る。
    pub probe_interval_secs: u64,
}

/// ADR-0032 D3: `auth = "publickey"` のクラスタに未接続なら、cooldown にする前にディスパッチャが 1 回だけ
/// 接続を試みるためのフック。引数は `(cluster_id, host)`。同期でブロックしてよい（`control_master_alive_blocking`
/// と同じ扱い）。本番では celeris が `task_worker::cluster_login::start_connect` 相当の実装を挿す。テストでは
/// 偽物を挿す。未設定（`None`）なら自動接続はせず、従来どおり cooldown に落ちる。
///
/// ADR-0053 D3（Phase 66）: `refresh_cluster_tunnels` もこの同じフックを再利用する。「TOTP を要求する前に
/// 鍵認証を試す」ため、`auth` の値に関わらず（`"totp"` のクラスタでも）まず呼ぶ。`"totp"` クラスタでは
/// 通常失敗する（鍵だけでは入れない）が、既にセッションが有効ならそのまま繋がることがある。
pub type ClusterConnector = Arc<dyn Fn(&str, &str) -> Result<(), String> + Send + Sync>;

/// ADR-0053 D3: master 上に forward を用意する（無ければ張る、あれば何もしない。冪等）フック。
/// 引数は `(host, listen, target)`。本番では celeris が `ssh -O forward -L <listen>:<target> <host>`
/// （届かなければ master 上で `ssh -N -L` を張るフォールバック）を挿す。テストは偽物を挿す。
pub type TunnelForwardEnsurer = Arc<dyn Fn(&str, &str, &str) -> Result<(), String> + Send + Sync>;

/// ADR-0053 D3 / Phase 85: forward の**先方（target）**の健康を見るフック。引数は `listen`
/// （`"127.0.0.1:18000"`）。本番では `GET http://<listen>/v1/models` の probe
/// （`task_worker::probe_models`）を挿す。テストは偽物を挿す。**listener（`-O forward` が届いているか）
/// とは別物**: これは `refresh_one_forward` がリスナーの存在を確認した後、かつ `probe_interval_secs` の
/// 間隔でしか呼ばない（Phase 85 のバックオフ。本番で `-O forward` は張れているのに先方の vLLM が
/// 落ちている観測から、tick を毎回 3 秒級の HTTP で遅くしないため）。
///
/// 届かないときは人が読む 1 行の理由（時間切れ・接続拒否・HTTP ステータス）を `Err` で返す。
/// `last_error` にそのまま載る（2026-09-24: 転送先が master のホストから時間切れなのか、先方の
/// vLLM が落ちているのかが `target_unreachable` だけでは見分けられなかった）。
pub type TunnelProbe = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// ADR-0053 Phase 85: forward の**リスナー**（`-O forward`/`ssh -N -L` が実際に手元の `listen` で待ち受けて
/// いるか）を見るフック。軽い確認（`listen` への TCP connect 等。ssh を起こさない）を想定。`None`
/// （フックを挿さない）のときは常に「存在しない」として扱う（＝安全側のデフォルト。`tunnel_forward_ensurer`
/// に(再)確立を試みさせる。Phase 66 までの「probe が失敗したら毎回張り直す」と同じ保守的な挙動）。
///
/// **listener の有無と target の健康は別の観測**（Phase 85 の本旨）: listener が有るのに target が
/// 不健全（先方が落ちている）なら `-O forward` は再発行しない（listener は既に有るので無意味な上、
/// 本番でこれが毎 tick 起きて tick が 6 秒に伸びた。`docs/adr/0053-llm-source-proxy.md`「Phase 85 追記」）。
pub type TunnelListenerProbe = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// ADR-0053 Phase 84b: クラスタの ssh master の多重接続の有無を調べるフック。引数は
/// `(ssh_command, host)`（`control_master_alive_blocking` と同じ形）。既定（`Dispatcher::new`）は本物の
/// `ssh -O check`（`control_master_alive_blocking`）。
///
/// **観測（2026-09-21）**: このフックが無かった頃は `refresh_cluster_liveness` が常に本物の
/// `control_master_alive_blocking` を直接呼んでいたため、`crates/celeris/src/lib.rs` の
/// `a_totp_cluster_with_a_forward_does_not_panic_the_first_tick_phase_66b` が `host = "pegasus"` の
/// クラスタでテストしていたところ、テストを動かすマシン自身が実際に `pegasus` へ ssh ControlMaster を
/// 張っていた（人が別作業で張った）ため、テストの意図（master 未接続 → `cluster_connector` が呼ばれる）
/// に反して「master 生存」と判定され、`cluster_connector` が一度も呼ばれずに落ちた。テストは実機の ssh
/// 状態に依存してはならないので、このフックで差し替え可能にした（テストは常に偽物を挿す。本物の
/// `ssh -O check` を経由するのは本番だけ）。
pub type ClusterLivenessProbe = Arc<dyn Fn(&[String], &str) -> bool + Send + Sync>;

/// ADR-0062 A（Phase 107）: master 越しの実通信で生存を確定させるフック。引数は
/// `(ssh_command, host, timeout)`。本物の実装は `ssh -o BatchMode=yes <host> -- true` を
/// `timeout` で打ち切って呼ぶ（本番では `task_worker::ssh::control_master_command_probe_blocking`）。
/// `-O check` は unix ソケットを見るだけなので、NAT / ファイアウォールの idle timeout で TCP が
/// 黙って死んでいても気づかない。こちらは実際にリモートへコマンドを 1 つ流すので確定できる。
pub type ClusterCommandProbe = Arc<dyn Fn(&[String], &str, Duration) -> bool + Send + Sync>;

/// ADR-0062 A: 実通信 probe の打ち切りに使うタイムアウト（既定 10 秒）。
const LIVENESS_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// ADR-0062 A: 死んだ接続の残骸を `ssh -O exit` で片付けるフック。引数は `(cluster_id, host)`。
pub type ClusterDisconnector = Arc<dyn Fn(&str, &str) -> Result<(), String> + Send + Sync>;

/// ADR-0062 A: celeris が保持している ssh master の終了を検出するフック。呼ぶたびに、その時点で
/// 新たに終了が確認できた master の一覧を返す（同じ終了を二度返さない契約。本物の実装は
/// `ClusterMasters` から exited のものを取り除きながら集める）。
pub type ClusterMasterWatcher = Arc<dyn Fn() -> Vec<ClusterMasterExit> + Send + Sync>;

/// ADR-0062 A: `ClusterMasterWatcher` が返す 1 件（自分で終了した master）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterMasterExit {
    pub cluster: String,
    pub exit_code: Option<i32>,
    pub stderr_tail: String,
}

/// ADR-0062 A: 明示的な切断を経ずに失われた接続の詳細。`mark_cluster_unavailable` が読み、
/// 報告に足してから `Event::ClusterMasterExited` を 1 回だけ残す（`reported`）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClusterDisconnectInfo {
    exit_code: Option<i32>,
    detail: String,
    reported: bool,
}

/// ADR-0053 D3: トンネル 1 本の状態遷移（Console / cluster API に出す。`take_tunnel_events` で取り出す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelEvent {
    pub cluster: String,
    /// `"login_needed"` のときは空。
    pub listen: String,
    pub kind: TunnelEventKind,
    pub at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelEventKind {
    /// forward が初めて（または前回の観測が無い状態から）繋がった（listener 有り・target 健全）。
    Up,
    /// forward の**リスナーが消えた**（master は生きているが `-O forward` が届かない）。
    Down,
    /// 一度 `Down`/`TargetUnreachable` を観測した forward が繋がり直した（listener 有り・target 健全）。
    Restored,
    /// master が落ち、鍵認証も失敗した（人の TOTP が要る）。
    LoginNeeded,
    /// ADR-0053 Phase 85: listener は有る（`-O forward` は張れている）が、target（先方の vLLM 等）が
    /// `/v1/models` に応答しない。**listener が無いわけではないので re-add はしない**（本番観測: forward は
    /// 張れているのに bnode150 が応答せず、`Down` 扱いで毎 tick 張り直していた不具合の修正）。
    TargetUnreachable,
}

impl TunnelEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TunnelEventKind::Up => "up",
            TunnelEventKind::Down => "down",
            TunnelEventKind::Restored => "restored",
            TunnelEventKind::LoginNeeded => "login_needed",
            TunnelEventKind::TargetUnreachable => "target_unreachable",
        }
    }
}

/// ADR-0053 D3: `tunnel_events` に積む上限（古いものから捨てる。無限に溜め込まない）。
const TUNNEL_EVENTS_CAP: usize = 100;

/// `Dispatcher::tunnel_state` のキー。
fn tunnel_key(cluster: &str, listen: &str) -> String {
    format!("{cluster}\u{0}{listen}")
}

/// ADR-0053 Phase 85: `[[clusters.forwards]] probe_interval_secs` の既定（30 秒）。
pub const DEFAULT_TUNNEL_PROBE_INTERVAL_SECS: u64 = 30;

/// ADR-0053 Phase 85: 1 forward の直近の観測（`Dispatcher::tunnel_state` に積む）。listener（`-O forward`
/// の有無）と target の健康（`/v1/models`）を別々に持つ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ForwardObservation {
    /// `-O forward`/`ssh -N -L` の手元のリスナーが有るか。
    listener: bool,
    /// listener 越しに target（`/v1/models`）が健全か。listener が無ければ意味を持たない（常に `false`）。
    target_healthy: bool,
    /// 直近の失敗理由（無ければ `None`）。
    last_error: Option<String>,
}

impl ForwardObservation {
    /// 従来の「forward が届く」（`tunnel_reachable`）と同じ意味: listener も target も健全。
    fn up(&self) -> bool {
        self.listener && self.target_healthy
    }

    fn phase(&self) -> ForwardPhase {
        match (self.listener, self.target_healthy) {
            (true, true) => ForwardPhase::Up,
            (true, false) => ForwardPhase::TargetUnreachable,
            (false, _) => ForwardPhase::Down,
        }
    }
}

/// `ForwardObservation` から導いた 3 値（イベント種別の判定用。`Unknown` は「まだ観測が無い」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForwardPhase {
    Up,
    Down,
    TargetUnreachable,
}

/// ADR-0066 D3（Phase 110b）: target probe の連続失敗に対する間隔の上限（10 分）。
/// `probe_interval_secs`（既定 30 秒。forward ごとに設定可）を最小間隔として、失敗するたびに倍にして
/// ここで頭打ちにする。1 回でも成功すれば `probe_interval_secs` に戻る。
pub const TUNNEL_PROBE_MAX_INTERVAL_SECS: u64 = 600;

/// 次に probe するまでの間隔（純粋関数）: 成功したら `min_secs`（forward の `probe_interval_secs`）に
/// 戻す。失敗したら現在の間隔を倍にし、`TUNNEL_PROBE_MAX_INTERVAL_SECS` で頭打ちにする
/// （30 → 60 → 120 → 240 → 480 → 600…）。`current_secs` が `min_secs` を下回っていても `min_secs` を
/// 下限にする（設定変更で `probe_interval_secs` が上がった場合の安全側）。
fn next_probe_interval_secs(current_secs: u64, min_secs: u64, healthy: bool) -> u64 {
    if healthy {
        return min_secs;
    }
    current_secs
        .max(min_secs)
        .saturating_mul(2)
        .min(TUNNEL_PROBE_MAX_INTERVAL_SECS)
}

/// ADR-0066 D3: forward 1 本の target probe の直近の結果（`Dispatcher::tunnel_probe_state` に積む。
/// 専用スレッドが書き、tick は読むだけ）。
#[derive(Debug, Clone)]
struct TargetProbeState {
    healthy: bool,
    /// `healthy == false` のときの理由（[`TunnelProbe`] の `Err`）。
    error: Option<String>,
    checked_at: Instant,
    /// 次に probe するまでの間隔（バックオフ済み）。
    interval_secs: u64,
}

/// ADR-0041 D5: この celeris が**面倒を見てよいタスク**の述語。`None`（既定）は「全部」＝従来どおり。
///
/// 検証（`--mode verify`）の celeris は、ここに「`genre = "smoke"` で、かつアダプタが `fake`」を渡す。
/// dispatch だけでなく、**ストア上の他のタスクの状態を変えうる経路すべて**（期限切れリースの回収、
/// `reviewing` の拾い上げ）で同じ述語を使う。手元で起こした run の後始末はこの述語に関係なく続ける
/// （自分が起こしたものは必ず自分が畳む）。
pub type TaskFilter = Arc<dyn Fn(&Task) -> bool + Send + Sync>;

impl ClusterSpec {
    /// このタスクの写し（ローカル）とリモートのパス（呼び出し側が D6 の `work_dir` で解決済み）から、
    /// ワーカー用の設定を作る。`task_id` は worktree のディレクトリ名とブランチ名に使う（ADR-0019 D2）。
    /// `mode`（ADR-0059 D1、`WorkspaceSpec::Remote.mode`）が `Shared` なら、クラスタの `sync` 設定に
    /// 関わらず**同期も worktree も行わない**（`SyncMode::None`）。`Worktree`（省略時の既定を含む）は
    /// 従来どおりクラスタの `sync` に従う。
    pub fn ssh_settings(
        &self,
        remote_path: &std::path::Path,
        task_id: task_core::TaskId,
        mode: task_core::WorkspaceMode,
    ) -> SshSettings {
        let mut settings = SshSettings::new(self.id.clone(), self.host.clone(), remote_path);
        settings.sync = match mode {
            task_core::WorkspaceMode::Shared => SyncMode::None,
            task_core::WorkspaceMode::Worktree => self.sync,
        };
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

/// ADR-0053 D3 / Phase 66b: `refresh_cluster_liveness` / `refresh_cluster_tunnels` は `ssh` を同期に
/// 呼び、`cluster_connector`（ADR-0032 D3）経由でネストした tokio ランタイムを `block_on` することがある。
/// `tick()` は celeris の tick ループの中から**同期のまま**呼ばれ、そのループ自身が（マルチスレッドの）
/// tokio ランタイムの上で動く async タスクなので、ここで直接ブロックすると (a) 他の非同期処理を
/// 数秒単位で止め、(b) ネストしたランタイムの `block_on` が「Cannot start a runtime from within a
/// runtime」で panic する（本番 2026-09-21 の観測、`crates/celeris/src/lib.rs` の `cluster_connector`）。
///
/// `f` を、tokio の文脈を一切持たない本物の OS スレッドへ逃がして実行する。マルチスレッド・ランタイムの
/// 中から呼ばれたときだけ `block_in_place` で包み、スケジューラが他のタスクを別ワーカーへ逃がせるように
/// する（`block_in_place` は `current_thread` ランタイムの中で呼ぶと panic するため、既存の
/// `#[tokio::test]`〈既定は current_thread〉の `tunnel_*` テストや、ランタイムが無い素の同期呼び出しでは
/// 使わない。どちらの場合も `f` は素の OS スレッドで動くので、ネストしたランタイムを作っても安全）。
///
/// Phase 81: `f` の戻り値をそのまま返す（`try_auto_connect_cluster` は `Result<(), String>` を
/// 呼び出し元に返す必要があるため、`refresh_cluster_liveness`/`refresh_cluster_tunnels` の頃の
/// `FnOnce() + Send`〈戻り値 `()`〉から一般化した。既存の 2 呼び出し（`()` を返す）はそのまま通る）。
fn run_cluster_hooks_off_async<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    let on_multi_thread_runtime = tokio::runtime::Handle::try_current()
        .map(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
        .unwrap_or(false);
    let spawn_and_join = move || match std::thread::scope(|scope| scope.spawn(f).join()) {
        Ok(v) => v,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    if on_multi_thread_runtime {
        tokio::task::block_in_place(spawn_and_join)
    } else {
        spawn_and_join()
    }
}

/// RFC 3339 の文字列（デーモンのスナップショット用）。書式化に失敗することは実質無いが、その場合は空文字列。
fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}
use crate::review::{
    HumanVerdicts, PLAN_FILE_NAME, PlanCheck, ReviewExtras, ReviewOutcome, ReviewSubject,
    ReviewerRun, Verdict, needs_reviewer_run, review_task,
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

/// ADR-0024/0025: `[accounts]` があるときのプール実行時設定（`celeris::config::AccountsConfig` の写し）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountsRuntimeConfig {
    /// ADR-0025 D1: アダプタごとの根ディレクトリ。`<root>/<id>/` が 1 アカウント。どちらか一方だけでもよい。
    pub roots: HashMap<AccountAdapter, PathBuf>,
    pub max_runs_per_account: usize,
    /// D6 の確認に使うモデル（celeris 側が使う。ディスパッチャ自身は確認を行わない。claude-code のみ）。
    pub check_model: String,
    /// 供給側失敗でアカウントを cooldown にするときのフォールバック秒数（= `error_cooldown_secs`）。
    pub fallback_cooldown_secs: u64,
}

impl AccountsRuntimeConfig {
    pub fn root_for(&self, adapter: AccountAdapter) -> Option<&PathBuf> {
        self.roots.get(&adapter)
    }
}

/// ディスパッチャの設定（`config.toml` から組み立てる。ADR-0005 D7）。
#[derive(Debug, Clone)]
pub struct DispatchConfig {
    pub delivery: task_ops::delivery::DeliveryPolicy,
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
    /// ADR-0069 Phase 118 D4: `tier` は他に何も分からないときの既定値（後方互換）で、実際の lane は
    /// `pick_reviewer` が `reviewer_tier_override` / 部署の `profile.review_tier` / worker lane から
    /// 動的に決める。
    pub reviewer_hint: task_core::WorkerHint,
    /// ADR-0069 Phase 118 D4: `[reviewer] tier` が明示されているときだけ `Some`（設定の優先順位で
    /// worker lane 一致の既定より強いが、部署の `profile.review_tier` には負ける）。
    pub reviewer_tier_override: Option<task_core::Tier>,
    /// ADR-0018: `WorkspaceSpec::Remote{cluster}` が指すクラスタ。キーは `cluster` の名前。
    pub clusters: HashMap<String, ClusterSpec>,
    /// ADR-0018 D2: 多重接続が無いクラスタを、この時間だけ dispatch の対象から外す。
    pub cluster_cooldown: Duration,
    /// ADR-0011（P-38）: 同じ試行での連続 requeue の上限。達したら供給側失敗を通常の失敗（attempts 消費）として扱う。
    pub max_requeues: u32,
    /// ADR-0054 D2（Phase 113）: `[review] max_reviewer_retries`。Reviewer run **自身のインフラ都合の
    /// 失敗**（`is_error` の結果・プロセス失敗・resume 拒否など。プロバイダが分類できた供給側失敗の
    /// `max_requeues` とは別軸）で reviewing を延期できる連続回数の上限。達したら「判定できなかった」
    /// を「reviewer infra failure ×N」として不合格にする（`fail_all` はしない。人の承認・command・
    /// artifact_exists の結果は保持する）。既定 3。
    pub max_reviewer_retries: u32,
    /// ADR-0070 D3（Phase 116）: `[dispatch] max_infra_retries`。ワーカー run **自身のインフラ都合の
    /// 失敗**（lease 失効・切替による中断・result.json 不在・セッション再開拒否・レート制限・DB busy。
    /// `provider_failure_outcome` が分類できない `Err`）で `attempts` を消費せず再試行できる連続回数の
    /// 上限（`consecutive_infra_requeues`）。達したら `WorkerError{retryable:false}` で
    /// `"infra failure ×N: …"` として打ち切る（`Trigger::LeaseExpired` は使わなくなった。
    /// `reclaim_expired_leases` もこの上限を通す）。既定 5。
    pub max_infra_retries: u32,
    /// ADR-0016 D1: `[[roles]]`。run 開始時に `RunContext.role`（指示文）を載せ、委譲された子の既定に使う。
    pub roles: Vec<RoleSpec>,
    /// ADR-0027 D1: `[[genres]]`。委譲の分野解決（`default_role` の既定の穴埋め）と、委譲できる run に渡す
    /// `RunContext.available_genres` に使う。
    pub genres: Vec<GenreSpec>,
    /// ADR-0016 D2: 委譲の上限（1 run の件数・木の深さ・木の run 数）。
    pub delegation: DelegationLimits,
    /// ADR-0024: `[accounts]` が設定されていればプール選択を有効にする。
    pub accounts: Option<AccountsRuntimeConfig>,
    /// ADR-0033 D6（Phase 24）: `[memory] dir`（絶対パス）。`None` なら記憶を読まないし書かない。
    pub memory_dir: Option<PathBuf>,
    /// ADR-0041 D1 / ADR-0042 D3: ローカルの worktree のブランチ接頭辞
    /// （`[workspace] worktree_branch_prefix`、既定 `celeris/`）。
    pub worktree_branch_prefix: String,
    /// ADR-0041 D1: `[selfdeploy] releases_dir`。その**親**の `current/manifest.json` が読めれば、
    /// 本番の sha を worktree の base の候補にする。`None` なら base は常に `main`（か `HEAD`）。
    pub releases_dir: Option<PathBuf>,
    /// ADR-0043 D3（Phase 56）: `[containers]`。コンテナ実行の runtime・既定のイメージ・ビルドの置き場。
    pub containers: ContainersRuntimeConfig,
    // ---- ADR-0047（Phase 61）: 知識ベース。ここから ----
    /// ADR-0047 D1 / D2: `[knowledge]`。正本の置き場と既定のマウント。
    pub knowledge: KnowledgeRuntimeConfig,
    // ---- ADR-0047（Phase 61）: ここまで ----
    /// ADR-0054 D1（Phase 67）: `[sessions] rollover_tokens`。CoS の対話・部門長のレビュー run の
    /// 継続セッションで、`approx_tokens`（run の usage の累計）がこれを超えたら次の run から
    /// 新しいセッションにする（要約を前置きに）。既定 400,000（`celeris::config` 側の既定値と同じ）。
    pub session_rollover_tokens: u64,
    /// ADR-0066 D1（Phase 110b）: `[workspace] shared_build_cache`（既定 true）。ローカルの git
    /// worktree のホスト実行に `CARGO_TARGET_DIR` を与えるかどうか。
    pub shared_build_cache: bool,
    /// ADR-0066 D1: `[workspace] build_cache_dir`（既定 `~/.local/celeris/build-cache`）。
    pub build_cache_dir: PathBuf,
    /// ADR-0066 D2（Phase 110b）: `[workspace] prune_after_secs`（既定 86400、`0` で無効）。終端に
    /// なってからこの秒数経った作業場所から、ビルド生成物だけを刈る。
    pub workspace_prune_after_secs: u64,
    /// ADR-0072 D18（Phase E1）: `[execution]`。continuation の可否と上限。
    pub execution: ExecutionConfig,
}

/// ADR-0072 D18（Phase E1）: `[execution]`。continuation（予算切れ・yield の続き）の可否と上限。
/// E2 以降の欄（`max_work_units` 等）は ExecutionPlan/WorkUnit と一緒に導入する（今回は範囲外）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionConfig {
    /// `[execution] continuation`（既定 `true`）。`false` なら E1 の continuation を無効にし、
    /// 予算切れ・yield を従来どおり `WorkerError{retryable:true}` として扱う（ADR-0072 §6 (f)）。
    pub continuation: bool,
    /// `[execution] max_continuations_per_work_unit`（既定 3）。暗黙の WorkUnit では 1 タスクの
    /// continuation の合計回数（1 つの WU の Run は最大 `continuation + 1` 回）。
    pub max_continuations_per_work_unit: u32,
    /// `[execution] no_progress_limit`（既定 2）。進捗なしの continuation が連続この回数で
    /// `blocked` にする。
    pub no_progress_limit: u32,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            continuation: true,
            max_continuations_per_work_unit: 3,
            no_progress_limit: 2,
        }
    }
}

/// `[knowledge]`（ADR-0047 D1 / D2。Phase 61）。
#[derive(Debug, Clone, Default)]
pub struct KnowledgeRuntimeConfig {
    /// 正本の置き場（絶対パス。既定 `~/.local/share/celeris/knowledge`）。**celeris は作らない**。
    pub root: PathBuf,
    /// 実効 profile（ADR-0046 D1）が何も言わないときに全ノードが継ぐマウント。
    pub default_mounts: Vec<task_core::KnowledgeMount>,
    /// ADR-0052 D1（Phase 64）: `[knowledge.langmem].base_url`。知識整理タスクを dispatch する直前に
    /// `GET <base_url>/models` を当てる。`None` なら検査しない（＝従来どおり `langmem` で走らせる）。
    pub langmem_base_url: Option<String>,
    /// Phase 65b: `[knowledge.langmem].api_key_secret` から解決した平文のトークン（`[secrets] dir`
    /// が無い・見つからない等なら `None`）。到達性の probe が `Authorization: Bearer` に使う
    /// （`llm-proxy` のように `/v1/models` が認証を要求する上流を指したときのため）。**値はログに出さない**。
    pub langmem_api_key: Option<String>,
    /// ADR-0052 D2（Phase 64）: `knowledge` ハーネスの `fallback`（倒す先の tier）。`None` は
    /// 「倒さない」（`fallback = false` か、そもそも `knowledge` ハーネスが無い）。
    pub fallback_tier: Option<Tier>,
}

/// `[containers]`（ADR-0043 D3 / ADR-0042 D3）。
#[derive(Debug, Clone)]
pub struct ContainersRuntimeConfig {
    /// `runtime = "auto" | "podman" | "docker"`（既定 `auto` = podman を先に試す）。
    pub preference: task_worker::RuntimePreference,
    /// `[container] image` も `dockerfile` も無いときのイメージ（既定 `celeris-worker:latest`）。
    pub image_default: String,
    /// Dockerfile からビルドしたイメージの作業場所（既定 `~/.local/celeris/containers`）。
    pub build_dir: PathBuf,
    /// 1 回のビルドの上限（既定 1800 秒）。
    pub build_timeout: Duration,
}

impl Default for ContainersRuntimeConfig {
    fn default() -> Self {
        Self {
            preference: task_worker::RuntimePreference::Auto,
            image_default: task_worker::container::DEFAULT_IMAGE.to_string(),
            build_dir: PathBuf::from("."),
            build_timeout: Duration::from_secs(task_worker::container::DEFAULT_BUILD_TIMEOUT_SECS),
        }
    }
}

/// ADR-0043 D3（Phase 56）: 1 タスク分の実行環境の判断（ディスパッチャが dispatch のときに決める）。
#[derive(Debug, Clone)]
pub enum ContainerDecision {
    /// 従来どおりホストで走らせる。
    Host,
    /// コンテナが要るのに runtime が使えない → run を始めず `blocked` にして人に聞く。
    Unavailable { question: String },
    /// コンテナで走らせる（イメージの用意は `run_worker` が run の直前にやる）。
    Container(Box<ContainerRun>),
}

/// コンテナで走らせるときの一式（ADR-0043 D3）。
#[derive(Debug, Clone)]
pub struct ContainerRun {
    /// コンテナの形。`image` はイメージを決めた後に埋める。
    pub plan: task_worker::ContainerPlan,
    pub image: task_worker::ImageSource,
    pub image_default: String,
    pub build_root: PathBuf,
    pub build_timeout: Duration,
    /// コンテナを要求したリポジトリの名前（人に見せる文面に出す）。
    pub repo: String,
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
    /// Phase 55/56 の合流（ADR-0044 P55-4 / ADR-0043 P56-7）: この run をコンテナで走らせているなら、
    /// ラベルでコンテナを止める口。`killpg` はコンテナの中の PID 名前空間には届かないので、
    /// `stop_run` がこれを `task_worker::kill_tree_with` に渡す。ホスト実行なら `None`。
    container: Option<Arc<dyn task_worker::ContainerStopper>>,
}

/// ADR-0033 D4（Phase 24 / Phase 27）: この run の途中で「部をまたぐ委譲」を止めたときに `StoreSink` が
/// 残した質問（1 件の部またぎにつき 1 件。同じ文面は 1 回だけ）。
fn cross_department_questions_of(events: &[(u64, Event)], run_id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, e) in events {
        if let Event::QuestionRaised { run_id: r, text } = e
            && r == run_id
            && !out.contains(text)
        {
            out.push(text.clone());
        }
    }
    out
}

/// ADR-0072 D7（Phase E1）: 「二重の安全網」。構造化した `Terminal::BudgetExhausted` を返さない
/// 古い経路・アダプタのために、`Terminal::Error{retryable:true}` のメッセージを字句判定する
/// （`task_core::is_budget_outcome` と同じ語彙。分類できなければ `None`）。
fn classify_budget_kind_from_text(message: &str) -> Option<task_core::BudgetKind> {
    let m = message.to_lowercase();
    if task_core::looks_like_context_exceeded(&m) {
        Some(task_core::BudgetKind::Context)
    } else if m.contains("max_turns") || m.contains("max turns") || m.contains("turn limit") {
        Some(task_core::BudgetKind::Turns)
    } else if m.contains("wall-clock") || m.contains("wall clock") || m.contains("budget") {
        Some(task_core::BudgetKind::WallClock)
    } else {
        None
    }
}

/// ADR-0072 D9（Phase E1）: `WorkerFinished.outcome` に載せる、人が読む 1 行の終わり方の説明。
fn describe_run_end(end: task_core::RunEnd) -> String {
    match end {
        task_core::RunEnd::Completed => "completed".to_string(),
        task_core::RunEnd::Yielded => "yielded".to_string(),
        task_core::RunEnd::BudgetExhausted { kind } => {
            let k = match kind {
                task_core::BudgetKind::Turns => "turns",
                task_core::BudgetKind::WallClock => "wall_clock",
                task_core::BudgetKind::Context => "context",
            };
            format!("budget_exhausted({k})")
        }
        task_core::RunEnd::Question => "question".to_string(),
        task_core::RunEnd::Failed { retryable } => format!("failed(retryable={retryable})"),
        task_core::RunEnd::HarnessError { class } => format!("harness_error({class:?})"),
        task_core::RunEnd::Cancelled => "cancelled".to_string(),
    }
}

/// ADR-0072 D9（Phase E1）: この run が continuation（予算切れ・yield の続き）なら、次の run の
/// `RunContext.continuation` に渡す最小限の文脈を events から純粋に組み立てる。`events` には、
/// これから始まる run 自身の `WorkerStarted`（run_seq の計算に使う）が既に入っている前提
/// （`run_worker` が `dispatch` の遷移と `WorkerStarted` の追記のあとに呼ばれるため）。
/// continuation でなければ `None`（前の run の会話・出力の全文は載せない。D9）。
fn build_continuation_context(events: &[(u64, Event)]) -> Option<task_worker::ContinuationContext> {
    if consecutive_continuations(events) == 0 {
        return None;
    }
    let checkpoint = latest_checkpoint(events, None)?;
    let run_seq = current_run_seq(events);
    let previous_end = events
        .iter()
        .rev()
        .find_map(|(_, ev)| match ev {
            Event::WorkerFinished {
                role: None,
                end: Some(e),
                ..
            } => Some(describe_run_end(*e)),
            _ => None,
        })
        .unwrap_or_else(|| "budget_exhausted".to_string());
    // これまでの run の 1 行要約（古い順、最大 10 件）。
    let mut seq_by_run: HashMap<&str, u32> = HashMap::new();
    let mut n = 0u32;
    for (_, ev) in events {
        if let Event::WorkerStarted {
            run_id, role: None, ..
        } = ev
        {
            n += 1;
            seq_by_run.insert(run_id.as_str(), n);
        }
    }
    let mut prior_runs: Vec<String> = Vec::new();
    for (_, ev) in events {
        if let Event::WorkerFinished {
            run_id,
            role: None,
            end: Some(e),
            ..
        } = ev
            && let Some(seq) = seq_by_run.get(run_id.as_str())
        {
            prior_runs.push(format!("Run #{seq} {}", describe_run_end(*e)));
        }
    }
    if prior_runs.len() > 10 {
        let start = prior_runs.len() - 10;
        prior_runs = prior_runs.split_off(start);
    }
    let checkpoint_json = serde_json::to_value(&checkpoint).ok()?;
    Some(task_worker::ContinuationContext {
        run_seq,
        previous_end,
        checkpoint: checkpoint_json,
        prior_runs,
    })
}

/// Phase 33: `recent_work_of` が `list_page` から読む候補の上限（裏方タスクを除いた後に
/// `RECENT_WORK_LIMIT` 件へ絞るための余裕）。
const RECENT_WORK_SCAN: usize = 100;
/// Phase 33（ADR-0033 D4 追記）: `context.recent_work` に渡す件数の上限。
const RECENT_WORK_LIMIT: usize = 10;

/// ADR-0048 D3（Phase 60b）: CoS の対話 run に渡す進行中の案件の件数の上限。
const ACTIVE_PROJECTS_SCAN: usize = 200;

/// Phase 41（ADR-0038 D1）: レビューの前置きに載せる、その途中目標の仕事の件数の上限。
const MILESTONE_REVIEW_TASK_LIMIT: usize = 20;
/// Phase 41: 途中目標の仕事を探すときに `list_page` から読む候補の上限。
const MILESTONE_REVIEW_TASK_SCAN: usize = 500;
/// Phase 41（ADR-0038 D1）: 1 件の仕事から載せる成果物の抜粋の字数（決定的に切る）。
const MILESTONE_REVIEW_EXCERPT_CHARS: usize = 4_000;
/// Phase 41（ADR-0038 D1）: 抜粋する成果物の名前（この順に見る）。
const MILESTONE_REVIEW_ARTIFACTS: [&str; 2] = ["answer.md", "report.md"];

/// Phase 33: その run が残した成果物の名前（`ArtifactProduced` から。重複は除く、順は登場順）。
fn artifact_names_of(events: &[(u64, Event)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, e) in events {
        if let Event::ArtifactProduced { artifact, .. } = e
            && !out.contains(&artifact.name)
        {
            out.push(artifact.name.clone());
        }
    }
    out
}

/// Phase 33（ADR-0033 D4 追記）: 終端タスクの短い要約（対話 run が「あなたの直近の仕事」に出す 1 行）。
/// `done` なら直近の `WorkerFinished` の `summary` の 1 行目、`failed` なら直近のレビュー不合格の理由か
/// 直近のワーカーのエラー（どちらが後かはイベント順で決まる）、それも無ければ `Failed` への遷移理由、
/// `blocked` なら直近の質問。Phase 25 の報告の文面の組み立て（`task_core::report`）をそのまま流用する
/// （`first_line` / `truncate_chars`）。LLM は使わない（DESIGN 原則 1）。
fn recent_work_outcome(task: &Task, events: &[(u64, Event)]) -> Option<String> {
    match task.status {
        Status::Done => events.iter().rev().find_map(|(_, e)| match e {
            Event::WorkerFinished {
                outcome,
                role: None,
                ..
            } => outcome
                .strip_prefix("done: ")
                .map(|s| truncate_chars(first_line(s), HEADLINE_MAX_CHARS)),
            _ => None,
        }),
        Status::Failed => events
            .iter()
            .rev()
            .find_map(|(_, e)| match e {
                Event::ReviewVerdict {
                    pass: false,
                    reason,
                    ..
                } => Some(truncate_chars(reason, HEADLINE_MAX_CHARS)),
                Event::WorkerFinished {
                    outcome,
                    role: None,
                    ..
                } => outcome
                    .strip_prefix("error(retryable=")
                    .and_then(|rest| rest.split_once("): "))
                    .map(|(_, message)| truncate_chars(message, HEADLINE_MAX_CHARS)),
                _ => None,
            })
            .or_else(|| {
                events.iter().rev().find_map(|(_, e)| match e {
                    Event::Transitioned {
                        to: Status::Failed,
                        reason,
                        ..
                    } => Some(reason.clone()),
                    _ => None,
                })
            }),
        Status::Blocked => events.iter().rev().find_map(|(_, e)| match e {
            Event::QuestionRaised { text, .. } => Some(truncate_chars(text, HEADLINE_MAX_CHARS)),
            Event::WorkerFinished {
                outcome,
                role: None,
                ..
            } => outcome
                .strip_prefix("question: ")
                .map(|s| truncate_chars(s, HEADLINE_MAX_CHARS)),
            _ => None,
        }),
        _ => None,
    }
}

/// ADR-0016 M5: 子待ちの親について覚えておくもの。
struct AwaitingChildren {
    run_id: String,
    plan: Option<PlanOutput>,
}

/// run 開始時に決める、ワーカーに渡す追加の文脈（ADR-0016 D1 / D3, ADR-0027 D1）。
#[derive(Debug, Default)]
struct RunExtras {
    role: Option<RoleContext>,
    children: Vec<ChildSummary>,
    /// ADR-0027 D1: 委譲できる run（`build_execute_prompt` を使う run）にだけ非空。
    available_genres: Vec<GenreContext>,
    /// ADR-0033 D4: `task.assignee` の組織ノード（担当が無いタスクでは `None`）。
    node: Option<NodeContext>,
    /// ADR-0033 D6: `[memory]` を設定し、担当が決まっている run にだけ載る長期記憶。
    memory: Option<MemoryContext>,
    /// ADR-0033 D4: 担当のノードとのこの案件での直近のやり取り（古い順）。
    conversation: Vec<ConversationTurn>,
    /// ADR-0033 D5（Phase 26）: 担当宛て + 全員向けの永続の認可（`standing_rules`。無ければ空）。
    standing_rules: Vec<String>,
    /// ADR-0033 D4: 分解・委譲できる run に渡す組織図。
    organization: Vec<OrgNodeContext>,
    /// ADR-0033 D4（Phase 28）: 対話用タスクの run だけ `Some`（相手が秘書かそれ以外か）。
    conversation_addressee: Option<ConversationAddressee>,
    /// Phase 30（ADR-0033 D4 追記）: 対話 run で、担当のノードが**自分の仕事の分野**（`node.genre`）を
    /// 持つときだけ `Some`。対話そのものは常に対話用分野で走る（`task.genre`）が、その人が自分の得意分野を
    /// 知って答えられるように、前置きに「仕事で使う道具」として渡す（実機の事故の再発防止:
    /// 検索ハーネスの genre を持つノードに話しかけても、その分野の run にはしない）。
    work_genre: Option<GenreContext>,
    /// Phase 33（ADR-0033 D4 追記。実機の事故の再発防止）: 対話 run にだけ、担当の直近の仕事
    /// （最大 10 件、更新の新しい順。案件を選んでいる対話ならその案件のものを先に）。
    recent_work: Vec<RecentWork>,
    /// Phase 41（ADR-0038 D1）: **途中目標レビューの対話 run** にだけ、その途中目標とそこまでの成果。
    milestone_review: Option<MilestoneReviewContext>,
    /// Phase 43（ADR-0039 D3）: 案件が作業場所を決めている run にだけ、その場所を説明する 1 行。
    workspace_note: Option<String>,
    /// ADR-0044 D2（Phase 53）: そのタスクのコメント（最新 20 件、古い順）。
    comments: Vec<CommentContext>,
    /// ADR-0044 D2: 直前の run を止めた人のコメント（あれば前置きの先頭に「人からの割り込み」として出る）。
    interrupt: Option<String>,
    /// ADR-0046 D1（Phase 59）: 担当ノードの実効 profile ＋ タスクの上書き。profile を 1 つも書いて
    /// いない組織では `None`（前置きは Phase 58 までとバイト単位で同じ）。
    profile: Option<task_core::EffectiveProfile>,
    /// ADR-0046 D4（Phase 59）: 既定（`production`）以外の進め方のときだけ `Some`。
    mode: Option<task_core::TaskMode>,
    /// ADR-0047 D2（Phase 61）: マウントされた知識の索引（本文は入れない）。
    knowledge: Option<task_worker::protocol::KnowledgeContext>,
    /// ADR-0048 D3（Phase 60b）: **CoS の対話 run** にだけ渡す、進行中の案件と途中目標。
    active_projects: Vec<ActiveProjectContext>,
    /// ADR-0059 D6（Phase 99）: **CoS の対話 run** にだけ渡す `[[clusters]]` の一覧
    /// （id・接続状態・実効 work_dir）。
    clusters: Vec<task_worker::ClusterContext>,
    /// ADR-0052 D2（Phase 64）: `langmem` の接続先に届かず、tier `cheap` の汎用ハーネスへ倒した run。
    /// 前置き（`role`）と予算をこの値で上書きする。通常の run では `None`。
    knowledge_fallback: Option<KnowledgeFallbackRun>,
    /// ADR-0054 D1（Phase 67）: 継続セッションの手がかり（CoS の対話・部門長のレビュー run だけ `Some`）。
    session: Option<task_worker::protocol::SessionHandle>,
    /// ADR-0054 D1（Phase 67）: `session.resume = true` のときだけ、前回の run 以降の差分（箇条書き）。
    session_diff: Vec<String>,
    /// ADR-0056 D3（Phase 79）: 担当ノードの実効 profile が継いだ skill mount のうち、KB に実在した
    /// もの（`RunContext.skills` にそのまま乗る）。
    skills: Vec<task_worker::protocol::SkillMount>,
    /// ADR-0056 D3（Phase 79）: mount 名にあったが KB に無かった skill（`run_ready` 相当の呼び出し元が
    /// `status` の進行イベントを 1 行出す。`RunContext` には乗らない）。
    missing_skills: Vec<String>,
}

struct ReviewEntry {
    /// Keep ownership until the verdict transaction has completed, across daemon handoff.
    _review_lock: Arc<std::fs::File>,
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

/// デーモン状態をメモリから公開するための送り口（ADR-0013 D4）。celeris が `[api]` 有効時に `set_snapshot_publisher` で渡す。
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
    /// （確認した事実は設定の書き換えでは古くならない）。celeris を再起動すると消える。
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
    /// ADR-0054 D1（Phase 67）: この run が継続セッションの対象なら `(node_id, kind, project_id)`。
    /// `session_established` / `session_resume_failed` がこれを使って `node_sessions` を書く。
    /// 継続セッションの対象でない run では `None`（両方 no-op）。
    session_key: Option<(String, task_core::SessionKind, Option<ProjectId>)>,
}

impl StoreSink {
    fn note(&self, msg: String) {
        let ev = Event::worker_progress(self.run_id.clone(), msg);
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
            && parent.lease.as_ref().map(|l| l.worker_run_id.as_str())
                == Some(self.run_id.as_str());
        if !ours {
            return Err("task is no longer running under this run".to_string());
        }
        // ADR-0033 D4 / Phase 28: 対話 run は返事だけをする。委譲は受け付けず、理由を `progress` に残す
        // （実機で秘書が返事の代わりに research-survey へ委譲し、対話タスクが `blocked` に落ちた事故の再発防止）。
        if task_core::is_conversation(&parent) {
            return Err("対話では委譲できない。返事に『次にやりたいこと』として書け".to_string());
        }
        // ADR-0033 D4 / D5 / SPEC §3.1: 部をまたぐ連携は秘書が認める。別の部の課を `assignee` にした提案は
        // **子を作らずに**質問（`approvals` の 1 行になる固定の形）を残し、run の終わりに `Question` 終端へ
        // 回す。既に人が答えていれば（`once` / `standing`）その場で通す。判定は組織図と `approvals` /
        // `standing_rules` の前方一致だけを見る決定的なもので、LLM は使わない（DESIGN 原則 1）。
        // Phase 27（監査 H-2）: **バッチは分ける** — 同じ部宛ての提案はその場で子にする。
        let org = self.store.org_list().map_err(|e| format!("store: {e}"))?;
        // ADR-0069 D1（Phase 114）: 委譲（LLM）が書いた担当は使わない。担当は matching が決めるので、
        // 部をまたぐ認可（下の split）も担当を名指しした提案には起きなくなる。捨てた事実は進行に残す。
        let stripped: Vec<DelegateTask> = tasks
            .iter()
            .map(|t| {
                let mut t = t.clone();
                if let Some(a) = t.assignee.take().filter(|a| !a.trim().is_empty()) {
                    self.note(format!(
                        "delegate: 担当の指定 {a} は使わない（「{}」の担当は celeris が skills と harness から決定的に選ぶ。ADR-0069 D1）",
                        t.title
                    ));
                }
                t
            })
            .collect();
        let tasks: &[DelegateTask] = &stripped;
        let split =
            task_ops::conversation::split_delegation(self.store.as_ref(), &org, &parent, tasks)
                .map_err(|e| format!("authorization: {e}"))?;
        for denied in &split.denied {
            self.note(format!(
                "delegate denied: {} は人が認めなかった（子は作っていない）",
                denied.key()
            ));
        }
        for pending in &split.pending {
            self.store
                .append_event(
                    self.task_id,
                    &Event::QuestionRaised {
                        run_id: self.run_id.clone(),
                        text: pending.question(),
                    },
                )
                .map_err(|e| format!("store: {e}"))?;
        }
        if !split.pending.is_empty() {
            self.note(format!(
                "delegate deferred: {} 件は秘書の認可待ち（部をまたぐ委譲。子は作っていない）: {}",
                split.pending.len(),
                split
                    .pending
                    .iter()
                    .map(|c| c.key())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if split.allowed.is_empty() {
            return Ok(());
        }
        let tasks: &[DelegateTask] = &split.allowed;
        let already = self
            .delegated_this_run
            .load(std::sync::atomic::Ordering::SeqCst);
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
        // ADR-0062 B2（Phase 107）: 継承した Remote が担当の道具不足で Local に落ちたことをログに残す。
        for (child_id, reason) in &outcome.workspace_downgrades {
            tracing::info!(task_id = %self.task_id, child_id = %child_id, %reason, "workspace downgraded to local (ADR-0062 B2)");
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
        // Phase 27（監査 H-2）: 「N 件は作った、M 件は秘書の認可待ち」がワーカーの目にも入るようにする。
        let pending = if split.pending.is_empty() {
            String::new()
        } else {
            format!("（{} 件は秘書の認可待ち）", split.pending.len())
        };
        self.note(format!(
            "delegated {n} child task(s){pending}: {}",
            listed.join(", ")
        ));
        tracing::info!(task_id = %self.task_id, run_id = %self.run_id, children = n, "delegated child tasks inserted");
        Ok(())
    }
}

impl EventSink for StoreSink {
    fn progress(&self, msg: &str) {
        let ev = Event::worker_progress(self.run_id.clone(), msg);
        if let Err(e) = self.store.append_event(self.task_id, &ev) {
            tracing::warn!(task_id = %self.task_id, error = %e, "failed to record progress");
        }
    }

    /// ADR-0048 D2（Phase 60a）: 構造化した進行をそのまま `Event::WorkerProgress` に残す
    /// （判断はしない。アダプタが決めた `kind` / `tool` / `summary` / `detail` を写すだけ）。
    fn progress_with(&self, msg: &str, fields: &task_core::ProgressFields) {
        let ev = Event::worker_progress_with(self.run_id.clone(), msg, fields.clone());
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

    /// ADR-0044 D2（Phase 53）: ワーカーの `{"type":"comment"}` は `author_kind = node` で残す。
    /// 人は起こさない（通知は ADR-0037 の 5 種のまま）。状態は変えない。
    fn comment(&self, body: &str) {
        let author = self
            .store
            .get(self.task_id)
            .ok()
            .flatten()
            .and_then(|t| t.assignee.clone());
        match task_ops::comment::post_node_comment(
            self.store.as_ref(),
            self.task_id,
            author,
            Some(self.run_id.clone()),
            body.to_string(),
            OffsetDateTime::now_utc(),
        ) {
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(task_id = %self.task_id, run_id = %self.run_id, error = %e, "failed to record the worker comment")
            }
        }
    }

    fn delegate(&self, tasks: &[DelegateTask]) {
        if let Err(reason) = self.delegate_impl(tasks) {
            tracing::warn!(task_id = %self.task_id, run_id = %self.run_id, %reason, "delegate proposal ignored");
            self.note(format!("delegate ignored: {reason}"));
        }
    }

    /// ADR-0070 D5（Phase 116）: `renew_lease` が DB busy/locked で失敗しても、すぐには諦めない。
    /// この呼び出しの中で最大 [`RENEW_LEASE_RETRIES`] 回（[`RENEW_LEASE_RETRY_DELAY`] 間隔）やり直す。
    /// それでも失敗したら WARN のみ（run はこの呼び出しの成否に関わらず続く。DB が一時的に混んでいた
    /// だけで run を止めない）。
    fn heartbeat(&self) {
        let Ok(mut last) = self.last_renew.lock() else {
            return;
        };
        if last.elapsed() < self.renew_every {
            return;
        }
        *last = Instant::now();
        let mut attempt = 0;
        loop {
            match self
                .store
                .renew_lease(self.task_id, &self.run_id, self.lease_ttl)
            {
                Ok(true) => return,
                Ok(false) => {
                    tracing::debug!(task_id = %self.task_id, run_id = %self.run_id, "lease not renewed (no longer running under this run)");
                    return;
                }
                Err(e) if task_core::is_busy_error(&e) && attempt < RENEW_LEASE_RETRIES => {
                    attempt += 1;
                    tracing::debug!(task_id = %self.task_id, run_id = %self.run_id, attempt, "lease renewal hit a busy database; retrying (ADR-0070 D5)");
                    std::thread::sleep(RENEW_LEASE_RETRY_DELAY);
                }
                Err(e) => {
                    tracing::warn!(task_id = %self.task_id, error = %e, "failed to renew lease");
                    return;
                }
            }
        }
    }

    /// ADR-0024 D4: run の途中でも観測値を `AccountBook` に記録する（`source = "run"`）。プールを使わない run では
    /// `account` が `None` なので no-op。
    fn rate_limit(&self, obs: RateLimitObservation) {
        let Some(account) = &self.account else { return };
        let Some(book) = &self.account_book else {
            return;
        };
        let Ok(mut book) = book.lock() else { return };
        book.record_observation(account, obs, ObservationSource::Run);
        if let Err(e) = book.save() {
            tracing::warn!(task_id = %self.task_id, %account, error = %e, "failed to save account book after rate_limit observation");
        }
    }

    /// ADR-0054 D1（Phase 67）: アダプタが run の途中で確定させた id（codex / acp）を `node_sessions` へ
    /// 書く。claude-code は celeris が前もって決めた id をそのまま報告するだけなので、通常は上書きでも
    /// 値は変わらない。`session_key` が無い run（継続セッションの対象でない）では no-op。
    fn session_established(&self, session_id: &str) {
        let Some((node_id, kind, project_id)) = &self.session_key else {
            return;
        };
        if let Err(e) = self
            .store
            .node_session_set_id(node_id, *kind, *project_id, session_id)
        {
            tracing::warn!(task_id = %self.task_id, error = %e, "failed to record the established session id");
        }
    }

    /// ADR-0054 D1（Phase 67）: resume が拒否されたら、そのセッションを retire する（次の run は新規
    /// セッションになる。ADR-0054 D1「失敗も同じ経路で作り直す」）。`session_key` が無ければ no-op。
    fn session_resume_failed(&self, reason: &str) {
        let Some((node_id, kind, project_id)) = &self.session_key else {
            return;
        };
        match self
            .store
            .node_session_retire(node_id, *kind, *project_id, OffsetDateTime::now_utc())
        {
            Ok(true) => {
                tracing::warn!(task_id = %self.task_id, node_id, %reason, "resume rejected; session retired");
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(task_id = %self.task_id, error = %e, "failed to retire the session after a rejected resume");
            }
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
    /// ADR-0054 D1 / Phase 67b 追記: この Reviewer run が部門長の継続セッション（`kind = lead`）の
    /// 対象なら `(department_id, Lead, None)`。`session_established`/`session_resume_failed` がこれを
    /// 使って `node_sessions` を書く。部署の無い（従来の独立）Reviewer run では `None`（両方 no-op）。
    /// Phase 67 の実装では**この配線が抜けていて**、Lead セッションの resume 拒否が一切 retire
    /// されなかった（ADR-0054 D1「失敗も同じ経路で作り直す」が Lead セッションには効いていなかった）。
    session_key: Option<(String, task_core::SessionKind, Option<ProjectId>)>,
}

impl EventSink for ReviewerSink {
    fn progress(&self, msg: &str) {
        let ev = Event::worker_progress(
            self.subject_run_id.clone(),
            format!("reviewer run {}: {msg}", self.review_run_id),
        );
        if let Err(e) = self.store.append_event(self.task_id, &ev) {
            tracing::warn!(task_id = %self.task_id, error = %e, "failed to record reviewer progress");
        }
    }

    fn artifact(&self, artifact: &ArtifactRef) {
        tracing::debug!(task_id = %self.task_id, review_run_id = %self.review_run_id, name = %artifact.name, "reviewer run artifact ignored");
    }

    fn rate_limit(&self, obs: RateLimitObservation) {
        let Some(account) = &self.account else { return };
        let Some(book) = &self.account_book else {
            return;
        };
        let Ok(mut book) = book.lock() else { return };
        book.record_observation(account, obs, ObservationSource::Run);
        if let Err(e) = book.save() {
            tracing::warn!(task_id = %self.task_id, %account, error = %e, "failed to save account book after reviewer rate_limit observation");
        }
    }

    /// ADR-0054 D1 / Phase 67b 追記: `StoreSink::session_established` と同じ（`node_sessions` の
    /// `session_id` を上書きする）。`session_key` が無ければ no-op。
    fn session_established(&self, session_id: &str) {
        let Some((node_id, kind, project_id)) = &self.session_key else {
            return;
        };
        if let Err(e) = self
            .store
            .node_session_set_id(node_id, *kind, *project_id, session_id)
        {
            tracing::warn!(task_id = %self.task_id, error = %e, "failed to record the established lead session id");
        }
    }

    /// ADR-0054 D1 / Phase 67b 追記: `StoreSink::session_resume_failed` と同じ（resume が拒否されたら
    /// この場で retire し、次の `resolve_node_session` が新しい Lead セッションを作る）。Phase 67 では
    /// この配線が抜けていて、部門長のレビュー run の resume 拒否が retire されずに残り続けた
    /// （本番で ULID の session_id が retire されないまま resume され続けた一因）。`session_key` が
    /// 無ければ no-op。
    fn session_resume_failed(&self, reason: &str) {
        let Some((node_id, kind, project_id)) = &self.session_key else {
            return;
        };
        match self
            .store
            .node_session_retire(node_id, *kind, *project_id, OffsetDateTime::now_utc())
        {
            Ok(true) => {
                tracing::warn!(task_id = %self.task_id, node_id, %reason, "lead session resume rejected; session retired");
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(task_id = %self.task_id, error = %e, "failed to retire the lead session after a rejected resume");
            }
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
    /// ADR-0070 D3（Phase 116）: `Trigger::InfraRequeue` で `Ready` に戻したタスクの再 dispatch を
    /// バックオフさせる（`task_ops::derive::infra_backoff_delay`）。プロセス内メモリのみ（DB に
    /// 永続化しない。dispatcher の再起動で猶予は失われるが、上限判定自体は `consecutive_infra_requeues`
    /// が events から数え直すので安全側。ADR-0070 §2 D3 参照）。
    infra_backoff: HashMap<TaskId, OffsetDateTime>,
    /// 「設定に合うプロバイダが無い」警告を出した（連続 tick で繰り返さない）タスク（ADR-0012 D2）。
    warned_unroutable: std::collections::HashSet<TaskId>,
    /// ADR-0062 B1（Phase 107）: 「担当が cluster:<id> を持たない」警告を出した（連続 tick で
    /// 繰り返さない）タスク。担当・道具が変わって使えるようになれば `task_may_use_cluster` が消す。
    warned_cluster_tool: std::collections::HashSet<TaskId>,
    /// この tick で `NoMatchingProvider` だった ready タスク（`is_idle` で待ち対象から外す。ADR-0012 D2）。
    unroutable: std::collections::HashSet<TaskId>,
    /// ADR-0044 D2（Phase 53）: **この tick で run を打ち切った**タスク。次の tick まで dispatch しない。
    /// 打ち切りは `handle.abort()`（= 子プロセスへの SIGKILL）で、**孫プロセスは即死しない**ので、
    /// 同じ tick で同じ worktree に次の run を入れると 2 つの書き手が重なる（Phase 53 の監査で発見）。
    just_aborted: std::collections::HashSet<TaskId>,
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
    /// ADR-0053 Phase 84b: `refresh_cluster_liveness` が使う「master 生存」判定フック。既定は本物の
    /// `control_master_alive_blocking`（`ssh -O check`）。テストは `set_cluster_liveness_probe` で
    /// 偽物に差し替え、実機の ssh 状態に依存しないようにする。
    cluster_liveness_probe: ClusterLivenessProbe,
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
    /// ADR-0024 D5/D7 / ADR-0025 D5: celeris（GUI の管理 API）が進行中のログイン中継を持っているアカウント
    /// （キーは `"<adapter>:<id>"`。同じ id でもアダプタが違えば別のログインとして扱う）。
    login_pending_accounts: std::collections::HashSet<String>,
    /// ADR-0043 D3（Phase 56）: 起動時に調べたコンテナ runtime（観測値）。celeris が
    /// `detect_container_runtime()` を呼んで埋める。埋まっていなければ「使えない」と同じ扱いで、
    /// コンテナが要るタスクは `blocked` になる。
    container_probe: task_worker::RuntimeProbe,
    /// ADR-0032 D3: `auth = "publickey"` のクラスタに自動で接続を張るフック。`None` なら自動接続しない
    /// （celeris 側が `set_cluster_connector` で挿す。未設定＝従来どおりの挙動）。
    cluster_connector: Option<ClusterConnector>,
    /// ADR-0062 A（Phase 107）: master 越しの実通信で生存を確定させるフック（`ssh -o BatchMode=yes
    /// <host> -- true`）。`None` なら実通信の probe はしない（`-O check` だけの従来どおり）。
    cluster_command_probe: Option<ClusterCommandProbe>,
    /// ADR-0062 A: クラスタごとに最後に実通信 probe を行った時刻（`liveness_probe_secs` の間引きに使う）。
    last_cluster_command_probe: HashMap<String, Instant>,
    /// ADR-0062 A: 実通信 probe が失敗した／master が自分で終了したときに `-O exit` で残骸を片付ける
    /// フック（引数は `(cluster_id, host)`）。`None` なら片付けを試みない。
    cluster_disconnector: Option<ClusterDisconnector>,
    /// ADR-0062 A: 明示的な切断を経ずに接続が失われたクラスタの詳細（exit code・stderr の末尾）。
    /// `mark_cluster_unavailable` が拾って報告に足し、`Event::ClusterMasterExited` を 1 回だけ残す
    /// （`reported`）。接続が戻れば消す。
    cluster_disconnect_info: HashMap<String, ClusterDisconnectInfo>,
    /// ADR-0062 A: celeris が保持している master（`Child`）の終了を検出するフック。`None` なら見ない
    /// （celeris 側が `set_cluster_master_watcher` で挿す）。
    cluster_master_watcher: Option<ClusterMasterWatcher>,
    /// ADR-0032 D4/D5: GUI 発の接続（`POST /clusters/{id}/connect`）が進行中のクラスタ id
    /// （celeris が `set_cluster_connect_pending` で反映する。D3 の自動接続とは別物）。
    connect_pending_clusters: std::collections::HashSet<String>,
    /// 壁時計の Unix 秒（テストで差し替えられるようにした関数。既定は実時刻）。
    now_unix_fn: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// ADR-0040 D4（Phase 47）: 新しい仕事を始めてよいか。`false`（= draining）のときは
    /// `dispatch_ready` も `recover_reviews` も動かさず、**手元の run とレビューの面倒だけ見続ける**
    /// （完了の記録、リースの更新、`aggregate` / `child_failed` の後処理は通常どおり動く）。
    accepting_new_work: bool,
    /// ADR-0041 D1 / ADR-0043 D2: この celeris が用意したローカルの作業場所（1 つ以上のリポジトリ）。
    /// **終端では消さない**（ADR-0043 D2 の改定。差分を見るために残す）。消すのは**中止**（cancel）
    /// のときだけで、worktree とブランチを消す。再起動では失われる（残った worktree は人が
    /// `git worktree remove` する。PROGRESS の未解決）。
    task_workspaces: HashMap<TaskId, task_worker::TaskWorkspaces>,
    /// ADR-0041 D5: 面倒を見てよいタスクの述語（`None` なら全部）。`--mode verify` の煙試験でだけ使う。
    eligible: Option<TaskFilter>,
    /// ADR-0052 D1（Phase 64）: 知識整理タスクを dispatch する直前に当てる到達性の検査
    /// （既定は `task_worker::probe_models`。テストは `set_knowledge_probe` で差し替える）。
    /// **LLM は呼ばない**（`GET <base_url>/models` の 1 回だけ）。
    knowledge_probe: KnowledgeProbe,
    /// ADR-0052 D1: 検査の結果のキャッシュ（`base_url` → (いつ調べたか, 結果)）。60 秒。
    knowledge_probe_cache: HashMap<String, (Instant, Reachability)>,
    /// ADR-0053 D3（Phase 66）: `[[clusters]].forwards` を(再)確立するフック。`None` なら何もしない。
    tunnel_forward_ensurer: Option<TunnelForwardEnsurer>,
    /// ADR-0053 D3 / Phase 85: forward の target（先方）の健康を見るフック。`None` なら常に「不健全」扱い。
    tunnel_probe: Option<TunnelProbe>,
    /// ADR-0053 Phase 85: forward のリスナーの有無を見るフック。`None` なら常に「無い」扱い
    /// （安全側のデフォルト。`tunnel_forward_ensurer` に(再)確立を試みさせる。Phase 66 までと同じ保守的な
    /// 挙動）。
    tunnel_listener_probe: Option<TunnelListenerProbe>,
    /// ADR-0053 D3 / Phase 85: forward ごとの直近の観測（キーは `tunnel_key`）。listener と target の
    /// 健康を別々に持つ（`ForwardObservation`）。
    tunnel_state: HashMap<String, ForwardObservation>,
    /// ADR-0066 D3（Phase 110b）: forward ごとの target probe の直近の結果（キーは `tunnel_key`）。
    /// **専用スレッド（`ensure_tunnel_prober_started`）が書き、`refresh_one_forward` は読むだけ**
    /// （tick の同期経路から HTTP probe を外すため）。listener が初めて有りになった forward は、
    /// ここにまだ記録が無い間だけ `refresh_one_forward` が 1 回だけ同期に probe して種を蒔く。
    tunnel_probe_state: Arc<std::sync::Mutex<HashMap<String, TargetProbeState>>>,
    /// ADR-0066 D3: 専用スレッドを起こしたら `Some`（二重に起こさない）。`Drop` でスレッドに停止を伝える。
    tunnel_prober_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// ADR-0053 D3: 「TOTP ログインが要る」と判定済みのクラスタ id（重複通知を防ぐ。master が戻れば消す）。
    cluster_login_needed: std::collections::HashSet<String>,
    /// ADR-0053 D3: 直近のトンネル状態遷移（Console / cluster API 向け。`take_tunnel_events` で取り出す）。
    tunnel_events: std::collections::VecDeque<TunnelEvent>,
    /// ADR-0053 D3: 最後にトンネルの生存を見た時刻（`refresh_cluster_liveness` と同じ間隔で間引く）。
    last_cluster_tunnel_refresh: Option<Instant>,
}

impl Drop for Dispatcher {
    /// ADR-0066 D3: 専用の target-probe スレッド（起こしていれば）に停止を伝える。tick を止めない設計と
    /// 同じ理由で、ここでも join はしない（スレッドは次の周回〈`TUNNEL_PROBER_STEP`〉で自分から終わる）。
    fn drop(&mut self) {
        if let Some(stop) = &self.tunnel_prober_stop {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// ADR-0066 D3: 専用スレッドが期限をチェックする周期。`probe_interval_secs`（最短でも 1 秒）よりずっと
/// 短くして、期限が来たらすぐ probe する。
const TUNNEL_PROBER_STEP: Duration = Duration::from_millis(200);

/// ADR-0066 D3: forward ごとの target probe を、tick とは無縁に裏で行い続けるループ
/// （`ensure_tunnel_prober_started` が専用スレッドで起こす）。`stop` が立ったら抜ける。
fn tunnel_prober_loop(
    probe: TunnelProbe,
    forwards: Vec<(String, String, String, u64)>,
    state: Arc<std::sync::Mutex<HashMap<String, TargetProbeState>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let now = Instant::now();
        for (cluster, listen, _target, min_secs) in &forwards {
            let key = tunnel_key(cluster, listen);
            let due = {
                let Ok(guard) = state.lock() else { continue };
                guard
                    .get(&key)
                    .map(|s| {
                        now.duration_since(s.checked_at) >= Duration::from_secs(s.interval_secs)
                    })
                    .unwrap_or(true)
            };
            if !due {
                continue;
            }
            let result = probe(listen);
            let healthy = result.is_ok();
            let Ok(mut guard) = state.lock() else {
                continue;
            };
            let prev_interval = guard
                .get(&key)
                .map(|s| s.interval_secs)
                .unwrap_or(*min_secs);
            guard.insert(
                key,
                TargetProbeState {
                    healthy,
                    error: result.err(),
                    checked_at: Instant::now(),
                    interval_secs: next_probe_interval_secs(prev_interval, *min_secs, healthy),
                },
            );
        }
        std::thread::sleep(TUNNEL_PROBER_STEP);
    }
}

/// ADR-0052 D1（Phase 65b で `bearer_token` を追加）: 到達性の検査のフック（差し替えられるようにして
/// ある。既定は本物の HTTP GET）。第 2 引数は `[knowledge.langmem].api_key_secret` から解決した
/// 平文のトークン（`llm-proxy` のように `/v1/models` が認証を要求する上流のため。値はログに出さない）。
pub type KnowledgeProbe = Arc<dyn Fn(&str, Option<&str>) -> Reachability + Send + Sync>;

/// ADR-0052 D2: フォールバックする run に載せる上書き（`run_extras` の結果に混ぜる）。
#[derive(Debug, Clone)]
struct KnowledgeFallbackRun {
    /// 倒した先のアダプタ（`WorkerStarted.adapter` と同じ。ログと進行イベントに出す）。
    adapter: String,
    /// 前置き（`task_worker::knowledge_fallback_instructions`）。
    instructions: String,
    /// ADR-0052 D2 の予算（`max_turns = 8` / `max_wall_secs = 600`）。
    budget: task_core::Budget,
}

/// ADR-0052 D2: フォールバック run の予算。
const KNOWLEDGE_FALLBACK_MAX_TURNS: u32 = 8;
const KNOWLEDGE_FALLBACK_MAX_WALL_SECS: u64 = 600;
/// ADR-0052 D2: フォールバック run が書く候補ファイル（ワーカーから見た位置）。
const KNOWLEDGE_CANDIDATES_REL: &str = "artifacts/knowledge-candidates.json";

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
        // ADR-0024 D4 / ADR-0025 D1: `<root>/.celeris-usage.json` から観測値・cooldown を読む（無ければ空から始める）。
        // アダプタごとに別の根ディレクトリ・別の帳簿（アカウントの記録はそのアダプタの中で閉じる）。
        let account_books: HashMap<AccountAdapter, Arc<StdMutex<AccountBook>>> =
            match &config.accounts {
                Some(accounts) => accounts
                    .roots
                    .iter()
                    .map(|(adapter, root)| {
                        (
                            *adapter,
                            Arc::new(StdMutex::new(AccountBook::load(
                                &root.join(".celeris-usage.json"),
                            ))),
                        )
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
            infra_backoff: HashMap::new(),
            warned_unroutable: std::collections::HashSet::new(),
            warned_cluster_tool: std::collections::HashSet::new(),
            just_aborted: std::collections::HashSet::new(),
            unroutable: std::collections::HashSet::new(),
            cluster_waiting: std::collections::HashSet::new(),
            awaiting_human: std::collections::HashSet::new(),
            awaiting_children: HashMap::new(),
            tx,
            rx,
            cluster_cooldown: HashMap::new(),
            cluster_connected: HashMap::new(),
            last_cluster_liveness: None,
            cluster_liveness_probe: Arc::new(|ssh_command: &[String], host: &str| {
                control_master_alive_blocking(ssh_command, host)
            }),
            ticks: 0,
            publisher: None,
            account_pool_providers,
            account_books,
            accounts_scan_cache: HashMap::new(),
            login_pending_accounts: std::collections::HashSet::new(),
            container_probe: task_worker::RuntimeProbe::default(),
            cluster_connector: None,
            cluster_command_probe: None,
            last_cluster_command_probe: HashMap::new(),
            cluster_disconnector: None,
            cluster_disconnect_info: HashMap::new(),
            cluster_master_watcher: None,
            connect_pending_clusters: std::collections::HashSet::new(),
            now_unix_fn: Arc::new(real_now_unix),
            accepting_new_work: true,
            task_workspaces: HashMap::new(),
            eligible: None,
            knowledge_probe: Arc::new(|base_url, bearer_token| {
                task_worker::probe_models(base_url, task_worker::PROBE_TIMEOUT, bearer_token)
            }),
            knowledge_probe_cache: HashMap::new(),
            tunnel_forward_ensurer: None,
            tunnel_probe: None,
            tunnel_listener_probe: None,
            tunnel_state: HashMap::new(),
            tunnel_probe_state: Arc::new(std::sync::Mutex::new(HashMap::new())),
            tunnel_prober_stop: None,
            cluster_login_needed: std::collections::HashSet::new(),
            tunnel_events: std::collections::VecDeque::new(),
            last_cluster_tunnel_refresh: None,
        }
    }

    /// ADR-0052 D1: 到達性の検査を差し替える（テストは偽のローカルサーバも起こさずに済ませる）。
    pub fn set_knowledge_probe(&mut self, probe: KnowledgeProbe) {
        self.knowledge_probe = probe;
        self.knowledge_probe_cache.clear();
    }

    /// ADR-0052 D1: `[knowledge.langmem].base_url` の到達性（60 秒キャッシュ）。
    /// `base_url` が無ければ「検査できない」= [`Reachability::Unknown`]。
    fn knowledge_reachability(&mut self, now: Instant) -> Reachability {
        let Some(base_url) = self.config.knowledge.langmem_base_url.clone() else {
            return Reachability::Unknown {
                reason: "[knowledge.langmem].base_url が無い".to_string(),
            };
        };
        if let Some((checked_at, cached)) = self.knowledge_probe_cache.get(&base_url)
            && now.duration_since(*checked_at) < task_worker::PROBE_CACHE_TTL
        {
            return cached.clone();
        }
        let token = self.config.knowledge.langmem_api_key.clone();
        let started = Instant::now();
        let outcome = (self.knowledge_probe)(&base_url, token.as_deref());
        log_slow_step("knowledge_probe", started);
        tracing::debug!(%base_url, ?outcome, "knowledge: probed the langmem endpoint");
        self.knowledge_probe_cache
            .insert(base_url, (now, outcome.clone()));
        outcome
    }

    /// ADR-0043 D3（Phase 56）: 起動時にコンテナ runtime を調べる（`podman info` → `docker info`）。
    /// 結果はログと `GET /daemon` に出る。**呼ばなければコンテナが要るタスクは `blocked`** になる
    /// （テストは `set_container_probe` で差し替える。`cargo test` は runtime を起こさない）。
    pub fn detect_container_runtime(&mut self) {
        let probe = task_worker::container::detect(
            self.config.containers.preference,
            CONTAINER_PROBE_TIMEOUT,
        );
        match probe.runtime {
            Some(rt) => tracing::info!(
                runtime = rt.as_str(),
                preference = %probe.preference,
                image_default = %self.config.containers.image_default,
                "container runtime detected"
            ),
            None => tracing::warn!(
                preference = %probe.preference,
                detail = %probe.summary(),
                "no container runtime; tasks that need one will be blocked"
            ),
        }
        self.container_probe = probe;
    }

    /// 調べた結果を差し替える（celeris の起動経路とテスト用）。
    pub fn set_container_probe(&mut self, probe: task_worker::RuntimeProbe) {
        self.container_probe = probe;
    }

    /// 起動時に調べたコンテナ runtime（スナップショット用）。
    pub fn container_probe(&self) -> &task_worker::RuntimeProbe {
        &self.container_probe
    }

    /// ADR-0040 D4（Phase 47）: 新しい仕事を始めるのをやめる／再開する。`false` にすると
    /// `dispatch_ready`（ready なタスクの起動）と `recover_reviews`（他のインスタンスが抱えている
    /// かもしれない reviewing の拾い上げ）を止める。**手元の run とレビューはそのまま面倒を見る**。
    pub fn set_accepting_new_work(&mut self, accepting: bool) {
        self.accepting_new_work = accepting;
    }

    /// ADR-0041 D5: 面倒を見てよいタスクを絞る（`--mode verify` の煙試験）。呼ばなければ従来どおり全部。
    /// 絞られたタスクは **ready のまま放置**され、リースの回収もレビューの拾い上げも起きない。
    pub fn set_eligible_tasks(&mut self, eligible: TaskFilter) {
        self.eligible = Some(eligible);
    }

    /// そのタスクをこのインスタンスが触ってよいか（述語が無ければ常に真）。
    fn is_eligible(&self, task: &Task) -> bool {
        self.eligible.as_ref().is_none_or(|f| f(task))
    }

    /// 手元で動いている run とレビューの数（ADR-0040 D4 の drain の判定に使う）。
    pub fn in_flight(&self) -> usize {
        self.running.len() + self.reviewing.len()
    }

    /// ADR-0040 D4: `[handoff] drain_timeout_secs` を超えたときに、残っている run とレビューを
    /// 打ち切る。DB の状態は変えない（リースが切れて新しい active が従来の「リース切れ」の経路で拾う）。
    /// 打ち切った数を返す。
    pub fn abort_all_runs(&mut self) -> usize {
        // ADR-0044 §5 Phase 53 追記（Phase 55）: drain も他の 4 つと同じ止め方
        // （プロセスグループへ SIGTERM → `kill_grace_secs` → SIGKILL）。
        let kill_grace = self.config.kill_grace;
        let mut aborted = 0;
        for (task_id, entry) in self.running.drain() {
            tracing::warn!(task_id = %task_id, run_id = %entry.run_id, "drain timeout; aborting the run (the lease will expire and the new active will reclaim it)");
            // Phase 55/56 の合流: コンテナで走っている run はラベル越しにも止める（P55-4 / P56-7）。
            task_worker::kill_tree_with(&entry.run_id, kill_grace, entry.container);
            entry.handle.abort();
            aborted += 1;
        }
        for (task_id, entry) in self.reviewing.drain() {
            tracing::warn!(task_id = %task_id, "drain timeout; aborting the review");
            task_worker::kill_tree(&entry.run_id, kill_grace);
            if let Some(review_run_id) = &entry.review_run_id {
                task_worker::kill_tree(review_run_id, kill_grace);
            }
            entry.handle.abort();
            aborted += 1;
        }
        self.pending_subjects.clear();
        aborted
    }

    /// ADR-0032 D3: `auth = "publickey"` のクラスタへの自動接続を有効にする（celeris 側が本番の実装を挿す）。
    /// 呼ばなければ従来どおり自動接続しない。
    pub fn set_cluster_connector(&mut self, connector: ClusterConnector) {
        self.cluster_connector = Some(connector);
    }

    /// ADR-0032 D4/D5: GUI 発の接続が進行中かを記録する（celeris の管理 API が呼ぶ。呼び出しは celeris 側の配線）。
    pub fn set_cluster_connect_pending(&mut self, id: &str, pending: bool) {
        if pending {
            self.connect_pending_clusters.insert(id.to_string());
        } else {
            self.connect_pending_clusters.remove(id);
        }
    }

    /// ADR-0053 D3（Phase 66）: `[[clusters]].forwards` を(再)確立するフックを挿す。呼ばなければ
    /// `refresh_cluster_tunnels` は forward を張り直さない（届かないまま観測するだけ）。
    pub fn set_tunnel_forward_ensurer(&mut self, ensurer: TunnelForwardEnsurer) {
        self.tunnel_forward_ensurer = Some(ensurer);
    }

    /// ADR-0053 D3 / Phase 85: forward の target（先方）の健康を見るフックを挿す。呼ばなければ常に
    /// 「不健全」扱い。
    pub fn set_tunnel_probe(&mut self, probe: TunnelProbe) {
        self.tunnel_probe = Some(probe);
    }

    /// ADR-0053 Phase 85: forward のリスナー（`-O forward` が実際に手元で待ち受けているか）を見るフックを
    /// 挿す。呼ばなければ常に「無い」扱い（安全側のデフォルト）。
    pub fn set_tunnel_listener_probe(&mut self, probe: TunnelListenerProbe) {
        self.tunnel_listener_probe = Some(probe);
    }

    /// ADR-0053 Phase 84b: クラスタの ssh master の多重接続の有無を調べるフックを差し替える。
    /// 呼ばなければ本物の `ssh -O check`（`control_master_alive_blocking`）のまま。テストは実機の ssh
    /// 状態に依存しないよう、必ずこれで偽物に差し替える。
    pub fn set_cluster_liveness_probe(&mut self, probe: ClusterLivenessProbe) {
        self.cluster_liveness_probe = probe;
    }

    /// ADR-0062 A（Phase 107）: master 越しの実通信 probe を挿す（celeris 側の配線）。
    pub fn set_cluster_command_probe(&mut self, probe: ClusterCommandProbe) {
        self.cluster_command_probe = Some(probe);
    }

    /// ADR-0062 A: 死んだ接続を `-O exit` で片付けるフックを挿す（celeris 側の配線）。
    pub fn set_cluster_disconnector(&mut self, disconnector: ClusterDisconnector) {
        self.cluster_disconnector = Some(disconnector);
    }

    /// ADR-0062 A: celeris が保持している master の終了検出フックを挿す（celeris 側の配線）。
    pub fn set_cluster_master_watcher(&mut self, watcher: ClusterMasterWatcher) {
        self.cluster_master_watcher = Some(watcher);
    }

    /// ADR-0053 D3: 直近のトンネル状態遷移を取り出す（呼ぶと空になる。celeris はこれを Discord/Console に流す）。
    pub fn take_tunnel_events(&mut self) -> Vec<TunnelEvent> {
        self.tunnel_events.drain(..).collect()
    }

    /// ADR-0053 D3: 現在「TOTP ログインが要る」状態のクラスタ id（昇順）。
    pub fn clusters_needing_login(&self) -> Vec<String> {
        let mut v: Vec<String> = self.cluster_login_needed.iter().cloned().collect();
        v.sort();
        v
    }

    /// ADR-0053 D3: forward が今届いているか（listener も target も健全。無ければ観測が無い＝`false`）。
    pub fn tunnel_reachable(&self, cluster: &str, listen: &str) -> bool {
        self.tunnel_state
            .get(&tunnel_key(cluster, listen))
            .map(ForwardObservation::up)
            .unwrap_or(false)
    }

    /// ADR-0053 Phase 85: forward の**リスナー**が今有るか（`-O forward` が届いているか。target の健康とは
    /// 別。無ければ観測が無い＝`false`）。
    pub fn tunnel_listener_present(&self, cluster: &str, listen: &str) -> bool {
        self.tunnel_state
            .get(&tunnel_key(cluster, listen))
            .map(|o| o.listener)
            .unwrap_or(false)
    }

    /// ADR-0053 Phase 85: forward の**target**が今健全か（`/v1/models` が応答するか。listener の有無とは
    /// 別。無ければ観測が無い＝`false`）。
    pub fn tunnel_target_healthy(&self, cluster: &str, listen: &str) -> bool {
        self.tunnel_state
            .get(&tunnel_key(cluster, listen))
            .map(|o| o.target_healthy)
            .unwrap_or(false)
    }

    /// ADR-0053 Phase 85: forward の直近の失敗理由（無ければ `None`）。
    pub fn tunnel_last_error(&self, cluster: &str, listen: &str) -> Option<String> {
        self.tunnel_state
            .get(&tunnel_key(cluster, listen))
            .and_then(|o| o.last_error.clone())
    }

    pub fn set_delivery_policy(&mut self, policy: task_ops::delivery::DeliveryPolicy) {
        self.config.delivery = policy;
    }

    pub fn config(&self) -> &DispatchConfig {
        &self.config
    }

    /// ADR-0033 D3: tick ループ（celeris）が報告の圧縮のためにストアを読む。ディスパッチャ自身の
    /// 判断には使わない（ここから LLM を呼ぶこともない）。
    pub fn store(&self) -> Arc<dyn TaskStore> {
        Arc::clone(&self.store)
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

    /// Phase 44（実機 2026-09-18）: `POST /reload` で `[[roles]]` / `[[genres]]` / `[delegation]` も読み直す。
    /// `reload_providers` とは別トランザクション（呼び出し側が両方呼ぶ）。実行中の run はそれぞれ `spawn_worker`
    /// 時点でこれらの値の写しを既に掴んでいるので、反映されるのは**次に起動する run から**
    /// （委譲で作られる子の budget を含む）。
    pub fn reload_config(
        &mut self,
        roles: Vec<RoleSpec>,
        genres: Vec<GenreSpec>,
        delegation: DelegationLimits,
    ) {
        self.config.roles = roles;
        self.config.genres = genres;
        self.config.delegation = delegation;
    }

    // ---- ADR-0024/0025: celeris（GUI の管理 API）が使うアカウント操作 ----

    /// そのアダプタ・アカウントで走っている run（ワーカー run + Reviewer run）の数。
    pub fn account_in_use(&self, adapter: AccountAdapter, id: &str) -> usize {
        let matches = |a: &Option<AccountAdapter>, acct: &Option<String>| {
            *a == Some(adapter) && acct.as_deref() == Some(id)
        };
        self.running
            .values()
            .filter(|e| matches(&e.account_adapter, &e.account))
            .count()
            + self
                .reviewing
                .values()
                .filter(|e| matches(&e.account_adapter, &e.account))
                .count()
    }

    /// ADR-0025 D1: `login_pending_accounts` のキー（同じ id でもアダプタが違えば別のログインとして扱う）。
    fn login_pending_key(adapter: AccountAdapter, id: &str) -> String {
        format!("{adapter}:{id}")
    }

    /// D7: 進行中のログイン中継の有無を記録する（celeris の `HashMap<String, LoginSession>` と対）。
    pub fn set_account_login_pending(&mut self, adapter: AccountAdapter, id: &str, pending: bool) {
        let key = Self::login_pending_key(adapter, id);
        if pending {
            self.login_pending_accounts.insert(key);
        } else {
            self.login_pending_accounts.remove(&key);
        }
    }

    /// ログイン中継の実行中は定期確認しない。
    pub fn account_login_pending(&self, adapter: AccountAdapter, id: &str) -> bool {
        self.login_pending_accounts
            .contains(&Self::login_pending_key(adapter, id))
    }

    /// ADR-0053 D1（Phase 65）: `llm-proxy` が同じアカウントプールの cooldown・観測値を共有するための
    /// アクセサでもある（CLI ワーカーの dispatch と**同じ帳簿**を返す。別の写しを作らない）。
    /// そのアダプタの `[accounts]` 根が設定されていなければ `None`。
    pub fn account_book(&self, adapter: AccountAdapter) -> Option<Arc<StdMutex<AccountBook>>> {
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
        let Some(book) = self.account_book(adapter) else {
            return;
        };
        let Ok(mut book) = book.lock() else { return };
        let has_observation = observation.is_some();
        if let Some(obs) = observation {
            book.record_observation(id, obs, ObservationSource::Check);
        }
        match result {
            "ok" => {
                book.clear_cooldown(id);
                if adapter == AccountAdapter::Codex && !has_observation {
                    book.clear_observation(id);
                }
            }
            "auth_failed" | "throttled" => {
                let reason = if result == "auth_failed" {
                    AccountCooldownReason::AuthFailed
                } else {
                    AccountCooldownReason::Throttled
                };
                let cooldown = cooldown_for_failure(
                    book.state(id),
                    reason,
                    now,
                    self.config
                        .accounts
                        .as_ref()
                        .map_or(300, |c| c.fallback_cooldown_secs),
                );
                book.set_cooldown(id, cooldown, now);
            }
            _ => {} // 通信失敗だけでは前回の観測を消さない。
        }
        book.record_check(
            id,
            AccountCheckRecord {
                at: now,
                result: result.to_string(),
                detail,
            },
        );
        if let Err(e) = book.save() {
            tracing::warn!(account_id = %id, %adapter, error = %e, "failed to save account book after check");
        }
    }

    /// D5 `DELETE /accounts/{id}`: 帳簿からもこのアカウントの記録を消す（ディレクトリの移動は celeris/task-api が行う）。
    pub fn remove_account_book_entry(&mut self, adapter: AccountAdapter, id: &str) {
        let Some(book) = self.account_book(adapter) else {
            return;
        };
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
            let ids: std::collections::HashSet<&str> =
                providers.iter().map(|p| p.id.as_str()).collect();
            publisher
                .provider_checks
                .retain(|id, _| ids.contains(id.as_str()));
            publisher.providers = providers;
        }
    }

    /// ADR-0022 D2: 疎通確認の結果をスナップショットに載せる（DB には書かない）。次の tick から `GET /providers` に出る。
    pub fn set_provider_check(&mut self, provider_id: &str, check: ProviderCheckView) {
        if let Some(publisher) = self.publisher.as_mut() {
            publisher
                .provider_checks
                .insert(provider_id.to_string(), check);
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
        // ADR-0043 D2: **中止**されたタスクの worktree とブランチを消す（終端〈done / failed〉では消さない）。
        self.cleanup_cancelled_worktrees()?;
        let abort_ms = lap(&mut at);
        // ADR-0066 D2（Phase 110b）: 終端になってから `prune_after_secs` 経った作業場所から、ビルド
        // 生成物だけを刈る（1 tick に最大 1 か所。探すところまでは軽いので同期、削除は別スレッド）。
        self.prune_one_workspace();
        let prune_ms = lap(&mut at);
        // ADR-0040 D4: draining のインスタンスは新しい仕事を始めない（拾い上げも dispatch もしない）。
        // 手元の run とレビューの完了・リース更新・後処理は上の `drain_completions` 以下でそのまま動く。
        if self.accepting_new_work {
            self.recover_reviews()?;
        }
        let recover_ms = lap(&mut at);
        // Phase 66b: `ssh` を呼ぶ・ネストしたランタイムを `block_on` しうるので、async ワーカーから逃がす
        // （`run_cluster_hooks_off_async` の説明を参照）。
        run_cluster_hooks_off_async(|| self.refresh_cluster_liveness());
        // ADR-0062 A: `try_wait` は非ブロッキングなので、他のフックと違いスレッドを逃がす必要は無い。
        self.refresh_cluster_master_exits();
        let cluster_ms = lap(&mut at);
        run_cluster_hooks_off_async(|| self.refresh_cluster_tunnels());
        let tunnel_ms = lap(&mut at);
        report.dispatched = if self.accepting_new_work {
            self.dispatch_ready()?
        } else {
            0
        };
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
                prune_ms,
                recover_ms,
                cluster_ms,
                tunnel_ms,
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
        let mut reason = reason;
        // ADR-0062 A（Phase 107）: 明示的な切断を経ずに master が終了していた／実通信 probe が
        // 失敗していたなら、その詳細（exit code・stderr の末尾）を報告に足し、`Event::ClusterMasterExited`
        // を 1 回だけ残す（同じ切断について 2 回目以降は reason に足すだけ。cooldown が明けて接続が
        // 戻れば `refresh_cluster_liveness` がこのエントリを消す）。
        if let Some(info) = self.cluster_disconnect_info.get(&spec.id).cloned() {
            let exit_str = info
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "unknown (signal?)".to_string());
            reason = format!(
                "{reason}\nssh master exited (exit_code={exit_str}): {}",
                info.detail
            );
            if !info.reported {
                self.store.append_event(
                    task_id,
                    &Event::ClusterMasterExited {
                        cluster: spec.id.clone(),
                        exit_code: info.exit_code,
                        stderr_tail: info.detail.clone(),
                    },
                )?;
                if let Some(entry) = self.cluster_disconnect_info.get_mut(&spec.id) {
                    entry.reported = true;
                }
            }
        }
        // ADR-0062 A: TOTP のクラスタは人の入力が要るので、その場での自動復旧を待たせない。
        if spec.auth == "totp" {
            reason =
                format!("{reason}\nGUI の「クラスタ」画面から TOTP を入力して再接続してください。");
        }
        let first = self
            .cluster_cooldown
            .insert(
                spec.id.clone(),
                Instant::now() + self.config.cluster_cooldown,
            )
            .is_none();
        if first {
            tracing::warn!(
                cluster = %spec.id, host = %spec.host, %reason,
                "no ssh ControlMaster connection; run `scripts/cluster-login.sh {}` to log in again", spec.host
            );
        }
        self.store.append_event(
            task_id,
            &Event::ClusterUnavailable {
                cluster: spec.id.clone(),
                host: spec.host.clone(),
                reason: reason.clone(),
            },
        )?;
        // ADR-0033 D3: クラスタが落ちたことは `infra` 相当のノードの悪い知らせとして人まで上げる
        // （案件に紐づかない）。同じホストの障害を毎 tick 繰り返さないよう、cooldown の間は 1 件だけにする。
        let now = OffsetDateTime::now_utc();
        let cooldown_secs =
            i64::try_from(self.config.cluster_cooldown.as_secs()).unwrap_or(i64::MAX);
        match crate::reports::cluster_report_recently_recorded(
            self.store.as_ref(),
            &spec.host,
            now,
            cooldown_secs,
        ) {
            Ok(true) => {}
            Ok(false) => {
                if let Err(e) = crate::reports::record_cluster_unavailable_report(
                    self.store.as_ref(),
                    &spec.id,
                    &spec.host,
                    &reason,
                    Some(task_id),
                    now,
                ) {
                    tracing::warn!(cluster = %spec.id, error = %e, "failed to record the cluster report");
                }
            }
            Err(e) => {
                tracing::warn!(cluster = %spec.id, error = %e, "failed to read the recent cluster reports")
            }
        }
        Ok(())
    }

    /// ADR-0032 D3: `auth = "publickey"` のクラスタに接続フックが刺さっていれば 1 回だけ接続を試みる。
    /// フックが無い、または `auth` が `"publickey"` でなければ `None`（＝試みなかった。呼び出し側は従来どおり
    /// cooldown に落とす）。試みた場合は結果（`Ok(())` = 成功、`Err(detail)` = 失敗の理由）を返す。
    ///
    /// ADR-0053 Phase 66b の「未解決事項」/ Phase 81: `dispatch_ready`（この関数の呼び出し元）は
    /// celeris の tick ループの中から**同期のまま**呼ばれ、そのループ自身がマルチスレッドの tokio
    /// ランタイムの上で動く async タスクなので、`cluster_connector`（ネストしたランタイムを
    /// `block_on` しうる）をここでインラインに呼ぶと `refresh_cluster_tunnels` と同じ形で panic
    /// しうる（本番はまだ踏んでいないが、理論上の危険性は Phase 66b で指摘済み）。
    /// `refresh_cluster_liveness`/`refresh_cluster_tunnels` と同じ `run_cluster_hooks_off_async`
    /// （本物の OS スレッドへ逃がし、マルチスレッド・ランタイムの上でだけ `block_in_place` で包む）に
    /// 通すことで、`cluster_connector` 側の防御的ガード（`Handle::try_current().is_ok()` なら
    /// エラーを返す）に頼らずに済むようにした。
    fn try_auto_connect_cluster(&self, spec: &ClusterSpec) -> Option<Result<(), String>> {
        if spec.auth != "publickey" {
            return None;
        }
        let connector = self.cluster_connector.as_ref()?.clone();
        let id = spec.id.clone();
        let host = spec.host.clone();
        Some(run_cluster_hooks_off_async(move || connector(&id, &host)))
    }

    /// ADR-0018 D2: 設定の全クラスタについて、多重接続の有無を 1 tick に 1 回調べる（`ssh -O check` は unix ソケットを
    /// 見るだけで即座に返る。ネットワークにも認証にも触れない）。結果は dispatch の判断とスナップショットの `connected` に使う。
    /// 接続が戻っていれば cooldown を解く（人がログインし直したら、次の tick から再開できるように）。
    fn refresh_cluster_liveness(&mut self) {
        // ADR-0041 D5 / Phase 66c: `--mode verify` は「migrations、API と `smoke` の煙試験だけ。他の
        // dispatch も裏方の仕事も無い」（`self.eligible.is_some()` が `--mode verify` の煙試験だけを
        // 目印。ADR-0041 D5、`set_eligible_tasks` の doc を参照）。`refresh_cluster_liveness` は DB へは
        // 書かないが、実クラスタへ `ssh -O check` を打つ副作用（生の ssh 呼び出し）は「裏方の仕事」その
        // ものなので、verify では走らせない。
        if self.eligible.is_some() {
            return;
        }
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
        let mut specs: Vec<(String, String, u64)> = self
            .config
            .clusters
            .values()
            .map(|c| (c.id.clone(), c.host.clone(), c.liveness_probe_secs))
            .collect();
        specs.sort();
        for (id, host, liveness_probe_secs) in specs {
            let mut alive = (self.cluster_liveness_probe)(&ssh_command, &host);
            // ADR-0062 A（Phase 107）: `-O check` は unix ソケットを見るだけで、NAT / ファイアウォールの
            // idle timeout で TCP が黙って死んでいても「Master running」を返し続ける。master 越しの
            // 実通信（`ssh -o BatchMode=yes <host> -- true`）で定期的に確定させる。
            if alive
                && liveness_probe_secs > 0
                && let Some(probe) = self.cluster_command_probe.clone()
            {
                let due = self.last_cluster_command_probe.get(&id).is_none_or(|last| {
                    now.duration_since(*last) >= Duration::from_secs(liveness_probe_secs)
                });
                if due {
                    self.last_cluster_command_probe.insert(id.clone(), now);
                    let ssh_command = ssh_command.clone();
                    let host_for_probe = host.clone();
                    let ok = run_cluster_hooks_off_async(move || {
                        probe(&ssh_command, &host_for_probe, LIVENESS_PROBE_TIMEOUT)
                    });
                    if !ok {
                        alive = false;
                        tracing::warn!(
                            cluster = %id, %host,
                            "liveness probe (ssh -- true) failed; treating the connection as dead (ADR-0062 A)"
                        );
                        self.cluster_disconnect_info.insert(
                            id.clone(),
                            ClusterDisconnectInfo {
                                exit_code: None,
                                detail: format!(
                                    "liveness probe: `ssh -o BatchMode=yes {host} -- true` failed or timed out"
                                ),
                                reported: false,
                            },
                        );
                        // ADR-0062 A: 死んだ接続の残骸を片付ける（`-O exit`。ベストエフォート）。
                        if let Some(disconnector) = self.cluster_disconnector.clone() {
                            let id_for_disconnect = id.clone();
                            let host_for_disconnect = host.clone();
                            let _ = run_cluster_hooks_off_async(move || {
                                disconnector(&id_for_disconnect, &host_for_disconnect)
                            });
                        }
                    }
                }
            }
            self.cluster_connected.insert(id.clone(), alive);
            if alive {
                // ADR-0062 A: 接続が戻ったら、前回の切断の詳細は捨てる（次に切れたときは新しい詳細で
                // 1 回だけ報告する）。
                self.cluster_disconnect_info.remove(&id);
                if self.cluster_cooldown.remove(&id).is_some() {
                    tracing::info!(cluster = %id, %host, "ssh ControlMaster connection is back; cluster cooldown cleared");
                }
            }
        }
    }

    /// ADR-0062 A（Phase 107）: celeris が保持している master（`Child`）の終了を検出する
    /// フック（[`ClusterMasterWatcher`]）を、tick ごとに 1 回呼ぶ。見つかった終了はクラスタごとに
    /// 記録し（[`ClusterDisconnectInfo`]）、次に `mark_cluster_unavailable` がそのクラスタを
    /// 拾ったときに stderr の末尾・exit code を報告へ足し、`Event::ClusterMasterExited` を 1 回残す。
    fn refresh_cluster_master_exits(&mut self) {
        if self.eligible.is_some() {
            return;
        }
        let Some(watcher) = self.cluster_master_watcher.clone() else {
            return;
        };
        for exit in watcher() {
            self.cluster_connected.insert(exit.cluster.clone(), false);
            tracing::warn!(
                cluster = %exit.cluster, exit_code = ?exit.exit_code,
                "cluster ssh master exited on its own (ADR-0062 A)"
            );
            self.cluster_disconnect_info.insert(
                exit.cluster.clone(),
                ClusterDisconnectInfo {
                    exit_code: exit.exit_code,
                    detail: exit.stderr_tail,
                    reported: false,
                },
            );
        }
    }

    /// ADR-0053 D3（Phase 66）: `[[clusters]].forwards` を持つクラスタのトンネルを見て、必要なら張り直す。
    /// `refresh_cluster_liveness` の直後に呼ぶ（`cluster_connected` を読むため、同じ間隔で間引く）。
    ///
    /// 1. master が死んでいたら、まず鍵認証だけで繋ぎ直す（`cluster_connector`。`auth` の値に関わらず試す
    ///    — 「TOTP を要求する前に鍵認証を試す」。ADR-0053 D3）。それでも駄目なら「TOTP ログインが要る」を
    ///    立てる（クラスタごとに 1 回だけ報告する。`cluster_login_needed` が同じ outage の間の重複を防ぐ）。
    /// 2. master が生きていれば、forward ごとに probe → 届かなければ `tunnel_forward_ensurer` で
    ///    (再)確立 → 再 probe。最終的な到達性の変化を `tunnel_events` に積む（Up/Down/Restored）。
    fn refresh_cluster_tunnels(&mut self) {
        // ADR-0041 D5 / Phase 66c（実機 2026-09-21、release 2b3aaf1d633a）: `--mode verify` は
        // dispatch も裏方の仕事もしない。しかしこれは無条件に呼ばれていたため、staging の verify
        // インスタンスでも forward の(再)確立・`cluster_login_needed` の報告書き込みが起こり、
        // `verify.sh` check 2（本番のコピーと件数が一致すること）が `reports(snapshot=135
        // staging=136)` で落ちた。`refresh_cluster_liveness` と同じ目印
        // （`self.eligible.is_some()` = `--mode verify` の煙試験）で丸ごと止める。
        if self.eligible.is_some() {
            return;
        }
        if !self
            .config
            .clusters
            .values()
            .any(|c| !c.forwards.is_empty())
        {
            return;
        }
        let now_instant = Instant::now();
        if let Some(last) = self.last_cluster_tunnel_refresh
            && now_instant.duration_since(last) < CLUSTER_LIVENESS_INTERVAL
        {
            return;
        }
        self.last_cluster_tunnel_refresh = Some(now_instant);

        let mut specs: Vec<ClusterSpec> = self
            .config
            .clusters
            .values()
            .filter(|c| !c.forwards.is_empty())
            .cloned()
            .collect();
        specs.sort_by(|a, b| a.id.cmp(&b.id));

        for spec in specs {
            let alive = self.ensure_cluster_master_for_tunnel(&spec);
            if !alive {
                for fwd in &spec.forwards {
                    self.observe_tunnel(
                        &spec.id,
                        &fwd.listen,
                        false,
                        false,
                        Some("the cluster ssh master is not connected".to_string()),
                    );
                }
                continue;
            }
            for fwd in spec.forwards.clone() {
                self.refresh_one_forward(&spec, &fwd);
            }
        }
    }

    /// master が生きているか確認し、死んでいれば鍵認証を 1 回試す（D3: TOTP の前に鍵認証）。
    /// 成功したら `cluster_connected` / cooldown を更新し、「ログインが要る」を解除する。
    /// 失敗したら「TOTP ログインが要る」を立てる（初めてのときだけ報告する）。
    fn ensure_cluster_master_for_tunnel(&mut self, spec: &ClusterSpec) -> bool {
        if self
            .cluster_connected
            .get(&spec.id)
            .copied()
            .unwrap_or(false)
        {
            self.clear_login_needed(&spec.id);
            return true;
        }
        if let Some(connector) = self.cluster_connector.clone() {
            match connector(&spec.id, &spec.host) {
                Ok(()) => {
                    self.cluster_connected.insert(spec.id.clone(), true);
                    self.cluster_cooldown.remove(&spec.id);
                    self.clear_login_needed(&spec.id);
                    tracing::info!(cluster = %spec.id, host = %spec.host, "tunnel: key auth reconnected the ssh master");
                    return true;
                }
                Err(detail) => {
                    tracing::debug!(cluster = %spec.id, host = %spec.host, %detail, "tunnel: key auth did not reconnect; a TOTP login is needed");
                }
            }
        }
        self.mark_login_needed(spec);
        false
    }

    fn clear_login_needed(&mut self, cluster_id: &str) {
        if self.cluster_login_needed.remove(cluster_id) {
            tracing::info!(cluster = %cluster_id, "tunnel: login is no longer needed");
        }
    }

    /// 初めて「ログインが要る」状態に入ったときだけ、イベントを積み報告を 1 件記録する（重複排除）。
    fn mark_login_needed(&mut self, spec: &ClusterSpec) {
        if !self.cluster_login_needed.insert(spec.id.clone()) {
            return;
        }
        let now = OffsetDateTime::now_utc();
        self.push_tunnel_event(&spec.id, "", TunnelEventKind::LoginNeeded, now);
        if let Err(e) = crate::reports::record_cluster_login_needed_report(
            self.store.as_ref(),
            &spec.id,
            &spec.host,
            now,
        ) {
            tracing::warn!(cluster = %spec.id, error = %e, "failed to record the cluster login-needed report");
        }
    }

    /// ADR-0053 D3 / Phase 85 / ADR-0066 D3: 1 本の forward。まずリスナー（`-O forward` の手元の待ち受け）
    /// の有無を見る（軽い。ssh は起こさない）。無ければ `tunnel_forward_ensurer` で(再)確立を試みる。
    ///
    /// **target の健康 probe は tick の同期経路から外れている**（ADR-0066 D3。Phase 85 の
    /// `probe_interval_secs` によるバックオフだけでは、tick の間隔と `probe_interval_secs` の既定が
    /// 同じ 30 秒だったため実質毎 tick 期限が来てしまい、`slow tick phases`〈`tunnel_ms`≈2000〉が
    /// 1 時間に 118 件出ていた〈本番観測〉）。listener が有るとき、この forward を初めて観測する
    /// （`tunnel_probe_state` にまだ記録が無い）場合だけ、この tick の中で 1 回だけ同期に probe して
    /// 種を蒔く。2 回目以降は専用スレッド（`ensure_tunnel_prober_started`）が裏で probe し続け、
    /// ここは `Mutex` を読むだけ（ssh も HTTP も呼ばない）。
    fn refresh_one_forward(&mut self, spec: &ClusterSpec, fwd: &ClusterForwardSpec) {
        let mut listener = self.probe_listener(&fwd.listen);
        let mut ensure_error: Option<String> = None;
        if !listener && let Some(ensure) = self.tunnel_forward_ensurer.clone() {
            match ensure(&spec.host, &fwd.listen, &fwd.target) {
                Ok(()) => {
                    listener = self.probe_listener(&fwd.listen);
                }
                Err(detail) => {
                    tracing::warn!(cluster = %spec.id, listen = %fwd.listen, target = %fwd.target, %detail, "tunnel: could not (re-)establish the forward");
                    ensure_error = Some(detail);
                }
            }
        }
        if !listener {
            let error = ensure_error
                .unwrap_or_else(|| "no listener on the local forward address".to_string());
            self.observe_tunnel(&spec.id, &fwd.listen, false, false, Some(error));
            return;
        }

        // **専用スレッドを起こすのは、この forward を種蒔きした後**（`ensure_tunnel_prober_started` は
        // 呼ぶたびに全 forward の一覧を見るので、先に起こすと「まだ種が無い」状態のこの forward を
        // 専用スレッドと取り合い、probe が二重に呼ばれることがある）。
        let key = tunnel_key(&spec.id, &fwd.listen);
        let existing = self
            .tunnel_probe_state
            .lock()
            .ok()
            .and_then(|guard| guard.get(&key).cloned());
        let observed = match existing {
            Some(state) => state,
            // まだ一度も probe していない（この forward の listener が今このプロセスで初めて有りに
            // なった）。専用スレッドの次の周回を待つと最初の観測が遅れるので、ここだけ 1 回同期に
            // probe して種を蒔く（Phase 85 までの「listener が有ればまず 1 回は確かめる」を保つ）。
            None => {
                let now = Instant::now();
                let result = self.probe_forward(&fwd.listen);
                let healthy = result.is_ok();
                let state = TargetProbeState {
                    healthy,
                    error: result.err(),
                    checked_at: now,
                    interval_secs: next_probe_interval_secs(
                        fwd.probe_interval_secs.max(1),
                        fwd.probe_interval_secs.max(1),
                        healthy,
                    ),
                };
                if let Ok(mut guard) = self.tunnel_probe_state.lock() {
                    guard.insert(key, state.clone());
                }
                state
            }
        };
        self.ensure_tunnel_prober_started();
        let error = if observed.healthy {
            None
        } else {
            Some(match &observed.error {
                Some(reason) => format!(
                    "target {} (as seen from {}) did not answer /v1/models through the forward: {reason}",
                    fwd.target, spec.host
                ),
                None => format!(
                    "target {} did not answer /v1/models through the forward",
                    fwd.target
                ),
            })
        };
        self.observe_tunnel(&spec.id, &fwd.listen, true, observed.healthy, error);
    }

    /// ADR-0066 D3: forward 一覧から、target probe を裏で行い続ける専用スレッドを起こす（二重に起こさ
    /// ない。`tunnel_probe` が挿してなければ何もしない）。`Drop` で止める。
    fn ensure_tunnel_prober_started(&mut self) {
        if self.tunnel_prober_stop.is_some() {
            return;
        }
        let Some(probe) = self.tunnel_probe.clone() else {
            return;
        };
        let forwards: Vec<(String, String, String, u64)> = self
            .config
            .clusters
            .values()
            .flat_map(|c| {
                let id = c.id.clone();
                c.forwards.iter().map(move |f| {
                    (
                        id.clone(),
                        f.listen.clone(),
                        f.target.clone(),
                        f.probe_interval_secs.max(1),
                    )
                })
            })
            .collect();
        if forwards.is_empty() {
            return;
        }
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let state = self.tunnel_probe_state.clone();
        let stop_for_thread = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("celeris-tunnel-probe".to_string())
            .spawn(move || tunnel_prober_loop(probe, forwards, state, stop_for_thread));
        match spawned {
            Ok(_handle) => self.tunnel_prober_stop = Some(stop),
            Err(e) => tracing::warn!(error = %e, "tunnel: could not start the target-probe thread"),
        }
    }

    /// forward のリスナーが手元に有るか（`-O forward`/`ssh -N -L` が届いているか）。
    fn probe_listener(&self, listen: &str) -> bool {
        self.tunnel_listener_probe
            .as_ref()
            .map(|p| p(listen))
            .unwrap_or(false)
    }

    /// forward の target（先方）が健全か（`/v1/models` が応答するか）。`refresh_one_forward` の
    /// 初回の種蒔きと、専用スレッドの両方から呼ぶ。
    fn probe_forward(&self, listen: &str) -> Result<(), String> {
        self.tunnel_probe
            .as_ref()
            .map(|p| p(listen))
            .unwrap_or_else(|| Err("no tunnel probe is configured".to_string()))
    }

    /// 状態遷移を記録する（`tunnel_state` を更新し、フェーズ（Up/Down/TargetUnreachable）が変わったときだけ
    /// `tunnel_events` に積む。Phase 85: 同じフェーズが続く間は 1 行も出さない＝スパムしない）。
    fn observe_tunnel(
        &mut self,
        cluster: &str,
        listen: &str,
        listener: bool,
        target_healthy: bool,
        last_error: Option<String>,
    ) {
        let key = tunnel_key(cluster, listen);
        let prev_phase = self.tunnel_state.get(&key).map(ForwardObservation::phase);
        let now = OffsetDateTime::now_utc();
        let observation = ForwardObservation {
            listener,
            target_healthy,
            last_error,
        };
        let new_phase = observation.phase();
        if prev_phase != Some(new_phase) {
            let kind = match new_phase {
                ForwardPhase::Up => {
                    if matches!(
                        prev_phase,
                        Some(ForwardPhase::Down) | Some(ForwardPhase::TargetUnreachable)
                    ) {
                        TunnelEventKind::Restored
                    } else {
                        TunnelEventKind::Up
                    }
                }
                ForwardPhase::Down => TunnelEventKind::Down,
                ForwardPhase::TargetUnreachable => TunnelEventKind::TargetUnreachable,
            };
            self.push_tunnel_event(cluster, listen, kind, now);
        }
        self.tunnel_state.insert(key, observation);
    }

    fn push_tunnel_event(
        &mut self,
        cluster: &str,
        listen: &str,
        kind: TunnelEventKind,
        at: OffsetDateTime,
    ) {
        tracing::info!(
            cluster,
            listen,
            kind = kind.as_str(),
            "tunnel: state transition"
        );
        self.tunnel_events.push_back(TunnelEvent {
            cluster: cluster.to_string(),
            listen: listen.to_string(),
            kind,
            at,
        });
        while self.tunnel_events.len() > TUNNEL_EVENTS_CAP {
            self.tunnel_events.pop_front();
        }
    }

    /// ADR-0013 D4: メモリ上の状態からスナップショットを作り `watch` に送る（DB には書かない。受け手がいなくても無害）。
    fn publish_snapshot(&mut self) {
        if self.publisher.is_none() {
            return;
        }
        // `&mut self` が要る（アカウントのスキャンキャッシュを埋める）ので、`self.publisher` を借りる前に計算する。
        let (accounts_root, accounts_roots, max_runs_per_account, accounts) =
            self.accounts_snapshot();
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
                connected: self
                    .cluster_connected
                    .get(&spec.id)
                    .copied()
                    .unwrap_or(false),
                cooldown_until: self
                    .cluster_cooldown
                    .get(&spec.id)
                    .filter(|until| **until > now_instant)
                    .map(|until| rfc3339(now + until.saturating_duration_since(now_instant))),
                auth: spec.auth.clone(),
                connect_pending: self.connect_pending_clusters.contains(&spec.id),
                // ADR-0053 D3（Phase 66）: トンネル（forward）の生存。`forwards` が無いクラスタは空。
                tunnel_login_needed: self.cluster_login_needed.contains(&spec.id),
                tunnel_forwards: spec
                    .forwards
                    .iter()
                    .map(|f| TunnelForwardLive {
                        listen: f.listen.clone(),
                        target: f.target.clone(),
                        up: self.tunnel_reachable(&spec.id, &f.listen),
                        listener: self.tunnel_listener_present(&spec.id, &f.listen),
                        target_healthy: self.tunnel_target_healthy(&spec.id, &f.listen),
                        last_error: self.tunnel_last_error(&spec.id, &f.listen),
                    })
                    .collect(),
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
            reports: None,
            // ADR-0033 D5（Phase 26）: 未読の件数は API が応答を組むときに埋める（`reports` と同じ理由）。
            approvals_pending: 0,
            providers,
            clusters,
            accounts_root,
            max_runs_per_account,
            accounts_roots,
            accounts,
            // ADR-0043 D3（Phase 56）: コンテナ実行の設定と起動時の検出。
            containers: Some(task_ops::daemon::ContainersLive {
                preference: self.config.containers.preference.as_str().to_string(),
                runtime: self.container_probe.program().map(str::to_string),
                probes: self
                    .container_probe
                    .tried
                    .iter()
                    .map(|(runtime, detail)| task_ops::daemon::ContainerProbeView {
                        runtime: runtime.clone(),
                        detail: detail.clone(),
                    })
                    .collect(),
                image_default: self.config.containers.image_default.clone(),
                build_dir: self.config.containers.build_dir.display().to_string(),
            }),
        };
        // 受け手（API）がいなければ送信は失敗するが、デーモンの動作には関係ない。
        let _ = publisher.tx.send(Some(snapshot));
    }

    /// ADR-0024 D5 / ADR-0025 D6: スナップショットに載せる `accounts_root`（claude-code の別名）/ `accounts_roots`
    /// （アダプタ → 根ディレクトリ）/ `max_runs_per_account` / `accounts[]`（`adapter` → `id` の順）。
    /// `[accounts]` が無ければ全て空。
    #[allow(clippy::type_complexity)]
    fn accounts_snapshot(
        &mut self,
    ) -> (
        Option<String>,
        HashMap<String, String>,
        Option<usize>,
        Vec<AccountLive>,
    ) {
        let Some(cfg) = self.config.accounts.clone() else {
            return (None, HashMap::new(), None, Vec::new());
        };
        let now = (self.now_unix_fn)();
        let mut items = Vec::new();
        let mut roots = HashMap::new();
        for adapter in AccountAdapter::ALL {
            let Some(root) = cfg.root_for(adapter) else {
                continue;
            };
            roots.insert(adapter.as_str().to_string(), root.display().to_string());
            let dirs = self
                .accounts_scan_cache
                .entry(adapter)
                .or_insert_with(|| scan_accounts(root, adapter))
                .clone();
            let Some(book) = self.account_book(adapter) else {
                continue;
            };
            let book = book.lock().unwrap_or_else(|e| e.into_inner());
            for d in &dirs {
                let in_use = self.account_in_use(adapter, &d.id);
                let state = book.state(&d.id);
                let eval = evaluate(
                    &AccountCandidate {
                        id: &d.id,
                        logged_in: d.logged_in,
                        in_use,
                    },
                    state,
                    cfg.max_runs_per_account,
                    now,
                );
                items.push(AccountLive {
                    adapter: adapter.as_str().to_string(),
                    id: d.id.clone(),
                    logged_in: d.logged_in,
                    in_use: in_use as u32,
                    usage: state
                        .and_then(|s| s.usage.as_ref())
                        .map(|u| AccountUsageLive {
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
                    cooldown: state.and_then(|s| s.cooldown.as_ref()).map(|c| {
                        AccountCooldownLive {
                            until: c.until,
                            reason: account_cooldown_reason_name(c.reason).to_string(),
                        }
                    }),
                    last_check: state.and_then(|s| s.last_check.as_ref()).map(|c| {
                        ProviderCheckView {
                            at: rfc3339(
                                OffsetDateTime::from_unix_timestamp(c.at)
                                    .unwrap_or(OffsetDateTime::UNIX_EPOCH),
                            ),
                            result: c.result.clone(),
                            detail: c.detail.clone(),
                        }
                    }),
                    login_pending: self
                        .login_pending_accounts
                        .contains(&Self::login_pending_key(adapter, &d.id)),
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
        // ADR-0061（Phase 104）: `since`（dispatch した時刻）も一緒に取り出し、run の wall time を計算する。
        let (account, account_adapter, run_since) = self
            .running
            .remove(&task_id)
            .map(|e| (e.account, e.account_adapter, Some(e.since)))
            .unwrap_or((None, None, None));
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
        // ADR-0033 D3 / ADR-0034 D2（監査 M-1〜M-3）: 報告の材料も、result を消費する前に取る。
        // `terminal_report` は `Ok(RunOutcome)` の内容（question / worker 自身が返した error）。
        // `adapter_error_text` は `Err(AdapterError)`（アダプタ／供給側の失敗）の表示文字列。
        // どちらも「報告するかどうか」は後で `outcome.next` を見て決める（run の終端ではなくタスクの終端状態）。
        let terminal_report = crate::reports::terminal_report(&result);
        let adapter_error_text = result.as_ref().err().map(|e| e.to_string());
        // ADR-0033 D6（Phase 24）: 結果ファイルの `memory` を、この run の担当の記憶に追記する
        // （run の終わり方に依らず。ファイル I/O だけで、覚える中身を決めるのはワーカー側）。
        self.absorb_memory(&task);
        // ADR-0033 D4 / SPEC §3.1: 部をまたぐ委譲を試みた run は、子を作らずに秘書へ聞く終わり方にする
        // （`StoreSink::delegate_impl` が `QuestionRaised` を残している）。
        let cross_department =
            cross_department_questions_of(&self.store.events_for(task_id)?, &run_id);
        // ADR-0070 D3（Phase 116）: `Trigger::InfraRequeue` を選んだときだけ `Some(n)`（n 回目の
        // インフラ再試行）。`Ok(outcome) =>` の中で `self.infra_backoff` のバックオフ期限を立てるのに使う。
        let mut infra_requeue_n: Option<u32> = None;
        // ADR-0072 D7（Phase E1）: この run の構造化した終わり方（`WorkerFinished.end` に写す）。
        let mut run_end: Option<task_core::RunEnd> = None;
        // ADR-0072 D9: `Terminal::Yielded` の生の checkpoint JSON（`result.json` の `yield`）。
        let mut yield_checkpoint_json: Option<serde_json::Value> = None;
        let (mut trigger, mut outcome_str, usage, provider_outcome) = match result {
            Ok(RunOutcome {
                terminal:
                    Terminal::Done {
                        summary,
                        usage,
                        evidence,
                    },
                ..
            }) => {
                subject = ReviewSubject {
                    summary: summary.clone(),
                    evidence,
                };
                run_end = Some(task_core::RunEnd::Completed);
                (
                    Trigger::WorkerDone,
                    format!("done: {summary}"),
                    usage,
                    ProviderOutcome::Ok,
                )
            }
            // ADR-0033 D4 / Phase 28: 対話 run は `Question` を出さない。人に聞きたいことは返事に書けば
            // よいので、そのまま `Done` 扱いにする（`approvals` の行は作らない。実機で秘書が「最終試行なので
            // 自分の一般知識で答えた」まま `Question` の代わりに走った事故の反省）。
            Ok(RunOutcome {
                terminal: Terminal::Question { text },
                ..
            }) if task_core::is_conversation(&task) => {
                subject = ReviewSubject {
                    summary: text.clone(),
                    evidence: Vec::new(),
                };
                run_end = Some(task_core::RunEnd::Completed);
                (
                    Trigger::WorkerDone,
                    format!("done: {text}"),
                    None,
                    ProviderOutcome::Ok,
                )
            }
            Ok(RunOutcome {
                terminal: Terminal::Question { text },
                ..
            }) => {
                run_end = Some(task_core::RunEnd::Question);
                (
                    Trigger::WorkerQuestion,
                    format!("question: {text}"),
                    None,
                    ProviderOutcome::Ok,
                )
            }
            Ok(RunOutcome {
                terminal: Terminal::Error { message, retryable },
                ..
            }) => {
                // ADR-0072 D7: 二重の安全網。構造化されていない `Error` でも、予算切れの語彙なら
                // `BudgetExhausted` として分類する（usage はこの経路では運べない）。
                run_end = if retryable {
                    classify_budget_kind_from_text(&message)
                        .map(|kind| task_core::RunEnd::BudgetExhausted { kind })
                } else {
                    None
                };
                if run_end.is_none() {
                    run_end = Some(task_core::RunEnd::Failed { retryable });
                }
                (
                    Trigger::WorkerError { retryable },
                    format!("error(retryable={retryable}): {message}"),
                    None,
                    ProviderOutcome::Ok,
                )
            }
            // ADR-0072 D9/D10（Phase E1）: graceful yield（result.json の `{"yield": {...}}`）。
            // `trigger`/`outcome_str` はここでは仮の値で、continuation の判定（下）で確定させる。
            Ok(RunOutcome {
                terminal: Terminal::Yielded { checkpoint, usage },
                ..
            }) => {
                run_end = Some(task_core::RunEnd::Yielded);
                yield_checkpoint_json = Some(checkpoint);
                (
                    Trigger::WorkerError { retryable: true },
                    "error(retryable=true): yielded".to_string(),
                    usage,
                    ProviderOutcome::Ok,
                )
            }
            // ADR-0072 D7（Phase E1）: turn / wall-clock / context の上限に当たった。usage を運ぶ。
            Ok(RunOutcome {
                terminal:
                    Terminal::BudgetExhausted {
                        kind,
                        message,
                        usage,
                    },
                ..
            }) => {
                run_end = Some(task_core::RunEnd::BudgetExhausted { kind });
                (
                    Trigger::WorkerError { retryable: true },
                    format!("error(retryable=true): budget exhausted ({kind:?}): {message}"),
                    usage,
                    ProviderOutcome::Ok,
                )
            }
            Err(e) => match provider_failure_outcome(&e) {
                // ADR-0010 D5（P-21）: 供給側失敗は attempts を消費せず requeue し、プロバイダを cooldown にする。
                Some(po)
                    if consecutive_requeues(&self.store.events_for(task_id)?)
                        < self.config.max_requeues =>
                {
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
                // ADR-0070 D3（Phase 116）: プロバイダが分類できない失敗（resume 拒否・プロセス
                // I/O・result.json 不在など）は「インフラ都合」として attempts を消費せず、
                // `max_infra_retries` までバックオフして再試行する。上限に達したときだけ
                // `WorkerError{retryable:false}`（無条件に `Failed`）で打ち切り、`"infra failure ×N"`
                // を付ける（D1 の失敗分類がこの接頭辞を見る）。
                None => {
                    let infra_n = consecutive_infra_requeues(&self.store.events_for(task_id)?) + 1;
                    if infra_n <= self.config.max_infra_retries {
                        infra_requeue_n = Some(infra_n);
                        (
                            Trigger::InfraRequeue,
                            format!("infra_requeue: adapter: {e}"),
                            None,
                            ProviderOutcome::Ok,
                        )
                    } else {
                        (
                            Trigger::WorkerError { retryable: false },
                            format!("{INFRA_FAILURE_MARKER}{infra_n}: adapter: {e}"),
                            None,
                            ProviderOutcome::Ok,
                        )
                    }
                }
            },
        };
        // ADR-0072 D7/D8/D9/D11/D18（Phase E1）: 予算切れ・yield の続き（continuation）。
        // checkpoint は常に合成して残す（(b)）。continuation そのものの可否・上限到達の扱いは
        // `[execution]` で決める。無効化・上限到達のときは trigger/outcome_str を従来の形に戻す。
        let mut checkpoint_event: Option<Event> = None;
        if let Some(end) = run_end
            && end.is_continuable()
        {
            let events_so_far = self.store.events_for(task_id)?;
            let run_seq = current_run_seq(&events_so_far);
            let workspace_dir = self.task_dir(&task);
            let artifacts_dir = workspace_dir.as_ref().map(|d| self.artifacts_dir(&task, d));
            let workspaces = self.task_workspaces_for(&task);
            let cwd = workspaces.as_ref().and_then(|w| w.cwd());
            let branch = workspaces
                .as_ref()
                .and_then(|w| w.repos.first())
                .and_then(|r| r.branch())
                .unwrap_or_default();
            // ADR-0072 D8: `tests_run`（最大 10 件）と `recent_activity`（最大 20 行）は、この run の
            // `WorkerProgress{kind: tool_use}` と、それに続く `tool_result` の組から作る。
            let activity: Vec<crate::checkpoint::ToolActivity> = events_so_far
                .iter()
                .filter_map(|(_, ev)| match ev {
                    Event::WorkerProgress {
                        run_id: r,
                        kind: Some(task_core::ProgressKind::ToolUse),
                        tool,
                        summary,
                        ..
                    } if r == &run_id => Some(crate::checkpoint::ToolActivity::Use {
                        tool: tool.clone(),
                        summary: summary.clone(),
                    }),
                    Event::WorkerProgress {
                        run_id: r,
                        kind: Some(task_core::ProgressKind::ToolResult),
                        error,
                        ..
                    } if r == &run_id => {
                        Some(crate::checkpoint::ToolActivity::Result { error: *error })
                    }
                    _ => None,
                })
                .collect();
            let mechanical = crate::checkpoint::gather(cwd, branch, &activity);
            // D9: 優先順位は `result.json.yield` > `checkpoint.json` > mechanical。
            let worker_checkpoint = yield_checkpoint_json
                .as_ref()
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .or_else(|| {
                    artifacts_dir
                        .as_deref()
                        .and_then(crate::checkpoint::read_worker_checkpoint)
                });
            let checkpoint_end = end
                .as_checkpoint_end()
                .unwrap_or(task_core::CheckpointEnd::BudgetExhausted);
            let ctx = task_core::CheckpointContext {
                task_id: task_id.to_string(),
                work_unit: None,
                run_id: run_id.clone(),
                run_seq,
                end: checkpoint_end,
                created_at: rfc3339(OffsetDateTime::now_utc()),
            };
            let checkpoint = task_core::merge_checkpoint(worker_checkpoint, mechanical, ctx);

            if self.config.execution.continuation {
                let continuations_so_far = consecutive_continuations(&events_so_far);
                let prev_checkpoint = latest_checkpoint(&events_so_far, None);
                let progressed =
                    task_core::checkpoint_shows_progress(prev_checkpoint.as_ref(), &checkpoint);
                let no_progress = if progressed {
                    0
                } else {
                    no_progress_streak(&events_so_far, None) + 1
                };
                checkpoint_event = Some(Event::CheckpointSaved {
                    run_id: run_id.clone(),
                    work_unit_id: None,
                    checkpoint: Box::new(checkpoint),
                });
                if continuations_so_far >= self.config.execution.max_continuations_per_work_unit
                    || no_progress >= self.config.execution.no_progress_limit
                {
                    // ADR-0072 D18: 上限到達・進捗なしは失敗にせず、人に聞く（blocked）。
                    trigger = Trigger::WorkerQuestion;
                    outcome_str = format!(
                        "question: 実行が進みません（continuation {continuations_so_far} 回 / 進捗なし {no_progress} 回）。予算を増やして続ける／分割し直す（replan）／中止のいずれかを選んでください。"
                    );
                } else {
                    trigger = Trigger::Continue {
                        why: task_core::ContinueWhy::Continue,
                    };
                    outcome_str = format!(
                        "continue: {} の続き（Run #{}）",
                        describe_run_end(end),
                        run_seq + 1
                    );
                }
            } else {
                // ADR-0072 §6 (f): `[execution] continuation = false` なら従来どおり
                // `WorkerError{retryable:true}` に戻す（checkpoint も保存しない）。
                trigger = Trigger::WorkerError { retryable: true };
                outcome_str = format!(
                    "error(retryable=true): {} (continuation disabled)",
                    describe_run_end(end)
                );
            }
        }
        // ADR-0054 D1（Phase 67）: CoS の対話 run（継続セッション）は、この run の usage を
        // `node_sessions.approx_tokens` に積む（rollover 判定の材料。turns も 1 進む）。継続セッションの
        // 対象でない run（`node_sessions` の行が無い）では `node_session_touch` が no-op で返るだけ。
        if task_core::is_conversation(&task) && task.assignee.as_deref() == Some(task_core::COS_ID)
        {
            let tokens = usage
                .as_ref()
                .map(|u| u.input_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0))
                .unwrap_or(0);
            if let Err(e) = self.store.node_session_touch(
                task_core::COS_ID,
                task_core::SessionKind::Conversation,
                None,
                tokens as i64,
                OffsetDateTime::now_utc(),
            ) {
                tracing::warn!(%task_id, error = %e, "failed to record session usage");
            }
        }
        // ADR-0033 D4: 部をまたぐ委譲の質問は、run の自己申告の終わり方より優先する（子は作られていない）。
        // Phase 27: 人に見せる質問は 1 件の部またぎにつき 1 つ（`approvals` の行の単位）。
        let mut questions: Vec<String> = Vec::new();
        if matches!(trigger, Trigger::WorkerQuestion) {
            questions.push(
                outcome_str
                    .strip_prefix("question: ")
                    .unwrap_or(outcome_str.as_str())
                    .to_string(),
            );
        }
        if !cross_department.is_empty() {
            if !matches!(trigger, Trigger::WorkerQuestion) {
                trigger = Trigger::WorkerQuestion;
                outcome_str = format!("question: {}", cross_department.join("\n"));
                subject = ReviewSubject::default();
            }
            questions.extend(cross_department.iter().cloned());
        }
        // ADR-0024 D4 / S10: プール経由の run の失敗は、原因がアカウント側（throttled/auth_failed/exhausted）なら
        // アカウントを cooldown にしプロバイダは cooldown にしない。`Spawn` 失敗（起動できない）はアカウントの
        // 責任ではないので、通常どおりプロバイダを cooldown にする（`failure_reason == Some("spawn")`）。
        let account_at_fault = account.is_some() && failure_reason != Some("spawn");
        let policy_outcome = if account_at_fault {
            ProviderOutcome::Ok
        } else {
            provider_outcome.clone()
        };
        self.policy.report(provider.clone(), &policy_outcome);

        // ADR-0061（Phase 104）: `retries` はこの run が始まった時点でタスクが既に消費していた試行回数
        // （= 遷移前の `task.attempts`）。
        let metrics = run_since.map(|since| task_core::RunMetrics {
            wall_ms: wall_ms_since(since),
            retries: task.attempts,
            peak_context_tokens: None,
            turns: None,
        });
        let finished = Event::WorkerFinished {
            run_id: run_id.clone(),
            outcome: outcome_str.clone(),
            usage,
            role: None,
            metrics,
            end: run_end,
        };
        let mut events = vec![finished];
        // ADR-0072 D5/D8（Phase E1）: `CheckpointSaved` は `WorkerFinished` と同じトランザクションで残す。
        if let Some(checkpoint_event) = checkpoint_event {
            events.push(checkpoint_event);
        }
        if let Some(reason) = failure_reason {
            match (&account, account_adapter) {
                // ADR-0024 D4: アカウントの cooldown として記録する（`ProviderThrottled` イベントは出さない）。
                (Some(acct), Some(adapter)) if reason != "spawn" => {
                    self.record_account_failure(adapter, acct, reason, &provider_outcome)
                }
                // ADR-0013 D9 / S10: プールを使わない、または Spawn 失敗（アカウント非依存）はプロバイダの
                // cooldown として、遷移と同じトランザクションで記録する。
                _ => {
                    if let Some(ev) =
                        self.provider_throttled_event(&provider, &provider_outcome, reason)
                    {
                        events.push(ev);
                    }
                }
            }
        }
        // ADR-0033 D5（Phase 26 / Phase 27）: `Question` で終わった run（部をまたぐ委譲の質問への置き換えも
        // 含む）は、既存の `answers[]` の経路（`Status::Blocked`）に加えて `approvals` にも 1 件ずつ残す
        // （部またぎは 1 件の委譲につき 1 行。同じ質問がまだ未決なら増やさない）。
        for text in &questions {
            if let Err(e) = crate::approvals::record_question_approval(
                self.store.as_ref(),
                &task,
                text,
                OffsetDateTime::now_utc(),
            ) {
                tracing::warn!(%task_id, %run_id, error = %e, "failed to record the approval for this question");
            }
        }
        match self
            .store
            .apply_transition_with_events(task_id, trigger, events)
        {
            Ok(outcome) => {
                tracing::info!(%task_id, %run_id, next = ?outcome.next, attempts = outcome.attempts, outcome = %outcome_str, "worker finished");
                // ADR-0070 D3（Phase 116）: `InfraRequeue` は `dispatch_ready` がすぐ拾わないよう、
                // バックオフの期限を立てる（30秒/2分/5分。`infra_backoff_delay`）。
                if let Some(n) = infra_requeue_n {
                    let until = OffsetDateTime::now_utc() + infra_backoff_delay(n);
                    tracing::warn!(%task_id, %run_id, attempt = n, until = %until, "infra failure; requeued with backoff (attempts not consumed; ADR-0070 D3)");
                    self.infra_backoff.insert(task_id, until);
                }
                // ADR-0033 D4（Phase 24 / 監査 M-5）: 対話用タスクの run なら、`summary`（質問なら本文、
                // 失敗なら理由）をそのノードの返事として `messages` に残す。**「返事できませんでした」は
                // タスクが `Failed` に落ちたときだけ**（requeue / まだ試行が残る失敗では書かない）。
                self.record_conversation_reply(&task, &run_id, &outcome_str, outcome.next);
                // ADR-0038 D1（Phase 41）: 対話 run が結果ファイルで宣言した次の途中目標を
                // `milestones` に入れる（`done` のときだけ。差し替えは決定的）。
                if outcome_str.starts_with("done: ") {
                    self.absorb_milestone_proposal(&task);
                }
                // ADR-0034 D2（監査 M-1〜M-3）: `question` は run の終端でそのまま届ける。`done` はレビューを
                // 通って `Status::Done` になってから（`on_review_finished` 側）作るので、ここでは作らない。
                // Phase 28: 対話 run の `Question` は `Done` 扱い（上の match）なので、ここでは報告しない
                // （レビューが通れば `on_review_finished` 側が通常の `Done` 報告を作る）。
                if !task_core::is_conversation(&task)
                    && matches!(
                        terminal_report,
                        Some(crate::reports::TerminalReport::Question { .. })
                    )
                    && let Some(question) = terminal_report.as_ref()
                    && let Err(e) = crate::reports::record_run_report(
                        self.store.as_ref(),
                        &task,
                        &run_id,
                        question,
                        None,
                        OffsetDateTime::now_utc(),
                    )
                {
                    tracing::warn!(%task_id, %run_id, error = %e, "failed to record the report for this run");
                }
                // `bad_news` は `Status::Failed` に遷移したときだけ、原因を問わず作る（ワーカー自身の `error`、
                // 供給側失敗が requeue 上限に達した場合のどちらも含む。監査 M-1）。
                if outcome.next == Status::Failed {
                    let bad_news = match &terminal_report {
                        Some(t @ crate::reports::TerminalReport::Error { .. }) => Some(t.clone()),
                        _ => adapter_error_text.as_ref().map(|message| {
                            crate::reports::TerminalReport::Error {
                                message: message.clone(),
                                retryable: true,
                            }
                        }),
                    };
                    if let Some(terminal) = bad_news.as_ref()
                        && let Err(e) = crate::reports::record_run_report(
                            self.store.as_ref(),
                            &task,
                            &run_id,
                            terminal,
                            None,
                            OffsetDateTime::now_utc(),
                        )
                    {
                        tracing::warn!(%task_id, %run_id, error = %e, "failed to record the report for this run");
                    }
                }
                if outcome.next == Status::Reviewing
                    && !self.spawn_review(task_id, run_id, &subject)?
                {
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

    /// ADR-0033 D6（Phase 24）: 結果ファイル（`<artifacts_dir>/result.json`）の `memory` を担当の記憶に追記する。
    /// `[memory]` を設定していない・担当がいない・`memory` が無いときは何もしない。失敗しても run は壊さない。
    fn absorb_memory(&self, task: &Task) {
        let (Some(dir), Some(node_id)) = (&self.config.memory_dir, task.assignee.as_deref()) else {
            return;
        };
        let Some(workspace) = self.task_dir(task) else {
            return;
        };
        // ADR-0036 D2: 結果ファイルはそのタスクの成果物ディレクトリの中。
        let Some(update) = task_worker::read_result_memory(&self.artifacts_dir(task, &workspace))
        else {
            return;
        };
        let today = OffsetDateTime::now_utc().date().to_string();
        let project = task.project_id.map(|p| p.to_string());
        if let Err(e) = MemoryDir::new(dir).append(node_id, project.as_deref(), &update, &today) {
            tracing::warn!(task_id = %task.id, error = %e, "failed to append to the node's memory");
        }
    }

    /// ADR-0038 D1（Phase 41）: 対話 run の結果ファイル（`<artifacts_dir>/result.json`）の
    /// `milestone_proposal` から、その案件に `status = proposed` の途中目標を 1 件作る
    /// （古い提案は `redesigned` に差し替える。判定中の途中目標自身は触らない）。
    /// 対話でない run・案件に属さない run・宣言が無い run では何もしない。失敗しても run は壊さない。
    fn absorb_milestone_proposal(&self, task: &Task) {
        if !task_core::is_conversation(task) {
            return;
        }
        let Some(project_id) = task.project_id else {
            return;
        };
        let Some(workspace) = self.task_dir(task) else {
            return;
        };
        let Some(proposal) =
            task_worker::read_result_milestone_proposal(&self.artifacts_dir(task, &workspace))
        else {
            return;
        };
        match task_ops::milestone_review::record_proposal(
            self.store.as_ref(),
            project_id,
            task.milestone_id,
            &proposal.title,
            &proposal.description,
        ) {
            Ok(Some(milestone)) => tracing::info!(
                task_id = %task.id,
                milestone_id = %milestone.id,
                "the reply proposed the next milestone; recorded as proposed"
            ),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(task_id = %task.id, error = %e, "failed to record the proposed milestone")
            }
        }
    }

    /// ADR-0033 D4（Phase 24 / 監査 M-5）: 対話用タスクの run の終わりを、そのノードの返事として
    /// `messages` に残す。`outcome_str` は `done: <summary>` / `question: <text>` /
    /// `error(...): <message>` のいずれか。`next` は遷移後の状態で、**失敗の返事は `Failed` のときだけ**
    /// 書く（retryable な途中失敗や requeue では、同じ問いに何度も「返事できませんでした」が並ばない）。
    fn record_conversation_reply(
        &self,
        task: &Task,
        run_id: &str,
        outcome_str: &str,
        next: Status,
    ) {
        if !task_core::is_conversation(task) {
            return;
        }
        let mut metadata = None;
        let text = if let Some(summary) = outcome_str.strip_prefix("done: ") {
            let mut text = summary.to_string();
            // ADR-0048 D3（Phase 60b）: CoS が `done` で返ってきたときだけ、結果ファイルの `actions` を
            // 決定的に実行する。実行できなかった action があれば返事に節を足し、実行結果は metadata に残す。
            if let Some(outcome) = self.absorb_console_actions(task, run_id) {
                if let Some(note) = outcome.failure_note() {
                    text.push_str(&note);
                }
                metadata = outcome.to_metadata();
            }
            text
        } else if let Some(question) = outcome_str.strip_prefix("question: ") {
            // 質問は人への問いかけそのものなので、返事としてもそのまま見せる（`approvals` にも 1 行入る）。
            question.to_string()
        } else if next == Status::Failed {
            task_core::failure_reply(outcome_str)
        } else {
            return;
        };
        if let Err(e) = task_ops::conversation::record_reply_with_metadata(
            self.store.as_ref(),
            task,
            run_id,
            &text,
            metadata,
            OffsetDateTime::now_utc(),
        ) {
            tracing::warn!(task_id = %task.id, error = %e, "failed to record the conversation reply");
        }
    }

    /// ADR-0048 D3（Phase 60b）: CoS（根ノード。`OrgKind::Secretary`）の対話 run の結果ファイルの
    /// `actions` を決定的に実行する。CoS 以外の対話・対話でない run・宣言が無い run では何もしない
    /// （`None`）。冪等（`task_ops::actions::execute` が `run_id` を記録し、2 回目は `None`）。
    /// 失敗しても run は壊さない。
    fn absorb_console_actions(
        &self,
        task: &Task,
        run_id: &str,
    ) -> Option<task_ops::actions::ActionsOutcome> {
        if !task_core::is_conversation(task) {
            return None;
        }
        let assignee = task.assignee.as_deref()?;
        let org = self.store.org_list().ok()?;
        let node = org.iter().find(|n| n.id == assignee)?;
        if node.kind != OrgKind::Secretary {
            return None;
        }
        let workspace = self.task_dir(task)?;
        let artifacts_dir = self.artifacts_dir(task, &workspace);
        let parsed = task_worker::read_result_actions(&artifacts_dir);
        if parsed.is_empty() {
            return None;
        }
        // Phase 98（ADR-0018）: `create_task.workspace` がクラスタを指すときに `[[clusters]]` へ照らして
        // 検証するため、既知のクラスタ id を渡す。
        let known_clusters: Vec<String> = self.config.clusters.keys().cloned().collect();
        match task_ops::actions::execute(
            self.store.as_ref(),
            &org,
            &self.config.roles,
            &self.config.genres,
            &known_clusters,
            task,
            run_id,
            &parsed.valid,
            &parsed.malformed,
            OffsetDateTime::now_utc(),
        ) {
            Ok(outcome) => outcome,
            Err(e) => {
                tracing::warn!(task_id = %task.id, %run_id, error = %e, "failed to execute console actions");
                None
            }
        }
    }

    fn on_review_finished(
        &mut self,
        task_id: TaskId,
        run_id: String,
        mut outcome: ReviewOutcome,
    ) -> Result<(), DispatchError> {
        let entry = self.reviewing.remove(&task_id);
        // ADR-0014 D1: Reviewer run の終わりを WorkerFinished{role: reviewer} として残す（判定の適用・延期・破棄のどれでも）。
        let completed_review_run = outcome.reviewer_run.as_ref().map(|r| r.run_id.clone());
        // ADR-0061（Phase 104）: `retries` は Reviewer run には無い概念（対象タスクの `attempts` とは別軸）
        // なので 0 固定。wall time は `ReviewEntry.since` から計算する。
        let review_metrics = entry.as_ref().map(|e| task_core::RunMetrics {
            wall_ms: wall_ms_since(e.since),
            retries: 0,
            peak_context_tokens: None,
            turns: None,
        });
        let mut reviewer_finished = outcome.reviewer_run.take().map(|r| Event::WorkerFinished {
            run_id: r.run_id,
            outcome: r.outcome,
            usage: r.usage,
            role: Some(RunRole::Reviewer),
            metrics: review_metrics,
            end: None,
        });
        let Some(task) = self.store.get(task_id)? else {
            return Ok(());
        };
        // ADR-0054 D1（Phase 67）: この run が Reviewer run を伴っていれば、対象タスクの部署の根ノード
        // （engineering/research/operations）の継続セッション（`kind = lead`）に usage を積む（rollover
        // 判定の材料。turns も 1 進む）。部署が無い・対応しないアダプタでは `node_session_touch` が no-op。
        if let Some(Event::WorkerFinished { usage, .. }) = &reviewer_finished
            && let Some(node_id) = task.assignee.as_deref()
        {
            match self.store.org_list() {
                Ok(org) => {
                    if let Some(department) = task_core::department_of(&org, node_id) {
                        let tokens = usage
                            .as_ref()
                            .map(|u| u.input_tokens.unwrap_or(0) + u.output_tokens.unwrap_or(0))
                            .unwrap_or(0);
                        if let Err(e) = self.store.node_session_touch(
                            &department,
                            task_core::SessionKind::Lead,
                            None,
                            tokens as i64,
                            OffsetDateTime::now_utc(),
                        ) {
                            tracing::warn!(%task_id, error = %e, "failed to record department lead session usage");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(%task_id, error = %e, "failed to list org for department lead session touch");
                }
            }
        }
        if task.status != Status::Reviewing {
            tracing::warn!(%task_id, status = ?task.status, "review result discarded (task no longer reviewing)");
            if let Some(ev) = &reviewer_finished {
                self.store.append_event(task_id, ev)?;
            }
            return Ok(());
        }
        let mut throttled_events = Vec::new();
        if let Some(pf) = outcome.provider_failure.take() {
            // ADR-0054 D2（Phase 113）: `pf.outcome = Some(..)` はプロバイダが分類できた供給側失敗
            // （Throttled/Exhausted/AuthFailed）で、従来どおりプロバイダ/アカウントの cooldown に
            // 報告する。`None` は「reviewer run 自身のインフラ都合の失敗」（is_error の結果・
            // プロセス失敗・resume 拒否など）で、こちらは cooldown の対象にしない
            // （プロバイダ・アカウントの問題ではなく、たまたまこの run が失敗しただけのため）。
            if let Some(classified) = pf.outcome.clone() {
                let review_account = entry.as_ref().and_then(|e| e.account.clone());
                let review_account_adapter = entry.as_ref().and_then(|e| e.account_adapter);
                if let Some(provider) = entry.as_ref().and_then(|e| e.provider.clone()) {
                    // ADR-0024 D4: プール経由の Reviewer run の失敗もプロバイダを cooldown にせず、アカウントに向ける。
                    let policy_outcome = if review_account.is_some() {
                        ProviderOutcome::Ok
                    } else {
                        classified.clone()
                    };
                    self.policy.report(provider.clone(), &policy_outcome);
                    match (&review_account, review_account_adapter) {
                        (Some(acct), Some(adapter)) => self.record_account_failure(
                            adapter,
                            acct,
                            cooldown_reason_name(&classified),
                            &classified,
                        ),
                        _ => {
                            if let Some(ev) = self.provider_throttled_event(
                                &provider,
                                &classified,
                                cooldown_reason_name(&classified),
                            ) {
                                throttled_events.push(ev);
                            }
                        }
                    }
                }
            }
            // ADR-0054 D2（Phase 113）: 分類できた供給側失敗は従来どおり `max_requeues` /
            // `REVIEWER_REQUEUED_PREFIX` で数える。reviewer run 自身のインフラ都合の失敗は、
            // 別のカウンタ・別の上限（`[review] max_reviewer_retries`、既定 3。criterion ごとではなく
            // この reviewing 試行での連続失敗回数だが、Reviewer 条件は同じ run でまとめて判定される
            // ため実質的に criterion ごとの回数と一致する）で数え、プロバイダの cooldown 回数とは
            // 混ぜない。
            let is_infra_failure = pf.outcome.is_none();
            let (deferrals, limit, prefix) = if is_infra_failure {
                (
                    consecutive_reviewer_infra_failures(&self.store.events_for(task_id)?),
                    self.config.max_reviewer_retries,
                    REVIEWER_INFRA_FAILURE_PREFIX,
                )
            } else {
                (
                    consecutive_reviewer_requeues(&self.store.events_for(task_id)?),
                    self.config.max_requeues,
                    REVIEWER_REQUEUED_PREFIX,
                )
            };
            if deferrals < limit {
                // ADR-0010 D5（P-29）/ ADR-0054 D2: Reviewer run の供給側・インフラ失敗は判定しない。
                // reviewing のまま次 tick に回す（attempts を消費しない）。
                self.store.append_event(
                    task_id,
                    &Event::worker_progress(run_id.clone(), format!("{prefix}{}", pf.message)),
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
                tracing::warn!(%task_id, %run_id, reason = %pf.message, infra = is_infra_failure, "reviewer run hit a provider/infra failure; review deferred");
                return Ok(());
            }
            // ADR-0011（P-38）/ ADR-0054 D2: 連続延期・連続インフラ失敗が上限に達したら、未判定の
            // Reviewer 条件を fail として通常どおり判定を適用する（人の承認・command・artifact_exists
            // の結果は `outcome.verdicts` に既に入っているのでそのまま残る）。
            let limit_message = if is_infra_failure {
                format!("reviewer infra failure ×{limit}: {}", pf.message)
            } else {
                format!("requeue limit ({limit}) reached: {}", pf.message)
            };
            tracing::warn!(%task_id, %run_id, reason = %pf.message, limit, infra = is_infra_failure, "reviewer run retry limit reached; failing reviewer criteria");
            if let Some(Event::WorkerFinished {
                outcome: finished_outcome,
                ..
            }) = reviewer_finished.as_mut()
            {
                *finished_outcome = format!("error(retryable=false): {limit_message}");
            }
            for (idx, criterion) in task.acceptance.iter().enumerate() {
                if matches!(criterion.check, Check::Reviewer)
                    && !outcome.verdicts.iter().any(|v| v.criterion_idx == idx)
                {
                    outcome.verdicts.push(Verdict {
                        criterion_idx: idx,
                        pass: false,
                        reason: limit_message.clone(),
                    });
                }
            }
            outcome.verdicts.sort_by_key(|v| v.criterion_idx);
        }
        let all_pass = outcome.all_pass();
        if let Some(old) = self.store.delivery_get(task_id)?
            && old.worker_run == run_id
            && completed_review_run.as_deref() == Some(old.review_run.as_str())
            && old.state == task_core::DeliveryState::Reviewing
            && let Some(verdict) = outcome
                .verdicts
                .iter()
                .find(|v| v.criterion_idx == old.criterion_idx)
        {
            let mut next = old.clone();
            next.decision = Some(all_pass && verdict.pass);
            next.state = if all_pass && verdict.pass {
                task_core::DeliveryState::MergeQueued
            } else {
                task_core::DeliveryState::Blocked
            };
            next.detail = outcome
                .verdicts
                .iter()
                .filter(|v| !v.pass || v.criterion_idx == old.criterion_idx)
                .map(|v| v.reason.clone())
                .collect::<Vec<_>>()
                .join("\n");
            let review_reason = verdict
                .reason
                .strip_prefix(&format!("reviewer({}): ", old.review_run))
                .unwrap_or(&verdict.reason);
            if review_reason.trim_start().starts_with("[needs-human]") {
                next.detail = format!("{}\n{}", review_reason, next.detail);
            }
            self.store.delivery_save(Some(&old), &next)?;
        }

        // ADR-0034 D2（監査 M-1〜M-3）: レビュー不合格の理由（この run が `Status::Failed` に直結した場合の
        // bad_news の材料。`Status::Ready` に戻るだけの途中の失敗では使わない）。
        let review_fail_message = if all_pass {
            None
        } else {
            let reasons: Vec<String> = outcome
                .verdicts
                .iter()
                .filter(|v| !v.pass)
                .map(|v| v.reason.clone())
                .collect();
            Some(reasons.join("; "))
        };
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
                    &Event::worker_progress(
                        run_id.clone(),
                        format!("waiting for {pending} delegated child task(s) before completing"),
                    ),
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
            (true, TaskKind::Plan, Some(mut plan)) => {
                let org = self.store.org_list()?;
                self.fix_plan_for_harness(&task, &mut plan, &org);
                // ADR-0039 D2: 子の作業場所は 明示 > 案件 > 親。
                let project_workspace =
                    task_ops::delegate::project_workspace(self.store.as_ref(), &task)
                        .map_err(ops_to_store)?;
                // ADR-0043 D2: 子のリポジトリは 明示（計画の `repos`）> 親 > 案件の primary。
                let project_repos = task_ops::delegate::project_repos(self.store.as_ref(), &task)
                    .map_err(ops_to_store)?;
                let home = task_core::home_dir();
                let workspace = task_core::WorkspaceContext {
                    project: project_workspace.as_ref(),
                    home: home.as_deref(),
                    repos: &project_repos,
                };
                let children = materialize_logging(
                    &task,
                    &plan,
                    &org,
                    &self.config.roles,
                    &self.config.genres,
                    workspace,
                    OffsetDateTime::now_utc(),
                    &mut |child_id, reason| {
                        tracing::info!(task_id = %task_id, child_id = %child_id, %reason, "workspace downgraded to local (ADR-0062 B2)");
                    },
                );
                let n = children.len();
                let r = self.store.complete_plan(
                    task_id,
                    events,
                    children,
                    self.config.plan_auto_accept,
                );
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
            (true, _, _) => {
                self.store
                    .apply_transition_with_events(task_id, Trigger::ReviewPass, events)
            }
            (false, _, _) => {
                self.store
                    .apply_transition_with_events(task_id, Trigger::ReviewFail, events)
            }
        };
        match result {
            Ok(outcome) => {
                tracing::info!(%task_id, %run_id, all_pass, next = ?outcome.next, attempts = outcome.attempts, "review finished");
                // ADR-0034 D2（監査 M-1〜M-3）: `result` はレビューを通って `Status::Done` になったときだけ作る
                // (ワーカーの「できました」がここで差し戻された分は報告にしない。DESIGN 原則 4)。
                if outcome.next == Status::Done
                    && let Some(review_entry) = entry.as_ref()
                {
                    let terminal = crate::reports::TerminalReport::Done {
                        summary: review_entry.subject.summary.clone(),
                        evidence: crate::reports::format_evidence(&review_entry.subject.evidence),
                    };
                    // ADR-0034 D7: ワーカーが結果ファイルで宣言した `report.kind`（無ければ既定の `result`）。
                    let declared = self.task_dir(&task).and_then(|ws| {
                        task_worker::read_result_report_kind(&self.artifacts_dir(&task, &ws))
                    });
                    if let Err(e) = crate::reports::record_run_report(
                        self.store.as_ref(),
                        &task,
                        &run_id,
                        &terminal,
                        declared.as_deref(),
                        OffsetDateTime::now_utc(),
                    ) {
                        tracing::warn!(%task_id, %run_id, error = %e, "failed to record the report for this run");
                    }
                } else if outcome.next == Status::Failed
                    && let Some(message) = review_fail_message.as_ref()
                {
                    // レビュー不合格が retry を使い切って `Status::Failed` になった場合の bad_news（原因を問わない。監査 M-1）。
                    let terminal = crate::reports::TerminalReport::Error {
                        message: message.clone(),
                        retryable: false,
                    };
                    if let Err(e) = crate::reports::record_run_report(
                        self.store.as_ref(),
                        &task,
                        &run_id,
                        &terminal,
                        None,
                        OffsetDateTime::now_utc(),
                    ) {
                        tracing::warn!(%task_id, %run_id, error = %e, "failed to record the report for this run");
                    }
                }
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
    ///
    /// Phase 45（実機バグ）: ここは親と子で別々のタスクのイベント id を比較するので、`events_for`（タスクごとの
    /// ローカルな `seq`）ではなく `events_for_with_global_ids`（`events` テーブルの全タスク共通の `id`）を
    /// 使わなければならない。`seq` で比較すると、子の方がイベント数が多い（＝ `seq` が大きい）場合に
    /// 「一度扱った失敗」でも毎回「新規」と判定され、同じ質問が繰り返し出る。
    fn newly_failed_delegated_children(
        &self,
        task_id: TaskId,
    ) -> Result<Vec<(Task, Option<String>)>, DispatchError> {
        let events = self.store.events_for_with_global_ids(task_id)?;
        // 直近に子の失敗を扱った時点（グローバル id。イベント id は単調増加）。
        let handled_at = events
            .iter()
            .rev()
            .find_map(|(id, e)| match e {
                Event::Transitioned { reason, .. } if reason == Trigger::ChildFailed.name() => {
                    Some(*id)
                }
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
            let Some(child) = self.store.get(id)? else {
                continue;
            };
            if child.status != Status::Failed {
                continue;
            }
            let child_events = self.store.events_for_with_global_ids(id)?;
            let failed_at = child_events.iter().rev().find_map(|(eid, e)| match e {
                Event::Transitioned {
                    to: Status::Failed, ..
                } => Some(*eid),
                _ => None,
            });
            // 既に扱った失敗（id が前回の child_failed より前）は数えない。
            if failed_at.is_some_and(|at| at <= handled_at) {
                continue;
            }
            let outcome = child_events.iter().rev().find_map(|(_, e)| match e {
                Event::WorkerFinished {
                    outcome,
                    role: None,
                    ..
                } => Some(outcome.clone()),
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
        // ADR-0033 D4 / Phase 28: 対話タスクは委譲できない（子を持たない）ので、そもそも子の失敗は
        // 起きないはずだが、念のため `retry_then_ask` の対象から外す（対話タスクが `blocked` に落ちる
        // 経路を完全に断つ）。
        if task_core::is_conversation(task) {
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
                format!(
                    "{} ({}): {}",
                    child.title,
                    child.id,
                    outcome.as_deref().unwrap_or("(no outcome recorded)")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        // 状態機械と同じ判定（ADR-0021 D1）。ここで分かるのは「やり直せるか」だけ。
        let will_retry = task.attempts < task.budget.max_retries;
        if will_retry {
            events.push(Event::worker_progress(
                run_id,
                format!(
                    "{} delegated child task(s) failed; retrying this task (attempt {}/{}): {listed}",
                    failed.len(),
                    task.attempts + 1,
                    task.budget.max_retries,
                ),
            ));
        } else {
            let text = format!(
                "委譲した子タスクが失敗し、やり直し（max_retries = {}）でも解決しませんでした。どうしますか。\n\
                 失敗した子: {listed}\n\
                 回答するとこのタスクは指示を持って再開します: celerisctl answer {} \"…\"",
                task.budget.max_retries, task.id,
            );
            // Phase 44（実機 2026-09-18）: この質問はディスパッチャ由来（ワーカーの `Question` ではない）だが、
            // Phase 26 と同じく `approvals` にも残す。そうしないと認可画面に出ず、`approval_pending` の
            // Discord 通知も飛ばない（受信箱にだけ出て気づかれない）。
            if let Err(e) = crate::approvals::record_question_approval(
                self.store.as_ref(),
                task,
                &text,
                OffsetDateTime::now_utc(),
            ) {
                tracing::warn!(task_id = %task.id, %run_id, error = %e, "failed to record the approval for the child-failure question");
            }
            events.push(Event::QuestionRaised {
                run_id: run_id.to_string(),
                text,
            });
        }
        match self
            .store
            .apply_transition_with_events(task.id, Trigger::ChildFailed, events)
        {
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
    fn schedule_aggregate_run(
        &mut self,
        task_id: TaskId,
        run_id: &str,
        mut events: Vec<Event>,
    ) -> Result<(), DispatchError> {
        let children = self.store.children(task_id)?.len();
        events.push(Event::worker_progress(
            run_id,
            format!(
                "all {children} delegated child task(s) finished; scheduling the aggregate run"
            ),
        ));
        match self
            .store
            .apply_transition_with_events(task_id, Trigger::Aggregate, events)
        {
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
                (TaskKind::Plan, Some(mut plan)) => {
                    let org = self.store.org_list()?;
                    self.fix_plan_for_harness(&task, &mut plan, &org);
                    // ADR-0039 D2: 子の作業場所は 明示 > 案件 > 親。
                    let project_workspace =
                        task_ops::delegate::project_workspace(self.store.as_ref(), &task)
                            .map_err(ops_to_store)?;
                    // ADR-0043 D2: 子のリポジトリは 明示（計画の `repos`）> 親 > 案件の primary。
                    let project_repos =
                        task_ops::delegate::project_repos(self.store.as_ref(), &task)
                            .map_err(ops_to_store)?;
                    let home = task_core::home_dir();
                    let workspace = task_core::WorkspaceContext {
                        project: project_workspace.as_ref(),
                        home: home.as_deref(),
                        repos: &project_repos,
                    };
                    let children = materialize_logging(
                        &task,
                        &plan,
                        &org,
                        &self.config.roles,
                        &self.config.genres,
                        workspace,
                        OffsetDateTime::now_utc(),
                        &mut |child_id, reason| {
                            tracing::info!(task_id = %task_id, child_id = %child_id, %reason, "workspace downgraded to local (ADR-0062 B2)");
                        },
                    );
                    self.store.complete_plan(
                        task_id,
                        Vec::new(),
                        children,
                        self.config.plan_auto_accept,
                    )
                }
                _ => self.store.apply_transition_with_events(
                    task_id,
                    Trigger::ReviewPass,
                    Vec::new(),
                ),
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

    /// ADR-0070 D5（Phase 116）: lease が期限切れでも、**このインスタンスが持っている run**
    /// （`self.running` に entry がある）のプロセスがまだ生きていれば（`process_group::group_alive`）、
    /// reclaim せず lease を延長して続行する（DB busy で数 tick 更新できなかっただけ、という実際に
    /// 起きた事故〈PROGRESS Phase 116〉をここで救う）。延長にも失敗したら次の tick に持ち越す。
    /// 死んでいる（またはこのインスタンスの管理外）ときだけ、ADR-0070 D3 の分岐
    /// （`InfraRequeue` でバックオフ再試行、`max_infra_retries` 到達で打ち切り）に乗せる。
    /// `Trigger::LeaseExpired`（無条件に attempts を消費する）はもう使わない。
    fn reclaim_expired_leases(&mut self) -> Result<usize, DispatchError> {
        let now = OffsetDateTime::now_utc();
        let mut count = 0;
        for task in self.store.list(Some(Status::Running))? {
            // ADR-0041 D5: 面倒を見ないタスクのリースは奪わない（verify は本番のコピーの行を書き換えない）。
            if !self.is_eligible(&task) {
                continue;
            }
            let Some(lease) = &task.lease else { continue };
            if lease.expires_at > now {
                continue;
            }
            if let Some(entry) = self.running.get(&task.id)
                && task_worker::process_group::group_alive(&entry.run_id)
            {
                let ttl = Duration::from_secs(task.budget.max_wall_secs) + self.config.lease_grace;
                match self.store.renew_lease(task.id, &entry.run_id, ttl) {
                    Ok(true) => {
                        tracing::warn!(task_id = %task.id, run_id = %entry.run_id, "lease expired but the run's process is still alive; extended instead of reclaiming (ADR-0070 D5)");
                        continue;
                    }
                    Ok(false) => {
                        // 一致しない（レース。他の何かがリースを動かした）。下の通常の reclaim へ。
                    }
                    Err(e) => {
                        tracing::warn!(task_id = %task.id, run_id = %entry.run_id, error = %e, "failed to extend the lease for a still-alive run; will retry reclaiming next tick");
                        continue;
                    }
                }
            }
            // ADR-0061（Phase 104）: `entry` を消費する前に `since`（wall time 計算用）を取っておく。
            let mut run_since: Option<OffsetDateTime> = None;
            if let Some(entry) = self.running.remove(&task.id) {
                run_since = Some(entry.since);
                // ADR-0044 Phase 53 追記: リース喪失も同じ止め方（プロセスグループごと。
                // コンテナで走っていればラベル越しにも同じ 2 段を送る）。
                self.stop_run(&entry.run_id, entry.handle, entry.container);
            }
            let metrics = run_since.map(|since| task_core::RunMetrics {
                wall_ms: wall_ms_since(since),
                retries: task.attempts,
                peak_context_tokens: None,
                turns: None,
            });
            let infra_n = consecutive_infra_requeues(&self.store.events_for(task.id)?) + 1;
            let (trigger, outcome_text) = if infra_n <= self.config.max_infra_retries {
                (
                    Trigger::InfraRequeue,
                    format!(
                        "infra_requeue: lease expired (run_id={})",
                        lease.worker_run_id
                    ),
                )
            } else {
                (
                    Trigger::WorkerError { retryable: false },
                    format!(
                        "{INFRA_FAILURE_MARKER}{infra_n}: lease expired (run_id={})",
                        lease.worker_run_id
                    ),
                )
            };
            let finished = Event::WorkerFinished {
                run_id: lease.worker_run_id.clone(),
                outcome: outcome_text,
                usage: None,
                role: None,
                metrics,
                end: Some(task_core::RunEnd::HarnessError {
                    class: task_core::HarnessErrorClass::LeaseExpired,
                }),
            };
            match self
                .store
                .apply_transition_with_events(task.id, trigger, vec![finished])
            {
                Ok(outcome) => {
                    tracing::warn!(task_id = %task.id, run_id = %lease.worker_run_id, next = ?outcome.next, attempts = outcome.attempts, "lease expired; reclaimed");
                    if matches!(trigger, Trigger::InfraRequeue) {
                        let until = now + infra_backoff_delay(infra_n);
                        self.infra_backoff.insert(task.id, until);
                    }
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

    /// ADR-0044 §5 Phase 53 追記（Phase 55）: **run の止め方はこれ 1 つ**。
    ///
    /// `cancel` / 人のコメントによる割り込み（`Interrupt`）/ 実時間・無入力のタイムアウト /
    /// リース喪失 / drain タイムアウトのどれも、ここを通って
    /// **ワーカーのプロセスグループに SIGTERM → `kill_grace_secs` → SIGKILL** を送る
    /// （`task_worker::kill_tree`）。ハーネスが起こした孫（`cargo test`、`node`、シェル）まで届く。
    /// タイムアウトだけは `task_worker::subprocess` の中でも同じ手順を踏むが、そちらが先に終わって
    /// いれば登録が無いので、ここは何もしない（二重には送らない）。
    ///
    /// tokio の `JoinHandle::abort()` は**従来どおり即座に**行う（run の記録を止めるための帳簿）。
    ///
    /// Phase 55/56 の合流（ADR-0044 P55-4 / ADR-0043 P56-7）: `container` が `Some`（= ADR-0043 D3 で
    /// コンテナ実行に倒した run）なら、`killpg` と**同じ 2 段**を
    /// `--label celeris.task=<task_id>` 越しにも送る（`<runtime> kill --signal TERM` → `grace` →
    /// `<runtime> rm -f`）。`killpg` は `<runtime> run` のクライアントにしか届かず、
    /// コンテナの中は別の PID 名前空間なので、これが無いと中のハーネスが生き残る。
    fn stop_run(
        &self,
        run_id: &str,
        handle: JoinHandle<()>,
        container: Option<Arc<dyn task_worker::ContainerStopper>>,
    ) {
        task_worker::kill_tree_with(run_id, self.config.kill_grace, container);
        handle.abort();
    }

    /// レビュー側（判定コマンドの run と Reviewer run）の停止。run は 2 本ありうるので両方に送る。
    fn stop_review(&self, entry: ReviewEntry) {
        task_worker::kill_tree(&entry.run_id, self.config.kill_grace);
        if let Some(review_run_id) = &entry.review_run_id {
            task_worker::kill_tree(review_run_id, self.config.kill_grace);
        }
        entry.handle.abort();
    }

    /// ADR-0002 D9: ストア上で `running` でなくなった（cancel / ADR-0044 D2 の割り込み等）run を
    /// 強制終了する。打ち切ったタスクは `just_aborted` に入れ、**この tick では dispatch し直さない**。
    fn abort_stale_runs(&mut self) -> Result<(), DispatchError> {
        self.just_aborted.clear();
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
                // ADR-0044 Phase 53 追記: プロセスグループごと止める（孫まで。コンテナならその中も）。
                self.stop_run(&entry.run_id, entry.handle, entry.container);
                self.just_aborted.insert(id);
            }
        }
        // レビュー中に cancel されたタスクの判定（Reviewer run を含む）も中断する。
        let ids: Vec<TaskId> = self.reviewing.keys().copied().collect();
        for id in ids {
            let still_reviewing =
                matches!(self.store.get(id)?, Some(t) if t.status == Status::Reviewing);
            if !still_reviewing && let Some(entry) = self.reviewing.remove(&id) {
                tracing::warn!(task_id = %id, "aborting review (task no longer reviewing)");
                self.stop_review(entry);
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
            if self.reviewing.contains_key(&task.id)
                || self.awaiting_children.contains_key(&task.id)
            {
                continue;
            }
            // ADR-0041 D5: 面倒を見ないタスクのレビューは拾わない（verify は他人のタスクを判定しない）。
            if !self.is_eligible(&task) {
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
        self.running.len()
            + self
                .reviewing
                .values()
                .filter(|e| e.provider.is_some())
                .count()
    }

    fn provider_in_use(&self, provider: &ProviderId) -> usize {
        self.running
            .values()
            .filter(|e| &e.provider == provider)
            .count()
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

    /// ADR-0052 D1: このタスクが「知識整理 run（`langmem` 固定）」で、かつ接続先に届かないなら、
    /// 倒す理由（人が読む 1 行）を返す。それ以外は `None`（＝従来どおり `langmem` で走らせる）。
    ///
    /// 検査は `base_url` ごとに 60 秒キャッシュするので、tick ごとには叩かない。
    fn knowledge_fallback_reason(&mut self, task: &Task, now: Instant) -> Option<String> {
        if task.worker_hint.adapter.as_deref() != Some(task_worker::LangMemAdapter::ID) {
            return None;
        }
        if support_kind(task) != Some("knowledge") {
            return None;
        }
        // `fallback = false`（または `knowledge` ハーネスが無い）なら倒さない。
        self.config.knowledge.fallback_tier?;
        self.knowledge_reachability(now)
            .should_fall_back()
            .map(str::to_string)
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
            // ADR-0044 D2: この tick で打ち切ったばかりの run と同じ worktree に、すぐ次の run を
            // 入れない（孫プロセスが片付く猶予を 1 tick 置く）。
            if self.just_aborted.contains(&task.id) {
                continue;
            }
            // ADR-0041 D5: verify モードは `genre = "smoke"` の煙試験だけを起こす（他は ready のまま）。
            if !self.is_eligible(&task) {
                continue;
            }
            // ADR-0046 D5（Phase 59）: 担当が決まっていないタスクは dispatch の前に matching で決める
            // （計画 run の子、人が作ったタスク、Console から作られたタスクが全部ここを通る）。
            let mut task = match self.assign_if_needed(task)? {
                Some(task) => task,
                // 候補が無くて `blocked` にした（人に聞いた）。この tick では dispatch しない。
                None => continue,
            };
            // ADR-0010 D6（P-3）: ready に入った時刻（DB の updated_at）からのバックオフ。
            if task.attempts > 0 {
                let delay = retry_backoff(
                    self.config.retry_backoff_base,
                    self.config.retry_backoff_max,
                    task.attempts,
                );
                if OffsetDateTime::now_utc() < task.updated_at + delay {
                    tracing::debug!(task_id = %task.id, attempts = task.attempts, delay_ms = delay.as_millis() as u64, "retry backoff; not dispatching yet");
                    continue;
                }
            }
            // ADR-0070 D3（Phase 116）: インフラ都合の再試行のバックオフ（`self.infra_backoff`。
            // `task.attempts` に依らない別軸。上のバックオフとは独立にゲートする）。期限を過ぎたら
            // このタスクへのゲートは外す（次に infra 失敗すればまた立て直す）。
            if let Some(until) = self.infra_backoff.get(&task.id).copied() {
                if OffsetDateTime::now_utc() < until {
                    tracing::debug!(task_id = %task.id, %until, "infra backoff; not dispatching yet");
                    continue;
                }
                self.infra_backoff.remove(&task.id);
            }
            // ADR-0018: リモート実行のタスクは、クラスタの設定・cooldown・並列度・多重接続を先に確かめる。
            // ADR-0062 B1（Phase 107）: `cluster_of` が `None` の理由を分ける。(a) 設定に無いクラスタ
            // → 従来どおり `unroutable`（人が設定を直すまで進まない）。(b) 担当が `cluster:<id>` を
            // 持たない → `blocked` にして人に質問を 1 件作る（設定の問題ではなく担当の問題なので、
            // 「no such cluster in the config」という誤解を招く文言は出さない）。
            let cluster = match self.resolve_cluster(&task) {
                ClusterResolution::Local => None,
                ClusterResolution::Resolved(spec, path, mode) => Some((spec, path, mode)),
                ClusterResolution::NotConfigured => {
                    if self.warned_unroutable.insert(task.id) {
                        let cluster_id = match &task.workspace {
                            WorkspaceSpec::Remote { cluster, .. } => cluster.clone(),
                            WorkspaceSpec::Local { .. } => String::new(),
                        };
                        tracing::warn!(task_id = %task.id, cluster = %cluster_id, "no such cluster in the config; task left ready");
                    }
                    self.unroutable.insert(task.id);
                    continue;
                }
                ClusterResolution::AssigneeLacksTool { cluster } => {
                    self.block_task_missing_cluster_tool(&task, &cluster)?;
                    continue;
                }
            };
            if let Some((spec, _, _)) = &cluster {
                if self
                    .cluster_cooldown
                    .get(&spec.id)
                    .is_some_and(|until| *until > now)
                {
                    // ADR-0018 D2: 人がログインするまで進まないので、待ち対象には数えない（`--until-idle` を止めない）。
                    self.cluster_waiting.insert(task.id);
                    continue;
                }
                if self.cluster_in_use(&spec.id) >= spec.concurrency {
                    continue;
                }
                // この tick の `refresh_cluster_liveness` の結果を使う（1 tick に 1 回だけ `ssh -O check` を呼ぶ）。
                let alive = self
                    .cluster_connected
                    .get(&spec.id)
                    .copied()
                    .unwrap_or(false);
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
                                format!(
                                    "no ssh ControlMaster connection to {} (host {})",
                                    spec.id, spec.host
                                ),
                            )?;
                            self.cluster_waiting.insert(task.id);
                            continue;
                        }
                    }
                }
            }
            let dir_started = Instant::now();
            // ADR-0041 D1 / ADR-0043 D2: ローカルの作業場所（1 つ以上のリポジトリ）を用意する
            // （`dir` はその親 = `runs/` `artifacts/` の置き場）。
            let worktree = self.task_workspaces_for(&task);
            let dir = match &worktree {
                Some(ws) => ws.task_dir.clone(),
                None => match self.task_dir(&task) {
                    Some(d) => d,
                    None => {
                        tracing::warn!(task_id = %task.id, "cannot resolve the workspace directory; task left ready");
                        continue;
                    }
                },
            };
            log_slow_step("task_dir", dir_started);
            // ADR-0052 D1 / D2（Phase 64）: 知識整理 run は dispatch の直前に `langmem` の接続先へ
            // `GET /models` を当て、届かなければ tier `cheap` の**汎用**ハーネスへ倒す
            // （`worker_hint.adapter` を外すだけ ＝ ADR-0049 の選び方にそのまま乗る）。LLM は呼ばない。
            let fallback_reason = self.knowledge_fallback_reason(&task, now);
            if let Some(reason) = &fallback_reason
                && let Some(tier) = self.config.knowledge.fallback_tier
            {
                task.worker_hint.adapter = None;
                task.worker_hint.tier = tier;
                task.budget.max_turns = KNOWLEDGE_FALLBACK_MAX_TURNS;
                task.budget.max_wall_secs = KNOWLEDGE_FALLBACK_MAX_WALL_SECS;
                tracing::info!(task_id = %task.id, %reason, ?tier, "knowledge: falling back to a generic harness");
            }
            // ADR-0069 D3 / D6（Phase 114）: `routing` を持つ execute タスクは、lane を決定的な policy
            // （TaskFeatures → 規則表 → 組織の天井）とリトライのエスカレーションで決める。人の明示・
            // System の tier はそのまま（記録だけ）。残量による調整はこの後の `select_tier`（別の層）。
            let lane_decision = self.decide_lane(&task)?;
            if let Some(decision) = &lane_decision {
                task.worker_hint.tier = decision.lane;
            }
            // ADR-0054 Phase 67c: CoS の対話 run だけ、継続セッションの (adapter, account) に留まれるかを
            // 先に試す（`run_extras` の `is_cos_conversation` と同じ判定を select_provider より前に
            // 軽く行う。継続セッションを見つけてから選ぶのでないと、ADR-0049 ランキングが先に別の
            // アダプタ・アカウントへ倒れてしまう）。
            let sticky_session = self.cos_conversation_session(&task)?;
            let Some((adapter_id, provider_id, selected_account)) = self.select_provider(
                &task.worker_hint,
                now,
                task.id,
                &mut full,
                sticky_session.as_ref(),
            ) else {
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
                Some((account_adapter, account_id)) => {
                    match self.adapter_for_account(&base_adapter, *account_adapter, account_id) {
                        Some(a) => a,
                        None => {
                            tracing::warn!(task_id = %task.id, provider = %provider_id, account_id, "adapter does not support account pools (with_env returned None); skipping this tick");
                            continue;
                        }
                    }
                }
                None => base_adapter,
            };
            let remaining = selected_account.as_ref().and_then(|(kind, id)| {
                let book = self.account_book(*kind)?;
                let book = book.lock().ok()?;
                let observation = book.state(id)?.usage.as_ref()?;
                crate::accounts::measured_remaining(observation, (self.now_unix_fn)())
            });
            let (tier, routing_reason) =
                match task_core::model_routing::select_tier(task.worker_hint.tier, remaining) {
                    Ok(decision) => decision,
                    Err(_) => continue, // quota refresh will make this task eligible again
                };
            // A legacy provider has no tier mapping: keep its historical behavior.
            if adapter
                .model_for_tier(task.worker_hint.tier)
                .ok()
                .flatten()
                .is_some()
            {
                task.worker_hint.tier = tier;
            }
            let resolved_model = match adapter.model_for_tier(task.worker_hint.tier) {
                Ok(model) => model,
                Err(reason) => {
                    self.store.apply_transition_with_events(
                        task.id,
                        Trigger::Unroutable,
                        vec![Event::worker_progress(
                            "routing",
                            format!("model routing blocked: {reason}"),
                        )],
                    )?;
                    continue;
                }
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
            let model = resolved_model
                .or_else(|| self.models.get(&provider_id).cloned())
                .unwrap_or_default();
            let event_started = Instant::now();
            self.store.append_event(
                task.id,
                &Event::WorkerStarted {
                    run_id: run_id.clone(),
                    adapter: adapter_id.clone(),
                    model: model.clone(),
                    provider: Some(provider_id.clone()),
                    // ADR-0024 D4: `account_pool` のプロバイダで選んだアカウント（プールを使わなければ `None`）。
                    account: account.clone(),
                    role: None,
                    task_role: task.role.clone(),
                },
            )?;
            // ADR-0069 D5: この run の routing の監査記録（担当・harness・lane・model・features・規則）。
            if let Some(mut decision) = lane_decision {
                if task_core::model_policy::lane_rank(task.worker_hint.tier)
                    < task_core::model_policy::lane_rank(decision.lane)
                {
                    decision.reasons.push(format!(
                        "quota layer lowered lane {:?} -> {:?} (budget guard)",
                        decision.lane, task.worker_hint.tier
                    ));
                }
                let record = task_core::RoutingRecord {
                    org_node: task.assignee.clone(),
                    harness: task.genre.clone(),
                    resolution: task_core::model_routing::LaneResolution {
                        lane: Some(task.worker_hint.tier),
                        adapter: adapter_id.clone(),
                        provider: Some(provider_id.clone()),
                        account: account.clone(),
                        model_id: model,
                        // ADR-0069 Phase 118 D1: 監査記録は「設定した」値ではなく「実際に CLI へ
                        // 渡った」値を残す（対応しないアダプタでは `None` になる）。
                        reasoning_effort: adapter
                            .reasoning_effort_for_tier(task.worker_hint.tier)
                            .filter(|_| adapter.supports_reasoning_effort()),
                    },
                    quota_reason: Some(routing_reason.clone()),
                    decision,
                    work_unit_id: None,
                };
                self.store.append_event(
                    task.id,
                    &Event::RoutingDecided {
                        run_id: run_id.clone(),
                        record: Box::new(record),
                    },
                )?;
            }
            if matches!(adapter_id.as_str(), "claude-code" | "codex") {
                self.store.append_event(task.id, &Event::worker_progress(&run_id,
                    format!("model routing: {routing_reason}; execution tier={:?}; provider={provider_id}", task.worker_hint.tier)))?;
            }
            // ADR-0052 D1: 検査の結果を進行（`status`）として残す（run が始まってから 1 行だけ）。
            let knowledge_fallback = match &fallback_reason {
                Some(reason) => {
                    self.store.append_event(
                        task.id,
                        &Event::worker_progress_with(
                            &run_id,
                            format!(
                                "langmem の接続先に届かない（{reason}）。cheap のハーネスに倒す（{adapter_id}）"
                            ),
                            task_core::ProgressFields::of(task_core::ProgressKind::Status),
                        ),
                    )?;
                    Some(KnowledgeFallbackRun {
                        adapter: adapter_id.clone(),
                        instructions: task_worker::knowledge_fallback_instructions(
                            KNOWLEDGE_CANDIDATES_REL,
                        ),
                        budget: task.budget,
                    })
                }
                None => None,
            };
            log_slow_step("append_worker_started", event_started);
            let limits = RunLimits {
                wall_clock: wall,
                idle_timeout: self.config.idle_timeout,
                kill_grace: self.config.kill_grace,
            };
            tracing::info!(task_id = %task.id, %run_id, adapter = %adapter_id, provider = %provider_id, account = account.as_deref(), "dispatching");
            let remote = cluster
                .as_ref()
                .map(|(spec, path, mode)| spec.ssh_settings(path, task.id, *mode));
            // ADR-0043 D3（Phase 56）: ホストか、コンテナか、runtime が無くて `blocked` か。
            let container =
                self.container_decision(&task, worktree.as_ref(), &adapter_id, remote.is_some());
            // Phase 55/56 の合流: コンテナで走らせるなら、止めるための口（runtime の実行ファイルと
            // `--label celeris.task=<task_id>`）を覚えておく（ADR-0044 P55-4 / ADR-0043 P56-7）。
            let container_stop: Option<Arc<dyn task_worker::ContainerStopper>> = match &container {
                ContainerDecision::Container(run) => {
                    Some(Arc::new(task_worker::ContainerStop::of(&run.plan)))
                }
                ContainerDecision::Host | ContainerDecision::Unavailable { .. } => None,
            };
            let mut extras =
                self.run_extras(&task, worktree.as_ref(), account.as_deref(), &adapter_id)?;
            // ADR-0056 D3（Phase 79）: mount 名にあったが KB に見つからなかった skill を `status` の
            // 進行イベントで 1 行ずつ報告する（run は落とさない）。
            for name in &extras.missing_skills {
                self.store.append_event(
                    task.id,
                    &Event::worker_progress_with(
                        &run_id,
                        format!("skill {name} not found"),
                        task_core::ProgressFields::of(task_core::ProgressKind::Status),
                    ),
                )?;
            }
            // ADR-0052 D2: フォールバックの前置き（LangMem に渡しているのと同じ抽出の指示 + 出力契約）を
            // 役割の指示文として載せる。依頼文（`maintenance_objective`）は `task.objective` のまま。
            if let Some(fallback) = &knowledge_fallback {
                extras.role = Some(RoleContext {
                    id: task
                        .role
                        .clone()
                        .unwrap_or_else(|| task_core::BUILTIN_KNOWLEDGE.to_string()),
                    instructions: fallback.instructions.clone(),
                });
                extras.knowledge_fallback = knowledge_fallback.clone();
            }
            // ADR-0043 D2: 中止されたときに片付けられるよう、この run で使う作業場所を覚えておく。
            if let Some(ws) = &worktree {
                self.task_workspaces.insert(task.id, ws.clone());
            }
            let handle = self.spawn_worker(
                task.id,
                task.worker_hint.tier,
                run_id.clone(),
                provider_id.clone(),
                account.clone(),
                account_adapter,
                adapter,
                dir,
                limits,
                remote,
                worktree,
                extras,
                container,
            );
            self.running.insert(
                task.id,
                RunEntry {
                    run_id,
                    provider: provider_id,
                    handle,
                    since: OffsetDateTime::now_utc(),
                    cluster: cluster.map(|(spec, ..)| spec.id),
                    account,
                    account_adapter,
                    container: container_stop,
                },
            );
            dispatched += 1;
        }
        Ok(dispatched)
    }

    /// ADR-0016 D1 / D3, ADR-0027 D1: run 開始時にワーカーへ渡す役割の指示文、委譲できる run なら使える
    /// 分野の一覧、集約 run なら子の要約。
    fn run_extras(
        &self,
        task: &Task,
        worktree: Option<&task_worker::TaskWorkspaces>,
        // ADR-0054 D1（Phase 67）: 選んだアカウントとアダプタ id。CoS の対話・部門長のレビュー run の
        // 継続セッション（`session` / `session_diff`）を決めるのに要る。他の全ての値の計算には使わない。
        account: Option<&str>,
        adapter_id: &str,
    ) -> Result<RunExtras, DispatchError> {
        let role = task.role.as_deref().map(|id| RoleContext {
            id: id.to_string(),
            instructions: RoleSpec::find(&self.config.roles, id)
                .and_then(|r| r.instructions.clone())
                .unwrap_or_default(),
        });
        // ADR-0027 D1 / ADR-0028 D3: 委譲の指示文を出す run（Execute/Approval）と、子の分野を選べる
        // Plan run（`build_plan_prompt` も同じ節を出す）にだけ使える分野の一覧を渡す
        // （プロンプト側の条件と同じ。`claude_code::build_prompt` 参照）。
        // ADR-0033 D4 / Phase 28: 対話 run は委譲できないので渡さない（`delegate.json` を書かせない）。
        let is_conv = task_core::is_conversation(task);
        let available_genres = if !is_conv
            && matches!(
                task.kind,
                TaskKind::Execute | TaskKind::Approval | TaskKind::Plan
            ) {
            self.config
                .genres
                .iter()
                .map(|g| GenreContext::from_spec(g, &self.config.roles))
                .collect()
        } else {
            Vec::new()
        };
        // ADR-0033 D4 / D6（Phase 24）: この run をする「人」（担当のノード）、その記憶、この案件での
        // 直近のやり取り、そして分解・委譲できる run には組織図。全てストアとファイルの読み取りだけで、
        // LLM は使わない（DESIGN 原則 1）。
        let org = if task.assignee.is_some() || !available_genres.is_empty() {
            self.store.org_list()?
        } else {
            Vec::new()
        };
        let assigned = task
            .assignee
            .as_deref()
            .and_then(|id| org.iter().find(|n| n.id == id));
        let node = assigned.map(|n| NodeContext {
            id: n.id.clone(),
            name: n.name.clone(),
            brief: n.brief.clone(),
        });
        // ADR-0033 D5（Phase 26）: 担当がいる run にだけ、その担当宛て + 全員向けの永続の認可を注入する
        // （担当がいない run に、たまたま同じ id を持つ他ノード宛ての規則を混ぜないため。memory/conversation
        // と同じ条件）。
        let standing_rules = match assigned {
            Some(n) => self
                .store
                .standing_rule_list(Some(&n.id))?
                .into_iter()
                .map(|r| r.rule)
                .collect(),
            None => Vec::new(),
        };
        let memory = match (&self.config.memory_dir, assigned) {
            (Some(dir), Some(n)) => Some(
                MemoryDir::new(dir).load(&n.id, task.project_id.map(|p| p.to_string()).as_deref()),
            ),
            _ => None,
        };
        let conversation = match assigned {
            Some(n) => {
                let mut turns: Vec<ConversationTurn> = self
                    .store
                    .message_list(
                        &n.id,
                        task.project_id,
                        task_ops::conversation::CONVERSATION_HISTORY,
                    )?
                    .iter()
                    .map(|m| ConversationTurn {
                        role: m.role,
                        text: m.text.clone(),
                    })
                    .collect();
                // 監査 L-6: 今回の本文（`objective`）と同じ最後の `user` の行は落とす（二重に載せない）。
                if let Some(last) = turns.last()
                    && last.role == task_core::MessageRole::User
                    && last.text == task.objective
                {
                    turns.pop();
                }
                turns
            }
            None => Vec::new(),
        };
        // ADR-0033 D4 / Phase 28: 対話 run にだけ、相手が秘書（ADR-0046 D6 の CoS）かそれ以外かを渡す
        // （`preamble` が「作業を始めるな、返事だけ書け」の指示文を出し分けるためだけの印。担当が組織に
        // 無ければ CoS 以外扱いにする。判定は決定的で LLM は使わない）。
        let conversation_addressee = if is_conv {
            Some(match assigned {
                Some(n) if n.kind == OrgKind::Secretary => ConversationAddressee::Secretary,
                _ => ConversationAddressee::Other,
            })
        } else {
            None
        };
        // 分解・委譲できる run（`available_genres` を渡す run と同じ条件）にだけ組織図を渡す。
        // ADR-0046 D6: **CoS の対話 run** にも渡す（誰が何をできるかを見せる。人選はしない）。
        let is_cos_conversation = conversation_addressee == Some(ConversationAddressee::Secretary);
        // ADR-0054 D1（Phase 67）: CoS の対話は継続セッション（全体で 1 本。`project_id` は常に
        // `None`。案件を開いているときも前置きに案件の文脈を足すだけでセッションは同じ）。対応しない
        // アダプタ（claude-code/codex/acp 以外）では継続しない（`None` のまま。前置きは Phase 66 までと
        // バイト単位で同じ）。organization / node / memory / conversation はこの直後で、継続中
        // （`resume = true`）なら差分だけにする（毎回流し直さない。D1「前置きは継続中は差分だけ」）。
        let (session, session_diff) = if is_cos_conversation {
            self.resolve_node_session(
                task_core::COS_ID,
                task_core::SessionKind::Conversation,
                None,
                adapter_id,
                account,
                OffsetDateTime::now_utc(),
            )?
        } else {
            (None, Vec::new())
        };
        // ADR-0054 D1: 継続中（`resume = true`）の CoS 対話 run だけ、この後の brief・記憶・組織の一覧・
        // 直近のやり取り・進行中の案件を差分に置き換える（省く）。新規セッション（`resume = false`。
        // rollover・アカウント変更・resume 失敗の後を含む）では、これまでどおり全量を渡す。
        let continuing = is_cos_conversation && session.as_ref().is_some_and(|s| s.resume);
        let node = if continuing { None } else { node };
        let memory = if continuing { None } else { memory };
        let conversation = if continuing { Vec::new() } else { conversation };
        let organization = if (available_genres.is_empty() && !is_cos_conversation) || continuing {
            Vec::new()
        } else {
            org.iter()
                .map(|n| OrgNodeContext::with_profile(n, &task_core::resolve_profile(&org, &n.id)))
                .collect()
        };
        // ADR-0046 D1（Phase 59）: 担当ノードの実効 profile（根→葉の merge ＋ タスクの上書き）。
        // profile を 1 つも書いていない組織では `None`（前置きは Phase 58 までとバイト単位で同じ）。
        let profile = assigned.and_then(|n| {
            let effective = task_core::resolve_profile(&org, &n.id);
            if effective.is_trivial() {
                None
            } else {
                Some(effective.with_task(task))
            }
        });
        // ADR-0046 D4（Phase 59）: 既定（`production`）の進め方は渡さない（前置きを変えない）。
        let mode = if task.mode == task_core::TaskMode::Production {
            None
        } else {
            Some(task.mode)
        };
        // Phase 30（ADR-0033 D4 追記）: 対話は常に対話用分野で走る（`task.genre`）。その人が自分の仕事で
        // 何を使うかを知って答えられるように、対話 run にだけ、担当ノード**自身**の分野
        // （`node.genre`。対話用分野とは別物）を「仕事で使う道具」として渡す。決定的（`[[genres]]` の
        // manifest を引くだけ）。
        let work_genre = if is_conv {
            assigned
                .and_then(|n| n.genre.as_deref())
                .and_then(|id| GenreSpec::find(&self.config.genres, id))
                .map(|g| GenreContext::from_spec(g, &self.config.roles))
        } else {
            None
        };
        // Phase 33（ADR-0033 D4 追記。実機の事故 — 担当が自分の直近の失敗を知らずに「対象タスク ID が
        // 必要です」と聞き返した — の再発防止）: 対話 run にだけ、担当の直近の仕事を渡す。
        let recent_work = if is_conv {
            match assigned {
                Some(n) => self.recent_work_of(&n.id, task.project_id)?,
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        // Phase 41（ADR-0038 D1）: 途中目標レビューの対話 run にだけ、その途中目標とそこまでの仕事の成果を
        // 渡す（集めるのは決定的: ストアのタスク・イベントと成果物ファイルを読むだけ）。
        let milestone_review = match task_core::milestone_review_of(task) {
            Some(milestone_id) => self.milestone_review_of(task.project_id, milestone_id)?,
            None => None,
        };
        // Phase 43（ADR-0039 D3）: 案件が作業場所を決めていれば、その場所を前置きに出す（決定的:
        // `projects.workspace` を引いて 1 行にするだけ）。対話 run（秘書との会話・途中目標のレビュー）には
        // 出さない（会話は編集をしないので、人のリポジトリの中で走らせる理由が無い。ADR-0039 D2）。
        let mut workspace_note = match conversation_addressee {
            Some(_) => None,
            None => task_ops::delegate::project_workspace(self.store.as_ref(), task)
                .map_err(ops_to_store)?
                .as_ref()
                .map(task_worker::preamble::workspace_note),
        };
        // ADR-0041 D1 / ADR-0043 D2 / D8: 作業ツリーを切る run には、リポジトリ一覧（ブランチ・base）、
        // 検査コマンド、成果物の置き場を足す。
        if conversation_addressee.is_none()
            && let Some(ws) = worktree
        {
            let mut line = task_worker::preamble::repos_note(&self.repo_notes(ws));
            // ADR-0066 D1（Phase 110b）: 共有ビルドキャッシュが有効で、かつ git のリポジトリが
            // 1 つでもあれば「`target/` は共有キャッシュにある」の 1 行を足す。
            if self.config.shared_build_cache && ws.repos.iter().any(|r| r.is_git()) {
                line.push_str(task_worker::preamble::shared_build_cache_note());
            }
            workspace_note = Some(match workspace_note {
                Some(note) => format!("{note}\n{line}"),
                None => line,
            });
        }
        // ADR-0043 D2: 計画 run には**案件のリポジトリの一覧**（名前 / 種類 / 説明）を渡す。
        // プランナーは子タスクごとに `repos: ["benchfs"]` と名前で指定する。
        if task.kind == TaskKind::Plan && conversation_addressee.is_none() {
            let listing =
                task_worker::preamble::project_repos_note(&self.project_repo_notes(task)?);
            if !listing.is_empty() {
                workspace_note = Some(match workspace_note {
                    Some(note) => format!("{note}\n{listing}"),
                    None => listing,
                });
            }
        }
        let events = self.store.events_for(task.id)?;
        // 集約 run（ADR-0016 D3）と、子の失敗によるやり直し run（ADR-0021 D1）は、子の結果を見て判断する。
        let children = if (task.aggregate && has_aggregate_transition(&events))
            || has_child_failed_transition(&events)
        {
            let mut out = Vec::new();
            for child in self.store.children(task.id)? {
                if child.kind == TaskKind::Approval {
                    continue;
                }
                let child_events = self.store.events_for(child.id)?;
                let outcome = child_events.iter().rev().find_map(|(_, e)| match e {
                    Event::WorkerFinished {
                        outcome,
                        role: None,
                        ..
                    } => Some(outcome.clone()),
                    _ => None,
                });
                let artifacts = child_events
                    .iter()
                    .filter_map(|(_, e)| match e {
                        Event::ArtifactProduced { artifact, .. } => Some(artifact.clone()),
                        _ => None,
                    })
                    .collect();
                // ADR-0041 D1: 子は親の作業場所を継ぐので、子ごとに別の worktree になる。親が成果を
                // 統合するときは子のブランチを merge する（統合は LLM の仕事）。
                let child_worktree = self.task_workspaces_for(&child);
                out.push(ChildSummary {
                    id: child.id,
                    title: child.title.clone(),
                    role: child.role.clone(),
                    status: child.status,
                    outcome,
                    artifacts,
                    workspace: child_worktree
                        .as_ref()
                        .map(|ws| ws.task_dir.clone())
                        .or_else(|| self.task_dir(&child)),
                    branch: child_worktree.and_then(|ws| {
                        ws.repos
                            .first()
                            .and_then(|r| r.branch().map(str::to_string))
                    }),
                });
            }
            out
        } else {
            Vec::new()
        };
        // ADR-0044 D2（Phase 53）: コメントの糸（最新 20 件、古い順）と、直前の run を止めた人のコメント。
        // どちらも決定的に引くだけ（LLM は関与しない）。
        let all_comments = self.store.comments_for(task.id)?;
        let interrupt =
            task_ops::comment::interrupting_comment(&events, &all_comments).map(|c| c.body.clone());
        let comments: Vec<CommentContext> = all_comments
            .iter()
            .skip(
                all_comments
                    .len()
                    .saturating_sub(task_core::PREAMBLE_COMMENTS),
            )
            .map(CommentContext::from)
            .collect();
        // ADR-0047 D2（Phase 61）/ ADR-0046 D1（Phase 59 追記）: 実効マウント（担当ノードの実効
        // profile が継いだ知識 ＋ 設定の既定 ＋ 案件の `projects/<slug>`）と、その索引。
        // 決定的（`index.json` とファイルを読むだけ。LLM も判断も無い）。
        let profile_knowledge: Vec<task_core::KnowledgeMount> = assigned
            .map(|n| task_core::resolve_profile(&org, &n.id).knowledge)
            .unwrap_or_default();
        let knowledge =
            self.knowledge_context(task, assigned.map(|n| n.id.as_str()), &profile_knowledge);
        // ADR-0056 D3（Phase 79）: 担当ノードの実効 profile が継いだ skills mount を KB から解決する。
        // 決定的（ファイルを読むだけ。LLM は関与しない）。見つからない名前は run を落とさず、呼び出し元
        // （`dispatch_ready`）が `status` の進行イベントを 1 行出す。
        let profile_skills_mounts: Vec<String> = assigned
            .map(|n| task_core::resolve_profile(&org, &n.id).skills_mounts)
            .unwrap_or_default();
        let (skills, missing_skills) = self.skills_context(&profile_skills_mounts);
        // ADR-0048 D3（Phase 60b）: CoS の対話 run にだけ、進行中の案件とその途中目標を渡す
        // （`actions` の `create_task.project` / `add_milestone.project` を選ぶ材料。決定的にストアを
        // 読むだけ。CoS 以外の run・継続中の run（ADR-0054 D1: 差分に「新しい案件」が乗る）では常に空）。
        let active_projects = if is_cos_conversation && !continuing {
            self.active_projects_context()?
        } else {
            Vec::new()
        };
        // ADR-0059 D6（Phase 99）/ Phase 99b 追記: CoS が `create_task.workspace` を組む材料として、
        // 既知のクラスタとその実効 work_dir を渡す（未登録なら `work_dir: null` なので、CoS は `path`
        // を省略すべきと分かる。D4 の指示文と対）。`recent_work` / `knowledge` / `profile` / `role` と
        // 同じく、継続中の run でも毎回渡す（1 回渡してもクラスタの登録・接続状態は変わりうる「いまの
        // 状態」であり、`active_projects` のような「前回からの差分」で足りるものとは性質が違う）。
        let clusters = if is_cos_conversation {
            self.cluster_context()
        } else {
            Vec::new()
        };
        Ok(RunExtras {
            role,
            children,
            available_genres,
            node,
            memory,
            conversation,
            standing_rules,
            organization,
            conversation_addressee,
            work_genre,
            recent_work,
            milestone_review,
            workspace_note,
            profile,
            mode,
            comments,
            interrupt,
            knowledge,
            active_projects,
            clusters,
            // ADR-0052 D2: フォールバックの判断は `dispatch_ready` がする（ここは run ごとの文脈だけ）。
            knowledge_fallback: None,
            session,
            session_diff,
            skills,
            missing_skills,
        })
    }

    /// ADR-0059 D6（Phase 99）: `[[clusters]]` の id（決定的な順、昇順）と、接続状態・実効
    /// work_dir（DB の上書き `cluster_settings` > 設定ファイルの `work_dir`）。CoS の対話 run にだけ渡す。
    fn cluster_context(&self) -> Vec<task_worker::ClusterContext> {
        let mut ids: Vec<&String> = self.config.clusters.keys().collect();
        ids.sort();
        ids.into_iter()
            .map(|id| {
                let spec = &self.config.clusters[id];
                let work_dir = self
                    .store
                    .cluster_settings_get(id)
                    .ok()
                    .flatten()
                    .and_then(|s| s.work_dir)
                    .or_else(|| {
                        spec.work_dir
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned())
                    });
                task_worker::ClusterContext {
                    id: id.clone(),
                    connected: self.cluster_connected.get(id).copied().unwrap_or(false),
                    work_dir,
                }
            })
            .collect()
    }

    /// ADR-0054 Phase 67c: この run が CoS の対話 run になるなら、その現役セッション（あれば）を返す。
    /// `run_extras` の `is_cos_conversation` と同じ判定（対話 run で、担当が `OrgKind::Secretary`）を
    /// `select_provider` より前に行う（sticky 選択がランキングより先に効く必要があるため。ADR-0054 D1
    /// の CoS セッションは `node_id = "cos"` 固定）。対象でなければ `Ok(None)`。
    fn cos_conversation_session(&self, task: &Task) -> Result<Option<NodeSession>, DispatchError> {
        if !task_core::is_conversation(task) {
            return Ok(None);
        }
        let Some(assignee) = task.assignee.as_deref() else {
            return Ok(None);
        };
        let org = self.store.org_list()?;
        let is_secretary = org
            .iter()
            .find(|n| n.id == assignee)
            .is_some_and(|n| n.kind == OrgKind::Secretary);
        if !is_secretary {
            return Ok(None);
        }
        Ok(self
            .store
            .node_session_active(task_core::COS_ID, SessionKind::Conversation, None)?)
    }

    /// ADR-0054 D1（Phase 67）: `(node_id, kind, project_id)` の継続セッションを決める・作る・引退させる
    /// （store の読み書き。判断そのものは `crate::sessions::decide`、純粋・テスト容易）。対応しない
    /// アダプタでは `(None, vec![])` を返す（このノード・kind は継続セッションを持たない）。
    fn resolve_node_session(
        &self,
        node_id: &str,
        kind: SessionKind,
        project_id: Option<ProjectId>,
        adapter_id: &str,
        account: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<(Option<task_worker::protocol::SessionHandle>, Vec<String>), DispatchError> {
        if !crate::sessions::adapter_supports_sessions(adapter_id) {
            return Ok((None, Vec::new()));
        }
        let active = self.store.node_session_active(node_id, kind, project_id)?;
        let action = crate::sessions::decide(
            active.as_ref(),
            adapter_id,
            account,
            self.config.session_rollover_tokens,
            // ADR-0054 D1: resume の失敗（アダプタがセッション不明/拒否を報告した）の明示検出は
            // 今回のスコープには含めない（PROGRESS の未解決事項）。供給側失敗として requeue されるだけ。
            false,
        );
        match action {
            crate::sessions::SessionAction::Resume => {
                let active = active.expect("SessionAction::Resume implies an active session");
                let diff = self.session_diff_since(node_id, active.last_used_at)?;
                Ok((
                    Some(task_worker::protocol::SessionHandle {
                        adapter: adapter_id.to_string(),
                        session_id: active.session_id.clone(),
                        resume: true,
                    }),
                    diff,
                ))
            }
            crate::sessions::SessionAction::Fresh(reason) => {
                if let Some(stale) = &active {
                    // ADR-0054 Phase 67b 追記: 本番の自己修復（P-67b-1）。`claude-code` の
                    // `session_id` が UUID でない行（Phase 67 が ULID を渡していた事故）を retire する
                    // ときは、次の障害調査のためにも必ずログへ残す。
                    if matches!(reason, crate::sessions::FreshReason::InvalidSessionId) {
                        tracing::warn!(
                            node_id,
                            kind = kind.as_str(),
                            adapter = adapter_id,
                            old_session_id = %stale.session_id,
                            "node_sessions.session_id はこのアダプタでは使えない形式（claude-code は UUID が必要）。\
                             retire して新しいセッションを作る（ADR-0054 Phase 67b）"
                        );
                    }
                    self.store
                        .node_session_retire(node_id, kind, project_id, now)?;
                }
                // claude-code は celeris が id を前もって決める（`--session-id`）。codex / acp は
                // アダプタが run の途中で初めて確定させるので、確定するまでは空文字（ADR-0054 D1）。
                // Phase 67b: id の発行は `crate::sessions::new_session_id`（純粋関数、テスト対象）に
                // 切り出した。Phase 67 まで使っていた `ulid::Ulid::new().to_string()`（ULID）は Claude
                // Code CLI 2.1.278 が `--session-id`/`--resume` に要求する UUID 形式ではなく、本番の
                // すべての CoS 対話・部門長レビュー run を壊した（2026-09-21 観測）。
                let session_id = crate::sessions::new_session_id(adapter_id);
                let new_session = NodeSession::new(
                    node_id,
                    kind,
                    project_id,
                    adapter_id,
                    account.map(str::to_string),
                    session_id.clone(),
                    now,
                );
                self.store.node_session_create(&new_session)?;
                let summary = if reason.needs_summary() {
                    self.session_summary(node_id)?
                } else {
                    Vec::new()
                };
                Ok((
                    Some(task_worker::protocol::SessionHandle {
                        adapter: adapter_id.to_string(),
                        session_id,
                        resume: false,
                    }),
                    summary,
                ))
            }
        }
    }

    /// ADR-0054 D1: 継続中セッションの前置きに出す差分（`since` より後に起きたこと。新しい人の発言・
    /// 終端タスクの要約・新しい案件。認可の結果は今回のスコープには含めない — PROGRESS の未解決事項）。
    fn session_diff_since(
        &self,
        node_id: &str,
        since: OffsetDateTime,
    ) -> Result<Vec<String>, DispatchError> {
        // `project_id: None` = 絞らない（`message_page` の規約。ADR-0048 D1）。CoS のセッションは
        // どの案件についての発言も同じ 1 本のセッションに乗るため、案件を問わず読む。
        let messages = self.store.message_page(Some(node_id), None, None, 50)?;
        let new_messages: Vec<(OffsetDateTime, task_core::MessageRole, String)> = messages
            .into_iter()
            .map(|m| (m.created_at, m.role, m.text))
            .collect();
        let finished_tasks = self.finished_tasks_since(since)?;
        let new_projects = self.new_projects_since(since)?;
        Ok(crate::sessions::diff_lines(
            &new_messages,
            &finished_tasks,
            &[],
            &new_projects,
            since,
        ))
    }

    /// ADR-0054 D1: 新しいセッションを継ぐときの「これまでの要約」（ADR-0033 D4 の対話履歴の末尾
    /// `CONVERSATION_HISTORY` 件）。
    fn session_summary(&self, node_id: &str) -> Result<Vec<String>, DispatchError> {
        let messages = self.store.message_page(
            Some(node_id),
            None,
            None,
            task_ops::conversation::CONVERSATION_HISTORY,
        )?;
        let history: Vec<(task_core::MessageRole, String)> =
            messages.into_iter().map(|m| (m.role, m.text)).collect();
        Ok(crate::sessions::summary_lines(
            &history,
            task_ops::conversation::CONVERSATION_HISTORY,
        ))
    }

    /// ADR-0054 D1: 前回の run 以降に終端になったタスク（支援タスクは除く）の 1 行要約。
    fn finished_tasks_since(
        &self,
        since: OffsetDateTime,
    ) -> Result<Vec<(OffsetDateTime, String)>, DispatchError> {
        let filter = ListFilter {
            statuses: vec![Status::Done, Status::Failed, Status::Blocked],
            ..ListFilter::default()
        };
        let page = self
            .store
            .list_page(&filter, ListOrder::UpdatedDesc, None, RECENT_WORK_SCAN)?;
        let mut out = Vec::new();
        for task in page.items {
            if support_kind(&task).is_some() || task.updated_at <= since {
                continue;
            }
            let events = self.store.events_for(task.id)?;
            let outcome = recent_work_outcome(&task, &events).unwrap_or_default();
            out.push((task.updated_at, format!("{} — {outcome}", task.title)));
        }
        Ok(out)
    }

    /// ADR-0054 D1: `since` より後に作られた案件（`proposed`/`active`。人が新しく開いたもの）。
    fn new_projects_since(
        &self,
        since: OffsetDateTime,
    ) -> Result<Vec<(OffsetDateTime, String)>, DispatchError> {
        let projects = self.store.project_list()?;
        Ok(projects
            .into_iter()
            .filter(|p| p.created_at > since)
            .map(|p| (p.created_at, p.title))
            .collect())
    }

    /// ADR-0048 D3（Phase 60b）: CoS の対話 run に渡す「進行中の案件とその途中目標」（`proposed` /
    /// `active` の案件だけ。決定的にストアを読むだけ。LLM も判断も無い）。
    fn active_projects_context(&self) -> Result<Vec<ActiveProjectContext>, DispatchError> {
        let mut projects = self.store.project_list()?;
        projects.retain(|p| matches!(p.status, ProjectStatus::Proposed | ProjectStatus::Active));
        projects.sort_by_key(|p| p.id);
        let mut out = Vec::new();
        for project in projects.into_iter().take(ACTIVE_PROJECTS_SCAN) {
            let mut milestones = self.store.milestone_list(project.id)?;
            milestones.sort_by_key(|m| m.id);
            out.push(ActiveProjectContext {
                repos: self
                    .store
                    .repo_list(project.id)?
                    .into_iter()
                    .map(|repo| repo.name)
                    .collect(),
                id: project.id.to_string(),
                title: project.title.clone(),
                status: project.status.as_str().to_string(),
                milestones: milestones
                    .into_iter()
                    .map(|m| ActiveMilestoneContext {
                        id: m.id.to_string(),
                        title: m.title,
                        status: m.status.as_str().to_string(),
                    })
                    .collect(),
            });
        }
        Ok(out)
    }

    /// ADR-0047 D2（Phase 61）: この run が読める知識の**索引だけ**を組む（純粋に近い: 設定・DB・
    /// `index.json`・ファイル名を読むだけ。本文は入れない）。
    ///
    /// 実効マウント = `[knowledge] default_mounts` ＋ 案件の `projects/<slug>`（自動）
    /// ＋ タスクの明示（Phase 61 では無い。Phase 59 の実効 profile がここに合流する）。
    /// ADR-0046 D1（Phase 59 追記）: `profile_knowledge` は担当ノードの**実効 profile**が継いだ
    /// マウント（`task_core::resolve_profile(..).knowledge`）。組織の和 → 設定の既定 → 案件の順で
    /// `merge_mounts` に渡す（先に出てきたものが前置きの索引で先頭に来る）。
    fn knowledge_context(
        &self,
        task: &Task,
        node_id: Option<&str>,
        profile_knowledge: &[task_core::KnowledgeMount],
    ) -> Option<task_worker::protocol::KnowledgeContext> {
        let root = &self.config.knowledge.root;
        // 案件は自動で `projects/<slug>` をマウントする（ADR-0047 D2）。
        let project = task
            .project_id
            .and_then(|id| self.store.project_get(id).ok().flatten());
        let mut project_mounts = Vec::new();
        if let Some(project) = &project {
            let slug = task_ops::docs::project_slug(&project.title, &project.id.to_string());
            project_mounts.push(task_core::KnowledgeMount::kb(format!("projects/{slug}")));
        }
        // ADR-0046 D1（Phase 59 追記）: 実効 profile ＋ 設定の既定 ＋ 案件の順で和を取る。
        let mounts = task_core::knowledge::merge_mounts(&[
            profile_knowledge,
            &self.config.knowledge.default_mounts,
            &project_mounts,
        ]);
        if mounts.is_empty() {
            return None;
        }
        let index = if task_ops::knowledge::exists(root) {
            task_ops::knowledge::ensure_index(root).items
        } else {
            Vec::new()
        };
        let mut items: Vec<task_core::KnowledgeItem> = Vec::new();
        for mount in &mounts {
            match mount.kind {
                task_core::MountKind::Kb => {
                    items.extend(
                        index
                            .iter()
                            .filter(|i| task_core::knowledge::mount_matches(mount, i))
                            .cloned(),
                    );
                }
                // `repo` は案件のリポジトリの文書の根のページ（ADR-0043 / ADR-0044 D7）。
                task_core::MountKind::Repo => {
                    if let Some(name) = mount.name.as_deref() {
                        items.extend(self.repo_doc_items(task, name, mount));
                    }
                }
                // `dir` は任意のローカルディレクトリの `*.md`（読み取り）。
                task_core::MountKind::Dir => {
                    if let Some(dir) = mount.path.as_deref() {
                        let label = mount.label();
                        items.extend(
                            task_ops::knowledge::list_pages(dir)
                                .into_iter()
                                .take(200)
                                .map(|rel| task_core::KnowledgeItem {
                                    title: rel.rsplit('/').next().unwrap_or(&rel).to_string(),
                                    path: dir.join(&rel).display().to_string(),
                                    scope: Some(label.clone()),
                                    ..task_core::KnowledgeItem::default()
                                }),
                        );
                    }
                }
                // `memory` はそのノードの手帳（ADR-0033 D3。中身は「覚えていること」の節に既に出ている）。
                task_core::MountKind::Memory => {
                    let node = mount.name.as_deref().or(node_id);
                    if let (Some(dir), Some(node)) = (&self.config.memory_dir, node) {
                        let notes = dir.join(node).join("notes.md");
                        if notes.exists() {
                            items.push(task_core::KnowledgeItem {
                                path: notes.display().to_string(),
                                title: "あなたの手帳（案件をまたぐ記憶）".to_string(),
                                scope: Some(format!("memory:{node}")),
                                ..task_core::KnowledgeItem::default()
                            });
                        }
                    }
                }
            }
        }
        Some(task_worker::protocol::KnowledgeContext {
            mounts,
            index: items,
        })
    }

    /// ADR-0056 D3（Phase 79）: `skills_mounts`（skill 名の一覧）を KB の `skills/<name>/` から解決する
    /// （`SKILL.md` があるかどうかを読むだけ。決定的、LLM は関与しない）。見つかったものは
    /// `RunContext.skills` に乗せる `SkillMount`、見つからなかった名前は 2 つ目の戻り値（呼び出し元が
    /// `status` の進行イベントを 1 行出し、run は落とさない）。
    fn skills_context(
        &self,
        mounts: &[String],
    ) -> (Vec<task_worker::protocol::SkillMount>, Vec<String>) {
        let root = &self.config.knowledge.root;
        let mut skills = Vec::new();
        let mut missing = Vec::new();
        for name in mounts {
            match task_ops::knowledge::skills_get(root, name) {
                Some(detail) => {
                    let description = task_ops::knowledge::skill_description(&detail.skill_md);
                    let path = root
                        .join(task_core::knowledge::SKILLS_DIR)
                        .join(name)
                        .display()
                        .to_string();
                    skills.push(task_worker::protocol::SkillMount {
                        name: name.clone(),
                        path,
                        description,
                    });
                }
                None => missing.push(name.clone()),
            }
        }
        (skills, missing)
    }

    /// `repo` マウント 1 件分（案件のその名前のリポジトリの文書の根のページ）。
    fn repo_doc_items(
        &self,
        task: &Task,
        name: &str,
        mount: &task_core::KnowledgeMount,
    ) -> Vec<task_core::KnowledgeItem> {
        let Some(project_id) = task.project_id else {
            return Vec::new();
        };
        let Ok(repos) = self.store.repo_list(project_id) else {
            return Vec::new();
        };
        let Some(repo) = repos.iter().find(|r| r.name == name) else {
            return Vec::new();
        };
        let task_core::WorkspaceSpec::Local { path, .. } = &repo.location else {
            return Vec::new();
        };
        let (config, _) = task_core::workspace_config::load_or_default(path);
        let docs_root = mount.docs.clone().unwrap_or(config.outputs.docs);
        let branch = task_ops::changes::default_branch(path, repo.default_branch.as_deref());
        let label = mount.label();
        task_ops::docs::list(path, &branch, &docs_root)
            .into_iter()
            .take(200)
            .map(|rel| task_core::KnowledgeItem {
                title: rel.rsplit('/').next().unwrap_or(&rel).to_string(),
                path: rel,
                scope: Some(label.clone()),
                ..task_core::KnowledgeItem::default()
            })
            .collect()
    }

    /// Phase 41（ADR-0038 D1）: レビューの対話 run に渡す「その途中目標のここまで」。
    /// その途中目標に属する仕事（裏方は除く。作られた順に最大 20 件）の title / status / 終端の要約 /
    /// 主な成果物（`answer.md` / `report.md` の先頭 4,000 字）を集める。LLM は使わない（DESIGN 原則 1）。
    fn milestone_review_of(
        &self,
        project_id: Option<ProjectId>,
        milestone_id: task_core::MilestoneId,
    ) -> Result<Option<MilestoneReviewContext>, DispatchError> {
        let Some(project_id) = project_id else {
            return Ok(None);
        };
        let Some(milestone) = self
            .store
            .milestone_list(project_id)?
            .into_iter()
            .find(|m| m.id == milestone_id)
        else {
            return Ok(None);
        };
        let filter = ListFilter {
            project_id: Some(project_id),
            ..ListFilter::default()
        };
        let page = self.store.list_page(
            &filter,
            ListOrder::CreatedDesc,
            None,
            MILESTONE_REVIEW_TASK_SCAN,
        )?;
        let mut subjects: Vec<Task> = page
            .items
            .into_iter()
            .filter(|t| t.milestone_id == Some(milestone_id) && support_kind(t).is_none())
            .collect();
        subjects.sort_by_key(|t| (t.created_at, t.id));
        let mut tasks = Vec::new();
        for task in subjects.into_iter().take(MILESTONE_REVIEW_TASK_LIMIT) {
            let events = self.store.events_for(task.id)?;
            tasks.push(MilestoneTaskResult {
                title: task.title.clone(),
                status: task.status,
                outcome: recent_work_outcome(&task, &events),
                artifacts_excerpt: self.milestone_artifacts_excerpt(&task),
            });
        }
        Ok(Some(MilestoneReviewContext {
            milestone: MilestoneBrief {
                id: milestone.id.to_string(),
                title: milestone.title.clone(),
                description: milestone.description.clone(),
                status: milestone.status.as_str().to_string(),
            },
            tasks,
        }))
    }

    /// Phase 41（ADR-0038 D1）: その仕事が残した `answer.md` / `report.md` の先頭 4,000 字
    /// （両方あれば名前を見出しに付けて繋ぎ、全体を 4,000 字で切る。読めなければ空文字）。
    fn milestone_artifacts_excerpt(&self, task: &Task) -> String {
        let Some(workspace) = self.task_dir(task) else {
            return String::new();
        };
        let dir = self.artifacts_dir(task, &workspace);
        let mut out = String::new();
        for name in MILESTONE_REVIEW_ARTIFACTS {
            if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
                out.push_str(&format!("#### {name}\n"));
                out.push_str(text.trim_end());
                out.push('\n');
            }
        }
        truncate_chars(&out, MILESTONE_REVIEW_EXCERPT_CHARS)
    }

    /// Phase 38（ADR-0028 追記。実機のレビュー不合格から）: 計画が**ハーネスで動く分野**の担当に
    /// 「自分で決めた名前のファイルを書け」と要求していたら、その `artifact_exists` の条件を落として
    /// `objective` に本当の成果物の名前を注記する（`task_core::plan::fix_harness_artifacts`）。
    /// 壊さず直す（Plan run は失敗させず、`Question` にもしない）。判定は決定的で LLM は呼ばない
    /// （DESIGN 原則 1）。直した事実は `warn` に残す。
    fn fix_plan_for_harness(&self, task: &Task, plan: &mut PlanOutput, org: &[task_core::OrgNode]) {
        for note in task_core::fix_harness_artifacts(
            plan,
            task,
            org,
            &self.config.roles,
            &self.config.genres,
        ) {
            tracing::warn!(task_id = %task.id, "{note}");
        }
        // ADR-0063 D3（Phase 109）: 調査系（literature/web-research）の受け入れ条件に部分達成の
        // 逃げ道が無ければ**警告**（拒否はしない。決定的、LLM は使わない）。
        for note in task_core::warn_missing_partial_ok(
            plan,
            task,
            org,
            &self.config.roles,
            &self.config.genres,
        ) {
            tracing::warn!(task_id = %task.id, "{note}");
        }
        // ADR-0069 D1（Phase 114）: 計画（LLM）が書いた担当は使わない（子の `routing.dropped_assignee`
        // に残り、担当は matching が決める）。捨てた事実をここで 1 行ずつ残す。
        for (index, t) in plan.tasks.iter().enumerate() {
            if let Some(a) = t.assignee.as_deref().filter(|a| !a.trim().is_empty()) {
                tracing::info!(task_id = %task.id, index, dropped_assignee = %a, "plan-supplied assignee ignored; matching decides (ADR-0069 D1)");
            }
        }
    }

    /// Phase 33（ADR-0033 D4 追記）: `node_id` の直近の仕事（対話・まとめ・承認・レビューは除く）を
    /// 更新の新しい順に最大 10 件集める。`current_project` があれば、その案件のものを先に、
    /// 残りは他の案件から（`current_project` の中でも他の案件のものでも、それぞれ更新の新しい順は保つ）。
    /// ストアの読み取りだけで、LLM は使わない（DESIGN 原則 1）。
    fn recent_work_of(
        &self,
        node_id: &str,
        current_project: Option<ProjectId>,
    ) -> Result<Vec<RecentWork>, DispatchError> {
        let filter = ListFilter {
            assignee: Some(node_id.to_string()),
            ..ListFilter::default()
        };
        let page = self
            .store
            .list_page(&filter, ListOrder::UpdatedDesc, None, RECENT_WORK_SCAN)?;
        let candidates: Vec<Task> = page
            .items
            .into_iter()
            .filter(|t| support_kind(t).is_none())
            .collect();
        let (same_project, other_project): (Vec<Task>, Vec<Task>) = if current_project.is_some() {
            candidates
                .into_iter()
                .partition(|t| t.project_id == current_project)
        } else {
            (Vec::new(), candidates)
        };
        let mut project_titles: HashMap<ProjectId, Option<String>> = HashMap::new();
        let mut out = Vec::with_capacity(RECENT_WORK_LIMIT);
        for task in same_project
            .into_iter()
            .chain(other_project)
            .take(RECENT_WORK_LIMIT)
        {
            let events = self.store.events_for(task.id)?;
            let artifacts = artifact_names_of(&events);
            let outcome = recent_work_outcome(&task, &events);
            let finished_at =
                if matches!(task.status, Status::Done | Status::Failed | Status::Blocked) {
                    Some(rfc3339(task.updated_at))
                } else {
                    None
                };
            let project_title = match task.project_id {
                Some(pid) => project_titles
                    .entry(pid)
                    .or_insert_with(|| self.store.project_get(pid).ok().flatten().map(|p| p.title))
                    .clone(),
                None => None,
            };
            out.push(RecentWork {
                task_id: task.id,
                title: task.title.clone(),
                project_title,
                status: task.status,
                finished_at,
                outcome,
                artifacts,
            });
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_worker(
        &self,
        task_id: TaskId,
        execution_tier: task_core::Tier,
        run_id: String,
        provider: ProviderId,
        account: Option<String>,
        account_adapter: Option<AccountAdapter>,
        adapter: Arc<dyn WorkerAdapter>,
        dir: PathBuf,
        limits: RunLimits,
        remote: Option<SshSettings>,
        worktree: Option<task_worker::TaskWorkspaces>,
        extras: RunExtras,
        container: ContainerDecision,
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
        // ADR-0066 D1（Phase 110b）: `[workspace] shared_build_cache`（既定 true）。
        let build_cache_dir = self
            .config
            .shared_build_cache
            .then(|| self.config.build_cache_dir.clone());
        tokio::spawn(async move {
            let result = run_worker(
                store,
                adapter,
                task_id,
                execution_tier,
                dir,
                &run_id,
                limits,
                lease,
                remote,
                worktree,
                extras,
                roles,
                genres,
                delegation,
                account,
                account_book,
                container,
                build_cache_dir,
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
        if !self.accepting_new_work {
            return Ok(false);
        }
        let Some(task) = self.store.get(task_id)? else {
            return Ok(true);
        };
        if task.status != Status::Reviewing {
            return Ok(true);
        }
        let Some(dir) = self.task_dir(&task) else {
            tracing::warn!(%task_id, "cannot review task with remote workspace");
            return Ok(true);
        };
        // Old and new daemons can overlap during live handoff. Both command checks
        // and model review hold the same per-task lock through verdict persistence.
        let lock_dir = dir.join("runs");
        std::fs::create_dir_all(&lock_dir)
            .map_err(|e| StoreError::Invalid(format!("review lock directory: {e}")))?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_dir.join(format!(".review-{task_id}.lock")))
            .map_err(|e| StoreError::Invalid(format!("review lock: {e}")))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(StoreError::Invalid(format!("review lock: {e}")).into());
            }
        }
        let review_lock = Arc::new(lock);
        // The previous owner may have committed a verdict after our first read.
        let Some(mut task) = self.store.get(task_id)? else {
            return Ok(true);
        };
        if task.status != Status::Reviewing {
            return Ok(true);
        }

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
        let review_run = reviewer
            .as_ref()
            .map(|(p, _, r)| (p.clone(), r.run_id.clone(), r.adapter.id().to_string()));
        let reviewer_run = reviewer.map(|(_, _, r)| r);
        if let Some(run) = &reviewer_run {
            task_ops::delivery::begin(
                self.store.as_ref(),
                &mut task,
                &self.config.workspace_root,
                &self.config.delivery,
                &run_id,
                &run.run_id,
            )
            .map_err(DispatchError::from)?;
        }

        let plan = if task.kind == TaskKind::Plan {
            Some(PlanCheck {
                depth: self.plan_depth(&task)?,
                limits: PlanLimits::default(),
                genres: self.config.genres.clone(),
                // ADR-0043 D2: 計画が子に書ける `repos` の名前（案件に登録されているものだけ）。
                repos: task_ops::delegate::project_repos(self.store.as_ref(), &task)
                    .map_err(ops_to_store)?
                    .into_iter()
                    .map(|r| r.name)
                    .collect(),
            })
        } else {
            None
        };
        // ADR-0016 M4: 集約 run のレビューには暗黙の条件「artifacts/summary.md がある」が加わる。
        let aggregate =
            task.aggregate && has_aggregate_transition(&self.store.events_for(task_id)?);

        // ADR-0018: 判定コマンドもクラスタで実行する。
        let cluster = self.cluster_of(&task);
        let remote_settings = cluster
            .as_ref()
            .map(|(spec, path, mode)| spec.ssh_settings(path, task.id, *mode));
        let cluster_id = cluster.as_ref().map(|(spec, ..)| spec.id.clone());
        let events = self.store.events_for(task_id)?;
        let produced = artifacts_for_run(&events, &run_id);
        // ADR-0014 D1: Reviewer run も対象タスクに WorkerStarted（role: reviewer）を残す（アカウント別の集計に含めるため）。
        if let Some((provider_id, review_run_id, adapter_id)) = &review_run {
            let model = self
                .adapters
                .get(provider_id)
                .and_then(|a| {
                    a.model_for_tier(self.config.reviewer_hint.tier)
                        .ok()
                        .flatten()
                })
                .or_else(|| self.models.get(provider_id).cloned())
                .unwrap_or_default();
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
        // ADR-0036 D1/D2: 判定（`plan.json` / `review.json` / `summary.md` / `ArtifactExists` の既定パス）は
        // 対象タスクの成果物ディレクトリを基準にする。
        let artifacts_dir = self.artifacts_dir(&task, &dir);
        let entry_subject = subject.clone();
        let entry_run_id = run_id.clone();
        let subject = subject.clone();
        let tx = self.tx.clone();
        let remote_review = remote_settings.clone();
        // ADR-0019 D1 6. / ADR-0041 D1: 判定コマンドは worktree の中で実行する（元のリポジトリでは実行しない）。
        let review_work_dir = self.work_dir_for(&task);
        // ADR-0043 D4: リポジトリが宣言した検査コマンド（`workspace.toml` の `[commands] check`）。
        // ADR-0046 D4（Phase 59）: `mode = prototype` は「明示の受け入れ条件だけ」なので使わない。
        let repo_checks = if task.mode == task_core::TaskMode::Prototype {
            Vec::new()
        } else {
            self.default_checks(&task)
        };
        // ADR-0046 D4（Phase 59）: `mode = research` は「結果に出典か計測の記録」を暗黙の条件に足す。
        let research = task.mode == task_core::TaskMode::Research;
        let running_review_lock = review_lock.clone();
        let handle = tokio::spawn(async move {
            let _review_lock = running_review_lock;
            let ws: Box<dyn Workspace> = match remote_review {
                Some(settings) => Box::new(SshWorkspace::new(&dir, settings)),
                None => Box::new(match review_work_dir {
                    Some(work) if work.is_dir() => LocalWorkspace::new(&dir).with_work_dir(work),
                    _ => LocalWorkspace::new(&dir),
                }),
            };
            let extras = ReviewExtras {
                subject,
                plan,
                reviewer: reviewer_run,
                human,
                aggregate,
                // ADR-0043 D4: リポジトリの `[commands] check`（タスクに検査コマンドが無いときだけ効く）。
                repo_checks,
                // ADR-0046 D4: `mode = research` の暗黙の条件。
                research,
            };
            let outcome = review_task(
                &task,
                ws.as_ref(),
                &dir,
                &artifacts_dir,
                &produced,
                timeout,
                extras,
            )
            .await;
            // The entry owns the lock through verdict persistence. Release this
            // task's copy before sending completion so it cannot outlive that entry.
            drop(_review_lock);
            let _ = tx.send(Completion::Review {
                task_id,
                run_id,
                outcome,
            });
        });
        self.reviewing.insert(
            task_id,
            ReviewEntry {
                _review_lock: review_lock,
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
            let child = match existing_children.iter().find(|c| {
                c.parent_id == Some(task.id) && c.kind == TaskKind::Approval && c.title == title
            }) {
                Some(c) => c.clone(),
                None => self.create_human_approval_child(task, idx, &title)?,
            };
            match child.status {
                Status::Done => {
                    resolved.insert(
                        idx,
                        (true, format!("approved (approval task {})", child.id)),
                    );
                }
                Status::Failed => {
                    let note = approval_decision_note(&self.store.events_for(child.id)?);
                    resolved.insert(
                        idx,
                        (
                            false,
                            format!("rejected (approval task {}){note}", child.id),
                        ),
                    );
                }
                Status::Cancelled => {
                    resolved.insert(
                        idx,
                        (false, format!("approval task {} was cancelled", child.id)),
                    );
                }
                _ => return Ok(None),
            }
        }
        Ok(Some(resolved))
    }

    /// `Human` criterion のための `Approval` 子タスクを新規作成する（ADR-0008 D2）。
    fn create_human_approval_child(
        &self,
        task: &Task,
        idx: usize,
        title: &str,
    ) -> Result<Task, DispatchError> {
        let now = OffsetDateTime::now_utc();
        let approval = Task {
            routing: None,
            repos: Vec::new(),
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
            // ADR-0033 D2（監査 D-3）: 派生タスクは親の案件・途中目標・担当を継ぐ（仕事の木から子が消えないように）。
            project_id: task.project_id,
            milestone_id: task.milestone_id,
            assignee: task.assignee.clone(),
            conversation: None,
            labels: Vec::new(),
            category: Default::default(),
            // ADR-0046 D4（Phase 59）: 派生タスクは親の進め方を継ぐ。
            skills: Vec::new(),
            mode: task.mode,
        };
        // ADR-0010 D2: 挿入・Created・ApprovalRequested を 1 トランザクションで。
        self.store
            .create_task(&approval, vec![Event::ApprovalRequested])?;
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
        let org = self.store.org_list().ok()?;
        let department = task
            .assignee
            .as_deref()
            .and_then(|id| task_core::department_of(&org, id));
        let node = department
            .as_deref()
            .and_then(|id| org.iter().find(|n| n.id == id))
            .map(|n| NodeContext {
                id: n.id.clone(),
                name: n.name.clone(),
                brief: n.brief.clone(),
            });
        let profile = department
            .as_deref()
            .map(|id| task_core::profile::resolve(&org, id));
        // ADR-0069 Phase 118 D4: reviewer の lane は、上ほど強い優先順位で決める。
        //   1. 部署の `profile.review_tier`（ADR-0069 D2。最も具体的な指定）。
        //   2. `[reviewer] tier` の明示（`reviewer_tier_override`）。
        //   3. 既定: worker run の lane に一致させ、組織の天井（`lane_ceiling`）で丸める。
        // 1./2. は丸めない（人・運用の明示は組織の既定より強い。D2 の「人の明示 tier は天井で
        // 丸めない」と同じ考え方を運用の明示にも適用する）。
        let ceiling = profile
            .as_ref()
            .map(|p| p.lane_ceiling())
            .unwrap_or_default();
        let worker_lane = task.worker_hint.tier;
        let (default_tier, default_clamp) = ceiling.clamp(worker_lane);
        let (reviewer_lane, review_rule_id, review_reasons) = match (
            profile.as_ref().and_then(|p| p.review_tier),
            self.config.reviewer_tier_override,
        ) {
            (Some(dept_tier), _) => (
                dept_tier,
                "reviewer/department-review-tier",
                vec![format!(
                    "org profile review.tier = {dept_tier:?} (most specific; ADR-0069 D2)"
                )],
            ),
            (None, Some(explicit)) => (
                explicit,
                "reviewer/explicit-config",
                vec![format!(
                    "[reviewer] tier = {explicit:?} (explicit config; Phase 118 D4)"
                )],
            ),
            (None, None) => {
                let mut reasons = vec![format!("default: matches the worker lane {worker_lane:?}")];
                if let Some(clamp) = &default_clamp {
                    reasons.push(clamp.clone());
                }
                (default_tier, "reviewer/matches-worker-lane", reasons)
            }
        };
        let mut hint = self.config.reviewer_hint.clone();
        hint.tier = reviewer_lane;

        // ADR-0054 Phase 67c: 部署のレビュー・切り分け run（`kind = lead`）も、継続セッションの
        // (adapter, account) に留まれるかを先に試す（`resolve_node_session` と同じキー）。
        let sticky_session = match &department {
            Some(dept_id) => self
                .store
                .node_session_active(dept_id, task_core::SessionKind::Lead, None)
                .ok()
                .flatten(),
            None => None,
        };
        let mut full = std::collections::HashSet::new();
        let (adapter_id, provider_id, selected_account) = self.select_provider(
            &hint,
            Instant::now(),
            task.id,
            &mut full,
            sticky_session.as_ref(),
        )?;
        let base_adapter = match self.adapters.get(&provider_id) {
            Some(a) => a.clone(),
            None => {
                tracing::warn!(task_id = %task.id, provider = %provider_id, adapter = %adapter_id, "no adapter instance for reviewer provider");
                return None;
            }
        };
        if let Err(reason) = base_adapter.model_for_tier(hint.tier) {
            if self.warned_unroutable.insert(task.id) {
                let _ = self.store.append_event(
                    task.id,
                    &Event::worker_progress(
                        subject_run_id,
                        format!("review model routing blocked: {reason}"),
                    ),
                );
            }
            return None;
        }
        let adapter = match &selected_account {
            Some((account_adapter, account_id)) => {
                match self.adapter_for_account(&base_adapter, *account_adapter, account_id) {
                    Some(a) => a,
                    None => {
                        tracing::warn!(task_id = %task.id, provider = %provider_id, account_id, "adapter does not support account pools for reviewer run; deferring");
                        return None;
                    }
                }
            }
            None => base_adapter,
        };
        let account = selected_account.as_ref().map(|(_, id)| id.clone());
        let account_adapter = selected_account.as_ref().map(|(a, _)| *a);
        let review_run_id = ulid::Ulid::new().to_string();
        // ADR-0054 D1 / Phase 67b 追記: 部署があれば、この run の `session_established`/
        // `session_resume_failed` を Lead セッション（`kind = lead`）に配線する（Phase 67 で抜けていた
        // 配線。下の `resolve_node_session` と同じ `(department, Lead, None)` のキー）。
        let session_key = department
            .clone()
            .map(|dept_id| (dept_id, task_core::SessionKind::Lead, None));
        let sink = ReviewerSink {
            store: self.store.clone(),
            task_id: task.id,
            subject_run_id: subject_run_id.to_string(),
            review_run_id: review_run_id.clone(),
            account: account.clone(),
            account_book: account_adapter.and_then(|a| self.account_book(a)),
            session_key,
        };
        // ADR-0054 D1（Phase 67）: 部署の根ノード（engineering/research/operations）は
        // レビュー・切り分け run を継続セッション（`kind = lead`）で走らせる（ADR-0051）。部署が無い
        // 仕事（従来の独立レビュアー）では継続しない。store のエラーはレビューそのものを止めない
        // （継続無し＝Phase 66 までと同じ挙動にフォールバックする）。
        let (session, session_diff) = match &department {
            Some(dept_id) => match self.resolve_node_session(
                dept_id,
                task_core::SessionKind::Lead,
                None,
                &adapter_id,
                account.as_deref(),
                OffsetDateTime::now_utc(),
            ) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(task_id = %task.id, department = %dept_id, error = %e, "failed to resolve the department lead session; reviewing without one");
                    (None, Vec::new())
                }
            },
            None => (None, Vec::new()),
        };
        tracing::info!(task_id = %task.id, %review_run_id, adapter = %adapter_id, provider = %provider_id, account = account.as_deref(), "starting reviewer run");
        // ADR-0069 Phase 118 D4: reviewer run にも `Event::RoutingDecided` を残す（Phase 114 は worker
        // run にしか出していなかった）。reviewer は `TaskFeatures` 規則表を通らないので `rule_id` は
        // 上で決めた 3 種のいずれか、`source = System`（policy ではなく config/組織の指定で決まる）。
        // ストア書き込み失敗はレビューそのものを止めない（`warned_unroutable` と同じベストエフォート）。
        let review_model_id = adapter
            .model_for_tier(hint.tier)
            .ok()
            .flatten()
            .unwrap_or_default();
        let review_reasoning_effort = adapter
            .reasoning_effort_for_tier(hint.tier)
            .filter(|_| adapter.supports_reasoning_effort());
        let review_record = task_core::RoutingRecord {
            org_node: department.clone(),
            harness: Some("reviewer".to_string()),
            decision: task_core::LaneDecision {
                lane: reviewer_lane,
                proposed: worker_lane,
                source: task_core::TierSource::System,
                rule_id: review_rule_id.to_string(),
                policy_version: task_core::LANE_POLICY_VERSION.to_string(),
                features: task_core::TaskFeatures::infer(task),
                reasons: review_reasons,
                clamped_by: if review_rule_id == "reviewer/matches-worker-lane" {
                    default_clamp.clone()
                } else {
                    None
                },
                hint: None,
                escalation: None,
                shadow: None,
            },
            resolution: task_core::model_routing::LaneResolution {
                lane: Some(reviewer_lane),
                adapter: adapter_id.clone(),
                provider: Some(provider_id.clone()),
                account: account.clone(),
                model_id: review_model_id,
                reasoning_effort: review_reasoning_effort,
            },
            quota_reason: None,
            work_unit_id: None,
        };
        let _ = self.store.append_event(
            task.id,
            &Event::RoutingDecided {
                run_id: review_run_id.clone(),
                record: Box::new(review_record),
            },
        );
        Some((
            provider_id,
            selected_account,
            ReviewerRun {
                node,
                profile,
                adapter,
                run_id: review_run_id,
                limits: RunLimits {
                    wall_clock: Duration::from_secs(task.budget.max_wall_secs),
                    idle_timeout: self.config.idle_timeout,
                    kill_grace: self.config.kill_grace,
                },
                sink: Box::new(sink),
                hint,
                // Phase 38（ADR-0028 追記）: レビュー対象の分野の manifest（決定的。設定を引くだけ）。
                subject_genre: task
                    .genre
                    .as_deref()
                    .and_then(|id| GenreSpec::find(&self.config.genres, id))
                    .map(|g| GenreContext::from_spec(g, &self.config.roles)),
                session,
                session_diff,
            },
        ))
    }

    /// ADR-0013 D9: cooldown に入った供給側失敗の `ProviderThrottled`。期限はポリシーの `cooldowns()` から取り、
    /// ポリシーが公開しない場合は `Throttled.retry_after` から計算する（どちらも無ければ記録しない）。
    fn provider_throttled_event(
        &self,
        provider: &str,
        outcome: &ProviderOutcome,
        reason: &str,
    ) -> Option<Event> {
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
    ///
    /// ADR-0054 Phase 67c: `sticky_session` にこのノード・kind の現役セッションが渡されたら、まず
    /// `crate::sessions::decide_sticky` でそのセッションの `(adapter, account_id)` に留まれるかを試す
    /// （`sticky_provider`）。留まれれば ADR-0049 のランキングを走らせない（毎 run アカウントを
    /// 付け替えてセッションを退役させ続ける事故の修正）。留まれなければ、これまでどおり下のランキングへ。
    #[allow(clippy::type_complexity)]
    fn select_provider(
        &mut self,
        hint: &task_core::WorkerHint,
        now: Instant,
        task_id: TaskId,
        full: &mut std::collections::HashSet<ProviderId>,
        sticky_session: Option<&NodeSession>,
    ) -> Option<(AdapterId, ProviderId, Option<(AccountAdapter, String)>)> {
        if let Some(sticky) = self.sticky_provider(sticky_session, hint, now, &*full) {
            return Some(sticky);
        }
        // 候補を列挙するための除外はこの選択だけ。満杯の集合へ候補自体を混ぜない。
        let mut visited = full.clone();
        let mut best_pool = None;
        let mut best_score = f64::NEG_INFINITY;
        let mut fallback = None;
        for _ in 0..64 {
            match self.policy.select(hint, now, &visited) {
                Selection::Picked { adapter, provider } => {
                    if !visited.insert(provider.clone()) {
                        break;
                    }
                    let limit = self.policy.concurrency_limit(provider.clone());
                    if self.provider_in_use(&provider) >= limit {
                        full.insert(provider);
                        continue;
                    }
                    if self.account_pool_providers.contains(&provider) {
                        let Some(account_adapter) = AccountAdapter::parse(&adapter) else {
                            full.insert(provider);
                            continue;
                        };
                        let requested_account = self
                            .adapters
                            .get(&provider)
                            .and_then(|a| a.account_id())
                            .map(str::to_owned);
                        let Some(account_id) =
                            self.pick_account(account_adapter, requested_account.as_deref())
                        else {
                            full.insert(provider);
                            continue;
                        };
                        let score = self.account_score(account_adapter, &account_id);
                        if score > best_score {
                            best_score = score;
                            best_pool =
                                Some((adapter, provider, Some((account_adapter, account_id))));
                        }
                    } else if fallback.is_none() {
                        fallback = Some((adapter, provider, None));
                    }
                }
                Selection::Busy => break,
                Selection::NoMatchingProvider => {
                    if best_pool.is_none() && fallback.is_none() {
                        self.unroutable.insert(task_id);
                        if self.warned_unroutable.insert(task_id) {
                            tracing::warn!(%task_id, ?hint, "no provider in the config matches this worker_hint");
                        }
                    }
                    break;
                }
            }
        }
        let selected = best_pool.or(fallback);
        if selected.is_some() {
            self.warned_unroutable.remove(&task_id);
        }
        selected
    }

    /// ADR-0054 Phase 67c: `sticky_session` があれば、`crate::sessions::decide_sticky` にかけて
    /// 「留まれるか」を判断する。使う材料（アカウントの状態・設定表に今もその tier を提供する行が
    /// あるか）はここで集める（I/O）。留まれるなら `select_provider` と同じ形の戻り値、留まれなければ
    /// `None`（呼び出し側が通常のランキングへフォールバックする）。
    #[allow(clippy::type_complexity)]
    fn sticky_provider(
        &mut self,
        sticky_session: Option<&NodeSession>,
        hint: &task_core::WorkerHint,
        now: Instant,
        full: &std::collections::HashSet<ProviderId>,
    ) -> Option<(AdapterId, ProviderId, Option<(AccountAdapter, String)>)> {
        let active = sticky_session?;
        let account_usable = match &active.account_id {
            None => true,
            Some(account_id) => AccountAdapter::parse(&active.adapter)
                .is_some_and(|adapter| self.account_usable(adapter, account_id)),
        };
        let provider = self.matching_provider_for_adapter(&active.adapter, hint.tier, now, full);
        // 設定行が見つかっても、プールの有無がセッション作成時と食い違っていたら（config を書き換えた
        // 等）留まらない。`decide` の `AccountChanged`（プールを使う ⇔ 使わないの切り替えも該当）と
        // 矛盾しないように。
        let provider_offers_tier = provider.as_ref().is_some_and(|p| {
            self.account_pool_providers.contains(p) == active.account_id.is_some()
        });
        let decision = crate::sessions::decide_sticky(
            Some(active),
            self.config.session_rollover_tokens,
            account_usable,
            provider_offers_tier,
        );
        if decision != crate::sessions::StickyDecision::Stick {
            return None;
        }
        let provider_id = provider?;
        let selected_account = active
            .account_id
            .clone()
            .and_then(|id| AccountAdapter::parse(&active.adapter).map(|a| (a, id)));
        Some((active.adapter.clone(), provider_id, selected_account))
    }

    /// ADR-0054 Phase 67c: `adapter_id` の設定行のうち、`requested_tier` と同じかそれ以上の tier を
    /// 提供し（`crate::sessions::tier_rank`。要求そのものから順に試す）、cooldown 中でも並列度上限でも
    /// ないものを 1 つ返す。`hint.adapter` をこの 1 アダプタに固定して `ProviderPolicy::select` に
    /// 任せるので、cooldown・除外集合の扱いは通常のランキングと同じ規則になる。
    fn matching_provider_for_adapter(
        &self,
        adapter_id: &str,
        requested_tier: Tier,
        now: Instant,
        excluded: &std::collections::HashSet<ProviderId>,
    ) -> Option<ProviderId> {
        let mut tiers: Vec<Tier> = [Tier::Cheap, Tier::Standard, Tier::Frontier]
            .into_iter()
            .filter(|t| {
                crate::sessions::tier_rank(*t) >= crate::sessions::tier_rank(requested_tier)
            })
            .collect();
        tiers.sort_by_key(|t| crate::sessions::tier_rank(*t));
        for tier in tiers {
            let pinned = task_core::WorkerHint {
                tier,
                adapter: Some(adapter_id.to_string()),
            };
            if let Selection::Picked { provider, .. } = self.policy.select(&pinned, now, excluded) {
                let limit = self.policy.concurrency_limit(provider.clone());
                if self.provider_in_use(&provider) < limit {
                    return Some(provider);
                }
            }
        }
        None
    }

    /// ADR-0054 Phase 67c: 指定した 1 アカウントが今すぐ使えるか（ログイン済み・cooldown 外・上限未満・
    /// 枯渇していない。`crate::accounts::evaluate` の除外判定をそのまま使う）。`pick_account` と同じ
    /// 読み取りだが、ベストスコアを探すのではなく特定の 1 件が使えるかだけを見る。
    fn account_usable(&mut self, adapter: AccountAdapter, account_id: &str) -> bool {
        let Some(cfg) = self.config.accounts.clone() else {
            return false;
        };
        let Some(root) = cfg.root_for(adapter) else {
            return false;
        };
        let dirs = self
            .accounts_scan_cache
            .entry(adapter)
            .or_insert_with(|| scan_accounts(root, adapter))
            .clone();
        let Some(dir) = dirs.iter().find(|d| d.id == account_id) else {
            return false;
        };
        let Some(book) = self.account_book(adapter) else {
            return false;
        };
        let now = (self.now_unix_fn)();
        let book = book.lock().unwrap_or_else(|e| e.into_inner());
        let candidate = AccountCandidate {
            id: account_id,
            logged_in: dir.logged_in,
            in_use: self.account_in_use(adapter, account_id),
        };
        evaluate(
            &candidate,
            book.state(account_id),
            cfg.max_runs_per_account,
            now,
        )
        .excluded
        .is_none()
    }

    fn account_score(&self, adapter: AccountAdapter, id: &str) -> f64 {
        let Some(book) = self.account_book(adapter) else {
            return f64::NEG_INFINITY;
        };
        let book = book.lock().unwrap_or_else(|e| e.into_inner());
        let candidate = AccountCandidate {
            id,
            logged_in: true,
            in_use: self.account_in_use(adapter, id),
        };
        crate::accounts::evaluate(
            &candidate,
            book.state(id),
            self.config
                .accounts
                .as_ref()
                .map_or(1, |c| c.max_runs_per_account),
            (self.now_unix_fn)(),
        )
        .score
        .unwrap_or(f64::NEG_INFINITY)
    }

    /// ADR-0024 D3 / ADR-0025 D2: `[accounts]` の指定アダプタのプールから 1 アカウントを選ぶ（残量に基づく決定的な
    /// 選択）。そのアダプタの根ディレクトリが無い、または選べるアカウントが無ければ `None`。
    /// ディレクトリのスキャンは tick につき高々 1 回（アダプタごと）。
    fn pick_account(&mut self, adapter: AccountAdapter, requested: Option<&str>) -> Option<String> {
        let cfg = self.config.accounts.clone()?;
        let root = cfg.root_for(adapter)?;
        let dirs = self
            .accounts_scan_cache
            .entry(adapter)
            .or_insert_with(|| scan_accounts(root, adapter))
            .clone();
        let now = (self.now_unix_fn)();
        let book = self.account_book(adapter)?;
        let book = book.lock().unwrap_or_else(|e| e.into_inner());
        let candidates: Vec<AccountCandidate<'_>> = dirs
            .iter()
            .filter(|d| requested.is_none_or(|id| d.id == id))
            .map(|d| AccountCandidate {
                id: d.id.as_str(),
                logged_in: d.logged_in,
                in_use: self.account_in_use(adapter, &d.id),
            })
            .collect();
        select_account(&candidates, &book, cfg.max_runs_per_account, now)
    }

    /// ADR-0024 D4: プール run の供給側失敗をアカウントの cooldown として記録する（プロバイダは cooldown にしない）。
    /// `reason` は `provider_failure_reason` と同じ語彙（`throttled` / `auth_failed` / `exhausted` / `spawn`）。
    fn record_account_failure(
        &self,
        adapter: AccountAdapter,
        account_id: &str,
        reason: &str,
        outcome: &ProviderOutcome,
    ) {
        let Some(cfg) = &self.config.accounts else {
            return;
        };
        let now = (self.now_unix_fn)();
        let fallback_secs = match outcome {
            ProviderOutcome::Throttled { retry_after } if retry_after.as_secs() > 0 => {
                retry_after.as_secs()
            }
            _ => cfg.fallback_cooldown_secs,
        };
        let cooldown_reason = account_cooldown_reason_from_failure(reason);
        let Some(book) = self.account_book(adapter) else {
            return;
        };
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
        base.with_env(&[(
            account_adapter.env_var().to_string(),
            dir.display().to_string(),
        )])
    }

    /// ADR-0005 D3: `Local{path}` がそのタスクの作業ディレクトリ。相対なら `workspace_root` 基準。
    ///
    /// ADR-0041 D1: ただし worktree を切るタスク（`mode = worktree` かつ `path` が git リポジトリ）では、
    /// **`runs/` `inputs/` `artifacts/` を置く場所**は `workspace_root/<task_id>` になる
    /// （作業ツリーそのものは `<そこ>/tree`。`git status --porcelain` を汚さないため外に出す）。
    fn task_dir(&self, task: &Task) -> Option<PathBuf> {
        match &task.workspace {
            WorkspaceSpec::Local { path, .. } => Some(match self.task_workspaces_for(task) {
                Some(ws) => ws.task_dir,
                None if path.is_absolute() => path.clone(),
                None => self.config.workspace_root.join(path),
            }),
            // ADR-0018 D1: クラスタ側が正で、手元は写し（`workspace_root/<task_id>`）。
            WorkspaceSpec::Remote { .. } => {
                Some(self.config.workspace_root.join(task.id.to_string()))
            }
        }
    }

    /// ADR-0041 D1 / ADR-0043 D2: そのタスクに用意する（用意した）ローカルの作業場所。
    ///
    /// 決め方は決定的（LLM は使わない）:
    ///
    /// 1. **タスクがリポジトリを選んでいる**（`task.repos`、ADR-0043 D2）→ `<task_dir>/repos/<name>/` に
    ///    1 つずつ並べる。`kind = git` で実際に git リポジトリなら worktree、そうでなければ実体への
    ///    シンボリックリンク（`kind = dir`、`mode = shared`、git でなかった場合）。
    ///    **リモートのリポジトリを含むタスクは `None`**（従来の ADR-0018 / 0019 の経路に倒す。
    ///    ローカルと混ぜたタスクは作成時に 422 で弾いてある）。
    /// 2. **選んでいない**（案件にリポジトリが無い・案件に属さない）→ Phase 49 と同じ 1 つだけの worktree
    ///    （`<task_dir>/tree`）。条件は 3 つ: `Local`、`mode = worktree`（既定）、`path` が git リポジトリ。
    ///
    /// base の決め方は `task_worker::resolve_base`（`main` / 本番の `current` / `HEAD`。ADR-0041 D1）。
    fn task_workspaces_for(&self, task: &Task) -> Option<task_worker::TaskWorkspaces> {
        let task_dir = self.config.workspace_root.join(task.id.to_string());
        if task.repos.is_empty() {
            let worktree = self.legacy_worktree_for(task, &task_dir)?;
            let name = task_worker::task_repos::repo_display_name(&worktree.repo);
            return Some(task_worker::TaskWorkspaces {
                task_dir,
                repos: vec![task_worker::TaskRepo::git(name, worktree)],
            });
        }
        let mut repos = Vec::with_capacity(task.repos.len());
        for reference in &task.repos {
            let row = match self.store.repo_get(reference.repo_id) {
                Ok(Some(row)) => row,
                Ok(None) => {
                    tracing::warn!(task_id = %task.id, repo = %reference.name, "the project repo is gone; falling back to the plain workspace");
                    return None;
                }
                Err(e) => {
                    tracing::error!(task_id = %task.id, repo = %reference.name, error = %e, "cannot read the project repo");
                    return None;
                }
            };
            let task_core::WorkspaceSpec::Local { path, mode } = &row.location else {
                // ADR-0043 D2 / D7: リモートは従来の経路（手元の写し + rsync）。
                return None;
            };
            let source = if path.is_absolute() {
                path.clone()
            } else {
                self.config.workspace_root.join(path)
            };
            let dir = task_dir.join(task_worker::REPOS_DIR_NAME).join(&row.name);
            let wants_worktree = row.kind == task_core::RepoKind::Git
                && mode.unwrap_or_default() == task_core::WorkspaceMode::Worktree
                && task_worker::is_git_repo(&source);
            let base = if wants_worktree {
                self.worktree_base(&source)
            } else {
                None
            };
            // A human-approved documentation reconciliation arrives with a committed worktree.
            // Preserve that exact branch for the subsequent ordinary verification/review task.
            if task_dir
                .join("artifacts/reconciliation-plan.json")
                .is_file()
                && let Some(marker) = task_ops::workspace::read_marker(&task_dir)
                && let Some(existing) = marker.repos.iter().find(|r| r.name == row.name)
                && std::path::Path::new(&existing.source) == source
                && std::path::Path::new(&existing.dir) == dir
                && let (Some(branch), Some(sha)) = (&existing.branch, &existing.base)
                && branch.starts_with("docs-reconcile/")
                && task_ops::changes::current_branch(&dir).as_ref() == Some(branch)
            {
                repos.push(task_worker::TaskRepo::git(
                    row.name.clone(),
                    task_worker::LocalWorktree {
                        dir,
                        task_dir: task_dir.clone(),
                        repo: source,
                        branch: branch.clone(),
                        base: task_worker::BaseRef {
                            kind: task_worker::BaseKind::Main,
                            sha: sha.clone(),
                        },
                    },
                ));
                continue;
            }
            match base {
                Some(base) => repos.push(task_worker::TaskRepo::git(
                    row.name.clone(),
                    task_worker::LocalWorktree {
                        dir,
                        task_dir: task_dir.clone(),
                        repo: source,
                        branch: format!("{}{}", self.config.worktree_branch_prefix, task.id),
                        base,
                    },
                )),
                None => repos.push(task_worker::TaskRepo::link(row.name.clone(), source, dir)),
            }
        }
        if repos.is_empty() {
            return None;
        }
        Some(task_worker::TaskWorkspaces { task_dir, repos })
    }

    /// ADR-0043 D3（Phase 56）: **このタスクをコンテナで走らせるか**。ストアと `workspace.toml` を
    /// 読むだけの決定的な判断で、LLM は使わない（DESIGN 原則 1）。
    ///
    /// - リモート（`WorkspaceSpec::Remote`）と作業場所の無いタスクはホスト（ADR-0043 D7 は後続）
    /// - `paperqa` / `local-deep-research` はホスト（道具立てがホストの venv にある。`container::decide`）
    /// - 1 つでも `run = container`（か `auto` + `[run] mode = "container"`）なら**コンテナ**
    /// - runtime が使えなければ `Unavailable`（run を始めず `blocked` にして人に聞く）
    fn container_decision(
        &self,
        task: &Task,
        worktree: Option<&task_worker::TaskWorkspaces>,
        adapter_id: &str,
        remote: bool,
    ) -> ContainerDecision {
        let Some(ws) = worktree else {
            return ContainerDecision::Host;
        };
        if remote {
            return ContainerDecision::Host;
        }
        // リポジトリごとの `run` と `is_primary`。Phase 49 の 1 リポジトリのタスクは
        // `project_repos` の行を持たないので `auto` + primary として扱う。
        let mut inputs: Vec<task_worker::RepoRunInput> = Vec::with_capacity(ws.repos.len());
        for repo in &ws.repos {
            let row = task
                .repos
                .iter()
                .find(|r| r.name == repo.name)
                .and_then(|r| self.store.repo_get(r.repo_id).ok().flatten());
            // `workspace.toml` は作業ツリーがあればそこ、無ければ元のリポジトリ（`repo_notes` と同じ規則）。
            let from = if repo.dir.is_dir() {
                &repo.dir
            } else {
                &repo.source
            };
            let (config, warning) = task_core::workspace_config::load_or_default(from);
            if let Some(warning) = warning {
                tracing::warn!(repo = %repo.name, %warning, "cannot read workspace.toml; using the defaults");
            }
            inputs.push(task_worker::RepoRunInput {
                name: repo.name.clone(),
                run: row
                    .as_ref()
                    .map(|r| r.run)
                    .unwrap_or(task_core::RepoRun::Auto),
                is_primary: row.as_ref().map(|r| r.is_primary).unwrap_or(true),
                config,
                config_dir: from.clone(),
            });
        }
        let Some(choice) = task_worker::container::decide(&inputs, adapter_id) else {
            return ContainerDecision::Host;
        };
        let Some(runtime) = self.container_probe.runtime else {
            return ContainerDecision::Unavailable {
                question: task_worker::container::unavailable_question(
                    &self.container_probe,
                    &choice.repo,
                ),
            };
        };
        // `dir` のリポジトリ（シンボリックリンク）は**実体**を同じパスでマウントする。
        let dir_repos: Vec<PathBuf> = ws
            .repos
            .iter()
            .filter(|r| !r.is_git())
            .map(|r| r.source.clone())
            .collect();
        let (uid, gid) = task_worker::container::host_ids();
        let plan = task_worker::ContainerPlan {
            runtime,
            program: runtime.as_str().to_string(),
            // イメージは `run_worker` が run の直前に決める（ビルドが要ることがある）。
            image: self.config.containers.image_default.clone(),
            task_dir: ws.task_dir.clone(),
            dir_repos,
            creds: Vec::new(),
            extra_mounts: choice.mounts.clone(),
            // ADR-0047 D3（Phase 61）: 知識ベースがあれば同じパスで見せる（`_inbox` だけ書き込み可）。
            knowledge_root: Some(self.config.knowledge.root.clone()).filter(|r| r.is_dir()),
            env: choice.env.clone(),
            task_id: task.id.to_string(),
            uid,
            gid,
        };
        ContainerDecision::Container(Box::new(ContainerRun {
            plan,
            image: choice.image,
            image_default: self.config.containers.image_default.clone(),
            build_root: self.config.containers.build_dir.clone(),
            build_timeout: self.config.containers.build_timeout,
            repo: choice.repo,
        }))
    }

    /// Phase 49（ADR-0041 D1）の 1 リポジトリだけの worktree（`<task_dir>/tree`）。
    fn legacy_worktree_for(
        &self,
        task: &Task,
        task_dir: &Path,
    ) -> Option<task_worker::LocalWorktree> {
        let WorkspaceSpec::Local { path, .. } = &task.workspace else {
            return None;
        };
        if task.workspace.local_mode() != task_core::WorkspaceMode::Worktree {
            return None;
        }
        let repo = if path.is_absolute() {
            path.clone()
        } else {
            self.config.workspace_root.join(path)
        };
        if !task_worker::is_git_repo(&repo) {
            return None;
        }
        let base = self.worktree_base(&repo)?;
        Some(task_worker::LocalWorktree {
            dir: task_dir.join(task_worker::WORKTREE_DIR_NAME),
            task_dir: task_dir.to_path_buf(),
            repo,
            branch: format!("{}{}", self.config.worktree_branch_prefix, task.id),
            base,
        })
    }

    /// ADR-0041 D1 の base の規則（`main` / 本番の `current` / `HEAD`）。
    fn worktree_base(&self, repo: &Path) -> Option<task_worker::BaseRef> {
        let current = self
            .config
            .releases_dir
            .as_deref()
            .and_then(task_worker::current_release_sha);
        task_worker::resolve_base(repo, current.as_deref())
    }

    /// ADR-0043 D2 / D4 / D8: 前置きの「作業場所」に出すリポジトリ一覧（決定的。`workspace.toml` を
    /// 読むだけで、判断も LLM も無い）。`description` / `check` / `outputs` は
    /// `.config/celeris/workspace.toml` に**書いてあることだけ**を使う（ADR-0043 §3）。
    ///
    /// 読む場所は、作業ツリーが既にあればそこ、無ければ元のリポジトリ（初回の dispatch では
    /// worktree をまだ切っていないため）。
    fn repo_notes(&self, ws: &task_worker::TaskWorkspaces) -> Vec<task_worker::preamble::RepoNote> {
        ws.repos
            .iter()
            .map(|repo| {
                let from = if repo.dir.is_dir() { &repo.dir } else { &repo.source };
                let (config, warning) = task_core::workspace_config::load_or_default(from);
                if let Some(warning) = warning {
                    tracing::warn!(repo = %repo.name, %warning, "cannot read workspace.toml; using the defaults");
                }
                task_worker::preamble::RepoNote {
                    name: repo.name.clone(),
                    dir: repo.dir.to_string_lossy().into_owned(),
                    git: repo.is_git(),
                    branch: repo.branch().map(str::to_string),
                    base: repo.worktree.as_ref().map(|w| w.base.sha12()),
                    base_kind: repo.worktree.as_ref().map(|w| w.base.kind.as_str().to_string()),
                    description: config.workspace.description.clone(),
                    check: config.commands.check.clone(),
                    docs: config.outputs.docs.clone(),
                    deliverables: config.outputs.deliverables.clone(),
                }
            })
            .collect()
    }

    /// ADR-0043 D2 / D4: 計画 run に渡す「この案件のリポジトリ」（名前 / 種類 / 置き場 / `workspace.toml` の
    /// `description`）。ストアと `workspace.toml` を読むだけで、判断も LLM も無い。
    fn project_repo_notes(
        &self,
        task: &Task,
    ) -> Result<Vec<task_worker::preamble::ProjectRepoNote>, DispatchError> {
        let repos =
            task_ops::delegate::project_repos(self.store.as_ref(), task).map_err(ops_to_store)?;
        Ok(repos
            .into_iter()
            .map(|repo| {
                let (location, local) = match &repo.location {
                    WorkspaceSpec::Local { path, .. } => {
                        (path.display().to_string(), Some(path.clone()))
                    }
                    WorkspaceSpec::Remote { cluster, path, .. } => {
                        (format!("{cluster}:{}", path.display()), None)
                    }
                };
                let description = local.as_deref().and_then(|dir| {
                    task_core::workspace_config::load_or_default(dir)
                        .0
                        .workspace
                        .description
                });
                task_worker::preamble::ProjectRepoNote {
                    name: repo.name,
                    kind: repo.kind.as_str().to_string(),
                    location,
                    description,
                    is_primary: repo.is_primary,
                }
            })
            .collect())
    }

    /// ADR-0043 D4: レビュー担当の `Check::Command` の既定になる検査コマンド
    /// （タスクに `acceptance` が明示されていればそれが勝つ。決めるのはここではなく `review.rs` の
    /// 呼び出し側）。**先頭のリポジトリの** `[commands] check` だけを使う。
    /// ADR-0069 D3 / D6（Phase 114）: このタスクの lane を決める（`routing` を持つ execute タスクだけ。
    /// それ以外は `None` で従来どおり `worker_hint.tier`）。担当の実効 profile の天井（`allowed_tiers` /
    /// `budget.max_lane`）で丸め、やり直し（`attempts > 0`）ならイベントの履歴から
    /// `EscalationPolicy` で 1 段まで上げる。LLM は使わない（DESIGN 原則 1）。
    fn decide_lane(&self, task: &Task) -> Result<Option<task_core::LaneDecision>, DispatchError> {
        if task.routing.is_none() || task.kind != TaskKind::Execute {
            return Ok(None);
        }
        let org = self.store.org_list()?;
        let profile = task
            .assignee
            .as_deref()
            .filter(|_| !org.is_empty())
            .map(|a| task_core::profile::resolve(&org, a));
        let ceiling = profile
            .as_ref()
            .map(|p| p.lane_ceiling())
            .unwrap_or_default();
        let Some(mut decision) = task_core::model_policy::decide_for_task(task, &ceiling) else {
            return Ok(None);
        };
        if task.attempts > 0 && decision.source.policy_decides() {
            let events: Vec<Event> = self
                .store
                .events_for(task.id)?
                .into_iter()
                .map(|(_, e)| e)
                .collect();
            let history = task_core::retry_policy::attempt_history(task, &events);
            let policy = task_core::EscalationPolicy::for_task(task, profile.as_ref());
            let next = policy.decide(&history, decision.lane, task_core::BudgetState::Ok);
            if next.lane() != decision.lane {
                decision.reasons.push(format!(
                    "retry lane {:?} -> {:?}",
                    decision.lane,
                    next.lane()
                ));
            }
            decision.lane = next.lane();
            decision.escalation = Some(next.describe());
            tracing::info!(task_id = %task.id, attempts = task.attempts, decision = %next.describe(), "retry lane decided (ADR-0069 D6)");
        }
        Ok(Some(decision))
    }

    /// ADR-0046 D5（Phase 59）: `assignee` が無い `ready` のタスクの担当を**決定的に**決める。
    ///
    /// - 決まったら `Event::Assigned { node, score, reason }` を残して担当を書き戻し、そのタスクを返す。
    /// - 候補が 1 つも無ければ `blocked` にして人に聞き（ADR-0021 の質問経路）、`None` を返す。
    /// - matching の対象でない（担当が居る・ハーネスが無い）タスクはそのまま返す。
    ///
    /// LLM は使わない（DESIGN 原則 1）。
    fn assign_if_needed(&mut self, task: Task) -> Result<Option<Task>, DispatchError> {
        use task_ops::matching::Assignment;
        let org = self.store.org_list()?;
        match task_ops::matching::decide(&org, &task) {
            Assignment::NotApplicable => Ok(Some(task)),
            Assignment::Assigned {
                node,
                score,
                reason,
            } => {
                let mut updated = task.clone();
                updated.assignee = Some(node.clone());
                updated.updated_at = OffsetDateTime::now_utc();
                let event = Event::Assigned {
                    node: node.clone(),
                    score,
                    reason: reason.clone(),
                };
                match self.store.update_task(&updated, event) {
                    Ok(stored) => {
                        tracing::info!(
                            task_id = %task.id, assignee = %node, score, reason = %reason,
                            "matching decided the assignee (ADR-0046 D5)"
                        );
                        Ok(Some(stored))
                    }
                    Err(e) => {
                        tracing::warn!(task_id = %task.id, error = %e, "could not write the matched assignee");
                        Ok(Some(task))
                    }
                }
            }
            Assignment::Unroutable { question } => {
                // Phase 44 と同じ規律: ディスパッチャ由来の質問も `approvals` に残す（そうしないと
                // 認可画面に出ず、Discord にも飛ばない）。
                let now = OffsetDateTime::now_utc();
                if let Err(e) = crate::approvals::record_question_approval(
                    self.store.as_ref(),
                    &task,
                    &question,
                    now,
                ) {
                    tracing::warn!(task_id = %task.id, error = %e, "failed to record the approval for the unroutable question");
                }
                let run_id = format!("matching-{}", task.id);
                let events = vec![Event::QuestionRaised {
                    run_id,
                    text: question,
                }];
                match self
                    .store
                    .apply_transition_with_events(task.id, Trigger::Unroutable, events)
                {
                    Ok(_) => {
                        tracing::info!(task_id = %task.id, "no org node can take this task; asking a human (ADR-0046 D5)")
                    }
                    Err(StoreError::InvalidTransition(e)) => {
                        tracing::warn!(task_id = %task.id, error = %e, "unroutable transition could not be applied");
                    }
                    Err(e) => return Err(e.into()),
                }
                Ok(None)
            }
        }
    }

    /// ADR-0062 B1（Phase 107）: 担当が `cluster:<id>` を持たない remote タスクを `blocked` にし、
    /// `assign_if_needed` の `Assignment::Unroutable` と同じ流儀（`approvals` に 1 件、
    /// `Trigger::Unroutable` で `ready → blocked`）で人に質問する。人が答える（または担当・tools を
    /// 変える）と次の tick で `ready` に戻り、そのとき改めて `cluster_of` が評価し直す。
    fn block_task_missing_cluster_tool(
        &mut self,
        task: &Task,
        cluster: &str,
    ) -> Result<(), DispatchError> {
        let assignee = task.assignee.as_deref().unwrap_or("(unknown)");
        let wanted = format!("{}{cluster}", task_core::CLUSTER_TOOL_PREFIX);
        let org = self.store.org_list().unwrap_or_default();
        let holders: Vec<&str> = org
            .iter()
            .filter(|n| task_core::resolve_profile(&org, &n.id).has_tool(&wanted))
            .map(|n| n.id.as_str())
            .collect();
        let question = format!(
            "担当 `{assignee}` には道具 `{wanted}` が無いため、このタスクは {cluster} で実行できません。\
             組織画面で担当の tools に `{wanted}` を足す{}、または作業場所をローカルに変えてください。",
            if holders.is_empty() {
                "か、担当を変える".to_string()
            } else {
                format!("、担当を `{}` などに変える", holders.join("` / `"))
            }
        );
        let now = OffsetDateTime::now_utc();
        if let Err(e) =
            crate::approvals::record_question_approval(self.store.as_ref(), task, &question, now)
        {
            tracing::warn!(task_id = %task.id, error = %e, "failed to record the approval for the missing-cluster-tool question");
        }
        let run_id = format!("cluster-routing-{}", task.id);
        let events = vec![Event::QuestionRaised {
            run_id,
            text: question,
        }];
        match self
            .store
            .apply_transition_with_events(task.id, Trigger::Unroutable, events)
        {
            Ok(_) => {
                tracing::info!(task_id = %task.id, %assignee, %cluster, "assignee lacks the cluster tool; blocked and asked a human (ADR-0062 B1)");
            }
            Err(StoreError::InvalidTransition(e)) => {
                tracing::warn!(task_id = %task.id, error = %e, "missing-cluster-tool transition could not be applied");
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    fn default_checks(&self, task: &Task) -> Vec<String> {
        let Some(ws) = self.task_workspaces_for(task) else {
            return Vec::new();
        };
        let Some(repo) = ws.repos.first() else {
            return Vec::new();
        };
        let from = if repo.dir.is_dir() {
            &repo.dir
        } else {
            &repo.source
        };
        task_core::workspace_config::load_or_default(from)
            .0
            .commands
            .check
    }

    /// テスト用: Phase 49 のときの「1 つだけの worktree」の姿（`task_workspaces_for` の先頭）。
    #[cfg(test)]
    fn local_worktree_for(&self, task: &Task) -> Option<task_worker::LocalWorktree> {
        self.task_workspaces_for(task)
            .and_then(|ws| ws.repos.into_iter().next())
            .and_then(|r| r.worktree)
    }

    /// ADR-0043 D2: ワーカーのカレントディレクトリ（先頭のリポジトリ）。レビューの判定コマンドもここで動かす。
    fn work_dir_for(&self, task: &Task) -> Option<PathBuf> {
        self.task_workspaces_for(task)
            .and_then(|ws| ws.cwd().map(Path::to_path_buf))
    }

    /// ADR-0043 D2（Phase 52。ADR-0041 D1 の後片付けを改める）: worktree は**終端では消さない**
    /// （`done` で未取り込みのものも `failed` も、差分を見るために残す）。消えるのは**中止**（cancel）
    /// のときだけで、worktree を消し、ブランチも `git branch -D` する（人の指示）。
    ///
    /// `done` / `failed` のときは、未コミットの変更があれば `WorkerProgress` を 1 行積んで記録だけ落とす。
    /// **celeris はコミットしない**（ADR-0019 D2）。
    fn cleanup_cancelled_worktrees(&mut self) -> Result<(), DispatchError> {
        let ids: Vec<TaskId> = self.task_workspaces.keys().copied().collect();
        for id in ids {
            // まだ走っている／判定中なら触らない（やり直しは同じ worktree を使い回す）。
            if self.running.contains_key(&id) || self.reviewing.contains_key(&id) {
                continue;
            }
            let status = match self.store.get(id)? {
                Some(t) => Some(t.status),
                // タスクごと消えていれば、記録も落とす（worktree は人の手に残す）。
                None => None,
            };
            match status {
                None => {
                    self.task_workspaces.remove(&id);
                }
                Some(Status::Cancelled) => {
                    let Some(workspaces) = self.task_workspaces.remove(&id) else {
                        continue;
                    };
                    for (name, outcome) in workspaces.remove_for_cancel() {
                        match outcome {
                            task_worker::CleanupOutcome::Removed => {
                                tracing::info!(task_id = %id, repo = %name, "cancelled: the worktree and its branch are gone");
                            }
                            task_worker::CleanupOutcome::AlreadyGone => {}
                            other => {
                                tracing::warn!(task_id = %id, repo = %name, outcome = ?other, "cancelled: could not remove the worktree; leaving it for a human");
                            }
                        }
                    }
                }
                Some(status) if status.is_terminal() => {
                    // ADR-0043 D2: 残す。未コミットの変更があることだけ 1 行記録する。
                    let Some(workspaces) = self.task_workspaces.remove(&id) else {
                        continue;
                    };
                    let dirty: Vec<String> = workspaces
                        .repos
                        .iter()
                        .filter(|r| {
                            r.is_git() && task_worker::status_is_clean(&r.dir) == Some(false)
                        })
                        .map(|r| r.dir.to_string_lossy().into_owned())
                        .collect();
                    if !dirty.is_empty() {
                        let run_id = last_run_id(&self.store.events_for(id)?).unwrap_or_default();
                        self.store.append_event(
                            id,
                            &Event::worker_progress(
                                run_id,
                                format!("未コミットの変更が残っています: {}", dirty.join(", ")),
                            ),
                        )?;
                    }
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// ADR-0066 D2（Phase 110b）: 終端（done / failed / cancelled）になってから
    /// `workspace_prune_after_secs` 経った作業場所から、ビルド生成物（`target/` 等）だけを刈る
    /// （ソースツリーと `artifacts/` は残す。ADR-0043 D2 の「終端では worktree を消さない」は変えない）。
    ///
    /// **1 tick に最大 1 か所**。探すところ（ストアの読み取りとメタデータの存在確認）までは同期で行う
    /// （軽い）。実際の削除は別スレッドに逃がし、終わったら `workspace_pruned` イベントを積む
    /// （tick はそれを待たない。`--mode verify` の煙試験インスタンスでは何もしない）。
    fn prune_one_workspace(&mut self) {
        if self.eligible.is_some() {
            return;
        }
        if self.config.workspace_prune_after_secs == 0 {
            return;
        }
        let now = OffsetDateTime::now_utc();
        let candidate = match task_worker::workspace_prune::find_prune_candidate(
            self.store.as_ref(),
            &self.config.workspace_root,
            now,
            self.config.workspace_prune_after_secs,
        ) {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "workspace prune: could not scan for candidates");
                return;
            }
        };
        let store = self.store.clone();
        let task_id = candidate.task_id;
        let task_dir = candidate.task_dir.clone();
        let thread = std::thread::Builder::new()
            .name("celeris-workspace-prune".to_string())
            .spawn(move || {
                let removed = task_worker::workspace_prune::prune(&candidate);
                if removed.is_empty() {
                    return;
                }
                let removed = task_worker::workspace_prune::relative_removed(&task_dir, &removed);
                tracing::info!(task_id = %task_id, removed = ?removed, "workspace: pruned build artifacts");
                if let Err(e) =
                    store.append_event(task_id, &Event::WorkspacePruned { removed })
                {
                    tracing::warn!(task_id = %task_id, error = %e, "workspace prune: could not record the event");
                }
            });
        if let Err(e) = thread {
            tracing::warn!(task_id = %task_id, error = %e, "workspace prune: could not spawn the prune thread");
        }
    }

    /// ADR-0036 D1: そのタスクの成果物ディレクトリ（`<dir>/artifacts` か `<dir>/.taskd/artifacts/<task_id>`）。
    /// 判定は `task_core::artifacts`（純粋関数）。アダプタ・レビュー・記憶の読み取りはすべてこれを使う。
    fn artifacts_dir(&self, task: &Task, workspace_dir: &Path) -> PathBuf {
        task_core::artifacts::artifacts_dir_for(task, workspace_dir)
    }

    /// ADR-0018: `WorkspaceSpec::Remote` のタスクのクラスタ設定とリモートのパス。ローカルのタスクは `None`。
    ///
    /// ADR-0062 B1（Phase 107）: **担当が `cluster:<id>` を持たないなら接続経路を渡さない**（remote を
    /// 組まない）。人間の指示により、ADR-0046 D8 の「道具を 1 つも宣言していないノードは従来どおり通す」
    /// という例外は**クラスタの利用可否についてだけ**廃止した（`task_may_use_cluster`）。
    ///
    /// ADR-0059 D6: `path` が絶対・`~`/`~/…` ならそのまま、それ以外（省略・相対）は実効 `work_dir`
    /// （DB 上書き `cluster_settings` > 設定の `[[clusters]] work_dir` > 無し）からの相対に解決する。
    /// 解決できなければ `path` をそのまま返す（`remote_dir_is_resolved` で検出できる形のまま。
    /// ここでは `None` にしない — 「クラスタが無い」とは別の失敗なので `unroutable` に混ぜない）。
    fn cluster_of(&mut self, task: &Task) -> Option<(ClusterSpec, PathBuf, WorkspaceMode)> {
        match self.resolve_cluster(task) {
            ClusterResolution::Resolved(spec, path, mode) => Some((*spec, path, mode)),
            ClusterResolution::Local
            | ClusterResolution::NotConfigured
            | ClusterResolution::AssigneeLacksTool { .. } => None,
        }
    }

    /// `cluster_of` の中身。`None` の理由を dispatch_ready が区別できるようにする（ADR-0062 B1）。
    fn resolve_cluster(&mut self, task: &Task) -> ClusterResolution {
        match &task.workspace {
            WorkspaceSpec::Local { .. } => ClusterResolution::Local,
            WorkspaceSpec::Remote { cluster, path, .. } => {
                if !self.task_may_use_cluster(task, cluster) {
                    return ClusterResolution::AssigneeLacksTool {
                        cluster: cluster.clone(),
                    };
                }
                let Some(spec) = self.config.clusters.get(cluster).cloned() else {
                    return ClusterResolution::NotConfigured;
                };
                let db_work_dir = self
                    .store
                    .cluster_settings_get(cluster)
                    .ok()
                    .flatten()
                    .and_then(|s| s.work_dir)
                    .map(PathBuf::from);
                let work_dir = db_work_dir.or_else(|| spec.work_dir.clone());
                let resolved =
                    resolve_remote_dir(path, work_dir.as_deref()).unwrap_or_else(|| path.clone());
                ClusterResolution::Resolved(Box::new(spec), resolved, task.workspace.remote_mode())
            }
        }
    }

    /// ADR-0062 B1: そのタスクの担当がそのクラスタを使えるか（決定的。組織の profile だけを見る）。
    /// 人間の指示（Phase 107 追加指示）により、ADR-0046 D8 の「道具を 1 つも宣言していないノードは
    /// 従来どおり通す」という例外はここでは**廃止した**（remote 作業場所でクラスタに繋いでよいのは、
    /// 実効 profile に `cluster:<id>` を持つノードだけ）。warn はタスクごとに 1 回
    /// （`warned_cluster_tool`。`warned_unroutable` と同じ流儀）。
    fn task_may_use_cluster(&mut self, task: &Task, cluster: &str) -> bool {
        let Some(assignee) = task.assignee.as_deref() else {
            return true;
        };
        let org = match self.store.org_list() {
            Ok(org) => org,
            Err(e) => {
                tracing::warn!(task_id = %task.id, error = %e, "could not read the org tree; allowing the cluster");
                return true;
            }
        };
        let effective = task_core::resolve_profile(&org, assignee);
        let wanted = format!("{}{cluster}", task_core::CLUSTER_TOOL_PREFIX);
        if effective.has_tool(&wanted) {
            self.warned_cluster_tool.remove(&task.id);
            return true;
        }
        if self.warned_cluster_tool.insert(task.id) {
            tracing::warn!(
                task_id = %task.id, %assignee, %cluster,
                "assignee does not have the cluster tool; not wiring the remote (ADR-0062 B1)"
            );
        }
        false
    }

    /// そのクラスタで走っている run の数（ADR-0018 D5: プロバイダとクラスタの二次元）。
    fn cluster_in_use(&self, cluster: &str) -> usize {
        self.running
            .values()
            .filter(|e| e.cluster.as_deref() == Some(cluster))
            .count()
            + self
                .reviewing
                .values()
                .filter(|e| e.cluster.as_deref() == Some(cluster))
                .count()
    }

    fn is_idle(&self) -> Result<bool, DispatchError> {
        if !self.running.is_empty() || !self.reviewing.is_empty() {
            return Ok(false);
        }
        if !self.store.list(Some(Status::Running))?.is_empty() {
            return Ok(false);
        }
        // ADR-0010 D8: 人間の承認待ちで延期中の reviewing は、人間が操作しない限り進まないので idle とみなす。
        if self.store.list(Some(Status::Reviewing))?.iter().any(|t| {
            !self.awaiting_human.contains(&t.id) && !self.awaiting_children.contains_key(&t.id)
        }) {
            return Ok(false);
        }
        // ADR-0012 D2（P-33）: 設定に合うプロバイダが無い ready タスクは、設定を直さない限り進まないので待ち対象から外す。
        // 窓いっぱいに返ってきた場合は窓の外に実行可能なタスクが残りうるので idle にしない（次 tick で窓が広がる）。
        let window = self.ready_window();
        let ready = self.store.ready_tasks(window)?;
        if ready.len() >= window {
            return Ok(false);
        }
        // ADR-0041 D5: 面倒を見ないタスク（verify の非 `smoke`）は、このインスタンスでは決して進まないので
        // 待ち対象に数えない。
        Ok(ready.iter().all(|t| {
            !self.is_eligible(t)
                || self.unroutable.contains(&t.id)
                || self.cluster_waiting.contains(&t.id)
        }))
    }
}

/// ADR-0043 D3 / D4: `[commands] setup` の 1 コマンドあたりの上限（設定キーにはしない。
/// `cargo fetch` / `pnpm install` が入る想定で、run の予算とは別に取る）。
const SETUP_TIMEOUT: Duration = Duration::from_secs(1800);

/// ADR-0043 D3（Phase 56）: 起動時の `<runtime> info` の上限（届かない docker デーモンで固まらない）。
const CONTAINER_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// ADR-0041 D1 / ADR-0043 D2: `<task_dir>/worktree.json` の中身。先頭の 5 つは Phase 49 からある
/// 「1 リポジトリのときの姿」で、複数リポジトリのタスクでは `repos[0]`（cwd になるもの）の写しが入る。
fn worktree_marker(ws: &task_worker::TaskWorkspaces) -> task_ops::workspace::WorktreeMarker {
    let first = ws.repos.first();
    task_ops::workspace::WorktreeMarker {
        repo: first
            .map(|r| r.source.to_string_lossy().into_owned())
            .unwrap_or_default(),
        dir: first
            .map(|r| r.dir.to_string_lossy().into_owned())
            .unwrap_or_default(),
        branch: first
            .and_then(|r| r.branch())
            .unwrap_or_default()
            .to_string(),
        base: first
            .and_then(|r| r.worktree.as_ref())
            .map(|w| w.base.sha.clone())
            .unwrap_or_default(),
        base_kind: first
            .and_then(|r| r.worktree.as_ref())
            .map(|w| w.base.kind.as_str().to_string())
            .unwrap_or_default(),
        repos: ws
            .repos
            .iter()
            .map(|r| task_ops::workspace::WorktreeMarkerRepo {
                name: r.name.clone(),
                kind: if r.is_git() {
                    "git".into()
                } else {
                    "dir".into()
                },
                source: r.source.to_string_lossy().into_owned(),
                dir: r.dir.to_string_lossy().into_owned(),
                branch: r.branch().map(str::to_string),
                base: r.worktree.as_ref().map(|w| w.base.sha.clone()),
                base_kind: r
                    .worktree
                    .as_ref()
                    .map(|w| w.base.kind.as_str().to_string()),
            })
            .collect(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_worker(
    store: Arc<dyn TaskStore>,
    adapter: Arc<dyn WorkerAdapter>,
    task_id: TaskId,
    execution_tier: task_core::Tier,
    dir: PathBuf,
    run_id: &str,
    limits: RunLimits,
    lease: LeaseRenewal,
    remote: Option<SshSettings>,
    worktree: Option<task_worker::TaskWorkspaces>,
    extras: RunExtras,
    roles: Vec<RoleSpec>,
    genres: Vec<GenreSpec>,
    delegation: DelegationLimits,
    account: Option<String>,
    account_book: Option<Arc<StdMutex<AccountBook>>>,
    container: ContainerDecision,
    // ADR-0066 D1（Phase 110b）: `[workspace] shared_build_cache` が有効なときだけ `Some`
    // （`<build_cache_dir>`）。ローカルの git worktree のホスト実行に限って
    // `CARGO_TARGET_DIR=<build_cache_dir>/cargo/<repo-key>` を与える（コンテナ・Remote は対象外。
    // `container_plan` が決まってから判断する）。
    build_cache_dir: Option<PathBuf>,
) -> Result<RunOutcome, AdapterError> {
    // リース取得後の状態（running, lease あり）をワーカーに渡す。
    let mut task = store
        .get(task_id)
        .map_err(|e| AdapterError::Other(format!("store: {e}")))?
        .ok_or_else(|| AdapterError::Other("task vanished".into()))?;
    task.worker_hint.tier = execution_tier;
    // ADR-0052 D2（Phase 64）: フォールバックした知識整理 run は、DB のタスクではなく**ワーカーに渡す
    // 写し**だけを書き換える（専用アダプタの固定を外し、予算を `max_turns = 8` / `max_wall_secs = 600` に）。
    if let Some(fallback) = &extras.knowledge_fallback {
        task.worker_hint.adapter = None;
        task.budget = fallback.budget;
        tracing::debug!(task_id = %task_id, adapter = %fallback.adapter, "knowledge: running the fallback extraction");
    }
    // ADR-0018 D1/D3: リモート実行のタスクは、クラスタの内容を写しに取り込み、ラッパを置き、その使い方を指示文に足す
    // （DB のタスクは変えない。ワーカーに渡す写しだけ）。ADR-0059 D6: `path` が実効 `work_dir` から解決
    // できなかった（`cluster_of` が絶対・`~` 始まりに直せなかった）ら、worktree 準備を試す前にここで
    // 明確なエラーにする。
    let workspace = match &remote {
        Some(settings) => {
            if !remote_dir_is_resolved(&settings.remote_dir) {
                return Err(AdapterError::Other(format!(
                    "cluster {}: no working directory registered for {:?}; register one via \
                     `PUT /clusters/{}/settings` or the clusters screen (ADR-0059 D6)",
                    settings.cluster, settings.remote_dir, settings.cluster
                )));
            }
            let mut effective_settings = settings.clone();
            let mut ws = SshWorkspace::new(&dir, effective_settings.clone());
            let prepared = match ws.prepare(&task).await {
                Ok(p) => p,
                // ADR-0059 D3: 自動の格下げ。`mode` 省略・`repos` 無し（コードを触らない仕事）なら、
                // worktree が切れなくても失敗にせず `shared`（`SyncMode::None`）として同じ run の中で
                // 続行する。明示的に `mode = "worktree"` を選んだタスクは格下げしない（利用者の意図を
                // 尊重し、設定ミスを隠さない）。
                Err(WorkspaceError::NotAGitRepository(reason))
                    if remote_mode_omitted(&task.workspace) && task.repos.is_empty() =>
                {
                    effective_settings.sync = SyncMode::None;
                    ws = SshWorkspace::new(&dir, effective_settings.clone());
                    let downgraded = ws.prepare(&task).await.map_err(|e| {
                        workspace_error_to_adapter(e, "workspace prepare (downgraded to shared)")
                    })?;
                    if let Some(new_workspace) = downgraded_remote_workspace(&task.workspace) {
                        if let Ok(Some(mut fresh)) = store.get(task_id) {
                            fresh.workspace = new_workspace.clone();
                            fresh.updated_at = OffsetDateTime::now_utc();
                            if let Err(e) = store.update_task(
                                &fresh,
                                Event::WorkspaceModeDowngraded {
                                    cluster: effective_settings.cluster.clone(),
                                    path: effective_settings
                                        .remote_dir
                                        .to_string_lossy()
                                        .into_owned(),
                                    reason,
                                },
                            ) {
                                tracing::warn!(task_id = %task_id, error = %e, "could not persist the workspace mode downgrade (ADR-0059 D3)");
                            }
                        }
                        task.workspace = new_workspace;
                    }
                    downgraded
                }
                Err(e) => return Err(workspace_error_to_adapter(e, "workspace prepare")),
            };
            ws.write_remote_exec_helper()
                .await
                .map_err(|e| workspace_error_to_adapter(e, "remote-exec helper"))?;
            task.objective
                .push_str(&remote_exec_instructions(&effective_settings));
            prepared
        }
        // ADR-0041 D1 / ADR-0043 D2: ローカルの作業場所（1 つ以上のリポジトリ）を用意し、その
        // 先頭をワーカーのカレントディレクトリにする（`runs/` `inputs/` `artifacts/` は作業ツリーの外の `dir`）。
        None => {
            let mut ws = LocalWorkspace::new(&dir);
            if let Some(wt) = &worktree {
                wt.ensure()
                    .await
                    .map_err(|e| workspace_error_to_adapter(e, "worktree prepare"))?;
                // 目印（`<task_dir>/worktree.json`）: API・CLI はこれを見て「run のログと成果物は
                // 作業ツリーの外にある」と判断する（git を起こさない。worktree を消した後も残す）。
                if let Err(e) =
                    task_ops::workspace::write_marker(&wt.task_dir, &worktree_marker(wt))
                {
                    tracing::warn!(task_id = %task_id, error = %e, "could not write the worktree marker");
                }
                if let Some(cwd) = wt.cwd() {
                    ws = ws.with_work_dir(cwd);
                }
            }
            ws.prepare(&task)
                .await
                .map_err(|e| AdapterError::Other(format!("workspace prepare: {e}")))?
        }
    };
    // ADR-0043 D3（Phase 56）: この run の実行環境。コンテナなら**イメージをここで用意する**
    // （`[container] image` はそのまま、`dockerfile` は内容の sha のタグでビルドしてキャッシュ）。
    // runtime が無い・ビルドが落ちたときは run を始めず、`setup` の失敗と同じ経路で人に聞く。
    let container_plan: Option<task_worker::SharedPlan> = match container {
        ContainerDecision::Host => None,
        ContainerDecision::Unavailable { question } => {
            tracing::warn!(task_id = %task_id, %question, "no container runtime; asking a human");
            return Ok(RunOutcome {
                terminal: Terminal::Question { text: question },
                exit_code: None,
            });
        }
        ContainerDecision::Container(run) => {
            let mut run = *run;
            let log_path = dir.join(task_worker::container::BUILD_LOG);
            let resolved = match task_worker::container::resolve_image(
                &run.image,
                &run.image_default,
                &run.build_root,
            ) {
                Ok(resolved) => resolved,
                Err(e) => {
                    tracing::warn!(task_id = %task_id, error = %e, "cannot resolve the container image");
                    return Ok(RunOutcome {
                        terminal: Terminal::Question {
                            text: task_worker::container::image_question(&run.repo, &e, &log_path),
                        },
                        exit_code: None,
                    });
                }
            };
            match resolved {
                task_worker::container::ResolvedImage::Ready(tag) => run.plan.image = tag,
                task_worker::container::ResolvedImage::Build(request) => {
                    let program = run.plan.program.clone();
                    let exists =
                        move |tag: &str| task_worker::container::image_exists(&program, tag);
                    if let Err(e) = task_worker::container::ensure_image(
                        &run.plan.program,
                        &request,
                        run.build_timeout,
                        &log_path,
                        &exists,
                    )
                    .await
                    {
                        tracing::warn!(task_id = %task_id, error = %e, "container image build failed");
                        return Ok(RunOutcome {
                            terminal: Terminal::Question {
                                text: task_worker::container::image_question(
                                    &run.repo, &e, &log_path,
                                ),
                            },
                            exit_code: None,
                        });
                    }
                    run.plan.image = request.tag;
                }
            }
            tracing::info!(task_id = %task_id, image = %run.plan.image, runtime = run.plan.runtime.as_str(), repo = %run.repo, "running in a container");
            Some(Arc::new(run.plan))
        }
    };
    // ADR-0043 D3 / D4: worktree を作った直後に `[commands] setup` を**一度だけ**流す
    // （記録は `runs/setup.log`。そのファイルがあれば済んでいる）。コンテナのタスクは
    // **コンテナの中で**流す（Phase 56）。落ちたら run を始めず、既存の質問の経路で
    // タスクを `blocked` にして人に聞く。
    if let Some(wt) = &worktree
        && remote.is_none()
        && !wt
            .task_dir
            .join(task_worker::task_repos::SETUP_LOG)
            .exists()
    {
        match task_worker::run_setup_in(
            &wt.repos,
            &wt.task_dir,
            SETUP_TIMEOUT,
            container_plan.as_ref(),
        )
        .await
        {
            Ok(outcome) if outcome.ok => {}
            Ok(outcome) => {
                tracing::warn!(task_id = %task_id, failures = ?outcome.failures, "setup failed; asking a human");
                return Ok(RunOutcome {
                    terminal: Terminal::Question {
                        text: format!(
                            "setup が失敗しました（`.config/celeris/workspace.toml` の `[commands] setup`）: {}。\
                             記録は `{}` にあります。直し方を教えてください（設定を直す／この手順を飛ばす）。",
                            outcome.failures.join(" / "),
                            wt.task_dir
                                .join(task_worker::task_repos::SETUP_LOG)
                                .display()
                        ),
                    },
                    exit_code: None,
                });
            }
            Err(e) => {
                tracing::warn!(task_id = %task_id, error = %e, "could not run setup");
                return Ok(RunOutcome {
                    terminal: Terminal::Question {
                        text: format!(
                            "setup が失敗しました（`.config/celeris/workspace.toml` の `[commands] setup` を流せませんでした）: {e}。\
                             直し方を教えてください（設定を直す／この手順を飛ばす）。"
                        ),
                    },
                    exit_code: None,
                });
            }
        }
    }
    // ADR-0041 D1 / ADR-0043 D2: ワーカーの cwd は先頭のリポジトリ（`workspace` は足回りの親のまま）。
    let work_dir = worktree
        .as_ref()
        .and_then(|wt| wt.cwd())
        .map(|cwd| cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()));
    // ADR-0036 D1: 成果物ディレクトリはタスクごと（共有 workspace では `.taskd/artifacts/<task_id>/`）。
    // 決めるのはディスパッチャで、アダプタは `req.artifacts_dir` に書くだけ。
    let artifacts_dir = task_core::artifacts::artifacts_dir_for(&task, &workspace);
    // ADR-0067 D3: run 完了後に未申告の成果物を拾うための控え（`workspace`/`artifacts_dir` はこの後
    // `req` に移る）。git worktree ではない local の作業場所に限る（`remote.is_none() &&
    // worktree.is_none()` を後段で見る）。
    let workspace_for_undeclared_scan = workspace.clone();
    let artifacts_dir_for_undeclared_scan = artifacts_dir.clone();
    if task.kind == TaskKind::Plan {
        // ADR-0007 D1: 前回の run の plan.json を今回の出力と誤読しない。
        let _ = tokio::fs::remove_file(artifacts_dir.join(PLAN_FILE_NAME)).await;
    }
    let events = store
        .events_for(task_id)
        .map_err(|e| AdapterError::Other(format!("store: {e}")))?;
    let prior_review = to_prior_review(prior_review_from_events(&events));
    // ADR-0044 D2: 対話 run（人への返事だけをする run。Phase 28）にはコメントの書き方を出さない。
    let writes_comments = extras.conversation_addressee.is_none();
    let req = RunRequest {
        protocol: PROTOCOL_VERSION,
        task: task.clone(),
        workspace,
        work_dir,
        artifacts_dir,
        context: RunContext {
            prior_review,
            inputs: task.inputs.clone(),
            answers: to_answers(answers_from_events(&events)),
            review: None,
            role: extras.role,
            children: extras.children,
            available_genres: extras.available_genres,
            node: extras.node,
            memory: extras.memory,
            conversation: extras.conversation,
            // ADR-0033 D5（Phase 26）: 担当宛て + 全員向けの永続の認可。
            standing_rules: extras.standing_rules,
            organization: extras.organization,
            // ADR-0059 D6（Phase 99）: CoS の対話 run だけに入る。
            clusters: extras.clusters,
            conversation_addressee: extras.conversation_addressee,
            work_genre: extras.work_genre,
            recent_work: extras.recent_work,
            // Phase 41（ADR-0038 D1）: 途中目標レビューの対話 run だけに入る。
            milestone_review: extras.milestone_review,
            // Phase 43（ADR-0039 D3）: 案件が作業場所を決めている run だけに入る。
            workspace_note: extras.workspace_note,
            knowledge: extras.knowledge,
            // ADR-0044 D2（Phase 53）: コメントの糸と、直前の run を止めた人のコメント。
            comments: extras.comments,
            interrupt: extras.interrupt,
            // ADR-0046 D1 / D4（Phase 59）: 実効 profile と進め方（どちらも既定なら `None`）。
            profile: extras.profile,
            mode: extras.mode,
            // 仕事の run はコメントを書ける。**対話 run は書かせない**（Phase 28 の「返事だけをする」と
            // ぶつかる）。レビュー run は `review.rs` が `RunContext::default()` を使うので既定の false。
            comments_enabled: writes_comments,
            // Phase 38（ADR-0028 追記）: レビュー run（`review.rs` が組む）だけに入る。
            subject_genre: None,
            // ADR-0048 D3（Phase 60b）: CoS の対話 run だけに入る。
            active_projects: extras.active_projects,
            // ADR-0054 D1（Phase 67）: 継続セッション（CoS の対話・部門長のレビュー run だけ）。
            session: extras.session,
            session_diff: extras.session_diff,
            // ADR-0056 D3（Phase 79）: mount された skills（KB に実在したものだけ）。
            skills: extras.skills,
            // ADR-0072 D9（Phase E1）: 予算切れ・yield の続きなら、前の run の checkpoint と
            // これまでの run の 1 行要約。continuation でない run では `None`
            // （プロンプトは D10 の追加分を除きバイト単位で従来どおり）。
            continuation: build_continuation_context(&events),
        },
    };
    // ADR-0066 D1（Phase 110b）: ローカルの git worktree のホスト実行にだけ、共有ビルドキャッシュの
    // `CARGO_TARGET_DIR` を与える（コンテナ実行〈`container_plan.is_some()`〉と Remote は対象外）。
    let adapter = if container_plan.is_none()
        && remote.is_none()
        && let Some(build_cache_dir) = &build_cache_dir
        && let Some(repo) = worktree.as_ref().and_then(|wt| wt.repos.first())
        && repo.is_git()
    {
        let env = [task_worker::build_cache::cargo_target_dir_env(
            build_cache_dir,
            &repo.source,
        )];
        match adapter.with_env(&env) {
            Some(wrapped) => wrapped,
            None => {
                tracing::debug!(task_id = %task_id, adapter = %adapter.id(), "adapter does not support with_env; CARGO_TARGET_DIR was not applied (ADR-0066 D1)");
                adapter
            }
        }
    } else {
        adapter
    };
    // ADR-0043 D3（Phase 56）: コンテナで走らせる run は、ここでアダプタを包んだ複製に差し替える
    // （差し込み点はアダプタ側の `container::wrap` 1 か所）。この経路を持たないアダプタ
    // （`with_container` が `None`）はホストのまま走る。
    let adapter = match &container_plan {
        Some(plan) => match adapter.with_container(Arc::clone(plan)) {
            Some(wrapped) => wrapped,
            None => {
                tracing::warn!(task_id = %task_id, adapter = %adapter.id(), "adapter does not support containers; running on the host");
                adapter
            }
        },
        None => adapter,
    };
    // ADR-0054 D1（Phase 67）: `run_worker` を通る run で継続セッションを持てるのは CoS の対話 run
    // だけ（部門長のレビュー run は `review.rs` の別経路。`run_extras` の `is_cos_conversation` と同じ
    // 判定で `extras.session` が埋まるので、ここでは `req.context.session` の有無だけを見ればよい）。
    let session_key = req.context.session.is_some().then(|| {
        (
            task_core::COS_ID.to_string(),
            task_core::SessionKind::Conversation,
            None,
        )
    });
    // ADR-0067 D3: `store` は `sink` に move されるので、後段の未申告成果物の登録用に控えておく。
    let store_for_undeclared_scan = Arc::clone(&store);
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
        session_key,
    };
    let outcome = adapter.run(req, run_id, limits, &sink).await;
    // ADR-0067 D3: run が成功したら、git worktree ではない local の作業場所（`remote`/`worktree` どちらも
    // 無い）に限り、`artifacts_dir` の外に書かれた `*.md` を「未申告の成果物」として登録する
    // （取りこぼし防止。取りに行くのは登録済みの結果ではなく、result.json が触れない場所に人向けの
    // 決定材料を残す run に対する保険）。
    if outcome.is_ok() && remote.is_none() && worktree.is_none() {
        let existing_paths: std::collections::HashSet<String> = events
            .iter()
            .filter_map(|(_, ev)| match ev {
                Event::ArtifactProduced { artifact, .. } => Some(artifact.path.clone()),
                _ => None,
            })
            .collect();
        let found = crate::undeclared_artifacts::scan_undeclared_markdown_artifacts(
            &workspace_for_undeclared_scan,
            &artifacts_dir_for_undeclared_scan,
            &existing_paths,
        );
        for artifact in found {
            let ev = Event::ArtifactProduced {
                run_id: run_id.to_string(),
                artifact,
            };
            if let Err(e) = store_for_undeclared_scan.append_event(task_id, &ev) {
                tracing::warn!(task_id = %task_id, error = %e, "failed to record an undeclared artifact (ADR-0067 D3)");
            }
        }
    }
    outcome
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

/// ADR-0059 D3: `mode` が省略された（明示的に `"worktree"` を選んでいない）Remote workspace か。
/// `Local` は関係ないので `false`。
fn remote_mode_omitted(workspace: &WorkspaceSpec) -> bool {
    matches!(workspace, WorkspaceSpec::Remote { mode: None, .. })
}

/// ADR-0059 D3: `Remote` workspace を `mode: Some(Shared)` に書き換えた複製（自動の格下げ）。
/// `Local` は関係ないので `None`。
fn downgraded_remote_workspace(workspace: &WorkspaceSpec) -> Option<WorkspaceSpec> {
    match workspace {
        WorkspaceSpec::Remote { cluster, path, .. } => Some(WorkspaceSpec::Remote {
            cluster: cluster.clone(),
            path: path.clone(),
            mode: Some(WorkspaceMode::Shared),
        }),
        WorkspaceSpec::Local { .. } => None,
    }
}

/// ADR-0061（Phase 104）: dispatch した時刻（`RunEntry`/`ReviewEntry` の `since`）から今までの
/// 壁時計時間をミリ秒で計算する。`since` が未来（時計のずれ等）なら 0 に丸める。
fn wall_ms_since(since: OffsetDateTime) -> u64 {
    (OffsetDateTime::now_utc() - since)
        .whole_milliseconds()
        .max(0) as u64
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
        Ok(WorkerMessage::Done {
            summary, evidence, ..
        }) => ReviewSubject { summary, evidence },
        _ => ReviewSubject::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ProviderSpec, StaticPolicy};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
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
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion {
                text: "c".into(),
                check,
            }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: dir.to_path_buf(),
                mode: None,
            },
            budget: Budget {
                max_turns: 1,
                max_wall_secs: 30,
                max_retries,
            },
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

    fn dispatcher(
        store: Arc<dyn TaskStore>,
        adapter: Arc<dyn WorkerAdapter>,
        max_concurrency: usize,
    ) -> Dispatcher {
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
                delivery: Default::default(),
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
                reviewer_tier_override: None,
                clusters: HashMap::new(),
                cluster_cooldown: Duration::from_secs(1),
                max_requeues: 5,
                max_reviewer_retries: 3,
                max_infra_retries: 5,
                roles: Vec::new(),
                genres: Vec::new(),
                delegation: DelegationLimits::default(),
                accounts: None,
                memory_dir: None,
                worktree_branch_prefix: task_worker::DEFAULT_BRANCH_PREFIX.to_string(),
                releases_dir: None,
                containers: ContainersRuntimeConfig::default(),
                knowledge: KnowledgeRuntimeConfig::default(),
                session_rollover_tokens: 400_000,
                // ADR-0066（Phase 110b）: 既定の試験用 dispatcher では無効にしておく（他のテストへの
                // 副作用を避ける。専用のテストが明示的に有効化する）。
                shared_build_cache: false,
                build_cache_dir: PathBuf::from("/nonexistent-build-cache"),
                workspace_prune_after_secs: 0,
                execution: ExecutionConfig::default(),
            },
        )
    }

    #[tokio::test]
    async fn draining_worker_completion_leaves_review_to_the_active_dispatcher() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: ":".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut old = dispatcher(store.clone(), adapter.clone(), 1);
        assert_eq!(old.tick().unwrap().dispatched, 1);
        old.set_accepting_new_work(false);
        for _ in 0..100 {
            old.tick().unwrap();
            if old.in_flight() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(old.in_flight(), 0);
        assert_eq!(
            store.get(task.id).unwrap().unwrap().status,
            Status::Reviewing
        );
        assert!(old.reviewing.is_empty());
        let mut active = dispatcher(store.clone(), adapter, 1);
        assert!(run_until_idle(&mut active, 100).await.idle);
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
    }

    #[tokio::test]
    async fn overlapping_dispatchers_share_review_ownership_until_verdict_is_saved() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: ":".into(),
                expect_exit: 0,
            },
            0,
        );
        task.status = Status::Reviewing;
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut old = dispatcher(store.clone(), adapter.clone(), 1);
        let mut active = dispatcher(store.clone(), adapter, 1);
        assert!(
            old.spawn_review(task.id, "subject".into(), &ReviewSubject::default())
                .unwrap()
        );
        old.set_accepting_new_work(false);
        assert!(
            !active
                .spawn_review(task.id, "subject".into(), &ReviewSubject::default())
                .unwrap()
        );
        // Even after the async check finishes, the first owner must persist its verdict.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !active
                .spawn_review(task.id, "subject".into(), &ReviewSubject::default())
                .unwrap()
        );
        assert!(run_until_idle(&mut old, 100).await.idle);
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
        active.tick().unwrap();
        assert!(active.reviewing.is_empty());
        assert_eq!(
            store
                .events_for(task.id)
                .unwrap()
                .iter()
                .filter(|(_, e)| matches!(e, Event::ReviewVerdict { .. }))
                .count(),
            1
        );
        // A later explicit review can reuse the lock; no stale lock file blocks recovery.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(
                dir.path()
                    .join("runs")
                    .join(format!(".review-{}.lock", task.id)),
            )
            .unwrap();
        file.try_lock().unwrap();
    }

    pub(super) async fn run_until_idle(d: &mut Dispatcher, max_ticks: usize) -> TickReport {
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
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "test -f touched".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![Evidence {
                    criterion: 0,
                    command: Some("x".into()),
                    exit: Some(0),
                    stdout_tail: None,
                }],
                usage: None,
            },
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

    /// Phase 44（実機 2026-09-18）: 委譲した子タスクが失敗し `retry_then_ask`（max_retries 到達）で親が
    /// `blocked` に落ちる質問は、ワーカーの `Question` ではなくディスパッチャ自身が立てるものだが、
    /// Phase 26 と同じく `approvals` にも 1 件残る（そうしないと認可画面に出ず、`approval_pending` の
    /// Discord 通知も飛ばない）。答えれば（`Trigger::Answer`）親は `ready` に戻る。
    #[tokio::test]
    async fn a_dispatcher_raised_child_failure_question_also_becomes_an_approval() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_org_for_reports(&store);

        // 子: 委譲され、走って失敗する（max_retries = 0 なので 1 回で failed）。
        let mut parent = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        parent.status = Status::Reviewing;
        parent.assignee = Some("coding-poc".into());
        store.insert(&parent).unwrap();
        let mut child = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        child.parent_id = Some(parent.id);
        store
            .delegate_children(parent.id, "run-1", vec![child.clone()])
            .unwrap();
        store
            .apply_transition(child.id, Trigger::Dispatch, None)
            .unwrap();
        store
            .apply_transition(child.id, Trigger::WorkerError { retryable: false }, None)
            .unwrap();
        assert_eq!(store.get(child.id).unwrap().unwrap().status, Status::Failed);

        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let handled = d
            .escalate_failed_children(&parent, "run-1", &mut Vec::new())
            .unwrap();
        assert!(handled);
        let after = store.get(parent.id).unwrap().unwrap();
        assert_eq!(
            after.status,
            Status::Blocked,
            "max_retries = 0 なのでやり直せず、人に聞く"
        );

        // ADR-0033 D5（Phase 26）と同じく `approvals` に 1 件残る。
        let approvals = store.approval_list(Some(true), None, None).unwrap();
        assert_eq!(approvals.len(), 1, "{approvals:?}");
        assert_eq!(approvals[0].node_id, "coding-poc");
        assert_eq!(approvals[0].task_id, Some(parent.id));
        assert!(
            approvals[0].question.contains("委譲した子タスクが失敗し"),
            "{}",
            approvals[0].question
        );

        // 答えれば ready に戻る（既存の answers[] の経路。Phase 29 の一本化はここでは検証しない）。
        store
            .apply_transition(parent.id, Trigger::Answer, None)
            .unwrap();
        assert_eq!(store.get(parent.id).unwrap().unwrap().status, Status::Ready);
    }

    /// Phase 45（実機バグ、2026-09-19）: `newly_failed_delegated_children` が「一度扱った失敗は数え直さない」
    /// （ADR-0021 D3）を判定するのに `events_for`（タスクごとのローカルな `seq`）で親と子を比較していたため、
    /// 子の方が親よりイベント数が多い（＝ `seq` が大きい）場合、子の失敗が毎回「新規」と誤判定され、親が
    /// resume するたびに同じ質問（`QuestionRaised` と `child_failed` への遷移）が繰り返された
    /// （実機の親 `01M2VG4YNG4DD7Z5BYPSB8W8AW` が 20 分で 5 回同じ質問をした事故）。
    /// `events_for_with_global_ids`（`events` テーブルのグローバル `id`）で比較すれば、2 回目以降は
    /// 「既に扱った失敗」と正しく判定され、親はやり直しの review pass だけで `done` になる。
    #[tokio::test]
    async fn child_failure_question_is_not_repeated_when_the_child_has_more_events_than_the_parent()
    {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_org_for_reports(&store);

        let mut parent = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        parent.assignee = Some("coding-poc".into());
        store.insert(&parent).unwrap();

        let mut child = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        child.parent_id = Some(parent.id);
        store
            .delegate_children(parent.id, "run-1", vec![child.clone()])
            .unwrap();
        store
            .apply_transition(child.id, Trigger::Dispatch, None)
            .unwrap();
        // 子に、親が今後 2 回の run で積む以上のイベントを積んでから失敗させる（実機の形の再現）。
        // 子の `seq`（タスクごとのローカルな連番）が親のどの `seq` よりも大きくなるようにする。
        for i in 0..200u32 {
            store
                .append_event(
                    child.id,
                    &Event::worker_progress("child-run", format!("padding {i}")),
                )
                .unwrap();
        }
        store
            .apply_transition(child.id, Trigger::WorkerError { retryable: false }, None)
            .unwrap();
        assert_eq!(store.get(child.id).unwrap().unwrap().status, Status::Failed);

        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);

        // 1 回目: 親が走って review pass するが、委譲した子が失敗しているので `max_retries = 0` によりやり直せず、
        // ディスパッチャが質問を立てて blocked になる。
        let report1 = run_until_idle(&mut d, 200).await;
        assert!(report1.idle);
        let after_first = store.get(parent.id).unwrap().unwrap();
        assert_eq!(
            after_first.status,
            Status::Blocked,
            "max_retries = 0 なのでやり直せず、人に聞く"
        );

        // 人間が答えると ready に戻る。
        store
            .apply_transition(parent.id, Trigger::Answer, None)
            .unwrap();
        assert_eq!(store.get(parent.id).unwrap().unwrap().status, Status::Ready);

        // 2 回目: 親がもう一度走って review pass する。子は同じ失敗のままだが、既に扱った失敗なので
        // 再度質問を出してはいけない（旧実装のバグ: per-task seq を比較すると、子の方が seq が大きいので
        // 「新規」と誤判定して blocked を繰り返した）。
        let report2 = run_until_idle(&mut d, 200).await;
        assert!(report2.idle);
        let after_second = store.get(parent.id).unwrap().unwrap();
        let parent_events = store.events_for(parent.id).unwrap();
        assert_eq!(
            after_second.status,
            Status::Done,
            "既に扱った子の失敗を数え直してはいけない: {parent_events:?}"
        );

        let questions = parent_events
            .iter()
            .filter(|(_, e)| matches!(e, Event::QuestionRaised { .. }))
            .count();
        assert_eq!(questions, 1, "{parent_events:?}");
        let child_failed_transitions = parent_events
            .iter()
            .filter(|(_, e)| matches!(e, Event::Transitioned { reason, .. } if reason == Trigger::ChildFailed.name()))
            .count();
        assert_eq!(child_failed_transitions, 1, "{parent_events:?}");

        // approvals も 1 件のまま（Phase 44）。
        let approvals = store.approval_list(Some(true), None, None).unwrap();
        assert_eq!(approvals.len(), 1, "{approvals:?}");
    }

    #[tokio::test]
    async fn question_blocks_task_and_review_fail_retries_until_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let q = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&q).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Question {
                text: "which?".into(),
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let report = run_until_idle(&mut d, 100).await;
        assert!(report.idle);
        assert_eq!(store.get(q.id).unwrap().unwrap().status, Status::Blocked);

        // レビュー失敗（存在しないファイル）は max_retries=1 で 2 回実行して failed。
        let dir2 = tempfile::tempdir().unwrap();
        let store2: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let f = new_task(
            dir2.path(),
            Check::Command {
                cmd: "test -f never".into(),
                expect_exit: 0,
            },
            1,
        );
        store2.insert(&f).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "claimed".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d2 = dispatcher(store2.clone(), adapter, 1);
        let report = run_until_idle(&mut d2, 200).await;
        assert!(report.idle);
        let t = store2.get(f.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Failed);
        assert_eq!(t.attempts, 2);
        let events = store2.events_for(f.id).unwrap();
        let starts = events
            .iter()
            .filter(|(_, e)| matches!(e, Event::WorkerStarted { .. }))
            .count();
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
            store
                .insert(&new_task(
                    dir.path(),
                    Check::Command {
                        cmd: "true".into(),
                        expect_exit: 0,
                    },
                    0,
                ))
                .unwrap();
        }
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
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

    /// ADR-0036: 自分の `artifacts_dir` に結果ファイルと成果物を書き、少し待ってから読み直して
    /// 「兄弟に上書きされていない」ことを確かめるアダプタ（実機の事故の再現条件を作る）。
    struct SiblingAdapter {
        delay: Duration,
    }

    #[async_trait]
    impl WorkerAdapter for SiblingAdapter {
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
            let id = req.task.id.to_string();
            std::fs::create_dir_all(&req.artifacts_dir).unwrap();
            std::fs::write(
                req.artifacts_dir.join("result.json"),
                format!(r#"{{"summary":"{id}","evidence":[]}}"#),
            )
            .unwrap();
            std::fs::write(req.artifacts_dir.join("report.md"), &id).unwrap();
            // 兄弟の run と重なる窓。
            tokio::time::sleep(self.delay).await;
            let back = std::fs::read_to_string(req.artifacts_dir.join("result.json")).unwrap();
            assert!(back.contains(&id), "兄弟に上書きされた: {back}");
            let rel = format!("{}/report.md", req.artifacts_rel());
            let artifact =
                task_worker::artifact::resolve(&req.workspace, "report.md", &rel, None).unwrap();
            sink.artifact(&artifact);
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: id,
                    evidence: vec![],
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    /// ADR-0036 D1: 単独タスク（親なし）は従来どおり `<workspace>/artifacts`。挙動もパスも変わらない。
    #[tokio::test]
    async fn a_standalone_task_keeps_the_plain_artifacts_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(
            dir.path(),
            Check::ArtifactExists {
                name: "report.md".into(),
            },
            0,
        );
        store.insert(&task).unwrap();
        let adapter = Arc::new(SiblingAdapter {
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 2);
        assert!(run_until_idle(&mut d, 200).await.idle);

        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
        assert!(
            !dir.path().join(".taskd").exists(),
            "単独タスクは `.taskd/artifacts/` を使わない"
        );
        let result = std::fs::read_to_string(dir.path().join("artifacts/result.json")).unwrap();
        assert!(result.contains(&task.id.to_string()), "{result}");
        let produced: Vec<String> = store
            .events_for(task.id)
            .unwrap()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::ArtifactProduced { artifact, .. } => Some(artifact.path),
                _ => None,
            })
            .collect();
        assert_eq!(produced, vec!["artifacts/report.md".to_string()]);
    }

    /// 実機の事故（2026-09-18、ADR-0036 §1）: 計画 run が作った兄弟 2 件が親の workspace を共有し、
    /// 両方が `<workspace>/artifacts/` に書いたので `sources.json` / `result.json` が混ざった。
    /// 同時に走らせても、結果ファイルと成果物がタスクごとに分かれていること。
    #[tokio::test]
    async fn siblings_sharing_one_workspace_do_not_mix_their_result_files_or_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut parent = new_task(dir.path(), Check::Human, 0);
        parent.kind = TaskKind::Plan;
        parent.status = Status::Done;
        store.insert(&parent).unwrap();
        let children: Vec<Task> = (0..2)
            .map(|_| {
                // plan / delegate の子は親の workspace をそのまま継ぐ（`plan::materialize`）。
                let mut c = new_task(
                    dir.path(),
                    Check::ArtifactExists {
                        name: "report.md".into(),
                    },
                    0,
                );
                c.parent_id = Some(parent.id);
                store.insert(&c).unwrap();
                c
            })
            .collect();

        let adapter = Arc::new(SiblingAdapter {
            delay: Duration::from_millis(150),
        });
        let mut d = dispatcher(store.clone(), adapter, 2);
        // 2 件が同じ tick で走り出す（並列度 2）。
        assert_eq!(d.tick().unwrap().dispatched, 2);
        let report = run_until_idle(&mut d, 400).await;
        assert!(report.idle);

        assert!(
            !dir.path().join("artifacts").exists(),
            "共有の `artifacts/` は作られない"
        );
        for c in &children {
            let t = store.get(c.id).unwrap().unwrap();
            assert_eq!(
                t.status,
                Status::Done,
                "{:?}",
                store.events_for(c.id).unwrap()
            );
            let own = dir.path().join(".taskd/artifacts").join(c.id.to_string());
            let result = std::fs::read_to_string(own.join("result.json")).unwrap();
            assert!(result.contains(&c.id.to_string()), "{result}");
            assert_eq!(
                std::fs::read_to_string(own.join("report.md")).unwrap(),
                c.id.to_string()
            );
            // 申告された成果物のパスは workspace 相対のタスクごとの形（GUI がそのまま読める）。
            let produced: Vec<String> = store
                .events_for(c.id)
                .unwrap()
                .into_iter()
                .filter_map(|(_, e)| match e {
                    Event::ArtifactProduced { artifact, .. } => Some(artifact.path),
                    _ => None,
                })
                .collect();
            assert_eq!(
                produced,
                vec![format!(".taskd/artifacts/{}/report.md", c.id)]
            );
        }
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
            std::fs::create_dir_all(&req.artifacts_dir).unwrap();
            match req.task.kind {
                TaskKind::Plan => {
                    // 2 回目以降（prior_review あり）は正しい plan を書き、1 回目は plan_json をそのまま書く。
                    let text = if req.context.prior_review.is_empty() {
                        self.plan_json.clone()
                    } else {
                        VALID_PLAN.to_string()
                    };
                    std::fs::write(req.artifacts_dir.join("plan.json"), text).unwrap();
                }
                TaskKind::Review => {
                    assert!(req.context.review.is_some());
                    std::fs::write(req.artifacts_dir.join("review.json"), &self.review_json)
                        .unwrap();
                }
                _ => {
                    std::fs::write(req.workspace.join("touched"), "1").unwrap();
                }
            }
            sink.progress("working");
            tokio::time::sleep(self.delay).await;
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: format!("{:?}", req.task.kind),
                    evidence: vec![],
                    usage: None,
                },
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
        let mut t = new_task(
            dir,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            max_retries,
        );
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
        assert_eq!(
            children.len(),
            3,
            "auto_accept=false leaves children in draft"
        );
        for c in &children {
            assert_eq!(c.parent_id, Some(plan.id));
            assert_eq!(c.workspace, plan.workspace);
        }
        let verdicts: Vec<(usize, bool, String)> = store
            .events_for(plan.id)
            .unwrap()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::ReviewVerdict {
                    criterion_idx,
                    pass,
                    reason,
                    ..
                } => Some((criterion_idx, pass, reason)),
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
            assert_eq!(
                t.status,
                Status::Done,
                "{}: {:?}",
                c.title,
                store.events_for(c.id).unwrap()
            );
        }
        let c = children.iter().find(|c| c.title == "c").unwrap();
        assert_eq!(c.worker_hint.tier, Tier::Cheap);
        let events = store.events_for(c.id).unwrap();
        let run_id = last_run_id(&events).unwrap();
        let reviewer_progress = events
            .iter()
            .filter(|(_, e)| matches!(e, Event::WorkerProgress { run_id: r, msg, .. } if r == &run_id && msg.starts_with("reviewer run ")))
            .count();
        assert!(reviewer_progress >= 2, "{events:?}");
        assert!(events.iter().any(|(_, e)| matches!(e, Event::ReviewVerdict { pass: true, reason, .. } if reason.contains("reviewer(") && reason.contains("fine"))));
        // WorkerStarted はワーカー run の 1 回と、ADR-0014 D1 で記録する Reviewer run の 1 回。
        assert_eq!(
            events
                .iter()
                .filter(|(_, e)| matches!(e, Event::WorkerStarted { role: None, .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|(_, e)| matches!(
                    e,
                    Event::WorkerStarted {
                        role: Some(RunRole::Reviewer),
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn invalid_plan_is_retried_with_prior_review_then_children_auto_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let plan = plan_task(dir.path(), 1);
        store.insert(&plan).unwrap();
        // ADR-0067 D2: `human` チェックには artifacts か知識ベースの参照が要る（この plan.json は
        // `depends_on` の範囲外エラーだけを狙っているので、`human` 側の違反を出さないよう
        // `artifact_exists` を添えておく）。
        let adapter = Arc::new(FileAdapter {
            plan_json: r#"{"tasks":[{"title":"a","objective":"o","acceptance":[{"text":"c","check":{"type":"human"}},{"text":"d","check":{"type":"artifact_exists","name":"result.md"}}],"depends_on":[9]}]}"#.into(),
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
        assert!(
            !verdicts[0].0 && verdicts[0].1.contains("out of range"),
            "{:?}",
            verdicts[0]
        );
        assert!(verdicts[1].0);
        // auto_accept=true: 子は ready で挿入され、その後 done まで進む（a, b は Command、c は Reviewer で review.json 無し→ fail → failed）。
        let children: Vec<Task> = store
            .list(None)
            .unwrap()
            .into_iter()
            .filter(|t| t.parent_id == Some(plan.id))
            .collect();
        assert_eq!(children.len(), 3);
        for c in &children {
            let ev = store.events_for(c.id).unwrap();
            assert!(matches!(&ev[0].1, Event::Created { task } if task.status == Status::Draft));
            assert!(
                matches!(&ev[1].1, Event::Transitioned { from: Status::Draft, to: Status::Ready, reason } if reason == "accept")
            );
        }
        let by_title = |t: &str| {
            children
                .iter()
                .find(|c| c.title == t)
                .map(|c| store.get(c.id).unwrap().unwrap())
                .unwrap()
        };
        assert_eq!(by_title("a").status, Status::Done);
        assert_eq!(by_title("b").status, Status::Done);
        let c = by_title("c");
        assert_eq!(
            c.status,
            Status::Failed,
            "{:?}",
            store.events_for(c.id).unwrap()
        );
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
        let other = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
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

    fn reviewer_routing_record(store: &dyn TaskStore, task_id: TaskId) -> task_core::RoutingRecord {
        store
            .events_for(task_id)
            .unwrap()
            .into_iter()
            .find_map(|(_, e)| match e {
                Event::RoutingDecided { record, .. } => Some(*record),
                _ => None,
            })
            .expect("routing_decided for the reviewer run")
    }

    /// ADR-0069 Phase 118 D4（既定）: `[reviewer] tier` も部署の `review.tier` も無ければ、reviewer の
    /// lane は worker run の lane に一致させ、組織の天井（`budget.max_lane`）で丸める。
    #[tokio::test]
    async fn reviewer_lane_defaults_to_the_worker_lane_capped_by_the_org_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut eng = org_node_of("eng", Some("secretary"), OrgKind::Department, None);
        eng.profile.budget.max_lane = Some(Tier::Standard);
        for n in [
            org_node_of("secretary", None, OrgKind::Secretary, Some("secretary")),
            eng,
        ] {
            store.org_upsert(&n).unwrap();
        }
        let mut r = new_task(dir.path(), Check::Reviewer, 0);
        r.assignee = Some("eng".into());
        r.worker_hint.tier = Tier::Frontier;
        store.insert(&r).unwrap();
        let adapter = Arc::new(FileAdapter {
            plan_json: String::new(),
            review_json: r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"ok"}]}"#.into(),
            delay: Duration::from_millis(5),
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let report = run_until_idle(&mut d, 300).await;
        assert!(report.idle);
        assert_eq!(store.get(r.id).unwrap().unwrap().status, Status::Done);
        let record = reviewer_routing_record(store.as_ref(), r.id);
        assert_eq!(record.harness.as_deref(), Some("reviewer"));
        assert_eq!(record.decision.lane, Tier::Standard, "{record:?}");
        assert_eq!(record.decision.proposed, Tier::Frontier);
        assert_eq!(record.decision.rule_id, "reviewer/matches-worker-lane");
        assert!(record.decision.clamped_by.is_some(), "{record:?}");
        assert_eq!(record.resolution.lane, Some(Tier::Standard));
    }

    /// ADR-0069 Phase 118 D4: `[reviewer] tier` の明示は既定（worker lane 一致）に勝つ。
    #[tokio::test]
    async fn explicit_reviewer_tier_config_wins_over_the_worker_lane_default() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        for n in [
            org_node_of("secretary", None, OrgKind::Secretary, Some("secretary")),
            org_node_of("eng", Some("secretary"), OrgKind::Department, None),
        ] {
            store.org_upsert(&n).unwrap();
        }
        let mut r = new_task(dir.path(), Check::Reviewer, 0);
        r.assignee = Some("eng".into());
        r.worker_hint.tier = Tier::Frontier;
        store.insert(&r).unwrap();
        let adapter = Arc::new(FileAdapter {
            plan_json: String::new(),
            review_json: r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"ok"}]}"#.into(),
            delay: Duration::from_millis(5),
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.reviewer_tier_override = Some(Tier::Cheap);
        let report = run_until_idle(&mut d, 300).await;
        assert!(report.idle);
        let record = reviewer_routing_record(store.as_ref(), r.id);
        assert_eq!(record.decision.lane, Tier::Cheap, "{record:?}");
        assert_eq!(record.decision.rule_id, "reviewer/explicit-config");
        assert_eq!(record.decision.clamped_by, None);
    }

    /// ADR-0069 Phase 118 D4: 部署の `[profile] review.tier`（ADR-0069 D2）は `[reviewer] tier` の
    /// 明示より強い（最も具体的な指定）。
    #[tokio::test]
    async fn department_review_tier_wins_over_the_explicit_reviewer_config() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut eng = org_node_of("eng", Some("secretary"), OrgKind::Department, None);
        eng.profile.review.tier = Some(Tier::Frontier);
        for n in [
            org_node_of("secretary", None, OrgKind::Secretary, Some("secretary")),
            eng,
        ] {
            store.org_upsert(&n).unwrap();
        }
        let mut r = new_task(dir.path(), Check::Reviewer, 0);
        r.assignee = Some("eng".into());
        r.worker_hint.tier = Tier::Standard;
        store.insert(&r).unwrap();
        let adapter = Arc::new(FileAdapter {
            plan_json: String::new(),
            review_json: r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"ok"}]}"#.into(),
            delay: Duration::from_millis(5),
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.reviewer_tier_override = Some(Tier::Cheap);
        let report = run_until_idle(&mut d, 300).await;
        assert!(report.idle);
        let record = reviewer_routing_record(store.as_ref(), r.id);
        assert_eq!(record.decision.lane, Tier::Frontier, "{record:?}");
        assert_eq!(record.decision.rule_id, "reviewer/department-review-tier");
    }

    /// ADR-0054 D1（Phase 67）: 部署の根ノード（`department_of` が返す id）は、Reviewer run のたびに
    /// **同じ継続セッション**（`kind = lead`）を使う。1 本目の run で新規セッション（`turns = 1`）ができ、
    /// 2 本目の run では **同じ `session_id` のまま** `turns = 2` に進む（`--resume` 相当の継続）。
    /// アダプタが継続に対応する id（`claude-code`）のときだけの経路（`crate::sessions::adapter_supports_sessions`）。
    #[tokio::test]
    async fn department_reviewer_runs_share_and_continue_one_lead_session() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let policy = StaticPolicy::new(
            vec![ProviderSpec {
                id: "p1".into(),
                // ADR-0054 D1: セッション継続に対応するアダプタ id（`crate::sessions::SUPPORTED_ADAPTERS`）。
                adapter: "claude-code".into(),
                tiers: vec![Tier::Frontier, Tier::Standard, Tier::Cheap],
                concurrency: 2,
                model: "m".into(),
            }],
            Duration::from_secs(1),
        );
        let file_adapter = Arc::new(FileAdapter {
            plan_json: String::new(),
            review_json: r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"ok"}]}"#.into(),
            delay: Duration::from_millis(5),
        });
        let mut adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>> = HashMap::new();
        adapters.insert("p1".into(), file_adapter);
        let mut d = Dispatcher::new(
            store.clone(),
            Box::new(policy),
            HashMap::from([("p1".to_string(), "m".to_string())]),
            adapters,
            std::collections::HashSet::new(),
            DispatchConfig {
                delivery: Default::default(),
                max_concurrency: 2,
                lease_grace: Duration::from_secs(60),
                idle_timeout: Duration::from_secs(5),
                kill_grace: Duration::from_millis(100),
                review_timeout: Duration::from_secs(5),
                workspace_root: PathBuf::from("/nonexistent"),
                plan_auto_accept: false,
                retry_backoff_base: Duration::ZERO,
                retry_backoff_max: Duration::ZERO,
                reviewer_hint: crate::review::reviewer_hint(),
                reviewer_tier_override: None,
                clusters: HashMap::new(),
                cluster_cooldown: Duration::from_secs(1),
                max_requeues: 5,
                max_reviewer_retries: 3,
                max_infra_retries: 5,
                roles: Vec::new(),
                genres: Vec::new(),
                delegation: DelegationLimits::default(),
                accounts: None,
                memory_dir: None,
                worktree_branch_prefix: task_worker::DEFAULT_BRANCH_PREFIX.to_string(),
                releases_dir: None,
                containers: ContainersRuntimeConfig::default(),
                knowledge: KnowledgeRuntimeConfig::default(),
                session_rollover_tokens: 400_000,
                shared_build_cache: false,
                build_cache_dir: PathBuf::from("/nonexistent-build-cache"),
                workspace_prune_after_secs: 0,
                execution: ExecutionConfig::default(),
            },
        );

        // 1 本目: `coding-poc`（セクション）の Reviewer run。部署は `department_of` で `coding` に解決する。
        let mut first = new_task(dir.path(), Check::Reviewer, 0);
        first.assignee = Some("coding-poc".into());
        store.insert(&first).unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(first.id).unwrap().unwrap().status, Status::Done);

        let after_first = store
            .node_session_active("coding", SessionKind::Lead, None)
            .unwrap()
            .expect("a lead session exists for the department after the first review");
        assert_eq!(after_first.adapter, "claude-code");
        assert!(
            !after_first.session_id.is_empty(),
            "claude-code sessions get a celeris-assigned id up front"
        );
        assert_eq!(after_first.turns, 1);

        // 2 本目: 別のタスクだが同じ部署。セッションは**続く**（同じ id、turns が進む）。
        let mut second = new_task(dir.path(), Check::Reviewer, 0);
        second.assignee = Some("coding-poc".into());
        store.insert(&second).unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(second.id).unwrap().unwrap().status, Status::Done);

        let after_second = store
            .node_session_active("coding", SessionKind::Lead, None)
            .unwrap()
            .expect("the lead session is still active");
        assert_eq!(
            after_second.session_id, after_first.session_id,
            "the second review resumes the same lead session"
        );
        assert_eq!(after_second.turns, 2);
        assert!(after_second.retired_at.is_none());

        // 部署の無い（担当なし）Reviewer run はセッションを持たない。
        let plain = new_task(dir.path(), Check::Reviewer, 0);
        store.insert(&plain).unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(plain.id).unwrap().unwrap().status, Status::Done);
        // `research` 部署は今回のタスクで一度も使っていないので、セッションは無い。
        assert_eq!(
            store
                .node_session_active("research", SessionKind::Lead, None)
                .unwrap(),
            None
        );
    }

    /// ADR-0054 D1 / Phase 67b 追記: `resume` を頼まれた Reviewer run では `session_resume_failed` を
    /// 報告する（実機の「resume が拒否された」を模す fake アダプタ）。それ以外の run は通常どおり
    /// `review.json` を書いて完了する。
    struct ResumeRejectingReviewAdapter {
        review_json: String,
    }

    #[async_trait]
    impl WorkerAdapter for ResumeRejectingReviewAdapter {
        fn id(&self) -> &str {
            "claude-code"
        }
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            std::fs::create_dir_all(&req.artifacts_dir).unwrap();
            let resuming = req.context.session.as_ref().is_some_and(|s| s.resume);
            if resuming {
                // 実機の crash 分類（`provider::looks_like_resume_rejection`）と同じ経路: resume を
                // 拒否されたら run の成否に関わらず報告する（ADR-0054 D1「失敗も同じ経路で作り直す」）。
                sink.session_resume_failed("simulated: no conversation found for session");
            }
            std::fs::write(req.artifacts_dir.join("review.json"), &self.review_json).unwrap();
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: "ok".into(),
                    evidence: vec![],
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    /// ADR-0054 D1 / Phase 67b 追記: resume が拒否されたら（`EventSink::session_resume_failed`）、
    /// その場で Lead セッションを retire し、次の Reviewer run は新しいセッション（別の id、
    /// `resume=false`）を作る（「失敗も同じ経路で作り直す」）。Phase 67 の実装は `ReviewerSink` に
    /// この配線が無く、部門長のレビュー run では resume 拒否が一切 retire されなかった（この場合の
    /// 本番の症状は「同じ壊れた session_id で `--resume` を延々と再試行する」）。
    #[tokio::test]
    async fn a_rejected_lead_session_resume_retires_it_and_the_next_review_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let policy = StaticPolicy::new(
            vec![ProviderSpec {
                id: "p1".into(),
                adapter: "claude-code".into(),
                tiers: vec![Tier::Frontier, Tier::Standard, Tier::Cheap],
                concurrency: 2,
                model: "m".into(),
            }],
            Duration::from_secs(1),
        );
        let adapter = Arc::new(ResumeRejectingReviewAdapter {
            review_json: r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"ok"}]}"#.into(),
        });
        let mut adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>> = HashMap::new();
        adapters.insert("p1".into(), adapter);
        let mut d = Dispatcher::new(
            store.clone(),
            Box::new(policy),
            HashMap::from([("p1".to_string(), "m".to_string())]),
            adapters,
            std::collections::HashSet::new(),
            DispatchConfig {
                delivery: Default::default(),
                max_concurrency: 2,
                lease_grace: Duration::from_secs(60),
                idle_timeout: Duration::from_secs(5),
                kill_grace: Duration::from_millis(100),
                review_timeout: Duration::from_secs(5),
                workspace_root: PathBuf::from("/nonexistent"),
                plan_auto_accept: false,
                retry_backoff_base: Duration::ZERO,
                retry_backoff_max: Duration::ZERO,
                reviewer_hint: crate::review::reviewer_hint(),
                reviewer_tier_override: None,
                clusters: HashMap::new(),
                cluster_cooldown: Duration::from_secs(1),
                max_requeues: 5,
                max_reviewer_retries: 3,
                max_infra_retries: 5,
                roles: Vec::new(),
                genres: Vec::new(),
                delegation: DelegationLimits::default(),
                accounts: None,
                memory_dir: None,
                worktree_branch_prefix: task_worker::DEFAULT_BRANCH_PREFIX.to_string(),
                releases_dir: None,
                containers: ContainersRuntimeConfig::default(),
                knowledge: KnowledgeRuntimeConfig::default(),
                session_rollover_tokens: 400_000,
                shared_build_cache: false,
                build_cache_dir: PathBuf::from("/nonexistent-build-cache"),
                workspace_prune_after_secs: 0,
                execution: ExecutionConfig::default(),
            },
        );

        // 1 本目: `coding-poc` の Reviewer run。新規セッション（resume していないので拒否は起きない）。
        let mut first = new_task(dir.path(), Check::Reviewer, 0);
        first.assignee = Some("coding-poc".into());
        store.insert(&first).unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(first.id).unwrap().unwrap().status, Status::Done);
        let after_first = store
            .node_session_active("coding", SessionKind::Lead, None)
            .unwrap()
            .expect("a lead session exists after the first review");
        let first_id = after_first.session_id.clone();

        // 2 本目: 同じ部署 → resume を頼まれる → アダプタが resume 拒否を報告する。
        let mut second = new_task(dir.path(), Check::Reviewer, 0);
        second.assignee = Some("coding-poc".into());
        store.insert(&second).unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(second.id).unwrap().unwrap().status, Status::Done);

        // resume 拒否はその場で retire する（次の `resolve_node_session` を待たない。ADR-0054 D1）。
        assert_eq!(
            store
                .node_session_active("coding", SessionKind::Lead, None)
                .unwrap(),
            None,
            "a rejected resume retires the session immediately"
        );

        // 3 本目: 次の run は新しいセッション（別の id、resume=false）を作る。
        let mut third = new_task(dir.path(), Check::Reviewer, 0);
        third.assignee = Some("coding-poc".into());
        store.insert(&third).unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(third.id).unwrap().unwrap().status, Status::Done);

        let after_third = store
            .node_session_active("coding", SessionKind::Lead, None)
            .unwrap()
            .expect("the next run starts a fresh lead session");
        assert_ne!(
            after_third.session_id, first_id,
            "a fresh session gets a new id"
        );
        assert_eq!(
            after_third.turns, 1,
            "a fresh session starts at 0 turns, then this run touches it once"
        );
    }

    /// Phase 113 D1/D2/D4(b)（ADR-0054 追記）: 1 回目の（resume を頼まれた）reviewer run は、実機の
    /// `claude_code.rs`（Phase 113 で直した後）と同じ挙動を模す — resume 拒否を `session_resume_failed`
    /// で報告**しつつ**、`Terminal::Error{retryable: true}` で終わる（`fail_all` されない、D2）。
    /// 2 回目（session が retire されて新規セッションになった run）は、正常に `review.json` を書いて
    /// `Terminal::Done` で終わる。
    struct ResumeRejectingThenSucceedingReviewAdapter {
        review_json: String,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl WorkerAdapter for ResumeRejectingThenSucceedingReviewAdapter {
        fn id(&self) -> &str {
            "claude-code"
        }
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            std::fs::create_dir_all(&req.artifacts_dir).unwrap();
            let resuming = req.context.session.as_ref().is_some_and(|s| s.resume);
            self.calls.fetch_add(1, Ordering::SeqCst);
            if resuming {
                // ADR-0054 Phase 113 D1: `claude_code.rs::run_claude_code` が `result` メッセージを
                // 観測できた run（`error_during_execution`/`is_error`）でも、resume 拒否の文言が
                // stderr にあれば `session_resume_failed` を報告するようになった。
                sink.session_resume_failed(
                    "No conversation found with session ID: 01a0d017-e32a-4cad-b10c-0cb63869ae13",
                );
                return Ok(RunOutcome {
                    terminal: Terminal::Error {
                        message: "claude result: error_during_execution".into(),
                        retryable: true,
                    },
                    exit_code: Some(1),
                });
            }
            std::fs::write(req.artifacts_dir.join("review.json"), &self.review_json).unwrap();
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: "ok".into(),
                    evidence: vec![],
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    /// Phase 113 D1+D2+D4(b)（ADR-0054 追記。本番のタスク 01M35X86XTK84F97QW0CN5PGMR /
    /// reviewer run 01M388BENASH3JEBWFS03KEQYT の再現）: 1 回目の reviewer run が resume 拒否で
    /// `error_during_execution`/`is_error` 終わっても、`fail_all` されず reviewing のまま延期され
    /// （D2）、resume 拒否は retire される（D1）。2 回目は新規セッション（`resume=false`）で走り、
    /// 判定が出て `Done` になる。Phase 67〜112 時点はこの経路（`result` を観測できた resume 拒否）を
    /// 一切 self-heal できず、この形の失敗はそのまま `failed` になっていた。
    #[tokio::test]
    async fn a_resume_rejection_that_still_produced_a_result_self_heals_and_the_retry_produces_a_verdict()
     {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let policy = StaticPolicy::new(
            vec![ProviderSpec {
                id: "p1".into(),
                adapter: "claude-code".into(),
                tiers: vec![Tier::Frontier, Tier::Standard, Tier::Cheap],
                concurrency: 2,
                model: "m".into(),
            }],
            Duration::from_secs(1),
        );
        let adapter = Arc::new(ResumeRejectingThenSucceedingReviewAdapter {
            review_json: r#"{"verdicts":[{"criterion":0,"pass":true,"reason":"ok"}]}"#.into(),
            calls: AtomicUsize::new(0),
        });
        let adapter_handle = adapter.clone();
        let mut adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>> = HashMap::new();
        adapters.insert("p1".into(), adapter);
        let mut d = Dispatcher::new(
            store.clone(),
            Box::new(policy),
            HashMap::from([("p1".to_string(), "m".to_string())]),
            adapters,
            std::collections::HashSet::new(),
            DispatchConfig {
                delivery: Default::default(),
                max_concurrency: 2,
                lease_grace: Duration::from_secs(60),
                idle_timeout: Duration::from_secs(5),
                kill_grace: Duration::from_millis(100),
                review_timeout: Duration::from_secs(5),
                workspace_root: PathBuf::from("/nonexistent"),
                plan_auto_accept: false,
                retry_backoff_base: Duration::ZERO,
                retry_backoff_max: Duration::ZERO,
                reviewer_hint: crate::review::reviewer_hint(),
                reviewer_tier_override: None,
                clusters: HashMap::new(),
                cluster_cooldown: Duration::from_secs(1),
                max_requeues: 5,
                max_reviewer_retries: 3,
                max_infra_retries: 5,
                roles: Vec::new(),
                genres: Vec::new(),
                delegation: DelegationLimits::default(),
                accounts: None,
                memory_dir: None,
                worktree_branch_prefix: task_worker::DEFAULT_BRANCH_PREFIX.to_string(),
                releases_dir: None,
                containers: ContainersRuntimeConfig::default(),
                knowledge: KnowledgeRuntimeConfig::default(),
                session_rollover_tokens: 400_000,
                shared_build_cache: false,
                build_cache_dir: PathBuf::from("/nonexistent-build-cache"),
                workspace_prune_after_secs: 0,
                execution: ExecutionConfig::default(),
            },
        );

        // 1 本目: `coding-poc` の Reviewer run。新規セッション（resume していないので拒否は起きない）。
        let mut first = new_task(dir.path(), Check::Reviewer, 0);
        first.assignee = Some("coding-poc".into());
        store.insert(&first).unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(first.id).unwrap().unwrap().status, Status::Done);

        // 2 本目: 同じ部署 → resume を頼まれる → resume 拒否で `error_during_execution` になる
        // （D1: self-heal で session を retire）が、`fail_all` はされず（D2）、3 本目として
        // 新規セッションでやり直して判定が出る。
        let mut second = new_task(dir.path(), Check::Reviewer, 0);
        second.assignee = Some("coding-poc".into());
        store.insert(&second).unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(
            store.get(second.id).unwrap().unwrap().status,
            Status::Done,
            "D2: the infra failure did not fail_all; the retried run produced a passing verdict"
        );
        assert_eq!(
            store.get(second.id).unwrap().unwrap().attempts,
            0,
            "the deferred retry did not consume an attempt"
        );
        assert!(
            adapter_handle.calls.load(Ordering::SeqCst) >= 2,
            "the adapter ran at least twice for the second task"
        );

        let events = store.events_for(second.id).unwrap();
        assert!(
            events
                .iter()
                .any(|(_, e)| matches!(e, Event::ReviewVerdict { pass: true, .. })),
            "{events:?}"
        );
        // resume 拒否は D1 どおりその場で retire された（`node_sessions` の履歴で確認できる: 3 本目の
        // 部署は同じ `coding` だが、resume 拒否のあとは新規セッションで走った——2 本目のタスクが
        // `Done` になったこと自体が、resume 拒否の run 単体では止まらず先に進んだ証拠。ここでは加えて
        // 有効なセッションが（retire→新規作成で）存在することも確認する）。
        assert!(
            store
                .node_session_active("coding", SessionKind::Lead, None)
                .unwrap()
                .is_some(),
            "a fresh lead session exists after the self-heal"
        );
    }

    /// ADR-0054 D1（Phase 67）: `resolve_node_session` の店じまい（store の読み書き側）。純粋な判断は
    /// `crate::sessions::decide` で別途テスト済みなので、ここでは実際に `node_sessions` を作る・続ける・
    /// rollover で作り直す・アカウント変更で作り直す、の 4 つが store に正しく反映されることを見る。
    #[tokio::test]
    async fn resolve_node_session_resumes_under_the_limit_then_rolls_over_then_retires_on_account_change()
     {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut d = dispatcher(
            store.clone(),
            Arc::new(FileAdapter {
                plan_json: String::new(),
                review_json: String::new(),
                delay: Duration::ZERO,
            }),
            1,
        );
        d.config.session_rollover_tokens = 100;
        let now = OffsetDateTime::now_utc();

        // 1 本目: 現役セッションが無いので新規（要約は乗らない）。
        let (first, diff0) = d
            .resolve_node_session(
                "cos",
                SessionKind::Conversation,
                None,
                "claude-code",
                Some("acct-a"),
                now,
            )
            .unwrap();
        let first = first.unwrap();
        assert!(
            !first.resume,
            "the first run of a session is never a resume"
        );
        assert!(diff0.is_empty(), "a brand-new session has no summary yet");
        assert!(!first.session_id.is_empty());
        let first_id = first.session_id.clone();

        // 累計トークンが閾値未満なら続く（`--resume` 相当）。
        store
            .node_session_touch("cos", SessionKind::Conversation, None, 50, now)
            .unwrap();
        let (second, _) = d
            .resolve_node_session(
                "cos",
                SessionKind::Conversation,
                None,
                "claude-code",
                Some("acct-a"),
                now,
            )
            .unwrap();
        let second = second.unwrap();
        assert!(second.resume);
        assert_eq!(second.session_id, first_id);

        // 閾値を超えたら次の run から新規セッション（前のセッションは引退、前置きには要約が乗る）。
        store
            .node_session_touch("cos", SessionKind::Conversation, None, 100, now)
            .unwrap();
        let (third, summary) = d
            .resolve_node_session(
                "cos",
                SessionKind::Conversation,
                None,
                "claude-code",
                Some("acct-a"),
                now,
            )
            .unwrap();
        let third = third.unwrap();
        assert!(!third.resume, "rollover starts a fresh session");
        assert_ne!(third.session_id, first_id);
        // 要約自体は空（対話履歴が無いテストなので）でも、`needs_summary` 経路（`session_summary` 呼び出し）
        // を通ったことは rollover で `resume = false` になったことから確認できる。
        let _ = summary;
        let after_rollover = store
            .node_session_active("cos", SessionKind::Conversation, None)
            .unwrap()
            .unwrap();
        assert_eq!(after_rollover.session_id, third.session_id);
        assert_eq!(after_rollover.turns, 0, "a fresh session starts at 0 turns");

        // アカウントが変わると（プールが枯渇して別アカウントに倒れた等）、同じトークン量でも作り直す。
        let (fourth, _) = d
            .resolve_node_session(
                "cos",
                SessionKind::Conversation,
                None,
                "claude-code",
                Some("acct-b"),
                now,
            )
            .unwrap();
        let fourth = fourth.unwrap();
        assert!(!fourth.resume, "an account change starts a fresh session");
        assert_ne!(fourth.session_id, third.session_id);
        let after_account_change = store
            .node_session_active("cos", SessionKind::Conversation, None)
            .unwrap()
            .unwrap();
        assert_eq!(after_account_change.session_id, fourth.session_id);
        assert_eq!(after_account_change.account_id.as_deref(), Some("acct-b"));
    }

    /// ADR-0054 Phase 67b 追記: 本番事故の再現と自己修復。`node_sessions` に Phase 67 が残した
    /// ULID の `session_id`（`--session-id`/`--resume` を Claude Code CLI 2.1.278 に拒否される）を持つ
    /// `claude-code` の行が既にあっても、`resolve_node_session` はそれを resume させず、retire した上で
    /// UUID の新しいセッションを作る。
    #[tokio::test]
    async fn resolve_node_session_self_heals_a_non_uuid_claude_code_session_id() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let d = dispatcher(
            store.clone(),
            Arc::new(FileAdapter {
                plan_json: String::new(),
                review_json: String::new(),
                delay: Duration::ZERO,
            }),
            1,
        );
        let now = OffsetDateTime::now_utc();

        // 本番で観測された壊れた行を直接作る（Phase 67 が `ulid::Ulid::new().to_string()` を
        // `--session-id` に渡していた事故。2026-09-21 13:53 UTC 観測、`turns=1`）。
        let broken = NodeSession::new(
            "cos",
            SessionKind::Conversation,
            None,
            "claude-code",
            Some("claude_max_lab".to_string()),
            "01M323X6TJQSFEP0MKXABWVY78",
            now,
        );
        store.node_session_create(&broken).unwrap();
        store
            .node_session_touch("cos", SessionKind::Conversation, None, 10, now)
            .unwrap();

        let (fresh, _summary) = d
            .resolve_node_session(
                "cos",
                SessionKind::Conversation,
                None,
                "claude-code",
                Some("claude_max_lab"),
                now,
            )
            .unwrap();
        let fresh = fresh.unwrap();
        assert!(
            !fresh.resume,
            "an invalid stored session id must never be resumed"
        );
        assert_ne!(fresh.session_id, "01M323X6TJQSFEP0MKXABWVY78");
        assert!(
            task_worker::provider::is_valid_uuid(&fresh.session_id),
            "{}",
            fresh.session_id
        );

        // 壊れた行は retire され、新しい（0 turns の）行に置き換わっている。
        let after = store
            .node_session_active("cos", SessionKind::Conversation, None)
            .unwrap()
            .unwrap();
        assert_eq!(after.session_id, fresh.session_id);
        assert_eq!(after.turns, 0, "a fresh session starts at 0 turns");
        assert_eq!(after.account_id.as_deref(), Some("claude_max_lab"));
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
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
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
                Some(Event::ApprovalDecided {
                    by: "human".into(),
                    approved: true,
                    note: Some("looks good".into()),
                }),
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

    /// Phase 113 D3/D4(d)（ADR-0054 追記）: `Review` kind の run だけを見る。1 回目（`n=0`）は
    /// `Reviewer` criterion（idx 1）を不合格にし、2 回目以降（`rereview` 後）は合格にする
    /// （resume やインフラ都合の失敗ではなく、判定そのものが変わるケースを模す）。
    struct TogglingReviewAdapter {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl WorkerAdapter for TogglingReviewAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            if req.task.kind != TaskKind::Review {
                return Ok(done_outcome());
            }
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            std::fs::create_dir_all(&req.artifacts_dir).unwrap();
            let pass = n >= 1;
            std::fs::write(
                req.artifacts_dir.join("review.json"),
                format!(r#"{{"verdicts":[{{"criterion":1,"pass":{pass},"reason":"r"}}]}}"#),
            )
            .unwrap();
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: "ok".into(),
                    evidence: vec![],
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    /// Phase 113 D3/D4(d)（ADR-0054 追記。本番のタスク 01M35X86XTK84F97QW0CN5PGMR の障害を踏まえた
    /// 回復手段）: `Human` 条件が承認済みのまま `Reviewer` 条件だけが不合格で `failed`
    /// （直前の遷移は `review_fail`）になったタスクを `task_ops::comment::rereview` で再判定すると、
    /// 承認済みの `Approval` 子タスクを**再利用**し（人に二度承認させない）、`Reviewer` 条件だけを
    /// やり直す。2 回目は合格して `Done` になる。
    #[tokio::test]
    async fn rereview_from_failed_reuses_the_approved_human_child_and_only_reruns_the_reviewer() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(dir.path(), Check::Human, 0);
        task.acceptance.push(Criterion {
            text: "r".into(),
            check: Check::Reviewer,
        });
        store.insert(&task).unwrap();
        let adapter = Arc::new(TogglingReviewAdapter {
            calls: AtomicUsize::new(0),
        });
        let mut d = dispatcher(store.clone(), adapter.clone(), 2);

        let approval = wait_for_approval_child(&mut d, &store, task.id).await;
        store
            .apply_transition(
                approval.id,
                Trigger::Approve,
                Some(Event::ApprovalDecided {
                    by: "human".into(),
                    approved: true,
                    note: Some("looks good".into()),
                }),
            )
            .unwrap();
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(
            t.status,
            Status::Failed,
            "the reviewer criterion genuinely failed the first time"
        );
        assert_eq!(t.attempts, 1);

        let approvals_before: Vec<_> = store
            .list(None)
            .unwrap()
            .into_iter()
            .filter(|c| c.parent_id == Some(task.id) && c.kind == TaskKind::Approval)
            .collect();
        assert_eq!(approvals_before.len(), 1);
        assert_eq!(approvals_before[0].id, approval.id);
        assert_eq!(approvals_before[0].status, Status::Done);

        // `failed` からの再レビュー（最後の遷移が `review_fail` なので許される。D3）。
        let result = task_ops::comment::rereview(store.as_ref(), task.id, None).unwrap();
        assert_eq!(result.to, Status::Reviewing);
        assert_eq!(
            store.get(task.id).unwrap().unwrap().attempts,
            0,
            "attempts rolled back to what it was when the approved child was created"
        );

        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Done, "the retried reviewer run passed");

        let approvals_after: Vec<_> = store
            .list(None)
            .unwrap()
            .into_iter()
            .filter(|c| c.parent_id == Some(task.id) && c.kind == TaskKind::Approval)
            .collect();
        assert_eq!(
            approvals_after.len(),
            1,
            "no new Approval child was created; the human was not asked again"
        );
        assert_eq!(approvals_after[0].id, approval.id);
        assert_eq!(
            adapter.calls.load(Ordering::SeqCst),
            2,
            "the reviewer ran exactly twice"
        );
    }

    /// ADR-0033 D2（監査 D-3）: 承認子タスクは親の `project_id` / `milestone_id` / `assignee` を継ぐ
    /// （案件の仕事の木から子が消えないように）。
    #[tokio::test]
    async fn human_check_approval_child_inherits_the_parents_project_milestone_and_assignee() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(dir.path(), Check::Human, 1);
        task.project_id = Some(ProjectId::new());
        task.milestone_id = Some(MilestoneId::new());
        task.assignee = Some("research-survey".into());
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 2);

        let approval = wait_for_approval_child(&mut d, &store, task.id).await;
        assert_eq!(approval.project_id, task.project_id);
        assert_eq!(approval.milestone_id, task.milestone_id);
        assert_eq!(approval.assignee, task.assignee);
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
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 2);

        let approval = wait_for_approval_child(&mut d, &store, task.id).await;
        store
            .apply_transition(
                approval.id,
                Trigger::Reject,
                Some(Event::ApprovalDecided {
                    by: "human".into(),
                    approved: false,
                    note: Some("not ready".into()),
                }),
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
        let mut approval = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        approval.kind = TaskKind::Approval;
        approval.status = Status::Ready;
        store.insert(&approval).unwrap();

        let mut child = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        child.parent_id = Some(approval.id);
        child.created_at = now;
        store.insert(&child).unwrap();

        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 2);

        // 承認前: 何 tick 回しても子は dispatch されず Ready のまま。
        for _ in 0..5 {
            let report = d.tick().unwrap();
            assert_eq!(
                report.dispatched, 0,
                "child must not be dispatched while its Approval parent is pending"
            );
        }
        assert_eq!(store.get(child.id).unwrap().unwrap().status, Status::Ready);

        // reject すると子は cancelled になり、以降も dispatch されない。
        store
            .apply_transition(
                approval.id,
                Trigger::Reject,
                Some(Event::ApprovalDecided {
                    by: "human".into(),
                    approved: false,
                    note: None,
                }),
            )
            .unwrap();
        assert_eq!(
            store.get(approval.id).unwrap().unwrap().status,
            Status::Failed
        );
        assert_eq!(
            store.get(child.id).unwrap().unwrap().status,
            Status::Cancelled
        );
        for _ in 0..5 {
            let report = d.tick().unwrap();
            assert_eq!(report.dispatched, 0);
        }
        assert_eq!(
            store.get(child.id).unwrap().unwrap().status,
            Status::Cancelled
        );
    }

    fn done_outcome() -> RunOutcome {
        RunOutcome {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
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
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                // 下のテストは「50 ms 後の tick ではまだ cooldown 中」を見る。負荷で tick が遅れても明けないよう、
                // cooldown は十分長く取る（`run_until_idle` は 20 ms × 300 tick = 6 秒まで待つ）。
                return Err(AdapterError::Throttled {
                    retry_after: Duration::from_millis(1500),
                });
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
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "test -f touched".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();
        let adapter = Arc::new(FlakyProviderAdapter {
            calls: AtomicUsize::new(0),
        });
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
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "test -f never".into(),
                expect_exit: 0,
            },
            1,
        );
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "claimed".into(),
                evidence: vec![],
                usage: None,
            },
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
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
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
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "test -f touched".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();
        let mut d = dispatcher(store.clone(), Arc::new(HeartbeatAdapter), 1);
        d.config.lease_grace = Duration::from_millis(400);
        let before = OffsetDateTime::now_utc();
        assert_eq!(d.tick().unwrap().dispatched, 1);
        let initial = store
            .get(task.id)
            .unwrap()
            .unwrap()
            .lease
            .unwrap()
            .expires_at;
        assert!(
            initial > before + time::Duration::seconds(25),
            "acquired with max_wall_secs + grace"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        let renewed = store
            .get(task.id)
            .unwrap()
            .unwrap()
            .lease
            .expect("still running")
            .expires_at;
        assert!(renewed < initial, "renewed={renewed} initial={initial}");
        assert!(
            renewed > OffsetDateTime::now_utc() + time::Duration::seconds(4),
            "ttl = idle_timeout(5s) + grace"
        );
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
    }

    /// レビューが無く、`sink.heartbeat()` も呼ばない（テストが lease の自然な更新に邪魔されないよう
    /// に）、しばらく走り続けるだけのアダプタ。
    struct SlowNoHeartbeatAdapter;

    #[async_trait]
    impl WorkerAdapter for SlowNoHeartbeatAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            _req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(done_outcome())
        }
    }

    /// ADR-0070 D5（Phase 116。D6(c)）: lease が期限切れでも、dispatcher が抱えている run の
    /// プロセスがまだ生きていれば（`process_group::group_alive`）、`reclaim_expired_leases` は
    /// reclaim せず lease を延長する（DB busy で数 tick 更新できなかっただけ、という実際の事故を
    /// 再現する）。「偽の子プロセス」はテスト自身の pid を run_id に登録するだけで作る（本物の
    /// 子プロセスを起こす必要はない。`process_group::group_alive` は「その pid が生きているか」しか
    /// 見ないため）。
    #[tokio::test]
    async fn a_lease_past_its_expiry_is_extended_instead_of_reclaimed_while_the_process_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.budget.max_wall_secs = 0;
        store.insert(&task).unwrap();
        let mut d = dispatcher(store.clone(), Arc::new(SlowNoHeartbeatAdapter), 1);
        d.config.lease_grace = Duration::from_millis(20);
        assert_eq!(d.tick().unwrap().dispatched, 1);

        let run_id = store
            .get(task.id)
            .unwrap()
            .unwrap()
            .lease
            .expect("running with a lease")
            .worker_run_id;
        assert!(
            !task_worker::process_group::group_alive(&run_id),
            "the fake in-process adapter never registers a real child"
        );
        // 「偽の子プロセス」を自分の pid で登録する（本物の子プロセスは要らない。生死しか見ないため）。
        let guard =
            task_worker::process_group::ProcessGroup::register(&run_id, Some(std::process::id()));
        assert!(task_worker::process_group::group_alive(&run_id));

        // lease が確実に切れるまで待つ（max_wall_secs(0) + lease_grace(20ms)）。
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(
            store
                .get(task.id)
                .unwrap()
                .unwrap()
                .lease
                .unwrap()
                .expires_at
                < OffsetDateTime::now_utc(),
            "the lease must actually be expired before reclaim runs"
        );

        let reclaimed = d.reclaim_expired_leases().unwrap();
        assert_eq!(
            reclaimed, 0,
            "a still-alive process is extended, not reclaimed"
        );
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Running, 0));
        assert!(
            t.lease.unwrap().expires_at > OffsetDateTime::now_utc(),
            "the lease was extended instead of reclaimed"
        );
        assert!(
            d.running.contains_key(&task.id),
            "the run entry is still tracked (not removed by reclaim)"
        );

        drop(guard);
        // 死んでいれば（登録を外せば `group_alive` が偽になる）今度こそ reclaim される。
        tokio::time::sleep(Duration::from_millis(30)).await;
        let reclaimed = d.reclaim_expired_leases().unwrap();
        assert_eq!(reclaimed, 1, "a dead process is reclaimed as usual");
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(
            (t.status, t.attempts),
            (Status::Ready, 0),
            "InfraRequeue does not consume attempts"
        );
        let events = store.events_for(task.id).unwrap();
        assert!(events.iter().any(|(_, e)| matches!(
            e,
            Event::Transitioned { from: Status::Running, to: Status::Ready, reason } if reason == "infra_requeue"
        )));
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
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
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
            check: Check::Command {
                cmd: "test -f second".into(),
                expect_exit: 0,
            },
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
                .apply_transition(
                    id,
                    Trigger::Approve,
                    Some(Event::ApprovalDecided {
                        by: "human".into(),
                        approved: true,
                        note: None,
                    }),
                )
                .unwrap();
        };
        let mut d = dispatcher(
            store.clone(),
            Arc::new(CountingAdapter {
                calls: AtomicUsize::new(0),
            }),
            2,
        );

        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle, "only a human can make progress now");
        let first = approvals(&store);
        assert_eq!(first.len(), 1);
        assert!(
            first[0].title.ends_with("(attempt 1)"),
            "{}",
            first[0].title
        );
        assert_eq!(
            store.get(task_id).unwrap().unwrap().status,
            Status::Reviewing
        );

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
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            if req.task.kind == TaskKind::Review {
                if self.review_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(AdapterError::Throttled {
                        retry_after: Duration::from_millis(200),
                    });
                }
                std::fs::create_dir_all(&req.artifacts_dir).unwrap();
                std::fs::write(
                    req.artifacts_dir.join("review.json"),
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
        let adapter = Arc::new(FlakyReviewerAdapter {
            review_calls: AtomicUsize::new(0),
        });
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
                Event::WorkerFinished {
                    outcome,
                    role: Some(RunRole::Reviewer),
                    ..
                } => Some(outcome.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(reviewer_outcomes.len(), 2, "{events:?}");
        assert!(
            reviewer_outcomes[0].starts_with("requeue: "),
            "{reviewer_outcomes:?}"
        );
        assert!(
            reviewer_outcomes[1].starts_with("done: "),
            "{reviewer_outcomes:?}"
        );
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
                Event::WorkerStarted {
                    run_id,
                    role,
                    provider,
                    ..
                } => Some((run_id.clone(), *role, provider.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(started.len(), 2, "{events:?}");
        assert_eq!(
            (started[0].1, started[1].1),
            (None, Some(RunRole::Reviewer))
        );
        assert_eq!(started[1].2.as_deref(), Some("p1"));
        let reviewer_finished: Vec<&str> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerFinished {
                    run_id,
                    outcome,
                    role: Some(RunRole::Reviewer),
                    ..
                } if *run_id == started[1].0 => Some(outcome.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(reviewer_finished.len(), 1, "{events:?}");
        assert!(
            reviewer_finished[0].starts_with("done: "),
            "{reviewer_finished:?}"
        );
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
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            if self.review_only && req.task.kind != TaskKind::Review {
                return Ok(done_outcome());
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(AdapterError::Throttled {
                retry_after: Duration::from_millis(10),
            })
        }
    }

    /// Phase 113（ADR-0054 D2）: `AlwaysThrottledAdapter` と違い、`Err(AdapterError::Throttled)`
    /// （プロバイダが分類できる供給側失敗）ではなく `Ok(Terminal::Error{retryable: true})` を返す
    /// （`is_error` の結果・クラッシュ相当。分類できない「reviewer run 自身のインフラ都合の失敗」）。
    struct AlwaysInfraFailingReviewAdapter {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl WorkerAdapter for AlwaysInfraFailingReviewAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            if req.task.kind != TaskKind::Review {
                return Ok(done_outcome());
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(RunOutcome {
                terminal: Terminal::Error {
                    message: "claude result: error_during_execution".into(),
                    retryable: true,
                },
                exit_code: Some(1),
            })
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
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            1,
        );
        store.insert(&task).unwrap();
        let adapter = Arc::new(AlwaysThrottledAdapter {
            calls: AtomicUsize::new(0),
            review_only: false,
        });
        let mut d = dispatcher(store.clone(), adapter.clone(), 1);
        d.config.max_requeues = 2;
        let report = run_until_idle(&mut d, 500).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Failed, 2));
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 6);
        let one_attempt = [
            "dispatch",
            "requeue",
            "dispatch",
            "requeue",
            "dispatch",
            "worker_error",
        ];
        let expected: Vec<&str> = one_attempt
            .iter()
            .chain(one_attempt.iter())
            .copied()
            .collect();
        assert_eq!(transition_reasons(&store, task.id), expected);
        let events = store.events_for(task.id).unwrap();
        assert!(events.iter().any(|(_, e)| matches!(e, Event::WorkerFinished { outcome, .. } if outcome.contains("requeue limit (2) reached"))));

        // max_requeues = 0 なら最初の供給側失敗から attempts を消費する。
        let task0 = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task0).unwrap();
        d.config.max_requeues = 0;
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(
            transition_reasons(&store, task0.id),
            vec!["dispatch", "worker_error"]
        );
    }

    /// 常に分類できない `Err`（`AdapterError::Other`）を返すワーカー run（ADR-0070 D3。resume 拒否や
    /// result.json 不在などが実機で取る経路のスタンド・イン）。
    struct AlwaysInfraFailingWorkerAdapter {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl WorkerAdapter for AlwaysInfraFailingWorkerAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            _req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(AdapterError::Other("session resume rejected".into()))
        }
    }

    /// ADR-0070 D3（Phase 116。D6(b)）: ワーカー run 自身のインフラ都合の失敗（プロバイダが分類でき
    /// ない `Err`）は `max_infra_retries` まで `attempts` を消費せず `Trigger::InfraRequeue` で
    /// 再試行し、上限に達したときだけ `WorkerError{retryable:false}` で `"infra failure ×N"` として
    /// 打ち切る（そのときだけ `attempts` が 1 進む）。`infra_backoff` はテストのために毎 tick 手で
    /// 解除する（本番の 30秒/2分/5分のタイミングそのものは `derive::infra_backoff_delay` の単体
    /// テストで確かめている）。
    #[tokio::test]
    async fn infra_failure_retry_limit_fails_the_task_without_consuming_attempts_until_then() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();
        let adapter = Arc::new(AlwaysInfraFailingWorkerAdapter {
            calls: AtomicUsize::new(0),
        });
        let mut d = dispatcher(store.clone(), adapter.clone(), 1);
        d.config.max_infra_retries = 2;

        let mut report = TickReport::default();
        for _ in 0..200 {
            report = d.tick().unwrap();
            // インフラ再試行のバックオフ（30秒〜）を待たない。カウント・分類のテストなので、
            // タイミングそのものは触らない（別途 derive::infra_backoff_delay の単体テストが確かめる）。
            d.infra_backoff.clear();
            if report.idle {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(
            (t.status, t.attempts),
            (Status::Failed, 1),
            "2回のinfra_requeueはattemptsを消費せず、3回目で初めてfailed（attempts+1）"
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            transition_reasons(&store, task.id),
            vec![
                "dispatch",
                "infra_requeue",
                "dispatch",
                "infra_requeue",
                "dispatch",
                "worker_error"
            ]
        );
        let events = store.events_for(task.id).unwrap();
        assert!(events.iter().any(|(_, e)| matches!(
            e,
            Event::WorkerFinished { outcome, .. } if outcome.starts_with("infra failure ×3: ")
        )));
        let (class, reason) = task_ops::derive::classify_task_failure(&events);
        assert_eq!(class, task_ops::derive::FailureClass::Infra);
        assert!(reason.starts_with("infra failure ×3: "));
    }

    /// 監査 M-1〜M-3（ADR-0034 D2）: 報告はタスクの終端状態に合わせて作るテスト向けの、最小の組織（秘書 → coding → coding-poc）。
    fn seed_org_for_reports(store: &Arc<dyn TaskStore>) {
        let now = OffsetDateTime::now_utc();
        for (id, parent, kind) in [
            ("secretary", None, OrgKind::Secretary),
            ("coding", Some("secretary"), OrgKind::Department),
            ("coding-poc", Some("coding"), OrgKind::Section),
        ] {
            store
                .org_upsert(&OrgNode {
                    profile: Default::default(),
                    id: id.into(),
                    parent_id: parent.map(str::to_string),
                    name: id.into(),
                    kind,
                    genre: None,
                    brief: String::new(),
                    position: 0,
                    created_at: now,
                    updated_at: now,
                })
                .unwrap();
        }
    }

    /// 2 回目以降の run で `ready` ファイルを作るアダプタ(レビューが 1 回差し戻されてから通る状況を作る)。
    struct AttemptGatedAdapter {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl WorkerAdapter for AttemptGatedAdapter {
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
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n >= 1 {
                std::fs::write(req.workspace.join("ready"), "1").unwrap();
            }
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: "ok".into(),
                    evidence: vec![],
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    /// 監査 M-1/M-2: 1 回目の「できました」がレビューで差し戻され、2 回目で通っても、`result` 報告は 1 件だけ。
    #[tokio::test]
    async fn a_review_retry_that_eventually_passes_produces_exactly_one_done_report() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_org_for_reports(&store);
        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: "test -f ready".into(),
                expect_exit: 0,
            },
            1,
        );
        task.assignee = Some("coding-poc".into());
        task.project_id = Some(ProjectId::new());
        store.insert(&task).unwrap();
        let adapter = Arc::new(AttemptGatedAdapter {
            calls: AtomicUsize::new(0),
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let report = run_until_idle(&mut d, 300).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Done);
        assert_eq!(
            t.attempts, 1,
            "1 回目のレビュー差し戻しで attempts を消費し、2 回目で done になる"
        );
        let reports = store.report_list(&ReportFilter::default()).unwrap();
        let done_reports: Vec<_> = reports
            .iter()
            .filter(|r| r.kind == ReportKind::Result)
            .collect();
        assert_eq!(
            done_reports.len(),
            1,
            "差し戻された 1 回目は報告にせず、done の報告は 1 件だけ: {reports:?}"
        );
    }

    /// 監査 M-1: 供給側失敗が requeue の上限に達して通常の失敗（`Status::Failed`）になったら、bad_news を 1 件作る。
    #[tokio::test]
    async fn requeue_limit_reached_produces_one_bad_news_report() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_org_for_reports(&store);
        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.assignee = Some("coding-poc".into());
        task.project_id = Some(ProjectId::new());
        store.insert(&task).unwrap();
        let adapter = Arc::new(AlwaysThrottledAdapter {
            calls: AtomicUsize::new(0),
            review_only: false,
        });
        let mut d = dispatcher(store.clone(), adapter.clone(), 1);
        d.config.max_requeues = 1;
        let report = run_until_idle(&mut d, 500).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Failed);
        let reports = store.report_list(&ReportFilter::default()).unwrap();
        let bad_news_at_source: Vec<_> = reports
            .iter()
            .filter(|r| r.kind == ReportKind::BadNews && r.node_id == "coding-poc")
            .collect();
        assert_eq!(
            bad_news_at_source.len(),
            1,
            "requeue 上限で failed になったら bad_news は 1 件だけ: {reports:?}"
        );
    }

    /// worker が返す `retryable: true` の `error` を毎回返すアダプタ(供給側失敗ではなく、ワーカー自身の申告)。
    struct RetryableErrorAdapter;

    #[async_trait]
    impl WorkerAdapter for RetryableErrorAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            _req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            sink.progress("working");
            Ok(RunOutcome {
                terminal: Terminal::Error {
                    message: "flaky".into(),
                    retryable: true,
                },
                exit_code: Some(1),
            })
        }
    }

    /// 監査 M-1: `max_retries` に余裕があるうちの retryable な失敗は `Status::Ready` に戻るだけで、
    /// bad_news をリトライのたびに作らない(以前は `WorkerError` のたびに bad_news が飛んでいた)。
    #[tokio::test]
    async fn three_retryable_worker_errors_in_a_row_produce_no_bad_news_report() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_org_for_reports(&store);
        // max_retries は大きめに取り、3 回失敗するまでの間は確実に `ready` に戻るだけにする。
        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            50,
        );
        task.assignee = Some("coding-poc".into());
        task.project_id = Some(ProjectId::new());
        store.insert(&task).unwrap();
        let adapter = Arc::new(RetryableErrorAdapter);
        let mut d = dispatcher(store.clone(), adapter, 1);
        // 少なくとも 3 回、retryable な失敗を経験するまで tick する(タイミングにより 3 を超えても構わない。
        // ここで確かめたいのは「failed になる前に bad_news が作られないこと」)。
        for _ in 0..200 {
            d.tick().unwrap();
            let t = store.get(task.id).unwrap().unwrap();
            if t.attempts >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let t = store.get(task.id).unwrap().unwrap();
        assert!(
            t.attempts >= 3,
            "少なくとも 3 回は retryable な失敗を経たはず: attempts={}",
            t.attempts
        );
        assert_ne!(
            t.status,
            Status::Failed,
            "max_retries=50 なのでまだ failed にならない"
        );
        let reports = store.report_list(&ReportFilter::default()).unwrap();
        assert_eq!(
            reports
                .iter()
                .filter(|r| r.kind == ReportKind::BadNews)
                .count(),
            0,
            "途中の retryable な失敗では bad_news を作らない: {reports:?}"
        );
    }

    /// ADR-0011（P-38）: Reviewer run の供給側失敗による延期も max_requeues までで、超えたら Reviewer 条件を fail にして判定する。
    #[tokio::test]
    async fn reviewer_requeue_limit_fails_reviewer_criteria() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Reviewer, 0);
        store.insert(&task).unwrap();
        let adapter = Arc::new(AlwaysThrottledAdapter {
            calls: AtomicUsize::new(0),
            review_only: true,
        });
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

    /// Phase 113 D2/D4(c)（ADR-0054 追記。実機障害 2026-09-23、タスク 01M35X86XTK84F97QW0CN5PGMR）:
    /// reviewer run **自身のインフラ都合の失敗**（`is_error`/クラッシュ相当。`AlwaysThrottledAdapter`
    /// が使う「プロバイダが分類できる供給側失敗」とは別カウンタ）は `max_reviewer_retries` までは
    /// `fail_all` せず reviewing のままやり直す。上限に達したときだけ「reviewer infra failure ×N」で
    /// 不合格にする（`consecutive_reviewer_requeues` とは別に `consecutive_reviewer_infra_failures` で
    /// 数える）。
    #[tokio::test]
    async fn reviewer_infra_failure_retry_limit_fails_reviewer_criteria() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(dir.path(), Check::Reviewer, 0);
        store.insert(&task).unwrap();
        let adapter = Arc::new(AlwaysInfraFailingReviewAdapter {
            calls: AtomicUsize::new(0),
        });
        let mut d = dispatcher(store.clone(), adapter.clone(), 2);
        d.config.max_reviewer_retries = 2;
        let report = run_until_idle(&mut d, 500).await;
        assert!(report.idle);
        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Failed, 1));
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 3);
        let events = store.events_for(task.id).unwrap();
        // 最後の遷移（review_fail）の直前までで数える（その後ろには ReviewVerdict が続く）。
        let last_transition = events
            .iter()
            .rposition(|(_, e)| matches!(e, Event::Transitioned { .. }))
            .unwrap();
        assert_eq!(
            consecutive_reviewer_infra_failures(&events[..last_transition]),
            2
        );
        // プロバイダが分類できる供給側失敗のカウンタ（`REVIEWER_REQUEUED_PREFIX`）は動かない
        // （別軸であることの裏取り）。
        assert_eq!(consecutive_reviewer_requeues(&events[..last_transition]), 0);
        assert!(events.iter().any(|(_, e)| matches!(e, Event::ReviewVerdict { pass: false, reason, .. } if reason.starts_with("reviewer infra failure ×2"))));
    }

    /// 監査の指摘（ADR-0012 D2）: 取得窓（max_concurrency*4+16）を優先度の高い経路なしタスクが埋めても、窓の外の実行可能な
    /// タスクが dispatch され、それが終わるまで idle にならない。
    #[tokio::test]
    async fn unroutable_tasks_do_not_starve_or_hide_routable_tasks_outside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut unroutable = Vec::new();
        for _ in 0..25 {
            let mut t = new_task(
                dir.path(),
                Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
                0,
            );
            t.priority = 10;
            t.worker_hint.adapter = Some("nonexistent".into());
            store.insert(&t).unwrap();
            unroutable.push(t.id);
        }
        let routable = new_task(
            dir.path(),
            Check::Command {
                cmd: "test -f touched".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&routable).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let first = d.tick().unwrap();
        assert!(
            !first.idle,
            "a routable task is still waiting beyond the window"
        );
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        assert_eq!(
            store.get(routable.id).unwrap().unwrap().status,
            Status::Done
        );
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
            let mut t = new_task(
                dir.path(),
                Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
                0,
            );
            t.workspace = WorkspaceSpec::Remote {
                cluster: "slow".into(),
                path: dir.path().to_path_buf(),
                mode: None,
            };
            store.insert(&t).unwrap();
            remote_ids.push(t.id);
        }
        let local = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&local).unwrap();

        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
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
                host: "celeris-localhost".into(),
                concurrency: 1,
                sync: SyncMode::None,
                delete_on_push: false,
                setup: vec![],
                env: vec![],
                rsync_excludes: vec![],
                worktree: Default::default(),
                auth: "manual".into(),
                forwards: vec![],
                work_dir: None,
                keepalive_secs: 0,
                liveness_probe_secs: 0,
            },
        );
        if !control_master_alive_blocking(&["ssh".to_string()], "celeris-localhost") {
            eprintln!("skip: celeris-localhost への多重接続が無い");
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
        assert_eq!(
            store.get(local.id).unwrap().unwrap().status,
            Status::Running,
            "ローカルはクラスタの枠を使わない"
        );

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
        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.workspace = WorkspaceSpec::Remote {
            cluster: "offline".into(),
            path: PathBuf::from("/remote/project"),
            mode: None,
        };
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.clusters.insert(
            "offline".into(),
            ClusterSpec {
                id: "offline".into(),
                host: "celeris-no-such-host-for-tests".into(),
                concurrency: 1,
                sync: SyncMode::Rsync,
                delete_on_push: false,
                setup: vec![],
                env: vec![],
                rsync_excludes: vec![],
                worktree: Default::default(),
                auth: "manual".into(),
                forwards: vec![],
                work_dir: None,
                keepalive_secs: 0,
                liveness_probe_secs: 0,
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
        assert!(
            report.idle,
            "a task waiting for a human login does not keep the daemon from going idle"
        );

        let snap = rx.borrow().clone().expect("snapshot published");
        assert_eq!(snap.clusters.len(), 1, "{snap:?}");
        let live = &snap.clusters[0];
        assert_eq!(
            (
                live.id.as_str(),
                live.host.as_str(),
                live.concurrency,
                live.in_use,
                live.connected
            ),
            ("offline", "celeris-no-such-host-for-tests", 1, 0, false)
        );
        let until = live.cooldown_until.clone().expect("cooldown_until");
        assert!(
            until > snap.last_tick_at,
            "cooldown ends after the tick: {until} vs {}",
            snap.last_tick_at
        );
        assert!(
            !snap.unroutable.contains(&task.id),
            "人待ちは経路なしではない（監査 4-1）: {snap:?}"
        );
        assert!(d.cluster_waiting.contains(&task.id));

        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!((t.status, t.attempts), (Status::Ready, 0));
        let events = store.events_for(task.id).unwrap();
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                Event::ClusterUnavailable { cluster, host, .. } if cluster == "offline" && host == "celeris-no-such-host-for-tests"
            )),
            "{events:?}"
        );
        // 2 tick 目: cooldown 中は再度イベントを足さない（1 件のまま）。
        d.tick().unwrap();
        let again = store.events_for(task.id).unwrap();
        assert_eq!(
            again
                .iter()
                .filter(|(_, e)| matches!(e, Event::ClusterUnavailable { .. }))
                .count(),
            1,
            "{again:?}"
        );
    }

    /// ADR-0062 B1（Phase 107）: 担当に `cluster:<id>` が無い remote タスクは、設定に無いクラスタ
    /// （`ClusterUnavailable` / `unroutable`）とは違い、`blocked` にして人に質問する
    /// （「no such cluster in the config」という誤解を招く文言は出さない）。
    #[tokio::test]
    async fn assignee_without_the_cluster_tool_is_blocked_with_a_question() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let now = OffsetDateTime::now_utc();
        for (id, parent, tools) in [
            ("cos", None, vec![]),
            ("web-research", Some("cos"), vec!["tavily".to_string()]),
        ] {
            store
                .org_upsert(&OrgNode {
                    profile: task_core::Profile {
                        tools,
                        ..Default::default()
                    },
                    id: id.into(),
                    parent_id: parent.map(str::to_string),
                    name: id.into(),
                    kind: if parent.is_none() {
                        OrgKind::Secretary
                    } else {
                        OrgKind::Department
                    },
                    genre: None,
                    brief: String::new(),
                    position: 0,
                    created_at: now,
                    updated_at: now,
                })
                .unwrap();
        }

        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.workspace = WorkspaceSpec::Remote {
            cluster: "sirius".into(),
            path: PathBuf::from("/remote/project"),
            mode: None,
        };
        task.assignee = Some("web-research".into());
        store.insert(&task).unwrap();

        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.clusters.insert(
            "sirius".into(),
            cluster_spec_with_auth("sirius", "sirius", "manual"),
        );

        let report = d.tick().unwrap();
        assert_eq!(report.dispatched, 0);

        let t = store.get(task.id).unwrap().unwrap();
        assert_eq!(t.status, Status::Blocked, "{t:?}");
        let events = store.events_for(task.id).unwrap();
        assert!(
            events.iter().any(
                |(_, e)| matches!(e, Event::QuestionRaised { text, .. } if text.contains("cluster:sirius"))
            ),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|(_, e)| matches!(e, Event::ClusterUnavailable { .. })),
            "設定の問題ではないので ClusterUnavailable は出さない: {events:?}"
        );

        // warn の dedupe: 2 tick 目でも同じ 1 件のまま増えない（そもそも blocked なので ready_tasks に
        // 出てこない。`warned_cluster_tool` の dedupe 自体は `task_may_use_cluster` の単体でも効く）。
        let events_again = store.events_for(task.id).unwrap();
        assert_eq!(events_again.len(), events.len());
    }

    /// ADR-0062 A（Phase 107）: celeris が保持していた master が明示的な切断を経ずに自分で終了した
    /// ことを `set_cluster_master_watcher` で知らせると、次に `mark_cluster_unavailable` がそのクラスタを
    /// 拾ったときに `Event::ClusterMasterExited`（stderr の末尾・exit code 付き）を 1 回だけ残す。
    #[tokio::test]
    async fn a_master_that_exited_on_its_own_is_reported_once_with_its_stderr_tail() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.workspace = WorkspaceSpec::Remote {
            cluster: "sirius".into(),
            path: PathBuf::from("/remote/project"),
            mode: None,
        };
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.clusters.insert(
            "sirius".into(),
            cluster_spec_with_auth("sirius", "celeris-no-such-host-for-tests", "manual"),
        );
        d.set_cluster_liveness_probe(Arc::new(|_ssh_command: &[String], _host: &str| false));
        let watcher_calls = Arc::new(AtomicUsize::new(0));
        let calls = watcher_calls.clone();
        d.set_cluster_master_watcher(Arc::new(move || {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                vec![ClusterMasterExit {
                    cluster: "sirius".into(),
                    exit_code: Some(255),
                    stderr_tail: "mux_client_request_session: read from master failed: Broken pipe"
                        .into(),
                }]
            } else {
                vec![]
            }
        }));

        d.tick().unwrap();
        let events = store.events_for(task.id).unwrap();
        let exits: Vec<_> = events
            .iter()
            .filter(|(_, e)| matches!(e, Event::ClusterMasterExited { .. }))
            .collect();
        assert_eq!(exits.len(), 1, "{events:?}");
        let Event::ClusterMasterExited {
            cluster,
            exit_code,
            stderr_tail,
        } = &exits[0].1
        else {
            unreachable!()
        };
        assert_eq!(cluster, "sirius");
        assert_eq!(*exit_code, Some(255));
        assert!(stderr_tail.contains("Broken pipe"), "{stderr_tail}");
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                Event::ClusterUnavailable { reason, .. } if reason.contains("Broken pipe") && reason.contains("255")
            )),
            "reason に exit code と stderr が入る: {events:?}"
        );

        // 2 tick 目: watcher はもう新しい終了を返さないので、`ClusterMasterExited` は増えない
        // （cooldown 中でもあるので `mark_cluster_unavailable` 自体もう呼ばれない）。
        d.tick().unwrap();
        let events_again = store.events_for(task.id).unwrap();
        let exits_again = events_again
            .iter()
            .filter(|(_, e)| matches!(e, Event::ClusterMasterExited { .. }))
            .count();
        assert_eq!(exits_again, 1, "{events_again:?}");
    }

    /// ADR-0062 A（Phase 107）: `-O check` は生きている（unix ソケットはある）が、master 越しの実通信
    /// probe（`ssh -- true`）が失敗するとき、`refresh_cluster_liveness` は接続を死んだと判定し、
    /// `-O exit` の片付けフック（`cluster_disconnector`）を 1 回呼ぶ。
    #[test]
    fn a_failing_command_probe_marks_the_connection_dead_and_cleans_up() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        let mut spec = cluster_spec_with_auth("sirius", "sirius", "manual");
        spec.liveness_probe_secs = 30;
        d.config.clusters.insert("sirius".into(), spec);
        // `-O check` は常に成功（unix ソケットはある）。
        d.set_cluster_liveness_probe(Arc::new(|_ssh_command: &[String], _host: &str| true));
        // 実通信 probe は失敗（NAT の idle timeout で TCP が黙って死んでいる、を模す）。
        d.set_cluster_command_probe(Arc::new(
            |_ssh_command: &[String], _host: &str, _timeout: Duration| false,
        ));
        let disconnect_calls = Arc::new(AtomicUsize::new(0));
        let calls = disconnect_calls.clone();
        d.set_cluster_disconnector(Arc::new(move |_id: &str, _host: &str| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));

        d.refresh_cluster_liveness();
        assert_eq!(
            d.cluster_connected.get("sirius"),
            Some(&false),
            "実通信 probe が失敗したら死んだ扱いにする"
        );
        assert_eq!(
            disconnect_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "-O exit で片付ける"
        );
        assert!(
            d.cluster_disconnect_info
                .get("sirius")
                .is_some_and(|info| info.detail.contains("liveness probe")),
            "{:?}",
            d.cluster_disconnect_info
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
            forwards: vec![],
            work_dir: None,
            keepalive_secs: 0,
            liveness_probe_secs: 0,
        }
    }

    /// ADR-0053 D3（Phase 66）: forward 付きの `ClusterSpec` を作る（`refresh_cluster_tunnels` 用）。
    /// `probe_interval_secs` は既定（`DEFAULT_TUNNEL_PROBE_INTERVAL_SECS`）にしておく。ADR-0066 D3
    /// 以降、target probe のバックオフは `d.tunnel_probe_state`（専用スレッドが書く）に移った。
    /// 間引きを避けたいテストは `d.last_cluster_tunnel_refresh` を過去にずらす（`CLUSTER_LIVENESS_INTERVAL`
    /// の間引きだけがテストから直接いじれる。target probe 側は 1 forward につき最初の 1 回だけ同期に
    /// 種を蒔くので、通常のテストはそれだけで足りる）。
    fn cluster_spec_with_forward(
        id: &str,
        host: &str,
        auth: &str,
        listen: &str,
        target: &str,
    ) -> ClusterSpec {
        let mut spec = cluster_spec_with_auth(id, host, auth);
        spec.forwards = vec![ClusterForwardSpec {
            listen: listen.into(),
            target: target.into(),
            probe_interval_secs: DEFAULT_TUNNEL_PROBE_INTERVAL_SECS,
        }];
        spec
    }

    /// ADR-0059 D1（Phase 99）: `mode: Shared` は、クラスタの `sync` 設定に関わらず `SyncMode::None`
    /// を強制する。`mode` 省略（既定の `Worktree`）は従来どおりクラスタの `sync` に従う。
    #[test]
    fn ssh_settings_shared_mode_forces_sync_none_regardless_of_cluster_sync() {
        let mut spec = cluster_spec_with_auth("pegasus", "pegasus", "manual");
        spec.sync = SyncMode::Worktree;
        let task_id = TaskId::new();

        let worktree_settings = spec.ssh_settings(
            std::path::Path::new("/work/x"),
            task_id,
            WorkspaceMode::Worktree,
        );
        assert_eq!(worktree_settings.sync, SyncMode::Worktree);

        let shared_settings = spec.ssh_settings(
            std::path::Path::new("/work/x"),
            task_id,
            WorkspaceMode::Shared,
        );
        assert_eq!(shared_settings.sync, SyncMode::None);

        // rsync クラスタでも同じ: 省略（既定）は従来どおり、shared は None を強制する。
        spec.sync = SyncMode::Rsync;
        assert_eq!(
            spec.ssh_settings(
                std::path::Path::new("/work/x"),
                task_id,
                WorkspaceMode::Worktree
            )
            .sync,
            SyncMode::Rsync
        );
        assert_eq!(
            spec.ssh_settings(
                std::path::Path::new("/work/x"),
                task_id,
                WorkspaceMode::Shared
            )
            .sync,
            SyncMode::None
        );
    }

    /// ADR-0059 D6（Phase 99）: `cluster_of` の実効 `work_dir` は DB の上書き（`cluster_settings`）
    /// > 設定ファイルの `[[clusters]] work_dir` > 無し、の順。相対パスはそこからの相対に解決する。
    #[test]
    fn cluster_of_resolves_relative_paths_against_the_db_override_then_the_config_work_dir() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter: Arc<dyn WorkerAdapter> = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let mut spec = cluster_spec_with_auth("pegasus", "pegasus", "manual");
        spec.work_dir = Some(PathBuf::from("/work/NBB/config-default"));
        d.config.clusters.insert("pegasus".into(), spec);

        let mut task = new_task(
            std::path::Path::new("/unused"),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.workspace = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::from("benchfs"),
            mode: None,
        };

        // DB の上書きがまだ無ければ設定ファイルの `work_dir` を使う。
        let (_, resolved, mode) = d.cluster_of(&task).expect("cluster configured");
        assert_eq!(resolved, PathBuf::from("/work/NBB/config-default/benchfs"));
        assert_eq!(mode, WorkspaceMode::Worktree);

        // DB の上書きがあればそちらが勝つ。
        store
            .cluster_settings_set(
                "pegasus",
                Some("/work/NBB/db-override"),
                OffsetDateTime::now_utc(),
            )
            .unwrap();
        let (_, resolved, _) = d.cluster_of(&task).expect("cluster configured");
        assert_eq!(resolved, PathBuf::from("/work/NBB/db-override/benchfs"));

        // 絶対パス・`~` はどちらの `work_dir` からも独立にそのまま使う。
        task.workspace = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::from("/scratch/x"),
            mode: Some(WorkspaceMode::Shared),
        };
        let (_, resolved, mode) = d.cluster_of(&task).expect("cluster configured");
        assert_eq!(resolved, PathBuf::from("/scratch/x"));
        assert_eq!(mode, WorkspaceMode::Shared);
    }

    /// ADR-0059 D6: `work_dir` がどこにも無ければ、相対・空の `path` は解決できず、`cluster_of` は
    /// 受け取った `path` をそのまま返す（`remote_dir_is_resolved` で「解決できなかった」と判定できる形）。
    #[test]
    fn cluster_of_leaves_the_path_unresolved_when_there_is_no_work_dir_anywhere() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter: Arc<dyn WorkerAdapter> = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "pegasus".into(),
            cluster_spec_with_auth("pegasus", "pegasus", "manual"),
        );
        let mut task = new_task(
            std::path::Path::new("/unused"),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.workspace = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::new(),
            mode: None,
        };
        let (_, resolved, _) = d.cluster_of(&task).expect("cluster configured");
        assert_eq!(resolved, PathBuf::new());
        assert!(!task_worker::remote_dir_is_resolved(&resolved));
    }

    /// ADR-0059 D3（Phase 99）: 実 ssh を起こさずに worktree 準備の exit コードを模す。`-O check`
    /// （多重接続の確認。`control_master_alive` から呼ばれうる）には常に成功で答え、それ以外の呼び出しは
    /// `exit_code` を返す（中身は見ない。`ensure_worktree` の分岐はコードそのものだけで決まる）。
    fn write_stub_ssh(dir: &std::path::Path, exit_code: i32) -> PathBuf {
        let script = dir.join("stub-ssh.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$3\" = \"-O\" ] && [ \"$4\" = \"check\" ]; then exit 0; fi\nexit {exit_code}\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }
        script
    }

    fn remote_task(
        dir: &std::path::Path,
        mode: Option<WorkspaceMode>,
        repos: Vec<task_core::RepoRef>,
    ) -> Task {
        let mut task = new_task(
            dir,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.workspace = WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::from("/work/proj"),
            mode,
        };
        task.repos = repos;
        task
    }

    fn done_adapter() -> Arc<dyn WorkerAdapter> {
        Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_worker_for_test(
        store: Arc<dyn TaskStore>,
        task_id: TaskId,
        dir: PathBuf,
        settings: SshSettings,
    ) -> Result<RunOutcome, AdapterError> {
        run_worker(
            store,
            done_adapter(),
            task_id,
            Tier::Standard,
            dir,
            "run1",
            RunLimits {
                wall_clock: Duration::from_secs(30),
                idle_timeout: Duration::from_secs(5),
                kill_grace: Duration::from_millis(100),
            },
            LeaseRenewal {
                ttl: Duration::from_secs(60),
                every: Duration::from_secs(30),
            },
            Some(settings),
            None,
            RunExtras::default(),
            Vec::new(),
            Vec::new(),
            DelegationLimits::default(),
            None,
            None,
            ContainerDecision::Host,
            None,
        )
        .await
    }

    /// ADR-0059 D3: `mode` 省略・`repos` 無し（コードを触らない仕事）で worktree 準備が exit 65 になったら、
    /// 失敗にせず `shared`（`SyncMode::None`）として続行し、`Event::WorkspaceModeDowngraded` を残し、
    /// `task.workspace.mode` を `Some(Shared)` に書き戻す。
    #[tokio::test]
    async fn exit_65_with_no_mode_and_no_repos_downgrades_to_shared_and_continues() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let stub = write_stub_ssh(tmp.path(), 65);
        let task = remote_task(tmp.path(), None, Vec::new());
        store.insert(&task).unwrap();

        let mut settings = SshSettings::new("pegasus", "pegasus", PathBuf::from("/work/proj"));
        settings.sync = SyncMode::Worktree;
        settings.task_id = task.id.to_string();
        settings.ssh_command = vec![stub.to_string_lossy().into_owned()];

        let outcome =
            run_worker_for_test(store.clone(), task.id, tmp.path().join("mirror"), settings).await;
        assert!(outcome.is_ok(), "{:?}", outcome.err());

        let events = store.events_for(task.id).unwrap();
        assert!(
            events
                .iter()
                .any(|(_, e)| matches!(e, Event::WorkspaceModeDowngraded { cluster, .. } if cluster == "pegasus")),
            "{events:?}"
        );
        let stored = store.get(task.id).unwrap().expect("task exists");
        assert_eq!(
            stored.workspace,
            WorkspaceSpec::Remote {
                cluster: "pegasus".into(),
                path: PathBuf::from("/work/proj"),
                mode: Some(WorkspaceMode::Shared),
            }
        );
    }

    /// ADR-0059 D3: `repos` があるタスク（コードを触る想定）は、`mode` 省略でも exit 65 で格下げせず
    /// 従来どおり失敗する。
    #[tokio::test]
    async fn exit_65_with_repos_present_does_not_downgrade() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let stub = write_stub_ssh(tmp.path(), 65);
        let repo = task_core::RepoRef {
            repo_id: task_core::RepoId::new(),
            name: "agent-platform".into(),
        };
        let task = remote_task(tmp.path(), None, vec![repo]);
        store.insert(&task).unwrap();

        let mut settings = SshSettings::new("pegasus", "pegasus", PathBuf::from("/work/proj"));
        settings.sync = SyncMode::Worktree;
        settings.task_id = task.id.to_string();
        settings.ssh_command = vec![stub.to_string_lossy().into_owned()];

        let outcome =
            run_worker_for_test(store.clone(), task.id, tmp.path().join("mirror"), settings).await;
        assert!(outcome.is_err());
        let events = store.events_for(task.id).unwrap();
        assert!(
            !events
                .iter()
                .any(|(_, e)| matches!(e, Event::WorkspaceModeDowngraded { .. })),
            "{events:?}"
        );
        let stored = store.get(task.id).unwrap().expect("task exists");
        assert_eq!(stored.workspace.remote_mode(), WorkspaceMode::Worktree);
    }

    /// ADR-0059 D3: 明示的に `mode = "worktree"` を選んだタスクは、`repos` が無くても exit 65 で
    /// 格下げしない（利用者の意図を尊重する）。
    #[tokio::test]
    async fn exit_65_with_explicit_worktree_mode_does_not_downgrade() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let stub = write_stub_ssh(tmp.path(), 65);
        let task = remote_task(tmp.path(), Some(WorkspaceMode::Worktree), Vec::new());
        store.insert(&task).unwrap();

        let mut settings = SshSettings::new("pegasus", "pegasus", PathBuf::from("/work/proj"));
        settings.sync = SyncMode::Worktree;
        settings.task_id = task.id.to_string();
        settings.ssh_command = vec![stub.to_string_lossy().into_owned()];

        let outcome =
            run_worker_for_test(store.clone(), task.id, tmp.path().join("mirror"), settings).await;
        assert!(outcome.is_err());
        let events = store.events_for(task.id).unwrap();
        assert!(
            !events
                .iter()
                .any(|(_, e)| matches!(e, Event::WorkspaceModeDowngraded { .. })),
            "{events:?}"
        );
    }

    // ---- ADR-0053 D3: `refresh_cluster_tunnels` の状態機械（偽の ssh/probe で完全に決定的） ----

    /// down → 鍵認証 ok → forward up: master が死んでいても `cluster_connector` が繋ぎ直し、
    /// 続けて forward が(再)確立されて `Up` イベントが立つ。
    #[tokio::test]
    async fn tunnel_down_then_key_auth_ok_brings_the_forward_up() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "pegasus".into(),
            cluster_spec_with_forward(
                "pegasus",
                "pegasus",
                "totp",
                "127.0.0.1:19001",
                "bnode150:19001",
            ),
        );
        // master は死んでいる（cluster_connected に何も入っていない）。
        d.set_cluster_connector(Arc::new(|_id: &str, _host: &str| Ok(())));
        let ensure_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let forward_present = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ensure_calls_hook = ensure_calls.clone();
        let forward_present_hook = forward_present.clone();
        d.set_tunnel_forward_ensurer(Arc::new(
            move |_host: &str, _listen: &str, _target: &str| {
                ensure_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                forward_present_hook.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        ));
        // Phase 85: listener と target の健康は別のフック。この試験では両方を同じフラグに束ねる
        // （ensure が listener を立て、立った listener はそのまま target も健全とみなす）。
        let forward_present_listener = forward_present.clone();
        d.set_tunnel_listener_probe(Arc::new(move |_listen: &str| {
            forward_present_listener.load(std::sync::atomic::Ordering::SeqCst)
        }));
        let forward_present_probe = forward_present.clone();
        d.set_tunnel_probe(Arc::new(move |_listen: &str| {
            if forward_present_probe.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(())
            } else {
                Err("GET /models failed: could not connect".to_string())
            }
        }));

        d.refresh_cluster_tunnels();

        assert_eq!(d.cluster_connected.get("pegasus"), Some(&true));
        assert!(d.clusters_needing_login().is_empty());
        assert_eq!(ensure_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(d.tunnel_reachable("pegasus", "127.0.0.1:19001"));
        let events = d.take_tunnel_events();
        assert!(
            events.iter().any(|e| e.cluster == "pegasus"
                && e.listen == "127.0.0.1:19001"
                && e.kind == TunnelEventKind::Up),
            "{events:?}"
        );
    }

    /// down → 鍵認証も失敗 → login_needed が立つ。同じ outage の間は 2 回目の tick で再度立てない
    /// （イベントも報告も 1 回だけ）。
    #[tokio::test]
    async fn tunnel_down_and_key_auth_fails_marks_login_needed_once() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "pegasus".into(),
            cluster_spec_with_forward(
                "pegasus",
                "pegasus",
                "totp",
                "127.0.0.1:19002",
                "bnode150:19002",
            ),
        );
        d.set_cluster_connector(Arc::new(|_id: &str, _host: &str| {
            Err("permission denied (keyboard-interactive)".to_string())
        }));

        d.refresh_cluster_tunnels();
        assert_eq!(d.clusters_needing_login(), vec!["pegasus".to_string()]);
        let first_events = d.take_tunnel_events();
        assert_eq!(
            first_events
                .iter()
                .filter(|e| e.kind == TunnelEventKind::LoginNeeded)
                .count(),
            1,
            "{first_events:?}"
        );

        // 2 回目の tick（間引きを避けるため、間隔を過ぎさせる）。まだ同じ outage の間なので、
        // login_needed の再検出（新しいイベント）は起きない。
        d.last_cluster_tunnel_refresh =
            Some(Instant::now() - CLUSTER_LIVENESS_INTERVAL - Duration::from_millis(1));
        d.refresh_cluster_tunnels();
        assert_eq!(d.clusters_needing_login(), vec!["pegasus".to_string()]);
        let second_events = d.take_tunnel_events();
        assert!(
            second_events
                .iter()
                .all(|e| e.kind != TunnelEventKind::LoginNeeded),
            "login_needed must not fire twice for the same outage: {second_events:?}"
        );
    }

    /// listener が消えている（`-O forward` の手元の待ち受けが無い）が master は生きている:
    /// `tunnel_forward_ensurer` で張り直しを試みる（呼ばれたことを確認する。「listener missing →
    /// re-add」。Phase 85: listener の有無だけで re-add を決める。target の健康は別）。
    #[tokio::test]
    async fn tunnel_listener_absent_is_re_added() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "pegasus".into(),
            cluster_spec_with_forward(
                "pegasus",
                "pegasus",
                "totp",
                "127.0.0.1:19003",
                "bnode150:19003",
            ),
        );
        d.cluster_connected.insert("pegasus".into(), true);
        let ensure_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listener_present = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ensure_calls_hook = ensure_calls.clone();
        let listener_present_hook = listener_present.clone();
        d.set_tunnel_forward_ensurer(Arc::new(
            move |_host: &str, _listen: &str, _target: &str| {
                ensure_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                listener_present_hook.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        ));
        let listener_present_probe = listener_present.clone();
        d.set_tunnel_listener_probe(Arc::new(move |_listen: &str| {
            listener_present_probe.load(std::sync::atomic::Ordering::SeqCst)
        }));
        // listener が有る間は target も健全（この試験は listener の有無だけを見たいので単純化する）。
        d.set_tunnel_probe(Arc::new(|_listen: &str| Ok(())));

        // 1 回目: 最初から listener が有る。ensure は呼ばれない。
        d.refresh_cluster_tunnels();
        assert_eq!(ensure_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(d.tunnel_reachable("pegasus", "127.0.0.1:19003"));

        // listener が消える（誰かが master 側を再起動した、等）。
        listener_present.store(false, std::sync::atomic::Ordering::SeqCst);
        d.last_cluster_tunnel_refresh =
            Some(Instant::now() - CLUSTER_LIVENESS_INTERVAL - Duration::from_millis(1));
        d.refresh_cluster_tunnels();
        assert_eq!(
            ensure_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a missing listener must be re-added"
        );
        assert!(d.tunnel_reachable("pegasus", "127.0.0.1:19003"));
    }

    /// ADR-0053 Phase 85 の本旨: listener は有る（`-O forward` は張れている）が target
    /// （先方の vLLM 等）の `/v1/models` probe が失敗する。**re-add はしない**（listener が有るので
    /// `tunnel_forward_ensurer` を呼ぶのは無意味）。状態は `TargetUnreachable` として観測され、
    /// `up = false` だが `listener = true` / `target_healthy = false` / `last_error` が立つ。
    ///
    /// 本番観測（2026-09-21）: `-O forward` は張れているのに bnode150 が応答せず、旧実装（listener と
    /// target を区別しない）は「届かない＝forward が無い」と誤認し、毎 tick `-O forward` を打ち直して
    /// いた（5 分で 37 回）。この試験はその回帰を防ぐ。
    #[tokio::test]
    async fn tunnel_listener_present_target_unhealthy_does_not_re_add() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "pegasus".into(),
            cluster_spec_with_forward(
                "pegasus",
                "pegasus",
                "totp",
                "127.0.0.1:19004",
                "bnode150:19004",
            ),
        );
        d.cluster_connected.insert("pegasus".into(), true);
        d.set_tunnel_listener_probe(Arc::new(|_listen: &str| true));
        let ensure_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ensure_calls_hook = ensure_calls.clone();
        d.set_tunnel_forward_ensurer(Arc::new(
            move |_host: &str, _listen: &str, _target: &str| {
                ensure_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        ));
        d.set_tunnel_probe(Arc::new(|_listen: &str| {
            Err("GET /models timed out after 2s".to_string())
        }));

        d.refresh_cluster_tunnels();

        assert_eq!(
            ensure_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a present listener must not be re-added just because the target is unhealthy"
        );
        assert!(!d.tunnel_reachable("pegasus", "127.0.0.1:19004"));
        assert!(d.tunnel_listener_present("pegasus", "127.0.0.1:19004"));
        assert!(!d.tunnel_target_healthy("pegasus", "127.0.0.1:19004"));
        assert_eq!(
            d.tunnel_last_error("pegasus", "127.0.0.1:19004"),
            Some(
                "target bnode150:19004 (as seen from pegasus) did not answer /v1/models through \
                 the forward: GET /models timed out after 2s"
                    .to_string()
            )
        );
        let events = d.take_tunnel_events();
        assert!(
            events.iter().any(|e| e.cluster == "pegasus"
                && e.listen == "127.0.0.1:19004"
                && e.kind == TunnelEventKind::TargetUnreachable),
            "{events:?}"
        );
    }

    /// ADR-0066 D3（Phase 110b）: この forward を初めて観測するときだけ、`refresh_cluster_tunnels` は
    /// 同期に 1 回 probe して種を蒔く（listener が有れば必ず 1 回は確かめる、という Phase 85 までの
    /// 前提を保つ）。**2 回目以降はもう同期に probe しない** — 専用スレッドが裏で行うので、probe 自体が
    /// 遅くても（本番観測: `/v1/models` が 2 秒級）tick は待たない。これが Phase 110b の本旨（`slow tick
    /// phases`〈`tunnel_ms`≈2000〉が 1 時間に 118 件出ていた不具合の直し）。
    #[tokio::test]
    async fn target_probe_seeds_synchronously_once_then_the_tick_stops_waiting_on_it() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "pegasus".into(),
            cluster_spec_with_forward(
                "pegasus",
                "pegasus",
                "totp",
                "127.0.0.1:19006",
                "bnode150:19006",
            ),
        );
        d.cluster_connected.insert("pegasus".into(), true);
        d.set_tunnel_listener_probe(Arc::new(|_listen: &str| true));
        d.set_tunnel_forward_ensurer(Arc::new(|_host: &str, _listen: &str, _target: &str| Ok(())));
        let probe_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe_calls_hook = probe_calls.clone();
        d.set_tunnel_probe(Arc::new(move |_listen: &str| {
            probe_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // 本番観測（無応答の Qwen 先方への `/v1/models`）の遅さを模す。
            std::thread::sleep(Duration::from_millis(150));
            Ok(())
        }));

        let first_started = Instant::now();
        d.refresh_cluster_tunnels();
        assert!(
            first_started.elapsed() >= Duration::from_millis(150),
            "the first observation seeds synchronously"
        );
        assert_eq!(probe_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(d.tunnel_reachable("pegasus", "127.0.0.1:19006"));

        // 次の tick（`CLUSTER_LIVENESS_INTERVAL` を過ぎさせる）。probe はもう同期では呼ばれないので、
        // probe が遅くても tick は速い。
        d.last_cluster_tunnel_refresh =
            Some(Instant::now() - CLUSTER_LIVENESS_INTERVAL - Duration::from_millis(1));
        let second_started = Instant::now();
        d.refresh_cluster_tunnels();
        assert!(
            second_started.elapsed() < Duration::from_millis(100),
            "the tick must not wait on the slow target probe once it has been seeded: {:?}",
            second_started.elapsed()
        );
    }

    /// ADR-0066 D3: 失敗が続くと probe の間隔が伸び、1 回成功すれば最短間隔に戻る（純粋関数。
    /// スレッドも時刻も使わない）。
    #[test]
    fn next_probe_interval_secs_grows_on_failure_and_resets_on_success() {
        assert_eq!(next_probe_interval_secs(30, 30, false), 60);
        assert_eq!(next_probe_interval_secs(60, 30, false), 120);
        assert_eq!(next_probe_interval_secs(120, 30, false), 240);
        assert_eq!(next_probe_interval_secs(240, 30, false), 480);
        assert_eq!(
            next_probe_interval_secs(480, 30, false),
            600,
            "capped at the max"
        );
        assert_eq!(
            next_probe_interval_secs(600, 30, false),
            600,
            "stays at the max"
        );
        assert_eq!(
            next_probe_interval_secs(600, 30, true),
            30,
            "one success resets to the minimum"
        );
        // 設定変更で `probe_interval_secs`（min）が現在値より大きくなっても、min を下限にする。
        assert_eq!(next_probe_interval_secs(10, 30, false), 60);
    }

    /// ADR-0053 Phase 85: 同じフェーズ（ここでは `TargetUnreachable`）が続く間、状態遷移のイベント
    /// （Console に出る 1 行）は 1 回しか積まない（毎回 probe しても、フェーズが変わらなければスパムしない）。
    #[tokio::test]
    async fn tunnel_target_unreachable_emits_the_event_once_across_many_ticks() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "pegasus".into(),
            cluster_spec_with_forward(
                "pegasus",
                "pegasus",
                "totp",
                "127.0.0.1:19007",
                "bnode150:19007",
            ),
        );
        d.cluster_connected.insert("pegasus".into(), true);
        d.set_tunnel_listener_probe(Arc::new(|_listen: &str| true));
        d.set_tunnel_forward_ensurer(Arc::new(|_host: &str, _listen: &str, _target: &str| Ok(())));
        d.set_tunnel_probe(Arc::new(|_listen: &str| {
            Err("GET /models timed out after 2s".to_string())
        }));

        for _ in 0..5 {
            // `CLUSTER_LIVENESS_INTERVAL` の間引きを毎回越えさせる。ADR-0066 D3: target probe 自体は
            // 最初の 1 回だけ同期で種を蒔き、以後は `tunnel_probe_state` の既存の観測を読むだけ
            // （専用スレッドがこのテストの短い実行時間の中で probe をやり直すことは通常無い）。
            // それでもイベントが 1 回しか積まれないことを確かめる。
            d.last_cluster_tunnel_refresh =
                Some(Instant::now() - CLUSTER_LIVENESS_INTERVAL - Duration::from_millis(1));
            d.refresh_cluster_tunnels();
        }
        let events = d.take_tunnel_events();
        let target_unreachable: Vec<_> = events
            .iter()
            .filter(|e| e.kind == TunnelEventKind::TargetUnreachable)
            .collect();
        assert_eq!(
            target_unreachable.len(),
            1,
            "a steady TargetUnreachable state must log the transition once, not every tick: {events:?}"
        );
    }

    /// ADR-0041 D5 / Phase 66c（実機 2026-09-21）: `--mode verify` の celeris（`set_eligible_tasks` で
    /// 「`smoke` の煙試験だけ」に絞られたインスタンス）は「migrations、API と煙試験だけ。他の裏方の仕事は
    /// 無い」はずなのに、`[[clusters.forwards]]` を持つクラスタが設定されていると
    /// `refresh_cluster_tunnels` が forward の(再)確立と `cluster_login_needed` の報告書き込みを行い、
    /// staging と本番のコピーで `reports` の件数がずれて `verify.sh` check 2 が落ちた
    /// （`reports(snapshot=135 staging=136)`）。`set_eligible_tasks` を呼んだ（＝ verify）インスタンスは
    /// `refresh_cluster_tunnels` を丸ごと素通りし、フック（`cluster_connector` / `tunnel_forward_ensurer`
    /// / `tunnel_probe`）を一切呼ばず、報告も書かないことを確かめる。
    #[tokio::test]
    async fn verify_mode_does_not_refresh_cluster_tunnels_or_write_a_report() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.clusters.insert(
            "pegasus".into(),
            cluster_spec_with_forward(
                "pegasus",
                "pegasus",
                "totp",
                "127.0.0.1:19005",
                "bnode150:19005",
            ),
        );
        // master は死んでいる（`cluster_connected` に何も入っていない）ので、verify でなければ
        // `cluster_connector` → 失敗 → `cluster_login_needed` の報告書き込みに至るはずの状態。
        let connector_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connector_calls_hook = connector_calls.clone();
        d.set_cluster_connector(Arc::new(move |_id: &str, _host: &str| {
            connector_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("permission denied (keyboard-interactive)".to_string())
        }));
        let ensure_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ensure_calls_hook = ensure_calls.clone();
        d.set_tunnel_forward_ensurer(Arc::new(
            move |_host: &str, _listen: &str, _target: &str| {
                ensure_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        ));
        let probe_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe_calls_hook = probe_calls.clone();
        d.set_tunnel_probe(Arc::new(move |_listen: &str| {
            probe_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));
        let listener_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listener_calls_hook = listener_calls.clone();
        d.set_tunnel_listener_probe(Arc::new(move |_listen: &str| {
            listener_calls_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        }));

        let reports_before = store
            .report_list(&task_core::ReportFilter::default())
            .unwrap()
            .len();

        // ADR-0041 D5: `--mode verify` の目印（celeris は verify のときだけこれを呼ぶ）。
        d.set_eligible_tasks(Arc::new(|task: &Task| {
            task.genre.as_deref() == Some("smoke")
        }));

        d.refresh_cluster_liveness();
        d.refresh_cluster_tunnels();

        assert_eq!(
            connector_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "verify mode must not touch the cluster connector"
        );
        assert_eq!(
            ensure_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "verify mode must not (re-)establish forwards"
        );
        assert_eq!(
            probe_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "verify mode must not probe forwards"
        );
        assert_eq!(
            listener_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "verify mode must not probe forward listeners"
        );
        assert!(d.clusters_needing_login().is_empty());
        assert!(d.take_tunnel_events().is_empty());
        assert_eq!(d.cluster_connected.get("pegasus"), None);

        let reports_after = store
            .report_list(&task_core::ReportFilter::default())
            .unwrap()
            .len();
        assert_eq!(
            reports_after, reports_before,
            "verify mode must not write a cluster_login_needed report"
        );
    }

    /// ADR-0032 D3: `auth = "publickey"` かつ接続フックが刺さっていれば、未接続のクラスタは cooldown にする前に
    /// 1 回だけ自動接続を試みる。成功したら `cluster_connected` が true になり、そのまま dispatch が続く
    /// （`ClusterUnavailable` は残らない）。
    #[tokio::test]
    async fn publickey_cluster_auto_connects_and_dispatch_continues_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.workspace = WorkspaceSpec::Remote {
            cluster: "auto".into(),
            path: PathBuf::from("/remote/project"),
            mode: None,
        };
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.clusters.insert(
            "auto".into(),
            cluster_spec_with_auth(
                "auto",
                "celeris-no-such-host-for-tests-auto-ok",
                "publickey",
            ),
        );
        let calls: Arc<StdMutex<Vec<(String, String)>>> = Arc::new(StdMutex::new(Vec::new()));
        let calls_for_hook = calls.clone();
        d.set_cluster_connector(Arc::new(move |id: &str, host: &str| {
            calls_for_hook
                .lock()
                .unwrap()
                .push((id.to_string(), host.to_string()));
            Ok(())
        }));

        let report = d.tick().unwrap();
        assert_eq!(report.dispatched, 1, "{report:?}");
        assert_eq!(
            *calls.lock().unwrap(),
            vec![(
                "auto".to_string(),
                "celeris-no-such-host-for-tests-auto-ok".to_string()
            )]
        );
        assert_eq!(d.cluster_connected.get("auto"), Some(&true));
        assert!(
            !d.cluster_cooldown.contains_key("auto"),
            "success does not cool the cluster down"
        );
        let events = store.events_for(task.id).unwrap();
        assert!(
            !events
                .iter()
                .any(|(_, e)| matches!(e, Event::ClusterUnavailable { .. })),
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
        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.workspace = WorkspaceSpec::Remote {
            cluster: "auto".into(),
            path: PathBuf::from("/remote/project"),
            mode: None,
        };
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.clusters.insert(
            "auto".into(),
            cluster_spec_with_auth(
                "auto",
                "celeris-no-such-host-for-tests-auto-fail",
                "publickey",
            ),
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
            let mut task = new_task(
                dir.path(),
                Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
                0,
            );
            task.workspace = WorkspaceSpec::Remote {
                cluster: "auto".into(),
                path: PathBuf::from("/remote/project"),
                mode: None,
            };
            store.insert(&task).unwrap();
            let adapter = Arc::new(InstantAdapter {
                terminal: Terminal::Done {
                    summary: "ok".into(),
                    evidence: vec![],
                    usage: None,
                },
                delay: Duration::ZERO,
            });
            let mut d = dispatcher(store.clone(), adapter, 1);
            d.config.clusters.insert(
                "auto".into(),
                cluster_spec_with_auth("auto", "celeris-no-such-host-for-tests-not-auto", auth),
            );
            let call_count = Arc::new(StdMutex::new(0u32));
            let call_count_for_hook = call_count.clone();
            d.set_cluster_connector(Arc::new(move |_id: &str, _host: &str| {
                *call_count_for_hook.lock().unwrap() += 1;
                Ok(())
            }));

            let report = d.tick().unwrap();
            assert_eq!(report.dispatched, 0, "{auth}: {report:?}");
            assert_eq!(
                *call_count.lock().unwrap(),
                0,
                "{auth}: hook must not run for auth={auth:?}"
            );
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
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "fern03".into(),
            cluster_spec_with_auth("fern03", "celeris-no-such-host-for-tests-live", "publickey"),
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
        let live = snap
            .clusters
            .iter()
            .find(|c| c.id == "fern03")
            .expect("fern03 in snapshot");
        assert_eq!(
            (live.auth.as_str(), live.connect_pending),
            ("publickey", false)
        );

        d.set_cluster_connect_pending("fern03", true);
        d.tick().unwrap();
        let snap = rx.borrow().clone().expect("snapshot published");
        let live = snap
            .clusters
            .iter()
            .find(|c| c.id == "fern03")
            .expect("fern03 in snapshot");
        assert!(live.connect_pending, "connect_pending set");

        d.set_cluster_connect_pending("fern03", false);
        d.tick().unwrap();
        let snap = rx.borrow().clone().expect("snapshot published");
        let live = snap
            .clusters
            .iter()
            .find(|c| c.id == "fern03")
            .expect("fern03 in snapshot");
        assert!(!live.connect_pending, "connect_pending cleared");
    }

    /// ADR-0018 実装メモ M1: 接続が戻っていれば、その tick で cooldown が解ける。`celeris-localhost` への多重接続が無い環境では skip。
    #[tokio::test]
    async fn cluster_cooldown_is_cleared_once_the_control_master_is_back() {
        if !control_master_alive_blocking(&["ssh".to_string()], "celeris-localhost") {
            eprintln!("skip: celeris-localhost への多重接続が無い");
            return;
        }
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store, adapter, 1);
        d.config.clusters.insert(
            "local".into(),
            ClusterSpec {
                id: "local".into(),
                host: "celeris-localhost".into(),
                concurrency: 1,
                sync: SyncMode::Rsync,
                delete_on_push: false,
                setup: vec![],
                env: vec![],
                rsync_excludes: vec![],
                worktree: Default::default(),
                auth: "manual".into(),
                forwards: vec![],
                work_dir: None,
                keepalive_secs: 0,
                liveness_probe_secs: 0,
            },
        );
        d.cluster_cooldown
            .insert("local".into(), Instant::now() + Duration::from_secs(3600));
        d.refresh_cluster_liveness();
        assert_eq!(d.cluster_connected.get("local"), Some(&true));
        assert!(
            !d.cluster_cooldown.contains_key("local"),
            "cooldown is cleared when the connection is back"
        );

        // ADR-0023 D1: 5 秒以内の 2 回目は `ssh -O check` を回さず、前回の結果をそのまま使う。
        d.cluster_connected.insert("local".into(), false);
        d.refresh_cluster_liveness();
        assert_eq!(
            d.cluster_connected.get("local"),
            Some(&false),
            "間引いた回は確認し直さない"
        );
        // 前回の確認を古くすると、次の呼び出しで確認し直す。
        d.last_cluster_liveness =
            Some(Instant::now() - CLUSTER_LIVENESS_INTERVAL - Duration::from_millis(1));
        d.refresh_cluster_liveness();
        assert_eq!(
            d.cluster_connected.get("local"),
            Some(&true),
            "間隔を過ぎたら確認し直す"
        );
    }

    /// ADR-0013 D4: tick の最後にメモリ上のスナップショットが `watch` に送られる（実行中の run、プロバイダの使用数、cooldown）。
    #[tokio::test]
    async fn tick_publishes_daemon_snapshot_to_watch() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
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
                credential_refs: Default::default(),
                tier_models: Default::default(),
                account_id: None,
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
        assert!(
            rx.borrow().is_none(),
            "nothing is published before the first tick"
        );

        d.tick().unwrap();
        let snap = rx.borrow().clone().expect("snapshot after the first tick");
        assert_eq!(
            (snap.ticks, snap.instance_id.as_str(), snap.tick_ms),
            (1, "inst-1", 50)
        );
        assert_eq!(snap.pid, std::process::id());
        assert_eq!(snap.in_flight.len(), 1);
        assert_eq!(snap.in_flight[0].task_id, task.id);
        assert_eq!(snap.in_flight[0].kind, InFlightKind::Worker);
        assert_eq!(snap.in_flight[0].provider, "p1");
        assert_eq!(snap.providers[0].in_use, 1);
        assert!(snap.cooldowns.is_empty());

        d.policy.report(
            "p1".into(),
            &ProviderOutcome::Throttled {
                retry_after: Duration::from_secs(60),
            },
        );
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle);
        let snap = rx.borrow().clone().unwrap();
        assert!(snap.ticks > 1);
        assert!(snap.in_flight.is_empty());
        assert_eq!(snap.providers[0].in_use, 0);
        assert_eq!(snap.cooldowns.len(), 1);
        assert_eq!(
            (
                snap.cooldowns[0].provider.as_str(),
                snap.cooldowns[0].reason.as_str()
            ),
            ("p1", "throttled")
        );
        assert!(
            snap.cooldowns[0].until > snap.last_tick_at,
            "until is in the future"
        );

        // ADR-0022 D2: 疎通確認の結果はスナップショットにだけ載る（DB には書かない）。
        assert!(snap.providers[0].last_check.is_none(), "確認する前は空");
        d.set_provider_check(
            "p1",
            ProviderCheckView {
                at: "2026-09-16T02:00:00Z".into(),
                result: "ok".into(),
                detail: None,
            },
        );
        d.tick().unwrap();
        let snap = rx.borrow().clone().unwrap();
        assert_eq!(
            snap.providers[0].last_check,
            Some(ProviderCheckView {
                at: "2026-09-16T02:00:00Z".into(),
                result: "ok".into(),
                detail: None
            })
        );

        // reload でプロバイダ表を差し替えても、残った id の記録は保つ。消えた id の記録は落とす。
        d.set_snapshot_providers(vec![
            ProviderLive {
                credential_refs: Default::default(),
                tier_models: Default::default(),
                account_id: None,
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
                credential_refs: Default::default(),
                tier_models: Default::default(),
                account_id: None,
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
        assert_eq!(
            snap.providers[0]
                .last_check
                .as_ref()
                .map(|c| c.result.as_str()),
            Some("ok"),
            "p1 の記録は残る"
        );
        assert!(
            snap.providers[1].last_check.is_none(),
            "p2 はまだ確認していない"
        );

        d.set_snapshot_providers(vec![ProviderLive {
            credential_refs: Default::default(),
            tier_models: Default::default(),
            account_id: None,
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
        assert!(
            snap.providers[0].last_check.is_none(),
            "消えた p1 の記録は残さない"
        );
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
                    terminal: Terminal::Done {
                        summary: summary.into(),
                        evidence: vec![],
                        usage: None,
                    },
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
                self.aggregate_children
                    .store(req.context.children.len(), Ordering::SeqCst);
                if self.write_summary {
                    std::fs::create_dir_all(&req.artifacts_dir).unwrap();
                    std::fs::write(req.artifacts_dir.join("summary.md"), "# summary\n").unwrap();
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
            acceptance: vec![Criterion {
                text: "c".into(),
                check: Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            }],
            role: Some("implementer".into()),
            genre: None,
            depends_on: deps,
            tier: None,
            assignee: None,
            workspace: None,
        }
    }

    fn roles() -> Vec<RoleSpec> {
        vec![
            RoleSpec {
                id: "lead".into(),
                instructions: Some("You lead; delegate implementation.".into()),
                ..RoleSpec::default()
            },
            RoleSpec {
                id: "implementer".into(),
                tier: Some(Tier::Cheap),
                ..RoleSpec::default()
            },
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
        let mut parent = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            1,
        );
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
                proposal(
                    "self",
                    vec![task_core::DelegateDep::Id(parent.id.to_string())],
                ),
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
        assert_eq!(
            p.status,
            Status::Done,
            "{:?}",
            store.events_for(parent.id).unwrap()
        );
        assert_eq!(p.attempts, 0, "aggregate does not consume attempts");
        let children = store.children(parent.id).unwrap();
        assert_eq!(
            children.len(),
            2,
            "only the two valid proposals were inserted"
        );
        assert_eq!(children[0].title, "a");
        assert_eq!(children[1].title, "b");
        assert_eq!(children[1].depends_on, vec![children[0].id]);
        assert_eq!(children[0].role.as_deref(), Some("implementer"));
        assert_eq!(
            children[0].worker_hint.tier,
            Tier::Cheap,
            "role default applied to the child"
        );
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
        assert!(
            msgs.iter()
                .any(|m| m.starts_with("delegate rejected: tasks[2]") && m.contains("title")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.starts_with("delegate rejected: tasks[3]")
                    && m.contains("delegating task itself")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.starts_with("waiting for ") && m.contains("delegated child task")),
            "{msgs:?}"
        );
        assert_eq!(
            transition_reasons(&store, parent.id),
            vec![
                "dispatch",
                "worker_done",
                "aggregate",
                "dispatch",
                "worker_done",
                "review_pass"
            ]
        );
        // 親の run は 2 回（最初 + 集約）。役割名が WorkerStarted に残り、指示文が RunContext に載る。
        let started: Vec<Option<String>> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerStarted {
                    role: None,
                    task_role,
                    ..
                } => Some(task_role.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            started,
            vec![Some("lead".to_string()), Some("lead".to_string())]
        );
        let role = adapter
            .seen_role
            .lock()
            .unwrap()
            .clone()
            .expect("role context");
        assert_eq!(role.id, "lead");
        assert_eq!(role.instructions, "You lead; delegate implementation.");
        assert_eq!(
            adapter.aggregate_children.load(Ordering::SeqCst),
            2,
            "aggregate run saw both children"
        );
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
                    terminal: Terminal::Done {
                        summary: summary.into(),
                        evidence: vec![],
                        usage: None,
                    },
                    exit_code: Some(0),
                })
            };
            if req.task.role.as_deref() == Some("literature-reader") {
                return done("child");
            }
            *self.seen_available_genres.lock().unwrap() =
                Some(req.context.available_genres.clone());
            sink.delegate(std::slice::from_ref(&self.proposal));
            done("delegated")
        }
    }

    #[tokio::test]
    async fn delegate_can_select_a_different_genre_and_available_genres_reach_the_prompt_context() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut parent = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
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
            RoleSpec {
                id: "lead".into(),
                ..RoleSpec::default()
            },
            RoleSpec {
                id: "literature-reader".into(),
                ..RoleSpec::default()
            },
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
        assert_eq!(
            p.status,
            Status::Done,
            "{:?}",
            store.events_for(parent.id).unwrap()
        );
        let children = store.children(parent.id).unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].role.as_deref(), Some("literature-reader"));
        assert_eq!(
            children[0].genre.as_deref(),
            Some("literature"),
            "explicit genre wins"
        );

        let available = adapter
            .seen_available_genres
            .lock()
            .unwrap()
            .clone()
            .expect("available_genres seen");
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
        let extras = d.run_extras(&plan, None, None, "claude-code").unwrap();
        let ids: Vec<&str> = extras
            .available_genres
            .iter()
            .map(|g| g.id.as_str())
            .collect();
        assert_eq!(ids, vec!["coding"]);

        // 分野が無い設定では空のまま。
        d.config.genres = Vec::new();
        let extras = d.run_extras(&plan, None, None, "claude-code").unwrap();
        assert!(extras.available_genres.is_empty());
    }

    /// Phase 38（ADR-0028 追記。実機のレビュー不合格から）: ディスパッチャの配線 2 つ —
    /// (1) `available_genres[].harness` が `default_role` の役割のアダプタから決定的に埋まる、
    /// (2) 計画がハーネス系の担当に別名のファイルを要求していたら、子を作る前にその条件を落として
    ///     `objective` に本当の成果物の名前を注記する（LLM は呼ばない）。
    #[test]
    fn harness_genres_are_marked_and_the_plan_is_fixed_before_children_are_created() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let plan_parent = plan_task(dir.path(), 0);
        store.insert(&plan_parent).unwrap();
        let adapter: Arc<dyn WorkerAdapter> = Arc::new(FileAdapter {
            plan_json: VALID_PLAN.into(),
            review_json: r#"{"verdicts":[]}"#.into(),
            delay: Duration::from_millis(0),
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.config.roles = vec![task_core::RoleSpec {
            id: "literature-reader".into(),
            adapter: Some("paperqa".into()),
            ..task_core::RoleSpec::default()
        }];
        d.config.genres = vec![task_core::GenreSpec {
            id: "literature".into(),
            description: "関連研究の調査".into(),
            output_artifacts: vec!["answer.md: 引用付きの答え".into(), "papers.json".into()],
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-reader".into()],
            ..task_core::GenreSpec::default()
        }];

        let extras = d
            .run_extras(&plan_parent, None, None, "claude-code")
            .unwrap();
        assert_eq!(
            extras.available_genres[0].harness.as_deref(),
            Some("paperqa")
        );
        assert!(extras.available_genres[0].is_harness());

        let mut plan = task_core::PlanOutput {
            tasks: vec![task_core::NewTask {
                harness: None,
                mode: Default::default(),
                skills: Vec::new(),
                repos: Vec::new(),
                title: "候補テーマの抽出".into(),
                objective: "候補テーマを candidates.json にまとめよ".into(),
                acceptance: vec![Criterion {
                    text: "candidates.json に候補テーマがある".into(),
                    check: Check::ArtifactExists {
                        name: "candidates.json".into(),
                    },
                }],
                depends_on: vec![],
                kind: task_core::NewTaskKind::Execute,
                tier: None,
                role: None,
                genre: Some("literature".into()),
                assignee: None,
                workspace: None,
                category: None,
                labels: Vec::new(),
                partial_ok: None,
            }],
        };
        d.fix_plan_for_harness(&plan_parent, &mut plan, &[]);
        assert_eq!(
            plan.tasks[0].acceptance[0].check,
            Check::Reviewer,
            "落とすと 0 件になるので内容はレビュアーが見る"
        );
        assert!(
            plan.tasks[0].objective.ends_with(
                "（注: この担当の成果物は answer.md / papers.json に固定。要求した内容は answer.md の中で述べる）"
            ),
            "{}",
            plan.tasks[0].objective
        );
    }

    /// 受け入れ 3: `aggregate = false` の親は子が終わるまで reviewing のまま、終わったら run を増やさず done。
    #[tokio::test]
    async fn non_aggregate_parent_stays_reviewing_until_children_finish_then_completes() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let parent = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
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
            let child_running = store
                .children(parent.id)
                .unwrap()
                .iter()
                .any(|c| c.status == Status::Running);
            if p.status == Status::Reviewing
                && child_running
                && d.awaiting_children.contains_key(&parent.id)
            {
                observed_waiting = true;
                break;
            }
            if r.idle {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            observed_waiting,
            "parent should be reviewing while its delegated child runs"
        );
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
        assert_eq!(
            transition_reasons(&store, parent.id),
            vec!["dispatch", "worker_done", "review_pass"]
        );
        let events = store.events_for(parent.id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|(_, e)| matches!(e, Event::WorkerStarted { role: None, .. }))
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .any(|(_, e)| matches!(e, Event::Delegated { .. }))
        );
    }

    /// 受け入れ 2: 上限（1 run の件数・木の深さ・木の run 数）を超える提案は拒否され、理由が WorkerProgress に残り、親は失敗しない。
    #[tokio::test]
    async fn delegation_limits_reject_with_reasons_and_do_not_fail_the_run() {
        async fn run_with(
            limits: DelegationLimits,
            proposals: Vec<DelegateTask>,
            depth: u32,
        ) -> (Vec<String>, usize, Status) {
            let dir = tempfile::tempdir().unwrap();
            let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
            // depth 個の祖先の下に親を置く（根 = 深さ 1）。
            let mut ancestor: Option<TaskId> = None;
            for _ in 1..depth {
                let mut a = new_task(
                    dir.path(),
                    Check::Command {
                        cmd: "true".into(),
                        expect_exit: 0,
                    },
                    0,
                );
                a.parent_id = ancestor;
                a.status = Status::Done;
                store.insert(&a).unwrap();
                ancestor = Some(a.id);
            }
            let mut parent = new_task(
                dir.path(),
                Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
                0,
            );
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
            (
                progress_msgs(&store, parent.id),
                store.children(parent.id).unwrap().len(),
                p.status,
            )
        }

        // 1 run の件数: 2 件のうち 1 件だけ。
        let (msgs, n, status) = run_with(
            DelegationLimits {
                max_delegate_per_run: 1,
                ..DelegationLimits::default()
            },
            vec![proposal("a", vec![]), proposal("b", vec![])],
            1,
        )
        .await;
        assert_eq!(n, 1, "{msgs:?}");
        assert_eq!(status, Status::Done);
        assert!(
            msgs.iter()
                .any(|m| m.contains("delegate rejected: tasks[1]")
                    && m.contains("per-run delegation limit (1)")),
            "{msgs:?}"
        );

        // 木の深さ: 深さ 2 の親は max_tree_depth = 2 で子を作れない。
        let (msgs, n, status) = run_with(
            DelegationLimits {
                max_tree_depth: 2,
                ..DelegationLimits::default()
            },
            vec![proposal("a", vec![])],
            2,
        )
        .await;
        assert_eq!(n, 0, "{msgs:?}");
        assert_eq!(status, Status::Done);
        assert!(
            msgs.iter().any(|m| m.contains("delegate rejected")
                && m.contains("tree depth would become 3 (max 2)")),
            "{msgs:?}"
        );

        // 木の run 数: 親自身の run が 1 回目なので max_tree_runs = 1 で拒否。
        let (msgs, n, status) = run_with(
            DelegationLimits {
                max_tree_runs: 1,
                ..DelegationLimits::default()
            },
            vec![proposal("a", vec![])],
            1,
        )
        .await;
        assert_eq!(n, 0, "{msgs:?}");
        assert_eq!(status, Status::Done);
        assert!(
            msgs.iter()
                .any(|m| m.contains("delegate rejected") && m.contains("worker runs (max 1)")),
            "{msgs:?}"
        );
    }

    async fn wait_for_approval_child(
        d: &mut Dispatcher,
        store: &Arc<dyn TaskStore>,
        task_id: TaskId,
    ) -> Task {
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
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
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
                Ok(terminal) => Ok(RunOutcome {
                    terminal: terminal.clone(),
                    exit_code: Some(0),
                }),
                Err(retry_after) => Err(AdapterError::Throttled {
                    retry_after: *retry_after,
                }),
            }
        }
        fn with_model(&self, model: &str) -> Option<Arc<dyn WorkerAdapter>> {
            self.with_env(&[("TEST_MODEL".into(), model.into())])
        }
        fn with_env(&self, extra: &[(String, String)]) -> Option<Arc<dyn WorkerAdapter>> {
            let mut env = self.env.clone();
            env.extend(extra.iter().cloned());
            Some(Arc::new(PoolAdapter {
                env,
                ..self.clone()
            }))
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
        pool_dispatcher_with_requeues(
            store,
            adapter,
            second_provider,
            accounts_root,
            max_runs_per_account,
            max_concurrency,
            5,
        )
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
        let account_pool_providers: std::collections::HashSet<ProviderId> =
            ["p1".to_string()].into();
        Dispatcher::new(
            store,
            Box::new(policy),
            HashMap::from([("p1".to_string(), "m".to_string())]),
            adapters,
            account_pool_providers,
            DispatchConfig {
                delivery: Default::default(),
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
                reviewer_tier_override: None,
                clusters: HashMap::new(),
                cluster_cooldown: Duration::from_secs(1),
                max_requeues,
                max_reviewer_retries: 3,
                max_infra_retries: 5,
                roles: Vec::new(),
                genres: Vec::new(),
                delegation: DelegationLimits::default(),
                accounts: Some(AccountsRuntimeConfig {
                    roots: HashMap::from([(AccountAdapter::ClaudeCode, accounts_root)]),
                    max_runs_per_account,
                    check_model: "haiku".into(),
                    fallback_cooldown_secs: 300,
                }),
                memory_dir: None,
                worktree_branch_prefix: task_worker::DEFAULT_BRANCH_PREFIX.to_string(),
                releases_dir: None,
                containers: ContainersRuntimeConfig::default(),
                knowledge: KnowledgeRuntimeConfig::default(),
                session_rollover_tokens: 400_000,
                shared_build_cache: false,
                build_cache_dir: PathBuf::from("/nonexistent-build-cache"),
                workspace_prune_after_secs: 0,
                execution: ExecutionConfig::default(),
            },
        )
    }

    fn usage_window(utilization: f64, resets_at_secs_from_now: i64) -> RateLimitObservation {
        RateLimitObservation {
            five_hour: Some(task_core::RateWindow {
                utilization,
                resets_at: 10_000 + resets_at_secs_from_now,
            }),
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
        let book_path = dir.path().join(".celeris-usage.json");
        {
            let mut book = AccountBook::load(&book_path);
            book.record_observation("a", usage_window(0.8, 90_000), ObservationSource::Run);
            book.record_observation("b", usage_window(0.1, 90_000), ObservationSource::Run);
            book.save().unwrap();
        }

        let ws_dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(
            ws_dir.path(),
            Check::Command {
                cmd: "test -f touched".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            }),
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
        assert_eq!(
            dir_value.as_deref(),
            Some(dir.path().join("b").to_string_lossy().as_ref())
        );
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
        let task1 = new_task(
            ws_dir.path(),
            Check::Command {
                cmd: "test -f touched".into(),
                expect_exit: 0,
            },
            0,
        );
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
        let mut d = pool_dispatcher_with_requeues(
            store.clone(),
            adapter,
            None,
            dir.path().to_path_buf(),
            1,
            1,
            0,
        );
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
        assert!(
            !events
                .iter()
                .any(|(_, e)| matches!(e, Event::ProviderThrottled { .. }))
        );

        // 次に投入したタスクは、cooldown 中の "a" を避けて "b" に行く。
        let task2 = new_task(
            ws_dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task2).unwrap();
        for _ in 0..50 {
            d.tick().unwrap();
            let events2 = store.events_for(task2.id).unwrap();
            if events2
                .iter()
                .any(|(_, e)| matches!(e, Event::WorkerStarted { .. }))
            {
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
        let task = new_task(
            ws_dir.path(),
            Check::Command {
                cmd: "test -f touched".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done {
                summary: "unused".into(),
                evidence: vec![],
                usage: None,
            }),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured: captured.clone(),
            spawn_failure: true,
        });
        let mut d = pool_dispatcher_with_requeues(
            store.clone(),
            adapter,
            None,
            dir.path().to_path_buf(),
            1,
            1,
            0,
        );
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
            if store
                .events_for(task.id)
                .unwrap()
                .iter()
                .any(|(_, e)| matches!(e, Event::WorkerFinished { .. }))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let events = store.events_for(task.id).unwrap();
        let account = events.iter().find_map(|(_, e)| match e {
            Event::WorkerStarted { account, .. } => account.clone(),
            _ => None,
        });
        assert!(
            account.is_some(),
            "run should have used a pooled account: {events:?}"
        );
        // `ProviderThrottled` イベントが記録される（アカウントの cooldown としては扱わない）。
        assert!(
            events
                .iter()
                .any(|(_, e)| matches!(e, Event::ProviderThrottled { .. }))
        );

        // プロバイダは cooldown になる。アカウント自体は cooldown にならない。
        d.tick().unwrap(); // もう 1 tick 回し、最新のスナップショットを送らせる。
        rx.changed().await.ok();
        let snapshot = rx.borrow().clone().unwrap();
        assert!(
            !snapshot.cooldowns.is_empty(),
            "provider should be cooling down: {snapshot:?}"
        );
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
        let task = new_task(
            ws_dir.path(),
            Check::Command {
                cmd: "test -f touched".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let pool_adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done {
                summary: "should not run".into(),
                evidence: vec![],
                usage: None,
            }),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured,
            spawn_failure: false,
        });
        let fallback_adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
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
                Event::WorkerStarted {
                    provider, account, ..
                } => Some((provider.clone(), account.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(provider.as_deref(), Some("p2"));
        assert_eq!(account, None);
    }

    #[test]
    fn portable_work_uses_headroom_across_pools_then_falls_back() {
        let claude = accounts_fixture();
        let codex = tempfile::tempdir().unwrap();
        std::fs::create_dir(codex.path().join("gpt")).unwrap();
        std::fs::write(codex.path().join("gpt/auth.json"), "{}").unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = pool_dispatcher(store, adapter, None, claude.path().into(), 2, 2);
        d.now_unix_fn = Arc::new(|| 10_000);
        d.config
            .accounts
            .as_mut()
            .unwrap()
            .roots
            .insert(AccountAdapter::Codex, codex.path().into());
        d.account_books.insert(
            AccountAdapter::Codex,
            Arc::new(StdMutex::new(AccountBook::new_in_memory())),
        );
        d.account_pool_providers.insert("gpt".into());
        d.policy = Box::new(StaticPolicy::new(
            [
                ("p1", "claude-code"),
                ("local", "acp"),
                ("gpt", "codex"),
                ("research", "paperqa"),
            ]
            .into_iter()
            .map(|(id, adapter)| ProviderSpec {
                id: id.into(),
                adapter: adapter.into(),
                tiers: vec![Tier::Standard],
                concurrency: 2,
                model: String::new(),
            })
            .collect(),
            Duration::from_secs(5),
        ));
        let hint = WorkerHint {
            tier: Tier::Standard,
            adapter: None,
        };
        let now = Instant::now();
        let task = TaskId::new();
        let mut full = std::collections::HashSet::new();
        for id in ["a", "b"] {
            d.record_account_check(
                AccountAdapter::ClaudeCode,
                id,
                "ok",
                None,
                Some(usage_window(0.8, 3600)),
            );
        }
        d.record_account_check(
            AccountAdapter::Codex,
            "gpt",
            "ok",
            None,
            Some(usage_window(0.2, 3600)),
        );
        assert_eq!(
            d.select_provider(&hint, now, task, &mut full, None)
                .unwrap()
                .1,
            "gpt"
        );
        assert!(
            full.is_empty(),
            "enumeration must not exclude eligible providers for other tasks"
        );
        // 同点なら設定順。Codex を優先する固定ではない。
        d.record_account_check(
            AccountAdapter::Codex,
            "gpt",
            "ok",
            None,
            Some(usage_window(0.8, 3600)),
        );
        assert_eq!(
            d.select_provider(&hint, now, task, &mut full, None)
                .unwrap()
                .1,
            "p1"
        );
        for id in ["a", "b"] {
            d.record_account_check(
                AccountAdapter::ClaudeCode,
                id,
                "ok",
                None,
                Some(usage_window(1.0, 3600)),
            );
        }
        assert_eq!(
            d.select_provider(&hint, now, task, &mut full, None)
                .unwrap()
                .1,
            "gpt"
        );
        let pinned = WorkerHint {
            tier: Tier::Standard,
            adapter: Some("claude-code".into()),
        };
        assert!(
            d.select_provider(&pinned, now, task, &mut full, None)
                .is_none()
        );
        d.record_account_check(AccountAdapter::Codex, "gpt", "auth_failed", None, None);
        assert_eq!(
            d.select_provider(&hint, now, task, &mut full, None)
                .unwrap()
                .1,
            "local"
        );
        // 再確認が成功すれば cooldown を解除して復帰する。
        d.record_account_check(
            AccountAdapter::Codex,
            "gpt",
            "ok",
            None,
            Some(usage_window(0.1, 3600)),
        );
        full.clear();
        assert_eq!(
            d.select_provider(&hint, now, task, &mut full, None)
                .unwrap()
                .1,
            "gpt"
        );
    }

    /// ADR-0054 Phase 67c: 継続セッションが `select_provider` の入口に渡ると、ADR-0049 のランキングが
    /// 別のアカウントを勧めていても、そのセッションのアカウントに留まる（本番 2026-09-21 の事故の直接の
    /// 再現: スコアの逆転だけで account_change 扱いにならないことを確かめる）。使えなくなれば
    /// （cooldown）ランキングへフォールバックすることも確認する。
    #[tokio::test]
    async fn select_provider_sticks_to_the_sessions_account_over_a_better_scoring_one() {
        let dir = accounts_fixture();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            delay: Duration::ZERO,
        });
        let mut d = pool_dispatcher(store, adapter, None, dir.path().into(), 2, 2);
        d.now_unix_fn = Arc::new(|| 10_000);
        // "b" の残量が多い（スコアが高い）ので、sticky が無ければ "b" が勝つ。
        d.record_account_check(
            AccountAdapter::ClaudeCode,
            "a",
            "ok",
            None,
            Some(usage_window(0.8, 3600)),
        );
        d.record_account_check(
            AccountAdapter::ClaudeCode,
            "b",
            "ok",
            None,
            Some(usage_window(0.1, 3600)),
        );
        let hint = WorkerHint {
            tier: Tier::Standard,
            adapter: None,
        };
        let now = Instant::now();
        let task = TaskId::new();

        // ランキングだけなら "b" が勝つ（前提の確認）。
        let mut full = std::collections::HashSet::new();
        assert_eq!(
            d.select_provider(&hint, now, task, &mut full, None)
                .unwrap()
                .2,
            Some((AccountAdapter::ClaudeCode, "b".to_string()))
        );

        let active = NodeSession::new(
            task_core::COS_ID,
            SessionKind::Conversation,
            None,
            "claude-code",
            Some("a".to_string()),
            "550e8400-e29b-41d4-a716-446655440000",
            OffsetDateTime::now_utc(),
        );

        // 継続セッションが "a" にあれば、"b" の方がスコアが高くても "a" に留まる。
        let mut full = std::collections::HashSet::new();
        let (adapter_id, provider_id, selected_account) = d
            .select_provider(&hint, now, task, &mut full, Some(&active))
            .unwrap();
        assert_eq!(adapter_id, "claude-code");
        assert_eq!(provider_id, "p1");
        assert_eq!(
            selected_account,
            Some((AccountAdapter::ClaudeCode, "a".to_string()))
        );

        // "a" が cooldown（例: 認証失敗）に落ちたら、留まれないので通常のランキング（"b"）へ戻る。
        d.record_account_check(AccountAdapter::ClaudeCode, "a", "auth_failed", None, None);
        let mut full = std::collections::HashSet::new();
        let (_, _, selected_account) = d
            .select_provider(&hint, now, task, &mut full, Some(&active))
            .unwrap();
        assert_eq!(
            selected_account,
            Some((AccountAdapter::ClaudeCode, "b".to_string())),
            "unusable account must fall back to the normal ranking"
        );
    }

    /// (d) run の途中で受け取った `rate_limit_event` の観測値が `AccountBook` とスナップショットに反映される。
    #[tokio::test]
    async fn mid_run_rate_limit_observation_lands_in_the_book_and_the_snapshot() {
        let dir = accounts_fixture();
        let ws_dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(
            ws_dir.path(),
            Check::Command {
                cmd: "test -f touched".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            }),
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
        assert_eq!(
            snapshot.accounts_root.as_deref(),
            Some(dir.path().to_string_lossy().as_ref())
        );
        assert_eq!(snapshot.max_runs_per_account, Some(2));
        let a_or_b = snapshot
            .accounts
            .iter()
            .find(|a| a.usage.is_some())
            .unwrap_or_else(|| {
                panic!(
                    "no account carries the observation: {:?}",
                    snapshot.accounts
                )
            });
        let usage = a_or_b.usage.as_ref().unwrap();
        assert_eq!(usage.five_hour.map(|w| w.utilization), Some(0.42));
        assert_eq!(usage.source, "run");
    }

    /// (e) 帳簿（`AccountBook`）はファイルに保存され、celeris の再起動（新しい `Dispatcher`）後も残る。
    #[tokio::test]
    async fn account_book_is_persisted_and_reloaded_after_restart() {
        let dir = accounts_fixture();
        let ws_dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(
            ws_dir.path(),
            Check::Command {
                cmd: "test -f touched".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            }),
            delay: Duration::ZERO,
            observation: Some(usage_window(0.33, 90_000)),
            env: Vec::new(),
            captured,
            spawn_failure: false,
        });
        let mut d1 = pool_dispatcher(store.clone(), adapter, None, dir.path().to_path_buf(), 2, 2);
        let report = run_until_idle(&mut d1, 200).await;
        assert!(report.idle);
        assert!(dir.path().join(".celeris-usage.json").exists());
        drop(d1);

        // "celeris を再起動" = 新しい Dispatcher（同じ store・同じ accounts root）を作る。
        let never_used = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done {
                summary: "unused".into(),
                evidence: vec![],
                usage: None,
            }),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured: Arc::new(StdMutex::new(Vec::new())),
            spawn_failure: false,
        });
        let mut d2 = pool_dispatcher(
            store.clone(),
            never_used,
            None,
            dir.path().to_path_buf(),
            2,
            2,
        );
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
        let usage = used_account
            .expect("observation survives restart")
            .usage
            .as_ref()
            .unwrap();
        assert_eq!(usage.five_hour.map(|w| w.utilization), Some(0.33));
    }

    // ---- ADR-0033 D4 / D6（Phase 24）: 対話と記憶 ----

    /// 渡された `RunContext` を記録し、結果ファイルに `memory` を書いてから終端を返す。
    /// `proposals` があれば委譲も提案する。
    struct PersonAdapter {
        terminal: Terminal,
        seen: Arc<StdMutex<Option<RunContext>>>,
        memory: Option<&'static str>,
        proposals: Vec<DelegateTask>,
    }

    #[async_trait]
    impl WorkerAdapter for PersonAdapter {
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
            *self.seen.lock().unwrap() = Some(req.context.clone());
            if let Some(memory) = self.memory {
                let artifacts = req.artifacts_dir.clone();
                std::fs::create_dir_all(&artifacts).unwrap();
                std::fs::write(artifacts.join("result.json"), memory).unwrap();
            }
            if !self.proposals.is_empty() && req.task.assignee.as_deref() == Some("research-survey")
            {
                sink.delegate(&self.proposals);
            }
            Ok(RunOutcome {
                terminal: self.terminal.clone(),
                exit_code: Some(0),
            })
        }
    }

    fn person_adapter(terminal: Terminal) -> PersonAdapter {
        PersonAdapter {
            terminal,
            seen: Arc::new(StdMutex::new(None)),
            memory: None,
            proposals: Vec::new(),
        }
    }

    fn org_node_of(id: &str, parent: Option<&str>, kind: OrgKind, genre: Option<&str>) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            profile: Default::default(),
            id: id.into(),
            parent_id: parent.map(str::to_string),
            name: format!("{id} 課"),
            kind,
            genre: genre.map(str::to_string),
            brief: format!("{id} の担当"),
            position: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn seed_conversation_org(store: &dyn TaskStore) {
        for n in [
            org_node_of("secretary", None, OrgKind::Secretary, Some("secretary")),
            org_node_of("research", Some("secretary"), OrgKind::Department, None),
            org_node_of("research-survey", Some("research"), OrgKind::Section, None),
            org_node_of("research-data", Some("research"), OrgKind::Section, None),
            org_node_of("coding", Some("secretary"), OrgKind::Department, None),
            org_node_of("coding-poc", Some("coding"), OrgKind::Section, None),
        ] {
            store.org_upsert(&n).unwrap();
        }
    }

    fn person_dispatcher(
        store: Arc<dyn TaskStore>,
        adapter: Arc<dyn WorkerAdapter>,
        workspace_root: PathBuf,
        memory_dir: Option<PathBuf>,
    ) -> Dispatcher {
        let mut d = dispatcher(store, adapter, 2);
        d.config.workspace_root = workspace_root;
        d.config.memory_dir = memory_dir;
        d.config.genres = vec![GenreSpec {
            id: "secretary".into(),
            description: "人と話す".into(),
            default_role: Some("secretary".into()),
            roles: vec!["secretary".into()],
            ..GenreSpec::default()
        }];
        d
    }

    fn assigned_task(workspace_root: &std::path::Path, name: &str, assignee: &str) -> Task {
        let dir = workspace_root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut task = new_task(&dir, Check::Human, 0);
        task.assignee = Some(assignee.to_string());
        task
    }

    fn delegate_to(assignee: &str) -> DelegateTask {
        DelegateTask {
            title: "任せたい仕事".into(),
            objective: "やっておいて".into(),
            // ADR-0067 D2: `human` チェックには artifacts か知識ベースの参照が要る。
            acceptance: vec![
                Criterion {
                    text: "できた".into(),
                    check: Check::Human,
                },
                Criterion {
                    text: "result.md exists".into(),
                    check: Check::ArtifactExists {
                        name: "result.md".into(),
                    },
                },
            ],
            role: None,
            genre: None,
            depends_on: vec![],
            tier: None,
            assignee: Some(assignee.to_string()),
            workspace: None,
        }
    }

    /// ADR-0033 D4: 話しかけると対話用タスクができ、run の `summary` が `role = node` の行になる
    /// （`run_id` 付き）。前置きには役職と brief・記憶・直近のやり取りが載る。
    /// ADR-0033 D6: 結果ファイルの `memory` が日付付きの箇条書きで追記される。
    #[tokio::test]
    async fn a_conversation_run_answers_in_messages_and_writes_its_memory() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        let memory_root = dir.path().join("memory");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        // 先週覚えたこと。
        task_worker::MemoryDir::new(&memory_root)
            .append(
                "secretary",
                None,
                &task_worker::MemoryUpdate {
                    notes: vec!["人は図より表が好き".into()],
                    project: vec![],
                },
                "2026-09-10",
            )
            .unwrap();

        let started = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "先週の続きを教えて",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        let seen = Arc::new(StdMutex::new(None));
        let adapter = Arc::new(PersonAdapter {
            terminal: Terminal::Done {
                summary: "3 本の候補が出ています".into(),
                evidence: vec![],
                usage: None,
            },
            seen: seen.clone(),
            memory: Some(
                r#"{"summary":"ok","evidence":[],"memory":{"notes":["pegasus は pjsub"]}}"#,
            ),
            proposals: Vec::new(),
        });
        let mut d = person_dispatcher(
            store.clone(),
            adapter,
            workspace_root,
            Some(memory_root.clone()),
        );
        run_until_idle(&mut d, 40).await;

        // 1. 返事が `role = node` の行になり、run_id が付く。
        let thread = store.message_list("secretary", None, 20).unwrap();
        assert_eq!(thread.len(), 2, "{thread:?}");
        assert_eq!(thread[0].role, MessageRole::User);
        assert_eq!(thread[1].role, MessageRole::Node);
        assert_eq!(thread[1].text, "3 本の候補が出ています");
        assert!(thread[1].run_id.is_some(), "返事には run_id が付く");
        assert_eq!(
            store.get(started.task.id).unwrap().unwrap().status,
            Status::Done
        );

        // 2. 前置きに役職・brief・記憶・直近のやり取りが載っている。
        let context = seen.lock().unwrap().clone().expect("the run happened");
        let node = context.node.clone().expect("node context");
        assert_eq!(node.id, "secretary");
        assert_eq!(node.brief, "secretary の担当");
        assert_eq!(
            context.memory.clone().expect("memory").notes,
            "- 2026-09-10: 人は図より表が好き\n"
        );
        // 監査 L-6: 今回の本文は `objective` に載るので、直近のやり取りには**入れない**（二重に載せない）。
        assert!(
            context.conversation.is_empty(),
            "{:?}",
            context.conversation
        );
        let preamble = task_worker::preamble::render(&context, "artifacts");
        assert!(
            preamble.contains("## あなた: secretary 課 (secretary)"),
            "{preamble}"
        );
        assert!(
            preamble.contains("- 2026-09-10: 人は図より表が好き"),
            "{preamble}"
        );
        assert!(!preamble.contains("## 直近のやり取り"), "{preamble}");

        // 3. 結果ファイルの `memory` が追記されている（古い記憶の後ろに）。
        let notes = std::fs::read_to_string(memory_root.join("secretary/notes.md")).unwrap();
        assert_eq!(notes.lines().count(), 2, "{notes}");
        assert!(
            notes.lines().next().unwrap().contains("人は図より表が好き"),
            "{notes}"
        );
        assert!(
            notes
                .lines()
                .next_back()
                .unwrap()
                .contains("pegasus は pjsub"),
            "{notes}"
        );
    }

    /// ADR-0033 D6: `[memory]` を設定していない構成では記憶を読まないし書かない。
    #[tokio::test]
    async fn without_a_memory_dir_nothing_is_read_or_written() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "やあ",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        let seen = Arc::new(StdMutex::new(None));
        let adapter = Arc::new(PersonAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            seen: seen.clone(),
            memory: Some(r#"{"summary":"ok","memory":{"notes":["覚えて"]}}"#),
            proposals: Vec::new(),
        });
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root, None);
        run_until_idle(&mut d, 40).await;

        let context = seen.lock().unwrap().clone().expect("the run happened");
        assert!(context.memory.is_none(), "記憶を渡さない");
        assert!(context.node.is_some(), "役職は記憶とは別に渡る");
        assert!(!dir.path().join("memory").exists(), "書きもしない");
    }

    /// ADR-0033 D4: run が `Error` に終わったら「返事できませんでした: …」を返事にする。
    #[tokio::test]
    async fn a_failed_conversation_run_says_it_could_not_answer() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "調子はどう",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        let adapter = Arc::new(person_adapter(Terminal::Error {
            message: "harness died".into(),
            retryable: false,
        }));
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root, None);
        run_until_idle(&mut d, 40).await;

        let thread = store.message_list("secretary", None, 20).unwrap();
        let reply = thread.last().expect("a reply");
        assert_eq!(reply.role, MessageRole::Node);
        assert!(
            reply.text.starts_with("返事できませんでした: "),
            "{}",
            reply.text
        );
        assert!(reply.text.contains("harness died"), "{}", reply.text);
    }

    /// 監査 L-6（Phase 27）: 直近のやり取りには**前回まで**が載り、今回の本文（`objective` と同じ最後の
    /// `user` の行）は落ちる。
    #[tokio::test]
    async fn the_recent_turns_keep_the_past_but_drop_this_very_message() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        let first = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "先週の続きを教えて",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        task_ops::conversation::record_reply(
            store.as_ref(),
            &first.task,
            "run-1",
            "承知しました",
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        let second = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "その後どう",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        let adapter = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let d = person_dispatcher(store.clone(), adapter, workspace_root, None);
        let extras = d
            .run_extras(&second.task, None, None, "claude-code")
            .unwrap();
        let texts: Vec<&str> = extras
            .conversation
            .iter()
            .map(|t| t.text.as_str())
            .collect();
        assert_eq!(
            texts,
            vec!["先週の続きを教えて", "承知しました"],
            "{texts:?}"
        );
    }

    /// 監査 M-5（Phase 27）: 途中の失敗（retryable でまだ試行が残る）では返事を書かない。
    /// 失敗して `Failed` に落ちたときだけ「返事できませんでした」を 1 行書く。
    #[tokio::test]
    async fn a_retried_conversation_run_answers_only_once() {
        struct FlakyPersonAdapter {
            failures_left: AtomicUsize,
        }
        #[async_trait]
        impl WorkerAdapter for FlakyPersonAdapter {
            fn id(&self) -> &str {
                "instant"
            }
            async fn run(
                &self,
                _req: RunRequest,
                _run_id: &str,
                _limits: RunLimits,
                _sink: &dyn EventSink,
            ) -> Result<RunOutcome, AdapterError> {
                if self.failures_left.load(Ordering::SeqCst) > 0 {
                    self.failures_left.fetch_sub(1, Ordering::SeqCst);
                    return Ok(RunOutcome {
                        terminal: Terminal::Error {
                            message: "harness hiccup".into(),
                            retryable: true,
                        },
                        exit_code: Some(1),
                    });
                }
                Ok(RunOutcome {
                    terminal: Terminal::Done {
                        summary: "順調です".into(),
                        evidence: vec![],
                        usage: None,
                    },
                    exit_code: Some(0),
                })
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        let started = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "調子はどう",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        let adapter = Arc::new(FlakyPersonAdapter {
            failures_left: AtomicUsize::new(1),
        });
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root, None);
        run_until_idle(&mut d, 60).await;

        assert_eq!(
            store.get(started.task.id).unwrap().unwrap().status,
            Status::Done
        );
        let thread = store.message_list("secretary", None, 20).unwrap();
        let replies: Vec<&Message> = thread
            .iter()
            .filter(|m| m.role == MessageRole::Node)
            .collect();
        assert_eq!(replies.len(), 1, "返事は 1 行だけ: {replies:?}");
        assert_eq!(replies[0].text, "順調です");
        assert!(
            !thread
                .iter()
                .any(|m| m.text.starts_with("返事できませんでした")),
            "途中の失敗は返事にしない: {thread:?}"
        );
    }

    /// ADR-0033 D4 / SPEC §3.1 → ADR-0069 D1（Phase 114）: 委譲（LLM）が別の部の課を `assignee` に
    /// 書いても、その担当は使わない（捨てて進行に残す）。担当を名指ししないので部をまたぐ認可の質問も
    /// 起きず、子はその場で作られ、担当は matching が決める。以前はここで子を作らずに秘書へ質問し、
    /// 人が `once` / `standing` で認めると次の run で通っていた（その 2 本のテストはこの挙動変更で
    /// 役目を終えたので、1 本にまとめて新しい規則を確かめる。`split_delegation` 自体の単体テストは
    /// `task-ops` に残る）。
    #[tokio::test]
    async fn a_delegation_naming_other_departments_creates_children_and_drops_the_assignees() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        let task = assigned_task(&workspace_root, "t1", "research-survey");
        store.create_task(&task, vec![]).unwrap();

        let adapter = Arc::new(PersonAdapter {
            terminal: Terminal::Done {
                summary: "delegated".into(),
                evidence: vec![],
                usage: None,
            },
            seen: Arc::new(StdMutex::new(None)),
            memory: None,
            proposals: vec![delegate_to("research-data"), delegate_to("coding-poc")],
        });
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root, None);
        run_until_idle(&mut d, 40).await;

        let children: Vec<Task> = store
            .children(task.id)
            .unwrap()
            .into_iter()
            .filter(|c| c.kind == TaskKind::Execute)
            .collect();
        assert_eq!(children.len(), 2, "部をまたいでも止めない: {children:?}");
        assert!(
            children.iter().all(|c| c.assignee.is_none()),
            "{children:?}"
        );
        assert!(
            store
                .approval_list(Some(true), None, None)
                .unwrap()
                .is_empty(),
            "担当を名指ししないので部をまたぐ認可は起きない"
        );
        assert_ne!(store.get(task.id).unwrap().unwrap().status, Status::Blocked);
        let notes: Vec<String> = store
            .events_for(task.id)
            .unwrap()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerProgress { msg, .. } => Some(msg),
                _ => None,
            })
            .collect();
        for dropped in ["research-data", "coding-poc"] {
            assert!(
                notes
                    .iter()
                    .any(|m| m.contains(&format!("担当の指定 {dropped} は使わない"))),
                "{notes:?}"
            );
        }
    }

    /// 同じ部の中の委譲は、これまでどおり子タスクになる（規則が効きすぎないこと）。ADR-0069 D1 以降、
    /// 委譲が書いた担当は子に残らない。
    #[tokio::test]
    async fn a_delegation_inside_the_same_department_still_creates_children() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let task = assigned_task(&workspace_root, "t2", "research-survey");
        store.create_task(&task, vec![]).unwrap();

        let adapter = Arc::new(PersonAdapter {
            terminal: Terminal::Done {
                summary: "delegated".into(),
                evidence: vec![],
                usage: None,
            },
            seen: Arc::new(StdMutex::new(None)),
            memory: None,
            proposals: vec![delegate_to("research-data")],
        });
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root, None);
        run_until_idle(&mut d, 10).await;

        // Human check の承認用の子（`kind = approval`）は数に入れない。
        let children: Vec<Task> = store
            .children(task.id)
            .unwrap()
            .into_iter()
            .filter(|c| c.kind == TaskKind::Execute)
            .collect();
        assert_eq!(children.len(), 1, "同じ部の中なら子ができる");
        // ADR-0069 D1: 委譲が書いた担当は使わない（matching が決める）。
        assert_eq!(children[0].assignee, None);
        assert_eq!(children[0].title, "任せたい仕事");
    }

    /// Phase 28（ADR-0033 D4 追記）: 対話 run は委譲できない。実機で秘書が返事の代わりに research-survey へ
    /// 委譲し、対話タスクが `blocked` に落ちた事故の再発防止。`delegate` は子を作らず、理由が
    /// `WorkerProgress` に残る。
    #[tokio::test]
    async fn a_conversation_run_cannot_delegate() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let started = task_ops::conversation::start(
            store.as_ref(),
            "research-survey",
            None,
            "調べて",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        let adapter = Arc::new(PersonAdapter {
            terminal: Terminal::Done {
                summary: "やっておきます".into(),
                evidence: vec![],
                usage: None,
            },
            seen: Arc::new(StdMutex::new(None)),
            memory: None,
            proposals: vec![delegate_to("research-data")],
        });
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root, None);
        run_until_idle(&mut d, 40).await;

        let children: Vec<Task> = store.children(started.task.id).unwrap();
        assert!(children.is_empty(), "対話 run は委譲できない: {children:?}");
        let notes: Vec<String> = store
            .events_for(started.task.id)
            .unwrap()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerProgress { msg, .. } => Some(msg),
                _ => None,
            })
            .collect();
        assert!(
            notes.iter().any(|m| m.contains("対話では委譲できない")),
            "{notes:?}"
        );
        assert_eq!(
            store.get(started.task.id).unwrap().unwrap().status,
            Status::Done
        );
    }

    /// Phase 28（ADR-0033 D4 追記）: 対話 run は `Question` を出さない。そのまま `Done` の返事になり、
    /// `approvals` の行はできない（実機で「最終試行なので自分の一般知識で答えた」まま走った事故の反省）。
    #[tokio::test]
    async fn a_conversation_run_turns_a_question_into_a_reply_without_an_approval() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let started = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "この案件をお願いします",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        let adapter = Arc::new(person_adapter(Terminal::Question {
            text: "予算とクラスタを教えてください".into(),
        }));
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root, None);
        run_until_idle(&mut d, 40).await;

        assert_eq!(
            store.get(started.task.id).unwrap().unwrap().status,
            Status::Done,
            "Question は Done 扱い"
        );
        let thread = store.message_list("secretary", None, 20).unwrap();
        let reply = thread.last().expect("a reply");
        assert_eq!(reply.role, MessageRole::Node);
        assert_eq!(reply.text, "予算とクラスタを教えてください");
        assert!(
            store.approval_list(None, None, None).unwrap().is_empty(),
            "approvals の行はできない"
        );
    }

    /// Phase 28（ADR-0033 D4 追記）: 対話 run には委譲の道具（`available_genres`）を渡さない。
    /// 相手が秘書かそれ以外かで `conversation_addressee` を出し分ける。通常タスクには付かない。
    /// ADR-0046 D6（Phase 59 追記）: **CoS（根）の対話 run** にだけ、誰が何をできるかの組織図
    /// （`organization`）を渡す（人選はしない。matching が決める）。CoS 以外の対話 run には渡さない。
    #[test]
    fn conversation_runs_get_no_delegation_tools_but_get_the_addressee() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let to_secretary = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "hi",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
        .task;
        let to_survey = task_ops::conversation::start(
            store.as_ref(),
            "research-survey",
            None,
            "hi",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
        .task;

        let adapter = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let d = person_dispatcher(store.clone(), adapter, workspace_root.clone(), None);

        let extras = d
            .run_extras(&to_secretary, None, None, "claude-code")
            .unwrap();
        assert!(
            extras.available_genres.is_empty(),
            "対話 run は委譲できない: {extras:?}"
        );
        // ADR-0046 D6: CoS（根 = `secretary`。`OrgKind::Secretary`）宛ての対話には組織の一覧が付く。
        assert!(
            !extras.organization.is_empty(),
            "CoS 宛ての対話には組織の一覧が付く: {extras:?}"
        );
        assert!(
            extras
                .organization
                .iter()
                .any(|n| n.id == "research-survey")
        );
        assert_eq!(
            extras.conversation_addressee,
            Some(ConversationAddressee::Secretary)
        );

        let extras = d.run_extras(&to_survey, None, None, "claude-code").unwrap();
        assert_eq!(
            extras.conversation_addressee,
            Some(ConversationAddressee::Other)
        );
        // CoS 以外（`research-survey`）宛ての対話には組織の一覧を付けない。
        assert!(
            extras.organization.is_empty(),
            "CoS 以外の対話には付けない: {extras:?}"
        );

        // 通常タスク（対話由来でない）には付かない。
        let ordinary = assigned_task(&workspace_root, "ordinary", "research-survey");
        let extras = d.run_extras(&ordinary, None, None, "claude-code").unwrap();
        assert_eq!(extras.conversation_addressee, None);
    }

    /// ADR-0048 D3（Phase 60b）: CoS の対話 run にだけ、進行中の案件（`proposed` / `active`）と
    /// その途中目標を渡す。`done` / `cancelled` の案件は出さない。CoS 以外の対話・通常タスクには付かない。
    #[test]
    fn cos_conversations_carry_active_projects_and_their_milestones() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let mut active = titled_project("進行中の案件");
        active.workspace = Some(WorkspaceSpec::Local {
            path: dir.path().join("agent-platform"),
            mode: Default::default(),
        });
        active.status = ProjectStatus::Active;
        store.project_create(&active).unwrap();
        let milestone = store
            .milestone_create(
                active.id,
                "最初の途中目標",
                "d",
                MilestoneStatus::InProgress,
            )
            .unwrap();

        let mut done = titled_project("終わった案件");
        done.status = ProjectStatus::Done;
        store.project_create(&done).unwrap();

        let to_secretary = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "hi",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
        .task;
        let to_survey = task_ops::conversation::start(
            store.as_ref(),
            "research-survey",
            None,
            "hi",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
        .task;

        let adapter = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let d = person_dispatcher(store.clone(), adapter, workspace_root.clone(), None);

        let extras = d
            .run_extras(&to_secretary, None, None, "claude-code")
            .unwrap();
        assert_eq!(
            extras.active_projects.len(),
            1,
            "{:?}",
            extras.active_projects
        );
        let project = &extras.active_projects[0];
        assert_eq!(project.id, active.id.to_string());
        assert_eq!(project.title, "進行中の案件");
        assert_eq!(project.status, "active");
        assert_eq!(project.repos, vec!["agent-platform"]);
        assert_eq!(project.milestones.len(), 1);
        assert_eq!(project.milestones[0].id, milestone.id.to_string());
        assert_eq!(project.milestones[0].title, "最初の途中目標");
        assert_eq!(project.milestones[0].status, "in_progress");

        // CoS 以外の対話には渡さない。
        let extras = d.run_extras(&to_survey, None, None, "claude-code").unwrap();
        assert!(extras.active_projects.is_empty());

        // 通常タスクにも渡さない。
        let ordinary = assigned_task(&workspace_root, "ordinary", "research-survey");
        let extras = d.run_extras(&ordinary, None, None, "claude-code").unwrap();
        assert!(extras.active_projects.is_empty());
    }

    /// ADR-0054 D1（Phase 67）: 継続中（`resume = true`）の CoS 対話 run は、brief・記憶・組織の一覧・
    /// 進行中の案件を**流し直さない**（前置きは差分だけ）。1 本目（新規セッション）はこれまでどおり全量を
    /// 渡す。CoS 以外への対話にはそもそもセッションが付かないので、毎回全量のまま（対象外）。
    #[test]
    fn a_continuing_cos_session_drops_the_full_preamble_and_a_fresh_one_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let mut active = titled_project("進行中の案件");
        active.status = ProjectStatus::Active;
        store.project_create(&active).unwrap();

        let to_secretary = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "hi",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
        .task;

        let adapter = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let d = person_dispatcher(store.clone(), adapter, workspace_root.clone(), None);

        // 1 本目: 現役セッションが無いので新規（全量。Phase 66 までと同じ振る舞い）。
        let first = d
            .run_extras(&to_secretary, None, None, "claude-code")
            .unwrap();
        let session = first
            .session
            .as_ref()
            .expect("claude-code supports continuous sessions");
        assert!(
            !session.resume,
            "the first run of a session is never a resume"
        );
        assert!(first.node.is_some(), "fresh session: brief を渡す");
        assert!(
            !first.organization.is_empty(),
            "fresh session: 組織の一覧を渡す"
        );
        assert_eq!(
            first.active_projects.len(),
            1,
            "fresh session: 進行中の案件を渡す"
        );

        // 2 本目（同じノード）: 続く（resume）。前置きは差分だけになる。
        let second = d
            .run_extras(&to_secretary, None, None, "claude-code")
            .unwrap();
        let session2 = second.session.as_ref().expect("still claude-code");
        assert!(session2.resume, "the second run resumes the same session");
        assert_eq!(session2.session_id, session.session_id);
        assert!(
            second.node.is_none(),
            "continuing session: brief を流し直さない: {:?}",
            second.node
        );
        assert!(
            second.organization.is_empty(),
            "continuing session: 組織の一覧を流し直さない: {:?}",
            second.organization
        );
        assert!(
            second.active_projects.is_empty(),
            "continuing session: 進行中の案件を流し直さない（差分に「新しい案件」が乗る）: {:?}",
            second.active_projects
        );

        // CoS 以外への対話にはセッションが付かない（`is_cos_conversation` でなければ常に `None`）ので、
        // 何度呼んでも全量のまま（対象外）。
        let to_survey = task_ops::conversation::start(
            store.as_ref(),
            "research-survey",
            None,
            "hi",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
        .task;
        let other = d.run_extras(&to_survey, None, None, "claude-code").unwrap();
        assert!(other.session.is_none());
        assert!(
            other.node.is_some(),
            "CoS 以外は継続の対象外なので brief は毎回渡す"
        );
    }

    /// Phase 99b（ADR-0059 追記、実機 2026-09-22 13:19 UTC タスク 01M34MACCEZ032A6YF8R4BMFM1）:
    /// クラスタ一覧（`RunExtras::clusters`）は `recent_work` / `knowledge` / `profile` / `role` と同じ
    /// 「いまの状態」なので、継続中（resume）の CoS 対話 run でも毎回渡す。Phase 99 まではフル
    /// プリアンブルと同じ扱いで継続中は空になっており、CoS はセッションが rollover するまでクラスタと
    /// 実効 work_dir を知れなかった。
    #[test]
    fn cluster_context_is_carried_into_continuing_cos_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let to_secretary = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "hi",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
        .task;

        let adapter = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root.clone(), None);
        d.config.clusters.insert(
            "pegasus".into(),
            cluster_spec_with_auth("pegasus", "pegasus", "manual"),
        );

        // 1 本目（新規セッション）: 従来どおりクラスタ一覧を渡す。
        let first = d
            .run_extras(&to_secretary, None, None, "claude-code")
            .unwrap();
        assert!(!first.session.as_ref().unwrap().resume);
        assert_eq!(first.clusters.len(), 1, "fresh session: クラスタ一覧を渡す");
        assert_eq!(first.clusters[0].id, "pegasus");

        // 2 本目（継続 = resume）: brief などの全量前置きは空になるが、クラスタ一覧は毎回渡す。
        let second = d
            .run_extras(&to_secretary, None, None, "claude-code")
            .unwrap();
        assert!(second.session.as_ref().unwrap().resume);
        assert!(
            second.node.is_none(),
            "continuing session: brief は流し直さない（対比のための確認）"
        );
        assert_eq!(
            second.clusters.len(),
            1,
            "continuing session でもクラスタ一覧は毎回渡す（Phase 99b）"
        );
        assert_eq!(second.clusters[0].id, "pegasus");

        // CoS 以外への対話には（継続の有無に関わらず）クラスタ一覧を渡さない。
        let to_survey = task_ops::conversation::start(
            store.as_ref(),
            "research-survey",
            None,
            "hi",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
        .task;
        let other = d.run_extras(&to_survey, None, None, "claude-code").unwrap();
        assert!(other.clusters.is_empty(), "CoS 以外の run では常に空");
    }

    /// Phase 43（ADR-0039 D3）: 案件が作業場所を決めていれば、その run の前置きに出す 1 行が `RunExtras` に
    /// 入る。決めていない案件・案件に属さないタスク・対話 run には入らない（従来どおりのプロンプト）。
    #[test]
    fn run_extras_carry_the_projects_workspace_note() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let now = OffsetDateTime::now_utc();
        let mut with_workspace = titled_project("Pluvio の PoC");
        with_workspace.workspace = Some(WorkspaceSpec::Remote {
            cluster: "pegasus".into(),
            path: PathBuf::from("/work/NBB/rmaeda/workspace/rust/benchfs"),
            mode: None,
        });
        store.project_create(&with_workspace).unwrap();
        let plain = titled_project("作業場所なし");
        store.project_create(&plain).unwrap();

        let adapter = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let d = person_dispatcher(store.clone(), adapter, workspace_root.clone(), None);

        let mut task = assigned_task(&workspace_root, "poc", "research-survey");
        task.project_id = Some(with_workspace.id);
        let extras = d.run_extras(&task, None, None, "claude-code").unwrap();
        assert_eq!(
            extras.workspace_note.as_deref(),
            Some(
                "この案件のコードはクラスタ pegasus の `/work/NBB/rmaeda/workspace/rust/benchfs` にある。\
                 いまのカレントディレクトリはその写しで、celeris が run の前後で同期する。"
            )
        );

        task.project_id = Some(plain.id);
        assert_eq!(
            d.run_extras(&task, None, None, "claude-code")
                .unwrap()
                .workspace_note,
            None
        );
        task.project_id = None;
        assert_eq!(
            d.run_extras(&task, None, None, "claude-code")
                .unwrap()
                .workspace_note,
            None
        );

        // 対話 run には出さない（会話は編集をしない。ADR-0039 D2）。
        let conversation = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            Some(with_workspace.id),
            "状況を教えて",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            now,
        )
        .unwrap()
        .task;
        assert_eq!(
            d.run_extras(&conversation, None, None, "claude-code")
                .unwrap()
                .workspace_note,
            None
        );
    }

    /// Phase 30（ADR-0033 D4 追記）: 対話は**ノードの `genre`（仕事のハーネス）に関係なく**常に対話用分野
    /// （`task.genre`）で走る。実機の事故: 関連研究調査課（`genre = web-research` = LDR）に話しかけたら
    /// 検索ハーネスが会話しようとして証拠ゲートで落ちた。ただし「人」らしさは保つため、対話 run にだけ、
    /// 担当ノードが自分の仕事の分野を持てば `context.work_genre` として前置きに渡す（分野を持たない
    /// ノードや通常タスクには乗らない）。
    #[test]
    fn conversation_runs_get_the_nodes_own_work_genre_when_it_has_one() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        for n in [
            org_node_of("secretary", None, OrgKind::Secretary, Some("secretary")),
            org_node_of("research", Some("secretary"), OrgKind::Department, None),
            org_node_of(
                "research-survey",
                Some("research"),
                OrgKind::Section,
                Some("literature"),
            ),
            org_node_of("research-data", Some("research"), OrgKind::Section, None),
        ] {
            store.org_upsert(&n).unwrap();
        }

        let to_survey = task_ops::conversation::start(
            store.as_ref(),
            "research-survey",
            None,
            "なぜ web search に失敗しているのでしょうか？",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
        .task;
        let to_data = task_ops::conversation::start(
            store.as_ref(),
            "research-data",
            None,
            "図表の相談",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
        .task;

        // 対話そのものは常に対話用分野で走る（ノードの genre = literature ではない。分野の解決は
        // `task_ops::conversation` 側のテストで見ているのでここでは genre 未指定 = `None` のまま）。
        assert_eq!(to_survey.genre, None);

        let adapter = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root.clone(), None);
        d.config.genres.push(GenreSpec {
            id: "literature".into(),
            description: "関連研究の調査".into(),
            capabilities: vec!["学術文献の検索".into(), "引用グラフの探索".into()],
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-reader".into()],
            ..GenreSpec::default()
        });

        let extras = d.run_extras(&to_survey, None, None, "claude-code").unwrap();
        let work_genre = extras
            .work_genre
            .expect("research-survey has its own genre");
        assert_eq!(work_genre.id, "literature");
        assert_eq!(work_genre.description, "関連研究の調査");
        assert_eq!(
            work_genre.capabilities,
            vec!["学術文献の検索".to_string(), "引用グラフの探索".to_string()]
        );

        // 分野を持たないノードには `work_genre` が乗らない。
        let extras = d.run_extras(&to_data, None, None, "claude-code").unwrap();
        assert!(extras.work_genre.is_none());

        // 通常タスク（対話由来でない）には、担当が genre を持っていても乗らない
        // （`work_genre` は対話専用。仕事の run は `task.genre` 自体がその分野になる）。
        let ordinary = assigned_task(&workspace_root, "ordinary", "research-survey");
        let extras = d.run_extras(&ordinary, None, None, "claude-code").unwrap();
        assert!(extras.work_genre.is_none());
    }

    /// ADR-0033 D5（Phase 26）: `Question` で終わった run は既存の `Blocked` / `answers[]` に加えて、
    /// `approvals` にも担当ノード宛ての 1 件を残す。
    #[tokio::test]
    async fn a_question_from_an_assigned_run_creates_a_pending_approval() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let task = assigned_task(&workspace_root, "t3", "research-survey");
        store.create_task(&task, vec![]).unwrap();

        let adapter = Arc::new(person_adapter(Terminal::Question {
            text: "どのクラスタを使いますか".into(),
        }));
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root, None);
        run_until_idle(&mut d, 40).await;

        assert_eq!(
            store.get(task.id).unwrap().unwrap().status,
            Status::Blocked,
            "既存の質問の終端はそのまま"
        );
        let pending = store.approval_list(Some(true), None, None).unwrap();
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(pending[0].node_id, "research-survey");
        assert_eq!(pending[0].task_id, Some(task.id));
        assert_eq!(pending[0].question, "どのクラスタを使いますか");
        assert!(pending[0].is_pending());
    }

    /// 担当のいないタスクの質問は秘書へ回る（フォールバック。ADR-0033 D5）。
    #[tokio::test]
    async fn a_question_without_an_assignee_goes_to_the_secretary() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        let q = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&q).unwrap();
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Question {
                text: "続けますか".into(),
            },
            delay: Duration::ZERO,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        run_until_idle(&mut d, 40).await;

        let pending = store.approval_list(Some(true), None, None).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].node_id, "secretary");
    }

    /// ADR-0033 D5（Phase 26）: 全員向け + そのノード向けの永続の認可が run の前置きに載る。
    #[tokio::test]
    async fn standing_rules_for_everyone_and_the_assignee_reach_the_preamble() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspaces");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let now = OffsetDateTime::now_utc();
        store
            .standing_rule_append(&StandingRule {
                id: StandingRuleId::new(),
                node_id: None,
                rule: "深夜は連絡しない".into(),
                created_at: now - time::Duration::minutes(2),
            })
            .unwrap();
        store
            .standing_rule_append(&StandingRule {
                id: StandingRuleId::new(),
                node_id: Some("secretary".into()),
                rule: "pegasus のジョブは 1 ノードで始めてよい".into(),
                created_at: now - time::Duration::minutes(1),
            })
            .unwrap();
        // 他ノード宛ての規則は混ざらない。
        store
            .standing_rule_append(&StandingRule {
                id: StandingRuleId::new(),
                node_id: Some("coding-poc".into()),
                rule: "coding-poc だけの規則".into(),
                created_at: now,
            })
            .unwrap();

        task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            None,
            "やあ",
            &[],
            &[],
            task_core::CONVERSATION_GENRE,
            now,
        )
        .unwrap();

        let seen = Arc::new(StdMutex::new(None));
        let adapter = Arc::new(PersonAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            seen: seen.clone(),
            memory: None,
            proposals: Vec::new(),
        });
        let mut d = person_dispatcher(store.clone(), adapter, workspace_root, None);
        run_until_idle(&mut d, 40).await;

        let context = seen.lock().unwrap().clone().expect("the run happened");
        assert_eq!(
            context.standing_rules,
            vec![
                "深夜は連絡しない".to_string(),
                "pegasus のジョブは 1 ノードで始めてよい".to_string(),
            ],
            "全員向け + secretary 向けだけ（他ノード宛ては混ざらない）"
        );
        let preamble = task_worker::preamble::render(&context, "artifacts");
        assert!(preamble.contains("永続の認可"), "{preamble}");
        assert!(preamble.contains("深夜は連絡しない"), "{preamble}");
        assert!(
            preamble.contains("pegasus のジョブは 1 ノードで始めてよい"),
            "{preamble}"
        );
        assert!(!preamble.contains("coding-poc だけの規則"), "{preamble}");
    }

    // ---- Phase 33（ADR-0033 D4 追記。実機の事故の再発防止）: 担当は自分の仕事を知っている ----

    /// `assignee` の仕事を 1 件作る（`kind = execute`、`Check::Human` はダミー。テストが `status` /
    /// `project_id` / `updated_at` を直接指定できるようにするだけの下請け）。
    fn work_task(
        title: &str,
        status: Status,
        assignee: &str,
        project_id: Option<ProjectId>,
        updated_at: OffsetDateTime,
    ) -> Task {
        let dir = std::path::PathBuf::from("/nonexistent");
        let mut t = new_task(&dir, Check::Human, 0);
        t.title = title.to_string();
        t.status = status;
        t.assignee = Some(assignee.to_string());
        t.project_id = project_id;
        t.updated_at = updated_at;
        t
    }

    fn titled_project(title: &str) -> Project {
        let now = OffsetDateTime::now_utc();
        Project {
            archived_at: None,
            paused_from: None,
            id: ProjectId::new(),
            title: title.to_string(),
            request: "r".into(),
            status: ProjectStatus::Active,
            secretary_summary: None,
            workspace: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Phase 33 受け入れ 1: `run_extras` は対話 run にだけ、担当の直近の仕事を
    /// 「更新の新しい順・案件優先・裏方（対話・まとめ・承認・レビュー）除外・最大 10 件」で集める。
    #[test]
    fn run_extras_recent_work_orders_by_project_then_recency_excludes_support_and_caps_at_ten() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        let base = OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap();

        let project_a = titled_project("案件A");
        store.project_create(&project_a).unwrap();
        let project_b = titled_project("案件B");
        store.project_create(&project_b).unwrap();

        // 案件 A の仕事 6 件（t=100..105。新しい順は A5, A4, ..., A0）。
        let mut a_ids = Vec::new();
        for i in 0..6i64 {
            let t = work_task(
                &format!("A{i}"),
                Status::Done,
                "research-survey",
                Some(project_a.id),
                base + time::Duration::seconds(100 + i),
            );
            store.insert(&t).unwrap();
            a_ids.push(t.id);
        }
        // 案件 B の仕事 6 件（t=200..205）。
        let mut b_ids = Vec::new();
        for i in 0..6i64 {
            let t = work_task(
                &format!("B{i}"),
                Status::Done,
                "research-survey",
                Some(project_b.id),
                base + time::Duration::seconds(200 + i),
            );
            store.insert(&t).unwrap();
            b_ids.push(t.id);
        }
        // 裏方タスク（承認）は一番新しいが除外される。
        let mut support = work_task(
            "approval",
            Status::Ready,
            "research-survey",
            Some(project_a.id),
            base + time::Duration::seconds(999),
        );
        support.kind = TaskKind::Approval;
        store.insert(&support).unwrap();
        // まとめ（圧縮）役割も裏方として除外される。
        let mut compaction = work_task(
            "compaction",
            Status::Done,
            "research-survey",
            Some(project_a.id),
            base + time::Duration::seconds(998),
        );
        compaction.role = Some(task_core::COMPACTION_ROLE.to_string());
        store.insert(&compaction).unwrap();
        // 他の担当の仕事は一番新しいが除外される。
        let other_assignee = work_task(
            "other",
            Status::Done,
            "research-data",
            None,
            base + time::Duration::seconds(999),
        );
        store.insert(&other_assignee).unwrap();

        let adapter: Arc<dyn WorkerAdapter> = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let dir = tempfile::tempdir().unwrap();
        let d = person_dispatcher(store.clone(), adapter, dir.path().to_path_buf(), None);

        // 案件 A を選んでいる対話。
        let mut conv = work_task(
            "conversation",
            Status::Ready,
            "research-survey",
            Some(project_a.id),
            base + time::Duration::seconds(1000),
        );
        conv.conversation = Some(task_core::MessageId::new());
        store.insert(&conv).unwrap();

        let extras = d.run_extras(&conv, None, None, "claude-code").unwrap();
        assert_eq!(
            extras.recent_work.len(),
            10,
            "capped at 10: {:?}",
            extras.recent_work
        );
        let ids: Vec<TaskId> = extras.recent_work.iter().map(|w| w.task_id).collect();
        let mut expected_a = a_ids.clone();
        expected_a.reverse();
        let mut expected_b_top4 = b_ids.clone();
        expected_b_top4.reverse();
        expected_b_top4.truncate(4);
        let mut expected = expected_a;
        expected.extend(expected_b_top4);
        assert_eq!(ids, expected, "案件 A が先、残りは更新の新しい順");
        assert!(!ids.contains(&support.id), "承認は裏方なので除外");
        assert!(!ids.contains(&compaction.id), "まとめは裏方なので除外");
        assert!(
            !ids.contains(&other_assignee.id),
            "他の担当の仕事は含めない"
        );
    }

    // ---- Phase 41（ADR-0038 D1）: 途中目標レビューの対話 run ----

    /// 受け入れ 1: レビューの対話 run（対話の印 + `milestone_id`）には、その途中目標と、
    /// 属する仕事（裏方は除く）の title / status / 終端の要約 / 成果物の抜粋が渡る。
    #[test]
    fn run_extras_fills_the_milestone_review_context_with_results_and_artifact_excerpts() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        let base = OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap();
        let project = titled_project("Pluvio");
        store.project_create(&project).unwrap();
        let milestone = store
            .milestone_create(
                project.id,
                "隣接領域の動向調査",
                "近い分野を洗う",
                MilestoneStatus::InProgress,
            )
            .unwrap();

        // 成果物を持つ done の仕事（`answer.md` を書いてある）。
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("survey");
        std::fs::create_dir_all(ws.join("artifacts")).unwrap();
        std::fs::write(ws.join("artifacts/answer.md"), "候補 A / 候補 B / 候補 C\n").unwrap();
        let mut done = work_task(
            "web 調査",
            Status::Done,
            "research-survey",
            Some(project.id),
            base,
        );
        done.workspace = WorkspaceSpec::Local {
            path: ws.clone(),
            mode: None,
        };
        done.milestone_id = Some(milestone.id);
        store.insert(&done).unwrap();
        store
            .append_event(
                done.id,
                &Event::WorkerFinished {
                    run_id: "r-1".into(),
                    outcome: "done: 候補を 3 本に絞った".into(),
                    usage: None,
                    role: None,
                    metrics: None,
                    end: None,
                },
            )
            .unwrap();
        // Go 待ちの draft も文脈に入る。
        let mut draft = work_task(
            "候補の比較",
            Status::Draft,
            "research-survey",
            Some(project.id),
            base + time::Duration::seconds(10),
        );
        draft.milestone_id = Some(milestone.id);
        store.insert(&draft).unwrap();
        // 裏方（承認）は入らない。
        let mut support = work_task(
            "approval",
            Status::Done,
            "research-survey",
            Some(project.id),
            base + time::Duration::seconds(20),
        );
        support.kind = TaskKind::Approval;
        support.milestone_id = Some(milestone.id);
        store.insert(&support).unwrap();

        let adapter: Arc<dyn WorkerAdapter> = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let d = person_dispatcher(store.clone(), adapter, dir.path().to_path_buf(), None);

        let started = task_ops::milestone_review::start_review(
            store.as_ref(),
            &project,
            &milestone,
            "途中目標『隣接領域の動向調査』の仕事が止まりました。",
            &[],
            &d.config.genres.clone(),
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap();

        let extras = d
            .run_extras(&started.task, None, None, "claude-code")
            .unwrap();
        let review = extras
            .milestone_review
            .expect("the review context is filled");
        assert_eq!(review.milestone.title, "隣接領域の動向調査");
        assert_eq!(review.milestone.status, "in_progress");
        assert_eq!(review.tasks.len(), 2, "裏方は入らない: {:?}", review.tasks);
        assert_eq!(review.tasks[0].title, "web 調査");
        assert_eq!(
            review.tasks[0].outcome.as_deref(),
            Some("候補を 3 本に絞った")
        );
        assert!(
            review.tasks[0]
                .artifacts_excerpt
                .contains("候補 A / 候補 B / 候補 C"),
            "{:?}",
            review.tasks[0]
        );
        assert!(
            review.tasks[0].artifacts_excerpt.contains("answer.md"),
            "{:?}",
            review.tasks[0]
        );
        assert_eq!(review.tasks[1].title, "候補の比較");
        assert_eq!(review.tasks[1].status, Status::Draft);
        // 前置きにも出る。
        let context = RunContext {
            milestone_review: Some(review),
            ..RunContext::default()
        };
        let preamble = task_worker::preamble::render(&context, "artifacts");
        assert!(
            preamble.contains("## 途中目標『隣接領域の動向調査』のここまで"),
            "{preamble}"
        );
        assert!(preamble.contains("候補 A / 候補 B / 候補 C"), "{preamble}");

        // 普通の対話 run（`milestone_id` 無し）には何も渡らない。
        let plain = task_ops::conversation::start(
            store.as_ref(),
            "secretary",
            Some(project.id),
            "やあ",
            &[],
            &d.config.genres.clone(),
            task_core::CONVERSATION_GENRE,
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        assert!(
            d.run_extras(&plain.task, None, None, "claude-code")
                .unwrap()
                .milestone_review
                .is_none()
        );
    }

    /// 受け入れ 1: 対話 run の結果ファイルの `milestone_proposal` から `proposed` の途中目標が 1 件できる
    /// （無ければ作らない。判定中の途中目標は差し替えの対象にしない）。
    #[test]
    fn absorb_milestone_proposal_records_the_next_milestone_from_the_result_file() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        let project = titled_project("Pluvio");
        store.project_create(&project).unwrap();
        let milestone = store
            .milestone_create(
                project.id,
                "隣接領域の動向調査",
                "",
                MilestoneStatus::InProgress,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("review");
        std::fs::create_dir_all(ws.join("artifacts")).unwrap();
        let adapter: Arc<dyn WorkerAdapter> = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let d = person_dispatcher(store.clone(), adapter, dir.path().to_path_buf(), None);

        let mut task = work_task(
            "対話",
            Status::Done,
            "secretary",
            Some(project.id),
            OffsetDateTime::now_utc(),
        );
        task.workspace = WorkspaceSpec::Local {
            path: ws.clone(),
            mode: None,
        };
        task.milestone_id = Some(milestone.id);
        task.conversation = Some(task_core::MessageId::new());
        store.insert(&task).unwrap();

        // 結果ファイルが無ければ何も作らない。
        d.absorb_milestone_proposal(&task);
        assert_eq!(store.milestone_list(project.id).unwrap().len(), 1);

        std::fs::write(
            ws.join("artifacts/result.json"),
            r#"{"summary":"結果","milestone_proposal":{"title":"候補の絞り込み","description":"3 本に"}}"#,
        )
        .unwrap();
        d.absorb_milestone_proposal(&task);
        let all = store.milestone_list(project.id).unwrap();
        assert_eq!(all.len(), 2, "{all:?}");
        let proposal = all
            .iter()
            .find(|m| m.title == "候補の絞り込み")
            .expect("the proposal");
        assert_eq!(proposal.status, MilestoneStatus::Proposed);
        assert_eq!(proposal.description, "3 本に");
        // 判定中の途中目標はそのまま。
        assert_eq!(
            all.iter().find(|m| m.id == milestone.id).map(|m| m.status),
            Some(MilestoneStatus::InProgress)
        );

        // 2 回目の提案は 1 回目を差し替える（`proposed` は常に 1 件）。
        std::fs::write(
            ws.join("artifacts/result.json"),
            r#"{"summary":"結果","milestone_proposal":{"title":"実験計画","description":""}}"#,
        )
        .unwrap();
        d.absorb_milestone_proposal(&task);
        let all = store.milestone_list(project.id).unwrap();
        assert_eq!(
            all.iter()
                .filter(|m| m.status == MilestoneStatus::Proposed)
                .count(),
            1,
            "{all:?}"
        );
        assert_eq!(
            all.iter().find(|m| m.id == proposal.id).map(|m| m.status),
            Some(MilestoneStatus::Redesigned)
        );
    }

    /// ADR-0048 D3（Phase 60b）: CoS（`secretary` = `OrgKind::Secretary`）の対話 run の結果ファイルの
    /// `actions` から `create_task` が実行され、担当なし・`ready` のタスクができる。実行できなかった
    /// action があれば理由が `failed` に残り、`ActionsOutcome::to_metadata` が `Some` になる。
    /// CoS 以外の対話・対話でない run では何もしない。
    #[test]
    fn absorb_console_actions_executes_the_declared_actions_for_the_cos_only() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("console");
        std::fs::create_dir_all(ws.join("artifacts")).unwrap();
        let adapter: Arc<dyn WorkerAdapter> = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let mut d = person_dispatcher(store.clone(), adapter, dir.path().to_path_buf(), None);
        d.config.genres.push(GenreSpec {
            id: "coding".into(),
            description: "コードを直す".into(),
            ..GenreSpec::default()
        });

        let mut task = work_task(
            "対話",
            Status::Done,
            "secretary",
            None,
            OffsetDateTime::now_utc(),
        );
        task.workspace = WorkspaceSpec::Local {
            path: ws.clone(),
            mode: None,
        };
        task.conversation = Some(task_core::MessageId::new());
        store.insert(&task).unwrap();

        // 結果ファイルが無ければ何もしない。
        assert!(d.absorb_console_actions(&task, "run-1").is_none());
        assert_eq!(store.list(None).unwrap().len(), 1, "対話タスク自身だけ");

        // 有効な action ＋ 検証に落ちる action の混在。
        std::fs::write(
            ws.join("artifacts/result.json"),
            r#"{"summary":"やります","actions":[
                {"type":"create_task","title":"直す","objective":"直して","acceptance":["直った"],"harness":"coding"},
                {"type":"add_milestone","project":"01ZZZZZZZZZZZZZZZZZZZZZZZZ","title":"存在しない案件"}
            ]}"#,
        )
        .unwrap();
        let outcome = d
            .absorb_console_actions(&task, "run-1")
            .expect("actions were declared");
        assert_eq!(outcome.executed.len(), 1);
        assert_eq!(outcome.failed.len(), 1);
        let created = outcome.executed[0].task_id.expect("task id");
        let stored = store.get(created).unwrap().expect("task");
        assert_eq!(stored.status, Status::Ready);
        assert_eq!(stored.assignee, None, "matching は別経路（次 tick）");
        assert!(outcome.to_metadata().is_some());
        assert!(
            outcome
                .failure_note()
                .expect("failure note")
                .contains("実行できなかった action")
        );

        // 同じ run の 2 回目は何もしない（冪等）。
        assert!(d.absorb_console_actions(&task, "run-1").is_none());
        assert_eq!(
            store.list(None).unwrap().len(),
            2,
            "重複してタスクが増えない"
        );

        // CoS 以外（`research-survey`）宛ての対話には何もしない。
        let mut other = work_task(
            "対話",
            Status::Done,
            "research-survey",
            None,
            OffsetDateTime::now_utc(),
        );
        other.workspace = WorkspaceSpec::Local {
            path: ws.clone(),
            mode: None,
        };
        other.conversation = Some(task_core::MessageId::new());
        assert!(d.absorb_console_actions(&other, "run-2").is_none());

        // 対話でない run には何もしない。
        let mut ordinary = other.clone();
        ordinary.conversation = None;
        assert!(d.absorb_console_actions(&ordinary, "run-3").is_none());
    }

    /// ADR-0054 Phase 112 D3（D4(c)）: `task-worker::codex` の「result.json が無いが最終メッセージが
    /// その形（`summary`+`actions`）なら回収する」救済（`final_message_is_recoverable_result`）は、
    /// 回収したテキストをそのまま `<artifacts_dir>/result.json` として disk に書く（task-worker 側の
    /// 単体テスト `codex::tests::phase_112_a_recoverable_final_message_is_written_as_result_json_and_its_summary_is_used`
    /// で確認済み）。ここでは「書かれた後」を検査する: 書かれた `result.json` に対して
    /// `absorb_console_actions` が通常どおり動き、`create_task` action が実行されてタスクが `ready` で
    /// 作られることを確認する（`absorb_console_actions` は disk を読むだけで、その内容が本来モデルが
    /// 書くはずだったものか、D3 の救済で回収されたものかを区別しない — それがこの救済の狙いそのもの）。
    #[test]
    fn absorb_console_actions_executes_actions_recovered_from_the_final_message() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("console");
        std::fs::create_dir_all(ws.join("artifacts")).unwrap();
        let adapter: Arc<dyn WorkerAdapter> = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let mut d = person_dispatcher(store.clone(), adapter, dir.path().to_path_buf(), None);
        d.config.genres.push(GenreSpec {
            id: "coding".into(),
            description: "コードを直す".into(),
            ..GenreSpec::default()
        });

        let mut task = work_task(
            "対話",
            Status::Done,
            "secretary",
            None,
            OffsetDateTime::now_utc(),
        );
        task.workspace = WorkspaceSpec::Local {
            path: ws.clone(),
            mode: None,
        };
        task.conversation = Some(task_core::MessageId::new());
        store.insert(&task).unwrap();

        // 最終メッセージから回収された `result.json`（codex.rs が `final_message_is_recoverable_result`
        // で判定してそのまま書き込んだものと同じ形）。
        std::fs::write(
            ws.join("artifacts/result.json"),
            r#"{"summary":"直すタスクを作りました","actions":[
                {"type":"create_task","title":"直す","objective":"直して","acceptance":["直った"],"harness":"coding"}
            ]}"#,
        )
        .unwrap();
        let outcome = d
            .absorb_console_actions(&task, "run-1")
            .expect("actions were declared");
        assert_eq!(outcome.executed.len(), 1, "{:?}", outcome.executed);
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        let created = outcome.executed[0].task_id.expect("task id");
        let stored = store.get(created).unwrap().expect("task");
        assert_eq!(stored.status, Status::Ready);
        assert_eq!(stored.title, "直す");
    }

    /// ADR-0048 D3: `record_conversation_reply` は `done` な CoS の返事に actions を実行し、
    /// 実行できなかった action を本文に足し、実行結果を `Message.metadata` に残す。
    #[test]
    fn record_conversation_reply_runs_actions_and_attaches_the_result() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());

        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("console");
        std::fs::create_dir_all(ws.join("artifacts")).unwrap();
        let adapter: Arc<dyn WorkerAdapter> = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let mut d = person_dispatcher(store.clone(), adapter, dir.path().to_path_buf(), None);
        d.config.genres.push(GenreSpec {
            id: "coding".into(),
            description: "コードを直す".into(),
            ..GenreSpec::default()
        });

        let mut task = work_task(
            "対話",
            Status::Done,
            "secretary",
            None,
            OffsetDateTime::now_utc(),
        );
        task.workspace = WorkspaceSpec::Local {
            path: ws.clone(),
            mode: None,
        };
        task.conversation = Some(task_core::MessageId::new());
        store.insert(&task).unwrap();

        std::fs::write(
            ws.join("artifacts/result.json"),
            r#"{"summary":"やります","actions":[{"type":"create_task","title":"直す","objective":"直して","acceptance":["直った"],"harness":"coding"}]}"#,
        )
        .unwrap();

        d.record_conversation_reply(&task, "run-1", "done: やります", Status::Done);
        let messages = store.message_list("secretary", None, 10).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].text.starts_with("やります"));
        let metadata = messages[0].metadata.as_ref().expect("metadata");
        assert_eq!(metadata.actions_executed.len(), 1);
        assert!(metadata.actions_executed[0].summary.contains("直す"));
        assert!(metadata.actions_failed.is_empty());
    }

    /// Phase 33 受け入れ 1: 通常の run（対話でない）では `recent_work` は常に空
    /// （担当がいても、他に仕事があっても）。
    #[test]
    fn run_extras_recent_work_is_empty_for_ordinary_runs() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        let base = OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap();
        let done = work_task("done work", Status::Done, "research-survey", None, base);
        store.insert(&done).unwrap();

        let adapter: Arc<dyn WorkerAdapter> = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let dir = tempfile::tempdir().unwrap();
        let d = person_dispatcher(store.clone(), adapter, dir.path().to_path_buf(), None);

        let ordinary = work_task(
            "ordinary",
            Status::Ready,
            "research-survey",
            None,
            base + time::Duration::seconds(1),
        );
        store.insert(&ordinary).unwrap();
        let extras = d.run_extras(&ordinary, None, None, "claude-code").unwrap();
        assert!(extras.recent_work.is_empty(), "{:?}", extras.recent_work);
    }

    /// Phase 33 受け入れ 2: `outcome` は Phase 25 の報告の組み立て（`task_core::report`）を流用して
    /// 決定的に作る — `done` は summary の 1 行目、`failed` は直近のレビュー不合格の理由かワーカーの
    /// エラー（`web search returned nothing` / `idle timeout` を含む。より後のイベントが勝つ）、
    /// それも無ければ `Failed` への遷移理由、`blocked` は直近の質問。
    #[test]
    fn recent_work_outcome_reuses_the_report_wording_for_done_failed_and_blocked() {
        let done_events = vec![(
            0u64,
            Event::WorkerFinished {
                run_id: "r1".into(),
                outcome:
                    "done: Pluvio と比較可能な非同期ランタイムを 3 件確認した\n詳細は成果物を参照"
                        .into(),
                usage: None,
                role: None,
                metrics: None,
                end: None,
            },
        )];
        let done_task = Task {
            status: Status::Done,
            ..new_task(std::path::Path::new("/nonexistent"), Check::Human, 0)
        };
        assert_eq!(
            recent_work_outcome(&done_task, &done_events),
            Some("Pluvio と比較可能な非同期ランタイムを 3 件確認した".to_string())
        );

        let idle_timeout_events = vec![(
            0u64,
            Event::WorkerFinished {
                run_id: "r1".into(),
                outcome: "error(retryable=true): idle timeout".into(),
                usage: None,
                role: None,
                metrics: None,
                end: None,
            },
        )];
        let failed_task = Task {
            status: Status::Failed,
            ..new_task(std::path::Path::new("/nonexistent"), Check::Human, 0)
        };
        assert_eq!(
            recent_work_outcome(&failed_task, &idle_timeout_events),
            Some("idle timeout".to_string())
        );

        let search_nothing_events = vec![(
            0u64,
            Event::WorkerFinished {
                run_id: "r1".into(),
                outcome: "error(retryable=true): web search returned nothing (possible search path failure: \
                          expired key, CAPTCHA, or network block)"
                    .into(),
                usage: None,
                role: None,
                metrics: None,
                end: None,
            },
        )];
        assert_eq!(
            recent_work_outcome(&failed_task, &search_nothing_events),
            Some(
                "web search returned nothing (possible search path failure: expired key, CAPTCHA, or network block)"
                    .to_string()
            )
        );

        // レビュー不合格は、その前のワーカーの `done` より優先される（より後のイベントだから）。
        let review_failed_events = vec![
            (
                0u64,
                Event::WorkerFinished {
                    run_id: "r1".into(),
                    outcome: "done: 一見よさそう".into(),
                    usage: None,
                    role: None,
                    metrics: None,
                    end: None,
                },
            ),
            (
                1u64,
                Event::ReviewVerdict {
                    run_id: "r1".into(),
                    criterion_idx: 0,
                    pass: false,
                    reason: "rejected: evidence missing".into(),
                },
            ),
        ];
        assert_eq!(
            recent_work_outcome(&failed_task, &review_failed_events),
            Some("rejected: evidence missing".to_string())
        );

        // どちらも無ければ `Failed` への遷移理由にフォールバックする。
        let fallback_events = vec![(
            0u64,
            Event::Transitioned {
                from: Status::Reviewing,
                to: Status::Failed,
                reason: "child_failed".into(),
            },
        )];
        assert_eq!(
            recent_work_outcome(&failed_task, &fallback_events),
            Some("child_failed".to_string())
        );

        let question_events = vec![(
            0u64,
            Event::QuestionRaised {
                run_id: "r1".into(),
                text: "どちらの案で進めますか？".into(),
            },
        )];
        let blocked_task = Task {
            status: Status::Blocked,
            ..new_task(std::path::Path::new("/nonexistent"), Check::Human, 0)
        };
        assert_eq!(
            recent_work_outcome(&blocked_task, &question_events),
            Some("どちらの案で進めますか？".to_string())
        );

        // 進行中のタスクには要約を出さない。
        let running_task = Task {
            status: Status::Running,
            ..new_task(std::path::Path::new("/nonexistent"), Check::Human, 0)
        };
        assert_eq!(recent_work_outcome(&running_task, &done_events), None);
    }

    /// Phase 33 受け入れ 3: `recent_work` の各行に案件名・成果物名・終了時刻も乗る。
    #[test]
    fn run_extras_recent_work_carries_project_title_and_artifacts() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        seed_conversation_org(store.as_ref());
        let base = OffsetDateTime::from_unix_timestamp(1_760_000_000).unwrap();
        let project = titled_project("Pluvio の新テーマ");
        store.project_create(&project).unwrap();

        let done = work_task(
            "先行研究のまとめ",
            Status::Done,
            "research-survey",
            Some(project.id),
            base,
        );
        store.insert(&done).unwrap();
        store
            .append_event(
                done.id,
                &Event::WorkerFinished {
                    run_id: "r1".into(),
                    outcome: "done: Pluvio と比較可能な非同期ランタイムを 3 件確認した".into(),
                    usage: None,
                    role: None,
                    metrics: None,
                    end: None,
                },
            )
            .unwrap();
        store
            .append_event(
                done.id,
                &Event::ArtifactProduced {
                    run_id: "r1".into(),
                    artifact: ArtifactRef {
                        name: "survey.md".into(),
                        path: "artifacts/survey.md".into(),
                        sha256: String::new(),
                        kind: "text".into(),
                        declared: true,
                    },
                },
            )
            .unwrap();

        let adapter: Arc<dyn WorkerAdapter> = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let dir = tempfile::tempdir().unwrap();
        let d = person_dispatcher(store.clone(), adapter, dir.path().to_path_buf(), None);

        let mut conv = work_task(
            "conversation",
            Status::Ready,
            "research-survey",
            Some(project.id),
            base + time::Duration::seconds(1),
        );
        conv.conversation = Some(task_core::MessageId::new());
        store.insert(&conv).unwrap();

        let extras = d.run_extras(&conv, None, None, "claude-code").unwrap();
        assert_eq!(extras.recent_work.len(), 1);
        let w = &extras.recent_work[0];
        assert_eq!(w.task_id, done.id);
        assert_eq!(w.title, "先行研究のまとめ");
        assert_eq!(w.project_title.as_deref(), Some("Pluvio の新テーマ"));
        assert_eq!(w.status, Status::Done);
        assert!(w.finished_at.is_some());
        assert_eq!(
            w.outcome.as_deref(),
            Some("Pluvio と比較可能な非同期ランタイムを 3 件確認した")
        );
        assert_eq!(w.artifacts, vec!["survey.md".to_string()]);
    }

    // ---- Phase 49（ADR-0041 D1）: ローカルの作業場所もタスクごとに worktree ----

    /// テスト用の git リポジトリ（`main` に 1 コミット）。返すのは `main` の sha。
    fn init_test_repo(dir: &std::path::Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::fs::write(dir.join("README.md"), b"hello\n").unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", "first"]] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        git_out(dir, &["rev-parse", "HEAD"])
    }

    /// `git -C <dir> <args...>` の stdout（trim 済み）。失敗したら panic。
    fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    // ---- ADR-0044 D2（Phase 53）: 人のコメントによる割り込み ----

    /// 走り続けて、渡された `RunContext` を記録するアダプタ。`hold` の間は終わらない
    /// （1 回目の run を「走っている」状態にするため）。2 回目以降はすぐ `done` になる。
    struct InterruptProbeAdapter {
        seen: Arc<StdMutex<Vec<task_worker::RunContext>>>,
        hold: Duration,
    }

    #[async_trait]
    impl WorkerAdapter for InterruptProbeAdapter {
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
            let first = match self.seen.lock() {
                Ok(mut seen) => {
                    seen.push(req.context.clone());
                    seen.len() == 1
                }
                Err(_) => false,
            };
            if first {
                // 1 回目: 人が割り込むまで走り続ける（tick がこの run を abort する）。
                sink.progress("working");
                tokio::time::sleep(self.hold).await;
            }
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: "ok".into(),
                    evidence: vec![],
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    /// ADR-0044 D2 / D8: 走っている run に人がコメントすると、
    /// 1. タスクは `ready` に戻り（attempts 据え置き、`Transitioned{reason:"comment"}`）、
    /// 2. `WorkerFinished{outcome:"interrupted: comment"}` が残り（失敗ではないので報告は作らない）、
    /// 3. ディスパッチャが次の tick でその run を止め（cancel と同じ `abort_stale_runs` の経路）、
    /// 4. **次の run の前置きの先頭**にそのコメントが「人からの割り込み」として載る。
    #[tokio::test]
    async fn a_human_comment_interrupts_the_running_run_and_the_next_run_carries_it() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(dir.path(), Check::Human, 2);
        task.attempts = 1;
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(InterruptProbeAdapter {
            seen: seen.clone(),
            hold: Duration::from_secs(30),
        });
        let mut d = dispatcher(store.clone(), adapter, 1);

        // 1 tick で dispatch し、run が**実際に走り出す**まで待つ（spawn されただけでは前置きは組まれない）。
        d.tick().unwrap();
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Running);
        assert_eq!(d.running.len(), 1, "run が手元で走っている");
        for _ in 0..50 {
            if seen.lock().map(|s| !s.is_empty()).unwrap_or(false) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(seen.lock().unwrap().len(), 1, "1 回目の run が始まっている");

        // 人がコメントする（API と同じ経路）。
        let result = task_ops::comment::post_human_comment(
            store.as_ref(),
            task.id,
            "方針を変えたい。まず設計を書いて".into(),
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        assert_eq!(result.effect, task_ops::comment::CommentEffect::Interrupted);
        let after = store.get(task.id).unwrap().unwrap();
        assert_eq!(after.status, Status::Ready);
        assert_eq!(after.attempts, 1, "割り込みは試行を消費しない");
        let events = store.events_for(task.id).unwrap();
        assert!(
            events
                .iter()
                .any(|(_, e)| matches!(e, Event::WorkerFinished { outcome, .. } if outcome == "interrupted: comment"))
        );

        // 次の tick で走っていた run が止まり、同じ tick で走り直す。
        d.tick().unwrap();
        assert!(
            !d.running.contains_key(&task.id)
                || store.get(task.id).unwrap().unwrap().status == Status::Running,
            "古い run は捨てられている"
        );
        // 走り直した run の前置きに割り込みが載るまで回す。
        for _ in 0..40 {
            if seen.lock().map(|s| s.len() >= 2).unwrap_or(false) {
                break;
            }
            d.tick().unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let contexts = seen.lock().unwrap().clone();
        assert!(
            contexts.len() >= 2,
            "2 回目の run が始まっていない: {}",
            contexts.len()
        );
        let second = &contexts[1];
        assert_eq!(
            second.interrupt.as_deref(),
            Some("方針を変えたい。まず設計を書いて"),
            "次の run に割り込みが渡る"
        );
        assert_eq!(second.comments.len(), 1);
        assert_eq!(
            second.comments[0].author_kind,
            task_core::CommentAuthorKind::Human
        );
        assert!(second.comments_enabled, "ワーカー run はコメントを書ける");
        assert!(contexts[0].interrupt.is_none(), "1 回目には割り込みが無い");

        // 前置きの先頭に「人からの割り込み」として出る。
        let preamble = task_worker::preamble::render(second, "artifacts");
        assert!(preamble.starts_with("## コメント"), "{preamble}");
        assert!(
            preamble.contains("**人からの割り込み**: 方針を変えたい。まず設計を書いて"),
            "{preamble}"
        );
        assert!(
            preamble.contains("短い進捗や判断の記録はコメントに書け"),
            "{preamble}"
        );
    }

    /// ADR-0044 D2: ワーカーの `{"type":"comment"}` 行は `author_kind = node` で残り、状態は変えない。
    #[tokio::test]
    async fn a_worker_comment_is_recorded_as_a_node_comment_without_touching_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.assignee = Some("impl".into());
        store.insert(&task).unwrap();
        let adapter = Arc::new(CommentingAdapter);
        let mut d = dispatcher(store.clone(), adapter, 1);
        run_until_idle(&mut d, 20).await;

        let comments = store.comments_for(task.id).unwrap();
        assert_eq!(comments.len(), 1, "{comments:?}");
        assert_eq!(comments[0].author_kind, task_core::CommentAuthorKind::Node);
        assert_eq!(comments[0].author.as_deref(), Some("impl"));
        assert_eq!(comments[0].body, "ビルドが通った");
        assert!(comments[0].run_id.is_some(), "run に紐づく");
        assert_eq!(
            store.get(task.id).unwrap().unwrap().status,
            Status::Done,
            "状態は変えない"
        );
    }

    /// `comment` を 1 行出してから終わるアダプタ。
    struct CommentingAdapter;

    #[async_trait]
    impl WorkerAdapter for CommentingAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            _req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            sink.comment("ビルドが通った");
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: "ok".into(),
                    evidence: vec![],
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    /// 走った run の cwd / workspace / artifacts_dir を記録し、`files` を cwd に作るアダプタ。
    struct RecordingAdapter {
        seen: Arc<StdMutex<Vec<(PathBuf, PathBuf, PathBuf)>>>,
        files: Vec<String>,
    }

    #[async_trait::async_trait]
    impl WorkerAdapter for RecordingAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push((
                    req.cwd().to_path_buf(),
                    req.workspace.clone(),
                    req.artifacts_dir.clone(),
                ));
            }
            for name in &self.files {
                std::fs::write(req.cwd().join(name), b"x").unwrap();
            }
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: "ok".into(),
                    evidence: vec![],
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    /// `workspace_root` を実体のあるディレクトリにした dispatcher（既定の `/nonexistent` では worktree を作れない）。
    fn worktree_dispatcher(
        store: Arc<dyn TaskStore>,
        adapter: Arc<dyn WorkerAdapter>,
        workspace_root: &std::path::Path,
        releases_dir: Option<PathBuf>,
    ) -> Dispatcher {
        let mut d = dispatcher(store, adapter, 1);
        d.config.workspace_root = workspace_root.to_path_buf();
        d.config.releases_dir = releases_dir;
        d
    }

    fn git_task(
        repo: &std::path::Path,
        mode: Option<task_core::WorkspaceMode>,
        check: Check,
    ) -> Task {
        let mut task = new_task(repo, check, 0);
        task.workspace = WorkspaceSpec::Local {
            path: repo.to_path_buf(),
            mode,
        };
        task
    }

    /// ADR-0041 D1: ローカルの git リポジトリは、タスクごとの worktree
    /// （`<workspace_root>/<task_id>/tree`、ブランチ `celeris/<task_id>`、base は `main`）で動く。
    /// 成果物・`runs/` は作業ツリーの**外**（`<workspace_root>/<task_id>/`）。
    #[tokio::test]
    async fn a_local_git_workspace_runs_in_a_per_task_worktree_on_its_own_branch() {
        let repo_dir = tempfile::tempdir().unwrap();
        let main_sha = init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        // 判定コマンドも worktree の中で走る（ADR-0019 D1 6.）: アダプタが cwd に置いたファイルが見える。
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "test -f in-tree".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen: seen.clone(),
            files: vec!["in-tree".into()],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        run_until_idle(&mut d, 60).await;

        let task_dir = root.path().join(task.id.to_string());
        let tree = task_dir.join("tree");
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
        assert!(
            tree.join(".git").exists(),
            "worktree at <workspace_root>/<task_id>/tree"
        );
        assert!(
            tree.join("README.md").is_file(),
            "追跡ファイルが checkout されている"
        );
        // ワーカーの cwd は worktree、`workspace`（= `runs/` の親）と成果物はその外。
        let runs = seen.lock().unwrap().clone();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].0.canonicalize().unwrap(),
            tree.canonicalize().unwrap(),
            "cwd は worktree"
        );
        assert_eq!(
            runs[0].1.canonicalize().unwrap(),
            task_dir.canonicalize().unwrap(),
            "workspace は worktree の親"
        );
        assert_eq!(
            runs[0].2,
            task_dir.canonicalize().unwrap().join("artifacts"),
            "成果物は作業ツリーの外"
        );
        assert!(task_dir.join("artifacts").is_dir());
        assert!(task_dir.join("runs").is_dir());
        assert!(
            !tree.join("runs").exists(),
            "`runs/` を作業ツリーに作らない（git status を汚さない）"
        );
        // ブランチは `celeris/<task_id>` で、base は `main`。celeris はコミットしない。
        let branch = format!("celeris/{}", task.id);
        assert!(git_ok(
            repo_dir.path(),
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}")
            ]
        ));
        assert_eq!(
            git_out(&tree, &["rev-parse", "HEAD"]),
            main_sha,
            "base は main"
        );
        assert_eq!(
            git_out(&tree, &["rev-parse", "--abbrev-ref", "HEAD"]),
            branch
        );
        // 目印（API / CLI が「run のログは作業ツリーの外」と判断するのに使う）。
        let marker = task_ops::workspace::read_marker(&task_dir).expect("worktree.json");
        assert_eq!(marker.branch, branch);
        assert_eq!(marker.base, main_sha);
        assert_eq!(marker.base_kind, "main");
        assert_eq!(
            task_ops::workspace::local_dir(&store.get(task.id).unwrap().unwrap(), root.path()),
            task_dir
        );
    }

    // ---- ADR-0066 D1（Phase 110b）: 共有ビルドキャッシュの `CARGO_TARGET_DIR` ----

    /// `req.context.workspace_note` を記録するだけのアダプタ。
    struct WorkspaceNoteAdapter {
        seen: Arc<StdMutex<Vec<Option<String>>>>,
    }

    #[async_trait::async_trait]
    impl WorkerAdapter for WorkspaceNoteAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            self.seen.lock().unwrap().push(req.context.workspace_note);
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: "ok".into(),
                    evidence: vec![],
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    /// 共有ビルドキャッシュが有効で git のリポジトリがある run の前置きに、`target/` は共有キャッシュに
    /// あるという 1 行が足される。無効なら足されない。
    #[tokio::test]
    async fn the_preamble_notes_the_shared_build_cache_when_enabled() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(WorkspaceNoteAdapter { seen: seen.clone() });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        d.config.shared_build_cache = true;
        run_until_idle(&mut d, 60).await;
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
        let notes = seen.lock().unwrap().clone();
        assert_eq!(notes.len(), 1);
        let note = notes[0].as_deref().unwrap_or_default();
        assert!(
            note.contains("target/") && note.contains("CARGO_TARGET_DIR"),
            "{note}"
        );
    }

    #[tokio::test]
    async fn the_preamble_does_not_note_the_shared_build_cache_when_disabled() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(WorkspaceNoteAdapter { seen: seen.clone() });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        d.config.shared_build_cache = false;
        run_until_idle(&mut d, 60).await;
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
        let notes = seen.lock().unwrap().clone();
        assert_eq!(notes.len(), 1);
        let note = notes[0].as_deref().unwrap_or_default();
        assert!(!note.contains("CARGO_TARGET_DIR"), "{note}");
    }

    /// `[workspace] shared_build_cache`（既定 true）が有効なローカルの git worktree のホスト実行に、
    /// `CARGO_TARGET_DIR=<build_cache_dir>/cargo/<repo-key>` が渡る。
    #[tokio::test]
    async fn shared_build_cache_sets_cargo_target_dir_for_a_local_git_worktree_on_the_host() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let captured: CapturedEnvs = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            }),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured: captured.clone(),
            spawn_failure: false,
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        d.config.shared_build_cache = true;
        d.config.build_cache_dir = cache_dir.path().to_path_buf();
        run_until_idle(&mut d, 60).await;

        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
        let runs = captured.lock().unwrap().clone();
        assert_eq!(runs.len(), 1, "{runs:?}");
        let expected =
            task_worker::build_cache::cargo_target_dir_env(cache_dir.path(), repo_dir.path());
        assert!(
            runs[0].contains(&expected),
            "expected {expected:?} in {:?}",
            runs[0]
        );
    }

    /// `[workspace] shared_build_cache = false` なら `CARGO_TARGET_DIR` は渡らない。
    #[tokio::test]
    async fn shared_build_cache_disabled_does_not_set_cargo_target_dir() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let captured: CapturedEnvs = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            }),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured: captured.clone(),
            spawn_failure: false,
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        d.config.shared_build_cache = false;
        run_until_idle(&mut d, 60).await;

        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
        let runs = captured.lock().unwrap().clone();
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert!(
            !runs[0].iter().any(|(k, _)| k == "CARGO_TARGET_DIR"),
            "{:?}",
            runs[0]
        );
    }

    /// リモート（クラスタ）実行には `CARGO_TARGET_DIR` を渡さない（ADR-0066 D1 は対象外と決めている）。
    /// `run_worker` を直接呼ぶ（`remote_task`/`write_stub_ssh` は既存の Phase 99 のテストと同じ小道具）。
    #[tokio::test]
    async fn shared_build_cache_is_not_applied_to_remote_workspaces() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let stub = write_stub_ssh(tmp.path(), 0);
        let task = remote_task(tmp.path(), Some(WorkspaceMode::Shared), Vec::new());
        store.insert(&task).unwrap();

        let mut settings = SshSettings::new("pegasus", "pegasus", PathBuf::from("/work/proj"));
        settings.sync = SyncMode::None;
        settings.task_id = task.id.to_string();
        settings.ssh_command = vec![stub.to_string_lossy().into_owned()];

        let captured: CapturedEnvs = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(PoolAdapter {
            terminal_or_throttled: Ok(Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            }),
            delay: Duration::ZERO,
            observation: None,
            env: Vec::new(),
            captured: captured.clone(),
            spawn_failure: false,
        });
        let outcome = run_worker(
            store.clone(),
            adapter,
            task.id,
            Tier::Standard,
            tmp.path().join("mirror"),
            "run1",
            RunLimits {
                wall_clock: Duration::from_secs(30),
                idle_timeout: Duration::from_secs(5),
                kill_grace: Duration::from_millis(100),
            },
            LeaseRenewal {
                ttl: Duration::from_secs(60),
                every: Duration::from_secs(30),
            },
            Some(settings),
            None,
            RunExtras::default(),
            Vec::new(),
            Vec::new(),
            DelegationLimits::default(),
            None,
            None,
            ContainerDecision::Host,
            // `Some` のまま渡しても、`remote.is_some()` なので適用されないことを確かめる。
            Some(tmp.path().join("build-cache")),
        )
        .await;
        assert!(outcome.is_ok(), "{:?}", outcome.err());

        let runs = captured.lock().unwrap().clone();
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert!(
            !runs[0].iter().any(|(k, _)| k == "CARGO_TARGET_DIR"),
            "{:?}",
            runs[0]
        );
    }

    // ---- ADR-0066 D2（Phase 110b）: 終端タスクの作業場所からビルド生成物を刈る ----

    /// `tick()` は、終端になってから `workspace_prune_after_secs` 経った作業場所の `target/` を消し、
    /// `workspace_pruned` イベントを積む（削除は背景スレッド。少し待てば反映される）。
    #[tokio::test]
    async fn tick_prunes_the_oldest_terminal_workspace_and_records_an_event() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let root = tempfile::tempdir().unwrap();
        let mut d = dispatcher(store.clone(), done_adapter(), 1);
        d.config.workspace_root = root.path().to_path_buf();
        d.config.workspace_prune_after_secs = 1;

        let task = new_task(
            root.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();
        store
            .apply_transition_with_events(task.id, Trigger::Dispatch, vec![])
            .unwrap();
        store
            .apply_transition_with_events(task.id, Trigger::WorkerDone, vec![])
            .unwrap();
        store
            .apply_transition_with_events(task.id, Trigger::ReviewPass, vec![])
            .unwrap();
        let mut current = store.get(task.id).unwrap().unwrap();
        current.updated_at = OffsetDateTime::now_utc() - time::Duration::days(2);
        store
            .update_task(&current, Event::worker_progress("x", "backdated"))
            .unwrap();

        let task_dir = root.path().join(task.id.to_string());
        let target_dir = task_dir.join("repos").join("benchfs").join("target");
        std::fs::create_dir_all(&target_dir).unwrap();

        d.tick().unwrap();
        // 削除は背景スレッドなので、少し待って反映を確かめる。
        for _ in 0..100 {
            if !target_dir.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!target_dir.exists(), "target/ should have been pruned");
        assert!(
            task_dir.join("repos").join("benchfs").is_dir(),
            "the repo dir itself is kept"
        );
        let events = store.events_for(task.id).unwrap();
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                Event::WorkspacePruned { removed }
                    if removed == &vec!["repos/benchfs/target".to_string()]
            )),
            "{events:?}"
        );
    }

    /// `workspace_prune_after_secs == 0` は無効（何も消さない）。
    #[tokio::test]
    async fn workspace_prune_after_secs_zero_disables_pruning() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let root = tempfile::tempdir().unwrap();
        let mut d = dispatcher(store.clone(), done_adapter(), 1);
        d.config.workspace_root = root.path().to_path_buf();
        d.config.workspace_prune_after_secs = 0;

        let task = new_task(
            root.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        store.insert(&task).unwrap();
        store
            .apply_transition_with_events(task.id, Trigger::Dispatch, vec![])
            .unwrap();
        store
            .apply_transition_with_events(task.id, Trigger::WorkerDone, vec![])
            .unwrap();
        store
            .apply_transition_with_events(task.id, Trigger::ReviewPass, vec![])
            .unwrap();
        let mut current = store.get(task.id).unwrap().unwrap();
        current.updated_at = OffsetDateTime::now_utc() - time::Duration::days(30);
        store
            .update_task(&current, Event::worker_progress("x", "backdated"))
            .unwrap();

        let task_dir = root.path().join(task.id.to_string());
        let target_dir = task_dir.join("repos").join("benchfs").join("target");
        std::fs::create_dir_all(&target_dir).unwrap();

        d.tick().unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            target_dir.exists(),
            "prune_after_secs = 0 must disable pruning"
        );
    }

    /// ADR-0041 D1: 前置きに作業ツリー・ブランチ・base と「このブランチにコミットせよ」が出る。
    #[tokio::test]
    async fn the_preamble_note_names_the_worktree_the_branch_and_the_base() {
        let repo_dir = tempfile::tempdir().unwrap();
        let main_sha = init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let d = worktree_dispatcher(
            store.clone(),
            Arc::new(RecordingAdapter {
                seen: Arc::new(StdMutex::new(Vec::new())),
                files: vec![],
            }),
            root.path(),
            None,
        );
        let worktree = d.task_workspaces_for(&task).expect("worktree plan");
        let extras = d
            .run_extras(&task, Some(&worktree), None, "claude-code")
            .unwrap();
        let note = extras.workspace_note.expect("workspace_note");
        let tree = root.path().join(task.id.to_string()).join("tree");
        assert!(note.contains(&format!("→ `{}`", tree.display())), "{note}");
        assert!(
            note.contains(&format!("ブランチ `celeris/{}`", task.id)),
            "{note}"
        );
        assert!(
            note.contains(&format!("base `{}`（main）", &main_sha[..12])),
            "{note}"
        );
        assert!(
            note.contains(&format!("カレントディレクトリは `{}`", tree.display())),
            "{note}"
        );
        assert!(note.contains("ブランチにコミットせよ"), "{note}");
        assert!(note.contains("`main` に直接コミットするな"), "{note}");
        assert!(
            note.contains("`git checkout` でブランチを変えるな"),
            "{note}"
        );
        // ADR-0043 D8: 成果物と文書の置き場。
        assert!(
            note.contains(&format!("は `{}/docs` の下に置け", tree.display())),
            "{note}"
        );
        assert!(note.contains("`artifacts/` は run の中間物"), "{note}");
    }

    /// ADR-0041 D1: 本番の `current` が `main` の子孫なら、その sha から分岐する（本番より古いコードから始めない）。
    #[tokio::test]
    async fn the_worktree_branches_from_the_current_release_when_it_is_ahead_of_main() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        // `main` の先に 1 コミット（= 本番のリリースが main に未反映の状態）。
        let _ = git_out(repo_dir.path(), &["checkout", "-q", "-b", "released"]);
        std::fs::write(repo_dir.path().join("shipped.txt"), b"x").unwrap();
        let _ = git_out(repo_dir.path(), &["add", "-A"]);
        let _ = git_out(repo_dir.path(), &["commit", "-q", "-m", "shipped"]);
        let shipped = git_out(repo_dir.path(), &["rev-parse", "HEAD"]);
        let _ = git_out(repo_dir.path(), &["checkout", "-q", "main"]);
        // 偽の `current` リリース（`releases_dir` の親にある。ADR-0040 D6）。
        let home = tempfile::tempdir().unwrap();
        let releases = home.path().join("releases");
        std::fs::create_dir_all(&releases).unwrap();
        let current = home.path().join("current");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(
            current.join("manifest.json"),
            format!("{{\"sha\":\"{shipped}\",\"sha12\":\"{}\"}}", &shipped[..12]),
        )
        .unwrap();

        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen,
            files: vec![],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), Some(releases));
        let worktree = d.local_worktree_for(&task).expect("worktree plan");
        assert_eq!(worktree.base.kind.as_str(), "current");
        assert_eq!(worktree.base.sha, shipped);
        run_until_idle(&mut d, 60).await;
        // クリーンなので worktree は消えているが、ブランチは残り、その先端は `current` の sha。
        let branch = format!("celeris/{}", task.id);
        assert_eq!(git_out(repo_dir.path(), &["rev-parse", &branch]), shipped);
    }

    /// ADR-0043 D2（ADR-0041 D1 の改定）: **終端では worktree を消さない**（`done` で未取り込みの
    /// 差分を見るために残す）。ブランチも残る。
    #[tokio::test]
    async fn a_worktree_survives_the_terminal_state_together_with_its_branch() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen,
            files: vec![],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        run_until_idle(&mut d, 60).await;
        // 終端に達した次の tick で「片付け」が回っても消えない。
        d.tick().unwrap();

        let task_dir = root.path().join(task.id.to_string());
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
        assert!(
            task_dir.join("tree").is_dir(),
            "ADR-0043 D2: 終端では消さない"
        );
        assert!(task_dir.join("artifacts").is_dir(), "成果物は残る");
        assert!(
            git_ok(
                repo_dir.path(),
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/celeris/{}", task.id)
                ]
            ),
            "ブランチも残る"
        );
        let events = store.events_for(task.id).unwrap();
        assert!(
            !events.iter().any(|(_, e)| matches!(e, Event::WorkerProgress { msg, .. } if msg.starts_with("未コミット"))),
            "クリーンなら「未コミットの変更」は出さない"
        );
    }

    /// ADR-0043 D2: **中止**（cancel）されたタスクの worktree とブランチは消える。
    #[tokio::test]
    async fn cancelling_a_task_removes_its_worktree_and_branch() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        // 質問で止まる run（`blocked`）にして、終端になる前に人が中止できるようにする。
        let adapter = Arc::new(InstantAdapter {
            terminal: Terminal::Question {
                text: "どちらで進めますか".into(),
            },
            delay: Duration::ZERO,
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        run_until_idle(&mut d, 60).await;
        let task_dir = root.path().join(task.id.to_string());
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Blocked);
        assert!(task_dir.join("tree").is_dir());
        // 未コミットの変更があっても cancel は消す（人の指示なので）。
        std::fs::write(task_dir.join("tree/wip.txt"), b"x").unwrap();

        // 人が GUI / CLI から中止する（`task-ops::gate::cancel` と同じ遷移）。
        store
            .apply_transition(task.id, task_core::Trigger::Cancel, None)
            .unwrap();
        d.tick().unwrap();

        assert_eq!(
            store.get(task.id).unwrap().unwrap().status,
            Status::Cancelled
        );
        assert!(
            !task_dir.join("tree").exists(),
            "中止したら worktree は消える"
        );
        assert!(
            task_dir.join("artifacts").is_dir(),
            "run の記録と成果物は残る"
        );
        assert!(
            !git_ok(
                repo_dir.path(),
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/celeris/{}", task.id)
                ]
            ),
            "中止したらブランチも消える"
        );
    }

    /// ADR-0041 D1 / ADR-0043 D2: 未コミットの変更が残っていれば `WorkerProgress` を 1 行積む（worktree は残す）。
    #[tokio::test]
    async fn a_dirty_worktree_is_kept_and_a_progress_line_is_appended() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "test -f left-behind".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen,
            files: vec!["left-behind".into()],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        run_until_idle(&mut d, 60).await;
        d.tick().unwrap();

        let tree = root.path().join(task.id.to_string()).join("tree");
        assert!(
            tree.join("left-behind").is_file(),
            "未コミットの変更ごと残す"
        );
        let events = store.events_for(task.id).unwrap();
        let progress: Vec<&String> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerProgress { msg, .. } if msg.starts_with("未コミット") => {
                    Some(msg)
                }
                _ => None,
            })
            .collect();
        assert_eq!(progress.len(), 1, "1 行だけ");
        assert!(
            progress[0].contains(&tree.display().to_string()),
            "{}",
            progress[0]
        );
        // 2 回目の tick で重ねて積まない（記録は片付けたら落とす）。
        d.tick().unwrap();
        let again = store
            .events_for(task.id)
            .unwrap()
            .iter()
            .filter(|(_, e)| matches!(e, Event::WorkerProgress { msg, .. } if msg.starts_with("未コミット")))
            .count();
        assert_eq!(again, 1);
    }

    // ---- ADR-0043 D2 / D4（Phase 52）: 複数のリポジトリ ----

    /// 案件と `project_repos` を仕込み、その案件のタスクを返す。
    fn project_with_repos(
        store: &Arc<dyn TaskStore>,
        repos: &[(&str, &std::path::Path, task_core::RepoKind)],
    ) -> (task_core::ProjectId, Vec<task_core::ProjectRepo>) {
        let now = OffsetDateTime::now_utc();
        let project = task_core::Project {
            archived_at: None,
            paused_from: None,
            id: task_core::ProjectId::new(),
            title: "benchfs".into(),
            request: "測る".into(),
            status: task_core::ProjectStatus::Active,
            secretary_summary: None,
            workspace: None,
            created_at: now,
            updated_at: now,
        };
        store.project_create(&project).unwrap();
        let mut out = Vec::new();
        for (i, (name, path, kind)) in repos.iter().enumerate() {
            let repo = task_core::ProjectRepo {
                id: task_core::RepoId::new(),
                project_id: project.id,
                name: (*name).to_string(),
                kind: *kind,
                location: WorkspaceSpec::local(*path),
                default_branch: None,
                sync: None,
                run: task_core::RepoRun::Auto,
                is_primary: i == 0,
                created_at: now,
            };
            store.repo_create(&repo).unwrap();
            out.push(store.repo_get(repo.id).unwrap().unwrap());
        }
        (project.id, out)
    }

    #[tokio::test]
    async fn approved_docs_reconciliation_reuses_exact_worktree_branch_and_commit() {
        use task_ops::{
            docs_maintenance as docs,
            workspace::{WorktreeMarker, WorktreeMarkerRepo},
        };
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let base = init_test_repo(&source);
        let workspace = root.path().join("workspaces");
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let (project_id, repos) = project_with_repos(
            &store,
            &[("code", source.as_path(), task_core::RepoKind::Git)],
        );
        let mut task = new_task(
            &source,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.project_id = Some(project_id);
        task.repos = repos.iter().map(task_core::RepoRef::of).collect();
        store.insert(&task).unwrap();
        let task_dir = workspace.join(task.id.to_string());
        let attached = task_dir.join("repos/code");
        let state = root.path().join("state");
        let mut plan = docs::proposal(&docs::audit(&source, "main").unwrap());
        plan.actions.push(docs::Action::Rewrite {
            path: "README.md".into(),
            body: "# Approved canonical guide\n".into(),
        });
        docs::approve_plan(&state, "repo", &plan).unwrap();
        let approved_sha =
            docs::apply_plan(&source, "main", &attached, &plan, &state, "repo").unwrap();
        let branch = git_out(&attached, &["branch", "--show-current"]);
        let marker = WorktreeMarker {
            repo: source.display().to_string(),
            dir: attached.display().to_string(),
            branch: branch.clone(),
            base: base.clone(),
            base_kind: "main".into(),
            repos: vec![WorktreeMarkerRepo {
                name: "code".into(),
                kind: "git".into(),
                source: source.display().to_string(),
                dir: attached.display().to_string(),
                branch: Some(branch.clone()),
                base: Some(base.clone()),
                base_kind: Some("main".into()),
            }],
        };
        task_ops::workspace::write_marker(&task_dir, &marker).unwrap();
        std::fs::create_dir_all(task_dir.join("artifacts")).unwrap();
        std::fs::write(
            task_dir.join("artifacts/reconciliation-plan.json"),
            serde_json::to_vec(&plan).unwrap(),
        )
        .unwrap();
        let d = worktree_dispatcher(
            store,
            Arc::new(RecordingAdapter {
                seen: Arc::new(StdMutex::new(Vec::new())),
                files: vec![],
            }),
            &workspace,
            None,
        );
        let planned = d.task_workspaces_for(&task).unwrap();
        assert_eq!(planned.repos[0].branch(), Some(branch.as_str()));
        assert_eq!(planned.repos[0].worktree.as_ref().unwrap().base.sha, base);
        planned.ensure().await.unwrap();
        assert_eq!(git_out(&attached, &["rev-parse", "HEAD"]), approved_sha);
        assert_eq!(
            std::fs::read_to_string(attached.join("README.md")).unwrap(),
            "# Approved canonical guide\n"
        );
        assert_eq!(git_out(&source, &["rev-parse", "main"]), base);
        std::fs::remove_file(task_dir.join("artifacts/reconciliation-plan.json")).unwrap();
        assert_ne!(
            d.task_workspaces_for(&task).unwrap().repos[0].branch(),
            Some(branch.as_str())
        );
        std::fs::write(
            task_dir.join("artifacts/reconciliation-plan.json"),
            serde_json::to_vec(&plan).unwrap(),
        )
        .unwrap();
        let mut mismatched = marker;
        mismatched.repos[0].source = root.path().join("other").display().to_string();
        task_ops::workspace::write_marker(&task_dir, &mismatched).unwrap();
        assert_ne!(
            d.task_workspaces_for(&task).unwrap().repos[0].branch(),
            Some(branch.as_str())
        );
    }

    /// ADR-0043 D2: git 2 つ + `dir` 1 つのタスクは、`repos/<name>/` に worktree 2 つと
    /// シンボリックリンク 1 つを持ち、cwd は**先頭のリポジトリ**になる。前置きには全部が並ぶ。
    #[tokio::test]
    async fn a_task_with_several_repos_gets_one_worktree_per_git_repo_and_a_link_for_the_rest() {
        let root = tempfile::tempdir().unwrap();
        let code = root.path().join("benchfs");
        let paper = root.path().join("benchfs-paper");
        init_test_repo(&code);
        init_test_repo(&paper);
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("one.csv"), b"1\n").unwrap();

        let ws_root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let (project_id, repos) = project_with_repos(
            &store,
            &[
                ("benchfs", code.as_path(), task_core::RepoKind::Git),
                ("benchfs-paper", paper.as_path(), task_core::RepoKind::Git),
                ("data", data.as_path(), task_core::RepoKind::Dir),
            ],
        );
        // 先頭が cwd になる（`repos[0]`）。
        let mut task = new_task(
            &code,
            Check::Command {
                cmd: "test -f in-tree".into(),
                expect_exit: 0,
            },
            0,
        );
        task.project_id = Some(project_id);
        task.repos = repos.iter().map(task_core::RepoRef::of).collect();
        store.insert(&task).unwrap();

        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen: seen.clone(),
            files: vec!["in-tree".into()],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, ws_root.path(), None);
        // 前置き（`run_extras`）は dispatch の前に組める。
        let workspaces = d.task_workspaces_for(&task).expect("workspaces");
        assert_eq!(workspaces.repos.len(), 3);
        let note = d
            .run_extras(&task, Some(&workspaces), None, "claude-code")
            .unwrap()
            .workspace_note
            .expect("note");
        for name in ["benchfs", "benchfs-paper", "data"] {
            assert!(note.contains(&format!("- `{name}` →")), "{note}");
        }
        assert!(
            note.contains("ディレクトリ。読み書き可。git ではない"),
            "{note}"
        );
        assert!(
            note.contains(&format!("ブランチ `celeris/{}`", task.id)),
            "{note}"
        );

        run_until_idle(&mut d, 60).await;

        let task_dir = ws_root.path().join(task.id.to_string());
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
        // git のリポジトリは worktree。
        for name in ["benchfs", "benchfs-paper"] {
            let dir = task_dir.join("repos").join(name);
            assert!(dir.join(".git").exists(), "{name} は worktree");
            assert!(dir.join("README.md").is_file());
        }
        // `dir` はシンボリックリンク（コピーしない）。
        let link = task_dir.join("repos").join("data");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(link.join("one.csv").is_file());
        // cwd は先頭のリポジトリ。`runs/` と `artifacts/` は作業ツリーの外。
        let (cwd, workspace, artifacts) = seen.lock().unwrap()[0].clone();
        assert_eq!(cwd, task_dir.join("repos/benchfs").canonicalize().unwrap());
        assert_eq!(workspace, task_dir.canonicalize().unwrap());
        assert_eq!(
            artifacts,
            task_dir.canonicalize().unwrap().join("artifacts")
        );
        assert!(task_dir.join("runs").is_dir());
        // 判定コマンドも先頭の worktree で走った（`test -f in-tree` が通っている）。
        assert!(task_dir.join("repos/benchfs/in-tree").is_file());

        // 目印（`worktree.json`）に全部が並ぶ（ファイル閲覧 API がこれを見る）。
        let marker = task_ops::workspace::read_marker(&task_dir).expect("marker");
        assert_eq!(marker.repos.len(), 3);
        assert_eq!(marker.repos[0].name, "benchfs");
        assert_eq!(marker.repos[0].kind, "git");
        assert_eq!(marker.repos[2].kind, "dir");
        assert_eq!(
            marker.dir, marker.repos[0].dir,
            "先頭の写しが Phase 49 の目印になる"
        );
        assert_eq!(
            task_ops::workspace::local_dir(&store.get(task.id).unwrap().unwrap(), ws_root.path()),
            task_dir
        );
    }

    /// ADR-0006 Phase 115 D3（本番障害 01M3915FARENW8M0JM11XVF6W0）: 報告のまとめ（`report-compressor`）
    /// のような diff を作らない内部タスクは、その案件にリポジトリが付いていても worktree を作らない
    /// （`work_dir = workspace` のまま走る）。`task_ops::add::create_support_task` を実際に通して、
    /// `resolve_repos` の primary 継承の抑止（`workspace_mode: Shared`）と `task_workspaces_for` の
    /// 両方が効くことを確かめる（D4(c)）。
    #[tokio::test]
    async fn a_compaction_task_does_not_get_a_worktree_even_when_its_project_has_a_primary_repo() {
        let root = tempfile::tempdir().unwrap();
        let code = root.path().join("agent-platform");
        init_test_repo(&code);

        let ws_root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let (project_id, _repos) = project_with_repos(
            &store,
            &[("agent-platform", code.as_path(), task_core::RepoKind::Git)],
        );
        store
            .org_upsert(&org_node_of("secretary", None, OrgKind::Secretary, None))
            .unwrap();
        store
            .org_upsert(&org_node_of(
                "engineering",
                Some("secretary"),
                OrgKind::Department,
                None,
            ))
            .unwrap();

        let spec = task_ops::add::NewTaskSpec {
            title: "報告のまとめ: engineering 課".into(),
            objective: "まとめてください".into(),
            acceptance: Vec::new(),
            kind: TaskKind::Execute,
            tier: None,
            priority: Some(task_ops::add::PriorityInput::Number(0)),
            parent: None,
            depends_on: Vec::new(),
            max_turns: Some(8),
            max_wall_secs: Some(600),
            max_retries: 1,
            role: Some(task_core::report::COMPACTION_ROLE.to_string()),
            genre: None,
            aggregate: false,
            project_id: Some(project_id),
            milestone_id: None,
            assignee: Some("engineering".to_string()),
            workspace: None,
            cluster: None,
            workspace_mode: Some(task_core::WorkspaceMode::Shared),
            adapter: None,
            repos: Vec::new(),
            labels: Vec::new(),
            skills: Vec::new(),
            mode: None,
            category: None,
            features: None,
            provenance: Default::default(),
            status: None,
        };
        let task = task_ops::add::create_support_task(
            store.as_ref(),
            spec,
            &[],
            &[],
            OffsetDateTime::now_utc(),
        )
        .unwrap();
        // ADR-0006 Phase 115 D3: 案件の primary（`agent-platform`）を暗黙に継がない。
        assert!(task.repos.is_empty(), "{:?}", task.repos);

        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen: seen.clone(),
            files: Vec::new(),
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, ws_root.path(), None);
        // `task_workspaces_for` が `None`（worktree を用意しない）ことを直接確かめる。
        assert!(
            d.task_workspaces_for(&task).is_none(),
            "compaction task should not get any worktree"
        );

        run_until_idle(&mut d, 60).await;
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);

        let task_dir = ws_root.path().join(task.id.to_string());
        // work_dir が無い＝ cwd はそのまま workspace（`task_dir`）。
        let (cwd, workspace, _artifacts) = seen.lock().unwrap()[0].clone();
        assert_eq!(
            cwd, workspace,
            "cwd should just be the workspace, no worktree cwd"
        );
        assert_eq!(workspace, task_dir);
        assert!(!task_dir.join("tree").exists(), "no single-repo worktree");
        assert!(!task_dir.join("repos").exists(), "no per-repo worktree dir");
    }

    /// ADR-0043 D3 / D4: `[commands] setup` が落ちたら run を始めず、既存の質問の経路で `blocked` にする。
    #[tokio::test]
    async fn a_failing_setup_blocks_the_task_with_a_question_instead_of_starting_the_run() {
        let root = tempfile::tempdir().unwrap();
        let code = root.path().join("benchfs");
        init_test_repo(&code);
        std::fs::create_dir_all(code.join(".config/celeris")).unwrap();
        std::fs::write(
            code.join(".config/celeris/workspace.toml"),
            b"[commands]\nsetup = [\"exit 3\"]\n",
        )
        .unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", "setup"]] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&code)
                .args(&args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let ws_root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let (project_id, repos) = project_with_repos(
            &store,
            &[("benchfs", code.as_path(), task_core::RepoKind::Git)],
        );
        let mut task = new_task(
            &code,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.project_id = Some(project_id);
        task.repos = repos.iter().map(task_core::RepoRef::of).collect();
        store.insert(&task).unwrap();

        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen: seen.clone(),
            files: vec![],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, ws_root.path(), None);
        run_until_idle(&mut d, 60).await;

        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Blocked);
        assert!(seen.lock().unwrap().is_empty(), "ワーカーは起こさない");
        let task_dir = ws_root.path().join(task.id.to_string());
        let log = std::fs::read_to_string(task_dir.join("runs/setup.log")).expect("setup.log");
        assert!(log.contains("$ (benchfs) exit 3"), "{log}");
        let events = store.events_for(task.id).unwrap();
        let question = events
            .iter()
            .find_map(|(_, e)| match e {
                Event::WorkerFinished { outcome, .. } if outcome.starts_with("question:") => {
                    Some(outcome.clone())
                }
                _ => None,
            })
            .expect("question outcome");
        assert!(question.contains("setup が失敗しました"), "{question}");
        assert!(question.contains("workspace.toml"), "{question}");
    }

    /// ADR-0043 D3（Phase 56）: `[run] mode = "container"` のリポジトリを使うタスクは、コンテナ
    /// runtime が使えないと **run を始めず** `blocked` になり、人に質問が積まれる。
    ///
    /// runtime の検出には**偽の podman / docker**（`info` が失敗する sh スクリプト）を使う。
    /// 本物の podman / docker にもネットワークにも触らない。
    #[tokio::test]
    async fn a_container_task_is_blocked_with_a_question_when_no_runtime_works() {
        let root = tempfile::tempdir().unwrap();
        let code = root.path().join("benchfs");
        init_test_repo(&code);
        std::fs::create_dir_all(code.join(".config/celeris")).unwrap();
        std::fs::write(
            code.join(".config/celeris/workspace.toml"),
            b"[run]\nmode = \"container\"\n",
        )
        .unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", "container"]] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&code)
                .args(&args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        // 偽の runtime: `info` が必ず落ちる（podman は rootless、docker はデーモン不在を模す）。
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for (name, message) in [
            ("podman", "newuidmap: Operation not permitted"),
            ("docker", "Cannot connect to the Docker daemon"),
        ] {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\necho '{message}' 1>&2\nexit 1\n")).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }

        let ws_root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let (project_id, repos) = project_with_repos(
            &store,
            &[("benchfs", code.as_path(), task_core::RepoKind::Git)],
        );
        let mut task = new_task(
            &code,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        task.project_id = Some(project_id);
        task.repos = repos.iter().map(task_core::RepoRef::of).collect();
        store.insert(&task).unwrap();

        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen: seen.clone(),
            files: vec![],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, ws_root.path(), None);
        // 実物の検出（`detect_with` + `probe_program`）を偽の実行ファイルに向ける。
        let probe =
            task_worker::container::detect_with(task_worker::RuntimePreference::Auto, |rt| {
                task_worker::container::probe_program(
                    &bin.join(rt.as_str()).display().to_string(),
                    Duration::from_secs(10),
                )
            });
        assert!(!probe.is_available(), "{probe:?}");
        d.set_container_probe(probe);
        run_until_idle(&mut d, 60).await;

        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Blocked);
        assert!(seen.lock().unwrap().is_empty(), "ワーカーは起こさない");
        let events = store.events_for(task.id).unwrap();
        let question = events
            .iter()
            .find_map(|(_, e)| match e {
                Event::WorkerFinished { outcome, .. } if outcome.starts_with("question:") => {
                    Some(outcome.clone())
                }
                _ => None,
            })
            .expect("question outcome");
        assert!(
            question.contains("コンテナ runtime が使えません"),
            "{question}"
        );
        assert!(question.contains("benchfs"), "{question}");
        assert!(question.contains("newuidmap"), "{question}");
        assert!(question.contains("Cannot connect"), "{question}");
        // `setup` も走らない（実行環境が決まらないので run の手前で止まる）。
        assert!(
            !ws_root
                .path()
                .join(task.id.to_string())
                .join("runs/setup.log")
                .exists()
        );
    }

    /// ADR-0043 D3（Phase 56）: `[run] mode` を書いていない（= `host`）リポジトリのタスクは、
    /// runtime が使えなくても従来どおりホストで走る（コンテナの工事は既存の運用を変えない）。
    #[tokio::test]
    async fn a_host_task_still_runs_when_no_container_runtime_is_available() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen: seen.clone(),
            files: vec![],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        // 既定（`RuntimeProbe::default()` = 何も使えない）のまま走らせる。
        assert!(!d.container_probe().is_available());
        run_until_idle(&mut d, 60).await;
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Done);
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "ホストのタスクはそのまま走る"
        );
    }

    /// ADR-0043 D4: リポジトリの `[commands] check` は、タスクが検査コマンドを書いていないときだけ
    /// レビューの暗黙の条件になる（書いていればタスクの方が勝つ）。
    #[tokio::test]
    async fn the_repository_check_commands_are_the_reviewers_default() {
        let root = tempfile::tempdir().unwrap();
        let code = root.path().join("benchfs");
        init_test_repo(&code);
        std::fs::create_dir_all(code.join(".config/celeris")).unwrap();
        std::fs::write(
            code.join(".config/celeris/workspace.toml"),
            b"[commands]\ncheck = [\"test -f in-tree\"]\n",
        )
        .unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", "check"]] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&code)
                .args(&args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let ws_root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let (project_id, repos) = project_with_repos(
            &store,
            &[("benchfs", code.as_path(), task_core::RepoKind::Git)],
        );
        // 受け入れ条件に `Check::Command` が無い → リポジトリの `check` が暗黙の条件として足される。
        let mut task = new_task(
            &code,
            Check::ArtifactExists {
                name: "missing.json".into(),
            },
            0,
        );
        task.project_id = Some(project_id);
        task.repos = repos.iter().map(task_core::RepoRef::of).collect();
        store.insert(&task).unwrap();
        // 自分で検査コマンドを書いたタスクには足さない（明示が勝つ）。
        let mut explicit = new_task(
            &code,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            0,
        );
        explicit.project_id = Some(project_id);
        explicit.repos = task.repos.clone();
        store.insert(&explicit).unwrap();

        let seen = Arc::new(StdMutex::new(Vec::new()));
        // ワーカーが cwd に `in-tree` を置くので、リポジトリの `check` は通る。
        let adapter = Arc::new(RecordingAdapter {
            seen,
            files: vec!["in-tree".into()],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, ws_root.path(), None);
        assert_eq!(
            d.default_checks(&store.get(task.id).unwrap().unwrap()),
            vec!["test -f in-tree".to_string()]
        );
        run_until_idle(&mut d, 60).await;

        let verdicts = |id: TaskId| -> Vec<(usize, bool, String)> {
            store
                .events_for(id)
                .unwrap()
                .iter()
                .filter_map(|(_, e)| match e {
                    Event::ReviewVerdict {
                        criterion_idx,
                        pass,
                        reason,
                        ..
                    } => Some((*criterion_idx, *pass, reason.clone())),
                    _ => None,
                })
                .collect()
        };
        // 条件 0（`artifact_exists`）は落ち、暗黙の条件 1（リポジトリの `check`）は通る。
        let mine = verdicts(task.id);
        assert_eq!(mine.len(), 2, "{mine:?}");
        assert_eq!(mine[0].0, 0);
        assert!(!mine[0].1, "{mine:?}");
        assert_eq!(mine[1].0, 1);
        assert!(mine[1].1, "{mine:?}");
        assert!(mine[1].2.contains("workspace.toml check"), "{mine:?}");

        // 自分で検査コマンドを書いたタスクには暗黙の条件は足されない。
        let theirs = verdicts(explicit.id);
        assert_eq!(theirs.len(), 1, "{theirs:?}");
        assert!(theirs[0].1, "{theirs:?}");
        assert_eq!(
            store.get(explicit.id).unwrap().unwrap().status,
            Status::Done
        );
    }

    /// ADR-0041 D1: `mode = "shared"` は従来どおり `path` をそのまま作業ディレクトリにする。
    #[tokio::test]
    async fn shared_mode_keeps_the_repository_itself_as_the_working_directory() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            repo_dir.path(),
            Some(task_core::WorkspaceMode::Shared),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen: seen.clone(),
            files: vec![],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        assert!(d.local_worktree_for(&task).is_none());
        run_until_idle(&mut d, 60).await;

        let runs = seen.lock().unwrap().clone();
        assert_eq!(
            runs[0].0,
            repo_dir.path().canonicalize().unwrap(),
            "cwd はリポジトリそのもの"
        );
        assert_eq!(
            runs[0].2,
            repo_dir.path().canonicalize().unwrap().join("artifacts")
        );
        assert!(
            !root.path().join(task.id.to_string()).exists(),
            "タスクごとのディレクトリは作らない"
        );
    }

    /// ADR-0041 D1: git リポジトリでない `path` は `mode` の既定が `worktree` でも従来どおり。
    #[tokio::test]
    async fn a_local_path_that_is_not_a_git_repository_is_unchanged() {
        let plain = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = git_task(
            plain.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let adapter = Arc::new(RecordingAdapter {
            seen: seen.clone(),
            files: vec![],
        });
        let mut d = worktree_dispatcher(store.clone(), adapter, root.path(), None);
        assert!(d.local_worktree_for(&task).is_none());
        run_until_idle(&mut d, 60).await;

        let runs = seen.lock().unwrap().clone();
        assert_eq!(runs[0].0, plain.path().canonicalize().unwrap());
        assert_eq!(runs[0].1, plain.path().canonicalize().unwrap());
        assert!(!root.path().join(task.id.to_string()).exists());
    }

    /// ADR-0041 D1: 委譲の子は親の作業場所を継ぐので、**子ごとに別の worktree**になる。
    /// 親の集約 run には子のブランチ名が渡る（親はそれを merge する）。
    #[tokio::test]
    async fn each_delegated_child_gets_its_own_worktree_and_the_parent_sees_the_branches() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_repo(repo_dir.path());
        let root = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut parent = git_task(
            repo_dir.path(),
            None,
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
        );
        parent.aggregate = true;
        store.insert(&parent).unwrap();
        let mut children = Vec::new();
        for title in ["a", "b"] {
            let mut child = git_task(
                repo_dir.path(),
                None,
                Check::Command {
                    cmd: "true".into(),
                    expect_exit: 0,
                },
            );
            child.parent_id = Some(parent.id);
            child.title = title.into();
            child.status = Status::Done;
            store.insert(&child).unwrap();
            children.push(child);
        }
        store
            .append_event(
                parent.id,
                &Event::Transitioned {
                    from: Status::Running,
                    to: Status::Reviewing,
                    reason: "aggregate".into(),
                },
            )
            .unwrap();
        let d = worktree_dispatcher(
            store.clone(),
            Arc::new(RecordingAdapter {
                seen: Arc::new(StdMutex::new(Vec::new())),
                files: vec![],
            }),
            root.path(),
            None,
        );
        let extras = d.run_extras(&parent, None, None, "claude-code").unwrap();
        let mut branches: Vec<String> = extras
            .children
            .iter()
            .filter_map(|c| c.branch.clone())
            .collect();
        branches.sort();
        let mut expected: Vec<String> = children
            .iter()
            .map(|c| format!("celeris/{}", c.id))
            .collect();
        expected.sort();
        assert_eq!(branches, expected, "子ごとに別のブランチ");
        // 子の worktree は互いに別のディレクトリ（親の作業ツリーも共有しない）。
        let dirs: Vec<PathBuf> = children
            .iter()
            .map(|c| d.local_worktree_for(c).expect("child worktree").dir)
            .collect();
        assert_ne!(dirs[0], dirs[1]);
        assert_ne!(
            dirs[0],
            d.local_worktree_for(&parent).expect("parent worktree").dir
        );
        // 子でも成果物はタスクごとのディレクトリの中（`.taskd/artifacts/<id>` ではない）。
        assert_eq!(
            extras.children[0].workspace.as_deref(),
            Some(root.path().join(children[0].id.to_string()).as_path())
        );
    }
    #[tokio::test]
    async fn routing_applies_quota_tier_and_explicit_account_to_the_executed_model() {
        use task_core::model_routing::ModelBinding;
        for (utilization, expected, known) in [
            (0.1, "frontier-id", true),
            (0.8, "standard-id", true),
            (0.95, "cheap-id", true),
            (0.8, "frontier-id", false),
        ] {
            let accounts = accounts_fixture();
            let mut book = AccountBook::load(&accounts.path().join(".celeris-usage.json"));
            let mut obs = usage_window(utilization, 90_000);
            obs.seven_day = if known { obs.five_hour } else { None };
            book.record_observation("a", obs, ObservationSource::Run);
            book.save().unwrap();
            let ws = tempfile::tempdir().unwrap();
            let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
            let mut task = new_task(
                ws.path(),
                Check::Command {
                    cmd: "test -f touched".into(),
                    expect_exit: 0,
                },
                0,
            );
            task.worker_hint.tier = Tier::Frontier;
            store.insert(&task).unwrap();
            let captured = Arc::new(StdMutex::new(Vec::new()));
            let adapter = Arc::new(task_worker::tiered::TieredAdapter {
                base: Arc::new(PoolAdapter {
                    terminal_or_throttled: Ok(Terminal::Done {
                        summary: "ok".into(),
                        evidence: vec![],
                        usage: None,
                    }),
                    delay: Duration::ZERO,
                    observation: None,
                    env: vec![],
                    captured: captured.clone(),
                    spawn_failure: false,
                }),
                models: [
                    (Tier::Frontier, "frontier-id"),
                    (Tier::Standard, "standard-id"),
                    (Tier::Cheap, "cheap-id"),
                ]
                .into_iter()
                .map(|(tier, id)| {
                    (
                        tier,
                        ModelBinding {
                            name: id.into(),
                            model_id: Some(id.into()),
                            unavailable_reason: None,
                            reasoning_effort: None,
                        },
                    )
                })
                .collect(),
                account_id: Some("a".into()),
                credential_error: None,
            });
            let mut d = pool_dispatcher(
                store.clone(),
                adapter,
                None,
                accounts.path().to_path_buf(),
                2,
                2,
            );
            d.set_now_unix_fn(Arc::new(|| 10_000));
            assert!(run_until_idle(&mut d, 200).await.idle);
            let envs = captured.lock().unwrap();
            assert_eq!(envs.len(), 1);
            assert!(
                envs[0].contains(&("TEST_MODEL".into(), expected.into())),
                "{envs:?}"
            );
            let events = store.events_for(task.id).unwrap();
            assert!(events.iter().any(|(_,e)| matches!(e,Event::WorkerStarted {model,account,..} if model == expected && account.as_deref() == Some("a"))));
            assert!(events.iter().any(|(_,e)| matches!(e,Event::WorkerProgress {msg,..} if msg.contains(if known { "measured quota remaining" } else { "quota remaining unknown" }))));
        }
    }
    #[tokio::test]
    async fn unavailable_tier_blocks_before_starting_any_worker() {
        let ws = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(ws.path(), Check::Human, 0);
        store.insert(&task).unwrap();
        let adapter = Arc::new(task_worker::tiered::TieredAdapter {
            base: Arc::new(InstantAdapter {
                terminal: Terminal::Question {
                    text: "must not run".into(),
                },
                delay: Duration::ZERO,
            }),
            models: [(
                task.worker_hint.tier,
                task_core::model_routing::ModelBinding {
                    name: "fable".into(),
                    model_id: None,
                    unavailable_reason: Some("unverified executable ID".into()),
                    reasoning_effort: None,
                },
            )]
            .into(),
            account_id: None,
            credential_error: None,
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        d.tick().unwrap();
        assert_eq!(store.get(task.id).unwrap().unwrap().status, Status::Blocked);
        let events = store.events_for(task.id).unwrap();
        assert!(
            !events
                .iter()
                .any(|(_, e)| matches!(e, Event::WorkerStarted { .. }))
        );
        assert!(events.iter().any(|(_, e)| matches!(e, Event::WorkerProgress { msg, .. } if msg.contains("unverified executable ID"))));
    }

    /// ADR-0069（Phase 114）テスト用: 3 lane の束縛を持つ TieredAdapter（cheap だけ reasoning effort 付き）。
    fn three_lane_adapter() -> Arc<dyn WorkerAdapter> {
        use task_core::model_routing::ModelBinding;
        Arc::new(task_worker::tiered::TieredAdapter {
            base: Arc::new(InstantAdapter {
                terminal: Terminal::Question {
                    text: "not needed".into(),
                },
                delay: Duration::ZERO,
            }),
            models: [
                (Tier::Frontier, "frontier-id", None),
                (Tier::Standard, "standard-id", None),
                (Tier::Cheap, "cheap-id", Some("low")),
            ]
            .into_iter()
            .map(|(tier, id, effort)| {
                (
                    tier,
                    ModelBinding {
                        name: id.into(),
                        model_id: Some(id.into()),
                        unavailable_reason: None,
                        reasoning_effort: effort.map(str::to_string),
                    },
                )
            })
            .collect(),
            account_id: None,
            credential_error: None,
        })
    }

    fn routing_record(events: &[(u64, Event)]) -> Option<task_core::RoutingRecord> {
        events.iter().rev().find_map(|(_, e)| match e {
            Event::RoutingDecided { record, .. } => Some((**record).clone()),
            _ => None,
        })
    }

    /// ADR-0069 D3 / D5: `routing` を持つ execute タスクは、LLM のヒント（frontier）ではなく
    /// TaskFeatures の規則表で lane が決まり（機械的・検証可能・戻せる → cheap）、その lane の model が
    /// 走り、`RoutingDecided` に features・規則・版・reasoning effort が残る。
    #[tokio::test]
    async fn lane_policy_decides_the_tier_and_records_the_routing_decision() {
        let ws = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(
            ws.path(),
            Check::Command {
                cmd: "cargo test".into(),
                expect_exit: 0,
            },
            2,
        );
        task.objective = "crates/task-core/src/model.rs の typo を直す".into();
        task.genre = Some("coding".into());
        task.worker_hint.tier = Tier::Frontier;
        task.routing = Some(task_core::TaskRouting {
            tier_source: task_core::TierSource::Hint,
            ..Default::default()
        });
        store.insert(&task).unwrap();
        let mut d = dispatcher(store.clone(), three_lane_adapter(), 1);
        d.tick().unwrap();
        let events = store.events_for(task.id).unwrap();
        assert!(
            events.iter().any(
                |(_, e)| matches!(e, Event::WorkerStarted { model, .. } if model == "cheap-id")
            ),
            "{events:?}"
        );
        let record = routing_record(&events).expect("routing_decided");
        assert_eq!(record.decision.lane, Tier::Cheap);
        assert_eq!(record.decision.hint, Some(Tier::Frontier));
        assert_eq!(
            record.decision.rule_id,
            "cheap/mechanical-verifiable-reversible"
        );
        assert_eq!(
            record.decision.policy_version,
            task_core::LANE_POLICY_VERSION
        );
        assert_eq!(record.harness.as_deref(), Some("coding"));
        assert_eq!(record.resolution.model_id, "cheap-id");
        // ADR-0069 Phase 118 D1: この記録は「設定した」値ではなく「実際に CLI へ渡った」値。
        // `InstantAdapter`（このテストの基盤アダプタ）は `WorkerAdapter::supports_reasoning_effort`
        // の既定（`false`）のままなので `None`（`crates/task-worker/src/codex.rs` の
        // `tier_reasoning_effort_reaches_cli_as_a_dash_c_config_override` が実際に codex へ渡る
        // ことを別途検証する）。
        assert_eq!(record.resolution.reasoning_effort, None);
        // 監査の集計に乗る。
        let audit = task_ops::routing_audit::task_routing_audit(store.as_ref(), task.id).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].lane, Some(Tier::Cheap));
        assert_eq!(
            audit[0].rule_id.as_deref(),
            Some("cheap/mechanical-verifiable-reversible")
        );

        // 人の明示 tier は policy が触らない。`routing` の無い既存タスクは記録も出さない。
        let store2: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut human = task.clone();
        human.id = TaskId::new();
        human.routing = Some(task_core::TaskRouting {
            tier_source: task_core::TierSource::Human,
            ..Default::default()
        });
        store2.insert(&human).unwrap();
        let mut legacy = task.clone();
        legacy.id = TaskId::new();
        legacy.routing = None;
        store2.insert(&legacy).unwrap();
        let mut d2 = dispatcher(store2.clone(), three_lane_adapter(), 2);
        d2.tick().unwrap();
        let h = store2.events_for(human.id).unwrap();
        assert!(h.iter().any(
            |(_, e)| matches!(e, Event::WorkerStarted { model, .. } if model == "frontier-id")
        ));
        assert_eq!(
            routing_record(&h).map(|r| r.decision.rule_id),
            Some("explicit/human".to_string())
        );
        let l = store2.events_for(legacy.id).unwrap();
        assert!(l.iter().any(
            |(_, e)| matches!(e, Event::WorkerStarted { model, .. } if model == "frontier-id")
        ));
        assert!(routing_record(&l).is_none());
    }

    /// ADR-0069 D6: 同じ lane でレビュー不合格が 2 回続いたタスクのやり直しは 1 段だけ上がり
    /// （cheap → standard）、理由が `RoutingDecided.decision.escalation` に残る。
    #[tokio::test]
    async fn repeated_review_failures_escalate_the_retry_lane_one_step() {
        let ws = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let mut task = new_task(
            ws.path(),
            Check::Command {
                cmd: "cargo test".into(),
                expect_exit: 0,
            },
            3,
        );
        task.objective = "crates/task-core/src/model.rs の typo を直す".into();
        task.genre = Some("coding".into());
        task.routing = Some(task_core::TaskRouting::default());
        task.attempts = 2;
        task.updated_at = OffsetDateTime::now_utc() - time::Duration::days(1);
        store.insert(&task).unwrap();
        let decision =
            task_core::model_policy::decide_for_task(&task, &Default::default()).unwrap();
        assert_eq!(decision.lane, Tier::Cheap);
        for run in ["r1", "r2"] {
            let record = task_core::RoutingRecord {
                org_node: None,
                harness: Some("coding".into()),
                decision: decision.clone(),
                resolution: task_core::model_routing::LaneResolution {
                    lane: Some(Tier::Cheap),
                    ..Default::default()
                },
                quota_reason: None,
                work_unit_id: None,
            };
            for event in [
                Event::RoutingDecided {
                    run_id: run.into(),
                    record: Box::new(record),
                },
                Event::ReviewVerdict {
                    run_id: format!("{run}-review"),
                    criterion_idx: 0,
                    pass: false,
                    reason: "tests fail".into(),
                },
                Event::Transitioned {
                    from: Status::Reviewing,
                    to: Status::Ready,
                    reason: "review_fail".into(),
                },
            ] {
                store.append_event(task.id, &event).unwrap();
            }
        }
        let mut d = dispatcher(store.clone(), three_lane_adapter(), 1);
        d.tick().unwrap();
        let events = store.events_for(task.id).unwrap();
        assert!(
            events.iter().any(
                |(_, e)| matches!(e, Event::WorkerStarted { model, .. } if model == "standard-id")
            ),
            "{events:?}"
        );
        let record = routing_record(&events).expect("routing_decided");
        assert_eq!(record.decision.lane, Tier::Standard);
        assert_eq!(record.decision.proposed, Tier::Cheap);
        assert!(
            record
                .decision
                .escalation
                .as_deref()
                .is_some_and(|e| e.starts_with("escalate Cheap -> Standard")),
            "{:?}",
            record.decision.escalation
        );
    }

    // ---- ADR-0056 D3（Phase 79）: mount された skills を run に届ける ----

    fn write_kb_skill(kb_root: &std::path::Path, name: &str, description: &str, body: &str) {
        let dir = kb_root.join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    /// `skills_context` は KB にある skill を `SkillMount` に解決し、無い名前は 2 つ目の戻り値
    /// （`missing`）に回す（run は落とさない）。
    #[test]
    fn skills_context_resolves_mounted_skills_and_reports_missing_ones() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let adapter = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let mut d = dispatcher(store, adapter, 1);
        let kb = tempfile::tempdir().unwrap();
        d.config.knowledge.root = kb.path().to_path_buf();
        write_kb_skill(kb.path(), "writing", "文章の書き方", "本文");

        let (skills, missing) = d.skills_context(&["writing".to_string(), "ghost".to_string()]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "writing");
        assert_eq!(skills[0].description, "文章の書き方");
        assert_eq!(
            skills[0].path,
            kb.path()
                .join("skills")
                .join("writing")
                .display()
                .to_string()
        );
        assert_eq!(missing, vec!["ghost".to_string()]);
    }

    /// ADR-0046 D1（継承）+ ADR-0056 D3: `run_extras` は担当ノードの実効 profile が継いだ
    /// `skills_mounts`（親と子の和、重複は落ちる）を KB から解決して `RunExtras.skills` に積む。
    #[test]
    fn run_extras_resolves_the_assigned_nodes_effective_skills_mounts() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let now = OffsetDateTime::now_utc();
        store
            .org_upsert(&OrgNode {
                id: "cos".into(),
                parent_id: None,
                name: "cos".into(),
                kind: OrgKind::Secretary,
                genre: None,
                brief: String::new(),
                profile: task_core::Profile {
                    skills_mounts: vec!["writing".into()],
                    ..task_core::Profile::default()
                },
                position: 0,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        store
            .org_upsert(&OrgNode {
                id: "engineering".into(),
                parent_id: Some("cos".into()),
                name: "engineering".into(),
                kind: OrgKind::Department,
                genre: None,
                brief: String::new(),
                // `writing` は親と重複（落ちる）、`ghost` は KB に無い（missing に回る）。
                profile: task_core::Profile {
                    skills_mounts: vec!["rust-review".into(), "writing".into(), "ghost".into()],
                    ..task_core::Profile::default()
                },
                position: 0,
                created_at: now,
                updated_at: now,
            })
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut task = new_task(dir.path(), Check::Human, 0);
        task.assignee = Some("engineering".into());
        store.insert(&task).unwrap();

        let adapter = Arc::new(person_adapter(Terminal::Done {
            summary: "ok".into(),
            evidence: vec![],
            usage: None,
        }));
        let mut d = dispatcher(store, adapter, 1);
        let kb = tempfile::tempdir().unwrap();
        d.config.knowledge.root = kb.path().to_path_buf();
        write_kb_skill(kb.path(), "writing", "d", "body");
        write_kb_skill(kb.path(), "rust-review", "d", "body");

        let extras = d.run_extras(&task, None, None, "claude-code").unwrap();
        let names: Vec<&str> = extras.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["writing", "rust-review"],
            "根→葉の和で重複が落ちる: {names:?}"
        );
        assert_eq!(extras.missing_skills, vec!["ghost".to_string()]);
    }

    /// 実際の dispatch（`run_until_idle`）で `RunContext.skills` がアダプタに届き、mount 名にあったが
    /// KB に無かった skill は `status` の進行イベントを 1 行残すだけで run を失敗させない。
    #[tokio::test]
    async fn dispatch_delivers_mounted_skills_and_reports_missing_ones_without_failing_the_run() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let now = OffsetDateTime::now_utc();
        store
            .org_upsert(&OrgNode {
                id: "engineering".into(),
                parent_id: None,
                name: "engineering".into(),
                // 根は secretary だけ（ADR-0033 D1）。skills mount の解決そのものは対話・非対話を
                // 区別しないので、根で 1 段だけの組織にする。
                kind: OrgKind::Secretary,
                genre: None,
                brief: String::new(),
                profile: task_core::Profile {
                    skills_mounts: vec!["writing".into(), "ghost".into()],
                    ..task_core::Profile::default()
                },
                position: 0,
                created_at: now,
                updated_at: now,
            })
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut task = new_task(dir.path(), Check::Human, 0);
        task.assignee = Some("engineering".into());
        store.insert(&task).unwrap();

        let seen = Arc::new(StdMutex::new(None));
        let adapter = Arc::new(PersonAdapter {
            terminal: Terminal::Done {
                summary: "ok".into(),
                evidence: vec![],
                usage: None,
            },
            seen: seen.clone(),
            memory: None,
            proposals: Vec::new(),
        });
        let mut d = dispatcher(store.clone(), adapter, 1);
        let kb = tempfile::tempdir().unwrap();
        d.config.knowledge.root = kb.path().to_path_buf();
        write_kb_skill(kb.path(), "writing", "d", "body");

        assert!(run_until_idle(&mut d, 20).await.idle);
        let context = seen.lock().unwrap().clone().expect("the adapter ran");
        assert_eq!(context.skills.len(), 1);
        assert_eq!(context.skills[0].name, "writing");

        let events = store.events_for(task.id).unwrap();
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                Event::WorkerProgress { msg, kind, .. }
                    if msg == "skill ghost not found" && *kind == Some(task_core::ProgressKind::Status)
            )),
            "{events:?}"
        );
        let status = store.get(task.id).unwrap().unwrap().status;
        assert!(
            matches!(status, Status::Reviewing | Status::Done),
            "a missing skill mount must not fail the run: {status:?}"
        );
    }

    // ========== ADR-0072（Phase E1）: run lifecycle / checkpoint / continuation ==========

    /// 常に `Terminal::BudgetExhausted` を返す（checkpoint は書かない。mechanical だけになる）。
    struct AlwaysBudgetExhaustedAdapter;

    #[async_trait]
    impl WorkerAdapter for AlwaysBudgetExhaustedAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            _req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            Ok(RunOutcome {
                terminal: Terminal::BudgetExhausted {
                    kind: task_core::BudgetKind::Turns,
                    message: "error_max_turns".into(),
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    /// ADR-0072 (a)(b)(e)(g): 予算切れは `Trigger::Continue` で `Ready` に戻り attempts を消費しない。
    /// 進捗のない checkpoint が連続 2 回続くと `blocked`（`QuestionRaised` + `Approval`）。人の回答
    /// （`Trigger::Answer`）で窓が戻り、また continuation が進む。
    #[tokio::test]
    async fn budget_exhausted_run_continues_without_consuming_attempts_then_blocks_on_no_progress()
    {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            2,
        );
        let task_id = task.id;
        store.insert(&task).unwrap();
        let mut d = dispatcher(store.clone(), Arc::new(AlwaysBudgetExhaustedAdapter), 1);

        let report = run_until_idle(&mut d, 400).await;
        assert!(report.idle, "{report:?}");

        let stored = store.get(task_id).unwrap().unwrap();
        assert_eq!(stored.status, Status::Blocked, "{stored:?}");
        assert_eq!(
            stored.attempts, 0,
            "continuation は attempts を消費しない (g)"
        );

        let events = store.events_for(task_id).unwrap();
        let checkpoint_events = events
            .iter()
            .filter(|(_, e)| matches!(e, Event::CheckpointSaved { .. }))
            .count();
        assert!(checkpoint_events >= 2, "{checkpoint_events}");
        let ends: Vec<_> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::WorkerFinished {
                    role: None, end, ..
                } => Some(*end),
                _ => None,
            })
            .collect();
        assert!(
            ends.iter().all(|e| matches!(
                e,
                Some(task_core::RunEnd::BudgetExhausted {
                    kind: task_core::BudgetKind::Turns
                })
            )),
            "{ends:?}"
        );
        // D18/D21: 上限到達・進捗なしは（cross_department の質問などと同じ扱いで）`WorkerFinished.outcome`
        // が `"question: ..."` になり、`Trigger::WorkerQuestion` の一般経路で `blocked` になる。
        let last_outcome = events
            .iter()
            .rev()
            .find_map(|(_, e)| match e {
                Event::WorkerFinished {
                    role: None,
                    outcome,
                    ..
                } => Some(outcome.clone()),
                _ => None,
            })
            .expect("some WorkerFinished");
        assert!(last_outcome.starts_with("question: "), "{last_outcome}");
        assert!(last_outcome.contains("進捗なし"), "{last_outcome}");

        // (g): continue は試行に数えない（`retry_policy::attempt_history`）。
        let events_only: Vec<Event> = events.iter().map(|(_, e)| e.clone()).collect();
        let history = task_core::retry_policy::attempt_history(&stored, &events_only);
        assert!(history.is_empty(), "{history:?}");
        // (g): `classify_task_failure` は blocked のタスクを見ても Failed 前提の分類を返すだけなので
        // ここでは呼ばない。`stats::classify_outcome` が continue を Error に数えないことは
        // `task-api::stats::tests::outcome_prefixes_are_classified` で確認済み。

        // D18: 人の回答で窓が戻る。
        store
            .apply_transition(
                task_id,
                Trigger::Answer,
                Some(Event::Answered {
                    question: "実行が進みません".into(),
                    answer: "続けてください".into(),
                }),
            )
            .unwrap();
        assert_eq!(store.get(task_id).unwrap().unwrap().status, Status::Ready);
        let report = run_until_idle(&mut d, 200).await;
        assert!(report.idle, "{report:?}");
        // 同じ（進捗を示さない）アダプタなので、また 2 回の無進捗 continuation の後 blocked に戻る。
        let stored = store.get(task_id).unwrap().unwrap();
        assert_eq!(stored.status, Status::Blocked);
        assert_eq!(stored.attempts, 0);
    }

    /// 1 回目は `Terminal::Yielded`（checkpoint で進捗を申告）、2 回目は `done` を返すアダプタ。
    /// 渡された `RunContext` を記録する。
    struct YieldThenDoneAdapter {
        seen: Arc<StdMutex<Vec<task_worker::RunContext>>>,
    }

    #[async_trait]
    impl WorkerAdapter for YieldThenDoneAdapter {
        fn id(&self) -> &str {
            "instant"
        }
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            let n = {
                let mut seen = self.seen.lock().unwrap();
                seen.push(req.context.clone());
                seen.len()
            };
            if n == 1 {
                Ok(RunOutcome {
                    terminal: Terminal::Yielded {
                        checkpoint: serde_json::json!({
                            "completed": ["A を実装した"],
                            "remaining": ["B のテスト"],
                            "next_action": "B のテストを書く",
                        }),
                        usage: None,
                    },
                    exit_code: Some(0),
                })
            } else {
                Ok(done_outcome())
            }
        }
    }

    /// ADR-0072 (c)(d): result.json の `yield` は `Terminal::Yielded` → `Trigger::Continue` になり、
    /// 次の run の `RunContext.continuation` に checkpoint と Run 番号が載る（会話の全文は載らない —
    /// そもそも `ContinuationContext` は checkpoint の JSON しか運ばない）。1 回目の `RunContext` は
    /// continuation を持たない。
    #[tokio::test]
    async fn yielded_run_continues_and_the_next_run_context_carries_the_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            1,
        );
        let task_id = task.id;
        store.insert(&task).unwrap();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let mut d = dispatcher(
            store.clone(),
            Arc::new(YieldThenDoneAdapter { seen: seen.clone() }),
            1,
        );

        let report = run_until_idle(&mut d, 400).await;
        assert!(report.idle, "{report:?}");
        let stored = store.get(task_id).unwrap().unwrap();
        assert_eq!(
            stored.attempts, 0,
            "continuation は attempts を消費しない (g)"
        );
        assert_eq!(stored.status, Status::Done, "{stored:?}");

        let contexts = seen.lock().unwrap().clone();
        assert_eq!(contexts.len(), 2, "{contexts:?}");
        assert!(
            contexts[0].continuation.is_none(),
            "最初の run には continuation は無い: {:?}",
            contexts[0].continuation
        );
        let cont = contexts[1]
            .continuation
            .as_ref()
            .expect("2 回目の run は continuation を持つ");
        assert_eq!(cont.run_seq, 2);
        assert_eq!(cont.previous_end, "yielded");
        assert_eq!(cont.checkpoint["next_action"], "B のテストを書く");
        assert_eq!(cont.prior_runs, vec!["Run #1 yielded".to_string()]);

        let events = store.events_for(task_id).unwrap();
        assert!(
            events
                .iter()
                .any(|(_, e)| matches!(e, Event::CheckpointSaved { .. })),
            "{events:?}"
        );
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                Event::WorkerFinished {
                    role: None,
                    end: Some(task_core::RunEnd::Yielded),
                    ..
                }
            )),
            "{events:?}"
        );
    }

    /// ADR-0072 §6 (f): `[execution] continuation = false` なら、予算切れは従来どおり
    /// `WorkerError{retryable:true}` になり attempts を消費する（continuation しない）。
    #[tokio::test]
    async fn continuation_disabled_restores_the_legacy_worker_error() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            1,
        );
        let task_id = task.id;
        store.insert(&task).unwrap();
        let mut d = dispatcher(store.clone(), Arc::new(AlwaysBudgetExhaustedAdapter), 1);
        d.config.execution.continuation = false;

        let report = run_until_idle(&mut d, 400).await;
        assert!(report.idle, "{report:?}");
        let stored = store.get(task_id).unwrap().unwrap();
        assert_eq!(stored.status, Status::Failed, "{stored:?}");
        assert_eq!(
            stored.attempts, 2,
            "max_retries=1 なので 2 回試行して失敗 (f)"
        );
        let events = store.events_for(task_id).unwrap();
        assert!(
            !events
                .iter()
                .any(|(_, e)| matches!(e, Event::CheckpointSaved { .. })),
            "continuation を無効にしたら checkpoint も作らない: {events:?}"
        );
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                Event::Transitioned { reason, .. } if reason == "worker_error"
            )),
            "{events:?}"
        );
        assert!(
            !events.iter().any(
                |(_, e)| matches!(e, Event::Transitioned { reason, .. } if reason == "continue")
            ),
            "{events:?}"
        );
    }

    /// ADR-0072 (h): daemon の再起動（同じ store で新しい Dispatcher）の後も、
    /// 最新の checkpoint から continuation が組まれる（全ての状態は DB/events にあり、
    /// dispatcher のメモリには無いため）。
    #[tokio::test]
    async fn continuation_survives_a_dispatcher_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let task = new_task(
            dir.path(),
            Check::Command {
                cmd: "true".into(),
                expect_exit: 0,
            },
            2,
        );
        let task_id = task.id;
        store.insert(&task).unwrap();
        // 「daemon 再起動前」の状態を events で直接作る（実際の Dispatcher の tick は使わない — 即完了
        // する fake adapter で 2 つの tick にまたがる非同期の完了を決定的に待つのは難しいため。
        // ここで作る events は、実際の on_worker_finished が積む形とバイト単位で同じにする）。
        store
            .apply_transition_with_events(
                task_id,
                Trigger::Dispatch,
                vec![Event::WorkerStarted {
                    run_id: "run-1".into(),
                    adapter: "instant".into(),
                    model: "m".into(),
                    provider: Some("p1".into()),
                    account: None,
                    role: None,
                    task_role: None,
                }],
            )
            .unwrap();
        let checkpoint = task_core::Checkpoint {
            schema: task_core::CHECKPOINT_SCHEMA.into(),
            task_id: task_id.to_string(),
            work_unit: None,
            run_id: "run-1".into(),
            run_seq: 1,
            end: task_core::CheckpointEnd::BudgetExhausted,
            source: task_core::CheckpointSource::Mechanical,
            completed: vec![],
            remaining: vec!["続きの作業".into()],
            decisions: vec![],
            files_changed: vec![],
            tests_run: vec![],
            known_failures: vec![],
            artifact_refs: vec![],
            next_action: "続ける".into(),
            open_questions: vec![],
            plan_issue: None,
            repo_state: None,
            recent_activity: vec![],
            created_at: "2026-09-24T00:00:00Z".into(),
        };
        store
            .apply_transition_with_events(
                task_id,
                Trigger::Continue {
                    why: task_core::ContinueWhy::Continue,
                },
                vec![
                    Event::WorkerFinished {
                        run_id: "run-1".into(),
                        outcome: "continue: budget_exhausted(turns) の続き（Run #2）".into(),
                        usage: None,
                        role: None,
                        metrics: None,
                        end: Some(task_core::RunEnd::BudgetExhausted {
                            kind: task_core::BudgetKind::Turns,
                        }),
                    },
                    Event::CheckpointSaved {
                        run_id: "run-1".into(),
                        work_unit_id: None,
                        checkpoint: Box::new(checkpoint),
                    },
                ],
            )
            .unwrap();
        let stored = store.get(task_id).unwrap().unwrap();
        assert_eq!(stored.status, Status::Ready, "{stored:?}");
        assert_eq!(stored.attempts, 0);
        // ここまでが「daemon 再起動前」に相当する状態。d1（前のプロセスの Dispatcher）は無く、
        // 全ての状態は DB/events にある（`infra_backoff` 等のプロセス内メモリは何も使っていない）。

        // 新しい Dispatcher（同じ store）で続きを回す。
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let mut d2 = dispatcher(
            store.clone(),
            Arc::new(InterruptProbeAdapter {
                seen: seen.clone(),
                hold: Duration::ZERO,
            }),
            1,
        );
        let report = run_until_idle(&mut d2, 200).await;
        assert!(report.idle, "{report:?}");
        let stored = store.get(task_id).unwrap().unwrap();
        assert_eq!(stored.attempts, 0, "continuation は attempts を消費しない");
        assert_eq!(stored.status, Status::Done, "{stored:?}");

        let contexts = seen.lock().unwrap().clone();
        assert_eq!(contexts.len(), 1, "{contexts:?}");
        let cont = contexts[0]
            .continuation
            .as_ref()
            .expect("再起動後の run も continuation を持つ");
        assert_eq!(cont.run_seq, 2);
        assert!(cont.previous_end.starts_with("budget_exhausted"));
    }
}

// ========== ADR-0052（Phase 64）: 知識整理 run のフォールバック ==========
#[cfg(test)]
mod knowledge_fallback_tests {
    use super::tests::run_until_idle;
    use super::*;
    use crate::policy::{ProviderSpec, StaticPolicy};
    use async_trait::async_trait;
    use std::sync::Mutex as SyncMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use task_core::*;

    /// 知識整理 run を受ける「汎用」アダプタ（開発用の `fake` と同じ id）。走ったら
    /// `artifacts/knowledge-candidates.json` を書いて `done` になる。
    struct CandidatesAdapter {
        id: &'static str,
        seen: Arc<SyncMutex<Vec<RunRequest>>>,
    }

    #[async_trait]
    impl WorkerAdapter for CandidatesAdapter {
        fn id(&self) -> &str {
            self.id
        }
        async fn run(
            &self,
            req: RunRequest,
            _run_id: &str,
            _limits: RunLimits,
            _sink: &dyn EventSink,
        ) -> Result<RunOutcome, AdapterError> {
            std::fs::create_dir_all(&req.artifacts_dir).ok();
            std::fs::write(
                req.artifact_path("knowledge-candidates.json"),
                r#"{"candidates": [{"op": "create", "path": "environment/tools/x.md", "title": "x",
                    "tags": [], "scope": "environment", "body": "b", "sources": ["task:x"],
                    "confidence": "high"}]}"#,
            )
            .ok();
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(req);
            }
            Ok(RunOutcome {
                terminal: Terminal::Done {
                    summary: "1 件の候補を書いた".into(),
                    evidence: vec![],
                    usage: None,
                },
                exit_code: Some(0),
            })
        }
    }

    fn knowledge_task(dir: &std::path::Path) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            routing: None,
            mode: Default::default(),
            skills: Vec::new(),
            repos: Vec::new(),
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "知識整理: pegasus".into(),
            objective: "この仕事から知識の候補を抽出せよ".into(),
            acceptance: Vec::new(),
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Cheap,
                adapter: Some("langmem".into()),
            },
            workspace: WorkspaceSpec::Local {
                path: dir.to_path_buf(),
                mode: None,
            },
            budget: Budget {
                max_turns: 4,
                max_wall_secs: 900,
                max_retries: 1,
            },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: Some(task_core::report::KNOWLEDGE_ROLE.to_string()),
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

    /// `langmem`（専用）と `fake`（汎用）の 2 つの供給元を持つディスパッチャ。
    /// `generic` が false なら汎用の供給元を置かない（ADR-0052 D2「候補が無ければ従来どおり失敗」）。
    fn knowledge_dispatcher(
        store: Arc<dyn TaskStore>,
        seen: Arc<SyncMutex<Vec<RunRequest>>>,
        fallback_tier: Option<Tier>,
        generic: bool,
    ) -> Dispatcher {
        let mut providers = vec![ProviderSpec {
            id: "qwen".into(),
            adapter: "langmem".into(),
            tiers: vec![Tier::Cheap],
            concurrency: 1,
            model: "qwen".into(),
        }];
        let mut adapters: HashMap<ProviderId, Arc<dyn WorkerAdapter>> = HashMap::new();
        adapters.insert(
            "qwen".into(),
            Arc::new(CandidatesAdapter {
                id: "langmem",
                seen: seen.clone(),
            }),
        );
        if generic {
            providers.push(ProviderSpec {
                id: "cheap-generic".into(),
                adapter: "fake".into(),
                tiers: vec![Tier::Cheap],
                concurrency: 1,
                model: "fake".into(),
            });
            adapters.insert(
                "cheap-generic".into(),
                Arc::new(CandidatesAdapter { id: "fake", seen }),
            );
        }
        let policy = StaticPolicy::new(providers, Duration::from_secs(1));
        Dispatcher::new(
            store,
            Box::new(policy),
            HashMap::new(),
            adapters,
            std::collections::HashSet::new(),
            DispatchConfig {
                delivery: Default::default(),
                max_concurrency: 2,
                lease_grace: Duration::from_secs(60),
                idle_timeout: Duration::from_secs(5),
                kill_grace: Duration::from_millis(100),
                review_timeout: Duration::from_secs(5),
                workspace_root: PathBuf::from("/nonexistent"),
                plan_auto_accept: false,
                retry_backoff_base: Duration::ZERO,
                retry_backoff_max: Duration::ZERO,
                reviewer_hint: crate::review::reviewer_hint(),
                reviewer_tier_override: None,
                clusters: HashMap::new(),
                cluster_cooldown: Duration::from_secs(1),
                max_requeues: 5,
                max_reviewer_retries: 3,
                max_infra_retries: 5,
                roles: Vec::new(),
                genres: Vec::new(),
                delegation: DelegationLimits::default(),
                accounts: None,
                memory_dir: None,
                worktree_branch_prefix: task_worker::DEFAULT_BRANCH_PREFIX.to_string(),
                releases_dir: None,
                containers: ContainersRuntimeConfig::default(),
                knowledge: KnowledgeRuntimeConfig {
                    langmem_base_url: Some("http://127.0.0.1:1/v1".to_string()),
                    fallback_tier,
                    ..KnowledgeRuntimeConfig::default()
                },
                session_rollover_tokens: 400_000,
                shared_build_cache: false,
                build_cache_dir: PathBuf::from("/nonexistent-build-cache"),
                workspace_prune_after_secs: 0,
                execution: ExecutionConfig::default(),
            },
        )
    }

    fn started_adapter(store: &Arc<dyn TaskStore>, task_id: TaskId) -> Option<String> {
        store
            .events_for(task_id)
            .expect("events")
            .into_iter()
            .rev()
            .find_map(|(_, e)| match e {
                Event::WorkerStarted { adapter, .. } => Some(adapter),
                _ => None,
            })
    }

    /// ADR-0052 D1 + D2: 接続先に届かなければ tier `cheap` の汎用ハーネスで走り、`status` の進行が
    /// 理由つきで残り、前置きは LangMem と同じ抽出の指示 + 出力契約、予算は 8 turn / 600 秒になる。
    /// 書かれた `artifacts/knowledge-candidates.json` は `apply_finished` が読む場所にある。
    #[tokio::test]
    async fn an_unreachable_langmem_endpoint_runs_the_extraction_on_a_cheap_generic_harness() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let task = knowledge_task(dir.path());
        store.insert(&task).expect("insert");
        let seen = Arc::new(SyncMutex::new(Vec::new()));
        let mut d = knowledge_dispatcher(store.clone(), seen.clone(), Some(Tier::Cheap), true);
        d.set_knowledge_probe(Arc::new(|_, _| Reachability::Unreachable {
            reason: "接続できない: Connection refused".into(),
        }));
        run_until_idle(&mut d, 100).await;

        assert_eq!(
            started_adapter(&store, task.id).as_deref(),
            Some("fake"),
            "cheap の汎用ハーネスで走る"
        );
        let events = store.events_for(task.id).expect("events");
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                Event::WorkerProgress { msg, kind, .. }
                    if *kind == Some(ProgressKind::Status)
                        && msg.contains("langmem の接続先に届かない")
                        && msg.contains("Connection refused")
                        && msg.contains("cheap のハーネスに倒す")
            )),
            "{events:?}"
        );

        let requests = seen.lock().expect("seen");
        assert_eq!(requests.len(), 1);
        let req = &requests[0];
        assert_eq!(req.task.budget.max_turns, 8, "ADR-0052 D2 の予算");
        assert_eq!(req.task.budget.max_wall_secs, 600);
        assert_eq!(
            req.task.worker_hint.adapter, None,
            "専用アダプタの固定は外れている"
        );
        assert_eq!(
            req.task.objective, task.objective,
            "依頼文（maintenance_objective）は同じ入力のまま"
        );
        let role = req.context.role.as_ref().expect("role");
        assert!(
            role.instructions
                .contains(task_worker::langmem::extraction_instructions()),
            "{}",
            role.instructions
        );
        assert!(
            role.instructions
                .contains("artifacts/knowledge-candidates.json")
        );
        assert!(role.instructions.contains("道具は使わない"));
        // `apply_finished` が読む場所に書かれている。
        assert!(
            task_core::artifacts::artifacts_dir_for(&task, dir.path())
                .join("knowledge-candidates.json")
                .exists()
        );
    }

    /// ADR-0052 D1: 200 が返れば従来どおり `langmem` で走る（フォールバックしない）。
    /// 検査は `base_url` ごとに 60 秒キャッシュするので、tick を何度回しても 1 回しか叩かない。
    #[tokio::test]
    async fn a_reachable_endpoint_keeps_using_langmem_and_the_probe_is_cached() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let task = knowledge_task(dir.path());
        store.insert(&task).expect("insert");
        let seen = Arc::new(SyncMutex::new(Vec::new()));
        let mut d = knowledge_dispatcher(store.clone(), seen.clone(), Some(Tier::Cheap), true);
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        d.set_knowledge_probe(Arc::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Reachability::Ok
        }));
        run_until_idle(&mut d, 100).await;

        assert_eq!(started_adapter(&store, task.id).as_deref(), Some("langmem"));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "60 秒キャッシュ");
        let requests = seen.lock().expect("seen");
        assert_eq!(requests[0].task.budget.max_turns, 4, "予算は元のまま");
        assert!(
            requests[0]
                .context
                .role
                .as_ref()
                .is_none_or(|r| !r.instructions.contains("出力の契約 (output contract)"))
        );
        // 進行に「倒す」の 1 行は出ない。
        assert!(
            !store.events_for(task.id).expect("events").iter().any(
                |(_, e)| matches!(e, Event::WorkerProgress { msg, .. } if msg.contains("倒す"))
            )
        );
    }

    /// Phase 65b: `[knowledge.langmem].api_key_secret` から解決したトークンが probe に渡ること
    /// （celeris の `llm-proxy` のように `GET /v1/models` が認証を要求する上流を指したときのため）。
    #[tokio::test]
    async fn the_resolved_api_key_is_passed_to_the_knowledge_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let task = knowledge_task(dir.path());
        store.insert(&task).expect("insert");
        let seen = Arc::new(SyncMutex::new(Vec::new()));
        let mut d = knowledge_dispatcher(store.clone(), seen, Some(Tier::Cheap), true);
        d.config.knowledge.langmem_api_key = Some("secret-proxy-token".to_string());
        let seen_tokens = Arc::new(SyncMutex::new(Vec::new()));
        let capture = seen_tokens.clone();
        d.set_knowledge_probe(Arc::new(move |_, token| {
            capture
                .lock()
                .expect("lock")
                .push(token.map(str::to_string));
            Reachability::Ok
        }));
        run_until_idle(&mut d, 100).await;
        assert_eq!(
            seen_tokens.lock().expect("lock").as_slice(),
            [Some("secret-proxy-token".to_string())]
        );
    }

    /// ADR-0052 D2: `fallback = false`（＝ `fallback_tier` が無い）なら、届かなくても倒さない
    /// （検査もしない。従来どおり `langmem` に出す）。
    #[tokio::test]
    async fn fallback_false_keeps_the_dedicated_adapter_even_when_the_endpoint_is_down() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let task = knowledge_task(dir.path());
        store.insert(&task).expect("insert");
        let seen = Arc::new(SyncMutex::new(Vec::new()));
        let mut d = knowledge_dispatcher(store.clone(), seen, None, true);
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        d.set_knowledge_probe(Arc::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Reachability::Unreachable {
                reason: "接続できない".into(),
            }
        }));
        run_until_idle(&mut d, 100).await;
        assert_eq!(started_adapter(&store, task.id).as_deref(), Some("langmem"));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "無効なら検査もしない");
    }

    /// ADR-0052 D2: tier `cheap` の汎用の供給元が 1 つも無ければ、従来どおり dispatch されずに
    /// `ready` のまま残る（＝ 供給が戻れば次の tick で拾われる。`retryable = true` と同じ扱い）。
    #[tokio::test]
    async fn without_any_cheap_generic_provider_the_run_stays_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let task = knowledge_task(dir.path());
        store.insert(&task).expect("insert");
        let seen = Arc::new(SyncMutex::new(Vec::new()));
        let mut d = knowledge_dispatcher(store.clone(), seen.clone(), Some(Tier::Cheap), false);
        d.set_knowledge_probe(Arc::new(|_, _| Reachability::Unreachable {
            reason: "接続できない".into(),
        }));
        for _ in 0..3 {
            assert_eq!(d.tick().expect("tick").dispatched, 0);
        }
        assert_eq!(
            store.get(task.id).expect("get").expect("some").status,
            Status::Ready
        );
        assert!(seen.lock().expect("seen").is_empty());
        assert!(started_adapter(&store, task.id).is_none());
    }

    /// 知識整理 run 以外（`role` が `knowledge` でない、または adapter が `langmem` でない）は
    /// 一切影響を受けない（検査もしない）。
    #[tokio::test]
    async fn other_tasks_never_trigger_the_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let mut task = knowledge_task(dir.path());
        task.role = None;
        task.worker_hint.adapter = Some("fake".into());
        store.insert(&task).expect("insert");
        let seen = Arc::new(SyncMutex::new(Vec::new()));
        let mut d = knowledge_dispatcher(store.clone(), seen, Some(Tier::Cheap), true);
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        d.set_knowledge_probe(Arc::new(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Reachability::Unreachable {
                reason: "接続できない".into(),
            }
        }));
        run_until_idle(&mut d, 100).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(started_adapter(&store, task.id).as_deref(), Some("fake"));
    }
}
