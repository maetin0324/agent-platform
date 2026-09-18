-- Migration 8 (schema_version=8): Phase 39 / ADR-0037。人の判断が要るときだけ Discord に知らせる。
--
-- `notifications` は「その (kind, key) についてはもう知らせた」ことだけを覚える小さな表。
-- `UNIQUE(kind, key)` が重複排除そのもので、判定（決定的）は tick ごとに何度でも走ってよい
-- （2 回目以降の `INSERT OR IGNORE` は何も起こさない）。
--
-- 列の意味:
--   `body`     — 送る文面（決定的な定型文。LLM は関与しない。webhook の URL は入らない）。
--   `sent_at`  — 送れた時刻（RFC 3339）。まだなら NULL。
--   `attempts` — POST を試した回数（3 回で諦める。ADR-0037 D1）。
--   `ok`       — NULL = まだ決着していない（次の tick で再送）、1 = 送れた、0 = 諦めた。
--   `error`    — 最後の失敗の理由。**URL・ホスト名は入れない**（秘密なので。ADR-0037 D3）。
--
-- 既存の migration と同じ流儀: 外部キー制約は張らない。

CREATE TABLE IF NOT EXISTS notifications (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    sent_at TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    ok INTEGER,
    error TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_notifications_kind_key ON notifications (kind, key);
CREATE INDEX IF NOT EXISTS idx_notifications_pending ON notifications (ok, created_at);
