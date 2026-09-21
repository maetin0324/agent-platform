-- Migration 23 (schema_version=23): Phase 67 / ADR-0054 D1。
--
-- `node_sessions` — ノードごとの**継続セッション**。アダプタの CLI が持つ「会話の継続」
--                    （`claude --resume` / `codex exec resume` / ACP の `session/load`）を、celeris 側で
--                    1 行の目印として追跡する。
--
--   node_id          — 組織ノードの id。CoS の対話は `cos` 固定（`kind = conversation`、
--                       `project_id` は常に NULL。ADR-0054 D1: 案件を開いていてもセッションは同じ 1 本）。
--                       部署の根ノード自身のレビュー・切り分け run は `kind = lead`、部署ごとに 1 本。
--   kind             — `conversation` | `lead`。
--   project_id       — 今のところ常に NULL（将来、案件単位のセッションを持たせる余地として列だけ用意）。
--   adapter          — セッションを持つアダプタ id（`claude-code` / `codex` / `acp`）。
--   account_id       — セッションを開いたときのアカウント（プールを使わない設定では NULL）。
--                       次の run で選ばれたアカウントがこれと違えば、このセッションは retire して作り直す。
--   session_id       — アダプタに渡す／アダプタから返る継続用の id。celeris が決める場合（claude-code の
--                       `--session-id`）は前もって埋まる。アダプタが決める場合（codex の `thread.started`、
--                       ACP の `session/new` の `sessionId`）は最初 `''` で、run の途中で確定してから書く。
--   turns            — このセッションで走った run の回数（新規作成時 0。1 run 終えるたびに +1）。
--   approx_tokens     — このセッションの累計トークン数のおおよそ（run の usage の合計）。
--                       `[sessions] rollover_tokens` を超えたら次の run から新しいセッションにする。
--   created_at       — このセッションを作った時刻。
--   last_used_at     — 最後に run で使った時刻。
--   retired_at       — 引退させた時刻（rollover・アカウント変更・resume 失敗・「新しい会話」で NULL でなくなる）。
--                       `retired_at IS NULL` が「有効なセッション」の唯一の判定。

CREATE TABLE IF NOT EXISTS node_sessions (
    id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    project_id TEXT,
    adapter TEXT NOT NULL,
    account_id TEXT,
    session_id TEXT NOT NULL,
    turns INTEGER NOT NULL DEFAULT 0,
    approx_tokens INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    last_used_at TEXT NOT NULL,
    retired_at TEXT
);

-- 「このノード・kind・案件の、今どれが有効なセッションか」を引く経路。`retired_at IS NULL` の行は
-- 高々 1 件になるよう、アプリケーション側（`node_session_create` の直前に `node_session_retire`）で維持する
-- （SQLite の部分 UNIQUE インデックスは使わず、決定的なコードで保証する。DESIGN 原則 1）。
CREATE INDEX IF NOT EXISTS idx_node_sessions_active
    ON node_sessions (node_id, kind, project_id, retired_at);
