# ADR-0035: 研究文献調査は、まず論文を集める（arXiv / OpenAlex からの取得と、literature 版の証拠ゲート）

- 日付: 2026-09-18
- 状態: **Accepted**（人間の決定。実機で「関連研究調査を LDR にやらせたら、レビュアー（Claude）が
  『出典 15 件が Medium / Qiita / note の非学術ブログ』で不合格にした」ことを受けたもの）
- 関連: ADR-0027 D3（`paperqa` アダプタ。「文献の取得は鍵無しで始める」）、ADR-0029（LDR = Web 調査）、
  ADR-0031（証拠ゲートと検索の記録。LDR 側の先例）、ADR-0033 D2（案件 = `project_id`）、
  ADR-0006 D3（結果ファイル規約）、DESIGN §5.4 / SPEC §2.2

## 1. 文脈

人間の決定: **課を分ける**。

| 課 | 分野（genre） | ハーネス | 何を調べるか |
|---|---|---|---|
| 研究文献調査課 | `literature` | PaperQA2（`paperqa`） | 査読済み・プレプリントの**論文** |
| Web 調査課 | `web-research` | Local Deep Research（`local-deep-research`） | 論文化されていない GitHub 上の実装、一般 Web |

ところが今の `paperqa` アダプタ（ADR-0027 D3）は `paper_directory`（手元の PDF）を読んで答えるだけで、
**論文を探して取ってくる段が無い**。本番の `paper_directory` には Phase 17 の動作確認で置いた 3 本の
テキストしか入っていない。この状態で「関連研究調査」を投げると、**手元の 3 本の中だけで答える**ことになり、
関連研究調査になっていない。ADR-0031 が LDR に対して直した「0 件なのに done」と同じ種類の問題である
（証拠の量をハーネスが決定的に見ていない）。

### 実機で確かめた事実（2026-09-18、このホストから。鍵無し）

- `https://export.arxiv.org/api/query?search_query=...&max_results=N&sortBy=relevance` → **HTTP 200**。
  `search_query=all:"ad-hoc file system"`（引用符つき）は `totalResults 0` を返すが、
  `all:ad+hoc+file+system`（引用符なし）は 3 件返る。**引用符で括らない**こと。
  各 `<entry>` は `<id>http://arxiv.org/abs/1003.3565v1`、`<title>`、`<published>`、
  `<link rel="related" title="pdf" href="https://arxiv.org/pdf/1003.3565v1">`、著者、
  任意の `<arxiv:doi>` / `<arxiv:journal_ref>` を持つ。
- `https://api.openalex.org/works?search=...&per_page=3&filter=is_oa:true&mailto=<メール>` → **HTTP 200**、
  鍵不要（`mailto` は polite pool 用）。1 件の中身は `id` / `doi` / `publication_year` / `title` /
  `open_access.oa_url` / `best_oa_location.pdf_url` / `primary_location.pdf_url` /
  `primary_location.source.display_name`（venue）/ `authorships[].author.display_name`。
  検索語「asynchronous I/O runtime ad hoc file system」で 3622 件、1 件目から
  `Ad Hoc File Systems for High-Performance Computing (2020)` のような**学術論文**が返る。
- PDF は鍵無しで落ちた: `https://arxiv.org/pdf/1003.3565v1` → 200 / 798,684 B / `application/pdf`、
  OpenAlex の `best_oa_location.pdf_url`（`https://upcommons.upc.edu/.../AdHocFileSystems.pdf`）→ 200 /
  7,943,385 B / `application/pdf`。Python の `urllib.request`（標準ライブラリのみ、UA を明示）でも同じ。
- 名前解決は `AF_UNSPEC` cold で 0.02 秒（ADR-0031 で見つかった 5 秒の症状は今回は出ていない。
  出たら同 ADR どおり `RES_OPTIONS = "single-request"` を `env` に入れる）。
- `~/taskd/paperqa/.venv/bin/pqa`（paper-qa 2026.8.12、ADR-0027 の実機確認どおり）と
  `~/taskd/paperqa/settings/qwen-local.json`（`agent_type = "fake"` / `embedding = "sparse"` /
  LiteLLM の `timeout = 900`）は健在で、LLM は `http://127.0.0.1:18000/v1` の Qwen3.8-27B
  （`/v1/models` が 200）。`pqa` は PDF ディレクトリを渡せば索引を作れる（Phase 18 で確認済み）。

## 2. 決定

### D1. 取得はアダプタが埋め込む Python ランナーでやる（LDR と同じ作り）

`crates/task-worker/src/paperqa_acquire.py` を `include_str!` で taskd のバイナリに埋め込み、run ごとに
`runs/<run_id>/paperqa_acquire.py` として書き出して `<python> <その場所> <run_dir>/acquire_input.json` で
起動する（ADR-0029 D1 の LDR ランナーと同じ形。taskd の外に置くのは venv だけ）。
標準ライブラリ（`urllib` / `json` / `xml`）しか使わない。

出力は 1 行 1 メッセージ: `progress: <text>` と、最後に 1 行だけ
`TASKD_ACQUIRE {"candidates": n, "pdfs": m, "engines": {"arxiv": a, "openalex": b}}`。

1. **問いから検索語を作るのはアダプタ（Rust、決定的。LLM は使わない）**。`task.objective` から
   **ASCII の名詞句**（英字で始まる語の連なり。`ad-hoc FS` / `asynchronous I/O runtime` のように
   日本語や句読点で区切られる）を抜き、語数の多い順・出現順で最大 4 本を選ぶ。1 本も取れなければ
   `objective` 全文を 1 本の検索語にする。検索語はランナーの入力 JSON に渡す。
2. ランナーは arXiv（`export.arxiv.org/api/query`、`max_results` 20/語、`sortBy=relevance`、
   **引用符で括らない**）と OpenAlex（`api.openalex.org/works?search=&per_page=20&filter=is_oa:true`、
   `mailto` があれば付ける）を叩き、候補を **DOI / arXiv id / タイトル正規化**で重複排除する。
   並べ替えは「各 (検索語, エンジン) の結果リストを順位でラウンドロビン」= relevance 順の総当たりで、
   `max_candidates`（既定 30）まで。
3. open access の PDF だけ `max_pdfs`（既定 12）本まで落とす（arXiv は `https://arxiv.org/pdf/<id>`、
   OpenAlex は `best_oa_location.pdf_url` → `primary_location.pdf_url` → `open_access.oa_url`）。
   **案件ごとの corpus**: `paper_directory/<project_id>/`（案件が無ければ `paper_directory/_shared/`）。
   **既にあるファイルは再取得しない**。先頭が `%PDF` でない応答は捨てる（HTML のログインページ等）。
4. `artifacts/papers.json`（全候補: `title` / `authors` / `year` / `venue` / `doi` / `arxiv_id` /
   `url` / `pdf_url` / `file` / `pdf_downloaded` / `source_engine`）と
   `artifacts/sources.json`（LDR と同じ形: `url` / `title` / `engine` / `cited`。ランナーの時点では
   `cited` は全て `false`）を書く。

### D2. 順序: 取得 → 索引 → 回答

`paperqa` の 1 run は 2 段になる。

1. 取得ランナー（D1）。
2. `pqa` に**その案件の** `paper_directory` / `index_directory` / `index name` を渡して回答（既存の実装）。
   索引も案件ごと（`index_directory/<project_id>/`、名前も `<project_id>`）にする。
   **ADR-0027 D3 の「索引はタスクごと」からの変更**（U17-2: タスクごとだと索引を毎回作り直す。
   corpus が案件ごとになったので、索引も案件ごとにすれば同じ案件の別タスク・リトライで使い回せる）。

取得が 0 本でも `pqa` は走らせる（既存 corpus があるかもしれない）。取得ランナーが失敗しても
run はそこで止めず、`progress` に残して `pqa` に進む（判定は D3 のゲートが行う）。

`cited` は `pqa` の答えが出てから**アダプタが決定的に**決める（答えに DOI / arXiv id / PDF のファイル名 /
`著者姓+年` / 正規化したタイトルのどれかが現れるか）。突き合わせに失敗すれば `false`。決めた結果で
`artifacts/sources.json` を書き直す。

### D3. 証拠ゲート（ADR-0031 の literature 版）

```toml
[adapters.paperqa.evidence]
min_candidates = 5   # 検索が返した候補論文の数
min_pdfs = 3         # corpus に入った PDF の数（既にあったものを含む）
min_cited = 2        # 答えが引用した出典の数
```

- `0` を書けばその項目は見ない（全部 0 ならゲート無し）。
- 満たさなければ `Terminal::Error { message, retryable: true }`。**`AdapterError` にはしない**
  （プロバイダを cooldown にする話ではない。ADR-0031 D2 と同じ）。
- **成果物（`answer.md` / `papers.json` / `sources.json`）は消さずに残す**。
- **取得が 0 件（`candidates == 0`）のときは別メッセージ**:
  `"literature search returned nothing (possible network or API problem)"`。
  「論文が見つからなかった」と「検索経路が壊れている」を運用者が区別できるようにする（ADR-0031 D2 と同じ理由）。
- 取得そのものを止める構成（`[adapters.paperqa.acquire] max_candidates = 0`）ではゲートも見ない
  （手元の corpus だけで動かす従来の使い方を壊さないため）。

### D4. 成果物の形

`artifacts/answer.md` は従来どおり `pqa` の答え本文で、その末尾に `## 出典` を足す。
`sources.json` の `cited` が真のものを先に、次にそれ以外を、`[n] 著者 (年). タイトル. venue. URL` で並べる
（欠けている項目は飛ばす。引用されたものには行末に `(引用)` を付け、人が「答えの根拠」と「見つかったが
読まれていない候補」を見分けられるようにする）。人が「見るべき関連研究へのリンク」をそのまま読める形にする（SPEC §2.2）。
`## 出典` は取得の段を行った run だけに付く（`max_candidates = 0` の構成では従来どおり答え本文だけ）。

### D5. 検索語は LLM が立てる（Phase 36 で追記。実機の失敗から）

- 日付: 2026-09-18（Phase 34 の実機の run を見た人間の決定）
- これは §3 の 1 行目「LLM に検索語を作らせる（今回は採らない）」の**撤回**である。

#### 何が起きたか（本番、2026-09-18。run `01M2SES1R5XQBP3C3M3986K908`）

D1 手順 1 の決定的な抽出は、実依頼「学術動向調査: Pluvio 隣接領域の候補テーマ抽出 …」（日本語）から
`ad-hoc` / `runtime` / `I/O` / `Pluvio` のような**文脈を失った語**を出した。arXiv / OpenAlex はその語に
忠実に「モバイル ad hoc ネットワーク」「pluvio-thermal な地形学」「Cilk ランタイム」「PyTorch」を返し、
30 候補のうち隣接領域は 0〜1 件、PDF 6 本はすべて無関係だった。PaperQA2 は正しく
"I cannot answer this question due to insufficient information" と答え、D3 の証拠ゲートが `cited=0` で
落とした。**ゲートは正しく働いた。間違っていたのは検索語である。**

原因は 3 つあり、どれも決定的な抽出では直せない。

1. 依頼文の語（`ad-hoc` / `runtime`）は**分野の文脈の中でだけ**意味が決まる。文脈は日本語の側にある。
2. 固有名詞（`Pluvio`）は学術索引に存在しない。単独で引くと同じ綴りの別分野（降雨計）に当たる。
3. arXiv の `all:a b c` は API では **`all:a OR all:b OR all:c`** として扱われる（実機で確認。
   echo された `search_query` がそうなっていた）。語を並べるほど無関係な論文が増えていた。

#### 決定

1. **検索語は LLM が立てる**（取得ランナーの最初の段）。**PaperQA2 と同じ LLM 先**
   （settings の `llm`。`acquire.query_model` → `[[providers]] model` → settings の `llm` の順に決め、
   LiteLLM の `provider/` 接頭辞は落とす）に、OpenAI 互換の `chat/completions` を
   `OPENAI_BASE_URL` へ**直接 1 回**叩く（`pqa` は使わない。別の run も立てない）。
   入力はタスクの `title` / `objective` と、あればこの案件についての記憶
   （`context.memory.project`。`RunContext` に案件の依頼文そのものは無いため、案件単位で人が与えた
   文脈として最も近いものを渡す）。出力は JSON 固定:

   ```json
   {"queries": [{"text": "ad hoc file system HPC", "engines": ["arxiv", "openalex"],
                 "arxiv_categories": ["cs.DC", "cs.OS"]}],
    "exclude_terms": ["mobile ad hoc network", "language runtime"]}
   ```

   3〜6 本。プロンプトには「英語で書く」「学術検索向けの短い句（3〜6 語）」「固有名詞（`Pluvio` 等）は
   単独で使わず説明語と組む」「曖昧語（`ad hoc` / `runtime`）には必ず分野語を添える」「検索エンジンが
   間違って返してくる近い同名語を `exclude_terms` に挙げる」を書く。
2. **JSON が壊れていたら D1 手順 1 の決定的な抽出に落ちる**（そのことを `progress:` に出し、
   `artifacts/queries.json` の `error` に理由を残す）。`acquire.query_llm = false`（既定 `true`）で
   常に決定的な抽出だけを使える。LLM を呼ぶ先が分からない構成（settings も `--llm` も無い）でも
   落ちる。**判定は相変わらず D3 のゲートが決定的に行う**（LLM に「足りているか」を聞かない）。
3. **arXiv は語を `AND` で綴じ、`cat:` で分野を絞る**:
   `all:ad AND all:hoc AND all:file AND all:system AND (cat:cs.DC OR cat:cs.OS)`。
   カテゴリは LLM が出したもの、無ければ既定 `cs.DC OR cs.OS OR cs.PF OR cs.NI`。
   `AND` で 0 件だったときだけ、同じ語を `OR` で 1 回だけ引き直す（カテゴリの縛りは残す。
   実機で長い `AND` は 0 件になりやすい）。機能語（`for` / `the` / `of` …）は `AND` から落とす。
   **OpenAlex は `filter=is_oa:true,primary_topic.field.id:17`**（open access かつ Computer Science。
   `GET https://api.openalex.org/fields` で **fields/17 = Computer Science** を実機で確認、2026-09-18）。
   計算機科学以外で使うときは `acquire.openalex_filter` で差し替える。
4. **除外語で候補を落とす**（決定的）。候補の**タイトル + 要旨**に `exclude_terms` のどれかが
   含まれていれば捨てる。要旨は arXiv の `<summary>`、OpenAlex の `abstract_inverted_index` を
   組み直したもの（先頭 600 字）で、`papers.json` にも残す。
5. **記録**: `papers.json` の各候補に `query_text`（どの検索語が連れてきたか。エンジンは従来の
   `source_engine`）を足し、`artifacts/queries.json`（LLM の出力そのまま + `generated_by`:
   `"llm"` / `"fallback"` + 落ちた理由）を書いて成果物として申告する。

#### なぜ「別の run を立てる」案を採らなかったか

§3 が重いと言ったのは「`[[genres]] literature` の run に `queries.json` を書かせる」案で、run が
2 本になり、ディスパッチャ・予算・ゲートが絡む。ここで足したのは**取得ランナーの中の 1 回の
`chat/completions`** であり、run は 1 本のまま、失敗しても決定的な抽出に落ちるだけ
（実機で 5 秒、Qwen3 は `chat_template_kwargs.enable_thinking = false` を付けないと考えるだけで
`max_tokens` を使い切って `content` が空になる。付けない口には 1 回だけ付け直さずに再送する）。
DESIGN 原則 1（ディスパッチャとストアに LLM を入れない）は守られている: LLM を呼ぶのは
ハーネス（アダプタが起動するランナー）だけで、判定は決定的なゲートのままである。

## 3. 採らない（今回は）

- ~~**LLM に検索語を作らせる**。依頼文が日本語のとき、英語の検索語を LLM に書かせれば質は上がるが、
  そのための run を別に立てると重い（`[[genres]] literature` の run のプロンプトで
  `artifacts/queries.json` を書かせる案）。まず決定的な抽出で回し、実機で足りなければ別 ADR で足す。~~
  → **Phase 36 で撤回**（D5）。実機で決定的な抽出が無関係な論文しか連れて来なかったため、
  取得ランナーの中で LLM が検索語を立てるようにした（別の run は立てない）。
- Semantic Scholar / Crossref を叩く（ADR-0027 の「鍵無しで始める」を保つ。Semantic Scholar は
  鍵無しだと 429）。arXiv と OpenAlex で足りるかを先に実機で見る。
- 引用グラフ（OpenAlex の `referenced_works` / `cited_by`）をたどる。まず 1 段の検索だけ。
- 取得した PDF の本文で重複排除する（タイトル正規化と DOI / arXiv id で十分かを先に見る）。
- `paperqa` に委譲（`delegate.json`）を扱わせる（ADR-0027 D3 のまま）。

## 4. 受け入れ条件（Phase 34）

1. 取得ランナーが、**本物の API を叩かずに**（`--fixture <dir>`）検索語ごとの応答を読み、重複排除・
   `max_candidates` / `max_pdfs` の上限・案件ごとの corpus・既存ファイルの再取得なしを満たし、
   `papers.json` / `sources.json` を規定の形で書く。
2. アダプタが 2 段（取得 → `pqa`）の順で起動し、ゲートの 3 パターン（通る / 落ちる / 取得 0 件の別メッセージ）
   になり、成果物を申告し、`answer.md` の末尾に `## 出典` が付く。
3. 既存の `paperqa` のテスト（Phase 17〜18）が通る（索引のパスが案件ごとになった分だけ期待値を更新する）。
4. 実機: 本番と同じ `~/taskd/paperqa` の venv と settings、トンネル越しの Qwen で、Pluvio の隣接領域を
   問う `literature` のタスクを `taskctl worker run` で 1 回通し、`papers.json` の件数・PDF 本数・
   `answer.md` の出典が**学術論文**になっていることを `docs/PROGRESS.md` に記録する。
5. `cargo test --workspace`（FAILED 0）/ `cargo clippy --workspace --all-targets -- -D warnings` exit 0。

## 5. 受け入れ条件（Phase 36 / D5）

1. ランナーが、**本物の API も LLM も叩かずに**（`--fixture <dir>` の `llm-1.json`）LLM の検索語 JSON を
   読み、`cat:` と OpenAlex の `filter` を付け、除外語で候補を落とし、`queries.json` /
   `papers.json`（`query_text` 付き）を規定の形で書く。壊れた JSON では決定的な抽出に落ちて、
   その理由を `progress:` と `queries.json` に残す。
2. アダプタが LLM の段の設定（PaperQA2 と同じ LLM 先・依頼文）を取得ランナーの入力に載せ、
   `queries.json` を成果物として申告する。既存の `paperqa` のテスト（Phase 17〜18、34）が通る。
3. 実機: 上の失敗した run と同じ `objective` を、本番と同じ venv / settings / トンネルの Qwen で
   `taskctl worker run` に 1 回通し、`queries.json` の検索語・`papers.json` のうち隣接領域の件数・
   PDF 本数・`cited`・`answer.md` が "cannot answer" でないことを `docs/PROGRESS.md` に記録する。

### D6. `candidates.json` → `papers.json`（Phase 38 で改名。ADR-0028 の追記と対）

- 日付: 2026-09-18（人間の決定）
- 実機で、秘書の計画が研究文献調査課（`literature`）に「候補テーマを 3〜5 件 **`candidates.json` に
  まとめよ**」という objective と `artifact_exists: candidates.json` を付けた。`candidates.json` は
  D1 手順 4 の**検索コーパスの固定名**（title/authors/doi/abstract/pdf_downloaded）で、ワーカー（pqa）は
  計画が指定したファイルを書けない。答えの中身（Zhu2025 / Maurya2025 / Saeik2021 を引いた候補 3 件）は
  良かったのに、レビュアーは基準どおり「`candidates.json` に候補テーマが無い」と不合格にした。
- **「候補」という語がテーマ候補と紛れる**ため、この分野の成果物は
  `answer.md` / `papers.json` / `queries.json` / `sources.json` に改めた（中身の形は変えていない。
  ランナーの入力 JSON の鍵も `candidates_path` → `papers_path`）。
- 計画が名前を勝手に決められないようにする側の対策（分野の manifest に成果物の説明を持たせ、計画・
  レビュアーのプロンプトに「ハーネスで動く分野の成果物の名前は固定」を出し、計画の後に決定的に直す）は
  **ADR-0028 の「Phase 38 追記」**に書いた。
