# ADR-0031: 調査の「証拠の量」をハーネスが判定する（決定的ゲートと検索の記録）、検索は鍵付き API を主経路にする

- 日付: 2026-09-17
- 状態: **Accepted**（人間が受けたレビューの指摘。特に「**0 件なのに done になるのを直すこと**が検索エンジン選び以上に重要」）
- 関連: ADR-0029（`web-research` と LDR）、ADR-0030（API キーを GUI から預かる）、ADR-0027 D4（受け入れ条件の判定は taskd）、ADR-0010 D5（供給側の失敗）

## 1. 文脈

ADR-0029 の実機確認で、次の経路が通ってしまうことが分かった。

```
検索が失敗（0 件）
  → LLM が一般知識でそれらしい report.md を書く
  → artifact_exists report.md
  → done
```

レビューの指摘のとおり、**「検索できなかった」と「調べた結果その情報が無かった」は全く違う**。前者は供給側の失敗で、
やり直すか人に返すべきもの。後者は調査の結果で、報告してよいもの。**この区別を LLM に委ねてはいけない**。

検索経路そのものも、このホストからは無料の一般 Web 検索が実用にならない（ADR-0029 の実測）。レビューの結論は
「SearXNG を一般 Web 検索の主経路にするのは諦め、Tavily を既定、Exa を意味検索、GitHub / OpenAlex / arXiv を専門検索、
SearXNG は安定するサイト固有エンジンに限定」。鍵は ADR-0030 の仕組みで GUI から預かる。

### 実機で確かめた事実（2026-09-17）

- LDR の `quick_summary` / `detailed_research` の戻り値は `{summary, findings, sources, questions, iterations, formatted_findings, research_id}`。
  `sources` は集めたリンクの一覧（`link` / `title` / 本文などを持つ辞書）で、**件数・ドメイン・引用の集計はここから作れる**。
- LDR の設定は `LDR_` + 設定キーの大文字化で環境変数から渡せる（Tavily = `LDR_SEARCH_ENGINE_WEB_TAVILY_API_KEY`、Exa = `..._EXA_API_KEY`）。

## 2. 決定

### D1. 調査 run は「証拠の記録」を必ず残す

`artifacts/` に 3 つ書く（`report.md` は従来どおり）:

| ファイル | 中身 |
|---|---|
| `report.md` | 人が読む報告（要約・所見・出典） |
| `sources.json` | `[{url, title, engine?, cited: bool}]`（重複は URL で 1 件） |
| `research.json` | `{queries: [{query, engine?, result_count}], iterations, counts: {queries, search_results, sources, sources_cited, unique_domains}}` |

`sources.json` / `research.json` は**ランナー（Python）が LDR の戻り値から機械的に作る**。LLM に書かせない。
`cited` は要約中の `[n]` の参照から決める（LDR は出典を番号で引く）。

### D2. 決定的な証拠ゲート（ハーネス側。LLM に判断させない）

```toml
[adapters.local_deep_research.evidence]
min_search_results = 5    # 検索が返した件数の合計
min_sources = 3           # 実際に証拠として集まった出典の数
min_cited = 2             # 報告が引用した出典の数
min_domains = 2           # 出典の異なるドメイン数
```

- 既定は上の値。`0` を書けばその項目は見ない（全部 0 なら従来どおりの挙動）。
- 満たさなければ `Terminal::Error { message: "insufficient web evidence: …（実際の数と閾値）", retryable: true }` にする。
  **`report.md` は消さずに残す**（何が起きたかを人が読めるように）。`research.json` も残す。
- 検索が 1 件も返らなかった（`search_results == 0`）ときは、メッセージを分けて
  `"web search returned nothing（検索経路の問題の可能性）"` とする。運用者が鍵切れ・CAPTCHA・ネットワーク遮断を区別できるようにする。
- retryable なので、ディスパッチャは従来どおり attempts を消費して再試行し、上限に達したら `failed` → 受信箱に出る。
  **供給側の失敗（`AdapterError`）にはしない**（プロバイダを cooldown にする話ではないため）。

### D3. 受け入れ条件は二段構え（運用の指針）

1. ハーネスの決定的ゲート（D2）＝「証拠が足りているか」
2. `Check::Reviewer`＝「問いに答えているか」

`web-research` 分野の例の設定とドキュメントに「`--check-artifact report.md` だけにしない。`--check-reviewer` を併用する」と書く。

### D4. 検索経路（設定だけで切り替える。コードは変えない）

- 既定を **Tavily**（一般 Web）にし、**Exa**（意味検索）を併用できるようにする。鍵は ADR-0030 の `env_from_secrets` で渡す。
- **SearXNG は一般 Web 検索の主経路にしない**。使うならサイト固有で安定するエンジンだけに絞る（`engines = [...]`）。
- GitHub / OpenAlex / arXiv は専門検索として併用できるようにする（LDR 側の設定）。
- 例の設定に「鍵が無いときの既定」（`wikipedia`）も残し、コメントで乗り換え方を書く。

## 3. 採らない

- LDR の LangGraph エージェントによる query routing（レビューの「面白い」案）を今回入れる。まず証拠ゲートと記録を入れ、
  実際に Tavily / Exa を回してから、必要なら別 ADR で検討する（ハーネス側の挙動が変わるため、先に測りたい）。
- ゲートの判定を LLM に委ねる（D2 の理由）。
- 証拠が足りないときに `Question`（人への質問）にする。まずは retryable なエラーで、再試行と受信箱の既存の流れに乗せる。

## 4. 受け入れ条件（Phase 21）

1. スタブのランナーで `research.json` / `sources.json` / `report.md` の 3 つが書かれ、成果物として申告される。
2. 閾値を満たさない出力（0 件・出典 1 件・同一ドメインのみ）で `Error{retryable: true}` になり、メッセージに実数と閾値が入る。
   `search_results == 0` のときは別のメッセージになる。`report.md` は残る。
3. 閾値を全部 0 にすると従来どおり `done` になる。
4. 実機（鍵が届いたら）: Tavily を既定にした `web-research` のタスクが `done` になり、`research.json` の件数がゲートを満たす。
   → **2026-09-17 に充足**。落ちる側（`search_results: 0` で `retryable` な Error）と通る側
   （`search_results: 19 / sources: 18 / cited: 16 / domains: 16` で `done`）の両方を実機で確認した。

   併せて判明: このホストで**どの検索エンジンでも 0 件になっていた真因は DNS** だった。glibc の A/AAAA 並行送信を
   ルータの DNS が取りこぼしてプロセス最初の名前解決が 5.01 秒かかり、LDR の DNS ピン留め
   （`_RESOLVE_TIMEOUT_SECONDS = 5`）が fail-closed して例外を投げ、検索エンジンがそれを握りつぶしていた。
   アダプタの `env` に `RES_OPTIONS = "single-request"` を入れると直る。**§1 と ADR-0029 の「無料の一般 Web 検索が
   使えない」という実測は、この前提で読み直す必要がある**（少なくとも 0 件系はこれが原因の可能性が高い）。
   この切り分けができたのは D2 のゲートが「0 件なのに done」を止めていたからである。
5. `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / GUI の検査一式。
