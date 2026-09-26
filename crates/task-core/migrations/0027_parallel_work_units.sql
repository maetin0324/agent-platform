-- Migration 27 (schema_version=27): ADR-0074 D1（Phase F2）。
--
-- `work_units` に、v2（`celeris.execution-plan/2`）の並列実行が使う列を足す。すべて events
-- （`WorkUnitTransitioned` / `WorkUnitCommitted` / `PhaseIntegrated`）の派生で、落としても
-- `replay` で作り直せる（`lease_run_id` / `lease_expires_at` は Task の lease と同じく揮発で、
-- replay の突き合わせからは外す。ADR-0074 §5.2）。
--
--   phase             — v2 の工程の key（v1 の WU は NULL のまま）
--   lease_run_id      — この WU を今実行している run（`acquire_work_unit_lease`。D1.5）
--   lease_expires_at  — 上の lease の期限
--   branch            — `celeris-wu/<task_id>/<key>`（D1.2）
--   base_commit       — この WU の worktree を切った時点の基点 sha（D1.2）
--   head_commit       — `WorkUnitCommitted`（run が done になったときの決定的な commit）
--   integrated_commit — `PhaseIntegrated`（工程末尾の統合で Task ブランチに merge された commit）
--
-- v1・atomic な Task はすべて NULL のまま（挙動は 1 バイトも変えない）。

ALTER TABLE work_units ADD COLUMN phase TEXT;
ALTER TABLE work_units ADD COLUMN lease_run_id TEXT;
ALTER TABLE work_units ADD COLUMN lease_expires_at TEXT;
ALTER TABLE work_units ADD COLUMN branch TEXT;
ALTER TABLE work_units ADD COLUMN base_commit TEXT;
ALTER TABLE work_units ADD COLUMN head_commit TEXT;
ALTER TABLE work_units ADD COLUMN integrated_commit TEXT;
CREATE INDEX IF NOT EXISTS idx_work_units_lease ON work_units (lease_expires_at) WHERE lease_run_id IS NOT NULL;
