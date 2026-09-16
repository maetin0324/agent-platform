# ADR-GUI-0005: Phase G2（操作）で確定した細部

- 日付: 2026-09-15
- 状態: **Accepted**（Phase G2 の実装で確定。docs/DESIGN.md §4.1, §4.3「操作」, §4.4, §4.6, §6.3 の 2, §6.6, §8.2 CSRF, §10 Phase G2 の範囲内の細部）
- 関連: docs/DESIGN.md §4, §6, §8, §10 Phase G2 / docs/taskd-api-v1.md §1.2, §1.4, §1.5, §3.4, §3.10〜§3.15, §5.4, §5.7 / docs/adr/0003, 0004

## 1. 文脈

G2 は「操作」（strong）。詳細と受信箱の action（approve / reject / answer / cancel）、作成フォーム（`POST /tasks`）、Plan フォーム（`POST /plans`）、
replay（`POST /replay`）、CSRF、flash、409 / 422 の表示を作る。G0 / G1 で確定した規約（`TaskdClient`、loader は taskd のエラーを `Response` として投げる、
middleware は root に集約、mock-taskd で単体テスト、実 taskd で e2e）に乗せつつ、次の細部を決める必要があった。

## 2. 決定

### D1. CSRF 検査は root middleware（`csrfCheck`）で行い、action では再検査しない

- `app/middleware/security.server.ts` に `csrfViolation(request): string | null`（純粋関数）と `csrfCheck`（違反なら 403 `text/plain` の `Response` を投げる）を置き、
  `app/root.tsx` の `middleware` を `[hostCheck, csrfCheck, securityHeaders]` の順にする。React Router の middleware は document request と `.data` request の両方に
  掛かるので、`<Form method="post">` の初回送信（document）もクライアント遷移後の送信（`.data`）も同じ検査を通る。
- 規則は docs/DESIGN.md §8.2 のとおり: GET / HEAD / OPTIONS 以外について、`Origin` があれば `new URL(request.url).origin`（受けた `Host` から組み立てた自分のオリジン）と
  大文字小文字を無視して一致、`Sec-Fetch-Site` があれば `same-origin` / `none`。どちらも無い要求（curl、Node の `fetch`）は通す。
  理由: ブラウザは同一オリジンのフォーム送信に必ず `Origin` を付ける（Chromium / Firefox / Safari とも POST では付く）ので、無い要求はブラウザ由来ではない。
  taskd 自身も `Origin` 付きの POST を 403 にするが、BFF は `Origin` を転送しない（§8.1）ので GUI 側で止めないと素通りする。
- 403 の本文は理由の 1 行（`forbidden: origin http://evil.example is not http://127.0.0.1:7700`）。ErrorBoundary は通らない（middleware が投げた `Response` は
  そのまま応答になる）。
- **実装で判明した点（Express 層への追加）**: React Router 8 は document request の変更系（curl の `POST /tasks/<id>` や JS 無効時のフォーム送信）に対して、
  root middleware より**前**に独自の Origin 検査（`throwIfPotentialCSRFAttack`）を行い、不一致なら **400 `Bad Request`** を返す（`.data` request には無い）。
  受け入れ条件 7 は 403 を要求するので、`server/app.ts`（React Router のハンドラの前）に `expressCsrfGuard` を置き、同じ純粋関数 `csrfViolation` で検査して 403 を返す。
  root middleware の `csrfCheck` は `.data` request（クライアント遷移後のフォーム送信）のために残す。検査規則は 1 か所（`csrfViolation`）で、入口が 2 つある形。
  docs/DESIGN.md §8.2「React Router の middleware で実装」からの逸脱なので、PROGRESS の「提案」G2-P1 に記載。

### D2. action の結果は「クッキーの flash」ではなく action の戻り値（`actionData`）で表示する

- docs/DESIGN.md §6.3 の 2「`TransitionResult` を flash に載せて loader を再検証」を、**セッションクッキーを使わずに**実現する。action は `data(outcome, {status})` を返し、
  React Router が同じ navigation の中で loader を再検証してから `actionData` と一緒に描画する。表示は `app/components/Flash.tsx` の `TransitionFlash` / `ErrorFlash` / `FieldErrors`。
  理由: (1) loopback で認証無しの G2 にはセッションが無く、flash のためだけにクッキー（署名鍵・`SameSite`）を導入すると G5 の認証設計を先回りしてしまう。
  (2) `actionData` は同じ画面に留まる操作（承認・回答・取り消し）にはそのまま使え、成功時に `redirect` する作成フォームは遷移先で結果（`draft` の詳細）が見えるので flash が要らない。
  (3) 再読込で消える（flash の本来の性質）。
- `TransitionOutcome`（`app/taskd/action-types.ts`）は `{ok: true, intent, taskId, result: TransitionResult} | {ok: false, intent, taskId, error: ActionError}`。
  失敗を例外にせず data にするのは、taskd の 409 / 422 は**正常系**（docs/taskd-api-v1.md §3.5「押した結果の 409 も正常系として扱う」）であり、ErrorBoundary に落とすと
  画面全体が入れ替わって元のフォームが消えるため。応答の HTTP status は taskd のもの（409 / 422 / 503）をそのまま使う（`data(outcome, {status})`）。
- `ActionError` は `Problem` を画面向けに整理した形: `conflict`（409 なら true。`conflict` と `invalid_transition` を区別しない —— どちらも「状態が変わりました」として
  再検証後の最新状態を見せるのが正しい対処で、ユーザがすることは同じ）、`fields`（422 の `errors[].field` → 文言。**文言は taskd のものをそのまま**）、`messages`。
  `TaskdUnavailable` は 503 `unavailable` にする（root のバナーも同時に出る）。
- **実装で判明した点（`shouldRevalidate`）**: React Router の既定は「action が 4xx / 5xx を返したら loader を再検証しない」。そのままだと 409 を受けた古いタブが
  古い `status` と押せない操作ボタンを表示し続け、docs/DESIGN.md §4.3「409 は『状態が変わりました』として再取得」に反する（受け入れ条件 2 の e2e で発覚）。
  `app/lib/revalidate.ts` の `revalidateAfterActionErrors`（`actionStatus >= 400` なら再検証、それ以外は既定）を root と action を持つ 3 ルート
  （`tasks.$id` / `inbox` / `daemon`）の `shouldRevalidate` に置く。
- **route 以外の export と `.server` モジュール**: React Router が server-only モジュールへの参照を消せるのは `loader` / `action` / `middleware` 等の route export だけで、
  テスト用に export した補助関数（`runTaskAction` 等）が `actions.server.ts` を参照するとクライアントバンドルの生成が失敗する（`pnpm build` で発覚）。
  action 本体は `app/taskd/route-actions.server.ts` に集め、ルートは `action` の中からだけ呼ぶ。純粋な `formString` は `app/taskd/forms.ts`（`.server` でない）に置く。

### D3. `expected_status` は常に描画時の `status` を hidden で送る

- 詳細（`/tasks/:id`）は `task.status`、受信箱は区画ごとの固定値（承認待ち `ready`、質問 `blocked`、draft `draft`、注意は `item.task.status`）。docs/DESIGN.md §4.1 / §4.3 のとおり。
- これにより「2 つのタブで同じ Approval を開いて片方で承認」は他方が 409 `conflict` になる（受け入れ条件 2）。SSE の再検証が先に走ればボタン自体が消えるので、
  409 が見えるのは再検証より先に押した場合だけ。e2e は `page.route("**/events", abort)` で片方のタブの SSE を止めて古い画面を再現する。

### D4. 受信箱の「この Plan の子を全部受け入れ」は子ごとに `POST /tasks/{id}/approve` を**直列**に呼び、原子性が無いことを UI に書く

- docs/DESIGN.md §4.1 のとおり。action は `task_id` を複数受け取り、1 件ずつ `applyTransition` して `TransitionOutcome[]` を返す。途中で失敗しても残りは続ける（1 件目が 409 でも
  2 件目は送る）。応答の status は全て成功なら 200、そうでなければ最初の失敗の status。並列にしないのは taskd の書き込みが SQLite の単一書き込みであり、並列にしても速くならず
  `db_busy` の可能性だけ増えるため。

### D5. フォームは `NewTaskSpec` / `NewPlanSpec` と 1:1 で、GUI 側の検証はしない（`required` 属性も付けない）

- docs/DESIGN.md §4.4「GUI 側の検証は『必須欄が空』程度」に対し、G2 では**それも付けない**（`required` を付けると taskd の 422 文言 `title must not be blank` /
  `at least one acceptance criterion is required (...)` が一度も見えず、受け入れ条件 5 の「422 の文言がそのまま表示」を確認できない）。
  空欄は本文から省いて taskd の既定を使う（`deny_unknown_fields` なので型に無いキーは入れない）。`title` / `objective` / `goal` は必須フィールドなので空でも `""` で送る。
- 受け入れ条件ビルダーは `criterion_type[]` / `criterion_value[]` の並び（`FormData.getAll`）で行を表す。値が空白だけの行は送らない（全行空なら `acceptance: []` → 422）。
  `command` は `expect_exit: 0` 固定（CLI と同じ。docs/taskd-api-v1.md §3.4）。
- `depends_on` は存在する候補（`GET /tasks?limit=500&order=created_desc`）のチェックボックスに加え、自由入力欄（`depends_on_extra`、空白 / カンマ区切り）を置く。
  存在しない id を指定して `dependency <id> does not exist` を確認するため（受け入れ条件 5）。候補一覧は G2 では素朴な全件（500 件上限）で、絞り込み UI は作らない。
- 作成成功は `redirect("/tasks/<id>")`（`draft`。`kind=approval` なら taskd が `ready` にする）。

### D6. デーモン画面は G2 では replay に必要な最小限だけ

- docs/DESIGN.md §10 で「デーモン画面」は G4 の項目。G2 の受け入れ条件 8（replay ボタン）を満たすために `/daemon` を作るが、内容は `GET /daemon` / `GET /config` の値を
  `<dl>` で素直に出すことと replay フォームに留め、経過時間・遅延判定（`3 × tick_ms`）・停止/復旧バナー・`awaiting_human` / `unroutable` と受信箱の照合は G4 で作る
  （CLAUDE.md「次のフェーズの準備を先回りしない」）。
- replay の結果は `taskctl replay` と同じ形の文字列（`<n> mismatches across <m> tasks`）で出す。503 `replay_in_progress` は `ErrorFlash`。

### D7. e2e の fixture は G2 でも `basic` を使い、足りないタスクは spec が taskctl / API で作る

- 受け入れ条件 4（後続を持つ `ready` の cancel）に必要な「後続を持ち、かつワーカーに拾われない `ready`」は、`draft` のまま置く依存先 → それに依存する `ready`（依存未完了なので
  ディスパッチされない）→ さらにそれに依存する `ready` の 3 件を spec の中で `taskctl add` / `approve` で作る。受け入れ条件 2 も専用の Approval（`kind=approval`）を
  `POST /tasks` で作る（fixture の唯一の Approval は条件 1 が消費するため）。`scripts/taskd.sh fixture basic` の中身は G1 から変えない（G1 の e2e と共有）。
- `e2e/g2.spec.ts` は `beforeAll` で `basic` を作り直して起動する（G1 の e2e が `sse probe` 等を追加しているため、既知の状態に戻す）。
- Playwright はテストが 1 件失敗するとワーカーを再起動して `beforeAll` を再実行する（= fixture が作り直される）ので、各テストは前のテストが作ったタスクに
  依存しない（受け入れ条件 7 は自前で draft を作る）。`e2e/g0.spec.ts` にも `beforeAll` を足し、`basic` が 7710 を掴んだままでも `dev` を起動できるようにした。
- 受け入れ条件 1 の「親が SSE 経由で done」の待ちは 60 秒（仕様に上限は無い）。e2e 中に taskd の tick が 10〜30 秒止まる現象を繰り返し観測したため
  （`docs/taskd-requests.md` R1、PROGRESS の G2-U1）。受け入れ条件 5 の 30 秒は仕様どおり。

## 3. 影響

- 新規依存: なし（`@testing-library/react` も入れない。フォームの DOM 検証は Playwright（実 taskd）で行い、単体テストは action / builder の純粋関数と mock-taskd に対する
  HTTP の形に限る）。
- `app/root.tsx` のナビゲーションに「新規タスク」「新規 Plan」「デーモン」を追加。
- G5 の認証（セッションクッキー）を入れても D2 は変わらない（flash はクッキーに依存しない）。
