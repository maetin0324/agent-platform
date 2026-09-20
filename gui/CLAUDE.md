# celeris-gui（このリポジトリの `gui/`）

celeris の Web GUI。React + Remix（= React Router 8 framework mode、SSR）の Node サーバが BFF として
celeris の HTTP API v1 を呼ぶ。SQLite には触らない。

celeris 本体はこのディレクトリの親（`..`、環境変数 `CELERIS_REPO` で上書き可）にある **同じ git リポジトリ**（ADR-0020）。
同居していても境界は変わらない: **GUI から celeris に入る依存は作らない**（Rust のコードを読み書きしない、`crates/` を import しない、
DB を開かない）。契約は `docs/celeris-api-v1.md` と `app/celeris/types.ts` だけ。

## 最初に読むもの（毎セッション）
1. `docs/DESIGN.md` — 設計。§6 アーキテクチャ、§8 セキュリティ、§10 のフェーズと受け入れ条件。ここに書かれた設計原則とフェーズ順は変更しない
2. `docs/celeris-api-v1.md` — celeris が提供する API v1 の仕様。**GUI から見た契約はこれと `app/celeris/types.ts` だけ**
3. `docs/PROGRESS.md` — どのフェーズまで終わっているか。ここが現在地
4. `docs/adr/` — 過去の設計判断（境界、フロントエンドスタック）。矛盾する変更をしない

## 作業の進め方
- 今回のフェーズだけをやる。次のフェーズの準備を先回りしない
- 設計判断をしたら `docs/adr/NNNN-*.md` を追加してから実装する（GUI 側の ADR は 0003 から）
- celeris は `scripts/celeris.sh build` でビルドし、`scripts/celeris.sh start <name>` で fake ワーカー + `[api]` の設定で起動する。結合テスト（`pnpm e2e`）はその実 celeris に対して行う
- 各フェーズ完了時に必ず:
  - `pnpm lint`、`pnpm typecheck`、`pnpm test`、`pnpm build`（G1 以降は `pnpm e2e` も）を実行し、出力の要点を報告に含める
  - `pnpm gen:types && git diff --exit-code app/celeris/types.ts` が差分ゼロであることを確認する
  - `docs/PROGRESS.md` を更新（完了日、証拠コマンドと結果、監査結果、未解決事項、提案、celeris への依頼）
  - `git add -A . && git commit -m "phase G<N>: <summary>"`（**`.` を付ける**。celeris 側の変更を巻き込まないため。ADR-0020 D3）
- 同じアプローチを 3 回失敗したら、`docs/PROGRESS.md` に状況を書き、報告本文で人間に質問する
- **celeris の API が足りない・仕様と違うと分かったら、GUI 側で回避しない。** `docs/celeris-requests.md` に「エンドポイント / 期待（`docs/celeris-api-v1.md` の節）/ 実際（`curl` の出力）/ できないこと」を書き、
  `docs/PROGRESS.md` に `## Phase G<N> — BLOCKED` を書いてコミットし、止まる
- 仕様が曖昧なところは celeris の実際の挙動に合わせて進めてよい。`docs/PROGRESS.md` の「提案」に「`docs/celeris-api-v1.md` §x をこう明確化すべき」と書く
- `package.json` の版は完全固定（`^` 無し）。新しい依存を足すときは `docs/PROGRESS.md` に理由を書く。RC / next / canary は使わない

## 禁止
- **SQLite を直接開かない**（`better-sqlite3` 等を入れない。`.sqlite3` ファイルを読まない）
- **celeris の crate に依存しない**（Rust のコードを書かない、`cargo` は celeris をビルドするためだけに使う）
- **celeris API の仕様外の挙動に頼らない**（文書に無いフィールド、`celerisctl` の文字出力の解析、派生値の再計算、判断ロジックの再実装）
- テストで外部ネットワークに出ない（例外は `pnpm install` と `pnpm exec playwright install chromium` の準備だけ）
- LLM を GUI の協調判断に使わない（GUI は表示と操作の中継だけ。何をいつ動かすかは celeris が決める）
- ブラウザから celeris を直接呼ぶコード（クライアント側で `CELERIS_API_URL` を参照する等）を書かない。トークンをブラウザに渡さない
- `dangerouslySetInnerHTML`、`eval`、外部 CDN の読み込み
- `docs/DESIGN.md` と `docs/celeris-api-v1.md` の書き換え（提案は `docs/PROGRESS.md` の「提案」節へ）

## 完了報告の書き方
受け入れ条件ごとに「条件」「実行したコマンド（または Playwright の操作）」「出力の要点（exit code、テスト数、表示された文字列、差分ゼロなど）」を箇条書きで本文に書く。
評価器はトランスクリプトしか見ないので、ファイルに書いただけでは完了と判定されない。
