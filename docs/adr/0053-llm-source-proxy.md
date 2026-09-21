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
