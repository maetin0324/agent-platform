-- Migration 10 (schema_version=10): Phase 43 / ADR-0039 D1。
--
-- 案件に「作業場所（コードのある場所）」を持たせる。実機の事故（2026-09-18）: 委譲された PoC の子タスクが
-- 空のローカル workspace に置かれ、ワーカーが objective の文面からリポジトリの場所を知って自分で
-- `ssh pegasus` し、人のリポジトリへ直接書いた（SPEC §3.7「手元で編集してリモートで検証」に反する）。
--
--   `workspace` — `WorkspaceSpec` の JSON（`{"kind":"local","path":"…"}` /
--                 `{"kind":"remote","cluster":"pegasus","path":"…"}`）。決めていない案件は NULL で
--                 従来どおり（分解した仕事は親の workspace を継ぐ）。

ALTER TABLE projects ADD COLUMN workspace TEXT;
