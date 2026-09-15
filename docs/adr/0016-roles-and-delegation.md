# ADR-0016: 役割と委譲（組織的な木構造の実行）

- 日付: 2026-09-15
- 状態: **Accepted**（2026-09-15 に Phase 10 として実装。D1〜D3 と実装メモ M1〜M10。D4（木の予算）は本 Phase の受け入れ条件に無く未実装）
- 関連: ADR-0007（Plan と子タスク）、ADR-0008（承認ゲート）、ADR-0013（API）、DESIGN §4.1 / §4.2 / §5.6 / §6 Phase 10

## 文脈

人間の当初の狙いは「会社組織のように、木構造でエージェントを走らせて使いこなす基盤」だった。現状できているのは次まで。

- `Plan` kind のタスクが、1 回の run で子タスクの一覧（`artifacts/plan.json`）を出し、`materialize` が子として挿入する。
- 親子は `parent_id`、先行後続は `depends_on`。Plan の入れ子は深さ 3 まで（`MAX_PLAN_DEPTH`）。
- `Check::Human` は `Approval` 子タスクになり、人間の承認を待つ。GUI に DAG 画面がある。

足りないのは次の 3 点。

1. **役割が無い**: kind は `Execute` / `Plan` / `Approval` / `Review` の 4 つで、「この仕事は設計担当に」「実装は 3 人に分ける」「レビューは別の担当」という割り当てを表現できない。
   プロンプトもタスクの `objective` だけで、立場（何を任され、何を任せてよいか）が伝わらない。
2. **実行中に増やせない**: 分解は Plan の run が終わった瞬間に確定する。作業してみて初めて分かった追加作業を、その run の中から子として足せない。
3. **集約が無い**: 子が全部終わっても、親は「子の完了」を待つだけで、結果をまとめる run が無い。人間が GUI で個別に読む必要がある。

## 決定（案）

### D1. `Task.role`（自由記述の役割名）と役割ごとの既定

- `Task` に `role: Option<String>`（例 `"lead"` / `"implementer"` / `"reviewer"` / `"researcher"`）を足す。状態機械は role を見ない（**判断は増やさない**）。
- 設定 `[[roles]]` で、役割ごとの既定（`tier`、`adapter`、`max_turns`、`max_wall_secs`、`permission_mode`、プロンプトに前置きする指示文）を持つ。
  タスクに書かれた値が優先、無ければ役割の既定、無ければ全体の既定。
- ワーカープロトコルの `RunRequest.task` に `role` と役割の指示文を載せる。アダプタはそれを system prompt の前置きにする。
- 効果: 「部長 / 実装者 / レビュア」を設定で定義し、Plan の出力（`plan.json`）で子ごとに役割を指定できる。

### D2. 実行中の委譲（`delegate` メッセージ）

- ワーカープロトコルに `{"type":"delegate","tasks":[{title, objective, acceptance, role?, depends_on?}]}` を追加する（`progress` と同じ経路の追記メッセージ）。
- ディスパッチャは受け取った提案を**検証してから**子タスクとして挿入する（`task-ops::add::create_task` と同じ検証 + 深さ・件数の上限。既定は 1 run あたり 8 件、木の深さ 5）。
  挿入は `WorkerProgress` と同じトランザクションで、`Event::Delegated{run_id, task_ids}` を残す。
- 親は run を続けてよい（子は独立に dispatch される）。親が `done` を返しても、**子が終わるまで親は `reviewing` のまま**にする
  （`ready_tasks` の「親が Approval なら親の done を待つ」規則を、`pending_children > 0` の一般規則に広げる）。
- 無限増殖の防止: 深さ・件数の上限に加え、木全体の run 数の上限（設定 `max_tree_runs`、既定 100）を親タスクごとに数え、超えたら `delegate` を拒否して `WorkerProgress` に理由を残す。

### D3. 集約 run（`aggregate`）

- `Task.aggregate: bool`（既定 false）。true の親は、子が全て終端になった時点で **もう 1 回だけ** run を起動する。
  その run には `context.children`（子の `title` / `status` / 直近 run の `outcome` / 成果物の一覧）を渡し、要約を `artifacts/summary.md` に書かせる。
- 集約 run の結果も通常どおり受け入れ条件で判定する（`Check::ArtifactExists{"summary.md"}` を既定で足す）。
- `attempts` の扱いは通常 run と同じ。集約 run の失敗は親の失敗。

### D4. 木の予算

- `Budget` に `tree_max_wall_secs` / `tree_max_tokens`（任意）を足し、親から子へ**配分**する（子の合計が親の上限を超えない）。
- 超過したら、それ以上の `delegate` を拒否し、実行中の子はそのまま完走させる（途中で殺さない）。使用量は `WorkerFinished.usage` の合計で数える。

### D5. GUI（`taskd-gui` 側の別フェーズ）

- DAG 画面を「組織図」表示に拡張（役割ごとに色・レーン、親子を入れ子の矩形、依存を辺）。
- タスク詳細に「部下」（子）と「上司」（親）、委譲の履歴（`Delegated` イベント）を出す。

## 採らない

- 役割の階層そのものを型にする（`Manager` / `Worker` kind を増やす）: kind は状態機械に影響するので増やさない。役割は**属性**にとどめる。
- エージェント同士の直接の会話（メッセージパッシング）: 調整はタスクと成果物を通じてだけ行う（原則 1: 協調判断は決定的に）。

## 影響

- スキーマ: `tasks` に `role`（`json` 内。列は増やさない）、`Event::Delegated` の追加（任意フィールドなので既存 DB は読める）。
- ワーカープロトコル: `delegate` メッセージの追加（プロトコル版を上げる）。
- 受け入れ条件は Phase 10（DESIGN §6）に書く。

## 実装メモ（Phase 10、2026-09-15。DESIGN §6 Phase 10 の 1〜6）

本文の決定（D1〜D3）は変えていない。実装で決めた細部を記す。D4（木の予算）と D5（GUI）は本 Phase の範囲外。

- **M1. 集約 run のための遷移は `Trigger::Aggregate`（`reviewing → ready`、attempts 据え置き、reason `"aggregate"`）。**
  状態の集合と `TaskKind` は変えない。ADR-0010 D1 が `Requeue` / `DependencyFailed` を足したのと同じ「遷移表への追加」であり、
  `replay` は `aggregate` を attempts を増やさない reason として扱う。集約 run はこの遷移の後の通常 dispatch なので、リース・
  バックオフ・再起動後の復旧・`WorkerStarted`/`WorkerFinished` の記録は全て通常 run と同じ（D3「attempts の扱いは通常 run と同じ」）。
- **M2. 委譲された子は `draft` で挿入し、同じトランザクションで `Accept` して `ready` にする。** `plan.auto_accept` は見ない
  （Plan の子は人間が中身を確かめる前提だが、委譲は既に承認され実行中の親の判断であり、待たせると親の run が空回りする）。
  親には `Event::Delegated{run_id, task_ids}` を同じトランザクションで残す（`TaskStore::delegate_children`）。
- **M3. 役割の既定はタスク作成時に解決し、指示文は run 時に付ける。** `task_ops::add::create_task_with_roles` が `NewTaskSpec` の
  省略値（`tier` / `adapter` / `max_turns` / `max_wall_secs` は `Option` になった）を `[[roles]]` → 全体既定の順で埋める。
  役割名は自由記述で、設定に無い役割も許す（既定も指示文も無いだけ）。ディスパッチャは run 開始時に `RunContext.role = {id, instructions}`
  を載せ、`WorkerStarted.task_role` に役割名を記録する（`role` は既に worker/reviewer の別に使っているため別名）。
  `taskctl add --role` は `--config <taskd.toml>` があれば既定を解決し、無ければ役割名だけを保存する。
- **M4. 集約 run の判定。** `task.aggregate && 子が 1 件以上 && 子が全て終端` のとき、レビュー全 pass の直後に `Aggregate` を適用して
  `ready` に戻す。その後の run は `RunContext.children[]`（`id` / `title` / `role` / `status` / 直近 run の `outcome` / 成果物）を受け取り、
  レビューでは暗黙の条件「`artifacts/summary.md` が存在する」（`criterion_idx = acceptance.len()`。Plan の暗黙条件と同じ位置づけ）が加わる。
  集約 run が review_fail で `ready` に戻った再試行も集約 run（`Transitioned{reason:"aggregate"}` がイベント列にあれば以後の run は集約 run）。
  集約は 1 回だけ: 集約遷移の後に子が増えても再集約はしない。
- **M5. 子待ちの親。** レビュー全 pass の時点で終端でない子があれば、判定（`ReviewVerdict`）はその場で記録し、
  `WorkerProgress{msg: "waiting for N delegated child task(s)"}` を残して遷移は適用しない（`reviewing` のまま）。ディスパッチャは
  メモリ上の `awaiting_children` に入れ、毎 tick に子を数え直す。全て終端になったら M4（aggregate）か `ReviewPass`（aggregate = false）。
  再起動でメモリが消えた場合は既存の復旧経路（`recover_reviews`）が再レビューする（判定が二重に記録されるだけで状態は変わらない）。
  `aggregate = false` の親は子の成否を問わず `done` になる（子の失敗は木の中で見える。親の受け入れ条件は既に pass している）。
  子待ちの親は `awaiting_human` と同じく idle 判定の待ち対象から外す（子が `ready`/`running` なら子の側で idle でなくなる）。
- **M6. 上限の数え方。** 木の深さは「祖先の数 + 1」（根 = 1）。提案された子の深さが `max_tree_depth` を超えたら拒否。
  木の run 数は、根から全ての子孫について `WorkerStarted{role: None}`（ワーカー run）を数えた合計が `max_tree_runs` に達していたら拒否。
  1 run あたりの件数は同じ run の複数の `delegate` メッセージをまたいで数える。拒否は提案 1 件ごとに `WorkerProgress{msg: "delegate rejected: tasks[i] <title>: <reason>"}`
  に残し、通ったものだけ挿入する。run 自体は失敗しない。
- **M7. `depends_on` の書き方。** 同じ `delegate` の `tasks` 配列内のインデックス（整数）か、既存タスクの ID（文字列）。ID は存在し、
  `failed`/`cancelled` でなく、委譲元のタスク自身とその祖先でないこと（受け入れ 4）。配列内の閉路・自己参照は Plan と同じ検出で拒否。
- **M8. LLM アダプタの委譲は `artifacts/delegate.json`。** claude-code / codex はストリームのプロトコルを話さず結果ファイル規約
  （`artifacts/result.json`）で終端を決めているので、委譲も同じ規約にする: run の終わりに `artifacts/delegate.json`（`{"tasks":[…]}`、
  `delegate` メッセージの `tasks` と同じ形）があれば `EventSink::delegate` に渡す。run 開始時に前回のファイルは消す。
  プロンプトには役割の指示文（`## Role`）、委譲の方法、集約 run なら `## Delegated child tasks` の一覧を足す。
- **M9. ワーカープロトコルは v2。** 追加は `delegate` メッセージ、`context.role`、`context.children`、`task.role` / `task.aggregate`。
  全て追加のみで、v1 のワーカーはそのまま動く（`protocol` フィールドは検査していない）。
- **M10. Plan の出力 `NewTask` に `role`（任意）を足す。** D1「plan.json で子ごとに役割を指定できる」のため。`materialize` は親の
  `aggregate` を引き継がず、子の `aggregate` は false。
