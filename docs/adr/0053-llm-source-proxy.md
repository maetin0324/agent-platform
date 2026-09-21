# ADR-0053: LLM source は供給元の抽象 — ログイン済みの Claude Code / Codex の認証情報を使うローカル OpenAI 互換プロキシと、切れにくい Qwen トンネル

- 日付: 2026-09-21
- 状態: **Accepted**（人の指示: 「claude code や codex は各コンフィグディレクトリに API キーがあるはず。openclaw などはログイン済みの
  claude code や codex から API キーを取得しバックエンドとして利用する実装があるので、API key 取得まで行けば claude と GPT の LLM source
  抽象化が可能。プロバイダーは LLM source 抽象のレイヤで、ハーネスと密結合になるのは好ましくない。claude を opencode で動かせたり
  研究リサーチハーネスで動かせたりするといい」「特定の LLM source に依存せずにタスクを実行し続けられる基盤のほうが重要。Qwen アクセス用の
  pegasus ssh トンネルのセッション切れ・TOTP 再入力もできる限り減らしたい。残り budget に応じて無料無限の Qwen をどれだけ使うかの判断が
  できるように」。任意のハーネスが Claude / ChatGPT の枠を食うことは許容）
- 関連: ADR-0049（供給元を固定しない。tier・残量で決定的に選ぶ）、ADR-0024 / 0025（アカウントプール。`claude-accounts/<id>/`、
  `codex-accounts/<id>/`）、ADR-0032（クラスタ接続。TOTP 中継、ssh master）、ADR-0047 D4 / ADR-0052（LangMem の接続先）

## 1. 決定

### D1. `celeris` が **ローカルの OpenAI 互換プロキシ**を提供する（`[llm_proxy] listen = "127.0.0.1:18100"`）

- エンドポイント: `GET /v1/models`、`POST /v1/chat/completions`（stream 対応）、`POST /v1/embeddings` は 501。認証は `Authorization: Bearer <api.token>`
  （loopback のみ bind）。
- **モデル名は抽象**: `celeris/<tier>`（`frontier` / `standard` / `cheap`。供給元はプロキシが選ぶ）、`claude/<tier>`、`gpt/<tier>`、
  `qwen/<tier>`（明示）。実モデルへの写像は `[llm_proxy.models]`（既定: Claude は ADR-0049 の tier 写像、GPT は `gpt-5*` の写像、
  Qwen は `qwen3.8-27b`）。
- **供給元（LLM source）の種類**:
  1. `claude-oauth`: `claude-accounts/<id>/.credentials.json` の `claudeAiOauth.accessToken` を **Anthropic Messages API**
     （`https://api.anthropic.com/v1/messages`、ヘッダ `Authorization: Bearer <token>`、`anthropic-beta: oauth-2025-04-20`、
     `anthropic-version: 2023-06-01`）に使い、OpenAI 互換の要求/応答へ双方向に写す（messages・system・tools・stream の SSE を含む）。
     `expiresAt` が過ぎていれば `refreshToken` で更新し、ファイルに書き戻す（Claude Code と同じ形式）。
  2. `codex-oauth`: `codex-accounts/<id>/auth.json` の `tokens.access_token` / `tokens.account_id` を **ChatGPT の Codex backend**
     （`https://chatgpt.com/backend-api/codex/responses`、ヘッダ `Authorization: Bearer`、`chatgpt-account-id`、`OpenAI-Beta: responses=experimental`、
     `originator: codex_cli_rs`）に使う。Responses API ↔ chat/completions を写す。期限切れは `refresh_token` で更新
     （`https://auth.openai.com/oauth/token`、client_id は codex-cli のもの）し、ファイルに書き戻す。
  3. `openai-compatible`: 既存の Qwen（`http://127.0.0.1:18000/v1`）などをそのまま中継。
- **選択は決定的**（ADR-0049 の規則をそのまま使う）: 抽象モデル `celeris/<tier>` は、(a) 到達可能で無料の `qwen` を最優先
  （`[llm_proxy] prefer_free = true`）、(b) 次にアカウントプールの残量スコア（Claude / Codex を跨いで比較。枯渇・未ログイン・cooldown は
  飛ばす）、(c) 同点は設定順。`claude/<tier>` / `gpt/<tier>` はその供給元の中でアカウントを選ぶ。選んだ供給元とアカウントは応答ヘッダ
  `x-celeris-source` / `x-celeris-account` に出し、`llm_proxy_requests` 表（migration。request id・source・account・model・tokens・latency・
  status）に残す。429 / 401 を受けたらそのアカウントに cooldown を付け（ADR-0024 の観測と同じ表）、**同じ要求を次の候補でやり直す**
  （stream 開始前なら。開始後は切断を返す）。
- 残量の観測は既存（Claude の usage、Codex の `account/rateLimits/read`）を使い、プロキシの使用量も加算する。

### D2. ハーネスは供給元を知らない（プロバイダ = LLM source）

- `[[providers]]` に **`source = "proxy"`** の供給元を足せる: `adapter = "acp"`（opencode）や `paperqa` / `local-deep-research` / `langmem` が
  `base_url = http://127.0.0.1:18100/v1`、`model = "celeris/standard"` などを使う。opencode の設定（`tools/opencode/*.json`）はプロキシを
  1 つの OpenAI 互換プロバイダとして登録する（モデル名 = 抽象名）。これで「claude を opencode で」「Claude / GPT で PaperQA」ができる。
- `claude-code` / `codex` アダプタは従来どおり CLI をアカウントの設定ディレクトリで起こす（CLI 自身が認証する）。プロキシは
  それ以外のハーネスのため。**ハーネスの契約（入出力・指示文）は供給元に依存しない**（ADR-0049 D1 のまま）。
- 既定の設定: `paperqa-qwen` / `ldr-qwen` / `opencode-qwen` / `langmem-main` の `base_url` をプロキシに向け、モデルを `celeris/cheap`
  （PaperQA は `celeris/standard`）にする。Qwen が生きていれば従来どおり Qwen、落ちていれば Claude / GPT に自動で倒れる（ADR-0052 の
  フォールバックはプロキシの中に吸収される。ADR-0052 D1 の probe は不要になるが、残しても害はない）。

### D3. Qwen トンネルは `celeris` が張り、切れにくくする

- 今の `celeris-qwen-tunnel.service`（systemd。`-O check pegasus` が通るときだけ起きる）を `celeris` の中に取り込む:
  `[[clusters]] pegasus` の ssh master（ADR-0032。TOTP は GUI から 1 回）に **`-O forward -L 127.0.0.1:18000:127.0.0.1:18000`** を
  master 経由で足す（`ssh -O forward -L … pegasus`、bnode150 へは pegasus 上の ProxyJump ではなく master の port forward を使う。
  bnode150 が直接届かないなら master 上で `ssh -N -L` を起こす）。master は **`ControlPersist=yes`（無期限）＋ `ServerAliveInterval=30` /
  `ServerAliveCountMax=3`** で張り、切れたら **TOTP を要求する前に鍵認証を試し**、それでも駄目なときだけ人に TOTP を頼む
  （Discord 通知 `cluster_login_needed`、ADR-0037 の種を 1 つ足す）。tick ごとに `-O check`、forward の生存は `/v1/models` の probe。
- 目標: TOTP は「pegasus 側のセッション寿命」に 1 回。切れたことと復帰したことは Console と Discord に出る。
- systemd の `celeris-qwen-tunnel.*` は配備後に無効化する（`install-units.sh --remove-old` の対象に足す）。

### D4. 予算に応じた Qwen の使い方（可視化まで）

- `GET /llm/sources` → 供給元ごとの到達性・残量（短期/長期）・cooldown・直近 1 時間の要求数と token 数。GUI「アカウント」画面に
  「LLM source」の節。`celeris/<tier>` がどこに倒れているかが見える。判断（Qwen をどれだけ使うか）は人が `prefer_free` と tier 写像で
  決める。自動の予算配分はこの ADR では作らない。

## 2. 採らない

- 供給元ごとの独自 API をハーネスに直接教える（プロキシに閉じる）。
- OAuth トークンを DB や別の場所に写す（CLI のファイルをそのまま読む。更新も同じファイルに書く）。
- Qwen トンネルの TOTP 自動入力（人の操作。頻度を減らすだけ）。

## 3. 受け入れ条件

- **Phase 65（D1・D2）**: プロキシ（`crates/llm-proxy` または `celeris` 内のモジュール）: `/v1/models`、`/v1/chat/completions`（非 stream / stream）、
  Anthropic 写像・Codex Responses 写像・OpenAI 互換の中継、トークン更新、決定的な選択（free 優先 → 残量 → 設定順）、429/401 の cooldown と
  やり直し、`llm_proxy_requests`。テストは**偽の上流**（ローカル HTTP）で: 3 種の写像の往復、stream の SSE、期限切れの更新、429 → 次の候補。
  実機: 本番の Claude アカウントで `curl` 1 回（`celeris/cheap` → Qwen が落ちているので Claude に倒れる）、opencode と LangMem をプロキシに向けて
  1 タスク完走。`docs/llm-source.md`。
- **Phase 66（D3・D4）**: master 経由の forward、生存監視、鍵→TOTP の順、Discord の種、`GET /llm/sources` と GUI。実機: トンネルが切れた状態から
  GUI の TOTP 1 回で復帰し、`celeris/cheap` が Qwen に戻ること。systemd の tunnel unit の撤去。
- どの Phase も `cargo test --workspace --no-fail-fast` / clippy / GUI 一式、PROGRESS の実機の証跡。**認証情報の値はログ・応答・PROGRESS に出さない。**

## Phase 65 追記（2026-09-21。D1・D2 の実装）

- `crates/llm-proxy`（新クレート。`task-core`/`task-dispatch` に依存し、`task-worker`/`task-api` からは
  依存されない側に置いた。`GET /llm/sources` は task-api が `LlmSourcesReader` トレイトで受け取る
  薄い包みを celeris が渡す形にし、task-api が `task-dispatch`/`task-worker` を知る境界を破らないように
  した。ADR-0017 M2 と同じ配慮）。
- **migration の番号**: Phase 64（ADR-0052 の知識整理 run リトライ）と並行して開発したため、この
  worktree には Phase 64 の `0021_knowledge_run_retry.sql` が無い。連番を切らさずに `0022` を
  当てるため、`crates/task-core/migrations/0021_reserved_for_knowledge_run_retry.sql`（no-op の
  予約）を置いた。**merge 時**: このファイルと `store.rs` の `21 => Ok(MIGRATION_0021)` を消し、
  Phase 64 の本物の `0021_knowledge_run_retry.sql` に置き換える（`SCHEMA_VERSION` は `22` のまま）。
- **claude-oauth のトークン更新エンドポイント / client_id は実機で確認していない**（Claude Code CLI の
  公知の値を既定にしたが、このセッションでは本物の資格情報ファイルを読まない制約のため検証できず、
  `[llm_proxy.sources.claude_oauth] token_url` / `client_id` で上書きできるようにしてある）。
- **opencode（D2 の 4 プロバイダのうち 1 つ）は明示的に無効のまま出した**: opencode の
  `"model": "<providerId>/<modelId>"` が `modelId` 内のスラッシュ（`celeris/cheap`）をどう扱うか、
  実機（またはソース）で確認できなかったため。`config/celeris.acp-opencode.example.toml` は既定を
  直接 Qwen のままにし、JSON テンプレートと注意点は `docs/llm-source.md` §7 にコメントで示した
  （too hard な部分を偽装しない、という受け入れ条件どおりの判断）。
- 他の 3 プロバイダ（PaperQA / LDR / LangMem）は素直な `env`/`settings` の書き換えで確認できたので、
  `config/celeris.research.example.toml` / `celeris.web-research.example.toml` /
  `celeris.example.toml`（`[knowledge.langmem]`）/ `docs/knowledge.md` をプロキシへ向けた（既存の
  `Config::load`/`validate` のテストで実際に読めることを確認済み）。
- **実機確認**: このサンドボックスには実際の Claude/Codex 資格情報も外向きネットワークも無い
  （CLAUDE.md の禁止事項、かつ ADR-0009 P-34 の「使えなければ手順を書いて人間に依頼する」に従う）。
  `docs/llm-source.md` §8 に curl での確認手順を書いたので、認証が使える環境の人（またはエージェント）
  が実行し、結果を PROGRESS に追記すること。

## Phase 65b 追記（2026-09-21。codex-oauth の上流エラーの可視化と Codex CLI 互換の要求形）

- **観測（本番、2026-09-21 08:47–08:55 UTC、release `0b1815cab8f6`）**: `celeris/cheap` →
  claude-oauth は 200/`pong` で動くのに、`gpt/*`（codex-oauth。cheap=gpt-5-mini、standard=gpt-5、
  frontier=gpt-5-codex。stream の有無・`max_tokens` の有無に関わらず）は全て 0.4 秒以内に
  `400 {"error":{"message":"upstream error","type":"upstream_error"}}` で落ちていた。ログには
  `candidate failed before any bytes were sent; trying the next one … kind: upstream` しか出ず、
  上流の本文が見えないため原因を特定できなかった。認証は正常（`account_check … result ok`、401 は
  出ていない）。
- **原因**: ChatGPT の Codex backend（`.../backend-api/codex/responses`）は Codex CLI（`codex-rs`）
  が送る形以外を拒否する。Phase 65 の実装は `store` を送らず、`stream` をクライアントの値のまま
  送り、`instructions` を system メッセージが無いときは省略し、`temperature`/`max_output_tokens`
  を常に転送していた。これらのどれか（複数の可能性が高い）が拒否の原因だった。
- **変更**:
  1. `crates/llm-proxy/src/sources/codex.rs`: 上流へは常に `store: false`・`stream: true` を送り、
     `instructions` は system が無ければ既定の一文を入れ、`temperature`/`max_output_tokens` は
     既定では送らない（opt-in）。`parallel_tool_calls: true` と、`tools` があれば既定
     `tool_choice: "auto"` を付けた。`reasoning_effort` を設定したときだけ `reasoning`/`include`
     を付ける。ヘッダに `User-Agent: codex_cli_rs/<version>` を追加した（値は未確認。設定で上書き
     可能）。クライアントが非 stream を求めたときは、常に stream で要求した上流の SSE をこの層で
     集約して 1 つの `chat.completion` に組み立てる（`aggregate_stream`）。既存の streaming 経路
     （`build_chunk_stream`/`ResponsesStreamMapper`）は変えていない。
  2. 上流が非 2xx を返したとき、`error.message` と上位の `detail`/`message` の両方を見て 300 文字
     までの要約を作り（`extract_error_summary`）、WARN ログ（`status`/`summary`）とプロキシの
     エラー応答（`error.message`）の両方に出す。`llm_proxy_requests.error_kind` は変えていない
     （migration は増やさない。本文はログと応答にだけ残る）。
  3. `crates/llm-proxy/src/config.rs` の `[llm_proxy.sources.codex_oauth]` に
     `user_agent` / `send_sampling_params` / `reasoning_effort` を追加した。
  4. **コーディネーターからの追加指示**（同じ Phase 内。ADR-0052 D1 の到達性 probe）:
     `[knowledge.langmem].base_url` を `llm-proxy`（`/v1/models` が Bearer を要求する）に向けると、
     従来の probe は 401 を「落ちている」と誤認して永久にフォールバックしてしまう。
     `crates/task-worker/src/probe.rs` の `probe_models` に `bearer_token: Option<&str>` を足し、
     `[knowledge.langmem].api_key_secret` から解決した値（`crates/celeris/src/config.rs`
     `dispatch_config()` が解決し、`task_dispatch::KnowledgeRuntimeConfig.langmem_api_key` に運ぶ。
     `crates/task-dispatch/src/dispatcher.rs` の `KnowledgeProbe`/`knowledge_reachability` も
     第 2 引数を通す）で `Authorization: Bearer` を送る。**401/403 は `Unreachable` ではなく
     `Unknown`**（＝従来どおり `langmem` で走らせる。認証エラーは「LLM が落ちている」ことを意味
     しない）に分類を変えた。トークンの値はどのログにも出さない。
- **テスト**（すべて偽の上流。外部ネットワークには出ない）:
  - `crates/llm-proxy/src/sources/codex.rs` の単体テスト 6 件（要求の形が既定で
    `store:false`/`stream:true`/`instructions` あり/`temperature`・`max_output_tokens` 無し、
    system がある場合の `instructions`、`send_sampling_params`/`reasoning_effort` の opt-in、
    `extract_error_summary` が `detail`/`error.message`/`message` を見ること、300 文字で切ること）。
  - `crates/llm-proxy/tests/proxy_integration.rs`: `codex_non_stream_round_trip`
    （非 stream クライアント要求が、上流には `store`/`stream`/`instructions`/欠落フィールドの形で
    送られ、SSE 上流からの tool call + usage が 1 つの集約応答になること）、
    `codex_tool_call_round_trip`、`codex_stream_round_trip`（変わらず動くことを確認）、
    `codex_400_upstream_error_surfaces_the_detail_field`（`{"detail":"Store must be set to false"}`
    がプロキシのエラー本文にそのまま出ること）。
  - `crates/task-worker/src/probe.rs`: `a_bearer_token_is_sent_as_an_authorization_header`
    （ヘッダが実際に送られる）、`without_a_bearer_token_the_same_upstream_answers_401`、
    `a_401_or_403_from_the_probe_is_unknown_not_unreachable`（401/403 が `Unknown` になり
    `should_fall_back()` が `None` を返すこと）。
  - `crates/task-dispatch/src/dispatcher.rs`:
    `the_resolved_api_key_is_passed_to_the_knowledge_probe`。
  - `crates/celeris/src/config.rs`: `dispatch_config_resolves_the_langmem_api_key_from_secrets`。
- **ゲート**: `cargo test --workspace --no-fail-fast` exit 0（1623 passed / 0 failed。doctest 含む
  全クレート）。`cargo clippy --workspace --all-targets -- -D warnings` exit 0（警告 0）。
  非テストコードに `unwrap()` を増やしていない。
- **実機確認は未実施**（このセッションには本物の Codex 資格情報も外向きネットワークも無い。
  ADR-0009 P-34）。`docs/llm-source.md` §8 の手順 4b（`gpt/cheap` への 1 回の curl）を、認証が
  使える環境の人（またはエージェント）が実行し、結果を PROGRESS に追記すること。
