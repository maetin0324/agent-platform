# ADR-0034: 報告の生成と圧縮 — ADR-0033 D3 を今の taskd の上で動かす細部

- 日付: 2026-09-17（Phase 25）
- 状態: Accepted
- 上位: `docs/SPEC.md` §2.4 / §3.5、`docs/adr/0033-organization-projects-and-reports.md` **D3**
- 関連: ADR-0007（Reviewer = 別 run）、ADR-0018/0032（クラスタ）、ADR-0013 D4（デーモンのスナップショット）
- 原則: DESIGN §1 の 1（協調判断に LLM を使わない）・2（状態はエージェントの外）・6（追記専用）は守る

ADR-0033 D3 が決めた「生成は決定的、圧縮だけ LLM、悪い知らせは素通り」を実装するにあたり、
**D3 が書いていない細部**をここで決める。表（`reports`）は migration 0006 で既にあり、**新しい migration は足さない**。

## D1. 「案件なし」の報告は `project_id` を空文字列で書く

`reports.project_id` は 0006 で `NOT NULL` として作られている。一方 D3 は、クラスタが落ちた等の
**案件に紐づかない悪い知らせ**を要求する。migration を足さない方針（Phase 25 の作業単位）なので、
モデルは `Option<ProjectId>` とし、**DB には `None` を空文字列で書く**（読むときに空なら `None`）。
`project_id` で絞る問い合わせは実在の ULID としか一致しないので、空文字列の行が混ざることはない。

- 採らなかった案: 0007 で列を NULL 可にする（並行する Phase 24 と migration 版数を取り合う）。
- 将来: 次に `reports` を触る migration があれば NULL 可に直してよい（読み書きは 1 か所 `task-core/src/report.rs`）。

**Phase 27 で解消**: migration 0007（`messages.task_id` を足す回）で `reports` を作り直し、`project_id` を
NULL 可にした（SQLite は列の NOT NULL を落とせないので表を作り直して写す）。空文字列のセンチネルは
`NULLIF(project_id, '')` で NULL に直し、モデルの読み書きは素直な `Option<ProjectId>` になった。

## D2. 生成の対象は「タスクの終端状態」（監査で改訂。当初は「run の終端」だった）

当初の D2 は「`done` / `error` / `question` という run の終端で作る」だったが、監査（Phase 25 後）で
2 つの逸脱が見つかった: (1) `done` をワーカーの run 終端（`WorkerFinished`）で作っていたため、
レビューで差し戻された「できました」まで報告になっていた（DESIGN 原則 4 違反）。(2) `error` を
`Result<RunOutcome, AdapterError>` の `Err`（供給側失敗）から作らない実装だったため、供給側失敗が
`max_requeues` に達して通常の失敗に転じたときも報告が作られなかった。

改訂後の規則は **「タスクの状態機械の終端遷移」に合わせる**:

- `result`（`kind = result`）は、レビューを通って **`Status::Done` に遷移したとき**だけ作る
  （`on_review_finished` の `Trigger::ReviewPass` 適用後）。ワーカーの「できました」がレビューで
  差し戻され、後で通ったとしても、報告になるのは最後に `Done` になった 1 回だけ。
- `bad_news` は **`Status::Failed` に遷移したときだけ、原因を問わず** 1 件作る:
  ワーカー自身が返した `error`、供給側失敗が `max_requeues` に達して通常の失敗に転じた場合、
  レビュー不合格（`Check` が fail して `max_retries` を使い切った場合）のどれでも同じ扱い。
  `Status::Ready` に戻るだけの途中の失敗（`retryable` でまだ試行が残っている、供給側失敗で
  requeue された）は報告にしない（同じ試行で何度も起こり、SPEC §3.5「通知は数時間単位」に反するため）。
- `question` は従来どおり **run の終端**（`Trigger::WorkerQuestion` の直後）で作る。人の返事を
  待つ状態は、タスクの終端を待たず即座に知らせるべきため。
- `progress` は作らない（run の途中経過は人の見る単位ではない）。

`assignee` の無いタスク（Phase 23 以前の作り方）は報告を作らない（互換のため）。`assignee` があっても
組織に存在しないノードなら、報告を作らず `warn!` する（監査 M-5。`level_of` が未知ノードで 0 を返すため、
チェックが無いと「秘書の報告」に化けていた）。

## D3. まとめの run は `role = "report-compressor"` の `execute` タスク

新しい `TaskKind` も新しいプロトコルも足さない（ADR-0033 の「載せ替え」の方針）。

- `assignee` = 親ノード、`project_id` = 子の報告の案件（複数の案件があれば**案件ごとに 1 件ずつ**）、
  `objective` = 子の報告（`headline` + `body`）＋「上司として 1 件にまとめよ」の指示（決定的な文字列）。
- **受け入れ条件は空**。出力は「1 件の報告」そのもので、決定的に確かめられるものが無い
  （条件ゼロのレビューは全 pass = `done`）。報告は run の終端の時点で既に作られている。
- その run の `done` を受けたとき、`sources` は **`report_unreviewed_children(親)` のうち
  `created_at <= まとめタスクの created_at` かつ **`project_id` がまとめタスクと同じ**もの**
  （= objective に載せた集合。監査 H-1: 案件フィルタが無いと、並行する別案件のまとめ run が先に `done` に
  なったとき、その案件の子報告まで巻き込んでしまっていた）。1 件も無ければ報告を作らない（やり直しで
  空のまとめが生えるのを防ぐ）。
- 同じ親・同じ案件のまとめタスクが終端でない間は、次のまとめを作らない（二重集計の防止）。
- **まとめの run はレビューではなく要約**（監査で明記。D2 の「差し戻し」の経路はここには無い）:
  受け入れ条件が空なので `Check` は 1 つも無く、レビューは常に全 pass = `done`。つまりまとめの run が
  `reviewing` から `ready` に戻ることはなく、`done` になるかその手前で失敗する（供給側失敗、アダプタの
  起動失敗など）かのどちらかだけ。まとめ run が失敗し続ける場合のバックオフは D3a を見よ。

### D3a. まとめ run の失敗はバックオフする（監査 H-2）

まとめ run が失敗すると、子の報告は「レビュー待ち」のまま `reports` に残る（`sources` に入っていない）。
次の tick でまた閾値（件数または経過時間）を満たすので、対処が無いと**失敗するたびに新しいまとめタスクが
作られ続ける**。これを防ぐため、`schedule_report_compaction` は「開いている（終端でない）まとめタスクが
無いこと」に加えて、**「同じノード・同じ案件のまとめタスクが直近 `compress_after_secs` 以内に `Failed` /
`Cancelled` で終わっていないこと」**も確認する。両方の条件を満たしたときだけ次のまとめを作る。
`compress_after_secs`（既定 2 時間）を再利用し、新しい設定は増やさない。

## D4. 悪い知らせは「1 段ずつ鎖でつなぐ」

`bad_news` は生成と同時に各祖先へ複製する（SPEC §2.4）。各コピーの `sources` は**1 段下の報告の id**
（最初のコピーだけは元の報告）。こうすると「どの `sources` にも入っていない報告 = レビュー待ち」という
規則がそのまま成り立ち、悪い知らせが**圧縮の対象として二重に上がらない**。秘書のコピーだけが未読として残る。

クラスタが落ちた（`Event::ClusterUnavailable`）ときの報告先は、`kind = department` かつ id が `infra` のノード、
無ければ秘書。同じホストの障害はクラスタの cooldown の間 1 件だけにする（毎 tick 報告しない）。

## D5. 圧縮の判断は tick ループ（taskd）、生成はディスパッチャ（task-dispatch）

- 生成: `task-dispatch` の終端処理（`on_worker_finished`）で、既存の `Event` 追記の隣。同じ SQLite。
- 圧縮の判断: `taskd` の `tick_loop` の中で、チャネルに送らずその場でストアを見る（B1 規約）。
  やるのは「`reports` を読む」「まとめタスクを 1 件作る」だけで、run を起こすのは次の tick の通常の dispatch。
- 閾値は `[reports] compress_after = 4` / `compress_after_secs = 7200`（既定）。

## D6. `last_notified_at` は API プロセスのメモリに置く

通知の判定（`notify_now`）は決定的（`bad_news` の未読があれば即 true、無ければ「未読があり前回の通知から 2 時間」）。
`last_notified_at` を置く列が `reports` には無く、migration を足さないので、**API プロセスのメモリ**に持つ。
その結果、`DaemonSnapshot.reports` は**ディスパッチャではなく API が応答を組むときに埋める唯一のフィールド**になる
（ディスパッチャが送るスナップショットでは常に `None`）。taskd を再起動すると「まだ通知していない」に戻るだけで、
未読の件数は DB から毎回数え直すので、人が見る値は失われない。

- 採らなかった案: `AdminRequest` にもう 1 種類足してディスパッチャのメモリに置く（API → tick ループの往復が増え、
  Phase 24 と `admin.rs` を取り合う）。

`last_notified_at` は**プロセスのメモリ**であって特定の GUI クライアントの状態ではないので、**複数の GUI
クライアントで共有される**: 誰か 1 人が `POST /reports/notified` を叩くと、他の全クライアントの通知も
同じ 2 時間分だけ止まる（次に未読が増えるか `bad_news` が来るまで、誰にも通知されない）。これは
マルチユーザ通知の作り分け（非目標。DESIGN §6 の非目標）をしない前提での妥当なトレードオフとして受け入れる。
`POST /reports/notified` は状態を進める操作なので**管理系 API 扱い**とし、loopback でもトークン必須にする
（§5.10 の管理系の規則と同じ。ADR-0017）。

## D7. `report.kind` はワーカーが宣言し、taskd は固定表で写すだけ（P-72。実装は先送り）

`ReportKind` には `Proposal` が既にあるが、Phase 25 では生成しない（D2 の生成規則は `Done`/`Failed`/
`Question` の 3 遷移からしか作らないため）。人間の依頼（P-72）は「ワーカーが `Done` の結果を
`proposal`（提案）として宣言できるようにしたい」というもので、次のとおり決める:

- 結果ファイル規約（`artifacts/result.json`）に、任意の `"report": {"kind": "..."}` を足す。
  ワーカーは `done` を返すとき、その結果が「提案」なのか「ただの結果」なのかを**自分で宣言**する。
- taskd（`record_run_report` 相当）は、この値を**固定表**（`"proposal"` → `ReportKind::Proposal`、それ以外・
  欠落・未知の値 → `ReportKind::Result`）で写すだけ。判断（この結果が提案に値するか）はしない
  （DESIGN 原則 1。LLM 的な判断をディスパッチャ・ストアに持ち込まない）。
- **実装は Phase 24（結果ファイルの `memory` 拡張）のマージ後に行う**。結果ファイルのスキーマを
  同時に 2 つの Phase が触ると、`docs/protocol/worker-protocol.schema.json` の生成元が競合するため。
  Phase 25 の時点ではコード変更は入れず、この ADR に決定だけ記録する。

**Phase 27 で実装**: 結果ファイルの規約に `"report": {"kind": "result"|"proposal"|"bad_news"|"question"}` を
足した（任意。`PROTOCOL_VERSION` は 4 のまま — 追加だけで、既存のワーカーは何も変えなくてよい）。
読むのは `task_worker::read_result_report_kind`（ファイル I/O だけ）、写すのは
`task_dispatch::reports::declared_kind`（固定表: `"proposal"` → `Proposal`、それ以外・未知・欠落 →
`Result`）。宣言が効くのは `Done` → `Status::Done` の報告だけで、`bad_news` / `question` の生成規則
（D2）は変わらない（`done` を名乗って悪い知らせに化けることはない）。

## 3. 採らない

- 報告の生成を LLM にやらせる（ADR-0033 の「採らない」のまま）。
- `progress` の報告を作る（数時間単位の通知に対して細かすぎる）。
- 報告を `events` に混ぜる（`events` はタスクの真実の追記ログで、報告は別の軸の観測値。
  `replay` の対象にしないために表を分ける）。
