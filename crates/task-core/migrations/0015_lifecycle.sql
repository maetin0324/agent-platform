-- Migration 15 (schema_version=15): Phase 55 / ADR-0044 D6。
--
-- 案件と途中目標の「中止・一時停止・アーカイブ」。
--
--   `projects.status`    — 既存の `proposed` / `active` / `paused` / `done` に **`cancelled`** を足す
--                          （値は文字列。この表に CHECK 制約は無いので列の変更は不要。`ProjectStatus` の
--                          `parse` が知らない値を弾く）。終端は `done` と `cancelled`。
--   `projects.paused_from` — `pause` する直前の状態。`resume` でここへ戻す（ADR-0044 D6）。
--                          `paused` でない行は NULL。
--   `projects.archived_at` — アーカイブした時刻（RFC 3339）。NULL ならアーカイブされていない。
--                          終端（`done` / `cancelled`）の案件だけがアーカイブできる。
--                          `GET /projects` と `GET /tasks` は既定でこの列が NULL のものだけを返す
--                          （`?archived=1` で全部）。
--   `milestones.status`  — 既存の `proposed` / `approved` / `in_progress` / `reached` / `redesigned` に
--                          **`paused`** と **`cancelled`** を足す（同上、CHECK 制約は無い）。
--   `milestones.paused_from` — 同じく `pause` する直前の状態。
--
-- 既存の migration と同じ流儀: 外部キー制約は張らない。列は「絞り込みと復元のための事実」だけを持ち、
-- 判断（連鎖の中止・dispatch の抑止）は `task-ops` / ストアの関数が行う。

ALTER TABLE projects ADD COLUMN archived_at TEXT;
ALTER TABLE projects ADD COLUMN paused_from TEXT;
ALTER TABLE milestones ADD COLUMN paused_from TEXT;

-- 一覧（`GET /projects`）が「アーカイブされていない案件」を引く。
CREATE INDEX IF NOT EXISTS idx_projects_archived_at ON projects (archived_at);

-- ディスパッチャ（`ready_tasks`）が「一時停止中の途中目標」を引く。
CREATE INDEX IF NOT EXISTS idx_milestones_status ON milestones (status);
