# ADR-0055: スマホ（Nothing 2a / Chrome、393×851）で使える GUI — 機械検査で回し続ける

- 日付: 2026-09-21
- 状態: **Accepted**（人の指示: スマホから見ると html 要素の横幅が大きすぎて縦画面の横幅を飛び出し横スクロールできる部分がある、タスクの
  状態欄に done / running 以外の詳細な説明が入っている、などスマホの UX が悪い。Playwright で確認しつつ、デザイナーの skill も使って
  できるだけ直す。やることが無くなりにくいので最後に回し続ける）
- 関連: ADR-0048 D4（Console）、ADR-0054 D3、gui/CLAUDE.md（`dangerouslySetInnerHTML` 禁止など）

## 1. 決定

### D1. 機械検査（`gui/scripts/mobile-audit.mjs`。Playwright、Chrome の UA、393×851、`deviceScaleFactor 2.75`）

すべての画面（`/`、`/org`、`/org/<node>`、`/projects`、`/projects/<id>`（各タブ）、`/tasks/<id>`（各タブ）、`/board`、`/approvals`、
`/reports`、`/releases`、`/knowledge`、`/knowledge/inbox`、`/clusters`、`/accounts`、`/help`）を、モック（`gui/test/mock-celeris`）の
データで開き、次を検査して JSON レポートを出す。**1 件でも落ちたら CI（`pnpm mobile-audit`）が非 0**:

1. **横はみ出し**: `document.documentElement.scrollWidth <= 393`、かつ `getBoundingClientRect().right > 393` の要素が無い。
2. **タップ領域**: 操作できる要素（button / a / input / select）の最小サイズ 44×44（例外は同じ行の隣接リンク群で、`data-touch-ok` を付けたもの）。
3. **状態表示**: タスク・案件・途中目標の**状態バッジは 1 語**（`labels.ts` の状態ラベル。説明文は `title` 属性か開いたときの詳細に）。
4. **文字**: 本文 14px 以上、`overflow-wrap: anywhere` が長い id / パス / URL に効いている（`word-break` の検査は横はみ出しで代替）。
5. **固定要素**: 上部の帯と下部の入力欄が内容を隠さない（最後の要素の `bottom` が入力欄の `top` より上に出せる = スクロールで到達できる）。
6. **横スクロールが要る表**: 表は `overflow-x: auto` の箱に入れ、箱自体は 393 に収まる（1 と両立）。
7. スクリーンショットを `gui/test/mobile-audit/<route>.png` に残す（差分の目視用。git には入れない）。

### D2. 直し方の規律（デザインは `artifact-design` の骨（余白・階層・トークン）に倣う）

- 幅は常に流体（`max-w-full`、`min-w-0`、grid は 1 列に落とす）。固定幅（`w-[...px]`）は禁止（`min-w` も）。
- 状態は**バッジ 1 語 + 色**。理由・詳細は行の下か開閉に。
- id（ULID）・sha・パスは `font-mono text-xs break-all` で、表示は末尾 6〜12 字に省略して全文は `title`/コピー。
- 一覧はカード（縦積み）、表は最小限。ナビは下部固定のタブ（Console / ボード / 案件 / 認可 / その他）。
- 入力欄は画面下固定、キーボード表示時に隠れない（`100dvh` と `env(safe-area-inset-bottom)`）。
- 一度に 1 画面ずつ直し、監査が緑になったら次へ。**機能を落とさない**（既存の vitest・API 契約は変えない）。

### D3. ループ

- 監査 → 最も重い違反（横はみ出し > 状態欄 > タップ領域 > 文字）から 1 画面を直す → vitest / typecheck / build → 監査 → コミット → まとめて
  リリース・昇格（ライブ）。他の Phase（ADR-0053 / 0054）の実装が入る間はそれを優先し、空いた時間でこのループを回す。
- 監査のレポートは `docs/PROGRESS.md` にラウンドごとに 1 行（違反数の推移）で残す。

## 2. 受け入れ条件（Phase 69 以降、ラウンド制）

- Phase 69: `mobile-audit.mjs` と `pnpm mobile-audit`、初回レポート、横はみ出しが 0 になるまで。状態バッジの 1 語化。
- 以後のラウンド: 違反 0 を維持しつつ D2 の規律で画面を磨く（Console のチャット吹き出し、ボードのカード、タスク画面のタブ、案件画面）。
- 実機: 人がスマホで見て気になった画面を Console から伝えれば、その画面が次のラウンドの先頭になる。
