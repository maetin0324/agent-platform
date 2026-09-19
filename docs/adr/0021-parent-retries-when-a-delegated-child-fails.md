# ADR-0021: 委譲した子が失敗したときは、親がやり直す（駄目なら人に聞く）

- 日付: 2026-09-16
- 状態: Accepted（人間の判断「親がこの失敗を引き継ぐのではなく、自動でリトライするか、リトライできないなら人間に判断を投げるという形がいいですね」）
- 関連: ADR-0016（役割と委譲、特に D2 / M5）、ADR-0002 D2/D3（状態機械）、ADR-0010 D1、P-56

## 文脈

ADR-0016 M5 の現状: 親は委譲した子が全て**終端**になるまで `reviewing` で待ち、その後に集約 run（`aggregate = true`）か
`ReviewPass` に進む。`failed` も終端なので、**子が失敗しても親はそのまま done になる**。

P-56 で挙げていた代案は「子が 1 件でも failed なら親を `review_fail` にする」だったが、これは親が子の失敗を引き継いで
`failed` になるだけで、組織としてのやり直しの余地が無い。人間の判断は「引き継ぐのではなく、やり直す。やり直せないなら人に聞く」。

## 決定

### D1. 子の失敗は「親のやり直し」として扱う（引き継がない）

委譲した子（`Event::Delegated.task_ids`）が全員終端になった時点で、**新たに `failed` になった子**が 1 件でもあれば、
親に新しいトリガ `Trigger::ChildFailed` を適用する:

| 条件 | 遷移 | attempts |
|---|---|---|
| `attempts + 1 <= max_retries` | `reviewing → ready`（**やり直し**） | +1 |
| それ以外 | `reviewing → blocked`（**人に聞く**。D2） | 据え置き |

**`failed` にはしない。** やり直しの run は、子の結果（誰が何で失敗したか）を `context.children` で受け取る。
同じ子にもう一度やらせるか、別の分け方で委譲し直すか、自分でやるかは **run の中の判断**（＝ LLM）で、ディスパッチャは決めない（原則 1）。

### D2. やり直せないときは、失敗にせず人間に質問を出す

`blocked` にすると同時に `Event::QuestionRaised{run_id, text}` を積む。本文は決定的に組み立てる
（失敗した子の id / title / 直近の `WorkerFinished.outcome` と、`taskctl answer <親 id> "…"` の案内）。
受信箱の「質問」区画（`GET /inbox` の `questions[]`）と GUI にそのまま出る。人が `answer` すれば既存の `Trigger::Answer` で
`blocked → ready` に戻り、回答は次の run の `context.answers` に載る。

`Event::QuestionRaised` を足すのは、これまでの質問が `WorkerFinished{outcome: "question: …"}` という
**run の終わり方**としてしか表せなかったため。ここで質問を出すのは run の終了ではない（run は既に終わっている）ので、
既存のイベントを流用すると run の記録が壊れる。派生値（`latest_question`、受信箱の `questions[]`）は両方を見る。

### D3. 対象は「委譲した子」だけ、「新たに失敗した子」だけ

- `Event::Delegated` で挿入された子に限る。Plan kind が materialize した子は対象外（従来どおり。承認ゲートで人が見る）。
- `cancelled` の子は対象外（人が意図して止めたか、先行の失敗で連鎖したもの。改めて聞き直さない）。
- **一度扱った失敗は数え直さない**: 子が `failed` になったイベントの**グローバル id** が、親の直近の
  `Transitioned{reason: "child_failed"}` の id より大きいものだけを数える。これが無いと、親が別の子に割り当て直して
  成功しても、古い失敗のせいで親が永久に完了できない。

### D4. 設定で切れる

`[delegation] on_child_failure = "retry_then_ask"`（既定）| `"ignore"`（ADR-0016 M5 までの挙動）。

## 結果

- 委譲した子の失敗で親が落ちることは無くなる（`Trigger::ChildFailed` は `failed` を作らない）。
- 人が見るのは「自動でやり直しても駄目だったもの」だけになる。受信箱の「質問」がその入口。
- 状態機械にトリガが 1 つ、イベントが 1 つ増える（ADR-0010 / ADR-0016 と同じ流儀）。全網羅テストは 4×8×13 になる。
- `replay` は `Transitioned{reason: "child_failed"}` をそのまま再現できる（`QuestionRaised` は状態を変えないので無視）。

## Phase 45 追記（2026-09-19、実機）

D3 の「一度扱った失敗は数え直さない」判定は、`Dispatcher::newly_failed_delegated_children`
（`crates/task-dispatch/src/dispatcher.rs`）で実装されていたが、親の直近の `child_failed` 遷移の位置
（`handled_at`）と子の `failed` 遷移の位置（`failed_at`）の両方を `TaskStore::events_for` から取っていた。
この `events_for` が返す `u64` は**タスクごとにローカルな `seq`**（0 始まり）であり、`events` テーブルの
グローバルな `id`（Phase 9b / ADR-0013 D6 で導入済み）ではない。そのため「親の `seq` ≥ 子の `seq`」を
グローバルな前後関係の代わりに使っており、**子の方が親よりイベント数が多い**（＝ `seq` が大きい）場合に
毎回「新規の失敗」と誤判定していた。

症状（本番、2026-09-19、親 `01M2VG4YNG4DD7Z5BYPSB8W8AW`）: 委譲した子が 1 回失敗した後、人間が
`taskctl answer` で答えて親を `ready` に戻すたびに、親は同じ子の失敗を「新規」として数え直し、
`blocked` と「委譲した子タスクが失敗し… どうしますか」という質問（`Event::QuestionRaised` と
`approvals` の新しい行、Phase 44）を繰り返した。20 分で 5 回、同じ質問が出た。

修正: `TaskStore` に `events_for_with_global_ids`（`events.id` 昇順で返す）を追加し、
`newly_failed_delegated_children` の `handled_at`/`failed_at` の両方をこちらから取るようにした
（`events_for` はタスクをまたいだ比較に使えないことをドキュメントに明記した）。他に `events_for` の
戻り値をタスクをまたいで比較している箇所は無かった（`crates/task-dispatch` / `task-ops` / `taskd` /
`task-api` を監査。詳細は Phase 45 の `docs/PROGRESS.md`）。

回帰テスト: `crates/task-dispatch/src/dispatcher.rs` の
`child_failure_question_is_not_repeated_when_the_child_has_more_events_than_the_parent`
（子に親より多くの `WorkerProgress` を積んでから失敗させ、親が 2 回目の run でも `blocked` を
繰り返さず `done` になること、`QuestionRaised` と `child_failed` 遷移がそれぞれちょうど 1 件であることを
確認）。旧実装（`events_for_with_global_ids` を `events_for` に戻した状態）では失敗することを確認済み。
