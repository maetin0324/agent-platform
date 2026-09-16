# taskd — タスク管理層 実装方針

対象読者: この文書を渡されたClaude Code。人間（Ryosuke）は方針を決め、実装は本文書に従って進める。
判断に迷ったら本文書の「設計原則」に戻る。原則に反する実装は、たとえ動いても差し戻す。

改訂履歴:
- 初版（Phase 0〜6 の基準）
- 2026-09-14 改訂: Phase 0〜6 で積み上がった提案 P-1〜P-37 の採否を反映し、Phase 7（仕上げ）を追加（ADR-0009）。
  採用した提案の番号を本文中に `（P-n）` で示す
- 2026-09-14 改訂 2: 連続 requeue の上限（P-40, ADR-0011）と、複数アカウント運用・evidence の任意化・`taskctl worker run`
  （P-41, ADR-0012）を反映し、Phase 8 を追加（いずれも人間の許可による）
- 2026-09-14 改訂 3: Web GUI（`gui/`。当時は別プロジェクト `taskd-gui`。2026-09-16 に ADR-0020 で同じリポジトリへ）のために、taskd の HTTP API 層（§5.10）と基盤（`task-ops`、SQLite の WAL と
  スキーマ版数、events のグローバル id）を追加し、Phase 9 を追加（ADR-0013。人間の方針「GUI のために taskd の変更が必要なら変更する」による）

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

API 層（§5.10）: `axum`（HTTP/JSON + SSE）。taskd のプロセス内で `[api]` 設定時だけ動く。

禁止: Web UI（`gui/` の別プロセス・別言語。taskd の Rust 側は §5.10 の HTTP API までを提供する。同じリポジトリでも依存は作らない。ADR-0020）、ORM、分散DB、メッセージブローカー。

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
│   ├── task-ops/             # 人間の操作（approve / reject / answer / cancel / add / plan / replay）の判断と検証、イベントからの派生値
│   ├── task-api/             # HTTP/JSON + SSE の API 層（§5.10。GUI の BFF と curl 向け。協調判断はしない）
│   ├── taskd/                # デーモン本体（ループ、設定読込、ログ）
│   └── taskctl/              # CLI（add / ls / show / approve / reject / cancel / answer / log / replay / plan ...）
├── config/
│   ├── taskd.example.toml
│   └── taskd.multi-account.example.toml   # 複数アカウント運用の例（§5.4）
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
    pub role: Option<String>,       // ADR-0016: 役割（[[roles]] の既定と指示文を引く。状態機械は見ない）
    pub aggregate: bool,            // ADR-0016: 子が全て終端になった後に集約 run を 1 回だけ行う
    pub workspace: WorkspaceSpec,   // Local{path} | Remote{cluster, path}（Remote = クラスタでコマンドを実行。ADR-0018）
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
running ──(供給側失敗: requeue)───▶ ready      (attempts 据え置き。プロバイダは cooldown。同じ試行で max_requeues 回まで) (P-21/P-40)
reviewing ──(all checks pass)─────▶ done
reviewing ──(check fail, retry可)─▶ ready      (レビュー結果を次回の入力に添付)
reviewing ──(check fail, retry不可)▶ failed
blocked ──(human answers)─────────▶ ready      (回答は次回の context.answers に載る)                (P-10)
非終端 ──(cancel)─────────────────▶ cancelled  (done / failed / cancelled からは無効)               (P-4)
非終端 ──(dependency failed)──────▶ cancelled  (depends_on の先が failed / cancelled になったとき)  (P-9)
```

- `attempts` は「成功で終わらなかった実行の回数」。`worker_error` / `lease_expired` / `review_fail` だけが増やす。`requeue` / `answer` / `cancel` / `dependency_failed` は増やさない（ADR-0002 D3、ADR-0010 D1）。
  ただし同じ試行での連続 `requeue` が設定 `max_requeues`（既定 5、0 で requeue しない）に達した後の供給側失敗は、通常の `worker_error`（Reviewer run では `review_fail`）として attempts を消費する。回数は `events` から数える（P-40、ADR-0011）。
- `Approval` kind のタスクは `running` に入らず、`ready` から人間の `taskctl approve` で直接 `done`、`reject` で `failed`。親の `Approval` が `done` でない子は、`status` が `ready` でも **dispatch されない**（`ready_tasks` に現れない。P-6）。
- 終端化に伴う伝播は、元の遷移と**同一トランザクション**で行う（ADR-0010 D2）:
  - `Approval` が `failed`（reject）または `cancelled` → 終端でない直接の子を `cancelled`
  - `Approval` 以外のタスクが終端 → 終端でない直接の `Approval` 子（Human check 用）を `cancelled`（P-37）
  - タスクが `failed` / `cancelled` → 終端でない後続（`depends_on` に含むタスク）を `cancelled`（reason `dependency_failed`、推移的。P-9）
- `Plan` kind のタスクはワーカー（プランナー）の出力として**子タスク群のJSON**を返す。スキーマ検証を通れば子タスクを `draft` で挿入し、プランタスク自身は `reviewing` → `done`。子の `draft`→`ready` は設定 `plan.auto_accept` が true なら自動、false なら人間の承認。

遷移は `task-core` 内の純粋関数 `fn transition(state, trigger) -> Result<outcome, Invalid>` として実装し、**全遷移を表駆動でテストする**。
入力は追記ログの `Event` ではなく `Trigger`（Accept / Dispatch / WorkerDone / WorkerQuestion / WorkerError{retryable} / LeaseExpired / Requeue / ReviewPass / ReviewFail / Answer / Approve / Reject / Cancel / DependencyFailed / Aggregate / ChildFailed）とする（ADR-0002 D2、ADR-0016 M1、ADR-0021 D1）。
`Aggregate` は `reviewing → ready`（attempts 据え置き、reason `"aggregate"`）で、`aggregate = true` の親が子の完了後に集約 run を 1 回だけ行うために使う。
`ChildFailed` は委譲した子が失敗した親に使い、やり直せるなら `reviewing → ready`（attempts 消費）、やり直せないなら
`reviewing → blocked`（attempts 据え置き、人の判断待ち）。**親を `failed` にはしない**（ADR-0021）。

### 4.3 Event（追記専用）

```rust
pub enum Event {
    Created{task}, Transitioned{from,to,reason},
    WorkerStarted{run_id, adapter, model, provider?, role?, task_role?},  // provider = アカウント ID（P-41）、role = worker | reviewer（P-45）、task_role = タスクの役割（ADR-0016）
    WorkerProgress{run_id, msg},
    ArtifactProduced{run_id, artifact}, WorkerFinished{run_id, outcome, usage, role?},
    ReviewVerdict{run_id, criterion_idx, pass, reason},
    ApprovalRequested, ApprovalDecided{by, approved, note},
    Answered{question, answer},                                   // P-10
    ProviderThrottled{provider, until, reason?},                   // reason = throttled | auth_failed | exhausted | spawn
    ClusterUnavailable{cluster, host, reason},                     // ADR-0018: ssh の多重接続が無く、人のログイン待ち
    Delegated{run_id, task_ids},                                   // ADR-0016: run 中に提案された子タスクの挿入
    QuestionRaised{run_id, text},                                  // ADR-0021: ディスパッチャが人に出した質問（run の終了ではない）
}
```

`events(task_id, seq, ts, json)` テーブル。現在状態 `tasks` テーブルは派生ビューとして扱い、`taskctl replay` でイベントから再構築できること（Phase 2 の受け入れ条件）。
`Transitioned.reason` は必ずトリガの機械可読名（`"review_fail"` など）で、`replay` はこれで `attempts` を復元する。
`role` は省略時がワーカー run（既存のイベントと同じ直列化）。`Reviewer` run も `WorkerStarted` / `WorkerFinished` を対象タスクに残し、
アカウント別の使用量の集計に含める（P-45、ADR-0014 D1）。ワーカー run を前提にする派生値（直近 run、質問文、受信箱）は `role` で Reviewer run を除く。

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
- **書き込みトランザクションは `BEGIN IMMEDIATE` で始める**（P-42、ADR-0015）。WAL では、読んでから書くトランザクションを DEFERRED で始めると、
  途中で他の接続が書いた場合に書き込みへの格上げが busy_timeout を待たずに `SQLITE_BUSY` で失敗する。`append_event` の採番と挿入、
  `release_lease` の読み書きも 1 つのトランザクションに入れる。
- GUI / API のための読み取り（ADR-0013）: `events_since(after_id, limit)`（`events` のグローバル単調 id による全タスク横断の追尾）、
  `list_page(filter, order, cursor, limit)`（keyset ページング。`text_contains` は `title` と `objective` の部分一致で、
  SQLite の LIKE なので ASCII の大文字小文字は区別しない。P-45）、`count_by_status()`、`event_rows_for(task_id, after_seq, limit)`。
  `tasks` には一覧のための非正規化列 `title` / `updated_at` / `objective` を持つ。
- マイグレーションは `migrations/NNNN_*.sql` を起動時に適用し、`schema_migrations` 表で版数を管理する。バイナリの知らない新しい版数の DB は
  開かない（`SchemaTooNew`）。接続は WAL・明示的な busy_timeout・`synchronous=NORMAL`（ディスパッチャ・API・`taskctl` の同時アクセスのため。
  WAL はネットワークファイルシステム上では使えないので DB はローカルディスクに置く）。

### 5.2 Dispatcher (`task-dispatch`)

決定的ループ。1 tick ごとに:

1. 終了したワーカー／レビューの結果を取り込み、状態遷移をストアに書く
2. 期限切れリースを回収 → `running`→`ready`（attempts+1）
3. `ready_tasks` を `priority DESC, created_at ASC` で取得。`attempts > 0` のタスクは `updated_at + min(base·2^(attempts-1), cap)` まで見送る（P-3）
4. 各タスクについて `WorkerHint` と設定のプロバイダ表から**アダプタ×プロバイダ（= アカウント）**を選ぶ（§5.5 の `ProviderPolicy::select`）。選んだプロバイダが並列度の上限なら除外して次の候補を選び直す（設定表の順の「優先 + あふれ」。P-41）。全体の並列度上限を超えたら待つ。条件に合うプロバイダが設定に無いタスクは warn を出して `ready` のまま残し、`--until-idle` の待ち対象から外す（取得件数はその分だけ広げ、後ろの実行可能なタスクを飢餓させない）
5. リース取得 → ワーカー起動（非同期）→ `running`。ワーカーの出力がある間はリースを延長する（P-7）
6. ワーカー終了イベントを受けて `reviewing` へ → Reviewer 起動。供給側失敗（レート制限・認証失敗・枯渇・起動失敗）は `requeue` にしてプロバイダを cooldown にする（P-21）。cooldown 中のプロバイダは 4. で飛ばされるので、次の run は別のアカウントに回りうる。同じ試行での連続 requeue は `max_requeues` まで（P-40）

**LLM呼び出しは一切ここに書かない。** 選択規則は全て設定とコードで表現する。tick間隔は設定 `tick_ms`（既定 2000。P-22）。

### 5.3 Worker protocol (`task-worker`)

オーケストレータ→ワーカーは**サブプロセス起動＋stdin/stdout の JSON Lines**。すべてのアダプタはこの形に正規化する。

```
→ {"type":"run","protocol":2,"task":{...Task...},"workspace":"/abs/path","context":{"prior_review":[...],"inputs":[...],"answers":[...],"review":{...},"role":{...},"children":[...]}}
← {"type":"progress","msg":"..."}
← {"type":"artifact","name":"bench.json","path":"artifacts/bench.json"}
← {"type":"question","text":"..."}            # → タスクを blocked にする
← {"type":"done","summary":"...","evidence":[{"criterion":0,"command":"cargo test","exit":0,"stdout_tail":"..."}],"usage":{"input_tokens":..,"output_tokens":..}}
← {"type":"error","message":"...","retryable":true,"provider_failure":{"kind":"throttled","retry_after_secs":60}}
← {"type":"delegate","tasks":[{"title":"...","objective":"...","acceptance":[...],"role":"implementer","depends_on":[0]}]}   # ADR-0016: 実行中の委譲
```

- `evidence` は受け入れ条件ごとに「何を実行して何が出たか」。Reviewer はこれと成果物だけを見る。`criterion` 以外（`command` / `exit` / `stdout_tail`）は、コマンドを伴わない条件（`ArtifactExists` / `Reviewer` / `Human`）では省略してよい（P-41、旧 P-12）。
- `usage` は取れる範囲で。取れないアダプタは省略可。
- `context.role` は `[[roles]]` から引いた役割の指示文、`context.children` は集約 run（`aggregate`）で渡す子の一覧（ADR-0016）。
- `delegate` は run 中に子タスクを提案する（上限: 1 run の件数 / 木の深さ / 木の run 数）。CLI 系アダプタは `artifacts/delegate.json` に書く規約（ADR-0016 M8）。
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
- アダプタのインスタンスは `[[providers]]` の行（= 1 アカウント）ごとに作る。`[adapters.<種別>]` を共通設定とし、行の `env` を重ね（同名キーは行が優先。`CLAUDE_CONFIG_DIR` / `CODEX_HOME` 等でアカウントを分離）、行の `model` が空でなければ `--model` に使う（P-41、ADR-0012 D1）。ワーカーは taskd 自身の環境を引き継ぐので、複数アカウント運用では `ANTHROPIC_API_KEY` 等を taskd の環境から外しておく。

### 5.5 Provider policy（モデル供給層との境界）

```rust
pub trait ProviderPolicy {
    fn pick(&self, hint: &WorkerHint, now: Instant) -> Option<(AdapterId, ProviderId)>;
    fn report(&mut self, provider: ProviderId, outcome: &ProviderOutcome); // Ok / Throttled{retry_after} / AuthFailed / Exhausted
    fn concurrency_limit(&self, provider: ProviderId) -> usize;

    /// 除外集合（並列度の上限に達したプロバイダ等）付きの選択（P-41、ADR-0012 D2）。
    /// 既定実装は `pick` から導く（選べなければ Busy）ので、上の 3 メソッドだけを実装したポリシーもそのまま動く。
    fn select(&self, hint: &WorkerHint, now: Instant, excluded: &HashSet<ProviderId>) -> Selection { /* 既定実装 */ }
}

pub enum Selection {
    Picked { adapter: AdapterId, provider: ProviderId },
    Busy,                // 条件に合うプロバイダはあるが、全て cooldown 中または除外されている（一時的）
    NoMatchingProvider,  // 条件に合うプロバイダが設定に無い（設定を直さない限り解消しない）
}
```

本プロジェクトでは `StaticPolicy`（設定表の優先順位、Throttled / AuthFailed / Exhausted は cooldown まで除外、`select` は除外集合と cooldown を飛ばして次の行へ）のみ実装。残量推定に基づく配分やアカウント間の自動切替は供給層側で `ProviderPolicy` を差し替える。

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
taskctl worker run --config <taskd.toml> --task <id> [--provider <id> | --adapter <種別>] [--workspace <dir>]   # 1 タスクを 1 アカウントで 1 回実行（デバッグ用）
```

出力を閉じたパイプ（`| head` 等）に流しても異常終了しないこと。

`taskctl worker run`（P-41、ADR-0012 D4）はデーモンとディスパッチャを通さずに、アダプタの認証・モデル指定・プロンプト・結果ファイルを確かめるためのコマンド。
- **DB を変えない**（リース・遷移・イベント追記をしない）。レビューも行わない。`context.prior_review` / `context.answers` はディスパッチャと同じく `events` から組み立てる。
- プロバイダは `--provider`、`--adapter` に合う最初の行、どちらも無ければタスクの `worker_hint` に合う最初の行。
- `running` / `reviewing` のタスクは `--workspace` 無しでは拒否する（デーモンの run と作業ディレクトリを取り合わないため）。確認用途では `--workspace` にコピーを指定することを推奨する。
- 出力は `progress:` / `artifact:` を逐次、最後に `result: <終端メッセージの JSON>`。exit code は done=0、question=3、error=4、タスクやプロバイダの不在=1、引数の構文誤り=2、SIGINT / SIGTERM による中断=130（ワーカーのプロセスを kill してから終わる）。

`taskctl` の操作の判断と検証は `task-ops`（§5.10 の API と共有）にあり、`taskctl` は引数解析と出力整形だけを持つ（ADR-0013 D7）。

### 5.9 の補足: タスク作成時の検証（`task-ops`。P-45、ADR-0014 D3）

`taskctl add` と `POST /api/v1/tasks` は同じ関数を通り、次を検証する（順に）: `title` / `objective` が空白だけでないこと、受け入れ条件が 1 つ以上あること、
`parent` が存在すること、`depends_on` が存在し `failed` / `cancelled` でないこと。違反は何も挿入せずエラー（API は 422、CLI は exit 1）。
`taskctl show --json` は `GET /api/v1/tasks/{id}` と同じ型・同じ直列化で出す（差は API が埋める `runs[].files` と `timers.now`、および CLI が設定を読まないこと）。
`taskctl` の `--config` は環境変数 `TASKD_CONFIG` でも渡せる（P-54。`add --role` で `[[roles]]` の既定を引くときに使う）。

### 5.9 の補足 2: クラスタでのコマンド実行（ADR-0018、Phase 12）

`WorkspaceSpec::Remote{cluster, path}` のタスクは、**コマンドだけをクラスタで実行する**（ワーカー＝LLM は taskd のホストで動く）。

- `path` はクラスタ側の作業ディレクトリ（既存プロジェクトでよい）。taskd は `workspace_root/<task_id>` に写しを持つ。
- 順序: pull（クラスタ → 写し）→ ワーカーが写しを編集 → push（写し → クラスタ。既定では削除しない）→ `Check::Command` をクラスタで実行 → pull。
  同期の両方向で `.taskd/` / `runs/` / `inputs/`（taskd の管理用）は除外する（P-46）。`artifacts/` は双方向に同期する（成果物はクラスタで作られることがある）。
- `TaskDetail.workspace_dir` は Remote のタスクでは**写し**（`workspace_root/<task_id>`）を指す。GUI が run のログを開く経路になる（P-48）。
- 接続は**人が張った ssh の多重接続（`ControlMaster`）を借りる**。`BatchMode=yes` で対話的な認証は行わない。
  接続が無ければそのクラスタを cooldown にし、`Event::ClusterUnavailable` を残し、そのタスクは「人待ち」として `--until-idle` の待ち対象から外す。
- 並列度は「プロバイダ（アカウント）」と「クラスタ」の二次元。`ssh` 自体の失敗（終了コード 255）は供給側失敗、リモートコマンドの非ゼロ終了は判定の失敗。
- ワーカーがクラスタでコマンドを流すためのラッパ `.taskd/remote-exec` を run ごとに置く（同期対象からは外す）。
- **同期は 3 通り（`[[clusters]] sync`。ADR-0019）**: `worktree`（git 管理下のプロジェクトの既定の選択）、`rsync`、`none`（共有 FS）。
  `worktree` では、クラスタ側で `git worktree add -B taskd/<task_id>` を切り、**その中だけ**を同期・実行の対象にする。
  追跡ファイルしか checkout されないので未追跡の巨大データを持ち込まず（実測: benchfs は 263 GB 中 220 GB が未追跡）、
  `worktree_paths` の sparse-checkout でさらに絞れる。元のリポジトリの作業ツリーには触らない。
  taskd は commit も push もしない（変更は worktree に残り、diff / commit / `git worktree remove` は人の操作）。
  `TaskDetail.worktree` にパスとブランチを出す。git リポジトリでないディレクトリにこれを指定した run は供給側失敗になる。

### 5.10 API 層（`task-api`。ADR-0013）

Web GUI（`gui/`。Remix のサーバが BFF として呼ぶ別プロセス）と `curl` のための HTTP API。仕様の詳細は `docs/gui/api.md`、
型の JSON Schema は `docs/api/v1/*.schema.json`（コミットし、生成との一致をテストする）。

- **位置**: taskd のデーモンプロセス内で、設定 `[api]`（`listen`、`token_file`、`allowed_hosts`）があるときだけリッスンする（既定は無効）。
  API は自分専用の SQLite 接続を使う。taskd が止まっている間は API も無い（`taskctl` は従来どおり DB を直接使える）。
- **形**: `/api/v1`、HTTP/JSON、エラーは `application/problem+json`。一覧は keyset ページング。変更系は `expected_status` による楽観的検査。
  通知は SSE（`events` のグローバル id をカーソルにし、`Last-Event-ID` で再開。taskctl の書き込みも同じ経路で届く）。gRPC / WebSocket は使わない。
- **協調判断をしない**: 読み取りはストアのクエリ、状態変更は `task-ops` → `TaskStore::apply_transition(_with_events)` / `create_task` だけ。
  LLM 呼び出し、ワーカーの起動、`Check::Command` の実行はしない（原則 1〜4）。
- **デーモンの状態**: ディスパッチャが tick ごとにメモリ上のスナップショット（実行中の run、プロバイダの cooldown と使用数、承認待ちで延期中、
  経路なし、最終 tick）を `tokio::sync::watch` で API に渡し、`GET /api/v1/daemon` と SSE で返す。DB には書かない（観測値であり `replay` の対象外）。
  cooldown は `ProviderPolicy::cooldowns()`（既定実装つき）で取る。
- **セキュリティ**: 既定のバインドは loopback。loopback 以外では `token_file` 必須（`Authorization: Bearer`）。`Host` を許可リストで検査し、
  CORS は出さない（ブラウザは taskd を直接呼ばない）。run のログと成果物は、記録済みのパスと ULID 形式の `run_id` を作業ディレクトリに結合して
  `canonicalize` し、作業ディレクトリ外を返さない。`[[providers]].env` の値とトークンは返さない。

- **今できる操作**: `TaskDetail` だけでなく `TaskRef` / `TaskSummary` にも `actions`（`approve` / `reject` / `answer` / `cancel`）を入れる。
  GUI は受信箱・一覧・DAG・依存関係のどこから来た参照でも、この規則を再実装しない（ADR-0015 D4）。
- **観測可能性**（ADR-0015 D1〜D3）: API は要求ごとに `method` / `path` / `status` / `duration_ms` / `request_id` を記録し、1 秒を超えたものを警告する
  （`GET /stream` は対象外）。デーモンは `max(1 秒, tick_ms × 2)` を超えた tick と、その中の遅い段階を警告する。DB がネットワークファイルシステム上にあれば
  起動時に警告する（WAL はローカルディスク前提。§5.1）。
- **API の異常終了**: API サーバのタスクが落ちてもデーモンは動き続け、停止時にエラーをログに出す（tick ループでは監視しない。実際に落ちるのは稀）。

SQLite は WAL・明示的な busy_timeout・スキーマ版数（知らない新しい版の DB は開かない）で、ディスパッチャ・API・`taskctl` の同時アクセスに備える（§5.1、ADR-0013 D5）。

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
  8. 供給側失敗が続くタスクは、同じ試行で `max_requeues` 回 requeue した後に通常の失敗として attempts を消費する（P-40、Phase 7 の後に追加）

### Phase 8 — 複数アカウント運用とデバッグ CLI

（人間の依頼により Phase 7 の後に実施し、実施後にこの節を追加した。ADR-0012）

- `[[providers]]` の行ごとのアダプタ（env / model によるアカウント分離）、`ProviderPolicy::select` による上限・cooldown 時の次のアカウントへのフォールバック、条件に合うプロバイダが無いタスクの区別、`WorkerStarted.provider`、`evidence` の任意フィールド、`taskctl worker run`
- 受け入れ（fake ワーカーとローカル SQLite で再現し、`taskctl replay` 差分ゼロを併せて確認する。実機での確認は §6 冒頭の規則に従う）:
  1. 同じアダプタ種別の 2 アカウント（`env` が異なる）で、先頭アカウントの並列度上限をあふれた分が 2 つ目のアカウントで実行され、各 run の `WorkerStarted.provider` / `model` にアカウントごとの値が残る
  2. レート制限・認証失敗を返したアカウントは cooldown になり、同じタスクが attempts を消費せず次のアカウントで実行される
  3. 条件に合うプロバイダが設定に無いタスクは `taskd --until-idle` を止めず、それより優先度の低い実行可能なタスクも dispatch される
  4. `evidence` の要素は `criterion` だけでも読め、旧形式（全フィールドあり）も読める
  5. `taskctl worker run` が DB を変えずに 1 タスクを指定したアカウントで実行し、done / question / error の exit code を返す。SIGTERM で exit 130 とともにワーカーのプロセスが終了する

### Phase 9 — GUI のための基盤と HTTP API 層

（人間の依頼により追加。ADR-0013。GUI 本体は `gui/`（ADR-0020 以前は別リポジトリ `taskd-gui`）で、`run-gphases.sh` により G フェーズとして進める）

- 9a 基盤: SQLite の WAL・busy_timeout・スキーマ版数、`events` のグローバル id と `events_since`、`task-ops` の抽出、`Event` の JSON Schema、
  `ProviderThrottled` の記録、一覧のページング
- 9b API: `task-api`（§5.10）、デーモン状態の公開、API 型の JSON Schema、`taskctl show --json`
- 受け入れ（fake ワーカーとローカル SQLite で再現し、`taskctl replay` 差分ゼロを併せて確認する）:
  1. 版数 1 の既存 DB を開くと最新の版数に移行し、移行の前後で `events_for` と `taskctl replay` の結果が変わらない。知らない新しい版数の DB は開かない
  2. ファイル DB が WAL になり、`taskd` の実行中に `taskctl` で書き込んでも `database is locked` にならない
  3. `taskctl` の全コマンドと e2e が `task-ops` の抽出後も無変更のテストで通る（挙動が変わらない）
  4. `[api]` が無い設定ではリッスンしない。有効にすると `curl /api/v1/health` が `api_version` と `schema_version` を返す
  5. API から approve / reject / answer / cancel / タスク作成 / plan を行うと状態機械を通って遷移し、無効な遷移は 409（problem+json）、`expected_status` の不一致は 409
  6. SSE を購読中に `taskctl add` すると 2 秒以内に `Created` が届き、`Last-Event-ID` で再接続しても取りこぼさない
  7. fake ワーカーのレート制限シナリオで `GET /api/v1/daemon` に実行中の run とプロバイダの cooldown が現れ、`ProviderThrottled` がイベントに残る
  8. loopback 以外で `token_file` 無しは設定エラー、許可されない `Host` は 400、作業ディレクトリ外を指す成果物は 403、`env` の値は応答に含まれない
  9. `docs/api/v1/*.schema.json` が生成結果と一致する

### Phase 10 — 役割と委譲（組織的な木構造。ADR-0016）

設計は ADR-0016。**状態機械と `TaskKind` は変えない**（役割は属性、委譲は子タスクの挿入で表す）。

- `Task.role: Option<String>` と設定 `[[roles]]`（id / tier / adapter / max_turns / max_wall_secs / 指示文）。
  タスクの値 > 役割の既定 > 全体の既定の順に効く。`RunRequest.task` に役割と指示文を載せる
- ワーカープロトコルに `{"type":"delegate","tasks":[…]}`（実行中の子タスクの提案）。上限は設定
  `max_delegate_per_run`（既定 8）/ `max_tree_depth`（既定 5）/ `max_tree_runs`（既定 100）
- `Event::Delegated{run_id, task_ids}`。親は子が全て終端になるまで `reviewing` のまま（`pending_children > 0` の一般規則）。
  委譲した子が `failed` になったら、親は**失敗を引き継がず**やり直す（`Trigger::ChildFailed`）。やり直せなければ
  `blocked` にして `Event::QuestionRaised` で人に聞く（ADR-0021。設定 `[delegation] on_child_failure`）
- `Task.aggregate: bool`。true の親は子が全て終端になった後に 1 回だけ run し、`artifacts/summary.md` を作る
- 受け入れ:
  1. `[[roles]]` の既定が run に反映され、`WorkerStarted` から役割が追える（`taskctl add --role lead` と API の `role`）
  2. fake ワーカーが `delegate` で子 2 件を提案すると、`task-ops` の検証を通ったものだけが子として挿入され、`Event::Delegated` が残る。
     上限（1 run の件数・木の深さ・木の run 数）を超える提案は拒否され、理由が `WorkerProgress` に残る（タスクは失敗しない）
  3. 子が全て終端になるまで親は `reviewing` のまま。`aggregate = true` の親は最後に 1 回だけ run し、`artifacts/summary.md` が
     受け入れ条件で判定される。`aggregate = false` の親は従来どおり
  4. 循環・自己参照の提案（自分自身や祖先を `depends_on` にする）は拒否される
  5. `taskctl show --json` と `GET /api/v1/tasks/{id}` に `role` と `delegated`（この run が作った子）が出る
  6. `taskctl replay` の差分ゼロ（`Delegated` は状態を変えない）、`cargo test --workspace` と clippy が通る

### Phase 11 — アカウント管理の API（ADR-0017）

設計は ADR-0017。**GUI 側の画面は別フェーズ（G フェーズ）**。ここでは taskd の API と設定の仕組みだけを作る。

- `providers_include = "providers.d/*.toml"`（トップレベルのキー。`[[providers]]` との TOML 上の衝突を避けるため）と `providers.d/<id>.toml`（1 アカウント 1 ファイル）
- 管理系 API（**loopback でもトークン必須**）: `POST /api/v1/providers`、`PATCH /api/v1/providers/{id}`、
  `DELETE /api/v1/providers/{id}`、`POST /api/v1/reload`、`POST /api/v1/providers/{id}/check`
- `check` は、そのアカウントの env で短い run（30 秒 / 1 ターン）を 1 回だけ行い、`ok` / `auth_failed` / `throttled` /
  `spawn_failed` を返す。タスクにもイベントにも残さない（観測値）
- 受け入れ:
  1. `POST /api/v1/providers` が `providers.d/<id>.toml` を作り、`POST /api/v1/reload` の後の tick から新しいアカウントが使われる。
     実行中の run は影響を受けない（テストは fake アダプタで行う）
  2. 管理系はトークン無しで 401（loopback でも）。読み取り系は従来どおり
  3. `env` の値・`token_file` の中身は、応答にもログにも出ない（`grep` で確認する）
  4. `POST /api/v1/providers/{id}/check` が 4 種類の結果を返す（fake アダプタで `ok` と `auth_failed` を再現）
  5. 重複 id の追加は 409、存在しない id の変更・削除は 404、`reload` で cooldown が消えることのテスト
  6. `cargo test --workspace` と clippy が通り、`docs/api/v1/api-v1.schema.json` が再生成されている

### Phase 12 — クラスタでのコマンド実行（ssh + ControlMaster。ADR-0018）

人間の判断: **LLM は手元で動かし、クラスタで実行するのはコマンドだけ**。pegasus / sirius は 2 要素認証なので、ssh を張るのは人の操作で、
taskd は `ControlMaster` の多重接続を借りるだけ（対話的な認証は行わない）。

- `[[clusters]]`（host / concurrency / sync / delete_on_push / setup / env / rsync_excludes）、既存の `WorkspaceSpec::Remote{cluster, path}` の実装、`.taskd/remote-exec`（ワーカー用のラッパ）、
  リモートでの `Check::Command` 実行、rsync による往復同期（`sync = "none"` で無効）、クラスタごとの並列度と cooldown、`Event::ClusterUnavailable`
- 受け入れ（**ssh 先を `localhost` にして行い、外部ネットワークに出ない**）:
  1. `WorkspaceSpec::Remote` のタスクで、run の前に pull・後に push が行われ、`Check::Command` が ssh 越しに実行される（クラスタにしか無いファイルを使う条件が通る）
  2. 多重接続が無いクラスタのタスクは、供給側失敗として requeue され（attempts を消費しない）、そのクラスタが cooldown に入り、
     `Event::ClusterUnavailable` が残る。GUI の「注意」に「ログインし直してください」が出る
  3. `sync = "rsync"` では pull → run → push → 判定 → pull の順で往復し、クラスタで作られた成果物がローカルの sha256 で判定される。`sync = "none"` では同期しない。
     **既存プロジェクトを指すタスクで push が既存ファイルを消さない**（`delete_on_push = false` が既定）
  4. `.taskd/remote-exec <cmd>` がクラスタで実行され、終了コードと出力がそのまま返る。同期対象から外れている
  5. クラスタとプロバイダの並列度が両方守られる。設定に無い `cluster` のタスクは `unroutable` として扱われ、`--until-idle` を止めない
  6. ssh 自身の失敗（終了コード 255）は供給側失敗、リモートコマンドの非ゼロ終了は判定の失敗として区別される
  7. `taskctl replay` の差分ゼロ
- 運用の道具: `config/ssh-config.example`、`scripts/cluster-login.sh`（人が 2 要素認証を通して接続を張る）、`scripts/cluster-check.sh`（前提と共有 FS の判定）

**第 1 段階（2026-09-15 実装済み）**: 上の 1〜7 と、`[[clusters]]`、`SshWorkspace`、`taskctl add --cluster`、`Event::ClusterUnavailable`。

**第 2 段階（残り。ここが Phase 12 の完了条件）**:
  8. `GET /api/v1/clusters` が、設定の一覧（id / host / concurrency / sync / delete_on_push / setup の有無）に、
     いま多重接続があるか（`connected`）と cooldown の残りを付けて返す。`env` の値は返さない
  9. 受信箱の `attention` に、`ClusterUnavailable` が直近 24 時間にあるクラスタを 1 件ずつ出す
     （`type: "cluster_unavailable"`、`cluster` / `host` / `at` / 対象タスク数。GUI が「ログインし直してください」と出せる形）
  10. `taskctl worker run --cluster <id>` が、クラスタ側の作業ディレクトリに対して 1 回の run を実行する
      （デーモン無しの動作確認。DB は変更しない。多重接続が無ければ exit 4 と理由）
  11. `taskctl show --json` と `GET /api/v1/tasks/{id}` に、そのタスクのクラスタ（`workspace_dir` と並ぶ `cluster`）が出る
  12. 上を含めて `cargo test --workspace` と clippy が通り、`docs/api/v1/api-v1.schema.json` が再生成されている

### 非目標（本プロジェクトではやらない）

Web UI（HTTP API 層は §5.10 で Rust 側の範囲。UI は `gui/` の別プロセス）、予算・残量推定、残量推定に基づく複数アカウントの自動切替、マルチユーザ、通知。これらは接続層・供給層の担当。
（**リモートワークスペースの実装は非目標から外した**。人間の当初の狙い「複数クラスタへのタスク投入」に必要なため、Phase 12 / ADR-0018 で本プロジェクトの範囲とする。
ジョブスケジューラ経由の投入と、クラスタ側に taskd を常駐させる構成は引き続き採らない。）
（設定表の順に従う決定的なフォールバック — 並列度の上限・cooldown 中のアカウントを飛ばすこと — は Phase 8 で本プロジェクトの範囲とした。どのアカウントをどれだけ使うかの最適化は供給層が `ProviderPolicy` を差し替えて行う。）

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
