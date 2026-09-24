-- Migration 26 (schema_version=26): ADR-0072 D5 / Phase E2。
--
-- `execution_plans` / `work_units` / `runs` — Task の下の内部実行層（ExecutionPlan / WorkUnit / Run）
-- の**派生の索引**。正本は `events`（`Event::ExecutionPlanned` / `Event::WorkUnitTransitioned` /
-- `Event::WorkerStarted` / `Event::WorkerFinished` / `Event::CheckpointSaved`）で、この 3 表は
-- 対応する Event と同じトランザクションで書く（DESIGN 原則 6）。落としても `replay` で events から
-- 作り直せる。暗黙の WorkUnit（ExecutionPlan を持たない Task）には行を作らない。
--
-- execution_plans:
--   id             — ULID
--   task_id        — 対象タスク
--   version        — 1, 2, …（replanning で +1。E2 は常に 1）
--   origin         — 'planner' | 'human' | 'repair' | 'fixture'
--   planner_run_id — origin = 'planner' のときの run_id
--   status         — 'active' | 'superseded' | 'completed' | 'abandoned'
--   json           — ExecutionPlanSpec 全体
--   created_at / superseded_at
--
-- work_units:
--   id             — ULID
--   task_id        — 対象タスク
--   plan_id        — この行を作った計画の版（replan で持ち越した done の WU は元の plan_id のまま）
--   key            — 計画の中の slug（task_id 内で一意）
--   seq            — 計画の中の順番（トポロジカル順の tie-break）
--   kind           — investigate|design|implement|test|release|repair|other
--   status         — WorkUnit の状態（ADR-0072 D6）
--   blocked_reason — question|dependency_failed|limit（status = blocked のとき）
--   depends_on_json — key の配列（JSON）
--   runs / continuations / retries — 回数
--   last_run_id / last_checkpoint_run_id
--   json           — WorkUnitSpec と repair_of など
--   created_at / updated_at
--
-- runs:
--   run_id         — 既存の run_id（WorkerStarted と同じ）
--   task_id        — 対象タスク
--   work_unit_id   — NULL = 暗黙の WorkUnit
--   role           — worker|reviewer|planner|wrap_up
--   seq            — WorkUnit の中の Run #n
--   status         — Run の状態（ADR-0072 D6）
--   adapter / model / account / session_id
--   checkpoint_json — 確定した Checkpoint（あれば）
--   usage_json / metrics_json
--   started_at / finished_at
--
-- `runs` は E2 以降、全タスクの worker / reviewer / planner run について書く（atomic なタスクも
-- 含む）。E2 より前の run は埋め戻さない。

CREATE TABLE IF NOT EXISTS execution_plans (
    id            TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL,
    version       INTEGER NOT NULL,
    origin        TEXT NOT NULL CHECK (origin IN ('planner','human','repair','fixture')),
    planner_run_id TEXT,
    status        TEXT NOT NULL CHECK (status IN ('active','superseded','completed','abandoned')),
    json          TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    superseded_at TEXT,
    UNIQUE (task_id, version)
);
CREATE INDEX IF NOT EXISTS idx_execution_plans_task ON execution_plans (task_id, status);

CREATE TABLE IF NOT EXISTS work_units (
    id            TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL,
    plan_id       TEXT NOT NULL,
    key           TEXT NOT NULL,
    seq           INTEGER NOT NULL,
    kind          TEXT NOT NULL,
    status        TEXT NOT NULL,
    blocked_reason TEXT,
    depends_on_json TEXT NOT NULL DEFAULT '[]',
    runs          INTEGER NOT NULL DEFAULT 0,
    continuations INTEGER NOT NULL DEFAULT 0,
    retries       INTEGER NOT NULL DEFAULT 0,
    last_run_id   TEXT,
    last_checkpoint_run_id TEXT,
    json          TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (task_id, key)
);
CREATE INDEX IF NOT EXISTS idx_work_units_task ON work_units (task_id, status);

CREATE TABLE IF NOT EXISTS runs (
    run_id        TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL,
    work_unit_id  TEXT,
    role          TEXT NOT NULL,
    seq           INTEGER NOT NULL,
    status        TEXT NOT NULL,
    adapter TEXT, model TEXT, account TEXT, session_id TEXT,
    checkpoint_json TEXT,
    usage_json TEXT, metrics_json TEXT,
    started_at    TEXT NOT NULL,
    finished_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_runs_task ON runs (task_id, started_at);
CREATE INDEX IF NOT EXISTS idx_runs_work_unit ON runs (work_unit_id);
