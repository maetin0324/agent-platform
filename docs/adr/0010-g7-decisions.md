# ADR-GUI-0010: Phase G7（クラスタと委譲の表示）で確定した細部

- 日付: 2026-09-16
- 状態: **Accepted**（Phase G7 の実装で確定。docs/DESIGN.md §10 Phase G7 の範囲内の細部）
- 関連: docs/DESIGN.md §10 Phase G7 / docs/taskd-api-v1.md §3.23（`GET /clusters`）, §5.1 (d)（`cluster_unavailable`）,
  Task の `role` / `delegated[]` / `cluster` 節 / docs/adr/0016, 0018（taskd 側）

## 1. 文脈

G7 は taskd の Phase 10（役割と委譲、ADR-0016）と Phase 12（クラスタでのコマンド実行、ADR-0018）で増えた情報を表示する（light）。
API は揃っている前提で、GUI は表示と導線だけを足す。fixture 構築と画面実装で確定した細部を記録する。

## 2. 決定

### D1. `fixture clusters` は実際に `~/.ssh/config` の `taskd-localhost`（localhost への ssh 多重接続）を使う

`ClusterLive.connected` は `ssh -O check` の結果そのもの（taskd 側で計算済み、GUI・fixture 側では再計算しない）なので、
「接続あり」を作るには本物の多重接続が要る。この環境には Phase 12 の受け入れテスト（`agent-platform/tests/e2e/tests/cluster_scenarios.rs`）
と同じ `Host taskd-localhost`（`HostName 127.0.0.1`、`ControlMaster auto`）が `~/.ssh/config` に既にあり、多重接続も張られていた
（`ssh -O check taskd-localhost` が成功）。`fixture clusters` はこれを前提にし、無ければ `die` で明確に失敗する
（`ssh -MNf taskd-localhost` を先に張るよう指示）。「接続なし」は `taskd-no-such-host-for-tests`（`cluster_scenarios.rs` と同じ、
DNS 解決だけで失敗し外部ネットワークには出ない）を host にした 2 つ目の `[[clusters]]` で作る。

### D2. `fixture clusters` の Remote タスクは taskd 本体（cargo でビルドした実バイナリ）に対して行う。push/pull/判定は実際に ssh 越しに行われる

`agent-platform/tests/e2e/tests/cluster_scenarios.rs` の `remote_task_syncs_runs_and_is_checked_on_the_cluster` と同じ構成
（`secret.txt` をクラスタ側にだけ置き、判定コマンド `grep -q cluster-only answer.txt` はクラスタ側で実行される。
GUI 側の fake ワーカー（`test/taskd/fixtures/clusters-worker.sh`）は pull 済みの手元の写しで `secret.txt` を `answer.txt` に
コピーするだけ）。taskd の crate には依存せず、`scripts/taskd.sh` からビルド済みバイナリを呼ぶだけなので CLAUDE.md の禁止事項に抵触しない。

### D3. `fixture delegation` の親タスクの受け入れ条件は、委譲した子を待たせる前に評価される値を使う（`true`）

`crates/task-dispatch/src/dispatcher.rs` の `review_task`（`all_pass` の判定）を確認すると、「委譲した子が終端でなければ判定だけ記録して
`reviewing` のまま待つ」（`pending_children > 0` の分岐）は **`all_pass == true` のときだけ**通る。判定は run のたびに（delegate した最初の run
にも）行われるため、`test -f artifacts/summary.md`（集約 run でしか作らないファイル）のような条件を親の受け入れ条件にすると、最初の
delegate run の直後に `all_pass=false` となって `Ready` へ差し戻され、`needs_aggregate_run` に一度も到達しないまま `delegate` を繰り返し、
`max_retries`（既定 2）を使い切って `failed` になる（実測。デバッグ用の fake ワーカーで `context.children` を出力させて確認した）。
`--check-cmd true` にすると、delegate 直後の判定は無条件に pass し、`pending_children > 0` で待ち → 子が終端 → 集約 run（`context.children` が
非空で渡ってくる）→ 集約 run 自身の判定も pass → 通常どおり `done` になる。集約 run が `artifacts/summary.md` を書く（GUI の成果物一覧で確認できる）
ことと、親タスクの受け入れ条件そのものは切り離した。

### D4. DAG での「委譲で生まれた子を親の下に寄せる」は既存の `layoutGraph`（`app/lib/graph-layout.ts`）がそのまま満たす

`delegate` で作られた子タスクは `parent_id` が委譲元のタスクになる（taskd 側 `task-core::store::delegate_children` の制約
`child.parent_id != Some(parent_id)` はエラーになる）。`layoutGraph` は既に `GraphNode.parent_id` ごとに group ノードを合成している
（G3、ADR-0006 D5）ので、追加のコードなしで委譲の子も親の下に配置される。G7 では `graph-layout.ts` を変更していない。

### D5. 一覧・DAG のノードに役割（`role`）のテキストラベルを出すことは、taskd への依頼として保留した（実装しない）

DESIGN §10 Phase G7 の実装節は「一覧と DAG のノードに役割を出す」と書くが、`docs/taskd-api-v1.md` の `TaskSummary`
（`GET /tasks` の一覧行）にも `GraphNode`（`GET /graph` のノード）にも `role` フィールドが無い（`role` は `TaskDetail` にしか無い、
ADR-0016 D1「GUI の表示用に最上位にも出す」は詳細画面だけを指す）。行・ノードごとに `GET /tasks/{id}` を追加で呼んで埋める案は、
一覧・DAG の意図された使い方（1 回の一覧取得で完結する）を超える N+1 の呼び出しになり、CLAUDE.md「taskd API の仕様外の挙動に頼らない」
「判断ロジックの再実装をしない」の精神に反すると判断し、行わなかった。この API 拡張は G7 の受け入れ条件（1〜7、番号付きのもの）には
含まれない実装節側の記述であり、`docs/taskd-requests.md` に依頼を書いて次フェーズ以降に回す（下記「未解決事項」「taskd への依頼」）。

### D6. `/clusters` は `GET /clusters` 1 回だけで完結する

`ClusterView` は設定（`[[clusters]]`）と `DaemonSnapshot.clusters[]`（`ClusterLive`）を taskd 側で結合済み（`cooldown_remaining_secs` も
サーバ側で計算済み）。`app/routes/providers.tsx` の `cooldown` のように GUI 側で `fetchedAt` を基準に減算する必要が無いため、
`/daemon`（`GET /daemon`）は呼ばない。

### D7. 受信箱の `cluster_unavailable` は他の `attention` 項目と見た目のパターンは揃えつつ、`/clusters` への遷移を追加する

G6 時点（ADR-0009 D5）では「崩れないように文字列だけ出す」暫定対応だった部分を、G7 の受け入れ条件 2 のとおり `/clusters` へのリンクにした。
`AttentionItem` の `cluster_unavailable` バリアントは `task` を持たない（クラスタ単位の集約）ため、他の 3 種（`task` を持つ）とは
別枝のまま維持する（ADR-0009 D5 で既に分岐済み）。

### D8. タスク作成フォームの `role` は `GET /config` の `roles[]` を候補に出しつつ自由入力を許す

`NewTaskSpec.role` は自由記述（ADR-0016 D1、`[[roles]]` に無い名前でもエラーにならず名前だけ保存される）なので、`<select>` で候補に
縛らず、`tasks.new.tsx` が既に持つ `depends_on` の「候補 + 自由入力欄」と同じパターン（`<datalist>`）にした。`aggregate` は
`NewTaskSpec.aggregate?: boolean` なのでチェックボックス 1 つ、未チェックならフォームから送らない（`buildNewTaskSpec` の他の任意項目と
同じ「空欄は本文から省く」規則、ADR-0005 D5）。

## 3. 未解決事項（この ADR の範囲で判明したもの）

- D5 のとおり、`TaskSummary` / `GraphNode` に `role` が無いため、一覧・DAG のノードへの役割ラベル表示は実装していない。
  `docs/taskd-requests.md` に依頼を記録する。

## 4. taskd への依頼

- `TaskSummary`（`GET /tasks` の一覧行）と `GraphNode`（`GET /graph` のノード）に、`role`（`Option<String>`、`TaskDetail.role` と同じ規則）を
  追加してほしい。無いと GUI 側は一覧・DAG に役割を出すのに N+1 の `GET /tasks/{id}` を要し、CLAUDE.md の「派生値の再計算・仕様外の挙動に
  頼らない」方針と衝突する。詳細は `docs/taskd-requests.md`。
