# celeris HTTP API v1 仕様

- 状態: **Accepted**（人間の決定 H1 / H5〜H7。celeris 側の ADR-0013、GUI 側の ADR-GUI-0001）。改訂日 2026-09-14
- 改訂: 2026-09-21 Phase 67（ADR-0054 D1、ノードごとの継続セッションと resume）— **追加のみ。v1 のまま**。
  エンドポイント 98: `POST /console/new-conversation`（§3.109。CoS の継続セッションを捨てる。**管理系**、
  204、本文なし）。DB のスキーマ版数は **23**（migration 0023: `node_sessions`。ノードごとの継続セッション
  の目印。本文・トークンの値は書かない）。CoS の対話・部門長のレビュー run 自体の挙動（`--resume` 等、
  前置きの差分化）は `GET /console` / `POST /console/instruct`（§3.98 / §3.107）の応答の形を変えない
  （継続かどうかはサーバ内部の判断で、API の応答契約には出ない）。
- 改訂: 2026-09-21 Phase 65（ADR-0053 D1/D4、LLM source のローカル OpenAI 互換プロキシ）— **追加のみ。
  v1 のまま**。エンドポイント 97: `GET /llm/sources`（§3.108。供給元ごとの到達性・アカウントの残量・
  cooldown・直近 1 時間の要求/token 数。読み取りだが認証は必要。`[llm_proxy]` が無効なら 409
  `llm_proxy_unavailable`）。GUI の表示は Phase 66。プロキシ自体（`POST /v1/chat/completions` 等）は
  `127.0.0.1:18100` の別ポートで、この `/api/v1` 契約には含まれない（`docs/llm-source.md` 参照）。
  DB のスキーマ版数は **22**（migration 0022: `llm_proxy_requests`。プロキシの要求記録。本文は書かない）。
- 改訂: 2026-09-21 バグ報告の対応（昇格の成否が画面に出なかった）— **追加のみ。v1 のまま**。
  `GET /releases` の `items[]` に `promote_failed`（`promote_failed.json`。直近の昇格の試みが
  失敗した記録）が増えた。§3.66〜3.67。
- 改訂: 2026-09-20 Phase 59（ADR-0046、組織 = Agent Profile の継承木）— **追加のみ。v1 のまま**。
  (1) 組織のノードが `profile` を持つようになった（`OrgNode.profile`、`GET /org` の `effective_profiles[]`、
  `POST /org` と `PATCH /org/{id}` の `profile`。§3.42〜3.44）。
  (2) タスクに `skills` / `mode` が増え、`harness`（= `Task.genre`）が編集できるようになった
  （`POST /tasks`、`PATCH /tasks/{id}`。§3.74）。
  (3) イベント種別 `assigned`（matching が担当を決めた。`{node, score, reason}`）が増えた。
  DB のスキーマ版数は **16**（migration 0016: `org_nodes.profile_json` / `tasks.skills_json` / `tasks.mode`）。
  設定は `[[genres]]` + `[[roles]]` から `[[harnesses]]` に移った（互換の読み込みは残る。GUI からは
  `GET /config` の `genres[]` がそのままハーネスの一覧として見える）
- 改訂: 2026-09-19 Phase 57（ADR-0044 B3、文書）— **追加のみ。v1 のまま。DB は変わらない**（正本は
  git のファイル）。案件の文書が GUI から読み書きできるようになった: エンドポイント 81〜86
  （`GET /projects/{id}/docs`、`GET|PUT|DELETE /projects/{id}/docs/page`、
  `POST /projects/{id}/docs/init`、`POST /tasks/{id}/artifacts/promote`。§3.92〜3.97）。
  `GET /tasks/{id}/timeline` に `kind = "doc"`（逆リンク）が増えた。エラーコードに
  `docs_unavailable`（409）・`etag_mismatch`（409）・`page_exists`（409）・`page_not_found`（404）が増えた。
  **変更系は管理系**（`token_file` 未設定でも 401）で、**組織の「人」は呼べない**
  （ワーカーは自分の worktree のブランチに文書を書き、人が取り込む）
- 改訂: 2026-09-19 Phase 55（ADR-0044 B2 = D6、中止・一時停止・アーカイブ / §5 Phase 53 追記）—
  **追加 + 認可の破壊的変更**。(1) 案件・途中目標を止められるようになった: エンドポイント 73〜80
  （`POST /projects/{id}/{cancel|pause|resume|archive|unarchive}`、
  `POST /milestones/{id}/{cancel|pause|resume}`。§3.84〜3.91）、`ProjectStatus` に `cancelled`、
  `MilestoneStatus` に `paused` / `cancelled`、`Project.{archived_at, paused_from}`、
  `Milestone.paused_from`、`GET /projects?archived=` と `GET /tasks?archived=`、
  `Event::Transitioned.reason` に `project_cancelled` / `milestone_cancelled`。
  DB のスキーマ版数は **15**（migration 0015: `projects.archived_at` / `projects.paused_from` /
  `milestones.paused_from`）。(2) **変更を伴うエンドポイントを 1 つ残らず管理系に揃えた**
  （§1.3。`POST /tasks`、`approve` / `reject` / `answer` / `cancel` / `retry`、`POST /plans`、
  `POST /replay`、`PATCH /projects/{id}`、`POST /projects/{id}/milestones`、`PATCH /milestones/{id}` が
  トークン必須になった。**v1 の破壊的変更**。GUI は BFF がトークンを持つので画面は変わらない）。
  (3) 走っている run の**止め方が 1 つになった**: `cancel` / 人のコメントによる割り込み /
  タイムアウト / リース喪失 / drain のどれも、ワーカーの**プロセスグループ**へ SIGTERM →
  `kill_grace_secs` → SIGKILL（ハーネスが起こした孫まで消える）
- 改訂: 2026-09-19 Phase 54（ADR-0043 A2、変更の取り込み）— **追加のみ。v1 のまま**。タスクが
  作ったブランチを**人が**見て取り込めるようになった: エンドポイント 68〜72
  （`GET /tasks/{id}/changes`、`GET /tasks/{id}/changes/{repo}/diff`、
  `POST /tasks/{id}/changes/{repo}/integrate`、`POST /tasks/{id}/changes/{repo}/pr/merge`、
  `GET /projects/{id}/integrations`。§3.79〜3.83）。取り込みの記録は `task_integrations`
  （DB のスキーマ版数は **14**。migration 0014）。エラーコードに `default_branch_busy`（409）と
  `pr_unavailable`（409）が増えた。**取り込みの変更系は管理系**（`token_file` 未設定でも 401）で、
  **組織の「人」＝ワーカーからは呼べない**（ワーカープロトコルには出していない）。
  `config.toml` に `[github]`（`gh` / `merge_method`）が増えた
- **マージ（Phase 54）**: Phase 53（ADR-0044 B1）と並行で進んだ枝なので、番号を整えた。
  **ADR-0043 A2 は §3.79〜3.83（エンドポイント 68〜72）**、B1 の §3.74〜3.78（63〜67）はそのまま。
  migration は **0013 = B1（`task_comments`）、0014 = A2（`task_integrations`）**で `SCHEMA_VERSION = 14`。
  `GET /tasks/{id}/timeline`（§3.78）の `integration` は A2 の記録から**実際に出るようになった**
- 改訂: 2026-09-19 Phase 53（ADR-0044 B1、タスク管理）— **追加のみ。v1 のまま**。人がタスクを手で触れる
  ようになった: エンドポイント 63〜67（`PATCH /tasks/{id}`、`GET|POST /tasks/{id}/comments`、
  `POST /tasks/{id}/reopen`、`GET /tasks/{id}/timeline`。§3.74〜3.78）、`Task.labels[]` / `Task.category`、
  `TaskSummary.{labels, category, priority_label, project_id, milestone_id}`、`TaskDetail.priority_label`、
  `GET /tasks` の絞り込み（`labels` / `categories` / `milestone_id` / `tiers` / `priorities` / `q`）、
  イベント種別 `edited`、`Action::{Edit, Reopen}`、`RunOutcomeKind::Interrupted`。
  `POST /tasks` の既定が `ready`（人の経路だけ。`celerisctl add` と計画・委譲は従来どおり `draft`）、
  `priority` の既定が **P2（= 10）**。DB のスキーマ版数は 13（migration 0013: `task_comments` と
  `tasks.labels_json` / `tasks.category`）
- **マージ（Phase 52 + 53）**: 両方の節が同じ番号を取っていたので、**ADR-0043 A1 が §3.68〜3.73
  （エンドポイント 57〜62）、ADR-0044 B1 が §3.74〜3.78（エンドポイント 63〜67）**に整えた。
  `PATCH /tasks/{id}`（§3.74）は ADR-0043 D2 の **`repos`** も受ける
- 改訂: 2026-09-19 Phase 52（ADR-0043 A1、ワークスペース）— **追加のみ。v1 のまま**。案件が「リポジトリ」を
  複数持てるようになった（`project_repos`）: エンドポイント 57〜60（`GET|POST /projects/{id}/repos`、
  `PATCH|DELETE /repos/{id}`）、`ProjectDetail.repos[]`、`POST /tasks` の `repos`（名前の配列）、
  `Task.repos[]`（`{repo_id, name}`）。タスクの作業ツリーが GUI から読めるようになった:
  エンドポイント 61〜62（`GET /tasks/{id}/tree`、`GET /tasks/{id}/tree/file`。読み取り。トークン不要）。
  **`Project.workspace` は primary のリポジトリの `location` の写し**（GUI の後方互換。従来どおり読める）。
  DB のスキーマ版数は 12（migration 0012: `project_repos` と `tasks.repos_json`）。
  worktree のブランチ接頭辞の既定が `celeris/` → **`celeris/`**、`workspace_root` の既定が
  **`~/.local/celeris/workspaces`** に変わった（ADR-0042 D3。どちらも `config.toml` で変えられる）。
  worktree は**終端では消えなくなり**、**中止（`POST /tasks/{id}/cancel`）で worktree とブランチが消える**（ADR-0043 D2）
- 改訂: 2026-09-19 Phase 51（ADR-0041 D5）— **型は変わらない**。`mode = "verify"` のプロセスに限り、
  `GET /config` の `roles[]` / `genres[]` / `providers[]` に組み込みの `smoke`（`adapter = "fake"`）が
  1 つずつ増え、`reviewer` が `{adapter: "fake", tier: "standard"}` になる（`Config::apply_verify_smoke`。
  設定ファイルに同じ id があっても上書きする）。`mode = "normal"`（本番）の応答は 1 バイトも変わらない。
  `POST /tasks` はそのプロセスで `{"genre":"smoke","role":"smoke"}` を受け付け、verify の celeris はその
  タスク**だけ**を dispatch する（§3.1、`docs/selfdeploy.md` の検査 6）
- 改訂: 2026-09-19 Phase 50（ADR-0041 D3/D4）— `GET /releases` の `items[]` に `promoted_at` / `on_main` /
  `changes`、`POST /releases/{sha12}/promote` の応答に `script_from` を追加（追加のみ。v1 のまま）。
  昇格に使う `promote.sh` は**いま動いている版のもの**に変わった（§3.67）
- 改訂: 2026-09-18 Phase 38（ADR-0028 追記）— `GET /config` の `genres[].{input_artifacts, output_artifacts}` の
  各要素は `"名前"` に加えて **`"名前: 説明"`** の形も取る（型は `Vec<String>` のまま。追加のみ、v1 のまま）。
  **GUI は `:` の前を成果物の名前として扱い、後ろを説明として出すこと**（`ConfigView` / `GenreConfigView` の
  型は変えていない。値の文字列に説明が付くだけ）。
- 改訂: 2026-09-18 Phase 31（実機の事故: 失敗した仕事をやり直す手段が無かった）— `POST /tasks/{id}/retry`
  （§3.63）、イベント種別 `retried`、`task_ops::actions` に `Action::Retry` を追加（追加のみ。v1 のまま）
- 改訂: 2026-09-17 Phase 27（Phase 24/25 の監査対応、GUI-R3/R4、ADR-0034 D7）— `TaskSummary.assignee` /
  `TaskSummary.conversation`、`ProjectTaskView.conversation`、`Message.task_id` を追加（追加のみ。v1 のまま）。
  **`POST /projects` を管理系に変更**（`token_file` 未設定でも 401。破壊的変更はここだけ）。
  `POST/PATCH /org` の `genre` は `[[genres]]` にあるものだけ（無い id は 422）。
  `GET /approvals?pending=false` が「決定済みだけ」に絞り込むようになった（R5。以前は全件）。
  DB のスキーマ版数は 7（migration 0007: `messages.task_id` の追加と `reports.project_id` の NULL 可）
- 改訂: 2026-09-17 Phase 18（ADR-0028 D1/D4、分野を能力レジストリに）— `GET /config` の
  `genres[].{capabilities, input_artifacts, output_artifacts}` を追加（3 つとも任意、空なら省略。追加のみ。v1 のまま）
- 改訂: 2026-09-17 Phase 16（ADR-0027 D1、分野）— `POST /tasks` の `genre`、`TaskSummary.genre` / `TaskDetail.genre`、
  `GET /tasks` の `genre=` クエリ、`GET /config` の `genres[]` を追加（追加のみ。v1 のまま）
- 改訂: 2026-09-16 実機確認の反映 — エンドポイント 33（`runs/{run_id}/prompt`）、`RunFiles.prompt`、
  `ProviderCheckResponse.detail` / `last_check.detail` を追加（追加のみ。v1 のまま）
- 改訂: 2026-09-16 ADR-0023（run の指示の保存・子待ちの可視化）— エンドポイント 32（`runs/{run_id}/request`）、`RunFiles.request`、
  `DaemonSnapshot.awaiting_children[]` を追加（追加のみ。v1 のまま）
- 改訂: 2026-09-16 ADR-0022（疎通確認の記録）— `GET /providers` の `last_check` を追加（追加のみ。v1 のまま）
- 改訂: 2026-09-16 ADR-0021（委譲した子の失敗）— イベント種別 `question_raised`、`GET /config` の `delegation.on_child_failure` を追加（追加のみ。v1 のまま）
- 改訂: 2026-09-15 Phase 10（ADR-0016 役割と委譲）— `POST /tasks` の `role` / `aggregate`、`TaskDetail.role` / `delegated[]`、`GET /config` の `roles[]` / `delegation`、イベント種別 `delegated` を追加（全て追加のみ。v1 のまま）
- 提供者: **celeris**（crate `task-api`、axum）。celeris のデーモンプロセス内で、`config.toml` に `[api]` 節があるときだけ動く
- 利用者: `celeris-gui` の BFF（Remix = React Router framework mode のサーバ側 loader / action）と `curl`。**ブラウザは直接呼ばない**
- 正の型定義: Rust（`task-core` / `task-ops` / `task-api`、`serde` + `schemars`）。JSON Schema を `docs/api/v1/api-v1.schema.json` にコミットし、
  テストで生成一致を検証する（§7）。`celeris-gui` はこのファイルから TypeScript の型を生成する
- 実装の順序: celeris の Phase 9a（基盤: task-ops / WAL / events の id / スキーマ版数 / ProviderThrottled / list_page）→ 9b（本仕様）。
  GUI の G フェーズは 9b 完了後に始める（DESIGN-GUI §10）

本文書は celeris の実装者が**これだけで実装とテストを書ける**ことを目標にする。各エンドポイントの要求・応答の型、ステータスコード、境界条件を書く。
派生値（受信箱、詳細、run の要約、プロバイダの集計）の**計算規則は celeris 側（`task-ops` / `task-api`）にある**。GUI は再計算しない。

---

## 1. 全体

### 1.1 有効化と設定（`config.toml`）

```toml
[api]
listen = "127.0.0.1:7710"      # これを書いたときだけ API が動く（Option<SocketAddr>。無ければ無効）。推奨ポートは 7710（GUI は 7700）
# token_file = "secrets/api.token"       # 非 loopback で listen するとき必須。指定があれば loopback でも要求する。相対パスは設定ファイル基準
# allowed_hosts = ["celeris.lab.example"]  # Host 許可リストへの追加（localhost / 127.0.0.1 / [::1] とポート付きの形は常に許可）
```

- `listen` が無ければ API は動かない（既定は無効。デーモンに暗黙のネットワーク口を開けない）。Phase 9a の `celeris::config::ApiConfig{listen, token_file, allowed_hosts}` がこの形。
- `listen` が loopback（`127.0.0.0/8`、`::1`）以外で `token_file` が無い → `ConfigError::Invalid`（`[api] listen = … is not a loopback address; token_file is required`。起動時 exit 2）。
- `token_file` の内容（前後の空白を除いた 1 行）がトークン。ファイルが読めない・空 → 設定エラー。トークンはログにも API にも出さない。
- SSE の同時接続数の上限は task-api の定数 16（設定キーにしない）。
- API は**自分専用の `SqliteStore` 接続**を持ち、DB 呼び出しは `spawn_blocking` で行う（ディスパッチャの接続と Mutex を共有しない。ADR-0013 D3）。
- `providers_include = "providers.d/*.toml"`（トップレベル、`[[providers]]` と併用可。ADR-0017 M1）を設定すると、
  §3.24〜3.28 のプロバイダ管理エンドポイントが `providers.d/<id>.toml`（1 アカウント 1 ファイル、ファイル名昇順で
  `[[providers]]` の後ろに連結）を読み書きできるようになる。未設定なら 5 本とも 409。
- `[secrets] dir = "secrets"`（ADR-0030 D1。GUI から API キー等を預かる置き場所）を設定すると、§3.36〜3.38 の
  秘密管理エンドポイントが `dir` 配下に 1 秘密 1 ファイル（0600）で読み書きできるようになる。未設定なら 3 本とも
  409 `secrets_unavailable`。値を環境変数として run に流し込む `env_from_secrets` は `[adapters.<種別>]` と
  `[[providers]]` の行の両方に書ける（§3.36 参照）。

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

- `token_file` が設定されていれば全エンドポイント（`GET /health` を除く）で `Authorization: Bearer <token>` を要求する。無ければ 401 `unauthorized`（`WWW-Authenticate: Bearer realm="celeris"`）。比較は定数時間。
- `token_file` が無い（= loopback のみ）場合は認証しない。**ただし管理系エンドポイントは例外**で、`token_file` が無くても常に 401 にする（ADR-0017 D1: loopback でも管理操作にはトークンを要求する）。管理 API を使うには `token_file` の設定が要る。
- **Phase 55（ADR-0044 §5 Phase 53 追記）から、変更を伴うエンドポイントは 1 つ残らず管理系**（`POST` / `PATCH` / `PUT` / `DELETE` の全部）。読み取り（`GET`）は従来どおりで、`token_file` が無ければ loopback から素通しのまま。
  - この Phase で管理系に揃えたもの（それまでトークン不要だった）: `POST /tasks`、`POST /tasks/{id}/{approve|reject|answer|cancel|retry}`、`POST /plans`、`POST /replay`、`PATCH /projects/{id}`、`POST /projects/{id}/milestones`、`PATCH /milestones/{id}`。**v1 の破壊的変更**（冒頭の変更点一覧）。
  - GUI は BFF がトークンを持つ（`CELERIS_API_TOKEN_FILE`）ので画面は変わらない。`celerisctl` は HTTP API を使わず SQLite を直接開くので影響しない。組織の「人」（ワーカー）はそもそも API を叩かない（SPEC §3.6）。
- `GET /health` は常に無認証（版とスキーマ版数だけを返す。G0 の疎通確認用）。ただし Host 検査は受ける。

### 1.4 Host 検査・Origin・CSRF

- 全要求で `Host` ヘッダを許可リスト（`localhost`、`127.0.0.1`、`[::1]`、`listen` のホスト、`allowed_hosts`。ポートは無視）と照合し、外れれば 400 `host_not_allowed`。DNS rebinding 対策。
- 変更系（`POST`/`PATCH`/`DELETE`。Phase 11 で `PATCH`/`DELETE` が増えた）に `Origin` ヘッダが付いていれば 403 `origin_forbidden`。ブラウザから直接呼ばれる設計ではないので、`Origin` の存在自体を「想定外の呼び出し」とみなす（`curl` と Node の `fetch` は `Origin` を送らない）。Content-Type / 本文サイズの検査は本文を伴う `POST`/`PATCH` だけ（`DELETE` は本文を取らない）。
- `Content-Type: application/json` の要求（1.2）と合わせて、フォーム送信型の CSRF は成立しない。

### 1.5 エラー

`application/problem+json`（RFC 9457）。本体は `Problem`:

```json
{"type":"urn:celeris:problem:invalid_transition","title":"invalid transition","status":409,
 "detail":"task 01J… (kind=execute, status=done) cannot be approved","code":"invalid_transition",
 "instance":"urn:celeris:request:01J…","task_status":"done","kind":"execute","trigger":"approve"}
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
| `org_node_not_found` / `project_not_found` / `milestone_not_found` | 404 | 組織のノード / 案件 / 途中目標が無い（ADR-0033、§3.42〜3.49。ULID でない案件・途中目標の id もここ） |
| `report_not_found` | 404 | 報告が無い（ADR-0033 D3、§3.50〜3.53。ULID でない id もここ） |
| `approval_not_found` / `standing_rule_not_found` | 404 | 認可 / 永続の認可が無い（ADR-0033 D5、§3.56〜3.60。ULID でない id もここ） |
| `method_not_allowed` | 405 | |
| `conflict` | 409 | `expected_status` 不一致。`expected`, `actual` |
| `invalid_transition` | 409 | 状態機械または task-ops の写像が拒否。`task_status`, `kind`, `trigger`（`InvalidTransition{status, kind, trigger}` の写し。`trigger` は `Trigger::name()`） |
| `org_node_exists` | 409 | `POST /org` の `id` が既にある（更新は `PATCH /org/{id}`） |
| `org_node_in_use` | 409 | 消そうとした組織のノードが未終了のタスクを抱えている、または子を持つ（ADR-0033 D1） |
| `release_not_found` | 404 | ADR-0040 D6: `POST /releases/{sha12}/promote` の sha12 が `[selfdeploy] releases_dir` に無い（sha12 の形でないときも同じ） |
| `release_not_promotable` | 409 | ADR-0040 D6: 昇格を受け付けられない（`verify.json` が無い／`ok` でない、既に `current`、既に昇格中、`scripts/promote.sh` が無い、`[selfdeploy]` が無い）。`detail` に理由の一行 |
| `default_branch_busy` | 409 | ADR-0043 D5（§3.81）: 人のチェックアウトが取り込み先のブランチを出したまま未コミットの変更を持っている。`detail` は `"<default_branch> が編集中"`。**何も触っていない**ので、人が片付けてからもう一度押す |
| `pr_unavailable` | 409 | ADR-0043 D5（§3.81 / §3.82）: PR の経路が使えない（`origin` リモートが無い、`gh` が PATH に無いか認証されていない、merge しようとした PR が開いていない）。`detail` に理由の一行 |
| `docs_unavailable` | 409 | ADR-0044 D7（§3.92〜3.97）: 案件の文書の根が使えない（まだ無い＝`POST /projects/{id}/docs/init` で用意する、primary がリモート、`$HOME` が無い、`git` が動かない、置き場が空でない）。`detail` に理由の一行 |
| `etag_mismatch` | 409 | ADR-0044 D7（§3.95 / §3.96）: 読んでから誰かがそのページを直した（ページがあるのに `etag` を付けなかったときも同じ）。拡張フィールド `etag` にいまの値。再読み込みしてから直す |
| `page_exists` | 409 | ADR-0044 D7（§3.97）: 昇格の宛先にもうページがある。`overwrite: true` なら上書きできる |
| `page_not_found` | 404 | ADR-0044 D7（§3.93 / §3.96）: そのパスのページが default_branch に無い |
| `payload_too_large` | 413 | 本文 > 1 MiB |
| `unsupported_media_type` | 415 | 変更系で `Content-Type` が JSON でない |
| `range_not_satisfiable` | 416 | ファイル系の `Range` / `offset` がサイズを超える。`Content-Range: bytes */<size>` |
| `validation` | 422 | task-ops の検証失敗。`errors: [{field?, message}]`。`message` は `celerisctl` と同じ文言（§5.6）。`field` は task-api が文言から推定できるときだけ（`acceptance` / `depends_on` / `goal`） |
| `too_many_streams` | 503 | SSE 接続数が 16 を超えた。`Retry-After: 5` |
| `db_busy` | 503 | `SQLITE_BUSY`（busy_timeout 超過）。`Retry-After: 1` |
| `standby` | 503 | ADR-0040 D4: いまこのプロセスは `standby`（または `draining`）なので、ディスパッチャの状態を要する管理系（`POST /reload`、`POST /providers/{id}/check`、クラスタ接続、アカウントの確認・削除・ログイン中継、`POST /notify/test`）を受けられない。`detail` は `"standby"`、`Retry-After: 2`。窓は 1〜2 tick（昇格の引き継ぎ中）なので、GUI はその間だけ「切り替え中」を出して再送すればよい。読み書きの通常のエンドポイントはそのまま動く |
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

`StoreError::SchemaTooNew` は起動時に起きるので API のエラーにはならない（celeris が exit 2）。

---

## 2. エンドポイント一覧（95）

| # | メソッド | パス | 目的 | 応答型 | 出所 |
|---|---|---|---|---|---|
| 1 | GET | `/health` | 版、スキーマ版数、DB の journal_mode | `Health` | task-api |
| 2 | GET | `/inbox` | 承認待ち / 質問 / draft / 注意 | `Inbox` | task-ops（+ スナップショット） |
| 3 | GET | `/tasks` | 一覧（フィルタ・keyset ページング） | `TaskList` | store `list_page` + task-ops |
| 4 | POST | `/tasks` | `celerisctl add` 相当 | 201 `Task` | task-ops |
| 5 | GET | `/tasks/{id}` | 詳細（`celerisctl show --json` と同一） | `TaskDetail` | task-ops |
| 6 | GET | `/tasks/{id}/events` | そのタスクのイベント（`after_seq`） | `EventsPage` | store `events_for` |
| 7 | GET | `/tasks/{id}/runs` | run の要約一覧 | `RunList` | task-ops + ファイル存在 |
| 8 | GET | `/tasks/{id}/runs/{run_id}/stdout` | `runs/<run_id>/stdout.jsonl` | バイト列 | ファイル |
| 9 | GET | `/tasks/{id}/runs/{run_id}/stderr` | `runs/<run_id>/stderr.log` | バイト列 | ファイル |
| 10 | GET | `/tasks/{id}/runs/{run_id}/result` | `runs/<run_id>/result.json` | `application/json` | ファイル |
| 11 | GET | `/tasks/{id}/artifacts` | `ArtifactProduced` の一覧 + 現在の sha256 | `ArtifactList` | events + ファイル |
| 12 | GET | `/tasks/{id}/artifacts/{idx}` | 成果物本体 | バイト列 | ファイル |
| 13 | POST | `/tasks/{id}/approve` | `celerisctl approve`（draft → Accept / Approval → Approve） | `TransitionResult` | task-ops |
| 14 | POST | `/tasks/{id}/reject` | `celerisctl reject` | `TransitionResult` | task-ops |
| 15 | POST | `/tasks/{id}/answer` | `celerisctl answer` | `TransitionResult` | task-ops |
| 16 | POST | `/tasks/{id}/cancel` | `celerisctl cancel` | `TransitionResult` | task-ops |
| 17 | POST | `/plans` | `celerisctl plan` 相当 | 201 `Task` | task-ops |
| 18 | POST | `/replay` | `celerisctl replay`（読み取りのみ） | `ReplayReport` | task-ops |
| 19 | GET | `/graph` | DAG（`depends_on` の辺、`parent_id` の入れ子） | `Graph` | task-ops |
| 20 | GET | `/events` | 全タスク横断のイベント（`after_id`。ポーリング / `curl` 用） | `EventsPage` | store `events_since` |
| 21 | GET | `/stream` | SSE（§4） | `text/event-stream` | store `events_since` + スナップショット |
| 22 | GET | `/providers` | 定義 + 稼働状況 + 集計 | `Providers` | 設定 + スナップショット + task-api の集計 |
| 23 | GET | `/daemon` | ディスパッチャのメモリ上のスナップショット | `DaemonView` | `tokio::sync::watch` |
| 24 | GET | `/config` | `config.toml` の要約（秘密は出さない） | `ConfigView` | 設定（celeris が起動時に渡す） |
| 25 | GET | `/schema` | `api-v1.schema.json` の内容 | `application/schema+json` | `include_str!` |
| 26 | GET | `/clusters` | `[[clusters]]` の定義 + 接続の有無 + cooldown（ADR-0018、Phase 12） | `Clusters` | 設定 + スナップショット |
| 27 | POST | `/providers` | `providers.d/<id>.toml` を作る（ADR-0017、Phase 11。**管理系: トークン必須**） | 201 `ProviderConfigView`（`Location`） | celeris（ファイル書き込みのみ） |
| 28 | PATCH | `/providers/{id}` | 並列度・tier・model・env を変更する（**管理系**） | 200 `ProviderConfigView` | celeris（ファイル書き込みのみ） |
| 29 | DELETE | `/providers/{id}` | `providers.d/<id>.toml` を消す（**管理系**） | 200 `{}` | celeris（ファイル削除のみ） |
| 30 | POST | `/providers/{id}/check` | そのアカウントの env で短い疎通確認を 1 回行う（**管理系**） | 200 `ProviderCheckResponse` | celeris（`task-worker` 経由。タスク・イベントには残らない） |
| 31 | POST | `/reload` | 設定と `providers.d/` を読み直し、稼働中のプロバイダ選定・アダプタを差し替える（**管理系**） | 200 `ReloadResult` | celeris（`Dispatcher` の差し替え） |
| 32 | GET | `/tasks/{id}/runs/{run_id}/request` | `runs/<run_id>/request.json`（ワーカーに渡した `RunRequest`。ADR-0023 D2） | `application/json` | ファイル |
| 33 | GET | `/tasks/{id}/runs/{run_id}/prompt` | `runs/<run_id>/prompt.txt`（claude-code / codex に実際に渡した文面。ADR-0023 M1） | `text/plain` | ファイル |
| 34 | GET | `/secrets` | GUI から預かる秘密（API キー等）の一覧。値は含まない（ADR-0030、Phase 20。**管理系**） | `SecretList` | celeris（ファイル読み取りのみ） |
| 35 | PUT | `/secrets/{id}` | 秘密の作成／置き換え（0600）（**管理系**） | 200 `SecretPutResult` | celeris（ファイル書き込みのみ） |
| 36 | DELETE | `/secrets/{id}` | 秘密の削除（**管理系**） | 200 `{}` | celeris（ファイル削除のみ） |
| 37 | POST | `/clusters/{id}/connect` | 接続を開始する（コード不要で張れれば即完了。ADR-0032、Phase 22。**管理系**） | 200 `ClusterConnectStart` | celeris（`ssh -M -N` の起動・借用） |
| 38 | POST | `/clusters/{id}/connect/code` | 進行中の接続に検証コード（TOTP 等）を渡す（**管理系**） | 200 `ClusterConnectResult` | celeris（`SSH_ASKPASS` 経由の中継） |
| 39 | DELETE | `/clusters/{id}/connect` | 進行中の接続を取り消す、または張った接続を切る（**管理系**） | 200 `{}` | celeris（子プロセスの終了） |
| 40 | GET | `/org` | 組織（一つ、役割の木）の全ノード（ADR-0033 D1、Phase 23） | `OrgList` | store `org_list` |
| 41 | POST | `/org` | 役職を足す（**管理系**） | 201 `OrgNode`（`Location`） | store `org_upsert` |
| 42 | PATCH | `/org/{id}` | 役職を変える・付け替える（**管理系**） | 200 `OrgNode` | store `org_upsert` |
| 43 | DELETE | `/org/{id}` | 役職を消す。仕事を抱えていたら 409（**管理系**） | 204 | store `org_delete` |
| 44 | GET | `/projects` | 案件の一覧（新しい順。ADR-0033 D2） | `ProjectList` | store `project_list` |
| 45 | POST | `/projects` | 案件を投げる（`status = proposed`。直後に秘書の run が起きるので**管理系**。Phase 27） | 201 `Project`（`Location`） | store `project_create` |
| 46 | GET | `/projects/{id}` | 案件 + 途中目標 + 仕事の木 | `ProjectDetail` | store（`project_id` で絞った `tasks`） |
| 47 | PATCH | `/projects/{id}` | 案件の状態を変える | 200 `Project` | store `project_set_status` |
| 48 | POST | `/projects/{id}/milestones` | 途中目標を足す（`seq` はストアが採番） | 201 `Milestone`（`Location`） | store `milestone_create` |
| 49 | PATCH | `/milestones/{id}` | 途中目標の状態を変える（SPEC §7 のアジャイル） | 200 `Milestone` | store `milestone_set_status` |
| 50 | GET | `/org/{id}/messages` | そのノードとのやり取り（古い順。ADR-0033 D4、Phase 24） | `MessageList` | store `message_list` |
| 51 | POST | `/org/{id}/messages` | そのノードに話しかける（**管理系**） | 202 `MessageAccepted` | `task_ops::conversation::start` |
| 52 | GET | `/notify` | Discord への通知の設定と直近の送信（ADR-0037、Phase 39。URL は出さない） | `NotifyView` | 設定 + store `notification_recent` |
| 53 | POST | `/notify/test` | テスト送信を 1 回（**管理系: `token_file` 未設定でも 401**） | 200 `NotifyTestResult` | celeris（`[secrets]` の webhook へ POST） |
| 54 | POST | `/milestones/{id}/decide` | 途中目標の判定（`ok` / `discuss` / `ng`。ADR-0038 D2、Phase 41）（**管理系**） | 202 `MilestoneDecided` | `task_ops::milestone_review::decide` |
| 55 | GET | `/releases` | リリース一覧・検証状態・`current`/`previous`・引き継ぎの進行（ADR-0040 D6、Phase 48） | `Releases` | `[selfdeploy] releases_dir` のファイル + store `instance_list` |
| 56 | POST | `/releases/{sha12}/promote` | そのリリースへ昇格する（`promote.sh` を起こして 202）（**管理系: `token_file` 未設定でも 401**） | 202 `ReleasePromoteAccepted` | celeris（`<release>/scripts/promote.sh` を detached で起動） |
| 57 | GET | `/projects/{id}/repos` | その案件のリポジトリ（primary が先頭。ADR-0043 D1、Phase 52） | `RepoList` | store `repo_list` |
| 58 | POST | `/projects/{id}/repos` | リポジトリを足す（**管理系: `token_file` 未設定でも 401**） | 201 `ProjectRepo` | store `repo_create` |
| 59 | PATCH | `/repos/{id}` | リポジトリを変える（名前・場所・`run`・primary。**管理系**） | 200 `ProjectRepo` | store `repo_update` |
| 60 | DELETE | `/repos/{id}` | リポジトリを消す。未終端のタスクが使っていたら 409（**管理系**） | 204 | store `repo_delete` |
| 61 | GET | `/tasks/{id}/tree` | タスクの作業ツリーの一覧（ADR-0043 D6） | `TreeView` | `worktree.json` + ファイル |
| 62 | GET | `/tasks/{id}/tree/file` | そのファイルの本文（テキスト 512 KiB まで） | `TreeFileView` | ファイル |
| 63 | PATCH | `/tasks/{id}` | タスクを編集する（題名・目的・受け入れ条件・優先度・ラベル・種類・リポジトリ・担当・役割・tier・アダプタ・途中目標・依存・予算。ADR-0044 D1 + ADR-0043 D2、Phase 53）（**管理系**） | 200 `TaskEditResult` | `task_ops::edit::edit_task` |
| 64 | GET | `/tasks/{id}/comments` | そのタスクのコメント（古い順。ADR-0044 D2） | `CommentList` | store `comments_for` |
| 65 | POST | `/tasks/{id}/comments` | 人のコメント。状態に応じて**割り込み**・回答・記録になる（**管理系**） | 201 `CommentResult` | `task_ops::comment::post_human_comment` |
| 66 | POST | `/tasks/{id}/reopen` | 終端のタスクを同じ worktree のまま再開する（`done`/`failed` → `ready`）（**管理系**） | 200 `TransitionResult` | `task_ops::comment::reopen` |
| 67 | GET | `/tasks/{id}/timeline` | 起きたこと 1 本（イベント・コメント・認可・報告・委譲・リリース・取り込み。ADR-0044 D5） | `Timeline` | store + `[selfdeploy] releases_dir` |
| 68 | GET | `/tasks/{id}/changes` | リポジトリごとの差分の要約と取り込みの記録（ADR-0043 D5、Phase 54） | `ChangesView` | `worktree.json` + `git` + store |
| 69 | GET | `/tasks/{id}/changes/{repo}/diff` | 1 ファイルの unified diff（200 KiB で切る） | `ChangeDiffView` | `git diff` |
| 70 | POST | `/tasks/{id}/changes/{repo}/integrate` | 取り込む（`merge` / `pr` / `discard`）（**管理系: `token_file` 未設定でも 401**） | 200 `IntegrateResult` | `git` / `gh` + store |
| 71 | POST | `/tasks/{id}/changes/{repo}/pr/merge` | その PR を Celeris から merge する（**管理系**） | 200 `IntegrateResult` | `gh pr merge` + store |
| 72 | GET | `/projects/{id}/integrations` | その案件の PR と取り込み（タスク × リポジトリごとに最新の 1 件） | `ProjectIntegrations` | store + `gh pr view` |
| 73 | POST | `/projects/{id}/cancel` | 案件を中止し、属する非終端タスクと途中目標を連鎖で `cancelled` にする（ADR-0044 D6、Phase 55。**管理系: `token_file` 未設定でも 401**） | 200 `ProjectLifecycle` | `task_ops::lifecycle` |
| 74 | POST | `/projects/{id}/pause` | 案件を一時停止する（属するタスクは dispatch されない。**管理系**） | 200 `ProjectLifecycle` | `task_ops::lifecycle` |
| 75 | POST | `/projects/{id}/resume` | 一時停止を解く（`paused_from` へ戻す。**管理系**） | 200 `ProjectLifecycle` | `task_ops::lifecycle` |
| 76 | POST | `/projects/{id}/archive` | 終端の案件をアーカイブする（一覧から既定で隠れる。**管理系**） | 200 `ProjectLifecycle` | `task_ops::lifecycle` |
| 77 | POST | `/projects/{id}/unarchive` | アーカイブを解除する（**管理系**） | 200 `ProjectLifecycle` | `task_ops::lifecycle` |
| 78 | POST | `/milestones/{id}/cancel` | 途中目標を中止し、属する非終端タスクを連鎖で `cancelled` にする（**管理系**） | 200 `MilestoneLifecycle` | `task_ops::lifecycle` |
| 79 | POST | `/milestones/{id}/pause` | 途中目標を一時停止する（**管理系**） | 200 `MilestoneLifecycle` | `task_ops::lifecycle` |
| 80 | POST | `/milestones/{id}/resume` | 一時停止を解く（**管理系**） | 200 `MilestoneLifecycle` | `task_ops::lifecycle` |
| 81 | GET | `/projects/{id}/docs` | 案件の文書のツリー（`?q=` は `git grep -il`。ADR-0044 D7、Phase 57） | `DocsTree` | `git ls-tree` / `git log` |
| 82 | GET | `/projects/{id}/docs/page` | ページ 1 枚（raw / html / front matter / 履歴 / etag） | `DocPage` | `git show` / `git log` |
| 83 | POST | `/projects/{id}/docs/init` | 文書リポジトリを用意する（無い案件だけ。**管理系**） | 200 `DocsInitResult` | `git init` + store |
| 84 | PUT | `/projects/{id}/docs/page` | ページを既定のブランチに直接コミットする（**管理系**） | 200 `DocPageResult` | 一時 worktree + `git commit` |
| 85 | DELETE | `/projects/{id}/docs/page` | ページを消す（**管理系**） | 200 `DocPageResult` | 一時 worktree + `git commit` |
| 86 | POST | `/tasks/{id}/artifacts/promote` | 成果物をページに昇格する（**管理系**） | 200 `DocPageResult` | 成果物 + 一時 worktree |
| 87 | GET | `/console` | 全案件の流れ（正規化したブロック、時刻順、カーソル付き。ADR-0048 D1、Phase 60a） | `ConsolePage` | events + messages + approvals + milestones + reports |
| 88 | GET | `/console/stream` | 同じブロックを SSE で流す（`event: console.block`） | SSE | 同上 |
| 89 | GET | `/tasks/{id}/runs/{run_id}/events` | その run のイベントだけ（折り畳んだ進行を開いたとき） | `EventsPage` | store `event_rows_for` |
| 90 | GET | `/knowledge/tree` | 知識ベースのツリー（`?scope=` / `?q=`。ADR-0047、Phase 61） | `KnowledgeTree` | `index.json` + `grep` |
| 91 | GET | `/knowledge/page` | ページ 1 枚（raw / html / front matter / 履歴 / etag） | `KnowledgePage` | ファイル + 履歴 |
| 92 | PUT | `/knowledge/page` | ページを 1 件 1 コミットで書く（**管理系**） | 200 `KnowledgePageResult` | ファイル + コミット |
| 93 | GET | `/knowledge/inbox` | `_inbox/` の候補（出典・取り込み先つき） | `KnowledgeInbox` | ファイル |
| 94 | POST | `/knowledge/inbox/{id}/accept` | 候補を正本に取り込む（**管理系**） | 200 `KnowledgePageResult` | ファイル + コミット |
| 95 | POST | `/knowledge/inbox/{id}/reject` | 候補を捨てる（**管理系**） | 200 `KnowledgeRejectResult` | ファイル + コミット |

---

## 3. 各エンドポイント

記法: `→` は成功応答。エラーは §1.5 の共通分に加え、各項に書いたもの。

### 3.1 `GET /health` → 200 `Health`

```json
{"api_version":"1","schema_version":11,"celeris_version":"0.9.0","instance_id":"01J…",
 "started_at":"…","now":"…","db":{"journal_mode":"wal","busy_timeout_ms":5000},
 "release":"a1b2c3d4e5f6","mode":"normal","role":"active"}
```

- `api_version` は `"1"` 固定。互換性を壊す変更は `/api/v2` で行う（ADR-0013 D8）。
- `schema_version` は `schema_migrations` の最大版数（= `task_core::SCHEMA_VERSION`。現在 **14**:
  0001 init / 0002 events id / 0003 tasks の title・updated_at 列 / 0004 tasks の objective 列（ADR-0014 D2）/
  0005 tasks の genre 列（ADR-0027）/ 0006 組織・案件・報告・対話・認可の表と tasks の project_id・
  milestone_id・assignee 列（ADR-0033）/ 0007 messages の task_id 列と reports.project_id の NULL 可（Phase 27）/
  0008 notifications（ADR-0037）/ 0009 notifications の project_id 列 / 0010 projects の workspace 列（ADR-0039）/
  0011 daemon_instances（ADR-0040 D4）/ 0012 project_repos と tasks の repos_json 列（ADR-0043 D1/D2）/
  0013 task_comments と tasks の labels_json・category 列（ADR-0044 D2/D3）/
  0014 task_integrations（ADR-0043 D5））。
- `journal_mode` は `PRAGMA journal_mode` の実測値（`"wal"` でなければ設定不備。GUI は警告を出す）。
- ADR-0040 D3 / D4（Phase 47）: `release` はこのプロセスのリリース（`--release <sha12>` / 環境変数
  `CELERIS_RELEASE` / 既定 `"dev"`）、`mode` は `"normal"` か `"verify"`（`--mode`）、`role` は
  `"active"` / `"standby"` / `"draining"` / `"verify"`。昇格（`promote.sh`）と検証（`verify.sh`）は
  「どの版がどの役割で動いているか」をここだけで判定する。`role` が `standby` / `draining` の間は
  ディスパッチャの状態を要する管理系が 503 `standby`（§1.5）。
- ADR-0041 D5（Phase 51）: `mode == "verify"` のプロセスは、**`genre = "smoke"` のタスクだけ**を
  dispatch する（他の ready は動かさない・リースも奪わない・レビューも拾わない・`daemon_instances` にも
  書かない）。その役割・分野・プロバイダは verify モードが組み込みで足すもので、すべて偽のアダプタ
  （`adapter = "fake"`）。`GET /config` にだけ姿が出る（§3.21）。本番（`normal`）には何の影響も無い。
- 無認証（1.3）。DB のパスは出さない（`GET /config` に出す）。

### 3.2 `GET /inbox` → 200 `Inbox`

計算規則は §5.1。クエリ無し。`attention.unroutable` はデーモンのスナップショット（3.23）から合成する。スナップショットがまだ無ければ空。

- `questions[].approval_id`（Phase 29。GUI 監査対応）: その質問に対応する**未決の** `approvals` の id
  （§3.56）。GUI はこれで `POST /approvals/{id}/decide`（3.57）へ直接リンクできる。行がまだ無い、または
  既に決定済みなら `null`。

### 3.3 `GET /tasks` → 200 `TaskList`

| クエリ | 型 | 既定 | 意味 |
|---|---|---|---|
| `status` | `Status`、複数可（`?status=ready&status=running` または `status=ready,running`） | 全て | |
| `kind` | `TaskKind`、複数可 | 全て | |
| `genre` | 文字列（自由記述）、複数可（`kind` と同じ形） | 全て | `Task.genre` の完全一致（`ListFilter.genres`。ADR-0027 D1）。列挙型の検証はしない |
| `parent` | `TaskId` | — | 直接の子だけ（`ListFilter.parent_id`） |
| `project` | `ProjectId`（ULID） | — | その案件のタスクだけ（`ListFilter.project_id`。ADR-0033 D2）。ULID でなければ 400 |
| `root_only` | bool | false | `parent_id IS NULL` のものだけ。`parent` と AND で効く（同時指定は空になるだけで、エラーではない） |
| `q` | 文字列（最大 200 文字） | — | `title` / `objective` / **コメント本文**の部分一致（`ListFilter.text_contains` + `text_includes_comments`。SQLite の LIKE なので **ASCII の大文字小文字は区別しない**。`%` `_` はリテラル。ADR-0014 D2 / ADR-0044 D4） |
| `label` | 文字列（小文字 `[a-z0-9-]`）、複数可 | 全て | **AND**（指定したラベルを全部持つタスクだけ）。規則に合わない値は 400（ADR-0044 D4） |
| `category` | `feature`/`bug`/`research`/`ops`/`docs`/`other`、複数可 | 全て | OR。知らない値は 400（ADR-0044 D3/D4） |
| `assignee` | 文字列（`org_nodes.id`） | — | 完全一致（`ListFilter.assignee`） |
| `milestone` | `MilestoneId`（ULID） | — | その途中目標のタスクだけ。ULID でなければ 400 |
| `tier` | `frontier`/`standard`/`cheap`、複数可 | 全て | `worker_hint.tier`（`json_extract`）。知らない値は 400 |
| `priority` | `P0`〜`P3` または整数、複数可 | 全て | `Task.priority` の完全一致（ラベルは P0=30 / P1=20 / P2=10 / P3=0 に写す） |
| `order` | `dispatch` / `updated_desc` / `created_desc` | `updated_desc` | `ListOrder` と同じ: `dispatch` = `priority DESC, created_at ASC, id ASC`（`ready_tasks` と同じ）。`updated_desc` = `updated_at DESC, id DESC`。`created_desc` = `created_at DESC, id DESC` |
| `limit` | 1..=500 | 100 | |
| `archived` | bool（`1`/`0`/`true`/`false`） | `false` | `true` なら**アーカイブされた案件のタスクも返す**。既定はそれらを隠す（`ListFilter.hide_archived`。ADR-0044 D6、Phase 55）。案件に属さないタスクは常に見える。`GET /tasks/{id}`（個別）は既定でもそのまま見える |
| `cursor` | 不透明文字列 | — | 前応答の `next_cursor`（`Page<T>.next_cursor` をそのまま）。解読できない cursor は 400 |

- `TaskStore::list_page(&ListFilter, ListOrder, cursor, limit) -> Page<Task>` をそのまま使い、`Task` を `TaskSummary` に写す（`children` / `pending_children` / `backoff_until` の付加は task-ops）。
- `total` は `Page.total`（同じフィルタでの総件数。cursor に依らない）。
- `items[].children` / `pending_children` は `parent_id` で集計（`pending` = 非終端）。`backoff_until` は §5.3。
- `items[].role` は `Task.role`（ADR-0016 D1）。一覧の行に役割のラベルを出すための値で、`TaskDetail.role` と同じ（GUI-R2。役割なしは `null`）。
- `items[].genre` は `Task.genre`（ADR-0027 D1）。`role` と同じ理由で一覧の行に出す値で、`TaskDetail.genre` と同じ（分野なしは `null`）。
- `items[].assignee` は `Task.assignee`（組織のノード id。ADR-0033 D2。担当なしは `null`）、
  `items[].conversation` は**対話用タスクか**（`true` なら人への返事のための run。§3.54〜3.55）。
  どちらも GUI-R3（Phase 27）で足した。仕事の木やタスク一覧から対話用タスクを隠すのに使う
  （API 側の絞り込みは足していない。GUI がこの真偽値で弾く）。
- `items[].support` は**裏方タスクの印**（Phase 29。GUI 監査 H4）: `"conversation"` | `"compaction"` |
  `"approval"` | `"review"` | `null`。決定的な優先順（`task_core::support_kind`）で 1 つだけ付く:
  対話（`conversation.is_some()`）> 圧縮（`role == "report-compressor"`）> 承認（`kind == "approval"`）>
  合成レビュー（`kind == "review"`）。人が見る本体の仕事（`kind == "execute"` かつどれにも当たらない）は
  `null`。GUI はこれで仕事の木から裏方を一括で外せる（`conversation` は互換のため残す。同じ判定の下位互換）。
- `items[].labels` / `items[].category` / `items[].priority_label` / `items[].project_id` /
  `items[].milestone_id` は Phase 53（ADR-0044 D3/D4）で足した。ボードのカードが要るものを一覧に出す。
  `priority_label` は `priority`（`i32`）を P0〜P3 に丸めた文字列（30 以上 = P0、20..30 = P1、
  10..20 = P2、10 未満 = P3）。`priority`（`i32`）は互換のため残す。
- 複数のフィルタを同時に書いたら **AND**（`label` どうしも AND、`category` / `tier` / `priority` /
  `status` / `kind` / `genre` は同じキーの中では OR）。
- `counts_by_status` は**フィルタに関係なく** DB 全体の status 別件数（`count_by_status()` の `Vec<(Status, u64)>` をオブジェクトに。0 件の status は現れない）。タイトルバーの件数表示用。
- 空のときは `{"items":[],"next_cursor":null,"total":0,"counts_by_status":{…}}`。

### 3.4 `POST /tasks` → 201 `Task`（`Location: /api/v1/tasks/{id}`）

要求本文は `task_ops::add::NewTaskSpec`（`celerisctl add` の引数と 1:1。Phase 9b で `Deserialize` + `JsonSchema` + `deny_unknown_fields` と `#[serde(default)]` を付ける。§6.2）:

```json
{"title":"add CLI parsing","objective":"…",
 "acceptance":[{"type":"human","text":"reviewer is happy"},
               {"type":"command","cmd":"cargo test","expect_exit":0},
               {"type":"artifact_exists","name":"bench.json"},
               {"type":"reviewer","text":"the diff is minimal"}],
 "kind":"execute","tier":"standard","adapter":null,"priority":0,
 "parent":null,"depends_on":["01J…"],
 "max_turns":10,"max_wall_secs":600,"max_retries":2,"workspace":null,
 "role":"lead","genre":null,"aggregate":false,
 "project_id":null,"milestone_id":null,"assignee":null,
 "labels":["infra"],"category":"ops","status":"ready"}
```

- `acceptance[]` は `task_ops::add::CriterionSpec`（`Human{text}` / `Command{cmd, expect_exit}` / `ArtifactExists{name}` / `Reviewer{text}`）を `#[serde(tag = "type", rename_all = "snake_case")]` で表したもの。
- 省略可能なフィールドと既定は `celerisctl add` と同じ: `kind=execute`、`priority=0`、`parent=null`、`depends_on=[]`、
  `max_retries=2`、`workspace=null`（→ `Local{path: "<task_id>"}`、相対）、`role=null`、`genre=null`、`aggregate=false`。`title` / `objective` / `acceptance` は必須。
- **`tier` / `max_turns` / `max_wall_secs` / `adapter` は Phase 10 から任意**（ADR-0016 D1 / M3）。省略時は
  **`role` に一致する `[[roles]]` の既定 → `genre` に一致する `[[genres]]` の `default_role` の既定 → 全体の既定**
  （`tier=standard`、`max_turns=10`、`max_wall_secs=600`、`adapter=null`。ADR-0027 D1）の順で埋める。
  書いた値は常に役割の既定より優先する。`max_retries` に役割・分野の既定は無い（常に 2）。
- `role` は自由記述の役割名（ADR-0016 D1）。`[[roles]]` に無い名前でもエラーにせず、名前だけ保存する（既定も指示文も付かない）。
  状態機械は `role` を見ない。`GET /config` の `roles[]` が設定にある役割の一覧。
- **`project_id` / `milestone_id` / `assignee` は Phase 23 から任意**（ADR-0033 D2）。`project_id` はその案件の
  仕事の木にタスクを載せる（`GET /projects/{id}` に出る）。`milestone_id` は `project_id` と同じ案件のもので
  あること（違えば 422）。`assignee` は `GET /org` のノード id で、**省略された `tier` / `adapter` / 予算は
  役割・分野より先にここから埋まる**（ノードの `genre` → その分野の `default_role` → その役割の既定）。
  タスク自身に書いた値の方が常に強い。知らない `assignee` / 無い案件 / 案件違いの途中目標は 422 `validation`。
  3 つとも省略した従来の本文はそのまま通る（互換）。
- `genre` は分野の id（ADR-0027 D1）。**`[[genres]]` が 1 件でも設定されている celeris では常に検証する**
  （celeris は起動時に完全な設定を持つので、`celerisctl add --config` 無しのような「検証しない」緩さは API には無い）。
  `genre` を省略し `role` が指定されていれば、その役割を含む分野がちょうど 1 つだけあるとき、その分野を継ぐ
  （0 件・2 件以上は継がない）。`GET /config` の `genres[]` が設定にある分野の一覧
  （`{id, description, capabilities?, input_artifacts?, output_artifacts?, default_role, roles}`。
  `capabilities` / `input_artifacts` / `output_artifacts` は Phase 18（ADR-0028 D1）の任意の自由記述で、空なら省略される。
  Phase 38: `input_artifacts` / `output_artifacts` の要素は `名前` でも `名前: 説明` でもよい（GUI は `:` の前を名前として扱う））。
- `aggregate`（ADR-0016 D3）: true の親は、委譲した子が全て終端になった後に集約 run を 1 回だけ行い `artifacts/summary.md` を書く。
  応答の `Task` では **false のとき省略される**（`#[serde(skip_serializing_if)]`。`role` / `genre` も `null` のとき省略）。
- `acceptance` は**クライアントが並べた順**で保存する（CLI は accept → cmd → artifact → reviewer の固定順で渡す。並びに意味は無い）。
  `command` の `text` は `` `<cmd>` exits 0 ``（現状の `CriterionSpec::into_criterion` は `expect_exit` に関わらずこの文。CLI も常に `expect_exit = 0`）、`artifact_exists` の `text` は `artifact <name> exists`。整形は task-ops が行う。
- **`labels` / `category` / `priority` のラベル表記 / `status` は Phase 53 から任意**（ADR-0044 D1/D3）:
  - `labels`: 小文字の `[a-z0-9-]`、1〜64 文字、最大 8 個（重複は畳む）。違反は 422 `validation`。
  - `category`: `feature` / `bug` / `research` / `ops` / `docs` / `other`。**既定は `other`**
    （応答の `Task` では既定のとき省略される）。
  - `priority`: `"P1"` のようなラベルでも `20` のような整数でも書ける（P0=30 / P1=20 / P2=10 / P3=0）。
    **省略時は P2（= 10）**。`celerisctl add` は `--priority` の既定 0 を明示して渡すので従来どおり。
  - `status`: `"draft"` か `"ready"` だけ（他は 422）。
- 初期 `status`（ADR-0044 D1 で変わった）: `kind=approval` なら従来どおり `ready`。それ以外は
  **`POST /tasks` では `ready`**（人は Go を出す側なので draft を挟まない）。`status: "draft"` を
  明示したときだけ Go 待ちの `draft` で始まる。`celerisctl add`・計画（`POST /plans`）・委譲・分解の
  子は**従来どおり `draft`**（この規則は API のハンドラが `status` を省略時に `ready` で埋めることで
  実現していて、`task_ops::add` の既定は変わっていない）。
  `Created` イベントと同一トランザクション（`task_ops::add::create_task(store, spec, now) -> Task`）。
- 422 `validation`（`OpsError::Validation` の文言そのまま。`celerisctl add` も同じ関数を通る。検査はこの順。ADR-0014 D3 / ADR-0027 D1）:
  - `title` が空白だけ → `title must not be blank`（`field: "title"`）
  - `objective` が空白だけ → `objective must not be blank`（`field: "objective"`）
  - `acceptance` が空 → `at least one acceptance criterion is required (--accept, --check-cmd, --check-artifact, or --check-reviewer)`（`field: "acceptance"`）
  - `parent` が存在しない → `parent <id> does not exist`（`field: "parent"`）
  - `depends_on[i]` が存在しない → `dependency <id> does not exist`、`failed` / `cancelled` → `dependency <id> has status Failed and cannot be depended on`（`Failed` / `Cancelled` は `{:?}` 表記。`field: "depends_on"`）
  - `genre` が `[[genres]]` に無い（`[[genres]]` が空でない設定に限る）→ `unknown genre: "<genre>"`
  - `genre` と `role` を両方指定し、`role` がその分野の `roles` に無い（`[[genres]]` が空でない設定に限る）→ `role "<role>" is not one of genre "<genre>"'s roles`
- 検証に失敗したら何も挿入しない。
- **`repos` は Phase 52 から任意**（ADR-0043 D2）。そのタスクが使う案件のリポジトリを**名前で**並べる
  （`"repos": ["benchfs", "benchfs-paper"]`。名前は `GET /projects/{id}/repos` の `name`）。
  省略すると **親のタスクの `repos` → 案件の primary** を継ぐ。`repos[0]` がワーカーの
  カレントディレクトリになる。422 `validation` になるのは次の 3 つ:
  - `project_id` を書かずに `repos` を書いた → `repos can only be used on a task that belongs to a project`
  - その案件に無い名前 → `task repo "<name>" is not one of this project's repositories`
  - リモートのリポジトリを他と混ぜた → `a task cannot mix a remote repository with other repositories yet (ADR-0043 D2)`
  応答の `Task.repos[]` は `{"repo_id": "01J…", "name": "benchfs"}` の配列（空なら省略される）。

### 3.5 `GET /tasks/{id}` → 200 `TaskDetail`

- `celerisctl show --json <id>` と**同じ型・同じ直列化**（task-ops の `TaskDetail` を compact な JSON で出す。ADR-0013 D12）。差は次の 3 点（Phase 9b で確定）:
  API は `runs[].files` を埋める（celerisctl は `null`）、`timers.now` は応答時刻、celerisctl は `--config <config.toml>`
  （または `CELERIS_CONFIG`）を渡さない限り `workspace_dir` / `timers.backoff_until` / `timers.max_requeues` / `worktree` を
  設定の既定値で計算する（`--workspace-root` で基準だけ上書き可）。`--config` を渡せば API と同じ値になる。
- `workspace_dir` は `WorkspaceSpec::Local{path}` を `workspace_root` で絶対化した文字列（`canonicalize` はしない。存在しなくてもよい）。
  `Remote{cluster, path}` では**手元の写し** `workspace_root/<task_id>`（run のログ `runs/` と成果物はここ。クラスタ側のパスは `task.workspace.path`。ADR-0018 D1、Phase 12）。
- `cluster` は `WorkspaceSpec::Remote` の `cluster`（`[[clusters]] id`）。`Local` は `null`（Phase 12）。
- `worktree` は `sync = "worktree"` のクラスタで動くタスクだけに出る（ADR-0019 D2）。
  `{project, dir, branch}` = 元のリポジトリ / クラスタ上の worktree のパス（既定 `<project>/.celeris-worktrees/<task_id>`、
  `worktree_root` があればその下）/ ブランチ `celeris/<task_id>`。**celeris は commit しない**ので、変更は worktree の作業ツリーに残る。
  GUI はここを「クラスタで結果を見る場所」として出す（`git -C <dir> diff`、`git -C <dir> commit`、`git worktree remove <dir>` は人の操作）。
- **Phase 49（ADR-0041 D1）**: ローカルの作業場所（`kind = "local"`、`mode = "worktree"` 既定）が git リポジトリの
  タスクにも `worktree` が出る（`{project, dir, branch}` = 元のリポジトリ / `<workspace_root>/<task_id>/tree` /
  `celeris/<task_id>`）。このとき `workspace_dir` は `<workspace_root>/<task_id>`（run のログ `runs/` と成果物は
  作業ツリーの**外**にある）。celeris が用意した目印 `<workspace_dir>/worktree.json` があるタスクだけがこの扱いで、
  worktree を消した後も `runs/` と `artifacts/` は同じ場所から引ける。
  `sync = "rsync"` / `"none"` のクラスタと Local のタスクでは `null`。
  `celerisctl show --json` は `--config <config.toml>`（または `CELERIS_CONFIG`）を渡したときだけ `worktree` を出せる
  （`[[clusters]]` を知らないと worktree のパスが決まらないため）。
- `role` は `task.role` と同じ値を最上位にも出したもの（GUI の表示用。Phase 10、ADR-0016 D1）。役割が無ければ `null`。
- `genre` は `task.genre` と同じ値を最上位にも出したもの（`role` と同じ理由。Phase 16、ADR-0027 D1）。分野が無ければ `null`。
- `delegated[]` は、このタスクの run が `delegate` で作った子の履歴（`Event::Delegated` の出現順。Phase 10、ADR-0016 D2）。
  1 要素は `{run_id, ts, tasks: TaskRef[]}` で、`ts` はイベントの `ts`、`tasks` は子の**現在の**状態（既に存在しない ID は落とす）。
  委譲された子は `children[]` にも出る（`delegated[]` はどの run が作ったかを足すだけ）。
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

`worker_progress` は `{run_id, msg}` に加えて、ADR-0048 D2（Phase 60a）の
`kind`（`tool_use` / `tool_result` / `text` / `thinking` / `status`）/ `tool` / `summary` / `detail`（4 KiB まで）/
`truncated` / `error` を**あれば**持つ（**追加のみ**。付けないワーカー・導入前のイベントには無い）。
Console（§3.98）はこの形だけを見る。

`types` の語彙（`Event` の `type`、17 種）: `created`、`transitioned`、`worker_started`、`worker_progress`、`artifact_produced`、
`worker_finished`、`review_verdict`、`approval_requested`、`approval_decided`、`answered`、`provider_throttled`、
`cluster_unavailable`（Phase 12。`{cluster, host, reason}`）、
`delegated`（Phase 10。`{run_id, task_ids}`。状態は変えないので `replay` は無視する）、
`question_raised`（ADR-0021。`{run_id, text}`。ディスパッチャが人に出した質問。同じトランザクションの
`transitioned{to: "blocked", reason: "child_failed"}` と対。状態は変えないので `replay` は無視する）、
`retried`（Phase 31。`{from}`。`failed`/`cancelled` を複製してやり直した新しいタスクに付く。状態は
変えないので `replay` は無視する。§3.63）、
`edited`（ADR-0044 D1、Phase 53。`{fields}`。人が `PATCH /tasks/{id}` で変えた項目名の一覧）、
`assigned`（ADR-0046 D5、Phase 59。`{node, score, reason}`。`assignee` が無いタスクの担当を matching が
決定的に決めたときに 1 件だけ付く。`node` は決まった担当の id、`score` はタスクの `skills` とその担当の
実効 `skills` の重なりの件数、`reason` は人が読める理由の文。GUI のタスク画面の「なぜこの担当か」はこれを表示する）。

- `seq` 昇順。`has_more` が true なら最後の `seq` を `after_seq` に入れて続きを取る。
- `items[].id` はグローバル id（ADR-0013 D6）。`items[].ts` は `events.ts`。

### 3.7 `GET /tasks/{id}/runs` → 200 `RunList`

§5.2 の規則で events から組み立て、`files` を埋める。`started_at` 昇順。

### 3.8 ファイル系: `GET /tasks/{id}/runs/{run_id}/{stdout|stderr|result|request|prompt}`、`GET /tasks/{id}/artifacts/{idx}`

**パス解決（ユーザ入力のパスは受け取らない。ADR-0013 D11）**

1. `<ws>` = `WorkspaceSpec::Local{path}`（相対なら `workspace_root` 基準）を `canonicalize`。失敗（存在しない）→ 404 `file_not_found`。`Remote` → 404 `file_not_found`（`detail: "remote workspace"`）。
2. run: `run_id` が `^[0-9A-HJKMNP-TV-Z]{26}$` に一致しなければ 403 `path_forbidden`。対象 = `<ws>/runs/<run_id>/{stdout.jsonl|stderr.log|result.json|request.json}`。
   `request.json` は**ワーカーに渡した `RunRequest`**（objective・役割の指示文・クラスタ用の追記・前回の判定・人の回答・子の結果。ADR-0023 D2）。
   `prompt.txt` は claude-code / codex に**実際に渡した文面**（`build_prompt` の結果。ADR-0023 M1。fake アダプタの run には無い）。
   どちらにも秘密は含まれない（`[[providers]].env` の値やトークンは `RunRequest` に入らない）。run のディレクトリごと無ければ 404 `run_not_found`。
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
- 成果物には `X-Celeris-Sha256: <記録値>` と `X-Celeris-Sha256-Current: <現在の値>`（計算は 64 MiB までで、超えるファイルは省略）。
- `X-Celeris-Size: <現在のバイト数>` を常に付ける（追尾用）。
- 範囲: `Range: bytes=a-b` に 206 + `Content-Range` で応える（単一範囲のみ。複数範囲は 416）。または `?offset=N&length=M`（`Range` と併用不可、併用は 400）。`offset == size` は **200 で空本体**（追尾で「新着なし」を表すため）。`offset > size` / `Range` の開始がサイズ超 → 416。
- 本体はストリーミング。サイズ上限は設けない（追尾は `offset` で行う）。

### 3.9 `GET /tasks/{id}/artifacts` → 200 `ArtifactList`

`ArtifactProduced` を出現順に並べ、`idx`、`run_id`、`ts`、`artifact`（`ArtifactRef`）、`exists`、`size`、`sha256_current`、`sha256_matches`（記録値との一致。`exists=false` なら `null`）。パス検査（3.8）に落ちるものは `exists=false, forbidden=true` として一覧には残す（GUI は警告表示、本体は 403）。

### 3.10 `POST /tasks/{id}/approve` → 200 `TransitionResult`

本文 `DecisionBody{note?: string, expected_status?: Status}`（空本体は `{}` と同じ。ただし `Content-Type` は必要）。呼ぶのは `task_ops::gate::approve(store, id, note, expected_status)`。

- 写像は `celerisctl approve` と同じ: `status == draft`（kind 不問）→ `Trigger::Accept`（イベント追加無し）。`kind == approval && status == ready` → `Trigger::Approve` + `Event::ApprovalDecided{by:"human", approved:true, note}` を同一トランザクション。
- それ以外 → 409 `invalid_transition`（`OpsError::InvalidState`: `detail: "task <id> (kind=<kind>, status=<status>) cannot be approved"`、`trigger: "approve"`）。
- `expected_status` があり現在と違う → 409 `conflict`（`OpsError::Conflict`。写像より先に検査される）。
- `by` は `"human"` 固定（H9）。

### 3.11 `POST /tasks/{id}/reject` → 200 `TransitionResult`

本文 `DecisionBody`。`task_ops::gate::reject(store, id, note, expected_status)`: `kind == approval && status == ready` のみ `Trigger::Reject` + `ApprovalDecided{approved:false, note}`。それ以外は 409 `invalid_transition`（`cannot be rejected`）。`draft` の取り消しは `cancel`（ADR-0004 D2）。

### 3.12 `POST /tasks/{id}/answer` → 200 `TransitionResult`

本文 `AnswerBody{answer: string, expected_status?}`。`answer` が空白のみ → 422 `validation`（`answer must not be blank`。task-api が task-ops を呼ぶ前に検査する。CLI は clap が空文字を通すので挙動は CLI と同じにしない）。
`task_ops::gate::answer(store, id, answer, expected_status)`: `status != blocked` → 409 `invalid_transition`（`cannot be answered; only blocked tasks accept an answer`）。
`Trigger::Answer` + `Event::Answered{question, answer}`（`question` は §5.5 の `latest_question`。無ければ空文字列）を同一トランザクション。

- **Phase 29（GUI 監査 H2）**: このタスクの未決の `approvals`（§3.56）があれば、同じ遷移の中で
  `once` + 同じ `answer` の文言で決定済みにする（`approvals` が無ければ何もしない。決定的）。
  `POST /approvals/{id}/decide`（3.57）は既にこの経路（`gate::answer`）に相乗りしているので、
  どちらから答えても `GET /approvals?pending=true` から同じように消える（両方向が揃う）。

### 3.13 `POST /tasks/{id}/cancel` → 200 `TransitionResult`

本文 `CancelBody{expected_status?}`。`task_ops::gate::cancel(store, id, expected_status)`: 終端（`done|failed|cancelled`）→ 409 `invalid_transition`（`cannot be cancelled`）。伝播（Approval の子、`depends_on` の後続）はストアが同一トランザクションで行い、`cascaded` にその id を列挙する（§5.7）。

### 3.14 `POST /plans` → 201 `Task`（`Location`）

本文 `task_ops::plan::NewPlanSpec`（`celerisctl plan` と 1:1。9b で serde / JsonSchema を付ける）: `goal`（必須）、`workspace?`、`tier=frontier`、`priority=0`、`max_turns=30`、`max_wall_secs=900`、`max_retries=1`。
`task_ops::plan::create_plan(store, spec, now) -> Task`。`title` = `goal` の 1 行目の先頭 80 文字（char 境界）。`goal` が空白のみ → 422（`goal must not be blank`、`field: "goal"`）。`kind=plan`、`acceptance=[]`、`status=draft`、`parent_id=null`。

### 3.15 `POST /replay` → 200 `ReplayReport`

本文は空（`{}`）。`task_ops::replay::replay(store) -> ReplayReport{tasks, mismatches}`。`celerisctl replay` と同じ規則（`Created` で初期化、`Transitioned` で上書き、`worker_error|lease_expired|review_fail` で attempts+1）で全タスクを再構築し、`tasks` との差分を返す。**DB は変更しない。** 数万イベントで数秒かかりうるので、`spawn_blocking` で行い、同時実行は 1 つ（2 つ目は 503 `replay_in_progress`、`Retry-After: 5`）。

### 3.16 `GET /graph` → 200 `Graph`

| クエリ | 既定 | 意味 |
|---|---|---|
| `root` | — | このタスクの祖先・子孫（`depends_on` と `parent_id` を両方向にたどる）だけ |
| `depth` | 無制限 | `root` からの最大ホップ数 |
| `include_terminal` | true | false で `done|failed|cancelled` を除く（辺も除く） |

`nodes[] = {id, title, status, kind, parent_id, role}`（`role` は `Task.role`。ノードに役割のラベルを出すための値。役割なしは `null`。GUI-R2）、
`edges[] = {from, to, kind: "depends_on"}`（`from` = 先行、`to` = 後続）。親子は `parent_id` で表し、辺にしない。レイアウトはクライアント。上限 5,000 ノード（超えたら 422 `validation`、`detail` で `root` の指定を促す）。

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

`items[]` は `[[providers]]` の順。定義（`id` / `adapter` / `tiers` / `concurrency` / `model` = 実効モデル / `env_keys` = **キー名だけ**）は設定から、`in_use` と `cooldown` と `last_check` はスナップショット（無ければ `null`）、`stats` は §5.8 の集計。

`last_check`（ADR-0022 D2）は直近の `POST /providers/{id}/check` の結果 `{at, result, detail}`
（`result` は §3.27 と同じ 4 値、`detail` は人が読む一行。ADR-0022 M1）。
**メモリだけに持つ観測値**で、celeris を再起動すると `null` に戻る（イベントにも DB にも残さない）。自動では走らないので、
値が入るのは人が `check` を叩いた後だけ。`reload` でプロバイダ表を差し替えても、同じ `id` の記録は残る。

ADR-0017 M4: `POST /reload` に成功すると、次の tick のスナップショットに乗った一覧（`providers.d/` を含む）を優先して返す。最初の tick が来る前だけ起動時に固定した一覧にフォールバックする。`GET /config` の `providers[]` も同じ規則。

### 3.20 `GET /daemon` → 200 `DaemonView`

```json
{"now":"…","snapshot":{"instance_id":"01J…","pid":1234,"hostname":"lab-01","started_at":"…","last_tick_at":"…","ticks":8812,"tick_ms":2000,
  "in_flight":[{"task_id":"01J…","run_id":"01J…","provider":"claude-a","kind":"worker","since":"…"}],
  "cooldowns":[{"provider":"claude-b","until":"…","reason":"throttled"}],
  "awaiting_human":["01J…"],"awaiting_children":["01J…"],"unroutable":[],
  "providers":[{"id":"claude-a","adapter":"claude-code","tiers":["frontier","standard","cheap"],"concurrency":2,"model":"claude-sonnet-5","in_use":1}]}}
```

- ディスパッチャが tick の最後に `DaemonSnapshot` を `tokio::sync::watch` に送り、API は最新値を読む（I/O 無し。ADR-0013 D4）。DB には書かない。
- 最初の tick より前は `snapshot: null`。
- `awaiting_human[]` は承認待ちで延期中のタスク、`awaiting_children[]` は**委譲した子が終わるのを待っている親**
  （どちらも `reviewing` のままだが理由が違う。ADR-0023 D3）。GUI はこれを見て「判定中」と「部下待ち」を区別する
  （`reviewing` かつ子が非終端、という再計算を GUI 側でしない）。
- cooldown は `ProviderPolicy::cooldowns(now) -> Vec<Cooldown{provider, until: Instant, reason: CooldownReason}>`（既定実装は空。Phase 9a で `StaticPolicy` が実装済み）で取り、`Instant` を壁時計に直す。`reason` の語彙は `throttled | auth_failed | exhausted`（`Spawn` は `provider_failure_outcome` が `Exhausted` に写すので cooldown の理由としては現れない。`ProviderThrottled.reason` には `spawn` も入りうる）。
- `last_tick_at` が `now` から `3 × tick_ms` 以上古ければ GUI は「ディスパッチャが遅延」と表示する（API は判定しない）。
- API に繋がらないこと自体が「celeris 停止」を意味する（GUI 側で表示）。
- **`containers`**（Phase 56、ADR-0043 D3。古いスナップショットには無いので任意）: コンテナ実行の設定と、
  **起動時に 1 度だけ**調べた runtime の能力。

```json
"containers":{"preference":"auto","runtime":"docker","image_default":"celeris-worker:latest",
  "build_dir":"/home/u/.local/celeris/containers",
  "probes":[{"runtime":"podman","detail":"newuidmap: write to uid_map failed: Operation not permitted"},
            {"runtime":"docker","detail":"ok"}]}
```

  - `preference` は `[containers] runtime`（`"auto"` / `"podman"` / `"docker"`）。`"auto"` は podman を先に試す。
  - `runtime` は実際に使うもの。**`null` なら `run = container` のタスクは dispatch されず `blocked`** になる
    （質問「コンテナ runtime が使えません…」）。GUI は設定画面で赤く出す。
  - `probes[]` は試した順の `<runtime> info` の結果（`detail` は成功なら `"ok"`、失敗なら理由の 1 行）。
  - これは**観測値**で、DB には書かないし `replay` の対象でもない（再起動すると調べ直す）。

### 3.21 `GET /config` → 200 `ConfigView`

`config.toml` の要約。`db`（絶対パス）、`workspace_root`、`tick_ms`、`max_concurrency`、`lease_grace_secs`、`idle_timeout_secs`、`kill_grace_secs`、`review_timeout_secs`、`error_cooldown_secs`、`retry_backoff_base_secs`、`retry_backoff_max_secs`、`max_requeues`、`plan.auto_accept`、`reviewer{adapter, tier}`、`providers[]{id, adapter, tiers, concurrency, model, env_keys}`、`clusters[]{id, host, concurrency, sync, delete_on_push, has_setup, env_keys, rsync_excludes}`（Phase 12）、
`roles[]{id, tier, adapter, max_turns, max_wall_secs, has_instructions}`（Phase 10。`[[roles]]` の順）、
`genres[]{id, description, capabilities?, input_artifacts?, output_artifacts?, default_role, roles}`（Phase 16、ADR-0027 D1。
`capabilities` / `input_artifacts` / `output_artifacts` は Phase 18、ADR-0028 D1 — 3 つとも自由記述の任意フィールドで、空なら省略される。
`[[genres]]` の順。`[[genres]]` を書かない設定では `[]`）、
`delegation{max_delegate_per_run, max_tree_depth, max_tree_runs, on_child_failure}`（Phase 10 / ADR-0021。既定 8 / 5 / 100 /
`"retry_then_ask"`。`on_child_failure` は `"retry_then_ask"` か `"ignore"`）、`api{bind, auth_required, allowed_hosts}`、`config_path`。
`[[providers]].env` の**値**、`[adapters.*].env` の値、`[[clusters]].env` の値と `setup` の中身、`[[roles]].instructions` の**本文**（有無だけを `has_instructions` で出す）、`token_file` のパスと内容は出さない。

- ADR-0041 D5（Phase 51）: `GET /health` の `mode` が `"verify"` のプロセスでは、`roles[]` / `genres[]` /
  `providers[]` の末尾に組み込みの **`smoke`**（`adapter = "fake"`、`tier`/`tiers` は `standard`）が 1 つずつ
  増え、`reviewer` が `{adapter: "fake", tier: "standard"}` になる。設定ファイルに同じ id があっても
  上書きされる（煙試験が本物の LLM を呼ぶ経路を設定で開けられないようにするため）。**型は変わらない**し、
  本番（`mode = "normal"`）の応答も変わらない。GUI は本番の API しか見ないので対応は要らない。`task-api` は `celeris` crate に依存しないので、この型は task-api に置き、celeris が起動時に値を作って `ApiState` に渡す。

### 3.22 `GET /schema` → 200 `application/schema+json`

コミット済み `docs/api/v1/api-v1.schema.json` を `include_str!` で返す（開発・型生成の確認用。GUI の型生成はリポジトリのファイルから行い、この応答には依存しない）。

### 3.23 `GET /clusters` → 200 `Clusters`（ADR-0018、Phase 12。トンネルは ADR-0053 D3、Phase 66）

`items[]` は `[[clusters]]` の順。定義（`id` / `host` / `concurrency` / `sync` / `delete_on_push` / `has_setup` = `setup` の有無 / `env_keys` = **キー名だけ** / `rsync_excludes` / `auth`）は設定から、
`in_use`（そのクラスタで走っている run + 判定の数）/ `connected`（この tick の `ssh -O check` の結果 = 多重接続があるか）/ `cooldown_until` / `cooldown_remaining_secs` / `connect_pending` /
`tunnel_forwards` / `tunnel_login_needed` は スナップショットから（無ければ `null` / `false` / `[]`）。`env` の値と `setup` の中身は出さない（ADR-0018 D7）。

- `auth`: `"manual"`（既定）/ `"publickey"` / `"totp"`（ADR-0032 D1）。GUI はこれで「クラスタ」画面の案内を出し分ける
  （3.39〜3.41、§10）。
- `connect_pending`: GUI 発の接続（`POST /clusters/{id}/connect`）が celeris 側で進行中か（ADR-0032 D5）。
  **プロンプト文字列はここには出さない**（`POST /clusters/{id}/connect` の応答にだけ載る。ADR-0024/0025 の
  「URL とコードは action の戻り値にだけ置く」と同じ規律）。
- ディスパッチャは 1 tick に 1 回、設定の全クラスタに `ssh -o BatchMode=yes -O check <host>` を実行する（unix ソケットを見るだけ。ネットワークにも認証にも触れない）。
  接続が戻れば cooldown はその tick で解ける。`auth = "publickey"` のクラスタは、未接続を見つけると cooldown にする前に
  1 回だけ自動で接続を試みる（ADR-0032 D3）。
- GUI は `connected == false` のクラスタに、`auth` に応じた案内を出す（3.39〜3.41）。受信箱の `attention[].cluster_unavailable`（§5.1 (d)）と対。
- `tunnel_forwards[]`（ADR-0053 D3、Phase 66）: `[[clusters.forwards]]`（`listen` / `target`）と、forward
  越しに `GET <listen>/v1/models` が届くか（`up`。観測が無ければ `null`）。`forwards` を持たないクラスタは
  空配列。
- `tunnel_login_needed`（ADR-0053 D3）: `[[clusters.forwards]]` を持つクラスタで、ssh master が落ち、
  **鍵認証を試しても**繋がらなかった状態（人の TOTP 入力が要る）。`GET /clusters` にはこれだけが出る
  （プロンプト文字列やコードは `POST /clusters/{id}/connect`/`connect/code` の応答にだけ載る。§3.39〜3.41
  と同じ経路で接続する）。この状態は Discord にも `cluster_login_needed`（§5.1）で 1 回だけ知らせる。

```json
{"items": [
  {"id": "pegasus", "host": "pegasus", "concurrency": 2, "sync": "rsync", "delete_on_push": false,
   "has_setup": false, "env_keys": [], "rsync_excludes": [], "auth": "totp",
   "in_use": 0, "connected": false, "cooldown_until": null, "cooldown_remaining_secs": null,
   "connect_pending": false,
   "tunnel_forwards": [{"listen": "127.0.0.1:18000", "target": "bnode150:18000", "up": false}],
   "tunnel_login_needed": true}
]}
```

### 3.24〜3.28 プロバイダ管理（ADR-0017、Phase 11。**すべて管理系: `token_file` 未設定でも 401**）

設計は ADR-0017。`config.toml` を直接書き換えず、`providers_include`（例: `providers_include = "providers.d/*.toml"`）が指す
ディレクトリに 1 アカウント 1 ファイル（`providers.d/<id>.toml`。`[[providers]]` の 1 行と同じ形）を読み書きする。
反映（実際の dispatch と `GET /providers`/`GET /config` への表示）は `POST /reload` を呼んだ**次の tick から**で、
実行中の run には影響しない。`providers_include` が設定されていない構成では、この 5 本は全て 409
`providers_admin_unavailable` を返す。

#### 3.24 `POST /providers` → 201 `ProviderConfigView`（`Location: /api/v1/providers/{id}`）

要求本文: `{"id": "acct-b", "adapter": "fake"|"claude-code"|"codex"|"acp"|"paperqa"|"local-deep-research", "tiers"?: [...], "concurrency"?: 1, "model"?: "", "env"?: {...}}`
（`tiers`/`concurrency`/`model` は省略可、`[[providers]]` と同じ既定）。`id` は 1〜64 文字の ASCII 英数字・`-`・`_`
（`providers.d/<id>.toml` のファイル名になるため、パス区切りは拒否）。`adapter` は既知の 6 種類のみ。
`id` が既にあれば 409 `provider_exists`。応答・ログとも `env` は `env_keys`（キー名だけ）で、値は一切出さない。
**`command`/`args`（ADR-0026 D2: `adapter = "acp"` の行だけが持つ実行ファイル／引数の上書き）と `settings`
（ADR-0027 D3: `adapter = "paperqa"` の行だけが持つ PaperQA 設定ファイルの上書き）は本文に含められない**
（含まれていたら値を見る前に 422 `invalid_provider`。実行するコマンド／設定を HTTP から差し替えられないように
するため。`providers.d/<id>.toml` は人が直接編集する。ADR-0026 D7、ADR-0027 D3）。`local-deep-research` の
`[adapters.local_deep_research].settings`（ADR-0029 D1）は行ごとの上書きが無く、そもそも `ProviderConfig` に
対応するフィールドが無いので、この制約の対象外（本文に置けるフィールドは他アダプタと同じ）。

#### 3.25 `PATCH /providers/{id}` → 200 `ProviderConfigView`

要求本文は `{"tiers"?, "concurrency"?, "model"?, "env"?}`（渡したフィールドだけ上書き。`id`/`adapter` は変更不可）。
存在しない `id` は 404 `provider_not_found`。**`command`/`args` は 3.24 と同じく本文に含められない**（422
`invalid_provider`。含まれていたらファイルには一切触れない）。ファイルに人が直接書いた `command`/`args` は、
これらを含まない PATCH では変更されずそのまま残る。

#### 3.26 `DELETE /providers/{id}` → 200 `{}`

`providers.d/<id>.toml` を削除する。存在しない `id` は 404 `provider_not_found`。

#### 3.27 `POST /providers/{id}/check` → 200 `ProviderCheckResponse`

```json
{"result": "ok" | "auth_failed" | "throttled" | "spawn_failed", "checked_at": "…", "detail": "Confirmed ready; …"}
```

`detail`（ADR-0022 M1）は人が読むための一行（ワーカーの返答、または失敗の理由）。`GET /providers` の `last_check.detail` にも同じ値が出る。
**見ているのは「このアカウントで CLI が起動して応答するか」だけ**なので、ワーカープロトコル上のエラー（`Terminal::Error`）は
アカウントの問題ではなく `ok` として扱い、理由を `detail` に入れる。起動できない・認証切れ・枯渇はそれぞれ
`spawn_failed` / `auth_failed` / `throttled`。

そのアカウントの env で短い run（30 秒・1 ターン）を 1 回だけ行い、疎通を確かめる（ADR-0017 D2）。DB には
一切触れない（タスクにもイベント列にも残らない、観測値）。結果は**次の tick のスナップショット**にも載り、
`GET /providers` の `last_check` として読める（ADR-0022 D2。celeris の再起動で消える）。
**自動では走らない**（起動時の一括確認も定期実行もしない。1 回ごとに実際の API 呼び出しを 1 ターン消費するため。ADR-0022 D3）。存在しない `id` は 404 `provider_not_found`。設定の
再読込自体が失敗した（`providers.d/` の壊れた TOML 等）場合は 400。**celeris（`task-worker` に依存する側）が実行し、
task-api 自身はワーカーを起動しない**（DESIGN §5.10 の境界。ADR-0017 M2）。

#### 3.28 `POST /reload` → 200 `ReloadResult`（`{"reloaded": true}`）

`config.toml` と `providers_include` の指すディレクトリを読み直す。設定の検証に失敗したら 400 を返し、
稼働中の状態には触れない（古い設定のまま動き続ける）。実行中の run はそれぞれ差し替え前のアダプタ・役割・
分野の写しを既に掴んでいるので、reload の影響を受けない。**反映は次に起動する run / 次 tick から**。

反映されるもの（Phase 44、実機 2026-09-18 の前は役割・分野・委譲設定・`[reports]`/`[notify]`/`[conversation]`
が対象外で、`max_turns` を変えても委譲された子が古い値のまま動いていた）:

| 設定 | 反映先 | いつから効くか |
| --- | --- | --- |
| `[[providers]]` / `providers.d/` | `StaticPolicy`・アダプタ一式・`GET /providers`/`GET /config` の一覧 | 次 tick の dispatch から |
| `[[roles]]` / `[[genres]]` / `[delegation]` | `Dispatcher` の役割・分野・委譲設定（`RunContext`、委譲される子の budget） | 次に起動する run から（実行中の run は古い写しのまま） |
| `[reports]` | 報告の圧縮の閾値 | 次 tick から |
| `[notify]` | 通知の間隔・webhook の秘密 id・GUI base URL | 次 tick から |
| `[conversation]` | 対話が常に走る分野 | 次に始まる対話から |

**cooldown はメモリ上（`StaticPolicy` の内部状態）なので reload で消える**（ADR-0017 D1）。

再起動が要るもの（reload では触れない。変更しても黙って古いまま動き続ける）: `db` / `workspace_root` /
`[api]` / `[[clusters]]`。**`[accounts]` だけは例外的にこの reload 自体を 400 で拒否する**（ADR-0024。S7）:
`claude_dir` / `max_runs_per_account` / `check_model` のどれかが読み直した設定で変わっていれば、`detail` に
再起動が必要な旨を書いて拒否する（それ以外のフィールドの変更は反映されない）。

### 3.29〜3.35 アカウントのプール（claude-code / codex。ADR-0024・ADR-0025、Phase 13/14）

設計は ADR-0024（claude-code）と ADR-0025（codex を追加）。`[accounts] claude_dir` / `codex_dir` の下の 1
ディレクトリ（`<claude_dir>/<id>/` または `<codex_dir>/<id>/`）が 1 アカウントで、アカウントは `(adapter, id)`
で識別する（**id が同じでもアダプタが違えば別のアカウント**）。`account_pool = true` のプロバイダ
（claude-code か codex）の run は、残量（claude-code は stream-json の `rate_limit_event`、codex は
`token_count` の `rate_limits` が出す `five_hour` / `seven_day` の `utilization`）から選んだアカウントの環境変数
（claude-code は `CLAUDE_SECURESTORAGE_CONFIG_DIR`、codex は `CODEX_HOME`）で起動する（選び方は ADR-0024 D3。
アダプタが違っても同じ計算式を使う）。`[accounts]` はどちらか一方の根ディレクトリだけでもよい。
`[accounts]` が無い構成では、`GET /accounts` は `{"root": null, "roots": {}, "items": []}`、管理系は 409
`accounts_unavailable`。**3.30 以降はすべて管理系**（`token_file` 未設定でも 401。ADR-0017 M3）。`id` の規則は
プロバイダと同じ（1〜64 文字の ASCII 英数字・`-`・`_`）。3.30〜3.35 は `?adapter=` クエリを受け取る
（省略時 `"claude-code"`。未知の値は 400 `bad_request`）。

#### 3.29 `GET /accounts` → 200 `AccountList`

```json
{"root": "/home/u/celeris/claude-accounts",
 "roots": {"claude-code": "/home/u/celeris/claude-accounts", "codex": "/home/u/celeris/codex-accounts"},
 "max_runs_per_account": 2,
 "items": [
   {"adapter": "claude-code", "id": "a", "dir": "/home/u/celeris/claude-accounts/a", "logged_in": true, "in_use": 1,
     "usage": {"five_hour": {"utilization": 0.14, "resets_at": "…"}, "seven_day": {"utilization": 0.24, "resets_at": "…"},
               "status": "allowed", "observed_at": "…", "source": "run"},
     "score": 0.81, "excluded_reason": null, "cooldown": null,
     "last_check": {"at": "…", "result": "ok", "detail": "ok"}, "login_pending": false,
     "stats": {"runs": 12, "done": 10, "error": 1, "input_tokens": 1234, "output_tokens": 567}},
   {"adapter": "codex", "id": "c", "dir": "/home/u/celeris/codex-accounts/c", "logged_in": false, "in_use": 0,
     "usage": null, "score": null, "excluded_reason": "not_logged_in", "cooldown": null,
     "last_check": null, "login_pending": false,
     "stats": {"runs": 0, "done": 0, "error": 0, "input_tokens": 0, "output_tokens": 0}}]}
```

- `root` は `roots["claude-code"]` の別名として残す（後方互換。ADR-0025 D6）。`roots` はアダプタ →
  設定されていればその絶対パス・無ければ `null`。
- `items[]` は `adapter` → `id` の順（`"claude-code"` が先）。`logged_in` はログイン済みを示すファイル
  （claude-code は `.credentials.json`、codex は `auth.json`）の有無（中身は読まない）。
- `usage` / `score` / `excluded_reason` / `cooldown` / `last_check` / `login_pending` / `in_use` はスナップショットの観測値（最初の tick 前は `null` / `false` / 0）。
  `usage` と `cooldown` はそのアダプタの根ディレクトリの `.celeris-usage.json` にも保存され、再起動後も残る（`last_check` も同じ）。
- `excluded_reason`（選ばれない理由）: `not_logged_in` / `at_capacity` / `cooldown` / `five_hour_exhausted` / `seven_day_exhausted` / `rejected`。選べるなら `null` で `score` が入る。
- `cooldown.reason`: `auth_failed`（再ログインが必要）/ `throttled` / `exhausted`。
- `stats` は `WorkerStarted.account` と対応する `WorkerFinished` から、同じ `WorkerStarted.adapter` のものだけ集計する（§5.8 のプロバイダ集計と同じ規則。同じ id でもアダプタが違えば別集計）。
- 時刻はすべて RFC 3339。

#### 3.30 `POST /accounts` → 201 `AccountView`

要求本文 `{"id": "b", "adapter": "codex"}`（`adapter` 省略時 `"claude-code"`）。`<root>/<id>/` を 0700 で作る
（`root` はそのアダプタの根ディレクトリ。設定されていなければ 409 `accounts_unavailable`）。既にあれば 409
`account_exists`。作っただけでは `logged_in: false`（3.33 でログインする）。

#### 3.31 `DELETE /accounts/{id}` → 200 `{}`

`?adapter=`（省略時 `claude-code`）。ディレクトリを `<root>/.removed/<id>-<unix秒>/` に移す（認証ファイルは消さない。人が後で片付ける）。
`in_use > 0` なら 409 `account_in_use`、無ければ 404 `account_not_found`。進行中のログインは止める。
**実装は celeris 側で行う**（`AdminRequest::AccountRemove` 経由。`in_use` はディスパッチャの権威ある値
`account_in_use`（running/reviewing を直接見る。`(adapter, id)` で判定）で判定するので、task-api のスナップショット経由のレースが無い。
`admin_tx` が無い構成は 409 `accounts_unavailable`）。応答の形・ステータスコードは変わらない。

#### 3.32 `POST /accounts/{id}/check` → 200 `AccountCheckResponse`

`?adapter=`（省略時 `claude-code`）。

```json
{"result": "ok" | "auth_failed" | "throttled" | "spawn_failed", "checked_at": "…", "detail": "ok",
 "usage": {"five_hour": {…}, "seven_day": {…}, "status": "allowed", "observed_at": "…", "source": "check"}}
```

claude-code はそのアカウントで `claude -p … --max-turns 1 --model <[accounts].check_model>` を、codex は
`codex exec --json --skip-git-repo-check "Reply with exactly: ok"`（モデルは指定しない）を、それぞれ 1 回だけ
（60 秒まで）実行し、観測値（`rate_limit_event` / `token_count`）を記録する。codex は出力に 401 の行が出たら
再試行を待たずに打ち切って `auth_failed` にする（未ログインだと 10 回ほど再試行して約 40 秒かかるため）。
**自動では走らない**（ADR-0022 D3 と同じ理由）。celeris 側で実行する（task-api はプロセスを起動しない）。

#### 3.33 `POST /accounts/{id}/login` → 200 `AccountLoginStart`

`?adapter=`（省略時 `claude-code`）。アダプタで流儀が違う（ADR-0025 D5）:

- **claude-code**（`kind: "paste_code"`）: `{"kind": "paste_code", "url": "https://claude.com/cai/oauth/authorize?…", "expires_at": "…"}`。
  celeris が `claude auth login` を `CLAUDE_SECURESTORAGE_CONFIG_DIR=<dir>` で起動し、出力から認可 URL を取り出して返す
  （15 秒以内に出なければ 502 `login_failed`）。人はこの URL を自分のブラウザで開いて認可し、表示されたコードを
  3.34 で渡す。10 分で打ち切る。
- **codex**（`kind: "device_code"`）: `{"kind": "device_code", "url": "https://auth.openai.com/codex/device", "user_code": "ABCD-EFGHI", "expires_at": "…"}`。
  celeris が `codex login --device-auth` を `CODEX_HOME=<dir>` で起動し（標準入力は使わない）、URL と一回限りの
  コードを取り出して返す。人はこの URL を別のデバイスで開いて `user_code` を入力する（**GUI には貼り戻さない**）。
  子プロセスの終了を待って（最大 15 分）完了を判定する: `auth.json` ができていれば `logged_in: true` になる
  （GUI は tick ごとの再検証で気づく）。**`login/code`（3.34）は使わない**。

どちらも既に進行中のログインがあれば古い方を止めてやり直す。**`user_code`・URL はログに出さない**（3.34 も同様）。

#### 3.34 `POST /accounts/{id}/login/code` → 200 `AccountLoginResult`

claude-code のみ。要求本文 `{"code": "…"}`。`{"result": "ok" | "failed", "detail": "…"}`。コードを `claude auth login` の標準入力に渡し、終了を 30 秒まで待つ。
exit 0 かつ `.credentials.json` ができたら `ok`。進行中のログインが無ければ 409 `login_not_started`。**コードはログにも応答にも出さない**。
`?adapter=codex` は 409 `login_code_not_supported`（codex はデバイス認証だけで完結する。ADR-0025 D5）。

#### 3.35 `DELETE /accounts/{id}/login` → 200 `{}`

`?adapter=`（省略時 `claude-code`）。進行中のログインを止める（無ければ何もしない）。

#### プロバイダ管理への追加（3.19 / 3.24 / 3.25）

`ProviderView` / `ProviderConfigView` / `POST /providers` / `PATCH /providers/{id}` の本文に `account_pool: bool`（既定 `false`）を追加。
`true` は `adapter` が `"claude-code"` か `"codex"` で、かつ `[accounts]` にそのアダプタの根ディレクトリが設定されているときだけ有効
（ADR-0025 D1）。それ以外の `adapter` はもちろん、
**対応する根ディレクトリが設定されていない構成（`GET /accounts` の `roots["<adapter>"]: null`）で `account_pool = true` を
`POST`/`PATCH /providers` に渡した時点で** 422 `invalid_provider` を返す（S1。`create`/`patch` の時点で拒否する。
reload を待たない）。
プール経由の run の失敗は、原因がアカウント側（throttled/auth_failed/exhausted）ならアカウントを cooldown に
しプロバイダは cooldown にしない。**Spawn 失敗（起動できない）はアカウントの責任ではないので、通常どおり
プロバイダを cooldown にする**（ADR-0024 D4、S10）。

### 3.36〜3.38 秘密（API キー等）の管理（ADR-0030、Phase 20。**すべて管理系: `token_file` 未設定でも 401**）

設計は ADR-0030。ADR-0017 の「API キーを GUI から入力して保存しない」を上書きする（人間の依頼）。秘密は celeris が
`[secrets] dir` の下にファイルで持つ（1 秘密 = 1 ファイル、ファイル名 = id、中身 = 値 1 行・0600）。**値を返す
API は無い**（作成・削除だけ）。`[secrets]` が未設定なら 3 本とも 409 `secrets_unavailable`。`id` の規則はプロバイダ
と同じ（1〜64 文字の ASCII 英数字・`-`・`_`。`PUT`/`DELETE` の id はパスにあるので、無効な形は
`PATCH`/`DELETE /providers/{id}` と同じ規約で 404 `secret_not_found` にする）。

**使い方（`env_from_secrets`）**: `[adapters.<種別>]` と `[[providers]]` の行の両方に、環境変数名 → 秘密 id の
対応 `env_from_secrets = { LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY = "tavily" }` を書ける。値は `build_adapters`
（アダプタを組み立てるとき）に読むので、鍵を入れ替えたら `POST /reload` が要る（GUI は保存後に自動で呼ぶ）。
優先順は celeris の環境 < `[adapters.*].env` < `[adapters.*].env_from_secrets` < 行の `env` < 行の
`env_from_secrets`。**秘密が見つからないのは設定エラーにしない**（`warn!` を出してその環境変数を渡さず、その層
は下の層の値に道を譲る。run はワーカー自身のエラー（鍵が無い）で失敗する）。`[[providers]].env` に平文で書く
従来の方法も残る（既存設定を壊さない）。

#### 3.36 `GET /secrets` → 200 `SecretList`

```json
{"dir": "/home/u/celeris/secrets",
 "items": [
   {"id": "tavily", "updated_at": "2026-09-17T01:00:00Z", "fingerprint": "a1b2c3d4",
    "used_by": [
      {"scope": "adapter", "name": "local-deep-research", "env": "LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY"},
      {"scope": "provider", "name": "ldr-tavily", "env": "LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY"}
    ]}]}
```

**値は返さない**。`fingerprint` は値の sha256 の先頭 8 桁（16 進。人が「入れ替えた鍵が届いているか」を確かめる
ためで、値そのものは復元できない）。`updated_at` はファイルの mtime。`used_by` は**稼働中の設定から導く**
（`GET /secrets` の呼び出しのたびに `[adapters.*].env_from_secrets` と `[[providers]].env_from_secrets` を
走査する。GUI は再計算しない）。**設定が参照している id は、まだ鍵を入れていなくても `items[]` に載る**
（`updated_at` と `fingerprint` が `null`。GUI が「鍵を入れる場所」を一覧に出せるようにするため）。したがって
`items[]` = ファイルとして存在する秘密 ∪ どこかの `env_from_secrets` が指している id。ファイルがある id を
先に id 昇順、続けて未設定の id を id 昇順で並べる。

#### 3.37 `PUT /secrets/{id}` → 200 `SecretPutResult`

要求本文 `{"value": "tvly-abc123..."}`。作成／置き換え（既にあれば上書き）、ファイルを 0600 で
atomic（一時ファイル→rename）に書く。空文字・空白だけの値は 422 `validation`。応答は
`{"id", "updated_at", "fingerprint"}`（**値は含まない**）。無効な id（パス区切りを含む等）は 404
`secret_not_found`（`PATCH`/`DELETE /providers/{id}` と同じ規約。本文を見る前に判定する）。

#### 3.38 `DELETE /secrets/{id}` → 200 `{}`

ファイルを削除する。無ければ 404 `secret_not_found`。

**ログ・応答のどこにも値は出ない**（`who = "admin"`, `op = "secret_put" | "secret_delete"`, `secret_id` だけ記録
する。ADR-0024 D5 と同じ規律）。

### 3.39〜3.41 クラスタへの接続を GUI から張る（ADR-0032、Phase 22。**すべて管理系: `token_file` 未設定でも 401**）

設計は ADR-0032。ADR-0018 D2/D7 の「celeris から対話的な認証は絶対に行わない・接続を張るのは人力」を、
`[[clusters]].auth` で opt-in する形に上書きする（既定 `"manual"` は従来どおり celeris が接続を張らない）。
実際の ssh の起動・`SSH_ASKPASS` を使った検証コードの中継は celeris 側（`AdminRequest::ClusterConnect*`）が行い、
task-api 自身は ssh を起動しない（DESIGN §5.10 の境界）。未知の `id`（`[[clusters]]` に無い）はどの操作も
404 `cluster_not_found`。

#### 3.39 `POST /clusters/{id}/connect` → 200 `ClusterConnectStart`

```json
{"kind": "connected", "prompt": null, "expires_at": null}
```
```json
{"kind": "needs_code", "prompt": "(rmaeda@130.158.241.2) Verification code: ", "expires_at": "2026-09-17T01:35:00Z"}
```

- `kind = "connected"`: コード不要で張れた（`auth = "publickey"` で鍵だけの接続が通った、または既に
  多重接続があった。ADR-0032 D2: 既にあれば新しく張らずに成功を返す）。
- `kind = "needs_code"`: ssh がプロンプトを出した（`auth = "totp"`）。`prompt` は ssh が実際に出した文字列を
  そのまま返す（ユーザ名・ホスト名を含みうる）。人はこれを見て検証コードを入力し、3.40 に渡す。
  `expires_at`（既定 300 秒後）を過ぎたセッションは celeris が自動で片付ける。
- `auth = "manual"` のクラスタへの `connect` は 409 `cluster_connect_not_supported`
  （人が `scripts/cluster-login.sh` で張る運用のまま。ADR-0032 D7）。
- 接続そのものの失敗（ssh の失敗、タイムアウト）は 502 `cluster_connect_failed`（`detail` に一行の手がかり）。
- **プロンプト文字列はログに出さない**（ユーザ名・ホスト名が入るため。ADR-0032 D4）。`GET /clusters` にも出さない
  （3.23）。

#### 3.40 `POST /clusters/{id}/connect/code` → 200 `ClusterConnectResult`

要求本文 `{"code": "123456"}`。`{"ok": true, "detail": null}`。

- コードは `trim` して空、または制御文字を含めば ssh に渡さず 422 `validation`（`claude_account.rs::submit_code`
  と同じ注入防止。長さや文字種は制限しない）。この場合 `admin_tx` には何も送らない。
- 進行中のセッションが無ければ 409 `cluster_connect_not_started`。
- **コードが間違っていた（ssh が接続できなかった）場合は 422 ではなく 200 `{"ok": false, "detail": "…"}`**。
  422 は「celeris がコードを ssh に渡すことすら拒んだ」ときだけで、ssh の認証結果は `ok` で伝える
  （ADR-0032 D5。GUI は `ok: false` を握りつぶさずに画面へ出す）。`detail` に**コードは含まれない**。
- **コードは受け取ってもログにも応答にも出さない**（`who = "admin"`, `op = "cluster_connect_code"`, `cluster`
  だけ記録する。ADR-0032 D4）。

#### 3.41 `DELETE /clusters/{id}/connect` → 200 `{}`

進行中の接続セッションを取り消す（ssh の子プロセスをプロセスグループごと落とす）、または既に張った接続を切る
（`ssh -O exit <host>` を `BatchMode=yes` で呼ぶ）。無ければ何もしない。

### 3.42〜3.49 組織・案件・途中目標（ADR-0033 D1/D2、Phase 23）

SPEC §3.2〜§3.3 の「組織（一つ、役割の木）」と「案件・仕事の木」を第一級のエンティティにしたもの。
既存の `tasks` は実行基盤として残り、案件の仕事の木は `tasks WHERE project_id = ?`（DAG は従来どおり
`parent_id` / `depends_on`）。**組織の編集（3.43〜3.45）と案件の作成（3.46 の `POST`）が管理系**
（`token_file` 未設定でも 401。案件を作ると秘書の run が起きるので、`POST /org/{id}/messages` と同じ規律。
Phase 27 の監査 M-4）。読み取りと途中目標の操作は通常の要求（トークンを設定した celeris では、他の全要求と
同じくトークンが要る）。

#### 3.42 `GET /org` → 200 `OrgList`

```json
{"items":[{"id":"secretary","parent_id":null,"name":"秘書","kind":"secretary","brief":"…",
           "position":0,"created_at":"…","updated_at":"…"},
          {"id":"research-survey","parent_id":"research","name":"関連研究調査課","kind":"section",
           "genre":"literature","brief":"…","position":6,"created_at":"…","updated_at":"…"}]}
```

- 並びは `position` 昇順、同値なら `id` 昇順。**木は GUI が `parent_id` で組む**（API は入れ子にしない）。
- `kind` は `secretary`（根。1 つだけ）/ `department`（部）/ `section`（課）。
- `genre` は `[[genres]] id`（無ければ項目ごと出ない）。その「人」が仕事に使うハーネスの束（ADR-0027/0028）。
  書き込み（3.43 / 3.44）では**設定の `[[genres]]` にある id だけ**を受ける（無い id は 422 `validation`。
  分野を 1 つも設定していない celeris では検証しない。Phase 27 の監査 L-1）。
- 初期の形は `org_include` が指すファイル（`config/org.example.toml`）から、**DB の `org_nodes` が空のときだけ**
  蒔かれる。以後は DB が正で、設定を書き換えても反映されない（ADR-0033 D1）。

**Phase 59（ADR-0046 D1）**: 各ノードは `profile` を持ち、**子は親を継ぐ**。応答には
そのノード自身の `profile`（空なら項目ごと出ない）と、**継いだ後**の `effective_profiles[]` の両方が入る。

```json
{"items":[{"id":"cos","parent_id":null,"name":"Chief of Staff","kind":"secretary",
           "profile":{"harnesses":{"allowed":["conversation","plan"],"default":"conversation"},
                      "knowledge":[{"kind":"kb","scope":"user"}],
                      "policy":["人に返す文は、人が数十秒で読める分量にする。"]},
           "position":0,"created_at":"…","updated_at":"…"},
          {"id":"software-engineering","parent_id":"engineering","name":"Software Engineering","kind":"section",
           "profile":{"skills":["rust","sqlite"],"tools":["gh"],"harnesses":{"default":"coding"}},
           "position":0,"created_at":"…","updated_at":"…"}],
 "effective_profiles":[{"node_id":"cos","chain":["cos"],"harnesses_allowed":["conversation","plan"],
                        "harness_default":"conversation","policy":["…"]},
                       {"node_id":"software-engineering","chain":["cos","engineering","software-engineering"],
                        "skills":["software","rust","sqlite"],"harnesses_allowed":["coding"],
                        "harness_default":"coding","tools":["gh"],"policy":["…","…"]}]}
```

- `effective_profiles[]` は `items[]` と**同じ並び**で、`node_id` で対応づく。`chain` は根から葉までの
  ノード id（GUI の「どこから継いだか」）。
- 継ぎ方（ADR-0046 D1。GUI はこれを再実装しない。表示は `effective_profiles` をそのまま使う）:
  `skills` / `knowledge` / `tools` / `harnesses.allowed` / `permissions.approvals` は**親と和**（根→葉の順、
  重複は落ちる）、`deny_tools` は和だが**常に勝つ**（実効の `tools` から引かれる）、
  `run` / `model.tier` / `harnesses.default` / `review.*` は**子が勝つ**、`model.allowed_tiers` は**交わり**
  （空の親は制限なし）、`policy` は根→葉の順に**連結**。
- `profile` の項目はすべて任意。空の `profile` は応答に出ない（Phase 58 までのノードと 1 バイトも変わらない）。

#### 3.43 `POST /org` → 201 `OrgNode`（`Location: /api/v1/org/{id}`）（**管理系**）

要求本文 `{"id":"coding-poc","name":"PoC・R&D 課","kind":"section","parent_id":"coding","genre":"coding","brief":"…","position":4}`。
`brief` と `position` は省略可（既定 `""` / `0`）。

- `id` は英小文字ケバブ（`[a-z0-9-]`、1〜64 文字、先頭末尾は `-` でない）。
- 既にある `id` は 409 `org_node_exists`（更新は 3.44）。
- 検証に落ちたら 422 `validation`: 秘書が 2 人、秘書に親がある、秘書以外に親が無い、親が存在しない、
  自分を祖先にする、種類の順序違反（`secretary` > `department` > `section`）。

**Phase 59（ADR-0046 D1）**: 任意で `profile` を受ける（省略時は空）。`profile` の検証に落ちたら
422 `validation`（`errors[0].field = "profile"`）:

- `tools` / `deny_tools` の語彙は `gh` / `tavily` / `exa` / `docker` / `cluster:<id>` だけ（それ以外は 422）。
- `harnesses.allowed[]` / `harnesses.default` / `review.harness` は**設定にあるハーネス id**か組み込み
  （`conversation` / `plan` / `reviewer` / `smoke`）だけ（ハーネスを 1 つも設定していない celeris では検証しない）。
- `skills[]` は小文字の `[a-z0-9._-]`、1〜64 文字。
- `run` は `"host"` / `"container"`、`model.tier` と `model.allowed_tiers[]` と `review.tier` は
  `"frontier"` / `"standard"` / `"cheap"` だけ（知らない綴りは本文の解析で落ちて 400 `bad_request`）。

#### 3.44 `PATCH /org/{id}` → 200 `OrgNode`（**管理系**）

`{"name":…, "kind":…, "parent_id":…, "genre":…, "brief":…, "position":…}` のうち**書いた項目だけ**を変える。
`genre` は設定の `[[genres]]` にある id だけ（無い id は 422 `validation`）。
`"genre": null` と書けば分野を外せる（書かなければ今の値のまま）。無い id は 404 `org_node_not_found`。
検証は 3.43 と同じ（付け替えで木が壊れるなら 422）。

**Phase 59（ADR-0046 D1）**: `profile` は**丸ごと差し替え**（部分更新はしない）。書かなければ今の値のまま、
`{}` を書けば空になる。検証は 3.43 と同じ。実効 profile（継いだ後）は `GET /org` の
`effective_profiles[]` で読む（`PATCH` の応答は**そのノード自身の** `profile` だけを返す）。

#### 3.45 `DELETE /org/{id}` → 204（**管理系**）

- そのノードを `assignee` に持つ**未終了のタスク**があれば 409 `org_node_in_use`（SPEC の「消すときに仕事を
  抱えていたら」。ADR-0033 D1）。
- 子ノードが残っていても 409 `org_node_in_use`（木を宙ぶらりんにしない）。
- 無い id は 404 `org_node_not_found`。

#### 3.46 `GET /projects` → 200 `ProjectList` / `POST /projects` → 201 `Project`（**`POST` は管理系**）

要求本文 `{"title":"Pluvio の新テーマ","request":"Pluvio を基盤に用いた新たな研究テーマの模索、検証"}`。
作られた案件は必ず `status = "proposed"`（秘書が理解確認・方針・最初の途中目標を返すまで人の返事待ち。
SPEC §7 / ADR-0033 D2）。空白だけの `title` / `request` は 422 `validation`。
一覧は `created_at` の降順。

**Phase 55（ADR-0044 D6）**: `GET /projects` は**アーカイブされた案件を既定で隠す**。`?archived=1`
（`1`/`0`/`true`/`false`）で全部返す。`GET /projects/{id}`（個別）は既定でもそのまま見える。
`PATCH /projects/{id}`（3.48）と `POST /projects/{id}/milestones`（3.49）、`PATCH /milestones/{id}`（3.49）は
Phase 55 から**管理系**（§1.3）。また **`PATCH` では `paused` / `cancelled` を入れられない**
（422 `validation`、`errors[0].field = "status"`）: `paused_from`（`resume` の戻り先）が空のままになり、
中止の連鎖も起きないので、§3.84〜3.91 の専用のエンドポイントを使う。

**Phase 43（ADR-0039 D1）**: 任意で `workspace`（**案件の作業場所** = コードのある場所）を受ける:

```json
{"title":"Pluvio の PoC","request":"…",
 "workspace":{"kind":"local","path":"~/workspace/rust/pluvio-poc"}}
{"title":"benchfs の検証","request":"…",
 "workspace":{"kind":"remote","cluster":"pegasus","path":"/work/NBB/rmaeda/workspace/rust/benchfs"}}
```

- `kind` は `local`（手元の普段のパス。SPEC §2.1）か `remote`（クラスタ側の作業ディレクトリ。ADR-0018 D1。
  celeris は写しを持ち、`sync` の設定で往復する）。
- `local` の `~` / `~/…` は celeris の `$HOME` で**展開して保存する**（`GET` は展開後の絶対パスを返す）。
  `remote` の `path` はクラスタ側なので展開しない。
- `remote` の `cluster` が `[[clusters]]`（`GET /clusters`）に無ければ 422 `validation`
  （`errors[].field = "workspace.cluster"`）。
- 省略すれば従来どおり「作業場所なし」（`Project.workspace` は応答に出ない）。
- この作業場所は**分解（`POST /projects/{id}/plan`）とその子タスク**が継ぐ（明示 > 案件 > 親。ADR-0039 D2）。
  コードを扱う案件では、GUI から必ず入れてもらうのがよい（入れないと子タスクは空の作業ディレクトリに置かれ、
  ワーカーが自分で `ssh` してリポジトリを探しに行く。実機の事故 2026-09-18）。

**Phase 52（ADR-0043 D1）**: 案件は**リポジトリを複数持てる**ようになった（3.68〜3.71）。
`POST /projects {workspace}` と `PATCH /projects {workspace}` は従来どおり使え、**primary のリポジトリを
作る／書き換える**（`"workspace": null` は primary を消す。未終端のタスクが使っていれば 409 `repo_in_use`）。
`Project.workspace` は primary の `location` の写しなので、従来の GUI はそのまま動く。
`GET /projects/{id}` の応答には `repos[]`（primary が先頭）が増えた。

**Phase 49（ADR-0041 D1）**: `kind = "local"` は任意で `mode` を持てる（`"worktree"` | `"shared"`、**既定
`"worktree"`**）。知らない値は 400 `bad_request`（本文の解析で落ちる）。`remote` にこのキーは無い。

```json
{"workspace":{"kind":"local","path":"~/workspace/agent-platform","mode":"worktree"}}
```

- `"worktree"`（既定）: `path` が git リポジトリなら、celeris は**タスクごとに `git worktree` を切る**。
  ワーカーのカレントディレクトリは `<workspace_root>/<task_id>/tree`、ブランチは `celeris/<task_id>`
  （接頭辞は `[workspace] worktree_branch_prefix`）、base は `main`（無ければ `HEAD`。本番の `current`
  リリースが `main` の子孫ならその sha）。run のログ（`runs/`）と成果物（`artifacts/`）は**作業ツリーの外**の
  `<workspace_root>/<task_id>/` に置かれ、ファイル系エンドポイント（§3.7〜§3.9）と `workspace_dir` も
  そちらを指す。celeris は**コミットしない**し、ブランチも消さない。run が終端に達したとき
  `git status --porcelain` が空なら worktree だけ消す（空でなければ残し、`WorkerProgress`
  「未コミットの変更が残っています: `<dir>`」を 1 行積む）。
- `"shared"`: 従来どおり `path` をそのまま作業ディレクトリにする（celeris 専用の使い捨てリポジトリ向け）。
- `path` が git リポジトリでなければ `"worktree"` でも従来どおり（`"shared"` と同じ）。
- 省略したものは応答の JSON にも出ない（Phase 48 までと同じ本文）。

#### 3.47 `GET /projects/{id}` → 200 `ProjectDetail`

```json
{"project":{…Project…},
 "milestones":[{"id":"01J…","project_id":"01J…","seq":1,"title":"関連研究を棚卸し","description":"",
                "status":"in_progress","created_at":"…","updated_at":"…",
                "review":{"message_id":"01J…","text":"候補を 3 本に絞りました。次は…","at":"…"},
                "proposal":{"id":"01J…","seq":2,"title":"候補の比較実験","status":"proposed", …}}],
 "tasks":[{"id":"01J…","title":"調べる","status":"ready","parent_id":null,"depends_on":[],
           "assignee":"research-survey","milestone_id":"01J…","conversation":false,"support":null}]}
```

`project` には案件の作業場所（`workspace`。ADR-0039 D1。決めていない案件では出ない）も入る。
`tasks` は**仕事の木を描くのに必要な分だけ**（詳細は `GET /tasks/{id}`）。`project_id` が一致するタスクだけが
入り、他の案件・案件に属さないタスクは出ない。ULID でない id・無い案件は 404 `project_not_found`。
`conversation` が `true` の行は**対話用タスク**（人への返事のための run。3.54 参照）なので、仕事の木からは
隠してよい（GUI-R3。Phase 27）。`support`（Phase 29。§3.3 の `TaskSummary.support` と同じ規則）が
`null` でない行は裏方（対話・報告のまとめ・承認・合成レビュー・**途中目標レビュー**）なので、GUI は
仕事の木から一括で外せる。

**Phase 41（ADR-0038 D1 / D4）**: `milestones[]` の各行は `Milestone` のフィールドが**そのまま平らに**出たうえで、
2 つが増える（どちらも無ければ省略。既存の読み手はそのまま動く）:

- `review`: その途中目標についての**秘書のレビューの返事**（`{message_id, text, at}`）。途中目標の仕事が
  止まると celeris が秘書の対話 run（裏方 `support = "milestone_review"`。`tasks[]` にも出る）を 1 回起こし、
  その返事がここに入る。**まだ無ければ「秘書が結果をまとめています」**（`tasks[]` にその裏方タスクが
  `ready` / `running` で居る）。
- `proposal`: その返事が提案した**次の途中目標**（`proposed` の最新。`review` がある行にだけ付く）。

GUI はこの 2 つが揃ったカードに「ok」「議論」「ng」の 3 ボタンと自由記述欄を出す（3.63）。

#### 3.48 `PATCH /projects/{id}` → 200 `Project`

`{"status":"proposed"|"active"|"paused"|"done"}`。知らない値は 400 `bad_request`（本文の解析で落ちる）。

**Phase 43（ADR-0039 D1）**: `status` と `workspace` はどちらも任意になった（書いたものだけ変える）。

- `{"workspace":{"kind":"local","path":"~/workspace/rust/pluvio-poc"}}` — 作業場所だけを設定・差し替える
  （`POST` と同じ検証・`~` の展開・422）。
- `{"workspace":null}` — 作業場所を消す（「作業場所なし」に戻す）。
- `{}`（どちらも書かない）は 422 `validation`。

#### 3.49 途中目標: `POST /projects/{id}/milestones` → 201 `Milestone` / `PATCH /milestones/{id}` → 200 `Milestone`

- 作成の本文は `{"title":"…","description":"…","status":"proposed"}`（`description` と `status` は省略可。
  既定は `""` と `proposed`）。`seq` は**その案件の中での通し番号**をストアが採番する（1 始まり）。
- 状態は `proposed` / `approved` / `in_progress` / `reached` / `redesigned`。達成ごとに人が判定し、
  Go を出すか再設計する（SPEC §7 のアジャイル）。
- 無い案件・無い途中目標は 404 `project_not_found` / `milestone_not_found`。

### 3.50〜3.53 報告（ADR-0033 D3、Phase 25）

報告は**下から上へ**流れる。生成は決定的（run の `done` / `error` / `question` から celeris が 1 件作る。LLM は呼ばない）、
**圧縮だけが LLM**（親ノードが子の報告 4 件、または最古が 2 時間を過ぎたら「まとめの run」を 1 回起こし、その `done` が
親の報告になる。`sources` に子の id が入る）。**悪い知らせ（`bad_news`）は圧縮を待たず、各祖先に複製されて秘書まで届く**
（SPEC §2.4）。人が見るのは `level = 0`（秘書）の報告。

- **読み取り（3.50 / 3.51）は通常の認証**、**既読と通知（3.52 / 3.53）は管理系**（`token_file` 未設定でも 401）。

#### 3.50 `GET /reports` → 200 `ReportList`

```
GET /api/v1/reports?project=<ULID>&node=<org id>&level=<n>&unread=true&limit=50
```

- 新しい順（`created_at` 降順、同値は id 降順）。`limit` の既定は 50、上限 500。知らないクエリキーは 400。
- `level=0&unread=true` が**秘書レベルの未読**（GUI の「報告の流れ」の既定）。
- `Report`: `{id, project_id?, node_id, task_id?, kind, level, headline, body, sources[], read_at?, created_at}`。
  `kind` は `progress` / `result` / `bad_news` / `proposal` / `question`。`project_id` が無いものは「案件なし」
  （クラスタが落ちた等、案件に紐づかない悪い知らせ）。

#### 3.51 `GET /reports/{id}` → 200 `ReportDetail`

- `{report, sources_expanded[]}`。`sources_expanded` は `report.sources` の順に引いた元の報告（消えていたものは飛ばす）。
- 無い id・ULID でない id は 404 `report_not_found`。

#### 3.52 `POST /reports/read` → 200 `{updated}`（**管理系**）

- 本文 `{"ids": ["<report id>", …]}`。既に既読のものは触らない（`updated` は未読から既読に変わった件数）。
- ULID でない id は 404 `report_not_found`。

#### 3.53 `POST /reports/notified` → 200 `{last_notified_at}`（**管理系**）

- GUI がブラウザ通知を出したときに呼ぶ。次の通知は**2 時間後**まで出ない（SPEC §3.5「通知は数時間単位」）。
- `last_notified_at` は **API プロセスのメモリ**にある観測値で、DB には書かない（celeris を再起動すると「まだ通知していない」に戻る）。

#### `GET /daemon` への追加（3.20）

`DaemonSnapshot.reports`（古いスナップショットには無いので `null` でもよい）:

```
reports: { unread_secretary: u32, unread_bad_news: u32, last_notified_at?: String, notify_now: bool }
```

`notify_now` は決定的に決まる: **`bad_news` の未読があれば即 true**、無ければ「未読があり、前回の通知から 2 時間以上経った」とき true。
この 1 フィールドだけはディスパッチャではなく**API が応答を組むときに埋める**（`last_notified_at` が API 側にあるため）。

#### 3.54〜3.55 対話（ADR-0033 D4、Phase 24）

SPEC §3.4「組織の木を見て誰に言うかを決め、その担当に直接言う。相手は人なので先週の議論の続きとして話せる」。
**新しいプロトコルは足していない**: 話しかけると `messages` に `role = "user"` の行が 1 つ入り、そのノードの
run を 1 回起こすための**対話用タスク**（`kind = "execute"`、受け入れ条件なし、`assignee` = そのノード、
`title` = `"対話: <本文の先頭 40 字>"`）が `ready` で 1 件できる。返事はその run の `artifacts/result.json` の
`summary` で、ディスパッチャが `role = "node"` の行として（`run_id` 付きで）足す。

#### 3.54 `GET /org/{id}/messages?project=<ULID>&limit=<n>` → 200 `MessageList`

```json
{"items":[{"id":"01J…","node_id":"secretary","project_id":"01J…","role":"user",
           "text":"この案件をお願いします","task_id":"01J…","created_at":"…"},
          {"id":"01J…","node_id":"secretary","project_id":"01J…","role":"node",
           "text":"理解の確認です。…","run_id":"01J…","task_id":"01J…","created_at":"…"}]}
```

- `task_id` は**その 1 往復を起こした対話用タスク**（`role = "user"` の行にも `role = "node"` の行にも
  同じ id。GUI-R4 / migration 0007。Phase 27 より前に入った行には無い）。

- 並びは**古い順**（`created_at` 昇順、同値は `id` 昇順）。`limit`（既定 50、上限 500）を超えるときは
  **新しい方**を残す（直近のやり取りを読むため）。
- `project` を書けばその案件のスレッド、書かなければ**案件に紐づかない雑談**だけ（混ざらない）。
- 無いノードは 404 `org_node_not_found`、ULID でない `project` は 404 `project_not_found`
  （存在しない案件の ULID は空の一覧）。
- 読み取りなので管理系ではない（トークンを設定した celeris では他の読み取りと同じくトークンが要る）。
- GUI はここをポーリングするか、`GET /stream` のイベントを見て引き直す（返事は同期では返らない）。

#### 3.55 `POST /org/{id}/messages` → 202 `MessageAccepted`（**管理系**）

要求本文 `{"text":"先週の続きで、隣接分野も見てほしい","project_id":"01J…"}`（`project_id` は省略可）。
応答は `{"message_id":"01J…","task_id":"01J…"}`。

- **202**（同期で返事を待たない）。返事が入ったかは 3.54 で見る。
- 人格を持つノードに指示を出す経路なので**管理系**（`token_file` 未設定でも 401。`POST /org` と同じ規律）。
- 無いノードは 404 `org_node_not_found`。空白だけの `text`・存在しない `project_id` は 422 `validation`。
- run の道具立ては決定的に決まる: そのノードの `genre` → 無ければ**対話用分野**（`[[genres]] id = "secretary"`）
  → その分野の `default_role` → 役割の `tier` / `adapter`。予算は対話用の小さい既定
  （`max_turns = 6` / `max_wall_secs = 300` / `max_retries = 1`）で、役割の既定より優先する。
- run のプロンプトには、ノードの `brief`・そのノードの長期記憶（ADR-0033 D6）・**この案件のこのノードとの
  直近のやり取り（既定 20 件）**が前置きされる。
- run が `error` に終わったときの返事は `"返事できませんでした: <理由>"`。これを書くのは**タスクが `failed` に
  落ちたときだけ**で、途中のやり直し（retryable / requeue）では書かない（1 通の問いに返事は 1 行。Phase 27 の
  監査 M-5）。`question` は本文をそのまま返事にすることに加え、**`approvals` に 1 件を作る**（Phase 26、§3.6）。
- **同じノード・同じ案件の対話は直列**（監査 M-3）: 未終了の対話タスクがあれば、新しい対話タスクの
  `depends_on` にそれが入る。つまり 2 通続けて送ると、2 通目は 1 通目の返事が終わるまで `ready` にならない
  （GUI は `GET /tasks/{id}` の `depends_on` で待ち行列を見せられる）。
- 対話用タスクは**報告を作らない**（返事は `messages` で読むもので、報告の流れには出ない。監査 M-6）。
- **秘書の最初の返事**: `POST /projects`（3.46。こちらも管理系）で案件を作ると、その直後に秘書ノードへ
  `request` を本文とした対話が 1 回自動で起きる（SPEC §7）。秘書がいない構成（組織を種蒔きしていない）では
  何も起きない。

### 3.56〜3.60 認可（ADR-0033 D5、Phase 26）

SPEC §3.6「少しでも聞くべきだとエージェントが判断したら、あなたに指示を仰ぐ。あなたはそれに対して
『今回だけ』か『同じようなことは今後ずっと』のどちらかの認可を出す。永続の認可は文字で記録してエージェントに
注入する」。既存の `Question` 終端（`Status::Blocked` / `answers[]`。ADR-0010）に接続する: run が質問で終わると
`approvals` に 1 件できる（宛先は `task.assignee`、無ければ秘書）。**新しいプロトコルは足していない**:
人が答えると、既存の「質問に答える」経路（`POST /tasks/{id}/answer` と同じ `answers[]`）でタスクが再開する。

- **読み取り（3.56 / 3.58）は通常の認証**、**決める・作る・消す（3.57 / 3.59 / 3.60）は管理系**
  （`token_file` 未設定でも 401）。

#### 3.56 `GET /approvals?pending=&project=&node=` → 200 `ApprovalList`

- 古い順（`created_at` 昇順、同値は id 昇順。答える順に並ぶキュー）。`pending` は**三値**:
  `pending=true` で未決定だけ（`decision` 無し）、`pending=false` で**決定済みだけ**（`decision` あり。
  「決めたものの履歴」）、省略すると全件（GUI からの依頼 R5。Phase 27 で `false` が絞り込むようになった）。
  `project` / `node` と AND で効く。
- `Approval`: `{id, project_id?, node_id, task_id?, question, decision?, answer?, created_at, decided_at?}`。
  `decision` は `once` / `standing` / `denied`（未決定は無い）。
- **部をまたぐ委譲の認可**（SPEC §3.1 / Phase 27）は `question` が
  **`"cross-department: <委譲元> -> <委譲先>: <理由>"`** の固定の形で来る（`node_id` = 委譲元、
  `task_id` = 委譲しようとした親タスク）。`once` ならそのタスクの次の run で委譲が通り、`standing` なら
  以後ずっと通る（このとき `standing_rules.rule` には答えの文ではなく
  **`"cross-department: <委譲元> -> <委譲先>"`** が入る）。`denied` なら子は作られず、ワーカーには
  `answers[]` の「認めない: …」が見える。GUI は接頭辞 `cross-department: ` で「連携の認可」として
  見せ分けられる。

#### 3.57 `POST /approvals/{id}/decide` → 200 `ApprovalDecideResult`（**管理系**）

要求本文 `{"decision":"once"|"standing"|"denied","answer":"…","scope":"node"|"all"}`（`scope` は省略可、既定
`node`。`standing` のときだけ意味を持つ）。

- `once` → 既存の「質問に答える」経路（`answers[]`）でそのタスクを再開する。
- `standing` → 同じことをして、さらに `standing_rules` に 1 行追加する（`scope = "node"` ならそのノード宛て、
  `"all"` なら全員）。
- `denied` → 答えを `"認めない: <answer>"` にして再開する（ワーカーが自分で判断できるように）。
- 応答は `{approval, standing_rule?, transition?}`。`standing_rule` は `decision = "standing"` のときだけ、
  `transition`（`TransitionResult`。3.12 `POST /tasks/{id}/answer` と同じ形）は `approval.task_id` があるときだけ載る。
- `answer` が空白だけは 422 `validation`。無い id・ULID でない id は 404 `approval_not_found`。
  `scope` が `"node"`/`"all"` 以外は 400。

#### 3.58 `GET /standing-rules?node=` → 200 `StandingRuleList`

- `node` を書けば**全員向け（`node_id = null`）+ そのノード向け**、書かなければ**絞り込み無し（全ノード分。
  GUI の一覧・編集用）**。古い順。
- `StandingRule`: `{id, node_id?, rule, created_at}`。`node_id` が無いものは全員向け。

#### 3.59 `POST /standing-rules` → 201 `StandingRule`（**管理系**）

要求本文 `{"node_id":"coding-poc","rule":"…"}`（`node_id` は省略すると全員向け）。GUI から直接、質問を経ずに
永続の認可を足すためのもの。`rule` が空白だけは 422 `validation`。

#### 3.60 `DELETE /standing-rules/{id}` → 204（**管理系**）

無い id・ULID でない id は 404 `standing_rule_not_found`。

#### `GET /daemon` への追加（3.20）

`DaemonSnapshot.approvals_pending`（`u32`。古いスナップショットには無いので既定は 0）: 未決定の認可の件数。
`reports` と同じ理由で**API が応答を組むときに埋める**（ディスパッチャの送るスナップショットでは常に 0）。

### 3.61〜3.62 GUI が SPEC の一本を通すために要る API（GUI 監査対応、Phase 29）

GUI の監査（SPEC §4 との突き合わせ、実機操作あり）で「案件が分解されて組織を流れる、を GUI から起動も
観察もできない」と判定された。§3.55 の対話 run は返事だけ（Phase 28）なので、人が方針に納得した後、
分解そのものを起こす入口が無かった。3.61 がその入口。3.62 は SPEC §3.2 の「記憶は案件をまたぐ」を
人が確認するための読み取り専用の窓口（ADR-0033 D6）。

#### 3.61 `POST /projects/{id}/plan` → 202 `{task_id}`（**管理系**）

要求本文 `{"milestone_id":"01J…","note":"急がなくてよい"}`（両方省略可）。GUI の「この方針で進める」の
入口（ADR-0033 D4 に「分解は人が `POST /projects/{id}/plan` で起こす」を追記）。

- 案件の `request`、途中目標（**`approved` / `in_progress` のもの**。`milestone_id` を指定すればそれだけを
  使う）、`note`（人の一言）、そして**この案件の秘書との直近のやり取り**（`messages` 最大 20 件）を
  1 つの `goal` にまとめ、`kind = "plan"` のタスクを 1 件作る（**新しいタスクの種類は作らない**。
  `task_ops::add::create_support_task` にそのまま渡すだけ）。`project_id` / `milestone_id`（明示したときだけ）
  / `assignee = <その案件の秘書>` を持ち、`role` / `genre` は秘書の分野から解決する。受け入れ条件は空で、
  作った直後から `ready`（人が明示的にこの API を呼んだ時点が承認）。
- プランナー（この run）の出力（`PlanOutput.tasks[]`）から作られる子は、親（この plan タスク）の
  `project_id` / `milestone_id` を継ぐ（`task_core::plan::materialize`。Phase 23 の監査 D-3 で確定済み）。
  `assignee` は組織図と分野の manifest を渡されたプロンプトの中で秘書が振る（ADR-0033 D4）。
- 案件の `status` が `proposed` なら `active` にする。`milestone_id` を指定していれば、その途中目標を
  `in_progress` にする（指定しなければ途中目標の状態は変えない。文脈として読むだけ）。
- 応答は `{"task_id":"01J…"}`（202。run を待たない）。
- 401（管理系。`token_file` 未設定でも）。無い案件は 404 `project_not_found`。`milestone_id` がその案件の
  ものでなければ 422 `validation`（`milestone <id> does not belong to project <id>`）。秘書がいない構成
  （組織を種蒔きしていない）は 422 `validation`（`no secretary is configured`）。

#### 3.63 `POST /milestones/{id}/decide` → 202 `MilestoneDecided`（**管理系**）

要求本文 `{"decision":"ok"|"discuss"|"ng","note":"…"}`。SPEC §7 の「途中目標の達成ごとに人が判定し、
Go か再設計」を**人の 3 つの答え**にしたもの（ADR-0038 D2）。**達成にするのは人の `ok` だけ**で、
秘書は「達成と言えるか」を提案するにとどまる（3 値を LLM に解釈させない）。

| `decision` | celeris がすること（決定的） |
|---|---|
| `ok` | この途中目標を `reached`。提案された次の途中目標（`proposed` の最新）を `approved` にし、**その途中目標の分解**を 3.61 と同じ経路で起こす（`note` は計画の `note` に渡り、秘書への `messages` にも `role = "user"` で残る）。分解が始まるので、応答時点のその途中目標は `in_progress` |
| `discuss` | 状態は何も変えない。`note` を秘書への対話として送る（3.55 と同じ経路。案件付き）。秘書の返事に新しい `milestone_proposal` があれば `proposed` の途中目標が差し替わる（古い提案は `redesigned`）。人は納得したら `ok` を押す |
| `ng` | この途中目標を `redesigned`（達成にしない）。提案された次の途中目標も `redesigned`。理由 +「この途中目標自体の再設計を提案せよ」の定型を秘書への対話として送る |

応答（202。run は待たない）:

```json
{"decision":"ok","milestone":{…Milestone…},"next_milestone":{…Milestone…},
 "plan_task_id":"01J…","message_id":null,"conversation_task_id":null}
```

- `next_milestone` / `plan_task_id` は `ok`（提案があるとき）、`message_id` / `conversation_task_id` は
  `discuss` / `ng` で入る（無いものは省略）。提案がまだ無い途中目標に `ok` を押すと、達成にするだけで
  分解は起こさない（`plan_task_id` は `null`）。
- `note` は `discuss` / `ng` では**必須**（空白だけも 422 `validation`）。`ok` では任意。
- 401（管理系。`token_file` 未設定でも）。無い途中目標・ULID でない id は 404 `milestone_not_found`。
  既に `reached` の途中目標は 409 `milestone_reached`。秘書がいない構成は 422 `validation`。

#### 3.62 `GET /org/{id}/memory?project=<id>` → 200 `{notes, project, notes_path, project_path}`（読み取り）

```json
{"notes":"- 2026-09-17: pegasus は pjsub で投げる\n",
 "project":"- 2026-09-17: Pluvio は非同期ランタイム基盤らしい\n",
 "notes_path":"/var/lib/celeris/memory/secretary/notes.md",
 "project_path":"/var/lib/celeris/memory/secretary/projects/01J….md"}
```

- `<memory_dir>/<node_id>/notes.md`（案件をまたぐ記憶）と `projects/<project_id>.md`（案件の引き出し）の
  **全文**（前置き用の 8,000 字カット。ADR-0033 D6 とは別で、上限は切らない）。無ければ空文字列。
  `project` を書かなければ `project` / `project_path` は `null`。
- `[memory]`（`config.toml`）が設定されていなければ 409 `memory_unavailable`。無いノードは 404
  `org_node_not_found`。
- **書き込み API は無い**（記憶は run の後にワーカーが書く。ADR-0033 D6）。人が直したければ
  `notes_path` / `project_path` のファイルを直接編集する。この応答がパスを返すのはそのため。

### 3.63 `POST /tasks/{id}/retry` → 201 `RetryResult`（Phase 31。実機の事故、2026-09-18）

実機で、案件の調査タスクが（LLM 先の停止で）`failed` になったが、API にも GUI にも「やり直す」手段が
無く、「古い draft を取り消して秘書に分解し直させる」という遠回りをした。SPEC §7 のアジャイル（途中目標
ごとに判定してやり直す）には、失敗した仕事を人が一手でやり直せることが要る。

要求本文 `{"accept": false}`（省略可。既定 `false`）。

- `failed` または `cancelled` のタスクを**複製して新しいタスクを作る**（`Failed`/`Cancelled` を非終端に
  戻す状態機械の遷移は**足していない**。DESIGN の状態機械を壊さないため）。それ以外の状態は 409
  `invalid_transition`（`trigger: "retry"`）。無いタスクは 404 `task_not_found`。
- 複製するもの: `title` / `objective` / `acceptance` / `inputs` / `worker_hint` / `budget` / `role` /
  `genre` / `project_id` / `milestone_id` / `assignee` / `parent_id` / `workspace`。`depends_on` は
  **元と同じ**。`attempts` は 0 から。新しいタスクは既定で `draft`（人が Go を出す。§3.10 の
  `POST /tasks/{id}/approve` で `draft → ready`）。`accept: true` を本文に付ければ、作成の時点で
  `ready` から始まる（別の遷移は経由しない。`task_ops::add::create_support_task` と同じ「その場で
  `ready` を書く」流儀）。新しいタスクに `Event::Created` + `Event::Retried{from: <元の id>}` を記録する。
- **元のタスクに依存していた未終端のタスク**（`draft` / `ready` / `blocked`。対話タスクは
  `DependencyFailed` の対象外なので `draft`/`ready` のまま残っていることがある。P-78）と、**その依存の
  失敗で `cancelled` になっていたタスク**（最後の `Transitioned` の `reason` が `"dependency_failed"`
  のもの。人が直接 `cancel` したものは対象外）の `depends_on` を、元の id から新しい id に張り替える。
  後者は `cancelled` → `draft` に戻し（`Event::Transitioned{from: "cancelled", to: "draft",
  reason: "retried"}` を記録）、前者は状態を変えずに `depends_on` だけ書き換える。
- 応答は `201 {"task_id": "01J…", "rewired": ["01J…", …]}`（`Location: /api/v1/tasks/{task_id}`）。
  `rewired` は張り替えたタスクの id（順不同）。
- `token_file` があれば通常どおりトークン必須、無ければ他の変更系と同じ（**管理系ではない**。
  §3.10〜3.13 と同じ扱い）。
- `task_ops::actions(task)`（§5.4）は `failed` / `cancelled` のタスクに `Action::Retry`（`"retry"`）を足す。
  受信箱の `failed` 項目、`GET /tasks/{id}` の `failed`/`cancelled` 表示、案件の仕事の木の失敗ノードは、
  みな `actions` にこれが立つのでボタンの表示に迷わない。

### 3.64〜3.65 通知（Discord）（ADR-0037、Phase 39 / Phase 40）

「人の判断が要るとき」だけ Discord の webhook に 1 通投げる仕組みの、設定の確認とテスト送信。
**判定と送信は celeris の tick が決定的に行う**（LLM は関与しない）。API は台帳（`notifications` 表）を
読むだけで、送信は celeris に委譲する。

知らせるのは「人の判断が要る」出来事だけ（ADR-0037 D1）: `milestone_ready` / `approval_pending` /
`question_blocked` / `bad_news` / `secretary_reply` / `task_ready` / `cluster_login_needed`
（`cluster_login_needed` は ADR-0053 D3、Phase 66。クラスタの ssh master が落ち、鍵認証も失敗して
人の TOTP 入力が要る状態。`key` = クラスタ id。celeris が outage ごとに 1 回だけ台帳へ書くので、
同じ outage で 2 通目が来ることはない）。`result` / `progress` は**知らせない**（SPEC §3.5 の
数時間単位の流れは GUI の報告の仕事）。同じ `(kind, key)` は 1 回だけ送り、失敗したら次の tick で
再送する（最大 3 回。429 はここに数えない）。

Phase 40（実機 2026-09-18）で変わった点:

- `milestone_ready` は「動いているものが無く、人の手が要る」状態（ready/running/reviewing/blocked が
  0 件、done が 1 件以上）で鳴る。**全部が終端である必要はない** — Go 待ちの `draft` が残っていてもよい
  （むしろそここそが人の判断が要る瞬間）。`key` は `<途中目標 id>:<done の件数>` で、Go を出して
  また止まると done の件数が変わるので再び鳴る。
- `milestone_ready` / `approval_pending` / `question_blocked` / `bad_news` / `secretary_reply` のうち
  `milestone_ready` を除く 4 種は、**celeris の起動より前に作られた出来事は対象にしない**
  （backfill 禁止。GUI で既に見た昨日以前の履歴が起動直後に一斉送信されることはない）。
- 1 tick（`interval_secs`）に送るのは最大 1 通。`bad_news` が複数 pending なら 1 通に束ねる
  （台帳の行は個別に決着する）。

webhook の URL は**秘密**で、`[secrets]`（§3.36〜3.38 / ADR-0030）に id `discord-webhook`（既定。
`[notify] discord_webhook_secret` で変えられる）で登録する。**URL は応答にもログにも問題詳細にも出ない。**

#### 3.64 `GET /notify` → 200 `NotifyView`（読み取り）

```jsonc
{
  "configured": true,                 // その id の秘密が登録されていて、送れる状態か
  "secret_id": "discord-webhook",     // GUI はこの id で「API キー」画面への導線を出す
  "fingerprint": "3f9a1c02",          // 値の sha256 の先頭 8 桁（値は復元できない）。未登録なら省略
  "gui_base_url": "http://192.168.1.103:7700",  // 文面に付けるリンクの根。未設定なら省略
  "recent": [                          // 直近 10 件（新しい順）
    {
      "kind": "milestone_ready",
      "key": "01J…:2",                // 途中目標 id / 認可 id / タスク id / 報告 id / 案件 id
                                       // （`milestone_ready` は `<途中目標 id>:<done の件数>`。Phase 40）
      "project_id": "01K…",           // GUI 依頼 G13i-P1（Phase 40）: milestone_ready はその途中目標の
                                       // 案件、secretary_reply はその案件自身、他の種は省略（null 相当）
      "created_at": "2026-09-18T12:00:00Z",
      "sent_at": "2026-09-18T12:00:01Z",   // まだなら省略
      "attempts": 1,
      "ok": true,                      // 省略 = まだ決着していない（次の tick で再送）、false = 諦めた
      "error": "http status 404"       // 失敗の理由（**URL・ホスト名は入らない**）。無ければ省略
    }
  ]
}
```

- 通常の認証だけ（`token_file` があればトークン必須）。本文に URL は**絶対に含まれない**。
- `configured` が false のとき、GUI は「未設定」と出し、`secret_id` を添えて API キー画面へ導く。
- 送らずに畳んだ行（秘密が無い間に起きた出来事）は `ok: false`、`attempts: 0`、
  `error: "discord webhook is not configured"` で並ぶ（ADR-0037 D2:「秘密が無い間の出来事は通知しない」）。
- `project_id` は GUI がリンクを作るためだけの補助情報で、判定は celeris がその通知を作った時点で
  分かっている案件 id をそのまま台帳に書いたもの（応答時に途中目標から逆引きしない）。

#### 3.65 `POST /notify/test` → 200 `NotifyTestResult`（**管理系: `token_file` 未設定でも 401**）

要求本文は無し（`{}` でよい）。定型のテスト文を 1 通だけ送る。台帳（`notifications`）には残さない。

```jsonc
{ "ok": true, "detail": "the test message was delivered" }
```

- `ok: false` でも 200（送り先が 404 を返した等）。`detail` は**種別だけ**の短い文で、URL・ホスト名は入らない。
- 409 `notify_unavailable`: 秘密が登録されていない（`[secrets]` 自体が無い場合も含む）、または celeris に
  委譲できない構成。`detail` には id（既定 `discord-webhook`）だけを書く。
- 401 `unauthorized`: トークン無し（`token_file` を設定していない構成でも 401）。

### 3.66〜3.67 リリース（自己改善のデプロイ）（ADR-0040 D6、Phase 48）

`scripts/selfdeploy/release.sh` が作った**不変のリリース**（`~/.local/celeris/releases/<sha12>/`）を一覧し、
検証済みのものへ**人が**昇格する。設計は `docs/adr/0040-self-improvement-deploy.md`、運用は
`docs/selfdeploy.md`。読む先は `[selfdeploy] releases_dir`（既定 `releases`。設定ファイルのディレクトリ基準）。

| | |
|---|---|
| 何を読むか | `<releases_dir>/<sha12>/{manifest.json, gate.json, verify.json, changes.json, promoted.json}`、`promote.lock`、`<releases_dir>` の**親**の `current` / `previous` の symlink、`daemon_instances`（ADR-0040 D4）、`[selfdeploy] repo` の git（`on_main` のためだけ。ADR-0041 D3） |
| 何を書くか | `POST .../promote` のときだけ `<release>/promote.lock` と `<release>/promote.log`。**DB も設定も本番プロセスも、`[selfdeploy] repo` の git リポジトリも触らない** |
| 誰が昇格するか | **人だけ**（ADR-0040 D5）。この API か shell から。celeris の中に自動で呼ぶ経路は無い |

`.build` / `.cargo-target` のような `.` で始まる名前と、組み立て途中の `<sha12>.partial` は一覧に出ない。
`manifest.json` / `gate.json` が壊れていても一覧は落ちず、その 1 件が `gate_ok: false` と `problem` を持つ。

#### 3.66 `GET /releases` → 200 `Releases`（読み取り。**管理系ではない**）

```jsonc
{
  "current": "9ca90bd4f1c2",          // <releases_dir>/../current が指す sha12。無ければ null
  "previous": "448ae0c33b19",         // 同 previous。rollback 先
  "running": {                        // いまこの要求に答えているプロセス自身（GET /health と同じ値）
    "release": "9ca90bd4f1c2",        // --release / CELERIS_RELEASE / "dev"
    "role": "active",                 // active | standby | draining | verify
    "instance_id": "01K…"
  },
  "instances": [                      // daemon_instances（ADR-0040 D4）。started_at 昇順
    { "instance_id": "01K…", "release": "9ca90bd4f1c2", "pid": 1234, "role": "draining",
      "started_at": "2026-09-19T09:00:00Z", "heartbeat_at": "2026-09-19T10:00:00Z",
      "handoff_requested_at": "2026-09-19T09:59:00Z", "drained_at": null }
  ],
  "items": [                          // built_at の新しい順（読めなかったものは最後）
    {
      "sha12": "abcdef123456",
      "ref": "self/01M2…",            // release.sh に渡した ref。読めなければ null
      "built_at": "2026-09-19T08:00:00Z",
      "schema_version": 11,
      "gate_ok": true,                // gate.json の ok（cargo test / clippy / build / pnpm … が全部 exit 0）
      "verify": { "ok": true, "live_ok": true, "at": "2026-09-19T08:30:00Z" },  // null = 未検証
      "promoted_at": null,            // promoted.json（昇格に成功したときだけ）。null = 一度も昇格していない
      "on_main": false,               // git merge-base --is-ancestor <sha> main。null = 分からない
      "changes": {                    // changes.json（ADR-0041 D4）。null = Phase 48 以前のリリース
        "base": "9ca90bd4f1c2",       // ビルド時の current の sha12（null = current が無かった）
        "stale": false,               // base != いまの current（＝この差分はもう「いま」の話ではない）
        "commit_count": 3,
        "file_count": 12,
        "sensitive": ["scripts/selfdeploy/verify.sh"],   // 安全に関わる変更。空なら普通のリリース
        "commits": [{ "sha": "…40 桁…", "subject": "phase 50: …" }]   // 新しい順、最大 50 件
      },
      "is_current": false,
      "is_previous": false,
      "promoting": false,             // promote.lock の pid がまだ生きている
      "promote_failed": null,         // promote_failed.json（直近の昇格の試みが失敗したときだけ）。§3.67 の後注
      "problem": null                 // manifest/gate が読めなかったときだけ一行（普段は省略）
    }
  ]
}
```

- 通常の認証だけ（`token_file` があればトークン必須。`GET /notify` と同じ扱い）。
- `[selfdeploy]` が無い構成、`releases_dir` がまだ無い（初回）ときも **200**（`items: []`）。
- GUI は `verify` で状態を出し分ける: `null` =「未検証」、`ok && live_ok` =「検証済み（ライブ引き継ぎ）」、
  `ok && !live_ok` =「検証済み（停止 → 起動）」、`!ok` =「検証に落ちた」。
- 引き継ぎの進行は `instances` で見える（旧が `draining`、新が `active`。ADR-0040 D4）。
  昇格の最中は `GET /releases` を数秒ごとに読み直せばよい（SSE には載らない）。

**ADR-0041 D3 / D4（Phase 50）で増えた 3 つ:**

- **`promoted_at`**: `promote.sh` が昇格に成功したときに書く `<release>/promoted.json` の
  `{promoted_at, mode, from}` の `promoted_at`。まだ昇格していないリリースは `null`。
- **`on_main`**: その sha が `[selfdeploy] repo`（既定 `~/workspace/agent-platform`。`~` は celeris の
  `$HOME` で展開）の `main` の**祖先**か。`git -C <repo> merge-base --is-ancestor <sha> main` の
  終了コードそのままで、`0` → `true`、`1` → `false`、それ以外（リポジトリが無い・`main` が無い・
  git が無い・時間切れ・その sha を知らない）は **`null`**。**celeris はこのリポジトリを読むだけ**で、
  checkout も fetch も merge もしない（反映は人がやる。ADR-0041 D3）。
  GUI は `current` の行が `on_main: false` のときだけ「本番は main に未反映: `git merge --ff-only <sha12>`」と出す。
- **`changes`**: `release.sh` が**ビルド時に**書いた `<release>/changes.json` の要約。
  `sensitive` は `scripts/selfdeploy/lib.sh` の `SD_SENSITIVE_PATTERNS`（`scripts/selfdeploy/`、`deploy/`、
  `crates/celeris/src/instance.rs`、`crates/celeris/src/releases.rs`、`crates/task-api/src/releases.rs`、
  `crates/task-core/migrations/`、`CLAUDE.md`、`gui/CLAUDE.md`、`.claude/`、`config/`、`docs/adr/0040-`、
  `docs/adr/0041-`）に**前方一致**したファイル。**判定は `release.sh` の側で済んでいて、API も GUI も
  その結果を運ぶだけ**（パターンを 2 か所に置かない）。`changes.json` が無いリリース（Phase 48 以前）は `null`。

#### 3.67 `POST /releases/{sha12}/promote` → 202 `ReleasePromoteAccepted`（**管理系: `token_file` 未設定でも 401**）

要求本文は無し（`{}` でよい）。`promote.sh <sha12>` を **detached**（`setsid`、stdin は `/dev/null`、
stdout/err は `<release>/promote.log`）で起こし、`promote.lock` に pid を書いてすぐ返す。
**昇格の完了は待たない。**

**どちらの `promote.sh` を起こすか**（ADR-0041 D4。Phase 50 で変わった）:
**いま動いている版**のもの（`<releases_dir>/<current>/scripts/promote.sh`）を使う。昇格は「動いている
本番を止めて／引き継いで新しい版に替える」作業で、その手順を知っているべきなのはいまの本番だから。
実装者が `scripts/selfdeploy/` を壊したリリースを作っても、その壊れた昇格スクリプトは走らない
（新しい昇格スクリプトは、それ自身が一度昇格されてから次の昇格で使われる）。`current` に `scripts/` が
無い（Phase 48 以前のリリース、または初回）ときだけ昇格先のものを使う。どちらを使ったかは `script_from`。

```jsonc
{ "sha12": "abcdef123456",
  "log": "/home/…/celeris/releases/abcdef123456/promote.log",   // 中身は API では出さない
  "started_at": "2026-09-19T10:00:00Z",
  "script_from": "current" }   // "current" | "target"
```

- 404 `release_not_found`: その sha12 のディレクトリが無い（sha12 の形＝16 進 7〜40 桁でないときも同じ）。
- 409 `release_not_promotable`: `verify.json` が無い／`ok` でない、既に `current`、既に昇格中
  （`promote.lock` の pid が生きている）、`current` にも昇格先にも `scripts/promote.sh` が無い
  （どちらも Phase 48 より前のリリース）、`[selfdeploy]` が無い。`detail` に理由の一行。
- 401 `unauthorized`: トークン無し（`token_file` を設定していない構成でも 401）。
- **この要求に答えた celeris 自身が、その昇格で `draining` になって最後には終わる**（ADR-0040 D4 の
  ライブ引き継ぎ）。202 を返した後に同じプロセスの API が閉じるのは正常。GUI は `GET /releases` を
  読み直して `running.release` が新しい sha12 になるのを待つ（同じポートを新旧が `SO_REUSEPORT` で
  共有するので接続は切れない）。
- `promote.sh` は `verify.json.ok` を自分でも確かめる（`--force` は無い）。この API の 409 はその前段の
  早い拒否で、二重の防壁になっている。
- **`promote.sh` が detached で始まった後に失敗しても、この 202 は変わらない**（celeris は「起こせた」
  ことしか知らない）。バグ報告（2026-09-21）: 押した後 GUI に何も出ず、成功も失敗も分からなかった —
  `promote.sh` は落ちるとただ死ぬだけで、`promote.lock` の pid が消えると `promoting` が偽に戻るので、
  「終わった」ようにしか見えなかった。`promote.sh` はどこで死んでも（EXIT トラップ）
  `<release>/promote_failed.json` に `{failed_at, error}`（`error` はログの末尾 20 行）を書くようになった。
  `GET /releases` の `items[].promote_failed` はこれを写す（§3.66）。次の昇格の試みが始まる
  （この API が呼ばれる）と、そのリリースの `promote_failed.json` は消える — 古い失敗が残り続けない。
  GUI は `promoting` が偽で `promote_failed` が非 `null` のときだけ赤いバナーを出す。

### 3.68〜3.71 案件のリポジトリ（ADR-0043 D1、Phase 52。**57〜60。変更系はすべて管理系: `token_file` 未設定でも 401**）

案件は**リポジトリを複数持つ**（論文の `benchfs-paper` とコードの `benchfs`、git ではないデータの置き場）。
`is_primary` の 1 件が「主なリポジトリ」で、**`Project.workspace` はその `location` の写し**である
（GUI の後方互換。`PATCH /projects {workspace}` は primary を書き換える）。

#### 3.68 `GET /projects/{id}/repos` → 200 `RepoList`

- 読み取り（トークン不要）。並びは **primary が先頭**、あとは作った順
- 知らない案件は 404 `project_not_found`

#### 3.69 `POST /projects/{id}/repos` → 201 `ProjectRepo`

```json
{
  "name": "benchfs",
  "kind": "git",
  "location": {"kind": "local", "path": "~/workspace/rust/benchfs"},
  "default_branch": "main",
  "run": "auto",
  "is_primary": true
}
```

- `location` だけが必須。`name` を省略するとパスの末尾から slug を作る（`benchfs`）。
  `kind` を省略すると `<path>/.git` があれば `git`、無ければ `dir`（リモートは `git`）
- `location.kind = "local"` の `path` の `~` は celeris の `$HOME` で展開して保存する（ADR-0039 D5）。
  `remote` の `cluster` が `[[clusters]]` に無ければ 422 `validation`
- `name` は案件内で一意の slug（`[a-z0-9._-]`、1〜64 文字、先頭が `.` / `-` でないこと）。
  重複・不正な名前は 422。`<workspace_root>/<task_id>/repos/<name>/` というディレクトリ名になるため
- **案件の最初の 1 件は自動的に `is_primary = true`** になる（案件に主なリポジトリが無い状態を作らない）。
  `is_primary: true` を立てると、同じ案件の他の行の `is_primary` は落ちる
- `sync` は `location.kind = "remote"` のときだけ（`worktree`〈既定〉 / `rsync` / `none`）。
  **`sync: "none"` は 422**（ADR-0043 D7 のリモート (b) は未実装）
- `run` は `auto`（既定。`workspace.toml` の `[run] mode` に従う）/ `host` / `container`。
  **`container` はこの Phase では読むだけで、実行には使われない**（ADR-0043 A3）

#### 3.70 `PATCH /repos/{id}` → 200 `ProjectRepo`

- 書いたものだけ変える（1 つも書かなければ 422）。`default_branch` / `sync` は `null` を明示すると消える
- `is_primary: true` でこの行が primary になる（`false` は何もしない。primary を空にはできない）
- `kind` を `dir` にすると `default_branch` は落ちる。`location` を `local` にすると `sync` は落ちる
- 知らない id は 404 `repo_not_found`

#### 3.71 `DELETE /repos/{id}` → 204

- **未終端（`done` / `failed` / `cancelled` 以外）のタスクがそのリポジトリを使っていたら 409 `repo_in_use`**
- primary を消したら、残りのうち一番古いものが primary になる

### 3.72〜3.73 タスクの作業ツリーの閲覧（ADR-0043 D6、Phase 52。**読み取り。トークン不要**）

タスクの作業場所は `<workspace_root>/<task_id>/repos/<name>/`（git は worktree、`dir` は実体への
シンボリックリンク）。`<workspace_root>/<task_id>/worktree.json` がその目印で、この 2 つの
エンドポイントはそれだけを見る（git は起こさない）。

#### 3.72 `GET /tasks/{id}/tree?repo=&path=` → 200 `TreeView`

- `repo` を省略すると**先頭のリポジトリ**（= ワーカーのカレントディレクトリ）。そのタスクに無い名前は 404
- `path` はそのリポジトリの作業ツリーからの**相対パス**（省略・空は根）。
  `..` を含むもの・絶対パスは **403 `path_forbidden`**。解決した実体が作業ツリーの外に出るもの
  （シンボリックリンクでの脱出）も 403
- 作業ツリーを持たないタスク（`worktree.json` が無い）は 404 `file_not_found`
- `entries` はディレクトリが先、あとは名前順（決定的）。`kind` は `dir` / `file` / `other`

```json
{
  "repo": "benchfs",
  "path": "src",
  "repos": [
    {"name": "benchfs", "kind": "git", "dir": "/home/u/.local/celeris/workspaces/01J.../repos/benchfs",
     "branch": "celeris/01J...", "base": "9602b596826c..."},
    {"name": "data", "kind": "dir", "dir": "/home/u/.local/celeris/workspaces/01J.../repos/data"}
  ],
  "entries": [
    {"name": "bin", "path": "src/bin", "kind": "dir"},
    {"name": "lib.rs", "path": "src/lib.rs", "kind": "file", "size": 1234}
  ]
}
```

#### 3.73 `GET /tasks/{id}/tree/file?repo=&path=` → 200 `TreeFileView`

- `path` は必須（省略は 400 `bad_request`）。境界の規則は 3.72 と同じ
- ディレクトリを指したら 403 `path_forbidden`
- **テキストで 512 KiB 以下のときだけ `text` を返す**。超えたら `too_large: true`（`text` なし）、
  バイナリ（NUL を含む・UTF-8 でない）は `binary: true`（`text` なし）。どちらも `size` は返す

---

### 3.74〜3.78 タスク管理: 編集・コメント・再開・タイムライン（ADR-0044 B1、Phase 53）

**Phase 59（ADR-0046 D2 / D3 / D4）**: `POST /tasks` と `PATCH /tasks/{id}` は 3 つの項目を足した。

| 項目 | 型 | 意味 |
|---|---|---|
| `skills` | `string[]` | そのタスクに**必要な能力タグ**（小文字 `[a-z0-9._-]`、最大 12 個）。`assignee` が無いタスクの担当は、これとノードの実効 `skills` の重なりで決まる（ADR-0046 D5） |
| `mode` | `"prototype"` / `"production"` / `"research"` | 進め方。既定 `production`。前置きの規則とレビューの厳しさが変わる（ADR-0046 D4） |
| `harness` | `string`（`PATCH` は `null` で外せる） | 実行契約の id。**`Task.genre` 列がそのまま harness id**（応答の `Task` では従来どおり `genre` として出る） |

- `skills` の綴り違反・件数超過は 422 `validation`。`mode` / `harness` の知らない綴りは本文の解析で落ちて
  400 `bad_request`（`harness` は「設定に無い id」なら 422 `validation`）。
- `assignee` と `harness` の組み合わせが合わない（その担当が `harnesses.allowed` にその harness を持たない）
  ときは 422 `validation`（作成時も編集時も）。**profile を 1 つも書いていないノードは従来どおり通る**。
- `EditResult.fields` には `"skills"` / `"mode"` / `"harness"` が入りうる。

人がタスクに手を入れるための 5 本。**`PATCH /tasks/{id}` と `POST /tasks/{id}/comments`、
`POST /tasks/{id}/reopen` は管理系**（`token_file` 未設定でも 401）。読み取り 2 本は通常の認証だけ。

#### 3.74 `PATCH /tasks/{id}` → 200 `{task, fields}`（**管理系**）

要求本文は `task_ops::edit::TaskEdit`。**書いた項目だけ**が変わる。`null` を書ける項目
（`assignee` / `role` / `adapter` / `milestone_id`）は `null` で消す、省略で据え置き。

```json
{"title":"…","objective":"…","acceptance":[{"type":"human","text":"…"}],
 "priority":"P1","labels":["infra"],"category":"ops","repos":["benchfs","benchfs-paper"],
 "assignee":"infra-section","role":"implementer","tier":"frontier","adapter":null,
 "milestone_id":"01J…","depends_on":["01J…"],
 "max_turns":30,"max_wall_secs":1800,"max_retries":2,
 "expected_status":"ready"}
```

- 応答の `fields` は**実際に変わった項目の名前**（決まった並び: `title`, `objective`, `acceptance`,
  `priority`, `labels`, `category`, `repos`, `assignee`, `role`, `tier`, `adapter`, `milestone_id`,
  `depends_on`, `budget`）。何も変わらなければ空配列で、イベントも積まない。
- 変わったときは `Event::Edited{fields, by: "human"}` を**同じトランザクション**で積む
  （`replay` はこのイベントを無視する。状態機械は通らない）。
- **終端（`done` / `failed` / `cancelled`）は 409 `invalid_transition`**（`task_status` / `kind` 付き）。
  やり直すなら `POST /tasks/{id}/retry`、同じ worktree で続けるなら `POST /tasks/{id}/reopen`。
- **`running` / `reviewing` は受け付けるが、走っている run は止めない**（次の run から効く）。
  止めたければ `POST /tasks/{id}/comments`（D2 の割り込み）か `POST /tasks/{id}/cancel`。
- `expected_status` が現在と違えば 409 `conflict`（`expected` / `actual` 付き）。
- 422 `validation`: 1 つも項目を書いていない、空白だけの `title` / `objective`、空の `acceptance`、
  ラベルの規則違反（`[a-z0-9-]` でない・9 個以上）、知らない `assignee`、案件違い・案件なしの
  `milestone_id`、存在しない・`failed`/`cancelled` の `depends_on`、自分自身への依存、
  **依存の循環**（`depends_on would create a cycle: …`。作成時は新しい id が誰の `depends_on` にも
  入っていないので起きないが、編集では起こせる。循環した 2 件は `ready_tasks` が永久に返さない）、
  **`genre` の `roles` に無い `role`**（作成時と同じ規則。`genre` はこの API では変えられない）。
- **`tier` / `adapter` / 予算は再解決しない**。ADR-0033 D2 の解決順（タスク > 役割 > 担当 > 分野）は
  **作成時に 1 回**効いて `worker_hint` と `budget` に焼き付く。`assignee` や `role` を変えても
  それらは動かないので、変えたければ同じ本文に `tier` / `adapter` / `max_turns` …も書くこと
  （GUI の編集フォームは tier を担当の隣に並べている）。
- **`status` / `attempts` / リースは触らない**。ストアはこの 3 つを、編集を書き戻すトランザクションの
  中で読み直した値で書く（編集フォームを開いている間にディスパッチャが run を始めていても、
  その run を壊さない）。応答の `task` はその読み直した状態を持つ。
- 知らないキーは 400 `bad_request`（`deny_unknown_fields`）。
- **`repos`（ADR-0043 D2。Phase 52 + 53 のマージで入った）**: この案件のリポジトリを**名前で**差し替える
  （`project_repos.name`。§3.68）。解決の規則は `POST /tasks` と同じで、**そのタスクの案件の中**から引く
  （継承〈親 → primary〉は作成時だけの規則なので、`[]` を書けば「リポジトリを使わない」になる）。
  知らない名前・リモートと他のリポジトリの混在・案件に属さないタスクの空でない `repos` は 422 `validation`。
  **走っている run には効かない**（次の run の worktree から。`PATCH` は run を止めない）。

#### 3.75 `GET /tasks/{id}/comments` → 200 `CommentList`

`{"items":[{"id":"01J…","task_id":"01J…","author_kind":"human","author":null,
  "body":"…","run_id":null,"created_at":"2026-09-19T…Z"}]}` — **古い順**（`created_at`、同時刻は `id`）。
`author_kind` は `human`（人）/ `node`（組織の「人」。`author` にノード id）/ `system`（celeris）。
知らないタスクは 404 `task_not_found`。

#### 3.76 `POST /tasks/{id}/comments` → 201 `CommentResult`（**管理系**）

本文は `{"body":"…"}`（空白だけは 422、20,000 文字超は 422）。応答:

```json
{"comment":{…TaskComment…},"effect":"interrupted",
 "transition":{"id":"…","from":"running","to":"ready","reason":"comment","cascaded":[]},
 "can_reopen":false}
```

**人のコメントの効き方（ADR-0044 D2 の表。決定的）**:

| タスクの状態 | `effect` | 何が起きる |
|---|---|---|
| `running` / `reviewing` | `interrupted` | 新しいトリガ `Interrupt` で `ready` に戻す（**attempts 据え置き**、`Transitioned{reason:"comment"}`）。`WorkerFinished{outcome:"interrupted: comment"}` を同じトランザクションで積む。走っていた run はディスパッチャが次の tick で止める（`cancel` と同じ `abort_stale_runs` の経路）。**報告（ADR-0034）は作らない**。次の run の前置きの先頭に「**人からの割り込み**: …」として載る |
| `blocked` | `answered` | `POST /tasks/{id}/answer` と同じ（`Event::Answered`、`Trigger::Answer`、未決の `approvals` も決まる） |
| `ready` / `draft` | `stored` | 記録するだけ（次の run の前置きの「コメント」節に載る。`draft` は Go 待ちのまま） |
| `done` / `failed` / `cancelled` | `terminal` | 記録するだけ。`can_reopen` が真なら GUI は「再開」を出せる（`cancelled` は worktree が無いので偽） |

- ワーカーのコメントはこの API では作れない（ワーカー・プロトコルの `{"type":"comment"}` 行が
  `author_kind = node` で入る。`docs/protocol/worker-protocol.md` §4.7）。ワーカーのコメントは
  **人を起こさない**（状態も変えない）。対話 run とレビュー run にはコメントを書かせない。
- ADR-0044 D8: `interrupted` は `bad_news` にも `error_cooldown` にも数えない
  （`RunOutcomeKind::Interrupted`。§5.2）。
- `WorkerFinished{outcome:"interrupted: comment"}` は、**いま走っているワーカー run**
  （= リースの `worker_run_id`）にだけ付く。`reviewing` の割り込みでは付かない（直近のワーカー run は
  既に `done: …` で終わっており、そこに重ねるとその run の記録を壊すため）。遷移
  （`Transitioned{reason:"comment"}`）はどちらでも残る。
- **認証の非対称**: `blocked` のタスクへのコメントは `POST /tasks/{id}/answer` と同じ状態変化を
  起こすが、こちらは**管理系**（トークン必須）で `answer` は通常の認証だけ。コメントは
  「走っている run を止める」「終端のタスクに記録を足す」もできるので、`PATCH` / `reopen` と同じ
  管理系の扱いに揃えた（ADR-0044 D1/D2 がどちらも「管理系」と書いている）。`answer` / `cancel` /
  `retry` / `POST /tasks` を同じ扱いに揃えるかは別 Phase の判断（`docs/PROGRESS.md` の提案 P-53d）。

#### 3.77 `POST /tasks/{id}/reopen` → 200 `TransitionResult`（**管理系**）

本文は `{"expected_status":"done"}`（任意）。新しいトリガ `Reopen` で **`done` / `failed` → `ready`**、
**attempts は 0 に戻す**（`reason: "reopen"`）。worktree とブランチはそのままなので、続きから直せる。

- `cancelled` は**再開しない**（worktree もブランチも消してある。ADR-0044 D6）→ 409 `invalid_transition`。
  やり直すなら `POST /tasks/{id}/retry`（複製）。
- 非終端も 409 `invalid_transition`。`expected_status` 不一致は 409 `conflict`。

#### 3.78 `GET /tasks/{id}/timeline` → 200 `Timeline`

そのタスクに起きたことを **`at` の昇順で 1 本**にまとめる（同時刻は元の順を保つ安定ソート）。

```json
{"task_id":"01J…","items":[
  {"kind":"event","at":"…","seq":0,"event":{…Event…}},
  {"kind":"comment","at":"…","comment":{…TaskComment…}},
  {"kind":"approval","at":"…","approval":{…Approval…}},
  {"kind":"report","at":"…","report":{…Report…}},
  {"kind":"delegation","at":"…","run_id":"…","tasks":[{…TaskRef…}]},
  {"kind":"release","at":"…","sha12":"abcdef012345","commits":["…"]},
  {"kind":"integration","at":"…","action":"pr","detail":"gui: merged — PR #42 https://…"}]}
```

- `event`: `events` の 1 行（遷移・run・質問・回答・**編集**・**割り込み**）。ただし `Delegated` だけは
  `delegation` に畳む（同じことを 2 回出さない）。
- `approval`: そのタスクの認可（`at` は決まっていれば `decided_at`、まだなら `created_at`）。
- `report`: そのタスクが元になった報告（ADR-0034）。
- `delegation`: `Event::Delegated` の run と、作られた子の `TaskRef`。
- `release`: **そのタスクのブランチのコミット**（`worktree.json` の `branch` と `base` から
  `rev-list <base>..<branch>` で引く）が `~/.local/celeris/releases/*/changes.json` の `commits` に含まれる
  リリース。`worktree.json` が無い・**`base` が空**・git が動かない・`[selfdeploy]` が無いときは
  **何も出さない**（タイムラインは落ちない）。`base` が無いまま `rev-list <branch>` を引くと
  `main` の歴史まで「このタスクの変更」として並ぶので、そこは出さない側に倒す。
- `items[].event` は**新しい方から 2,000 件**まで（それより古いイベントは `GET /tasks/{id}/events`
  でページングして見る）。クエリパラメータは受け付けない。
- 並びは `at` を**時刻として**比べる（RFC 3339 の小数秒があるので、文字列比較では順が狂う）。
- `integration`（ADR-0043 D5 / A2 の取り込み: merge / PR / discard）は `task_integrations`（§3.79〜3.83）から
  出る（Phase 54 との合流で有効になった）。`action` は `merge` / `pr` / `discard`、`detail` は
  `<リポジトリ>: <行方>` + PR の番号と URL + 理由の一行。`at` は**人が押した時刻**（記録の `created_at`。
  PR の同期では動かない）。GUI は知らない `kind` を無視できるようにしておくこと。
- `doc`（ADR-0044 D7、Phase 57 の逆リンク）は **front matter の `tasks:` にこのタスクを持つページ**
  （`{"kind":"doc","at":"…","project_id":"01J…","path":"docs/research/fs.md","title":"調べたこと"}`）。
  案件の文書の根を `git grep -l "<タスク id>"` で絞ってから front matter を確かめるので、本文に id が
  出ただけのページは載らない。`at` はそのページの**最後のコミットの時刻**（読めなければ空）。
  文書の根が無い案件・git が動かないときは**何も出さない**（タイムラインは落ちない）。
- `knowledge`（ADR-0047 D4/D5、Phase 62）は、そのタスクの終端から起きた知識整理 run（`knowledge_runs`）を
  1 件（`state` は `scheduled` / `applied` / `failed`。`applied` のときだけ `ingested` / `inbox` / `discarded`）。
  ADR-0052 D2（Phase 64）で `via` が増えた: `"langmem"` なら従来どおり Qwen で抽出、`"fallback:<adapter>"` なら
  Qwen に届かず tier `cheap` の汎用ハーネスで抽出した（GUI は後者に「cheap のハーネスで抽出」を出す）。
  run が 1 度も始まっていなければ `via` は付かない。
- 知らないタスクは 404 `task_not_found`。

---

### 3.79〜3.83 変更の取り込み（ADR-0043 D5、Phase 54。**68〜72。変更系は管理系: `token_file` 未設定でも 401**）

タスクは `celeris/<task_id>` ブランチに変更を積む（ADR-0043 D2）。それを**人が**見て、`main` に
取り込むか、PR にするか、捨てる。**押せるのは人だけ**（SPEC §3.6）。ワーカープロトコル
（`docs/protocol/worker-protocol.md`）にはこの経路が無いので、組織の「人」が `main` を動かす道は無い。

見る先は §3.72 と同じ目印（`<workspace_root>/<task_id>/worktree.json`）で、`kind = "git"` の
リポジトリだけが対象（`dir` は出ない）。目印が無いタスクは 404 `file_not_found`。

celeris はここで **`git` と `gh` だけ**を、待ち時間の上限付きで起こす（読み取り 30 秒、書き込み 300 秒、
`gh pr view` 10 秒、`gh pr create` / `gh pr merge` 120 秒）。LLM もワーカーも起こさない。

#### 3.79 `GET /tasks/{id}/changes` → 200 `ChangesView`

- リポジトリごとに `base`（`merge-base(default_branch, head)`）/ `head` / `ahead`（`base..head` の
  コミット数）/ `files` / `stat` / `dirty` / `missing` / `origin` / `integration` を返す
- `files` は **base から「いまの作業ツリー」まで**（コミット済み + 未コミット + 追跡外）。
  `dirty` は `git status --porcelain` が空でないこと、`ahead` はコミットの数なので、
  **コミットが 1 つも無いタスクは `ahead = 0`**（GUI は「変更なし」）
- worktree が消えていてもブランチが残っていれば、元のリポジトリで `base..<branch>` を見る
  （そのとき `dirty` は常に `false`）。**どちらも無ければ `missing: true`、`ahead: 0`、`files: []`**
- `default_branch` は `project_repos.default_branch`、無ければ検出（`origin/HEAD` → `main` → `master`）
- **PR の同期はここでだけ行う**（ADR-0043 D5「常時同期はしない」）。`integration.state` が `open` で
  `pr_number` があれば `gh pr view <n> --json state,mergedAt,mergeable,reviewDecision,url` を 10 秒で呼び、
  `OPEN` / `MERGED` / `CLOSED` を `open` / `merged` / `closed` に写す。`merged` になったら**その場で
  worktree とローカルのブランチを片付ける**。`gh` が失敗したときは記録を**触らない**（`failed` にしない）
- `gh`（真偽）は `gh auth status` の結果（**プロセス内で 60 秒だけ覚える**）。`merge_method` は `[github] merge_method`

```json
{
  "task_id": "01J...",
  "gh": true,
  "merge_method": "merge",
  "repos": [
    {"repo": "benchfs", "branch": "celeris/01J...", "default_branch": "main",
     "base": "9602b596826c...", "head": "1f2e3d4c5b6a...", "ahead": 3,
     "files": [{"path": "src/lib.rs", "status": "M", "additions": 12, "deletions": 3},
               {"path": "notes.md", "status": "?", "additions": 8, "deletions": 0}],
     "stat": {"files": 2, "additions": 20, "deletions": 3},
     "dirty": true, "missing": false, "origin": true,
     "integration": {"id": "01J...", "task_id": "01J...", "repo": "benchfs", "method": "pr",
                     "state": "open", "pr_number": 42, "pr_url": "https://github.com/o/r/pull/42",
                     "created_at": "2026-09-19T10:00:00Z", "updated_at": "2026-09-19T10:00:00Z"}}
  ]
}
```

#### 3.80 `GET /tasks/{id}/changes/{repo}/diff?path=` → 200 `ChangeDiffView`

- `path` は必須（省略は 400 `bad_request`）。`..` と絶対パスは 403 `path_forbidden`
- そのタスクに無い `repo` は 404 `file_not_found`
- **200 KiB で切る**（切ったら `truncated: true`）。差分が無ければ `diff` は空文字列
- 追跡外のファイルは `git diff --no-index -- /dev/null <path>` の出力（「全部追加」の形）

#### 3.81 `POST /tasks/{id}/changes/{repo}/integrate` → 200 `IntegrateResult`（**管理系**）

要求本文 `IntegrateBody`: `{"method": "merge" | "pr" | "discard", "note"?: string, "confirm"?: bool}`。

- **`merge`**: 一時 worktree（`<workspace_root>/.integrate/<ulid>`）でブランチを `default_branch` に
  `rebase` し、成功したら `default_branch` を進める。`origin` へ push は**しない**
  - 人のチェックアウトが `default_branch` を出していて `git status --porcelain` が空でなければ
    **409 `default_branch_busy`**（`detail` は `"<default_branch> が編集中"`）。**記録も残さない**
  - 出していて綺麗なら `git -C <local.path> merge --ff-only <sha>`（人の作業ツリーも進む）
  - 別のブランチ（または detached）なら `git -C <local.path> update-ref refs/heads/<default_branch> <new> <old>`
    （**人の作業ツリーには触らない**）
  - 成功したら worktree を消してブランチを `git branch -D` し、`state = "done"`（`merged_at` も入る）
  - `rebase` が衝突したら `rebase --abort` して worktree もブランチも残し、`state = "conflict"` を記録して
    **「衝突の解消: <題名>」タスクを自動で作る**（応答の `child_task_id`。下の注記）
- **`pr`**: `origin` リモートが無い、または `gh` が使えない（PATH に無い・認証されていない）ときは
  **409 `pr_unavailable`**。そうでなければ `git push -u origin <branch>` →
  `gh pr create --base <default_branch> --head <branch> --title <タスクの題名> --body <生成>` →
  `state = "open"`（`pr_number` / `pr_url`）。本文は**決定的**（目的 / 受け入れ条件 / 最新の報告の要約 /
  `Celeris task <id>` と `[notify] gui_base_url` があればそのリンク）。LLM は使わない
- **`discard`**: `{"confirm": true}` が要る（無ければ 422 `validation`、`errors[0].field = "confirm"`）。
  worktree を消してブランチを `git branch -D` し、`state = "done"`
- `git` / `gh` が失敗したときは **200** を返し、記録が `state = "failed"` と `detail`（理由の一行）を持つ
  （GUI はそれをそのまま出す）。409 になるのは上の 2 つだけ
- `note` は記録の `detail` の先頭に入る（PR の本文には入れない）

**衝突の解消タスク**（ADR-0043 D5）: 親 = 元のタスク、担当（`assignee` / `role` / `genre` / `tier` / 予算）は
親と同じ、`status = "ready"`、前置きに衝突したファイルの一覧。受け入れ条件は `Check::Command` 3 本
（作業ツリーが clean / rebase が進行中でない / `default_branch` が `HEAD` の祖先）。
**親のブランチの上で**働かせるため、`workspace` は**親の worktree のパス + `mode = "shared"`** で
`repos` は空（Phase 54 の実装判断。`docs/PROGRESS.md` の P54-2）。終わったら人がもう一度 `merge` を押す。

#### 3.82 `POST /tasks/{id}/changes/{repo}/pr/merge` → 200 `IntegrateResult`（**管理系**）

- 本文は空の JSON（`{}`）でよい
- そのリポジトリの最新の記録が `method = "pr"` かつ `state = "open"` でなければ **409 `pr_unavailable`**
- `gh pr merge <n> --<[github] merge_method> --delete-branch` → そのまま `gh pr view` で同期する。
  `merged` になったら worktree とローカルのブランチを片付ける
- `gh` が失敗したら 200 で `state = "failed"` と `detail`

#### 3.83 `GET /projects/{id}/integrations` → 200 `ProjectIntegrations`

- その案件のタスクの取り込みを、**タスク × リポジトリごとに最新の 1 件**だけ、新しい順に最大 200 件
- `state = "open"` のものは `gh pr view` で同期する（**1 回の呼び出しで 20 件まで**）
- 無い案件は 404 `project_not_found`

```json
{"items": [{"integration": {"id": "01J...", "task_id": "01J...", "repo": "benchfs", "method": "pr",
                            "state": "open", "pr_number": 42, "pr_url": "https://github.com/o/r/pull/42",
                            "created_at": "2026-09-19T10:00:00Z", "updated_at": "2026-09-19T10:05:00Z"},
            "task_title": "ワークスペース A2", "task_status": "done"}]}
```

---

### 3.84〜3.91 中止・一時停止・アーカイブ（ADR-0044 D6、Phase 55。**73〜80。すべて管理系: `token_file` 未設定でも 401**）

タスクの中止は従来どおり `POST /tasks/{id}/cancel`（3.13）。ここはその**上の 2 階層**（途中目標と案件）。
要求本文は `{}`（空本体も可）。未知のフィールドは 400。どれも応答は 200。

| 操作 | 何が起きる |
|---|---|
| `cancel` | 属する**非終端タスクを全部** `cancelled` にする。案件の中止は**非終端の途中目標**（`proposed`/`approved`/`in_progress`/`paused`）も `cancelled` にする。最後に自分が `cancelled`。連鎖は**決定的・同期** |
| `pause` | 自分が `paused`。元の状態は `paused_from` に残る。**属するタスクは dispatch されない**（`ready` のまま。状態機械は触らない）。走っている run は最後まで走る |
| `resume` | `paused_from` へ戻す（無ければ案件は `active`、途中目標は `in_progress`）。`paused_from` は消える |
| `archive` | **終端（`done` / `cancelled`）の案件だけ**。`archived_at` が入り、`GET /projects` と `GET /tasks` から既定で消える |
| `unarchive` | `archived_at` が消える |

- **中止で止まる run**: タスクが `running` でなくなるので、次の tick でディスパッチャが気付き、
  ADR-0044 §5 Phase 53 追記の**統一された止め方**（ワーカーの**プロセスグループ**へ SIGTERM →
  `kill_grace_secs` → SIGKILL）で止める。worktree とブランチはその後の掃除（ADR-0043 D2）が消す。
- **タスクに残るもの**: `Event::Transitioned{to: "cancelled", reason: "project_cancelled" | "milestone_cancelled"}`
  （「自分が止められたのか、上ごと止まったのか」がタイムラインで読める）。案件・途中目標そのものには
  イベント表を作らない（`updated_at` だけが動く）。**通知は作らない**（ADR-0044 D8）。
- **dispatch の抑止の範囲**: `paused` / `cancelled` / アーカイブ済みの案件と、`paused` / `cancelled` の
  途中目標に属するタスクは `TaskStore::ready_tasks` から外れる。計画・レビュー・まとめ・報告の圧縮といった
  **裏方の run も同じ `tasks` の行**なので、この 1 か所で全部止まる。
  **例外は対話（`Task.conversation`）**: 止まっている案件でも人が秘書と話せるよう、対話タスクだけは起きる。
- 知らない id（ULID でない形も含む）は 404 `project_not_found` / `milestone_not_found`。
  いまの状態でできない操作は 409 `invalid_transition`（`trigger` に `project_pause` などが入る）:
  **終端の** `cancel`、終端・一時停止中の `pause`、`paused` でないものの `resume`、非終端の案件の `archive`。
  終端は案件が `done` / `cancelled`、**途中目標が `reached` / `redesigned` / `cancelled`**
  （達成・再設計の記録は止められないし畳めない）。
  **`archive` / `unarchive` は冪等**（既にその状態なら 200 でそのまま返す。GUI の二度押しを 409 にしない）。

#### 3.84〜3.88 `POST /projects/{id}/{cancel|pause|resume|archive|unarchive}` → 200 `ProjectLifecycle`

```json
{"project": {"id": "01J...", "title": "Pluvio の新テーマ", "request": "…", "status": "cancelled",
             "created_at": "…", "updated_at": "…"},
 "cancelled_tasks": [{"id": "01J...", "title": "調査", "kind": "execute", "status": "cancelled", "actions": []}],
 "cancelled_milestones": ["01J..."]}
```

- `cancelled_tasks` / `cancelled_milestones` は **`cancel` のときだけ**中身が入る（他は空配列）。
- `project.paused_from` は `pause` の後だけ出る（`resume` で消える）。`project.archived_at` は
  `archive` の後だけ出る。

#### 3.89〜3.91 `POST /milestones/{id}/{cancel|pause|resume}` → 200 `MilestoneLifecycle`

```json
{"milestone": {"id": "01J...", "project_id": "01J...", "seq": 2, "title": "統合・選定",
               "description": "", "status": "paused", "paused_from": "in_progress",
               "created_at": "…", "updated_at": "…"},
 "cancelled_tasks": []}
```

- 案件の状態は変えない（途中目標だけ）。`cancelled_tasks` は `cancel` のときだけ。
- **`PATCH /projects/{id}` / `PATCH /milestones/{id}` からは `paused` / `cancelled` を入れられない**
  （422。`paused_from` が空になり連鎖も起きないため）。GUI の「状態を直接変える」プルダウンからも
  この 2 つは外してある。

---

### 3.92〜3.97 文書（ADR-0044 D7、Phase 57。**81〜86。変更系は管理系: `token_file` 未設定でも 401**）

**正本は git のファイル**（DB には何も持たない）。案件の文書の根は

1. その案件の **primary リポジトリ**（`project_repos.is_primary`。ADR-0043 D1）が `kind = "git"` で
   **手元（`location.kind = "local"`）**にあれば、その `.config/celeris/workspace.toml` の
   `[outputs] docs`（既定 `docs`）
2. primary が `dir`、または案件にリポジトリが無ければ **`POST /projects/{id}/docs/init`（§3.94）で
   `~/workspace/<案件 slug>/` に作る**（`git init -b main` + `docs/README.md` の最初のコミット）。
   作ったら primary の `git` リポジトリとして登録する

見えるのは **`<docs>/**/*.md` の default_branch の中身**（人の作業ツリーの未コミットの変更は出ない）。
`path` は**リポジトリ相対**（`docs/research/xxx.md`）で、文書の根で始まっていなければ根の下だと解釈する
（`?path=research/xxx.md` も同じページ）。`..`・絶対パスは **403 `path_forbidden`**、`.md` で終わらない
パスは **422 `validation`**。文書の根がまだ無い案件の**読み取り**は **409 `docs_unavailable`**
（読み取りは何も作らない。作るのは管理系の §3.94 / §3.95 / §3.97 だけ）。

celeris はここで **`git` だけ**を、待ち時間の上限付きで起こす（読み取り 30 秒、書き込み 300 秒。§3.79 と同じ
枠組み）。LLM もワーカーも起こさない。人の編集は**一時 worktree でコミットして default_branch を
fast-forward** する（ADR-0043 D5 の `merge` と同じやり方）。author / committer は
`Celeris (human) <celeris@local>`。

組織の「人」（ワーカー）にはこの経路を**出していない**。ワーカーは自分の worktree のブランチに
`docs/` を書き、人が「変更」タブ（§3.81）で取り込む。

#### 3.92 `GET /projects/{id}/docs` → 200 `DocsTree`

- `root`（文書の根）・`repo`（リポジトリの名前）・`default_branch` と、ページの一覧（パスの昇順、最大 500。
  超えたら `truncated: true`）
- 1 件ごとに `path` / `title`（front matter の `title` → 1 行目の `# ` → ファイル名）/ `updated_at` /
  `last_commit`（`sha` / `at` / `author` / `subject`）
- `?q=` があれば **`git grep -i -l -F`**（大文字小文字を区別しない固定文字列。正規表現ではない）で絞る
- 無い案件は 404 `project_not_found`、文書の根が無ければ 409 `docs_unavailable`

```json
{"project_id": "01J...", "repo": "benchfs", "root": "docs", "default_branch": "main", "truncated": false,
 "items": [{"path": "docs/research/fs.md", "title": "調べたこと", "updated_at": "2026-09-19T11:00:00Z",
            "last_commit": {"sha": "…", "at": "2026-09-19T11:00:00Z", "author": "Celeris (human)",
                            "subject": "docs: docs/research/fs.md"}}]}
```

#### 3.93 `GET /projects/{id}/docs/page?path=` → 200 `DocPage`

- `raw`（front matter を含む Markdown のもと）、`html`（**サーバで描画**。`pulldown-cmark`、表・脚注・
  打ち消し線あり、**生 HTML は捨てる**）、`title` / `tags[]` / `tasks[]`（front matter）、
  `history`（直近 20 件、新しい順）、`etag`（**blob の sha**）
- 本文中の `celeris:task/<ULID>` は `/tasks/<ULID>` に、`[[相対パス.md]]` は文書タブへのリンクに開く
  （根の外に出るものはリンクにしない）
- 512 KiB を超えるページは `too_large: true` で `raw` / `html` が空
- 無いページは 404 `page_not_found`

#### 3.94 `POST /projects/{id}/docs/init` → 200 `DocsInitResult`（**管理系**）

- 本文は空の JSON（`{}`）でよい
- 文書の根が既にあれば**何もせず** `created: false`（その根を返す）
- 無ければ `~/workspace/<案件 slug>/` に作る（slug は案件の題名の ASCII 化。作れなければ案件の id）。
  既に同じ名前の git リポジトリがあればそれを使い、**中身は触らない**
- `$HOME` が分からない・`git` が動かない・置き場が空でないときは 409 `docs_unavailable`

#### 3.95 `PUT /projects/{id}/docs/page` → 200 `DocPageResult`（**管理系**）

```json
{"path": "docs/research/fs.md", "body": "# 調べたこと\n…", "etag": "<blob sha>", "message": "docs: 直した"}
```

- `etag` は **§3.93 が返した値**。ページが既にあるのに `etag` が無い / 違えば **409 `etag_mismatch`**
  （`etag` 拡張フィールドにいまの値が載る）。新しいページは `etag` を付けない
- `message` の既定は `docs: <path>`
- **人のチェックアウトが default_branch を出していて未コミットの変更があれば 409 `default_branch_busy`**
  （ADR-0043 D5 と同じ規則。何も触らない）
- 中身が同じなら新しいコミットを作らず `unchanged: true` を返す
- 文書の根が無い案件では**その場で作る**（§3.94 と同じ）

#### 3.96 `DELETE /projects/{id}/docs/page?path=&etag=` → 200 `DocPageResult`（**管理系**）

- `etag` の規則は §3.95 と同じ（無い / 違えば 409 `etag_mismatch`）。消えたページは 404 `page_not_found`
- 応答は `deleted: true`、`etag: null`

#### 3.97 `POST /tasks/{id}/artifacts/promote` → 200 `DocPageResult`（**管理系**）

```json
{"name": "answer.md", "path": "docs/research/fs.md", "title": "調べたこと", "overwrite": false}
```

- `name` はそのタスクの成果物（`ArtifactProduced` の `name`。同じ名前が複数あれば**いちばん新しいもの**）。
  無ければ 404 `artifact_not_found`、UTF-8 で読めなければ 422 `validation`
- 中身の先頭に front matter を**混ぜて**コミットする（`title` と `tasks: [<タスク id>]`。既にある
  front matter の `tags` は残し、同じタスクは 2 回足さない）
- 宛先が既にあれば **409 `page_exists`**（`overwrite: true` なら上書き）
- 案件に属さないタスクは 409 `docs_unavailable`、知らないタスクは 404 `task_not_found`
- コミットの規則（一時 worktree・fast-forward・`default_branch_busy`）は §3.95 と同じ
- 昇格したページはそのタスクの `GET /tasks/{id}/timeline` に `kind = "doc"` として出る（逆リンク）

### 3.98〜3.100 Console（ADR-0048 D1/D2、Phase 60a。**87〜89。すべて読み取り**）

Console は「全案件の流れが一本で見える画面」。celeris は**正規化したブロック**だけを返し、GUI は
イベントの種類やアダプタごとの差を知らない。**読み取りだけ**で状態は変えない。入力（`POST /console/instruct`）と
CoS の `actions` は ADR-0048 D3（Phase 60b。§3.107）。

#### 3.98 `GET /console?scope=&since=&limit=` → 200 `ConsolePage`

| クエリ | 既定 | 説明 |
|---|---|---|
| `scope` | `all` | `all` / `project:<ULID>` / `node:<org ノード id>`。形が違えば 400 |
| `since` | – | 前回の `next_cursor`（**不透明な文字列**。GUI は中身を解釈しない）。形が違えば 400 |
| `limit` | 100 | 1〜**200**（超えたら 200 に丸める。0 は 400） |

- `items` は**時刻の昇順**（新しいものが最後。GUI は下に足す）。
- `since` **無し**は「いちばん新しい `limit` 件」、`since` **有り**は「そのカーソルより後の `limit` 件」。
  同じブロックを 2 回返さない。
- `next_cursor` は最後のブロックの位置。1 件も無ければ渡した `since` をそのまま返す（増分取得に使える）。
- 引きはすべて上限付き（`events` は 1 回 4,000 件の窓、対話・認可・報告は `limit × 4`（最大 800））。
  窓より古いものは `GET /tasks/{id}/events` や `GET /reports` で見る。

ブロックは `kind` で 9 種（ADR-0048 D1 の 8 種 + ADR-0047 D5 の `knowledge`。Phase 62 で埋まった）。
どれも `at`（RFC 3339）と `cursor` を持つ:

| `kind` | 中身 | 由来 |
|---|---|---|
| `human` | 人の発言（`text` / `node_id` / `project_id` / `task_id`） | `messages`（`role = user`） |
| `reply` | CoS・部署ノードの返事（Markdown。`run_id` 付き）。CoS が `actions`（§3.107）を宣言していれば `actions_result`（`MessageMetadata`: `actions_executed[]` / `actions_failed[]`） | `messages`（`role = node`） |
| `task` | 開始・終了・失敗・中止・割り込みの 1 行（`task`: `from` / `to` / `reason` / `assignee` / `harness` / `tier` / `mode` / `elapsed_secs`） | `Event::Transitioned` |
| `progress` | run ごとに束ねたワーカーの進行。`progress`: `run_id` / `count` / `tool_count` / `last_status` / `started_at` / `updated_at` / `first[]` / `last[]` / `truncated`。見出し用に `title` / `assignee` / `harness` / `tier` | `Event::WorkerProgress`（ADR-0048 D2 の正規化） |
| `question` | ディスパッチャの質問（`text` / `answered` / `answer` / `run_id`） | `Event::QuestionRaised` + `Event::Answered` |
| `approval` | 認可 1 件（`Approval` をそのまま。`decision` / `answer` / `decided_at` 付き） | `approvals` |
| `milestone` | 途中目標の提案（`proposed` のものだけ）と秘書のレビューの返事 | `milestones` + `messages` |
| `report` | 報告（見出しと本文） | `reports` |
| `knowledge` | 知識整理 run の結果（ADR-0047 D4/D5、Phase 62）。`task_id` / `task_title` / `run_task_id` / `state`（`applied` \| `failed`。`scheduled` は出ない）/ `ingested` / `inbox` / `discarded` / `via`（ADR-0052 D2、Phase 64。`"langmem"` \| `"fallback:<adapter>"`。GUI は後者に「cheap のハーネスで抽出」を出す） | `knowledge_runs`（`state != scheduled`）+ 元のタスク |

範囲の効き方:

- `project:<id>`: そのタスク（`tasks.project_id`）・その案件についての対話（`messages.project_id`）・
  報告（`reports.project_id`）・その案件の途中目標。
- `node:<id>`: そのノードが担当のタスク（`tasks.assignee`）・そのノードとの対話（`messages.node_id`）・
  そのノードの報告（`reports.node_id`）。**途中目標は出ない**（途中目標は案件のもの）。
- 同じ質問が `approvals` にもあるときは **`approval` の側だけ**出す（同じことを 2 回出さない）。

`progress` の `first[]` / `last[]` は折り畳みの見出し用に**始めの 3 行と終わりの 3 行**だけ
（`truncated = true` なら間が省かれている）。全行は §3.100 で取る。1 行は
`{at, seq, kind?, tool?, text, error?}` で、`text` は `summary` があればそれ、無ければ `msg`。

#### 3.99 `GET /console/stream?scope=&since=`

`GET /stream` と同じ枠組み（認証・同時接続数 16・`heartbeat`・停止で閉じる）。`scope` は §3.98 と同じ。

```
event: hello
data: {"cursor":"00001789…000.12345.e12345","scope":"all","now":"…"}

event: console.block
data: {…§3.98 のブロック 1 件…}

event: heartbeat
data: {"now":"…"}
```

- `since` を渡さなければ**「今」から**（履歴は §3.98 で取る）。
- `progress` は **run ごとに 1 秒に 1 回まで**にまとめて流す（1 回の道具呼び出しごとにフレームが飛ばない）。
  流れてくる `progress` ブロックは**その run の積み上げ**（`count` / `tool_count` はその接続で見た合計）。
- `task` / `human` / `reply` / `question` / `approval` / `milestone` / `report` はまとめずにそのまま流す
  （対話・認可・報告は 1 秒ごとに見に行く）。
- `Last-Event-ID` は使わない（`id:` を付けない。再開は `since` で行う）。

#### 3.100 `GET /tasks/{id}/runs/{run_id}/events?after_seq=&limit=` → 200 `EventsPage`

折り畳んだ `progress` を開いたときに取る、その run の**全行**。`GET /tasks/{id}/events` の run 絞り込み版で、
`run_id` を持つイベント（`worker_started` / `worker_progress` / `artifact_produced` / `worker_finished` /
`review_verdict` / `question_raised` / `delegated`）だけを `seq` 昇順で返す（遷移は run に紐づかないので入らない）。
`limit` は既定 500・最大 5,000。知らないタスクは 404 `task_not_found`。

`worker_progress` の行には ADR-0048 D2 の `kind` / `tool` / `summary` / `detail` / `truncated` / `error` が
**あれば**入っている（付けないワーカー・導入前のイベントには無い）。

### 3.101〜3.106 知識ベース（ADR-0047、Phase 61。**90〜95。変更系は管理系: `token_file` 未設定でも 401**）

**正本は `[knowledge] root`（既定 `~/.local/share/celeris/knowledge`）の Markdown**（DB には何も持たない）。案件の文書（§3.92〜3.97）と
同じ流儀だが、**正本は作業ツリーのファイルそのもの**なので、読み取りは常にファイルを読む（人が編集中の未コミットの
変更もそのまま見える）。書き込みは作業ツリーに書いてから**そのパスだけを 1 件 1 コミット**する。
author / committer は `Celeris (human) <celeris@local>`（`celerisctl knowledge record` が作る候補だけ
`Celeris (knowledge) <celeris@local>`）。

- `path` は **KB の根からの相対**（`environment/clusters/pegasus.md`）。`..`・絶対パスは **403 `path_forbidden`**、
  `.md` で終わらないパスは **422 `validation`**
- `etag` は**ページの中身の sha256**（文書の blob sha とは別物。GUI は opaque な値として扱う）
- `[knowledge] root` が設定されていなければ **409 `knowledge_unavailable`**。設定されていてもディレクトリが
  まだ無ければ、**読み取りは `initialized: false` を返すだけ**（何も作らない）、変更系は
  409 `knowledge_unavailable`（用意するのは `celerisctl knowledge init` だけ）
- 書き込みのあとは必ず `index.json` を作り直す（索引は**再生成できる派生物**。バージョン管理には入らない）
- `_inbox/` の候補は**索引にも検索にも出ない**。人が §3.105 / §3.106 で accept / reject する

組織の「人」（ワーカー）はこの HTTP ではなく **`celerisctl knowledge`**（ADR-0047 D3）で KB を読む。
前置きには索引（`path` / `title` / `tags`、最大 200 件）だけが載り、本文は載らない。

#### 3.101 `GET /knowledge/tree?scope=&q=` → 200 `KnowledgeTree`

- `root`（絶対パス）・`initialized`・`generated_at`（`index.json` を作った時刻）・`inbox_count`
- `items`: `path` / `title` / `tags[]` / `scope` / `sources[]` / `updated` / `confidence`
  （`?q=` が無ければパスの昇順、最大 500。超えたら `truncated: true`）
- `scopes`: **ページのあるディレクトリ**の一覧（`items[].path` の親ディレクトリを重複無し・名前順に並べたもの。
  画面のツリーの見出し）。front matter の `scope`（`project:pluvio` のようなラベル）はここには入らない
  — それは `items[].scope` の方。`?scope=` / `?q=` で絞っても **`scopes` は KB 全体のまま**（見出しは動かない）
- `?scope=` は **KB の相対パスの接頭辞**（`user` / `environment/clusters` / `projects/pluvio`）か
  front matter の `scope` の値（`project:pluvio`）
- `?q=` があれば **索引の `tags` / `title` と本文の全文一致**で絞り、
  **tag 一致数 → title 一致数 → 本文一致 → `updated` の新しい順**に並べる（ADR-0047 D3）

```json
{"root": "/home/u/knowledge", "initialized": true, "generated_at": "2026-09-20T01:00:00Z",
 "inbox_count": 2, "truncated": false, "scopes": ["environment/clusters", "user"],
 "items": [{"path": "environment/clusters/pegasus.md", "title": "pegasus の使い方",
            "tags": ["environment", "cluster", "pegasus"], "scope": "environment",
            "sources": ["human"], "updated": "2026-09-20", "confidence": "low"}]}
```

#### 3.102 `GET /knowledge/page?path=` → 200 `KnowledgePage`

- `raw`（front matter を含む Markdown のもと）、`html`（**サーバで描画**。`pulldown-cmark`、
  **生 HTML は捨てる**）、`title` / `tags[]` / `scope` / `sources[]` / `confidence` / `updated`、
  `history`（直近 20 件、新しい順）、`etag`
- 本文中の `celeris:task/<ULID>` は `/tasks/<ULID>` に、`[[相対パス.md]]` は `/knowledge?path=…` に開く
- 512 KiB を超えるページは `too_large: true` で `raw` / `html` が空
- 無いページは 404 `page_not_found`

#### 3.103 `PUT /knowledge/page` → 200 `KnowledgePageResult`（**管理系**）

```json
{"path": "environment/clusters/pegasus.md", "body": "---\ntitle: …\n---\n…", "etag": "<sha256>", "message": "knowledge: 直した"}
```

- `etag` は **§3.102 が返した値**。ページが既にあるのに `etag` が無い / 違えば **409 `etag_mismatch`**
  （`etag` 拡張フィールドにいまの値が載る）。新しいページは `etag` を付けない
- `message` の既定は `knowledge: <path>`
- 中身が同じなら新しいコミットを作らず `unchanged: true`
- `_inbox/` のパスは **403 `path_forbidden`**（候補は §3.105 / §3.106 で扱う）

#### 3.104 `GET /knowledge/inbox` → 200 `KnowledgeInbox`

- `items`: `id`（`_inbox/<id>.md` のファイル名から `.md` を取ったもの）/ `path` / `title` / `tags[]` /
  `scope` / `sources[]` / `confidence` / `created`（front matter のまま。RFC 3339 か `YYYY-MM-DD`）/ `body` / `html` /
  `target`（取り込み先。front matter の `path`、無ければ `scope` と題名からの既定）/ `target_exists`
- 新しい順（id の降順 = 記録した時刻の降順）

#### 3.105 `POST /knowledge/inbox/{id}/accept` → 200 `KnowledgePageResult`（**管理系**）

```json
{"path": "environment/clusters/pegasus.md", "overwrite": false}
```

- 本文は省略してよい（`{}` か空）。`path` を渡せばそこへ、渡さなければ候補の `target` へ移す
- `_inbox/` から消して宛先に書き、**両方のパスを 1 コミット**にする。`path:`（候補専用の鍵）は落ち、
  `updated` は今日になる
- 宛先が既にあれば **409 `page_exists`**（`overwrite: true` なら上書き）
- 知らない id は 404 `candidate_not_found`、`/` や `..` を含む id は 403 `path_forbidden`

#### 3.106 `POST /knowledge/inbox/{id}/reject` → 200 `KnowledgeRejectResult`（**管理系**）

- `_inbox/<id>.md` を消してコミットする（履歴には残る）。応答は `{"id": "…", "sha": "…"}`
- 知らない id は 404 `candidate_not_found`

### 3.107 `POST /console/instruct`（ADR-0048 D3、Phase 60b。**96。管理系**）→ 202 `ConsoleInstructAccepted`

Console の入力欄の文の入口。`POST /org/{id}/messages`（§3.47）と同じ経路に載せるだけで、返事は待たない
（`GET /console` / `GET /console/stream` で拾う）。

```json
{"text": "@software-engineering このバグを直して", "scope": null}
```

`InstructBody`: `text`（必須。空白だけは 422 `validation`）、`scope`（省略可。`all` / `node:<id>` /
`project:<id>`。形が違えば 400 `bad_request`）。

- **相手の決め方**（決定的。LLM は関与しない）:
  1. `scope = node:<id>` → そのノード。
  2. それ以外で `text` が `@<node-id> `（`@` の直後に空白でない 1 語、その後ろに空白）で始まる → その
     ノード。**`@<node-id> ` は送る本文から取り除く**（残りの本文だけがそのノードへの発言になる）。
  3. `scope = project:<id>` で `@mention` が無い → **CoS**（組織の根。`OrgKind::Secretary`）への対話を
     その案件に紐づける（`Message.project_id` / `Task.project_id` がその案件）。
  4. それ以外（省略・`all`、`@mention` 無し）→ CoS への対話（案件に紐づかない）。
- ノード宛て（1・2）は知らない id なら 404 `org_node_not_found`。CoS 宛て（3・4）は組織に CoS
  （`OrgKind::Secretary` のノード）が無ければ 404 `org_node_not_found`（`id: "cos"`）。
- 応答 `ConsoleInstructAccepted`: `message_id`（入った `role = "user"` の行）、`task_id`（起こした対話用
  タスク）、`node_id`（実際に話しかけた相手。ノード宛てならそのノード、CoS 宛てなら CoS の id）。
- CoS への対話 run の前置きには、通常の CoS 対話（§3.47 のノード一覧）に加えて**進行中の案件とその途中目標**
  （`proposed` / `active` の案件だけ）と、`actions` の宣言の仕方（JSON の形と目安）が入る。CoS が結果ファイルに
  `actions`（`create_task` / `propose_project` / `add_milestone` / `ask_human`）を宣言すると、taskd が**決定的に**
  実行する（LLM はしない）:
  - `create_task`: **`ready`**（人の Go 済み）のタスクを作る。`assignee` を省けば次 tick の matching（ADR-0046 D5）が
    決める。`project` / `milestone` は id（省略可）、`harness` はハーネス id（`[[harnesses]]` を設定していれば検証）、
    `acceptance` は 1 件以上必須。
  - `propose_project`: `proposed` の案件を作る。`repos[]` は**絶対パス**（1 件目が primary。名前・種類は
    `POST /projects/{id}/repos` と同じ既定から決める）。
  - `add_milestone`: 既存の案件の末尾に `proposed` の途中目標を足す。
  - `ask_human`: taskd 側では何も作らない（人への問いかけ自体が返事の本文）。「実行できた」として記録するだけ。
  - 検証に落ちた action（知らない harness / repos / 案件など）は**実行されない**。CoS の返事の Markdown に
    「実行できなかった action: …」の節が付き、`reply` ブロックの `actions_result.actions_failed[]` にも理由が残る。
  - 実行結果は `Message.metadata`（`MessageMetadata`）に残り、`GET /console` の `reply` ブロックが
    `actions_result` として運ぶ（§3.98 の表）。**同じ run の actions は 1 回だけ実行される**（冪等）。

### 3.108 `GET /llm/sources`（ADR-0053 D4、Phase 65/66。**97**）→ 200 `LlmSourcesView`

LLM source のローカル OpenAI 互換プロキシ（`crates/llm-proxy`。`127.0.0.1:18100`、`/api/v1` の外）が
使っている供給元の観測。判断（選択・cooldown）はプロキシの中で決定的に行われる。ここは**見えるように
するだけ**（GUI の表示は `/accounts` の「LLM source」節、Phase 66）。

- 認証は必要（読み取り専用だが Bearer 必須。トークン不要の `GET /health` とは違う）。
- `[llm_proxy]` が無効（`enabled = false`、または `claude_oauth`/`codex_oauth`/`openai_compatible` が
  1 つも無い）なら **409 `llm_proxy_unavailable`**。

```json
{"sources": [
  {"id": "claude-oauth", "kind": "claude-oauth", "enabled": true,
   "accounts": [{"id": "acct-a", "logged_in": true, "remaining": 0.62,
                 "remaining_short": 0.62, "remaining_long": 0.81}],
   "last_hour_requests": 12, "last_hour_prompt_tokens": 3400, "last_hour_completion_tokens": 900},
  {"id": "openai-compatible:qwen", "kind": "openai-compatible", "enabled": true, "reachable": true,
   "accounts": [], "last_hour_requests": 40, "last_hour_prompt_tokens": 9000,
   "last_hour_completion_tokens": 5000}
],
"celeris_tiers": [
  {"tier": "frontier", "resolves_to": "claude-oauth"},
  {"tier": "standard", "resolves_to": "claude-oauth"},
  {"tier": "cheap", "resolves_to": "openai-compatible:qwen"}
]}
```

- `sources[].id`: `claude-oauth` / `codex-oauth` / `openai-compatible:<id>`（設定の `[[llm_proxy.sources.openai_compatible]] id`）。
- `reachable`: `openai-compatible` だけ `GET <base_url>/models` の probe 結果（60 秒キャッシュ）。
  `claude-oauth`/`codex-oauth` は到達性ではなくアカウントの残量で見るので、フィールド自体が省略される
  （`null` を書かない。省略 = 「この供給元には意味が無い」）。
- `accounts[].remaining`: 0.0〜1.0。短期・長期のうち**厳しい方**（残りが少ない方）。測れないとき
  （観測が古い・無い）はフィールドを省略する（値を捏造しない。ADR-0024 D3 と同じ規律）。
  `cooldown_until`/`cooldown_reason` も 429/401 を受けた直後だけ載る（このアカウントプールは CLI
  ワーカーの dispatch と**同じ帳簿**を共有するので、`GET /accounts` の cooldown とも一致する）。
- `accounts[].remaining_short` / `remaining_long`（ADR-0053 D4、Phase 66）: 短期枠（Claude の 5 時間 /
  Codex の週内相当）・長期枠（7 日）それぞれ単独の残り。`remaining` と同じ「測れないときは省略」の規律。
- `last_hour_*`: `llm_proxy_requests`（migration 0022）の直近 1 時間の集計。本文は記録しないので
  ここにも出ない。
- `celeris_tiers[]`（ADR-0053 D4、Phase 66）: `celeris/<tier>` が**今**どこに解決するか（`server.rs` の
  実際の選択と同じ決定的な計算を、副作用なしでなぞるだけ）。`resolves_to` は `sources[].id` と同じ形。
  選べる候補が無ければ `null`（`no_source_available` になる状態）。古いスナップショットには無いので
  省略時は空配列として扱う。

### 3.109 `POST /console/new-conversation`（ADR-0054 D1、Phase 67。**98。管理系**）→ 204

CoS の**継続セッション**（`node_sessions`。§3.107 の対話 run が `--resume` 等で続けているもの）を捨てる。
GUI の「新しい会話」ボタンの入口。**薄い**: ディスパッチャには触らず、ストアの `node_sessions.retired_at`
を立てるだけ（DESIGN 原則 1）。

- 要求本文は無い（`{}` を送っても無視される）。
- 現役セッションが有っても無くても **204**（結果として「捨てた」状態にするだけなので、既に無かったことを
  エラーにしない）。
- 効果: 次に人が CoS（`GET /console` の `scope=all` / `project:<id>`。§3.98）に話しかけたときの対話 run は
  **新規セッション**（前置きは全量。§3.107 の「進行中の案件」等をもう一度渡す）から始まる。捨てなければ、
  逼迫（`[sessions] rollover_tokens`）かアカウント変更が起きるまで、前置きは差分だけ（`session_diff`）が
  続く。
- 部門長（engineering/research/operations の根ノード）のレビュー・切り分け run（ADR-0051）の継続セッション
  （`kind = lead`）はこの API の対象外（部署ごとに 1 本、GUI からの操作は今回のスコープに無い。組織画面の
  「継続中のセッション」表示は Phase 68 の D3）。

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
| 終了 | クライアントが切ればサーバは即座に購読を解除する。celeris の停止時は接続を閉じる |

クライアント（BFF）の規約: `task.event` を受けたら該当画面のデータを**再取得**する（イベント本体から状態を組み立てない。真実は DB）。`celerisctl` による書き込みも同じ経路で流れる（in-process 通知は使わない。ADR-0013 D6）。

---

## 5. 派生値の計算規則（task-ops / task-api）

全て **`crates/task-ops`** の関数として実装し、`celerisctl show --json` / `celerisctl` の各コマンド / `task-api` / ディスパッチャが同じ関数を使う。GUI は結果を表示するだけ。

### 5.1 受信箱（`task_ops::inbox(store, snapshot: Option<&DaemonSnapshot>, now)`）

| 区画 | 抽出 | 各項目の埋め方 | 並び |
|---|---|---|---|
| `approvals[]` | `kind == approval && status == ready` | `parent` = `parent_id` のタスク（無ければ `null`）。`criterion_idx` / `attempt` は title を `Approval needed: <title> — criterion <idx> (attempt <n>)` として解析（`task_ops::parse_human_approval_title`。ディスパッチャの `human_approval_title` と対）。解析できなければ `null`。`criterion_text` = 親の `acceptance[idx].text`（無ければ approval の `objective`）。`requested_at` = `ApprovalRequested` の `ts`（無ければ `created_at`）。`last_run` = 親の `last_run_id` の `RunSummary`。`evidence` = `<ws>/runs/<run_id>/result.json` が `done` なら `evidence[]`（task-api が読む。読めなければ `[]`）。`other_verdicts` = 親の同 run の `ReviewVerdict`。`artifacts` = `artifacts_for_run`。`previous_decisions` = 親の他の Approval 子（同じ `criterion_idx`）の `ApprovalDecided` | `requested_at` 昇順 |
| `questions[]` | `status == blocked` | `question` = §5.5。`asked_at` = その `WorkerFinished`（または `QuestionRaised`）の `ts`。`run_id` = 同。`previous` = `answers_from_events`（`AnswerNote` の履歴） | `asked_at` 昇順 |
| `drafts[]` | `status == draft` を `parent_id` でまとめる | `parent` = Plan 等（`null` = 根）。`plan_summary` = 親の直近 `WorkerFinished.outcome` が `done: ` 始まりならその後ろ。`drafts` = `TaskSummary` | 親の `created_at` 昇順、根は最後 |
| `attention[]` | (a) `failed` かつ `updated_at >= now − 24h`、(b) `ready && max_requeues > 0 && consecutive_requeues > 0 && consecutive_requeues >= max_requeues − 1`（一度も requeue していないものは含めない）、(c) スナップショットの `unroutable`、(d) `WorkspaceSpec::Remote` で終端でないタスクの `ClusterUnavailable` が `ts >= now − 24h` にあるクラスタ（**クラスタごとに 1 件**。スナップショットがあり `clusters[].connected == true` なら出さない — 接続が戻っていれば用済み。Phase 12） | (a) `reason` = 直近 `WorkerFinished.outcome` と、直近 run の fail の `ReviewVerdict.reason` を（あるものだけ）`; ` で結合。(b) `count` / `max`。(c) `hint` = `worker_hint`、`at` = スナップショットの `last_tick_at`。(d) `cluster` / `host`（イベントの `host`。空なら `clusters[].host`）/ `at` = 最新の `ts` / `tasks` = 該当タスク数。GUI は「`scripts/cluster-login.sh <host>` でログインし直してください」と出す | `at` 降順 |
| `counts` | 上の件数 + `count_by_status()` | `drafts` は **draft タスクの件数**（グループ数ではない）。他は各区画の要素数 | |

### 5.2 run の要約（`task_ops::runs(events) -> Vec<RunSummary>`）

`RunOutcomeKind` は `done` / `question` / `error` / `requeue` / `lease_expired` に加えて
**`interrupted`**（ADR-0044 D2: `outcome` が `interrupted: ` で始まる = 人のコメントで止めた run）。
`interrupted` は失敗ではないので、`bad_news`（ADR-0034）にも `error_cooldown`（§5.8 の `error`）にも
数えない。

- `WorkerStarted{run_id, adapter, model, provider}` で開始（`started_at` = `ts`）。同じ `run_id` の `WorkerProgress` を `progress` に数え、`ArtifactProduced` を `artifacts` に数え、`ReviewVerdict` を `verdicts` に数える。
- `WorkerFinished{run_id, outcome, usage}` で終了（`finished_at` = `ts`）。`outcome` の分類（`RunOutcomeKind`）は**接頭辞**で決める（ディスパッチャの文字列と対）:
  - `done: ` → `done`（`outcome_text` = 後ろの summary）
  - `question: ` → `question`
  - `requeue: ` → `requeue`
  - `lease_expired`（完全一致）→ `lease_expired`
  - `interrupted: ` → `interrupted`（ADR-0044 D2。人のコメントで止めた run。失敗ではない）
- それ以外（`error(retryable=…): …`）→ `error`
- `WorkerFinished` が無い run は `finished_at = null, outcome = null`（実行中、または回収前）。
- Reviewer run も `WorkerStarted` / `WorkerFinished`（`role: "reviewer"`）を持つので一覧に現れ、`RunSummary.role` が `reviewer` になる（ADR-0014 D1。`role` の無いイベントは `worker`）。Reviewer run の進捗（`WorkerProgress`）は従来どおり対象 run に `reviewer run <id>: ` 接頭辞で付き、`reviewer run requeued: ` で始まるものは対象 run の `RunSummary.reviewer_deferrals` に数える。Reviewer run の `outcome` もワーカー run と同じ接頭辞の規則。

### 5.3 タイマー（`task_ops::timers(task, events, config, now)`）

- `lease_expires_at` = `task.lease.expires_at`（`running` のとき。`renew_lease` はイベントを出さないので、GUI は `running` の詳細を 5 秒ごとに再取得する）。
- `backoff_until` = `status == ready && attempts > 0` のとき `updated_at + retry_backoff(base, max, attempts)`（`min(base·2^(attempts−1), max)`。`base = 0` なら `null`）。過去なら `null`。
- `consecutive_requeues` / `consecutive_reviewer_requeues` は `task_ops::derive` に移動済み（Phase 9a。規則は不変: `Transitioned` を新しい順に見て `requeue` を数え `dispatch` は読み飛ばす / 最後の `Transitioned` 以降の `REVIEWER_REQUEUED_PREFIX` を数える）。`retry_backoff` / `artifacts_for_run` / `last_run_id` / `human_approval_title` / `approval_decision_note` / `latest_question` / `prior_review_from_events` / `answers_from_events` も同じモジュールにある。
- `max_requeues` は設定値。

### 5.4 可能な操作（`task_ops::actions(task) -> Vec<Action>`）

`approve`: `status == draft` または `kind == approval && status == ready`。`reject`: `kind == approval && status == ready`。`answer`: `status == blocked`。`cancel`: 非終端。`retry`（Phase 31。§3.63）: `status == failed` または `status == cancelled`。**`edit`（ADR-0044 D1。§3.74）: 非終端。`reopen`（ADR-0044 D2。§3.77）: `status == done` または `status == failed`**（`cancelled` には付かない）。

この結果は `TaskDetail.actions` だけでなく、**`TaskRef` と `TaskSummary` にも入る**（ADR-0015 D4）。受信箱・一覧・DAG・依存関係のどこから来た参照でも、GUI は `actions` を見るだけでよく、この規則を再実装しない。

### 5.5 質問文（`task_ops::latest_question(events)`）

`events_for` を後ろから見て最初に見つかった質問。次の 2 つを同じように扱う:
- `WorkerFinished{outcome}` が `"question: "` で始まるもの（ワーカーが聞いた。接頭辞を除いた文字列）
- `QuestionRaised{text}`（ディスパッチャが聞いた。ADR-0021 D2。委譲した子が失敗し、親がやり直せなかったとき）

無ければ空文字列。

### 5.6 検証（`task_ops::add::create_task` / `task_ops::plan::create_plan` の中）

文言は 3.4 / 3.14 のとおり（`OpsError::Validation`）。`celerisctl add` / `plan` も同じ関数を通す（Phase 9a で移行済み）。9b では**テーブル駆動テストを task-ops に置く**（GUI の G2 はこのテーブルを HTTP 越しに再確認するだけ）。

### 5.7 `TransitionResult.cascaded`（9b で task-ops の `TransitionResult{id, from, to, reason}` に追加。`#[serde(default)]`）

`apply_transition` の前後で `events_since` の増分を読み、遷移対象以外の `task_id` に付いた `Transitioned{to: cancelled}` を `TaskRef` にして返す（トランザクション前の最新 id を控え、後で `events_since(id)` を読む。同時に他の書き込みが挟まっても `reason` が `dependency_failed` / `cancel` のものだけを拾うので過剰には含まれない）。

### 5.8 プロバイダの集計（`task_api::stats`。task-ops ではなく task-api のメモリ）

- 起動時に `events_since(0, 5000)` を繰り返して全イベントを 1 回走査し、以後は SSE と同じポーリングループの増分で更新する。**メモリ内の観測値**で、真実ではない（再起動で再計算）。
- `WorkerStarted{run_id, provider}` で run 表に `provider`（`null` なら `"unknown"`）を登録し、`WorkerFinished{run_id, outcome, usage}` で閉じる。分類は §5.2。`input_tokens` / `output_tokens` は `usage` の和（`null` は 0）。
- `by_day` は `WorkerFinished.ts` の UTC 日付で直近 30 日。
- Reviewer run（`role: reviewer`）も同じ規則で集計に**含める**（ADR-0014 D1）。

---

## 6. 型

### 6.1 型の出所

| 出所 | 型 | 備考 |
|---|---|---|
| `task-core`（既存） | `Task`, `TaskId`, `TaskKind`, `Status`, `Tier`, `WorkerHint`, `WorkspaceSpec`, `Budget`, `Lease`, `Check`, `Criterion`, `ArtifactRef`, `Usage`, `Event` | serde 表現そのまま。`Event` は Phase 9a で `JsonSchema` を derive 済み（`until` は `#[schemars(with = "String")]`）。`ProviderThrottled.reason: Option<String>`（任意フィールド、語彙 `throttled \| auth_failed \| exhausted \| spawn`。ADR-0013 D9）。`Event::WorkerStarted.account: Option<String>`（Phase 13、ADR-0024 D4） |
| `task-core`（Phase 13、実装済み） | `RateWindow { utilization: f64, resets_at: i64 }`、`RateLimitObservation { five_hour, seven_day, status, resets_at, observed_at }` | `rate_limit_event`（claude-code）/ `token_count`（codex、`RateLimitObservation::from_codex_token_count`）の観測値（ADR-0024 D4、ADR-0025 D3）。`AccountUsageLive`/`AccountUsageView` はこれを壁時計に直したもの |
| `task-core`（Phase 14、ADR-0025） | `AccountAdapter { ClaudeCode, Codex }`（serde `"claude-code"` \| `"codex"`） | アカウントプールのアダプタの次元。`(adapter, id)` でアカウントを識別する |
| `task-core`（Phase 9a、実装済み。`genres` は Phase 16） | `EventRow { id: u64, task_id: TaskId, seq: u64, ts: String, event: Event }`、`ListFilter { statuses, kinds, genres, parent_id, root_only, text_contains }`、`ListOrder { Dispatch, UpdatedDesc, CreatedDesc }`、`Page<T> { items, next_cursor, total }`、`SCHEMA_VERSION` | `events_since` / `list_page` / `count_by_status` の型。`EventRow` は `docs/api/v1/event.schema.json` のルート |
| `task-ops`（Phase 9a、実装済み） | `add::{NewTaskSpec, CriterionSpec, create_task}`、`plan::{NewPlanSpec, create_plan}`、`gate::{TransitionResult, approve, reject, answer, cancel}`、`replay::{ReplayReport, ReplayMismatch, replay}`、`derive::{ReviewNote, AnswerNote, …}`、`OpsError` | 9b で `Deserialize` / `Serialize` / `JsonSchema` を付ける（`NewTaskSpec` / `NewPlanSpec` は `deny_unknown_fields` + `#[serde(default)]`、`CriterionSpec` は `tag = "type"`、`ReplayMismatch.field` は `&'static str` のまま文字列に出る） |
| `task-ops`（Phase 9b で追加） | `TaskRef`, `TaskSummary`, `TaskList`, `TaskDetail`, `Timers`, `CriterionView`, `VerdictView`, `RunSummary`, `RunFiles`, `RunOutcomeKind`, `ApprovalLink`, `ApprovalDecisionView`, `Action`, `Inbox`, `InboxCounts`, `ApprovalItem`, `EvidenceView`, `QuestionItem`, `DraftGroup`, `AttentionItem`, `Graph`, `GraphNode`, `GraphEdge`, `DelegatedView`（Phase 10）, `TransitionResult.cascaded`, `DaemonSnapshot`, `InFlight`, `InFlightKind`, `CooldownView`, `ProviderLive`, `AccountLive`（Phase 13、`adapter: String` を Phase 14 で追加）, `AccountUsageLive`, `AccountCooldownLive`（Phase 13） | ビュー型。全て `JsonSchema`。`DaemonSnapshot` は task-dispatch が作り task-api が読むので、両者が依存する task-ops に置く（ADR-0013 D3/D4 の依存方向を満たす）。`CooldownView` は `task_dispatch::policy::Cooldown`（`Instant`）を壁時計に直した写し |
| `task-core`（Phase 52、ADR-0043 D1/D2） | `ProjectRepo`, `RepoId`, `RepoKind`, `RepoRun`, `RepoSync`, `RepoRef`、`Task.repos: Vec<RepoRef>` | 案件のリポジトリ（`project_repos`）と、タスクが使うリポジトリ。`Task.repos` は空なら省略される（従来の応答と 1 バイトも変わらない） |
| `task-api`（Phase 9b） | `Health`, `DbInfo`, `Problem`, `ValidationError`, `DecisionBody`, `AnswerBody`, `CancelBody`, `EventsPage`, `RunList`, `ArtifactList`, `ArtifactView`, `Providers`, `ProviderView`, `ProviderStats`, `DailyUsage`, `DaemonView`, `ConfigView`, `ReviewerConfigView`, `ProviderConfigView`, `RoleConfigView`（Phase 10）, `GenreConfigView`（Phase 16）, `ApiConfigView`, `StreamHello`, `StreamHeartbeat`, `StreamReset`, `ApiV1Schema`、`RepoList`, `RepoCreateBody`, `RepoPatchBody`, `TreeView`, `TreeRepoView`, `TreeEntry`, `TreeFileView`（Phase 52、ADR-0043 D1/D6） | HTTP の要求・応答の包み。`POST /tasks` / `POST /plans` の本文は task-ops の `NewTaskSpec` / `NewPlanSpec` そのもの |
| `task-api`（Phase 13、ADR-0024） | `AccountList`, `AccountView`, `AccountUsageView`, `RateWindowView`, `AccountCooldownView`, `AccountStats`, `AccountCreateBody`, `AccountCheckResponse`, `AccountLoginStart`, `AccountLoginCodeBody`, `AccountLoginResult` | `GET/POST /accounts`・`DELETE /accounts/{id}`・`POST /accounts/{id}/check`・`POST`/`DELETE /accounts/{id}/login`・`POST /accounts/{id}/login/code` の要求・応答。Phase 14（ADR-0025）で `AccountList.roots: HashMap<String, Option<String>>`、`AccountView.adapter: String`、`AccountCreateBody.adapter: String`（既定 `"claude-code"`）、`AccountLoginStart.kind: String`（`"paste_code"` \| `"device_code"`）と `user_code: Option<String>` を追加（すべて既存フィールドはそのまま。追加のみ） |
| `task-core`（Phase 54、ADR-0043 D5） | `TaskIntegration`, `IntegrationId`, `IntegrationMethod`, `IntegrationState` | 変更の取り込みの記録（`task_integrations`。migration 0014、スキーマ版数 14） |
| `task-ops`（Phase 54、ADR-0043 D5） | `changes::{ChangedFile, DiffStat}` | `git` の出力を写しただけの値（`RepoChangesView` の中に入る） |
| `task-api`（Phase 54、ADR-0043 D5） | `ChangesView`, `RepoChangesView`, `ChangeDiffView`, `IntegrateBody`, `IntegrateResult`, `ProjectIntegrations`, `ProjectIntegrationItem` | §3.79〜3.83 の要求・応答 |
| `task-api`（Phase 20、ADR-0030） | `SecretList`, `SecretView`, `SecretUse`, `SecretPutBody`, `SecretPutResult` | `GET/PUT/DELETE /secrets...` の要求・応答（値は一切含まない）。`used_by: Vec<SecretUse>` は稼働中の設定（`[adapters.*].env_from_secrets` と `[[providers]].env_from_secrets`）から celeris が導く |

### 6.2 Rust 表記（serde の属性はコメントで示す。`JsonSchema` は全て derive）

```rust
// ---- task-core（実装済み）----
pub struct EventRow { pub id: u64, pub task_id: TaskId, pub seq: u64, pub ts: String, pub event: Event }
pub struct ListFilter { pub statuses: Vec<Status>, pub kinds: Vec<TaskKind>, pub genres: Vec<String> /* Phase 16, ADR-0027 D1 */, pub parent_id: Option<TaskId>, pub root_only: bool, pub text_contains: Option<String> }
pub enum ListOrder { Dispatch, UpdatedDesc, CreatedDesc }
pub struct Page<T> { pub items: Vec<T>, pub next_cursor: Option<String>, pub total: u64 }
// Event::ProviderThrottled { provider: String, until: OffsetDateTime, #[serde(default, skip_serializing_if = "Option::is_none")] reason: Option<String> }
// Event::WorkerStarted / WorkerFinished に #[serde(default, skip_serializing_if = "Option::is_none")] role: Option<RunRole>（None = ワーカー run。ADR-0014 D1）
// Phase 10（ADR-0016）: Task に #[serde(default, skip_serializing_if = "Option::is_none")] role: Option<String> と
//   #[serde(default, skip_serializing_if = "std::ops::Not::not")] aggregate: bool（false と null は直列化で省かれる）。
//   Event::Delegated { run_id: String, task_ids: Vec<TaskId> } を追加（type 名 `delegated`）
// ADR-0021: Event::QuestionRaised { run_id: String, text: String } を追加（type 名 `question_raised`）。
//   委譲した子が失敗し、親がやり直せなかったときにディスパッチャが出す質問。`TaskDetail.latest_question` /
//   `Inbox.questions[]` はこれも見る（`WorkerFinished{outcome: "question: …"}` と同じ扱い）。
// #[serde(rename_all = "snake_case")] pub enum RunRole { Worker, Reviewer }

// ---- task-ops: 参照・一覧 ----
pub struct TaskRef { pub id: TaskId, pub title: String, pub kind: TaskKind, pub status: Status, pub actions: Vec<Action> }
pub struct TaskSummary {
    pub id: TaskId, pub parent_id: Option<TaskId>, pub kind: TaskKind, pub status: Status, pub title: String,
    pub priority: i32, pub tier: Tier, pub adapter: Option<String>, pub attempts: u32, pub max_retries: u32,
    pub depends_on: Vec<TaskId>, pub created_at: String, pub updated_at: String,
    pub lease_expires_at: Option<String>, pub backoff_until: Option<String>,
    pub children: u32, pub pending_children: u32, pub role: Option<String> /* GUI-R2 */,
    pub genre: Option<String> /* Phase 16, ADR-0027 D1 */,
    pub assignee: Option<String> /* Phase 27, GUI-R3 */, pub conversation: bool /* Phase 27, GUI-R3 */,
    pub support: Option<String> /* Phase 29, GUI 監査 H4 */,
    pub actions: Vec<Action>,
    // ---- Phase 53（ADR-0044 D3/D4）: ボードのカードが要るもの ----
    pub labels: Vec<String>, pub category: TaskCategory, pub priority_label: String,
    pub project_id: Option<ProjectId>, pub milestone_id: Option<MilestoneId>,
}
pub struct TaskList { pub items: Vec<TaskSummary>, pub next_cursor: Option<String>, pub total: u64, pub counts_by_status: BTreeMap<Status, u64> }

// ---- task-ops: 詳細（celerisctl show --json と同一）----
pub struct TaskDetail {
    pub task: Task, pub workspace_dir: Option<String>, pub cluster: Option<String> /* Phase 12 */,
    pub role: Option<String> /* Phase 10 */, pub genre: Option<String> /* Phase 16, ADR-0027 D1 */,
    pub priority_label: String /* Phase 53, ADR-0044 D3: P0〜P3 */,
    pub delegated: Vec<DelegatedView> /* Phase 10 */,
    pub timers: Timers, pub criteria: Vec<CriterionView>,
    pub runs: Vec<RunSummary>, pub prior_review: Vec<ReviewNote>, pub answers: Vec<AnswerNote>,
    pub latest_question: Option<String>, pub approvals: Vec<ApprovalLink>,
    pub dependencies: Vec<TaskRef>, pub dependents: Vec<TaskRef>, pub children: Vec<TaskRef>,
    pub actions: Vec<Action>, pub worker_run_hint: Option<String>,
    pub worktree: Option<WorktreeView> /* ADR-0019 */,
}
/// ADR-0019 D2: クラスタ側の worktree。celeris はここだけを触り、commit はしない。
pub struct WorktreeView { pub project: String, pub dir: String, pub branch: String }
/// Phase 10（ADR-0016 D2）: 1 回の `delegate`（`Event::Delegated`）の要約。`tasks` は子の現在の状態（消えた ID は落とす）。
pub struct DelegatedView { pub run_id: String, pub ts: String, pub tasks: Vec<TaskRef> }
pub struct Timers { pub now: String, pub lease_expires_at: Option<String>, pub backoff_until: Option<String>,
    pub consecutive_requeues: u32, pub max_requeues: u32, pub consecutive_reviewer_requeues: u32 }
pub struct CriterionView { pub idx: usize, pub text: String, pub check: Check, pub latest_verdict: Option<VerdictView>, pub approval: Option<ApprovalLink> }
pub struct VerdictView { pub run_id: String, pub criterion_idx: usize, pub pass: bool, pub reason: String, pub ts: String }
pub struct RunSummary { pub run_id: String, pub role: RunRole /* worker | reviewer */, pub adapter: String, pub model: String, pub provider: Option<String>,
    pub account: Option<String> /* Phase 13/14: プールのアカウント（`WorkerStarted.account`）。プールでない run では出ない */,
    pub started_at: String, pub finished_at: Option<String>, pub outcome: Option<RunOutcomeKind>, pub outcome_text: Option<String>,
    pub usage: Option<Usage>, pub progress: u32, pub artifacts: u32, pub verdicts: u32, pub reviewer_deferrals: u32,
    pub files: Option<RunFiles> }
pub struct RunFiles { pub stdout: bool, pub stderr: bool, pub result: bool,
    pub request: bool /* ADR-0023 D2 */, pub prompt: bool /* ADR-0023 M1 */ }
// #[serde(rename_all = "snake_case")]
pub enum RunOutcomeKind { Done, Question, Error, Requeue, LeaseExpired, Interrupted /* Phase 53, ADR-0044 D2 */ }
pub struct ReviewNote { pub criterion: usize, pub pass: bool, pub reason: String }   // task_ops::derive（実装済み）。task_worker::PriorReview への写像はディスパッチャ側
pub struct AnswerNote { pub question: String, pub answer: String }                    // task_ops::derive（実装済み）
pub struct ApprovalLink { pub approval: TaskRef, pub criterion_idx: Option<usize>, pub attempt: Option<u32>, pub decided: Option<ApprovalDecisionView> }
pub struct ApprovalDecisionView { pub by: String, pub approved: bool, pub note: Option<String>, pub ts: String }
// #[serde(rename_all = "snake_case")]
pub enum Action { Approve, Reject, Answer, Cancel, Retry /* Phase 31 */,
                 Edit /* Phase 53, ADR-0044 D1 */, Reopen /* Phase 53, ADR-0044 D2 */ }

// ---- Phase 53（ADR-0044 B1）: 編集・コメント・再開・タイムライン ----
// #[serde(rename_all = "snake_case")]
pub enum TaskCategory { Feature, Bug, Research, Ops, Docs, Other }   // 既定 Other（Task の JSON では省略）
// Task に足した 2 つ（どちらも既定なら JSON に出ない。導入前のタスクもそのまま読める）:
//   pub labels: Vec<String>,          // 小文字 [a-z0-9-]、1..=64 文字、最大 8 個
//   pub category: TaskCategory,
// Event に足した 1 つ（type 名 `edited`。状態は変えない。replay は無視する）:
//   Edited { fields: Vec<String>, by: String }

// ---- Phase 59（ADR-0046）: 組織 = Agent Profile の継承木 ----
// #[serde(rename_all = "snake_case")]
pub enum KnowledgeKind { Kb, Repo, Memory }
// #[serde(rename_all = "snake_case")]
pub enum ProfileRun { Host, Container }
// #[serde(rename_all = "snake_case")]
pub enum TaskMode { Prototype, Production, Research }   // 既定 Production（Task の JSON では省略）

pub struct KnowledgeMount { pub kind: KnowledgeKind, pub scope: Option<String>, pub name: Option<String>,
                            pub path: Option<String>, pub docs: Option<String> }
pub struct HarnessPrefs { pub allowed: Vec<String>, pub default: Option<String> }
pub struct ModelPrefs   { pub tier: Option<Tier>, pub allowed_tiers: Vec<Tier> }
pub struct ReviewPrefs  { pub harness: Option<String>, pub tier: Option<Tier> }
pub struct Permissions  { pub approvals: Vec<String> }

// `OrgNode.profile`（deny_unknown_fields。**全項目が任意**。空なら JSON に出ない）
pub struct Profile {
    pub skills: Vec<String>, pub knowledge: Vec<KnowledgeMount>, pub harnesses: HarnessPrefs,
    pub tools: Vec<String>, pub deny_tools: Vec<String>, pub run: Option<ProfileRun>,
    pub model: ModelPrefs, pub policy: Vec<String>, pub review: ReviewPrefs, pub permissions: Permissions,
}

// `GET /org` の `effective_profiles[]`（継いだ後。GUI はこれをそのまま表示する。再計算しない）
pub struct EffectiveProfile {
    pub node_id: String, pub chain: Vec<String> /* 根→葉 */,
    pub skills: Vec<String>, pub knowledge: Vec<KnowledgeMount>,
    pub harnesses_allowed: Vec<String>, pub harness_default: Option<String>,
    pub tools: Vec<String> /* deny_tools を引いた後 */, pub deny_tools: Vec<String>,
    pub run: Option<ProfileRun>, pub tier: Option<Tier>, pub allowed_tiers: Vec<Tier>,
    pub policy: Vec<String>, pub review_harness: Option<String>, pub review_tier: Option<Tier>,
    pub approvals: Vec<String>,
}

// Task に足した 2 つ（どちらも既定なら JSON に出ない）:
//   pub skills: Vec<String>,   // 必要な能力タグ（小文字 [a-z0-9._-]、最大 12 個）
//   pub mode: TaskMode,        // 既定 production
// Event に足した 1 つ（type 名 `assigned`。状態は変えない。replay は無視する）:
//   Assigned { node: String, score: usize, reason: String }
//   → GUI のタスク画面の「なぜこの担当か」。`score` は タスクの skills ∩ ノードの実効 skills の件数

// `PATCH /tasks/{id}` の本文（deny_unknown_fields。省略 = 据え置き、Option<Option<T>> は null で消す）
pub struct TaskEdit {
    pub title: Option<String>, pub objective: Option<String>, pub acceptance: Option<Vec<CriterionSpec>>,
    pub priority: Option<PriorityInput> /* "P0".."P3" か i32 */, pub labels: Option<Vec<String>>,
    pub category: Option<TaskCategory>,
    pub repos: Option<Vec<String>> /* Phase 52+53 のマージ, ADR-0043 D2: 案件のリポジトリ名 */,
    pub assignee: Option<Option<String>>, pub role: Option<Option<String>>,
    pub tier: Option<Tier>, pub adapter: Option<Option<String>>, pub milestone_id: Option<Option<MilestoneId>>,
    pub depends_on: Option<Vec<TaskId>>,
    pub max_turns: Option<u32>, pub max_wall_secs: Option<u64>, pub max_retries: Option<u32>,
    // ---- Phase 59（ADR-0046 D2 / D3 / D4）----
    pub skills: Option<Vec<String>>, pub mode: Option<TaskMode>, pub harness: Option<Option<String>>,
    pub expected_status: Option<Status>,
}
pub struct EditResult { pub task: Task, pub fields: Vec<String> }
// #[serde(untagged)]
pub enum PriorityInput { Label(PriorityLabel), Number(i32) }
// #[serde(rename_all = "UPPERCASE")]
pub enum PriorityLabel { P0, P1, P2, P3 }                             // P0=30 / P1=20 / P2=10 / P3=0

pub struct CommentId(pub Ulid);
// #[serde(rename_all = "snake_case")]
pub enum CommentAuthorKind { Human, Node, System }
pub struct TaskComment { pub id: CommentId, pub task_id: TaskId, pub author_kind: CommentAuthorKind,
    pub author: Option<String>, pub body: String, pub run_id: Option<String>, pub created_at: String /* RFC3339 */ }
pub struct CommentBody { pub body: String }                            // POST /tasks/{id}/comments の本文
pub struct CommentList { pub items: Vec<TaskComment> }
// #[serde(rename_all = "snake_case")]
pub enum CommentEffect { Stored, Interrupted, Answered, Terminal }
pub struct CommentResult { pub comment: TaskComment, pub effect: CommentEffect,
    pub transition: Option<TransitionResult>, pub can_reopen: bool }
pub struct ReopenBody { pub expected_status: Option<Status> }

pub struct Timeline { pub task_id: TaskId, pub items: Vec<TimelineItem> }
// #[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimelineItem {
    Event { at: String, seq: u64, event: Event },
    Comment { at: String, comment: TaskComment },
    Approval { at: String, approval: Approval },
    Report { at: String, report: Report },
    Delegation { at: String, run_id: String, tasks: Vec<TaskRef> },
    Release { at: String, sha12: String, commits: Vec<String> },
    Integration { at: String, action: String, detail: String },        // ADR-0043 A2
    Doc { at: String, project_id: ProjectId, path: String, title: String },   // ADR-0044 D7（Phase 57）
    Knowledge { at: String, run_task_id: TaskId, state: String,               // ADR-0047 D4/D5（Phase 62）
        ingested: Option<u32>, inbox: Option<u32>, discarded: Option<u32>,
        via: Option<String> },                                                // ADR-0052 D2（Phase 64）
}

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
    ClusterUnavailable { cluster: String, host: String, at: String, tasks: u32 },   // Phase 12（ADR-0018）
}

// ---- task-ops: 操作の入力（実装済みの型に 9b で serde / JsonSchema を付ける。POST /tasks, POST /plans の本文そのもの）----
// #[serde(deny_unknown_fields)]
pub struct NewTaskSpec {
    pub title: String, pub objective: String, pub acceptance: Vec<CriterionSpec>,
    #[serde(default)] pub kind: TaskKind /* execute */,
    // Phase 10（ADR-0016 M3）: tier / max_turns / max_wall_secs / adapter は Option になった（省略時は役割の既定 → 全体の既定）
    #[serde(default)] pub tier: Option<Tier> /* 既定 standard */,
    // Phase 53（ADR-0044 D3）: priority は "P1" でも 20 でもよく、**省略時は P2（= 10）**
    #[serde(default)] pub priority: Option<PriorityInput>,
    #[serde(default)] pub parent: Option<TaskId>, #[serde(default)] pub depends_on: Vec<TaskId>,
    #[serde(default)] pub max_turns: Option<u32> /* 既定 10 */, #[serde(default)] pub max_wall_secs: Option<u64> /* 既定 600 */,
    #[serde(default = "2")] pub max_retries: u32,
    #[serde(default)] pub role: Option<String> /* Phase 10 */,
    #[serde(default)] pub genre: Option<String> /* Phase 16, ADR-0027 D1 */,
    #[serde(default)] pub aggregate: bool /* Phase 10 */,
    #[serde(default)] pub workspace: Option<PathBuf> /* JSON では文字列 */,
    #[serde(default)] pub cluster: Option<String> /* Phase 12 */, #[serde(default)] pub adapter: Option<String>,
    // Phase 23（ADR-0033 D2）
    #[serde(default)] pub project_id: Option<ProjectId>, #[serde(default)] pub milestone_id: Option<MilestoneId>,
    #[serde(default)] pub assignee: Option<String>,
    // Phase 53（ADR-0044 D1/D3）。`status` は draft か ready だけ（`POST /tasks` は省略時に ready を入れる）
    #[serde(default)] pub labels: Vec<String>, #[serde(default)] pub category: Option<TaskCategory>,
    #[serde(default)] pub status: Option<Status>,
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
pub struct GraphNode { pub id: TaskId, pub title: String, pub status: Status, pub kind: TaskKind, pub parent_id: Option<TaskId>,
                      pub role: Option<String> /* GUI-R2 */ }
pub struct GraphEdge { pub from: TaskId, pub to: TaskId, pub kind: String /* "depends_on" */ }

// ---- task-ops: デーモンのスナップショット（task-dispatch が作り、task-api が読む）----
pub struct DaemonSnapshot { pub instance_id: String, pub pid: u32, pub hostname: String, pub started_at: String, pub last_tick_at: String,
    pub ticks: u64, pub tick_ms: u64, pub in_flight: Vec<InFlight>, pub cooldowns: Vec<CooldownView>,
    pub awaiting_human: Vec<TaskId>, pub awaiting_children: Vec<TaskId> /* ADR-0023: 委譲した子を待っている親 */,
    pub unroutable: Vec<TaskId>, pub providers: Vec<ProviderLive>,
    #[serde(default)] pub clusters: Vec<ClusterLive> /* Phase 12 */,
    #[serde(default)] pub accounts_root: Option<String> /* Phase 13, claude-code の別名 */, #[serde(default)] pub max_runs_per_account: Option<usize> /* Phase 13 */,
    #[serde(default)] pub accounts_roots: HashMap<String, String> /* Phase 14, ADR-0025 */,
    #[serde(default)] pub accounts: Vec<AccountLive> /* Phase 13 */ }
pub struct InFlight { pub task_id: TaskId, pub run_id: String, pub provider: String, pub kind: InFlightKind, pub since: String }
// #[serde(rename_all = "snake_case")]
pub enum InFlightKind { Worker, Reviewer }
/// `task_dispatch::policy::Cooldown{provider, until: Instant, reason: CooldownReason}`（実装済み）を壁時計に直したもの。
pub struct CooldownView { pub provider: String, pub until: String, pub reason: String /* "throttled" | "auth_failed" | "exhausted" */ }
pub struct ProviderLive { pub id: String, pub adapter: String, pub tiers: Vec<Tier>, pub concurrency: usize, pub model: Option<String>,
    pub env_keys: Vec<String> /* ADR-0017 */, pub in_use: u32, pub last_check: Option<ProviderCheckView> /* ADR-0022 */,
    #[serde(default)] pub account_pool: bool /* Phase 13 */ }
/// Phase 12（ADR-0018）: `[[clusters]]` の稼働状況（`id` 昇順）。`connected` はこの tick の `ssh -O check` の結果。
/// ADR-0032 D1/D5: `auth`（既定 `"manual"`）と `connect_pending`（既定 `false`）を追加（古いスナップショットとの互換用）。
pub struct ClusterLive { pub id: String, pub host: String, pub concurrency: usize, pub in_use: u32, pub connected: bool, pub cooldown_until: Option<String>,
    #[serde(default = "default_manual")] pub auth: String, #[serde(default)] pub connect_pending: bool }
/// Phase 13（ADR-0024）: プールの 1 アカウントの稼働状況（`AccountView` から `dir`/`logged_in`/`stats` を除いたもの。Unix 秒のまま）。
/// Phase 14（ADR-0025）: `adapter` を追加（既定 `"claude-code"`。古いスナップショットとの互換用）。
pub struct AccountLive { #[serde(default = "default_claude_code")] pub adapter: String, pub id: String, pub logged_in: bool, pub in_use: u32, pub usage: Option<AccountUsageLive>, pub score: Option<f64>,
    pub excluded_reason: Option<String>, pub cooldown: Option<AccountCooldownLive>, pub last_check: Option<ProviderCheckView>, pub login_pending: bool }
pub struct AccountUsageLive { pub five_hour: Option<RateWindow>, pub seven_day: Option<RateWindow>, pub status: Option<String>, pub observed_at: i64, pub source: String }
pub struct AccountCooldownLive { pub until: i64, pub reason: String }

// ---- task-api ----
pub struct Health { pub api_version: String, pub schema_version: u32, pub celeris_version: String, pub instance_id: String,
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
    pub env_keys: Vec<String>, pub in_use: Option<u32>, pub cooldown: Option<CooldownView>,
    pub last_check: Option<ProviderCheckView> /* ADR-0022 */, pub stats: ProviderStats,
    #[serde(default)] pub account_pool: bool /* Phase 13 */ }
/// ADR-0022 D2: 直近の疎通確認（`{at, result}`）。task-ops の `ProviderLive.last_check` と同じ型。
pub struct ProviderCheckView { pub at: String, pub result: String }
pub struct ProviderStats { pub runs: u64, pub done: u64, pub question: u64, pub error: u64, pub requeue: u64, pub lease_expired: u64,
    pub input_tokens: u64, pub output_tokens: u64, pub by_day: Vec<DailyUsage> }
pub struct DailyUsage { pub day: String /* YYYY-MM-DD */, pub runs: u64, pub input_tokens: u64, pub output_tokens: u64 }
pub struct DaemonView { pub now: String, pub snapshot: Option<DaemonSnapshot> }
// Phase 12（ADR-0018）
pub struct Clusters { pub items: Vec<ClusterView> }
// ADR-0032 D1/D5: `auth`（設定。既定 `"manual"`）と `connect_pending`（スナップショット。既定 `false`）を追加。
pub struct ClusterView { pub id: String, pub host: String, pub concurrency: usize, pub sync: String /* "rsync" | "none" */, pub delete_on_push: bool,
    pub has_setup: bool, pub env_keys: Vec<String>, pub rsync_excludes: Vec<String>,
    pub in_use: Option<u32>, pub connected: Option<bool>, pub cooldown_until: Option<String>, pub cooldown_remaining_secs: Option<u64>,
    #[serde(default = "default_manual")] pub auth: String, #[serde(default)] pub connect_pending: bool }
// ADR-0032 D5: `POST /clusters/{id}/connect` / `POST /clusters/{id}/connect/code` / `DELETE /clusters/{id}/connect`（Phase 22）。
pub struct ClusterConnectStart { pub kind: String /* "connected" | "needs_code" */,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub prompt: Option<String> /* needs_code のときだけ */,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub expires_at: Option<String> /* needs_code のときだけ */ }
#[serde(deny_unknown_fields)]
pub struct ClusterConnectCodeBody { pub code: String }
pub struct ClusterConnectResult { pub ok: bool, #[serde(default, skip_serializing_if = "Option::is_none")] pub detail: Option<String> }
pub struct ConfigView { pub config_path: String, pub db: String, pub workspace_root: String, pub tick_ms: u64, pub max_concurrency: usize,
    pub lease_grace_secs: u64, pub idle_timeout_secs: u64, pub kill_grace_secs: u64, pub review_timeout_secs: u64, pub error_cooldown_secs: u64,
    pub retry_backoff_base_secs: u64, pub retry_backoff_max_secs: u64, pub max_requeues: u32, pub plan_auto_accept: bool,
    pub reviewer: ReviewerConfigView, pub providers: Vec<ProviderConfigView>, #[serde(default)] pub clusters: Vec<ClusterConfigView> /* Phase 12 */,
    #[serde(default)] pub roles: Vec<RoleConfigView> /* Phase 10 */,
    #[serde(default)] pub genres: Vec<GenreConfigView> /* Phase 16, ADR-0027 D1 */,
    #[serde(default)] pub delegation: DelegationLimits /* Phase 10 */, pub api: ApiConfigView }
/// Phase 10（ADR-0016 D1）: `[[roles]]` 1 行。`instructions` の**本文は出さない**（有無だけ）。
pub struct RoleConfigView { pub id: String, pub tier: Option<Tier>, pub adapter: Option<String>, pub max_turns: Option<u32>,
    pub max_wall_secs: Option<u64>, pub has_instructions: bool }
/// Phase 16（ADR-0027 D1）: `[[genres]]` 1 行。`capabilities` / `input_artifacts` / `output_artifacts` は
/// Phase 18（ADR-0028 D1）: 3 つとも自由記述の `Vec<String>` で、空なら省略される（`skip_serializing_if`）。
/// Phase 38（ADR-0028 追記）: `input_artifacts` / `output_artifacts` の要素は `名前: 説明` の形も取る
/// （型は変えない。GUI は `:` の前を名前として表示・照合に使う）。
pub struct GenreConfigView { pub id: String, pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub input_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub output_artifacts: Vec<String>,
    pub default_role: Option<String>, pub roles: Vec<String> }
/// Phase 10（ADR-0016 D2）: task-core の型。既定は 8 / 5 / 100。
pub struct DelegationLimits { pub max_delegate_per_run: usize, pub max_tree_depth: u32, pub max_tree_runs: u32 }
pub struct ReviewerConfigView { pub adapter: Option<String>, pub tier: Tier }
pub struct ProviderConfigView { pub id: String, pub adapter: String, pub tiers: Vec<Tier>, pub concurrency: usize, pub model: Option<String>, pub env_keys: Vec<String>, #[serde(default)] pub account_pool: bool /* Phase 13 */ }
// Phase 11（ADR-0017）: プロバイダ管理。`ProviderConfigView` は §3.24/§3.25 の応答にも使う。
pub struct ReloadResult { pub reloaded: bool }
pub struct ProviderCheckResponse { pub result: ProviderCheckResult, pub checked_at: String }
pub enum ProviderCheckResult { Ok, AuthFailed, Throttled, SpawnFailed } // snake_case で直列化（"ok" | "auth_failed" | "throttled" | "spawn_failed"）
// Phase 13（ADR-0024）: Claude アカウントのプール。ProviderConfigView / ProviderView に `account_pool: bool` を追加。
// Phase 14（ADR-0025）: codex を追加。`AccountList.roots`、`AccountView.adapter`、`AccountCreateBody.adapter`、
// `AccountLoginStart.kind`/`user_code` はすべて追加のみ（既存フィールドは変えない）。
pub struct AccountList { pub root: Option<String> /* roots["claude-code"] の別名 */,
    #[serde(default)] pub roots: HashMap<String, Option<String>> /* "claude-code" | "codex" -> 絶対パス or null */,
    pub max_runs_per_account: usize, pub items: Vec<AccountView> /* adapter -> id の順 */ }
pub struct AccountView { #[serde(default = "default_claude_code")] pub adapter: String, pub id: String, pub dir: String, pub logged_in: bool, pub in_use: u32, pub usage: Option<AccountUsageView>,
    pub score: Option<f64>, pub excluded_reason: Option<String>, pub cooldown: Option<AccountCooldownView>,
    pub last_check: Option<ProviderCheckView>, pub login_pending: bool, pub stats: AccountStats }
pub struct AccountUsageView { pub five_hour: Option<RateWindowView>, pub seven_day: Option<RateWindowView>, pub status: Option<String>,
    pub observed_at: String, pub source: String /* "run" | "check" */ }
pub struct RateWindowView { pub utilization: f64, pub resets_at: String }
pub struct AccountCooldownView { pub until: String, pub reason: String /* "auth_failed" | "throttled" | "exhausted" */ }
pub struct AccountStats { pub runs: u64, pub done: u64, pub error: u64, pub input_tokens: u64, pub output_tokens: u64 }
pub struct AccountCreateBody { pub id: String, #[serde(default = "default_claude_code")] pub adapter: String /* Phase 14 */ }
pub struct AccountCheckResponse { pub result: ProviderCheckResult, pub checked_at: String, pub detail: Option<String>, pub usage: Option<AccountUsageView> }
pub struct AccountLoginStart { #[serde(default = "default_paste_code")] pub kind: String /* "paste_code" | "device_code", Phase 14 */,
    pub url: String, #[serde(default, skip_serializing_if = "Option::is_none")] pub user_code: Option<String> /* codex のみ, Phase 14 */,
    pub expires_at: String }
pub struct AccountLoginCodeBody { pub code: String }
pub struct AccountLoginResult { pub result: String /* "ok" | "failed" */, pub detail: Option<String> }
// DaemonSnapshot に `#[serde(default)] pub accounts: Vec<AccountLive>` を追加（AccountView から dir / logged_in / stats を除いた観測値）。
// DaemonSnapshot に `#[serde(default)] pub accounts_roots: HashMap<String, String>` を Phase 14 で追加（アダプタ -> 絶対パス）。
// ADR-0032 D1: `auth`（既定 `"manual"`、`[[clusters]].auth` から。celeris が `GET /config` の `ConfigView` を
// 組み立てるときに `auth: c.auth.clone()` を渡す）を追加。
pub struct ClusterConfigView { pub id: String, pub host: String, pub concurrency: usize, pub sync: String, pub delete_on_push: bool, pub has_setup: bool, pub env_keys: Vec<String>, pub rsync_excludes: Vec<String>,
    #[serde(default = "default_manual")] pub auth: String }
pub struct ApiConfigView { pub bind: String, pub auth_required: bool, pub allowed_hosts: Vec<String> }
pub struct StreamHello { pub cursor: u64, pub now: String, pub daemon: Option<DaemonSnapshot> }
pub struct StreamHeartbeat { pub now: String }
pub struct StreamReset { pub reason: String /* "cursor_too_old" | "cursor_ahead" */, pub cursor: u64 }

// ---- Phase 20（ADR-0030）: GUI から預かる秘密（API キー等）。値を返すフィールドは無い ----
pub struct SecretList { pub dir: Option<String>, pub items: Vec<SecretView> /* id 昇順 */ }
pub struct SecretView { pub id: String, pub updated_at: Option<String>, pub fingerprint: Option<String> /* sha256 先頭 8 桁 */, pub used_by: Vec<SecretUse> }
pub struct SecretUse { pub scope: String /* "adapter" | "provider" */, pub name: String, pub env: String }
#[serde(deny_unknown_fields)]
pub struct SecretPutBody { pub value: String /* 空白だけは 422 */ }
pub struct SecretPutResult { pub id: String, pub updated_at: String, pub fingerprint: String }
// `[adapters.<種別>]` と `[[providers]]` の行に `#[serde(default)] pub env_from_secrets: HashMap<String, String>`
// を追加（環境変数名 -> 秘密 id）。`GET /config` のビュー型（`ProviderConfigView` 等）には出さない
// （id への参照であっても、それが「その環境変数が秘密に紐づいている」という設定上の事実を漏らすだけで
// 値は漏れないが、今回は追加しない実装判断。知りたければ `GET /secrets` の `used_by` を見る）。

// ---- Phase 23（ADR-0033 D1/D2）: 組織・案件・途中目標。`OrgNode` / `Project` / `Milestone` は task-core の型 ----
pub struct OrgNode { pub id: String, pub parent_id: Option<String>, pub name: String, pub kind: OrgKind /* secretary|department|section */,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub genre: Option<String>,
    #[serde(default)] pub brief: String, #[serde(default)] pub position: i64, pub created_at: String, pub updated_at: String }
pub struct OrgList { pub items: Vec<OrgNode> /* position 昇順、同値は id 昇順 */ }
#[serde(deny_unknown_fields)]
pub struct OrgCreateBody { pub id: String, pub name: String, pub kind: OrgKind, #[serde(default)] pub parent_id: Option<String>,
    #[serde(default)] pub genre: Option<String>, #[serde(default)] pub brief: Option<String>, #[serde(default)] pub position: Option<i64> }
#[serde(deny_unknown_fields)]
pub struct OrgPatchBody { /* 書いた項目だけ変える。genre は null で外せる（Option<Option<String>>） */
    #[serde(default)] pub name: Option<String>, #[serde(default)] pub kind: Option<OrgKind>, #[serde(default)] pub parent_id: Option<String>,
    #[serde(default, deserialize_with = "double_option")] pub genre: Option<Option<String>>,
    #[serde(default)] pub brief: Option<String>, #[serde(default)] pub position: Option<i64> }
pub struct Project { pub id: ProjectId, pub title: String, pub request: String,
    // Phase 55（ADR-0044 D6）: `cancelled` を追加。終端は done | cancelled。
    pub status: ProjectStatus /* proposed|active|paused|done|cancelled */,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub secretary_summary: Option<String>,
    // Phase 55（ADR-0044 D6）: アーカイブした時刻（RFC 3339）。無ければ項目ごと出ない。
    #[serde(default, skip_serializing_if = "Option::is_none")] pub archived_at: Option<String>,
    // Phase 55（ADR-0044 D6）: pause する直前の状態（resume の戻り先）。paused でなければ出ない。
    #[serde(default, skip_serializing_if = "Option::is_none")] pub paused_from: Option<ProjectStatus>,
    pub created_at: String, pub updated_at: String }
pub struct ProjectList { pub items: Vec<Project> /* created_at 降順 */ }
#[serde(deny_unknown_fields)]
pub struct ProjectCreateBody { pub title: String, pub request: String }
#[serde(deny_unknown_fields)]
pub struct ProjectPatchBody { pub status: ProjectStatus }
pub struct ProjectDetail { pub project: Project,
    // Phase 52（ADR-0043 D1）: この案件のリポジトリ（primary が先頭）。project.workspace は primary の location の写し。
    #[serde(default)] pub repos: Vec<ProjectRepo>,
    pub milestones: Vec<MilestoneView>, pub tasks: Vec<ProjectTaskView> }

// ---- Phase 52（ADR-0043 D1 / D6）: 案件のリポジトリと、タスクの作業ツリーの閲覧 ----
pub struct ProjectRepo { pub id: RepoId /* ULID */, pub project_id: ProjectId, pub name: String,
    pub kind: RepoKind /* git|dir */, pub location: WorkspaceSpec /* local{path} | remote{cluster,path} */,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub default_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub sync: Option<RepoSync> /* worktree|rsync|none。remote のみ */,
    #[serde(default)] pub run: RepoRun /* auto|host|container。既定 auto */,
    #[serde(default)] pub is_primary: bool, pub created_at: String /* RFC 3339 */ }
pub struct RepoRef { pub repo_id: RepoId, pub name: String }   // Task.repos[] の 1 件
pub struct RepoList { pub items: Vec<ProjectRepo> }            // primary が先頭
#[serde(deny_unknown_fields)]
pub struct RepoCreateBody { #[serde(default)] pub name: Option<String>, #[serde(default)] pub kind: Option<RepoKind>,
    pub location: WorkspaceSpec, #[serde(default)] pub default_branch: Option<String>,
    #[serde(default)] pub sync: Option<RepoSync>, #[serde(default)] pub run: Option<RepoRun>,
    #[serde(default)] pub is_primary: bool }
#[serde(deny_unknown_fields)]
pub struct RepoPatchBody { /* 書いた項目だけ変える */
    #[serde(default)] pub name: Option<String>, #[serde(default)] pub kind: Option<RepoKind>,
    #[serde(default)] pub location: Option<WorkspaceSpec>,
    #[serde(default, deserialize_with = "double_option")] pub default_branch: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")] pub sync: Option<Option<RepoSync>>,
    #[serde(default)] pub run: Option<RepoRun>, #[serde(default)] pub is_primary: Option<bool> }
pub struct TreeView { pub repo: String, pub path: String /* 作業ツリー相対。根は "" */,
    pub repos: Vec<TreeRepoView>, pub entries: Vec<TreeEntry> }
pub struct TreeRepoView { pub name: String, pub kind: String /* git|dir */, pub dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub base: Option<String> }
pub struct TreeEntry { pub name: String, pub path: String, pub kind: String /* dir|file|other */,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub size: Option<u64> }
pub struct TreeFileView { pub repo: String, pub path: String, pub size: u64, pub binary: bool, pub too_large: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub text: Option<String> }
// ---- Phase 54（ADR-0043 D5）: 変更の取り込み ----
pub enum IntegrationMethod { Merge, Pr, Discard }        // serde: "merge" | "pr" | "discard"
pub enum IntegrationState { Done, Open, Merged, Closed, Conflict, Failed }  // serde: snake_case
pub struct TaskIntegration { pub id: IntegrationId /* ULID */, pub task_id: TaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub repo_id: Option<RepoId>, // 案件のリポジトリの行が無ければ None
    pub repo: String /* タスクの中での名前。URL もこれ */, pub method: IntegrationMethod, pub state: IntegrationState,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub pr_number: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub pr_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub merged_at: Option<String> /* RFC 3339 */,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub detail: Option<String> /* 人に見せる一行 */,
    pub created_at: String, pub updated_at: String }
pub struct ChangedFile { pub path: String, pub status: String /* A|M|D|?|T。? は git の管理外 */,
    pub additions: u64, pub deletions: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub binary: bool }
pub struct DiffStat { pub files: u64, pub additions: u64, pub deletions: u64 }
pub struct ChangesView { pub task_id: String, pub repos: Vec<RepoChangesView>,
    pub gh: bool /* gh が PATH にあって認証済み */, pub merge_method: String /* [github] merge_method */ }
pub struct RepoChangesView { pub repo: String, pub branch: String, pub default_branch: String,
    pub base: String, pub head: String, pub ahead: u64, pub files: Vec<ChangedFile>, pub stat: DiffStat,
    pub dirty: bool, pub missing: bool, pub origin: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub integration: Option<TaskIntegration> }
pub struct ChangeDiffView { pub repo: String, pub path: String, pub diff: String, pub truncated: bool }
#[serde(deny_unknown_fields)]
pub struct IntegrateBody { pub method: IntegrationMethod,
    #[serde(default)] pub note: Option<String>,
    #[serde(default)] pub confirm: bool /* discard のときだけ必須 */ }
pub struct IntegrateResult { pub integration: TaskIntegration,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub child_task_id: Option<String> /* 衝突の解消タスク */ }
pub struct ProjectIntegrations { pub items: Vec<ProjectIntegrationItem> }
pub struct ProjectIntegrationItem { pub integration: TaskIntegration, pub task_title: String, pub task_status: Status }

pub struct MilestoneView { #[serde(flatten)] pub milestone: Milestone, // Milestone のフィールドは平らに出る
    #[serde(default, skip_serializing_if = "Option::is_none")] pub review: Option<MilestoneReviewView>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub proposal: Option<Milestone> }
pub struct MilestoneReviewView { pub message_id: String, pub text: String, pub at: String /* RFC 3339 */ }
pub struct ProjectTaskView { pub id: TaskId, pub title: String, pub status: Status, pub parent_id: Option<TaskId>,
    pub depends_on: Vec<TaskId>, pub assignee: Option<String>, pub milestone_id: Option<MilestoneId> }
pub struct Milestone { pub id: MilestoneId, pub project_id: ProjectId, pub seq: i64, pub title: String, #[serde(default)] pub description: String,
    // Phase 55（ADR-0044 D6）: `paused` と `cancelled` を追加。終端は cancelled。
    pub status: MilestoneStatus /* proposed|approved|in_progress|reached|redesigned|paused|cancelled */,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub paused_from: Option<MilestoneStatus>,
    pub created_at: String, pub updated_at: String }
// Phase 55（ADR-0044 D6）: `POST /projects/{id}/{cancel|pause|resume|archive|unarchive}` の応答（§3.84〜3.88）。
pub struct ProjectLifecycle { pub project: Project,
    #[serde(default)] pub cancelled_tasks: Vec<TaskRef> /* cancel のときだけ */,
    #[serde(default)] pub cancelled_milestones: Vec<MilestoneId> /* cancel のときだけ */ }
// Phase 55（ADR-0044 D6）: `POST /milestones/{id}/{cancel|pause|resume}` の応答（§3.89〜3.91）。
pub struct MilestoneLifecycle { pub milestone: Milestone, #[serde(default)] pub cancelled_tasks: Vec<TaskRef> }
#[serde(deny_unknown_fields)]
pub struct MilestoneCreateBody { pub title: String, #[serde(default)] pub description: Option<String>, #[serde(default)] pub status: Option<MilestoneStatus> }
#[serde(deny_unknown_fields)]
pub struct MilestonePatchBody { pub status: MilestoneStatus }
#[serde(deny_unknown_fields)]
pub struct MilestoneDecideBody { pub decision: MilestoneDecision /* ok|discuss|ng */, #[serde(default)] pub note: Option<String> }
pub struct MilestoneDecided { pub decision: MilestoneDecision, pub milestone: Milestone,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub next_milestone: Option<Milestone>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub plan_task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub conversation_task_id: Option<TaskId> }
// `Task` に `#[serde(default, skip_serializing_if = "Option::is_none")]` の
// `project_id: Option<ProjectId>` / `milestone_id: Option<MilestoneId>` / `assignee: Option<String>` を追加
// （`NewTaskSpec` にも同名の任意フィールド）。導入前の JSON・DB 行はそのまま読める。

// ---- Phase 24（ADR-0033 D4）: 対話。`Message` は task-core の型 ----
pub struct Message { pub id: MessageId /* ULID */, pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub project_id: Option<ProjectId>,
    pub role: MessageRole /* user|node */, pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub run_id: Option<String> /* role = node のときの run */,
    pub created_at: String }
pub struct MessageList { pub items: Vec<Message> /* created_at 昇順（古い順） */ }
#[serde(deny_unknown_fields)]
pub struct MessagePostBody { pub text: String, #[serde(default)] pub project_id: Option<ProjectId> }
pub struct MessageAccepted { pub message_id: String /* ULID */, pub task_id: TaskId /* 対話用タスク */ }
// `Task` に `#[serde(default, skip_serializing_if = "Option::is_none")]` の
// `conversation: Option<MessageId>`（対話由来ならきっかけの発言）を追加。**DB の列は増やしていない**。

/// スキーマ生成のルート（`task_worker::ProtocolSchema` と同じ流儀。1 フィールド = 1 公開型）。
pub struct ApiV1Schema {
    pub health: Health, pub problem: Problem, pub inbox: Inbox, pub task_list: TaskList, pub task: Task, pub task_detail: TaskDetail,
    pub events_page: EventsPage, pub run_list: RunList, pub artifact_list: ArtifactList, pub graph: Graph,
    pub new_task: NewTaskSpec, pub new_plan: NewPlanSpec, pub decision: DecisionBody, pub answer: AnswerBody, pub cancel: CancelBody,
    pub transition_result: TransitionResult, pub retry: RetryBody, pub retry_result: RetryResult, /* Phase 31 */
    pub replay_report: ReplayReport, pub providers: Providers, pub daemon: DaemonView,
    pub config: ConfigView, pub stream_hello: StreamHello, pub stream_event: EventRow, pub stream_daemon: DaemonSnapshot,
    pub stream_heartbeat: StreamHeartbeat, pub stream_reset: StreamReset, pub clusters: Clusters /* Phase 12 */,
    pub provider_config: ProviderConfigView, pub reload: ReloadResult, pub provider_check: ProviderCheckResponse /* Phase 11 */,
    pub account_list: AccountList, pub account: AccountView, pub account_check: AccountCheckResponse,
    pub account_login_start: AccountLoginStart, pub account_login_result: AccountLoginResult, /* Phase 13 */
    pub secrets: SecretList, pub secret_put: SecretPutResult, /* Phase 20 */
    pub cluster_connect_start: ClusterConnectStart, pub cluster_connect_result: ClusterConnectResult, /* Phase 22, ADR-0032 */
    pub org_list: OrgList, pub org_create: OrgCreateBody, pub org_patch: OrgPatchBody, /* Phase 23, ADR-0033 D1 */
    pub project_list: ProjectList, pub project_create: ProjectCreateBody, pub project_patch: ProjectPatchBody,
    pub project_detail: ProjectDetail, pub milestone_create: MilestoneCreateBody, pub milestone_patch: MilestonePatchBody,
    pub message_post: MessagePostBody, pub message_accepted: MessageAccepted, pub message_list: MessageList, /* Phase 24, ADR-0033 D4 */
    pub console: ConsolePage, pub console_block: ConsoleBlock, pub console_hello: ConsoleHello, /* Phase 60a, ADR-0048 D1 */
}

// ---- ADR-0048 D1（Phase 60a）: Console ----

/// `GET /console`。`items` は時刻の昇順。
pub struct ConsolePage { pub items: Vec<ConsoleBlock>, pub next_cursor: Option<String> }

/// `GET /console/stream` の `event: hello`。
pub struct ConsoleHello { pub cursor: String, pub scope: String, pub now: String }

/// 9 種のブロック（`#[serde(tag = "kind", rename_all = "snake_case")]`）。どれも `at`（RFC 3339）と `cursor` を持つ。
pub enum ConsoleBlock {
    Human { at: String, cursor: String, message_id: String, node_id: String, project_id: Option<ProjectId>, task_id: Option<TaskId>, text: String },
    Reply { at: String, cursor: String, message_id: String, node_id: String, project_id: Option<ProjectId>, task_id: Option<TaskId>, run_id: Option<String>, text: String },
    Task { at: String, cursor: String, task: ConsoleTaskLine },
    Progress { at: String, cursor: String, progress: ConsoleProgress, title: String, assignee: Option<String>, harness: Option<String>, tier: Tier, project_id: Option<ProjectId> },
    Question { at: String, cursor: String, task_id: TaskId, run_id: String, node_id: Option<String>, project_id: Option<ProjectId>, text: String, answered: bool, answer: Option<String> },
    Approval { at: String, cursor: String, approval: Approval },
    Milestone { at: String, cursor: String, milestone: Milestone, review: Option<MilestoneReviewView> },
    Report { at: String, cursor: String, report: Report },
    /// 知識整理 run の結果（ADR-0047 D4/D5、Phase 62）。`state` は `applied` | `failed`（`scheduled` は出ない）。
    /// `via`（ADR-0052 D2、Phase 64）は `"langmem"` か `"fallback:<adapter>"`（分からなければ `null`）。
    Knowledge { at: String, cursor: String, project_id: Option<ProjectId>, task_id: TaskId, task_title: String,
        run_task_id: TaskId, state: String, ingested: u32, inbox: u32, discarded: u32, via: Option<String> },
}

/// `task` ブロックの 1 行（`task_ops::console`）。
pub struct ConsoleTaskLine {
    pub task_id: TaskId, pub title: String, pub from: Status, pub to: Status, pub reason: String,
    pub assignee: Option<String>, pub harness: Option<String>, pub tier: Tier, pub mode: Option<String>,
    pub project_id: Option<ProjectId>, pub elapsed_secs: Option<u64>,
}

/// run ごとに束ねた進行（`task_ops::console`）。`first` / `last` は始めと終わりの 3 行まで。
pub struct ConsoleProgress {
    pub task_id: TaskId, pub run_id: String, pub count: usize, pub tool_count: usize,
    pub last_status: Option<String>, pub started_at: String, pub updated_at: String,
    pub first: Vec<ConsoleProgressLine>, pub last: Vec<ConsoleProgressLine>, pub truncated: bool,
}

pub struct ConsoleProgressLine {
    pub at: String, pub seq: u64,
    /// `tool_use` / `tool_result` / `text` / `thinking` / `status`（ADR-0048 D2）。
    pub kind: Option<ProgressKind>, pub tool: Option<String>,
    /// `summary` があればそれ、無ければ `msg`。
    pub text: String, pub error: bool,
}
```

`Option<String>` の時刻フィールドは RFC 3339 文字列（`OffsetDateTime` を持つ型は `#[serde(with = "time::serde::rfc3339")] #[schemars(with = "String")]`）。`BTreeMap<Status, u64>` は JSON ではキーが status 名のオブジェクト。

---

## 7. スキーマファイルと生成

| 事項 | 決め |
|---|---|
| ファイル | `docs/api/v1/` に 2 つ。**(1) `event.schema.json`**（Phase 9a で生成済み。ルート `EventRow`。task-core のテストが一致を検証）。**(2) `api-v1.schema.json`**（Phase 9b。ルート `ApiV1Schema`、`Event` / `EventRow` / `Task` などの共有型は `$defs` に 1 回だけ現れる）。**GUI の型生成は (2) だけを読む**（型ごとにファイルを分けると各ファイルが自分の `$defs` に `Task` / `Event` を抱え、TS 生成で同名の型が重複するため。(1) は celeris 自身の契約・テスト用） |
| 生成 | `UPDATE_SCHEMA=1 cargo test -p task-api`（`schemars::schema_for!(ApiV1Schema)`、整形は `serde_json::to_string_pretty`、末尾改行 1 つ）。既存の `task-worker` / `task-core` と同じ手順 |
| 一致テスト | `crates/task-api/src/schema.rs` の `committed_schema_matches_generated`（生成結果 == コミット済みファイル。差分があればテスト失敗、メッセージで再生成コマンドを示す） |
| 方言 | schemars 1.x の既定（JSON Schema 2020-12、`$defs`）。`$dynamicRef` 等は使わない。`json-schema-to-typescript` 16 が読める範囲に留める（G0 で確認。読めなければ celeris 側で `SchemaSettings::draft07()` に切り替える提案を出す） |
| GUI 側 | `pnpm gen:types` = `json2ts -i "$CELERIS_REPO/docs/api/v1/api-v1.schema.json" -o app/celeris/types.ts --additionalProperties=false`。生成物をコミットし CI で差分ゼロを検査 |
| 互換性 | フィールドの追加（任意）は v1 のまま。削除・型変更・意味変更は `/api/v2` |

---

## 8. celeris 実装者向けの補足（テストの観点）

最低限、次を `crates/task-api/tests/` に置く（fake の `SqliteStore` と `tempfile` だけで動く。ネットワークは loopback のみ）。

1. **認証**: `token_file` あり → 無トークン 401、誤トークン 401、正トークン 200。`token_file` 無し + 非 loopback bind → `Config::validate` がエラー。`/health` は無トークンで 200。
2. **Host / Origin**: `Host: evil.example` → 400。`POST` に `Origin` → 403。`Content-Type` 無し → 415。1 MiB 超 → 413。未知フィールド → 400。
3. **一覧**: 250 件で `limit=100` を 3 回たどって全件・重複なし・順序どおり（3 つの `order` 全て）。`q` の `%` エスケープ。`cursor` の改竄 → 400。
4. **詳細**: `GET /tasks/{id}` の本体が、同じ `ViewContext` で呼んだ `task_ops::view::task_detail` の compact な直列化と byte 単位で一致（`timers.now` と API が埋める `runs[].files` を除く。`celerisctl show --json` も同じ関数・同じ直列化。§3.5）。
5. **操作**: gate.rs / add.rs / plan.rs / cancel.rs / replay.rs の既存テストを task-ops に移したうえで、HTTP 越しに同じケース（approve の 4 通り、reject の 2 通り、answer の 3 通り、cancel の終端 3 通り、add の検証 5 通り）を確認。`expected_status` 不一致 409 `conflict` で状態不変。2 つ目の同じ approve が 409 `invalid_transition`。
6. **伝播**: Approval を reject → 子が `cascaded` に入る。先行を cancel → 後続が `dependency_failed` で `cascaded` に入る。
7. **ファイル**: `../x`、絶対パス、ワークスペース外への symlink、`run_id` に `..` → 全て 403。存在しない run → 404。`Range` と `offset` の 200 / 206 / 416。`.html` の成果物が `application/octet-stream` で返る。`X-Celeris-Sha256-Current` が改変後に変わる。
8. **SSE**: 購読中に `append_event` → 2 秒以内に `task.event` が届く。`Last-Event-ID` で再開して取りこぼし・重複なし。10,001 件遅れで `reset`。17 本目の接続が 503。切断後にポーリングが止まる（`events_since` 呼び出し回数で確認）。
9. **daemon**: `watch` に値を送る → `GET /daemon` と SSE `daemon` に反映。送る前は `snapshot: null`。
10. **スキーマ**: `committed_schema_matches_generated`。`GET /schema` の本体がファイルと一致。
11. **同時アクセス**: ディスパッチャ相当の書き込みループ（別スレッド、別接続）と API の読み取り 1,000 回を並走させて `database is locked` が出ない（WAL + busy_timeout の確認）。

---

## 9. 未決・確認事項（2026-09-14 に 1・3・5・6 を決定。1・3・6 は ADR-0014、5 は人間の確認）

1. **Reviewer run の使用量**（**決定: 記録して集計に含める**。P-G14 / ADR-0014 D1）: `WorkerStarted` / `WorkerFinished` が記録されないため、アカウント別のトークン集計から漏れる。Reviewer run にも同じイベント（または `ReviewerRunFinished{run_id, provider, usage}`）を残すかは celeris 側の判断（P-G14 として `celeris-proposals.md` に追加）。
2. **一括承認**（Plan の子を全部 Accept）: API には置かない。GUI が 1 件ずつ `POST /tasks/{id}/approve` を直列に呼ぶ（原子性が無いことを UI に明記）。
3. **`q` の対象**（**決定: `title` と `objective`**。P-G15 / ADR-0014 D2）: `title` のみ。`objective` の検索が要るなら `tasks.objective` 列の追加を提案する。
4. **`StaticPolicy` が cooldown の理由を保持するか**: `Cooldown.reason` は `Option`。保持しない実装でも仕様は満たす。
5. **`/health` を無認証にすること**（**決定: 無認証のまま**。人間の確認）: 版と `journal_mode` だけを返す。問題があれば認証必須に変える（GUI は G0 の疎通確認をトークン付きで行えばよい）。
6. **`POST /tasks` の追加検証**（**決定: `title` / `objective` の空白と `parent` の存在を検査する**。P-G16 / ADR-0014 D3）（`title` / `objective` の空白、`parent` の存在、`workspace` の空文字）: Phase 9a の task-ops は CLI と同じく検査しない。API 越しでも同じにしてある（挙動を変えない）。GUI 側はフォームの必須欄で防ぐ。celeris 側で足すなら task-ops に置き CLI も同じ関数を通す（提案として `celeris-proposals.md` P-G16）。

---

## 10. Phase 9b の実装で確定した細部（GUI から見える挙動）

本文が明示していなかった点を、celeris の実装（Phase 9b、ADR-0013「実装メモ」）に合わせて確定したもの。GUI はこれを契約として扱ってよい。

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
- 422 の `errors[].field` は、推定できるときだけ（`title` / `objective` / `acceptance` / `parent` / `depends_on` / `goal` / `answer`）。

**派生値**
- `RunSummary.outcome_text`: `done` 以外（`question` / `requeue`）でも接頭辞を除いた残りを入れる。`error` は文字列全体、`lease_expired` は `null`。
- `DaemonSnapshot.in_flight[]` の `kind: "reviewer"` の `run_id` は **Reviewer run 自身の id**（`WorkerStarted{role: reviewer}` と同じ。ADR-0014 D1）。
- `DaemonSnapshot.unroutable[]` は「設定に合うプロバイダ／クラスタが無い」タスクだけ。クラスタの多重接続待ち（cooldown 中を含む）のタスクは
  ここには**入らない**（Phase 12。受信箱の `attention[].cluster_unavailable` が代わりに知らせる。ADR-0018 実装メモ M8）。
- `Providers.items[].stats`:
  - 最初の `GET /providers` で全イベントを走査し、以後は要求のたびに増分だけ読む（celeris のメモリ上の観測値。再起動で再計算）。
  - `runs` は `WorkerStarted` の数（実行中を含む）。`by_day[].runs` はその日に終わった run の数。
- Remote ワークスペースの `runs[].files` とファイル系エンドポイントは、手元の写し `workspace_root/<task_id>` を見る（Phase 12 で変更。それ以前は全て `false` / 404 `remote workspace`）。写しが無ければ 404 `file_not_found`（`workspace directory does not exist`）。64 MiB を超える成果物は `sha256_current` と `sha256_matches` が `null`。

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

**運用ログ（ADR-0015）**
- API は要求ごとに `method` / `path` / `status` / `duration_ms` / `request_id`（= `X-Request-Id`）を記録する。既定は `debug`、**1 秒以上かかった要求は `warn`**（`slow api request`）。`GET /stream` は長時間つなぐのが正常なので警告の対象外。
- デーモンは `max(1 秒, tick_ms × 2)` を超えた tick を `warn`（`slow tick`）で記録する。
- DB がネットワークファイルシステム（NFS など）上にあると起動時に `warn`。SQLite の WAL はローカルディスクを前提にしている（ADR-0013 D5）。GUI の fixture もローカルディスクに置くこと。

**起動**
- celeris は `[api]` の `token_file` が読めない・空なら exit 2。DB が知らない新しい版数でも exit 2。
- API の DB 接続を開けない・bind できない場合は起動に失敗する（API 無しで動き続けない）。
- `celeris_version` は celeris crate の版（現在 `"0.1.0"`）。


### 部署レビューからデプロイ準備への引き渡し（ADR-0051）

`GET /tasks/{id}/changes` の省略可能な `delivery` は自己改善案件の進行状態。
`state` は `reviewing | merge_queued | merging | preparing | ready | blocked`。
`task_id, project_id, repo_id, repo, branch, base, head, default_branch, department,
review_run, worker_run, criterion_idx, decision, detail, release, prepare_pid, notification` を保持する。
`decision` は既存Reviewer runのマージ判定。`release` は検証対象sha12。
`ready` はビルドとsnapshot検証の成功であり、本番昇格とは異なる。
既存 `/releases/{sha12}/promote` だけが人のデプロイ操作を受け付ける。

管理系 `POST /tasks/{id}/rereview` は `ReopenBody {expected_status?: "done"}` を受け取り、
Reviewer条件がある通常のdone仕事をreviewingへ戻す。返却は `TransitionResult`。
実装runは再実行せず、既存成果のレビューを再実行する。認証、404、409、422は他の管理操作と同じ。
