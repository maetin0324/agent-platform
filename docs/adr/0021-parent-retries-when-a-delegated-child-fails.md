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
