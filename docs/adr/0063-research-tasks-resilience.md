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
