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
| G11 | API キー（秘密）の管理画面（taskd Phase 20 / ADR-0030） | **DONE** | 2026-09-17 |
| G12 | クラスタへの接続を GUI から張る（taskd Phase 22 / ADR-0032） | **DONE** | 2026-09-17 |
| G13a | 組織の木と案件（仕事の木）。SPEC §4 の 6 画面のうち 2 つ（taskd Phase 23 / ADR-0033 D1・D2） | **DONE** | 2026-09-17 |
| G13b〜G13e | 秘書との対話・報告・認可・成果物と、taskd Phase 27 への追従 | **DONE** | 2026-09-17 |
| G13f | GUI 監査の対応（H1〜H4 / M1〜M5 / 言葉 / 裏方の印 / 記憶。taskd Phase 29） | **DONE** | 2026-09-17 |
| G13g | 既存 e2e（g0〜g9）の `/` が秘書になったことへの追従。G0 の「taskd 停止中でも 200」の引き継ぎ先を `/org/secretary` に | **DONE** | 2026-09-18 |

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

## Phase G12 — DONE（2026-09-17）

taskd 側の Phase 22（ADR-0032: クラスタへの接続を GUI から張る）への追従。GUI 側の新しい設計判断は無し
（ADR-0032 D6 をそのまま実装）。

### 成果物

- `pnpm gen:types` 再生成（`ClusterConnectStart` / `ClusterConnectResult`、`ClusterView.auth` /
  `.connect_pending`、`ClusterConfigView.auth`、`ClusterLive.auth` / `.connect_pending`）。2 回実行して同一。
- `app/taskd/clusters-admin.server.ts`（新規）: `startClusterConnect` / `submitClusterConnectCode` /
  `cancelClusterConnect`。**`POST /reload` は呼ばない**（設定が変わらないため。プロバイダ・秘密とはここが違う）。
- `app/routes/clusters.tsx`: action を新設（`cluster_connect` / `cluster_connect_code` / `cluster_connect_cancel`）。
  `auth` で 3 通りに出し分ける:
  - `manual`: 従来の `scripts/cluster-login.sh` の案内のまま。
  - `publickey`: 「接続」ボタンだけ。
  - `totp`: 「接続」→ taskd が返したプロンプト文字列 → コード入力欄（`type="password"` / `autocomplete="off"` /
    `inputmode="numeric"`）→「送信」。取り消しも置く。
  - `connected` のときは「切断」。平文 HTTP の注意書きを出す。**コードは `fetcher.data` にも画面にも残さない**。
- `app/routes/help.tsx`: 用語集と「クラスタ」画面の説明に接続方式（`auth` の 3 種）を追記。
- testid: `cluster-auth` / `cluster-connect` / `cluster-connect-prompt` / `cluster-connect-code` /
  `cluster-connect-submit` / `cluster-connect-cancel` / `cluster-disconnect` / `cluster-connect-pending`。

### 受け入れ条件と証拠

- `pnpm lint`（biome、116 files）/ `pnpm typecheck` / `pnpm build` exit 0、`pnpm test` **235 passed**（25 ファイル）。
  `test/unit/clusters.test.ts` は 24 件（接続 3 本の素通しと 401/404/409/422/502、`ok: false` を握りつぶさない、
  **`reload` を呼ばない**こと、loader の `auth` / `connect_pending`、下記の回帰 6 件）。
- 使い捨ての taskd（17710）と dev サーバ（17700）＋偽 ssh で、light / dark のスクリーンショットを取り
  `auth` 3 種の見た目を目視確認。**運用中の 7710 / 7700 には触っていない**。確認後に片付け済み。
- e2e は運用中のサービスと衝突するため未実行。

### 実機で見つかった不具合と、その回帰テスト

**「接続処理が進行中です」から抜け出せなくなる**（人間が実機で発見。「GUI で入力するフォームがありません」）。

- 症状: `connect_pending === true` のとき、コード入力欄も接続ボタンも出ず、取り消すことしかできない。
- 原因: `showConnectButton` の条件に `!showPendingElsewhere` が入っていた。プロンプト文字列は
  `POST` の応答にしか載らない（ADR-0032 D5）ので、**画面を開き直しただけでこの状態になる**。
- **`/accounts` のログインで同じ問題を一度直してある**（`70434cf`「ログインをやり直す」）のに、
  クラスタ側で再発させた。同じ扱いに揃え、進行中でも接続ボタンを出してラベルを「接続し直す」にした
  （taskd は `connect` を受けると古いセッションを畳んでから張り直すので、押し直せば入力欄に戻れる）。
- 再発防止: このリポジトリには DOM を描画する unit テストが無い（G10-U1）ため、判定を純粋関数
  `clusterConnectPanelState` に**切り出して**テストできるようにし、6 件の回帰テストを足した。

### 未解決事項

- G12-U1: e2e 未実行（上記）。
- G12-U2: コードは平文 HTTP を通る（LAN 前提。ADR-0032 D6 の注意書きを画面に出しているだけ）。
- G12-U3: DOM を描画する unit テストが無い件（G10-U1）は未解決のまま。今回は判定を純粋関数に切り出して
  回避したが、描画そのものの回帰は目視に頼っている。

## Phase G13a — 組織の木と案件（2026-09-17）

taskd 側の Phase 23（ADR-0033 D1/D2: `org_nodes` / `projects` / `milestones`）への追従。SPEC §4 の 6 画面
（秘書との対話／組織の木／仕事の木（DAG）／報告の流れ／認可の要求／成果物）のうち、今の API で作れる
「組織の木」と「案件（仕事の木を含む）」の 2 つを実装（ADR-0033 D8）。残り 4 つはナビにプレースホルダを置いた
（G13b で taskd 側の対話・報告・認可 API ができてから）。

### 成果物

- `pnpm gen:types` 再生成（`OrgNode` / `OrgList` / `OrgCreateBody` / `OrgPatchBody` / `Project` / `ProjectList` /
  `ProjectDetail` / `ProjectTaskView` / `ProjectCreateBody` / `ProjectPatchBody` / `Milestone` /
  `MilestoneCreateBody` / `MilestonePatchBody` / `Task.assignee` / `.project_id` / `.milestone_id`）。2 回実行して同一。
- **ナビの組み替え**（`app/root.tsx`）: 先頭の「業務」区画に SPEC §4 の順で「秘書」（`/org/secretary`、プレースホルダ）
  「組織」（`/org`）「案件」（`/projects`）「報告」（`/reports`、プレースホルダ）「認可」（`/approvals`、プレースホルダ）
  「成果物」（`/artifacts`、プレースホルダ）を並べ、既存の受信箱・一覧・DAG・新規タスク・新規 Plan・デーモン・
  プロバイダ・アカウント・クラスタを 1 つの「裏方」区画にまとめて末尾に下げた（ADR-0033 D8「人が見る単位は
  案件と組織になり、タスクは裏方に下がる」）。プレースホルダは共通コンポーネント `app/components/Placeholder.tsx`
  （「G13b で作ります」＋ SPEC の該当節の一文だけ）。
- **`/org`**（`app/routes/org.tsx`）: `GET /org` は position 順の平らな配列なので、`app/lib/org-tree.ts` の
  `buildOrgTree`（純粋関数）で `parent_id` から木に組む（根の判定は `kind === "secretary"` または
  `parent_id` 無し。存在しない `parent_id` を指す孤児は根の下に出す）。秘書を根に縦の組織図として描画
  （ネストした `<ul>`、部→課はインデントと左罫線）。各ノードに `kind`・`genre` バッジと「抱えている仕事の数」
  （後述）を出す。ノードを選ぶ（`?selected=<id>` の query）と右に詳細（brief・分野・抱えているタスク一覧・
  無効化した「話す（G13b）」ボタン）。編集は `app/taskd/org-admin.server.ts`（`POST/PATCH/DELETE /org...`。
  すべて管理系、`token_file` 未設定でも 401）: 追加（id/name/kind/parent_id/genre/brief/position）、
  変更、削除（確認付き。409 `org_node_in_use` の文面をそのまま出す）。`genre` は `GET /config` の
  `genres[]` があれば選択式、無ければ自由入力（`/tasks/new` と同じ落とし方）。組織は DB が正
  （ADR-0033 D1）なので、プロバイダ・秘密の管理と違い **`POST /reload` は呼ばない**。
  - **「抱えている仕事の数」の求め方（taskd への依頼あり。後述）**: `GET /tasks` の応答（`TaskSummary`）に
    `assignee` が載っていない（`Task`/`ProjectTaskView` にはあるが一覧の要約型には無い）ため、`GET /projects`
    の全案件について `GET /projects/{id}` を束ねて取り、その `tasks[].assignee`（`ProjectTaskView`）を
    `app/lib/org-tree.ts` の `countWorkload` で集計している。**案件に属さない（`project_id` が無い）タスクの
    割り当ては数えられない**（未解決事項参照）。
- **`/projects`**（`app/routes/projects.tsx`）: 一覧は `GET /projects` に加え、「途中目標の数」を出すため
  各案件の `GET /projects/{id}` を束ねて `milestones.length` を添える（API 応答をそのまま数えるだけで、
  GUI 側の新しい判断はしていない）。新しい案件フォーム（title + request、`placeholder` に SPEC §6 の
  例文「Pluvio を基盤に用いた新たな研究テーマの模索、検証」）→ `POST /projects` → 成功したら詳細へ redirect。
- **`/projects/:id`**（`app/routes/projects.$id.tsx`）: `request` 全文、`secretary_summary`（あれば）、案件の
  `status` 変更（select + submit）、途中目標の一覧（`seq` 順、`status` バッジ、Go/再設計の状態変更フォーム）、
  途中目標を足すフォーム。**仕事の木**: `GET /projects/{id}` の `tasks`（`ProjectTaskView[]`）を
  `app/lib/work-tree.ts::projectTasksToGraph` で `/graph`（`app/routes/graph.tsx`）と同じ `Graph` 型に写し、
  同じ `layoutGraph`（`app/lib/graph-layout.ts`）で描く。新しい部品 `app/components/WorkTree.tsx` は `/graph`
  と違い **ノードをクリックすると `/tasks/:id` へ移る**（`onNodeClick` を足しただけで、レイアウト・色分けは
  `/graph` と共有）。各ノードには `assignee` の**組織ノードの名前**（`GET /org` と突き合わせ）を
  `layoutGraph` の `role`（ラベル 2 行目）に流用して出す。
- `app/components/Flash.tsx` に `OrgActionFlash` / `ProjectActionFlash`（`ProviderActionFlash` 等と同じ形。
  組織は reload が無い分クラスタの `ClusterConnectOutcome` に近い）。
- `help.tsx`: 用語集に「組織」「案件」「途中目標（milestone）」「仕事の木（DAG）」を SPEC の言葉で追加、
  「画面ごとの説明」に秘書・組織・案件・報告・認可・成果物の 6 行を SPEC §4 の順で追加。
- testid: `org-tree` / `org-node`（`data-org-id`）/ `org-node-detail` / `org-add-form` / `org-add-submit` /
  `org-edit-form` / `org-edit-submit` / `org-delete` / `project-row`（`data-project-id`）/ `project-status` /
  `project-milestone-count` / `project-new-form` / `project-title` / `project-request` / `project-new-submit` /
  `project-status-form` / `project-status-submit` / `project-request-text` / `project-secretary-summary` /
  `milestones-section` / `milestone-row`（`data-milestone-id`）/ `milestone-status` / `milestone-status-submit` /
  `milestone-new-form` / `milestone-new-submit` / `work-tree`（`work-tree-placeholder` はマウント前）。

### 実装中に見つけて直したもの（`app/taskd/client.server.ts`）

**`DELETE /org/{id}` の実機確認で、削除が成功しても例外になっていた。** `crates/task-api/src/handlers.rs::delete_org_node`
（Phase 23）は `StatusCode::NO_CONTENT`（204、本文なし）を返すが、他の管理系の `DELETE`（`/providers/{id}` 等）は
200 で `{}` を返す。`TaskdClient.delete()` は常に `res.json()` を呼んでいたため、204 の空本文の解析に失敗し、
`TaskdError`/`TaskdUnavailable` ではない素の `SyntaxError` として `deleteOrgNode` の外へ漏れていた
（`toActionError` は未知の例外を re-throw するので、action が 500 になる）。**taskd は §3.45 の文書どおりに
動いている**（GUI 側の共有クライアントが 204 を想定していなかっただけ）ので、`taskd-requests.md` には書かず
`TaskdClient.delete()` 側で 204 を素通しするよう直した（`res.status === 204` なら `{}` を返す）。
`test/unit/client.test.ts` に空本文の 204 で例外にならないことの回帰テストを追加、`test/unit/org.test.ts` の
delete 系テストも `res.writeHead(204); res.end();`（本文なし）で実機に合わせた。

### 受け入れ条件と証拠

- `pnpm gen:types` を 2 回実行して差分ゼロ（`git diff --exit-code app/taskd/types.ts` 相当を目視確認）。
- `pnpm lint`（biome、134 files）/ `pnpm typecheck` / `pnpm build` すべて exit 0。
- `pnpm test` **281 passed**（30 ファイル）。新規: `test/unit/org-tree.test.ts`（`buildOrgTree` の根・孤児・
  並び順、`countWorkload`/`flattenProjectTasks`）、`test/unit/work-tree.test.ts`（`projectTasksToGraph` の
  parent_id/depends_on の写し方、assignee → 組織名の解決）、`test/unit/org.test.ts`（`loadOrg` が `GET /org` +
  各案件の `tasks` から木と件数を組む、`buildOrgCreateInput`/`buildOrgPatchInput`、`createOrgNode`/
  `patchOrgNode`/`deleteOrgNode` の成功・409 `org_node_exists`・409 `org_node_in_use`・401・204 no-body）、
  `test/unit/projects.test.ts`（`loadProjects` の途中目標件数の束ね、1 件の詳細取得失敗時のフォールバック、
  `createProject` の成功・422）、`test/unit/projects.detail.test.ts`（`loadProjectDetail`、`patchProjectStatus`、
  `createMilestone`、`patchMilestoneStatus` の成功・404・400）。
- **実機での見た目の確認**（使い捨ての taskd。運用中の 7710/7700 には触れていない）: `scripts/taskd.sh build`
  でビルドし、`config/org.example.toml` を `org_include` に、`taskd.toml` に `[[genres]] coding` /
  `[[genres]] literature`（`config.rs` のテストと同じ内容）を足した使い捨て taskd を 127.0.0.1:17910 に、
  GUI を 127.0.0.1:17900 に起動。`POST /projects` で「Pluvio の新テーマ」案件、`POST /projects/{id}/milestones`
  で途中目標を 1 件、`POST /tasks` で assignee・project_id・milestone_id・parent・depends_on 付きのタスクを
  4 件作った（親子 1 組、depends_on 2 本、assignee は `research-survey` / `coding-poc` / `infra` の 3 者、
  1 件は未承認の draft のまま）。Playwright で確認したこと:
  - `/org`: 秘書を根に部→課の縦の組織図、`coding-poc` を選ぶと brief・分野・「抱えている仕事（未終了）0 /
    担当した仕事（累計）1」・`done` の「PoC検証」タスク・無効化した「話す（G13b）」ボタンが出る。`infra`
    部門（課を持たず直接 assignee にした）が「1」（draft の未終了タスク）を正しく数えている。
  - `/projects`: 一覧に「Pluvio の新テーマ」（proposed・途中目標 1）、新規案件フォームの placeholder に SPEC
    §6 の例文。
  - `/projects/:id`: 依頼全文、途中目標（#1、approved バッジ）、**仕事の木**が親子（点線の枠でグルーピング）
    ＋ depends_on の辺（2 本）を正しく描き、各ノードに `[関連研究調査課]` `[PoC・R&D 課]` `[インフラ部]` の
    組織名が出て、status の色（done=緑、draft=中立）も `/graph` と同じ配色で出る。ノード「PoC検証」を
    クリックすると `/tasks/<id>` に実際に遷移した。
  - **401**（`[api] token_file` 未設定）: `/org` の追加・削除は `unauthorized` + ADR-GUI-0012 D1 の案内文。
  - **409 `org_node_in_use`**: `[api] token_file` ありの構成に張り替え、子を持つ「コーディング部」の削除を
    試みると `org node coding is still in use: 3 child node(s) still report to it` がそのまま出た。
  - light / dark 両方で確認。確認後は taskd・GUI とも停止し、使い捨てディレクトリは削除済み。
- e2e（Playwright の `pnpm e2e` 一式）は運用中の taskd / GUI（7700/7710）と衝突するため今回は実行していない
  （上記の実機確認は別ポート・別ディレクトリの使い捨て taskd で行った）。

### taskd への依頼（`docs/taskd-requests.md` に追記）

- **R3（`docs/taskd-requests.md` の「未対応」。BLOCKED ではない。組織の木の「抱えている仕事」用）**: `GET /tasks` の `TaskSummary` に `assignee` /
  `project_id` / `milestone_id` が無い（`Task` と `ProjectTaskView` にはある）。今回は `GET /projects` の
  全件を `GET /projects/{id}` で束ねて代替した（案件に属さない `assignee` 付きタスクは数えられない）。
  `TaskSummary` に `assignee` を足す、または `GET /tasks?assignee=` を足すと、案件をまたいだ正確な
  「抱えている仕事」の集計が 1 回の要求でできる。

### 未解決事項

- G13a-U1: 「抱えている仕事の数」は `project_id` の無いタスクの `assignee` を数えられない（上記のとおり
  `GET /projects/{id}` の束ねで代替しているため）。taskd に `TaskSummary.assignee` が足されたら、そちらを
  1 回の `GET /tasks` 呼び出しに寄せる。
- G13a-U2: `/projects` の一覧は案件数ぶん `GET /projects/{id}` を呼ぶ（途中目標の件数を出すため。N+1）。
  一人で使う前提で案件数は少ない想定だが、案件が増えたら `ProjectList` 自体に `milestone_count` を足す方が
  素直（taskd 側の設計判断なので提案に留める）。
- G13a-U3: 秘書・報告・認可・成果物はプレースホルダのまま（G13b、taskd 側 Phase 24〜26 待ち）。
- G13a-U4: e2e 未実行（上記の理由）。DOM を描画する unit テストが無い件（G10-U1）も未解決で、`/org` の木の
  描画・`/projects/:id` の仕事の木の描画は目視と Playwright のスクリーンショットでのみ確認している。
- G13a-U5: 組織の編集フォーム（`buildOrgPatchInput`）は `name`/`brief` を空にして保存すると「変更なし」
  ではなく空文字を送る（`providers-admin.server.ts` の `model` と同じ扱いに揃えた設計判断。ADR は起こしていない）。

### 提案（`docs/gui/api.md` への変更提案。採否は人間）

- G13a-P1: 3.42〜3.49 に「`GET /tasks` の `TaskSummary` には `assignee`/`project_id`/`milestone_id` が無い」旨と、
  GUI 側は `GET /projects/{id}` を束ねて代替していることを明記すると、次に触る人が同じ勘違い（`GET /tasks`
  に `assignee` フィルタがあるはずと読んでしまう）をしなくて済む。
- G13a-P2: §3.45 の「204」は他の管理系 DELETE（3.24〜3.28 の provider 等は 200 `{}`）と揃っていない。
  意図的なら「本文なし」と明記し、GUI 側の `TaskdClient` 実装者への注意書きを添えるとよい（今回
  `res.json()` が空文字列で例外になる実装バグを実機で見つけて直した）。

## Phase G13b-1 — 報告の流れ（2026-09-17）

taskd 側の Phase 25（ADR-0033 D3、ADR-0034: 報告の生成・圧縮）への追従。SPEC §4 の 6 画面のうち
「報告の流れ」を実装し、プレースホルダを置き換えた（ADR-0033 D8）。GUI 側の新しい設計判断は無し
（taskd 側 ADR をそのまま実装。§3.50〜3.53 の契約に従うだけ）。

### 成果物

- `pnpm gen:types` は再生成済みの `app/taskd/types.ts`（`Report` / `ReportDetail` / `ReportList` /
  `ReportKind` / `ReportsReadBody` / `ReportsReadResult` / `ReportsNotifiedResult` / `DaemonSnapshot.reports` /
  `ReportsLive`）をそのまま使用。2 回実行して差分ゼロを確認。
- `app/lib/reports.ts`（新規、純粋関数。DOM を描画する unit テストが無い件（G10-U1）を踏まえ、判断・計算は
  すべてここに集約）: `buildReportsQuery`（`GET /reports` のクエリを組む。**`kind` は taskd の API に無いので
  含めない** — §3.50 は `project`/`node`/`level`/`unread`/`limit` しか受け付けず「知らないクエリキーは 400」。
  既定は `level=0&unread=true`）、`filterReportsByKind`（kind の絞り込みは画面側だけで行う）、
  `reportProjectName` / `reportNodeName`（`GET /projects` / `GET /org` からの名前解決。`project_id` が無ければ
  「案件なし」。ADR-0034 D1）、`relativeTimeLabel`（既存の `formatDuration(secondsBetween(...))` の流用）、
  `reportsBadgeTone`（bad_news があれば `danger`）、`notificationMessage` / `reportsNotificationKey` /
  `shouldFireNotification`（ADR-0034 D6 の `notify_now` は taskd が決める。**bad_news の未読がある間
  `notify_now` は常に true**なので、GUI 側だけで「未読件数の組が変わらない限り再度は鳴らさない」重複排除を
  足した。判断が必要だった点、後述）。
- `app/taskd/reports-admin.server.ts`（新規）: `markReportsRead` / `markReportsNotified`
  （`POST /reports/read` / `POST /reports/notified`。管理系、`org-admin.server.ts` と同じ作り）。
- `app/components/ReportsList.tsx`（新規）: 報告 1 件の行（`ReportRow`。kind バッジ・headline・案件名・
  担当ノード名・相対時刻・未読ドット）を `/reports` と `/projects/:id` の「報告」タブで共有する部品。
  クリックで展開すると `GET /reports/{id}`（`app/routes/reports.$id.tsx`。resource route、コンポーネント無し）
  を呼んで `body` と `sources_expanded` を出し、`sources_expanded` の各報告も同じ `ReportRow` で再帰的に
  展開できる（「圧縮の元を見に行ける」）。展開した行の中に「既読にする」（1 件、`POST /reports/read`）。
- `app/routes/reports.tsx`（プレースホルダを置き換え）: 既定は秘書レベル（`level=0`）の未読を新しい順に
  1 件 1 行で（`GET /reports` 自体が新しい順）。絞り込み（filter: 未読だけ/全部、level: 秘書/部/課/すべて、
  project）は `<Form method="get">` で taskd に転送、kind は画面側のチェックボックスで絞る（`useSearchParams`
  から読み、taskd へは送らない）。「表示中の未読をすべて既読にする」ボタン（現在のフィルタ後の一覧の未読 id
  をまとめて `POST /reports/read`）。
- `app/routes/reports.$id.tsx`（新規、resource route）: `GET /reports/{id}` の中継のみ。
- `app/routes.ts`: `reports/:id` を追加。
- `app/routes/projects.$id.tsx`: 「報告」タブを追加（`GET /reports?project=<id>`。**`level` を付けない
  ＝全レベル**。この案件のすべての段の報告を、同じ `ReportsList` で出す）。
- `app/root.tsx`: root loader で `GET /daemon` を呼び `DaemonSnapshot.reports`（`ReportsLive`）を
  `reportsLive` として loaderData に足す（taskd に届かない・エラーなら badge を出さないだけにする）。
  ナビの「報告」に未読数バッジ（`reportsBadgeTone` で bad_news があれば赤、無ければ中立。0 件は出さない）。
  「報告」の隣に「通知を有効にする」ボタン（`Notification.requestPermission()` を呼ぶだけ。taskd には
  問い合わせない）。`app/components/NotificationsWatcher.tsx`（新規、root に 1 回だけマウント）が
  `reportsLive` を見て `shouldFireNotification` が true を返したらブラウザ通知を 1 回出し、続けて
  `POST /reports/notified`（`/reports` route の action に `intent=reports_notified` で `fetcher.submit`）を呼ぶ。
- `app/taskd/action-types.ts` に `ReportOpOutcome`、`app/components/Flash.tsx` に `ReportActionFlash`。
- `help.tsx`: 「報告」画面の説明を実装内容に更新、用語集に「報告」「悪い知らせ」「圧縮」を SPEC の言葉で追加。
- testid: `reports-section` / `report-row`（`data-report-id` / `data-report-kind`）/ `report-headline` /
  `report-body` / `report-sources` / `report-mark-read` / `reports-filter-level` / `reports-unread-badge` /
  `notifications-enable`。

### 実装中に実機で見つけて直したもの

**`ReportRow` の個別「既読にする」が 401 のとき、何も表示されずに黙って失敗していた。** 実機（token_file
未設定の使い捨て taskd）で「既読にする」を押しても見た目が変わらず、原因を追うと `readFetcher.data` の
`ok: false` を握りつぶしていた（`isRead` の判定にしか使っていなかった）。`ReportsList.tsx` に
`{readFetcher.data && !readFetcher.data.ok && <ErrorFlash error={readFetcher.data.error} />}` を足し、
行の中に 401 の文言（ADR-GUI-0012 D1 の案内込み）が出るようにした。他の管理系操作（`OrgActionFlash` 等）は
ページ全体の `fetcher.data` を見るので同じ穴は無いが、行ごとに `useFetcher` を持つ `ReportRow` だけこの形に
なっていた。回帰テストは追加していない（DOM を描画する unit テストが無いため。G10-U1。実機で確認済み）。

### 受け入れ条件と証拠

- `pnpm lint`（biome、140 files）/ `pnpm typecheck` / `pnpm build` すべて exit 0。`pnpm test`
  **316 passed**（31 ファイル）。新規 `test/unit/reports.test.ts`（33 件）: `buildReportsQuery`
  （既定 `level=0&unread=true`、`filter=all` で unread を送らない、`level=` で level を送らない、`kind` を
  絶対に送らない）、`filterReportsByKind`、`reportProjectName`/`reportNodeName`、`relativeTimeLabel`、
  `reportsBadgeTone`、`notificationMessage`/`reportsNotificationKey`、`shouldFireNotification`（許可・
  `notify_now`・重複排除の場合分け）、`loadReports`（クエリの組み立てと `GET /projects`/`GET /org` 失敗時の
  フォールバック）、`loadReportDetail`、`markReportsRead`/`markReportsNotified`（成功・401・404
  `report_not_found`）。`test/unit/projects.detail.test.ts` に 2 件追加（`GET /reports?project=<id>` を
  `level` 無しで呼ぶこと、失敗時のフォールバック）。
- `pnpm gen:types` を 2 回実行して差分ゼロ（`app/taskd/types.ts` は既に Phase 25 分を含んでいた）。
- **実機での見た目の確認**（使い捨ての taskd。運用中の 7710/7700 には触っていない）: `scripts/taskd.sh build`
  で taskd をビルドし、`config/org.example.toml` を種にした組織と `[[genres]] coding`/`literature`、
  `[adapters.fake]` を TITLE の内容で分岐する fake-worker（`FAIL` → `error`、`ASK` → `question`、それ以外 →
  `done`）に差し替えた使い捨て taskd を 127.0.0.1:17950 に、GUI を 127.0.0.1:17940 に起動した。
  `POST /projects` で「Pluvio の新テーマ」案件、`assignee`/`project_id` 付きのタスクを 3 件（`coding-poc` の
  PoC 検証→`done`、`research-survey` の関連研究調査→`question`、`project_id` 無しで `infra` のノード監視→
  `error(retryable=false)`）作って承認し、Phase 25 の生成経路（`task-dispatch::record_run_report`）で
  `result`/`question`/`bad_news` の 3 種の報告を実際に作らせた（`reports` テーブルへの直接 INSERT はしていない）。
  Playwright で確認したこと（light/dark 両方）:
  - `/reports` 既定表示: 秘書レベルの未読が新しい順、`bad_news` は赤バッジ・目立つ見た目、`案件なし`
    （project_id 無しの bad_news）が正しく出る、担当ノード名（秘書 = bad_news は各祖先へ複製されるため）。
  - 行を展開: `body`（理由・retryable）と「既読にする」、`sources_expanded`（圧縮元 = infra レベルの
    元の bad_news）が入れ子で表示され、さらに展開できる。
  - level フィルタを「すべて」にすると、`結果`（中立）・`質問`（注意色）が課のレベルのまま見える
    （4 件溜まる／2 時間経つまで秘書へ圧縮されないのは ADR-0034 D3 の設計どおり）。
  - `/projects/:id` の「報告」タブ: 案件に紐づく全レベルの報告（20 件）が同じ行部品で出る。
  - ナビの「報告」バッジ: 未読数が赤（bad_news 有り）で表示され、既読にすると件数が実際に減る
    （`10` → `9`。SSE の再検証で自動的に更新された）。
  - **401**（`[api] token_file` 未設定）: 個別の「既読にする」で 401 の文言が出る（上記のバグ修正の確認）。
  - **管理系トークンを設定**した構成に張り替え、「既読にする」が成功して一覧から消え、バッジも減ることを確認。
  - **通知**: `context.grantPermissions(["notifications"])` と `window.Notification` を差し替えたスタブで
    確認（実ブラウザの通知許可 UI は自動テストできないため）。root を開いた時点で `notify_now: true` を検知し、
    `new Notification("taskd: 報告", { body: "悪い知らせ 9 件 / 未読の報告 9 件" })`
    （`unread_bad_news > 0` を先頭に出す仕様どおり）が 1 回だけ呼ばれ、続けて `POST /reports/notified`
    （`/reports.data`、React Router の single-fetch 経由）が呼ばれることを確認した。
  - 「通知を有効にする」ボタン: クリックで `Notification.requestPermission()` が呼ばれることを確認。
  - 確認後は taskd・GUI とも停止し、使い捨てディレクトリ（scratchpad）は削除済み。
- e2e（Playwright の `pnpm e2e` 一式）は運用中の taskd / GUI（7700/7710）と衝突するため今回は実行していない
  （上記の実機確認は別ポート・別ディレクトリの使い捨て taskd で行った）。

### 未解決事項

- G13b1-U1: `shouldFireNotification` の重複排除（未読件数の組が変わらない限り再通知しない）は taskd 側の
  ADR-0034 には明記されていない GUI 側だけの判断（「判断が必要だった点」）。bad_news の未読がある間
  `notify_now` は常に true という仕様（ADR-0034 D6）のままだと、SSE の daemon イベントのたびに通知APIを
  呼び続けることになりかねないため、今回は「未読件数が変わらない限り鳴らさない」を GUI 側に追加した。
  taskd 側で `notify_now` の意味を変える（例: 一度 `notified` を呼んだら bad_news でも一定時間は false にする）
  なら、この GUI 側の重複排除は不要になる。ADR は起こしていない（GUI 単体の実装詳細と判断したため）。
- G13b1-U2: kind の絞り込みは taskd の `GET /reports` に無いため画面側だけで行う。案件・レベルの絞り込みと
  違い、ページング（`limit`）を跨いだ絞り込みにはならない（1 ページの中でだけ絞る）。件数が多い運用では
  taskd 側に `kind` フィルタを足す方が素直（`docs/gui/api.md` への提案は下記）。
- G13b1-U3: DOM を描画する unit テストが無い件（G10-U1）は未解決のまま。`ReportsList`/`ReportRow`/
  `NotificationsWatcher` の描画・展開・通知の配線は実機の Playwright スクリーンショットとスタブ検証でのみ
  確認している。
- G13b1-U4: e2e 未実行（上記の理由）。
- G13b1-U5: 秘書・認可・成果物はプレースホルダのまま（taskd 側 Phase 24・26 待ち）。

### 提案（`docs/gui/api.md` への変更提案。採否は人間）

- G13b1-P1: `GET /reports` に `kind` フィルタを足すと、GUI 側の画面内絞り込み（1 ページ限定）をやめて
  taskd に転送できる。
- G13b1-P2: ADR-0034 D6 の `notify_now`（bad_news の未読がある間は常に true）は、GUI が「同じ状態のまま
  何度も通知しない」重複排除を自前で持つ前提になっている。§3.53 に「GUI 側で最後に通知した未読件数の組を
  覚えておき、変化があったときだけ通知を出すことを想定している」と明記すると、次に触る人がこの前提を
  再発見しなくて済む。
## Phase G13b-2 — 秘書との対話（2026-09-17）

taskd 側 Phase 24（ADR-0033 D4/D6: `messages`、`POST/GET /org/{id}/messages`、`POST /projects` の最初の返事）への
追従。SPEC §4 の 1「秘書との対話 — 案件を投げる、状況を聞く、方針を変える」と 2「組織の木 — ノードを選ぶと
その『人』に直接話せる」、§3.4「口出し」を実装し、G13a で置いた `/org/secretary` のプレースホルダと
`/org` の無効な「話す（G13b）」ボタンを実物に置き換えた。**taskd 側は Phase 24 のブランチ
（`worktree-agent-ab88f57bd6139007d`、`e8f097a`）をこの worktree に取り込んでビルドした**（衝突は Phase 25 と
同じ行に触っていた 4 ファイル＋文書 3 ファイル。どちらも残す形で解決し、`cargo test --workspace` で確認した）。

### 成果物

- **`app/lib/conversation.ts`（新規、純粋関数）**: `projectTitleFromText`（本文の先頭 40 字、超えたら `…`。
  改行は空白に潰す）、`replyArrived`（ポーリングの終了条件: 送った発言（202 の `message_id`）より後ろに
  `role = "node"` の行が入ったか。`message_id` が分からないとき＝案件を作った直後は「一覧の最後が node」）、
  `CONVERSATION_POLL_MS = 2500` / `CONVERSATION_WAIT_LIMIT_MS = 10 分`、`SECRETARY_NODE_ID`、`ConversationData`。
- **`app/taskd/conversation.server.ts`（新規、中継）**: `loadConversation`（`GET /org` + `GET /projects` +
  `GET /org/{id}/messages?project=&limit=200` を束ねる。`GET /org` だけは落ちても続ける＝名前の表示にしか
  使わないため。`?project=` が空文字のときは「案件なし」として送らない）、`sendMessage`（`POST /org/{id}/messages`。
  **管理系**。202 の `{message_id, task_id}` をそのまま返す）、`startProjectFromMessage`（`POST /projects`
  `{title: 先頭 40 字, request: 本文}`。作った後に GUI から続けて話しかけない＝taskd が秘書の最初の返事を
  自分で起こすため）、`buildMessagePostBody`、`runConversationAction`（フォームの `new_project` の有無だけで
  どちらを呼ぶか決める。本文の中身は読まない）。
- **`app/components/Conversation.tsx`（新規、画面）**: 左に案件の選択（「案件なし（雑談）」を含む `<select>`。
  選ぶと `?project=` に GET）、中央にそのノード・その案件のやり取り（古い順、`role` で左右に分け、
  **node の返事は `MarkdownViewer` で描く**）、下に入力欄・送信・（秘書のときだけ）「新しい案件として投げる」。
  送信 → 202 → **「考え中」を出して `GET` を 2.5 秒ごとに引き直し**（`useRevalidator`）、返事が入ったら止める。
  10 分で諦めて案内を出す（`conversation-timeout`）。返事に `run_id` があり、それが**その画面で送った発言への
  返事**なら `/tasks/{task_id}`（202 が返したタスク）へのリンクを出す。それ以外は run id をそのまま見せる
  （`Message` に `task_id` が無いため。taskd への依頼 R4）。
- **ルート**: `app/routes/org.secretary.tsx`（プレースホルダを置き換え。相手は `secretary` 固定）と
  `app/routes/org.$id.tsx`（新規。`/org/:id`）。どちらも同じ中継・同じ部品で、action は
  `POST /org/{id}/messages` の結果を **202 のまま**返し、「新しい案件として」のときだけ
  `/org/…?project=<新しい案件>&waiting=1` へ redirect する（`waiting=1` は「秘書の最初の返事を待つ」印）。
  `app/routes.ts` への追加は 1 行（`org/:id`。静的な `org/secretary` を先に置く）。
- **導線**: `/org` のノード詳細の「話す」を有効化（`org-talk` → `/org/:id`、秘書は `/org/secretary`）。
  `/projects/:id` の仕事の木の下に、担当の付いたタスクごとの「担当に話す」（`work-tree-talk` →
  `/org/{assignee}?project={project}`）。`/projects` の新規フォームは残したまま、
  「秘書に話しかけても同じです」の案内を足した（`project-new-secretary-hint`）。
- **`help.tsx`**: 画面の説明「秘書」を実物の説明に差し替え、用語集に「秘書」「口出し」「対話（messages）」を
  SPEC の言葉で追加。
- testid: `conversation` / `conversation-project-select`（`conversation-project-form`）/ `conversation-message`
  （`data-role`）/ `conversation-input` / `conversation-send` / `conversation-thinking` /
  `conversation-new-project-toggle` / `conversation-run-link` / `conversation-timeout` / `org-talk` /
  `work-tree-talk`（`work-tree-assignees`）/ `project-new-secretary-hint`。

### 判断したこと（ADR は起こしていない。GUI の中の話）

- **「案件を投げる」は明示のトグルにした**（SPEC §4 の「案件を選ばず本文を送るとそれが新しい案件」を、
  トグルの**既定値**で表現）。`/org/secretary` で案件を選んでいなければ「新しい案件として投げる」は
  最初から入っており、案件を選ぶと外れる。action は本文を読まず、フォームの値だけで `POST /projects` と
  `POST /org/{id}/messages` を切り替える（GUI 側で新しい判断をしない、という既存の方針に合わせた）。
- **返事待ちはポーリング（2.5 秒）**。SSE（`GET /stream` の `task.event`）でも終端は拾えるが、`messages` に
  行が入るのは run の終端処理の中なので、結局 `GET /org/{id}/messages` を引き直すことになる。
  引き直しは既存の `useRevalidator`（loader の再検証）で足り、新しい購読を足さずに済む。
- **「考え中」の終了条件は 202 の `message_id` を基準にする**（件数の増減ではなく）。再検証で一覧が入れ替わっても
  判定がぶれない。案件を作った直後だけは自分の発言の id が分からない（`POST /projects` は返さない）ので、
  「一覧の最後が node の発言」を使う。
- **10 分で諦める**。run が落ちた場合は taskd が「返事できませんでした: …」を返事として入れる（実機で確認）ので
  通常は止まるが、万一何も入らないときに永久にポーリングしないための保険。
- **`/projects` の新規フォームは残した**（SPEC は「秘書に話しかけても同じ」であって、フォームを消せとは言っていない。
  `title` を自分で決めたいときのために残し、案内文で対話と同じであることを書いた）。

### 受け入れ条件と証拠

- `pnpm lint`（biome、139 files）exit 0 / `pnpm typecheck` exit 0 / `pnpm build` exit 0。
- `pnpm test` **302 passed**（31 ファイル。G13a の 281 から +21）。新規 `test/unit/conversation.test.ts` **21 件**:
  `projectTitleFromText`（40 字まで・超えたら `…`）、`replyArrived`（未反映・返事前・返事後・前の返事は数えない・
  id が無いとき）、`loadConversation`（3 本を束ねる／`?project=` が付く／空の `?project=` は送らない／
  `GET /org` が落ちても出す／404 `org_node_not_found` と 401 をそのまま投げる）、`buildMessagePostBody`、
  `sendMessage`（202 の素通し・本文・401・404）、`startProjectFromMessage`（`POST /projects` の本文が
  `{先頭 40 字, 本文}`・続けて `/messages` を呼ばない・422）、`runConversationAction`（トグルの有無で呼び分け）。
- `pnpm gen:types` を 2 回実行して同一、かつコミット済みの `app/taskd/types.ts` と同一（Phase 24 取り込み後に
  1 度再生成してある）。`scripts/sync-gui-docs.sh --check` = `up to date`。
- **実機での見た目と動作の確認**（使い捨ての taskd を 127.0.0.1:17911、GUI を 127.0.0.1:17901 に立てた。
  **運用中の 7710 / 7700 には触れていない**）。構成: Phase 24 を取り込んだこのブランチを `cargo build`、
  `org_include = config/org.example.toml`、`[memory] dir`、`[[genres]] secretary / coding / literature`、
  全役割を偽アダプタ `fake`（LLM は呼ばない。`done` の `summary` に秘書の返事の形の Markdown を出すだけ）に
  当て、`[api] token_file` あり。Playwright（light / dark）で確認したこと:
  - `/org/secretary`: 空のスレッドで「新しい案件として投げる」が**既定で入っている**。本文だけ書いて送ると
    `POST /projects` → `?project=<id>&waiting=1` へ移り、**「考え中」が出て**、2.5 秒ごとの再検証の後に
    **秘書の返事が Markdown（`## 理解の確認` / `## 大まかな方針` / `## 最初の途中目標（提案）` の 3 見出し）で
    画面に出た**。案件名は本文の先頭 40 字 + `…`。案件の選択も作った案件に切り替わり、トグルは外れる。
  - 続けて同じスレッドに「状況を教えてください。」と送ると `user → node → user → node` の順に並び
    （SPEC §3.4「先週の議論の続きとして話せる」）、2 通目の返事には「この返事を作った run（裏方）」の
    `/tasks/:id` リンクが出た。「案件なし」に切り替えるとやり取りは 0 件（混ざらない）。
  - `/org?selected=coding-poc` の「話す」→ `/org/coding-poc` に遷移し、案件なしの雑談として送って返事が出た
    （分野を持つノードなので `coding` の役割で run。秘書以外にはトグルが出ない）。
  - `/projects/:id`: 仕事の木の下に「担当: 秘書」＋「担当に話す」（→ `/org/secretary?project=…`）が出る。
  - `/projects`: 「秘書に話しかけても同じです」の案内が出る。`/help#screens` の秘書の説明も更新済み。
  - **失敗した run**（偽アダプタをわざと壊し、終端メッセージを出さずに終わらせた）: 返事として
    `返事できませんでした: error(retryable=true): worker exited without terminal message (exit=0)` が
    画面に出て「考え中」が止まった（再試行ぶん 2 行入る）。
  - light / dark 両方でスクリーンショットを取得。確認後に taskd・GUI とも停止し、使い捨てディレクトリは削除済み。
- e2e（`pnpm e2e`）は運用中の taskd / GUI（7700/7710）と衝突するため今回も実行していない（上記は別ポートの
  使い捨て環境で行った）。

### 未解決事項

- G13b-2-U1: **過去の返事から裏方の run へのリンクが出せない**（`Message` に `task_id` が無い。taskd への依頼 R4(a)）。
  今はその画面で送った発言への返事にだけリンクが出て、それ以外は run id をそのまま見せている。
- G13b-2-U2: **対話用タスクが案件の仕事の木・タスク一覧に混ざる**（taskd 側 U24-4、依頼 R4(b)）。
  実機でも `対話: …[秘書]` のノードが仕事の木に出る。`title` の前置きで除くのは文書に無い挙動に頼ることになるので
  やっていない。
- G13b-2-U3: 返事待ちはポーリング（2.5 秒）で、SSE は使っていない。返事が 1 秒以内に入ると「考え中」は一瞬しか
  出ない（実機で 1 度、`waitForSelector` が拾えないほど短かった）。
- G13b-2-U4: DOM を描画する unit テストが無い（G10-U1 と同じ）。画面側の分岐（左右の振り分け、Markdown、
  「考え中」、トグルの既定値）は純粋関数のテストと Playwright の目視でのみ確認している。
- G13b-2-U5: 失敗した run は**再試行のたびに**「返事できませんでした: …」が 1 行ずつ入る（実機で 2 行）。
  人から見ると同じ文言が並ぶ。taskd 側で最後の 1 回だけにするか、GUI で畳むかは決めていない。
- G13b-2-U6: `/reports` と通知（G13b-1）は別の担当が並行して作業中のため触っていない。`/approvals` と
  `/artifacts` はプレースホルダのまま（taskd 側 Phase 26 待ち）。

### 提案（`docs/gui/api.md` への変更提案。採否は人間）

- G13b-2-P1: §3.55（`POST /org/{id}/messages`）に、**返事の待ち方の推奨**（`message_id` より後ろに
  `role = "node"` が入るまで `GET` を引き直す）を 1 行書いておくと、次に触る人が件数の増減で判定して
  ぶれる実装をしなくて済む。
- G13b-2-P2: §3.46（`POST /projects`）の応答に、秘書の最初の対話の `message_id` / `task_id` を含める
  （または 3.54 の `Message` に `task_id` を足す）と、案件を作った直後の「考え中」も
  同じ基準（自分の発言の id）で終われる。今は「一覧の最後が node」で代用している。

## Phase G13c — 成果物（2026-09-17）

SPEC §4 の 6 画面のうち「成果物」を実装し、プレースホルダを置き換えた（ADR-0033 D8）。GUI 側の新しい
taskd 依存は無し（既存の `GET /projects`・`GET /projects/{id}`・`GET /tasks/{id}`・`GET /tasks/{id}/artifacts`・
`GET /org` を組み合わせるだけ。taskd 側は触っていない）。

### 成果物

- `app/lib/artifacts.ts`（新規、純粋関数。DOM を描画する unit テストが無い件 G10-U1 を踏まえ、判断・計算を
  ここに集約）: `workspacePlace`（`Task.workspace` の `Local{path}` / `Remote{cluster, path}` を「置き場所」の
  表示に変える。Local は taskd が絶対化した `workspace_dir`（無ければ生の `path`）をそのまま出し
  `vscode://file/<path>` リンクを添える。Remote はコードが実際にあるのはクラスタ側なので `task.workspace.path`
  を使い（`workspace_dir` は手元の写しでしかない。docs/gui/api.md §3.5）、`cluster:path` の形でリンクは付けない）、
  `isSourcesArtifact` / `parseSourcesJson`（`sources.json` を名前で判定し、`[{url, title, engine?, cited}]`
  （docs/adr/0031-web-research-evidence-gate.md）をリンク集に変換。形が違えば `null` で通常の JSON 表示に
  フォールバック）、`resolveAssigneeName`（`~/lib/work-tree.ts` と同じ規則）、`buildProjectArtifactRows`
  （タスクごとに束ねた成果物 `TaskArtifactBundle` を新しい順（`ts` 降順）に平らにする）、`artifactRelativeTime`。
- `app/components/ArtifactsList.tsx`（新規）: 成果物 1 件の行（`ArtifactRow`）を `/artifacts` と
  `/projects/:id` の「成果物」節で共有する部品（`~/components/ReportsList.tsx` と同じ作り。G13b-1 の依頼
  どおり）。本体は「開く」を押したときだけ `/files/tasks/:id/artifacts/:idx` を fetch し taskd が返した実際の
  `Content-Type` でビューアを選ぶ（`~/routes/tasks.$id.tsx::ArtifactRow` / `~/lib/artifact-view.ts` の
  `pickViewer`/`isJson`/`artifactStatusMessage` をそのまま流用。既存の `MarkdownViewer`/`CodeViewer`/
  `ImageViewer`/`Sha256Badge` も流用）。`sources.json` はリンク集（url・title、`cited` は「引用」バッジ、
  クリックで新規タブに開く）、その他の JSON は `CodeViewer` で整形表示。各行に置き場所（`workspacePlace` の
  結果。ローカルは vscode リンク付き、リモートはコピー用のモノスペース表示のみ）と担当ノード名・タスク
  へのリンク・作られた時刻（相対表示）を出す。
- `app/routes/artifacts.tsx`（プレースホルダを置き換え）: 案件を選ぶ（`GET /projects`。`<Form method="get">` +
  `project` の select + 送信ボタン、`/reports` の絞り込みフォームと同じ作り）→ 選んだ案件のタスク
  （`GET /projects/{id}` の `tasks`。仕事の木と同じ集合、ADR-0033 D2）の成果物を横断して一覧する。
  1 タスクごとに `GET /tasks/{id}/artifacts`（成果物本体）と `GET /tasks/{id}`（`workspace_dir` /
  `task.workspace` を「置き場所」に使う）を束ねる（N+1。G13a と同じ判断: 一人で使う前提で案件のタスク数は
  少ない）。案件が見つからない（404 `project_not_found`）場合は例外にせず「案件が見つかりません」を表示。
  taskd への問い合わせを行う私的ヘルパー（`loadTaskArtifactBundles`）はモジュール外に切り出していない
  （下記「実装中に見つけたもの」参照）。担当ノード名は `GET /org` から解決する（taskd 側に判断値を作らせない）。
- `app/routes/projects.$id.tsx`: 「成果物」節を追加（`loadProjectDetail` が同じ形の私的ヘルパーで
  `detail.tasks` ぶんの成果物を束ね、`buildProjectArtifactRows` で組んだ `artifactRows` を loader データに
  足す。画面側は `~/routes/artifacts.tsx` と同じ `ArtifactsList` で出す。G13b-1 の「報告」タブと同じ作り）。
- `help.tsx`: 「成果物」の用語集項目と画面ごとの説明を、プレースホルダの文言から実装内容（横断一覧・
  Markdown 描画・sources.json のリンク集・置き場所の表示）に更新。SPEC §2.2・§3.7 の言葉を引用。
- testid: `artifacts-section`（`/artifacts` 全体、`/projects/:id` の成果物節）/ `artifacts-project-select` /
  `artifact-row`（`data-artifact-name`, `data-task-id`）/ `artifact-view` / `artifact-links` /
  `artifact-workspace`（`data-task-id`）。ほか `artifact-toggle` / `artifact-download` / `artifact-forbidden` /
  `artifact-missing` は `~/routes/tasks.$id.tsx::ArtifactRow` と同じ命名を踏襲。

### 実装中に見つけたもの（判断が必要だった点）

**`.server.ts` への切り出しは React Router のクライアントバンドル除去の対象外になる。** 当初、
`/artifacts` と `/projects/:id` で taskd への問い合わせ（`GET /tasks/{id}` + `GET /tasks/{id}/artifacts` を
束ねる部分）を共有するため `app/taskd/artifacts.server.ts` を新設し、両ルートの `loadArtifacts` /
`loadProjectDetail`（loader 本体ではなく、テスト用に公開している集約関数）からそれを import する形にしたところ、
`pnpm build` が「Server-only module referenced by client」で失敗した。React Router の dot-server プラグインは
`loader`/`action`/`middleware`/`headers` からの参照だけを自動で取り除く（docs/DESIGN.md には明記が無い実装上の
制約）ため、それ以外の**公開エクスポート**（テスト用に `export` している `loadArtifacts` / `loadProjectDetail`
自体）が `.server.ts` を値として import すると、クライアントバンドルにサーバ専用コードが混ざりうると判定されて
ビルドが止まる。既存の `loadProjectDetail` 等が問題なく動いていたのは、`TaskdClient` を**型としてだけ**受け取り
（`client: TaskdClient` は引数の型注釈で、`getTaskdClient()` のような値は呼ばない）、`.server.ts` への値の依存が
無かったため。今回は `app/taskd/artifacts.server.ts` を削除し、集約ロジック（`loadTaskArtifactBundles`）を
各ルートファイルに私的関数として複製した（`~/lib/artifacts.ts` の純粋関数だけを共有する）。ADR は起こしていない
（GUI 単体のビルド上の制約であり、taskd との契約やユーザに見える挙動には関わらないため）。次に GET の集約を
複数ルートで共有したくなったら、`loader` の中だけで呼ぶか、`~/lib/*`（型のみで `.server.ts` に依存しない
純粋モジュール）に置くとよい。

### 受け入れ条件と証拠

- `pnpm lint`（biome、145 files）/ `pnpm typecheck`（`react-router typegen && tsc -b`）/ `pnpm build` すべて
  exit 0。`pnpm test` **335 passed**（33 ファイル。G13b-1 までの 316 + 新規 19: `test/unit/artifacts.test.ts`
  17 件 — `workspacePlace`（local/remote/フォールバック）、`isSourcesArtifact`、`parseSourcesJson`（正常系・
  不正 JSON・非配列・欠損フィールド）、`resolveAssigneeName`、`buildProjectArtifactRows`（新しい順への整列、
  bundle 無しタスクの除外）、`artifactRelativeTime` / `test/unit/artifacts.route.test.ts` 4 件 —
  `loadArtifacts`（project 未指定、案件を選んだときの束ね、404 `project_not_found` の非例外化、1 タスクの
  取得失敗が他のタスクの行に影響しないこと）/ `test/unit/projects.detail.test.ts` に 1 件追加
  （`loadProjectDetail` が `artifactRows` を組むこと）。
- `pnpm gen:types` を 2 回実行し、`git status --porcelain app/taskd/types.ts` が両方とも空（差分ゼロ。
  taskd 側のスキーマは変えていないので当然だが、手順どおり確認した）。
- **実機での見た目の確認**（使い捨ての taskd。運用中の 7710/7700 には触れていない）: `scripts/taskd.sh build`
  で taskd をビルドし、`config/org.example.toml` を `org_include` に、`taskd.toml` に `[[genres]] secretary` /
  `coding` / `literature` を足した使い捨て taskd を `TASKD_API_LISTEN=127.0.0.1:17970` で起動、GUI を
  `TASKD_API_URL=http://127.0.0.1:17970 TASKD_GUI_BIND=127.0.0.1:17971 pnpm dev` で起動した。TITLE 分岐の
  fake ワーカー（`*survey*` → `report.md`/`sources.json`/`research.json` を明示的な `{"type":"artifact",...}`
  で申告、`*PoC*` → `main.rs`）に差し替え、`POST /projects` で「Pluvio の新テーマ」案件、
  `assignee`/`project_id`/`genre` 付きのタスクを 2 件（`research-survey` の survey → `done`（human 承認 1 件
  込み）、`coding-poc` の PoC 実装（`workspace = "lab/pluvio-poc"` を明示） → `done`）作って完了させた
  （`reports` テーブルへの直接 INSERT はしていない。Phase 25 の生成経路で実際に報告も作られた）。
  Playwright（`chromium.launch()` を直接使うスクリプト。`pnpm e2e` 一式は運用中の taskd/GUI と衝突するため
  今回は使っていない）で light/dark 両方のスクリーンショットを確認:
  - `/artifacts?project=<id>`: 案件選択の select に「Pluvio の新テーマ」、成果物 4 件が新しい順
    （`main.rs` → `research.json` → `sources.json` → `report.md`）で並ぶ。`report.md` を開くと Markdown が
    その場で描画（見出し・番号付きリストが効いている）。`sources.json` を開くとリンク集（3 件の url・title、
    引用ありの 2 件に「引用」バッジ、クリックで新規タブ）。`research.json` は `CodeViewer` で整形表示。
    `main.rs` は `code` kind のバッジと `CodeViewer`。各行に置き場所（`/tmp/…/workspaces/lab/pluvio-poc` /
    `/tmp/…/workspaces/01M2R987QZRA73QEN2GSTTQX7Q`。青いモノスペースのリンクで `vscode://file/…`）と
    担当ノード名（`（PoC・R&D 課）` / `（関連研究調査課）`）・`done` バッジ・相対時刻が出る。
  - `/projects/<id>`: 「成果物」節に同じ 4 件が「報告」節の下に出る（依頼・途中目標・仕事の木・報告・成果物の
    順。G13b-1 の並びを踏襲）。
  - light / dark とも配色・コントラストに問題なし（`CodeViewer`/`MarkdownViewer`/バッジとも既存のトークンを
    そのまま使っているので新規の配色調整は不要だった）。
  - 確認後は taskd・GUI とも停止し、使い捨てディレクトリ（`/tmp/taskd-gui-run-rmaeda/g13c-verify`）と
    スクリーンショット・一時スクリプトは削除済み。運用中の `127.0.0.1:7710` / `127.0.0.1:7700` は確認前後で
    `curl` の 200 / 302 を確認し、無傷であることを確かめた。
- e2e（Playwright の `pnpm e2e` 一式）は運用中の taskd / GUI（7700/7710）と衝突するため今回は実行していない
  （上記の実機確認は別ポート・別ディレクトリの使い捨て taskd + 直接 `chromium.launch()` するスクリプトで
  行った）。

### 未解決事項

- G13c-U1: DOM を描画する unit テストが無い件（G10-U1）は未解決のまま。`ArtifactsList`/`ArtifactRow` の
  描画・展開・sources.json のリンク集化・workspace の表示は実機の Playwright スクリーンショットでのみ
  確認している（純粋関数側は unit テスト済み）。
- G13c-U2: `/artifacts` は 1 タスクごとに `GET /tasks/{id}` と `GET /tasks/{id}/artifacts` の 2 回、案件の
  タスク数ぶん呼ぶ（2N+1）。G13a の「抱えている仕事の数」と同じ N+1 の判断だが、こちらは 2 倍。一人で使う
  前提でタスク数は少ない想定だが、増えたら taskd 側に「案件の成果物一覧」を直接返すエンドポイントを足す方が
  素直（下記「提案」）。
- G13c-U3: `sources.json` の判定は**ファイル名の完全一致**（`name === "sources.json"`）で行っている。
  taskd 側にファイル種別を表す専用のフィールドは無い（`ArtifactRef.kind` は自由記述の文字列で、
  `web-research` ハーネスは `"json"` を入れている。他の JSON 成果物と区別できない）ため、名前判定にした。
  同名で別内容のファイルを作るハーネスが増えたら壊れる（現状は `local_deep_research` だけが `sources.json`
  を作る。ADR-0031）。
- G13c-U4: e2e 未実行（上記の理由）。
- G13c-U5: 秘書・認可はまだプレースホルダのまま（taskd 側 Phase 24・26 待ち）。SPEC §4 の 6 画面のうち
  「成果物」「報告」「組織」「案件」の 4 つが実装済み。

### 提案（`docs/gui/api.md` への変更提案。採否は人間）

- G13c-P1: `GET /projects/{id}` の `tasks`（`ProjectTaskView`）に成果物の有無・件数（または `ArtifactView[]`
  そのもの）を足せれば、`/artifacts` の N+1（2N+1）を 1 回の呼び出しに減らせる。件数が多い運用で効いてくる
  （G13a-P2 の「`ProjectList` に `milestone_count` を」と同じ種類の提案）。
- G13c-P2: `ArtifactRef` に「リンク集」「整形表示」等の表示ヒントを持たせる（例: `kind` の語彙を広げて
  `"links"` を足す）と、GUI 側でファイル名の完全一致に頼らずに `sources.json` 相当を判定できる
  （G13c-U3 の代替案）。優先度は低い（現状 1 ハーネスだけの話のため）。
## Phase G13d — 認可（2026-09-17）

taskd 側 Phase 26（ADR-0033 D5: `approvals` / `standing_rules`、`GET/POST /approvals...`、`GET/POST/DELETE /standing-rules...`、
`DaemonSnapshot.approvals_pending`）に追従し、G13a のプレースホルダ（`/approvals`）を実物に置き換えた。
SPEC §3.6「少しでも聞くべきだとエージェントが判断したら、あなたに指示を仰ぐ。あなたはそれに対して『今回だけ』か
『同じようなことは今後ずっと』のどちらかの認可を出す。永続の認可は文字で記録してエージェントに注入する」と、
§4 の 5「認可の要求 — 聞かれたことに『今回だけ／今後ずっと』で答える。永続の認可の一覧と編集」を実装した。
main（`70986b5`、phase 26）に `git merge --ff-only` してから着手。

### 成果物

- **`app/lib/approvals.ts`（新規、純粋関数）**: `splitApprovals`（未決の要求と決めたものの履歴を `Approval.decision`
  の有無で分ける。**実機で `GET /approvals?pending=false` が絞り込まないことを確認したため、`pending` クエリには
  頼らずフィルタ無しの 1 回の取得を `decision` で分ける**。下記「判断したこと」参照）、`approvalProjectName` /
  `approvalNodeName`（`~/lib/reports.ts` と同じ作り。案件名・ノード名を `GET /projects` / `GET /org` から解決する）、
  `standingRuleTargetName`（`node_id` が無ければ「全員」）、`approvalsPendingCount`（`DaemonSnapshot.approvals_pending`
  の既定 0 込みの読み取り）。
- **`app/taskd/approvals-admin.server.ts`（新規、中継）**: `buildApprovalDecideInput` / `decideApproval`
  （`POST /approvals/{id}/decide`。**管理系**）、`buildStandingRuleCreateInput` / `createStandingRule`
  （`POST /standing-rules`。**管理系**）、`deleteStandingRule`（`DELETE /standing-rules/{id}`。**管理系**、204 本文無し）。
  `org-admin.server.ts` と同じ作り: taskd のエラーはそのまま `{ok:false, error}` にする。
- **`app/taskd/action-types.ts`**: `ApprovalOpOutcome` / `StandingRuleOpOutcome` を追加。
- **`app/components/Flash.tsx`**: `ApprovalActionFlash`（`standing` で答えたときは永続の認可への追加も知らせる）、
  `StandingRuleActionFlash` を追加。
- **`app/routes/approvals.tsx`（プレースホルダを置き換え）**: 上に**認可待ち**（`ApprovalRow`。質問を `MarkdownViewer`
  で描き、`answer` の欄 + 3 つのボタン「今回だけ」「今後ずっと」「認めない」+ `scope`（このノードだけ／全員）の
  `<select>`。1 つの `fetcher.Form`（`useFetcher({key: "approval-<id>"})`）に 3 つの `name="decision" value="once|standing|denied"`
  submit ボタンを持たせ、`answer` の欄を共有した）、下に**決めたもの**の履歴（同じ `ApprovalRow`。`decision` があれば
  フォームの代わりに決定・答えを表示）。さらに下に**永続の認可**の一覧（`StandingRuleRow`。`org.tsx` の `org-delete` と
  同じ `<details>` の確認付き削除）と追加フォーム（`node_id` の `<select>`（空 = 全員）+ `rule` のテキストエリア）。
  ノードへのリンク（`裏方のタスク` → `/tasks/{task_id}`）、相対時刻（`~/lib/reports.ts` の `relativeTimeLabel` を再利用）。
- **`app/root.tsx`**: `DaemonSnapshot.approvals_pending` を loader で読み（`approvalsPendingCount`）、ナビの「認可」に
  バッジ（`badge: "org_approvals"`、`data-testid="approvals-pending-badge"`）を追加。既存の受信箱バッジ
  （`badge: "approvals"`、`counts.approvals`）とは別の値・別の testid にした（意味が違う: 受信箱は task の承認待ち、
  こちらは組織のノードからの認可の要求）。
- **`app/routes/org.tsx`**: `loadOrg` が `?selected=<node>` のときだけ `GET /standing-rules?node=<node>` を呼び
  （未選択なら呼ばない）、ノード詳細（`OrgNodeDetail`）に「この人への永続の認可」を読み取り専用で表示
  （`standingRuleTargetName` で全員／このノードのバッジを出し、追加・削除は `/approvals` へのリンクに誘導）。
- **`help.tsx`**: 画面の説明「認可」を実物の説明に差し替え、用語集に「認可」「今回だけ／今後ずっと」「永続の認可」を
  SPEC の言葉で追加。
- testid: `approvals-section` / `approval-row`（`data-approval-id`）/ `approval-question` / `approval-answer` /
  `approval-once` / `approval-standing` / `approval-denied` / `approval-scope` / `approval-task-link` /
  `standing-rules-section` / `standing-rule-row`（`data-rule-id`）/ `standing-rule-add-form` /
  `standing-rule-add-submit` / `standing-rule-delete` / `approvals-pending-badge`。
  `org.tsx` 側にも `org-node-standing-rules` / `org-node-standing-rule`（`data-rule-id`）を追加。

### 判断したこと（ADR は起こしていない。GUI の中の話。実機で見つかった taskd の挙動への対応）

- **`GET /approvals?pending=false` に頼らず、フィルタ無しで 1 回取得して `decision` の有無で分けた**（`docs/taskd-requests.md`
  R5）。実機（使い捨ての taskd、`config/org.example.toml` + 偽アダプタ）で確認したところ、`pending=true` は仕様どおり
  未決定だけに絞れるが、`pending=false` は決定済みかどうかに関わらず全件を返した。`GET /approvals?pending=true` と
  `?pending=false` を素直に 2 回呼ぶ設計（当初案）だと、決めたものの履歴に未決の要求まで混ざって二重表示になる。
  `Approval.decision` は応答に必ず含まれるドキュメント化されたフィールドで、その有無で分けることは
  `filterReportsByKind`（`~/lib/reports.ts`）と同種の「GUI 側でのフィルタ」であって新しい判断値を作ることには
  当たらないと判断し、`splitApprovals` として実装した。`docs/taskd-requests.md` に taskd 側の直し方も添えて記録し、
  直り次第 2 回呼びに戻せるようにしてある。
- **答えるボタンは 1 つの `<form>` に 3 つの `name="decision"` submit ボタン**にした（`app/routes/tasks.$id.tsx` の
  approve/reject は操作ごとに別々の `<Form>` + 別々の `note` 欄を持つが、今回は要求された testid が `approval-answer`
  1 つだけなので、`answer` の欄を共有する 1 つのフォームにした。HTML の標準的な「複数 submit ボタン」パターン）。
- **`scope` は常に送る**（`decision = "standing"` のときだけ意味を持つ。§3.57）。「今回だけ／認めない」を押しても
  `scope` は無視されるだけで害が無いので、UI 側で表示を出し分ける複雑さを避けた。
- **組織の木のノード詳細では読み取り専用**にし、追加・削除は `/approvals` に誘導した（同じ CRUD を 2 箇所に置くと
  どちらが正か紛らわしくなるため。SPEC は「表示」としか言っていない）。

### 受け入れ条件と証拠

- `pnpm lint`（biome、148 files）exit 0 / `pnpm typecheck` exit 0 / `pnpm test` **369 passed**（33 ファイル。
  G13b-2 の 302 から +67。新規 `test/unit/approvals.test.ts` 32 件 + `test/unit/org.test.ts` に標準の認可の
  loader テスト 3 件を追加）/ `pnpm build` exit 0。`pnpm gen:types` を 2 回実行して同一、かつコミット済みの
  `app/taskd/types.ts` と同一（Phase 26 取り込み後に生成済みで差分ゼロ）。
- 新規テストの内訳: `approvalProjectName`/`approvalNodeName`/`standingRuleTargetName`、`approvalsPendingCount`
  （daemon 無し・snapshot 無し・古いスナップショット・値ありの 4 パターン）、`splitApprovals`、`loadApprovals`
  （`GET /approvals` を 1 回だけ呼ぶこと・`GET /org`/`GET /projects` が落ちても返すこと・401 をそのまま投げること）、
  `buildApprovalDecideInput`、`decideApproval`（once/standing/denied・404・401・422）、`buildStandingRuleCreateInput`、
  `createStandingRule`/`deleteStandingRule`（成功・422・401・404・204 本文無し）。`org.test.ts`
  には `?selected=` の有無で `GET /standing-rules` を呼ぶかどうか、`node=` が付くこと、失敗しても組織は返すことを追加。
- **実機での見た目と動作の確認**（使い捨ての taskd を 127.0.0.1:17930、GUI を 127.0.0.1:17901 に立てた。
  **運用中の 7710 / 7700 には触れていない。確認後、taskd・GUI とも停止し使い捨てディレクトリは削除済み**）。
  構成: `config/org.example.toml` を `org_include`、全役割を偽アダプタ `fake`（`question` を毎回返すだけの
  `sh` スクリプト。LLM は呼ばない）に当て、`[api] token_file` あり。`POST /org/secretary/messages` と
  `POST /org/coding-poc/messages` で質問に終わる run を起こし `approvals` を作った。Playwright（light / dark、
  Chromium）で確認したこと:
  - `/approvals`: 認可待ちに質問（`approval-question`、Markdown 描画）・宛先ノード・案件（「案件なし」）・
    裏方のタスクへのリンク・相対時刻が並んだ。ナビの「認可」バッジ（`approvals-pending-badge`）が件数と一致した
    （4 件のときバッジも `4`）。
  - `answer` に文言を入れ、`scope` を「全員」にして「今後ずっと」を押すと、その行が認可待ちから消え、
    「決めたもの」に決定（今後ずっと）と答えが表示され、**「永続の認可」に新しい行（対象: 全員、規則文どおり）が
    現れた**（受け入れ条件のゴール）。`scope` を「このノードだけ」にすると対象がそのノード名になることも確認した。
  - 「認めない」で答えると、決めたものの履歴に「決定: 認めない」「答え: （そのまま）」が出た（taskd 側が
    `"認めない: <answer>"` に整形して再開する、§3.57 の仕様どおり）。
  - 永続の認可の「削除」は `<details>` の確認（「本当に削除しますか？」）を経て `DELETE` した。追加フォームから
    `node_id` を選ぶ／空にする（全員）を試し、どちらも一覧に反映された。
  - 組織の木（`/org?selected=coding-poc`）のノード詳細に「この人への永続の認可」が、そのノード宛て・全員宛て
    両方を含めて表示された。
  - light / dark 両方でスクリーンショットを取得。コンソールエラー・CSP 違反は 0 件。
  - 検証中に taskd の `pending=false` フィルタの不具合を発見した（上記「判断したこと」「未解決事項」参照）。
    また、この偽ワーカーは毎回 `question` で終わる設定にしたため、1 件答えるとタスクが再度実行されてほぼ同じ
    質問がすぐに新しい `approvals` として現れる（実機の仕様どおりの挙動で、GUI のバグではない）。
- e2e（`pnpm e2e`）は運用中の taskd / GUI（7700/7710）と衝突するため今回も実行していない（上記は別ポートの
  使い捨て環境で行った。G13b-2 までと同じ扱い）。

### 未解決事項

- G13d-U1: **`GET /approvals?pending=false` が絞り込まない**（`docs/taskd-requests.md` R5）。GUI 側は
  `decision` の有無で分けて回避済みだが、taskd 側が直ればクエリでの絞り込みに戻せる。
  現状の実装のままでも実害は無い（`Approval.decision` は仕様どおりのフィールド）。
  現状データ量が少ない前提（`GET /approvals` 全件取得）なので、件数が増えたときはページング等の検討が要る
  （taskd 側に `?limit=`/`?before=` 等が無い。今回は範囲外）。
- G13d-U2: DOM を描画する unit テストが無い（G10-U1 と同じ）。3 つのボタンの出し分け・`<details>` の開閉・
  Markdown 描画は純粋関数のテストと Playwright の目視でのみ確認している。
- G13d-U3: `/artifacts`（G13c）は別の担当が並行して作業中のため触っていない。

### 提案（`docs/taskd-api-v1.md` への変更提案。採否は人間）

- G13d-P1: `docs/taskd-requests.md` R5 のとおり、`GET /approvals?pending=false` を
  `decision IS NOT NULL` で絞り込むよう直してほしい。直り次第 GUI 側は 2 回呼び（`pending=true` / `pending=false`）に
  戻し、`GET /approvals` 全件取得をやめられる。

## Phase G13e — Phase 27 への追従（2026-09-17）

taskd 側 Phase 27（ADR-0033/0034 の仕上げ、R3/R4/R5 の解決、`POST /projects` の管理系化）に追従した。
まず `git merge --ff-only main`（`2adea0e`）でこの worktree を Phase 27 まで進めてからビルド・着手した。

### 成果物

- `bash scripts/sync-gui-docs.sh` と `pnpm gen:types` を実行し、`gui/docs/taskd-api-v1.md` と
  `app/taskd/types.ts`（`TaskSummary.assignee`/`.conversation`、`ProjectTaskView.conversation`、
  `Message.task_id`、`schema_version = 7` 等）を taskd 側の Phase 27 に合わせた。
- **`gui/docs/taskd-requests.md`**: R3・R4・R5 を「対応済み」に移した（各節に taskd 側の対応と GUI 側
  Phase G13e での追従を要約し、原文は下に残した）。
- **対話用タスクを裏方に隠す（R3。SPEC「タスクは裏方」/ ADR-0033 D8）**:
  - `app/lib/work-tree.ts::projectTasksToGraph` が `conversation: true` のタスクを仕事の木から**完全に除外**
    （`/projects/:id`）。`app/routes/projects.$id.tsx` は `workTasks`（対話用を除いた配列）を件数表示・
    「担当に話す」一覧にも使うよう揃えた。
  - `app/routes/tasks.tsx`（`/tasks`）は既定で対話用タスクを一覧から隠し、「対話用も表示」チェックボックス
    （`show_conversation=1`、`data-testid="tasks-show-conversation"`）で表示できるようにした。`GET /tasks`
    には送らない（taskd に絞り込みは無いので、`items` を受け取った後 GUI 側の表示だけを切り替える）。
  - `app/lib/org-tree.ts`: `flattenProjectTasks`/`AssignedTaskView`（G13a が `GET /projects/{id}` を
    案件数ぶん束ねていた代替実装）を削除し、`countWorkload` / 新設の `tasksByAssignee` を
    **`TaskSummary[]`**（`GET /tasks?limit=500` を 1 回。`app/routes/tasks.new.tsx` と同じ「全件を 1 回で」
    パターン）から計算する形に直した（N+1 の解消）。どちらも `conversation: true` を数えない・含めない。
  - `app/routes/org.tsx::loadOrg` は `GET /projects`/`GET /projects/{id}` の束ねをやめ、`GET /tasks` を
    1 回呼ぶだけにした。組織ノード詳細の「抱えているタスク」一覧は `TaskSummary` を直接使うため、
    `TaskSummary` に `project_id` が無く**案件名の列は落とした**（トレードオフ。taskd-requests.md R3 に記録）。
- **返事から裏方の run へ（R4）**: `app/components/Conversation.tsx` が `Message.task_id` を直接使うよう
  変更し、`Waiting`/`replyLink`/`linkedIndex`（「その画面で送った直後の発言にだけ」というヒューリスティック）
  を削除した。過去の返事（画面を開き直した後のもの）にも `/tasks/:id` へのリンクが出る。
- **認可の 2 回呼び（R5）**: `app/routes/approvals.tsx::loadApprovals` を `GET /approvals?pending=true` /
  `?pending=false` の 2 回呼びに戻し、`app/lib/approvals.ts::splitApprovals`（G13d の回避策）を削除した。
- **`POST /projects` の管理系化（Phase 27 M-4）の確認**: 401 の応答は他の管理系（`POST /org` 等）と同じ
  `code: "unauthorized"` で、案内文は `app/components/Flash.tsx` の 1 か所（`error.code === "unauthorized"`）
  に集約されているため、エンドポイントごとに揃える作業は不要だった（確認のみ）。
  `app/taskd/projects-admin.server.ts` の冒頭コメントが「管理系ではない」のままだったので Phase 27 に
  合わせて書き直した。
- unit テスト: `test/unit/org-tree.test.ts`（`countWorkload`/`tasksByAssignee` が対話用を除くこと、
  `TaskSummary` ベースへの書き換え）、`test/unit/work-tree.test.ts`（対話用タスクの除外、対話用タスクを
  指す `parent_id`/`depends_on` の扱い）、`test/unit/org.test.ts`（`loadOrg` が 1 回の `GET /tasks?limit=500`
  で組むこと）、`test/unit/approvals.test.ts`（`pending=true`/`pending=false` の 2 回呼び）、
  `test/unit/projects.test.ts`（`POST /projects` の 401）。既存の `ProjectTaskView`/`TaskSummary` を使う
  テストフィクスチャ（`test/fixtures/api/*.json` 含む）に `conversation` フィールドを足した。

### 受け入れ条件と証拠

- `pnpm lint`（biome、152 files）exit 0 / `pnpm typecheck` exit 0 / `pnpm test` **391 passed**（35 ファイル。
  G13d の 369 から純増ではなく、`splitApprovals`/`flattenProjectTasks` 系のテストを削って書き換えた差分込み）。
  `pnpm build` exit 0。
- `pnpm gen:types` を 2 回実行して同一（`diff` ゼロ）。`scripts/sync-gui-docs.sh --check` = `up to date`。
- **実機での見た目と動作の確認**（使い捨ての taskd を 127.0.0.1:17940、GUI を 127.0.0.1:17962 に立てた。
  **運用中の 7710 / 7700 には触れていない**。確認後、taskd・GUI とも停止し使い捨てディレクトリは削除済み）。
  main（Phase 27 込み）を `cargo build -p taskd -p taskctl` し、`config/org.example.toml` を `org_include`、
  `[[roles]]` を全部 `fake` アダプタのまま、`[api] token_file` あり。`POST /projects` で「Pluvio の新テーマ」
  案件を作り（秘書の最初の返事が自動で入る＝対話用タスク 1 件目）、`POST /org/coding-poc/messages` で
  もう 1 件（対話用タスク 2 件目）、`POST /tasks` で `research-survey`/`coding-poc` 宛ての普通のタスク 2 件を
  作って承認した（Approval needed の子タスクが自動でできるので、この案件のタスクは合計 6 件: 対話用 2 件 +
  普通 2 件 + その承認子 2 件）。Playwright（light / dark、Chromium）で確認したこと:
  - `/projects/:id`: 「仕事の木」の見出しの件数が **4**（対話用 2 件を除いた数）で、木にも
    「対話に…」のノードは描かれない。下の「担当に話す」一覧も 4 行（対話用の 2 件を含まない）。
  - `/tasks?q=対話`: 既定（「対話用も表示」オフ）では 0 件（「対話用タスクしかありません。上の
    「対話用も表示」を付けてください。」の案内）。チェックを付けて絞り込み直すと、対話用タスク 2 件
    （`対話: 状況を教えてください。` / `対話: Pluvio を基盤に用いた…`）が一覧に出た。
  - `/org?selected=coding-poc`: 「抱えている仕事（未終了）0 / 担当した仕事（累計）2」（対話用タスクを
    含めれば 3 になるところ、正しく 2）。「抱えているタスク」一覧も対話用タスクを含まない 2 件で、
    案件名の列は無い（トレードオフどおり）。
  - `/org/secretary?project=<id>`: 画面を**開き直した後**（そのセッションで送った発言ではない）の秘書の
    最初の返事に「この返事を作った run（裏方）」のリンクが出て、`href` が `/tasks/<対話用タスクの id>`
    （R4 が「送った直後だけ」の制約を解消したことの直接確認）。
  - light / dark 両方でスクリーンショットを確認。コンソールエラーは 0 件（dev モードの vite HMR
    WebSocket の CSP 警告と React の hydration mismatch 警告のみで、いずれも `pnpm dev`（HMR 有効）
    特有のもの。`pnpm build && pnpm start` の本番相当では出ない経路）。
  - `GET /approvals` の 2 回呼びは、この案件のシナリオでは認可の要求が発生しなかったため実機では
    確認できていない（unit テストで `pending=true`/`pending=false` の 2 リクエストが飛ぶことを確認済み）。

### 判断したこと（ADR は起こしていない。GUI の中の話）

- **組織ノード詳細の「抱えているタスク」一覧から案件名の列を落とした**（`TaskSummary` に `project_id` が
  無いため。N+1 をやめる代わりのトレードオフ。taskd-requests.md R3 に明記した）。
- **`/tasks` の「対話用も表示」はクライアント側だけの絞り込み**（`GET /tasks` にクエリを送らない）。
  taskd に `?conversation=` のような絞り込みは無く、`TaskSummary.conversation` は一覧の応答に既に
  含まれているので、GUI 側で弾くだけで済む（`filterReportsByKind` 等、既存の「ドキュメント化された
  フィールド値で GUI 側が分ける」パターンと同じ）。
- **`/tasks` の総件数・status 別件数（`counts_by_status`）はそのまま**（対話用を除いた表示件数と
  一致しないことがある）。taskd から返る値をそのまま出す既存方針（GUI 側では再計算しない）を優先した。

### 未解決事項

- G13e-U1: `TaskSummary` に `project_id` が無いため、組織ノード詳細の「抱えているタスク」一覧は
  案件名を出せない（G13a-U1 の裏返し。taskd 側に `project_id` を足すかは taskd-requests.md R3 の
  対応済みメモに記録した程度に留めた）。
- G13e-U2: `/tasks` の「N 件」表示は対話用を含む全件のままで、既定表示（対話用を除く）の見た目の件数と
  ずれることがある（上記「判断したこと」参照）。
- G13e-U3: `GET /approvals` の 2 回呼びへの回帰は unit テストのみで、実機では認可の要求を再現できず
  未確認（fake ワーカーが `question` を返すよう仕込めば再現できるが、今回は他の受け入れ条件を優先した）。
- G13e-U4: DOM を描画する unit テストは無い（G10-U1/G13d-U2 と同じ）。今回の変更（トグル、リンクの
  出し分け）も純粋関数のテストと Playwright の目視でのみ確認している。

## Phase G13f-1 — GUI 監査の対応（2026-09-17）

SPEC §4 に照らした GUI 監査（実機操作あり）で「6 画面は揃ったが、SPEC の仕事感の芯が出ていない」と
判定された。その指摘のうち **今の API で直せるもの（G13f-1）** と、**taskd 側 Phase 29 の追加 API に
依存するもの（G13f-2: H3 / M4 / 裏方の印 / H2）** を、同じ worktree で続けて実装した
（別 worktree だと `gui/**` の衝突が避けられないため、コーディネータの指示でここに畳んだ）。
着手時に `git merge --ff-only main`（`4cefc9c`）、G13f-2 の前に `git merge --no-edit main`（`7ee512d` = Phase 29）。

### 指摘ごとの対応（G13f-1）

1. **H1 操作の失敗が 0.3 秒で消える** — 変更系のフォームをすべて **fetcher 方式**に寄せた
   （`inbox.tsx` / `projects.tsx` / `tasks.$id.tsx` / `tasks.new.tsx` / `plans.new.tsx` / `daemon.tsx`。
   `/org` `/approvals` `/reports` `/artifacts` は元から fetcher）。原因は `useTaskdStream` が `daemon`
   イベント（taskd は tick ごとに無条件で流す）で `revalidate()` し、ナビゲーション方式の `actionData` が
   捨てられること。fetcher の `data` は再検証では消えない。受信箱は**項目ごとに 1 つの fetcher**を持たせ、
   結果をその行に出す。回帰テスト: `test/unit/action-feedback.test.ts`（変更系フォームを持つ 11 ルートが
   `actionData` と `<Form method="post">` を使っていないことをソースで確かめる。コメントは除いて見る）と
   e2e の「操作の失敗は消えない」（422 の表示が 3 秒後も残る）。
2. **初期画面** — `/` は `routes/home.tsx` が `/org/secretary` へリダイレクト（302）。受信箱は
   `/inbox`（裏方の区画）に残した。
3. **仕事の木（H4）** — `app/lib/graph-layout.ts` の固定 180×44 をやめ、`nodeBox` が中身から幅
   （180〜320px）と高さを決める。タイトルは最大 2 行で、入り切らなければ末尾を `…`（`wrapLabelLines`、
   全角 1em / 半角 0.56em の近似）。まとめのタスクは G13f-1 では `role = "report-compressor"` で外し、
   **G13f-2 で `support` に置き換えた**。
4. **組織の木を「人」に見せる** — 各行は名前を先頭に太く、その下に一言（1 行に省略）、右端に小さく分野の id。
   英語の `kind` バッジは出さず、部・課の 1 文字だけ添える。左列を 26rem にして名前が省略されないようにした。
5. **言葉を SPEC に揃える** — `app/lib/labels.ts`（新規）に案件・途中目標・タスクの状態、役職の印、
   認可の決定の日本語を集め、業務 6 画面のラベル・見出し・状態値を日本語にした（`request`→「依頼」、
   `title`→「題名」、`node_id`→「誰に」、`rule`→「規則文」、`scope`→「『今後ずっと』の範囲」など）。
   人の呼び方は**担当**に統一（画面から「ノード」「人」を消した。help の説明文だけ「人（担当）」と一度言い換え）。
6. **報告（M3）** — 本文を `MarkdownViewer` で描き、展開した中に**案件へ／担当に話す／裏方のタスク／
   その案件の成果物**へのリンク（`ReportLinks`）。`bad_news` は行そのものを赤系の背景に。絞り込みカードの
   内情説明（「taskd に転送するのは…」）を消した。
7. **秘書の画面（M1）** — `loadConversation` が `GET /inbox` も読み、`conversationTrouble`（純粋関数）で
   その対話の裏方タスクが `attention`（`unroutable` / `failed` / `requeue_limit_near`）に出ていたら
   「返事を作れない状態です: <理由>」を出して待つのをやめる。10 分の上限に達したときも同じ形で知らせ、
   プロバイダの設定と裏方のタスクへ誘導する。
8. **認可（M5）** — `groupApprovals`（純粋関数）で同じ文面の未決要求を 1 枚にまとめ「N 件」を出す。
   答えは**まとめて全件に送る**（action が `id` を複数受け取り順に決める）。カードの高さを詰め、
   案内文は `<details>` に畳んだ。
9. **細かいもの** — help から「G13b」「Phase」の露出を消し、`app/components/Placeholder.tsx` を削除、
   `projects.$id.tsx` の空括弧（`HelpLink`）を解消、`vscode://file//tmp` の二重スラッシュを直し
   （`app/lib/artifacts.ts`）、「通知を有効にする」をナビから報告画面の中へ移した
   （`app/components/NotificationsEnable.tsx`）。
10. **裏方から戻れる（M2）** — `/tasks` の行に案件（リンク）・担当（リンク）・途中目標、`/tasks/:id` の
    ヒーローに同じ 3 つを出した。`TaskSummary` に `project_id` が無いので、`/tasks` の loader が
    `GET /projects` + 各案件の `GET /projects/{id}` を束ねて索引を作る（`app/lib/project-index.ts`。
    N+1 なので `docs/taskd-requests.md` R6 に「`TaskSummary` に入れば消せる」と記録した）。

### 指摘ごとの対応（G13f-2、taskd 側 Phase 29 に追従）

1. **H3 「この方針で進める」** — `POST /projects/{id}/plan`（§3.61、管理系、202 `{task_id}`）。案件詳細に
   「分解を秘書に頼む」カード（承認済み・進行中の途中目標から選ぶ or 指定しない、人のひとこと）。押すと
   「分解を秘書に頼みました（裏方のタスク）。仕事の木がこれから増えていきます」と出し、待たない。
   案件が「提案中」でも押せる（taskd が「進行中」にする）。
2. **M4 記憶を見せる** — `GET /org/{id}/memory?project=`（§3.62）。組織の担当詳細に「覚えていること
   （案件をまたぐ）」と、案件を選べば「この案件の引き出し」を Markdown で表示。編集はせず、
   `notes_path` / `project_path` を「直すならこのファイル」として出す。409 `memory_unavailable` は
   「記憶の置き場所が設定されていません」。
3. **裏方の印** — `TaskSummary.support` / `ProjectTaskView.support` を使う `isSupportTask` に統一し、
   仕事の木・組織の「抱えている仕事」・`/tasks` の既定表示から `support != null` を外した。
   `/tasks` のトグルは「裏方も表示」（`show_support=1`、`data-testid="tasks-show-support"`）。
   `role = "report-compressor"` の判定は消した。
4. **H2 受信箱の質問 → 認可へ** — `QuestionItem.approval_id` があれば受信箱の回答欄を出さず、
   `/approvals#approval-<id>` へのリンクにした（回答は認可画面に一本化）。認可のカードは
   まとめた 2 件目以降にも錨（`id="approval-<id>"`）を置く。
5. **e2e** — 「この方針で進める」→ 偽プランナー（`test/taskd/fixtures/org-worker.sh` が `kind = plan` で
   `assignee` 付きの子を 3 件返す）→ 仕事の木に担当付きで増える、を追加した。

### 受け入れ条件と証拠

- `pnpm lint`（biome、160 files）exit 0 / `pnpm typecheck` exit 0 / `pnpm test` **453 passed**（39 ファイル。
  G13e の 391 から +62。新規 `test/unit/labels.test.ts` / `action-feedback.test.ts` / `home.route.test.ts` /
  `project-index.test.ts` と、`graph-layout` / `work-tree` / `conversation` / `approvals` / `org` /
  `projects.detail` への追加）/ `pnpm build` exit 0。
- `pnpm gen:types` を 2 回実行して同一（`diff` ゼロ）。`bash ../scripts/sync-gui-docs.sh` 実行済み
  （`gui/docs/taskd-api-v1.md` を Phase 29 に更新）。
- **e2e**: `gui/e2e/g13.spec.ts` を**別ポート**で実行し **11 passed**。運用中の 7700 / 7710 には触っていない:
  `TASKD_API_LISTEN=127.0.0.1:17971 scripts/taskd.sh fixture org` →
  `TASKD_GUI_BIND=127.0.0.1:17905 TASKD_API_URL=http://127.0.0.1:17971 TASKD_API_LISTEN=127.0.0.1:17971
  TASKD_API_TOKEN_FILE=$(pwd)/.run/org/api.token pnpm exec playwright test e2e/g13.spec.ts`。
  中身: 秘書に投げる → 返事 → 案件（日本語の言葉・仕事の木・担当への導線）→ この方針で進める（担当付きの
  子が 3 件）→ 記憶 → 受信箱の質問から認可へ → 組織の木 → 報告 → 認可（同じ文面が「2 件」でまとまり、
  答えると履歴に移る）→ 成果物 → 失敗が消えない → 裏方から案件・担当へ戻れる。
  `playwright.config.ts` に「既定の 7700 / 7710 は運用中を掴むので必ず別ポートを環境変数で指定する」注意書きと
  `TASKD_API_URL` の上書きを入れた。既存の g0〜g9 は触っていない。
- **見た目の確認**: 使い捨ての taskd（fake アダプタ、`config/org.example.toml`）に対して Playwright で
  light / dark のスクリーンショット（秘書・組織・案件・報告・認可・成果物・一覧・受信箱）を撮り、
  SPEC §4 の言葉と突き合わせた。確認できたこと: 仕事の木のノードが日本語のタイトル + `[担当]` で
  はみ出さない／組織の木が名前・一言・分野の「人」の並びになっている／案件詳細に「この方針で進める」と
  分解された 3 件が担当付きで出る／報告が Markdown で読め、リンクが出る／認可が 1 枚にまとまる。
  撮影用の spec は使い捨てで、コミットには含めていない。

### 判断したこと（ADR は起こしていない。GUI の中の話）

- **`/` を秘書へのリダイレクトにした**（`routes/home.tsx`）。受信箱は `/inbox`。**既存の e2e
  g0 / g1 / g2 / g4 / g6 / g7 は `page.goto("/")` で受信箱を見る**ので、そのままでは失敗する
  （今回は「既存の g0〜g9 は触らない」指示のため直していない。下の未解決事項。**Phase G13g で追従済み**）。
- **`/tasks` の列**: ID と優先度と分野を落とし、案件・担当・途中目標を足した。役割の列は残した
  （g7 が `task-role` を見るため）。
- **`/tasks` の索引は N+1** のまま（上記 R6）。件数が増えたら taskd 側の `TaskSummary.project_id` に載せ替える。
- **認可の決定は「まとめた全件に同じ答えを送る」**。原子性は無い（受信箱の「この Plan の子を全部受け入れ」と
  同じ扱い）。最初の失敗でそこまでの結果を返す。
- **記憶は読み取りだけ**（taskd に書き込み API が無い。§3.62）。`notes_path` を出して人がファイルを直す。
- **仕事の木には秘書の「分解」タスク（`kind = plan`）は出る**（`support` が付かないため）。人が見て意味の
  ある仕事なのでそのままにした。

### 未解決事項

- ~~G13f-U1: 既存の e2e（g0/g1/g2/g4/g6/g7）が `/` に受信箱を期待している~~ **Phase G13g で解消**（下記）。
- G13f-U2: `TaskSummary` に `project_id` / `milestone_id` が無い（`docs/taskd-requests.md` R6）。
  `/tasks` の索引は案件数ぶんの `GET /projects/{id}` を束ねている。
- G13f-U3: DOM を描画する unit テストは無い（G10-U1 と同じ）。今回も純粋関数のテスト + ソースの静的検査
  （`action-feedback.test.ts`）+ Playwright の目視で確かめている。
- G13f-U4: 認可を答えた直後、その行が「決めたもの」に移るとカード内の成功表示（fetcher の flash）も
  一緒に消える（失敗のときはカードが残るので消えない）。履歴に決定と答えが出るので実害は無いと判断した。
- G13f-U5: `/tasks` の総件数・status 別件数は taskd の値のまま（裏方を除いた表示件数とずれることがある。
  G13e-U2 の引き継ぎ）。

### 提案（`docs/DESIGN.md` / `docs/taskd-api-v1.md` への変更提案。採否は人間）

- G13f-P1: `TaskSummary` に `project_id` / `milestone_id` を足してほしい（R6）。GUI の N+1 が消える。
- G13f-P2: `GET /inbox` の `attention` に「その担当・その案件」の手がかり（`assignee`）があると、
  秘書の画面での「返事を作れない状態」の判定が `messages` の `task_id` 突き合わせ無しで済む。

## Phase G13g — 既存 e2e の追従（2026-09-18）

G13f-1 で `/` を秘書（`/org/secretary`）へのリダイレクトにしたため、既存の `e2e/g0〜g9.spec.ts` のうち
`page.goto("/")` で受信箱を見る箇所が失敗する（G13f-U1）。また G0 の受け入れ条件 4「taskd 停止中でも `/` が
200」の契約の引き継ぎ先を決める必要があった。この 2 点への追従と、追従中に見つかった 2 件の既存バグ（G13g 発、
G13f-1 とは無関係）の修正を行った。

### やったこと

1. **`/` → `/inbox` への置き換え（受信箱の中身を検証している箇所だけ）**
   - `e2e/g1.spec.ts`（受け入れ条件 2）、`e2e/g2.spec.ts`（受け入れ条件 1）、`e2e/g4.spec.ts`（受け入れ条件 2b）、
     `e2e/g6.spec.ts`（受け入れ条件 2 の 2 番目のテスト、`inbox-empty-help` 導線）、`e2e/g7.spec.ts`（受け入れ条件 2）
     の `page.goto("/")` を `page.goto("/inbox")` に直した。検証内容（`approval-item` / `attention-item` /
     `inbox-empty-help` 等）はそのまま。
   - `e2e/g0.spec.ts` の「受け入れ条件 4（前半）: taskd の状態表示」（`/` の footer・health 表示）はそのまま
     `page.goto("/")` を使う（受信箱の中身は見ておらず、root のフッタ・バナーの有無だけを見るテストなので
     `/org/secretary` へリダイレクトされても root の表示は変わらない）。
2. **G0 受け入れ条件 4（後半）「taskd 停止中でも 200」の引き継ぎ先を `/org/secretary` にした**
   - `e2e/g0.spec.ts` の当該テストを `page.goto("/org/secretary")` に変更（`/` は 302 を経由するだけなので
     直接開いても同じ検証になる）。`taskd-banner` に「taskd に接続できません」が出て 200 で開き、
     例外ページにならないことをそのまま検証する。
   - **`routes/org.secretary.tsx` / `routes/org.$id.tsx` の loader を直した**: 従来は `TaskdUnavailable` も
     他の taskd エラーと同じく `Response` として投げ、ErrorBoundary で `TaskdBanner` を描いていた。この経路は
     React Router のドキュメント応答ステータスに投げた `Response` のステータス（503）がそのまま使われるため、
     taskd 停止中は 200 にならなかった。`isTaskdUnavailable` の場合だけ loader 内で捕まえ、空の
     `ConversationData`（`node: null, projects: [], messages: [], attention: []`）を返すようにした
     （`routes/inbox.tsx` の `loadInbox` と同じ作法。root が taskd の状態を独自に検査して既にバナーを出すので、
     ここでは「対話はまだ何も無い」状態を描くだけでよい。`Conversation` コンポーネントは `node`/`projects`/
     `messages` が空でもそのまま描画できる作り）。`TaskdError`（404 等）は従来どおり `Response` を投げて
     ErrorBoundary に任せる。
   - `e2e/g0.spec.ts` の `getWithHost`（受け入れ条件 5: Host 検査）は `/` を GET していたが、`/` が 302 に
     なったことで「正しい Host は 200」の判定が壊れる（302 は 200 ではない）ため、Host 検査だけなら
     どのパスでもよいので `/healthz` を GET するように変えた（Host 検査は root の middleware で全ルート共通）。
3. **`e2e/g0.spec.ts`・`e2e/g1.spec.ts`・`e2e/g2.spec.ts`・`e2e/g4.spec.ts`・`e2e/g7.spec.ts` を別ポートで
   動かせるようにした**（`e2e/g6.spec.ts` はもともとポートをハードコードしていない）。`playwright.config.ts`
   の既定（7700/7710）は運用中の GUI/taskd を掴むため、`TASKD_GUI_BIND` / `TASKD_API_URL` /
   `TASKD_API_LISTEN` を上書きできるよう、ハードコードされていた `127.0.0.1:7700` / `127.0.0.1:7710` を
   `process.env.*` 参照（既定値は従来どおり）に置き換えた。`scripts/taskd.sh` を直接呼ぶ `sh()` / `taskctl()`
   ヘルパーは `TASKD_API_LISTEN` を明示的に子プロセスへ渡す（`e2e/g13.spec.ts` と同じ作法）。
4. **既存バグ 1（G13f-1 とは無関係）: `/tasks` の「さらに読む」が確実にクラッシュしていた**
   — `app/routes/tasks.tsx` の `useFetcher<TaskList>()` は、`fetcher.load("/tasks?...")` がこの route 自身の
   loader を叩くことを見落としていた。この route の loader は監査 M2（G13f-2、`docs/taskd-requests.md` R6 の
   仮対応）で `TaskList` ではなく `TasksData`（`{ tasks, config, placements, assigneeNames }`）を返すように
   変わっていたが、fetcher 側は直していなかったため `page.items.map(...)` が `undefined.map` で毎回
   `TypeError` を投げ、画面がクラッシュして行が 0 件になっていた（Playwright で再現・確認。
   `fetcher.data.tasks.items` / `fetcher.data.tasks.next_cursor` に直した）。
5. **既存バグ 2（G13f-1 とは無関係）: `e2e/g1.spec.ts` の「さらに読む」テストが裏方タスクを数えていた**
   — 上のクラッシュを直した後、`limit=2` を最後まで押しても 11 件中 10 件しか集まらなかった。原因は
   `basic` フィクスチャの `approval`（`Human-B` の承認の子）が `TaskSummary.support = "approval"` を持ち、
   `/tasks` は既定でこれを隠す（ADR-0033 D8、G13f-2「裏方の印」）ため、画面には最大 10 件しか出ない仕様
   になっていたこと。テストは `/tasks?limit=500` の生の 11 件と比べていたので、`~/lib/work-tree` の
   `isSupportTask` で同じフィルタをかけてから期待値を作るように直した（`show_support=1` は付けずに開く
   画面と同じ条件で比べる）。
6. **既存の flaky（G13f-1 とは無関係）: `e2e/g2.spec.ts` 受け入れ条件 1 の `flash` 待ちが安定しない**
   — 承認直後、root の SSE 再検証（`daemon` イベント。tick_ms=200 で無条件に届く。G1-U1）により
   `/inbox` の loader が数百 ms 以内に再検証され、承認済みの項目が一覧から消える（その `<li>` に閉じている
   `fetcher`/`flash` も一緒に消える）。これは `/approvals` 画面で既に確認・容認されていた挙動と同じ
   （G13f-U4「認可を答えた直後…成功表示も一緒に消える。実害は無いと判断した」）。この環境では 3 回連続で
   再現し、`toHaveAttribute`/`toHaveText` の 5 秒待ちでも `flash` を一度も観測できなかった。**製品コードは
   変えず**、テストを POST の応答そのもの（`page.waitForResponse("/inbox.data")` が 200）で成否を確認する
   形に直した（以降の `approval_decided` イベント・note・`replay` 0 mismatches の検証はそのまま）。
7. **`e2e/g6.spec.ts` 受け入れ条件 3（`/help` 内リンク全 200）** — `/help` はロゴ（`app/root.tsx`）とナビの
   「秘書」の両方から `/org/secretary` を指す。`basic` フィクスチャには組織（`secretary` ノード）が無いため
   どちらも 404 になる。クロール対象から `/` と `/org/secretary` を除外した（他のリンクはそのまま全 200 を
   要求する）。

### 受け入れ条件と証拠

- `pnpm lint`（biome、160 files）exit 0 / `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0 /
  `pnpm test` **453 passed**（39 ファイル、G13f-2 から件数変わらず。今回はソースの `tasks.tsx` 修正のみで
  新規ロジックは追加していない）/ `pnpm build` exit 0 / `pnpm gen:types && git diff --exit-code
  app/taskd/types.ts` 差分ゼロ。
- **e2e（別ポート、運用中の 7700/7710 には触っていない）**:
  ```
  cd gui
  scripts/taskd.sh build
  TASKD_RUN_ROOT=/tmp/taskd-gui-e2e-g13g TASKD_GUI_BIND=127.0.0.1:17900 \
    TASKD_API_URL=http://127.0.0.1:17910 TASKD_API_LISTEN=127.0.0.1:17910 \
    pnpm exec playwright test e2e/g0.spec.ts e2e/g1.spec.ts e2e/g2.spec.ts \
      e2e/g4.spec.ts e2e/g6.spec.ts e2e/g7.spec.ts
  ```
  結果: **34 passed**、1 failed、3 did not run（下記「未解決事項」参照。実行前後で `curl 127.0.0.1:7700/healthz`
  ／`curl 127.0.0.1:7710/api/v1/health` がともに 200 のままであることを確認済み＝運用中のインスタンスには
  触れていない）。内訳:
  - `e2e/g0.spec.ts`: 5/5 pass（受け入れ条件 4 前半・後半、5、`/healthz`、CSP）。
  - `e2e/g1.spec.ts`: 8/8 pass（「さらに読む」を含む）。
  - `e2e/g2.spec.ts`: 8/8 pass（受け入れ条件 1「受信箱で Approval を note 付きで承認」を含む）。
  - `e2e/g4.spec.ts`: 5/5 pass。
  - `e2e/g6.spec.ts`: 5/5 pass（受け入れ条件 3「`/help` 内のリンクがすべて 200」を含む）。
  - `e2e/g7.spec.ts`: 7/11 pass、1 failed（`clusters` フィクスチャ、下記）、3 did not run（同じ
    `describe` 内の残りで `beforeAll` が失敗したため未実行）。`delegation` フィクスチャを使う後半の
    3 件は 3/3 pass。

### 未解決事項

- **G7 の `clusters` フィクスチャはこの環境で未実行のまま**（A3-U1・G7-U 系と同じ、既知の環境依存）。
  `scripts/taskd.sh fixture clusters` は `~/.ssh/config` の `taskd-localhost`（localhost への ssh 多重接続、
  `ControlMaster auto`）が張られていることを前提にしており（ADR-0010 D1）、このホストには無い
  （`ssh -O check taskd-localhost` → `No ControlPath specified`）。`ssh -MNf taskd-localhost` を先に張れる
  環境の人に実行を依頼する（G13g で新たに壊れたものではない。前回・前々回のフルランでも同じ理由で未実行）。
- G13g-U1: **G13f-2 由来の 2 件のバグ（上記「既存バグ 1・2」）は、`e2e/g0〜g9` を今回はじめて通しで
  実行し直したことで見つかった**。G13f-2 自身は e2e フルランを行わずに完了扱いにしていた（`e2e/g13.spec.ts`
  のみ実行）ため、`/tasks` の「さらに読む」がクラッシュする状態が本番ビルドに残っていた。今後、既存画面に
  影響する変更をした回は、たとえ新規 spec を書いても `g0〜g9` を含むフルランを一度は通すべき、という教訓として
  残す。
- G13g-U2: **`e2e/g2.spec.ts` 受け入れ条件 1 の `flash` 消失（上記「既存の flaky」）は、製品として直すかどうか
  判断が要る**。今回はテスト側で吸収したが、`/inbox` も `/approvals`（G13f-U4）と同じ「決めた直後にカードごと
  消えて成功表示が一瞬しか出ない」設計になっている。人に見える形で承認結果を一定時間見せたいなら、
  「直近で決めた項目」を一覧から即座に外さず、SSE 再検証の周期（tick_ms=200）より長く留める・フラッシュを
  グローバルな場所に出す、等の設計変更が要る。これは GUI 全体の一貫した振る舞いに関わる判断で、勝手に決めず
  ここに記録するだけにした（採否は人間 / 次フェーズ）。

### 提案（`docs/DESIGN.md` / `docs/taskd-api-v1.md` への変更提案。採否は人間）

- G13g-P1: 上記 G13g-U2 のとおり、「決めた直後に一覧から消えて成功表示が一瞬で消える」挙動
  （`/inbox` の承認・`/approvals` の認可の両方）をどう扱うか、方針を決めて ADR にしてほしい
  （据え置き・一定時間残す・グローバルフラッシュ、等の選択肢がある）。

## Phase G13h — やり直すと Go（実機の報告から。2026-09-18）

taskd 側 Phase 31（`docs/PROGRESS.md`）に対応する GUI 側の追従。実機で、失敗したタスクをやり直す手段が
GUI に無く、人が遠回り（古い draft を取り消して秘書に分解し直させる）をした報告から。

### 実装したこと

- `app/taskd/action-types.ts`: `RetryOutcome`（`{ok:true, taskId /* 元 */, result: RetryResult}` |
  `{ok:false, taskId, error}`）を追加。
- `app/taskd/actions.server.ts`: `applyRetry`（`POST /tasks/{id}/retry`。`accept` チェックボックスを
  `RetryBody.accept` に写す）と `retryData`（201 応答）を追加。`GateAction = Exclude<Action, "retry">` を
  導入し、`ACTIONS` / `readIntent` / `TransitionInput.intent` をこれに絞った（`Action` に `retry` が
  加わったことで `applyTransition` の `switch` が非網羅になった `tsc` のエラーを解消。`retry` は
  `readTransitionForm` を経由しない別経路であることの型上の裏付けにもなる）。
- `app/taskd/route-actions.server.ts`: `runRetryAction`（`applyRetry` を呼ぶだけ）を追加。
- `app/routes/tasks.$id.tsx`: `action` が `intent === "retry"` を先に判定して `runRetryAction` に分岐する
  （`runTaskAction`/`readTransitionForm` は `retry` を未知の intent として 400 で拒み続ける。二重にしない）。
  画面には `detail.actions.includes("retry")` のとき「やり直す」ボタン（`accept` チェックボックス付き、
  `data-testid="action-retry"`）を追加。専用の `retryFetcher` を持ち、`fetcher.data.ok` になったら
  `useNavigate` で新しいタスク（`result.task_id`）へ遷移する。
- `app/routes/inbox.tsx`: 「注意」区画の `failed` 項目（`AttentionRow`）に同じ「やり直す」を追加
  （`action={`/tasks/${item.task.id}`}` で `tasks.$id.tsx` の action に直接投げる。受信箱自身の
  `runInboxAction`（`task_id[]` を直列に処理する既存の一括処理）は `TransitionOutcome[]` 専用で
  `RetryOutcome` とは形が違うので広げていない）。
- `app/routes/projects.$id.tsx`: 「仕事の木」の一覧（`work-tree-assignees`）に、`draft` の行へ「Go”
  （`/tasks/{id}/approve`、`data-testid="work-tree-go"`）、`failed`/`cancelled` の行へ「やり直す」
  （`data-testid="work-tree-retry"`）を追加。一覧の絞り込みを「`assignee` がある」から「`assignee` がある
  **または** `draft`/`failed`/`cancelled`」に広げ、担当のいない actionable なタスクも拾えるようにした
  （`WorkTreeTaskRow` に切り出し、行ごとに `go`/`retry` 用の別 fetcher を持つ）。
- `app/components/Flash.tsx`: `RetryFlash`（成功時は新タスクへのリンクと `rewired` の一覧、失敗時は
  既存の `ErrorFlash`）を追加。
- `docs/taskd-api-v1.md`（`scripts/sync-gui-docs.sh`）と `app/taskd/types.ts`（`pnpm gen:types`）は
  taskd 側の変更をそのまま反映（`Action` に `"retry"`、`Event` に `retried`、`RetryBody`/`RetryResult`）。

### 判断したこと

- **「失敗ノードのクリック先」は結局 `/tasks/:id`**: 案件の仕事の木（`WorkTree`、React Flow の図）は
  ノードを押すと `/tasks/:id` へ遷移するだけの図なので、そこに直接ボタンを置くのではなく、遷移先の
  `/tasks/:id` に「やり直す」を置けば依頼の「失敗ノードのクリック先」は自然に満たされる。図そのものへの
  ボタン追加はしていない（React Flow のノード内 UI は複雑になるため。判断が必要というほどのことではないが、
  依頼文の「クリック先」という言い回しをそのまま実装した根拠として記録する）。
- **「案件画面の draft ノードに Go」は `work-tree-assignees` の行に直接置いた**（`/tasks/:id` への遷移を
  挟まない）。ここだけは依頼文が「案件画面の…に」と明示していたため。

### 受け入れ条件ごとの証拠

- `pnpm lint`（biome、161 files）exit 0。
- `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0（`GateAction` の導入で `applyTransition` の
  `switch` の非網羅エラーを解消したことを含む）。
- `pnpm test` **459 passed**（40 ファイル。Phase G13g の 453 から `test/unit/tasks.detail.retry.test.ts`
  の 6 件を追加）:
  - `runRetryAction が accept:false/true を正しく本文に写し、新タスク id と rewired を返す` の 2 件
  - `404 task_not_found` `409 invalid_transition`（`cannot be retried`）`401 unauthorized` を
    `ActionError` として返す（投げない）ことの 3 件
  - `runTaskAction は retry を未知の intent として拒み続ける`（ルートが分岐しなければ 400 になることの
    裏付け）の 1 件
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts` 差分ゼロ（生成物をコミット済み）。
- `pnpm build` exit 0。
- **e2e（別ポート、運用中の 7700/7710 には触れていない）**:
  ```
  cd gui
  scripts/taskd.sh build
  TASKD_RUN_ROOT=<隔離用の一時ディレクトリ> TASKD_API_LISTEN=127.0.0.1:18971 scripts/taskd.sh fixture org
  TASKD_RUN_ROOT=<同上> TASKD_GUI_BIND=127.0.0.1:18901 TASKD_API_URL=http://127.0.0.1:18971 \
    TASKD_API_LISTEN=127.0.0.1:18971 \
    TASKD_API_TOKEN_FILE=<同上>/org/api.token \
    pnpm exec playwright test e2e/g13.spec.ts
  ```
  結果: **12 passed**（既存 11 件 + 新規「失敗 → やり直す → Go → 動く（Phase 31）」）。新規テストは
  `test/taskd/fixtures/org-worker.sh` に `Fail-G13h` という題名の分岐を追加して実現した:
  1 回目の run は決定的に非リトライ失敗（`{"type":"error","retryable":false}`）してタスクが `failed` に
  なり、マーカーファイル（作業ディレクトリ直下）を残す。`POST /tasks/{id}/retry` は `workspace` を
  そのまま複製する決定（taskd 側 Phase 31）により、やり直した新タスクは**同じ作業ディレクトリ**を使うので、
  2 回目の run（Go の後）はマーカーを見つけて成功する（`done`）。実行前後で
  `curl 127.0.0.1:7700/healthz` / `curl 127.0.0.1:7710/api/v1/health` がともに 200 のままであることを
  確認済み（運用中のインスタンスには触れていない）。
  `e2e/g0〜g9` のフルランは今回は行っていない（G13g で教訓化した「既存画面に影響する変更をした回は
  フルランすべき」に当てはまるほどの変更ではない——今回触った 3 画面のうち `tasks.$id.tsx` の
  既存ボタン（approve/reject/answer/cancel）のマークアップ・ロジックは変更しておらず、`retry` の枝を
  追加しただけであることをコード上でも確認している）。

### 未解決事項

なし。

### 提案

なし。

## Phase G13i — Discord 通知の区画（2026-09-18）

taskd 側の Phase 39（ADR-0037: 人の判断が要るときだけ Discord に知らせる）への追従。「報告」画面
（`/reports`）の通知の節に Discord の区画を足した（ブラウザ通知の節は変更していない）。GUI 側の新しい
設計判断は 1 点（後述、対象へのリンクが引けない `milestone_ready` の扱い）。

### 成果物

- `pnpm gen:types` 再生成（`NotifyView` / `NotifyRecent` / `NotifyTestResult`）。2 回実行して差分ゼロ。
- `app/lib/notify.ts`（新規、純粋関数。DOM を描画する unit テストが無い件（G10-U1）を踏まえ、判断・計算は
  すべてここに集約）: `notifyKindLabel`（5 種を SPEC の言葉で）、`notifyResultLabel`（Phase 39 の判断 2:
  `error === "discord webhook is not configured"` を専用の文言「未設定のため送っていません」に畳む）、
  `notifyResultTone`、`notifyTargetHref`（`key` から画面内リンクを組む。後述の判断参照）。
- `app/taskd/notify-admin.server.ts`（新規）: `sendNotifyTest`（`POST /notify/test`。管理系、
  `reports-admin.server.ts` と同じ作り。台帳には残らない）。
- `app/taskd/action-types.ts` に `NotifyTestOutcome`、`app/components/Flash.tsx` に `NotifyTestFlash`
  （action レベルの失敗は既存の `ErrorFlash`、成功時は `result.ok` を見て届いた／届かなかったを出し分ける）。
- `app/routes/reports.tsx`: `loadReports` が `GET /notify` も束ねる（`ReportsData.notify` /
  `.notifyError`）。`GET /secrets`（`app/routes/accounts.tsx`）と同じ理由で、`loadReports` はテストから
  loader を介さず直接呼ばれるため `actions.server.ts` の `toActionError` を使えず、`notifyViewError` として
  同じ変換をここに複製した。`action` に `notify_test` intent を追加（`ReportOpOutcome | NotifyTestOutcome`
  の合併型を返す）。画面には新規の `DiscordSection`（testid `discord-section`）を、既存のブラウザ通知の行の
  直後に追加: 未設定なら `/accounts#secrets` への導線（id `discord-webhook` を案内）、設定済みなら
  fingerprint と「テスト送信」ボタン（testid `discord-test`）、直近 10 件（testid `discord-recent-row`、
  `data-notification-kind`）を種のバッジ・対象へのリンク・時刻・結果で 1 行ずつ出す。
- `app/components/ReportsList.tsx`: `<li>` に `id={`report-${report.id}`}` を足した（Discord 区画の
  `bad_news` のリンク先 `/reports#report-{id}` が着地できるように。表示・testid は変更していない）。
- `app/routes/accounts.tsx`: 「API キー」節の `<section>` に `id="secrets"` を足した（`/accounts#secrets` の
  錨。既存の `aria-labelledby="secrets-heading"` はそのまま）。
- `app/routes/help.tsx`: 用語集に「Discord 通知」、「報告」の画面説明に Discord 区画への言及を追記。
- testid: `discord-section` / `discord-configured`（`data-configured`）/ `discord-test` /
  `discord-recent-row`（`data-notification-kind`）。

### 判断が必要だった点

**`milestone_ready` の「対象へのリンク」は作れない**: ADR-0037 D1 の `key` は途中目標 id だが、taskd の
API に途中目標単体を引く経路（`GET /milestones/{id}` 相当）が無く、`Milestone.project_id` を知るには
「案件を全件 GET → 各案件の詳細を GET → 該当の途中目標を探す」という N+1 の総当たりが要る
（`/org` の `countWorkload` が仕事の数を数えるのに使っている手と同種だが、直近 10 件の表示のためだけに
案件を総当たりするのは重いと判断し、やらなかった）。GUI 側では `notifyTargetHref` が `milestone_ready` に
`null` を返し、キー（id）をリンクにせずただのテキストで出す。**taskd 側への提案**: `NotifyRecent` に
`milestone_ready` のときだけ `project_id` を足す（他の 4 種は `key` 自体がそのままリンク可能な id なので
対称性は崩れるが、`milestone_ready` だけ特別扱いする方が N+1 より安い）。ADR は起こしていない
（GUI 単体の実装詳細と判断した。`docs/gui/api.md` への提案として次節に記録）。

### 実機での見た目・実際の送信の確認

使い捨ての taskd（`target/debug/taskd`、`[secrets] dir = "secrets"` + `[notify] discord_webhook_secret =
"discord-webhook"` / `interval_secs = 2` / `gui_base_url`、`[api] token_file`）を 127.0.0.1:18098 に、
GUI dev サーバを 127.0.0.1:18097（`TASKD_API_URL=http://127.0.0.1:18098`）に、偽の Discord webhook
（Node の `http` サーバ、127.0.0.1:18099、受けた POST 本文をファイルに記録して 204 を返すだけ）に
それぞれ起動した。**運用中の 7710 / 7700 には触っていない**（前後で `curl 127.0.0.1:7710/api/v1/health` /
`curl 127.0.0.1:7700/` が生きていることを確認済み）。Playwright（light/dark）で確認したこと:

1. **未設定**（`secrets/discord-webhook` が無い状態）: `GET /notify` は `{"configured":false,...}`。画面は
   「未設定です。アカウント → API キーに id discord-webhook で Webhook URL を登録してください」を出す
   （light/dark とも表示崩れなし）。
2. **設定済み**（`secrets/discord-webhook` に偽 URL `http://127.0.0.1:18099/webhook` を書いた状態。
   taskd は毎回ファイルを読み直すので再起動不要）: `GET /notify` が `configured:true` と fingerprint を返し、
   画面は「設定済み（fingerprint 025e5572）」＋「テスト送信」ボタンを出す。
3. **テスト送信**: 「テスト送信」を押すと `POST /notify/test` → taskd が実際に偽 webhook へ 1 通 POST
   （`{"content":"taskd のテスト送信です。…","username":"taskd"}`）し、偽サーバのログに記録された
   （= 本当に届いた）。画面には `flash-notify-test`「テスト送信: 届きました（the test message was
   delivered）」が出た。台帳（`GET /notify` の `recent`）には残らないことも確認（ADR-0037 D4 どおり）。
4. **直近の送信**: `notifications` 表に 5 種それぞれ 1 行ずつ（`milestone_ready`/`approval_pending`/
   `question_blocked` は `ok:true`、`bad_news` は `ok:false, error:"http status 404"`、`secretary_reply`
   は `ok:false, error:"discord webhook is not configured"`）を仕込んで表示を確認: 5 種とも SPEC の言葉の
   バッジ、`milestone_ready` 以外はリンク付き（`question_blocked` の行は挿入直後に taskd の tick が
   `ok:null` → 実際に偽 webhook へ送って `ok:true` に変わり、偽サーバのログにも記録された。表示だけでなく
   taskd の再送ロジックそのものが生きて動くことも確認できた）、結果の文言（送れた／失敗（理由）／
   未設定のため送っていません）がそれぞれ正しく出た。
5. 確認後、使い捨ての taskd・GUI dev サーバ・偽 webhook サーバはすべて停止し、リポジトリに残した一時ファイル
   （`.g13i-*.mjs`）は削除済み。使い捨てのデータ（`/tmp` 配下）はリポジトリの外。

### 受け入れ条件ごとの証拠

- `pnpm lint`（biome、164 files）exit 0。
- `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0。
- `pnpm test` **480 passed**（41 ファイル。G13h の 459 から新規 `test/unit/notify.test.ts`
  12 件 + `test/unit/reports.test.ts` に 9 件を追加）:
  - `notify.test.ts`: `notifyKindLabel`（5 種＋未知の kind）、`notifyResultLabel`（送れた／未設定のため
    送っていません／失敗（理由）／失敗／送信待ち）、`notifyResultTone`、`notifyTargetHref`
    （4 種はリンク、`milestone_ready` は `null`、key の URI エンコード）
  - `reports.test.ts`: `loadReports` が `GET /notify` を束ねる（成功・未設定・失敗時のフォールバック）、
    `sendNotifyTest`（200 `ok:true`/`ok:false`、401 `unauthorized`、409 `notify_unavailable`）
- `pnpm build` exit 0。
- `pnpm gen:types` を 2 回実行して差分ゼロ。`scripts/sync-gui-docs.sh --check` up to date
  （`docs/gui/api.md` §3.64〜3.65 の同期）。
- 実機での見た目・実際の送信の確認は上記のとおり（light/dark、5 種の表示、テスト送信の実配達、
  taskd 自身の再送ロジックの動作）。
- Phase 40 追従（2026-09-18）: taskd 側 Phase 40 で `GET /notify` の `recent[]` に `project_id`
  （省略可。`milestone_ready` はその途中目標の案件、`secretary_reply` は案件自身）が増えたのに追従し、
  `notifyTargetHref` が `project_id` があれば `milestone_ready` も `/projects/<project_id>` へリンクするように
  した（`secretary_reply` も `project_id` があればそちらを優先）。`bash ../scripts/sync-gui-docs.sh` と
  `pnpm gen:types` を各 2 回実行して差分ゼロ（2 回目は無変化）。`test/unit/notify.test.ts` に 2 件追加
  （`milestone_ready` が `project_id` ありでリンク、`secretary_reply` が `project_id` を優先）。
  `pnpm lint`/`typecheck`/`test`（482 件 pass）/`build` すべて exit 0。G13i-P1 を解消。

### 未解決事項

- G13i-U1: e2e（`pnpm e2e`）は運用中の taskd / GUI（7700/7710）と衝突するため未実行。確認は unit テストと
  使い捨て環境での Playwright スクリーンショット・実配達確認で行った（既存フェーズと同じ扱い）。
- G13i-U2: `milestone_ready` の対象へのリンクは、taskd が `project_id` を返さない古い記録（または
  Phase 40 以前の taskd）のときは `null` のまま（Phase 40 追従参照）。
- G13i-U3: DOM を描画する unit テストが無い件（G10-U1）は未解決のまま。`DiscordSection` の描画・状態分岐は
  実機の Playwright とスタブ検証（unit テストの `notify.test.ts`/`reports.test.ts`）でのみ確認している。

### 提案（`docs/gui/api.md` への変更提案。採否は人間）

- G13i-P1: 解消（Phase 40 追従、上記参照）。`GET /notify` の `recent[]` に `project_id` が増えたことで、
  `milestone_ready` も対象の案件へリンクできるようになった。

## Phase G13j — 途中目標のレビューカード（2026-09-18）

taskd 側 Phase 41（ADR-0038: 途中目標の判定を「結果の報告 → 次の提案 → ok / 議論 / ng」の対話にする）
への追従。案件画面の途中目標カードに、秘書のレビューの返事・次の提案・3 ボタン（ok / 議論 / ng）＋
自由記述欄を追加した。

### 実装したこと

- `pnpm gen:types`（2 回、差分ゼロ）: `MilestoneView`（`Milestone` を平らにした上に `review?` /
  `proposal?`）、`MilestoneReviewView`、`MilestoneDecideBody`、`MilestoneDecided` を追加。
  `ProjectDetail.milestones` の型が `Milestone[]` → `MilestoneView[]` に変わった。
  `bash scripts/sync-gui-docs.sh`（`docs/taskd-api-v1.md` §3.47・3.63 を反映）。
- `app/lib/milestone-review.ts`（新規、純粋関数）: `milestoneWorkTasks` / `milestoneIsStalled`
  （taskd 側 `crates/taskd/src/milestone_review.rs::ready_milestones` と同じ条件 — 裏方を除くその
  途中目標のタスクに ready/running/reviewing/blocked が 0 件・done が 1 件以上 — を GUI 側でも判定し、
  「秘書が結果をまとめています…」の表示に使う）、`milestoneDecisionNoteRequired` /
  `milestoneDecisionValid`（`discuss`/`ng` は自由記述必須）。
- `app/taskd/projects-admin.server.ts`: `decideMilestone`（`POST /milestones/{id}/decide`。フォームの
  `decision`/`note` をそのまま送るだけ。GUI は 3 値を解釈しない）。
- `app/taskd/action-types.ts`: `ProjectOpOutcome` に `milestone_decide`（`decided: MilestoneDecided`）を追加。
- `app/components/Flash.tsx`: `ProjectActionFlash` に `milestone_decide` の分岐（`ok`/`discuss`/`ng` ごとの
  文言、`ok` で `plan_task_id` があれば裏方のタスクへのリンク）。
- `app/routes/projects.$id.tsx`:
  - 途中目標カードに `MilestoneReviewPanel`（新規のローカルコンポーネント）を追加。`m.status` が
    `reached`/`redesigned` ならバッジのみ（`review` が残っていてもパネルは出さない。判定済みのものに
    誤って ok/議論/ng を押させないため）。それ以外で `m.review` があればパネル（秘書のまとめを
    `MarkdownViewer` で描画、`m.proposal` があれば次の途中目標の題名・説明、note 欄、3 ボタン）。
    `review` が無くまだ `milestoneIsStalled(tasks, m.id)` なら「秘書が結果をまとめています…」。
  - `MilestoneReviewPanel` は途中目標ごとに専用の `useFetcher`（`WorkTreeTaskRow` と同じ考え方）を持ち、
    `discuss`/`ng` を押す前に `milestoneDecisionValid` で note を確認（空なら送らず `aria-invalid` で
    赤くし、送信を止める。JS 無効時は taskd 側の 422 がそのまま出る）。`discuss` が通ったら
    `useNavigate` で `/org/secretary?project=<id>&waiting=1` へ遷移し、既存の「考え中」（`~/lib/conversation.ts`
    の `replyArrived`）にそのまま乗る。
  - 既存の直接状態変更（`milestone_status` の select + submit）は `<details>`（`状態を直接変える（裏方）`）に
    畳んだ（消していない。誤って押さないように）。
- `app/routes/help.tsx`: 「途中目標（milestone）」の説明に ok / 議論 / ng の判定と、直接変更が裏方に
  畳んであることを追記。
- testid: `milestone-review` / `milestone-review-text` / `milestone-proposal` / `milestone-decide-note` /
  `milestone-decide-ok` / `milestone-decide-discuss` / `milestone-decide-ng`（依頼どおり）。加えて
  `milestone-review-pending`（止まっているが返事がまだ）と `milestone-status-details`（畳んだ既存フォーム）。

### 見つけて直したこと（taskd 側の変更に GUI 側の fixture が追従していなかった）

- `gui/test/taskd/org.toml.tmpl`: taskd 側コミット `08267d9`（研究部を PaperQA2 / LDR で分割、
  `config/org.example.toml` に `research-web`（`genre = "web-research"`）を追加）に、GUI 側の e2e 用
  `[[genres]]` が追従しておらず、`scripts/taskd.sh fixture org && scripts/taskd.sh start org` が
  `invalid config: [[org]] research-web: genre "web-research" is not defined in [[genres]]` で起動不能
  になっていた（本 Phase の実機確認で発覚。`e2e/g13.spec.ts` もこの fixture を使うため、taskd 側の
  マージ以降は同じ理由で壊れていたはず）。`[[genres]] id = "web-research"`（`literature-reader` を
  借りるだけの最小定義。この課へタスクを流す e2e は今のところ無い）を足して解消。GUI 側の
  ファイル（`gui/**`）で直せる範囲だったため、本 Phase のコミットに含めた。

### 実機での見た目・実際の確認

使い捨ての taskd（`scripts/taskd.sh fixture org` を隔離した `TASKD_RUN_ROOT`・`TASKD_API_LISTEN
127.0.0.1:18931` で用意し、コピーされた `fake-worker.sh` にだけ「途中目標のレビュー対話
（タイトルに『途中目標』を含む）」の分岐を追加して `milestone_proposal`（結果ファイル
`artifacts/result.json`）を宣言するようにした。共有される `test/taskd/fixtures/org-worker.sh` 本体は
変更していない）を、GUI dev サーバ（`127.0.0.1:18901`、`TASKD_API_URL=http://127.0.0.1:18931`）と
組み合わせて確認した。**運用中の 7710 / 7700 には触っていない**（前後で
`curl 127.0.0.1:7710/api/v1/health` / `curl 127.0.0.1:7700/` の生存を確認済み）。

1. `POST /projects` → `POST /projects/{id}/milestones`（`status=in_progress`）→
   `POST /tasks`（`project_id`/`milestone_id`/`assignee=research-survey`）→ `approve` の順で作り、
   taskd 自身の tick（`tick_ms=200`）に「done → `ready_milestones` → レビュー対話を起こす →
   dispatch → done（`milestone_proposal` 付き）」を実行させた（**messages と milestones は SQL では
   なく、taskd の本来の決定的なロジックを実機で走らせて作った**。依頼文の「API / SQL で仕込む」の
   うち、より実機に近い経路を選んだ）。`GET /projects/{id}` で `milestones[0].review` と `.proposal`
   が実際に付くことを確認。
2. Playwright（Chromium）で `/projects/<id>` を light/dark で撮影（`docs/PROGRESS.md` に添付できないため
   本文に記載): 秘書のまとめ（Markdown 描画）・次の途中目標の提案・note 欄・ok/議論/ng の 3 ボタンが
   意図どおりの見た目で表示され、崩れは無かった。
3. `ng` を空の note で押す → 送信されず、note 欄が赤く（`aria-invalid=true`）なることを確認（クライアント
   側の抑止が効いている）。
4. `ok` を押す → `flash-milestone-decide`「達成にして、次の途中目標を承認し、分解を秘書に頼みました
   （裏方のタスク）」が出て、対象の途中目標のバッジが「達成」に、次の途中目標が「進行中」になり、
   仕事の木に分解された 3 件の draft タスクが増えることを確認（`plan_task_id` へのリンクも機能）。
   **ここで `reached` になった途中目標に `review` が残ったままレビューパネルとボタンが出続けるバグを
   発見**（`m.review` の有無だけで出し分けていたため）。`m.status` が `reached`/`redesigned` なら
   パネルを出さないよう修正し、再度スクリーンショットで確認済み（バッジのみになった）。
5. 別の途中目標（`ok` の分解 run 自身が「裏方の done 1 件・active 0 件」に見えて、taskd がもう一度
   レビュー対話を起こした—plan タスクの子が全員 `draft` のまま Go 待ちのときに taskd 自身がその途中目標を
   「止まっている」と判定する、という taskd 側の挙動。GUI のバグではないので直していない）に対して
   `discuss` を note 付きで押す → `/org/secretary?project=<id>&waiting=1` へ遷移し、その対話に
   「途中目標『…』の判定について相談です。もう少し候補を増やしてほしい」が `role=user` で入り、
   秘書の返事（fixture の応答）が続くことを確認（既存の「考え中」の仕組みにそのまま乗った）。
6. 確認後、使い捨ての taskd・GUI dev サーバは停止し、`.run`（gitignore 対象のシンボリックリンク）と
   一時データ（`/tmp` 配下）は削除済み。verify 用の一時スクリプト（`.g13j-verify*.mjs`）もリポジトリに
   残していない。

### 受け入れ条件ごとの証拠

- `pnpm lint`（biome、166 files）exit 0。
- `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0。
- `pnpm test` **498 passed**（42 ファイル。G13i の 482 から `test/unit/milestone-review.test.ts` 8 件 +
  `test/unit/projects.detail.test.ts` に 8 件を追加）:
  - `milestone-review.test.ts`: `milestoneIsStalled`（done とその他の状態の組み合わせ、裏方を除く、
    他の途中目標のタスクを数えない）6 件、`milestoneDecisionNoteRequired`/`milestoneDecisionValid`
    （ok は任意、discuss/ng は空・空白だけで無効）2 件
  - `projects.detail.test.ts`: `loadProjectDetail` が `milestones[].review`/`.proposal` をそのまま通す
    1 件、`decideMilestone` の `ok`/`discuss`/`ng` の本文 3 件、422（discuss で note 空）/401/404/409
    （`milestone_reached`）の伝播 4 件（計 8 件）
- `pnpm build` exit 0。
- `pnpm gen:types` を 2 回実行して差分ゼロ。`bash scripts/sync-gui-docs.sh --check` up to date。
- 実機での見た目・実際の確認は上記のとおり（light/dark、ok/discuss/ng 3 種、note 必須の抑止、
  `reached` になったカードがボタンを失うことを含む）。

### 未解決事項

- G13j-U1: e2e（`pnpm e2e`）は運用中の taskd / GUI（7700/7710）と衝突するため未実行（既存フェーズと
  同じ扱い）。確認は unit テストと使い捨て環境での Playwright 操作・スクリーンショットで行った。
- G13j-U2: taskd 側の挙動として、`ok` で分解した直後の plan タスク（子が全員 draft）自身が
  「done 1 件・active 0 件」に見え、その途中目標のレビュー対話がもう一度起きることを実機で確認した
  （上記 5）。GUI 側はこの状態も正しくカードに出せている（`review`/`proposal` が更新されればそのまま
  表示する）ので直していないが、taskd 側で「意図した動作か」を確認した方がよいかもしれない
  （taskd 側のバックログとして記録するだけに留める。GUI からは判断しない）。
- G13j-U3: DOM を描画する unit テスト（G10-U1 / G13i-U3 と同じ制約）は今回も無い。`MilestoneReviewPanel`
  の描画・分岐は実機の Playwright でのみ確認している。

### 提案（`docs/gui/api.md` への変更提案。採否は人間）

なし。

## Phase G13k — 案件の作業場所（2026-09-18）

taskd 側 Phase 43（ADR-0039「案件が作業場所（コードのある場所）を持ち、計画・委譲の子がそれを継ぐ」）
への追従。本番で子タスクがワーカーに `ssh` させて人のリポジトリへ直接書いた事故（2026-09-18）を
受けたもので、案件に作業場所（`Local{path}` / `Remote{cluster, path}`）を持たせ、GUI から作成・編集・
消去できるようにした。

### 実装したこと

- `pnpm gen:types`（2 回、差分ゼロ）: `Project.workspace` / `ProjectCreateBody.workspace` /
  `ProjectPatchBody.workspace`（`status` も含めどちらも任意に）が増えた。`bash scripts/sync-gui-docs.sh`
  （`docs/taskd-api-v1.md` §3.46〜3.48 を反映）。
- `app/lib/workspace-form.ts`（新規、純粋関数）: `WorkspaceKind`（`undecided`/`local`/`remote`）、
  `workspaceKindOf`、`readWorkspaceFromForm`（`workspace_kind`/`workspace_path`/`workspace_cluster`
  から `WorkspaceSpec | null` を組む。「まだ決めない」は `null`）、`workspaceSummaryText`（`path` または
  `cluster:path` の 1 行）。GUI 側では検証しない: 空のパス・知らないクラスタもそのまま taskd に送る。
- `app/components/WorkspaceFields.tsx`（新規）: 「作業場所」の入力欄一式（手元／クラスタ／まだ決めない
  の切り替え、`GET /clusters` からのクラスタ選択、パス入力）。`/projects` の新規フォーム、秘書の
  「新しい案件として」（`Conversation.tsx`）、`/projects/:id` の編集カードが共有する。
  `allowUndecided`（既定 `true`。編集カードは `false` — 「まだ決めない」への切り替えは無く、消去は
  別ボタン）。422 の `workspace.cluster` はクラスタ欄の下に `FieldErrors` でそのまま出す。
- `app/taskd/action-types.ts`: `ProjectOpOutcome` に `project_workspace`（`project: Project` を返す。
  `project_status` と同じ形）を追加。
- `app/taskd/projects-admin.server.ts`: `readProjectCreateInput` が `workspace_kind` 等から
  `workspace` を組んで足す（「まだ決めない」ならキー自体を省略）。`patchProjectWorkspace`（新規、
  `PATCH /projects/{id}` に `{workspace}` を送るだけ。`null` を明示すれば消去）。
- `app/components/Flash.tsx`: `PROJECT_OP_LABEL` に `project_workspace: "作業場所を変更"`。
- `app/routes/projects.tsx`: `loadProjects` が `GET /clusters` も束ねて `clusters` を返す（落ちても
  一覧・作成フォームは出す）。新規フォームに `WorkspaceFields`（`allowUndecided` 既定のまま）を追加。
- `app/routes/projects.$id.tsx`: 「作業場所」カード（`依頼` の次、`途中目標` の前）。現在値
  （`workspaceSummaryText`。未設定なら `Alert tone="warning"`「未設定 — コードを扱う仕事は空の作業
  ディレクトリで走ります」）、編集フォーム（`WorkspaceFields allowUndecided={false}`、`intent =
  "project_workspace_save"`）、消去ボタン（別フォーム、`intent = "project_workspace_clear"` で
  `workspace: null` を送る）。`loadProjectDetail` が `GET /clusters` も束ねる。
- `app/components/Conversation.tsx`: 秘書の「新しい案件として」チェックが付いているときだけ
  `WorkspaceFields`（`idPrefix="conversation-workspace"`）を出す（`newProjectChecked` の state。
  案件を切り替えたら初期状態に戻す）。`app/lib/conversation.ts::ConversationData` /
  `app/taskd/conversation.server.ts::loadConversation` に `clusters`（`GET /clusters`。落ちても対話は
  出す）を追加。`startProjectFromMessage` が `workspace` 引数を取り、`runConversationAction` が
  `readWorkspaceFromForm` の結果を渡す。
- `app/lib/artifacts.ts::WorkspacePlace` / `workspacePlace`: ADR-0039 D3「編集は手元、検証はリモートで」
  に合わせ、`localCopyNote`（Remote かつ `workspace_dir` があるときだけ「手元の写し: `<workspace_dir>`」）
  を足した。`app/components/ArtifactsList.tsx`（`/artifacts` 横断一覧・`/projects/:id` の「成果物」節が
  共有）の `artifact-workspace` に、その案内文を添える（testid `artifact-workspace-local-copy`）。
  `app/routes/tasks.$id.tsx` の `task-workspace-note`（Remote のときだけ出る）も、`workspace_dir` が
  あれば「手元の写し: `<値>`（クラスタ側の元のパスは表示されません）」と具体的な値を出すよう改めた
  （従来は値の無い一般的な注意文だけだった）。
- `app/routes/help.tsx`: 用語集に「作業場所」（ADR-0039、3 択・カードでの編集・消去・子タスクへの
  継承・実機の事故を要約）を追加。「案件」「秘書」の画面ごとの説明にも作業場所への言及を足した。
- testid: `project-workspace`（案件画面のカードの `section`）、`project-workspace-kind`、
  `project-workspace-path`、`project-workspace-cluster`、`project-workspace-save`、
  `project-workspace-clear`（依頼どおりの 6 つ。新規フォーム・秘書の対話フォームでも
  `project-workspace-kind`/`-path`/`-cluster` を共有）。加えて `project-workspace-unset`（未設定の警告）。

### 実機での見た目の確認

使い捨ての taskd（`TASKD_API_LISTEN=127.0.0.1:18971`。`scripts/taskd.sh build` → 手作りの
`taskd.toml`（既定テンプレートに `token_file = "api.token"` と `[[clusters]] id = "pegasus", auth =
"manual"` を足したもの）→ `scripts/taskd.sh start g13k-workspace`）と GUI dev サーバ
（`TASKD_API_URL=http://127.0.0.1:18971`、`TASKD_GUI_BIND=127.0.0.1:18901`、
`TASKD_API_TOKEN_FILE=.../api.token`。`pnpm dev`）を組み合わせ、**運用中の 7710 / 7700 には触っていない**
（前後で `curl 127.0.0.1:7710/api/v1/health` → 200、`curl 127.0.0.1:7700/` → 302、`ps aux` で
production の `node server.js` 2 プロセスと `taskd`（`/home/rmaeda/taskd/taskd.toml`）が動いたままである
ことを確認済み）。

1. `POST /projects`（管理系。トークン付き）で案件を作成 → `/projects` の一覧・新規フォームを
   Playwright（Chromium）で light/dark 撮影: 作業場所欄が「まだ決めない」の既定で表示、崩れなし。
2. その案件の `/projects/:id` を light/dark で撮影（3 状態）:
   - **未設定**: 「未設定 — コードを扱う仕事は空の作業ディレクトリで走ります」の警告カードと、
     既定 `手元` の編集フォームが表示。
   - `PATCH /projects/{id}` で `{"workspace":{"kind":"local","path":"~/workspace/rust/pluvio-poc"}}`
     を送った後（taskd が `$HOME` で展開して保存）: 現在値に展開後の絶対パス
     （`/home/rmaeda/workspace/rust/pluvio-poc`）が出て、編集フォームもその値で初期化されている。
   - 続けて `{"workspace":{"kind":"remote","cluster":"pegasus","path":"/work/NBB/rmaeda/workspace/rust/benchfs"}}`
     を送った後: 現在値が `pegasus:/work/NBB/rmaeda/workspace/rust/benchfs`、編集フォームが
     `クラスタ` / `pegasus` / 該当パスで初期化されている。
   - 3 状態とも light/dark で表示崩れなし。
3. `taskctl add --cluster pegasus --workspace /work/NBB/rmaeda/workspace/rust/benchfs` で Remote
   workspace のタスクを作り、`/tasks/:id` を撮影: 「cluster: pegasus 手元の写し:
   `/tmp/taskd-gui-run-rmaeda/g13k-workspace/workspaces/<task_id>`（クラスタ側の元のパスは表示されません）」
   が具体的な値付きで表示されることを確認（ADR-0039 D3）。
4. 確認後、使い捨ての taskd・GUI dev サーバは停止し、`.run/g13k-workspace`（`/tmp` 配下の実体）は
   削除済み。verify 用の一時スクリプトもリポジトリに残していない。

### 受け入れ条件ごとの証拠

- `pnpm lint`（biome、169 files）exit 0。
- `pnpm typecheck`（`react-router typegen && tsc -b`）exit 0。
- `pnpm test` **530 passed**（43 ファイル。G13j の 498 から +32）:
  - `test/unit/workspace-form.test.ts`（新規）10 件: `readWorkspaceFromForm`（undecided/local/remote/
    未検証の空値）5 件、`workspaceKindOf` 2 件、`workspaceSummaryText` 3 件
  - `test/unit/projects.test.ts` +13: `readProjectCreateInput` の workspace 3 種 3 件、
    `createProject` の workspace 本文 3 種 + 422（`workspace.cluster`）4 件、`patchProjectWorkspace`
    の local/remote/消去（`null`）/422 4 件、`loadProjects` の `GET /clusters` 通過・失敗時空扱い 2 件
  - `test/unit/projects.detail.test.ts` +2: `loadProjectDetail` が `project.workspace` を素通りし
    `GET /clusters` を選択肢として添える 1 件、`GET /clusters` が落ちても詳細は返す 1 件
  - `test/unit/conversation.test.ts` +6: `loadConversation` の `GET /clusters` 通過・失敗時空扱い
    2 件、`startProjectFromMessage` の workspace 付き・`null`・422 3 件、`runConversationAction` で
    作業場所欄が `POST /projects` の `workspace` になる 1 件
  - `test/unit/artifacts.test.ts` +1: `workspacePlace` の remote + `workspace_dir` で
    `localCopyNote` が付く 1 件（既存の local/remote 2 件は `localCopyNote: null` を足して更新）
- `pnpm build` exit 0。
- `pnpm gen:types` を 2 回実行して差分ゼロ。`bash scripts/sync-gui-docs.sh --check` up to date。
- 実機での見た目の確認は上記のとおり（3 状態 × light/dark、Remote タスクの「手元の写し」表示）。

### 未解決事項

- G13k-U1: e2e（`pnpm e2e`）は運用中の taskd / GUI（7700/7710）と衝突するため未実行（既存フェーズと
  同じ扱い）。確認は unit テストと使い捨て環境での Playwright 撮影で行った。
- G13k-U2: DOM を描画する unit テスト（G10-U1 以降と同じ制約）は今回も無い。`WorkspaceFields` /
  「作業場所」カードの分岐は実機の Playwright でのみ確認している。
- G13k-U3: 秘書の「新しい案件として」に添えた作業場所は、実機で `POST /projects` の本文に載ることを
  unit テスト（`runConversationAction`）で確認したが、Playwright での実機確認は `/projects` の新規
  フォームと `/projects/:id` の編集カードだけに絞った（秘書の対話画面はチェックボックスの表示切り替え
  を含み、対話 run 自体は taskd 側の fixture ワーカーが要るため、G13b-2/G13j までの確認範囲に揃えて
  今回は省いた）。
- G13k-U4: `/artifacts`・`/projects/:id` の「成果物」節（`ArtifactsList`）の「手元の写し」表示は
  unit テスト（`workspacePlace`）でのみ確認し、実機の Playwright 撮影は `/tasks/:id` の表示（同じ
  `workspace_dir` の値を使う）で代表させた（成果物の実データを使い捨て環境で 1 件登録するコストに対し、
  表示ロジックは共通の `workspacePlace` に集約されているため）。

### 提案（`docs/gui/api.md` への変更提案。採否は人間）

なし。

## Phase G14 — 「リリース」画面（ADR-0040 D6。2026-09-19）

- 完了日: 2026-09-19
- 目的: taskd Phase 48 で入った `GET /releases` / `POST /releases/{sha12}/promote`
  （`docs/taskd-api-v1.md` §3.66〜3.67）を画面にする。**昇格は人が押す**（ADR-0040 D5）。
  taskd 側の実装と同じコミットで入れた（この Phase は taskd Phase 48 と 1 対 1）。
- 変更したファイル:
  - `app/routes/releases.tsx`（新規）— `loadReleases`（`GET /releases` をそのまま返す。並び・
    `is_current` / `promoting` は taskd が計算済みなので再計算しない）、`action`（`release_promote` のみ）、
    画面、`ErrorBoundary`（`clusters.tsx` と同形）。
  - `app/lib/releases.ts`（新規）— 表示の判断を集めた純粋関数（DOM の unit テストが無い制約 G10-U1）。
  - `app/lib/labels.ts` — `instanceRoleLabel`（`active` / `standby` / `draining` / `verify` の日本語）。
  - `app/taskd/releases-admin.server.ts`（新規）、`app/taskd/action-types.ts`（`ReleasePromoteOutcome`）、
    `app/components/Flash.tsx`（`ReleasePromoteFlash`）。
  - `app/routes.ts`（`route("releases", ...)`）、`app/root.tsx`（裏方の最後に「リリース」）、
    `app/routes/help.tsx`（`SCREENS` に「リリース」）。
  - `app/taskd/types.ts` / `docs/taskd-api-v1.md` — `pnpm gen:types` と `scripts/sync-gui-docs.sh` で再生成
    （taskd Phase 47 の持ち越し U47-3 をここで解消。`Health` の `release`/`mode`/`role` もこれで入った）。
  - `test/unit/releases.test.ts`（新規）、`test/mock-taskd/fixtures.ts`（`releaseItem` / `defaultReleases` /
    `defaultReleasePromoteAccepted`）、`test/unit/action-feedback.test.ts` に `releases.tsx` を追加。
- 受け入れ条件ごとの証拠:
  - 条件: 一覧・検証状態・`current` が出て、検証済みで current でないものにだけ「昇格」が出る。
    実行: `pnpm test`（`test/unit/releases.test.ts`）— `releaseVerifyState` の 4 通り
    （未検証 / 検証済み（ライブ引き継ぎ）/ 検証済み（停止 → 起動）/ 検証に落ちました）と
    `promoteAvailability` の 5 通り（押せる / current / 昇格中 / 未検証・検証落ち / ファイルが読めない）。
  - 条件: 「昇格」は確認付きで、BFF の action が `POST /releases/{sha12}/promote` を呼ぶ。
    実行: 同テストの `promoteRelease` 4 件 — 202 のとき要求本文が `{}` で `/api/v1/releases/<sha>/promote`
    に飛ぶこと、404 / 409 / 401 が `{ok:false, error}` になること。画面側は `<details>` で一段隠したうえ、
    ボタンの `onClick` で `window.confirm`（`promoteConfirmText` は `live_ok` で文言が変わる）。
  - 条件: 202 / 409 の結果が SSE の再検証で消えない。
    実行: `pnpm test`（`test/unit/action-feedback.test.ts`）— `releases.tsx` に `actionData` と
    `<Form method="post">` が無い（行ごとの `useFetcher({key: "release-<sha12>"})` を使っている）。
  - 条件: 昇格中は `GET /releases` を 2 秒ごとに読み直し、`instances` で進行を出す。
    実行: 同テストの `handoffInFlight` / `handoffProgressText` — `draining` が居る / `active` が 2 つ /
    どれかが `promoting` のいずれかで真になり、文言に旧の「引き継ぎ中」と新の「稼働中」が出る。
    ポーリングは `HANDOFF_POLL_MS = 2000` で `revalidator.state === "idle"` のときだけ発火する
    （`root.tsx` の再接続ポーリングと同じ作り）。
  - 条件: フェーズのゲート。
    実行: `pnpm lint` exit 0（biome、174 ファイル）/ `pnpm typecheck` exit 0 /
    `pnpm test` exit 0（**45 ファイル、558 tests passed**）/ `pnpm build` exit 0。
    `pnpm gen:types` を流し直しても `app/taskd/types.ts` は変わらない（md5 一致＝差分ゼロ）。
- 未解決事項:
  - G14-U1: `pnpm e2e` は未実行（既定の 7700 / 7710 が運用中の GUI / taskd を掴むため。G13k-U1 と同じ）。
    別ポート（`TASKD_API_LISTEN=127.0.0.1:7713` / `TASKD_GUI_BIND=127.0.0.1:7703`）を与えて人が流すときに、
    `/releases` が 200 であることと `/help` のリンクを足すのがよい。
  - G14-U2: `promote.log` は API から読めない（taskd 側の意図。ADR-0040 D6）。画面にもログは出さず、
    進行は `instances` だけで見せている。
  - G14-U3: 「昇格」を押したあとの数秒、taskd が旧から新へ切り替わる窓では管理系 API が
    503 `standby` を返しうる（ADR-0040 D4）。`/releases` は読み取りなので影響を受けないが、
    他の画面（`/providers` の reload など）はその間だけ 503 になる。今回は特別扱いしていない。
- 提案（`docs/taskd-api-v1.md` への変更提案。採否は人間）:
  - G14-P1: `daemon_instances` の変化を SSE の `daemon` イベントに載せてほしい。載れば
    「リリース」画面の 2 秒ポーリング（この画面だけの特例）を消せる。

## Phase G16 — 案件のリポジトリとタスクの作業ツリー（ADR-0043 D1/D6、Phase 52。2026-09-19）

- 完了日: 2026-09-19
- 目的: taskd Phase 52（ADR-0043 A1）で入った 2 つを画面にする。
  (1) 案件が**リポジトリを複数持てる**ようになった（`docs/taskd-api-v1.md` §3.68〜3.71、
  `ProjectDetail.repos[]`、`POST /tasks` の `repos`）。`Project.workspace` は primary の `location` の
  写しなので、**従来の「作業場所」カードと `/projects` の単一 `workspace` フォームはそのまま動く**。
  (2) タスクの作業ツリーが GUI から読めるようになった（§3.72〜3.73、読み取り・トークン不要）。
- 決めたこと:
  - G16-D1: 「リポジトリ」節は `/projects/:id` の「作業場所」カードの**すぐ下**に置く。作業場所は
    primary の写しなので、同じものが 2 か所に出る形だが、**どちらも taskd が持つ値そのまま**で
    GUI 側の再計算はしない（`workspace` を変えれば primary が変わる、は taskd 側の規則）。
  - G16-D2: `WorkspaceFields` は触らず、兄弟の `RepoFields` を足す（`/projects` の既存の単体テストを
    壊さないため。ADR-0039 D1 の作業場所フォームは今までどおり）。純粋関数も
    `~/lib/workspace-form.ts` ではなく兄弟の `~/lib/repo-form.ts` に置く。
  - G16-D3: `RepoFields` は**入力欄を常に全部描く**（クラスタの選択は `hidden` で隠すだけ）。
    `/projects` の「追加のリポジトリ」はこの組を繰り返し、`form.getAll()` で列ごとに読むので、
    行によって欄が欠けると並びがずれるため。パスが空の行は送らない。
  - G16-D4: `/projects` の作成は **`POST /projects` → 行ごとに `POST /projects/{id}/repos`** の順
    （`POST /projects` は 1 つの `workspace` しか受けない。§3.46 / §3.69）。案件は作れたが
    リポジトリで 422 / 409 になったときは、案件へのリンクを添えて taskd の文言を出す
    （`ProjectCreateFailure.projectId`）。作り直させない。
  - G16-D5: リポジトリの操作（追加・変更・主にする・削除）の結果は**行ごとの `useFetcher`** に載せる
    （SSE の再検証で消えないため。監査 H1 / Phase G14 と同じ）。`ProjectOpOutcome` に
    `repo_create` / `repo_patch` / `repo_primary` / `repo_delete` を足した。
  - G16-D6: 作業ツリーの画面は**兄弟のルート `/tasks/:id/files`**（`/tasks/:id/runs/:runId` と同じ形）。
    中身は全部 `~/components/task-files.tsx`（自己完結の部品）に入れてあるので、ADR-0044 B1 の
    タブの殻ができたらそこへ 1 行で載せ替えられる。`/tasks/:id` に付けたのは導線のリンク 1 本だけ。
  - G16-D7: 作業ツリーの移動は全部 `<Link>`（`?repo=&path=&file=`）で、そのたびに loader が走る。
    クライアントから taskd を呼ぶコードは書かない（DESIGN §8.1）。一覧（`GET /tasks/{id}/tree`）が
    落ちたらページが出せないので例外のまま（`ErrorBoundary`）、選んだファイルの 403 / 404 だけは
    `fileError` として一覧を出したまま文言を見せる。
  - G16-D8: `binary` / `too_large` は**本文を出さず大きさだけ**出す（§3.73）。中身の推測はしない。
- 変更したファイル:
  - `app/lib/repo-form.ts`（新規）— 置き場所の 2 択（`RepoPlace`）、`repoLocationFrom` /
    `repoLocationText` / `repoPlaceOf` / `primaryRepo`。
  - `app/lib/task-files.ts`（新規）— `treeBreadcrumbs` / `parentPath` / `fileBody`（text / binary /
    too_large）/ `pickTreeFileViewer` / `isJsonPath` / `taskFilesHref`。
  - `app/lib/labels.ts` — `repoKindLabel` / `repoRunLabel` / `repoSyncLabel` / `PRIMARY_REPO_MARK` /
    `SET_PRIMARY_REPO_LABEL` / `REPO_KIND_AUTO_LABEL` / `treeEntryKindLabel` / `fileSizeLabel` /
    `binaryFileLabel` / `tooLargeFileLabel`。
  - `app/taskd/repos-admin.server.ts`（新規）— `listRepos` / `createRepo` / `patchRepo` /
    `setPrimaryRepo` / `deleteRepo` と読み手（`readRepoCreateBody` / `readRepoPatchBody` /
    `readExtraRepoCreateBodies`）。
  - `app/taskd/task-files.server.ts`（新規）— `loadTaskFiles` / `readTaskFilesQuery`。
  - `app/taskd/action-types.ts` — `ProjectOpOutcome` に 4 つの repo の op を追加。
  - `app/components/RepoFields.tsx`（新規）、`app/components/ProjectRepos.tsx`（新規）、
    `app/components/task-files.tsx`（新規）、`app/components/Flash.tsx`（`PROJECT_OP_LABEL` に 4 語）。
  - `app/routes/projects.$id.tsx` — 「リポジトリ」節と 4 つの intent。
  - `app/routes/projects.tsx` — 「追加のリポジトリ」（繰り返し行）と、201 後の `POST .../repos`。
  - `app/routes/tasks.$id.files.tsx`（新規）、`app/routes.ts`（`tasks/:id/files`）、
    `app/routes/tasks.$id.tsx`（「ファイル」への導線のリンク 1 本）。
  - `test/mock-taskd/fixtures.ts` — `projectRepo` / `defaultRepoList` / `treeView` / `treeFileView`。
  - `test/unit/repos-admin.test.ts`（新規、21 件）、`test/unit/projects.repos.test.ts`（新規、14 件）、
    `test/unit/task-files.test.ts`（新規、25 件）。
  - `app/taskd/types.ts` / `docs/taskd-api-v1.md` — taskd 側が Phase 52 で同期済み（`pnpm gen:types` を
    流し直しても変わらない）。
- 受け入れ条件ごとの証拠:
  - 条件: `/projects/:id` に「リポジトリ」節があり、一覧（名前・種類・場所・実行環境・既定のブランチ・
    主の印）と、追加・変更・主にする・削除ができる。
    実行: `pnpm test`（`test/unit/projects.repos.test.ts`）— `loadProjectDetail` が
    `ProjectDetail.repos` を**並べ替えず**そのまま通すこと（primary が先頭なのは taskd が決める）、
    `repos` が無い応答でも詳細が出ること、行に出す文言（`repoLocationText` が `cluster:path` /
    パスそのまま、`repoKindLabel` / `repoRunLabel` / `repoSyncLabel` が知らない値を素のまま返す）。
    ルート側は `case "repo_create":` 〜 `case "repo_delete":` の 4 つと `ProjectRepos` の描画をソースで確認。
  - 条件: フォームの読み手が `RepoCreateBody` / `RepoPatchBody` を仕様どおりに組む（空欄はキーごと送らない）。
    実行: `pnpm test`（`test/unit/repos-admin.test.ts`）— パスだけ書いたら `{location}` だけ、
    `kind = auto` はキーを送らない、`name` / `default_branch` / `run` の空欄もキーを送らない、
    remote は `{kind:"remote", cluster, path}`、編集は `name` / `location` / `default_branch` を必ず送り
    `default_branch` の空欄は `null`（消す）を明示、「主にする」は `{is_primary: true}` だけ。
  - 条件: taskd のエラー文言をそのまま出す（GUI 側で検証しない）。
    実行: 同テスト — 422 `validation`（`errors[].field = "name"` が `error.fields.name` に入る）、
    409 `repo_in_use`（`detail` そのまま、`conflict: true`）、404 `repo_not_found`、401 `unauthorized`。
  - 条件: `/projects` の従来の単一 `workspace` フォームが今までどおり動く。
    実行: `pnpm test` — 既存の `test/unit/projects.test.ts`（`readProjectCreateInput` の 4 件、
    `createProject` の workspace 本文 3 種）が無改変で通る。加えて `projects.repos.test.ts` で
    `WorkspaceFields` / `readProjectCreateInput` / `project-new-form` が残っていることを確認。
  - 条件: 「追加のリポジトリ」は行ごとに `POST /projects/{id}/repos` される。
    実行: `pnpm test`（`repos-admin.test.ts` の `readExtraRepoCreateBodies` 3 件）— 行が無ければ空、
    2 行を列ごとに突き合わせて `RepoCreateBody` を並べる、パスが空の行は送らない。
    ルート側は `readExtraRepoCreateBodies` / `createRepo` の呼び出しをソースで確認。
  - 条件: タスクの作業ツリーが読める（リポジトリの選択・パンくず・一覧・ファイルの本文）。
    実行: `pnpm test`（`test/unit/task-files.test.ts`）— `loadTaskFiles` が taskd の並びと
    `repos[]` をそのまま返すこと、`repo` / `path` を省いたらクエリに載せないこと（先頭のリポジトリ・
    根は taskd が決める）、指定したら載せること、ファイルを選ぶと一覧と同じ `repo` で
    `GET /tasks/{id}/tree/file` を引くこと。`treeBreadcrumbs` / `parentPath` / `taskFilesHref` の
    組み立ても別に検証。
  - 条件: `binary` / `too_large` は大きさだけを出す。
    実行: 同テスト — `fileBody` が `{kind:"binary", text:null, message:"バイナリのため表示しません（2.0 KiB）"}` /
    `{kind:"too_large", …"512 KiB を超えるため表示しません（1.0 MiB）"}` を返すこと、
    loader が `binary: true` の応答をそのまま通すこと（`text` は付かない）。
  - 条件: 403 / 404 は taskd の文言をそのまま出す。
    実行: 同テスト — `GET /tasks/{id}/tree/file` の 403 `path_forbidden` は**一覧を出したまま**
    `fileError`（status 403 / code / detail そのまま）になること、
    `GET /tasks/{id}/tree` の 404 `file_not_found` は例外になる（＝画面は `ErrorBoundary`）こと。
  - 条件: クライアントから taskd を直接呼ばない。
    実行: 同テスト — `app/components/task-files.tsx` に `fetch(` も `TASKD_API_URL` も無いこと、
    `/tasks/:id/files` に `action` が無いこと、`app/routes.ts` に兄弟のルートが登録されていること。
  - 条件: フェーズのゲート。
    実行: `pnpm lint` exit 0（biome、185 ファイル）/ `pnpm typecheck` exit 0 /
    `pnpm test` exit 0（**48 ファイル、627 tests passed**。G14 時点の 558 から +69）/ `pnpm build` exit 0。
    `pnpm gen:types` を流し直しても `app/taskd/types.ts` は変わらない
    （md5 `7c6b6b64aedd8340bb1a03b60d72539e` が前後で一致＝差分ゼロ）。
- 未解決事項:
  - G16-U1: `pnpm e2e` は未実行（G13k-U1 / G14-U1 と同じく、既定の 7700 / 7710 が運用中の
    GUI / taskd を掴むため）。別ポートを与えて人が流すときは、`/projects/:id` の「リポジトリ」節と
    `/tasks/:id/files`（作業ツリーを持つタスクが要る）を見るのがよい。
  - G16-U2: DOM を描画する unit テストは今回も無い（G10-U1）。行の描画・`hidden` のクラスタ欄・
    削除の `confirm` は Playwright でのみ確認できる。
  - G16-U3: `/tasks/:id` のタブの殻は ADR-0044 B1（別の担当）。いまは `/tasks/:id` の見出しに
    「ファイル」のリンクを 1 本足しただけで、タブになったら `~/components/task-files.tsx` を
    そのまま載せ替える（ルート `/tasks/:id/files` は残してよい）。
  - G16-U4: `POST /tasks` の `repos`（タスクがどのリポジトリを使うか）と `Task.repos[]` の表示は
    この Phase では入れていない（`/tasks/new` は案件を選ばない画面のまま）。作業ツリーの画面は
    タスクが実際に使っているリポジトリを `GET /tasks/{id}/tree` の `repos[]` から出している。
  - G16-U5: `sync`（`worktree` / `rsync`）は一覧に出すだけで、フォームからは送っていない
    （remote のときだけ有効で、`none` は taskd が 422。既定の `worktree` で足りるため）。
- 提案（`docs/taskd-api-v1.md` への変更提案。採否は人間）:
  - G16-P1: `POST /projects` の本文に `repos[]`（`RepoCreateBody` の配列）を足してほしい。いまは
    案件を作ってから 1 行ずつ `POST /projects/{id}/repos` するので、途中で 422 になると
    「案件はあるがリポジトリは半分」という中途半端な状態が残る（GUI は案件へのリンクを出して
    続きを案内しているが、原子的に作れる方がよい）。
  - G16-P2: `GET /tasks/{id}/tree` に「このタスクが `repos` を持たない（コードを伴わない調査）」と
    「作業ツリーがまだ作られていない（`draft` / `ready`）」を区別できる `code` がほしい。いまは
    どちらも 404 `file_not_found` なので、画面の言い方を分けられない。

## Phase G17 — タスク管理: タブ・編集・コメント・ボード（ADR-0044 B1。2026-09-19）

> **マージの註**: この枝も自分を「G16」と書いていたが、ADR-0043 A1 の GUI（上の G16）と
> 番号がぶつかったので、**マージのときに G17 に振り直した**（提案の番号も `G16-P*` → `G17-P*`）。
> `app/components/task-files.tsx` の stub は A1 の本物に差し替え、この枝の
> 「ファイル」タブに載せた（下の「マージで変えたところ」）。

- 完了日: 2026-09-19
- 目的: taskd Phase 53 で入った `PATCH /tasks/{id}` / `GET,POST /tasks/{id}/comments` /
  `POST /tasks/{id}/reopen` / `GET /tasks/{id}/timeline` と、`GET /tasks` の新しいフィルタ
  （`docs/taskd-api-v1.md` §3.3・§3.74〜3.78。マージで番号を振り直した）を画面にする。taskd 側の実装と**同じコミット**で入れた
  （この Phase は taskd Phase 53 と 1 対 1）。G15 は taskd Phase 50 の GUI 追従で、独立した節は作らなかった。
- 変更したファイル:
  - `app/routes/tasks.$id.tsx` — **タブ**（概要 / タイムライン / 変更 / ファイル / 成果物。`?tab=` で
    切り替え、SSR で解決）。概要に**編集フォーム**（題名・目的・tier・優先度 P0〜P3・ラベルのチップ・
    種類・担当・途中目標・依存）、タイムラインに `GET /tasks/{id}/timeline` の 1 本 + **コメント入力** +
    終端の**「再開」**。loader が `/timeline` `/comments` `/org` も読む。action は `edit` / `comment` /
    `reopen` に分岐。
  - `app/components/task-changes.tsx` / `task-files.tsx`（新規）— **差し替えるだけの stub**。
    ADR-0043 A1/A2 が本物を入れる（マウント点は 1 行で、直前に印のコメント）。
    **マージ後**: `task-files.tsx` は A1 の本物（G16）に差し替え済み。`task-changes.tsx` は
    stub のまま（ADR-0043 A2 が入れる）。
  - `app/routes/board.tsx`（新規）— `/board?project=…`。ADR-0044 D4 の 6 列、カード（題名・担当・tier・
    優先度・ラベル・種類・途中目標）、クエリに束縛したフィルタ欄、カード上の優先度 / tier / 担当の
    その場変更（`useFetcher` → `PATCH`）。既定で裏方（`TaskSummary.support`）を隠す。
  - `app/lib/board.ts`（新規）— 優先度の写像（`P0↔30 … P3↔0`、`i32 → ラベル`は taskd と同じ丸め）、
    列分け、`BoardFilter` の解析 / 組み立て、ラベルの形の検査。
  - `app/taskd/tasks-admin.server.ts`（新規）— `buildTaskEdit` / `editTask` / `commentOnTask` /
    `reopenTask` / `buildProjectTaskSpec`。
  - `app/routes/projects.$id.tsx` — **「タスクを追加」**（案件と各途中目標カード）、「ボードで見る」。
  - `app/routes.ts`（`route("board", …)`）、`app/root.tsx`（ナビに「ボード」、表示名を **Celeris** に）、
    `app/routes/help.tsx` と 21 ルートの `meta` の `<title>`（`- taskd-gui` → `- Celeris`）。
    `package.json` の `name`・`healthz` の `name`・`TASKD_GUI_RELEASE`・unit 名は**そのまま**。
  - `app/lib/labels.ts`（列・種類・優先度・tier・コメントの書き手・`effect` の文面・タイムラインの種別・
    編集した項目名・タブ名）、`app/components/Flash.tsx`、`app/taskd/action-types.ts`。
  - `app/taskd/actions.server.ts` — `Action` に `edit` / `reopen` が増えて `applyTransition` の網羅 switch が
    壊れたので `GateAction` から 2 つを除いた（`tsc` が検出）。
  - `app/taskd/types.ts` / `docs/taskd-api-v1.md` — `pnpm gen:types` と `scripts/sync-gui-docs.sh` で再生成。
  - `test/unit/{board,board.loader,tasks.manage.action}.test.ts`（新規）、`test/unit/labels.test.ts` と
    `test/unit/tasks.detail.loader.test.ts` を拡張（計 **+61 tests**）、
    `test/mock-taskd/{fixtures,server}.ts` に `serveTaskManagement`（comments / timeline / PATCH / reopen /
    フィルタ）、`test/fixtures/api/*.json` に増えた必須フィールド。
- 受け入れ条件ごとの証拠:
  - 条件: タスク画面が 5 つのタブになり、変更・ファイルは空でよい。
    実行: `pnpm test`（`tasks.detail.loader.test.ts`）— `?tab=` の解決、各タブの loader が読むもの、
    stub が「ADR-0043 で入る」を出すこと。
  - 条件: 編集フォームが `PATCH /tasks/{id}` を呼び、変わった項目が出る。
    実行: 同（`tasks.manage.action.test.ts`）— `buildTaskEdit` が「省略 = 触らない / 空 = 消す」を
    作り分けること、成功時に `fields` を、409 / 422 を文面のまま返すこと。
  - 条件: コメント欄が `POST /tasks/{id}/comments` を呼び、`effect` ごとに出す文面が変わる。
    実行: 同 — `stored` / `interrupted` / `answered` / `terminal` の 4 通り。
  - 条件: 終端のタスクに「再開」が出て `POST /tasks/{id}/reopen` を呼ぶ。
    実行: 同 — `actions` に `reopen` があるときだけ出す（GUI は規則を再実装しない）。
  - 条件: ボードが 6 列で、フィルタがクエリに束縛され、カード上で優先度 / tier / 担当を変えられる。
    実行: `pnpm test`（`board.test.ts` / `board.loader.test.ts`）— 8 状態 → 6 列の対応、
    優先度の写像（境界を含む往復）、フィルタの解析 → `GET /tasks` のクエリ文字列（繰り返しパラメータ込み）。
  - 条件: フェーズのゲート。
    実行: `pnpm lint` exit 0（biome、182 ファイル）/ `pnpm typecheck` exit 0 /
    `pnpm test` exit 0（**48 ファイル、620 tests passed**）/ `pnpm build` exit 0 /
    `pnpm gen:types` の後も `app/taskd/types.ts` はバイト一致。
- 未解決事項:
  - **`pnpm e2e` を回していない**（実 taskd のビルドとポートが要り、taskd Phase 53 の工事と
    ぶつかるため）。タブ化で `event-item` がタイムラインへ、`artifact-item` が成果物へ移ったので
    `e2e/g1.spec.ts` / `g3.spec.ts` の `page.goto` に `?tab=timeline` / `?tab=artifacts` を足し、
    `g5*.spec.ts` のフッタの表示名を Celeris に直した（**未実行**。次に e2e を回すときに確かめる）。
  - loader が `/timeline` と `/comments` の両方を読む（タイムラインにもコメントは載るので冗長）。
    コメント数をタブに出すために残した。
- 提案（`docs/taskd-api-v1.md` への変更提案。採否は人間）:
  - G17-P1: `GET /tasks/{id}/timeline` に `?kinds=` の絞り込みが欲しい（いまは全部返す）。
  - G17-P2: ボードは案件ごとに `GET /tasks?project=…&limit=500` を 1 回投げている。列ごとの件数だけ
    先に欲しくなったら `counts_by_status` をフィルタ後の値でも返してほしい（いまは DB 全体）。

### マージで変えたところ（Phase 52 + 53 = G16 + G17）

- **「ファイル」タブに A1 の本物を載せた**。`app/components/task-files.tsx` は B1 の stub を捨てて
  A1 の `TaskFiles`（名前付きエクスポート。リポジトリ切り替え・パンくず・一覧・本文）にし、
  `app/routes/tasks.$id.tsx` の `?tab=files` から呼ぶ。loader は **`?tab=files` のときだけ**
  `GET /tasks/{id}/tree`（と選んだファイルの本文）を引く（他のタブで毎回叩かないため）。
  403 / 404 はタブの中に taskd の文言を出すだけで、ページは落とさない。
- **兄弟のルート `/tasks/:id/files` はそのまま残した**（リダイレクトにはしていない）。同じ部品を
  全画面で出すページで、`ErrorBoundary` で 403 / 404 を扱う経路をすでに持っており、
  ブックマークや `docs/` のリンクを壊さないため。ヒーローの「ファイル」ボタンの行き先だけ
  `/tasks/:id?tab=files` に変えた（タブの中で見えた方が周りの文脈が残る）。
- **`app/lib/labels.ts`** は両方の節をそのまま並べた（A1: `repoKindLabel` / `treeEntryKindLabel` /
  `fileSizeLabel` …、B1: `boardColumnLabel` / `taskCategoryLabel` / `taskTabLabel` …）。
- **`app/routes/projects.$id.tsx`** は A1 の「リポジトリ」節と B1 の「タスクを足す」フォームの両方。
  `action` の `intent` も `repo_*` と `task_create` の両方を持つ。
- **サーバ専用モジュールの置き場を 2 つ動かした**（`pnpm build` が通らなかったため。React Router は
  ルートの `loader` / `action` からしかサーバ用のコードを剥がせず、テストのために公開している
  `loadTaskDetail` から `*.server` を runtime で参照すると**クライアントの束に混ざる**と言って止まる）:
  - `app/taskd/task-files.server.ts` → **`app/taskd/task-files.ts`**（中身は無変更。Node 専用の
    API は使っておらず、注入された `TaskdClient` を呼ぶだけ。`~/lib/task-files.ts` の純粋関数とは別物）。
  - `toActionError` を `app/taskd/actions.server.ts` → **`app/taskd/errors.ts`** に移し、
    `actions.server.ts` からは再輸出した（既存の import は 1 つも直していない）。
- **`app/taskd/types.ts`** は `pnpm gen:types` で作り直した（手では触っていない）。

## Phase G18 — 変更の取り込み（ADR-0043 D5 / taskd Phase 54。2026-09-19）

> **マージの註**: この枝も自分を「G17」と書いていたが、ADR-0044 B1 の GUI（上の G17）と
> 番号がぶつかったので、**マージのときに G18 に振り直した**（決めたこと・未解決・提案の番号も
> `G17-*` → `G18-*`）。下の G18-D1（「タブの殻はこの Phase では作らない」）はマージで実際に
> 変わっている: B1 のタブの殻ができたので、`~/components/task-changes.tsx` を
> **`/tasks/:id` の「変更」タブ（`?tab=changes`）に載せた**（`tasks.$id.tsx` の loader は
> `?tab=changes` のときだけ `GET /tasks/{id}/changes` を引く。「ファイル」タブと同じ作り）。
> **兄弟のルート `/tasks/:id/changes` は残した**（差分のリンクと取り込みの `fetcher` の送り先で、
> `ErrorBoundary` の経路も持つ）。ヒーローの「変更」ボタンの行き先だけ `/tasks/:id?tab=changes` に変えた。
> `app/taskd/task-changes.server.ts` は **`app/taskd/task-changes.ts`** に改名した
> （G17 のマージで `task-files.server.ts` を動かしたのと同じ理由。`loadTaskDetail` が
> runtime で `*.server` を参照するとクライアントの束に混ざって `pnpm build` が落ちる）。
> `docs/taskd-api-v1.md` の節番号は **§3.79〜3.83**（エンドポイント 68〜72）に振り直した。

- 完了日: 2026-09-19
- 目的: taskd Phase 54（ADR-0043 D5）で入った「変更の取り込み」を画面にする。タスクがブランチに作った
  変更を**人が見てから**、既定のブランチに取り込む（merge）・PR を作る・捨てる（discard）を選べるようにし、
  その記録（`task_integrations`）をタスクと案件の両方に出す。取り込みは**人だけ**（SPEC §3.6）で、
  組織の「人」が `main` を動かす経路は作らない。
  使う API は 5 つ: `GET /tasks/{id}/changes`、`GET /tasks/{id}/changes/{repo}/diff?path=`、
  `POST /tasks/{id}/changes/{repo}/integrate`（管理系）、`POST /tasks/{id}/changes/{repo}/pr/merge`（管理系）、
  `GET /projects/{id}/integrations`。
- 決めたこと:
  - G18-D1: 画面は**兄弟のルート `/tasks/:id/changes`**（`/tasks/:id/files` と同じ形。G16-D6）。中身は全部
    `~/components/task-changes.tsx`（自己完結の部品）に入れてあるので、ADR-0044 B1 のタブの殻ができたら
    そこへ 1 行で載せ替えられる。`/tasks/:id` に足したのは導線のリンク 1 本（「変更」）だけで、
    タブの殻はこの Phase では作らない（別の担当）。
  - G18-D2: 読み取り（一覧・差分）は `<Link>`（`?repo=&file=`）で loader を走らせ、取り込みは
    ルートの `action`（intent は `integrate` / `pr_merge` の 2 つ）に出す `useFetcher`。
    フォームには `action="/tasks/:id/changes"` を明示して、部品をどこに載せても送り先が変わらないようにする
    （`/projects/:id` の `TaskRow` が `/tasks/:id` に直接 POST するのと同じ作り）。
  - G18-D3: 結果は**リポジトリごとの `useFetcher`**（キー `integrate-<taskId>-<repo>`）に載せる。
    SSE の再検証で消えないため（監査 H1 / Phase G14 / `ProjectRepos.tsx` と同じ）。
  - G18-D4: **衝突と git の失敗は 200**（`integration.state === "conflict"` / `"failed"`）なので
    `IntegrateOutcome` は `ok: true` のままにして、部品が `state` で出し分ける。衝突のときは taskd の
    `detail`（衝突したファイル・作った解消タスクの id）をそのまま出し、`child_task_id` が返っていれば
    `/tasks/<child_task_id>` へのリンクも出す。GUI では衝突かどうかを判定しない。
  - G18-D5: 409 `default_branch_busy`（「main が編集中」）は `ErrorFlash` が 409 を一律
    「状態が変わりました」と読むので、**部品側に専用の Alert をもう 1 枚**出して taskd の文言と
    次にやること（手元の default_branch を片付ける）を見せる。`Flash.tsx` は触らない。
  - G18-D6: 「PR を作る」を押せるかどうかは taskd が返す `origin` / `gh` だけで決める（`prUnavailableReason`）。
    押せないときはボタンを `disabled` にして理由を文で出す。GUI で `gh` を探したりはしない。
  - G18-D7: `discard` の確認は**画面の中の 2 段階**（「捨てる（確認）」→ 赤い帯の「本当に捨てる」）で、
    2 段目だけが `confirm: true` を送る。`window.confirm` は使わない（`ProjectRepos` の削除と違い、
    消えるものの説明（リポジトリ名とブランチ名）を出したいため）。`confirm` を落としたときに
    422 `validation` になるのは taskd 側の規則で、GUI では検証しない。
  - G18-D8: 差分は `parseDiff` で 1 行ずつ `meta` / `hunk` / `add` / `del` / `ctx` に分けて `<span>` で描く
    （`dangerouslySetInnerHTML` は使わない。GUI CLAUDE.md の禁止）。`truncated` のときは
    「途中で切りました（200 KiB）」を添える。`missing` は「取り込み済み・中止済み（作業ツリーも
    ブランチもありません）」、`ahead === 0 && files.length === 0` は「変更なし」。
  - G18-D9: 案件の「PR と取り込み」節は `GET /projects/{id}/integrations` をそのまま並べるだけ
    （タスク × リポジトリごとに最新の 1 件・新しい順は taskd が決める）。**操作は置かない**
    （取り込みはタスクの画面で行う）。落ちても案件の詳細自体は出す（`.catch(() => ({items: []}))`。
    `GET /clusters` と同じ扱い）。
- 変更したファイル:
  - `app/lib/task-changes.ts`（新規）— `parseDiff` / `DIFF_LINE_CLASS` / `shortSha` / `statChip` /
    `fileDeltaChip` / `changedFileStatusTone` / `integrationStateTone` / `taskChangesHref`。
  - `app/lib/labels.ts` — `integrationMethodLabel` / `integrationStateLabel` / `changedFileStatusLabel` /
    `integrateMergeLabel` / `prUnavailableReason` と、`CREATE_PR_LABEL` / `DISCARD_CHANGES_LABEL` /
    `DISCARD_CHANGES_CONFIRM_LABEL` / `MERGE_PR_LABEL` / `NO_CHANGES_LABEL` / `CHANGES_MISSING_LABEL` /
    `DIFF_TRUNCATED_LABEL`。
  - `app/taskd/task-changes.server.ts`（新規）— `loadTaskChanges` / `readTaskChangesQuery` /
    `integrateChange` / `mergePullRequest` / `readIntegrateBody`。
  - `app/taskd/action-types.ts` — `IntegrateOutcome` を追加。
  - `app/components/task-changes.tsx`（新規）、`app/components/ProjectIntegrations.tsx`（新規）。
  - `app/routes/tasks.$id.changes.tsx`（新規、loader + action + meta + ErrorBoundary）、
    `app/routes.ts`（`tasks/:id/changes`）、`app/routes/tasks.$id.tsx`（「変更」への導線のリンク 1 本）。
  - `app/routes/projects.$id.tsx` — loader に `GET /projects/{id}/integrations` を足し、「PR と取り込み」節を追加。
  - `test/mock-taskd/fixtures.ts` — `repoChangesView` / `changesView` / `changeDiffView` /
    `taskIntegration` / `integrateResult` / `defaultProjectIntegrations`（5 つのエンドポイントぶん）。
  - `test/unit/task-changes.test.ts`（新規、45 件）。
  - `app/taskd/types.ts` / `docs/taskd-api-v1.md` — taskd 側が Phase 54 で同期済み（GUI からは触っていない。
    `pnpm gen:types` を流し直しても変わらない）。
- 受け入れ条件ごとの証拠:
  - 条件: `/tasks/:id/changes` でリポジトリごとの変更（ブランチ → 既定のブランチ、base / head の短い sha、
    進んだコミット数、dirty、stat、ファイル一覧）が読める。
    実行: `pnpm test`（`test/unit/task-changes.test.ts`）— `loadTaskChanges` が taskd の並びと値を
    そのまま返すこと（`files` は `src/lib.rs` / `src/new.rs` / `docs/old.md` の順）、ファイルを選ばなければ
    差分を引かないこと（要求は 1 本だけ）、`missing` / `ahead: 0` もそのまま通すこと。
    表示の組み立ては `shortSha`（`9602b596826c9f0f3b1c` → `9602b596826c`、空は `-`）と
    `statChip`（`3 ファイル +42 −12`）、`fileDeltaChip`（`+12 −4` / バイナリは行数を出さない）で検証。
  - 条件: ファイルを選ぶと unified diff が色つきで出る（`dangerouslySetInnerHTML` を使わない）。
    実行: 同テスト — `loadTaskChanges` が `GET .../changes/{repo}/diff` を `path=src/lib.rs` で引くこと、
    `parseDiff` が `["meta","meta","meta","meta","hunk","ctx","del","add","ctx"]` に分けること
    （`+++` / `---` はファイル名なので meta、`\ No newline…` と `Binary files…` も meta）、
    末尾の改行で余分な空行を作らないこと。ソースの確認で `app/components/task-changes.tsx` に
    `dangerouslySetInnerHTML` が無く `parseDiff` を使っていること。`truncated: true` はそのまま通り、
    文言は `DIFF_TRUNCATED_LABEL`（「途中で切りました（200 KiB）」）。
  - 条件: `missing` と「変更なし」を人の言葉で出す。
    実行: 同テスト — `CHANGES_MISSING_LABEL` が「取り込み済み・中止済み（作業ツリーもブランチも
    ありません）」、`NO_CHANGES_LABEL` が「変更なし」であること、loader が `missing: true` /
    `ahead: 0, files: []` をそのまま通すこと。
  - 条件: merge / PR / 捨てる（確認）の 3 つが押せて、結果が画面に残る。
    実行: 同テスト（`integrateChange`、8 件）— `merge` は本文 `{method:"merge",note:"ok"}` をそのまま送り
    200 の記録を返すこと、`discard` は `{method:"discard",confirm:true}` を送ること、
    `readIntegrateBody` が空欄をキーごと送らないこと（`note: ""` は送らない、`confirm` は確認欄が
    出ているときだけ `true`）。ボタンの文言は `integrateMergeLabel("main") === "main に取り込む"` /
    `integrateMergeLabel("develop") === "develop に取り込む"`（`default_branch` は taskd が返した値）。
    ルート側は `case "integrate":` / `case "pr_merge":` の 2 つと `<TaskChanges` の描画をソースで確認。
  - 条件: 409 `default_branch_busy` の「main が編集中」が画面に残る。
    実行: 同テスト — `integrateChange` が `{ok:false, error:{status:409, code:"default_branch_busy",
    detail:"main が編集中"}}` を返すこと（`toActionError` は 409 を一律 `conflict: true` にするので、
    部品には `data-testid="task-changes-busy"` の専用 Alert を足して文言と次にやることを出す。ソースで確認）。
  - 条件: PR を作れないとき（`origin` が無い・`gh` が使えない）は理由が出る。
    実行: 同テスト — `prUnavailableReason(true,true) === null` /
    `(false,true) === "origin リモートが無いので PR を作れません"` /
    `(true,false) === "gh が使えない（PATH に無い・未認証）ので PR を作れません"` /
    `(false,false)` は両方を言う 1 文。409 `pr_unavailable` は taskd の `detail` そのまま。
  - 条件: 衝突は 200 で「衝突の解消」タスクへ導ける。
    実行: 同テスト — `state: "conflict"` + `child_task_id: "01CHILD"` が `ok: true` のまま返り、
    `detail` に解消タスクの id が入っていること。git の失敗（`state: "failed"` + `detail`）も同じく 200。
  - 条件: 開いている PR を「Celeris で merge」できる。
    実行: 同テスト（`mergePullRequest`）— **空の JSON 本文 `{}`** を POST し、`state: "merged"` /
    `pr_number: 42` を返すこと、開いている PR が無ければ 409 `pr_unavailable`（`detail` そのまま）。
    方法（`merge_method`）は `ChangesView` の値をそのまま画面に出す。
  - 条件: 差分の 403 / 404 は一覧を出したまま文言を出し、一覧の 404 は画面ごとエラーにする。
    実行: 同テスト — `GET .../diff` の 403 `path_forbidden` は `changes.repos` を保ったまま
    `diffError`（status / code / detail そのまま）になること、`GET /tasks/{id}/changes` の 404
    `file_not_found` は例外になる（＝画面は `ErrorBoundary`）こと。
  - 条件: 案件に「PR と取り込み」節があり、`GET /projects/{id}/integrations` を新しい順に出す。
    実行: 同テスト — `loadProjectDetail` が `items` を並べ替えずそのまま返すこと
    （先頭が PR #42 / `task_title` 「ベンチマークの並列化」）、404 `project_not_found` でも
    案件の詳細自体は出て節が空になること。ソースの確認で `<ProjectIntegrations items={integrations} />` と
    「PR と取り込み」の見出しがあること。
  - 条件: クライアントから taskd を直接呼ばない。
    実行: 同テスト — `app/components/task-changes.tsx` / `app/components/ProjectIntegrations.tsx` に
    `fetch(` も `TASKD_API_URL` も無いこと、PR のリンクが `rel="noreferrer noopener"` で開くこと、
    `app/routes.ts` に `route("tasks/:id/changes", "routes/tasks.$id.changes.tsx")` が登録されていること、
    `/tasks/:id` に `data-testid="task-changes-link"` の導線があること。
  - 条件: フェーズのゲート。
    実行: `pnpm lint` exit 0（biome、191 ファイル）/ `pnpm typecheck` exit 0 /
    `pnpm test` exit 0（**49 ファイル、672 tests passed**。G16 時点の 48 ファイル / 627 から +1 ファイル・+45）/
    `pnpm build` exit 0（client / ssr とも）。`pnpm gen:types` を流し直しても `app/taskd/types.ts` は
    変わらない（md5 `d3b066ca6ff1d1e7e2cfe5023e2c098c` が前後で一致＝差分ゼロ。なお `git diff` は
    taskd Phase 54 が同期した未コミットの差分を出すので、GUI 側からの変更が無いことは md5 で見る）。
- 未解決事項:
  - G18-U1: `pnpm e2e` は未実行（G13k-U1 / G14-U1 / G16-U1 と同じく、既定の 7700 / 7710 が運用中の
    GUI / taskd を掴むため）。別ポートを与えて人が流すときは、`/tasks/:id/changes`（変更を持つ
    タスクが要る）と `/projects/:id` の「PR と取り込み」節を見るのがよい。
  - G18-U2: DOM を描画する unit テストは今回も無い（G10-U1）。差分の色分け・`discard` の 2 段階の確認・
    「Celeris で merge」が `state === "open"` のときだけ出ること・`disabled` の「PR を作る」は
    Playwright でのみ確認できる。
  - G18-U3: `/tasks/:id` のタブの殻は ADR-0044 B1（別の担当）。いまは「変更」のリンクを 1 本足した
    だけで、タブになったら `~/components/task-changes.tsx` をそのまま載せ替える
    （ルート `/tasks/:id/changes` は `action` があるので残す）。
  - G18-U4: PR の状態の同期は「画面を開いたとき」だけ（ADR-0043 D5）。GUI からは何もしていない
    （`GET /tasks/{id}/changes` が返した `integration` をそのまま出すだけ）ので、
    開いていない間に GitHub 側で merge / close されても画面はすぐには変わらない。
  - G18-U5: `updated_at` / `merged_at` は taskd が返した ISO の文字列をそのまま出している
    （このリポジトリに日時の整形の共通関数が無いため。`/daemon` の `formatDuration` は経過時間用）。
- 提案（`docs/taskd-api-v1.md` への変更提案。採否は人間）:
  - G18-P1: 409 `default_branch_busy` に、片付けるべきパス（人のチェックアウトの `local.path`）を
    載せてほしい。いまは `detail` の「main が編集中」だけなので、リポジトリが複数あるとき
    どのチェックアウトを片付ければよいかが画面から分からない。
  - G18-P2: `TaskIntegration` に「このタスクのどの run の結果か」または `note` そのものを別の欄で
    返してほしい。いまは人のひとことが `detail` の先頭に混ざるので、409 の理由や衝突ファイルの一覧と
    同じ場所に出てしまう（画面では 1 行として出すしかない）。
  - G18-P3: `GET /tasks/{id}/changes` で「このタスクは git のリポジトリを持たない（調査などコードを
    伴わない仕事）」と「作業ツリーがまだ作られていない」を画面で言い分けたい（G16-P2 と同じ理由）。
    いまは前者を `repos: []`、後者を 404 `file_not_found` として扱っているが、
    `docs/taskd-api-v1.md` に明記されると GUI の文言を確信を持って書ける。

## Phase G19 — 中止・一時停止・アーカイブ（ADR-0044 D6 / taskd Phase 55。2026-09-19）

- 完了日: 2026-09-19
- 目的: taskd Phase 55（ADR-0044 D6）で入った「案件・途中目標の中止・一時停止・アーカイブ」を画面にする。
  タスク 1 件の中止（`POST /tasks/{id}/cancel`）は従来どおりで、ここは**その上の 2 階層**。
  使う API は 8 つ（すべて**管理系**、本文 `{}`、応答 200）:
  `POST /projects/{id}/{cancel|pause|resume|archive|unarchive}` → `ProjectLifecycle`、
  `POST /milestones/{id}/{cancel|pause|resume}` → `MilestoneLifecycle`
  （`docs/taskd-api-v1.md` §3.84〜3.91、エンドポイント 73〜80）。
  読み取り側は `GET /projects?archived=` と `GET /tasks?archived=`（既定でアーカイブ済みを隠す）。
- 決めたこと:
  - G19-D1: **押せるかどうかの最終判断は taskd**（409 `invalid_transition`）。GUI には
    `app/lib/lifecycle.ts` という**表示の判定だけ**の純関数を置き、「その状態で意味のないボタンを
    出さない」ためにだけ使う。押されたら必ず taskd に送り、409 の文言をそのまま出す
    （状態機械を GUI で作り直さない。CLAUDE.md の「仕様外の挙動に頼らない」）。
    判定の根拠は §3.84〜3.91 の「いまの状態でできない操作」の 4 行だけ:
    中止済みの `cancel` / 終端・一時停止中の `pause` / `paused` でないものの `resume` / 非終端の案件の `archive`。
    したがって案件の終端は `done` / `cancelled`、途中目標の終端は `reached` / `redesigned` / `cancelled`
    （taskd 側が Phase 55 で `ProjectStatus::is_terminal()` / `MilestoneStatus::is_terminal()` を
    この 2 つに確定させ、§3.84〜3.91 に明記した。G19-P3 を参照）。
    **「中止」の出し方は案件と途中目標で違う**: `POST /milestones/{id}/cancel` は**終端の途中目標を
    409 にする**ので `cancel: !terminal`（達成・再設計・中止済みには出さない）。一方
    `POST /projects/{id}/cancel` は**完了した案件でも受ける**ので `cancel: status !== "cancelled"`。
    どちらも taskd の規則をそのまま写しただけで、GUI 側で「完了したものは中止できないはずだ」と
    決めつけてはいない。
  - G19-D2: **「アーカイブ」は終端でなくても消さずに `disabled` で出す**（理由を文で添える）。
    消してしまうと「どうすれば押せるのか」が画面から分からないため（G18-D6 の「PR を作る」と同じ扱い）。
    `archive` / `unarchive` は taskd 側で冪等なので、二度押しを GUI で止めたりはしない。
  - G19-D3: 中止とアーカイブの確認は**画面の中の 2 段階**（G18-D7 と同じ。`window.confirm` は使わない）。
    1 段目のボタンで赤い帯／情報の帯を開き、2 段目のボタンだけが `intent` を POST する。
    確認の文には対象の題名と「何が消えるか」を書く（中止は作業ツリーとブランチが消えて取り返しがつかない）。
  - G19-D4: 結果は**対象ごとの `useFetcher`**（`project-lifecycle-<id>` / `milestone-lifecycle-<id>`）に
    載せる。SSE の再検証で消えないため（監査 H1 / G14 / G18-D3 と同じ）。
  - G19-D5: 中止の flash には **taskd が返した `cancelled_tasks` / `cancelled_milestones` の長さ**を
    そのまま件数として出し、止まったタスクへのリンクも並べる（連鎖を GUI で数え直さない）。
    `cancel` 以外は空配列なので件数は出さない。
  - G19-D6: 一時停止のバナーは**必ず「新しい仕事は始まりません（走っている仕事は最後まで走ります）」**
    まで言う。止めたつもりで走り続けている run を事故と誤解しないため（§3.84〜3.91 の `pause` の行）。
    出す場所は案件詳細（ヘッダの直下）とボード（選んだ案件・途中目標が止まっているとき）。
  - G19-D7: 案件の「状態を直接変える」プルダウンから **`paused` を外した**（`proposed` / `active` / `done`
    の 3 つに）。`PATCH /projects/{id}` で `paused` にしても `paused_from` が入らず連鎖も起きないので、
    一時停止はヘッダのボタン（`POST …/pause`）に一本化する。`cancelled` も同じ理由で並べない。
    途中目標の「状態を直接変える」（裏方）も同じく `proposed` / `approved` / `in_progress` /
    `reached` / `redesigned` の 5 つのまま。
    **追記（taskd P55-7）**: taskd 側も同じ規則を入れ、`PATCH /projects/{id}` と `PATCH /milestones/{id}` は
    `status` に `paused` / `cancelled` を書くと **422 `validation`**（`errors[0].field = "status"`）で
    専用のエンドポイントへ案内するようになった（`docs/taskd-api-v1.md` §3.46 / §3.84〜3.91。
    `PATCH {"status":"paused"}` が 200 だった v1 の挙動の変更）。GUI の選択肢は変えていない
    （**この判断は GUI だけの取り決めではなく、taskd と同じ規則が 2 か所にある**という位置づけに変わった）。
    万一 422 が返っても、文言は `ProjectActionFlash` → `ErrorFlash` がそのまま出す（GUI では検証しない）。
  - G19-D8: 一覧の絞り込み（「アーカイブを表示」）は **`<Form method="get">` で URL の `?archived=1` を
    付け外しする**（ボードの絞り込みと同じ「URL がそのまま状態」。リンクとして共有できる）。
    隠す・出すの判断は taskd がするので、GUI は `archived=1` を**付けるかどうか**だけを決め、
    返ってきた `items` を絞り直さない。既定ではクエリごと送らない。
  - G19-D9: ボードの「アーカイブされた案件」のバナーは**作らなかった**。`GET /projects`（ボードの
    案件プルダウンの元）が既定でアーカイブ済みを隠すので、ボードからはそもそも選べない。
    アーカイブの状態は `/projects`（バッジ）と `/projects/:id`（バッジ + バナー）で見せる。
- 変更したファイル:
  - `app/lib/lifecycle.ts`（新規）— `projectLifecycleButtons` / `milestoneLifecycleButtons` /
    `projectIsTerminal` / `milestoneIsTerminal` / `projectIsArchived` / `projectIsPaused` /
    `milestoneIsPaused` / `readArchivedParam` / `archivedQuery` と終端の一覧 2 つ。
  - `app/lib/labels.ts` — `PROJECT_STATUS_LABEL` に `cancelled`、`MILESTONE_STATUS_LABEL` に
    `paused` / `cancelled`。`PAUSE_LABEL` / `RESUME_LABEL` / `CANCEL_LABEL` / `CANCEL_CONFIRM_LABEL` /
    `ARCHIVE_LABEL` / `ARCHIVE_CONFIRM_LABEL` / `UNARCHIVE_LABEL` / `CANCEL_STOP_LABEL` /
    `ARCHIVED_BADGE_LABEL` / `SHOW_ARCHIVED_LABEL` / `ARCHIVE_ONLY_TERMINAL_HINT` /
    `PROJECT_PAUSED_BANNER` / `MILESTONE_PAUSED_BANNER` / `PROJECT_CANCELLED_BANNER` /
    `PROJECT_ARCHIVED_BANNER` と `projectCancelConfirmText` / `milestoneCancelConfirmText` /
    `projectArchiveConfirmText` / `cancelledCountLabel`。
  - `app/taskd/projects-admin.server.ts` — `cancelProject` / `pauseProject` / `resumeProject` /
    `archiveProject` / `unarchiveProject` / `cancelMilestone` / `pauseMilestone` / `resumeMilestone`
    （内部は `projectLifecycle` / `milestoneLifecycle` の 2 つの私的ヘルパー）。
    冒頭の JSDoc を Phase 55 の認可の変更（`PATCH /projects/{id}` 等も管理系になった）に合わせた。
  - `app/taskd/action-types.ts` — `ProjectLifecycleOp` / `MilestoneLifecycleOp` と
    `ProjectOpOutcome` の `{ok:true, op, lifecycle}` の 2 枝、失敗側の `op` の union。
  - `app/components/Flash.tsx` — `ProjectActionFlash` に 8 つの `op` の文言と、中止のときの
    件数・止まったタスクへのリンク（`LIFECYCLE_OPS`）。
  - `app/routes/projects.$id.tsx` — `action` に 8 つの `intent`、`ProjectLifecycleActions` /
    `MilestoneLifecycleActions`（新規の部品）、ヘッダのバッジ 3 つ（一時停止・中止・アーカイブ済み）、
    バナー 3 つ、`PROJECT_STATUSES` から `paused` を外し、状態の tone に `cancelled` / `paused` を追加。
  - `app/routes/projects.tsx` — loader に `?archived=` の読み取り（`ProjectsData.showArchived`）、
    「アーカイブを表示」の GET フォーム、行のアーカイブ済みバッジ、tone に `cancelled`。
  - `app/routes/board.tsx` — 選んだ案件・途中目標が止まっているときのバナー 3 つ。
  - `app/routes/tasks.tsx` — `loadTasks` が `?archived=` を `GET /tasks` に素通しする 1 行。
  - `test/mock-taskd/fixtures.ts` — `project` / `milestone` / `taskRef` / `projectLifecycle` /
    `milestoneLifecycle`。
  - `test/mock-taskd/server.ts` — `serveLifecycle`（8 経路）と `serveProjectList`（`?archived=1`）。
  - `test/unit/lifecycle.test.ts`（新規、24 件。うち 1 件は taskd 側の `is_terminal()` 確定を受けた追加分）、
    `test/unit/labels.test.ts`（+4）、
    `test/unit/projects.test.ts`（+2）、`test/unit/projects.detail.test.ts`（+1）、
    `test/unit/tasks.loader.test.ts`（+1）。
  - `app/taskd/types.ts` — `pnpm gen:types` で再生成（`ProjectLifecycle` / `MilestoneLifecycle`、
    `ProjectStatus` の `cancelled`、`MilestoneStatus` の `paused` / `cancelled`、
    `Project.{archived_at, paused_from}`、`Milestone.paused_from`）。手では書いていない。
  - `docs/taskd-api-v1.md` は**触っていない**（`scripts/sync-gui-docs.sh` が `docs/gui/api.md` から
    写す生成物。ADR-0020 D4）。
- 受け入れ条件ごとの証拠:
  - 条件: 案件のヘッダに「一時停止／再開」「中止（確認付き）」「アーカイブ／アーカイブ解除（確認付き）」が
    状態に応じて出る。終端でない案件はアーカイブできない。
    実行: `pnpm test`（`test/unit/lifecycle.test.ts`）—
    `projectLifecycleButtons({status:"active"})` が
    `{pause:true, resume:false, cancel:true, archive:true, unarchive:false, archiveEnabled:false}`、
    `"paused"` は `pause:false / resume:true`、`"done"` は `pause:false / archiveEnabled:true`、
    `"cancelled"` は `cancel:false / archiveEnabled:true`、`archived_at` が入ると
    `archive:false / unarchive:true`。**`archiveEnabled` が真になるのは `done` と `cancelled` だけ**
    （5 つの状態を回して `["done","cancelled"]` を確認）。
  - 条件: 途中目標のカードに「一時停止／再開」「中止（確認付き）」が出る。
    実行: 同テスト — `milestoneLifecycleButtons({status:"in_progress"})` が
    `{pause:true, resume:false, cancel:true}`、`"paused"` は `{pause:false, resume:true, cancel:true}`
    （一時停止中でも中止はできる）、`"cancelled"` と `"reached"` は 3 つとも false。
    6 つの状態を回して**一時停止できるのも中止できるのも** `["proposed","approved","in_progress"]` だけ
    （`reached` / `redesigned` / `cancelled` は終端で、taskd が 409 にする）。
    **案件との違い**: 案件は `done` でも中止できる（上の G19-D1）。
  - 条件: 8 つの経路に空の本文を送り、応答をそのまま画面に渡す。
    実行: 同テスト（`serveLifecycle` の偽 taskd）— `cancelProject` が
    `POST /api/v1/projects/p1/cancel` に **body `"{}"`** を送り、
    `{ok:true, op:"project_cancel", lifecycle:{project:{status:"cancelled"}, cancelled_tasks:[1 件],
    cancelled_milestones:["m1"]}}` を返すこと。`pause` は `paused_from:"active"` を、
    `milestone pause` は `paused_from:"in_progress"` をそのまま通すこと。
    `archive` → `archived_at:"2026-09-19T12:00:00Z"`、`unarchive` → `archived_at` 無し、
    **二度押しも 200**（冪等）。id は URL エンコードして送る（`a/b` → `/projects/a%2Fb/cancel`）。
  - 条件: できない操作（409）・無い id（404）・トークン無し（401）は taskd の文言のまま出る。
    実行: 同テスト — 非終端の案件の `archive` が
    `{ok:false, op:"project_archive", error:{status:409, code:"invalid_transition", conflict:true}}`、
    `pause` の 404 `project_not_found` と 401 `unauthorized`、
    `resumeMilestone` の 404 `milestone_not_found` と 409 `invalid_transition`。
    いずれも例外にせず data として返る（401 の案内文は `Flash.tsx` の 1 か所に集約済み）。
  - 条件: 中止のあと「何件止まったか」が画面に出る。
    実行: `pnpm test`（`test/unit/labels.test.ts`）—
    `cancelledCountLabel(3, 1) === "仕事 3 件・途中目標 1 件を中止しました"`、
    `cancelledCountLabel(0) === "仕事 0 件を中止しました"`。
    `Flash.tsx` の `ProjectActionFlash` が `lifecycle.cancelled_tasks.length` /
    `cancelled_milestones.length` をそのまま渡し、止まったタスクを `/tasks/<id>` のリンクで並べること
    （`data-testid="flash-lifecycle-cancelled"` / `"flash-lifecycle-task"`）をソースで確認。
  - 条件: 一時停止・中止・アーカイブのバッジが題名の横に出る。
    実行: ソースの確認 — `app/routes/projects.$id.tsx` の `PageHeader` の `actions` に
    `project-status` に加えて `project-paused-badge` / `project-cancelled-badge` /
    `project-archived-badge`。`app/routes/projects.tsx` の一覧の「状態」欄に
    `project-status` + `project-archived-badge`。文言は
    `projectStatusLabel("cancelled") === "中止"` / `milestoneStatusLabel("paused") === "一時停止"` /
    `milestoneStatusLabel("cancelled") === "中止"` / `ARCHIVED_BADGE_LABEL === "アーカイブ済み"`
    （`test/unit/labels.test.ts`）。
  - 条件: 一時停止のバナーが案件とボードに出る。
    実行: `pnpm test`（`test/unit/labels.test.ts`）— `PROJECT_PAUSED_BANNER` が
    「一時停止中」「新しい仕事は始まりません」「走っている仕事は最後まで走ります」の 3 つを含むこと、
    `MILESTONE_PAUSED_BANNER` も同じ断りを含むこと。ソースの確認で
    `projects.$id.tsx` に `project-paused-banner` / `project-cancelled-banner` /
    `project-archived-banner`、`board.tsx` に `board-project-paused` / `board-project-cancelled` /
    `board-milestone-paused`（選んだ案件・途中目標を `filter.project` / `filter.milestone` で引いて判定）。
  - 条件: 一覧に「アーカイブを表示」があり、URL の `?archived=1` で切り替わる。
    実行: `pnpm test`（`test/unit/projects.test.ts`、2 件）— 既定では
    `GET /api/v1/projects` に `archived` を**付けない**（`showArchived === false`、
    アーカイブ済みの `p9` は並ばない）。`?archived=1` のときだけ `archived=1` を付け、
    偽 taskd（`serveProjectList`）がアーカイブ済みも返すと `["p1","p9"]` が並ぶこと。
    `readArchivedParam` / `archivedQuery` の対応（`""`→false→クエリ無し、`"1"`→true→`"1"`、
    `"0"`→false）は `test/unit/lifecycle.test.ts`。
  - 条件: タスク一覧も `?archived=1` を素通しする。
    実行: `pnpm test`（`test/unit/tasks.loader.test.ts`）— `?archived=1` のとき
    `/api/v1/tasks?archived=1`、無いときは `/api/v1/tasks`（クエリを足さない）。
  - 条件: 新しい項目（`archived_at` / `paused_from` / `paused` / `cancelled`）を loader が素通りさせる。
    実行: `pnpm test`（`test/unit/projects.detail.test.ts`）— `GET /projects/{id}` が返した
    `status:"paused"` / `paused_from:"active"` / `archived_at` と、`paused` / `cancelled` の
    途中目標 2 件がそのまま `loaderData` に載ること（GUI 側で計算し直さない）。
  - 条件: 変更系はすべて BFF のトークン付きクライアントを通る（Phase 55 で変更系が全部管理系になった）。
    実行: ソースの監査 — `client.post|patch|put|delete` の呼び出しは `app/taskd/*.server.ts` と
    `app/taskd/task-changes.ts` の **52 か所すべて**が引数の `TaskdClient` を使い、
    `action` を持つルート 20 本のうち **18 本が `getTaskdClient()`**（= `TaskdClient.fromEnv()` で
    `TASKD_API_TOKEN_FILE` を読み `Authorization: Bearer` を付ける 1 つの共有クライアント）。
    残り 2 本（`routes/login.tsx` / `routes/logout.ts`）は GUI 自身のセッションクッキーだけで
    taskd を呼ばない。`app/` に残る素の `fetch(` 3 か所（`tasks.$id.runs.$runId.tsx` /
    `tasks.$id.tsx` / `components/ArtifactsList.tsx`）は**ブラウザから GUI 自身の
    `/files/...` を GET する**もので、taskd を直接呼んでも変更もしていない。
  - 条件: フェーズのゲート。
    実行: `pnpm lint` exit 0（biome、199 ファイル）/ `pnpm typecheck` exit 0 /
    `pnpm test` exit 0（**53 ファイル、758 tests passed**。この Phase の前は 52 ファイル / 726 なので
    +1 ファイル・+32）/ `pnpm build` exit 0（client / ssr とも）。
    `pnpm gen:types` は**2 回流しても差分ゼロ**（1 回目で Phase 55 の型が入り、2 回目は `diff -u` が無出力）。
    `git diff --exit-code app/taskd/types.ts` は**この Phase の再生成ぶん**を出す（コミット前なので当然）。
- 未解決事項:
  - G19-U1: `pnpm e2e` は未実行（G13k-U1 / G14-U1 / G16-U1 / G18-U1 と同じく、既定の 7700 / 7710 が
    運用中の GUI / taskd を掴むため）。別ポートを与えて人が流すときは、`/projects/:id` の
    「一時停止 → バナー → 再開」「中止（2 段階）→ 件数の flash」「完了の案件のアーカイブ →
    `/projects` から消える → 『アーカイブを表示』で戻る」を見るのがよい。
  - G19-U2: DOM を描画する unit テストは今回も無い（G10-U1 / G18-U2）。確認の 2 段階・
    `disabled` のアーカイブ・バッジとバナーの出現は Playwright でのみ確認できる。
    いまは純関数（`app/lib/lifecycle.ts`）と文言（`app/lib/labels.ts`）に分けて、判定の側だけを
    テストで押さえている。
  - G19-U3: `paused` の案件・途中目標に属するタスクは taskd が dispatch しないだけで、`ready` のまま
    ボードの「待ち」に並ぶ。バナーで断ってはいるが、カード 1 枚 1 枚には印が無い
    （`TaskSummary` に「上が止まっている」を表す項目が無いため。下の G19-P1）。
  - G19-U4: 一時停止・中止は報告も通知も作らない（ADR-0044 D8）ので、**他の画面から止まったことに
    気づく手がかりが無い**（受信箱にも `/reports` にも出ない）。案件・ボードを開いたときだけ分かる。
  - G19-U5: `POST /projects/{id}/cancel` の `cancelled_milestones` は **id の配列**なので、flash では
    件数しか出していない（題名を出すには案件の詳細を引き直す必要がある）。下の G19-P2。
- 提案（`docs/taskd-api-v1.md` への変更提案。採否は人間）:
  - G19-P1: `TaskSummary` に「このタスクは上（案件・途中目標）が止まっているので dispatch されない」を
    表す真偽値が欲しい（`blocked_by_parent` など）。いまはボードの「待ち」に並ぶカードが、
    順番待ちなのか上ごと止まっているのかを画面で言い分けられない（判定を GUI でやり直すのは
    ADR-0033 D8 / CLAUDE.md の禁止に当たる）。
  - G19-P2: `ProjectLifecycle.cancelled_milestones` を `cancelled_tasks` と同じく
    `{id, title, seq}` の配列にしてほしい。いまは id だけなので、「何が止まったか」を人の言葉で
    出すには案件の詳細を引き直すことになる。
  - G19-P3: （**解決済み**）§3.84〜3.91 に「非終端の途中目標（`proposed`/`approved`/`in_progress`/`paused`）」
    としか書かれていなかったので、終端を補集合（`reached` / `redesigned` / `cancelled`）で読んでいた。
    taskd 側が Phase 55 で `MilestoneStatus::is_terminal()` = `reached | redesigned | cancelled` /
    `ProjectStatus::is_terminal()` = `done | cancelled` と揃え、§3.84〜3.91 に明記して
    `scripts/sync-gui-docs.sh` を流し直したので、`app/lib/lifecycle.ts` の 2 つの一覧と 1:1 で一致する。
  - G19-P4: `GET /projects` の `archived` のように、**`GET /projects` にも `status` の絞り込み**が
    欲しい（中止済みだけを隠す、など）。アーカイブしていない中止済みの案件が一覧に残り続けるため。

## Phase G20 — 案件の文書（ADR-0044 D7 / taskd Phase 57。2026-09-19）

> **マージの註**: この枝（taskd Phase 57 / ADR-0044 B3）はコメントの中で自分を「G19」と書いていたが、
> main の G19 は ADR-0044 D6 の GUI（上の G19）で先に埋まっていたので、**マージのときに G20 に振り直した**。
> 併せて taskd 側の節番号も §3.84〜3.89 → **§3.92〜3.97**（エンドポイント 81〜86）に振り直してある。

- 完了日: 2026-09-19
- 目的: taskd Phase 57（ADR-0044 D7）で入った「案件の文書（**正本は git のファイル**）」を画面にする。
  使う API は 6 つ（`docs/taskd-api-v1.md` §3.92〜3.97、エンドポイント 81〜86。**変更系はすべて管理系**）:
  `GET /projects/{id}/docs`、`GET|PUT|DELETE /projects/{id}/docs/page`、
  `POST /projects/{id}/docs/init`、`POST /tasks/{id}/artifacts/promote`。
- 作ったもの:
  - `app/routes/projects.$id.docs.tsx`（`/projects/:id/docs`。ツリー・描画・編集とプレビュー・履歴・
    削除・検索・「文書を用意する」）、`app/routes.ts` に 1 行（`tasks/:id/files` と同じ兄弟のルート）。
  - `app/lib/docs.ts`（純粋関数。`celeris:task/<id>` と `[[相対パス]]` の開き方）と
    `test/unit/docs.test.ts`（**17 件**）。
  - `app/taskd/docs.ts`（読み取りの中継）/ `app/taskd/docs-admin.server.ts`（変更系。§3.94〜3.97）、
    `app/taskd/action-types.ts` に `DocsOpOutcome`、`app/lib/labels.ts` に文書の言葉と `docsErrorHint`。
  - `app/routes/tasks.$id.tsx`: 成果物タブの「文書に昇格」（`intent=promote`）と、タイムラインの
    `kind = "doc"`（逆リンク）の行。
  - `app/routes/projects.$id.tsx`: **末尾に「文書」節（入口だけ）**。ヘッダ（G19 の中止・一時停止・
    アーカイブ）は触っていない。
  - `test/mock-taskd/fixtures.ts` に文書の 4 つ（`docsTree` / `docPage` / `docPageResult` / `docsInitResult`）。
- 決めたこと・未解決・提案は **taskd 側の `docs/PROGRESS.md` の「Phase 57」**（P57-1〜P57-6、
  とくに **P57-4「GUI は `html` ではなく `raw` を描く」**＝`dangerouslySetInnerHTML` の禁止を守る）に
  まとめてある。ここでは重複して書かない。

---

## Phase G21 — 知識ベースの画面（ADR-0047 D5。2026-09-20）

- 完了日: 2026-09-20
- 目的: celeris Phase 61（ADR-0047 D5）で入った「知識ベース（**正本は `[knowledge] root` の Markdown**）」を
  画面にする。使う API は 6 つ（`docs/celeris-api-v1.md` §3.98〜3.103、エンドポイント 87〜92。
  **変更系はすべて管理系**）: `GET /knowledge/tree`、`GET|PUT /knowledge/page`、`GET /knowledge/inbox`、
  `POST /knowledge/inbox/{id}/accept`、`POST /knowledge/inbox/{id}/reject`。

### 作ったもの

- `app/routes/knowledge.tsx`（`/knowledge`。左は置き場ごとに束ねたツリー + 検索（`?q=`） + 置き場の絞り込み
  （`?scope=`）、右は選んだページ（`?path=`）の描画・メタ情報・編集フォーム・履歴）。
- `app/routes/knowledge.inbox.tsx`（`/knowledge/inbox`。`_inbox` の候補を 1 枚ずつ出し、取り込み先を
  書き換えられる accept（`overwrite` のチェック付き）と reject）。**別ルートにした**（`/knowledge` の中の
  タブではなく）: 候補は索引にも検索にも出ない別の世界で、`GET /knowledge/inbox` も別の呼び出しだから。
- `app/routes.ts` に 2 行（`reports` の前）、`app/root.tsx` の `NAV_GROUPS`「業務」に「知識」（`database`）。
- `app/celeris/knowledge.ts`（読み取りの中継。`loadKnowledge` / `loadKnowledgeInbox` / `readKnowledgeQuery`）、
  `app/celeris/knowledge-admin.server.ts`（変更系。§3.100 / §3.102 / §3.103）、
  `app/celeris/action-types.ts` に `KnowledgeOpOutcome`。
- `app/lib/knowledge.ts`（**純粋関数**。置き場ごとの束ね方・URL・パスの検査・front matter の読み取り・
  出典の開き先・`[[相対パス]]` と `celeris:task/<id>` の書き換え）と `app/components/KnowledgeMeta.tsx`
  （置き場 / タグ / 出典 / 確度 / 更新。ページと候補で共用）。
- `app/lib/labels.ts` に知識ベースの言葉と `knowledgeErrorHint`。
- `test/mock-celeris/fixtures.ts` に 7 つ（`knowledgeTree` / `knowledgeTreeUninitialized` / `knowledgePage` /
  `knowledgePageResult` / `knowledgeRejectResult` / `knowledgeInbox` / `knowledgeCandidate`）と
  `test/mock-celeris/server.ts` に `serveKnowledge`（6 経路を一度に登録）。
- `test/unit/knowledge.test.ts`（**27 件**）。

### 決めたこと

- **G21-1: `html` は使わない。`raw` を `react-markdown` で描く。** celeris は `html` も返すが、
  `dangerouslySetInnerHTML` の禁止（gui/CLAUDE.md、DESIGN §8.3）を守る。文書タブ（G20 / P57-4）と同じ判断。
  `celeris:task/<ULID>` は `/tasks/<ULID>` に、`[[相対パス.md]]` は `/knowledge?path=…` に、
  **KB の根の外に出るものは文字のまま**（`resolveKnowledgePath` が `null` を返す）。
- **G21-2: 「用意する」ボタンは出さない。** `initialized === false` のときは置き場（`root`）を見せて
  「`celerisctl knowledge init` で用意してください」と書くだけ（init の API が無い。§3.98）。
  `[knowledge] root` 自体が無い 409 `knowledge_unavailable` は別の板で `detail` + 案内を出す。
- **G21-3: 送る前のパスの検査は「先に見せるだけ」。** `knowledgePathProblem` が `..`・絶対パス・`\`・
  `.md` で終わらないもの・`_inbox/` を止めるが、**判定の正本は celeris**（403 `path_forbidden` /
  422 `validation`）。GUI は通してしまっても構わない前提で、保存ボタンを無効にするだけ。
- **G21-4: `?q=` のときは束ねない。** 検索の並びは celeris が決める（タグ一致数 → title 一致数 → 本文 →
  `updated` の新しい順。ADR-0047 D3）ので、束ねると順位が壊れる。`?q=` があれば平らな一覧を
  celeris が返した順のまま出し、無いときだけ `scopes` + ディレクトリで束ねる。
- **G21-5: 出典（`sources[]`）の開き先。** `task:<ULID>` → `/tasks/<ULID>`、
  `url:<…>` → 外部リンク（`target="_blank" rel="noreferrer noopener"`。**`http`/`https` だけ**。
  `javascript:` 等は文字のまま）、`message:<id>` と `human` は文字のまま。
- **G21-6: front matter の読み取りは編集画面のためだけ。** `parseKnowledgeFrontMatter` は
  `key: value` / `key: [a, b]` / `key:` + `- a` という**ごく狭い YAML** しか読まない。
  書いている最中の本文を人に見せ返す用で、正本は保存後に celeris が返す `KnowledgePage`。

### 実行したコマンドと出力の要点

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| 型が生成できる・二度目で変わらない | `pnpm gen:types`（2 回） | exit 0。`app/celeris/types.ts` の sha256 は 2 回目も `c10e490b…` で**同一**（生成は冪等）。HEAD との差分は **+193 行 / -0 行**＝`Confidence` と `Knowledge*` 9 型の追加だけ（celeris 側の `docs/api/v1/api-v1.schema.json` が未コミットなので `git diff --exit-code` は 1 を返す。スキーマを含めてコミットすれば 0） |
| lint | `pnpm lint` | exit 0。`Checked 211 files in 84ms. No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0（`react-router typegen && tsc -b`、出力なし） |
| test | `pnpm test` | exit 0。**Test Files 55 passed (55) / Tests 802 passed (802)**。うち `test/unit/knowledge.test.ts` が **27 件** |
| build | `pnpm build` | exit 0。client 127 modules / SSR ともに成功。新しいチャンク `knowledge-*.js`（12.52 kB）・`knowledge.inbox-*.js`（6.77 kB）・`KnowledgeMeta-*.js`（5.32 kB） |
| e2e | （実行せず） | `pnpm e2e` は実 celeris のバイナリが要るのでこの枝では**動かしていない**。知識ベースの e2e は celeris 側の `[knowledge] root` を設定した fixture が必要 |

### 未解決事項

- **U1: e2e が無い。** `/knowledge` と `/knowledge/inbox` は unit（純粋関数 + mock celeris）だけで見ている。
  実 celeris に `[knowledge] root` と `celerisctl knowledge init` 済みの KB を用意する fixture
  （`test/celeris/celeris.toml.tmpl` に `[knowledge]`）を足せば e2e にできる。
- **U2: 候補の件数バッジがナビに出ない。** `tree.inbox_count` は `/knowledge` のヘッダにしか出していない。
  ナビのバッジ（「認可」「報告」と同じ）にするには root の loader で `GET /knowledge/tree` を引く必要があり、
  全画面に 1 回の呼び出しを足すことになるので今回は見送った。
- **U3: ページの削除が無い。** `DELETE /knowledge/page` は API に無い（文書には §3.96 がある）。
  画面にも出していない。消すのは手元の git で。

### 提案

- **P-G21-1: `docs/celeris-api-v1.md` §3.98 の `scopes` の意味を書き足してほしい。**
  いまは「ページのあるディレクトリの一覧」とあるが、`?scope=` は「KB 相対の接頭辞**か** front matter の
  `scope` の値（`project:pluvio`）」の両方を受ける。`scopes[]` に返るのがどちらの世界の値なのか
  （ディレクトリだけか、front matter の `scope` も混ざるか）が読み取れない。GUI は
  「`scopes` はディレクトリ、`items[].scope` は front matter」と解釈して**別々に**出している。
- **P-G21-2: §3.101 の `created` の形を決めてほしい。** `KnowledgeItem.updated` は「RFC 3339 か
  `YYYY-MM-DD`」と書いてあるが、`KnowledgeCandidate.created` には何も書いていない。GUI はそのまま出している。
- **P-G21-3: `GET /knowledge/tree` に `?q=` を付けたときの `scopes` の扱い。** 絞った結果に合わせて
  減るのか、KB 全体のままなのかが書かれていない。GUI は `?q=` のときツリーを束ねない（G21-4）ので
  実害は無いが、絞り込みの `<select>` には `scopes` をそのまま出している。

## Phase G22 — Console: 指示・CoS の actions・画面（ADR-0048 D4 + P-59-a。2026-09-20）

- 完了日: 2026-09-20
- 目的: celeris Phase 60b（ADR-0048 D3、`POST /console/instruct` と CoS の宣言的 `actions`）を受けて、
  `docs/adr/0048-console.md` D4 の GUI（`/` = Console、`/org/:id` も同じ部品）を作る。あわせて P-59-a
  （`docs/PROGRESS.md` の提案。CoS への改名の GUI 側の仕上げ）を同じフェーズで片づける（対象ファイルが
  ほぼ重なるため。指示は「Node page /org/{id} … CoS もここで受ける」で、結局 CoS 専用ページを無くす形に
  なったので、まとめた方が手戻りが少ないと判断した）。
- 使う API: `docs/gui/api.md` §3.98〜3.100（`GET /console` / `GET /console/stream` /
  `GET /tasks/{id}/runs/{run_id}/events`。読み取り）、§3.107（`POST /console/instruct`。**管理系**）。

### 作ったもの（新規ファイル）

- `app/lib/console.ts`（**純粋関数**。`~/lib/reports.ts` / `~/lib/approvals.ts` と同じ方針。HTTP も React も
  持たない）: `ConsoleData` 型、`normalizeScope` / `parseScope` / `scopeForProject` / `scopeForNode`、
  `findMentionQuery` / `matchMentionCandidates` / `applyMention`（`@node` 補完）、
  `replyTargetForMessageBlock` / `buildInstructBody`（返信先・既定範囲・`@mention` の優先順位）、
  `progressSummaryLine` / `taskLineSummary`、`consoleWaitingCounts`（上部の帯の件数）、
  `appendConsoleBlock`（SSE の積み上げ。`progress` は run 単位で置き換え、それ以外は `cursor` で重複排除、
  上限を超えたら古い方から落とす）、`formatRunEventRow`（「すべて見る」の行整形）。
- `app/celeris/console.server.ts`（中継）: `loadConsole`（`GET /console` + `GET /org` + `GET /projects` を束ねる。
  org/projects は落ちても Console 自体は出す）、`sendInstruct`（`POST /console/instruct`）、
  `buildInstructBodyFromForm`。
- `app/hooks/useConsoleStream.ts`: `createConsoleStreamController`（`EventSource` に依存しない JSON 解釈 +
  カーソル追跡。`~/hooks/useCelerisStream.ts` の `createStreamController` と同じ切り出し方）と、それを
  `EventSource` に配線する `useConsoleStream`（切断時は既定の自動再接続に任せず、自前で閉じて最新の
  カーソルから張り直す）。
- `app/routes/console.stream.ts`（`/console/stream` の SSE 中継。`~/routes/events.ts` と同じ作り）。
- `app/routes/tasks.$id.runs.$runId.events.ts`（`/tasks/:id/runs/:runId/events` の resource route。
  Console の `progress` ブロックの「すべて見る」が `useFetcher().load()` から呼ぶ。`~/routes/reports.$id.tsx`
  と同じ作り）。
- `app/components/Console.tsx`（画面本体）: 上部の待ち件数の帯（`consoleWaitingCounts`）、左の範囲ピッカー
  （全体／案件／ノード。`useNavigate` で `/`・`/?scope=project:<id>`・`/org/:id` へ行き来する）、中央の流れ
  （sticky-to-bottom スクロール: 下端付近を見ているときだけ新着で自動スクロール）、下の入力欄
  （`@` 補完、Enter 送信・Shift+Enter 改行、「返信」で選んだ先を上に表示）。
- `app/components/ConsoleBlockItem.tsx`（ブロック 1 件、`kind` で 9 分岐）。**状態変更のロジックは持たない**:
  認可・途中目標の判定・タスクへのコメント／回答は、既存の画面（`/approvals`・`/projects/:id`・`/tasks/:id`）
  の action をそのまま React Router のクロスルート fetcher（`fetcher.Form action="/…"`。
  `~/routes/inbox.tsx` の `retryFetcher.Form action={`/tasks/${id}`}` と同じ作法）で叩くだけ。
- テスト: `test/unit/console.test.ts`（27 件。`~/lib/console.ts` の純粋関数）、
  `test/unit/console.server.test.ts`（9 件。`loadConsole` / `sendInstruct` を mock celeris で）、
  `test/unit/useConsoleStream.test.ts`（6 件。`createConsoleStreamController`。`EventSource` は使わない）、
  `test/unit/console.stream.route.test.ts`（4 件。SSE 中継。`test/unit/events.route.test.ts` と同じ作り）、
  `test/unit/tasks.runs.events.route.test.ts`（3 件。run の全行の中継）。

### 変更したもの

- `app/routes.ts`: `console/stream` と `tasks/:id/runs/:runId/events` を追加。`org/secretary` は残したまま
  コメントを更新（P-59-a）。
- `app/routes/home.tsx`（全面書き換え）: 「`/` は秘書へ 302」（Phase G13f-1）をやめ、`/` 自身が
  `scope=all`（既定）の Console になった。`?scope=project:<id>` / `?scope=node:<id>` の深リンクを受ける
  （`normalizeScope` で不正な形は `all` に丸める）。celeris 停止中も 200 で開く契約
  （docs/DESIGN.md §10 Phase G0 受け入れ条件 4）はここが引き継ぐ。
- `app/routes/org.$id.tsx`（全面書き換え）: 旧 `Conversation` 部品（202 → ポーリングで返事を待つ）をやめ、
  `scope=node:<id>` の Console を描く。CoS（`id: "cos"`）もこのルートで受ける。
- `app/routes/org.secretary.tsx`（全面書き換え）: celeris に問い合わせない、`/org/cos` への 302 だけの
  ルートにした（P-59-a）。
- `app/celeris/action-types.ts`: `ConsoleInstructOutcome` を追加。
- `app/celeris/client.server.ts`: `CelerisClient.consoleStream()`（`GET /console/stream`）を追加
  （既存の `stream()` と同じ作り。タイムアウト無し、`signal` で切断）。
- `app/celeris/types.ts`: 手では触っていない（`pnpm gen:types` で再生成しただけ。下記「gen:types」参照）。
- P-59-a（画面の言葉「秘書」→「Chief of Staff（CoS）」。コード上の判断・ADR の歴史を指すコメントは変えていない）:
  `app/lib/notify.ts`（Discord 通知の種別ラベル）、`app/root.tsx`（ナビ先頭を「秘書 → `/org/secretary`」から
  「Console → `/`」に。エラー画面の「秘書へ戻る」を「Console へ戻る」に）、`app/routes/org.tsx`
  （`ORG_KIND_LABEL.secretary` と見出しの説明文）、`app/routes/help.tsx`（用語集の「秘書」項目を
  「CoS（Chief of Staff）」に改名し Console を指すよう本文を更新。SCREENS の先頭を「秘書」から「Console」の
  説明に差し替え。他の項目内の「秘書」表記も CoS に）、`app/routes/reports.tsx`（level セレクトの選択肢と
  Discord 通知の説明文）、`app/routes/projects.$id.tsx`（Alert のタイトル 3 箇所・カードの説明文・
  「議論」の遷移先を `/org/secretary?project=…` から `/?scope=project:<id>` に変更。途中目標の対話は
  もう「待つ」UI を持たない専用ページではなく Console の SSE で自然に流れてくるため）、
  `app/routes/projects.tsx`（説明文とヒント。「秘書に話しかけても同じです」のリンク先を `/org/secretary`
  から `/`〈Console〉に）、`app/routes/inbox.tsx`（説明文）、`app/components/Flash.tsx`
  （`project_plan` / `milestone_decide` の結果文言 4 箇所）、`app/components/ReportsList.tsx`
  （「担当に話す」リンクを `/org/secretary?project=…` の分岐を無くして単純化。Console の `node:<id>` 範囲は
  案件で絞る仕組みを持たないため、`?project=` は元々効いていなかった）。

### 削除したもの

- `app/components/Conversation.tsx` / `app/celeris/conversation.server.ts` / `app/lib/conversation.ts` /
  `test/unit/conversation.test.ts`: `org.$id.tsx` と `org.secretary.tsx` の両方が Console に置き換わり、
  これらを使う場所が無くなったため削除した（他のどの画面もこれらを import していないことを grep で確認
  済み。`SECRETARY_NODE_ID` 等は `~/lib/console.ts` の `COS_NODE_ID` に引き継いだ）。

### 決めたこと・逸脱（明示）

- **G22-1: 「Node page は同じ部品」を文字どおり取り、専用の CoS ルートを削除した。** ADR-0048 D4 と
  今回の指示は「`/org/{id}` は同じ Console 部品を `scope=node:<id>` で使う（latitude あり）」だったが、
  それを機に旧 `~/routes/org.secretary.tsx`（`Conversation` を使う独立ページ）と
  `~/components/Conversation.tsx`・`~/celeris/conversation.server.ts` を丸ごと削除した。理由: (1) CoS の
  node id は既に `cos`（ADR-0046 D6）で、`/org/:id` は元から `id === "cos"` を素通りで受けていた（`org.tsx`
  の `node.id === "secretary"` 分岐は既に到達しないデッドコードだった）。(2) Conversation の「新しい案件として」
  チェックボックスは、CoS が `propose_project` action を宣言する経路（ADR-0048 D3）と `/projects` の
  新規フォームの二重に機能が被っていた。**`/org/secretary` は 302 リダイレクトとして残した**（旧リンク・
  ブックマークが壊れないように）。
- **G22-2: Console の入力欄が「いま見ている画面の範囲」を既定の宛先にする（ADR-0048 D3 を GUI 側で補う）。**
  `POST /console/instruct` の相手の決め方（§3.107）は「`scope` 省略・`@mention` 無し → CoS（案件に紐づかない）」
  だけで、「いま `/org/coding-poc` を開いているから既定でそのノードへ」という規則は無い。これだと
  ノードの Console ページで無地の文を打つと（意図に反して）CoS へ飛んでしまう。GUI 側で
  `buildInstructBody(text, replyTarget, defaultScope)` の第 3 引数として「範囲が `node:<id>` /
  `project:<id>` に絞られた画面なら、その scope を既定にする」ロジックを足した。ただし
  **`@<node-id> ` で本文が始まるときは `scope` を付けない**（celeris の規則は「`scope` が `@mention` より
  先勝ち」なので、既定の scope を付けると打った `@mention` が無視されてしまうため。優先順位は
  `~/lib/console.ts` の `buildInstructBody` のコメントに明記した）。celeris 側の契約を変える提案ではなく、
  純粋に GUI 側のデフォルト値の付け方の話。
- **G22-3: 認可・途中目標・タスクのコメント／回答は「その場で答える」を React Router のクロスルート
  fetcher で実装し、Console 専用の action は書いていない。** 各ブロックの `fetcher.Form` は
  `action="/approvals"`・`action={`/projects/${projectId}`}`・`action={`/tasks/${taskId}`}` へ直接投げ、
  既存の action（`approval_decide` / `milestone_decide` / `answer` / `comment`）をそのまま再利用する
  （`~/routes/inbox.tsx` に既に同じパターンがある）。celeris への要求の形・検証はすべて既存コードのまま。
- **G22-4: SSE の積み上げは `progress` を run 単位で「置き換え」、それ以外は `cursor` で重複排除、
  上限（既定 300 件）を超えたら古い方から落とす。** D1「Console は progress を run ごとに束ねる」に
  合わせ、同じ `run_id` の `progress` ブロックが SSE で届くたびに一覧内の既存行を置き換える（積み増さない）。
  上限を超えたらブラウザのメモリを無限に増やさないため古い方から捨てる（`GET /console?since=` による
  「もっと見る」的な遡りページングは今回は作っていない。§未解決事項 U1）。
- **G22-5: 「すべて見る」は `progress` ブロックの `first[]`/`last[]` とは独立に、開いたときだけ
  `GET /tasks/{id}/runs/{run_id}/events` を 1 回引く。** `ProgressBlockView` は折り畳み（既定）→ 展開
  （`first`/`last` を表示。追加のネットワークは無し）→「すべて見る」（`useFetcher().load()`。
  `~/components/ReportsList.tsx` の `ReportRow` と同じ「開いたときだけ取りに行く」パターン）の 3 段。
- **G22-6: sticky-to-bottom は「下端から 96px 以内なら自動スクロール」のしきい値で判定。** `scroll` イベントで
  `scrollHeight - scrollTop - clientHeight` を見て、新着ブロックが来たときにその値が閾値未満なら
  `scrollTop = scrollHeight` にする、というだけの単純な実装（DOM 直操作。ライブラリは足していない）。
- **G22-7: 上部の待ち件数の帯は、いま読み込んでいるブロックから数える。** 新しい API 呼び出しは足していない
  （`GET /inbox` 等を別途叩かない）。`question` は `answered=false`、`approval` は `decision` 未設定、
  `milestone` は celeris が `proposed` のものしか流さない（Phase 60a 追記 7）ので出ているだけ数える —
  ただし SSE 接続前（初期表示 100 件）に収まらない古い待ちは数えない（§未解決事項 U2）。
- **G22-8: `knowledge` ブロックは最小限。** ADR-0048 D1 / Phase 60a が「予約・誰も作らない」としている
  とおり、「知識: `<title>`（`<state>`）」の 1 行だけを出す実装にした（過剰投資しない、との指示どおり）。

### 実行したコマンドと出力の要点

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint` | exit 0。`Checked 219 files in ~90ms. No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0（`react-router typegen && tsc -b`、出力なし） |
| test | `pnpm test` | exit 0。**Test Files 59 passed (59) / Tests 836 passed (836)**（フェーズ開始時点の基準 816 件以上。
  新規 5 ファイルで 49 件 + `home.route.test.ts` 差し替えで +5 件、`conversation.test.ts` 削除で -34 件、
  旧 `home.route.test.ts` の -2 件を差し引いて 816 → 836） |
| build | `pnpm build` | exit 0。client / SSR とも成功。新チャンク `Console-*.js`（24.87 kB / gzip 7.33 kB）、
  `console.stream-*.js` / `tasks._id.runs._runId.events-*.js`（resource route、ほぼ空）、`org._id-*.js`・`home-*.js` |
| gen:types（生成が冪等か） | `pnpm gen:types` を 2 回連続 | 2 回とも exit 0。`app/celeris/types.ts` の sha256 が
  2 回目も同一（`fb220503…`）＝**生成は冪等で、手編集していないことの傍証** |
| gen:types と HEAD の差分 | `git diff --exit-code app/celeris/types.ts` | **exit 1（非ゼロ）**。理由は下記「gen:types の差分について」 |
| gui ⇔ celeris の API 文書の同期 | `bash scripts/sync-gui-docs.sh --check`（リポジトリ根） | exit 0。`sync-gui-docs: up to date` |

#### gen:types の差分について（重要。G21 と同じ状況）

`git diff --exit-code app/celeris/types.ts` は非ゼロで終わる。**手で編集したからではない**: このフェーズを
始めた時点で `git status`（リポジトリ根）は `crates/task-api/src/console.rs`・`crates/task-core/src/
console_action.rs`（新規）・`crates/task-core/migrations/0017_console_actions.sql`（新規）・
`docs/api/v1/api-v1.schema.json` など celeris 側 Phase 60b の変更が**未コミットのまま**このワークツリーに
存在しており（celeris 側は「実装済み・テスト済み・コミット準備済み」という前提で渡された）、
`gui/app/celeris/types.ts` も**その未コミットのスキーマから既にこのセッション開始前に再生成された状態**
だった（`ConsoleBlock.actions_result`・`InstructBody`・`ConsoleInstructAccepted`・`MessageMetadata` 等は、
このフェーズの最初の読み取りの時点で既に入っていたことをトランスクリプトで確認できる）。
`git diff`（HEAD 比較）が指しているのは「HEAD＝最後にコミットされた版（celeris 側 Phase 60a まで）」と
「未コミットの Phase 60b 込みの作業ツリー」の差であり、**`pnpm gen:types` を実行する前から存在していた差分**
である。実際に確認したこと:
1. `pnpm gen:types` を連続 2 回実行しても `types.ts` の sha256 は変わらない（決定的な生成）。
2. 差分の中身（`MilestoneId` の並び順の移動を含む）は、このフェーズの最初に `types.ts` を読んだときの内容と
   一致する（新しく増えた行ではなく、HEAD が古いだけ）。
3. `docs/api/v1/api-v1.schema.json` 自体、celeris 側で `git status` に `M` として出ている（未コミット）。

celeris 側の Phase 60b の変更一式（`crates/` と `docs/api/v1/`・`docs/adr/`・`docs/PROGRESS.md`・
`docs/gui/api.md`）がコミットされれば、この差分は自然に解消する。GUI 側では `types.ts` を一切手編集して
いない（`gui/CLAUDE.md` の禁止事項を守っている）ので、このフェーズの範囲では対処のしようがない
（celeris 側のコミットを待つだけ）。

### 未解決事項

- **U1: `GET /console?since=` を使った過去への遡り（無限スクロール的な「もっと見る」）は未実装。**
  初期表示は `GET /console?scope=&limit=100`（既定）だけで、SSE の上限（既定 300 件）を超えて古い方から
  落ちたブロックを画面から再度呼び戻す手段が無い。頻度の高い画面で長時間開きっぱなしにすると、
  古いやり取りは再読み込み（`GET /console` をもう一度呼ぶ）でしか戻れない。
- **U2: 上部の帯の待ち件数は「いま読み込んでいる分だけ」。** 初期表示 100 件・SSE 接続後の積み上げの
  範囲でしか数えないので、それより古い未回答の質問・未決の認可・提案中の途中目標は帯に出ない
  （`/approvals` 等の専用画面では正しく全件出る）。より正確にするなら `GET /inbox` 等を別途叩く必要があるが、
  「新しい API 呼び出しは足さない」方針（G22-7）とトレードオフになるため見送った。
- **U3: e2e は未実行。** タスク指示どおり `pnpm e2e` は走らせていない（実 celeris のバイナリと fake
  ワーカーの起動が要る枝で、時間内に最優先すべきユニット/型/ビルドの緑化を優先した）。特に「実機:
  Console から『〜を直して』と 1 行入れる → CoS の返事と `create_task` → matching → progress → report」
  （ADR-0048 §3 受け入れ条件 5）は e2e か手動確認でしか検証できていない。
- **U4: `@node` 補完はローカルの部分一致だけで、選ばれた候補が実在するかを事前検証しない。**
  celeris 側が 404 `org_node_not_found` を返せば `ErrorFlash` にそのまま出る（GUI 側で二重に判定しない
  という方針どおり）が、候補一覧自体は `GET /org` のスナップショットなので、組織が変わった直後は
  ズレる可能性がある（他の画面の `GET /org` キャッシュと同程度のリスクで、新しい問題ではない）。
- **U5: 途中目標ブロックの「議論」を選んだ後の遷移先を `/?scope=project:<id>` にしたが、旧
  `Conversation` の「考え中」表示（送った直後から返事が来るまでの明示的なインジケータ）に相当するものが
  Console には無い。** Console は SSE で新着が自然に流れてくる前提の UI なので「考え中」を出していない。
  返事が来るまでの間、待っていることが分かりにくい可能性がある（追加のローディング表示は今回は作らなかった）。

### 提案

- **P-G22-1: `docs/gui/api.md` §3.107 に「GUI が既定の `scope` を補うときの優先順位」の注記が欲しい。**
  celeris の規則自体は 1〜4 の順で決定的だが、GUI 側で「いま見ている画面の範囲」を既定にする発想（G22-2）
  が自然に思いつくかどうかは実装者次第。`@mention` が常に最優先であることを明記しておくと、
  次に同じ機能を作る実装者が同じ落とし穴（既定 scope で `@mention` を潰してしまう）を踏まずに済む。
- **P-G22-2: `GET /console` に「もっと古い方へ」ページングする軽い規則が欲しい（U1 の解消）。**
  いまの `since` は「前回の `next_cursor` から前へ」の一方向（新しい方へ）しか想定していないように読める。
  逆向き（`before=` のような）のクエリがあると、SSE で溢れて捨てたブロックを取り戻せる。
- **P-G22-3: `milestone` ブロックに、次の途中目標の提案（`MilestoneView.proposal` 相当）が乗っていない。**
  `docs/gui/api.md` §3.98 の `milestone` は生の `Milestone` + `review?: MilestoneReviewView`だけで、
  `/projects/:id` 側で使っている「次の途中目標の提案」（`proposal.title` / `proposal.description`）に
  相当する情報が無い。Console の `milestone` ブロックのレビューカードは、そのため提案の見出しを
  出していない（`review` の Markdown 本文には書かれているはずだが、構造化はされていない）。

## celeris Phase 62 に合わせた GUI の追従（ADR-0047 D4。2026-09-20）

celeris 側の Phase 62（`docs/PROGRESS.md` の同名節）が Console の予約ブロック `knowledge`
（G22 では「Phase 60a では誰も作らない予約」だった）を埋め、`_inbox` の候補に `op`
（`create`/`update`/`merge`/`retire`）を追加し、タスクのタイムラインに `knowledge` 項目を足した。
GUI 側はこの celeris の変更に**追従しただけ**で、新しい画面・新しい GUI 独自の判断は無い
（celeris が返す形をそのまま出す、という既存の方針のまま）。独立した G フェーズとしては起こしていない
（celeris 側の Phase 62 の一部として実施。番号は振らない）。

- `pnpm gen:types` で `ConsoleBlock::Knowledge`（`task_id`/`task_title`/`run_task_id`/`state`/
  `ingested`/`inbox`/`discarded`）、`TimelineItem::Knowledge`、`KnowledgeCandidate.op` を取り込んだ。
- `app/components/ConsoleBlockItem.tsx` の `KnowledgeBlockView`（Phase 60b / G22 で足した予約の描画）を
  新しい形に合わせて書き換えた:「この仕事から知識 N 件: 取り込み a / 候補 b / 破棄 c」+ 対象タスクへの
  リンク + 候補が 1 件以上あれば `/knowledge/inbox` へのリンク。
- `app/routes/tasks.$id.tsx` のタイムライン（`timelineItemKey`/`TimelineBody`/`TIMELINE_TONE`）に
  `knowledge` の分岐を足した（`scheduled`/`applied`/`failed` で文面を変える）。
- `app/routes/knowledge.inbox.tsx` の候補カードに `op` のバッジ（`~/lib/knowledge.ts` の
  `knowledgeOpTone`）と、`merge`/`retire` のときだけ出るヒント文（`knowledgeOpHint`）を足した。
  出典（`sources`）の `task:<id>` は既存の `KnowledgeSources` がそのままリンクにするので、
  「候補の元になったタスクへのリンク」に新しいコードは要らなかった。
- 証跡: `pnpm lint` / `pnpm typecheck` / `pnpm test`（838 passed。`~/lib/knowledge.ts` の
  `knowledgeOpTone`/`knowledgeOpHint` の単体テストと、mock サーバ経由で `op` が往復することを
  確認する 1 本を追加）/ `pnpm build` はいずれも exit 0。`bash scripts/sync-gui-docs.sh --check` は
  `up to date`。DOM を描画する unit テストは無い方針（G10-U1）のまま、`e2e` は実行していない
  （実 celeris バイナリが要る。既存の理由と同じ）。


### 部署レビュー・デプロイ準備の表示（2026-09-20）

- ADR-0051のdeliveryを変更タブに表示し、部署内レビューから検証済みリリースへの導線と処理中の再検証を追加。CoSの技術レビュー段階は設けない。
- API拡張はRust側の正本・生成schema/typesを更新し同期。既存のデプロイ管理APIを使い、ブラウザへAPIトークンは渡さない。
- lint/typecheck/test（839件）/build成功。`node scripts/check-delivery.mjs` で12状態・幅の表示とデプロイ画面への導線を確認（外部ネットワーク・本番POSTなし）。

## Phase G23 — スマホ UX の機械検査 mobile-audit と最初のラウンド（ADR-0055。2026-09-21）

Phase 69 / ADR-0055 D1（機械検査）と D2（最初のラウンド）。`gui/scripts/mobile-audit.mjs` と
`pnpm mobile-audit` を新規に作り、ADR-0055 D1 が挙げた全画面（20 route、タスクの 5 タブを含む）を
Playwright Chromium（393×851、`deviceScaleFactor 2.75`、Nothing Phone 2a の Chrome UA）で開いて
D1 の 1〜6 を検査し、`test/mobile-audit/report.json` + スクリーンショット（gitignore 済み）に残す。

### 監査の作り

- **`gui/scripts/check-delivery.mjs`（リポジトリ根）の手口を再利用**: `pnpm build` した `server.js` を
  空きポートで `spawn`、偽の celeris を `node:http` で立てて `CELERIS_API_URL` に向け、Playwright で
  操作する。実 celeris は起動しない・外部ネットワークに出ない。
- **`test/mock-celeris/server.ts` の `startMockCeleris` はそのまま使えなかった**: `./fixtures`
  （拡張子なし）の相対 import が vite/vitest の TS 解決の下でしか解決できず、Node 24 の組み込み型剥がし
  （拡張子の補完をしない）では `ERR_MODULE_NOT_FOUND` になる。`fixtures.ts` 自身は celeris の型を
  **type-only** import しているだけなので、そちらは `check-delivery.mjs` と同じ手口で直接 import できる
  （`await import(".../fixtures.ts")`）。ルーティングは `mobile-audit.mjs` の中に素の `node:http` で
  書き直した（`gui/CLAUDE.md` の「GUI から celeris に入る依存は作らない」は変えていない。celeris の型は
  読むだけで、Rust には触っていない）。
- **`gui/tsconfig.node.json` に `"exclude": ["scripts/mobile-audit.mjs"]` を足した**。この 1 ファイルは
  Node（このプロジェクトの `lib: ["ES2023"]`）と、`page.evaluate` へ `toString()` で送るブラウザの関数
  （`document` / `window` / `getComputedStyle` / `Element` が要る）の 2 つの実行環境を意図的に混ぜている
  ので、DOM の lib を足すより対象外にする方が素直と判断した。
- D1 の 6 項目をそれぞれ純関数（`checkOverflow` 等）で実装し、`toString()` を組み立てて
  `context.addInitScript` で毎ページに注入する（`page.evaluate(fn)` は `fn` 単体しか送れないため）。
- **横はみ出し（D1-1）と表（D1-6）の両立**: `overflow-x: auto/scroll` な祖先を持つ要素は横はみ出しの
  対象から外す（D1 本文の「1 と両立」の読み方。表を `overflow-x-auto` の箱に入れる、という D1-6 の狙いが
  D1-1 の「1 件でもあれば落とす」と矛盾しないようにするための、意図した除外）。
- **タップ領域（D1-2）の checkbox/radio**: `~/components/ui/form.ts` の `chipLabelClass` のように
  `<label>` で包んで大きく押せるようにする作りが既にあるので、`<input>` 自身ではなく包んでいる
  `<label>` の大きさで判定する（`<input>` 単体が小さいこと自体は違反にしない）。
- **状態バッジ（D1-3）**は `data-status-badge` を付けた要素だけを見る（`~/components/ui/badge.tsx` の
  `StatusBadge` と、`projects.tsx` / `projects.$id.tsx` の案件・途中目標バッジに付けた）。
- **文字（D1-4）**は `font-mono`（id / sha / パス）を除外し、直接テキストを持つ末端要素だけを見る。

### 直した内容（優先順位: 横はみ出し → 状態バッジ → タップ領域 → 文字）

1. **横はみ出し（38 → 0）**: `~/components/ui/card.tsx` の `CardHeader` の `actions`（バッジの並び）が
   `shrink-0` で折り返さず、`/releases` の 4 つ並ぶバッジ（位置・gate・検証・sensitive）が 393px を
   突き破っていた。`actions` を `w-full flex-wrap`（`sm:` 以上は `w-auto shrink-0` で従来どおり）にした。
2. **状態バッジの 1 語化**: 調べた結果、`~/lib/labels.ts` の `taskStatusLabel` / `projectStatusLabel` /
   `milestoneStatusLabel` は元から 1 語（例: 進行中・完了・一時停止）だった。機械検査に「歯」を持たせる
   ため `data-status-badge` を実際のバッジ要素に付けて回った（違反 0 のまま、今後の回帰を検査できるように
   した）。加えて、ADR の元の人の指摘（「done/running 以外の詳細な説明」）に最も近い実物だった
   `~/components/task-changes.tsx` の「デプロイの状態」カードの見出し（celeris が返す `state` ごとに
   「部署内レビュー・マージ判定中」等の**説明文そのもの**を見出しにしていた）を、1 語バッジ
   （レビュー中・待機中・取込中・検証中・準備完了・要確認）+ `title` 属性の全文 + 本文の 1 行に分けた
   （D2「状態はバッジ 1 語 + 色。理由・詳細は行の下か開閉に」）。
3. **タップ領域（279 → 94）**: 44×44 未満が広い範囲に散っていたのは、共有部品の高さが軒並み 44px
   未満だったため（1 箇所直すと何十件も減る）:
   - `~/components/ui/button.tsx` の `SIZES`（`xs`/`sm`/`md`）を、モバイルはすべて `h-11`、
     `lg:` でデスクトップは元の `h-7`/`h-8`/`h-10` に戻すようにした。
   - `~/components/ui/form.ts` の `inputClass`/`selectClass`（`h-9` → `h-11 lg:h-9`）、
     `chipLabelClass`（checkbox/radio を包むチップ。`min-h-11 lg:min-h-0` を足した）。
   - `~/root.tsx` のロゴ `<a>`（`min-h-11` を足した）と `~/components/HelpLink.tsx`
     （見出し横の「?」リンク。`size-6` → `size-11 lg:size-6`）。
   - `~/routes/help.tsx` の目次（同じ行に並ぶチップのリンク群）は D1-2 の例外そのものなので
     `data-touch-ok` を付けた（44px に拡げるのではなく、意図的な例外として扱った）。
4. **文字（457 → 200）**: 12px（`text-xs`）・11.2px（`text-[0.7rem]`）が広い範囲に散っていたのも
   共有部品由来だったので、モバイルは `text-sm`、`lg:` でデスクトップは元のサイズに戻す形で直した:
   `~/root.tsx` の footer（`celeris_version` 等の `dt` ラベル）・ナビの見出し（業務/裏方/ヘルプ）・
   `ConnectionPill`、`~/components/ui/misc.tsx` の `DataItem` の `dt`、`~/components/ui/form.ts` の
   `hintClass`（80 箇所で使われている）。

### 新しい純関数と単体テスト

- `~/lib/format.ts`（新規）: `shortId(id, tailLength=8)`（長い id/sha を末尾で省略。ADR-0055 D2）、
  `truncateLabel(text, maxLength=40)`（長い見出しを省略）。どちらも celeris への値は変えない表示専用。
  `~/routes/tasks.$id.tsx` の run 一覧の `run_id` 列に適用（`font-mono text-xs break-all` + `title`）。
- `test/unit/format.test.ts`（新規、6 件）。

### 証跡

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint` | exit 0。`Checked 222 files in ~90ms. No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0（`mobile-audit.mjs` は `tsconfig.node.json` から意図的に除外。理由は上記） |
| test | `pnpm test` | exit 0。**Test Files 60 passed (60) / Tests 855 passed (855)**（前回 839 + 新規 `format.test.ts` 6 件 + 既存の増分） |
| build | `pnpm build` | exit 0（client / server とも） |
| mobile-audit（初回。UI を直す前、mock のバグを直した直後） | `MOBILE_AUDIT_SKIP_BUILD=1 node scripts/mobile-audit.mjs` | exit 1。**736 件**（`overflow` 38 / `tap-target` 279 / `font-size` 457 / `status-badge` 0）。20 route 全て 200 応答 |
| mobile-audit（最初のラウンド後） | `pnpm mobile-audit` | exit 1。**294 件**（`overflow` **0** / `status-badge` **0** / `tap-target` **94**（-68%） / `font-size` **200**（-56%））。20 route 全て 200 応答、`page-error` 0 |

#### 違反数の推移（初回 → 最初のラウンド後。D3 の「ラウンドごとに 1 行」の記録）

| rule | 初回 | 最初のラウンド後 | 減った理由（主なもの） |
| --- | --- | --- | --- |
| overflow | 38 | **0** | `~/components/ui/card.tsx` の `CardHeader.actions` を折り返しに（すべて `/releases`）|
| status-badge | 0 | 0 | 元から労働の対象（`labels.ts`）は 1 語だった。`data-status-badge` を実物に付けて検査に「歯」を持たせただけ |
| tap-target | 279 | 94 | `~/components/ui/button.tsx`（`SIZES`）・`~/components/ui/form.ts`（`inputClass`/`selectClass`/`chipLabelClass`/`hintClass`）・`~/root.tsx`（ロゴ `<a>`）・`~/components/HelpLink.tsx`・`help.tsx` 目次（`data-touch-ok`）と、checkbox/radio は包む `<label>` で判定するよう検査自体も直した |
| font-size | 457 | 200 | `~/root.tsx`（footer の `dt` / ナビ見出し / `ConnectionPill`）・`~/components/ui/misc.tsx`（`DataItem` の `dt`）・`~/components/ui/form.ts`（`hintClass`）。いずれもモバイルは `text-sm`、`lg:` でデスクトップは元のサイズに戻す作り |

（途中の値をコミットごとに記録してはいないので、行ごとの正確な増減の内訳はこの表の「主なもの」欄が言葉で示す以上には残していない。次のラウンド以降は 1 回直すたびに `pnpm mobile-audit` の数字をここに積む。）

### 未解決事項（次のラウンドへ。ADR-0055 D3 は「やることが無くなりにくいので最後に回し続ける」と
明記しているので、0 を最終形とはみなさない）

- **U1: タップ領域の残り 94 件の多くは、文中の生のテキストリンク**（例: Console ブロックの「〜を見る」
  リンク、`help.tsx` の本文中のリンク）。行の高さがそのまま当たり判定になっており、44px にするには
  行間・パディングを個別に見直す必要がある（`data-touch-ok` を機械的に付けるのは筋が違うので避けた）。
- **U2: 文字の残り 200 件は分散している**（最頻出は "1"/"0"/"-" のような値そのものが小さいフォントで
  出る箇所や、案件題名の 2 次的な参照など）。共有部品側の大きな塊は今回の 3 箇所（footer・ナビ見出し・
  `DataItem`/`hintClass`）でほぼ払底したので、残りは画面ごとに 1 つずつ見ていく必要がある。
- **U3: 固定要素（D1-5）・表（D1-6）は今回 0 件のまま**（違反していない）だが、D2 が求める
  「下部固定のタブ（Console / ボード / 案件 / 認可 / その他）」はまだ入れていない
  （`~/root.tsx` のモバイル用ナビは今も**上部** sticky。固定要素チェックは通っているので機械検査上は
  問題ないが、D2 の具体的な指示とは異なる。次のラウンドの候補）。
- **U4: e2e（`pnpm e2e`）は未実行**（実 celeris が要る既存の理由のまま。`mobile-audit` は別の仕組み）。

### 提案

- **P-G23-1: D1-5（固定要素）の検査は「一番下までスクロールしてから」の 1 点しか見ていない。**
  実際のスマホでは、キーボード表示時に `100dvh` の入力欄が動く挙動（ADR-0055 D2）まではカバーできない
  （Playwright はソフトキーボードを再現できない）。次のラウンドで Console の入力欄を作り込むときは、
  手元の実機での目視確認も添える方が安全（`docs/PROGRESS.md` の「LLM 呼び出しを伴う実機確認」と同じ扱い）。

## Phase G24 — スマホ UX ラウンド 2: 下部固定タブと違反ゼロ（ADR-0055。2026-09-21）

Phase 70 / ADR-0055 D2（残タスク）と D3（ラウンドを回し続ける）。前回（Phase G23）の「未解決事項」
U1・U2・U3 に手を付けた: D2 が求める下部固定タブを実装し、タップ領域・文字サイズの違反を
20 route 全部で 0 にした。celeris・Rust 側は変更していない（`gui/CLAUDE.md` の境界どおり）。

### 下部固定タブ（ADR-0055 D2 の具体化）

- **`~/root.tsx` を大きく組み替えた**: `Sidebar`（デスクトップ専用、`hidden lg:block` の `<aside>`）、
  `MobileTopBar`（モバイルのみ、ロゴ + 接続状態 + ログアウト。`lg:hidden`）、`MobileTabBar`（モバイルの
  下部固定タブ。`fixed inset-x-0 bottom-0 lg:hidden`）の 3 つに分けた。以前は 1 つの `<aside>` が
  `sticky top-0` のまま、モバイルは内側の要素を `lg:hidden`/`order-*` で出し分けていた
  （「上部 sticky」。U3 で指摘した形）。
- **タブは 4 本 + その他**（`MOBILE_TABS`）: Console / ボード / 案件 / 認可。認可には
  `approvalsPending` のバッジ（赤丸のみ、文字は出さない。文字を出すと D1-4 の 14px 検査に掛かるため）。
- **「その他」は下からせり出すシート**（`MOBILE_OTHER`）: 組織・報告・リリース・知識・クラスタ・
  アカウント・ヘルプの 7 つ（人が言った並びどおり）。閉じている間はシートの DOM 自体が無い
  （`{sheetOpen && (...)}`）ので機械検査に影響しない。開くボタン・背景（`<button>` にして a11y の
  「静的要素にクリックだけ付けている」警告を避けた）・Escape（`document` に `keydown` を張る）で閉じる。
- **入力欄との関係（ADR-0055 D2「入力欄は画面下固定…隠れない」）**: Console の入力欄自体は今回も
  `position: fixed` にしていない（既存のまま、メッセージ一覧の下の通常のフローの要素）。今回の要求は
  「下部固定タブより上に来る・内容が隠れない」ことなので、本文側のラッパーに
  `pb-28 lg:pb-0`（112px。タブの高さ `h-16`=64px + 余裕）を足し、D1-5 の検査（後述の直し込み後）が
  20 route 全部で 0 のままであることで確認した。タブは
  `style={{ paddingBottom: "env(safe-area-inset-bottom)" }}` で実機の safe area 分も確保する
  （Playwright の headless では 0 になるので機械検査では見えない。Phase G23 の提案 P-G23-1 のとおり、
  実機の目視確認が要る項目として下記「未解決事項」に残す）。
- 新しいアイコン `more`（3 点）を `~/components/ui/Icon.tsx` に追加。

### 監査スクリプト自身の 2 つの見落としを直した（下部固定タブを初めて作ったことで発覚）

下部固定タブができるまで `position: fixed` な要素が画面に存在しなかったため、D1-5（固定要素）の検査
（`checkFixedOverlays`）は常に `bottomBars.length === 0` で早期リターンし、**一度も実際には動いていな
かった**。タブを作った直後に初めて動き、以下 2 つの誤検知を見つけて直した（`~/scripts/mobile-audit.mjs`）。

1. **`<aside class="hidden lg:block">`（デスクトップ専用）の中身を見落とす**: `display` は継承しない
   ので、祖先が `display:none` でも子要素自身の `getComputedStyle().display` は変わらない
   （`"block"` のまま）。一方 `getBoundingClientRect()` は祖先が非表示ならボックスを持たず 0 になる。
   D1-4（文字サイズ）等の検査は「自分の `style.display` だけ」を見ていたので、デスクトップ専用ナビの
   中の小さい文字（ナビ見出し等）を「見えている」と誤検知していた。祖先をたどって判定する
   `isNotVisible(el)` を新設し、D1-1・D1-2・D1-4・D1-5 のすべてに適用した。
2. **閉じた `<details>` の中身が消えない**: `<details>`（`tasks.$id.tsx` の「生のイベント」）が閉じて
   いても、Chromium は中身（`<summary>` 以外）の `getComputedStyle().display` を `"none"` に**しない**
   （実測で確認。UA の見せ方は `display` プロパティには出ない別の仕組みらしい）。そのため
   `isNotVisible` の祖先チェインだけでは検知できず、`task-timeline` で D1-5 が 1 件誤検知した
   （閉じた `<details>` の中の「ありません。」という文字の `getBoundingClientRect().bottom` が実在の
   値を返し、タブの上端より下にあると判定された）。`isNotVisible` に「直接の親が閉じた `<details>`
   で、自分が `<summary>` でない」という構造的な判定を追加した。
- この 2 点の副作用として、D1-4（文字サイズ）の違反数もラウンド開始直後の 200 → 168（見落とし 1 を
  直した時点） → 162（見落とし 2 を直した時点）と、UI を 1 行も直さずに減った（デスクトップ専用要素・
  閉じた `<details>` の中の小さい文字が正しく対象外になったため）。

### タップ領域・文字サイズを 0 にした（優先順位: Console → ボード → タスクの各タブ → 案件詳細 → 残りの全画面）

共有部品を先に潰すと 1 回の変更で何十件も減る（Phase G23 と同じ考え方）。主な直し方:

- **`~/components/ui/badge.tsx`**（`Badge`/`KindBadge`/`RoleLabel`/`GenreLabel`）: モバイル
  `text-sm`（`Badge` は `leading-5`）、`lg:` で元の `text-xs`/`text-[0.7rem]` に戻す。案件名・状態・
  役割・分野など、ほぼ全画面のバッジがこれで一度に直った。
- **`~/components/ui/misc.tsx`**: `SectionTitle` の件数ピル、`PageHeader` の `eyebrow` を同じパターンで。
- **`~/components/ui/form.ts`**: 新しい `touchLinkClass`（孤立した文中リンクの当たり判定を 44 に広げる。
  `-my-2.5 inline-flex min-h-11 min-w-11 items-center py-2.5`。見た目の行間は相殺で変えない）。
  `theadClass` も `text-sm lg:text-xs` に。ADR-0055 D1-2 の「同じ行の隣接リンク群」の例外
  （`data-touch-ok`）とは別物: `help.tsx` の目次のような隣接リンク群にはラウンド 1 のとおり
  `data-touch-ok` を使い、孤立した 1 本のリンク（Console のタスクへのリンク、案件・タスク画面の
  「〜へ」リンク等）には今回作った `touchLinkClass` を使う（**ADR の「文中リンクを機械的に免除しない」**
  を守り、当たり判定を実際に広げた。免除では済ませていない）。
- **`~/components/Console.tsx` / `~/components/ConsoleBlockItem.tsx`**: `WaitingStrip` の文字、
  8 種のブロック（task/progress/question/approval/milestone/report/knowledge/human/reply）すべての
  小さい文字（`text-xs`/`text-[0.7rem]`）を `text-sm lg:text-xs`（or `lg:text-[0.7rem]`）に、孤立リンク
  （タスク題名・知識の候補リンク等）に `touchLinkClass`、progress の折りたたみボタンに `min-h-11`。
- **`~/routes/tasks.$id.tsx`**: `TaskTabs`（5 つのタブ）に `min-h-11` を追加（5 タブ × 5 画面 = 25 件が
  1 回の変更で消えた）。担当・案件・親タスクへのリンク、タイムラインの各行、`TaskRefList` 等に
  `touchLinkClass`/`text-sm lg:text-xs`。
- **`~/routes/board.tsx`**: カード題名のリンクを `min-h-11` の行に、列の件数ピル・カードの属性行を
  `text-sm lg:text-xs`、行内編集の 3 つの `<select>`（優先度・レベル・担当）を
  `h-11 lg:h-7 text-sm lg:text-xs`（元は `h-7 text-xs` の完全固定で、モバイルでも 28px のままだった
  のが根本原因）。
- **`~/routes/projects.$id.tsx` / `~/components/ReportsList.tsx`**（`/reports` と `/projects/:id` の
  「報告」タブの共有部品）/ `~/routes/projects.tsx` / `~/routes/projects.$id.docs.tsx`: 同じパターン
  （小さい文字・孤立リンク・行内編集 `<select>`）。
- 残りの画面（`org.tsx`・`approvals.tsx`・`reports.tsx`・`knowledge.tsx`・`knowledge.inbox.tsx`・
  `KnowledgeMeta.tsx`・`accounts.tsx`・`help.tsx`）も同じパターンで潰し、最後に `help.tsx` の目次・
  本文中の孤立リンク（17 件、最後に残っていた塊）を `touchLinkClass` で直して合計 0 にした。
  `accounts.tsx` の 2 つの `<pre>`（config.toml の例）は `font-mono` が付いていなかった不備だったので
  付けた（コード例は等幅にするのが元々の意図で、D1-4 の対象外になるのは副作用ではなく正しい直し方）。

### D2 の規律を守ったこと

- 幅の固定値（`w-[...px]`）は追加していない。`min-h-11`/`min-w-11` はタップ領域そのものの最小値
  （D1-2 が要求する値）で、レイアウト幅の固定ではない（既存の `chipLabelClass`/`Button` と同じ考え方）。
- 状態はバッジ 1 語のまま変えていない（`data-status-badge` の対象は今回増やしていない）。
- 既存の vitest はすべて green のまま。新しい**純関数**は今回作っていない（`touchLinkClass` はクラス名
  の文字列定数で関数ではない。監査スクリプトの `isNotVisible` は `mobile-audit.mjs` 自身の検査ロジック
  で、`~/lib/*.ts` の対象ではない）ので、新規のユニットテストは無し。

### 証跡

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint` | exit 0。`Checked 222 files. No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0 |
| test | `pnpm test` | exit 0。**Test Files 60 passed (60) / Tests 856 passed (856)** |
| build | `pnpm build` | exit 0（client・server とも） |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（celeris API 契約は変えていない） |
| mobile-audit（ラウンド開始時。Phase G23 の続き） | `node scripts/mobile-audit.mjs` | exit 1。**294 件**（tap-target 94 / font-size 200） |
| mobile-audit（下部タブ実装直後） | 〃 | exit 1。**263 件**（tap-target 94 / font-size 168 / **fixed-overlay 1**。新規） |
| mobile-audit（検査スクリプトの 2 つの見落としを直した後） | 〃 | exit 1。**197 件**（tap-target 94 / font-size 103 / fixed-overlay **0**） |
| mobile-audit（Console 一式） | 〃 | exit 1。**159 件**（tap-target 84 / font-size 75） |
| mobile-audit（`tasks.$id.tsx` 一式） | 〃 | exit 1。**101 件**（tap-target 45 / font-size 56）。task-overview/timeline/changes/files/artifacts の 5 route が 0 に |
| mobile-audit（`board.tsx`） | 〃 | exit 1。**73 件**（tap-target 40 / font-size 33）。board が 0 に |
| mobile-audit（案件系一式） | 〃 | exit 1。**53 件**（tap-target 30 / font-size 23）。projects/project-detail/project-docs が 0 に |
| mobile-audit（org/approvals/reports/knowledge/knowledge-inbox/accounts） | 〃 | exit 1。**17 件**（tap-target 17 / font-size **0**） |
| mobile-audit（`help.tsx`。最終） | `pnpm mobile-audit` | **exit 0。violations 0 件**。20 route 全て 200 応答、`page-error` 0 |

#### 違反数の推移（D3「ラウンドごとに 1 行」の記録。ラウンド 1 後 → ラウンド 2 後）

| rule | ラウンド 1 後 | ラウンド 2 後 | 減った理由（主なもの） |
| --- | --- | --- | --- |
| overflow | 0 | 0 | 変更なし |
| status-badge | 0 | 0 | 変更なし |
| fixed-overlay | 0（未検査） | 0 | 下部固定タブで初めて検査が動き、監査スクリプト自身の 2 つの見落とし（`isNotVisible`）を直して 0 に |
| tap-target | 94 | **0** | `TaskTabs`（`min-h-11`）、`touchLinkClass`（新規）、board の行内編集 `<select>`、各画面の孤立リンク |
| font-size | 200 | **0** | `Badge`/`RoleLabel`/`GenreLabel`/`SectionTitle`/`PageHeader` の共有修正で大半が一括で消え、残りは画面ごとに `text-sm lg:text-xs` |

### 未解決事項（ADR-0055 D3 は「やることが無くなりにくいので最後に回し続ける」と明記しているので、
0 を最終形とはみなさない。次にやること）

- **U1（G23 から継続、実質解消）**: 文中の孤立リンクは今回 `touchLinkClass` で当たり判定を実際に
  広げた（免除ではない）。新しい画面・新しいリンクを足すときは、単独のリンクなら `touchLinkClass`、
  同じ行の隣接リンク群なら `data-touch-ok` を使う、という 2 つの型が揃ったので、次のラウンドはこれに
  従うだけで良い。
- **U2（G23 から継続、解消）**: 文字サイズの残りは共有部品（Badge・SectionTitle・PageHeader・
  form.ts）でほぼ払底し、今回さらに個々の画面を全部見たので現在 0。今後増える画面・部品が
  `text-xs`/`text-[0.7rem]` を新しく書いたときに再発するので、レビューで見る必要がある
  （機械検査が捕まえるので気づける）。
- **U3（G23 から継続、解消）**: 下部固定タブを実装した。
- **U4（G23 から継続）**: e2e（`pnpm e2e`）は今回も未実行（実 celeris が要る既存の理由のまま）。
- **U5（新規）**: Console の入力欄自体はまだ `position: fixed` にしていない（本文の通常のフロー）。
  今回の受け入れ条件（下部タブより上・内容が隠れない）は `pb-28` の余白で満たしたが、ADR-0055 D2 の
  「入力欄は画面下固定」という文字どおりの形にはまだしていない。次のラウンドで Console の入力欄を
  実際に画面下へ固定するなら、`100dvh` とキーボード表示時の挙動を含めて実機の目視確認を添えること
  （P-G23-1 のとおり。Playwright は software keyboard を再現しない）。
- **U6（新規）**: 「その他」シートは 7 項目を 2 列グリッドで出す最小限の作りで、検索や並び替えは無い。
  項目が増えたら見直しが要る。

### 提案

- **P-G24-1**: `mobile-audit.mjs` の `isNotVisible` は今回の 2 件（`hidden lg:block` の祖先・閉じた
  `<details>`）はカバーしたが、他にも「`getComputedStyle` では見えるが実際には描かれない」ケース
  （`content-visibility: hidden`、`clip-path`、画面外への `transform` 等）は理論上あり得る。今のところ
  実害が出ていないので追加の一般化はしていないが、次に誤検知が出たら同じ考え方（構造的にたどる）で
  直すのが良い。
- **P-G24-2**: `touchLinkClass` は「見た目の位置を変えずに当たり判定だけ広げる」ための負のマージン
  トリックなので、隣接する要素とレイアウト上重なる場合（詰まった行）は当たり判定が視覚的な境界を
  超えて隣の要素に食い込むことがある。今回はそれが実害になる詰まった行は見当たらなかったが、次の
  ラウンドで新しい画面に使うときは実機かブラウザの開発者ツールで確認するとよい。

## Phase G25 — スマホ UX ラウンド 3（磨き。ADR-0055。2026-09-21）

Phase 71 / ADR-0055 D2（規律）・D3（ラウンドを回し続ける）。機械検査（`pnpm mobile-audit`）は
ラウンド 2 で違反 0 になったので、今回は数字ではなく「デザイナーの目」で画面ごとの階層・余白・
タイポグラフィ・1 画面 1 主操作を見直した（`artifact-design` skill のガイド：余白・階層・トークンに
倣う。celeris・Rust 側は変更していない）。

### Console（`/`）

- **入力欄を画面下固定にした**（ADR-0055 D2「入力欄は画面下固定、キーボード表示時に隠れない」。
  Phase G24 の未解決事項 U5 の解消）。`~/components/Console.tsx` の `ConsoleInput` の外枠を
  `fixed inset-x-0 bottom-16 z-20 ... lg:static`（下部固定タブ `h-16` の直上。タブ自身が
  `env(safe-area-inset-bottom)` を確保しているので入力欄側では重ねて確保しない）にし、
  `lg:` はこれまでどおりの通常フローに戻す。フローから外れた分の高さは、`BlockStream` の直後に置いた
  `aria-hidden` の spacer（`h-52 lg:hidden`）で確保する（要素を複製せず 1 つの `ConsoleInput` のまま。
  `~/scripts/mobile-audit.mjs` の D1-5（固定要素）検査で、実機を想定した「一番下までスクロールしても
  内容が隠れない」ことを確認済み）。
- **CoS 側の吹き出しに「誰・いつ」の帯を先頭に付けた**（human は右寄せ、それ以外は左寄せ。新設の
  `BlockHeader`）。これまでは「誰」を先頭、「いつ」を末尾に別々に置く・置かないブロックが混在していた
  （reply は先頭に名前だけ、milestone は無し、report/knowledge は末尾だけ）ので、8 種類の吹き出し全部を
  同じ形（アイコン + 誰 + いつ、を先頭の 1 行）に揃え、本文中に「いつ」が重複する行を削った。
- **task ブロックをカード化した**（ADR-0054 D2「作ったタスクは task ブロックとして返事の直下に出る」の
  見た目）。状態は `StatusBadge`（`task.to`。1 語 + 色、ADR-0055 D1-3 のまま）にし、`project`
  バッジではなく本文行に統合、`task_id` は `shortId` で末尾表示 + `title` 属性で全文（ADR-0055 D2
  「id は末尾だけ、全文は title」）。
- 人の発言（`HumanBlockView`）も同じ帯の形にし、返信ボタンを本文の下（右寄せ）に移した。

### Board（`/board`）

- **モバイルは segmented control で列を 1 つずつ見る方式を選んだ**（ADR-0055 D2「横スワイプの行、
  または segmented control。どちらか選んで理由を書く」）。理由: 6 列（待ち/進行中/止まっている/完了/
  失敗/中止）を横スワイプのカード列にすると、列の切れ目が分かりにくく誤ってスワイプで隣の列の
  カードを操作してしまう事故が起きやすいと判断した。segmented control は「今どの状態を見ているか」が
  常に文字で分かり、絞り込みフォームの「選ぶ」操作（既にこの画面にある `<select>` 群）と操作感が
  揃う。`md:` 以上（タブレット・デスクトップの既存グリッド）は変更していない。
  実装は `~/components/Console.tsx` の `ScopePicker` と同じ「`md:block` で常に表示、それ未満は
  state で表示を切り替える」作り（要素は複製しない）。ピル行は `sticky top-14`（モバイル上部帯の
  すぐ下）で、列を切り替えても常に見える。
- **カードを「折り目の上」だけに絞った**: 題名・状態（1 語）・優先度・担当だけを常に出し、種類・
  レベル・途中目標・ラベル・行内編集（優先度/レベル/担当の `<select>`）は「種類・レベル・ラベルを
  見る」の開閉トグルに移した（`lg:` は常に開いたまま、既存の見た目を変えない）。

### Task（`/tasks/<id>`）

- **タブを横スクロールのピル行にした**（`overflow-x-auto`、折り返さない。`lg:` は元の折り返し行の
  まま）。5 タブ（概要・タイムライン・変更・ファイル・成果物）が 393px に収まらないときは端が切れて
  スクロールで見せる（D1-1/D1-6 の「overflow-x-auto の箱」と同じ扱い）。
- **メタデータ（基本情報・タイマー）の `DataList` を 2 列既定にした**。`~/components/ui/misc.tsx` の
  `DataList` の既定は `sm:`（640px）未満は 1 列なので、393px の実機では常に 1 列だった。この画面だけ
  `className` で `grid-cols-1 min-[374px]:grid-cols-2 sm:grid-cols-2 lg:grid-cols-4` に上書きし、
  374px 以上の実機は 2 列、それ未満のごく狭い端末だけ 1 列に戻す。
  - **この変更で `~/components/ui/misc.tsx` の `DataItem` の `dt`（ラベル）に横はみ出しが 1 件出た**
    （`consecutive_reviewer_requeues` のように区切りの無い長い 1 語の API フィールド名を、狭い列幅に
    詰めると折り返せず突き破っていた）。`dt` に `break-words` を足して直した（`dd` は元から
    `break-words` を持っていたが `dt` には無かった）。これは `DataItem` 全体に効くので、他の画面の
    ラベルにも今後同じ種類の語が来ても安全になった副次効果がある。
- **操作（承認・回答・中止など）を画面下の全幅ボタンにした**。以前は `max-w-xs`/`max-w-sm` が
  ブレークポイント無しで常に効いていたため、モバイルでも 320px 止まりだった（`w-full` と併記されていて
  も `max-w-xs` が勝っていた）。`sm:` からだけ効くように直し、モバイルは各操作カードを縦に積む
  （`flex-col`）。approve/reject/answer/cancel/retry が同時に候補になる状態（例: approve と reject が
  両方出る）では、どちらも等しく「今この場面の答え」なので、どちらか一方だけを残して他を隠すことは
  しなかった（隠すとむしろ選べなくなる）。

### Project（`/projects/<id>`）・Approvals（`/approvals`）

- **Approvals**: 未決の要求カードの 3 つの決定ボタン（今回だけ／今後ずっと／認めない）のうち、最も
  選ばれやすい「今後ずっと」を主役にして先頭・全幅にした（`order-*` はモバイルだけ、DOM の順序
  自体は変えていないのでキーボード操作の順は変わらない）。「今後ずっと」の範囲（`<select>`）と
  書き方の説明は、別々だった 2 つの `<details>` を 1 つ（「「今後ずっと」の範囲・書き方」）にまとめた
  （secondary actions in a details disclosure。既定値 `node` は開かなくても効く）。
- **Project**: 案件・途中目標のライフサイクル操作（一時停止/再開/中止/アーカイブ/アーカイブ解除）は
  もともとカードの上に平積みだった。一時停止・再開・中止はこのカードの主役の操作として直接出す
  （モバイルは縦積み全幅、`sm:` から元どおりの横並び）。使う頻度が低いアーカイブ／アーカイブ解除は
  「その他の操作」の `<details>` に畳んだ。**この 1 点はデスクトップの見た目も変わる**（アーカイブは
  今までは常に見えていたが、今は開かないと見えない）。この画面はすでに「タスクを追加」
  （`AddTaskForm`）・「状態を直接変える（裏方）」がブレークポイントに関係なく常時 `<details>` に
  畳まれている作りなので、それと同じ考え方（頻度が低い操作は開閉に）に揃えた方が画面全体で一貫すると
  判断した。案件・途中目標カードは元々 `<Card>`/`<li>` で縦積みだった（この点は変更なし）。

### 監査（機械検査。数値は変えていないことの確認）

| rule | ラウンド 2 後 | ラウンド 3 後 |
| --- | --- | --- |
| overflow | 0 | **0**（維持。タスク画面のメタデータ 2 列化で 1 件出たが `DataItem.dt` の `break-words` で直した） |
| status-badge | 0 | **0**（維持。Console の task ブロックに `StatusBadge` を新設したが 1 語のまま） |
| fixed-overlay | 0 | **0**（維持。Console の入力欄を新たに `position: fixed` にしたが、spacer で確保できている） |
| tap-target | 0 | **0**（維持） |
| font-size | 0 | **0**（維持） |
| 合計 | 0 | **0**（`pnpm mobile-audit` exit 0） |

### 証跡

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint` | exit 0。`Checked 222 files. No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0 |
| test | `pnpm test` | exit 0。**Test Files 60 passed (60) / Tests 856 passed (856)**（既存のまま。今回は新しい
  純関数を作っていない — レイアウト・CSS のみの変更のため、新規ユニットテストは追加していない。
  Phase G24 の `touchLinkClass` と同じ扱い） |
| build | `pnpm build` | exit 0（client・server とも） |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（celeris API 契約は変えていない） |
| mobile-audit | `pnpm mobile-audit` | **exit 0。violations 0 件**。20 route 全て 200 応答、`page-error` 0 |

before/after のスクリーンショット（`gui/test/mobile-audit/*.png`、git には入れない。差分の目視用）:
`gui/test/mobile-audit/home.png`（Console）、`board.png`、`task-overview.png`、`approvals.png`、
`project-detail.png` の 5 枚で今回の変更を確認した（他 15 route は変更していないが同じ実行で
再生成されている）。

### 未解決事項

- **U7（新規）**: Console 入力欄の spacer（`h-52`）は実測ではなく見積もり（返信先バナー表示時の
  最大想定高さ）。`pnpm mobile-audit` の D1-5 検査（返信バナー非表示の既定状態）では 0 件だが、
  返信バナー表示時に spacer が実際に足りているかは実機・ブラウザの開発者ツールで確認していない
  （ResizeObserver で動的に高さを測る方法もあるが、今回は複雑さを避けて見積もりにした）。
- **U8（新規、P-G23-1 の系譜）**: 入力欄を `position: fixed` にしたことで、ADR-0055 D2「キーボード
  表示時に隠れない」（`100dvh`）の本来の対象になった。Playwright はソフトキーボードを再現できない
  ため、実機（iOS Safari / Android Chrome）でキーボードを開いたときに入力欄が隠れないかは
  未確認（手順は P-G23-1 のとおり、人またはネットワーク・実機が使える環境のエージェントに依頼）。
- **U9（新規）**: Project 画面のアーカイブ／アーカイブ解除を `<details>` に畳んだ変更は、デスクトップ
  でも「常時表示 → 開閉」に変わる（他の画面の項目は `lg:`/`sm:` で戻すのに対し、この 1 点だけは
  ブレークポイントに関係ない）。既存の `AddTaskForm`/`milestone-status-details` と同じ規律に揃えた
  判断だが、人が「アーカイブは今までどおり常に見せたい」と言うなら次のラウンドで戻す。
- **U10（新規）**: Board の segmented control は 6 択の水平スクロールなので、6 番目（中止）が画面外に
  隠れがち（スクロールしないと見えない）。よく使う列（進行中・止まっている）が左側に来るよう
  `BOARD_COLUMNS` の並び順に依存している（並び順自体は変えていない）。
- **U11（新規）**: MilestoneReviewPanel（ok/議論/ng の 3 ボタン）・MilestoneLifecycleActions と
  AddTaskForm・状態変更 `<details>` が同じカードに同時に出るケースは、まだ縦に長い（1 画面 1 主操作
  には全面的には揃っていない）。次のラウンドの候補。

### 提案

- **P-G25-1**: Console 入力欄の spacer を実測にする（`ResizeObserver` で `ConsoleInput` の実高さを
  測り、spacer の `style.height` に反映する）と、返信バナーの有無に関わらず正確に隙間を詰められる。
  今回は見積もりの `h-52` で 0 違反を確認できたので優先度は低いが、次に Console の入力欄の形を
  変えるときはこの手当てもセットにするとよい。
- **P-G25-2**: Board の segmented control を「横スワイプのカード列」に変える場合（人がそちらを好むなら）、
  `scroll-snap-type` と各カードの `min-w-full` で作れる。今回選んだ segmented control との使い勝手の
  比較は実機の人の声を待つ。
## Phase G27 — LLM source の可視化とクラスタのトンネル表示（ADR-0053 D4、celeris Phase 66。2026-09-21）

celeris 側（`docs/PROGRESS.md` の Phase 66）が D3（Qwen トンネルを celeris が張る）・D4（`GET
/llm/sources` の拡張）を実装したのを受け、GUI 側の受け入れ条件を実装した。`GET /llm/sources` と
`GET /clusters` の新フィールドはどちらも既存エンドポイントの追加フィールドなので、新しい画面は
作らず既存の `/accounts`・`/clusters` に節を足した。celeris の crate には触れていない
（`gui/CLAUDE.md` の境界どおり）。番号は G25/G26 が別ラウンドで使用済みのため G27。

### 実装

- **`app/lib/llm-sources.ts`（新規）**: 表示専用の純関数だけを集めた（`sourceLabel` / `sourceStatusWord`
  / `formatRemaining` / `tierResolutionLabel` / `tierLabel` / `cooldownRemainingLabel` /
  `isAccountCoolingDown` / `forwardStatusWord`）。celeris が返した値をそのまま整形するだけで、
  選択・到達性・残量の判断はしない（celeris が既に決定済み。ADR-0055 の既存方針と同じ）。
- **`/accounts`**: `loadAccounts` が `GET /llm/sources` を追加で呼ぶ（`AccountsData.llmSources` /
  `llmSourcesUnavailable`（409 `llm_proxy_unavailable`）/ `llmSourcesError`）。`[llm_proxy]` 未設定でも
  `/accounts` 全体は壊れない（`secrets` と同じ 3 分岐: エラー / 未設定 / 表示）。「LLM source」節
  （`LlmSourcesSection`）: `celeris/<tier>` の解決先を `<dl>` で 3 tier 分、供給元ごとにカード
  （`LlmSourceCard`。一語の状態バッジ `data-status-badge="llm-source"` = `reachable` /
  `unreachable` / `enabled` / `disabled`、直近 1 時間の要求/token 数）、アカウントごとの行
  （`LlmAccountRow`。`shortId` で id を短縮、短期/長期の残量 %、cooldown 中なら残り時間のバッジ）。
- **`/clusters`**: `tunnel_login_needed` が立っているクラスタに `Alert tone="danger"` で
  「ログインが必要（TOTP）」を**全文**で出す（バッジには入れない。状態バッジの 1 語規約はトンネルの
  `up`/`down` バッジ側にだけ適用）。`tunnel_forwards[]` は 1 本ごとに `listen → target` と一語の状態
  バッジ（`data-status-badge="tunnel"` = `up` / `down` / `unknown`）。
- 通知の種類が 1 つ増えた（`cluster_login_needed`。celeris ADR-0037/ADR-0053 D3）ので、
  `app/celeris/types.ts` の `NotificationKind` に追従し、`app/lib/notify.ts` の
  `NOTIFY_KIND_LABEL`（「クラスタのログインが必要（TOTP）」）と `notifyTargetHref`（`/clusters` へ）に
  足した（`/reports` の Discord 区画がこの型を直接使っているため）。

### 検証

- `pnpm gen:types`: `docs/api/v1/api-v1.schema.json`（celeris 側で再生成済み）からの差分は
  `NotificationKind` に `cluster_login_needed` を追加、`ClusterView`/`ClusterConfigView` に
  `tunnel_forwards`/`tunnel_login_needed`/`forwards`、`ClusterForwardView`（新規）、
  `LlmSourcesView` に `celeris_tiers`、`LlmCelerisTierView`（新規）、`LlmSourceAccountView` に
  `remaining_short`/`remaining_long`。再実行しても差分ゼロ（安定）。
- `pnpm vitest run test/unit/llm-sources.test.ts`: **17 passed**（`sourceLabel` の 3 マッピング、
  `sourceStatusWord`/`forwardStatusWord` の一語・12 字以内保証を含む、`formatRemaining` の丸め・
  非数の扱い・範囲外のクランプ、`tierResolutionLabel`、`cooldownRemainingLabel`、
  `isAccountCoolingDown`）。
- `test/unit/notify.test.ts`: `cluster_login_needed` を `KINDS` に追加、`notifyTargetHref` の新しい
  ケース（`/clusters` へのリンク）のテストを追加。
- `pnpm lint` / `pnpm typecheck`: exit 0。
- `pnpm test`: **61 files / 877 passed**（新規 17 件を含む）。
- `pnpm build`: exit 0。
- `scripts/sync-gui-docs.sh --check`（celeris 側から実行）: up to date（`docs/gui/api.md` §3.23 /
  §3.108 / §3.64〜3.65 の更新分を反映）。
- `pnpm mobile-audit`: **`{"ok": true, "total": 0, "by_rule": {}}`**（393×851、Nothing Phone 2a UA、
  20 route）。フィクスチャに `pegasus`（`auth: "totp"`、`tunnel_login_needed: true`、
  `tunnel_forwards`）と `GET /llm/sources`（claude-oauth 1 アカウント・openai-compatible:qwen
  unreachable・tier 解決 3 件）を追加した上での結果。
  - **before/after**（Phase G24 の main、`git worktree add --detach` で `4b3e7ec` を別ディレクトリに
    checkout して同じ監査を実行): before も `{"ok": true, "total": 0}`（tap-target 0 / font-size 0）。
    今回の追加後も `total: 0`。overflow・status-badge・fixed-element も両方 0（そもそも新しい
    バッジは `data-status-badge` を付けて 1 語に収めた: `reachable`/`unreachable`/`enabled`/
    `disabled`/`up`/`down`/`unknown`、すべて 12 字以内・空白なし）。
  - 実装の途中で `LlmAccountRow` の残量表示（`text-xs`）と `TunnelForwardRow` の見出し（`text-xs`）が
    font-size 違反を一時的に増やした（`text-sm`/`text-sm` へ修正して解消）ことを、`report.json` の
    差分で確認した。最終的にゼロを維持した。

### 未解決事項

- **実機のトンネル切断/復帰の見た目は未確認**（celeris 側 Phase 66 の未確認事項と同じ。本物の
  pegasus/bnode150 が無いため）。`data-status-badge="tunnel"` のバッジが実際に `up`→`down`→`up` と
  切り替わるのを実機で確認すること。
- G23 の U1（孤立リンクの当たり判定）・U5（Console 入力欄の真の固定）・U6（「その他」シートの拡張）は
  今回のスコープ外（celeris 側の依頼どおり LLM source/クラスタの表示に限定した）。

### 提案

- `LlmSourceCard`/`LlmAccountRow` は `/accounts` に埋め込んだが、供給元の数が増えると縦に長くなる。
  celeris 側の運用で供給元が 4〜5 を超えるようなら、専用タブ（Console の下部タブに準じる形）への
  切り出しを検討してよい（今回は 2〜3 供給元想定のフィクスチャなので据え置いた）。

## Phase G26 — スマホ UX ラウンド 4（ADR-0055。2026-09-21）

Phase 72 / ADR-0055 D2（規律）・D3（ラウンドを回し続ける）。celeris・Rust 側は変更していない
（`crates/` 無変更）。`accounts.tsx` / `clusters.tsx` / `app/celeris/types.ts` /
`test/mock-celeris/*` の accounts・clusters・llm-sources ハンドラは触っていない（並行して別エージェントが
編集中のため）。今回は Phase G25 の未解決事項 U7・U10・U11 の解消と、`/org`・`/org/<node>`・
`/reports`・`/knowledge`・`/knowledge/inbox`・`/releases`・`/help` の D2 磨き、その他の画面の
タイポグラフィ・省略表示の仕上げを行った。

### U7（Console 入力欄の spacer を実測にした）

- `~/components/Console.tsx` の `ConsoleInput` に `rootRef` を足し、`ResizeObserver` で自分の
  border-box の高さ（`entry.borderBoxSize[0].blockSize`、無ければ `entry.contentRect.height`）を
  測って `onHeightChange` で親（`Console`）へ渡すようにした。親は `inputHeight` state を持ち、
  spacer（`aria-hidden` の `div`）の `style.height` に反映する。返信先バナーの表示・非表示や、
  テキストエリアの行数変化にも `ResizeObserver` が追随するので、見積もりの `h-52`（13rem）は
  「初回描画・SSR・`ResizeObserver` が無い環境（テストの `environment: "node"` 等）のフォールバック」
  としてだけ残した（クラス名はそのまま、インライン `style` が実測値で上書きする）。
  P-G25-1 で提案した対応そのもの。
- `ResizeObserver` はブラウザにしか無いので `typeof ResizeObserver === "undefined"` で早期リターンし、
  vitest（`environment: "node"`）はこの副作用を素通りする（新しい DOM テストは要らない。既存のとおり
  レイアウト部品には unit テストを持たせない方針のまま）。

### U10（Board の segmented control を横スクロールから 3×2 のグリッドにした）

- `~/routes/board.tsx` の `board-column-picker` を、`flex gap-2 overflow-x-auto`（6 択を横スクロール、
  6 番目が隠れがちだった）から `grid grid-cols-3 gap-1.5`（393px で 3 列 × 2 行、6 つ全部が一度に
  見える）に変えた。ラベルが狭い列幅で折り返しても横はみ出しにはならない（縦に伸びるだけ）ので
  D1-1 と両立する。念のため各ボタンに `title={label}` も付けた（U10 の選択肢のもう一方「短いラベル +
  title」も部分的に取り入れた形）。`md:` 以上（列グリッドがそのまま出る）は変更していない。

### U11（task/milestone カードを「主役 1 つ + 残りは開閉」に寄せた）

- **Board のカード（`BoardCard`）**: 既に Phase 71 で「題名・状態・優先度・担当だけ折り目の上、
  種類・レベル・途中目標・ラベル・行内編集は開閉」になっていた（主役の操作は題名のリンク 1 つ）ので
  変更なし。確認のみ。
- **途中目標のカード（`~/routes/projects.$id.tsx` の `MilestoneLifecycleActions`）**: 「一時停止／再開」
  は途中目標カードの主役の操作として常に出すが、確認も要り頻度も低い「中止」を `<details>`
  （summary「その他の操作（中止）」）に畳んだ。中止・一時停止・再開は同じ 1 つの `fetcher`
  （`milestone-lifecycle-${id}`）のままで、確認ダイアログ（`milestone-cancel-confirm`）もその
  `<details>` の中に移した。Project 画面のアーカイブ（Phase 71、U9）と同じ「低頻度操作は開閉に」の
  規律に揃えている。**この 1 点はデスクトップの見た目も変わる**（U9 と同じ理由の判断）。
  `MilestoneReviewPanel`（ok/議論/ng）は途中目標が判定待ちのときの主役の操作なのでそのまま常時表示。

### `/org`・`/org/<node>` の磨き

- `~/routes/org.tsx` の `OrgTreeItem` に、サブツリーごとの開閉ボタン（子がいるノードだけ、既定は
  開いた状態 = 挙動を変えない）を足し、「木を字下げした一覧として、サブツリーごとに開閉できる」形に
  した（大きな組織で 1 画面が長くなりすぎるのを防ぐ）。
- ノードの行に、`GET /org` の `effective_profiles[]`（根→葉で継いだ結果。GUI は継承を再計算しない）
  から「既定のハーネス」（`harness_default`、1 行のテキスト）と「動かす場所」（`run`。`host`/`container`
  を `profileRunLabel` で「ホスト」/「コンテナ」の 1 語にし、`Badge` + `data-status-badge="org-mode"`
  で出す）を添えた。値が無いノードには何も出さない（既定は空表示のまま）。
- `/org/<node>`（`~/routes/org.$id.tsx`）は `Console` 部品をそのまま使うだけの画面なので、U7 の
  Console 側の直しがそのまま効く。この画面自体のコードは変更していない。

### `/reports` の磨き

- `~/components/ReportsList.tsx` の `ReportRow`（depth 0）を、字下げだけの行から
  `rounded-lg border border-border bg-surface` のカードにした（「一覧はカード（縦積み）」の規律。
  差し込みの元報告 `depth > 0` は今までどおり `border-l` の字下げのまま区別する）。悪い知らせの
  赤枠はそのまま優先される。
- 展開した本文の下に `id: {shortId(report.id)}`（`font-mono text-xs break-all`、`title` に全文）を
  足した（ADR-0055 D2「id は末尾だけ、全文は title」。今まで報告の id を画面上で参照する手段が
  無かった）。

### `/knowledge`・`/knowledge/inbox` の磨き

- 検索結果（`?q=` のとき）の `PageLink` に `card` オプションを足し、枠付きのカード（題名 +
  `font-mono text-xs break-all` のパス + タグのチップ）にした。ツリーの並び（`groups` の下の一覧）は
  今までどおり軽いナビ行のまま（意図的に変えていない。ナビとしての密度を保つため）。検索欄
  （`knowledge-search`）は既に `inputClass`（`w-full`）なので変更なし。
- 知識ベースのパス（`~/components/KnowledgeMeta.tsx` の `Mono` によるページのパス、
  `~/routes/knowledge.inbox.tsx` の候補のパス、`~/routes/knowledge.tsx` の置き場のルート）に
  `break-all` を足した（ADR-0055 D2「パスは font-mono + break-all」。モックのパスは短く監査には
  出なかったが、長い置き場でも折り返せるようにする直し）。

### `/releases` の磨き

- gate バッジ（`gate ✓`/`gate ✗`）と検証バッジ（`検証済み（ライブ引き継ぎ）` 等）が「1 語」ではなかった
  （空白・括弧を含む）ので、`~/lib/releases.ts` に新しい純粋関数
  `releaseGateBadgeLabel`（「通過」/「失敗」）と `releaseVerifyBadgeLabel`（`unverified`→「未検証」、
  `ok_live`/`ok_stop_start`→どちらも「検証済み」、`ng`→「検証NG」。切替方法の違いは色と行の下の
  詳細に任せる）を追加した。既存の `releaseGateLabel`/`releaseVerifyLabel`（詳細な文言）は削除せず、
  バッジの `title` と「検証」`DataItem` の下の 1 行（`release-verify-detail`）に残したので情報は
  失っていない。バッジには `data-status-badge` を新たに付けた（今まで対象外だったので `pnpm
  mobile-audit` の D1-3 検査が新たに効くようになった）。
- 「upgrade」ボタン（`release-promote`。`canPromote` の `<details>` の中、= 出ているときは常に
  押せる）に `w-full sm:w-auto` を足し、モバイルでは全幅にした（「押せるときだけ全幅」の要件）。
- `item.sha12` は元から 12 文字の短い sha（設計名のとおり）なので、これ以上の省略はしていない。

### `/help` の磨き

- 目次（`nav`）は Phase 61（G21？）から既にピル行（`rounded-lg border ... px-2.5 py-1.5` を
  `flex flex-wrap` で並べる、`data-touch-ok`）だったので変更なし。
- 「画面ごとの説明」「用語集」「困ったとき」の説明文（`dd`）に `leading-relaxed` を足し、長い日本語の
  説明文の読みやすさ（行間）を上げた（タイポグラフィの仕上げ）。

### 画面共通のタイポグラフィ・省略表示の仕上げ（項目 3）

`truncate` を使っていて `title` の無かった箇所に `title` を足した（ADR-0055 D2「省略した表示は
`title`/コピー用の要素で全文」・項目 3「無いまま truncate しない」）:

- `~/components/task-files.tsx`（ファイルツリーの `entry.name` × 3 箇所）
- `~/components/task-changes.tsx`（変更ファイルの `file.path`。ついでに、この行のバッジが
  `{file.status} {changedFileStatusLabel(file.status)}`（例: 「A 追加」）と 2 語になっていたのを
  `changedFileStatusLabel(file.status)` だけの 1 語にし、生の git 記号は `title` に移した）
- `~/components/ProjectIntegrations.tsx`（PR/取り込み行のタスク題名リンク）
- `~/components/ArtifactsList.tsx`（`sources.json` のリンク集の題名）
- `~/routes/tasks.new.tsx`（`depends_on` の候補一覧の題名）
- `~/components/ConsoleBlockItem.tsx`（report ブロックの案件名バッジ）

`~/routes/tasks.tsx` の一覧行の題名リンクは既に `title={item.id}`（id を出す設計）が付いているため
そのまま。`ConsoleBlockItem.tsx` の `who`（発言者名。`ReactNode` で文字列とは限らない）は対象外
（短い名前がほとんどで、`title` に渡せる保証もない）。

### 監査（機械検査。数値は 0 を維持したことの確認）

| rule | ラウンド 3 後 | ラウンド 4 後 |
| --- | --- | --- |
| overflow | 0 | **0**（維持） |
| status-badge | 0 | **0**（維持。releases の gate/verify バッジに `data-status-badge` を新設したが 1 語のまま、org のモードバッジも 1 語） |
| fixed-overlay | 0 | **0**（維持。Console の spacer を実測にしても 0 のまま） |
| tap-target | 0 | **0**（維持。Board の segmented control を grid にしても各セル `min-h-11` のまま） |
| font-size | 0 | **0**（維持） |
| 合計 | 0 | **0**（`pnpm mobile-audit` exit 0、20 route 全て 200 応答） |

### 証跡

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint` | exit 0。`Checked 222 files. No fixes applied.`（一度 biome の整形差分が 2 件出たため `biome check --write` で直してから再確認） |
| typecheck | `pnpm typecheck` | exit 0 |
| test | `pnpm test` | exit 0。**Test Files 60 passed (60) / Tests 857 passed (857)**（Phase G25 の 856 から +1。`releaseGateBadgeLabel`/`releaseVerifyBadgeLabel` の新規ユニットテスト 1 件を `test/unit/releases.test.ts` に追加） |
| build | `pnpm build` | exit 0（client・server とも） |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（celeris API 契約は変えていない） |
| mobile-audit | `pnpm mobile-audit` | **exit 0。violations 0 件**。20 route 全て 200 応答、`page-error` 0 |

before/after のスクリーンショット（`gui/test/mobile-audit/*.png`、git には入れない。差分の目視用）:
`gui/test/mobile-audit/home.png`（Console）、`board.png`、`org.png`、`reports.png`、`knowledge.png`、
`knowledge-inbox.png`、`releases.png`、`help.png`、`project-detail.png` で今回の変更を確認した
（他 route は変更していないが同じ実行で再生成されている）。

### 未解決事項

- **U8（継続、未実機）**: Phase G25 の入力欄 `position: fixed` 化にともなう「キーボード表示時に
  隠れないか」（`100dvh`）は Playwright では再現できないため、今回も未確認のまま。実機（iOS Safari /
  Android Chrome）が使える人またはエージェントに依頼する（手順は P-G23-1 のとおり）。
- **U9（据え置き）**: Project 画面のアーカイブ（Phase 71）に加え、今回 Milestone の「中止」も
  デスクトップの見た目が変わる開閉化をした。人が「常に見せたい」と言えば、この 2 点をまとめて
  次のラウンドで戻す。
- **U12（新規）**: `/org` のサブツリー開閉は「既定で開いている」ので、大きな組織（数十ノード）での
  効果は開閉した後にしか出ない。効果を確認するには実機か、モックの組織データを増やした検証が要る
  （今回のモックデータは小さいノード数なので、開閉自体の動作は確認できたが「長い画面が短くなる」
  効果は目視で確認できていない）。
- **U13（新規）**: `/releases` の gate/verify バッジは新たに `data-status-badge` を付けたが、
  `ok_live`/`ok_stop_start` を同じ「検証済み」の 1 語にしたことで、バッジの文字だけでは切替方法が
  分からなくなった（色と、下の「検証」欄の詳細行でしか分からない）。人が「バッジのままで見分けたい」
  と言うなら、色の凡例をどこかに置くか、アイコンで区別する案を次のラウンドで検討する。

### 提案

- **P-G26-1**: `/org` の階層バッジ（既定のハーネス・動かす場所）は `effective_profiles` の継承結果を
  そのまま出しているので、根に近いノードほど同じ値が並びがち（例: 全ノードが根の既定を継いでいると
  全部同じバッジになる）。差分（自分の代で上書きしたかどうか）を出す案は次のラウンド。
- **P-G26-2**: U7 で `ConsoleInput` の実測に切り替えたことで、今後 Console の入力欄の形（例: 添付
  ボタンの追加等）を変えても spacer 側は自動で追随する。この仕組み（`ResizeObserver` +
  `onHeightChange`）は他の「モバイルで固定表示にしている要素」（例えば将来ボードの segmented
  control を `sticky` から `fixed` にする等）にも転用できる。

## Phase G28 — Console のチャット吹き出し・「新しい会話」・部門長の継続セッション表示（ADR-0054 D3、celeris Phase 68。2026-09-21）

celeris 側（`crates/task-api`/`task-ops`/`task-worker`/`celerisctl`）と同じ Phase の GUI 側。celeris が
`ConsoleBlock::Reply` に `state`/`thinking`/`steps[]` を、`OrgList` に `lead_sessions[]` を足したのを
受けて、GUI を対応させた。celeris 側の詳細は `docs/PROGRESS.md`（このリポジトリの親）の
`## Phase 68`、ADR は `docs/adr/0054-stateful-sessions-and-streaming-chat.md`（celeris 側）を参照。

### 受け入れ条件ごとの結果

1. **育つ吹き出し**: `app/components/ConsoleBlockItem.tsx::ReplyBlockView` が `block.state ===
   "streaming"` のとき、BlockHeader の横にパルスする丸のインジケータ（`console-reply-streaming-dot`）、
   「考え中…」の 1 行（`console-reply-thinking`。`block.thinking` が空ならプレースホルダ文言）、
   `steps[]` の行（新規 `ReplyStepRow`。`tool_use` は `[tool] 要約`、`tool_result` は `→ 要約`、
   `error` ならバッジと背景色。`ProgressLineRow` と同じ見た目）、本文（`MarkdownViewer`）の順に描く。
   `state === "done"`（既定）は従来どおり確定した吹き出しのまま（actions_result・run へのリンク・
   返信ボタンも従来どおりそこだけに出す。育つ最中は「返信」を出さない — 育っている返事に「返信」しても
   まだ実体の `message_id` が無いため）。
2. **SSE の積み上げ規約**: `app/lib/console.ts::appendConsoleBlock` を、`reply` かつ `run_id` があるとき
   `run_id`/`task_id` が一致する既存の吹き出しを探し、**`state === "streaming"` のときだけ**
   `text` を連結・`steps` を積み増す・`thinking` は届いた値が非 null のときだけ置き換える、という
   規約にした（celeris の SSE は「その接続で見た増分だけ」を送るため。docs/PROGRESS.md Phase 68 の
   「Phase 68 追記」2 参照）。`state === "done"` の `reply` は確定した本文そのものなので**置き換える**
   （育つ最中の `thinking`/`steps` を残さない）。
3. **「新しい会話」**: `app/routes/console.new-conversation.ts`（新規リソースルート、`POST
   /console/new-conversation` の中継。`~/routes/logout.ts` と同じ作り、画面は持たない）。
   `Console.tsx` の `NewConversationButton`（`window.confirm` で確認。ブラウザ以外では確認せずそのまま
   送る。`~/components/ProjectRepos.tsx` の「削除」と同じ作り）。celeris 側の応答が 204・本文なしなので、
   `CelerisClient.post`（`delete` にしか無かった 204 の扱いを追加。`app/celeris/client.server.ts`）。
   `ConsoleNewConversationOutcome`（`app/celeris/action-types.ts`）。
4. **「この案件の文脈で話す」**: 既存の scope 機構（`?scope=project:<id>` のときの既定の返信先。
   Phase G22 から機能）を変えず、`ConsoleInput` に明示のラベル（`console-scope-context` バッジ）を
   追加しただけ（返信先バナーが出ているときは規則が競合しないよう出さない）。
5. **部門長の継続セッション**: `/org?selected=<department>` の `OrgNodeDetail` に「継続中のセッション:
   turns / tokens / 最終使用」（`org-node-lead-session`）を、`GET /org` の `lead_sessions[]` から
   `node_id` で対応づけて出す（無いノードには出さない。`app/routes/org.tsx`）。CoS は対象外（Console
   のチャット欄自身が状態を見せるため）。
6. **fixture / テスト**: `test/mock-celeris/fixtures.ts` に `consoleReplyBlock()`（確定した `reply`）と
   `consoleGrowingReplySteps()`（1 本の run が thinking → tool_use → text → 確定、の 4 段で育つ様子を
   4 件の `ConsoleBlock` として持つ）を追加。`test/unit/console.test.ts` に
   `appendConsoleBlock`（SSE の積み上げ）の新規テスト 3 本（増分の積み上げ／`state=done` での置き換え／
   `consoleGrowingReplySteps()` を順に足すと 1 つの吹き出しに育つことを通しで検査）。G10-U1 の方針
   （DOM を描画する unit テストは置かない）どおり、育つ様子の検証は純粋関数（`appendConsoleBlock`）の
   レベルで行い、`consoleBlocks()`（Console の既定モック）には混ぜていない（後述の未解決事項）。

### 監査（機械検査。数値は 0 を維持したことの確認）

| rule | ラウンド 4 後（Phase G26） | G28 後 |
| --- | --- | --- |
| overflow / status-badge / fixed-overlay / tap-target / font-size | 0 | **0**（維持） |
| 合計 | 0 | **0**（`pnpm mobile-audit` exit 0、21 route） |

`/org?selected=coding`（部門長の継続セッション表示）を新しく監査対象に加えた（ADR-0055 D1 の元の
一覧には無い、Phase 68 の新しい UI のための追加。`gui/scripts/mobile-audit.mjs` の `ROUTES` に
`org-detail` を追加し、`org` の mock に `lead_sessions` を足した）。この route を初めて監査したことで、
Phase 68 以前から潜在していた違反（`OrgNodeDetail` の「追加・削除は「認可」から」リンクが
`tap-target`（144.8×16 < 44×44）・`font-size`（12px < 14px）の両方に違反）が見つかったので、
`touchLinkClass` + `text-sm` に直した（`app/routes/org.tsx`）。Phase 68 の新規コード（`console-reply-*`）
自体は違反 0 だった（既存の `BlockShell`/`ProgressLineRow` と同じスタイルの土台を再利用したため）。

### 証跡（コマンドと出力の要点）

- `pnpm gen:types && git diff --exit-code app/celeris/types.ts`: 差分あり（`ConsoleBlock.reply` の
  `state`/`thinking`/`steps`、`ConsoleReplyStep`、`OrgList.lead_sessions`、`NodeSessionSummary` が
  追加。celeris のスキーマ変更をそのまま反映。再生成しても安定することを 2 回連続実行で確認）。
- `pnpm typecheck` exit 0。
- `pnpm lint` exit 0（`biome check --write` で import 順・フォーマット差分を自動修正してから確認）。
- `pnpm test` exit 0（**881 passed / 61 files**。Phase G26 の 878 から +3）。
- `pnpm build` exit 0（client・server とも）。
- `pnpm mobile-audit` exit 0（**違反 0 件**。21 route、`org-detail` を新規追加）。

### 未解決事項

- **U-G28-1（実機未確認）**: 実際の celeris + claude-code/codex/acp に対して、スマホ幅で CoS に 1 往復し、
  「考え中…」→ tool call の行 → 本文の順に同じ吹き出しが育ち、`actions` を出したときに `task` カードが
  返事の直下に出ることを目視で確認していない（認証・ネットワークが使えるサンドボックスではないため。
  celeris 側 ADR-0009 P-34 と同じ扱い）。
- **U-G28-2**: 育つ返事の fixture（`consoleGrowingReplySteps()`）を Console の既定モック
  （`consolePage()`/`consoleBlocks()`）には混ぜていない。混ぜると `checkFixedOverlays`（D1-5）が
  `console-stream`（内側で `overflow-y-auto` する箱）の中身を文書全体のスクロールでしか判定できず、
  実際にはスクロールで届く末尾要素を偽陽性で「固定バーに隠れている」と報告する（`mobile-audit.mjs` の
  既知の限界。Phase 68 で実際に踏んだ）。次にこの検査を触る Phase で、内側の `overflow-y-auto` 要素も
  末尾までスクロールしてから判定するよう `checkFixedOverlays` を直すことを提案する（P-G28-1）。
- **U-G28-3**: 「継続中のセッション」の表示形式（`3 turns ・ 12,345 tokens ・ 最終使用
  2026-09-21T01:00:00Z`）は RFC 3339 の生文字列のまま（`console` の他のブロックの `at` 表示と同じ
  流儀に合わせた）。人が読みやすい相対時刻（「3 分前」等）にする専用の整形は無い（既存の
  `~/lib/time-delta.ts` は経過秒数の差分表示だけで、絶対時刻→相対時刻の変換関数が無いため今回は
  追加しなかった）。

### 提案

- **P-G28-1**: 上記 U-G28-2 のとおり、`checkFixedOverlays` に「`overflow-y: auto` な子孫要素もその
  scrollHeight まで一時的にスクロールしてから判定する」処理を足すと、Console のような内側スクロール
  領域を持つ画面の検査精度が上がる。
- celeris 側の提案（ACP の読み取り道具の許可が fail-closed であること等）は
  `docs/adr/0054-stateful-sessions-and-streaming-chat.md`（celeris 側）の「Phase 68 追記」を参照。

## Phase G29 — スマホ UX ラウンド 5: 流れるチャットの磨き（ADR-0055 D2/D3、Phase 73。2026-09-21）

Phase G28（Phase 68 の GUI）で作った「育つ吹き出し」を、Claude Code / Codex のチャット欄に近い見た目へ
磨いた。celeris・Rust 側は変更していない（`crates/` 無変更）。ワークツリーの分岐点が Phase 68（`45a0dfa`）
より前だったため、まずそのコミットの差分だけを取り込んでから着手した（詳細は `docs/PROGRESS.md`
「Phase 73」の「作業の前提」を参照）。

### 1. チャット吹き出し（393px。U1 相当・受け入れ条件 1）

- **`~/components/ConsoleBlockItem.tsx::ReplyStepRow`** を作り直した:
  - `tool_use` は道具名を `<span className="font-semibold">` で太字にし、要約は `~/lib/format.ts::
    truncateLabel(step.text, 90)` で省略、`title` に全文（省略していないときも `title` は常に全文を
    持つ。押しても壊れない）。
  - `tool_result` は既定で畳む: 新しい純粋関数 `~/lib/console.ts::firstLine`/`hasMoreThanFirstLine`
    で「1 行目だけ」を `<summary>` に見せ、改行を含む（＝ 1 行目の裏に何かある）ときだけ `<details>`
    にする（1 行しか無い結果に空の開閉矢印を出さない）。開くと `<pre className="whitespace-pre-wrap
    [overflow-wrap:anywhere]">` で全文。
  - どちらも `[overflow-wrap:anywhere]`（Tailwind の任意プロパティ）にした。`break-words`
    （`overflow-wrap: break-word`）は「はみ出したときだけ折る」ので、詰まった flex 行では
    min-content の計算に効かず横はみ出しを起こしうる。`.markdown`（Phase 68 で既に
    `overflow-wrap: anywhere` になっていた）と同じ規約に揃えた。
- **「考え中…」のパルス**: 既存の `animate-pulse` に `motion-reduce:animate-none` を足した
  （`prefers-reduced-motion: reduce` で止まる。CSS だけ、JS の分岐は無い）。`.markdown` の本文は
  Phase 68 から既に `line-height: 1.75` なので「comfortable line-height」は元から満たしていた。
- **自動追従 + 「最新へ」ピル**: `~/lib/console.ts::shouldStickToBottom(scrollHeight, scrollTop,
  clientHeight, threshold=96)`（新規の純粋関数。既存の `stickyRef` の中の計算式をそのまま切り出した
  だけ）。`~/components/Console.tsx::BlockStream` はこれを `state` としても持ち（`stuck`）、
  追従していない（人が上にスクロールして読んでいる）間だけ、下端に戻るピル
  （`console-jump-to-latest`、`min-h-11` の丸ボタン）を出す。押すと最下部へスクロールし、
  追従を再開する。

### 2. 人の吹き出しと入力欄（受け入れ条件 2）

- 右寄せ・時刻表示は Phase 71 の `BlockHeader`（`align="end"`）がそのまま満たしていた（変更なし）。
- **送信待ちヒント**: 新しい純粋関数 `~/lib/console.ts::hasStreamingReply(blocks)`
  （`state === "streaming"` の `reply` が 1 件でもあるか）を `Console` → `ConsoleInput` に
  `streaming` prop として渡し、`true` のとき「送信待ち（前の run が終わってから）」
  （`console-queue-hint`）を「送る」ボタンの横に出す。**送信自体は無効化しない**（ADR-0054 D2
  「入力欄は run 中も打てる」。キューそのものは既存の直列化（Phase 27 監査 M-3 の `depends_on`）が
  担うので、ここでは状態を伝えるだけ）。
- **「新しい会話」を overflow メニューへ**: `NewConversationMenu`（新規）。`lg:` は従来どおり
  `NewConversationButton` をインラインで表示。モバイルは `size-11` の「⋯」ボタン
  （`console-overflow-trigger`）を押すと `role="menu"` のパネル（`console-overflow-menu`）が開き、
  その中に「新しい会話」がある（`~/root.tsx` の `MobileOtherSheet` と同じ「背景の透明ボタンで閉じる」
  作り）。これで Console の主役のボタンは「送る」1 つだけになる。`NewConversationButton` に
  `onSubmitted` を足し、押す（確認 OK 後）とメニューが閉じる。

### 3. 組織ノード画面: lead_sessions をカードに（受け入れ条件 3）

`~/routes/org.tsx` の `OrgNodeDetail`。以前は 2 列の `dl`（`grid-cols-2`、`sm:` 以上でしか
`col-span-2` が効かない）の中に「turns ・ tokens ・ 最終使用」を 1 行のテキストで詰め込んでいたため、
393px では実質 1 列（~180px 幅）に窮屈に収まっていた。独立した `rounded-lg border` のカード
（`org-node-lead-session`）に出し、3 つの値を `flex flex-wrap` で並べる（狭ければ折り返すだけで
横はみ出さない。`最終使用` は `min-w-0 flex-1` + `break-words` で長い ISO 時刻でも安全）。

### 4. U13: リリースの検証バッジにアイコン（Phase G26 の未解決事項）

`~/lib/releases.ts::releaseVerifyIcon`（新規の純粋関数。`ReleaseVerifyState → IconName | null`）:
`ok_live` は `zap`（⚡）、`ok_stop_start` は `rotate`（⟳）、`unverified`/`ng` はアイコン無し
（バッジの文字だけで意味が通るため）。`~/routes/releases.tsx` の `release-verify` バッジに、色（既存の
`releaseVerifyTone`）に加えてこのアイコンを添えた。文字は変えていない（`releaseVerifyBadgeLabel` は
そのまま「検証済み」の 1 語）。

### 5. U12: 組織の開閉トグルが実際に畳む中身を持つように

`gui/test/mock-celeris/fixtures.ts::orgList()`（新規のエクスポート）と `orgLeadSessions()`
（付随する `lead_sessions`）: `cos`（secretary）→ `coding`（department）→ `coding-poc`（section。
既存の他 fixture が `assignee`/`selected` として参照する id なので変えていない）の下に、
`coding-poc-alpha` → `coding-poc-alpha-1` → `coding-poc-alpha-1-x` の 3 段（+ 兄弟
`coding-poc-beta`）を追加。`research` 部・`research-survey` 課も足した。

- **`gui/scripts/mobile-audit.mjs`**: 以前はここに 3 ノードだけのその場限りの木を書いていたが、
  `fx.orgList()` を使うよう差し替えた。これで `/org` の開閉トグル（フェーズ 72）が実際に 4 世代ぶんの
  サブツリーを畳めることをスクリーンショット（`test/mobile-audit/org.png`/`org-detail.png`）で
  確認できる。
- **`test/unit/org-tree.test.ts`**: `buildOrgTree(orgList().items)` が `cos → coding → coding-poc →
  coding-poc-alpha → coding-poc-alpha-1 → coding-poc-alpha-1-x` の 5 世代を正しく組むことを確認する
  テストを追加（`buildOrgTree` 自体の再帰はすでに汎用だったので新しいバグは無かったが、「深い
  サブツリーが実際に組める」ことを固定データで留めておく）。

### 6. Phase 68 の fixed-overlay 偽陽性（U-G28-2 の解消、P-G28-1 の実装）

`gui/scripts/mobile-audit.mjs::checkFixedOverlays`（D1-5）に、判定の前に「`overflow-y: auto/scroll`
かつ実際にスクロールできる（`scrollHeight > clientHeight`）祖先をすべて一時的に末尾までスクロールし、
判定が終わったら元の位置に戻す」処理を足した（`window.scrollTo` は文書全体にしか効かず、
`console-stream` のような**それ自身がスクロールする箱**の中身は動かせなかったため）。ルールそのもの
（「固定要素より下に来てはいけない」）は緩めていない — 判定の入力を実機に近づけただけ。

これが効いていることを実際に確かめるため、`gui/scripts/mobile-audit.mjs` の `/api/v1/console` モックに
育つ返事のスナップショット（`fx.consoleGrowingReplySnapshot()`。新規 fixture。
`consoleGrowingReplySteps()` の 4 件の SSE 増分を `appendConsoleBlock` と同じ規則でその場で積み上げた
1 件）を混ぜた。Phase 68 はまさにこの理由（偽陽性）で混ぜるのを見送っていたが、今回は
`pnpm mobile-audit` が引き続き 0 件のままであることを確認した（下記の証跡）。

### 監査（機械検査）

| rule | G28 後 | G29 後 |
| --- | --- | --- |
| overflow / status-badge / fixed-overlay / tap-target / font-size | 0 | **0**（維持） |
| 合計 | 0 | **0**（`pnpm mobile-audit` exit 0、21 route 全て 200） |

途中、新設した `org-node-lead-session` カードの `<dt>` 3 箇所が `text-xs`（12px）のままで
`font-size` 違反 3 件が一度出たが、`text-sm lg:text-xs`（ADR-0055 D1-4 の規約）に直して 0 に戻した。

### 証跡（コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint` | exit 0。`Checked 225 files. No fixes applied.`（`biome check --write` で新規コードの整形差分を一度自動修正してから確認） |
| typecheck | `pnpm typecheck` | exit 0 |
| test | `pnpm test` | exit 0。**Test Files 61 passed (61) / Tests 893 passed (893)**（Phase G28 の 881 から +12: `hasStreamingReply` 3 件、`firstLine`/`hasMoreThanFirstLine` 3 件、`shouldStickToBottom` 4 件、`releaseVerifyIcon` 1 件、`buildOrgTree` の深いサブツリー 1 件） |
| build | `pnpm build` | exit 0（client・server とも） |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（celeris API 契約は変えていない。今回は API 変更そのものが無い） |
| mobile-audit | `pnpm mobile-audit` | **exit 0。violations 0 件**。21 route 全て 200 応答、`page-error` 0 |

before/after のスクリーンショット（`gui/test/mobile-audit/*.png`、git には入れない。差分の目視用）:
`home.png`（育つ吹き出し・送信待ちヒント・overflow メニュー）、`org.png`/`org-detail.png`
（深いサブツリーの開閉・lead_sessions カード）、`releases.png`（検証バッジのアイコン）で今回の変更を
確認した。

### 未解決事項

- **U-G29-1（実機未確認）**: 「最新へ」ピル（上にスクロールしてから新しいブロックが来る操作）、
  overflow メニューのタップでの開閉、キーボード表示時の入力欄（U8 から継続）は Playwright では
  再現しづらい・していない。実機（iOS Safari / Android Chrome）が使える人またはエージェントに依頼する
  （手順は P-G23-1 のとおり）。
- **U-G29-2**: `ReplyStepRow` の `tool_use` の要約は 90 字で切っている（`truncateLabel`）。実機の
  celeris が返す `summary` がこれより大きく長い（例: 長いコマンドライン全体）場合、`title` で全文は
  見えるが、393px でホバー相当の手段が無いスマホでは「タップして長押し」以外に全文を見る手段が無い。
  次のラウンドでタップで展開できるようにする案がある（今回は tool_result の `<details>` と役割が
  重ならないよう見送った）。
- **U-G29-3**: `NewConversationMenu` の背景を閉じるボタン（`fixed inset-0`）は、メニューを開いている
  間だけ DOM に存在する（閉じているときは無い）ので機械検査には映らない。スクリーンリーダーでの
  フォーカストラップ（Tab で外に出られてしまわないか）は確認していない（既存の `MobileOtherSheet` も
  同じ作りで、これまで指摘は出ていない）。

### 提案

- **P-G29-1**: 今回 `checkFixedOverlays` に内側スクローラの追随を足したので（P-G28-1 の実装）、
  同じ考え方（`isNotVisible` の構造的な判定と同様）を他の検査（例えば D1-2 タップ領域）にも広げられる
  余地がある。今のところ他の検査で内側スクロールに起因する誤検知は出ていないので急ぎではない。
- **P-G29-2**: U-G29-2 のとおり、`tool_use` の要約をタップで展開できるようにする（`tool_result` と
  同じ `<details>` にするか、別の折り畳みにするか）は次のラウンドの候補。

## Phase G30 — スマホ UX ラウンド 6（ADR-0055 D2、Phase 74。2026-09-21）

celeris 側は無変更（`crates/` 無変更）。Phase 74 の依頼（`gui/CLAUDE.md`・ADR-0055・ADR-0048 D2）に沿って
5 項目を実装した。ラウンド 5（Phase G29）の未解決事項 U-G29-2 / 提案 P-G29-2（`tool_use` の要約のタップ
展開）から始め、タスク詳細画面のタイムライン、`/approvals` 等の時刻表示、空・エラー状態の確認まで進めた。

### 1. tool_use の要約をタップで展開（U-G29-2 / P-G29-2 の解消。受け入れ条件 1）

- `~/lib/console.ts` に `TOOL_SUMMARY_MAX_LENGTH`（= 90。Phase 73 でハードコードしていた値を定数化）と
  純粋関数 `toolSummaryTruncated(text, maxLength = TOOL_SUMMARY_MAX_LENGTH)` を追加（vitest 3 本）。
- `~/components/ConsoleBlockItem.tsx::ReplyStepRow` を作り直した: `tool_use` も `tool_result` も、
  省略・折り畳みが起きるときは同じ「行全体をタップで開閉」の作り（`useState` + `button` +
  `aria-expanded`）にした。`tool_result` はこれまで `<details>`/`<summary>` を使っていたが、
  `<details>` は開閉状態を `aria-expanded` として公開しない（ブラウザによって扱いが違う）ため、
  受け入れ条件が明示する disclosure semantics（`aria-expanded` を持つ、キーボード操作可能、44px の
  タップ領域）を満たすよう `button` ベースに揃えた。閉じているときは `title` にも全文を残す（マウス操作
  の人にはホバーでも見える）。`ReplyStepRow` を `export` し、`~/routes/tasks.$id.tsx` のタイムライン
  （2 で説明）から再利用する。
- 機械検査で実データを通すため、`test/mock-celeris/fixtures.ts::consoleGrowingReplySnapshot()` の
  `tool_use` の要約を 95 字（90 字超）にし、`gui/scripts/mobile-audit.mjs` の Console モックが展開ボタン
  （縮めた状態）を実際に描画するようにした（screenshot で目視確認。下記「証跡」参照）。

### 2. タスク詳細（`/tasks/<id>`）の worker progress（ADR-0048 D2、受け入れ条件 2）

- これまでタイムラインタブは `worker_progress` イベントを 1 件ずつ生の `event.msg`（構造化前の文字列）
  で出していた（ADR-0048 D2 が定義した `kind`/`tool`/`summary`/`detail`/`error` を一切使っていなかった）。
  celeris 側に run 単位の集約 API が無い（Console の `progress` ブロックは `GET /console` がその集約を
  返す）ので、GUI 側で「連続する `worker_progress` イベント」を 1 つの折り畳みにまとめることにした。
- 新規 `~/lib/task-timeline.ts`（純粋関数、DOM を持ち込まない。`~/lib/console.ts` と同じ方針）:
  - `groupTimelineWorkerProgress(items)`: 連続する `worker_progress` を `{kind:"progress_group", items}`
    にまとめる（1 件だけでも `progress_group`。折り畳みの有無は呼び出し側が `items.length` で決める）。
  - `timelineProgressGroupSummary(items)`: 「N 件 ・ 最後: <kind>」の一行要約（受け入れ条件 2 の
    「最後の progress kind」）。時刻は呼び出し側で別途 `relativeTimeLabel` を添える。
  - `workerProgressStep(event)`: `Event`（`worker_progress`）を Console の `ConsoleReplyStep` と同じ形
    （`{kind, text: summary ?? msg, tool, error}`）にする。`~/lib/console.ts::formatRunEventRow` の
    `summary ?? msg` と同じ規則。
  - vitest: `groupTimelineWorkerProgress`（連続まとめ・1 件・別種に挟まれた 2 区間・無し・空配列）、
    `timelineProgressGroupSummary`、`workerProgressStep`（8 本、`test/unit/task-timeline.test.ts`）。
- `~/routes/tasks.$id.tsx` に `TimelineProgressGroupRow` を追加: 既定は折り畳み（`aria-expanded` を持つ
  `button`、44px のタップ領域）、見出しは他の `event` 行と同じ体裁（時刻・`timelineKindLabel("event")`
  バッジ・`Mono` の `worker_progress`）のすぐ下に一行要約、開くと `ReplyStepRow`（1 で export したもの）
  が並ぶ = Console の progress ブロックを開いたときと同じ見た目になる。`groupTimelineWorkerProgress` は
  タイムラインの描画直前（`TimelineTab` の `<ol>`）で呼び、`progress_group` は `TimelineProgressGroupRow`、
  それ以外は既存の `TimelineRow` に出し分ける。
- 機械検査で実データを通すため、`test/mock-celeris/fixtures.ts::timelineWorkerProgressItems()`（新規。
  `tool_use` ×2（うち 1 件は 90 字超、`Bash` の長いコマンドライン）+ `tool_result` ×1（複数行）の 3 件）
  を追加し、`gui/scripts/mobile-audit.mjs` の `/tasks/{id}/timeline` モックに混ぜた。

### 3. 時刻表示の統一（受け入れ条件 3）

- 監査の結果、Console のブロック（`block.at`）と `/tasks/:id` のタイムライン（`item.at`）・run 一覧
  （`run.started_at`/`finished_at`）は celeris が返す**生の ISO 文字列をそのまま**表示していた
  （393px には長すぎ、`/approvals`・`/artifacts` 等が既に使っている「n 前」の相対表示
  `~/lib/reports.ts::relativeTimeLabel` と揃っていなかった）。加えて `~/lib/artifacts.ts::
  artifactRelativeTime` が `relativeTimeLabel` と**実装が一字一句同じ**まま別名で重複していた
  （「2 つの形式が混在していたら 1 つの pure helper に揃える」の該当箇所）。
  - `artifactRelativeTime` を削除し、`~/components/ArtifactsList.tsx` は `relativeTimeLabel`
    （`~/lib/reports.ts`）を直接使うように変更（`test/unit/artifacts.test.ts` から重複テストを削除。
    `relativeTimeLabel` 自体のテストは既存の `test/unit/reports.test.ts` にある）。
  - Console: `~/lib/console.ts::ConsoleData` に `fetchedAt`（loader が読み込んだ時刻。`~/celeris/
    console.server.ts::loadConsole` が `new Date().toISOString()` を足す）を追加。`root` の SSE が
    daemon tick ごとに全ルートを再検証するので、Console を長く開いていても `fetchedAt` は定期的に
    更新される（1 回だけ読んで固定、ではない）。`~/components/ConsoleBlockItem.tsx::BlockHeader` の
    `at`（文字列そのまま）を `atIso` + `fetchedAt` に変え、内部で `relativeTimeLabel(atIso, fetchedAt)`
    を出し、絶対時刻は `title` に残した（8 ブロック種すべて + `TaskBlockView` の行内時刻）。
  - `/tasks/:id`: `TaskDetailData` に `fetchedAt` を追加（loader）。タイムラインの `item.at`、run 一覧の
    `started_at`/`finished_at` を `relativeTimeLabel` + `title`（絶対時刻）に変えた。
  - `/approvals`: 既存の 2 箇所（認可待ち・決めたもの）の `relativeTimeLabel` 表示に `title`（絶対時刻）
    を足し、`StandingRuleRow`（永続の認可）の `rule.created_at` 生表示も `relativeTimeLabel` + `title`
    に揃えた（`fetchedAt` を新しい prop として渡す）。
- 「1 つの primary action per card」の確認（受け入れ条件 3 の後半）: `/approvals`・`/inbox` のカードを
  読み直した。認可カードの「今回だけ／今後ずっと／認めない」は 3 択の決定 UI（メニューの寄せ集めではない）
  なので対象外、`/inbox` の draft カードも「受け入れ／取り消し」+ グループ単位の「全部受け入れ」で
  意図的に分かれている。変更は不要と判断した（コードは変えていない）。

### 4. 空・エラー状態の確認（受け入れ条件 4。実装は無し、確認のみ）

ADR-0055 D1 が定める 21 route（監査対象そのもの）を `EmptyState`/`Alert`/`ErrorBoundary` の有無で
確認した。すべて何らかの形でカバーされている:

- ほとんどのルートは自前の `EmptyState`（一覧が空のとき）と `ErrorBoundary`（`celerisErrorResponse` /
  404 等）を持つ。
- `/`・`/org/<node>` は `~/components/Console.tsx::BlockStream` が持つ 1 つの `EmptyState`
  （「まだ何も流れていません」）を共有し、自前の `ErrorBoundary` は持たない（celeris 停止中はローカルで
  拾って 200 のまま出す。それ以外は `~/root.tsx` の共有 `ErrorBoundary` に委ねる。`/org/secretary` は
  リダイレクトのみで描画しない）。この委譲は既存の意図した設計（`home.tsx`/`org.$id.tsx` のコメント）で、
  今回変更していない。
- `/help` は celeris に問い合わせない静的ページ（loader 無し）なので、空・エラー状態そのものが無い。
- `/tasks/:id` の「変更」「ファイル」タブの中身（`~/components/task-changes.tsx`/`task-files.tsx`）は
  個別に `EmptyState`（リポジトリ無し・差分無し・ディレクトリが空 等）と `Alert`（busy・conflict 等）を
  複数持つ。
- 393px でのはみ出しは `pnpm mobile-audit`（D1-1）がそのまま検査する。今回は空・エラー状態を**新しく
  作っていない**（既存で足りていた）ので、コード変更は無い。

### 監査（機械検査）

| rule | G29 後 | G30 後 |
| --- | --- | --- |
| overflow / status-badge / fixed-overlay / tap-target / font-size | 0 | **0**（維持） |
| 合計 | 0 | **0**（`pnpm mobile-audit` exit 0、21 route 全て 200） |

新しく混ぜた実データ（`tool_use` 95 字の要約、`worker_progress` の折り畳み groupの見出し行）でも
違反 0 のまま（`test/mobile-audit/home.png`・`task-timeline.png` で目視確認: 展開ボタンの `…` と
折り畳みの一行要約が 393px 内に収まっている）。

### 証跡（コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint`（事前に `pnpm exec biome check --write .` で新規コードの整形・import 順を自動修正） | exit 0。`Checked 227 files. No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0 |
| test | `pnpm test` | exit 0。**Test Files 62 passed (62) / Tests 904 passed (904)**（Phase G29 の 893 から +11: `toolSummaryTruncated` 3 件、`groupTimelineWorkerProgress`/`timelineProgressGroupSummary`/`workerProgressStep` 8 件） |
| build | `pnpm build` | exit 0（client・server とも） |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（API 変更なし） |
| mobile-audit | `pnpm mobile-audit` | **exit 0。violations 0 件**。21 route 全て 200 応答 |

### 変更したファイル

- `app/lib/console.ts`（`TOOL_SUMMARY_MAX_LENGTH`/`toolSummaryTruncated`、`ConsoleData.fetchedAt`）
- `app/lib/task-timeline.ts`（新規: `groupTimelineWorkerProgress`/`timelineProgressGroupSummary`/`workerProgressStep`）
- `app/lib/artifacts.ts`（`artifactRelativeTime` 削除）
- `app/celeris/console.server.ts`（`loadConsole` が `fetchedAt` を足す）
- `app/components/ConsoleBlockItem.tsx`（`ReplyStepRow` 作り直し・export、`BlockHeader` を `atIso`/`fetchedAt` に、全ブロック種に `fetchedAt` を通す）
- `app/components/Console.tsx`・`app/components/ArtifactsList.tsx`（`fetchedAt`/`relativeTimeLabel` の配線）
- `app/routes/home.tsx`・`app/routes/org.$id.tsx`（celeris 停止中のフォールバックに `fetchedAt` を追加）
- `app/routes/tasks.$id.tsx`（`TaskDetailData.fetchedAt`、`TimelineProgressGroupRow` 新設、run 一覧の時刻表示）
- `app/routes/approvals.tsx`（`title` 属性の追加、`StandingRuleRow` の時刻表示）
- `test/mock-celeris/fixtures.ts`（`timelineWorkerProgressItems()` 新規、`consoleGrowingReplySnapshot()` の要約を長く）
- `scripts/mobile-audit.mjs`（`/tasks/{id}/timeline` モックに worker_progress を混ぜる）
- `test/unit/console.test.ts`・`test/unit/task-timeline.test.ts`（新規）・`test/unit/artifacts.test.ts`・`test/unit/tasks.detail.loader.test.ts`

### 未解決事項

- **U-G30-1（実機未確認）**: `ReplyStepRow`/`TimelineProgressGroupRow` のタップ展開、`aria-expanded`
  のスクリーンリーダーでの読み上げは Playwright の機械検査（クリック操作をしない）では確認していない。
  実機（iOS Safari / Android Chrome、または VoiceOver/TalkBack）での確認が要る。
- **U-G30-2**: `relativeTimeLabel` の元になる `formatDuration`（`~/lib/time-delta.ts`）は分・秒だけの
  表示で、時間・日をまとめない（例: 3 日前が「4320m0s 前」になる）。今回この表示を Console・タイムライン・
  run 一覧・`StandingRuleRow` に広げたことで、古い時刻ほどこの読みにくさが目立つ場所が増えた。
  `/approvals` 等では以前から同じ形式だったので新しい不具合ではないが、次のラウンドで「時間」「日」の
  単位を足すか検討する余地がある（`formatDuration` 自体の変更は他画面にも影響するため、今回は見送った）。
- **U-G30-3**: `/tasks/:id` の「生のイベント（絞り込み）」節（`~/routes/tasks.$id.tsx` の `raw-events`
  `<details>`）は `worker_progress` を従来どおり `event.msg` のまま出している（今回のグループ化はタイム
  ラインタブの `<ol>` だけに適用し、裏方向けの生ログはあえて変えていない）。

### 提案

- **P-G30-1**: U-G30-2 のとおり、`formatDuration`/`relativeTimeLabel` に「時間」「日」の単位を足す
  リファクタリングは影響範囲が広い（`/approvals`・`/artifacts`・`/reports`・Console・`/tasks/:id` 全部）
  ので、専用のラウンドとして切り出すとよい。

## Phase G31 — スマホ UX ラウンド 7（時刻表記・ダークモード監査・カード密度。ADR-0055、Phase 75。2026-09-21）

celeris 側は無変更（`crates/` 無変更）。Phase 75 の依頼（`gui/CLAUDE.md`・ADR-0055・Phase G30 の
U-G30-2 / P-G30-1）に沿って 3 項目を実装した。

### 1. 相対時刻・期間表示の日本語化（P-G30-1 の解消。受け入れ条件 1）

- `~/lib/time-delta.ts`: `formatDuration` を「分・秒だけ」（`"3m12s"`/`"45s"`）から、上位 2 単位までに
  丸めた日本語表記（`"3分12秒"`・`"1時間12分"`・`"2日3時間"`。ちょうどの単位は下位を出さない: `"1時間"`・
  `"1日"`）に変えた。内部で新規 `splitDuration`（日/時間/分/秒への分解。負値は 0）を export し、
  `~/lib/reports.ts::relativeTimeLabel` からも使えるようにした。
- `~/lib/reports.ts::relativeTimeLabel`（「n 前」表示）は `formatDuration` を呼ぶのをやめ、独自ロジックに
  した: `formatDuration` の 2 単位より粗く、**1 単位だけ**に丸める（コンパクトにするため。「3分12秒前」
  ではなく「3分前」）。10 秒未満は「たった今」。7 日（`RELATIVE_ABSOLUTE_THRESHOLD_SECONDS`）を超えたら
  絶対日付にフォールバックする新規 `absoluteDateLabel`（同じ年なら `"M/D"`、年をまたぐと `"YYYY/M/D"`。
  UTC の年月日で比較。celeris の ISO は UTC でそのまま扱う既存方針に合わせた）。どちらの関数も絶対時刻
  そのものは返さない（呼び出し側が `title` 属性に生の ISO を残す規律は変えていない）。
- 呼び出し側の統一: `~/routes/accounts.tsx` に 2 箇所あった `` `${formatDuration(secondsBetween(...))} 前` ``
  という手書きパターン（`observed_at`・秘密の`updated_at`）を `relativeTimeLabel` の直接呼び出しに揃えた
  （`formatDuration`/`secondsBetween` 自体は cooldown 残り・使用量リセットまでの残り時間表示に残るので
  import は維持）。`~/components/ReportsList.tsx` の相対時刻表示に `title={report.created_at}` が
  抜けていたのを追加した（他の相対時刻表示箇所は Phase 74 で既に `title` を持っていたことを確認済み:
  Console・タイムライン・run 一覧・`/approvals`・`ArtifactsList`）。`~/routes/daemon.tsx`・
  `~/routes/providers.tsx`・`~/lib/llm-sources.ts` の `formatDuration`（経過・cooldown 残り。「n 前」
  ではない期間表示）はそのまま新しい日本語書式に切り替わる（呼び出し側の変更なし）。
- テスト: `test/unit/time-delta.test.ts` を `splitDuration`/`formatDuration` の table-driven テスト
  （`it.each`、0 秒・59 秒・ちょうど 1 分/1 時間/1 日の境界・負値を含む 17 ケース）に作り直した。
  `test/unit/reports.test.ts::relativeTimeLabel` も table-driven（たった今・n 秒前・分/時間/日の切り替え・
  7 日境界・年またぎの絶対日付、8 ケース）に作り直した。副作用として `test/unit/llm-sources.test.ts`
  （`cooldownRemainingLabel`）・`test/unit/console.test.ts`（`progressSummaryLine`/`taskLineSummary` の
  「経過」表示）にあった旧書式（`"1m0s"`・`"24s"`・`"33s"`）の期待値を新書式に更新した。

### 2. ダークモードの機械検査・コントラスト規則（受け入れ条件 2）

- `gui/scripts/mobile-audit.mjs`: 各画面を `page.emulateMedia({ colorScheme })` で `light`/`dark` の
  2 回ずつ開くようにした（`goto` 前に設定するので初回描画から `app/app.css` の
  `@media (prefers-color-scheme: dark)` が効く）。スクリーンショットは light が従来どおり `<route>.png`、
  dark が新規 `<route>.dark.png`（21 route × 2 scheme = 42 ファイル）。`report.json` の `routes`/
  `violations` に `scheme` を追加し、サマリの標準エラー出力に `by_scheme`（light/dark 別の違反数）を足した。
- 新規ルール `contrast`（WCAG AA）: `getComputedStyle` で要素の `color` と、要素自身から祖先へ
  `backgroundColor` をたどって合成した実効背景色（`findEffectiveBackground`。半透明の背景が複数重なる
  ケースがあるため、単純な「最初に見つかった不透明色」ではなく `compositeOver` で実際にアルファ合成する。
  ダークモードのバッジ背景 `rgb(.. / 0.12〜0.26)` はこの合成をしないと実際の色より大きく誤る）に対する
  相対輝度比（`relativeLuminance`/`contrastRatio`、WCAG の標準式）を計算し、`font-size < 18px` は 4.5:1、
  `>= 18px` は 3:1 を下回ったら違反にした。対象は直接テキストを持つ末端要素のみ（`checkFontSize` と同じ
  絞り込み方針）。`runChecks`/注入スクリプトの両方に配線し、CI は 1 件でも違反があれば非 0 のまま。
- 実測: `docs/adr/0011-visual-design-system.md` のコメントどおり `app/app.css` のトークン（`--fg`/
  `--fg-muted`/`--fg-subtle` × `--bg`/`--surface`/`--surface-2`/`--surface-3`、ソフトバッジの
  `*-soft-fg` × `*-soft`）を Python で手計算したところ、ライト・ダークとも全組み合わせが 4.5:1 以上
  だった（ダークのソフトバッジは `--bg`/`--surface`/`--surface-2` の上に合成しても 6.5:1 以上）。
  実際に `pnpm mobile-audit` を回しても `contrast` 違反は 0 件（下記「監査」参照）で、トークン側の
  手直しは不要だった（**デザイントークンは変更していない**。受け入れ条件の「トークンで直す」は今回
  違反が出なかったので該当なし）。

### 3. ボードカードの密度（受け入れ条件 3）

- `~/routes/board.tsx::BoardCard`: 3 箇所の `gap-1.5`（6px。4/8/12/16 のスケール外）を `gap-2`（8px）に
  揃えた（状態・優先度バッジの行、種類・レベルバッジの行、行内編集の `<select>` 群の行）。題名リンクの
  `mt-1.5` も `mt-2` に揃えた（`lg:` 側の間隔は変えていない。`lg:mt-1.5` はデスクトップ専用クラスなので
  触っていない）。
- 題名（`board-card-title`）を 2 行クランプにした: `Link`（タップ領域確保のため `flex min-h-11
  items-center` は維持）の中に `<span className="line-clamp-2 min-w-0 [overflow-wrap:anywhere]">` を
  新設し、`title={item.title}`（全文）を `Link` に付けた。`line-clamp-2` は `display: -webkit-box` を
  要求し `flex` と両立しないため、外側の `Link` ではなく内側の `span` に掛けている（`min-w-0` は
  flex item がクランプ前に横に伸びきらないようにするため）。`overflow-wrap: anywhere` は Console の
  `ConsoleBlockItem.tsx` と同じ `[overflow-wrap:anywhere]` の書き方（id やハイフン無しの長い英数字が
  混じっても単語の途中で折り返せる）。
- 機械検査で実データを通すため、`gui/scripts/mobile-audit.mjs` の `/api/v1/tasks` モックに 2 件目の
  カード（日本語 + 区切りの無い英数字混じり、90 字超の題名。モバイルの既定タブ `ready` で見える
  ステータス）を混ぜた。`test/mobile-audit/board.png`/`board.dark.png` を目視確認: 長い題名が 2 行で
  末尾「…」に丸まり、バッジ行の間隔がそろっている（下記「証跡」参照）。

### 監査（機械検査）

| rule | G30 後 | G31 後（light） | G31 後（dark、新規） |
| --- | --- | --- | --- |
| overflow / status-badge / fixed-overlay / tap-target / font-size / table-wrap | 0 | 0 | 0 |
| contrast（新規） | - | **0** | **0** |
| 合計 | 0 | **0** | **0** |

`pnpm mobile-audit` は 21 route × 2 scheme = 42 通り全て 200 応答、違反 0 件で exit 0
（`by_scheme: {"light": 0, "dark": 0}`）。

### 証跡（コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint`（`accounts.tsx` の 1 箇所で `pnpm exec biome check --write .` による自動整形が要った） | exit 0。`Checked 227 files in 98ms. No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0 |
| test | `pnpm test` | exit 0。**Test Files 62 passed (62) / Tests 925 passed (925)**（Phase G30 の 904 から +21: `splitDuration` 6 件・`formatDuration` 11 件（旧 3 件を置き換え）・`relativeTimeLabel` 8 件（旧 1 件を置き換え）で、差し引き正味 +21） |
| build | `pnpm build` | exit 0（client・server とも） |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（API 変更なし） |
| mobile-audit | `pnpm mobile-audit` | **exit 0。violations 0 件**（`by_rule: {}`、`by_scheme: {"light": 0, "dark": 0}`）。21 route × 2 scheme = 42 通り全て 200 応答 |

### 変更したファイル

- `app/lib/time-delta.ts`（`formatDuration` を日本語 2 単位表記に、`splitDuration` 新規 export）
- `app/lib/reports.ts`（`relativeTimeLabel` を 1 単位・「たった今」・7 日超で絶対日付にする独自ロジックへ、`absoluteDateLabel` 新規）
- `app/routes/accounts.tsx`（2 箇所を `relativeTimeLabel` 直接呼び出しに統一）
- `app/components/ReportsList.tsx`（相対時刻表示に `title` を追加）
- `app/routes/board.tsx`（`BoardCard` の gap/margin を 4/8/12/16 スケールに揃え、題名を 2 行クランプ + `overflow-wrap: anywhere` + `title`）
- `scripts/mobile-audit.mjs`（`contrast` ルール新設、light/dark 二重実行、`<route>.dark.png`、`/api/v1/tasks` モックに長い題名のカードを追加）
- `test/unit/time-delta.test.ts`・`test/unit/reports.test.ts`（table-driven に作り直し）
- `test/unit/llm-sources.test.ts`・`test/unit/console.test.ts`（期間表示の期待値を新書式に更新）

### 未解決事項

- **U-G31-1（実機未確認、ADR-0009 P-34 継続）**: ダークモードの実際の見え方（OLED での黒つぶれ・
  コントラストの主観的な見やすさ）、`line-clamp-2` の Safari 系ブラウザでの挙動（`-webkit-line-clamp`
  ベースなので最新の Chromium/Safari では動くはずだが、Playwright の Chromium でしか確認していない）は
  実機での確認が要る（認証・ネットワークが使えるサンドボックスではないため今回も未実施）。
- **U-G31-2**: `contrast` ルールは leaf text 要素のみを見ており、アイコン（SVG の `fill`）やフォーカス
  リングの色対比は検査していない（ADR-0055 D1 のスコープ外）。
- **U-G31-3**: `formatDuration`/`relativeTimeLabel` の絶対日付フォールバック（7 日超）は UTC の年月日
  比較で、ブラウザのローカルタイムゾーンでの「日付が変わった」感覚とはずれうる（celeris・GUI とも
  タイムゾーン変換をしない既存方針に合わせた。ずれが問題になったら次のラウンドで検討）。

## Phase G32 — スマホ UX ラウンド 8: アクセシブルな名前・フォーカス・ライブリージョン（ADR-0055、Phase 76。2026-09-21）

celeris 側は無変更（`crates/` 無変更）。Phase 76 の依頼（`gui/CLAUDE.md`・ADR-0055・U-G31-2「`contrast` ルールは
アイコン・フォーカスリングを見ていない」）に沿って、機械検査に 3 ルールを足し、それが見つけた違反と、
目視で見つけたフォーカスリングのコントラスト不足を直した。

### 1. `a11y-name`（アクセシブルな名前。受け入れ条件 1 前半）

- `scripts/mobile-audit.mjs` に `computeAccessibleName(el)` を追加: `aria-label` → `aria-labelledby`
  （参照先の `textContent`）→ `<label for>` / 包む `<label>` → `input[type=submit|button|reset]` の
  `value` → 自身の `textContent` → 最後の手段として `title` の順で見る。`placeholder` は仕様上アクセシブルな
  名前にならないので対象に入れていない（意図して外した。プレースホルダしか無い入力欄を見落とさないため）。
- `checkA11yNames()`: `button, a[href], input:not([type=hidden]), select, textarea, [role=button],
  [role=tab], [role=menuitem]` のうち可視なものを全部見て、`computeAccessibleName` が空文字なら違反
  （ルール名 `a11y-name`）。`runChecks()` に配線。
- 実装直後の実行では違反 0 件だった（`~/components/ui/Icon.tsx` は常に `aria-hidden` で、アイコンのみの
  ボタン（下部固定タブの「閉じる」「ログアウト」「その他の操作」等）は既に個別の `aria-label` を持っていた
  ため、ラウンド 2〜7 の積み重ねで既に揃っていたことが分かった）。**ルールが実際に働くことは、
  `~/components/Console.tsx` の `console-overflow-trigger`（アイコンのみのボタン）から一時的に
  `aria-label="その他の操作"` を外して `pnpm mobile-audit` を再実行し、`home`/`org-node` の light/dark
  4 件が `a11y-name` で落ちる（`detail: "no accessible name on <button>"`）ことを確認してから元に戻した
  （証跡は「監査」節）。

### 2. `a11y-structure`（画面の骨格。受け入れ条件 1 後半）

- `checkA11yStructure()`: 可視な `h1` がちょうど 1 個・見出しレベルが直前の見出しから 2 段以上飛ばない
  （axe-core の heading-order と同じ考え方）・`img` は `alt` 属性を持つ・`svg` は `role="img"`（`aria-label`/
  `aria-labelledby`/`<title>` のいずれかで名前を持つ）か装飾なら `aria-hidden="true"`・可視な `main`（または
  `role=main`）と `nav`（または `role=navigation`）が画面に 1 つ以上ある、の 4 点を見る。`runChecks()` に配線。
- 実装直後の実行で 4 件（`home`/`org-node` の light/dark）: `~/components/Console.tsx`（`/` と `/org/:id` が
  共有）だけが他の画面と違い `~/components/ui/misc.tsx::PageHeader`（既定 `h1`）を使わず、可視な見出しが
  0 個だった（`/org` や `/projects` 等は `PageHeader` 経由で `h1` を 1 個持っており、違反はここだけだった）。
  `~/components/Console.tsx` の先頭に `<h1 className="sr-only">Console</h1>` を足して解消（見た目は変えず、
  構造だけ足す）。
- `img`/`svg`/ランドマークの違反は最初から 0 件だった（`Icon` コンポーネントは既に常に `aria-hidden`、
  `<main>` は `~/root.tsx` に常時、`<nav>` はデスクトップ幅では `Sidebar`、モバイル幅（393px）では
  `MobileTabBar` のどちらか一方が可視になる作りが既にできていた）。

### 3. フォーカスの可視性と順序（受け入れ条件 2）

- **リングのコントラスト不足を発見・修正**: `~/components/ui/form.ts` の `FIELD`（`input`/`select`/
  `textarea` 共通）は `focus:outline-none focus:ring-3 focus:ring-primary/20` だけで、不透明度 20% の
  box-shadow リングだった。Python で実測（`relativeLuminance`/`contrastRatio` を手計算、WCAG の式どおり）
  したところ、light で `--primary`（`#4f46e5`）20% を `--surface`（`#ffffff`）に合成した実効色との対比が
  **1.36:1**、dark で `--primary`（`#7c83f7`）20% を `--surface`（`#11141c`）に合成した実効色との対比が
  **1.32:1** で、どちらも要求の 3:1 を大きく下回っていた。`~/components/ui/button.tsx` の `BASE` と同じ
  `focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring`（不透明な `--ring`
  トークンの実線 outline、2px）に揃えた。同じく Python で実測: light `--ring`（`#6366f1`）対
  `--surface`/`--bg`/`--surface-2` が 4.47:1 / 4.17:1 / 4.03:1、dark `--ring`（`#818cf8`）対
  同じ 3 つが 6.17:1 / 6.55:1 / 5.77:1 で、全組み合わせが 3:1 を十分上回る。`~/app/app.css` の
  `:focus-visible { outline: 2px solid var(--ring); outline-offset: 2px; }`（Phase 69 から存在）は
  `button`/`a`/`Icon` を持つボタン等には既に効いていた（`~/components/ui/button.tsx` の `BASE` が既に
  同じ `focus-visible:outline-*` を持っていたため）ので、今回直したのはフォームの `input`/`select`/
  `textarea` だけ（`~/components/ui/form.ts` の `FIELD` 1 箇所。`inputClass`/`textareaClass`/`selectClass`/
  `multiSelectClass` は全部これを継承するので 1 箇所の修正で揃う）。この変更は `lg:` で分岐していない
  （デスクトップにも同じ効果が及ぶ）が、見た目のレイアウトは変えず純粋にアクセシビリティの不具合修正
  なので「明らかに中立」として扱った（デスクトップは元々リングが薄すぎて実質見えていなかったのが、
  見えるようになるだけ）。
- **DOM 順**: `~/root.tsx` は元々、下部固定タブ（`MobileTabBar`）が本文（`<main>` を含む `div>`）の
  **後ろ**の兄弟要素として置かれており（フェーズ 71 の実装のまま）、DOM 順としては既に「本文の後」を
  満たしていた。Console の入力欄（`ConsoleInput`）も `position: fixed` で画面下に貼り付くだけで、DOM 上は
  `BlockStream`（ブロックの一覧）の直後というブロック内の自然な位置にあり、これも問題無かった
  （`checkFocusOrder` の「罠が無い」「composer に届く」が実装直後から 0 件だったのはこのため）。
- **スキップリンク**: `~/root.tsx` の認証済みレイアウトの先頭（`<div className="min-h-screen lg:grid...">`
  の最初の子）に `<a href="#main-content">本文へ</a>` を追加。既定は `sr-only`（`clip` で 1×1 に潰す）、
  `focus:not-sr-only` でフォーカス時だけ見える固定バッジになる。`<main>` に `id="main-content"` と
  `tabIndex={-1}`（アンカーが指すために必要。クリック/Enter でジャンプしたあとフォーカスが `<main>` 自体に
  移る）を追加。
  - **副作用**: `sr-only` は `display:none` にしないので、既存の `tap-target` ルール（44×44 未満は違反）が
    未フォーカス時の 1×1 のアンカーに反応し、実装直後の `pnpm mobile-audit` で 21 route × 2 scheme = 42 件の
    `tap-target` 違反が出た（アプリ本体の既存欠陥ではなく、スキップリンクを足したこと自体が原因）。
    スキップリンクはタッチで狙って押す対象ではないので、`~/scripts/mobile-audit.mjs` の既存の例外機構
    `data-touch-ok` を付けて解消した（0 件に戻ったことを確認済み）。
- **`focus-order`（新設ルール）**: `checkFocusOrder(page, route)`（`scripts/mobile-audit.mjs`。`runChecks`
  とは別枠、Node 側から実際に `page.keyboard.press("Tab")` を送る）。文書の先頭から Tab を押し続け、
  `window.__cssPathRef(document.activeElement)`（`runChecks` と同じ `cssPathRef` を注入スクリプト経由で
  公開）が直前と全く同じ文字列のままなら罠と判定して止める。歩数の上限は「画面上の可視な操作可能要素数 + 10」
  （決め打ちの固定値だと画面ごとの複雑さのばらつきで誤検知しうるため）。`console-text`（Console の
  composer）を持つ画面（`home`・`org-node`・`org-detail`。実際に composer を持つのは `home`/`org-node`
  だけで `org-detail` は持たない）だけは、composer に到達したかも見て、到達しなければ違反にする。
  - **ルールの検証**: `~/components/Console.tsx` の `console-text` に一時的に `tabIndex={-1}` を付けて
    `pnpm mobile-audit` を再実行し、`home`/`org-node` の light/dark 4 件が `focus-order` で落ちる
    （`detail: "composer (console-text) not reached from document start within 31 Tab presses (21
    focusable elements on page)"`）ことを確認してから元に戻した（証跡は「監査」節）。

### 4. ライブリージョン（受け入れ条件 3）

- `~/components/ConsoleBlockItem.tsx::BlockShell` に `busy?: boolean` を追加し、中身の箱に
  `aria-busy={busy || undefined}` を付けられるようにした。`ReplyBlockView`（育つ返事）は
  `busy={streaming}` を渡す。
- 「考え中…」の行（`console-reply-thinking`）に `aria-live="polite"` を追加（celeris 側が最新の 1 行だけを
  置き換え式で送る設計なので、読み上げは常に 1 回分で済む）。
- `~/components/MarkdownViewer.tsx` に `live?: boolean`（既定 `false`）を追加し、`true` のときだけ
  `aria-live="polite"` / `aria-atomic="false"` を付ける。`ReplyBlockView` は確定していく本文
  （`block.text`。run 中は「ここまでの積み上げ」、ADR-0054 D2）だけ `live={streaming}` で渡す。報告・
  途中目標のレビュー・文書ページ等、他の `MarkdownViewer` 利用箇所（開いたあと中身が変わらない）は
  既定のまま（`live` を渡さない = ライブリージョンにしない）にして、無関係な画面までライブリージョンだらけに
  しないようにした。
- **spam しない**: `tool_use`/`tool_result` の 1 手ごとの行（`console-reply-steps` の中の `ReplyStepRow`）は
  上記のどちらのライブリージョンにも含めていない（見た目の並び順は変えていないが、`aria-live` を持つ
  兄弟要素の外に置いている）。「考え中…」の 1 行と、確定していく本文の更新だけが読み上げられ、tool の
  1 手ごとには読み上げが飛ばない。
- **状態バッジの `role="status"`**: `~/components/ConsoleBlockItem.tsx::TaskBlockView` の
  `<StatusBadge status={t.to} />` にだけ `role="status"` を追加した（Console はこのブロック自体が SSE で
  生きたまま更新される画面のため）。`/board`・`/tasks`・`/tasks/:id` 等、他の画面の `StatusBadge` は
  `role` を付けていない（スコープを絞った理由は「未解決事項」参照）。

### 監査（機械検査。ルール実装直後 → 全修正後）

| rule | 実装直後 | 全修正後（light） | 全修正後（dark） |
| --- | --- | --- | --- |
| overflow / status-badge / fixed-overlay / font-size / table-wrap / contrast | 0 | 0 | 0 |
| `a11y-name`（新規） | 0 | 0 | 0 |
| `a11y-structure`（新規） | 4（`home`/`org-node` の light+dark） | 0 | 0 |
| `focus-order`（新規） | 0 | 0 | 0 |
| `tap-target`（既存。スキップリンク追加の副作用） | 42（21 route × 2 scheme） | 0 | 0 |
| 合計 | 46 | **0** | **0** |

`pnpm mobile-audit` は 21 route × 2 scheme = 42 通り全て 200 応答、違反 0 件で exit 0
（`by_rule: {}`、`by_scheme: {"light": 0, "dark": 0}`）。

### 証跡（コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint`（`scripts/mobile-audit.mjs` の 1 箇所で `pnpm exec biome check --write .` による自動整形が要った） | exit 0。`Checked 227 files in ...ms. No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0 |
| test | `pnpm test` | exit 0。**Test Files 62 passed (62) / Tests 925 passed (925)**（Phase G31 から変わらず。新規の純粋関数は追加していないため） |
| build | `pnpm build` | exit 0（client・server とも） |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（API 変更なし） |
| mobile-audit（ルール実装直後、修正前） | `pnpm mobile-audit` | exit 1。`{"ok": false, "total": 46, "by_rule": {"tap-target": 42, "a11y-structure": 4}, "by_scheme": {"light": 23, "dark": 23}}` |
| mobile-audit（全修正後） | `pnpm mobile-audit` | **exit 0。violations 0 件**（`by_rule: {}`、`by_scheme: {"light": 0, "dark": 0}`）。21 route × 2 scheme = 42 通り全て 200 応答 |
| a11y-name ルールの検証 | `console-overflow-trigger` の `aria-label` を一時除去 → `pnpm mobile-audit` | exit 1。`{"total": 4, "by_rule": {"a11y-name": 4}}`（`home`/`org-node` の light/dark）。元に戻して再実行し 0 件を確認 |
| focus-order ルールの検証 | `console-text` に一時的に `tabIndex={-1}` → `pnpm mobile-audit` | exit 1。`{"total": 4, "by_rule": {"focus-order": 4}}`（`home`/`org-node` の light/dark、`detail` は「composer 未到達」）。元に戻して再実行し 0 件を確認 |
| フォーカスリングのコントラスト実測（Python、WCAG の相対輝度式） | 修正前: light `ring/20% on surface` = 1.36:1、dark = 1.32:1 | どちらも 3:1 未満（違反） |
| 同上（修正後） | light `--ring` on `--surface`/`--bg`/`--surface-2` = 4.47 / 4.17 / 4.03:1、dark = 6.17 / 6.55 / 5.77:1 | 全組み合わせ 3:1 以上 |

### 変更したファイル

- `scripts/mobile-audit.mjs`（`computeAccessibleName`・`checkA11yNames`・`checkA11yStructure`・
  `checkFocusOrder` を新設、`runChecks`/`addInitScript` バンドル/メインループに配線）
- `app/root.tsx`（スキップリンク「本文へ」、`<main id="main-content" tabIndex={-1}>`）
- `app/components/ui/form.ts`（`FIELD` のフォーカスリングを不透明な `outline-ring` トークンに変更）
- `app/components/Console.tsx`（`<h1 className="sr-only">Console</h1>` を追加）
- `app/components/ConsoleBlockItem.tsx`（`BlockShell` に `busy`、「考え中…」に `aria-live`、
  確定本文の `MarkdownViewer` に `live`、Console の task 状態バッジに `role="status"`）
- `app/components/MarkdownViewer.tsx`（`live?: boolean` プロパティを追加）

### 未解決事項

- **U-G32-1（実機未確認、ADR-0009 P-34 継続）**: VoiceOver / TalkBack・実機キーボードでの読み上げ順・
  読み上げ内容の確認は未実施（認証・ネットワークが使えるサンドボックスではないため）。機械検査は DOM 構造と
  ARIA 属性の存在・Tab キーでの到達性までしか見ておらず、実際に何がどう読み上げられるかは支援技術の実装に依存する。
- **U-G32-2**: `role="status"` は Console の task ブロック（`TaskBlockView`）だけに絞った。`~/hooks/useCelerisStream.ts`
  は root で SSE を受けるたびに全ルートを再検証するので、理屈のうえでは `/board`・`/tasks`・`/tasks/:id` 等の
  `StatusBadge` も「ライブに変わりうる」。ただしそれらは Console のように「1 画面を開いたまま流れを追う」
  用途ではなく、通常のページ全体再検証（React Router の revalidate）で書き換わるだけなので、全部に
  `role="status"` を付けると無関係な操作（他のバッジのフィルタ変更等）のたびに読み上げが増えて「騒がしく
  しない」の精神に反すると判断し、今回は Console に絞った。他画面への広げ方は次のラウンドで検討（下の
  「提案」参照）。
- **U-G32-3**: `contrast` ルール（Phase 75）はテキストの前景色/背景色だけを見ており、フォーカスリング自体の
  コントラストは自動検査していない（今回は Python での手計算のみ。U-G31-2 の解消は「フォーカスリングを
  不透明なトークンに揃えて手計算で確かめる」までで、audit スクリプトへのルール化はしていない）。
- **U-G32-4**: `focus-order` の罠判定は「Tab を押しても `activeElement` の `cssPathRef` が変わらない」を
  唯一の基準にしている。理論上はフォーカスが A → B → A と 2 要素間を往復するループ（B から A に戻るケース）は
  「直前と同じ」判定に引っかからず見逃す。今回の 21 画面では発生しなかった（0 件のまま）が、将来モーダル等の
  実装ミスでそういう罠が入る可能性はある。

### 提案

- **P-G32-1**: `role="status"` を Console 以外（`/board` 等、SSE で頻繁に更新される一覧画面）にも広げるなら、
  「ページ全体の再検証」ではなく「個々の要素が実際に変わったときだけ」読み上げる仕組み（差分検出）が要る。
  今のまま全バッジに付けると再検証のたびに画面上の全バッジが一斉に「読み上げ候補」になり、かえって煩くなる
  おそれがあるため、次にやるなら先に celeris 側 API から「何が変わったか」が拾える形（SSE のペイロードに
  変更フィールドを載せる等）を検討したい。

## Phase G33 — スマホ UX ラウンド 9: 性能予算（ADR-0055、Phase 77。2026-09-21）

celeris 側は無変更（`crates/` 無変更）。Phase 77 の依頼（`gui/CLAUDE.md`・ADR-0055・Phase G32 の続き）に沿って、
`gui/scripts/mobile-audit.mjs` に `perf` ルール（CPU x4 スロットリング下の初回 JS/CSS 転送量・DOM ノード数・
LCP/FCP）を足し、そこが最初に落とした「初回 JS が予算を大きく超える」を、重い依存（`react-markdown`・
CodeMirror・`@xyflow/react`+dagre）と、タスク詳細の非表示タブの部品を `React.lazy` に切り出すことで直した。
あわせて体感速度（Console・ボード・タスク詳細の遷移中スケルトン）を足した。

### 1. 計測（`perf` ルール。受け入れ条件 1）

- `scripts/mobile-audit.mjs`: light scheme のときだけ（実行時間を抑えるため。D1 のこれまでのラウンドと同じ
  判断）、`context.newCDPSession(page)` → `Emulation.setCPUThrottlingRate({ rate: 4 })`（ミッドレンジ機の
  近似）を掛けてから `goto` する。`page.on("response", ...)` で `resourceType() === "script" | "stylesheet"`
  の応答の `content-length` を合算し（ヘッダが無い応答＝SSE 等は数えない）、**`load` が発火した時点で
  listener を外す**（`page.off`）。ここが肝: 外さずに `checkFocusOrder`/`PERF_SETTLE_MS` の待ちまで
  聞き続けると、`React.lazy` の Suspense 境界がハイドレーション直後に発行する動的 `import()` の応答まで
  「初回」に数えてしまい、遅延読み込みで減らしたはずの初回転送量が見かけ上減らない（実際に最初の実装では
  そうなっていたので、`load` 時点で切る設計に直した）。
- FCP/LCP は `addInitScript` で全ページに配線した `PerformanceObserver`（`{ type: "paint" | "largest-contentful-paint",
  buffered: true }`）から読む。DOM ノード数は `document.querySelectorAll("*").length`。
- 予算超過は `perf` ルールの違反（1 指標につき 1 件）。`report.json` の `perf`（route ごとの実測）・
  `perf_budget`（使った予算値）に出し、標準エラーに固定幅の表を 1 回 `console.error`（`biome` が
  `console.log` を禁止しているため。他のルールと同じ作り）で出す。

### 2. 直した内容（受け入れ条件 2）

計測（最適化前 → 最適化後。CPU x4、light）:

| route | js_kb (before→after) | css_kb (before→after) | dom (after) | lcp_ms (after) |
| --- | --- | --- | --- | --- |
| home | 590.7 → 441.6 | 58.7 → 59.0 | 340 | 300 |
| org | 581.1 → 431.5 | 58.7 → 59.0 | 391 | 148 |
| org-node | 590.7 → 441.6 | 58.7 → 59.0 | 355 | 164 |
| org-detail | 581.1 → 431.5 | 58.7 → 59.0 | 632 | 164 |
| projects | 415.6 → 415.4 | 58.7 → 59.0 | 275 | 132 |
| project-detail | 1087.5 → 458.7 | 73.7 → 59.0 | 674 | 192 |
| project-docs | 564.0 → 414.4 | 58.7 → 59.0 | 276 | 144 |
| board | 416.0 → 416.9 | 58.7 → 59.0 | 403 | 140 |
| approvals | 562.5 → 412.9 | 58.7 → 59.0 | 308 | 136 |
| reports | 566.0 → 416.4 | 58.7 → 59.0 | 295 | 144 |
| releases | 415.9 → 415.8 | 58.7 → 59.0 | 346 | 128 |
| knowledge | 571.0 → 421.4 | 58.7 → 59.0 | 281 | 128 |
| knowledge-inbox | 563.5 → 413.9 | 58.7 → 59.0 | 305 | 136 |
| clusters | 411.0 → 410.8 | 58.7 → 59.0 | 312 | 132 |
| accounts | 426.9 → 426.7 | 58.7 → 59.0 | 322 | 132 |
| help | 406.4 → 406.3 | 58.7 → 59.0 | 610 | 176 |
| task-overview | 907.5 → 483.8 | 58.7 → 59.0 | 433 | 164 |
| task-timeline | 907.5 → 483.8 | 58.7 → 59.0 | 336 | 160 |
| task-changes | 907.5 → 483.8 | 58.7 → 59.0 | 321 | 136 |
| task-files | 907.5 → 483.8 | 58.7 → 59.0 | 295 | 148 |
| task-artifacts | 907.5 → 483.8 | 58.7 → 59.0 | 259 | 132 |

（DOM ノード数・LCP/FCP は最適化前後でほぼ変わらない。もともと DOM 数は最大 674〈project-detail〉、
LCP は最大 220ms 前後で、どちらも予算の 1500 / 2500ms に遠く届いていない。CSS がわずかに増えている
のはスケルトンの Tailwind クラスぶん。`project-detail` の CSS だけ 73.7→59.0 と下がっているのは、
`@xyflow/react/dist/style.css`〈15.4KB〉が `WorkTree` を `React.lazy` にしたことでチャンク読み込み後に
届く扱いになり、`load` までの計測に乗らなくなったため）。

- **`react-markdown`/`remark-gfm`（gzip 前 154KB。11 箇所が使う一番重い依存）**: `~/components/MarkdownViewer.tsx`
  の中身を `~/components/MarkdownViewerBody.tsx` に移し、`MarkdownViewer.tsx` は `React.lazy` + `Suspense`
  の薄い窓口にした（プロパティ・呼び出し側は 1 つも変えていない。11 箇所とも無修正）。SSR は
  `renderToPipeableStream`（`app/entry.server.tsx`、既存のストリーミング SSR。ADR-0002 D2 のまま）が
  サーバ側で `import()` を解決してから本文入りの HTML を返すので、最初の応答に中身がそのまま乗る
  （フォールバック `MarkdownSkeleton` が実際に見えるのは、クライアント遷移直後にこのチャンクがまだ届いて
  いない一瞬だけ）。
- **CodeMirror 6（`@codemirror/*`。gzip 前 263KB。`/tasks/:id` の「ファイル」「成果物」タブ、
  `~/components/ArtifactsList.tsx`、`/tasks/:id/runs/:runId` が使う）**: 同じ形で
  `~/components/CodeViewerEditor.tsx`（本体）+ `~/components/CodeViewer.tsx`（`React.lazy` の窓口）に分けた。
  `Suspense` のフォールバックは元々あった「マウント前は `<pre>` で生テキストを出す」を流用（見た目は
  変わらない。中身のテキストは即読める）。
- **`@xyflow/react` + `@dagrejs/dagre`（gzip 前 228KB。`/projects/:id` の「仕事の木」）**: 同じ形で
  `~/components/WorkTreeGraph.tsx`（本体）+ `~/components/WorkTree.tsx`（`React.lazy` の窓口）に分けた。
  外枠（高さ・角丸・`data-testid`）は窓口側に寄せ、本体側の「マウント後にだけ描く」ガード（ADR-0006 D5、
  `ResizeObserver` 依存のため）はそのまま残した（`React.lazy` で遅延しても SSR はこのモジュールの
  `import()` 自体は解決するので、このガードが無いと SSR で落ちる）。
- **タスク詳細の非表示タブ（`~/components/task-changes.tsx`・`~/components/task-files.tsx`）**: `?tab=` で
  5 つのうち 1 つしか同時に出ないのに、`~/routes/tasks.$id.tsx` はこれまで 5 タブぶんの部品を全部静的
  import していた。「変更」「ファイル」タブの部品だけ `React.lazy`（`TaskTabSkeleton` フォールバック）に
  した。兄弟ルート（`/tasks/:id/changes`・`/tasks/:id/files`）は従来どおり静的 import のままなので、
  ビルド時に `[INEFFECTIVE_DYNAMIC_IMPORT]` という Rollup の警告が **SSR ビルドの方にだけ**出る
  （Node はローカルの `require` なので分割の得が無いという指摘で、実害は無い。クライアント側は 2 つの
  エントリ〈タブ入りの `/tasks/:id` と兄弟ルート〉から参照されるので自動的に共有チャンクへ切り出され、
  実測どおり初回 JS が減っている）。「概要」「タイムライン」「成果物」タブは今回そのまま
  （既定で表示される「概要」を遅延させると効果が薄いうえ複雑さが増すため、費用対効果で見送った）。
- **アイコンの棚卸し**: `~/components/ui/Icon.tsx`（ほぼ全画面が読み込む共有チャンク。手書き SVG 46KB）を
  `grep`/`tsc` で全画面から棚卸しし、どこからも使われていなかった `image` を削除した（`info` は
  `~/components/ui/misc.tsx::ALERT_ICON` という `Record<Tone, IconName>` 経由の間接参照でだけ使われて
  いたので、最初に消して `tsc` のエラーで気づいて戻した。直書きしている以上、他のアイコンは軒並みどこかの
  画面で使われているので個別チャンク化ではなく「使っていない分だけ削る」方針にした）。効果は 21 route
  合計で 0.1〜0.2KB/route と小さい（もともと無駄が少なかった）。
- 「dead CSS の削除」「長い一覧の仮想化」は今回は見送った。CSS は最適化前から 58.7〜73.7KB で予算
  120KB の半分以下、DOM ノード数も最大 674（`project-detail`）で予算 1500 に遠いため、実測が「問題ない」と
  示している対象に手を入れる必要が無いと判断した（`@tanstack/react-virtual` は `~/routes/tasks.tsx`
  〈一覧〉が既に使っているので、将来 DOM 数が予算に近づいたらそちらを流用すればよい）。

### 3. 体感速度（受け入れ条件 3）

- `~/components/ui/skeleton.tsx`: `Skeleton`（`animate-pulse rounded-md bg-surface-2`。装飾なので
  `aria-hidden`）を新設。`app/app.css` の `@media (prefers-reduced-motion: reduce)` に `.animate-pulse` を
  足し（既存の `.animate-pulse-dot`・`.animate-fade-in` と同じ扱い）、モーション低減の設定でアニメーション
  だけ止まる（高さはそのまま予約されるのでガタつかない）。
- **Console の吹き出し流**（`~/components/Console.tsx::BlockStream`）: `useNavigation()` で `/`・`/org/:id`
  への遷移が pending か見て、pending の間は中身を `ConsoleStreamSkeleton`（吹き出し 3 つぶんの骨組み）に
  差し替える。枠自体は `h-[clamp(10rem,calc(100dvh-30rem),36rem)]`（フェーズ 71 から固定）なのでレイアウトは
  動かない。`aria-busy` も付けた。
- **ボードの列**（`~/routes/board.tsx`）: 絞り込みフォーム（`method="get"`）を送ると `/board` への
  再ナビゲーションになる。pending の間は各列を `BoardColumnSkeleton`（前回のカード枚数を 1〜4 に丸めて
  同じ枚数ぶんのプレースホルダを積む。高さのガタつきを小さくするため）に差し替える。グリッドの列数・
  `data-testid="board-columns"` は変えない。
- **タスク詳細のタブ**（`~/routes/tasks.$id.tsx`）: `?tab=` の切り替えは `/tasks/:id` への再ナビゲーション
  になり、`changes`/`files`/`timeline` の中身を loader が引き直す。pending の間はタブの中身全体を
  `TaskTabSkeleton` に差し替える（`React.lazy` の `Suspense` フォールバックと同じ形を流用。チャンク待ち・
  ナビゲーション待ちのどちらでも同じ見た目になる）。
- どれも `pnpm test`（node 環境、DOM レンダリングのテストが無い既存方針。`vitest.config.ts` の
  `environment: "node"`）には引っかからない範囲の変更で、`pnpm mobile-audit` は `page.goto` による
  フルナビゲーションしかしない（`navigation.state` が `loading` になる瞬間を作らない）ため、これらの
  スケルトン自体は機械検査には現れない（目視・実機の確認事項として次節に残す）。

### 4. 予算の調整（受け入れ条件 5 の「達成不能なら 10% 増し」）

- 当初案の JS 予算 350KB は、最適化を尽くしても届かない。`entry.client`（React 19 + React Router の
  クライアントランタイム、182KB）・`components`（3 画面以上が共有する UI の自動チャンク、77KB）・`Icon`
  （47KB）・`jsx-runtime`（34KB）・`root`（18KB）だけでどの画面でも約 360KB が土台としてかかり、これは
  アプリ側のコード分割では削れない（フレームワークそのものの重さ）。
- 実測した最重量ルート（`/tasks/:id` の各タブ、`React.lazy` 適用後で 483.8KB）の 10% 増し（≈532KB）を
  `PERF_BUDGET.jsBytes` にした（`CLAUDE.md`／`gui/CLAUDE.md` の「達成不能なら実測値の 10% 増しにして
  理由を書く」を適用）。CSS（120KB）・DOM ノード数（1500）・LCP（2500ms）は当初案のまま
  （実測が余裕を持って収まっているため変更不要）。

### 監査・ゲート（証拠コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint` | exit 0。`Checked 231 files in ...ms. No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0 |
| test | `pnpm test` | exit 0。**Test Files 62 passed (62) / Tests 925 passed (925)**（Phase G32 から件数変わらず。今回追加した新規コードは React コンポーネント〈スケルトン・lazy の窓口〉と監査スクリプトの計測関数で、どちらもこのリポジトリの vitest 方針〈`environment: "node"`、DOM レンダリングのテスト無し〉の対象外。既存テストのうち `test/unit/task-changes.test.ts` 1 件を `TaskChanges` の静的 import 文字列チェックから `React.lazy` 呼び出しの文字列チェックに書き換えた） |
| build | `pnpm build` | exit 0（client・server とも）。SSR ビルドにだけ `[INEFFECTIVE_DYNAMIC_IMPORT]` 警告 2 件（上記「2. 直した内容」参照。実害なし） |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（API 変更なし） |
| mobile-audit（`perf` 実装直後、最適化前） | `pnpm mobile-audit` | exit 1。全 21 route が `perf` で `initial JS ... > 350KB budget` 違反（`by_rule: {"perf": 21}`） |
| mobile-audit（全対応後） | `pnpm mobile-audit` | **exit 0。violations 0 件**（`by_rule: {}`、`by_scheme: {"light": 0, "dark": 0}`）。21 route × 2 scheme = 42 通り全て 200 応答。`perf`（light のみ）を含む全 12 ルールが 0 件 |

### 変更したファイル

- `scripts/mobile-audit.mjs`（`perf` ルール新設: CDP CPU x4 スロットリング、`PerformanceObserver` 配線、
  JS/CSS 転送量・DOM ノード数・LCP/FCP の計測、`checkPerfBudget`、`formatPerfTable`、`report.json` の
  `perf`/`perf_budget`）
- `app/components/MarkdownViewer.tsx`（`React.lazy` の窓口に）／`app/components/MarkdownViewerBody.tsx`（新規、本体）
- `app/components/CodeViewer.tsx`（`React.lazy` の窓口に）／`app/components/CodeViewerEditor.tsx`（新規、本体）
- `app/components/WorkTree.tsx`（`React.lazy` の窓口に）／`app/components/WorkTreeGraph.tsx`（新規、本体）
- `app/routes/tasks.$id.tsx`（`TaskChanges`/`TaskFiles` を `React.lazy` に、`TaskTabSkeleton`、
  `useNavigation` によるタブ遷移中スケルトン）
- `app/components/Console.tsx`（`BlockStream` に `loading`、`ConsoleStreamSkeleton`、`useNavigation`）
- `app/routes/board.tsx`（`BoardColumnSkeleton`、`useNavigation` による絞り込み遷移中スケルトン）
- `app/components/ui/skeleton.tsx`（新規、`Skeleton` primitive）
- `app/components/ui/Icon.tsx`（未使用の `image` を削除）
- `app/app.css`（`prefers-reduced-motion: reduce` に `.animate-pulse` を追加）
- `test/unit/task-changes.test.ts`（`TaskChanges` の import 文字列チェックを `React.lazy` 形に更新）

### 未解決事項

- **U-G33-1（実機未確認、ADR-0009 P-34 継続）**: CPU x4 スロットリングは CDP の近似で、実機（Nothing 2a
  クラスの中位機）での実際の JS 実行時間・LCP は未確認。今回のヘッドレス計測では全 route が LCP 300ms
  未満（予算 2500ms に大きく余裕）だったが、実 celeris・実データ（もっと長いタスク一覧・Console の
  履歴等）ではもっと重くなりうる。
- **U-G33-2**: `~/components/Console.tsx`・`~/routes/board.tsx`・`~/routes/tasks.$id.tsx` に足した
  遷移中スケルトン（受け入れ条件 3）は `pnpm mobile-audit`（`page.goto` のフルナビゲーションしか行わない）
  にも `pnpm test`（DOM レンダリングテスト無し）にも現れない。実際にクライアント側 SPA 遷移
  （タブ切り替え・絞り込みフォーム送信・Console のノード間移動）で意図どおりスケルトンが出てレイアウトが
  ガタつかないかは、実機か `pnpm e2e`（Playwright、今回のゲートには入っていない）での確認が要る。
- **U-G33-3**: JS 予算 532KB は「今回の最適化後の実測 + 10%」であり、今後さらに画面や依存が増えれば
  再び頭打ちになる。次に予算を超えたら、まず「タスク詳細の概要/タイムラインタブも `React.lazy` にする」
  「`components` 共有チャンクの中身を洗う」を検討するとよい（今回は費用対効果で見送った）。
- **U-G33-4**: `perf` は light scheme でしか計らない（D1 のこれまでのルールと同じ判断）。ダークモード
  固有の重さ（無いはずだが）は今回の計測に乗らない。

### 提案

- **P-G33-1**: `React.lazy` にしたタブ・重い部品（`MarkdownViewer`・`CodeViewer`・`WorkTree`・
  `TaskChanges`・`TaskFiles`）について、実機か `pnpm e2e` で「チャンク読み込み中のスケルトン→実体への
  差し替えでガタつかない」「オフライン・低速回線での初回表示が壊れない」を一度確認しておくと安心
  （ADR-0009 P-34、今回のサンドボックスでは未実施）。
- **P-G33-2**: `components`（77KB）・`Icon`（47KB）はどちらも「ほぼ全画面が使う土台」なので、これ以上
  削るなら個々の関数・アイコン単位の分割ではなく、使用頻度の低い画面（`/clusters`・`/accounts` 等）
  だけが使う UI 部品を洗い出して分離する方が筋が良さそう（次のラウンドで検討）。
## celeris 側 Phase 78（ADR-0056、MCP サーバー）に伴う最小追従（2026-09-21）

新しい GUI フェーズではなく、celeris 側 Phase 78 の受け入れ条件（型再生成・最小ラベル）に合わせただけの
差分。詳細な決定は celeris 側 `docs/PROGRESS.md` の Phase 78 節を参照。

- `pnpm gen:types`: `ConsoleBlock`（`human`）に `author?: string | null`、`Profile` /
  `EffectiveProfile` に `skills_mounts?: string[]`、`McpScope` / `McpClient` / `McpClientsView` /
  `McpCall` / `McpCallsView` が増えた（`GET /mcp/clients` / `GET /mcp/calls`。GUI からはまだ呼んでいない）。
- `app/components/ConsoleBlockItem.tsx`: `human` ブロックに `block.author` があれば
  `外部（<mcp: を外した client_id>）` の `Badge` を 1 つ出すだけ（`data-testid="console-human-author"`）。
  MCP クライアントの表示名解決（`GET /mcp/clients` を引いて `name` を出す）はしていない
  （`author` の生の値をそのまま見せる最小実装。次のラウンドでやるなら「アカウント」画面の
  「MCP クライアント」節と合わせて設計するとよい）。
- ゲート: `pnpm typecheck` / `pnpm lint` 差分無し、`pnpm test`（**925 passed**、Phase G32 と同数。
  今回のラベルにテストは追加していない）、`pnpm build` 成功。

## Phase G34 — MCP クライアントの GUI（ADR-0056 D4、celeris Phase 78。2026-09-21）

celeris 側は無変更（`crates/` 無変更。celeris は既に main に Phase 78 が入っており、`GET /mcp/clients` /
`GET /mcp/calls?client=` は使える状態）。celeris 側 Phase 78 の PROGRESS 節が残した「「アカウント」画面の
MCP クライアント節は後続の GUI Phase」を実装した。Phase 66 の「LLM source」節（`~/routes/accounts.tsx`）を
パターンに、`GET /mcp/clients` をそのまま表示する節を足し、Console の「外部（<client_id>）」帯（Phase 78 の
最小実装。id をそのまま出していた）を `GET /mcp/clients` の `name` で解決するようにした。

### 1. `/accounts` の「MCP クライアント」節（受け入れ条件 1）

- `app/routes/accounts.tsx`: `loadAccounts` に `GET /mcp/clients` を追加（`GET /llm/sources` /
  `GET /secrets` と同じ規律 — 落ちても `/accounts` 自体は壊さず `mcpClientsError` に入れる。管理系の
  トークン必須エンドポイントなので 401 になりうる）。`McpClientsSection` → `McpClientCard` を新設:
  - id/name（`McpClient.name` を主見出し、`id` は `Mono` の副題）
  - 認証の種類バッジ（`token_hash` の有無から `token`/`none`。1 語、`data-status-badge="mcp-auth"`）
  - 失効状態バッジ（`revoked_at` の有無から `active`/`revoked`。1 語、`data-status-badge="mcp-client"`）
  - スコープのチップ（`docs/mcp.md` §4 の並びに揃えて重複を落とす。`~/lib/mcp.ts::sortMcpScopes`/`mcpScopeLabel`）
  - `last_used_at` は `relativeTimeLabel` + `title` に絶対時刻（無ければ「未使用」）
  - 客 1 件ごとの直近の呼び出し（`GET /mcp/calls?client=<id>`）を `<details>` の開閉で遅延取得する
    `McpClientCallsDisclosure`（`~/components/ConsoleBlockItem.tsx::ProgressBlockView` の「すべて見る」と
    同じ `useFetcher().load()` の作り）。新設した resource route `app/routes/mcp.clients.$id.calls.ts`
    （`routes/tasks.$id.runs.$runId.events.ts` と同じ形）が `GET /mcp/calls?client=` を中継する。
    呼び出し 1 件は tool / ok・error バッジ / latency_ms / 時刻を表示。
  - 「接続のしかた」の一言と `/help#mcp` へのリンク（`docs/mcp.md` の要約）。
  - モバイル幅（393px）でカードは流動幅（`grid xl:grid-cols-2`。ADR-0055 D2 と同じ作り）。

### 2. Console の「外部（<client name>）」帯（受け入れ条件 2）

- `~/lib/mcp.ts::resolveMcpAuthorLabel(author, clients)`: `mcp:<client_id>` を `GET /mcp/clients` の
  `name` に解決し、見つからなければ id にフォールバック（未取得・失効後に消えた等）。人の発言
  （`author` 無し）は `null`（帯を出さない）。
- `~/celeris/console.server.ts::loadConsole` に `GET /mcp/clients` を追加（`org`/`projects` と同じ
  ベストエフォート — 落ちても Console 自体は出し、名前解決は id にフォールバックするだけ）。
  `~/lib/console.ts::ConsoleData` に `mcpClients: McpClient[]` を追加。
- `~/components/Console.tsx` → `BlockStream` → `~/components/ConsoleBlockItem.tsx::ConsoleBlockItem`
  まで `mcpClients` を通し、`HumanBlockView` が `resolveMcpAuthorLabel` を呼ぶように変更（従来の
  `block.author.replace(/^mcp:/, "")`〈id をそのまま出す最小実装〉を置き換え）。帯は人の発言と同じ
  右寄せのまま、`Badge tone="info"`（元は `neutral`）にして目立ちすぎない程度に区別を付けた。
- `~/routes/tasks.$id.tsx` の「タイムライン」タブは `Message`/`ConsoleBlock` の `human`/`reply`
  ブロックを描画しておらず（`TaskComment`〈`task_comments`、`CommentAuthorKind = human|node|system`〉を
  描画する別のデータモデル。MCP の `author` は `messages.metadata` の欄で、`task_comments` とは無関係）、
  `~/components/ConsoleBlockItem.tsx::ReplyStepRow` だけを再利用している（`worker_progress` の 1 手表示）。
  確認の結果、著者を表示する箇所ではないため変更していない。

### 3. `/help` の「MCP で外から使う」（受け入れ条件 3）

- `app/routes/help.tsx` に `Section id="mcp"` を追加（TOC にも追加）。外部エージェントは人ではないこと
  （案件・タスクは CoS 経由、知識は `_inbox` 経由、`tools`/`permissions`/`review` は変更不可）、8
  スコープの一覧、2 通りのつなぎ方（`auth = "token"` の Bearer 口 / `auth = "none"` の loopback 限定
  トンネル専用口）を簡潔にまとめ、`docs/mcp.md` と `/accounts#mcp-clients` にリンクした。

### 4. モックデータ・vitest（受け入れ条件 4）

- `test/unit/mcp.test.ts`（新規）: `sortMcpScopes`（並び順・重複排除・空入力）、`mcpScopeLabel`（全
  スコープが非空ラベルを持つ）、`mcpAuthKindWord`（token/none）、`mcpClientStatusWord`（active/revoked）、
  `resolveMcpAuthorLabel`（null 早期リターン・name 解決・id フォールバック）— 計 11 件。
- `test/unit/accounts.test.ts`: `loadAccounts` が `GET /mcp/clients` をそのまま `mcpClients` に渡すこと、
  401 のとき `mcpClientsError` に落ちて `/accounts` 自体は壊れないこと — 2 件追加。
- `test/unit/console.server.test.ts`: `loadConsole` が `GET /mcp/clients` を `mcpClients` に束ねること、
  落ちても Console 自体は出す（空扱い）こと — 2 件追加。
- `scripts/mobile-audit.mjs`: `GET /api/v1/mcp/clients`（token 付き・有効な客 1 件 + `--no-token` で
  失効済みの客 1 件。auth = token/none・状態 = active/revoked の全パターン）、`GET /api/v1/mcp/calls`
  （ok 1 件・error 1 件）を追加。`GET /api/v1/console` の既定の流れに `author = "mcp:chatgpt"` の
  human ブロックを混ぜ、「外部（chatgpt）」の帯（id ではなく `GET /mcp/clients` の name で解決される
  こと）も `home`/`org-node` などの route で監査に通した。

### ゲート（証拠コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| 型の再生成 | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | exit 0、差分ゼロ（`McpClient` 等は Phase 78 で既に生成済み。今回は型の変更なし） |
| lint | `pnpm lint` | exit 0。`Checked 234 files in ...ms. No fixes applied.`（Biome の自動整形を先に適用してから確認） |
| typecheck | `pnpm typecheck` | exit 0（`~/routes/home.tsx`/`~/routes/org.$id.tsx` の `isCelerisUnavailable` フォールバック値に `mcpClients: []` を追加して解消した 2 件を含む） |
| test | `pnpm test` | exit 0。**Test Files 63 passed (63) / Tests 940 passed (940)**（Phase G33 の 925 から +15。内訳は上記「4.」） |
| build | `pnpm build` | exit 0（client・server とも）。`mcp.clients._id.calls` チャンクが増えた以外、既存の `[INEFFECTIVE_DYNAMIC_IMPORT]` 警告 2 件は Phase G33 から変化なし |
| mobile-audit（1 回目、`/help#mcp` へのインラインリンクの tap 領域を広げる前） | `pnpm mobile-audit` | exit 1。`tap-target` 違反 2 件（light/dark 各 1）: `/accounts` の「MCP クライアント」節の案内文中のインラインリンクが 202.1×17.0px（44×44 未満） |
| 対応 | `app/routes/accounts.tsx` | そのリンクに `touchLinkClass`（`-my-2.5 inline-flex min-h-11 min-w-11 items-center py-2.5`。`~/routes/help.tsx` の本文中リンクと同じ形）を追加 |
| mobile-audit（対応後） | `pnpm mobile-audit` | **exit 0、violations 0 件**（`by_rule: {}`、`by_scheme: {"light": 0, "dark": 0}`）。21 route × light/dark、`perf` を含む全 12 ルールが 0 件。`accounts` route の初回 JS は 432.3KB（予算 532KB 以内） |

### 変更したファイル

- `app/lib/mcp.ts`（新規）: `sortMcpScopes` / `mcpScopeLabel` / `mcpAuthKindWord` / `mcpClientStatusWord` / `resolveMcpAuthorLabel`
- `app/routes/mcp.clients.$id.calls.ts`（新規、resource route）・`app/routes.ts`（登録）
- `app/routes/accounts.tsx`（`AccountsData.mcpClients`/`mcpClientsError`、`McpClientsSection`/`McpClientCard`/`McpClientCallsDisclosure`/`McpCallRow`、`touchLinkClass` の追加インポート）
- `app/routes/help.tsx`（`Section id="mcp"`、TOC、`Mono` の追加インポート）
- `app/lib/console.ts`（`ConsoleData.mcpClients`）・`app/celeris/console.server.ts`（`loadConsole` に `GET /mcp/clients` を追加）
- `app/components/Console.tsx`（`mcpClients` を `BlockStream` → `ConsoleBlockItem` へ中継）
- `app/components/ConsoleBlockItem.tsx`（`ConsoleBlockItem`/`HumanBlockView` に `mcpClients` を追加、`resolveMcpAuthorLabel` で名前解決）
- `app/routes/home.tsx`・`app/routes/org.$id.tsx`（`isCelerisUnavailable` フォールバックに `mcpClients: []`）
- `scripts/mobile-audit.mjs`（`/api/v1/mcp/clients`・`/api/v1/mcp/calls` のモック、Console の流れに MCP 発の human ブロックを追加）
- `test/unit/mcp.test.ts`（新規）・`test/unit/accounts.test.ts`・`test/unit/console.server.test.ts`（テスト追加）

### 未解決事項

- **U-G34-1**: `docs/mcp.md` §7.2/7.3（Claude Code / Codex の実際の接続）と同様、この GUI 節が表示する
  `GET /mcp/clients`/`GET /mcp/calls` の実データでの見た目（本物の celeris + 本物の MCP クライアントの
  呼び出し履歴）は未確認（ADR-0009 P-34。サンドボックスに外向きネットワークも実物のクライアントも無い）。
  `test/mock-celeris` と `scripts/mobile-audit.mjs` の作り物のデータでのみ確認した。
- **U-G34-2**: `McpClientCallsDisclosure` は `<details>` を開いたときにだけ `GET /mcp/clients/:id/calls`
  を取りに行く作りのため、`pnpm mobile-audit`（`page.goto` のフルナビゲーションしかしない。閉じた
  `<details>` の中身は開かない）では実際に開いた状態の見た目・追加のネットワーク往復は検査していない。
  `pnpm e2e`（今回のゲートには入っていない）か実機での確認が要る。
- **U-G34-3**: `/tasks/:id` のタイムラインタブは `Message`/`ConsoleBlock` を描画しない設計だと確認した
  （「2.」参照）が、SPEC/ADR のどこにも明記が無く GUI コードから読み取っただけの理解。将来タイムラインに
  対話（Console の human/reply）が混ざる設計変更があれば、この Phase の `resolveMcpAuthorLabel` をそこにも
  適用する必要がある。

### 提案

- **P-G34-1**: `docs/mcp.md` §7.2/7.3 の実機接続確認（celeris 側 Phase 78 の未解決事項）が終わったら、
  ついでにこの GUI 節の実データ表示（U-G34-1）も確認するとよい（同じ celeris インスタンスに対して行える）。
- **P-G34-2**: `McpClientCard` の「直近の呼び出し」はクライアント 1 件あたり最大 100 件（celeris 側の
  上限）を毎回取り直す作り。呼び出しの多い客が増えたら、`~/routes/reports.tsx` のページングのような
  「もっと見る」の分割を検討する余地がある（今回は Phase 78 の規模〈直近 100 件〉に見合う簡潔な実装を
  優先した）。

## Phase G35 — skills を GUI から見る・作る・mount する（ADR-0056 D3 続き、celeris Phase 82。2026-09-21）

celeris 側 Phase 82（`GET/PUT/DELETE /skills…`、`POST/DELETE /org/{id}/skills…`。§3.112〜3.117）に乗って
GUI を実装した。`/knowledge` に skills タブ（別ルート `/knowledge/skills`）を足し、一覧・詳細・作成・更新・
削除を行える。mount する先（どのノードに効かせるか）は ADR-0056 D3「mount が門」の設計どおり
`/org` 画面の担当詳細に持たせた（skills 画面からは mount できない。読み取り専用で「mount しているノード」を
見せるだけ）。

### 1. `/knowledge/skills`: skill の一覧・閲覧・作成・更新・削除（受け入れ条件 2 前半）

- `app/celeris/skills.ts`（`loadSkills`/`readSkillsQuery`。`~/celeris/knowledge.ts` と同じ「一覧 + 選んだ
  1 件」の形）、`app/celeris/skills-admin.server.ts`（`putSkill`/`deleteSkill`/`mountSkill`/`unmountSkill`/
  `readSkillPutBody`/`readSkillName`。`~/celeris/knowledge-admin.server.ts` と同じ規律 — GUI 側で検証せず
  celeris の 422/409 の文言をそのまま出す）。
- `app/routes/knowledge.skills.tsx`（新規ルート、`app/routes.ts` に登録）: 左にカード一覧
  （`name`/`description`/`updated`/`mounted_by` のバッジ）、右に詳細（SKILL.md を `MarkdownViewer`
  〈lazy、既存部品をそのまま使う〉で描画・付属ファイルの一覧・mount しているノードへのリンク・削除）
  またはエディタ（`?create=1`/`?edit=1`。名前欄 + `skill_md` のテキストエリア）。
  `app/routes/knowledge.tsx` に「skills」への導線を追加（`knowledge.inbox.tsx` と同じ兄弟ルートの形）。
- 削除は mount されている間は celeris が 409 `skill_mounted` で断る。GUI はその文言をそのまま出し、
  「先に組織画面で外してから」と案内するだけ（判定は celeris 側）。

### 2. `/org` の担当詳細: 「mount された skills」節（own / inherited、mount / unmount。受け入れ条件 2 後半）

- `app/routes/org.tsx::MountedSkillsSection`: そのノード自身の `profile.skills_mounts`（own）と、
  `GET /org` が返す `effective_profiles[].skills_mounts`（継いだ後）の差分から `inherited`（親から継いだ
  もの）を出す。own は unmount できる（`DELETE /org/{id}/skills/{skill}`）が、inherited は「継承」バッジ
  だけを見せて操作は出さない（ADR-0056 D3「mount が門」を GUI 側でも守る — 親ノードで外すよう案内する）。
  own/inherited の分割は `~/lib/skills.ts::splitMountedSkills`（純粋関数。`GET /org` の継承計算を GUI で
  やり直しているわけではなく、celeris が返した 2 つの配列の差分を取るだけ）。
  - mount の picker は `GET /skills`（`loadOrg` に追加。`[knowledge] root` 未設定で 409 になっても
    `null` にして「mount された skills」節は own/inherited の表示だけに落とす — 画面は壊さない）。
  - mount/unmount は `OrgOpOutcome` とは別の結果型 `OrgSkillMountOutcome`（`app/celeris/action-types.ts`）
    を使う別の `useFetcher`（`key: "org-skills"`）。既存の作成・削除・profile 編集フォームの
    `fetcher.data` を汚さないため。`action()` の `switch` に `skill_mount`/`skill_unmount` の 2 intent を
    追加しただけで、既存の intent の挙動は 1 バイトも変えていない。

### 3. `~/lib/skills.ts`（純粋関数。DOM を描画する unit テストが無い〈G10-U1〉ので、判断はここに集める）

- `isValidSkillName`（celeris の `[a-z0-9-]{1,64}` と同じ規則）、`skillsHref`（画面の URL）、
  `skillMarkdownTemplate`（新規作成フォームの雛形）、`parseSkillFrontMatter`/`skillMarkdownProblem`
  （送る前に celeris の 422 を先に見せるだけ。判定の正本は celeris）、`splitMountedSkills`
  （own/inherited）。
- **P-G35-a（監査対応の追記）**: `skillMarkdownBody`/`splitSkillFrontMatter` を追加した。`SKILL.md` の
  本文は `# <name>` の見出しから始まる慣習（`skillMarkdownTemplate` 自身もそう作る）があり、それを
  そのまま `MarkdownViewer` に渡すと `react-markdown` が実際の `<h1>` を生成し、`PageHeader` の
  `<h1>「skills」` と合わせて 1 画面に h1 が 2 つになって `pnpm mobile-audit` の `a11y-structure`
  （ADR-0055 D1 拡張、Phase 76）に落ちた。`knowledge.tsx`（`prepareKnowledgeBody`）と同じ「表示のため
  front matter を落とす」に加え、見出しレベルを 1 段落とす（`skillMarkdownBody`）ことで、SKILL.md が
  何を書いていても「1 画面 1 つの h1」を保つ。

### ゲート

`pnpm gen:types && git diff --exit-code app/celeris/types.ts`（celeris 側スキーマに
`SkillList`/`SkillDetailView`/`SkillSummaryView`/`SkillPutBody`/`SkillPutResult`/`SkillFileBody`/
`OrgSkillMountBody` が増えた分だけ。2 回連続で `gen:types` を実行し出力が同一であることを確認 =
冪等）/ `pnpm typecheck` / `pnpm lint`（biome、`--write` の自動整形を適用）/ `pnpm test`
（**965 passed**、Phase G34 の 940 から +25 = `test/unit/skills.test.ts` 新設）/ `pnpm build` すべて exit 0。

`pnpm mobile-audit`:

| 段階 | コマンド | 出力の要点 |
|---|---|---|
| 1 回目（新ルート追加後、監査対応の前） | `pnpm mobile-audit` | exit 1。**14 件**（`tap-target` 8・`font-size` 4・`a11y-structure` 2）。`org-detail`: mount 解除ボタンとリンクのタップ領域不足（`Button size="xs"` は `text-xs` が常時 12px、`select` に上書きした `h-8 text-xs` が `selectClass` 既定の `h-11 lg:h-9`/`text-sm` を潰していた）、own の skill 名リンクが `min-h-11` 無しで 20px 高。`knowledge-skill-detail`: mount 先バッジへのリンクとヒント文中リンクが 44×44 未満、SKILL.md 本文の `# rust-review` が `react-markdown` で実際の `<h1>` になり `PageHeader` の h1 と重複 |
| 対応 | `app/routes/org.tsx`・`app/routes/knowledge.skills.tsx`・`app/lib/skills.ts` | 「外す」ボタンを `size="sm"`（`text-sm` 常時）に、picker の `select` は `selectClass` の上書きをやめて `max-w-xs` だけ足す、own の skill 名リンクに `flex min-h-11 items-center`、孤立リンク（mount 先バッジ・「組織へ」「skills へ」）に `touchLinkClass`（`~/components/ui/form.ts` の既存クラス）、SKILL.md の描画に `skillMarkdownBody`（P-G35-a）を通す |
| 2 回目（対応後） | `pnpm mobile-audit` | **exit 0、violations 0 件**（`by_rule: {}`、`by_scheme: {"light": 0, "dark": 0}`）。23 route × light/dark + `perf`。新ルート `knowledge-skills`（412.6KB）・`knowledge-skill-detail`（412.6KB）とも予算（532KB）内、DOM ノード数・LCP も他ルートと同水準 |

celeris 側のゲート（同じセッションで実装。詳細は `docs/PROGRESS.md`「Phase 82」）: `cargo test --workspace
--no-fail-fast` **1780 passed / 0 failed**（exit 0）、`cargo clippy --workspace --all-targets -- -D
warnings` exit 0。`crates/task-ops/src/knowledge.rs` に `set_skill_mount`/`skills_delete` を追加し、
`crates/celeris-mcp/src/tools/org.rs::mount_common` を**同じ関数を呼ぶ**ようリファクタしたので、
mount/unmount の挙動は GUI 経由でも MCP 経由でも同一（`docs/adr/0056-mcp-server.md`「Phase 82 追記」参照）。

### 変更したファイル

- `app/celeris/skills.ts`・`app/celeris/skills-admin.server.ts`（新規）
- `app/celeris/action-types.ts`（`SkillOpOutcome`/`OrgSkillMountOutcome`）
- `app/lib/skills.ts`（新規。純粋関数一式）
- `app/routes/knowledge.skills.tsx`（新規）・`app/routes.ts`（登録）・`app/routes/knowledge.tsx`（導線）
- `app/routes/org.tsx`（`OrgData.skills`、`loadOrg` に `GET /skills`、`action()` に `skill_mount`/
  `skill_unmount`、`MountedSkillsSection` 新設、2 本目の `useFetcher`）
- `test/mock-celeris/fixtures.ts`（`skillSummary`/`skillList`/`skillListEmpty`/`skillDetail`/
  `skillPutResult`、`orgList()` の `coding` に `profile.skills_mounts` と `effective_profiles` を追加）
- `test/mock-celeris/server.ts`（`serveSkills`/`serveOrgSkillMount`）
- `test/unit/skills.test.ts`（新規、25 件）
- `scripts/mobile-audit.mjs`（`knowledge-skills`/`knowledge-skill-detail` route、`GET /api/v1/skills`/
  `GET /api/v1/skills/rust-review` のモック）

### 未解決事項

- **U-G35-1**: mount/unmount を GUI から実際に celeris（本物の `[knowledge] root` + 組織）に対して行い、
  `GET /org` の `effective_profiles` が正しく更新されて見えることは未確認（ADR-0009 P-34。サンドボックス
  に外向きネットワークも実物の celeris も無い）。`test/mock-celeris` と `scripts/mobile-audit.mjs` の
  作り物のデータでのみ確認した。celeris 側 Phase 82 の実機確認（`docs/PROGRESS.md`「Phase 82」参照）と
  合わせて行うとよい。
- **U-G35-2**: skill の作成フォームは `files`（付属ファイル）の入力欄を GUI に出していない
  （`readSkillPutBody`/`SkillPutBody.files` は実装済みだが、`SkillEditor` は `skill_md` だけの単純な
  フォーム）。SKILL.md 本体だけで足りる運用を想定した最小実装で、付属ファイルが要る skill は今のところ
  `skills_put`（MCP）か将来のフォーム拡張で足す前提。
- **U-G35-3**: 「mount された skills」の picker は `own` に既にある skill 名だけを候補から外し、
  `inherited`（親から継いでいるだけ）の skill 名はそのノードでの「明示的な own mount」として再度選べる
  仕様のままにした（celeris 側が重複を弾かないので害は無いが、GUI からその状態を意図して作る導線は
  用意していない）。

### 提案

- **P-G35-1**: U-G35-2 の付属ファイル入力は、使う場面が出てきたら `~/routes/org.tsx::ProfileEditForm`
  の「知識（knowledge）」欄と同じ「固定数の空行 + 並行配列」パターンで足せる。
- **P-G35-2**: `MountedSkillsSection` の picker は現状 `GET /skills` の全件を毎回渡すだけ（件数の上限は
  celeris 側の `MAX_INDEX_ITEMS` 相当に準じる想定）。skill の数が増えたら検索欄を足すか、
  `~/components/ui/form.ts` の `multiSelectClass` を使った複数選択に変えるとよい。

## Phase G36 — `pnpm e2e:staging`/`pnpm e2e:mock`: verify.sh の検査 4b が使う read-only e2e（celeris Phase 83、ADR-0041 D3/D5、ADR-0055 D3。2026-09-21）

celeris 側 `verify.sh`（`docs/PROGRESS.md`「Phase 83」）が staging（本番 DB のスナップショット）に対して
GUI の e2e を検査 4b として組み込めるように、**読み取り専用**の e2e を新設した。既存の `gui/e2e/*.spec.ts`
（`pnpm e2e`）は `scripts/celeris.sh` で自分専用の celeris を起こしタスクを作る結合テストで、staging の
「検査 6 の煙試験だけが書き込む」という前提を壊すため、そのままでは使えなかった（ADR-0055 D3 が残した
既知のギャップ。celeris 側の背景は `docs/PROGRESS.md`「Phase 83」参照）。

### 1. `gui/e2e/*.spec.ts`（`pnpm e2e`）の棚卸し

`g0`〜`g9`・`g13`・`g5-a11y`・`g5-release` の 13 ファイルすべてが `execFileSync` で `scripts/celeris.sh`
（`build`/`fixture`/`start`/`stop`）を呼ぶ、**自分専用の使い捨て celeris を前提にした結合テスト**だった。
読み取りだけのアサーションを持つファイルもセットアップ自体が書き込みを伴うため、1 ファイルも staging へ
そのまま流用できず、**全 13 ファイルを `pnpm e2e` 用のまま無変更で残した**。

### 2. `gui/scripts/lib/celeris-fixture.mjs`（新規。`mobile-audit.mjs` からの切り出し）

`gui/scripts/mobile-audit.mjs`（ADR-0055 D1）が持っていた「偽の celeris」（`node:http`、
`test/mock-celeris/fixtures.ts` の値）と「画面一覧」をここに切り出した。画面一覧は
`buildRoutes({taskId, projectId, orgId, orgHeadId, skillName})` という関数にし、引数省略時は
`mobile-audit.mjs` がこれまで使っていた固定の fixture id（`TASK_ID`/`PROJECT_ID`/`ORG_NODE_ID`
`= "coding-poc"`/`ORG_HEAD_ID = "coding"`/`SKILL_NAME`）になる。`mobile-audit.mjs` は
`import { ROUTES, getFreePort, setupMockCeleris, waitForHealth } from "./lib/celeris-fixture.mjs"` に
変え、自前で持っていた同名の関数・定数を削除した。**切り出しの前後で `pnpm mobile-audit` の出力が
変わらないことを確認済み**（下の「ゲート」参照。途中 `/org?selected=coding` を誤って `orgId`
〈`coding-poc`〉と混同する取り違えを 1 度入れてしまい `pnpm mobile-audit` が 254 件の `overflow` 違反を
出したが、`orgHeadId`〈部門長ノード。葉ノードの `orgId` とは別の id〉として分離して直した）。

### 3. `gui/scripts/e2e-check.mjs`（新規。`pnpm e2e:mock`/`pnpm e2e:staging` の実体）

`mobile-audit.mjs` と同じ作り（`@playwright/test` から Playwright の test runner を使わず `chromium`
だけを `createRequire` で借りる、生の Node スクリプト）で書いた。2 通りのモード:

- **`pnpm e2e:mock`**（`E2E_GUI_URL` 未設定）: `setupMockCeleris()` と `pnpm build` 済みの GUI を
  自分のポートに起こす。CI・オフラインでの動作確認用。
- **`pnpm e2e:staging`**（`E2E_REQUIRE_STAGING=1`。`E2E_GUI_URL`/`E2E_API_URL`/`E2E_TOKEN_FILE`）:
  **何も起動しない**。`E2E_GUI_URL` に接続するだけ。`discoverIds()` が **Node 側の `fetch`**
  （ブラウザではない。gui/CLAUDE.md「ブラウザから celeris を直接呼ぶコードを書かない」を守る）で
  `E2E_API_URL` から `taskId`/`projectId`/`orgId`/`orgHeadId`/`skillName` を読む。見つからなければ、
  その id が要る画面と `/tasks/<id>` のタブ切り替え検査をスキップし、理由を `notes` に積む
  （タスクは絶対に作らない）。

検査する内容（両モード共通）:

1. `buildRoutes()` の画面一覧を 393×851 と 1280×800 の両方で `page.goto`（`waitUntil: "load"`）し、
   200 応答・コンソールエラー無し・失敗した要求（`status >= 400`。**401 のみ許容**）無しを確認。
2. `/`（desktop viewport のときだけ）: `[data-testid="console-screen"]` が描画されること。
   ブロック数（`[data-console-block]` の件数）は `notes` に参考情報として出す（0 件でも失敗にしない
   — staging のスナップショットに Console 発言が無いことはありうるため）。
3. `/accounts`: `[data-testid="llm-sources-section"]`・`[data-testid="mcp-clients-section"]` が
   描画されること（`app/routes/accounts.tsx` はデータが空でもこの 2 節の外枠は常に描く作りなので、
   0 件データでも失敗にならない）。
4. `/knowledge/skills`: `[data-testid="knowledge-skills"]` が描画されること。
5. `ids.taskId` があれば `/tasks/<id>?tab=overview` を開き、5 つのタブ（`TASK_TABS` と同じ
   overview/timeline/changes/files/artifacts）を `[data-testid="task-tab-<tab>"]` の `<Link>` クリックで
   切り替え、対応する節（`info-section`/`timeline-section`/`changes-section`/`files-section`/
   `artifacts-section`）が `waitForSelector` で出ることを確認。`?tab=` を差し替えるだけの `<Link>`
   （`replace` ナビゲーション）で `POST` は一切無い。

外部ネットワーク不使用の後押しとして、`context.route("**/*", ...)` で GUI のオリジン以外への要求を
`abort` する（`mobile-audit.mjs` と同じ。`page.on("response")` の失敗要求判定はこの遮断対象を除外して
数える — 自分で塞いだ要求を「失敗した要求」として誤検知しないため）。

`gui/package.json`: `"e2e:mock": "node scripts/e2e-check.mjs"`、
`"e2e:staging": "E2E_REQUIRE_STAGING=1 node scripts/e2e-check.mjs"`。`E2E_REQUIRE_STAGING=1` かつ
`E2E_GUI_URL` 未設定なら usage を出して exit 2（`pnpm e2e:staging` を env 無しで直接叩いたときに
mock にフォールバックして誤解させないため）。`@playwright/test` が無い環境（celeris 側の release の
`gui/`。`pnpm install --prod` で devDependency が剥がれる）では exit 3 で「未インストール」を返す
（クラッシュしない）。`gui/.gitignore` に `test/e2e-check/`（レポート置き場。`test/mobile-audit/` と
同じ扱い）を追加。

### ゲート

`pnpm gen:types && git diff --exit-code app/celeris/types.ts`（celeris の API 契約は変えていないので
差分ゼロ）/ `pnpm typecheck`（新規 `.mjs` 2 本は `tsconfig.node.json` の `scripts/**/*.mjs` に含まれ
checkJs で型検査される。`mobile-audit.mjs` と違い DOM/ブラウザの関数を `toString()` で送る作りではない
ので除外リストには入れず、`node:http`/`node:net` の戻り値や `catch (e)` の `e` に JSDoc で型を付けて
素通りさせた）/ `pnpm lint`（biome。import の並び順と `test/e2e-check/report.json` の整形を `--write`
で 1 回直した）/ `pnpm test`（**965 passed**、Phase G35 から変化なし — 新規ファイルに unit テストは
足していない。判断ロジックを持つ純粋関数を切り出していないため）/ `pnpm build` すべて exit 0。

`pnpm mobile-audit`（`MOBILE_AUDIT_SKIP_BUILD=1`）→ **exit 0、violations 0 件**（`gui/scripts/lib/
celeris-fixture.mjs` への切り出し前後で出力が変わらないことを確認。1 回目のビルド直後の実行では
`focus-order` が 1 件出たが、同じビルドに対する 2 回目の実行では 0 件になったので、ビルド直後の CPU
負荷による既知のフレーク〈Tab キー送出のタイミング依存〉と判断した）。

`pnpm e2e:mock` → **`{"ok": true, "mode": "mock", "failures": [], "notes": ["home: rendered 7 console
block(s)"]}`、exit 0**（`pnpm build` からの通しと `E2E_SKIP_BUILD=1` の両方で確認）。`pnpm e2e:staging`
の staging 経路（`discoverIds`・自前で何も起動しない）は、偽の celeris + GUI を自分のポートに起こして
外側から `E2E_REQUIRE_STAGING=1 E2E_GUI_URL=... E2E_API_URL=...` で呼ぶ使い捨てのスクリプトで確認した
（`{"ok": true, "mode": "staging", ...}`、`discoverIds` が実際に `GET /projects`/`/org`/`/skills`/`/tasks`
を叩いて id を見つけ、タブ切り替えまで通ることを確認。リポジトリには残していない）。

celeris 側（`scripts/selfdeploy/verify.sh` に検査 4b として組み込み。詳細は `docs/PROGRESS.md`
「Phase 83」）: `bash -n scripts/selfdeploy/verify.sh` / `bash -n scripts/selfdeploy/lib.sh` exit 0。
`cargo test --workspace`/`cargo clippy` はホストのディスク逼迫（`docs/PROGRESS.md`「Phase 83」の
「重大: ディスク」参照）のため未実行 — `crates/` はこの Phase で無変更。

### 変更したファイル

- `scripts/lib/celeris-fixture.mjs`（新規）
- `scripts/e2e-check.mjs`（新規）
- `scripts/mobile-audit.mjs`（上の lib に委譲。出力は無変更）
- `package.json`（`e2e:mock`/`e2e:staging`）・`.gitignore`（`test/e2e-check/`）

### 未解決事項

- **U-G36-1**: `pnpm e2e:staging` を本物の staging（celeris 側の `verify.sh` が起こす release ビルドの
  GUI）に対して実際に走らせたことはまだ無い（celeris 側 Phase 83 の未解決事項と同じ）。オフラインの
  代替確認（偽の celeris + GUI）でコード経路は通したが、本物の release ビルド（`pnpm install --prod`
  済みの `gui/`）から `$SD_REPO/gui` の Playwright を使って staging を叩く、という celeris 側の分岐は
  実機で確認できていない。
- **U-G36-2**: staging のスナップショットに `tasks`/`projects`/`org` が 1 件も無いと、動的な画面は
  すべてスキップされ `ok` が固定画面だけの検査で真になりうる（celeris 側「Phase 83」の未解決事項と同じ）。
- **U-G36-3**: コンソールのブロック数・失敗した要求の**内容**までは検査結果に反映しない（`notes` に
  出すだけ）。将来 GUI 側でこの検査結果を見せる画面を作るなら、`report.json`（`test/e2e-check/`）の
  形をそのまま使えるが、今回は `verify.json` に文字列 1 行（`detail`）が載るだけ。

### 提案

- **P-G36-1**: `e2e-check.mjs` の画面一覧（`buildRoutes`）は `mobile-audit.mjs` の一覧を流用しているが、
  mobile 専用の見た目チェック（タップ領域・コントラスト等）とナビゲーション（e2e）で本来必要な画面が
  完全に一致するとは限らない。乖離が出てきたら `buildRoutes` に「audit 用」「e2e 用」のフラグを足すことも
  検討する。
- **P-G36-2**: U-G36-1 の実機確認が終わったら、`E2E_SKIP_BUILD`/`E2E_REQUIRE_STAGING` の名前や既定値を
  実際の運用に合わせて見直す余地があるかもしれない（現状は celeris 側 `verify.sh` の呼び方に合わせて
  決め打ちしたもの）。

## Phase G37 — スマホ UX ラウンド 10: skills / MCP 画面の磨きと残件（ADR-0055、celeris Phase 84。2026-09-21）

celeris 側は無変更（`crates/` 無変更。GUI だけの Phase）。`docs/adr/0055-mobile-ux.md`・Phase 80〜83・
`gui/docs/PROGRESS.md` Phase G30〜G36 の未解決事項から、skills / MCP 画面の磨きと 3 つの leftover を対応した。

### 1. skills 画面の磨き（`/knowledge/skills`・詳細・`/org` の mount 済み skills。受け入れ条件 1）

- **付属ファイルの入力（U-G35-2 の解消）**: `~/routes/knowledge.skills.tsx::SkillEditor` に行入力を追加
  （パス欄 + 中身のテキストエリアの組を「追加」/「削除」できる。API はもともと `SkillPutBody.files` を
  受け付けていたので `~/celeris/skills-admin.server.ts::readSkillPutBody`（`file_path`/`file_content` の
  並行配列を読む）は無変更）。`PUT /skills/{name}` は送った分のファイルだけ書き、触れなかった既存ファイル
  はそのまま残る仕様（`task_ops::knowledge::skills_put`）なので、既存の付属ファイル（`GET /skills/{name}`
  は名前だけ返す。中身は運ばない — ADR-0056 D3 P-79-a）は「参考情報として名前だけ表示、中身は読み込まない」
  にした（勝手に空文字で上書きする事故を避けるため）。付属ファイルのパスは `~/lib/skills.ts::
  skillFilePathProblem`（celeris の `safe_relative_path` と同じ規則。絶対パス・`\`・`..`・`SKILL.md`
  自身を拒否）で送信前に検査し、行ごとにインラインでエラーを出す。
- **frontmatter 雛形ボタン**: SKILL.md 欄の上に「雛形を使う」ボタン（`skillMarkdownTemplate` を呼んで
  本文を差し替える。既存の作成時の初期値と同じ雛形）。
- **検証メッセージのインライン化**: 従来 1 本にまとまっていた `skillMarkdownProblem` を
  `skillNameProblem`（名前欄だけ）/`skillBodyProblem`（SKILL.md 欄だけ）に分割し（`skillMarkdownProblem`
  はこの 2 つを順に見るだけの合成関数として残す。保存ボタンの活性判定に使用）、名前欄・SKILL.md 欄・
  付属ファイルの行それぞれの直下にそのフィールドの理由だけを出す（以前は 1 つのエラー文をフォーム全体の
  下にまとめて出していた）。
- **mounted-by チップの省略 + title（受け入れ条件「shortId 風の省略と title」）**: `~/routes/
  knowledge.skills.tsx` の一覧カードの `mounted_by` チップ、詳細画面の mount 先バッジのどちらも
  `~/lib/format.ts::shortId`（既存。id/sha 表示で使っている末尾省略関数）で表示を短くし、`title`（一覧側）
  または `title`+`aria-label`（詳細側。バッジは `<Link>` の中にあるのでアクセシブルな名前も全文にする）に
  全文を残した。組織ノードの id はケバブケースの短い文字列がほとんどで today の fixture では実際には
  省略が起きないものもあるが、`~/lib/format.ts` の既存の省略規約（ADR-0055 D2「id・sha・パスは末尾に省略、
  全文は title」）に揃えた。
- **SKILL.md の `#` 見出しが `<h1>` を 2 つ作らない件（Phase 82 の監査対応）**: `~/lib/skills.ts::
  skillMarkdownBody`（front matter を落とし見出しを 1 段落とす）は変更していない。今回の変更後も
  `pnpm mobile-audit` の `a11y-structure` が 0 件のままであることを確認した（下記「監査」）。
- **画面 1 つに主操作 1 つ**: 編集フォームの `variant="primary"` は「保存」の 1 つだけ（「雛形を使う」
  「付属ファイルを追加」「削除」は `ghost`/`secondary`/`danger`）。詳細画面は「直す」（`secondary`）と
  折りたたみの中の「削除する」（`danger`）だけで primary は無い。一覧画面も「新しい skill」は
  `secondary`。
- **監査対象に作成・編集フォームを追加**: これまで `gui/scripts/lib/celeris-fixture.mjs::buildRoutes` は
  `/knowledge/skills`（一覧）と `?name=` （詳細）だけを監査していて、`?create=1`/`?edit=1`（今回磨いた
  フォーム本体）が一度も `pnpm mobile-audit`/`pnpm e2e:*` を通っていなかった（「監査 0 件」を名乗りながら
  実際には新しい UI を検査していない、という事故になりかねない穴だった）。`knowledge-skill-create`
  （`/knowledge/skills?create=1`）・`knowledge-skill-edit`（`/knowledge/skills?name=<x>&edit=1`）を
  route 一覧に追加した（GET だけなので `pnpm e2e:staging` でも安全）。`mobile-audit.mjs`/`e2e-check.mjs`
  はこの一覧を共有しているので、両方に一度で効く。

### 2. MCP クライアント節の磨き（`/accounts`。受け入れ条件 2）

- **直近の呼び出しの遅延読み込みにスケルトン**: `~/routes/accounts.tsx::McpClientCallsDisclosure` は
  `<details>` を開いたときだけ `useFetcher().load()` する作り（Phase G34）のまま、読み込み中の表示を
  「読み込み中…」の 1 行から `McpCallListSkeleton`（`~/components/ui/skeleton.tsx::Skeleton` を使った
  行 2 本ぶんの骨組み、`aria-hidden`）に変えた。
- **エラーの種類を 1 語のバッジに**: `~/routes/accounts.tsx::McpCallRow` の `call.error_kind`
  （地の文の `<span className="text-danger">` だった）を `Badge tone="danger"`
  （`data-status-badge="mcp-call-error"`）に変更（ADR-0055 D1-3「状態バッジは 1 語」の規律をエラー種別にも
  揃えた。`pnpm mobile-audit` の `status-badge` ルールは空白・12 文字超を違反にするが、`error_kind` は
  `rate_limited` のような celeris 側の固定語彙なので該当しない）。
- **接続 URL のヒント + コピー**: `~/lib/mcp.ts::mcpConnectionUrlHint(client)`（新規、純粋関数）が
  `docs/mcp.md` §2 の既定値（`auth = "token"` は `http://127.0.0.1:18200/mcp`、`auth = "none"` は
  `http://127.0.0.1:18201/mcp`）を `mcpAuthKindWord` から選んで返す（**トークンの値は一切含まない** —
  `token_hash` はそもそも値そのものを持たない）。`McpClientCard` の `dl` にこのヒントと
  `~/components/ui/misc.tsx::CopyButton`（新規、`navigator.clipboard.writeText`。失敗（API 無し・拒否）
  は静かに諦める）を添えた行を追加した。実際の `[mcp] listen` はデプロイごとの設定で変わりうるので
  「よくある既定」のヒントである旨をコードコメントに明記した。

### 3. 残件 3 件（受け入れ条件 3）

**U-G32-2（`role="status"` を board/task の状態バッジに広げる）**: `~/lib/live-status.ts`（新規）に
`isLiveStatusScreen(screen)` という小さな純粋関数を切り出した。root の SSE（`~/hooks/
useCelerisStream.ts`）は「今開いている画面の loader」を再検証するだけで celeris は差分を送らないため、
「全バッジに付けると煩くなる」という Phase 76 の懸念への回答は「SSE の再検証で更新され続ける画面かどうか」
だけに絞ることにした。対象は `board`/`task-list`/`task-detail` の 3 画面（`~/routes/board.tsx` のボード
カード、`~/routes/tasks.tsx` の一覧行、`~/routes/tasks.$id.tsx` のヘッダ）。`task-new-candidates`
（`/tasks/new` の depends_on 候補。作るときに一度読むだけ）・`project-integrations`・`help-example`
（説明用の例）は対象外として関数の union 型に残し、対象外であることをテストで固定した（Console の task
ブロックは Phase 76 で個別対応済みなのでこの関数の対象に含めていない）。

**tool_use 要約の展開にキーボード操作（Phase 74）**: `~/components/ConsoleBlockItem.tsx::ReplyStepRow`
を確認したところ、`tool_use`/`tool_result` の展開トグルは Phase 74 の時点で既にネイティブな
`<button type="button" aria-expanded=...>` になっていた（コードコメントにも「キーボード（Enter/Space）
でも操作できるようにした」と明記済み）。ネイティブ `<button>` は Enter/Space を標準で処理するので追加の
`onKeyDown` は不要 — **コード変更は無し**。`gui/scripts/mobile-audit.mjs::checkFocusOrder`（Tab キーで
実際に押して `activeElement` が進むかを見る）が `home`/`org-node`（Console のモックデータに `tool_use`/
`tool_result` の展開可能な行を含む）で 0 件のままであることを今回のフルラウンドの監査で再確認し、回帰が
無いことを確かめた。

**U-G31-3（絶対日付フォールバックが UTC）**: `~/lib/reports.ts::absoluteDateLabel` を `getUTCFullYear`/
`getUTCMonth`/`getUTCDate` から `getFullYear`/`getMonth`/`getDate`（実行環境のローカルタイムゾーン）に
変更した。絶対時刻そのものは返さない規律（呼び出し側が `title` に生の ISO を残す）は変えていない。
**既知の限界**（コードコメントと本節「未解決事項」に明記）: SSR（GUI サーバーの Node プロセス）と
CSR（ブラウザ）は同じ `Date` のローカル getter を使うが、両者のタイムゾーンが同じ保証は無い
（ADR-0055 が想定するスマホからのリモートアクセスでは、GUI サーバーのホストとブラウザが別タイムゾーンに
ありうる）。ずれた場合、7 日を超えた絶対日付表示だけ SSR の文字列とハイドレーション後の文字列が食い違い、
React が 1 回だけ静かに client 側の値に差し替える（相対表示・7 日以内はどちらの環境でも同じ計算になる
ので影響しない）。今回はこのずれの検出・警告抑制までは実装していない（下記「未解決事項」）。

### テスト（新規・変更）

- `test/unit/skills.test.ts`: `skillNameProblem`/`skillBodyProblem`（分割後の単体、既存の
  `skillMarkdownProblem` の合成としての振る舞いも確認）・`skillFilePathProblem`（相対パスは通す、空文字は
  エラーにしない、絶対パス・`\`・`..`・`SKILL.md` 自身は拒否）を追加。
- `test/unit/mcp.test.ts`: `mcpConnectionUrlHint`（token/none の既定 URL、トークンの値そのものを含まない
  ことの確認）を追加。
- `test/unit/live-status.test.ts`（新規）: `isLiveStatusScreen` の全 6 パターンを table-driven で確認
  （board/task-list/task-detail が true、それ以外が false）。
- `test/unit/reports.test.ts`: `relativeTimeLabel` の絶対日付フォールバックが `process.env.TZ` に応じて
  変わることを 2 ケース（`UTC`・`Pacific/Kiritimati`）で確認（同じ ISO でもタイムゾーンで結果が変わる
  ことを直接示す。UTC のままなら両ケースが同じ値になってしまうので、これが違うこと自体がローカル化の
  証拠になる）。

### ゲート（証拠コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| lint | `pnpm lint`（`pnpm exec biome check --write .` で import 順・未使用 import を 1 回自動整形） | exit 0。`Checked 243 files in ...ms. No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0 |
| test | `pnpm test` | exit 0。**Test Files 65 passed (65) / Tests 982 passed (982)**（Phase G36 の 965 から +17: `skills.test.ts` 9、`mcp.test.ts` 3、`live-status.test.ts` 6、`reports.test.ts` 2 の内訳） |
| build | `pnpm build` | exit 0（client・server とも）。既存の `[INEFFECTIVE_DYNAMIC_IMPORT]` 警告 2 件は Phase G33 から変化なし |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（celeris の API 契約は変えていない） |
| mobile-audit | `MOBILE_AUDIT_SKIP_BUILD=1 pnpm mobile-audit` | **exit 0、violations 0 件**（`by_rule: {}`、`by_scheme: {"light": 0, "dark": 0}`）。**25 route**（`knowledge-skill-create`/`knowledge-skill-edit` を追加。Phase G36 の 23 から +2） × light/dark。新ルートの初回 JS は 416.9KB（予算 532KB 以内）、`accounts` は 433.7KB |
| e2e:mock | `E2E_SKIP_BUILD=1 pnpm e2e:mock` | **`{"ok": true, "mode": "mock", "failures": []}`、exit 0** |

### 変更したファイル

- `app/lib/skills.ts`（`skillNameProblem`/`skillBodyProblem`/`skillFilePathProblem` 新規、
  `skillMarkdownProblem` を合成関数に）
- `app/routes/knowledge.skills.tsx`（`SkillEditor` に付属ファイル行入力・雛形ボタン・フィールドごとの
  インラインエラー、`SkillsSidebar`/`SkillView` の mounted-by チップに `shortId`+`title`/`aria-label`）
- `app/lib/mcp.ts`（`mcpConnectionUrlHint` 新規）
- `app/components/ui/misc.tsx`（`CopyButton` 新規）・`app/components/ui/Icon.tsx`（`copy` アイコン新規）
- `app/routes/accounts.tsx`（`McpClientCard` に接続 URL ヒント行、`McpClientCallsDisclosure` に
  `McpCallListSkeleton`、`McpCallRow` の `error_kind` をバッジに）
- `app/lib/live-status.ts`（新規、`isLiveStatusScreen`）
- `app/routes/board.tsx`・`app/routes/tasks.tsx`・`app/routes/tasks.$id.tsx`（状態バッジに
  `role="status"` を条件付きで追加）
- `app/lib/reports.ts`（`absoluteDateLabel` を UTC からローカルタイムゾーンへ）
- `scripts/lib/celeris-fixture.mjs`（`knowledge-skill-create`/`knowledge-skill-edit` を監査対象に追加）
- `test/unit/skills.test.ts`・`test/unit/mcp.test.ts`・`test/unit/live-status.test.ts`（新規）・
  `test/unit/reports.test.ts`

### 未解決事項

- **U-G37-1（U-G31-3 の残り、実機未確認、ADR-0009 P-34 継続）**: 絶対日付フォールバックをローカル
  タイムゾーン化したことで、**SSR（GUI サーバー）とブラウザのタイムゾーンが異なる実配置では、7 日を
  超えた絶対日付表示だけ 1 回の hydration mismatch が起きる**（コードコメントに詳細を明記）。このサンドボックス
  はサーバー・ブラウザとも UTC のため `pnpm mobile-audit`/`pnpm e2e:mock` では再現しない。実機（スマホの
  ブラウザ、日本時間 UTC+9 を想定）で `/reports`・`/accounts` 等の 7 日超の日付表示を開き、ブラウザの
  開発者コンソールに hydration 関連の警告/エラーが出ないか、日付が正しく現地日付で見えるかを確認する
  必要がある。出るようであれば、次のラウンドで `suppressHydrationWarning` かクライアント限定コンポーネント
  への切り出しを検討する。
- **U-G37-2（実機未確認、ADR-0009 P-34 継続）**: skills 編集フォームの付属ファイル入力・MCP クライアント節の
  コピー機能・呼び出し履歴のスケルトンは、いずれも実際のブラウザでの操作感（`navigator.clipboard` の許可
  ダイアログの出方、`<details>` を開いた瞬間のスケルトン→実体の差し替わり方）を実機か `pnpm e2e`
  （Playwright の対話操作。今回のゲートには入っていない）で確認していない。
- **U-G37-3**: `mcpConnectionUrlHint` は `docs/mcp.md` §2 の**既定値**を返すだけで、実際にそのデプロイが
  使っている `[mcp]` の `listen` 設定は見ていない（GUI は celeris の設定ファイルを読まない）。設定を
  変えている環境では実際の URL と表示が食い違う。ヒントである旨は UI・コードコメントの両方に明記した。
- **U-G37-4**: `~/lib/live-status.ts::isLiveStatusScreen` の対象は今回の 3 画面（board/task-list/
  task-detail）に絞った。案件詳細（`ProjectIntegrations`）等、他の画面の状態バッジも理屈のうえでは
  SSE 経由で更新されうるが、今回はスコープを「board/task の状態バッジ」に限定する指示だったため広げて
  いない。次に広げるなら、案件・タスクの区別なく「開いたまま流れを追う画面」を一つずつ確認してから
  `LIVE_STATUS_SCREENS` に足すとよい。
- 本番反映は次節（コーディネータが `release.sh`/`verify.sh`/`promote.sh` を実行）。

### 提案

- **P-G37-1**: U-G37-1 が実機で問題になるようなら、`~/lib/reports.ts` の絶対日付表示だけをクライアント
  限定（`useEffect` で初回マウント後に local 値へ差し替える、または `suppressHydrationWarning`）にする
  小さな wrapper コンポーネントを作ると安全に倒せる。ただし今回はこの後方互換な広い書き換えを避け、
  「小さな純粋関数 + テスト」という指示の分量に収めた。
- **P-G37-2**: skills の付属ファイル入力は「今回のフォーム内だけの追加/上書き」で、KB 上の個別ファイル
  削除（サーバー側 API が個別ファイル削除を提供していない）はできない。運用でファイル削除の要望が出たら、
  celeris 側に `DELETE /skills/{name}/files/{path}` 相当を足すかどうかの検討が要る（このワークトリーク
  からは `crates/` に触れないため提案のみ）。

## Phase G38 — スマホ UX ラウンド 11: 運用画面（クラスタ・LLM source・リリース）を電話優先のステータスボードに（ADR-0055、celeris Phase 86。2026-09-21）

celeris 側は無変更（`crates/` 無変更。GUI だけの Phase）。`docs/adr/0055-mobile-ux.md` D3 のループどおり、Phase 65〜85 で
作った運用系 3 画面（`/clusters`・`/accounts` の LLM source 節・`/releases`）を、既存の API 契約（`docs/celeris-api-v1.md` /
`app/celeris/types.ts`）を変えずに、電話幅で最初に見る「ステータスボード」として磨いた。判断（何を選ぶか・何が正しいか）は
celeris の中にあるまま、GUI 側は表示の言い換えだけを追加している（`gui/CLAUDE.md`「判断ロジックの再実装」の禁止）。

### 1. `/clusters`（受け入れ条件 1）

- **クラスタ全体の 1 語バッジ**: `app/routes/clusters.tsx::clusterStatusWord`（新規、`ClusterView.connected`/
  `tunnel_login_needed` から `"connected"` / `"login-needed"` / `"down"` / `"unknown"` を返す純粋関数）。
  `tunnel_login_needed` を「down」より優先する（鍵認証も失敗して人の TOTP が要る状態を、ただの切断より先に伝える）。
  従来の `connectedLabel`（`"connected"`/`"disconnected"`/`"-"`）を置き換えた。**`"disconnected"` → `"down"` へのラベル
  変更**に伴い、`e2e/g7.spec.ts`（受け入れ条件 1・2・3・6、実 celeris に対する Playwright）の該当アサーションを
  `toHaveText("down")` に更新した（`pnpm e2e:mock`/`pnpm mobile-audit` の対象ではないが、実 celeris 向けの `pnpm e2e` を
  壊さないため。実行はしていない — このスコープでは実 celeris を起動しないため未確認）。
- **トンネル（forward）の表示**: Phase 85 で作った `listener`/`target_healthy`/`last_error` の 1 語バッジ＋理由の文
  （`TunnelForwardRow`）はロジックとしては無変更。表示のためのコンテナの `text-xs` を `text-sm` にした（後述の
  font-size 監査対応。中の `Mono`（host:port）は `font-mono` なので対象外のまま）。
- **「接続（TOTP）」の全幅主操作**: `item.tunnel_login_needed` が真のときだけ、既存の「接続」ボタン（`data-testid=
  "cluster-connect"`。フォーム・intent は無変更）のラベルを「接続（TOTP）」にし、`className="w-full sm:w-auto"`
  （`/releases` の upgrade ボタンと同じ、スマホだけ全幅にしてタブレット以上は auto に戻すパターン）を付けた。
  それ以外（初回接続・`tunnel_login_needed` を伴わない再接続）は従来どおり小さいボタン・「接続」/「接続し直す」の
  ラベルのまま（ボタン自体・`data-testid`・action の intent は 1 つのまま増やしていない — 2 つの見た目違いの
  ボタンを重ねて出す設計は避けた）。
- **「これは何を意味しますか？」の disclosure**: トンネル一覧の下に `<details>` を足し、「このトンネルは Qwen への
  接続で、target が応答しない間 `celeris/<tier>` は自動で Claude/GPT に倒れる（タスクは止まらない）」ことを説明する
  1 段落。`/accounts` の LLM source 節への導線も添えた。

### 2. `/accounts` の LLM source 節（受け入れ条件 2）

- **`tierResolutionReason`（`app/lib/llm-sources.ts`、新規）**: `celeris/<tier>` が今の供給元に解決した理由を
  `"free-first"` / `"cooldown"` / `"unreachable"` / `"no-source"` / `"unknown"` の 1 語で返す純粋関数。**celeris の
  選択アルゴリズム（ADR-0053 D1 の「free 優先 → 残量スコア」）を GUI で再現・再計算するのではなく**、`GET /llm/sources`
  が既に返している値（解決先の `kind`・無料中継の `enabled`/`reachable`・oauth プールの `accounts[].cooldown_until`）
  だけを読んで、ADR が文書化した規則の一言説明を組み立てる:
  - 解決先の `kind === "openai-compatible"` → `"free-first"`（D1(a) どおり最優先で選ばれた）。
  - 解決先が oauth プールで、無料中継が `enabled && reachable === false` → `"unreachable"`（無料源が落ちているので
    アカウントに倒れた）。
  - 解決先が oauth プールで、そのプールに cooldown 中のアカウントが 1 件でもあれば → `"cooldown"`（**どのアカウントが
    実際に選ばれたかは celeris だけが知っている**。「このプールに cooldown 中のアカウントがいる」という既存の事実を
    示すだけで、celeris の内部の順位付けを再現してはいない — この不正確さは未解決事項に記載）。
  - それ以外 → `"unknown"`（無料源が無い・cooldown も無い、普通のプール選択）。
  `tierResolutionReasonLabel` が 1 行の日本語文にする。`LlmSourcesSection` の `celeris_tiers` の `dl` に、既存の
  解決先ラベルの下へ `data-testid="llm-tier-reason-<tier>"` として追加した。
- **cooldown の絶対時刻**: `cooldownUntilTitle`（新規、Unix 秒 → RFC 3339）を `LlmAccountRow` の cooldown バッジの
  `title` に付けた（ADR-0055 D2「相対時刻＋`title` に絶対時刻」の規律を Unix 秒の `cooldown_until` にも適用）。
- **全部 cooldown の警告カード**: `allAccountsCoolingDown`（新規、汎用の純粋関数）を `claude-oauth` の供給元に適用し、
  1 件以上のアカウントがあって**全員**が cooldown 中のときだけ `Alert tone="warning"`
  （`data-testid="llm-claude-all-cooldown"`）を出す。0 件（アカウントが無い）は「全員 cooldown」とは言えないので
  出さない。

### 3. `/releases`（受け入れ条件 3）

- **`ReleaseVerify` は集計値しか運ばない**ことを確認した（`docs/celeris-api-v1.md` §3.67 相当、`crates/task-api/
  src/types.rs::ReleaseVerify` は `ok`/`live_ok`/`at` の 3 フィールドのみ。`scripts/selfdeploy/verify.sh` の doc
  コメントで「`ok` は検査 1〜4・4b・6 が全部真、`live_ok` は検査 5（N-1 互換）」と定義されている）。celeris が
  個別の検査結果を返していない以上、GUI 側で 6 個の偽の内訳を作ることは `gui/CLAUDE.md`「派生値の再計算・判断
  ロジックの再実装」の禁止に触れる。そこで:
  - **`releaseVerifyCheckGroups`（`app/lib/releases.ts`、新規）**: 「検査 1〜4・4b・6」（`ok` を反映）と「検査 5
    （N-1 互換）」（`live_ok` を反映）の**2 グループ**を返す。各グループの語は `"通過"`/`"失敗"`/`"未実施"`
    （`verify` が無ければ両方 `"未実施"`）。`ReleaseCard` の CardBody に `<ul data-testid="release-verify-checks">`
    として追加し、各語を `data-status-badge="release-check"` のバッジにした（受け入れ条件の「検査 1〜6・4b の
    コンパクトな一覧・1 語の結果」を、実在する 2 つの集計値の範囲で満たした。個別の 6 検査の合否は celeris の外に
    出ていないため表示できない — 未解決事項に記載）。
  - **`releaseModeWord`（新規）**: 切替方法を `"live"`/`"stop-start"`/`"unknown"` の英語 1 語で返す（
    `releaseVerifyState` の `ok_live`/`ok_stop_start` をそのまま英語にしただけ）。既存の `releaseVerifyBadgeLabel`
    は U13（Phase 73）の判断どおり両方を「検証済み」の同じ日本語 1 語にまとめているが、今回追加したバッジは
    逆に切替方法そのものを見せる（アイコン・色は既存の `releaseVerifyIcon`/`releaseVerifyTone`（Phase 73 割り当て）
    をそのまま流用し、二重に定義していない）。未検証・NG のときは切替方法が定まらないので出さない。
- **現行リリースのカードをスマホで先頭に**: `ReleaseCard` のルート `Card` に
  `className={... item.is_current ? "order-first xl:order-none" : ""}` を付けた。`xl:grid-cols-2` になる前
  （スマホ・タブレット幅）は現行が先頭に来て、デスクトップ（`xl:` の 2 列グリッド）は `built_at` 降順の既存の並びの
  まま（`xl:order-none` で打ち消す）。
- **upgrade ボタンの可否は無変更**: `promoteAvailability`/`canPromote` のロジックには触れていない。

### 4. 通知/受信箱（受け入れ条件 4）

`/inbox` というルートは存在するが、`docs/adr/0055-mobile-ux.md` D1 が挙げる機械検査対象の画面一覧
（`/`、`/org`、`/projects`、`/tasks/<id>`、`/board`、`/approvals`、`/reports`、`/releases`、`/knowledge`、
`/knowledge/inbox`、`/clusters`、`/accounts`、`/help`）には**含まれていない**（`/knowledge/inbox` は別ルートで、
こちらは対象。裏方の `/inbox` は SPEC §4 の 6 画面にも入っていない、Phase G13f-1 の設計どおり）。
`gui/scripts/lib/celeris-fixture.mjs::buildRoutes` にも `/inbox` は無く、`pnpm mobile-audit`/`pnpm e2e:mock` の
対象外のまま（今回もこの一覧を広げていない — CLAUDE.md「今回のフェーズだけをやる」）。

とはいえ受け入れ条件が「存在するなら」満たすことを求めていたので、軽い改善は加えた:
- **相対時刻**: `app/routes/inbox.tsx` の `loader` を `{ inbox, fetchedAt }` を返す形にし（`loadInbox` 自体・
  `test/unit/inbox.loader.test.ts` は無変更。`fetchedAt` は表示専用の基準時刻）、承認（`requested_at`）・質問
  （`asked_at`、無ければ非表示）の行に `~/lib/reports.ts::relativeTimeLabel` の「n 前」を `<time dateTime title>`
  で追加した（`title` に生の ISO を残す既存の規律のまま）。
- **カード・主操作 1 つ**: 既存の `<li>` は角丸・枠線・背景色で既に「カード」の見た目になっていた（`Card` コンポーネント
  そのものは使っていない）。承認の「承認」/「却下」は二者択一の決定であり主操作 1 つに絞れない性質のものなので、
  そのまま残した（`variant="primary"` は使っておらず success/danger で並列、他画面の「主操作 1 つ」規律とは別枠と
  判断）。この構造自体は今回のスコープではないので変更していない。

### 5. fixture（受け入れ条件 6「新しい状態を描画させる」）

`gui/scripts/lib/celeris-fixture.mjs`（`pnpm mobile-audit`/`pnpm e2e:mock` が共有）と
`gui/test/mock-celeris/fixtures.ts`（`pnpm test` の `defaultReleases`）を拡張した:

- **クラスタ**: `pegasus` のトンネルを Phase 85 の実機と同じ形（`listener: true`・`target_healthy: false`・
  `last_error`）にして「unreachable」バッジと理由の文を監査対象にし、`gpu2`（`auth: "manual"`、`connected: false`、
  `tunnel_login_needed: false`）を足して「down」バッジを監査対象にした。**`gpu2` を `auth: "publickey"` にすると
  `focus-order` の機械検査が誤検知した**（下記「見つけて直したバグ」参照）ので `"manual"` にした。
- **LLM source**: `claude-oauth` の 2 アカウントを両方 cooldown にして「Claude のアカウントが全て cooldown 中です」
  警告カードと絶対 `title` 付きのバッジを監査対象にし、`openai-compatible:qwen` を `reachable: true` にして
  `cheap` tier の解決先にした（`"free-first"` の理由を監査対象にする）。`frontier`/`standard` は引き続き
  `claude-oauth` に解決したままなので、`"cooldown"`（無料源は reachable だが frontier/standard は別 tier として
  claude-oauth に倒れており、そのプールが cooldown 中）の理由も同時に監査対象になる。
- **リリース**: `defaultReleases.items` に `ok_stop_start`（`cccccccccccc`）と `ng`（`dddddddddddd`）の 2 件を足し、
  既存の `unverified`/`ok_live` と合わせて `releaseVerifyState` の 4 状態すべてを監査対象にした
  （`test/unit/releases.test.ts::loadReleases` の並び順アサーションを 4 件に更新）。

### 見つけて直したバグ（`focus-order` の誤検知、実装ロジックのバグではない）

`gpu2` を最初 `auth: "publickey"` にしたところ、`pnpm mobile-audit` の `focus-order` 検査が `/clusters` で
「Tab did not move focus away from this element after step 6 (trap)」を報告した。調べたところ実際のキーボード
トラップではなく、監査スクリプト（`gui/scripts/mobile-audit.mjs::cssPathRef`）が要素の識別に `element.id` を使って
おり、`<input type="hidden" name="id" value={item.id} />` を含む `<form>` では HTML の「named form control」機構に
より `form.id` が文字列ではなくその `<input>` 要素自身を返す（ブラウザの仕様上の挙動）。これが `"[object
HTMLInputElement]"` という壊れた文字列に化け、**構造が同じ 2 つの接続フォーム**（`pegasus` と `gpu2`、どちらも
`auth !== "manual"` で同じ形の「接続」ボタンを持つ）の CSS パス署名が深さ 6 で衝突し、「別の要素にフォーカスが
移った」ことを「同じ要素のまま」と誤検知した。`gpu2` を `auth: "manual"`（フォームを持たない）にして衝突を避けた。
**GUI の実装（フォーム・フォーカス移動そのもの）にバグは無い**ことをコードを読んで確認済み。監査スクリプト自体
（`cssPathRef`）の修正はこの Phase のスコープ外（GUI 表示のみのスコープであり、`scripts/mobile-audit.mjs` の
識別ロジックの改善は次のラウンドの課題として未解決事項に記載）。

### テスト（新規・変更）

- `test/unit/clusters.test.ts`: `clusterStatusWord` の全パターン（`tunnel_login_needed` 優先、`connected` の 3 値、
  1 語バッジの検証）。
- `test/unit/llm-sources.test.ts`: `tierResolutionReason`（no-source / free-first / unreachable / cooldown /
  unknown の 5 パターン）・`tierResolutionReasonLabel`（全語で非空）・`cooldownUntilTitle`（Unix 秒 → RFC 3339）・
  `allAccountsCoolingDown`（全員 cooldown / 一部だけ / 0 件）。
- `test/unit/releases.test.ts`: `releaseModeWord`（live/stop-start/unknown、1 語バッジの検証）・
  `releaseVerifyCheckGroups`（未検証は両方未実施、ok/live_ok の反映、1 語バッジの検証）。`loadReleases` の並び順
  アサーションをフィクスチャの 4 件に更新。

### ゲート（証拠コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（celeris の API 契約は変えていない） |
| lint | `pnpm lint`（`pnpm exec biome check --write .` で自動整形、`--unsafe` で 1 件の optional chain 提案を適用） | exit 0。`Checked 243 files … No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0（出力なし） |
| test | `pnpm test` | exit 0。**Test Files 65 passed (65) / Tests 1004 passed (1004)**（Phase G37 の 982 から +22: `clusters.test.ts` +4、`llm-sources.test.ts` +9、`releases.test.ts` +9 の内訳） |
| build | `pnpm build` | exit 0（client・server とも）。既存の `[INEFFECTIVE_DYNAMIC_IMPORT]` 警告 2 件は Phase G33 から変化なし |
| mobile-audit | `pnpm mobile-audit` | **exit 0、`{"ok": true, "total": 0, "by_rule": {}, "by_scheme": {"light": 0, "dark": 0}}`**（25 route × light/dark）。1 回目は font-size 26 件・focus-order 2 件で落ち、`text-xs` → `text-sm`（4 箇所）と `gpu2` の `auth` 変更で解消したことを確認済み |
| e2e:mock | `pnpm e2e:mock` | **`{"ok": true, "mode": "mock", "failures": []}`** |

`crates/` を一切変更していないため `cargo test --workspace`/`cargo clippy --workspace -- -D warnings` はこの Phase の
スコープ外（実行していない。Phase 80/82/83/84 と同じ扱い）。

### 変更したファイル

- `app/routes/clusters.tsx`（`clusterStatusWord` 新規、`ClusterCard` の状態バッジ・接続ボタン・disclosure）
- `app/routes/accounts.tsx`（`LlmSourcesSection` の tier 理由・全員 cooldown 警告、`LlmAccountRow` の cooldown title）
- `app/lib/llm-sources.ts`（`tierResolutionReason`/`tierResolutionReasonLabel`/`cooldownUntilTitle`/
  `allAccountsCoolingDown` 新規）
- `app/routes/releases.tsx`（`ReleaseCard` の mode バッジ・検査一覧・`order-first xl:order-none`）
- `app/lib/releases.ts`（`releaseModeWord`/`releaseVerifyCheckGroups` 新規）
- `app/routes/inbox.tsx`（`loader` の戻り値に `fetchedAt`、承認・質問行に相対時刻）
- `scripts/lib/celeris-fixture.mjs`（クラスタ・LLM source の fixture 拡張）
- `test/mock-celeris/fixtures.ts`（`defaultReleases` に `ok_stop_start`/`ng` の 2 件）
- `e2e/g7.spec.ts`（`cluster-connected` の期待値を `"disconnected"` → `"down"` に更新）
- `test/unit/clusters.test.ts`・`test/unit/llm-sources.test.ts`・`test/unit/releases.test.ts`（新規テスト、
  `loadReleases` の並び順アサーション更新）

### 未解決事項

- **実機未確認**（ADR-0009 P-34。このサンドボックスに本物の celeris・本物のブラウザ・外向きネットワークが無い）。
  特にスマホ実機での「接続（TOTP）」全幅ボタン・disclosure の開閉・現行リリースの並び替えは確認できていない。
- **`releaseVerifyCheckGroups` は 2 グループの粒度まで**: `GET /releases` の `ReleaseVerify` が `ok`/`live_ok` の
  集計値しか運ばないため、「検査 1〜6・4b の一覧」を個別の 8 項目には分解できていない。個別の合否が欲しいなら
  celeris 側で `verify.json` に検査ごとの結果を残し、`ReleaseVerify` に運ぶ API 拡張が要る（このワークトリークは
  `crates/` に触れないため提案のみ。下記「提案」参照）。
- **`tierResolutionReason` の「cooldown」は近似**: プール内に 1 件でも cooldown 中のアカウントがいることを示す
  だけで、「実際に選ばれたアカウントが cooldown 中かどうか」までは分からない（celeris は `resolves_to` に
  供給元 id しか返さず、選ばれたアカウント id までは返さないため）。
- **`/inbox` は ADR-0055 D1 の監査対象外のまま**: 今回の相対時刻の追加は機械検査（`pnpm mobile-audit`）を通っていない
  （そもそも対象ルート一覧に無い）。次にこの画面を本格的に磨くなら、まず D1 の一覧に `/inbox` を足すかどうかを
  ADR レベルで決める必要がある（このワークトリークは ADR を書き換えられないため、決定は次の Phase 起動時に）。
- `e2e/g7.spec.ts` の更新（`"down"`）は実 celeris に対する `pnpm e2e` を実行して確認していない（`pnpm e2e:mock` の
  スコープ外。celeris を実際に起こす環境で確認すること）。
- **監査スクリプトの `cssPathRef` の脆さ**: `element.id` を素朴に読むと named-form-control の shadowing で壊れた
  文字列になり、構造が同じ複数要素があると focus-order を誤検知しうる（今回は fixture 側の回避で凌いだ）。
  `scripts/mobile-audit.mjs::cssPathRef` を `element.getAttribute("id")` に変える、または `id` 属性そのものを
  持つ要素だけ id を使うようにすると根本的に直る（この Phase はスコープ外。次ラウンドの候補）。

### 提案

- **P-G38-1**: celeris 側に `ReleaseVerify.checks: { name: string; ok: bool }[]` 相当の内訳を足せば、
  `releaseVerifyCheckGroups` を本当の 1〜6・4b の個別結果に置き換えられる（`docs/celeris-api-v1.md` の拡張が要る。
  GUI 側の変更は表示の分解だけで済む）。
- **P-G38-2**: `scripts/mobile-audit.mjs::cssPathRef` の `node.id` を `node.getAttribute("id")` に変える（安全側）。
  named-form-control の shadowing はどのフォームでも起こりうる（`name="id"` の hidden input はこのリポジトリの
  複数の画面パターン）ので、他の画面でも同じ誤検知が将来起きうる。
- **P-G38-3**: `/inbox` を ADR-0055 D1 の監査対象に含めるかどうかを ADR レベルで判断してほしい（含めるなら、
  カードの `Card` コンポーネント化・承認/却下の主操作の扱いも次のラウンドで検討する）。

## Phase G39 — スマホ UX ラウンド 12: 監査スクリプトの堅牢化と /inbox の追加（ADR-0055、celeris Phase 87。2026-09-21）

celeris 側は無変更（`crates/` 無変更。GUI だけの Phase）。Phase G38 の提案 P-G38-2・P-G38-3 を人が採用したのを受けて、
`gui/scripts/mobile-audit.mjs` 自身の堅牢化と、監査対象からこぼれていた `/inbox` の追加をやった。

### 1. P-G38-2: `cssPathRef` の `node.id` を `node.getAttribute("id")` に

`gui/scripts/mobile-audit.mjs::cssPathRef`（要素の識別に使う CSS パス風の署名を組む関数）が、祖先を辿る途中で
`node.id`（`Element.id` という **IDL 属性**）を読んでいた。`<form>` が `<input type="hidden" name="id"
value={...} />` のような「name/id が `"id"` の named form control」を持つと、HTML の named-property 機構
（named getter）により `form.id` は文字列ではなく**その control 要素自身**を返す（ブラウザの仕様上の挙動。
content 属性の `id` が付いていなくても起きる）。`part += "#" + node.id` はこれを暗黙の文字列変換で
`"[object HTMLInputElement]"` に化けさせ、**構造が同じ複数のフォーム**（例: `/clusters` の複数の接続フォーム）
では同じ壊れた文字列に collapse する。その結果、本来ここで打ち切らずに祖先を辿り続けていれば区別できたはずの
違い（親要素内での位置など）を握りつぶして 2 つの要素の署名が衝突し、`focus-order` 検査が「同じ要素から
フォーカスが動いていない（罠）」と誤検知していた（Phase G38 で実際に踏み、`gpu2` フィクスチャを
`auth: "manual"`（フォームを持たない形）にして衝突を避けていた）。

`node.getAttribute("id")` は content 属性を直接読むだけで named-property の影響を受けないので、これに変えて
修正した（`cssPathRef` 自身のコメントに Phase 87 の説明を追記）。**回帰検査**として、`scripts/lib/celeris-
fixture.mjs` の `gpu2` フィクスチャを Phase G38 の回避前（`auth: "publickey"`）に戻し、`pegasus`（`auth: "totp"`）
と構造が同じ「接続」フォームを再び 2 つ並べた。修正後の `pnpm mobile-audit` が `focus-order` の誤検知を含め
0 件で通ることを実行して確認した（ユニットテストではなく、実際の Playwright 監査そのものを回帰検査にした。
`mobile-audit.mjs` は 1 つのファイルの中で Node 側と `page.evaluate` へ `toString()` で送るブラウザ側の 2 つの
実行環境を混ぜており（`tsconfig.node.json` が型検査からも除外している。ADR-0055 コメント参照）、`cssPathRef` を
素の Node の単体テストとして `import` すると、この行为が呼ぶ `window`/`document`/`Element` に依存しない形へ
書き直すか jsdom 相当を新規依存として足す必要があり、CLAUDE.md の「新しい依存を足さない」制約と衝突するため
見送った。代わりに次の 2 点で「単に import しただけでは重い処理が走らない」堅牢化はした:
- `cssPathRef` を `export` した（将来ユニットテストが要るときに `toString()` 経由の再構築なしで直接呼べる。
  `Function.prototype.toString()` は `export` キーワードを含まないので、`page.evaluate` への注入は無変更）。
- ファイル末尾を `node scripts/mobile-audit.mjs` として直接実行されたときだけ `main()`（実 Chromium 起動・
  偽の celeris・`pnpm build` を伴う）を呼ぶ module-execution guard に変え、`OUT_DIR`（既存スクリーンショットの
  削除）の初期化もトップレベルから `main()` の中に移した。これにより、この先 `cssPathRef` などの純関数を
  vitest から `import` しても、それだけで重い監査本体が走ってしまう事故は起きない。

### 2. P-G38-3: `/inbox` を機械検査対象に

- **route 一覧**: `scripts/lib/celeris-fixture.mjs::buildRoutes`（`mobile-audit.mjs`/`e2e-check.mjs` が共有）に
  `{ route: "inbox", path: "/inbox" }` を足した（人の指示どおり、ADR は書き換えていない）。監査対象が
  25 → **26 route**（× light/dark）。
- **fixture のバグ修正**: `GET /inbox` の fixture が `Inbox` 型（`docs/celeris-api-v1.md` §3.2、
  `app/celeris/types.ts::Inbox`）と違う形（`{ approvals: 0, attention: 0, by_status: {}, drafts: 0,
  questions: 0 }`。`counts` が無く、配列であるべきフィールドが数だった）を返していた。`/inbox` が監査対象に
  無かったため気づかれずに残っていた潜在バグで、実際にこの画面を開くと `app/routes/inbox.tsx` の
  `isEmpty`（`inbox.counts.approvals` を読む）が `inbox.counts is undefined` で例外落ちしていたはず。承認待ち
  1 件・質問 1 件を持つ、型どおりの `Inbox` に直した（`counts: { approvals: 1, questions: 1, drafts: 0,
  attention: 0, by_status: {} }`）。これで `/inbox` の承認・質問カードが実際に描画されるようになった
  （受け入れ条件「fixture に承認 1 件・質問 1 件を作ってカードを描画させる」）。
- **`e2e-check.mjs`**: `/inbox` の構造チェックを足した（`accounts`/`knowledge-skills` と同じパターン）。
  `approvals-section`/`questions-section` の存在は mock・staging 共通で見る。`approval-item`/`question-item`
  が 1 件以上あることは mock モードだけで見る（staging はスナップショット次第で 0 件のこともあるため）。
- **見つけて直した違反（1 回目の `pnpm mobile-audit` で 16 件）**:
  - `tap-target` ×4（light/dark 各 2）: `approval-title`/`question-title` の `<Link>` がテキストだけの `<a>` で
    高さ 20px しかなく 44×44 未満だった。`app/routes/board.tsx` のカード見出しリンクと同じ
    `flex min-h-11 items-center` を付けて自分の箱を広げた（`app/routes/inbox.tsx` の `ApprovalRow`/
    `QuestionRow`）。
  - `font-size` ×12（light/dark 各 6）: `~/components/ui/misc.tsx::StatCard` のラベル・hint と、
    `approval-requested-at`/`question-asked-at` の `<time>` がどちらも `text-xs`（12px）固定だった。
    `~/components/ui/badge.tsx::Badge` と同じ「モバイルは `text-sm`、デスクトップは `lg:text-xs`」の
    パターンに直した（`StatCard` は `/daemon` でも使われているので副次的にそちらの潜在バグも直った）。
  - 修正後は `pnpm mobile-audit` が 26 route × light/dark で **0 件**。

### 3. 監査レポートの証跡強化（受け入れ条件 3）

- JSON レポート（`test/mobile-audit/report.json`）に `git_sha`（`git rev-parse --short=12 HEAD`。取れなければ
  `"unknown"`）と、`routes[]` の各エントリに `duration_ms`（そのルート・scheme のページ訪問に掛かった時間）を
  足した。
- 標準エラー出力の末尾に 1 行の要約を追加: `routes=<N> schemes=2 violations=<N> perf_worst=<route> <kb>KB`
  （`perf_worst` は light scheme の js+css 転送量が最大のルート）。既存の `by_rule`/`by_scheme` の JSON 出力・
  `formatPerfTable` は変えていない。
- `pnpm mobile-audit` の終了コードの意味（違反 0 件なら 0、それ以外は 1）・`MOBILE_AUDIT_SKIP_BUILD` 等の
  既存の環境変数は無変更。

### テスト（新規・変更）

新しいユニットテストは足していない（上記「1.」の理由により、`cssPathRef` は実際の `pnpm mobile-audit` の
実行そのものを回帰検査にした）。`app/routes/inbox.tsx`/`~/components/ui/misc.tsx` の変更は見た目（クラス名）
だけで、`test/unit/inbox.loader.test.ts`/`test/unit/inbox.action.test.ts` が検査するデータの形・action の
挙動には触れていないため、既存テストは無変更のまま通る。

### ゲート（証拠コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ（celeris の API 契約は変えていない） |
| lint | `pnpm lint`（`biome check .`） | exit 0。`Checked 243 files … No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0（出力なし） |
| test | `pnpm test` | exit 0。**Test Files 65 passed (65) / Tests 1004 passed (1004)**（Phase G38 と同数。新規テストは追加していない） |
| build | `pnpm build` | exit 0（client・server とも）。既存の `[INEFFECTIVE_DYNAMIC_IMPORT]` 警告 2 件は変化なし |
| mobile-audit（1 回目、`/inbox` 追加直後） | `pnpm mobile-audit` | exit 1。**16 件**（`tap-target` 4、`font-size` 12）、全て `/inbox` |
| mobile-audit（修正後） | `pnpm mobile-audit` | **exit 0**。`{"ok": true, "total": 0, "by_rule": {}, "by_scheme": {"light": 0, "dark": 0}, "git_sha": "aac4cbcdbe8b"}`。末尾の要約: `routes=26 schemes=2 violations=0 perf_worst=task-overview 544.8KB`（26 route × light/dark。`focus-order` の誤検知（`gpu2`/`pegasus` の衝突）が再発していないことも確認済み） |
| e2e:mock | `pnpm e2e:mock` | **`{"ok": true, "mode": "mock", "failures": []}`**（`inbox` の `approvals-section`/`questions-section`/`approval-item`/`question-item` の検査を含む） |

`crates/` を一切変更していないため `cargo test --workspace`/`cargo clippy --workspace -- -D warnings` はこの Phase の
スコープ外（実行していない。Phase 80/82/83/84/86 と同じ扱い）。

### 変更したファイル

- `scripts/mobile-audit.mjs`（`cssPathRef` の `getAttribute("id")` 化・`export`、`git_sha`/`duration_ms`/
  1 行要約、module-execution guard、`OUT_DIR` 初期化を `main()` の中に移動）
- `scripts/lib/celeris-fixture.mjs`（`buildRoutes` に `/inbox` を追加、`GET /inbox` fixture を型どおりの
  `Inbox` に、`gpu2` を `auth: "publickey"` に復元）
- `scripts/e2e-check.mjs`（`/inbox` の構造チェックを追加）
- `app/routes/inbox.tsx`（`approval-title`/`question-title` の `<Link>` に `flex min-h-11 items-center`、
  対応する `<time>` を `text-sm ... lg:text-xs` に）
- `app/components/ui/misc.tsx`（`StatCard` のラベル・hint を `text-sm ... lg:text-xs` に）

### 未解決事項

- **実機未確認**（ADR-0009 P-34。このサンドボックスに本物の celeris・本物のブラウザ・外向きネットワークが無い）。
- **`/inbox` の他の題材が未監査のまま**: `draft-group`（受け入れ待ちの draft）・`attention-item`（注意）・
  `approval-parent-title`（承認の親タスク）・`question-approval-link`（`approval_id` がある質問）は今回の
  fixture では 0 件/非表示のままで、機械検査を実際には通っていない。いずれも `approval-title`/`question-title`
  と同じ「テキストだけの `<a className="hover:underline">`」構造なので、同じ 44×44 未満のタップ領域不足を
  将来 fixture を拡張したときに踏む可能性が高い（次ラウンドの候補。今回は「今回の Phase だけをやる」
  （CLAUDE.md）に従い、実際に監査が検出した違反だけを直した）。
- **`cssPathRef` はユニットテストしていない**: 上記「1.」のとおり、jsdom 相当の新規依存を足さずに素の Node で
  DOM 依存の純関数をテストする土台が無いため、`pnpm mobile-audit` の実行そのものを回帰検査にした。`export`
  と module-execution guard は追加したので、将来 jsdom 相当が使える状況になれば直接テストできる。
- Phase G38 の未解決事項（`releaseVerifyCheckGroups` の粒度、`tierResolutionReason` の「cooldown」判定の近似、
  `e2e/g7.spec.ts` の `"down"` 更新が実 celeris で未確認など）は変化なし。

### 提案

- **P-G39-1**: `/inbox` の `draft-group`/`attention-item`/`approval-parent-title`/`question-approval-link` を
  fixture に足して監査対象にし、上記の未解決事項にある潜在的なタップ領域不足を先回りして直す（次ラウンド）。
- **P-G39-2**: `cssPathRef`（および `isNotVisible` 等の DOM 依存の純関数群）を単体テストしたくなったら、
  jsdom 相当の軽量 DOM 実装を devDependency に足すかどうかを判断する（`package.json` の版固定・
  「新しい依存を足すときは理由を書く」の運用に従う）。

## Phase G40 — スマホ UX ラウンド 13: focus-order のフレーク解消と /inbox の残り（ADR-0055、celeris Phase 88。2026-09-21）

celeris 側は無変更（`crates/` 無変更。GUI だけの Phase）。Phase G39 が本番反映されたあとに 1 回だけ観測された
`focus-order` のフレーク（`home` light、「composer に 32 Tab で届かない」）の原因を突き止めて直し、Phase G39 の
提案 P-G39-1（`/inbox` の残りの題材を fixture に足す）を実装した。

### 1. `focus-order` のフレークの原因調査と修正

**調査**: `docs/PROGRESS.md` Phase 87 の本番反映の記録によると、merge 後の 1 回目の `pnpm mobile-audit` で
`focus-order` が 1 件（`home`、light、「composer (console-text) not reached … within 32 Tab presses
(22 focusable elements on page)」）出て、再実行では 0 件だった。まず `checkFocusOrder`（`scripts/mobile-audit.mjs`）
自体の Tab 予算は既に `focusableCount + 10`（最低 20）と動的だったので（「固定 32」ではなく、たまたま
22 + 10 = 32 だっただけ）、予算不足ではなく**タイミング依存の何か**だと当たりを付けた。

- まず「ハイドレーション中・クライアント限定描画の途中に歩き始めると、予算計算時と実際に歩く時点で
  focusable 要素数がずれるのでは」という仮説で、`document.fonts.ready` と DOM ノード総数の安定待ち
  （`waitForPageIdle`。最大 5 秒、100ms 間隔で 3 回連続同じ件数になったら抜ける）を追加した。加えて、
  要素の同一性判定を `cssPathRef` の再構築だけに頼らず、歩き始めに全 focusable 要素へ割り振る一意な連番
  （`data-mobile-audit-focus-id`）で行うようにし（skip link・固定タブバー・disclosure トグルのように
  フォーカス時の状態で class 構成が変わりうる要素を踏んでも誤判定しないため）、予算内に届かなかったときに
  実際に辿った経路（selector の列）を違反 `detail` に残す診断強化もした。この段階で `pnpm mobile-audit` を
  再実行したところ、`/inbox` の fixture 拡張（後述の 2.）が晒した別の違反（`project-detail` の
  `WorkTreeGraph`＝react-flow の overflow・タップ領域）が新たに出た。これは `waitForPageIdle` を `runChecks`
  （D1-1〜D1-6 等の全画面共通の検査）の前にも入れてしまい、dagre レイアウトが `useEffect` で非同期に確定する
  のを待ってしまったことが原因だと分かったので、**`waitForPageIdle` は `checkFocusOrder` の中だけに限定**し、
  `runChecks` 側は従来どおり `load` 直後の DOM をそのまま見る作りに戻した（このスコープ外の潜在バグを新たに
  検出・破壊しないため。CLAUDE.md「今回の Phase だけをやる」）。
- この時点で `pnpm mobile-audit` を再実行すると、`home` light で `focus-order` が 1 件（今回追加した診断強化
  のおかげで、実際に辿った経路が violation の `detail` に残った）。経路を見ると、Console の「育つ返事」
  ブロック（`console-block-reply`、`console-reply-step-toggle` を 2 つ）まで進んだあと、次の Tab で
  `document.activeElement` が `<body>` になっていた（＝ DOM 順としては直後にあるはずの `composer`
  （`console-text`）に届く前に、フォーカスが失われていた）。**単体の切り分けスクリプト**（`checkFocusOrder`
  と同じ手順を celeris fixture・Playwright で直接 15 回連続実行し、`document.activeElement` の遷移を都度
  記録するもの。CPU スロットリングは掛けない）を書いて実行したところ、15/15 回とも `composer` に正しく届き
  （実際の歩数は 16〜17 手で、`focusableCount` が数えた 22 とは食い違っていた＝この数え方自体は祖先の
  `display:none` を見ないので過大に数える既知の粗さがあるが、予算が十分あるので実害は無い）、フレークは
  一切再現しなかった。単体スクリプトと `mobile-audit.mjs` フルランの違いを洗い出したところ、**`perf`
  （Phase 77）が light scheme に掛ける CPU x4 スロットリング（`Emulation.setCPUThrottlingRate`）が、
  `checkFocusOrder` の実行中もそのまま有効なまま**という違いに行き着いた。スロットリング下では、Tab で
  フォーカスが `console-stream`（Console の内側スクロール領域）の奥の要素へ移るたびにブラウザが自動で
  発火するスクロール（focus のための `scrollIntoView` 相当）に連動して走る「最新へ」ボタン
  （`console-jump-to-latest`）の表示判定（React の再描画）が 4 倍遅れ、次の Tab 押下のタイミングと衝突して
  `document.activeElement` が一瞬 `<body>` に落ちることがある、という仮説を立てた。
- **修正**: `main()` の中で、perf 計測（`PERF_SETTLE_MS` の待ちと `readPerfMetrics`・JS/CSS 転送量の集計・
  `checkPerfBudget`）が終わった直後に `cdpSession.send("Emulation.setCPUThrottlingRate", { rate: 1 })` で
  スロットリングを解除してから `checkFocusOrder` を呼ぶ順序に変更した（以前は `checkFocusOrder` → perf 計測
  → `cdpSession.detach()` の順で、スロットリングは `checkFocusOrder` の間ずっと有効だった）。`focus-order`
  は実際のキー入力への応答性を見る検査なので、ミッドレンジ機の近似（`perf`）とは切り離して素の速度で
  行うのが筋でもある。修正後、`pnpm mobile-audit`（フルビルドを含む）を 1 回、`MOBILE_AUDIT_SKIP_BUILD=1`
  での再実行を複数回、新設の `--repeat 3` で 1 回（下記 3.）実行し、通算 8 回連続で `violations=0` を確認した
  （途中、`waitForPageIdle` を `runChecks` にも掛けていた段階で 1 回だけ `focus-order` が出たのは上記のとおり
  診断強化が機能した結果で、スロットリング解除後は一度も再現していない）。

### 2. P-G39-1: `/inbox` の残りの題材を機械検査対象に

- `scripts/lib/celeris-fixture.mjs` の `GET /inbox` fixture に以下を足した（`docs/celeris-api-v1.md` §5.1 の
  型どおり）:
  - 承認 1 件に `parent`（`approval-parent-title`）を追加。
  - 質問 1 件に `approval_id`（`question-approval-link`。既存の `approval_id` が無い分岐は Phase 87 から
    監査対象済みなので、これで両分岐とも通る）。
  - draft グループ 1 件（`draft-group`）×draft タスク 2 件（`draft-item` ×2）を追加。`counts.drafts` は
    celeris の契約どおり「draft タスクの件数」（グループ数ではない）なので `2` にした。
  - attention 1 件（`type: "failed"`、`attention-item`。`cluster_unavailable` 以外の分岐＝`AttentionRow`）。
- 上記が実際に描画されるようになったことで機械検査が新たに検出した違反（Phase 87 の `approval-title` と
  同じ「テキストだけの `<a>`/`<label>`」構造・`size="xs"` ボタンの `text-xs` 固定）を直した
  （`app/routes/inbox.tsx`）:
  - `tap-target` ×2: `approval-parent-title` 内の `<Link>`、`AttentionRow` の「受け入れ済み（ready）で
    始める」チェックボックスの `<label>`（`app/routes/projects.$id.tsx` の同じチェックボックスと同じ
    `flex min-h-11 items-center` を付けた）。
  - `tap-target`（fixture 拡張で追加した `draft-group`/`draft-item`/`question-approval-link` の
    `<Link>` にも同じ `flex min-h-11 items-center` を先回りで付けた。監査は通っていたが、直さなければ
    fixture 次第で踏む形だったため、Phase 87 の `approval-title` と同じ規律で一緒に直した）。
  - `font-size` ×5: `draft-approve`/`draft-cancel`（draft 2 件分）・`draft-approve-all` の `Button`
    （`size="xs"` は `text-xs`＝12px 固定で、アイコンを伴わない文字だけのボタンだと本文扱いになる）。
    `StatCard`/`Badge` と同じ「モバイルは `text-sm`、デスクトップは `lg:text-xs`」で `className` から
    上書きした（`cn()` は `tailwind-merge` ベースなので、`Button` に渡す `className` が `SIZES[size]` の
    `text-xs` に競合して勝つ）。
  - 修正後は `pnpm mobile-audit` が 26 route × light/dark で引き続き **0 件**。

### 3. 任意（受け入れ条件 3）: `--routes <glob>` と `--repeat <N>`

- `scripts/mobile-audit.mjs` に簡易な CLI 引数解析（`parseCliArgs`）を足した。`--routes <pattern>`（または
  `--routes=<pattern>`）はカンマ区切りで、各要素は `*` だけをワイルドカードとして扱う単純な正規表現に変換して
  `route` id（`scripts/lib/celeris-fixture.mjs::buildRoutes` の `route`）に前方一致ではなく完全一致もどきで
  当てる（`filterRoutes`。一致が 0 件ならエラーで即終了、ブラウザ・偽の celeris は起動しない）。
  `--repeat <N>`（または `--repeat=<N>`）は route/scheme の歩み全体を N 回繰り返す。
- ブラウザ・偽の celeris・ビルド済み GUI サーバは（`--repeat` のときも）1 回だけ起動し、繰り返すのは
  route/scheme のループだけにした（`allViolations`/`routeReports`/`perfRecords` を繰り返しの内側で作り直す）。
  `report.json` は繰り返しのたびに上書きするので最終回の内容が残り、標準エラーへの JSON・1 行要約は
  `--repeat` 指定時だけ `rep`/`repeat` を添えて回ごとに出す。既定（`--routes`/`--repeat` を渡さない）は
  これまでの「全 26 route を 1 回だけ」と 1 バイトも変わらない。
- 実測: `MOBILE_AUDIT_SKIP_BUILD=1 node scripts/mobile-audit.mjs --routes home,inbox --repeat 2` が
  2 route × light/dark × 2 回を数秒で終える（フルビルド・フル 26 route の `pnpm mobile-audit` は数十秒〜
  1 分規模）。`--routes nonexistent-route` はブラウザを起動せず `exit 1` で即エラーになることを確認した。
  `gui/docs/PROGRESS.md`（この節）に使い方を記録した（ドキュメントは他に無いため、ここが唯一の記述）。

### テスト（新規・変更）

新しいユニットテストは足していない（`mobile-audit.mjs`/`celeris-fixture.mjs` は Phase G39 から続く理由と
同じく、vitest からは重すぎる・DOM 依存のため、`pnpm mobile-audit`/`e2e:mock` の実行そのものを回帰検査に
した）。`app/routes/inbox.tsx` の変更は見た目（クラス名）だけで、`test/unit/inbox.loader.test.ts`/
`test/unit/inbox.action.test.ts` が検査するデータの形・action の挙動には触れていないため、既存テストは
無変更のまま通る。

### ゲート（証拠コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ |
| lint | `pnpm lint`（`biome check .`） | exit 0。`Checked 243 files … No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0（出力なし） |
| test | `pnpm test` | exit 0。**Test Files 65 passed (65) / Tests 1004 passed (1004)**（Phase G39 と同数） |
| build | `pnpm build` | exit 0（client・server とも）。既存の `[INEFFECTIVE_DYNAMIC_IMPORT]` 警告 2 件は変化なし |
| mobile-audit（原因特定前、`waitForPageIdle` を `runChecks` にも掛けていた段階） | `pnpm mobile-audit` | exit 1。`focus-order` 1 件（`home` light、`detail` に実際の focus 経路を出力） |
| mobile-audit（`waitForPageIdle` を `focus-order` だけに限定した直後、スロットリング解除前） | `pnpm mobile-audit` | exit 1。`focus-order` 1 件（`home` light、`console-block-reply` の後で `<body>` に落ちる経路） |
| mobile-audit（スロットリング解除後、`pnpm mobile-audit` 1 回 + `MOBILE_AUDIT_SKIP_BUILD=1` 再実行複数回 + `--repeat 3` 1 回） | 通算 8 回 | **すべて exit 0、`{"ok":true,"total":0}`、`routes=26 schemes=2 violations=0`** |
| e2e:mock | `pnpm e2e:mock` | **`{"ok": true, "mode": "mock", "failures": []}`** |
| `--routes`/`--repeat`（任意の受け入れ条件 3） | `node scripts/mobile-audit.mjs --routes home,inbox --repeat 2`（`MOBILE_AUDIT_SKIP_BUILD=1`） | exit 0。2 route × light/dark を 2 回、`rep=1/2`・`rep=2/2` それぞれ `violations=0` |

`crates/` を一切変更していないため `cargo test --workspace`/`cargo clippy --workspace -- -D warnings` はこの
Phase のスコープ外（実行していない。Phase 80/82/83/84/86/G39 と同じ扱い）。

### 変更したファイル

- `scripts/mobile-audit.mjs`（`waitForPageIdle` の追加（`checkFocusOrder` 専用）、`checkFocusOrder` の
  一意な連番ベースの同一性判定・診断強化（`detail` に focus 経路）、CPU スロットリング解除の順序変更
  （perf 計測後・`checkFocusOrder` 前）、`--routes`/`--repeat` の CLI 引数解析と、それに伴う route/scheme
  ループの繰り返し対応）
- `scripts/lib/celeris-fixture.mjs`（`/inbox` fixture に `approval.parent`・`question.approval_id`・
  `drafts`（1 グループ×2 件）・`attention`（1 件）を追加）
- `app/routes/inbox.tsx`（`approval-parent-title`/`draft-group` の親・`draft-item`/
  `question-approval-link`/attention の「受け入れ済み」チェックボックスに `flex min-h-11 items-center`、
  `draft-approve`/`draft-cancel`/`draft-approve-all` に `text-sm lg:text-xs`）

### 未解決事項

- **実機未確認**（ADR-0009 P-34。このサンドボックスに本物の celeris・本物のブラウザ・外向きネットワークが
  無い）。`focus-order` のフレークの機構（CPU スロットリングとスクロール連動の再描画の競合）は特定・解消
  したが、Console 自体（`useConsoleStream`・`console-jump-to-latest` の表示判定）は変更していない。実機・
  実 celeris（SSE が実際に `console.block` を流し続ける環境）で同種の競合が別の形で起きないかは未確認。
- `focusableCount`（`checkFocusOrder` が Tab 予算を決めるための数え方）は要素自身の `display`/`visibility`
  しか見ておらず、祖先が非表示のケースを数に入れてしまう既知の粗さがある（実測で 22 と数えたが実際に
  歩けたのは 16〜17 手だった）。予算は十分に余裕があるため実害は無いが、正確に数えるなら `isNotVisible`
  と同じ祖先を辿る判定に揃えるべき（次ラウンド候補。今回は「フレークの原因ではない」と切り分けが付いた
  ので手を付けていない）。
- P-G39-2（`cssPathRef` 等の単体テスト化の是非）は変化なし。

### 提案

- **P-G40-1**: `checkFocusOrder` の `focusableCount` を `isNotVisible`（祖先の `display:none`/`visibility:hidden`
  ・閉じた `<details>` を辿る）に揃えて正確に数える（現状は要素自身の可視性しか見ないので過大に数える）。
- **P-G40-2**: `perf` の CPU スロットリングを `checkFocusOrder` 以外の重い検査（`runChecks` 全体）にも
  意図的に掛けて「ミッドレンジ機での操作性」を見る監査を足すかどうか検討する（今回はスコープ外として
  むしろ外す方向にしたが、別ルールとして足す価値はあるかもしれない）。

## Phase G41 — スマホ UX ラウンド 14: タイムゾーンに安全な時刻表示（ADR-0055、celeris Phase 90。2026-09-22）

celeris 側は無変更（`crates/` 無変更。GUI だけの Phase）。Phase 84（G37、U-G31-3）の未解決事項 U-G37-1
（`~/lib/reports.ts::absoluteDateLabel` の絶対日付フォールバックが `Date` のローカル getter＝実行環境の
タイムゾーンに依存していて、SSR（GUI サーバー、通常 UTC）と CSR（ブラウザ）のタイムゾーンが食い違う実配置
ではハイドレーション直後に表示が変わりうる、というもの。人が実際にシカゴから見ていて GUI ホストが UTC、
という状況で顕在化しうる）を解消した。

### 1. `<LocalTime>` コンポーネントと純粋関数の明示的な timeZone 化（受け入れ条件 1）

- **`~/lib/reports.ts`**: `absoluteDateLabel`（従来は非 export の内部関数）を export し、`Date` の
  ローカル/UTC getter をやめて `Intl.DateTimeFormat`（`formatToParts` で年月日だけを取り出す
  `zonedDateParts`）に変えた。`absoluteDateLabel`/`relativeTimeLabel` はどちらも第 3 引数
  `timeZone: string = "UTC"`（IANA 名）を取る。新規 `dateTimeLabel(iso, timeZone = "UTC")`（年月日時分、
  `mode="datetime"` 用）も追加した。3 関数とも「絶対時刻そのものは返さない」規律は変えていない
  （呼び出し側 = `LocalTime` が `title`/`dateTime` 属性に生の ISO を残す）。
- **`~/lib/clock.ts`**（新規）: 「毎分すくなくとも 1 回」更新する共有の時計（受け入れ条件 3）。
  モジュールスコープに `now: number | null` と購読者集合を持ち、`setInterval` は購読者が 1 人以上いる間
  だけ 1 本（`LocalTime` を画面にいくつ並べても増えない）。`document.visibilitychange` でタブが非表示の
  間は止め、表示に戻ったら即座に 1 回進めてから再開する。`getServerClockSnapshot()` は常に `null`
  （サーバ・ハイドレーション前の決定的な値）。
- **`~/lib/time-zone.ts`**（新規）: 視聴者の「表示タイムゾーン」設定（受け入れ条件 2、詳細は次節）。
  `getServerTimeZoneSnapshot()` は常に `"UTC"`（決定的）。
- **`~/components/LocalTime.tsx`**（新規）: `iso`・`fetchedAtIso`（省略可）・`mode`
  （`"relative"`〈既定〉/`"absolute"`/`"datetime"`）・`className`・`dataTestId` を受け取り、`<time
  dateTime={iso} title={iso}>` を描く。`useSyncExternalStore` を 2 回使う
  （`subscribeTimeZonePreference`+`getServerTimeZoneSnapshot`、`subscribeClock`+`getServerClockSnapshot`）:
  **サーバとハイドレーション直後のクライアントは必ず同じ文字列を描画する**（タイムゾーンは `"UTC"`、
  「今」は `fetchedAtIso ?? iso`）。マウント後、React が `getServerSnapshot` から `getSnapshot` に切り
  替える通常の状態更新として、視聴者の解決済みタイムゾーンと毎分更新される時計に切り替わる。これは
  ハイドレーションの一致判定の**後**に起きる通常の再描画なので「Hydration failed」等の警告にはならない
  （`useSyncExternalStore` がこの用途のために提供する公式パターン）。
- **JSX からの直接呼び出しをすべて `<LocalTime>` に置き換えた**（`grep -rn "relativeTimeLabel\|
  absoluteDateLabel" app` で見つかった全 18 箇所。`~/lib/reports.ts` 自身と `LocalTime.tsx` 以外に
  直接呼び出しは残っていないことを確認済み）: `ArtifactsList.tsx`（1）・`ConsoleBlockItem.tsx`（2）・
  `ReportsList.tsx`（1）・`accounts.tsx`（5: `client.created_at`/`last_used_at`/`call.at`/
  `item.usage.observed_at`/`item.updated_at`〈秘密の更新時刻〉）・`approvals.tsx`（3: `head.created_at`/
  `approval.created_at`/`rule.created_at`）・`inbox.tsx`（2: `<time>` を `<LocalTime>` に統合。`dateTime`
  属性は `LocalTime` 自身が持つので二重にしない）・`tasks.$id.tsx`（4: run の `started_at`/`finished_at`、
  タイムラインの `item.at`/`last.at`）。`org.tsx` の `toLocaleString("ja-JP")`（`approx_tokens` の桁区切り、
  時刻ではない）はスコープ外のまま変更していない。
- 既存の `title={iso}` を個別に持っていたラッパー要素（`<span>`/`<p>`/`<time>`）は、`LocalTime` 自身が
  `title`/`dateTime` を持つため二重にせず、構造的に必要なもの（`<td>` 等）だけ残して中身を `LocalTime`
  に差し替えた。

### 2. 視聴者ごとの「表示タイムゾーン」設定（受け入れ条件 2）

- `~/lib/time-zone.ts`: `"auto"`（既定、`Intl.DateTimeFormat().resolvedOptions().timeZone` で検出）か
  固定の IANA 名を `localStorage`（キー `celeris:viewer-time-zone`）に保存する。celeris への問い合わせは
  無い、この端末・このブラウザだけの見た目の好み。読み書きは `typeof window` チェック + try/catch で包む
  （SSR・プライベートブラウジング・ストレージ無効化のいずれでも例外にしない）。同じタブ内の変更は自作
  イベント、他タブでの変更はブラウザ標準の `storage` イベントの両方で `useSyncExternalStore` の再描画に
  つながる。既知の候補 7 件（`auto`/UTC/Asia/Tokyo/America/Chicago/America/New_York/
  America/Los_Angeles/Europe/London）を `TIME_ZONE_OPTIONS` として持つ（`isKnownTimeZoneValue` で検査可能。
  `<select>` は自由入力ではなくここから選ぶ）。
- `~/components/TimeZonePreference.tsx`（新規、共通部品）: ラベル「表示タイムゾーン」+ `<select>` +
  「いま『{resolved}』として表示しています…現在時刻の例: {`<LocalTime mode="datetime">`}」というプレビュー
  行（設定を変えた効果がその場で分かるように。`mode="datetime"` の唯一の呼び出し元）。設定 UI 自身も
  `useSyncExternalStore`（`getServerTimeZonePreferenceSnapshot` は常に `"auto"`）でハイドレーション安全。
- モバイルの「その他」シート（`~/root.tsx::MobileTabBar`）のヘッダ直下と、`/help` の新しい節
  「表示設定」（`id="settings"`、目次にも追加）の両方に `<TimeZonePreference>` を置いた。

### 3. 相対時刻の毎分更新（受け入れ条件 3）

- `~/lib/clock.ts` の共有ストアで実現（上記 1 節）。`test/unit/clock.test.ts` で「複数回購読しても
  `setInterval` は 1 本だけ」「毎分すくなくとも 1 回、購読者に通知する」「最後の購読者が抜けると止まり、
  再購読すればまた動く」を `vi.useFakeTimers()` で確認した。

### 4. テスト（受け入れ条件 4）

- `test/unit/reports.test.ts`: 既存の「`relativeTimeLabel` の絶対日付フォールバック — ローカルタイム
  ゾーン（Phase 84, U-G31-3）」節（`process.env.TZ` を切り替える方式）を、`absoluteDateLabel`/
  `relativeTimeLabel` の第 3 引数に明示的な `timeZone` を渡す方式に置き換えた（`"UTC"` 既定・
  `Pacific/Kiritimati`〈年またぎが解消〉・`America/Chicago`〈DST 中は UTC-5、同じ瞬間でも日付が変わる〉の
  3 ケース）。新規 `dateTimeLabel` のテスト（UTC 既定・`Asia/Tokyo` で時刻がずれる）も追加した。
- `test/unit/clock.test.ts`（新規、5 件）・`test/unit/time-zone.test.ts`（新規、7 件。vitest の
  `environment: "node"`〈`window`/`document` が無い〉で「SSR と同じ経路」を自然に検査でき、`"auto"`/
  `"UTC"` へのフォールバックが例外を投げないことを直接確認できた）。
- `LocalTime`/`TimeZonePreference` 自体（React コンポーネント）の DOM 描画テストは追加していない
  （`~/lib/reports.ts` の docstring どおり「DOM を描画する単体テストはこのリポジトリに無い」慣例に従い、
  判断・計算はすべて純粋関数側でテストし、コンポーネントは薄い配線に留めた）。正しく配線されていることは
  `pnpm build` + `pnpm e2e:mock`（ハイドレーション検査、下記）で確認した。
- `scripts/e2e-check.mjs`: 既存のビューポート走査（26 route × mobile/desktop、UTC の既定タイムゾーン）
  に加えて、Playwright の `timezoneId` を `America/Chicago`/`Asia/Tokyo` にした 2 回の実行で `home`・
  `inbox` を開き、コンソールに "Hydration failed"/"Text content does not match"（大小文字を問わず正規表現
  一致）等のハイドレーション不一致警告が出ないことを確認する検査を追加した。**このサンドボックス
  （`pnpm e2e:mock`）は GUI サーバー・ブラウザとも UTC で、モック celeris の時刻も固定できない**ため、
  ブラウザ側だけを非 UTC にする形で「LocalTime が SSR/CSR の最初の描画を必ず一致させる設計になっている
  こと」を検査する（サーバー側も非 UTC にした「サーバーとブラウザが異なる非 UTC」の完全な組み合わせは
  実機でのみ確認できる。下記「未解決事項」）。

### ゲート（証拠コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ |
| lint | `pnpm lint`（`biome check .`。1 回目は accounts.tsx の折り返しで 1 件、`biome check --write .` で自動整形） | exit 0。`Checked 249 files … No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0（`react-router typegen && tsc -b`、出力なし） |
| test | `pnpm test` | exit 0。**Test Files 67 passed (67) / Tests 1019 passed (1019)**（Phase G40 の 1004 から +15: `clock.test.ts` 5、`time-zone.test.ts` 7、`reports.test.ts` +3） |
| build | `pnpm build` | exit 0（client・server とも）。既存の `[INEFFECTIVE_DYNAMIC_IMPORT]` 警告 2 件は変化なし。新規チャンク `LocalTime-*.js` 4.20kB（gzip 1.99kB） |
| mobile-audit | `MOBILE_AUDIT_SKIP_BUILD=1 pnpm mobile-audit` | **exit 0、`{"ok": true, "total": 0, "by_rule": {}, "by_scheme": {"light": 0, "dark": 0}}`**（26 route × light/dark。route 数・違反数とも Phase G40 から変化なし） |
| e2e:mock | `E2E_SKIP_BUILD=1 pnpm e2e:mock` | **`{"ok": true, "mode": "mock", "failures": []}`**（`America/Chicago`/`Asia/Tokyo` の 2 タイムゾーン × `home`/`inbox` のハイドレーション検査を含む。コンソールエラー・ハイドレーション警告とも 0 件） |

`crates/` を一切変更していないため `cargo test --workspace`/`cargo clippy --workspace -- -D warnings` はこの
Phase のスコープ外（実行していない。Phase 80/82/83/84/86/87/88/G39/G40 と同じ扱い）。

### 変更したファイル

- `app/lib/reports.ts`（`absoluteDateLabel` を export + `Intl.DateTimeFormat` ベースに、`relativeTimeLabel`
  に `timeZone` 引数、`dateTimeLabel` 新規）
- `app/lib/clock.ts`（新規）・`app/lib/time-zone.ts`（新規）
- `app/components/LocalTime.tsx`（新規）・`app/components/TimeZonePreference.tsx`（新規）
- `app/components/ArtifactsList.tsx`・`app/components/ConsoleBlockItem.tsx`・`app/components/ReportsList.tsx`・
  `app/routes/accounts.tsx`・`app/routes/approvals.tsx`・`app/routes/inbox.tsx`・`app/routes/tasks.$id.tsx`
  （`relativeTimeLabel`/`absoluteDateLabel` の直接呼び出しを `<LocalTime>` に置き換え）
- `app/root.tsx`（モバイル「その他」シートに `<TimeZonePreference>`）・`app/routes/help.tsx`（新しい節
  「表示設定」、目次に追加）
- `scripts/e2e-check.mjs`（`America/Chicago`/`Asia/Tokyo` でのハイドレーション検査を追加）
- `test/unit/reports.test.ts`（絶対日付フォールバックのテストを明示的 `timeZone` 方式に置き換え、
  `dateTimeLabel` のテスト追加）・`test/unit/clock.test.ts`（新規）・`test/unit/time-zone.test.ts`（新規）

### 未解決事項

- **実機未確認**（ADR-0009 P-34）。このサンドボックスは GUI サーバー・ブラウザとも UTC で、`pnpm e2e:mock`
  のハイドレーション検査もブラウザの `timezoneId` を変えるだけ（GUI サーバー側は依然 UTC）なので、
  「サーバーとブラウザの両方が非 UTC で、かつ互いに異なる」実配置そのものは再現できていない。実機
  （日本のスマホ、GUI サーバーは UTC 想定）で `/`・`/inbox`・7 日超の絶対日付が出る画面（`/reports`・
  `/accounts` 等）を開き、ブラウザの開発者コンソールにハイドレーション関連の警告/エラーが出ないこと、
  「その他」シート／`/help` の「表示タイムゾーン」を切り替えると表示が実際に変わることを確認する必要
  がある。
- 表示タイムゾーンの設定は `localStorage`（この端末・このブラウザだけ）が仕様どおりの範囲で、別の端末・
  シークレットウィンドウでは毎回「自動」に戻る。celeris 側に永続化する（アカウント単位の設定にする）
  要望が出たら、GUI 単独では実現できない（celeris 側のスキーマ拡張が要る）ため別途検討。
- `TIME_ZONE_OPTIONS` の候補は 7 件（auto/UTC/Asia/Tokyo/America/Chicago/America/New_York/
  America/Los_Angeles/Europe/London）に絞った。自由入力（任意の IANA 名をテキストで入力）にはしていない
  （`isKnownTimeZoneValue`/`Intl.DateTimeFormat` の妥当性検査は用意してあるので、要望があれば `<select>`
  に「その他（入力）」を足すだけで拡張できる）。
- `LocalTime`/`TimeZonePreference` コンポーネント自体の DOM 描画テストは無い（このリポジトリの既存の
  慣例どおり。`pnpm e2e:mock`/`pnpm mobile-audit`/実機確認で配線を確かめる）。

### 提案

- **P-G41-1**: U-G37-1 の完全な確認（GUI サーバーとブラウザが「互いに異なる非 UTC」の実配置）は、この
  サンドボックスの制約上どうしても実機頼みになる。次に実機確認の機会があれば、`TZ=Asia/Tokyo node
  server.js` のような GUI サーバー起動オプションを一時的に用意して、`e2e-check.mjs` 側からもサーバーの
  タイムゾーンを制御できるようにすると、このギャップをサンドボックス内で埋められる（今回はスコープ外
  として見送った）。
- **P-G41-2**: 「表示タイムゾーン」をアカウント単位で celeris 側に永続化する要望が出た場合、
  `docs/celeris-api-v1.md` にビューア設定用の小さなエンドポイントを足すかどうかの検討が要る（このワーク
  トリークは `crates/` に触れないため提案のみ）。

## Phase G42 — スマホ UX ラウンド 15: 本物のモバイル・エミュレーションで監査（ADR-0055、celeris Phase 91。2026-09-22）

celeris 側は無変更（`crates/` 無変更。GUI だけの Phase）。ADR-0055 D1 の機械検査（`gui/scripts/mobile-
audit.mjs`）は Phase 69 の最初のコミットから `isMobile: true`・`hasTouch: true`・Chrome-on-Android の
UA・`deviceScaleFactor 2.75` を指定していた（機能的には既に「本物のモバイル」だった）が、(1) その設定を
Playwright の組み込みデバイス記述子に揃えて `gui/scripts/e2e-check.mjs` の mobile viewport にも同じ設定を
広げ、(2) 実際のタッチ入力（CDP）で操作する検査、(3) `100dvh`/`env(safe-area-inset-bottom)` の構造検査を
新設した。(3) の開発中に、Console 入力欄の `position: fixed` が壊れている実バグを 1 件発見して直した。

### 1. 忠実性（受け入れ条件 1）

- **`gui/scripts/lib/celeris-fixture.mjs`**: `MOBILE_DEVICE` を新設した。Playwright の `devices["Pixel 7"]`
  （`isMobile`/`hasTouch`/`defaultBrowserType` 等、実機の Chrome-on-Android に近い挙動一式）を土台に、
  ADR-0055 D1 が指定する寸法（393×851）・`deviceScaleFactor`（2.75）・UA（Nothing Phone 2a、Phase 69 から
  変わらない文字列）だけを上書きする。mobile-audit.mjs と e2e-check.mjs の両方がここから取ることで、
  「2 か所に同じ寸法・UA を書くと片方だけ更新し忘れる」事故を防ぐ（このファイルの既存の設計方針どおり）。
- **`gui/scripts/mobile-audit.mjs`**: 手で列挙していた `VIEWPORT`/`DEVICE_SCALE_FACTOR`/`USER_AGENT` の
  定数と `browser.newContext({...})` の個別指定を、`MOBILE_DEVICE` を直接渡す形に置き換えた。値そのものは
  変わっていないので、26 route × light/dark の違反数は **0→0**（変化なし）。
- **`gui/scripts/e2e-check.mjs`**: `VIEWPORTS` の `mobile` エントリを、`{ width, height }` だけの指定から
  `MOBILE_DEVICE`（`isMobile`/`hasTouch`/UA/dpr を含む）に変えた。`desktop` エントリは指示どおり
  `{ viewport: { width: 1280, height: 800 } }` のまま変更していない（非モバイル）。`isMobile: true` は
  viewport-meta の扱い・スクロールバーの挙動を変えるので、`pnpm e2e:mock` を再実行して確認したが、
  新たな失敗（コンソールエラー・失敗した要求・構造チェック）は出なかった（`{"ok":true,"failures":[]}`
  のまま、`home`/`accounts`/`knowledge-skills`/`inbox` の構造チェック・タスクのタブ切り替え・ハイドレー
  ション検査〈`America/Chicago`/`Asia/Tokyo`〉すべて変化なし）。

### 2. タッチ操作（受け入れ条件 2、新設ルール）

`gui/scripts/mobile-audit.mjs` に 2 つの D2 拡張ルールを追加した。どちらも **light scheme だけ**で行う
（`perf` と同じ慣例。dark は色だけなのでタッチの結果は変わらない、実行時間を抑えるため）。CPU スロット
リング解除後（`focus-order` と同じ理由）、`focus-order` の**後**で呼ぶ（タップがフォーカスを動かしうる
ため、先に `focus-order` の Tab 歩行を済ませておく必要がある）。

- **`touch-scroll`**: `overflow-x: auto/scroll` かつ `scrollWidth > clientWidth` なコンテナ全部（D1-6
  `checkTables` が見る「表を包む箱」と同種のもの全般）に対し、CDP `Input.dispatchTouchEvent`
  （`touchStart`→`touchMove` を 10 段階→`touchEnd`）で実際にスワイプし、`scrollLeft` が実際に動く
  ことを確認する。JS の合成 `TouchEvent` ではブラウザのネイティブなオーバーフロー・スクロール
  （compositor が担う）は動かないため、CDP の低レベル入力を直接叩く（Playwright の `touchscreen.tap()`
  が内部でしていることのスワイプ版。同じ `page` で light scheme のときだけ生きている CPU スロットリング
  用の CDP セッションを使い回す）。判定後は元の `scrollLeft` に戻す。
- **`tap`**: 各画面の「主な操作」（`[data-primary-action]` があればそれ、無ければ最初の
  `button[type="submit"]`、それも無ければ primary variant の `Button`〈`bg-primary`+`text-primary-fg`〉）
  を `click()` ではなく `locator.tap()`（`hasTouch: true` のコンテキストで CDP のタッチ入力）で操作し、
  コンソールエラー・例外が出ないことを確認する。無効化されている（`disabled`/`aria-disabled`）ボタンや
  対象が無い画面はスキップ（違反にしない）。`<a>` は対象外（タップでナビゲーションが起きるとこの監査の
  前提が崩れるため、意図して `button` だけを見る）。

**開発中に見つけた 2 件は監査スクリプト自身の不具合で、アプリのコードは直していない**（いずれも既存の
慣例を見落としていたことが原因）:

1. `tap` が `releases`（`release-promote`）・`knowledge/skills`（`skill-delete-submit`）の主操作を
   「見える」と誤判定し、`locator.tap()` が 5 秒でタイムアウトしていた。原因は、これらの操作が閉じた
   `<details>` の中にあり、Chromium の `getComputedStyle` 上は `display: none` に**ならない**という
   既知の落とし穴（`isNotVisible` の docstring に既にある。Phase 76 以来の慣例）を、`checkPrimaryAction
   Tap` 内の可視判定が踏んでいたため。`isNotVisible`（`addInitScript` で全ページに注入済み）を
   `window.__isNotVisible` として明示的に公開し、`checkPrimaryActionTap` から再利用するよう直した
   （`page.evaluate` は addInitScript の関数宣言を暗黙のグローバルとしては見つけられない — 明示的な
   `window.__xxx` への代入が要ることを実測で確認。既存の `window.__cssPathRef`/`window.__run
   MobileAudit` と同じ理由）。
2. `touch-scroll` が `task-changes`/`task-files` のタブ行（`[data-testid="task-tabs"] ul`）を誤検知して
   いた（全 5 タブ画面で同じ DOM 構造・同じ `scrollWidth`/`clientWidth` のはずなのに 2 画面だけ失敗する、
   タイミング依存の症状）。原因は、先行する `checkFixedOverlays`（D1-5）が文書全体を末尾までスクロール
   したまま戻さない（内側スクロール領域だけ戻す作り）ため、CDP のビューポート相対座標がずれていたこと。
   対象コンテナを `scrollIntoViewIfNeeded()` してから測るよう修正し、`checkTouchScroll` の冒頭で
   `window.scrollTo(0, 0)` にも戻すようにした。

偽の celeris（`celeris-fixture.mjs::setupMockCeleris`）は GET しか実装しない（このリポジトリの e2e/監査の
慣例: 書き込みを試さない）ので、`tap` が変更系の POST を発行すると、GUI の action は celeris からの
404（"not_found"）をそのまま HTTP ステータスとして返す（`clusters.tsx` の `cluster_connect` 等）。Chromium
はこの種の非 2xx な応答を、アプリの JS が実際に `console.error` を呼んだかどうかに関わらず「Failed to
load resource: the server responded with a status of NNN」として自動的にコンソールへ出す（ブラウザ自身の
ネットワークログで、アプリのバグの兆候ではない）。この定型メッセージだけを `tap` の対象から除いた
（正規表現 1 つ）。26 route（light のみ）で最終的に **0 件**。

### 3. ビューポート単位（受け入れ条件 3、新設ルール、実バグの発見）

`checkViewportUnits`（D1 拡張その 5）を追加した。Console の入力欄（`[data-testid="console-input"]`）と
下部固定タブバー（`[data-testid="mobile-tabbar"]`）の両方が画面にある場合に、入力欄の下端がタブバーの
上端より下に出ない（＝重ならない）こと、両方ともビューポート内（`top >= 0` かつ `bottom <= height`）に
収まっていることを確かめる。どちらか一方でも無い画面（Console 以外）は対象外。

**実際に発見して直したバグ**: `home`/`org-node`（Console がある 2 route）の light/dark で、入力欄の下端
（814〜816px）がタブバーの上端（787px）より下に出ていた＝入力欄がタブバーの下（画面外）に一部隠れて
いた。原因は `~/root.tsx` の `<div className="animate-fade-in"><Outlet /></div>`（各画面共通のページ
遷移アニメーション。Console 入力欄はこの中でレンダーされる）にあった:

- `--animate-fade-in: fade-in 0.25s ease-out both;`（`app/app.css`）の `animation-fill-mode: both` は、
  アニメーション終了後も `to` の値（`transform: none`）を「適用中」として保持し続ける。Chromium は
  この「今もアニメーションが効いている」要素を、fixed/absolute な子孫の containing block に差し替えて
  しまう（`position: fixed` の入力欄が、ビューポートではなくこの `.animate-fade-in` div を基準に配置
  される）。`transform: none` という値そのものは見た目には何もしない（恒等変換）が、Chromium の
  `getComputedStyle` はこれを行列 `matrix(1,0,0,1,0,0)` として返し、CSS の仕様上「`none` 以外の
  `transform`」はやはり containing block を作る対象になる。
- 加えて、`transform` プロパティ自体（アニメーション実行中の 0.25 秒間、`translateY(4px)` → `none`）も
  同じ理由で containing block を差し替える。

`app/app.css` を 2 か所直した（見た目は変えていない）:

1. `@keyframes fade-in` を `transform: translateY(4px)`/`none` から、独立した CSS プロパティ
   `translate: 0 4px`/`translate: 0` に変更した。`translate`/`scale`/`rotate` は `transform` と違い
   containing block を作らない（仕様上の明記）。
2. `--animate-fade-in` の `animation-fill-mode` を `both` から `backwards` に変更した（`forwards` を
   外した）。`to` の状態（動きが無い静止状態）は要素本来の既定の見た目と一致するので、`forwards` を
   外しても見た目は変わらない。

2 つとも直した結果、`home`/`org-node` とも **0 件**になった（実測: `x=0, width=393, bottom=787`、
`offsetParent: null` ＝ containing block がビューポートに戻ったことを直接確認した）。

**この検査自体の実装で踏んだ落とし穴（デバッグに時間を要した）**: 上記のバグを CSS で直した直後、
`checkViewportUnits` を D1 の他の検査と同じ `runChecks()`（`load` 直後に 1 回だけ呼ぶ）に入れたまま
26 route フルの `pnpm mobile-audit` を回すと、**壊れていたときと同じ `home`/`org-node` の 2 route で
違反が検出できなかった**（見逃す）。原因は 2 つ:

1. `runChecks()` の中で `checkFixedOverlays`（D1-5）が `checkViewportUnits` より先に文書全体を末尾まで
   スクロールし、内側スクロール領域だけ戻して文書自体のスクロール位置は戻さない。壊れていた入力欄は
   実質「通常フローの要素」のように振る舞っていたので、スクロールに連動して画面外へ動いてしまい、
   「タブバーと重ならない」という判定がたまたま真になっていた。`checkViewportUnits` の冒頭で
   `window.scrollTo(0, 0)` に戻すよう直した。
2. CSS を修正した後も、`.animate-fade-in` の 0.25 秒のフェードインアニメーションが**実際に再生中**の
   間は、アニメーション対象が `translate`（containing block を作らないはずのプロパティ）であっても、
   Chromium は同じ containing block の差し替えを一時的に行うことを実測で確認した（`getAnimations()`
   が空になる＝アニメーションが完全に終わるまで続く、`animation-fill-mode` を `backwards` にしていても）。
   `load` 直後（アニメーション開始直後）に測ると、直したはずのバグを毎回検出してしまう（一過性の偽陽性）。
   `checkViewportUnits` を `runChecks()` の外へ切り出し（`checkViewportUnitsSettled`、Node 側）、composer と
   タブバーが両方ある画面だけアニメーション時間分（0.25 秒 + 余裕 = 400ms）待ってから呼ぶようにした
   （対象画面が少ないので実行時間への影響は軽い）。`focus-order` の**後**（両スキームで）呼ぶ。

### 4. ゲート（証拠コマンドと出力の要点）

| 条件 | コマンド | 出力の要点 |
| --- | --- | --- |
| gen:types | `pnpm gen:types && git diff --exit-code app/celeris/types.ts` | 差分ゼロ |
| lint | `pnpm lint` | exit 0。`Checked 249 files … No fixes applied.` |
| typecheck | `pnpm typecheck` | exit 0（`react-router typegen && tsc -b`、出力なし） |
| test | `pnpm test` | exit 0。**Test Files 67 passed (67) / Tests 1019 passed (1019)**（Phase G41 から件数不変。新設の検査は script レベルの統合検査で、vitest の単体テストは追加していない） |
| build | `pnpm build` | exit 0（client・server とも）。既存の `[INEFFECTIVE_DYNAMIC_IMPORT]` 警告 2 件は変化なし |
| mobile-audit（1 回目） | `MOBILE_AUDIT_SKIP_BUILD=1 pnpm mobile-audit` | exit 0、**`{"ok":true,"total":0,"by_rule":{},"by_scheme":{"light":0,"dark":0}}`**、`routes=26 schemes=2 violations=0`。実測 **89.2 秒**（`time` コマンド、build 別） |
| mobile-audit（2 回目） | 同上 | exit 0、同じ `{"ok":true,"total":0}`。実測 **89.1 秒** |
| mobile-audit（修正前の再現、参考） | `--routes "home,org-node,org-detail"` を CSS 未修正の状態で実行 | exit 1、**`{"total":4,"by_rule":{"viewport-units":4},"by_scheme":{"light":2,"dark":2}}`**（`home`/`org-node` の light/dark。詳細は上記「3.」） |
| e2e:mock | `E2E_SKIP_BUILD=1 pnpm e2e:mock` | **`{"ok":true,"mode":"mock","failures":[]}`** |

`crates/` を一切変更していないため `cargo test --workspace`/`cargo clippy --workspace -- -D warnings` はこの
Phase のスコープ外（実行していない。Phase 80/82/83/84/86/87/88/G39/G40/G41 と同じ扱い）。

release ゲート換算の目安: `pnpm-mobile-audit` は Phase G41（79.9 秒）から新設の `touch-scroll`/`tap`/
`viewport-units` 分で **+9〜10 秒**（89 秒台）。release ゲートの `SD_AUDIT_TIMEOUT`（既定 600 秒）には
十分な余裕がある。

### 変更したファイル

- `scripts/lib/celeris-fixture.mjs`（`MOBILE_DEVICE` 新設。`@playwright/test` の `devices` を使うため
  `createRequire` を追加）
- `scripts/mobile-audit.mjs`（`MOBILE_DEVICE` を使うようコンテキスト生成を置き換え、`checkViewportUnits`・
  `checkViewportUnitsSettled`・`checkTouchScroll`・`checkPrimaryActionTap` を新設し、メインループに配線）
- `scripts/e2e-check.mjs`（mobile viewport を `MOBILE_DEVICE` に。desktop は無変更）
- `app/app.css`（`.animate-fade-in`/`@keyframes fade-in` — `transform` を `translate` に、
  `animation-fill-mode` を `both` から `backwards` に）

### 未解決事項

- 実機未確認（ADR-0009 P-34）。ヘッドレス Chromium は `env(safe-area-inset-bottom)` を実機のように 0 より
  大きい値へ解決しないため、`viewport-units` 検査は「重ならない・ビューポート内」という構造の健全性
  までしか確かめられない。実機（ホームインジケータの帯がある機種）で Console 画面を開き、入力欄が
  帯の上に正しく来ることを目視確認する必要がある。
- `.animate-fade-in` に起因する一過性（0.25 秒間、実際にアニメーションが再生している間だけ）の containing
  block の差し替えは、`transform` を使わない（`translate` にした）今回の修正でも完全には無くならないこと
  を実測で確認した（Chromium が「アニメーション実行中の要素」自体を特別扱いしている可能性が高い。
  スコープ外の深掘りはしていない）。恒久的な破損（ページを開いている間ずっと壊れたまま）は直したが、
  ページ表示直後の 0.25 秒だけ入力欄の位置がわずかにずれる余地は残る（実害はほぼ無いと判断: この間に
  ユーザーが入力欄を操作することは現実的にまず無い）。完全に無くすには、Console 入力欄
  （`ConsoleInput`）を `~/root.tsx` の `.animate-fade-in`／`<Outlet/>` の外（`MobileTabBar` と同じ階層）に
  移すアーキテクチャ変更が要る。次にこの種の「固定要素の親をアニメーションさせる」変更をするときは、
  この落とし穴（Chromium は非 `transform` プロパティのアニメーションでも実行中は containing block を
  差し替えうる）を踏まえること。
- `touch-scroll`/`tap` は実行時間予算のため light scheme だけで行う（既存の `perf` と同じ判断）。dark
  固有のタッチ挙動の違いは通常無い（色だけが変わる設計）という前提に乗っている。
- 本番 = Phase 65〜90。実装中: Phase G42（このワークトリー。GUI のみ）。

### 提案

- **P-G42-1**: Console 入力欄を `~/root.tsx` のレイアウトレベル（`MobileTabBar` と同じ階層）に移せば、
  ページ遷移アニメーションの影響を構造的に受けなくなる。現状は `Console` コンポーネントの一部として
  各ルートの `<Outlet/>` 配下でレンダーされているため、この移動には「どのルートが Console を表示するか」
  をレイアウト側に伝える仕組み（context か prop）が要る、それなりの大きさの変更になる。次にこの画面の
  構造を触る Phase があれば検討する。
