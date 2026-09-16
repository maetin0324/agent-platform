# ADR-GUI-0011: 画面デザインの刷新（デザイントークン・共通 UI 部品・サイドバーのレイアウト）

- 日付: 2026-09-16
- 状態: **Accepted**（人間の依頼「taskd-gui の画面デザインが簡素すぎるので現代のウェブサイトのデザインくらいリッチにして下さい」）
- 関連: docs/DESIGN.md §6.2（ルート構成）, §8.2（CSP）/ docs/adr/0002（Tailwind 4 + shadcn/ui 方針）/ docs/adr/0008（a11y・CSP）

## 1. 文脈

G0〜G7 は機能と受け入れ条件を優先し、見た目は Tailwind の素のユーティリティ（`border`・`text-gray-*`）で最小限に作ってきた。
結果として、全画面が「枠線 + 素のテキスト + 素のボタン」になり、状態（status）の区別・情報の階層・操作の主従が読み取りにくい。
今回は **機能・API・ルート・data-testid を変えずに**、見た目だけを作り直す。

## 2. 決定

### D1. 色はセマンティックなデザイントークン（CSS 変数）で持ち、ライト / ダークを `prefers-color-scheme` で切り替える

`app/app.css` の `:root` に `--bg` / `--surface` / `--fg` / `--primary` / `--success` … を定義し、Tailwind 4 の `@theme inline` で
`bg-surface`・`text-fg-muted`・`border-border` 等のユーティリティにする。画面側は `gray-*`・`amber-50` 等の生の色を使わず、トークンだけを使う
（ダークモードで壊れないため）。切り替えボタンは作らない（状態の保存に JS とストレージが要り、CSP の nonce 付きインラインスクリプトが増えるため。OS の設定に従う）。
コントラストは本文・補助文字とも WCAG AA（4.5:1）以上になる値を選ぶ（G5 の axe 検査で serious 0 件を維持）。

### D2. 共通 UI 部品を `app/components/ui/` にソースとして置く（新しい依存は足さない）

shadcn/ui の考え方（部品をソースで持つ、`cn` で上書き可能）に倣い、`Button`（`buttonClass`）・`Card`・`Badge` / `StatusBadge`・`PageHeader`・
`EmptyState`・`Alert`・`StatCard`・フォーム部品のクラス（`inputClass` 等）・`Icon` を手書きする。`@base-ui/react` や `lucide-react` は入れない
（今回の範囲では振る舞いを持つ部品が要らず、依存と cooldown 確認を増やさないため）。アイコンは 24px グリッドのストローク SVG を `Icon.tsx` に直書きし、
常に `aria-hidden` にする（リンク・ボタンのアクセシブルな名前は従来の文字列のまま）。

### D3. レイアウトは左サイドバー + 本文。ナビゲーションは 1 つの `<nav>` をレスポンシブにする

`lg` 以上では固定の左サイドバー（ブランド・グループ分けしたナビ・接続状態）、未満では上部の横スクロールのバーになる。
**同じリンクを 2 つ描かない**（data-testid の重複で e2e の strict mode が壊れるため）。現在地は `useLocation` から `aria-current="page"` を付けて強調する。
リンクは従来どおり `<a href>`（ナビゲーションの挙動は変えない）。ログイン画面は従来どおり `nav` を出さない。

### D4. 状態（status）は色 + ドット付きのバッジで統一する。役割（role）は色分けしない

`draft`=neutral、`ready`=info、`running`=primary（ドットが脈動）、`blocked`=warning、`reviewing`=teal、`done`=success、`failed`=danger、
`cancelled`=neutral（淡色）。文字列（status 名）はバッジの中にそのまま残す（色だけに頼らない。テストの表示文字列も変わらない）。
役割は DESIGN §10 Phase G7 の「色分けはせず、テキストのラベル」に従い、全役割共通の中立な枠付きラベルにする。DAG のノード色も同じトークンに揃える。

### D5. 変えないもの

ルート・loader / action・API 呼び出し・`data-testid`・表示文字列（見出し・ボタン名・件数の書式）・フォームの name / value・見出しのレベル（h1 / h2）・
`role="alert"` / `role="status"` 等の意味付け。フォントは外部から読まない（CSP）ので、日本語向けのシステムフォントをフォールバックに並べるだけにする。
