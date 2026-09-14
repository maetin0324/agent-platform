# PROGRESS — taskd

現在地: **Phase 5 完了（2026-09-14）**。次は Phase 6（承認ゲートと codex アダプタ）。

| Phase | 内容 | 状態 | 完了日 |
|---|---|---|---|
| 0 | 調査と ADR（実装なし） | 完了 | 2026-09-13 |
| 1 | task-core（状態機械・SqliteStore） | 完了 | 2026-09-13 |
| 2 | taskctl と replay | 完了 | 2026-09-13 |
| 3 | fake ワーカーとディスパッチャ | 完了 | 2026-09-13 |
| 4 | claude-code アダプタとドッグフーディング | 完了（実機ドッグフードは人間確認待ち） | 2026-09-13 |
| 5 | Planner / Reviewer（LLM） | 完了 | 2026-09-14 |
| 6 | 承認ゲートと codex アダプタ | 未着手 | |

---

## Phase 0 — 調査と ADR

### 成果物

- `docs/adr/0001-scope-and-principles.md` — スコープ、原則のクレート依存による強制、クレート選定、参考設計から借りる点
- `docs/adr/0002-state-machine.md` — `Trigger` / `StateView` / 遷移表（Phase 1 のテスト仕様）、`attempts` 規則、kind ごとの初期状態
- `docs/adr/0003-worker-protocol.md` — トランスポート、終端規則、タイムアウト、パス検査、スキーマ管理、アダプタ写像
- `docs/protocol/worker-protocol.md` — プロトコル v1 初版（暫定 JSON Schema 付き）

### 調査した一次情報

| 対象 | 出所 | 要点 |
|---|---|---|
| Symphony | `raw.githubusercontent.com/openai/symphony/main/SPEC.md` | 単一権限オーケストレータ、tick 手順、workspace-per-issue、`stall_timeout_ms`/`turn_timeout_ms`、指数バックオフ、`attempt` 変数。耐久 DB 無し（taskd と異なる） |
| Bernstein | `bernstein.run`、GitHub README | 協調ループに LLM 無し、`.sdd/lineage/<run_id>/spine.jsonl` の追記ジャーナル、byte-identical replay |
| dsh | `deepseek-harness.github.io/.../reference/subsystems/{session,persistence,subagent}`、`apps/cli/README.md` | Session = 追記専用 `SessionEvent` ログ（真実）、履歴は派生。`--profile headless` は最終回答テキストのみ。サブエージェントは in-process、`SubagentResult.stopReason ∈ {completed, aborted, error, max-tokens, refusal}` |
| Claude Code | `code.claude.com/docs/en/{headless,cli-reference,env-vars,authentication}.md`、ローカル `claude --help`（v2.1.270） | stream-json の `system/init` → `assistant`/`user` → `result{subtype,usage,…}`。`--json-schema` は `--output-format json` 専用。`CLAUDE_CONFIG_DIR` でアカウント分離。exit 0/1/2/130/143 |
| Codex | `developers.openai.com/codex/noninteractive` | `codex exec --json` で JSONL（`thread.started`, `item.*`, `turn.completed/failed`）、`-o` で最終メッセージをファイルへ |

### 受け入れ条件と証拠

- 条件: 3 つの ADR が存在する
  - コマンド: `ls docs/adr/`
  - 結果: `0001-scope-and-principles.md`, `0002-state-machine.md`, `0003-worker-protocol.md` の 3 ファイル
- 条件: `docs/protocol/worker-protocol.md` の初版がある
  - コマンド: `ls docs/protocol/`
  - 結果: `worker-protocol.md`（§7 の暫定 JSON Schema は `python3 -m json.tool` で構文検証済み）
- CLAUDE.md の共通条件 `cargo test --workspace` / `cargo clippy --workspace -- -D warnings`
  - 結果: Phase 0 は実装なしで `Cargo.toml` が存在せず、両コマンドとも `error: could not find Cargo.toml` で exit 101。Phase 1 開始時にワークスペースを作る（提案 P-1）

### 未解決事項（人間の判断待ち）

Phase 1 に影響するのは P-4 と P-6。採否が出るまでは DESIGN.md の文言どおり実装する。

1. **P-4** `cancel` を非終端状態に限定するか（現状は「any」）
2. **P-6** 親 `Approval` 待ちの子を「`ready` だが dispatch されない」で統一するか（§4.2 と §5.1 の不整合）
3. **P-10** `context.answers` を追加するか。追加しないと `taskctl answer` の回答がワーカーに届かない（Phase 3 までに要決定）

### 提案（DESIGN.md への修正提案。詳細は各 ADR 末尾）

| # | 節 | 提案 | 採用まで実装で使う既定 |
|---|---|---|---|
| P-1 | §6 Phase 0 | Phase 0 の完了条件から cargo コマンドを除く | — |
| P-2 | §2 | クレート一覧（`ulid`, `time`, `sha2`, `thiserror`, `toml`, `schemars`…）を追記 | ADR-0001 D3 |
| P-3 | §5.2 | リトライに指数バックオフ設定を追加 | 次 tick で即再投入 |
| P-4 | §4.2 | `cancel` を非終端状態のみに | 文言どおり any |
| P-5 | §5.9 | `reject` の `draft` タスクへの動作 = `cancelled` | 同左を実装（未規定のため） |
| P-6 | §4.2/§5.1/§6 Phase 6 | 承認待ちは「`ready` だが dispatch 不可」に統一、Phase 6 受け入れ文言も合わせる | §5.1 の定義 |
| P-7 | §5.1 | `renew_lease` の追加 | ttl = `max_wall_secs` + 猶予 60 s |
| P-8 | §4.2 | 「running ──(worker error)──▶ ready \| failed」を図に追記 | ADR-0002 D2/D3 |
| P-9 | §5.7 | 先行タスク失敗時に後続を `cancelled` | 何もしない |
| P-10 | §5.3 | `context.answers` を追加 | 未実装（Phase 3 までに要決定） |
| P-11 | §5.3 | `run` に `run_id`, `attempt` を追加 | 未実装 |
| P-12 | §5.3 | `evidence[].command/exit/stdout_tail` を任意に | 必須のまま |
| P-13 | §5.4 | CLI 系アダプタの結果ファイル規約（`artifacts/result.json`） | Phase 4 の ADR-0004 で確定 |
| P-14 | §5.4 | dsh 行の記述を実態（headless はテキストのみ）に合わせる | — |

### 次の Phase（Phase 1）に持ち越すこと

- workspace `Cargo.toml` と `crates/task-core` の作成
- ADR-0002 D8 の遷移表を全セル列挙するテスト
- `acquire_lease` の 2 スレッド同時取得テスト

---

## Phase 1 — DONE（2026-09-13）

### 成果物

- `Cargo.toml`（workspace root, `members = ["crates/task-core"]`）
- `crates/task-core/Cargo.toml`（依存: `ulid`, `time`, `rusqlite(bundled)`, `sha2`, `thiserror`, `serde`, `serde_json`。dev: `tempfile`。ADR-0001 D3 準拠、`std::process`/`tokio`/HTTPクライアント/LLM SDK 無し）
- `crates/task-core/src/model.rs` — `TaskId`, `TaskKind`, `Status`, `Tier`, `WorkerHint`, `WorkspaceSpec`, `Budget`, `Lease`, `Check`, `Criterion`, `ArtifactRef`, `Task`, `Usage`, `Event`（DESIGN §4.1/§4.3 準拠）
- `crates/task-core/src/transition.rs` — `StateView`, `Trigger`, `Outcome`, `InvalidTransition`, `transition()`（ADR-0002 D2/D3/D8 準拠の純粋関数）とテーブル駆動テスト
- `crates/task-core/src/store.rs` — `TaskStore` trait, `SqliteStore` 実装（DESIGN §5.1: `insert`/`get`/`list`/`append_event`/`events_for`/`acquire_lease`/`release_lease`/`ready_tasks`）とテスト
- `crates/task-core/migrations/0001_init.sql` — `tasks`（派生ビュー、JSON列＋検索用非正規化列）と `events`（追記専用、`PRIMARY KEY(task_id, seq)`）
- `crates/task-core/src/lib.rs` — 上記の再エクスポート

作業分担: `model.rs`・ワークスペース構成・`transition.rs`/`store.rs` の設計判断（疎結合方針、DBスキーマ、遷移規則の書き下し）は自分で行った。`transition.rs` 本体とそのテーブル駆動テスト、`store.rs`+マイグレーション本体とそのテストは、互いのファイルに触れない独立ユニットとして implementer サブエージェント2体に並列実装させた（担当: transition.rs 単体／store.rs+migrations 単体）。

### 受け入れ条件と証拠

- 条件: 状態遷移表（ADR-0002 D8）の全セル（有効・無効）を網羅するテストが通る
  - コマンド: `cargo test --workspace`
  - 結果: exit 0。`test result: ok. 14 passed; 0 failed`。うち `transition::tests::table_simple_triggers_full_cross_product`（kind4×status8×単純trigger9=288ケース）と `table_retry_triggers_full_cross_product`（kind4×status8×リトライ系16ケース=512ケース、`(attempts,max_retries)` 境界4通り含む）の2テストで D8 の全12トリガー（`WorkerError`のretryable2通り込みで13列）×8行×4kindを網羅。手動で D8 の表と実装コードを突き合わせ（監査で実施済み）、差分ゼロを確認。
- 条件: `acquire_lease` の並行テスト（2スレッドで同時取得し1つだけ成功）が通る
  - コマンド: `cargo test --workspace -- store::tests::acquire_lease_is_exclusive_under_concurrency`
  - 結果: `test store::tests::acquire_lease_is_exclusive_under_concurrency ... ok`。`std::thread::spawn` 2本 + `Barrier` で同一 task_id に対し異なる `worker_run_id` で同時に `acquire_lease` を呼び、`assert_ne!(result_a, result_b)` で排他性を検証。排他はSQLの条件付き `UPDATE ... WHERE id=? AND status='ready'`（トランザクション内）で担保。
- CLAUDE.md の共通条件
  - コマンド: `cargo test --workspace`
    結果: exit 0、**14 tests passed**（transition 6件 + store 8件）、doc-tests 0件、失敗 0
  - コマンド: `cargo clippy --workspace --all-targets -- -D warnings`
    結果: exit 0、警告 0件

### 監査結果

auditor サブエージェントを1回起動（読み取り専用）。初回判定は **条件付き可**。「不可」3件のうち2件を修正済み、1件（手続き）は本節の更新とコミットで解消:

- **(A)【不可→修正済み】** `acquire_lease` が `kind` を見ずに `Approval` タスクを `running` にできた（ADR-0002 D8「Approvalの`Dispatch`は無効」に違反）。`store.rs::acquire_lease` に `kind_col == kind_str(TaskKind::Approval)` ガードを追加し拒否するよう修正。回帰テスト `acquire_lease_rejects_approval_kind_even_when_ready` を追加。
- **(B)【不可→修正済み】** 状態変更（ready→running）が `Event::Transitioned` と同一トランザクションで記録されていなかった（ADR-0002 D2 違反、Phase 2 replay の差分ゼロ条件に抵触しうる）。`acquire_lease` を `rusqlite::Transaction` で包み、UPDATE成功時に同一トランザクション内で `Event::Transitioned{from:Ready,to:Running,reason:"dispatch"}` を追記するよう修正。回帰テスト `acquire_lease_appends_transitioned_event_atomically`（成功時に1件追記・失敗時は追記しないことを確認）を追加。
- **(C)【手続き→解消】** Phase 1 の成果物が未コミットだった。本節の追記後に `git add -A && git commit -m "phase 1: ..."` を実行する（このコミットで解消）。

再監査は auditor サブエージェントを再起動せず自分で実施: 上記2件の修正後に `cargo test --workspace`（14 passed, exit 0）と `cargo clippy --workspace --all-targets -- -D warnings`（exit 0, warnings 0）を再実行して確認済み。監査で「条件付き可」の範囲に留めてよいとされた軽微な指摘（後述の未解決事項）は Phase 1 の完了条件には含めない。

### 未解決事項

Phase 0 から持ち越し（未着手、人間の判断待ち）:
1. P-4 `cancel` を非終端状態に限定するか（現状は「any」のまま実装、ADR-0002 D8 の ※P-4 セルどおり）
2. P-6 親 `Approval` 待ちの子の扱い統一（現状は「ready だが `ready_tasks()` に現れない」で実装、ADR-0002 D5 どおり）
3. P-10 `context.answers` の追加要否（Phase 3 までに要決定）

Phase 1 の監査で新たに判明し、次 Phase 以降に持ち越す軽微な指摘（「不可」ではなく「条件付き可」の範囲）:
4. `table_simple_triggers_full_cross_product` の期待値関数 `expected_simple` が D8 の表をデータとして持たず、`transition()` と同じ条件式を書き写す形になっている。実装と期待値を同時に間違えると検出できない構造的弱点。今回は監査で手動突き合わせ済みだが、Phase 2 以降でテーブルをデータ（配列やCSV等）として外出しする改善の余地がある。
5. `append_event` の `SELECT MAX(seq)+1` → `INSERT` が別文（`acquire_lease` 内の新規追記は同一トランザクション化済みだが、`TaskStore::append_event` 単体はそのまま）。現状は `SqliteStore` が `Mutex<Connection>` で直列化されるため実害はないが、将来複数コネクション運用に変えた場合はレースになりうる。Phase 3 でディスパッチャがイベント追記を多用する前に、`append_event` 自体もトランザクション化するか検討。
6. `sha2` 依存は宣言のみで未使用（`ArtifactRef.sha256` は単なる `String` で計算コードが無い）。ADR-0001 D3 で先行合意済みの依存であり、成果物のハッシュ計算を実装する Phase（Workspace/collect実装時）まで未使用のままで問題ない。

### 提案（DESIGN.md への修正提案。DESIGN.md 本体は編集していない）

| # | 節 | 提案 | 採用まで実装で使う既定 |
|---|---|---|---|
| P-15 | §5.1 | `TaskStore` の操作一覧に `events_for(task_id) -> Vec<(seq, Event)>` を追記する（replayや`taskctl log`に必須のため、Phase 1で実装・使用済み） | 実装済み（`store.rs`） |
| P-16 | §5.1 | 「SQLトランザクションで排他」の対象を「状態を変更する操作は変更後の `Event::Transitioned` 追記まで含めて同一トランザクションにする」と明文化する（ADR-0002 D2 の「同一トランザクションで追記」とDESIGN文言の整合を取るため） | ADR-0002 D2 どおり実装（`acquire_lease`のみ該当。Phase 1時点で他に遷移を伴う操作はない） |

Phase 0 からの既存提案（P-1〜P-14）は状況変化なし。

---

## Phase 2 — DONE（2026-09-13）

### 成果物

- `docs/adr/0004-taskctl-cli.md` — `taskctl` の CLI→トリガー写像、`TaskStore::apply_transition` の追加、
  DB パス規約、`taskctl add` の CLI→`Task` 写像、`replay` の再構築アルゴリズムを定める ADR
- `crates/task-core/src/store.rs` — `TaskStore::apply_transition(task_id, trigger, extra_event)` を追加
  （`acquire_lease` と同じ単一トランザクション構造。ADR-0004 D1）。`StoreError::InvalidTransition` を追加
- `crates/taskctl/` — 新規クレート（workspace member に追加）
  - `src/main.rs` — clap CLI（`--db` グローバルオプション、8 サブコマンドの dispatch）
  - `src/error.rs` — `CliError`、`parse_task_id`
  - `src/commands/add.rs` — `taskctl add`
  - `src/commands/query.rs` — `taskctl ls` / `taskctl show` / `taskctl log`
  - `src/commands/gate.rs` — `taskctl approve` / `taskctl reject` / `taskctl answer`
  - `src/commands/replay.rs` — `taskctl replay`

作業分担: ADR-0004 の設計判断、`task-core::apply_transition` の実装（原子性が Phase 1 監査で重視された不変条件のため）、
taskctl のクレート雛形（`Cargo.toml`/`main.rs`/`commands/mod.rs`/各コマンドの `Args` 構造体と関数シグネチャ）、
`replay.rs`（受け入れ条件に直結する再構築ロジック）は自分で行った。
`add.rs`・`query.rs`（ls/show/log）・`gate.rs`（approve/reject/answer）の3ファイルは、互いに触れない独立ユニットとして
implementer サブエージェント3体に並列実装させた（担当: それぞれ1ファイルのみ、シグネチャと ADR-0004 の指示を渡した）。

### 受け入れ条件と証拠

- 条件: `taskctl add` → `approve` → `show` が期待どおり
  - コマンド（実バイナリ、`/tmp` の一時 DB で実行）:
    ```
    TASKD_DB=/tmp/phase2-demo.sqlite3 taskctl add --title hello --objective "READMEに使用例を追記" \
      --accept "cargo test が exit 0"
    # => 01M2E5ZZVXDY4J448VN16HF7VN
    taskctl show 01M2E5ZZVXDY4J448VN16HF7VN   # status: Draft, events: [0] Created
    taskctl approve 01M2E5ZZVXDY4J448VN16HF7VN # => Ready
    taskctl show 01M2E5ZZVXDY4J448VN16HF7VN   # status: Ready, events: [0] Created, [1] Transitioned{from:Draft,to:Ready,reason:"accept"}
    ```
  - 追加で確認した経路: `kind=approval` タスクの `approve`（`Ready→Done` + `ApprovalDecided{approved:true}` 追記）、
    `draft` タスクへの `reject` がエラー（exit 1、状態不変。P-5 不採用の確認）、`ls --tree` が親子をインデント表示。
  - 単体テストでも同じ写像を検証済み（`commands::add::tests::*` 5件、`commands::gate::tests::*` 5件、
    `commands::query::tests::*` 6件）。
- 条件: `taskctl replay` がイベントから再構築した状態と `tasks` テーブルが一致（差分ゼロ）
  - コマンド: 上記デモ DB に対する `taskctl replay`
  - 結果: `replay: 0 mismatches across 5 tasks`、exit 0
  - 差分検出そのものの検証: `commands::replay::tests::replay_detects_status_drift_from_events`
    （`tasks` 行を更新せず `events` だけに `Transitioned` を追記すると `status`/`attempts` の両方で
    mismatch を検出することを確認）
- CLAUDE.md の共通条件
  - コマンド: `cargo test --workspace`
    結果: exit 0、**38 tests passed**（task-core 19 件 + taskctl 19 件、doc-tests 0 件、失敗 0）
  - コマンド: `cargo clippy --workspace --all-targets -- -D warnings`
    結果: exit 0、警告 0 件

### 監査結果

auditor サブエージェントを1回起動（読み取り専用）。判定は **条件付き可**（個別項目はすべて「可」または
「条件付き可」で、「不可」は手続き1件のみ）:

- **(A)【条件付き可→修正済み】** `apply_transition`（`store.rs`）が ADR-0002 D1「`running` から出る全遷移で
  リースを解放する」を満たしていなかった（`running` を出る遷移でも `lease`/`lease_worker_run_id`/
  `lease_expires_at` が更新されず残る）。Phase 2 の CLI 経路（Accept/Approve/Reject/Answer）はどれも
  `running` から出ないため実害は未発生だったが、Phase 3 のディスパッチャが `apply_transition` を
  `WorkerDone`/`WorkerError`/`LeaseExpired` に再利用する前提で放置すべきでないと判断し、その場で修正した。
  `apply_transition` に `running → 非running` 判定を追加し、該当時は `lease` を `None` にして
  `lease_worker_run_id`/`lease_expires_at` 列も `NULL` にする。回帰テスト
  `store::tests::apply_transition_releases_lease_when_leaving_running` を追加（`acquire_lease` でリースを
  取ってから `WorkerDone` を適用し、`json` 経由の `lease` と生の SQL 列の両方が `None`/`NULL` になることを確認）。
- **(B)【条件付き可→修正済み】** `add.rs`/`gate.rs`/`query.rs` の冒頭 doc comment が、実装を委譲した
  implementer サブエージェントへの作業指示文のまま残っていた（「実装者への指示」「このファイルだけを
  編集し…」等）。実装済みの今となっては読者を誤解させるため、通常の設計コメント（ADR 参照＋要約）に
  書き直した。
- **(C)【手続き→解消】** 本節の追記と `git commit` の実行で解消する（後述）。
- 監査で指摘された軽微な点（対応しない）: `taskctl show`/`ls` の出力を `| head` 等で途中で打ち切ると
  Rust の `println!` が SIGPIPE で panic する（`Broken pipe (os error 32)`）。通常のパイプ利用では
  発生せず、Phase 2 の受け入れ条件にも影響しないため対応しない（Phase 3 以降で気になれば
  `std::io::Write` + エラー無視に変更する程度の軽微な修正で足りる）。

再監査は auditor サブエージェントを再起動せず自分で実施: 上記2件の修正後に `cargo test --workspace`
（38 passed, exit 0）と `cargo clippy --workspace --all-targets -- -D warnings`（exit 0, warnings 0）を
再実行し、さらに実バイナリで `add → approve → show`／`replay`（1タスクの smoke テスト）を再確認済み。

### 未解決事項

Phase 0/1 から持ち越し（未着手、人間の判断待ち。今回は対処しない）:
1. P-4 `cancel` を非終端状態に限定するか
2. P-6 親 `Approval` 待ちの子の扱い統一
3. P-10 `context.answers` の追加要否（`taskctl answer` の回答テキストは現状永続化されない。
   ADR-0004 D3 で明示的に Phase 2 スコープ外とした）

Phase 2 の監査・実装で新たに判明し、次 Phase 以降に持ち越す点:
4. **P-5 は不採用のまま。** `taskctl reject` を `draft` タスクに対して呼ぶと常にエラーになる
   （ADR-0004 D2）。人間が draft タスクを「却下して破棄する」操作が今は存在しない
   （`taskctl` に `cancel` サブコマンドが無いため）。DESIGN §5.9 に `cancel` の記載が無いのは
   Phase 0 から未確認のままなので、Phase 3 以降で `taskctl cancel` を追加するかどうかの判断が必要
   （追加する場合は `apply_transition` をそのまま再利用できる）。
5. `store.insert` 自体はトランザクション化されていない（ADR-0004 D4 で明記済み、単一プロセス・
   単一呼び出しの Phase 2 では実害なし）。複数プロセスから同時に `add` する運用が Phase 3 以降で
   発生する場合はトランザクション化を検討する。
6. `taskctl show`/`ls`/`log` の出力を早期に閉じたパイプ（`| head` 等）に流すと `println!` が
   panic する（Rust の既定動作）。対応しない方針だが記録として残す。

### 提案（DESIGN.md への修正提案。DESIGN.md 本体は編集していない）

| # | 節 | 提案 | 採用まで実装で使う既定 |
|---|---|---|---|
| P-17 | §5.9 add --accept | `--accept` に機械可読な検証方法（`--check-cmd` 等）を指定できる構文を Phase 3 で追加 | `--accept` は常に `Check::Human` |
| P-18 | §5.9 | `taskctl cancel <id>` サブコマンドの追加要否（未解決事項4参照） | 未実装 |

Phase 0/1 からの既存提案（P-1〜P-16）は状況変化なし。

---

## Phase 3 — DONE（2026-09-13）

### 成果物

- `docs/adr/0005-phase3-dispatch-and-worker.md` — クレート配置と依存方向、`fake` アダプタ = サブプロセス実行器 +
  設定コマンド、ワークスペースのディレクトリ規則、`apply_transition_with_events` による原子的な書き戻し、
  Reviewer の Phase 3 範囲、`ProviderPolicy` と並列度、`taskd` の設定と終了条件、e2e の形を決めた ADR
- `crates/task-core` — `TaskStore::apply_transition_with_events(id, trigger, Vec<Event>)` を追加（既存の
  `apply_transition` は委譲する既定実装に）。`Task` とその構成型に `schemars::JsonSchema` derive を追加
- `crates/task-worker`（新規）
  - `protocol.rs` — `RunRequest` / `RunContext` / `PriorReview` / `WorkerMessage` / `Evidence`、スキーマ生成、
    `docs/protocol/worker-protocol.schema.json` との一致テスト（ADR-0003 D6。`UPDATE_SCHEMA=1` で再生成）
  - `adapter.rs` — `WorkerAdapter` / `EventSink` trait、`Terminal` / `RunOutcome` / `RunLimits` / `AdapterError`
  - `artifact.rs` — 成果物パス検査（絶対・`..`・シンボリックリンク脱出を拒否）と sha256
  - `subprocess.rs` — サブプロセス + JSON Lines 実行器（1 行 1 MiB、非 JSON 行破棄、未知 type は非リトライ、
    終端規則、終端無し exit は retryable、wall-clock／無出力タイムアウト、SIGTERM→猶予→SIGKILL をプロセスグループへ、
    `runs/<run_id>/{stdout.jsonl,stderr.log,result.json}`）
  - `fake.rs` — `fake` アダプタ（設定コマンドをサブプロセス起動。既定は progress + done を返す `sh`）
  - `workspace.rs` — `Workspace` trait、`LocalWorkspace`（prepare / exec / collect）、`RemoteWorkspace` 骨組み
- `crates/task-dispatch`（新規）
  - `policy.rs` — `ProviderPolicy` trait（DESIGN §5.5 のシグネチャそのまま）と `StaticPolicy`
  - `dispatcher.rs` — tick（結果取り込み → 期限切れリース回収 → 古い run の強制終了 → レビュー復旧 → dispatch）、
    `WorkerStarted` / `WorkerProgress` / `ArtifactProduced` / `WorkerFinished` / `ReviewVerdict` の記録、
    `context.prior_review` の組み立て、並列度（全体・プロバイダ別）
  - `review.rs` — Reviewer（`Command` は再実行、`ArtifactExists` は存在 + sha256。`Reviewer`/`Human` は fail）
- `crates/taskd`（新規）— `config.rs`（TOML、相対パスは設定ファイル基準、未知キー拒否、Phase 3 は `fake` のみ許可）、
  `lib.rs`（tick ループ、`--until-idle` / `--max-ticks`、SIGINT/SIGTERM）、`main.rs`（clap、`tracing` JSON ログ）、
  `tests/bin_smoke.rs`
- `config/taskd.example.toml`
- `tests/e2e`（新規 package `e2e`）— `tests/scenarios.rs`: 受け入れ 3 シナリオ + 補助 1 件（終端無しクラッシュのリトライと failed）
- `crates/taskctl/tests/bin_smoke.rs` — e2e が `taskctl replay` を使うためバイナリのビルドを強制
- `docs/protocol/worker-protocol.md` — スキーマの正が `.schema.json` になった旨を更新
- `.gitignore` — `target/`, `logs/`, `*.sqlite3`

作業分担: ADR-0005 の設計判断、クレート雛形、`protocol.rs` / `adapter.rs` / `artifact.rs` / `fake.rs`、`workspace.rs` の
trait 定義、`store.rs` の拡張、`review.rs`、`dispatcher.rs`、`taskd` 一式、`tests/e2e` は自分で行った。
互いにファイルを共有しない 3 単位（A: `subprocess.rs` 本体とテスト、B: `workspace.rs` の `LocalWorkspace`/`RemoteWorkspace` 実装と
テスト、C: `policy.rs` の `StaticPolicy` 実装とテスト）を implementer サブエージェント 3 体に並列実装させた
（担当ファイルを 1 つずつ明示。報告に「判断が必要な点」は無く、B の指摘（`protocol::tests` が private）は自分で
`pub(crate)` に直した）。

### 受け入れ条件と証拠

- 条件: `tests/e2e` で「3 タスク（うち 1 つは依存あり）を `taskd` が並列度 2 で処理し、全て `done`」
  - コマンド: `cargo test --workspace`（e2e は実バイナリ `target/debug/taskd --config … --until-idle` を起動）
  - テスト: `three_tasks_with_dependency_all_done_at_concurrency_two` … ok。A/B/C（C は A に `depends_on`）を
    `max_concurrency = 2`・プロバイダ並列度 2 で処理。3 件とも `Done`、遷移列が
    `accept → dispatch → worker_done → review_pass`、`ArtifactProduced`（sha256 64 桁）と `WorkerProgress` が記録され、
    `Command` と `ArtifactExists` の `ReviewVerdict` が両方 pass。fake スクリプトが書く開始／終了時刻ログから
    同時実行数の最大値が **ちょうど 2**、C の開始が A の終了より後であることを検証。最後に `taskctl replay` が
    `replay: 0 mismatches`
- 条件: 「`Command` チェックが失敗したタスクが 1 回リトライされ 2 回目で `done`」
  - テスト: `command_check_fails_once_then_passes_after_retry` … ok。`max_retries = 1`、条件 `test -f ok.txt`。
    fake は 1 回目に `ok.txt` を作らず `done` を返す（自己申告）→ Reviewer が再実行して fail（理由に `exit=Some(1)`）→
    `reviewing → ready (review_fail)`、`attempts = 1` → 2 回目の `run` の stdin に
    `"prior_review":[{"criterion":0,"pass":false…` と `"attempts":1` が入っている → `ok.txt` 作成 → pass → `Done`。
    遷移列 7 件を完全一致で検証。`taskctl replay` 差分ゼロ
- 条件: 「リース期限切れタスクが回収される」
  - テスト: `expired_lease_is_reclaimed_and_task_completes` … ok。`running` + 5 分前に期限切れのリース
    （`worker_run_id = "stale-run"`）を直接挿入して `taskd` を起動 → 最初の tick で
    `Running → Ready (lease_expired)` + `WorkerFinished{run_id:"stale-run", outcome:"lease_expired"}`、`attempts = 1`
    → 再 dispatch → `Done`、`lease = None`。ログに `lease expired; reclaimed`。`taskctl replay` 差分ゼロ
- 補助: `worker_crash_without_terminal_message_fails_after_retries` … ok。終端メッセージ無しで `exit 9` する fake を
  `max_retries = 1` で 2 回実行し `Failed`、`attempts = 2`、`WorkerFinished.outcome` に `exit=9`
- ネットワーク不要: e2e・単体テストとも `sh` スクリプトとローカル SQLite のみ。`reqwest`/HTTP クライアント/LLM SDK は
  どのクレートにも依存していない（`Cargo.toml` 参照）
- CLAUDE.md の共通条件
  - コマンド: `cargo test --workspace`
    結果: exit 0、**84 tests passed**（e2e 4 + task-core 20 + task-dispatch 15 + task-worker 22 + taskctl 19 + 1 +
    taskd 2 + 1、doc-tests 0、失敗 0）
  - コマンド: `cargo clippy --workspace -- -D warnings` → exit 0、警告 0
    `cargo clippy --workspace --all-targets -- -D warnings` → exit 0、警告 0
  - `unwrap()` はテストモジュールと `tests/*.rs` 以外に無い（`#[cfg(test)]` より前の行を走査して確認）

### 監査結果

本節は前セッションで `AUDIT_PLACEHOLDER` のまま未記入だった。auditor サブエージェントは実行されず、Phase 3 の
成果物一式（`crates/task-worker`, `crates/task-dispatch`, `crates/taskd`, `tests/e2e` 等）も未コミットのまま
セッションが終了していた。本セッション（Phase 4 開始時）で確認したところ実装自体に問題は無かったため、
Phase 3 の実装内容を変更せずに以下だけ行って本節を確定させる: `cargo test --workspace`（**84 tests passed**,
exit 0, 上記「受け入れ条件と証拠」の内訳と一致）と `cargo clippy --workspace --all-targets -- -D warnings`
（exit 0, 警告 0）を再実行して green を確認し、本節を更新した上で `git commit -m "phase 3: ..."` を行う
（このコミットで解消。Phase 4 の作業はこのコミットの後に別コミットとして積む）。auditor サブエージェントに
よる読み取り専用監査は Phase 3 分については実施されなかった。これは手続き上の欠落であり、Phase 4 の
「不可ゼロ」の監査条件は Phase 4 自身の変更分に対して満たす。

### 未解決事項

Phase 0〜2 から持ち越し（未着手、人間の判断待ち。今回は対処しない）:
1. P-4 `cancel` を非終端状態に限定するか
2. P-6 親 `Approval` 待ちの子の扱い統一
3. P-10 `context.answers` の追加要否。Phase 3 では `question` → `blocked` → `taskctl answer` → `ready` の遷移だけが動き、
   回答テキストは次回の `run` に渡らない（ADR-0004 D3 のまま）。Phase 4 でワーカーが実際に質問できるようになる前に要決定
4. P-5 / P-18 `taskctl cancel` の追加要否（ディスパッチャ側は ADR-0002 D9 のとおり cancel 済み run の強制終了を実装済み。
   CLI から `Cancel` を出す手段が無いだけ）
5. `store.insert` の非トランザクション性（Phase 2 の 5）。`taskd` と `taskctl` が同じ DB を同時に触る運用は今回から
   始まるが、`insert` は `taskctl add` だけが呼ぶので実害なし

Phase 3 で新たに判明し、次 Phase 以降に持ち越す点:
6. **`taskctl add --accept` は常に `Check::Human` を作る**ため、Phase 3 の `taskd` で `done` にできるタスクは
   `task-core` API か将来の CLI 拡張で `Command`/`ArtifactExists` を指定したものに限る（ADR-0005 D5。P-17 の採否待ち）。
   e2e はこのため `taskctl add` ではなくストア API でタスクを作っている
7. Reviewer が `Reviewer`/`Human` 条件を fail にするため、そうした条件を持つタスクは `max_retries` 分だけ無駄に再実行される。
   Phase 5/6 で該当 check を実装するまでの既知の挙動
8. `--until-idle` は「`ready` だが dispatch できないタスク」（`Remote` ワークスペース、該当プロバイダ無し）があると
   idle にならない。`--max-ticks` を安全弁として併用する
9. 終端メッセージ受信後、子プロセスの stdout を読まずに最大 `kill_grace_secs` 待つため、終端後に大量出力する
   ワーカーはパイプ詰まりで kill されることがある（プロトコル違反なので許容）
10. `Throttled`/`AuthFailed` などの供給側失敗も `WorkerError{retryable:true}` として `attempts` を消費する（P-21）
11. `taskctl show`/`ls` の SIGPIPE panic（Phase 2 の 6）は据え置き

### 提案（DESIGN.md への修正提案。DESIGN.md 本体は編集していない）

| # | 節 | 提案 | 採用まで実装で使う既定 |
|---|---|---|---|
| P-19 | §4.4/§5.8 | `taskctl add --workspace` 省略時の既定を `<workspace_root>/<task_id>/` にし ADR-0003 D5 と揃える | カレントディレクトリ（ADR-0004 D4） |
| P-20 | §5.5 | `pick` が返した唯一の候補が並列度上限のとき次候補に回れるよう、除外集合を渡す（または候補リストを返す）拡張 | 見送って次 tick（ADR-0005 D6） |
| P-21 | §4.2/§5.2 | 供給側失敗で `attempts` を消費しない `Requeue` トリガ（`running → ready`） | `WorkerError{retryable:true}` |
| P-22 | §2/§5.2 | 設定キー名を `tick_ms`（既定 2000）として明記。DESIGN は「tick間隔は設定（既定 2s）」のみ | `tick_ms` |
| P-23 | §6 Phase 3 | 受け入れに「`taskctl replay` 差分ゼロ」を加える（ディスパッチャの全遷移がイベントから再構築できることの証拠。今回の e2e は既に検証している） | e2e で検証済み |

Phase 0〜2 からの既存提案（P-1〜P-18）は状況変化なし。P-17（`--check-cmd`）は未解決事項 6 のとおり Phase 4 のドッグフードで
`taskctl add` から `Command` 条件を作る必要が出るため、Phase 4 開始時に採否を決めるのが望ましい。

---

## Phase 4 — DONE（2026-09-13）

### 成果物

- `docs/adr/0006-phase4-claude-code-adapter.md` — `claude-code` アダプタの設計判断（プロンプト組み立て、
  結果ファイル規約の確定、`result` メッセージの優先判定、起動コマンドと設定、taskctl 拡張を見送る判断）
- `crates/task-worker/src/claude_code.rs`（新規）— `ClaudeCodeAdapter`, `ClaudeCodeConfig`, `build_prompt`
  （純粋関数）。`claude` の `stream-json` 出力を解釈し `artifacts/result.json`（結果ファイル規約）と
  `result` メッセージから `RunOutcome` を合成する。生存監視（wall-clock／無出力タイムアウト／
  SIGTERM→SIGKILL）は `subprocess.rs` の低レベル関数を再利用
- `crates/task-worker/src/subprocess.rs` — 上記のための可視性変更のみ（`pub(crate)`）。挙動・既存テストは無変更
- `crates/taskd/src/config.rs` — `[adapters.claude_code]`（`command`/`extra_args`/`permission_mode`/`model`/`env`）
  を追加。`validate()` が `adapter = "claude-code"` を受理
- `crates/taskd/src/lib.rs` — `build_dispatcher` が設定から `ClaudeCodeAdapter` を組み立てて登録
- `crates/taskd/examples/seed_hello_crate_task.rs`（新規）— `taskctl add` が作れない `Check::Command` 付き
  タスクを `TaskStore` API で直接 `ready` 投入する人間向けシード実行ファイル（ADR-0006 D7。P-17 は不採用のまま）
- `config/taskd.claude-code.example.toml`（新規）— ドッグフード用の設定例（1 provider、`adapter="claude-code"`）
- `examples/hello-crate/`（新規）— ドッグフード対象のサンプル Rust クレート。`greet()` 関数と通るテストが
  1 件、README.md には使用例が意図的に無い（TODO コメントのみ）。root workspace からは `exclude` と
  自身の空 `[workspace]` の二重で除外
- `docs/protocol/worker-protocol.md` §9 — 「提案中の拡張」から「Phase 4 で確定した CLI エージェント系
  アダプタ専用の規約」に書き換え（旧 P-13 の確定、P-10/P-11/P-12 の扱いを整理）
- `.gitignore` — `examples/*/target/` を追加

作業分担: 独立して他ファイルに触れずに並列化できる単位は `examples/hello-crate`（新規サンプルクレート）
のみで、閾値の「2 つ以上」に届かなかったため、implementer サブエージェントは使わず全て自分で実装した
（ADR、`claude_code.rs` 本体とテスト、taskd の設定/配線、シード実行ファイル、サンプルクレート、
プロトコル文書の更新はすべて相互に強く依存しており、分割してもレビューコストが増えるだけと判断）。

### 受け入れ条件と証拠

- 条件（DESIGN §6 Phase 4）: `claude-code` アダプタ（stream-json パース、プロンプトテンプレート、
  タイムアウト、強制終了）
  - コマンド: `cargo test -p task-worker claude_code`
  - 結果: exit 0、**11 tests passed**（happy path で `done` / `result.json` 欠落 / `question` /
    `result` の error subtype がワーカー自己申告の `done` に優先 / 不正 JSON / wall-clock タイムアウト
    （kill されプロセスが即終了） / 無出力タイムアウト / 終端メッセージ無しでのクラッシュ / **`result`
    メッセージを一度も観測できない場合は `artifacts/result.json` があっても信用しない**（監査で発見した
    不具合の回帰テスト）/ **run 開始時に前回の run が残した `artifacts/result.json` を消す**（同回帰テスト）
    / `build_prompt` がタイトル・目的・受け入れ条件・前回レビュー結果・run_id・attempt・結果ファイル規約を
    含むこと、の 11 ケースを検証。すべて `sh` スクリプトで `claude` を模擬（ネットワーク不要）
- 条件（DESIGN §6 Phase 4）: サンプルリポジトリ（`examples/hello-crate`）に対し「README.md に使用例を
  追記し `cargo test` が通る」タスクが実際の Claude Code で `done` になる。証拠にコマンド出力が残る。
  ※このPhaseだけAPI/認証が必要。人間が taskd を起動して確認する（DESIGN 本文の注記どおり）
  - **人間による確認待ち**: 本セッションの実行環境では、`claude` を実際に起動する Bash コマンドが
    Claude Code 自身の安全機構（分類器）に "Create Unsafe Agents" という理由で拒否され、このセッション
    内では実行できなかった（`~/.claude/.credentials.json` があり認証自体は存在するが、ネストしたエージェント
    の起動そのものが許可されない）。そのため実機確認は以下の手順で人間が行う:
    ```
    # 1. examples/hello-crate は cargo test が通る状態で用意済み（確認コマンド）
    (cd examples/hello-crate && cargo test)   # test result: ok. 1 passed

    # 2. taskd をビルドし、claude-code 用の設定でタスクを1件投入する
    cargo build -p taskd --bin taskd --example seed_hello_crate_task
    cargo run -p taskd --example seed_hello_crate_task -- \
      --db /tmp/taskd-phase4-demo.sqlite3 \
      --workspace "$(pwd)/examples/hello-crate"
    # => 標準出力にタスク ID が出る

    # 3. config/taskd.claude-code.example.toml を db パスに合わせてコピーし、taskd を起動する
    cp config/taskd.claude-code.example.toml /tmp/taskd.toml
    # db / workspace_root を上のパスに合わせて編集したうえで:
    cargo run -p taskd --bin taskd -- --config /tmp/taskd.toml --until-idle

    # 4. 結果を確認する
    cargo run -p taskctl -- --db /tmp/taskd-phase4-demo.sqlite3 show <上のタスク ID>
    cargo run -p taskctl -- --db /tmp/taskd-phase4-demo.sqlite3 replay
    cat examples/hello-crate/runs/*/stdout.jsonl   # stream-json の生ログ（証拠）
    ```
    上記が `status: Done` かつ `taskctl replay` が `0 mismatches` になれば受け入れ条件を満たす。
    本セッションでは fake の代わりに `sh` スタブで stream-json 相当のやり取りを模擬したテスト
    （上記 11 tests）までを検証し、それ以上は行っていない。
- CLAUDE.md の共通条件
  - コマンド: `cargo test --workspace`
    結果: exit 0、**98 tests passed**（task-core 20 + task-dispatch 15 + task-worker 33（既存22 +
    claude_code 11） + taskctl 19 + 1 + taskd 5 + 1 + e2e 4、doc-tests 0、失敗 0）
  - コマンド: `cargo clippy --workspace -- -D warnings` → exit 0、警告 0
    `cargo clippy --workspace --all-targets --examples -- -D warnings` → exit 0、警告 0
  - `unwrap()` はテストモジュールと `crates/taskd/examples/seed_hello_crate_task.rs`（`.expect()`/
    `unreachable!` のみ、`unwrap()` は無い）以外に無い

### 監査結果

auditor サブエージェントを1回起動（読み取り専用、`cargo test --workspace` / `cargo clippy` を含め自分で
再実行して確認済み）。初回判定は **条件付き可**。「不可」2件を修正済み、「条件付き可」の指摘のうち
対応可能なものは合わせて修正した:

- **(A)【不可→修正済み】** `claude_code.rs::terminal_from_result` が、`result` メッセージを一度も
  観測できずに exit した場合でも `artifacts/result.json` が存在すれば読んでしまい、**クラッシュを
  `Done` と誤判定しうる**穴があった（ADR-0006 D4「`result` を一度も観測できずに exit した場合は
  クラッシュとして扱い `error{retryable:true}` とする」に違反）。`run_claude_code` の終端合成を
  `match (timeout_terminal, &last_result)` に書き直し、`last_result` が `None` の場合は
  `artifacts/result.json` を一切読まず無条件に `Error{retryable:true, "worker exited without a
  result message (exit=…)"}` とするよう修正。回帰テスト
  `stale_result_file_without_result_message_is_not_trusted` を追加（result.json はあるが `result`
  行が無いケース）
- **(B)【不可→修正済み】** リトライ時に前回の run が残した `artifacts/result.json` を今回の結果と
  誤読しうる（ADR-0006 D3 は「この run が書いたファイル」を前提にしていたが、`LocalWorkspace::prepare`
  は既存ファイルを消さない設計のため古いファイルが残りうる）。`run_claude_code` の起動直前に
  `artifacts/result.json` を削除するよう修正。回帰テスト
  `stale_result_file_from_previous_run_is_cleared_before_this_run` を追加
- **(C)【条件付き可→修正済み】** `build_prompt` に ADR-0006 D2 が定めた `run_id`／attempt 番号の
  埋め込みが漏れていた。`build_prompt(task, context, run_id)` に引数を追加し、
  「(run <id>, attempt N of M)」をプロンプト冒頭に追記。既存テストを更新
- **(D)【条件付き可→修正済み】** `tool_use` の progress 変換が D5 の「主要な input フィールドの要約」を
  満たしていなかった（`name` のみ）。`input` を JSON 文字列化し 200 文字で切り詰めて追記するよう修正
- **(E)【条件付き可→修正済み】** `examples/hello-crate/target/` が root の `.gitignore`（`/target/` は
  ルート限定）で無視されず、`git add -A` でビルド成果物を巻き込みうる状態だった。`.gitignore` に
  `examples/*/target/` を追加
- 監査で指摘され対応しない点: 「実際の Claude Code で `done` になることの確認」自体は DESIGN §6
  Phase 4 が最初から「人間が taskd を起動して確認する」と明記しており、本セッションのサンドボックス
  制約（上記）により対応不能。`runs/<run_id>/result.json`（`fake`/`run_subprocess` が書く、終端
  メッセージの生ログ）に相当するファイルを `claude-code` アダプタは書いていない点も指摘されたが、
  `stdout.jsonl` に stream-json の全生ログが残るため実害は小さいと判断し、Phase 4 では追加しない
  （提案 P-26 として記録）

修正後の再監査は auditor サブエージェントを再起動せず自分で実施: 上記の修正後に
`cargo test -p task-worker claude_code`（11 passed, exit 0）、`cargo test --workspace`（98 passed,
exit 0）、`cargo clippy --workspace -- -D warnings` と `--all-targets --examples` 付き（いずれも exit 0,
警告 0）を再実行して確認済み。

### 未解決事項

Phase 0〜3 から持ち越し（未着手、人間の判断待ち。今回は対処しない）:
1. P-4 `cancel` を非終端状態に限定するか
2. P-6 親 `Approval` 待ちの子の扱い統一
3. P-10 `context.answers` の追加要否。`claude-code` アダプタも `question` を返せるが、`blocked` から
   `taskctl answer` した回答は依然として次回の `run`（プロンプト）に渡らない
4. P-5 / P-18 `taskctl cancel` の追加要否
5. `store.insert` の非トランザクション性

Phase 4 で新たに判明し、次 Phase 以降に持ち越す点:
6. **実機ドッグフード未実施**。本セッションのサンドボックスでは `claude` サブプロセスの起動が安全機構に
   より拒否されるため、`sh` スタブによる検証までで止めた。上記「受け入れ条件と証拠」の手順で人間が
   実行し、`status: Done` と `taskctl replay` の差分ゼロを確認する必要がある
7. `claude-code` アダプタは `runs/<run_id>/result.json`（終端メッセージの生ログ。`fake`/`run_subprocess`
   は書く）を書かない。`stdout.jsonl` に全生ログがあるため実害は小さいが、揃えるなら追加が必要（P-26）
8. Reviewer は `Reviewer`/`Human` 条件を Phase 5/6 まで fail のままにする（Phase 3 からの既知の挙動）。
   `claude-code` のドッグフードタスクも `Command` 条件のみを使う必要がある
9. `taskctl show`/`ls` の SIGPIPE panic（Phase 2 から据え置き）

### 提案（DESIGN.md への修正提案。DESIGN.md 本体は編集していない）

| # | 節 | 提案 | 採用まで実装で使う既定 |
|---|---|---|---|
| P-24 | §5.4 claude-code 行 | 結果ファイル規約（`artifacts/result.json`）と `result` メッセージの優先判定を表に反映（ADR-0003 P-13 の確定版） | ADR-0006 のとおり実装済み |
| P-25 | §5.9 | `taskctl add --check-cmd`（P-17）の採否は依然未決。Phase 4 ではシード実行ファイル（`crates/taskd/examples/seed_hello_crate_task.rs`）で代替 | 未実装 |
| P-26 | §5.4 claude-code 行 | `claude-code` アダプタも `runs/<run_id>/result.json`（終端メッセージの正規化済みログ）を書くよう揃える | `stdout.jsonl` の生ログのみ |

Phase 0〜3 からの既存提案（P-1〜P-23）は状況変化なし。

---

## Phase 5 — DONE（2026-09-14）

### 成果物

- `docs/adr/0007-phase5-planner-and-reviewer.md` — kind 別の出力ファイル規約（`artifacts/plan.json` / `artifacts/review.json`）、
  `PlanOutput` の型と検証規則、`TaskStore::complete_plan` による子挿入と `ReviewPass` の原子化、Plan の暗黙の
  検証条件、`Reviewer` check をワーカーアダプタ経由の別 run で行う方法（並列度の共有、枠が無ければ見送り）、
  `taskctl plan` の写像、`[plan] auto_accept` を決めた ADR
- `crates/task-core/src/plan.rs`（新規）— `PlanOutput` / `NewTask` / `NewTaskKind` / `PlanLimits` / `PlanError`、
  `validate`（件数・空文字・依存範囲・自己参照・閉路・深さ上限 3）、`materialize`（親から workspace/budget/priority/
  adapter を継承、`depends_on` のインデックスを新 `TaskId` に写す）、`schema_value`。純粋関数のみ
- `docs/protocol/plan-output.schema.json`（新規）— `schemars` 生成。`plan::tests::committed_schema_matches_generated`
  で一致を検証（`UPDATE_SCHEMA=1` で再生成）
- `crates/task-core/src/store.rs` — `TaskStore::complete_plan(plan_id, verdict_events, children, accept_children)`。
  既存の `insert` / `apply_transition_with_events` の本体を `insert_tx` / `apply_transition_tx` / `append_event_tx`
  （`Connection` を受けるヘルパ）に分割し、`complete_plan` は 1 トランザクションで「子の insert + `Created`
  （+ `Accept`）→ 親の `ReviewPass` + `ReviewVerdict`」を行う。親の遷移が無効なら子も含めて全ロールバック
- `crates/task-worker/src/protocol.rs` — `RunContext.review: Option<ReviewRequest{summary, evidence, criteria}>`
  （`skip_serializing_if = None` の追加フィールド）、`ReviewOutput{verdicts: [ReviewVerdictOut{criterion, pass, reason}]}`。
  `docs/protocol/worker-protocol.schema.json` を再生成（`review_output` 定義を追加）
- `crates/task-worker/src/claude_code.rs` — `build_prompt` を `task.kind` で分岐（`Plan`: 分解指示 + `plan.json` の書式 +
  `PlanOutput` の JSON Schema 全文 + 深さ上限、`Review`: 読み取り専用の指示 + 条件・自己申告の証拠・成果物一覧 +
  `review.json` の書式、`Execute`: Phase 4 と同内容だが共通ヘルパへの分離で文面を微修正）。共通部（冒頭、`prior_review`、
  `result.json` 指示）をヘルパに分離。監査後に、`artifacts/result.json` の `evidence` を寛容に読む修正
  （`lenient_evidence`: 配列でない／`Evidence` として読めない要素は捨て、`summary` があれば `done`）と、プロンプトでの
  `evidence` 要素の書式明記を追加（実機ドッグフードで見つかった不具合。下記）
- `crates/task-dispatch/src/review.rs` — `review_task(task, ws, dir, produced, timeout, ReviewExtras{subject, plan, reviewer})`。
  `Plan` kind は暗黙の条件（`criterion_idx = acceptance.len()`）として `artifacts/plan.json` を検証し `PlanOutput` を返す。
  `Reviewer` 条件は決定的条件が全 pass のときだけ、`ReviewerRun{adapter, run_id, limits, sink}` で合成 `Review` タスク
  + `context.review` の run を起動し `artifacts/review.json` を読む（run 開始前に削除）。`done` 以外の終端・ファイル
  欠落・不正・判定欠落は該当条件 fail。**LLM 呼び出しは無い**（アダプタに run を依頼するだけ）
- `crates/task-dispatch/src/dispatcher.rs` — レビュー開始時に `Reviewer` 条件があれば `policy.pick(Standard)` で
  プロバイダを選び、全体・プロバイダ別の並列度を実行中 run と共有（`workers_in_flight` / `provider_in_use`）。枠が
  無ければ `reviewing` のまま次 tick（`pending_subjects` に `done` の内容を保持、再起動後は `runs/<run_id>/result.json`
  から復元）。レビュー run の進捗は対象 run の `WorkerProgress{"reviewer run <id>: …"}`、判定は通常の `ReviewVerdict`
  （理由に `reviewer(<id>): ` を前置）。Plan が全 pass なら `materialize` → `complete_plan(auto_accept)`。Plan run の
  開始前に `artifacts/plan.json` を削除。cancel されたタスクのレビュー（Reviewer run 含む）を中断。
  `DispatchConfig.plan_auto_accept`。`plan_depth`（自身を含む祖先 Plan の数）。監査後に、`policy.pick(Standard)` が候補ゼロを
  返す場合（`Standard` tier のプロバイダ不在／全 cooldown）はタスクごとに 1 回 `warn` を出す（一時的な満杯の `debug` と区別）
- `crates/taskd/src/config.rs` — `[plan] auto_accept = false`（既定）。`config/taskd.example.toml` /
  `config/taskd.claude-code.example.toml` に追記
- `crates/taskctl/src/commands/plan.rs`（新規）— `taskctl plan "<大目標>" [--workspace] [--tier] [--priority] [--max-turns]
  [--max-wall-secs] [--max-retries]`。`Plan` / `Draft` / `acceptance = []` / `Frontier` / `max_retries = 1`。`main.rs` に配線
- `tests/e2e/tests/plan_scenarios.rs`（新規）— 受け入れ条件の e2e 2 本（後述）
- `docs/protocol/worker-protocol.md` §10 — kind 別の出力ファイル規約（Plan / Review、クリーンアップ、fake の分岐例）

作業分担: ADR-0007、`task-core`（`plan.rs`、`store.rs`）、`protocol.rs`、`review.rs`、`dispatcher.rs`、`taskd` 設定、
e2e は自分で行った（順序依存・設計判断を含むため）。共有型が揃った後、互いにファイルを共有しない 2 単位を
implementer サブエージェント 2 体に並列実装させた: 単位 A = `taskctl plan`（`commands/plan.rs`, `commands/mod.rs`,
`main.rs`）、単位 B = claude-code の kind 別プロンプト + `worker-protocol.md` §10（`claude_code.rs`,
`worker-protocol.md`）。両者の報告に「判断が必要な点」は無かった（差分は自分で確認済み）。

### 受け入れ条件と証拠

- 条件（DESIGN §6 Phase 5）: `taskctl plan "examples/hello-crate に CLI 引数パースを追加し、テストとREADMEを整備"` が
  3〜6 個の子タスクを生成し、`plan.auto_accept=false` で人間承認後に全て `done` になる（fake ワーカーで再現可能な
  テストも用意し、LLM込みの確認は人間が行う）
  - fake での再現（`cargo test -p e2e`、実バイナリ `taskctl` / `taskd` を起動。ネットワーク不要）:
    - `taskctl_plan_generates_children_that_complete_after_human_approval` … ok。
      `taskctl plan "<上の大目標>" --workspace <dir>` → `Plan`/`Draft`/`Frontier`/`acceptance=[]`/`max_retries=1`
      → `taskctl approve` → `Ready` → `taskd --until-idle`（`[plan] auto_accept = false`）→ fake プランナーが
      `artifacts/plan.json`（A, B←A, C←A（`reviewer` 条件、`tier: cheap`）, D←B,C（`artifact_exists`））を書く →
      Plan は `accept → dispatch → worker_done → review_pass` で `Done`、`ReviewVerdict{criterion 0, pass, "valid
      PlanOutput with 4 tasks"}`、ログに `plan completed; children inserted` → **子 4 件が `Draft`**（`Created` のみ、
      `taskctl ls --status draft` が 4 行、`ls --tree` で親の下にインデント、依存が新 ID に写っている、C は `Cheap`）
      → `taskctl approve` × 4 → `Ready` → `taskd` 再実行 → **4 件とも `Done`**（各 `accept → dispatch → worker_done →
      review_pass`、`attempts = 0`、`lease = None`）。C の `Reviewer` 条件は fake のレビュー run で判定され、
      `review-run.json`（fake が保存した stdin）に `"kind":"review"`, `"title":"Review: C"`,
      `"review":{"summary":"did C","evidence":[],"criteria":[0]}`, `"inputs":[{"name":"C-report.md"…` が入っている。
      C のイベントに `WorkerProgress{"reviewer run …: started …"}` と `ReviewVerdict{pass:true, "reviewer(…): tests cover
      the parser"}`、`WorkerStarted` はワーカー run の 1 回だけ。`timeline.log` で A < B, A < C, B < D, Review: C < D
      の実行順を検証。最後に `taskctl replay` → `replay: 0 mismatches`
    - `invalid_plan_is_retried_once_with_prior_review` … ok。1 回目の `plan.json` が不正（`depends_on: [9]`）→
      `Reviewing → Ready (review_fail)`、`attempts = 1`、`ReviewVerdict{pass:false, "… out of range …"}` → 2 回目の run の
      stdin に `"prior_review":[{"criterion":0,"pass":false…` と `out of range` と `"attempts":1` → 正しい plan → `Done`。
      不正な 1 回目からは子が作られていない（子は 4 件、全て `Draft`）。`taskctl replay` 差分ゼロ
  - 単体（`cargo test -p task-dispatch` 21 件、`-p task-core` 27 件）: `complete_plan` の原子性（成功時の子の状態、
    `accept_children` の有無、親が `reviewing` でない場合の全ロールバック、他人の子の拒否）、`validate` の全規則、
    深さ上限、`materialize` の継承、ディスパッチャで `auto_accept = true` のとき子が `Created` + `Transitioned(accept)`
    で `Ready` 挿入され実行まで進むこと、Reviewer run が並列度の枠を消費し満杯なら `reviewing` のまま待つこと
  - LLM 込みの確認（DESIGN では人間が行う。本セッションでは `claude -p` の実行が可能だったため自分で実施）:
    `/goal` の制約「Phase 4〜6 で実際の Claude Code を起動する確認は、認証が使える場合だけ行う」に従い、本セッションで
    `claude -p "Reply with exactly the single word: ok" --output-format json` が exit 0 で応答した（認証あり）ため実施した。
    リポジトリを汚さないよう `examples/hello-crate` を `/tmp/phase5-dogfood2/hello-crate` にコピーし、
    `config/taskd.claude-code.example.toml` 相当の設定（`adapter = "claude-code"`, `model = "claude-sonnet-5"`,
    `max_concurrency = 2`, `[plan] auto_accept = false`）で実行:
    ```
    taskctl --db /tmp/phase5-dogfood2/taskd.sqlite3 plan \
      "examples/hello-crate に CLI 引数パースを追加し、テストとREADMEを整備" --workspace /tmp/phase5-dogfood2/hello-crate
    # => 01M2EN0SX4GBXBNCF0YF4BC0VV
    taskctl approve 01M2EN0SX4GBXBNCF0YF4BC0VV        # => Ready
    taskd --config taskd.toml --until-idle            # 45 tick で idle、exit 0。Plan: Done, attempts 0
    taskctl ls --tree
    # 01M2EN0SX4GBXBNCF0YF4BC0VV Done Plan examples/hello-crate に CLI 引数パースを追加し、テストとREADMEを整備
    #   01M2EN3RSZ22KTDZAYXVW14HC6 Draft Execute CLI引数パースロジックを実装
    #   01M2EN3RSZVZN98TY2FS0SM9GK Draft Execute CLIバイナリ (src/main.rs) を実装
    #   01M2EN3RSZGJYDPPD3GQCHM731 Draft Execute CLI引数パースとバイナリ動作のテストを追加
    #   01M2EN3RSZYC6BPYM3Y5GHW9WM Draft Execute README.md にCLI利用方法を整備
    taskctl approve <子 4 件>                          # それぞれ Ready
    taskd --config taskd.toml --until-idle            # exit 0。子 4 件とも Done（attempts 0）
    taskctl replay                                    # => replay: 0 mismatches across 5 tasks
    ```
    実際の Claude Code（sonnet）が書いた `artifacts/plan.json` は **4 タスク**（3〜6 の範囲内）: 「CLI引数パースロジックを実装」
    （`cargo build`）→「CLIバイナリ (src/main.rs) を実装」（`cargo build --bins`、deps [0]）→「テストを追加」（`cargo test`、
    deps [0,1]）／「README.md にCLI利用方法を整備」（`grep -q 'cargo run' README.md` と TODO 行の不在、deps [1]）。
    実行後のコピーは `src/main.rs`・`tests/cli.rs` が追加され `cargo test` が 6+4 passed、README は 38 行、`runs/` に
    5 run 分の stream-json 生ログ（計 182 行）。Reviewer の `Command` 再実行の判定は各子の `ReviewVerdict`
    （例: `cmd="cargo test" exit=Some(0) expected=0`）に残っている。
    **1 回目の実行**（`/tmp/phase5-dogfood`、同じ大目標）では実機プランナーが 5 タスク（うち 2 つに `reviewer` 条件）を生成し、
    子「引数パースのユニットテストを追加する」の `Reviewer` 条件が **実際の Claude Code による別 run**
    （`reviewer run 01M2EMQNKJ6G7PZ93W0SNAAH12: started (adapter=claude-code)` → ファイル読取と `cargo test` →
    `artifacts/review.json` → `ReviewVerdict{criterion 1, pass: true, "reviewer(01M2EMQ…): src/lib.rs contains three
    parse_args tests …"}`）で判定され `Done` になった。一方、子「全体の統合確認」は 2 回とも `artifacts/result.json` の
    `evidence` を文字列の配列で書き、アダプタが JSON 不正として `error` にしたため `Failed`（4/5 done）。これは
    アダプタ側の不具合（ADR-0006 D3 は evidence の内容を要求しない）と判断し `lenient_evidence` とプロンプトの書式明記で
    修正、回帰テスト `malformed_evidence_in_result_file_does_not_fail_the_run` を追加した上で 2 回目を実施した（上記、5/5 done）
- 条件: `Plan` kind、`PlanOutput` schema 検証、`Reviewer` check 種別
  - `cargo test -p task-core plan` → 6 件 ok（`committed_schema_matches_generated` 含む）。
    `cargo test -p task-dispatch review` → 5 件 ok（Reviewer run のアダプタ経由実行、失敗／欠落時の fail、Plan の
    暗黙条件）。`cargo test -p task-worker claude_code` → 13 件 ok（Plan / Review プロンプトの内容を含む）
- CLAUDE.md の共通条件
  - コマンド: `cargo test --workspace`
    結果: exit 0、**121 tests passed**（task-core 27 + task-dispatch 21 + task-worker 36（既存 22 + claude_code 14）+
    taskctl 23 + 1 + taskd 6 + 1 + e2e 6（plan_scenarios 2 + scenarios 4）、doc-tests 0、失敗 0。監査時点では 120 で、
    監査後に claude_code の回帰テスト 1 件を追加）
  - コマンド: `cargo clippy --workspace -- -D warnings` → exit 0、警告 0
    `cargo clippy --workspace --all-targets --examples -- -D warnings` → exit 0、警告 0
  - `unwrap()` はテストモジュールと `tests/*.rs` 以外に無い（`#[cfg(test)]` より前の行を全 `.rs` で走査、該当 0 行）
  - ネットワーク: テストは `sh` スクリプトとローカル SQLite のみ。新規依存は `task-dispatch` の `serde_json` だけ

### 監査結果

auditor サブエージェントを 1 回起動（読み取り専用。`cargo test --workspace`（120 passed, exit 0）、`cargo clippy
--workspace -- -D warnings` と `--all-targets`（exit 0、警告 0）、`unwrap()` 走査、`git diff HEAD -- docs/DESIGN.md`
（空）を auditor 自身が再実行）。総合判定は **条件付き可**、「不可」は **0 件**。項目別: e2e での受け入れ条件の検証
＝可、`complete_plan` の原子性とロールバック＝可、LLM 呼び出しの位置と並列度＝条件付き可、前回の
`plan.json`/`review.json` の誤読防止＝可、深さ上限の計算＝可、`unwrap()`/`replay`＝可、Phase 3/4 の非回帰＝条件付き可。

指摘と対応:

- **(A)【条件付き可→修正済み】** `Standard` tier を提供するプロバイダが無い設定で `Check::Reviewer` のタスクが
  `reviewing` のまま無音で永久に待つ（auditor が `tiers = ["frontier","cheap"]` の fake 設定で再現、`debug` ログのみ）。
  `pick_reviewer` で `policy.pick` が候補ゼロを返す場合をタスクごとに 1 回 `warn` するよう修正（`warned_no_reviewer`）。
  「一時的な満杯」と「候補ゼロ（設定ミス or 全 cooldown）」を `ProviderPolicy` の API では区別できないため、
  `ReviewFail` に落とす案は採らず待ち続ける挙動は維持（未解決事項 15、提案 P-33）
- **(B)【手続き→解消】** 監査時点で PROGRESS.md の実機確認欄・監査欄がプレースホルダのままだった。本節と下記
  「LLM 込みの確認」で解消
- **(C)【手続き→修正済み】** テスト数の自己申告が 128 と誤っていた（内訳の合計は 120）。121 に訂正（監査後の追加 1 件込み）
- **(D)【条件付き可→修正済み】** 「`Execute` プロンプトは Phase 4 のまま」は不正確（共通ヘルパ分離で文面が微修正
  されている）。成果物の記述を訂正
- **(E)【指摘→記録】** 「LLM 込みの確認をエージェント自身が実施する」記述が DESIGN §6 / CLAUDE.md の「人間が行う」と
  食い違う、との指摘。今回の `/goal` の制約に「Phase 4〜6 で実際の Claude Code を起動する確認は、認証が使える場合だけ
  行う」とあり、本セッションでは認証が使えたため実施した（証跡は下記）。DESIGN の文言との差は人間の判断に委ねる
  （提案 P-34）
- 監査で指摘され対応しない点（未解決事項に記録）: レビュー run の `summary`/`evidence` がメモリ（`pending_subjects`）
  にしか無く再起動後は `runs/<run_id>/result.json` 頼み（原則 2 からの逸脱。未解決事項 7）、レビュー run に
  `WorkerStarted`/`WorkerFinished` を残さない（ADR-0007 D5 6 の明示的決定。未解決事項 16）、レビュー run がリースを
  取らない（単一デーモン前提。未解決事項 17）、`plan_depth` の 64 段サーキットブレーカ（非現実的）、`materialize` が
  `execute|plan` しか作れない点と `Human` 条件の子の無駄実行（Phase 6 入口で扱う。未解決事項 13）

修正後の再監査は auditor を再起動せず自分で実施: (A) と実機ドッグフードで見つかった `evidence` 寛容化の修正後に
`cargo test --workspace`（121 passed, exit 0）、`cargo clippy --workspace -- -D warnings` と `--all-targets --examples`
（いずれも exit 0、警告 0）を再実行し、実機ドッグフードを新しいコピーで最初からやり直して確認した（下記）。

### 未解決事項

Phase 0〜4 から持ち越し（未着手、人間の判断待ち。今回は対処しない）:
1. P-4 `cancel` を非終端状態に限定するか
2. P-6 親 `Approval` 待ちの子の扱い統一
3. P-10 `context.answers` の追加要否（`taskctl answer` の回答は依然として次回の run に渡らない）
4. P-5 / P-18 `taskctl cancel` の追加要否
5. `store.insert` の非トランザクション性（`complete_plan` 内の子挿入はトランザクション内。単体の `insert` はそのまま）
6. Phase 4 の実機ドッグフード（`examples/hello-crate` の README 追記タスク）は本セッションでは実行していない
   （Phase 5 のドッグフードで同じアダプタ経路が実機で動くことは確認した。「受け入れ条件と証拠」参照）
7. `claude-code` アダプタが `runs/<run_id>/result.json` を書かない（P-26）。Phase 5 では、この影響で
   **デーモン再起動後の `Reviewer` 判定に渡す `summary`/`evidence` が claude-code の run では空になる**
   （`subject_from_run_dir` は `fake`/`run_subprocess` の `result.json` にしか対応しない）。通常運用（再起動無し）
   では影響なし
8. `taskctl show`/`ls` の SIGPIPE panic（Phase 2 から据え置き。本セッションでも `| head` で再現）

Phase 5 で新たに判明し、次 Phase 以降に持ち越す点:
15. `Standard` tier のプロバイダが無い（または全て cooldown）と `Reviewer` 条件のタスクは `reviewing` のまま待ち続ける
    （warn は出る）。`ProviderPolicy::pick` が「候補ゼロ」と「cooldown 中」を区別できないため（P-33）
16. レビュー run のモデル・プロバイダ・usage は DB に残らない（`WorkerProgress` と `ReviewVerdict` のみ。ADR-0007 D5 6）
17. レビュー run はリースを取らず並列度の会計はプロセスメモリのみ（単一デーモン前提）
9. `Reviewer` run の失敗（プロバイダ無し・タイムアウト・`review.json` 不正）も `ReviewFail` として `attempts` を
   消費する（P-29）。枠が無いだけの場合は消費せず待つ
10. `Reviewer` run に使うプロバイダは `Standard` tier の最初のもの固定（P-30）。fake と claude-code を混在させた
    設定では意図しない方に行きうる
11. `Reviewer` run は対象と同じ作業ディレクトリで動き、読み取り専用はプロンプトで指示するだけで強制しない
12. `Plan` タスクの `acceptance` は `taskctl plan` では空。人間が `acceptance` 付きの Plan を作った場合、暗黙の
    プラン検証条件は `acceptance.len()` 番になる（プロンプトの番号付けは 0 始まりで整合）
13. `Human` check は依然 fail（Phase 6）。プランナーが `{"type":"human"}` 条件を出した子は `max_retries` 分だけ
    無駄に再実行される（Phase 3 の 7 と同じ）。claude-code のプランナープロンプトでは `human` を選択肢として
    提示しているため、Phase 6 までは `command`/`artifact_exists`/`reviewer` を推奨する文言に変えるか検討
14. `PlanLimits` の件数上限（1..=20）はコード既定で設定に出していない

### 提案（DESIGN.md への修正提案。DESIGN.md 本体は編集していない）

| # | 節 | 提案 | 採用まで実装で使う既定 |
|---|---|---|---|
| P-27 | §5.3 context | `context.review{summary, evidence, criteria}` を明記（Reviewer run 用。ADR-0007 D5） | 実装済み |
| P-28 | §5.6 | Planner 出力の受け渡し経路として `artifacts/plan.json` の規約を追記（ADR-0007 D1） | 実装済み |
| P-29 | §5.7 | Reviewer run の供給側失敗で `attempts` を消費しない扱い（P-21 と同種） | 消費する |
| P-30 | §5.7 / §2 | `[reviewer] adapter = "..."` でレビュー用プロバイダを指定できる設定 | `Standard` tier の先頭 |
| P-31 | §5.6 | 子タスクが継承する `budget` の規定（現状は親と同じ） | 親と同じ |
| P-32 | §6 Phase 5 | 受け入れに「`taskctl replay` 差分ゼロ」と「不正な plan の 1 回リトライ」を明記（e2e は既に検証） | e2e で検証済み |
| P-33 | §5.5 | `ProviderPolicy::pick` の戻り値で「候補ゼロ」と「一時的に不可（cooldown）」を区別できるようにする | 区別せず待つ + warn |
| P-34 | §6 Phase 4〜6 | 「LLM 込みの確認は人間が行う」を「認証が使える環境ならエージェントが実行し証跡を残してよい」に緩める（今回の `/goal` 制約と整合させる） | `/goal` の制約に従い実施 |

Phase 0〜4 からの既存提案（P-1〜P-26）は状況変化なし。
