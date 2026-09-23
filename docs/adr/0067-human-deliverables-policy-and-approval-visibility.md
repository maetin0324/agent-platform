# ADR-0067: 人が読む成果物の置き場の規則と、承認画面からの可視化

- 日付: 2026-09-23
- 状態: **Accepted**
- 関連: ADR-0036（成果物はタスクごと。`artifacts_dir` / `ArtifactRef.path`）、ADR-0047（知識ベース。
  `page_path` / `Index`）、ADR-0044 D7（成果物 → 知識ベースの昇格）、ADR-0033（案件・報告・組織）、
  SPEC §3.7（成果物の置き場所）・§4.6（成果物の画面）

## 1. 文脈 — 実機で起きたこと（本番、BenchFS 案件のタスク 01M35X86XTK84F97QW0CN5PGMR）

`scientific-writing` の「Phase1: framing/contribution 候補案の作成（HUMAN GATE 1 向け決定材料）」で、
計画（CoS / project_plan が作った受け入れ条件）が `docs/paper/phase1/framing-candidates.md が存在する`
（`command` チェック `test -f …`）・`reviewer`・`human`（「人間が HUMAN GATE 1 の判断材料としてこの内容を
確認する」）の 3 条件だった。作業場所は `{"kind":"local","path":"<task_id>"}` の空ディレクトリ（git
worktree ではない）。ワーカーは `docs/paper/phase1/framing-candidates.md`（164 行の決定パケット）を
`artifacts/` の**外**に書き、`artifacts/` には `result.json`/`sources.json`/`validate_framing.py` だけを
置いた。`.md` 本体は result.json の `artifacts` にも無いので、`GET /tasks/{id}/artifacts` は空、
`GET /tasks/{id}/changes` は「working tree が無い」404。結果、承認（01M37KDV94FPN70M5VYED7SGD2）が
受信箱に来たのに GUI からは見るべき成果物がどこにも無く、人は手で知識ベースへ移して判断した。

人の指摘（設計上の問題として直す）:

1. Celeris が対象リポジトリの `docs/` を情報置き場として使うと、リポジトリのドキュメントが腐る。Celeris
   側の判断過程を対象リポジトリの追跡ファイルに残すのも不適切。
2. 人が判断する材料は GUI から見えなければならない（SPEC §3.7「成果物は人が普段見る場所に、公開できる形で」）。

`Check::ArtifactExists` の実装を調べると、この事故は構造的に起きうることが分かった: `claude-code` /
`codex` / `acp` アダプタ（`crates/task-worker/src/claude_code.rs` 他）は結果ファイルの `summary` /
`question` / `evidence` しか読まず、**`Event::ArtifactProduced` を一度も出さない**（`sink.artifact()` を
呼ぶのは `local-deep-research` / `paperqa` / ストリーミングプロトコルのハーネスだけ。
`crates/task-worker/src/subprocess.rs:223`、`langmem.rs:454`、`local_deep_research.rs:704`、
`paperqa.rs:2038`）。`Check::ArtifactExists` は「その run が申告したパス → 無ければ既定パス」の順で
ファイル**存在**だけを見る（`crates/task-dispatch/src/review.rs:301`）ので判定自体は通るが、
`GET /tasks/{id}/artifacts` は `Event::ArtifactProduced` の一覧（`crates/task-api/src/files.rs::produced`）
でしかなく、コーディング系ハーネスが書いたファイルはここに一切現れない。

## 2. 決定

### D1. 置き場の規則（明文化し、プロンプトに反映）

人が読む決定材料（調査報告、framing 案、gate 資料、比較表、提案書）は**知識ベース**
（`projects/<project>/…` のページ、ADR-0047）か**登録済み artifacts**（作業場所の `artifacts/` 配下で
`result.json`/`Event::ArtifactProduced` に載るもの、ADR-0036）に置く。対象リポジトリに書いてよいのは
「そのリポジトリ自身の成果」（コード、テスト、決定後の論文本文・README など）だけで、Celeris の判断過程・
候補案・決定パケットは対象リポジトリの追跡ファイル（`docs/` を含む）に置かない。

この規則を、ワーカーが読むプロンプトの 4 か所に短く書く（既存の文面の構造は壊さない）:

- **CoS の会話プロンプト**（`crates/task-worker/src/preamble.rs::actions_instructions`）: `create_task`
  の説明の直後に、`acceptance` の `human` 条件を書くときの置き場ルールを 1 段落。
- **`create_task` の計画（Plan run）**（`crates/task-worker/src/claude_code.rs::build_plan_prompt` の
  `plan.json` スキーマ節）: 子タスクの `check` を選ぶときの同じルールを 1 段落。
- **`project_plan`**（`crates/task-ops/src/project_plan.rs::start`）: Plan タスクの `goal` に短い
  フッターとして同じルールを追記する（この Plan タスク自身は `acceptance = []` で作られる
  `task_ops::add::create_support_task` に渡るだけなので、規則はワーカーへの指示文＝`goal` に足す）。
- **ワーカーのプリアンブル**（`crates/task-worker/src/preamble.rs::render`）: 全 run 共通で出す短い節
  （`paperqa` を含む、`preamble::render` を直接使う全ハーネスに効く）。

### D2. 計画時の検証（`human` チェックには成果物か知識ページの参照を要求する）

`Check` に **`KnowledgePage { path: String }`** を追加する（`task_core::knowledge::page_path` と同じ
「KB の根からの相対、`.md`、`..` 不可」の形。検証は書き込み時ではなく型止まりでよい — 実在確認は
既存の `Check::ArtifactExists` と同様、レビュー時に行う余地を残すが、今回のスコープでは受け入れ条件の
**組み立て時の存在検証はしない**。人が確認するときに知識ベース側の 404 で気付ける）。

純粋関数 `task_core::model::validate_human_checks_have_deliverable(acceptance: &[Criterion]) ->
Result<(), String>` を追加する: **`Check::Human` を持つ受け入れ条件が 1 つでもあれば、同じ acceptance の
どれかが `Check::ArtifactExists` か `Check::KnowledgePage` でなければならない**。`Check::Command` の
`test -f docs/…` のような「作業場所の任意パス」だけでは不足（作業場所は GUI から見えるとは限らない。
ADR-0036 の事故がまさにこれ）。理由文字列は固定: 「人が確認する成果物が GUI から見える場所（artifacts か
知識ベース）に無い」。

呼び出し箇所（`task-ops` の add / plan / project_plan / delegate の検証。子タスクの `acceptance` が
決まる 3 つの経路 + 1 つの単独経路）:

- `task_ops::add::build_acceptance`（`POST /tasks`・`celerisctl add`。単独タスク作成の経路）。
- `task_core::plan::validate`（Plan run の `plan.json` → 子タスク。`PlanError::NoHumanDeliverable`）。
- `task_core::delegate::validate_one`（`delegate.json` → 子タスク。`DelegateError::NoHumanDeliverable`）。
- `task-ops::project_plan` は `acceptance = []` の Plan タスクを作るだけ（子の acceptance を決めない）
  なので、検証はここには置かない。実際に子の `acceptance` が決まるのは上記の `plan::validate` 側
  （project_plan が起こす Plan run も `build_plan_prompt` → `plan.json` → `plan::validate` を通る）。

### D3. ワーカーの取りこぼし防止（未申告の成果物を拾う）

`crates/task-dispatch/src/dispatcher.rs::run_worker` の run 完了後（`adapter.run(...)` が `Ok` を返した
とき）、**git worktree ではない `local` の作業場所に限り**（`remote.is_none() && worktree.is_none()`。
worktree の場合はリポジトリの成果と区別できないので対象外、ADR-0036 の所有規則と揃える）、作業場所の
`artifacts/`（`artifacts_dir`）の外にある `*.md` を走査し、その run の間に既に登録済みの `path`
（`Event::ArtifactProduced` の履歴。同一タスクの以前の run 分も含む。retry のたびに重複登録しない）と
重ならないものを「未申告の成果物」として `Event::ArtifactProduced { artifact: ArtifactRef { declared:
false, .. } }` で登録する。

- `ArtifactRef` に `declared: bool` を追加（`#[serde(default = "declared_default")]`、既定 `true`。
  通常の申告済み成果物はすべて `declared: true` のまま — 挙動もスキーマの互換性も変えない）。
- 上限: 件数 20、1 ファイル 1 MiB。空ファイルは対象外。
- 除外: `node_modules` / `.venv` / `target` / `.git`（配下は再帰しない）。`artifacts_dir` 自身の配下も
  対象外（そこは既に「成果物置き場」として扱われている）。
- 実装は `crates/task-dispatch/src/undeclared_artifacts.rs`（走査は純粋関数、I/O は sha256 の読み取りと
  ディレクトリ列挙だけ。LLM 呼び出しは無い）。

### D4. GUI の承認画面（受信箱・タスク詳細の人レビューパネル）

- `ApprovalItem.artifacts`（`crates/task-ops/src/inbox.rs`）の要素に `idx`（`GET /tasks/{parent_id}
  /artifacts/{idx}` と同じ添字。`ArtifactProduced` の**全履歴**での出現順）を足す
  （`#[serde(flatten)]` で既存フィールドの形は保つ）。添字は `task_ops::derive::artifacts_for_run_with_idx`
  で求める（`ArtifactExists` の解決規則と同じ「名前が一致する最後の 1 件」ではなく、`GET
  /tasks/{id}/artifacts` の一覧と同じ「登場順」を使う — 添字を GUI がそのまま fetch に使うため）。
- 本文取得 API は既にある: `GET /tasks/{id}/artifacts/{idx}`（`crates/task-api/src/files.rs`）が
  `Content-Type`（`.md` は `text/markdown`）付きで返す。GUI 側の中継 `files/tasks/:id/artifacts/:idx`
  （`gui/app/routes/files.artifacts.ts`）も既にある。新規エンドポイントは追加しない
  （既存で D4 の要求「本文取得 API」を満たす）。
- `gui/app/routes/inbox.tsx` の `ApprovalRow` と `gui/app/components/HumanReviewPanel.tsx` の
  `HumanReviewItem` に、Markdown の成果物をその場で `~/components/MarkdownViewer`（`MarkdownViewerBody`、
  `react-markdown`/`remark-gfm`）で描画する折りたたみを足す（`gui/app/routes/tasks.$id.tsx` の
  `ArtifactRow` と同じ「開く」トグル + fetch のパターンを再利用）。
- `Check::KnowledgePage` の受け入れ条件は、対応する `criteria[].check.path` から
  `~/lib/knowledge.ts::knowledgeHref` でリンクと（分かれば）タイトルを出す。

### D5. `celerisctl plan-lint`

DB 上の `draft` / `ready` のタスクの受け入れ条件を D2 の規則（`validate_human_checks_have_deliverable`）
で点検し、違反（タスク id・タイトル・理由）を一覧する読み取り専用のコマンド。直しはしない。

既存の `celerisctl plan <goal>`（`Command::Plan(PlanArgs)`、`goal` は素の位置引数）に `lint` サブコマンドを
足すと `celerisctl plan lint` が「`goal = \"lint\"` で Plan タスクを作る」と衝突する（`goal` は
`clap` の必須位置引数なので、`plan` を `Subcommand` に組み替えない限り両立しない）。既存 CLI を壊さないため、
`celerisctl plan lint` ではなく独立コマンド **`celerisctl plan-lint`** にした（D5 冒頭の指示文が許す
「既存の近いサブコマンドへの追加」の代わりの実装）。

## 3. 却下した案

- **`Check::Human` の解決時に知識ベース/artifacts の実在を強制する**（レビュー時に 404 なら不合格）。
  今回は「計画時に置き場の種別があるか」だけを見る。存在検証まで持ち込むと、`KnowledgePage` を書いた後で
  ページを作る（先に受け入れ条件を書き、ワーカーがページを作ってから run が終わる）という通常の順序が
  レビュー時点で不合格になりうる。既存の `ArtifactExists` もレビュー時にしか実在を見ない設計を踏襲する。
- **対象リポジトリへの書き込みを機械的に禁止する**（ワーカーのファイル書き込み自体を制限する）。
  「そのリポジトリ自身の成果」は引き続き書いてよいので、書き込み先そのものを禁止するのは過剰。プロンプトで
  導き、計画時の検証（D2）と取りこぼし防止（D3）で事故が GUI から見えなくなることを防ぐ、という 2 段構えにした。
- **D3 を worktree タスクにも広げる**。worktree の作業ツリーはリポジトリの成果物そのもの（コミット前の
  diff）と区別がつかず、`*.md` を機械的に「未申告の成果物」として拾うと、単なる作業中のメモや
  README 編集まで artifacts 一覧に混ざる。`GET /tasks/{id}/changes`（worktree の diff）が既に GUI から
  見えるので、worktree タスクは対象外にした。

## 4. 影響

- `Check` に 1 バリアント追加（`docs/protocol/*.schema.json` 相当、`docs/api/v1/api-v1.schema.json` を
  `UPDATE_SCHEMA=1` で再生成）。
- `ArtifactRef` に `declared: bool` を追加（`#[serde(default)]` で過去のイベント JSON との互換を保つ）。
- `ApprovalItem.artifacts` の要素形が `ArtifactRef` → `{idx} & ArtifactRef`（flatten）に変わる
  （追加のみ、既存フィールドは残る）。
- `PlanError` / `DelegateError` に `NoHumanDeliverable { index }` を追加。
