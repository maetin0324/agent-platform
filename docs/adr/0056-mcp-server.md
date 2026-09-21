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
