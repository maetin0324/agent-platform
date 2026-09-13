-- Phase 1: SqliteStore initial schema.
-- DESIGN.md §4.3: events は追記専用（append-only）の正典（source of truth）であり、
-- tasks は現在状態を高速に引くための派生ビュー（非正規化されたスナップショット）として扱う。
-- このクレートのコードから events に対して UPDATE/DELETE を発行してはならない。

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    kind TEXT NOT NULL,
    parent_id TEXT,
    priority INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    lease_worker_run_id TEXT,
    lease_expires_at TEXT,
    json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks (status);
CREATE INDEX IF NOT EXISTS idx_tasks_parent_id ON tasks (parent_id);

CREATE TABLE IF NOT EXISTS events (
    task_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    ts TEXT NOT NULL,
    json TEXT NOT NULL,
    PRIMARY KEY (task_id, seq)
);
