-- Migration 17 (schema_version=17): Phase 60b / ADR-0048 D3。
--
-- 「Console の入力（POST /console/instruct）と CoS の actions」。
--
--   `messages.metadata_json`  — CoS の返事に添える actions の実行結果（実行できた action / 実行できな
--                               かった action）。GUI の Console が `reply` ブロックの `actions_result`
--                               として出す。正本はこの列（`Message.metadata`）。導入前の行・actions を
--                               伴わない返事は NULL。
--   `console_action_runs`     — その run の actions を実行済みか（決定的な冪等性の目印。ADR-0048 D3
--                               「Idempotent per run」）。`run_id` は一意。2 回目の実行要求は何もしない。

ALTER TABLE messages ADD COLUMN metadata_json TEXT;

CREATE TABLE IF NOT EXISTS console_action_runs (
    run_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    executed_at TEXT NOT NULL
);
