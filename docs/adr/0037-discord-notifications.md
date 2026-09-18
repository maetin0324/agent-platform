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

## 2. 決定

### D1. 知らせるのは「人の判断が要る」ときだけ（5 種。決定的に判定）

| 種 | いつ | 文面の骨 |
|---|---|---|
| `milestone_ready` | 途中目標に属する仕事（裏方を除く）が**すべて終端**（done / failed / cancelled）になった。かつ途中目標が `reached` でない | 「途中目標『…』の仕事が終わりました（done N / failed M）。達成の判定と次の Go をお願いします」＋案件へのリンク |
| `approval_pending` | `approvals` に未決の行が**新しく**できた | 「認可の要求: <担当>『<質問の先頭 120 字>』」＋認可画面へのリンク |
| `question_blocked` | タスクが `blocked`（人への質問）になった（`approval_pending` と重複するものは 1 回だけ） | 「<担当> が質問で止まっています: …」 |
| `bad_news` | 秘書レベル（level 0）の `bad_news` 報告が**新しく**できた | 「悪い知らせ: <headline>」 |
| `secretary_reply` | 案件が `proposed` のまま秘書の最初の返事が付いた（案件の理解確認・方針・最初の途中目標の提案。人の返事待ち） | 「秘書から『<案件>』の方針の提案が届きました。返事をお願いします」 |

- **`result` / `progress` は知らせない**（SPEC §3.5 の「数時間単位」は GUI の報告の流れの仕事。Discord は判断待ちだけ）。
- 重複排除は **`notifications` 表**（migration 0008）で: `(kind, key)` を 1 回だけ送る（`key` = milestone id / approval id / task id / report id / project id）。
  送れなかったら次の tick で再送（最大 3 回、以後は諦めて `failed` を記録）。

### D2. Webhook URL は秘密（ADR-0030 の仕組みをそのまま使う）

- URL は `[secrets]` の 1 件として GUI の「アカウント → API キー」から登録する（id は既定 `discord-webhook`）。
- `[notify] discord_webhook_secret = "discord-webhook"`（既定値。書かなくてよい）。秘密が無ければ**何も送らない**（エラーにしない）。
- 値はログ・応答に出さない。GUI には「設定済み / 未設定」と fingerprint だけ。

### D3. 送信は taskd の tick から、決定的に

- `tick_loop` が `[notify] interval_secs`（既定 30）ごとに D1 の条件を DB から評価し、未送信のものを Discord に POST する
  （`{"content": "...", "username": "taskd", ...}`。埋め込みは使わず素の Markdown 1 通）。**LLM は関与しない**。
- HTTP は `reqwest`（rustls）を taskd に足す（ワークスペースに HTTP クライアントは無い。他の用途にも使える）。
  タイムアウト 10 秒、失敗は warn（URL は伏せる）。
- 文面にはリンクを入れる: `[notify] gui_base_url = "http://192.168.1.103:7700"`（任意。無ければリンク無し）。

### D4. API と GUI

- `GET /notify` → `{configured: bool, secret_id, fingerprint?, gui_base_url?, recent: [{kind, key, sent_at, ok, error?}]}`（読み取り）。
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
