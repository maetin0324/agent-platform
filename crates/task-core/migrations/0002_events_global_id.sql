-- Migration 2 (schema_version=2): events にグローバルな単調 id を持たせる（ADR-0013 D6）。
-- 既存の `events`（PRIMARY KEY(task_id, seq)）を id 付きの表に作り直す。行の内容・順序は
-- 変えない（rowid 順で写す）。追記専用の不変条件は保つ（表の作り直しであり、UPDATE/DELETE
-- による書き換えではない）。

CREATE TABLE events_new (
    id INTEGER PRIMARY KEY,
    task_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    ts TEXT NOT NULL,
    json TEXT NOT NULL,
    UNIQUE(task_id, seq)
);

INSERT INTO events_new (task_id, seq, ts, json)
SELECT task_id, seq, ts, json FROM events ORDER BY rowid;

DROP TABLE events;

ALTER TABLE events_new RENAME TO events;
