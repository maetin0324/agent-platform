-- Migration 18 (schema_version=18): Phase 62 / ADR-0047 D4。
--
-- 「知識の自動メンテナンス」。
--
--   `knowledge_runs` — 元のタスクごとに知識整理 run を高々 1 回だけ起こすための決定的な目印
--                       （`celeris::knowledge_maint` が tick で読み書きする）。`task_id` は元のタスク
--                       （PK）、`run_task_id` は知識整理の裏方タスク（`role = "knowledge"`。
--                       `task_core::report::KNOWLEDGE_ROLE`）。`state` は `scheduled` | `done` | `failed`。
--                       `applied_at` / `summary_json` は run が終端になり
--                       `task_ops::knowledge::apply_candidates` が候補を適用し終えたときに埋まる
--                       （`summary_json` = `{candidates, ingested, inbox, discarded}`）。

CREATE TABLE IF NOT EXISTS knowledge_runs (
    task_id TEXT PRIMARY KEY,
    run_task_id TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    applied_at TEXT,
    summary_json TEXT
);

CREATE INDEX IF NOT EXISTS idx_knowledge_runs_run_task_id ON knowledge_runs (run_task_id);
