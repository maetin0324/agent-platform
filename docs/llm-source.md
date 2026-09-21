# LLM source のローカル OpenAI 互換プロキシ（ADR-0053 D1/D2、Phase 65）

celeris は `[llm_proxy]` を有効にすると、`127.0.0.1:18100`（既定）で OpenAI 互換の
`POST /v1/chat/completions` を提供する。ログイン済みの Claude Code / Codex の資格情報
（`[accounts] claude_dir` / `codex_dir`）と、既存の OpenAI 互換エンドポイント（Qwen 等）を
「供給元（LLM source）」として束ね、opencode・PaperQA・Local Deep Research・LangMem のような
「adapter を選ばない」ハーネスから、供給元を意識せずに使えるようにする。

実装は `crates/llm-proxy`（独立クレート）。celeris（daemon）はこれを起動・停止するだけで、
選択やトークン更新の判断ロジックは持たない。

## 1. モデル名

- `celeris/<tier>`（`frontier` / `standard` / `cheap`）: 供給元をプロキシが決定的に選ぶ。
- `claude/<tier>` / `gpt/<tier>` / `qwen/<tier>`: 供給元を明示的に固定する。
- `<source>:<具体モデル名>`（例 `claude:claude-sonnet-5`）: 素通り。tier 写像を経由しない。
  供給元の接頭辞（`claude` / `gpt` / `qwen`）を明示したときだけ許す。

tier → 具体モデルの写像は `[llm_proxy.models]`。**既定値は未確認**（`config/celeris.model-tiers.example.toml`
が同種のモデル ID を「実行モデルID未確認」と明記しているのと同じ理由。既存のリポジトリの規約に
沿った命名の一例を既定にしているだけで、実際に存在するモデル ID かどうかは運用前に確認・上書きすること）。
Qwen は tier に関わらず `qwen3.8-27b`（ADR-0053 D1 で明記）。

## 2. 供給元（source）

| source | 種類 | 資格情報 | 上流 |
|---|---|---|---|
| `claude-oauth` | Claude アカウントプール | `<claude_dir>/<id>/.credentials.json`（Claude Code と同じ形） | `https://api.anthropic.com/v1/messages`（Messages API） |
| `codex-oauth` | Codex（ChatGPT）アカウントプール | `<codex_dir>/<id>/auth.json`（Codex CLI と同じ形） | `https://chatgpt.com/backend-api/codex/responses`（Responses API） |
| `openai-compatible` | 中継（例 Qwen） | 無し（`api_key` は平文の bearer。無ければ付けない） | 設定した `base_url` そのまま |

`claude-oauth` / `codex-oauth` は **ADR-0024/0025 のアカウントプールを再利用する**（`.celeris-usage.json`
帳簿を CLI ワーカーの dispatch と共有。cooldown・観測値は同じ表に書く。別の写しは作らない）。

### codex-oauth の要求形（Phase 65b 追記）

本番で `gpt/*`（codex-oauth）経由の要求が全て 0.4 秒以内に `400 {"error":{"message":"upstream error","type":"upstream_error"}}`
で落ちる事故があった（2026-09-21。`docs/adr/0053-llm-source-proxy.md` の Phase 65b 追記）。原因は
ChatGPT の Codex backend（`https://chatgpt.com/backend-api/codex/responses`）が Codex CLI
（`codex-rs`）と違う形の要求を拒否すること。この層は Codex CLI と同じ形で送る:

- **`store: false`、`stream: true` を常に送る**（クライアントの `stream` の値に関わらず。Codex
  backend は非 stream の応答を受け付けない）。クライアントが `stream: false` を要求したときは、
  この層が上流の SSE を集約して 1 つの `chat.completion` に組み立てる（`response.output_text.delta`
  でテキスト、`response.output_item.done` の `function_call` で tool call、`response.completed`
  （`incomplete`/`failed` も同様）で usage / finish_reason を集める）。
- **`instructions` は常に入れる**（クライアントに system メッセージが無ければ既定の一文。Codex CLI
  は空にしない）。
- **`temperature` / `max_output_tokens` は既定では送らない**（Codex CLI が送らないフィールドで、
  送ると拒否されることがある）。`[llm_proxy.sources.codex_oauth] send_sampling_params = true` で
  明示的に opt-in したときだけクライアントの値を転送する。
- `reasoning_effort`（`[llm_proxy.sources.codex_oauth]`）を設定したときだけ
  `"reasoning": {"effort": ..., "summary": "auto"}` と `"include": ["reasoning.encrypted_content"]`
  を付ける（既定では付けない）。
- `parallel_tool_calls: true` を常に付け、`tools` があって `tool_choice` の指定が無ければ
  `tool_choice: "auto"` を付ける。
- ヘッダに `User-Agent: codex_cli_rs/<version>`（`[llm_proxy.sources.codex_oauth] user_agent`。
  **既定値のバージョン番号は未確認**。`codex --version` 等の実機で確認して上書きすること）を追加した。
  他のヘッダ（`Authorization`、`chatgpt-account-id`、`OpenAI-Beta: responses=experimental`、
  `originator: codex_cli_rs`、`session_id`、`accept: text/event-stream`）は Phase 65 のまま。

### 上流エラーの読み方（Phase 65b 追記）

上流が非 2xx を返したとき、この層は本文から `error.message`（OpenAI 互換の形）と、ChatGPT backend
がよく返す上位の `detail` / `message` フィールドの両方を見て、300 文字までの要約を作る
（`sources::codex::extract_error_summary`。トークン・ヘッダは絶対に含めない）。この要約は:

1. **WARN ログ**に出る（`llm-proxy: codex-oauth upstream returned a non-success status`。
   `status` と `summary` のフィールド）。本番でログを見れば、以前のように
   `candidate failed before any bytes were sent` だけでなく、実際に上流が何と言って拒否したかが
   分かる（例: `{"detail":"Store must be set to false"}` → 要約は `Store must be set to false`）。
2. **プロキシの応答本文**（`error.message`）にそのまま出る。`llm_proxy_requests.error_kind` は
   `upstream` のまま変えていない（本文を書く列は増やしていない。CLAUDE.md「migration は今回の
   Phase だけ」に沿って、要約はログと応答にだけ出す）。

### トークン更新

- **claude-oauth**: `.credentials.json` の `expiresAt`（unix ms）を見て、送信前に残り 60 秒を切っていれば
  先に更新する。401 を受けたら事後に 1 回だけ更新して再試行する。どちらも `refresh_token` の
  grant で `token_url`（既定 `https://console.anthropic.com/v1/oauth/token`。**この URL と
  client_id `9d1c250a-e61b-44d9-88ed-5944d1962f5e` は Claude Code CLI の実装から知られている値だが、
  このセッションでは実機で確認していない**）を叩き、同じファイルへ atomic に（`.tmp` + rename、
  mode 0600）書き戻す。
- **codex-oauth**: `auth.json` に有効期限に相当するフィールドが無い（ADR-0053 のフィクスチャどおり）ので、
  **事後（401 を受けてから）だけ**更新する（claude-oauth と違い、事前更新はしない）。
  `token_url = https://auth.openai.com/oauth/token`、`client_id = app_EMoamEEZ73f0CkXaXp7hrann`
  （ADR-0053 の指示にあった値）。
- 資格情報の値は**どのログ・応答にも出さない**（`ClaudeTokens`/`CodexTokens` は値を隠す `Debug` を持つ。
  自動テストで captured tracing subscriber を使い確認済み）。

## 3. 選択（決定的。ADR-0053 D1 / ADR-0049 の再利用）

`celeris/<tier>`:
1. `prefer_free = true`（既定）なら、設定順で最初に到達可能な `openai-compatible` を使う
   （`GET <base_url>/models` の probe。`probe_cache_secs` 秒キャッシュ、既定 60）。
2. 到達可能な `openai-compatible` が無ければ、Claude と Codex のアカウントプールを跨いで
   残量スコアを比較する（ADR-0024 D3 の `evaluate`/スコア式そのもの。除外: 未ログイン・cooldown・
   5 時間枠/週次枠の枯渇）。同点は設定順（Claude を先に見る）→ 使用中の少ない方 → id 昇順。
3. どちらも無ければ 503 `no_source_available`。

`claude/<tier>` / `gpt/<tier>` はそのプールだけを見る。`qwen/<tier>` は `openai-compatible` だけを見る
（`prefer_free` は関係ない。明示されているため）。

**429/401 を受けたら**、そのアカウントに cooldown を付け（既存の帳簘。401 は `AuthFailed`、429 は
`Throttled`）、**まだバイトを送っていなければ**同じ要求を次の候補でやり直す。stream 開始後
（upstream から 200 を受けて bytes を転送し始めた後）に何か起きたら、やり直さずに切断するだけ
（OpenAI 形式のエラーフレームを送らない。ADR-0053: 「開始後は切断を返す」）。

選ばれた供給元・アカウントは応答ヘッダ `x-celeris-source` / `x-celeris-account` に出る。

## 4. 要求の記録（`llm_proxy_requests`。migration 0022）

`id` / `ts` / `source` / `account` / `requested_model` / `upstream_model` / `prompt_tokens` /
`completion_tokens` / `latency_ms` / `status`（`ok`/`error`/`unavailable`）/ `error_kind`。
**本文（プロンプト・応答）は書かない**。stream の要求は、ストリームが終わった時点でまとめて 1 行を書く
（先頭で「選んだ」ことは記録されるが、行そのものは完了後）。

`GET /llm/sources`（主 API、`/api/v1/llm/sources`。ADR-0053 D4）が、供給元ごとの到達性・アカウントの
残量・cooldown・直近 1 時間の要求/token 数を返す（GUI の表示は Phase 66。ここでは API と型だけ）。

## 5. 設定

`config/celeris.example.toml` の `[llm_proxy]` に worked example がある。要点:

```toml
[llm_proxy]
# enabled は省略可（source が 1 つでもあれば自動で有効）
listen = "127.0.0.1:18100"
prefer_free = true

[llm_proxy.sources.claude_oauth]
# accounts_dir を省略すると [accounts] claude_dir を使う

[llm_proxy.sources.codex_oauth]
# 同上（[accounts] codex_dir）

[[llm_proxy.sources.openai_compatible]]
id = "qwen"
base_url = "http://127.0.0.1:18000/v1"

[llm_proxy.models.claude]
standard = "claude-sonnet-5"
# ... 運用前に確認・上書きすること（§1 参照）
```

## 6. ハーネスをプロキシに向ける（ADR-0053 D2）

既定の設定は 4 つのプロバイダをプロキシに向ける。**PaperQA だけ `celeris/standard`、他は
`celeris/cheap`**。

| プロバイダ | 設定ファイル | 変更点 |
|---|---|---|
| `paperqa-qwen` | `config/celeris.research.example.toml` | `[adapters.paperqa].env`: `OPENAI_BASE_URL` → `http://127.0.0.1:18100/v1`、`OPENAI_API_KEY` → `[api] token_file` と同じ値。`model = "openai/celeris/standard"` |
| `ldr-qwen` | `config/celeris.web-research.example.toml` | `[adapters.local_deep_research.settings]`: `llm.openai_endpoint.url` → プロキシ、`llm.openai_endpoint.api_key` → 同上、`llm.model = "celeris/cheap"`。`[[providers]] model = "celeris/cheap"` |
| `langmem-main` | `config/celeris.example.toml`（`[knowledge.langmem]`）、`docs/knowledge.md` | `base_url` → プロキシ、`model = "celeris/cheap"` |
| `opencode-qwen` | `config/celeris.acp-opencode.example.toml` | **コメントで手順を示すのみ**（下の §7 を参照。実機で opencode の provider/model の区切り方を確認できていないため、既定は直接 Qwen のまま） |

いずれも Qwen が生きていれば従来どおり Qwen、落ちていれば自動で Claude / GPT のアカウントプールに
倒れる（`prefer_free = true` のときの `celeris/<tier>` の振る舞い。ADR-0052 の
knowledge_maint 側フォールバックはこのプロキシの中に吸収される）。

**注意**: 上の 3 つ（paperqa/ldr/langmem）はいずれも `OPENAI_API_KEY` / `api_key` にプロキシの
bearer トークン（`[api] token_file` の中身）が必要。直接 Qwen を叩いていた頃の `"unused"` は
このプロキシには通らない（`GET /healthz` を除く全エンドポイントが Bearer を要求する）。

## 7. opencode をプロキシに向ける（未検証。ADR-0053 D2）

`tools/opencode/` はこのリポジトリにまだ無いので、ここに JSON を示す
（`config/opencode.openai-compat.example.json` の書き方を踏襲）。

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "celeris-proxy": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "celeris LLM source proxy",
      "options": { "baseURL": "http://127.0.0.1:18100/v1", "apiKey": "{env:CELERIS_API_TOKEN}" },
      "models": { "celeris/cheap": { "name": "celeris (auto)" } }
    }
  },
  "model": "celeris-proxy/celeris/cheap",
  "permission": { "edit": "allow", "bash": "allow", "webfetch": "deny", "task": "deny" }
}
```

`CELERIS_API_TOKEN` は `[api] token_file` と同じ値（`[[providers]] env` で渡す）。

**未検証・要確認**: opencode の `"model"` フィールドは `"<providerId>/<modelId>"` の形だが、
`modelId` 自体に `celeris/cheap` のようにスラッシュを含めてよいか（opencode 側が最初の `/` だけで
区切るか）は、このセッションでは opencode の実機・ソースで確認していない。安全側に振るなら、
`models` のキーをスラッシュ無しの別名（例 `"celeris-cheap"`）にし、`[llm_proxy]` 側で
`qwen:<model>` 形の素通り構文と同様の別名解決を別 ADR で足す運用にしてもよい。**この理由により
`config/celeris.acp-opencode.example.toml` の既定はまだ直接 Qwen を指すままにしてあり、上の JSON は
コメントで参照するだけにした**（ADR-0053 D1/D2 の受け入れ条件「too hard なら明示的に無効のまま出す」
に従った判断）。

## 8. 本番の運用手順（PROGRESS.md にも同じ内容を記録）

1. `[llm_proxy]` を有効化（`[llm_proxy.sources.claude_oauth]` / `codex_oauth` を追加するか、
   既存の Qwen 中継を `[[llm_proxy.sources.openai_compatible]]` として登録）。`POST /reload` では
   反映されない設定なので celeris を再起動する。
2. `curl -s -H "Authorization: Bearer $(cat ~/.config/celeris/api.token)" http://127.0.0.1:18100/healthz`
   → `{"status":"ok"}`。
3. `curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:18100/v1/models` → `celeris/*` 等が並ぶ。
4. `curl -s -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{"model":"celeris/cheap","messages":[{"role":"user","content":"hi"}]}' \
     http://127.0.0.1:18100/v1/chat/completions` → 200 と応答本文。`x-celeris-source` ヘッダで
   選ばれた供給元を確認する。
4b. **codex-oauth（Phase 65b で修正）**を明示的に確認する:
   `curl -s -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{"model":"gpt/cheap","messages":[{"role":"user","content":"hi"}]}' \
     http://127.0.0.1:18100/v1/chat/completions` → 200 と応答本文、`x-celeris-source: codex-oauth`。
   400 等が返ったら、celeris のログの `llm-proxy: codex-oauth upstream returned a non-success status`
   の `summary` フィールドに上流が言っている理由が出る（§「上流エラーの読み方」参照）。
5. 4 つのプロバイダ（`paperqa-qwen` / `ldr-qwen` / `opencode-qwen` / `langmem-main`）を**1 つずつ**
   プロキシへ向け、そのつど 1 タスクを流して結果を確認する（同時に全部変えない）。`langmem-main` を
   プロキシへ向けるときは、`[knowledge.langmem].api_key_secret` を必ず設定すること（Phase 65b:
   到達性 probe の `GET /v1/models` がこのトークンで `Authorization: Bearer` を送る。無いと 401 が
   返るが、401/403 は「落ちている」と誤認せず `Unknown`（＝従来どおり `langmem` で走らせる）として
   扱うので、フォールバックし続けることはない）。

## 9. 明示した既知の制約（Phase 65 の範囲）

- マルチモーダル（画像等）は扱わない（テキストのみ）。
- streaming の tool call は Claude/Codex とも 1 つの引数チャンクをそのまま転送する簡略化（OpenAI の
  実際の増分ぶんまで細切れにする厳密な互換ではないが、最終的な組み立て結果は正しい）。
- `openai-compatible` の stream は生バイトの中継で、`model` フィールドだけ書き換える（usage の
  取り出しはしない。ログの `prompt_tokens`/`completion_tokens` は relay の stream では `null`）。
- アカウントの同時実行カウント（`IN_USE_PENALTY` の分母）は、このプロキシ内の同時要求だけで数える
  （CLI ワーカーの `in_use` とは別枠。cooldown・観測値は共有するが、公平性の計算は共有していない）。
- D3（切れにくい Qwen トンネル）・D4（GUI の「LLM source」節）は Phase 66。
