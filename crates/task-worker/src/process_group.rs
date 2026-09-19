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
//! I/O も LLM も持たない（シグナルを送るだけ）。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use tracing::warn;

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
    let Some(pgid) = pgid_of(run_id) else {
        return false;
    };
    // 表からは先に落とす（同じ run に二重に止めを掛けない）。
    with_registry(|map| map.remove(run_id));
    if !signal_group(pgid, Signal::SIGTERM) {
        return false;
    }
    // `grace` 後の SIGKILL。tokio のランタイムに依らない（`tick()` は同期の関数から呼ばれる）。
    // pid の再利用は理論上あり得るが、`killpg` は「その pgid のグループ長」にしか届かず、
    // Linux の pid は上限まで順に配られるので `grace`（既定 10 秒）の間に一周することはない。
    std::thread::Builder::new()
        .name("taskd-killpg".to_string())
        .spawn(move || {
            std::thread::sleep(grace);
            signal_group(pgid, Signal::SIGKILL);
        })
        .map_err(|e| warn!(pgid, error = %e, "could not spawn the SIGKILL timer thread"))
        .ok();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_is_visible_until_it_is_dropped() {
        let run_id = format!("reg-{}", std::process::id());
        {
            let _guard = ProcessGroup::register(&run_id, Some(4242));
            assert_eq!(pgid_of(&run_id), Some(4242));
        }
        assert_eq!(pgid_of(&run_id), None);
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
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let pidfile = dir.path().join("grandchild.pid");
        let script = format!(
            "sleep 300 & echo $! > {}; wait",
            pidfile.to_string_lossy()
        );
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
