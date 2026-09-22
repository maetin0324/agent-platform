# ADR-0061: coding worker のハーネス routing 基盤（Phase 104、Phase 1: adapter層 + routing 骨格 + メトリクス記録）

- 日付: 2026-09-22
- 状態: **Accepted**（自己改善タスク。人間の依頼「coding worker で使うハーネスをタスク特性に応じて
  自動選択・routing できる機能を設計・実装する」。全体像の Phase 1: adapter 層に 1 つ新ハーネスを足し、
  routing policy の骨格を作り、run ごとのコスト/性能メトリクスを記録する。A/B ベンチマーク経路・
  fallback/retry policy・複数タスクでの比較評価レポートは元の依頼の範囲だが、**このタスク自体の範囲外**
  （後続の Phase 2 task）と明示されている）
- 関連: ADR-0003（ワーカープロトコル）、ADR-0006（claude-code アダプタ・結果ファイル規約）、
  ADR-0008（codex アダプタ）、ADR-0026（汎用 ACP アダプタ・opencode）、DESIGN §5.4

## 1. 調査結果: 現在の coding worker/adapter/runner 構造とハーネス差し替えポイント

`crates/task-worker` に `trait WorkerAdapter`（`adapter.rs`）があり、1 run を実行して
`RunOutcome{Terminal, exit_code}` に正規化するだけの薄い境界になっている（判断はしない。判断は
`task-dispatch`）。既存の実装は:

- `claude_code.rs` / `codex.rs`: 各 CLI 独自のストリーム（`stream-json` / `--json` の JSON Lines）を
  読み、`artifacts/result.json`（「結果ファイル規約」、ADR-0006 D3）で終端を合成する専用ループ。
- `acp.rs`: Agent Client Protocol（JSON-RPC）を話す任意の CLI（最初の実装は `opencode acp`）を同じ
  規約で包む**汎用**アダプタ（ADR-0026）。「明確で局所的な少数ファイル修正」以外の「通常の実装・調査・
  テスト反復」枠は、ADR-0026 の時点で **既に** OpenCode/Pi 系ハーネスを `adapter = "acp"` として
  差し込める状態になっている（`[adapters.acp] command = "pi"` のように差し替えるだけで良い）。
- `fake.rs`: taskd 独自プロトコル（JSON Lines）を直接話すテスト用アダプタ。

ハーネスの差し替えポイントは 3 か所:

1. **`task-worker`**: 新しい `impl WorkerAdapter`（新規ファイル）。
2. **`crates/celeris/src/config.rs`**: `[adapters.<id>]` の設定型と `Config::validate` の既知
   アダプタ一覧。
3. **`crates/celeris/src/lib.rs::build_adapters`**: `p.adapter.as_str()` の match に 1 行足す
   （`[[providers]] adapter = "<id>"` の行ごとにインスタンスを作る、ADR-0012 D1）。

**タスクからアダプタを選ぶ経路** は `Task.worker_hint.adapter: Option<String>`
（`task-dispatch::Dispatcher::matching_provider_for_adapter`）。`Some(id)` ならそのアダプタに固定、
`None` なら `ProviderPolicy::select`（tier とプールの残量で選ぶ。claude-code/codex のアカウント
プールをまたいで残量スコア順、ADR-0024）に委ねる。**routing policy が触るべき唯一のフィールドは
この `worker_hint.adapter` であり、dispatcher やアダプタ自体を書き換える必要はない**（後述 D3）。

## 2. 決定

### D1. 3 番目のハーネスとして `aider` アダプタを最小構成で追加する

元の依頼の優先候補（OpenCode / Pi / mini-SWE-agent / Aider）のうち、**OpenCode/Pi 相当は ADR-0026 の
`acp` アダプタで既にカバーされている**（`[adapters.acp] command` を差し替えるだけで良い。新規実装は
不要）ため、Phase 1 で実際にコードを書く価値が最も高いのは「明確で局所的な少数ファイル修正 → aider 系」
の枠を埋める `aider` である。`mini-swe-agent` は次点候補として残す（D5「見送った候補」）。

`aider` CLI は celeris 独自のワーカープロトコルもストリーム JSON も話さない、プレーンテキストの
対話ツールなので、`claude-code`/`codex` と同じ「結果ファイル規約」（`artifacts/result.json`）で
`RunOutcome` を合成する。プロンプト組み立ては `claude_code::build_prompt` を再利用する。

- `crates/task-worker/src/aider.rs`（新規）: `AiderAdapter` / `AiderConfig`。`codex.rs`/`claude_code.rs`
  より単純（JSON Lines の逐次解釈をせず、各行をそのまま `sink.progress` に流し、プロセス終了後に
  `artifacts/result.json` を読むだけ。生存監視は `subprocess.rs` を再利用）。
  - 起動引数: `--yes-always --no-check-update --no-show-model-warnings --no-auto-commits
    --no-gitignore [--model <model>] [extra_args...] --message <prompt>`。
  - **`--no-auto-commits`**: celeris のタスクライフサイクルが commit を管理する前提を崩さない
    （アダプタが黙って git へコミットしない。既存の claude-code/codex も同じ立場）。
  - トークン使用量は aider 自身が終了直前に stdout へ書く `Tokens: N sent, M received.` /
    `Cost: $X message, $Y session.` を最良努力で拾う（省略形〈`1.2k`〉は無視して `None` のまま。
    無ければ `None`。「取得可能な範囲」という CLAUDE.md の方針どおり）。実機（`aider-chat` 0.86.2、
    ローカルの OpenAI 互換モックエンドポイント）で書式を確認済み（本 ADR §4、`docs/PROGRESS.md`
    Phase 104）。`Cost:` 行があればそれを最優先し、無ければ `model` が分かるときだけ
    `task_core::estimate_cost_usd`（D2 の静的単価表）で埋める。
- `crates/celeris/src/config.rs`: `AiderAdapterConfig`（`command`/`extra_args`/`model`/`env`/
  `env_from_secrets`。`codex` と同じ形、resume 相当の概念は無い）、`Config::validate` の既知アダプタ
  一覧に `aider` を追加。
- `crates/celeris/src/lib.rs::build_adapters`: `AiderAdapter::ID` の分岐を追加（`codex` の分岐と
  ほぼ同じ形。`resume_mode` が無いだけ）。

**GUI（`gui/`）は変更していない**（`gui/app/routes/providers.tsx::ADAPTER_OPTIONS` に `"aider"` は
まだ無い）。`gui/CLAUDE.md` が GUI 側を独立した Phase 系列（別ゲート、別 `docs/PROGRESS.md`）として
扱うよう定めており、この Phase の受け入れ条件は Rust 側（`cargo test --workspace` /
`cargo clippy --workspace -- -D warnings`）に限られる。運用者は `config.toml` に手で
`[[providers]] id = "aider-1" adapter = "aider"` を書けば使える（GUI の「プロバイダ追加」フォームに
選択肢を足すのは別 Phase の提案として残す。§5「提案」）。

### D2. メトリクス記録: `Usage` にコスト/キャッシュトークンを足し、`WorkerFinished` に `RunMetrics`（wall time・retry）を足す

- `task_core::model::Usage` に `cache_read_tokens` / `cache_creation_tokens` / `cost_usd: Option<f64>`
  を追加（既存の `input_tokens`/`output_tokens` の意味は変えない。追加のみ）。
- `task_core::pricing::estimate_cost_usd(model, &Usage) -> Option<f64>`: モデル名の**前方一致**で引く
  静的単価表（2026-09 時点の公開価格からの概算スナップショット）。**純粋関数のみ**（I/O・ネットワーク
  呼び出しはしない。DESIGN 原則 1、CLAUDE.md「ディスパッチャやストアに LLM 呼び出しを入れない」と同じ
  精神で「外部の価格 API を呼ばない」）。単価表に無いモデルは `None`（0 円と偽らない）。
- `claude_code.rs`/`codex.rs`: CLI の `usage` から `cache_read_input_tokens`/`cache_creation_input_tokens`
  相当のフィールドを best-effort で拾い、`model` が分かるときだけ `estimate_cost_usd` で `cost_usd` を
  埋める（codex 側のキャッシュトークンのフィールド名は実機で確認していない。best-effort、ADR-0008 D3
  と同じ「取れる範囲だけ」の方針）。
- `Event::WorkerFinished` に `metrics: Option<RunMetrics>{ wall_ms, retries }` を追加。
  `task-dispatch::Dispatcher` が `RunEntry.since`/`ReviewEntry.since`（dispatch した時刻）から
  `wall_ms` を、`Task.attempts`（遷移前の値）から `retries` を計算して埋める。**harness/model は
  同じ `run_id` の `Event::WorkerStarted` に既にあるので `RunMetrics` には持たない**（二重管理をしない、
  DESIGN 原則）。
- success/failure は `WorkerFinished.outcome` の文字列を既存の `task-api::stats::classify_outcome`
  が分類する既存の仕組みをそのまま使う（新しい分類器を作らない）。

これで「runごとに success/failure、token 使用量、推定 cost、wall time、harness、model、retry 回数」は
すべて `Event::WorkerStarted{adapter, model}` + `Event::WorkerFinished{outcome, usage, metrics}` の
2 イベントから決定的に読める（`task-api::stats`/`task-ops::view` が既にこの 2 イベントを読んでいる
経路に、追加したフィールドがそのまま乗る。新しい集計テーブルは作っていない）。

### D3. routing policy の骨格: `task_core::routing`

`crates/task-core/src/routing.rs`（新規、**純粋関数のみ**）:

- `RoutingSignals::from_task(&Task) -> Self`: タスクの題名・目的・受け入れ条件から**決定的に**
  抽出した特性（説明の文字数、受け入れ条件数、tier、ファイルパスらしき手がかりの出現回数、
  「再現」「traceback」等の isolated-issue 語の出現回数）。LLM は呼ばない。
- `trait RoutingPolicy { fn decide(&self, &RoutingSignals) -> RoutingDecision }`:
  `RoutingDecision{ primary, candidates: Vec<String>, reason: String }`。
- `StaticRoutingPolicy`: 元の依頼の分類表をそのまま固定ルール化:
  - 複雑・長時間・高自律（説明が長い/受け入れ条件が多い/tier=frontier）→ `claude-code`（安全側の
    既定。ここは「今までどおり」に倒す。routing 導入で退行させない）
  - 短く局所的・ファイルパスの手がかりあり → `aider`（フォールバック列は
    `[aider, mini-swe-agent, claude-code]`。`mini-swe-agent` は未導入だが「将来入れる想定の名前」
    として列に残す）
  - isolated-issue 語あり → `mini-swe-agent`（フォールバック列 `[mini-swe-agent, acp, claude-code]`）
  - それ以外（通常の実装・調査・テスト反復）→ `acp`（OpenCode/Pi 系の汎用ハーネス。D1 のとおり
    ADR-0026 で既に実装済み）
- `MetricsAwareRoutingPolicy<F>`: `StaticRoutingPolicy` を土台に、`F: Fn(&str) -> Option<f64>`
  （ハーネスごとの成功率）で候補列を並べ替えるだけの薄いラッパー。**「将来メトリクスから更新できる
  構造」の実演**（Phase 2 で `task-api::stats` の集計から成功率関数を作って差し込む想定）。

単体テストで固定ルールの分岐（局所修正→aider、isolated issue→mini-swe-agent、複雑/frontier→
claude-code、それ以外→acp）と `MetricsAwareRoutingPolicy` の並べ替えを確認済み（本 ADR §4）。

### D4. `routing.rs` は意図的に**まだライブの task 作成経路（`task-ops::add`）へ配線しない**

元の依頼は「fallback/retry policy の設計は Phase 2 task の範囲外」と明示している。routing の判断
（例: `primary = "aider"`）を `WorkerHint.adapter` へ実際に書き込むと、そのハーネスに対応する
`[[providers]]` 行が config.toml に無い環境（大半の既存デプロイがそう）では、そのタスクは
「存在しないアダプタに固定されたまま、対応するプロバイダが現れるまで永遠に dispatch されない」
という退行を production に持ち込む。これは fallback policy 抜きでは安全に配線できない
（CLAUDE.md「今回のPhaseだけをやる。次のPhaseの準備を先回りしない」）。

よって Phase 1 では **`routing.rs` を、Phase 2 が「fallback/retry policy」と一緒に配線するための、
十分にテストされた独立ライブラリとして仕上げるところまでとする**。配線先の候補（次 Phase への
引き継ぎ）: `task-ops::add::create_task_with_roles` が `RoutingDecision.candidates` のうち
「実際にこのビルドへ実装済み」（`aider` は D1 で実装済み、`acp`/`claude-code`/`codex` は既存）かつ
「このデプロイの config.toml に対応する provider 行がある」の両方を満たす最初の候補だけを
`WorkerHint.adapter` に採用し、無ければ `None`（既存の残量ベース選択）に黙って倒す。後者の判定
（config.toml を見る）は今の `task-ops` の責務の外にある（`task-dispatch`/`celeris` 側の情報）ため、
Phase 2 はこの受け渡しの形も設計する必要がある。

### D5. 見送った候補

- **mini-swe-agent**: pip パッケージとして存在するが、この ADR の時点でこのホストに未導入
  （`pip show mini-swe-agent` 失敗）。`aider` と同じ「結果ファイル規約 + プレーンテキスト subprocess」
  パターンで実装できる見込みが高く、routing.rs の `HARNESS_MINI_SWE_AGENT` 定数・フォールバック列には
  既に名前を残してある。次点候補として Phase 2 以降に持ち越す。
- **OpenHands/Goose**: 元の依頼で「必要に応じて」と書かれた二次候補。ACP を話せるなら
  `[adapters.acp] command` の差し替えだけで載る可能性がある（ADR-0026 D1 の想定どおり）が未確認。
  このタスクの範囲外。
- **GUI のプロバイダ追加フォームへの `"aider"` 追加**: `gui/CLAUDE.md` により別 Phase 系列
  （別ゲート）。config.toml の直接編集では既に使える。

## 3. 決定しなかったこと（Phase 2 に残す）

- A/B ベンチマーク経路・cost per successful task の比較。
- fallback/retry policy（本 ADR D4）と、それに伴う `task-ops::add` への routing の実配線。
- `mini-swe-agent` アダプタの実装。
- GUI のプロバイダ追加フォームへの `aider` の追加。

## 4. 実機確認

`aider-chat` 0.86.2（`uv venv` で作った隔離環境。システム Python は変更していない）を、ローカルの
OpenAI 互換モック HTTP サーバ（`127.0.0.1`、外部ネットワークには出ない）に向けて実行し、
`--edit-format diff` で `artifacts/result.json` を作らせて exit 0 を確認した。
`Tokens: 123 sent, 45 received.` の書式をこのとき確認し、`aider.rs::parse_aider_usage` の実装根拠に
した。同じ確認を `crates/task-worker/src/aider.rs::tests::real_aider_binary_end_to_end`
（`#[ignore]`。`cargo test --workspace` の既定では走らない。`AIDER_TEST_BIN` で実行ファイルを指定）
として実行可能な形でリポジトリに残した（ローカルの HTTP モックだけを使うので CLAUDE.md
「テストで外部ネットワークに出ない」に沿う）。手順と実行結果は `docs/PROGRESS.md` Phase 104 参照。
