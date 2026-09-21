# ADR-0056: 外部エージェントが Celeris を操作する MCP サーバー — 知識・タスク/案件・組織の一覧/閲覧/作成

- 日付: 2026-09-21
- 状態: **Accepted**（人の指示: 「celeris を外部から操作可能にする MCP サーバー。ChatGPT のチャットや他の場所で動いたエージェントが
  知識ベースに MCP 経由で知識を投入する、ChatGPT のチャットや Deep Research でアイデアの種を作り、それを元に案件作成・タスク実行、
  各組織のワーカーに特化した skills を外部から注入する。知識ベース・タスク・組織の 3 つについて一覧取得・閲覧・作成」。
  質問への回答: 接続は **ChatGPT の Secure MCP tunnel（outbound 443 で手元の MCP を取りに来る）を使うのでローカルに HTTP で公開**、
  それ以外は Tailscale 等で LAN に入る。外部が作る案件・タスクは **CoS に渡す**（直接作らない）。外部からの知識は **`_inbox` 経由**。
  skills は **SKILL.md を KB に置きノードに mount**）
- 関連: ADR-0047（知識ベース。`_inbox`、出典、秘密の拒否）、ADR-0048（Console。人の発言 → CoS → actions）、ADR-0046（組織 = profile の継承木）、
  ADR-0054（CoS の継続セッション）、ADR-0017 M2（API は task-dispatch / task-worker を知らない）、ADR-0045 D2（秘密は `[secrets]` 側）

## 1. 文脈

Celeris の入口は今まで GUI（Console）と `celerisctl` だけだった。人が ChatGPT や Deep Research で作った「種」（調査結果、アイデア、
仕様の草案）を Celeris に流すには、いちど手でコピーするしかない。MCP（Model Context Protocol）は LLM クライアントから外部ツールを呼ぶ
標準で、ChatGPT・Claude・Codex・opencode がクライアントになれる。Celeris を MCP サーバーとして出せば、外のエージェントが
**知識を投入し、CoS に依頼を渡し、組織の profile に skills を足す**ことができる。

守るべき境界: 外部エージェントは **人ではない**。案件やタスクの生成判断は今までどおり CoS（ADR-0048）が行い、知識は今までどおり
候補として `_inbox` に入り（ADR-0047 D3）、組織の変更は人が GUI で見て戻せる。MCP は「もう一つの入口」であって権限の抜け道ではない。

## 2. 決定

### D1. 転送と認証: Streamable HTTP を手元に出し、公開は外側の仕組みに任せる

- `celeris` が **MCP Streamable HTTP**（MCP 仕様 2025-06-18。`POST /mcp` に JSON-RPC、応答は JSON か SSE、`GET /mcp` は SSE の購読、
  `Mcp-Session-Id` ヘッダ）を **`[mcp] listen`（既定 `127.0.0.1:18200`）** で出す。API（7710）とは別の口（公開範囲を分けるため）。
  LAN の別アドレスに bind してもよい（設定次第）。TLS は持たない。**公開経路（ChatGPT の Secure MCP tunnel、Tailscale、Cloudflare Tunnel）は
  Celeris の外**で用意する。
- 認証は **クライアントごとの Bearer トークン**。`mcp_clients` 表（`id`、`name`、`token_hash`（SHA-256）、`scopes`（D4）、`created_at`、
  `last_used_at`、`revoked_at`）。発行と管理は `celerisctl mcp client add <name> [--scope …]`（トークンは **発行時に 1 度だけ表示**。
  DB にはハッシュのみ）、`celerisctl mcp client ls|revoke <id>`。値はログ・応答・PROGRESS に出さない。
- 口は **複数持てる**（`[[mcp.listeners]]`。最初の口だけなら `[mcp] listen` でもよい）。口ごとに `auth = "token"`（既定）か `"none"`。
  **`none` は listen が loopback のときだけ**許し（loopback 以外は設定エラー）、しかも **`client = "<id>"` で名前付きクライアントに固定する**
  （その口に来た要求はすべてそのクライアントとして扱う。スコープ・監査・流量制限は `mcp_clients` の行に従う。`celerisctl mcp client add <name>
  --no-token` で「トークンを持たない客」を作る）。用途は **ChatGPT の Secure MCP tunnel**: トンネルの手元側エージェントが同じマシンから
  `http://127.0.0.1:<port>/mcp` を叩き、**Bearer ヘッダを足す機能が無い**（2026-09-21 人の確認）ので、認証は「そのポートに届けるのは
  トンネルだけ」という配置で担保する。例:

  ```toml
  [[mcp.listeners]]
  listen = "127.0.0.1:18200"          # Claude Code / Codex / LAN の客（Tailscale 越し含む）。Bearer 必須
  auth = "token"
  [[mcp.listeners]]
  listen = "127.0.0.1:18201"          # ChatGPT Secure MCP tunnel の手元側だけが叩く。認証なし・客は chatgpt に固定
  auth = "none"
  client = "chatgpt"
  ```
- CLI エージェント（Claude Code / Codex / opencode）向けに **`celerisctl mcp stdio --client <id>`**（stdio ↔ 手元の HTTP の橋。トークンは
  `--token-file` か環境変数）。中身は同じサーバー。
- **OAuth 2.1 は採らない**（この ADR では）。必要になったら別 ADR（動的クライアント登録 + PKCE + 認可画面）。

### D2. 道具（tools）— 3 つの領域、それぞれ一覧・閲覧・作成

すべて JSON Schema 付き。名前は `<領域>_<動詞>`。`limit` の既定 20・上限 100。id は ULID の全文。

**知識（ADR-0047）**
- `knowledge_list { scope?, tag?, limit? }` — `index.json` から（path / title / tags / scope / updated / confidence）。
- `knowledge_search { query, scope?, limit? }` — `celerisctl knowledge search` と同じ検索。
- `knowledge_get { path }` — 本文（Markdown）とメタ。`_retired` は 404。
- `knowledge_propose { title, body, tags, scope, sources?, confidence? }` — **`_inbox` に候補として置く**（ADR-0047 D3 と同じ書式。
  出典に `mcp:<client_id>` を必ず足す。`secret_finding` で秘密を拒否。直接コミットはしない）。返値は候補のパス。

**タスク・案件（読むだけ。作るのは CoS 経由）**
- `tasks_list { status?, project_id?, limit? }`、`tasks_get { id }`（状態・担当・直近の報告の要約・成果物一覧。run の生ログは返さない）。
- `projects_list { status?, limit? }`、`projects_get { id }`（途中目標とタスクの一覧）。
- `console_instruct { text, project_id? }` — **人の発言と同じ経路**（`POST /console/instruct` の中身）で CoS に渡す。発言の `author` は
  `mcp:<client_id>`（Console では「外部（<client name>）」の帯で人の発言と区別して見せる。ADR-0048 D4 の `human` ブロックの variant）。
  返値は `message_id` / `task_id`（対話 run）。案件化・タスク化は CoS が判断し、actions で作る（ADR-0048 D3。作ったタスクは今までどおり ready）。
- `console_reply { task_id, wait_secs? }` — 対話 run の返事（`reply` の本文）と、その run が起こした actions の結果（作った案件・タスクの id と題名）
  を返す。終わっていなければ `wait_secs`（上限 60）まで待ってから `{ state: "pending" }`。Deep Research のような一往復の客が結果を取るための道具。

**組織（ADR-0046）**
- `org_list {}` — 木（id / name / parent / kind / skills / harness の既定 / 継続セッションの有無）。
- `org_get { node_id }` — 実効 profile（`EffectiveProfile`）と `skills_mounts`（D3）。
- `org_create_node { parent_id, id, name, profile? }` — 子ノードを作る（`profile` は `Profile` の部分集合: skills / harnesses / model / policy /
  knowledge mounts / skills_mounts。`tools` と `permissions` は**外からは触れない**）。スコープ `org:write`。
- `org_mount_skill { node_id, skill }` / `org_unmount_skill { node_id, skill }` — D3 の mount を足す・外す。スコープ `org:write`。

**skills（D3 の置き場）**
- `skills_list {}`、`skills_get { name }`（SKILL.md 本文と付属ファイルの一覧）。
- `skills_put { name, skill_md, files? }` — KB の `skills/<name>/SKILL.md`（＋付属ファイル）を書く（Claude Code の skills 形式。frontmatter の
  `name` / `description` 必須。名前は `[a-z0-9-]{1,64}`）。**知識と違って直接書く**（mount されるまで何にも効かない = mount が門。
  出典 `mcp:<client_id>` を frontmatter に残す）。スコープ `skills:write`。

**resources**（読むだけの客のため）: `celeris://knowledge/<path>`、`celeris://tasks/<id>`、`celeris://projects/<id>`、`celeris://org/<node_id>`、
`celeris://skills/<name>`。中身は対応する `*_get` と同じ。

### D3. skills = SKILL.md を KB に置き、ノードに mount して run に届ける

- 置き場: 知識ベースの **`skills/<name>/SKILL.md`**（＋同じディレクトリの付属ファイル）。ADR-0047 のスコープの外側の専用ディレクトリ
  （`index.json` には載せない。`skills_list` が一覧）。git 管理は KB と同じ。
- **`Profile.skills_mounts: Vec<String>`**（skill 名）を profile に足す。継承は `knowledge` mounts と同じ規則（親の mount を子が継ぐ。
  `EffectiveProfile.skills_mounts` に平坦化）。既存の `Profile.skills`（マッチングのタグ）とは**別物**（名前が近いが役割が違う。
  ドキュメントで明記）。
- 届け方（ワーカーの run 開始時、`RunContext` に `skills: [{name, path}]`）:
  - `claude-code`: 作業場所の **`.claude/skills/<name>/`** に SKILL.md と付属ファイルを写す（Claude Code が自動で読む形式）。
  - `codex`: 作業場所の `AGENTS.md` の末尾に `## Skills（celeris）` 節として各 SKILL.md の本文を連結（既存の AGENTS.md は壊さない。
    節は run ごとに書き直す）。
  - `acp`（opencode）: 前置きの `skills` 節として本文を渡す。
  - 研究系（PaperQA / LDR / LangMem）: 対象外（道具を使う契約ではない）。
- `request.json` に届けた skills の一覧を残す（何が効いていたかを後から追える）。

### D4. スコープと監査

- スコープ: `knowledge:read`、`knowledge:propose`、`tasks:read`、`console:instruct`、`org:read`、`org:write`、`skills:read`、`skills:write`。
  `celerisctl mcp client add` の既定は **read 系 + `knowledge:propose` + `console:instruct`**（`org:write` / `skills:write` は明示）。
  `--no-token` の客（`auth = "none"` の口に固定する客）も同じ規則。ChatGPT に skills を書かせたいなら `--scope skills:write` を明示する。
  スコープ外の tool は `tools/list` に**出さない**（呼ばれたら JSON-RPC の `-32601`）。
- すべての `tools/call` を **`mcp_calls` 表**（`client_id`、`tool`、`ok`、`error_kind`、`latency_ms`、`at`）に残す（引数と結果の本文は残さない）。
  `console_instruct` は Console にも出るので二重には書かない。
- 流量: `[mcp] rate_limit_per_min`（既定 60、クライアントごと）。超えたら JSON-RPC エラー（`-32000`、`retry_after`）。
- 管理 API: `GET /mcp/clients`（id / name / scopes / last_used_at。トークンは出ない）と `GET /mcp/calls?client=`（直近 100 件）。GUI の
  「アカウント」画面に「MCP クライアント」の節（後続の GUI Phase）。

### D5. 実装の置き場

- 新クレート **`crates/celeris-mcp`**（JSON-RPC 2.0 と MCP の最小実装: `initialize` / `notifications/initialized` / `tools/list` / `tools/call` /
  `resources/list` / `resources/read` / `ping`。SSE は「応答をストリームで返す」最小形）。外部クレートは足さない（MCP の芯は小さい。
  `axum` / `serde_json` / `tokio` は既にある）。`task-api` と同じく **`task-dispatch` / `task-worker` を知らない**（ADR-0017 M2）。
  必要な操作（知識の検索・候補の書き込み、Console の instruct、組織の読み書き）は `task-ops` / `task-core` の既存関数を呼ぶ。
- `celeris` が `[mcp]` を読んで起動（`[llm_proxy]` と同じ形。reload 対象外）。`celerisctl mcp client …` は DB を直接開く（`knowledge rerun` と同じ）。
- migration `0024_mcp.sql`（`mcp_clients`、`mcp_calls`）→ `SCHEMA_VERSION = 24`（配備は停止→起動）。

### D6. 採らない

- 外部からの**直接の**タスク・案件作成（人の決定「CoS に渡す」）。
- 知識の直接コミット（`_inbox` 経由のみ）。
- OAuth 2.1 / TLS / 認可画面（外側の仕組みに任せる。必要なら別 ADR）。
- 外部からの `tools` / `permissions` / `review` の変更（組織の権限に関わる欄は GUI と config だけ）。

## 3. 受け入れ条件

- **Phase 78（D1・D2・D4・D5）**: `crates/celeris-mcp`、`[mcp]` 設定、migration 0024、`celerisctl mcp client add|ls|revoke` / `mcp stdio`、
  上の tools と resources（D3 の `skills_mounts` は profile の欄と `org_*` / `skills_*` の読み書きまで。run への届け方は Phase 79）。
  テストは**偽の MCP クライアント**（HTTP で `initialize` → `tools/list` → `tools/call`）で: スコープごとに `tools/list` が変わる、
  認証なし/失効トークンの 401、`knowledge_propose` が `_inbox` に候補を置き出典に `mcp:<client>` が入る、秘密の拒否、
  `console_instruct` が `author = mcp:<client>` の発言を作り `console_reply` が返事と actions を返す、`org_create_node` の `tools` /
  `permissions` 無視、流量制限、`mcp_calls` の記録、stdio 橋の往復。`docs/mcp.md`（接続手順: ChatGPT の Secure MCP tunnel / Claude Code /
  Codex の設定例、スコープ、運用）。`docs/gui/api.md` に管理 API。
  実機: `celerisctl mcp client add chatgpt`、curl で `initialize` と `tools/list`、`knowledge_propose` 1 件が `_inbox` に入る、
  `console_instruct` 1 件に CoS が返事する（`console_reply` で取る）。
- **Phase 79（D3）**: `Profile.skills_mounts` の継承、`RunContext.skills`、claude-code / codex / acp への届け方、`request.json` の記録、
  fake アダプタのテスト（作業場所に `.claude/skills/<name>/SKILL.md` が現れる、AGENTS.md に節が足される、前置きに載る）。
  実機: `skills_put` で 1 つ置き、`org_mount_skill` で engineering に mount、coding のタスク 1 件の `request.json` と作業場所で確認。
- どの Phase も `cargo test --workspace --no-fail-fast` / clippy / GUI 一式、PROGRESS の実機の証跡。**トークンの値はログ・応答・PROGRESS に出さない。**

## Phase 78 追記（2026-09-21。D1・D2・D4・D5 と D3 のデータモデルを実装したときの逸脱と細部）

実装は ADR の決定どおり。**決定を変えた点は無い**（口の複数化・`auth = "none"` の `client` 固定は
実装前にコーディネーターから ADR 本文そのものへの追記として指示され、この節はその後の実装）。
書いていなかった細部と、あえて別のやり方にした点だけを残す。

### 決めた細部（ADR が書いていなかったこと）

- **P-78-a: `Message.metadata.author` は新しい列を増やさず、既存の `metadata_json`（migration 0017）に
  足した。** ADR-0048 D3 が「CoS の返事の actions 結果」専用に用意した列だが、`role = user` の行にも
  同じ列があるので流用した（migration 0024 は `mcp_clients` / `mcp_calls` の 2 表だけで、`messages` には
  触れない）。`MessageMetadata::is_empty()` に `author.is_none()` を足し、`ConsoleBlock::Human.author`
  へそのまま写す（`crates/task-api/src/console.rs::message_block`）。
- **P-78-b: `console_instruct` は CoS（`COS_ID`）以外に話しかけられない。** ADR-0048 D3 の
  `POST /console/instruct` は `scope=node:<id>` や `@<node-id>` でノードを指名できるが、ADR-0056 D2 の
  `console_instruct { text, project_id? }` にはその余地が無い（そもそも仕様が node を受けない）。
  `task_ops::conversation::start_as`（新設。`start`/`start_with_milestone` と同じ芯 `start_full` を
  共有し、`author: Option<&str>` だけ追加）は任意の `node_id` を受けられるが、celeris-mcp 側は常に
  `COS_ID` を渡す。`console_reply` も同じ前提（`message_page(Some(COS_ID), …)`）で返事を探す。
- **P-78-c: skills（D3 の置き場）は KB の `skills/` を `_inbox`/`_retired` と同じ「索引・検索から除く
  専用ディレクトリ」にした。** `task_core::knowledge::{SKILLS_DIR, is_skills}` を追加し、
  `task_ops::knowledge::{walk, grep}` から除いた。SKILL.md 自身の frontmatter は KB の front matter
  （`title`/`tags`/`scope`/…）と語彙が違う（`name`/`description`。Claude Code の skills 形式）ので、
  `task_core::knowledge::front_matter` は使わず、`task_ops::knowledge::skill_frontmatter`
  （`---\nkey: value\n---`だけを読む最小パーサ）を別に書いた。
- **P-78-d: `skills_put` の冪等な書き直しが `git commit` を失敗させていたのを直した。** 同じ内容を
  2 回書く（`source:` が既に入っている skill_md をそのまま渡す等）と `git add` が何もステージしない。
  既存の `commit_paths`（`record`/`inbox_accept`/`apply_candidates` と共有）に `git diff --cached
  --quiet` を挟み、変更が無ければ新しいコミットを作らず今の `HEAD` を返すようにした（他の呼び出し側は
  常に新しいファイル名を使うので影響なし）。
- **P-78-e: `org_create_node` の `tools`/`permissions`/`review` は「受け取っても無視」。** MCP の
  `ProfileInput`（`celeris-mcp::tools::org`）は JSON としては `tools`/`permissions`/`review` を
  受け付けるが（`deny_unknown_fields` で丸ごと拒否すると「なぜ無視されるのか」が見えにくいと判断）、
  `Profile` に組み立てる段で必ず空 / 既定値に落とす（`ProfileInput::into_profile`）。
- **P-78-f: `McpScope` の JSON 表現は `as_str()` と同じ `"knowledge:read"` 形（`:` 入り）。**
  `#[serde(rename_all = "snake_case")]` だと `"knowledge_read"` になり、スコープ文字列
  （`celerisctl mcp client add --scope`・DB の `scopes` 列・MCP ツールのスコープ判定）と食い違うため、
  `Serialize`/`Deserialize`/`JsonSchema` を手で実装した（`task_core::mcp::McpScope`）。
- **P-78-g: `resources/list` は知識の索引・組織・skills だけを列挙する。** タスク・案件は件数が
  非有界（KB や組織と違って `MAX_INDEX_ITEMS` のような上限がそもそも無い）なので、`tasks_list` /
  `projects_list` で id を知ってから `resources/read` で読む前提にした（`docs/mcp.md` §5 に明記）。
- **P-78-h: JSON-RPC はバッチ（配列）を受けない。** MCP 2025-06-18 の仕様は 1 要求 1 応答が基本で、
  celeris の他クレートも 1 要求単位の決定的処理を好む流儀なので、`POST /mcp` の本文は単一の JSON-RPC
  オブジェクトだけを受ける（配列を送ると `-32600`）。
- **P-78-i: `GET /mcp` は 405。** ADR-0056 D5 が明示的に許した簡略化（サーバー起点の SSE 購読は
  実装しない）。`POST /mcp` の応答も常に JSON（`Accept: text/event-stream` を見ても JSON を返す）。
- **P-78-j: セッション（`Mcp-Session-Id`）とレート制限のカウンタはプロセスのメモリだけに持つ。**
  celeris の再起動（`--reload` の対象外なのでプロセス自体は再起動が要る）でセッションは失効し、
  クライアントは `initialize` からやり直す（DB には残さない。`mcp_clients`/`mcp_calls` だけが永続）。
- **P-78-k: `console_reply` の `Failed`/`Cancelled` の形は ADR に明記が無かったので決めた。**
  `Done { reply, actions[] }` は ADR どおり。`Failed { reply: Option<String> }`（対話 run が失敗しても
  `failure_reply` が書いた返事があれば返す）、`Cancelled`（本文なし）を追加した。
- **P-78-l: トークンの生成は新しい crate を足さず `ulid`（既存依存）を 3 本つないで作った。**
  `rand` は workspace の `Cargo.lock` に間接依存として複数バージョンが既にあるが、`celeris-mcp` が
  直接使う体では無かったため、ADR-0056 D5「外部クレートは足さない」の精神に沿って避けた
  （`celeris_mcp::auth::generate_token`）。
- **P-78-m: `celeris-mcp` は `task-api` と同じ「専用の `SqliteStore` 接続を自分で開く」流儀。**
  `McpState::open` が `[mcp]` の口とは別に DB を開く（`ApiState::new` と同じ多重接続。WAL なので
  問題ない）。`celerisctl mcp client …` も同様に DB を直接開く（`knowledge rerun` と同じ管理系）。
- **P-78-n: `mcp_clients.id` は `celerisctl mcp client add <name>` の `<name>` をそのまま使う
  （ULID を新しく振らない）。** D1 の `[[mcp.listeners]] client = "chatgpt"` の例が
  `mcp client add chatgpt` の名前をそのまま指しており、`add` の引数が名前 1 つしか無い
  （別に id を選ばせる引数が無い）ことから、`id = name` と読むのがいちばん驚きが少ない。
  同じ名前で 2 回 `add` すると（`id` が主キーなので）友好的なエラーになる。`org_nodes.id`
  （人が選ぶ小文字ケバブの id）と同じ発想で、`tasks`/`projects` の ULID とは違う流儀。

### GUI（D2 の `author`。Phase 78 の範囲内で最小限）

- `Message.metadata.author` → `ConsoleBlock::Human.author` → GUI の `human` ブロックに
  「外部（<mcp: を外した client_id>）」の `Badge` を 1 つ出すだけ（`gui/app/components/
  ConsoleBlockItem.tsx`）。MCP クライアントの表示名解決（`GET /mcp/clients` の `name` を引く）は
  していない（生の `client_id` を見せる最小実装。詳細は `gui/docs/PROGRESS.md` の該当節）。
- 「アカウント」画面の「MCP クライアント」節（`GET /mcp/clients`/`GET /mcp/calls` を実際に呼ぶ画面）は
  ADR-0056 D4 が「後続の GUI Phase」と明記したとおり、今回はやっていない。

### やっていないこと（ADR のとおり Phase 79）

- `RunContext.skills` / claude-code・codex・acp への実際の届け方 / `request.json` への記録。
- 「アカウント」画面の MCP クライアント節（管理 API はあるが GUI からはまだ呼ばない）。

### 実機（このセッションでは未実施。ADR-0009 P-34）

`docs/mcp.md` §8 に手順を書いた（`celerisctl mcp client add`、`curl` での `initialize` /
`tools/list` / `knowledge_propose` / `console_instruct` + `console_reply`）。認証・ネットワークが
使える環境の人（またはエージェント）が実行し、結果を `docs/PROGRESS.md` の Phase 78 節に追記すること。

## Phase 79 追記（2026-09-21。D3: mount した skills を run に届ける）

実装は ADR の決定どおり（届け方: claude-code はファイルコピー、codex は `AGENTS.md` の節、acp は前置き）。
**決定を変えた点は無い**。ADR がデータ形しか書いていなかった `RunContext.skills[]` の中身と、
アダプタごとの実装細部だけを残す。

### 決めた細部（ADR が「届ける」としか書いていなかったこと）

- **P-79-a: `SkillMount` は `{name, path, description}` の 3 つだけで、`SKILL.md` の本文は運ばない。**
  `path` は KB の `skills/<name>/`（ディレクトリ）の絶対パス。本文はアダプタが起動直前にそこから読む
  （`crates/task-worker/src/skills.rs::read_skill_md`）。`RunContext`（`request.json` に残る）を
  肥大させないのと、`knowledge` の「索引だけ渡す、本文は道具で読む」という既存の設計原則
  （`docs/adr/0047-*` D2）に揃える判断。`description` は `SKILL.md` の frontmatter から
  `task_ops::knowledge::skill_description`（新設。Phase 78 の `skill_frontmatter` を再利用）で抜く。
- **P-79-b: 見つからない skill は「`status` の進行イベントを 1 行出して run は続ける」を、
  ディスパッチャの `run_extras` ではなく呼び出し元（`dispatch_ready` 相当。run_id が確定した直後）で
  行う。** `run_extras` 自身はストアに書き込まない純粋寄りの関数（既存の `knowledge_fallback` の
  `status` 行も同じ場所で出している）ので、その並びに合わせた。`RunExtras.missing_skills:
  Vec<String>` を新設し、`extras.skills`（KB に実在したものだけ）とは別に運ぶ。
- **P-79-c: `claude-code` の削除は「対象の skill ディレクトリだけ」に絞った。**
  `.claude/skills/<name>/` が既にあれば `remove_dir_all` してから丸ごと写す（run ごとに新しい内容へ
  置き換える。stale なファイルが残らない）が、`.claude/skills/` 配下の**他の**skill ディレクトリや
  `.claude/settings.json` 等には一切触れない（`crates/task-worker/src/skills.rs::deliver_claude_code`。
  fake アダプタのテスト `deliver_claude_code_only_touches_its_own_skill_directories` で確認）。
- **P-79-d: `codex` の `AGENTS.md` 節は `<!-- celeris:skills:start -->` 〜 `<!-- celeris:skills:end
  -->` の HTML コメントで区切り、その中だけを run ごとに書き直す（`rewrite_agents_md`。純粋関数、
  テスト容易）。** 区切りの外側（人や他の仕組みが書いた内容）は常に保つ。`skills` が空の run は
  `AGENTS.md` に一切触れない（無ければ作らない。ADR-0056 D3 の「既存の AGENTS.md は壊さない」を、
  「そもそも今回 skills が無ければ何もしない」まで広げた解釈）。
- **P-79-e: `acp` は前置き（プロンプト文面そのもの）に直接埋め込む。** acp（opencode）はファイルを
  自動で読む契約が無い（ADR-0054 Phase 68 の判断と同じ理由: ACP には claude-code の `.claude/`・
  codex の `AGENTS.md` に相当する「エージェントが自動で読む規約」が無い）ので、`build_prompt` が
  組んだ本文の末尾に `crate::skills::preamble_section` の出力をそのまま足す
  （`crates/task-worker/src/acp.rs::run_acp`）。`claude_code::build_prompt` は `claude-code` と
  `codex` にも共有されているため、共有関数自体は変えず、`acp.rs` 側だけで `prompt` 文字列に追記した
  （claude-code / codex のプロンプトは Phase 78 までと 1 バイトも変わらない）。
- **P-79-f: 前置き用の `## Skills（celeris）` 節の組み立て（本文の読み込みを含む）は
  `preamble.rs` ではなく新設の `crates/task-worker/src/skills.rs` に置いた。** `preamble.rs` の
  冒頭コメントが「ここは純粋関数だけで、I/O も LLM も無い」と明記しており、`knowledge_section` も
  索引（メタデータ）だけを受けて本文は読まない設計なので、ファイル I/O を要する skills の本文読み込み
  はその外に出した（`skills.rs` は `delegate_file.rs` と同じ「アダプタ共有の小道具」の位置づけ。
  `lib.rs` に `pub mod skills;` を追加）。
- **P-79-g: 研究系アダプタ（paperqa / local-deep-research / langmem）は変更していない。**
  `RunContext.skills` は既定で空 Vec（`skip_serializing_if`）なので、これらのアダプタの prompt.txt /
  request.json は Phase 78 までと 1 バイトも変わらない（触っていないことを確認するテストは追加して
  いない。研究系はそもそも `preamble::render` を使わない設計 — `docs/adr/0033-*` 参照 — なので
  `context.skills` を読む経路自体が無い）。

### やっていないこと（Phase 79 のスコープ外）

- GUI（「アカウント」画面の MCP クライアント節）は引き続き未着手（ADR-0056 D4 が明記した後続 Phase）。
- `org_mount_skill` した skill を実際に外部（ChatGPT 等）から `skills_put` で書く実機確認（ADR-0009
  P-34。認証・ネットワークが使える環境の人・エージェントに依頼）。

### 実機（このセッションでは未実施。ADR-0009 P-34）

`docs/PROGRESS.md` の「Phase 79」節に手順を書いた（`skills_put` で 1 つ置く、`org_mount_skill` で
engineering に mount する、coding のタスクを 1 件流して `request.json` と作業場所を見る）。認証・
ネットワークが使える環境の人（またはエージェント）が実行し、結果を同節に追記すること。
