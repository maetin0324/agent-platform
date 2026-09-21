# ADR-0041: 自己改善ループを回す前に直す 5 つ — 作業ツリーの分離・検証の直列化と件数検査・main への反映・昇格前の差分表示・煙試験

- 日付: 2026-09-19
- 状態: **Accepted**（人間の問い「agent-platform から agent-platform 自身の自己改善ループをさせるにあたって問題となる部分は
  ありますか？」への回答を人が承認: 「お願いします」）
- 関連: ADR-0040（自己改善のデプロイ）、ADR-0019（リモートの `sync = "worktree"`）、ADR-0039（案件の作業場所）、
  ADR-0036（成果物の置き場）、SPEC §3.6（危ない操作は人が握る）

## 1. 文脈

ADR-0040 で本番はリリースから動き、昇格は人が握るようになった。しかし自己改善の案件を実際に回すと、次で止まるか、危ない昇格を
人が見抜けない:

1. 案件の作業場所 `local: ~/workspace/agent-platform` を**全タスクが同じ作業ツリーで共有**する。並列の実装者が別のブランチを
   `checkout` すると互いの未コミット変更を壊す。人（と人の補助エージェント）も同じチェックアウトで作業している。
2. `verify.sh` の件数一致検査（ADR-0040 D3 の検査 2）は、スナップショットの後に**本番 API を数える**。ループ中は本番が常に
   動いているので、件数がずれて偽陰性になる。ポートと `~/taskd/staging/` も固定で、**同時実行すると衝突**する。
3. 実装者は `self/<task-id>` にコミットし人はその sha を昇格できるが、**誰も `main` に戻さない**。次のタスクは古い `main` から
   分岐し、本番と `main` が乖離する。
4. 昇格は**新しいリリースに同梱された `promote.sh`** を実行し、GUI は gate / verify の合否しか見せない。実装者が
   `scripts/selfdeploy/` や引き継ぎの機構（`crates/taskd/src/instance.rs`）を変えたリリースを、人がそれと知らずに昇格しうる。
5. verify モードは**ワーカーを一切動かさない**。アダプタ・前置き・委譲・レビューの回帰は verify を通過して本番に届く。

禁止事項が指示文だけで強制力が無い（ワーカーが同じ Unix ユーザー）ことは最大の構造的な穴だが、工事が大きいので**この ADR では
扱わない**（PROGRESS の提案へ。上の 5 つを入れてから判断する）。

## 2. 決定

### D1. ローカルの作業場所も、タスクごとに `git worktree` を切る（Phase 49）

ADR-0019 がリモートに対して決めたことを、ローカルにも同じ形で持ち込む。

- 対象: タスクの（継承した）作業場所が `kind = local` で、その `path` が git リポジトリ（`git -C <path> rev-parse --git-dir` が通る）
  のとき。案件の `workspace` に **`mode: "worktree" | "shared"`** を足し、既定は **`worktree`**（git でなければ `shared` と同じ）。
  `shared` は従来どおり `path` をそのまま作業ディレクトリにする（自分専用の使い捨てリポジトリ向け）。
- 置き場所: **`<workspace_root>/<task_id>/tree`**（taskd が持つ場所。リポジトリの中に置かない。ADR-0019 D3 と同じ理由で
  元のディレクトリには触れない）。成果物は従来どおり `<workspace_root>/<task_id>/artifacts/`（ADR-0036）。
- ブランチ: `git -C <path> worktree add -b <branch_prefix><task_id> <dir> <base>`。`branch_prefix` は ADR-0019 の設定
  （`[workspace] worktree_branch_prefix`、既定 `taskd/`）をローカルにも使う。**ADR-0040 D5 の `self/<task-id>` はこれに置き換える**
  （実装者は「taskd が用意したブランチ」にコミットする。自分で切らない）。
- **`base` の決め方（決定的）**: リポジトリの `main`（無ければ `HEAD`）。ただし本番の `current` リリースの sha が分かり
  （`~/taskd/current/manifest.json`。taskd は `[selfdeploy] releases_dir` の親の `current` を読む）、かつそれが `main` の
  **子孫**（`main` が本番に追いついていない）なら **`current` の sha を base にする**。本番より古いコードから分岐させない。
  どちらを base にしたかは前置き（ADR-0039 D3 の作業場所の節）に書く: 「作業ツリー `<dir>`、ブランチ `<b>`、base `<sha>`（main /
  current）」。
- taskd はコミットしない（ADR-0019 D2）。run の終端で `git status --porcelain` が**空なら worktree を消す**（コミットはリポジトリに
  残る。ブランチも残す）。空でなければ残し、`WorkerFinished` の後に `WorkerProgress`「未コミットの変更が残っています: <dir>」を
  1 行積む。`taskctl workspace prune`（任意）は今回作らない。
- 前置きの指示文（役割ではなく作業場所の節）に「このブランチにコミットせよ。`main` に直接コミットするな。`git checkout` で
  ブランチを変えるな」を入れる。
- 委譲の子は親の作業場所を継ぐ（ADR-0039 D2）ので、**子ごとに別の worktree**になる。親が子の成果を統合するときは子のブランチを
  merge する（LLM の仕事。前置きに子のブランチ名を `context.children` で渡す）。

### D2. `verify.sh` の件数検査は「同じスナップショットのマイグレーション前後」で比べ、実行は直列化する（Phase 50）

- 検査 2 の目的は「マイグレーションで行が消えたり変わったりしていない」こと。本番 API を数える必要は無い。
  **スナップショット（`.backup` 直後、マイグレーション前）を `sqlite3` で数え**、staging API（マイグレーション後）と比べる。
  対象は従来どおり tasks / projects / milestones / org_nodes / approvals / reports / messages の件数と、`tasks` の `(id, status)` の digest。
  `verify.json.counts` は `{snapshot, staging}` の 2 つになる（`prod` / `prod_after` は消す）。
- **直列化**: `verify.sh` は `flock` で `~/taskd/staging/.lock` を取る。待ち時間の上限 `SD_VERIFY_LOCK_WAIT`（既定 1800 秒）。
  取れなければ exit 75（EX_TEMPFAIL）で「他の検証が走っている」と出す。`release.sh` も `~/taskd/releases/.build/.lock-<sha12>`
  で同じ sha の同時ビルドを 1 つにする（`cargo` の target lock で直列化はされるが、`.build/<sha12>` のワークツリー作成が競合する）。
- N-1 互換（検査 5）はそのまま。

### D3. 昇格したリリースは `main` に反映されているかを見せる。base の規則で乖離を止める（Phase 50）

- `promote.sh` は成功時に `<release>/promoted.json` `{promoted_at, mode, from}` を書く。**リポジトリには触らない**
  （人のチェックアウトを fast-forward しない）。
- `GET /releases` の各 item に **`promoted_at`**（`promoted.json`）と **`on_main: bool`**（`git -C <repo> merge-base --is-ancestor
  <sha> main`。`[selfdeploy] repo` で作業チェックアウトの場所を指定。既定 `~/workspace/agent-platform`。読めなければ `null`）を足す。
  GUI: `current` が `on_main = false` なら「本番は main に未反映: `git merge --ff-only <sha12>`」と出す。
- 反映の作業自体は人が行う（`docs/selfdeploy.md` に手順）。D1 の base の規則により、未反映でも次のタスクは本番の sha から分岐する
  ので乖離はしない。

### D4. 昇格の前に「何が変わるか」を見せ、安全に関わる変更は目立たせる。昇格は `current` 側のスクリプトで（Phase 50 / G15）

- `release.sh` はリリースに **`changes.json`** を書く: `{base: <current の sha | null>, commits: [{sha, subject}] (最大 50),
  files: [path...], sensitive: [path...]}`。`base` はビルド時の `~/taskd/current` の sha。無ければ `commits`/`files` は空。
  `sensitive` は次のパターンに当たる `files`: `scripts/selfdeploy/`, `deploy/`, `crates/taskd/src/instance.rs`,
  `crates/taskd/src/releases.rs`, `crates/task-api/src/releases.rs`, `crates/task-core/migrations/`, `CLAUDE.md`, `gui/CLAUDE.md`,
  `.claude/`, `config/`, `docs/adr/0040-`, `docs/adr/0041-`（一覧は `scripts/selfdeploy/lib.sh` の 1 か所に置く）。
- `GET /releases` の item に `changes: {base, commit_count, file_count, sensitive: [...], commits: [...]}` を足す。`base` が今の
  `current` と違えば `changes.stale = true`。
- GUI「リリース」画面: 行を開くとコミット一覧と変更ファイル数。`sensitive` が空でなければ**赤いバッジ「安全に関わる変更 N 件」**と
  そのパス一覧を最初から開いて出す。その場合の「昇格」は `window.confirm` ではなく **sha12 を入力させる確認**にする。
- `POST /releases/{sha12}/promote` は **`current` のリリースの `scripts/promote.sh`** を使う（`current` に `scripts/` が無い
  Phase 48 以前のリリースなら新しい方を使い、応答に `script_from: "current" | "target"` を出す）。新しいコードの昇格スクリプトは、
  それ自身が一度昇格されてから次の昇格で使われる。

### D5. 検証に煙試験を足す: verify モードで**偽のアダプタだけ**を使って 1 件流す（Phase 51）

- `--mode verify` を拡張する（新しいモードは作らない）: verify の taskd は、**`genre = "smoke"` のタスクだけ**を dispatch する。
  `smoke` の役割・分野は verify モードが**組み込みで足す**（設定ファイルにあっても上書きしない。`[[roles]] id = "smoke"
  adapter = "fake"`、`FakeAdapter` の既定コマンド）。それ以外のタスクは従来どおり一切 dispatch しない。通知・報告の圧縮・途中目標
  レビュー・クラスタ・アカウントも従来どおり動かさない。**`daemon_instances` にも書かない**。
- `verify.sh` の検査 6 `smoke`: staging に `POST /tasks {title: "smoke", genre: "smoke", …}`（管理系。staging のトークン）→
  `accept` → 60 秒以内に `done` になり、`WorkerStarted` / `WorkerFinished` の event があり、`GET /tasks/{id}/report`（あれば）が
  返ることを確かめる。これで dispatch → ワーカー起動 → 結果の取り込み → レビュー → 終端 → 報告の生成、までの回帰を検証が拾う。
- 検査 6 も `verify.json.ok` の条件に入れる。N-1 互換（検査 5）では煙試験をしない（旧バイナリが `smoke` を知らないため）。

## 3. 採らない

- ワーカーを別 Unix ユーザー／サンドボックスで動かす（最大の穴だが今回のスコープ外。PROGRESS の提案へ）。
- `promote.sh` が `main` を fast-forward する（人のチェックアウトを機械が動かさない）。
- 自然文の判断で「安全に関わる変更」を判定する。パスのパターンで決める。
- 煙試験で本物の LLM を呼ぶ。`fake` だけ。

## 4. 受け入れ条件

- **Phase 49（D1）**: `mode` の既定 `worktree`、`<workspace_root>/<task_id>/tree` に worktree、ブランチ `taskd/<task_id>`、
  base の規則（main / current。テストは tempdir の git リポジトリで両方）、終端でクリーンなら worktree を消す・汚れていれば残して
  `WorkerProgress`、前置きの文、子は別 worktree、`shared` は従来どおり。`cargo test --workspace` / clippy。
- **Phase 50（D2–D4）**: 検査 2 がスナップショット前後比較になり本番 API を数えない、`flock`（同時 2 本目が待つ／exit 75）、
  `promoted.json` / `changes.json`、`GET /releases` の `promoted_at` / `on_main` / `changes`、GUI のバッジと sha 入力の確認、
  `promote` が `current` のスクリプトを使う。テストは tempdir の git リポジトリと偽のリリースで。GUI 一式。
- **Phase 51（D5）**: verify モードで `smoke` だけ dispatch される（他の ready は動かない）、`verify.sh` の検査 6、`verify.json.ok` に含む。
  実機: `release.sh main` → `verify.sh` で検査 6 が通る。
- **実機（Phase 49–51 の後）**: 自己改善案件の最初のタスクが worktree で動き、`release.sh`/`verify.sh` を自分で通し、GUI の
  「リリース」画面に差分と `on_main` が出て、人が昇格する。

## 5. Phase 83 追記（2026-09-21）: 検査 4b — GUI の e2e（read-only）

D5 の煙試験は「dispatch → ワーカー起動 → 結果の取り込み → レビュー → 終端 → 報告の生成」という celeris 側の
回帰は拾うが、GUI 側の回帰（画面が壊れて 200 は返すが中身が描画されない・コンソールエラーが出る等）は
検査 3/4 の「200 か JSON か」までしか見ていなかった。`gui/CLAUDE.md` の e2e（`pnpm e2e`、`gui/e2e/*.spec.ts`）は
使い捨ての celeris にタスクを作って承認する結合テストで、ADR-0041 D2/D5 が前提にした「staging は検査 6 の
煙試験だけが書き込む」を壊すので、そのままでは verify.sh に組み込めなかった（ADR-0055 D3 が残した既知のギャップ）。

`verify.sh` に**検査 4b（gui-e2e）**を足す。検査 4 が起こした staging の GUI/celeris に対して、
**読み取り専用**（ナビゲーションと `?tab=` の切り替えだけ。`POST` は一切しない）の
`pnpm e2e:staging`（`gui/scripts/e2e-check.mjs`）を走らせる。**検査 6（煙試験）より前**に置く（検査 6 が
足す 1 件が e2e のナビゲーションに写り込まないように）。`verify.json.ok` の条件は「1〜4・4b・6 が全部真」に
広がる（`live_ok`＝検査 5 の意味は変わらない）。

release の `gui/`（`release.sh` の `pnpm install --prod`）には Playwright が devDependency なので入っていない。
検査 4b は `$SD_REPO/gui`（作業チェックアウトの `pnpm install` 済みの方）があればそちらを使い、無ければ
release の `gui/` を試し、どちらにも `@playwright/test` が無ければ `false — not installed` にする
（クラッシュさせない。ADR-0041 D2 の「本番には触れない」は変えない設計判断: Playwright が無い環境でも
verify.sh 自体は最後まで走り切り、原因が分かる形で false になる）。詳細は `docs/selfdeploy.md` の
「検査 4b」節、受け入れ条件は `docs/PROGRESS.md` の Phase 83。
