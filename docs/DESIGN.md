# taskd Web GUI — 設計（`taskd-gui`）

- 状態: **Accepted**（人間が §11 の H1〜H9 に回答。2026-09-14 改訂）
- 対象: taskd（`docs/DESIGN.md`）の上に載せる Web GUI。**実装は別リポジトリ `taskd-gui`** で行う。本リポジトリ（taskd）では DESIGN §2 / §6 と CLAUDE.md により
  Web UI の実装は非目標・禁止であり、本文書は設計だけを置く。taskd 側に必要な変更は ADR-0013 で決定済み（Phase 9）。
- 関連: [ADR-GUI-0001 境界](adr/0001-architecture-boundary.md) / [ADR-GUI-0002 フロントエンド](adr/0002-frontend-stack.md) / [taskd HTTP API v1](taskd-api-v1.md) /
  [taskd への提案](taskd-proposals.md) / `taskd-gui` 立ち上げ用ファイル（taskd リポジトリの docs/gui/bootstrap/README.md） / taskd 側 `docs/adr/0013-taskd-api-and-gui-foundations.md`
- `taskd-gui` リポジトリでは本文書が `docs/DESIGN.md`、`taskd-api-v1.md` が `docs/taskd-api-v1.md` になる（taskd リポジトリの docs/gui/bootstrap/README.md の対応表）。

---

## 0. 要約

- **何を作るか**: 研究者 1 人（将来は研究室内の数人）が、承認待ち（Approval / blocked の質問 / draft の子）を処理し、タスクの状態・イベント・run の生ログ・
  成果物を読み、タスクと Plan を作るためのローカル Web GUI。
- **境界（ADR-GUI-0001、H1）**: `taskd-gui` は別プロセス・別リポジトリ。**taskd が提供する HTTP/JSON + SSE API v1**（`/api/v1`、crate `task-api`、
  taskd のデーモンプロセス内で `[api]` 設定時だけ動く）だけを使う。SQLite を開かず、taskd の crate にも依存しない。ブラウザは taskd を直接呼ばない。
  taskd 停止中は GUI も操作不可（`taskctl` は従来どおり DB を直接触る）。
- **デーモン状態（H5）**: ディスパッチャが `tokio::sync::watch` でメモリ上のスナップショットを API に渡し、`GET /api/v1/daemon` と SSE の `daemon` で公開。DB には書かない。
- **リアルタイム**: `events` のグローバル単調 id（H4）を taskd の API がポーリングして SSE で流し、`taskd-gui` の BFF が中継、ブラウザは loader を再検証する。
- **フロントエンド（ADR-GUI-0002、H2）**: **React 19.3 + Remix = React Router 8.3 framework mode（SSR）**。loader / action が BFF。TanStack Query は不要。
  Tailwind 4 + shadcn/ui（Base UI）、React Flow + dagre、CodeMirror 6、Vitest 5、Playwright 1.63、Biome 2.5、pnpm 11（cooldown 7 日）、TypeScript 7。
  型は taskd がコミットする `docs/api/v1/api-v1.schema.json` から生成（H7）。
- **配布**: Node 24 LTS + `pnpm build` の `build/` + `server.js`（`@react-router/express`）。systemd。単一バイナリ（Node SEA）は実験項目。

---

## 1. 目的

1. 原則 5「人間は承認待ちキューだけを見ればよい」を GUI で実現する。受信箱を開けば、いま人間の判断を待っているものが全部見え、その場で決められる。
2. `taskctl show` の読みにくさ（P-39）を、taskd 側の `task-ops::TaskDetail` で解消し、`taskctl show --json` と GUI が**同じ JSON** を使う（ADR-0013 D7 / D12）。
   GUI は派生値を再計算しない。
3. 複数アカウント運用（ADR-0012）の状況 — どのアカウントで何が走り、どこが cooldown 中で、どれだけ使ったか — を見えるようにする。
4. 状態の真実は SQLite（taskd の内側）のまま。GUI は真実を作らず、API 越しの写しを見せ、API 越しに状態機械を通してだけ変える。

## 2. 非目標

- taskd リポジトリでの Web UI 実装（DESIGN §6）。本文書は設計のみ。
- マルチユーザ（権限・監査）。単一ユーザ前提。localhost バインドと SSH ポートフォワードで使う。共有ホストでの利用は §8 のパスワードとトークンで最低限守るに留める。`by` は `"human"` のまま（H9）。
- 通知（メール・Slack）、予算・残量推定、複数アカウントの自動切替（供給層の担当）。
- GUI からのワーカー実行（`taskctl worker run` 相当）や `Check::Command` の実行。GUI も taskd の API もプロセスを起動しない。
- ログ・成果物の全文検索、DB の編集、`events` の削除・修正、`taskd.toml` の編集（読むだけ）。
- リモートワークスペース（`WorkspaceSpec::Remote`）のファイル閲覧。
- **taskd 停止中の操作**（初版にあった「GUI は taskd が止まっていても動く」は捨てた。ADR-GUI-0001 D1）。
- taskd の API の**仕様外の挙動に頼ること**（GUI 側で SQLite を開く、`taskctl` の文字出力を解析する、判断ロジックを再実装する）。足りなければ taskd への提案として止まる（§10.0）。

## 3. 設計の前提

### 3.1 Phase 8 時点のコードで確認した事実（GUI の設計に影響するもの。2026-09-14）

| 事実 | 出所 |
|---|---|
| 状態変更は `TaskStore::apply_transition(_with_events)` / `create_task` / `complete_plan` / `acquire_lease` だけが行い、遷移・関連イベント・伝播は同一トランザクション | `crates/task-core/src/store.rs` |
| `taskctl approve` は `draft` なら `Accept`、`Approval && ready` なら `Approve` + `ApprovalDecided{by:"human"}`。`reject` は `Approval && ready` のみ。`answer` は `blocked` のみで、質問文は直近の `WorkerFinished.outcome` の `"question: "` から取る。`cancel` は非終端のみ | `crates/taskctl/src/commands/{gate,cancel}.rs` |
| `taskctl add` の検証と既定値（条件 1 つ以上、`depends_on` の存在と非 failed/cancelled、`kind=execute` / `tier=standard` / `max_turns=10` / `max_wall_secs=600` / `max_retries=2`、workspace 既定 `<task_id>`、`Approval` は `ready` で作る）、`plan` の既定（`tier=frontier` / 30 / 900 / 1、title = 1 行目 80 文字） | `crates/taskctl/src/commands/{add,plan}.rs` |
| `WorkerFinished.outcome` の書式: `done: <summary>` / `question: <text>` / `error(retryable=<bool>): <msg>` / `requeue: adapter: <e>` / `lease_expired` | `crates/task-dispatch/src/dispatcher.rs` |
| Human check の Approval 子の title: `Approval needed: <title> — criterion <idx> (attempt <attempts+1>)`。Reviewer run の延期は対象 run の `WorkerProgress` に `reviewer run requeued: ` 接頭辞 | 同上 |
| Reviewer run は `WorkerStarted` / `WorkerFinished` を残さない（進捗だけ `reviewer run <id>: ` 接頭辞で対象 run に付く）→ ADR-0014（P-G14）で `role: reviewer` 付きで記録するよう変更済み。Reviewer run も run 一覧と使用量の集計に現れる | 同上 `ReviewerSink` |
| アダプタ ID は `"fake"` / `"claude-code"` / `"codex"`。設定は `[adapters.fake|claude_code|codex]` と `[[providers]]{id, adapter, tiers, concurrency, model, env}` | `crates/taskd/src/config.rs` |
| `taskd --config <toml> [--until-idle] [--max-ticks N] [--log-format json|text]`。`taskctl --db <path>`（`TASKD_DB`）。`taskctl worker run` の exit code: done=0 / question=3 / error=4 / 130 | `crates/taskd/src/main.rs`、`crates/taskctl/src/main.rs`、ADR-0012 |
| ファイル: `<ws>/artifacts/`、`<ws>/runs/<run_id>/{stdout.jsonl, stderr.log, result.json}`。`<ws>` は `WorkspaceSpec::Local{path}`（相対なら `workspace_root` 基準） | `task-worker/src/{workspace,subprocess}.rs`、DESIGN §4.4 |
| e2e の流儀: `tempfile` の中に `taskd.toml`（`tick_ms = 50`、`[adapters.fake] command = ["sh", "<script>"]`）と `sh` のワーカースクリプトを書き、実バイナリ `taskd --until-idle` を起動して `taskctl replay` で差分ゼロを確認 | `tests/e2e/tests/*.rs` |

### 3.2 Phase 9（ADR-0013）で変わること = G フェーズの前提

| 項目 | 内容 |
|---|---|
| `task-ops` crate | approve / reject / answer / cancel / add / plan / replay の判断と検証、`TaskDetail` 等の派生ビュー。`taskctl` と `task-api` が共用。`taskctl show --json` |
| SQLite | WAL、`busy_timeout` 5000 ms、`synchronous=NORMAL`、`schema_migrations`（`SchemaTooNew`） |
| `events` | `id INTEGER PRIMARY KEY`（グローバル単調）、`UNIQUE(task_id, seq)`、`events_since(after_id, limit)` |
| `Event` | `JsonSchema` derive。`ProviderThrottled{provider, until, reason?}` を `Requeue` と同じトランザクションで記録 |
| `tasks` | `title` / `updated_at` の非正規化列、`list_page` / `count_by_status` |
| `task-api` | axum。`/api/v1`（`taskd-api-v1.md` の 25 エンドポイント）。`[api]` で有効化。Bearer / Host 検査 / パス検査。`DaemonSnapshot` を `watch` で受ける |
| スキーマ | `docs/api/v1/api-v1.schema.json`（1 ファイル）と一致テスト |

G フェーズは「taskd API v1 は `taskd-api-v1.md` のとおり実装済み」を前提にしてよい。ただし細部が変わりうるので、**G0 で `GET /api/v1/health` とスキーマファイルの存在を確認**する（§10.0）。

## 4. ユースケースと画面

原則 5 に従い、**受信箱を既定の画面**にする。他の画面は受信箱から辿れる補助。各画面の中身は `GET /api/v1/...` の応答をそのまま描く（派生値の規則は `taskd-api-v1.md` §5）。

### 4.1 受信箱（Inbox）— `GET /inbox`

| 区画 | 中身（`Inbox` の項目） | 操作（`POST /tasks/{id}/...`） |
|---|---|---|
| 承認待ち `approvals[]` | 親の title / objective、条件の文（`criterion_text`）、試行番号（`attempt`）、親の直近 run の summary（`last_run.outcome_text`）、evidence、同 run の他条件の判定（`other_verdicts`）、成果物、以前の承認 / 却下と note（`previous_decisions`） | Approve / Reject（note 任意。Reject は note 推奨: 次 run の `prior_review` に届く）。`expected_status: "ready"` 付き |
| 質問 `questions[]` | 質問文、過去の `Answered` 履歴、run のログ末尾へのリンク | Answer（複数行）。`expected_status: "blocked"` |
| 受け入れ待ちの draft `drafts[]` | Plan ごとにまとめる。Plan の objective と summary、子の title / objective / acceptance / tier / depends_on | Accept（1 件ずつ、または「この Plan の子を全部」= 子ごとに `approve` を直列に呼ぶ。**原子性は無い**ことを UI に明記）/ Cancel |
| 注意 `attention[]` | `failed`（直近 24h、理由）、連続 requeue が上限間近、経路なし（`unroutable`。デーモンのスナップショット由来） | Cancel、詳細へ |

承認待ちの件数（`counts.approvals`）はタイトルバーに常時出す（root の loader が `GET /inbox` の `counts` だけを使う…のではなく、`GET /tasks` の `counts_by_status` と `GET /inbox` を root で 1 回呼ぶ。SSE で再検証）。

### 4.2 タスク一覧 / ツリー / DAG — `GET /tasks`、`GET /graph`

- 一覧: `status` / `kind` / `parent` / `root_only` / `q`（title）で絞り込み、`order = dispatch | updated_desc | created_desc`。フィルタと `cursor` は URL の検索パラメータ。
  サーバ側 keyset ページング（`limit` 既定 100）+ 仮想スクロール（`@tanstack/react-virtual`）。「さらに読む」で `next_cursor` を進める。
- ツリー: `parent_id` による入れ子（`taskctl ls --tree` と同じ）。一覧の `parent` フィルタで掘る。
- DAG: `GET /graph`（`root` / `depth` / `include_terminal`）。ノード = タスク、辺 = `depends_on`、親子は group ノード。色 = status、枠 = kind。層状レイアウト（dagre）はクライアント。
  数百ノードまでを想定し、それ以上は `root` で絞る（API は 5,000 ノードで 422）。

### 4.3 タスク詳細 — `GET /tasks/{id}`（`TaskDetail` = `taskctl show --json`）

| 節 | 中身 | 出所（`TaskDetail` のフィールド / エンドポイント） |
|---|---|---|
| ヘッダ | id、kind、status、title、priority、`worker_hint`、`attempts / max_retries`、workspace の実パス、budget、parent / depends_on / dependents / children（各 status 付き） | `task`, `workspace_dir`, `dependencies`, `dependents`, `children` |
| タイマー | リースの残り、バックオフ解除まで、連続 requeue / `max_requeues`、Reviewer run の延期回数。計算は `timers.now` 基準 | `timers`。`running` の間は 5 秒ごとに再取得（`renew_lease` はイベントを出さない） |
| 受け入れ条件と判定 | 各 `Criterion` の種別、直近 run の `ReviewVerdict`、`Human` は対応する Approval 子（状態・note・リンク） | `criteria[]`（`latest_verdict`, `approval`） |
| run 一覧 | adapter / provider / model / 開始・終了 / 所要 / outcome（done / question / error / requeue / lease_expired）/ usage / 成果物数 / 判定数 / ファイルの有無 | `runs[]`（`GET /tasks/{id}/runs` と同じ） |
| イベントのタイムライン | `seq` 順。種別でフィルタ、`WorkerProgress` は折りたたみ | `GET /tasks/{id}/events?after_seq=&types=` |
| 生ログ | run を選び、`stdout.jsonl` を構造化表示（claude-code の stream-json / codex の `item.*` を「発話 / ツール / 結果」に整形。fake の JSON Lines は生表示）と生テキスト、`stderr.log` 末尾、`result.json` 整形。実行中の run は追尾（`?offset=` を 1 秒ごと） | `GET /tasks/{id}/runs/{run_id}/{stdout,stderr,result}`（BFF の `/files/...` 経由） |
| 成果物 | `GET /tasks/{id}/artifacts` の一覧（name / path / kind / 記録 sha256 / 現在の一致）。閲覧（md / json / log / diff / text / 画像）と保存。`sha256_matches = false` は警告 | `GET /tasks/{id}/artifacts`, `/artifacts/{idx}` |
| prior_review / answers | 次 run に渡る `context.prior_review` と `context.answers`（ディスパッチャと同じ関数で組み立てたもの） | `prior_review`, `answers` |
| 操作 | `actions[]` にあるものだけボタンを出す。押すときは `expected_status` を付け、409 は「状態が変わりました」として再取得 | `POST /tasks/{id}/{approve,reject,answer,cancel}` |
| 補助 | `worker_run_hint`（`taskctl worker run …` のコピー用。GUI は実行しない） | `worker_run_hint` |

### 4.4 作成（タスク / Plan）— `POST /tasks`、`POST /plans`

- タスク: `NewTaskSpec` と 1:1 のフォーム（title、objective、受け入れ条件ビルダー: Human / Command / ArtifactExists / Reviewer、kind、tier、priority、parent、depends_on の選択、budget、workspace）。
  検証は **taskd（task-ops）が行い**、422 の `errors[]` をフィールドに対応づけて表示する。GUI 側の検証は「必須欄が空」程度に留め、文言は taskd のものを出す。作成後は詳細へ遷移（`draft`、Approval は `ready`）。
- Plan: `NewPlanSpec`（goal、workspace、tier、priority、budget）。`GET /config` の `plan_auto_accept` を表示（false なら「子は受信箱に来る」と説明）。

### 4.5 プロバイダ（アカウント）— `GET /providers`

| 列 | 出所 |
|---|---|
| id / adapter / tiers / concurrency / 実効 model / `env_keys`（**値は出さない**） | 設定（`ProviderView`） |
| 実行中の run 数（`in_use / concurrency`） | デーモンのスナップショット |
| cooldown 中か、いつまで、理由（throttled / auth_failed / exhausted / spawn） | スナップショット `cooldown`（履歴は `ProviderThrottled` イベント） |
| run 数、done / question / error / requeue / lease_expired の内訳、input / output tokens（日別・累計） | `stats`（taskd がイベントから集計。Reviewer run も含む。ADR-0014） |

### 4.6 デーモン — `GET /daemon`、`GET /config`、`POST /replay`

生存（API に繋がるか。`last_tick_at` が `3 × tick_ms` 以上古ければ「遅延」）、pid / host / 起動時刻 / tick 回数、設定の要約（`GET /config`）、実行中 run（task / run_id / provider / 開始からの経過 / worker か reviewer か）、
承認待ちで延期中（`awaiting_human`）、経路なし（`unroutable`）。`replay` ボタンで `POST /replay` を実行して差分を表示。status 別件数は `counts_by_status`。
API に繋がらないときは全画面に「taskd に接続できません」を出し、操作を無効化する（§6.5）。

### 4.7 `taskctl show` との関係（P-39）

`TaskDetail` は taskd 側 `task-ops` に置き、`taskctl show --json` と `GET /tasks/{id}` が同じ JSON を返す（ADR-0013 D12）。人間向けテキスト表示（`taskctl show`）の改善は引き続き taskd 側の後回し。

## 5. 情報の出所の表（taskd API v1 のどのエンドポイントか）

| 画面 / 項目 | エンドポイント | taskd 側の出所 | 更新の契機（GUI） |
|---|---|---|---|
| 受信箱（承認待ち / 質問 / draft / 注意） | `GET /inbox` | task-ops（DB）+ スナップショット（unroutable） | SSE `task.event` / `daemon` |
| タイトルバーの件数 | `GET /inbox`（`counts`） | `count_by_status` | 同上 |
| 一覧 / ツリー | `GET /tasks` | `list_page` | SSE `task.event`、URL 変更 |
| DAG | `GET /graph` | task-ops | SSE `task.event` |
| 詳細: ヘッダ・条件・判定・run 一覧・prior_review・answers・操作 | `GET /tasks/{id}` | task-ops `TaskDetail` | SSE `task.event`（`?task_id=`） |
| 詳細: タイマー | `GET /tasks/{id}`（`timers`） | task-ops + 設定 | 上記 + `running` 中は 5 秒ごと |
| 詳細: タイムライン | `GET /tasks/{id}/events` | `events_for` | SSE `task.event` |
| 詳細: 生ログ / result.json / 成果物本体 | `GET /tasks/{id}/runs/{run_id}/{stdout,stderr,result}`、`GET /tasks/{id}/artifacts/{idx}` | ファイル（taskd がパス検査） | 追尾は `?offset=` を 1 秒ごと |
| 詳細: 成果物一覧と sha256 | `GET /tasks/{id}/artifacts` | events + ファイル | SSE `task.event` |
| 作成 | `POST /tasks`、`POST /plans` | task-ops（検証・挿入） | action 完了で loader 再検証 |
| 操作 | `POST /tasks/{id}/{approve,reject,answer,cancel}` | task-ops → 状態機械 | 同上 |
| プロバイダ: 定義 / 使用量 / cooldown / 並列度 | `GET /providers` | 設定 / task-api の集計 / スナップショット | SSE `daemon` |
| デーモン | `GET /daemon` | `watch` | SSE `daemon` |
| 設定の要約 | `GET /config` | 設定 | 起動時 1 回 |
| replay | `POST /replay` | task-ops | 手動 |
| 生存 | `GET /health`（無認証） | task-api | root loader が毎回 |
| リアルタイム | `GET /stream`（BFF が `/events` で中継） | `events_since` + `watch` | 常時 |

## 6. アーキテクチャ

### 6.1 構成（ADR-GUI-0001 / ADR-0013）

```
ブラウザ ──HTTP(S)──▶ taskd-gui（Node 24、React Router 8 framework mode、SSR。BFF）
   ▲  EventSource /events        │ loader / action / resource route が fetch
   └─ HTML + hydration            ▼
                              taskd（デーモン）: task-api（axum、/api/v1）──▶ task-ops ──▶ 状態機械 ──▶ SQLite（WAL）
                                    ▲  watch::Receiver<DaemonSnapshot>                          ▲
                                    └── ディスパッチャ（tick ごとに送る）                 taskctl（従来どおり）
```

- `taskd-gui` は 1 つの Node プロセス（`server.js`）。taskd の API 以外に何も開かない（DB / ファイルシステム上のワークスペースにも触れない）。
- taskd の API は既定で loopback（`127.0.0.1:7710`）。`taskd-gui` も既定で loopback（`127.0.0.1:7700`）。研究者はブラウザを手元で開くか、SSH ポートフォワードで 7700 だけを手元に引く。

### 6.2 `taskd-gui` のルート構成（`app/routes.ts`。明示的な定義）

| ルート | パス | loader が呼ぶ API | action |
|---|---|---|---|
| `root` | — | `GET /health`（生存）、`GET /inbox`（件数） | — |
| `inbox` | `/` | `GET /inbox` | — |
| `tasks` | `/tasks` | `GET /tasks?…`（URL の検索パラメータをそのまま渡す） | — |
| `tasks.new` | `/tasks/new` | `GET /tasks?status=…`（依存候補）、`GET /config` | `POST /tasks` |
| `tasks.$id` | `/tasks/:id` | `GET /tasks/{id}`、`GET /tasks/{id}/events`、`GET /tasks/{id}/artifacts` | `intent = approve \| reject \| answer \| cancel` → 対応する `POST` |
| `tasks.$id.runs.$runId` | `/tasks/:id/runs/:runId` | `GET /tasks/{id}/runs`（要約）。本体は `/files/...` | — |
| `plans.new` | `/plans/new` | `GET /config` | `POST /plans` |
| `graph` | `/graph` | `GET /graph?…` | — |
| `providers` | `/providers` | `GET /providers` | — |
| `daemon` | `/daemon` | `GET /daemon`、`GET /config` | `intent = replay` → `POST /replay` |
| `login` | `/login` | — | パスワード検証 → セッションクッキー（非 loopback のみ） |
| resource `events` | `/events?task_id=` | `GET /stream`（SSE をそのまま中継。`Last-Event-ID` 転送） | — |
| resource `files` | `/files/tasks/:id/runs/:runId/:name`、`/files/tasks/:id/artifacts/:idx` | 対応するファイル系 API（`Range` / `offset` / `download` を転送。応答ヘッダも転送） | — |
| resource `healthz` | `/healthz` | 無し（GUI 自身の生存。taskd を呼ばない） | — |

### 6.3 データの流れ

1. **読み取り**: ブラウザ → `taskd-gui` の loader（サーバ）→ `TaskdClient`（`app/taskd/client.server.ts`。`TASKD_API_URL`、`TASKD_API_TOKEN_FILE` から構成）→ taskd。応答は `loaderData` として SSR / クライアント遷移の両方で使う。
   `TaskdClient` は `api-v1.schema.json` から生成した型だけを使い、`application/problem+json` を `TaskdError{status, code, detail, extra}`、接続失敗・タイムアウトを `TaskdUnavailable` に変換する。
2. **更新**: `<Form method="post">` または `useFetcher()` → action → `TaskdClient.post(...)`（`expected_status` 付き）→ 成功なら `TransitionResult` を flash に載せて loader を再検証、
   `TaskdError` は `data({error}, {status})` でフォームに戻す（422 の `errors[]` はフィールドへ、409 は「状態が変わりました」）。
3. **リアルタイム**: `useTaskdStream({taskId?})` フック（root で 1 本）が `EventSource("/events")` を開き、`task.event` / `daemon` を受けたら `useRevalidator().revalidate()`（250 ms デバウンス）。
   `reset` を受けたら同じく再検証。`error` が続いたら「接続が切れました」表示（自動再接続は `EventSource` 任せ）。**イベント本体から状態を組み立てない。**
4. **ファイル**: 生ログ・成果物は resource route が taskd の応答を**ヘッダごと**中継する（`Content-Type` / `Content-Disposition` / `X-Content-Type-Options` / `X-Taskd-*`）。ビューアは `/files/...` を `fetch` して表示する。
5. **設定の要約**は root loader で 1 回取り、`useRouteLoaderData("root")` で共有。

### 6.4 SSE の中継

- `app/routes/events.ts`（default export 無し）: `loader({request})` が `TaskdClient.stream(taskId, lastEventId, request.signal)` で taskd の `GET /stream` を開き、その `body`（ReadableStream）を
  `new Response(body, {headers: {"Content-Type": "text/event-stream", "Cache-Control": "no-store", "X-Accel-Buffering": "no"}})` で返す。バイト列は加工しない（`id:` 行もそのまま）。
- ブラウザが切ればタスクの `request.signal` で上流を閉じる。taskd 側の 503 `too_many_streams` は同じ status で返す。
- `Last-Event-ID`（ブラウザの `EventSource` が再接続時に付ける）はそのまま転送。

### 6.5 taskd 停止時の挙動

- root loader の `GET /health` が `TaskdUnavailable` → HTML は描くが、全ページに「taskd に接続できません（`TASKD_API_URL`）」のバナーと、操作ボタンの無効化。子ルートの loader も同じ例外を投げ、
  各ルートの `ErrorBoundary` が同じバナーを出す（500 にしない）。
- 5 秒ごとに root だけ再検証（`useRevalidator`）し、復旧したらバナーを消す。SSE は `EventSource` が自動で再接続する。
- `taskctl` は従来どおり使える旨をバナーに書く。

### 6.6 状態変更の経路

GUI の全操作は taskd の `POST` → `task-ops` → `TaskStore::apply_transition(_with_events)` / `create_task`。GUI は SQL も判断ロジックも持たない。

## 7. API 概要

詳細は [`taskd-api-v1.md`](taskd-api-v1.md)。要点:

- 25 エンドポイント。読み取り: `/health` `/inbox` `/tasks` `/tasks/{id}` `/tasks/{id}/events` `/tasks/{id}/runs` `/tasks/{id}/runs/{run_id}/{stdout,stderr,result}` `/tasks/{id}/artifacts[/{idx}]` `/graph` `/events` `/stream` `/providers` `/daemon` `/config` `/schema`。
  変更: `POST /tasks` `/plans` `/tasks/{id}/{approve,reject,answer,cancel}` `/replay`。
- `GET /daemon` はメモリ（`watch`）から。SSE は `hello` / `task.event`（`id:` = events の id）/ `daemon` / `heartbeat` / `reset`。`Last-Event-ID` で再開。
- `/health` は `api_version` / `schema_version` を返す（無認証）。Bearer は `token_file` 設定時（非 loopback では必須）。`Host` 検査。CORS 無し。変更系は `Content-Type: application/json` 必須、`Origin` があれば 403。
- エラーは `application/problem+json` + `code`。ページングは keyset（`cursor`）。変更系は `expected_status` で 409 `conflict`。
- 型は Rust（`schemars`）→ `docs/api/v1/api-v1.schema.json`（1 ファイル）→ `json-schema-to-typescript` → `app/taskd/types.ts`。

## 8. セキュリティ

二段の境界がある: (A) `taskd-gui` ↔ taskd API、(B) ブラウザ ↔ `taskd-gui`。

### 8.1 (A) BFF ↔ taskd

- トークンは `TASKD_API_TOKEN_FILE` からサーバ起動時に読み、メモリに持つ。**ブラウザに渡さない**（HTML / loaderData / クッキー / ログのいずれにも出さない）。
- `TASKD_API_URL` は既定 `http://127.0.0.1:7710`。別ホストの taskd を指す場合は SSH ポートフォワードを推奨（`http://` の平文を LAN に出さない）。
- BFF は `Host` / `Origin` を taskd に転送しない（自分の `Host` 検査は 8.2）。

### 8.2 (B) ブラウザ ↔ taskd-gui（React Router の middleware で実装）

| 事項 | 決め |
|---|---|
| バインド | 既定 `127.0.0.1:7700`（`TASKD_GUI_BIND`）。SSH: `ssh -L 7700:127.0.0.1:7700 host` |
| 非 loopback | `TASKD_GUI_PASSWORD_FILE` が無ければ起動を拒否（exit 2、stderr に理由）。あれば `/login` でパスワードを受け、署名付き `HttpOnly; SameSite=Strict; Path=/`（https なら `Secure`）のセッションクッキーを発行。署名鍵は `TASKD_GUI_SESSION_SECRET_FILE`、無ければプロセスごとに乱数（再起動でログアウト）。パスワード比較は定数時間、失敗は 1 秒待つ |
| loopback | 認証無し（従来どおり） |
| DNS rebinding | `Host` を許可リスト（`localhost`, `127.0.0.1`, `[::1]`, バインドのホスト, `TASKD_GUI_ALLOWED_HOSTS`）で検査。外れれば 400 |
| CSRF | 変更系（action）は `Origin` があれば自分のオリジンと一致、`Sec-Fetch-Site` があれば `same-origin` / `none` であること。違えば 403。`SameSite=Strict` と併用 |
| CSP | `default-src 'self'; script-src 'self' 'nonce-<r>'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'`。nonce は middleware が要求ごとに生成し `<Scripts nonce>` / `<ScrollRestoration nonce>` に渡す（React Router の SSR が出す hydration スクリプトのため）。`style-src 'unsafe-inline'` は Tailwind と React Flow / CodeMirror のインラインスタイルのため（G5 で外せるか確認） |
| その他ヘッダ | `X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer`、`Cache-Control: no-store`（HTML と `.data`）。`build/client` のハッシュ付きアセットだけ `immutable` |

### 8.3 成果物・ログの描画（LLM が書いた信用できない内容）

- 本体は resource route が taskd の応答をそのまま中継する（taskd がテキスト系か `application/octet-stream` しか返さない。`text/html` は来ない）。同一オリジンの `<a download>` / `<img>`（png/jpeg/gif/webp のみ）で使う。
- Markdown は `react-markdown`（HTML パススルー無し、`remark-gfm`）で描画。リンクは `rel="noopener noreferrer"`、外部リンクは新規タブ。
- JSON / ログ / diff / ソースは CodeMirror（読み取り専用）に文字列として渡す。`dangerouslySetInnerHTML` は使わない（Biome の `noDangerouslySetInnerHtml` を error）。
- stream-json の整形は JSON をパースして React 要素にする（文字列連結で HTML を作らない）。

### 8.4 その他

- `GET /config` の `env_keys` はキー名だけ（taskd 側で値を落とす）。GUI もログに要求本文・トークンを出さない。
- 依存: `pnpm audit --audit-level=high` 0 件を CI で毎週。`minimumReleaseAge` 7 日、`strictDepBuilds`。
- GUI も taskd の API もプロセスを起動しない。`Check::Command` とワーカーは taskd のディスパッチャだけが実行する。

## 9. 配布（Node のサーバ）

| 事項 | 決め |
|---|---|
| ランタイム | Node **24 LTS**（React Router 8 の最低 22.22.0）。開発機の Node 22.21.0 は要更新 |
| ビルド | `pnpm install --frozen-lockfile && pnpm build`（`react-router build` → `build/server/index.js` + `build/client/`） |
| サーバ | `server.js`（ESM、`@react-router/express` + Express 5）: 環境変数を検証（`TASKD_API_URL`、`TASKD_API_TOKEN_FILE`、`TASKD_GUI_BIND`、非 loopback なら `TASKD_GUI_PASSWORD_FILE`）→ `build/client` を配信（`/assets` は `immutable`）→ React Router のハンドラ。失敗は exit 2 |
| 起動 | `node server.js`（または `pnpm start`）。ログは stderr に JSON 1 行 / 要求（パスと status と所要。本文は出さない） |
| リリース物 | `dist/taskd-gui-<version>.tar.gz` = `build/` + `server.js` + `package.json` + `pnpm-lock.yaml` + `README.md` + `deploy/taskd-gui.service`（systemd の例）。展開先で `pnpm install --prod --frozen-lockfile --ignore-scripts` |
| systemd | `deploy/taskd-gui.service`（`User=taskd`、`Environment=...`、`ExecStart=/usr/bin/node server.js`、`Restart=on-failure`）。taskd 本体の unit の `After=` に置く |
| コンテナ | 任意。`Dockerfile`（`node:24-slim`、非 root、`build/` と本番依存だけ）。`TASKD_API_URL` でホストの taskd を指す（`--network host` か loopback のポートフォワード） |
| 単一バイナリ | Node SEA は実験項目（ADR-GUI-0002 D6）。G5 で `esbuild`/`rolldown` によるサーバ 1 ファイル化 + `node --build-sea` を試し、結果を PROGRESS に記録する。失敗しても G5 は完了できる |
| 開発 | `pnpm dev`（`react-router dev`、HMR）。taskd は `scripts/taskd.sh start dev` で起動 |
| 版 | `package.json` の `version`。`GET /healthz` と画面のフッタに GUI の版と taskd の `taskd_version` / `api_version` / `schema_version` を出す |

## 10. 実装フェーズ（`taskd-gui` リポジトリ。`run-gphases.sh` で 1 フェーズずつ自律進行）

### 10.0 前提と共通規則

**前提**

- taskd は隣のリポジトリ **`../agent-platform`**（環境変数 `TASKD_REPO` で上書き可）にあり、taskd の Phase 9a / 9b（ADR-0013）が完了している。
  `scripts/taskd.sh build` が `(cd "$TASKD_REPO" && cargo build -p taskd -p taskctl)` を実行し、`$TASKD_REPO/target/debug/{taskd,taskctl}` を使う。
- `scripts/taskd.sh start <name>` は `.run/<name>/` に `taskd.toml`（fake ワーカー: `[adapters.fake] command = ["sh", ".run/<name>/fake-worker.sh"]`、`tick_ms = 200`、
  `retry_backoff_base_secs = 0`、`[plan] auto_accept = false`、`[[providers]] id = "fake-local" adapter = "fake" concurrency = 2 model = "fake"`、**`[api] listen = "127.0.0.1:7710"`**）と DB・`workspaces/` を作って
  `taskd --config .run/<name>/taskd.toml --log-format text` をバックグラウンドで起動する（pid を `.run/<name>/taskd.pid`、ログを `.run/<name>/taskd.log`）。`stop` / `status` / `logs` もある。
  `scripts/taskd.sh fixture <scenario>` は `taskctl`（`--db .run/<name>/taskd.sqlite3`）と `taskd --until-idle` で既知の DB を作る（シナリオは各フェーズで定義。taskd の `tests/e2e/tests/*.rs` と同じ流儀）。
- GUI は `TASKD_API_URL=http://127.0.0.1:7710` で taskd を指す。結合テスト（Playwright）は**その実 taskd** に対して行う。単体テストは `test/mock-taskd/`（プロセス内 HTTP サーバ、固定応答）で動く。
- **テストは外部ネットワークに出ない。** 例外は `pnpm install`（パッケージ取得）と `pnpm exec playwright install chromium`（ブラウザ取得）の 2 つだけで、どちらもテスト実行中ではなく準備で行う。
- G0 で **`GET /api/v1/health` の `api_version == "1"`** と **`$TASKD_REPO/docs/api/v1/api-v1.schema.json` の存在**を確認する。無ければ G0 は BLOCKED（下記）。

**各フェーズ共通の完了条件**（taskd の DESIGN §6 と同じ書式。証拠はコマンド出力か Playwright の操作で示す）

1. `pnpm lint`、`pnpm typecheck`、`pnpm test`、`pnpm build` が exit 0（テスト数を報告）。G1 以降は `pnpm e2e`（Playwright、実 taskd）も exit 0。
2. `pnpm gen:types && git diff --exit-code app/taskd/types.ts` が差分ゼロ（生成物がコミット済み）。
3. auditor サブエージェントの監査が「可」または「条件付き可」で「不可」ゼロ。
4. `docs/PROGRESS.md` に `## Phase G<N> — DONE` の節（受け入れ条件ごとの証拠、監査結果、未解決事項、提案、taskd への依頼）。
5. `git status` がクリーンで、直近のコミットが `phase G<N>:` で始まる。

**taskd の API が足りない・仕様と違うと分かったときの扱い**

- **`taskd-gui` 側で回避しない**（SQLite を開かない、`taskctl` の出力を解析しない、派生値を再計算しない、仕様外のフィールドや挙動に頼らない）。
- `docs/taskd-requests.md` に「エンドポイント / 期待（`docs/taskd-api-v1.md` の節）/ 実際（`curl` の出力）/ GUI で何ができないか」を追記し、
  `docs/PROGRESS.md` に `## Phase G<N> — BLOCKED` を書いてコミットし、**止まる**。オーケストレータが taskd 側を直してから同じフェーズを再開する。
- 仕様の**曖昧さ**（どちらとも読める）は、taskd の実際の挙動に合わせて進めてよい。ただし PROGRESS の「提案」に「`taskd-api-v1.md` の §x をこう明確化すべき」と書く。

**モデルとターン数の目安**

| フェーズ | 内容 | モデル | 最大ターン |
|---|---|---|---|
| G0 | 骨組みと前提の確定 | strong | 60 |
| G1 | 読み取りとストリーム | light | 50 |
| G2 | 操作 | strong | 60 |
| G3 | ログ・成果物・DAG | light | 50 |
| G4 | プロバイダとデーモン | light | 40 |
| G5 | 認証・配布・仕上げ | strong | 60 |

strong = 設計判断（雛形の選択、BFF の骨格、CSRF・認証、配布）を含むフェーズ。light = 仕様どおりに画面を積むフェーズ。

### Phase G0 — 骨組みと前提の確定（strong, 60）

- リポジトリ: `pnpm create react-router@latest`（`node-custom-server` テンプレート）→ TypeScript 7（動かなければ 6.0.x）、Biome、Vitest、Playwright、Tailwind 4、shadcn/ui（`-t react-router`）、
  `pnpm` の設定（`minimumReleaseAge = 10080`、`strictDepBuilds`、`packageManager`）。`package.json` の版は完全固定。
- `scripts/taskd.sh`（build / start / stop / status / fixture の骨組み）、`test/taskd/taskd.toml.tmpl`、`.run/` を `.gitignore`。
- 型生成 `pnpm gen:types`（`$TASKD_REPO/docs/api/v1/api-v1.schema.json` → `app/taskd/types.ts`）。
- `app/taskd/client.server.ts`（`TaskdClient`: `get` / `post` / `stream` / `file`、`TaskdError` / `TaskdUnavailable`）と `test/mock-taskd/`（`GET /health`、problem+json、接続拒否のケース）。
- root: `Host` 検査 middleware、CSP nonce、`GET /health` を出すトップ、taskd 停止時のバナー。`/healthz`。
- 受け入れ:
  1. `pnpm install --frozen-lockfile`、`pnpm lint`、`pnpm typecheck`、`pnpm test`、`pnpm build` が exit 0（`pnpm-lock.yaml` をコミット）
  2. `scripts/taskd.sh build && scripts/taskd.sh start dev` → `curl -s http://127.0.0.1:7710/api/v1/health | jq -r .api_version` が `1`、`test -f "$TASKD_REPO/docs/api/v1/api-v1.schema.json"` が exit 0
  3. `pnpm gen:types && git diff --exit-code app/taskd/types.ts` が差分ゼロ。`grep -c "export interface \(TaskDetail\|Inbox\|EventRow\|DaemonSnapshot\)" app/taskd/types.ts` が 4
  4. `pnpm build && node server.js &` → Playwright: `/` に taskd の `taskd_version` / `api_version 1` / `schema_version` が表示される。`scripts/taskd.sh stop dev` 後に `/` を開くと HTTP 200 で「taskd に接続できません」が表示される（例外で落ちない）
  5. `curl -s -o /dev/null -w '%{http_code}' -H 'Host: evil.example' http://127.0.0.1:7700/` が `400`
  6. `pnpm test` に `TaskdClient` の単体テスト: problem+json（409 `conflict`）が `TaskdError{status:409, code:"conflict"}` に、接続拒否が `TaskdUnavailable` になる

### Phase G1 — 読み取りとストリーム（light, 50）

- `scripts/taskd.sh fixture basic`: fake ワーカーで (a) 依存あり 3 タスク全て done、(b) Human check を持つタスク（Approval 子が `ready` で待つ）、(c) `question` で `blocked`、(d) `plan.auto_accept = false` の Plan と draft の子 2 件、(e) `failed` 1 件（`retryable: false`）。
- 画面: 受信箱、一覧（URL パラメータ、`next_cursor`、仮想スクロール）、詳細（`TaskDetail` の全節。生ログ・成果物本体は G3）、タイムライン（`types` フィルタ）。
- `/events` の中継と `useTaskdStream`、`useRevalidator` による再検証。
- `test/fixtures/api/*.json` を実 taskd から `curl` で採取してコミット（`scripts/capture-fixtures.sh`）。型付きで import し `pnpm typecheck` が形を検証する。
- 受け入れ:
  1. `scripts/taskd.sh fixture basic` のあと `taskctl --db .run/basic/taskd.sqlite3 ls` に done 3・ready（Approval）1・reviewing 1・blocked 1・draft 2・done（Plan）1・failed 1 が含まれる（出力を PROGRESS に貼る）。`taskctl replay` が `0 mismatches`
  2. Playwright: `/` の承認待ちに `Approval needed:` の項目が 1 件（親の title、条件の文、summary が表示）、質問に (c) の質問文、draft に Plan の下の子 2 件、注意に failed 1 件
  3. Playwright: `/tasks?status=done` の行数が `taskctl ls --status done | wc -l` と一致。`/tasks?limit=2` で「さらに読む」を最後まで押して集めた id が重複なく全件と一致
  4. Playwright: `/tasks/<id>`（(b) の親）に条件と `Human` 条件の Approval 子へのリンク、run 1 件、タイムラインに `approval_requested` は無く（親ではなく子のイベント）、`worker_finished` がある。`curl /api/v1/tasks/<id>` の `runs.length` と画面の run 件数が一致
  5. SSE: `scripts/taskd.sh start basic` のまま Playwright で `/tasks` を開き、`taskctl add --title "sse probe" --objective x --accept y` を実行 → **3 秒以内**にリロード無しで `sse probe` が現れる。`curl -N -m 3 http://127.0.0.1:7700/events` の出力に `event: hello` と `event: task.event` が含まれる
  6. `pnpm test`: 各 loader の単体テスト（mock-taskd）と `useTaskdStream` のデバウンスのテストが通る（件数を報告）

### Phase G2 — 操作（strong, 60）

- 詳細と受信箱の action（`intent = approve | reject | answer | cancel`、`expected_status` 付き）、作成フォーム（`POST /tasks`）、Plan フォーム（`POST /plans`）、`replay`。
- CSRF（`Origin` / `Sec-Fetch-Site`）、flash（`TransitionResult.cascaded` の表示）、409 / 422 の表示。
- 受け入れ（`scripts/taskd.sh fixture basic` + `scripts/taskd.sh start basic` で実 taskd を並走）:
  1. Playwright: 受信箱で Approval を note 付きで承認 → `taskctl show --json <approval-id> | jq -r .task.status` が `done`、`.runs`… ではなく `curl /api/v1/tasks/<id>/events` に `approval_decided`（`approved: true`、note）がある。親タスクが SSE 経由で `done` に変わる（fake ワーカーの他条件は pass）。`taskctl replay` が `0 mismatches`
  2. Playwright: 2 つのページで同じ Approval を開き、片方で承認 → もう片方の承認は「状態が変わりました」（409 `conflict` または `invalid_transition`）と表示され、状態は変わらない
  3. Playwright: blocked のタスクに回答 → `ready`。`answered` イベントの `question` が画面に出ていた質問文と一致
  4. Playwright: 後続を持つ `ready` タスクを cancel → flash に `cascaded` の後続 id が出て、後続の status が `cancelled`（`reason: dependency_failed`）
  5. Playwright: 作成フォームで条件ゼロ → 422 の文言 `at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)` がそのまま表示。存在しない `depends_on` → `dependency <id> does not exist`。正しい入力 → `/tasks/<id>`（`draft`）へ遷移 → 承認 → `ready` → fake ワーカー並走で **30 秒以内**に `done`（SSE で自動更新）
  6. Playwright: Plan フォームで作成 → `draft` の Plan。フォームに `plan.auto_accept = false`（`GET /config`）の説明が出る
  7. `curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Origin: http://evil.example' -d 'intent=cancel' http://127.0.0.1:7700/tasks/<id>` が `403` で、状態は不変
  8. デーモン画面の replay ボタン → `POST /replay` の結果 `0 mismatches` が表示

### Phase G3 — ログ・成果物・DAG（light, 50）

- `/files/...` の中継、run のログビューア（`stdout.jsonl` の構造化表示: claude-code の stream-json / codex の `item.*` / それ以外は生表示、`stderr` 末尾、`result.json`）、追尾、成果物ビューア（CodeMirror / Markdown / 画像 / 保存）、sha256 警告、DAG（React Flow + dagre）。
- `test/fixtures/stream-json/` に claude-code / codex の出力例（taskd の `crates/task-worker/src/{claude_code,codex}.rs` のテストにある行を写す）。
- 受け入れ:
  1. Playwright: (a) の run を開くと `stdout.jsonl` の行数が `wc -l .run/basic/workspaces/<ws>/runs/<run_id>/stdout.jsonl` と一致し、`result.json` が整形表示される
  2. `pnpm test`: stream-json の整形が claude-code の例で「発話 / ツール呼び出し / 結果」の 3 種を出し、fake の JSON Lines は生表示になる
  3. Playwright: Markdown の成果物（fixture で `<script>alert(1)</script>` を含む md を出す）が**テキストとして**表示され `alert` は実行されない（`page.on("dialog")` が呼ばれない）。JSON は CodeMirror、PNG は `<img>`。「保存」で得たファイルの sha256 が `X-Taskd-Sha256` と一致
  4. 成果物ファイルを fixture 後に書き換える → 画面に sha256 不一致の警告
  5. mock-taskd で 403 `path_forbidden` を返す → 画面に「アクセスできません（path_forbidden）」（実 taskd での細工 DB は taskd 側のテストに任せる）
  6. Playwright: `/graph` で (a) の `depends_on` の辺が 1 本、Plan の子 2 件が Plan の group の中。スクリーンショットをベースラインとしてコミット（`toHaveScreenshot`）
  7. 追尾: 10 秒かけて progress を 5 回出す fake ワーカーのタスクを実行中に開くと、リロード無しで行が増える

### Phase G4 — プロバイダとデーモン（light, 40）

- `scripts/taskd.sh fixture multi-account`（fake 2 アカウント `acct-a` / `acct-b`、`acct-a` が 1 回 `provider_failure: throttled(300s)` を返す。taskd の e2e `throttled_account_falls_back_to_the_next_account` と同じ）と `fixture unroutable`（cheap のタスクに frontier だけのプロバイダ）。
- プロバイダ画面、デーモン画面、停止 / 復旧のバナー。
- 受け入れ:
  1. Playwright: `/providers` で `acct-a` が requeue 1、`acct-b` が done 1、tokens の合計が `curl /api/v1/tasks/<id>/runs` の `usage` の和と一致。`acct-a` に cooldown の残り時間が表示される（`until` はスナップショット由来）
  2. Playwright: `/daemon` に pid / hostname / `ticks` が表示され、5 秒後の再読込で `ticks` が増える。fixture (b) の親が `awaiting_human` に 1 件。`fixture unroutable` では受信箱の注意と `/daemon` の `unroutable` に同じ id
  3. `scripts/taskd.sh stop <name>` → **5 秒以内**に全ページに「taskd に接続できません」、`start` → 5 秒以内に消える。SSE が再接続して `task.event` が再び届く（`taskctl add` で確認）
  4. 遅い fake ワーカー（20 秒）の実行中、`/daemon` の `in_flight` に task / run_id / provider / 経過時間が出て、終了後に消える

### Phase G5 — 認証・配布・仕上げ（strong, 60）

- BFF → taskd のトークン（`TASKD_API_TOKEN_FILE`）、非 loopback のパスワード認証とセッションクッキー、CSP の確認、a11y、`server.js` と systemd の unit、リリース tar.gz、任意で Dockerfile、実験で Node SEA、README（導入手順）。
- 受け入れ:
  1. `TASKD_GUI_BIND=0.0.0.0:7700 node server.js` が `TASKD_GUI_PASSWORD_FILE` 無しで exit 2（stderr に理由）。あり: 未ログインの `GET /` が `/login` へ 302、誤パスワードは 401 ページ、正しいパスワードで `Set-Cookie` に `HttpOnly; SameSite=Strict`、`curl` でクッキー無しの `/events` が 401
  2. taskd を `token_file` 付きで起動し、GUI に `TASKD_API_TOKEN_FILE` を渡すと動く。渡さないとバナーに `unauthorized`。`grep -r "<token>" build/ .run/*/gui.log` が 0 件（トークンが HTML・ログに出ない）
  3. `curl -H 'Host: evil.example'` が 400。全ページの応答に `Content-Security-Policy` があり、Playwright の全シナリオでコンソールに CSP 違反が 0 件
  4. `@axe-core/playwright` で `/`、`/tasks`、`/tasks/<id>`、`/tasks/new`、`/providers`、`/daemon` の critical / serious が 0 件
  5. `pnpm audit --audit-level=high` が 0 件。`pnpm release` が `dist/taskd-gui-<version>.tar.gz` を作り、空ディレクトリに展開して `pnpm install --prod --frozen-lockfile --ignore-scripts && node server.js` で `/` が 200（Playwright の smoke）
  6. `deploy/taskd-gui.service` と README の導入手順がある。（任意）`docker build` が通る。（実験）Node SEA の試行結果が PROGRESS に記録されている（成否は問わない）

## 11. 人間の決定（決定済み）

| # | 事項 | 決定 |
|---|---|---|
| H1 | 境界 | **別プロセス、taskd の HTTP API 経由**（初版推奨の (d) 直接 DB ではなく (b)）。GUI は SQLite を開かず crate にも依存しない。ブラウザは taskd を直接呼ばない。taskd 停止中は GUI も操作不可 |
| H2 | フロントエンド | **React + Remix**。実体は React Router 8 framework mode（ADR-GUI-0002 §2。確認事項は同 §4） |
| H3 | `task-ops` の置き場 | 新 crate `crates/task-ops` |
| H4 | events の順序列 | 表の作り直し（`id INTEGER PRIMARY KEY`） |
| H5 | デーモン状態 | 毎 tick の DB スナップショットは採らない。`tokio::sync::watch` でメモリから API 層へ。`ProviderPolicy::cooldowns()`（既定実装つき）。`ProviderThrottled{provider, until, reason?}` を `Requeue` と同じトランザクションで記録 |
| H6 | GUI からの操作 | 許可（承認・回答・作成・cancel・plan）。taskd API 経由 |
| H7 | 型共有 | schemars → `docs/api/v1/*.schema.json`（taskd がコミット）→ TS |
| H8 | パッケージ管理 | pnpm、`minimumReleaseAge` 7 日 |
| H9 | 複数人利用 | 当面単一トークン。`by` は `"human"` のまま |
| H10 | taskd への追加提案 P-G14〜P-G16 | 提案どおり採用（Reviewer run のイベント記録、`q` の objective 検索、作成時の検証）。taskd の ADR-0014 で実装済み |
| H11 | ADR-GUI-0002 §4 の確認事項 | Fable の案どおり確定: Remix = React Router 8 framework mode、Node 24 LTS ランタイム + `build/` での配布（単一バイナリは実験項目）、Node 24 への更新が前提、pnpm 11、TypeScript 7（問題があれば 6.0.x）。`/health` は無認証のまま |

ADR-0013 で不採用・後回し: P-G4（DB スナップショット表）、P-G12（crate タグ）、P-G8 / P-G10 / P-G13。

## 12. 参考

- 本リポジトリ: `docs/DESIGN.md` §1, §4, §5, §6、`docs/adr/0009`〜`0013`
- taskd API v1: [`taskd-api-v1.md`](taskd-api-v1.md)。提案の状態: [`taskd-proposals.md`](taskd-proposals.md)
- 一次情報の出典一覧（Remix の実体、ライブラリの版・日付・ライセンス）: ADR-GUI-0002 §2, §8
- `taskd-gui` 立ち上げ: taskd リポジトリの `docs/gui/bootstrap/README.md`
