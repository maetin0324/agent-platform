//! ADR-0040 D4（Phase 47）: celeris の「インスタンスの役割」とライブ引き継ぎ。
//!
//! 昇格（新しいリリースへの切り替え）は、新しいプロセスを**同じ DB・同じポート**に対して起こし、
//! 古いプロセスに `handoff_requested_at` を書いて `draining` にし、新しいプロセスが `active` を
//! 引き継ぐことで行う。判断はすべて tick の中で決定的に行い、LLM もワーカーも関与しない
//! （DESIGN 原則 1〜4。ここから `task-worker` は見えない）。
//!
//! 規則（ADR-0040 D4 そのまま）:
//!
//! - 起動時: `--mode verify` なら役割 `verify`（`daemon_instances` に**行を書かない**）。
//!   そうでなければ、heartbeat が新しい `active` が居て **`release` が同じ**なら二重起動なので exit 3。
//!   `release` が違うなら `standby` になり、その `active` の行に `handoff_requested_at` を書く。
//!   `active` が居なければ自分が `active`。
//! - 毎 tick: 自分の行に heartbeat を打つ。`active` は `handoff_requested_at` を見たら**同じ tick で**
//!   `draining` へ（listener を閉じ、dispatch と裏方を止める。手元の run は面倒を見続ける）。
//!   `standby` は `active` が `draining` になった／heartbeat が古くなったのを見たら `active` へ。
//! - 手元の run が 0 になったら `drained_at` を書いて exit 0。`[handoff] drain_timeout_secs` を
//!   超えたら残りを abort して exit 0。
//! - 他のインスタンスの行は、`drained_at` が付くか heartbeat が古くなったら消す。

use std::sync::Arc;
use std::time::Duration;

use task_core::{DaemonInstance, InstanceRole, SharedRole, StoreError, TaskStore};
use time::OffsetDateTime;

/// `release` を書いていないときの既定（作業チェックアウトから直接起こした場合）。
pub const DEV_RELEASE: &str = "dev";
/// `--release` も無いときに見る環境変数（systemd の unit が渡す）。
pub const RELEASE_ENV: &str = "CELERIS_RELEASE";

/// このプロセスの身元（ADR-0040 D4）。`instance_id` は API の `GET /health` に出るものと同じ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceIdentity {
    pub instance_id: String,
    pub release: String,
    pub pid: u32,
}

impl InstanceIdentity {
    /// `release` は `--release <sha12>` > 環境変数 `CELERIS_RELEASE` > `"dev"` の順。
    pub fn new(cli_release: Option<&str>) -> Self {
        Self {
            instance_id: ulid::Ulid::new().to_string(),
            release: resolve_release(cli_release),
            pid: std::process::id(),
        }
    }
}

/// `--release` > `CELERIS_RELEASE` > `"dev"`（どちらも空文字は「無い」扱い）。
pub fn resolve_release(cli_release: Option<&str>) -> String {
    cli_release
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var(RELEASE_ENV)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEV_RELEASE.to_string())
}

/// heartbeat が「新しい」とみなす窓（ADR-0040 D4: `3 × tick + lease_grace`）。
pub fn freshness_window(tick: Duration, lease_grace_secs: u64) -> Duration {
    tick.saturating_mul(3) + Duration::from_secs(lease_grace_secs)
}

/// 起動時の判断（純粋な関数。DB には触れない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupDecision {
    /// `active` がいないので自分が `active` になる。
    Active,
    /// 別の `release` の `active` がいるので `standby` になり、その行に引き継ぎを要求する。
    Standby { active_instance_id: String },
    /// 同じ `release` の `active` が既にいる。何もせず exit 3（同じ版を二重に起こさない）。
    DuplicateRelease { instance_id: String, pid: u32 },
}

/// そのプロセスがまだ生きているか（同一ホスト前提。ADR-0040 D4「同一ホスト・同一 SQLite」）。
/// 判定できない環境では `true`（＝生きている）を返す。**保守的な側**（二重起動を疑う側）に倒す。
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return true;
    }
    match std::fs::metadata(format!("/proc/{pid}")) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        // `/proc` が無い（Linux 以外）等。判定できないので生きている扱い。
        Err(_) => true,
    }
}

/// ADR-0040 D4 の起動時の規則。`rows` は `daemon_instances` の全行。
///
/// `alive` は「その pid のプロセスがまだ生きているか」（本番は `pid_alive`）。heartbeat が新しくても
/// プロセスが消えていれば二重起動ではない — `SIGKILL` で落ちた直後に同じ版を起こし直すのは普通の
/// 復旧手順なので、そこで exit 3 を返すと復旧できなくなる（本 Phase での ADR-0040 D4 の細則）。
pub fn decide_startup(
    rows: &[DaemonInstance],
    release: &str,
    now: OffsetDateTime,
    freshness: Duration,
    alive: &dyn Fn(u32) -> bool,
) -> StartupDecision {
    // 生きている `active` だけを見る（`drained_at` が付いた行は終わったインスタンス）。
    let fresh_active: Vec<&DaemonInstance> = rows
        .iter()
        .filter(|r| {
            r.role == InstanceRole::Active
                && r.drained_at.is_none()
                && r.is_fresh(now, freshness)
                && alive(r.pid)
        })
        .collect();
    // 同じ版が動いていれば、それが誰であっても二重起動（`started_at` が古い方を代表に選ぶ）。
    if let Some(same) = fresh_active.iter().find(|r| r.release == release) {
        return StartupDecision::DuplicateRelease {
            instance_id: same.instance_id.clone(),
            pid: same.pid,
        };
    }
    match fresh_active.first() {
        Some(active) => StartupDecision::Standby {
            active_instance_id: active.instance_id.clone(),
        },
        None => StartupDecision::Active,
    }
}

/// 毎 tick の判断（純粋な関数。DB には触れない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickDecision {
    /// 役割は変わらない。
    Stay,
    /// `standby` → `active`（dispatch と裏方を始める）。
    Promote,
    /// `active` → `draining`（listener を閉じ、dispatch と裏方を止める）。
    Drain,
}

/// ADR-0040 D4 の毎 tick の規則。`self_id` は自分の `instance_id`。
pub fn decide_tick(
    role: InstanceRole,
    self_id: &str,
    rows: &[DaemonInstance],
    now: OffsetDateTime,
    freshness: Duration,
) -> TickDecision {
    match role {
        InstanceRole::Active => {
            let asked = rows
                .iter()
                .any(|r| r.instance_id == self_id && r.handoff_requested_at.is_some());
            if asked {
                TickDecision::Drain
            } else {
                TickDecision::Stay
            }
        }
        InstanceRole::Standby => {
            // 生きている他の `active` が 1 つも無ければ（`draining` になった／heartbeat が止まった）昇格する。
            let another_active = rows.iter().any(|r| {
                r.instance_id != self_id
                    && r.role == InstanceRole::Active
                    && r.drained_at.is_none()
                    && r.is_fresh(now, freshness)
            });
            if another_active {
                TickDecision::Stay
            } else {
                TickDecision::Promote
            }
        }
        // `draining` はもう役割を変えない（run が 0 になるか drain timeout で終わる）。
        // `verify` はこの表に触れない。
        InstanceRole::Draining | InstanceRole::Verify => TickDecision::Stay,
    }
}

/// Phase 119 D4（監視）: `rows` のうち、自分（`self_id`）以外で `drained_at` が付いているのに
/// `alive(pid)` が真の行（＝「drain 後にプロセスが終了しない」障害。D1/D2 で直したが、念のための
/// 監視）。`instance_delete_stale` がこの行を消す直前に `Supervisor::step` が呼び、見つかった行だけ
/// WARN ログに残す。純粋な判定だけを持つ（DB にもログにも触れない）ので単体テストできる。
pub fn stale_but_alive_rows<'a>(
    rows: &'a [DaemonInstance],
    self_id: &str,
    alive: &dyn Fn(u32) -> bool,
) -> Vec<&'a DaemonInstance> {
    rows.iter()
        .filter(|r| r.instance_id != self_id && r.drained_at.is_some() && alive(r.pid))
        .collect()
}

/// `Supervisor::start` の結果。
pub enum Started {
    Running(Supervisor),
    /// 同じ `release` の `active` が既にいる（exit 3）。
    Duplicate {
        instance_id: String,
        pid: u32,
    },
}

/// 1 tick 進めた結果。呼び出し側（tick ループ）がこれを見て listener とディスパッチャを動かす。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// 何も変わらない。
    Stay,
    /// `active` になった。dispatch と裏方を始める。
    Promoted,
    /// `draining` になった。**この tick で** listener を閉じ、dispatch と裏方を止める。
    Draining,
    /// 手元の run が 0 になった（`drained_at` を書いた）。exit 0。
    Drained,
    /// `[handoff] drain_timeout_secs` を超えた（`drained_at` を書いた）。残りを abort して exit 0。
    DrainTimedOut,
}

/// `daemon_instances` の自分の行を持ち、毎 tick 役割を決めるもの（ADR-0040 D4）。
/// `--mode verify` では**作らない**（verify はこの表に触れない）。
pub struct Supervisor {
    store: Arc<dyn TaskStore>,
    identity: InstanceIdentity,
    /// API と共有する役割（API はこれを読んで `standby` の 503 を返す）。
    role: SharedRole,
    started_at: OffsetDateTime,
    freshness: Duration,
    drain_timeout: Duration,
    /// ADR-0070 D4（Phase 116）: `false`（既定）なら drain timeout で abort しない。
    drain_force_abort: bool,
    /// `draining` になった時刻（drain timeout の基準）。
    drain_started_at: Option<OffsetDateTime>,
    /// ADR-0070 D4: drain timeout の WARN ログをログスパムにしないための一度きりの印。
    drain_timeout_warned: bool,
}

impl Supervisor {
    /// 起動時の判断を行い、`daemon_instances` に自分の行を書く（`standby` なら `active` の行に
    /// `handoff_requested_at` も書く）。同じ `release` の `active` がいれば行を書かずに
    /// `Started::Duplicate` を返す。
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        store: Arc<dyn TaskStore>,
        identity: InstanceIdentity,
        role: SharedRole,
        freshness: Duration,
        drain_timeout: Duration,
        drain_force_abort: bool,
        now: OffsetDateTime,
    ) -> Result<Started, StoreError> {
        let rows = store.instance_list()?;
        let decision = decide_startup(&rows, &identity.release, now, freshness, &pid_alive);
        let initial = match decision {
            StartupDecision::DuplicateRelease { instance_id, pid } => {
                return Ok(Started::Duplicate { instance_id, pid });
            }
            StartupDecision::Active => InstanceRole::Active,
            StartupDecision::Standby { active_instance_id } => {
                // 先に相手へ「引き継ぎたい」と伝える（自分の行を書く前でも後でも結果は同じだが、
                // 先に伝えておけば自分の登録に失敗しても相手は drain を始められる）。
                match store.instance_request_handoff(&active_instance_id, now) {
                    Ok(true) => tracing::info!(
                        active = %active_instance_id, release = %identity.release,
                        "handoff requested (ADR-0040 D4)"
                    ),
                    Ok(false) => tracing::info!(
                        active = %active_instance_id,
                        "handoff was already requested for the active instance"
                    ),
                    Err(e) => return Err(e),
                }
                InstanceRole::Standby
            }
        };
        let supervisor = Self {
            store,
            identity,
            role,
            started_at: now,
            freshness,
            drain_timeout,
            drain_force_abort,
            drain_started_at: None,
            drain_timeout_warned: false,
        };
        supervisor
            .store
            .instance_register(&supervisor.row(initial, now))?;
        supervisor.role.set(initial);
        tracing::info!(
            instance_id = %supervisor.identity.instance_id,
            release = %supervisor.identity.release,
            pid = supervisor.identity.pid,
            role = %initial,
            "instance registered (ADR-0040 D4)"
        );
        Ok(Started::Running(supervisor))
    }

    pub fn identity(&self) -> &InstanceIdentity {
        &self.identity
    }

    pub fn role(&self) -> InstanceRole {
        self.role.get()
    }

    fn row(&self, role: InstanceRole, now: OffsetDateTime) -> DaemonInstance {
        DaemonInstance {
            instance_id: self.identity.instance_id.clone(),
            release: self.identity.release.clone(),
            pid: self.identity.pid,
            role,
            started_at: self.started_at,
            heartbeat_at: now,
            handoff_requested_at: None,
            drained_at: None,
        }
    }

    /// 1 tick 進める。`in_flight` は**このインスタンスが抱えている** run とレビューの数
    /// （`Dispatcher::in_flight`）。heartbeat → 役割の判断 → 古い行の掃除、の順に行う。
    pub fn step(&mut self, now: OffsetDateTime, in_flight: usize) -> Result<Step, StoreError> {
        // 1. heartbeat（何かの拍子に行が消えていたら登録し直す）。
        let current = self.role.get();
        if !self
            .store
            .instance_heartbeat(&self.identity.instance_id, now)?
        {
            tracing::warn!(
                instance_id = %self.identity.instance_id,
                "the daemon_instances row disappeared; registering it again"
            );
            self.store.instance_register(&self.row(current, now))?;
        }
        // 2. 役割の判断。
        let rows = self.store.instance_list()?;
        let mut step = match decide_tick(
            current,
            &self.identity.instance_id,
            &rows,
            now,
            self.freshness,
        ) {
            TickDecision::Stay => Step::Stay,
            TickDecision::Promote => {
                self.store.instance_set_role(
                    &self.identity.instance_id,
                    InstanceRole::Active,
                    now,
                )?;
                self.role.set(InstanceRole::Active);
                tracing::info!(
                    instance_id = %self.identity.instance_id, release = %self.identity.release,
                    "standby -> active (the previous active is draining or gone; ADR-0040 D4)"
                );
                Step::Promoted
            }
            TickDecision::Drain => {
                self.store.instance_set_role(
                    &self.identity.instance_id,
                    InstanceRole::Draining,
                    now,
                )?;
                self.role.set(InstanceRole::Draining);
                self.drain_started_at = Some(now);
                tracing::info!(
                    instance_id = %self.identity.instance_id, release = %self.identity.release, in_flight,
                    "active -> draining (a newer release asked for the handoff; ADR-0040 D4)"
                );
                Step::Draining
            }
        };
        // 3. 終わった・死んだ他のインスタンスの行を消す。
        //
        // Phase 119 D4（監視）: `drained_at` が付いた行は ADR-0040 D4 の設計どおりこの直後に消える
        // （すぐ下の `instance_delete_stale`。旧に `heartbeat_at` の猶予を与えない）。消える前に、
        // その pid がまだ生きていれば WARN を出す — D1/D2 が直した「drain 後にプロセスが終了しない」
        // 障害（本番 2026-09-24）の再発を journal で気付けるようにするための、念のための監視。
        // `GET /health`/`GET /releases` の `instances`（`daemon_instances` をそのまま返す）は、この
        // 行が消える直前の tick に限って同じ `drained_at`/`pid` を見せる（`status.sh` の
        // `stale_instances`〈Phase 119 D3〉は systemd を直接見るのでこの削除タイミングに左右されない）。
        for r in stale_but_alive_rows(&rows, &self.identity.instance_id, &pid_alive) {
            tracing::warn!(
                instance_id = %r.instance_id, release = %r.release, pid = r.pid,
                drained_at = ?r.drained_at,
                "stale: a drained daemon instance's process is still alive (it did not exit \
                 after drain; Phase 119 D1/D4). check with `ps --pid <pid>` or `systemctl \
                 --user status celeris@<release>`"
            );
        }
        let stale_before =
            now - time::Duration::try_from(self.freshness).unwrap_or(time::Duration::MAX);
        match self
            .store
            .instance_delete_stale(&self.identity.instance_id, stale_before)
        {
            Ok(removed) if !removed.is_empty() => {
                tracing::info!(removed = ?removed, "removed drained or dead daemon_instances rows");
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "could not remove the stale daemon_instances rows")
            }
        }
        // 4. drain の進み具合（`Step::Draining` を返した tick では listener を閉じるのが先なので、
        //    drained の判定は次の tick から）。
        if step == Step::Stay && self.role.get() == InstanceRole::Draining {
            if in_flight == 0 {
                self.store
                    .instance_mark_drained(&self.identity.instance_id, now)?;
                tracing::info!(instance_id = %self.identity.instance_id, "drained; exiting 0 (ADR-0040 D4)");
                step = Step::Drained;
            } else if self.drain_timed_out(now) {
                if self.drain_force_abort {
                    self.store
                        .instance_mark_drained(&self.identity.instance_id, now)?;
                    tracing::warn!(
                        instance_id = %self.identity.instance_id, in_flight,
                        drain_timeout_secs = self.drain_timeout.as_secs(),
                        "drain timeout; aborting the remaining runs and exiting 0 (ADR-0040 D4, \
                         drain_force_abort = true)"
                    );
                    step = Step::DrainTimedOut;
                } else if !self.drain_timeout_warned {
                    // ADR-0070 D4（Phase 116）: run のプロセスが生きている限り待つ。abort しない。
                    self.drain_timeout_warned = true;
                    tracing::warn!(
                        instance_id = %self.identity.instance_id, in_flight,
                        drain_timeout_secs = self.drain_timeout.as_secs(),
                        "drain timeout reached but runs are still alive; waiting instead of \
                         aborting (ADR-0070 D4). set [handoff] drain_force_abort = true to force \
                         an abort"
                    );
                }
            }
        }
        Ok(step)
    }

    fn drain_timed_out(&self, now: OffsetDateTime) -> bool {
        let Some(started) = self.drain_started_at else {
            return false;
        };
        let limit = time::Duration::try_from(self.drain_timeout).unwrap_or(time::Duration::MAX);
        now - started >= limit
    }

    /// 終了時に自分の行を消す（SIGTERM / `--until-idle` / `--max-ticks` の停止。drain のときは
    /// `drained_at` を残したまま消える）。失敗しても停止は止めない（次に起きた誰かが古い行として消す）。
    pub fn deregister(&self) {
        match self.store.instance_delete(&self.identity.instance_id) {
            Ok(_) => {
                tracing::info!(instance_id = %self.identity.instance_id, "instance row removed")
            }
            Err(e) => tracing::warn!(error = %e, "could not remove the instance row"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::SqliteStore;

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000 + secs).expect("ts")
    }

    const WINDOW: Duration = Duration::from_secs(30);

    fn row(id: &str, release: &str, role: InstanceRole, heartbeat: i64) -> DaemonInstance {
        DaemonInstance {
            instance_id: id.into(),
            release: release.into(),
            pid: 1234,
            role,
            started_at: at(0),
            heartbeat_at: at(heartbeat),
            handoff_requested_at: None,
            drained_at: None,
        }
    }

    /// `release` は `--release` > `CELERIS_RELEASE` > `"dev"`。
    #[test]
    fn the_release_string_comes_from_the_flag_then_the_env_then_dev() {
        assert_eq!(resolve_release(Some("abc123def456")), "abc123def456");
        assert_eq!(resolve_release(Some("  spaced  ")), "spaced");
        // 環境変数はプロセス全体なので、この 1 つのテストの中だけで触る。
        assert_eq!(resolve_release(None), DEV_RELEASE);
        unsafe { std::env::set_var(RELEASE_ENV, "fromenv12345") };
        assert_eq!(resolve_release(None), "fromenv12345");
        assert_eq!(
            resolve_release(Some("cli12345")),
            "cli12345",
            "flag wins over the env"
        );
        unsafe { std::env::set_var(RELEASE_ENV, "  ") };
        assert_eq!(
            resolve_release(None),
            DEV_RELEASE,
            "blank env is not a release"
        );
        unsafe { std::env::remove_var(RELEASE_ENV) };

        let identity = InstanceIdentity::new(Some("sha12sha12ab"));
        assert_eq!(identity.release, "sha12sha12ab");
        assert_eq!(identity.pid, std::process::id());
        assert!(!identity.instance_id.is_empty());
    }

    /// ADR-0040 D4: `3 × tick + lease_grace`。
    #[test]
    fn the_freshness_window_is_three_ticks_plus_the_lease_grace() {
        assert_eq!(
            freshness_window(Duration::from_millis(500), 30),
            Duration::from_millis(31_500)
        );
        assert_eq!(
            freshness_window(Duration::from_secs(1), 0),
            Duration::from_secs(3)
        );
    }

    /// (d) 同じ版の `active` が生きていれば二重起動なので exit 3 の判断になる。
    #[test]
    fn the_same_release_running_as_active_is_a_duplicate() {
        let live = |_: u32| true;
        let rows = vec![row("old", "sha-aaa", InstanceRole::Active, 0)];
        assert_eq!(
            decide_startup(&rows, "sha-aaa", at(1), WINDOW, &live),
            StartupDecision::DuplicateRelease {
                instance_id: "old".into(),
                pid: 1234
            }
        );
        // heartbeat が古ければ二重起動ではない（前のプロセスは死んでいる）。
        assert_eq!(
            decide_startup(&rows, "sha-aaa", at(31), WINDOW, &live),
            StartupDecision::Active
        );
        // `drained_at` が付いた行も同様に数えない。
        let mut drained = rows.clone();
        drained[0].drained_at = Some(at(0));
        assert_eq!(
            decide_startup(&drained, "sha-aaa", at(1), WINDOW, &live),
            StartupDecision::Active
        );
        // heartbeat が新しくてもプロセスが消えていれば二重起動ではない（SIGKILL 直後の起こし直し）。
        assert_eq!(
            decide_startup(&rows, "sha-aaa", at(1), WINDOW, &|_| false),
            StartupDecision::Active
        );
        // 自分自身の pid は必ず生きている（`pid_alive` の素振り）。
        assert!(pid_alive(std::process::id()));
        assert!(pid_alive(0), "pid が分からない行は生きている扱い");
    }

    /// 別の版の `active` がいれば standby になり、その行に引き継ぎを要求する。
    #[test]
    fn a_different_release_becomes_standby_and_an_empty_table_becomes_active() {
        let live = |_: u32| true;
        let rows = vec![row("old", "sha-aaa", InstanceRole::Active, 0)];
        assert_eq!(
            decide_startup(&rows, "sha-bbb", at(1), WINDOW, &live),
            StartupDecision::Standby {
                active_instance_id: "old".into()
            }
        );
        assert_eq!(
            decide_startup(&[], "sha-bbb", at(1), WINDOW, &live),
            StartupDecision::Active
        );
        // `standby` / `draining` / `verify` の行は「active がいる」ことにならない。
        let others = vec![
            row("s", "sha-ccc", InstanceRole::Standby, 0),
            row("d", "sha-ddd", InstanceRole::Draining, 0),
            row("v", "sha-eee", InstanceRole::Verify, 0),
        ];
        assert_eq!(
            decide_startup(&others, "sha-bbb", at(1), WINDOW, &live),
            StartupDecision::Active
        );
    }

    /// 毎 tick の規則: active は要求を見たら drain、standby は active が消えたら promote。
    #[test]
    fn the_tick_rules_are_symmetric() {
        let mut me = row("me", "new", InstanceRole::Active, 0);
        assert_eq!(
            decide_tick(InstanceRole::Active, "me", &[me.clone()], at(1), WINDOW),
            TickDecision::Stay
        );
        me.handoff_requested_at = Some(at(1));
        assert_eq!(
            decide_tick(InstanceRole::Active, "me", &[me.clone()], at(1), WINDOW),
            TickDecision::Drain
        );

        let standby = row("me", "new", InstanceRole::Standby, 0);
        let old_active = row("old", "prev", InstanceRole::Active, 0);
        let rows = vec![standby.clone(), old_active.clone()];
        assert_eq!(
            decide_tick(InstanceRole::Standby, "me", &rows, at(1), WINDOW),
            TickDecision::Stay
        );
        // (a) 旧が draining になったら昇格する。
        let mut draining = rows.clone();
        draining[1].role = InstanceRole::Draining;
        assert_eq!(
            decide_tick(InstanceRole::Standby, "me", &draining, at(1), WINDOW),
            TickDecision::Promote
        );
        // (e) heartbeat が止まっても昇格する。
        assert_eq!(
            decide_tick(InstanceRole::Standby, "me", &rows, at(31), WINDOW),
            TickDecision::Promote
        );
        // draining と verify は役割を変えない。
        assert_eq!(
            decide_tick(InstanceRole::Draining, "me", &rows, at(31), WINDOW),
            TickDecision::Stay
        );
        assert_eq!(
            decide_tick(InstanceRole::Verify, "me", &[], at(31), WINDOW),
            TickDecision::Stay
        );
    }

    /// Phase 119 D4: `drained_at` が付いた行のうち、pid がまだ生きている（＝「drain 後にプロセスが
    /// 終了しない」障害）ものだけを拾う。自分自身の行・`drained_at` 無し・pid が死んでいる行は拾わない。
    #[test]
    fn stale_but_alive_rows_finds_only_drained_rows_whose_pid_is_still_running() {
        let mut still_stuck = row("stuck", "old-rel", InstanceRole::Draining, 0);
        still_stuck.drained_at = Some(at(5));
        still_stuck.pid = 9001;
        let mut cleanly_exited = row("exited", "older-rel", InstanceRole::Draining, 0);
        cleanly_exited.drained_at = Some(at(3));
        cleanly_exited.pid = 9002;
        let still_draining = row("draining", "mid-rel", InstanceRole::Draining, 10);
        // まだ drain していない（`drained_at` 無し）行は、pid が生きていても対象外。
        let mut myself = row("me", "new-rel", InstanceRole::Active, 10);
        myself.drained_at = Some(at(5)); // 自分自身は（理屈上あり得なくても）除外される。
        myself.pid = 9001;

        let rows = vec![
            still_stuck.clone(),
            cleanly_exited.clone(),
            still_draining,
            myself,
        ];
        let alive = |pid: u32| pid == 9001; // 9001 だけ生きている扱い。9002 は死んでいる。

        let found = stale_but_alive_rows(&rows, "me", &alive);
        assert_eq!(
            found
                .iter()
                .map(|r| r.instance_id.as_str())
                .collect::<Vec<_>>(),
            vec!["stuck"],
            "only the still-running, drained, non-self row should be flagged"
        );
    }

    fn supervisor(
        store: &Arc<dyn TaskStore>,
        release: &str,
        now: OffsetDateTime,
        drain: Duration,
    ) -> Started {
        supervisor_with(store, release, now, drain, false)
    }

    /// ADR-0070 D4（Phase 116）: `drain_force_abort` を選べる版。既定の `supervisor()` は常に `false`
    /// （drain timeout で abort しない、が既定の挙動）。
    fn supervisor_with(
        store: &Arc<dyn TaskStore>,
        release: &str,
        now: OffsetDateTime,
        drain: Duration,
        drain_force_abort: bool,
    ) -> Started {
        Supervisor::start(
            Arc::clone(store),
            InstanceIdentity {
                instance_id: format!("inst-{release}"),
                release: release.into(),
                pid: 1,
            },
            SharedRole::new(InstanceRole::Standby),
            WINDOW,
            drain,
            drain_force_abort,
            now,
        )
        .expect("start")
    }

    /// (a) 新 standby → 旧 draining → 新 active、を `Supervisor` の一連の呼び出しで確かめる（DB 付き）。
    #[test]
    fn the_handoff_runs_through_the_store() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let Started::Running(mut old) = supervisor(&store, "old", at(0), Duration::from_secs(60))
        else {
            panic!("the first instance must become active");
        };
        assert_eq!(old.role(), InstanceRole::Active);

        let Started::Running(mut new) = supervisor(&store, "new", at(1), Duration::from_secs(60))
        else {
            panic!("a different release must become standby");
        };
        assert_eq!(new.role(), InstanceRole::Standby);
        let rows = store.instance_list().expect("list");
        let old_row = rows
            .iter()
            .find(|r| r.instance_id == "inst-old")
            .expect("old row");
        assert_eq!(
            old_row.handoff_requested_at,
            Some(at(1)),
            "standby は active に引き継ぎを要求する"
        );

        // 旧は次の tick で draining になる（手元に run が 1 つあるのでまだ終わらない）。
        assert_eq!(old.step(at(2), 1).expect("step"), Step::Draining);
        assert_eq!(old.role(), InstanceRole::Draining);
        // 新はそれを見て active になる。
        assert_eq!(new.step(at(3), 0).expect("step"), Step::Promoted);
        assert_eq!(new.role(), InstanceRole::Active);
        assert_eq!(new.step(at(4), 0).expect("step"), Step::Stay);

        // 旧は run が終わったら drained になり、その行は新 active が掃除する。
        assert_eq!(old.step(at(5), 1).expect("step"), Step::Stay);
        assert_eq!(old.step(at(6), 0).expect("step"), Step::Drained);
        assert!(
            store
                .instance_list()
                .expect("list")
                .iter()
                .any(|r| r.drained_at == Some(at(6)))
        );
        assert_eq!(new.step(at(7), 0).expect("step"), Step::Stay);
        let left: Vec<String> = store
            .instance_list()
            .expect("list")
            .into_iter()
            .map(|r| r.instance_id)
            .collect();
        assert_eq!(left, ["inst-new"], "drained の行は消える");
    }

    /// (d) 同じ版をもう一度起こしたら行を書かずに `Duplicate`。
    #[test]
    fn starting_the_same_release_twice_is_refused_without_touching_the_table() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let Started::Running(_first) = supervisor(&store, "same", at(0), Duration::from_secs(60))
        else {
            panic!("the first instance must become active");
        };
        match supervisor(&store, "same", at(1), Duration::from_secs(60)) {
            Started::Duplicate { instance_id, pid } => {
                assert_eq!((instance_id.as_str(), pid), ("inst-same", 1));
            }
            Started::Running(_) => panic!("the same release must not start twice"),
        }
        assert_eq!(
            store.instance_list().expect("list").len(),
            1,
            "行は増えない"
        );
    }

    /// ADR-0070 D4（Phase 116）: `drain_force_abort = true` のときだけ、drain timeout を過ぎたら
    /// run が残っていても `DrainTimedOut` になる（従来の挙動、人が明示的に強い昇格を選んだとき）。
    #[test]
    fn the_drain_timeout_ends_the_drain_with_runs_left_when_force_abort_is_set() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let Started::Running(mut old) =
            supervisor_with(&store, "old", at(0), Duration::from_secs(10), true)
        else {
            panic!("active");
        };
        let Started::Running(_new) =
            supervisor_with(&store, "new", at(1), Duration::from_secs(10), true)
        else {
            panic!("standby");
        };
        assert_eq!(old.step(at(2), 3).expect("step"), Step::Draining);
        assert_eq!(old.step(at(5), 3).expect("step"), Step::Stay);
        assert_eq!(old.step(at(12), 3).expect("step"), Step::DrainTimedOut);
        let row = store
            .instance_list()
            .expect("list")
            .into_iter()
            .find(|r| r.instance_id == "inst-old")
            .expect("row");
        assert_eq!(row.drained_at, Some(at(12)));
    }

    /// ADR-0070 D4（Phase 116。D6(d)）: 既定（`drain_force_abort = false`）では、drain timeout を
    /// 過ぎても run が残っている間は `DrainTimedOut` にならない（`Step::Stay` のまま待ち続け、
    /// `drained_at` も書かない）。手元の run が本当に 0 になったときだけ `Drained` になる。
    #[test]
    fn the_drain_timeout_does_not_abort_alive_runs_by_default() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let Started::Running(mut old) = supervisor(&store, "old", at(0), Duration::from_secs(10))
        else {
            panic!("active");
        };
        let Started::Running(_new) = supervisor(&store, "new", at(1), Duration::from_secs(10))
        else {
            panic!("standby");
        };
        assert_eq!(old.step(at(2), 3).expect("step"), Step::Draining);
        assert_eq!(old.step(at(5), 3).expect("step"), Step::Stay);
        // drain_timeout_secs (10) を過ぎても、in_flight が 0 でない限り待ち続ける。
        assert_eq!(old.step(at(12), 3).expect("step"), Step::Stay);
        assert_eq!(old.step(at(3600), 1).expect("step"), Step::Stay);
        let row = store
            .instance_list()
            .expect("list")
            .into_iter()
            .find(|r| r.instance_id == "inst-old")
            .expect("row");
        assert!(
            row.drained_at.is_none(),
            "abort していないので drained_at は書かれない"
        );
        // 手元の run が 0 になれば、通常どおり Drained で終わる。
        assert_eq!(old.step(at(3601), 0).expect("step"), Step::Drained);
    }

    /// (e) 旧の heartbeat が止まったら standby は昇格し、旧の行を消す。
    #[test]
    fn a_stale_heartbeat_promotes_the_standby_and_removes_the_dead_row() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let Started::Running(_old) = supervisor(&store, "old", at(0), Duration::from_secs(60))
        else {
            panic!("active");
        };
        let Started::Running(mut new) = supervisor(&store, "new", at(1), Duration::from_secs(60))
        else {
            panic!("standby");
        };
        assert_eq!(new.step(at(2), 0).expect("step"), Step::Stay);
        // 旧が heartbeat を打たないまま窓を過ぎる。
        assert_eq!(new.step(at(100), 0).expect("step"), Step::Promoted);
        let left: Vec<String> = store
            .instance_list()
            .expect("list")
            .into_iter()
            .map(|r| r.instance_id)
            .collect();
        assert_eq!(left, ["inst-new"]);
    }

    /// 行が消えても heartbeat で気づいて登録し直す（`deregister` は自分の行だけ消す）。
    #[test]
    fn a_missing_row_is_registered_again_and_deregister_removes_it() {
        let store: Arc<dyn TaskStore> = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let Started::Running(mut only) = supervisor(&store, "solo", at(0), Duration::from_secs(60))
        else {
            panic!("active");
        };
        assert!(store.instance_delete("inst-solo").expect("delete"));
        assert_eq!(only.step(at(1), 0).expect("step"), Step::Stay);
        let rows = store.instance_list().expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].role, InstanceRole::Active);
        only.deregister();
        assert!(store.instance_list().expect("list").is_empty());
    }
}
