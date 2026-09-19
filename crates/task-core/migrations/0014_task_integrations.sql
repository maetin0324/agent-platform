-- Migration 14 (schema_version=14): Phase 54 / ADR-0043 D5。
--
-- タスクのブランチを**人が**取り込んだ記録（intake）。取り込みは 3 通り:
--   `merge`   — default_branch に rebase して fast-forward（push はしない）
--   `pr`      — `git push -u origin <branch>` → `gh pr create`。以後 GUI に PR が出る
--   `discard` — worktree とブランチを消す（確認付き）
--
-- 列の意味（ADR-0043 D5 の列に 1 つだけ足している。下の注記を参照）:
--   `task_id`    — 取り込むブランチを作ったタスク。
--   `repo_id`    — `project_repos.id`。**NULL 可**（注記）。
--   `repo_name`  — タスクの中でのリポジトリの名前（`repos/<name>/`）。API の URL もこれで引く。
--   `method`     — `merge` / `pr` / `discard`。
--   `state`      — `done`（merge / discard が終わった）/ `open`（PR が開いている）/
--                  `merged` / `closed`（PR の行方）/ `conflict`（rebase が衝突した。解消タスクを作った）/
--                  `failed`（git / gh が失敗した。`detail` に理由）。
--   `pr_number` / `pr_url` — `pr` のときだけ。
--   `merged_at`  — PR が merge された時刻（`gh pr view --json mergedAt`）か、`merge` が成功した時刻。
--   `detail`     — 人に見せる 1 行（409 の理由、衝突したファイル、gh の失敗など）。
--
-- ADR-0043 D5 との差（Phase 54 の実装判断。PROGRESS の P54-1 にも書く）:
--   D5 の列は `(id, task_id, repo_id, method, state, pr_number, pr_url, merged_at, created_at)` だが、
--   (1) **`repo_name` を足した**。Phase 49 の 1 リポジトリのタスク（`project_repos` の行を持たない）にも
--       取り込みが要るし、API の URL（`/changes/{repo}`）も名前で引くため。
--   (2) そのため **`repo_id` は NULL 可**（`project_repos` に行が無いリポジトリ）。
--   (3) `detail` と `updated_at` を足した（人に理由を見せる / PR の同期で書き換わる）。
--
-- 既存の migration と同じ流儀: 外部キー制約は張らない。

CREATE TABLE IF NOT EXISTS task_integrations (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    repo_id TEXT,
    repo_name TEXT NOT NULL,
    method TEXT NOT NULL CHECK(method IN ('merge','pr','discard')),
    state TEXT NOT NULL CHECK(state IN ('done','open','merged','closed','conflict','failed')),
    pr_number INTEGER,
    pr_url TEXT,
    merged_at TEXT,
    detail TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- タスク画面（`GET /tasks/{id}/changes`）が「このリポジトリの最新の取り込み」を引く。
CREATE INDEX IF NOT EXISTS idx_task_integrations_task
    ON task_integrations (task_id, repo_name, created_at DESC);

-- 案件画面（`GET /projects/{id}/integrations`）が開いている PR を先に引く。
CREATE INDEX IF NOT EXISTS idx_task_integrations_state
    ON task_integrations (state, updated_at DESC);
