# ADR-0063: 調査系タスクの生存率を上げる（PaperQA のアブスト妥協と OA 探索、LDR の再挑戦強化、受け入れ条件の部分達成、秘密の持ち方）

- 日付: 2026-09-23
- 状態: **Accepted**（人の指示。本番の案件 BenchFS で、文献調査（PaperQA2）と Web 調査（LDR）が
  2 回とも `failed` になり、原因が「主要論文が 403 で本文を取れない」「一次情報〈GitHub〉が Web 検索で
  見つからない」という調査の質の問題と、`acquire_input.json` に API キーが平文で残る運用上の問題の
  両方だったことから）
- 関連: ADR-0027 D3（`paperqa` アダプタ）、ADR-0035（文献取得・証拠ゲート）、ADR-0029（`local-deep-research`
  アダプタ）、ADR-0031（Web 調査の証拠ゲート）、ADR-0033 D6 / ADR-0038（レビューと prior_review）、
  ADR-0047（知識ベース）、ADR-0052（知識整理のフォールバック。同じ「使えないときに機械的に迂回する」考え方）

## 1. 文脈（2026-09-23、案件 BenchFS）

1. **文献調査**: acquire は動いていた（候補 30、PDF 6、検索語も妥当）。しかし CHFS（HPC Asia 2022）・
   UnifyFS・Mochi・BeeOND のような主要論文が ACM / eScholarship で `HTTP 403` になり本文が取れず、
   落とせたのは周辺の arXiv 論文だけ。PaperQA は本文からしか引用しないため `cited` が `min_cited` に
   届かず、アダプタが hard error にして 2 回で `failed`。
2. **Web 調査**: 70 KB の報告と 14 出典を出したが、CHFS の一次情報（`github.com/otatebe/chfs`）が
   見つからずブログ依存になり、受け入れ条件「各システムの一次情報に基づく」で reviewer が 2 回不合格。
   2 回目は `prior_review`（1 回目の不合格理由）を context には受け取っていたが、`mode`/`iterations`
   は 1 回目と同じ `quick` のままで、埋められなかった。
3. **秘密の扱い**: `acquire_input.json`（run ディレクトリ、権限 664、ワーカー / reviewer が読める）に
   `query_llm.api_key`（プロキシ用トークンの値）が平文で書かれていた。

人の方針（2026-09-23）: 「アブストラクトまで確認できることが多いので、本文が有料ならアブストまでで
妥協する」。

## 2. 決定

### D1. PaperQA: 本文が取れない論文はアブストラクトで妥協する

- `[adapters.paperqa.acquire] abstract_fallback`（既定 `true`）: PDF が取れない候補（403・非 PDF・
  URL 無し）は、Unpaywall / Semantic Scholar でもう一段 OA PDF を探し（`find_oa_pdf`）、それでも
  無ければメタデータの abstract を `<key>_abstract.txt` として corpus に入れる（本文でないことを
  明記した注記付き。`paperqa_acquire.py::abstract_document_text`）。候補には `abstract_only: bool`
  を持たせ、`answer.md` の出典行に `(引用・アブストのみ)` と印を付ける。
- 起点の資料（目的文・`inputs` に含まれる URL）を acquire の seed にする
  （`paperqa.rs::extract_seed_urls` / `classify_seed_url`）。PDF / DOI / arXiv の URL は
  `paperqa_acquire.py::candidate_from_seed` で候補にし、他の検索結果より前に置いて
  `max_candidates` で落ちないようにする。GitHub / GitLab の URL は論文ではないので corpus には
  入れず、`answer.md` に「## 一次情報（実装）」節として載せる（Rust 側で完結。python には渡さない）。
- `[adapters.paperqa.evidence] insufficient_is_error`（既定 `false`）: 取得が 0 件（検索経路の問題）は
  従来どおり常に hard error。それ以外の閾値未達（`min_candidates`/`min_pdfs`/`min_cited`）は、既定では
  hard error にせず `Terminal::Done` にする。`artifacts/research.json` に
  `{"evidence": {cited, cited_fulltext, cited_abstract_only, min_cited, insufficient}}` を書き、
  `answer.md` に「## 証拠の質」節（本文 / アブストのみの内訳と、不足なら代替案〈Web 調査へ切り替える・
  人が著者版 PDF の URL を与える〉）を必ず足す。`true` にすると Phase 108 までの hard error に戻る。
- **採らない**: OA 探索や seed URL のためにネットワーク経路そのものを増やすことはしない
  （Unpaywall / Semantic Scholar は acquire が既に叩いている arXiv / OpenAlex と同じ「鍵不要の公開
  API」の枠に収め、テストは全て `Fetcher` の fixture 経由でネットワークに出ない）。

### D2. LDR: 再挑戦を強くする

- `[adapters.local_deep_research] retry_mode`（既定 `detailed`）/ `retry_iterations`（既定 `5`）:
  `task.attempts >= 1`（前回 reviewer 不合格で再試行になった run）では、通常の `mode`/`iterations`
  ではなくこちらを使う（`local_deep_research.rs::run_ldr`）。
- `context.prior_review` のうち `pass = false` の条件の理由を「## 必ず埋める項目」として問いの先頭に
  置く（`must_cover_items` / `build_must_cover_section`）。これは LDR に渡す問いそのものの一部になる
  （ADR-0029 の「素の目的だけを渡す」設計を壊さないよう、合格した条件・空の理由は載せない）。
- 必読の一次情報（`must_read_urls`）: 目的文中の URL と、知識ベースの索引
  （`context.knowledge.index`）のうち `primary-sources` / `一次情報` タグを持つページの `sources`
  （前置きの出典。人が `celerisctl knowledge record --source` で付けたもの）を集め、
  `ldr_run.py` の後処理（`add_must_read_sources`）で LDR が返した `sources`/report.md/sources.json の
  **末尾に**（LDR 自身の `[n]` 引用番号を壊さないため）強制的に加える。ページの `<title>` は
  `fetch_url_title`（失敗すれば URL そのもの。ここだけは実行時にネットワークに出るが、テストは
  関数注入で回避）。
- **採らない**: LDR の検索クエリそのものを書き換えて一次情報を「検索させる」実装。LDR の検索エンジン
  内部（SearXNG/Tavily 等）を celeris から強制できないため、「見つからなかったら結果に足す」という
  決定的な後処理にした。

### D3. 受け入れ条件の部分達成

- CoS の `create_task` の指示文（`preamble.rs::actions_instructions`）に、調査系
  （`literature`/`web-research`）の子タスクは受け入れ条件を対象ごとに分けるか、
  「一次情報で確認できなかった項目は『未確認』と明記されていれば不合格の理由にしない」の一文を
  レビュアー条件に含めるよう追記した。
- `NewTask.partial_ok: Option<bool>`（`task_core::plan`）を追加。`plan.json` の検証
  （`task_core::warn_missing_partial_ok`）は、genre が `literature`/`web-research` に解決される子で、
  `partial_ok: true` も、受け入れ条件の文面に `未確認`/`対象ごと` のキーワードも無ければ**警告**を返す
  （**拒否はしない**。決定的、LLM は使わない）。`dispatcher.rs::fix_plan_for_harness` から
  `fix_harness_artifacts` と並べて呼び、`tracing::warn!` に残す（既存の「壊さず直す」規則と同じ扱い）。

### D4. 秘密の持ち方

- `paperqa.rs`: `acquire_input.json` の `query_llm.api_key` には実際の値ではなく
  `"<env:OPENAI_API_KEY>"` のようなプレースホルダだけを書く。実値は元から子プロセスの環境変数
  （`.envs(config.env)`）として渡っているので、`paperqa_acquire.py::resolve_env_placeholder` が
  プレースホルダを見たら `os.environ` から読み直す。
- `local_deep_research.rs`: `[adapters.local_deep_research].settings` のうちキーが
  `api_key`/`token`/`password`/`secret` で終わる値（例 `llm.openai_endpoint.api_key`）は、
  `redact_secret_settings` が `LDR_` + キーの大文字化（ADR-0031 で確認済みの LDR 自身の環境変数規則）
  の名前に変換して子プロセスの環境変数として渡し、`ldr_input.json` にはその環境変数名への
  プレースホルダだけを書く。`ldr_run.py::convert_setting_value`/`resolve_env_placeholder` が解決する。
- 既存の run ディレクトリに残っている平文の値は celeris からは消せない。運用者（人）が
  `celeris-api-token`（`[api] token_file` の値）をローテーションする必要がある
  （`docs/PROGRESS.md` の Phase 109 節に記載。親が扱う）。

## 3. 受け入れ条件（Phase 109）

1. `paperqa.rs` / `paperqa_acquire.py`: `insufficient_is_error=false` で `Terminal::Done` になり
   `research.json.evidence` が入ること、`true` で従来どおり hard error になること、seed URL の分類・
   抽出、`acquire_input.json` に `api_key` の実値が出ないこと、abstract fallback と OA 探索
   （Unpaywall/Semantic Scholar）の純粋関数がユニットテストで確認できること（ネットワーク無し）。
2. `local_deep_research.rs` / `local_deep_research_run.py`: `attempts >= 1` で `mode`/`iterations` が
   上がること、`must_cover`/`must_read_urls` が問いと入力 JSON に載ること、`ldr_input.json` に
   秘密の実値が出ないこと。
3. `preamble.rs` / `plan.rs`: CoS への指示文に一文が追記されること、`warn_missing_partial_ok` が
   調査系で条件不足を警告し、それ以外・条件充足では警告しないこと（拒否しないこと）。
4. `cargo test --workspace --no-fail-fast`（FAILED 0）、
   `cargo clippy --workspace --all-targets -- -D warnings`（exit 0）。`docs/protocol/plan-output.schema.json`
   のみ差分（`NewTask.partial_ok` を追加したため）、`docs/api` には差分なし。
5. 実機（親が行う）: BenchFS の文献調査を再実行して abstract 妥協で `done` になり、報告に証拠の質の
   内訳が出ること。Web 調査の再挑戦で CHFS の一次情報が `sources` に入ること。

## Phase 109b 追記（2026-09-23）

Phase 109 を本番で試したら（2026-09-23 11:04〜11:10 UTC、昇格直後の 2 タスクのやり直し）見つかった
6 つの欠陥と、その直し。本文（上記 D1〜D4・受け入れ条件 1〜5）は書き換えない。

### 観測

1. **PaperQA — OpenAlex の 429 一斉障害**: Phase 109 で Unpaywall/Semantic Scholar の照会が増え、
   OpenAlex への要求が短時間に集中し、全クエリが `HTTP Error 429` になった（`engines: {arxiv: 30,
   openalex: 0}`）。
2. **PaperQA — cited が本文の文字列一致に頼りすぎ**: abstract 妥協で `pdfs: 8, abstracts: 22` まで
   取れたのに `cited=0`。`answer.md` には文書の内容を使った記述があるのに、プロキシ経由のモデルが
   PaperQA 標準の引用マーカー（`(key pages x-y)`）を本文に残さなかった。
3. **PaperQA — 成果物名が `answer.md` のみ**: 調査系の受け入れ条件が `artifact_exists report.md`
   （LDR と同じ名前）だと決定的検査そのものが落ち、reviewer が評価されないまま 2 回で `failed`。
4. **LDR — `must_read_urls` の本文が問いに載らない**: `add_must_read_sources` は答えが出た**後**に
   URL とタイトルを `sources` 末尾に足すだけで、本文（README 等）を LDR に一切渡していなかった。
   加えて、知識ベースの `sources: ["human", ...]` のような URL でない値まで拾っていた。
5. **LDR — `detailed` の `iterations` が渡っていない**: `research.json` の `iterations` が 3 のまま
   （`retry_iterations = 5` を設定していたのに）。原因は `detailed_research(query, settings_snapshot=
   None, progress_callback=None, **kwargs)` が `iterations`/`questions_per_iteration` を名前付き引数
   として宣言しておらず、`**kwargs` に落ちて黙って無視されていたこと（`settings_override` と同じ
   構造の罠。ADR-0029 の既知の記録の類例）。
6. **LDR — プロキシの一過性の 503 が「答え」として保存される**: 昇格直後の llm-proxy で候補が一瞬
   全部無くなり `no_source_available` の 503 を返した瞬間、LDR 自身が LLM 呼び出しの例外を握りつぶし、
   `str(exc)` をそのまま `summary` に書いて「合成できた」ことにしていた。`report.md` は 986 バイトの
   エラー文面だけになり、`sources_cited=0` で `insufficient web evidence` の hard error になった。

### 決定と実装

- **A1（OpenAlex の礼儀）**: `paperqa_acquire.py` に `RateLimiter`（OpenAlex/Unpaywall/Semantic
  Scholar 共通、1 req/sec）と `fetch_with_retry`（429 を `Retry-After` かフォールバックの 2/4/8 秒で
  最大 3 回まで再試行）を追加。`openalex_url` は `mailto` を**常に**付ける（未設定なら
  `DEFAULT_CONTACT_EMAIL`）。OA 探索（Unpaywall/Semantic Scholar）は元から PDF が無い候補だけに限って
  いた（変更なし）。
- **A2（cited の数え方）**: `pqa` の CLI に `--output json` 相当が無く、実際に使われた
  `Answer.contexts` を確実に取り出す口を確認できなかったため、埋め込み Python への全面書き換え
  （`pqa ask` を捨てて `paperqa` の Python API を呼ぶ）は採らなかった。代わりに、`pqa` の生の標準出力
  にある `References`/`Sources` 見出し（PaperQA2 が `Answer.contexts` から機械的に組み立てる一覧で、
  答えの本文にインライン引用マーカーが無くても出ることがある）を `extract_references_section` で
  切り出し、`answer_cites(references, candidate)` を本文一致との**和**として `cited` に数える
  （`paperqa.rs`）。`--output json` が実機で使えることを確認できたら、そちらを優先する再検討をする
  （未解決事項）。
- **A3（report.md）**: `answer.md` を書く箇所で同じ内容を `artifacts/report.md` にも書き、両方を
  成果物として申告する（`paperqa.rs`）。`config/celeris.research.example.toml` の
  `[[genres]] id = "literature"` の `output_artifacts` と `preamble.rs::actions_instructions`（CoS
  への指示文）で `report.md` を標準として案内する。
- **A4（LLM 呼び出しの再試行）**: PaperQA 自身の LLM 呼び出しの再試行は litellm の
  `litellm_params.num_retries` が担う。`config/paperqa.qwen-local.example.json` の
  `llm_config`/`summary_llm_config`/`agent.agent_llm_config` の `num_retries` を 1 → 3 に上げた
  （celeris のコードは触らない。運用者が使う設定ファイルの変更）。
- **B1（必読の一次情報の本文取り込み）**: `local_deep_research.rs::must_read_urls` の知識ベース側の
  収集に `http(s)://` フィルタを追加（`human` のような値を落とす）。`local_deep_research_run.py` に
  `build_primary_source_entries`（GitHub/GitLab のリポジトリ URL は `raw.githubusercontent.com/.../
  HEAD/README.md`（GitLab は `/-/raw/HEAD/README.md`）、それ以外は HTML → `html_to_text` で最大 6 KB
  に切ったテキスト）を追加し、「## 必読の一次情報（本文抜粋）」として research question の直後に
  足してから LDR の各モード関数を呼ぶ。`add_must_read_sources` は `http(s)://` 以外を捨て、追加した
  各エントリに `primary: true` を付ける。`build_evidence_manifest` はその `primary` フラグを
  `sources.json` にも引き継ぐ。答えが出た後、`apply_primary_source_citations` が report の本文
  （書き終えた `report.md` を読み直す）に抜粋の内容や URL 自身が現れるか調べ、現れれば該当ソースを
  `cited: true` にする（`excerpt_is_cited`。URL 一致、または抜粋の 1 行〈24 字以上〉の verbatim
  一致）。
- **B2（`detailed` の `iterations`）**: `iteration_setting_overrides(func, iterations,
  questions_per_iteration)` を追加。`inspect.signature(func)` で実際に宣言されている名前だけを直接
  kwarg として渡し、宣言されていなければ `search.iterations`/`search.questions_per_iteration` として
  settings（`settings_override`/`settings_snapshot` 経由）に回す。`detailed_research` は
  `query`/`settings_snapshot`/`progress_callback` しか宣言しないので、両方とも settings 経由になる。
  quick/report が実際に `iterations` を宣言していれば（未確認、`inspect.signature` が失敗した場合と
  同じ扱い）直接 kwarg のまま。
- **B3（例外を report にしない・再試行）**: `UpstreamLlmError` / `detect_upstream_llm_error`
  （`error code: \d{3}` や `no_source_available` 等の文面を検出）/ `call_ldr_stage`（例外と偽装エラー
  結果の両方をこの型に統一）/ `run_with_retries`（2/4/8 秒のバックオフで最大 3 回再試行、
  `time.sleep` を呼び出し時に解決するので `main()` を実際に呼ぶテストでも注入できる）を追加。
  最終的に失敗すれば `report.md` を書かない（`report` モードは `generate_report` 自身が既に書いた
  ファイルを消してから再試行/終了する）。**celeris の既存の契約（非 0 の exit code + stderr の短い
  メッセージ）はそのまま使う**: 新しい `result.json` ファイルは作らなかった（Rust 側
  〈`run_ldr`〉に新しい契約を追加するのは本 Phase の変更範囲を超えると判断したため。既存の
  「exit≠0 → `Terminal::Error{retryable:true}`、`report.md` は書かれていないので成果物にならない」
  という経路で、指示の意図〈report.md を書かない・retryable なエラーとして扱う・再試行する〉は
  満たしている）。
- **B4（`insufficient_is_error` を LDR にも）**: `EvidenceThresholds.insufficient_is_error`
  （既定 `false`）を追加。`search_results == 0`（検索経路の問題）は従来どおり常に hard error。
  それ以外の閾値未達は既定では `Terminal::Done` にし、`append_evidence_quality_section` が
  `report.md` の末尾に「## 証拠の質」節（出典/引用/ドメイン数、必読の一次情報のうち使われた件数、
  証拠不足ならその理由）を足す（`local_deep_research.rs`）。B3 の例外（`UpstreamLlmError` 由来の
  非 0 exit）はこの節の対象外で、そのまま `Terminal::Error` になる（証拠不足と呼び出し失敗は別物の
  まま）。
- **C1（プロキシの再走査）**: `chat_completions` は `attempts_for` が空を返しても即 503 にせず、
  1 秒待って最大 2 回まで（合計 2 秒、要件の 3 秒以内）候補列を再計算する。それでも空なら
  `Retry-After: 5` を付けて 503 を返す（`llm-proxy/src/server.rs`）。起動直後の probe 未完了の
  source が「未知」として候補に含まれるか確認したところ、`ProxyState::reachable` は probe
  キャッシュが無いとき（起動直後）実際に probe を実行してから結果を使っており、「未知だから除外」
  という扱いにはなっていなかった（確認のみ、コード変更なし）。

### ゲート

- `cargo test --workspace --no-fail-fast`: exit 0、FAILED 0（下記「証拠」節に詳細）。
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0。
- `docs/api`/`docs/protocol` に差分なし（`git status` で確認。型は変えていない）。

### 未解決事項（Phase 109b）

- A2: `pqa ask` に `--output json`（または同等の構造化出力）が実際にあるかどうかを実機で確認して
  いない。あれば `Answer.contexts`/`used_contexts` を直接読む実装に置き換えるほうが、`References`
  見出しの体裁に依存する現在の実装より確実（P-109b-1）。
- B1: `excerpt_is_cited` は「抜粋の 1 行が本文に verbatim で現れるか」という粗い基準で、LLM が
  抜粋を言い換えて使った場合は `cited` にならない（本文一致の限界。ADR-0035 の `answer_cites` と
  同じ種類の妥協）。
- B3: 「report.md を書かず、retryable な error にする」を celeris の既存の exit code / stderr
  契約の中で実現した（新しい JSON ファイルは作っていない）。将来 LDR 以外のアダプタでも同種の
  「成功した体裁の失敗」が見つかったら、`crates/task-worker/src/protocol.rs` 側に共通の型を
  足すかどうか検討する（P-109b-2）。
- C1: 「候補が一瞬全部消える」事象そのものの根本原因（claude-oauth の 429/cooldown から codex への
  切替の一瞬）は直していない（再走査で覆うだけ）。頻発するようなら `record_failure` 側の cooldown
  の付け方を見直す。

## Phase 109c 追記（2026-09-23）

Phase 109b を本番で試した 3 回目のやり直し（2026-09-23 12:12〜12:26 UTC）で見つかった「対象ごとの
整理が無い」欠陥と、その直し。本文（上記 D1〜D4・受け入れ条件 1〜5、Phase 109b 追記）は書き換えない。

### 観測

1. **Web 調査（LDR）**: 必読の一次情報 6 件は全て fetch され `cited: true` になったが、1 回目の
   不合格理由（BeeOND の公式章の本文が無い）を「## 必ず埋める項目」として**問いの先頭**に足した
   再挑戦で、report 全体が BeeOND だけの報告になった。他 4 システムは末尾に README の生テキストが
   貼られているだけで、deployment model の整理（server/client 配置・cache/direct I/O・file
   semantics・replication 等）が無く、無い項目に「未確認」も付いていなかった。
2. **文献調査（PaperQA）**: `answer.md` は 35 行の総論で、対象ごとの整理も比較分類も無く、
   引用マーカーも無いため `cited=0` のままだった。8 システム×6 観点を 1 つの質問に詰め込んでいた。

### 決定と実装

- **A（対象と観点を構造として扱う）**: `crates/task-worker/src/research_targets.rs` に純関数
  `research_targets(objective) -> Vec<String>`（`/`・`、`・`・`・`，` で連なる英数字識別子の列を対象の
  列挙とみなす。URL のパス区切りは先に取り除く）と `research_aspects(objective) -> Vec<String>`
  （目的文の括弧書きの列挙を優先、無ければ既定 7 項目）を追加。PaperQA / LDR 両方の入力 JSON
  （`ask` の問い / `ldr_input.json`）と `research.json` に `targets`/`aspects` として載せる。
  `preamble.rs`（CoS への指示文）に「対象を 1 行に列挙し、観点を括弧で列挙する。対象が 5 を超えるなら
  対象ごとにタスクを分ける」を追記し、受け入れ条件の一文を「**対象ごとに**、指定の観点が一次情報
  （または文献）に基づいて整理されている。確認できない観点は『未確認』と明記されていれば不合格の
  理由にしない」に具体化した（D）。
- **B1（LDR: 問いを置き換えない）**: `local_deep_research.rs::build_query` の組み立て順を
  「目的文 → 前回からの改善点（見出しを「## 必ず埋める項目」から「## 前回からの改善点（必ず埋める）」
  に改名）→ 前回の report.md（`attempts >= 1` のときだけ、先頭 20 KB）→ 人間の回答」に変更した
  （Phase 109 まで「必ず埋める項目」を先頭に置いていたことが、LDR がそこだけに検索・合成を引きずられる
  原因だった）。前回の report.md は `run_ldr` が消す前に読む（`read_prior_report`）。
- **B2（LDR: docs サイトの深追い）**: `local_deep_research_run.py::build_primary_source_entries` に、
  必読 URL が readthedocs / `doc.`・`docs.` サブドメイン / `/docs/` パスのとき、トップ HTML の
  同一ホスト・1 階層下のリンクのうち URL かリンクテキストに `config|configuration|deploy|install|
  setup|tuning|architecture|usage|admin|semantics` を含むものを最大 8 ページ（`select_docs_subpages`）
  追加で fetch する処理を足した（各ページ 6 KB、呼び出し全体で合計 48 KB の予算。
  `DOCS_SUBPAGES_TOTAL_MAX_CHARS`）。GitHub の `docs/` 直下の `.md` 一覧（ADR 原案の一部）は、
  ディレクトリ一覧に GitHub API が要り、鍵無し・決定的なテストが組みにくいため Phase 109c では
  見送った（README + docs サブページの深追いで実際の欠陥は塞がる。未解決事項 P-109c-1）。
- **B3（LDR: 構造化合成）**: `local_deep_research_run.py` に `apply_structured_synthesis` を追加。
  `targets` が非空（既定 `structured_synthesis = true`）のとき、LDR の findings と必読の一次情報の
  抜粋を材料に、LDR と同じ `settings` の `llm.model`/`llm.openai_endpoint` へ 1 回 `chat/completions`
  を叩き、対象×観点の表 + 対象ごとの節（`# 対象別の整理`）を `report.md` の**先頭**に足す（LDR の生の
  findings はそのまま後段に残る）。プロンプトは「各セルは事実（出典）か『未確認』のどちらか。推測
  しない」と明記。呼び出しは既存の `run_with_retries`/`call_ldr_stage`（2/4/8 秒バックオフ）を再利用
  し、**best-effort**（最後まで失敗しても `report.md` は書き換えず run 自体も失敗にしない。LDR の生の
  findings が既にフォールバックとしてある）。`[adapters.local_deep_research] structured_synthesis`
  （既定 `true`）で無効化できる。
- **C（PaperQA、縮小版）**: 対象ごとに `pqa ask` を複数回呼ぶ（元の C2）と PaperQA の Python API への
  切り替え（元の C1、`session.contexts` から `cited` を数える）は、**この開発環境からは実機の
  `paperqa`/`pqa` の Python API 面を検証できず**（パッケージ未導入・ネットワーク禁止）、既存の
  paperqa.rs のテスト（43 件、`pqa` CLI の argv 組み立てを前提にしたスタブ多数）への影響も大きいため、
  Phase 109c では**見送った**（未解決事項 P-109c-1 として記録。実機で API を確認できる環境で
  改めて取り組む）。代わりに、目的文から対象が取れているとき `build_question` が 1 回の `pqa ask`
  の問いに「対象ごとに `### <対象名>` の節を立て、観点ごとに `事実（出典）` か `未確認` で書き、
  最後に対象×観点の Markdown 表でまとめよ」という形式指示を足す縮小策を実装した
  （`render_structured_answer_instructions`）。対象×観点の情報を `research.json` にも残す。
  `cited` の数え方（Phase 109b A2 の文字列一致 + References 節）自体は変えていない。

### ゲート

- `cargo test --workspace --no-fail-fast`: exit 0、**FAILED 0**（passed 合計 **1955**、Phase 109b の
  1937 から新規 18 件増: `research_targets` 11 件、`local_deep_research` +5 件、`paperqa` +2 件）。
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0（`research_targets.rs` の
  `manual_pattern_char_comparison` を 1 件修正済み）。
- `git status --porcelain | grep -E "docs/api|docs/protocol"`: 出力なし（型を変えていないので
  schema/types.ts の再生成は不要）。

### 未解決事項（Phase 109c）

- **P-109c-1（PaperQA の Python API・対象ごとの複数 ask）**: `pqa ask` の CLI から
  `from paperqa import ask, Settings`（または `Docs.query`）への切り替えと、対象ごとに 1 回ずつ
  `ask` を呼んで `session.contexts` の和集合で `cited` を数える実装は、実機で API を確認できる環境
  （paperqa パッケージが入った venv）でなければ安全に進められないため見送った。今回実装した
  「1 回の問いの中で構造化させる」縮小策（`render_structured_answer_instructions`）は、対象別の整理・
  『未確認』の明記という失敗の症状には対処するが、PaperQA2 自身の引用マーカーが本文に出ない場合の
  `cited=0` は直っていない（Phase 109b A2 の限界がそのまま残る）。
- **P-109c-2（GitHub の `docs/` 直下ファイル一覧）**: B2 で見送った、GitHub リポジトリの `docs/`
  直下の `.md` を API 経由で列挙する処理。今回は README + docs サブページ（readthedocs 等）の深追いで
  実際の欠陥（FINCHFS の `finchfs.readthedocs.io`）は塞げているが、GitHub の `docs/` に一次情報を
  置くプロジェクトでは同種の欠落が再発しうる。
- **P-109c-3（LDR の構造化合成の材料の上限）**: `structured_synthesis_material` は本文+抜粋を
  20000 字で単純に切り詰めるだけで、対象ごとに均等な材料を渡す配慮は無い（対象数が多いと後半の対象の
  材料が削られやすい）。目的文の対象が 6 件を超えるケースが増えたら見直す。
- 実機未確認（ADR-0009 P-34）。本番での確認（親エージェントが行う）: Web 調査（BeeOND を含む対象）を
  やり直し、対象×観点の表が出て reviewer が通ること。文献調査を（対象の取れる目的文で）やり直し、
  `research.json`/`answer.md` に対象別の節が出ること（`cited` は Phase 109b までの仕組みのまま）。

## Phase 109d 追記（2026-09-23）

Phase 109c で見送った P-109c-1（PaperQA の Python API 切り替え、対象ごとの複数 `ask`）に、親が本番の
venv で `paperqa==2026.8.12` を実際に `inspect` して確認した API 面（`paperqa.ask`/`Settings`/
`PQASession`/`Context`/`Text`/`Doc` の公開名とフィールド）を前提に、改めて取り組む。本文（上記
D1〜D4・受け入れ条件 1〜5、Phase 109b・109c 追記）は書き換えない。

### 決定と実装

- **C1（`pqa ask` CLI → PaperQA の Python API）**: 新しい埋め込み Python ランナー
  `crates/task-worker/src/paperqa_ask.py` を追加（`paperqa_acquire.py` と同じ「`include_str!` で
  run ディレクトリに書き出し、`<入力 JSON>` を渡して起動する」流儀）。`import paperqa` /
  `from paperqa import Settings, ask` は `try/except ImportError` で守り、失敗すれば
  `output_path` に `{"error": ..., "answers": []}` を書いて **exit 3** で終わる（`paperqa` が
  入っていないテスト環境やまだ設定していない venv 向け）。入力 JSON は
  `{settings_name, settings_dir, paper_directory, index_directory, index_name, model,
  targets, aspects, comparison_target, max_asks, fallback_question, output_path}`
  （ADR 原案の `questions: [{id, target, question}]` は、質問の組み立て自体を
  `paperqa_ask.py::build_questions_for_targets` という**純関数**にして `python3 -c` から直接テスト
  できるようにするため、Rust 側で事前に組み立てず、上記の材料だけを渡してランナー自身に組ませる形に
  変えた。目的は同じ — 対象ごとに小さく問う — で、実装の置き場所を変えただけ）。`PQA_SETTINGS_DIR` は
  `settings_dir` に設定してから `Settings.from_name(settings_name)`（無ければ `Settings()`）、
  `settings.agent.index.paper_directory`/`index_directory`/`name` を上書きし、`model` があれば
  `settings.llm` も上書きする（CLI の `--llm` と同じ意味）。各 `ask()` は 2/4/8 秒のバックオフで
  最大 3 回まで再試行する（`ask_with_retries`/`is_transient_error`。503/429/502/接続断のみ再試行、
  それ以外は即座に伝播）。1 問が最終的に失敗しても他の問いは続け、その問いは
  `{"error": "..."}`・空の答えとして記録する（run 全体は失敗にしない）。
- **C1'（`paper_qa_ask.py` の純関数）**: `build_questions_for_targets`（質問の生成。対象が空なら
  `fallback_question` の単一問い、対象があれば対象ごとの問い + 空きがあれば総括の問い、
  `max_asks` を超えない）、`flatten_contexts`（`PQASession.contexts` 相当を `{docname, dockey,
  citation, score, question}` に平坦化。辞書でも属性アクセスの実物でも動く `_get` ヘルパ経由）、
  `build_target_aspect_table`（対象×観点の Markdown 表。観点が答えに無ければ `未確認`）の 3 つは
  すべて標準ライブラリだけで書かれ、`paperqa` パッケージ無しに `python3 -c` から直接呼べる
  （テストの (b)(c)(d) はこれで確認する）。
- **C2（Rust 側、`paperqa.rs`）**: `run_paperqa` の 2 段目を `pqa ask` の `Command` 組み立てから
  `paperqa_ask.py` を起動する形に全面書き換え（`run_acquire` と対になる構造）。`[adapters.paperqa]
  command` の既定を `"pqa"` → `"python"` に変更（`task_worker::PaperQaConfig::default`・
  `celeris::config::default_paperqa_command`）。旧 `pqa` 実行ファイルを指す設定が来たら
  `resolve_ask_command` が同じディレクトリの `python` に自動で置き換えて `tracing::warn!` を 1 回出す
  （`config.command` の `file_name()` が `"pqa"` と一致するかだけを見る、決定的な判定）。
  `config.settings`（設定ファイルへのフルパス、拡張子無し。既存の `settings_llm` と同じ前提）は
  `split_settings_path` で `settings_dir`（親ディレクトリ）/`settings_name`（ファイル名、`.json` が
  付いていても剥がす）に分ける。`docs/`・`config/celeris.research.example.toml` を更新（`command` の
  既定例を venv の `python` に、`pqa ask`/`--llm` 等の CLI 前提の記述を Python API 前提に書き換え）。
  `pqa` の生の rich 出力をパースしていた `extract_answer`/`extract_references_section`/
  `strip_log_prefix`/`strip_ansi` は不要になったので削除した（構造化 JSON を直接読むため）。
  `exit == 3` は「一般の非 0 終了」とは別扱いにし、`retryable: false` の分かりやすいメッセージ
  （「paperqa が import できない。`[adapters.paperqa] command` が指す環境に `paperqa` を入れること」）
  にする（インストールしない限り再試行しても直らないため。テスト (e)）。
- **C2'（`cited` の数え方）**: `research.json.evidence.cited` は、全 `ask()` 呼び出しの `contexts` に
  現れた `docname`/`dockey`（英数字だけに正規化して比較。`parsing.use_doc_details = false` だと
  PaperQA2 は corpus のファイル名から `docname` を作るので、既存の `answer_cites` と同じ「ファイル名の
  語幹」で突き合わせる）の和集合と、答え全文（全問いの `answer` を連結したもの）に対する既存の
  `answer_cites`（文字列一致、補助）との **OR**。新しく `cited_from_contexts: u32`
  （`context_identifiers`/`candidate_in_contexts` で突き合わせられた件数）を `research.json.evidence`
  と「## 証拠の質」節に足した。`sources.json` の `cited` も同じ定義（`marked` を作る 1 箇所を直しただけ
  で両方に伝わる）。Phase 109b A2 の `References`/`Sources` 節スクレイピングはこの数え方に置き換わり、
  不要になった。
- **C3（対象ごとの質問）**: `research_targets(objective)` が対象を返すとき、対象ごとに 1 問
  （「<対象> について、次の観点を提示された文献の範囲で答えよ: <観点リスト>。文献に無い観点は
  『未確認』と書け。各事実に引用を付けよ。」）+ 目的文に比較先があれば総括 1 問（「対象ごとに
  <比較先> と『公平比較可能』か『背景比較のみ』かを分類し理由を1行で述べよ」）。比較先は
  `research_targets.rs::comparison_target(objective)`（「<識別子>と比較」「<識別子>との比較」を
  決定的に抜く、新規の純関数）。上限は `[adapters.paperqa] max_asks`（既定 8）で、対象は先頭から。
  総括は「対象を全部入れてなお枠が余っていれば」だけ追加するので、対象数が `max_asks` ちょうどか
  それを超えていれば総括は入らない（テスト (b)）。対象が 1 つも取れなければ、`build_question`
  （Phase 109c までの単一問いの組み立て。前置き・目的文・答えの人間の回答履歴）の出力を
  `fallback_question` としてそのまま使う（テスト (c)）。対象・観点は引き続き `research.json` にも残す
  （`write_research_json`、変更なし）。
- **C4（`answer.md`/`report.md` の組み立て）**: 対象が 1 件でも取れていれば「# 対象別の整理」
  （`render_target_sections`: 表 → `### <対象>` の節（その対象の答え全文）→ （総括の問いがあれば）
  `## 総括`）を先頭に置く。表の組み立て自体は Python 側（`build_target_aspect_table`。C1' 参照）が
  やり、Rust はその文字列をそのまま埋め込むだけ（対象ごとの節・総括の並びは Rust 側
  `render_target_sections` が組む）。対象が無ければフォールバックの単一の答えをそのまま使う。
  続けて「## 引用された文献（contexts）」（`render_contexts_section`: `docname` で重複排除し、
  `citation`（無ければ `docname`）とどの問い〈`id`〉で使われたかを列挙）、既存の「## 出典」
  （`render_sources_section`。取得の段を行ったときだけ）・「## 証拠の質」・「## 一次情報（実装）」は
  そのまま続く。

### ゲート

- `cargo test --workspace --no-fail-fast`: exit 0、**FAILED 0**（passed 合計 **1960**、Phase 109c の
  1955 から新規 5 件〈`research_targets` の `comparison_target` テスト 2 件、`paperqa` の
  `resolve_ask_command`/`split_settings_path` テスト 2 件、`paperqa_ask.py` の純関数を
  `python3 -c` から呼ぶテスト 1 件〉。既存 45 件の `pqa` CLI argv ベースのテストは、`ask_input.json`
  を読むテスト（`ask_input_carries_settings_and_index_paths`/`ask_input_defaults_when_...`/
  `ask_input_carries_model_only_when_set`）や、実物の `paperqa_ask.py` を `sys.modules["paperqa"]`
  の偽物越しに動かすテスト用スタブ〈`ask_stub_script`。LDR の `sys.modules["local_deep_research"]`
  注入と同じ考え方〉に書き換え、件数はほぼ変わらない）。
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0（警告 0）。
- `git status --porcelain | grep -E "docs/api|docs/protocol"`: 出力なし（型を変えていないので
  schema/types.ts の再生成は不要）。

### 変更ファイル

- 新規: `crates/task-worker/src/paperqa_ask.py`
- `crates/task-worker/src/paperqa.rs`（C1・C2・C2'・C3・C4。`extract_answer`/
  `extract_references_section`/`strip_log_prefix`/`strip_ansi` を削除し、`AskAnswer`/`AskContext`/
  `AskOutput`/`resolve_ask_command`/`split_settings_path`/`context_identifiers`/
  `candidate_in_contexts`/`render_target_sections`/`render_contexts_section` を追加）
- `crates/task-worker/src/research_targets.rs`（`comparison_target` を追加）
- `crates/celeris/src/config.rs`（`PaperQaAdapterConfig.max_asks`、`command` の既定を `python` に）
- `crates/celeris/src/lib.rs`（`PaperQaConfig` の構築に `max_asks` を配線）
- `config/celeris.research.example.toml`（`command` の例を venv の `python` に、`max_asks` の例と
  コメントの更新）

### 未解決事項

- P-109d-1: `answer_cites`（文字列一致の補助）は Phase 109b A2 からの既存の限界（言い換えられた引用は
  拾えない）がそのまま残る。`contexts` ベースの突き合わせ（C2'）はこの限界を実際の証拠で補うが、
  `docname` がファイル名以外（`parsing.use_doc_details = true` 等）から作られる設定では
  `candidate_in_contexts` のファイル名突き合わせが機能しない可能性がある（本番の設定
  `parsing.use_doc_details = false` を前提にしている。ADR-0027 参照）。
- P-109d-2: `build_target_aspect_table`（Python 側）のセル抽出（`_extract_aspect_line`）は
  「`- <観点>: <内容>` 形式の行」を優先し、無ければ「観点の文字列を含む行をそのまま使う」という
  素朴な規則。モデルが指示した形式を守らない答え方をした場合、本当は書いてある事実を「未確認」扱いに
  してしまう可能性がある（表は補助であり、対象ごとの節に答え全文は残るので、実害は限定的）。
- 実機未確認（ADR-0009 P-34）。本番での確認（親エージェントが行う）:
  `[adapters.paperqa] command` を venv の `python`（`paperqa` パッケージが入ったもの）に向け、対象の
  取れる目的文（例 BenchFS の文献調査）で再実行し、(1) `research.json.evidence.cited_from_contexts`
  が 0 より大きいこと、(2) `answer.md` の先頭に「# 対象別の整理」（対象×観点の表）が出ること、
  (3) 「## 引用された文献（contexts）」に実際に使われた文献が出ることを確認する。

## Phase 109e 追記（2026-09-23）

本番で観測（run `01M378JWR702TADBQCQRCEAB5V`）: `Settings.from_name` は `PQA_SETTINGS_DIR` を見ない
（`pqa_directory("settings")` は `~/.pqa/settings/` 固定。本番 venv の `paperqa==2026.8.12` のソースで
確認）ため、Phase 109d C1 の「`PQA_SETTINGS_DIR=settings_dir` を設定してから `from_name`」は常に
`FileNotFoundError` になっていた。`paperqa_ask.py` は Rust 側が新たに組む `settings_path`（絶対パス、
`.json` 付き。`split_settings_path` を 3 要素に拡張）が実在すればそれを `Settings.model_validate_json`
→ `model_dump()` → `Settings(**...)` で直接読み（`load_settings`）、無ければ従来どおり
`Settings.from_name(settings_name)` にフォールバックする（paperqa 自身の同梱設定名向け）。
`PQA_SETTINGS_DIR` の設定は削除した。どちらでも見つからなければ `SettingsResolutionError` → exit 2、
アダプタは探したパスを含むメッセージで `retryable: false` にする（設定の誤りは再試行しても直らない）。

## Phase 109f 追記（2026-09-23）

本番で観測（run `01M37A3JMMFXVY30SWD4EZ01JN`）: 目的文の末尾に人が足した節見出し
「## 方針（人の指定、2026-09-23）」の括弧内が `research_aspects` に観点の列挙と誤認され、
`research.json.aspects` が `["人の指定", "2026-09-23"]` になった。目的文本体の「各システムの目的・
semantics・deployment model・server/core利用・data pathを整理し」は拾われず、`targets` も
「Mochi-Margo-Mercury、UCX、io_uring」が「(必要ならDAOS/Lustre)」の括弧に阻まれて落ちていた。
`research_targets`/`research_aspects` を**目的文の最初の段落**（`\n\n` または `## ` 見出しより前）
だけを見るように制限し、目的文中の「(必要なら…)」「(optional …)」「（任意…）」の括弧は対象抽出前に
取り除き、識別子の直後に区切りなしで非 ASCII 文字が続く場合（例: `io_uring・RDMA統合` の `RDMA`）は
複合語の一部とみなして対象から外した。観点は「各…の A・B・C を整理」の形からも拾うようにし、日付・
「人の指定」系の語・長すぎる文・URL・1 文字を弾く妥当性チェックを追加した（`MAX_ASPECT_CANDIDATE_CHARS`
はプロセの丸ごと混入を弾く目的で緩め（24 文字）に設定。`deployment model`/`server/core利用` のような
複合語の観点を落とさないことを優先し、仕様の目安「10 文字」とはあえて一致させていない）。

追加で、明示の「対象:」/「観点:」マーカー（`preamble.rs` が調査系タスクの `objective` の 1 行目に
指示する形式）にも対応した。親エージェントからの追加観測（run `01M37AZ129EMB93N50MZ132S8K`）で、
`preamble.rs` が例示していた `CHFS / FINCHFS / …`（`/` の前後に空白がある書き方）が明示形でも認識
されず、一般走査へのフォールバックで括弧内の観点の一部（`server/core`）がそれらしい鎖として誤検出
された（`targets = ["model", "server", "core"]`）ことが分かった。マーカー行の丸括弧はまるごと
（中身を問わず）取り除いたうえで、区切り文字（`/`・`、`・`，`・`,`）の前後の空白を畳んで正規化してから
対象の鎖を抽出するようにした。`preamble.rs` の指示文も `対象: <対象1> / <対象2> / …（観点: <観点1>、
<観点2>、…）` の明示形に合わせて書き換えた。

さらに、`paperqa_ask.py::build_target_aspect_table`（対象×観点の表の組み立て）が、PaperQA の答え
（`formatted_answer`）の先頭に質問文がそのままエコーされるケース（ローカルモデルでよくある挙動）で、
質問文自体が全観点名を含んでいるために `_extract_aspect_line` の緩い代替一致（観点名を含む行をそのまま
使う規則）がエコー行を拾ってしまい、全セルに質問文が貼られる事故が同じ観測から見つかった。
`_extract_aspect_line`/`build_target_aspect_table` に `_strip_echoed_question` を追加し、
`Question:`/`質問:`/`質問：` で始まる行と、その対象への質問文（`answer.question`）と一致する行を
観点抽出の前に取り除くようにした（見つからなければ従来どおり「未確認」）。

## Phase 109g 追記（2026-09-23）

本番で観測（2026-09-23 15:12〜15:27 UTC、文献調査 `01M37D6TMRZSBQDC1SKBV22JAW`、Phase 109f）:
抽出は正しく（`targets`/`aspects` とも本番の目的文どおり）、`report.md` に対象別の表・節・引用文献の
一覧が出るところまでは通ったが、**全 8 対象で「BenchFS との比較分類」が『未確認』**のまま reviewer が
2 回とも不合格にした（「公平比較可能」「背景比較のみ」の語が全文で 0 件）。原因は、この分類が文献検索
ではなく**BenchFS の設計条件（知識ベース `projects/benchfs/architecture-overview.md`）と対象の設計を
照らす判断**であるのに、Phase 109d C3 の総括の問いは PaperQA（文献の範囲でしか答えない）にそのまま
「BenchFS と比較して分類せよ」とだけ投げていたこと。副次的に、対象の主要論文（CHFS の HPC Asia 2022
論文、GekkoFS の IEEE Cluster 2018 論文など）が corpus に毎回入るとは限らない（検索語が LLM 生成で
run ごとに揺れる）ことも分かった。本文（上記 D1〜D4・受け入れ条件 1〜5、Phase 109b〜109f 追記）は
書き換えない。

### 決定と実装

- **A（比較分類は判断として立てる）**: `comparison_target` があるとき、総括の問いを「設計条件の提示 →
  必ず二択で分類 → 確度も添える」という判断の形に作り直した。設計条件の材料は、`paperqa.rs` が
  `ask_input.json` の `comparison_context` に材料（`knowledge_root`、`knowledge_index`〈`context.knowledge.index`
  の `path`/`title`/`tags` だけを写したもの〉、目的文の比較の言い回しを含む 1 文〈
  `research_targets::comparison_target_paragraph`、新規の決定的な純関数〉）として渡すだけにとどめ、
  実際のページ探索・本文の読み込み・切り詰めは `paperqa_ask.py` 側の新しい純関数
  （`find_comparison_page_path`／`strip_front_matter_block`／`comparison_design_context`）と、それを
  ファイル読み込みと組み合わせる `load_comparison_design_context`（`main()` から呼ぶ、唯一の非純粋な
  部分）に任せた。`context.knowledge` は索引だけ（本文は入れない。ADR-0047 D2）という既存の境界を
  ハーネス側では守り、`task-worker` は KB のファイルを直接読まない（`docs/protocol` の型
  〈`RunRequest`/`RunContext`/`KnowledgeContext`〉は一切変えていない。`PaperQaConfig.knowledge_root`
  という**Rust 内部の**設定フィールドを 1 つ足しただけ）。
  - `build_questions_for_targets`（既存、Phase 109d C3）はそのまま: 総括の問いの**プレースホルダ**
    （旧来の 1 行の分類問い、未確認を許す）を積む。`main()` は対象ごとの答えが出そろってから、
    設計条件が見つかっていれば `build_comparison_question`（新規）でこのプレースホルダを**置き換えて**
    から `ask()` する。「設計条件同士の比較で判断する。文献に BenchFS への言及が無いことは理由に
    ならない」「必ず『公平比較可能』か『背景比較のみ』のどちらかに分類し、確度（高/中/低）も添えよ」を
    明記し、対象ごとの答え（各 1.5 KB に切り詰め）を材料として問いに埋め込む。設計条件が全く見つから
    なければ（KB にページが無く、目的文にも比較の言い回しが無い）従来の 1 行の問いのまま（この場合だけ
    『未確認』を許す、という ADR の決定どおり）。
  - `build_target_aspect_table`（Python）に `comparison_target` 引数を追加。「<比較先> との比較分類」列
    （`_is_comparison_aspect`: 観点名が「比較」と比較先の名前を両方含むかで判定）は、対象ごとの答えでは
    なく**総括の答え**から `extract_comparison_classification`（新規）が分類語（『公平比較可能』/
    『背景比較のみ』）と確度（`（高）`/`確度: 高` のどちらの表記も）を抜いて埋める（無ければ
    『未確認』のまま）。
  - `paperqa.rs::render_target_sections` に `comparison_target: Option<&str>` を追加し、総括の節の見出し
    を比較先が分かっていれば「## <比較先> との比較分類」に変える（無ければ従来どおり「## 総括」）。
- **B（既知の論文を種にする）**: `paperqa.rs::kb_primary_source_seed_urls`（新規、`local_deep_research::
  must_read_urls` と同じタグ判定）が知識ベースの索引のうち `primary-sources`/`一次情報` タグを持つ
  ページの `sources` を拾い、`extract_seed_urls`（目的文 + `inputs` 由来）の結果に重複排除して足す。
  DOI/arXiv/PDF は取得ランナーの `acquire_input.json.seed_urls` に（Phase 109 の seed 機構にそのまま
  載る）、GitHub は「一次情報（実装）」に回る（従来と同じ分岐）。人が置く著者版 PDF
  （`paper_directory/<project_id>/` 直下）は PaperQA の索引がディレクトリ全体を見るため次の run から
  自然に使われる（コード変更は不要。`config/celeris.research.example.toml` に運用の一文を追記した）。
- **C（受け入れの文）**: `preamble.rs::actions_instructions`（CoS への指示文）に「観点に比較先との比較
  分類を含める場合、それは文献検索ではなく比較先の設計条件との照合による判断で、必ず『公平比較可能』か
  『背景比較のみ』のどちらかを確度付きで出させる。分類の欠落は不合格の理由になる」を 1 行追記した。

### ゲート

- `cargo test --workspace --no-fail-fast`: exit 0、**FAILED 0**（passed 合計 **2028**、Phase 109f の
  1996〈`docs/PROGRESS.md` Phase 109f 参照〉から新規 32 件: `research_targets` +2 件
  〈`comparison_target_paragraph`〉、`paperqa`（Rust）+7 件〈`kb_primary_source_seed_urls` の単体テスト、
  `ask_input.json` の `comparison_context` テスト、seed の合流テスト、`render_target_sections` の見出し
  テスト〉、`paperqa_ask.py` の純関数を `python3 -c` から呼ぶテスト +1 件〈`comparison_design_context`/
  `build_comparison_question`/`extract_comparison_classification`/`build_target_aspect_table` 統合〉。
  既存テストは全てそのまま通る（`build_questions_for_targets`/`build_target_aspect_table` の呼び出し側
  シグネチャは新しい引数がデフォルト付き〈Python は既定引数、Rust は新しいテストのみ新シグネチャを使う〉
  なので、既存呼び出しは変更不要）。
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0（警告 0）。
- `git status --porcelain | grep -E "docs/api|docs/protocol"`: 出力なし（`RunRequest`/`RunContext`/
  `KnowledgeContext` の型は変えていない。`PaperQaConfig.knowledge_root` は docs/protocol の対象外の
  Rust 内部設定）。

### 変更ファイル

- `crates/task-worker/src/paperqa_ask.py`（`find_comparison_page_path`/`strip_front_matter_block`/
  `truncate_text`/`comparison_design_context`/`load_comparison_design_context`/
  `build_comparison_question`/`extract_comparison_classification`/`_is_comparison_aspect` を追加。
  `build_target_aspect_table` に `comparison_target` 引数、`main()` に総括の問いの置き換えを追加）
- `crates/task-worker/src/paperqa.rs`（`PaperQaConfig.knowledge_root`、`kb_primary_source_seed_urls`、
  `ask_input.json` への `comparison_context` の組み立て、`render_target_sections` の見出し分岐、
  seed URL の合流。新規テスト 7 件）
- `crates/task-worker/src/research_targets.rs`（`comparison_target_paragraph` を追加、テスト 2 件）
- `crates/task-worker/src/preamble.rs`（`actions_instructions` に一文追記）
- `crates/celeris/src/lib.rs`（`PaperQaConfig` の構築に `knowledge_root: Some(config.knowledge.root.clone())`
  を配線）
- `config/celeris.research.example.toml`（著者版 PDF の置き場と KB `primary-sources` タグの運用を追記）

### 未解決事項（Phase 109g）

- P-109g-1: `extract_comparison_classification` は「対象名を含む行」の中から分類語を探す行ベースの
  素朴な規則で、対象名が別の対象名の部分文字列になっている場合（例 `"FINCHFS"` は `"CHFS"` を部分文字列
  として含む）に誤って手前の行を拾う可能性が理論上ある。総括の問いでは「対象ごとに 1 行」の形式を明示
  指示しているため実害は限定的だが、モデルが対象名を並べて書いた場合（`"CHFS/FINCHFS どちらも..."`）は
  誤判定しうる（表は補助で、総括の節の本文には答え全文が残るので実害はさらに限定的）。
- P-109g-2: `find_comparison_page_path` は知識ベースの索引全体を線形走査する素朴な一致（タイトル/パス/
  タグの部分文字列）で、比較先の名前が別の無関係なページのタイトルに偶然含まれる場合に誤ったページを
  拾う可能性がある（例: 比較先 "CHFS" があり、"benchfs" という無関係なページタイトルにも "chfs" が
  部分文字列として含まれる、といった衝突）。実運用では比較先は固有のプロジェクト名（`BenchFS` 等）なので
  実害は小さいと見ているが、確認はしていない。
- 実機未確認（ADR-0009 P-34）。本番での確認（親エージェントが行う）: BenchFS の文献調査を（比較先が
  取れる目的文で）やり直し、(1) `research.json`/`ask_input.json` の `comparison_context` に KB の
  `architecture-overview.md` が拾われること、(2) `answer.md` の「## BenchFS との比較分類」節に対象ごとの
  『公平比較可能』/『背景比較のみ』と確度が出ること、(3) reviewer が「BenchFS との比較分類」観点を合格
  と判定すること。
