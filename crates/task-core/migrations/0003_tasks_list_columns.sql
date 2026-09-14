-- Migration 3 (schema_version=3): 一覧のページングと検索のための非正規化列と索引
-- （ADR-0013 D10）。既存行は json 列から title / updated_at を埋める。

ALTER TABLE tasks ADD COLUMN title TEXT NOT NULL DEFAULT '';
ALTER TABLE tasks ADD COLUMN updated_at TEXT NOT NULL DEFAULT '';

UPDATE tasks SET
    title = COALESCE(json_extract(json, '$.title'), ''),
    updated_at = COALESCE(json_extract(json, '$.updated_at'), '');

CREATE INDEX IF NOT EXISTS idx_tasks_status_priority_created_at ON tasks (status, priority, created_at);
CREATE INDEX IF NOT EXISTS idx_tasks_updated_at ON tasks (updated_at);
