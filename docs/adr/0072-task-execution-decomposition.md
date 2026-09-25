# ADR-0072: Task の下に内部の実行層（ExecutionPlan / WorkUnit / Run）を置き、session の予算切れを Task の失敗にしない

- 日付: 2026-09-24
- 状態: **Accepted**（Phase E0 = 調査と設計。Phase E1 で着手し Accepted にした。2026-09-24）
- 関連:
  - ADR-0002（状態機械）、ADR-0003 / 0006（ワーカープロトコルと結果ファイル規約）
  - ADR-0007（Planner / Reviewer）、ADR-0016 / 0021（委譲と子の失敗）
  - ADR-0043（worktree）、ADR-0054（継続セッション、Phase 112 / 113）
  - ADR-0061（harness routing とメトリクス）、ADR-0069（routing の 4 層）、ADR-0070（失敗の可視化、`InfraRequeue`）
- 前提の調査: `docs/execution-architecture-2026-09-24.md`（以下「調査」。§番号はその文書の節）

## 1. 文脈

大きな依頼（routing 再設計、Knowledge GC、大規模 GUI、調査→設計→実装→テスト→review→release）が、
案件の下で**ほぼ 1 Task = 1 worker session**として dispatch されている。調査で確かめた事実は次のとおり。

- Task と run は 1:1 で結合している（調査 §0-1）。
  - `running: HashMap<TaskId, RunEntry>`、`tasks` 行の lease、`attempts`。
  - 仕事の run は `--no-session-persistence`。
- `error_max_turns` と wall-clock 超過は `Terminal::Error{retryable:true}` になり、`attempts` を 1 つ消費する。
  - 次の試行は何も引き継がない新規 run になる。
  - usage も捨てられる（調査 §6）。
- reviewer の不合格（`cargo fmt --check` 程度でも）は `ReviewFail` → Task 全体の再実行になる。局所修復の仕組みは無い（調査 §8・§9）。
- 分解の仕組み（`Plan` kind・委譲）はどちらも**ユーザーに見える子 Task**を作る（調査 §15）。
- `TaskFeatures` は lane の決定にしか使われていない（調査 §3）。

知識ベースには「大きな PoC を 1 session でやると `error_max_turns` になる。分割が要る」という経験知が既にある。
しかし architecture はそれを強制していない。

## 2. 用語

```
Project
└─ Task                      … ユーザーが達成したい goal（1 つのまま。GUI・ボード・受信箱・配送の単位）
   └─ ExecutionPlan (v1, v2…) … 内部の実行計画（WorkUnit の集合と依存）。単純な Task では作らない
      ├─ WorkUnit A           … context を分けてよい、意味のある作業単位
      │  ├─ Run #1            … harness の 1 回の実行（= 1 session）
      │  └─ Run #2            … Run #1 の checkpoint から新しい context で続ける（continuation）
      └─ WorkUnit B …
```

- **暗黙の WorkUnit**: ExecutionPlan を持たない Task（atomic）は「Task の objective そのものを spec とする WorkUnit が 1 つ」
  として扱う。DB に行は作らない（D5）。
- **Run**: 既存の `run_id`（`WorkerStarted` 〜 `WorkerFinished`）そのもの。新しい id 体系は作らない。

---

## 3. 決定

### D1. Task / ExecutionPlan / WorkUnit / Run の責務

| 層 | 責務 | 持たないもの | 状態の置き場 |
|---|---|---|---|
| **Task** | ユーザーの goal、受け入れ条件（完了の定義）、担当（Ownership）、worktree とブランチ（`celeris/<task_id>`）、最終レビュー、配送、attempts（**最終レビューのやり直し回数**） | 実行の途中経過の細部 | `tasks` / `events`（従来どおり） |
| **ExecutionPlan** | WorkUnit の集合・依存・版（replanning の履歴）・作った根拠（gate / planner / human） | 担当・モデルの選択（ADR-0069 D1 と同じく LLM に選ばせない） | `execution_plans`（D5） |
| **WorkUnit** | 1 つの context に収まる意味のある作業（spec・完了の目安・任意の決定的検査）、continuation / retry / repair の回数、最新の checkpoint | 独自の worktree・独自のレビュー・独自の配送（それらは Task のもの） | `work_units`（D5） |
| **Run** | harness の 1 session。終わり方（D7）、usage、checkpoint（D8） | 次に何をするかの判断（daemon が決める） | 既存の `WorkerStarted` / `WorkerFinished` + `runs` 索引（D5） |

WorkUnit は「LLM session 1 回 = WorkUnit」ではない。WorkUnit の例は現行調査、decomposition の設計、core data model・scheduler の実装、
continuation・checkpoint の実装、reviewer・repair の実装、統合テスト、release の検証など。
1 つの WorkUnit が複数の Run（continuation）にまたがってよい。

### D2. Project / Task の UX を維持する理由（WorkUnit を子 Task にしない）

- **人が見る単位を増やさない**: SPEC §3.3 の「仕事の木」は**方針の異常を一目で見る**ためのもの。そこに session の都合の分割
  （Run #3 の続き、fmt の修復）が並ぶと、木が実行の雑音で埋まる。人が口を出したいのは goal の粒度であって、
  context の切れ目ではない。
- **Task に付いているものはすべて goal の単位で意味を持つ**: worktree とブランチ、受け入れ条件、レビュー、配送、通知、受信箱。
  子 Task にすると、これらが WorkUnit ごとに複製される。worktree はタスクごと（ADR-0043 D2）、配送はタスクごと（ADR-0051）。
  結果として「同じ変更を 6 本のブランチに分けて順に取り込む」ことになる。
- 既存の子 Task の仕組み（`Plan` kind、`delegate.json`）は**ユーザーに見える別の deliverable を作るとき**のために残す（D22）。
- 例外: 人が「この WorkUnit を独立した Task にしたい」と判断したときは、GUI から Task に昇格できるようにする
  （E5 の提案。本 ADR の範囲外）。

### D3. Planner（Lead）を常時は使わず、必要なときだけ起こす（Selective Activation）理由

- ADR-0069 は組織の木を「継承の名前空間」として扱い、`CoS → Engineering Lead → Software Lead → Worker` の**常時の中継をやめた**。
  中継に戻すと、小さな Task にも LLM の往復が 2〜3 回増える。費用・遅延・context の分断が増え、判断の出所が曖昧になる。
- 大半の Task は atomic で、計画は要らない。Planner を起こすのは、次の条件のどれかに当たったときだけにする。
  - (a) Complexity Gate（D13）が compound と判定した。
  - (b) replanning が必要になった（D17）。
  - (c) 人が明示した（`execution: compound`、または計画の再作成を頼んだ）。
- 起こした Planner は**task-local**。その Task の WorkUnit 計画だけを作って終わる。Worker に仕事を振るのも、結果を受けるのも daemon で、
  lead は中継をしない（D14）。
- ADR-0069 §5 が予約した「部門リードのセッションを選択的に起こす」は、この仕組みで実現する。
  - Planner は Task の担当が属する部署の lead ノードの**実効 profile**（knowledge・policy・記憶）で走る。
  - 起動条件は上の (a)〜(c) と、`TaskFeatures.cross_cutting = High`（複数の部署の skill にまたがる）。

### D4. 決定論的な scheduler を維持する理由（daemon は LLM-free のまま）

- DESIGN 原則 1（協調の判断に LLM を使わない）と原則 2（状態はエージェントの外）をそのまま守る。
- daemon がやること:
  - ExecutionPlan の実行
  - 依存の解決
  - ready queue
  - Run の終わり方の分類（D7）
  - continuation / retry / repair の判定（D11）
  - 上限の強制（D18）
  - Complexity Gate（D13）
- LLM を使うのは次の 4 役だけ。
  - Planner（意味のある分解と replanning の提案）
  - Worker
  - Reviewer
  - 任意の wrap-up run（D10）: checkpoint を書かせるだけの短い Worker run
- support-task と adapter / harness のパターンを引き継ぐ。
  - daemon は決定的に動く。
  - LLM は task / adapter 側に置く。
  - 状態は DB と events に外に出す。
  - 失敗から回復できる（再起動後は DB から続ける。D15）。
- Planner の出力は**提案**にすぎない。daemon が schema と上限で検証してから採用し（D14）、失敗しても Task を失敗させない。

### D5. データモデル: events が正本、3 つの表は派生の索引。暗黙の WorkUnit には行を作らない

DESIGN 原則 6（全イベントを追記専用で記録し、現在状態はイベントから再構成できる）に合わせる。

- **正本は `events`**（Task ごとの列）。実行層の出来事も Event として同じ列に積む。
- 表 `execution_plans` / `work_units` / `runs` は `tasks` と同じ**派生のスナップショット**。書くのは対応する Event と**同じトランザクション**。
- `replay` で events から作り直せる（E2 の受け入れ条件）。

**新しい Event**（すべて Task の状態を変えない。`replay` の attempts 計算は無視する。追加のみで、既存の JSON はそのまま読める）:

| Event | 中身 | 導入 |
|---|---|---|
| `CheckpointSaved { run_id, work_unit_id: Option<String>, checkpoint: Box<Checkpoint> }` | D8 の checkpoint（確定値。16 KiB 上限） | E1 |
| `ExecutionGated { decision: Box<ExecutionGateDecision> }` | D13 の判定と根拠 | E3 |
| `ExecutionPlanned { plan_id, version, origin, supersedes: Option<String>, reason: Option<String>, plan: Box<ExecutionPlanSpec> }` | 計画の採用。replanning も同じ Event で、`supersedes` を持つ | E2 |
| `WorkUnitTransitioned { work_unit_id, key, from, to, reason, run_id: Option<String> }` | WorkUnit の状態遷移（D6） | E2 |

**既存の Event への追加**（すべて `serde(default)` の Option。導入前の run には無い）:
- `WorkerStarted`
  - `work_unit_id: Option<String>`（E2）
  - `run_seq: Option<u32>`（WorkUnit の中の Run #n。暗黙の WU ではタスク内の連番。E1）
  - `session_id: Option<String>`（E1b の wrap-up 用）
- `WorkerFinished`
  - `end: Option<RunEnd>`（D7。E1）
  - `metrics` の `RunMetrics` に `peak_context_tokens: Option<u64>` と `turns: Option<u32>`（E1）
- `RoutingRecord`（`RoutingDecided`）: `work_unit_id: Option<String>`（E2）

**migration 0026（`SCHEMA_VERSION` 25 → 26。E2 で入れる。E1 は migration 無し）**:

```sql
-- ADR-0072 D5。3 つとも events の派生索引（正本は events）。書くのは対応する Event と同じトランザクションだけ。
CREATE TABLE IF NOT EXISTS execution_plans (
    id            TEXT PRIMARY KEY,           -- ULID
    task_id       TEXT NOT NULL,
    version       INTEGER NOT NULL,           -- 1, 2, …（replanning で +1）
    origin        TEXT NOT NULL CHECK (origin IN ('planner','human','repair','fixture')),
    planner_run_id TEXT,                      -- origin = planner のとき、その run
    status        TEXT NOT NULL CHECK (status IN ('active','superseded','completed','abandoned')),
    json          TEXT NOT NULL,              -- ExecutionPlanSpec（rationale・WorkUnit の spec・採用時の上限の写し）
    created_at    TEXT NOT NULL,
    superseded_at TEXT,
    UNIQUE (task_id, version)
);
CREATE INDEX IF NOT EXISTS idx_execution_plans_task ON execution_plans (task_id, status);

CREATE TABLE IF NOT EXISTS work_units (
    id            TEXT PRIMARY KEY,           -- ULID
    task_id       TEXT NOT NULL,
    plan_id       TEXT NOT NULL,              -- この行を作った計画の版（replan で持ち越した done の WU は元の plan_id のまま）
    key           TEXT NOT NULL,              -- 計画の中の slug（'survey'、'core-model'、'repair-1'）
    seq           INTEGER NOT NULL,           -- 計画の中の順番（トポロジカル順の tie-break）
    kind          TEXT NOT NULL,              -- investigate|design|implement|test|release|repair|other
    status        TEXT NOT NULL,              -- D6 の WorkUnit 状態
    blocked_reason TEXT,                      -- question|dependency_failed|limit（status = blocked のとき）
    depends_on_json TEXT NOT NULL DEFAULT '[]', -- key の配列
    runs          INTEGER NOT NULL DEFAULT 0,
    continuations INTEGER NOT NULL DEFAULT 0,
    retries       INTEGER NOT NULL DEFAULT 0,
    last_run_id   TEXT,
    last_checkpoint_run_id TEXT,
    json          TEXT NOT NULL,              -- WorkUnitSpec（D14）と repair_of など
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (task_id, key)
);
CREATE INDEX IF NOT EXISTS idx_work_units_task ON work_units (task_id, status);

CREATE TABLE IF NOT EXISTS runs (
    run_id        TEXT PRIMARY KEY,           -- 既存の run_id（WorkerStarted と同じ）
    task_id       TEXT NOT NULL,
    work_unit_id  TEXT,                       -- NULL = 暗黙の WorkUnit
    role          TEXT NOT NULL,              -- worker|reviewer|planner|wrap_up
    seq           INTEGER NOT NULL,           -- WorkUnit の中の Run #n
    status        TEXT NOT NULL,              -- D6 の Run 状態
    adapter TEXT, model TEXT, account TEXT, session_id TEXT,
    checkpoint_json TEXT,                     -- 確定した Checkpoint（CheckpointSaved と同じ中身）
    usage_json TEXT, metrics_json TEXT,
    started_at    TEXT NOT NULL,
    finished_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_runs_task ON runs (task_id, started_at);
CREATE INDEX IF NOT EXISTS idx_runs_work_unit ON runs (work_unit_id);
```

- `runs` は E2 以降、**全タスクの** worker / reviewer / planner run について書く。atomic なタスクの run も含む（metrics の集計を
  events の JSON 走査から外すため）。E2 より前の run は埋め戻さない。表示は従来どおり `view.rs` が events から作る。
- `work_units.key` は Task の中で一意にする。replan で持ち越した done の WU は同じ行のまま使う。新しい版で消えた未完了の
  WU は `superseded` にする。
- **暗黙の WorkUnit**: `execution_plans` の行を持たない Task は、`work_unit_id = NULL` の Run だけを持つ。E1 の continuation の
  回数や最新の checkpoint は events から数える（`consecutive_requeues` と同じ形の純粋関数）。行が必要になるのは、repair（D16）で
  初めて WorkUnit を足すときだけ。そのときに、暗黙の WU を `key = "main"`（status `done`）として実体化する
  （`execution_plans.origin = 'repair'`）。

### D6. 状態機械

**Run**（`runs.status` と `WorkerFinished.end`。D7 の分類そのもの）:

```
running ─┬─> completed        （Terminal::Done）
         ├─> yielded          （result.json の yield。自分から区切った）
         ├─> budget_exhausted （turn / wall / context の上限に当たった）
         ├─> question         （Terminal::Question）
         ├─> failed           （ワーカー自身の error。結果ファイルが不正な場合を含む）
         ├─> harness_error    （供給側の失敗 = Requeue、インフラ = InfraRequeue、lease の失効）
         └─> cancelled        （Cancel / Interrupt / 古い lease の結果を捨てた）
```

**WorkUnit**（`work_units.status`。E2 以降、計画のある Task だけ）:

| from | 事象 | to | 回数の欄 |
|---|---|---|---|
| pending | 依存がすべて done | ready | — |
| ready / needs_continuation | scheduler がこの WU の Run を起動 | running | runs+1 |
| running | Run completed（任意の決定的検査も pass。E4） | done | — |
| running | Run budget_exhausted / yielded、上限内 | needs_continuation | continuations+1 |
| running | Run failed、`retries < max` | ready | retries+1 |
| running | Run failed、上限に到達 | failed | — |
| running | Run question | blocked（question） | — |
| running | Run harness_error | ready、checkpoint があれば needs_continuation | 数えない（ADR-0010 / 0070 の既存カウンタ） |
| running | continuation の上限、または進捗なし（D18） | blocked（limit） | — |
| pending / ready | 依存先が failed | blocked（dependency_failed） | — |
| blocked | 人の回答（`Answer`） | ready / needs_continuation | 上限の窓を 0 に戻す |
| 未完了すべて | replanning で消えた | superseded | — |
| 未完了すべて | Task の中止 | cancelled | — |

**Task**:
- `Status` は**増やさない**（draft / ready / running / blocked / reviewing / done / failed / cancelled のまま。理由は §5 の R3）。
- 人の依頼にある Task の状態名（planning / executing / verifying / needs_human / done / failed）は、表示用の**導出値**
  `ExecutionPhase` として作る（純粋関数 `task_core::execution::phase(&Task, Option<&ExecutionSnapshot>)`）。

| 表示（ExecutionPhase） | 実体 |
|---|---|
| direct | 計画を持たない Task（従来と同じ見え方） |
| planning | Running で、実行中の run の role が planner。または Ready で、次の run が planner |
| executing | Ready / Running で、計画に未完了の WU がある |
| verifying | Reviewing |
| repairing | Ready / Running で、次（または実行中）の WU が repair |
| needs_human | Blocked |
| done / failed / cancelled | 同じ名前の Status |

**新しい `Trigger`**（`crates/task-core/src/transition.rs`。どちらも attempts を**変えない**。`replay` が attempts を数える理由の集合
`{worker_error, lease_expired, review_fail}`（`crates/task-ops/src/replay.rs:48`）に入らないので、attempts の復元にも影響しない）:

| Trigger | 遷移 | `Transitioned.reason` | 使う場面 |
|---|---|---|---|
| `Continue { why: ContinueWhy }` | Running → Ready | `why` ごとの静的な名前: `continue` / `advance` / `work_unit_retry` / `planned` / `replan` | continue = budget_exhausted か yielded の続き（E1）。advance = WU が done で、未完了の WU が残る（E2）。work_unit_retry = WU の失敗で、WU の retry が残る（E2）。planned = planner run が有効な計画を出した、または atomic に倒した（E3）。replan = 次の run を planner（replan モード）にする（E4） |
| `ReviewRepair` | Reviewing → Ready | `review_repair` | 修復できる不合格（D16）で、repair の上限内（E4） |

既存の Trigger との対応（変えない）:
- Run completed で、それが最後の WU（または atomic）なら `WorkerDone`（Running → Reviewing）。
- Run question は `WorkerQuestion`（Running → Blocked）。
- 上限到達・進捗なしは、dispatcher が出す質問として `WorkerQuestion` に `QuestionRaised`（ADR-0021 D2 の形）を添え、Blocked にする。
- WU の failed で replan できないときは `WorkerError{retryable:false}`（Running → Failed、outcome は `"work unit <key> failed: …"`）。
- supply / infra は従来どおり `Requeue` / `InfraRequeue`。
- 最終レビューの修復できない不合格は、従来どおり `ReviewFail`（`retry_or_fail`、attempts+1）。
- `Answer` / `Cancel` / `Reopen` / `Rereview` の意味も変えない。

**並列度（初期版）: 1 Task の中では WorkUnit を直列に実行する。** 理由:
- (1) すべての WU が Task の worktree とブランチを共有する（同時に編集すると衝突する）。
- (2) dispatcher の `running` と lease は TaskId が key（調査 §0-1）。直列なら、既存の lease・reclaim・drain・`InfraRequeue`・
  `abort_stale_runs` がそのまま使える。
- Task をまたいだ並列は従来どおり `max_concurrency` で効く。
- Task の中での並列は、読み取り専用の WU（investigate）から始めるのが自然。WU ごとの lease と別の worktree が要るので、
  別 ADR にする（§7 U3）。

### D7. Run の終わり方の分類（`RunEnd`）

`task_core::execution::RunEnd`（serde の tag 付き enum。`WorkerFinished.end` に入れる）:

```rust
pub enum RunEnd {
    Completed,
    Yielded,
    BudgetExhausted { kind: BudgetKind },      // Turns | WallClock | Context
    Question,
    Failed { retryable: bool },
    HarnessError { class: HarnessErrorClass }, // Supply | Infra | LeaseExpired | IdleTimeout
    Cancelled,
}
```

**アダプタ側の変更**（`crates/task-worker/src/adapter.rs` の `Terminal` に 2 つ足す）:
- `Terminal::Yielded { checkpoint: serde_json::Value, usage: Option<Usage> }`
- `Terminal::BudgetExhausted { kind: BudgetKind, message: String, usage: Option<Usage> }`

**予算切れでも usage を運ぶ**。今は捨てているので metrics が欠ける（調査 §11）。これを直す。

| harness | budget_exhausted の検出 | yielded の検出 |
|---|---|---|
| claude-code | `result.subtype == "error_max_turns"` → Turns。wall-clock の打ち切り（`claude_code.rs:907-914`）→ WallClock。`result` の文言か stderr が context 超過（`prompt is too long` / `context window` / `context_length`。**実機の文言は未確認**、§7 U1）→ Context | result.json に `yield` があり、subtype が success |
| codex | wall-clock の打ち切り → WallClock。`turn.failed` の文言が context 超過（`context_length_exceeded` 等。**未確認**）→ Context。turn の上限は無い（調査 §4） | 同上 |
| acp | wall-clock → WallClock。`stopReason == "max_turn_requests"` → Turns（**未確認**） | 同上 |
| aider | wall-clock → WallClock | 同上 |
| paperqa / LDR / langmem | 対象外（アダプタ自身が結果を書く固定パイプライン。従来どおり `Error`） | 対象外 |

- **二重の安全網**: 構造化した `Terminal` を返さないアダプタや古い経路のために、daemon は `Terminal::Error.message` を
  `retry_policy::is_budget_outcome`（`retry_policy.rs:251`。E1 で pub にし、context 超過の語を足す）で字句判定し、
  `BudgetExhausted` に写す。
- `idle timeout`（無出力）は harness_error（IdleTimeout）に分類する。ただし E1 では**遷移を変えない**（従来どおり
  `WorkerError{retryable:true}` で attempts を消費する。§7 U7）。

### D8. checkpoint の意味論と JSON schema

**意味**: 「この Run が終わった時点で、次の Run が**会話なしで**続けるのに要る最小の状態」。人に見せる報告ではない（報告は ADR-0034 の
経路のまま）。DESIGN 原則 2（状態はエージェントの外に置く）の実装である。

**作られ方**（3 つの出所を daemon が決定的に合成する。出所は `source` に残す）:
1. **worker**: ワーカーが run 中に `<artifacts_dir>/checkpoint.json` を**書き足していく**（プロンプトで指示する。D10）。予算切れで
   強制終了されても、最後に書いたものが残る。
2. **yield / done**: result.json の `yield`（D10）、または done の result.json に任意で添える `checkpoint`（WU の最終状態。依存する
   WU への引き継ぎに使う）。1 より優先する。
3. **mechanical**: daemon（`run_worker` の後段。spawn 済みの task の中で行う I/O で、tick は止めない）が決定的に集める。
   - `repo_state`: `git rev-parse` / `git status --porcelain` / `git diff --stat <base>`
   - `files_changed`: `git diff --name-status <base>` と未追跡のファイル
   - `tests_run`: この run の `WorkerProgress{kind: tool_use}` のうちテストのコマンドに当たるもの、最大 10 件
   - `recent_activity`: 直近の tool_use 20 行

   local の worktree だけが対象。remote は E1 では 1 と 2 だけにする（§7 U9）。

**合成の規則**:
- 意味の欄（completed / remaining / decisions / known_failures / next_action / open_questions / plan_issue）は worker の値を採る。
- 事実の欄（repo_state / files_changed のパス集合）は mechanical を正とし、worker の注記を path で添える。
- tests_run は和集合を取る（コマンドで重複を除く）。
- worker の checkpoint が無い、または schema 違反のときは mechanical だけで作る（`source = "mechanical"`）。その場合
  `remaining = ["（checkpoint がありません）元の目的を続けてください"]`、`next_action = "git diff で現状を確認して続きから"` とする。

**JSON schema `celeris.checkpoint/1`**（`docs/protocol/checkpoint.schema.json` を schemars から生成する。E1）:

```json
{
  "schema": "celeris.checkpoint/1",
  "task_id": "01M…",
  "work_unit": "core-model",            // 暗黙の WU は null
  "run_id": "01M…",
  "run_seq": 2,
  "end": "budget_exhausted",            // completed | yielded | budget_exhausted（D7。daemon が入れる）
  "source": "merged",                   // worker | yield | mechanical | merged（daemon が入れる）
  "completed":     ["store に work_units を追加し、単体テスト 12 本を通した"],
  "remaining":     ["dispatcher の ready queue への配線", "restart 復元のテスト"],
  "decisions":     [{"what": "WU は直列実行", "why": "worktree 共有と TaskId キーの lease"}],
  "files_changed": [{"path": "crates/task-core/src/execution.rs", "change": "added", "note": "型と純粋関数"}],
  "tests_run":     [{"command": "cargo test -p task-core execution", "exit": 0, "summary": "12 passed"}],
  "known_failures":[{"what": "clippy の needless_borrow 1 件", "detail": "execution.rs:210"}],
  "artifact_refs": [{"path": "artifacts/design-notes.md", "kind": "doc"}],
  "next_action":   "dispatcher.rs の dispatch_ready で next_work_unit を呼ぶ",
  "open_questions":[],
  "plan_issue":    null,                // 計画そのものが誤っていると気づいたときの 1 文（D17 の replanning を起こす）
  "repo_state":    {"branch": "celeris/01M…", "base": "main@1a2b3c4", "head": "9f8e7d6", "uncommitted": true,
                    "diff_stat": "5 files changed, 420 insertions(+), 12 deletions(-)"},
  "recent_activity": ["Bash: cargo test -p task-core", "Edit: crates/task-core/src/execution.rs"],
  "created_at": "2026-09-24T12:00:00Z"
}
```

- 人の依頼にある必須 8 欄: `completed / remaining / decisions / files_changed / tests_run / known_failures / artifact_refs / next_action`。
- 追加した欄: `schema, task_id, work_unit, run_id, run_seq, end, source, open_questions, plan_issue, repo_state, recent_activity, created_at`。
- 上限: 配列はそれぞれ 30 件、文字列はそれぞれ 500 文字（超えたら切り詰めて `…` を付ける）、全体は 16 KiB。切り詰めは決定的に行う。
  `CheckpointSaved` の event と `runs/<run_id>/checkpoint.json`（全文）の両方に残す。
- ワーカー側の型は `deny_unknown_fields` にしない（寛容に読む。未知の欄は捨てる）。daemon が書く確定値は厳密な型にする。

### D9. continuation（新しい context での再開）と、ADR-0054 の resume との使い分け

**規則**: 「予算切れ・yield の続きは、**checkpoint から新しい session**で始める。resume はしない」。

理由:
- 予算切れの session を resume すると、会話全体が戻る。context は縮まらず、次の run も上限の近くから始まってまた尽きる。
  continuation の目的は context の圧縮なので、resume とは目的が逆になる。
- 仕事の run はもともとセッションを持たない（`--no-session-persistence`。調査 §0-1）。ステートレスなワーカー（DESIGN 原則 2）の
  延長として checkpoint を足すのが最小の変更になる。
- ADR-0054 の継続セッション（`node_sessions`）は、長期に続く「人」（CoS / 部門長）のための仕組みで、単位はノード。WorkUnit の
  Run に流用すると、rollover・アカウント固定（Phase 67c）・self-heal の意味が混ざる。

resume を使うのは次の 2 つだけにする。
- (a) **wrap-up run**（E1b、任意、既定はオフ）: 予算切れで worker の checkpoint が無い（または古い）とき、harness が resume できて
  （claude-code / codex）、切れた理由が Turns / WallClock（Context ではない）なら、同じ session を 3 turn だけ resume して
  「checkpoint.json を書いて yield せよ」と頼む。session id は `runs.session_id` / `WorkerStarted.session_id` に置く。
  node_sessions は使わない。
- (b) 将来の選択肢（採らない）: `InfraRequeue`（context が尽きていない中断）で同じ session を resume する。今は ADR-0070 D3 の
  「同じ worktree で attempts を消費せずにもう一度動かす」のまま。

**continuation の prompt の組み立て**（`RunContext.continuation: Option<ContinuationContext>` を新設し、`preamble::render` が描く。
`None` の run のプロンプトは現状とバイト単位で同じ）:

```
## 続きの実行（Run #3 / WorkUnit core-model）
これは新しい session です。前の Run の会話は引き継がれていません。前の Run は budget_exhausted（max_turns=40）で終わりました。
下の checkpoint と作業ツリーの現状から再開してください。完了済みの作業はやり直さないこと。
最初に `git status` と `git diff --stat` で現状を確かめてください。
### checkpoint（Run #2 の終わり）
- 完了: …   - 残り: …   - 決めたこと: …   - 変えたファイル: …   - 実行したテスト: …   - 既知の失敗: …
- 次の一手: …
### これまでの Run（1 行ずつ）
- Run #1 budget_exhausted（40 turns、tokens in/out …）  - Run #2 budget_exhausted（…）
```

- `ContinuationContext{ run_seq, previous_end, checkpoint, prior_runs: Vec<String>（1 run 1 行、最大 10）, work_unit: Option<WorkUnitBrief>,
  plan_overview: Vec<String>（WU ごとの key・title・status を 1 行）, dependency_checkpoints: Vec<Checkpoint の要約>（依存する WU の最終 checkpoint の completed / decisions / artifact_refs） }`。
- 計画のある Task の WU の run では、`## Objective` を **WorkUnit の objective** に差し替える。Task 全体の目的は
  「## このタスク全体の目的（参考）」として先頭 1,500 文字だけ載せる。受け入れ条件は WU の `done_when` と、
  「最終レビューで確かめる Task の受け入れ条件（参考）」の 2 つを載せる。
- 既存の文脈（`prior_review`、`answers`、`comments`、`knowledge`、`profile`）はそのまま載せる。
- `result.json` との関係: 終端の契約は result.json のまま（ADR-0006 D3）。追加するのは次の 2 つだけ。どちらも任意。
  - `{"yield": {<checkpoint の意味の欄>}}` → `Terminal::Yielded`
  - `{"summary": …, "checkpoint": {…}}` → done のときの WU の最終 checkpoint
- 優先順位は `result.json.yield` > `checkpoint.json` > mechanical。

### D10. graceful yield（予算が尽きる前に自分で区切る）

全 harness の共通（E1）: coding 系の execute run（claude-code / codex / acp / aider。対話・レビュー・計画・研究 harness は除く）の
プリアンブルに、次の 2 つを足す。

1. **予算の予告**
   - 「この run の上限: 最大 {max_turns} turn / {max_wall_secs} 秒（開始 {started_at} UTC）」
   - 「残りが約 20% になったら、または context が長くなってきたと感じたら、作業を区切りのよい所で止め、checkpoint を書いて
     result.json に `yield` を書いて終了せよ。予算切れで打ち切られるより、区切って止まる方が良い」
   - claude-code 以外は turn の上限が無いので、wall の文だけを出す。
2. **rolling checkpoint**: 「意味のある区切り（1 つの小目標の完了、方針の決定、テストの実行）のたびに `<artifacts>/checkpoint.json` を
   上書きせよ」（schema の例を載せる）。

harness ごとの可否:

| harness | 実行中の予告 | 真の graceful yield | E1 | E1b |
|---|---|---|---|---|
| claude-code | できない（`-p` で stdin は null。`claude_code.rs:855`） | 近似: 静的な予告 + rolling checkpoint。wrap-up（D9 (a)）で短い resume をして事後に yield させる | 静的な予告 + rolling | wrap-up（`--session-id` を仕事の run でも付ける） |
| codex | できない（`exec`） | 同上。wrap-up は `exec resume` | 同上 | wrap-up |
| acp | **できる**（ACP の `session/cancel` の後、同じ session に `session/prompt` を送る。調査 §4） | adapter が wall の 80% で cancel し、「checkpoint を書いて yield せよ」と追加の prompt を送る | 静的な予告 + rolling | 真の yield |
| aider | できない（`--message` 1 回） | 無理。mechanical だけ | 静的な予告 | — |

### D11. retry / continuation / repair / replan の違い

| | 何が起きたか | 何をやり直すか | context | 数える欄と上限 | attempts |
|---|---|---|---|---|---|
| **continuation** | session の予算切れ、または自分での yield（仕事はまだ正しく進んでいる） | 同じ WU の**続き** | 新しい session + checkpoint | `continuations`（WU ごと、既定 3）、進捗なし 2 回で停止 | 消費しない |
| **retry** | Run が**失敗**した（ワーカーの error、結果が不正） | 同じ WU を**最初から**（同じ worktree） | 新しい session。直前の checkpoint があれば添える | atomic では Task の `attempts`（従来どおり）。計画のある Task では WU の `retries`（既定 = `budget.max_retries`） | atomic: +1。計画のある Task: 消費しない（WU の retries で数える） |
| **requeue / infra requeue** | 供給側・インフラの都合（仕事の失敗ではない） | 同じ Run を**もう一度** | 新しい session | 既存の `max_requeues` / `max_infra_retries` | 消費しない（従来どおり） |
| **repair** | 最終レビューの**修復できる**不合格（fmt / lint / 小さな test / merge-base） | 失敗した点だけを直す**新しい WU**（`kind = repair`） | 最小の context（D16）。元の実装の context は作り直さない | `repairs`（Task ごと、既定 3。種類ごとに 2） | 消費しない |
| **replan** | 計画そのものが誤っている（WU の failed・進捗なし・`plan_issue`・実質的な review 不合格） | 計画を**直す**（done の WU は保持する） | planner（replan モード） | `replans`（Task ごと、既定 3） | review 不合格が起点なら +1（`ReviewFail` のまま）。それ以外は消費しない |

### D12. Task が終端として失敗する（terminal failure）条件

**Task が `failed` になるのは、次のどれかに当たったときだけ**（それ以外の「行き詰まり」は、失敗にせず `blocked` で人に聞く）:

1. 最終レビューの**修復できない**不合格が `max_retries` を使い切った（従来の `ReviewFail` → `retry_or_fail`）。
2. ワーカーが**やり直しても直らない** error を返した（`Terminal::Error{retryable:false}`。従来どおり）。
3. 計画のある Task で、WU が `failed`（retry の上限）になり、かつ replan できない（上限に到達・planner が有効な計画を出せない・
   人が replan を拒んだ）。
4. インフラ・供給側の再試行の上限（ADR-0070 D3 / ADR-0010。従来どおり）。
5. 人の中止（`cancelled`）・Approval の reject（従来どおり）。

**失敗にしないもの**:
- 予算切れ（turn / wall / context）
- continuation の上限到達
- 進捗なし
- Task の総予算（run 数・トークン・壁時計）の超過
- planner の出力が不正（atomic に倒す）

これらはすべて `WorkerQuestion`（dispatcher が出す質問）で `blocked` にし、受信箱に出す。質問文には「予算を増やして続ける /
分割し直す（replan） / 中止」の選択肢を書く。
`classify_task_failure` の分類（ADR-0070 D1）は変えない。3 の outcome には接頭辞 `"work unit <key> failed: "` を付ける（class は work）。

### D13. Complexity Gate（atomic / compound の決定的な判定）

- **いつ**: `dispatch_ready` の中で、`assign_if_needed` の後、`decide_lane` の前。Task の最初の dispatch で 1 回だけ判定する
  （`ExecutionGated` が無く、計画も無い Task）。
- **記録**:
  - `Event::ExecutionGated` と `Task.routing.execution: Option<ExecutionGateDecision>`（`TaskRouting` に serde(default) で追加。
    `WorkspaceModeDowngraded` と同じく、決定的な書き戻し）。
  - 中身は `ExecutionGateDecision { mode: Atomic|Compound, source: Policy|Human|Hint, score, threshold, rule_id,
    signals: Vec<GateSignal{name, weight, detail}>, policy_version: "exec-gate/1", shadow: bool }`。
- **対象外（常に atomic。rule_id `atomic/out-of-scope`）**:
  - `kind != Execute`
  - 対話（`is_conversation`）
  - support-task（`support_kind` が Some）
  - `routing` の無い Phase 114 より前のタスク
  - 固定パイプラインの harness（literature / web-research / knowledge、すなわち paperqa / LDR / langmem）
  - `workspace_mode = Shared` の内部タスク
- **優先順位**: 人の明示（`NewTaskSpec.execution = atomic|compound`、出自が Human のときだけ）> 規則表。CoS（Agent）が書いた
  `execution` は**ヒント**として +2 を足すだけ（ADR-0069 D1 と同じ考え方）。
- **規則表**（`TaskFeatures::infer_with_hints` を**そのまま**使い、足りない信号だけを `ExecutionSignals::from_task` で足す）:

| # | 信号 | 条件 | 重み |
|---|---|---|---|
| F1 | context_size | High / Medium | +2 / +1 |
| F2 | expected_length | High | +2 |
| F3 | tool_intensity | High | +1 |
| F4 | cross_cutting | High / Medium | +2 / +1 |
| F5 | 判断と実装が混ざる | judgment = High かつ tool_intensity ≥ Medium | +1 |
| S1 | 工程の数 | 題名 + 目的に出てくる工程語の種類（調査・investigate・survey / 設計・design / 実装・implement / テスト・test / review・レビュー / release・リリース・deploy・配送）が 3 以上 / 2 | +2 / +1 |
| S2 | 独立した成果物 | 受け入れ条件 6 件以上、または `ArtifactExists` 3 件以上 | +1 |
| S3 | 途中に人・reviewer の関門 | `Check::Human` と `Check::Command` の両方がある | +1 |
| S4 | 複数の実行環境 | remote の workspace と local の repos が混在、または skills が 2 つ以上の部署にまたがる | +1 |
| S5 | 目的の長さ | 2,000 文字超（4,000 超は F1 に含まれる） | +1 |
| S6 | 過去の類似タスク | 同じ担当ノードかつ同じ genre の、直近で終端になったタスク 20 件（14 日以内）で、`budget_exhausted` の Run か continuation を持ったタスクが 30% 以上 | +2 |
| H | CoS のヒント | Agent が `execution: compound` を書いた | +2 |

- **判定**: `score ≥ 5` なら compound（rule_id `compound/score`）、それ未満は atomic（`atomic/score`）。
  - 強制規則: `expected_length = High` かつ `cross_cutting = High` なら compound（`compound/long-and-broad`）。
  - `max_turns ≤ 10` かつ目的が 400 文字未満なら atomic（`atomic/small`）。
- S6 は daemon が store を決定的に読む（`runs` の索引。E2 以降。E3 の時点では events から数える実装でもよい）。
- **運用モード**: `[execution] gate = "off" | "shadow" | "on"`。
  - E3 の既定は `shadow`（判定と記録だけをして、計画は作らない。metrics で閾値を調整する）。
  - E6 の dogfood の結果を見て、人が `on` に切り替える。
  - `shadow` のときも `ExecutionGateDecision.shadow = true` として記録する。
- ML の分類器は使わない。ADR-0069 §5 の shadow classifier の枠と同じく、後で並べて記録できる。

### D14. Planner（task-local、structured schema、検証）

- **起動**:
  - gate が compound と判定した Task（`gate = on`）は、最初の run を planner run にする。この run は `runs.role = planner`、
    `WorkerStarted.role = Some(Planner)`（`RunRole` に Planner を追加）。
  - replan（D17）でも planner run を起こす。
  - planner run も Task の lease の下で走る（D6 の直列規則）。
- **誰として走るか**（Selective Lead Activation）:
  - 実効 profile は、Task の担当が属する部署の**lead ノード**（`task_core::department_of`）のもの（knowledge のマウント・
    standing rules・記憶）。部署が無ければ Task の担当のもの。
  - lead の**継続セッション（node_sessions）は resume しない**（D9 と同じく、計画は新しい context で作る）。
  - 担当とモデルは選ばない（ADR-0069 D1）。
- **harness と lane**:
  - `[execution.planner]` で adapter（既定は claude-code）、`permission_mode`（読み取り中心。既定は `plan`）、`max_turns`（既定 40）、
    `max_wall_secs`（既定 1,200）を決める。
  - lane は `TierSource::System` の frontier で、組織の天井で丸めない（計画 run の従来の扱いと同じ）。
- **入力**:
  - Task の title / objective / acceptance / repos
  - `TaskFeatures` と gate の signals
  - 知識の索引（「大きな PoC は分割せよ」などの経験知が出る）
  - 上限（D18）、出力の schema と例
  - replan のときは、今の計画、WU の状態、最新の checkpoint、失敗の理由
- **出力**: `<artifacts>/execution-plan.json`、schema は `celeris.execution-plan/1`（`docs/protocol/execution-plan.schema.json`。
  schemars から生成し、`plan-output.schema.json` と同じくテストで一致を確かめる）:

```json
{
  "schema": "celeris.execution-plan/1",
  "rationale": "調査→設計→実装→検証の 4 工程で、各工程は 1 context に収まる",
  "work_units": [
    {
      "key": "survey", "kind": "investigate",
      "title": "現行の dispatch と review の調査", "objective": "…",
      "depends_on": [],
      "done_when": ["docs/…md に file:line 付きで現状が書かれている"],
      "checks": [{"cmd": "test -s docs/survey.md", "expect_exit": 0}],
      "context": {"paths": ["crates/task-dispatch/src/"], "from_work_units": [], "knowledge": []},
      "harness": null,
      "features": {"judgment": "high"},
      "budget": {"max_turns": 40, "max_wall_secs": 1800},
      "outputs": ["docs/survey.md"]
    }
  ]
}
```

  - `deny_unknown_fields`。`assignee` / `tier` / `model` の欄は**持たない**（書かれていれば schema 違反になる）。
  - `harness` は `[[genres]]` にある id だけを許す。担当の profile が許す harness に限る。
  - `checks` は `Command` だけ（決定的な検査。E4 で使う）。
- **daemon の検証**（純粋関数 `task_core::execution_plan::validate`）:
  - 件数は 1..=`max_work_units`（既定 8）
  - key は `[a-z0-9-]{1,32}` で一意
  - `depends_on` は既知の key だけで、**循環を拒否**する（トポロジカルソート）
  - 重複の検出: title を正規化して一致、または objective のトークン集合の Jaccard ≥ 0.9 なら拒否する
  - 入れ子の計画は禁止（固定 2 層。WU は計画を持たない）
  - `budget` は上限（WU あたり `max_turns ≤ 80`、`max_wall_secs ≤ 3600`）で丸める（丸めたことを記録する）
  - replan のときは、done の WU の key と spec が**変わっていない**こと（done は不変）
- **不正なとき**: 検証エラーを添えて planner をもう 1 回だけ実行する（DESIGN §5.6 の「1 回だけ再試行」）。それでも不正なら
  **atomic に倒す**（暗黙の WU で実行する。`Continue{planned}` と、進行ログに「計画を採用できず直接実行に切り替えた: <理由>」を残す）。
  Task は失敗させない（DESIGN §5.6 は「不正なら failed」だが、これは `Plan` kind の子 Task 生成の規則。ここは内部の計画なので安全側に
  倒す。§6 の「SPEC / DESIGN との関係」）。
- 採用するときは、`ExecutionPlanned` と `execution_plans` / `work_units` の行を同じトランザクションで書く。planner run 自身の
  完了は `Continue{planned}`（Running → Ready）。

### D15. scheduler（決定的。ready queue・依存・伝播・再起動からの復元）

- **ready queue は 2 段**:
  - Task の段: 既存の `ready_tasks`（変えない）。
  - WU の段: dispatch が決まった Task について、純粋関数 `next_work_unit(&[WorkUnit]) -> NextStep` で次を選ぶ。
    - `RunWorkUnit(id)`: needs_continuation を優先し、次に ready を `seq` 順。直前の WU の続きを先に片付けると context の連続性が良い。
    - `RunPlanner{replan}`
    - `AllDone`
    - `Stuck(reason)`
- **依存**: WU の `depends_on` がすべて done になったら、pending → ready。判定は WU の遷移のたびに同じトランザクションで決定的に行う。
- **伝播**:
  - WU が failed になると、それに依存する未着手の WU を blocked（dependency_failed）にし、replan を起こす（D17）。replan で
    計画が直れば、superseded / ready に移る。
  - Task の Cancel は、未完了の WU をすべて cancelled にする。
  - WU の question は、Task を `WorkerQuestion` で blocked にする。人の回答（`Answer`）で WU は ready に戻る。
- **Task の完了**: 最後の WU が done になったら `WorkerDone`。`ReviewSubject.summary` には、WU ごとの最終 checkpoint の
  `completed` を key ごとに 1 段落ずつ並べた決定的な要約を入れる。evidence は最後の run のもの。
- **再起動からの復元**: scheduler の状態はすべて DB にある（メモリ上に queue を持たない）。
  - 実行中に落ちた run は、既存の lease 失効 → `reclaim_expired_leases` → `InfraRequeue` の経路で Task が Ready に戻る。
  - WU の `running` は、tick の最初に照合する。Task が Running でない、または lease の run_id が `last_run_id` と違うなら、
    checkpoint があれば needs_continuation、無ければ ready に戻す（`WorkUnitTransitioned{reason: "restart_reconcile"}`）。
  - `infra_backoff` がメモリだけにあるのは従来どおり（P-116-2）。
- **atomic な Task の経路は変えない**: 計画を持たない Task は `next_work_unit` を通らない。E1 の continuation だけが効く。

### D16. reviewer repair（不合格を局所的に修復する）

**分類**（純粋関数 `task_core::execution::classify_review_failure(&Task, &[ReviewVerdict], &[Criterion]) -> RepairDecision`）。
不合格の verdict をすべて見て、**全部が修復できる種類**のときだけ repair にする（1 つでも実質的な不合格があれば repair しない）。

| class | 条件（決定的） | repair の予算・lane |
|---|---|---|
| `format` | `Check::Command`（または `workspace.toml` の check）の失敗で、cmd に `cargo fmt` / `rustfmt` / `prettier` / `biome format` / `gofmt` / `black` / `ruff format` を含む | max_turns 12、wall 600、cheap |
| `lint` | cmd に `clippy` / `eslint` / `biome check` / `biome lint` / `ruff check` / `tsc` / `typecheck` を含む | max_turns 20、wall 1200、standard |
| `test_small` | cmd がテストの実行（`cargo test` / `pnpm test` / `vitest` / `pytest` / `go test`）で、tail から数えた失敗が 1〜3 件（`test result: FAILED. N passed; M failed`、`M failed` 等の字句） | max_turns 30、wall 1800、standard |
| `reviewer_local` | `Check::Reviewer` の不合格で、`review.json` の verdict に任意の `repair: {"scope":"local","class":"format\|lint\|test\|doc\|other","hint":"…"}` がある。または reason が fmt / clippy の語だけを指す（字句の予備判定） | class に応じる |
| `merge_base` | 配送の `[delivery-repair]`（`crates/celeris/src/delivery.rs:338-353`）が扱う取り込みの技術的な失敗（base の移動・衝突・ビルド） | max_turns 30、wall 1800、standard |
| `substantive` | 上のどれでもない（`Check::Human` の不合格を含む） | repair しない。計画があり replan の余地があれば replan（D17）、それ以外は従来の `ReviewFail` |

**repair WorkUnit の生成**:
- `ReviewRepair`（Reviewing → Ready、attempts は変えない）と、`work_units` への `kind = repair`、`key = repair-<n>` の追加を**同じ
  トランザクション**で行う。atomic な Task では、このときに暗黙の WU を `main`（done）として実体化する（D5）。
- **元の実装の context は作り直さない**。repair の run に渡すのは次のものだけ。
  - repair 専用の objective（テンプレート固定）: 「次の検査が失敗した。**失敗を直すことだけ**をせよ。設計や他のコードは変えるな。
    直した後に同じコマンドを実行して exit を確かめよ」
  - 失敗した検査: cmd / exit / stdout と stderr の tail。`ReviewVerdict.reason` そのもの（`review.rs:286-305`）
  - Task の title と目的の先頭 600 文字
  - 最新の WU の checkpoint の `decisions` / `files_changed`（上限あり）
  - `git diff --stat`

  Task の全文の objective、計画、以前の Run の履歴は**載せない**。
- repair の run が done になったら、`WorkerDone` → Reviewing に戻り、再判定する。
  - `ReviewRepair` は attempts を変えないので、`Check::Human` の Approval 子タスクの title（`human_approval_title` が
    `attempts + 1` を含む）が一致し、**承認済みの人の判定は再利用される**（ADR-0054 Phase 113 D3 と同じ性質）。
  - 決定的な検査と reviewer run はもう一度走る。
- **上限**: Task ごとに `max_repairs` = 3、同じ class は 2 回まで。超えたら従来の `ReviewFail`（attempts を消費する）に戻る。
- **配送の repair（E4 の後半、任意）**: `[delivery-repair]` の `Trigger::Reopen`（タスク全体の再実行）を、「Reopen + `merge_base` の
  repair WU を同じトランザクションで作る」に変える。Reopen 後の dispatch は、残っている repair WU だけを走らせる。

### D17. replanning（誰が・いつ・どう監査するか）

- **起こすもの**（すべて daemon が決定的に判定する）:
  1. WU が `failed` になった（retry の上限）。
  2. WU が進捗なしで止まった（D18）。
  3. checkpoint に `plan_issue` が書かれた（ワーカーが「計画が誤っている」と申告した。例: 「migration M が先に要る」）。
  4. 計画のある Task の最終レビューで、実質的な不合格が出た（`ReviewFail` の後の最初の dispatch）。
  5. 人の依頼（`POST /tasks/{id}/execution-plan/replan`、または GUI のボタン。E5）。
- **誰が**:
  - 新しい計画の**案**は planner run（D14、replan モード）が作る。入力は今の計画、WU の状態、checkpoint、起こした理由。
  - daemon が検証して採用する。done の WU は不変で、持ち越す。
  - 人は `PUT /tasks/{id}/execution-plan`（origin human）で直接書き換えてもよい。
- **形**: planner は差分ではなく**新しい版の全体**を出す。daemon が旧版との差を計算し、次の 3 つに分ける。
  - 追加: 新しい key
  - 変更: 未完了の WU で spec が変わったもの
  - 削除: 未完了の WU で新しい版に無いもの。superseded にする

  例（人の依頼の例）: A は done、M（migration）を追加、B は blocked_by M（`depends_on: ["m"]`）、C は blocked_by B。
- **監査**:
  - `ExecutionPlanned{version: n+1, supersedes: <旧 plan_id>, reason, plan}` を残す。
  - 旧版の行は `superseded`。消した WU は `WorkUnitTransitioned{to: superseded, reason: "replan v<n+1>"}`。
  - GUI の Execution 節に、版の履歴（版・理由・差分の件数・作ったもの）を出す。
- **上限**: Task ごとに `max_replans` = 3。超えたら blocked（人に聞く）。replan の planner run 自身は Run の上限（`max_runs_per_task`）
  にも数える。

### D18. 上限（runaway decomposition の防止）と既定値

`[execution]`（`crates/celeris/src/config.rs`。すべて任意、既定値は下の表）。組織の profile の `budget` で狭められるのは
`max_runs_per_task` と `max_task_tokens` だけ（最も厳しい値を採る。ADR-0069 D2 と同じ規則）。

| 設定 | 既定 | 意味 | 超えたとき |
|---|---|---|---|
| `continuation` | `true` | E1 の continuation を有効にする（`false` で従来の挙動） | — |
| `max_continuations_per_work_unit` | 3 | 1 つの WU の Run は最大 4 回 | blocked（limit）→ 人に聞く |
| `no_progress_limit` | 2 | 進捗なしの continuation が連続 2 回で止める。進捗の定義は次の 3 つのどれも起きないこと: `files_changed` のパス集合か HEAD が変わる / `completed` が増える / `remaining` が減る | blocked（limit）→ replan が可能なら replan |
| `max_work_units` | 8 | 1 つの計画の WU の数（repair を除く） | 計画の検証で拒否 |
| `max_total_work_units` | 16 | Task の生涯で作る WU の数（repair と replan で足したものを含む） | replan を拒否 → blocked |
| `max_planning_depth` | 1（固定） | Task → ExecutionPlan → WorkUnit → Run の固定 2 層。WU は計画を持てない | 検証で拒否 |
| WU の `max_retries` | `task.budget.max_retries`（既定 2） | WU の Run の失敗 | WU は failed → replan |
| `max_repairs` / 同じ class | 3 / 2 | 最終レビューの修復 | 従来の `ReviewFail` |
| `max_replans` | 3 | 計画の版の更新 | blocked |
| `max_runs_per_task` | 24 | worker / planner / repair / wrap-up の Run の合計（reviewer は含めない） | blocked |
| `max_task_tokens` | 無し | Task の総トークン（input + output。設定したときだけ有効） | blocked |
| `max_task_wall_secs` | 86,400 | 最初の dispatch から数える | blocked |
| WU の予算の上限 | `max_turns ≤ 80`、`max_wall_secs ≤ 3600` | planner の指定を丸める | 丸めて記録する |
| WU の予算の既定 | `max(task.budget.max_turns, 30)` turn、`max(task.budget.max_wall_secs, 1800)` 秒 | planner が書かなかったとき | — |
| checkpoint の大きさ | 16 KiB（配列 30 件、文字列 500 文字） | D8 | 決定的に切り詰める |

「blocked」はすべて D12 の `WorkerQuestion`（dispatcher が出す質問）。人が回答すると、その窓のカウンタ（continuation・進捗なし・
Task の総量）は回答の時点から数え直す。`consecutive_*` と同じく、最後の `answer` 以降の events を数える。

### D19. metrics

- **run 単位**（E1）: `WorkerFinished.end`、budget 切れでも usage を運ぶ、`RunMetrics.peak_context_tokens`、`RunMetrics.turns`。
  - `peak_context_tokens` は、claude-code なら stream-json の assistant メッセージごとの usage（input + cache_read + cache_creation）の
    最大値。codex / acp は取れる範囲だけ（§7 U2）。
- **Task 単位**（E5）: 純粋関数 `task_core::execution_metrics::summarize(&Task, &[Event]) -> ExecutionMetrics`。
  - 欄: `gate_mode`、`work_units`、`runs`（role ごと）、`continuations`、`budget_exhausted`（kind ごと）、`max_turn_failures`、`retries`、
    `repairs`（class ごと）、`replans`、`peak_context_tokens`、`total_tokens`、`cost_usd`、`wall_ms`（最初の dispatch から終端まで）、
    `final_status`。
  - API は `GET /tasks/{id}/execution`（計画・WU・runs・metrics）。
- **集計**（E5）: `GET /metrics/execution?since=&group_by=gate_mode|genre|assignee|lane` を `runs` の索引から作る。
  - decomposition policy（gate の閾値）を後で直すための材料にする。
  - shadow モードの「compound と判定したが atomic で走らせた」タスクが実際に予算切れになったかを並べて見られるようにする。
- `task-api::stats::classify_outcome` と `view.rs::classify_outcome` は、`end` があればそれを優先する。無い run は従来の字句判定のまま。

### D20. GUI

- タスク詳細の overview に **Execution 節**（E5）。
  - 計画を持たない Task は「直接実行（Run n 回、continuation m 回）」の 1 行だけ。
  - 計画のある Task:
    - 見出し: 「3 / 6 WorkUnits 完了 · 現在: <WU の title> · Run #2 · <model> · <harness> · <status>」
    - WU の表: key / title / status / 依存 / 担当（Task と同じ）/ harness / model・lane / Run 数 / continuation / retry / 最後の
      checkpoint（`next_action` と `remaining` の件数）/ 失敗・レビューの理由
    - 版の履歴（replan）
  - WU を選ぶと、その Run の一覧（既存の run 詳細ルートへのリンク）と最新の checkpoint の全文が見られる。
- runs の表（既存）に `end` のバッジ（continued / yielded / budget_exhausted）と WU の key の列を足す。
- ExecutionPhase をタスクの状態バッジの横に出す（planning / executing / verifying / repairing）。
- **古いタスクの詳細は壊さない**: `TaskDetail.execution: Option<ExecutionView>`（serde(default)）。無ければ節を出さない。
  `gen:types` の差分は追加だけ。
- モバイル幅（ADR-0055）: WU の表はカードの一覧に折り返す。`mobile-audit` の違反 0。

### D21. organization routing との境界

- **Ownership**: WU は Task の担当を継ぐ（変えない）。planner は担当を書けない。
  - 部署をまたぐ仕事（別の部署の skill が要る WU）を計画で見つけたら、`plan_issue` か planner の `rationale` で申告する。
  - daemon は Task を blocked にして CoS に聞く（SPEC §3.1「部をまたぐ連携は秘書が認める」。ADR-0069 §4 の予約）。
  - 部署をまたぐ成果物は、WU ではなく**別の Task**にする（D22）。
- **Harness**: WU の `harness`（genre）は、担当の profile が許す範囲で WU ごとに変えてよい。例: 調査の WU を literature にし、
  実装の WU を coding にする。書かなければ Task の genre を使う。
- **Model**: WU ごとに `TaskFeatures::infer` を **WU の view**（Task を複製し、objective / acceptance / budget / genre を WU の spec に
  差し替えたもの）で計算し、WU の `features` で上書きしてから `ModelPolicy` にかける。
  - `EscalationPolicy` の履歴は WU ごとに取る。continuation（BudgetExhausted）では lane を上げない（ADR-0069 D6 の
    `never_escalate_on` どおり）。
  - `RoutingDecided.record.work_unit_id` に残す。
- **Review**: 最終レビューは Task の単位のまま（ADR-0069 D4 / Phase 118 の reviewer の lane 規則はそのまま）。WU の `checks` は
  決定的な検査だけで、LLM reviewer は起こさない。

### D22. support-task・子 Task（Plan / 委譲）との違い

| | 見える単位 | 作る者 | worktree / レビュー / 配送 | 状態 |
|---|---|---|---|---|
| **WorkUnit**（本 ADR） | Task の内部（GUI では Task 詳細の Execution 節だけ） | gate + planner / 人 / repair の規則 | **親の Task のものを共有** | `work_units`（D6） |
| **support-task**（`create_support_task`） | 独立した Task（`support_kind` のラベルで人の仕事の木から隠す） | tick の決定的な判断 | 自分のもの（多くは Shared で worktree を持たない） | 通常の Task の状態機械 |
| **子 Task（`Plan` kind / `delegate.json`）** | 独立した Task（仕事の木に出る） | 計画 run・実行中の委譲 | それぞれが自分のものを持つ | 通常の Task の状態機械、親は `awaiting_children` |

使い分け:
- 成果物が**別々に取り込み・判断される**もの（別のリポジトリ、別の部署、人が個別に承認したいもの）は子 Task（または CoS の
  `create_task`）にする。
- 1 つの goal を**context の都合で分けて実行する**だけなら WorkUnit にする。
- support-task は「daemon が起こす裏方の仕事」で、goal の分解ではない。support-task は常に atomic（D13 の対象外）。
- 子 Task を持つ親（委譲した run）の WU 化は扱わない。委譲を使う Task は、従来どおり atomic で走る（`delegate.json` を出す run は、
  計画のある Task の WU の中からは使えないようにする。E3 の検証で WU の run には `available_genres` を渡さない）。

### D23. migration と後方互換

- **E1 は migration 無し**（Event の追加フィールドと新しい Event だけ。`tasks.json` と `events.json` の中）。
- **E2 で migration 0026**（D5。`CREATE TABLE IF NOT EXISTS` だけで、既存の表には触れない）。埋め戻しはしない。
  `SchemaTooNew` の規則（ADR-0013 D5）どおり、旧いバイナリは 26 の DB を開けない。ロールバック（`rollback.sh`）の手順は
  ADR-0040 D2 のまま。3 つの表は派生なので、落としても events から作り直せる。
- **既存の小さな Task は挙動を変えない**:
  - 計画を持たない Task は暗黙の WU で、行を作らない。
  - 予算切れ以外の終わり方（done / question / error / requeue / infra）の遷移は変わらない。
  - 変わるのは予算切れの扱いだけ（`WorkerError` → `Continue`）で、`[execution] continuation = false` で戻せる。
  - プロンプトの追加（D10 の予算の予告・rolling checkpoint）は coding 系の execute run だけ。既存のスナップショットテストは
    期待値を更新する（追加した節だけの差分であることを確かめる）。
- **API / GUI**: 追加だけ。`TaskDetail.execution`、`RunSummary.end` / `work_unit`、`GET /tasks/{id}/execution`、
  `POST|PUT /tasks/{id}/execution-plan`。`api-v1.schema.json` / `event.schema.json` を再生成する。
- **既存のカウンタとの整合**:
  - `attempt_history`（`retry_policy.rs:266`）は `continue` / `advance` / `work_unit_retry` / `planned` / `replan` / `review_repair` を
    試行に数えない。
  - `classify_task_failure` は変えない。
  - `consecutive_requeues` / `consecutive_infra_requeues` は、新しい理由を「その他」として扱う（連続を切る）。
  - `stats` は `end` を優先する。

### D24. SPEC / DESIGN との関係

- SPEC §3.3「仕事の木（DAG）」: 人が見る木は Task のまま。WorkUnit は木に出さない（D2）。SPEC §2.3 の「種々の仕事に分解」は、
  goal の単位では従来どおり CoS / 計画の子 Task で行う。WorkUnit は Task の中の実行の分割で、SPEC とは矛盾しない。
- SPEC §3.1「部をまたぐ連携は秘書が認める」: WU は部をまたがない（D21）。
- DESIGN 原則 1〜2・6: D4 / D5 / D8 で守る。
- DESIGN §5.6「計画が不正なら 1 回だけ再試行し、それでも不正なら `failed`」: 内部の実行計画では「atomic に倒す」にした（D14）。
  `Plan` kind の規則は変えない。DESIGN.md は書き換えず、PROGRESS の「提案」に「§5.6 に内部の実行計画の注記を足す」を置く。
- DESIGN §4.2 の状態集合は変えない（D6）。

---

## 4. 再利用するもの・新設するもの

| 再利用する（変えないか、追加だけ） | 新設する |
|---|---|
| Project / Task、`Status`、既存の `Trigger`、`attempts`、`replay` | `Trigger::Continue{why}`、`Trigger::ReviewRepair` |
| `events`（正本）、`apply_transition_with_events`（同じトランザクション） | `CheckpointSaved` / `ExecutionGated` / `ExecutionPlanned` / `WorkUnitTransitioned` |
| `WorkerStarted` / `WorkerFinished` / `RoutingDecided`（Option の欄を足す） | migration 0026: `execution_plans` / `work_units` / `runs` |
| `TaskFeatures::infer_with_hints`、`ModelPolicy`、`EscalationPolicy`、`LaneCeiling` | `ExecutionSignals` と Complexity Gate（`execution_gate.rs`） |
| matching（Ownership）、`[[genres]]`（Harness）、`TieredAdapter`（Model） | `execution_plan.rs`（型・検証・DAG・`next_work_unit`） |
| `WorkerAdapter` と 4 つの coding adapter、result.json の規約、`result_report.rs` | `Terminal::Yielded` / `Terminal::BudgetExhausted`、`RunEnd`、checkpoint の読み取り |
| `preamble::render`、`RunContext` | `RunContext.continuation`、予算の予告と rolling checkpoint の文面 |
| worktree（Task ごとに 1 つ）、`ensure`、prune | mechanical checkpoint（`task-dispatch/src/checkpoint.rs`。git の読み取りだけ） |
| `review_task`、`ReviewVerdict`、`resolve_human_approvals`、rereview の性質 | `classify_review_failure`、repair WU、`review.json` の任意の `repair` |
| `node_sessions` / `sessions.rs`（CoS / lead のまま） | 仕事の run の `session_id`（E1b の wrap-up だけ） |
| `Requeue` / `InfraRequeue` / `max_*` の既存カウンタ、`WorkerQuestion` + `QuestionRaised` + approvals | continuation / no-progress / repair / replan のカウンタ（events から導出） |
| `routing_audit`、`Usage`、`RunMetrics`、pricing | `ExecutionMetrics`、`GET /tasks/{id}/execution`、`GET /metrics/execution` |
| GUI のタスク詳細・runs の表・run 詳細 | Execution 節、ExecutionPhase のバッジ |
| support-task、`Plan` kind、委譲（そのまま残す） | planner run（`RunRole::Planner`、`execution-plan.json`） |

## 5. 採らない（代替案と却下の理由）

- **R1: WorkUnit を子 Task にする**（`Plan` / 委譲を流用する）。GUI とボードが実行の雑音で埋まる。worktree・レビュー・配送が WU ごとに
  複製される。人の依頼の「Task を大量に作って GUI を埋めない」に反する（D2）。
- **R2: continuation を session の resume で行う**（ADR-0054 の流用）。context が圧縮されず、すぐにまた尽きる。仕事の run には
  セッションが無い。rollover やアカウント固定の意味が混ざる（D9）。wrap-up のための短い resume だけは許す。
- **R3: Task の `Status` に planning / executing / verifying / needs_human を足す**。状態機械の遷移表（ADR-0002 D8 と全列挙のテスト）、
  API の schema、GUI の型・フィルタ・ボードの列、通知、受信箱、`classify_*` をすべて変えることになる割に、得るものは表示だけ。
  導出値の `ExecutionPhase` で足りる（D6）。
- **R4: 初期版から 1 Task の中で WU を並列に実行する**。worktree を共有すると衝突する。`running` / lease / reclaim は TaskId が key で、
  すべて作り直しになる。直列でも「context を分ける」という主目的は達成できる（D6）。
- **R5: atomic / compound の判定を LLM（CoS・lead）にさせる**。DESIGN 原則 1 に反し、監査・再現もできない。判定は決定的に行い、
  LLM は意味の分解だけにする（D4 / D13）。
- **R6: lead の常時の中継（CoS → Eng Lead → SW Lead → Worker）に戻す**。ADR-0069 が却下した形。小さな Task の費用と遅延が増える（D3）。
- **R7: 実行層を表だけで持つ（events に積まない）**。DESIGN 原則 6（追記専用のイベントから再構成できる）と監査に反する（D5）。
- **R8: 実行層を events だけで持つ（表を作らない）**。E1 の範囲ではこれで足り、そうする。ただし Task をまたぐ集計（metrics・gate の S6）
  と GUI の一覧が JSON の走査になるので、E2 から派生の索引を置く。
- **R9: `max_turns` を一律に大きくする**。尽きる時期が遅れるだけで、context の肥大と費用は増える。途中で落ちたときに失うものも増える。
- **R10: 予算切れを `InfraRequeue` として扱う**。checkpoint の意味と進捗の判定を持たない。インフラの障害と仕事の区切りを
  同じカウンタで数えることになる（D11）。
- **R11: ML の分類器で gate を判定する**。データがまだ無い。shadow の記録（D13 / D19）を貯めてから検討する（ADR-0069 §5 と同じ扱い）。

## 6. Phase E1〜E6 の実施計画

各 Phase の完了時に `cargo fmt --all -- --check` / `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` を通す。
GUI を触る Phase では `pnpm typecheck` / `lint` / `test` / `gen:types` の差分ゼロと `mobile-audit` を通す。
テストは偽のアダプタとスタブの CLI を使い、外部ネットワークに出ない。

### E1: Run lifecycle / checkpoint / continuation（暗黙の WorkUnit。migration 無し。**これだけで本番の価値がある**）

**受け入れ条件**:
- (a) claude-code の `error_max_turns`（スタブの stream）と wall-clock の打ち切りが `RunEnd::BudgetExhausted{Turns|WallClock}` になる。
  usage が `WorkerFinished` に残る。Task は `Continue{continue}` で Ready（attempts は不変）になる。
- (b) run 中に書かれた `checkpoint.json` と mechanical（git）が D8 の規則で合成され、`CheckpointSaved` として残る。
  - schema 違反・checkpoint が無いときは mechanical だけで作る。
  - 16 KiB に切り詰める。
- (c) 次の run の `request.json` の `context.continuation` と `prompt.txt` に「続きの実行（Run #2）」の節と checkpoint が載る。前の run の
  会話・出力の全文は載らない。continuation を持たない run のプロンプトは、D10 の追加分を除いてバイト単位で同じ。
- (d) result.json の `yield` が `RunEnd::Yielded` になり、continuation する。
- (e) `max_continuations_per_work_unit` への到達と、進捗なし 2 回で `blocked` になる（`QuestionRaised` と approval が付く）。人の回答で
  窓が戻る。
- (f) done / question / error / requeue / infra の既存の遷移と既存のテストが変わらない。`[execution] continuation = false` で、予算切れが
  従来の `WorkerError` に戻る。
- (g) `attempt_history` / `replay` / `classify_task_failure` / `stats` が `continue` を試行・失敗に数えない。
- (h) daemon の再起動（同じ store で新しい `Dispatcher`）の後も、最新の checkpoint から continuation が組まれる。
- (i) codex / acp / aider でも、wall-clock の打ち切りが BudgetExhausted になり、continuation する（スタブ）。

**触るファイル**:
- task-core:
  - `crates/task-core/src/execution.rs`（新規: `RunEnd`、`BudgetKind`、`Checkpoint`、合成・切り詰め・進捗の判定、`ContinueWhy`）
  - `model.rs`（`CheckpointSaved`、`WorkerFinished.end`、`WorkerStarted.run_seq`、`RunMetrics` の追加欄）
  - `transition.rs`（`Continue`）
  - `retry_policy.rs`（`is_budget_outcome` を pub にし、context の語を足す）
- task-worker:
  - `adapter.rs`（`Terminal` の 2 変種）
  - `claude_code.rs` / `codex.rs` / `acp.rs` / `aider.rs`（検出と usage と `peak_context_tokens`）
  - `result_report.rs`（`yield` / `checkpoint` の読み取り）
  - `preamble.rs`（予算の予告・rolling checkpoint の指示・続きの節）
  - `protocol.rs`（`RunContext.continuation`、`WorkerMessage` の対応）
- task-dispatch:
  - `dispatcher.rs`（`on_worker_finished` の分類 → `Continue` / 上限 → 質問、`run_worker` の後段の mechanical checkpoint、
    `run_extras` の continuation）
  - `checkpoint.rs`（新規、git の読み取り）
- task-ops:
  - `derive.rs`（`consecutive_continuations`、`latest_checkpoint`、`no_progress_streak`）
  - `view.rs`（`RunSummary.end`）
  - `replay.rs`
- その他: `crates/task-api`（schema の再生成）、`crates/celeris/src/config.rs`（`[execution]`）、`docs/protocol/checkpoint.schema.json`、
  `docs/protocol/worker-protocol.md`（result.json の `yield` / `checkpoint`）

**テスト（名前は目安）**:
- `execution::tests::*`（合成・切り詰め・進捗）
- `claude_code::tests::error_max_turns_becomes_budget_exhausted_with_usage`
- `dispatcher::tests::budget_exhausted_run_continues_without_consuming_attempts`
- `continuation_prompt_carries_checkpoint_not_conversation`
- `continuation_limit_blocks_and_answer_resets`
- `no_progress_twice_blocks`
- `continuation_survives_dispatcher_restart`
- `continuation_disabled_restores_worker_error`
- `transition::tests`（全列挙のオラクルに `Continue` を足す）

**並行**: E1b は E1 の後で、E2 と並行できる。

### E1b（小、任意）: wrap-up run と ACP の真の yield

- **受け入れ条件**:
  - `[execution] wrap_up = true` のとき、claude-code / codex の予算切れ（Turns / WallClock）で worker の checkpoint が無ければ、
    同じ session を 3 turn だけ resume して checkpoint を書かせる（`runs.role = wrap_up`）。
  - context 超過・resume の拒否のときは mechanical に倒す。
  - acp は wall の 80% で `session/cancel` を送り、追加の prompt で yield させる。
  - 既定（オフ）のときは E1 と同じ挙動。
- **触るファイル**: `claude_code.rs`（仕事の run にも `--session-id` を付ける。オプトインのときだけ）、`codex.rs`、`acp.rs`、
  `dispatcher.rs`。

### E2: ExecutionPlan / WorkUnit のデータモデルと決定的な scheduler（手動・fixture の計画）

- **受け入れ条件**:
  - (a) migration 0026（`SCHEMA_VERSION = 26`）。旧い DB からの移行テスト。
  - (b) `POST /tasks/{id}/execution-plan`（origin human。D14 の検証をすべて通す。循環・重複・件数・key を拒否）と
    `celerisctl execution plan set|show`。
  - (c) fixture の 3 WU（A → B → C）の Task が、A → B → C の順に Run を起こす。各完了で `Continue{advance}`、最後は `WorkerDone` →
    最終レビュー → done。
  - (d) WU の失敗 → retry → 上限で failed。依存先は blocked（dependency_failed）。planner が無い E2 では replan できないので、Task は
    failed（D12 の 3）。
  - (e) WU の question → Task が blocked → 回答で再開する。
  - (f) WU の continuation（E1 の仕組みを WU の単位で使う）。
  - (g) `runs` の索引が全タスクの run について書かれる。`replay` で `work_units` / `runs` が events から作り直せる（一致を確かめる）。
  - (h) 再起動後の照合（D15）。
  - (i) 計画を持たない Task の挙動・プロンプトが E1 と同じ。
- **触るファイル**:
  - `crates/task-core/migrations/0026_execution.sql`、`store.rs`（型・読み書き・同じトランザクションの API）
  - `crates/task-core/src/execution_plan.rs`（新規）、`model.rs`（`ExecutionPlanned` / `WorkUnitTransitioned`）
  - `dispatcher.rs`（`next_work_unit` の配線、WU の view での routing）
  - `crates/task-ops/src/execution.rs`（新規）、`replay.rs`
  - `crates/task-api`（handlers / types / schema）、`crates/celerisctl/src/commands/execution.rs`
  - `docs/protocol/execution-plan.schema.json`
- **並行**: E1 の完了が前提。E1b とは並行できる。

### E3: Complexity Gate と自動 planning

- **受け入れ条件**:
  - (a) D13 の規則表の単体テスト（信号ごと、閾値の境目、強制規則、対象外、人の明示 > 規則 > ヒント）。
  - (b) `ExecutionGated` と `Task.routing.execution` の記録。
  - (c) `gate = shadow` では記録だけで、実行は atomic のまま。
  - (d) `gate = on` の compound の Task で最初の run が planner になり、`execution-plan.json` を検証して採用し（`Continue{planned}`）、
    WU を順に実行する（偽の planner アダプタ）。
  - (e) planner の出力が不正なら 1 回だけ再試行し、それでも不正なら atomic に倒す（Task は失敗しない）。
  - (f) planner の出力に `assignee` / `tier` があれば schema 違反になる。
  - (g) planner は lead の実効 profile で走り、node_sessions は resume しない。
  - (h) WU ごとの `RoutingDecided.work_unit_id` と、WU の view での lane。
- **触るファイル**:
  - `crates/task-core/src/execution_gate.rs`（新規）、`model.rs`（`TaskRouting.execution`、`RunRole::Planner`）、`model_policy.rs`
    （WU の view の helper）
  - `task-ops/src/add.rs`（`NewTaskSpec.execution`）、`actions.rs`（CoS のヒント）
  - `dispatcher.rs`（gate と planner run）、`claude_code.rs`（planner のプロンプト）、`preamble.rs`
  - `celeris/src/config.rs`（`[execution] gate`、`[execution.planner]`）
- **並行**: E2 の後。E4 とは `dispatcher.rs` で衝突しうるので、既定は E3 → E4 の順。worktree を分けるなら、E3 は `dispatch_ready`、
  E4 は `on_review_finished` を触る。

### E4: reviewer repair と replanning

- **受け入れ条件**:
  - (a) `classify_review_failure` の分類表のテスト（fmt / lint / test_small / reviewer_local / substantive、混在なら substantive）。
  - (b) 大きな実装の後に `cargo fmt --check` だけが不合格になった Task が、`ReviewRepair` → repair WU（最小の context。request.json に
    元の objective の全文が**無い**ことを確かめる）→ 再レビュー → done になる。attempts は変わらず、人の承認は再利用される。
  - (c) repair の上限を超えると従来の `ReviewFail` に戻る。
  - (d) WU の failed・`plan_issue`・進捗なし・実質的な不合格で replan の planner run が起き、done の WU を保持した v2 が採用される。
    人の依頼の例（A done / M 追加 / B blocked by M / C blocked by B）の fixture で確かめる。
  - (e) replan の上限で blocked。
  - (f) 版の履歴（`ExecutionPlanned.supersedes`）が監査できる。
  - (g) WU の決定的な `checks`（Command）が WU の完了前に走り、失敗なら WU を retry する。
  - (h)（任意）配送の `[delivery-repair]` を merge_base の repair WU に置き換える。
- **触るファイル**: `crates/task-core/src/execution.rs`（`classify_review_failure`）、`execution_plan.rs`（差分）、
  `crates/task-worker/src/protocol.rs`（`ReviewOutput` の任意の `repair`）、`review.rs`（WU の checks の実行を再利用）、`dispatcher.rs`
  （`on_review_finished`）、`crates/celeris/src/delivery.rs`（任意）。
- **並行**: E2 の後（E3 と並行する場合は上の注意に従う）。replan の部分は E3 の planner が前提。

### E5: GUI と metrics

- **受け入れ条件**:
  - (a) `ExecutionMetrics` の純粋関数のテスト。`GET /tasks/{id}/execution` と `GET /metrics/execution`。
  - (b) タスク詳細の Execution 節（D20）。計画なし・計画あり・replan あり・repair ありの 4 つの fixture で vitest。
  - (c) 古いタスクの詳細（`execution` 無し）が変わらない。
  - (d) runs の表の `end` / WU の列。
  - (e) `gen:types` の差分ゼロ（2 回実行）、`mobile-audit` の違反 0、`pnpm build`。
- **触るファイル**: `crates/task-core/src/execution_metrics.rs`（新規）、`crates/task-ops/src/view.rs`、`crates/task-api`（handlers / types /
  stats）、`gui/app/routes/tasks.$id.tsx`、`gui/app/components/ExecutionSection.tsx`（新規）、`gui/app/celeris/types.ts`（生成）。
- **並行**: API の部分は E2 の後に始められる（E3 / E4 と並行）。最後に gen:types を取り直す。

### E6: end-to-end の dogfood

- **受け入れ条件**:
  - 「Celeris 自身の大きな自己改善」（例: 本 ADR の E5 の GUI、または次の大きな改修）を `gate = on` で 1 件流す。
  - 比較の基準は、同規模の過去の 1 巨大 session の Task（例: 01M38J4X53P1Y684FS42Z6R0VZ の routing 再設計、Knowledge GC の複製）。
    `routing_audit` と events から、max-turn failure の回数、peak context、retry の回数、完了までの時間、トークンと費用、reviewer の
    repair の回数を表にする。
  - 結果を PROGRESS に証跡として残す。gate の閾値を調整し、`gate` の既定を `on` にするかを人に提案する。
- **実行**: 認証が使える環境ではエージェントが実行し、証跡を残す（ADR-0009 P-34）。使えなければ手順を書いて人に頼む。本番の昇格は
  人の判断。

## 7. 未解決事項（E0 時点。既定値を選んだうえで記録する）

- **U1**: context 超過の実際の文言（claude-code の `result` / stderr、codex の `turn.failed`、ACP の `stopReason`）は実機で未確認。
  E1 は既知の候補の字句判定と、「分類できなければ従来の `WorkerError`」の安全側で実装し、実機の確認を E6 で行う。
- **U2**: codex / acp が turn ごとの usage（peak context）を出すかは未確認。取れなければ `None` にする。
- **U3**: 1 Task の中での WU の並列（読み取り専用の investigate から）。WU ごとの lease と別の worktree が要るので、別 ADR にする。
- **U4**: WU の checkpoint ごとに WIP の commit を求めるか。既定は「求めない」（mechanical の `repo_state.uncommitted` で記録する）。
  配送や diff を見ると WU ごとの commit が有用なので、E6 の結果で再検討する。
- **U5**: 部署をまたぐ WU（D21）を CoS の認可で許すか、常に別の Task にするか。初期版は「別の Task」にする。
- **U6**: `pricing.rs` は `claude-fable-5-1` の単価を持たない（P-118-1）。E5 の cost の集計が frontier で欠ける。
- **U7**: `idle timeout` を harness_error（インフラ扱い、attempts を消費しない）に変えるか。E1 では遷移を変えない（記録だけ）。
- **U8**: 上限到達による blocked の質問に対する人の回答の語彙（「続ける（予算 +N）/ replan / 中止」）を、GUI のボタンにするか。
  E5 で決める。
- **U9**: remote（ssh）の worktree の mechanical checkpoint は E1 の対象外（worker の checkpoint だけ）。
- **U10**: gate の閾値（score ≥ 5）と重みは推測の初期値。shadow の記録（E3〜E5）と dogfood（E6）で調整する。

## Phase E1 実装時の逸脱・明確化（2026-09-24）

実装しながら見つかった、D5〜D10 の記述とコードの食い違い。黙って逸脱せず、ここに記録する。

- **D5 からの逸脱: `WorkerStarted.run_seq` は event のフィールドとして持たせず、events から純粋に導出する**
  （`task_ops::derive::current_run_seq`。非 Reviewer の `WorkerStarted` の件数を数えるだけ）。
  理由: `run_seq` は「そのタスクの中の何回目の worker run か」という、既存の events から完全に復元できる
  情報であり、DESIGN 原則 6（events から現在状態を再構成できる）にも適う。一方でこれを `Event::WorkerStarted`
  の新規フィールドとして追加すると、この enum バリアントを struct literal で組み立てている**約 60 箇所**
  （task-core / task-ops / task-dispatch / task-api / celeris / celerisctl の実装とテスト）すべてに機械的な
  変更が要る（`Event::WorkerFinished.end`（D7 の本体で必須）と合わせると 140 箇所超）。値を持たない冗長な
  フィールドのためにこの範囲を広げるのは不釣り合いと判断し、導出関数に倒した。`WorkerFinished.end` は
  D7 の分類そのもの（continuation 判定に必須）なので、そちらは予定どおり追加した。
- **D5 の checkpoint schema: JSON の tag を `"kind"` から `"type"` に変更**。`RunEnd::BudgetExhausted{kind:
  BudgetKind}` のフィールド名 `kind` が `#[serde(tag = "kind")]` の外側タグ名と衝突する（`schemars`/`serde`
  が「variant field name conflicts with internal tag」でコンパイルエラーにする）ため、外側タグを `"type"`
  にした（`Check`（`task_core::model::Check`）など、このコードベースの他の tagged enum と同じ命名）。
- **`task_core::execution::Decision` を `CheckpointDecision` に改名**。`task_core::approval::Decision`
  （既存、`lib.rs` で re-export 済み）と名前が衝突するため。
- **D19「`task-api::stats::classify_outcome` と `view.rs::classify_outcome` は `end` があれば優先する」の実装**:
  両方の `classify_outcome` に `end: Option<&RunEnd>` を追加し、`end` が `Yielded`/`BudgetExhausted` かつ
  outcome 文字列が `"continue: "` で始まるときだけ新設の `RunOutcomeKind::Continued` を返す（それ以外は
  従来どおり文字列の接頭辞で分類）。`RunSummary` にも `end: Option<RunEnd>`（D20 のバッジの下地）を追加した。
  同じ作業で **P-E0-3**（`stats.rs::classify_outcome` が `"infra_requeue: "` を分類していなかった）も直した
  （`Requeue` に分類する。供給側の `requeue:` と同じ「attempts を消費しない再試行」という性質のため）。
- **P-E0-2 の直接の修正は見送った**: 調査で見つけた「claude-code で result.json が無いとき、
  `Ok(Terminal::Error)` になり attempts を消費する（ADR-0070 D3 の想定と食い違う）」は、この Phase の
  `RunEnd` 分類の追加とは独立した既存の不整合で、直すと `WorkerError` → `InfraRequeue` の遷移になり
  既存のテスト・本番挙動が変わる。E1 の「(f) 既存の遷移は変わらない」を優先し、今回は手を付けていない。
  E2 以降（`HarnessErrorClass::Supply`/`Infra` の配線）で改めて判断すること。
- **reviewer/planner run と `Terminal::Yielded`/`BudgetExhausted`**: reviewer run は worker run と同じ
  `claude_code::run_claude_code` を通るため、理論上は reviewer run でも予算切れ・yield の `Terminal` が
  返りうる。continuation（D9/D11）は worker run だけの仕組みなので、reviewer 側では「判定できなかった」
  として `max_reviewer_retries` の再試行に倒す（既存の `Terminal::Error{retryable:true}` と同じ経路。
  `crates/task-dispatch/src/review.rs`）。checkpoint は作らない（reviewer run に checkpoint の出番は無い）。

## Phase E2 実装時の逸脱・明確化（2026-09-24）

実装しながら見つかった、D5〜D18 の記述とコードの食い違い・簡略化。黙って逸脱せず、ここに記録する。

- **`WorkerStarted`/`WorkerFinished` に `work_unit_id` を足さなかった**（E1 の `run_seq` 省略と同じ理由）。
  D5 は `WorkerStarted.work_unit_id: Option<String>` を新設イベントフィールドとして挙げているが、
  `Event::WorkerStarted`/`WorkerFinished` を struct literal で組み立てている箇所（約 140、E1 の時点の
  カウント）に機械的な変更が要る。代わりに、どの run がどの WorkUnit のものかは **`runs` 索引の
  `work_unit_id` 列**（`Event::WorkUnitTransitioned{to: Running, run_id: Some(...)}` と同じトランザクションに
  近い形で書く）と、**`Event::CheckpointSaved.work_unit_id`**（D5 で元から予定されていた欄。E1 で既に
  存在）だけで賄った。`on_worker_finished` はこの run が「いま `running` の WU で `last_run_id` が一致する
  もの」を `work_units` テーブルから引いて判定する（`WorkerFinished` 自体の走査ではない）。`replay`/(g) の
  再構築でも `runs`/`work_units` を `ExecutionPlanned`/`WorkUnitTransitioned`/`CheckpointSaved` から
  組み立てるので、`WorkerStarted.work_unit_id` が無くても events から派生索引を再現できる（D5 の
  「events が正本」は保たれる）。
- **WU の run の continuation は `events` ではなく `runs` 索引から組み立てる**（`work_unit_continuation_context`。
  `crates/task-dispatch/src/dispatcher.rs`）。E1 の `build_continuation_context`（暗黙の WU 用）は
  `events` を後ろから走査して `WorkerStarted`/`WorkerFinished` を数えるが、これらのイベントに
  `work_unit_id` が無い（上記の逸脱）ため、WU をまたいで区別できない。`runs` 索引は最初から
  `work_unit_id` で引けるので、WU の run ではこちらを正とする（`runs_for_work_unit`）。
  `no_progress_streak`/`latest_checkpoint`（`task_ops::derive`）は `CheckpointSaved.work_unit_id` で
  フィルタできるので、これらは予定どおり `work_unit_id: Option<&str>` を取るよう拡張した
  （E1 の申し送りどおり）。
- **WU の Completed run では checkpoint を merge・保存しない**（D8 は「yield / done の result.json に
  任意で添える checkpoint」を挙げているが、E2 では実装していない）。`checkpoint` の合成は
  `RunEnd::is_continuable()`（`Yielded`/`BudgetExhausted`）のときだけ行う（E1 と同じ条件）。結果として、
  依存する WU への「完了要約」の引き継ぎ（`WorkUnitPromptContext.dependency_summaries`）は、その WU が
  continuation を経ていれば実際の `completed` 配列を使うが、1 回の run でそのまま完了した WU では
  固定文言「完了」にフォールバックする。より詳しい要約が要るなら、E4 以降で done の result.json の
  任意 `checkpoint` を読む経路を足すこと。
- **WU の予算（D18「WU の予算の既定」）は Task の budget をそのまま使う**。`WorkUnitSpec.budget`
  （`max_turns`/`max_wall_secs`）は D14 の検証（丸め）は通すが、E2 の scheduler はまだ実際の run の
  `RunLimits`/`task.budget` へ反映していない（fixture/human の計画は Task と同程度の粒度を想定していた
  ため、実害は小さいと判断）。E3 で planner が WU ごとに異なる予算を出すようになったら、
  `start_work_unit_run` で `task.budget` を `wu.spec.budget`（無ければ D18 の既定式）に差し替えること。
- **D21（WU ごとの routing）は配線していない**。`RoutingRecord.work_unit_id` フィールドはスキーマに
  足したが、WU の run でも Task 全体と同じ `TaskFeatures`/`ModelPolicy` で routing を決めており、
  `work_unit_id` は常に `None` のまま記録される。`WorkUnitSpec.harness`/`features` を実際の routing に
  反映する「WU の view」（D21）は E3 の範囲として持ち越した。
- **D15 の依存伝播で `WorkUnitTransitioned.from` を一律 `pending` にしている**（`newly_blocked`/
  `newly_ready` の対象行）。`newly_ready`（`task_core::execution_plan::newly_ready`）は定義上
  `pending` の WU だけを対象にするので厳密に正しい。`dependents_to_block` は理論上 `ready`/`blocked` の
  WU も対象にしうるが、E2 の直列実行では「あるWUに依存するWUは、その依存先が失敗した時点でまだ
  `pending`（`ready` に上がるには依存が全て `done` である必要があり、1 つでも `failed` なら永久に
  `done` にならない）」という性質上、実際には `pending` にしかならない（`ready`/`blocked` に到達しない）。
  そのため一律 `pending` としても監査上の不正確さは生じない。
- **Task の Cancel は WU の行をカスケードしない**。計画のある Task を中止しても、`work_units` の
  行は最後に見た状態のまま残る（`superseded`/`cancelled` にはならない）。D6 の状態機械表にある
  「未完了すべて｜Task の中止｜cancelled」は未実装。E2 の受け入れ条件 (a)〜(i) には含まれないため、
  持ち越した（次の Phase で `on_cancel` 相当のカスケードを足すこと）。
- **WU の再起動照合（(h)）は `reclaim_expired_leases`（lease 失効）の経路にだけ配線した**
  （`reconcile_work_unit_run`）。`abort_stale_runs`（idle timeout・drain）や `Cancel`/`Interrupt` による
  明示的な停止では、WU の行が `running` のまま残りうる（Task 側は正しく `ready`/`blocked` 等に遷移する
  ので、次に `dispatch_ready` が回ったとき `next_work_unit` が `Stuck` を返し、その tick は
  スキップされてしまう可能性がある）。D15 が名指ししている「lease 失効 → `reclaim_expired_leases` →
  `InfraRequeue`」の経路は塞いだが、他の停止経路の WU 照合は E3 以降で拡張すること。
- **`WuDispatchGate` の Answer 再開はイベントの種類ではなく状態の形で判定する**
  （`wu_dispatch_gate`）。「Task が `Ready` で、計画があり、`next_work_unit` が `Stuck`（＝ `ready`/
  `needs_continuation` の WU が無い）」ときに、`blocked(question|limit)` の WU を機械的に
  `resume_after_answer` で戻す。`Trigger::Answer` そのものをフックしていないのは、`Answer` の適用箇所
  （`task-ops::gate`/`task-api` 等）を変えずに済ませるため。E2 の対象では、この形になるのは
  「WU が `question`/`limit` で `blocked` になった Task に人が `Answer` した直後」だけなので、実害は無い。
- **checkpoint の `run_seq`（WU 版）は `work_units.runs` 列をそのまま使う**（`CheckpointContext.run_seq:
  wu.runs`）。`start_work_unit_run` が dispatch 時に `runs` を先に +1 しているので、`on_worker_finished`
  で読み直した `wu.runs` の値がそのままこの run の番号になる。

## Phase E3 実装時の逸脱・明確化（2026-09-24）

実装しながら見つかった、D13〜D21 の記述とコードの食い違い・簡略化。黙って逸脱せず、ここに記録する。

- **D13 の「対象外」は 2 段階に分けて実装した**。`kind != Execute` / 対話 / support-task / `routing`
  無しの 4 条件は、`dispatcher.rs::execution_gate_if_needed` が gate を呼ぶ前にフィルタし、
  `Event::ExecutionGated` そのものを**記録しない**（CoS の対話 1 発言ごとに別タスクが作られる・
  support-task が daemon の裏方仕事として大量に湧く、という production の実態を踏まえ、Task の生涯に
  1 件は確実に増える監査イベントを、判定するまでもなく atomic な種別にまで広げるのは無駄と判断した）。
  固定パイプラインの harness（`literature`/`web-research`/`knowledge`）と `workspace_mode = Shared` の
  内部タスクの 2 条件は `execution_gate::decide` 自身（`out_of_scope_rule`）が判定し、
  `rule_id = "atomic/out-of-scope"` として記録する（こちらは execute タスクとして gate に本当に
  到達しうる、まれだが監査したいケース）。単体テスト（(a)）は `out_of_scope_rule` に対して 6 条件
  すべてを確認しているので、判定ロジック自体はどちらの経路でも同じ関数を通る。
- **S4（複数の実行環境）と S6（直近の budget_exhausted 実績）は既定値で運用している**。
  `ExecutionGateInputs { multi_environment: false, recent_budget_exhausted_ratio: None }` を
  `dispatch_ready` から渡しており、org の部署またぎ判定や `runs` 索引を横断した直近タスクの集計は
  実装していない（D13 は「S6 は daemon が store を決定的に読む…E3 の時点では events から数える実装で
  もよい」としており、E3 の時点では未実装でもよいと明記されている）。規則表・スコア計算・閾値の境目は
  純粋関数として単体テスト済み（(a)）なので、S4/S6 の実データ配線は E5/E6 の shadow の記録を見てから
  行う（U10 と同じ「重みは推測の初期値」の扱い）。
- **`[execution.planner].permission_mode` は設定に持つが、実行時の配線はしていない**。D14 は
  「読み取り中心、既定 `plan`」としているが、`ClaudeCodeAdapter` の `permission_mode` は
  プロバイダ単位で 1 つのアダプタインスタンスに固定されており（`with_model`/`with_env` と同じ形の
  `with_permission_mode` フックが無い）、run ごとに上書きする経路が無かった。E3 では配線を見送り、
  planner run もアダプタの既定（通常 `bypassPermissions`）で走る。プロンプト側で「計画を書くだけで、
  コードは書かない・実装はしない」と明示しているので Task を壊す実害は無いが、D14 の「plan
  permission-mode」を字義どおり強制してはいない。`with_permission_mode` トレイトフックの追加は
  E4/E5 で検討すること。
- **D21（WU ごとの lane）は `decide_for_work_unit`（`task_core::model_policy`）で配線したが、
  リトライのエスカレーション（`EscalationPolicy`）は適用していない**。`decide_lane`（Task 全体）は
  `task.attempts > 0` のときエスカレーションを効かせるが、`decide_lane_for_work_unit` は行わない。
  D11 のとおり計画のある Task では WU の失敗が `task.attempts` を消費しない（`retries` は WU 側の
  カウンタ）ため、`task.attempts` を見るエスカレーションの判定はそもそもほとんど発火しない
  （継続 WU の retry では `task.attempts` が動かない）。D21 の「continuation では lane を上げない」は
  結果としてそのまま守られているが、「WU の retry でエスカレーションする」経路自体は未実装。
- **Planner の出力の harness 検証は `[[genres]]` の id 集合との照合だけ**（`validate_plan_harnesses`）。
  D14 は「担当の profile が許す harness に限る」としているが、担当ノードの実効 profile が許可する
  genre のサブセットまでは絞っていない（`[[genres]]` が空の設定では検証自体をしない。既存の
  `role`/`genre` の検証と同じ緩さに揃えた）。
- **planner run の `runs` 索引・`RoutingDecided` は `RunRole::Planner`/`RunIndexRole::Planner`/
  `TierSource::System`（frontier 固定）で記録するが、`node_sessions` には触れない**（D9/D14「resume
  しない」は、そもそも planner run が `is_conversation(task) == false` なので継続セッションの判定に
  掛からず、実装を足すまでもなく満たされている。テストで `RunContext.session.is_none()` を確認した）。
- **E2b からの指摘の修正（本 ADR の範囲外だが `dispatcher.rs` を触るついでに直した）**: E2 は
  `run_index_start` を WU の run（`start_work_unit_run`）にしか配線しておらず、計画の無い Task の
  worker run と reviewer run は `runs` 索引に行が作られていなかった（`run_index_finish` は E2 の時点で
  既に全 run で呼ばれていたが、対応する `INSERT` が無いので `UPDATE` が 0 行に当たって黙って
  no-op になっていた）。E3 で `dispatch_ready`（暗黙 WU の worker run）と `spawn_review`（reviewer run）
  の両方に `run_index_start` を足し、`on_review_finished` の 3 つの終了経路（延期・引き分け再判定・
  通常の pass/fail）すべてに `run_index_finish`（`finish_reviewer_run_index`）を足した。テスト
  `plain_task_writes_both_a_worker_and_a_reviewer_row_to_the_runs_index` で確認。E2 の PROGRESS.md の
  「(g)」の記述（"`run_index_finish` を atomic/WU を問わず常に呼ぶ"）は事実として不正確だったので、
  この Phase の PROGRESS.md で訂正する。
- **replan（D17）は実装していない**。`ExecutionPlannerContext.replan` は常に `false`、
  `ContinueWhy::Replan` の配線は無い（D17 は E4 の範囲）。E3 の planner は「1 回だけ再試行、
  それでも不正なら atomic に倒す」（D14）だけを実装した。

## Phase E2b 実装時の逸脱・明確化（2026-09-24）

E2 の受け入れ条件 (g) 後半（`work_units`/`runs` の replay 再構築）を実装しながら見つかった、D5/D15 の
記述とコードの食い違い・簡略化。

- **発見: `runs` 索引は E2 の時点で WU 経由の worker run にしか書かれていなかった**。実装前に
  `run_index_start` の呼び出し箇所を全数調査したところ（`grep -rn run_index_start crates/`）、
  `crates/task-dispatch/src/dispatcher.rs::start_work_unit_run`（WU の dispatch）の 1 箇所だけだった。
  `run_index_finish` は `on_worker_finished` の末尾で常に呼ばれる（PROGRESS.md の Phase E2 節の
  「(g) `runs` の全件書き込み」はこれを指す）が、対応する `run_index_start` が無い run（暗黙の WU の
  worker run、reviewer run）に対しては行が存在しないため `run_index_finish` は黙って `Ok(false)`
  を返すだけで、`runs` 表に行は増えない。つまり本番の `runs` 表は現時点で「計画のある Task の WU
  run」しか持っていない。`rebuild_work_units_and_runs`/`celerisctl replay --apply` は、この欠けている
  行（暗黙の WU の worker run・reviewer run）も events（`WorkerStarted`/`WorkerFinished`）だけから
  作るので、本番 DB へ初めて `--apply` したときは「新規」の `runs` 行が多数 `run_mismatches` として
  出る（既存の行が壊れているのではなく、単に無かった行を埋める）。この欠落自体を dispatcher.rs 側で
  埋める（`run_index_start` を暗黙 WU・reviewer run にも呼ぶよう配線する）のは本 Phase の範囲外
  （`dispatcher.rs` は触らない指示。次の一手として記録する）。
- **`rebuild_work_units_and_runs` の引数は `Event` ではなく `EventRow`（`ts` 付き）にした**。
  PROGRESS.md の申し送りは `rebuild_work_units_and_runs(events: &[Event])` という形を提案していたが、
  `work_units.created_at`/`updated_at`、`runs.started_at`/`finished_at` は文字列（`Option` ではない）
  で、events だけから復元できる唯一の時刻情報は `EventRow::ts`（`event_rows_for` が返す）である。
  `Event` のみでは復元できないため、`TaskStore::event_rows_for(task_id, None, usize::MAX)` を使う形に
  変えた。
- **`work_units.id` は最初に遷移した `WorkUnitTransitioned.work_unit_id` から復元する**。
  `Event::ExecutionPlanned.plan.work_units` は `key` しか運ばない（`id` は `execution_plan_adopt` が
  `new_id()` で発行し、DB にしか残らない）。実際にはほぼ全ての WU が最低 1 回は状態遷移する
  （依存先の失敗で `dependency_failed` に遷移する等）ため、`WorkUnitTransitioned` の系列から
  `key -> id` を復元できる。一度も遷移していない WU（理論上ありうるが、E2b のテストでは発生しない）
  だけ `rebuilt-<task_id>-<key>` という仮の id にする（`diff_execution` は `id` そのものを比較しないので、
  `--check`/`--apply` の結果には影響しない）。
- **WU のカウンタ（`runs`/`continuations`/`retries`）は `WorkUnitTransitioned.reason` の静的文字列から
  復元する**。event 自体は「結果（`to`）」しか運ばず「差分」を運ばないため、`dispatch`→`runs+=1`、
  `continue`→`continuations+=1`、`retry`→`retries+=1`、それ以外（`completed`/`question`/`limit`/
  `failed`/`harness_error`/`dependency_failed`/`dependency_ready`/`restart_reconcile`/`answer`）は
  カウンタを動かさない、という対応表を `task-dispatch::execution_scheduler`/`dispatcher.rs` の実装から
  読み取って `task_ops::replay` 側にも同じ表を持たせた（2 か所に同じ知識があるのは望ましくないが、
  event の schema を変えずに済む範囲で最小の重複にとどめた。D5 の「reason は決定的な静的文字列」を
  前提にできるのはこのため）。
- **`work_units.updated_at` は `dispatch` の遷移でだけ更新する**。dispatcher.rs のコードを読むと、
  `work_unit_transition` に渡す `WorkUnitRow` の `updated_at` を明示的に書き換えているのは
  `start_work_unit_run`（dispatch 時）だけで、`execution_scheduler::decide`（`now_wu`）はステータスと
  `blocked_reason` だけを変え、`updated_at` には触れない。`rebuild_work_units_and_runs` も同じ挙動
  （`dispatch` の `WorkUnitTransitioned` のときだけ `updated_at = ts`）にして、実際の scheduler の書き方と
  一致させた。
- **`diff_execution` は `id`/`created_at`/`updated_at`/`started_at`/`finished_at`/`session_id` を比較しない**。
  `work_units`/`runs` への書き込みは対応する `Event` の追記と**別トランザクション**（D5 の本文・
  Phase E2 の「(g)」コメント参照）なので、scheduler が呼ぶ `OffsetDateTime::now_utc()` と、
  `rebuild_work_units_and_runs` が使う `EventRow::ts`（イベントの追記時刻）は近いが一致する保証が無い。
  この 5 欄を比較に含めると、events と索引が完全に整合していても「タイムスタンプが 1 ミリ秒ずれている」
  だけで `--check` が誤検知する。業務上意味のある欄（`status`/`blocked_reason`/`runs`/`continuations`/
  `retries`/`last_run_id`/`spec` など。`work_unit_id`/`role`/`seq`/`adapter`/`model`/`account`/
  `checkpoint`/`usage`/`metrics`）だけを比較対象にした。
- **`runs.session_id` と `work_units.last_checkpoint_run_id` は常に `None` にした**。本番の scheduler も
  これらを一切書いていない（`session_id` は `start_work_unit_run` の唯一の呼び出しで常に `None`、
  `last_checkpoint_run_id` へ代入している箇所はコード全体に無い）ことをコード検索で確認済みなので、
  再構築側もそれに合わせた（events にも情報が無い）。
- **reviewer run の `seq` は worker run と別の連番にした**（`current_run_seq` が reviewer を除外して
  数えるのと対称）。本番の `runs` 索引は reviewer run を 1 件も持っていない（上記の発見）ため、
  reviewer run の `seq` にどんな値を書くべきかという先例が無い。E2b では「reviewer run だけの
  1 始まりの連番」という決定的で単純な規則を新設した。GUI・監査で reviewer run の `seq` を人が見る
  用途が具体化したら、この規則は再検討してよい。
- **`execution_plans` 表は本 Phase の再構築対象に含めない**。E2 の範囲では replan が無く、1 Task に
  つき `ExecutionPlanned` イベントは高々 1 件（＝ `execution_plans` は高々 1 行）なので、
  `execution_plan_adopt` が書く行と events の食い違いが起きる余地が小さい。E4 で replan
  （`ExecutionPlanned.supersedes`）が入ったら、`execution_plans` の版の履歴も再構築対象に加えること
  （次の一手として `docs/PROGRESS.md` にも記録する）。
- **`celerisctl replay` の既定（フラグ無し）の出力は変えていない**（後方互換）。`work_units`/`runs` の
  突き合わせは `--check`（読み取りのみ）または `--apply`（食い違ったタスクだけ書き戻す）を明示した
  ときだけ行う。`--apply` は `--check` を含む。

## Phase E4 実装時の逸脱・明確化（2026-09-25）

E4（reviewer repair・replanning）を実装しながら見つかった、D11〜D18 の記述とコードの食い違い・簡略化。
黙って逸脱せず、ここに記録する。

- **触るファイルが ADR §6 E4 の表より広い**。表は `execution.rs`・`execution_plan.rs`・`protocol.rs`
  （`ReviewOutput.repair`）・`review.rs`・`dispatcher.rs`（`on_review_finished`）・任意で
  `delivery.rs` を挙げているが、実装のために以下も触った（理由を添える）。
  - `crates/task-core/src/store.rs`: `review_repair_apply`/`execution_plan_replan` の 2 メソッドを
    `TaskStore` に追加した。repair・replan はいずれも「`Event::Transitioned` の適用」と
    「`execution_plans`/`work_units` の書き込み」を**同じトランザクション**で行う必要があり（D5 の
    「派生索引は対応する Event と同じトランザクションで書く」という不変条件）、既存の
    `execution_plan_adopt`/`apply_transition_with_events` だけでは組み合わせられなかった。
  - `crates/task-dispatch/src/dispatcher.rs` の `on_worker_finished`（`on_review_finished` だけでなく）:
    D17 の replan トリガー 1./2.（WU の failed・進捗なし/継続の上限）は最終レビューではなく WU の
    run の終わり方そのものから起きる（`execution_scheduler::decide` の結果を見て `Trigger` を決める
    場所）。ADR 表の「`dispatcher.rs`（`on_review_finished`）」だけでは D17 1./2. を実装できない。
    合わせて `on_worker_finished` を `finish_worker_result`（後段の共通処理）に分割し、E4 (g) の
    WU checks 用の非同期分岐（`spawn_work_unit_checks`/`on_work_unit_checks_finished`）を追加した。
  - `crates/task-ops/src/execution.rs`: `replan`（D17 の diff・検証・採用）を追加した。`adopt_plan` と
    対になる ops 層の関数で、E2 の `adopt_plan` と同じ置き場が自然だった。
  - `crates/celeris/src/config.rs`: `[execution] max_repairs`/`max_repairs_per_class`/`max_replans`
    （D18 の上限。既存の `[execution]` 節に足すのが素直だった）。
  - `crates/task-api/src/{execution.rs,types.rs}`: `GET /tasks/{id}/execution-plan` に `versions`
    （版の履歴。(f)）を足した。E4 の受け入れ条件 (f) が名指ししている API なので、範囲内と判断した。
  - これらはすべて ADR 表が想定していなかった配線先だが、D16/D17 を実際に動かすには避けられなかった。

- **D16「lane（cheap/standard）」は強制しない**。E3 の `permission_mode` と同じ理由（ADR-0072
  「Phase E3 実装時の逸脱・明確化」参照）: dispatcher が worker_hint を直接上書きする仕組みは
  planner run 専用（`is_planner_dispatch` 分岐）にしかなく、WU の lane は
  `model_policy::decide_for_work_unit`（policy が決める）が握っている。repair WU の `max_turns`/
  `max_wall_secs`（D16 の表の budget 列）はそのまま `WorkUnitSpec.budget` に反映したが、lane
  （cheap/standard）は強制のフックを新設せず、既存の policy にそのまま委ねた（repair の
  objective が短く判断も軽いので、既定の判定でも大きく外れない想定）。

- **repair の「同じ class は 2 回まで」カウンタは、WU の `title` の接頭辞
  （`"repair (<bucket>): …"`）から復元する**。`WorkUnitSpec` に repair 専用の欄を足すと、
  planner が書く同じ schema（`deny_unknown_fields`）に影響するため避け、`kind = repair` の WU の
  `title` を決定的な形式で書いて、次の repair 判定時にそこから bucket を読み戻す（
  `Dispatcher::repair_bucket_of_title`）。

- **repair WU の `checks` は空のまま**。D16 は「直した後に同じコマンドを実行して exit を確かめよ」と
  プロンプトに書くだけで、`WorkUnitCheck`（決定的な自動検証。(g)）は repair WU には自動で付けない。
  repair の合否は最終レビュー（`Check::Command`/`Check::Reviewer` の再判定）に委ねる設計のままにした
  （D16 の「repair の run が done になったら Reviewing に戻り、再判定する」という記述どおり）。

- **D17 3.（checkpoint の `plan_issue`）は replan のトリガーとして配線していない**。
  `Checkpoint.plan_issue`（D8、E1 で既に存在）自体は読めるが、これを Task レベルの
  `Trigger::Continue{why: Replan}` に昇格させるには、WU の状態（`decision.updated.status`）も
  同時に書き換える必要があり（`NeedsContinuation`/`Ready` のままだと `next_work_unit` が
  `RunWorkUnit` を返してしまい、replan の Stuck 判定に一度も到達しない）、かつ既存の
  `WorkUnitBlockedReason::{Question,Limit}` を流用すると `wu_dispatch_gate` の「人の回答直後だけ
  再開する」判定（下記）を誤動作させかねない。安全な設計（新しい `WorkUnitBlockedReason` 変種を
  足すか、専用の判定を分ける）を詰め切れなかったため、E4 では実装を見送った。D17 1.（WU failed）・
  2.（進捗なし/継続の上限）・4.（実質的な review 不合格）の 3 経路は実装・テスト済み。plan_issue は
  E5/E6 で改めて設計すること（次の一手）。

- **replan run のプロンプトに「今の計画・WU の状態・checkpoint」を渡す配線はしていない**。E3 の
  `permission_mode` と同じ理由（`claude_code.rs` は触らない指示）。`ExecutionPlannerContext.replan`
  （既に E3 が用意していた bool）だけを `true` にして渡す。実 LLM が意味のある replan 案を作るには
  `claude_code.rs::build_execution_plan_prompt` 側で「現在の計画」「WU ごとの状態」「起こした理由」を
  レンダリングする追加が要る（E5 以降）。テストは偽の planner アダプタ（`PlannerScriptAdapter`）で
  検証しているので、この配線が無くても E4 の受け入れ条件（採用・diff・上限）自体は確かめられている。

- **`give_up_or_retry_planner`（E3）の attempts のカウント方式を修正した**。E3 の実装は
  `Event::WorkerStarted{role: Planner}` を**タスクの生涯全部**で数えていたため、E4 で replan
  （2 回目以降の planner run）を導入すると、fresh planning で既に 2 回使っていた場合に replan の
  1 回目が即座に「使い果たした」扱いになってしまう不具合があった。**直近の `Event::ExecutionPlanned`
  （無ければ Task の最初）から数える**よう変更した（D14 の「1 回だけ再試行」は 1 回の計画作成試行の
  中の話で、fresh planning と各 replan はそれぞれ別の「試行の窓」を持つべき、という解釈）。

- **`give_up_or_retry_planner` の「諦めた」ときの振る舞いを、`active` な計画の有無で分岐した**。
  E3 は常に atomic フォールバック（`Task.routing.execution.mode = Atomic`）だったが、これは
  replan の give-up（既に WU の履歴がある計画を持つ Task）には適用できない（実行済みの WU を
  「無かったこと」にしてしまう）。`execution_plan_active(task_id).is_some()` なら
  `Trigger::WorkerQuestion`（blocked。D12「失敗にしないもの」）に倒し、無ければ従来どおり atomic に
  倒す。

- **`wu_dispatch_gate` の「人の回答直後だけ `blocked(question|limit)` を再開する」判定を、直前の
  `Event::Transitioned.reason` が実際に `"answer"` かどうかで見るよう変更した**（E2 の実装は
  `next_work_unit` が `Stuck` を返すたびに無条件で再開していた。実害が無かった理由は
  「Stuck のまま Task が Ready に戻るのは Answer の直後だけ」という前提だったが、E4 の
  `Trigger::Continue{why: Replan}`（WU の failed/limit から Task を Running → Ready に戻す）が
  この前提を崩す。人の回答ではなく replan で Ready に戻った Task の `Blocked(Limit)` の WU を、
  人が答えてもいないのに機械的に再開してしまうと、replan が一度も起きずに同じ壁に当たり続ける）。

- **`wu_dispatch_gate` の `NextStep::AllDone`/`Stuck` の扱いを変えた**。E2/E3 は「`AllDone` は
  理論上到達しない（`plan_complete` は即座に `WorkerDone` へ遷移するため）」という前提で `Skip` に
  倒していたが、E4 で repair の上限を使い切った後の `ReviewFail`、または実質的な review 不合格
  （D17 4.）が Task を「計画は全 WU done のまま `Ready`」の状態に戻すようになったため、この前提が
  崩れる。`AllDone`（すべて done なのに Ready）・`Stuck` で `Failed`/`Blocked(dependency_failed|limit)`
  の WU が残っている場合は、`replan_gate`（`max_replans` を見て `RunPlanner{replan:true}` か `Skip`
  かを返す）に倒すようにした。

- **D12 3. と D18 の「blocked」の適用範囲の整理**: D12 3. は「WU が failed（retry の上限）になり、
  かつ replan できない（上限に到達…）場合、Task は failed」と明記している。一方 D18 の上限の表は
  「`max_replans` を超えたら blocked」とだけ書いており、一見矛盾する。D12 の「失敗にしないもの」の
  一覧（進捗なし・継続の上限到達は失敗にしない、を明記）と合わせて読み、**WU failed からの replan
  が尽きた場合は D12 3. のとおり `failed`**、**進捗なし/継続の上限（"limit"）からの replan が尽きた
  場合は D18 のとおり `blocked`**、という 2 段構えで実装した（`finish_worker_result` の
  `matches!(decision.reason, "failed" | "limit")` の分岐。replan できないときは元の
  `decision.trigger`（"failed" → `WorkerError{retryable:false}`、"limit" →
  `WorkerQuestion`）にそのまま戻すだけで、この 2 段構えは自然に実現される）。

- **replan で持ち越す（`done` ではない）WU の `runs`/`continuations`/`retries` はリセットする**。
  D17 の本文には明記が無いが、D18 の「（人の回答は）最後の `answer` 以降の events を数える」と同じ
  発想で、replan も「窓を作り直す」機会だと解釈した。リセットしない場合、retries を使い切った直後の
  WU が replan でそのまま持ち越されても、次の 1 回の失敗で即座にまた「上限に到達」してしまい、
  replan の意味が薄れる。

- **`execution_plans` に `supersedes`/`reason` の列は追加していない**。この 2 つは既に
  `Event::ExecutionPlanned` に持たせてある（E2 の時点で用意済み）ので、DB の行に重複させず、
  `GET /tasks/{id}/execution-plan` の `versions`（(f)）は `id`/`version`/`origin`/`planner_run_id`/
  `status`/`created_at`/`superseded_at` だけを返す。`supersedes`/`reason` を監査したい場合は
  events を読む（GUI での表示は E5 の範囲）。

- **E2b の申し送り「`execution_plans` の版の履歴の再構築（replan 導入後）も replay の対象に加える」
  は E4 でも見送った**。`task_ops::replay::rebuild_work_units_and_runs` と
  `celerisctl replay --check/--apply` は今回変更していない（触るファイルの範囲外。`replay.rs` は
  ADR 表にも今回の指示にも含まれていない）。replan 後に `--apply` を伴わない状況で
  `execution_plans` が events と食い違うケースの検出は、次の一手として記録する。

- **(h)（配送の repair を merge_base の repair WU に置き換える）は未実装**。ADR 自身が「任意。
  時間があれば」としている項目で、E4 の必須の受け入れ条件 (a)〜(g) の実装・検証を優先し、時間の
  制約により見送った。`RepairClass::MergeBase` の型・budget は用意済み（`classify_review_failure`
  自体からは返らない設計。ADR 本文の該当コメント参照）なので、E5 以降で
  `crates/celeris/src/delivery.rs` 側から直接 `RepairClass::MergeBase` を使って repair WU を組み立てる
  実装を足すのは比較的小さい追加になる見込み。
