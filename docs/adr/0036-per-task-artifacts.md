# ADR-0036: 成果物はタスクごと（共有 workspace では `.taskd/artifacts/<task_id>/`）

- 日付: 2026-09-18
- 状態: **Accepted**（人間の決定。本番で兄弟タスクの成果物が上書きされた事故を受けたもの）
- 関連: ADR-0003 D5（`artifact.path` はワークスペース相対）、ADR-0005 D3（1 タスク = 1 作業ディレクトリ）、
  ADR-0006 D3（結果ファイル規約 `artifacts/result.json`）、ADR-0007 D1/D4（`plan.json` / `review.json`）、
  ADR-0016 D3/M8（`delegate.json` / `summary.md`）、ADR-0018 D1（Remote の写しと同期の除外）、
  ADR-0033 D6（結果ファイルの `memory`）、ADR-0034 D7（結果ファイルの `report`）、
  DESIGN §5.4 / §5.8 / §5.9 補足 2、`docs/protocol/worker-protocol.md` §9 / §10

## 1. 文脈 — 実機で起きたこと（本番、2026-09-18）

秘書の計画 run が作った 2 つの兄弟タスク（研究文献調査課 = PaperQA2、Web 調査課 = LDR）が、
**親（計画タスク）の workspace をそのまま継いだ**（`plan::materialize` / `delegate` は
`workspace: parent.workspace.clone()`）。両方のアダプタが `<workspace>/artifacts/` に書いたため、

- LDR の `sources.json` が PaperQA2 の `sources.json` を**上書き**した。
- レビュアーは**相手の課の成果物**を読んで判定した。
- `artifacts/result.json` も同じパスなので、`max_concurrency` を 3 にした今、
  兄弟が同時に走ると**結果ファイルそのものが競合**する（これまで表に出なかったのは `max_concurrency = 1`
  だったからにすぎない）。

`artifacts/` は「そのタスクの成果物置き場」という前提（ADR-0005 D3: 1 インスタンス = 1 タスクの作業
ディレクトリ）で全アダプタが書いているが、**workspace を共有するタスクが存在する**（plan / delegate の子）
ため、その前提が本番で崩れた。

## 2. 決定

### D1. 成果物ディレクトリはタスクごと。`RunRequest.artifacts_dir` で渡す

`RunRequest` に `artifacts_dir: PathBuf`（絶対パス）を足す。**ディスパッチャが決め**、アダプタは
そこに書く（アダプタは判断しない）。

- タスクが workspace を**自分で所有**しているとき → 従来どおり `<workspace>/artifacts/`
- workspace を**親から継いでいる**とき（plan / delegate の子）→ `<workspace>/.taskd/artifacts/<task_id>/`

所有の判定は `task_core::artifacts_dir_for(task, workspace_dir)`（純粋関数。LLM も I/O も無い）:

1. `task.parent_id` が無い → **所有**（単独タスク。既存の挙動・パス・テストを 1 バイトも変えない）
2. 作業ディレクトリの末尾の要素がそのタスクの id（既定の `workspace_root/<task_id>`、および
   `WorkspaceSpec::Remote` の手元の写し）→ **所有**
3. それ以外（親から継いだ path、案件のディレクトリを親子で共有している場合）→ **共有**

`parent_id` を見るのは、`--workspace` に自分で作ったディレクトリを渡した**単独**タスク（既存のテストと
本番の大半）を「共有」に落とさないため。事故の原因は「親から継いだ」ことなので、判定も血縁で行う。

`WorkspaceSpec::Remote` は規則 2 で**所有**になる（手元の写しは `workspace_root/<task_id>` でタスクごとに
分かれている）。ADR-0018 D1 の同期は `.taskd/` を両方向で除外するので、Remote の成果物を
`.taskd/artifacts/` に置くと**クラスタで作られた成果物が写しに降りてこない**。写しが既にタスクごとである
以上、Remote を共有扱いにする実益は無い（ただし `sync = rsync` / `none` で同じリモートディレクトリを
複数タスクが使う場合、**クラスタ側**の `artifacts/` では依然ぶつかる。`sync = worktree` なら
worktree がタスクごとなのでぶつからない。残件として PROGRESS に記す）。

### D2. 「run が書くファイル」は全て `artifacts_dir` の中

`result.json`（ADR-0006 D3）、`delegate.json`（ADR-0016 M8）、`plan.json`（ADR-0007 D4）、
`review.json`（ADR-0007 D1）、`summary.md`（ADR-0016 M4）、`answer.md` / `sources.json` /
`papers.json`（Phase 38 で `candidates.json` から改名）/ `report.md` / `research.json`（ADR-0031 / ADR-0035）は、すべて
`artifacts_dir` を基準に読み書きする。結果ファイルから読む `memory`（ADR-0033 D6）と
`report`（ADR-0034 D7）も同じ。

レビュー run（合成した `Review` kind のタスク）は、**対象タスクの `artifacts_dir`** を使う
（レビューは対象タスクの成果物についての判定であり、`review.json` はその対象の成果物の隣に置く）。

### D3. ワーカーへの指示は `artifacts_dir` の**相対パス**で書く

`claude-code` / `codex` / `acp` のプロンプトの「`artifacts/result.json` に書け」は、
`artifacts_dir` の workspace 相対表記（単独タスクなら `artifacts`、共有なら `.taskd/artifacts/<task_id>`）
で組む。単独タスクでは従来と**バイト単位で同一**の文面になる（プロンプトの回帰テストがそれを固定する）。

### D4. `ArtifactRef.path` は従来どおり workspace 相対

GUI の `GET /tasks/{id}/artifacts/{idx}` は記録済みの `ArtifactRef.path` を workspace に結合して
`canonicalize` する（ADR-0013 D11）ので、`.taskd/artifacts/<id>/report.md` のような相対パスも
そのまま読める（隠しディレクトリを弾く規則は無い）。よって:

- 成果物の申告（`artifact` メッセージ、アダプタ側の `crate::artifact::resolve`）は**workspace 相対**のまま。
- `Check::ArtifactExists{name}` の解決は、その run が申告したパス → 無ければ
  `<artifacts_dir の相対表記>/<name>` の順（従来は常に `artifacts/<name>`）。
- 案件の成果物一覧（`GET /tasks/{id}` の `artifacts[]` を束ねる GUI の見せ方）は変わらない。

### D5. `PROTOCOL_VERSION` は 4 のまま

`run` メッセージへの**追加のみ**で、既存フィールドの意味は変えない。`artifacts_dir` を知らない
ワーカー（`artifacts/` に決め打ちで書くもの）は、単独タスクでは従来どおり動く。共有 workspace の
タスクでは `artifacts_dir` を読む必要がある。`docs/protocol/worker-protocol.md` に追記する。

## 3. 却下した案

- **子タスクに独自の workspace を与える**（`workspace_root/<child_id>`）。親のリポジトリ・入力・前の子の
  出力が見えなくなり、plan / delegate の意味（同じ作業場で分担する）が壊れる。
- **ファイル名にタスク id を混ぜる**（`result-<id>.json`）。ワーカーへの指示が長くなり、既存の
  「`artifacts/result.json` に書け」という 1 行の規約を全ハーネス（PaperQA2 / LDR の Python 側も）で
  変える必要がある。ディレクトリを分けるほうが規約は 1 行のままで済む。
- **`artifacts/<task_id>/`**（`.taskd/` でなく `artifacts/` の下に掘る）。`artifacts/` は Remote 同期の
  対象で、かつ人が見る成果物置き場なので、単独タスクと共有タスクで階層が混ざるのを避けた。
  `.taskd/` は既に「taskd の管理用」と決まっている（ADR-0018 D1、同期から除外）。

## 4. 影響

- `RunRequest` に 1 フィールド追加（`docs/protocol/worker-protocol.schema.json` を再生成）。
- `Workspace::prepare` は `artifacts_dir` を作る。`Workspace::collect` は `artifacts_dir` 配下を列挙する
  （`path` は workspace 相対のまま）。
- 単独タスク（`parent_id` なし）の挙動・パス・プロンプトは不変。
