# ADR-0028: 分野を「能力レジストリ」にする（capabilities と入出力の成果物、Planner による分野選択）

- 日付: 2026-09-17
- 状態: **Accepted**（人間の依頼「manifest 化を先に終わらせてください」。人間の提案:
  「genre を固定 enum にせず `name / harness command / capabilities / input artifact types / output artifact types` の manifest にしておき、
  Planner には分野の一覧だけ渡して選ばせる」）
- 関連: ADR-0027（分野の導入。本 ADR はその拡張）、ADR-0016（役割と委譲）、ADR-0007（Planner）、ADR-0026 / ADR-0027 D3（アダプタ）

## 1. 文脈

ADR-0027 で分野（`[[genres]]`）を入れたが、持っているのは `id` / `description` / `default_role` / `roles` だけ。
委譲するワーカーには分野の一覧を渡しているものの、**渡しているのは説明文だけ**なので、
「その分野は何ができるのか」「何を渡せばよく、何が返ってくるのか」を書き手の文章力に頼っている。

分野を増やす（web-research / browser / data-analysis / presentation …）ほど、この曖昧さは routing の失敗として出る。
人間の提案どおり、分野を**能力レジストリ**にして、機械可読な形で「できること」と「入出力」を持たせる。

さらに、今は **Planner が分野を選べない**（`PlanOutput.tasks[]` に分野が無く、子は親の分野を継ぐだけ）。
「大きなタスクを分野ごとの子に割る」のは Planner の仕事なので、ここを開ける。

## 2. 決定

### D1. `[[genres]]` に能力と入出力を足す（すべて任意。既存の設定はそのまま動く）

```toml
[[genres]]
id = "related-research"
description = "先行研究の確認・新規性の検討"
capabilities = ["学術文献の検索", "引用グラフの探索", "PDF 全文からの根拠抽出"]
input_artifacts = ["question", "pdf", "bibliography"]
output_artifacts = ["answer.md", "citations.json"]
default_role = "literature-reader"
roles = ["literature-scout", "literature-reader", "novelty-skeptic"]
```

- 3 つとも `Vec<String>` の自由記述（固定 enum にしない。人間の提案どおり）。空なら出力にも出さない。
- `input_artifacts` / `output_artifacts` は**約束ではなく目安**（**Phase 38 追記**: ただし
  ハーネスで動く分野では約束である。§2.5 D6〜D8）。taskd は中身を検査しない（受け入れ条件の判定は従来どおり `Check`）。
  「この分野に投げるなら何を用意すべきか」「戻ってくるものは何か」を、委譲側と Planner に伝えるためのラベル。
- ハーネス（アダプタ）は引き続き**役割が持つ**（ADR-0027 D2）。manifest に `harness command` は入れない
  （同じ分野でも役割ごとに実行器を変えられる設計を壊さないため。どのアダプタが使われるかは `GET /config` の
  `roles[].adapter` と `genres[].roles` の対応で分かる）。

### D2. 分野の一覧は「能力つき」でワーカーに渡す

`RunContext.available_genres[]`（ADR-0027 D1）の各要素に `capabilities` / `input_artifacts` / `output_artifacts` を足す。
プロンプトの「使える専門家」節も、次の形で出す:

```
## 使える専門家（分野と役割）
- related-research: 先行研究の確認・新規性の検討
  できること: 学術文献の検索 / 引用グラフの探索 / PDF 全文からの根拠抽出
  渡すもの: question, pdf, bibliography → 返るもの: answer.md, citations.json
  役割: literature-scout, literature-reader, novelty-skeptic
```

### D3. Planner も分野を選べるようにする

- `PlanOutput.tasks[]`（`NewTask`）に `genre: Option<String>` と `role: Option<String>` を足す（`role` も今まで無かった）。
  `plan.rs` の `materialize` は、子の分野を **明示 > 役割から一意に決まる分野 > 親の分野** の順で決める（ADR-0027 D1 の委譲と同じ規則）。
  未知の分野・分野に属さない役割は `PlanError` で拒否する（Plan run は失敗になり、従来どおり retry / 人の判断に回る）。
- Plan kind の run の `RunContext.available_genres` を埋める（今は Execute / Approval だけ）。Planner のプロンプトにも D2 の節を出す。
- **挙動の変更**: 委譲と同じ優先順にそろえた結果、Plan の子の `tier` は「指定なしのとき **親の tier を継ぐ**」になる
  （従来は親に関係なく `Standard`）。ADR-0016 の委譲と Plan で規則が違うのは分かりにくいので、こちらに寄せる。
  既存のテストと e2e の期待値は 1 箇所ずつ直した。
- `worker-protocol` の版は 3 のまま（追加フィールドのみ）。

### D4. API / GUI

- `GET /config` の `genres[]` に 3 つのフィールドを足す（`GenreConfigView`）。
- GUI のタスク作成で分野を選んだとき、`description` に加えて **できること / 渡すもの / 返るもの** を出す（選択の助けになる情報を、GUI 側で再計算せずそのまま表示する）。

## 2.5 Phase 38 追記（成果物の説明、ハーネス系分野の規約）

- 日付: 2026-09-18（人間の決定。**実機のレビュー不合格**から）
- 関連: ADR-0035 D6（`candidates.json` → `papers.json` の改名）、ADR-0033 D4（計画のプロンプトに manifest と
  組織図を渡す）、ADR-0027 D3（`paperqa`）、ADR-0029（`local-deep-research`）

### 何が起きたか（本番、2026-09-18）

秘書の計画 run が研究文献調査課（`literature` = PaperQA2 ハーネス）に「候補テーマを 3〜5 件、新規性・
実現可能性・接続点・引用付きで **`candidates.json` にまとめよ**」という objective と、
`artifact_exists: candidates.json` の受け入れ条件を付けた。ところが `candidates.json` は**ハーネス
（取得ランナー）が書く論文の検索コーパス**の固定名で、ワーカー（`pqa`）は計画が指定したファイルを書けない
（ハーネス run は `answer.md` を返すだけ）。答えの中身は良かったのに、レビュアーは基準どおり不合格にした。
**計画がハーネスの成果物の名前を知らずに勝手に決めている**のが原因である。

D1 は `input_artifacts` / `output_artifacts` を「約束ではなく目安」と書いた。**ハーネスで動く分野では
目安ではなく約束である**（担当は名前を選べない）。そこを次の 4 点で埋める。

### D5. `output_artifacts` の 1 要素に説明を書ける（`名前: 説明`）

```toml
output_artifacts = ["answer.md: 引用付きの答え（これが答え）",
                    "papers.json: 検索した論文の一覧（コーパス。答えではない）",
                    "sources.json: 出典と引用の有無"]
```

- 型は今までどおり `Vec<String>`（`名前` だけの従来の書き方も引き続き有効）。**名前は `:` の前**
  （`task_core::artifact_entry_name` / `artifact_entry_description`、`GenreSpec::output_artifact_names`）。
- 計画とレビュアーのプロンプトには名前と説明の両方を出す。`GET /config` の `GenreConfigView` は
  **型を変えない**（値の文字列に説明が付くだけ。GUI は `:` の前を名前として扱う。`docs/gui/api.md`）。

### D6. 「ハーネス系の分野」は `default_role` のアダプタで決まる（決定的）

- `GenreSpec::is_harness(roles)` = `default_role` の役割の `adapter` が `paperqa` /
  `local-deep-research`（`task_core::HARNESS_ADAPTERS`）のどれか。LLM には聞かない。
- `RunContext.available_genres[].harness` にそのアダプタ id を載せる（プロトコルは追加のみ。版は 4 のまま）。
- ハーネスでない分野（`claude-code` / `codex` / `acp` の coding 等）は**従来どおり自由**
  （成果物の名前はワーカーが決められる）。この場合プロンプトは Phase 37 までと 1 バイトも変わらない。

### D7. 計画とレビュアーのプロンプトに成果物の規約を出す（manifest から決定的に組む）

- **Plan run**: 「この分野の担当は**ハーネス**で動く。成果物は次の名前で固定され、担当が別のファイルを
  書くことはできない: `<output_artifacts>`。受け入れ条件（`artifact_exists`）にはこの名前だけを使うこと。
  **内容の要求は `objective` に書き、レビュアー条件で判定させる**（『X を Y に書け』ではなく
  『答えに X を含めよ』）」。
- **Reviewer run**（対象タスクの分野がハーネス系のとき。`RunContext.subject_genre`）: 同じ一覧を出し、
  「`papers.json` は検索コーパスであって答えではない。答えは `answer.md`。テーマ候補等の内容はそこで
  判定せよ。ファイル名の不一致だけを理由に不合格にするな」。

### D8. 計画の後の検証（決定的。壊さず直す）

`PlanOutput.tasks[]` の `acceptance` に `artifact_exists` があり、その子の分野（`materialize` と同じ
「明示 > 役割 > 担当 > 親」で解決）がハーネス系で、名前が `output_artifacts` に無ければ:

1. **その基準を落とす**（`Question` にしない。Plan run も失敗させない）、
2. `warn` を 1 行出す（`task_core::plan::fix_harness_artifacts` が文面を返し、ディスパッチャが `warn!`）、
3. `objective` の末尾に「（注: この担当の成果物は `<一覧>` に固定。要求した内容は `<答えの成果物>` の中で
   述べる）」を足す。

落とすと受け入れ条件が 0 件になる場合だけ、落とす代わりに**同じ文のレビュアー条件**にする
（条件ゼロのタスクを作らないため。「内容はレビュアーに判定させる」という D7 の方針と一致する）。
判定はすべて決定的で、LLM は呼ばない（DESIGN 原則 1）。

## 3. 採らない

- 入出力の型を taskd が検査する（成果物の検査は `Check::ArtifactExists` 等、既存の受け入れ条件の仕事）。
- manifest にハーネスのコマンドを持たせる（D1 の理由）。
- 分野を固定 enum にする（人間の提案どおり、設定で増やせるままにする）。

## 4. 受け入れ条件（Phase 18）

1. `[[genres]]` に `capabilities` / `input_artifacts` / `output_artifacts` を書け、`GET /config` と `RunContext.available_genres` に出る。空なら省略される。
2. 委譲できる run と **Plan run** のプロンプトに D2 の形で出る（スタブのワーカーでの確認）。
3. `PlanOutput.tasks[]` の `genre` / `role` が効き、子の分野が「明示 > 役割 > 親」で決まる。未知の分野・不一致の役割は Plan の失敗になる。
4. 既存の設定（3 フィールドなし・Plan の `genre` なし）がそのまま動く。
5. `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / GUI の検査一式。
