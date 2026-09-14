# ADR-0011: 連続 requeue の上限（P-38）

- 日付: 2026-09-14
- 状態: Accepted（人間の判断: 「requeue の回数はコンフィグで設定できるように。デフォルト 5 回くらい」）
- 関連: [ADR-0010](0010-phase7-hardening.md) D5 / `docs/PROGRESS.md` Phase 7 未解決事項 2・提案 P-38

## 文脈

ADR-0010 D5 で、供給側失敗（レート制限・認証失敗・枯渇・起動失敗）は attempts を消費せず `requeue` するようにした。
しかし上限が無いため、恒久的な失敗（コマンドのパス誤り、失効した認証、誤分類）が cooldown ごとに永久に requeue され、
タスクが `failed` にならず `taskd --until-idle` も終わらない（Phase 7 監査の指摘）。

## 決定

### D1. 設定

`taskd.toml` のトップレベルに `max_requeues`（既定 **5**）を追加し、`DispatchConfig.max_requeues` に写す。`0` なら requeue しない
（供給側失敗は最初から通常の失敗として扱う）。

### D2. 数え方（状態は DB から決める）

「同じ試行（attempt）での連続 requeue 回数」を、そのタスクの `events` から決定的に数える。`Transitioned` を新しい順に見て、
`reason = "requeue"` を数え、`"dispatch"` は読み飛ばし、それ以外の reason（`accept`・`answer`・`worker_error`・`review_fail` 等）に
当たったら止める。デーモンを再起動しても同じ値になる（メモリ上のカウンタは持たない）。

Reviewer run の延期（ADR-0010 D5, P-29）は、`reviewing` に入った最後の `Transitioned` 以降の
`WorkerProgress{msg: "reviewer run requeued: ..."}` の件数で数える。

### D3. 上限に達したとき

供給側失敗を **P-21 以前と同じ通常の失敗**として扱う（新しいトリガや状態は追加しない）:

- ワーカー run: `Trigger::WorkerError{retryable: true}`（attempts を消費し、`max_retries` を超えたら `failed`）。
  `WorkerFinished.outcome = "error(retryable=true): requeue limit (<N>) reached: adapter: <error>"`。
- Reviewer run: 未判定の `Reviewer` 条件を `pass=false, reason="requeue limit (<N>) reached: <error>"` とし、`ReviewFail` を適用する。

どちらの場合もプロバイダの cooldown（`ProviderPolicy::report`）は従来どおり行う。

不採用の案: 上限到達で即 `failed` にする案は、一時的に長引いたレート制限で `max_retries` の猶予を使わずにタスクを落とすため採らない。
承認待ち（`Approval`）に回す案は、人間が判断すべき内容（研究上の判断・破壊的操作）ではないため採らない。

最悪の実行回数は 1 タスクあたり `(max_retries + 1) × (max_requeues + 1)` 回で有界になる。

## 結果

- `task-dispatch`: `consecutive_requeues` / `consecutive_reviewer_requeues`、`on_worker_finished` と `on_review_finished` の分岐。
- `taskd`: `max_requeues` 設定。`config/taskd.example.toml` に追記。
- `docs/DESIGN.md` への反映（§4.2 / §5.2 / §6 Phase 7 受け入れ 8）は、人間の許可を得て 2026-09-14 に行った（P-40）。
