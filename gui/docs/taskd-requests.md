# taskd への依頼

GUI 側で回避せず、taskd の API に足りない・仕様（`docs/taskd-api-v1.md`）と違う点を書く。書いたら `docs/PROGRESS.md` に `## Phase G<N> — BLOCKED` を書いて止まる。

## 未対応

（現在は無し。R3〜R5 は taskd 側 Phase 27 で解決済み。下の「対応済み」参照。）

## 対応済み

### R3 — 対応済み（2026-09-17、taskd 側 Phase 27 / GUI-R3）

- `TaskSummary` に `assignee: Option<String>` と `conversation: bool`、`ProjectTaskView` に
  `conversation: bool` を足した（`project_id` / `milestone_id` は `TaskSummary` には足していない。依頼の
  主目的だった「抱えている仕事の数」の集計には `assignee` だけで足りるため）。
- GUI 側（Phase G13e）: `/org` の「抱えている仕事の数」を、`GET /projects/{id}` を案件数ぶん束ねる N+1 の
  代替から、**1 回の `GET /tasks?limit=500`** の `TaskSummary.assignee` を数える形に直した
  （`app/lib/org-tree.ts::countWorkload` / `tasksByAssignee`、`app/routes/org.tsx::loadOrg`）。
  `TaskSummary` に `project_id` が無いため、組織ノード詳細の「抱えているタスク」一覧から案件名の列は
  落とした（N+1 をやめた代わりのトレードオフ）。
- `conversation: true` のタスク（対話用。人への返事のための run）は、仕事の木（`/projects/:id`、
  `app/lib/work-tree.ts::projectTasksToGraph`）と組織の「抱えている仕事」から**完全に除外**、`/tasks`
  一覧では既定で隠し「対話用も表示」のトグル（`show_conversation=1`）で出せるようにした
  （`app/routes/tasks.tsx`）。
- 以下は原文（記録のため残す）。

### R3（原文、2026-09-17、Phase G13a）: `TaskSummary` に `assignee` / `project_id` / `milestone_id` が無い

- **エンドポイント**: `GET /tasks`（`TaskList.items[]` = `TaskSummary`）。
- **期待（ADR-0033 D1「組織の木」、SPEC §3.2「誰が何を抱えているか」）**: 組織の木の各ノードに「抱えている仕事の数」を出すには、
  タスクの `assignee`（組織のノード id）を横断的に数えられる必要がある。
- **実際（`docs/gui/api.md` §3.3 の `TaskSummary` 定義、Phase 23 で `assignee`/`project_id`/`milestone_id` が追加されたのは
  `Task`（`GET /tasks/{id}`）と `ProjectTaskView`（`GET /projects/{id}` の `tasks[]`）だけ）**:
  ```
  pub struct TaskSummary {
      pub id: TaskId, pub parent_id: Option<TaskId>, pub kind: TaskKind, pub status: Status, pub title: String,
      pub priority: i32, pub tier: Tier, pub adapter: Option<String>, pub attempts: u32, pub max_retries: u32,
      pub depends_on: Vec<TaskId>, pub created_at: String, pub updated_at: String,
      pub lease_expires_at: Option<String>, pub backoff_until: Option<String>,
      pub children: u32, pub pending_children: u32, pub role: Option<String>, pub genre: Option<String>,
      pub actions: Vec<Action>,
  }
  ```
  `assignee` が無い。`GET /tasks` にも `?assignee=` フィルタは無い（`docs/gui/api.md` §3.3 の一覧はクエリに `assignee` を含まない）。
- **できないこと**: 「組織の木の各ノードが抱えている仕事の数」を、1 回の `GET /tasks` で横断的に数えられない。
  今回（`/org` の loader、`app/routes/org.tsx` / `app/lib/org-tree.ts`）は `GET /projects` の全件を `GET /projects/{id}`
  で束ね、その `tasks[].assignee`（`ProjectTaskView`）を集計する代替で対応した。**この代替では `project_id` の無い
  （案件に属さない）タスクへの割り当ては数えられない**。案件数ぶんの N+1 呼び出しにもなる。
- **依頼**: `TaskSummary` に `assignee: Option<String>`（`Task.assignee` と同じ規則。`role`/`genre` を足した R2 と同じ流儀）を足し、
  可能なら `GET /tasks` に `?assignee=<org_node_id>` フィルタ（`?parent=`/`?project=` と同じ形）を足してほしい。
  1 回の `GET /tasks?assignee=<id>` （またはフィルタ無しで一覧を取って `assignee` で数える）で組織の木の全ノードの
  ワークロードが求まるようになる。

### R4 — 対応済み（2026-09-17、taskd 側 Phase 27 / GUI-R4）

- (a) `Message` に `task_id: Option<TaskId>` を足した（migration 0007。`role = user` の行にも
  `role = node` の行にも同じ id が入る）。GUI 側（Phase G13e）: `app/components/Conversation.tsx` が
  `Waiting`/`replyLink` の「送った直後の発言だけ」ヒューリスティックをやめ、`Message.task_id` を直接
  リンク先にした。画面を開き直した後の**過去の返事**からも `/tasks/:id`（裏方の run）へ行けるようになった。
- (b) `TaskSummary.conversation: bool` / `ProjectTaskView.conversation: bool` を足した（`title` の
  前置きに頼らない、仕様に書かれた値）。GUI 側（Phase G13e）: `app/lib/work-tree.ts::projectTasksToGraph`
  が `conversation: true` のタスクを仕事の木から完全に除外、`app/routes/projects.$id.tsx` の
  「担当に話す」一覧・件数表示も同じ絞り込みに揃えた。`/tasks` 一覧は既定で隠し、トグルで表示できる
  （R3 の対応済みメモも参照）。
- 以下は原文（記録のため残す）。

### R4（原文、2026-09-17、Phase G13b-2）: `Message` に `task_id` が無く、対話用タスクが仕事の木に混ざる

- **エンドポイント**: `GET /org/{id}/messages`（`MessageList.items[]` = `Message`）と `GET /projects/{id}`（`tasks[]` = `ProjectTaskView`）。
- **期待（SPEC §3.4「相手は人」、§3.3「仕事の木をパッと見れば、おかしな方針を立てていないかが分かる」）**:
  (a) 返事（`role = "node"`）から、それを作った run の**裏方のタスク**（`/tasks/:id`）へ行けること。
  (b) 案件の仕事の木は「案件が分解された仕事」だけで、人との対話そのものは混ざらないこと。
- **実際**:
  (a) `Message` は `{id, node_id, project_id?, role, text, run_id?, created_at}` で、**`task_id` が無い**。`POST /org/{id}/messages` の 202 は
      `{message_id, task_id}` を返すので、**自分がその画面で送った直後の返事にだけ**タスクを結びつけられる（`app/components/Conversation.tsx`）。
      画面を開き直した後の過去の返事は `run_id` しか分からず、`run_id` からタスクを引く API は無い（`GET /tasks/{id}/runs` はタスク id が要る）。
  (b) 対話用タスク（`title = "対話: …"`、`assignee` = 相手のノード、`project_id` = 選んだ案件）は `ProjectTaskView` にも `TaskSummary` にも
      そのまま出る（taskd 側 Phase 24 の未解決事項 U24-4）。実機で確認: 秘書に案件を投げると、その案件の「仕事の木」に
      `対話: Pluvio を基盤に用いた…[秘書]` のノードが 1 件出る。
- **できないこと**: (a) 過去の返事から裏方の run へ行く導線を、仕様どおりの値だけでは作れない。(b) 仕事の木から対話用タスクを
  除くことが、**仕様に書かれた値**ではできない（`title` の `"対話: "` 前置きに依存するのは「文書に無い挙動に頼る」ことになるのでやらない）。
- **依頼**: (a) `Message` に `task_id: Option<TaskId>`（`run_id` と同じ規則。追加のみ）を足してほしい。
  (b) `ProjectTaskView`（と `TaskSummary`）に「対話由来かどうか」が分かる値（例: `conversation: Option<MessageId>` をそのまま出す、
  または `GET /projects/{id}` / `GET /tasks` に `?exclude=conversation` のような絞り込み）を足してほしい。
  どちらも足されるまでは、GUI は (a) 送った直後の返事にだけリンクを出し、(b) 対話用タスクも仕事の木に描いたままにする。

### R5 — 対応済み（2026-09-17、taskd 側 Phase 27）

- `GET /approvals?pending=false` が「決定済みだけ」（`decision IS NOT NULL`）に絞り込まれるよう直った
  （`ApprovalStore::approval_list` の第 1 引数が `bool` から `Option<bool>` に変わり、`None` = 全件 /
  `Some(true)` = 未決定だけ / `Some(false)` = 決定済みだけ）。
- GUI 側（Phase G13e）: `app/routes/approvals.tsx::loadApprovals` を `pending=true` / `pending=false` の
  **2 回呼び**に戻し、`app/lib/approvals.ts` の `splitApprovals`（G13d の回避策）は削除した
  （クエリの絞り込みを taskd に任せる、本来の設計に戻せた）。
- 以下は原文（記録のため残す）。

### R5（原文、2026-09-17、Phase G13d）: `GET /approvals?pending=false` が絞り込まない

- **エンドポイント**: `GET /approvals?pending=`（§3.56）。
- **期待（`docs/taskd-api-v1.md` §3.56「`pending=true` で未決定だけ」）**: `pending=false` は「決定済みだけ」（`decision IS NOT NULL`）に
  絞り込むと読める（`GUI`は「上に未決の要求、下に決めたものの履歴」を作るのに使う想定だった）。
- **実際（実機で確認。使い捨ての taskd、`config/org.example.toml` + 偽アダプタ）**: `pending=true` は正しく未決定だけに絞れるが、
  `pending=false` は**フィルタ無しと同じ全件**を返す（`decision` が付いた行も付いていない行も両方入る）。
  ```
  # 1 件を once で決定した直後
  $ curl .../approvals?pending=true   # -> 未決定 1 件だけ（正しい）
  $ curl .../approvals?pending=false  # -> 決定済み 1 件 + 未決定 1 件（全 2 件。決定済みだけにならない）
  ```
- **できないこと**: `pending=false` に頼って「決めたものの履歴」を作ると、未決の要求まで混ざって二重に表示される。
- **GUI 側の対応（回避ではなく、ドキュメント化されたフィールドで代替）**: `GET /approvals`（フィルタ無し）を 1 回だけ呼び、
  応答に必ず含まれる `Approval.decision`（§3.56「`decision` は once/standing/denied（未決定は無い）」）の有無で
  GUI 側で pending / decided に分けた（`app/lib/approvals.ts` の `splitApprovals`。`filterReportsByKind` 等、既存の
  「ドキュメント化されたフィールド値で GUI 側が分ける」パターンと同じ）。**新しい判断値は作っていない**。
- **依頼**: `pending=false` が `decision IS NOT NULL` で絞り込まれるように直してほしい（`pending=true` の実装を参考に）。
  直ったら GUI 側は `pending=true` / `pending=false` の 2 回呼びに戻せる（クエリの絞り込みを taskd に任せる方が本来の設計）。

### R2 — 対応済み（2026-09-16、taskd 側で追加）

- `TaskSummary` と `GraphNode` に `role: Option<String>` を足した（`Task.role` そのまま。役割が無ければ `null`）。
  既存フィールドの意味は変えていないので v1 のまま（追加のみ）。`docs/taskd-api-v1.md` §3.3 / §3.16、スキーマ、`app/taskd/types.ts` に反映済み。
- `skip_serializing_if` は付けていない。`TaskSummary.adapter` など既存の任意フィールドと同じく **`null` を出す**（生成される型は `string | null`）。
- これで一覧の行と DAG のノードに、追加の `GET /tasks/{id}` 無しで役割ラベルを出せる（DESIGN §10 Phase G7 の残り 1 項目）。
- 以下は原文（記録のため残す）。

### R2（原文、2026-09-16、Phase G7）: `TaskSummary` / `GraphNode` に `role` が無い

- **エンドポイント**: `GET /tasks`（`TaskList.items[]` = `TaskSummary`）、`GET /graph`（`Graph.nodes[]` = `GraphNode`）。
- **期待（docs/DESIGN.md §10 Phase G7）**: 「一覧と DAG のノードに役割を出す（色分けはせず、テキストのラベル）」。
- **実際（`docs/taskd-api-v1.md` §3.2 の `TaskSummary` 定義、§3.9 の `Graph`/`GraphNode` 定義。`app/taskd/types.ts` の生成結果とも一致）**:
  ```
  pub struct TaskSummary {
      pub id: TaskId, pub parent_id: Option<TaskId>, pub kind: TaskKind, pub status: Status, pub title: String,
      pub priority: i32, pub tier: Tier, pub adapter: Option<String>, pub attempts: u32, pub max_retries: u32,
      pub depends_on: Vec<TaskId>, pub created_at: String, pub updated_at: String,
      pub lease_expires_at: Option<String>, pub backoff_until: Option<String>,
      pub children: u32, pub pending_children: u32, pub actions: Vec<Action>,
  }
  pub struct GraphNode { pub id: TaskId, pub title: String, pub status: Status, pub kind: TaskKind, pub parent_id: Option<TaskId> }
  ```
  どちらにも `role` が無い。`role` は `TaskDetail`（`GET /tasks/{id}`）にしか出ない（ADR-0016 D1「GUI の表示用に最上位にも出す」は詳細画面の
  ことだけを指しており、一覧・DAG には及んでいない）。
- **できないこと**: 一覧の各行・DAG の各ノードに役割のテキストラベルを出せない。行・ノードごとに追加で `GET /tasks/{id}` を呼べば埋まるが、
  一覧・DAG は 1 回の取得で完結する設計（`docs/taskd-api-v1.md` の意図）に反する N+1 呼び出しになり、CLAUDE.md の「taskd API の仕様外の挙動に
  頼らない」方針とも衝突するため行っていない（`docs/adr/0010-g7-decisions.md` D5）。
- **依頼**: `TaskSummary` と `GraphNode` に、`TaskDetail.role` と同じ規則の `role: Option<String>`（`#[serde(skip_serializing_if = "Option::is_none")]`）
  を追加してほしい。追加のみで v1 のまま拡張できる想定（ADR-0016 と同じ流儀）。

## 調査依頼（API の不足・仕様違いではないので BLOCKED にはしない）

### R1 — 回答済み（2026-09-15、taskd の ADR-0015 / PROGRESS phase 9）

- **原因は taskd ではなく DB の置き場所**。`.run/` が NFS 上の `$HOME` にあり、SQLite の WAL がネットワーク FS で動いていた（taskd の ADR-0013 D5）。
  `.run` をローカルディスクに置くと G2 の e2e はフルスイート 3 回とも 8/8 で通り、停止は 1 度も起きなかった。
- **GUI 側の対応（Phase G5、docs/adr/0008 D13）**: `scripts/taskd.sh` の `RUN_ROOT` を `TASKD_RUN_ROOT` で上書きできるようにし、既定をローカルディスク
  （`${TMPDIR:-/tmp}/taskd-gui-run-$USER`）にして `.run` はそこへのシンボリックリンクにした。ネットワーク FS 上なら警告を出す。
- **taskd 側の対応**: 遅い要求（1 秒超）・遅い tick・DB がネットワーク FS 上にある場合の警告ログ（ADR-0015）。
- 以下は原文（記録のため残す）。

### R1（原文）: GUI（ブラウザ + SSE 中継）が接続している間、taskd の tick が 10〜30 秒止まることがある（2026-09-15、Phase G2 の e2e で 5 回観測）

- **現象**: `scripts/taskd.sh fixture basic && scripts/taskd.sh start basic` の直後〜数十秒の間に GUI から承認 / 回答 / 作成→承認を行うと、
  taskd の API は `POST` に 200 で応答するのに、その後 10〜30 秒の間 (1) ディスパッチャが遷移を進めない（Approval 子を承認しても親が `reviewing` のまま、
  `ready` にしたタスクの fake ワーカー run が `dispatching` から `worker finished` まで 24 秒）、(2) `daemon` / `task.event` の SSE が届かない、
  (3) 場合によっては `GET /tasks/{id}` が 15 秒（GUI の `TaskdClient` のタイムアウト）応答しない。tick のログ（`tick ticks=N`）もその間出ない。
  - 例（`.run/basic/taskd.log`）: `04:53:59.906 dispatching task_id=01M2HPMAZPME9YGGAYXD9XKCGH` → 次の行が `04:54:24.103 worker finished ... outcome=done`。
    fake ワーカー（`.run/basic/fake-worker.sh`）単体の実行は 20 ms。
  - 例（GUI の要求ログ）: `POST /tasks/<id>.data 200 114ms`（approve）の直後の `GET /tasks/<id>.data` が `503 15120ms`（BFF 側タイムアウト）。
  - 例（受け入れ条件 1）: 受信箱で Approval を承認 → 親 Human-B が `done` になるまで約 30 秒（停止しない回は 0.5 秒）。
- **再現しないもの**: taskd 単体に `curl` で同じ操作（承認→親 done: 0.44〜0.60 秒 ×3、作成→承認→done: 0.87〜1.16 秒 ×3、起動直後でも 0.52 / 1.16 秒）。
  GUI 経由の SSE クライアント（curl）を 3〜4 本付けた状態でも再現せず。GUI の `.data` を 250 ms 間隔で叩き続けても再現せず。
  Chromium が `EventSource` + 250 ms スロットルの再検証（`GET /tasks/{id}` + `GET /tasks/{id}/events` + `GET /health` + `GET /inbox` を並列）を行っている
  状態で、変更系の `POST` を行った直後にだけ起きている。
- **GUI 側で確認済みのこと**: ブラウザが切った `/events` の上流 `/stream` は 8 秒以内に閉じる（ゾンビ化しない）。GUI は SQLite を開かない。
- **できないこと**: GUI からは原因を特定できない（taskd 内部の tick ループ / SQLite の書き込みロック / `spawn_blocking` の状態が見えない）。
  仕様には反していないので GUI は回避せず、受け入れ条件 1 の e2e の待ちを 60 秒にしてある（仕様に上限は無い）。受け入れ条件 5（30 秒以内に done）は
  停止が 24 秒だった回も含めて通っているが、停止が長引けば落ちる。
- **依頼**: 上記の条件で tick が止まる原因の調査（候補: ディスパッチャの `store` 呼び出しが API 側の書き込みと `busy_timeout` 5 秒で競合して連鎖する、
  SSE 購読者の `events_since` ポーリングと `Mutex<Connection>` の競合、WAL チェックポイント）。要求ごとのログ（`X-Request-Id`、所要時間）が API にあれば
  GUI 側からも切り分けられる。
