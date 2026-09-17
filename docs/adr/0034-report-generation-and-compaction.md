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

## D2. 生成の対象は「ワーカープロトコルの終端」だけ

`done` / `error` / `question` の 3 つ（`task.assignee` と組が必要）。**アダプタ自身の失敗**
（起動できない・レート制限・認証失敗 = 供給側失敗）は報告にしない。requeue と cooldown の話であって、
人に上げる「悪い知らせ」ではなく、同じ試行で何度も起こるため（SPEC §3.5「通知は数時間単位」に反する）。
`progress` も作らない（run の途中経過は人の見る単位ではない）。

`assignee` の無いタスク（Phase 23 以前の作り方）は報告を作らない。互換のため。

## D3. まとめの run は `role = "report-compressor"` の `execute` タスク

新しい `TaskKind` も新しいプロトコルも足さない（ADR-0033 の「載せ替え」の方針）。

- `assignee` = 親ノード、`project_id` = 子の報告の案件（複数の案件があれば**案件ごとに 1 件ずつ**）、
  `objective` = 子の報告（`headline` + `body`）＋「上司として 1 件にまとめよ」の指示（決定的な文字列）。
- **受け入れ条件は空**。出力は「1 件の報告」そのもので、決定的に確かめられるものが無い
  （条件ゼロのレビューは全 pass = `done`）。報告は run の終端の時点で既に作られている。
- その run の `done` を受けたとき、`sources` は **`report_unreviewed_children(親)` のうち
  `created_at <= まとめタスクの created_at` のもの**（= objective に載せた集合）。1 件も無ければ報告を作らない
  （やり直しで空のまとめが生えるのを防ぐ）。
- 同じ親・同じ案件のまとめタスクが終端でない間は、次のまとめを作らない（二重集計の防止）。

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

## 3. 採らない

- 報告の生成を LLM にやらせる（ADR-0033 の「採らない」のまま）。
- `progress` の報告を作る（数時間単位の通知に対して細かすぎる）。
- 報告を `events` に混ぜる（`events` はタスクの真実の追記ログで、報告は別の軸の観測値。
  `replay` の対象にしないために表を分ける）。
