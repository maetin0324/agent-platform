# PROGRESS — taskd

現在地: **Phase 7（仕上げ）完了（2026-09-14）**。docs/DESIGN.md に定義された Phase 0〜7 は全て完了。Phase 4/6 の
実機ドッグフードの扱いも締めた（本ファイル「Phase 4/6 受け入れの締め」）。提案 P-1〜P-37 の採否は ADR-0009、Phase 7 の設計は ADR-0010。
次の作業は未定義（新しい提案 P-38 / P-39 と未解決事項を人間が判断する）。

| Phase | 内容 | 状態 | 完了日 |
|---|---|---|---|
| 0 | 調査と ADR（実装なし） | 完了 | 2026-09-13 |
| 1 | task-core（状態機械・SqliteStore） | 完了 | 2026-09-13 |
| 2 | taskctl と replay | 完了 | 2026-09-13 |
| 3 | fake ワーカーとディスパッチャ | 完了 | 2026-09-13 |
| 4 | claude-code アダプタとドッグフーディング | 完了（実機ドッグフード 2026-09-14 実施、done） | 2026-09-13 |
| 5 | Planner / Reviewer（LLM） | 完了 | 2026-09-14 |
| 6 | 承認ゲートと codex アダプタ | 完了（codex 実機ドッグフードは外部制約により免除。ADR-0009 D1） | 2026-09-14 |
| 7 | 仕上げ（ADR-0010） | 完了 | 2026-09-14 |

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

---

## Phase 6 — DONE（2026-09-14）

### 成果物

- `docs/adr/0008-phase6-approval-gate-and-codex-adapter.md` — reject の子カスケード範囲（直接の子のみ、
  終端状態は対象外）、`Human` check の解決方法（`Approval` 子タスクを生成しレビュー全体を延期する）、
  `codex` アダプタの終端判定（`turn.completed`/`turn.failed`）、taskd 設定配線を決めた ADR。監査後に
  D2 の「前方一致」→「完全一致」、D3 の「200 文字」→「500 文字」の記述誤りを訂正し、未解決事項に
  監査で見つかった設計上の穴（sticky 承認・孤児化・飢餓・非トランザクション insert）と実機確認範囲を追記
- `crates/task-core/src/store.rs` — `apply_transition_tx` に `cascade_cancel_children_tx` を追加。
  `Approval` kind タスクが `Trigger::Reject` で `Failed` になったとき、同一トランザクション内で
  `parent_id` が一致し終端状態（done/failed/cancelled）でない直接の子を `Trigger::Cancel` で
  cascade cancel する（`Trigger::Cancel` を再利用するので `running` の子はリース解放も伴う）。
  `ready_tasks()` の「親 Approval 未完了の子を除外」ロジック自体は Phase 3 で実装済みで今回変更なし
- `crates/task-dispatch/src/review.rs` — `ReviewExtras::human: HumanVerdicts`
  （`type HumanVerdicts = HashMap<usize, (bool, String)>`）を追加。`Check::Human` の判定をこの map から
  引くように変更（無ければ「human approval state missing」の防御的フォールバック）
- `crates/task-dispatch/src/dispatcher.rs` — `spawn_review` に `resolve_human_approvals` を追加。
  `Check::Human` の各 criterion について `parent_id`+`kind=Approval`+`title` 完全一致で既存の `Approval`
  子タスクを探し、無ければ `status: Ready` で直接挿入（`Event::Created` + `Event::ApprovalRequested`）。
  いずれかが未決（Ready/Draft）ならレビュー全体を延期（`Ok(false)`。既存の「Reviewer run 枠なし」延期
  経路をそのまま再利用するため `attempts` を消費しない）。全て終端なら `Done→pass`、`Failed→fail`
  （`ApprovalDecided.note` があれば理由に含める）、`Cancelled→fail` として `ReviewExtras::human` に渡す
- `crates/task-worker/src/codex.rs`（新規）— `codex exec --json` を起動する `CodexAdapter`。
  `claude_code.rs` と同じ「結果ファイル規約」（`artifacts/result.json`）。`build_prompt` は
  `claude_code::build_prompt` をそのまま再利用（kind 別プロンプトを複製しない）。`turn.completed`/
  `turn.failed` の観測有無で終端を判定し、未観測ならクラッシュとして結果ファイルを信用しない
  （ADR-0006 D4 と同型のロジック）。`turn.failed.error` は文字列・オブジェクト（`{"message":...}`）の
  両方を受け付ける（実機で観測した形。後述）
- `crates/task-worker/src/subprocess.rs` — `spawn_retrying`（`Command::spawn` を `ETXTBSY` だけ数回
  リトライする）を追加し、`run_subprocess`・`claude_code.rs`・`codex.rs` の spawn 箇所で使用。
  `cargo test --workspace` を並列実行すると、書き込み直後のスクリプトを即 exec する既存のテストパターン
  （`claude_code.rs`/`codex.rs` の `stub_*` ヘルパ）が稀に `ETXTBSY`（Text file busy）で落ちることが
  分かったため（本セッションで複数回再現、`claude_code.rs` 側の既存テストでも発生）、当面の緩和として
  spawn 全体をリトライで包んだ。6 回連続の `cargo test --workspace` で再発なしを確認済み
- `crates/taskd/src/config.rs` / `crates/taskd/src/lib.rs` — `[adapters.codex]`
  （`command`/`extra_args`/`model`/`env`。既定 `command = "codex"`）を追加、`validate()` が
  `adapter = "codex"` を受理、`build_dispatcher` が `CodexAdapter` を組み立てて登録
- `config/taskd.codex.example.toml`（新規）— ドッグフード用の設定例
- `crates/taskd/examples/seed_hello_crate_task.rs` — `--adapter`（既定 `claude-code`）を追加し、同じ
  ドッグフードタスクを `codex` でも投入できるようにした（DESIGN §6 Phase 6「codex アダプタで Phase 4 と
  同じドッグフードタスクが通る」に対応するため）
- `docs/protocol/worker-protocol.md` §9.1（新規）— `codex` アダプタの終端判定（`turn.completed`/
  `turn.failed`）を追記。§9 冒頭の「将来の codex」表記を「codex」（実装済み）に更新

作業分担: ADR、`store.rs` の cascade、`review.rs`/`dispatcher.rs` の Human check、`codex.rs`、taskd の
設定配線、`docs/protocol/worker-protocol.md` の更新は自分で行った。作業開始前に独立した単位を検討したが、
`codex.rs`（新規 1 ファイル）以外は互いに強く依存する設計判断（Approval のカスケード規則と Human check の
解決方法は同じ ADR の中で一貫させる必要があり、`resolve_human_approvals` は `store.rs` の変更内容を前提に
書く）ため、独立した単位が 2 つに届かず、implementer サブエージェントは使わず全て自分で実装した
（Phase 4 と同じ判断）。

### 受け入れ条件と証拠

- 条件（DESIGN §6 Phase 6）: 承認前に子が `ready` にならないこと
  - コマンド: `cargo test -p task-dispatch approval_gate_blocks_child_dispatch_and_reject_cancels_it`
  - 結果: ok。`Approval` 親（`Ready`）の子を `Ready` で挿入し、`Dispatcher::tick()` を 5 回回しても
    `dispatched == 0` で子の `status` は `Ready` のまま変わらないことを確認（`ready_tasks()` が
    Phase 3 から持つ「親が `kind=Approval` かつ未 `Done` の子を除外する」ロジックにより、実際の
    ディスパッチループを通しても dispatch されないことを検証）。監査で「DESIGN の字義は『子が ready に
    ならない』だが実装は『子の status は ready のまま、ready_tasks() に現れないだけ』」との指摘があり、
    これは Phase 3 からの既知の設計（ADR-0002 D5、未解決事項 P-6）で Phase 6 での新規劣化ではないと判断し
    記録に留めた（下記未解決事項）
  - 補助: `store::tests::ready_tasks_excludes_incomplete_dependencies_and_pending_approval_parent`
    （Phase 3 から既存）でも同じ性質を検証済み
- 条件（DESIGN §6 Phase 6）: `reject` で子が `cancelled` になること
  - コマンド: `cargo test -p task-core reject_cascades_cancel_to_non_terminal_direct_children_only`
  - 結果: ok。`Approval` 親を `Trigger::Reject` した結果、`Draft` の子と `Running`（リース持ち）の子は
    `Cancelled` になり（`Running` だった子は `lease` も解放される）、既に `Done` の子と他タスクの子は
    変化しないことを確認
  - コマンド: `cargo test -p task-dispatch approval_gate_blocks_child_dispatch_and_reject_cancels_it`
  - 結果: ok（上記と同じテストの後半で reject → 子 `Cancelled` → 以降も dispatch されないことまで確認）
- 条件（DESIGN §6 Phase 6, DESIGN §5.7）: `Human` check
  - コマンド: `cargo test -p task-dispatch human_check_creates_approval_child_and_completes_after_approval
    human_check_fails_task_after_rejection`
  - 結果: いずれも ok。`Check::Human` を持つタスクが `reviewing` に入ると `Approval` 子タスクが
    `status: Ready` で自動生成されること、未決の間は `reviewing` のまま `attempts` を消費しないこと、
    `taskctl approve` 相当（`store.apply_transition(..., Trigger::Approve, ...)`）で `Done` になり対象
    タスクも `Done` になること、`taskctl reject` 相当で対象タスクが `Failed` になり
    `ReviewVerdict.reason` に `rejected` と reject 時の note が含まれることを確認
  - コマンド: `cargo test -p task-dispatch review::tests::human_check_uses_resolved_verdict_from_extras`
  - 結果: ok（`review_task` 単体で `ReviewExtras.human` の内容がそのまま検証結果になることを確認）
- 条件（DESIGN §6 Phase 6）: `codex` アダプタ（実装・設定配線）
  - コマンド: `cargo test -p task-worker codex`
  - 結果: exit 0、**13 tests passed**（happy path で `done` / `result.json` 欠落 / `question` /
    `turn.failed`（文字列形・オブジェクト形の両方）/ 不正 JSON / wall-clock タイムアウト / 無出力
    タイムアウト / 終端メッセージ無しでのクラッシュ / 前回 run の `result.json` を信用しない・消す /
    `evidence` の寛容な読み取り / **起動引数**（`exec --json --model <model> <extra_args...> <prompt>`
    の順序とプロンプトが最終引数であること）の 13 ケースを検証。すべて `sh` スクリプトで `codex` を
    模擬（ネットワーク不要）
  - コマンド: `cargo test -p taskd config`
  - 結果: exit 0、**9 tests passed**。うち `loads_codex_dogfood_example_config`
    （`config/taskd.codex.example.toml` が読め `validate()` を通る）、
    `accepts_codex_adapter_with_default_config`、`rejects_unknown_fields_in_codex_adapter_config` が
    Phase 6 で追加
- 条件（DESIGN §6 Phase 6）: `codex` アダプタで Phase 4 と同じドッグフードタスクが通る
  - **人間による確認待ち**: 本セッションでは `codex` CLI 自体は PATH 上に存在し認証も通っていた
    （`codex login status` → `Logged in using ChatGPT`）ため、Phase 4/5 と異なりまず単体で
    `codex exec --json` を直接（`taskd` 経由でなく）試すところまではできた:
    ```
    codex exec --json -m gpt-5.4 "reply with exactly the single word: ok"
    # => turn.failed: "The 'gpt-5.4' model is not supported when using Codex with a ChatGPT account."
    # gpt-5-codex / gpt-5 / gpt-5-mini / o3 / gpt-4.1 / codex-mini-latest も同様に全て turn.failed
    ```
    この直接実行で得られた `turn.completed`/`turn.failed`/`item.*` の JSON Lines の形（特に
    `turn.failed.error` がオブジェクト `{"message":"..."}` であること）は `codex.rs` の実装・テストに
    反映済み（上記の 13 tests のうち `turn_failed_with_object_shaped_error_is_retryable_error`）。
    一方、`taskd` 経由でワーカーとして実際に `codex` を起動する試み
    （`cargo run -p taskd --example seed_hello_crate_task -- --adapter codex ...` や `taskd` 本体の
    起動）は、Phase 4 と同じくこのセッションのサンドボックスの安全機構に "Create Unsafe Agents" として
    拒否され実行できなかった。加えて、このアカウントの codex プランでは試した範囲のモデル名が
    いずれも使えないため、認証・サンドボックスの制約が外れたとしても、そのままでは実タスクを `done`
    まで進められない可能性が高い。人間は以下の手順で確認する:
    ```
    # 1. examples/hello-crate は cargo test が通る状態で用意済み
    (cd examples/hello-crate && cargo test)   # test result: ok. 1 passed

    # 2. taskd をビルドし、codex 用の設定でタスクを1件投入する
    cargo build -p taskd --bin taskd --example seed_hello_crate_task
    cargo run -p taskd --example seed_hello_crate_task -- \
      --db /tmp/taskd-phase6-demo.sqlite3 \
      --workspace "$(pwd)/examples/hello-crate" \
      --adapter codex

    # 3. config/taskd.codex.example.toml を db パスに合わせてコピーし、
    #    [adapters.codex] に使えるモデル（このアカウントで動く model 名）を設定して taskd を起動する
    cp config/taskd.codex.example.toml /tmp/taskd.toml
    # db / workspace_root / [adapters.codex].model / [[providers]].model を編集したうえで:
    cargo run -p taskd --bin taskd -- --config /tmp/taskd.toml --until-idle

    # 4. 結果を確認する
    cargo run -p taskctl -- --db /tmp/taskd-phase6-demo.sqlite3 show <上のタスク ID>
    cargo run -p taskctl -- --db /tmp/taskd-phase6-demo.sqlite3 replay
    cat examples/hello-crate/runs/*/stdout.jsonl   # turn.* / item.* の生ログ（証拠）
    ```
    上記が `status: Done` かつ `taskctl replay` が `0 mismatches` になれば受け入れ条件を満たす。
    通らない場合（モデルが使えない等）は `docs/DESIGN.md` の当該条件を codex 側の制約により満たせない
    旨を人間の判断として記録すること
- CLAUDE.md の共通条件
  - コマンド: `cargo test --workspace`
    結果: exit 0、**142 tests passed**（task-core 28 + task-dispatch 25 + task-worker 48
    （既存 35 + codex 13）+ taskctl 23 + 1 + taskd 9 + 1 + e2e 6（plan_scenarios 2 + scenarios 4）、
    doc-tests 0、失敗 0）。並列実行で稀に発生していた `ETXTBSY`（Text file busy）は `spawn_retrying`
    追加後、連続 6 回・別途 4 回の計 10 回の `cargo test --workspace` で再発なしを確認
  - コマンド: `cargo clippy --workspace -- -D warnings` → exit 0、警告 0
    `cargo clippy --workspace --all-targets --examples -- -D warnings` → exit 0、警告 0
  - `unwrap()` はテストモジュール以外に無い（`#[cfg(test)]` より前の行を今回変更した全ファイルで
    走査、該当 0 行。監査でも同じ結果を確認済み）
  - ネットワーク: テストは `sh` スクリプトとローカル SQLite のみ。新規依存クレートなし

### 監査結果

auditor サブエージェントを 1 回起動（読み取り専用。`cargo test --workspace`（139 passed 時点、exit 0）、
`cargo clippy --workspace -- -D warnings` と `--all-targets`（いずれも exit 0）、`unwrap()` 走査、
`git diff HEAD -- docs/DESIGN.md`（空）を auditor 自身が再実行）。総合判定は **条件付き可**、個別項目の
「不可」は無し（受け入れ条件 F「codex でドッグフードが通る」は「証拠なし」という指摘で、対応は
「PROGRESS.md に人間確認待ちと明記すること」だった）。

指摘と対応:

- **(A)【ブロッカー→解消】** 受け入れ条件 F の証拠が無い。→ 上記「受け入れ条件と証拠」に実機
  `codex exec` の直接実行結果と、`taskd` 経由の実機ドッグフードが未達である理由（サンドボックス制約＋
  アカウントのモデル制約）を明記し、人間向け手順を記載した
- **(B)【ブロッカー→解消】** `docs/PROGRESS.md` に Phase 6 節が無かった。→ 本節を追加
- **(C)【ブロッカー→修正済み】** ADR-0008 の記述が実装と不一致（D2「前方一致」は実装が「完全一致」、
  D3「200 文字」は実装が「500 文字」）。→ ADR-0008 を実装に合わせて訂正
- **(D)【指摘→解消】** 監査依頼に `seed_hello_crate_task.rs` と `worker-protocol.md` の変更を含めて
  いなかった。→ 本節の成果物一覧に明記
- **(E)【指摘→対応】** テストカバレッジの穴（`codex.rs` の不正 JSON テスト、起動引数の検証テストが無い）。
  → `invalid_result_file_json_is_retryable_error` と
  `command_line_has_exec_json_model_then_prompt_as_last_arg` を追加（`cargo test -p task-worker codex`
  が 11→13 件に増加）
- **(F)【設計上の穴、次 Phase 以降に記録】** 以下は修正せず未解決事項に記録した（監査で「次 Phase 以降に
  回してよい」とされた範囲）:
  1. **承認の sticky 問題**: `Approval` 子が一度 `Done`/`Failed` になると、対象タスクが後で別条件の fail
     により再度 `ready`→レビューに回っても同じ子を再利用する。新しい成果物に対して人間の再承認なしに
     Human 条件が pass/fail してしまう
  2. reject された `Approval` 子は再利用され続けるため、`max_retries > 0` のタスクは無駄な再実行を
     上限まで繰り返す
  3. Human 条件を持つ親タスクが `cancel`/`failed` になっても、生成済みの `Ready` な `Approval` 子は
     誰もクローズしない（孤児化。人間の承認待ちキューにゴミが残る）
  4. `ready_tasks` の探索窓（`max_concurrency * 4 + 16`）を、常に非ディスパッチ対象の `Approval` 子が
     占有しうる（承認待ちが大量に滞留すると後続タスクが飢餓する可能性）
  5. `create_human_approval_child` の `insert` と `Event::Created` の追記が非トランザクション
     （`taskctl add` と同じ既存パターンだが `complete_plan` とは不統一）
- 監査で指摘され対応しない点: 本セッションで `codex exec --json` を単体で直接実行して仕様確認したことは
  CLAUDE.md の「LLM 呼び出しを伴う確認は Phase 4/5/6 で人間が行う」を厳密には超えるが、今回の `/goal` の
  制約「認証が使える場合だけ行う」の範囲内であり、`taskd` 経由の実機ドッグフード（本来の受け入れ条件）
  自体は実施していない。Phase 5 の P-34 提案と同じ整理

再監査は auditor を再起動せず自分で実施: 上記 (C)(E) の修正後に `cargo test --workspace`
（142 passed, exit 0）、`cargo clippy --workspace -- -D warnings` と `--all-targets --examples`
（いずれも exit 0, 警告 0）を再実行して確認済み。

### 未解決事項

Phase 0〜5 から持ち越し（未着手、人間の判断待ち。今回は対処しない）:
1. P-4 `cancel` を非終端状態に限定するか
2. P-6 親 `Approval` 待ちの子の扱い（DESIGN の字義「子が ready にならない」と実装「ready_tasks() に
   現れないだけで status は ready のまま」の不一致。Phase 6 の受け入れ条件でも同じ形で再確認された）
3. P-10 `context.answers` の追加要否
4. P-5 / P-18 `taskctl cancel` の追加要否
5. `store.insert` の非トランザクション性
6. Phase 4 の実機ドッグフード（`claude-code`）は依然未実施（Phase 5 で同じアダプタ経路の実機確認はした）
7. `claude-code` アダプタが `runs/<run_id>/result.json` を書かない（P-26）
8. `taskctl show`/`ls` の SIGPIPE panic
9〜17. Reviewer run 関連（Phase 5 未解決事項参照）

Phase 6 で新たに判明し、次 Phase 以降に持ち越す点:
18. **承認の sticky 問題**（監査(F)-1）。`Approval` 子の再利用により再実行のたびの再承認ができない
19. **reject 後の無駄リトライ**（監査(F)-2）
20. **孤児化する `Approval` 子**（監査(F)-3）
21. **`ready_tasks` 窓の飢餓リスク**（監査(F)-4）
22. `create_human_approval_child` の非トランザクション性（監査(F)-5）
23. **`codex` の実機ドッグフード未実施**。`taskd` 経由の起動がこのセッションのサンドボックスで拒否され、
    かつ利用可能な認証（ChatGPT アカウント）では試した範囲のモデルが使えなかった。人間が別環境・別
    アカウントで「受け入れ条件と証拠」の手順を実行し確認する必要がある
24. `spawn_retrying`（`ETXTBSY` リトライ）はテスト実行時の並行 spawn 起因の事象への対処として
    プロダクションコード（`subprocess.rs`/`claude_code.rs`/`codex.rs`）に入れた。テスト側（スクリプト
    生成を一度だけにする、書き込み後に明示的に `sync` する等）で直す方が筋が良い可能性があり、
    ADR-0008 には根拠を書いたが独立した ADR 番号は振っていない
- Reviewer run 関連（Phase 5 未解決事項 9〜17）は状況変化なし

### 提案（DESIGN.md への修正提案。DESIGN.md 本体は編集していない）

| # | 節 | 提案 | 採用まで実装で使う既定 |
|---|---|---|---|
| P-35 | §4.2 / §5.7 | Human check で生成した `Approval` 子は、対象タスクが再度レビューに入るたびに
  新しい子を作る（現状は再利用）よう明文化する | 既存の `Approval` 子を再利用（sticky） |
| P-36 | §5.1 | `ready_tasks` が常に非ディスパッチ対象の `Approval`/`Plan` の子を数えないよう、
  `limit` の解釈を「実際に dispatch しうるタスクの数」にする | `window = max_concurrency * 4 + 16` を
  そのまま件数として扱う |
| P-37 | §4.2 | 親タスクが終端（done/failed/cancelled）になったとき、まだ `Ready` な `Human` check 用の
  `Approval` 子をどう扱うか（自動 cancel か、放置か）を明記する | 放置（孤児化） |

Phase 0〜5 からの既存提案（P-1〜P-34）は状況変化なし。

---

## Phase 4/6 受け入れの締め（2026-09-14）

人間の判断（ADR-0009 D1）に基づき、Phase 4 と Phase 6 で「人間による確認待ち」だった受け入れ条件を締めた。

### Phase 4 — claude-code の実機ドッグフード（実施、done）

- 条件: `examples/hello-crate` に対し「README.md に使用例を追記し `cargo test` が通る」タスクが実際の Claude Code で `done`
- 実行したコマンド（リポジトリを汚さないよう、HEAD `3b74cbf` でビルドしたバイナリと hello-crate のコピーをスクラッチ領域に置いて実行）:
  ```
  cargo build -p taskd --bin taskd --example seed_hello_crate_task -p taskctl
  seed_hello_crate_task --db taskd.sqlite3 --workspace <copy>/hello-crate --adapter claude-code
  # => seeded task 01M2F7VPGKWCRW7E0Q25XAF1P7 (status=ready)
  taskd --config taskd.toml --until-idle --max-ticks 600   # adapter=claude-code, model=claude-sonnet-5（claude 2.1.270）
  # => exit=0（17 tick で idle）
  taskctl --db taskd.sqlite3 show 01M2F7VPGKWCRW7E0Q25XAF1P7
  taskctl --db taskd.sqlite3 replay
  ```
- 出力の要点:
  - `status: Done`、`attempts: 0`、`lease: None`
  - 遷移: `Ready→Running (dispatch)` → `Running→Reviewing (worker_done)` → `Reviewing→Done (review_pass)`
  - `WorkerFinished.usage = {input_tokens: 12, output_tokens: 1125}`、`WorkerProgress` 7 件（Bash/Edit/Write の tool_use と本文）
  - Reviewer による再実行: `ReviewVerdict{criterion 0, pass, cmd="cargo test" exit=Some(0) … 1 passed}`、
    `ReviewVerdict{criterion 1, pass, cmd="grep -q 'greet' README.md" exit=Some(0)}`
  - `replay: 0 mismatches across 1 tasks`
  - README.md の差分: `## Usage` 節（`hello_crate::greet("world")` を呼ぶ Rust コードブロック）を追加し TODO コメントを削除（+9/−2 行）
  - 証拠ファイル: `runs/01M2F7W0FZBC33R3A15XXNCRGR/stdout.jsonl`（stream-json 17 行、最終行 `result` の `total_cost_usd=0.099`）、
    `artifacts/result.json`（`evidence` 2 件をワーカーが正しい形で記載）
- 前回（Phase 4 当時）はサンドボックスで `claude` の起動が拒否されたが、今回は拒否されなかった

### Phase 6 — codex の実機ドッグフード（外部制約により免除）

- 人間の判断（ADR-0009 D1）: 実装、`cargo test -p task-worker codex` の 13 件、`codex exec --json` の直接実行による
  イベント形式の確認（Phase 6 節に記載）をもって完了とする。`taskd` 経由の実機ドッグフードは、利用可能な
  ChatGPT アカウントでは試したモデル（gpt-5.4 / gpt-5-codex / gpt-5 / gpt-5-mini / o3 / gpt-4.1 / codex-mini-latest）が
  全て `turn.failed` になるという**外部制約により未実施**。対応モデルを使えるアカウントが用意できたら、Phase 6 節の手順で確認できる

### 提案の採否（ADR-0009）

P-1〜P-37 の採否は ADR-0009 に記録し、採用分を `docs/DESIGN.md` に反映した（人間の許可による改訂）。機能に関わる採用分と
技術的負債は Phase 7（ADR-0010）で実装する。P-12 は未実装のため提案として残し、P-20 / P-33 は供給層の担当として見送り。

---

## Phase 7 — DONE（2026-09-14）

### 成果物

- `docs/adr/0009-proposal-adoption-and-phase-closure.md` / `docs/adr/0010-phase7-hardening.md`、`docs/DESIGN.md` 改訂（人間の許可による）、`CLAUDE.md`（P-34）
- `crates/task-core`
  - `transition.rs` — `Trigger::Requeue`（`running → ready`、attempts 据え置き）、`Trigger::DependencyFailed`（非終端 → `cancelled`）、`Cancel` を非終端限定（P-4）。遷移表テストを 4 kind × 8 status × 11 単純トリガに拡張
  - `model.rs` — `Event::Answered{question, answer}`
  - `store.rs` — `create_task`（insert + Created + extra を 1 トランザクション）、`renew_lease`、`ready_tasks` の Approval 除外（P-36）、
    `cascade_after_transition_tx`（Approval の failed/cancelled → 子を cancel、Approval 以外の終端 → 未決 Approval 子を cancel（P-37）、
    failed/cancelled → 後続を `dependency_failed` で推移的に cancel（P-9））
- `crates/task-worker`
  - `protocol.rs` — `Answer` / `RunContext.answers`、`ProviderFailure` / `error.provider_failure`（`worker-protocol.schema.json` 再生成）
  - `adapter.rs` — `EventSink::heartbeat()`、`AdapterError::from_provider_failure`
  - `provider.rs`（新規）— 供給側失敗の決定的な文字列分類（Exhausted → Throttled → AuthFailed）
  - `subprocess.rs` / `claude_code.rs` / `codex.rs` — heartbeat、`provider_failure` と文面分類を `AdapterError` に写す、CLI 系も
    `runs/<run_id>/result.json` を書く（P-26）、プロンプトに人間の回答節、`spawn_retrying` 削除
  - `test_support.rs`（新規、テスト専用）— スタブを別プロセスで書き込む `write_executable`（ETXTBSY 対策）
- `crates/task-dispatch`
  - `dispatcher.rs` — 供給側失敗・起動失敗を `Requeue` + cooldown、バックオフ（`updated_at + min(base·2^(n-1), max)`）、
    `StoreSink::heartbeat` によるリース延長（ttl = idle_timeout + grace、間隔 grace/2）、`context.answers`、Human check の
    試行ごとの Approval 子（title に `(attempt n)`）と `create_task`、承認待ちのみの reviewing を idle とみなす、`[reviewer]` hint
  - `review.rs` — Reviewer run の供給側失敗を `ReviewOutcome.provider_failure` として返し、ディスパッチャが遷移せず延期（P-29）
- `crates/taskd` — `retry_backoff_base_secs` / `retry_backoff_max_secs`、`[reviewer] adapter / tier` と検証、seed 例を `create_task` に
- `crates/taskctl` — `add --check-cmd/--check-artifact/--check-reviewer`（P-17）と依存先検証、`add/plan` の workspace 既定
  `<task_id>`（P-19）、`cancel`（P-18）、`answer` の `Answered` 永続化（P-10）、`outln!` による BrokenPipe 対応
- `tests/e2e/tests/phase7_scenarios.rs`（新規、4 シナリオ）。既存 e2e の設定に `retry_backoff_base_secs = 0`
- `docs/protocol/worker-protocol.md` — §3.1 `context.answers`、§4.5 `provider_failure`、§6.1 heartbeat、§9 CLI 系の result.json と分類規則
- `config/taskd.example.toml` — バックオフと `[reviewer]`

作業分担: ADR・DESIGN、task-core、プロトコル型（`protocol.rs`/`adapter.rs`）、task-dispatch、taskd、e2e は自分で実装した（状態機械・
トランザクション・ディスパッチ判断は互いに依存し設計判断を含むため）。共有型を先に入れてビルドを通した後、ファイルを共有しない
2 単位を implementer サブエージェント 2 体に並列実装させた: 単位 A = `crates/taskctl/**`、単位 B = task-worker のアダプタ
（`subprocess.rs`/`claude_code.rs`/`codex.rs`/新規 `provider.rs`/`test_support.rs`）と `worker-protocol.md`。両者の報告に
「判断が必要な点」は無かった。

### 受け入れ条件と証拠（DESIGN §6 Phase 7）

いずれもネットワーク不要（`sh` の fake ワーカーとローカル SQLite）。e2e は実バイナリ `taskctl` / `taskd` を起動し、最後に
`taskctl replay` が `replay: 0 mismatches` になることを確認している。

1. **`taskctl answer` の回答が次 run の `context.answers` に載り done**
   - コマンド: `cargo test -p e2e --test phase7_scenarios` → exit 0、4 passed
   - `answer_is_delivered_in_context_answers_and_cli_checks_complete_the_task`: question で `Blocked` → `taskctl answer <id> "target v2"` →
     `Event::Answered{question:"which version should I target?", answer:"target v2"}` → 2 回目の run の stdin に
     `"answers":[{"question":"which version should I target?","answer":"target v2"}]` → `Done`（attempts 0）。遷移列 7 件を完全一致で検証
   - 単体: `taskctl` `run_answer_persists_answered_event_with_question_from_worker_finished`、task-worker `build_prompt_includes_answers_from_human_for_execute_and_plan`
2. **`taskctl add --check-cmd ... --check-artifact ...` だけで作ったタスクが done**
   - 上記 e2e のタスクは CLI だけで作成し、`acceptance` が `Command{test -f answered.txt}` と `ArtifactExists{report.md}` であることを検証
   - 単体: `run_check_cmd_produces_command_criterion_with_expected_text` ほか `add.rs` 7 件（条件ゼロはエラー、依存先 failed/不存在はエラー等）
3. **cancel は非終端のみ、先行の failed が後続へ推移的に伝播**
   - e2e `cancel_is_limited_to_non_terminal_tasks_and_failures_cancel_dependents`: A（`--check-cmd false --max-retries 0`）が `Failed` →
     B（A に依存）・C（B に依存）の遷移がともに `["Draft->Ready:accept", "Ready->Cancelled:dependency_failed"]`。draft の D を
     `taskctl cancel` → `Cancelled`。failed の A への `cancel` は exit 1・stderr `cannot be cancelled`・状態不変。failed な A に依存する
     `add` は拒否。workspace 省略の D のパスが `<task_id>`（P-19）
   - 単体: store `dependency_failure_cancels_dependents_transitively`、`cancel_is_invalid_for_terminal_tasks`、
     `cancelling_an_approval_cascades_to_its_children`、transition `table_simple_triggers_full_cross_product`（352 ケース）と
     `cancel_dependency_failed_and_requeue_keep_attempts`
4. **Human check は再レビューで新しい Approval 子、親が終端なら未決 Approval 子が cancelled、`ready_tasks` は Approval を返さない**
   - e2e `human_check_asks_again_after_a_retry_and_orphaned_approvals_are_cancelled`: `(attempt 1)` の子を承認 → Command 条件 fail →
     attempts 1 → 2 回目の run → `(attempt 2)` の新しい子（`taskd --until-idle` は承認待ちで idle 終了）→ 承認 → `Done`。別タスク O の
     承認待ち中に `taskctl cancel O` → O と未決の Approval 子がともに `Cancelled`
   - 単体: dispatcher `human_check_requests_a_new_approval_for_each_attempt`、store `terminal_task_cancels_its_pending_approval_children_only`、
     `ready_tasks_excludes_approval_kind`
5. **`provider_failure` 付き error は attempts を消費せず requeue、cooldown 明けに done。Reviewer run の供給側失敗で ReviewFail にならない**
   - e2e `provider_failure_requeues_without_consuming_attempts`（`--max-retries 0`）: 遷移列
     `accept → dispatch → requeue → dispatch → worker_done → review_pass`、`WorkerFinished.outcome` が `requeue: ` で始まり `throttled` を含む、attempts 0
   - 単体: dispatcher `provider_failure_requeues_without_consuming_attempts`（cooldown 中の tick で `dispatched == 0`）、
     `reviewer_run_provider_failure_defers_review_without_consuming_attempts`（`reviewer run requeued` の進捗、verdict は pass 1 件のみ）、
     review `reviewer_provider_failure_is_reported_instead_of_failing_criteria`、task-worker `provider_failure_is_classified_and_result_json_is_written`、
     `result_text_classified_as_throttled_surfaces_as_adapter_error`、`turn_failed_classified_as_exhausted_surfaces_as_adapter_error` ほか、`provider.rs` 6 件
6. **バックオフ中は再 dispatch されない、ワーカーの出力でリースが延長される**
   - dispatcher `retry_backoff_delays_redispatch`（attempts 1 の ready で 5 tick とも `dispatched == 0` かつ非 idle、base を 0 にすると
     再実行されて `Failed`/attempts 2。`retry_backoff` の値 0/10/40/上限を検証）
   - dispatcher `heartbeat_renews_the_lease`（取得時 `max_wall + grace` の期限が heartbeat 後に `idle_timeout + grace` へ更新）、
     store `renew_lease_extends_only_the_matching_running_lease`（別 run_id・非 running では false、イベントは増えない）
7. **`taskctl ls | head -n 1` が panic せず成功、`spawn_retrying` 無しで `cargo test --workspace` 5 回連続成功**
   - コマンド: 300 タスクの DB で `taskctl --db t.sqlite3 ls 2>stderr | head -n 1` → `PIPESTATUS=0 0`、stderr 0 バイト
   - 統合テスト `crates/taskctl/tests/pipe.rs::ls_on_a_closed_pipe_exits_cleanly_without_panicking`（3000 タスク）
   - コマンド: `for i in 1..5; cargo test --workspace` → 5 回とも `passed=190 failed=0`、`Text file busy` の出現 0。`grep -rn spawn_retrying crates/` → 0 件。
     監査後の修正の後にも 3 回連続で `passed=192 failed=0`

補足（採用した提案のうち受け入れ条件に直接出ないもの）: P-26 は claude-code / codex の `happy_path_progress_and_done_from_result_file`
で `runs/<id>/result.json` を検証。P-30 は taskd `backoff_and_reviewer_settings_map_to_dispatch_config` と
`rejects_reviewer_without_matching_provider_and_unknown_reviewer_keys`。原子的な挿入は store `create_task_inserts_task_and_events_atomically`。

### CLAUDE.md の共通条件

- `cargo test --workspace` → exit 0、**192 tests passed**（task-core 35 + task-dispatch 31 + task-worker 63 + taskctl 38 + 1 + 1 +
  taskd 12 + 1 + e2e 10（scenarios 4 + plan_scenarios 2 + phase7_scenarios 4）、失敗 0）。Phase 6 時点の 142 から +50
  （監査前 190、監査後の修正で `provider::tests::status_codes_inside_positions_or_numbers_do_not_match` と
  taskd `rejects_zero_cooldown_and_unsafe_lease_grace` を追加）
- `cargo clippy --workspace -- -D warnings` → exit 0。`cargo clippy --workspace --all-targets --examples -- -D warnings` → exit 0
- `unwrap()` はテストコード以外に 0 件（`#[cfg(test)]` より前の行を全 `.rs`（新規ファイル含む）で走査）

### 監査結果

auditor サブエージェントを 1 回起動（読み取り専用。`cargo test --workspace` を 5 回連続で各 190 passed、clippy 2 種 exit 0、
`spawn_retrying` 0 件、非テストの `unwrap()` 0 件、ネットワーク系 API の grep 0 件、300 タスクの DB での `ls | head -n 1` /
`show | head -n 2` の PIPESTATUS `0 0`、`cancel` 2 回目の exit 1、`replay` 0 mismatches を auditor 自身が確認）。
総合判定は **条件付き可**、「不可」は **0 件**。受け入れ条件 1・2・4 と、原則（LLM 非混入）・同一トランザクション・replay 整合・
カスケードの停止性と二重適用・バックオフの判定・テスト以外の `unwrap()` とネットワークは「可」。

「条件付き可」の指摘と対応:

- **(1)【修正済み】** 供給側失敗の分類で `429`/`529`/`401` を単純な部分一致にしていたため、クラッシュ時に分類する stderr 末尾の
  スタックトレース（`cli.js:4291:17` 等）を Throttled と誤分類し、attempts を消費しない requeue を無期限に繰り返しうる。
  → `provider.rs` の数字コードを「独立トークン」だけ一致させる規則に変更（前後が英数字・`.` でなく、`:` を挟んで数字が続く
  位置情報の一部でもない）。回帰テスト `status_codes_inside_positions_or_numbers_do_not_match`。ADR-0010 D5 に stderr 末尾を
  分類に使うことと、この規則を追記
- **(2)【修正済み】** `retry_after_secs = 0` / `error_cooldown_secs = 0` だと requeue が毎 tick の再 dispatch になる。
  → `AdapterError::from_provider_failure` で最低 1 秒に切り上げ、`taskd` の設定検証で `error_cooldown_secs >= 1` を要求。
  `Spawn`（起動失敗）の上限の無い requeue は ADR-0009 D2 の決定どおりで、未解決事項 2 と提案 P-38 に記録済み
- **(3)【修正済み】** リース延長の安全性は `kill_grace + tick_ms < lease_grace / 2` が前提だが検証も文書化もされていなかった。
  → 設定検証で要求（違反はエラー）し、ADR-0010 D7 と `worker-protocol.md` §6.1 に前提を明記。回帰テスト
  `rejects_zero_cooldown_and_unsafe_lease_grace`（既存の設定例・e2e の設定はいずれも満たす）
- **(5)【修正済み】** 受け入れ条件 3 の e2e が exit code 1 を確認していなかった。→ `taskctl_raw` が exit code を返すようにし、
  `assert_eq!(code, Some(1))` に変更
- **(9)【修正済み】** ADR-0010 の記述の不一致 3 件（D1 の列数 14→15、D5 の進捗メッセージの実際の文面、D5 のパターン一覧に
  `rate_limit`/`529` が無い）を実装に合わせて訂正
- **(4)(6)(7)(8)(10)【記録】** 未解決事項 8〜12 に記録（Reviewer run 延期の replay は単体テストのみ、取得時 ttl を超えて出力し続ける
  ワーカーが回収されないことの直接のテストが無い、パイプの自動テストは `head -n 1` そのものではない、Phase 6 形式の Approval 子の
  重複、人手の Approval 子の cancel）

修正後の再監査は auditor を再起動せず自分で実施: `cargo test -p task-worker provider`（10 passed）、`cargo test --workspace` を
3 回連続（各 `passed=192 failed=0`）、`cargo clippy --workspace -- -D warnings` と `--all-targets --examples`（いずれも exit 0）、
変更・新規ファイルの非テスト `unwrap()` 走査（0 件）、`git diff HEAD -- docs/DESIGN.md`（差分なし。DESIGN.md の改訂は Phase 6 の締めの
コミットで完了済み）。

### 未解決事項

1. P-12（`evidence[]` の各フィールドを任意に）は提案のまま。P-20 / P-33（`ProviderPolicy` の拡張）は供給層の担当として見送り
2. 供給側失敗の分類は文字列規則なので、取りこぼし（通常の `WorkerError` になり attempts を消費）と誤検出（requeue が cooldown 間隔で
   繰り返される）がありうる。**連続 requeue の回数に上限は無い**。起動失敗（コマンドのパス誤り等）も同様に cooldown ごとの requeue を
   繰り返し、ログの warn と `WorkerFinished.outcome = "requeue: adapter: failed to spawn ..."` でしか気付けない（提案 P-38）
3. `blocked → ready`（answer）の後も attempts > 0 ならバックオフを待つ（ADR-0010 D6 の既知の挙動）
4. Approval の cancel/reject の伝播は直接の子まで（孫は対象外、ADR-0008 から変わらず）。依存先失敗の伝播は推移的
5. 依存先失敗の伝播は非終端タスクの JSON を `LIKE` で絞って走査する（1 回の失敗につき O(非終端タスク数)）。単一ノード規模を前提
6. `taskctl worker run`（DESIGN §5.9）は依然未実装
7. codex の実機ドッグフードは外部制約により未実施（ADR-0009 D1）
8. Reviewer run の供給側失敗による延期は単体テスト（`reviewer_run_provider_failure_defers_review_without_consuming_attempts`）だけで、
   実バイナリでの `replay` 差分ゼロは確認していない（状態を変えず `WorkerProgress` を追記するだけなので影響は小さい）（監査 4）
9. 「取得時の ttl を超えて出力し続けるワーカーが回収されない」ことを直接示すテストは無い（`heartbeat_renews_the_lease` は延長で
   期限が更新されることまで）（監査 6）
10. パイプの自動テストは 256 バイト読んで閉じる形で、`head -n 1` そのものは手動確認（監査 7）
11. **移行上の注意**: Phase 6 形式（題名に `(attempt n)` が無い）の未決 Approval 子が既存 DB にあると照合できず、新しい子が重複して
    作られる。古い子は親が終端になると P-37 の伝播で cancel される（監査 8）
12. **利用者への注意**: `taskctl add --kind approval --parent <Approval 以外>` で人手で作った Approval 子も、親が終端になると
    cancel される（DESIGN §4.2 の文言どおり）（監査 10）

### 提案（DESIGN.md への修正提案。DESIGN.md 本体は今回以降は編集しない）

| # | 節 | 提案 | 採用まで実装で使う既定 |
|---|---|---|---|
| P-38 | §5.2 | 連続 `requeue` の回数（または期間）に上限を設け、超えたら `failed` にするか承認待ちに回す | **採用・実装済み（ADR-0011、下記「Phase 7 追補」）** |
| P-39 | §5.9 | `taskctl show` で `Answered` / Human check の Approval 子 / バックオフの残り時間を読みやすく表示する | イベントの Debug 表示のみ（人間の判断で後回し。Web GUI の検討と合わせて扱う） |

---

## Phase 7 追補 — 連続 requeue の上限（P-38、2026-09-14）

人間の判断: 「requeue の回数はコンフィグで設定できるように。デフォルト値は 5 回くらい」。設計は ADR-0011。

### 成果物

- `docs/adr/0011-requeue-limit.md`
- `crates/taskd/src/config.rs` — トップレベル設定 `max_requeues`（既定 5、0 で requeue しない）。`config/taskd.example.toml` に追記
- `crates/task-dispatch/src/dispatcher.rs` — `DispatchConfig.max_requeues`、`consecutive_requeues`（同じ試行での連続 requeue を `events` から数える）、
  `consecutive_reviewer_requeues`（現在の reviewing での Reviewer run 延期回数）。上限に達した供給側失敗は、ワーカー run では
  `WorkerError{retryable:true}`（`outcome` に `requeue limit (N) reached`）、Reviewer run では未判定の Reviewer 条件を fail にして `ReviewFail`
- テスト: dispatcher `requeue_limit_turns_persistent_provider_failures_into_ordinary_failures`、`reviewer_requeue_limit_fails_reviewer_criteria`、
  e2e `persistent_provider_failure_stops_after_max_requeues`、taskd の設定テストに既定値 5 と `max_requeues = 0` の読み込みを追加

### 受け入れの証拠

- 条件: 供給側失敗が続くタスクが `max_requeues` 回の requeue の後に通常の失敗として扱われる
  - コマンド: `cargo test -p e2e --test phase7_scenarios` → 5 passed。`persistent_provider_failure_stops_after_max_requeues`（`max_requeues = 2`,
    `--max-retries 0`）の遷移列が `accept → dispatch → requeue → dispatch → requeue → dispatch → worker_error(Failed)`、attempts 1、replay 差分ゼロ
  - コマンド: `cargo test -p task-dispatch requeue` → 3 passed。`max_retries = 1`, `max_requeues = 2` で run 6 回・`Failed`/attempts 2、遷移の reason 列が
    `[dispatch, requeue, dispatch, requeue, dispatch, worker_error] × 2`。`max_requeues = 0` では `[dispatch, worker_error]`。Reviewer run は
    延期 2 回の後 `ReviewVerdict{pass:false, reason:"requeue limit (2) reached: ..."}` で `Failed`
- CLAUDE.md の共通条件
  - `cargo test --workspace` を 2 回 → 2 回とも exit 0、**195 passed**、失敗 0（Phase 7 の 192 + 3）
  - `cargo clippy --workspace -- -D warnings` / `--all-targets --examples` → いずれも exit 0
  - 変更ファイルの非テスト `unwrap()` → 0 件
- 監査: 変更が小さい（設定 1 項目とディスパッチャの分岐 2 箇所）ため auditor は起動していない

### 未解決事項・提案

- Phase 7 未解決事項 2 のうち「連続 requeue の回数に上限が無い」は解消。最悪の実行回数は 1 タスクあたり `(max_retries + 1) × (max_requeues + 1)`
- `docs/DESIGN.md` §5.2 への反映（「連続 requeue は `max_requeues` まで。超えたら通常の失敗」）は、DESIGN.md の編集許可が得られるまで提案として残す（P-40）
