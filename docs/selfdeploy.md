# 自己改善のデプロイ — 運用手順（ADR-0040）

`agent-platform` 自身を taskd の上で改善し、**動いている本番を壊さずに**新しい版へ移るための道具。
設計は `docs/adr/0040-self-improvement-deploy.md`（D1〜D5）。ここはその**使い方**だけを書く。

## 0. 全体像

```
release.sh <ref>  →  verify.sh <sha12>  →  promote.sh <sha12>        （戻すとき: rollback.sh）
  本番に触れない      本番に触れない        人だけが実行する           人だけが実行する
  誰が実行してもよい   誰が実行してもよい
```

```
~/taskd/
  taskd.toml            本番の設定（**誰も書き換えない**。Phase 46 で変わるところは無い）
  taskd.sqlite3         本番の DB（`sqlite3 .backup` と `mode=ro` で読むだけ）
  api.token             本番 API のトークン
  current -> releases/<sha12>     いま動いている版
  previous -> releases/<sha12>    直前の版（rollback 先）
  releases/<sha12>/     bin/{taskd,taskctl}  gui/  manifest.json  gate.json  verify.json
                        scripts/          selfdeploy 一式の写し（ADR-0040 D6。昇格に作業チェックアウトが要らない）
                        promote.log       この API 経由の昇格の出力（§4c）
                        promote.lock      昇格中の pid
  releases/.build/      release.sh が生やす detached worktree（成功したら消える）
  releases/.cargo-target/  CARGO_TARGET_DIR（リリース間で共有。ビルドを速くするだけ）
  staging/              verify.sh の作業場所（毎回作り直す）
  backups/              昇格前の DB のコピーと promote-<ts>.log
```

ポート:

| | 本番 | staging（verify.sh） |
|---|---|---|
| taskd API | `127.0.0.1:7710` | `127.0.0.1:7711`（N-1 は `:7712`） |
| GUI | `0.0.0.0:7700` | `127.0.0.1:7701` |

## 1. 最初に一度だけ: systemd の unit を入れる

テンプレート unit はリポジトリの `deploy/systemd/` にある。**人が一度だけ**入れる:

```bash
bash ~/workspace/agent-platform/scripts/selfdeploy/install-units.sh
# = deploy/systemd/taskd@.service と taskd-gui@.service を ~/.config/systemd/user/ に置いて daemon-reload
systemctl --user cat taskd@.service        # 入ったことの確認
loginctl show-user "$USER" | grep Linger   # Linger=yes であること（ログアウトしても動き続ける）
```

`%i` はリリースの `sha12`。`taskd@<sha12>` は `~/taskd/releases/<sha12>/bin/taskd --config ~/taskd/taskd.toml
--release <sha12>`、`taskd-gui@<sha12>` は `~/taskd/releases/<sha12>/gui/` で `node server.js` を動かす。
GUI の環境変数は今の本番（手で `node server.js` を起こしていたもの）と同じ。

`--release` フラグと `--mode` / `--db` / `--listen` / `--workspace-root` / `--token-file` は **Phase 47** で
taskd 本体に入る。Phase 47 より前のバイナリで作ったリリースは `taskd@<sha12>` として起動できない
（unit が `--release` を渡すため）。初回の移行は Phase 47 が入った sha を昇格すること。

## 2. リリースを作る（`release.sh`）

```bash
scripts/selfdeploy/release.sh HEAD          # または ブランチ名 / タグ / sha
scripts/selfdeploy/release.sh taskd/01M2XXX # 自己改善の案件の実装ブランチ（taskd が切る。ADR-0041 D1）
```

- `~/taskd/releases/.build/<sha12>` に **detached worktree** を生やして、そこでだけビルドする。
  作業チェックアウト（`~/workspace/agent-platform`）が汚れていても、その中身は使われない。
- gate（この順。1 つでも非 0 ならリリースを作らない）:
  `cargo test --workspace` → `cargo clippy --workspace -- -D warnings` → `cargo build --release -p taskd -p taskctl`
  → GUI `pnpm install --frozen-lockfile` → `pnpm typecheck` → `pnpm test` → `pnpm build`
- 成功したら `~/taskd/releases/<sha12>/` に `bin/`（taskd, taskctl）、`gui/`（build/ server.js package.json
  pnpm-lock.yaml pnpm-workspace.yaml と `pnpm install --prod` の node_modules）、`manifest.json`、`gate.json`、
  `gate-logs/`、`scripts/` を置き、ビルド用の worktree を消す。
- `scripts/` は **その sha の `scripts/selfdeploy/*.sh` をそのまま写したもの**（実行ビットごと。ADR-0040 D6、
  Phase 48）。`POST /releases/{sha12}/promote` はこの `<release>/scripts/promote.sh` を起こすので、
  **作業チェックアウトが別のブランチにいても、無くても昇格できる**。`lib.sh` は
  `dirname "${BASH_SOURCE[0]}"` で自分の隣を読むだけで、場所はすべて `TASKD_HOME` 基準なので
  リリースの中から source しても動く（`SD_REPO` を要るのは `release.sh` の worktree 操作だけ）。
- 失敗したら**リリースディレクトリは作らず**、`~/taskd/releases/.build/<sha12>/gate.json` と
  `.gate-<step>.log` を残す（次に同じ sha で `release.sh` を回すと消える）。
- 掃除: `current` / `previous` / いま作った版 / 検証済み（`verify.json.ok`）の新しい 3 件だけを残す。

`manifest.json`:

```json
{"sha": "...", "sha12": "...", "ref": "HEAD", "built_at": "...", "profile": "release",
 "schema_version": 10, "taskd_version": "0.1.0", "gui_version": "0.1.0", "gate_ok": true}
```

`schema_version` は その sha の `crates/task-core/src/store.rs` の `pub const SCHEMA_VERSION` を読んだもの。

## 3. 検証する（`verify.sh`）

```bash
scripts/selfdeploy/verify.sh <sha12>
scripts/selfdeploy/verify.sh --dry-run <sha12>   # 前提だけ確かめる（何も起こさない）
```

やること（ADR-0040 D3。本番には触れない）:

1. `~/taskd/staging/` を作り直し、`sqlite3 "file:~/taskd/taskd.sqlite3?mode=ro" ".backup staging.sqlite3"`。
2. 新リリースの taskd を **verify モード**で `127.0.0.1:7711` に起こす
   （`--mode verify --db <snapshot> --listen … --workspace-root … --token-file … --release <sha12>`。
   設定は**本番の `taskd.toml` をそのまま読む**。上書きは CLI だけ）。
3. 検査:
   1. 起動し、`health.schema_version` が新バイナリの `SCHEMA_VERSION` と等しい（＝本番のデータで
      マイグレーションが通った）
   2. **件数一致**: 本番 API（`:7710`、GET だけ）と staging API で
      `tasks` / `projects` / `milestones` / `org` / `approvals(pending=false)` / `reports` / `messages` の件数と、
      `tasks` の `{id,status}` の集合（sha256 の先頭 16 桁）が一致する
   3. 主要 GET が 200 かつ JSON（`inbox` / `org/<最初のノード>/memory` / `notify` / `clusters` / `providers` / `config`）
   4. 新リリースの GUI を `127.0.0.1:7701` に起こして `/healthz`（`release` が新しい sha12）と
      `/`, `/org`, `/projects`, `/projects/<最新>`, `/approvals`, `/reports`, `/clusters` が 200
   5. **N-1 互換**: `~/taskd/current/bin/taskd`（旧）を、**新バイナリがマイグレーションした後の**同じ
      スナップショットに対して `:7712` で起こし、1〜3 と同じ検査。落ちたら `live_ok = false`
      （`current` が無い初回も `live_ok = false`）
4. `~/taskd/releases/<sha12>/verify.json` を書く。`ok` は **1〜4 が全部真**のとき。`live_ok` は 5。
5. 起こしたプロセスは `trap` で必ず止める（自分が起こした pid だけ）。

### 件数一致が落ちたとき

本番は検証の間も動いている。タスクの status が動けば当然ずれる。`verify.json` の `counts` には
`prod`（スナップショットの直後）・`staging`・`prod_after`（検査の後）の 3 つが入っているので、
`prod` と `prod_after` が違っていれば**本番が動いていたせい**。本番が静かなときに `verify.sh` を回し直す。

### staging の見方

```
~/taskd/staging/
  staging.sqlite3        本番 DB のコピー（新バイナリがマイグレーション済み）
  api.token              staging だけのトークン（毎回作り直す）
  workspaces/            verify モードの作業場所（何も動かないので空のまま）
  logs/taskd-new.log     新リリースの taskd の出力 ← 起動に失敗したらまずここ
  logs/taskd-old.log     N-1（旧バイナリ）の出力 ← `SchemaTooNew` はここに出る
  logs/gui.log           staging GUI の出力
  logs/counts-*.json     件数の生データ（prod / staging / prod-after / n1）
  logs/health-*.json     health の生データ
```

`verify.sh` は次に走ったときに `staging/` を**丸ごと作り直す**。残したいログは先にコピーすること。

## 4. 昇格する（`promote.sh`。人だけ）

```bash
scripts/selfdeploy/status.sh | less        # verify.json.ok と live_ok を見る
scripts/selfdeploy/promote.sh <sha12>
```

- `verify.json.ok` が真でなければ**拒否する**（`--force` は無い）。
- ログは `~/taskd/backups/promote-<ts>.log`。DB のコピーは `~/taskd/backups/<ts>-pre-<sha12>.sqlite3`。

### 4a. 初回の移行（停止 → 起動）

いまの本番は `~/workspace/agent-platform/target/debug/taskd` を手で起こしたもので、`daemon_instances` も
`/health` の `role` も知らない。`promote.sh` はそれを見て**停止 → 起動**を選ぶ（`live_ok` も偽）。

1. 旧 taskd の pid を**設定パスまで含めた完全一致**で探す（`argv[0]` の basename が `taskd` で、
   `--config ~/taskd/taskd.toml` を持ち、`--mode` / `--db` / `--listen` 等を持たないもの。
   `grep` や `bash -c` や staging のプロセスは当たらない）→ `SIGTERM` → `kill_grace_secs + 10` 秒待つ
2. 旧が終わってから DB をバックアップ（引き継ぎ中の仕事もコピーに入る）
3. `systemctl --user start taskd@<sha12>` → `/health` が 200 かつ `schema_version` が期待どおりになるまで 60 秒待つ
4. 駄目なら新を止め、旧 unit があれば起こし直し、**DB は戻さずに**失敗を報告する
   （スキーマが進んでいるかもしれないので、戻すかどうかは人が `rollback.sh --restore-db` で選ぶ）
5. `:7700` で LISTEN している旧 GUI（`node server.js`）を `ss -ltnp` で見つけて `SIGTERM` →
   `systemctl --user start taskd-gui@<sha12>` → `/healthz` が 200 になるまで待つ
6. `enable` 新 / `disable` 旧、`current` / `previous` の symlink を更新

**この間だけ API と GUI が止まる**（数十秒）。走っていたワーカーの run はリースが切れて新しい daemon が拾う。

### 4b. 2 回目から（ライブ引き継ぎ）

`verify.json.live_ok` が真で、動いている taskd の `/health` が `role` を持っているとき:

1. DB をバックアップ
2. `systemctl --user start taskd@<新>` → 新は `standby` で起き、同じポートに `SO_REUSEPORT` で bind する
3. `/health` を**毎秒 5 回ずつ**見て、5 回とも `release == <新>` かつ `role == active` になるまで 60 秒待つ
   （新旧が同じポートを共有するので 1 回の応答では判らない。5 回とも新なら旧はもう listener を閉じている）
   - ならなければ `systemctl --user stop taskd@<新>` して**失敗**。旧はそのまま動き続ける
4. `enable` 新 / `disable` 旧
5. `systemctl --user start taskd-gui@<新>` → `:7700/healthz` の `release` が新になるまで待つ → `stop taskd-gui@<旧>`
6. `current` / `previous` を更新

**旧 taskd の drain 完了は待たない**。旧は `draining` になって自分が起こした run だけ面倒を見続け、
手元の run が 0 になったら exit 0 する（最長 `[handoff] drain_timeout_secs`）。進み具合は `status.sh` の
`daemon_instances` で見える。API が一瞬 `503 {"detail":"standby"}` を返す管理系（`reload` / `check` /
クラスタ接続 / アカウントのログイン / `notify/test`）があるが、窓は 1〜2 tick。

### 4c. GUI から昇格する（Phase 48 / G14。ADR-0040 D6）

GUI「リリース」画面（`/releases`）の「昇格」ボタンは `POST /releases/{sha12}/promote`（管理系）を叩き、
taskd が **`~/taskd/releases/<sha12>/scripts/promote.sh <sha12>`** を detached（`setsid`、stdin は
`/dev/null`、stdout/err は `<release>/promote.log`）で起こす。やることは shell から `promote.sh` を
叩くのと**同じ**で、押すのは人（ADR-0040 D5。taskd の中に自動で呼ぶ経路は無い）。

```bash
# shell からと同じことを API で
curl -sS -X POST -H "Authorization: Bearer $(cat ~/taskd/api.token)" \
     http://127.0.0.1:7710/api/v1/releases/<sha12>/promote
# → 202 {"sha12":"…","log":"~/taskd/releases/<sha12>/promote.log","started_at":"…"}
```

- 断られるとき（409 `release_not_promotable`）: `verify.json` が無い／`ok` でない、既に `current`、
  既に昇格中（`promote.lock` の pid が生きている）、`scripts/promote.sh` が無い（Phase 48 より前の
  リリース — その場合は shell から `scripts/selfdeploy/promote.sh` を使う）。
- ログは `<release>/promote.log`（**API では中身を出さない**）と、`promote.sh` 自身が書く
  `~/taskd/backups/promote-<ts>.log`。進行は `GET /releases` の `instances`（旧 `draining` / 新 `active`）
  と `status.sh` で見る。
- **202 を返した taskd 自身がその昇格で `draining` になって終わる**（ライブ引き継ぎ）。API が一瞬
  切り替わるのは正常（新旧が `SO_REUSEPORT` で同じポートを共有する）。

`~/taskd/taskd.toml` に足す設定は無い（`[selfdeploy] releases_dir` の既定が `releases` なので、
`~/taskd/releases` をそのまま見る）。

## 5. 戻す（`rollback.sh`。人だけ）

```bash
scripts/selfdeploy/rollback.sh              # previous を昇格し直す
scripts/selfdeploy/rollback.sh --restore-db # DB も昇格前に書き戻す（昇格後の仕事は失われる）
```

- 旧バイナリの `SCHEMA_VERSION`（`previous/manifest.json`）が DB の `schema_version`（`/health`、
  取れなければ `sqlite3 "select max(version) from schema_migrations"`）**以上**なら、そのまま
  `promote.sh previous` と同じことをする（live か停止→起動かは `previous` の `verify.json.live_ok` 次第）。
- 旧の方が低い（`SchemaTooNew` で起動できない）ときは**拒否する**。`--restore-db` を付けたときだけ:
  1. `~/taskd/backups/` の直近の `*-pre-*.sqlite3` を選ぶ（`*-pre-rollback.sqlite3` は選ばない）
  2. いまの DB を `<ts>-pre-rollback.sqlite3` に退避
  3. taskd（unit か、完全一致で見つけた pid）と GUI を止める
  4. 選んだコピーを `taskd.sqlite3` に上書きし、`-wal` / `-shm` を消す
  5. `taskd@<previous>` / `taskd-gui@<previous>` を起こす → symlink を更新

**書き戻すと、昇格してから今までに進んだ仕事は消える。** どちらが損かを人が決める。

## 6. いまを見る（`status.sh`）

```bash
scripts/selfdeploy/status.sh
```

JSON 1 つ。`current` / `previous`、`releases[]`（`gate`（各段の exit と秒数）、`verify`（`ok` / `live_ok` /
落ちた検査の名前））、本番 `:7710` の `/health`、`:7700` の `/healthz`、`daemon_instances`（Phase 47 で
表ができるまでは `null`）、`backups` の新しい 10 件。**読むだけ**なので誰が実行してもよい。

## 7. 禁止（ADR-0040 D5）

taskd の上の「人」（ワーカー）が自己改善の案件でやってよいのは **`release.sh` と `verify.sh` まで**。

やってはいけないこと:

- `promote.sh` / `rollback.sh` / `install-units.sh` を実行する（リリースの中の `<release>/scripts/*.sh` も同じ）
- `POST /releases/{sha12}/promote` を叩く（GUI の「昇格」ボタンと同じもの。押すのは人だけ）
- `systemctl` を叩く（本番の unit を start / stop / restart / enable / disable する）
- `~/taskd/taskd.toml` を編集する
- `~/taskd/*.sqlite3` に書き込む（読むのは `sqlite3 "file:…?mode=ro"` と `.backup` だけ）
- 本番のプロセスに `kill` などのシグナルを送る
- `127.0.0.1:7710` / `0.0.0.0:7700` に bind する
- `main` に直接コミットする（実装者は **taskd が用意した worktree とブランチ `taskd/<task-id>`** にコミットする。ADR-0041 D1）
- `git checkout` で作業ツリーのブランチを変える・自分でブランチを切る（作業ツリーは taskd がタスクごとに用意する）

実装者は `gate.json` / `verify.json` を `artifacts/` に写し、報告に「検証済み sha」を書く。
昇格は人が `status.sh` で `verify.json.ok` を見てから行う。

## 8. taskd.toml について

**Phase 46 / Phase 48 とも `~/taskd/taskd.toml` に変えるところは無い。** Phase 48 で入った
`[selfdeploy] releases_dir` は既定が `releases`（設定ファイルのディレクトリ基準）なので、書かなければ
`~/taskd/releases` を見る。別の場所に置きたいときだけ書く。 `[handoff]`（`drain_timeout_secs` など）は
Phase 47 で taskd 本体に入るときに足す設定で、それまでは既定値（`drain_timeout_secs = 3600`）で動く。
`promote.sh` が読むのは既存の `kill_grace_secs` だけ（無ければ 10 秒）。
`verify.sh` は `taskd.toml` を**そのまま**新リリースに読ませる（本番の設定が新しいバイナリで通るかを
見るのが目的なので、上書きは CLI フラグだけ）。
