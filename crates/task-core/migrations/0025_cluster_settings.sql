-- Migration 25 (schema_version=25): ADR-0059 D6 / Phase 99。
--
-- `cluster_settings` — `[[clusters]]` のうち、GUI から上書きできる値。今のところ `work_dir`
-- （クラスタ側の実効の作業ディレクトリ）だけ。行が無ければ設定ファイルの値を使う（celeris 側で解決、
-- このテーブルは「上書き」だけを持つ）。
--
--   cluster_id  — `[[clusters]] id`。
--   work_dir    — 絶対パスか `~`/`~/…`。`NULL` にはせず、上書きを消すときは行ごと削除する
--                  （`PUT /clusters/{id}/settings {"work_dir": null}`）。
--   updated_at  — 最後に書いた時刻（RFC 3339）。

CREATE TABLE IF NOT EXISTS cluster_settings (
    cluster_id TEXT PRIMARY KEY,
    work_dir TEXT,
    updated_at TEXT NOT NULL
);
