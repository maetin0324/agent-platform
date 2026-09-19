-- Migration 11 (schema_version=11): Phase 47 / ADR-0040 D4。
--
-- 昇格（新しいリリースへの切り替え）をライブ引き継ぎで行うために、taskd の「インスタンスの役割」を
-- DB に 1 行ずつ持つ。判断はすべて tick の中で決定的に行う（LLM は関与しない）。
--
-- 列の意味:
--   `instance_id`          — ULID（プロセスごとに 1 つ。API の `instance_id` と同じ値）。
--   `release`              — そのプロセスのリリース（`--release <sha12>` / `TASKD_RELEASE` / `"dev"`）。
--                            起動時に同じ `release` の `active` がいたら二重起動なので exit 3 する。
--   `pid`                  — そのプロセスの pid（`status.sh` が人に見せる）。
--   `role`                 — `active`（dispatch と裏方を動かす。常に 1 つ）/ `standby`（API だけ受ける）/
--                            `draining`（listener を閉じ、手元の run だけ面倒を見る）/ `verify`（D3 の検証。
--                            **この表には行を書かない**。値としてだけ許す）。
--   `started_at`           — 起動時刻（RFC 3339）。
--   `heartbeat_at`         — 最後の tick の時刻（RFC 3339）。`now - heartbeat_at` が
--                            `3 × tick + lease_grace` を超えたら「古い」＝死んだとみなす。
--   `handoff_requested_at` — 新しい standby が「引き継ぎたい」と書いた時刻。`active` はこれを見たら
--                            同じ tick で `draining` になる。
--   `drained_at`           — 手元の run が 0 になって exit する直前に書く時刻。
--
-- 既存の migration と同じ流儀: 外部キー制約は張らない。`release` は SQLite の予約語なので常に引用する。

CREATE TABLE IF NOT EXISTS daemon_instances (
    instance_id TEXT PRIMARY KEY,
    "release" TEXT NOT NULL,
    pid INTEGER NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('active','standby','draining','verify')),
    started_at TEXT NOT NULL,
    heartbeat_at TEXT NOT NULL,
    handoff_requested_at TEXT,
    drained_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_daemon_instances_role ON daemon_instances (role, heartbeat_at);
