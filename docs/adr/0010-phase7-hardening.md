# ADR-0010: Phase 7 — 仕上げ（人間とのやりとり、取り消しと伝播、承認ゲートの穴、供給側失敗、運用上の負債）

- 日付: 2026-09-14
- 状態: Accepted（Phase 7）
- 関連: [ADR-0009](0009-proposal-adoption-and-phase-closure.md) D2/D3 / [ADR-0002](0002-state-machine.md) /
  [ADR-0004](0004-taskctl-cli.md) / [ADR-0005](0005-phase3-dispatch-and-worker.md) /
  [ADR-0007](0007-phase5-planner-and-reviewer.md) / [ADR-0008](0008-phase6-approval-gate-and-codex-adapter.md)

## 文脈

ADR-0009 で採用した提案を、既存の設計原則（協調判断に LLM を使わない、状態は SQLite、全遷移を `events` に追記、
状態変更と関連イベントは同一トランザクション）を崩さずに実装する方法を決める。

## 決定

### D1. 状態機械の変更（`task-core::transition`）

| 変更 | 内容 |
|---|---|
| P-4 | `Cancel` は非終端（`draft/ready/running/blocked/reviewing`）からのみ有効。終端からは `InvalidTransition` |
| P-9 | 新トリガ `DependencyFailed`: 非終端 → `cancelled`、attempts 据え置き、reason `"dependency_failed"` |
| P-21 | 新トリガ `Requeue`: `running → ready`、attempts 据え置き、reason `"requeue"` |

`replay` の attempts 復元規則（`worker_error | lease_expired | review_fail` で +1）は変えない（新しい 2 つは増やさない）。
遷移表テストは 14 列（`WorkerError` の retryable 2 通りを含む）× 8 行 × 4 kind を網羅する。

### D2. ストア（`task-core::store`）

- **`create_task(task, extra_events)`**（D3 の負債）: `insert` + `Event::Created` + `extra_events` を 1 トランザクション。
  `taskctl add/plan`、Human check の `Approval` 子（`ApprovalRequested` を extra に）、seed 例で使う。`insert` は
  テスト用に残す。
- **終端化に伴うカスケード**（`apply_transition_tx` 内、同一トランザクション、再帰）:
  1. `kind = Approval` のタスクが `failed` または `cancelled` になったら、終端でない**直接の子**を `Cancel`
     （ADR-0008 D1 の Reject 限定を、承認されなかった全ケースに広げる。承認待ちの子が孤児化しないため）。
  2. `kind ≠ Approval` のタスクが終端になったら、終端でない**直接の子のうち `kind = Approval`** を `Cancel`（P-37）。
  3. タスクが `failed` または `cancelled` になったら、`depends_on` にそのタスクを含む終端でないタスクへ
     `DependencyFailed`（P-9）。後続も `cancelled` になるので推移的に伝播する。
  再帰は「終端でないものだけを対象にし、対象は必ず終端になる」ため停止する。
- **`ready_tasks`**（P-36）: SQL で `kind != 'approval'` を追加。`acquire_lease` の kind ガードは防御として残す。
- **`renew_lease(task_id, run_id, ttl) -> bool`**（P-7）: `status = running` かつリースの run_id が一致する場合だけ
  `expires_at = now + ttl` に更新する。状態遷移ではないのでイベントは追記しない（lease は `replay` の比較対象外）。

### D3. `Event::Answered{question, answer}` と `context.answers`（P-10）

- `taskctl answer <id> <text>` は `Trigger::Answer` と同一トランザクションで `Event::Answered` を追記する。
  `question` は最後の `WorkerFinished.outcome` のうち `"question: "` で始まるものから取る（無ければ空文字列）。
- `RunContext.answers: Vec<Answer{question, answer}>`（`#[serde(default, skip_serializing_if = "Vec::is_empty")]`。
  前方互換）。ディスパッチャは、そのタスクの全 `Answered` を時系列で渡す。
- `claude-code` / `codex` のプロンプト（共通の `build_prompt`）は、回答があれば「以前の質問への人間の回答」節を載せる。

### D4. `taskctl`（P-17, P-18, P-19, 出力パイプ）

- `add`: `--check-cmd <cmd>`（`Command{cmd, expect_exit: 0}`、text は `` `<cmd>` exits 0 ``）、
  `--check-artifact <name>`（`ArtifactExists`、text は `artifact <name> exists`）、`--check-reviewer <text>`（`Reviewer`）。
  `--accept`（`Human`）を含めいずれか 1 つ以上が必須。`acceptance` の並びは accept → cmd → artifact → reviewer の
  固定順（clap で出現順を保つ仕組みは入れない）。`--depends-on` の先が存在しない、または `failed`/`cancelled` なら
  エラーにする（挿入した瞬間に後続が永久に進まない状態を作らない）。
- `cancel <id>`: `Trigger::Cancel` を `apply_transition` で適用する。終端なら exit 1。カスケードはストアが行う。
- `add` / `plan` の `--workspace` 省略時は `WorkspaceSpec::Local{ path: "<task_id>" }`（相対。ディスパッチャが
  `workspace_root` 基準で解決する。ADR-0005 D3）。Plan の子は従来どおり親のワークスペースを継承する。
- 標準出力への書き込みはヘルパ経由にし、`BrokenPipe` なら exit 0 で静かに終わる（`println!` の panic を避ける）。

### D5. 供給側失敗と `Requeue`（P-21, P-29）

- **検出はアダプタ、判断はディスパッチャ。** アダプタは供給側失敗を既存の
  `AdapterError::{Throttled{retry_after}, AuthFailed, Exhausted}` として返す（`Terminal` は変えない）。
  - プロトコル（`fake` 等）: `error` メッセージに任意フィールド
    `provider_failure: {"kind":"throttled","retry_after_secs":N} | {"kind":"auth_failed"} | {"kind":"exhausted"}` を追加。
    付いていれば実行器が上記 `AdapterError` に写す。
  - `claude-code` / `codex`: エラー文面（`result` の `result` テキスト、`turn.failed` / `error` のメッセージ）を
    決定的な文字列規則で分類する（`429`・`rate limit`・`overloaded` → Throttled（retry_after 60 秒）、
    `usage limit`・`quota`・`credit balance` → Exhausted、`401`・`invalid api key`・`authentication`・`not logged in`・
    `/login` → AuthFailed）。どれにも当たらなければ従来どおり `Terminal::Error`。
- ディスパッチャ: ワーカー run の結果が `Throttled | AuthFailed | Exhausted | Spawn`（起動失敗）なら `Trigger::Requeue`
  （`WorkerFinished.outcome = "requeue: <error>"`）とし、`ProviderPolicy::report` でプロバイダを cooldown にする
  （`Spawn` は `Exhausted` として報告）。cooldown 中は `pick` が候補を返さないので、ホットループにならない。
- Reviewer run（P-29）: `review_task` の結果に `provider_failure` を持たせ、ディスパッチャはそのとき遷移を適用せず、
  `WorkerProgress{"reviewer run <id>: requeued: ..."}` を記録してプロバイダを cooldown にし、`reviewing` のまま次 tick に
  回す（`attempts` を消費しない）。`review.json` の不正・タイムアウトは従来どおり fail（モデル側の失敗）。

### D6. リトライのバックオフ（P-3）

設定 `retry_backoff_base_secs`（既定 10）と `retry_backoff_max_secs`（既定 300）。ディスパッチャは
`attempts > 0` の `ready` タスクを `updated_at + min(base·2^(attempts-1), max)` まで dispatch しない。
`updated_at` は `ready` に入った遷移で更新される DB 上の値なので、再起動しても同じ判断になる（状態はエージェントの外）。
`base = 0` で無効（テスト用）。`blocked → ready`（answer）後も attempts > 0 なら待つ（単純さを優先。既知の挙動として記録）。

### D7. リースの延長（P-7）

`EventSink` に `heartbeat()`（既定は何もしない）を追加し、アダプタは stdout の 1 行を読むたびに呼ぶ。
ディスパッチャの `StoreSink::heartbeat` は、前回の延長から `lease_grace / 2` 以上経っていれば
`renew_lease(ttl = idle_timeout + lease_grace)` を呼ぶ。取得時の ttl は従来どおり `max_wall_secs + lease_grace`。

安全性: アダプタは最後の出力から `idle_timeout` で強制終了する。延長後の期限は
「最後の出力 − grace/2 + idle_timeout + grace」以降なので、生きているワーカーのリースが先に切れることはない。
一方、デーモンが落ちた場合は最後の出力から `idle_timeout + lease_grace` 程度で回収できる（従来は `max_wall_secs + grace`）。

### D8. 承認ゲート（P-35）と `--until-idle`

- Human check の `Approval` 子の title を `Approval needed: <title> — criterion <idx> (attempt <n>)`（`n = attempts + 1`）
  にし、完全一致で照合する。再レビュー（`review_fail` で attempts が増えた後）では新しい子が作られる。
  reject の note は `ReviewVerdict.reason` → 次 run の `prior_review` でワーカーに届く。
- `is_idle` は「DB 上の `reviewing` が全て人間の承認待ちで延期中」の場合も idle とみなす（人間が操作しない限り
  進まない状態で `--until-idle` が終われるようにする）。

### D9. Reviewer の adapter / tier 指定（P-30）

`[reviewer] adapter = "<adapter id>"`（省略可）, `tier = "standard"`（既定）。`DispatchConfig.reviewer_hint` に写し、
Reviewer run の `pick` と合成 `Review` タスクの `worker_hint` に使う。`validate()` は、`Check::Reviewer` を満たせる
プロバイダ（adapter と tier が一致）が 1 つも無い設定をエラーにする（P-33 の「無音で待ち続ける」を設定時に防ぐ）。

### D10. 運用上の負債

- **P-26**: `claude-code` / `codex` も終端を `WorkerMessage` に正規化して `runs/<run_id>/result.json` に書く。
  `question` / `error` の場合も書く（`fake` と同じ）。
- **ETXTBSY**: `spawn_retrying` を削除する。原因は「テストが書き込み直後のスクリプトを exec する間に、別スレッドの
  fork がその書き込み fd を継承している」ことなので、テストのスタブ作成を**別プロセス（`sh -c 'cat > f; chmod'`）で
  書き込む**ヘルパに置き換える（テストプロセス自身は書き込み fd を持たない）。

## 結果

- `task-core`: `transition.rs`（D1）、`model.rs`（`Event::Answered`）、`store.rs`（D2）。
- `task-worker`: `protocol.rs`（`Answer`, `RunContext.answers`, `ProviderFailure`）、`adapter.rs`（`heartbeat`）、
  `subprocess.rs` / `claude_code.rs` / `codex.rs`（D3 プロンプト, D5 分類, D7 heartbeat, D10）、`docs/protocol/*`。
- `task-dispatch`: `dispatcher.rs` / `review.rs`（D3, D5〜D9）。`taskd`: 設定（D6, D9）。`taskctl`: D3/D4。
- `tests/e2e`: Phase 7 シナリオ。
