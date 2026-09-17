-- Migration 6 (schema_version=6): ADR-0033 D1〜D5。組織・案件・途中目標・報告・対話・認可のテーブルを
-- 1 回で作る（Phase 23 が使うのは org_nodes / projects / milestones / tasks の列だけだが、後続の Phase が
-- 並行で載るので表だけ先に用意する。使わない列があってもよい）。
--
-- 既存の migration と同じ流儀: 外部キー制約は張らない（`PRAGMA foreign_keys` を触らない方針。
-- 参照の整合はストアの関数が見る）。id は ULID か英小文字ケバブの文字列。

-- D1: 組織（一つ、役割の木）。`kind` は secretary | department | section、根の secretary は 1 つだけ。
CREATE TABLE IF NOT EXISTS org_nodes (
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    genre TEXT,
    brief TEXT NOT NULL DEFAULT '',
    position INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_org_nodes_parent_id ON org_nodes (parent_id);

-- D2: 案件と途中目標。
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    request TEXT NOT NULL,
    status TEXT NOT NULL,
    secretary_summary TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_projects_status ON projects (status);

CREATE TABLE IF NOT EXISTS milestones (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_milestones_project_id ON milestones (project_id, seq);

-- D3: 報告（下から上へ。`sources` は元になった report id の JSON 配列）。
CREATE TABLE IF NOT EXISTS reports (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    task_id TEXT,
    kind TEXT NOT NULL,
    level INTEGER NOT NULL DEFAULT 0,
    headline TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL DEFAULT '',
    sources TEXT NOT NULL DEFAULT '[]',
    read_at TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_reports_project_level_read ON reports (project_id, level, read_at);

-- D4: 対話（秘書にも、どの「人」にも話せる）。
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    project_id TEXT,
    role TEXT NOT NULL,
    text TEXT NOT NULL DEFAULT '',
    run_id TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_node_project_created ON messages (node_id, project_id, created_at);

-- D5: 認可（今回だけ / 今後ずっと）。
CREATE TABLE IF NOT EXISTS approvals (
    id TEXT PRIMARY KEY,
    project_id TEXT,
    node_id TEXT NOT NULL,
    task_id TEXT,
    question TEXT NOT NULL DEFAULT '',
    decision TEXT,
    answer TEXT,
    created_at TEXT NOT NULL,
    decided_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_approvals_decision ON approvals (decision);

CREATE TABLE IF NOT EXISTS standing_rules (
    id TEXT PRIMARY KEY,
    node_id TEXT,
    rule TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

-- D2: タスクは案件・途中目標に属し、組織のノードに割り当てられる。いずれも任意（NULL 許容）で、
-- 0004/0005 と同じく作成後に変わらないので、既存行は json 列から 1 回だけ埋める（導入前の行は NULL）。
ALTER TABLE tasks ADD COLUMN project_id TEXT;
ALTER TABLE tasks ADD COLUMN milestone_id TEXT;
ALTER TABLE tasks ADD COLUMN assignee TEXT;

UPDATE tasks SET
    project_id = json_extract(json, '$.project_id'),
    milestone_id = json_extract(json, '$.milestone_id'),
    assignee = json_extract(json, '$.assignee');

CREATE INDEX IF NOT EXISTS idx_tasks_project_id ON tasks (project_id);
