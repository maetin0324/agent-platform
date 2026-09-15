# ADR-GUI-0003: G0 の雛形で確定した細部（ツールチェーン、セキュリティ middleware、taskd 停止時の扱い、taskd.sh）

- 日付: 2026-09-15
- 状態: **Accepted**（Phase G0 の実装で確定。docs/DESIGN.md §6 / §8 / §10 と docs/adr/0002 の範囲内の細部の決定）
- 関連: docs/DESIGN.md §6.3, §6.5, §8.2, §9, §10.0 / docs/adr/0002 D1, D5, D8, D9

## 1. 文脈

G0 は雛形の選択と BFF の骨格を作るフェーズ（strong）。ADR-0002 が版と方針を決めているが、実装時に決めるべき細部が残っていた:
TypeScript 7 が動くか、`minimumReleaseAge` 7 日と版固定の両立、`Host` 検査と CSP nonce をどこに置くか、taskd 停止時に loader が何を返すか、
`scripts/taskd.sh` の配置、shadcn/ui の初期化方法。

## 2. 決定

### D1. TypeScript 7.0.2（Go 実装）をそのまま使う。`pnpm typecheck` = `react-router typegen && tsc -b`

- `tsc -b`（project references: `tsconfig.node.json` = server.js と各 config、`tsconfig.vite.json` = app / server / test / e2e）が 7.0.2 で動いた。6.0.x への fallback（ADR-0002 D9）は不要。
- `checkJs: true` で `server.js` も検査する。

### D2. 版固定と 7 日 cooldown の両立: **cooldown を優先し、7 日以上経った最新版に固定**する

- `pnpm-workspace.yaml` に `minimumReleaseAge: 10080` と `strictDepBuilds: true`（`allowBuilds` は空。install 時に build script が必要な依存は現状無い）。
- 2026-09-15 時点で 7 日未満だったため、DESIGN §0 の「React 19.3」ではなく **React 19.2.8**、Vite **8.2.2**（8.3.0 は 09-10 公開）、Biome **2.5.12**、`@types/node` **24.13.3**（Node 24 LTS に合わせて 24 系）にした。
  React Router 8.3.1 の peer（`react >= 19.2.7`）は満たす。月 1 回の更新で 19.3 系に上げる（ADR-0002 D9 の更新規則）。
- テンプレートにあった `compression` / `morgan` / `cross-env` は入れない（ログは server.js の JSON 1 行、`NODE_ENV` はシェルで渡す）。`@testing-library/react` は G0 では使わないので G1 で入れる。

### D3. ブラウザ ↔ GUI のセキュリティは **root の React Router middleware** に置く（`app/middleware/security.server.ts`）

- `hostCheck`: `Host` を `localhost` / `127.0.0.1` / `[::1]` / `TASKD_GUI_BIND` のホスト / `TASKD_GUI_ALLOWED_HOSTS` と照合（ポート無視）。外れれば `throw new Response(…, {status: 400})`。
  loader より前に止まるので taskd は呼ばれない。resource route（`/healthz`、G1 の `/events`）にも効く。
- `securityHeaders`: 要求ごとに 16 byte の乱数 nonce を作って `RouterContext`（`nonceContext`）に置き、`next()` の応答に CSP（DESIGN §8.2 の値 + `form-action 'self'`）、`X-Content-Type-Options`、`Referrer-Policy`、`X-Frame-Options: DENY`、`Cache-Control: no-store`（未設定のとき）を付ける。
- nonce の流れ: middleware → `RouterContextProvider` → `entry.server.tsx`（`loadContext.get(nonceContext)`）→ `renderToPipeableStream({nonce})` と React context（`NonceContext`）→ `Layout` の `<Scripts nonce>` / `<ScrollRestoration nonce>`。
- **開発時（`import.meta.env.DEV`）だけ `script-src 'self' 'unsafe-inline'`**（Vite の React Refresh が nonce 無しの inline script を入れるため）。本番・e2e は nonce 付きの厳格な CSP。
- Express 側（`server.js`）は bind の検証、静的配信、要求ログだけを持つ。`react-router dev` でも同じ middleware が効く（ADR-0002 D5）。

### D4. taskd 停止時: **root loader は例外を投げず状態を返す**。子ルートは投げる

- `loadHealth(client)` が `TaskdUnavailable` → `{health: null, unavailable: true}`、`TaskdError`（例: 401）→ `{health: null, problem: "401 unauthorized"}` を返し、root は HTML を 200 で描いてバナーを出す（DESIGN §6.5「500 にしない」）。
- バナー表示中は `useRevalidator` で 5 秒ごとに root だけ再検証し、復旧したら消える。
- G1 以降の子ルートの loader は `TaskdUnavailable` をそのまま投げ、各ルートの `ErrorBoundary` が同じバナーを出す（root は既に描けている）。
- `TaskdClient` は `app/taskd/client.server.ts`（サーバ専用）、エラー型は `app/taskd/errors.ts`（Node API を使わないのでクライアントからも import 可。ErrorBoundary で `name` を見る）。

### D5. `server.js` は G5 まで **非 loopback への bind を拒否**する（exit 2）

パスワード認証（DESIGN §8.2）は G5 で入れる。それまで `TASKD_GUI_BIND` が loopback 以外なら起動しない（認証無しで LAN に開けない）。

### D6. `scripts/taskd.sh` の形

- `.run/<name>/{taskd.toml, fake-worker.sh, taskd.sqlite3, workspaces/, taskd.log, taskd.pid}`。`taskd.toml` は `test/taskd/taskd.toml.tmpl` から `@RUN_DIR@` / `@API_LISTEN@` を置換して作る（相対パスは設定ファイル基準）。
- `[api] listen` は `TASKD_API_LISTEN`（既定 `127.0.0.1:7710`）。同じポートに別プロセスが答えていれば `start` は失敗する（取り残し検出）。pid は `exec` で起動した taskd 自身のもの。
- `fixture <scenario>` は骨組みだけ（`case` に G1 以降でシナリオを足す）。`taskctl <name> …` で `--db` を補って `taskctl` を呼べる。

### D7. 型生成

`scripts/gen-types.mjs` が `$TASKD_REPO/docs/api/v1/api-v1.schema.json`（JSON Schema 2020-12、`$defs`）を `json2ts --additionalProperties=false` で `app/taskd/types.ts` にする。
`json-schema-to-typescript` 16 は 2020-12 の `$defs` を読めた（taskd 側の draft-07 切り替えは不要）。生成物は Biome の対象から外す（`biome.json` の `!app/taskd/types.ts`）。

### D8. shadcn/ui

G0 では `components.json` と `app/lib/utils.ts` を置くまで（部品は G1 以降で `shadcn add` する）。`pnpm dlx shadcn init` が対話や `^` 付きの依存追加を伴う場合は、
必要なファイルを手で書き、依存は版固定で足す（結果は PROGRESS に記録）。

## 3. 影響

- G1 の子ルートは `getTaskdClient()` と `TaskdUnavailable` をそのまま投げる規約に従う。
- CSP を厳格にしたい開発者は `pnpm build && pnpm start` で確認する（`pnpm dev` は `'unsafe-inline'`）。
- 版更新は月 1 回、`minimumReleaseAge` を満たす版に上げる。
