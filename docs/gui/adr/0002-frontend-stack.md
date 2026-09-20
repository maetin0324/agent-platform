# ADR-GUI-0002: フロントエンドスタック — React + Remix（React Router framework mode）

- 日付: 2026-09-14（初版 Proposed → 同日 人間の決定で改訂。一次情報の確認日も 2026-09-14）
- 状態: **Accepted**（人間の決定 H2「React を使い、Remix を採用する」。§4 の確認事項は 2026-09-14 に全て本 ADR の案どおり確定。DESIGN-GUI §11 の H11）
- 関連: [ADR-GUI-0001](0001-architecture-boundary.md) / `docs/gui/DESIGN-GUI.md` §6, §8, §9, §10 / `docs/gui/api.md`

## 1. 文脈

- 人間の決定: **React + Remix**。初版（React 19 + Vite SPA + TanStack Router/Query、Rust の単一バイナリに埋め込み）は React の選定だけが残り、
  フレームワークと配布形態は Remix 前提で組み直す。
- 境界（ADR-GUI-0001）: ブラウザは celeris を直接呼ばない。`celeris-gui` のサーバ（BFF）が celeris の HTTP API v1 を呼ぶ。
- 2026 年 9 月時点で「React ベースの Remix」が何を指すかは自明でないため、一次情報で確認した（§2）。
- 実装と保守の多くを Claude Code に任せる（`run-gphases.sh` で G フェーズを自律進行）。学習データの多さと規約の明確さを重視する。
- npm のサプライチェーン事故（2025-09 Shai-Hulud、2026-08 再発）を前提に、pnpm の `minimumReleaseAge` 7 日（H8）と依存の少なさに配慮する。

## 2. 「Remix」の実体の確認（一次情報、2026-09-14）

| 事実 | 出典（確認日 2026-09-14） |
|---|---|
| 2024-05-15「Merging Remix and React Router」: "What we planned to release as **Remix v3** is now going to be released as **React Router v7**." 移行は `@remix-run/*` → `react-router` の import 置換で codemod を提供 | https://remix.run/blog/merging-remix-and-react-router |
| 2024-11-22「React Router v7」: "React Router v7 brings everything you love about Remix back into React Router proper." "We encourage all Remix v2 users to upgrade to React Router v7." `react-router` 7.0.0 は 2024-11-22 公開 | https://remix.run/blog/react-router-v7 、npm registry `time["7.0.0"]` |
| React Router の Remix v2 からの移行ガイド: "React Router v7 is the next major version of Remix after v2." パッケージ対応 `@remix-run/react` → `react-router`、`@remix-run/node` → `@react-router/node`、`@remix-run/dev` → `@react-router/dev`、`@remix-run/serve` → `@react-router/serve`、`@remix-run/express` → `@react-router/express`、`@remix-run/fs-routes` → `@react-router/fs-routes`。スクリプトは `react-router dev / build / typegen && tsc`、`react-router-serve` | https://reactrouter.com/7.18.3/upgrading/remix |
| 2026-06-17「React Router v8」: `react-router` 8.0.0 公開（registry `time["8.0.0"]` = 2026-06-17）。最低 **Node 22.22.0、React 19.2.7、Vite 7**。ESM のみ。middleware が既定。Framework Mode は v7 と同じ。"React Router v7 will continue to receive security updates, just like v6 (and Remix v2) did." **"With the release of React Router v8 we are officially marking React Router v6 and Remix v2 as End of Life (EOL)"** | https://remix.run/blog/react-router-v8 |
| `react-router` の最新は **8.3.1（2026-08-28）**、MIT、`engines.node >=22.22.0`、peer `react >=19.2.7`。`version-7` タグは 7.18.3（2026-08-28）。`@react-router/dev` / `node` / `serve` / `express` / `fs-routes` も 8.3.1、`@react-router/dev` の peer は `vite ^7 || ^8`、`typescript ^5.1 || ^6 || ^7` | npm registry（`registry.npmjs.org/<pkg>` の `dist-tags` / `time` / `versions[].engines` / `peerDependencies`） |
| `@remix-run/react` の最新は 2.17.5（2026-06-01）、peer **`react ^18`**（React 19 不可）、内部で `react-router 6.30.4` に依存。上記のとおり EOL | npm registry |
| 2026-04-30「Remix 3 Beta Preview」、2026-08-31「Remix 3 Release Candidate」: "A full-stack JavaScript framework built on web primitives"。"a brand-new UI runtime complete with composable event handling, styles, animations, and built-in components"。"We talked about the ideas we love from React and React Router (previously Remix v2)." **2026-10-02 に Remix Jam で 3.0 を公開予定**。インストールは `npx remix@next new` | https://remix.run/blog/remix-3-beta-preview 、https://remix.run/blog/remix-3-release-candidate |
| `remix` npm: `latest` 2.17.5、`next` **3.0.0-rc.2（2026-09-08）**。rc の `engines.node >=24.3.0`。依存は全て `@remix-run/*`（`fetch-router`、`ui`、`data-table`、`html-template`、`render-middleware` …）で、**`react` / `react-dom` への依存も peer も無い**。`@remix-run/ui` 0.9.0 の説明は "UI runtime, headless primitives, and styled components for Remix" | npm registry（`versions["3.0.0-rc.2"].dependencies` / `peerDependencies`） |
| remix.run のトップページは "The fully-stacked web framework"（Web API 上に server runtime, routing, auth, sessions, DB, **a UI framework**, assets, styling, components）と説明し、**React にも React Router にも言及しない** | https://remix.run/ |
| GitHub remix-run/remix の直近リリースは `ui v0.9.0`、`static-middleware`、`session-middleware`、`render-middleware`、`node-hmr` 等の Remix 3 系パッケージ（2026-09-08） | https://github.com/remix-run/remix/releases |

### 2.1 オーケストレータの理解に対する正誤

| 理解 | 判定 | 根拠 |
|---|---|---|
| 「Remix v2 は React Router v7 の framework mode に統合された」 | **正しい**。さらに 2026-06-17 に v8 が出て、Remix v2 は EOL | 2024-05-15 / 2024-11-22 / 2026-06-17 の公式ブログ、移行ガイド |
| 「Remix 3 は React に依存しない方向と発表された」 | **正しい**（公式ブログは "React を使わない" と明言してはいないが、Remix 3 は独自の UI runtime（`@remix-run/ui`）を持ち、`remix@3.0.0-rc.2` は `react` に依存も peer 指定もしない。トップページも React に触れない） | RC の依存関係、`@remix-run/ui` の説明、remix.run トップ |

### 2.2 解釈: 「React + Remix」を最も忠実に満たすもの

| 候補 | React を使うか | 保守状況 | 判定 |
|---|---|---|---|
| **React Router v8 framework mode**（`react-router` 8.3.1 + `@react-router/dev` / `node` / `serve`） | ✓（React 19.2.7+） | 現行。Remix v2 の直接の後継（旧称 "Remix v3"）。年 1 回のメジャー | **採用** |
| Remix v2（`@remix-run/react` 2.17.5） | ✓（React 18 のみ） | **EOL（2026-06-17）**。React 19 不可 | 不採用 |
| Remix 3（`remix@3.0.0-rc.2`） | ✗（独自 UI runtime。React 不使用） | RC。2026-10-02 に 3.0 予定。Node 24.3+ | 「React + Remix」を満たさないので不採用。§4 で確認 |
| React Router v7（7.18.3） | ✓（React 18 可） | セキュリティ更新のみ | 新規に始める理由が無い。不採用 |

**結論: 「Remix」= React Router v8 の framework mode と解釈する。** 名前は React Router だが、これが Remix v2 の設計（loader / action / nested routes / SSR / resource routes）を
React で使う唯一の現行系列であり、公式が Remix v2 利用者に案内した移行先である。以下、本文書と DESIGN-GUI では「Remix（framework mode）」と書いたら
React Router v8 framework mode を指す。

## 3. 決定

### D1. フレームワーク: React Router **8.3.x** framework mode（`react-router`、`@react-router/dev`、`@react-router/node`、`@react-router/express`）、React **19.3.x**

- `react-router.config.ts` に `ssr: true`（既定）。`app/routes.ts` で明示的にルートを定義（`@react-router/fs-routes` は使わない。ルート数が少なく、明示の方が Claude Code に読みやすい）。
- 型は `react-router typegen` が生成する `+types/` を使い、`pnpm typecheck` = `react-router typegen && tsc --noEmit`。

### D2. SSR にする（SPA mode は採らない）

- SPA mode（`ssr: false`）は「サーバの loader / action が無い」モードで、データ取得は `clientLoader` = ブラウザから直接になる。ブラウザが celeris を呼ばない（ADR-GUI-0001 D1）ためには
  BFF に別の API 層を自作することになり、framework mode を採る意味が無くなる。
- SSR なら **サーバ側 `loader` / `action` がそのまま BFF**。celeris のトークンはサーバにしか無い。クライアント遷移時も loader はサーバで実行される（`.data` 要求）。
- 出典: SPA mode の仕様（https://reactrouter.com/how-to/spa "Regular loaders and actions don't run on a server in SPA mode"）、データ取得（https://reactrouter.com/start/framework/data-loading "Automatically removed from client bundles (safe for server-only APIs)"）。

### D3. データ取得と更新: loader / action だけ。**クライアントのキャッシュライブラリ（TanStack Query）は入れない**

- 読み取り: 各ルートの `loader` が `CelerisClient`（サーバ専用モジュール `app/celeris/client.server.ts`）で celeris を呼び、`loaderData` として描画する。
- 更新: `<Form method="post">` / `useFetcher()` → `action` → celeris の `POST`。action 完了後は React Router がそのページの loader を**自動で再検証**する（キャッシュの手動無効化が要らない）。
- リアルタイム: resource route `/events`（D4）から `EventSource` で `task.event` / `daemon` を受け、`useRevalidator().revalidate()` を 250 ms でデバウンスして呼ぶ。
  イベント本体から状態を組み立てない（真実は celeris）。
- 一覧の絞り込み・ページングは URL の検索パラメータ（`?status=…&cursor=…`）に持ち、loader が読む。
- TanStack Router / Query は不要になる（ルータは React Router、サーバ状態は loader）。`@tanstack/react-virtual` だけ残す（仮想スクロールは React 汎用）。
- 出典: https://reactrouter.com/start/framework/data-loading（`useRevalidator` / `shouldRevalidate`）、https://reactrouter.com/how-to/resource-routes 。

### D4. SSE の中継は resource route

`app/routes/events.ts`（default export 無し、`loader` のみ）が celeris の `GET /api/v1/stream` を `fetch` し、`Last-Event-ID` と `?task_id=` を転送して
`Response(body, {headers: {"Content-Type": "text/event-stream"}})` で**そのまま流す**（`request.signal` で上流を閉じる）。
出典: resource routes の仕様（"return a Response … Content-Type: text/event-stream"）。

### D5. サーバ: 本番は `@react-router/express` + Express 5 の小さな `server.js`、開発は `react-router dev`

- 公式テンプレート `node-custom-server` の形（https://reactrouter.com/start/framework/deploying 、`@react-router/serve` も内部は Express 5 + `@react-router/express`）。
  自前にする理由: バインドアドレス / ポートを CLI・環境変数で決める、`build/client` を `immutable` で配信する、起動時に設定検証（非 loopback ならパスワード必須）をして exit 2 で止める。
- `Host` 許可リスト、ブラウザ ↔ GUI の認証、CSRF 検査は **React Router の middleware（v8 で既定機能）** としてルートに置く（`react-router dev` でも同じコードが効く）。

### D6. 配布: Node 24 LTS + `build/` ディレクトリ + systemd。単一バイナリは実験項目

| 形態 | 決め |
|---|---|
| 必須 | Node **24 LTS**（`v24.21.0`、2026-09-07。React Router 8 の最低 22.22.0 を満たす。Node 22 は Maintenance LTS）。`pnpm build` の `build/`（`build/server/index.js` + `build/client/`）+ `server.js` + `package.json` / `pnpm-lock.yaml`。`pnpm install --prod --frozen-lockfile` → `node server.js` |
| リリース物 | `celeris-gui-<ver>.tar.gz`（上記一式）。systemd の unit 例（`Environment=CELERIS_API_URL=… CELERIS_API_TOKEN_FILE=…`、`ExecStart=node server.js`） |
| 任意 | コンテナ（`Dockerfile`、`node:24-slim`） |
| 実験 | **Node SEA**（single executable applications）: Node 26.8.2 のドキュメントで Stability 1.1（Active development）、ESM の main と assets の同梱に対応（https://nodejs.org/api/single-executable-applications.html ）。サーバ側を 1 ファイルにバンドル（依存ごと）してから `node --build-sea` する必要があり、React Router の SSR ビルドとの相性は未検証。G5 の「できれば」項目に留め、失敗しても G5 の完了を妨げない |

- 初版の「Rust 単一バイナリに静的アセットを埋め込む」は、SSR サーバが Node である以上成立しない（Node ランタイムが必要）。人間に確認（§4 の 2）。

### D7. UI 部品・スタイル・可視化・ビューア

| 用途 | 選定 | 版（2026-09-14 時点の `latest`） | 備考 |
|---|---|---|---|
| CSS | Tailwind CSS + `@tailwindcss/vite` | 4.3.3 | `create-react-router` の既定テンプレートが Tailwind を設定済み |
| UI 部品 | shadcn/ui（Base UI 版） | `shadcn` CLI 4.21.0 / `@base-ui/react` 1.8.0 | `pnpm dlx shadcn@latest init -t react-router` が公式にある（https://ui.shadcn.com/docs/installation/react-router ）。部品はソースとして持つ |
| DAG | `@xyflow/react`（React Flow 12）+ `@dagrejs/dagre` | 12.11.6 / 3.1.1 | クライアント専用。SSR では `HydrateFallback` 相当のプレースホルダを出し、`useEffect` 後に描く。ELK は使わない |
| 仮想スクロール | `@tanstack/react-virtual` | 3.14.13 | |
| コード / ログ / JSON | CodeMirror 6（`@codemirror/view` 6.43.11、`@codemirror/state` 6.7.4、`lang-json` 6.0.2、`lang-markdown` 6.5.2）読み取り専用 | | クライアント専用。Monaco は不採用（サイズ） |
| Markdown | `react-markdown` 10.1.0 + `remark-gfm` 4.0.1 | | HTML パススルー無し（LLM 生成物） |
| SSE | ブラウザ標準 `EventSource`（薄いフック `useCelerisStream`） | | |

### D8. テスト

| 層 | 道具 | 方針 |
|---|---|---|
| 単体・コンポーネント | Vitest 5.0.x + `@testing-library/react` 16.3.x + `createRoutesStub`（`react-router` 同梱） | loader / action は**関数として**テストし、`CelerisClient` を差し替える。ルート部品は `createRoutesStub` で描画（https://reactrouter.com/start/framework/testing ） |
| celeris API のモック | `test/mock-celeris/`: Node の `http.createServer` で `api-v1` の固定応答（`test/fixtures/api/*.json`）と SSE を返す**プロセス内サーバ**。loopback のみ | 単体テストは実 celeris 無しで動く。固定応答は G1 で実 celeris から `curl` で採取してコミットし、`pnpm gen:types` の型で検証する |
| 結合（実 celeris） | Playwright 1.63.x（chromium のみ）+ `scripts/celeris.sh`（`$CELERIS_REPO` を `cargo build -p celeris -p celerisctl`、fake ワーカー + `[api]` の `config.toml` で起動）+ `scripts/fixture.sh`（`celerisctl` と `celeris --until-idle` で既知の DB を作る） | 受け入れ条件はこれで示す。全て loopback |
| a11y | `@axe-core/playwright` 4.13.0（MPL-2.0。テスト専用依存） | G5 でクリティカル 0 |
| 外部ネットワーク | テストは出ない。例外は `pnpm install`（パッケージ取得）と `pnpm exec playwright install chromium`（ブラウザ取得）の 2 つだけ | |

### D9. Lint / 型 / パッケージ管理

| 事項 | 決め |
|---|---|
| Lint / Format | Biome 2.5.x（`pnpm lint` = `biome check .`、`pnpm format` = `biome format --write .`）。ESLint / Prettier は入れない |
| TypeScript | **7.0.x**（Go 実装、2026-07-08）。`@react-router/dev` の peer が `^7` を含む。`react-router typegen` か `tsc` が 7 で動かなければ **6.0.x（JS 実装の最終系列、6.0.3 = 2026-04-16）** に下げてよい（G0 で判断し PROGRESS に記録） |
| パッケージ管理 | **pnpm 11.x**（`latest-11` = 11.26.0、2026-09-06）。pnpm 12（12.0.0 = 2026-08-26）は公開 3 か月後に検討。設定: `minimumReleaseAge = 10080`（7 日、H8）、`strictDepBuilds`、`allowBuilds` 許可リスト、CI は `--frozen-lockfile`。`package.json` の `packageManager` で版を固定 |
| 版の固定 | `package.json` は完全固定（`^` 無し）。更新は月 1 回まとめて。メジャー更新は公開から 3 か月以上経ってから。RC / next / canary は使わない |
| 型生成 | `json-schema-to-typescript` 16.0.0（`pnpm gen:types`）。生成物 `app/celeris/types.ts` をコミット、CI で差分ゼロ |
| 実行時検証 | 入れない（zod 等）。契約は celeris の schema。BFF は celeris の応答を信頼する |

## 4. 人間に確認すべき点（2026-09-14 確認済み: 全て本 ADR の案どおり）

1. **「Remix」の解釈**: 本 ADR は「Remix = React Router v8 framework mode」と解釈した。もし意図が Remix 3（React を使わない新フレームワーク）なら、
   「React を使う」と両立しないため再設計になる。→ **確認済み**: 本 ADR の解釈（React Router 8 framework mode）で進める。
2. **配布形態**: GUI は Node 24 ランタイムを必要とする（単一バイナリではない）。許容できるか。Node SEA は実験項目。→ **確認済み**: 許容（Node 24 ランタイム + `build/`。SEA は実験項目のまま）。
3. **Node の版**: 開発機の Node は `v22.21.0` で、React Router 8 の最低 `22.22.0` を満たさない。Node 24 LTS への更新が前提（`docs/gui/bootstrap/README.md`）。→ **確認済み**: 更新する（人間がホストで実施。`run-gphases.sh` の preflight が検査する）。
4. **pnpm 11 か 12 か**: 11.x にした（12 は公開 3 週間）。12 でよければ変える。→ **確認済み**: pnpm 11。
5. **TypeScript 7**: 7.0 系は programmatic API が未整備（7.1 予定）。本スタックはそれに依存しないが、問題が出れば 6.0.x に下げる方針でよいか。→ **確認済み**: この方針でよい。

## 5. スタック一覧（Remix 前提で組み直したもの）

| 層 | 選定 | 版 | 理由・備考 |
|---|---|---|---|
| ランタイム | Node 24 LTS | 24.21.0 | React Router 8 の最低 22.22.0。Vitest 5 は 22.12+ / 24 / 26 |
| 言語 | TypeScript | 7.0.x（fallback 6.0.x） | D9 |
| フレームワーク | React Router framework mode（旧 Remix） | 8.3.x | D1。React 19.3.x |
| ビルド | Vite（`@react-router/dev` 経由） | 8.3.x | Rolldown 統一。`@react-router/dev` の peer `^7 || ^8` |
| サーバ | `@react-router/express` + Express 5 の `server.js` | 8.3.x / 5.x | D5 |
| ルーティング / データ | React Router の loader / action / middleware | | D3 |
| UI 部品 / CSS | shadcn/ui（Base UI）+ Tailwind 4 | 4.21.x / 1.8.x / 4.3.x | D7 |
| DAG | `@xyflow/react` + `@dagrejs/dagre` | 12.11.x / 3.1.x | D7 |
| 仮想スクロール | `@tanstack/react-virtual` | 3.14.x | |
| ビューア | CodeMirror 6 / `react-markdown` + `remark-gfm` | | D7 |
| SSE | `EventSource` + resource route | | D4 |
| 型 | `api-v1.schema.json` → `json-schema-to-typescript` 16 | | D9 |
| テスト | Vitest 5 / Testing Library 16 / Playwright 1.63 / axe | | D8 |
| Lint | Biome 2.5 | | D9 |
| パッケージ | pnpm 11（cooldown 7 日） | | D9 |
| 配布 | `build/` + Node 24 + systemd（tar.gz）。コンテナ任意。SEA 実験 | | D6 |

## 6. 不採用・次点（人間の決定を踏まえた更新）

| 候補 | 扱い |
|---|---|
| Remix v2（`@remix-run/*` 2.x） | EOL、React 18 のみ。不採用 |
| Remix 3（`remix@next`） | React を使わない。不採用（§4 の 1 で確認） |
| React Router v7 | セキュリティ更新のみ。新規採用の理由なし |
| React + Vite SPA + TanStack Router/Query（初版の推奨） | 人間の決定で Remix に置換。SPA では BFF を別に作る必要があり、H1 とも合わない |
| SPA mode（`ssr: false`） | D2 |
| htmx + Rust テンプレート（初版の次点） | 「React を使う」決定と合わない。次点から外す |
| Next.js | 人間の決定は Remix。比較は行わない |
| Vue / Svelte / Solid / Preact / Leptos / Dioxus / Lit / Angular / Datastar | 初版の評価（React が最上位）のとおり不採用。理由は初版 §8 と同じ（DAG ライブラリの成熟度、学習データ、破壊的変更の予定） |
| Rust 単一バイナリ（axum + rust-embed） | SSR サーバが Node になるため成立しない。不採用 |
| `@react-router/serve` をそのまま本番に | バインド制御・起動時検証のため自前の `server.js`（D5）。開発の手軽さでは同等 |
| `@react-router/fs-routes` | 明示的な `routes.ts` を選ぶ（D1） |
| ELK（`elkjs`） | ライセンス（EPL/GPL）とサイズ。dagre で足りる |

## 7. 再評価の条件

- React Router 9（年 1 回のメジャー、2027 年半ばと推定）が出て 3 か月経過。
- Remix 3 が React を公式に選択肢として扱うようになったとき（現時点で兆候なし）。
- TypeScript 7.1 で programmatic API が安定したとき（7.0.x の fallback 規定を外す）。
- pnpm 12 公開から 3 か月（12 へ更新）。
- Node 26 が LTS 入り（2026-10 予定）して 3 か月（24 → 26 は急がない）。
- Node SEA が Stable になったとき（単一バイナリ配布を本番形態に昇格）。

## 8. 出典（一次）

- Remix ブログ: https://remix.run/blog/merging-remix-and-react-router （2024-05-15）、https://remix.run/blog/react-router-v7 （2024-11-22）、
  https://remix.run/blog/react-router-v8 （2026-06-17）、https://remix.run/blog/remix-3-beta-preview （2026-04-30）、https://remix.run/blog/remix-3-release-candidate （2026-08-31）
- Remix トップ: https://remix.run/ 、GitHub Releases: https://github.com/remix-run/remix/releases
- React Router ドキュメント: https://reactrouter.com/7.18.3/upgrading/remix 、https://reactrouter.com/start/framework/data-loading 、
  https://reactrouter.com/how-to/resource-routes 、https://reactrouter.com/how-to/spa 、https://reactrouter.com/start/framework/deploying 、https://reactrouter.com/start/framework/testing
- npm registry（`dist-tags` / `time` / `engines` / `peerDependencies`、2026-09-14 取得）: `react-router`、`@react-router/{dev,node,serve,express,fs-routes}`、`@remix-run/react`、`remix`、`@remix-run/ui`、
  `react`、`react-dom`、`vite`、`vitest`、`@playwright/test`、`@biomejs/biome`、`tailwindcss`、`@tailwindcss/vite`、`@xyflow/react`、`@dagrejs/dagre`、`@codemirror/*`、`react-markdown`、`remark-gfm`、
  `json-schema-to-typescript`、`typescript`、`@tanstack/react-virtual`、`@testing-library/react`、`@base-ui/react`、`shadcn`、`@axe-core/playwright`、`pnpm`
- Node: https://nodejs.org/dist/index.json （LTS 一覧）、https://nodejs.org/api/single-executable-applications.html （SEA、v26.8.2 の文書）
- shadcn/ui: https://ui.shadcn.com/docs/installation/react-router
