# ADR-0033: 組織・案件・報告 — `docs/SPEC.md` を今の taskd の上に載せる

- 日付: 2026-09-17（夜）
- 状態: **Accepted**（人間の指示「SPEC.md を元にして、同じ工程・判断・subagent 呼び出しで長時間作業してみて下さい」）
- 関連: `docs/SPEC.md`（**この ADR の上位文書**。矛盾したら SPEC が正）、ADR-0027/0028（分野＝ハーネスの束）、
  ADR-0010（Question と answers）、ADR-0023（委譲）、ADR-0016（役割）、ADR-0018/0032（クラスタ）
- `docs/DESIGN.md` との関係: DESIGN §0–§1 の「タスク管理層」という枠組みは SPEC に置き換わる。
  ただし DESIGN §1 の**原則 1〜3（協調判断に LLM を使わない・状態は外に置く・ワーカーは交換可能）は生きる**。
  DESIGN.md 自体は編集しない（CLAUDE.md の規約）。扱いは PROGRESS の提案に書く。

## 1. 文脈

SPEC.md が言うものは、今の taskd が持っていない概念を 5 つ要求する。

| SPEC の言葉 | 今の taskd にあるか |
|---|---|
| 秘書 / 組織（一つ、役割の木、各ノードは長期記憶を持つ「人」） | 無い。`roles` は名札、`genres` はハーネスの束で、人ではない |
| 案件（曖昧な依頼 → 途中目標 → 分解 → 実行。アジャイル） | 無い。`plan` タスクが一段だけ分解する |
| 報告（上に行くほどレビューと圧縮。良い知らせも悪い知らせも。通知は数時間単位） | 無い。`inbox` は「失敗した」等の機械的な注意だけ |
| 認可（迷ったら聞く。今回だけ / 今後ずっと。永続分は記録して注入） | 半分。`Question` → `answers` は「今回だけ」しか無い |
| 口出し（組織の木で相手を選び、その「人」に直接話す。先週の続きとして） | 無い |

一方、SPEC §8 で「流用する」と決まったものは、今の taskd にそのまま**ある**: 仕事の木（`tasks` の `parent_id` / `depends_on`）、
分野ごとのハーネス（`genres` + アダプタ）、クラスタ接続（ADR-0018/0032）、アカウント切り替えと予算の観測（ADR-0024/0025）。
SPEC §4 は「サーバ側は今の CLI の実装を流用してどうにかする」と言っている。

**したがってこの ADR は「作り直し」ではなく「載せ替え」**: 既存の `tasks` を**実行の基盤**として残し、その上に
組織・案件・報告・認可・対話を**新しい第一級の概念**として足す。人が見る単位は案件と組織になり、タスクは裏方に下がる。

## 2. 決定

### D1. 組織は DB の第一級エンティティ（`org_nodes`）。設定から種を蒔き、GUI で編集する

```
org_nodes(id TEXT PK, parent_id TEXT NULL, name TEXT, kind TEXT, genre TEXT NULL, brief TEXT,
          position INTEGER, created_at, updated_at)
```

- `kind`: `secretary`（根。1 つだけ）/ `department`（部）/ `section`（課）。
- `genre`: その「人」が仕事をするときに使うハーネスの束（ADR-0027/0028 の `[[genres]]` の id）。
  SPEC §3.2 追記「全ての人を単一のハーネスで表現するのではなく用途に応じてハーネスを使い分ける」の実装。
  `department` は `genre` を持たなくてよい（課に振る）。`secretary` は対話用の分野（D4）を持つ。
- `brief`: 担当の一言（SPEC の組織図の言葉そのまま）。プロンプトに前置きされる。
- 初期の形は SPEC §3.2 の組織図。`config/org.example.toml` に `[[org]]` として書き、**DB が空のときだけ**種を蒔く
  （以後の編集は GUI → API → DB。設定は再読込しない。ADR-0024 の accounts と同じ「DB が正」）。
- 役職を足す・分ける・消すは API（`POST/PATCH/DELETE /org/{id}`）。消すときに仕事を抱えていたら 409。

### D2. 案件（`projects`）と途中目標（`milestones`）。タスクは案件に属し、組織のノードに割り当てられる

```
projects(id ULID PK, title, request TEXT, status TEXT, secretary_summary TEXT NULL, created_at, updated_at)
milestones(id ULID PK, project_id, seq INTEGER, title, description, status TEXT, created_at, updated_at)
tasks += project_id TEXT NULL, milestone_id TEXT NULL, assignee TEXT NULL   -- assignee = org_nodes.id
```

- `projects.status`: `proposed`（秘書が理解確認と方針を出し、人の返事待ち）/ `active` / `paused` / `done`。
- `milestones.status`: `proposed` / `approved` / `in_progress` / `reached` / `redesigned`。
  **SPEC §7 のアジャイル**: 途中目標の達成ごとに人が判定し、Go を出すか再設計する。
- 案件の仕事の木 = `tasks WHERE project_id = ?`（既存の `parent_id` / `depends_on` がそのまま DAG）。
- `assignee` が無いタスクは従来どおり `worker_hint` で配る（互換）。`assignee` があれば、そのノードの `genre` から
  役割・分野を解決する。
- **優先順（Phase 23 の監査 D-2 で確定）**: `assignee` は「誰の仕事か」であって「どうやるか」ではない。
  **タスクが明示した `role` が勝ち**、`assignee` 由来の既定（genre → `default_role` → tier / adapter / 予算）は
  **`role` が無いときだけ**埋める。つまり解決順は task > role > **assignee** > genre.default_role > parent。
  ADR-0016 D1「タスクの値 > 役割の既定」に揃える。`role` がその `assignee` の分野に属さなければ従来の整合検証（422）。
- ディスパッチャが作る派生タスク（`Approval`、合成 `Review`）は親の `project_id` / `milestone_id` を**必ず継ぐ**
  （案件の仕事の木から子が消えないように。監査 D-3）。

### D3. 報告（`reports`）は下から上へ。各段で親がレビューして圧縮する

```
reports(id ULID PK, project_id, node_id, task_id NULL, kind TEXT, level INTEGER,
        headline TEXT, body TEXT, sources TEXT /* JSON: 元になった report id の配列 */,
        read_at NULL, created_at)
```

- `kind`: `progress` / `result` / `bad_news` / `proposal`（「この framing で論文が書けそう」）/ `question`。
- **生成**: タスクの run が終わるたび（`Done` / `Error` / `Question`）に、担当ノードの報告を 1 件作る
  （**LLM ではなく決定的に**: 結果ファイルの `summary` と `evidence`、成果物の一覧から組む。DESIGN 原則 1）。
- **圧縮**: 親ノードは、子から上がった未処理の報告が溜まると（既定: 4 件、または最古が 2 時間経過）
  **レビュー用の run** を 1 回起こし、子の報告を 1 件にまとめて自分の報告にする（`sources` に元の id）。
  これは LLM の仕事（レビュアー役）で、`Check::Reviewer` と同じ「別 run が中身を判定する」形。
  秘書まで上がった報告が**人の見る報告**。
- **悪い知らせは圧縮を待たない**: `bad_news` は各段を素通りして即座に秘書まで上がる（SPEC §2.4）。
- **通知**: GUI は秘書レベルの未読報告を「報告の流れ」として出す。**数時間単位**（SPEC §3.5）なので、
  ブラウザ通知は「前回の通知から 2 時間以上経ち、未読があるとき」だけ（`bad_news` は即時）。

### D4. 対話（`conversations` / `messages`）— 秘書にも、どの「人」にも話せる

```
messages(id ULID PK, node_id, project_id NULL, role TEXT /* user | node */, text TEXT, run_id NULL,
         created_at, task_id NULL /* Phase 27 / migration 0007: 1 往復を起こした対話用タスク */)
```

- 人がノードに話しかける → **対話用分野**（設定 `[conversation] genre`。既定 `secretary`）で run を
  1 回起こす。入力は: ノードの `brief`、ノードの記憶（D6）、この案件の直近のやり取り、そして本文。
  出力は返事（`artifacts/result.json` の `summary` を返事にする。**新しいプロトコルは足さない**）。
- **Phase 30 追記（実機の事故、2026-09-18）**: 当初は「ノードの `genre` の対話用ハーネスで run する」
  としていたが、人が**関連研究調査課**（`genre = web-research` = Local Deep Research）に話しかけたところ、
  検索ハーネスが会話しようとして web 検索を実行し、0 件 → 証拠ゲートで `failed` になった。
  検索ハーネスや PaperQA は会話ができない。**対話は常にノードの `genre` に関係なく対話用分野で走る**
  （`genre` は「仕事をするときのハーネス」であって「話すときのハーネス」ではない）ことに改めた。
  「人」らしさは、担当ノードが自分の仕事の分野を持てば前置きに「仕事で使う道具」として 1 行渡すだけで保つ。
- **秘書の最初の仕事**（SPEC §7）: 案件の依頼文を受けたら、返事に **(a) 案件の理解の確認 (b) 大まかな方針
  (c) 最初の途中目標の提案** を含める。これは秘書の `brief` と分野の指示文で作る（プロンプトの仕事。コードは
  「返事を `messages` に入れ、`projects.status = proposed` にする」だけ）。
- 人が「それで進めて」と言う → 秘書が**計画の run**（既存の `plan` の仕組み）を起こし、
  `PlanOutput.tasks[]` に **`assignee`**（組織のノード id）を含めて出す。分野の manifest（ADR-0028）と組織図を
  プロンプトに渡し、「どの課に何を振るか」を秘書に決めさせる。ディスパッチはこれまでどおり決定的。
- **部をまたぐ連携は秘書が認める**（SPEC §3.1）: 課のタスクが別の部の課へ委譲（`delegate.json`）しようとしたら、
  同じ部の中でなければ**自動では受けず、秘書への `question` にする**（D5 の認可の一種）。
  Phase 27（監査 H-1 / H-2）で認可に接続した: 質問は `approvals` の 1 行として
  **`"cross-department: <from_node> -> <to_node>: <理由>"`** の固定の形にし（`node_id` = 委譲元、
  `task_id` = 親タスク）、委譲のたびに `approvals`（同じタスク・同じ鍵の決定）と `standing_rules`
  （鍵を先頭に含む規則）を**前方一致で**引いて、`once` / `standing` なら通し、`denied` なら通さない。
  **バッチは分ける**: 同じ部宛ての提案はその場で子にし、部またぎの提案だけを質問にする。
- **対話 run は返事だけ（委譲・Question 不可）**（Phase 28。実機の一本目で秘書が返事の代わりに委譲し、
  子の失敗で `blocked` に落ちた事故から確定）。対話 run（`task.conversation.is_some()`）は
  `delegate.json` を受け付けず（子を作らず、理由を `progress` で返す）、`Question` 終端も出さない
  （そのまま `Done` の返事になる。人に聞きたいことは返事の本文に書く）。**作業（委譲・実装・調査・多ターンの
  分解）は、人が方針と途中目標を承認してから、上の「秘書が計画の run を起こす」経路で始まる**。対話タスクの
  `depends_on`（直列化）は順番だけを守り、前が失敗しても後続を巻き込まない（P-78。`DependencyFailed` を
  対話タスクだけ免除）。予算は `max_turns` を 6 → 10 に上げた（読むだけでも数ターン使うため）。
- **分解は人が `POST /projects/{id}/plan` で起こす**（Phase 29。GUI の「この方針で進める」ボタンの入口。
  GUI 監査で「案件が分解されて組織を流れる、を GUI から起動も観察もできない」と判定されたため）。
  対話 run は返事だけ（上の項目）なので、人が秘書の返事を読んで方針に納得したら、この API で明示的に
  分解を起こす。案件の `request`、途中目標（`approved` / `in_progress` のもの。`milestone_id` を指定すれば
  それ）、人の一言（`note`）、秘書との直近のやり取り（`messages` 最大 20 件）を 1 つの `goal` にまとめ、
  既存の `kind = plan` の仕組み（`task_ops::add::create_support_task`）にそのまま渡すだけ（新しいタスクの
  種類は作らない）。`assignee = secretary`、`role` / `genre` は秘書の分野から解決する。案件が `proposed`
  なら `active` に、指定した途中目標があれば `in_progress` にする。プランナーの出力（`PlanOutput.tasks[]`）
  から作る子は、上の「秘書が計画の run を起こす」と同じ経路（`materialize` が親の `project_id` /
  `milestone_id` を継ぎ、`assignee` はこの run のプロンプト＝組織図と manifest から秘書が振る）。

### D5. 認可（`approvals`）— 「今回だけ」と「今後ずっと」

```
approvals(id ULID PK, project_id NULL, node_id, task_id NULL, question TEXT, decision TEXT NULL
          /* once | standing | denied */, answer TEXT NULL, created_at, decided_at NULL)
standing_rules(id ULID PK, node_id NULL /* NULL = 全員 */, rule TEXT, created_at)
```

- 既存の `Question` 終端（ADR-0010）を `approvals` に接続する: ワーカーの `question` は `approvals` の 1 行になり、
  人が `once` で答えれば従来どおり `answers[]` として次の run に渡る。`standing` で答えれば、
  **答えを `standing_rules` に 1 行追加**し、以後そのノード（または全員）の run のプロンプトに常に前置きされる
  （SPEC §3.6「永続の認可は文字で記録してエージェントに注入する」）。
  例外は**部をまたぐ委譲の質問**（D4 の最終項）で、`standing` のときは答えの文ではなく**質問の鍵**
  （`"cross-department: <from> -> <to>"`）をそのまま規則にする（委譲の照合が前方一致でできるように。Phase 27）。
- 自動判定・自動化は**今回はやらない**（規則がまだ無い。溜まってから ADR にする）。

### D6. 記憶（ノードごとのファイル）— 案件をまたぐ

```
<memory_dir>/<node_id>/notes.md               -- 案件をまたぐ記憶（クラスタの使い方、人の興味、直近の相談）
<memory_dir>/<node_id>/projects/<project_id>.md -- 案件の引き出し
```

- run の前にプロンプトへ**前置き**する（`notes.md` 全文 + その案件の引き出し）。上限は文字数で切る（既定 8,000 字ずつ）。
- run の後、結果ファイルに `memory` があれば追記する: `{"memory": {"notes": ["…"], "project": ["…"]}}`
  （結果ファイル規約の拡張。ワーカーが「覚えておくべきこと」を箇条書きで返す。**LLM に書かせるのはここだけ**）。
- DESIGN 原則 2「状態はエージェントの外に置く」は守られる: ハーネスのプロセスは相変わらずステートレスで、
  記憶はファイルにあり、注入されるだけ。「人」らしさは**注入される記憶と brief** で作る。
- **検索ハーネス（`local-deep-research`）は例外**: 問いを濁さないため、記憶・やり取り・役職・`memory` の
  書式指示を**一切載せない**（渡すのは素の `objective` だけ。ADR-0029 / Phase 19 の実機の回帰）。
  記憶の追記もしない（書式を教えていないので書いてこない）。Phase 27 の監査 M-2 で確定。

### D7. LLM source（SPEC §8 追記）は、今回は**名前だけ**揃える

SPEC は「要求する能力と登録済みアカウントの予算のバランスで LLM source を振り分ける」「Qwen をいろんなハーネスに注入する」
と言う。今の `providers` + `account_pool` + `tiers` がその 7 割で、ADR-0026 の ACP はローカル Qwen を既に注入している。
**今回は概念の名前を GUI で「LLM source」に寄せるだけ**にし、能力ベースの振り分けは別 ADR にする（先に組織と案件を通す）。

### D8. GUI は案件と組織を中心に組み直す（別項目 G13）

SPEC §4 の 6 画面。既存の「タスク」画面は残すが、ナビの末尾に下げる（裏方）。

## 3. 採らない

- `tasks` を捨てて新しい実行基盤を書く。SPEC §4/§8 が流用を指示している。
- 報告の生成を LLM にやらせる。生成は決定的、**圧縮だけ** LLM（原則 1）。
- 組織を設定ファイルで管理し続ける。GUI で編集する以上、DB が正。
- 認可の自動判定。規則が溜まる前に作ると当てずっぽうになる。

## 4. Phase の切り方（この夜にやる順）

| Phase | 内容 | 主担当 |
|---|---|---|
| 23 | D1 + D2: `org_nodes` / `projects` / `milestones` / `tasks` の列追加、migration 0006、API、`assignee` の解決 | Opus |
| 24 | D4 + D6: 対話（messages）と記憶の注入・追記、秘書の最初の返事、`PlanOutput.assignee` | Opus |
| 25 | D3: 報告の生成と圧縮、`bad_news` の素通り、秘書レベルの未読 | Opus |
| 26 | D5: 認可（once / standing）と `standing_rules` の注入 | Sonnet |
| G13 | D8: 秘書との対話 / 組織の木 / 案件と仕事の木 / 報告の流れ / 認可 / 成果物 | Opus |
| 実機 | 最初の一本（SPEC §6）を、ローカル Qwen で秘書の最初の返事まで通す | — |

各 Phase の受け入れ条件はその Phase の実装依頼に書く。共通条件は従来どおり
（`cargo test --workspace` / clippy / GUI 一式 / PROGRESS / commit）。
