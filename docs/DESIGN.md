# taskd — タスク管理層 実装方針

対象読者: この文書を渡されたClaude Code。人間（Ryosuke）は方針を決め、実装は本文書に従って進める。
判断に迷ったら本文書の「設計原則」に戻る。原則に反する実装は、たとえ動いても差し戻す。

改訂履歴:
- 初版（Phase 0〜6 の基準）
- 2026-09-14 改訂: Phase 0〜6 で積み上がった提案 P-1〜P-37 の採否を反映し、Phase 7（仕上げ）を追加（ADR-0009）。
  採用した提案の番号を本文中に `（P-n）` で示す

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

補助クレート（P-2、ADR-0001 D3）: `ulid`（ID）、`time`（RFC3339）、`sha2`（成果物ハッシュ）、`thiserror`（エラー型）、
`toml`、`nix`（プロセスグループへのシグナル）、`async-trait`。テスト用に `tempfile`。

禁止: Web UI、ORM、分散DB、メッセージブローカー。

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
│       ├── worker-protocol.md          # §5.3 のJSONプロトコル仕様
│       ├── worker-protocol.schema.json # schemars 生成（正）
│       └── plan-output.schema.json     # §5.6 の PlanOutput（schemars 生成）
├── Cargo.toml                # workspace
├── crates/
│   ├── task-core/            # ドメインモデル、状態機械、イベント、SQLiteストア（純粋ロジック。LLM・プロセス起動なし）
│   ├── task-dispatch/        # 決定的ディスパッチャ、リース管理、リトライ、並列度制御、Reviewer
│   ├── task-worker/          # ワーカープロトコル、アダプタ（fake / claude-code / codex / dsh / openai-compat）、Workspace
│   ├── taskd/                # デーモン本体（ループ、設定読込、ログ）
│   └── taskctl/              # CLI（add / ls / show / approve / reject / cancel / answer / log / replay / plan ...）
├── config/
│   └── taskd.example.toml
├── examples/
│   └── hello-crate/          # ドッグフード対象のサンプルクレート
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
running ──(worker error)──────────▶ ready | failed  (attempts+1、retryable でなければ failed)            (P-8)
running ──(lease expired/crash)───▶ ready      (attempts+1, max_retries超えで failed)
running ──(供給側失敗: requeue)───▶ ready      (attempts 据え置き。プロバイダは cooldown)           (P-21)
reviewing ──(all checks pass)─────▶ done
reviewing ──(check fail, retry可)─▶ ready      (レビュー結果を次回の入力に添付)
reviewing ──(check fail, retry不可)▶ failed
blocked ──(human answers)─────────▶ ready      (回答は次回の context.answers に載る)                (P-10)
非終端 ──(cancel)─────────────────▶ cancelled  (done / failed / cancelled からは無効)               (P-4)
非終端 ──(dependency failed)──────▶ cancelled  (depends_on の先が failed / cancelled になったとき)  (P-9)
```

- `attempts` は「成功で終わらなかった実行の回数」。`worker_error` / `lease_expired` / `review_fail` だけが増やす。`requeue` / `answer` / `cancel` / `dependency_failed` は増やさない（ADR-0002 D3、ADR-0010 D1）。
- `Approval` kind のタスクは `running` に入らず、`ready` から人間の `taskctl approve` で直接 `done`、`reject` で `failed`。親の `Approval` が `done` でない子は、`status` が `ready` でも **dispatch されない**（`ready_tasks` に現れない。P-6）。
- 終端化に伴う伝播は、元の遷移と**同一トランザクション**で行う（ADR-0010 D2）:
  - `Approval` が `failed`（reject）または `cancelled` → 終端でない直接の子を `cancelled`
  - `Approval` 以外のタスクが終端 → 終端でない直接の `Approval` 子（Human check 用）を `cancelled`（P-37）
  - タスクが `failed` / `cancelled` → 終端でない後続（`depends_on` に含むタスク）を `cancelled`（reason `dependency_failed`、推移的。P-9）
- `Plan` kind のタスクはワーカー（プランナー）の出力として**子タスク群のJSON**を返す。スキーマ検証を通れば子タスクを `draft` で挿入し、プランタスク自身は `reviewing` → `done`。子の `draft`→`ready` は設定 `plan.auto_accept` が true なら自動、false なら人間の承認。

遷移は `task-core` 内の純粋関数 `fn transition(state, trigger) -> Result<outcome, Invalid>` として実装し、**全遷移を表駆動でテストする**。
入力は追記ログの `Event` ではなく `Trigger`（Accept / Dispatch / WorkerDone / WorkerQuestion / WorkerError{retryable} / LeaseExpired / Requeue / ReviewPass / ReviewFail / Answer / Approve / Reject / Cancel / DependencyFailed）とする（ADR-0002 D2）。

### 4.3 Event（追記専用）

```rust
pub enum Event {
    Created{task}, Transitioned{from,to,reason},
    WorkerStarted{run_id, adapter, model}, WorkerProgress{run_id, msg},
    ArtifactProduced{run_id, artifact}, WorkerFinished{run_id, outcome, usage},
    ReviewVerdict{run_id, criterion_idx, pass, reason},
    ApprovalRequested, ApprovalDecided{by, approved, note},
    Answered{question, answer},                                   // P-10
    ProviderThrottled{provider, until},
}
```

`events(task_id, seq, ts, json)` テーブル。現在状態 `tasks` テーブルは派生ビューとして扱い、`taskctl replay` でイベントから再構築できること（Phase 2 の受け入れ条件）。
`Transitioned.reason` は必ずトリガの機械可読名（`"review_fail"` など）で、`replay` はこれで `attempts` を復元する。

### 4.4 Artifact

成果物は `<作業ディレクトリ>/artifacts/` 配下のファイルとして置き、DBには `ArtifactRef{name, path, sha256, kind}` だけ持つ。ログ、diff、ベンチ結果JSON、レポートMarkdownが典型。
作業ディレクトリは `WorkspaceSpec::Local{path}` の `path`（相対なら `workspace_root` 基準）。`taskctl add/plan` で省略した場合は `<workspace_root>/<task_id>/`（P-19）。run ごとの生ログは `runs/<run_id>/{stdout.jsonl, stderr.log, result.json}`。

---

## 5. コンポーネント

### 5.1 Store (`task-core`)

- `trait TaskStore` と `SqliteStore` 実装。
- 操作: `insert`, `create_task(task, extra_events)`（insert と `Created` 等を 1 トランザクション）, `get`, `list(filter)`, `append_event`, `events_for(task_id)`（P-15）,
  `acquire_lease(task_id, worker_run_id, ttl)`（SQLトランザクションで排他）, `release_lease`, `renew_lease(task_id, worker_run_id, ttl)`（P-7）,
  `ready_tasks(limit)`（deps全て `done` かつ `status=ready` かつ親Approval充足、`kind != approval`。P-36）,
  `apply_transition(_with_events)(task_id, trigger, events)`, `complete_plan(plan_id, verdicts, children, accept_children)`。
- **状態を変更する操作は、変更後の `Event::Transitioned` と関連イベント（`WorkerFinished`, `ReviewVerdict`, `ApprovalDecided`, `Answered` …）の追記、および §4.2 の伝播まで含めて同一トランザクションで行う**（P-16）。
- マイグレーションは `migrations/NNNN_*.sql` を起動時に適用。

### 5.2 Dispatcher (`task-dispatch`)

決定的ループ。1 tick ごとに:

1. 終了したワーカー／レビューの結果を取り込み、状態遷移をストアに書く
2. 期限切れリースを回収 → `running`→`ready`（attempts+1）
3. `ready_tasks` を `priority DESC, created_at ASC` で取得。`attempts > 0` のタスクは `updated_at + min(base·2^(attempts-1), cap)` まで見送る（P-3）
4. 各タスクについて `WorkerHint` と設定のプロバイダ表から**アダプタ×プロバイダ**を選ぶ（§5.5 の `ProviderPolicy`）。並列度上限（全体・プロバイダ別）を超えたら待つ
5. リース取得 → ワーカー起動（非同期）→ `running`。ワーカーの出力がある間はリースを延長する（P-7）
6. ワーカー終了イベントを受けて `reviewing` へ → Reviewer 起動。供給側失敗（レート制限・認証失敗・枯渇・起動失敗）は `requeue` にしてプロバイダを cooldown にする（P-21）

**LLM呼び出しは一切ここに書かない。** 選択規則は全て設定とコードで表現する。tick間隔は設定 `tick_ms`（既定 2000。P-22）。

### 5.3 Worker protocol (`task-worker`)

オーケストレータ→ワーカーは**サブプロセス起動＋stdin/stdout の JSON Lines**。すべてのアダプタはこの形に正規化する。

```
→ {"type":"run","protocol":1,"task":{...Task...},"workspace":"/abs/path","context":{"prior_review":[...],"inputs":[...],"answers":[...],"review":{...}}}
← {"type":"progress","msg":"..."}
← {"type":"artifact","name":"bench.json","path":"artifacts/bench.json"}
← {"type":"question","text":"..."}            # → タスクを blocked にする
← {"type":"done","summary":"...","evidence":[{"criterion":0,"command":"cargo test","exit":0,"stdout_tail":"..."}],"usage":{"input_tokens":..,"output_tokens":..}}
← {"type":"error","message":"...","retryable":true,"provider_failure":{"kind":"throttled","retry_after_secs":60}}
```

- `evidence` は受け入れ条件ごとに「何を実行して何が出たか」。Reviewer はこれと成果物だけを見る。
- `usage` は取れる範囲で。取れないアダプタは省略可。
- `context.answers`（P-10）は `blocked` から人間が `taskctl answer` した回答の履歴 `[{question, answer}]`。
- `context.review`（P-27）は `Reviewer` check の run でのみ付く `{summary, evidence, criteria}`。
- `error.provider_failure`（任意、P-21）は供給側の失敗を示し、付いていれば `requeue` になる。
- `run_id` と試行回数は CLI 系アダプタのプロンプト文面に埋め込む（P-11）。wire スキーマには入れない。
- 仕様は `docs/protocol/worker-protocol.md` に置き、`schemars` で生成した schema（`worker-protocol.schema.json`）とテストで一致を検証する。

### 5.4 Adapters

`trait WorkerAdapter { async fn run(&self, req: RunRequest, run_id, limits, sink: &dyn EventSink) -> Result<RunOutcome, AdapterError> }`

| adapter | 実装方針 | Phase |
|---|---|---|
| `fake` | 設定されたコマンドをサブプロセスとして起動し、プロトコルをそのまま話す。全テストはこれで動く。ネットワーク不要 | 3 |
| `claude-code` | `claude -p <prompt> --output-format stream-json --verbose` をワークスペースで起動。stream-json をパースして progress に変換。**終端は結果ファイル規約で決める**: ワーカーは `artifacts/result.json` に `{"summary","evidence"}` または `{"question"}` を書き、アダプタは stream-json の `result` メッセージ（error subtype が優先）とこのファイルから `done`/`question`/`error` を合成する（P-13/P-24）。設定ディレクトリ（`CLAUDE_CONFIG_DIR` 等）をプロバイダごとに切替えてアカウントを分離 | 4 |
| `codex` | `codex exec --json` の非対話モード。`turn.completed`/`turn.failed` を終端シグナルとし、結果ファイル規約は claude-code と同じ | 6 |
| `dsh` | DeepSeek Harness。`--profile headless` は最終回答テキストしか返さないため、構造化された進捗は `--profile sdk`（JSON-RPC）かセッション JSONL から取る。終端は結果ファイル規約（P-14） | 任意 |
| `openai-compat` | OpenAI互換エンドポイント（ローカル vLLM / llama.cpp）に対する最小ループ。ツールは `bash` と `write_file` のみ。Reviewer や軽作業向け | 任意 |

- アダプタは**ワーカーの生存監視**（wall-clock 上限、無出力タイムアウト）と**強制終了**を必ず実装する。
- アダプタは終端メッセージを正規化して `runs/<run_id>/result.json` に書く（P-26）。
- アダプタは供給側の失敗（レート制限・認証失敗・枯渇）を、エラー文面の決定的な規則で `AdapterError` として分類して返す（P-21）。遷移の判断はディスパッチャが行う。

### 5.5 Provider policy（モデル供給層との境界）

```rust
pub trait ProviderPolicy {
    fn pick(&self, hint: &WorkerHint, now: Instant) -> Option<(AdapterId, ProviderId)>;
    fn report(&mut self, provider: ProviderId, outcome: &ProviderOutcome); // Ok / Throttled{retry_after} / AuthFailed / Exhausted
    fn concurrency_limit(&self, provider: ProviderId) -> usize;
}
```

本プロジェクトでは `StaticPolicy`（設定表の優先順位、Throttled は cooldown まで除外）のみ実装。残量推定やアカウント間ルーティングは供給層側で `ProviderPolicy` を差し替える。
（trait の拡張提案 P-20 / P-33 は供給層の担当として見送り。）

### 5.6 Planner

- `Plan` kind タスクを `Frontier` tier のワーカーに渡す。プロンプトテンプレートは「目標を、独立に検証可能な受け入れ条件を持つ子タスク群に分解し、以下のJSON Schemaで返せ」。
- 出力はワーカーが `artifacts/plan.json` に書く `PlanOutput{ tasks: Vec<NewTask> }`（P-28）を schema 検証。不正なら1回だけ再試行、それでも不正なら `failed`。
- 分解の深さは上限 3。子が `Plan` を含んでよいが、深さ超過は拒否。
- 子タスクは親の `workspace` / `budget` / `priority` / adapter 指定を継承する（P-31）。

### 5.7 Reviewer

`reviewing` に入ったタスクの各 `Criterion` を `Check` 種別に応じて判定:

- `Command` — ワークスペースで実際に再実行する（ワーカーの自己申告を信じない）
- `ArtifactExists` — ファイル存在と sha256 記録
- `Reviewer` — 設定 `[reviewer]`（adapter 任意、tier 既定 `Standard`。P-30）のワーカーに「受け入れ条件・証拠・成果物」を渡し、`artifacts/review.json` に `{"verdicts":[{"criterion","pass","reason"}]}` を書かせる。ワーカーとは**別プロセス、別プロンプト**。決定的な条件が 1 つでも fail なら実行しない。レビュー run の供給側失敗は fail にせず延期する（P-29）
- `Human` — `Approval` 子タスクを生成して待つ。子は**レビューの試行ごとに新しく作る**（P-35）。未決の間はレビュー全体を延期し、attempts を消費しない

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
taskctl add   --title --objective [--accept "<人間が確認する条件>"]... [--check-cmd "<cmd>"]... [--check-artifact <name>]... [--check-reviewer "<text>"]...
              [--parent] [--depends-on]... [--tier] [--kind] [--workspace]     # 条件は最低1つ。--workspace 省略時は <workspace_root>/<task_id>/
taskctl plan  "<大目標>" [--workspace]      # Planタスクを1つ作る
taskctl ls    [--status ready|running|...] [--tree]
taskctl show  <id>                       # 状態、条件、証拠、直近イベント
taskctl approve|reject <id> [--note]     # draft の approve は accept。reject は Approval のみ（P-5 不採用）
taskctl cancel <id>                      # 非終端のみ（P-18）
taskctl answer <id> "<回答>"             # blocked解除。回答は Answered イベントとして残り次回 run に渡る
taskctl log   <id> [--follow]
taskctl replay                          # eventsからtasksを再構築し差分を報告
taskctl worker run --adapter fake --task <id>   # アダプタ単体実行（デバッグ用。未実装）
```

出力を閉じたパイプ（`| head` 等）に流しても異常終了しないこと。

---

## 6. 実装フェーズと受け入れ条件

各 Phase は独立した `/goal` で回す。受け入れ条件は**コマンド出力で示せる形**にしてある。
Phase 完了時に必ず: `cargo test --workspace` と `cargo clippy --workspace -- -D warnings` が通る、`docs/PROGRESS.md` に完了日と証拠コマンド出力の要約を記す、`git commit` する。
（Phase 0 は実装が無いため cargo コマンドの条件は適用しない。P-1）

LLM を使う実機確認は、認証が使える環境ならエージェントが実行して証跡を `PROGRESS.md` に残してよい。使えない場合は手順を書いて人間に依頼する（P-34）。

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
- 受け入れ: `tests/e2e` で「3タスク（うち1つは依存あり）を `taskd` が並列度2で処理し、全て `done`」「`Command` チェックが失敗したタスクが1回リトライされ2回目で `done`」「リース期限切れタスクが回収される」の3シナリオが通り、いずれも `taskctl replay` が差分ゼロ（P-23）。ネットワーク不要

### Phase 4 — claude-code アダプタとドッグフーディング

- `claude-code` アダプタ（stream-json パース、プロンプトテンプレート、タイムアウト、強制終了）
- 受け入れ: サンプルリポジトリ（本リポジトリ内 `examples/hello-crate`）に対し「`README.md` に使用例を追記し `cargo test` が通る」タスクが実際の Claude Code で `done` になる。証拠にコマンド出力が残る。※API/認証が必要

### Phase 5 — Planner と Reviewer（LLM）

- `Plan` kind、`PlanOutput` schema 検証、`Reviewer` check 種別
- 受け入れ: `taskctl plan "examples/hello-crate に CLI 引数パースを追加し、テストとREADMEを整備"` が 3〜6 個の子タスクを生成し、`plan.auto_accept=false` で人間承認後に全て `done` になる（fake ワーカーで再現可能なテストも用意し、LLM込みの確認も行う）。fake のテストでは「不正な plan が 1 回リトライされる」ことと `taskctl replay` 差分ゼロも確認する（P-32）

### Phase 6 — 承認ゲートと codex アダプタ

- `Approval` kind の親子ブロック、`Human` check、`codex` アダプタ
- 受け入れ: 承認前に子が `ready_tasks()` に現れず dispatch されないこと（P-6）、`reject` で子が `cancelled` になることのテスト。`codex` アダプタで Phase 4 と同じドッグフードタスクが通る（※利用可能なアカウント・モデルがある場合。無い場合は ADR に外部制約として記録する。ADR-0009）

### Phase 7 — 仕上げ（人間とのやりとり、取り消しと伝播、供給側失敗、運用上の負債）

- `context.answers`（P-10）、`taskctl add --check-*`（P-17）、`taskctl cancel` と非終端限定の cancel（P-18/P-4）、依存先失敗の伝播（P-9）、
  Human check の試行ごとの承認と孤児 Approval の cancel（P-35/P-37）、`ready_tasks` からの Approval 除外（P-36）、
  `Requeue`（P-21/P-29）、リトライのバックオフ（P-3）、`renew_lease`（P-7）、workspace 既定（P-19）、`[reviewer]` 設定（P-30）、
  CLI 系アダプタの `runs/<run_id>/result.json`（P-26）、ETXTBSY のテスト側対処、`create_task` による原子的な挿入、出力パイプ対応
- 受け入れ（すべて fake ワーカーとローカル SQLite で再現し、`taskctl replay` 差分ゼロを併せて確認する）:
  1. `question` で `blocked` になったタスクに `taskctl answer` すると、次の run の stdin の `context.answers` に質問と回答が載り `done` になる
  2. `taskctl add --check-cmd ... --check-artifact ...` だけで作ったタスクが `taskd` で `done` になる
  3. `taskctl cancel` は非終端タスクだけを `cancelled` にし、終端タスクには exit 1。先行タスクが `failed` になると後続が推移的に `cancelled`（reason `dependency_failed`）
  4. Human check を持つタスクが、別条件の fail で再レビューされるとき新しい `Approval` 子が作られる。親が終端になると未決の `Approval` 子が `cancelled`。`ready_tasks` は `Approval` を返さない
  5. `provider_failure` 付きの `error` を返すワーカーは attempts を消費せず `requeue` され、cooldown 後に `done`。Reviewer run の供給側失敗で `ReviewFail` にならない
  6. バックオフ期間中の再 dispatch が起きないこと、ワーカーの出力でリースが延長されること（ユニットテスト）
  7. `taskctl ls | head -n 1` が panic せず成功する。`spawn_retrying` を持たない状態で `cargo test --workspace` を連続 5 回実行して全て成功する

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
- LLM を使う実機確認は、認証が使える環境ならエージェントが実行してよい（証跡を `PROGRESS.md` に残す。P-34）
