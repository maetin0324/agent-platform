# ADR-0013: taskd の HTTP API 層と、GUI のための基盤（task-ops・SQLite・events の id・スキーマ）

- 日付: 2026-09-14
- 状態: Accepted（人間の判断。`docs/gui/DESIGN-GUI.md` §11 の H1〜H9 への回答）
- 関連: `docs/DESIGN.md` §1, §5, §6 / `docs/gui/`（GUI 側の設計。ADR-GUI-0001 / 0002）/ `docs/gui/taskd-proposals.md`（P-G1〜P-G13）

## 文脈

Web GUI（別プロジェクト `taskd-gui`）の設計で、Fable の初版は「`taskd-gui` が task-core を使って SQLite を直接読み書きし、デーモンの
メモリ状態は taskd が毎 tick DB のスナップショット表に書く」案を推奨した。人間の判断は次のとおり:

- **H1**: `taskd-gui` は別プロセスだが、**taskd が提供する API 層を経由して**taskd を使う（GUI は SQLite を直接開かない）。
- **H2**: React を使い、フレームワークは Remix（GUI 側の決定。ADR-GUI-0002 で具体化）。
- **H3**: `task-ops` を新しい crate にする。**H4**: `events` を作り直してグローバルな単調 id を持たせる。
- **H5**: デーモン状態を毎 tick DB に書くのは非効率。**taskd から見える HTTP や gRPC などの API 層**でメモリ状態を公開する。
  そのために taskd の変更が必要なら変更する。
- **H6〜H9**: 推奨どおり（GUI からの操作を許可、型は schemars 経由、pnpm の cooldown 7 日、`by` は `"human"` のまま）。

## 決定

### D1. 境界: `taskd-gui` は taskd の HTTP API だけを使う

```
ブラウザ ──HTTP──▶ taskd-gui（Remix のサーバ。BFF）──HTTP/JSON + SSE──▶ taskd（API 層 + ディスパッチャ）──▶ SQLite
                                                                              ▲
                                                                   taskctl（従来どおり SQLite を直接）
```

- `taskd-gui` は SQLite を開かず、taskd の crate にも依存しない。契約は **HTTP API v1 と、コミットされた JSON Schema**だけ。
- ブラウザは taskd を直接呼ばない（Remix のサーバが loader / action から呼ぶ）。したがって taskd の API は CORS を出さない。
- taskd が止まっている間は GUI からの操作もできない。`taskctl` は従来どおり DB に直接触れて動く（非常時の手段）。

### D2. プロトコル: HTTP/1.1 + JSON + SSE（gRPC は採らない）

- ベースパス `/api/v1`。要求・応答は JSON、サーバからの通知は SSE（`text/event-stream`）。
- gRPC を採らない理由: 利用者は TypeScript のサーバ（Remix）と `curl` で、protobuf のツールチェーンと二つ目のスキーマ源（`.proto`）が増える。
  サーバ → クライアントの一方向ストリームは SSE で足り、既存の型定義（`serde` + `schemars` → JSON Schema）をそのまま契約にできる。
- WebSocket も採らない（双方向は不要。操作は POST）。

### D3. API 層の置き場: 新しい crate `crates/task-api`（axum）を taskd のプロセス内で動かす

- `task-api` はライブラリ（`pub fn router(state: ApiState) -> axum::Router`）。`taskd` は設定 `[api]` があるときだけ同じ tokio ランタイムで
  リッスンする（**既定は無効**。デーモンに暗黙のネットワーク口を開けない）。
- API は **自分専用の `SqliteStore` 接続**を持ち（ディスパッチャの接続と Mutex を共有しない）、DB 呼び出しは `spawn_blocking` で行う。
- **API のハンドラは協調判断をしない。** 読み取りはストアのクエリ、状態変更は `task-ops`（D7）経由の `TaskStore::apply_transition(_with_events)` /
  `create_task` だけ。LLM 呼び出し・ワーカーの起動・`Check::Command` の実行はしない（原則 1〜4）。
- 依存: `task-api` → `task-ops`, `task-core`, `axum`, `tokio`, `serde`, `serde_json`, `schemars`。`task-dispatch` と `task-worker` には依存しない
  （デーモンの状態は D4 の型で受け取る）。成果物・ログのパス検査は `task-api` 内に置く（ワークスペースの外に出ない）。

### D4. デーモン状態はメモリから直接公開する（DB に書かない）

- ディスパッチャは tick の最後に `DaemonSnapshot` を作り、`tokio::sync::watch::Sender` に送る（値の置き換えだけで I/O は無い）。API は
  `watch::Receiver` から最新値を読む。`GET /api/v1/daemon` と SSE の `daemon` イベントで返す。
- 内容: `instance_id`（起動ごとの ULID）, `pid`, `hostname`, `started_at`, `last_tick_at`, `ticks`, `tick_ms`, `in_flight[]`（task_id, run_id,
  provider, kind = worker | reviewer, since）, `cooldowns[]`（provider, until, reason）, `awaiting_human[]`, `unroutable[]`, `providers[]`
  （id, adapter, tiers, concurrency, 実効 model, 使用中の数。**env の値は含めない**）。
- cooldown は `StaticPolicy` のメモリにしか無いので、`ProviderPolicy` に既定実装つきの `fn cooldowns(&self, now: Instant) -> Vec<Cooldown>`
  を追加する（既定は空。供給層の既存ポリシーは壊れない）。`Instant` は送る時点で壁時計（RFC 3339）に直す。
- 真実ではなく観測値なので、`replay` の対象外。デーモンが止まると失われる（止まっていること自体は API に繋がらないことで分かる）。
- P-G4（`daemon_status` 表）は採らない。

### D5. SQLite: WAL・明示的な busy_timeout・スキーマ版数（P-G2 / P-G11）

- `SqliteStore::open` で `PRAGMA journal_mode=WAL`、`busy_timeout`（既定 5000 ms）、`synchronous=NORMAL`、`foreign_keys` は変えない。
  taskd のディスパッチャ・API・taskctl の 3 接続が同時に開くため。WAL はネットワークファイルシステム上では使えない（DB はローカルディスクに置く）。
- `schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT)` を導入し、マイグレーションを版数で管理する。既存 DB（表が無く `tasks` がある）
  は版数 1 とみなす。**DB の版数がバイナリの知る版数より新しければ `StoreError::SchemaTooNew` で開かない。**
- 移行は 1 トランザクション。移行の前後で `taskctl replay` の差分がゼロであることをテストで確認する。

### D6. `events` のグローバル単調 id（P-G3 / H4）

- マイグレーション 0002 で `events` を `id INTEGER PRIMARY KEY, task_id, seq, ts, json, UNIQUE(task_id, seq)` に作り直す（既存行は rowid 順に写す）。
  追記専用の不変条件は保つ（移行は表の作り直しで、行の内容・順序を変えない）。
- `TaskStore::events_since(after_id: u64, limit: usize) -> Vec<EventRow{id, task_id, seq, ts, event}>` を追加。SSE はこれを API の接続で
  ポーリングする（taskctl の書き込みも同じ経路で拾う。in-process 通知は使わない）。
- `events_for`（`seq` 順）と `replay` の規則は変えない。

### D7. `task-ops` crate（P-G1 / H3）

- `crates/task-ops`: `taskctl` の approve / reject / answer / cancel / add / plan / replay の**判断と検証**、派生ビュー
  （質問文・answers・prior_review・連続 requeue・バックオフ・Human check の Approval 子の対応・`TaskDetail`）を移す。
- `taskctl` は引数解析と出力整形だけ、`task-api` は HTTP の写像だけ、ディスパッチャは派生関数（`prior_review` / `answers` / `consecutive_requeues` 等）を
  `task-ops` から使う。挙動は変えない（既存テストは移して全て通す）。
- 依存は `task-core` のみ（`task-worker` / `task-dispatch` に依存しない）。ワーカープロトコルの型（`PriorReview` / `Answer`）への写像は
  ディスパッチャ側で行う。ビューの型は `schemars::JsonSchema` を derive する。
- 呼び出し主体（`by`）は当面 `"human"` 固定（H9）。

### D8. 型とスキーマ（P-G6 / H7）

- `Event` に `JsonSchema` を derive する。API の要求・応答の型（`task-ops` のビュー型、`task-api` の要求型、`EventRow`、`DaemonSnapshot` 等）の
  JSON Schema を `docs/api/v1/*.schema.json` に生成してコミットし、既存の `committed_schema_matches_generated` と同じ一致テストを置く
  （`UPDATE_SCHEMA=1` で再生成）。`taskd-gui` はこのファイルから TypeScript の型を生成する。
- `GET /api/v1/health` は `api_version`（`"1"`）と DB の `schema_version` を返す。互換性を壊す変更は `/api/v2` で行う。

### D9. `ProviderThrottled` を記録する（P-G5）

cooldown に入る供給側失敗を観測した run のタスクに、`Requeue` の遷移と同じトランザクションで
`Event::ProviderThrottled{provider, until, reason?}` を追記する（`reason` は `throttled | auth_failed | exhausted | spawn`、任意フィールド）。

### D10. 一覧のページングと検索（P-G7）

`tasks` に `title` と `updated_at` の非正規化列と索引を追加し（マイグレーション 0003）、`TaskStore::list_page(filter, order, cursor, limit)`、
`count_by_status()` を追加する。keyset ページング（`order = dispatch` は `priority DESC, created_at ASC, id`、`updated_desc` 等）。

### D11. セキュリティ

- 既定のバインドは `127.0.0.1`。**loopback 以外にバインドする設定では `token_file` を必須**にし（無ければ設定エラー）、`Authorization: Bearer` を要求する。
- `Host` ヘッダを許可リスト（`localhost`, `127.0.0.1`, `[::1]`, 設定値）で検査（DNS rebinding 対策）。変更系は `Content-Type: application/json` を要求。
- ファイル系エンドポイント（run のログ・成果物）はユーザ入力のパスを受け取らない。`run_id` は ULID 形式を検査し、成果物は `events` に記録された
  `ArtifactRef.path` を使い、作業ディレクトリに結合 → `canonicalize` → 作業ディレクトリ配下であることを確認する。`Content-Type` は
  テキスト系か `application/octet-stream` のみ、`X-Content-Type-Options: nosniff`。
- `[[providers]].env` の値、トークンは API に出さない。

### D12. 採らない・後回し

- P-G4（DB スナップショット表）→ D4 で置き換え。P-G12（crate のタグで GUI が依存）→ D1 で不要（HTTP API とスキーマのファイルが契約）。
- P-G8（Approval 子の構造化）、P-G10（`WorkerFinished` の exit_code / duration）、P-G13（`by`）は後回し。
- `taskctl show --json` は `TaskDetail` で提供する（P-G9。P-39 の CLI 表示の改善そのものは引き続き後回し）。

## 実装の順序（DESIGN.md §6 Phase 9）

1. **9a 基盤**: D5（PRAGMA・版数）、D6（events の id）、D7（task-ops の抽出）、D8（`Event` の JsonSchema）、D9（ProviderThrottled）、D10（ページング）。
2. **9b API**: D2〜D4、D8（API 型のスキーマ）、D11。エンドポイントの詳細は `docs/gui/api.md`（taskd が提供する API v1 の仕様として改訂したもの）に従う。

## 結果

- 新しい crate: `crates/task-ops`, `crates/task-api`。`taskd` に `[api]` 設定。`taskctl` は `task-ops` を使う。
- `docs/DESIGN.md` に API 層（§5.10）と Phase 9 を追加する（人間の方針「taskd に変更が必要なら変更する」に基づく）。
- `taskd-gui` 側の設計（Remix、BFF、画面、G フェーズ）は `docs/gui/` にあり、`run-gphases.sh` で自動進行する。

## 実装メモ（Phase 9b、2026-09-14。`docs/gui/api.md` が明示していなかった点の決め）

GUI から見える挙動は `docs/gui/api.md` §10 にも写してある。

**D5 の追補: 書き込みトランザクションは `BEGIN IMMEDIATE`（Phase 9 監査の「不可」への対応）**
- 監査で見つかった問題:
  - DEFERRED トランザクションは、読んだ後に書き込みへ格上げする。
  - WAL では、その間に別の接続が書き込むと格上げが SQLITE_BUSY で即座に失敗し、busy_timeout は効かない。
  - taskd の実行中に `taskctl` や API から書き込むと、ディスパッチャの tick が `database is locked` で失敗し、デーモンが終了していた（Phase 9 以前からある不具合）。
- 対応: `SqliteStore` の書き込みを全て `TransactionBehavior::Immediate` で始め、書き込みロックは busy_timeout の範囲で待つ。
  - 対象: `apply_transition_with_events` / `create_task` / `complete_plan` / `acquire_lease` / `renew_lease` / マイグレーション。
  - `append_event` は seq の採番と INSERT を 1 つのトランザクションに入れる。
  - `release_lease` は読んだ json を書き戻すので、同じくトランザクションに入れる。
- 回帰テスト:
  - task-core `concurrent_read_then_write_transactions_on_two_connections_wait_instead_of_failing`（修正前は `DatabaseBusy` で失敗）。
  - e2e `writes_from_taskctl_and_api_while_taskd_ticks_fast_never_hit_database_is_locked`（tick 20 ms の taskd に、taskctl と API から書き込み続ける）。

**taskd への組み込み**
- `[api] listen` があるときだけ、ディスパッチャに `SnapshotPublisher`（`watch::Sender`）を付け、API 専用の DB 接続を開いて bind する。
- 起動失敗の扱い:
  - `token_file` が読めない・空 → 設定読込時のエラー（exit 2）。
  - DB 接続・bind の失敗 → 起動失敗。黙って API 無しでは動かない。
- 停止: tick ループの終了後に shutdown を送り、SSE を閉じて最大 5 秒待つ。
- `instance_id` は起動ごとの ULID で、スナップショットと `/health` で同じ値を使う。
- `hostname` は `/proc/sys/kernel/hostname` → `HOSTNAME` → `"unknown"` の順で取る。
- `awaiting_human` は、reviewing のタスクだけを残すよう毎 tick 刈り込む。
- `GET /config` の `config_path` は `Config::load` が記録した絶対パス（`#[serde(skip)]`）。

**要求の検査**
- 順序: Host → `OPTIONS` の 405 → 認証 → POST の Origin / Content-Type / 本文サイズ。
  - 認証が有効な構成では、未定義のパスも 404 より先に 401 になる。
- 400 にするもの:
  - Host ヘッダが複数ある要求、absolute-form で許可されない authority。
  - 未知のクエリパラメータと、単一値のキーの重複（本文の `deny_unknown_fields` に揃え、BFF の誤りを早く表に出す）。
  - 数値でない `Last-Event-ID`。

**`GET /tasks/{id}` と `taskctl show --json`**
- 両者とも `task_ops::view::task_detail` の結果を compact な JSON に直列化する。
- 差は 2 つ: API は `runs[].files` を埋め（taskctl は `null`）、`timers.now` が応答時刻になる。
- taskctl は `taskd.toml` を読まないので、`ViewContext` は設定の既定値を使う（`--workspace-root` で上書き）。

**派生値の解釈**
- `RunSummary.outcome_text`: `done` 以外でも接頭辞を除いた残りを入れる（`error` は文字列全体）。
- `AttentionItem::Failed.reason`: 直近の `WorkerFinished.outcome` と、直近 run の不合格 `ReviewVerdict.reason` が両方あれば `; ` で結合する。
- `AttentionItem::Unroutable.at`: スナップショットの `last_tick_at`。
- `AttentionItem::RequeueLimitNear`: 一度も requeue していないタスクは含めない（`max_requeues = 1` で ready が全て並ぶのを防ぐ。監査）。
- `InboxCounts.drafts`: draft タスクの件数（グループ数ではない。監査）。子の件数は全件から 1 回だけ集計する（draft ごとに全件を読むと二乗になっていた。監査）。
- `DaemonSnapshot.in_flight[]` の Reviewer run の `run_id`: レビュー対象のワーカー run の id（Reviewer run 自身の id は events に残らない）。
- SchemaTooNew の DB では taskd は exit 2（設定エラーと同じ扱い）。
- 存在しない id の指定: `/graph?root=` は 404 `task_not_found`、`/events?task_id=` は空ページ。
- プロバイダ集計:
  - 最初の `GET /providers` で全イベントを走査し、以後は要求のたびに増分だけ読む（SSE のループには載せない）。
  - `stats.runs` は `WorkerStarted` の数（実行中を含む）。

**SSE**
- 送る順は `hello` →（必要なら）`reset`。
- 遅れの判定は `最新 id − 要求 id > 10,000`。

**ファイル**
- `run_id` の形式検査はワークスペースの canonicalize より先に行う。run ディレクトリが無ければ `run_not_found`。
- 成果物のパスは、空・絶対パス・`..` を字句的に 403 にしてから canonicalize する（task-worker の `artifact::resolve` と同じ規則）。
- 範囲指定:
  - `Range` の開始がサイズ以上なら 416（空ファイルを含む）。
  - `offset == size` は 200 で空本体。
