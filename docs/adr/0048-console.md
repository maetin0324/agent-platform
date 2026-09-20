# ADR-0048: Console — 全案件の流れが一本で見え、その場で指示できる画面。CoS の返事は「動く」

- 日付: 2026-09-20
- 状態: **Accepted**（人間の方針 2026-09-20: 「Claude Code のようにトップレベルでチャットの流れが見えつつ指示できる画面が無く、
  仕事が投げづらい」。回答: CoS なら全案件の流れが見え、下のノードも必要に応じて見に行ける。人の発言からタスクを直接作ってよく、
  即 ready でよい。ワーカーの進行は既定は折り畳み、必要なら詳細を見られること。アダプタごとに表示情報の取り方が違うのでは、という指摘）
- 関連: ADR-0046（CoS = 根、matching）、ADR-0033 D4（対話の規則）、ADR-0044 D2（コメントの割り込み）/ D5（タイムライン）、
  ADR-0038（途中目標の対話）、ADR-0037（通知）、`GET /stream`（SSE）

## 1. 決定

### D1. 一本の流れ（Console stream）

- `GET /console?scope=all|project:<id>|node:<id>&since=<cursor>&limit=` — 初期表示用の履歴（時刻順、カーソル付き）。
- `GET /console/stream?scope=…` — SSE。既存の `GET /stream` の上に、Console 用の**正規化したブロック**を流す:

| block | 中身 | 由来 |
|---|---|---|
| `human` | 人の発言 | `messages`（role user）、Console からの指示 |
| `reply` | CoS または部署ノードの返事（Markdown） | `messages`（role assistant） |
| `task` | タスクの開始・終了・失敗・中止・割り込み（1 行。担当・harness・tier・mode・経過） | `Event::Transitioned` |
| `progress` | ワーカーの進行（D2 の正規化）。**既定は折り畳み**（「Systems & Performance / coding が作業中 … tool 12 回」） | `Event::WorkerProgress` |
| `question` / `approval` | 質問・認可。**その場で答える入力欄とボタン** | ADR-0021 / ADR-0033 D5 |
| `milestone` | 途中目標の提案（ok / 議論 / ng をその場で） | ADR-0038 |
| `report` | 報告（見出し。開くと本文） | ADR-0034 |
| `knowledge` | 知識の候補が入った・取り込まれた | ADR-0047 |

- 範囲: **既定は全案件**（`all`）。案件・ノードで絞る。ノードの画面は同じ Console をそのノードのタスクと対話に絞ったもの（「必要に応じて
  下を見に行く」）。CoS の対話はどの範囲でも見える。
- 各ブロックに「返信」。返信先がタスクなら**そのタスクへのコメント**（ADR-0044 D2 の効き方: 走っていれば割り込む）、返事なら
  その対話の続き、質問なら回答。

### D2. ワーカーの進行の正規化（アダプタごとの差はここで吸収）

ワーカー・プロトコルの `progress` 行に任意の構造化フィールドを足す（`PROTOCOL_VERSION` は据え置き。無ければ従来の文字列）:

```json
{"type":"progress","msg":"…","kind":"tool_use","tool":"Bash","summary":"cargo test --workspace","detail":"…（省略可。4 KiB まで）"}
```

- `kind`: `tool_use` / `tool_result` / `text`（モデルの発話）/ `thinking`（あれば要約だけ）/ `status`（アダプタの節目）/ `comment`
  （ADR-0044 D2 のコメント行は別 type のまま）。
- 各アダプタの写像（ここだけがアダプタ固有）: `claude-code` は stream-json の `tool_use` / `tool_result` / `text`（今も `worker_progress
  tool_use: Bash {...}` として出しているものを構造化する）; `codex` は JSON イベント（`item.command_execution` 等）; `acp` は
  `session/update` の `tool_call` / `agent_message_chunk`; `paperqa` / `local-deep-research` / `langmem` は `status` だけ（節目）。
- Console は `progress` を run ごとに束ね、折り畳みの見出しに「担当 / harness / tier / 経過 / tool 回数 / 最後の `status`」、開くと
  各行（`tool_use` は `tool` + `summary`、`tool_result` は要約、`text` は本文）を出す。**詳細は必要なときだけ** `GET /tasks/{id}/runs/{run}/events`
  で取る（初期表示には載せない）。
- 記録は今までどおり `events`（`WorkerProgress`）に残す。ワーカーの stdout の丸ごと保存も従来どおり（`runs/<id>/stdout.jsonl`）。

### D3. 入力: 素の文は CoS へ。CoS の返事は「動く」

- 入力欄の文は `POST /console/instruct {text, scope}`（管理系）。
  - `scope = node:<id>` か `@<node-id>` で始まる文 → そのノードとの対話（既存の `/org/{id}/messages`）。
  - それ以外 → **CoS との対話 run**（`conversation` harness）。前置きには従来の内容（brief / standing rules / 記憶 / 直近の仕事 / 対話）
    に加えて**組織の一覧**（id・名前・skills・harnesses。ADR-0046 D6）、**進行中の案件と途中目標**、**知識の索引**（ADR-0047 D2）。
- CoS の結果ファイルに宣言的な **`actions`**（ADR-0034 D7 / ADR-0038 の `milestone_proposal` と同じ流儀。taskd が決定的に実行）:

```json
{"summary": "…", "actions": [
  {"type": "create_task", "title": "…", "objective": "…", "acceptance": [...], "harness": "coding", "skills": ["rust"],
   "mode": "prototype", "repos": ["agent-platform"], "project": "<id or null>", "milestone": "<id or null>", "assignee": null},
  {"type": "propose_project", "title": "…", "request": "…", "repos": [...]},
  {"type": "add_milestone", "project": "<id>", "title": "…", "description": "…"},
  {"type": "ask_human", "text": "…"}
]}
```

- `create_task` で作られるタスクは **`ready`**（人が Console で言ったので Go 済み。ADR-0044 D1 の「人が作ったタスク」と同じ）。
  `assignee` が無ければ ADR-0046 D5 の matching。`project` が無ければ**案件なしのタスク**（Console の全体の流れにだけ出る）。
  検証に落ちた action（知らない harness / repos / 案件）は**実行せず** `reply` に「実行できなかった action: …」を付けて人に見せる。
- 「案件として」「途中目標に」など人が儀式を求めれば `propose_project` / `add_milestone`。小さな頼みは `create_task` 1 つで済ませる
  （CoS の指示文に判断の目安を書く: 1 タスクで 1 時間以内に終わり、承認が要らない変更なら task）。
- 対話 run の禁止事項（ADR-0033 D4 / Phase 28: 委譲しない・Question を出さない・道具を使わない）はそのまま。動くのは **`actions` を
  taskd が実行する経路だけ**。

### D4. GUI

- ルート `/`（今は秘書の対話）を **Console** にする。左に範囲（全体 / 案件 / ノード）、中央に流れ（新しいものが下。自動スクロール、
  折り畳みの既定は D2）、下に入力欄（`@node` 補完、Enter で送信、Shift+Enter 改行）。各ブロックの操作: 返信・開く・タスク画面へ・
  認可の once / standing / deny・質問の回答・途中目標の ok / 議論 / ng。
- ノードの画面（`/org/{id}`）は同じ部品で `scope = node:<id>`。
- 未読の扉: 質問・認可・途中目標の待ちは上部に固定の帯（数）。
- 通知（ADR-0037）はそのまま。Console は開いているときの画面。

## 2. 採らない

- CoS が道具を直接使う・委譲する（ADR-0033 D4 のまま。動くのは actions だけ）。
- 人の発言を LLM が「案件か task か」以外に解釈して勝手に権限を広げる（tools / 認可は profile と standing rules）。
- ワーカーの生の stdout を Console に流す（正規化した progress だけ。詳細は要求時）。

## 3. 受け入れ条件（Phase 60 / G22）

1. `GET /console` と `/console/stream`（範囲 3 種、カーソル、ブロック 8 種）。テストは偽アダプタで task → progress → report の流れが 1 本になること。
2. progress の構造化（`kind` / `tool` / `summary` / `detail`）: claude-code アダプタの写像（stream-json のテストデータで）、codex / acp は
   少なくとも `status` に、paperqa / LDR は `status`。従来の文字列 progress と混在できる。
3. `POST /console/instruct` と CoS の `actions`（create_task → ready + matching、propose_project、add_milestone、ask_human、検証失敗の返し方）。
   対話 run の前置きに組織の一覧・進行中の案件・知識の索引。
4. GUI `/` = Console（範囲・流れ・折り畳み・展開・返信・その場の認可と回答・途中目標の判定・`@node`）。ノード画面も同じ部品。
5. 実機: Console から「〜を直して」と 1 行入れる → CoS の返事と `create_task` → matching で担当が決まり `ready` → 進行が折り畳みで流れ、
   開くと tool_use が見える → 終わると report ブロックが出る。

## Phase 60a 追記（2026-09-20。D1 の読み取り側と D2 の実装で決めたこと）

Phase 60a で入ったのは **D1 の読み取り側**（`GET /console` / `GET /console/stream`）と **D2**（進行の正規化）だけ。
D3（`POST /console/instruct` と CoS の `actions`）と D4（GUI）は Phase 60b。

1. **`progress` の `error`**。D2 が挙げた 4 つ（`kind` / `tool` / `summary` / `detail`）に加えて `truncated` と
   **`error`** を足した。D2 の本文が `tool_result` に「エラーフラグ」を求めているのに、置き場が無かったため
   （`kind` は `tool_result` で埋まっている）。どちらも任意・既定 false なので `PROTOCOL_VERSION` は 4 のまま。
2. **`comment` は `kind` に入れない**。D2 の列挙に `comment` があるが、コメントは ADR-0044 D2 の**別 type**
   （`{"type":"comment"}` → `task_comments`）のままで、`progress` の `kind` ではない。`ProgressKind` は
   `tool_use` / `tool_result` / `text` / `thinking` / `status` の 5 つ。
3. **`thinking` は `detail` を持たない**。D2 の「あれば要約だけ」をそのまま実装した（思考の本文は流さない）。
4. **`codex` の `reasoning` は `thinking`**。D2 は codex について `tool_use` / `tool_result` / `text`
   「where the event types allow, else status」としていたが、`item.reasoning` は素直に `thinking` に写る
   ので、そうした。`todo_list` など残りは `status`。
5. **範囲の効き方**。D1 は「CoS の対話はどの範囲でも見える」と書いているが、`GET /console` は
   **範囲どおりに絞る**（`project:<id>` はその案件のタスク・対話・報告・途中目標、`node:<id>` は
   そのノードのタスク・対話・報告）。全体の流れが見たいときは `scope=all` で、CoS の対話は常にそこに出る。
   「どの範囲でも CoS が見える」は画面側の置き方（Phase 60b / D4）で決める。
6. **質問と認可の重複**。Phase 44 でディスパッチャの質問も `approvals` に残るようになったため、
   同じ質問が `question` と `approval` の 2 ブロックに出る。同じタスク・同じ質問文の認可があるときは
   **`approval` の側だけ**出す（答える口が 1 つになる）。
7. **`milestone` は `proposed` だけ**。D1 の `milestone` は「途中目標の提案」なので、`approved` 以降の
   途中目標は流れに出さない（案件の画面で見る）。ノードの範囲には出さない。
8. **カーソル**。`(時刻のナノ秒, 同時刻の並びを決める tie, events の読み進み)` の 3 つ組を 1 つの
   不透明な文字列にした。出どころ（`events` / `messages` / `approvals` / `milestones` / `reports`）が
   ばらばらなので、時刻だけでは同時刻の取りこぼしと重複が避けられない。`since` は時刻について**閉区間**で
   引き、カーソルで落とす。
9. **`progress` の詳細**。折り畳みの見出しには始めの 3 行と終わりの 3 行だけを載せ、全行は
   **`GET /tasks/{id}/runs/{run_id}/events`**（Phase 60a で追加）で取る（D2 の「詳細は必要なときだけ」）。
10. **SSE のまとめ方**。`progress` は run ごとに 1 秒に 1 回まで。流れてくるのは**その接続で見た積み上げ**
    （`count` / `tool_count` はその run の合計）。対話・認可・報告・途中目標は 1 秒ごとに見に行く。
    `Last-Event-ID` は使わない（再開は `since`）。
11. **`knowledge` ブロック**は形だけ予約した（Phase 60a では誰も作らない）。中身は ADR-0047 側が決める。
