# ADR-0027: タスクの分野（genre）でハーネスを切り替える。最初の非コーディング分野として関連研究調査（PaperQA2）

- 日付: 2026-09-17
- 状態: **Accepted**（人間の依頼「調査やその他のタスク毎にジャンルを分けて、それぞれのタスク分野に特化したハーネスを使い作業ワーカーが作業できるようにしたい」。
  人間の選択: 分野は**新しい概念として足す**、調査の LLM は**ローカル Qwen 優先**、文献の取得は**鍵無しで始める**）
- 関連: ADR-0016（役割と委譲）、ADR-0026（汎用 ACP アダプタ）、ADR-0012 D1（プロバイダごとのアダプタ）、ADR-0006 D3（結果ファイル規約）、
  DESIGN §5.4 のアダプタ表と §4 のタスク（提案 P-64 / P-65）

## 1. 文脈

今の taskd で「どのハーネスが動くか」を決めているのは `worker_hint.adapter`（= 役割の `adapter`、無ければ親から継承）だけ。つまり**切り替えの部品はあるが、分野という概念が無い**。
結果として次が足りない。

- 役割名は自由記述で検証されない。`--role literature-scout` と書き間違えても、黙って親のアダプタで走る。
- 委譲する親（lead）に「**どんな専門家（役割・分野）に任せられるか**」を伝える手段が無い。プロンプトにも出ていないので、親は自分と同じハーネスで全部やろうとする。
- 分野ごとの既定（どのアダプタ・どの tier・どんな指示文）をまとめて置く場所が無い。

そして調査の実行器が無い。関連研究調査は「それっぽい論文を並べる」ことではなく「**見落としている論文が無いか**」を詰める仕事で、
コーディング用ハーネスにやらせるものではない。人間の調査（本 ADR の依頼文）では PaperQA2 が第一候補、次に Local Deep Research（Web 側）という結論になっている。

### このホストで確認した事実（2026-09-17）

- `paper-qa` 2026.8.12 を `uv` の隔離環境（`~/taskd/paperqa/.venv`）に導入。CLI は `pqa {ask,search,index,view,save}`。
- `-s <名前>` は**拡張子を自分で足す**ので、設定ファイルはパスから `.json` を除いて渡す（`-s ~/taskd/paperqa/settings/qwen-local`）。
- LiteLLM 経由で OpenAI 互換の口を向ける（`llm` / `summary_llm` / `agent.agent_llm` と、それぞれの `*_config` に `api_base` と `timeout`）。
  **既定のタイムアウトは 60 秒で、ローカル Qwen（27B、ssh トンネル越し）だと 1 回の要約に 180 秒かかって落ちる**ため、設定で伸ばす必要がある。
- 埋め込みは `embedding = "sparse"` で外部の鍵が要らない（Semantic Scholar は鍵無しだと 429。Crossref は 200）。
- **`agent.agent_type` は `"fake"` を使う**。既定の `ToolSelector` はこの版の組み合わせで
  `AttributeError: 'LiteLLMModel' object has no attribute 'get_router'` で落ちる（paper-qa 2026.8.12 + 同梱の lmi）。
  `fake` は「検索 → 証拠収集 → 回答」の固定手順で、LLM にツール選択をさせない。taskd 側が木構造で段取りを決める本 ADR の方針とも合う。
- 動作確認（ローカルの .txt を 2 本置いた `paper_directory`、ローカル Qwen、`embedding = "sparse"`）: `pqa ask` が exit 0 で
  **引用付きの回答**を返した（UnifyFS と GekkoFS を根拠つきで挙げ、BurstFS は「文脈からは確認できない」と区別した）。索引作成を含めて数分。

## 2. 決定

### D1. 分野（genre）を設定の第一級の概念にする

```toml
[[genres]]
id = "coding"
description = "コードを書く・直す・テストする"
default_role = "implementer"
roles = ["lead", "implementer"]

[[genres]]
id = "literature"
description = "関連研究の調査・先行研究の確認・新規性の検討"
default_role = "literature-reader"
roles = ["literature-scout", "literature-reader", "novelty-skeptic"]
```

- タスクに `genre: Option<String>` を足す（`role` と同じ扱い。`tasks` テーブルの列 → **`SCHEMA_VERSION` を 5 に上げる migration** `0005_tasks_genre_column.sql`）。
- **決め方の順序**は既存の規則（ADR-0016 D1）に 1 段足すだけ:
  `tier` / `adapter` / 予算 = **タスクの値 > 役割の既定 > 分野の既定（`default_role` の役割） > 親の値**。
- 検証（`Config::validate` と API の作成時）: 知らない `genre` はエラー。`genre` と `role` の両方があるとき、その役割が分野の `roles` に無ければエラー。
  `[[genres]]` を 1 つも書かない構成は今までどおり動く（分野は任意）。
- プロンプト: run のプロンプトに、そのタスクの分野の `description` を入れる。**委譲できる親（`delegate` を使える run）には「使える分野と役割の一覧」を渡す**
  （`RunContext.available_genres[]` = `{id, description, roles[{id, description}]}`）。これが無いと親は他分野に任せようがない。
- `DelegateTask` に `genre: Option<String>` を足す（`role` と同じ扱い。子の分野を親が選べる）。未指定なら役割から、役割も無ければ親の分野を継ぐ。

### D2. 「分野 → ハーネス」は役割経由のまま。分野は**まとまりと入口**

分野そのものにアダプタを持たせない（`default_role` の役割が持つ）。理由: 同じ分野でも「広く探す」「深く読む」「反証する」で使う実行器やモデルが違うため
（ADR-0026 の ACP でも、役割ごとに別の ACP エージェントを指せる）。分野は「どの専門家集団に投げるか」を表し、役割が「その中の誰か」を表す。

### D3. 関連研究調査のハーネス = PaperQA2（`adapter = "paperqa"`）

- `task-worker` に `paperqa` アダプタを足す。**PaperQA2 はエージェントではなく調査エンジン**なので、ワーカープロトコル（`artifacts/result.json`）は
  **アダプタが代わりに書く**（ADR-0006 D3 の規約は保つ。ディスパッチャから見た形は他のアダプタと同じ）。
- 1 run の手順:
  1. `pqa -s <settings> --agent.index.paper_directory <papers> --agent.index.index_directory <index>/<task 用> --agent.index.name <name> ask "<問い>"` を
     ワークスペースを cwd として起動（`process_group(0)` / `kill_on_drop` / 壁時計・無出力タイムアウトは他と同じ）。
  2. 問いはタスクの `objective`（+ 役割の指示文と `context.answers`）から作る。`request.json` / `prompt.txt` は他のアダプタと同じ共有ヘルパで残す。
  3. 標準出力は `runs/<run_id>/stdout.log` に全部残しつつ、行を `progress` に写す（PaperQA2 は検索・要約の進捗を出す）。
  4. 終了後、**アダプタが** `artifacts/answer.md`（回答本文と引用）と `artifacts/result.json`（`{"summary": <回答の要点>, "evidence": [{"criterion": …}]}`）を書き、`Done` を合成する。
     出力が空・exit != 0 なら `Error{retryable}`（`classify_provider_failure` で供給側の失敗も見る）。
- 設定:
  ```toml
  [adapters.paperqa]
  command = "/home/u/taskd/paperqa/.venv/bin/pqa"
  settings = "/home/u/taskd/paperqa/settings/qwen-local"   # `.json` は付けない（実機の仕様）。設定側で agent_type = "fake"、
                                                           # embedding = "sparse"、LiteLLM の timeout を伸ばしておく
  paper_directory = "/home/u/taskd/paperqa/papers"
  index_directory = "/home/u/taskd/paperqa/index"
  env = { OPENAI_API_KEY = "unused", OPENAI_BASE_URL = "http://127.0.0.1:18000/v1" }
  ```
  `[[providers]] adapter = "paperqa"` の行ごとに `settings` / `env` / `model` を上書きできる（ADR-0026 D2 と同じ作り。`model` は PaperQA の設定より優先して `--llm` に渡す）。
- **委譲はしない**（`delegate.json` を読まない）。調査の分割は taskd 側（親の run）が行う。
- 索引: `index_directory` の下に**タスクごと**の索引を作る（同時実行で索引を壊さないため）。`paper_directory` は読み取りだけ。

**Phase 34 追記（ADR-0035。2026-09-18）**: この D3 は 2 点が上書きされた。
1. run は 2 段になった（取得 → 索引と回答）。`paper_directory` は読み取りだけではなく、取得ランナーが
   **案件ごとのサブディレクトリ**（`paper_directory/<project_id>/`、案件が無ければ `_shared`）に PDF を書く。
2. 索引は**タスクごとではなく案件ごと**（`index_directory/<project_id>/`、索引名も `<project_id>`）。
   corpus が案件ごとになったので、同じ案件の別タスク・リトライで索引を使い回せる（U17-2 の緩和）。
また、`settings` に**必ず**入れるものが 1 つ増えた: `parsing.multimodal = false`。既定
（`ON_WITH_ENRICHMENT`）は PDF の画像を取り出して**既定モデル `gpt-4o-2024-11-20`** で説明文を作るため、
ローカル Qwen のエンドポイントでは 404 になり PDF の索引作成が丸ごと落ちる（ADR-0035 の実機記録）。
PDF を索引するには venv に `pillow` も必要（pypdf の画像抽出）。

### D4. 受け入れ条件の判定は今までどおり taskd

調査タスクの受け入れ条件は `Check::Reviewer`（別の LLM run が判定）か `Check::Human` を使う。
PaperQA2 の回答自体を成功判定にしない（ADR-0026 D1 と同じ考え方）。

### D5. Web 調査（Local Deep Research）は次の段

`web-research` 分野と `local-deep-research` アダプタは本 ADR の範囲外。分野の仕組みができていれば `[[genres]]` と `[[roles]]` と
アダプタを 1 つ足すだけで載る。人間の調査どおり「論文は PaperQA2、Web・実装・製品は LDR」で分ける。

## 3. 採らない

- 分野にアダプタを直接持たせる（D2 の理由）。
- PaperQA2 の中のエージェント（ToolSelector）を止めて、`search_papers` / `gather_evidence` を taskd のツールとして呼ぶ構成
  （人間の調査の「最終的にはこちらが強い」案。二重オーケストレーションの問題は理解した上で、まずは既製の調査ループをそのまま使って実地で確かめる。
  必要になったら別 ADR で分解する）。
- Semantic Scholar の鍵を前提にする（鍵無しで始める。`settings` に足せば使える）。
- Onyx / DeerFlow（taskd と役割が重複しすぎる。人間の調査の結論と同じ）。

## 4. 受け入れ条件（Phase 16: 分野、Phase 17: 調査ハーネス）

**Phase 16（分野）**
1. `[[genres]]` を設定でき、知らない分野・分野に属さない役割は設定エラー / API の 422。
2. タスクに `genre` を付けると、役割・tier・アダプタが「タスク > 役割 > 分野の既定 > 親」の順で決まる（ユニットテスト）。
3. 委譲できる run のプロンプトに分野と役割の一覧が入り、`DelegateTask.genre` で子の分野を選べる（スタブのワーカーで e2e）。
4. `tasks` の migration（`SCHEMA_VERSION` 5）で既存 DB が読める。API / GUI に `genre` が出る（一覧の絞り込みと作成フォーム）。

**Phase 17（調査ハーネス）**
5. `paperqa` アダプタがスタブの `pqa` で `done` を合成し、出力が `progress` と `artifacts/answer.md` に残る（オフライン）。
6. 実機: ローカルの論文 2 本を置いた `paper_directory` に対し、`genre = "literature"` のタスクが taskd 経由で `done` になり、
   `artifacts/answer.md` に引用付きの回答が入る（LLM はトンネル越しの Qwen3.8-27B）。
7. `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / GUI の検査一式。
8. 提案 P-64（DESIGN §4 にタスクの `genre`）と P-65（§5.4 のアダプタ表に `paperqa`）を PROGRESS に書く。
