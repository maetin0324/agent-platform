# ADR-0047: Knowledge Base / Memory Layer — 正本はローカルの Markdown、共通のアクセス、LangMem は抽出と整理の層

- 日付: 2026-09-20
- 状態: **Accepted**（人間の提案「Celeris Knowledge Base / Memory Layer 導入提案」2026-09-20 をそのまま採用。原則: Local-first、
  Human-readable、Agent-shared、Self-maintaining、User-owned、Implementation-independent。初期は Knowledge Graph・Vector DB・
  大規模 Semantic Search を必須にしない）
- 関連: ADR-0046（profile の `knowledge` マウント）、ADR-0033 D3（ノードの長期記憶 `memory/<node>/notes.md`）、ADR-0044 D7（文書は
  git が正本 — 同じ流儀）、ADR-0042（`~/.local/celeris`）、ADR-0048（Console）

## 1. 決定

### D1. 置き場と形

- 正本は **`~/knowledge/`**（`[knowledge] root` で変えられる。TrueNAS で永続化されたホームの下を想定）。**git リポジトリ**にする
  （無ければ `git init`。変更は 1 件ごとにコミット。ADR-0044 D7 の文書と同じ扱いなので GUI の描画・編集・履歴をそのまま使える）。
- 人が読める Markdown（front matter 付き）。DB や外部サービスを正本にしない。索引（`index.json`）は**再生成できる派生物**。

```
~/knowledge/
  user/                 # User Knowledge: profile.md, expertise.md, preferences.md, goals.md …
  environment/          # 環境: clusters/pegasus.md, servers/home-dev.md, tools/…
  projects/<slug>/      # 案件の知識: design.md, decisions.md, status.md …（案件の slug は ADR-0044 D7 と同じ）
  experience/           # 経験: YYYY/MM/<slug>.md（問題・解法・結果・採らなかった案と理由）
  _inbox/               # 抽出された候補（D4）。まだ索引に入らない
  index.json            # 派生物。{path, title, tags, scope, sources, updated}
```

- 1 ファイル = 1 トピック。front matter: `title`、`tags: [..]`、`scope: user | environment | project:<id> | experience`、
  `sources: [task:<id>, message:<id>, human, url:<…>]`、`created`、`updated`、`confidence: high | medium | low`。本文は Markdown。
- ノードの長期記憶 `memory/<node>/notes.md`（ADR-0033 D3）は**そのノードの私的な手帳**として残す。KB へは D4 の整理で昇格する。
  実効 profile の `knowledge = [{kind = "memory"}]` はこの手帳をマウントする。

### D2. マウント（何を誰が見るか）

- ADR-0046 D1 の `knowledge` の kind: `kb`（`scope` = KB の相対パス。`user`、`environment/clusters`、`projects/<slug>` …）、`repo`
  （案件のリポジトリの `docs/`。ADR-0043）、`dir`（任意のローカルディレクトリ。読み取り）、`memory`（ノードの手帳）。
- **案件は自動で `projects/<slug>` をマウント**する。CoS は `user/*` と `projects/*` を、Operations は `environment/*` を、と
  組織の profile で決める。実効マウント = 組織（根→葉の和）＋案件＋タスクの明示。
- 前置きには**索引**を出す（マウントされた scope の `title` / `tags` / `path` を最大 200 件。長いものは省く）。本文は入れない。
  ワーカーは D3 の道具で必要なものだけ読む。

### D3. エージェントからの共通アクセス（決定的。LLM 不在）

- ワーカーからは **`celerisctl knowledge`**（ワーカーの PATH に置く。コンテナでは KB を同じパスに読み取り専用で、`_inbox` は書き込み可で
  マウント。ADR-0043 D3 の mounts に足す）:
  - `celerisctl knowledge search <query> [--scope …] [--limit N]` — front matter の `tags` / `title` の一致と、本文の全文一致
    （`git grep -il`、無ければ `grep -ril`）。順位は tag 一致数 → title 一致 → `updated` の新しさ。索引は `index.json`。
  - `celerisctl knowledge get <path>` — 本文（front matter 付き）。
  - `celerisctl knowledge record --title … --scope … --tags … --source task:<id> [--confidence …] < body.md` — **候補**を `_inbox/` に書く
    （正本には直接書かない。D4 が整理する）。
- API（GUI と CoS 用）: `GET /knowledge/tree?scope=&q=`、`GET /knowledge/page?path=`（ADR-0044 D7 の `docs/page` と同じ形。描画・履歴・etag）、
  `PUT /knowledge/page`（人の編集。コミット）、`GET /knowledge/inbox`、`POST /knowledge/inbox/{id}/{accept|reject}`（管理系）。
  `celerisctl knowledge reindex` が `index.json` を作り直す（daemon は起動時と `_inbox` 変化時に呼ぶ）。
- 前置きの案内文: 「知識は `celerisctl knowledge search` で探し、`get` で読む。将来も使える事実を得たら `record` で候補に入れる
  （一時的な情報・雑談・推測は入れない。出典を付ける）」。

### D4. 自動メンテナンス（LangMem は Memory Management の層）

- **抽出のトリガ**は決定的: タスクが終端になり報告（ADR-0034）ができたとき、その案件・ノードに対して **知識整理 run**
  （裏方 `support = "knowledge"`。人には見せない。1 タスクにつき 1 回）を tick が起こす。入力: 報告・`result.json`・コメント・
  人との対話（そのタスクに紐付くもの）・既存の関連 KB ページ（D3 の search で上位 10 件）。
- 実行は **`tools/langmem/`**（python venv。LangMem の memory manager を使う）を `knowledge` harness（adapter = 新しい `langmem`
  アダプタ。他のアダプタと同じワーカー・プロトコル）で起こす。LLM は設定 `[knowledge.langmem] provider = "…"`（既定は本番の
  Qwen ローカル。Claude も選べる）。出力は **候補の集合**: `{op: create | update | merge | retire, path, title, tags, scope, body, sources, confidence}`。
- **適用**: `confidence = high` かつ `op = create | update` は KB に直接コミット（author `Celeris (knowledge) <celeris@local>`、
  message に task id。人は git で戻せる）。`merge` / `retire` と `medium` / `low` は `_inbox/` に置き、人が GUI で accept / reject。
  同じ `path` を人が編集中（未コミットの差分あり）なら候補は `_inbox/` へ。
- 保存しないもの（抽出の指示文に明記）: 一時的な情報、雑談、重複、信頼性の低い推測、秘密（API キー・パスワード・トークン。
  適用前に決定的な検査 — `sk-…`、`ghp_…`、`-----BEGIN` などのパターン — で弾く）。
- ノードの手帳（`memory/<node>/notes.md`）も同じ run が読み、KB に昇格すべきものを候補にする。

### D5. Console と GUI

- GUI「知識」画面: ツリー（scope 別）、検索、ページの描画・編集・履歴（ADR-0044 D7 の部品）、`_inbox` の一覧（accept / reject、出典へのリンク）。
- タスクのタイムライン（ADR-0044 D5）に「この仕事から知識 N 件が候補になった／取り込まれた」を載せる。
- Console（ADR-0048）で CoS は `search` の結果を返事に使える（対話 run の前置きに索引、道具として `celerisctl knowledge`）。

### D6. 交換可能な部品

- 検索の実装（今は grep）・LangMem・将来の埋め込み索引（Vector DB）は `knowledge::Index` trait の後ろに置く。正本のファイル形式は変えない。
- 埋め込み検索が要るときは `index.json` の隣に `index.<impl>/` を派生物として作る（正本と混ぜない）。

## 2. 採らない（初期）

- Knowledge Graph、Vector DB、時系列 Knowledge、大規模 Semantic Search。
- すべての会話の保存。再利用価値があるものだけ（D4）。
- LangMem を正本にする。正本はファイル。

## 3. 受け入れ条件

**Phase 61（D1〜D3、D5 の画面。LangMem 無し）**: `~/knowledge` の初期化（git、雛形の `user/profile.md` など）、front matter と `index.json`、
`celerisctl knowledge search|get|record|reindex`（tempdir の KB でテスト）、API 5 本と管理系の認可、profile の `knowledge` マウントの実効化と
前置きの索引、コンテナへのマウント、GUI「知識」画面と `_inbox`。実機: 自己改善案件のタスクの前置きに索引が出て、ワーカーが `search` で
`environment/clusters/pegasus.md` を引ける。

**Phase 62（D4）**: `langmem` アダプタと `tools/langmem`、知識整理 run のトリガ、候補の適用規則（high は直接、他は `_inbox`）、秘密の検査、
手帳の昇格、タイムラインの表示。実機: 1 タスクの終端から候補ができ、high が KB にコミットされ、GUI で読める。

---

## Phase 61 追記（2026-09-20。D1〜D3 と D5 の画面を実装したときの逸脱と細部）

実装は ADR の決定どおり。**決定を変えた点は無い**。書いていなかった細部と、あえて別のやり方にした点だけを残す。

### 決めた細部（ADR が書いていなかったこと）

- **P-61-a: `index.json` はバージョン管理に入れない。** D1 が「再生成できる派生物」と書いているので、`init` が
  `.gitignore` に `index.json` と `index.*/`（D6 の将来の埋め込み索引）を書く。こうしないと、ページを 1 枚直すたびに
  索引の差分が同じコミットに混ざって履歴が読めなくなる。
- **P-61-b: `etag` はページの中身の sha256。** 文書（ADR-0044 D7）は blob sha を使うが、KB は**正本が作業ツリー
  そのもの**なので、まだコミットされていない人の編集にも etag が要る。GUI から見れば opaque な文字列なので、
  型は同じまま。
- **P-61-c: 書き込みは一時 worktree を使わず、作業ツリーに書いてそのパスだけをコミットする。**
  文書（ADR-0044 D7）が一時 worktree を使うのは「人のチェックアウトが編集中かもしれない共有リポジトリ」だから。
  KB は人も celeris も同じ 1 本の作業ツリーを見るので、`default_branch_busy` の概念が無い（「人が編集中」は
  そのまま次に読む内容になる）。
- **P-61-d: front matter に `_inbox` 専用の鍵 `path` を足した。** D3 の `record` は取り込み先を書けると便利で、
  D4 の候補（`{op, path, …}`）とも形が揃う。accept のときに落とすので、正本のページには残らない。
  書かなければ `scope` と題名から `<scope のディレクトリ>/<slug>.md` を当てる。
- **P-61-e: 検索の `--scope` は「KB の相対パスの接頭辞」と「front matter の `scope` の値」の両方に当たる。**
  D2 のマウントの `scope` はパス（`environment/clusters`）、D1 の front matter の `scope` はラベル
  （`project:pluvio`）で、語が同じなのに指すものが違う。両方受けるのがいちばん驚きが少ない。
- **P-61-f: front matter の実装は `task_ops::docs` と共有せず、`task_core::knowledge` に別に書いた。**
  読む鍵が違い（`docs` は `title` / `tags` / `tasks`、KB は `title` / `tags` / `scope` / `sources` / `created` /
  `updated` / `confidence` / `path`）、KB は**書き戻し（往復）**が要る。`docs` 側は 1 バイトも変えていない。
  将来どちらかを直すときに一方だけ壊れないよう、両方に往復のテストを置いた。
- **P-61-g: `[knowledge]` は既定でも値を持つ（`root = "~/knowledge"`）。** ADR-0045 D2 の P-58-a
  （「`None` に意味がある設定に暗黙の既定を入れない」）とは逆に見えるが、ここは `None` に意味が無く、
  **celeris はこのディレクトリを一切作らない**（読み取りは `initialized: false` を返すだけ）。
  用意するのは `celerisctl knowledge init` だけ。
- **P-61-h: `default_mounts` の既定は `["kb:user", "kb:environment"]`。** ADR-0046 D7 の木がまだ `knowledge` を
  持たない（Phase 59）ので、その間の橋渡し。Phase 59 が入ったら実効 profile の `knowledge` と和を取る。

### D4（Phase 62）に先出しした部分

- **秘密の検査**（`task_core::knowledge::secret_finding`）は Phase 61 で入れた。`record` が拒否するので、
  ワーカーが書く候補にはこの時点から効く。Phase 62 の「適用前の検査」も同じ関数を使う。
- **`Celeris (knowledge)` の author** は `record` の候補にだけ使っている。D4 の「high を直接コミット」は Phase 62。

### やっていないこと（ADR のとおり Phase 62）

- LangMem（`tools/langmem`・`langmem` アダプタ・知識整理 run のトリガ・候補の適用規則・手帳の昇格）
- タスクのタイムライン（ADR-0044 D5）への「この仕事から知識 N 件」の表示

---

## Phase 62 追記（2026-09-20。D4 = LangMem を実装したときの逸脱と細部）

実装は ADR の決定どおり。**決定を変えた点は無い**。Phase 61 の 3 つの提案（P-61-i〜k）に答え、
書いていなかった細部を残す。

### 提案への答え

- **P-61-i（daemon の起動時 reindex）**: 採用した。celeris の起動時（`--mode verify` を除く）に KB が
  あれば `ensure_index` を 1 回呼ぶ（`crates/celeris/src/lib.rs`）。`_inbox` の変化を tick ごとに見る
  仕組み（inotify・mtime 比較）はまだ無いが、知識整理 run の適用（`apply_candidates`）が毎回
  `reindex` するので、有効にしている環境では実質的に索引が古くなり続けることはない。
- **P-61-j（`[knowledge] default_mounts` の既定を Phase 59 後に `[]` へ）**: **見送り**。ADR-0046 D7 の
  木はまだ `knowledge` を明示的に持たない（Phase 59 のノードの profile を見ても `knowledge` フィールドは
  空）ので、`default_mounts` は今も唯一の実効マウントの出所。Phase 62 の範囲外（今回の Phase の
  受け入れ条件でもない）。次に組織側へ `knowledge` を足す Phase が判断すること。
- **P-61-k（`DELETE /knowledge/page` か retire-only か）**: **retire-only に決めた**。新しい API
  エンドポイントは足さない。「ページを捨てる」は `op: retire`（知識整理 run が提案する、または
  人が `celerisctl knowledge record` で候補を作って手で accept する）の 1 本に統一し、
  accept が対象ページを `_retired/<同じ相対パス>` へ動かしてコミットする（`task_ops::knowledge::
  RETIRED_DIR`）。`_retired/` は `_inbox/` と同じく索引にも検索にも出ない。理由: `DELETE` は
  「消えて終わり」だが、KB は git が正本なので実は `git rm` と同じことしかできない。それなら
  「今の場所から動かして、人の判断が要ったことを front matter の `op` に残す」retire の方が、
  「なぜ消えたか」を後から追える（`_retired/` のページ自体に理由を書ける。`git log` も残る）。
  ページを直接消したい人はエディタ + `git rm` + `celerisctl knowledge reindex`（Phase 61 のまま）。

### 決めた細部（ADR が書いていなかったこと）

- **P-62-a: `knowledge` harness は `[[harnesses]]`/`[[genres]]` に射影されない組み込みのまま**。
  ADR-0046 D3（`Config::project_harnesses`）は「組み込みの `plan`/`reviewer`/`smoke` はタスクの
  `genre` として使わない（計画 run の『使える分野』に混ぜない）」と決めている。`knowledge` も同じ
  理由でこの規則に従わせた。そのため知識整理タスクの生成（`crates/celeris/src/knowledge_maint.rs`）は
  役割・分野の解決に頼らず、`NewTaskSpec.adapter = Some("langmem")` / `tier = Some(Tier::Cheap)` を
  **直接指定**する（`role` は `task_core::report::KNOWLEDGE_ROLE`（= `"knowledge"`）を印として
  付けるだけで、これが `[[roles]]` に無くても落ちない。`report-compressor` と同じ扱い）。
- **P-62-b: 知識整理タスクの担当ノードは「元のタスクの担当」**。ADR の「その案件・ノードに対して
  知識整理 run」の「ノード」を、元のタスクの `assignee` と読んだ（自分の仕事を自分で棚卸しする形）。
  担当が無いタスク（`assignee = None`）は対象外。
- **P-62-c: 知識整理 run の「適用」は、まとめの run（ADR-0033 D3）と違って `task-dispatch` に手を
  入れず、celeris の tick 側だけで完結させた**。`task_ops::knowledge::apply_candidates` の呼び出しと
  `knowledge_runs` の状態遷移は `crates/celeris/src/knowledge_maint.rs::apply_finished`
  （tick から `schedule` の直後に呼ぶ）が行う: `knowledge_runs` が `scheduled` のまま、その
  run のタスクが終端になったものを見つけ、`done` なら `artifacts/knowledge-candidates.json`
  （`task_core::artifacts::artifacts_dir_for` で決定的に場所を計算。知識整理タスクは既定の
  `workspace: None` → 自分の workspace を所有するので `<workspace_root>/<task_id>/artifacts/`）を
  読んで適用、`failed`/`cancelled` なら候補を読まず `failed` にする。まとめの run（`report-compressor`）
  は「ディスパッチャが `done` を親の報告にする」という**既存の別経路**を使っているが、知識整理は
  そのような既存の合流点が無いので、Phase 25 の圧縮と同じ「tick が同期でストアと結果ファイルを見る」
  形を素直に踏襲した。結果として、知識整理 run 自身の「done/failed」は**通常のタスク終端の報告**
  （担当ノードの通常の報告）としても 1 件残る（ADR は禁止していないので、特別扱いはしていない）。
- **P-62-d: `result_summary`（ADR-0047 D4「report・result.json summary」の後者）は依頼文に別枠で
  入れていない**。終端になったタスクの報告本文（`report.body`）は、そのタスクの run が `done` の
  ときに `report::report_for_done` が `summary`（result.json の値そのもの）＋証拠＋成果物一覧から
  組み立てたものなので、`result.json` の `summary` は実質的に `report.body` に既に入っている。
  二重に持たせず `report_headline`/`report_body` だけを渡す（`MaintenanceInput.result_summary` は
  フィールドとして残してあるので、将来 result.json を別途読みたくなったときに埋める先はある）。
- **P-62-e: Console の `knowledge` ブロックと Timeline の `knowledge` 項目の形を、Phase 60a が
  予約した形（`entry_id`/`title`/`state`。KB の 1 ページの状態変化を指す形）から書き換えた**。
  D5 の要求（「この仕事から知識 N 件が候補になった／取り込まれた」）はタスク単位の集計であって
  KB ページ単位ではないため、`task_id`/`task_title`/`run_task_id`/`state`
  （`applied`/`failed`。`scheduled` は Console には出さず、Timeline には出す）/`ingested`/`inbox`/
  `discarded` を持つ形にした（`docs/api/v1/api-v1.schema.json` を `UPDATE_SCHEMA=1` で作り直し済み）。
- **P-62-f: `langmem` の python 依存が無い（venv 未セットアップ）は `retryable = false`**。
  ランナーが `import langmem` に失敗すると `CELERIS_LANGMEM_MISSING` を stderr に出して exit 1 し、
  アダプタ（`crates/task-worker/src/langmem.rs`）がこれを見て非再試行のエラーにする（ADR-0047 D4
  「if langmem is not importable, the runner exits with a clear error (retryable=false)」）。
  再試行しても直らない設定の誤りだからで、他アダプタの「供給側の失敗」分類（`classify_provider_failure`）
  とは別の、この 1 パターンだけの決定的な検査。

### 実機（まだやっていない。ADR-0009 P-34）

- `scripts/knowledge/setup-langmem.sh` を実行して venv を作る（ネットワークに出るので、このセッションからは
  行っていない）。
- `[adapters.langmem]` / `[knowledge.langmem]` / `[[providers]] adapter = "langmem"` を本番の
  `config.toml` に足し、celeris を再起動する（設定の再読み込みでは拾えない節が混ざる）。
- 終端になったタスクを 1 本用意し、知識整理 run が起き、`confidence: high` の候補が知識ベースへ
  コミットされ、GUI の Console/タイムライン/`_inbox` から見えることを確認する。
- 詳しい手順は `docs/PROGRESS.md` の「Phase 62」節の「本番の手順」を参照。
