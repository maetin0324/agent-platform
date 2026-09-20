//! api.md §8.11（同時アクセス）: ディスパッチャ相当の書き込みループ（別スレッド・別接続）と API の読み取り 1,000 回を
//! 並走させ、`database is locked`（503 `db_busy` / 500）が出ないこと（WAL + busy_timeout）。

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use common::*;
use serde_json::json;
use task_core::{SqliteStore, Status, TaskKind, TaskStore, Trigger};
use time::OffsetDateTime;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn api_reads_do_not_see_database_locked_while_another_connection_writes() {
    let env = TestEnv::new();
    let seed = new_task(TaskKind::Execute, Status::Draft);
    env.seed(&seed);
    let app = env.router();

    let stop = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(AtomicU64::new(0));
    let writer = {
        let stop = Arc::clone(&stop);
        let writes = Arc::clone(&writes);
        let db_path = env.db_path.clone();
        let seed_id = seed.id;
        std::thread::spawn(move || -> Result<(), String> {
            let store = SqliteStore::open(&db_path).map_err(|e| e.to_string())?;
            let mut i = 0u64;
            while !stop.load(Ordering::SeqCst) {
                let spec: task_ops::add::NewTaskSpec = serde_json::from_value(json!({
                    "title": format!("writer {i}"), "objective": "o", "acceptance": [{"type": "human", "text": "ok"}]
                }))
                .map_err(|e| e.to_string())?;
                let task = task_ops::add::create_task(&store, spec, OffsetDateTime::now_utc())
                    .map_err(|e| e.to_string())?;
                store
                    .append_event(seed_id, &progress(&format!("tick {i}")))
                    .map_err(|e| e.to_string())?;
                store
                    .apply_transition(task.id, Trigger::Accept, None)
                    .map_err(|e| e.to_string())?;
                writes.fetch_add(1, Ordering::SeqCst);
                i += 1;
            }
            Ok(())
        })
    };

    let paths = [
        "/api/v1/events?limit=50".to_string(),
        format!("/api/v1/tasks/{}/events?limit=20", seed.id),
        "/api/v1/health".to_string(),
        format!("/api/v1/tasks/{}/artifacts", seed.id),
        "/api/v1/providers".to_string(),
    ];
    let mut reads = 0;
    for i in 0..1_000 {
        let path = &paths[i % paths.len()];
        let resp = send(&app, get(path)).await;
        assert_eq!(resp.status, 200, "read {i} {path}: {}", resp.text());
        reads += 1;
    }
    stop.store(true, Ordering::SeqCst);
    writer
        .join()
        .expect("writer thread")
        .expect("writer never saw database is locked");

    assert_eq!(reads, 1_000);
    assert!(
        writes.load(Ordering::SeqCst) > 0,
        "the writer made progress concurrently"
    );
    let health = send(&app, get("/api/v1/health")).await.json();
    assert_eq!(health["db"]["journal_mode"], "wal");
    let total = env.store.list(None).expect("list").len() as u64;
    assert_eq!(total, writes.load(Ordering::SeqCst) + 1);
}
