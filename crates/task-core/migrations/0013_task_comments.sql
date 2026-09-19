-- Migration 13 (schema_version=13): Phase 53 / ADR-0044 D2・D3。
--
-- タスク単位のコメントと、ラベル・種類。
--
--   `task_comments` — 人・組織の「人」・taskd が 1 件ずつ書く**残る**記録（`progress` と違って消えない）。
--                     人のコメントは担当をすぐ起こす（ADR-0044 D2 の表。判断は task-ops）。
--     `author_kind`   `human`（人）/ `node`（組織のノード）/ `system`（taskd）。
--     `author`        `node` のときの `org_nodes.id`（それ以外は NULL）。
--     `run_id`        ワーカーが書いたコメントの run（人のコメントは NULL）。
--
--   `tasks.labels_json` — `Task.labels` の写し（JSON の配列。`GET /tasks?label=` の絞り込み用）。
--   `tasks.category`    — `Task.category` の写し（`feature|bug|research|ops|docs|other`。`?category=` 用）。
--
-- 既存の migration と同じ流儀: 外部キー制約は張らない。正本は `tasks.json` で、列は絞り込みのための写し
-- （挿入時と `PATCH /tasks/{id}` のときに書き直す）。

CREATE TABLE IF NOT EXISTS task_comments (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    author_kind TEXT NOT NULL CHECK(author_kind IN ('human','node','system')),
    author TEXT,
    body TEXT NOT NULL,
    run_id TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_task_comments_task ON task_comments (task_id, created_at, id);

ALTER TABLE tasks ADD COLUMN labels_json TEXT;
ALTER TABLE tasks ADD COLUMN category TEXT;

-- 既存の行は「ラベル無し・種類 other」。`json` 側には何も書かない（`Task` の serde の既定と同じ）。
UPDATE tasks SET labels_json = '[]', category = 'other' WHERE labels_json IS NULL;

CREATE INDEX IF NOT EXISTS idx_tasks_category ON tasks (category);
