-- Migration 5 (schema_version=5): ADR-0027 D1. タスクの分野（genre）を持つ列を足す。
-- genre は任意（NULL 許容）で、objective と同じく作成後に変わらないので、既存行は json 列から 1 回だけ埋める。

ALTER TABLE tasks ADD COLUMN genre TEXT;

UPDATE tasks SET genre = json_extract(json, '$.genre');
