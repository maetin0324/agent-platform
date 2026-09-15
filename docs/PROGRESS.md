# taskd-gui 進捗

設計は `docs/DESIGN.md`（§10 にフェーズと受け入れ条件）、taskd の API は `docs/taskd-api-v1.md`。各フェーズの完了時にこのファイルへ `## Phase G<N> — DONE` の節を追加する。
`run-gphases.sh` はこのファイルの `## Phase G<N> — DONE` / `BLOCKED` / `PARTIAL` を見て進む。

## 現在地

| フェーズ | 内容 | 状態 | 完了日 |
|---|---|---|---|
| G0 | 骨組みと前提の確定 | **DONE** | 2026-09-15 |
| G1 | 読み取りとストリーム | 未着手 | — |
| G2 | 操作 | 未着手 | — |
| G3 | ログ・成果物・DAG | 未着手 | — |
| G4 | プロバイダとデーモン | 未着手 | — |
| G5 | 認証・配布・仕上げ | 未着手 | — |

前提: taskd（`$TASKD_REPO`、既定 `../agent-platform`）の Phase 9a / 9b（`docs/adr/0013`）が完了していること。G0 の受け入れ条件 2 で確認する。

## 引き継ぎ（前のフェーズから）

（なし）

## 提案（`docs/DESIGN.md` / `docs/taskd-api-v1.md` への変更提案。採否は人間）

- G0-P1: `docs/DESIGN.md` §0 / §10 の「React 19.3」は「React 19.2 以上（cooldown 7 日を満たす最新）」と読み替えた（ADR-0003 D2）。文言を「19.2+」にすると実態と合う。
- G0-P2: `docs/DESIGN.md` §10 Phase G0 の「shadcn/ui（`-t react-router`）」: `shadcn init -t react-router` は新規プロジェクト生成用で、既存プロジェクトには `components.json` を置くだけでよい。G0 の記述を「`components.json` と `lib/utils.ts` を置く」に緩めるとよい（ADR-0003 D8）。

## taskd への依頼（`docs/taskd-requests.md` の要約）

（なし）

## 節の書式（各フェーズで使う）

```
## Phase G<N> — DONE（YYYY-MM-DD）

### 成果物
- 追加・変更したファイルと要点

### 受け入れ条件と証拠
1. **<条件>** — コマンド（または Playwright の操作）と出力の要点（exit code、テスト数、表示された文字列、差分ゼロ）
2. ...

### 共通条件
- `pnpm lint` / `pnpm typecheck` / `pnpm test`（N passed）/ `pnpm build` / `pnpm e2e`（N passed）
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts` 差分ゼロ

### 監査結果
- auditor の判定と、指摘への対応

### 未解決事項
### 提案
### taskd への依頼
```

## Phase G0 — DONE（2026-09-15）

### 成果物
- 雛形: `package.json`（版は完全固定、`packageManager: pnpm@11.27.0`、Node >= 24）、`pnpm-workspace.yaml`（`minimumReleaseAge: 10080`、`strictDepBuilds: true`）、`pnpm-lock.yaml`、
  `tsconfig.{json,node.json,vite.json}`（TypeScript 7.0.2、`tsc -b`）、`biome.json`（`noDangerouslySetInnerHtml: error`、`noConsole`、生成物 `app/taskd/types.ts` は対象外）、
  `react-router.config.ts`（SSR）、`vite.config.ts`（Tailwind 4 + React Router）、`vitest.config.ts`、`playwright.config.ts`（chromium のみ、webServer = `pnpm build && node server.js`）、`components.json` + `app/lib/utils.ts`（shadcn/ui。部品は G1 以降）
- サーバ: `server.js`（`TASKD_GUI_BIND` の検証、非 loopback は G5 まで exit 2、`/assets` immutable、stderr に JSON 1 行の要求ログ）、`server/app.ts`（`@react-router/express`）
- アプリ: `app/routes.ts`（明示的定義: `/` = `routes/inbox.tsx`、`/healthz` = `routes/healthz.ts`）、`app/root.tsx`（root middleware の登録、`GET /health` の loader、taskd 停止時バナー + 5 秒ごとの再検証、フッタに GUI / taskd の版）、
  `app/middleware/security.server.ts`（`Host` 検査 → 400、CSP nonce、セキュリティヘッダ）、`app/entry.server.tsx`（nonce を React と `<Scripts>` へ）、`app/context.ts` / `app/nonce.ts`、`app/config.server.ts`
- taskd クライアント: `app/taskd/client.server.ts`（`TaskdClient`: `get` / `post` / `stream` / `file` / `health`、`fromEnv`、Bearer、タイムアウト、problem+json → `TaskdError`、接続失敗 → `TaskdUnavailable`）、`app/taskd/errors.ts`、`app/taskd/health.server.ts`（`loadHealth`）、`app/taskd/types.ts`（生成物、897 行）
- 型生成: `scripts/gen-types.mjs`（`pnpm gen:types`。`$TASKD_REPO/docs/api/v1/api-v1.schema.json` → `json2ts --additionalProperties=false`）
- taskd 補助: `scripts/taskd.sh`（build / start / stop / status / logs / taskctl / fixture の骨組み）、`test/taskd/taskd.toml.tmpl`（fake ワーカー、`tick_ms = 200`、`retry_backoff_base_secs = 0`、`[plan] auto_accept = false`、`[api] listen = "127.0.0.1:7710"`）、`test/taskd/fake-worker.sh`
- テスト: `test/mock-taskd/{server.ts,fixtures.ts}`（プロセス内の偽 taskd。problem+json / SSE のヘルパ）、`test/unit/client.test.ts`（15 件）、`e2e/g0.spec.ts`（5 シナリオ）
- 文書: `docs/adr/0003-g0-scaffold-decisions.md`（TS 7 採用、cooldown と版固定の両立、middleware の置き場、taskd 停止時の loader の規約、非 loopback 拒否、taskd.sh の形、型生成、shadcn）

### 受け入れ条件と証拠
1. **`pnpm install --frozen-lockfile` / `pnpm lint` / `pnpm typecheck` / `pnpm test` / `pnpm build` が exit 0** —
   `pnpm install` exit 0（`Done in 5m 36.3s using pnpm v11.27.0`、`pnpm-lock.yaml` をコミット）。`pnpm lint` → `Checked 32 files in 30ms. No fixes applied.` exit 0。
   `pnpm typecheck`（`react-router typegen && tsc -b`）→ exit 0。`pnpm test` → `Test Files 1 passed (1)`、`Tests 15 passed (15)` exit 0。`pnpm build` → `✓ built in 874ms` / `✓ built in 464ms` exit 0
2. **`scripts/taskd.sh build && scripts/taskd.sh start dev` → `/health` の `api_version` が `1`、スキーマファイルが存在** —
   `cargo build -p taskd -p taskctl` → `Finished dev profile in 1m 42s` exit 0。`start dev` → `taskd 'dev' started (pid …, api http://127.0.0.1:7710)`。
   `curl -s http://127.0.0.1:7710/api/v1/health` → `{"api_version":"1","schema_version":4,"taskd_version":"0.1.0","instance_id":"01M2HD…","db":{"journal_mode":"wal","busy_timeout_ms":5000}}`（`jq -r .api_version` = `1`）。
   `test -f "$TASKD_REPO/docs/api/v1/api-v1.schema.json"` → exit 0
3. **`pnpm gen:types && git diff --exit-code app/taskd/types.ts` が差分ゼロ、対象 4 型の `export interface`** — コミット後に再実行して差分ゼロ（exit 0）。
   `grep -c "export interface \(TaskDetail\|Inbox\|EventRow\|DaemonSnapshot\)" app/taskd/types.ts` → `5`（`TaskDetail` / `Inbox` / `EventRow` / `DaemonSnapshot` の 4 つに加え `InboxCounts` が正規表現に一致。4 型は全て存在）
4. **`/` に `taskd_version` / `api_version 1` / `schema_version`、taskd 停止時は 200 で「taskd に接続できません」** —
   Playwright `e2e/g0.spec.ts` シナリオ 1: `data-testid` の `taskd_version` = `0.1.0`、`api_version` = `1`、`schema_version` = `4` が実 taskd の `/health` と一致、フッタに `api_version 1`。
   シナリオ 2: `scripts/taskd.sh stop dev` 後の `page.goto("/")` が status **200**、`[data-testid=taskd-banner]` に「taskd に接続できません」、`start dev` 後にリロード無しで 6.0 秒でバナー消失。
   curl でも確認: 停止中の `/` が 200 で本文に `taskd に接続できません（http://127.0.0.1:7710）`
5. **`Host: evil.example` が 400** — `curl -s -o /dev/null -w '%{http_code}' -H 'Host: evil.example' http://127.0.0.1:7700/` → `400`。Playwright シナリオ 3 でも 400（正しい Host は 200）
6. **TaskdClient の単体テスト** — `test/unit/client.test.ts`: 409 `conflict` の problem+json → `TaskdError{status: 409, code: "conflict", extra.expected/actual}`、接続拒否 → `TaskdUnavailable{baseUrl}`、他にタイムアウト・422・非 JSON 500・stream / file のヘッダ転送・`loadHealth` の 3 状態。`pnpm test` 15 passed

### 共通条件
- `pnpm lint` exit 0 / `pnpm typecheck` exit 0 / `pnpm test` 15 passed / `pnpm build` exit 0 / `pnpm e2e` **5 passed (13.1s)**（実 taskd `dev` に対して。G0 では任意だが実施）
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts` 差分ゼロ（exit 0）
- 手動確認: `/` の応答ヘッダに `content-security-policy: default-src 'self'; script-src 'self' 'nonce-…'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'`、`x-content-type-options: nosniff`、`referrer-policy: no-referrer`、`cache-control: no-store`、`x-frame-options: DENY`。HTML の全 `<script>` に nonce

### 監査結果
- auditor の判定: **条件付き可**（「不可」ゼロ）。受け入れ条件 1〜6 は全て「満たしている」（auditor 自身が `pnpm install --frozen-lockfile --offline` / lint / typecheck / test 15 passed / build / gen:types 差分ゼロ / `pnpm e2e` 5 passed / curl を実行して確認）。
  禁止事項（SQLite、仕様外フィールド、派生値の再計算、ブラウザからの直接呼び出し、トークン露出、`dangerouslySetInnerHTML` / CDN、テストの外部ネットワーク、版固定）は「明確な違反なし」。
  トークン露出は `TASKD_API_TOKEN_FILE` にダミーを置いて `/` の HTML と stderr ログに 0 件であることを確認済み。
- 条件と対応:
  1. PROGRESS に監査結果を書いてコミットし `git status` をクリーンにする → 本コミットで対応。
  2. 「設計との差 1」（Host 検査とセキュリティヘッダが `express.static` の静的アセットと React Router の未定義パス 404 に掛からない）→ 下の未解決事項に記載。G5 の受け入れ条件 3（全ページに CSP）までに Express 層か catch-all ルートで解消する。
  3. 内側の Express（`server/app.ts`）で `x-powered-by` が無効化されていない → **修正済み**（`app.disable("x-powered-by")` を追加。`curl -sI /` で `X-Powered-By` 0 件）。
  4. 要求ログの `path` が静的配信でマウント相対になる → **修正済み**（`req.originalUrl` を使う。`/assets/...` と記録されることを確認）。
- 修正後の再検証（自分で実行）: `pnpm lint` exit 0、`pnpm typecheck` exit 0、`pnpm test` 15 passed、`pnpm build` exit 0、`pnpm e2e` 5 passed (13.8s)。

### 未解決事項
- **Host 検査とセキュリティヘッダの適用範囲**（監査指摘）: `server.js` の `express.static` が React Router のハンドラより前にあるため、`/assets/*` は `Host: evil.example` でも 200 で、CSP 等のヘッダも付かない。ルート未定義のパス（例 `/nope`）は React Router が middleware を通さず 404 を返すため `Host` 偽装でも 404（400 にならず、CSP も無い）。
  G0 の条件 5（`/` が 400）は満たすが、G5 の条件 3「全ページの応答に CSP」までに Express 層（`server.js`）での Host 検査 + ヘッダ付与、または catch-all ルート（`route("*")`）で解消する。G1 の `/events` はルートツリー内に置き root middleware が掛かるようにする。
- `pnpm dev`（Vite 開発サーバ）では CSP の `script-src` が `'unsafe-inline'`（React Refresh の inline script のため）。本番・e2e は nonce 付き。G5 で外せるか確認する（ADR-0003 D3）
- `strictDepBuilds: true` で `allowBuilds` は空のまま install が通った（build script を要求する依存が無い）。依存追加時に必要になれば許可リストに足す
- `@testing-library/react` は未導入（G0 のテストは loader 関数と HTTP クライアントのみ）。コンポーネントテストが要る G1 で入れる
- shadcn/ui は `components.json` と `cn` だけ（`pnpm dlx shadcn init` はネットワーク取得が 150 秒で終わらず断念。CLI が必要になる `shadcn add` は G1 で再試行）
- `scripts/taskd.sh fixture` はシナリオ未定義（G1 で `basic` を追加）
- 新しい依存（ADR-0002 の一覧外）: `clsx` 2.1.1、`tailwind-merge` 3.6.0（shadcn/ui の `cn` に必要。どちらも公開 7 日以上）

### 提案
- 上の「提案」節の G0-P1（React 19.2 以上の読み替え）、G0-P2（shadcn の初期化方法）

### taskd への依頼
- なし（`GET /health`、Host 検査、problem+json の形は `docs/taskd-api-v1.md` §1.4 / §1.5 / §3.1 のとおりだった）
