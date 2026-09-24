# Task execution architecture の現状調査（Phase E0、2026-09-24）

この文書は ADR-0072（Task execution decomposition）の前提となる**現状の事実**をまとめる。設計判断は書かない（ADR-0072 に書く）。
行番号は main `d792c63` 時点のもの。パスはリポジトリ相対。`D` = `crates/task-dispatch/src/dispatcher.rs`（19,485 行）。

調査のきっかけになった症状（人の依頼の要旨）: routing 再設計、Knowledge GC、大規模 GUI、調査→設計→実装→テスト→review→release の
ような大きな依頼が、ほぼ「1 Task = 1 worker session」で dispatch される。context が膨らみ、max turns や context limit に当たり、
session が途中で終わると Task 全体が `failed` になる。reviewer の `cargo fmt` 程度の指摘でも Task 全体が再投入される。

---

## 0. 結論（先に要点）

1. **Task と worker run は実質 1:1 で結合している。** 結合している箇所は次の 5 つ。
   - (a) dispatcher のメモリ上の `running: HashMap<TaskId, RunEntry>`（D:1279）。1 Task につき同時に 1 run まで。
   - (b) lease は `tasks` 行に 1 つだけ（`lease_worker_run_id`）。`acquire_lease`（`crates/task-core/src/store.rs:2428`）が Ready→Running と lease の設定を同時に行う。
   - (c) run の終わりは必ず Task の `Trigger` に写る（`on_worker_finished` D:2873）。
   - (d) `attempts` は Task の欄で、run の失敗 1 回ごとに消費される（`transition.rs:338`、`retry_or_fail` `transition.rs:136`）。
   - (e) 仕事の run はセッションを持たない（`--no-session-persistence`、`crates/task-worker/src/claude_code.rs:830`）。**resume の単位は「ノード（CoS / 部門長）」であってタスクではない。**
2. **`error_max_turns` と wall-clock 超過は、普通の「retryable なワーカー失敗」として扱われる。** 経路は `Terminal::Error{retryable:true}` → `Trigger::WorkerError` → `attempts+1`。`max_retries`（既定 2）を使い切ると Task が `failed` になる。次の試行は**何も引き継がない新規 run**で、同じ worktree で最初からやり直す。前の run が何をしたかを伝える経路は無い（渡るのは `prior_review` と `answers` だけ）。さらにこの経路では **usage（トークン数）が捨てられる**（`claude_code.rs:1236-1253`。usage を持つのは `Done` だけ）。
3. **reviewer の不合格は Task 全体の再実行になる。** 経路は `Trigger::ReviewFail` → `retry_or_fail` → `Ready`（D:3658）。次の run は元の objective 全体と `prior_review` で最初から始まる。「repair（局所修復）」の仕組みは dispatch 側にも review 側にも無い。似たものは配送の `[delivery-repair]`（`crates/celeris/src/delivery.rs:338-353`）だけで、これも `Trigger::Reopen` で**タスク全体を開き直す**。
4. **分解の仕組みは 2 つある（`Plan` kind の `plan.json` と実行中の `delegate.json`）が、どちらも「ユーザーに見える子 Task」を作る。** Task の内部に閉じた実行計画の層は無い。
5. **`TaskFeatures`（9 軸）は lane（モデル品質の帯）を決めるためだけに使われている。** 「1 session で実行してよいか」の判断には使われていない。
6. **turn / wall の予算をモデルに知らせるプロンプトの文面は無い。** `--max-turns` を渡すのは claude-code だけで、codex / acp / aider は turn の上限を持たず wall-clock だけで止まる。

---

## 1. Chat / GUI / CoS から Task が作られるまで

| 経路 | 入口 | Task を作る関数 | 出自（`SpecOrigin`） |
|---|---|---|---|
| GUI / API | `POST /api/v1/tasks`（`crates/task-api/src/handlers.rs:76` の route → `create_task` `:1106`） | `task_ops::add::create_task_with_roles`（`crates/task-ops/src/add.rs:401`）→ `build_task`（`:449`） | `Human`（既定。`serde(skip)` なので API からは偽装できない。`add.rs:177`） |
| CoS（Chat） | 対話 run が `result.json` の `actions` に `create_task` を書く（`crates/task-core/src/console_action.rs:31-62`）→ `absorb_console_actions`（D:3305）→ `task_ops::actions::execute`（`crates/task-ops/src/actions.rs:100`） | `create_task_action`（`actions.rs:250`）→ `create_task_with_roles` | `Agent`。担当と tier は捨てる。人の発言に `@node` / `tier:` があるときだけ採る（`human_mentions_node` / `human_mentions_tier`、`actions.rs:415/429`。ADR-0069 D1） |
| 対話そのもの | `task_ops::conversation::start`（`crates/task-ops/src/conversation.rs:52`）。`CONVERSATION_MAX_TURNS = 10`（`:26`） | 同上 | — |
| 計画（`Plan` kind） | 計画 run の `artifacts/plan.json` → review pass 後に `materialize` → `store.complete_plan`（D:3604-3645） | `task_core::plan::materialize`（`crates/task-core/src/plan.rs:489`） | Agent（担当は捨てる） |
| 実行中の委譲 | run の `artifacts/delegate.json` → `forward_delegate_file`（`claude_code.rs:1041`）→ `StoreSink::delegate_impl`（D:921） | `task_core::delegate::materialize_delegated`（`crates/task-core/src/delegate.rs:467`） | Agent |
| 裏方（support） | tick の決定的な判断（報告のまとめ、知識整理、Knowledge GC、doc gardener、案件計画） | `create_support_task`（`add.rs:420`）。受け入れ条件ゼロ可、`Ready` で開始 | System |

- `build_task` は既定値を「タスクの値 > 役割 > 担当の分野の既定役割 > 分野の既定役割 > 全体」の順で埋める（`add.rs:604-651`）。`Task.routing = TaskRouting{tier_source, assignee_explicit, dropped_assignee, features}` を記録する（`:621-628`）。**routing（lane）は作成時には計算しない**。記録するのは出自だけで、lane は dispatch 時に決まる（§3）。
- 予算の既定: 通常のタスクは `DEFAULT_MAX_TURNS = 10` / `DEFAULT_MAX_WALL_SECS = 600` / `DEFAULT_MAX_RETRIES = 2`（`add.rs:308-310`）。計画の子は `max_turns 30 / wall 900 / retries 1`（`crates/task-ops/src/plan.rs:135-137`）。**大きな依頼でも、CoS が `max_turns` を書かない限り 10 turn / 10 分で走る**。

## 2. Task の dispatch（D の tick）

`Dispatcher::tick`（D:1985）が毎 tick 次の順に回す。

1. `drain_completions`（D:2846）
2. `settle_awaiting_children`（D:3919）
3. `reclaim_expired_leases`（D:4008）
4. `abort_stale_runs`（D:4133）
5. `cleanup_cancelled_worktrees`（D:7102）
6. `recover_reviews`（D:4167）
7. cluster の更新
8. `dispatch_ready`（D:4260）

**`dispatch_ready` の流れ**
- 候補は `store.ready_tasks(window)`（`store.rs:2556`）で取る。条件は `status = ready AND kind != approval` で、並びは `priority DESC, created_at ASC`。止められた案件・途中目標、依存が `done` でないもの、`Approval` の親が未完のものは除く。ADR-0002 D5 のとおり、依存待ちは状態ではなく「dispatch できるか」で表す。
- 候補ごとの関門:
  - `assign_if_needed`（D:6946。matching が決まらなければ `Trigger::Unroutable` → `blocked`）
  - `retry_backoff`（attempts > 0 のとき。D:4296-4306）
  - `infra_backoff`（D:4313。プロセス内メモリのみ）
  - クラスタ
  - `decide_lane`（D:6898）
  - `select_provider`（D:6282）→ `select_tier` → `model_for_tier`
- 確保と起動:
  - `run_id = ULID`（D:4502）
  - `acquire_lease(task.id, &run_id, wall + lease_grace)`（D:4506）
  - `Event::WorkerStarted`（D:4517）、`Event::RoutingDecided`（D:4558。`routing` を持つ execute タスクだけ）
  - `spawn_worker`（D:5616）→ `run_worker`（D:7396）→ `adapter.run(req, run_id, limits, &sink)`
  - `self.running.insert(task.id, RunEntry{..})`（D:4657）
- 並列度: `workers_in_flight()`（D:4200）= `running.len()` + provider を使うレビュー。これを `max_concurrency` と比べる。
- lease の更新: `StoreSink::heartbeat`（D:1105）が stdout の行ごとに `renew_lease` を呼ぶ（busy なら最大 3 回まで再試行。ADR-0070 D5）。
- `run_worker` は run ごとに次を行う: タスクを読み直す → worktree を `ensure`（既存を再利用）→ setup → `RunRequest` を組む。コンテキストに入るのは `prior_review = prior_review_from_events(..)`（D:7650。直前にレビューした run の verdict だけ。`crates/task-ops/src/derive.rs:30`）、`answers`、`comments`、`interrupt`、`profile`、`knowledge`、`session`（CoS だけ）など（`RunContext`、`crates/task-worker/src/protocol.rs:361-497`）。

## 3. routing（ADR-0069 の 4 層と `TaskFeatures`）

| 層 | 実装 | 記録 |
|---|---|---|
| Ownership | `task_ops::matching::decide`（`crates/task-ops/src/matching.rs:77`）を `assign_if_needed`（D:6946）が呼ぶ | `Event::Assigned` |
| Harness | `genre` と `worker_hint.adapter`（`add.rs:647-651`）。`task_core::routing::StaticRoutingPolicy`（ADR-0061）はライブラリのみで、呼ぶ側が無い | `WorkerStarted.adapter` |
| Model | `ModelPolicy`（`crates/task-core/src/model_policy.rs`）→ `model_routing::resolve` / `select_tier` → `TieredAdapter` | `Event::RoutingDecided{record: RoutingRecord}`（`model.rs:1019`、`model_policy.rs:524`） |
| Review | `review.rs` と `EscalationPolicy`（`crates/task-core/src/retry_policy.rs:109`）、reviewer の lane（Phase 118 D4、D:6008 `pick_reviewer`） | `ReviewVerdict` と `RoutingDecided`（reviewer run） |

- **`TaskFeatures`**（`model_policy.rs:43-62`）の 9 軸は judgment / ambiguity / verifiability / reversibility / consequence / context_size / tool_intensity / expected_length / cross_cutting で、値は各 `Level::{Low,Medium,High}`（`:25`）。`TaskFeatures::infer(&Task)`（`:247-370`）は LLM を使わず決定的に計算する。
  - `context_size`: `repos ≥ 2` または `objective > 4000` 文字なら High（`:324`）。
  - `tool_intensity`: remote、または道具系ハーネスで command 検査があれば High。
  - `expected_length`: `max_wall_secs ≥ 3600` または `max_turns ≥ 60` なら High（`:340`）。
  - `cross_cutting`: `repos ≥ 2`、`skills ≥ 4`、または横断語を含めば High。
  - 明示の上書きは `TaskFeatureHints`（`:67`。`Task.routing.features`）。
- **使い道は lane を決めることだけ**: `ModelPolicy::decide`（`:597`）の規則表（`rule_id` は `frontier/...`・`cheap/...`・`standard/default`）で決まる。**「1 session で実行してよいか」を判定する箇所は無い。**
- 監査の集計: `routing_audit` という**テーブルは無い**。`task_core::routing_audit::routing_audit(&Task, &[Event])`（`crates/task-core/src/routing_audit.rs:68`）が events から組み立てる純粋関数で、API は `crates/task-api/src/routing.rs:35`。

## 4. worker / harness の呼び出し

共通の契約は `trait WorkerAdapter::run(RunRequest, run_id, RunLimits, &dyn EventSink) -> Result<RunOutcome, AdapterError>`（`crates/task-worker/src/adapter.rs`）。

- `Terminal = Done{summary, evidence, usage} | Question{text} | Error{message, retryable}`（`adapter.rs:15-28`）。
- `RunLimits{wall_clock, idle_timeout, kill_grace}`（`:41`）。
- `AdapterError` は Spawn / Io / Serde / Throttled / AuthFailed / Exhausted / Other（`:51`）。

結果ファイルの規約は ADR-0006 D3 で決まっている。

- 置き場所は `<artifacts_dir>/result.json`。書式は `{"summary","evidence"}` か `{"question"}` のどちらか。モデルへの指示は `result_json_instructions`（`claude_code.rs:422`）が出す。
- 任意の追加フィールドは `crates/task-worker/src/result_report.rs` が読む: `report`、`milestone_proposal`、`actions`、`memory`。
- 正規化した写しを `runs/<run_id>/result.json` に置く（`subprocess::write_result_json`、`crates/task-worker/src/subprocess.rs:327`）。
- Phase 115 の救済: worktree 相対で書かれた結果を拾う（`adopt_result_json_written_under_work_dir`、`subprocess.rs:362`）。

| harness | 入口 | turn の上限 | 終わり方の判定 | セッション（resume） |
|---|---|---|---|---|
| claude-code | `run_claude_code`（`claude_code.rs:736`） | `--max-turns <budget.max_turns>`（`:790-791`） | stream-json の `result` メッセージ。`is_error` か `subtype != success`（`error_max_turns` / `error_during_execution`）なら **result.json より優先して** `Error{retryable:true, "claude result: <subtype>"}`（`:1236-1253`）。この経路では usage を捨てる。wall-clock 超過は `"wall clock exceeded"`（`:907-914`）、idle は `"idle timeout"`（`:916-922`）、`result` メッセージが無ければ `"worker exited without a result message"`（`:977-1001`） | 仕事の run は `--no-session-persistence`（`:830`）。CoS / lead だけ `--session-id` か `--resume`（`:808-832`）。stdin は null（`:855`）なので、run の途中からは指示を注入できない |
| codex | `run_codex`（`crates/task-worker/src/codex.rs:563`）→ `run_codex_once`（`:283`） | **無い**（`budget.max_turns` を使わない） | `turn.failed` → `Error "codex turn failed"`（`:750`） | `exec resume <id>` / `experimental_resume`（Phase 67/68/112）。`thread.started` で id が確定する |
| acp（opencode など） | `run_acp`（`crates/task-worker/src/acp.rs:785`） | **無い**（ACP の `stopReason` を文言に残すだけ） | `terminal_from_result_file(.., stop_reason)`（`:737-779`） | `session/new` / `session/load`。`session/cancel` がある（`:658-662`） |
| aider | `run_aider`（`crates/task-worker/src/aider.rs:137`） | 無い（1 回の `--message`） | result.json | 無い |
| paperqa / LDR / langmem | `run_paperqa`（`paperqa.rs:1614`）/ `run_ldr`（`local_deep_research.rs:390`）/ `run_langmem`（`langmem.rs:189`） | エンジン固有（LDR は `iterations`） | **アダプタ自身が** result.json を書く | 無い |

- プロンプト: `claude_code::build_prompt`（`claude_code.rs:139`）→ `prompt_header`（`:153`。`(run {run_id}, attempt {n} of {max})`）→ `preamble::render`（`crates/task-worker/src/preamble.rs:41`）→ 分野 → `## Objective`。codex と acp も同じ組み立てを使う。
- **予算についての文面は無い**: max_turns・残り turn・残り時間をモデルに伝える文面はどこにも無い（`preamble.rs` / `claude_code.rs` を grep して確認）。モデルが知る予算は「試行の何回目か」だけ。
- **graceful yield**（予算が尽きる前に自分で区切って止まること）は、どの harness にも無い。

## 5. session / run の状態

- **`node_sessions`**（migration 0023、`crates/task-core/migrations/0023_node_sessions.sql`、型は `crates/task-core/src/node_session.rs`）。
  - 列: `node_id, kind('conversation'|'lead'), project_id, adapter, account_id, session_id, turns, approx_tokens, created_at, last_used_at, retired_at`。
  - 対象は **CoS の対話（1 本）と部門長のレビュー run（部署ごとに 1 本）だけ**。
- 判断の純粋関数は `crates/task-dispatch/src/sessions.rs` にある。
  - `decide`（`:58`）: NoActive / ResumeFailed / AdapterChanged / InvalidSessionId / AccountChanged / RolloverExceeded なら新規（Fresh）、それ以外は Resume。
  - `decide_sticky`（Phase 67c）。
  - rollover は `approx_tokens >= [sessions] rollover_tokens`（既定 400k）。
- I/O は `resolve_node_session`（D:5081）が行う。
- self-heal: アダプタが再開の拒否を検出すると `EventSink::session_resume_failed` → `node_session_retire` になり、次の run は新規セッションになる（D:1168、ADR-0054 Phase 67/67b）。
  - Phase 113 D1: `result` メッセージを観測できた run（`error_during_execution`）でも判定する（`claude_code.rs:1028-1036`、`provider.rs::looks_like_resume_rejection`）。
  - Phase 112: codex の `exec resume` で承認とサンドボックスの設定を落とさないよう `-c` に翻訳し、翻訳できなければ新規スレッドにする。対話で result.json が無いときは最終メッセージから回収する。
- **run の状態を表すテーブルは無い。** run は events 上の `WorkerStarted`〜`WorkerFinished` の組（同じ `run_id`）として現れるだけ。
  - 表示用には `task_ops::view::RunSummary`（`crates/task-ops/src/view.rs:214`）が events から組み立てる。
  - run の終わり方は `WorkerFinished.outcome` の**文字列**で、次の 2 か所が字句で分類している。
    - `view.rs:463 classify_outcome`
    - `crates/task-api/src/stats.rs:48 classify_outcome`
  - `WorkerFinished.outcome` の接頭辞は `done:` / `question:` / `error(retryable=..):` / `requeue: adapter:` / `infra_requeue:` / `infra failure ×N:` / `interrupted:`。
- `RunContext.session` は、仕事の run では常に `None`（`protocol.rs:472-477` の doc）。**Execute タスクには run をまたぐ会話の状態が無い**（DESIGN 原則 2「ワーカーはステートレス」の実装どおり）。

## 6. max turns / 予算 / lease の扱い

- `Budget{max_turns, max_wall_secs, max_retries}`（`crates/task-core/src/model.rs:192`）。
  - `max_turns` を効かせているのは claude-code の `--max-turns` だけ。
  - `max_wall_secs` は `RunLimits.wall_clock`（D:4503 `wall`）と lease の ttl（`wall + lease_grace`、D:4504）に使う。
- lease が切れたとき（`reclaim_expired_leases`、D:4008）:
  - プロセスが生きていれば延長する（ADR-0070 D5）。
  - 死んでいれば `InfraRequeue`（attempts を消費しない）。上限を超えたら `WorkerError{retryable:false}` で `"infra failure ×N: lease expired"` になる。
  - `Trigger::LeaseExpired` は状態機械に残っているが、dispatcher からはもう使っていない。
- **`error_max_turns` / wall-clock 超過の経路**:
  1. `Terminal::Error{retryable:true}`（`claude_code.rs:1236-1253` / `:907-914`）
  2. `on_worker_finished` の `Ok(Terminal::Error)` 分岐（D:2966-2974）
  3. `Trigger::WorkerError{retryable:true}`
  4. `transition.rs:338-353`: `attempts+1`。`attempts <= max_retries` なら `Ready`、超えたら `Failed`
- 監査側では `retry_policy::is_budget_outcome`（`retry_policy.rs:251`、`"max_turns"` / `"wall clock"` / `"budget"` の字句判定）が `AttemptOutcome::BudgetExhausted` に分類する。使い道は「lane を上げない」（`never_escalate_on`、`:128`）だけで、**状態遷移は他の失敗と同じ**。
- `classify_task_failure`（`crates/task-ops/src/derive.rs:204`）は max_turns 超過を **`work`** に分類する（ADR-0070 D1。テスト `derive.rs:640` の `"error(retryable=false): max_turns exceeded"`）。
- 経験知: 知識ベースと PROGRESS に「大きな PoC を 1 session でやると `error_max_turns` になる」という記録が繰り返し出てくる。
  - `docs/PROGRESS.md:3912`、`:5372`
  - ADR-0039 §1
  - `conversation.rs:24`（対話の 6 → 10 turn）
  - ADR-0022 M2（`max_turns=1` の検査）

  それでも architecture はこれを強制していない。

## 7. retry（`Trigger::Retry` / `InfraRequeue` / `EscalationPolicy`）

`attempts` の意味（ADR-0002 D3）は「成功で終わらなかった実行の回数」。どのトリガーで attempts がどう動くかは次のとおり。

| トリガー | 遷移 | attempts | 根拠 |
|---|---|---|---|
| `WorkerError{retryable:true}` | Running → Ready \| Failed | +1 | `transition.rs:338` |
| `WorkerError{retryable:false}` | Running → Failed | +1 | 同上 |
| `ReviewFail` | Reviewing → Ready \| Failed（`retry_or_fail`、`:136`） | +1 | `:376` |
| `Requeue`（供給側失敗） | Running → Ready | 据え置き | `:172`。上限は `consecutive_requeues < max_requeues`（D:2976-2990、`derive.rs:70`） |
| `InfraRequeue`（Phase 116） | Running → Ready | 据え置き | 上限は `consecutive_infra_requeues + 1 <= max_infra_retries`（既定 5、D:2991-3016）。バックオフは 30 秒 / 2 分 / 5 分（`derive.rs:146`） |
| `Reopen` | → Ready | 0 に戻す | ADR-0044 D2 |
| `Rereview`（Failed から） | → Reviewing | −1 | ADR-0054 Phase 113 D3 |
| `Answer` / `Interrupt` / `Aggregate` | — | 据え置き | — |

- `replay` は `reason ∈ {worker_error, lease_expired, review_fail}` の件数で attempts を復元する（`crates/task-ops/src/replay.rs:48`）。
- **`Trigger::Retry` という名前のトリガーは無い。** 「やり直す」（`POST /tasks/{id}/retry`、`task_ops::retry::retry_task`）は**タスクを複製**して新しい Task を作り、`Event::Retried{from}` を残す（ADR-0070 D2。既定は `ready`）。
- `EscalationPolicy`（ADR-0069 D6、`retry_policy.rs:109`）は**lane を選ぶだけ**で、失敗させるのは状態機械。
  - `attempt_history`（`:266`）は Transitioned の理由から試行履歴を作る。`infra_requeue` は数えない。
  - 呼ばれるのは `decide_lane`（D:6915-6931）の中だけ。

## 8. review（`review.rs`、reviewer run、Phase 113）

- `spawn_review`（D:5680）の流れ:
  1. タスクごとのファイルロックを取る。
  2. 人の承認を解決する。
  3. `needs_reviewer_run`（`crates/task-dispatch/src/review.rs:150`）なら `pick_reviewer`（D:6008。部署の lead セッションを `resolve_node_session` で決める）。
  4. `review_task`（`review.rs:262`）。
- `review_task` の判定順:
  1. 決定的な検査（`Check::Command` を worktree で**再実行**。verdict の理由は `cmd=.. exit=.. expected=.. stdout_tail=.. stderr_tail=..`。`review.rs:286-305`）
  2. `ArtifactExists` / `KnowledgePage` / `Human`
  3. 暗黙の検査（`plan.json`、集約の `summary.md`、repo の `.config/celeris/workspace.toml [commands] check`（`:400-412`）、調査の証拠）
  4. 全部 pass したときだけ LLM reviewer run（`run_reviewer_inner`、`:614`。合成の `Review` タスク `synthetic_review_task` `:536` を作り、`artifacts/review.json` を読む）
- **verdict の `run_id` は worker run の id**（D:3563-3568）。レビューは「直前の worker run の成果」を判定する。
- 結果の適用（`on_review_finished`、D:3348）:
  - 全部 pass: 委譲した子を待つ（`awaiting_children`、D:1306）→ 子の失敗をエスカレーションする → 集約 run → `ReviewPass`（D:3571-3655）。
  - 1 つでも fail: `Trigger::ReviewFail`（D:3658）→ `retry_or_fail`。次の run は**元の objective 全体**に `prior_review`（「## Previous attempt's review result」、`claude_code.rs:391`）を足して最初から始まる。worktree は再利用するが、会話は無い。
- Phase 113:
  - reviewer run 自身のインフラ失敗は「判定不能」扱いになる（`ReviewerProviderFailure{outcome: None}`、`review.rs:92`）。`max_reviewer_retries`（既定 3）まで延期し、attempts は消費しない。
  - `Failed` からの `rereview` は、直前の遷移が `review_fail` のときだけ許す。
- 本番の実例（PROGRESS「Phase 116 の本番反映」）: 自己改善タスクの配送が **reviewer の `cargo fmt --check` 指摘**で全部落ち、そのたびに Task 全体をやり直していた。

## 9. repair

**局所修復の仕組みは無い。** 近いものは次の 3 つ。

- 配送の `[delivery-repair]`（`crates/celeris/src/delivery.rs:338-353`。関数 `advance` `:179`）: 取り込み・リリース検証で技術的に失敗したとき、`Done` のタスクへ system コメントを 1 回だけ付けて **`Trigger::Reopen`**（attempts を 0 に戻してタスク全体を再実行）。
- `rereview`（ADR-0054 Phase 113 D3）: 実装をやり直さず判定だけをやり直す。
- 再試行 run の `prior_review`: 不合格の理由は渡るが、元の objective 全体から再開するので、元の実装の context を毎回作り直すことになる。

## 10. worktree の lifecycle

- 配置（ADR-0043 D2、`crates/task-worker/src/task_repos.rs:1-17`）: `<workspace_root>/<task_id>/{repos/<name>/, artifacts/, inputs/, runs/, worktree.json}`。git リポジトリはブランチ `celeris/<task_id>` の worktree、`dir` はシンボリックリンク。旧形式は `tree/`（`local_worktree.rs:23`）。
- 作成と再利用: `TaskWorkspaces::ensure` / `LocalWorktree::ensure`（`local_worktree.rs:90`）。**タスク単位で 1 つ**で、再試行や再 run でも同じものを使う。どのリポジトリに worktree を作るかは `task_workspaces_for`（D:6600）が決める。
- 片付け: 終端になっても自動では消さない。中止（cancel）で消す（`cleanup_cancelled_worktrees` D:7102、`remove_for_cancel` `task_repos.rs:112`）。
  - Phase 110b（ADR-0066 D2）: 終端から `prune_after_secs` が経ったらビルド生成物だけを消す（`prune_one_workspace` D:7171、`workspace_prune.rs:69/133`）。
  - Phase 110b D1: 共有の `CARGO_TARGET_DIR`（`build_cache.rs`）。
- Phase 115 D3: diff を作らない内部タスク（`workspace_mode: Shared`）は worktree を作らない（`add.rs:564-598`）。

## 11. metrics

- `Usage{input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd}`（`model.rs:692`）と `RunMetrics{wall_ms, retries}`（`:711`）を `WorkerFinished` に載せる（ADR-0061 D2。D:3062-3072）。
- 費用: `task_core::pricing::estimate_cost_usd`（`crates/task-core/src/pricing.rs:75`、静的な単価表 `PRICE_TABLE` `:26`）。未解決事項 P-118-1: `claude-fable-5-1` は単価表に無く `None` になる。
- 集計: `task-api::stats`（run 単位の outcome 分類）と `routing_audit`（run 単位で lane・結果・費用を結ぶ。テーブルは無く events から導出）。
- **欠けているもの**:
  - (a) budget 切れで終わった run の usage（前述のとおり捨てられる）
  - (b) run ごとのピークの context 量
  - (c) 「continuation / 再開」の概念
  - (d) Task 単位の集計（run 数、壁時計、総トークン）。GUI の `RunSummary` はあるが、集計の API は無い。

## 12. support-task の状態管理

- **support 用の kind は無い。** `task_core::report::support_kind(&Task)`（`crates/task-core/src/report.rs:47-67`）が role・kind・対話の有無から、次のいずれかを導出するラベルにすぎない。
  - milestone_review / conversation / plan / compaction（`report-compressor`）/ knowledge / doc_gardener / approval / review
- 作るのは `create_support_task`（`add.rs:420`）。受け入れ条件ゼロ可で、`Ready` で始まる。呼ぶ側は次のとおり。
  - `celeris/src/reports.rs:238`
  - `knowledge_maint.rs:105/156`
  - `knowledge_gc.rs:377`
  - `doc_gardener.rs:193`
  - `task-ops/src/project_plan.rs:152`
- 状態は**通常のタスクの状態機械そのもの**。独立した Task として、ボードや events に出る。
- `support_kind` は、人の仕事の木や報告からこれらを除くのに使う（`view.rs:450`、`comment.rs:310`、`delivery.rs:26`、`milestone_review.rs:69`）。

## 13. failure の伝播

- `Event::Transitioned{from, to, reason}` の `reason` は `Trigger::name()`（`transition.rs:69-96`）で、機械可読の名前。
- 詳しい理由は `WorkerFinished.outcome` の文字列（§5 の接頭辞）と `ReviewVerdict.reason` に入る。
- Phase 116（ADR-0070 D1）: `classify_task_failure`（`derive.rs:204`）が `FailureClass{Infra, Work}`（`:159`）を返す。判定は次の順。
  - 最後の遷移が `review_fail` なら Work（不合格の verdict の理由を使う）。
  - そうでなければ最後の worker `WorkerFinished` を見る。`infra failure ×` / `requeue limit` の印があれば Infra、無ければ Work。
  - ディスク不足は Infra に倒す。

  使われる場所は `inbox.rs:460`、`view.rs:856`（`TaskDetail.failure`）、`celeris/src/notify.rs:630`（`task_failed` の通知）。
- 委譲した子の失敗は、親をやり直すか人に聞く（ADR-0021、`OnChildFailure::RetryThenAsk`、`escalate_failed_children` D:3803）。依存タスクの失敗は `DependencyFailed` で連鎖的に `cancelled` にする。

## 14. データの現状（events / `Event` / `Task` / schema_migrations）

- **テーブル**: `tasks(id, status, kind, parent_id, priority, created_at, lease_worker_run_id, lease_expires_at, json)`（`0001_init.sql`）。
  - `events` は 0002 で `events(id INTEGER PK, task_id, seq, ts, json, UNIQUE(task_id, seq))` になった。追記専用で、`Event` を JSON で持つ。
  - **events が正本、`tasks` は派生のスナップショット**（`0001_init.sql` の冒頭、DESIGN 原則 6）。
- **`SCHEMA_VERSION = 25`**（`store.rs:76`）。
  - migration は `include_str!` 定数（`store.rs:35-72`）と `migration_sql`（`:1299`）で定義し、`migrate` は `:1259`。
  - 直近の追加: 0021 knowledge_run_retry / 0022 llm_proxy_requests / 0023 node_sessions / 0024 mcp / 0025 cluster_settings。
  - ADR-0069 は migration 無し（`Task.routing` は `json` 列の中）。
- **`Status`**（`model.rs:56-64`）: Draft / Ready / Running / Blocked / Reviewing / Done / Failed / Cancelled（ADR-0002 D1）。`NeedsInput` は無く、`Blocked` がその役を担う。
- **`TaskKind`**（`:46`）: Plan / Execute / Review / Approval。
- **`Trigger`**（`transition.rs:17-64`）の 23 個: Accept, Dispatch, WorkerDone, WorkerQuestion, WorkerError{retryable}, LeaseExpired, ReviewPass, ReviewFail, Answer, Approve, Reject, Cancel, Requeue, DependencyFailed, Aggregate, ChildFailed, Interrupt, Reopen, Rereview, ProjectCancelled, MilestoneCancelled, Unroutable, InfraRequeue。
- **`Event`**（`model.rs:853-1022`）: Created, Transitioned, WorkerStarted, WorkerProgress, ArtifactProduced, WorkerFinished, ReviewVerdict, ApprovalRequested, ApprovalDecided, Answered, QuestionRaised, Delegated, ClusterUnavailable, ClusterMasterExited, ProviderThrottled, Retried, Edited, Assigned, WorkspaceModeDowngraded, WorkspacePruned, RoutingDecided。追加のフィールドはすべて `serde(default)` で、後方互換を保つ慣習がある。
- **`Task`**（`model.rs:449-527`）の欄:
  - 識別と内容: id, parent_id, kind, title, objective, acceptance, inputs, depends_on
  - 状態と実行: status, priority, worker_hint{tier, adapter}, workspace, repos, budget, attempts, lease, created_at, updated_at
  - 割り当てと分類: role, genre, aggregate, project_id, milestone_id, assignee, labels, category, skills, mode, conversation, routing

  **セッションや実行計画を表す欄は無い。**

## 15. 既存の分解の仕組み（ExecutionPlan と混同しないため）

| 仕組み | 出力 | 作るもの | 上限 | 失敗の扱い |
|---|---|---|---|---|
| `Plan` kind（ADR-0007、DESIGN §5.6） | `artifacts/plan.json` の `PlanOutput{tasks}`（`plan.rs:109`、`deny_unknown_fields`、schema は `docs/protocol/plan-output.schema.json`） | **ユーザーに見える子 Task**（Draft、auto_accept なら Ready） | 1..=20 件（`PlanLimits`、`plan.rs:115`）、深さ 3（`MAX_PLAN_DEPTH`、`:20`） | 不正なら 1 回だけ再試行、それでも不正なら failed |
| 実行中の委譲（ADR-0016） | `artifacts/delegate.json` | **ユーザーに見える子 Task**（親は Reviewing のまま `awaiting_children`） | 1 run あたり 8 / 深さ 5 / 木全体で 100 run（`DelegationLimits`、`delegate.rs:57-86`） | 親をやり直すか人に聞く（ADR-0021） |
| 案件の計画（`project_plan.rs`） | 案件の途中目標とタスク | Task | — | — |

どれも**Task を増やす**方向の分解で、子はそれぞれ独立した worktree、レビュー、配送、ボード上の行を持つ。
「1 つの goal のまま、その中の実行を複数の session に分ける」層は無い。

「Selective Lead Activation」はコードにも ADR にも実装が無い（ADR-0069 §5 に Phase 2 の予約として書かれているだけ）。
現在の「lead」は、ADR-0054 の `SessionKind::Lead`（部署ごとの reviewer セッション）を指す。部署に属するタスクのレビューのたびに毎回使われ、選んで起こすものではない。

## 16. GUI の task detail

- `gui/app/routes/tasks.$id.tsx`（2,638 行）。
  - 上部: 状態・種類・役割・分野などのバッジ、`TaskRoutingPanel`（`:572`）、`FailureBanner`（`:855`）。
  - タブ（`labels.ts:357`）: overview / timeline / changes / files / artifacts。
  - overview の節: 人のレビュー、情報、編集、子 / 依存、タイマー、受け入れ条件、**runs**（`:1122`。`RunSummary` の表）、委譲、前回のレビュー、回答、操作、worker run のヒント。
- run の詳細は別のルート（`tasks.$id.runs.$runId.tsx`）。
- 「Execution」節（WorkUnit と continuation）に当たるものは無い。

## 17. 設計への含意（ADR-0072 への入力）

1. 1:1 の結合を**そのまま生かし**、「Task の lease を持つ run が 1 つ」の規則を守る。そのうえで、次の run が**何をするか**（どの WorkUnit を、どの checkpoint から始めるか）を決定的に選ぶ層を足すのが最小の変更になる。1 Task の中で並列に run を走らせるには、`running` の key、lease、worktree の共有をすべて作り直す必要がある。
2. budget 切れを `WorkerError` と区別するには、アダプタが「予算切れ」を構造化して返し（今は文字列の `Error`）、usage も運ぶ必要がある。
3. 仕事の run はもともとステートレス（セッション無し）なので、「checkpoint から新しい context で再開する」は**既存の性質の延長**になる。resume は CoS / lead の仕組みであり、仕事の run に広げる必然性は無い。
4. reviewer の不合格は verdict の理由に構造（cmd / exit / tail）を持つので、fmt / lint / 小さな test 失敗を**決定的に分類**できる。
5. events が正本という原則（DESIGN 原則 6）を守るなら、新しい層の状態も events に積み、テーブルは派生の索引にする。
