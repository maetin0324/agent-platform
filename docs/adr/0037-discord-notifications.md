# ADR-0037: 人の判断が要るときだけ、Discord に知らせる

- 日付: 2026-09-18
- 状態: **Accepted**（人間の依頼「案件が途中目標まで達成したか、途中で権限の認可が必要など人間の確認が必要だった場合
  通知を飛ばすようにしたい。Discord の webhook URL を GUI から登録したらそこにメッセージが飛ぶように」）
- 関連: SPEC §2.4 / §3.5（通知は数時間単位。悪い知らせは目立つ形で）/ §3.6（認可）/ §7（途中目標ごとに人が判定する）、
  ADR-0030（秘密は GUI から預かる。webhook URL も秘密）、ADR-0034 D6（GUI のブラウザ通知 `notify_now`）、ADR-0033 D5（認可）

## 1. 文脈

今日、案件の 2 つの調査が終わり、次の仕事「候補テーマの統合と選定」は**人の Go 待ち**の `draft` で止まった。
人はブラウザを開いていなければそれを知らない。GUI のブラウザ通知（ADR-0034 D6）は「開いているタブ」にしか届かない。
SPEC の言う「複数の案件を抱えて楽になる」には、**人の手が要る瞬間だけ**手元に届く経路が要る。

### 1.1 Phase 40 の追記（実機、2026-09-18 07:50）

Phase 39 を配備して webhook を登録した直後の最初の走査で、**`bad_news` 8 件（今日の失敗の履歴。既に GUI で
見て対応済み）が同じ秒に一斉送信され、5 件が届き 3 件が HTTP 429** になった。一方、**人が本当に知りたかった
`milestone_ready`（統合・選定の途中目標が Go 待ちの draft で止まっていること）は 1 件も作られなかった**:
その途中目標には done の調査 2 件のほかに `draft`（人の Go 待ち）の統合・PoC があり、旧い D1 の
「属する仕事がすべて終端」が偽になっていたため。**Go 待ちの draft がある状態こそ「人の判断が要る」瞬間
なのに、その定義では一生鳴らない。** D1・D3 を以下のとおり改める（D5 として追記）。

## 2. 決定

### D1. 知らせるのは「人の判断が要る」ときだけ（5 種。決定的に判定）

| 種 | いつ | 文面の骨 |
|---|---|---|
| `milestone_ready`（**Phase 40 で改定**） | 途中目標に属する仕事（裏方を除く）のうち **ready / running / reviewing / blocked が 0 件**、**done が 1 件以上**（＝「動いているものが無く、人の手が要る」）。かつ途中目標が `reached` でない。**全部が終端である必要はない** — Go 待ちの `draft` が残っていてもよい（むしろそここそが人の判断が要る瞬間） | 「途中目標『…』: done N / failed M。Go 待ちの仕事が K 件（担当: …）。達成の判定と次の Go をお願いします」＋案件へのリンク |
| `approval_pending` | `approvals` に未決の行が**新しく**できた | 「認可の要求: <担当>『<質問の先頭 120 字>』」＋認可画面へのリンク |
| `question_blocked` | タスクが `blocked`（人への質問）になった（`approval_pending` と重複するものは 1 回だけ） | 「<担当> が質問で止まっています: …」 |
| `bad_news` | 秘書レベル（level 0）の `bad_news` 報告が**新しく**できた | 「悪い知らせ: <headline>」 |
| `secretary_reply` | 案件が `proposed` のまま秘書の最初の返事が付いた（案件の理解確認・方針・最初の途中目標の提案。人の返事待ち） | 「秘書から『<案件>』の方針の提案が届きました。返事をお願いします」 |

- **`result` / `progress` は知らせない**（SPEC §3.5 の「数時間単位」は GUI の報告の流れの仕事。Discord は判断待ちだけ）。
- 重複排除は **`notifications` 表**（migration 0008 / 0009）で: `(kind, key)` を 1 回だけ送る（`key` = milestone id
  ＋done の件数（Phase 40。下記 D5） / approval id / task id / report id / project id）。
  送れなかったら次の tick で再送（最大 3 回、以後は諦めて `failed` を記録。**429 はこの再送回数に数えない**）。

### D2. Webhook URL は秘密（ADR-0030 の仕組みをそのまま使う）

- URL は `[secrets]` の 1 件として GUI の「アカウント → API キー」から登録する（id は既定 `discord-webhook`）。
- `[notify] discord_webhook_secret = "discord-webhook"`（既定値。書かなくてよい）。秘密が無ければ**何も送らない**（エラーにしない）。
- 値はログ・応答に出さない。GUI には「設定済み / 未設定」と fingerprint だけ。

### D3. 送信は taskd の tick から、決定的に（**Phase 40 で間隔を改定**）

- `tick_loop` が `[notify] interval_secs`（既定 30）ごとに D1 の条件を DB から評価し、未送信のものを Discord に POST する
  （`{"content": "...", "username": "taskd", ...}`。埋め込みは使わず素の Markdown 1 通）。**LLM は関与しない**。
- HTTP は `reqwest`（rustls）を taskd に足す（ワークスペースに HTTP クライアントは無い。他の用途にも使える）。
  タイムアウト 10 秒、失敗は warn（URL は伏せる）。
- 文面にはリンクを入れる: `[notify] gui_base_url = "http://192.168.1.103:7700"`（任意。無ければリンク無し）。
- **1 tick に送るのは最大 1 通**（`notify::select_batch`）。`bad_news` が複数 pending なら、それらを 1 通に
  束ねる（「悪い知らせ N 件: …／…」。台帳の行は個別に決着する）。他の種は 1 通ずつ、最古のものから。
- **429（レート制限）は失敗ではない**: `Retry-After`（秒。ヘッダかボディの `retry_after`。無ければ既定 5 秒）
  を読み、その時間が経つまで次の送信を控える。**attempts には数えない**（台帳の行に触れない。次の tick で
  そのまま再挑戦する）。

### D5. backfill 禁止 と `milestone_ready` の再定義（Phase 40。実機 2026-09-18）

- **`milestone_ready` を除く 4 種**（`bad_news` / `approval_pending` / `question_blocked` /
  `secretary_reply`）は、**taskd の起動時刻より後に作られたもの**（`created_at >= 起動時刻`）だけを判定の
  対象にする。起動前の出来事は走査対象外で、台帳に行を作らない（「未設定のため送っていません」の行も
  作らない）。`milestone_ready` は状態の判定なので起動時刻を見ない（起動直後に 1 回評価してよい）。
- `milestone_ready` は D1 の表のとおり改める: 「動いているものが無く、人の手が要る」
  （ready/running/reviewing/blocked が 0 件、done が 1 件以上）で鳴る。`key` は
  `<途中目標 id>:<done の件数>` — Go を出して仕事が進み、また止まれば done の件数が変わるので再び鳴る。

### D6. GUI がリンクを作るための `project_id`（Phase 40。GUI 依頼 G13i-P1）

- GUI には途中目標単体を引く API が無く、`milestone_ready` の `key`（途中目標 id）だけでは案件への
  リンクを作れない。`notifications` に `project_id TEXT NULL`（migration 0009）を持たせ、判定
  （`taskd::notify::scan`）が候補を作った時点で分かっている案件 id をそのまま台帳に書く
  （応答時に途中目標から逆引きしない。安い方: 書き込み時に 1 回決めるだけで済む）。
  `milestone_ready` はその途中目標の案件、`secretary_reply` はその案件自身、他の種は `NULL`。
- `GET /notify` の `recent[]` に `project_id`（省略 = null）を追加。GUI はこれで案件へのリンクを作る。

### D4. API と GUI

- `GET /notify` → `{configured: bool, secret_id, fingerprint?, gui_base_url?, recent: [{kind, key, project_id?, sent_at, ok, error?}]}`（読み取り）。
- `POST /notify/test` → その場でテスト送信（管理系）。200 `{ok, detail}`。
- GUI: 「報告」画面の通知の節に **Discord** の区画 — 設定済み / 未設定（未設定なら API キー画面への導線、id `discord-webhook` を案内）、
  「テスト送信」、直近の送信 10 件。ブラウザ通知の節はそのまま。

## 3. 採らない

- Slack / メール等の複数経路の抽象化。まず Discord 1 本。経路が増えたら `notifiers` に一般化する。
- 通知の文面を LLM に書かせる。決定的な定型文にリンクを付けるだけで足りる（人は GUI で中身を読む）。
- 「結果が出た」ことを逐一知らせる。SPEC の数時間単位の流れに反する。

## 4. 受け入れ条件（Phase 39 / G13i）

1. `notifications` 表と 5 種の判定が決定的に働く（偽の送信先で: 各条件で 1 回だけ送る、再送は 3 回まで、秘密が無ければ送らない）。
2. `POST /notify/test` が管理系で 401 / 409（未設定）/ 200。`GET /notify` に URL が出ない。
3. GUI の Discord 区画（設定・テスト・直近）。
4. **実機**: 人間が GUI から webhook URL を登録 → テスト送信が Discord に届く → 本番の案件で `milestone_ready` が届く。
5. `cargo test --workspace` / clippy / GUI 一式。

## 5. 受け入れ条件（Phase 40。D5/D6）

1. `milestone_ready` が新しい定義（D5）で決定的に働く: done が進みつつ draft が残る状態で鳴る、
   ready が残っていれば鳴らない、Go の後に再び止まれば別の key で再び鳴る、`reached` なら鳴らない。
2. `milestone_ready` を除く 4 種は taskd の起動前の出来事を対象にしない（行を作らない）。
3. 1 tick に最大 1 通、`bad_news` の束ね、429 は attempts に数えない（`Retry-After` の間は送らない）。
4. `GET /notify` の `recent[]` に `project_id` が乗る（`milestone_ready` はその途中目標の案件、
   `secretary_reply` はその案件自身、他は省略）。
5. `cargo test --workspace` / clippy。
