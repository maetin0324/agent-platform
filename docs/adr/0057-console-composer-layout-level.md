# ADR-0057: Console 入力欄（composer）をレイアウトレベルへ（GUI のみ）

- 日付: 2026-09-22
- 状態: **Accepted**
- 関連: ADR-0055（スマホ UX、D1「固定要素」、Phase 91 で見つけた `.animate-fade-in` の containing-block バグ）、
  ADR-0048 D4（Console。`/` と `/org/:id` が同じ部品を使う）、ADR-0054 D2/D3（キュー・案件の文脈）

## 1. 文脈

Phase 91（ADR-0055 ラウンド 15）で、`~/root.tsx` の `.animate-fade-in`（各画面共通のページ遷移アニメーション、
`<Outlet/>` を包む）が、実行中の 0.25 秒間 Chromium の `position: fixed` な子孫の containing block を差し替える
実装依存の挙動を踏み、Console 入力欄（composer。当時は `Console` コンポーネントの一部として `<Outlet/>` の
内側でレンダーされていた）が一時的にタブバーの下に隠れるバグを引き起こしていた。CSS 側の対処（`transform`
→`translate`、`animation-fill-mode: both`→`backwards`）で恒久的な破損は消えたが、アニメーション再生中は
同じ現象が残ると記録した（`gui/docs/PROGRESS.md` Phase G42 の提案 P-G42-1）。構造的な解決は、composer を
`<Outlet/>` の外（`MobileTabBar` と同じ階層）に出し、そもそも `.animate-fade-in` の子孫にしないことだった。

## 2. 決定

### D1. composer を `~/root.tsx` に、React Context で登録する

- composer の実体（`~/components/ConsoleComposer.tsx`。旧 `~/components/Console.tsx::ConsoleInput`）を
  `~/root.tsx` に**モバイル版**として直接レンダーする（`<Outlet/>` の兄弟、`MobileTabBar` と同じ階層）。
  **デスクトップ版**は従来どおり `Console` コンポーネントの右カラム内（`<Outlet/>` の中）に残す — デスクトップは
  `position: static`（通常フロー）なので、そもそも containing-block バグの対象外であり、`<Outlet/>` の中に
  居ても実害が無い。見た目は Phase 91 まで通り不変。
- どのルートが composer を出すか（`/`・`/org/:id` だけ）と、送信先・状態（org/projects/streaming/返信先）を
  ルート側からレイアウトへ伝える仕組みは、**React Context**（`~/components/ConsoleComposerContext.tsx` の
  `ConsoleComposerProvider`/`useRegisterConsoleComposer`/`useConsoleComposerContext`）にした。
  `Console` コンポーネントが `useRegisterConsoleComposer({org, projects, streaming})` でレイアウトへ登録し、
  ブロックの「返信」も同じ Context の `replyTarget`/`setReplyTarget` を通す（`BlockStream` は `<Outlet/>` の中、
  モバイル版 composer は外にいるので、返信先はどちらか一方のローカル state では届かない）。
  - **代替案 A（React Router の `handle`）**: ルート定義に `handle: { hasConsoleComposer: true }` を付け、
    `useMatches()` で読む方式。「このページは composer を出す」の判定だけなら handle でも書けるが、
    composer が必要とする org/projects/streaming という**データ**まで運ぶには結局 loader 経由の
    `useRouteLoaderData` かレイアウトが独自にもう一度 celeris を呼ぶ必要があり、`Console`（`/`・`/org/:id`
    の両方から呼ばれる共有コンポーネント）が既に持っているデータをそのまま渡せる Context の方が単純だった。
  - **代替案 B（実 DOM ポータル、`ReactDOM.createPortal`）**: `Console` 側にモバイル用のポータル先 DOM
    ノードを置き、そこへ描画する案。ポータル**先**の DOM ノードが `<Outlet/>` の中にある限り、`position:
    fixed` の containing block は結局そのノードの祖先（`.animate-fade-in`）に左右されるため、根本解決に
    ならない。ポータル先を `<Outlet/>` の外（root）に置くなら、それは事実上 D1 と同じ配線を SSR 非対応な
    `ref`/`useEffect` 越しに行うだけで複雑さが増える（SSR 時は `ref` が無く、初回描画で composer が
    出ない・位置がずれるフラッシュが起きる）ため採らなかった。
- どの scope（誰宛てに送るか）・composer を出すパスかどうかの判定自体は、celeris への問い合わせを伴わない
  **純粋関数**（`~/lib/console-composer.ts::isConsoleComposerPathname`/`consoleComposerScopeForLocation`）に
  切り出した。`useLocation()` の pathname/search だけで決まり、SSR でもクライアントでも同じ結果になる
  （このリポジトリの「新しい判断ロジックを作らない」方針、ADR-0048 と同じ理由）。org/projects の**データ**
  だけが Context 経由の非同期な登録（`useEffect`）になる。

### D2. モバイル版とデスクトップ版は別インスタンス（状態は Context で共有、`text`/`mention` はローカル）

- `ConsoleComposer` は `variant: "mobile" | "desktop"` を受け、CSS クラス（`fixed ... lg:hidden` / `hidden
  ... lg:block`）だけで表示を切り替える。2 つの DOM インスタンスが同時に存在しうるが、`lg:` の境界で必ず
  排他的に 1 つだけが見える（既存の `Sidebar`/`MobileTopBar`、`NewConversationMenu` と同じ「両方レンダーし
  CSS で出し分ける」慣習に倣う）。
- 入力中のテキスト（`text`/`mention`）は各インスタンスのローカル state。返信先（`replyTarget`）・
  org/projects/streaming は Context で共有。モバイル版インスタンスは `key={pathname}` を持たせ、
  `/` ↔ `/org/:id` の遷移で強制的に作り直す（`~/root.tsx` 自体は画面遷移で unmount しないため、`key` が
  無いと下書きテキストが画面をまたいで残ってしまう。「ページを離れたら破棄」）。`?scope=` だけの変化
  （`/` 上でのスコープ切り替え）では `pathname` が変わらないので下書きは保たれる。デスクトップ版は
  `Console` ごと通常どおり画面遷移で unmount/remount されるので、同じ効果になる。
- `POST /console/instruct` の送信先（`action`）は `fetcher.submit(..., { action: pathname })` で明示する。
  モバイル版はルートの外（root 自身の route context）に居るため、既定の「最寄りのルートへ送る」に頼れない
  （root 自身は `action` を持たない）。

### D3. 副作用として見つかった潜在バグも合わせて直した

- Phase 92 の実装中、`mobile-audit.mjs` の `fixed-overlay` 検査（ADR-0055 D1-5）が、footer が composer
  （高さ約 170px、`position: fixed`）の下に隠れうる潜在バグを新たに検出した。これは Phase 91 以前から
  存在していたが、`.animate-fade-in` の containing-block バグにより `load` 直後は composer がビューポート
  基準で測れておらず（`fixed-overlay` の判定が「ビューポート下半分にある fixed 要素」だけを見るため、
  ずれた composer が対象から外れて）見かけ上合格していた（今回の構造修正でこの見せかけの合格が消えた）。
  `~/root.tsx` に `FooterComposerSpacer`（`~/components/ConsoleComposer.tsx` と同じ実測高さ、無ければ既定
  208px）を足して直した（`docs/PROGRESS.md`/`gui/docs/PROGRESS.md` の Phase 92 節に詳細）。

## 3. 影響

- `~/components/Console.tsx` から composer 実装が抜け、`~/components/ConsoleComposer.tsx`（新規）・
  `~/components/ConsoleComposerContext.tsx`（新規）・`~/lib/console-composer.ts`（新規、純粋関数）に分かれた。
- `mobile-audit.mjs` の `viewport-units` 検査（Phase 91 で追加した「アニメーション終了を 400ms 待ってから
  測る」版）に加えて、待ち無し（`load` 直後）の追加測定（`viewport-units-immediate`）を足した。両方とも
  0 件を維持する（構造的に直ったので待ちはもう理屈の上では不要だが、将来の再発検知のため両方残す）。
- デスクトップの見た目・DOM 構造は不変（`Console` の右カラム内、`position: static`）。
- 実機（`env(safe-area-inset-bottom)` を含む本当の意味での確認）は未確認のまま（ADR-0009 P-34）。

## Phase 102 追記（2026-09-22）

本番でホーム（`/`）から CoS に送信すると 405 になる不具合が見つかった。D2 の `fetcher.submit(...,
{ action: pathname })` が `pathname` をそのまま送信先にしていたが、`/` は home.tsx の**インデックスルート**
で、React Router では素の `action: "/"` はインデックスルート自身ではなく親（`root`。action 無し）に解決
される（`"/?index"` の形がインデックスルート自身を指す React Router の仕様）。`/org/:id` は非インデックス
なので影響しなかった。修正は「決め方は純粋関数」という D1 の方針どおり `~/lib/console-composer.ts` に
`consoleComposerActionFor(pathname)`（`/` → `/?index`、それ以外はそのまま）を足し、`ConsoleComposer.tsx`
の `submit()` から呼ぶ形にした（登録側が action を渡す代替案は採らなかった。判断ロジックを 1 か所
〈`console-composer.ts`〉に保つ既存の方針に揃えたため）。
