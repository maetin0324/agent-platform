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
