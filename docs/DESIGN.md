# taskd — タスク管理層 実装方針

対象読者: この文書を渡されたClaude Code。人間（Ryosuke）は方針を決め、実装は本文書に従って進める。
判断に迷ったら本文書の「設計原則」に戻る。原則に反する実装は、たとえ動いても差し戻す。

---

## 0. 目的と位置づけ

研究・研究室インフラ向けの大規模エージェント基盤のうち、**タスク管理層**を実装する。
基盤全体は3層に分離する。本文書のスコープは (2) のみ。

1. 接続層 — 各スパコン／ホストへの接続とコマンド実行（別プロジェクト、MCPサーバとして露出予定）
2. **タスク管理層（本プロジェクト: `taskd`）** — タスクの永続化、状態遷移、ディスパッチ、ワーカー起動、レビュー、承認ゲート
3. モデル供給層 — 複数AIサービス／アカウントの抽象化と予算に応じたルーティング（別プロジェクト）

(1)(3) は後から差し込めるよう、本プロジェクトでは**インタフェース（trait）だけ定義し、ローカル実装とダミー実装のみ持つ**。

参考にする既存設計（コードは流用せず、設計を借りる）:

- OpenAI Symphony (`github.com/openai/symphony` の `SPEC.md`) — チケット状態機械を制御プレーンにする設計、オーケストレータ↔エージェント間プロトコル
- Bernstein — スケジューリングにLLMを一切使わない決定的ディスパッチ、エージェント外に置く状態ディレクトリとリプレイジャーナル
- DeepSeek Harness (dsh) — ヘッドレスCLIを持つワーカー候補の一つ。追記専用セッションログの考え方

---

## 1. 設計原則（最優先）

1. **協調判断にLLMを使わない。** どのタスクを次に走らせるか、どのワーカーに割り当てるか、リトライするかは全て決定的なコードで決める。LLMが介在するのは「プランナー（分解）」「ワーカー（実行）」「レビュアー（検証）」の3役だけ。
2. **状態はエージェントの外に置く。** タスクの真実は SQLite にある。会話の文脈やワーカーのメモリに状態を持たせない。ワーカーはステートレスで、タスクを受け取り、成果物と証拠を返して終了する。
3. **ワーカーは交換可能な電池。** Claude Code / Codex / dsh / ローカルモデル / テスト用フェイクを、同一のJSONプロトコルで叩く。ワーカー固有の事情はアダプタに閉じ込める。
4. **完了はレビューが決める。** ワーカーの「できました」は完了ではない。受け入れ条件に対する証拠をレビュアー（別のLLM実行、または決定的な検証スクリプト）が判定して初めて `done` になる。
5. **人間の関与は承認ゲートとして明示する。** 破壊的操作や研究上の判断は `approval` タスクとしてキューに乗せ、人間は承認待ちキューだけを見ればよい状態を作る。
6. **全イベントを追記専用で記録する。** 状態遷移、ワーカー起動、成果物、レビュー判定は `events` テーブルにイベントソーシングで残す。現在状態はイベントから再構成できること。
7. **最小から始める。** 本文書の Phase 順に実装し、各 Phase の受け入れ条件を満たしてから次に進む。将来必要になりそうな抽象化を先回りして入れない（YAGNI）。ただし Phase 5 で定義する trait 境界だけは最初から守る。

---

## 2. 技術選定

| 項目 | 選定 | 理由 |
|---|---|---|
| 言語 | Rust (stable, edition 2024) | 単一静的バイナリで配布しやすい。接続層（Rust）と共有しやすい |
| 永続化 | SQLite（`rusqlite` bundled） | 単一ファイル、依存ゼロ、ラップトップでも制御プレーンでも同じ |
| 非同期 | `tokio` | サブプロセス管理とタイムアウトのため |
| シリアライズ | `serde` / `serde_json` / `schemars` | ワーカープロトコルとプランナー出力のJSON Schema検証 |
| CLI | `clap` | `taskctl` |
| ログ | `tracing` + JSON出力 | 構造化ログ。`task_id`, `worker_id` をspanに載せる |
| 設定 | TOML (`config/taskd.toml`) | プロバイダ定義、並列度、ワークスペースルート |

禁止: Web UI、ORM、分散DB、メッセージブローカー。Phase 6 までは不要。

---

## 3. リポジトリ構成

```
agent-platform/
├── CLAUDE.md                 # Claude Code向けの短い運用ルール（本文書を参照）
├── docs/
│   ├── DESIGN.md             # 本文書
│   ├── PROGRESS.md           # Phase進捗。各Phase完了時に必ず更新
│   ├── adr/                  # 設計判断の記録 (ADR-0001-*.md ...)
│   └── protocol/
│       └── worker-protocol.md  # §5.3 のJSONプロトコル仕様（schemaも同梱）
├── Cargo.toml                # workspace
├── crates/
│   ├── task-core/            # ドメインモデル、状態機械、イベント、SQLiteストア（純粋ロジック。LLM・プロセス起動なし）
│   ├── task-dispatch/        # 決定的ディスパッチャ、リース管理、リトライ、並列度制御
│   ├── task-worker/          # ワーカープロトコル、アダプタ（fake / claude-code / codex / dsh / openai-compat）
│   ├── taskd/                # デーモン本体（ループ、設定読込、ログ）
│   └── taskctl/              # CLI（add / ls / show / approve / reject / log / replay / plan / worker ...）
├── config/
│   └── taskd.example.toml
└── tests/
    └── e2e/                  # fakeワーカーだけで動くエンドツーエンドテスト
```

---

## 4. ドメインモデル

### 4.1 Task

```rust
pub struct Task {
    pub id: TaskId,                 // ULID
    pub parent_id: Option<TaskId>,
    pub kind: TaskKind,             // Plan | Execute | Review | Approval
    pub title: String,
    pub objective: String,          // 何を達成するか（自然言語）
    pub acceptance: Vec<Criterion>, // 受け入れ条件。各項目は「検証方法」を持つ
    pub inputs: Vec<ArtifactRef>,   // 依存する成果物
    pub depends_on: Vec<TaskId>,    // 先行タスク（DAG）
    pub status: Status,
    pub priority: i32,
    pub worker_hint: WorkerHint,    // 要求能力: Tier { Frontier | Standard | Cheap } と任意のadapter指定
    pub workspace: WorkspaceSpec,   // Local{path} | Remote{cluster, path}（Remoteは未実装、型だけ）
    pub budget: Budget,             // max_turns, max_wall_secs, max_retries
    pub attempts: u32,
    pub lease: Option<Lease>,       // { worker_run_id, expires_at }
    pub created_at, updated_at,
}

pub struct Criterion {
    pub text: String,               // 例: "cargo test が exit 0"
    pub check: Check,               // Command{cmd, expect_exit:0} | ArtifactExists{name} | Reviewer（LLM判定）| Human
}
```

### 4.2 状態機械

```
draft ──(approve/plan accepted)──▶ ready
ready ──(deps満了 & lease取得)────▶ running
running ──(worker done)───────────▶ reviewing
running ──(worker asks/blocked)───▶ blocked
running ──(lease expired/crash)───▶ ready      (attempts+1, max_retries超えで failed)
reviewing ──(all checks pass)─────▶ done
reviewing ──(check fail, retry可)─▶ ready      (レビュー結果を次回の入力に添付)
reviewing ──(check fail, retry不可)▶ failed
blocked ──(human answers)─────────▶ ready
any ──(cancel)────────────────────▶ cancelled
```

- `Approval` kind のタスクは `running` に入らず、`ready` から人間の `taskctl approve` で直接 `done`、`reject` で `failed`。子タスクは親の `Approval` が `done` になるまで `ready` にならない。
- `Plan` kind のタスクはワーカー（プランナー）の出力として**子タスク群のJSON**を返す。スキーマ検証を通れば子タスクを `draft` で挿入し、プランタスク自身は `reviewing` → `done`。子の `draft`→`ready` は設定 `plan.auto_accept` が true なら自動、false なら人間の承認。

遷移は `task-core` 内の純粋関数 `fn transition(state, event) -> Result<state, Invalid>` として実装し、**全遷移を表駆動でテストする**。

### 4.3 Event（追記専用）

```rust
pub enum Event {
    Created{task}, Transitioned{from,to,reason},
    WorkerStarted{run_id, adapter, model}, WorkerProgress{run_id, msg},
    ArtifactProduced{run_id, artifact}, WorkerFinished{run_id, outcome, usage},
    ReviewVerdict{run_id, criterion_idx, pass, reason},
    ApprovalRequested, ApprovalDecided{by, approved, note},
    ProviderThrottled{provider, until},
}
```

`events(task_id, seq, ts, json)` テーブル。現在状態 `tasks` テーブルは派生ビューとして扱い、`taskctl replay` でイベントから再構築できること（Phase 2 の受け入れ条件）。

### 4.4 Artifact

成果物は `workspace/<task_id>/artifacts/` 配下のファイルとして置き、DBには `ArtifactRef{name, path, sha256, kind}` だけ持つ。ログ、diff、ベンチ結果JSON、レポートMarkdownが典型。

---

## 5. コンポーネント

### 5.1 Store (`task-core`)

- `trait TaskStore` と `SqliteStore` 実装。
- 操作: `insert`, `get`, `list(filter)`, `append_event`, `acquire_lease(task_id, worker_run_id, ttl)`（SQLトランザクションで排他）, `release_lease`, `ready_tasks(limit)`（deps全て `done` かつ `status=ready` かつ親Approval充足）。
- マイグレーションは `migrations/NNNN_*.sql` を起動時に適用。

### 5.2 Dispatcher (`task-dispatch`)

決定的ループ。1 tick ごとに:

1. 期限切れリースを回収 → `running`→`ready`（attempts+1）
2. `ready_tasks` を `priority DESC, created_at ASC` で取得
3. 各タスクについて `WorkerHint` と設定のプロバイダ表から**アダプタ×プロバイダ**を選ぶ（§5.5 の `ProviderPolicy`）。並列度上限（全体・プロバイダ別）を超えたら待つ
4. リース取得 → ワーカー起動（非同期）→ `running`
5. ワーカー終了イベントを受けて `reviewing` へ → Reviewer 起動

**LLM呼び出しは一切ここに書かない。** 選択規則は全て設定とコードで表現する。tick間隔は設定（既定 2s）。

### 5.3 Worker protocol (`task-worker`)

オーケストレータ→ワーカーは**サブプロセス起動＋stdin/stdout の JSON Lines**。すべてのアダプタはこの形に正規化する。

```
→ {"type":"run","task":{...Task...},"workspace":"/abs/path","context":{"prior_review":[...],"inputs":[...]}}
← {"type":"progress","msg":"..."}
← {"type":"artifact","name":"bench.json","path":"artifacts/bench.json"}
← {"type":"question","text":"..."}            # → タスクを blocked にする
← {"type":"done","summary":"...","evidence":[{"criterion":0,"command":"cargo test","exit":0,"stdout_tail":"..."}],"usage":{"input_tokens":..,"output_tokens":..}}
← {"type":"error","message":"...","retryable":true}
```

- `evidence` は受け入れ条件ごとに「何を実行して何が出たか」。Reviewer はこれと成果物だけを見る。
- `usage` は取れる範囲で。取れないアダプタは省略可。
- 仕様は `docs/protocol/worker-protocol.md` に JSON Schema 付きで置き、`schemars` で生成した schema とテストで一致を検証する。

### 5.4 Adapters

`trait WorkerAdapter { async fn run(&self, req: RunRequest, sink: EventSink) -> Result<Outcome> }`

| adapter | 実装方針 | Phase |
|---|---|---|
| `fake` | 設定されたスクリプト／固定応答を返す。全テストはこれで動く。ネットワーク不要 | 1 |
| `claude-code` | `claude -p <prompt> --output-format stream-json --verbose` をワークスペースで起動。stream-json をパースして progress/done に変換。プロンプトはタスク・受け入れ条件・証拠の返し方を含むテンプレート。設定ディレクトリ（`CLAUDE_CONFIG_DIR` 等）をプロバイダごとに切替えてアカウントを分離 | 3 |
| `codex` | `codex exec` の非対話モード。同様 | 3 |
| `dsh` | DeepSeek Harness のヘッドレスCLI。同様 | 4（任意） |
| `openai-compat` | OpenAI互換エンドポイント（ローカル vLLM / llama.cpp）に対する最小ループ。ツールは `bash` と `write_file` のみ。Reviewer や軽作業向け | 4 |

アダプタは**ワーカーの生存監視**（wall-clock 上限、無出力タイムアウト）と**強制終了**を必ず実装する。

### 5.5 Provider policy（モデル供給層との境界）

```rust
pub trait ProviderPolicy {
    fn pick(&self, hint: &WorkerHint, now: Instant) -> Option<(AdapterId, ProviderId)>;
    fn report(&mut self, provider: ProviderId, outcome: &ProviderOutcome); // Ok / Throttled{retry_after} / AuthFailed / Exhausted
    fn concurrency_limit(&self, provider: ProviderId) -> usize;
}
```

本プロジェクトでは `StaticPolicy`（設定表の優先順位、Throttled は cooldown まで除外）のみ実装。残量推定やアカウント間ルーティングは供給層側で `ProviderPolicy` を差し替える。

### 5.6 Planner

- `Plan` kind タスクを `Frontier` tier のワーカーに渡す。プロンプトテンプレートは「目標を、独立に検証可能な受け入れ条件を持つ子タスク群に分解し、以下のJSON Schemaで返せ」。
- 出力は `PlanOutput{ tasks: Vec<NewTask> }` を schema 検証。不正なら1回だけ再試行、それでも不正なら `failed`。
- 分解の深さは上限 3。子が `Plan` を含んでよいが、深さ超過は拒否。

### 5.7 Reviewer

`reviewing` に入ったタスクの各 `Criterion` を `Check` 種別に応じて判定:

- `Command` — ワークスペースで実際に再実行する（ワーカーの自己申告を信じない）
- `ArtifactExists` — ファイル存在と sha256 記録
- `Reviewer` — `Standard` tier のワーカーに「受け入れ条件・証拠・成果物」を渡し `{"pass":bool,"reason":str}` を返させる。ワーカーとは**別プロセス、別プロンプト**
- `Human` — `Approval` 子タスクを生成して待つ

全部 pass で `done`。fail は理由をイベントに残し、`max_retries` 内なら `ready` に戻す（次回の `context.prior_review` に理由を添付）。

### 5.8 Workspace（接続層との境界）

```rust
pub trait Workspace {
    async fn prepare(&self, task: &Task) -> Result<PathBuf>;   // ディレクトリ作成、inputs配置
    async fn exec(&self, cmd: &str, timeout: Duration) -> Result<ExecResult>;
    async fn collect(&self, task: &Task) -> Result<Vec<ArtifactRef>>;
}
```

本プロジェクトは `LocalWorkspace` のみ。`RemoteWorkspace{cluster}` は型と `unimplemented!()` の骨組みだけ置き、接続層プロジェクトが実装する。

### 5.9 CLI (`taskctl`)

```
taskctl add   --title --objective --accept "cargo test exits 0" [--parent] [--tier] [--kind]
taskctl plan  "<大目標>"                 # Planタスクを1つ作る
taskctl ls    [--status ready|running|...] [--tree]
taskctl show  <id>                       # 状態、条件、証拠、直近イベント
taskctl approve|reject <id> [--note]
taskctl answer <id> "<回答>"             # blocked解除
taskctl log   <id> [--follow]
taskctl replay                          # eventsからtasksを再構築し差分を報告
taskctl worker run --adapter fake --task <id>   # アダプタ単体実行（デバッグ用）
```

---

## 6. 実装フェーズと受け入れ条件

各 Phase は独立した `/goal` で回す。受け入れ条件は**コマンド出力で示せる形**にしてある。
Phase 完了時に必ず: `cargo test --workspace` と `cargo clippy --workspace -- -D warnings` が通る、`docs/PROGRESS.md` に完了日と証拠コマンド出力の要約を記す、`git commit` する。

### Phase 0 — 調査とADR（実装なし）

- Symphony の `SPEC.md`、dsh のヘッドレスCLIとサブエージェント委譲、Claude Code の `-p --output-format stream-json` の出力形式を読む
- `docs/adr/0001-scope-and-principles.md`, `0002-state-machine.md`, `0003-worker-protocol.md` を書く。本文書と矛盾する点があれば ADR に「本文書の修正提案」として書き、実装で勝手に変えない
- 受け入れ: 3つの ADR が存在し、`docs/protocol/worker-protocol.md` の初版がある

### Phase 1 — task-core（状態機械とストア）

- Task / Event / Criterion 型、`transition` 純粋関数、`SqliteStore`、マイグレーション
- 受け入れ: 状態遷移表の全セル（有効・無効）を網羅するテストが通る。`acquire_lease` の並行テスト（2スレッドで同時取得し1つだけ成功）が通る

### Phase 2 — taskctl と replay

- `add / ls / show / approve / reject / answer / log / replay`
- 受け入れ: `taskctl add` → `approve` → `show` が期待どおり。`taskctl replay` がイベントから再構築した状態と `tasks` テーブルが一致（差分ゼロ）

### Phase 3 — fake ワーカーとディスパッチャ

- ワーカープロトコル、`fake` アダプタ、`StaticPolicy`、ディスパッチャ、`LocalWorkspace`、Reviewer（`Command` と `ArtifactExists` のみ）
- 受け入れ: `tests/e2e` で「3タスク（うち1つは依存あり）を `taskd` が並列度2で処理し、全て `done`」「`Command` チェックが失敗したタスクが1回リトライされ2回目で `done`」「リース期限切れタスクが回収される」の3シナリオが通る。ネットワーク不要

### Phase 4 — claude-code アダプタとドッグフーディング

- `claude-code` アダプタ（stream-json パース、プロンプトテンプレート、タイムアウト、強制終了）
- 受け入れ: サンプルリポジトリ（本リポジトリ内 `examples/hello-crate`）に対し「`README.md` に使用例を追記し `cargo test` が通る」タスクが実際の Claude Code で `done` になる。証拠にコマンド出力が残る。※この Phase だけAPI/認証が必要。人間が `taskd` を起動して確認する

### Phase 5 — Planner と Reviewer（LLM）

- `Plan` kind、`PlanOutput` schema 検証、`Reviewer` check 種別
- 受け入れ: `taskctl plan "examples/hello-crate に CLI 引数パースを追加し、テストとREADMEを整備"` が 3〜6 個の子タスクを生成し、`plan.auto_accept=false` で人間承認後に全て `done` になる（fake ワーカーで再現可能なテストも用意し、LLM込みの確認は人間が行う）

### Phase 6 — 承認ゲートと codex アダプタ

- `Approval` kind の親子ブロック、`Human` check、`codex` アダプタ
- 受け入れ: 承認前に子が `ready` にならないこと、`reject` で子が `cancelled` になることのテスト。`codex` アダプタで Phase 4 と同じドッグフードタスクが通る

### 非目標（本プロジェクトではやらない）

Web UI、リモートワークスペースの実装、予算・残量推定、複数アカウントの自動切替、マルチユーザ、通知。これらは接続層・供給層の担当。

---

## 7. コーディング規約と運用ルール

- 1 Phase = 1 ブランチ = 1 PR 相当。Phase をまたいでファイルを大改造しない
- テストは `fake` アダプタとローカルSQLiteだけで完結させる。テストで外部ネットワークに出ない
- `unwrap()` はテスト以外禁止。エラーは `thiserror` で型付け
- 設計判断（ライブラリ選定、スキーマ変更、プロトコル変更）は必ず `docs/adr/` に1ファイル追加してから実装
- 本文書に書かれていないことを「たぶん必要」で足さない。必要だと思ったら `docs/PROGRESS.md` の「提案」節に書いて先に進む
- 詰まって同じアプローチを3回失敗したら、方針を変える前に `docs/PROGRESS.md` に状況を書き、`question` として人間に投げる（`/goal` 中でも文章で明示すること）
- 各 Phase の完了報告には、受け入れ条件ごとに「実行したコマンド」と「出力の要点」を必ず本文に含める（`/goal` の評価器はトランスクリプトしか見ない）