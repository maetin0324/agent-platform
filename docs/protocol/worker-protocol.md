# taskd ワーカープロトコル v1（初版）

- 状態: Draft（Phase 0 初版）。規範は `docs/DESIGN.md` §5.3 と [ADR-0003](../adr/0003-worker-protocol.md)
- JSON Schema: 正は隣の `worker-protocol.schema.json`（Phase 3 で `task-worker::protocol` の Rust 型から `schemars` で生成。`task-worker` のテスト `committed_schema_matches_generated` が一致を検証し、`UPDATE_SCHEMA=1 cargo test -p task-worker` で再生成する）。本文書 §7 の手書きスキーマは説明用の抜粋
- §9 は Phase 4（ADR-0006）で確定した CLI エージェント系アダプタ（`claude-code` 等）専用の規約。§1〜§8 の
  JSON Lines プロトコルとは別物で、DESIGN §5.4 の表への反映は `docs/PROGRESS.md` の提案 P-24 として持ち越し中

## 1. 概要

オーケストレータ（taskd）はワーカーを **サブプロセス** として起動し、stdin に `run` を 1 行書いて閉じる。
ワーカーは stdout に JSON Lines で `progress` / `artifact` を任意回、最後に `done` / `error` / `question` のいずれか 1 つを書いて exit する。
全アダプタ（fake / claude-code / codex / dsh / openai-compat）はこの形に正規化する。

```
taskd ──stdin──▶ {"type":"run", ...}\n  (EOF)
taskd ◀─stdout── {"type":"progress", ...}\n
                 {"type":"artifact", ...}\n
                 ...
                 {"type":"done", ...}\n   | {"type":"error", ...}\n | {"type":"question", ...}\n
       ◀─exit──
```

## 2. 符号化の規則

| 規則 | 内容 |
|---|---|
| 形式 | 1 行 1 JSON オブジェクト。UTF-8、`\n` 終端。行内に改行を含めない（文字列内は `\n` エスケープ） |
| 識別 | 全メッセージに `type`（文字列）が必須 |
| 未知フィールド | 受信側は無視する（前方互換） |
| 未知 `type` | run を `error{retryable:false}` 相当で打ち切る |
| 非 JSON 行 | 破棄し警告ログ（ワーカーの stderr は別途 `runs/<run_id>/stderr.log` へ） |
| 行長 | 1 MiB 以下。超過はプロトコル違反 |
| バージョン | `run.protocol = 1`。非互換変更で上げる |

## 3. taskd → ワーカー

### 3.1 `run`

```json
{"type":"run",
 "protocol":1,
 "task":{ "...": "task-core::Task を serde でそのまま直列化したもの" },
 "workspace":"/abs/path/to/workspace/<task_id>",
 "context":{
   "prior_review":[{"criterion":0,"pass":false,"reason":"cargo test exit 101: ..."}],
   "inputs":[{"name":"spec.md","path":"inputs/spec.md","sha256":"…","kind":"doc"}]
 }}
```

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `protocol` | integer | ✓ | `1` |
| `task` | object | ✓ | `Task`（id, kind, title, objective, acceptance[], inputs[], depends_on[], status, priority, worker_hint, workspace, budget, attempts, …） |
| `workspace` | string | ✓ | 絶対パス。ワーカーの cwd。`artifact.path` の基準 |
| `context.prior_review` | array | ✓（空可） | 直前のレビュー結果。`{criterion: usize, pass: bool, reason: string}` |
| `context.inputs` | array | ✓（空可） | 依存成果物の `ArtifactRef`。`prepare()` で `workspace/inputs/` に配置済み |

## 4. ワーカー → taskd

### 4.1 `progress`（任意回）

```json
{"type":"progress","msg":"running cargo test"}
```

| フィールド | 型 | 必須 |
|---|---|---|
| `msg` | string | ✓ |

`WorkerProgress{run_id, msg}` として記録される。無出力タイムアウトのカウンタをリセットする。

### 4.2 `artifact`（任意回）

```json
{"type":"artifact","name":"bench.json","path":"artifacts/bench.json","kind":"json"}
```

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `name` | string | ✓ | 一意名。`ArtifactExists{name}` の照合キー |
| `path` | string | ✓ | **ワークスペース相対**。絶対パス・`..`・ワークスペース外へのシンボリックリンクは拒否 |
| `kind` | string | – | `log` / `diff` / `json` / `md` / その他自由文字列。省略時は拡張子から推定 |

受信時に taskd が sha256 を計算し `ArtifactProduced{run_id, artifact: ArtifactRef{name,path,sha256,kind}}` を記録する。ファイルが無ければ警告のみ。

### 4.3 `question`（終端）

```json
{"type":"question","text":"Which Python version should the benchmark target?"}
```

| フィールド | 型 | 必須 |
|---|---|---|
| `text` | string | ✓ |

タスクは `blocked` になる。ワーカーはこの行を書いたら exit する。人間の回答は `taskctl answer` で記録され、次回の `run` に渡す（§9 P-10 参照。DESIGN 未反映）。

### 4.4 `done`（終端）

```json
{"type":"done",
 "summary":"Added CLI parsing with clap; all tests pass.",
 "evidence":[
   {"criterion":0,"command":"cargo test","exit":0,"stdout_tail":"test result: ok. 12 passed"},
   {"criterion":1,"command":"test -f README.md","exit":0,"stdout_tail":""}
 ],
 "usage":{"input_tokens":12345,"output_tokens":678}}
```

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `summary` | string | ✓ | 人間向け要約 |
| `evidence` | array | ✓（空可） | 受け入れ条件ごとの証拠 |
| `evidence[].criterion` | integer | ✓ | `task.acceptance` の添字 |
| `evidence[].command` | string | ✓ | 実行したコマンド（§9 P-12 で任意化を提案） |
| `evidence[].exit` | integer | ✓ | 終了コード（同上） |
| `evidence[].stdout_tail` | string | ✓ | 出力末尾（同上）。4 KiB を目安に切り詰める |
| `usage` | object | – | `input_tokens`, `output_tokens`（integer）。取れないアダプタは省略 |

`done` は完了ではない。タスクは `reviewing` に入り、Reviewer が `Command` を再実行し `ArtifactExists` を検査する（DESIGN 原則 4）。

### 4.5 `error`（終端）

```json
{"type":"error","message":"claude exited with error_max_turns","retryable":true}
```

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `message` | string | ✓ | |
| `retryable` | boolean | ✓ | `true` → `attempts+1` の上で `max_retries` 内なら `ready`、超過で `failed`。`false` → `failed` |

## 5. 終了規則

1. 終端メッセージ（`done` / `error` / `question`）は run につき 1 つ。終端後の行は捨てる。
2. 終端メッセージ無しで exit した場合、exit code に関わらず `error{retryable:true, message:"worker exited without terminal message (exit=N)"}` と等価に扱う。
3. exit code は `WorkerFinished{run_id, outcome, usage}` に記録するが、状態遷移には使わない。
4. taskd 側で run を打ち切った場合（タイムアウト・cancel）は SIGTERM → 猶予（既定 10 s）→ SIGKILL。子プロセスグループごと終了させる。

## 6. タイムアウト

| 上限 | 出所 | 既定 | 超過時 |
|---|---|---|---|
| wall-clock | `task.budget.max_wall_secs` | タスクごと | `error{retryable:true,"wall clock exceeded"}` |
| 無出力 | アダプタ設定 `idle_timeout_secs` | 300 | `error{retryable:true,"idle timeout"}` |

`progress` / `artifact` の受信で無出力カウンタをリセットする。

## 7. JSON Schema（説明用の手書き抜粋。正は `worker-protocol.schema.json`）

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://taskd.local/protocol/worker-protocol-v1.json",
  "title": "taskd worker protocol v1",
  "$defs": {
    "ArtifactRef": {
      "type": "object",
      "required": ["name", "path", "sha256", "kind"],
      "properties": {
        "name": {"type": "string"},
        "path": {"type": "string"},
        "sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
        "kind": {"type": "string"}
      }
    },
    "PriorReview": {
      "type": "object",
      "required": ["criterion", "pass", "reason"],
      "properties": {
        "criterion": {"type": "integer", "minimum": 0},
        "pass": {"type": "boolean"},
        "reason": {"type": "string"}
      }
    },
    "Evidence": {
      "type": "object",
      "required": ["criterion", "command", "exit", "stdout_tail"],
      "properties": {
        "criterion": {"type": "integer", "minimum": 0},
        "command": {"type": "string"},
        "exit": {"type": "integer"},
        "stdout_tail": {"type": "string"}
      }
    },
    "Usage": {
      "type": "object",
      "properties": {
        "input_tokens": {"type": "integer", "minimum": 0},
        "output_tokens": {"type": "integer", "minimum": 0}
      }
    },
    "RunRequest": {
      "type": "object",
      "required": ["type", "protocol", "task", "workspace", "context"],
      "properties": {
        "type": {"const": "run"},
        "protocol": {"const": 1},
        "task": {"type": "object", "description": "task-core::Task (schema generated in Phase 1)"},
        "workspace": {"type": "string"},
        "context": {
          "type": "object",
          "required": ["prior_review", "inputs"],
          "properties": {
            "prior_review": {"type": "array", "items": {"$ref": "#/$defs/PriorReview"}},
            "inputs": {"type": "array", "items": {"$ref": "#/$defs/ArtifactRef"}}
          }
        }
      }
    },
    "WorkerMessage": {
      "oneOf": [
        {
          "type": "object", "required": ["type", "msg"],
          "properties": {"type": {"const": "progress"}, "msg": {"type": "string"}}
        },
        {
          "type": "object", "required": ["type", "name", "path"],
          "properties": {
            "type": {"const": "artifact"},
            "name": {"type": "string"},
            "path": {"type": "string"},
            "kind": {"type": "string"}
          }
        },
        {
          "type": "object", "required": ["type", "text"],
          "properties": {"type": {"const": "question"}, "text": {"type": "string"}}
        },
        {
          "type": "object", "required": ["type", "summary", "evidence"],
          "properties": {
            "type": {"const": "done"},
            "summary": {"type": "string"},
            "evidence": {"type": "array", "items": {"$ref": "#/$defs/Evidence"}},
            "usage": {"$ref": "#/$defs/Usage"}
          }
        },
        {
          "type": "object", "required": ["type", "message", "retryable"],
          "properties": {
            "type": {"const": "error"},
            "message": {"type": "string"},
            "retryable": {"type": "boolean"}
          }
        }
      ]
    }
  }
}
```

## 8. 例: fake ワーカーの 1 run

stdin:

```json
{"type":"run","protocol":1,"task":{"id":"01J8…","kind":"execute","title":"add README example","acceptance":[{"text":"cargo test exits 0","check":{"command":{"cmd":"cargo test","expect_exit":0}}}],"budget":{"max_turns":20,"max_wall_secs":600,"max_retries":1},"attempts":0},"workspace":"/srv/ws/01J8…","context":{"prior_review":[],"inputs":[]}}
```

stdout:

```json
{"type":"progress","msg":"editing README.md"}
{"type":"artifact","name":"readme.diff","path":"artifacts/readme.diff","kind":"diff"}
{"type":"progress","msg":"running cargo test"}
{"type":"done","summary":"Added usage example to README","evidence":[{"criterion":0,"command":"cargo test","exit":0,"stdout_tail":"test result: ok. 3 passed"}]}
```

→ taskd: `WorkerProgress` ×2, `ArtifactProduced`, `WorkerFinished{outcome: done}`, `Transitioned{running→reviewing, reason:"worker_done"}`。

## 9. CLI エージェント系アダプタの結果ファイル規約（Phase 4、ADR-0006 で確定）

`claude-code`（および将来の `codex`/`dsh`）は本文書 §1〜§8 の JSON Lines プロトコルを**話さない**。
`claude` CLI は独自の `stream-json` イベント（`system`/`assistant`/`user`/`result`）を吐くだけであり、
taskd はこれを直接パースできない。そこでこれらのアダプタはプロンプトでワーカー（Claude Code 自身）に
次を指示し、アダプタが作業ディレクトリの `artifacts/result.json` を読んで本文書の `done`/`question` に
相当する終端を合成する（旧 P-13。ADR-0006 D3 で確定。旧 P-11 の `run_id`/`attempt` はスキーマ変更せず
プロンプト文面にのみ埋め込む。旧 P-12 の「evidence を任意化」は Phase 3 の Reviewer が `evidence` の
内容を見ずに `Command`/`ArtifactExists` を再実行するため実質的に問題にならず、スキーマは変更しない）:

```json
{"summary": "...", "evidence": []}
```
```json
{"question": "..."}
```

判定順序（ADR-0006 D4）: stream-json の最後の `{"type":"result",...}` が `is_error:true` か
`subtype != "success"` なら、結果ファイルの内容によらず `error{retryable:true}` とする（自己申告の
`done` は信用しない）。`success` の場合のみ結果ファイルを読み、無い／不正なら `error{retryable:true}`。

`context.answers`（旧 P-10、`question` → `blocked` → `taskctl answer` の回答をワーカーへ渡す経路）は
Phase 4 でも未実装のまま（`docs/PROGRESS.md` の未解決事項を参照）。
