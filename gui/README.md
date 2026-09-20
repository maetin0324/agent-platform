# celeris-gui

celeris（研究タスクの自律実行デーモン）の Web GUI。React Router 8（Remix。framework mode、SSR）の Node サーバが
BFF（Backend for Frontend）として celeris の HTTP API v1（`docs/celeris-api-v1.md`）を呼ぶ。**SQLite には触らない**。
ブラウザは celeris を直接呼ばず、celeris の API トークンもブラウザには渡らない。設計の詳細は `docs/DESIGN.md` を参照。

## 必要なもの

- Node **24 LTS**
- pnpm **11**（`packageManager` で固定。`corepack enable` を推奨）
- celeris が `[api] listen = "..."` を設定して起動していること（`docs/celeris-api-v1.md` §1）

## リリース物の導入手順

1. リリース tar を展開する。

   ```sh
   tar xzf celeris-gui-<version>.tar.gz
   cd celeris-gui-<version>
   ```

2. 本番依存だけを、ロックファイルどおりに、install スクリプトを実行せずに入れる。

   ```sh
   pnpm install --prod --frozen-lockfile --ignore-scripts
   ```

3. 環境変数を設定する（下表）。

4. 起動する。

   ```sh
   node server.js
   ```

   起動に失敗する場合（設定不備）は `exit 2` になり、理由が stderr に出る。正常時は stderr に
   `celeris-gui: listening on http://<host>:<port> (celeris API <url>, auth <password|none>, token <yes|no>)` が 1 行出て、その後は要求ごとに
   JSON 1 行（パス・status・所要時間）を出す。本文やトークンはログに出さない。

## 環境変数

| 変数 | 既定 | 説明 |
|---|---|---|
| `CELERIS_API_URL` | `http://127.0.0.1:7710` | celeris の HTTP API のベース URL |
| `CELERIS_API_TOKEN_FILE` | （無し） | celeris 側の `token_file` と同じ内容のファイル。celeris が `[api] token_file` を設定している場合は必須（無いと BFF からの呼び出しは celeris に `401 unauthorized` として拒否され、GUI 側は画面上部のバナーに `unauthorized` を出す） |
| `CELERIS_GUI_BIND` | `127.0.0.1:7700` | GUI 自身が listen する `host:port` |
| `CELERIS_GUI_PASSWORD_FILE` | （無し） | 非 loopback バインド時は必須（無いと起動時に `exit 2`）。ファイルの内容（trim した 1 行）がログインパスワードになる。loopback バインドでも、このファイルを明示すれば認証を要求する（opt-in） |
| `CELERIS_GUI_SESSION_SECRET_FILE` | （無し） | セッションクッキーの署名鍵。任意。無いとプロセス起動時にランダムな鍵を生成する（= サーバ再起動でログアウトされる） |
| `CELERIS_GUI_ALLOWED_HOSTS` | （無し） | GUI 自身の `Host` 検査で許可する追加のホスト名（カンマ区切り）。バインドしているホスト・ポートは常に許可される |

## SSH ポートフォワードでの利用（推奨）

celeris-gui は単一ユーザ・ローカル前提の設計（`docs/DESIGN.md` §2, §8）。最も安全な使い方は、GUI をホスト側で
`127.0.0.1:7700` に bind したまま、手元の端末から SSH でポートフォワードすることです。

```sh
ssh -L 7700:127.0.0.1:7700 <host>
```

その上でブラウザから `http://127.0.0.1:7700/` を開く。この構成では loopback バインドのままなので
（`CELERIS_GUI_PASSWORD_FILE` を明示しない限り）追加の認証は不要です。

### 非 loopback（`0.0.0.0` 等）で公開する場合の注意

- `CELERIS_GUI_PASSWORD_FILE` が必須（無いと起動しない）。
- `CELERIS_GUI_ALLOWED_HOSTS` に実際に使うホスト名を設定すること。`Host` 検査の許可リストにはバインドのホスト（`0.0.0.0` 等）も入るため、
  公開時は利用者がアクセスする名前を明示的に許可し、それ以外を 400 で落とす運用にする。
- GUI は平文 HTTP のみで提供する。**TLS 終端をリバースプロキシで前段に置いても、セッションクッキーへの
  `Secure` 属性の付与には対応していない**（`docs/adr/0008` D3。GUI 自身が `https:` の URL で要求を受けたときだけ
  `Secure` を付ける実装で、`trust proxy` を有効化していないため）。TLS を挟む場合はこの制約を理解した上で使うこと。
- 単一ユーザ・単一パスワード前提であり、マルチユーザの権限・監査は無い（`docs/DESIGN.md` §2）。
- パスワード失敗時は 1 秒待って 401 を返すだけで、並列の試行回数は制限しない。非 loopback で公開するなら、前段（SSH、ファイアウォール、
  リバースプロキシ）で接続元とレートを制限すること。

## systemd

`deploy/celeris-gui.service` を参考に配置する。

```sh
sudo cp deploy/celeris-gui.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now celeris-gui.service
```

unit 中の `After=`/`Wants=celeris.service` は例。実際の celeris 本体の unit 名に合わせて書き換えること。
`WorkingDirectory=/opt/celeris-gui` に上記の「リリース物の導入手順」で展開・`pnpm install` 済みの一式を置く想定。

## Docker（任意）

`Dockerfile` を同梱している（`docker build` はこのリポジトリの検証には含めていない。ビルドに registry への
アクセスが要るため）。

```sh
docker build -t celeris-gui .
docker run --network host \
  -e CELERIS_API_URL=http://127.0.0.1:7710 \
  -e CELERIS_API_TOKEN_FILE=/run/secrets/celeris-api-token \
  -v /path/to/token:/run/secrets/celeris-api-token:ro \
  celeris-gui
```

`--network host` を使わない場合は、`CELERIS_API_URL` でホスト側の celeris を指す（例: `http://host.docker.internal:7710`）。
コンテナ内のバインドは常に非 loopback（`0.0.0.0`）になるため、`CELERIS_GUI_PASSWORD_FILE`（と必要なら
`CELERIS_GUI_SESSION_SECRET_FILE`）を必ず渡すこと。

## 開発

```sh
pnpm install
scripts/celeris.sh build
scripts/celeris.sh start dev
pnpm dev
```

- `pnpm test` — 単体テスト（Vitest。`test/mock-celeris/` を使い、外部ネットワークには出ない）
- `pnpm e2e` — 結合テスト（Playwright。`scripts/celeris.sh start dev` 等の実 celeris に対して行う）。
  `e2e/g7.spec.ts` の `scripts/celeris.sh fixture clusters` は `~/.ssh/config` の `celeris-localhost`（localhost への ssh 多重接続）を使う
  （docs/adr/0010 D1）。無ければ `ssh -MNf celeris-localhost` を先に張っておくこと（無いと `fixture clusters` が明確に失敗する。
  外部ネットワークには出ない）
- `pnpm lint` / `pnpm typecheck`
- `pnpm release` — `dist/celeris-gui-<version>.tar.gz` を作る（`scripts/release.sh`）
- `scripts/celeris.sh` が作る `.run/`（celeris の DB・ログ）はローカルディスク（既定 `${TMPDIR:-/tmp}/celeris-gui-run-$USER`）へのシンボリックリンク。
  SQLite の WAL は NFS 上では動かないため。置き場所は `CELERIS_RUN_ROOT` で変えられる（`docs/adr/0008` D13）

## `/healthz`

`GET /healthz` は GUI 自身の生存確認用で、**認証を必要としない**。celeris の情報は含まない（バージョンや接続先など、
celeris 側の情報は認証後の画面フッタでのみ表示する）。プロセス監視や `pnpm e2e` の `webServer` の起動待ちに使う。

## セキュリティ要点

- celeris の API トークン（`CELERIS_API_TOKEN_FILE`）はサーバ（BFF）だけが保持し、ブラウザには一切渡さない。
- 応答ヘッダとサーバログにトークンや平文パスワードが出ないことをテストで確認している。
- 未知の `Host` ヘッダは 400 で拒否する（`CELERIS_GUI_ALLOWED_HOSTS` で許可を追加できる）。
- 状態変更を伴う操作は CSRF 検査（`Origin` / `Sec-Fetch-Site` の同一オリジン確認。違えば 403）を経由し、セッションクッキーは `SameSite=Strict`。
- 全ページの応答に nonce 付きの `Content-Security-Policy` を付与する。
