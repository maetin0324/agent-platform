-- Migration 24（schema_version=24）: ADR-0056 D1 / D4（Phase 78）。
--
-- `mcp_clients` — 外部エージェントが Celeris の MCP サーバーへ話しかけるためのクライアント。
--   トークンの値そのものは持たない（`token_hash` は SHA-256 の 16 進文字列。値はログ・応答・DB の
--   どこにも平文で残らない。`celerisctl mcp client add` が発行時に 1 度だけ表示する）。
--
--   id           — クライアントの id（ULID）。
--   name         — 人が付けた名前（`chatgpt` 等）。
--   token_hash   — トークンの SHA-256（16 進、64 文字）。一意（NULL 可。複数行が NULL でも
--                   SQLite の UNIQUE は NULL 同士を区別する）。**NULL は `--no-token` で作った客**
--                   （ADR-0056 D1 の `auth = "none"` の口に `client = "<id>"` で固定する専用。
--                   `auth = "token"` の口では絶対に一致しない＝この客は Bearer では認証できない）。
--   scopes       — カンマ区切りのスコープ（`knowledge:read,knowledge:propose,...`）。
--   created_at   — 発行時刻。
--   last_used_at — 最後に `tools/call` を受けた時刻（NULL は未使用）。
--   revoked_at   — 失効させた時刻（NULL は有効）。
--
-- `mcp_calls` — `tools/call` の監査ログ（ADR-0056 D4: 引数と結果の本文は残さない）。
--
--   id          — 呼び出しの id（ULID）。
--   client_id   — `mcp_clients.id`。
--   tool        — 呼んだ道具の名前。
--   ok          — 成功したか。
--   error_kind  — 失敗したときの種類（JSON-RPC のエラーコード相当の短い文字列。成功なら NULL）。
--   latency_ms  — かかった時間（ミリ秒）。
--   at          — 呼び出し時刻。

CREATE TABLE IF NOT EXISTS mcp_clients (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    token_hash TEXT UNIQUE,
    scopes TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
);

CREATE TABLE IF NOT EXISTS mcp_calls (
    id TEXT PRIMARY KEY,
    client_id TEXT NOT NULL,
    tool TEXT NOT NULL,
    ok INTEGER NOT NULL,
    error_kind TEXT,
    latency_ms INTEGER NOT NULL,
    at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_mcp_calls_client_at ON mcp_calls (client_id, at DESC, id DESC);
