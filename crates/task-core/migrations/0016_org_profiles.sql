-- Migration 16 (schema_version=16): Phase 59 / ADR-0046。
--
-- 「組織 = Agent Profile の継承木」。
--
--   `org_nodes.profile_json` — ADR-0046 D1 の profile（skills / knowledge / harnesses / tools /
--                              deny_tools / run / model / policy / review / permissions）。正本はこの列。
--                              空の profile は NULL（導入前のノードと同じ）。子は親を継ぐが、**継承の
--                              計算は保存しない**（`task_core::profile::resolve` が読むたびに決める）。
--   `tasks.skills_json`      — ADR-0046 D2 の「そのタスクに必要な能力タグ」の写し（JSON の配列）。
--                              正本は `tasks.json` の中の `skills`。matching（D5）と絞り込みに使う。
--   `tasks.mode`             — ADR-0046 D4 の進め方（`prototype` | `production` | `research`）。
--                              既定は `production`（導入前のタスクは全部これ。従来の挙動と同じ）。
--
-- 既存の migration と同じ流儀: 外部キー制約は張らない。正本は `tasks.json` / `org_nodes.profile_json` で、
-- `tasks` 側の 2 列は絞り込みのための写し（挿入時と `PATCH /tasks/{id}` のときに書き直す）。
-- 判断（継承の merge・担当の決定・レビューの切替）は `task-core` / `task-ops` の純粋関数が行う。

ALTER TABLE org_nodes ADD COLUMN profile_json TEXT;

ALTER TABLE tasks ADD COLUMN skills_json TEXT;
ALTER TABLE tasks ADD COLUMN mode TEXT;

-- 既存の行は「必要な skill 無し・production」。`json` 側には何も書かない（`Task` の serde の既定と同じ）。
UPDATE tasks SET skills_json = '[]' WHERE skills_json IS NULL;
UPDATE tasks SET mode = 'production' WHERE mode IS NULL;

-- ADR-0046 D5: matching は「その harness を受けられるノード」を引く。絞り込みは `mode` と skill でも効く。
CREATE INDEX IF NOT EXISTS idx_tasks_mode ON tasks (mode);
