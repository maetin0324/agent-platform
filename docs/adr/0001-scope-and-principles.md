# ADR-0001: スコープと設計原則の確定

- 日付: 2026-09-13
- 状態: Accepted（Phase 0）
- 関連: `docs/DESIGN.md` §0–§3, §7 / [ADR-0002](0002-state-machine.md) / [ADR-0003](0003-worker-protocol.md)

## 文脈

`taskd` は3層構成（接続層 / タスク管理層 / モデル供給層）のうちタスク管理層だけを実装する。
Phase 0 では実装を書かず、参考設計（Symphony, Bernstein, dsh, Claude Code headless）を一次情報で読み、
DESIGN.md の原則を「コードで検査できる規則」に落とす。DESIGN.md と矛盾する点は末尾に修正提案として列挙し、
人間が採否を決めるまで実装で変えない。

## 調査した一次情報と、そこから借りる設計

### OpenAI Symphony (`github.com/openai/symphony`, `SPEC.md`)

借りるもの:

- **単一権限のオーケストレータ。** 「スケジューリング状態を変更するのはオーケストレータだけ。ワーカーの結果は全て報告として戻り、明示的な状態遷移に変換される」。taskd ではディスパッチャ＋ストアがこれに当たる。ワーカーがタスク状態を直接書くことはない。
- **tick の手順。** 1) 実行中の照合（stall 検出・状態更新） 2) 候補取得 3) `priority` 昇順→作成時刻の古い順でソート 4) 空きスロット分だけ dispatch。DESIGN §5.2 の手順と同型（taskd は `priority DESC, created_at ASC`）。
- **タスク毎ワークスペース。** `<root>/<sanitized_key>`、`[^A-Za-z0-9._-]` を `_` に置換、ワークスペースは必ずルート配下（safety invariant）。taskd は key に ULID を使うので置換は不要だが「ルート配下」の検査は `artifact.path` に対して行う（ADR-0003）。
- **タイムアウトの二段構え。** `stall_timeout_ms`（無出力 5 分でオーケストレータが kill）と `turn_timeout_ms`（1 時間）。DESIGN §5.4「wall-clock 上限、無出力タイムアウト」に対応。
- **失敗リトライは指数バックオフ**（`min(10s·2^(attempt-1), cap)`）。DESIGN には retry の待ち時間規定がない。Phase 3 では「次 tick で即再投入」を既定とし、バックオフは提案に留める（後述）。
- **`attempt` をプロンプト変数として渡す。** ADR-0003 の `run` 拡張提案に反映。

借りないもの:

- Symphony は耐久 DB を持たず、再起動復旧はトラッカー（外部 issue tracker）駆動。taskd は原則 2「状態はエージェントの外（SQLite）」に従い、in-memory claim の代わりに **DB 上のリース** を使う。
- `turn_input_required` は「実装依存で失敗扱い」。taskd は `question` を一級のメッセージとして `blocked` 状態に落とす（原則 5）。

### Bernstein (`bernstein.run`, GitHub 各ミラー README)

- 「協調ループにモデルを置かない（No model in the coordination loop）」「スケジューラは決定的で、ジャーナルから byte-identical に再実行できる」。原則 1・6 の根拠。
- 状態はエージェント外の `.sdd/` ディレクトリに置き、`lineage/<run_id>/spine.jsonl` に追記専用で記録。taskd では `events` テーブルが同じ役割を担い、`taskctl replay` で `tasks` を再構築できることを Phase 2 の受け入れ条件にする（DESIGN §4.3）。

### DeepSeek Harness / dsh (`github.com/deepseek-ai/deepseek-harness`, 公式リファレンス「Sessions」「Session Persistence」「Subagent」)

- **Session = 型付き `SessionEvent` の追記専用ログが唯一の真実**で、モデルに見せるメッセージ履歴は `Session.deriveMessages()` で毎回ログから射影する。taskd の「`tasks` は `events` の派生ビュー」と同じ考え方。
- 永続化は `dsh-session-persistence-jsonl`：1 セッション 1 論理 JSONL、既定は checksum 付き Zstandard フレーム連結、設定で raw lines。クラッシュ時は「物理的に有効な連続部分」だけ返し、修復は読み手の責務。
- サブエージェントは **in-process**（`spawn` = 文脈を継がない / `fork` = 親の完了ターン接頭辞を継ぐ）。結果は `SubagentResult{output, structured?, stopReason}`、`stopReason ∈ {completed, aborted, error, max-tokens, refusal}`。`TurnEndReason` には `blocked` がある。taskd の `done` / `error` / `question` の三分法はこれと整合する。
- **ヘッドレス CLI は `dsh --profile headless "<prompt>"` で「最終回答を印字して終了」するだけ**で、stdout の JSON イベント形式は文書化されていない。構造化された進捗を取るなら `--profile sdk`（JSON-RPC）か セッション JSONL（raw lines 設定）を読む必要がある。DESIGN §5.4 の `dsh` 行「ヘッドレスCLI。同様」はこの点で不足している（修正提案 P-6）。

### Claude Code headless（ローカル `claude --version` = 2.1.270、公式 docs `headless.md`, `cli-reference.md`, `env-vars.md`, `authentication.md`）

- `claude -p "<prompt>" --output-format stream-json` は JSON Lines。`system/init`（`session_id`, `model`, `cwd`, `tools`）→ `assistant`（`message.content[]` に `text` / `tool_use`）→ `user`（`tool_result`）… → 最後に `result`（`subtype ∈ {success, error_max_turns, error_max_budget_usd, error_during_execution}`, `is_error`, `result`, `num_turns`, `duration_ms`, `total_cost_usd`, `usage{input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens}`, `session_id`）。
- `--verbose` は「推奨」であり必須ではない（docs）。`--include-partial-messages` で `stream_event` の delta が追加される（taskd には不要）。
- `--json-schema` は `--output-format json` とだけ組み合わせ可能で、`stream-json` では使えない（docs）。つまり **進捗ストリームと構造化された最終出力は同時に取れない**。ADR-0003 の「結果ファイル規約」提案の根拠。
- 終了コード: 0 成功 / 1 エラー / 2 部分失敗 / 130 SIGINT / 143 SIGTERM。
- アカウント分離: `CLAUDE_CONFIG_DIR` で `.credentials.json`・設定・キャッシュを丸ごと別ディレクトリにできる。`ANTHROPIC_API_KEY` は `-p` では無条件に使われる。`CLAUDE_CODE_OAUTH_TOKEN`（`claude setup-token`）も可。DESIGN §5.4 の「設定ディレクトリをプロバイダごとに切替」は実現可能。
- 関連フラグ: `--max-turns`, `--max-budget-usd`, `--permission-mode`, `--allowedTools/--disallowedTools`, `--tools`, `--append-system-prompt`, `--no-session-persistence`, `--bare`, `--add-dir`。
- `--input-format stream-json` の stdin スキーマは docs に具体例がない。taskd はワーカーをステートレスに使うので使用しない。

### Codex（`developers.openai.com/codex/noninteractive`）

- `codex exec --json "<prompt>"` で stdout が JSONL（`thread.started`, `turn.started`, `item.*`, `turn.completed`, `turn.failed`, `error`）。`-o/--output-last-message <path>` で最終メッセージをファイルに書ける。Phase 6 の `codex` アダプタは claude-code と同じ結果ファイル規約で吸収できる。

## 決定

### D1. スコープ

DESIGN §0 のとおり、実装するのはタスク管理層のみ。他層との境界は次の 3 つの trait に限定し、本プロジェクトでは括弧内の実装だけを持つ。

| 境界 | trait | 本プロジェクトの実装 |
|---|---|---|
| ワーカー実行 | `WorkerAdapter` | `fake`, `claude-code`, `codex`, （任意）`dsh`, `openai-compat` |
| モデル供給 | `ProviderPolicy` | `StaticPolicy` |
| 接続層 | `Workspace` | `LocalWorkspace`（`RemoteWorkspace` は型と `unimplemented!()` の骨組みのみ） |

非目標（Web UI、リモート実行、予算・残量推定、複数アカウント自動切替、マルチユーザ、通知）はコードにもフラグにも入れない。

### D2. 原則をクレート依存関係で強制する

| 原則 | 検査可能な規則 |
|---|---|
| 1. 協調判断に LLM を使わない | `task-core` と `task-dispatch` は HTTP クライアント・LLM SDK・`std::process`/`tokio::process` に依存しない。`Cargo.toml` の依存リストで確認する。プロセス起動は `task-worker` のアダプタだけ |
| 2. 状態はエージェントの外 | ワーカーへ渡すのは `run` 1 行と workspace のみ。ワーカーのセッション再開（`--resume` 等）は使わない |
| 3. ワーカーは交換可能 | 全アダプタは ADR-0003 の `WorkerMessage` に正規化し、ディスパッチャはアダプタ種別で分岐しない |
| 4. 完了はレビューが決める | `running → done` の直接遷移は状態機械に存在しない（ADR-0002） |
| 5. 承認ゲート | `Approval` kind と `Human` check 以外に人間待ちの経路を作らない |
| 6. 追記専用イベント | `events` テーブルに UPDATE/DELETE を発行しない。`SqliteStore` にそのメソッドを持たせない |
| 7. 最小から | Phase の受け入れ条件に不要な抽象は入れない。必要と思ったものは `PROGRESS.md` の「提案」へ |

### D3. 技術選定の確定（DESIGN §2 の表に加える具体クレート）

ライブラリ選定は ADR に残すというルールに従い、Phase 1–3 で使う予定のクレートをここで確定する。バージョンは Phase 1 で `Cargo.lock` に固定する。

| 用途 | クレート | 備考 |
|---|---|---|
| ID | `ulid` | `TaskId`, `run_id` |
| 時刻 | `time`（`serde` feature） | RFC3339 で保存 |
| 永続化 | `rusqlite`（`bundled`） | DESIGN §2 |
| ハッシュ | `sha2` | `ArtifactRef.sha256` |
| エラー | `thiserror` | DESIGN §7 |
| 設定 | `toml` + `serde` | `config/taskd.toml` |
| 非同期 | `tokio`（`rt-multi-thread`, `process`, `io-util`, `time`, `sync`, `signal`） | `task-worker`, `taskd` のみ |
| スキーマ | `schemars` | プロトコル JSON Schema 生成 |
| ログ | `tracing`, `tracing-subscriber`（`json`） | |
| CLI | `clap`（`derive`） | |
| テスト | `tempfile`, `assert_cmd`（dev） | 外部ネットワークに出ない |

Rust は stable（ローカル `cargo 1.94.0`）、`edition = "2024"`。

### D4. Phase の進め方

DESIGN §6 のとおり。Phase 0 は実装なしのため `cargo test --workspace` / `cargo clippy` は Cargo ワークスペースが存在せず実行できない（`PROGRESS.md` に実際の出力を記す）。Phase 1 開始時に workspace `Cargo.toml` を作る。

## 結果

- 以降の Phase は ADR-0002（状態機械）と ADR-0003（プロトコル）を仕様として実装する。
- DESIGN.md の修正が必要と判断した点は下の一覧に集約し、人間の採否を待つ。採否が出るまでは DESIGN.md の記述を実装する。

## DESIGN.md 修正提案（本 ADR のスコープ分）

採否は人間が決める。番号は `PROGRESS.md` の「提案」節と共通。

- **P-1（§6 Phase 0 受け入れ）** 「`cargo test --workspace` と `cargo clippy` が通る」は Phase 0 には適用できない（ワークスペース未作成）。Phase 0 の完了条件を「3 ADR と protocol 初版の存在」だけに限定する記述を追記する。
- **P-2（§2 技術選定）** 上記 D3 のクレート一覧を DESIGN §2 の表か脚注に追記する（DESIGN は主要 6 項目のみで、`ulid`/`sha2`/`time`/`thiserror` が未記載）。
- **P-3（§5.2 リトライ）** リトライ時の待ち時間が未規定。Symphony に倣い `min(base·2^(attempts-1), cap)` のバックオフを設定項目として追加することを提案。採用されるまで Phase 3 は「次 tick で再投入」で実装する。
