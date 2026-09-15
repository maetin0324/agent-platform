# ADR-0016: 役割と委譲（組織的な木構造の実行）

- 日付: 2026-09-15
- 状態: **Proposed**（設計のみ。実装は人間の判断を待つ）
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
