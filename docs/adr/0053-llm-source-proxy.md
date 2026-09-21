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

## Phase 66 追記（D3・D4。2026-09-21）

D3（Qwen トンネルを celeris が張る）と D4（`GET /llm/sources` の可視化を GUI に出す）を実装した。
決定・逸脱は次のとおり。

- **D3 の構成**: `[[clusters]]` に `forwards`（`[[clusters.forwards]] listen / target`）を足した
  （`task_dispatch::dispatcher::ClusterForwardSpec`。Qwen 専用ではなく、ssh master 越しの port
  forward 全般として作った）。`Dispatcher::refresh_cluster_tunnels`（`refresh_cluster_liveness` と
  同じ 5 秒間隔）が: (1) master が死んでいれば、**既存の `ClusterConnector`（ADR-0032 D3 の鍵認証
  フック）をそのまま再利用して**接続を試す（`auth` の値に関わらず呼ぶ — `"totp"` のクラスタでも
  「TOTP を要求する前に鍵認証を試す」ため）。(2) 成功すれば forward ごとに `TunnelForwardEnsurer`
  で(再)確立 → `TunnelProbe` で再確認。(3) 鍵認証も失敗したクラスタは `cluster_login_needed`
  に立て、**同じ outage の間は 1 回だけ**報告を書く（`clear_login_needed` で解除するまで再度は
  立たない。`refresh_cluster_liveness`/`ClusterConnector` を流用したことで、ADR-0032 の「TOTP は
  人の操作でだけ」の原則をそのまま守れている）。
- **celeris 側の実装**（`crates/celeris/src/lib.rs`）: `tunnel_forward_ensurer` は
  `ssh -o BatchMode=yes -O forward -L <listen>:<target> <host>` を試し、失敗したら
  **フォールバックとして `ssh -o BatchMode=yes -N -L <listen>:<target> <host>` を別プロセスとして
  spawn**（ADR-0053 D3 の「bnode150 が直接届かないなら master 上で `ssh -N -L` を起こす」を
  文字どおり実装）。この子プロセスは `TunnelForwardRegistry`（`Arc<Mutex<HashMap<String,
  std::process::Child>>>`、Drop で全部 kill）に保持し、生きている間は二重に起こさない。
  `tunnel_probe` は `task_worker::probe_models(&format!("http://{listen}/v1"), …, None)`
  （Qwen 中継そのものは celeris の外なので bearer は付けない。ADR-0052/Phase 65b の
  `bearer_token` 引数はプロキシの `/v1/models` 用で、これとは別物）。
  **どちらも `std::process::Command` の同期呼び出し**（`ClusterConnector` と同じ流儀。tick は
  同期関数なので、非同期ランタイムを持ち出さない。`cluster_connector` が使う一時ランタイムより
  単純: forward の確立はブロッキングな `ssh` 呼び出し 1 回で終わる）。
- **逸脱（D3）**: ADR は「tick ごとに `-O check`」と書いていたが、実装は既存の
  `refresh_cluster_liveness`（5 秒間隔、`CLUSTER_LIVENESS_INTERVAL`）が求めた `cluster_connected`
  を**そのまま読む**（`ensure_cluster_master_for_tunnel` は `cluster_connected` が `true` ならそこで
  終わる）。二重に `-O check` を打たない設計判断で、ADR の「tick ごとに `-O check`」の精神
  （新鮮さを保つ）は間隔を共有することで満たしている。
- **通知（Discord）**: 新しい `NotificationKind::ClusterLoginNeeded`（`"cluster_login_needed"`）を
  `task_core::notify` に足した。`ReportKind` は増やさず、`report_for_cluster_login_needed` が
  `bad_news` の見出しに `"<host> は TOTP ログインが必要"` という決まった接尾辞を付け、
  `celeris::notify::scan_cluster_login_needed` がその接尾辞だけを拾う（`scan_bad_news` 側は同じ
  接尾辞を除外し、二重に鳴らさない）。`key` = 報告 id なので、outage ごとに新しい報告 → 新しい
  key → 次の outage でまた 1 回鳴る（DB の `(kind, key)` 恒久 dedup と「1 outage = 1 回」を
  両立させる、Dispatcher の in-memory dedup との組み合わせ）。
- **可観測性**: `Dispatcher` は直近のトンネル状態遷移（Up/Down/Restored/LoginNeeded）を
  `tunnel_events`（上限 100 件の VecDeque、`take_tunnel_events` で取り出す）に積む。celeris は
  今回これを Discord/報告以外の専用ログ・GUI タイムラインには配線していない（`tracing::info!` には
  出る）。**「Console に見える」は `GET /clusters` の `tunnel_forwards[].up` / `tunnel_login_needed`
  （GUI の /clusters 画面）で満たした**。task 単位のイベント列（`task_core::Event`）に積む設計は
  採らなかった（トンネルはタスクに紐づかないシステム全体の状態なので、per-task イベントログに
  混ぜるのは筋が悪いと判断した）。`take_tunnel_events` は将来 GUI 専用のタイムラインを追加すると
  きのための取り出し口として残してある。
- **D4 の拡張**: `LlmSourceAccountView` に `remaining_short`（5 時間 / Codex 週内相当）・
  `remaining_long`（7 日）を追加（`remaining` は従来どおり両者のうち厳しい方）。
  `LlmSourcesView` に `celeris_tiers: [{tier, resolves_to}]` を追加（`server.rs::resolves_tier` が
  `attempts_for` と**同じ決定的な選択**を副作用なしでなぞり、先頭候補の `source_label()` を返す。
  3 tier とも同じ選択規則なので実質同じ結果になりうるが、tier ごとのモデル写像が無ければ候補が
  空になりうるため tier ごとに計算する）。
- **GUI**（Phase G27、`gui/docs/PROGRESS.md` に詳細）: `/accounts` に「LLM source」節
  （供給元カード・`celeris/<tier>` の解決先・短期/長期残量・cooldown）、`/clusters` に
  トンネル（forward）の一覧と「ログインが必要（TOTP）」の明示（Alert。バッジには入れず全文で
  出す。ADR-0055 D1-3 の「状態バッジは 1 語」はトンネルの `up`/`down` バッジにだけ適用した）。
- **実機**: このセッションには本物の pegasus/bnode150 も TOTP も無いため、**トンネルが切れた状態
  から GUI の TOTP 1 回で復帰し `celeris/cheap` が Qwen に戻ることは未確認**（ADR-0009 P-34）。
  `docs/PROGRESS.md` の「Phase 66」節に本番の運用手順（設定キー・人が一度だけ行うこと・確認方法）
  を書いた。認証・ネットワークが使える環境の人（またはエージェント）が実行し、結果を追記すること。

## Phase 66b 追記（本番の起動パニックの修正。2026-09-21）

**観測（本番、2026-09-21 12:40 UTC、release `8a979f24048b` = main + Phase 66）**: 上の本番運用手順
どおり `[[clusters.forwards]] listen = "127.0.0.1:18000" target = "bnode150:18000"`（`auth = "totp"`
の `pegasus`）を本番設定に足して celeris を起こすと、数秒で以下の panic でデーモンが落ちた
（staging を `verify.sh` で検証する段階）。forward を外すと起動する。

```
thread 'main' (2853009) panicked at crates/celeris/src/lib.rs:621:31: Cannot start a runtime from
within a runtime. This happens because a function (like `block_on`) attempted to block the current
thread while the thread is being used to drive asynchronous tasks.
```

- **原因**: `crates/celeris/src/lib.rs:621` は `cluster_connector`（ADR-0032 D3 のクロージャ）の中で、
  ネストした `current_thread` の tokio ランタイムを作って `.block_on(...)` する。このクロージャは
  「ディスパッチループの中から**同期で**呼ばれる」（元のコメントどおり）ことを前提に書かれていたが、
  実際には `Dispatcher::tick()`（`crates/task-dispatch/src/dispatcher.rs`）が celeris の
  `tick_loop`（`async fn`、`#[tokio::main]` の既定＝**マルチスレッド**ランタイムの上で動く）から
  **インラインで**（`tokio::spawn` も `spawn_blocking` も経由せず）呼ばれており、`tick()` の実行自体が
  すでに tokio ランタイムのワーカースレッド上にある。Phase 66 の `Dispatcher::refresh_cluster_tunnels`
  は「master が死んでいれば `auth` の値に関わらず既存の `cluster_connector` を再利用して鍵認証を試す」
  （ADR-0053 D3。TOTP の前に鍵認証を試すため）という設計どおりに実装されていたため、これが
  **`auth = "totp"` のクラスタで初めて** `cluster_connector` を実行に至らせた。`cluster_connector`
  自体は ADR-0032 D3（`auth = "publickey"` の自動接続。`try_auto_connect_cluster`）のために元から
  存在したが、本番に `auth = "publickey"` のクラスタが無かったため、この「ランタイムの中でネストした
  ランタイムを `block_on` する」経路はこれまで一度も実行されておらず、テスト（`tunnel_*` 4 件）も
  `ClusterConnector`/`TunnelForwardEnsurer`/`TunnelProbe` を全部フェイクの closure に差し替えていた
  ため、実物の `cluster_connector`（celeris 側）を経由しない形でしか検証していなかった。**Phase 66
  のレビュー・ゲート（`cargo test --workspace` / clippy）はどちらもこの経路を実際には実行しないため、
  検出できなかった**（下の「テストが検出できなかった理由」を参照）。
- **修正**:
  1. `crates/task-dispatch/src/dispatcher.rs` に `run_cluster_hooks_off_async`（新規、非 pub）を足し、
     `Dispatcher::tick()` の `self.refresh_cluster_liveness()` / `self.refresh_cluster_tunnels()` の
     呼び出しをこれで包んだ。中身は「本物の OS スレッド（`std::thread::scope`）へ処理を逃がし、
     呼び出し元がマルチスレッドの tokio ランタイムの上にいるときだけ `tokio::task::block_in_place` で
     包んで join する」という形（`tokio::runtime::Handle::try_current()` と
     `Handle::runtime_flavor()` で判定）。`block_in_place` は `current_thread` ランタイムの中で呼ぶと
     panic するため、既存の `#[tokio::test]`（既定は `current_thread`）の `tunnel_*` 4 件やランタイムの
     無い素の同期呼び出しはこの分岐を通らず、`std::thread::scope` だけを使う（実 OS スレッドなので
     ネストしたランタイムを作っても常に安全）。実験で確認した性質（下記）に基づく設計。
  2. `crates/celeris/src/lib.rs` の `cluster_connector` に「呼ばれた時点で
     `tokio::runtime::Handle::try_current()` が `Ok` なら（＝上の 1. の契約が破られている）、
     ネストしたランタイムを作らずに `tracing::error!` を出して
     `Err("cluster connect must not be called from an async context")` を返す」という防御的な
     ガードを追加した（panic の代わりに、呼び出し側で cooldown に落ちるだけにする。今回の直接原因は
     1. で塞いだので、このガードは「将来また同じ形の呼び出しを誰かが async の中から直接書いてしまった
     場合」の保険）。
  3. `tunnel_forward_ensurer` / `tunnel_probe`（どちらも celeris 側。ssh の spawn と
     `task_worker::probe_models` の TCP 直叩き）はネストしたランタイムを持たないため panic はしない
     が、それぞれ数秒ブロックしうる（`ssh` の起動、`PROBE_TIMEOUT = 3` 秒）。これらは
     `refresh_cluster_tunnels()` の中から呼ばれるので、1. の `run_cluster_hooks_off_async` で
     `refresh_cluster_tunnels()` ごと async ワーカーから逃がしたことで同時に解決した（追加の変更は
     不要だった）。
  4. **実験で確認した tokio の挙動**（`/tmp` の使い捨てクレートで検証。ローカル実行のみ、外部
     ネットワークには出ていない）:
     - マルチスレッド・ランタイムの async タスクの中で、素朴に `Builder::new_current_thread().build()`
       → `.block_on()` すると、本番と同じ文言で panic する（再現した）。
     - `tokio::task::block_in_place(|| { let rt = Builder::new_current_thread().build()...;
       rt.block_on(...) })` は panic しない。
     - しかし `block_in_place` のクロージャの中でも `Handle::try_current()` は `true` のまま
       （＝「今 tokio ランタイムの中にいるか」を防御的ガードの判定に使うなら、`block_in_place` だけ
       では守れない）。
     - `block_in_place` のクロージャの中で `std::thread::scope` により新しい OS スレッドを spawn する
       と、その OS スレッドの中では `Handle::try_current()` が `false` になる（tokio の文脈は
       スレッドローカルで、新しい OS スレッドには引き継がれない）。この性質を使って、2. の防御的
       ガードと 1. の実際の退避を矛盾なく両立させた。
  5. **`try_auto_connect_cluster`（`auth = "publickey"` の自動接続。ADR-0032 D3、`dispatch_ready` の
     中から呼ばれる）は今回のスコープ外**: これも `cluster_connector` を同じインライン（tick 内、
     async ワーカー上）の形で呼んでおり、理論上は同じ形の panic を起こしうる潜在バグだが、本番に
     `auth = "publickey"` のクラスタが実在しないため一度も踏まれていない。2. の防御的ガードにより
     **panic はしなくなった**（呼ばれれば `Err` → 通常の cooldown 経路に落ちる）が、`dispatch_ready`
     の同じループ反復の**後段**でワーカー起動のため `tokio::spawn` を呼ぶ必要があり、`tick()` 全体を
     1. と同じ形で退避させるのはこの Phase の範囲を越える（`[[clusters.forwards]]` の起動パニックと
     いう報告された不具合ではない）。将来 `auth = "publickey"` のクラスタを本番で使うときは、同じ
     `run_cluster_hooks_off_async` の形で `try_auto_connect_cluster` 側も退避させる必要がある
     （`docs/PROGRESS.md` の「未解決事項」に記載）。
  6. **テストが検出できなかった理由**: Phase 66 の `tunnel_*` 4 件（`crates/task-dispatch/src/
     dispatcher.rs`）は `Dispatcher::refresh_cluster_tunnels()` を**直接**（`tick()` を経由せず）、
     かつ `#[tokio::test]`（既定 flavor = `current_thread`）から呼んでいた。celeris 側の実物の
     `cluster_connector`（ネストしたランタイムを作る形）も使わず、単純な同期の closure
     （`Arc::new(|_, _| Ok(()))` 等）に差し替えていた。この 2 点（呼び出し経路が `tick()` を通らない・
     ネストしたランタイムを作る実物のフックを使わない）のどちらが欠けても、本番の panic は
     再現しない。Phase 66b で `crates/celeris/src/lib.rs` に
     `a_totp_cluster_with_a_forward_does_not_panic_the_first_tick_phase_66b`
     （`#[tokio::test(flavor = "multi_thread")]`）を足し、(a) `build_dispatcher` で本番と同じ配線をし、
     (b) `dispatcher.tick()` を直接呼び、(c) `cluster_connector` だけを実物と**同じ形**
     （ネストした `current_thread` ランタイム + `block_on`。実 ssh はしない）の偽物に差し替えることで、
     この 2 点を両方満たす回帰テストにした（この修正を外すと実際にこのテストが落ちることを確認した）。

### Phase 66c 追記（`--mode verify` が裏方の仕事をしていた副作用の修正。2026-09-21）

Phase 66b の修正は実機で有効だった: release `2b3aaf1d633a` で `[[clusters.forwards]]` を設定した
状態でも `verify.sh` check 1（起動して数秒生きていること）が通り、ログに
`tunnel: state transition … kind: login_needed` → `down` が出て panic は起きなかった。

**新たに観測した副作用**: `verify.sh` check 2（staging が本番のコピーと件数が一致すること）が
`reports(snapshot=135 staging=136)` で落ちた。`--mode verify` の staging インスタンスは
「migrations、API と `smoke` の煙試験だけ。他の dispatch も裏方の仕事も無い」
（`crates/celeris/src/lib.rs` の `run()` のログ文言のとおり）はずなのに、`Dispatcher::tick()` は
role に関わらず（`Active`/`Draining`/`Verify` のどれでも）呼ばれ、その中の
`refresh_cluster_liveness()` / `refresh_cluster_tunnels()` は verify かどうかを一切見ていなかった。
このため staging（本番データのコピー）でも `refresh_cluster_tunnels` が master 未接続 →
`cluster_connector` 失敗 → `mark_login_needed` → `record_cluster_login_needed_report`
（`crates/task-dispatch/src/reports.rs`）と進み、staging 側だけに `reports` 行が 1 件余計に
増えた（本番の `reports` テーブルにはこの verify 実行由来の行は無いので、コピー元の 135 件に対して
staging は 136 件になった）。

- **原因**: `refresh_cluster_liveness` / `refresh_cluster_tunnels`（`crates/task-dispatch/src/
  dispatcher.rs`）はどちらも `Dispatcher` が「`--mode verify` かどうか」を判定する手段を持っていな
  かった。実は `Dispatcher` は既にその目印を持っている: `eligible: Option<TaskFilter>`
  （`set_eligible_tasks` で設定。doc コメントに「`--mode verify` の煙試験でだけ使う」と明記されて
  おり、celeris は `verify == true` のときだけこれを呼ぶ。`crates/celeris/src/lib.rs` の `run()`）。
  この 2 つのメソッドはこの既存の目印を見ずに、`Active`/`Draining`/`Verify` のどの role でも無条件に
  実クラスタへ ssh を打ち、状態を書き換えていた。
- **修正**: `crates/task-dispatch/src/dispatcher.rs` の `refresh_cluster_liveness` /
  `refresh_cluster_tunnels` の先頭に `if self.eligible.is_some() { return; }` を追加した。
  `self.eligible.is_some()` は「`--mode verify` の煙試験だけに絞られている」ことの既存の目印なので、
  celeris 側（`build_dispatcher`/`run()`）への変更は不要（`set_eligible_tasks` を呼ぶタイミングは
  Phase 66 時点のまま）。
  - `refresh_cluster_tunnels` を丸ごと止めることで、forward の(再)確立・probe・`cluster_connector`
    の呼び出し・`cluster_login_needed` の報告書き込みのすべてが verify では起きなくなる
    （`reports` の件数差分が解消する）。
  - `refresh_cluster_liveness` は `reports`/`notifications` へは書かない（`cluster_connected` という
    メモリ上の HashMap を更新するだけ）ため件数差分には無関係だったが、依頼どおり「副作用（実クラス
    タへの `ssh -O check`）があるなら verify では止めるべきか」を確認し、`--mode verify` の
    「他の dispatch も裏方の仕事も無い」という約束をより文字どおりに守るため、こちらも同じ目印で
    止めた（verify の煙試験は fake アダプタのローカルタスクだけなので、クラスタの生死を知る必要が
    そもそも無い）。
- **通知（Discord）は元々二重に安全だった**: `record_cluster_login_needed_report` が書くのは
  `reports` 表だけで、`notifications` 表への行は別経路（`crates/celeris/src/lib.rs` の
  `notify::schedule`）が `reports` を読んで作る。その経路は `tick_loop` の中で
  `if role == InstanceRole::Active { … }` に包まれており、`--mode verify`（`role = Verify`）では
  そもそも実行されない。したがって今回の副作用は `reports` テーブルの件数差分だけで、Discord へ
  実際に送られることは無かった（が、`reports` 行自体が本番のコピーと staging とで食い違うのは
  `verify.sh` check 2 の趣旨（差分ゼロの確認）に反するので、上記の修正で塞いだ）。
- **テスト**: `crates/task-dispatch/src/dispatcher.rs` に
  `verify_mode_does_not_refresh_cluster_tunnels_or_write_a_report`（既存の `tunnel_*` と同じ流儀。
  `auth = "totp"` + `[[clusters.forwards]]` のクラスタ、master 未接続、`cluster_connector`/
  `tunnel_forward_ensurer`/`tunnel_probe` を全部フェイクの closure に差し替えて呼び出し回数を数える）
  を追加した。`set_eligible_tasks` を呼んだ（＝ verify 相当）状態で `refresh_cluster_liveness()` /
  `refresh_cluster_tunnels()` を呼び、(a) 3 つのフックが 1 回も呼ばれないこと、(b) `tunnel_events`/
  `clusters_needing_login()` が空のままであること、(c) `store.report_list(&ReportFilter::default())`
  の件数が呼び出し前後で変わらないことを確認する。**この修正を一時的に取り消して実行し、実際に
  `assertion left == right failed: verify mode must not touch the cluster connector`（`left: 1`）で
  落ちることを確認してから修正を復元した**（回帰テストとして機能することの検証）。
- **ゲート**: `cargo test --workspace --no-fail-fast` / `cargo clippy --workspace --all-targets --
  -D warnings` の結果は `docs/PROGRESS.md` の「Phase 66c」節に記載。

## Phase 81 追記（2026-09-21。Phase 66b の未解決事項 5.: `try_auto_connect_cluster` の退避）

Phase 66b の修正 5. が「`try_auto_connect_cluster`（`auth = "publickey"` の自動接続。
`dispatch_ready` の中から呼ばれる）は今回のスコープ外」として残していた潜在バグ（`cluster_connector`
を `refresh_cluster_tunnels` と同じ「tick 内・async ワーカー上でインラインに呼ぶ」形のままにしていた
ため、`auth = "publickey"` のクラスタが実在すれば `run_cluster_hooks_off_async` を経由せず、
`cluster_connector` 側の防御的ガード頼みになっていた）を塞いだ。

- **決めたこと**: `try_auto_connect_cluster` を、`refresh_cluster_liveness`/`refresh_cluster_tunnels`
  と同じ `run_cluster_hooks_off_async`（`crates/task-dispatch/src/dispatcher.rs`。本物の OS
  スレッドへ逃がし、マルチスレッド・ランタイムの上でだけ `block_in_place` で包む）に通した。
  `cluster_connector(&spec.id, &spec.host)` の呼び出しをそのままクロージャに包んで渡す形
  （`Result<(), String>` を返す必要があるため、`run_cluster_hooks_off_async` 自体を
  `FnOnce() + Send`〈戻り値 `()`〉から `FnOnce() -> T + Send, T: Send` へ一般化した。既存の 2
  呼び出し〈`refresh_cluster_liveness`/`refresh_cluster_tunnels`、どちらも `()` を返す〉は
  そのまま通る）。
- **`cluster_connector`（`crates/celeris/src/lib.rs`）の防御的ガードは変更していない**: Phase
  66b が足した「`Handle::try_current().is_ok()` なら panic の代わりに `Err` を返す」ガードは
  そのまま残す（将来また同じ形の呼び出しを誰かが async の中から直接書いてしまった場合の保険。
  ADR-0009 の多層防御の考え方どおり、退避〈今回の本体の修正〉とガード〈保険〉を両方持つ）。
- **テスト**: `crates/task-dispatch/src/dispatcher.rs` の既存 2 本
  （`publickey_cluster_auto_connects_and_dispatch_continues_on_success`、
  `publickey_cluster_auto_connect_failure_gets_a_distinguishable_reason`。どちらも
  `#[tokio::test]`〈既定 `current_thread`〉から `dispatcher.tick()` を直接呼ぶ）は無変更で
  green のまま（`run_cluster_hooks_off_async` は `current_thread` ランタイムの上では
  `block_in_place` を使わず素の OS スレッドだけを使うため、挙動が変わらない）。
  `crates/celeris/src/lib.rs` に
  `a_publickey_cluster_with_a_ready_task_does_not_panic_the_first_tick_phase_81`
  （`#[tokio::test(flavor = "multi_thread")]`。`a_totp_cluster_with_a_forward_does_not_panic_the_
  first_tick_phase_66b` と同じ配線 — `build_dispatcher`、`dispatcher.tick()` を直接呼ぶ、
  `cluster_connector` を実物と同じ形〈ネストした `current_thread` ランタイム + `block_on`〉の
  偽物に差し替える）を新設し、`auth = "publickey"` のクラスタに ready なタスクを 1 件置いて
  `dispatch_ready` から `try_auto_connect_cluster` を実際に通した。偽の `cluster_connector` は
  `Handle::try_current().is_err()` を assert してから `Err`（自動接続失敗）を返す
  （**成功を返さなかったのは意図的**: 成功させると後続の `SshWorkspace::prepare` が実際の
  ssh/rsync を試みてテストが外部ネットワークに出てしまうため。CLAUDE.md の「テストで外部
  ネットワークに出ない」を優先し、`try_auto_connect_cluster` が off-async で呼ばれることと
  tick がパニックしないことの確認に絞った）。
- **やっていないこと**: 成功パス（`try_auto_connect_cluster` が `Ok(())` を返し、その後
  `dispatch_ready` が実際に worker を spawn するところまで）を celeris の実配線（`build_dispatcher`
  + `FakeAdapter`）で確認する統合テストは追加していない（`SshWorkspace::prepare` の実 ssh/rsync を
  避けられないため）。この経路は `task-dispatch` 側の
  `publickey_cluster_auto_connects_and_dispatch_continues_on_success`（`InstantAdapter` で
  workspace 準備自体をバイパスする既存のユニットテスト）でカバー済みという判断。

## Phase 84b 追記（celeris のトンネルテストが実機の ssh master に依存していた。2026-09-21）

**観測（2026-09-21 21:07 UTC、release gate、main `331aab724fa4`）**: `crates/celeris/src/lib.rs` の
`a_totp_cluster_with_a_forward_does_not_panic_the_first_tick_phase_66b` が
`the totp cluster with a forward must still reach the cluster connector (key auth before TOTP,
ADR-0053 D3)`（`connector_calls == 0`）で落ちた。このテストは 1 日中 green だったが、~21:00 UTC に
人がこのマシンから実際に `pegasus` への ssh ControlMaster を張った（`~/.ssh/mux-rmaeda@pegasus03…` が
現れた）直後から落ちるようになった。

- **原因**: `a_totp_cluster_with_a_forward_does_not_panic_the_first_tick_phase_66b`（Phase 66b/81 追記
  を参照）は `build_dispatcher` + `dispatcher.tick()` という**本番と同じ配線**を検証の要（「実物の
  `cluster_connector` を、実物と同じ経路〈`tick()` の中〉から呼ぶ」）にしていたため、`cluster_connector`
  だけを偽物に差し替え、それ以外（`refresh_cluster_liveness` が呼ぶ `control_master_alive_blocking` =
  本物の `ssh -O check`）は本物のままにしていた。テストの `[[clusters]] host = "pegasus"` は「実在
  しないはず」という前提だったが、`ssh -O check pegasus` はホスト名の DNS 解決すら要らず、**手元の
  `~/.ssh/mux-rmaeda@pegasus*` ソケットの有無だけ**を見るので、人が同じマシンで実際に `pegasus` へ
  ログインしていると `alive = true` になる。`ensure_cluster_master_for_tunnel`
  （`crates/task-dispatch/src/dispatcher.rs`）は `cluster_connected` が `true` ならそこで即座に
  `return true` するため（ADR-0053 D3 の設計どおり: master が生きていれば鍵認証をやり直す必要が
  無い）、`cluster_connector` に一度も届かなくなった。**テストが実機の ssh 状態（このマシンで
  他の作業が張った ControlMaster）に依存していた**ことがバグの本体であり、`cluster_connector` 側にも
  `refresh_cluster_liveness`/`refresh_cluster_tunnels` 側にも実装のバグは無い。
- **決めたこと**: `refresh_cluster_liveness` が使う「master 生存」判定を、`cluster_connector` /
  `tunnel_forward_ensurer` / `tunnel_probe` と同じ流儀でフック化した
  （`task_dispatch::dispatcher::ClusterLivenessProbe = Arc<dyn Fn(&[String], &str) -> bool + Send +
  Sync>`、`Dispatcher::set_cluster_liveness_probe`）。既定（`Dispatcher::new`）は従来どおり本物の
  `control_master_alive_blocking`（`ssh -O check`）なので、本番の挙動（celeris は
  `set_cluster_liveness_probe` を呼ばない）は変わらない。**テストは実機の ssh 状態に依存してはならない
  ので、`ssh -O check` に触れうるテストは必ずこのフックを偽物に差し替える**（CLAUDE.md「テストで外部
  ネットワークに出ない」と同じ精神。`ssh -O check` は外部ネットワークには出ないが、実行環境の状態
  〈人が張った ControlMaster〉に依存する点で同種の脆さがある）。
  - `a_totp_cluster_with_a_forward_does_not_panic_the_first_tick_phase_66b`:
    `set_cluster_liveness_probe(|_, _| false)`（master 死亡、フォワード付き）に差し替え、
    クラスタ id/host も `~/.ssh/config` に実在しうる名前（`pegasus`/`sirius`/`fern03`）を避けて
    `test-cluster` に変えた。
  - `a_publickey_cluster_with_a_ready_task_does_not_panic_the_first_tick_phase_81`: 元々 host は
    `celeris-no-such-host-for-tests-auto-phase81`（安全な架空名）で実害は無かったが、一貫性のため
    同じく `set_cluster_liveness_probe(|_, _| false)` を挿した。
  - 新設: `a_totp_cluster_with_a_live_master_skips_the_connector_but_ensures_the_forward_phase_84b`
    （`crates/celeris/src/lib.rs`）。`set_cluster_liveness_probe(|_, _| true)`（master 生存）にすると、
    ADR-0053 D3 の設計どおり `cluster_connector` は呼ばれず（呼ばれたら偽物が `Err` を返して assert
    される想定だが、そもそも呼ばれないことを回数 0 で確認）、`tunnel_forward_ensurer` だけが呼ばれる
    （forward の(再)確立）ことを確認する回帰テスト。
- **`crates/task-dispatch/src/dispatcher.rs` の既存テストの棚卸し**: ワークスペース全体を
  `pegasus|sirius|fern03` で grep すると、`dispatcher.rs` の `tunnel_*` 系テスト（`tunnel_down_then_
  key_auth_ok_brings_the_forward_up` 等）とスナップショットの `connect_pending` テスト
  （`cluster_live_carries_auth_and_connect_pending` 系）に `"pegasus"` / `"fern03"` が残っている。
  これらは (a) `refresh_cluster_tunnels()` を**直接**呼ぶだけで `refresh_cluster_liveness()`（＝
  `ssh -O check` を打つ経路）を一切通らない、または (b) `host` 自体は安全な架空名
  （`celeris-no-such-host-for-tests-live` 等）で `id` だけが `"fern03"` という**ラベル**である、の
  いずれかであることをコードを読んで確認済みなので、実機の ssh 状態には依存しない。今回はこの Phase の
  スコープ（celeris の 2 テスト）を超えるリネームはしていない（CLAUDE.md「今回のPhaseだけをやる」）。
  将来 `dispatcher.rs` のテストが `tick()`/`refresh_cluster_liveness()` を経由するよう書き換わる場合は、
  同じフックで差し替えるか、実在しうるクラスタ名を避けること。
- **ゲート**: `cargo test -p celeris --lib phase_` 3 件 green、`cargo test --workspace --no-fail-fast`
  全 green（FAILED 0）、`cargo clippy --workspace --all-targets -- -D warnings` exit 0。詳細は
  `docs/PROGRESS.md` の「Phase 84b」節。
