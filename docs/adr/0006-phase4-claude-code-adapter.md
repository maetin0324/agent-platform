# ADR-0006: Phase 4 — claude-code アダプタとドッグフーディング

- 日付: 2026-09-13
- 状態: Accepted（Phase 4）
- 関連: `docs/DESIGN.md` §5.4, §6 Phase 4 / [ADR-0003](0003-worker-protocol.md) D7/D8 / [ADR-0005](0005-phase3-dispatch-and-worker.md) D2

## 文脈

ADR-0003 D7 は `claude-code` アダプタの形をほぼ確定していた（`claude -p <prompt> --output-format
stream-json --verbose …` を起動し、`result.subtype`/`is_error` と「結果ファイル規約」で終端を合成する）。
D8 は結果ファイル規約を「提案。Phase 4 の ADR で確定」としていた。本 ADR はそれを確定し、実装方針を定める。

`claude` CLI は taskd 独自のワーカープロトコル（`docs/protocol/worker-protocol.md`）を話さない。
`--output-format stream-json` は Claude Code 自身のイベント形式（`system`/`assistant`/`user`/`result`）を吐く。
したがって `claude-code` アダプタは「taskd プロトコルへの翻訳層」であり、`task-worker::subprocess::run_subprocess`
（fake アダプタが使う、stdin に `RunRequest` を書き stdout の `WorkerMessage` 行を読む実行器）はそのままでは使えない。

## 決定

### D1. 実行方式は専用の翻訳ループ。低レベル部分だけ `subprocess.rs` と共有する

`run_subprocess` を流用せず、`crates/task-worker/src/claude_code.rs` に専用のループを書く。ただし
プロセスグループへのシグナル送信・SIGTERM→猶予→SIGKILL・1 行 1 MiB 上限の読み取りは `subprocess.rs` の
既存実装（`send_signal_to_group`, `kill_now`, `reap_after_terminal`, `read_line_limited`, `LineOutcome`,
`MAX_LINE_BYTES`）を `pub(crate)` にして再利用する（ADR-0003 D4 の生存監視をここでも同じ規則で満たすため。
ロジックの複製を避ける）。wall-clock / 無出力タイムアウトの扱いも `run_subprocess` と同じ形（ループの先頭で
経過時間を見て打ち切り、`force_kill` なら即 kill、そうでなければ穏やかな刈り取り）にする。

### D2. プロンプトはタスクから機械的に組み立てる

`build_prompt(task, context) -> String` を純粋関数として実装し、次を含める:

- タイトル・目的（`task.title`, `task.objective`）
- 受け入れ条件を番号付きで列挙（`task.acceptance[].text` と、`Check::Command{cmd,expect_exit}` なら
  「レビュアーが `cmd` を再実行し exit が `expect_exit` と一致するか検証する」ことを明示）
- `context.prior_review` があれば「前回の判定」として列挙し、修正を促す
- 作業ディレクトリは cwd であること、`artifacts/` 配下に成果物を置くこと
- D3 の結果ファイル規約（`artifacts/result.json` の書式と、質問がある場合の書式）
- 「対話はできない。質問がある場合も `artifacts/result.json` に書いて終了せよ」という制約

`prior_review` の反映と `artifacts/result.json` の指示は ADR-0003 D8 / worker-protocol.md §9 P-13 の
内容をそのままプロンプト文面に落としたもの。`RunRequest`（`protocol.rs`）の wire スキーマは変更しない
（`claude` はこの JSON を読まないため、`fake`/将来の `codex` アダプタと共有するスキーマを Phase 4 のためだけに
広げる必要が無い。P-11 の `run_id`/`attempt` もプロンプト文面にだけ埋め込み、スキーマは変えない）。

### D3. 結果ファイル規約（ADR-0003 D8 / P-13 を確定）

ワーカー（Claude Code 自身）は作業完了時に workspace 直下 `artifacts/result.json` に次のいずれかを書く:

```json
{"summary": "...", "evidence": [{"criterion":0,"command":"...","exit":0,"stdout_tail":"..."}]}
```
```json
{"question": "..."}
```

`evidence` は空配列でもよい（Phase 3 の Reviewer は `Command`/`ArtifactExists` の判定にワーカー自己申告の
`evidence` を使わず実際に再実行するため、内容の正確さは要求しない。原則 4）。アダプタは:

1. プロセス終了後（force kill でない限り）に `artifacts/result.json` を読む。
2. `question` キーがあれば `Terminal::Question`。
3. `summary` キーがあれば `Terminal::Done{summary, evidence: evidence.unwrap_or_default(), usage}`
   （`usage` は D4 の `result` メッセージから）。
4. ファイルが無い／どちらのキーも無い／JSON として不正 → `Terminal::Error{retryable:true, "…missing/invalid
   artifacts/result.json …"}`。

### D4. `result` ストリームメッセージの優先判定

stream-json の最後に現れる `{"type":"result", "subtype":…, "is_error":…, "usage":{...}, ...}` を記録する。

- `is_error == true` または `subtype` が `"success"` 以外（例: `error_max_turns`, `error_during_execution`）の
  場合、D3 の結果ファイルより **先に** `Terminal::Error{retryable:true, message: "claude result: <subtype>"}`
  とする（自己申告の `done` があっても信用しない。ワーカーがエラー終了したという事実の方が強い）。
  Phase 4 では単純化のため全て `retryable:true` とする（`error_max_turns` のような一時的失敗が主目的。
  恒久的な拒否かどうかの区別は Reviewer が再実行時にも同じ壁にぶつかって `max_retries` で `failed` になる
  ため実害は小さい。将来 `is_error` の内容で細分化する余地は提案として残す）。
- `subtype == "success"` かつ `is_error != true` なら D3 に進む。
- `result` メッセージを一度も観測できずに exit した場合（クラッシュ等）は `subprocess.rs` と同じ
  `"worker exited without terminal message (exit=…)"` 相当の `Error{retryable:true}` とする。

### D5. progress への変換

`assistant` メッセージの `message.content[]` から `type=="text"` の `text` を、`type=="tool_use"` の
`name`（+ 主要な `input` フィールドの要約、長ければ切り詰め）を `sink.progress()` に渡す。`system`/`user`/
その他の `type` は無視する。JSON として parse できない行、または既知の `type` に一致しない行は
**プロトコル違反として打ち切らず**、無視して次の行を読む（ADR-0003 D1 の「非 JSON 行は破棄」に準じるが、
`claude` 自身のフォーマットは taskd が定義したものではないため、未知 `type` も同様に寛容に扱う。
これは自前プロトコルの `WorkerMessage`（未知 `type` はプロトコル違反）とは異なる方針であり、意図的な違い）。

### D6. 起動コマンドと設定

```
claude -p <prompt> --output-format stream-json --verbose \
  --permission-mode <config, 既定 "bypassPermissions"> \
  --max-turns <task.budget.max_turns> --no-session-persistence \
  [--model <config.model>] [config.extra_args...]
```

cwd は `req.workspace`。`--permission-mode bypassPermissions` を既定にする理由: taskd はワーカーの
標準入力を介した許可プロンプトに応答する仕組みを持たない（ADR-0003 原則: ワーカーはステートレスにサブプロセス
として起動し stdout だけを読む）ため、許可待ちで無出力タイムアウトに達して失敗するのを避ける。ワークスペースは
`LocalWorkspace::prepare` が作った専用ディレクトリなので、権限バイパスの影響範囲はそのディレクトリと
`claude` 自身のツール権限に閉じる。

設定 `[adapters.claude_code]`（taskd.toml）: `command`（既定 `"claude"`）, `extra_args`（既定 `[]`）,
`permission_mode`（既定 `"bypassPermissions"`）, `model`（省略可）, `env`（追加環境変数。将来の
`CLAUDE_CONFIG_DIR` によるアカウント分離はここに `env` として設定する。複数アカウント運用の抽象化自体は
供給層の担当なので Phase 4 では素通りの環境変数以上のことはしない）。

### D7. taskctl からのタスク作成は Phase 4 のスコープ外（P-17 は不採用のまま）

Phase 2 の未解決事項・P-17（`taskctl add --check-cmd` の追加）は、Phase 4 開始時に採否を決めるよう
Phase 3 の提案で持ち越されていた。本 Phase では **採用しない**。理由: `taskctl` の CLI 拡張は Phase 2 の
スコープに戻る変更であり、ドッグフード実演に必要なタスク投入は Phase 3 の e2e テストと同じ方法
（`task-core::TaskStore` API を直接呼ぶ）で足りる。`crates/taskd/examples/seed_hello_crate_task.rs` に
最小のシード実行ファイルを用意し、人間はこれで `Check::Command` 付きタスクを 1 件 `ready` として挿入できる。
`--check-cmd` の要否判断は改めて提案として残す。

## 結果

- `crates/task-worker/src/claude_code.rs` を新設。`ClaudeCodeAdapter`, `ClaudeCodeConfig`,
  `build_prompt`（純粋関数、ユニットテスト可能）。
- `crates/task-worker/src/subprocess.rs` の低レベルヘルパを `pub(crate)` に変更（可視性のみ、既存の
  挙動・テストは変更なし）。
- `crates/taskd/src/config.rs` に `AdaptersConfig.claude_code` を追加し、`validate()` が `adapter =
  "claude-code"` を受理するようにする。
- `crates/taskd/src/lib.rs::build_dispatcher` が設定に応じて `ClaudeCodeAdapter` を組み立て登録する。
- `crates/taskd/examples/seed_hello_crate_task.rs` と `config/taskd.claude-code.example.toml` を追加し、
  人間が `examples/hello-crate` に対するドッグフードタスクを投入・実行できるようにする。
- テストは `claude` の代わりに `sh` スクリプトで stream-json 相当の行を模擬する（ADR-0003/0005 の
  `fake` アダプタのテストと同じ手法）。ネットワークには一切出ない。実際の `claude` CLI 起動による
  確認は人間が行う（DESIGN §6 Phase 4 の注記どおり。本セッションの実行環境では `claude` サブプロセスの
  起動そのものが安全機構によって拒否されるため、このセッション内では実行できなかった。詳細は
  `docs/PROGRESS.md` Phase 4 節）。

## 監査による修正（実装後、auditor サブエージェントの指摘を反映）

- **D4 の徹底**: `result` メッセージを一度も観測できずに exit した場合、`artifacts/result.json` が
  （前回の run の名残やクラッシュ直前の書きかけとして）存在していても一切読まず、無条件に
  `Error{retryable:true, "worker exited without a result message (exit=…)"}` とするよう修正した
  （当初の実装は `last_result` が無くてもファイルがあれば読んでしまい、クラッシュを `Done` と
  誤判定しうる穴があった）。
- **D3 の徹底**: run 開始時に `artifacts/result.json` が既に存在すれば削除してから起動するよう
  修正した（リトライで前回の結果ファイルを今回の結果と誤読しないため）。
- D2: `build_prompt` に `run_id` と attempt 番号（`task.attempts + 1` / `max_retries + 1`）をプロンプト
  文面へ埋め込んだ（当初漏れていた）。
- D5: `tool_use` の progress 変換に `input` の要約（`serde_json::Value` を文字列化し 200 文字で切り詰め）
  を加えた（当初 `name` のみだった）。

## DESIGN.md 修正提案（本 ADR のスコープ分）

- **P-24（§5.4 claude-code 行）** 結果ファイル規約（D3）と `result` メッセージ優先判定（D4）を
  §5.4 の表に反映することを提案（ADR-0003 P-13 の確定版）。
- **P-25（§5.9）** `taskctl add --check-cmd`（P-17）の採否は依然未決。Phase 4 では見送り、
  シード実行ファイルで代替した（D7）。

## Phase 115 追記（2026-09-24）: `result.json` の置き場を `work_dir` に依存させない

### 背景

本番で「報告のまとめ: Engineering」（`celeris::reports` の compaction タスク、role
`report-compressor`、adapter `claude-code`）が 2 件連続で失敗した（01M3915FARENW8M0JM11XVF6W0 /
01M38T8N17MEWPTJQXGX1TNYJD）。outcome は `error(retryable=true): claude exited without
<workspace>/artifacts/result.json`。

原因は 2 つ重なっていた:

1. **`work_dir != workspace` の worktree で相対 `artifacts/result.json` を書いた**: Engineering の
   部署の案件にリポジトリ `agent-platform` が primary として登録されていたため、`task-ops::add::
   resolve_repos`（ADR-0043 D2 の「明示 > 親 > 案件の primary」）が compaction タスクの
   `repos`（作成時は空）にその primary を継がせ、結果として `task-dispatch::dispatcher::
   task_workspaces_for` がそのリポジトリの git worktree を用意していた（`work_dir =
   <workspace>/repos/agent-platform`）。プロンプトの大半（`preamble.rs` / `claude_code.rs::
   result_json_instructions`）は `RunRequest::artifacts_rel()` 経由で `work_dir.is_some()` のとき
   既に絶対パスを組んでいたが、`task-core::report::compaction_objective`（compaction タスクの
   `objective` そのもの）だけが `` `artifacts/result.json` `` と相対パスを直書きしていた。モデルは
   objective の指示に従い、cwd（= worktree）相対で書いてしまった。
2. **compaction タスクは diff を作らないのに、部署にリポジトリが付いているというだけで毎回 worktree
   （`repos/agent-platform` の git worktree）を作っていた**。無駄で遅い（`/home` は NFS 上の loop）。
   Phase 99（ADR-0059）で「コマンドを実行するだけのオペレーション」は worktree 不要とした流れと
   同じはずだったが、`workspace_mode` は `Local`（`cluster` 無し）には効いていなかった。

### 決定

**D1. プロンプトは常に絶対パス**。`report.rs` の compaction オブジェクトはタスク作成時に一度だけ
組み立てて保存する静的な文字列で、実行時の `work_dir`（部署のリポジトリの worktree になることが
ある）を知らない。ここでパスを断定するのをやめ、「下の Instructions（実行時に絶対パスで組む）を
見ろ」とだけ書く（`task_core::report::compaction_objective`）。`claude_code.rs::build_prompt` /
`preamble.rs` は既に `RunRequest::artifacts_rel()` 経由で `work_dir.is_some()` のとき絶対パスを
組んでいた（ADR-0036 D3）ので変更なし。加えて、`work_dir != workspace` の run にだけ、プロンプトの
先頭（`claude_code::work_dir_note`。`codex` も同じ関数を再利用）に「cwd は `<work_dir>`（リポジトリの
worktree）。成果物ディレクトリは `<artifacts_dir>`。相対 `artifacts/` はリポジトリの中を指すので
使わない。」の 2 行を足す（絶対パスの指示を読み飛ばして相対 `artifacts/` を書いてしまう事故の
再発防止。`work_dir` が無い・`workspace` と同じ run の文面は 1 バイトも変わらない）。

**D2. フォールバック**: run 終了時に `<artifacts_dir>/result.json` が無く、`<work_dir>/artifacts/
result.json`（`work_dir != workspace` のとき）が有れば、それを `<artifacts_dir>/result.json` へ移して
採用し、WARN「result.json was written under work_dir; moved」を出す
（`subprocess::adopt_result_json_written_under_work_dir`。claude-code / codex の両アダプタが、
結果ファイルを読む前・かつ Phase 112 D3「最終メッセージから回収」より前に呼ぶ）。worktree の中には
残さない（移動後に空になった `artifacts/` ディレクトリも消す）。リトライで前回の run の名残と誤読
しないよう、run 開始時に `<artifacts_dir>/result.json` と同様 `<work_dir>/artifacts/result.json` も
消す。

**D3. 内部タスクは worktree を作らない**: `resolve_repos`（`task-ops::add`）に「`Local`（`cluster` 無し）
で `NewTaskSpec.workspace_mode == Some(Shared)` なら、親・案件の primary への暗黙継承をしない」を
足した（ADR-0059 の `mode: "shared"` の語彙をそのまま流用。`Remote` の意味は変えない）。同時に
`build_task` が `Local` の `WorkspaceSpec.mode` にも `spec.workspace_mode` を通すようにした（従来は
`Remote` にしか効かず、`Local` は常に `mode: None` に固定されていた）。`workspace_mode: Some(Shared)`
になれば `dispatcher::legacy_worktree_for` も worktree を作らないので、diff を作らない内部タスクは
`repos` も worktree も持たず `work_dir = workspace` のまま走る。適用先:
`celeris::reports::compaction_spec`、`celeris::knowledge_maint` の知識整理タスク、
`task_ops::conversation::conversation_task`（対話は元々 `repos` が常に空で worktree を作らないが、
将来リポジトリ付きの案件に対話タスクを置く経路ができても worktree を切らせないよう念のため明示した）。
計画（`project_plan.rs`）や人が作る通常のタスクは対象外（`Local` の `workspace_mode` は明示しない限り
`None` のままで、従来どおり案件の primary を継ぐ）。

**D4. テスト**: `crates/task-worker/src/claude_code.rs` / `codex.rs` に
`work_dir_note_appears_in_the_prompt_when_work_dir_differs_from_workspace`（D1）と
`a_result_json_written_under_work_dir_is_adopted_and_not_left_behind`（D2）をそれぞれ追加。
`crates/task-dispatch/src/dispatcher.rs` に
`a_compaction_task_does_not_get_a_worktree_even_when_its_project_has_a_primary_repo`（D3。
`task_ops::add::create_support_task` を実際に通す）を追加。`crates/task-core/src/report.rs` の
既存テストに「`compaction_objective` は `artifacts/result.json` という文字列を一切含まない」を
追記（D1 の裏返し）。

### 結果

- `crates/task-worker/src/subprocess.rs`: `adopt_result_json_written_under_work_dir`（pub(crate)、
  D2）。
- `crates/task-worker/src/claude_code.rs`: `work_dir_note`（pub(crate)、D1）。プロンプトの先頭へ
  prepend、run 開始時の名残掃除、`(None, Some(meta))` 分岐で `terminal_from_result` の前に D2 の
  採用を呼ぶ。
- `crates/task-worker/src/codex.rs`: 同じ 3 箇所（`work_dir_note` は re-export して再利用）。
- `crates/task-core/src/report.rs`: `compaction_objective` からパスの直書きを除去（D1）。
- `crates/task-ops/src/add.rs`: `resolve_repos` に `skip_fallback: bool` を追加、`build_task` が
  `Local` の `mode` にも `workspace_mode` を通す（D3）。
- `crates/celeris/src/reports.rs` / `knowledge_maint.rs`、`crates/task-ops/src/conversation.rs`:
  該当タスクの `workspace_mode` / `WorkspaceSpec.mode` を `Some(Shared)` にする（D3）。

### 未解決事項

- P-115-1: D2 のフォールバックは `<work_dir>/artifacts/result.json` の 1 か所だけを見る。ワーカーが
  さらに別の相対パス解釈（例えば `work_dir` の親ディレクトリ相対）で書く可能性は未観測・未対応。
  実機でさらに別の誤読パターンが見つかれば追記する。
- P-115-2: D3 は `report-compressor` / `knowledge` / 対話の 3 種類だけを対象にした。今後増える
  「diff を作らない内部タスク」がこの 3 種類のパターン（`create_support_task` + `workspace_mode:
  Shared`）を踏襲するかは、その都度の実装者の判断に委ねる。

### 提案

- P-115-3: 本番の 01M3915FARENW8M0JM11XVF6W0 / 01M38T8N17MEWPTJQXGX1TNYJD は `failed` のまま残って
  いる可能性がある（本番 DB を直接操作していないので現状は変えていない）。デプロイ後、
  `celerisctl rereview` するか自然に再発しないことを確認するかは人の判断待ち。
