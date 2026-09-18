# ADR-0029: web-research 分野と Local Deep Research のハーネス

- 日付: 2026-09-17
- 状態: **Accepted**（人間の依頼「web-research を足して下さい」。人間の調査の結論「論文は PaperQA2、Web・実装・製品・仕様は Local Deep Research」に従う）
- 関連: ADR-0027（分野と `paperqa` アダプタ。本 ADR は同じ作りの 2 本目）、ADR-0028（能力レジストリ）、ADR-0026（ACP）

## 1. 文脈

ADR-0027 で `related-research`（PaperQA2）が入ったが、これは**論文だけ**を見る。新規性の確認では
「論文化されていないが既に実装・製品化されている」を見落とすので、Web・OSS・製品・仕様・技術報告を見る分野が要る（人間の調査の指摘）。

### このホストで確認した事実（2026-09-17）

- `local-deep-research` 1.10.7 を `uv` の隔離環境（`~/taskd/ldr/.venv`）に導入。**一発実行の CLI は無い**
  （実行ファイルは `ldr-web`（Flask の画面）と `ldr-mcp`（MCP サーバ）だけ）。使うのは Python API:
  `local_deep_research.api.{quick_summary, detailed_research, generate_report}`。
- 設定は `settings_override` の辞書で渡せる。必要なキーは
  `llm.provider = "openai_endpoint"` / `llm.openai_endpoint.url` / `llm.openai_endpoint.api_key` / `llm.model` /
  `search.tool = "searxng"` / `search.engine.web.searxng.default_params.instance_url`（既定は `http://localhost:8080` なので必ず上書きする）/
  `…default_params.max_results`。
- **このホストには SearXNG が既に動いている**（`127.0.0.1:8888`、JSON API も 200）。LLM は ADR-0026 と同じトンネル越しの
  vLLM Qwen3.8-27B（`127.0.0.1:18000/v1`）を使う。外に出るのは SearXNG が行う検索だけ。

## 2. 決定

### D1. `local-deep-research` アダプタ（`adapter = "local-deep-research"`）。PaperQA2 と同じ「調査エンジンを包む」形

- ADR-0027 D3 と同じ考え方: LDR はワーカープロトコルを話さないので、**アダプタが `artifacts/report.md` と
  `artifacts/result.json` を書き、成果物として申告する**。委譲はしない。終端の合成も同じ。
- LDR には CLI が無いので、**アダプタが実行用の Python スクリプトを持ち（`include_str!`）、run ごとに
  `runs/<run_id>/ldr_run.py` に書き出して `<python> <その場所>` で起動する**。taskd の外に置くファイルは venv だけ。
  スクリプトは taskd のバイナリと一緒に版が進む（外部ファイルの置き忘れ・版ずれが起きない）。
- スクリプトの約束（アダプタとの契約。どちらも taskd 側にあるので壊れない）:
  - 入力: 第 1 引数に JSON ファイル（`{query, mode, settings, iterations, questions_per_iteration, report_path}`）。
  - 標準出力: 1 行 1 メッセージの進捗（`progress: …`）と、最後に 1 行だけ `TASKD_RESULT {"summary": …, "sources": N}`。
  - `report_path` に本文（Markdown）を書く。失敗時は 0 以外で終了し、理由を標準エラーに出す。
- **`report.md` の形（Phase 32。実機で完走した web-research の run をレビュアーが不合格にした:
  「1477 行の未加工トランスクリプト。本文が Summary/Findings/Final synthesis で 3 回以上重複、
  同じ URL を 4 回書く水増しの参考文献」）。以後は決定的に（LLM を呼ばずに）次の形に固定する:**
  - 題名は LDR の統合結果（`formatted_findings`/`summary`）が `#` 見出しで始まればそれを使い、
    始まっていなければ objective の先頭 1 文（最大 80 字。objective 全文は使わない）。
  - 本文は `summary`/`formatted_findings`/`findings[].content` のうち実質同じもの（先頭 200 字が
    一致、または片方が他方を含む）を最も長い 1 つにまとめ、1 回だけ書く（区画見出しは付けない）。
  - `## 出典` は `sources` を引用番号順のまま URL で重複排除し、重複行は `[n] (= [m])` に畳む
    （本文中の `[n]` 参照を書き換えるのは安全にできないため。番号は保つ）。
- 設定:
  ```toml
  [adapters.local_deep_research]
  command = "/home/u/taskd/ldr/.venv/bin/python"   # LDR を入れた venv の python
  mode = "quick"                                   # quick | detailed | report（既定 quick）
  iterations = 2
  questions_per_iteration = 2
  env = { }
  settings = { "llm.provider" = "openai_endpoint", "llm.openai_endpoint.url" = "http://127.0.0.1:18000/v1",
               "llm.openai_endpoint.api_key" = "unused", "llm.model" = "qwen3.8-27b",
               "search.tool" = "searxng",
               "search.engine.web.searxng.default_params.instance_url" = "http://127.0.0.1:8888",
               "search.engine.web.searxng.default_params.max_results" = "5" }
  ```
  `[[providers]] adapter = "local-deep-research"` の行では `model`（= `llm.model` を上書き）と `env` と `settings`（キー単位で重ねる）を上書きできる。
  `settings` の値は文字列で書き、数値・真偽値・**JSON の配列**（`"[\"bing\"]"` のような文字列）はスクリプト側で変換する（TOML の型を混ぜない）。
  配列が要るのは `search.engine.web.searxng.default_params.engines` のような設定（下の D3 参照）。

### D2. `web-research` 分野（manifest 付き。ADR-0028 D1）

```toml
[[genres]]
id = "web-research"
description = "Web・OSS・製品・仕様・技術報告の調査（論文以外）"
capabilities = ["Web 検索（SearXNG）", "OSS 実装・製品・仕様の確認", "複数ソースの突き合わせ"]
input_artifacts = ["question"]
output_artifacts = ["report.md"]
default_role = "web-scout"
roles = ["web-scout"]

[[roles]]
id = "web-scout"
adapter = "local-deep-research"
tier = "cheap"
instructions = "あなたは Web 調査担当。問いに対して、出典 URL 付きで分かったことと分からなかったことを書く。"
```

`related-research`（論文）と `web-research`（それ以外）を**分けたまま**にする。親（Planner / lead）は ADR-0028 の
manifest を見て、論文の確認は `related-research`、実装・製品の確認は `web-research` に投げる。

### D3. 外に出る通信の扱い

- 検索は SearXNG（このホストのローカル）経由。LDR は取得した Web ページ本文も読む（`include_full_content`）ので、**実行時は外部ネットワークに出る**。
- **テストは外に出ない**（CLAUDE.md）。アダプタのテストはスタブの python スクリプトで行う。実機確認だけが実際に検索する。
- `search.engine.web.searxng.default_params.instance_url` を必ず設定する（既定の `localhost:8080` はこのホストでは別のもの = llama-server）。
- **このホストの実測（2026-09-17）**: 一般の Web 検索エンジンは軒並みこのホストからは使えない
  （`mojeek` は 403、SearXNG 経由でも `duckduckgo` / `brave` / `qwant` は CAPTCHA・レート制限・拒否、`google` と `wikipedia` は 0 件）。
  `engines = ["bing"]` を指定した SearXNG は 10 件返すが、**中身は問い合わせと無関係**だった（`GekkoFS` で別企業のポータルが並ぶ）。
  つまり **このホストには今のところ実用的な一般 Web 検索が無い**。使えるのは個別の API（`wikipedia` / `arxiv` / `github` / `stackexchange` / `openalex` 等）。
- **LDR はローカル（プライベート IP）の SearXNG を既定で使わない**。`LDR_SEARCH_ALLOW_PRIVATE_ENGINE_URLS=true`（または
  `LDR_SEARCH_PRIVATE_ENGINE_URL_ALLOWLIST` / 環境変数での URL 固定）が要る。アダプタの `env` で渡す。
- 経路の確認（実機）: `search.tool = "wikipedia"`、ローカル Qwen で `quick_summary` が **出典 3 件・1797 文字の要約**を返した。
  したがってアダプタが包む経路は動く。**検索の質はこのホストの環境の問題**で、次のどれかで解決する:
  (a) SearXNG 側で使えるエンジンを増やす（人間の運用判断）、(b) 鍵のある API（Brave / Tavily / Serper）を設定に足す、
  (c) 分野の目的に合わせて `github` / `stackexchange` / `openalex` 等の個別エンジンを指定する。
  既定の例の設定は **`search.tool = "wikipedia"`（鍵なしで必ず動く）**にし、コメントで (a)(b)(c) を案内する。

## 2.5 訂正（2026-09-17 夕）

**§1 の「このホストからは一般 Web 検索がほぼ使えない」という実測（mojeek 403 / DuckDuckGo CAPTCHA /
brave レート制限 / google・wikipedia 0 件）は、少なくとも 0 件になった分については誤診だった。**

真因は LDR の DNS ピン留め（`security/dns_pinning.py`、`_RESOLVE_TIMEOUT_SECONDS = 5`）と、このホストの
DNS の相性。glibc が A と AAAA を 1 つの UDP ソケットで並行送信すると `192.168.1.1` が片方を落とし、
**プロセスで最初の名前解決だけが必ず 5.01 秒**かかる。LDR は 5 秒で fail-closed して
`ConnectionError: DNS resolution failed while pinning host ...` を投げ、各検索エンジンがその例外を握りつぶして
`[]` を返すため、利用者からは「0 件」にしか見えない。LDR は run ごとに新しいプロセスなので毎回これに当たる。

対処はアダプタの `env` に `RES_OPTIONS = "single-request"`（glibc に順次問い合わせをさせる。root 不要）。
詳細と実測値は `docs/PROGRESS.md` の Phase 21 と ADR-0031 §4-4。

## 2.6 訂正2（本番の不具合から。2026-09-18。実機で確かめた事実）

**`[adapters.local_deep_research] mode = "detailed"` にすると、`quick` と同じ `settings` で常に
`Ollama model not configured. Please set llm.model in settings` という `ValueError` を出して失敗していた
（本番、2026-09-18。exit 1）。** 実機（LDR 1.10.7、`~/taskd/ldr/.venv`）で
`local_deep_research.api.detailed_research`/`quick_summary` の実装を読み、`detailed_research` を直接
呼んで確かめた:

- `quick_summary(query, research_id=None, retrievers=None, llms=None, username=None, provider=None,
  api_key=None, temperature=None, max_search_results=None, settings=None, settings_override=None,
  search_original_query=True, **kwargs)` — `settings_override` を明示の引数として受け取り、内部で
  `create_settings_snapshot(base_settings=settings, overrides=settings_override, ...)` を呼んで
  `kwargs["settings_snapshot"]` を作る。`generate_report` も同じ形（`settings`/`settings_override`
  を明示の引数に持つ）で、この不具合は無い。
- **`detailed_research(query, research_id=None, retrievers=None, llms=None, username=None,
  **kwargs)` には `settings_override` はおろか `settings` という引数すら無い。** 渡した
  `settings_override=...` は素通しの `**kwargs` に落ち、`detailed_research` 自身のコードは
  `"settings_snapshot" not in kwargs` のときだけ `create_settings_snapshot()`（**引数無し** =
  overrides 無し）を呼んで既定値・環境変数だけを使う。その後 `_init_search_system(..., **kwargs)`
  に渡る `settings_override` はどこにも消費されず単に無視される。結果、`llm.model` は既定のまま
  （Ollama 前提）で、対象の Qwen エンドポイントは設定されず上記のエラーになる。
- 実機で確かめた正しい呼び方: **`settings_snapshot=create_settings_snapshot(overrides=settings)` を
  `detailed_research` に渡す**（`settings` は `quick`/`report` と同じ、TOML の文字列値を
  `convert_setting_value` で変換したただの `dict`）。ローカルの vLLM Qwen（トンネル越し
  `127.0.0.1:18000/v1`）と Wikipedia 検索で 1 回通した:
  ```
  $ ~/taskd/ldr/.venv/bin/python - <<'EOF'
  from local_deep_research.api import detailed_research, create_settings_snapshot
  settings = {"llm.provider": "openai_endpoint",
              "llm.openai_endpoint.url": "http://127.0.0.1:18000/v1",
              "llm.openai_endpoint.api_key": "unused", "llm.model": "qwen3.8-27b",
              "search.tool": "wikipedia"}
  result = detailed_research("What is the capital of France?",
                              settings_snapshot=create_settings_snapshot(overrides=settings),
                              iterations=1, questions_per_iteration=1)
  print(list(result.keys()))
  EOF
  OK, keys: ['query', 'research_id', 'summary', 'findings', 'iterations', 'questions', 'formatted_findings', 'sources', 'metadata']
  ```
  （同じ `settings` を旧来どおり `settings_override=settings` として渡すと、上と同じ
  "Ollama model not configured" で `ValueError` になることも実機で再現確認済み。）
- ランナー（`crates/task-worker/src/local_deep_research_run.py`）の `detailed` 分岐だけをこの呼び方に
  直した。`quick`/`report` は元のまま（`settings_override` で正しく動く）。

## 3. 採らない

- `ldr-web`（Flask の画面）や `ldr-mcp`（MCP サーバ）を常駐させて HTTP / MCP で叩く
  （常駐プロセスの管理が増える。1 run = 1 プロセスの既存の形に合わせる）。
- GPT Researcher を同時に入れる（まず 1 本。A/B したくなったら `[[providers]]` を足すだけで済む形にしてある）。
- LDR の「ローカル文書コレクション」機能（`related-research` の PaperQA2 と役割が重なる）。

## 4. 受け入れ条件（Phase 19）

1. スタブの python スクリプトで、`progress:` 行が `progress` に写り、`TASKD_RESULT` から `Done` が合成され、
   `artifacts/report.md` が成果物として申告される（オフライン）。異常系（0 以外の終了・`TASKD_RESULT` 無し・タイムアウト）も。
2. `[adapters.local_deep_research]` と `[[providers]]` の行ごとの上書き（`model` / `env` / `settings`）が効く。
3. `web-research` 分野の manifest が `GET /config` と委譲・Plan のプロンプトに出る。
4. 実機: SearXNG + トンネル越しの Qwen3.8-27B で、`--genre web-research` のタスクがデーモン経由で `done` になり、
   `artifacts/report.md` に出典付きの調査結果が入る。
5. `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / GUI の検査一式。
6. 提案 P-66（DESIGN §5.4 のアダプタ表に `local-deep-research`）を PROGRESS に書く。
