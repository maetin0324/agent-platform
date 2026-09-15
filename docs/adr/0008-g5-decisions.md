# ADR-GUI-0008: Phase G5（認証・配布・仕上げ）で確定した細部

- 日付: 2026-09-15
- 状態: **Accepted**（Phase G5 の実装で確定。docs/DESIGN.md §8, §9, §10 Phase G5 の範囲内の細部）
- 関連: docs/DESIGN.md §8.1, §8.2, §9, §10 Phase G5 / docs/taskd-api-v1.md §1.1〜§1.3 / docs/adr/0003 D3, D5 / docs/adr/0005 D1

## 1. 文脈

G5 は「認証・配布・仕上げ」（strong）。非 loopback バインド時のパスワード認証とセッションクッキー、BFF → taskd のトークン、CSP の確認、a11y、
`server.js` と systemd unit、リリース tar.gz、任意で Dockerfile、実験で Node SEA、README を作る。設計判断を要する点を以下に記録する。

## 2. 決定

### D1. 認証の判定は root の React Router middleware（`authCheck`）で行い、`server.js` は起動時の環境変数検証だけを担う

- 順序: `hostCheck` → `authCheck` → `csrfCheck` → `securityHeaders`（`app/root.tsx` の `middleware`）。Host 検査の後、CSRF 検査の前。
- 未認証のとき: `/events` と `/files/*`（resource route。ブラウザの `EventSource` / `<a download>` / `<img>` から呼ばれ、302 で `/login` に飛ばしても意味が無い）は
  **401**（`WWW-Authenticate: Cookie`、本文 `unauthorized`）。それ以外（document request と `.data` request）は **302 `/login?next=<path>`**。
  `next` は同一オリジンのパス（`/` で始まり `//` で始まらない）だけを受け付ける（オープンリダイレクト防止）。
- 認証の対象外: `/login`（GET/POST）、`/logout`（POST）、`/healthz`（GUI 自身の生存確認。taskd の情報は含まない。Playwright の `webServer.url` にも使う）。
- `server.js` は起動時に `TASKD_GUI_BIND` が非 loopback で `TASKD_GUI_PASSWORD_FILE` が無ければ exit 2（stderr に理由）。パスワードファイルが指定されていて
  読めない・空でも exit 2。同じ検証を `app/auth.server.ts` の `readAuthConfig` も行う（React Router 側が単独で動く単体テストのため）。

### D2. 認証が有効になる条件 = バインドが非 loopback、**または** `TASKD_GUI_PASSWORD_FILE` が明示されている

- docs/DESIGN.md §8.2 は「非 loopback → パスワード必須、loopback → 認証無し」。loopback でパスワードファイルを**明示**したときに無視するのは運用者の
  意図に反する（設定したのに効かない）ので、明示されていれば loopback でも認証を要求する（opt-in）。既定（何も設定しない loopback）は従来どおり認証無し。
- 受け入れ条件 1 の e2e は設計どおり `TASKD_GUI_BIND=0.0.0.0:<port>` で起動して検証する（パスワード付き・数秒間・ランダムなパスワード）。

### D3. セッションクッキーは React Router の `createCookie`（HMAC-SHA256 署名）で発行し、サーバ側にセッション表は持たない

- 名前 `__taskd_gui_session`、`HttpOnly; SameSite=Strict; Path=/`、`Secure` は要求の URL が `https:` のときだけ（GUI 自身は http。TLS 終端を前に置く構成は
  `trust proxy` を有効にしていないので想定外。README に注記）。`maxAge` は 24 時間（`docs/DESIGN.md` に規定が無いので決めた。長い放置での漏えい窓と、
  日次の再ログインのバランス）。
- 値は `{ iat: <ms>, id: <128bit 乱数 hex> }` を署名したもの。署名鍵は `TASKD_GUI_SESSION_SECRET_FILE` の内容（trim）、無ければプロセス起動時の 256bit 乱数
  （再起動でログアウト。docs/DESIGN.md §8.2 どおり）。署名検証に失敗した・`iat` が `maxAge` より古いクッキーは未認証扱い。
- 新しい依存は足さない（`react-router` の `createCookie` は Web Crypto の HMAC を使う。`cookie-signature` 相当の実装を自作しない）。

### D4. パスワード比較は SHA-256 ダイジェスト同士の `timingSafeEqual`、失敗時は無条件に 1 秒待ってから 401 のログインページ

- 長さの異なる文字列を直接 `timingSafeEqual` に掛けられないため、両者を SHA-256 にしてから比較する（長さが一定になる）。
- 失敗応答は `/login` を **status 401** で再描画（`data({ error }, { status: 401 })`）。ブラウザは同じフォームを見る。`curl` は 401 を見る。
- パスワードは `TASKD_GUI_PASSWORD_FILE` の内容を trim した 1 行。ハッシュ化した保存形式（bcrypt 等）は単一利用者・ファイル権限で守る前提なので採らない
  （新しい依存を避ける。docs/DESIGN.md §8.2 も平文ファイル前提）。

### D5. 未認証の要求では root loader が taskd を呼ばない

- `/login` を描画する root loader は、`authCheck` が context に置く `sessionContext`（`{ enabled, authenticated }`）を見て、未認証なら `health`/`counts` を
  取りに行かず `null` を返す。ログイン前の画面に taskd の版・schema・接続先を出さないため（情報の最小化）。ナビゲーションも出さない。

### D6. taskd の 401 は「接続不可」ではなく「認可失敗」のバナーとして root で出す

- `GET /health` は taskd 側で無認証（docs/taskd-api-v1.md §1.3）なので、トークン無しでも root の `loadHealth` は成功する。トークンが無い・違うときに
  最初に失敗するのは root loader の `GET /inbox`（承認待ちバッジ用）。これが `TaskdError` の 401 なら `problem = "401 unauthorized"` として返し、
  `TaskdBanner` に「taskd の応答: 401 unauthorized」を出す（受け入れ条件 2 の「バナーに `unauthorized`」）。health が取れていても `problem` があれば
  バナーを出すように `App` を変える。他のステータス（5xx 等）は従来どおり `counts = null` に留める（子ルートの ErrorBoundary が個別に出す）。

### D7. CSP の `style-src 'unsafe-inline'` は外せない（確認結果）

- `@xyflow/react`（ノード位置の `style="transform: …"`）と `@tanstack/react-virtual`（行の `style="transform: translateY(…)"`）がインライン **style 属性**を使う。
  style 属性は nonce で許可できない（nonce は `<style>` 要素だけ）。`style-src-attr 'unsafe-inline'` に絞る案は、Tailwind v4 は外部 CSS だけなので
  `style-src-elem` から `'unsafe-inline'` を落とせる可能性があるが、CodeMirror の `style-mod` が `<style>` 要素を JS で挿入するため
  `EditorView.cspNonce` の配線が要る。G5 の範囲（確認）としては現状維持とし、PROGRESS の提案に書く。
- 「Playwright の全シナリオでコンソールに CSP 違反が 0 件」は `e2e/test.ts` に auto fixture を持つ `test` ラッパーを置き、全 spec がそれを import する。
  違反はブラウザのコンソールに `Content Security Policy` を含むエラーとして出るので、それを集めてテスト終了時に 0 件を assert する。

### D8. a11y は `@axe-core/playwright` 4.13.0（2026-08-11 公開、7 日以上経過）で、critical / serious だけをゲートにする

- moderate / minor は PROGRESS の未解決事項に列挙する（受け入れ条件は critical / serious が 0 件）。

### D9. リリースは `scripts/release.sh`（`pnpm release`）が `dist/taskd-gui-<version>.tar.gz` を作る。smoke は `--offline` で install する

- 同梱: `build/`、`server.js`、`package.json`、`pnpm-lock.yaml`、`pnpm-workspace.yaml`（`minimumReleaseAge` / `strictDepBuilds` の設定。無いと
  `--frozen-lockfile` の設定照合が変わる）、`README.md`、`deploy/taskd-gui.service`、`LICENSE`（あれば）。tar のトップは `taskd-gui-<version>/`。
- smoke（`e2e/g5-release.spec.ts`）は tar を空の一時ディレクトリに展開し、`pnpm install --prod --frozen-lockfile --ignore-scripts --offline` を実行する。
  テスト中に外部ネットワークへ出ないため `--offline`（開発機の pnpm store に本番依存は全て入っている）。展開先で `node server.js` を別ポートで起動し、`/` が 200 を返すことを見る。

### D10. Dockerfile は同梱するが `docker build` は G5 の検証に含めない（任意項目）

- `node:24-slim`、非 root、`build/` と本番依存だけ。ビルドには registry へのアクセスが要るため、この環境では実行しない（結果は PROGRESS に記録）。

### D11. Node SEA は「サーバ 1 ファイル化 → `--experimental-sea-config`」を 1 回だけ試し、結果を PROGRESS に記録する（成否は問わない）

### D12. トークン検証用の taskd は `scripts/taskd.sh fixture auth`（`test/taskd/auth.toml.tmpl` + `api.token`）

- `.run/auth/api.token` に乱数トークンを書き、`[api] token_file = "api.token"` の設定で起動する。DB は `basic` と同様 `--until-idle` で最小限（1 タスク）を作る。
- e2e はこの taskd に対して GUI を 2 回起動する（`TASKD_API_TOKEN_FILE` あり / 無し）。GUI の stderr は `.run/auth/gui.log` / `.run/auth/gui-notoken.log` に落とし、
  受け入れ条件 2 の `grep -r "<token>" build/ .run/*/gui*.log` を e2e の中で実行する。

## 3. 影響

- 追加ファイル: `app/auth.server.ts`、`app/routes/login.tsx`、`app/routes/logout.ts`、`e2e/test.ts`、`e2e/g5.spec.ts`、`e2e/g5-a11y.spec.ts`、`e2e/g5-release.spec.ts`、
  `scripts/release.sh`、`deploy/taskd-gui.service`、`Dockerfile`、`README.md`、`test/taskd/auth.toml.tmpl`、`test/unit/auth.test.ts`。
- 変更: `server.js`（起動時検証）、`app/root.tsx`（middleware・loader・バナー・ログアウト）、`app/routes.ts`、`scripts/taskd.sh`（`fixture auth`）、`package.json`（`release`、`@axe-core/playwright`）。
