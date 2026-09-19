# ADR-0040: 自己改善のためのデプロイ — リリース → 検証 → 昇格（ライブ引き継ぎ）

- 日付: 2026-09-19
- 状態: **Accepted**（人間の依頼「agent-platform の改善を taskd 上で動かせるようにしたい。自己コードベースを改善した後に、
  既存の動いているコードから新しいコードがちゃんと正しく使えることを確認次第 migration するイメージで、安全に自己改善
  できるデプロイ環境を整備してください」「ライブマイグレーションが出来るとなお良いです」）
- 関連: SPEC §3.6（認可。危ない操作は人が握る）/ §5（成果物は `~/workspace/...`）、ADR-0013 D5（`SchemaTooNew`）、
  ADR-0010（リース）、ADR-0017（reload）、ADR-0032 D2（ssh master は ControlPersist で taskd の外で生きる）、
  ADR-0039（案件の作業場所）

## 1. 文脈

いま本番は `~/workspace/agent-platform` の `target/debug/taskd` を手で `kill` → `nohup` で起こしている。GUI も同じ手順。
taskd 上の「人」に agent-platform 自身を改善させるには、(a) 本番が**作業中のチェックアウトに依存しない**こと、
(b) 新しいコードが**本番のデータと設定で本当に動く**ことを昇格の前に機械的に確かめること、(c) 昇格が**動いている
仕事を殺さず**、失敗したら戻せること、(d) 昇格そのものは**人が握る**こと、が要る。

## 2. 決定

### D1. リリースは不変のディレクトリ。本番は `~/taskd/current` が指すリリースから動く

```
~/taskd/releases/<sha12>/
  manifest.json        # {sha, ref, built_at, schema_version, profile, gate: {…exit codes/durations}}
  bin/taskd  bin/taskctl
  gui/                 # build/ server.js package.json pnpm-lock.yaml pnpm-workspace.yaml node_modules(prod)
  gate.json            # release.sh の結果（cargo test / clippy / build / pnpm typecheck / test / build）
  verify.json          # verify.sh の結果（D3）。無ければ未検証
~/taskd/current -> releases/<sha12>   # 昇格済み（symlink。人が読む用。systemd は unit 名で判る）
~/taskd/previous -> releases/<sha12>  # 直前の昇格（rollback 先）
~/taskd/backups/<ts>-pre-<sha12>.sqlite3   # 昇格直前の DB（`sqlite3 .backup`）
~/taskd/staging/      # verify の作業場所（毎回作り直す。DB のスナップショット、staging 用トークン、ログ）
```

- ビルドは `git worktree add --detach` した**作業チェックアウトとは別の**ワークツリーで行い（`~/taskd/releases/.build/<sha12>`）、
  `CARGO_TARGET_DIR=~/taskd/releases/.cargo-target`（共有。ビルドを速くする）。プロファイルは `release`。
  ビルド後にワークツリーは消す。リリースは `current` / `previous` と、検証済みの新しい 3 件を残して消す。
- **本番はもう `~/workspace/agent-platform/target` を使わない**。作業チェックアウトで何をしても本番は変わらない。

### D2. 3 段のパイプライン（`scripts/selfdeploy/`。bash、`set -euo pipefail`、決定的、LLM 不在）

| 段 | 何をする | 誰が実行できる |
|---|---|---|
| `release.sh <git-ref>` | ワークツリー → `cargo test --workspace` → `cargo clippy -D warnings` → `cargo build --release` → GUI `pnpm install --frozen-lockfile` → `typecheck` → `test` → `build` → `node_modules` を prod だけ入れ直す → `manifest.json` / `gate.json`。**どれか 1 つでも非 0 ならリリースを作らない**（`.build` に `gate.json` だけ残す） | 人、taskd の「人」（ワーカー）どちらでも。本番に触れない |
| `verify.sh <sha12>` | D3。本番 DB のスナップショットに対して新リリースを **verify モード**で起こし、API と GUI を叩き、**旧バイナリとの N-1 互換**も確かめて `verify.json` を書く | 同上。本番に触れない（DB は `sqlite3 .backup` で読むだけ） |
| `promote.sh <sha12>` | D4。`verify.json` が `ok` でなければ拒否（`--force` は無い）。DB をバックアップし、**ライブ引き継ぎ**（`verify.json.live_ok` が真）か **停止→起動**（偽）で切り替える。`current` / `previous` を更新 | **人だけ**（SPEC §3.6。認可の standing で機械に渡すのは今回やらない） |
| `rollback.sh` | `previous` へ `promote`。スキーマが新しくなっていて旧バイナリが読めない（`SchemaTooNew`）ときは、`--restore-db` を付けたときだけ `backups/` から復元して停止→起動（復元は昇格以降の仕事を失う。人が選ぶ） | 人だけ |
| `status.sh` | いまの `current` / `previous`、`daemon_instances`（D4）、リリース一覧と検証状態を JSON で | 誰でも |

### D3. 検証（staging）は「本番のデータと設定」で、しかし何も動かさずに行う

- `~/taskd/staging/` を作り直し、`sqlite3 ~/taskd/taskd.sqlite3 ".backup staging.sqlite3"`（WAL でも整合するコピー）。
- 新リリースの `bin/taskd` を **`--mode verify`** と上書きフラグで起こす:
  `taskd --config ~/taskd/taskd.toml --mode verify --db <staging.sqlite3> --listen 127.0.0.1:7711 --workspace-root <staging>/workspaces --token-file <staging>/api.token`。
  設定は本番のものをそのまま読み（役割・分野・プロバイダ・クラスタ・秘密の**設定**が通るかを見るため）、**上書きは CLI だけ**。
- **verify モード**（D4 の役割 `verify`）: マイグレーションは適用する（コピーに対して）。**dispatch しない、ワーカーを起こさない、
  tick の裏方（通知・報告の圧縮・途中目標レビュー・クラスタ接続・アカウント確認）を動かさない、Discord に送らない**。
  API は読み書きとも受ける（ここでの書き込みはコピーに対して）。`GET /health` に `mode` と `release` を載せる。
- 検査（すべて決定的。結果は `verify.json` に個別に記録）:
  1. 起動できて `health.schema_version` が新バイナリの `SCHEMA_VERSION` に等しい（マイグレーションが本番のデータで通った）。
  2. **件数一致**: 本番 API（`127.0.0.1:7710`、読み取り）と staging API で `tasks`（`show_support=1`）/ `projects` / `milestones` /
     `org` / `approvals(pending=false)` / `reports` / `messages` の件数と、各 `tasks` の `id,status` の集合が一致する。
  3. 主要 GET が 200 かつ JSON として読める（`inbox`, `org/secretary/memory`, `notify`, `clusters`, `providers`, `roles`, `genres`）。
  4. 新リリースの GUI を `TASKD_API_URL=http://127.0.0.1:7711 TASKD_GUI_BIND=127.0.0.1:7701`（loopback なのでパスワード無し）で起こし、
     `/`, `/org`, `/projects`, `/projects/<最新の案件>`, `/approvals`, `/reports`, `/clusters` が 200。
  5. **N-1 互換**: `current` の `bin/taskd`（旧）を、**新バイナリがマイグレーションした後の**同じスナップショットに対して
     `--mode verify --listen 127.0.0.1:7712` で起こし、1〜3 と同じ検査をする。旧が `SchemaTooNew` で起動できない、
     または検査に落ちるなら **`live_ok = false`**（ライブ引き継ぎ中は新旧が同じ DB を同時に使うため、旧が新スキーマを
     読めないなら安全に引き継げない。停止→起動で昇格する）。`current` が無い（初回）なら `live_ok = false`。
- staging のプロセスは検査の最後に必ず止める（`trap`）。`verify.json` の `ok` は 1〜4 の全部が真のとき。

### D4. 昇格はライブ引き継ぎ（handoff）。taskd の「インスタンスの役割」を DB に持つ

migration `0011_daemon_instances.sql`（`SCHEMA_VERSION = 11`）:

```
daemon_instances(instance_id TEXT PK, release TEXT NOT NULL, pid INTEGER NOT NULL, role TEXT NOT NULL
  CHECK(role IN ('active','standby','draining','verify')), started_at TEXT NOT NULL, heartbeat_at TEXT NOT NULL,
  handoff_requested_at TEXT NULL, drained_at TEXT NULL)
```

役割と規則（すべて tick の中で決定的に。LLM 不在）:

- **`active`**: dispatch する。tick の裏方（通知・報告・途中目標レビュー・クラスタ・アカウント）を動かす。API を受ける。
  **常に 1 つだけ**。
- **`standby`**: 起動時に、`heartbeat_at` が新しい（`now - heartbeat_at < 3 × tick + lease_grace`）`active` があれば
  standby になる。API は**起きてすぐ**受ける（同じポートに `SO_REUSEPORT` で bind。カーネルが振り分ける。読み書きは
  同じ DB なので問題ない）。ただし**ディスパッチャの状態を要する管理 API**（`reload`、`check`、クラスタ接続、
  アカウントのログイン中継、`notify/test`）は `503 {"detail":"standby"}` に `Retry-After: 2` を付けて返す
  （窓は 1〜2 tick。GUI はその間だけ「切り替え中」）。`active` の行に `handoff_requested_at` を書く。
- `active` は `handoff_requested_at` を見たら **`draining`** になる: **同じ tick で** API の listener を閉じ（受け付け済みの
  要求は完了させる）、dispatch と裏方を止める。**自分が起こしたワーカー run とレビューはそのまま面倒を見続ける**
  （リースの更新、終了の記録、`aggregate` / `child_failed` などの後処理）。手元の run が 0 になったら `drained_at` を
  書いて **exit 0**。`[handoff] drain_timeout_secs`（既定 3600）を超えたら残りの run を abort し（リースが切れて
  新しい active が拾う。従来の「リース切れ」と同じ経路）、exit 0。
- `standby` は `active` の行が `draining` になった（または `heartbeat_at` が古い＝旧が死んだ）のを見たら **`active`** になる。
  古い `active`/`draining` の行は、`drained_at` が付くか heartbeat が古くなったら消す。
- **`verify`**: D3 のためのモード。dispatch も裏方も無し。`daemon_instances` に行は**書かない**（本番の表に触れない —
  そもそも別 DB だが、規約として）。`--mode verify` のときだけ。
- 起動時に自分と同じ `release` の `active` がいたら**何もせず exit 3**（同じ版を二重に起こさない）。
- リースは従来どおり `renew_lease` で守られる。draining の run は新 active からは「リースが生きている running」に見えるので
  二重 dispatch は起きない。`max_concurrency` は各インスタンスの手元の数で数える（ADR-0010 のまま）。引き継ぎ中は
  合計が一時的に超えうる（上限は旧の残り run 数）。**採らない**: DB で合計を数える（今回のスコープ外。PROGRESS の提案へ）。
- ssh master（ADR-0032 D2）は ControlPersist で taskd の外で生きる。新 active は起動時に `-O check` で拾う（従来どおり）。
- GUI も同じ形: `server.js` の `listen` に `reusePort: true`（Node 24）。`GET /healthz` が `{release}` を返す。
  新 GUI を起こして `/healthz` が新 `release` を返したら旧を止める（数百 ms の並走。セッションは秘密ファイルで共有済み）。

systemd（user。linger 済み）: テンプレート unit `~/.config/systemd/user/taskd@.service` と `taskd-gui@.service`。
`%i` = `sha12`。`ExecStart=%h/taskd/releases/%i/bin/taskd --config %h/taskd/taskd.toml`。`Restart=on-failure`
（drain 後の exit 0 では再起動しない）。`promote.sh` は `systemctl --user start taskd@<new>` → `daemon_instances` で
新が `active`・旧が `draining` になるまで待つ（上限 60 秒。ならなければ新を止めて**失敗**。旧はそのまま）→
`enable taskd@<new>` / `disable taskd@<old>`（起動時に新だけが上がる）→ GUI を同様に → `current`/`previous` を更新。
**旧の drain 完了は待たない**（数時間かかりうる。`status.sh` で見える）。

停止→起動（`live_ok = false`、または旧が `daemon_instances` を知らない＝この ADR 以前の版。**初回の移行はこれ**）:
`promote.sh` は旧 pid を**設定パスまで含めた完全一致**で見つけて `SIGTERM` → 終了を待つ（`kill_grace_secs` + 10 秒）→
DB バックアップ → `start taskd@<new>` → `health` 200 と `schema_version` を 60 秒待つ → 失敗なら新を止めて旧の unit
（あれば）を起こし、スキーマが進んでいたら**復元はせず**失敗を報告して人に任せる（D2 の `rollback.sh --restore-db`）。

### D5. taskd 上の「人」に許すこと・許さないこと（自己改善の案件）

- 案件「agent-platform の自己改善」の作業場所は `local: ~/workspace/agent-platform`（ADR-0039）。実装者は
  **taskd が用意した worktree とブランチ（`taskd/<task-id>`）にコミットする**（ADR-0041 D1。自分でブランチを
  切らない。`main` には直接コミットしない。作業ツリーはタスクごとに分かれていて本番にも人のチェックアウトにも
  影響しないので、ここで壊しても本番は動き続ける）。
- 実装者は `scripts/selfdeploy/release.sh <ブランチ>` と `verify.sh <sha12>` を**自分で実行してよい**（本番に触れない）。
  結果の `gate.json` / `verify.json` を成果物（`artifacts/`）に写し、報告に「検証済み sha」を書く。
- **`promote.sh` / `rollback.sh` / `systemctl` / `~/taskd/taskd.toml` の編集 / `~/taskd/*.sqlite3` への書き込み /
  本番プロセスへの `kill` は禁止**。役割の指示文（`implementer`）に明記する。昇格は人が `status.sh` で `verify.json.ok` を
  見て実行する。将来これを standing の認可で機械に渡すかは、実機で数回回してから決める（PROGRESS の提案へ）。

### D6. API / GUI（後続 Phase。ここでは契約だけ）

- `GET /releases` → `{current, previous, items: [{sha12, ref, built_at, schema_version, gate_ok, verify: {ok, live_ok, at}?, promoted_at?}]}`
  （`~/taskd/releases/*/manifest.json` 等を読むだけ。管理系ではない）。`GET /health` に `release` / `mode` / `role` を追加。
- `POST /releases/{sha12}/promote`（管理系）→ `promote.sh` を起動して 202。**GUI からの昇格は人が押す**（D5 と矛盾しない）。
  GUI「リリース」画面（G14）: 一覧、検証状態、`current`、「昇格」ボタン（確認付き）、引き継ぎの進行（`daemon_instances`）。

## 3. 採らない

- Docker / 別ホストへのデプロイ。本番はこの LXC 1 台（SPEC「リモート実行は別プロジェクト」）。
- ワーカー子プロセスの再親付け（fd の受け渡し）。drain で足りる。
- 自然文の判断で昇格を決める。昇格は `verify.json.ok` と人。
- DB の二重書き込み防止に分散ロック。役割は 1 行の表と heartbeat で足りる（同一ホスト・同一 SQLite）。

## 4. 受け入れ条件

**Phase 47（taskd 本体。先に）**: migration 0011 と役割（active / standby / draining / verify）、`--mode` / `--db` / `--listen` /
`--workspace-root` / `--token-file` の上書き、`SO_REUSEPORT`、standby の 503、draining の drain と exit 0、drain timeout、
同版二重起動の exit 3、`health` の `release` / `mode` / `role`。テスト: 1 つの SQLite に 2 つの `Dispatcher`/tick を起こし、
(a) 新が standby → 旧が draining → 新が active になる、(b) draining 中の run は旧が完了させ新は二重 dispatch しない、
(c) verify は dispatch しない、(d) 同版二重起動は exit 3、(e) 旧の heartbeat が止まれば新が active になる。
`cargo test --workspace` / clippy。

**Phase 46（配備の道具。Phase 47 の後）**: `scripts/selfdeploy/{release,verify,promote,rollback,status}.sh`、systemd の
テンプレート unit、GUI の `reusePort` と `/healthz`、`docs/gui/` ではなく `docs/selfdeploy.md` に運用手順。テスト: bash の
スクリプトは `bash -n` と shellcheck（あれば）、`release.sh` を実リポジトリで 1 回、`verify.sh` を本番 DB のコピーで 1 回
（本番プロセスには触れない）。**実機の初回移行**（停止→起動で `taskd@<sha12>` と `taskd-gui@<sha12>` へ）は人（または
この会話のエージェント）が `promote.sh` で行い、以後の昇格がライブになることを 2 回目の昇格で確かめる。

**Phase 48 / G14（後続）**: D6 の API と GUI、案件「agent-platform の自己改善」の登録と `implementer` の指示文（D5）。

## Phase 48 追記（2026-09-19。D6 からの逸脱だけ）

実装で D6 の契約から変えたところ。設計（誰が昇格を決めるか、何に触れないか）は変えていない。

1. **`GET /releases` の応答に `running` と `instances` を足した**。D6 は `{current, previous, items}` だけだが、
   GUI「リリース」画面は「いま動いているのはどれか」「引き継ぎがどこまで進んだか」を出す必要がある
   （D6 自身が G14 の要件に「引き継ぎの進行（`daemon_instances`）」と書いている）。`running` は
   `GET /health` の `release` / `mode` / `role` と同じ値、`instances` は `daemon_instances` の行そのまま。
   別エンドポイント（`GET /instances`。PROGRESS の P47-1）にはせず、1 回の読み直しで画面が作れる形にした。
2. **`items[]` の `promoted_at` を出さない**。どのリリースがいつ昇格したかは `~/taskd/releases/<sha12>/` の
   どのファイルにも書かれていない（`promote.sh` は `~/taskd/backups/promote-<ts>.log` にしか残さない）。
   読むだけで作れないので落とした。代わりに **`is_current` / `is_previous`**（symlink から）と
   **`promoting`**（`promote.lock` の pid が生きているか）を出す。
3. **`items[]` に `problem`（文字列、省略可）を足した**。`manifest.json` / `gate.json` が壊れていても
   一覧全体を落とさないため（そのリリースは `gate_ok = false` と `problem` を持つ）。
4. **`[selfdeploy] releases_dir` を設定に足した**（既定 `releases`、設定ファイルのディレクトリ基準）。
   D6 は「`~/taskd/releases/*/manifest.json` 等を読むだけ」としか書いておらず、パスの出どころが無かった。
   `current` / `previous` の symlink は `releases_dir` の**親**にある（`sd_set_link` がそう張るため）。
   本番の `~/taskd/taskd.toml` は書き換え不要（既定でそのまま当たる）。
5. **`release.sh` が `scripts/selfdeploy/*.sh` をリリースに同梱する**（`<release>/scripts/`）。
   `POST /releases/{sha12}/promote` が起こすのは**リリースの中の** `promote.sh` で、作業チェックアウトが
   別のブランチにいても・無くても昇格できる。`lib.sh` は `dirname "${BASH_SOURCE[0]}"` で自分の隣を読み、
   場所は全部 `TASKD_HOME` 基準なので、そのまま動く（`SD_REPO` を要るのは `release.sh` だけ）。
   Phase 48 より前に作られたリリースには `scripts/` が無いので、その昇格は 409 になる（shell から行う）。
6. **`GET /releases` は task-api → taskd をチャネルではなくトレイト（`task_api::ReleaseSource`）で越える**。
   `reload` / `check` / `notify/test` は `AdminRequest` の非同期チャネルだが、こちらは同期のファイル読み取りで、
   `standby` でも答えられる（ディスパッチャの状態を要しない）ため 503 にしたくない。実体
   （`taskd::releases::FsReleases`）は taskd 側にあり、task-api はファイルの規約を知らないまま。
7. **`POST /releases/{sha12}/promote` に `require_active` を付けない**。昇格を始めるのに
   ディスパッチャは要らない（外部プロセスを起こすだけ）。むしろ `standby` からも押せる方がよい。
