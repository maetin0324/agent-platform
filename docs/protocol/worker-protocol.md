# taskd ワーカープロトコル v1/v2

- 状態: Draft（Phase 0 初版、Phase 10 で v2 に拡張）。規範は `docs/DESIGN.md` §5.3 と [ADR-0003](../adr/0003-worker-protocol.md)、
  v2 の追加分は [ADR-0016](../adr/0016-roles-and-delegation.md)（役割と委譲、実装メモ M3/M4/M8/M9）
- JSON Schema: 正は隣の `worker-protocol.schema.json`（Phase 3 で `task-worker::protocol` の Rust 型から `schemars` で生成。`task-worker` のテスト `committed_schema_matches_generated` が一致を検証し、`UPDATE_SCHEMA=1 cargo test -p task-worker` で再生成する）。本文書 §7 の手書きスキーマは説明用の抜粋
- §9 は Phase 4（ADR-0006）で確定した CLI エージェント系アダプタ（`claude-code` 等）専用の規約。§1〜§8 の
  JSON Lines プロトコルとは別物で、DESIGN §5.4 の表への反映は `docs/PROGRESS.md` の提案 P-24 として持ち越し中
- **v2（ADR-0016 M9, Phase 10）**: `run.protocol` を `2` に上げた。追加は `delegate` メッセージ（§4.x）、
  `context.role` / `context.children`（§3.1）、`task.role` / `task.aggregate`。全て**追加のみ**で、`protocol`
  フィールドの値は検査していないため v1 のワーカー（この節を実装しないもの）はそのまま動く
- **v3（ADR-0027 D1, Phase 16）**: `context.available_genres`、`task.genre`、`delegate` の `tasks[].genre`
- **v4（ADR-0033 D4/D6, Phase 24）**: `context.node` / `context.memory` / `context.conversation` /
  `context.standing_rules` / `context.organization`（§3.1）、`delegate` の `tasks[].assignee`、
  結果ファイルの `memory`（§9）。全て**追加のみ**で、v1〜v3 のワーカーはそのまま動く

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
 "protocol":2,
 "task":{ "...": "task-core::Task を serde でそのまま直列化したもの（role・aggregate を含む）" },
 "workspace":"/abs/path/to/workspace/<task_id>",
 "context":{
   "prior_review":[{"criterion":0,"pass":false,"reason":"cargo test exit 101: ..."}],
   "inputs":[{"name":"spec.md","path":"inputs/spec.md","sha256":"…","kind":"doc"}],
   "answers":[{"question":"which crate version?","answer":"1.0"}],
   "role":{"id":"lead","instructions":"You coordinate the work of others."},
   "children":[{"id":"01J9…","title":"implement parser","role":"implementer","status":"done",
                "outcome":"done","artifacts":[{"name":"parser.rs","path":"artifacts/parser.rs","sha256":"…","kind":"rs"}],
                "workspace":"/abs/path/to/workspace/<child_task_id>"}]
 }}
```

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `protocol` | integer | ✓ | `1`〜`4`（現在は `4`。v2 = ADR-0016 M9、v3 = ADR-0027 D1、v4 = ADR-0033 D4/D6）。ワーカーはこの値を検査する必要はない |
| `task` | object | ✓ | `Task`（id, kind, title, objective, acceptance[], inputs[], depends_on[], status, priority, worker_hint, workspace, budget, attempts, `role`, `aggregate`, …）。`task.role`（`Option<string>`）はタスクの役割名、`task.aggregate`（`bool`。既定 false）は集約 run の親かどうか（ADR-0016 D1/D3） |
| `workspace` | string | ✓ | 絶対パス。ワーカーの cwd。`artifact.path` の基準 |
| `context.prior_review` | array | ✓（空可） | 直前のレビュー結果。`{criterion: usize, pass: bool, reason: string}` |
| `context.inputs` | array | ✓（空可） | 依存成果物の `ArtifactRef`。`prepare()` で `workspace/inputs/` に配置済み |
| `context.answers` | array | –（省略可、空なら省略） | `taskctl answer` で記録された `question` → 人間の回答の履歴（時系列、`{question: string, answer: string}`）。ADR-0010 D3, P-10。前方互換のため未知のワーカーは無視してよい |
| `context.role` | object | –（省略可。v2, ADR-0016 D1/M3） | タスクに役割があるときだけ `Some`。`{id: string, instructions: string}`（`instructions` は `[[roles]]` に指示文が無ければ空文字列）。`claude-code`/`codex` はプロンプトの前置きにする（`## Role: <id>`） |
| `context.children` | array | –（省略可。空なら省略。v2, ADR-0016 D3/M4） | 集約 run（`task.aggregate == true` の親の、子が全て終端になった後の run）でのみ非空。`ChildSummary`: `{id, title, role?, status, outcome?, artifacts: ArtifactRef[], workspace?}` |
| `context.node` | object | –（省略可。v4, ADR-0033 D4） | `task.assignee` の組織ノード（担当が決まっている run だけ）。`{id, name, brief?}`。プロンプトの一番前に「あなたは誰で、何の担当か」として置かれる |
| `context.memory` | object | –（省略可。v4, ADR-0033 D6） | `[memory]` を設定し、担当が決まっている run だけ。`{notes?: string, project?: string}`（`<memory_dir>/<node_id>/notes.md` と `projects/<project_id>.md` の中身。それぞれ 8,000 字で切る） |
| `context.conversation` | array | –（省略可。空なら省略。v4, ADR-0033 D4） | その案件でのこのノードと人の**直近のやり取り**（既定 20 件、古い順）。`{role: "user"|"node", text: string}` |
| `context.standing_rules` | array | –（省略可。空なら省略。v4, ADR-0033 D5） | 「今後ずっと」の認可。**Phase 26 が埋める。今は常に空** |
| `context.organization` | array | –（省略可。空なら省略。v4, ADR-0033 D4） | 分解・委譲できる run（`context.available_genres` を渡す run と同じ条件）に渡す組織図。`{id, name, kind, parent_id?, brief?, genre?}`。「どの課に何を振るか」を `assignee` で決めさせる |

`context.answers` は、このタスクの `Event::Answered` を時系列に並べたもの（`question` は直前の
`WorkerFinished.outcome` の `"question: "` 接頭辞から取ったもの、無ければ空文字列）。`claude-code`/`codex`
アダプタは、空でなければプロンプトに「以前の質問への人間の回答」節として反映する（Execute/Plan のみ。
Review プロンプトには含めない）。JSON Lines プロトコルを直接話す `fake` 等のワーカーは、この配列を読んで
自由に扱ってよい（taskd 側は解釈を強制しない）。

`context.role` / `context.children` も同様に、JSON Lines を直接話すワーカーは自由に解釈してよい
（taskd 側は解釈を強制しない）。`claude-code`/`codex` の反映のしかたは §9 M8 を参照。

**v4 の前置き（ADR-0033 D4/D6, Phase 24）**: `context.node` / `context.standing_rules` / `context.memory` /
`context.conversation` / `context.role` は、CLI エージェント系アダプタでは `task_worker::preamble::render` が
**この順**で 1 か所に組む（役職と brief → 永続の認可 → 記憶 → 直近のやり取り → 役割の指示文 → 記憶の書き方）。
これらが全て空なら、前置きは Phase 23 までの出力と 1 バイトも変わらない。`local-deep-research` だけは
役割の指示文を載せない（ADR-0029 / Phase 19: 検索エンジンに渡す問いを役割の文面で濁さないため）。

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
   {"criterion":1,"command":"test -f README.md","exit":0,"stdout_tail":""},
   {"criterion":2}
 ],
 "usage":{"input_tokens":12345,"output_tokens":678}}
```

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `summary` | string | ✓ | 人間向け要約 |
| `evidence` | array | ✓（空可） | 受け入れ条件ごとの証拠 |
| `evidence[].criterion` | integer | ✓ | `task.acceptance` の添字 |
| `evidence[].command` | string | – | 実行したコマンド。`ArtifactExists` / `Reviewer` / `Human` の条件では省略してよい（ADR-0012 D3, P-12） |
| `evidence[].exit` | integer | – | 終了コード（同上） |
| `evidence[].stdout_tail` | string | – | 出力末尾（同上）。4 KiB を目安に切り詰める |
| `usage` | object | – | `input_tokens`, `output_tokens`（integer）。取れないアダプタは省略 |

`done` は完了ではない。タスクは `reviewing` に入り、Reviewer が `Command` を再実行し `ArtifactExists` を検査する（DESIGN 原則 4）。

### 4.5 `error`（終端）

```json
{"type":"error","message":"claude exited with error_max_turns","retryable":true}
```

```json
{"type":"error","message":"429 rate limit exceeded","retryable":true,
 "provider_failure":{"kind":"throttled","retry_after_secs":60}}
```

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `message` | string | ✓ | |
| `retryable` | boolean | ✓ | `true` → `attempts+1` の上で `max_retries` 内なら `ready`、超過で `failed`。`false` → `failed` |
| `provider_failure` | object | – | 供給側の失敗の種別（ADR-0010 D5, P-21）。付いていれば `retryable` の値に関わらずディスパッチャは `attempts` を消費せず `requeue` し、そのプロバイダを cooldown にする |

`provider_failure.kind` は次の 3 種のいずれか（`#[serde(tag="kind", rename_all="snake_case")]`。判定・分類は
アダプタが行い、遷移の判断（requeue するかどうか）はディスパッチャの責務。原則: 協調判断に LLM を使わず、
ここも決定的な規則だけで完結する）:

| `kind` | 追加フィールド | 意味 |
|---|---|---|
| `throttled` | `retry_after_secs`（integer） | レート制限。しばらく待てば復帰しうる |
| `auth_failed` | – | 認証切れ・未ログイン。人間の対応が要る |
| `exhausted` | – | 利用上限・クレジット枯渇 |

JSON Lines プロトコルを直接話す `fake` 等のワーカーは `provider_failure` を任意で付けてよい。
`claude-code`/`codex` はこのプロトコルを話さないので、代わりにエラー文面を決定的な文字列規則で分類する
（§9 参照）。

### 4.6 `delegate`（任意回、非終端。v2, ADR-0016 D2）

```json
{"type":"delegate","tasks":[
  {"title":"implement the parser","objective":"...","acceptance":[{"text":"cargo test passes","check":{"type":"command","cmd":"cargo test","expect_exit":0}}],"role":"implementer","depends_on":[]},
  {"title":"write docs","objective":"...","acceptance":[{"text":"README updated","check":{"type":"artifact_exists","name":"README.md"}}],"depends_on":[0]}
]}
```

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `tasks` | array | ✓（空も可だが意味がない） | `DelegateTask` の配列。1 回の `delegate` メッセージにつき複数件でよく、同じ run が複数回 `delegate` を送ってもよい |

`DelegateTask`（`task-core::DelegateTask`）:

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `title` | string | ✓ | 空文字列は不合格 |
| `objective` | string | ✓ | 空文字列は不合格 |
| `acceptance` | array | ✓（1 件以上） | `task-core::Criterion` の配列（`text` + `check`）。空配列は不合格 |
| `role` | string | – | 役割名（`[[roles]]` にあれば既定と指示文が効く。無くても自由記述として許される） |
| `depends_on` | array | –（省略可、空なら省略） | 各要素は整数（同じ `tasks` 配列内のインデックス）か、既存タスクの ID（文字列）。混在可 |
| `tier` | string | – | `frontier` / `standard` / `cheap`。省略時は役割の既定 → 親の tier |

未知フィールドは拒否する（`deny_unknown_fields`。綴り間違いの検出のため。§2 の「未知フィールドは無視する」
という一般規則とは意図的に逆。`Plan` の `NewTask` と同じ方針）。

taskd 側の扱い（ADR-0016 D2, 実装メモ M2/M6/M7）:

- ディスパッチャは受け取った提案を、ストアを見ない検証（空欄、`depends_on` の範囲・自己参照・閉路、ID の
  書式）と、ストアを見る検証（既存 ID の依存が存在し `failed`/`cancelled` でないこと、依頼元の祖先や自分
  自身に依存していないこと、木の深さ・件数・run 数の上限）の両方を通ったものだけを子タスクとして挿入する。
  挿入は `draft` → 同じトランザクションで `Accept`（`ready`）、`Event::Delegated{run_id, task_ids}` を記録する
  （`plan.auto_accept` は見ない）。
- 上限（設定 `[delegation]`。既定値）: `max_delegate_per_run`（8。同じ run の複数の `delegate` をまたいで数える）、
  `max_tree_depth`（5。根 = 1）、`max_tree_runs`（100。根から全ての子孫の `WorkerStarted` の合計）。
- 拒否した提案は挿入せず、理由を `WorkerProgress{msg: "delegate rejected: tasks[i] \"<title>\": <reason>"}` として
  残す。**run 自体は失敗しない**（拒否は run の terminal に影響しない）。
- 親は run を続けてよい。親が `done` を返しても、委譲した子が全て終端になるまで親は `reviewing` のまま
  （`task.aggregate == true` なら M4 の集約 run、`false` なら子の成否を問わず `done` になる）。
- 自分の親（祖先を含む）や自分自身を `depends_on` に指定することはできない。

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

### 6.1 heartbeat（リースの延長。プロトコルのメッセージではない）

`heartbeat` は本文書が定義する JSON Lines メッセージ（`progress`/`artifact`/`done`/`error`/`question`）の
1 つではない。ワーカーの stdout から **1 行読むたび**（`progress`/`artifact` 等の既知メッセージだけでなく、
破棄される非 JSON 行や行長超過も含む）に、アダプタ内部で `EventSink::heartbeat()` を呼ぶだけの生存通知
である（ADR-0010 D7, P-7）。ワーカー自身がこれを送出するのではなく、アダプタが「まだ子プロセスが出力して
いる」という事実からこの通知を合成する。

ディスパッチャ（`StoreSink::heartbeat`）はこれを使って DB 上のリース（`expires_at`）を延長する
（`renew_lease`。状態遷移ではないので `events` には記録しない）。取得時のリース ttl は
`max_wall_secs + lease_grace` のままだが、無出力タイムアウト（`idle_timeout`）より wall-clock の方が
大幅に長い場合でも、生きているワーカーのリースが `idle_timeout` 到達前に切れることはない
（延長間隔を `lease_grace / 2` 以下に保つので、延長後の期限は「直前の出力時刻 + `idle_timeout` + `lease_grace / 2`」以降になる）。
ただしこれは、無出力で強制終了された run の結果が `kill_grace`（SIGKILL までの猶予）と `tick_ms`（次 tick での取り込み）の
分だけ遅れて処理されることを含めて `kill_grace + tick_ms < lease_grace / 2` のときに成り立つ。`taskd` は設定検証でこれを要求する
（ADR-0010 D7）。

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
      "required": ["criterion"],
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
    "RoleContext": {
      "description": "v2, ADR-0016 D1/M3. Short excerpt only; the authoritative shape is worker-protocol.schema.json.",
      "type": "object",
      "required": ["id"],
      "properties": {
        "id": {"type": "string"},
        "instructions": {"type": "string"}
      }
    },
    "ChildSummary": {
      "description": "v2, ADR-0016 D3/M4. Short excerpt only; the authoritative shape is worker-protocol.schema.json.",
      "type": "object",
      "required": ["id", "title", "status"],
      "properties": {
        "id": {"type": "string"},
        "title": {"type": "string"},
        "role": {"type": "string"},
        "status": {"type": "string"},
        "outcome": {"type": "string"},
        "artifacts": {"type": "array", "items": {"$ref": "#/$defs/ArtifactRef"}},
        "workspace": {"type": "string"}
      }
    },
    "DelegateTask": {
      "description": "v2, ADR-0016 D2/M7. Short excerpt only; the authoritative shape is worker-protocol.schema.json.",
      "type": "object",
      "required": ["title", "objective", "acceptance"],
      "properties": {
        "title": {"type": "string"},
        "objective": {"type": "string"},
        "acceptance": {"type": "array", "description": "task-core::Criterion[]"},
        "role": {"type": "string"},
        "depends_on": {"type": "array", "description": "each item is an integer index into this tasks array, or a string task id"},
        "tier": {"type": "string"}
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
{"type":"run","protocol":2,"task":{"id":"01J8…","kind":"execute","title":"add README example","acceptance":[{"text":"cargo test exits 0","check":{"command":{"cmd":"cargo test","expect_exit":0}}}],"budget":{"max_turns":20,"max_wall_secs":600,"max_retries":1},"attempts":0},"workspace":"/srv/ws/01J8…","context":{"prior_review":[],"inputs":[]}}
```

stdout:

```json
{"type":"progress","msg":"editing README.md"}
{"type":"artifact","name":"readme.diff","path":"artifacts/readme.diff","kind":"diff"}
{"type":"delegate","tasks":[{"title":"double-check the wording","objective":"proofread the new README section","acceptance":[{"text":"a human approves","check":{"type":"human"}}],"role":"reviewer"}]}
{"type":"progress","msg":"running cargo test"}
{"type":"done","summary":"Added usage example to README","evidence":[{"criterion":0,"command":"cargo test","exit":0,"stdout_tail":"test result: ok. 3 passed"}]}
```

→ taskd: `WorkerProgress` ×2, `ArtifactProduced`, `Event::Delegated{run_id, task_ids}`（§4.6 の検証を通ればそれだけ）,
`WorkerFinished{outcome: done}`, `Transitioned{running→reviewing, reason:"worker_done"}`。

## 9. CLI エージェント系アダプタの結果ファイル規約（Phase 4、ADR-0006 で確定）

`claude-code`・`codex`（および将来の `dsh`）は本文書 §1〜§8 の JSON Lines プロトコルを**話さない**。
`claude` CLI は独自の `stream-json` イベント（`system`/`assistant`/`user`/`result`）を吐くだけであり、
taskd はこれを直接パースできない。そこでこれらのアダプタはプロンプトでワーカー（Claude Code 自身）に
次を指示し、アダプタが作業ディレクトリの `artifacts/result.json` を読んで本文書の `done`/`question` に
相当する終端を合成する（旧 P-13。ADR-0006 D3 で確定。旧 P-11 の `run_id`/`attempt` はスキーマ変更せず
プロンプト文面にのみ埋め込む。旧 P-12 の「evidence を任意化」は ADR-0012 D3 で採用し、`command` / `exit` /
`stdout_tail` を任意にした）:

```json
{"summary": "...", "evidence": []}
```
```json
{"question": "..."}
```

**`memory`（v4, ADR-0033 D6, Phase 24）**: 結果ファイルに `memory` があれば、taskd はその中身を担当ノードの
長期記憶に**日付付きの箇条書きで追記する**（`- 2026-09-17: …` の 1 項目 1 行）。

```json
{"summary": "...", "evidence": [], "memory": {"notes": ["pegasus は pjsub で投げる"], "project": ["Pluvio は非同期ランタイム基盤"]}}
```

- `notes[]` は `<memory_dir>/<node_id>/notes.md`（**案件をまたぐ**記憶: クラスタの使い方、人の好み、直近の相談）、
  `project[]` は `<memory_dir>/<node_id>/projects/<project_id>.md`（この案件だけの事）。
- `memory` が無い・空・形が違う・そもそも `[memory]` を設定していない・タスクに `assignee` が無いときは
  **何もしない**（run は失敗させない）。案件に属さない run の `project[]` は行き先が無いので捨てる。
- 追記は決定的なファイル操作だけで、何を覚えるかを決めるのはワーカー（**LLM に書かせるのはここだけ**）。
  プロンプトの前置きの末尾にその指示が入る。

判定順序（ADR-0006 D4）: stream-json の最後の `{"type":"result",...}` が `is_error:true` か
`subtype != "success"` なら、結果ファイルの内容によらず `error{retryable:true}` とする（自己申告の
`done` は信用しない）。`success` の場合のみ結果ファイルを読み、無い／不正なら `error{retryable:true}`。

`context.answers`（P-10、`question` → `blocked` → `taskctl answer` の回答をワーカーへ渡す経路）は
Phase 7（ADR-0010 D3）で実装した。`claude-code`/`codex` のプロンプトは、空でなければ「以前の質問への
人間の回答」節（`## Answers from a human to your earlier questions`、各回答を `- Q: ...` / `  A: ...`）を
`prior_review` の節の近くに載せる（Execute/Plan プロンプトのみ。Review プロンプトには載せない）。

**`runs/<run_id>/result.json`（P-26, ADR-0010 D10）**: `claude-code`/`codex` も、run の終端（`done`/
`question`/`error`。供給側失敗として分類された `error` の場合は `provider_failure` 付き）を本文書 §4 の
`WorkerMessage` に正規化し、1 行 JSON として `runs/<run_id>/result.json` に書いてから終了する
（`fake`/`run_subprocess` がワーカーから受信した生の行をそのまま書くのと同じ役割）。これは taskd の
再起動後にレビュー対象の `done` 内容を復元するために使われる（ADR-0007 D5）ので、CLI 系アダプタも
同じファイルを同じ形式で書く必要がある。

**`artifacts/delegate.json`（ADR-0016 M8, v2）**: `claude-code`/`codex` は §4.6 の `delegate` メッセージの
プロトコルを話さないので、代わりに作業ディレクトリ直下 `artifacts/delegate.json` を使う。形式は
`{"tasks":[…]}`（`tasks` は §4.6 の `DelegateTask` と同じ形。v4 から `tasks[].assignee`（組織ノードの id）を
書ける。`role` を書かなければそのノードの分野から tier / アダプタ / 予算が決まる。**自分と別の部の課へ
委譲しようとした提案は子を作らず、親の run が秘書への `question` で終わる**。SPEC §3.1 / ADR-0033 D4）。run 開始時（`artifacts/result.json` を消す
のと同じタイミング）に前回の run が残したファイルを消し、run の終わり（終端を決めた直後、`result.json` を
書く前）に存在すれば読んで、§4.6 と同じ検証・挿入の経路に渡す。ファイルが無ければ何もしない。JSON として
読めない場合は run を失敗させず、`WorkerProgress{msg:"delegate.json ignored: <error>"}` を残して無視する。
プロンプトには役割の指示文（`## Role: <id>`）、`artifacts/delegate.json` の書き方の指示、集約 run
（`task.aggregate == true` で子が全て終端になった後の run）なら `## Delegated child tasks` 節（各子を
`title` / `role` / `status` / 直近 run の `outcome` / `workspace` / `artifacts` で列挙し、`artifacts/summary.md`
を書くよう指示）を足す（`task_worker::claude_code::build_prompt` / `codex::run_codex` が共通で使う）。

**エラー文面の分類（供給側失敗。ADR-0010 D5）**: `claude-code`/`codex` はワーカープロトコルの
`provider_failure` フィールドを直接受け取れない（stream-json/JSON Lines の形式が異なるため）。代わりに
エラー文面を決定的な文字列規則（`task_worker::provider::classify_provider_failure`。大文字小文字を無視した
部分一致、LLM を呼ばない）で分類し、`AdapterError::{Throttled, AuthFailed, Exhausted}` として返す
（`run()` は `result.json` を書いた後にこの `Err` を返す。遷移の判断はディスパッチャが行う）:

| 分類 | 判定順 | 一致パターン（部分一致・大小無視） |
|---|---|---|
| `exhausted` | 1 | `usage limit`, `quota`, `credit balance` |
| `throttled`（`retry_after_secs:60` 固定） | 2 | `rate limit`, `rate_limit`, `overloaded`、独立トークンの `429` / `529` |
| `auth_failed` | 3 | `invalid api key`, `authentication`, `not logged in`, `/login`、独立トークンの `401` |

「独立トークン」は前後の文字が英数字・`.` でなく、`:` を挟んで数字が続く位置情報（`:17`、`12:`）の一部でもないこと
（`HTTP 429`、`status=429`、`HTTP 529: too many requests` は一致し、stderr のスタックトレースに含まれる `cli.js:4291:17` や
`file.js:429:17` のような位置情報は一致しない）。プロトコルの `provider_failure.retry_after_secs` は最低 1 秒に切り上げる。

どれにも当たらなければ分類せず、従来どおり `Terminal::Error{retryable:true}`（例:
`The 'gpt-5.4' model is not supported when using Codex with a ChatGPT account.` は分類対象外）。
分類に使う文面は、`claude-code` は `result` メッセージの `result`（文字列。無ければ `subtype`）、
`codex` は `turn.failed.error` のメッセージ。どちらも「`result`/`turn.*` を一度も観測できずに exit した
場合」は代わりに `runs/<run_id>/stderr.log` の末尾（最大 4 KiB）を分類する（`codex` はさらに
`{"type":"error","message":...}` 行を観測していればそれを優先する）。**wall-clock・無出力タイムアウトは
分類しない**（供給側の問題ではなくワーカー側の停止・暴走のため）。

### 9.1 `codex` アダプタ（Phase 6、ADR-0008 D3）

`codex exec --json` も同じ結果ファイル規約（`artifacts/result.json`）を使うが、正常終了の判定に使う
JSON Lines のイベント形が `claude-code` と異なる: `claude-code` の `{"type":"result",...}` の代わりに
`{"type":"turn.completed",...}` / `{"type":"turn.failed","error":...}` を見る。`turn.completed` を一度でも
観測できれば結果ファイルを読み、`turn.failed` はそのまま `error{retryable:true}`（供給側失敗として分類
できればディスパッチャへの `requeue` 経路。上記参照）にする。
`turn.completed`/`turn.failed` のどちらも一度も観測できずに exit した場合はクラッシュとして扱い、
`artifacts/result.json` を一切信用しない（§9 の判定順序と同じ考え方）。`item.*`（`item.started`/
`item.completed` 等）は進捗としてのみ扱い、内容の構造には依存しない（実機の codex-cli 0.154.0 で
`turn.failed.error` がオブジェクト（`{"message":"..."}`）で返ることを確認済み。将来この形が変わっても
読めるよう文字列・オブジェクトの両方を受け付ける）。プロンプトは `claude-code` と共通（`build_prompt`
を再利用）。

## 10. kind 別の出力ファイル（Phase 5、ADR-0007 D1/D5/D7）

§1〜§9 の `run`/`done`/`error`/`question` プロトコル自体は kind によらず同じ（`task.kind` に応じて
`RunRequest.task`/`context` の内容が変わるだけで、メッセージ形式は変更しない）。ただし `Plan` run と
`Review` run では、ワーカーは終端メッセージ（あるいは CLI エージェント系アダプタなら §9 の
`artifacts/result.json`）に加えて、作業ディレクトリ直下に追加のファイルを書く。ディスパッチャ側の
Reviewer（決定的コード。LLM 呼び出しはここには書かない）がそれを読んで判定する。

### 10.1 `Plan` run — `artifacts/plan.json`

`task.kind == "plan"` の run では、ワーカーは分解結果を作業ディレクトリ直下 `artifacts/plan.json` に
`PlanOutput` として書く（正の JSON Schema は隣の `plan-output.schema.json`。`task-core::plan::schema_value()`
から生成し `task-core` のテストで一致を検証する）。概形:

```json
{"tasks":[
  {"title":"...", "objective":"...",
   "acceptance":[{"text":"...", "check":{"type":"command","cmd":"...","expect_exit":0}}],
   "depends_on":[0],
   "kind":"execute",
   "tier":"standard"}
]}
```

検証規則の要約（`task-core::plan::validate`、全て決定的）:

- `tasks` は 1〜20 件（既定。DESIGN §6 の「3〜6 個」は受け入れ条件であって上限規則ではない）
- 各 `title`/`objective` は空でない。`acceptance` は 1 件以上、各 `text` も空でない
- `check` は `command` / `artifact_exists` / `reviewer` / `human` のいずれか（`task-core::Check` の 4 種）
- `depends_on` は同じ `tasks` 配列内のインデックスで、範囲内・自己参照無し・DAG（閉路無し）
- `kind:"plan"` の子は、その Plan 自身を含む祖先 `Plan` の数（`plan_depth`）が `MAX_PLAN_DEPTH`（3）を
  超えない場合のみ許される
- 未知フィールドは拒否（`#[serde(deny_unknown_fields)]`。綴り間違いの検出のため。§2 の「未知フィールドは
  無視する」という一般規則とは意図的に逆）

検証に失敗した場合、`Plan` タスクの `reviewing` は `ReviewFail` になり（`criterion_idx =
task.acceptance.len()`。`taskctl plan` が作る Plan は `acceptance = []` なので常に `criterion 0`）、
次回の `context.prior_review[criterion_idx].reason` に検証エラー文言（例:
`tasks[2].depends_on[0] = 7 is out of range (0..3)`）が載ってリトライされる（`max_retries` 内。DESIGN
§5.6「不正なら1回だけ再試行」）。全 pass なら子タスクの挿入と親のトランザクションが原子的に行われる
（`TaskStore::complete_plan`。ADR-0007 D3）。

### 10.2 `Review` run — `artifacts/review.json`

`Check::Reviewer` 条件を判定する際、ディスパッチャは対象タスクとは別の run を起動する。
`RunRequest.task` は永続化されない合成タスク（`kind = "review"`、`title = "Review: <対象 title>"`、
`objective`/`acceptance`/`workspace`/`budget` は対象タスクと同じ）。`RunRequest.context.review` に
`ReviewRequest{summary, evidence, criteria}` が入る（`criteria` は判定すべき `task.acceptance` の
インデックス一覧、`summary`/`evidence` は対象 run の `done` の内容）。`context.inputs` には対象 run の
`ArtifactProduced` が入る。run は対象タスクと同じ作業ディレクトリで実行される（成果物を直接読めるように
するため。ワーカーには読み取り専用で振る舞うよう指示するが強制はしない。既知の制約）。

ワーカーは判定結果を作業ディレクトリ直下 `artifacts/review.json` に `ReviewOutput` として書く
（`worker-protocol.schema.json` の `review_output` 定義。正）:

```json
{"verdicts":[{"criterion":0,"pass":true,"reason":"cargo test passes and README was updated"}]}
```

`criteria` に列挙された各インデックスについて `verdicts` に必ず 1 件対応するエントリが必要。次はすべて
該当する `Reviewer` 条件を `pass=false`（理由に原因を記録）として扱う: run の終端が `done` 以外
（`error`/`question`/クラッシュ/タイムアウト）、`artifacts/review.json` が無い、JSON として不正、
`criteria` のいずれかのインデックスに対応する `verdicts` エントリが欠落している。

### 10.3 run 開始前のクリーンアップ

リトライで前回 run の出力ファイルを今回の結果と誤読しないよう、ディスパッチャは run 開始前に
（§9 の `artifacts/result.json` と同様に）該当ファイルを削除してから起動する: `Plan` run なら
`artifacts/plan.json`、`Review` run なら `artifacts/review.json`。

### 10.4 例: `fake` アダプタでの kind 分岐

`fake` アダプタ（sh スクリプト。§1〜§8 の JSON Lines を stdin/stdout で読み書きする）は、stdin に来る
`run` 行の `"task":{"kind":"plan", ...}` / `"kind":"review"` を見て分岐できる。例（`sh`, `jq` 前提）:

```sh
#!/bin/sh
line=$(cat)
kind=$(printf '%s' "$line" | jq -r '.task.kind')
case "$kind" in
  plan)
    mkdir -p artifacts
    printf '%s' '{"tasks":[{"title":"a","objective":"do a","acceptance":[{"text":"c","check":{"type":"command","cmd":"true","expect_exit":0}}]}]}' \
      > artifacts/plan.json
    ;;
  review)
    mkdir -p artifacts
    printf '%s' '{"verdicts":[{"criterion":0,"pass":true,"reason":"looks fine"}]}' > artifacts/review.json
    ;;
esac
echo '{"type":"done","summary":"ok","evidence":[]}'
```

（`claude-code` アダプタでの kind 別プロンプトは §9 と同じ「翻訳層」であり、`task-worker::claude_code::
build_prompt` が `task.kind` で分岐する。ADR-0007 D7）
