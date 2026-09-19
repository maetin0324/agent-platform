# ADR-0045: 全面改名 — バイナリ `celeris` / `celerisctl`、unit `celeris@`、設定 `~/.config/celeris/`、状態 `~/.local/celeris/`

- 日付: 2026-09-20
- 状態: **Accepted**（人間の指示「名前変更をお願いします」。ADR-0042 D1 で「機能が本番で動いた後に改名だけの Phase を切る」とした
  もの。ADR-0043 / 0044 の全 Phase が本番で動いた直後に着手）
- 関連: ADR-0042（名前とパス）、ADR-0040（リリース・昇格）、ADR-0017（管理系 API）、ADR-0024 / 0025（アカウント）、ADR-0030（秘密）

## 1. 決定

### D1. 名前

| いま | これから | 備考 |
|---|---|---|
| バイナリ / crate `taskd`（`crates/taskd`） | **`celeris`**（`crates/celeris`） | ログの target 名も自動で変わる |
| バイナリ / crate `taskctl`（`crates/taskctl`） | **`celerisctl`**（`crates/celerisctl`） | |
| ライブラリ crate `task-core` / `task-ops` / `task-dispatch` / `task-worker` / `task-api` | **そのまま** | 内部名。改名は差分ばかりで価値が無い（ADR-0042 D1 の「crate 名」はこの 2 つの実行ファイルを指すと決める） |
| systemd `taskd@.service` / `taskd-gui@.service` | **`celeris@.service` / `celeris-gui@.service`** | `%i` = sha12 は同じ |
| GUI パッケージ `taskd-gui`、`/healthz.name` | **`celeris-gui`** | |
| 環境変数 `TASKD_*`（`TASKD_API_URL`、`TASKD_API_TOKEN_FILE`、`TASKD_GUI_*`、`TASKD_RELEASE`、`TASKD_HOME` …） | **`CELERIS_*`** | 互換の読み替えはしない（unit と文書を同時に直す） |
| 問題型 `urn:taskd:problem:*` / `urn:taskd:request:*`、`WWW-Authenticate: Bearer realm="taskd"` | **`urn:celeris:…`、`realm="celeris"`** | GUI が唯一のクライアント。同時に直す |
| Discord の `username: "taskd"` | **`Celeris`** | |
| `docs/gui/api.md` の写し `gui/docs/taskd-api-v1.md` | **`gui/docs/celeris-api-v1.md`** | `scripts/sync-gui-docs.sh` も |
| `config/taskd.example.toml` | **`config/celeris.example.toml`** | |
| リポジトリ名 `agent-platform`、`~/workspace/agent-platform` | **そのまま** | GitHub の改名は人の判断。設定・文書からの参照が多いので今回は触らない |

### D2. パス（XDG 流）

```
~/.config/celeris/                 # 設定と秘密（0700）
  config.toml                      # 旧 ~/taskd/taskd.toml。`celeris --config` の既定値
  org.toml  providers.d/  api.token  gui.password  gui.session-secret  secrets/
~/.local/celeris/                  # 状態
  celeris.sqlite3 (+ -wal / -shm)  # 旧 taskd.sqlite3
  releases/  backups/  staging/  workspaces/  containers/  memory/
  claude-accounts/  codex-accounts/  logs/
  tools/ldr  tools/paperqa  tools/opencode   # 旧 ~/taskd/{ldr,paperqa,opencode}
~/.config/systemd/user/celeris@.service, celeris-gui@.service
```

- 設定の相対パスは従来どおり**設定ファイルのディレクトリ基準**。省略時の既定値を新しい置き場に変える:
  `db` → `~/.local/celeris/celeris.sqlite3`、`workspace_root` → `~/.local/celeris/workspaces`（ADR-0042 D3 のまま）、
  `[selfdeploy] releases_dir` → `~/.local/celeris/releases`、`[memory] dir` → `~/.local/celeris/memory`、
  `[accounts] claude_dir / codex_dir` → `~/.local/celeris/{claude,codex}-accounts`、`[containers] build_dir` → `~/.local/celeris/containers`、
  `[secrets] dir` → `~/.config/celeris/secrets`、`[api] token_file` → `~/.config/celeris/api.token`。
- selfdeploy スクリプトの `TASKD_HOME`（既定 `~/taskd`）は **`CELERIS_CONFIG_DIR`**（既定 `~/.config/celeris`）と
  **`CELERIS_STATE_DIR`**（既定 `~/.local/celeris`）に分ける。`SD_CONFIG` = `$CELERIS_CONFIG_DIR/config.toml`、
  `SD_DB` は設定から読む（`db =` の値。無ければ既定）。
- `~/taskd/` は移行後に**残さない**（空になったら消す。互換のシンボリックリンクも作らない。文書とメモリを直す）。

### D3. 移行は一度だけの脚本 `scripts/selfdeploy/migrate-to-celeris.sh`（停止→起動。人が実行）

前提: 旧 `taskd@<old>` / `taskd-gui@<old>` が systemd で動いている（ADR-0040 以降の形）。`--dry-run` で計画だけ出す。

1. **改名後のコードでリリースを作る**（人が先に `CELERIS_STATE_DIR=~/.local/celeris scripts/selfdeploy/release.sh main`。
   `~/.local/celeris/releases/<new>/bin/celeris`。`current` が無いので `changes.json.base` は null）。
   `verify.sh <new>` は **`CELERIS_DB=~/taskd/taskd.sqlite3`** で旧 DB のスナップショットを取れるようにする（環境変数で DB の場所を上書き）。
   N-1（検査 5）は `current` 無しで偽 → 停止→起動。
2. 脚本本体（`migrate-to-celeris.sh <new sha12>`）:
   - 旧 unit を止める（`systemctl --user stop taskd-gui@<old> taskd@<old>`。`SD_STOP_WAIT` と同じく最大 300 秒、API が閉じたら先へ）。
   - **移す**（同一ファイルシステムなので `mv`。順序固定）: `taskd.sqlite3*` → `celeris.sqlite3*`、`releases/`（新しいものと同居。
     `.cargo-target` / `.build` も）、`backups/ staging/ workspaces/ memory/ claude-accounts/ codex-accounts/` → 状態、
     `ldr/ paperqa/ opencode/` → `tools/`、`api.token gui.password gui.session-secret org.toml providers.d/ secrets/` → 設定、
     `taskd.log daemon.log gui.log` → `logs/`、`taskd.toml.bak-*` / `taskd.sqlite3.bak-*` → `backups/pre-celeris/`。
   - **設定を書く**: `taskd.toml` → `~/.config/celeris/config.toml`。`/home/rmaeda/taskd/<x>` を上の表で新しい絶対パスに
     置き換える（決定的な対応表。知らない `<x>` が残っていたら止まる）。元は `backups/pre-celeris/taskd.toml` に残す。
   - unit を入れる（`install-units.sh`。`celeris@.service` / `celeris-gui@.service`。旧テンプレートは消す）。
   - `systemctl --user start celeris@<new>` → health 200（`release = <new>`、schema 不変）→ `celeris-gui@<new>` →
     `/healthz.name = celeris-gui` → `enable` 新 / `disable` 旧 → `current -> releases/<new>`、`previous -> releases/<old>`。
   - `~/taskd` が空なら消す。`promoted.json` を書く（`mode = "migrate"`）。
3. **戻し** `migrate-to-celeris.sh --rollback`: 新 unit を止め、ディレクトリを逆に移し、`backups/pre-celeris/taskd.toml` を戻し、
   旧テンプレート unit（脚本が `backups/pre-celeris/units/` に写しておく）を入れて `taskd@<old>` を起こす。DB は schema が変わって
   いないのでそのまま使える。
4. 以後の昇格は従来の `promote.sh`（新しい名前とパスで）。

### D4. 互換

- `TASKD_*` の読み替え、`~/taskd/taskd.toml` の既定パス、`urn:taskd` の受理は**残さない**。動いているものは unit と GUI だけで、
  どちらもこの Phase で直す。
- DB のスキーマ・表名・イベント名は変えない（`SCHEMA_VERSION` 据え置き）。ワーカープロトコルも変えない。
- `.taskd/artifacts/` の読み取り互換（ADR-0042 D2）はそのまま。

### D5. 文書

- `CLAUDE.md`（「agent-platform / Celeris」、既定の実行ファイル名）、`docs/selfdeploy.md`、`docs/workspace.md`、`docs/gui/api.md`、
  `gui/README.md`、`config/celeris.example.toml`、`deploy/systemd/`、`docs/PROGRESS.md`。
- 過去の ADR と PROGRESS の本文は**書き換えない**（歴史）。`docs/DESIGN.md` / `docs/SPEC.md` も触らない。

## 2. 採らない

- ライブラリ crate の改名（D1）。
- GitHub リポジトリ名の変更（人の判断）。
- `TASKD_*` → `CELERIS_*` の両対応期間。
- ライブ引き継ぎでの移行（unit 名・パスが変わるので停止→起動が素直。数十秒）。

## 3. 受け入れ条件（Phase 58）

1. `cargo build` で `target/*/celeris` と `celerisctl` ができ、`taskd` / `taskctl` は無い。`grep -rIw taskd` が crates / gui/app / scripts /
   deploy / config に**残らない**（ADR・PROGRESS・DESIGN・SPEC・migration SQL の歴史的な記述と、`.taskd/artifacts` の互換コードだけ例外）。
2. `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / GUI 一式が緑。e2e の実行ファイル名が新しい名前。
3. `release.sh` / `verify.sh` / `promote.sh` / `rollback.sh` / `status.sh` / `install-units.sh` が `CELERIS_CONFIG_DIR` / `CELERIS_STATE_DIR`
   で動く。`verify.sh` が `CELERIS_DB` を受ける。`migrate-to-celeris.sh --dry-run` が実機の `~/taskd` に対して**移動の計画と設定の
   置換結果**を出し、知らないパスが無いことを示す。
4. 実機（人）: `release.sh main` → `verify.sh` → `migrate-to-celeris.sh <sha12>` → health が `release = <sha12>`、`~/.config/celeris/config.toml`
   と `~/.local/celeris/` ができ、`~/taskd` が無く、GUI が `celeris-gui` を返し、その後 `promote.sh` で 1 回ライブ昇格できる。
