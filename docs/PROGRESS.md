# taskd-gui 進捗

設計は `docs/DESIGN.md`（§10 にフェーズと受け入れ条件）、taskd の API は `docs/taskd-api-v1.md`。各フェーズの完了時にこのファイルへ `## Phase G<N> — DONE` の節を追加する。
`run-gphases.sh` はこのファイルの `## Phase G<N> — DONE` / `BLOCKED` / `PARTIAL` を見て進む。

## 現在地

| フェーズ | 内容 | 状態 | 完了日 |
|---|---|---|---|
| G0 | 骨組みと前提の確定 | **DONE** | 2026-09-15 |
| G1 | 読み取りとストリーム | **DONE** | 2026-09-15 |
| G2 | 操作 | 未着手 | — |
| G3 | ログ・成果物・DAG | 未着手 | — |
| G4 | プロバイダとデーモン | 未着手 | — |
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
- G1-P2: `docs/DESIGN.md` §6.5「500 にしない」は React Router の本番ビルドが素の `Error` を ErrorBoundary に渡す前に汎用 500 へサニタイズすることと衝突しやすい（ADR-0004 D6）。「loader は taskd のエラーを `Response` として投げること」と実装上の注意を明記すると、次に同じ罠を踏まずに済む。

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
