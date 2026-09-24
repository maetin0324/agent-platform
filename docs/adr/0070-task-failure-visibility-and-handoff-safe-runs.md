# ADR-0070: 失敗を必ず人に見せ、やり直し方を一通りにし、切替と DB 遅延で生きた run を捨てない

- 日付: 2026-09-24
- 状態: **Accepted**（人間の指摘: 「Celeris の自己改善タスクとして渡した直近 2 つのタスクがどちらも
  失敗しているのに何の通知も無く、リトライ方法も不明瞭」）
- 関連: ADR-0037（通知）/ ADR-0040 D4（ライブ切替と draining）/ ADR-0010 D2・D7（リース）/
  ADR-0054 Phase 113 追記（reviewer のインフラ失敗、D2/D3 のひな形）/ ADR-0051（rereview）

## 1. 文脈（本番で確認済み。再調査はしていない）

- タスク A 01M38J4X53P1Y684FS42Z6R0VZ: 01:58 running。02:11 に GUI からの昇格（promote）が起き、
  旧インスタンスの journal に「drain timeout; aborting the run」。03:27 に「lease expired;
  reclaimed」で 89 分の作業を握りつぶし、reclaim のたびに `attempts` を消費して 3 回目
  （`max_retries=2`）で `failed`。人には何も通知されなかった。
- タスク B 01M38FXZZVDNY2VQS2R2ZDWYX0: 成果は配送・本番昇格済み（release `51d24a61c2ba`）なのに、
  その後のレビューで `cargo test --workspace` が exit 101 → 再 run 2 回が
  「claude exited without result.json」で `failed`。「配送済みなのに failed」なのに人には通知なし。
- 同時間帯（DB がまだ NFS 上の loop デバイス）: 「failed to renew lease」×5、「could not update the
  instance role this tick」×26、「delivery tick failed: database is locked」×1。DB は後にローカル
  ディスクへ移設済みだが、**lease 更新の失敗で生きている run を捨てる設計は残っている**。
- GUI: 失敗したタスクに「やり直す」（`retry`）はあるが目立たず、原因の説明も無い。受信箱に失敗の
  種類・原因・配送状態が出ない。

## 2. 決定

### D1. 失敗の分類と通知

`failed` への遷移を**分類**する（純粋関数、`task_ops::derive::classify_task_failure`）:

- **`infra`**: lease 失効・切替（drain）による中断・result.json 不在・セッション再開拒否・レート制限
  （requeue 上限到達を含む）・DB busy。実装では、D3 の `Trigger::InfraRequeue` を使い切ったあとの
  最終 `WorkerFinished.outcome` に `"infra failure ×N: …"` という接頭辞を付ける（ADR-0054 D2 の
  reviewer 側 `"reviewer infra failure ×N"` と同じ書式）。プロバイダの requeue 上限到達
  （`"requeue limit (…) reached"`）も同じ理由でここに含める。
- **`work`**: レビュー不合格（直前の `Event::ReviewVerdict{pass:false}` の理由 1 行）・
  max_turns 超過やワーカーの明示的な `error`（上記の `infra` マーカーを持たない
  `WorkerFinished.outcome`）。

分類結果と理由 1 行は `TaskDetail.failure`（`GET /tasks/{id}`）と、受信箱の
`AttentionItem::Failed`（`class` / `reason`。既存の `reason` フィールドをそのまま使う）に出す。
配送済み（`deliveries` に `release` が付いた記録がある）タスクの失敗は
`delivered_release: Some("<sha12>")` を持たせ、GUI 側で「成果は配送済み（release <sha12>）だが
レビューで不合格」と組み立てる（文言の組み立ては GUI、判定は celeris）。

`failed` への遷移のたびに、celeris の通知走査（`celeris::notify::scan`）に
`scan_task_failed` を足し、新しい `NotificationKind::TaskFailed`（`"task_failed"`）の
`notifications` 行を作る（既存 6 種と同じ `(kind, key)` 重複排除。`key = task_id:updated_at` 相当の
遷移識別子）。既存の `AttentionItem::Failed`（受信箱の「注意」区画。Phase 9 から存在）は
そのまま使い、今回は分類・配送状態・操作を追加するだけで、新しい区画は作らない
（受信箱は既に「人間は承認待ちキューだけ見ればよい」の唯一の場所なので、二重の置き場を作らない）。

CoS の次の会話の冒頭コンテキスト（`Dispatcher::recent_work_of` / `recent_work_outcome`。
Phase 33 / ADR-0033 D4 追記）は、**既に** `failed` タスクの直近の理由（レビュー不合格の理由か
ワーカーのエラー）を 1 行ずつ載せている。この Phase では文言を変えない（既存のスナップショットを
壊さない）。「未処理の失敗を 1 行ずつ載せる」という D1 の要求は、この既存の仕組みで満たされていると
判断した（新規実装なし。未解決事項に記録する）。

### D2. やり直しの一本化

- `POST /tasks/{id}/retry`（`task_ops::retry::retry_task`）は**既に** `Status::Failed | Cancelled`
  から常に使え、複製した新タスクの `attempts` は常に `0`（"reset_attempts: true 既定" は既存の挙動
  そのもの。API に新しいパラメータは足さない）。`POST /tasks/{id}/rereview` も ADR-0054 Phase 113 D3
  で `failed`（直前が `review_fail` のときだけ）から使える。**この Phase の新規実装は GUI と
  `celerisctl retry` の追加だけ**（バックエンドの状態機械は変えない）。
- GUI のタスク詳細で `status == failed` のとき、画面上部に赤いバナー
  「失敗: `<class>` — `<reason>`」+「やり直す」（`retry`。常に表示）+「再レビュー」
  （`task.actions` に `rereview` があるときだけ）を出す。「取り下げ」は**状態遷移を増やさない**
  （`Failed` は終端のまま。`Trigger::Cancel` の許容状態は変えない）: 既存の
  `POST /tasks/{id}/comments` で「対応不要と判断し、取り下げました」という定型コメントを残すだけの
  軽い操作にする（D3 で参照した状態機械の変更コストと、そもそも「失敗を握りつぶす」操作を状態機械に
  持ち込みたくないという判断。D2 も同じ考え方を取っている）。
- `Action::Rereview`（新設、`task_ops::view::Action`）を `Failed` かつ
  `task_ops::comment::can_rereview(task, events)`（既存の `review_fail_is_the_only_reason_for_failure`
  等の検証をタスク一覧・受信箱からも使えるよう切り出した純粋関数）が真のときだけ立てる。
  受信箱の `AttentionItem::Failed.task`（`TaskRef`）にも同じ規則で `rereview` を足す。
- `docs/gui/help`（`GET /help`。GUI の `/help` 画面）と `celerisctl retry` / `rereview` / `cancel`
  の `--help` 文面を同じ言葉に揃える: 「やり直す（retry）= 新しいタスクを複製し attempts を
  0 に戻す。再レビュー（rereview）= 実装 run はやり直さず判定だけをやり直す（review 判定だけが
  原因で failed になったときだけ）。取り下げ（コメント）= 状態は変えず対応不要と記録するだけ」。
- `celerisctl retry <task_id> [--accept]`（新設。`cancel.rs`/`rereview.rs` と同じ最小限の形）。

### D3. インフラ失敗は attempts を消費しない

新設 `Trigger::InfraRequeue`（`task_core::transition`）: `Running → Ready`、attempts 据え置き、
reason `"infra_requeue"`。既存の `Trigger::Requeue`（供給側失敗、ADR-0010 D1 P-21）と同じ形だが、
**別のカウンタ**で数える（`task_ops::derive::consecutive_infra_requeues`。
`Event::Transitioned.reason == "infra_requeue"` を新しい順に、`"dispatch"` は読み飛ばして数える。
`consecutive_requeues` と対称）。

`crates/task-dispatch/src/dispatcher.rs::on_worker_finished` の `Err(e) =>` 分岐で、
`provider_failure_outcome(&e)` が `None`（供給側失敗として分類できない: resume 拒否・プロセス
I/O・result.json 不在など、Phase 115 が直す経路もここを通る）のとき、
`consecutive_infra_requeues + 1 <= [dispatch] max_infra_retries`（既定 5）なら
`Trigger::InfraRequeue` で attempts を消費せず再試行し、超えたら `Trigger::WorkerError{retryable:
false}`（`Running` から無条件に `Failed`）で `"infra failure ×N: …"` を付けて打ち切る（D1 の
分類がこの接頭辞を見る）。`reclaim_expired_leases`（lease 失効）も同じ分岐（D5 参照）を通す。

再試行はバックオフする（`task_ops::derive::infra_backoff_delay`: 1 回目 30 秒、2 回目 2 分、
3 回目以降 5 分固定）。`Dispatcher` が `HashMap<TaskId, OffsetDateTime>`
（`infra_backoff`。プロセス内メモリのみ、DB に永続化しない）を持ち、`dispatch_ready` の候補ループで
既存の `retry_backoff`（attempts ベース）のすぐ後に同じ形でゲートする。dispatcher の再起動で
このバックオフ猶予は失われる（次の tick ですぐ再試行されうる）が、`consecutive_infra_requeues` は
`events` から数えるので上限判定自体は再起動をまたいでも正しい（**採らない**: バックオフの残り時間を
DB に永続化する。今回のスコープ外、未解決事項へ）。

「ADR-0054 のセッション resume を使い、途中までの作業を引き継ぐ」という要求は、**新しい配線を
足さない**: `Trigger::InfraRequeue` で `Ready` に戻ったタスクは、次の `dispatch_ready` で
通常の dispatch 経路（`resolve_node_session` / `sticky_session` を含む）をそのまま通るので、
継続セッションを持つノード（lead / CoS / reviewer）の run は自動的に同じセッションを resume する
（ADR-0054 D1 の経路そのまま）。通常の Execute タスクはそもそも run をまたぐ会話セッションを
持たないので、「引き継ぐ」は「同じ worktree のまま、attempts を消費せずにもう一度動かす」ことを
指す。

### D4. ライブ切替で生きている run を捨てない

`crates/celeris/src/instance.rs::Supervisor`: `[handoff] drain_force_abort`（新設、既定 `false`）を
足す。`drain_timeout_secs` に達しても `drain_force_abort = false` なら **`abort_all_runs` を
呼ばない**（1 回だけ WARN ログを出し、`in_flight == 0` になるまで `Draining` のまま待ち続ける。
`Step::DrainTimedOut` は返さない）。`drain_force_abort = true`（人が明示的に強い昇格を選んだとき
だけ設定する想定。`--force` 相当）のときだけ、従来どおり `Step::DrainTimedOut` → `abort_all_runs`。

待つ間の上限は「その run 自身の `max_wall_secs`」になる: draining なインスタンスは
D4 以前と同じく自分の run のリース更新・終了処理を続け（ADR-0040 D4 の記述どおり）、run が
`max_wall_secs`（+ `lease_grace`）を超えれば通常のリース失効の経路（D5）に乗る。
**新しい active が「旧が抱える run」の lease を横取りしない**という要求は、
(a) D4 が drain タイムアウトでの強制 abort をやめたことと、(b) D5 が lease 更新の失敗に強くなった
ことの組み合わせで実効的に満たす。lease に `instance_id` を埋め込んでインスタンスをまたいで
所有権を判定する仕組み（`daemon_instances` の heartbeat と突き合わせる等）は、今回は**採らない**
（`acquire_lease` の trait シグネチャを変える影響が大きい割に、上記 (a)(b) で本番事故の再発は
防げると判断した。未解決事項に記録し、実運用で問題が再現したら次の Phase で検討する）。

### D5. lease 更新は DB busy に耐える

- `StoreSink::heartbeat`（`crates/task-dispatch/src/dispatcher.rs`）: `renew_lease` が
  `StoreError::Sqlite` で `SQLITE_BUSY` / `SQLITE_LOCKED`（`task_core::store::is_busy_error`。
  新設の純粋関数）のときは、同じ heartbeat 呼び出しの中で最大 3 回（100ms 間隔）リトライする。
  それでも失敗したら **WARN ログのみ**（従来どおり run は止めない。run 自体はこの呼び出しの成否に
  依らず継続する）。
- `Dispatcher::reclaim_expired_leases`: lease が期限切れでも、**このインスタンスが持っている
  run**（`self.running` に entry がある）なら、まず `task_worker::process_group::group_alive`
  （新設。登録された pgid に対して signal 0 を送って生死を見る）でその run のプロセスが生きているか
  確かめる。生きていれば reclaim せず `renew_lease` で lease を延長して続行する（DB busy で
  数 tick 更新できなかっただけ、というケースをここで救う）。延長にも失敗したら次の tick に持ち越す
  （reclaim しない。プロセスが生きている限り）。死んでいる、またはこのインスタンスの管理外
  （`self.running` に無い）なら、D3 の分岐（`InfraRequeue` か、上限超えの `Failed`）に乗せる
  （**変更点**: 以前は無条件に `Trigger::LeaseExpired`〈`retry_or_fail`、attempts 消費〉だったのを
  D3 の attempts を消費しない経路に統合した。`Trigger::LeaseExpired` 自体は状態機械に残すが、
  `reclaim_expired_leases` からはもう使わない）。

## 3. 採らない

- lease への `instance_id`/`pid` の永続化によるクロスインスタンスの「横取り禁止」の直接判定
  （D4 参照。`acquire_lease` のシグネチャ変更が広範囲に及ぶため）。
- インフラ再試行のバックオフ猶予（`infra_backoff` map の残り時間）の DB 永続化（dispatcher 再起動で
  失われても、`max_infra_retries` の上限判定自体は event から数え直すので安全側）。
- 「取り下げ」を新しい状態遷移にする（D2 参照。コメントで足りると判断した）。
- CoS の「あなたの直近の仕事」の文言を変える（D1 参照。既存の Phase 33 の仕組みで足りると判断した）。

## 4. 追記（同日。実装中の指摘、本番でさらに確認された事実）

### D1 追記: ディスク不足も `infra` に分類する

2026-09-24 09:21Z、`/home` が満杯になり（このタスク自身の作業でも実際に再現した）、run が
`adapter: io error: No space left on device (os error 28)` で 2 回失敗して `failed` になった。
`AdapterError::Io` は既に D3 のインフラ分類経路（分類できない `Err`）を通るので `infra` にはなって
いたが、人が読む理由が英語の OS エラー文言のままだった。`task_ops::derive::classify_task_failure`
に `annotate_disk_full`（純粋関数）を足し、理由に `"No space left on device"` を含む場合は
**分類を無条件に `infra` にし**、理由の先頭に `"ディスク不足: "` を付ける。ディスク残量の事前チェック・
自動対処は行わない（分類と文言だけ。実装は別 Phase）。

### D2 追記: `retry` の `accept` 既定を `true` にし、`accept` 専用の道具を足す

本番で確認: `POST /tasks/{id}/retry` は `accept` を明示しないと複製が `draft` のまま止まり、`draft`
を `ready` にする API が（`approve` の兼用以外に）無かったため、「やり直したのに動かない」状態に
なった。

- `task-api::types::RetryBody.accept` の既定を `false` → **`true`** に変更（`serde(default =
  "default_retry_accept")`）。GUI の「やり直す」（受信箱・タスク詳細・失敗バナー・案件の仕事の木の
  4 箇所）も同じ既定に揃え、チェックボックスの意味を反転した:
  「下書き（draft）のまま始める」（既定チェック無し = `ready`）。失敗バナーの「やり直す」は
  チェックボックスを持たず、常に `ready` で始める。
- `celerisctl retry` の `--accept` フラグを廃止し、`--draft`（既定 `ready`、`draft` のまま始めたい
  ときだけ付ける）に変更。
- **`POST /tasks/{id}/accept`**（`task_ops::gate::accept`。`status == Draft` だけを許す、`approve`
  の薄い別名）と **`celerisctl accept <task_id>`** を新設した。`approve` は `Draft` でも
  `Trigger::Accept` を選ぶ（既存の実装のまま）ので挙動は変わらないが、`Approval` タスクの承認とも
  兼用でわかりにくかったため、名前だけで意図が伝わる専用の道具を足した。
- `docs/gui/help`（`/help#failure`）と本 ADR の D2、`celerisctl retry`/`accept`/`rereview`/`cancel`
  の説明文をこの既定に揃えた。

## 5. 受け入れ条件（D6）

(a) `failed` 遷移で `task_failed` 通知と受信箱の項目が出る。infra / work / 配送済みの 3 パターンを
テストする。(b) infra 分類の失敗は `attempts` を消費せずバックオフして再 dispatch し、
`max_infra_retries` 到達だけで `failed`（分類 infra）になる。(c) 偽の子プロセスが生きている間は
lease 失効で reclaim されない（`process_group::group_alive` を使う）。(d) drain timeout でも
生きている run は（`drain_force_abort` が既定 `false` の間は）abort されない。(e) GUI:
`failed` のタスク詳細にバナーとボタン、受信箱に失敗項目（vitest）、モバイル幅で崩れない
（`mobile-audit` 違反 0）。
