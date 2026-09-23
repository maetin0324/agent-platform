# ADR-0046: 組織は Agent Profile の継承木 — skill はタグ、harness は組織と 1 対 1 にしない、Chief of Staff が根

- 日付: 2026-09-20
- 状態: **Accepted**（人間の方針 2026-09-20。外部からの助言「組織を持つ意味は人間の会社の模倣ではなく、タスクに『どの知識・ハーネス・
  ツール・実行方針を使うか』を継承させること。木は文化・知識・権限・既定値、タグは能力。秘書は control plane」を人が採用。
  人の回答: CoS は根ノード、matching は決定的、木の名前は今のものを捨てて英語、mode は prototype / production / research、
  tools は提案の範囲）
- 関連: SPEC §3（組織）、ADR-0033（組織・案件・報告。D2 の優先順位 task > role > assignee > genre、D4 対話）、ADR-0028（分野 = 能力
  レジストリ）、ADR-0016（役割と委譲）、ADR-0043（ワークスペース）、ADR-0044（タスク管理）、ADR-0047（知識）、ADR-0048（Console）

## 1. 文脈

今の木は「フロントエンド」「パフォーマンス」（専門領域）、「PoC」（進め方）、`web-research` / `coding`（実行ハーネス）が同じ階層に
混ざり、ノードは `genre` 1 つと `brief` しか持たない。だから何を足しても「ふわっと」する。組織ノードが**継承させるもの**を持ち、
能力はタグ、進め方は mode、ハーネスは許可リスト、にすると木の意味がはっきりする。Kubernetes の対応で言えば、案件 = workload、
task = Job、worker = Pod、組織 = Namespace + policy + knowledge、skill = label、dispatcher = scheduler、harness = runtime、
knowledge = volume。

## 2. 決定

### D1. ノードは profile を持ち、子は親を継ぐ

`org_nodes.profile_json`（migration `0016_org_profiles.sql`。`SCHEMA_VERSION = 16`）:

```toml
skills     = ["hpc", "storage", "rdma", "benchmark"]                # 能力タグ。親と和
knowledge  = [ { kind = "kb", scope = "environment/clusters" },       # ADR-0047 の知識のマウント。親と和
               { kind = "repo", name = "pluvio", docs = "docs" },
               { kind = "memory" } ]                                  # このノードの長期記憶（今の notes.md）
harnesses  = { allowed = ["coding", "literature", "web-research"], default = "literature" }   # allowed は親と和、default は子が勝つ
tools      = ["cluster:pegasus", "cluster:sirius", "cluster:fern03", "gh", "tavily", "exa", "docker"]   # 親と和。deny_tools は常に勝つ
run        = "host"                                                  # "host" | "container"。子が勝つ
model      = { tier = "standard", allowed_tiers = ["frontier", "standard", "cheap"] }   # tier は子が勝つ、allowed は交わり
policy     = ["主張には根拠（論文か計測）を付ける"]                    # 根→葉の順に連結
review     = { harness = "reviewer", tier = "cheap" }                # 子が勝つ
permissions = { approvals = ["cluster-write", "external-post"] }     # once / standing の対象。親と和
```

- **実効 profile** `EffectiveProfile` = 根から葉まで上の規則で merge し、最後に**タスクの上書き**（`tier` / `harness` / `run` / `repos` /
  `skills`）。ADR-0033 D2 の「task > role > assignee > genre」はこれに置き換わる（`role` と `genre` は D3 で harness に吸収）。
- 計算は `task_core::profile::resolve(nodes, node_id) -> EffectiveProfile`（純粋関数。テスト容易）。前置きは実効 profile から作る:
  根→葉の `brief`（「文化」）、`policy` の箇条書き、`skills`、`tools`、mode の規則（D4）、知識の索引（ADR-0047）。
- `brief` と長期記憶（`memory/<node>/`）はノードに残る（「誰が何を知っているか」が組織そのもの）。

### D2. skill はタグ。組織にしない

- `frontend` / `react` / `rust` / `ucx` / `io_uring` / `benchmark` / `paper-writing` … は skill。ノードは「持っている skill」、タスクは
  「必要な skill」（`tasks.skills_json`。migration 0016）。計画 run（ADR-0028）は子タスクごとに `skills` を宣言する。
- **組織にする判定は機械的**: そのノード以下に共通で継承させたい knowledge / harness / tools / policy / 実行環境 / 権限 / 品質基準の
  どれかがあるときだけ。「役割が違う」だけなら skill。PoC・R&D 課は消える（D4 の mode）。

### D3. harness は実行契約。組織と 1 対 1 にしない

- 今の `[[genres]]`（能力・入出力の契約・対話用か）と `[[roles]]`（adapter・tier・指示文・予算）を **`[[harnesses]]`** に統合する:

```toml
[[harnesses]]
id = "coding"
description = "コードを書く・直す・テストする"
adapter = "claude-code"
tier = "standard"                     # 既定。ノード・タスクが上書き
instructions = "あなたは実装担当。…"
input_artifacts = ["issue description", "repository"]
output_artifacts = ["diff", "test results"]
budget = { max_turns = 60, max_wall_secs = 3600, max_retries = 1 }
conversation = false                  # 対話用ハーネス（今の secretary genre）は true
```

- 組み込みで要るもの: `conversation`（対話。今の `secretary` genre + role）、`plan`（計画）、`reviewer`（レビュー）、`smoke`（verify）。
- **設定の互換**: 読み込み側は旧い `[[genres]]` + `[[roles]]` を受け取り、決定的に `[[harnesses]]` へ写す（genre.id をそのまま
  harness id、`default_role` の adapter / tier / instructions / budget を取り込む。`default_role` を持たない role は「同名 harness の
  上書き」として残す）。warn を 1 行出す。`celerisctl config to-harnesses` が新しい形の設定を書き出す（人が差し替える）。
- タスクの `harness`（`tasks.genre` 列をそのまま harness id として使う。`role` 列は互換のため残し、書かない）。
- ノードは `harnesses.allowed` に無い harness のタスクを受けられない（matching の候補から外れる。明示の `assignee` なら 422）。

### D4. mode はタスクの属性

`tasks.mode`（migration 0016。`prototype` | `production` | `research`。既定 `production`。計画 run が決める、人が編集できる）:

| mode | 前置きに足す規則 | レビュー |
|---|---|---|
| `prototype` | 「動くことを最短で示す。テストは動作確認の最小限でよい。捨てる前提で書く。結論と次の一手を summary に」 | 明示の `acceptance` だけ。リポジトリの `check` は使わない |
| `production` | 「既存のテストと lint を通す。変更は小さく、理由をコミットに書く」 | `acceptance` ＋ リポジトリの `check`（ADR-0043 D4） |
| `research` | 「主張には出典か計測を付ける。数値は再現手順と一緒に。採らなかった案と理由も残す」 | `acceptance` ＋ 結果に `sources`（または計測の記録）が無ければ不合格 |

### D5. 担当の選び方は決定的（capability matching）

- `assignee` が無いタスク（計画 run の子、Console から作られたタスク）は、ディスパッチャの前段 `task_ops::matching::assign` が決める:
  候補 = 実効 profile の `harnesses.allowed` にそのタスクの harness を含む**葉と中間のノード全部**（根は除く）。
  スコア = |タスクの skills ∩ ノードの実効 skills|。最大スコアのノード。同点は**浅い方**、さらに同点は id の辞書順。
  タスクの skills が空なら「その harness を `default` に持つノード」を優先し、無ければ allowed を持つ最も浅いノード。
  候補が無ければ `blocked` にして人に聞く（「担当が見つからない: harness X / skills …」。ADR-0021 の質問経路）。
- CoS（計画 run・Console）が `assignee` を明示すれば従う（allowed に無ければ 422 で差し戻し）。**LLM が組織図を読んで人選する経路は
  無くす**（今の「秘書が部署を選ぶ」文面は計画出力から削る）。
- 結果は `Event::Assigned { node, score, reason }` に残す。GUI のタスク画面に「なぜこの担当か」を出す。

### D6. Chief of Staff（CoS）は根ノード

- 根ノードの id は **`cos`**（表示名 Chief of Staff）。今の `secretary` は改名する（DB・記憶ディレクトリ・API のパス
  `/org/cos/messages`・GUI の `/org/cos`。`/`→秘書の導線は CoS へ）。
- CoS の profile: `harnesses.allowed = ["conversation", "plan"]`、`tools = []`、知識は `user/*` と `projects/*`（ADR-0047）。
  CoS は**組織の全ノードの profile を読める**（前置きに組織の一覧: id / 名前 / skills / harnesses）が、人選は D5 に委ねる。
- 役割: 人との対話（Console）、案件の理解確認と提案（ADR-0033）、分解（計画 run に skills / harness / mode / repos を書かせる）、
  進捗の統合と報告（ADR-0034）、途中目標レビュー（ADR-0038）、Console からの `actions`（ADR-0048 D3）。

### D7. 木の初期構成（英語。今の木は捨てる）

```
cos                      Chief of Staff
engineering              Engineering
  software-engineering   Software Engineering       skills: software, rust, typescript, react, sqlite, api      harnesses: coding
  systems-performance    Systems & Performance      skills: hpc, storage, rdma, io_uring, benchmark, perf      harnesses: coding, data-analysis
research                 Research                   knowledge: kb:user, kb:projects   policy: 「主張には根拠」
  literature-research    Literature Research        harnesses: literature (default), web-research
  web-research           Web Research               harnesses: web-research (default), literature
  experiment-data        Experiment & Data          harnesses: coding, data-analysis (default)      skills: benchmark, statistics, plotting
  scientific-writing     Scientific Writing         harnesses: writing (default), literature         skills: paper-writing, latex
operations               Operations                 tools: cluster:*, docker
  cluster-hpc            Cluster & HPC Operations   knowledge: kb:environment/clusters   skills: slurm, ssh, module
  infrastructure         Infrastructure             skills: systemd, docker, networking
  monitoring-automation  Monitoring & Automation    skills: monitoring, scripting
```

- 新しい harness `data-analysis` と `writing` は coding ハーネス（claude-code）の指示文違いとして定義する（adapter は同じ）。
- **既存ノードの写像**（決定的。`celerisctl org migrate-v2` が DB・記憶ディレクトリ・tasks.assignee・messages・reports・approvals・
  standing_rules の node id を書き換える。逆写像も書いて `--rollback` を持つ）:
  `secretary → cos`、`coding → engineering`、`coding.frontend → software-engineering`（skill `frontend` を profile に足す）、
  `coding.performance → systems-performance`、`coding.poc → software-engineering`（記憶は追記して統合）、`research → research`、
  `research.survey → literature-research`、`research.web → web-research`、`research.writing → scientific-writing`、
  `research.data → experiment-data`、`infra → infrastructure`（親 `operations` を新設）。`cluster-hpc` / `monitoring-automation` は新設。
- `config/org.example.toml` と本番の `org.toml` を新しい木で書き直す（DB が正本。`org.toml` は seed）。

### D8. tools と権限

- `tools` の語彙（今回）: `cluster:<id>`（`[[clusters]]` の id）、`gh`、`tavily`、`exa`、`docker`。実効 profile に無い道具は
  **前置きで禁止を明記し**、クラスタは `cluster:<id>` が無ければそのタスクからは接続経路を渡さない（ADR-0018 の remote を組まない）。
  秘密（`[secrets]`）は tools に対応する id だけをワーカーの env に渡す（tavily / exa）。
- `permissions.approvals` はそのノード以下で once / standing の対象になる操作の名前（ADR-0033 D5 の認可の分類に足す）。

## 3. 採らない

- 人間の会社を模した細かい組織図。ノードは D2 の判定で増やす。
- LLM による人選（D5）。
- harness と組織の 1 対 1（D3）。
- 今の `genre` / `role` 二層の維持（互換の読み込みだけ残す）。

## 4. 受け入れ条件（Phase 59 / G21）

1. `profile_json` と `EffectiveProfile`（merge 規則のテスト: 和・子勝ち・deny 勝ち・交わり・連結）。前置きに profile の節。
2. `[[harnesses]]` と旧 `[[genres]]`+`[[roles]]` の互換読み込み（テスト: 本番の `config.toml` 相当を読んで同じ harness 集合になる）。
   `celerisctl config to-harnesses`。組み込み harness（conversation / plan / reviewer / smoke）。
3. `tasks.skills` / `tasks.mode`、計画出力の `skills` / `mode`（`repos` と同様に検証）、mode の前置きとレビューの切替。
4. matching（テスト: スコア・同点の規則・候補なし → blocked・明示 assignee の 422）、`Event::Assigned`。
5. CoS = `cos`。`celerisctl org migrate-v2`（tempdir の DB と記憶で往復テスト）。新しい seed の `org.toml`。
6. GUI: 組織画面のノードに profile（skills / knowledge / harnesses / tools / run / model / policy / review / permissions）の表示と編集
   （`PATCH /org/{id}` に `profile`）、タスク画面に harness / skills / mode の表示と編集（B1 の編集フォームに追加）、「なぜこの担当か」。
7. 実機: 本番 DB を `migrate-v2` で写像し、`config.toml` を harnesses 形式に書き直し、自己改善案件の計画 run が skills / harness / mode を
   書き、matching で担当が決まる。

## Phase 59 追記（実装時の逸脱・明確化。2026-09-20）

前段の agent が実装したコードは `cargo test` を 1 度も走らせずに中断していた。実際にテストを通したところ
見つかった、本文に明示が無かった／実装が本文と食い違っていた点を以下に記録する（`docs/PROGRESS.md` の
Phase 59 節に証跡がある）。

- **D5 の matching は「組織を 1 つも作っていない構成」では走らせない**。`org_nodes` が空（`org_include` を
  書いていない・組織を使わない従来どおりの運用）のとき、`task_ops::matching::decide` は候補を探さずに
  `NotApplicable` を返す。本文は「候補が無ければ blocked」としか書いていなかったが、それだと ADR-0041 D5 の
  `smoke` 煙試験のような、組織を使わない既存の genre 付きタスクが**組織を作っていないだけで**軒並み
  `blocked` になってしまう（`crates/celeris/tests/instance_handoff.rs` の実機相当のテストで発覚）。
- **D1 の `harnesses.allowed` の和（親と和）は、根専用のつもりの harness（`conversation` / `plan`）も
  子へ継がせる**。`cos` だけが `conversation` を `allowed` に持つ構成でも、`engineering` 以下の実効
  `harnesses_allowed` には `conversation` が含まれる（和なので）。D5 の「根は候補から除く」規則はそのまま
  効くので、`conversation` 系のタスクが誤って子に配られることは無い（そもそも対話タスクは常に明示の
  `assignee` を持つので matching 自体を通らない）が、**根しか無い組織**（子が 1 つも無い）でなければ、
  「根だけが持つ harness」というものは実効的には存在しない。テスト（`task_ops::matching::the_root_is_never_a_candidate`）
  はこの前提で書き直した。
- **`celerisctl org migrate-v2 --rollback` は `--dry-run` 無しの本番と同じ並べ替え規律が要る**。
  `replace_org_nodes` は「親が先に来る並び」を要求するが、`--rollback` が読み戻す `backup.nodes`
  （移行前の `org_list()` のスナップショット）は `position ASC, id ASC` の並びで、`position` が全ノード
  同じ値の組織では id の辞書順になり、親（`secretary`）が子より後ろに来て失敗しうる。`migrate()` の
  `next.sort_by_key`（kind 順: secretary → department → section）と同じ並べ替えを `rollback()` にも適用する。
- D7 の「本番 `org.toml`」（`config/org.example.toml`）は D7 の木そのまま（`cos` 根、13 ノード）で確定。
  `celerisctl org migrate-v2` のテスト用 `[[harnesses]]`（`crates/celerisctl/tests/org_migrate_v2.rs`）も
  この 7 harness（`conversation` / `plan` / `coding` / `data-analysis` / `writing` / `literature` /
  `web-research`）に揃えた。
- GUI（D6 の「GUI の `/org/cos`。`/` → 秘書の導線は CoS へ」）は**今回は値の対応だけ**を直した
  （`SECRETARY_NODE_ID` を `"secretary"` → `"cos"`。`celerisctl org migrate-v2` 後の本番で `/org/secretary`
  が 404 になる実害を防ぐため）。URL パス・「秘書」という画面の言葉・ナビの全面改名は、GUI 側の 8 ファイルに
  またがる別 Phase として `docs/PROGRESS.md` の「提案 P-59-a」に送った。§4-6 の受け入れ条件（profile の
  表示・編集、harness/skills/mode の編集、「なぜこの担当か」）はこの Phase で満たしている。

## Phase 107 追記（2026-09-23）

D8 の「道具を 1 つも宣言していないノードは remote も従来どおり通す」という例外は、人間の指示により
クラスタの利用可否についてだけ廃止した（matching の候補フィルタ・`create_task` の検証・作業場所の
継承すべてで `cluster:<id>` を強制する）。詳細は ADR-0062 D5/D6。
