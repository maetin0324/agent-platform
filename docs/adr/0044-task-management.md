# ADR-0044: タスク管理 — 人が細かく触れるタスク、コメントで即座に起こす、ボードと検索、中止・一時停止・アーカイブ、git を正本にした文書

- 日付: 2026-09-19
- 状態: **Accepted**（人間の回答 2026-09-19。要点: 人がかなり手を加えられること（秘書との会話で作ることもできる）、人のコメントは
  すぐに担当を起こす、期日・見積もり・スプリントは不要、文書の正本は git、GitHub Issues への写しは不要（PR は Celeris からも
  見えると嬉しいが常時同期は不要）、案件を跨ぐ依存は不要、中止したタスクの worktree とブランチは消す、中止・一時停止・
  アーカイブは提案どおり、タスク管理画面で担当エージェントのレベル（frontier / standard / cheap）を指定できること）
- 関連: SPEC §3（組織・案件・報告）/ §3.6（認可）/ §7（途中目標）、ADR-0021（質問と `answer`）、ADR-0033 D2（tier の優先順位
  task > role > assignee > genre）、ADR-0034（報告）、ADR-0038（途中目標の対話）、ADR-0043（ワークスペース。差分・PR はタスク画面）、
  ADR-0042（パス）

## 1. 文脈

タスクは状態機械・親子・依存・認可・質問・報告を持つが、**人が手で触る道具**が無い: 編集できない（`PATCH /tasks` が無い）、
タスク単位の会話が無い（対話はノード単位、質問は認可経由）、ラベルも検索もボードも無く、案件や途中目標を止められず、
成果としての文書は `answer.md` のような成果物ファイルに散っている。人は「秘書との会話で作り、見て、コメントし、止め、
必要なら細部まで直す」ことを望んでいる。GitHub Issues / Confluence の水準を、組織の道具として持つ。

## 2. 決定

### D1. タスクは人が編集できる。tier もタスクで決める

- `PATCH /tasks/{id}`（管理系）: `title` / `objective` / `acceptance` / `priority`（D3）/ `labels` / `category` / `assignee`（組織ノード）/
  `role` / **`tier`**（`worker_hint.tier`。ADR-0033 D2 の最上位「タスク」の指定）/ `adapter`（任意。`worker_hint.adapter`）/
  `milestone_id` / `depends_on` / `repos`（ADR-0043 D2）/ `budget`（`max_turns` / `max_wall_secs` / `max_retries`）。
  終端（done / failed / cancelled）のタスクは 409。`running` / `reviewing` は受け付けるが**次の run から効く**（`Event::Edited
  {fields: [...], by: "human"}` を積む。走っている run は止めない。止めたければ D2 のコメントか D6 の中止）。
- 人がタスクを作る: `POST /tasks` は既にある（管理系）。案件・途中目標・親・repos・tier を指定できるようにし、GUI の案件画面と
  途中目標カードに「タスクを追加」。人が作ったタスクは **`ready`**（人は Go を出す側なので draft を挟まない。`draft` にしたければ
  `status: "draft"` を明示）。
- GUI: タスク画面に「編集」（全フィールド。tier はプルダウン frontier / standard / cheap。担当はノードのプルダウン）。
  ボード（D4）でも tier / 優先度 / 担当は行内で変えられる。

### D2. タスク単位のコメント。人のコメントは担当を**すぐ起こす**

migration `0013_task_comments.sql`:

```
task_comments(id TEXT PK, task_id TEXT NOT NULL, author_kind TEXT NOT NULL CHECK(author_kind IN ('human','node','system')),
  author TEXT NULL,          -- node id（node のとき）
  body TEXT NOT NULL, run_id TEXT NULL, created_at TEXT NOT NULL)
```

- `GET /tasks/{id}/comments`、`POST /tasks/{id}/comments {body}`（人。管理系）。担当（組織の「人」）はワーカー・プロトコルの新しい行
  `{"type":"comment","body":"…"}` で書く（`progress` と違い**残る**。前置きに「短い進捗や判断の記録はコメントに書け」）。
  秘書・lead が委譲先のタスクに書く経路は `messages` ではなく同じ表（`author_kind = node`）。
- **人のコメントの効き方（決定的）**:
  | タスクの状態 | 何が起きる |
  |---|---|
  | `running` / `reviewing` | run を止める（ワーカーに SIGTERM → `kill_grace_secs`）。新しいトリガ **`Interrupt`** で `running/reviewing → ready`（attempts 据え置き。理由 `comment`）。次の run の前置きの先頭に「**人からの割り込み**: <コメント>」。worktree はそのまま（続きから直せる） |
  | `blocked` | `answer` と同じ（ADR-0021 D2。質問への回答として渡す） |
  | `ready` / `draft` | コメントを記録。次の run の前置きに載る（`draft` は Go 待ちのまま） |
  | `done` / `failed` / `cancelled` | コメントを記録。GUI に「再開」ボタン（`POST /tasks/{id}/reopen` → 新しいトリガ **`Reopen`**: `done/failed → ready`、attempts 0 に戻す。`cancelled` は再開しない: worktree が無い） |
- 割り込みで止めた run は `WorkerFinished{outcome: "interrupted: comment"}` として記録し、報告（ADR-0034）は作らない。
- ワーカーのコメントは人を起こさない（通知は ADR-0037 の 5 種のまま）。

### D3. ラベル・種類・優先度

- `labels: Vec<String>`（小文字、`[a-z0-9-]`、最大 8 個。自由）。`category: feature | bug | research | ops | docs | other`（既定 `other`）。
  `tasks` に列を足す（migration 0013）。計画 run（ADR-0028）の出力に `category` と `labels` を足し、planner に決めさせる（無ければ既定）。
- 優先度は **P0〜P3**。既存の `priority: i32`（大きいほど先）に写す: P0 = 30、P1 = 20、P2 = 10、P3 = 0。API は `priority: "P1"` を受け、
  `priority_label` を返す（`i32` は互換のため残す）。既定 P2。ready の並びは従来どおり `priority DESC, created_at ASC`。
- 期日・見積もり・スプリントは**作らない**（人の回答）。

### D4. ボードと検索

- `GET /tasks` のフィルタを足す: `label=`、`category=`、`assignee=`、`milestone=`、`tier=`、`priority=`、`q=`（title / objective /
  コメント本文の全文。**`LIKE` で引く**（Phase 53 追記: bundled SQLite に FTS5 はあるが `unicode61` は日本語を分かち書きしないので
  「調査」が 0 件になる。FTS5 は採らない）。複数指定は AND。`archived`（D6）は既定で隠す。
- GUI「ボード」画面（案件を選ぶ）: 列は **待ち（draft / ready）・進行中（running / reviewing）・止まっている（blocked）・完了（done）・
  失敗（failed）・中止（cancelled）**。カードに題名・担当・tier・優先度・ラベル・種類・途中目標。フィルタ欄（上のクエリ）。
  カード上で優先度・tier・担当をその場で変えられる（`PATCH`）。並べ替えは優先度で（ドラッグで列を跨ぐ状態変更はしない。
  状態は状態機械の仕事）。

### D5. タスクのタイムラインと紐付け

- `GET /tasks/{id}/timeline` → 時刻順に 1 本: イベント（遷移・run・質問・回答・編集・割り込み）、コメント、認可、報告、委譲
  （子の作成）、取り込み（ADR-0043 D5: merge / PR / discard）、**リリース**（そのタスクのブランチのコミット sha が
  `~/taskd/releases/*/changes.json` の `commits` に含まれるリリース → 「このタスクの変更はリリース <sha12> に入った」）。
- GUI のタスク画面はタブ: **概要**（編集可）／**タイムライン**（コメント欄はここ。下に入力欄）／**変更**（ADR-0043 D5: 差分・PR・
  3 ボタン）／**ファイル**（ADR-0043 D6）／**成果物**（従来）。

### D6. 中止・一時停止・アーカイブ（タスク・途中目標・案件の 3 階層）

- **中止**: `POST /tasks/{id}/cancel`（既存）。**中止したタスクは worktree とブランチを消す**（ADR-0043 D2。人の回答）。
  `POST /milestones/{id}/cancel` / `POST /projects/{id}/cancel`（管理系）: 属する非終端タスクを全部 cancel（走っている run は
  既存の cancel の経路で止める）、途中目標は `cancelled`、案件は `cancelled`。連鎖は決定的・同期。
- **一時停止**: `POST /projects/{id}/pause` / `resume`、`POST /milestones/{id}/pause` / `resume`。`projects.status` に `paused`、
  `milestones.status` に `paused` を足す。**paused の案件・途中目標に属するタスクは dispatch しない**（`ready` のまま。状態機械は
  触らない）。走っているものは終わらせる。`resume` で元の状態（案件は `active`、途中目標は pause 前の状態を `paused_from` 列に
  持つ）。
- **アーカイブ**: `POST /projects/{id}/archive` / `unarchive`（`projects.archived_at`）。終端（`completed` / `cancelled`）の案件だけ。
  一覧から既定で隠す（`?archived=1` で見える）。タスクも `GET /tasks` の既定で隠れる。
- GUI: 案件画面のヘッダに「一時停止／再開」「中止」「アーカイブ」（中止とアーカイブは確認付き）。途中目標カードに「一時停止／中止」。

### D7. 文書（Confluence 側）: 正本は git のファイル。案件の primary リポジトリの `docs/`

- 案件の文書の根 = **primary リポジトリ**（ADR-0043 D1）の `[outputs].docs`（既定 `docs/`）。primary が `dir` か無い案件は、
  Celeris が **`~/workspace/<案件 slug>/`** に `git init` した文書リポジトリを作り primary にする（SPEC §5: 成果物は `~/workspace/` に）。
- ページ = `docs/**/*.md`。先頭に任意の front matter（`title`、`tags`、`tasks: [<task id>]`）。無ければ 1 行目の `# ` が題名。
- API（正本は default_branch の内容。人の編集は default_branch に直接コミット）:
  - `GET /projects/{id}/docs` → ツリー（path / title / updated_at / 最終コミット）。
  - `GET /projects/{id}/docs/page?path=` → `{raw, html, title, tags, tasks, history: [{sha, at, author, subject}] (直近 20), etag: <blob sha>}`。
    Markdown の描画はサーバ側で決定的に（`pulldown-cmark`。生 HTML は捨てる）。
  - `PUT /projects/{id}/docs/page {path, body, etag, message?}`（管理系）→ `etag` が現在の blob と違えば 409。コミットの author は
    `Celeris (human) <celeris@local>`、message は `docs: <path>` か指定。**default_branch を人が checkout 中で dirty なら 409**
    （ADR-0043 D5 と同じ規則。文書は一時 worktree でコミットし default_branch を fast-forward する）。
  - `DELETE` 同様。
  - **昇格**: `POST /tasks/{id}/artifacts/promote {name: "answer.md", path: "docs/research/xxx.md", title?}`（管理系）→ 成果物を
    ページとしてコミット（front matter に `tasks: [id]`）。
  - 逆リンク: `GET /tasks/{id}/timeline` にそのタスクを `tasks:` に持つページを載せる。ページ本文中の `celeris:task/<id>` と
    `[[path]]` はリンクとして描画。
- 組織の「人」は文書を**タスクとして**書く（担当のリポジトリ = primary、worktree、ブランチ、取り込み。ADR-0043 D5）。
  前置きに「文書は `docs/` に Markdown で書く。題名は 1 行目」。
- GUI「文書」タブ（案件）: ツリー、描画、編集（テキスト。プレビュー付き）、履歴、タスクへのリンク。検索は D4 の `q=` を文書にも
  （`GET /projects/{id}/docs?q=` は `git grep`）。

### D8. 通知・報告との関係

- コメント・編集・中止・一時停止は報告（ADR-0034）を作らない。タイムラインに残るだけ。
- 人のコメントで割り込んだ run が失敗扱いにならないよう、`interrupted` は `bad_news` にも `error_cooldown` にも数えない。

## 3. 採らない

- 期日・見積もり・スプリント・ガントチャート。
- GitHub Issues への写し・双方向同期。PR だけ Celeris から見える（ADR-0043 D5）。
- ドラッグで状態を変える（状態は状態機械が決める）。
- 文書を DB に持つ。正本は git。
- 案件を跨ぐ依存。

## 4. 受け入れ条件（Phase 名は PROGRESS で振る）

- **B1（D1 / D2 / D3 / D4 / D5）**: `PATCH /tasks`、人のタスク作成（ready）、tier / priority(P0–P3) / labels / category、コメント表と
  API、ワーカーの `comment` 行、**人のコメントによる割り込み**（running を止めて ready、前置きの先頭に載る）、`answer` 相当、
  `reopen`、フィルタと `q=`、タイムライン API。GUI: タスク画面のタブ（概要・タイムライン・変更・ファイル・成果物。変更・
  ファイルは ADR-0043 A1/A2 の API が入るまで空でよい）、編集フォーム、ボード画面、案件・途中目標の「タスクを追加」。
  実機: 走っている run に人がコメントし、run が止まって次の run の前置きにコメントが載る。
- **B2（D6）**: 案件・途中目標の cancel / pause / resume / archive と連鎖、paused の dispatch 抑止、cancel での worktree・ブランチ削除、
  GUI のボタン。
- **B3（D7）**: 文書 API と GUI、昇格、逆リンク、既定の文書リポジトリの作成。実機: 調査の `answer.md` を昇格して GUI で読み、
  人が 1 行直してコミットが残る。
- どの Phase も `cargo test --workspace` / clippy / GUI 一式、PROGRESS の実機の証跡。

## 5. Phase 53 追記（2026-09-19。B1 の実装と監査から）

- D4 の検索は `LIKE`（上記）。
- **run の止め方を 1 つにする（B2 で実装）**: `cancel` / `Interrupt` / タイムアウトのどれも、**プロセスグループ**に SIGTERM →
  `kill_grace_secs` → SIGKILL とする。今日までは `cancel` と `Interrupt` が子プロセスだけを SIGKILL し、ハーネスが起こした孫
  （`cargo test` など）が生き残っていた。D2 の表の「SIGTERM → `kill_grace_secs`」はこの追記で実現する。
- **変更を伴う API はすべて管理系（bearer）に揃える（B2 で実装）**: `answer` / `cancel` / `retry` / `POST /tasks` / `approve` も
  `PATCH` / コメント / `reopen` と同じくトークン必須にする。GUI は BFF がトークンを持つので画面は変わらない。人以外（組織の「人」）
  はそもそも API を叩かない。
- `PATCH` で `assignee` / `role` を変えたとき tier / adapter / budget を再導出するかは**しない**（人が触った値を壊さない）。人が
  変えたいなら同じ `PATCH` で明示する。GUI の編集フォームは「担当を変えると役割の既定が変わります」と注記するだけ。


## 6. Phase 55 追記（2026-09-19。B2 = D6 と §5 の 2 項目の実装から）

D6 と §5 はそのまま実装した。以下は**決めきれていなかったところを決めた**記録（逸脱ではなく細目）。

- **`ProjectStatus` / `MilestoneStatus` の終端**: 案件は `done` / `cancelled`、途中目標は `cancelled`。
  `MilestoneStatus` に `paused` と `cancelled` を足した（`ProjectStatus` には `paused` が既にあったので
  `cancelled` だけ）。`paused_from` は `projects` と `milestones` の両方に列で持つ（D6 は案件についてだけ
  書いていたが、途中目標も同じ理由で要る）。migration `0015_lifecycle.sql`、`SCHEMA_VERSION = 15`。
- **案件の中止は非終端の途中目標も `cancelled` にする**（D6 は「途中目標は `cancelled`、案件は `cancelled`」と
  書いていて、案件の中止が途中目標に波及するかが読めなかった）。`reached` / `redesigned` は達成・再設計の
  記録なので触らない。同じ理由で **`MilestoneStatus::is_terminal()` は `reached` / `redesigned` /
  `cancelled` の 3 つ**とし、`POST /milestones/{id}/{cancel|pause}` もこの 3 つには効かない（409）。
  GUI のボタンの出し分け（`gui/app/lib/lifecycle.ts`）はこの規則と 1 対 1 に対応する。
- **連鎖の理由**: 状態機械に `Trigger::ProjectCancelled` / `MilestoneCancelled` を足した。遷移は `Cancel` と
  同じ（非終端 → `cancelled`、attempts 据え置き）で、`Event::Transitioned.reason` だけが
  `project_cancelled` / `milestone_cancelled` になる。タイムラインで「自分が止められたのか、上ごと
  止まったのか」が読める。**案件・途中目標そのものにはイベント表を作らない**（D8 の「タイムラインに残る
  だけ」に対して、案件のタイムラインは既存の `messages` と報告で足りる。動くのは `updated_at` だけ）。
- **dispatch の抑止は `TaskStore::ready_tasks` の 1 か所**。計画・レビュー・まとめ・報告の圧縮といった
  裏方の run も同じ `tasks` の行なので、ここだけで全部止まる（`milestone_review::schedule` や
  報告の圧縮の**スケジュール側は止めない**。行は作られるが `ready` のまま動かない）。
  **例外は対話（`task_core::is_conversation`）**: 止まっている案件でも人が秘書と「なぜ止めたか」を
  話せる必要があるので、対話タスクだけは起こす。アーカイブ済み・`cancelled` の案件も同じ扱いで抑止する。
- **`PATCH` からは `paused` / `cancelled` を入れられない**（422）。`PATCH /projects/{id} {status}` /
  `PATCH /milestones/{id} {status}` は従来どおりの素の setter だが、この 2 つだけは `paused_from` が
  空のままになり中止の連鎖も起きないので、専用のエンドポイントに誘導する。GUI の「状態を直接変える」
  プルダウンからも外した（`gui/app/routes/projects.$id.tsx`）。
- **`archive` / `unarchive` は冪等**（既にその状態なら 200 でそのまま返す）。GUI の二度押しを 409 に
  しないため。非終端の案件の `archive` だけが 409。
- **run の止め方の統一の実装**（§5 の 1 つ目）: `task-worker` に `process_group` モジュールを足した。
  全アダプタは既に `Command::process_group(0)` で子を新しいプロセスグループの長にしていたので、
  spawn 直後に「`run_id` → その pid」を 1 つの表に登録し、`kill_tree(run_id, grace)` が
  `killpg(SIGTERM)` → `grace` → `killpg(SIGKILL)` を送る。ディスパッチャの `cancel` / `Interrupt` /
  リース喪失 / drain の 4 経路と、`subprocess` の中のタイムアウトがこれを通る。
  **tokio の `JoinHandle::abort()` は従来どおり即座に行う**（run の記録を止めるための帳簿）。
  そのため直接の子は `kill_on_drop` で即 SIGKILL されるが、**ハーネスが起こした孫はグループ宛の
  SIGTERM を受けて片付けの猶予を持つ**（D2 の表が求めていたのはここ）。
- **認可を揃えた範囲**（§5 の 2 つ目）: `POST /tasks` / `approve` / `reject` / `answer` / `cancel` /
  `retry` / `POST /plans` / `POST /replay` / `PATCH /projects/{id}` / `POST /projects/{id}/milestones` /
  `PATCH /milestones/{id}` を管理系にした（`reject` と `replay` と案件・途中目標の 3 つは §5 が数えて
  いなかったが、「変更を伴う API はすべて」の規則に含まれる）。読み取りは従来どおり。
  **認可は本文の検証より先**（トークン無しの不正な本文は 400 ではなく 401）。
  `taskctl` は HTTP API を使わず SQLite を直接開くので影響しない。

## 7. Phase 55/56 追記（2026-09-19。B2 と A3 の合流で決めたこと）

**コンテナで走る run の止め方**（P55-4 と ADR-0043 P56-7 の合流）: B2 の `kill_tree` は
`killpg(SIGTERM)` → `grace` → `killpg(SIGKILL)` をワーカーのプロセスグループへ送るが、A3（ADR-0043 D3）が
コンテナ実行に倒した run では、プロセスグループの長は **`<runtime> run --rm -i …` のクライアント**であって
ハーネスではない。`killpg` は別の PID 名前空間には届かず、しかも `grace` 後の SIGKILL でクライアントを
殺すと（docker では）`--rm` の後始末が走らず**コンテナが取り残される**。そこで
`task_worker::kill_tree_with(run_id, grace, Option<Arc<dyn ContainerStopper>>)` を**唯一の合流点**とし、
ホストへの 2 段と**同じ瞬間**に `--label celeris.task=<task_id>` 越しの 2 段を送ることにした:
すぐ `<runtime> kill --signal TERM <ids>`（中のハーネスに片付けの猶予を与える）、`grace` 後に
`<runtime> rm -f <ids>`（A3 の `stop_by_label`。取り残しを消す）。口を持ち回すのは
`ContainerStop { program, task_id }` だけで（`ContainerPlan` 全体は要らない）、ディスパッチャが
dispatch のときに `ContainerDecision::Container` から作って `RunEntry` に置き、`stop_run` が渡す。
`container` が `None` なら従来と 1 バイトも変わらない。**レビュー側（`stop_review`）は渡していない**:
A3 の時点でレビューの判定コマンド・Reviewer run はホストで走る（Phase 56 の「未解決」）ので、
コンテナの口が無い。レビューもコンテナに入れるときは、同じ `RunEntry.container` を `ReviewEntry` にも
置いて `stop_review` から渡せばよい（合流点は 1 つのまま）。

## 8. Phase 57 追記（2026-09-19。B3 の実装で D7 から離れた／D7 が決めていなかったところ）

D7 を実装して決めた 6 つ。**決定そのものは変えていない**（本文は読み替えない）。詳細と証拠は
`docs/PROGRESS.md` の Phase 57（P57-1〜P57-6）にある。

1. **文書リポジトリを作るのは人が押したときだけ**（P57-1）: D7 は「primary が `dir` か無い案件は Celeris が
   作る」としか書いておらず、いつ作るかを決めていない。読み取り（ツリー・ページ）はトークンが要らないので、
   そこで `git init` と `project_repos` への登録をすると**無認証の GET が案件を書き換える**。そこで
   **`POST /projects/{id}/docs/init`（管理系）**を足し、ページの保存（`PUT`）と昇格からも同じ経路で作る。
   読み取りは何も作らず 409 `docs_unavailable`（GUI は「文書を用意する」ボタンを出す）。

2. **パスはリポジトリ相対で、根を省いても届く**（P57-2）: D7 は `path` の基準を決めていない。昇格の例
   （`docs/research/xxx.md`）に合わせて**リポジトリ相対**にし、文書の根で始まっていなければ根の下だと
   解釈する（`?path=research/xxx.md` も同じページ）。`..`・絶対パスは 403、`.md` 以外は 422。

3. **リモートの primary は未対応**（P57-3）: `location.kind = "remote"` のリポジトリは taskd からファイルが
   見えない（ADR-0018 / 0019 の同期の先にある）。この Phase では 409 `docs_unavailable` にし、
   `~/workspace/` に別のリポジトリを勝手に作ることはしない。

4. **GUI は `html` ではなく `raw` を描く**（P57-4）: D7 は「描画はサーバ側で決定的に」と決めており、API は
   そのとおり `html` を返す（生 HTML は捨ててある）。ただし GUI 側の規律（`gui/CLAUDE.md` の禁止事項）に
   `dangerouslySetInnerHTML` があるので、画面は `react-markdown` で `raw` を描く。`celeris:task/<id>` と
   `[[相対パス]]` の開き方は `gui/app/lib/docs.ts` の純粋関数に置いて単体テストで押さえた。`html` は
   契約として残る（GUI 以外の読み手のため）。

5. **既定の文書リポジトリの置き場は設定で渡す**（P57-5）: `~/workspace` は taskd が `$HOME` から組んで
   `ApiSettings.docs_repo_root` で API に渡す（`None` なら 409）。テストが人の `$HOME` を触らずに
   tempdir で回せるようにするため（環境変数の書き換えはしない）。

6. **front matter は最小の自前パーサ**（P57-6）: `serde_yaml` は入れず、`title` / `tags` / `tasks` の
   3 つだけを読む（`[a, b]` と `- a` の両方。閉じていない `---` は front matter として扱わない）。
   Markdown の描画は `pulldown-cmark`（`default-features = false`、`html` のみ）を足した。
