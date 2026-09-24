//! ADR-0064 D3 / D5（Phase 110a）: DB の背景メンテナンス。
//!
//! - **背景チェックポイント（D5）**: デーモンの各接続は `background_checkpoint = true`（`wal_autocheckpoint = 0`）
//!   で開くので、`PRAGMA wal_checkpoint` は自動では走らない。ここが `checkpoint_interval_secs` ごとに
//!   専用の接続で `PASSIVE` チェックポイントを打つ（fsync がリクエストや tick の中に落ちないように）。
//! - **定期バックアップ（D3）**: `[db] backup_dir` があれば `backup_interval_secs` ごとに
//!   `celeris-<unix_ts>.sqlite3` を rusqlite の backup API で書き、`backup_keep` 世代だけ残す。
//!
//! どちらも `tokio::spawn` の背景タスクで、専用の同期接続を `spawn_blocking` の中で開く（tick も
//! API のリクエストも待たせない）。失敗は WARN で継続する（デーモンは止めない）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use time::OffsetDateTime;

/// `background_checkpoint` の接続に合わせる WAL の目安サイズ（64 MB。ADR-0064 D5）。
/// `task_core::store` の同名の定数と値を揃えている（`configure_pragmas` の `journal_size_limit`）。
const JOURNAL_SIZE_LIMIT_BYTES: u64 = 64 * 1024 * 1024;

/// 動いている背景タスク。`stop` で graceful に止める（`RunningApi` 等と同じ流儀）。
pub struct RunningDbMaintenance {
    stop: tokio::sync::oneshot::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
    name: &'static str,
}

impl RunningDbMaintenance {
    pub async fn stop(self) {
        let _ = self.stop.send(());
        match tokio::time::timeout(Duration::from_secs(5), self.handle).await {
            Ok(Ok(())) => tracing::info!(task = self.name, "db maintenance task stopped"),
            Ok(Err(e)) => {
                tracing::error!(task = self.name, error = %e, "db maintenance task panicked")
            }
            Err(_) => tracing::warn!(
                task = self.name,
                "db maintenance task did not stop within 5s"
            ),
        }
    }
}

/// ADR-0064 D5: `checkpoint_interval` ごとに `PRAGMA wal_checkpoint(PASSIVE)` を打つ背景タスクを起こす。
pub fn spawn_checkpoint_task(
    db_path: PathBuf,
    checkpoint_interval: Duration,
    busy_timeout: Duration,
) -> RunningDbMaintenance {
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(checkpoint_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // 起動直後の 1 回は間隔を置く（最初の tick は即座に返るため）。
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = ticker.tick() => {
                    let path = db_path.clone();
                    let result = tokio::task::spawn_blocking(move || checkpoint_once(&path, busy_timeout)).await;
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::warn!(error = %e, "background checkpoint failed; will retry next interval"),
                        Err(e) => tracing::warn!(error = %e, "background checkpoint task panicked; will retry next interval"),
                    }
                }
            }
        }
    });
    RunningDbMaintenance {
        stop: stop_tx,
        handle,
        name: "checkpoint",
    }
}

fn checkpoint_once(db_path: &Path, busy_timeout: Duration) -> Result<(), rusqlite::Error> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.busy_timeout(busy_timeout)?;
    run_checkpoint_pragma(&conn, "PASSIVE")?;
    // ADR-0064 D5: WAL が `journal_size_limit`（64 MB）を超えていたら TRUNCATE も試みる（PASSIVE は
    // 読み取り中の接続があると縮められないことがあるため、これも best-effort）。
    if wal_file_len(db_path).is_some_and(|len| len > JOURNAL_SIZE_LIMIT_BYTES) {
        run_checkpoint_pragma(&conn, "TRUNCATE")?;
    }
    Ok(())
}

fn run_checkpoint_pragma(conn: &rusqlite::Connection, mode: &str) -> Result<(), rusqlite::Error> {
    conn.query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |_row| Ok(()))?;
    Ok(())
}

fn wal_file_len(db_path: &Path) -> Option<u64> {
    let mut name = db_path.as_os_str().to_os_string();
    name.push("-wal");
    std::fs::metadata(PathBuf::from(name)).ok().map(|m| m.len())
}

#[derive(Debug, thiserror::Error)]
enum BackupError {
    #[error("backup_dir {0} does not exist (create it first, e.g. with sudo)")]
    MissingDir(PathBuf),
    #[error("store error: {0}")]
    Store(#[from] task_core::StoreError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// ADR-0064 D3: `backup_interval` ごとに `backup_dir` へ `celeris-<unix_ts>.sqlite3` を書き、
/// `keep` 世代だけ残す背景タスクを起こす。
pub fn spawn_backup_task(
    db_path: PathBuf,
    backup_dir: PathBuf,
    backup_interval: Duration,
    keep: usize,
    busy_timeout: Duration,
) -> RunningDbMaintenance {
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(backup_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = ticker.tick() => {
                    let src = db_path.clone();
                    let dir = backup_dir.clone();
                    let result = tokio::task::spawn_blocking(move || backup_once(&src, &dir, keep, busy_timeout)).await;
                    match result {
                        Ok(Ok(dest)) => tracing::info!(backup = %dest.display(), "wrote a periodic db backup"),
                        Ok(Err(e)) => tracing::warn!(error = %e, "periodic db backup failed; will retry next interval"),
                        Err(e) => tracing::warn!(error = %e, "periodic db backup task panicked; will retry next interval"),
                    }
                }
            }
        }
    });
    RunningDbMaintenance {
        stop: stop_tx,
        handle,
        name: "backup",
    }
}

/// ADR-0064 D3: 1 回分のバックアップ。書いたファイルのパスを返す。
fn backup_once(
    db_path: &Path,
    backup_dir: &Path,
    keep: usize,
    busy_timeout: Duration,
) -> Result<PathBuf, BackupError> {
    // D2 の relocate-db.sh と同じ規律: ディレクトリは作らない（人が sudo で作る前提）。
    if !backup_dir.is_dir() {
        return Err(BackupError::MissingDir(backup_dir.to_path_buf()));
    }
    let ts = OffsetDateTime::now_utc().unix_timestamp();
    let dest = backup_dir.join(format!("celeris-{ts}.sqlite3"));
    task_core::backup_database(db_path, &dest, busy_timeout)?;
    prune_backups(backup_dir, keep)?;
    Ok(dest)
}

/// `backup_dir` の `celeris-*.sqlite3` を新しい順に見て、`keep` を超えた古い世代を消す。
fn prune_backups(backup_dir: &Path, keep: usize) -> std::io::Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(backup_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| is_backup_file_name(p))
        .collect();
    // ファイル名が `celeris-<unix_ts>.sqlite3`（固定書式の 10 進数）なので、文字列の昇順が時系列の昇順になる。
    files.sort();
    let excess = files.len().saturating_sub(keep);
    for old in files.into_iter().take(excess) {
        // 消せなくても他の世代の削除は続ける（NFS の一時的な失敗等で全体を止めない）。
        if let Err(e) = std::fs::remove_file(&old) {
            tracing::warn!(path = %old.display(), error = %e, "could not remove an old db backup");
        }
    }
    Ok(())
}

fn is_backup_file_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("celeris-") && n.ends_with(".sqlite3"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::TaskStore;

    #[test]
    fn checkpoint_once_runs_on_a_fresh_wal_db_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("celeris.sqlite3");
        // WAL journal_mode を持つファイルを作る（task-core の `SqliteStore::open` と同じ規約）。
        drop(task_core::SqliteStore::open(&db_path).unwrap());
        checkpoint_once(&db_path, Duration::from_millis(2000)).unwrap();
    }

    #[test]
    fn backup_once_writes_a_restorable_copy_and_prune_keeps_only_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("celeris.sqlite3");
        let store = task_core::SqliteStore::open(&db_path).unwrap();
        let task = sample_task();
        store.insert(&task).unwrap();
        drop(store);

        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();

        // 3 回バックアップし、keep=2 なら最新 2 つだけ残る。
        let mut written = Vec::new();
        for i in 0..3 {
            let dest = backup_once(&db_path, &backup_dir, 2, Duration::from_millis(2000))
                .unwrap_or_else(|e| panic!("backup {i}: {e}"));
            written.push(dest);
            // ファイル名が unix 秒なので、同じ秒に 2 回書くと衝突する（テストのみの配慮）。
            std::thread::sleep(Duration::from_millis(1100));
        }

        let remaining: Vec<PathBuf> = std::fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| is_backup_file_name(p))
            .collect();
        assert_eq!(remaining.len(), 2, "{remaining:?}");
        assert!(
            !remaining.contains(&written[0]),
            "the oldest generation should have been pruned"
        );

        // 残った最新のバックアップは復元して読める。
        let latest = written.last().unwrap();
        let restored = task_core::SqliteStore::open(latest).unwrap();
        assert_eq!(restored.get(task.id).unwrap().map(|t| t.id), Some(task.id));
    }

    #[test]
    fn backup_once_fails_clearly_when_the_backup_dir_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("celeris.sqlite3");
        drop(task_core::SqliteStore::open(&db_path).unwrap());
        let missing = dir.path().join("does-not-exist");
        let err = backup_once(&db_path, &missing, 48, Duration::from_millis(2000)).unwrap_err();
        assert!(matches!(err, BackupError::MissingDir(_)), "{err}");
    }

    /// `spawn_backup_task`/`spawn_checkpoint_task` の tick→spawn_blocking→stop の配線そのものを
    /// 確認する（中身のロジックは `checkpoint_once`/`backup_once` の単体テストで別途確認済み）。
    #[tokio::test]
    async fn spawned_tasks_run_on_their_interval_and_stop_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("celeris.sqlite3");
        drop(task_core::SqliteStore::open(&db_path).unwrap());
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let checkpoint = spawn_checkpoint_task(
            db_path.clone(),
            Duration::from_millis(20),
            Duration::from_millis(2000),
        );
        let backup = spawn_backup_task(
            db_path.clone(),
            backup_dir.clone(),
            Duration::from_millis(20),
            48,
            Duration::from_millis(2000),
        );

        // 両方とも少なくとも 1 回は tick が回るだけ待つ（内容はロジック側のテストで確認済みなので、
        // ここでは「バックアップ tick が実際にファイルを作った」ことだけ見る）。
        tokio::time::sleep(Duration::from_millis(300)).await;

        checkpoint.stop().await;
        backup.stop().await;

        let backups: Vec<PathBuf> = std::fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| is_backup_file_name(p))
            .collect();
        assert!(
            !backups.is_empty(),
            "expected at least one backup file to be written"
        );
    }

    fn sample_task() -> task_core::Task {
        let now = OffsetDateTime::now_utc();
        task_core::Task {
            routing: None,
            repos: Vec::new(),
            id: task_core::TaskId::new(),
            parent_id: None,
            kind: task_core::TaskKind::Execute,
            title: "db maintenance test task".into(),
            objective: "no-op".into(),
            acceptance: vec![task_core::Criterion {
                text: "n/a".into(),
                check: task_core::Check::Human,
            }],
            inputs: vec![],
            depends_on: vec![],
            status: task_core::Status::Draft,
            priority: 0,
            worker_hint: task_core::WorkerHint {
                tier: task_core::Tier::Standard,
                adapter: None,
            },
            workspace: task_core::WorkspaceSpec::Local {
                path: PathBuf::from("."),
                mode: None,
            },
            budget: task_core::Budget {
                max_turns: 1,
                max_wall_secs: 30,
                max_retries: 0,
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
            skills: Vec::new(),
            mode: task_core::TaskMode::default(),
            labels: Vec::new(),
            category: Default::default(),
        }
    }
}
