-- Migration 4 (schema_version=4): 一覧の検索（`q`）の対象に objective を加えるための非正規化列（ADR-0014 D2）。
-- objective は作成後に変わらないので、既存行は json 列から 1 回だけ埋める。

ALTER TABLE tasks ADD COLUMN objective TEXT NOT NULL DEFAULT '';

UPDATE tasks SET objective = COALESCE(json_extract(json, '$.objective'), '');
