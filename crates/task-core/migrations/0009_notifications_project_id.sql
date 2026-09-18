-- Migration 9 (schema_version=9): Phase 40 / ADR-0037 D6（GUI 依頼 G13i-P1）。
--
-- GUI が `milestone_ready` / `secretary_reply` の通知から案件へリンクを張れるように、
-- `notifications` に `project_id` を持たせる。判定（`taskd::notify::scan`）が候補を作った時点で
-- 案件 id を知っているので、そのまま台帳に書く（応答時に途中目標から逆引きしない。決定的で安い）。
--
--   `project_id` — `milestone_ready` はその途中目標の案件、`secretary_reply` はその案件自身、
--                  他の種は NULL（GUI はリンクを作らない）。

ALTER TABLE notifications ADD COLUMN project_id TEXT;
