# ADR-0051: 部署のレビューからマージ・デプロイ準備へつなぐ

2026-09-20。ユーザーの方針: レビューはその部署をまとめる担当が行い、同じレビューでマージ可否まで判断する。上司・CoSの重複レビューは設けない。ADR-0040/0041/0043 の「マージは人だけ」を明示的に有効化した自己改善案件について更新する。

- 既存の独立したReviewer runへ担当部署のノード、継承profile、review.tierを渡す。実装者の会話や自己申告だけで判断せず、成果物・条件・証拠を検証する。部署がない仕事は従来の独立レビュアーを使う。
- `[selfdeploy] delivery_projects` で許可した案件の自己リポジトリ1件について、レビュー開始時の既定ブランチと対象SHAを固定し、同じrunの暗黙のReviewer条件にマージ可否を追加する。通常条件も全て合格し、元タスクがdoneになって初めて取り込む。
- SQLite delivery記録に判定run、部署、SHA、状態、理由を保持する。既存ReviewVerdictイベントを監査証跡として残す。過去のdoneを承認済みと推定しない。必要な場合は管理API `POST /tasks/{id}/rereview` で done→reviewing に戻し、実装runを増やさず同じレビュアー経路で判定する。
- 自動取り込みは登録した自己リポジトリのfast-forwardのみ。承認後にSHAが変われば停止し、勝手にrebaseしない。gitが保持できる無関係な未コミット変更は保存し、重なる変更があれば停止する。pushしない。
- 合格後に固定SHAを取り込み、既存release/verifyをバックグラウンドで実行する。検証成功後だけGUIのデプロイ候補として通知する。promoteは呼ばない。
- 技術的な差し戻しは部署内に戻す。レビュー不合格は既存の修正・再レビュー経路を使う。CoSにはデプロイ操作待ち、またはレビュー理由の先頭に `[needs-human]` を明示したユーザーへの確認事項だけを渡す。delivery対象の実装詳細を定期集約でCoSへ再送しない。
- 再起動を跨ぐ外部操作は重複実行しない。準備完了の結果とSHAを照合し、確認できない中断は部署に理由付きで戻す。無効化は対象案件リストを空にしてreloadする。
- ライブ切替時も同じタスクのレビューは1本だけ。drainingのディスパッチャは実装完了後に新しいレビューを始めず、activeへ渡す。受入コマンドから判定の保存までタスク単位のファイルロックを持ち、async側の保持分は完了通知前に解放する。
- リリースはSHAごとのロックに加えて、共有Cargo出力の検査・梱包全体を直列化する。自前workspaceパッケージの出力を先に掃除し、異なる版のrlib/rmetaや古いdep-infoを混ぜない。外部依存のキャッシュは保持する。
- 自己リポジトリ以外、複数リポジトリ、Reviewer条件なしの仕事の取り込みは既存の手動経路を維持する。

## Phase 106 追記（2026-09-23）: mergeが成功したら release を作る前に origin へ push する

### 背景（本番、2026-09-22）

`merge_reviewed` が `selfdeploy.repo`（人の作業チェックアウト）の main へ merge した後、`release.sh` /
`verify.sh` を回して人に昇格を促すが、**push はしなかった**。そのため Phase 104（`4fff25348ed6`）は
本番に昇格された後もローカル main にしか無く、origin/main は 2 コミット遅れていた（親が手で
`git push origin main` した）。人の指示: 「release を作る前に push する」。

### 決定

- `merge_reviewed` が main への merge に成功した直後、`release.sh`（`start_prepare`）を起こす前に
  **`git push <remote> <branch>`** を行う。`[selfdeploy] push`（既定 `true`）と `push_remote`
  （既定 `"origin"`）で制御する。branch は merge 先（`Delivery.default_branch`。通常 `main`）。
- push は非対話（`GIT_TERMINAL_PROMPT=0` は `task_ops::changes::git` が既定で設定。
  `GIT_SSH_COMMAND` に `-o BatchMode=yes` を足す）。タイムアウト 120 秒。
- push の失敗は release の準備を止めない。`Delivery` に `pushed_at: Option<OffsetDateTime>` と
  `push_error: Option<String>`（stderr 末尾 500 バイト）を持たせ、`advance` が出す通知（「release の
  準備ができた」系のメッセージ）に「origin へ push 済み: `<sha>`」または「**push に失敗**:
  `<理由>`。人が `git push` してください」を 1 行足す。push 失敗は 1 度だけ再試行する（`push_error`
  に内部の目印 `[retried] ` を前置して「既に再試行済みでなお失敗」を覚え、以後は触らない。通知文には
  この目印を外して見せる）。
- 既に origin が同じかそれより先（`git rev-list --count <remote>/<branch>..<branch>` が 0）なら push
  を省略し `pushed_at` を今にする（`push_error` 無し）。
- **採らない**: `release.sh` 自体に push を入れる（release.sh は任意の ref から不変のリリースを作る
  道具で、リポジトリの状態を変えない。ADR-0040 D2）。force push。

### 実装からの逸脱（設計は変えていない）

- **migration を追加していない**。`Delivery` は `deliveries.json`（1 カラムの JSON blob。migration
  `0020_deliveries.sql`）にそのまま直列化されるだけで、専用の SQLite カラムを持たない。新フィールド
  2 つは `#[serde(default)]` なので、旧い JSON 行（`pushed_at`/`push_error` を持たない）も
  読める。`Delivery` に `#[serde(deny_unknown_fields)]` は付いていないため、旧バイナリが新しい JSON
  （2 フィールド増）を読んでも無視できる。したがって `SCHEMA_VERSION` は 25 のまま。
- `task_ops::changes` に `git_with_env`（`git` に追加の環境変数を渡す版）を足した。`push` だけ
  `GIT_SSH_COMMAND` を足す必要があり、既存の `git`（`GIT_TERMINAL_PROMPT=0` 等の固定環境だけを持つ）
  では表現できなかったため。`git` 自体は `git_with_env(dir, args, &[], timeout)` に委譲するだけで
  挙動は変えていない。

詳細・受け入れ条件は `docs/PROGRESS.md` の Phase 106 節、実装は `crates/celeris/src/delivery.rs`
（`push_merged` / `maybe_retry_push` / `push_error_display`）、`crates/task-core/src/delivery.rs`
（`Delivery::pushed_at` / `push_error`）、`crates/celeris/src/config.rs`
（`SelfdeployConfig::push` / `push_remote`）、`crates/task-ops/src/changes.rs`（`git_with_env`）。
