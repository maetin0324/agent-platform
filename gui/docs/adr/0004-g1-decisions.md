# ADR-GUI-0004: Phase G1（読み取りとストリーム）で確定した細部

- 日付: 2026-09-15
- 状態: **Accepted**（Phase G1 の実装で確定。docs/DESIGN.md §4.1〜§4.3, §6.2〜§6.4, §10 Phase G1 の範囲内の細部）
- 関連: docs/DESIGN.md §4, §6, §10 Phase G1 / docs/taskd-api-v1.md §2, §3.2〜§3.6, §4 / docs/adr/0003

## 1. 文脈

G1 は「読み取りとストリーム」（light）。受信箱・一覧・詳細の画面と SSE 中継を作る。G0 で確定した規約（`TaskdClient`、`TaskdUnavailable`
の扱い、middleware、mock-taskd）に乗せつつ、次の細部を決める必要があった。

## 2. 決定

### D1. `GET /inbox` は root と `inbox`（`/`）ルートの両方が独立に呼ぶ

docs/DESIGN.md §6.2 のルート表は `root` と `inbox` の両方に `GET /inbox` を挙げている。React Router framework mode の通常の流儀（各ルートの loader が
並列に独立して自分の必要なデータを取る）に従い、**共有はしない**。root は `counts` だけをタイトルバーの承認待ちバッジに使い、`inbox`（`/`）は
`Inbox` の全項目を描画する。taskd の `/inbox` はメモリ上の SQLite 読み取りで軽いため、二重取得のコストは無視できる。理由: ルート間でローダーの
結果を共有する仕組み（コンテキスト受け渡し等）を新設すると G0 の「middleware だけを共有する」規約から外れ、テストも複雑になる。

### D2. SSE クライアントは「純粋なデバウンス制御」と「React フック」に分離する（`app/hooks/useTaskdStream.ts`）

- `createStreamController(revalidate, debounceMs)`: `EventSource` に依存しないプレーン関数。**タイマーが無いときだけ** `notify(eventType)` で
  `debounceMs`（既定 250ms、docs/DESIGN.md §6.3）後に 1 回 `revalidate()` を呼ぶタイマーを開始する（`notify` のたびにリセットする素朴な trailing debounce
  にはしない）。`dispose()` でタイマーを止める。Vitest の `vi.useFakeTimers()` で jsdom 無しにテストできる（G0 の未解決事項「`@testing-library/react` は
  G1 で入れる」を回避: フックの中身を jsdom 無しでテスト可能な形に切り出したので、G1 では `@testing-library/react` を追加しない。追加は実際に DOM 描画の
  テストが要る G2 以降に先送りする）。
  **実機で見つけた不具合と修正**: 最初の実装は「`notify` のたびタイマーをリセットする」素朴な trailing debounce だった。`daemon` イベントは tick ごと
  （fixture の `tick_ms = 200ms`）に届き続けるため、`debounceMs`（250ms）より短い間隔でタイマーが永久にリセットされ、taskd が動き続ける限り
  `revalidate` が一度も呼ばれない（= 画面が更新されない）ライブロックになっていた。Playwright で `/tasks` を開いたまま `taskctl add` しても SSE 経由の
  反映が起きないという形で発現し、`EventSource.addEventListener` を差し替えて `notify` の呼ばれ方を追跡して原因を特定した。修正は上記のとおり
  「タイマーが無ければ開始する」方式（スロットル）にし、`debounceMs` ごとに高々 1 回・かつ必ず発火するようにした。回帰テストを
  `test/unit/useTaskdStream.test.ts` に追加（continuous な `notify` でも一定間隔で `revalidate` が呼ばれ続けることを検証）。
- `useTaskdStream({taskId})`: `useEffect` で `new EventSource("/events" + (taskId ? "?task_id=" + taskId : ""))` を開き、`task.event` / `daemon` / `reset`
  リスナーで `controller.notify(...)` を呼ぶ。`useRevalidator().revalidate` を渡す。アンマウントで `close()` + `dispose()`。root で 1 回だけ呼ぶ
  （docs/DESIGN.md §6.3 の 3）。
- 接続断の "接続が切れました" 表示（DESIGN §6.3 の 3 後半）は G1 の受け入れ条件に無いため実装しない（未解決事項に記載、G3/G4 で必要なら追加）。

### D3. `/tasks` の仮想スクロールは `@tanstack/react-virtual`（新規依存、3.14.11、pnpm cooldown 7 日を満たす最新）

- 2026-09-15 時点で `minimumReleaseAge` 10080 分（7 日）を満たす最新は 3.14.11（2026-09-07 公開。3.14.12/3.14.13 は直近すぎる）。
- 「さらに読む」ボタンで次ページを取得しクライアント側の配列に追加、`useVirtualizer` で描画する行だけ DOM に出す。
- **蓄積の破棄は「1 ページ目の内容が変わったとき」だけ**（`searchParams` の変化ではなく、loader が返す 1 ページ目の id 列 + `next_cursor` を
  文字列キーにして比較する。`app/routes/tasks.tsx` の `firstPageKey`）。当初は「`searchParams` の変化を見てリセット」で設計したが、
  root の SSE（D2）が taskd が動き続ける限り定期的に再検証を起こすため、loader の戻り値（`taskList`）の**参照**は同じ URL でも
  常に変わる。`searchParams`（＝ URL）ではなく**内容**をキーにしないと、蓄積したページが定期的に消えてしまう不具合になった
  （実機で確認。§D6 のライブロックと合わせて、常時接続の SSE と結びつく画面はどれも「参照の変化」ではなく「内容の変化」で
  再描画の要否を判断する必要がある）。「さらに読む」の `fetcher.data` 側にも同じ理由で同種のガード（`lastAppliedFetcherKey`）を置く
  （SSE の再検証は route の loader だけでなく読み込み済みの `fetcher` も再取得するため、同じページの二重追記を防ぐ）。

### D4. `/tasks/:id` は G1 では `TaskDetail` と `events` のタイムラインだけを描画し、生ログ・成果物本体・DAG は出さない（G3 の範囲）

`GET /tasks/{id}/artifacts` は G1 では呼ばない（一覧に出す成果物ビューアが G3 のため、中途半端な一覧だけを先に作らない。CLAUDE.md
「今回のフェーズだけをやる」）。`runs[].files` の有無だけは `TaskDetail.runs[]` に含まれるのでバッジ表示に使ってよい（本体リンクは G3 で有効化）。

### D5. `scripts/taskd.sh fixture basic` の構成

`.run/basic/` に対して `taskctl add` / `taskctl plan` を直列に呼び、`taskd --config .run/basic/taskd.toml --until-idle` で確定させる。
fake ワーカーはタスクごとに異なる応答を返す必要があるため、`test/taskd/fake-worker.sh`（既定）はそのままにし、`.run/basic/fake-worker.sh` を
シナリオ専用のスクリプト（タスクの `objective` またはワークスペース名で分岐）に差し替える。詳細は `scripts/taskd.sh` の `cmd_fixture` と
`test/taskd/fixtures/basic-worker.sh` のコメントに書く。

### D6. 子ルートは taskd のエラーを「素の Error」ではなく `Response` として投げる（auditor 指摘、実機バグの修正）

- **見つかった不具合**: `app/routes/tasks.tsx` / `tasks.$id.tsx` は当初、`TaskdUnavailable` / `TaskdError` をそのまま `throw` し、
  root や自身の `ErrorBoundary` で `isTaskdUnavailable` / `instanceof TaskdError` を見て振り分ける設計だった（docs/adr/0003 D4 の元の想定）。
  ところが React Router の**本番ビルド**は、loader が投げた「素の Error（`Response` ではない値）」を `ErrorBoundary` に渡す前に
  `Error("Unexpected Server Error")` へサニタイズし、実際の HTTP 応答も 500 になる（内部エラーの詳細を漏らさないための既定動作）。
  この結果 `isTaskdUnavailable(error)` 等の分岐は本番では**絶対に真にならない死にコード**になっており、taskd 停止中の `/tasks` や
  `/tasks/:id` は「500 + 汎用エラー画面」になっていた（`/` だけは inbox.tsx が loader 内で catch して例外を投げないため無事だった。
  G1 の受け入れ条件はどれも `/` しか taskd 停止時の表示を検証しておらず、`pnpm e2e` は当初これを検出できなかった）。
- **修正**: `app/taskd/errors.ts` に `taskdErrorResponse(e)` を追加し、`TaskdUnavailable` / `TaskdError` を `Response`（JSON body に
  `{kind: "unavailable" | "taskd_error", ...}`、`status` は taskd 側のもの、`unavailable` は 503）に変換する。`Response` を投げた場合は
  サニタイズされず、`isRouteErrorResponse(error)` と `error.data` でそのまま判別できる（React Router がこの経路だけは意図的な
  エラー応答とみなすため）。`app/routes/tasks.tsx` / `tasks.$id.tsx` の loader、`app/routes/inbox.tsx` の非 unavailable 分岐は
  `throw taskdErrorResponse(e)` に、`app/root.tsx` と `tasks.$id.tsx` の `ErrorBoundary` は `isRouteErrorResponse` + `error.data.kind` で
  判別するように書き直した。`app/routes/events.ts`（resource route、コンポーメントが無い）は `Response` を投げるのではなく
  `taskdErrorResponse(e)` をそのまま**返す**（taskd の 503 `too_many_streams` 等が同じ status で中継されるようにするため。
  DESIGN §6.4「taskd 側の 503 `too_many_streams` は同じ status で返す」）。
- **回帰テスト**: `e2e/g1.spec.ts` に「taskd 停止中の `/tasks` が 500 にならずバナーを出す」「存在しない id の `/tasks/:id` が 404 で
  『タスクが見つかりません』になる」を追加。`test/unit/events.route.test.ts` に taskd の 503 とその接続不可を `relayEvents` がそれぞれ
  503 として返すことを追加。

## 3. 影響

- G2 以降の操作系フックも `createStreamController` をそのまま再利用する。
- `@testing-library/react` の導入は G2 以降、実際に DOM 操作のアサーションが要るタイミングまで延期する。
