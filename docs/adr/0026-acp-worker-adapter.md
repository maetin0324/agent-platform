# ADR-0026: 汎用 ACP ワーカーアダプタ（最初の実装は opencode。OpenAI 互換 LLM をワーカーに使う）

- 日付: 2026-09-17
- 状態: **Accepted**（人間の依頼「OpenAI API 互換の任意の LLM をワーカーに指定できるようにしたいので、opencode を動かせるようにしたい」。
  人間の設計相談の結論「`opencode run` を直接包むより、汎用 ACP アダプタを `task-worker` に足し、その最初の実装として `opencode acp` を使う」を採用。
  実装順は人間の選択により**最初から汎用 ACP**）
- 関連: ADR-0003（ワーカープロトコル）、ADR-0006（claude-code アダプタ・結果ファイル規約）、ADR-0008（codex アダプタ）、ADR-0016（委譲）、
  DESIGN §5.4 のアダプタ表（`acp` は無いので提案 P-63 を出す）

## 1. 文脈

taskd はタスク木・依存・retry・review・人への質問・委譲・ワークスペース・プロバイダ選択・SQLite の永続状態を**外側**に持ち、`WorkerAdapter` は
「1 run を実行して `Done` / `Question` / `Error` に正規化する」だけの薄い境界になっている。足りないのは「もう 1 つのオーケストレータ」ではなく、
**1 タスクをうまくこなす実行器**。ACP（Agent Client Protocol）は「クライアントがエージェントのサブプロセスを起動して制御する」ための標準で、
公式の Rust SDK がある。ここに橋を 1 本架けておけば、ACP に対応した OSS ハーネスを `task-worker` をほぼ変えずに差し替え・比較できる。

### このホストで確認した事実（2026-09-17）

- `opencode` 1.18.31（`~/.opencode/bin`）。`opencode acp` は stdio 上の JSON-RPC（NDJSON）で動く。
- `initialize` に `protocolVersion: 2` を送っても **`protocolVersion: 1` を返す**（opencode は ACP v1）。応答の `agentCapabilities` は
  `loadSession` / `mcpCapabilities` / `promptCapabilities{embeddedContext,image}` / `sessionCapabilities{close,fork,list,resume}`。
- `session/new {cwd, mcpServers: []}` → `{sessionId, configOptions: [{id: "model", type: "select", currentValue, options: [...]}]}`。
  ACP の語彙に `session/set_config_option` がある（モデルはこれで選ぶ）。
- Rust SDK `agent-client-protocol` 2.1.0 は `ProtocolVersion::V1` と `V2`（`unstable_protocol_v2` フラグ）を持つ。付随して
  `agent-client-protocol-tokio`（stdio 用）がある。
- opencode の設定は環境変数で差し替えられる: `OPENCODE_CONFIG`（ファイル）/ `OPENCODE_CONFIG_CONTENT`（本文）/ `OPENCODE_CONFIG_DIR` /
  `OPENCODE_DISABLE_PROJECT_CONFIG` / `OPENCODE_PURE` / `OPENCODE_DB`。
- OpenAI 互換 LLM の実機確認: `ssh -J pegasus -L 18000:127.0.0.1:18000 bnode150` で、クラスタの vLLM（`qwen3.8-27b`、`Qwen/Qwen3.8-27B-FP8`、
  `max_model_len` 262144）を手元に引き、opencode の `provider` 設定（`npm: "@ai-sdk/openai-compatible"`、`options.baseURL`）で
  `opencode run` が実際に応答した（`ok`、exit 0）。**初回起動はプロバイダの npm パッケージ取得に 2 分以上かかる**（2 回目以降は数秒）。

## 2. 決定

### D1. `task-worker` に汎用 ACP アダプタ（`adapter = "acp"`）を足す。ACP は**運搬と観測と生存管理**だけ

```
ACP            = transport / observability / lifecycle（initialize・session・update・cancel・permission）
result.json    = agent-platform の意味論（done / question / error。ADR-0006 の結果ファイル規約のまま）
delegate.json  = 仕事の分割（ADR-0016 のまま。taskd が子タスクにする）
```

**エージェントの最終回答を成功判定にしない**。終端は今までどおり `artifacts/result.json` から合成し、受け入れ条件は taskd（Reviewer / Command）が検証する。

### D2. 設定

```toml
[adapters.acp]
command = "opencode"              # 既定。ACP エージェントの実行ファイル
args = ["acp"]
permission = "allow"              # session/request_permission への答え（allow | deny）。既定 allow
model_option_id = "model"         # session/set_config_option の id（既定 "model"）
startup_timeout_secs = 300        # initialize の応答を待つ上限（初回はプロバイダの取得で数分かかる）
env = { }                         # 共通の環境変数

[[providers]]
id = "opencode-qwen"
adapter = "acp"
tiers = ["standard", "cheap"]
concurrency = 1
model = "qwen-local/qwen3.8-27b"  # 空でなければ session/set_config_option で設定する
# command / args / env は行ごとに上書きできる（別の ACP エージェントを同居させるため）
env = { OPENCODE_CONFIG = "/home/u/taskd/opencode/qwen.json", OPENCODE_DISABLE_PROJECT_CONFIG = "1" }
```

- `ProviderConfig` に `command: Option<String>` と `args: Option<Vec<String>>` を足す（`adapter = "acp"` のときだけ意味がある。他のアダプタで指定したら設定エラー）。
- アダプタのインスタンスは従来どおりプロバイダ行ごと（ADR-0012 D1）。env の優先順も従来どおり（taskd < `[adapters.acp]` < 行）。
- `goose acp` 等を足すときは `[[providers]]` をもう 1 行書くだけ（コードは変えない）。

### D3. 1 run の手順

1. `command args...` を**ワークスペースを cwd として**起動（stdin/stdout をパイプ、`process_group(0)`、`kill_on_drop`）。stderr は `runs/<run_id>/stderr.log` へ。
2. `initialize`（`ProtocolVersion::V1`、クライアント能力は **fs も terminal も false**。taskd はファイルサーバにならない。エージェントは cwd で自分のツールを使う）。
   返ってきた版が V1 でなければ、その run は `spawn_failed` として終える（版の交渉はアダプタの中に閉じる）。
3. `session/new { cwd: <workspace>, mcpServers: [] }`。
4. `model` が空でなければ `session/set_config_option { sessionId, configId: <model_option_id>, value: <model> }`
   （**実機で確認**: opencode 1.18.31 は `configId` を要求し、`optionId` は `-32602 Invalid params` を返す。`session/new` の
   `configOptions[].id` と対になる。`session/request_permission` の選択肢の `optionId` とは別物）。
   その id が `configOptions` に無ければ warn を出して続ける（モデルはエージェント側の設定で決まっている場合がある）。
5. `session/prompt { sessionId, prompt: [{type: "text", text: <既存のプロンプト生成と同じ本文>}] }`。
   `request.json` / `prompt.txt` は他のアダプタと同じ共有ヘルパで残す（ADR-0023 M1）。
6. `session/update` の通知を `EventSink` に写す:
   | 通知 | 写し先 |
   |---|---|
   | `agent_message_chunk` / `agent_thought_chunk` | `progress`（500 文字で切る） |
   | `tool_call` / `tool_call_update` | `progress`（`tool: <name> <status>`） |
   | `plan` / それ以外 | `heartbeat` のみ |
   すべての通知で `heartbeat()` を呼ぶ（無出力タイムアウトの判定に使う）。
7. `session/prompt` の応答（`stopReason`）で本文は終わり。**終端は `artifacts/result.json`**（`{"summary","evidence"}` / `{"question"}`）から合成する。
   `stopReason` が `refusal` / `max_tokens` 等で結果ファイルが無ければ `Error{retryable: true}`（理由を message に入れる）。
8. `artifacts/delegate.json` は共有ヘルパで転送する（claude-code / codex と同じ）。

### D4. 権限要求・キャンセル・タイムアウト

- `session/request_permission` には**設定どおり即答する**（`allow` なら許可の選択肢、`deny` なら拒否の選択肢。選択肢の種別は要求に入っている）。
  taskd は対話できないので待たない。ワークスペースは使い捨ての作業ディレクトリなので既定は `allow`。
- 壁時計（`budget.max_wall_secs`）と無出力（`idle_timeout_secs`）は他のアダプタと同じ。超えたら `session/cancel` を送り、`kill_grace_secs` 後に
  プロセスグループへ SIGKILL（`send_signal_to_group`）。
- `initialize` の応答を `startup_timeout_secs` まで待つ（初回のプロバイダ取得が遅いため。既定 300 秒）。

### D5. 供給側の失敗の分類

JSON-RPC のエラー本文と stderr の末尾を既存の分類器（`classify_provider_failure`）に通し、`AuthFailed` / `Throttled` / `Exhausted` を返す。
分類できない失敗は `Error{retryable}`。アカウントのプール（ADR-0024 / 0025）は **acp では使わない**（鍵はエージェント側の設定にある）。

### D6. opencode 固有のことはコードに入れず、設定例と文書に置く

`config/taskd.acp-opencode.example.toml` と `docs/` に次を書く（taskd は env を渡すだけ）:

- `OPENCODE_DISABLE_PROJECT_CONFIG=1` を**必ず**渡す。ワークスペースはエージェントが書き換える場所なので、そこに置かれた `opencode.json` に
  taskd の設定を上書きさせない（権限やモデルを勝手に変えられないようにする）。
- `OPENCODE_CONFIG` に taskd 側が用意した設定ファイルを指す。その中で OpenAI 互換プロバイダ（`npm: "@ai-sdk/openai-compatible"`、`options.baseURL`）と
  既定モデル、そして権限を決める。
- **opencode 自身の subagent は止める**（`permission.task = "deny"`）。仕事の分割は taskd の `delegate.json` → 子タスクで行う（SQLite に残り replay できる）。
  `read` / `edit` / `bash` / `grep` / `glob` は許可。
- 初回は数分かかるので、`taskctl worker run` で一度暖気してからデーモンに載せる。

### D7. GUI と管理 API

`acp` を既知のアダプタに足す（`POST /providers` の検証、GUI のプロバイダ追加フォームの選択肢）。`command` / `args` は管理 API では受け取らない
（実行するコマンドを HTTP から書き換えられないようにする。`providers.d/*.toml` を人が編集する）。

## 3. 採らない

- `opencode run --format json` を包むだけのアダプタ（相談の第 1 段階）。ACP を最初から使うという人間の選択に従う。
- OpenHands / goose を先に入れる（goose も `goose acp` なので、この ADR の `[[providers]]` を 1 行足せば載る）。
- ACP v2（opencode が v1 のため。SDK では unstable。V1 固定にしておき、交渉はアダプタの中に閉じる）。
- ACP のファイルシステム能力（`fs/read_text_file` 等）と端末能力を taskd が提供すること（エージェントは cwd で自分のツールを使う）。

## 4. 受け入れ条件（Phase 15）

1. スタブの ACP エージェント（JSON-RPC を話す小さなスクリプト）で、`session/update` が `progress` に写り、`artifacts/result.json` から
   `done` / `question` / `error` が合成される（オフラインのテスト）。
2. 権限要求に設定どおり即答する。壁時計・無出力の超過で `session/cancel` → プロセスグループごと停止する。
3. 版が V1 でないエージェントには `spawn_failed`（交渉はアダプタの中）。
4. `[[providers]] adapter = "acp"` が `command` / `args` / `env` / `model` の行ごとの上書きで動き、`acp` 以外で `command` を書いたら設定エラー。
5. 実機: `opencode acp` + トンネルした `qwen3.8-27b` で、taskd に入れた実タスクが 1 周して `done` になる（成果物と受け入れ条件の判定込み）。
6. `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / GUI の検査一式。
7. 提案 P-63（DESIGN §5.4 のアダプタ表に `acp` を足す）を PROGRESS に書く。
