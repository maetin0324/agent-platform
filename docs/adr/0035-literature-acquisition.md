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
4. `artifacts/candidates.json`（全候補: `title` / `authors` / `year` / `venue` / `doi` / `arxiv_id` /
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
- **成果物（`answer.md` / `candidates.json` / `sources.json`）は消さずに残す**。
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

## 3. 採らない（今回は）

- **LLM に検索語を作らせる**。依頼文が日本語のとき、英語の検索語を LLM に書かせれば質は上がるが、
  そのための run を別に立てると重い（`[[genres]] literature` の run のプロンプトで
  `artifacts/queries.json` を書かせる案）。まず決定的な抽出で回し、実機で足りなければ別 ADR で足す。
- Semantic Scholar / Crossref を叩く（ADR-0027 の「鍵無しで始める」を保つ。Semantic Scholar は
  鍵無しだと 429）。arXiv と OpenAlex で足りるかを先に実機で見る。
- 引用グラフ（OpenAlex の `referenced_works` / `cited_by`）をたどる。まず 1 段の検索だけ。
- 取得した PDF の本文で重複排除する（タイトル正規化と DOI / arXiv id で十分かを先に見る）。
- `paperqa` に委譲（`delegate.json`）を扱わせる（ADR-0027 D3 のまま）。

## 4. 受け入れ条件（Phase 34）

1. 取得ランナーが、**本物の API を叩かずに**（`--fixture <dir>`）検索語ごとの応答を読み、重複排除・
   `max_candidates` / `max_pdfs` の上限・案件ごとの corpus・既存ファイルの再取得なしを満たし、
   `candidates.json` / `sources.json` を規定の形で書く。
2. アダプタが 2 段（取得 → `pqa`）の順で起動し、ゲートの 3 パターン（通る / 落ちる / 取得 0 件の別メッセージ）
   になり、成果物を申告し、`answer.md` の末尾に `## 出典` が付く。
3. 既存の `paperqa` のテスト（Phase 17〜18）が通る（索引のパスが案件ごとになった分だけ期待値を更新する）。
4. 実機: 本番と同じ `~/taskd/paperqa` の venv と settings、トンネル越しの Qwen で、Pluvio の隣接領域を
   問う `literature` のタスクを `taskctl worker run` で 1 回通し、`candidates.json` の件数・PDF 本数・
   `answer.md` の出典が**学術論文**になっていることを `docs/PROGRESS.md` に記録する。
5. `cargo test --workspace`（FAILED 0）/ `cargo clippy --workspace --all-targets -- -D warnings` exit 0。
