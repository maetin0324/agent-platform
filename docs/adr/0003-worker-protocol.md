# ADR-0003: ワーカープロトコル（サブプロセス + JSON Lines）

- 日付: 2026-09-13
- 状態: Accepted（Phase 0）。仕様本体は `docs/protocol/worker-protocol.md`。Phase 3 で `task-worker` に実装する
- 関連: `docs/DESIGN.md` §5.3, §5.4, §4.4 / [ADR-0001](0001-scope-and-principles.md) / [ADR-0002](0002-state-machine.md)

## 文脈

DESIGN §5.3 は 6 種のメッセージ（`run` / `progress` / `artifact` / `question` / `done` / `error`）を示すが、
終了規則、タイムアウト、パス検査、スキーマの所在、各 CLI アダプタからの写像は未規定。
Phase 0 の調査（ADR-0001）で分かった制約:

- Claude Code: `stream-json` と `--json-schema`（構造化出力）は併用不可。進捗を取りながら `done` の `evidence` を構造化して受け取る手段が stdout だけでは無い。
- Codex: `codex exec --json` は JSONL、`-o` で最終メッセージをファイルに書ける。
- dsh: `--profile headless` は最終回答テキストのみ。構造化イベントは `--profile sdk`（JSON-RPC）かセッション JSONL。
- 三者とも「最終回答が構造化 JSON である保証」は CLI から得られない。したがって **`done` の内容をどう確定するかはアダプタ共通の課題**。

## 決定

### D1. トランスポート

- オーケストレータはワーカーを **サブプロセス** として起動し、stdin に `run` メッセージを **1 行** 書いて stdin を閉じる（EOF）。
- ワーカーは stdout に **1 行 1 JSON オブジェクト**（UTF-8、`\n` 終端、改行を含まない）を書く。stderr は自由形式で、アダプタが `runs/<run_id>/stderr.log` に保存する。
- `--input-format stream-json` 等の双方向対話は使わない（原則 2: ワーカーはステートレス）。
- stdout の JSON でない行は破棄し、`tracing` に warn を出す（`WorkerProgress` にはしない）。JSON だが `type` が未知の行は `error{retryable:false}` 相当としてその run を打ち切る。
- 1 行の上限は 1 MiB。超過はプロトコル違反として run を `error{retryable:false}` にする。

### D2. メッセージ集合と型の所在

DESIGN §5.3 の 6 種をそのまま採用する。Rust 型は `task-worker` クレートに置く:

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerMessage { Progress{..}, Artifact{..}, Question{..}, Done{..}, Error{..} }   // ← ワーカー→taskd
pub struct RunRequest { protocol: u32, task: Task, workspace: PathBuf, context: RunContext }  // → ワーカー（`type: "run"`）
```

未知のフィールドは **無視**（前方互換）。必須フィールド欠落は解析エラー。`run.protocol` は `1` で固定し、非互換変更時に上げる。

### D3. 終端規則

- `done` / `error` / `question` は **終端メッセージ**。ワーカーは終端メッセージを書いたら速やかに exit する。終端後の行は捨てる。
- 終端メッセージが 1 行も無いままプロセスが exit した場合: exit code に関わらず `error{retryable:true, message:"worker exited without terminal message (exit=N)"}` として扱う（クラッシュ相当。ADR-0002 D3）。
- exit code は `WorkerFinished` に記録するが、状態遷移の判断には使わない（終端メッセージが優先）。
- `question` は終端である。ワーカーはその場で終了し、人間の回答は **次の run の `context`** に載せて渡す（DESIGN の `context` には無いフィールドのため提案 P-10）。

### D4. 生存監視と強制終了（DESIGN §5.4「必ず実装」）

| 上限 | 出所 | 超過時 |
|---|---|---|
| wall-clock | `task.budget.max_wall_secs` | kill → `error{retryable:true, message:"wall clock exceeded"}` |
| 無出力 | アダプタ設定 `idle_timeout_secs`（既定 300。Symphony の `stall_timeout_ms` に倣う） | kill → `error{retryable:true, message:"idle timeout"}` |
| cancel | ディスパッチャからの指示 | kill。結果は捨てる |

kill は SIGTERM → `kill_grace_secs`（既定 10）待って SIGKILL。子プロセスグループごと終了させる（`setsid` / process group）。

### D5. 成果物とパス

- `artifact.path` はワークスペース相対。絶対パス、`..` を含むパス、シンボリックリンク経由でワークスペース外に出るパスは拒否し、warn を出して無視する（Symphony の workspace safety invariant）。
- 受信時にアダプタが sha256 とサイズを計算して `ArtifactRef{name, path, sha256, kind}` を作り、`ArtifactProduced` を追記する。ファイルが存在しなければ無視して warn（`ArtifactExists` レビューで落ちる）。
- ワークスペース配置: `<workspace_root>/<task_id>/`。成果物は `artifacts/`（DESIGN §4.4）、run ごとの生ログは `runs/<run_id>/stdout.jsonl`, `stderr.log`。

### D6. JSON Schema の管理

- スキーマは `schemars` で Rust 型から生成し、`docs/protocol/worker-protocol.schema.json` として **コミット**する。
- `task-worker` のテストが生成結果とコミット済みファイルを比較し、差分があれば失敗する（更新手順: テストを `UPDATE_SCHEMA=1` で走らせて再生成 → コミット）。
- `docs/protocol/worker-protocol.md` 内のスキーマ抜粋は説明用。正は `.schema.json`（Phase 3 で生成するまでは md 内の手書き版を暫定の正とする）。

### D7. アダプタからの写像

| アダプタ | 起動 | progress | artifact | done / error | question | usage |
|---|---|---|---|---|---|---|
| `fake` | 設定された応答列（TOML or JSONL ファイル）をそのまま stdout に流す小さなスクリプト／同プロセス実装 | そのまま | そのまま | そのまま | そのまま | そのまま |
| `claude-code` | `claude -p <prompt> --output-format stream-json --verbose --permission-mode … --max-turns … --no-session-persistence`、`cwd = workspace`、`CLAUDE_CONFIG_DIR` をプロバイダ別に設定 | `assistant.message.content[].text` の要約行、`tool_use.name` | D8 の結果ファイル規約 | `result.subtype = success` かつ結果ファイルあり → `done`。`error_*` / `is_error` / exit≠0 → `error`（`error_max_turns` は retryable:true） | 結果ファイルに `question` が書かれていれば `question` | `result.usage.input_tokens/output_tokens` |
| `codex` | `codex exec --json -o <last.md> <prompt>` | `item.*` | 同上 | `turn.completed` → 結果ファイル確認、`turn.failed` → `error` | 同上 | `turn.completed` の usage があれば |
| `dsh`（任意） | `dsh --profile headless <prompt>`。stdout は最終回答テキストのみ | セッション JSONL（raw lines 設定）を tail するか、進捗なし | 同上 | 結果ファイル規約 | 同上 | 取れない場合は省略 |
| `openai-compat` | 自前ループ（Phase 4） | 各ツール呼び出し | 自前で `artifact` | 自前で `done` | 自前で `question` | API レスポンス |

### D8. 結果ファイル規約（提案。Phase 4 の ADR-0004 で確定）

CLI エージェント系アダプタ（claude-code / codex / dsh）では、プロンプトで
「作業完了時に `artifacts/result.json` に `{"summary","evidence":[…]}`（`done` と同じ形）、質問がある場合は `{"question":"…"}` を書け」
と指示し、プロセス終了後にアダプタがそのファイルを読んで `done` / `question` を合成する。ファイルが無ければ `error{retryable:true}`。

採る理由: (1) `stream-json` と `--json-schema` が併用できない、(2) codex / dsh でも同じ規約が使える、(3) 最終テキストの JSON パースに頼らない。
代替案（不採用の理由）: `--output-format json --json-schema` は進捗が取れない。最終 `result` テキストを JSON として解釈するのは壊れやすい。

## 結果

- Phase 3 は D1–D6 と `fake` アダプタを実装する。D7 の claude-code 以降は Phase 4/6。
- `docs/protocol/worker-protocol.md` 初版は本 ADR を仕様として書き起こしたもの。提案 P-10〜P-13 は同文書の「提案中の拡張」節に分離してあり、DESIGN に反映されるまで規範ではない。

## DESIGN.md 修正提案（本 ADR のスコープ分）

- **P-10（§5.3 context）** `question` で `blocked` にした後、人間の回答をワーカーに渡す経路が `context{prior_review, inputs}` に無い。`context.answers: [{question, answer, answered_at}]` の追加を提案。これが無いと `taskctl answer` が機能しない。
- **P-11（§5.3 run）** `run` に `run_id` と `attempt` を追加することを提案。`run_id` は成果物・ログのひも付け、`attempt` は Symphony 同様プロンプトで「n 回目」を伝えるため。
- **P-12（§5.3 evidence）** `evidence[].command / exit / stdout_tail` を任意フィールドにすることを提案。`ArtifactExists` / `Reviewer` / `Human` の条件では実行コマンドが存在しない。
- **P-13（§5.4 claude-code 行）** 「stream-json をパースして progress/done に変換」だけでは `done.evidence` の出所が決まらない。D8 の結果ファイル規約を §5.4 に追記することを提案。
- **P-14（§5.4 dsh 行）** 「ヘッドレスCLI。同様」は不正確。`--profile headless` は最終回答テキストのみで、構造化イベントは `--profile sdk`（JSON-RPC）かセッション JSONL からしか取れない旨を追記することを提案。優先度は低い（Phase 4 任意）。
