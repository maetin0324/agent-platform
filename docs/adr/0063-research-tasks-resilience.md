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
