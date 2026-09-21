-- Migration 22 (schema_version=22): Phase 65 / ADR-0053 D1。
--
-- `llm_proxy_requests` — celeris の LLM source ローカルプロキシ（`crates/llm-proxy`）が処理した
--                         `POST /v1/chat/completions` 1 件ごとの記録。**本文（プロンプト・応答）は
--                         書かない**。値そのもの（トークン等）も記録しない（トークン数の件数のみ）。
--
--   id               — 要求 id（ULID）。
--   ts               — 受け付けた Unix 秒。
--   source           — 選ばれた供給元（`claude-oauth` / `codex-oauth` / `openai-compatible:<id>`）。
--                       候補が無く選べなかったときは NULL。
--   account          — 選ばれたアカウント id（`claude-oauth`/`codex-oauth`）。`openai-compatible` や
--                       選べなかったときは NULL。
--   requested_model  — クライアントが渡した抽象モデル名（`celeris/cheap` 等）。
--   upstream_model   — 実際に上流へ渡した具体モデル名。選べなかったときは NULL。
--   prompt_tokens / completion_tokens — 上流の usage から分かった件数。不明なら NULL。
--   latency_ms       — 受け付けから応答完了（または失敗）までの所要時間。
--   status           — `ok` / `error` / `unavailable`（候補が無い）。
--   error_kind       — 失敗の種別（`unauthorized` / `rate_limited` / `upstream` / `network` 等）。無ければ NULL。
CREATE TABLE IF NOT EXISTS llm_proxy_requests (
    id TEXT PRIMARY KEY,
    ts INTEGER NOT NULL,
    source TEXT,
    account TEXT,
    requested_model TEXT NOT NULL,
    upstream_model TEXT,
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    latency_ms INTEGER NOT NULL,
    status TEXT NOT NULL,
    error_kind TEXT
);

CREATE INDEX IF NOT EXISTS idx_llm_proxy_requests_ts ON llm_proxy_requests (ts);
CREATE INDEX IF NOT EXISTS idx_llm_proxy_requests_source ON llm_proxy_requests (source, ts);
