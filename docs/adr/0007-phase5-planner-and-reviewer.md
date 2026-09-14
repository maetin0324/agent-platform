# ADR-0007: Phase 5 — Planner（`Plan` kind）と Reviewer（`Reviewer` check）

- 日付: 2026-09-13
- 状態: Accepted（Phase 5）
- 関連: `docs/DESIGN.md` §4.2, §5.6, §5.7, §6 Phase 5 / [ADR-0002](0002-state-machine.md) D3/D6 /
  [ADR-0003](0003-worker-protocol.md) / [ADR-0005](0005-phase3-dispatch-and-worker.md) D1/D4/D5 /
  [ADR-0006](0006-phase4-claude-code-adapter.md) D2/D3

## 文脈

Phase 5 は「`Plan` kind、`PlanOutput` schema 検証、`Reviewer` check 種別」を実装し、
`taskctl plan "<大目標>"` が 3〜6 個の子タスクを生成して `plan.auto_accept=false` で人間承認後に
全て `done` になること（fake ワーカーで再現可能なテスト付き）を受け入れ条件とする。
未確定だった点:

(a) プランナーの出力（子タスク群 JSON）をワーカープロトコルのどこで受け取るか、
(b) `PlanOutput` / `NewTask` の形と検証規則（深さ上限 3、依存の表現）、
(c) 子タスクの挿入と親 `Plan` の `ReviewPass` をどう原子的に行うか（ADR-0002 D6）、
(d) `Reviewer` check を「別プロセス・別プロンプト」で、かつディスパッチャに LLM 呼び出しを書かずに
どう実現するか（ADR-0005 D1 の予告）、(e) `taskctl plan` の CLI→`Task` 写像、(f) 設定 `plan.auto_accept`。

## 決定

### D1. kind 別の出力ファイル規約（`artifacts/plan.json`, `artifacts/review.json`）

ワーカープロトコル（`done`/`error`/`question`）は変更しない。kind ごとに追加で **作業ディレクトリ直下の
ファイル** を出力させ、それをディスパッチャ側の決定的コードが読む:

| kind | ワーカーが書くファイル | 内容 | 読む側 |
|---|---|---|---|
| `Plan` | `artifacts/plan.json` | `PlanOutput`（D2） | Reviewer（Plan 用の決定的検証。D4） |
| `Review` | `artifacts/review.json` | `ReviewOutput{verdicts:[{criterion,pass,reason}]}` | Reviewer（D5） |

理由: ADR-0003 D8 / ADR-0006 D3 の「結果ファイル規約」と同じ形にすれば `fake`（sh スクリプト）でも
`claude-code`（プロンプト指示）でも同じ経路で動き、stdout の JSON 解釈に頼らない。`artifact` メッセージで
申告されていなくてもファイルがあれば読む（`ArtifactExists` の fallback と同じ）。
リトライ時に前回の run のファイルを誤読しないよう、ディスパッチャは **run 開始前に `artifacts/plan.json`
（Plan kind の run）／`artifacts/review.json`（Review run）を削除する**（ADR-0006 監査の教訓）。

### D2. `PlanOutput` は `task-core::plan` の純粋な型と検証関数

```rust
pub struct PlanOutput { pub tasks: Vec<NewTask> }
pub struct NewTask {
    pub title: String, pub objective: String,
    pub acceptance: Vec<Criterion>,        // task-core の Criterion をそのまま（Check は 4 種全て可）
    #[serde(default)] pub depends_on: Vec<usize>,  // 同じ plan.json 内の tasks のインデックス
    #[serde(default)] pub kind: NewTaskKind,        // execute（既定）| plan
    #[serde(default)] pub tier: Option<Tier>,        // 省略時 Standard
}
pub fn validate(plan: &PlanOutput, plan_depth: u32, limits: &PlanLimits) -> Result<(), PlanError>;
pub fn materialize(parent: &Task, plan: &PlanOutput, now: OffsetDateTime) -> Vec<Task>;
```

- `#[serde(deny_unknown_fields)]`（プランナーの綴り間違いを検出するため。ワーカープロトコルの
  「未知フィールドは無視」とは逆の方針で、意図的な違い）。
- 検証規則（すべて決定的）: `tasks` が `limits.min_tasks..=limits.max_tasks` 件（既定 1..=20。
  DESIGN §6 の「3〜6 個」は受け入れ条件であって上限規則ではないので既定にはしない）、
  `title`/`objective` が空でない、`acceptance` が 1 件以上、`depends_on` のインデックスが範囲内で
  自己参照無し、依存グラフが DAG（閉路無し）、`kind = plan` の子は `plan_depth + 1 <= 3` のときだけ許す
  （DESIGN §5.6「深さ上限 3」。`plan_depth` = その Plan タスク自身を含む祖先の `Plan` の数。
  `taskctl plan` で作った根は 1）。
- `materialize`: 子は `id` 新規、`parent_id = parent.id`、`status = Draft`（ADR-0002 D4）、
  `priority`/`workspace`/`budget`/`worker_hint.adapter` を親から継承、`worker_hint.tier` は `NewTask.tier`
  （省略時 `Standard`）、`depends_on` はインデックスを新しい `TaskId` に写す、`inputs = []`。
- JSON Schema は `schemars` で生成し `docs/protocol/plan-output.schema.json` にコミット、テストで一致を
  検証する（ADR-0003 D6 と同じ運用。`UPDATE_SCHEMA=1` で再生成）。
- スキーマ検証は「`serde` でのデシリアライズ + `validate`」で行う。`jsonschema` クレートは追加しない
  （型定義とスキーマの正が一致していることはテストで保証されており、二重の検証器は不要）。

### D3. 子の挿入と親の `ReviewPass` は `TaskStore::complete_plan` で同一トランザクション

```rust
fn complete_plan(&self, plan_id: TaskId, verdict_events: Vec<Event>, children: Vec<Task>, accept_children: bool)
    -> Result<Outcome, StoreError>;
```

1 トランザクションで: 各子を `insert` + `Event::Created`（`accept_children` なら続けて `Trigger::Accept` を
適用し `Transitioned{draft→ready, "accept"}` を追記）→ 親に `Trigger::ReviewPass` を適用し
`Transitioned` + `verdict_events`（`ReviewVerdict`）を追記 → commit。親の遷移が無効なら全てロールバック。
ADR-0002 D6「auto_accept が true なら親 done と同一トランザクションで Accept」をそのまま満たす。
`replay` は子の `Created{task.status = draft}` と `Transitioned` から再構築できるので差分ゼロが保たれる。

### D4. Plan kind のレビュー = 通常の条件判定 + 暗黙の「プラン検証」条件

`reviewing` に入った `Plan` タスクに対し Reviewer は、`task.acceptance` の各条件を他 kind と同じく判定した
うえで、**追加の判定 1 件**（`criterion_idx = task.acceptance.len()`、暗黙の条件「`artifacts/plan.json` が
`PlanOutput` として妥当」）を行う。`taskctl plan` が作る Plan タスクは `acceptance = []` なので、この暗黙の
条件が `criterion 0` になり、リトライ時の `context.prior_review[0].reason` に検証エラー（例:
`tasks[2].depends_on[0] = 7 is out of range`）が載る。

- 全 pass → D3 の `complete_plan`（`accept_children = plan.auto_accept`）。
- fail あり → 通常どおり `ReviewFail`（ADR-0002 D3 のリトライ判定。DESIGN §5.6「不正なら 1 回だけ再試行」は
  `taskctl plan` の既定 `max_retries = 1` で表現する）。
- `Check` enum に `PlanValid` のような新種を足さない（DESIGN §4.1 の 4 種を維持。Plan の検証方法は kind で
  決まるので条件として持たせる必要が無い）。

### D5. `Reviewer` check はワーカーアダプタ経由の別 run（ディスパッチャに LLM 呼び出しを書かない）

DESIGN §5.7「`Standard` tier のワーカーに受け入れ条件・証拠・成果物を渡し `{"pass","reason"}` を返させる。
ワーカーとは別プロセス、別プロンプト」を次で実現する:

1. `reviewing` のタスクに `Check::Reviewer` の条件があれば、レビュー開始時に `ProviderPolicy::pick(
   WorkerHint{tier: Standard, adapter: None})` でアダプタ／プロバイダを選ぶ。候補が無い、または並列度上限
   （全体・プロバイダ別。**レビュー run も実行中 run と同じ枠を消費する**）に達していれば、その tick では
   レビューを開始せず `reviewing` のまま残す（次 tick の `recover_reviews` が拾う）。
2. まず決定的な条件（`Command`/`ArtifactExists`）を判定する。**1 件でも fail なら LLM レビューは実行しない**
   （`Reviewer` 条件は `pass=false, reason="not evaluated: a deterministic check failed"`）。無駄な LLM 呼び出しを
   避けるため。
3. 決定的条件が全 pass なら、アダプタに **合成した `Review` kind の `Task`**（永続化しない。`id` 新規、
   `parent_id = 対象タスク`、`title = "Review: <対象 title>"`、`objective`/`acceptance`/`workspace`/`budget` は
   対象と同じ、`worker_hint = {Standard, None}`）を `RunRequest.task` として渡す。`RunRequest.context` に
   新フィールド **`review: Option<ReviewRequest{ summary, evidence, criteria }>`** を追加し
   （`criteria` = `Reviewer` 条件のインデックス、`summary`/`evidence` = 対象 run の `done` の内容）、
   `context.inputs` にはその run の `ArtifactProduced` を入れる。`skip_serializing_if = None` の追加フィールド
   なので既存の `fake` スクリプト／スキーマ利用者との互換は保たれる（`worker-protocol.schema.json` は再生成）。
4. run は同じ作業ディレクトリで実行する（成果物を直接読めるようにするため）。プロンプトで「読み取り専用、
   ファイルを変更しない」と指示するが、強制はしない（既知の制約として記録）。
5. 終端が `done` なら `artifacts/review.json` を `ReviewOutput` として読み、`criteria` の各インデックスに対応する
   `verdicts` を採る。無い／不正／インデックス欠落／`done` 以外の終端 → 該当 `Reviewer` 条件を `pass=false`
   （理由に原因）。この fail も `ReviewFail` として `attempts` を消費する（供給側失敗で消費しない案は P-21 と
   同じ論点として提案に残す）。
6. 対象タスクのイベントには `WorkerProgress{run_id: 対象 run, msg: "reviewer run <review_run_id>: ..."}` で
   レビュー run の開始・終了を残し、判定は通常どおり `ReviewVerdict{run_id: 対象 run, criterion_idx, pass,
   reason: "reviewer(<review_run_id>): <reason>"}` として `Transitioned` と同一トランザクションで記録する。
   `WorkerStarted`/`WorkerFinished` はレビュー run には使わない（`last_run_id()` と `artifacts_for_run()` が
   対象 run を指し続けるようにするため）。
7. `Review` kind のタスクを人間が `taskctl add --kind review` で作った場合の扱いは変えない（ディスパッチは
   他 kind と同じ。Phase 5 では特別扱いしない）。

`evidence` は `WorkerFinished.outcome` 文字列に含まれない（Phase 3 の形式を維持）ため、レビュー開始時に
メモリ上の `RunOutcome` から渡す。デーモン再起動後の `recover_reviews` では `runs/<run_id>/result.json`
（`fake`/`run_subprocess` が書く終端メッセージ）があればそこから復元し、無ければ空で渡す。

### D6. `taskctl plan "<大目標>"`

`Task{ kind: Plan, title: 目標の先頭 80 文字（1 行目）, objective: 目標全文, acceptance: [],
worker_hint: {tier: Frontier（`--tier` で変更可）, adapter: None}, workspace: `--workspace`（既定カレント）,
budget: {max_turns: 30, max_wall_secs: 900, max_retries: 1}（各 `--max-*` で変更可）, priority: `--priority`,
status: Draft }` を `insert` + `Created`。`taskctl approve <id>` で `ready` になる（ADR-0002 D4）。
`--parent` は受け付けない（根の Plan のみ。入れ子の Plan はプランナーの出力で作る）。

### D7. 設定と配線

- `taskd.toml` に `[plan] auto_accept = false`（既定 false）を追加し `DispatchConfig.plan_auto_accept` に写す。
- `build_prompt`（claude-code, ADR-0006 D2）は `task.kind` で分岐する: `Plan` なら `PlanOutput` の JSON Schema
  （`schemars` 生成物の要約）と `artifacts/plan.json` の指示、`Review` なら `context.review` の条件・証拠・成果物一覧と
  `artifacts/review.json` の指示。どちらも `artifacts/result.json`（ADR-0006 D3）は従来どおり必要。
- `docs/protocol/worker-protocol.md` に §10「kind 別の出力ファイル」を追加。

## 結果

- `task-core`: `plan.rs`（`PlanOutput`/`NewTask`/`validate`/`materialize`/`PlanError`）、
  `TaskStore::complete_plan`、`docs/protocol/plan-output.schema.json`。
- `task-worker`: `protocol.rs` に `ReviewRequest`/`ReviewOutput`/`ReviewVerdictOut` と `RunContext.review`、
  `claude_code.rs` の `build_prompt` を kind 別に、`worker-protocol.schema.json` 再生成。
- `task-dispatch`: `review.rs` に Plan 検証と `Reviewer` run の判定（アダプタ経由）、`dispatcher.rs` に
  レビュー run の選択・並列度・イベント記録と `complete_plan` 呼び出し、`DispatchConfig.plan_auto_accept`。
- `taskd`: `[plan]` 設定。`taskctl`: `plan` サブコマンド。`tests/e2e`: Phase 5 シナリオ。

## DESIGN.md 修正提案（本 ADR のスコープ分）

- **P-27（§5.3 context）** `context.review` の追加を明記する（D5。実装済み）。
- **P-28（§5.6）** `PlanOutput` の出力経路が「ワーカー出力として返す」としか書かれていない。
  `artifacts/plan.json` の規約（D1）を追記する。
- **P-29（§5.7 Reviewer）** レビュー run の失敗（プロバイダ無し・タイムアウト・不正な `review.json`）で
  `attempts` を消費しない扱い（P-21 と同種）の要否。
- **P-30（§5.7）** `Reviewer` 条件の判定に使うプロバイダを設定で指定する `[reviewer] adapter = "..."` の要否
  （現状は `Standard` tier の最初のプロバイダ）。
