# 自己改善のデプロイ — 運用手順（ADR-0040）

`agent-platform` 自身を celeris の上で改善し、**動いている本番を壊さずに**新しい版へ移るための道具。
設計は `docs/adr/0040-self-improvement-deploy.md`（D1〜D5）と
`docs/adr/0041-self-improvement-loop-hardening.md`（D2〜D4）。ここはその**使い方**だけを書く。

## 0. 全体像

```
release.sh <ref>  →  verify.sh <sha12>  →  promote.sh <sha12>        （戻すとき: rollback.sh）
  本番に触れない      本番に触れない        人だけが実行する           人だけが実行する
  誰が実行してもよい   誰が実行してもよい
```

置き場は**設定**と**状態**に分かれている（ADR-0045 D2。XDG 流）。どちらも環境変数で動かせる:
`CELERIS_CONFIG_DIR`（既定 `~/.config/celeris`）と `CELERIS_STATE_DIR`（既定 `~/.local/celeris`）。

```
~/.config/celeris/       （0700。設定と秘密）
  config.toml            本番の設定（**誰も書き換えない**。移行の migrate-to-celeris.sh だけが 1 度書く）
  org.toml  providers.d/ 組織の種とプロバイダ（GUI が書く）
  api.token              本番 API のトークン
  gui.password  gui.session-secret  GUI の認証
  secrets/               ADR-0030 の秘密（1 秘密 = 1 ファイル）

~/.local/celeris/        （状態）
  celeris.sqlite3        本番の DB（`sqlite3 .backup` と `mode=ro` で読むだけ）。ADR-0065 D1/D2
                          （Phase 110a）以降、`config.toml` の `db =`（または `[db].path`）を
                          ローカルディスクの別パス（例 `/var/lib/celeris/celeris.sqlite3`）に向けて
                          よい。状態ディレクトリ本体はここのまま（§5b）。
  current -> releases/<sha12>     いま動いている版
  previous -> releases/<sha12>    直前の版（rollback 先）
  releases/<sha12>/     bin/{celeris,celerisctl}  gui/  manifest.json  gate.json  verify.json
                        changes.json      この版で何が変わるか（ADR-0041 D4。§2）
                        promoted.json     昇格の記録 {promoted_at, mode, from}（ADR-0041 D3。§4。成功したときだけ）
                        promote_failed.json  直近の昇格が失敗した記録 {failed_at, error}（§4。失敗したときだけ。
                                          次の昇格の試みが始まると消える）
                        scripts/          selfdeploy 一式の写し（ADR-0040 D6。昇格に作業チェックアウトが要らない）
                        promote.log       この API 経由の昇格の出力（§4c）
                        promote.lock      昇格中の pid
  releases/.build/      release.sh が生やす detached の作業ツリー（成功したら消える）
  releases/.build/.lock-<sha12>  同じ sha の二重ビルドを止める flock（ADR-0041 D2）
  releases/.lock-release  異なる sha も含め、検査から成果物のコピーまでを直列化する flock
  releases/.cargo-target/  CARGO_TARGET_DIR（リリース間で共有。ビルドを速くするだけ）
  staging/              verify.sh の作業場所（毎回作り直す。`.lock` だけは残る）
  staging/.lock         verify.sh を 1 本に直列化する flock（ADR-0041 D2）
  backups/              昇格前の DB のコピーと promote-<ts>.log
  backups/pre-celeris/  改名の移行で残した旧い設定と unit（§9）
  workspaces/  memory/  claude-accounts/  codex-accounts/  logs/
  tools/{ldr,paperqa,opencode}/   アダプタが使う venv と設定
```

`SD_DB`（selfdeploy が読む DB）は **`config.toml` の `db =` から決まる**。移行のあいだだけ
`CELERIS_DB` で、設定ファイルそのものは `CELERIS_CONFIG` で上書きできる（§9）。

**同時に走らせない**（ADR-0041 D2）。`verify.sh` は staging のディレクトリとポートを固定で使うので、
2 本目は `staging/.lock` で待つ（上限 `SD_VERIFY_LOCK_WAIT`、既定 1800 秒。超えたら **exit 75**
= EX_TEMPFAIL「いまは無理。あとでもう一度」）。`release.sh` は同じ sha の 2 本目が**待たずに** exit 75
（同じ sha を 2 度ビルドしても結果は同じなので、待つ意味が無い）。**75 は「壊れた」ではなく「混んでいる」**。

ポート:

| | 本番 | staging（verify.sh） |
|---|---|---|
| celeris API | `127.0.0.1:7710` | `127.0.0.1:7711`（N-1 は `:7712`） |
| GUI | `0.0.0.0:7700` | `127.0.0.1:7701` |

## 1. 最初に一度だけ: systemd の unit を入れる

テンプレート unit はリポジトリの `deploy/systemd/` にある。**人が一度だけ**入れる:

```bash
bash ~/workspace/agent-platform/scripts/selfdeploy/install-units.sh
# = deploy/systemd/celeris@.service と celeris-gui@.service を ~/.config/systemd/user/ に置いて daemon-reload
systemctl --user cat celeris@.service        # 入ったことの確認
loginctl show-user "$USER" | grep Linger   # Linger=yes であること（ログアウトしても動き続ける）
```

`%i` はリリースの `sha12`。`celeris@<sha12>` は `~/.local/celeris/releases/<sha12>/bin/celeris --config ~/.config/celeris/config.toml
--release <sha12>`、`celeris-gui@<sha12>` は `~/.local/celeris/releases/<sha12>/gui/` で `node server.js` を動かす。
GUI の環境変数はすべて `CELERIS_*`（Phase 58 / ADR-0045 D1 で改名。読み替えの互換は無い）。値は今の本番と同じ
（`0.0.0.0:7700` で bind、API は `127.0.0.1:7710`、トークン・パスワード・セッション鍵は `~/.config/celeris/` の下）。

`install-units.sh --remove-old` は**改名の移行のときだけ**使う（`migrate-to-celeris.sh` が渡す）。
消す対象の名前は環境変数 `SD_OLD_UNITS` で渡す — 改名前の名前を知っているのは移行スクリプトだけ。

## 2. リリースを作る（`release.sh`）

```bash
scripts/selfdeploy/release.sh HEAD          # または ブランチ名 / タグ / sha
scripts/selfdeploy/release.sh celeris/01M2XXX # 自己改善の案件の実装ブランチ（celeris が切る。ADR-0041 D1）
```

- `~/.local/celeris/releases/.build/<sha12>` に **detached worktree** を生やして、そこでだけビルドする。
  作業チェックアウト（`~/workspace/agent-platform`）が汚れていても、その中身は使われない。
- gate（この順。1 つでも非 0 ならリリースを作らない。全 9 段）:
  自前のworkspaceパッケージの `cargo clean -p …`（外部依存のキャッシュは保持）
  → `cargo test --workspace` → `cargo clippy --workspace -- -D warnings` → `cargo build --release -p celeris -p celerisctl`
  → GUI `pnpm install --frozen-lockfile` → `pnpm typecheck` → `pnpm test` → `pnpm build`
  → `pnpm mobile-audit`（`pnpm-mobile-audit`） → `pnpm e2e:mock`（`pnpm-e2e-mock`）
- **Phase 89（`pnpm-mobile-audit` / `pnpm-e2e-mock`。ADR-0041 追記、ADR-0055 D3）**: `pnpm-build` の直後に
  足した 2 段。どちらも `$BUILD/gui` で走り、直前の `pnpm-build` が作った `$BUILD/gui/build` を
  そのまま使い回す（`MOBILE_AUDIT_SKIP_BUILD=1` / `E2E_SKIP_BUILD=1`。ビルドをやり直さない）。
  - `pnpm-mobile-audit`: `gui/scripts/mobile-audit.mjs`（ADR-0055 D1）。偽の celeris + ビルド済み GUI を
    自分のポートに起こし、全画面 × light/dark を Playwright Chromium で監査する。1 件でも違反があれば
    非 0。
  - `pnpm-e2e-mock`: `gui/scripts/e2e-check.mjs` の `pnpm e2e:mock`（Phase 83 / G36 が足した読み取り専用
    e2e の、オフライン・偽 celeris に対するモード。`verify.sh` の検査 4b が使う `pnpm e2e:staging` とは
    別物で、こちらは何も外の staging に繋がない）。
  - どちらも `timeout "${SD_AUDIT_TIMEOUT:-600}"`（既定 600 秒）で壁時計の上限を掛ける。他の段と同じ
    `run_step` を通るので、lock の fd（8, 9）は継がない（Phase 66c の lock-leak 対策がそのまま効く）。
  - **Playwright の Chromium 実行ファイル**: `pnpm install --frozen-lockfile`（`pnpm-install` 段）は
    `@playwright/test` という npm パッケージを入れるだけで、Chromium の実行ファイル自体
    （`pnpm exec playwright install chromium` で落とすもの）は入れない。実行ファイルはホストの
    `~/.cache/ms-playwright/` に**バージョンごとに 1 つ**入っていて、`pnpm-lock.yaml` が固定されている
    限りどの worktree からも同じキャッシュを共有できる。**リリースを作るホストでは事前に一度
    `pnpm exec playwright install chromium` を実行しておくこと**（このリポジトリの devDependencies は
    固定版なので、以後 `pnpm install` のたびに入れ直す必要は無い）。無いままこの 2 段を回すと、
    ブラウザ起動の呼び出し**前**に軽い存在チェックが入り、ハングせずに `false — playwright browser not
    installed`（`.gate-pnpm-mobile-audit.log` / `.gate-pnpm-e2e-mock.log`）で即座に失敗する。
- 異なるSHAのビルドも共有出力を上書きしないよう直列化する。待ち時間の上限は
  `SD_RELEASE_LOCK_WAIT`（既定1800秒）。Cargo自身のロックだけでは、doctestや成果物コピーまで保護できない。
  別worktreeの相対dep-info・mtimeによる古いメタデータの再利用も防ぐため、自前パッケージを先に掃除する。
- 成功したら `~/.local/celeris/releases/<sha12>/` に `bin/`（celeris, celerisctl）、`gui/`（build/ server.js package.json
  pnpm-lock.yaml pnpm-workspace.yaml と `pnpm install --prod` の node_modules）、`manifest.json`、`gate.json`、
  `gate-logs/`、`scripts/` を置き、ビルド用の worktree を消す。
- `scripts/` は **その sha の `scripts/selfdeploy/*.sh` をそのまま写したもの**（実行ビットごと。ADR-0040 D6、
  Phase 48）。`POST /releases/{sha12}/promote` はこの `<release>/scripts/promote.sh` を起こすので、
  **作業チェックアウトが別のブランチにいても、無くても昇格できる**。`lib.sh` は
  `dirname "${BASH_SOURCE[0]}"` で自分の隣を読むだけで、場所はすべて `CELERIS_STATE_DIR` 基準なので
  リリースの中から source しても動く（`SD_REPO` を要るのは `release.sh` の worktree 操作だけ）。
- 失敗したら**リリースディレクトリは作らず**、`~/.local/celeris/releases/.build/<sha12>/gate.json` と
  `.gate-<step>.log` を残す（次に同じ sha で `release.sh` を回すと消える）。
- 掃除: `current` / `previous` / いま作った版 / 検証済み（`verify.json.ok`）の新しい 3 件だけを残す。

`manifest.json`:

```json
{"sha": "...", "sha12": "...", "ref": "HEAD", "built_at": "...", "profile": "release",
 "schema_version": 10, "celeris_version": "0.1.0", "gui_version": "0.1.0", "gate_ok": true}
```

`schema_version` は その sha の `crates/task-core/src/store.rs` の `pub const SCHEMA_VERSION` を読んだもの。

### 昇格したら何が変わるか（`changes.json`。ADR-0041 D4）

`release.sh` は**ビルド時の `current`** を起点に、そこからこのリリースまでの差分を書き留める:

```json
{"base": "70e3175eeb20",
 "at": "2026-09-19T13:00:00Z",
 "commits": [{"sha": "…40 桁…", "subject": "phase 50 / G15: …"}],
 "files": ["scripts/selfdeploy/verify.sh", "docs/PROGRESS.md"],
 "sensitive": ["scripts/selfdeploy/verify.sh"]}
```

- `base` は `~/.local/celeris/current/manifest.json` の sha12。`current` が無ければ `null` で、
  `commits` / `files` は空（比べる相手が無いので「何が変わるか」を言えない）。
- `commits` は `base..<sha>` を**新しい順に最大 50 件**。`files` は `git diff --name-only base <sha>`。
- **`sensitive`** は `files` のうち、`scripts/selfdeploy/lib.sh` の **`SD_SENSITIVE_PATTERNS`** に
  **前方一致**したもの。一覧はそこ 1 か所にしかない（API も GUI もこの結果を運ぶだけ）:

  `scripts/selfdeploy/` / `deploy/` / `crates/celeris/src/instance.rs` / `crates/celeris/src/releases.rs` /
  `crates/task-api/src/releases.rs` / `crates/task-core/migrations/` / `CLAUDE.md` / `gui/CLAUDE.md` /
  `.claude/` / `config/` / `docs/adr/0040-` / `docs/adr/0041-`

  これらを変えたリリースは「昇格の仕組みそのもの」「本番の設定」「エージェントへの指示文」を変える。
  GUI は赤いバッジ「安全に関わる変更 N 件」とパス一覧を**最初から開いて**出し、「昇格」は
  `window.confirm` ではなく **sha12 を打たせる確認**にする（§4c）。
- `GET /releases` の `items[].changes` は、これに `stale`（`base` がいまの `current` と違う）と
  `commit_count` / `file_count` を足したもの。`stale` が真なら、その差分はもう「いま昇格したら」の話では
  ないので、`release.sh` を回し直すのがよい。

## 3. 検証する（`verify.sh`）

```bash
scripts/selfdeploy/verify.sh <sha12>
scripts/selfdeploy/verify.sh --dry-run <sha12>   # 前提だけ確かめる（何も起こさない）
SD_VERIFY_LOCK_WAIT=60 scripts/selfdeploy/verify.sh <sha12>   # 他の検証を 60 秒だけ待つ
SD_E2E_TIMEOUT=120 scripts/selfdeploy/verify.sh <sha12>       # 検査 4b（gui-e2e）の時間切れを短く（既定 240 秒）
```

やること（ADR-0040 D3 / ADR-0041 D2。本番には触れない）:

0. `~/.local/celeris/staging/.lock` を `flock` で取る（**1 本ずつしか走らない**）。`SD_VERIFY_LOCK_WAIT` 秒
   （既定 1800）待って取れなければ **exit 75** で「他の検証が走っている」と言って終わる
   （何も起こさず、何も消さない）。
1. `~/.local/celeris/staging/` を作り直し（`.lock` だけ残す）、
   `sqlite3 "file:~/.local/celeris/celeris.sqlite3?mode=ro" ".backup staging.sqlite3"`。
   **その場（マイグレーション前）で件数を数える**（検査 2 の基準）。
2. 新リリースの celeris を **verify モード**で `127.0.0.1:7711` に起こす
   （`--mode verify --db <snapshot> --listen … --workspace-root … --token-file … --release <sha12>`。
   設定は**本番の `config.toml` をそのまま読む**。上書きは CLI だけ）。
3. 検査:
   1. 起動し、`health.schema_version` が新バイナリの `SCHEMA_VERSION` と等しい（＝本番のデータで
      マイグレーションが通った）
   2. **件数一致（ADR-0041 D2 で意味が変わった）**: **同じスナップショットのマイグレーション前後**を比べる。
      前は `.backup` 直後に `sqlite3` で数えた**生の行数**、後は staging API（＝新バイナリが
      マイグレーションした同じファイル）。対象は `tasks` / `projects` / `milestones` / `org_nodes` /
      `approvals`（全件と決定済み） / `reports` / `messages` の件数と、`tasks` の `{id,status}` の
      ダイジェスト（sha256 の先頭 16 桁。両側**同じ式**で計算する）。
      **本番 API はもう一切叩かない**（本番は検証の間も動いているので、本番と比べると当たり前にずれて
      偽陰性になっていた。ADR-0041 §1-2）
   3. 主要 GET が 200 かつ JSON（`inbox` / `org/<最初のノード>/memory` / `notify` / `clusters` / `providers` / `config`）
   4. 新リリースの GUI を `127.0.0.1:7701` に起こして `/healthz`（`release` が新しい sha12）と
      `/`, `/org`, `/projects`, `/projects/<最新>`, `/approvals`, `/reports`, `/clusters` が 200
   4b. **GUI の e2e（read-only。Phase 83 / G36）**: 検査 4 の GUI/celeris に対して `pnpm e2e:staging`
      （`gui/scripts/e2e-check.mjs`）を走らせる。詳しくは下の節
   5. **N-1 互換**: `~/.local/celeris/current/bin/celeris`（旧）を、**新バイナリがマイグレーションした後の**同じ
      スナップショットに対して `:7712` で起こし、1〜3 と同じ検査（件数は**スナップショット**と比べる。
      ここでも本番 API は読まない）。落ちたら `live_ok = false`（`current` が無い初回も `live_ok = false`）。
      **煙試験（6）はここではやらない**（旧バイナリは `smoke` を知らない）
   6. **煙試験（ADR-0041 D5）**: staging に 1 件だけタスクを流し、**dispatch → ワーカー起動 →
      結果の取り込み → レビュー → 終端 → 報告の生成**までを通す。詳しくは下の節
4. `~/.local/celeris/releases/<sha12>/verify.json` を書く。`ok` は **1〜4・4b・6 が全部真**のとき。`live_ok` は 5。
5. 起こしたプロセスは `trap` で必ず止める（自分が起こした pid だけ）。

### 検査 4b: GUI の e2e（read-only。Phase 83 / G36、ADR-0041 追記）

`gui/e2e/*.spec.ts`（`pnpm e2e`）はタスクを作って承認する結合テストで、`scripts/celeris.sh` が起こす
使い捨ての celeris に対して行う前提（ADR-0055 D3 が挙げていたギャップ）。staging はそのまま使えない
（検査 2 / 5 の件数一致は「検査 6 の煙試験だけが書き込む」という前提で組んである。ADR-0041 D2/D5）。

検査 4b は別物で、**読み取りだけ**（ナビゲーションと `?tab=` の切り替え。`POST` は一切しない）を
`gui/scripts/e2e-check.mjs`（`pnpm e2e:staging`）で行う。**検査 6（煙試験）より前**に置く（検査 6 が
足す 1 件がここに写り込まないように）:

1. mobile-audit（ADR-0055 D1）と同じ画面一覧を 393×851 と 1280×800 の両方で開き、200 で応答し、
   コンソールエラー・失敗した要求（401 は許容）が無いこと。`taskId` / `projectId` / `orgId` / `skillName`
   は Node 側（ブラウザではない）が `GET /api/v1/{tasks,projects,org,skills}` を staging のトークンで
   読んで見つけたものを使う（gui/CLAUDE.md の境界どおり、ブラウザは celeris を直接叩かない）。
   見つからなければその id が要る画面（`/projects/<id>` 等）はスキップする
2. `/`（Console）が `[data-testid="console-screen"]` を描画すること
3. `/accounts` が「LLM source」「MCP クライアント」節（`llm-sources-section` / `mcp-clients-section`）を
   描画すること
4. `/knowledge/skills` が描画すること
5. 見つかった実在のタスクで `/tasks/<id>` のタブ（概要・タイムライン・変更・ファイル・成果物）を
   `<Link>` のクリックで切り替え、対応する節（`info-section` 等）が出ること

**release の `gui/`（`release.sh` が `pnpm install --prod` した方）には Playwright が入っていない**
（devDependency なので剥がされる）。検査 4b は `$SD_REPO/gui`（既定 `~/workspace/agent-platform/gui`。
人・自己改善の作業ツリーが `pnpm install` 済みの方）から走らせ、release の `gui/` にしか無ければそちらを
使う。**どちらにも `@playwright/test` が無ければ、検査 4b は `false — not installed`**（クラッシュしない。
`verify.json` の他の検査には影響しない）。`SD_E2E_TIMEOUT`（既定 240 秒）を超えたら `false — timed out`。
ログは `~/.local/celeris/staging/logs/e2e-staging.log`。

手で確かめる（本物の celeris/GUI は起こさない。外部ネットワークに出ない）:

```bash
cd gui
pnpm e2e:mock       # 偽の celeris + pnpm build 済みの GUI に対して読み取り専用の e2e。exit 0 なら OK
```

### 検査 6: 煙試験（ADR-0041 D5）

検査 1〜5 は「起動する・データが残っている・画面が出る」しか見ない。アダプタ・前置き・委譲・レビューの
回帰は素通りして本番に届いていた。検査 6 はそこを塞ぐ。

**verify モードの celeris は `genre = "smoke"` のタスクだけを dispatch する**（それ以外の ready は
従来どおり 1 件も動かさない。リースも奪わない、レビューも拾わない、`daemon_instances` にも書かない）。
`smoke` の**役割・分野・プロバイダは verify モードが組み込みで足す**（`Config::apply_verify_smoke`）:

| | 中身 |
|---|---|
| `[[providers]] id = "smoke"` | `adapter = "fake"`、`tiers = ["standard"]`、`concurrency = 1` |
| `[[roles]] id = "smoke"` | `adapter = "fake"`、`tier = "standard"`、`max_turns = 1`、`max_wall_secs = 60` |
| `[[genres]] id = "smoke"` | `description = "検証の煙試験"`、`default_role = "smoke"`、`roles = ["smoke"]` |
| `[adapters.fake].command` | `FakeAdapter::default_command()` に固定 |
| `[reviewer]` | `adapter = "fake"` / `tier = "standard"` |

設定ファイル（`~/.config/celeris/config.toml`）に同じ id があっても**上書きする**。本番の設定に何を書いても、
煙試験が本物の LLM を呼ぶ経路は無い（ADR-0041 §3「煙試験で本物の LLM を呼ばない。`fake` だけ」）。
これらが出るのは verify モードの `GET /config` だけで、本番の設定ファイルは 1 バイトも変わらない。

`verify.sh` がすること（**検査 5 の後**。ここで 1 件足すので、先に回すと検査 2 / 5 の件数がずれる）:

1. `POST /tasks`（staging のトークン）
   `{"title":"smoke","genre":"smoke","role":"smoke","acceptance":[{"type":"command","cmd":"true","expect_exit":0}],"assignee":"<最初の組織ノード>"}`
   — 受け入れ条件がコマンドなのでレビューにも LLM が要らない。`assignee` を付けるのは
   **報告の生成（ADR-0034）まで**回帰に入れるため（報告は `assignee` のあるタスクにだけ作られる）。
   そのノードがスナップショットに無ければ `assignee` 無しでもう一度作り、報告の段だけ飛ばす
2. `POST /tasks/{id}/approve`（draft → ready）
3. `GET /tasks/{id}` を 1 秒ごとに見て、**60 秒以内**に `status == "done"`
4. `GET /tasks/{id}/events` に `worker_started` と、`outcome` が `done…` の `worker_finished` がある
5. `GET /reports?node=<assignee>` にそのタスク（`task_id`）の報告が出る

結果は `verify.json.checks` の `id = 6` に `{ok, task_id, elapsed_s, detail}` として載り、
`verify.json.ok` の条件に入る。生の JSON は `~/.local/celeris/staging/logs/smoke.json`。

落ちたときの見方:

- `the smoke task is 'ready' after 60.0s` — dispatch されていない。`logs/celeris-new.log` に
  `no provider in the config matches this worker_hint` が出ていないか（`smoke` の組み込みが
  入っていない＝そのリリースが Phase 51 より前）
- `the smoke task is 'failed' after …` — ワーカーかレビューが落ちた。`GET /tasks/{id}` の
  `runs` と `~/.local/celeris/staging/workspaces/<task_id>/runs/` を見る
- `no report for the smoke task under node …` — 終端での報告の生成（ADR-0034）が壊れている

### 件数一致が落ちたとき

`verify.json` の `counts` は `{snapshot, staging}` の 2 つ（`prod` / `prod_after` は ADR-0041 D2 で
**消えた**）。両方とも**同じスナップショット**を見ているので、ずれたなら理由は 1 つ:
**マイグレーションが行を消したか変えた**。`~/.local/celeris/staging/staging.sqlite3` は残っているので、
その場で `sqlite3` で突き合わせられる。

1 つだけ外因がある。`reports` と `messages` は「全件を返す入口」が API に無いので、
**組織のノードごと**（`messages` はさらに案件ごと）に数えて足している。組織のノードを消した後に
残っている行（`node_id` が `org_nodes` に無い行）はそこから漏れるので、`snapshot` の方が多く出る。
そのときは失敗の一行に `orphan reports/messages ...` の数が出る。

`verify.sh` が **exit 75** で終わったときは検査ではなく**順番待ち**（他の `verify.sh` が走っている）。
`SD_VERIFY_LOCK_WAIT` を伸ばすか、しばらく待ってから回し直す。

### staging の見方

```
~/.local/celeris/staging/
  .lock                  直列化の flock（作り直しでも消さない。ADR-0041 D2）
  staging.sqlite3        本番 DB のコピー（新バイナリがマイグレーション済み）
  api.token              staging だけのトークン（毎回作り直す）
  workspaces/            verify モードの作業場所（煙試験の 1 件だけがここに出る。ADR-0041 D5）
  logs/celeris-new.log     新リリースの celeris の出力 ← 起動に失敗したらまずここ
  logs/celeris-old.log     N-1（旧バイナリ）の出力 ← `SchemaTooNew` はここに出る
  logs/gui.log           staging GUI の出力
  logs/counts-*.json     件数の生データ（snapshot = マイグレーション前 / staging / n1）
  logs/health-*.json     health の生データ
  logs/smoke.json        煙試験の生データ（`{ok, task_id, elapsed_s, detail, report}`。ADR-0041 D5）
```

`verify.sh` は次に走ったときに `staging/` を**丸ごと作り直す**。残したいログは先にコピーすること。

## 4. 昇格する（`promote.sh`。人だけ）

```bash
scripts/selfdeploy/status.sh | less        # verify.json.ok と live_ok を見る
scripts/selfdeploy/promote.sh <sha12>
```

- `verify.json.ok` が真でなければ**拒否する**（`--force` は無い）。
- ログは `~/.local/celeris/backups/promote-<ts>.log`。DB のコピーは `~/.local/celeris/backups/<ts>-pre-<sha12>.sqlite3`。
- 成功したら `~/.local/celeris/releases/<sha12>/promoted.json` に `{promoted_at, mode, from}` を書く
  （ADR-0041 D3。`GET /releases` の `promoted_at` と `status.sh` はこれを読む）。
- **失敗したら**（`sd_die` を含め、`set -e` でどこで止まっても）`~/.local/celeris/releases/<sha12>/promote_failed.json`
  に `{failed_at, error}` を書く（`error` は `promote-<ts>.log` の末尾 20 行）。バグ報告（2026-09-21）:
  `promote.sh` は API から detached で起こされる（§4c）ので、失敗しても呼び出し元の celeris は
  「起動できた」ことしか知らない。この印が無いと、GUI は「昇格が終わった（`promoting` が偽に戻った）」
  ようにしか見えず、成功したのか失敗したのか区別できなかった。次の昇格の試みが始まると消える
  （`POST /releases/{sha12}/promote` が spawn の直前に消す）ので、古い失敗が残り続けることはない。
  `GET /releases` の `items[].promote_failed` と `status.sh` の `promote_failed` はこれを読む。
- **git リポジトリには触れない。** `main` への反映は人がやる（次節）。

### 4d. 昇格したら `main` に戻す（ADR-0041 D3）

昇格は `~/.local/celeris/` の中だけで完結し、あなたのチェックアウト（`~/workspace/agent-platform`）は
一切動かない。放っておくと「本番で動いているコード」が `main` に無い状態が続き、次のタスクが
古い `main` から分岐する。だから**昇格したら人が `main` に反映する**:

```bash
scripts/selfdeploy/status.sh | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["current"], [r["on_main"] for r in d["releases"] if r["is_current"]])'
# current が on_main: false なら
cd ~/workspace/agent-platform
git switch main
git merge --ff-only <sha12>     # 実装者のブランチ（celeris/<task-id>）の先端がその sha
```

- GUI の「リリース」画面は、**現行の行が `on_main: false` のとき**だけ
  「本番は main に未反映: `git merge --ff-only <sha12>`」と出す。
- `on_main` は `git -C <[selfdeploy] repo> merge-base --is-ancestor <sha> main` の結果。
  `repo` の既定は `~/workspace/agent-platform`（`~/.config/celeris/config.toml` に書かなくてよい）。
  リポジトリが無い・その sha を知らないときは `null` になり、GUI は何も言わない。
- `--ff-only` なので、**`main` が先に進んでいたら止まる**。そのときは実装者のブランチを
  `main` に rebase してから `release.sh` をやり直すのが早い（本番より古いコードを `main` に混ぜない）。

### 4a. 初回の移行（停止 → 起動）

いまの本番は `~/workspace/agent-platform/target/debug/celeris` を手で起こしたもので、`daemon_instances` も
`/health` の `role` も知らない。`promote.sh` はそれを見て**停止 → 起動**を選ぶ（`live_ok` も偽）。

1. 旧 celeris の pid を**設定パスまで含めた完全一致**で探す（`argv[0]` の basename が `celeris` で、
   `--config ~/.config/celeris/config.toml` を持ち、`--mode` / `--db` / `--listen` 等を持たないもの。
   `grep` や `bash -c` や staging のプロセスは当たらない）→ `SIGTERM` → `kill_grace_secs + 10` 秒待つ
2. 旧が終わってから DB をバックアップ（引き継ぎ中の仕事もコピーに入る）
3. `systemctl --user start celeris@<sha12>` → `/health` が 200 かつ `schema_version` が期待どおりになるまで 60 秒待つ
4. 駄目なら新を止め、旧 unit があれば起こし直し、**DB は戻さずに**失敗を報告する
   （スキーマが進んでいるかもしれないので、戻すかどうかは人が `rollback.sh --restore-db` で選ぶ）
5. `:7700` で LISTEN している旧 GUI（`node server.js`）を `ss -ltnp` で見つけて `SIGTERM` →
   `systemctl --user start celeris-gui@<sha12>` → `/healthz` が 200 になるまで待つ
6. `enable` 新 / `disable` 旧、`current` / `previous` の symlink を更新

**この間だけ API と GUI が止まる**（数十秒）。走っていたワーカーの run はリースが切れて新しい daemon が拾う。

### 4b. 2 回目から（ライブ引き継ぎ）

`verify.json.live_ok` が真で、動いている celeris の `/health` が `role` を持っているとき:

1. DB をバックアップ
2. `systemctl --user start celeris@<新>` → 新は `standby` で起き、同じポートに `SO_REUSEPORT` で bind する
3. `/health` を**毎秒 5 回ずつ**見て、5 回とも `release == <新>` かつ `role == active` になるまで 60 秒待つ
   （新旧が同じポートを共有するので 1 回の応答では判らない。5 回とも新なら旧はもう listener を閉じている）
   - ならなければ `systemctl --user stop celeris@<新>` して**失敗**。旧はそのまま動き続ける
4. `enable` 新 / `disable` 旧
5. `systemctl --user start celeris-gui@<新>` → `:7700/healthz` の `release` が新になるまで待つ → `stop celeris-gui@<旧>`
6. `current` / `previous` を更新

**旧 celeris の drain 完了は待たない**。旧は `draining` になって自分が起こした run だけ面倒を見続け、
手元の run が 0 になったら exit 0 する（最長 `[handoff] drain_timeout_secs`）。進み具合は `status.sh` の
`daemon_instances` で見える。API が一瞬 `503 {"detail":"standby"}` を返す管理系（`reload` / `check` /
クラスタ接続 / アカウントのログイン / `notify/test`）があるが、窓は 1〜2 tick。

### 4c. GUI から昇格する（Phase 48 / G14。ADR-0040 D6）

GUI「リリース」画面（`/releases`）の「昇格」ボタンは `POST /releases/{sha12}/promote`（管理系）を叩き、
celeris が **`~/.local/celeris/current/scripts/promote.sh <sha12>`** を detached（`setsid`、stdin は
`/dev/null`、stdout/err は `<release>/promote.log`）で起こす。やることは shell から `promote.sh` を
叩くのと**同じ**で、押すのは人（ADR-0040 D5。celeris の中に自動で呼ぶ経路は無い）。

**どちらの `promote.sh` か**（ADR-0041 D4。Phase 50 で変わった）: **いま動いている版（`current`）**の
もの。昇格は「動いている本番を止めて／引き継いで替える」作業なので、その手順を知っているべきなのは
いまの本番の方。実装者が `scripts/selfdeploy/` を壊したリリースを作っても、その壊れた昇格スクリプトは
走らない（新しい昇格スクリプトは、それ自身が一度昇格されてから次の昇格で使われる）。
`current` に `scripts/` が無い（Phase 48 以前・初回）ときだけ昇格先のものを使い、
応答の `script_from` が `"current"` / `"target"` のどちらかを言う。

**安全に関わる変更があるリリース**（`changes.sensitive` が空でない。§2）は、画面が赤いバッジ
「安全に関わる変更 N 件」とパス一覧を**最初から開いて**出し、「昇格」ボタンは**押す前に sha12 を
打たせる**（打つまでボタンは灰色。`window.confirm` は出ない）。普通のリリースは従来どおり
`window.confirm` の二重確認。

```bash
# shell からと同じことを API で
curl -sS -X POST -H "Authorization: Bearer $(cat ~/.config/celeris/api.token)" \
     http://127.0.0.1:7710/api/v1/releases/<sha12>/promote
# → 202 {"sha12":"…","log":"~/.local/celeris/releases/<sha12>/promote.log","started_at":"…"}
```

- 断られるとき（409 `release_not_promotable`）: `verify.json` が無い／`ok` でない、既に `current`、
  既に昇格中（`promote.lock` の pid が生きている）、`current` にも昇格先にも `scripts/promote.sh` が無い
  （どちらも Phase 48 より前のリリース — その場合は shell から `scripts/selfdeploy/promote.sh` を使う）。
- ログは `<release>/promote.log`（**API では中身を出さない**）と、`promote.sh` 自身が書く
  `~/.local/celeris/backups/promote-<ts>.log`。進行は `GET /releases` の `instances`（旧 `draining` / 新 `active`）
  と `status.sh` で見る。
- **202 を返した celeris 自身がその昇格で `draining` になって終わる**（ライブ引き継ぎ）。API が一瞬
  切り替わるのは正常（新旧が `SO_REUSEPORT` で同じポートを共有する）。

`~/.config/celeris/config.toml` に足す設定は無い（`[selfdeploy] releases_dir` の既定が `releases` なので、
`~/.local/celeris/releases` をそのまま見る）。

## 5. 戻す（`rollback.sh`。人だけ）

```bash
scripts/selfdeploy/rollback.sh              # previous を昇格し直す
scripts/selfdeploy/rollback.sh --restore-db # DB も昇格前に書き戻す（昇格後の仕事は失われる）
```

- 旧バイナリの `SCHEMA_VERSION`（`previous/manifest.json`）が DB の `schema_version`（`/health`、
  取れなければ `sqlite3 "select max(version) from schema_migrations"`）**以上**なら、そのまま
  `promote.sh previous` と同じことをする（live か停止→起動かは `previous` の `verify.json.live_ok` 次第）。
- 旧の方が低い（`SchemaTooNew` で起動できない）ときは**拒否する**。`--restore-db` を付けたときだけ:
  1. `~/.local/celeris/backups/` の直近の `*-pre-*.sqlite3` を選ぶ（`*-pre-rollback.sqlite3` は選ばない）
  2. いまの DB を `<ts>-pre-rollback.sqlite3` に退避
  3. celeris（unit か、完全一致で見つけた pid）と GUI を止める
  4. 選んだコピーを `celeris.sqlite3` に上書きし、`-wal` / `-shm` を消す
  5. `celeris@<previous>` / `celeris-gui@<previous>` を起こす → symlink を更新

**書き戻すと、昇格してから今までに進んだ仕事は消える。** どちらが損かを人が決める。

## 5b. DB をローカルディスクへ移す（`relocate-db.sh`。人だけ）

ADR-0065（Phase 110a）: 本番の DB（`~/.local/celeris`）が `/home` にあり、その実体が NFS 越しの
raw イメージ（`/dev/loop*`）だと、fsync が数十 ms かかり I/O が混むと `database is locked` や
GUI のタイムアウトが起きる（実機で観測。`docs/PROGRESS.md` Phase 110a）。DB ファイル**だけ**を
ローカルディスク（例 `/var/lib/celeris/`）へ移し、状態ディレクトリ本体（workspaces / releases /
backups / tools）は `/home` のまま残せる。

```bash
sudo install -d -o "$(whoami)" -g "$(whoami)" -m 0750 /var/lib/celeris   # 先に人が作る（スクリプトは作らない）
scripts/selfdeploy/relocate-db.sh /var/lib/celeris/celeris.sqlite3 --dry-run   # 計画と in-flight の確認だけ
scripts/selfdeploy/relocate-db.sh /var/lib/celeris/celeris.sqlite3            # 実行
```

手順（`docs/adr/0065-db-local-disk-and-store-resilience.md` D2）:

1. `GET /api/v1/tasks` の `counts_by_status`（`running` + `reviewing`）が 0 であることを確認する。
   0 でなければ何もせず止まる（待つか、`--dry-run` で様子を見てから改めて実行する）。
2. `current` の sha の `celeris@<sha>` / `celeris-gui@<sha>` を止める。
3. `sqlite3` があれば `VACUUM INTO`、無ければ現在のリリースの `celerisctl db backup`（rusqlite の
   backup API。ADR-0065 D2/D3 の新規サブコマンド）で新しい場所へコピーする。
4. 新しいファイルに `PRAGMA integrity_check`。`ok` でなければコピーを消して止まる
   （**旧 DB とサービスには触れない**）。
5. `config.toml` の `db =`（または `[db].path`）を書き換える。`config.toml.bak-<ts>` を残す。
6. 旧ファイルを `<旧パス>.moved-<ts>` に**リネーム**する（削除しない）。
7. unit を起こす。
8. `GET /api/v1/config`（認証つき）の `db` が新パスになっていることを確認する。**`GET /health` は
   無認証なので DB の絶対パスは出さない**（`db.filesystem` / `db.device` だけ出る。ADR-0065 D1）。

各段階の失敗は、その時点までに済んだことと**手で戻す手順**を stderr に出して exit 1 で止まる
（自動では戻さない）。冪等（`db` が既に指定パスなら何もせず exit 0）。ディレクトリは作らない
（無ければ「先に `sudo install -d` してください」と言って止まる）。

移した後は、`[db]` テーブルで `busy_timeout_ms`（本番では 15000 を勧める）・
`checkpoint_interval_secs`（既定 30）・`backup_dir`・`backup_interval_secs`（既定 3600）・
`backup_keep`（既定 48）も書ける（§8）。`backup_dir` を書くと、celeris がその間隔で
`celeris-<unix_ts>.sqlite3` を自分の背景タスクで書き、世代を`backup_keep`件だけ残す
（DB がローカルディスクにあると NFS 側のスナップショットに乗らなくなるため）。

## 6. いまを見る（`status.sh`）

```bash
scripts/selfdeploy/status.sh
```

JSON 1 つ。`current` / `previous`、`releases[]`（`gate`（各段の exit と秒数）、`verify`（`ok` / `live_ok` /
落ちた検査の名前）、`promoted_at` / `promoted`（`promoted.json`）、`promote_failed`（`promote_failed.json`。
直近の失敗が残っていれば `{failed_at, error}`、無ければ `null`）、`on_main`、`changes`（`base` / `stale` /
`commit_count` / `file_count` / `sensitive`））、本番 `:7710` の `/health`、`:7700` の `/healthz`、
`daemon_instances`、`backups` の新しい 10 件。**読むだけ**なので誰が実行してもよい
（`on_main` の git も `merge-base --is-ancestor` だけ。`$SD_REPO` に `main` が無ければ `null`）。

## 7. 禁止（ADR-0040 D5）

celeris の上の「人」（ワーカー）が自己改善の案件でやってよいのは **`release.sh` と `verify.sh` まで**。

やってはいけないこと:

- `promote.sh` / `rollback.sh` / `install-units.sh` を実行する（リリースの中の `<release>/scripts/*.sh` も同じ）
- `POST /releases/{sha12}/promote` を叩く（GUI の「昇格」ボタンと同じもの。押すのは人だけ）
- `systemctl` を叩く（本番の unit を start / stop / restart / enable / disable する）
- `~/.config/celeris/config.toml` を編集する
- `~/.local/celeris/*.sqlite3` に書き込む（読むのは `sqlite3 "file:…?mode=ro"` と `.backup` だけ）
- 本番のプロセスに `kill` などのシグナルを送る
- `127.0.0.1:7710` / `0.0.0.0:7700` に bind する
- `main` に直接コミットする（実装者は **celeris が用意した worktree とブランチ `celeris/<task-id>`** にコミットする。ADR-0041 D1）
- `git checkout` で作業ツリーのブランチを変える・自分でブランチを切る（作業ツリーは celeris がタスクごとに用意する）

実装者は `gate.json` / `verify.json` を `artifacts/` に写し、報告に「検証済み sha」を書く。
昇格は人が `status.sh` で `verify.json.ok` を見てから行う。

## 8. config.toml について

**Phase 46 / 48 / 50 とも設定に変えるところは無かった。Phase 58（改名）は置き場だけを変える。**
ADR-0045 D2 で「省略したときの既定」が新しい置き場になった:

| 設定 | 省略時の既定 |
|---|---|
| `db` | `~/.local/celeris/celeris.sqlite3` |
| `workspace_root` | `~/.local/celeris/workspaces` |
| `[selfdeploy] releases_dir` | `~/.local/celeris/releases` |
| `[memory] dir` | `~/.local/celeris/memory` |
| `[containers] build_dir` | `~/.local/celeris/containers` |
| `[secrets] dir` | `~/.config/celeris/secrets` |

ADR-0065 D1（Phase 110a）: `db` は文字列（従来どおり）でも、`[db]` テーブルでもよい
（`db = "<path>"` と `[db] path = "<path>"` は同じ意味）。テーブルにすると追加のキーが書ける:

| `[db]` のキー | 省略時の既定 |
|---|---|
| `path` | `~/.local/celeris/celeris.sqlite3`（`db = "..."` と同じ既定） |
| `busy_timeout_ms` | 5000（本番では 15000 を勧める。`relocate-db.sh` の後に書き足すとよい） |
| `checkpoint_interval_secs` | 30（背景チェックポイントの間隔。ADR-0065 D5） |
| `backup_dir` | 無し（無ければ定期バックアップをしない） |
| `backup_interval_secs` | 3600 |
| `backup_keep` | 48 |

**書いてあれば従来どおり**（相対パスは設定ファイルのディレクトリ基準）。`[api] token_file` と
`[accounts] claude_dir` / `codex_dir` には**暗黙の既定を入れない**: 「書いていない」こと自体が
「認証を使わない（ADR-0013 D3）」「そのプールを設定していない（ADR-0024 D2 / ADR-0025 D1）」という
意味を持っているため。推奨の置き場（`~/.config/celeris/api.token`、
`~/.local/celeris/{claude,codex}-accounts`）は `config/celeris.example.toml` に書いてあり、
移行スクリプトは本番の設定をそこへ書き換える。

Phase 50 で入った `[selfdeploy] repo` も既定が
`~/workspace/agent-platform`（`~` は celeris の `$HOME` で展開）なので、書かなければそのまま当たる
（読むのは `on_main` のためだけで、**書き換えることは無い**。無ければ `on_main` が `null` になるだけ）。
どちらも別の場所に置きたいときだけ書く。 `[handoff]`（`drain_timeout_secs` など）は
Phase 47 で celeris 本体に入るときに足す設定で、それまでは既定値（`drain_timeout_secs = 3600`）で動く。
`promote.sh` が読むのは既存の `kill_grace_secs` だけ（無ければ 10 秒）。
`verify.sh` は `config.toml` を**そのまま**新リリースに読ませる（本番の設定が新しいバイナリで通るかを
見るのが目的なので、上書きは CLI フラグだけ）。

## 9. 改名の移行（`migrate-to-celeris.sh`。一度だけ。人だけ）

Phase 58 / ADR-0045 D3。旧 `~/taskd/`（設定・DB・リリース・道具が 1 か所）を
`~/.config/celeris/`（設定と秘密）と `~/.local/celeris/`（状態）に分け、unit を
`celeris@` / `celeris-gui@` に替える。**停止 → 起動**なので、その間（数十秒）だけ API と GUI が止まる。

### 手順（この順に、人が実行する）

```bash
cd ~/workspace/agent-platform
git switch main && git pull --ff-only          # 改名後のコードを手元に

# 1. 新しい置き場でリリースを作る（本番には触れない。30〜60 分）
CELERIS_STATE_DIR=~/.local/celeris scripts/selfdeploy/release.sh main
#    → ~/.local/celeris/releases/<new>/bin/celeris ができる。`current` が無いので changes.base は null

# 2. 旧い置き場の設定と DB に対して検証する（本番には触れない）
CELERIS_STATE_DIR=~/.local/celeris \
CELERIS_CONFIG=~/taskd/taskd.toml \
CELERIS_DB=~/taskd/taskd.sqlite3 \
  scripts/selfdeploy/verify.sh <new sha12>
#    → verify.json.ok = true（live_ok は false。`current` が無いので N-1 検査は行われない）

# 3. 何が動くかを読む（何も変えない。設定の書き換え結果も全文出る）
scripts/selfdeploy/migrate-to-celeris.sh <new sha12> --dry-run | less
#    → 「unknown path(s)」で止まったら、その `<x>` を migrate-to-celeris.sh の TABLE に足してからやり直す

# 4. 移行する（停止 → 起動。ログは ~/.local/celeris/backups/migrate-<ts>.log）
scripts/selfdeploy/migrate-to-celeris.sh <new sha12>

# 5. 確かめる
curl -s http://127.0.0.1:7710/api/v1/health | python3 -m json.tool   # release = <new sha12>
curl -s http://127.0.0.1:7700/healthz | python3 -m json.tool         # name = "celeris-gui"
ls ~/.config/celeris ~/.local/celeris ; ls ~/taskd 2>&1              # ~/taskd は無い
scripts/selfdeploy/status.sh | head -20
```

以後の昇格は従来どおり `release.sh` → `verify.sh` → `promote.sh`（環境変数は要らない。既定が新しい置き場）。

### 何が起きるか

1. 旧 unit（`taskd@<old>` / `taskd-gui@<old>`）を止める。`SD_STOP_WAIT`（既定 300 秒）まで待ち、
   それでも生きていて API が閉じていれば先へ進む（実機 2026-09-19 の ext4 ジャーナル待ちの件。§4a と同じ規則）。
2. 同一ファイルシステムなので `mv` で移す（順序固定）。DB → `celeris.sqlite3*`、`releases/` の中身
   （`.build` / `.cargo-target` 込み）、`backups staging workspaces memory claude-accounts codex-accounts` →
   状態、`ldr paperqa opencode` → `tools/`、`api.token gui.password gui.session-secret org.toml providers.d/
   secrets/` → 設定、`*.log` → `logs/`、`*.bak-*` → `backups/pre-celeris/`。
3. 設定を**決定的な対応表**で書き換えて `~/.config/celeris/config.toml`（0600）に置く。
   旧い絶対パス（`/home/…/taskd/<x>`）も `~/taskd/<x>` も、今日 `~/taskd` の中に解決している**裸の相対値**
   （`db = "taskd.sqlite3"`、`workspace_root = "workspaces"`、`token_file`、`org_include`、
   `providers_include`、`[secrets] dir`、`[memory] dir`、`[accounts]` の 2 つ、`[selfdeploy] releases_dir` /
   `repo`）も絶対パスにする。**表に無いパスが 1 つでも残っていたら、何も動かさずに止まる。**
   最後に残った旧い名前（コメント・役割の指示文・`TASKD_*`）も `celeris` / `CELERIS_*` に改める。
   元のファイルは `~/.local/celeris/backups/pre-celeris/taskd.toml` に残る。
4. `install-units.sh --remove-old` で新しいテンプレートを入れ、旧テンプレートを消す
   （消す前に `backups/pre-celeris/units/` へ写す）。
5. `celeris@<new>` を起こし、`/health` が `release = <new>` / `schema_version` が期待どおりになるまで
   90 秒待つ → `celeris-gui@<new>` → `/healthz.name == "celeris-gui"` → `enable` 新 / `disable` 旧 →
   `current -> releases/<new>`、`previous -> releases/<old>`（旧リリースの実行ファイルは旧い名前のまま。
   `verify.sh` の検査 5 はその両方を見る）。
6. `~/taskd` が空なら消す（互換のシンボリックリンクは作らない。ADR-0045 D2）。
   `promoted.json` に `mode = "migrate"` を書く。

### 戻す

```bash
scripts/selfdeploy/migrate-to-celeris.sh --rollback
```

新 unit を止め、ディレクトリを逆に移し、`backups/pre-celeris/taskd.toml` と旧テンプレート unit を戻して
`taskd@<previous>` を起こす。DB は **schema が変わっていない**ので、そのまま読める
（`SCHEMA_VERSION` は据え置き。ADR-0045 D4）。


## 部署レビューから自動で候補を準備する（ADR-0051）

`[selfdeploy] delivery_projects = ["案件ID"]` を指定してreloadすると、
その案件の登録済み自己リポジトリに対する次のReviewer runでマージ可否も判定する。
部署長の別runが既存レビューを担当するため、CoSに技術的な再承認は求めない。
合格した固定SHAだけをfast-forwardし、release/verify成功後にCoSからデプロイ候補を通知する。
本番昇格は引き続きリリース画面から行う。空配列が既定（手動取り込み）。

完了済みの仕事を引き渡すには、管理API `POST /tasks/{id}/rereview` に
`{"expected_status":"done"}` を送る。実装をやり直さず、現在のコミットを再判定する。
競合・承認後の変更は部署へ差し戻し、既存の再開・修正経路を使う。マージ・ビルドの技術的失敗は一度だけ自動で実装担当へ戻し、同じタスクで無限に修正を繰り返さない。
準備のログは `<releases_dir>/.deliveries/<task-id>/<sha>/prepare.log` に残る。
