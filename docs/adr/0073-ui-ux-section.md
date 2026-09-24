# ADR-0073: engineering の下に UI/UX 課（`ui-ux`）を置き、`frontend` を software-engineering から移す

- 日付: 2026-09-24
- 状態: Accepted
- 関連: ADR-0046（組織と matching）、ADR-0069（routing の 4 層）、D8（tools の語彙）

## 状況
- 画面・操作の流れ・スマホ表示の仕事も、API や Rust の仕事も、すべて `software-engineering` に振り分けられていた。
  UI/UX 固有の作法（スマホ幅と PC 幅での表示確認、アクセシビリティ、スクリーンショットを証跡に残す）を
  課の policy として持たせる場所が無かった。
- matching（`task_ops::matching::decide`）は、タスクの harness を許す非根ノードのうち
  |task.skills ∩ 実効 skills| が最大のものを選び、同点は浅いノード → id の辞書順で決める。
  同じ skill を持つ課を並べるだけでは、辞書順で `software-engineering` が `ui-ux` に勝ってしまう。

## 決定
- D1: `engineering` 部の下に課 `ui-ux`（名前 UI/UX、genre `coding`）を置く。profile は次のとおり。
  - skills: `ui-design` / `ux` / `accessibility` / `frontend` / `react` / `typescript` / `css` / `responsive` / `usability`
  - tools: `gh` のみ
  - policy: 変更前後のスクリーンショットをスマホ幅（360/390/412px）とデスクトップ幅で artifacts に残す。
    表示確認には gui/ の Playwright（`scripts/check-*.mjs`）を使う。デザインだけで終わらせず、実装と表示確認まで行う。
  - harnesses: `default = "coding"`。`allowed` は継承（cos の conversation / plan、engineering の coding）で、
    `software-engineering` と同じ。
- D2: `frontend` は `software-engineering` から外し、`ui-ux` にだけ置く。`react` / `typescript` は両方の課が持つ
  （API や Remix の loader の仕事でも使うため）。
  - `[typescript, react, frontend]` のタスクは `ui-ux` が 3 点、`software-engineering` が 2 点で `ui-ux` に行く。
    `frontend` を両方に残すと 3 点同士で並び、辞書順で `software-engineering` が勝ってしまう。
  - `[typescript, react]` だけのタスクは 2 点同士で並び、辞書順で `software-engineering` に行く（従来どおり）。
    画面の仕事として振りたいときは `frontend` / `ui-design` / `css` などを付ける。
- D3: ブラウザでの確認のために新しい tool は足さない。Playwright はリポジトリ内のスクリプトであって、
  D8 の tools 語彙（`gh` / `tavily` / `exa` / `docker` / `cluster:*`）の項目ではない。
  UI の仕事にクラスタは要らないので `cluster:*` も与えない。
- D4: 本番は DB の `org_nodes` が正（種ファイル `config/org.example.toml` は DB が空のときにしか読まれない）。
  本番へは `POST /api/v1/org`（`ui-ux` の作成）と `PATCH /api/v1/org/software-engineering`（skills から `frontend` を外す）
  で反映する。

## 結果
- 画面・スマホ表示・アクセシビリティのタスクは `ui-ux` に、API / Rust / SQLite のタスクは `software-engineering` に
  決定的に振り分けられる。回帰テスト
  `config::tests::example_org_routes_ui_work_to_ui_ux_and_api_work_to_software_engineering`（crates/celeris）で固定した。
- 例の組織は 13 → 14 ノード。列挙しているテストを更新した。
- skill が空のタスクは、coding を許す課すべてが 0 点で並び、辞書順で `cluster-hpc` になる（この ADR の前から同じ規則。
  意味のある振り分けではないので、タスクには skill を付けることが前提）。

## 代替案
- `software-engineering` の policy に UI の作法を足すだけにする: API や Rust の仕事にもスクリーンショット要求が付いてしまう。
- `frontend` を両方に残し、辞書順以外の同点規則（例: 課の優先度）を足す: matching の規則を変えることになり、
  既存の組織すべてに影響する。skill の置き場所を変えるだけで足りる。
- ブラウザ操作用の tool（`playwright` など）を語彙に足す: 実体はリポジトリ内のスクリプトで、外部の資格情報も要らない。
  語彙を増やす理由が無い。
