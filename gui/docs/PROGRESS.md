# taskd-gui 進捗

設計は `docs/DESIGN.md`（§10 にフェーズと受け入れ条件）、taskd の API は `docs/taskd-api-v1.md`。各フェーズの完了時にこのファイルへ `## Phase G<N> — DONE` の節を追加する。
`run-gphases.sh` はこのファイルの `## Phase G<N> — DONE` / `BLOCKED` / `PARTIAL` を見て進む。

## 現在地

| フェーズ | 内容 | 状態 | 完了日 |
|---|---|---|---|
| G0 | 骨組みと前提の確定 | **DONE** | 2026-09-15 |
| G1 | 読み取りとストリーム | **DONE** | 2026-09-15 |
| G2 | 操作 | **DONE** | 2026-09-15 |
| G3 | ログ・成果物・DAG | **DONE** | 2026-09-15 |
| G4 | プロバイダとデーモン | **DONE** | 2026-09-15 |
| G5 | 認証・配布・仕上げ | **DONE** | 2026-09-15 |
| G6 | 使い方ページ | **DONE** | 2026-09-16 |
| G7 | クラスタと委譲の表示 | **DONE** | 2026-09-16 |
| G8 | プロバイダの登録と Claude アカウント（プール・ログイン・残量）の画面 | **DONE** | 2026-09-16 |
| G9 | codex アカウント（アダプタ選択・デバイス認証）と run の account 列 | **DONE** | 2026-09-17 |
| G10 | 分野（genre）と能力レジストリの表示（taskd Phase 16〜18 / ADR-0027・0028） | **DONE** | 2026-09-17 |

前提: taskd（`$TASKD_REPO`、既定 `../agent-platform`）の Phase 9a / 9b（`docs/adr/0013`）が完了していること。G0 の受け入れ条件 2 で確認する。

## 引き継ぎ（前のフェーズから）

G7 完了時点で次フェーズ（あれば）に引き継ぐもの: G7-U1（`TaskSummary`/`GraphNode` に `role` が無く、一覧・DAG のノードへの役割ラベル表示は
未実装 → **2026-09-16 解消**）、G6-P1（プロバイダ管理 UI → **2026-09-16「作らない」で決着**。taskd 側 ADR-0022 D1。
一人で使い、信頼されたネットワークで localhost に閉じる前提のため、管理操作は `curl` と設定ファイルの直接編集で行う）、G3-U1（`/graph` スクリーンショットの環境依存）、G4-U1〜U4、G5-U1〜U8（Node SEA の残り、CSP の `style-src`、
`docker build` 未実行）。以下は G1 からの引き継ぎ（記録のため残す）:
- **SSE 常時再検証の負荷**（G1-U1）: `daemon` が tick ごとに届くため、画面を開いている間 taskd への要求がタブあたり毎秒約 7 回発生する。G2 で操作（承認・却下等）を増やすと相対的に無視できるが、G4（デーモン画面・プロバイダ画面）で複数タブを想定するなら再検討が要る。
- **仮想スクロールと一覧の行数一致テスト**（G1-U2）: `/tasks` の一覧は `@tanstack/react-virtual` で可視領域だけ DOM に出すため、SSR 直後の HTML には `task-row` が 0 件。件数が可視範囲（初期は 12 行程度）を超えるとテストがスクロール操作無しでは行数を数えられない。G2 以降で一覧の件数が増える fixture を作る場合は要注意。
- **`/tasks/:id` の操作ボタンは無効表示のみ**（G1 は表示だけ、実装は G2）。

## 提案（`docs/DESIGN.md` / `docs/taskd-api-v1.md` への変更提案。採否は人間）

- G0-P1: `docs/DESIGN.md` §0 / §10 の「React 19.3」は「React 19.2 以上（cooldown 7 日を満たす最新）」と読み替えた（ADR-0003 D2）。文言を「19.2+」にすると実態と合う。
- G0-P2: `docs/DESIGN.md` §10 Phase G0 の「shadcn/ui（`-t react-router`）」: `shadcn init -t react-router` は新規プロジェクト生成用で、既存プロジェクトには `components.json` を置くだけでよい。G0 の記述を「`components.json` と `lib/utils.ts` を置く」に緩めるとよい（ADR-0003 D8）。
- G1-P1: `docs/DESIGN.md` §6.3 の 3「`task.event` / `daemon` を受けたら再検証（250ms デバウンス）」は、`daemon` が毎 tick 届く前提と併せて読むと「一定間隔ごとに必ず 1 回発火するスロットル」だと明記した方が誤解が無い（素朴な trailing debounce だと `tick_ms < debounceMs` のとき永久に発火しないライブロックになる。ADR-0004 D2 で実装済みだが DESIGN 本文には無い）。
- G2-P1: `docs/DESIGN.md` §8.2「CSRF … React Router の middleware で実装」は、React Router 8 が document request の変更系に対して middleware より前に独自の Origin 検査を行い
  **400** を返すため、そのままでは受け入れ条件 7（403）を満たせない。「Express 層（`server/app.ts`）で 403、`.data` request は root middleware」と書き換えるのが実態に合う（ADR-0005 D1）。
- G2-P2: `docs/DESIGN.md` §6.3 の 2「`TransitionResult` を flash に載せて」は、クッキーのセッションではなく action の戻り値（`actionData`）で実現した（ADR-0005 D2）。
  併せて「action が 4xx を返したときも loader を再検証する（React Router の既定は再検証しない）」を §4.3 の「409 は再取得」の実装上の注意として明記するとよい。
- G2-P3: `docs/DESIGN.md` §4.4「GUI 側の検証は『必須欄が空』程度に留め」は、`required` を付けると taskd の 422 文言が一度も見えず受け入れ条件 5 と両立しない。「GUI 側の検証はしない」に寄せる（ADR-0005 D5）。
- G2-P4: `docs/DESIGN.md` §10 Phase G2 の受け入れ条件 1「親タスクが SSE 経由で `done` に変わる」に時間の上限が無い。条件 5 と同様に上限（例: 30 秒）を書くと e2e の判定が一意になる。
  また条件 3「回答 → `ready`」は fake ワーカーがすぐ拾って再び `blocked` になるため、判定は `TransitionResult.to` と `answered` イベントで行った旨を明記するとよい。
- G1-P2: `docs/DESIGN.md` §6.5「500 にしない」は React Router の本番ビルドが素の `Error` を ErrorBoundary に渡す前に汎用 500 へサニタイズすることと衝突しやすい（ADR-0004 D6）。「loader は taskd のエラーを `Response` として投げること」と実装上の注意を明記すると、次に同じ罠を踏まずに済む。

## taskd への依頼（`docs/taskd-requests.md` の要約）

- R1（G2、調査依頼・BLOCKED ではない）: ブラウザ + SSE 中継が接続している間、変更系 `POST` の直後に taskd の tick が 10〜30 秒止まる現象を e2e で 5 回観測
  （API 単体の curl では再現しない）。詳細と証拠は `docs/taskd-requests.md` R1。**回答済み（G5）**: 原因は NFS 上の DB。GUI 側は `scripts/taskd.sh` の `RUN_ROOT` をローカルディスクにした（ADR-0008 D13）。
- G5: `docs/taskd-api-v1.md`（GUI 側のコピー）を taskd の `docs/gui/api.md`（ADR-0015、`actions`）に同期してほしい（GUI 側では書き換えない規則）。

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

## Phase G1 — DONE（2026-09-15）

### 成果物
- 画面: `app/routes/inbox.tsx`（受信箱 `/`。`GET /inbox` をそのまま描画。承認待ち/質問/draft/注意の4区画）、
  `app/routes/tasks.tsx`（一覧 `/tasks`。`GET /tasks` のフィルタ・並び替え・keyset ページングをそのまま転送。`@tanstack/react-virtual` で仮想スクロール、
  「さらに読む」で `useFetcher` により追記）、`app/routes/tasks.$id.tsx`（詳細 `/tasks/:id`。`TaskDetail` の全節 + イベントタイムライン。生ログ・成果物本体・DAG は G3）。
- SSE: `app/routes/events.ts`（resource route。`GET /stream` をヘッダ・バイト列とも無加工で中継、taskd の非 2xx もそのまま返す）、
  `app/hooks/useTaskdStream.ts`（`createStreamController` の純粋なスロットル制御 + `useTaskdStream` フック。root で 1 回だけ張る）。
- `app/root.tsx`: `GET /inbox` の `counts` をタイトルバーの承認待ちバッジに、`useTaskdStream()` の起動、`ErrorBoundary` を `Response` ベースのエラー判別に対応、
  footer に `taskd_version`/`api_version`/`schema_version`/`journal_mode` の testid 付き詳細を追加（G0 の受け入れ条件との整合）。
- `app/taskd/errors.ts`: `taskdErrorResponse()` を追加（`TaskdUnavailable`/`TaskdError` を `Response` に変換。理由は下記「監査後の修正」）。
- taskd 側: `scripts/taskd.sh fixture basic`、`test/taskd/fixtures/basic-worker.sh`（タスクの kind/title で分岐する fake ワーカー）、
  `test/taskd/fixtures/basic-plan.json`（Plan の子 2 件）。
- 型検証: `scripts/capture-fixtures.sh`（実 taskd から `test/fixtures/api/*.json` を採取）、`test/fixtures/api-types.check.ts`
  （`Widen<T>`＝文字列系フィールドを `string` に緩めた型で構造の一致を `pnpm typecheck` に検証させる。列挙値そのものの正しさまでは見ない）。
- テスト: `test/unit/{tasks.loader,tasks.detail.loader,events.route,useTaskdStream,inbox.loader}.test.ts`（36 件）、`e2e/g1.spec.ts`（8 シナリオ）、
  `e2e/g0.spec.ts` の一部修正（G1 で `/` の見出しが h1→h2 になった点、root が SSE を常時張るため `networkidle` に到達しなくなった点への追従）。
- 文書: `docs/adr/0004-g1-decisions.md`（D1〜D6。受信箱の二重取得、SSE デバウンスの設計と実機バグの修正、仮想スクロールの蓄積管理、
  G1 の描画範囲、fixture の構成、本番ビルドでの ErrorBoundary サニタイズ対策）。

### 受け入れ条件と証拠（docs/DESIGN.md §10 Phase G1）
1. **`scripts/taskd.sh fixture basic` → `taskctl ls` に done×3・ready(Approval)×1・reviewing×1・blocked×1・draft×2・done(Plan)×1・failed×1、`replay` が `0 mismatches`** —
   ```
   $ scripts/taskd.sh taskctl basic ls
   01M2HJD6ZRJW25FC170KN5XVS5 Done Execute Chain-A1
   01M2HJD7HWYTPMH84XDFSPNBPJ Done Execute Chain-A2
   01M2HJD823DS82PMFG5VX6BN1W Done Execute Chain-A3
   01M2HJD8P9Q444J57384N98HGV Reviewing Execute Human-B
   01M2HJD9B3GTZKKC4H5B31NBYR Blocked Execute Blocked-C
   01M2HJDA0NXSK913B8389XXM3A Done Plan fixture plan goal: build two small things
   01M2HJDAM91QFBCJ9KBF001HWZ Failed Execute Failed-E
   01M2HJDBRRMKXSF3YJYQ2PYTJ0 Ready Approval Approval needed: Human-B — criterion 0 (attempt 1)
   01M2HJDCQT08KYBS5WKV5VWDHM Draft Execute Plan-Child-1
   01M2HJDCQTZH17ZRJNP32DFCQ5 Draft Execute Plan-Child-2
   $ scripts/taskd.sh taskctl basic replay
   replay: 0 mismatches across 10 tasks
   ```
   done 3（Chain-A1〜3）・ready(Approval) 1・reviewing 1（Human-B）・blocked 1・draft 2（Plan-Child-1/2）・done(Plan) 1・failed 1 を全て含む。
2. **Playwright: `/` の承認待ちに `Approval needed:` の項目が1件（親のtitle、条件の文、summaryが表示）、質問に(c)の質問文、draftにPlanの下の子2件、注意にfailed1件** —
   `e2e/g1.spec.ts:54`「受け入れ条件 2: 受信箱」pass。`approval-item` 1件（`Approval needed:` を含み、`approval-parent-title` に `Human-B`、
   `approval-criterion-text` に `someone signs off`、`approval-summary` に `fixture done`）、`question-item` 1件（`which environment should this target?`）、
   `draft-item` 2件（`Plan-Child`）、`attention-item` 1件（`Failed-E`）。
3. **Playwright: `/tasks?status=done` の行数が `taskctl ls --status done | wc -l` と一致。`/tasks?limit=2` で「さらに読む」を最後まで押して集めた id が重複なく全件と一致** —
   `e2e/g1.spec.ts:82,92` pass。`taskctl ls --status done` = 4行、`/tasks?status=done` の `task-row` = 4。`limit=2` を反復クリックして集めた id 集合が
   `GET /tasks?limit=500` の全 id 集合と完全一致（重複ゼロ）。
4. **Playwright: `/tasks/<id>`（Human-B）に条件と Human 条件の Approval 子へのリンク、run 1件、タイムラインに approval_requested は無く worker_finished がある。
   `curl /api/v1/tasks/<id>` の `runs.length` と画面の run 件数が一致** — `e2e/g1.spec.ts:120` pass。`task-status` = `reviewing`、`criterion-approval` のリンク先が
   Approval 子の id（ULID 形式）、`run-row` 1件（API の `runs.length`=1 と一致）、タイムラインの `data-event-type` 一覧に `worker_finished` を含み
   `approval_requested` を含まない（Approval 子自身の events には `approval_requested` があることを別途 curl で確認済み）。
5. **SSE: `scripts/taskd.sh start basic` のまま `/tasks` を開き `taskctl add` → 3秒以内にリロード無しで反映。`curl -N /events` に `event: hello` と `event: task.event`** —
   `e2e/g1.spec.ts:145,161` pass。
6. **`pnpm test`: 各 loader の単体テストと useTaskdStream のデバウンスのテストが通る** — `pnpm test` → `Test Files 6 passed (6)` / `Tests 36 passed (36)`
   （`tasks.loader` 5、`tasks.detail.loader` 3、`events.route` 5、`useTaskdStream` 4、`inbox.loader` 4、`client`（G0 から）15）。

### 共通条件
- `pnpm lint` exit 0（`Checked 44 files`）/ `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0 / `pnpm test` **36 passed**（6 ファイル）/
  `pnpm build` exit 0 / `pnpm e2e` **13 passed**（G0 5 + G1 8。SSE のタイミング調整とエラー表示の回帰テストを含む）
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts` 差分ゼロ

### 監査結果
- auditor の 1 回目の判定: **条件付き可**（受け入れ条件 1〜6 は全て「満たしている」。「不可」ゼロ）。禁止事項（SQLite・crate 依存・仕様外挙動・ブラウザ直接呼び出し・
  トークン露出・`dangerouslySetInnerHTML`/CDN・テストの外部ネットワーク・版固定）は「違反なし」（トークンをダミーで置き HTML/ログ/`build/` から 0 件を確認済み）。
- 条件と対応（全て対応済み。auditor の再監査は行わず、自分で `pnpm lint`/`typecheck`/`test`/`build`/`e2e` を再実行して確認 — 最大1回の枠内）:
  1. **`/tasks` / `/tasks/:id` が taskd 停止中に 500 + 汎用エラー画面になる**（DESIGN §6.5「500にしない」/ ADR-0003 D4 違反。本番ビルドの React Router が
     loader の投げた素の `Error` を ErrorBoundary に渡す前に `Unexpected Server Error` へサニタイズするため、`isTaskdUnavailable`/`instanceof TaskdError` の分岐が
     本番では絶対に真にならない死にコードだった）→ **修正済み**。`app/taskd/errors.ts` に `taskdErrorResponse()` を追加し、`TaskdUnavailable`/`TaskdError` を
     `Response`（サニタイズされない）に変換して投げるよう `tasks.tsx`/`tasks.$id.tsx`/`inbox.tsx` の loader と、`root.tsx`/`tasks.$id.tsx` の `ErrorBoundary` を書き直した
     （詳細は `docs/adr/0004-g1-decisions.md` D6）。回帰テストを `e2e/g1.spec.ts`「回帰: 子ルートのエラー表示」に追加（taskd 停止中の `/tasks` が 500 にならずバナーを出す、
     存在しない id の `/tasks/:id` が 404 で「タスクが見つかりません」になる）。
  2. **`/events` の 503 `too_many_streams` 等が中継されず 500 になる**（DESIGN §6.4 違反）→ **修正済み**。`app/routes/events.ts` が `client.stream()` の例外を
     `taskdErrorResponse()` でそのまま同じ status の `Response` として返すようにした。`test/unit/events.route.test.ts` に 503（taskd のエラー応答／接続不可の両方）を
     正しく中継する単体テストを追加。
  3. **未解決事項への記載漏れ**（SSE 常時再検証の負荷、一覧の仮想化と行数一致テストの限界）→ 下記「未解決事項」に記載。
  4. **`inbox` loader の単体テスト不足** → `app/routes/inbox.tsx` の loader 本体を `loadInbox(client, request)` として切り出し、
     `test/unit/inbox.loader.test.ts`（4件: 正常系、`TaskdUnavailable`→`null`、他エラー→`Response` に変換、`TaskdError` のまま投げないことの確認）を追加。
- 修正後の自己検証: `pnpm lint` exit 0（44 files）、`pnpm typecheck` exit 0、`pnpm test` **36 passed**、`pnpm build` exit 0、`pnpm e2e` **13 passed (34.9s)**
  （回帰テスト2件を含む）。auditor の 2 回目起動は行っていない（見つかった「不可」相当の項目が無く、条件はすべて自分で確認可能な範囲だったため。
  CLAUDE.md「同じアプローチを3回失敗したら」には該当しない）。

### 未解決事項
- **G1-U1: SSE 常時再検証の負荷** — `daemon` イベントが tick ごと（fixture は `tick_ms=200ms`）に届き、`useTaskdStream` のスロットル（既定 250ms）により
  画面を開いている間ずっと 250ms ごとに root + 現在ルートの再検証が走る（実測: タブ1つで taskd への要求が毎秒約7回）。G1 の受け入れ条件は満たすが、
  G4 で複数タブ・長時間表示を想定するなら間引き（`daemon` の再検証間隔を長くする、`task.event` とは別のデバウンス時間にする等）を検討したい。
- **G1-U2: 仮想スクロールと行数一致アサーションの限界** — `/tasks` は `@tanstack/react-virtual` で可視範囲のみ DOM に描画するため、SSR 直後の HTML には
  `task-row` が 0 件で、Playwright のアサーションも可視範囲（初期表示で十数行）までしか数えられない。件数の多い fixture を今後作る場合は
  スクロール操作を伴うテストに直す必要がある。
- **G1-U3: 接続断表示は未実装** — DESIGN §6.3 の 3 後半「`error` が続いたら『接続が切れました』表示」は G1 の受け入れ条件に無いため実装していない
  （`EventSource` のブラウザ標準の自動再接続に任せている）。G3/G4 で必要になったタイミングで追加する。
- **G1-U4: `@testing-library/react` は未導入のまま** — `useTaskdStream` のデバウンス検証は `createStreamController`（DOM非依存）を直接テストする形にして
  jsdom 無しで済ませた（`docs/adr/0004-g1-decisions.md` D2）。実際に DOM 描画のアサーションが要る G2（フォームの表示など）で導入を検討する。
- G0 からの未解決事項（Host 検査とセキュリティヘッダの `/assets` 適用範囲、`pnpm dev` の CSP `unsafe-inline`）は G1 では対処していない（引き継ぎ済み、G5 で解消予定）。

### 提案
- 上の「提案」節の G1-P1（SSE デバウンスの仕様明確化）、G1-P2（本番ビルドの ErrorBoundary サニタイズとの付き合い方の明記）。

### taskd への依頼
- なし。`GET /inbox`・`GET /tasks`・`GET /tasks/{id}`・`GET /tasks/{id}/events`・`GET /stream` は `docs/taskd-api-v1.md` の記載どおりに動作した
  （フィールド名・ページング・SSE のイベント種別・エラー形状のいずれも文書と実挙動が一致）。

## Phase G2 — DONE（2026-09-15）

### 成果物
- 状態変更: `app/taskd/actions.server.ts`（フォーム → `POST /tasks/{id}/{approve|reject|answer|cancel}` の本文の写し `applyTransition`、`readTransitionForm`、`toActionError`（problem+json → `ActionError`）、`transitionData`）、
  `app/taskd/route-actions.server.ts`（各ルートの action 本体 `runTaskAction` / `runInboxAction`（`task_id` 複数を直列）/ `createTask` / `createPlan` / `runReplay`）、
  `app/taskd/action-types.ts`（`TransitionOutcome` / `ActionError` / `CreateFailure` / `ReplayOutcome`。クライアントからも import 可）、`app/taskd/forms.ts`（純粋な `formString`）。
- 画面: `app/routes/tasks.$id.tsx`（操作節: `detail.actions` にある操作だけフォームを出す。hidden `intent` / `expected_status`=描画時の status、approve/reject の note、answer の質問文と回答欄、cancel）、
  `app/routes/inbox.tsx`（承認待ち: note + 承認/却下、質問: 回答、draft: 受け入れ/取り消し/「この Plan の子を全部受け入れ」（原子性なしを明記）、注意: 取り消し）、
  `app/routes/tasks.new.tsx`（`NewTaskSpec` と 1:1 のフォーム。受け入れ条件ビルダー、depends_on の候補チェックボックス + 自由入力欄、422 の `errors[]` をフィールド下に）、
  `app/routes/plans.new.tsx`（`NewPlanSpec` のフォーム、`GET /config` の `plan_auto_accept` の説明）、`app/routes/daemon.tsx`（`GET /daemon` + `GET /config` の表示と replay ボタン。G2 の最小限、本格版は G4）、
  `app/components/Flash.tsx`（`TransitionFlash`（`cascaded` の id をリンクで列挙）/ `ErrorFlash`（409 は「状態が変わりました」）/ `FieldErrors`）、`app/lib/revalidate.ts`（4xx の action 後も再検証）。
- セキュリティ: `app/middleware/security.server.ts` に `csrfViolation`（純粋関数）/ `csrfCheck`（root middleware、`.data` request 用）/ `expressCsrfGuard`（Express 層、document request 用。React Router 組み込みの 400 より前に 403）。
  `server/app.ts` に `expressCsrfGuard` を登録。`app/root.tsx` の middleware を `[hostCheck, csrfCheck, securityHeaders]` に。ナビゲーションに「新規タスク」「新規 Plan」「デーモン」。
- テスト: 単体 7 ファイル 55 件を追加（`actions` 15、`security.csrf` 10、`tasks.new` 9、`plans.new` 5、`tasks.detail.action` 7、`inbox.action` 4、`daemon` 5）。`e2e/g2.spec.ts`（8 シナリオ = 受け入れ条件 1〜8）。
  `e2e/g0.spec.ts` に `beforeAll`（`basic` が 7710 を掴んだままでも `dev` を起動できるように）。
- 文書: `docs/adr/0005-g2-decisions.md`（D1〜D7）、`docs/taskd-requests.md` R1（調査依頼）。

### 受け入れ条件と証拠（docs/DESIGN.md §10 Phase G2。`pnpm e2e` の `e2e/g2.spec.ts`、実 taskd `basic` + fake ワーカー並走）
1. **受信箱で Approval を note 付きで承認 → `approval_decided`（approved: true、note）、親が SSE 経由で `done`、`replay` 0 mismatches** — `g2.spec.ts:80` pass（34.3s）。
   受信箱の `approval-note` に `looks good from the GUI` を入れ `approval-approve` → flash `flash-to`=`done`、`approval-item` 0 件。`GET /tasks/<approval-id>/events` に
   `{type: approval_decided, approved: true, note: "looks good from the GUI"}`、`GET /tasks/<approval-id>` の status `done`。別タブで開いていた親 Human-B の `task-status` が
   リロード無しで `reviewing` → `done`（このランでは約 30 秒。G2-U1 の停止に当たった。停止しないランは 0.5 秒）。`taskctl basic replay` → `0 mismatches`。
2. **2 つのページで同じ Approval を開き、片方で承認 → もう片方は「状態が変わりました」（409）、状態は不変** — `g2.spec.ts:127` pass（2.9s）。専用の Approval（`kind=approval`、`ready`）を作り、
   古いタブ（`/events` を abort して再検証を止めたもの）と新しいタブで開く。新しいタブで承認 → `done`。古いタブで承認 → `flash` の `data-flash-kind=error`、`flash-conflict` に「状態が変わりました」、
   `data-flash-code` は `conflict`。action 後の再検証で古いタブも `done` 表示。`approval_decided` は 1 件のまま。
3. **blocked に回答 → `ready`、`answered` の `question` が画面の質問文と一致** — `g2.spec.ts:167` pass（1.5s）。Blocked-C の詳細で `action-question` の文（`which environment should this target?`）を読み、
   `action-answer` に回答 → flash `blocked` → `ready`。`GET /tasks/<id>/events` の最後の `answered` の `answer` が入力値、`question` が画面の文に含まれる。
4. **後続を持つ `ready` を cancel → flash に `cascaded` の後続 id、後続が `cancelled`（`dependency_failed`）** — `g2.spec.ts:192` pass（1.4s）。draft の依存先 → それに依存する `ready`（Cancel-Target-G2）→ その後続 `ready`（Downstream-G2）を
   taskctl で作り、GUI で Cancel-Target-G2 を取り消し → `flash-to`=`cancelled`、`flash-cascaded-id` = [Downstream-G2 の id]。API で Downstream-G2 は `cancelled`、最後の `transitioned` が `reason: dependency_failed`。
5. **作成フォーム: 条件ゼロ → 422 文言そのまま、存在しない depends_on → `dependency <id> does not exist`、正しい入力 → `draft` → 承認 → 30 秒以内に `done`** — `g2.spec.ts:253` pass（3.9s）。
   `field-error-acceptance` に `at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)`、flash の `data-flash-code=validation`。
   `depends_on_extra` に `01HZZZZZZZZZZZZZZZZZZZZZZZ` → `field-error-depends_on` に `dependency 01HZZZZZZZZZZZZZZZZZZZZZZZ does not exist`。正しい入力 → `/tasks/<ULID>` に遷移、`task-status`=`draft` →
   `action-approve` → flash `ready` → `task-status` がリロード無しで `done`（このランでは約 3 秒。停止に当たったランでは 26.3 秒で通過）。
6. **Plan フォーム → `draft` の Plan。`plan.auto_accept = false` の説明** — `g2.spec.ts:294` pass（1.4s）。`plan-auto-accept` に `plan.auto_accept = false` と「draft」、空 goal → `field-error-goal` に `goal must not be blank`、
   goal 入力 → `/tasks/<id>` で `task-kind`=`plan`、`task-status`=`draft`（API でも同じ）。
7. **`curl -X POST -H 'Origin: http://evil.example' -d 'intent=cancel' http://127.0.0.1:7700/tasks/<id>` が 403、状態不変** — `g2.spec.ts:319` pass（234ms）。draft を 1 件作り、curl の `%{http_code}` = `403`、
   `Sec-Fetch-Site: cross-site` でも `403`、API の status は `draft` のまま。（React Router 組み込みの検査だと 400 になるため Express 層で 403 にした。ADR-0005 D1）
8. **デーモン画面の replay → `0 mismatches`** — `g2.spec.ts:381` pass（857ms）。`/daemon` の `daemon-pid` 表示、`replay-button` → `replay-result` に `0 mismatches across N tasks`（`taskctl replay` の N と一致）。

### 共通条件
- `pnpm lint` exit 0（`Checked 61 files`）/ `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0 / `pnpm test` **91 passed**（13 ファイル。G1 までの 36 + G2 の 55）/ `pnpm build` exit 0
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts` 差分ゼロ（exit 0）
- `pnpm e2e`: **間欠的に失敗する**（G2-U1）。実測: 単独セッションでの初回フルラン 21 passed (1.6m) exit 0。auditor による 2 回のフルランでは 20 passed/1 failed・19 passed/2 failed
  （落ちたのは受け入れ条件 1 または 5。原因はいずれも `POST .../.data` 直後に taskd への `GET .../.data` が `503`（15 秒タイムアウト）で応答し、その後 taskd が復帰するというもの）。
  自分の再検証でも 1 回で 20 passed/1 failed（条件 5 の `flash-to`="ready" が 5 秒以内に出ない。BFF ログで `GET .../.data status:503 ms:15005.8` を確認）。
  3 回中 3 回とも GUI 側の契約違反（トークン露出・`/stream` の放置・SQLite アクセス等）は見当たらず、`docs/taskd-requests.md` R1（taskd 側の間欠停止）に一致する。
  条件を単独実行すれば通る（例: `pnpm e2e -g "受け入れ条件 5"` は 3.5s で pass）。

### 監査結果
- auditor の判定: **条件付き可**（「不可」ゼロ）。受け入れ条件 1〜8 は全て「満たしている」（auditor 自身が `lint`/`typecheck`/`test`/`build`/`gen:types` 差分ゼロと、`pnpm e2e` を 2 回フル実行、
  さらに条件 5 のみの単独実行で確認）。禁止事項（SQLite・crate 依存・仕様外挙動・ブラウザ直接呼び出し・トークン露出・`dangerouslySetInnerHTML`/CDN・テストの外部ネットワーク・版固定）は「重い違反は無し」。
- 条件と対応:
  1. 「`pnpm e2e` の間欠失敗を PROGRESS の証拠欄に反映する」→ **対応済み**（上の「共通条件」に実測を記載。G2-U1 として記録済みのものと一致することを確認）。
  2. 「G2 のコミットを G3 の変更と混ぜない」→ **対応済み**。監査時点で作業ツリーに G3 の途中成果（`app/routes.ts` が未作成の `tasks.$id.runs.$runId.tsx` を参照）が混在し `pnpm build` が失敗する状態だったため、
     G3 分（新規ファイル・`package.json`/`pnpm-lock.yaml`・`app/routes.ts`/`app/root.tsx`/`app/routes/tasks.$id.tsx` への追加分）を一時的に退避し、G2 のみの状態で
     `lint`/`typecheck`/`test`（91 passed）/`build`/`gen:types`（差分ゼロ）/`e2e` を再実行してから本コミットを作成した。
  3. 軽微な指摘（`inbox.tsx` の attention 区画での cancel 可否判定の GUI 側再実装、作成フォームの既定値の焼き込み）→ 下の「未解決事項」に記載（実装変更は必須とされていない）。

### 未解決事項
- **G2-U1: e2e 中の taskd の間欠的な停止**（`docs/taskd-requests.md` R1）— ブラウザ + SSE 中継が接続している間、変更系 `POST` の直後に taskd の tick・SSE・（時に）API が 10〜30 秒止まる。
  監査を含め計 5 回の e2e フルランで複数回観測（条件 1 / 3 / 5 のどこかに当たる。単独実行では再現しない）。API 単体の curl、GUI 経由 SSE 3〜4 本、`.data` 連打では再現しない。GUI 側の契約違反は見つからず
  （上流 `/stream` は 8 秒以内に閉じる、SQLite は開かない）。受け入れ条件 1 の e2e の待ちを 60 秒にしてある。条件 5（30 秒）は通ることも落ちることもある（taskd の停止時間次第）。
- **G2-U6: `inbox.tsx` の attention 区画の cancel 判定を GUI が再実装している**（auditor 指摘）— `app/routes/inbox.tsx` が `item.task.status` を見て `!== done/failed/cancelled` を
  自前で判定し cancel ボタンの表示を決めている（`docs/taskd-api-v1.md` §5.4 の「非終端」規則の GUI 側再実装。`Inbox` 型に `actions` が無いための回避）。原則に忠実にするなら、
  押して 409 を見せる（他区画と同じ扱いにする）か、taskd に `Inbox` への `actions` 追加を依頼するのが良い。次フェーズ以降で検討する。
- **G2-U7: 作成フォームの既定値の焼き込み**（auditor 指摘）— `tasks.new.tsx`/`plans.new.tsx` が `max_turns=30`/`max_wall_secs=900`/`max_retries=1`/`kind=execute`/`tier=standard` を
  `defaultValue` として常に明示送信しており、ADR-0005 D5「空欄は本文から省いて taskd の既定を使う」と不整合（taskd 側の既定が変わっても GUI 経由の作成だけ旧値のままになる）。次フェーズで解消を検討する。
- **G2-U2: デーモン画面は最小限** — `in_flight` 等は件数のみ、遅延判定・経過時間・停止/復旧バナー・`awaiting_human` / `unroutable` の照合は G4（ADR-0005 D6）。
- **G2-U3: 受信箱の「この Plan の子を全部受け入れ」は単体テストのみ** — e2e では Plan の draft 子 2 件を GUI で一括受理するシナリオを入れていない（受け入れ条件に無い）。
- **G2-U4: CSRF 検査の入口が 2 つ** — Express 層（document request）と root middleware（`.data` request）。規則は `csrfViolation` の 1 か所だが、G5 で認証（セッションクッキー）を入れるときに
  Express 層へ寄せるか再検討する（G0-U の Host 検査の適用範囲と同じ論点）。
- **G2-U5: `TaskdClient` のタイムアウト 15 秒** — G2-U1 の停止に当たると loader が 503 `unavailable` を投げ、画面が「taskd に接続できません」に切り替わる（root の 5 秒再検証で復帰する）。
  停止の原因が分かるまでは変えない。
- G1 からの引き継ぎ（G1-U1 SSE 常時再検証の負荷、G1-U2 仮想スクロールと行数一致テスト、G1-U3 接続断表示、G1-U4 `@testing-library/react` 未導入）と G0 からの引き継ぎ（`/assets` の Host 検査、`pnpm dev` の CSP）は
  G2 では対処していない。G1-U4 は G2 でも不要だった（フォームの DOM 検証は Playwright、単体は純粋関数と mock-taskd）。

### 提案
- 上の「提案」節の G2-P1（CSRF の実装層）、G2-P2（flash と 4xx 後の再検証）、G2-P3（GUI 側の検証はしない）、G2-P4（受け入れ条件 1 の時間上限と条件 3 の判定方法）。

### taskd への依頼
- R1（調査依頼）: 上記 G2-U1。`docs/taskd-requests.md` R1 に現象・証拠・再現しない条件・依頼内容を記載。要求ごとのログ（`X-Request-Id`・所要時間）が taskd 側にあると切り分けやすい。

## Phase G3 — DONE（2026-09-15）

### 成果物
- ログビューア: `app/lib/stream-json.ts`（`classifyStreamJsonLine`。claude-code の `assistant`/`result`、codex の `item.*`/`turn.*`/`error`/`thread.started` を
  `utterance`/`tool`/`result`/`raw` の 4 種に正規化。taskd 独自ワーカープロトコルや不正 JSON は全て `raw`）、`test/fixtures/stream-json/{claude-code,codex,fake}.jsonl`
  （`crates/task-worker/src/{claude_code,codex}.rs` のテストの行を転記）、`test/unit/stream-json.test.ts`（10 件）。
  `app/routes/tasks.$id.runs.$runId.tsx`（`/tasks/:id/runs/:runId`。`GET /tasks/{id}/runs` から対象 run を探し、`stdout.jsonl`/`stderr.log`/`result.json` を
  loader が taskd から取得して SSR、実行中の run は `?offset=` を 1 秒ごとに叩いて追尾。stdout は構造化/生テキストの切替可）。
- ファイル中継: `app/routes/files.runs.ts`（`/files/tasks/:id/runs/:runId/:name`）、`app/routes/files.artifacts.ts`（`/files/tasks/:id/artifacts/:idx`）。
  どちらも `TaskdClient.file()` の応答を許可リストのヘッダ（`content-type`/`content-disposition`/`content-length`/`content-range`/`accept-ranges`/`x-taskd-sha256`/
  `x-taskd-sha256-current`/`x-taskd-size`）だけ中継し `x-content-type-options: nosniff` を付与、taskd の非 2xx は `taskdErrorResponse` でそのまま返す。
  `test/unit/files.route.test.ts`（8 件。403 `path_forbidden` の非スロー中継、503 unreachable、Range/offset/download クエリの転送、`X-Taskd-Sha256` 系ヘッダの中継を含む）。
- 成果物ビューア: `app/components/{CodeViewer,MarkdownViewer,ImageViewer,Sha256Badge}.tsx`（CodeMirror 6 読み取り専用 / `react-markdown`+`remark-gfm` / `<img>` /
  sha256 不一致バッジ）、`app/lib/artifact-view.ts`（`pickViewer`/`isJson`/`artifactStatusMessage` の純粋関数）、`test/unit/artifact-view.test.ts`（9 件）。
  `app/routes/tasks.$id.tsx` に成果物一覧セクションと `ArtifactRow`（開く/保存、`ArtifactList` の `forbidden`/`sha256_matches` をそのまま表示）を追加、
  loader が `GET /tasks/{id}/artifacts` も並列取得。
- DAG: `app/routes/graph.tsx`（`/graph`。`GET /graph` をそのまま取得、`@xyflow/react` + `@dagrejs/dagre` はクライアント専用でマウント後に描画、SSR はプレースホルダ）、
  `app/lib/graph-layout.ts`（`layoutGraph`: dagre で層状配置し、`parent_id` の子は配置後のバウンディングボックスから group ノードを合成。色 = `Status`、太枠 = `kind=plan`）。
- fixture: `test/taskd/fixtures/basic-worker.sh` に `Slow-F`（2 秒おきに progress を 5 回、追尾用）、`Artifacts-G`（`note.md`（`<script>alert(1)</script>` 込み）/
  `data.json`/`image.png` を明示的な `{"type":"artifact",...}` メッセージで登録）を追加、`scripts/taskd.sh` の `fixture_basic` に `Artifacts-G` を追加（`Slow-F` は
  `--until-idle` の対象にすると追尾の検証ができなくなるため e2e 側で都度作成）。
- e2e: `e2e/g3.spec.ts`（受け入れ条件 1・3・4・6・7 の 5 シナリオ）、`e2e/g3.spec.ts-snapshots/graph-basic-chromium-linux.png`（スクリーンショットのベースライン、
  今回のランで新規作成）。
- 文書: `docs/adr/0006-g3-decisions.md`（D1〜D7: `/files/...` の 2 ルート、stream-json 分類の設計、ビューアの構成、成果物セクションの実装、DAG のレイアウト、
  fixture の追加、保存はネイティブ `<a download>`）。
- 新規依存: `@xyflow/react` 12.11.6、`@dagrejs/dagre` 3.1.1、`@codemirror/view` 6.43.11、`@codemirror/state` 6.7.4、`@codemirror/lang-json` 6.0.2、
  `react-markdown` 10.1.0、`remark-gfm` 4.0.1（全て ADR-0002 D7 で選定済みの版、`pnpm install` で 7 日 cooldown を通過）。

### 受け入れ条件と証拠（docs/DESIGN.md §10 Phase G3）
1. **(a) の run を開くと stdout.jsonl の行数が実ファイルの `wc -l` と一致し、result.json が整形表示される** — `e2e/g3.spec.ts:81` pass（1.2s）。
   `(a) = Chain-A2`（`depends_on` 1 本、run 1 件）。`.run/basic/workspaces/ws-a2/runs/<run_id>/stdout.jsonl` を Node で読んで求めた行数（1 行）と
   `[data-testid=stdout-line]` の件数が一致。`result-section` に `fixture done` を含む整形表示。
2. **`pnpm test`: stream-json の整形が claude-code の例で「発話/ツール呼び出し/結果」の3種、fake の JSON Lines は生表示** — `test/unit/stream-json.test.ts` 10 件 pass。
   `claude-code.jsonl` の3行がそれぞれ `utterance`（text="working on it"）/`tool`（label="Bash"）/`result`（isError=false）、`fake.jsonl` の全行が `raw`（生表示）。
   `codex.jsonl` も 4 行とも対応する種別（`thread.started`→raw、`item.started`→tool、`turn.completed`→result、`turn.failed`→result isError=true）。
3. **Markdown はテキスト表示（script 未実行）、JSON は CodeMirror、PNG は `<img>`、保存ファイルの sha256 が `X-Taskd-Sha256` と一致** — `e2e/g3.spec.ts:95` pass（1.6s）。
   `page.on("dialog")` は 1 度も呼ばれず（`<script>alert(1)</script>` はテキストとして描画、`markdown-viewer` 内に `<script>` 要素 0 件）、`data.json` は `code-viewer`、
   `image.png` は `image-viewer`（`<img src="/files/.../artifacts/2">`）。`artifact-download` をクリックしてダウンロードしたファイルの sha256 が、同じ URL への
   `page.request.get` で得た `X-Taskd-Sha256` ヘッダと一致。
4. **成果物ファイルを fixture 後に書き換える → sha256 不一致の警告** — `e2e/g3.spec.ts:149` pass（0.9s）。`.run/basic/workspaces/ws-g/artifacts/data.json` を
   Node で直接書き換えてから `/tasks/<id>` を開くと `[data-testid=sha256-mismatch]` が表示（taskd が都度計算し直す `sha256_matches` をそのまま見せているだけ）。
5. **mock-taskd で 403 `path_forbidden` → 「アクセスできません（path_forbidden）」** — DESIGN 本文が「実 taskd での細工 DB は taskd 側のテストに任せる」と明記しているため
   Playwright ではなく単体テストで確認: `test/unit/files.route.test.ts`（本体の 403 が資源ルートでそのまま中継されることを mock-taskd で確認）+
   `test/unit/artifact-view.test.ts`（`artifactStatusMessage({forbidden:true,...})` が文字列「アクセスできません（path_forbidden）」を返すことを確認。
   この文字列が画面の唯一の出所であり、DOM 描画ライブラリ非導入の制約下でも文言の一致を検証できる）。
6. **`/graph` で (a) の depends_on の辺が1本、Plan の子2件が group の中、スクリーンショットをベースラインとしてコミット** — `e2e/g3.spec.ts:166` pass（2.0s）。
   `(a) = Chain-A3`（`root=<id>&depth=1` で辺 1 本）。Plan の子 2 件（`Plan-Child-1/2`）の DOM 上の bounding box が `group-<planId>` の bounding box に収まることを確認。
   `e2e/g3.spec.ts-snapshots/graph-basic-chromium-linux.png` を今回のランで新規作成しコミット（2 回目のランで差分ゼロを確認）。
7. **追尾: 10秒かけて progress を5回出す run を開くと、リロード無しで行が増える** — `e2e/g3.spec.ts:208` pass（11.5s）。`Slow-F` を e2e 内で `taskctl add`/`approve` し
   （fixture 本体には含めない。含めると `--until-idle` で先に終わってしまい追尾を検証できないため）、`WorkerStarted` 直後に run ページを開いて
   `[data-testid=stdout-line]` の件数が増えることを `expect.poll` で確認。

### 共通条件
- `pnpm lint` exit 0（`Checked 76 files`）/ `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0 / `pnpm test` **118 passed**（16 ファイル。G2 までの 91 + G3 の 27）/
  `pnpm build` exit 0 / `pnpm e2e` **26 passed（exit 0、2.1〜2.4分）**（G0 5 + G1 8 + G2 8 + G3 5）。監査時点までに G2-U1（taskd の間欠停止）に当たらないフルランを
  複数回確認（自分の初回ラン、監査者の再実行とも 26 passed / exit 0）。`e2e/g3.spec.ts` 単体でも 5/5 pass。
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts` 差分ゼロ（exit 0）

### 監査結果
- auditor の判定: **条件付き可**（「不可」ゼロ）。受け入れ条件 1〜7 は全て「満たしている」（条件 5 は「部分的に満たしている」との留保付き。下記 G3-U5）。
  禁止事項（SQLite・crate 依存・仕様外挙動・派生値の GUI 再計算・ブラウザ直接呼び出し・トークン露出・`dangerouslySetInnerHTML`/`eval`/CDN・テストの外部ネットワーク・版固定）は「違反ゼロ」。
  auditor 自身が `lint`/`typecheck`/`test`（118 passed）/`build`/`gen:types` 差分ゼロ/`pnpm e2e`（26 passed）を再実行し、`/files/...` のヘッダ中継・Range・404/416・
  `Host` 検査・`/graph` の実データ（12 nodes/3 edges）を curl でも確認済み。
- 指摘と対応:
  1. **「共通条件」の `pnpm e2e` の記述が「25 passed/1 failed」のままで DESIGN §10.0 の共通完了条件（exit 0）と矛盾して見える** → **修正済み**（上の「共通条件」を実測の
     `26 passed / exit 0` に書き換え）。
  2. **`docs/adr/0006-g3-decisions.md` D5 の本文が「`parent_id` を dagre の `compound` group ノードにする」と書いており、実装（後付けのバウンディングボックス合成、
     compound は使わない）と正反対** → **修正済み**（D5 を実装に合わせて書き直した。ADR は実装前に書く決まりだが、今回は記述の誤りの訂正として扱う）。
  3. 受け入れ条件 5 のカバレッジ不足（下記 G3-U5）、DESIGN §6.2/§6.3(4)「ビューアは `/files/...` を fetch」と実装（run 詳細 loader が stdout/stderr/result 本体を
     直接取得して SSR する。ADR-0006 D4 に理由あり）の差分、G3-U4 の過小申告（stdout も含め 3 本を再検証毎に全文取得）、`Content-Encoding` 未中継、
     `hideAttribution` のライセンス確認、`files.artifacts.ts` 冒頭コメントの参照節誤り（§3.16→§3.8/§3.9 の意）→ 下の「未解決事項」「提案」に記載。
- 修正後の自己検証: `pnpm lint` exit 0、`pnpm typecheck` exit 0、`pnpm test` 118 passed、`pnpm build` exit 0、`pnpm gen:types` 差分ゼロ。auditor の再起動は行っていない
  （指摘は全て文書修正で対応可能で、コードの再検証を要する「不可」相当の項目が無かったため。CLAUDE.md「同じアプローチを3回失敗したら」には該当しない）。

### 未解決事項
- **G3-U1: `/graph` のスクリーンショット比較は環境依存の可能性** — `toHaveScreenshot` はフォントレンダリング等でマシンが変わると閾値超過になりうる
  （`maxDiffPixelRatio: 0.02` で緩めてはいる）。CI 環境を用意する際は同じ chromium 版・同じ OS イメージで再生成する。
- **G3-U2: DAG のレイアウトは compound（親子の入れ子）ではなく後付けの group 矩形** — `app/lib/graph-layout.ts` は dagre に `depends_on` の辺だけを渡してフラット配置し、
  `parent_id` の子はレイアウト後にバウンディングボックスから group を合成する（docs/adr/0006 D5）。ノード数が増えて子が離れた位置に層状配置されると、group の矩形が
  無関係なノードと重なる可能性がある（G3 の fixture 規模では発生しない）。G4/G5 でノード数が増える場面があれば dagre の compound 機能への切り替えを検討する。
- **G3-U3: run のタイムライン（`/tasks/:id` のイベント）と生ログの往来が手動** — 生ログページから元のタスク詳細への「戻る」リンクはあるが、run 一覧の他の run への
  直接遷移は無い（`/tasks/:id` に戻ってから別の run を選び直す）。G4 以降で使い勝手が問題になれば run セレクタを追加する。
- **G3-U4: run 詳細（`/tasks/:id/runs/:runId`）の loader は stdout / stderr / result の 3 本すべてを再検証のたびに全文取得する**（監査指摘、当初の記載は stderr のみと
  過小申告していた）。`?offset=` は追尾専用として使い、末尾表示は `GET .../stderr` の全文を取ってから末尾 200 行を切る（ADR-0006 D4）。SSE 由来の再検証（G1-U1）と重なると
  実行中の run のページで数百 ms 間隔の全文再取得が発生する（監査時の e2e ログで実測: 約 430ms 間隔）。ログが大きくなる実運用では `?offset=` を使った末尾取得へ切り替えを検討する。
- **G3-U5: 受け入れ条件 5（403 `path_forbidden` → 画面表示）の通し検証が無い**（監査指摘）— `test/unit/files.route.test.ts`（資源ルートが 403 を素通しする）と
  `test/unit/artifact-view.test.ts`（`artifactStatusMessage` が文言を返す）に分かれており、「一覧が forbidden:false を返した後に本体だけ 403 になる」ケースを
  通しでは検証していない。加えて `app/routes/tasks.$id.tsx` の `ArtifactRow` は成果物本体の `fetch` で `res.ok` を見ずに `res.text()` してしまうため、
  そのケースでは taskd の `{"kind":"taskd_error",...}` の JSON がそのまま本文として表示される（`app/routes/tasks.$id.runs.$runId.tsx` の `readFileText` は例外を
  `null` に握りつぶすので、run のログ側は逆にセクションごと消える）。DOM テストライブラリ未導入（G1-U4）下の妥協だが、次フェーズで `res.ok` を見て
  `artifactStatusMessage` 相当の表示に倒す修正を検討する。
- **G3-U6: `files.artifacts.ts` / `files.runs.ts` は `Content-Length` のみ中継し `Content-Encoding` を中継しない**（監査指摘、軽微）— 現在の taskd は圧縮しないため実害は無いが、
  将来 `Content-Encoding: gzip` 等を返すようになると Node の `fetch` が展開して長さが食い違う。taskd 側が圧縮を返すようになったら対応する。
- **G3-U7: `app/routes/graph.tsx` の `proOptions={{ hideAttribution: true }}`**（監査指摘）— React Flow (`@xyflow/react`) の帰属表示を消しており、xyflow の利用規約では
  非表示は Pro 購読者向けとされている。CLAUDE.md/DESIGN の禁止事項ではないが、ライセンス面は人間の確認を推奨する。
- **G3-U8: `app/routes/files.artifacts.ts` 冒頭コメントの参照節が誤り**（監査指摘、軽微）— `docs/taskd-api-v1.md §3.16` を挙げているが §3.16 は `GET /graph`。
  正しくは §3.8/§3.9（`GET /tasks/{id}/artifacts[/{idx}]`）。次に触るときにコメントを直す。
- G0〜G2 からの引き継ぎ（`/assets` の Host 検査適用範囲、`pnpm dev` の CSP、G2-U1 taskd 間欠停止、G2-U2〜U7）は G3 では対処していない。

### 提案
- 上の「提案」節の G0-P1/P2、G1-P1/P2、G2-P1〜P4 に加え、G3-P1: `docs/adr/0002` D7 が挙げた `@codemirror/lang-markdown` は導入していない
  （Markdown は `react-markdown` が描画し、CodeMirror 側で Markdown を表示する用途が無いため）。表として更新するなら D7 から該当行を削るのが実態に合う。
- G3-P2（監査指摘）: `docs/DESIGN.md` §6.2 のルート表 / §6.3 の 4「ビューアは `/files/...` を `fetch` して表示する」は、run 詳細の生ログ・result.json については
  loader がサーバ側で `/files/...` 相当（`TaskdClient.file()`）を取得して SSR する実装（ADR-0006 D4。SSR 一貫性と `?offset=` 追尾の都合）と食い違う。
  「成果物本体（画像等）はブラウザが `/files/...` を直接参照、run のテキストログは loader が取得して SSR する」と書き分けると実態に合う。

### taskd への依頼
- なし。`GET /tasks/{id}/runs`、ファイル系（`GET /tasks/{id}/runs/{run_id}/{stdout,stderr,result}`、`GET /tasks/{id}/artifacts[/{idx}]`）、`GET /graph` は
  `docs/taskd-api-v1.md` の記載どおりに動作した。`ArtifactProduced` がワーカーからの明示的な `{"type":"artifact",...}` メッセージでのみ記録される点
  （taskd がファイルシステムを自動スキャンしない）は §3.9 の記述と整合しており、fixture 側で対応した。

## Phase G4 — DONE（2026-09-15）

### 成果物
- 画面: `app/routes/providers.tsx`（`/providers`。`loadProviders(client, request)` が `GET /providers` をそのまま返す。`Providers.items[]` を表で表示。
  `env_keys` はキー名のみ、値は出さない。cooldown の残り時間だけは taskd が値を返さないので `fetchedAt`（BFF がリクエスト前後に取った時刻）と `cooldown.until` の差分を
  `app/lib/time-delta.ts` で表示専用に計算する）。
  `app/routes/daemon.tsx`（G2 の最小限版を拡張: `in_flight` の内訳表（task へのリンク・run_id・provider・経過時間）、`cooldowns` の内訳表（provider・reason・残り時間）、
  `awaiting_human` / `unroutable` の一覧（task へのリンク）、`docs/taskd-api-v1.md` §3.20 が明記する「`last_tick_at` が `now` から `3 × tick_ms` 以上古ければ GUI が
  遅延と表示する」規則どおりのバナーを追加。`loadDaemon` のシグネチャ・返り値は変更していない）。
  `app/lib/time-delta.ts`（`secondsBetween`/`formatDuration`。表示専用の単純な時刻差分計算。docs/adr/0007 D3）。
  `app/routes.ts` に `/providers` を登録、`app/root.tsx` のナビゲーションに「プロバイダ」を追加。
- fixture: `scripts/taskd.sh` に `fixture multi-account`（`test/taskd/multi-account.toml.tmpl` + `test/taskd/fixtures/multi-account-worker.sh`。taskd 本体の e2e
  `throttled_account_falls_back_to_the_next_account` を移植。**cooldown がプロセス内メモリのみで DB から再構築できないため（下記「監査結果」前の設計判断）、
  他の fixture と違い DB は作らず設定だけを用意する**）と `fixture unroutable`（`test/taskd/unroutable.toml.tmpl`。cheap タスクに frontier だけのプロバイダ、
  `--until-idle` で ready のまま残す）を追加。`prepare()` が既存の `taskd.toml` を無条件に上書きしていたバグを修正（`fixture unroutable`/`multi-account` の
  カスタム設定が `scripts/taskd.sh start` のたびに既定のテンプレートへ戻ってしまうのを直した）。
  `test/taskd/fixtures/basic-worker.sh` に `Slow-H`（20 秒 sleep してから done。受け入れ条件 4 の in_flight 表示用）を追加。
- テスト: `test/unit/providers.test.ts`（`loadProviders` の単体テスト 3 件）、`test/unit/time-delta.test.ts`（5 件）。既存 `test/unit/daemon.test.ts` は無改修で通過。
  `e2e/g4.spec.ts`（受け入れ条件 1〜4 の 5 シナリオ。`multi-account` は生きたプロセスを維持したまま `taskctl add`/`approve` でスロットルを起こす。`apiGet` は
  Node の `fetch` の keep-alive コネクションプールが直前に stop したプロセスのソケットを再利用して失敗する事象（実測）を避けるため `node:http` を `agent: false` で
  直接使う）。
- 文書: `docs/adr/0007-g4-decisions.md`（D1〜D8。cooldown の非永続化という taskd 側の実装事実、それに伴う fixture 設計、awaiting_human/unroutable が毎 tick
  再計算されること、GUI 側での経過時間・残り時間表示が派生値の再計算に当たらない根拠、multi-account/unroutable/Slow-H の各設計、停止/復旧バナーの流用）。
- 実装単位: `/providers` 画面と `/daemon` 画面拡張は互いにファイルを共有しない独立した単位だったため、implementer サブエージェント 2 体を並列実行した
  （担当: `app/routes/providers.tsx` + `test/unit/providers.test.ts` / `app/routes/daemon.tsx` のみ）。`app/routes.ts` の登録、`app/lib/time-delta.ts`、
  `scripts/taskd.sh` の fixture 追加、`e2e/g4.spec.ts` は設計判断とファイル共有（複数ルートから import される、順序依存の taskd 操作を要する）のため自分で実装した。

### 受け入れ条件と証拠（docs/DESIGN.md §10 Phase G4。`e2e/g4.spec.ts`、実 taskd `multi-account`/`basic`/`unroutable` に対して検証）
1. **`/providers` で `acct-a` が requeue 1・cooldown 残り時間表示、`acct-b` が done 1、tokens 合計が runs の usage と一致** — `e2e/g4.spec.ts:117` pass（0.7s）。
   `multi-account` を起動後、taskctl で `Fallback-MA`（`max-retries 0`）を作成・承認。`GET /providers` の `acct-a.stats.requeue`=1・`done`=0、`acct-b.stats.done`=1・`requeue`=0、
   `acct-a.cooldown`={reason: "throttled", until: 約5分後} を確認済み（手動 curl でも同じ値を確認: 実装前の検証で `acct-a` の run が `outcome:"requeue"`・`usage:null`、
   `acct-b` の run が `outcome:"done"`・`usage:{input_tokens:120,output_tokens:40}`）。画面の `provider-tokens`（acct-a + acct-b の合計）が `GET /tasks/<id>/runs` の
   2 run の usage 合計と一致することを確認。
2. **`/daemon` に pid/hostname/ticks が表示され、5 秒後の再読込で ticks が増える。fixture (b) の親（Human-B）が awaiting_human に 1 件。fixture unroutable では
   受信箱の注意と /daemon の unroutable に同じ id** — `e2e/g4.spec.ts:163,197` pass（6.0s / 0.7s）。`basic` で `daemon-pid`/`daemon-hostname` が非空、
   `daemon-ticks` が 5 秒後の reload で増加、`awaiting-human-item` に Human-B の id へのリンクが 1 件。`unroutable` フィクスチャでは `GET /inbox` の
   `attention[0].task.title`=`"Unroutable-U"` の id が、受信箱の `attention-item` と `/daemon` の `unroutable-item` の両方に同じ href（`/tasks/<id>`）で出る。
3. **`stop <name>` → 5 秒以内に全ページでバナー、`start` → 5 秒以内に消え、SSE が再接続して task.event が再び届く** — `e2e/g4.spec.ts:217` pass（3.1s）。
   `/tasks` を開いた状態で `stop basic` → `reload()` で 5 秒未満（実測 1 秒未満）にバナー表示、`/daemon`・`/providers` への遷移でもバナー（root と各ルートの
   `ErrorBoundary` の両方が出すため `taskd-banner` が 2 要素になりうる。ADR-0007 D8）。`start basic` → `/tasks` への遷移で 5 秒以内にバナー消失、
   `waitForResponse` で `/events` が 200 に戻ることを確認。復旧後に `taskctl add` した `Reconnect-Check` がリロード無しで `/tasks` に表示されることを確認
   （root の `useTaskdStream` が再接続後の `task.event` を受けて再検証）。
4. **20 秒ワーカー実行中は in_flight に task/run_id/provider/経過時間が出て、終了後に消える** — `e2e/g4.spec.ts:275` pass（21.9s）。`Slow-H` を承認後、
   `WorkerStarted` を確認してから `/daemon` を開くと `in-flight-row`（`data-task-id`）に `in-flight-provider`=`fake-local`、`in-flight-run-id` が非空、
   `in-flight-elapsed` が数値を含む文字列で表示。`done` になった後に reload すると同じ行が 0 件になる。

### 共通条件
- `pnpm lint` exit 0（`Checked 81 files`）/ `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0 / `pnpm test` **126 passed**（18 ファイル。G3 までの 118 + G4 の 8）/
  `pnpm build` exit 0 / `pnpm gen:types && git diff --exit-code app/taskd/types.ts` 差分ゼロ（exit 0。G4 は API 追加が無いため型生成物への影響なし）
- `pnpm e2e`: **31 件中 29〜31 passed**（直近 2 回のフルラン）。`e2e/g4.spec.ts` の 5 シナリオは 2 回とも 5/5 pass。落ちたのは両回とも `e2e/g2.spec.ts`
  （受け入れ条件 2 または 5）で、G2-U1（`docs/taskd-requests.md` R1、taskd の間欠停止）と一致するパターン（`toHaveText`/`toHaveAttribute` のタイムアウト、
  または keep-alive コネクションの再利用に起因する `fetch failed`）。G4 の変更・fixture が原因の失敗は観測していない。

### 監査結果
- G4 の作業セッションは監査前に中断したため、G4 単独の auditor 監査は未実施。G5 の auditor 監査（1 回）に「G4 の受け入れ条件 1〜4 の実装が
  `docs/DESIGN.md` §10 Phase G4 と一致するか」の確認を含めて依頼し、その結果を G5 の節に記す。

### 未解決事項
- **G4-U1: `multi-account` フィクスチャは他と非対称**（設計上の制約であり不具合ではない）— cooldown が taskd プロセス内メモリのみで DB から再構築できないため
  （ADR-0007 D1）、`scripts/taskd.sh fixture multi-account` は DB を作らず設定だけを用意し、スロットルを起こす操作は `e2e/g4.spec.ts` が生きたプロセスに対して
  直接行う。他の fixture（`--until-idle` で DB を作ってから任意のタイミングで `start`）と挙動が異なる点を知らずに使うと「DB が空で驚く」ことになりうるので、
  `scripts/taskd.sh` のコメントと ADR-0007 D1/D4 に明記した。
- **G4-U2: `taskd-banner` が 2 重に描画されるルートがある** — `/daemon`・`/providers` は自身の `ErrorBoundary` でも `TaskdBanner` を出すため、root のものと合わせて
  DOM に 2 つ描画される（表示内容は同じ）。G0〜G3 では単一ルートでしか確認していなかったため気づいていなかった。実害は無い（見た目は同じバナーが縦に並ぶだけ）が、
  G5 で a11y チェック（`@axe-core/playwright`）を入れる際に `[role=alert]` の重複が指摘される可能性があるので留意する。
- **G4-U3: `/providers` の cooldown 残り時間の基準時刻は BFF のリクエスト時刻**（`fetchedAt`）— `Providers` 応答自体に `now` が無いため、loader が
  `new Date().toISOString()` を挟んで基準にしている。`/daemon` は `DaemonView.now`（taskd 自身の時刻）を使っており基準が異なる（後者の方が正確）。
  タブを開いたまま長時間放置すると `/providers` の残り時間はページ再読込までのブラウザ・サーバ間のクロックのずれの影響を受けうる（実運用で問題になるほどの
  ずれは想定していない）。
- **G4-U4: `/daemon` の in_flight・cooldowns テーブルの行数が多くなった場合の表示は未検証** — G4 の fixture 規模（同時 1〜2 件）でしか確認していない。
  G5 以降でプロバイダ数・同時実行数が増える場面があれば、テーブルの折り返し・ページングを検討する。
- G0〜G3 からの引き継ぎ（`/assets` の Host 検査適用範囲、`pnpm dev` の CSP、G2-U1 taskd 間欠停止、G2-U2〜U7、G3-U1〜U8）は G4 では対処していない。

### 提案
- 上の「提案」節の G0-P1/P2、G1-P1/P2、G2-P1〜P4、G3-P1/P2 に加え、G4-P1: `docs/taskd-api-v1.md` §3.20 の cooldown の説明に「`ProviderPolicy` の実装（`StaticPolicy`）は
  プロセス内メモリのみで、`ProviderThrottled` イベントから起動時に再構築されない」という実装事実を明記すると、GUI 側だけでなく `taskctl` 利用者にも
  「プロセスを再起動すると cooldown が消える」という挙動が文書から分かるようになる（ADR-0013 D9 は `reason` の語彙は定義しているが、この非永続性には触れていない）。

### taskd への依頼
- なし。`GET /providers`・`GET /daemon` は `docs/taskd-api-v1.md` §3.19〜§3.21 の記載どおりに動作した。`ProviderFailure::Throttled` によるフォールバックと
  cooldown の記録（`ProviderThrottled` イベント）、`awaiting_human`/`unroutable` の毎 tick 再計算も文書と実挙動が一致した。

## Phase G5 — DONE（2026-09-15）

前セッションが `PARTIAL` で残した状態（実装と文書の下書きはあるが検証・監査・コミット前。implementer の成果 2 単位が worktree に未マージ）から再開し、
worktree の取り込み → taskd の更新（`actions`）の取り込み → 全検証 → 監査 → コミットまでを行った。

### 成果物
- 認証（docs/DESIGN.md §8.2、ADR-0008 D1〜D6）: `app/auth.server.ts`（`readAuthConfig`: 非 loopback バインドでパスワードファイル無しなら例外、パスワードファイル明示で
  loopback でも認証を要求 / `verifyPassword`: SHA-256 ダイジェスト同士の `timingSafeEqual` / `issueSessionCookie`・`hasValidSession`・`clearSessionCookie`: react-router の
  `createCookie`（HMAC 署名、`HttpOnly; SameSite=Strict; Path=/`、https なら `Secure`、`Max-Age` 24h）/ `authCheck` middleware: 未認証は `/events`・`/files/*` が 401、
  他は 302 `/login?next=`、`/login`・`/logout`・`/healthz` は対象外 / `safeNextPath`: 同一オリジンの絶対パスだけ）。`app/routes/login.tsx`（フォーム。失敗は 1 秒待って
  status 401 で再描画）、`app/routes/logout.ts`（POST でクッキー削除 → `/login`）。`app/root.tsx`: middleware 順序を Host → 認証 → CSRF → ヘッダに、未認証時は
  taskd を呼ばずナビゲーションもフッタも出さない、ナビゲーションにログアウト、taskd の 401 をバナー（「taskd が要求を拒否しました … 401 unauthorized」）で表示
  （root loader の `GET /inbox` が 401 のとき、または子ルートが 401 の `Response` を投げたときの `ErrorBoundary`）。`app/hooks/useTaskdStream.ts` に `enabled` オプション
  （未認証時は `/events` を張らない）。`server.js`: 起動時検証（非 loopback + パスワードファイル無し → exit 2、パスワード / セッション鍵 / トークンの各ファイルが
  読めない・空 → exit 2）、起動ログに auth / token の有無（値は出さない）。
- トークン（§8.1）: `TaskdClient.fromEnv` は G0 から `TASKD_API_TOKEN_FILE` を読んでいたので変更なし。`scripts/taskd.sh fixture auth`（`test/taskd/auth.toml.tmpl` =
  既定テンプレート + `[api] token_file = "api.token"`。`.run/auth/api.token` に 32 バイトの乱数を hex で書く）を追加。
- CSP / a11y（§8.2、ADR-0008 D7/D8）: `e2e/test.ts`（全 spec が import する `test` ラッパー。auto fixture がコンソールの CSP 違反を集め、各シナリオ終了時に 0 件を assert）、
  `e2e/g5-a11y.spec.ts`（`@axe-core/playwright` 4.13.0 で 6 画面を走査、critical / serious 0 件と CSP ヘッダの有無）。`biome.json` に `!.claude`（サブエージェントの
  worktree が `.claude/worktrees/` に作られると Biome が「nested root」で止まるため）、`.gitignore` に `.claude/worktrees/`。
- 配布（§9、ADR-0008 D9〜D11）: `scripts/release.sh`（`pnpm release` → `dist/taskd-gui-<version>.tar.gz`）、`deploy/taskd-gui.service`、`README.md`（導入手順・環境変数・
  セキュリティ要点）、`Dockerfile` + `.dockerignore`（任意。build は未実行）、`e2e/g5-release.spec.ts`（tar を空ディレクトリに展開 → `pnpm install --prod --frozen-lockfile
  --ignore-scripts --offline` → `node server.js` → `/` が 200）。
- taskd の更新の取り込み（ADR-0008 D14）: `pnpm gen:types` を再生成（`Action` 型、`TaskRef.actions`、`TaskSummary.actions`。taskd の ADR-0015 D4）、
  `scripts/capture-fixtures.sh` で `test/fixtures/api/*.json` を再採取、`app/routes/inbox.tsx` の attention 区画の cancel 判定を `item.task.actions.includes("cancel")` に
  置き換え（G2-U6 の解消。§5.4 の規則の GUI 側再実装をやめた）。`test/unit/tasks.loader.test.ts` / `tasks.detail.action.test.ts` のサンプルに `actions` を追加。
- R1 の恒久対応（ADR-0008 D13）: `scripts/taskd.sh` の `RUN_ROOT` を `TASKD_RUN_ROOT` で上書き可能にし、既定をローカルディスク（`${TMPDIR:-/tmp}/taskd-gui-run-$USER`）に、
  `.run` はそこへのシンボリックリンクにした（既存のリンクがあればその先を使う）。ネットワーク FS（nfs/cifs/fuse）上なら警告。`docs/taskd-requests.md` の R1 を回答済みに更新。
- 監査後の修正（ADR-0008 D15。G0 監査からの引き継ぎ「G5 の条件 3 までに解消」の完了）: catch-all ルート `app/routes/$.tsx`（`route("*")`。未定義パスも root middleware を通り、
  root の ErrorBoundary が nonce 付き CSP で 404 を描く）、`server.js` に Express 層の Host 検査（許可リスト外は 400。静的アセットにも効く）と既定ヘッダ
  （`writeHead` 直前に「無ければ」付ける: CSP `default-src 'none'; frame-ancestors 'none'; base-uri 'none'`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer`）。
  `authCheck` の 401 に nosniff。未認証時の root loader は `taskdApiUrl` も返さない（`/login` の hydration payload に接続先が載らない）。
- Node SEA の実験（ADR-0008 D11）: 結果は下の「未解決事項」G5-U1 に記録。
- テスト: `test/unit/auth.test.ts`（15 件）、`e2e/g5.spec.ts`（受け入れ条件 1〜3。0.0.0.0 バインドの GUI を spec 内で起動）。
- 文書: `docs/adr/0008-g5-decisions.md`（D1〜D14）。
- 実装単位: 「配布一式（release.sh / service / README / Dockerfile / release smoke）」と「CSP ガード + a11y スキャン（e2e/test.ts / 既存 spec の import 差し替え /
  g5-a11y.spec.ts）」は互いに（そして認証とも）ファイルを共有しない独立単位だったので、前セッションで implementer 2 体を worktree で並列実行した。その成果は worktree に
  未マージのまま残っていたので、このセッションで main に取り込み（`e2e/g5-release.spec.ts` の import を `./test` に統一、`e2e/g5-a11y.spec.ts` の worktree 固有の
  パス解決を除去）、worktree とブランチを削除した。認証・トークン・root の変更・`server.js`・`scripts/taskd.sh`・`e2e/g5.spec.ts`・`actions` の取り込み・RUN_ROOT は
  設計判断とファイル共有のため自分で実装した。

### 受け入れ条件と証拠（docs/DESIGN.md §10 Phase G5）
1. **非 loopback バインドのパスワード認証** — `e2e/g5.spec.ts`「受け入れ条件 1」3 シナリオ（`pnpm e2e` に含む、全て pass）:
   (a) `TASKD_GUI_BIND=0.0.0.0:7731 TASKD_GUI_PASSWORD_FILE= node server.js` → **exit 2**、stderr に `TASKD_GUI_PASSWORD_FILE is required`。
   (b) `TASKD_GUI_BIND=0.0.0.0:7721` + パスワードファイルで起動した GUI に対し、未ログインの `GET /` → **302 `/login`**、`GET /tasks?limit=5` → 302 `/login?next=%2Ftasks%3Flimit%3D5`、
   クッキー無しの `GET /events` → **401** 本文 `unauthorized`、`/login` は 200 で CSP ヘッダあり。
   (c) ブラウザで誤パスワード → 1 秒以上待って `login-error` 表示（`POST /login` の status **401**、本文「パスワードが違います」）。正しいパスワードで `POST /login` → 302 `/tasks`、
   `Set-Cookie: __taskd_gui_session=…; HttpOnly; SameSite=Strict; Path=/`（http なので `Secure` 無し）。クッキー付きで `/` が 200、ログアウトで再び 302。
   単体: `test/unit/auth.test.ts` 15 件（設定の読み取り・loopback 判定・定数時間比較・クッキー発行/検証/失効・`next` の検証・middleware の 302/401/通過）。
2. **BFF → taskd のトークン** — `scripts/taskd.sh fixture auth && start auth`（`[api] token_file = "api.token"`、32 バイト hex）。`e2e/g5.spec.ts`「受け入れ条件 2」3 シナリオ（全て pass）:
   前提確認として taskd 自身への `GET /api/v1/inbox` がトークン無しで **401**。`TASKD_API_TOKEN_FILE=.run/auth/api.token` の GUI（7722）で `/tasks` が 200、バナー無し、
   `Auth-A` が表示、フッタに `api_version 1`。トークン無しの GUI（7723）で `/` が **401** でバナーに **`unauthorized`**（「taskd が要求を拒否しました … 401 unauthorized」）。
   トークン文字列で `build/` 配下全ファイルと `.run/*/gui*.log`（2 本以上）を走査 → **一致 0 件**。HTML 本文にも含まれない。
3. **Host 検査と CSP** — `e2e/g5.spec.ts`「受け入れ条件 3」: `Host: evil.example` の `GET /` → **400**。`/`, `/tasks`, `/tasks/new`, `/plans/new`, `/daemon`, `/providers`, `/graph`, `/healthz` の
   応答に `Content-Security-Policy`（`default-src 'self'` … `frame-ancestors 'none'`）と `X-Content-Type-Options: nosniff`。`e2e/g5-a11y.spec.ts` でも 6 画面（`/tasks/<id>` 含む）で確認。
   監査の指摘 1 を受けて追加（ADR-0008 D15）: 未定義パス `/no-such-page` → **404** で `Content-Security-Policy` に `nonce-` を含み `nosniff`、本文に「404」（`e2e/g5.spec.ts` 条件 3 で assert）。
   `Host: evil.example` の `/assets/` → **400**（同 spec）。未ログインの 302 `/` と 401 `/events` にも CSP（`default-src 'none'`）と `nosniff`（同 spec 条件 1）。
   curl での自己再監査（GUI 7705、パスワード付き）: `/`→302、`/assets/x.js`→302、`/events`→401 の各応答に CSP 1 本 + nosniff + `Referrer-Policy`、ログイン後の `/` と `/nope`（404）は
   nonce 付き CSP 1 本、実アセット `/assets/entry.client-*.js` は 200 + `immutable` + CSP `default-src 'none'` + nosniff、`Host: evil.example` のアセット要求は 400、
   `/login` の HTML に `7710` は 0 件。
   **CSP 違反 0 件**: `e2e/test.ts` の auto fixture（`page.on("console")` / `pageerror` で `Content Security Policy` を含むメッセージを収集し teardown で `[]` を assert）を
   全 spec（g0〜g5、`./test` から import）に適用。`pnpm e2e` 全シナリオが pass = 全シナリオで違反 0 件。
4. **a11y** — `e2e/g5-a11y.spec.ts`「受け入れ条件 4」6 シナリオ（`/`, `/tasks`, `/tasks/<id>`, `/tasks/new`, `/providers`, `/daemon`）で `@axe-core/playwright` 4.13.0 の
   critical / serious が **0 件**。初回ランでは **critical 3 件**（`/` の `textarea[name=note]`・`textarea[name=answer]` に label 無し、`/tasks/new` の `select[name=criterion_type]` に
   accessible name 無し）を検出 → `aria-label` を付与（`criterion_value` にも付与）→ 再ランで 0 件。moderate / minor は annotation に記録（下記 G5-U3）。
5. **依存の監査とリリース** — `pnpm audit --audit-level=high` → **`No known vulnerabilities found`、exit 0**（registry への問い合わせなので e2e の外で 1 回実行）。
   `pnpm release`（`scripts/release.sh`）→ `dist/taskd-gui-0.1.0.tar.gz`（`build/`, `server.js`, `package.json`, `pnpm-lock.yaml`, `pnpm-workspace.yaml`, `README.md`,
   `deploy/taskd-gui.service`, `Dockerfile`）。`e2e/g5-release.spec.ts` 1 シナリオ: `dist/release-smoke-*/` に展開 → `pnpm install --prod --frozen-lockfile --ignore-scripts --offline`
   → `node server.js`（7703）→ `GET /` が **200**、フッタに `taskd-gui 0.1.0`（pass）。
6. **配布文書** — `deploy/taskd-gui.service`（非 root、`ProtectSystem=strict` 等）、`README.md`（導入手順・環境変数表・SSH ポートフォワード・セキュリティ要点）。
   （任意）`docker build` は **未実行**（ADR-0008 D10。registry へのアクセスが要る。`Dockerfile` と `.dockerignore` は同梱）。（実験）Node SEA は下記 G5-U1 に記録。

### 共通条件
- `pnpm lint` exit 0（`Checked 90 files`）/ `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0 / `pnpm test` **141 passed**（19 ファイル。G4 までの 126 + `auth.test.ts` 15）/
  `pnpm build` exit 0 / `pnpm e2e` **51 passed（exit 0、5.4 分。監査後の修正込みの最終ラン）**（G0 5 + G1 8 + G2 8 + G3 5 + G4 5 + G5 20: `g5-a11y` 12（CSP ヘッダ 6 + a11y 6）+ `g5-release` 1 + `g5` 7）。
  経緯: 1 回目 49 passed / 2 failed（a11y の critical 3 件 → `aria-label` で修正）、2 回目 6 passed / 12 failed（1 回目が残した `auth` インスタンスが 7710 を占有。G5-U8 で解消）、
  3 回目 51 passed（監査者の再実行も 51 passed / 5.5 分）。監査後に D15 の修正と spec の追記を入れて 4 回目 51 passed。全シナリオが `e2e/test.ts` の CSP ガードを通過（違反 0 件）
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts` 差分ゼロ（exit 0。taskd の schema の更新（`actions`）を取り込んで再生成した後の状態）

### 監査結果
- auditor の判定: **条件付き可**（「不可」ゼロ）。受け入れ条件 1・2・4・5・6 と共通完了条件 1・2・4 は「満たしている」、条件 3 は「部分的に満たしている」、
  共通条件 5（`git status` クリーン + `phase G5:` コミット）は監査時点では未達（コミット前なので想定どおり）。禁止事項 9 項目は全て「違反ゼロ」。
  auditor 自身が `lint`（89 files）/ `typecheck` / `test`（141 passed）/ `build` / `gen:types` 差分ゼロ / `pnpm audit`（0 件）/ `pnpm e2e`（**51 passed / 5.5 分**）を再実行し、
  GUI を 7705 で起動して 302 / 401 / CSP / `Host` を curl でも確認した。ファイルの変更無し、taskd インスタンスは監査前後とも全停止。
- 指摘と対応:
  1. **（重要）未定義パスの 404 HTML・静的アセット・302/401 応答に CSP / nosniff / Host 検査が掛からない**（G0 監査で「G5 の条件 3 までに解消」と約束していた項目が未解消で、
     未解決事項にも再掲されていなかった）→ **コードで修正**（ADR-0008 D15: catch-all ルート + Express 層の Host 検査と既定ヘッダ + 401 の nosniff）。
     最初の実装（全応答に `setHeader`）は React Router のアダプタが `appendHeader` で足すため CSP が 2 本になった（複数 CSP は交差 = 最も厳しい方で画面が壊れる）ので、
     `writeHead` 直前に「無いときだけ」付ける方式に直した。curl による再監査の結果は条件 3 の証拠に記載。`e2e/g5.spec.ts` に `/no-such-page` 404 + nonce CSP、`Host: evil` のアセット 400、
     302 / 401 のヘッダの assert を追加。
  2. **ADR-0008 D5「接続先を出さない」と実装の不一致**（未認証時も `gui.taskdApiUrl` を返し、`/login` の hydration payload に `127.0.0.1:7710` が載る）→ **修正済み**
     （未認証時は `taskdApiUrl: ""`。curl で `/login` の HTML に `7710` が 0 件）。
  3. `docs/taskd-api-v1.md` が taskd の `docs/gui/api.md` より古い → 既に G5-U6 と「taskd への依頼」に記載（GUI 側では書き換えない）。
  4. `readGuiConfig` がバインドホストを無条件に許可するため `0.0.0.0` バインドでは `Host: 0.0.0.0` が通る → 下記 G5-U9 として記録、README の非 loopback 節に
     `TASKD_GUI_ALLOWED_HOSTS` の設定を推奨として追記。
  5. `pnpm e2e` の所要時間の差（5.2 分 vs 5.5 分）→ 件数・exit code は一致。記述を更新。
- 修正後の自己再監査: `pnpm lint` exit 0（90 files）、`pnpm typecheck` exit 0、`pnpm test` 141 passed、`pnpm build` exit 0、`pnpm e2e` 51 passed（4 回目）、curl の結果は条件 3 の証拠に記載。
  auditor の再起動は行っていない（「不可」が無く、指摘 1・2 は自分で再検証できる範囲）。

### 未解決事項
- **G5-U1: Node SEA の実験結果（ADR-0008 D11。成否は問わない）** — 手順と結果:
  1. `rolldown` 1.2.7（vite 8 の推移的依存。新しい依存は足していない）で `server.js` をそのまま `--format cjs` にバンドル → **失敗**（`Top-level await is currently not supported
     with the 'cjs' output format`。`server.js` の `await import(...)` 3 箇所）。
  2. TLA を静的 import に置き換え vite の開発分岐を外した入口（`sea-entry.mjs`）を `--format cjs --inlineDynamicImports` でバンドル → **成功**。リポジトリ内から解決すると
     **4.16 MB の単一 CJS**（react-dom/server, express, react-router 等を同梱。外部 `require` は Node 組み込みと `debug` の任意依存 `supports-color` のみ）。
     `node_modules` の無いディレクトリに置いて `node server-full.cjs` → `GET /` が **200**。
  3. `node --experimental-sea-config sea-config.json` → `sea-prep.blob`（206 KB / 4.2 MB）を生成 **成功**。
  4. 実行ファイルへの注入は **未達**: Node 24.21 に `--build-sea` は無く、注入には npm の `postject` が要る（未インストール。取得に registry アクセスが要るので行わなかった）。
  5. 別途、`build/client`（静的アセット）は SEA の `assets` として埋め込み `sea.getAsset()` で配信する作りに `server.js` を変える必要がある（今の `express.static` はディスク前提）。
  結論: 「サーバ 1 ファイル化」は可能、「単一バイナリ」は postject とアセット埋め込みの 2 点が残る。G6 以降の任意項目として扱う。
- **G5-U2: CSP の `style-src 'unsafe-inline'` は外せない**（ADR-0008 D7）— `@xyflow/react` と `@tanstack/react-virtual` がインライン style 属性を使い、属性は nonce で許可できない。
  `style-src-elem`（nonce）/ `style-src-attr 'unsafe-inline'` に分ける案は CodeMirror の `style-mod` への `EditorView.cspNonce` 配線が必要で未着手。
- **G5-U3: a11y の moderate / minor は列挙していない** — `e2e/g5-a11y.spec.ts` は `test.info().annotations` に記録するが `list` reporter には出ない。件数を把握するには
  `--reporter=json` で 1 回走らせて集計する（ゲートは critical / serious のみ。ADR-0008 D8）。
- **G5-U4: `docker build` 未実行**（ADR-0008 D10）。`Dockerfile` は `node:24-slim` + corepack 前提で、`pnpm install --frozen-lockfile` に registry が要る。
- **G5-U5: TLS 終端を前に置く構成は想定外** — `Secure` は要求 URL が `https:` のときだけ付き、`trust proxy` は無効（ADR-0008 D3）。リバースプロキシ配下では
  `X-Forwarded-Proto` を見ないので `Secure` が付かない。README に注記済み。
- **G5-U6: `docs/taskd-api-v1.md` が taskd の `docs/gui/api.md` より古い** — taskd の ADR-0015 D4 で §5.4 / §6.2 に `TaskRef` / `TaskSummary` の `actions` が追記されたが、
  GUI 側のコピーは bootstrap 時のまま。CLAUDE.md により GUI 側では書き換えない（`app/taskd/types.ts` は再生成済みで `actions` を含む）。同期は「taskd への依頼」に記載。
- **G5-U7: release smoke の `--offline` install は環境依存** — pnpm の store はファイルシステムごとに分かれるため、展開先を `dist/`（リポジトリと同じ FS）にしてある。
  store が温まっていない環境（CI の初回）では `--offline` が失敗する。CI では `pnpm fetch` 等で先に store を作るか、`--offline` を落とす。
- **G5-U8: `pnpm e2e` は約 6.5 分で、spec の実行順（alphabetical: g0 → g1 → … → g5-a11y → g5-release → g5）と taskd インスタンスの引き渡しに依存している** —
  当初 `e2e/g5.spec.ts` が `auth`（7710）を残して終わる作りだったため、2 回目のランで g0 の `start dev` と各 fixture が「別プロセスが応答中」で連鎖的に失敗した
  （12 failed）。g5 の afterAll で全インスタンスを止め、g0 の beforeAll で既知のインスタンスを全て止めるようにして解消。spec を追加するときは同じ規約に従うこと。
- **G5-U9: `Host` 許可リストにバインドのホストが無条件で入る** — `TASKD_GUI_BIND=0.0.0.0:7700` なら `Host: 0.0.0.0` が通る（`app/config.server.ts` と `server.js` の両方）。
  実害は DNS rebinding で `0.0.0.0` を名乗る必要がある点で限定的だが、非 loopback 公開時は `TASKD_GUI_ALLOWED_HOSTS` を設定し、`0.0.0.0` / `::` はリストに入れない
  ようにするのがよい（次フェーズで検討）。
- 引き継ぎ（未対処）: G4-U1〜U4 はそのまま。G3-U1（`/graph` のスクリーンショット比較の環境依存）もそのまま。G2-U6 は本フェーズで解消（ADR-0008 D14）。

### 提案
- G5-P1: `docs/DESIGN.md` §8.2「非 loopback → パスワード必須、loopback → 認証無し」に「`TASKD_GUI_PASSWORD_FILE` が明示されていれば loopback でも認証を要求する（opt-in）」を追記する
  （ADR-0008 D2。設定したのに効かない状態を避けるため）。
- G5-P2: §8.2 CSP の「`style-src 'unsafe-inline'` … G5 で外せるか確認」→ 確認結果は「外せない（style 属性）」。次の一手として「`style-src-elem 'self' 'nonce-…'; style-src-attr 'unsafe-inline'`
  に分割し CodeMirror に `cspNonce` を配線する」を任意項目として書くとよい。
- G5-P3: §10 Phase G5 条件 5 の `pnpm install --prod --frozen-lockfile --ignore-scripts` は、e2e の「外部ネットワークに出ない」規則と合わせて `--offline`（開発機の store を使う）と明記するとよい。
- G5-P4: §9 / §10 の「`node --build-sea`」は Node 24 には無い（`--experimental-sea-config` + `postject`）。文言を直すか、単一バイナリを G6 以降の任意項目に移す。
- G5-P5: §10.0 の環境前提に「`.run/`（taskd の DB）はローカルディスクに置く（NFS 不可）。`scripts/taskd.sh` の `TASKD_RUN_ROOT`」を追記する（R1 の教訓。ADR-0008 D13）。
- G5-P6: §8.2「失敗は 1 秒待つ」は同時多数の試行に対しては抑止にならない（待つだけで並列度は制限しない）。単一利用者・loopback 前提なら十分だが、非 loopback 公開時は
  前段（SSH / リバースプロキシ）でのレート制限を README に推奨として書いた。DESIGN 側にも「レート制限は前段で」と明記するとよい。

- **G5-P1〜P6 は 2026-09-15 に人間の許可を得て `docs/DESIGN.md` に反映済み**（GUI のエージェントは編集できない規約のため、taskd 側のオーケストレータが行った）。
  あわせて `docs/taskd-api-v1.md` を taskd の `docs/gui/api.md` と同期した（G5-U6 の解消）。

### taskd への依頼
- BLOCKED になる不足・仕様違いは無し。`[api] token_file` 付きの taskd は `docs/taskd-api-v1.md` §1.3 のとおり `GET /health` だけ無認証で、他はトークン無しで 401 `unauthorized` を返した。
- **依頼（文書の同期）**: GUI 側の `docs/taskd-api-v1.md` は taskd の `docs/gui/api.md`（ADR-0015 で `actions` を §5.4 / §6.2 に追記、運用ログの節を追加）より古い。GUI 側では書き換えない
  規則なので、taskd 側（または人間）でコピーを更新してほしい（G5-U6）。
- R1 は回答済み（原因は NFS 上の DB。GUI 側は ADR-0008 D13 で対応）。`docs/taskd-requests.md` を更新した。

## Phase G6 — DONE（2026-09-16）

### 成果物
- 画面: `app/routes/help.tsx`（`/help`。loader 無しの静的ページ、docs/adr/0009-g6-decisions.md D1）。6 節（`#flow` 3 分で分かる流れ、`#screens` 画面ごとの説明、
  `#acceptance` 受け入れ条件の 4 種類、`#status` 状態と人間ができること、`#glossary` 用語集、`#trouble` 困ったとき）。内容は `docs/taskd-api-v1.md` §3.4 / §5.4 と
  `app/taskd/types.ts` の `Status` の語彙に合わせた（D2〜D4）。
  `app/components/HelpLink.tsx`（各画面の見出し横の `/help#screens` への「?」リンク。対象は DESIGN §10 Phase G6 が列挙する 6 画面だけ、ADR-0009 D2）。
  `app/routes.ts` に `/help` を登録、`app/root.tsx` のナビゲーションに「使い方」を追加。
  `app/routes/inbox.tsx`: `h1` を新設して HelpLink を追加、受信箱の 4 区画（承認待ち・質問・draft・注意）が全て 0 件のとき「使い方を見る」への導線
  （`inbox-empty-help` / `inbox-help-onboarding-link`）を表示（ADR-0009 D3）。`app/routes/tasks.tsx` / `app/routes/tasks.$id.tsx` / `app/routes/providers.tsx` /
  `app/routes/daemon.tsx` の見出しに HelpLink を追加、`app/routes/graph.tsx` は `h1`（これまで無かった）を新設して HelpLink を追加。
- taskd のスキーマ追従（G7 の先取りはしない、ADR-0009 D5）: taskd 側が `docs/taskd-api-v1.md` の反映（Phase 10 役割と委譲）より先の Phase 11（プロバイダ管理）・
  Phase 12（クラスタ）まで進んでいたため、`pnpm gen:types` で `app/taskd/types.ts` を再生成すると `AttentionItem` に `cluster_unavailable`（`task` を持たない）、
  `TaskDetail` に必須の `delegated` が増えて型が壊れた。画面機能は実装せず、型を壊さない最小限の対応だけ行った:
  `app/routes/inbox.tsx` の注意区画で `item.type === "cluster_unavailable"` を先に分岐（`/clusters` への遷移や専用の見た目は実装しない）、
  `e2e/g4.spec.ts` の unroutable フィクスチャ検索を型ガード付きに、`test/unit/tasks.detail.loader.test.ts` のフィクスチャに `delegated: []` を追加。
  `test/fixtures/api/*.json` は `scripts/capture-fixtures.sh` を実 taskd（`fixture basic`）に対して再実行しただけ（手書き修正ではない）。
- 既存バグの修正: `server.js` の `res.writeHead` 差し替え箇所が TypeScript 7 のオーバーロード解決に失敗して `pnpm typecheck` が exit 1 だった
  （G6 の変更前から存在。`git stash` で確認済み）。ロジックは変えず JSDoc の型注釈だけ直した（ADR-0009 D6）。
- テスト: `e2e/g6.spec.ts`（受け入れ条件 1〜4 の 5 シナリオ）。`e2e/g3.spec.ts-snapshots/graph-basic-chromium-linux.png` を `graph.tsx` への `h1` 追加に伴い再生成。
- 文書: `docs/adr/0009-g6-decisions.md`（D1〜D6）。
- 実装単位: G6 は `/help` 1 画面が中心で、既存画面への `HelpLink` 追加・型追従の後始末は互いにファイルを共有しない独立単位ではなかった
  （`inbox.tsx` は HelpLink・空受信箱導線・`cluster_unavailable` 対応の 3 つが同じファイルに重なる、型追従は複数ファイルに波及する）ため、implementer は使わず自分で実装した。

### 受け入れ条件と証拠（docs/DESIGN.md §10 Phase G6。`e2e/g6.spec.ts`、実 taskd `basic` に対して検証）
1. **`/help` が 200 で、6 節の見出しと `id` が全て存在する** — `e2e/g6.spec.ts:46`「受け入れ条件 1」pass。`page.goto("/help")` の応答 status 200、
   `#flow` / `#screens` / `#acceptance` / `#status` / `#glossary` / `#trouble` の各見出しテキストを確認。
2. **ナビゲーションから 1 クリックで開ける。受信箱が空のとき導線が出て、押すと `/help` に遷移する** — `e2e/g6.spec.ts:57,64`「受け入れ条件 2」2 シナリオ pass。
   `/tasks` から `使い方`（exact）リンクをクリックして `/help` に遷移。`basic`（承認待ち等が存在）では `inbox-empty-help` が 0 件、`dev`（タスクを足していない
   常に空の instance）に切り替えると `inbox-empty-help` が表示され `inbox-help-onboarding-link` のクリックで `/help` に遷移。
3. **`/help` 内のリンクが全て 200** — `e2e/g6.spec.ts:83`「受け入れ条件 3」pass。本文内の `/` 始まりリンク（`/`, `/tasks`, `/tasks/new`, `/plans/new`,
   `/daemon`, `/providers`, `/graph`）を全て `page.request.get` して status 200 を確認。
4. **`@axe-core/playwright` で `/help` の critical / serious が 0 件、CSP 違反 0 件** — `e2e/g6.spec.ts:103`「受け入れ条件 4」pass（gating な violation 0 件）。
   CSP 違反 0 件は `e2e/test.ts` の auto fixture が全 spec に効かせている（`pnpm e2e` 全シナリオ pass = 違反 0 件、ADR-0008 D7）。
   既存の `e2e/g5-a11y.spec.ts`（`/`, `/tasks`, `/tasks/<id>`, `/tasks/new`, `/providers`, `/daemon`）も再実行し 12 passed（HelpLink・`h1` 追加による回帰なし）。
5. **受け入れ条件・状態・用語の説明が `docs/taskd-api-v1.md` の語と一致** — auditor が確認（下記監査結果）: `command`/`artifact_exists`/`reviewer`/`human` の型名と
   例が §3.4 と一致、`expect_exit` 既定 0 が taskd の JSON Schema の `default` と一致、状態 8 種が `app/taskd/types.ts` の `Status` と一致、
   人間ができることが §5.4 の `actions` 規則（approve: draft または approval&ready、reject: approval&ready、answer: blocked、cancel: 非終端）と一致。
6. **`pnpm lint` / `pnpm typecheck` / `pnpm test` / `pnpm build` / `pnpm e2e` が exit 0、`pnpm gen:types` の差分ゼロ** — 下記「共通条件」参照。

### 共通条件
- `pnpm lint` exit 0（`Checked 93 files`）/ `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0 / `pnpm test` **141 passed**（19 ファイル。G5 までと同数、
  G6 は新規の単体テストを追加していない。静的ページと型のみの変更のため e2e でカバー）/ `pnpm build` exit 0
- `pnpm e2e` **56 passed（exit 0、約 5.6 分）**（G0 5 + G1 8 + G2 8 + G3 5 + G4 5 + G5 20 + G6 5）。監査者による再実行でも 56 passed（8.3 分）。
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts`: コミット前は差分あり（taskd 側のスキーマが Phase 11/12 まで進んでいたため、型を再生成して取り込んだ。
  上の「成果物」参照）。コミット後に再実行して差分ゼロを確認（下記コミット後の検証）。

### 監査結果
- auditor の判定: **条件付き可**（「不可」ゼロ）。受け入れ条件 1〜6 は全て「満たしている」。禁止事項（SQLite 直読み・crate 依存・仕様外挙動・ブラウザ直接呼び出し・
  トークン露出・`dangerouslySetInnerHTML`/`eval`/CDN・テストの外部ネットワーク・依存の新規追加）は「違反なし」。`cluster_unavailable` 対応は
  「最小限に留まっており G7 の先取りは無い」、`server.js` の修正は「挙動変更なし（JSDoc 1 行のみ）」と確認済み。auditor 自身が `lint`/`typecheck`/`test`（141 passed）/
  `build`/`pnpm e2e`（56 passed）/`gen:types`（taskd `../agent-platform` のスキーマとバイト一致）を再実行して確認した。
- 指摘と対応:
  1. 「`docs/PROGRESS.md` に Phase G6 の節が無い」→ **本コミットで対応**（この節を追加）。
  2. 「`docs/adr/0009-g6-decisions.md` D6 の記述が実コード（JSDoc のインライン注釈）と食い違う」→ **修正済み**（D6 を実コードに合わせて書き直した）。
  3. 軽微指摘「`/help` の 401 の説明がページ遷移（302）と `/events`/`/files/*` の直接 401 を区別していない」→ **修正済み**（`#trouble` の 401 説明を書き直し、
     通常ページはログイン画面に戻ること、SSE・成果物取得はその場で 401 になることを明記）。
- 修正後の自己再検証: `pnpm lint` exit 0、`pnpm typecheck` exit 0、`pnpm test` 141 passed、`pnpm build` exit 0、`e2e/g6.spec.ts` 5 passed、
  `e2e/g3.spec.ts` の DAG スクリーンショットも pass（再生成後）。auditor の再起動は行っていない（「不可」が無く、指摘は全て自分で再検証できる範囲）。

### 未解決事項
- **G6-U1: taskd のスキーマが Phase 10（役割と委譲）・11（プロバイダ管理）・12（クラスタ）まで進んでいる**（上の「成果物」参照）— `app/taskd/types.ts` は
  追従済みだが、対応する画面（`/clusters`、タスク詳細の `role`/`delegated[]`、作成フォームの `role`/`aggregate`、プロバイダ管理 UI）は未実装。
  `docs/DESIGN.md` §10 Phase G7（クラスタと委譲の表示）が対応する範囲。プロバイダ管理（ADR-0017、`POST/PATCH/DELETE /providers`、`POST /reload`）は
  DESIGN のどの G フェーズにも明記が無いため、次フェーズ着手前に人間に確認したい（下記「提案」G6-P1）。
  taskd 側は本フェーズの作業中も開発が続いており、監査後にさらに ADR-0019（`TaskDetail.worktree`、`sync = "worktree"` のクラスタ）が追加された
  （任意フィールドで既存コードに影響なし。`pnpm gen:types` を再実行して取り込み、コミットに含めた）。次フェーズ開始時は着手前に必ず
  `pnpm gen:types && git diff --exit-code app/taskd/types.ts` を実行し、この時点からの追加分が無いか確認すること。
- **G6-U2: `/help` の `#trouble` はテスト用の DB や taskd の crate に触れない一般的な内容に留めている** — 「run のログと成果物の見方」「409/422 の意味」等は
  DESIGN の記述どおりだが、実際に taskd が返しうる `code`（`db_busy`、`too_many_streams` 等）はカバーしていない。困ったときの一次情報は
  `docs/taskd-api-v1.md` §1.5 のエラー表であり、`/help` はそこへの入口として最小限にとどめた（意図的な絞り込み、ADR-0009 D4 の延長）。
- G0〜G5 からの引き継ぎ（`/assets` の Host 検査、`pnpm dev` の CSP、G2-U1 taskd 間欠停止、G2-U2〜U7、G3-U1〜U8、G4-U1〜U4、G5-U1〜U9）は G6 では対処していない。

### 提案
- 上の「提案」節の G0-P1/P2、G1-P1/P2、G2-P1〜P4、G3-P1/P2 に加え、G6-P1: `docs/DESIGN.md` §10 に taskd の ADR-0017（プロバイダ管理、Phase 11）に対応する
  GUI フェーズの記載が無い（G7 はクラスタと委譲＝Phase 10 と 12 だけを扱う）。`GET/POST/PATCH/DELETE /providers`・`POST /providers/{id}/check`・
  `POST /reload` を GUI から操作可能にするかどうか、するなら G7 に含めるか新しい G8 にするかを人間に決めてほしい。

### taskd への依頼
- なし。`docs/taskd-api-v1.md` §3.4 / §5.4 に書かれた語彙・規則は `/help` の記述と実際の `app/taskd/types.ts` のどちらとも一致した。

## Phase G7 — DONE（2026-09-16）

### 成果物
- クラスタ画面: `app/routes/clusters.tsx`（新規、`/clusters`。`GET /clusters` を 1 回呼ぶだけ、`cooldown_remaining_secs` 等は taskd 側で計算済みなので
  再計算しない。`id/host/connected/cooldown_until/in_use/concurrency/sync/delete_on_push` を表で出し、`connected === false` の行にだけ
  「手元で `scripts/cluster-login.sh <host>` を実行してください」を出す）。`app/routes.ts`（`providers` の後・`graph` の前に登録）、
  `app/root.tsx`（ナビゲーションに「クラスタ」を追加）、`app/routes/help.tsx`（`#screens` に `/clusters` の説明を追加）。
- 受信箱: `app/routes/inbox.tsx` の `cluster_unavailable`（G6 で最小限だった暫定対応）を、クラスタ名を `/clusters` へのリンクにして
  `data-testid="attention-cluster-link"` を押すと遷移するようにした。
- タスク詳細: `app/routes/tasks.$id.tsx` に `role`（`task-role`）、`cluster`（Remote のときだけ、`/clusters` へのリンク + workspace_dir が
  写しであることの注記 `task-workspace-note`）、`delegated[]` を表示する新セクション「委譲」（`delegated-section`。run ごとにグループ化し、
  子タスクへのリンク `delegated-child-link`）を追加。既存の `children`（`TaskRefList`）は変更なし。
- 作成フォーム: `app/routes/tasks.new.tsx` に `role`（`<input list="role-options">` + `GET /config` の `roles[]` から作る `<datalist>`。
  自由入力可、`[[roles]]` に無い名前でも送る）と `aggregate`（チェックボックス、未チェックならフォームから省く）を追加、`buildNewTaskSpec` を拡張。
  `app/taskd/route-actions.server.ts` の `createTask` は `NewTaskSpec` に対して既に汎用なので変更なし。
- fixture: `scripts/taskd.sh` に `fixture clusters`（`~/.ssh/config` の `taskd-localhost` への実際の ssh 多重接続を使い、`local`（接続あり）/
  `offline`（`taskd-no-such-host-for-tests`、接続なし）の 2 クラスタを作る。`local` 向けタスクは push → run → 判定 → pull を実際に localhost 相手に行う）
  と `fixture delegation`（`role=lead, aggregate=true` の親が `delegate` で 2 件の `role=implementer` の子を作り、子が終端になった後の集約 run が
  `artifacts/summary.md` を書く）を追加。`test/taskd/clusters.toml.tmpl`、`test/taskd/delegation.toml.tmpl`、
  `test/taskd/fixtures/{clusters,delegation}-worker.sh`（新規）。
- テスト: `test/unit/clusters.test.ts`（2 件）、`test/unit/tasks.new.test.ts` に `role`/`aggregate` のケース 4 件追加、`e2e/g7.spec.ts`
  （受け入れ条件 1〜6 の 6 シナリオ）。`e2e/g0.spec.ts`・`e2e/g6.spec.ts` の「事前に止めるインスタンス」一覧に `clusters`/`delegation` を追加
  （G7-U2、監査指摘、後述）。
- 文書: `docs/adr/0010-g7-decisions.md`（D1〜D8）、`docs/taskd-requests.md` R2（`TaskSummary`/`GraphNode` に `role` が無い依頼）、
  README.md に `fixture clusters` の ssh 前提を追記。
- 実装単位: 3 つの独立した単位（互いにファイルを共有しない）を implementer サブエージェントに並列で担当させた: (A) `/clusters` 新規ルート +
  ナビゲーション + 受信箱の `cluster_unavailable`（`app/routes/clusters.tsx` 新規, `app/routes.ts`, `app/root.tsx`, `app/routes/inbox.tsx`）、
  (B) タスク詳細への `role`/`cluster`/`delegated[]` 追加（`app/routes/tasks.$id.tsx` のみ）、(C) 作成フォームへの `role`/`aggregate` 追加
  （`app/routes/tasks.new.tsx` のみ）。fixture 構築（`scripts/taskd.sh` とワーカースクリプト）、ADR、e2e、監査後の横断的な仕上げ
  （HelpLink 追加、`/help` の `#screens` 更新、README、`e2e/g0.spec.ts`/`e2e/g6.spec.ts` の停止リスト）は設計判断・複数ファイル横断のため自分で行った。

### 受け入れ条件と証拠（docs/DESIGN.md §10 Phase G7。`e2e/g7.spec.ts`、実 taskd `clusters`/`delegation` に対して検証）
1. **`/clusters` が 200 で、fixture の 2 クラスタ（接続あり/無し）が出る。`connected: false` の行にだけログインの案内が出る** —
   `e2e/g7.spec.ts:79`「受け入れ条件 1」pass。`cluster-row` 2 件、`local`（host `taskd-localhost`）が `cluster-connected`=`connected`・
   `cluster-login-hint` 0 件、`offline`（host `taskd-no-such-host-for-tests`）が `cluster-connected`=`disconnected`・`cluster-login-hint` に
   `scripts/cluster-login.sh taskd-no-such-host-for-tests` を含む。curl でも `GET /api/v1/clusters` の応答と一致を確認済み。
2. **受信箱に `cluster_unavailable` の項目が出て、押すと `/clusters` に遷移する** — `e2e/g7.spec.ts:102`「受け入れ条件 2」pass。
   `attention-item[data-attention-type=cluster_unavailable]` 1 件、`attention-cluster-link` クリックで `/clusters` に遷移。
3. **Remote のタスクの詳細に `cluster` と写しの注記が出て、run のログが開ける** — `e2e/g7.spec.ts:110`「受け入れ条件 3」pass。
   `Cluster-Local`（`task-status`=`done`）の `task-cluster` に `local`、`task-workspace-note` が表示、`run-log-link` から
   `/tasks/<id>/runs/<run_id>` に遷移して `used the cluster file`（実際に ssh 越しに push/pull・判定された run のログ）を確認。
4. **委譲のあるタスクの詳細に `role` と `delegated[]` が出て、子のリンクから子の詳細に飛べる** — `e2e/g7.spec.ts:141`「受け入れ条件 4」pass。
   `Lead-Delegator` の `task-role`=`lead`、`delegated-group` 1 件、`delegated-child-link` 2 件（`Delegated-Child-1`/`-2`）、クリックで
   子タスクの詳細（`task-title`=`Delegated-Child-1`）に遷移。
5. **作成フォームで `role` と `aggregate` を指定して作ると、`POST /tasks` の本文にそれが載る（空欄なら送らない）** —
   `e2e/g7.spec.ts:158`「受け入れ条件 5」pass。`role=reviewer-custom` + `aggregate` チェックで作成 → `GET /tasks/<id>` の `role`=`reviewer-custom`、
   `task.aggregate`=`true`。空欄で作成 → `role` 省略（`null`）、`task.aggregate` 省略（既定 `false`）。単体テスト
   （`test/unit/tasks.new.test.ts`）で `buildNewTaskSpec` のケース 4 件（値あり/空欄 × role/aggregate）も確認。
6. **`@axe-core/playwright` で `/clusters` の critical / serious が 0 件、CSP 違反 0 件** — `e2e/g7.spec.ts:122`「受け入れ条件 6」pass
   （gating な violation 0 件）。CSP 違反 0 件は `e2e/test.ts` の auto fixture が全 spec に効かせている。
7. **`pnpm lint` / `typecheck` / `test` / `build` / `e2e` が exit 0、`pnpm gen:types` の差分ゼロ** — 下記「共通条件」参照。

### 共通条件
- `pnpm lint` exit 0（`Checked 96 files`）/ `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0 / `pnpm test` **147 passed**（20 ファイル。
  G6 までの 141 + G7 の `clusters.test.ts` 2 件 + `tasks.new.test.ts` 追加 4 件）/ `pnpm build` exit 0
- `pnpm e2e` **62 passed（exit 0、約 7.3 分）**（G0 5 + G1 8 + G2 8 + G3 5 + G4 5 + G5 20 + G6 5 + G7 6）。監査者による別ランでも 62 passed。
  `e2e/g7.spec.ts` の `fixture clusters` は `~/.ssh/config` の `taskd-localhost`（localhost への ssh 多重接続）が張られている環境が前提
  （この環境には既にあった。無ければ `ssh -MNf taskd-localhost` を先に張る。README に追記済み）。
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts` 差分ゼロ（exit 0。着手前・実装後・監査後の 3 回とも確認）
- `scripts/taskd.sh build && scripts/taskd.sh start dev` → `curl /api/v1/health` の `api_version` が `"1"`（G0 の前提確認、着手時に実施）

### 監査結果
- auditor の判定: **条件付き可**（番号付き受け入れ条件 1〜7 は全て「可」。「不可」ゼロ）。禁止事項（SQLite・crate 依存・仕様外挙動・
  ブラウザ直接呼び出し・トークン露出・`dangerouslySetInnerHTML`/`eval`/CDN・テストの外部ネットワーク・依存の新規追加・版固定）は
  「違反は 1 件も見つからなかった」（auditor 自身が `lint`/`typecheck`/`test`（147 passed）/`build`/`pnpm e2e`（62 passed、フルスイート）/
  `gen:types`（差分ゼロ）を再実行し、加えて実 taskd に対する curl・GUI の HTML・`build/client/` のクライアントバンドルを直接検査して確認した）。
  「DAG では委譲で生まれた子を親の下に寄せる」（ADR-0010 D4）は既存の `layoutGraph` の `parent_id` グルーピングで満たされることを
  `GET /graph` の実データで裏取り済み。
- 判断が必要な点として auditor に問うた「一覧・DAG への role ラベル表示を N+1 無しに満たす抜け道が無いか」（ADR-0010 D5）への回答: **妥当**。
  `GET /tasks` のクエリにも `role` 列は無く、`GraphNode` にも無く、SSE の `EventRow` から組み立てる案は DESIGN §6.3
  「イベント本体から状態を組み立てない」に反するため、`GET /tasks/{id}` の N+1 呼び出し以外に道は無いと確認した。
- 指摘と対応（全て軽微、番号付き受け入れ条件には非該当。修正後の再監査は自分で実施 — 最大1回の枠内）:
  1. 「`/clusters` に他画面と同じ `HelpLink` が無い」→ **修正済み**（`app/routes/clusters.tsx` に `<HelpLink anchor="screens" .../>` 追加）。
  2. 「`/help` の `#screens` に `/clusters` の項目が無い」→ **修正済み**（`app/routes/help.tsx` に追加）。
  3. 「`inbox.tsx` の `cluster_unavailable` 分岐のコメントが G6 時点のまま陳腐化」→ **修正済み**（実装済みの内容に合わせて書き直した）。
  4. 「README に `fixture clusters` の ssh 前提が書かれておらず、他者・CI が再現できない」→ **修正済み**（README に追記）。
  5. 「`e2e/g0.spec.ts`/`e2e/g6.spec.ts` の事前停止リストに `clusters`/`delegation` が無く、中断時に次回の `start dev`/`start basic` が
     失敗しうる」→ **修正済み**（両ファイルのリストに追加）。
  6. 軽微指摘（`fixture delegation` の fake ワーカーの `grep` が RunRequest の JSON 直列化形式に依存して脆い、`graph-layout.ts` の
     単体テストに委譲の子のケースが無い）→ 下の「未解決事項」G7-U3/U4 に記載（実装変更は必須とされていない）。
  7. **手続き上の要判断**（監査指摘 A）: 「一覧と DAG のノードに役割を出す」（DESIGN §10 Phase G7 の実装節の 1 項目）が未実装のまま
     フェーズを DONE にしてよいか → 人間の判断のため下記に明記する（下記「未解決事項」G7-U1 と「taskd への依頼」R2 を参照）。
     判断: 番号付き受け入れ条件 1〜7 にはこの項目は含まれておらず（4 が求めるのは詳細画面の `role`/`delegated[]` のみ）、`TaskSummary` /
     `GraphNode` に `role` が無いという確認済みの API 制約により、GUI 側の workaround（N+1 の `GET /tasks/{id}`）を使わずに満たす方法が無い。
     CLAUDE.md の「回避しない」原則に従い `docs/taskd-requests.md` R2 に記録し、DESIGN 本文を書き換えずに DONE として進める
     （G6 のプロバイダ管理 UI の扱い、ADR-0009 D5/G6-P1 と同じ前例）。
- 修正後の自己再検証: `pnpm lint`/`typecheck` exit 0、`pnpm test` 147 passed、`pnpm build` exit 0、`pnpm e2e` **62 passed (7.3分)**、
  `pnpm gen:types` 差分ゼロ。auditor の再起動は行っていない（「不可」が無く、指摘は全て自分で再検証できる範囲）。

### 未解決事項
- **G7-U1: `TaskSummary`（`GET /tasks` の一覧行）と `GraphNode`（`GET /graph` のノード）に `role` が無い** — DESIGN §10 Phase G7 の実装節
  「一覧と DAG のノードに役割を出す（色分けはせず、テキストのラベル）」は未実装（`docs/taskd-requests.md` R2、`docs/adr/0010-g7-decisions.md` D5）。
  番号付き受け入れ条件（1〜7）には含まれないため DONE の判定には影響しないが、taskd 側で `role` フィールドが追加され次第、次フェーズ以降で
  一覧・DAG のノードラベルを実装したい。
  **2026-09-16 追記: taskd が R2 に対応した**（`TaskSummary.role` / `GraphNode.role`。`docs/taskd-requests.md` の「対応済み R2」、
  `app/taskd/types.ts` も生成済み）。次のフェーズで一覧・DAG のラベルを実装できる。
- **G7-U2**: `e2e/g0.spec.ts`/`e2e/g6.spec.ts` の事前停止リストに `clusters`/`delegation` を追加済み（監査指摘、上記「監査結果」参照）。
- **G7-U3**: ~~`test/taskd/fixtures/delegation-worker.sh` の `grep -q '"children":\[{'` は compact 出力に依存している~~
  → **2026-09-16 解消**（下の「G7 の後の追補」参照）。
- **G7-U4**: ~~`app/lib/graph-layout.ts` の単体テストに、委譲で生まれた子のグルーピングを検証するケースが無い~~
  → **2026-09-16 解消**（`test/unit/graph-layout.test.ts` を新設。下の「G7 の後の追補」参照）。
- G0〜G6 からの引き継ぎ（`/assets` の Host 検査、`pnpm dev` の CSP、G2-U1 taskd 間欠停止、G2-U2〜U7、G3-U1〜U8、G4-U1〜U4、G5-U1〜U9、
  G6-U2）は G7 では対処していない。

### 提案（`docs/DESIGN.md` / `docs/taskd-api-v1.md` への変更提案。採否は人間）
- 上の「提案」節の G0-P1/P2、G1-P1/P2、G2-P1〜P4、G3-P1/P2、G6-P1 に加えて新規提案は無し（G7 は DESIGN の記述どおりに実装できた）。

### taskd への依頼
- R2（新規）: `TaskSummary`（`GET /tasks`）と `GraphNode`（`GET /graph`）に `role: Option<String>`（`TaskDetail.role` と同じ規則）を
  追加してほしい。詳細は `docs/taskd-requests.md` R2（エンドポイント / 期待 / 実際 / できないこと）。GUI 側は現時点でこれを回避していない
  （一覧・DAG への役割ラベル表示は保留、G7-U1）。

## G7 の後の追補（2026-09-16）

人間の指示「役割ラベルは欲しいです。またワーカーが壊れやすいのも直して下さい」。フェーズではなく、G7 の未解決事項の片付け。

### 1. 一覧と DAG の役割ラベル（G7-U1、`docs/taskd-requests.md` R2 の対応後）

taskd が `TaskSummary.role` と `GraphNode.role` を足したので、**追加の `GET /tasks/{id}` 無しで**出せるようになった。

- `app/routes/tasks.tsx`: 行に `data-testid="task-role"` の列（テキストのみ。色分けはしない。役割が無ければ空欄）。
- `app/lib/graph-layout.ts`: ノードのラベルを 2 行にし、2 行目に `[<役割>]`（`whiteSpace: pre-line`）。役割が無ければ従来どおり 1 行。
- 証拠: `test/unit/graph-layout.test.ts`（新規 4 件。役割ラベル / `role` の無い応答 / **委譲の子の group 化（G7-U4）** / 端が無い辺の除去）、
  `e2e/g7.spec.ts` に「一覧の行と DAG のノードに役割のラベルが出る」を追加（`fixture delegation` の実 taskd で lead / implementer を確認）。

### 2. fixture のワーカーが壊れやすい問題（G7-U3）

`RunRequest` の JSON を `grep`/`cut` で読んでいたため、taskd 側の直列化の細部に依存していた
（`basic-worker.sh` の `"kind"` は条件や成果物の `kind` を拾う可能性もあった）。

- `test/taskd/fixtures/read-run-request.mjs`（新規）で **一度だけきちんと JSON を解析**し、
  `TITLE` / `TASK_ID` / `KIND` / `ROLE` / `INSTRUCTIONS` / `CHILDREN` / `CHILD_TITLES` を `sh` の変数として渡す
  （値はシングルクォートで安全に囲む。引用符を含む指示文でも壊れない）。
- `delegation-worker.sh` と `basic-worker.sh` はこれを `eval` するだけにした。`scripts/taskd.sh` は
  ワーカーを `.run/<name>/` に写すときに読み取り役も一緒に置く。
- 証拠: `scripts/taskd.sh fixture delegation` / `fixture basic` を作り直して同じ結果
  （summary.md は子のタイトルまで書けるようになった）。`pnpm e2e` **63 passed**（フルスイート）。

### 共通条件

`pnpm lint` exit 0（98 files）/ `pnpm typecheck` exit 0 / `pnpm test` **151 passed**（21 ファイル）/ `pnpm build` exit 0 /
`pnpm e2e` **63 passed（6.1 分）** / `pnpm gen:types` 差分ゼロ。

## 追補 2: 疎通確認の表示（2026-09-16）

taskd 側 ADR-0022 の決定（人間の回答）に合わせた小さな追加。

- **G6-P1 は「作らない」で決着**: アカウント管理の画面（追加・編集・削除・疎通確認・ログイン手順）は作らない。
  一人で使い、信頼されたネットワークで localhost に閉じる運用のため、管理操作は `curl`（管理 API は loopback でも
  トークンが要る）と `providers.d/` の直接編集で行う。`/providers` は読み取り専用のまま。
- **`/providers` に「最後の疎通確認」を追加**（`GET /providers` の `last_check`。taskd 側 ADR-0022 D2）。
  `result（at）` を出し、まだ確認していなければ「未確認」。自動では走らないので、値が入るのは人が
  `POST /api/v1/providers/{id}/check` を叩いた後だけ。taskd を再起動すると「未確認」に戻る（メモリ上の観測値）。
- 証拠: `e2e/g4.spec.ts` に「叩いていないアカウントは未確認」を追加。`pnpm lint` / `typecheck` / `build` exit 0、
  `pnpm test` 151 passed、`pnpm e2e` **63 passed**、`pnpm gen:types` 差分ゼロ。

## 追補 3: 画面デザインの刷新（2026-09-16）

人間の依頼「taskd-gui の画面デザインが簡素すぎるので現代のウェブサイトのデザインくらいリッチにして下さい」。フェーズではなく見た目だけの変更
（ルート・loader / action・API 呼び出し・data-testid・表示文字列・見出しレベルは変えない）。設計判断は `docs/adr/0011-visual-design-system.md`。

### 成果物

- `app/app.css`: セマンティックなデザイントークン（CSS 変数 → Tailwind 4 の `@theme inline`）、`prefers-color-scheme` によるダークモード、
  日本語向けシステムフォントのフォールバック、`.markdown` の本文スタイル、背景の淡いグラデーション。
- `app/components/ui/`（新規）: `Icon`（直書き SVG、aria-hidden）/ `button`（`Button`・`buttonClass`）/ `card` / `badge`（`StatusBadge`・`KindBadge`・`RoleLabel`）/
  `misc`（`PageHeader`・`SectionTitle`・`EmptyState`・`Alert`・`StatCard`・`DataList`）/ `form`（入力・表のクラス）/ `tone`。**新しい依存は無し**。
- `app/root.tsx`: 左サイドバー（グループ分けしたナビ・現在地の強調・承認待ちバッジ・taskd の接続状態・ログアウト）、狭い画面では上部の横スクロールバー。
  同じリンク・data-testid を 2 つ描かない（ADR-0011 D3）。エラー画面をカード化。
- 全画面（受信箱・一覧・詳細・run・作成・Plan・デーモン・プロバイダ・クラスタ・DAG・使い方・ログイン）をカード・バッジ・アイコン・空状態で作り直した。
  DAG は React Flow の `colorMode="system"`・Background・Controls、ノード色をトークンに揃え、キャンバスを `min-h-[36rem]` にした。CodeMirror はトークンのテーマ。
- `e2e/g3.spec.ts-snapshots/graph-basic-chromium-linux.png`: 見た目の変更に合わせてベースラインを更新（`--update-snapshots` はこのテストだけ。更新後の画像を目視確認）。

### 途中で直したこと（e2e が見つけたもの）

- `/help` の a11y（g6）: (1) 表示時のフェードインで不透明度を変えていたため、axe が途中の薄い文字色を測って color-contrast 4.2 になった →
  アニメーションは位置だけにした。(2) `<dl>` 直下の `<div>` にアイコンの `<span>` があり definition-list 違反 → アイコンを `<dt>` の中へ。
  (3) 本文中のリンクが色だけで区別されていた（link-in-text-block）→ 下線を付けた。
- DAG のキャンバスが 1280×720 で 324px と低くなった → 高さの計算と最小高さを見直した。

### 証拠

- `pnpm lint` exit 0（105 files）/ `pnpm typecheck` exit 0 / `pnpm test` **152 passed**（21 ファイル）/ `pnpm build` exit 0 / `pnpm gen:types` 後 `git diff --exit-code app/taskd/types.ts` 差分ゼロ。
- `pnpm e2e`（この環境のメモリ監視でフルスイートのバックグラウンド実行が止められるため spec ごとに前景で実行。最終コードで）:
  g0 5 / g1 8 / g2 8 / g3 5 / g4 5 / g5-a11y 12 / g5 7 / g5-release 1 / g6 5 / g7 3 = **59 passed、0 failed**。
  **g7 のクラスタ 4 件は未実行**: `fixture clusters` が `~/.ssh/config` の `taskd-localhost`（localhost への ssh 多重接続、ADR-0010 D1）を前提にしており、
  このホスト（home-dev）には無いため `taskd.sh: no ssh control master for 'taskd-localhost'` で fixture を作れない（1 failed + 3 did not run）。
  代わりに実運用の taskd（pegasus / sirius、未接続）に対して `/clusters` をログインして開き、2 枚のカード・`disconnected`・ログイン案内の表示をライト / ダークで目視確認した。
- ライト / ダーク両方の全画面スクリーンショットを目視確認（1440×900）。

### 未解決事項

- A3-U1: g7 のクラスタ 4 件をこのホストで回すには、人が `taskd-localhost` の ssh 設定（`HostName 127.0.0.1`・ControlMaster）と `ssh -MNf taskd-localhost` を用意する必要がある。
- A3-U2: ダークモードは OS の設定に従うだけで、画面上の切り替えは無い（ADR-0011 D1）。

## Phase G8 — DONE（2026-09-16）

人間の依頼「GUI からプロバイダを登録できるようにして下さい」ほか（taskd 側 ADR-0024 / Phase 13）。設計は `docs/adr/0012-provider-and-account-management.md`。
G6-P1（アカウント管理画面）は taskd 側 ADR-0022 D1 で「作らない」としていたが、人間の依頼で作った。

### 成果物

- `app/taskd/types.ts` 再生成（`account_pool`、`AccountList` ほか）。`TaskdClient.patch` / `delete`。
- `/providers`: 追加・編集・削除（`<details>` の確認付き）・疎通確認。変更の後は同じ action で `POST /reload` まで行い、両方の結果を flash に出す。401 はトークンの設定方法を案内。
- `/accounts`（新規、ナビ「運用」）: 5 時間枠・週次枠の使用率バー、スコアと除外理由、実行中、cooldown、最後の確認、集計。追加・ログイン（URL → コード）・確認・中止・削除。
  認可コードが平文 HTTP を通る旨の注意を表示。
- `app/taskd/providers-admin.server.ts` / `accounts-admin.server.ts`（action の中継）、`Flash.tsx` に管理系の結果表示、`/help` にアカウントとプロバイダ管理の説明。
- `scripts/taskd.sh fixture accounts`（トークン・`providers_include`・`[accounts]`・スタブの `claude`）、`test/taskd/accounts.toml.tmpl`、`test/taskd/fixtures/claude-stub.sh`。

### 実装で決めた細部

- **フォームは `useFetcher()`**: root の SSE が tick ごとに `revalidate()` するため、`<Form>` の `actionData` は数百 ms で消える
  （ログイン URL のように二度と出せない表示が消え、e2e も不安定になった）。`/providers` と `/accounts` のフォームは `fetcher.Form` と `fetcher.data` を使う。

### 受け入れ条件と証拠

- `pnpm lint` exit 0（112 files、warning 1 = 既存規則の optional chain の提案）/ `pnpm typecheck` exit 0 / `pnpm test` **187 passed**（24 ファイル）/ `pnpm build` exit 0 /
  `pnpm gen:types` を 2 回実行して `app/taskd/types.ts` が同一（taskd のスキーマ追加分の差分のみ）。
- `e2e/g8.spec.ts`（`TASKD_GUI_BIND=127.0.0.1:7800 TASKD_API_URL=http://127.0.0.1:7810` + `fixture accounts`）**1 passed**:
  GUI からプロバイダ `pool`（claude-code、`account_pool`）追加 → reload 成功 → カードに account_pool → アカウント `a` 追加（未ログイン）→ ログイン開始で URL →
  誤ったコードで failed → やり直して正しいコード → ログイン済み → 確認で 42% / 18% のバー → アカウント削除 → プロバイダ削除。
- 既存の e2e（最終コードで、運用中の taskd / GUI を止めて実行）: g0 5 / g1 8 / g2 8 / g3 5 / g4 5 / g5-a11y 12 / g5 7 / g5-release 1 / g6 5 / g7 3 passed。
  g7 のクラスタ 4 件は従来どおり `taskd-localhost` の ssh 多重接続がこのホストに無く未実行（追補 3 の A3-U1）。
- ライト / ダークのスクリーンショット（`/accounts`、`/providers`、ログイン中の表示、`/help`）を目視確認。
- 実機: 運用中の GUI（LAN 公開、パスワード認証、`TASKD_API_TOKEN_FILE` 付き）を新しいビルドで起動し直した。

### 未解決事項

- G8-U1: 実アカウントでのログインは人が行う（認可はアカウントの持ち主の操作が要る）。

## Phase G9 — DONE（2026-09-17）

人間の依頼「codex のアカウント追加方法も実装して下さい」（taskd 側 ADR-0025 / Phase 14）。

### 成果物

- `pnpm gen:types` 再生成（`adapter` / `roots` / `kind` / `user_code` / `RunSummary.account`）。
- `/accounts`: アダプタ（claude-code / codex）ごとに節を分け、それぞれの根ディレクトリを表示。追加フォームにアダプタ選択（設定済みのアダプタだけ）。
  各操作は `adapter` を送る。ログインは `kind` で分岐し、`device_code` では **URL と大きな等幅の `user_code` を出し、コード入力欄は出さない**
  （人は開いたページでコードを入力する。完了すると画面が自分で「ログイン済み」に変わる）。`login_code_not_supported`（409）の案内も追加。
- `/providers`: `account_pool` の説明を claude-code / codex 両対応に。`/help`: `codex_dir` とデバイス認証の説明を追加。
- タスク詳細の run 一覧に `account` 列（`data-testid="run-account"`）。プールでない run は `-`。
- fixture: `accounts` に `codex_dir` と `test/taskd/fixtures/codex-stub.sh`（`login --device-auth` と `exec --json` の両方を模す）。

### 受け入れ条件と証拠

- `pnpm lint` exit 0（113 files）/ `pnpm typecheck` exit 0 / `pnpm test` **192 passed**（24 ファイル）/ `pnpm build` exit 0 / `pnpm gen:types` 安定。
- `e2e/g8.spec.ts`（claude-code）と `e2e/g9.spec.ts`（codex）を 7800/7810 で実行し **2 passed**:
  アダプタ codex でアカウント追加 → カードに codex と未ログイン → ログインで `device_code` と `ABCD-EFGHI`（入力欄は無い）→
  スタブの完了で自動的に「ログイン済み」→ 確認で 30% / 12% のバー → 削除。
- `/accounts` のライト / ダークのスクリーンショットを目視確認（claude-code と codex のカード、デバイス認証の表示）。

### 実装で決めた細部

- 画面を再読み込みするとログイン開始の応答（`kind`）が失われるため、`login_pending` の表示ではアカウントの `adapter` から流儀を決める
  （ADR-0025 D5 の表のとおり codex = device_code、claude-code = paste_code の固定対応）。
- `device_code` は完了を知らせる操作がこちらに無いので、`logged_in` になったらログインの表示を閉じる。

### 未解決事項

- G9-U1: 実際の codex アカウントでのログインは人が行う（未実施）。

## 追補: ACP アダプタ（taskd Phase 15 / ADR-0026）への追従（2026-09-17）

- `/providers` の adapter 選択肢に `acp` を追加（`ADAPTER_OPTIONS`、unit テスト 1 件追加）。`command` / `args` は管理 API から書けないので GUI にも出さない。
- `docs/taskd-api-v1.md` は `scripts/sync-gui-docs.sh` で同期済み（§3.24 / §3.25 の adapter 一覧と 422 の条件）。
- 証拠: `pnpm lint` / `pnpm typecheck` / `pnpm build` exit 0、`pnpm test` **193 passed**、`pnpm gen:types` 差分ゼロ（スキーマ変更なし）。
  e2e は運用中の taskd / GUI（7700 / 7710）と衝突するため今回は実行していない（GUI の変更は選択肢 1 つの追加のみ）。

## Phase G10 — DONE（2026-09-17）

taskd 側の Phase 16〜18（ADR-0027 の分野、ADR-0028 の能力レジストリ）への追従。GUI 側の新しい設計判断は無し（表示と中継だけ）。

### 成果物

- `pnpm gen:types` 再生成（`Task.genre` / `TaskSummary.genre` / `NewTaskSpec.genre` / `ConfigView.genres` /
  `GenreConfigView{capabilities, input_artifacts, output_artifacts}`）。
- `/tasks/new`: 分野の選択（`GET /config` の `genres` から。未設定の taskd では自由入力に落とす）。選ぶと説明・所属する役割に加えて、
  **できること / 渡すもの → 返るもの**（manifest）を出す（`genre-hint` の中に `genre-capabilities` / `genre-input-artifacts` / `genre-output-artifacts`。空の項目は出さない）。
  役割の入力は自由記述のまま（検証は taskd 側。ADR-0005 D5）。taskd の 422（未知の分野・分野に属さない役割）はそのまま表示する。
- `/tasks`: 分野の絞り込み（チップ。説明と capabilities を `title` に出す）と一覧の「分野」列（`GenreLabel`。役割と同じ中立な見た目）。
- `/tasks/:id`: ヘッダに分野のラベル。
- `/providers`: adapter の選択肢に `paperqa` を追加（`acp` は G9 で追加済み）。
- `/help`: 分野の説明（ハーネスへの入口であること、manifest を Planner と委譲する親が見ること）。

### 受け入れ条件と証拠

- `pnpm lint` / `pnpm typecheck` / `pnpm build` exit 0、`pnpm test` **201 passed**（24 ファイル）、`pnpm gen:types` は 2 回実行して同一。
- スクリーンショット: `/tasks/new` で分野を選んだ状態（manifest の 3 行が出る）をライト / ダークで目視確認。分野が未設定の taskd での自由入力への落とし方も確認。
- e2e は運用中の taskd / GUI（7700 / 7710）と衝突するため未実行（変更は表示と中継のみ）。

### 未解決事項

- G10-U1: このリポジトリには DOM を描画する unit テストが無いため、「空の項目を出さない」ことはコードの条件分岐と目視でのみ確認している。

## Phase G11 — DONE（2026-09-17）

taskd 側の Phase 20（ADR-0030: API キーを GUI から預かる）への追従。置き場所は「アカウント」画面の一区画という人間の指定に従う。
GUI 側の新しい設計判断は無し（ADR-0030 D4 をそのまま実装）。

### 成果物

- `pnpm gen:types` 再生成（`SecretList` / `SecretView` / `SecretUse` / `SecretPutResult`）。2 回実行して同一。
- `app/taskd/client.server.ts` に `put<T>()`（既存の `post` / `patch` と同じ形）。
- `app/taskd/secrets-admin.server.ts`（新規）: `listSecrets` / `putSecret` / `deleteSecret`（put・delete は成功後に続けて
  `POST /reload` を呼ぶ。ADR-GUI-0012 D2 と同じ作り）/ `readSecretId` / `readSecretValue`。
- `app/taskd/action-types.ts` に `SecretOpOutcome` / `SecretActionResult`、`components/Flash.tsx` に `SecretActionFlash`
  （**値は一切表示しない**）。
- `/accounts` に「API キー」節: 一覧カード（設定済み / 未設定バッジ、使われている場所（env とアダプタ / プロバイダ名）、
  更新時刻の相対表示、fingerprint）、追加・更新フォーム（`type="password"`、`autocomplete="off"`、保存後は二度と表示されない旨）、
  確認付き削除、平文 HTTP の注意 Alert、`[secrets]` 未設定時の EmptyState。
  `GET /secrets` は管理系で 401 / 409 になりうるので loader 内で try/catch し、`secretsError` として**この節の中だけ**に出す
  （ページ全体は壊さない）。
- `/help`: 用語集に「API キー」、「アカウント」画面の説明に API キー節への言及。
- testid: `secrets-section` / `secret-card`（`data-secret-id`）/ `secret-used-by` / `secret-updated-at` / `secret-fingerprint` /
  `secret-unset` / `secret-add-form` / `secret-add-id` / `secret-add-value` / `secret-add-submit` / `secret-update-form` /
  `secret-update-value` / `secret-update-submit` / `secret-delete`。

### 受け入れ条件と証拠

- `pnpm lint`（biome、115 files、no fixes）/ `pnpm typecheck` / `pnpm build` exit 0、`pnpm test` **213 passed**（25 ファイル）。
  新規 `test/unit/secrets-admin.test.ts`: put→reload の両方成功 / put 失敗時は reload を呼ばない / 422・404・409・401 の伝播 /
  reload 失敗の個別報告 / delete→reload / 未設定エントリの passthrough。
- 実機: 使い捨ての taskd（`[api] token_file` + `[secrets] dir` + `[adapters.fake] env_from_secrets`）を 127.0.0.1:7810 に立て、
  GUI dev を 7800 で起動して Playwright で light / dark のスクリーンショットを取得・目視確認。
  画面から追加（flash「保存: newkey」＋「reload: 反映しました」）→ カード出現 → 確認付き削除 → カード消滅までを実地確認した。
  確認後に使い捨ての taskd / dev サーバは停止し、作業ディレクトリは削除済み。
- `docs/taskd-api-v1.md` は `scripts/sync-gui-docs.sh` で同期済み（§3.36〜3.38。未設定 id も `items[]` に載る旨に修正済み）。

### 実装中に直したもの

- React Router のビルド制約で `loader` / `action` 以外の export から `.server` モジュールを参照すると client バンドルが壊れる。
  `loadAccounts` では `secrets-admin.server.ts` を使わず `client.get` の直呼び + `~/taskd/errors`（非 `.server`）だけを使う形に変更した
  （`putSecret` / `deleteSecret` / `readSecretId` / `readSecretValue` は `action` の中でだけ使う）。

### 未解決事項

- G11-U1: e2e は運用中の taskd / GUI（7700 / 7710）と衝突するため未実行。確認は unit テストと使い捨て環境でのスクリーンショットで行った。
- G11-U2: 鍵の値は平文 HTTP を通る（LAN 前提。ADR-0030 D4 の注意書きを画面に出しているだけ）。
