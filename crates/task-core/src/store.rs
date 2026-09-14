//! SQLite ベースの `TaskStore` 実装。DESIGN.md §5.1 / §4.3 準拠。
//!
//! - `events` テーブルは追記専用（append-only）の正典。このモジュールから
//!   `events` に対して UPDATE/DELETE を発行することはない。
//! - `tasks` テーブルは `Task` 全体を `json` 列に保持しつつ、検索・排他制御に
//!   使う列（`status` / `kind` / `parent_id` / `priority` / `created_at` /
//!   `lease_*`）を非正規化して複製する。
//!
//! 状態機械のロジック（`crate::transition`）には依存しない（意図的な疎結合。
//! Phase 3 のディスパッチャが `transition()` の結果をこのストアに書き戻す）。

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration as StdDuration;

use rusqlite::{Connection, OptionalExtension, params};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::model::{Event, Status, Task, TaskId, TaskKind};
use crate::transition::{InvalidTransition, Outcome, StateView, Trigger, transition};

const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("time formatting error: {0}")]
    TimeFormat(#[from] time::error::Format),
    #[error("time parsing error: {0}")]
    TimeParse(#[from] time::error::Parse),
    #[error("mutex poisoned")]
    Poisoned,
    #[error("invalid stored data: {0}")]
    Invalid(String),
    #[error(transparent)]
    InvalidTransition(#[from] InvalidTransition),
}

pub trait TaskStore: Send + Sync {
    fn insert(&self, task: &Task) -> Result<(), StoreError>;
    fn get(&self, id: TaskId) -> Result<Option<Task>, StoreError>;
    fn list(&self, filter: Option<Status>) -> Result<Vec<Task>, StoreError>;
    /// イベントを追記し、割り当てられた `seq`（0始まり、task_id 内で単調増加）を返す。
    fn append_event(&self, task_id: TaskId, event: &Event) -> Result<u64, StoreError>;
    /// `task_id` に紐づく全イベントを `seq` 昇順で返す。
    fn events_for(&self, task_id: TaskId) -> Result<Vec<(u64, Event)>, StoreError>;
    /// 排他的にリースを取得する。成功したら true を返し、task の status を Running にし、
    /// lease = Some{worker_run_id, expires_at: now + ttl} をDBに書く。
    /// 既にリースされている／status != Ready の場合は false を返す（エラーではない）。
    fn acquire_lease(
        &self,
        task_id: TaskId,
        worker_run_id: &str,
        ttl: StdDuration,
    ) -> Result<bool, StoreError>;
    /// worker_run_id が現在のリースと一致する場合のみ lease を None にする。status は変更しない。
    fn release_lease(&self, task_id: TaskId, worker_run_id: &str) -> Result<(), StoreError>;
    /// status=Ready かつ depends_on が全て Done かつ（親が存在し kind=Approval の場合は親が Done）
    /// を満たすタスクを priority DESC, created_at ASC で最大 limit 件返す。
    fn ready_tasks(&self, limit: usize) -> Result<Vec<Task>, StoreError>;
    /// `transition::transition()` で検証した任意のトリガーを適用する汎用の書き込み口
    /// （ADR-0004 D1）。タスクの取得・`transition()` の呼び出し・`tasks` 行の更新・
    /// `Event::Transitioned` の追記（と任意の `extra_event`）を単一トランザクションで行う。
    /// タスクが存在しない場合は `StoreError::Invalid`、遷移が無効な場合は
    /// `StoreError::InvalidTransition` を返し、いずれもタスクの状態は変更しない。
    fn apply_transition(
        &self,
        task_id: TaskId,
        trigger: Trigger,
        extra_event: Option<Event>,
    ) -> Result<Outcome, StoreError> {
        self.apply_transition_with_events(task_id, trigger, extra_event.into_iter().collect())
    }
    /// `apply_transition` の一般形（ADR-0005 D4）。`extra_events` を順に、`Event::Transitioned`
    /// の直後に同一トランザクションで追記する。ディスパッチャが `WorkerFinished` や
    /// 条件ごとの `ReviewVerdict` を遷移と原子的に記録するために使う。
    fn apply_transition_with_events(
        &self,
        task_id: TaskId,
        trigger: Trigger,
        extra_events: Vec<Event>,
    ) -> Result<Outcome, StoreError>;

    /// ADR-0007 D3: Plan の子タスク群を挿入（`Created`、`accept_children` なら続けて `Accept`）し、
    /// 親に `ReviewPass` を適用して `verdict_events` を追記する。全体が 1 トランザクション。
    fn complete_plan(
        &self,
        plan_id: TaskId,
        verdict_events: Vec<Event>,
        children: Vec<Task>,
        accept_children: bool,
    ) -> Result<Outcome, StoreError>;
}

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Draft => "draft",
        Status::Ready => "ready",
        Status::Running => "running",
        Status::Blocked => "blocked",
        Status::Reviewing => "reviewing",
        Status::Done => "done",
        Status::Failed => "failed",
        Status::Cancelled => "cancelled",
    }
}

fn kind_str(k: TaskKind) -> &'static str {
    match k {
        TaskKind::Plan => "plan",
        TaskKind::Execute => "execute",
        TaskKind::Review => "review",
        TaskKind::Approval => "approval",
    }
}

fn format_rfc3339(t: OffsetDateTime) -> Result<String, StoreError> {
    Ok(t.format(&Rfc3339)?)
}

impl SqliteStore {
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn)
    }

    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(MIGRATION_0001)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }

    fn row_to_task(json: String) -> Result<Task, StoreError> {
        Ok(serde_json::from_str(&json)?)
    }

    /// 既にロック済みの connection を使ってタスクを取得する内部ヘルパー。
    /// `get()` が再度 Mutex をロックしないようにするために分離してある。
    fn get_locked(conn: &Connection, id: TaskId) -> Result<Option<Task>, StoreError> {
        let json: Option<String> = conn
            .query_row(
                "SELECT json FROM tasks WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        match json {
            Some(j) => Ok(Some(Self::row_to_task(j)?)),
            None => Ok(None),
        }
    }

    /// `insert` の本体（トランザクション内でも使えるよう `Connection` を受ける）。
    fn insert_tx(conn: &Connection, task: &Task) -> Result<(), StoreError> {
        let json = serde_json::to_string(task)?;
        let created_at = format_rfc3339(task.created_at)?;
        let (lease_worker_run_id, lease_expires_at) = match &task.lease {
            Some(lease) => (
                Some(lease.worker_run_id.clone()),
                Some(format_rfc3339(lease.expires_at)?),
            ),
            None => (None, None),
        };
        conn.execute(
            "INSERT INTO tasks (id, status, kind, parent_id, priority, created_at, \
             lease_worker_run_id, lease_expires_at, json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                task.id.to_string(),
                status_str(task.status),
                kind_str(task.kind),
                task.parent_id.map(|p| p.to_string()),
                task.priority,
                created_at,
                lease_worker_run_id,
                lease_expires_at,
                json,
            ],
        )?;
        Ok(())
    }

    /// `apply_transition_with_events` の本体（ADR-0004 D1 / ADR-0005 D4）。`tx` 内で任意のトリガーを
    /// 検証し、tasks の更新と Event::Transitioned (+ extra_events) の追記を行う。commit は呼び出し側。
    fn apply_transition_tx(
        tx: &Connection,
        task_id: TaskId,
        trigger: Trigger,
        extra_events: Vec<Event>,
    ) -> Result<Outcome, StoreError> {
        // ADR-0004 D1 / ADR-0005 D4: 任意のトリガーを検証し、tasks の更新と
        // Event::Transitioned (+ extra_events) の追記を単一トランザクションで行う。

        let json: Option<String> = tx
            .query_row(
                "SELECT json FROM tasks WHERE id = ?1",
                params![task_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let json = match json {
            Some(j) => j,
            None => return Err(StoreError::Invalid(format!("task not found: {task_id}"))),
        };
        let mut task = Self::row_to_task(json)?;

        let view = StateView {
            kind: task.kind,
            status: task.status,
            attempts: task.attempts,
            max_retries: task.budget.max_retries,
        };
        let outcome = transition(&view, &trigger)?;

        let now = OffsetDateTime::now_utc();
        // ADR-0002 D1: running から出る全遷移でリースを解放する。
        let leaving_running = view.status == Status::Running && outcome.next != Status::Running;
        task.status = outcome.next;
        task.attempts = outcome.attempts;
        task.updated_at = now;
        if leaving_running {
            task.lease = None;
        }

        let new_json = serde_json::to_string(&task)?;
        if leaving_running {
            tx.execute(
                "UPDATE tasks SET status = ?1, lease_worker_run_id = NULL, \
                 lease_expires_at = NULL, json = ?2 WHERE id = ?3",
                params![status_str(task.status), new_json, task_id.to_string()],
            )?;
        } else {
            tx.execute(
                "UPDATE tasks SET status = ?1, json = ?2 WHERE id = ?3",
                params![status_str(task.status), new_json, task_id.to_string()],
            )?;
        }

        let mut next_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM events WHERE task_id = ?1",
            params![task_id.to_string()],
            |row| row.get(0),
        )?;

        let transitioned = Event::Transitioned {
            from: view.status,
            to: outcome.next,
            reason: outcome.reason.to_string(),
        };
        let ts = format_rfc3339(OffsetDateTime::now_utc())?;
        tx.execute(
            "INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
            params![
                task_id.to_string(),
                next_seq,
                ts,
                serde_json::to_string(&transitioned)?
            ],
        )?;
        next_seq += 1;

        for event in extra_events {
            let ts = format_rfc3339(OffsetDateTime::now_utc())?;
            tx.execute(
                "INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
                params![
                    task_id.to_string(),
                    next_seq,
                    ts,
                    serde_json::to_string(&event)?
                ],
            )?;
            next_seq += 1;
        }

        Ok(outcome)
    }

    fn append_event_tx(conn: &Connection, task_id: TaskId, event: &Event) -> Result<u64, StoreError> {
        let next_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM events WHERE task_id = ?1",
            params![task_id.to_string()],
            |row| row.get(0),
        )?;
        let ts = format_rfc3339(OffsetDateTime::now_utc())?;
        let json = serde_json::to_string(event)?;
        conn.execute(
            "INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
            params![task_id.to_string(), next_seq, ts, json],
        )?;
        Ok(next_seq as u64)
    }
}

impl TaskStore for SqliteStore {
    fn insert(&self, task: &Task) -> Result<(), StoreError> {
        let conn = self.lock()?;
        Self::insert_tx(&conn, task)
    }

    fn get(&self, id: TaskId) -> Result<Option<Task>, StoreError> {
        let conn = self.lock()?;
        Self::get_locked(&conn, id)
    }

    fn list(&self, filter: Option<Status>) -> Result<Vec<Task>, StoreError> {
        let conn = self.lock()?;
        let mut tasks = Vec::new();
        match filter {
            Some(status) => {
                let mut stmt = conn.prepare("SELECT json FROM tasks WHERE status = ?1")?;
                let rows = stmt.query_map(params![status_str(status)], |row| {
                    row.get::<_, String>(0)
                })?;
                for row in rows {
                    tasks.push(Self::row_to_task(row?)?);
                }
            }
            None => {
                let mut stmt = conn.prepare("SELECT json FROM tasks")?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                for row in rows {
                    tasks.push(Self::row_to_task(row?)?);
                }
            }
        }
        Ok(tasks)
    }

    fn append_event(&self, task_id: TaskId, event: &Event) -> Result<u64, StoreError> {
        let conn = self.lock()?;
        let next_seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM events WHERE task_id = ?1",
            params![task_id.to_string()],
            |row| row.get(0),
        )?;
        let ts = format_rfc3339(OffsetDateTime::now_utc())?;
        let json = serde_json::to_string(event)?;
        conn.execute(
            "INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
            params![task_id.to_string(), next_seq, ts, json],
        )?;
        Ok(next_seq as u64)
    }

    fn events_for(&self, task_id: TaskId) -> Result<Vec<(u64, Event)>, StoreError> {
        let conn = self.lock()?;
        let mut stmt =
            conn.prepare("SELECT seq, json FROM events WHERE task_id = ?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map(params![task_id.to_string()], |row| {
            let seq: i64 = row.get(0)?;
            let json: String = row.get(1)?;
            Ok((seq, json))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (seq, json) = row?;
            let event: Event = serde_json::from_str(&json)?;
            events.push((seq as u64, event));
        }
        Ok(events)
    }

    fn acquire_lease(
        &self,
        task_id: TaskId,
        worker_run_id: &str,
        ttl: StdDuration,
    ) -> Result<bool, StoreError> {
        // ADR-0002 D2: 遷移（ready -> running, Trigger::Dispatch）の結果は
        // `Event::Transitioned` と同一トランザクションで追記する。D8: `Dispatch`
        // は `kind == Approval` では無効（Approval は running に入らない）。
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        let current: Option<(String, String, String)> = tx
            .query_row(
                "SELECT status, kind, json FROM tasks WHERE id = ?1",
                params![task_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;

        let (status_col, kind_col, json) = match current {
            Some(v) => v,
            None => return Ok(false),
        };
        if status_col != status_str(Status::Ready) || kind_col == kind_str(TaskKind::Approval) {
            return Ok(false);
        }

        let mut task = Self::row_to_task(json)?;

        let dur = time::Duration::new(ttl.as_secs() as i64, ttl.subsec_nanos() as i32);
        let now = OffsetDateTime::now_utc();
        let expires_at = now + dur;

        task.status = Status::Running;
        task.lease = Some(crate::model::Lease {
            worker_run_id: worker_run_id.to_string(),
            expires_at,
        });
        task.updated_at = now;

        let new_json = serde_json::to_string(&task)?;
        let expires_at_str = format_rfc3339(expires_at)?;

        let affected = tx.execute(
            "UPDATE tasks SET status = ?1, lease_worker_run_id = ?2, lease_expires_at = ?3, \
             json = ?4 WHERE id = ?5 AND status = ?6",
            params![
                status_str(Status::Running),
                worker_run_id,
                expires_at_str,
                new_json,
                task_id.to_string(),
                status_str(Status::Ready),
            ],
        )?;

        if affected != 1 {
            return Ok(false);
        }

        let event = Event::Transitioned {
            from: Status::Ready,
            to: Status::Running,
            reason: "dispatch".to_string(),
        };
        let next_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM events WHERE task_id = ?1",
            params![task_id.to_string()],
            |row| row.get(0),
        )?;
        let ts = format_rfc3339(OffsetDateTime::now_utc())?;
        let event_json = serde_json::to_string(&event)?;
        tx.execute(
            "INSERT INTO events (task_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
            params![task_id.to_string(), next_seq, ts, event_json],
        )?;

        tx.commit()?;
        Ok(true)
    }

    fn release_lease(&self, task_id: TaskId, worker_run_id: &str) -> Result<(), StoreError> {
        let conn = self.lock()?;

        let current: Option<(Option<String>, String)> = conn
            .query_row(
                "SELECT lease_worker_run_id, json FROM tasks WHERE id = ?1",
                params![task_id.to_string()],
                |row| Ok((row.get(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;

        let (lease_worker_run_id, json) = match current {
            Some(v) => v,
            None => return Ok(()),
        };

        if lease_worker_run_id.as_deref() != Some(worker_run_id) {
            return Ok(());
        }

        let mut task = Self::row_to_task(json)?;
        task.lease = None;
        task.updated_at = OffsetDateTime::now_utc();
        let new_json = serde_json::to_string(&task)?;

        conn.execute(
            "UPDATE tasks SET lease_worker_run_id = NULL, lease_expires_at = NULL, json = ?1 \
             WHERE id = ?2 AND lease_worker_run_id = ?3",
            params![new_json, task_id.to_string(), worker_run_id],
        )?;

        Ok(())
    }

    fn ready_tasks(&self, limit: usize) -> Result<Vec<Task>, StoreError> {
        let conn = self.lock()?;

        let mut stmt = conn.prepare(
            "SELECT json FROM tasks WHERE status = ?1 ORDER BY priority DESC, created_at ASC",
        )?;
        let rows = stmt.query_map(params![status_str(Status::Ready)], |row| {
            row.get::<_, String>(0)
        })?;

        let mut result = Vec::new();
        for row in rows {
            let task = Self::row_to_task(row?)?;

            let mut deps_done = true;
            for dep_id in &task.depends_on {
                match Self::get_locked(&conn, *dep_id)? {
                    Some(dep) if dep.status == Status::Done => {}
                    _ => {
                        deps_done = false;
                        break;
                    }
                }
            }
            if !deps_done {
                continue;
            }

            if let Some(parent_id) = task.parent_id
                && let Some(parent) = Self::get_locked(&conn, parent_id)?
                && parent.kind == TaskKind::Approval
                && parent.status != Status::Done
            {
                continue;
            }

            result.push(task);
            if result.len() >= limit {
                break;
            }
        }

        Ok(result)
    }

    fn apply_transition_with_events(
        &self,
        task_id: TaskId,
        trigger: Trigger,
        extra_events: Vec<Event>,
    ) -> Result<Outcome, StoreError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let outcome = Self::apply_transition_tx(&tx, task_id, trigger, extra_events)?;
        tx.commit()?;
        Ok(outcome)
    }

    fn complete_plan(
        &self,
        plan_id: TaskId,
        verdict_events: Vec<Event>,
        children: Vec<Task>,
        accept_children: bool,
    ) -> Result<Outcome, StoreError> {
        // ADR-0007 D3: 子の insert + Created (+ Accept) と親の ReviewPass を単一トランザクションで行う。
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        for child in &children {
            if child.parent_id != Some(plan_id) {
                return Err(StoreError::Invalid(format!(
                    "child {} does not belong to plan {plan_id}",
                    child.id
                )));
            }
            Self::insert_tx(&tx, child)?;
            Self::append_event_tx(
                &tx,
                child.id,
                &Event::Created {
                    task: Box::new(child.clone()),
                },
            )?;
            if accept_children {
                Self::apply_transition_tx(&tx, child.id, Trigger::Accept, vec![])?;
            }
        }
        let outcome = Self::apply_transition_tx(&tx, plan_id, Trigger::ReviewPass, verdict_events)?;
        tx.commit()?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArtifactRef, Budget, Check, Criterion, Tier, WorkerHint, WorkspaceSpec};
    use std::sync::Arc;
    use std::sync::Barrier;

    fn sample_task(status: Status) -> Task {
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "do something".to_string(),
            objective: "make it work".to_string(),
            acceptance: vec![Criterion {
                text: "tests pass".to_string(),
                check: Check::Command {
                    cmd: "true".to_string(),
                    expect_exit: 0,
                },
            }],
            inputs: vec![ArtifactRef {
                name: "spec".to_string(),
                path: "spec.md".to_string(),
                sha256: "abc".to_string(),
                kind: "doc".to_string(),
            }],
            depends_on: vec![],
            status,
            priority: 0,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::Local {
                path: "/tmp/workspace".into(),
            },
            budget: Budget {
                max_turns: 10,
                max_wall_secs: 600,
                max_retries: 2,
            },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn insert_then_get_roundtrips() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched, task);
    }

    #[test]
    fn list_filters_by_status() {
        let store = SqliteStore::open_in_memory().expect("open");
        let ready = sample_task(Status::Ready);
        let draft = sample_task(Status::Draft);
        store.insert(&ready).expect("insert ready");
        store.insert(&draft).expect("insert draft");

        let ready_list = store.list(Some(Status::Ready)).expect("list ready");
        assert_eq!(ready_list.len(), 1);
        assert_eq!(ready_list[0].id, ready.id);

        let all = store.list(None).expect("list all");
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn append_event_reads_back_in_seq_order() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");

        let e0 = Event::ApprovalRequested;
        let e1 = Event::Transitioned {
            from: Status::Draft,
            to: Status::Ready,
            reason: "ready to go".to_string(),
        };

        let seq0 = store.append_event(task.id, &e0).expect("append e0");
        let seq1 = store.append_event(task.id, &e1).expect("append e1");
        assert_eq!(seq0, 0);
        assert_eq!(seq1, 1);

        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], (0, e0));
        assert_eq!(events[1], (1, e1));
    }

    #[test]
    fn acquire_lease_is_exclusive_under_concurrency() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("open"));
        let task = sample_task(Status::Ready);
        store.insert(&task).expect("insert");

        let barrier = Arc::new(Barrier::new(2));

        let store_a = Arc::clone(&store);
        let barrier_a = Arc::clone(&barrier);
        let task_id = task.id;
        let handle_a = std::thread::spawn(move || {
            barrier_a.wait();
            store_a.acquire_lease(task_id, "worker-a", StdDuration::from_secs(60))
        });

        let store_b = Arc::clone(&store);
        let barrier_b = Arc::clone(&barrier);
        let handle_b = std::thread::spawn(move || {
            barrier_b.wait();
            store_b.acquire_lease(task_id, "worker-b", StdDuration::from_secs(60))
        });

        let result_a = handle_a.join().expect("join a").expect("acquire a");
        let result_b = handle_b.join().expect("join b").expect("acquire b");

        assert_ne!(result_a, result_b, "exactly one acquisition must succeed");
        assert!(result_a || result_b);
    }

    #[test]
    fn release_lease_then_reacquire_succeeds() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Ready);
        store.insert(&task).expect("insert");

        let acquired = store
            .acquire_lease(task.id, "worker-a", StdDuration::from_secs(60))
            .expect("acquire");
        assert!(acquired);

        // status is now Running, so a second acquire must fail.
        let second = store
            .acquire_lease(task.id, "worker-b", StdDuration::from_secs(60))
            .expect("acquire second");
        assert!(!second);

        store
            .release_lease(task.id, "worker-a")
            .expect("release");

        let after_release = store.get(task.id).expect("get").expect("some");
        assert!(after_release.lease.is_none());

        // release_lease intentionally does not change status (still Running,
        // per its contract): that is the dispatcher's job via transition().
        // To verify "release then reacquire succeeds" on the very same task,
        // we simulate that dispatcher step here with a direct status update
        // (test-only; this module owns `conn` so it may reach in directly).
        {
            let conn = store.conn.lock().expect("lock");
            let mut readied = after_release.clone();
            readied.status = Status::Ready;
            let json = serde_json::to_string(&readied).expect("serialize");
            conn.execute(
                "UPDATE tasks SET status = 'ready', json = ?1 WHERE id = ?2",
                params![json, task.id.to_string()],
            )
            .expect("reset to ready");
        }

        let reacquired = store
            .acquire_lease(task.id, "worker-c", StdDuration::from_secs(60))
            .expect("acquire third");
        assert!(reacquired);
    }

    #[test]
    fn ready_tasks_excludes_incomplete_dependencies_and_pending_approval_parent() {
        let store = SqliteStore::open_in_memory().expect("open");

        // Case 1: dependency not done -> excluded.
        let dep = sample_task(Status::Running);
        store.insert(&dep).expect("insert dep");
        let mut blocked_by_dep = sample_task(Status::Ready);
        blocked_by_dep.depends_on = vec![dep.id];
        store.insert(&blocked_by_dep).expect("insert blocked");

        // Case 2: dependency done -> included.
        let done_dep = sample_task(Status::Done);
        store.insert(&done_dep).expect("insert done dep");
        let mut unblocked = sample_task(Status::Ready);
        unblocked.depends_on = vec![done_dep.id];
        store.insert(&unblocked).expect("insert unblocked");

        // Case 3: parent is Approval and not Done -> excluded.
        let mut approval_parent = sample_task(Status::Reviewing);
        approval_parent.kind = TaskKind::Approval;
        store.insert(&approval_parent).expect("insert approval");
        let mut child_of_pending_approval = sample_task(Status::Ready);
        child_of_pending_approval.parent_id = Some(approval_parent.id);
        store
            .insert(&child_of_pending_approval)
            .expect("insert child pending");

        let ready = store.ready_tasks(10).expect("ready_tasks");
        let ready_ids: Vec<TaskId> = ready.iter().map(|t| t.id).collect();

        assert!(!ready_ids.contains(&blocked_by_dep.id));
        assert!(ready_ids.contains(&unblocked.id));
        assert!(!ready_ids.contains(&child_of_pending_approval.id));
    }

    /// ADR-0002 D8: `Dispatch`（ready -> running）は `kind == Approval` では
    /// 無効。`Approval` タスクは `acquire_lease` によって running に入っては
    /// ならない（DESIGN §4.2）。
    #[test]
    fn acquire_lease_rejects_approval_kind_even_when_ready() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut approval = sample_task(Status::Ready);
        approval.kind = TaskKind::Approval;
        store.insert(&approval).expect("insert approval");

        let acquired = store
            .acquire_lease(approval.id, "worker-a", StdDuration::from_secs(60))
            .expect("acquire_lease call");
        assert!(!acquired);

        let fetched = store.get(approval.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Ready);
        assert!(fetched.lease.is_none());
    }

    /// ADR-0002 D2: 成功した遷移は `Event::Transitioned` を同一トランザクションで
    /// 追記する。`acquire_lease` が実質的に駆動する ready->running 遷移でも
    /// この不変条件が成り立つことを確認する。
    #[test]
    fn acquire_lease_appends_transitioned_event_atomically() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Ready);
        store.insert(&task).expect("insert");

        let acquired = store
            .acquire_lease(task.id, "worker-a", StdDuration::from_secs(60))
            .expect("acquire");
        assert!(acquired);

        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(events.len(), 1);
        match &events[0].1 {
            Event::Transitioned { from, to, reason } => {
                assert_eq!(*from, Status::Ready);
                assert_eq!(*to, Status::Running);
                assert_eq!(reason, "dispatch");
            }
            other => panic!("expected Transitioned event, got {other:?}"),
        }

        // 失敗した acquire_lease（既にリース済み）はイベントを追加しない。
        let second = store
            .acquire_lease(task.id, "worker-b", StdDuration::from_secs(60))
            .expect("second acquire call");
        assert!(!second);
        let events_after = store.events_for(task.id).expect("events_for after");
        assert_eq!(events_after.len(), 1);
    }

    /// ADR-0004 D1: `apply_transition` は draft -> ready (Accept) を適用し、
    /// tasks の status と Event::Transitioned を同一トランザクションで反映する。
    #[test]
    fn apply_transition_accept_moves_draft_to_ready_and_appends_event() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Draft);
        store.insert(&task).expect("insert");

        let outcome = store
            .apply_transition(task.id, Trigger::Accept, None)
            .expect("apply_transition");
        assert_eq!(outcome.next, Status::Ready);

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Ready);

        let events = store.events_for(task.id).expect("events_for");
        assert_eq!(events.len(), 1);
        match &events[0].1 {
            Event::Transitioned { from, to, reason } => {
                assert_eq!(*from, Status::Draft);
                assert_eq!(*to, Status::Ready);
                assert_eq!(reason, "accept");
            }
            other => panic!("expected Transitioned event, got {other:?}"),
        }
    }

    /// ADR-0004 D1: 無効な遷移は `StoreError::InvalidTransition` を返し、
    /// タスクの状態もイベントログも変更しない。
    #[test]
    fn apply_transition_rejects_invalid_trigger_without_side_effects() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Done);
        store.insert(&task).expect("insert");

        let result = store.apply_transition(task.id, Trigger::Accept, None);
        assert!(matches!(result, Err(StoreError::InvalidTransition(_))));

        let fetched = store.get(task.id).expect("get").expect("some");
        assert_eq!(fetched.status, Status::Done);
        assert!(store.events_for(task.id).expect("events_for").is_empty());
    }

    /// ADR-0004 D2: `Approve`/`Reject` は `extra_event` として
    /// `Event::ApprovalDecided` を同一トランザクションで追記できる。
    #[test]
    fn apply_transition_appends_extra_event_atomically() {
        let store = SqliteStore::open_in_memory().expect("open");
        let mut approval = sample_task(Status::Ready);
        approval.kind = TaskKind::Approval;
        store.insert(&approval).expect("insert");

        let extra = Event::ApprovalDecided {
            by: "human".to_string(),
            approved: true,
            note: None,
        };
        let outcome = store
            .apply_transition(approval.id, Trigger::Approve, Some(extra.clone()))
            .expect("apply_transition");
        assert_eq!(outcome.next, Status::Done);

        let events = store.events_for(approval.id).expect("events_for");
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0].1, Event::Transitioned { .. }));
        assert_eq!(events[1].1, extra);
    }

    /// apply_transition が存在しない task_id に対して呼ばれた場合の扱い。
    #[test]
    fn apply_transition_missing_task_returns_invalid_error() {
        let store = SqliteStore::open_in_memory().expect("open");
        let result = store.apply_transition(TaskId::new(), Trigger::Accept, None);
        assert!(matches!(result, Err(StoreError::Invalid(_))));
    }

    /// ADR-0002 D1: running から出る全遷移でリースを解放する。`apply_transition` が
    /// 駆動する遷移（ここでは WorkerDone）でも `lease` が None になり、
    /// `lease_worker_run_id`/`lease_expires_at` 列も NULL になることを確認する。
    #[test]
    fn apply_transition_releases_lease_when_leaving_running() {
        let store = SqliteStore::open_in_memory().expect("open");
        let task = sample_task(Status::Ready);
        store.insert(&task).expect("insert");
        store
            .acquire_lease(task.id, "worker-a", StdDuration::from_secs(60))
            .expect("acquire_lease")
            .then_some(())
            .expect("lease should be acquired");

        let running = store.get(task.id).expect("get").expect("some");
        assert!(running.lease.is_some());

        store
            .apply_transition(task.id, Trigger::WorkerDone, None)
            .expect("apply_transition");

        let after = store.get(task.id).expect("get").expect("some");
        assert_eq!(after.status, Status::Reviewing);
        assert!(after.lease.is_none());

        let (lease_worker_run_id, lease_expires_at): (Option<String>, Option<String>) = {
            let conn = store.conn.lock().expect("lock");
            conn.query_row(
                "SELECT lease_worker_run_id, lease_expires_at FROM tasks WHERE id = ?1",
                params![task.id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query lease columns")
        };
        assert!(lease_worker_run_id.is_none());
        assert!(lease_expires_at.is_none());
    }

    #[test]
    fn apply_transition_with_events_appends_transitioned_then_extras_in_order() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task(Status::Ready);
        store.insert(&task).unwrap();
        assert!(store.acquire_lease(task.id, "run-1", StdDuration::from_secs(60)).unwrap());
        let extras = vec![
            Event::WorkerFinished { run_id: "run-1".into(), outcome: "done: x".into(), usage: None },
            Event::WorkerProgress { run_id: "run-1".into(), msg: "extra".into() },
        ];
        let outcome = store.apply_transition_with_events(task.id, Trigger::WorkerDone, extras).unwrap();
        assert_eq!(outcome.next, Status::Reviewing);
        let events = store.events_for(task.id).unwrap();
        let tail: Vec<(u64, String)> = events[events.len() - 3..]
            .iter()
            .map(|(seq, e)| (*seq, match e {
                Event::Transitioned { to, reason, .. } => format!("transitioned:{to:?}:{reason}"),
                Event::WorkerFinished { outcome, .. } => format!("finished:{outcome}"),
                Event::WorkerProgress { msg, .. } => format!("progress:{msg}"),
                _ => "other".into(),
            }))
            .collect();
        assert_eq!(tail, vec![
            (1, "transitioned:Reviewing:worker_done".to_string()),
            (2, "finished:done: x".to_string()),
            (3, "progress:extra".to_string()),
        ]);
        assert!(store.get(task.id).unwrap().unwrap().lease.is_none());
        // 無効な遷移では何も追記されない。
        let before = store.events_for(task.id).unwrap().len();
        assert!(store.apply_transition_with_events(task.id, Trigger::WorkerDone, vec![Event::ApprovalRequested]).is_err());
        assert_eq!(store.events_for(task.id).unwrap().len(), before);
    }

    /// ADR-0007 D3: 子の挿入と親の ReviewPass が同一トランザクションで、accept_children に応じて
    /// 子が draft / ready になる。親の遷移が無効なら子も挿入されない。
    #[test]
    fn complete_plan_inserts_children_and_passes_parent_atomically() {
        use crate::model::TaskKind;
        let store = SqliteStore::open_in_memory().expect("open");
        let mut plan = sample_task(Status::Reviewing);
        plan.kind = TaskKind::Plan;
        store.insert(&plan).expect("insert plan");
        let mut c1 = sample_task(Status::Draft);
        c1.parent_id = Some(plan.id);
        let mut c2 = sample_task(Status::Draft);
        c2.parent_id = Some(plan.id);
        c2.depends_on = vec![c1.id];
        let verdict = Event::ReviewVerdict {
            run_id: "r".into(),
            criterion_idx: 0,
            pass: true,
            reason: "plan ok".into(),
        };
        let outcome = store
            .complete_plan(plan.id, vec![verdict], vec![c1.clone(), c2.clone()], false)
            .expect("complete_plan");
        assert_eq!(outcome.next, Status::Done);
        assert_eq!(store.get(plan.id).unwrap().unwrap().status, Status::Done);
        let ev: Vec<Event> = store.events_for(plan.id).unwrap().into_iter().map(|(_, e)| e).collect();
        assert!(matches!(&ev[0], Event::Transitioned { to: Status::Done, reason, .. } if reason == "review_pass"));
        assert!(matches!(&ev[1], Event::ReviewVerdict { pass: true, .. }));
        for c in [&c1, &c2] {
            let got = store.get(c.id).unwrap().expect("child inserted");
            assert_eq!(got.status, Status::Draft);
            let ev = store.events_for(c.id).unwrap();
            assert_eq!(ev.len(), 1);
            assert!(matches!(&ev[0].1, Event::Created { .. }));
        }
        // draft の子は ready_tasks に出ない。
        assert!(store.ready_tasks(10).unwrap().is_empty());

        // accept_children = true: 子は ready、Created + Transitioned(accept)。
        let mut plan2 = sample_task(Status::Reviewing);
        plan2.kind = TaskKind::Plan;
        store.insert(&plan2).expect("insert plan2");
        let mut c3 = sample_task(Status::Draft);
        c3.parent_id = Some(plan2.id);
        store.complete_plan(plan2.id, vec![], vec![c3.clone()], true).expect("complete_plan 2");
        let got = store.get(c3.id).unwrap().unwrap();
        assert_eq!(got.status, Status::Ready);
        let ev = store.events_for(c3.id).unwrap();
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[1].1, Event::Transitioned { from: Status::Draft, to: Status::Ready, reason } if reason == "accept"));
        assert_eq!(store.ready_tasks(10).unwrap().len(), 1);

        // 親が reviewing でなければ全体がロールバックされ、子は挿入されない。
        let mut not_reviewing = sample_task(Status::Ready);
        not_reviewing.kind = TaskKind::Plan;
        store.insert(&not_reviewing).unwrap();
        let mut c4 = sample_task(Status::Draft);
        c4.parent_id = Some(not_reviewing.id);
        let err = store.complete_plan(not_reviewing.id, vec![], vec![c4.clone()], true).unwrap_err();
        assert!(matches!(err, StoreError::InvalidTransition(_)));
        assert!(store.get(c4.id).unwrap().is_none());
        assert!(store.events_for(c4.id).unwrap().is_empty());

        // 親子関係が違う子は拒否。
        let stranger = sample_task(Status::Draft);
        let mut plan3 = sample_task(Status::Reviewing);
        plan3.kind = TaskKind::Plan;
        store.insert(&plan3).unwrap();
        assert!(matches!(
            store.complete_plan(plan3.id, vec![], vec![stranger], false),
            Err(StoreError::Invalid(_))
        ));
    }
}
