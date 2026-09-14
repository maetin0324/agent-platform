# taskd HTTP API v1 仕様

- 状態: **Accepted**（人間の決定 H1 / H5〜H7。taskd 側の ADR-0013、GUI 側の ADR-GUI-0001）。改訂日 2026-09-14
- 提供者: **taskd**（crate `task-api`、axum）。taskd のデーモンプロセス内で、`taskd.toml` に `[api]` 節があるときだけ動く
- 利用者: `taskd-gui` の BFF（Remix = React Router framework mode のサーバ側 loader / action）と `curl`。**ブラウザは直接呼ばない**
- 正の型定義: Rust（`task-core` / `task-ops` / `task-api`、`serde` + `schemars`）。JSON Schema を `docs/api/v1/api-v1.schema.json` にコミットし、
  テストで生成一致を検証する（§7）。`taskd-gui` はこのファイルから TypeScript の型を生成する
- 実装の順序: taskd の Phase 9a（基盤: task-ops / WAL / events の id / スキーマ版数 / ProviderThrottled / list_page）→ 9b（本仕様）。
  GUI の G フェーズは 9b 完了後に始める（DESIGN-GUI §10）

本文書は taskd の実装者が**これだけで実装とテストを書ける**ことを目標にする。各エンドポイントの要求・応答の型、ステータスコード、境界条件を書く。
派生値（受信箱、詳細、run の要約、プロバイダの集計）の**計算規則は taskd 側（`task-ops` / `task-api`）にある**。GUI は再計算しない。

---

## 1. 全体

### 1.1 有効化と設定（`taskd.toml`）

```toml
[api]
listen = "127.0.0.1:7710"      # これを書いたときだけ API が動く（Option<SocketAddr>。無ければ無効）。推奨ポートは 7710（GUI は 7700）
# token_file = "secrets/api.token"       # 非 loopback で listen するとき必須。指定があれば loopback でも要求する。相対パスは設定ファイル基準
# allowed_hosts = ["taskd.lab.example"]  # Host 許可リストへの追加（localhost / 127.0.0.1 / [::1] とポート付きの形は常に許可）
```

- `listen` が無ければ API は動かない（既定は無効。デーモンに暗黙のネットワーク口を開けない）。Phase 9a の `taskd::config::ApiConfig{listen, token_file, allowed_hosts}` がこの形。
- `listen` が loopback（`127.0.0.0/8`、`::1`）以外で `token_file` が無い → `ConfigError::Invalid`（`[api] listen = … is not a loopback address; token_file is required`。起動時 exit 2）。
- `token_file` の内容（前後の空白を除いた 1 行）がトークン。ファイルが読めない・空 → 設定エラー。トークンはログにも API にも出さない。
- SSE の同時接続数の上限は task-api の定数 16（設定キーにしない）。
- API は**自分専用の `SqliteStore` 接続**を持ち、DB 呼び出しは `spawn_blocking` で行う（ディスパッチャの接続と Mutex を共有しない。ADR-0013 D3）。

### 1.2 プロトコルと共通規約

| 事項 | 決め |
|---|---|
| ベースパス | `/api/v1`。以下のパスは全てこれに続く |
| 転送 | HTTP/1.1。要求・応答とも `application/json; charset=utf-8`。サーバ → クライアントの通知は SSE（`text/event-stream`）。WebSocket / gRPC / HTTP/2 は使わない |
| CORS | **出さない**。`Access-Control-*` ヘッダは一切付けない。プリフライト（`OPTIONS`）は 405 |
| 共通応答ヘッダ | `Cache-Control: no-store`、`X-Content-Type-Options: nosniff`、`X-Request-Id: <ULID>`（Problem の `instance` と同じ） |
| ID | `TaskId` / `run_id` = ULID 文字列（26 文字、Crockford base32 `^[0-9A-HJKMNP-TV-Z]{26}$`）。イベントの `id` = 64 bit 整数（DB 全体で単調）。`seq` = 64 bit 整数（タスク内で 0 始まり） |
| 時刻 | RFC 3339、UTC、`Z` 終端、秒以下は任意桁（`time::serde::rfc3339` の出力そのまま）。`events.ts` / `created_at` / `updated_at` も同じ |
| 列挙 | `Status` / `TaskKind` / `Tier` は `snake_case` の文字列。`Check` / `Event` / `WorkspaceSpec` は tagged（`type` / `type` / `kind`）。**task-core の serde 表現そのまま**（§6） |
| ページング | keyset。`limit`（既定 100、最大 500。超過は 500 に丸める）と不透明な `cursor`（応答の `next_cursor`）。並び順はエンドポイントごとに固定 |
| 要求本文 | 変更系は `Content-Type: application/json` 必須（無ければ 415）。上限 1 MiB（超過は 413）。未知のフィールドは 400（`deny_unknown_fields`） |
| 楽観的検査 | 変更系は `expected_status` を受け取れる。現在の `status` と違えば 409 `conflict`（状態は変えない） |
| 冪等性 | 変更系は冪等ではない（同じ承認を 2 回送れば 2 回目は 409 `invalid_transition`）。`expected_status` を付けるのが正 |

### 1.3 認証（Bearer）

- `token_file` が設定されていれば全エンドポイント（`GET /health` を除く）で `Authorization: Bearer <token>` を要求する。無ければ 401 `unauthorized`（`WWW-Authenticate: Bearer realm="taskd"`）。比較は定数時間。
- `token_file` が無い（= loopback のみ）場合は認証しない。
- `GET /health` は常に無認証（版とスキーマ版数だけを返す。G0 の疎通確認用）。ただし Host 検査は受ける。

### 1.4 Host 検査・Origin・CSRF

- 全要求で `Host` ヘッダを許可リスト（`localhost`、`127.0.0.1`、`[::1]`、`listen` のホスト、`allowed_hosts`。ポートは無視）と照合し、外れれば 400 `host_not_allowed`。DNS rebinding 対策。
- 変更系（`POST`）に `Origin` ヘッダが付いていれば 403 `origin_forbidden`。ブラウザから直接呼ばれる設計ではないので、`Origin` の存在自体を「想定外の呼び出し」とみなす（`curl` と Node の `fetch` は `Origin` を送らない）。
- `Content-Type: application/json` の要求（1.2）と合わせて、フォーム送信型の CSRF は成立しない。

### 1.5 エラー

`application/problem+json`（RFC 9457）。本体は `Problem`:

```json
{"type":"urn:taskd:problem:invalid_transition","title":"invalid transition","status":409,
 "detail":"task 01J… (kind=execute, status=done) cannot be approved","code":"invalid_transition",
 "instance":"urn:taskd:request:01J…","task_status":"done","kind":"execute","trigger":"approve"}
```

| `code` | HTTP | 意味と付加フィールド |
|---|---|---|
| `bad_request` | 400 | JSON 構文誤り、未知フィールド、クエリの型誤り、ULID でない id、`cursor` の解読失敗 |
| `host_not_allowed` | 400 | 1.4 |
| `unauthorized` | 401 | 1.3 |
| `origin_forbidden` | 403 | 1.4 |
| `path_forbidden` | 403 | ファイル系: ワークスペース外・symlink 越え・不正な `run_id`（§3.8） |
| `task_not_found` | 404 | タスクが無い |
| `run_not_found` / `artifact_not_found` / `file_not_found` | 404 | run ディレクトリ / 成果物の添字 / ファイルが無い |
| `not_found` | 404 | 未定義のパス |
| `method_not_allowed` | 405 | |
| `conflict` | 409 | `expected_status` 不一致。`expected`, `actual` |
| `invalid_transition` | 409 | 状態機械または task-ops の写像が拒否。`task_status`, `kind`, `trigger`（`InvalidTransition{status, kind, trigger}` の写し。`trigger` は `Trigger::name()`） |
| `payload_too_large` | 413 | 本文 > 1 MiB |
| `unsupported_media_type` | 415 | 変更系で `Content-Type` が JSON でない |
| `range_not_satisfiable` | 416 | ファイル系の `Range` / `offset` がサイズを超える。`Content-Range: bytes */<size>` |
| `validation` | 422 | task-ops の検証失敗。`errors: [{field?, message}]`。`message` は `taskctl` と同じ文言（§5.6）。`field` は task-api が文言から推定できるときだけ（`acceptance` / `depends_on` / `goal`） |
| `too_many_streams` | 503 | SSE 接続数が 16 を超えた。`Retry-After: 5` |
| `db_busy` | 503 | `SQLITE_BUSY`（busy_timeout 超過）。`Retry-After: 1` |
| `internal` | 500 | その他（`detail` にエラー文。スタックやパスは出さない） |

`task_ops::OpsError` からの写像（Phase 9a の型）:

| `OpsError` | HTTP / `code` | 付加フィールド |
|---|---|---|
| `NotFound(id)` | 404 `task_not_found` | — |
| `InvalidState{id, context, action}` | 409 `invalid_transition` | `detail` = `Display`（`task <id> (<context>) cannot be <action>`）、`task_status` / `kind` は現在のタスクから、`trigger` は操作名（`approve` 等） |
| `Validation(msg)` | 422 `validation` | `errors: [{field?, message: msg}]` |
| `Conflict{expected, actual}` | 409 `conflict` | `expected`, `actual` |
| `Store(InvalidTransition{status, kind, trigger})` | 409 `invalid_transition` | `task_status`, `kind`, `trigger` |
| `Store(Sqlite(busy))` | 503 `db_busy` | — |
| `Store(その他)` | 500 `internal` | — |

`StoreError::SchemaTooNew` は起動時に起きるので API のエラーにはならない（taskd が exit 2）。

---

## 2. エンドポイント一覧（25）

| # | メソッド | パス | 目的 | 応答型 | 出所 |
|---|---|---|---|---|---|
| 1 | GET | `/health` | 版、スキーマ版数、DB の journal_mode | `Health` | task-api |
| 2 | GET | `/inbox` | 承認待ち / 質問 / draft / 注意 | `Inbox` | task-ops（+ スナップショット） |
| 3 | GET | `/tasks` | 一覧（フィルタ・keyset ページング） | `TaskList` | store `list_page` + task-ops |
| 4 | POST | `/tasks` | `taskctl add` 相当 | 201 `Task` | task-ops |
| 5 | GET | `/tasks/{id}` | 詳細（`taskctl show --json` と同一） | `TaskDetail` | task-ops |
| 6 | GET | `/tasks/{id}/events` | そのタスクのイベント（`after_seq`） | `EventsPage` | store `events_for` |
| 7 | GET | `/tasks/{id}/runs` | run の要約一覧 | `RunList` | task-ops + ファイル存在 |
| 8 | GET | `/tasks/{id}/runs/{run_id}/stdout` | `runs/<run_id>/stdout.jsonl` | バイト列 | ファイル |
| 9 | GET | `/tasks/{id}/runs/{run_id}/stderr` | `runs/<run_id>/stderr.log` | バイト列 | ファイル |
| 10 | GET | `/tasks/{id}/runs/{run_id}/result` | `runs/<run_id>/result.json` | `application/json` | ファイル |
| 11 | GET | `/tasks/{id}/artifacts` | `ArtifactProduced` の一覧 + 現在の sha256 | `ArtifactList` | events + ファイル |
| 12 | GET | `/tasks/{id}/artifacts/{idx}` | 成果物本体 | バイト列 | ファイル |
| 13 | POST | `/tasks/{id}/approve` | `taskctl approve`（draft → Accept / Approval → Approve） | `TransitionResult` | task-ops |
| 14 | POST | `/tasks/{id}/reject` | `taskctl reject` | `TransitionResult` | task-ops |
| 15 | POST | `/tasks/{id}/answer` | `taskctl answer` | `TransitionResult` | task-ops |
| 16 | POST | `/tasks/{id}/cancel` | `taskctl cancel` | `TransitionResult` | task-ops |
| 17 | POST | `/plans` | `taskctl plan` 相当 | 201 `Task` | task-ops |
| 18 | POST | `/replay` | `taskctl replay`（読み取りのみ） | `ReplayReport` | task-ops |
| 19 | GET | `/graph` | DAG（`depends_on` の辺、`parent_id` の入れ子） | `Graph` | task-ops |
| 20 | GET | `/events` | 全タスク横断のイベント（`after_id`。ポーリング / `curl` 用） | `EventsPage` | store `events_since` |
| 21 | GET | `/stream` | SSE（§4） | `text/event-stream` | store `events_since` + スナップショット |
| 22 | GET | `/providers` | 定義 + 稼働状況 + 集計 | `Providers` | 設定 + スナップショット + task-api の集計 |
| 23 | GET | `/daemon` | ディスパッチャのメモリ上のスナップショット | `DaemonView` | `tokio::sync::watch` |
| 24 | GET | `/config` | `taskd.toml` の要約（秘密は出さない） | `ConfigView` | 設定（taskd が起動時に渡す） |
| 25 | GET | `/schema` | `api-v1.schema.json` の内容 | `application/schema+json` | `include_str!` |

---

## 3. 各エンドポイント

記法: `→` は成功応答。エラーは §1.5 の共通分に加え、各項に書いたもの。

### 3.1 `GET /health` → 200 `Health`

```json
{"api_version":"1","schema_version":3,"taskd_version":"0.9.0","instance_id":"01J…",
 "started_at":"…","now":"…","db":{"journal_mode":"wal","busy_timeout_ms":5000}}
```

- `api_version` は `"1"` 固定。互換性を壊す変更は `/api/v2` で行う（ADR-0013 D8）。
- `schema_version` は `schema_migrations` の最大版数（= `task_core::SCHEMA_VERSION`。Phase 9a で 3: 0001 init / 0002 events id / 0003 tasks の title・updated_at 列）。
- `journal_mode` は `PRAGMA journal_mode` の実測値（`"wal"` でなければ設定不備。GUI は警告を出す）。
- 無認証（1.3）。DB のパスは出さない（`GET /config` に出す）。

### 3.2 `GET /inbox` → 200 `Inbox`

計算規則は §5.1。クエリ無し。`attention.unroutable` はデーモンのスナップショット（3.23）から合成する。スナップショットがまだ無ければ空。

### 3.3 `GET /tasks` → 200 `TaskList`

| クエリ | 型 | 既定 | 意味 |
|---|---|---|---|
| `status` | `Status`、複数可（`?status=ready&status=running` または `status=ready,running`） | 全て | |
| `kind` | `TaskKind`、複数可 | 全て | |
| `parent` | `TaskId` | — | 直接の子だけ（`ListFilter.parent_id`） |
| `root_only` | bool | false | `parent_id IS NULL` のものだけ。`parent` と AND で効く（同時指定は空になるだけで、エラーではない） |
| `q` | 文字列（最大 200 文字） | — | `title` の部分一致（`ListFilter.title_contains`。**大文字小文字を区別する**。`%` `_` はリテラル）。`objective` は対象外（`tasks.title` 列だけが索引付き。ADR-0013 D10） |
| `order` | `dispatch` / `updated_desc` / `created_desc` | `updated_desc` | `ListOrder` と同じ: `dispatch` = `priority DESC, created_at ASC, id ASC`（`ready_tasks` と同じ）。`updated_desc` = `updated_at DESC, id DESC`。`created_desc` = `created_at DESC, id DESC` |
| `limit` | 1..=500 | 100 | |
| `cursor` | 不透明文字列 | — | 前応答の `next_cursor`（`Page<T>.next_cursor` をそのまま）。解読できない cursor は 400 |

- `TaskStore::list_page(&ListFilter, ListOrder, cursor, limit) -> Page<Task>` をそのまま使い、`Task` を `TaskSummary` に写す（`children` / `pending_children` / `backoff_until` の付加は task-ops）。
- `total` は `Page.total`（同じフィルタでの総件数。cursor に依らない）。
- `items[].children` / `pending_children` は `parent_id` で集計（`pending` = 非終端）。`backoff_until` は §5.3。
- `counts_by_status` は**フィルタに関係なく** DB 全体の status 別件数（`count_by_status()` の `Vec<(Status, u64)>` をオブジェクトに。0 件の status は現れない）。タイトルバーの件数表示用。
- 空のときは `{"items":[],"next_cursor":null,"total":0,"counts_by_status":{…}}`。

### 3.4 `POST /tasks` → 201 `Task`（`Location: /api/v1/tasks/{id}`）

要求本文は `task_ops::add::NewTaskSpec`（`taskctl add` の引数と 1:1。Phase 9b で `Deserialize` + `JsonSchema` + `deny_unknown_fields` と `#[serde(default)]` を付ける。§6.2）:

```json
{"title":"add CLI parsing","objective":"…",
 "acceptance":[{"type":"human","text":"reviewer is happy"},
               {"type":"command","cmd":"cargo test","expect_exit":0},
               {"type":"artifact_exists","name":"bench.json"},
               {"type":"reviewer","text":"the diff is minimal"}],
 "kind":"execute","tier":"standard","adapter":null,"priority":0,
 "parent":null,"depends_on":["01J…"],
 "max_turns":10,"max_wall_secs":600,"max_retries":2,"workspace":null}
```

- `acceptance[]` は `task_ops::add::CriterionSpec`（`Human{text}` / `Command{cmd, expect_exit}` / `ArtifactExists{name}` / `Reviewer{text}`）を `#[serde(tag = "type", rename_all = "snake_case")]` で表したもの。
- 省略可能なフィールドと既定は `taskctl add` と同じ: `kind=execute`、`tier=standard`、`adapter=null`、`priority=0`、`parent=null`、`depends_on=[]`、
  `max_turns=10`、`max_wall_secs=600`、`max_retries=2`、`workspace=null`（→ `Local{path: "<task_id>"}`、相対）。`title` / `objective` / `acceptance` は必須。
- `acceptance` は**クライアントが並べた順**で保存する（CLI は accept → cmd → artifact → reviewer の固定順で渡す。並びに意味は無い）。
  `command` の `text` は `` `<cmd>` exits 0 ``（現状の `CriterionSpec::into_criterion` は `expect_exit` に関わらずこの文。CLI も常に `expect_exit = 0`）、`artifact_exists` の `text` は `artifact <name> exists`。整形は task-ops が行う。
- 初期 `status`: `kind=approval` なら `ready`、それ以外 `draft`。`Created` イベントと同一トランザクション（`task_ops::add::create_task(store, spec, now) -> Task`）。
- 422 `validation`（`OpsError::Validation` の文言そのまま。task-ops の検証はこの 2 種だけで、`title` / `objective` の空検査や `parent` の存在検査は**しない**（CLI と同じ。§9 の未決 6））:
  - `acceptance` が空 → `at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)`（`field: "acceptance"`）
  - `depends_on[i]` が存在しない → `dependency <id> does not exist`、`failed` / `cancelled` → `dependency <id> has status Failed and cannot be depended on`（`Failed` / `Cancelled` は `{:?}` 表記。`field: "depends_on"`）
- 検証に失敗したら何も挿入しない。

### 3.5 `GET /tasks/{id}` → 200 `TaskDetail`

- `taskctl show --json <id>` と**同じ型・同じ直列化**（task-ops の `TaskDetail` を compact な JSON で出す。ADR-0013 D12）。差は次の 3 点（Phase 9b で確定）:
  API は `runs[].files` を埋める（taskctl は `null`）、`timers.now` は応答時刻、taskctl は `taskd.toml` を読まないので
  `workspace_dir` / `timers.backoff_until` / `timers.max_requeues` は設定の既定値で計算する（`--workspace-root` で基準だけ上書き可）。
- `workspace_dir` は `WorkspaceSpec::Local{path}` を `workspace_root` で絶対化した文字列（`canonicalize` はしない。存在しなくてもよい）。`Remote` は `null`。
- `timers.now` は応答時刻。クライアントは `lease_expires_at - now` 等をこの `now` 基準で計算する（時計ずれ対策）。
- `runs[].files` は task-api が `<workspace_dir>/runs/<run_id>/` を `stat` して埋める（task-ops は `null`）。
- `actions` は今この状態で許される操作（§5.4）。GUI はボタンの表示にこれを使い、押した結果の 409 も正常系として扱う。
- 404 `task_not_found`。

### 3.6 `GET /tasks/{id}/events` → 200 `EventsPage`

| クエリ | 既定 | 意味 |
|---|---|---|
| `after_seq` | −1 | この `seq` より大きいものから |
| `limit` | 500（最大 5000） | |
| `types` | 全て | `Event` の `type` 名をカンマ区切り（例 `transitioned,worker_finished`）。未知の名前は 400 |

- `seq` 昇順。`has_more` が true なら最後の `seq` を `after_seq` に入れて続きを取る。
- `items[].id` はグローバル id（ADR-0013 D6）。`items[].ts` は `events.ts`。

### 3.7 `GET /tasks/{id}/runs` → 200 `RunList`

§5.2 の規則で events から組み立て、`files` を埋める。`started_at` 昇順。

### 3.8 ファイル系: `GET /tasks/{id}/runs/{run_id}/{stdout|stderr|result}`、`GET /tasks/{id}/artifacts/{idx}`

**パス解決（ユーザ入力のパスは受け取らない。ADR-0013 D11）**

1. `<ws>` = `WorkspaceSpec::Local{path}`（相対なら `workspace_root` 基準）を `canonicalize`。失敗（存在しない）→ 404 `file_not_found`。`Remote` → 404 `file_not_found`（`detail: "remote workspace"`）。
2. run: `run_id` が `^[0-9A-HJKMNP-TV-Z]{26}$` に一致しなければ 403 `path_forbidden`。対象 = `<ws>/runs/<run_id>/{stdout.jsonl|stderr.log|result.json}`。
3. 成果物: `idx` は `GET /tasks/{id}/artifacts` の `items[].idx`（`ArtifactProduced` の出現順、0 始まり）。範囲外 → 404 `artifact_not_found`。対象 = `<ws>` + 記録された `ArtifactRef.path`。
4. 対象を `canonicalize` し、`<ws>` の canonical パスで始まらなければ 403 `path_forbidden`（symlink でワークスペース外へ出るものを弾く）。ファイルでなければ（ディレクトリ等）403。存在しなければ 404 `file_not_found`。
5. ワークスペースの**外は絶対に出さない**が、中は信頼境界の内側とする。

**応答**

- `Content-Type` は拡張子から次の**閉じた表**で決める。それ以外は `application/octet-stream`。`text/html`、`image/svg+xml`、`application/javascript` 等の能動的な型は**決して返さない**。
  - `text/plain; charset=utf-8`: `.txt .log .jsonl .diff .patch .csv .tsv .toml .yaml .yml .rs .py .sh .ts .js .c .h .cpp .go .java .sql`（ソースは全て text/plain）
  - `application/json`: `.json`（`result` は常にこれ）
  - `text/markdown; charset=utf-8`: `.md`
  - `image/png` / `image/jpeg` / `image/gif` / `image/webp`: 対応する拡張子
- `Content-Disposition: inline; filename="<basename>"`（`?download=1` で `attachment`。`filename` は RFC 8187 でエスケープ）。
- 成果物には `X-Taskd-Sha256: <記録値>` と `X-Taskd-Sha256-Current: <現在の値>`（計算は 64 MiB までで、超えるファイルは省略）。
- `X-Taskd-Size: <現在のバイト数>` を常に付ける（追尾用）。
- 範囲: `Range: bytes=a-b` に 206 + `Content-Range` で応える（単一範囲のみ。複数範囲は 416）。または `?offset=N&length=M`（`Range` と併用不可、併用は 400）。`offset == size` は **200 で空本体**（追尾で「新着なし」を表すため）。`offset > size` / `Range` の開始がサイズ超 → 416。
- 本体はストリーミング。サイズ上限は設けない（追尾は `offset` で行う）。

### 3.9 `GET /tasks/{id}/artifacts` → 200 `ArtifactList`

`ArtifactProduced` を出現順に並べ、`idx`、`run_id`、`ts`、`artifact`（`ArtifactRef`）、`exists`、`size`、`sha256_current`、`sha256_matches`（記録値との一致。`exists=false` なら `null`）。パス検査（3.8）に落ちるものは `exists=false, forbidden=true` として一覧には残す（GUI は警告表示、本体は 403）。

### 3.10 `POST /tasks/{id}/approve` → 200 `TransitionResult`

本文 `DecisionBody{note?: string, expected_status?: Status}`（空本体は `{}` と同じ。ただし `Content-Type` は必要）。呼ぶのは `task_ops::gate::approve(store, id, note, expected_status)`。

- 写像は `taskctl approve` と同じ: `status == draft`（kind 不問）→ `Trigger::Accept`（イベント追加無し）。`kind == approval && status == ready` → `Trigger::Approve` + `Event::ApprovalDecided{by:"human", approved:true, note}` を同一トランザクション。
- それ以外 → 409 `invalid_transition`（`OpsError::InvalidState`: `detail: "task <id> (kind=<kind>, status=<status>) cannot be approved"`、`trigger: "approve"`）。
- `expected_status` があり現在と違う → 409 `conflict`（`OpsError::Conflict`。写像より先に検査される）。
- `by` は `"human"` 固定（H9）。

### 3.11 `POST /tasks/{id}/reject` → 200 `TransitionResult`

本文 `DecisionBody`。`task_ops::gate::reject(store, id, note, expected_status)`: `kind == approval && status == ready` のみ `Trigger::Reject` + `ApprovalDecided{approved:false, note}`。それ以外は 409 `invalid_transition`（`cannot be rejected`）。`draft` の取り消しは `cancel`（ADR-0004 D2）。

### 3.12 `POST /tasks/{id}/answer` → 200 `TransitionResult`

本文 `AnswerBody{answer: string, expected_status?}`。`answer` が空白のみ → 422 `validation`（`answer must not be blank`。task-api が task-ops を呼ぶ前に検査する。CLI は clap が空文字を通すので挙動は CLI と同じにしない）。
`task_ops::gate::answer(store, id, answer, expected_status)`: `status != blocked` → 409 `invalid_transition`（`cannot be answered; only blocked tasks accept an answer`）。
`Trigger::Answer` + `Event::Answered{question, answer}`（`question` は §5.5 の `latest_question`。無ければ空文字列）を同一トランザクション。

### 3.13 `POST /tasks/{id}/cancel` → 200 `TransitionResult`

本文 `CancelBody{expected_status?}`。`task_ops::gate::cancel(store, id, expected_status)`: 終端（`done|failed|cancelled`）→ 409 `invalid_transition`（`cannot be cancelled`）。伝播（Approval の子、`depends_on` の後続）はストアが同一トランザクションで行い、`cascaded` にその id を列挙する（§5.7）。

### 3.14 `POST /plans` → 201 `Task`（`Location`）

本文 `task_ops::plan::NewPlanSpec`（`taskctl plan` と 1:1。9b で serde / JsonSchema を付ける）: `goal`（必須）、`workspace?`、`tier=frontier`、`priority=0`、`max_turns=30`、`max_wall_secs=900`、`max_retries=1`。
`task_ops::plan::create_plan(store, spec, now) -> Task`。`title` = `goal` の 1 行目の先頭 80 文字（char 境界）。`goal` が空白のみ → 422（`goal must not be blank`、`field: "goal"`）。`kind=plan`、`acceptance=[]`、`status=draft`、`parent_id=null`。

### 3.15 `POST /replay` → 200 `ReplayReport`

本文は空（`{}`）。`task_ops::replay::replay(store) -> ReplayReport{tasks, mismatches}`。`taskctl replay` と同じ規則（`Created` で初期化、`Transitioned` で上書き、`worker_error|lease_expired|review_fail` で attempts+1）で全タスクを再構築し、`tasks` との差分を返す。**DB は変更しない。** 数万イベントで数秒かかりうるので、`spawn_blocking` で行い、同時実行は 1 つ（2 つ目は 503 `replay_in_progress`、`Retry-After: 5`）。

### 3.16 `GET /graph` → 200 `Graph`

| クエリ | 既定 | 意味 |
|---|---|---|
| `root` | — | このタスクの祖先・子孫（`depends_on` と `parent_id` を両方向にたどる）だけ |
| `depth` | 無制限 | `root` からの最大ホップ数 |
| `include_terminal` | true | false で `done|failed|cancelled` を除く（辺も除く） |

`nodes[] = {id, title, status, kind, parent_id}`、`edges[] = {from, to, kind: "depends_on"}`（`from` = 先行、`to` = 後続）。親子は `parent_id` で表し、辺にしない。レイアウトはクライアント。上限 5,000 ノード（超えたら 422 `validation`、`detail` で `root` の指定を促す）。

### 3.17 `GET /events` → 200 `EventsPage`

| クエリ | 既定 | 意味 |
|---|---|---|
| `after_id` | 0 | このグローバル id より大きいものから |
| `limit` | 500（最大 5000） | |
| `task_id` | — | 1 タスクに絞る |
| `types` | 全て | 3.6 と同じ |

`id` 昇順。`events_since(after_id, limit)` そのもの。SSE を使えないクライアント（`curl`、テスト）用。

### 3.18 `GET /stream`

§4。

### 3.19 `GET /providers` → 200 `Providers`

`items[]` は `[[providers]]` の順。定義（`id` / `adapter` / `tiers` / `concurrency` / `model` = 実効モデル / `env_keys` = **キー名だけ**）は設定から、`in_use` と `cooldown` はスナップショット（無ければ `null`）、`stats` は §5.8 の集計。

### 3.20 `GET /daemon` → 200 `DaemonView`

```json
{"now":"…","snapshot":{"instance_id":"01J…","pid":1234,"hostname":"lab-01","started_at":"…","last_tick_at":"…","ticks":8812,"tick_ms":2000,
  "in_flight":[{"task_id":"01J…","run_id":"01J…","provider":"claude-a","kind":"worker","since":"…"}],
  "cooldowns":[{"provider":"claude-b","until":"…","reason":"throttled"}],
  "awaiting_human":["01J…"],"unroutable":[],
  "providers":[{"id":"claude-a","adapter":"claude-code","tiers":["frontier","standard","cheap"],"concurrency":2,"model":"claude-sonnet-5","in_use":1}]}}
```

- ディスパッチャが tick の最後に `DaemonSnapshot` を `tokio::sync::watch` に送り、API は最新値を読む（I/O 無し。ADR-0013 D4）。DB には書かない。
- 最初の tick より前は `snapshot: null`。
- cooldown は `ProviderPolicy::cooldowns(now) -> Vec<Cooldown{provider, until: Instant, reason: CooldownReason}>`（既定実装は空。Phase 9a で `StaticPolicy` が実装済み）で取り、`Instant` を壁時計に直す。`reason` の語彙は `throttled | auth_failed | exhausted`（`Spawn` は `provider_failure_outcome` が `Exhausted` に写すので cooldown の理由としては現れない。`ProviderThrottled.reason` には `spawn` も入りうる）。
- `last_tick_at` が `now` から `3 × tick_ms` 以上古ければ GUI は「ディスパッチャが遅延」と表示する（API は判定しない）。
- API に繋がらないこと自体が「taskd 停止」を意味する（GUI 側で表示）。

### 3.21 `GET /config` → 200 `ConfigView`

`taskd.toml` の要約。`db`（絶対パス）、`workspace_root`、`tick_ms`、`max_concurrency`、`lease_grace_secs`、`idle_timeout_secs`、`kill_grace_secs`、`review_timeout_secs`、`error_cooldown_secs`、`retry_backoff_base_secs`、`retry_backoff_max_secs`、`max_requeues`、`plan.auto_accept`、`reviewer{adapter, tier}`、`providers[]{id, adapter, tiers, concurrency, model, env_keys}`、`api{bind, auth_required, allowed_hosts}`、`config_path`。
`[[providers]].env` の**値**、`[adapters.*].env` の値、`token_file` のパスと内容は出さない。`task-api` は `taskd` crate に依存しないので、この型は task-api に置き、taskd が起動時に値を作って `ApiState` に渡す。

### 3.22 `GET /schema` → 200 `application/schema+json`

コミット済み `docs/api/v1/api-v1.schema.json` を `include_str!` で返す（開発・型生成の確認用。GUI の型生成はリポジトリのファイルから行い、この応答には依存しない）。

---

## 4. SSE `GET /stream`

```
event: hello
data: {"cursor":12345,"now":"…","daemon":{…DaemonSnapshot…}}

event: task.event
id: 12346
data: {"id":12346,"task_id":"01J…","seq":7,"ts":"…","event":{"type":"transitioned","from":"ready","to":"running","reason":"dispatch"}}

event: daemon
data: {…DaemonSnapshot…}

event: heartbeat
data: {"now":"…"}

event: reset
data: {"reason":"cursor_too_old","cursor":20000}
```

| 事項 | 決め |
|---|---|
| 応答ヘッダ | `Content-Type: text/event-stream; charset=utf-8`、`Cache-Control: no-store`、`X-Accel-Buffering: no`。本体は最初に `hello` を送るまで待たせない（接続直後に flush） |
| 再開 | `Last-Event-ID` ヘッダ（`EventRow.id`）または `?after_id=`（ヘッダが優先）。省略時は「今」（`TaskStore::latest_event_id()`）から（過去は送らない）。`hello.cursor` が送信開始位置 |
| 取りこぼし | 要求された id から最新までが **10,000 件を超える**、または要求 id が最新より大きい（DB が入れ替わった）→ 最初に `reset` を送り、`cursor` = 最新 id から続ける。クライアントは全体を再取得する |
| `task.event` | `events_since(cursor, 1000)` を **250 ms 間隔**でポーリング（購読者が 0 なら止める）。`id:` 行に `EventRow.id`。`?task_id=` で 1 タスクに絞る（`hello.cursor` は絞らない） |
| `daemon` | `watch` の値が変わるたび（= 毎 tick）。`?task_id=` があっても送る |
| `heartbeat` | 15 秒ごと（プロキシのタイムアウト対策） |
| 接続数 | 定数 16。超過は 503 `too_many_streams` |
| 認証 | 他と同じ（Bearer / Host） |
| 終了 | クライアントが切ればサーバは即座に購読を解除する。taskd の停止時は接続を閉じる |

クライアント（BFF）の規約: `task.event` を受けたら該当画面のデータを**再取得**する（イベント本体から状態を組み立てない。真実は DB）。`taskctl` による書き込みも同じ経路で流れる（in-process 通知は使わない。ADR-0013 D6）。

---

## 5. 派生値の計算規則（task-ops / task-api）

全て **`crates/task-ops`** の関数として実装し、`taskctl show --json` / `taskctl` の各コマンド / `task-api` / ディスパッチャが同じ関数を使う。GUI は結果を表示するだけ。

### 5.1 受信箱（`task_ops::inbox(store, snapshot: Option<&DaemonSnapshot>, now)`）

| 区画 | 抽出 | 各項目の埋め方 | 並び |
|---|---|---|---|
| `approvals[]` | `kind == approval && status == ready` | `parent` = `parent_id` のタスク（無ければ `null`）。`criterion_idx` / `attempt` は title を `Approval needed: <title> — criterion <idx> (attempt <n>)` として解析（`task_ops::parse_human_approval_title`。ディスパッチャの `human_approval_title` と対）。解析できなければ `null`。`criterion_text` = 親の `acceptance[idx].text`（無ければ approval の `objective`）。`requested_at` = `ApprovalRequested` の `ts`（無ければ `created_at`）。`last_run` = 親の `last_run_id` の `RunSummary`。`evidence` = `<ws>/runs/<run_id>/result.json` が `done` なら `evidence[]`（task-api が読む。読めなければ `[]`）。`other_verdicts` = 親の同 run の `ReviewVerdict`。`artifacts` = `artifacts_for_run`。`previous_decisions` = 親の他の Approval 子（同じ `criterion_idx`）の `ApprovalDecided` | `requested_at` 昇順 |
| `questions[]` | `status == blocked` | `question` = §5.5。`asked_at` = その `WorkerFinished` の `ts`。`run_id` = 同。`previous` = `answers_from_events`（`AnswerNote` の履歴） | `asked_at` 昇順 |
| `drafts[]` | `status == draft` を `parent_id` でまとめる | `parent` = Plan 等（`null` = 根）。`plan_summary` = 親の直近 `WorkerFinished.outcome` が `done: ` 始まりならその後ろ。`drafts` = `TaskSummary` | 親の `created_at` 昇順、根は最後 |
| `attention[]` | (a) `failed` かつ `updated_at >= now − 24h`、(b) `ready && max_requeues > 0 && consecutive_requeues > 0 && consecutive_requeues >= max_requeues − 1`（一度も requeue していないものは含めない）、(c) スナップショットの `unroutable` | (a) `reason` = 直近 `WorkerFinished.outcome` と、直近 run の fail の `ReviewVerdict.reason` を（あるものだけ）`; ` で結合。(b) `count` / `max`。(c) `hint` = `worker_hint`、`at` = スナップショットの `last_tick_at` | `at` 降順 |
| `counts` | 上の件数 + `count_by_status()` | `drafts` は **draft タスクの件数**（グループ数ではない）。他は各区画の要素数 | |

### 5.2 run の要約（`task_ops::runs(events) -> Vec<RunSummary>`）

- `WorkerStarted{run_id, adapter, model, provider}` で開始（`started_at` = `ts`）。同じ `run_id` の `WorkerProgress` を `progress` に数え、`ArtifactProduced` を `artifacts` に数え、`ReviewVerdict` を `verdicts` に数える。
- `WorkerFinished{run_id, outcome, usage}` で終了（`finished_at` = `ts`）。`outcome` の分類（`RunOutcomeKind`）は**接頭辞**で決める（ディスパッチャの文字列と対）:
  - `done: ` → `done`（`outcome_text` = 後ろの summary）
  - `question: ` → `question`
  - `requeue: ` → `requeue`
  - `lease_expired`（完全一致）→ `lease_expired`
  - それ以外（`error(retryable=…): …`）→ `error`
- `WorkerFinished` が無い run は `finished_at = null, outcome = null`（実行中、または回収前）。
- Reviewer run は `WorkerStarted` / `WorkerFinished` を持たない（対象 run の `WorkerProgress` に `reviewer run <id>: ` 接頭辞で残るだけ）ので一覧には現れない。`reviewer run requeued: ` で始まる `WorkerProgress` は `RunSummary.reviewer_deferrals` に数える。

### 5.3 タイマー（`task_ops::timers(task, events, config, now)`）

- `lease_expires_at` = `task.lease.expires_at`（`running` のとき。`renew_lease` はイベントを出さないので、GUI は `running` の詳細を 5 秒ごとに再取得する）。
- `backoff_until` = `status == ready && attempts > 0` のとき `updated_at + retry_backoff(base, max, attempts)`（`min(base·2^(attempts−1), max)`。`base = 0` なら `null`）。過去なら `null`。
- `consecutive_requeues` / `consecutive_reviewer_requeues` は `task_ops::derive` に移動済み（Phase 9a。規則は不変: `Transitioned` を新しい順に見て `requeue` を数え `dispatch` は読み飛ばす / 最後の `Transitioned` 以降の `REVIEWER_REQUEUED_PREFIX` を数える）。`retry_backoff` / `artifacts_for_run` / `last_run_id` / `human_approval_title` / `approval_decision_note` / `latest_question` / `prior_review_from_events` / `answers_from_events` も同じモジュールにある。
- `max_requeues` は設定値。

### 5.4 可能な操作（`task_ops::actions(task) -> Vec<Action>`）

`approve`: `status == draft` または `kind == approval && status == ready`。`reject`: `kind == approval && status == ready`。`answer`: `status == blocked`。`cancel`: 非終端。

### 5.5 質問文（`task_ops::latest_question(events)`）

`events_for` を後ろから見て最初の `WorkerFinished{outcome}` のうち `"question: "` で始まるものの接頭辞を除いた文字列。無ければ空文字列（現在の `gate.rs::latest_question` をそのまま移す）。

### 5.6 検証（`task_ops::add::create_task` / `task_ops::plan::create_plan` の中）

文言は 3.4 / 3.14 のとおり（`OpsError::Validation`）。`taskctl add` / `plan` も同じ関数を通す（Phase 9a で移行済み）。9b では**テーブル駆動テストを task-ops に置く**（GUI の G2 はこのテーブルを HTTP 越しに再確認するだけ）。

### 5.7 `TransitionResult.cascaded`（9b で task-ops の `TransitionResult{id, from, to, reason}` に追加。`#[serde(default)]`）

`apply_transition` の前後で `events_since` の増分を読み、遷移対象以外の `task_id` に付いた `Transitioned{to: cancelled}` を `TaskRef` にして返す（トランザクション前の最新 id を控え、後で `events_since(id)` を読む。同時に他の書き込みが挟まっても `reason` が `dependency_failed` / `cancel` のものだけを拾うので過剰には含まれない）。

### 5.8 プロバイダの集計（`task_api::stats`。task-ops ではなく task-api のメモリ）

- 起動時に `events_since(0, 5000)` を繰り返して全イベントを 1 回走査し、以後は SSE と同じポーリングループの増分で更新する。**メモリ内の観測値**で、真実ではない（再起動で再計算）。
- `WorkerStarted{run_id, provider}` で run 表に `provider`（`null` なら `"unknown"`）を登録し、`WorkerFinished{run_id, outcome, usage}` で閉じる。分類は §5.2。`input_tokens` / `output_tokens` は `usage` の和（`null` は 0）。
- `by_day` は `WorkerFinished.ts` の UTC 日付で直近 30 日。
- Reviewer run の使用量は events に残らないため**集計外**（§8 の未決 1）。

---

## 6. 型

### 6.1 型の出所

| 出所 | 型 | 備考 |
|---|---|---|
| `task-core`（既存） | `Task`, `TaskId`, `TaskKind`, `Status`, `Tier`, `WorkerHint`, `WorkspaceSpec`, `Budget`, `Lease`, `Check`, `Criterion`, `ArtifactRef`, `Usage`, `Event` | serde 表現そのまま。`Event` は Phase 9a で `JsonSchema` を derive 済み（`until` は `#[schemars(with = "String")]`）。`ProviderThrottled.reason: Option<String>`（任意フィールド、語彙 `throttled \| auth_failed \| exhausted \| spawn`。ADR-0013 D9） |
| `task-core`（Phase 9a、実装済み） | `EventRow { id: u64, task_id: TaskId, seq: u64, ts: String, event: Event }`、`ListFilter { statuses, kinds, parent_id, root_only, title_contains }`、`ListOrder { Dispatch, UpdatedDesc, CreatedDesc }`、`Page<T> { items, next_cursor, total }`、`SCHEMA_VERSION` | `events_since` / `list_page` / `count_by_status` の型。`EventRow` は `docs/api/v1/event.schema.json` のルート |
| `task-ops`（Phase 9a、実装済み） | `add::{NewTaskSpec, CriterionSpec, create_task}`、`plan::{NewPlanSpec, create_plan}`、`gate::{TransitionResult, approve, reject, answer, cancel}`、`replay::{ReplayReport, ReplayMismatch, replay}`、`derive::{ReviewNote, AnswerNote, …}`、`OpsError` | 9b で `Deserialize` / `Serialize` / `JsonSchema` を付ける（`NewTaskSpec` / `NewPlanSpec` は `deny_unknown_fields` + `#[serde(default)]`、`CriterionSpec` は `tag = "type"`、`ReplayMismatch.field` は `&'static str` のまま文字列に出る） |
| `task-ops`（Phase 9b で追加） | `TaskRef`, `TaskSummary`, `TaskList`, `TaskDetail`, `Timers`, `CriterionView`, `VerdictView`, `RunSummary`, `RunFiles`, `RunOutcomeKind`, `ApprovalLink`, `ApprovalDecisionView`, `Action`, `Inbox`, `InboxCounts`, `ApprovalItem`, `EvidenceView`, `QuestionItem`, `DraftGroup`, `AttentionItem`, `Graph`, `GraphNode`, `GraphEdge`, `TransitionResult.cascaded`, `DaemonSnapshot`, `InFlight`, `InFlightKind`, `CooldownView`, `ProviderLive` | ビュー型。全て `JsonSchema`。`DaemonSnapshot` は task-dispatch が作り task-api が読むので、両者が依存する task-ops に置く（ADR-0013 D3/D4 の依存方向を満たす）。`CooldownView` は `task_dispatch::policy::Cooldown`（`Instant`）を壁時計に直した写し |
| `task-api`（Phase 9b） | `Health`, `DbInfo`, `Problem`, `ValidationError`, `DecisionBody`, `AnswerBody`, `CancelBody`, `EventsPage`, `RunList`, `ArtifactList`, `ArtifactView`, `Providers`, `ProviderView`, `ProviderStats`, `DailyUsage`, `DaemonView`, `ConfigView`, `ReviewerConfigView`, `ProviderConfigView`, `ApiConfigView`, `StreamHello`, `StreamHeartbeat`, `StreamReset`, `ApiV1Schema` | HTTP の要求・応答の包み。`POST /tasks` / `POST /plans` の本文は task-ops の `NewTaskSpec` / `NewPlanSpec` そのもの |

### 6.2 Rust 表記（serde の属性はコメントで示す。`JsonSchema` は全て derive）

```rust
// ---- task-core（実装済み）----
pub struct EventRow { pub id: u64, pub task_id: TaskId, pub seq: u64, pub ts: String, pub event: Event }
pub struct ListFilter { pub statuses: Vec<Status>, pub kinds: Vec<TaskKind>, pub parent_id: Option<TaskId>, pub root_only: bool, pub title_contains: Option<String> }
pub enum ListOrder { Dispatch, UpdatedDesc, CreatedDesc }
pub struct Page<T> { pub items: Vec<T>, pub next_cursor: Option<String>, pub total: u64 }
// Event::ProviderThrottled { provider: String, until: OffsetDateTime, #[serde(default, skip_serializing_if = "Option::is_none")] reason: Option<String> }

// ---- task-ops: 参照・一覧 ----
pub struct TaskRef { pub id: TaskId, pub title: String, pub kind: TaskKind, pub status: Status }
pub struct TaskSummary {
    pub id: TaskId, pub parent_id: Option<TaskId>, pub kind: TaskKind, pub status: Status, pub title: String,
    pub priority: i32, pub tier: Tier, pub adapter: Option<String>, pub attempts: u32, pub max_retries: u32,
    pub depends_on: Vec<TaskId>, pub created_at: String, pub updated_at: String,
    pub lease_expires_at: Option<String>, pub backoff_until: Option<String>,
    pub children: u32, pub pending_children: u32,
}
pub struct TaskList { pub items: Vec<TaskSummary>, pub next_cursor: Option<String>, pub total: u64, pub counts_by_status: BTreeMap<Status, u64> }

// ---- task-ops: 詳細（taskctl show --json と同一）----
pub struct TaskDetail {
    pub task: Task, pub workspace_dir: Option<String>, pub timers: Timers, pub criteria: Vec<CriterionView>,
    pub runs: Vec<RunSummary>, pub prior_review: Vec<ReviewNote>, pub answers: Vec<AnswerNote>,
    pub latest_question: Option<String>, pub approvals: Vec<ApprovalLink>,
    pub dependencies: Vec<TaskRef>, pub dependents: Vec<TaskRef>, pub children: Vec<TaskRef>,
    pub actions: Vec<Action>, pub worker_run_hint: Option<String>,
}
pub struct Timers { pub now: String, pub lease_expires_at: Option<String>, pub backoff_until: Option<String>,
    pub consecutive_requeues: u32, pub max_requeues: u32, pub consecutive_reviewer_requeues: u32 }
pub struct CriterionView { pub idx: usize, pub text: String, pub check: Check, pub latest_verdict: Option<VerdictView>, pub approval: Option<ApprovalLink> }
pub struct VerdictView { pub run_id: String, pub criterion_idx: usize, pub pass: bool, pub reason: String, pub ts: String }
pub struct RunSummary { pub run_id: String, pub adapter: String, pub model: String, pub provider: Option<String>,
    pub started_at: String, pub finished_at: Option<String>, pub outcome: Option<RunOutcomeKind>, pub outcome_text: Option<String>,
    pub usage: Option<Usage>, pub progress: u32, pub artifacts: u32, pub verdicts: u32, pub reviewer_deferrals: u32,
    pub files: Option<RunFiles> }
pub struct RunFiles { pub stdout: bool, pub stderr: bool, pub result: bool }
// #[serde(rename_all = "snake_case")]
pub enum RunOutcomeKind { Done, Question, Error, Requeue, LeaseExpired }
pub struct ReviewNote { pub criterion: usize, pub pass: bool, pub reason: String }   // task_ops::derive（実装済み）。task_worker::PriorReview への写像はディスパッチャ側
pub struct AnswerNote { pub question: String, pub answer: String }                    // task_ops::derive（実装済み）
pub struct ApprovalLink { pub approval: TaskRef, pub criterion_idx: Option<usize>, pub attempt: Option<u32>, pub decided: Option<ApprovalDecisionView> }
pub struct ApprovalDecisionView { pub by: String, pub approved: bool, pub note: Option<String>, pub ts: String }
// #[serde(rename_all = "snake_case")]
pub enum Action { Approve, Reject, Answer, Cancel }

// ---- task-ops: 受信箱 ----
pub struct Inbox { pub approvals: Vec<ApprovalItem>, pub questions: Vec<QuestionItem>, pub drafts: Vec<DraftGroup>,
    pub attention: Vec<AttentionItem>, pub counts: InboxCounts }
pub struct InboxCounts { pub approvals: u32, pub questions: u32, pub drafts: u32, pub attention: u32, pub by_status: BTreeMap<Status, u64> }
pub struct ApprovalItem { pub approval: TaskRef, pub parent: Option<TaskRef>, pub criterion_text: String,
    pub criterion_idx: Option<usize>, pub attempt: Option<u32>, pub requested_at: String, pub last_run: Option<RunSummary>,
    pub evidence: Vec<EvidenceView>, pub other_verdicts: Vec<VerdictView>, pub artifacts: Vec<ArtifactRef>,
    pub previous_decisions: Vec<ApprovalDecisionView> }
pub struct EvidenceView { pub criterion: usize, pub command: Option<String>, pub exit: Option<i32>, pub stdout_tail: Option<String> }  // task_worker::Evidence と同形
pub struct QuestionItem { pub task: TaskRef, pub question: String, pub asked_at: Option<String>, pub run_id: Option<String>, pub previous: Vec<AnswerNote> }
pub struct DraftGroup { pub parent: Option<TaskRef>, pub plan_summary: Option<String>, pub drafts: Vec<TaskSummary> }
// #[serde(tag = "type", rename_all = "snake_case")]
pub enum AttentionItem {
    Failed { task: TaskRef, reason: String, at: String },
    RequeueLimitNear { task: TaskRef, count: u32, max: u32, at: String },
    Unroutable { task: TaskRef, hint: WorkerHint, at: String },
}

// ---- task-ops: 操作の入力（実装済みの型に 9b で serde / JsonSchema を付ける。POST /tasks, POST /plans の本文そのもの）----
// #[serde(deny_unknown_fields)]
pub struct NewTaskSpec {
    pub title: String, pub objective: String, pub acceptance: Vec<CriterionSpec>,
    #[serde(default)] pub kind: TaskKind /* execute */, #[serde(default)] pub tier: Tier /* standard */,
    #[serde(default)] pub priority: i32, #[serde(default)] pub parent: Option<TaskId>, #[serde(default)] pub depends_on: Vec<TaskId>,
    #[serde(default = "10")] pub max_turns: u32, #[serde(default = "600")] pub max_wall_secs: u64, #[serde(default = "2")] pub max_retries: u32,
    #[serde(default)] pub workspace: Option<PathBuf> /* JSON では文字列 */, #[serde(default)] pub adapter: Option<String>,
}
// #[serde(tag = "type", rename_all = "snake_case")]
pub enum CriterionSpec { Human { text: String }, Command { cmd: String, #[serde(default)] expect_exit: i32 }, ArtifactExists { name: String }, Reviewer { text: String } }
// #[serde(deny_unknown_fields)]
pub struct NewPlanSpec { pub goal: String, #[serde(default)] pub workspace: Option<PathBuf>, #[serde(default = "frontier")] pub tier: Tier,
    #[serde(default)] pub priority: i32, #[serde(default = "30")] pub max_turns: u32, #[serde(default = "900")] pub max_wall_secs: u64, #[serde(default = "1")] pub max_retries: u32 }

// ---- task-ops: 結果（実装済み。`cascaded` だけ 9b で追加）----
pub struct TransitionResult { pub id: TaskId, pub from: Status, pub to: Status, pub reason: String, #[serde(default)] pub cascaded: Vec<TaskRef> }
pub struct ReplayReport { pub tasks: usize, pub mismatches: Vec<ReplayMismatch> }
pub struct ReplayMismatch { pub task_id: TaskId, pub field: String /* "status" | "attempts" */, pub replayed: String, pub stored: String }
pub struct Graph { pub nodes: Vec<GraphNode>, pub edges: Vec<GraphEdge> }
pub struct GraphNode { pub id: TaskId, pub title: String, pub status: Status, pub kind: TaskKind, pub parent_id: Option<TaskId> }
pub struct GraphEdge { pub from: TaskId, pub to: TaskId, pub kind: String /* "depends_on" */ }

// ---- task-ops: デーモンのスナップショット（task-dispatch が作り、task-api が読む）----
pub struct DaemonSnapshot { pub instance_id: String, pub pid: u32, pub hostname: String, pub started_at: String, pub last_tick_at: String,
    pub ticks: u64, pub tick_ms: u64, pub in_flight: Vec<InFlight>, pub cooldowns: Vec<CooldownView>,
    pub awaiting_human: Vec<TaskId>, pub unroutable: Vec<TaskId>, pub providers: Vec<ProviderLive> }
pub struct InFlight { pub task_id: TaskId, pub run_id: String, pub provider: String, pub kind: InFlightKind, pub since: String }
// #[serde(rename_all = "snake_case")]
pub enum InFlightKind { Worker, Reviewer }
/// `task_dispatch::policy::Cooldown{provider, until: Instant, reason: CooldownReason}`（実装済み）を壁時計に直したもの。
pub struct CooldownView { pub provider: String, pub until: String, pub reason: String /* "throttled" | "auth_failed" | "exhausted" */ }
pub struct ProviderLive { pub id: String, pub adapter: String, pub tiers: Vec<Tier>, pub concurrency: usize, pub model: Option<String>, pub in_use: u32 }

// ---- task-api ----
pub struct Health { pub api_version: String, pub schema_version: u32, pub taskd_version: String, pub instance_id: String,
    pub started_at: String, pub now: String, pub db: DbInfo }
pub struct DbInfo { pub journal_mode: String, pub busy_timeout_ms: u64 }
pub struct Problem { pub r#type: String, pub title: String, pub status: u16, pub detail: String, pub code: String, pub instance: String,
    #[serde(flatten)] pub extra: serde_json::Map<String, serde_json::Value> }
pub struct ValidationError { pub field: Option<String>, pub message: String }   // field は推定できるときだけ（§1.5）
// #[serde(deny_unknown_fields)] の 3 つ
pub struct DecisionBody { #[serde(default)] pub note: Option<String>, #[serde(default)] pub expected_status: Option<Status> }
pub struct AnswerBody { pub answer: String, #[serde(default)] pub expected_status: Option<Status> }
pub struct CancelBody { #[serde(default)] pub expected_status: Option<Status> }
pub struct EventsPage { pub items: Vec<EventRow>, pub has_more: bool }
pub struct RunList { pub runs: Vec<RunSummary> }
pub struct ArtifactList { pub items: Vec<ArtifactView> }
pub struct ArtifactView { pub idx: usize, pub run_id: String, pub ts: String, pub artifact: ArtifactRef, pub exists: bool, pub forbidden: bool,
    pub size: Option<u64>, pub sha256_current: Option<String>, pub sha256_matches: Option<bool> }
pub struct Providers { pub items: Vec<ProviderView> }
pub struct ProviderView { pub id: String, pub adapter: String, pub tiers: Vec<Tier>, pub concurrency: usize, pub model: Option<String>,
    pub env_keys: Vec<String>, pub in_use: Option<u32>, pub cooldown: Option<CooldownView>, pub stats: ProviderStats }
pub struct ProviderStats { pub runs: u64, pub done: u64, pub question: u64, pub error: u64, pub requeue: u64, pub lease_expired: u64,
    pub input_tokens: u64, pub output_tokens: u64, pub by_day: Vec<DailyUsage> }
pub struct DailyUsage { pub day: String /* YYYY-MM-DD */, pub runs: u64, pub input_tokens: u64, pub output_tokens: u64 }
pub struct DaemonView { pub now: String, pub snapshot: Option<DaemonSnapshot> }
pub struct ConfigView { pub config_path: String, pub db: String, pub workspace_root: String, pub tick_ms: u64, pub max_concurrency: usize,
    pub lease_grace_secs: u64, pub idle_timeout_secs: u64, pub kill_grace_secs: u64, pub review_timeout_secs: u64, pub error_cooldown_secs: u64,
    pub retry_backoff_base_secs: u64, pub retry_backoff_max_secs: u64, pub max_requeues: u32, pub plan_auto_accept: bool,
    pub reviewer: ReviewerConfigView, pub providers: Vec<ProviderConfigView>, pub api: ApiConfigView }
pub struct ReviewerConfigView { pub adapter: Option<String>, pub tier: Tier }
pub struct ProviderConfigView { pub id: String, pub adapter: String, pub tiers: Vec<Tier>, pub concurrency: usize, pub model: Option<String>, pub env_keys: Vec<String> }
pub struct ApiConfigView { pub bind: String, pub auth_required: bool, pub allowed_hosts: Vec<String> }
pub struct StreamHello { pub cursor: u64, pub now: String, pub daemon: Option<DaemonSnapshot> }
pub struct StreamHeartbeat { pub now: String }
pub struct StreamReset { pub reason: String /* "cursor_too_old" | "cursor_ahead" */, pub cursor: u64 }

/// スキーマ生成のルート（`task_worker::ProtocolSchema` と同じ流儀。1 フィールド = 1 公開型）。
pub struct ApiV1Schema {
    pub health: Health, pub problem: Problem, pub inbox: Inbox, pub task_list: TaskList, pub task: Task, pub task_detail: TaskDetail,
    pub events_page: EventsPage, pub run_list: RunList, pub artifact_list: ArtifactList, pub graph: Graph,
    pub new_task: NewTaskSpec, pub new_plan: NewPlanSpec, pub decision: DecisionBody, pub answer: AnswerBody, pub cancel: CancelBody,
    pub transition_result: TransitionResult, pub replay_report: ReplayReport, pub providers: Providers, pub daemon: DaemonView,
    pub config: ConfigView, pub stream_hello: StreamHello, pub stream_event: EventRow, pub stream_daemon: DaemonSnapshot,
    pub stream_heartbeat: StreamHeartbeat, pub stream_reset: StreamReset,
}
```

`Option<String>` の時刻フィールドは RFC 3339 文字列（`OffsetDateTime` を持つ型は `#[serde(with = "time::serde::rfc3339")] #[schemars(with = "String")]`）。`BTreeMap<Status, u64>` は JSON ではキーが status 名のオブジェクト。

---

## 7. スキーマファイルと生成

| 事項 | 決め |
|---|---|
| ファイル | `docs/api/v1/` に 2 つ。**(1) `event.schema.json`**（Phase 9a で生成済み。ルート `EventRow`。task-core のテストが一致を検証）。**(2) `api-v1.schema.json`**（Phase 9b。ルート `ApiV1Schema`、`Event` / `EventRow` / `Task` などの共有型は `$defs` に 1 回だけ現れる）。**GUI の型生成は (2) だけを読む**（型ごとにファイルを分けると各ファイルが自分の `$defs` に `Task` / `Event` を抱え、TS 生成で同名の型が重複するため。(1) は taskd 自身の契約・テスト用） |
| 生成 | `UPDATE_SCHEMA=1 cargo test -p task-api`（`schemars::schema_for!(ApiV1Schema)`、整形は `serde_json::to_string_pretty`、末尾改行 1 つ）。既存の `task-worker` / `task-core` と同じ手順 |
| 一致テスト | `crates/task-api/src/schema.rs` の `committed_schema_matches_generated`（生成結果 == コミット済みファイル。差分があればテスト失敗、メッセージで再生成コマンドを示す） |
| 方言 | schemars 1.x の既定（JSON Schema 2020-12、`$defs`）。`$dynamicRef` 等は使わない。`json-schema-to-typescript` 16 が読める範囲に留める（G0 で確認。読めなければ taskd 側で `SchemaSettings::draft07()` に切り替える提案を出す） |
| GUI 側 | `pnpm gen:types` = `json2ts -i "$TASKD_REPO/docs/api/v1/api-v1.schema.json" -o app/taskd/types.ts --additionalProperties=false`。生成物をコミットし CI で差分ゼロを検査 |
| 互換性 | フィールドの追加（任意）は v1 のまま。削除・型変更・意味変更は `/api/v2` |

---

## 8. taskd 実装者向けの補足（テストの観点）

最低限、次を `crates/task-api/tests/` に置く（fake の `SqliteStore` と `tempfile` だけで動く。ネットワークは loopback のみ）。

1. **認証**: `token_file` あり → 無トークン 401、誤トークン 401、正トークン 200。`token_file` 無し + 非 loopback bind → `Config::validate` がエラー。`/health` は無トークンで 200。
2. **Host / Origin**: `Host: evil.example` → 400。`POST` に `Origin` → 403。`Content-Type` 無し → 415。1 MiB 超 → 413。未知フィールド → 400。
3. **一覧**: 250 件で `limit=100` を 3 回たどって全件・重複なし・順序どおり（3 つの `order` 全て）。`q` の `%` エスケープ。`cursor` の改竄 → 400。
4. **詳細**: `GET /tasks/{id}` の本体が、同じ `ViewContext` で呼んだ `task_ops::view::task_detail` の compact な直列化と byte 単位で一致（`timers.now` と API が埋める `runs[].files` を除く。`taskctl show --json` も同じ関数・同じ直列化。§3.5）。
5. **操作**: gate.rs / add.rs / plan.rs / cancel.rs / replay.rs の既存テストを task-ops に移したうえで、HTTP 越しに同じケース（approve の 4 通り、reject の 2 通り、answer の 3 通り、cancel の終端 3 通り、add の検証 5 通り）を確認。`expected_status` 不一致 409 `conflict` で状態不変。2 つ目の同じ approve が 409 `invalid_transition`。
6. **伝播**: Approval を reject → 子が `cascaded` に入る。先行を cancel → 後続が `dependency_failed` で `cascaded` に入る。
7. **ファイル**: `../x`、絶対パス、ワークスペース外への symlink、`run_id` に `..` → 全て 403。存在しない run → 404。`Range` と `offset` の 200 / 206 / 416。`.html` の成果物が `application/octet-stream` で返る。`X-Taskd-Sha256-Current` が改変後に変わる。
8. **SSE**: 購読中に `append_event` → 2 秒以内に `task.event` が届く。`Last-Event-ID` で再開して取りこぼし・重複なし。10,001 件遅れで `reset`。17 本目の接続が 503。切断後にポーリングが止まる（`events_since` 呼び出し回数で確認）。
9. **daemon**: `watch` に値を送る → `GET /daemon` と SSE `daemon` に反映。送る前は `snapshot: null`。
10. **スキーマ**: `committed_schema_matches_generated`。`GET /schema` の本体がファイルと一致。
11. **同時アクセス**: ディスパッチャ相当の書き込みループ（別スレッド、別接続）と API の読み取り 1,000 回を並走させて `database is locked` が出ない（WAL + busy_timeout の確認）。

---

## 9. 未決・確認事項

1. **Reviewer run の使用量**: `WorkerStarted` / `WorkerFinished` が記録されないため、アカウント別のトークン集計から漏れる。Reviewer run にも同じイベント（または `ReviewerRunFinished{run_id, provider, usage}`）を残すかは taskd 側の判断（P-G14 として `taskd-proposals.md` に追加）。
2. **一括承認**（Plan の子を全部 Accept）: API には置かない。GUI が 1 件ずつ `POST /tasks/{id}/approve` を直列に呼ぶ（原子性が無いことを UI に明記）。
3. **`q` の対象**: `title` のみ。`objective` の検索が要るなら `tasks.objective` 列の追加を提案する。
4. **`StaticPolicy` が cooldown の理由を保持するか**: `Cooldown.reason` は `Option`。保持しない実装でも仕様は満たす。
5. **`/health` を無認証にすること**: 版と `journal_mode` だけを返す。問題があれば認証必須に変える（GUI は G0 の疎通確認をトークン付きで行えばよい）。
6. **`POST /tasks` の追加検証**（`title` / `objective` の空白、`parent` の存在、`workspace` の空文字）: Phase 9a の task-ops は CLI と同じく検査しない。API 越しでも同じにしてある（挙動を変えない）。GUI 側はフォームの必須欄で防ぐ。taskd 側で足すなら task-ops に置き CLI も同じ関数を通す（提案として `taskd-proposals.md` P-G16）。

---

## 10. Phase 9b の実装で確定した細部（GUI から見える挙動）

本文が明示していなかった点を、taskd の実装（Phase 9b、ADR-0013「実装メモ」）に合わせて確定したもの。GUI はこれを契約として扱ってよい。

**要求の検査**
- 順序: Host → `OPTIONS` の 405 → 認証 → `POST` の Origin / Content-Type / 本文サイズ → ルーティング。
  - 認証が有効な構成では、未定義のパスも 404 より先に 401 になる。
  - `Content-Type` の無い `POST` は、未定義のパスでも 415 になる。
- 400 `bad_request` / `host_not_allowed` になるもの:
  - Host ヘッダが複数ある要求、absolute-form の URI で authority が許可されない要求。
  - **未知のクエリパラメータ**と、単一値のキーの重複（全エンドポイント）。`status` / `kind` の繰り返し指定は可。
  - 数値でない `Last-Event-ID`。
- 空文字の `q=` / `cursor=` は指定無しとして扱う。

**存在しない id**
- `/events?task_id=<存在しない id>` → 200 で空のページ。
- `/graph?root=<存在しない id>` → 404 `task_not_found`。
- 422 の `errors[].field` は、推定できるときだけ（`acceptance` / `depends_on` / `goal` / `answer`）。

**派生値**
- `RunSummary.outcome_text`: `done` 以外（`question` / `requeue`）でも接頭辞を除いた残りを入れる。`error` は文字列全体、`lease_expired` は `null`。
- `DaemonSnapshot.in_flight[]` の `kind: "reviewer"` の `run_id` は、**レビュー対象のワーカー run の id**（Reviewer run 自身の id は events に残らないため）。
- `Providers.items[].stats`:
  - 最初の `GET /providers` で全イベントを走査し、以後は要求のたびに増分だけ読む（taskd のメモリ上の観測値。再起動で再計算）。
  - `runs` は `WorkerStarted` の数（実行中を含む）。`by_day[].runs` はその日に終わった run の数。
- Remote ワークスペースの `runs[].files` は `null` ではなく全て `false`。64 MiB を超える成果物は `sha256_current` と `sha256_matches` が `null`。

**SSE**
- 送る順は `hello` →（必要なら）`reset` → `task.event` …。
- 遅れの判定は `最新 id − 要求 id > 10,000`。

**ファイル**
- `run_id` の形式検査は、ワークスペースの解決より先に行う（不正な `run_id` は、ワークスペースが無くても 403）。
- run ディレクトリが無ければ 404 `run_not_found`、ファイルが無ければ 404 `file_not_found`、ディレクトリなら 403。
- 成果物のパスは、空・絶対パス・`..` を含むものを字句的に 403 にしてから canonicalize する。
- 範囲指定:
  - `Range` の開始がサイズ以上なら 416（空ファイルを含む）。
  - `bytes` 以外の単位は無視して全体を 200 で返す。形が不正なら 416。
  - `offset` / `length` は常に 200。

**起動**
- taskd は `[api]` の `token_file` が読めない・空なら exit 2。DB が知らない新しい版数でも exit 2。
- API の DB 接続を開けない・bind できない場合は起動に失敗する（API 無しで動き続けない）。
- `taskd_version` は taskd crate の版（現在 `"0.1.0"`）。
