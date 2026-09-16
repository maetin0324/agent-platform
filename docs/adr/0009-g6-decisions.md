# ADR-GUI-0009: Phase G6（使い方ページ）で確定した細部

- 日付: 2026-09-15
- 状態: **Accepted**（Phase G6 の実装で確定。docs/DESIGN.md §10 Phase G6 の範囲内の細部）
- 関連: docs/DESIGN.md §10 Phase G6 / docs/taskd-api-v1.md §3.4, §3.10〜§3.13, §5.4 / docs/adr/0007, 0008

## 1. 文脈

G6 は「使い方ページ」（light）。`/help` に taskd の概念（状態・受け入れ条件・承認・Plan・プロバイダ・run）と GUI の操作を結びつけた説明を置く。
仕様に無い機能は書かない。設計判断を要した点を記録する。

## 2. 決定

### D1. `/help` は loader を持たない静的コンポーネント

内容は `docs/taskd-api-v1.md` / `docs/DESIGN.md` の範囲だけで、taskd の現在の値には依存しない（状態一覧・受け入れ条件の型・用語はどれも
仕様上固定）。taskd に問い合わせる必要が無いので、他の画面と違い loader を置かない。taskd 停止中でも `/help` は開ける。

### D2. 「画面ごとの説明」（`#screens`）が対象にする画面は DESIGN §10 Phase G6 が列挙する 6 つだけ

受信箱 (`/`) / 一覧 (`/tasks`) / 詳細 (`/tasks/:id`) / DAG (`/graph`) / プロバイダ (`/providers`) / デーモン (`/daemon`)。
「各画面の見出しの隣に `/help#<id>` へのリンク（`?`）を置く」も同じ 6 画面に限る。`/tasks/new` と `/plans/new` はフォーム画面で
「画面ごとの説明」の対象に無いため `?` リンクは置かない。全ての `?` リンクは `/help#screens` を指す（画面固有のアンカーではなく、
「画面ごとの説明」節がどの画面についても書いているため）。共有コンポーネント `app/components/HelpLink.tsx` にまとめた。

`app/routes/graph.tsx` の default コンポーネントにはこれまで `h1` が無かった（`ErrorBoundary` 側にのみ存在）ので、G6 で
`<h1>DAG</h1>` を新設した。`app/routes/inbox.tsx` も同様に `h1` が無かったので新設した（後述 D3 とも関係）。

### D3. 受信箱の「空のとき」の導線は 4 区画（承認待ち・質問・draft・注意）の件数が全てゼロのときに出す

DESIGN の文言「受信箱が空のとき（初回起動時）」は、`InboxCounts` の 4 件数がゼロという状態を指すと読む（`by_status` 全体がゼロという
より厳しい「本当に何も無い」条件にはしない）。理由: `by_status` を使うと、進行中のタスクが多数あるが人間の対応が不要な状態（受信箱としては
「空」に見える）で導線が出ない。「対応が要る項目が無い」ことを知らせる導線として、4 区画の合計で判定する方が実態に合う。

### D4. `docs/taskd-requests.md に書く場面`（`#trouble`）は開発者向けの記述のまま残す

DESIGN §10 Phase G6 の実装項目に明記されている（「困ったとき」の 6 項目の 1 つ）。エンドユーザー向けページに開発者向け情報が混じるが、
仕様の記述どおりに実装する（GUI 側で機能を足し引きしない）。リンクにはしない（`docs/taskd-requests.md` は GUI のルートではなく、
受け入れ条件 3「`/help` 内のリンクが全て 200」に抵触するため）。

### D5. taskd のスキーマ更新（ADR-0016 役割と委譲、ADR-0017 プロバイダ管理、ADR-0018 クラスタ）を型生成だけ取り込んだ

`docs/taskd-api-v1.md` は既に Phase 10（役割と委譲）の差分を反映済みで、taskd の JSON Schema はそれ以降（Phase 11 プロバイダ管理・
Phase 12 クラスタ）まで進んでいた。CLAUDE.md の共通完了条件「`pnpm gen:types` の差分ゼロ」を満たすため型は再生成して取り込むが、
G7（クラスタと委譲の表示）の画面機能は実装しない（フェーズの先回りをしない）。型の変更で `pnpm typecheck` が壊れた箇所だけ最小限直した:

- `app/taskd/types.ts` の `AttentionItem` に `cluster_unavailable`（`task` を持たない）が増えたため、`app/routes/inbox.tsx` の注意区画で
  `item.type === "cluster_unavailable"` を先に分岐させ、`task` 前提のリンク・取り消しフォームを出さない別枝にした（`/clusters` への遷移や
  専用の見た目は G7 の受け入れ条件そのものなので実装しない。ここでは崩れずに文字列だけ出す）。`e2e/g4.spec.ts` の unroutable フィクスチャの
  検索も同じ理由で型を絞ってから `.task` を読むよう直した。
- `TaskDetail.delegated`（必須フィールド）が増えたため、`test/unit/tasks.detail.loader.test.ts` の手書きフィクスチャと
  `test/fixtures/api/task-detail.json`（`scripts/capture-fixtures.sh` の再採取、実 taskd の応答をそのまま使う）に `delegated: []` を足した。

### D6. `server.js` の `pnpm typecheck` 失敗を修正した（G6 の変更とは無関係の既存バグ）

`res.writeHead` を差し替える箇所（Express 層の既定ヘッダ付与、ADR-0008 D15）で `writeHead(...args)` が TypeScript 7 のオーバーロード解決に
引っかかり型エラーになっていた（G6 の変更前から存在。`server.js` に手を入れていないのに `pnpm typecheck` が失敗したことで発覚）。
`res.writeHead.bind(res)` を保持するローカル変数 `writeHead` に `/** @type {(...args: any[]) => any} */` の JSDoc を付け、呼び出し側の型だけを
緩めて解消した（ロジックは 1 行も変えていない）。
