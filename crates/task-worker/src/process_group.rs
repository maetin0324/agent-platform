//! ADR-0044 §5「Phase 53 追記」（Phase 55）: **run の止め方を 1 つにする**。
//!
//! 走っている run を止める道は 5 つある（`cancel` / ADR-0044 D2 の `Interrupt` / 実時間・無入力の
//! タイムアウト / リース喪失 = `abort_stale_runs` / ADR-0040 の drain タイムアウト）。Phase 54 までは
//! タイムアウトだけが `subprocess::kill_now` で**プロセスグループ**に SIGTERM → `kill_grace_secs` →
//! SIGKILL を送り、残りは tokio の `JoinHandle::abort()` に任せていた。`abort()` は future を落とすので
//! `kill_on_drop(true)` が効くが、それは**直接の子だけ**を SIGKILL する。ハーネス（`claude` / `codex`）が
//! 起こした孫（`cargo test`、`node`、シェル）はそのまま走り続け、worktree を掴んだままになる。
//!
//! このモジュールは「run_id → その run の子プロセスの pid（= プロセスグループ id）」の表を 1 つ持ち、
//! **どの経路からでも同じ止め方**（グループへ SIGTERM → `grace` → グループへ SIGKILL）ができるようにする。
//!
//! - 登録はアダプタの spawn 直後（1 行）。`Registration` は RAII で、run の future が終わる・落とされる
//!   ときに自動で表から消える。
//! - 止めるのは `kill_tree(run_id, grace)`。SIGTERM は同期に送り、`grace` 後の SIGKILL は別スレッドで送る
//!   （ディスパッチャの tick を `grace` 秒止めないため）。
//! - 全アダプタの `Command` は既に `process_group(0)` を付けているので、子は自分を長とする新しい
//!   プロセスグループに入る。したがって `killpg(子の pid)` がその run の一族全部に届く。
//!
//! ## コンテナで走る run（Phase 55/56 の合流。ADR-0044 P55-4 / ADR-0043 P56-7）
//!
//! ADR-0043 D3（Phase 56）が入ってからは、run が `<runtime> run --rm -i …` に包まれていることがある。
//! そのときプロセスグループの長は**runtime のクライアント**で、`killpg` は**コンテナの中の PID 名前空間
//! には届かない**。そこで [`kill_tree_with`] に [`crate::container::ContainerStopper`] を渡すと、
//! ホストへの 2 段（SIGTERM → `grace` → SIGKILL）と**同じ瞬間**に
//! `<runtime> kill --signal TERM --label celeris.task=<task_id>` →（`grace` 後）`rm -f` を送る。
//! 合流点はこの 1 関数だけで、呼ぶのは `Dispatcher::stop_run` である。
//!
//! I/O も LLM も持たない（シグナルを送るだけ）。
//!
//! ## Phase 119 D2: 正常終了（done / error / question / yield / budget）でも孤児を残さない
//!
//! Phase 54 まで直していたのは「タイムアウト・cancel・drain タイムアウトで打ち切られた run」だけだった。
//! **run が自分から終わった**（アダプタの spawn した子が exit する）経路では誰もプロセスグループへ
//! signal を送っていなかった。ワーカー自身の直接の子（`claude` / `codex` 本体）は `child.wait()` で
//! reap されて消えるが、その子が**バックグラウンドジョブとして** `&` で起こした孫プロセスは、
//! プロセスグループの長（＝直接の子）が死んでも生き残る。
//!
//! **最初の実装（`Drop for ProcessGroup` に SIGTERM を持たせる）は撤回した**: [`ProcessGroup`] の
//! guard は、アダプタが `child.wait()` を終えてから（`stream_child` のさらに後始末や呼び出し元の
//! 残りの処理を経て）関数が返るときに drop される。その `await` を挟む間に、reap 済みで**空いた
//! pid** を（高い並列度で大量に子プロセスを spawn する）別の run が拾ってしまうことがあり、
//! `Drop` から送った SIGTERM が**無関係な、まだ生きている別の run の子**に当たった（実機ではなく
//! `cargo test -p task-worker --lib paperqa::` で 6 件の非決定的な失敗として顕在化。詳細は
//! ADR-0040 追記の「Phase 119 D2 の撤回」）。
//!
//! 代わりに [`crate::subprocess::reap_after_terminal`] の**リーダーの `child.wait()` が返った直後、
//! 一切 `await` を挟まずに**同じプロセスグループへ SIGTERM を送るようにした（[`sweep`]）。ここが
//! `killpg` を送る全経路の中で**最も新鮮な**タイミングであり、pid 再利用の危険が最小になる
//! （abort 系の `kill_tree`/`kill_tree_with` と同じ「レジストリから引いた直後に送る」規律に揃う）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use tracing::warn;

use crate::container::ContainerStopper;

/// `run_id` → プロセスグループ id（= その run の直接の子の pid）。
fn registry() -> &'static Mutex<HashMap<String, i32>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, i32>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_registry<T>(f: impl FnOnce(&mut HashMap<String, i32>) -> T) -> T {
    let mut guard = registry().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// spawn 直後に置く RAII の登録。`child.id()` が `None`（既に終了している）なら何もしない。
///
/// 使い方（アダプタの spawn 直後に 1 行）:
/// ```ignore
/// let _pg = ProcessGroup::register(run_id, child.id());
/// ```
#[derive(Debug)]
pub struct ProcessGroup {
    run_id: Option<String>,
}

impl ProcessGroup {
    /// `pid` は `tokio::process::Child::id()`（`process_group(0)` で起こしているので pid = pgid）。
    pub fn register(run_id: &str, pid: Option<u32>) -> Self {
        let Some(pid) = pid else {
            return Self { run_id: None };
        };
        with_registry(|map| map.insert(run_id.to_string(), pid as i32));
        Self {
            run_id: Some(run_id.to_string()),
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(run_id) = &self.run_id {
            with_registry(|map| map.remove(run_id));
        }
    }
}

/// その run のプロセスグループ id（登録されていなければ `None`）。
pub fn pgid_of(run_id: &str) -> Option<i32> {
    with_registry(|map| map.get(run_id).copied())
}

/// ADR-0070 D5（Phase 116）: その run のプロセスグループがまだ生きているか。登録が無ければ `false`、
/// 登録があっても signal 0（`kill(pid, 0)` と同じ。実際には送らず存在確認だけする）で `ESRCH` なら
/// `false`（ゾンビでも `Ok` を返すので、回収されない限り「生きている」扱いになる）。`Dispatcher::
/// reclaim_expired_leases` が「lease は切れたが自分が起こした run のプロセスはまだ生きている」を
/// 見分けるために使う（reclaim せず lease を延長する）。
pub fn group_alive(run_id: &str) -> bool {
    match pgid_of(run_id) {
        Some(pgid) => signal::kill(Pid::from_raw(pgid), None).is_ok(),
        None => false,
    }
}

/// Phase 119 D2: ゾンビ（`<defunct>`）になった子を回収する。
///
/// `tokio::process::Child` の `kill_on_drop(true)` は、drop 時に自分の runtime へ「後で reap する」
/// タスクを投げる（Drop は同期なので、reap 自体は非同期にやるしかない）。その run の future が
/// `JoinHandle::abort()` でちょうど shutdown の直前に落とされた等、runtime が reap の完了を待たずに
/// 終わってしまうと、子は `wait(2)` されないまま zombie として残る（本番 2026-09-24 で
/// `[codex] <defunct>` を確認）。
///
/// ここは `waitpid(-1, WNOHANG)` で「回収できる子がもう無い」（`ECHILD`）まで拾い切る決定的な掃除。
/// **runtime が完全に止まった後にだけ呼ぶこと**: tokio は SIGCHLD 駆動で自分の追跡している子を
/// `waitid`/`try_wait` しているので、runtime が動いている間にここを呼ぶと、まだ生きている run の
/// `child.wait()` が受け取るはずの終了ステータスを横取りしてしまう（`main.rs` は
/// `Runtime::shutdown_timeout` の**後**にだけ呼ぶ）。
pub fn reap_finished_children() -> usize {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    let mut reaped = 0usize;
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => break,
            Ok(_) => reaped += 1,
            Err(Errno::ECHILD) => break,
            Err(e) => {
                warn!(error = %e, "reap_finished_children: waitpid failed");
                break;
            }
        }
    }
    reaped
}

/// プロセスグループへ signal を送る。もう居なければ（`ESRCH`）`false`。
pub fn signal_group(pgid: i32, sig: Signal) -> bool {
    match signal::killpg(Pid::from_raw(pgid), sig) {
        Ok(()) => true,
        Err(Errno::ESRCH) => false,
        Err(e) => {
            warn!(pgid, ?sig, error = %e, "failed to signal the worker process group");
            false
        }
    }
}

/// **run を止める唯一の入口**（ADR-0044 Phase 53 追記）。
///
/// その run のプロセスグループに SIGTERM を送り、`grace` 後に SIGKILL を送る（後者は別スレッド）。
/// 登録が無い（既に終わった run、プロセスを持たないアダプタ）なら何もせず `false`。
///
/// 呼び出し側は従来どおり `JoinHandle::abort()` も行う（run の記録を止めるための帳簿。直接の子は
/// `kill_on_drop` で即 SIGKILL されるが、ハーネスが起こした孫はここで送った SIGTERM を受け取って
/// 片付けの猶予を持つ）。
pub fn kill_tree(run_id: &str, grace: Duration) -> bool {
    kill_tree_with(run_id, grace, None)
}

/// [`kill_tree`] に**コンテナの口**（ADR-0043 P56-7 の [`ContainerStopper`]）を足した版。
///
/// ADR-0044 P55-4 / ADR-0043 P56-7（Phase 55/56 の合流）: `<runtime> run` のクライアントの pid へ
/// `killpg` を撃っても、**コンテナの中のプロセスには届かない**（別の PID 名前空間）。そこで
/// コンテナで走っている run では、ホストのプロセスグループへ送るのと**同じ 2 段**を
/// `--label celeris.task=<task_id>` 越しにも送る:
///
/// | 時点 | ホスト | コンテナ |
/// |---|---|---|
/// | すぐ | `killpg(SIGTERM)` | `<runtime> kill --signal TERM <ids>` |
/// | `grace` 後 | `killpg(SIGKILL)` | `<runtime> rm -f <ids>` |
///
/// これが**唯一の合流点**である（呼ぶ側は `Dispatcher::stop_run` 1 か所）。`container` が `None` なら
/// 従来と 1 バイトも変わらない。プロセスグループの登録が無くても `container` があれば
/// コンテナ側だけは止める（クライアントが先に死んでコンテナが取り残された場合）。
pub fn kill_tree_with(
    run_id: &str,
    grace: Duration,
    container: Option<Arc<dyn ContainerStopper>>,
) -> bool {
    let pgid = pgid_of(run_id);
    if pgid.is_some() {
        // 表からは先に落とす（同じ run に二重に止めを掛けない）。
        with_registry(|map| map.remove(run_id));
    }
    let signalled = match pgid {
        Some(pgid) => signal_group(pgid, Signal::SIGTERM),
        None => false,
    };
    if let Some(stopper) = &container {
        stopper.terminate_blocking();
    }
    if !signalled && container.is_none() {
        return false;
    }
    spawn_group_reaper(pgid.unwrap_or(0), grace, container);
    true
}

/// Phase 119 D2: リーダーが**自分から**終わった直後（`await` を挟まず、`child.id()` を取ってから
/// `child.wait()` が返るまでの間だけ）に、`crate::subprocess::reap_after_terminal` から呼ぶ。
/// `registry`（`run_id` の表）は経由しない — `pid` は呼び出し元がその場で `child.id()` から直接
/// 取ったばかりの新鮮な値であることが前提（`kill_tree` 系のように後から表を引くと、その間の
/// `await` で pid が再利用されているおそれがある。このモジュール先頭のコメント参照）。
///
/// プロセスグループが既に空（孤児が無い、ふつうのケース）なら `killpg` が `ESRCH` を返すので
/// 無害な no-op（追加のスレッドも立てない）。孤児が残っていれば SIGTERM → `grace` 後 SIGKILL。
pub fn sweep(pid: u32, grace: Duration) {
    let pgid = pid as i32;
    if signal_group(pgid, Signal::SIGTERM) {
        spawn_group_reaper(pgid, grace, None);
    }
}

/// `grace` 後に SIGKILL（と、あればコンテナの `rm -f`）を送る別スレッドを起こす。tokio のランタイムに
/// 依らない（`Drop`・`tick()` どちらも同期の文脈から呼ばれる）。`pgid == 0` はコンテナだけを持つ
/// （プロセスグループの登録が無い）呼び出しの印で、SIGKILL は送らない。
///
/// pid の再利用は理論上あり得るが、`killpg` は「その pgid のグループ長」にしか届かず、Linux の pid は
/// 上限まで順に配られるので `grace`（既定 10 秒）の間に一周することはない。
fn spawn_group_reaper(pgid: i32, grace: Duration, container: Option<Arc<dyn ContainerStopper>>) {
    std::thread::Builder::new()
        .name("celeris-killpg".to_string())
        .spawn(move || {
            std::thread::sleep(grace);
            if pgid != 0 {
                signal_group(pgid, Signal::SIGKILL);
            }
            if let Some(stopper) = container {
                stopper.stop_blocking();
            }
        })
        .map_err(|e| warn!(pgid, error = %e, "could not spawn the SIGKILL timer thread"))
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// このモジュールの実プロセスを使う非同期テストを直列化する。
    /// `reap_finished_children` のテストは process 全体を対象とするため、
    /// 別の integration-test binary で実行する。
    /// `tokio::sync::Mutex` を使うのは、await をまたいでガードを持ち続ける `#[tokio::test]` が
    /// `std::sync::Mutex` だと clippy（`await_holding_lock`）に落ちるため。
    fn child_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// 非同期テスト（`#[tokio::test]`）用。
    async fn serialize_child_process_tests() -> tokio::sync::MutexGuard<'static, ()> {
        child_test_lock().lock().await
    }

    #[test]
    fn registration_is_visible_until_it_is_dropped() {
        let run_id = format!("reg-{}", std::process::id());
        {
            let _guard = ProcessGroup::register(&run_id, Some(4_000_000));
            assert_eq!(pgid_of(&run_id), Some(4_000_000));
        }
        assert_eq!(pgid_of(&run_id), None);
    }

    /// Phase 119 D2: `sweep(pid, grace)` はそのプロセスグループへ SIGTERM を送る（登録テーブルは
    /// 経由しない、`child.id()` から直接渡された pid で動く）。孤児が居なければ `killpg` が
    /// `ESRCH` を返すだけの no-op であることも確認する（`spawn_group_reaper` の余計なスレッドを
    /// 立てない）。
    #[tokio::test]
    async fn sweep_signals_the_group_and_is_a_no_op_when_nothing_is_left() {
        let _serial = serialize_child_process_tests().await;
        let mut command = tokio::process::Command::new("sleep");
        command.arg("300").kill_on_drop(true);
        command.process_group(0);
        let mut child = command.spawn().unwrap_or_else(|e| panic!("spawn: {e}"));
        let pid = child.id().expect("child pid");

        sweep(pid, Duration::from_secs(10));
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap_or_else(|_| panic!("the process should have exited after sweep's SIGTERM"))
            .unwrap_or_else(|e| panic!("wait: {e}"));
        assert!(
            !status.success(),
            "sleep should have been signalled, not exited on its own"
        );

        // 掃除対象が既に居ない（既に reap 済みの pid）なら no-op。
        assert!(!signal_group(pid as i32, Signal::SIGTERM));
    }

    /// ADR-0070 D5（Phase 116）: 登録された偽の子プロセスが生きている間は `group_alive` が `true`、
    /// プロセスが死んでも登録がまだ残っていれば（`Dispatcher::reclaim_expired_leases` が
    /// `self.running` を消す前に見る、まさにこの状況）`false` になる。未登録の run は常に `false`。
    #[tokio::test]
    async fn group_alive_reflects_whether_the_registered_process_is_still_running() {
        let _serial = serialize_child_process_tests().await;
        assert!(!group_alive("no-such-run"));

        let mut command = tokio::process::Command::new("sleep");
        command.arg("300").kill_on_drop(true);
        command.process_group(0);
        let mut child = command.spawn().unwrap_or_else(|e| panic!("spawn: {e}"));
        let pid = child.id().expect("child pid");
        let run_id = format!("alive-{}", std::process::id());
        let guard = ProcessGroup::register(&run_id, Some(pid));
        assert!(group_alive(&run_id), "just-spawned child should be alive");

        // 登録はそのままに、プロセスだけを直接殺す（`kill_tree` は先に登録を消してしまうので使わない）。
        signal::kill(nix::unistd::Pid::from_raw(pid as i32), Signal::SIGKILL)
            .expect("kill the fake child");
        let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        assert!(
            !group_alive(&run_id),
            "a dead process should not be reported as alive even while still registered"
        );
        drop(guard);
        assert!(
            !group_alive(&run_id),
            "dropping the guard also unregisters it"
        );
    }

    #[test]
    fn a_child_without_a_pid_registers_nothing() {
        let run_id = format!("nopid-{}", std::process::id());
        let _guard = ProcessGroup::register(&run_id, None);
        assert_eq!(pgid_of(&run_id), None);
    }

    #[test]
    fn killing_an_unknown_run_is_a_no_op() {
        assert!(!kill_tree("no-such-run", Duration::from_millis(1)));
    }

    /// 実プロセスで一族ごと止まることを見る（孫まで）。
    #[tokio::test]
    async fn kill_tree_terminates_the_whole_group_including_grandchildren() {
        let _serial = serialize_child_process_tests().await;
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let pidfile = dir.path().join("grandchild.pid");
        let script = format!("sleep 300 & echo $! > {}; wait", pidfile.to_string_lossy());
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(&script)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command.process_group(0);
        let mut child = command.spawn().unwrap_or_else(|e| panic!("spawn: {e}"));
        let run_id = format!("group-{}", std::process::id());
        let guard = ProcessGroup::register(&run_id, child.id());

        // 孫の pid が書かれるのを待つ（上限付き）。
        let grandchild = wait_for_pid(&pidfile).await;

        assert!(kill_tree(&run_id, Duration::from_millis(200)));
        let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        drop(guard);

        // 孫も居なくなる（SIGTERM で `sleep` は死ぬ）。
        for _ in 0..100 {
            if !pid_alive(grandchild) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the grandchild {grandchild} is still alive after kill_tree");
    }

    /// Phase 55/56 の合流（ADR-0044 P55-4 / ADR-0043 P56-7）: コンテナで走る run では、ホストの
    /// プロセスグループへの 2 段と**同じ瞬間**に `<runtime> kill --signal TERM` →（`grace` 後）
    /// `rm -f` がラベル越しに出る。偽の runtime に argv を記録させて確かめる。
    #[cfg(unix)]
    #[tokio::test]
    async fn a_containerized_run_is_also_stopped_inside_the_container() {
        let _serial = serialize_child_process_tests().await;
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let log = dir.path().join("argv.log");
        let runtime = write_fake_runtime(dir.path(), &log);

        // ホスト側: 孫を持つ本物のプロセスグループ（`<runtime> run` のクライアントに相当）。
        let pidfile = dir.path().join("grandchild.pid");
        let script = format!("sleep 300 & echo $! > {}; wait", pidfile.to_string_lossy());
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(&script)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command.process_group(0);
        let mut child = command.spawn().unwrap_or_else(|e| panic!("spawn: {e}"));
        let run_id = format!("container-{}", std::process::id());
        let guard = ProcessGroup::register(&run_id, child.id());
        let grandchild = wait_for_pid(&pidfile).await;

        let stopper: Arc<dyn ContainerStopper> = Arc::new(crate::container::ContainerStop {
            program: runtime.to_string_lossy().into_owned(),
            task_id: "01TASKCONTAINER".into(),
        });
        assert!(kill_tree_with(
            &run_id,
            Duration::from_millis(200),
            Some(stopper)
        ));

        // SIGTERM の段は同期に出ている。
        let first = read_lines(&log);
        assert_eq!(
            first,
            vec![
                "ps -aq --filter label=celeris.task=01TASKCONTAINER",
                "kill --signal TERM c0ffee111111",
            ],
            "the container did not get a SIGTERM at the same moment as the process group"
        );

        // `grace` の後に `rm -f`（別スレッド）。
        let lines = wait_for_lines(&log, 4).await;
        assert_eq!(
            lines,
            vec![
                "ps -aq --filter label=celeris.task=01TASKCONTAINER",
                "kill --signal TERM c0ffee111111",
                "ps -aq --filter label=celeris.task=01TASKCONTAINER",
                "rm -f c0ffee111111",
            ],
            "the container was not removed after the grace period"
        );

        // ホスト側は従来どおり（孫まで消える）。
        let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        drop(guard);
        for _ in 0..100 {
            if !pid_alive(grandchild) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the grandchild {grandchild} is still alive after kill_tree_with");
    }

    /// 登録が無くても（クライアントが先に死んでコンテナだけ残った）コンテナは止める。
    #[cfg(unix)]
    #[tokio::test]
    async fn a_container_is_stopped_even_when_the_process_group_is_already_gone() {
        let _serial = serialize_child_process_tests().await;
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let log = dir.path().join("argv.log");
        let runtime = write_fake_runtime(dir.path(), &log);
        let stopper: Arc<dyn ContainerStopper> = Arc::new(crate::container::ContainerStop {
            program: runtime.to_string_lossy().into_owned(),
            task_id: "01TASKORPHAN".into(),
        });
        assert!(kill_tree_with(
            "no-such-run",
            Duration::from_millis(100),
            Some(stopper)
        ));
        let lines = wait_for_lines(&log, 4).await;
        assert_eq!(lines[1], "kill --signal TERM c0ffee111111");
        assert_eq!(lines[3], "rm -f c0ffee111111");
    }

    /// `ps -aq --filter …` に 1 件返し、呼ばれた argv を 1 行ずつ記録する偽の runtime。
    #[cfg(unix)]
    fn write_fake_runtime(dir: &std::path::Path, log: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-runtime");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nif [ \"$1\" = ps ]; then echo c0ffee111111; fi\nexit 0\n",
                log.to_string_lossy()
            ),
        )
        .unwrap_or_else(|e| panic!("write fake runtime: {e}"));
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    fn read_lines(path: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    async fn wait_for_lines(path: &std::path::Path, want: usize) -> Vec<String> {
        for _ in 0..200 {
            let lines = read_lines(path);
            if lines.len() >= want {
                return lines;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the fake runtime recorded only {:?}", read_lines(path));
    }

    async fn wait_for_pid(path: &std::path::Path) -> i32 {
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the grandchild never wrote its pid to {}", path.display());
    }

    fn pid_alive(pid: i32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}
