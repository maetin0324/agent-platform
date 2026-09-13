# ADR-0005: Phase 3 — fake ワーカー、ディスパッチャ、LocalWorkspace、Reviewer の実装方針

- 日付: 2026-09-13
- 状態: Accepted（Phase 3）
- 関連: `docs/DESIGN.md` §5.2–§5.8, §6 Phase 3 / [ADR-0002](0002-state-machine.md) / [ADR-0003](0003-worker-protocol.md) / [ADR-0004](0004-taskctl-cli.md)

## 文脈

Phase 3 は「ワーカープロトコル、`fake` アダプタ、`StaticPolicy`、ディスパッチャ、`LocalWorkspace`、
Reviewer（`Command` と `ArtifactExists` のみ）」を実装し、`tests/e2e` の 3 シナリオを通す。
ADR-0002/0003 で決まっていない次の点を確定する:

(a) クレート配置とクレート間の依存方向、(b) `fake` アダプタの形（同プロセスかサブプロセスか）、
(c) `Task.workspace` と ADR-0003 D5 の `<workspace_root>/<task_id>/` の関係、
(d) ワーカー終了・レビュー判定をストアに書き戻す際のイベントの原子性、
(e) `ProviderPolicy` の `pick` が返す候補と並列度上限の関係、
(f) `taskd` の設定ファイルと終了条件、(g) e2e テストの実行形態。

## 決定

### D1. クレート配置と依存方向

```
task-core  ◀── task-worker ◀── task-dispatch ◀── taskd
                                                  ▲
                                          tests/e2e (dev-dependency)
```

| クレート | 内容 |
|---|---|
| `task-worker` | プロトコル型（`RunRequest`, `WorkerMessage`, `Evidence`, `PriorReview`）、`WorkerAdapter`/`EventSink` trait、サブプロセス実行器（ADR-0003 D1/D3/D4/D5）、`fake` アダプタ、`Workspace` trait と `LocalWorkspace`/`RemoteWorkspace`、成果物ヘルパ（パス検査・sha256） |
| `task-dispatch` | `ProviderPolicy` trait と `StaticPolicy`、決定的ディスパッチャ（tick）、Reviewer（`Command`/`ArtifactExists`） |
| `taskd` | 設定（TOML）、`tracing` 初期化、ループ本体。ライブラリ + 薄い `main.rs` |
| `tests/e2e`（package `e2e`） | fake アダプタだけで動く end-to-end テスト |

`Workspace`（DESIGN §5.8）は「ワーカーを走らせる場所」の抽象なので `task-worker` に置く。
Reviewer は「協調ループの一部で LLM を呼ばない決定的コード」なので `task-dispatch` に置く
（Phase 5 の `Reviewer` check はワーカーアダプタ経由で別プロセスを起動する形にし、ディスパッチャ内に LLM 呼び出しを書かない）。

### D2. `fake` アダプタ = サブプロセス実行器 + 設定されたコマンド

ADR-0003 D7 の「小さなスクリプト／同プロセス実装」のうち **サブプロセス** を採る。
`fake` アダプタは、設定 `[adapters.fake] command = ["sh", "-c", "..."]`（または任意の実行ファイル）を
ワークスペースを cwd として起動し、stdin に `run` を 1 行書き、stdout の JSON Lines を読む。
コマンド未設定時の既定は「`progress` 1 行と `done{summary:"fake", evidence:[]}` を出す `sh` スクリプト」。

理由:
- DESIGN §5.3 の「全アダプタはサブプロセス + JSON Lines に正規化」を fake でも実際に通すことで、
  終端規則・タイムアウト・強制終了・成果物パス検査が **テストで動く**（同プロセス実装だとこれらが素通りになる）。
- e2e テストは `sh` スクリプトで振る舞いを書ける（1 回目は失敗し 2 回目で成功、など）。JSON の解析は不要
  （stdin は読み捨ててよく、cwd がワークスペースなのでマーカーファイルで回数を数えられる）。
- Phase 4 の `claude-code` / `codex` アダプタは同じサブプロセス実行器を再利用する。

### D3. ワークスペースのディレクトリ規則

- `WorkspaceSpec::Local{path}` の `path` を **そのタスクの作業ディレクトリ** とする（protocol の `workspace` フィールド、
  ワーカーの cwd、`Command` チェックの cwd、`artifact.path` の基準がすべてこれ）。
  相対パスは設定 `workspace_root` からの相対と解釈する。
- `LocalWorkspace::prepare` は `<dir>/`, `<dir>/artifacts/`, `<dir>/inputs/`, `<dir>/runs/` を作り、
  `task.inputs` のファイルを `<dir>/inputs/<name>` にコピーする。既存ファイルは消さない（e2e テストがスクリプトや
  マーカーを事前に置けるようにするため）。
- run ごとの生ログは `<dir>/runs/<run_id>/stdout.jsonl`, `stderr.log`, `result.json`（終端メッセージ）に置く。
- ADR-0003 D5 の `<workspace_root>/<task_id>/` は `taskctl add --workspace` 省略時の既定を将来変える案
  （現状は ADR-0004 D4 のとおりカレントディレクトリ）として提案 P-19 に回し、Phase 3 では採らない。
- `WorkspaceSpec::Remote` のタスクはディスパッチしない（warn を出して `ready` のまま残す）。接続層が実装するまで
  `RemoteWorkspace` は `unimplemented!()` の骨組みのみ。

### D4. ストアへの書き戻しは `apply_transition_with_events`

ADR-0002 D2「同一トリガに対する追加イベント（`WorkerFinished`, `ReviewVerdict` …）は `Transitioned` と
同一トランザクションで追記する」を満たすため、`TaskStore` に

```rust
fn apply_transition_with_events(&self, task_id, trigger, extra_events: Vec<Event>) -> Result<Outcome, StoreError>;
```

を追加し、既存の `apply_transition(id, trigger, Option<Event>)` はこれに委譲する既定実装にする（ADR-0004 D1 の
呼び出し側は変更不要）。ディスパッチャは:

| 事象 | trigger | 同一トランザクションの追加イベント |
|---|---|---|
| ワーカー `done` | `WorkerDone` | `WorkerFinished{run_id, outcome:"done: <summary>", usage}` |
| ワーカー `question` | `WorkerQuestion` | `WorkerFinished{outcome:"question: <text>"}` |
| ワーカー `error` / 終端無し / タイムアウト | `WorkerError{retryable}` | `WorkerFinished{outcome:"error(retryable=<b>): <msg>"}` |
| アダプタ内部エラー（起動失敗等） | `WorkerError{retryable:true}` | 同上 |
| リース期限切れ | `LeaseExpired` | `WorkerFinished{outcome:"lease_expired"}` |
| レビュー全 pass | `ReviewPass` | `ReviewVerdict` × 条件数 |
| レビュー fail あり | `ReviewFail` | `ReviewVerdict` × 条件数 |

`WorkerProgress` / `ArtifactProduced` は run の途中で `append_event` により逐次追記する（遷移を伴わない）。
`WorkerStarted{run_id, adapter, model}` は `acquire_lease` 成功直後に追記する。

**古い結果の破棄**: ワーカー終了時に `task.status == Running` かつ `task.lease.worker_run_id == run_id` でなければ
（リース回収済み・cancel 済み）結果を捨てて warn を出す（ADR-0002 D9）。

### D5. Reviewer（Phase 3 の範囲）

`reviewing` のタスクに対し、条件ごとに:

- `Command{cmd, expect_exit}` — `Workspace::exec(cmd, review_timeout)` をタスクの作業ディレクトリで再実行し、
  exit が一致すれば pass。タイムアウトは fail。理由に stdout/stderr の末尾を残す。
- `ArtifactExists{name}` — その run の `ArtifactProduced` で同名の成果物があればその `path` を、無ければ
  `artifacts/<name>` を見る。ファイルが存在すれば pass、理由に sha256 を残す。
- `Reviewer` / `Human` — Phase 3 では未対応。**fail**（理由 `"check kind not supported until Phase 5/6"`）とする。
  pass 扱いにすると原則 4（完了はレビューが決める）を破るため。副作用として `taskctl add --accept`（常に `Human`）
  で作ったタスクは Phase 3 の `taskd` では `done` にならない。これは P-17（`--check-cmd`）の採否待ちとして残す。

判定は `reviewing` に入った直後（同 tick）に非同期で開始し、各 tick で「`reviewing` だが判定中でない」タスクも
拾う（デーモン再起動後の復旧）。

### D6. `ProviderPolicy` と並列度

DESIGN §5.5 の trait シグネチャをそのまま採る。`pick` は「adapter 指定が一致し、tier を提供し、cooldown 中でない」
最初のプロバイダを **1 つ** 返す。返されたプロバイダが並列度上限（`concurrency_limit`）に達している、または全体上限
`max_concurrency` に達している場合、ディスパッチャはそのタスクを **この tick では見送る**（DESIGN §5.2「超えたら待つ」）。
次候補へのフォールバックは行わない（供給層側で差し替える `ProviderPolicy` の責務）。

`report`: `Throttled{retry_after}` はその時間、`AuthFailed` / `Exhausted` は設定 `error_cooldown_secs`（既定 300）だけ
cooldown する。`Ok` は cooldown を解除しない（期限で自然解除）。Phase 3 のアダプタは `Ok` しか報告しない。

### D7. `taskd` の設定と終了条件

`taskd --config <path> [--until-idle] [--max-ticks N]`。設定は TOML:

```toml
db = "taskd.sqlite3"           # taskctl の --db / TASKD_DB と同じファイルを指す
workspace_root = "workspaces"  # WorkspaceSpec::Local の相対パスの基準
tick_secs = 2
max_concurrency = 4
lease_grace_secs = 60          # ADR-0002 D7: ttl = max_wall_secs + 猶予
idle_timeout_secs = 300        # ADR-0003 D4
kill_grace_secs = 10           # ADR-0003 D4
review_timeout_secs = 600      # Command チェック 1 件あたり
error_cooldown_secs = 300      # D6

[adapters.fake]
command = ["sh", "-c", "cat >/dev/null; echo '{\"type\":\"done\",\"summary\":\"ok\",\"evidence\":[]}'"]

[[providers]]                   # 上から順に優先
id = "fake-local"
adapter = "fake"
tiers = ["frontier", "standard", "cheap"]
concurrency = 2
model = "fake"
```

`--until-idle`: 「実行中／判定中の run が無く、`ready_tasks(1)` が空で、DB に `running`/`reviewing` が無い」tick で
exit 0 する（e2e テスト用。`--max-ticks` は無限ループの安全弁）。通常運用は SIGINT/SIGTERM で停止する。

### D8. e2e テストの形

`tests/e2e` は workspace member の package `e2e`。各シナリオは一時ディレクトリに DB・設定・ワークスペース・`sh` スクリプトを
作り、**実バイナリ `taskd`** を `--until-idle` で起動して終了を待ち、`SqliteStore` で結果（status、イベント列）を検証する。
バイナリの場所は `target/debug/taskd`（テスト実行ファイルの 2 つ上のディレクトリ）から解決する。
`crates/taskd/tests/` に最小の統合テストを置き、`cargo test --workspace` で `taskd` バイナリが必ずビルドされるようにする。
タスクは `task-core` の API で直接挿入する（`taskctl add` は `Check::Human` しか作れないため。D5 参照）。

## 結果

- `task-core`: `schemars::JsonSchema` の derive を `Task` とその構成型に追加（ADR-0003 D6 のスキーマ生成のため。
  `OffsetDateTime` / `Ulid` は `with = "String"`）。`TaskStore::apply_transition_with_events` を追加。
- `task-worker`, `task-dispatch`, `taskd`, `tests/e2e` を新設し workspace member に追加。
- `docs/protocol/worker-protocol.schema.json` を生成してコミットし、テストで一致を検証（ADR-0003 D6）。

## DESIGN.md 修正提案（本 ADR のスコープ分）

- **P-19（§4.4/§5.8 ワークスペース配置）** `taskctl add --workspace` 省略時の既定を「カレントディレクトリ」から
  「`<workspace_root>/<task_id>/`」に変え、ADR-0003 D5 と揃えることを提案。Phase 4 でリポジトリのチェックアウトを
  `prepare()` に入れるときに合わせて決める。
- **P-20（§5.5 pick）** `pick` が返した唯一の候補が並列度上限に達している場合にフォールバックできないため、
  `pick` に「除外するプロバイダ集合」を渡す（または候補リストを返す）拡張を提案。
- **P-21（§4.2/§5.2 供給側の失敗）** Throttled / AuthFailed のような供給側の失敗で `attempts` を消費しない
  `Requeue` トリガ（`running → ready`、attempts 据え置き）の追加を提案。採用まで `WorkerError{retryable:true}` で扱う。
