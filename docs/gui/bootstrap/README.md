# GUI（`gui/`）の立ち上げ（bootstrap）

オーケストレータが `run-gphases.sh` で `gui/`（ADR-0020 以降は **この celeris リポジトリの中**。既定 `$CELERIS_REPO/gui`）を作り、
本ディレクトリと `docs/gui/` のファイルをコピーしてから、G フェーズを `claude -p "/goal …"` で 1 フェーズずつ回す。GUI の設計は `docs/gui/DESIGN-GUI.md`、celeris の API は `docs/gui/api.md`。

## 1. コピー先の対応

| コピー元（celeris 側） | コピー先（`gui/`） | 備考 |
|---|---|---|
| `docs/gui/DESIGN-GUI.md` | `docs/DESIGN.md` | GUI の設計。§10 がフェーズと受け入れ条件 |
| `docs/gui/api.md` | `docs/celeris-api-v1.md` | celeris HTTP API v1 の仕様（GUI から見た契約）。**以後も `scripts/sync-gui-docs.sh` で一方向に同期する**（ADR-0020 D4） |
| `docs/gui/adr/0001-architecture-boundary.md` | `docs/adr/0001-architecture-boundary.md` | |
| `docs/gui/adr/0002-frontend-stack.md` | `docs/adr/0002-frontend-stack.md` | |
| `docs/gui/celeris-proposals.md` | `docs/celeris-proposals.md` | 参考（採否の履歴） |
| `docs/gui/bootstrap/CLAUDE.md` | `CLAUDE.md` | 運用ルール |
| `docs/gui/bootstrap/GOAL_TEMPLATE.md` | `docs/GOAL_TEMPLATE.md` | `__N__` / `__MAXTURNS__` を置換して `/goal` に渡す |
| `docs/gui/bootstrap/PROGRESS.md` | `docs/PROGRESS.md` | 初期状態（G0 未着手） |
| `docs/gui/bootstrap/agents/implementer.md` | `.claude/agents/implementer.md` | サブエージェント（TS 版） |
| `docs/gui/bootstrap/agents/auditor.md` | `.claude/agents/auditor.md` | 同上 |
| （新規作成） | `docs/celeris-requests.md` | 空ファイル（見出しだけ）。BLOCKED 時に追記される |
| （新規作成） | `.gitignore` | `node_modules/`、`build/`、`.run/`、`dist/`、`test-results/`、`playwright-report/`、`.react-router/` |

コピー後に `git init && git add -A && git commit -m "bootstrap: design, api spec, rules"` で初期コミットを作る（G0 は `git status` クリーンから始める）。
`docs/DESIGN.md` 内のリンク（`adr/...`、`api.md`、`bootstrap/...`）はコピー先の相対パスに合わせて置換する:
`api.md` → `celeris-api-v1.md`、`bootstrap/README.md` → （削除または本ファイルへの参照を外す）。

## 2. `run-gphases.sh` が読むフェーズ表（機械的に読める形）

1 行 = `<phase> <model> <maxturns>`。`model` は `strong` / `light`（`STRONG_MODEL` / `LIGHT_MODEL` に対応）。

```
G0 strong 60
G1 light 50
G2 strong 60
G3 light 50
G4 light 40
G5 strong 60
```

`run-phases.sh` からの差分（要点）:

- `PHASES="${PHASES:-G0 G1 G2 G3 G4 G5}"`、上の表を `model_for` / `maxturns_for` で引く（`case "$1" in G0|G2|G5) …`）。
- `GOAL="$(sed -e "s/__N__/${N#G}/g" -e "s/__MAXTURNS__/$(maxturns_for "$N")/g" docs/GOAL_TEMPLATE.md)"`（テンプレートの見出しは `Phase G__N__` なので **数字だけ**を入れる）。
- `phase_done()    { grep -q  "^## Phase $1 — DONE" docs/PROGRESS.md; }`、`phase_stopped()` は `BLOCKED|PARTIAL`（`$1` は `G0` 等）。
- `cd $CELERIS_REPO/gui`（`CELERIS_REPO` は celeris リポジトリの根。`gen:types` は省略時に `gui/` の親を見る）。
- 各フェーズの前に `scripts/celeris.sh stop dev >/dev/null 2>&1 || true`（前回の celeris が残っていたら止める。G0 の前はスクリプトが無いので無視）。
- `CLAUDE_CODE_SUBAGENT_MODEL` は celeris と同じ既定（sonnet）。implementer は frontmatter で sonnet、auditor は opus。

## 3. 前提ツール（オーケストレータのホストに必要なもの）

| ツール | 版 | 確認コマンド | 備考 |
|---|---|---|---|
| Node.js | **24 LTS**（24.21.0 以上）。最低 22.22.0（React Router 8 の `engines`） | `node --version` | 開発機は `v22.21.0` で**不足**。`nvm install 24` 等で更新する |
| pnpm | 11.x（11.26.0 以上） | `pnpm --version` | 開発機に未導入。`npm install -g pnpm@11`（corepack は使わない）。`package.json` の `packageManager` で固定 |
| cargo / rustc | stable（celeris は 1.94、edition 2024） | `cargo --version` | `scripts/celeris.sh build` が `cargo build -p celeris -p celerisctl` を実行する |
| celeris | Phase 9a / 9b 完了（`docs/adr/0013`） | `test -f "$CELERIS_REPO/docs/api/v1/api-v1.schema.json"` | 無ければ G0 は BLOCKED |
| Playwright の chromium | 1.63 系 | `pnpm exec playwright install chromium`（G0 が実行） | ネットワークが要る準備作業。テスト中は不要 |
| jq / curl | 任意 | | 受け入れ条件の確認に使う |
| sqlite3 CLI | 不要 | | GUI は DB を開かない |

環境変数: `CELERIS_REPO`（既定 `../agent-platform`）、`CELERIS_API_URL`（既定 `http://127.0.0.1:7710`）、`CELERIS_API_TOKEN_FILE`（G5）、`CELERIS_GUI_BIND`（既定 `127.0.0.1:7700`）、
`CELERIS_GUI_PASSWORD_FILE` / `CELERIS_GUI_ALLOWED_HOSTS` / `CELERIS_GUI_SESSION_SECRET_FILE`（G5）。

## 4. G フェーズの一覧（詳細は `docs/DESIGN.md` §10）

| フェーズ | 内容 | モデル | 最大ターン |
|---|---|---|---|
| G0 | 雛形（React Router 8 framework mode、Biome、Vitest、Playwright、Tailwind、shadcn）、`scripts/celeris.sh`、型生成、`CelerisClient`、Host 検査、celeris 停止時のバナー | strong | 60 |
| G1 | fixture `basic`、受信箱、一覧（ページング）、詳細、タイムライン、SSE 中継と再検証 | light | 50 |
| G2 | approve / reject / answer / cancel / 作成 / Plan / replay の action、CSRF、409 / 422 の表示 | strong | 60 |
| G3 | ログビューア（stream-json 整形、追尾）、成果物ビューア、sha256 警告、DAG | light | 50 |
| G4 | プロバイダ画面、デーモン画面、停止 / 復旧のバナー、multi-account / unroutable の fixture | light | 40 |
| G5 | BFF のトークン、非 loopback のパスワード認証、CSP、a11y、`server.js`、systemd、tar.gz、（任意）Docker、（実験）Node SEA | strong | 60 |

## 5. 止まる条件

- `## Phase G<N> — BLOCKED`: celeris の API が足りない / 仕様と違う（`docs/celeris-requests.md` に内容）、または同じ失敗が 3 回 + 別案 1 回失敗。オーケストレータが celeris 側を直すか判断してから再開する。
- `## Phase G<N> — PARTIAL`: 最大ターン数に達した。`--continue` で同じフェーズを再開する（`run-phases.sh` と同じ）。
