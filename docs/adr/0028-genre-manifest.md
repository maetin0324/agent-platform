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
- `input_artifacts` / `output_artifacts` は**約束ではなく目安**。taskd は中身を検査しない（受け入れ条件の判定は従来どおり `Check`）。
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
