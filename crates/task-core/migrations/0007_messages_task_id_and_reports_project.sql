-- Migration 7 (schema_version=7): Phase 27。GUI からの依頼 R4 と ADR-0034 D1 の負債を 1 回で返す。
--
-- 1. `messages.task_id`: 対話の 1 往復（人の発言と、そのノードの返事）を、それを起こした対話用タスクに
--    直接ひも付ける（`role = user` の行にも `role = node` の行にも同じ id が入る）。今までは
--    `Task.conversation` → 人の発言 id、返事 → `run_id` の 2 段を辿る必要があった（Phase 24 の P-74）。
-- 2. `reports.project_id` を NULL 可にする: ADR-0034 D1 は「案件なしの報告は空文字列で書く」という
--    センチネルを採ったが、これは migration を足さない制約から来た回避策で、D1 自身が「次に reports を
--    触る migration があれば NULL 可に直してよい」と書いている。SQLite は列の NOT NULL を落とせないので
--    表を作り直して写す（行数は多くない）。
--
-- 既存の migration と同じ流儀: 外部キー制約は張らない。

ALTER TABLE messages ADD COLUMN task_id TEXT;

CREATE INDEX IF NOT EXISTS idx_messages_task_id ON messages (task_id);

-- reports: project_id を NULL 可にする（空文字列のセンチネルは NULL に直す）。
ALTER TABLE reports RENAME TO reports_old_0006;

CREATE TABLE reports (
    id TEXT PRIMARY KEY,
    project_id TEXT,
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

INSERT INTO reports (id, project_id, node_id, task_id, kind, level, headline, body, sources, read_at, created_at)
SELECT id, NULLIF(project_id, ''), node_id, task_id, kind, level, headline, body, sources, read_at, created_at
FROM reports_old_0006;

DROP TABLE reports_old_0006;

CREATE INDEX IF NOT EXISTS idx_reports_project_level_read ON reports (project_id, level, read_at);
