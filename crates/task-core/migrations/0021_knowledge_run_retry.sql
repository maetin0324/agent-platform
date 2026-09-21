-- Migration 21 (schema_version=21): Phase 64 / ADR-0052 D2 + D3。
--
-- 「知識整理 run のフォールバックと一度だけのやり直し」。
--
--   `knowledge_runs.retried_at` — ADR-0052 D3。失敗した知識整理 run を**一度だけ**作り直したときの時刻。
--                                  `NULL` = まだやり直していない（次の tick で 1 回だけ作り直す）。
--                                  値が入っている行は二度と自動では作り直さない（人が
--                                  `celerisctl knowledge rerun <task_id>` で消せる）。
--   `knowledge_runs.via`         — ADR-0052 D2。その run で実際に抽出した経路。
--                                  `"langmem"`（従来どおり Qwen）か `"fallback:<adapter>"`
--                                  （Qwen に届かず tier cheap の汎用ハーネスに倒した）。
--                                  `summary_json.via` にも同じ値が入る（Console・タイムラインが読む）。

ALTER TABLE knowledge_runs ADD COLUMN retried_at TEXT;
ALTER TABLE knowledge_runs ADD COLUMN via TEXT;
