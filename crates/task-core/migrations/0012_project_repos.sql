-- Migration 12 (schema_version=12): Phase 52 / ADR-0043 D1 / D2。
--
-- 案件は「リポジトリ」を複数持つ（論文の `benchfs-paper` とコードの `benchfs`、git でないデータの置き場）。
-- タスクはそのうち使うものを選び、git のものはリポジトリごとに worktree を切る。
--
-- 列の意味（ADR-0043 D1 そのまま）:
--   `name`           — 案件内で一意の slug（既定はディレクトリ名）。タスクの作業場所では
--                      `<workspace_root>/<task_id>/repos/<name>/` というディレクトリ名になる。
--   `kind`           — `git`（worktree を切る）/ `dir`（シンボリックリンクで見せる。コピーしない）。
--   `location_json`  — `WorkspaceSpec` の JSON（`{"kind":"local","path":"/abs"}` /
--                      `{"kind":"remote","cluster":"pegasus","path":"/abs"}`）。`~` は展開済み。
--   `default_branch` — git のみ。無ければ検出（origin/HEAD → main → master）。
--   `sync`           — remote のみ: `worktree`（既定 = ADR-0019 の (a)）/ `rsync` / `none`。
--   `run`            — `auto`（`workspace.toml` に従う、無ければ host）/ `host` / `container`。
--                      `container` の実行は ADR-0043 A3（この Phase では読むだけ）。
--   `is_primary`     — 案件の「主なリポジトリ」。1 案件に 1 つ。`Project.workspace` はこの写しを返す。
--
-- 既存 DB の写し（ADR-0043 D1「既存の `projects.workspace` は migration で `is_primary = 1` の
-- リポジトリ 1 件に写す」）は **Rust 側（`SqliteStore::backfill_project_repos`）でこの版を適用した
-- 直後に、同じトランザクションで**行う。理由は 2 つ:
--   1. id は ULID なので SQL では作れない。
--   2. `kind` は「パスが git なら git、でなければ dir」だが、SQL からはファイルシステムを見られない
--      （`<path>/.git` の有無で決める。リモートのパスは taskd から見えないので `git` に倒す
--      ＝ ADR-0018 / 0019 のクラスタ側の作業場所は git 前提の同期をするため。人は `PATCH /repos/{id}` で直せる）。
--
-- `projects.workspace` 列は**残し、primary の写しとして書き続ける**（ADR-0043 D1 は「書かない」と
-- 書いているが、N-1 互換〈ADR-0040 D3 の検査 5: 旧バイナリが新スキーマの DB を読む〉のために写しを
-- 残す方が安全なので、そちらを採る。読むときは primary の `location_json` が優先）。
--
-- 既存の migration と同じ流儀: 外部キー制約は張らない。

CREATE TABLE IF NOT EXISTS project_repos (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('git','dir')),
    location_json TEXT NOT NULL,
    default_branch TEXT,
    sync TEXT,
    run TEXT NOT NULL DEFAULT 'auto',
    is_primary INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    UNIQUE(project_id, name)
);

CREATE INDEX IF NOT EXISTS idx_project_repos_project ON project_repos (project_id, is_primary DESC, name ASC);

-- ADR-0043 D2: タスクが使うリポジトリ（`Vec<RepoRef {repo_id, name}>` の JSON）。
-- 正は従来どおり `tasks.json` の中の `repos` で、この列は「このリポジトリを参照している未終端の
-- タスクはあるか」（`DELETE /repos/{id}` の 409）を引くための索引として持つ。導入前の行は NULL。
ALTER TABLE tasks ADD COLUMN repos_json TEXT;
