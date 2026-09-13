# PROGRESS — taskd

現在地: **Phase 1 完了（2026-09-13）**。次は Phase 2（taskctl と replay）。

| Phase | 内容 | 状態 | 完了日 |
|---|---|---|---|
| 0 | 調査と ADR（実装なし） | 完了 | 2026-09-13 |
| 1 | task-core（状態機械・SqliteStore） | 完了 | 2026-09-13 |
| 2 | taskctl と replay | 未着手 | |
| 3 | fake ワーカーとディスパッチャ | 未着手 | |
| 4 | claude-code アダプタとドッグフーディング | 未着手 | |
| 5 | Planner / Reviewer（LLM） | 未着手 | |
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
