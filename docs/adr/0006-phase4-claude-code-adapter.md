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
