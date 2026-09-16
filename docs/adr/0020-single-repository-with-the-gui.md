# ADR-0020: taskd と GUI を 1 つのリポジトリにまとめる

- 日付: 2026-09-16
- 状態: Accepted（人間の判断「taskd と taskd-gui が別々のプロジェクトなのは使いづらいので、一つの directory にまとめて git も一つで管理できるようにして下さい」）
- 関連: ADR-0013（API 層と GUI の基盤）、GUI 側 ADR-GUI-0001（境界）、`docs/gui/bootstrap/README.md`、`run-gphases.sh`

## 文脈

GUI は `/home/rmaeda/workspace/taskd-gui` という別リポジトリで進めてきた（ADR-0013 の時点の想定）。実際に回してみて分かったこと:

- API の変更が 2 リポジトリにまたがる。`docs/api/v1/api-v1.schema.json` を taskd で更新 → GUI で `pnpm gen:types` →
  それぞれ別コミット、という手順が毎回要る。片方だけ進んだ状態が普通に起きる（G6 の直前もそうだった）。
- 仕様書の写し（`docs/gui/api.md` → GUI の `docs/taskd-api-v1.md`）が手作業で、ずれる。
- 人間から見ると「taskd を動かす」ために 2 つのディレクトリと 2 つの git を行き来することになる。

一方、**プロセスとしての境界**（GUI は taskd の HTTP API v1 だけを使い、DB にもワークスペースにも触らない。ADR-GUI-0001）は
実際に効いている制約で、これは残したい。

## 決定

### D1. 1 リポジトリ、`gui/` サブディレクトリ

`agent-platform` に `git subtree add --prefix=gui`（squash しない）で取り込む。GUI の 14 コミットはそのまま残る。

- `gui/` は今までどおり独立した pnpm プロジェクト（`gui/package.json`、`gui/CLAUDE.md`、`gui/docs/PROGRESS.md`、`gui/.gitignore`）。
  Rust のワークスペースには入れない（`Cargo.toml` の members は `crates/*` と `tests/e2e` のまま）。
- 元の `/home/rmaeda/workspace/taskd-gui` は `taskd-gui.merged-<日付>` に改名して残す（取り込みを確かめたら人が消す）。

### D2. リポジトリを 1 つにしても、プロセスとネットワークの境界は変えない

GUI は `TASKD_API_URL` の HTTP API v1 だけを使う（ADR-GUI-0001 は有効）。同じリポジトリにあることを利用して
DB を直接読む、`crates/` を import する、といった近道は取らない。`gui/` から taskd のソースに入る依存は作らない。

### D3. 経路の既定値を直す

| 対象 | 前 | 後 |
|---|---|---|
| `run-gphases.sh` の `GUI_REPO` | `/home/rmaeda/workspace/taskd-gui` | `$TASKD_REPO/gui` |
| `run-gphases.sh` の bootstrap | `$GUI_REPO/.git` があれば skip | `$GUI_REPO/package.json` があれば skip（`git init` はしない） |
| `gui/scripts/gen-types.mjs` の `TASKD_REPO` 既定 | `../agent-platform` | `gui/` の親（= リポジトリの根） |
| GUI の作業ルール（`gui/CLAUDE.md`） | `git add -A` | `git add -A .`（`gui/` の外を巻き込まない） |

### D4. API 仕様の写しは script で同期する

taskd が正であるファイルは **`docs/gui/api.md` → `gui/docs/taskd-api-v1.md` の 1 つだけ**。これを
`scripts/sync-gui-docs.sh`（`--check` でずれの検出だけ）で更新し、`run-gphases.sh` が各フェーズの前に実行する。
symlink にしないのは `gui/Dockerfile` の `COPY . .` がビルドコンテキストの外を追えないため。

`gui/docs/DESIGN.md`（立ち上げ時に `docs/gui/DESIGN-GUI.md` から作った）と `gui/docs/adr/*` は**写しではない**。
GUI 側が自分で育てる文書なので同期の対象にしない（実際 G5 の決定が GUI 側にだけ入っている）。
GUI から taskd への提案は今までどおり `gui/docs/taskd-requests.md` と `docs/gui/taskd-proposals.md` で行う。

## 結果

- 人間が見るディレクトリは 1 つ。API を変える変更（schema + 型 + 画面）が 1 コミットに収まる。
- CI / 受け入れの手順は「`cargo test --workspace` と `cd gui && pnpm test`」の 2 本になる（同じリポジトリで）。
- GUI の G フェーズのランナー（`run-gphases.sh`）は今までどおり `gui/` を作業ディレクトリにして回る。
