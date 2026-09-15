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
| G5 | 認証・配布・仕上げ | 未着手 | — |

前提: taskd（`$TASKD_REPO`、既定 `../agent-platform`）の Phase 9a / 9b（`docs/adr/0013`）が完了していること。G0 の受け入れ条件 2 で確認する。

## 引き継ぎ（前のフェーズから）

G1 の未解決事項（下記）のうち、G2 着手前に効いてくるものを引き継ぐ:
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
  （API 単体の curl では再現しない）。詳細と証拠は `docs/taskd-requests.md` R1。

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
