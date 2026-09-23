# ADR-0062: ssh master の keepalive と実通信 probe、remote 作業場所は `cluster:<id>` を持つ担当だけ

- 日付: 2026-09-23
- 状態: **Accepted**（Phase 107 として実装する。本番の観測への対応、および人間の追加指示）
- 関連: ADR-0018 D2/D5（クラスタ接続の判定・供給側失敗）、ADR-0032 D2（`ControlPersist` の訂正）、
  ADR-0039 D2（作業場所の継承）、ADR-0046 D5/D8（matching、tools の語彙）、ADR-0060（master を
  celeris の cgroup の外で起こす）、ADR-0033 D5（人への質問・blocked）

## 1. 文脈

2026-09-23 の本番運用で 3 つの不整合が観測された。

1. **sirius の ssh master が黙って死ぬ**: `Host sirius`（`ControlPersist 10`、`ServerAliveInterval 0`）
   は 01:01 に接続 → 01:10 に `-O check` 失敗 → 再接続 → 03:42 に run が使おうとして
   `mux_client_request_session: read from master failed: Broken pipe` → TOTP 無しでは入れず master
   消滅。celeris が起こす master は `ssh -o BatchMode=yes -M -N <host>` だけで **keepalive を
   付けていない**ため、NAT / ファイアウォールの idle timeout で TCP が黙って死んでも、`-O check`
   （unix socket を見るだけ）は「Master running」を返し続ける。
2. **担当に `cluster:<id>` が無いタスクが ready のまま放置され、log が毎 tick 出る**: CoS の計画が
   案件の remote workspace を**担当を問わず**子タスクに継がせたため、web-research / literature-research
   のような調査タスクまで sirius 行きになった。`cluster_of` が None → 「no such cluster in the
   config; task left ready」という**誤解を招く文言**（設定に無いのではなく担当に道具が無い）→
   `unroutable` に入り、`assignee_may_use_cluster` の warn が dedupe 無しで 2 秒ごとに出続けた
   （2 時間で 1,500 行超）。
3. **テストの偽 ssh master が漏れる**: `cluster_login.rs` のテストが起こす偽 ssh（`-M -N` で
   `while true; do sleep 3600; done` のように無限にブロックするスクリプト）が、アサーション失敗
   （`ClusterMaster` に Drop が無い。ADR-0060）でテストが `panic!` すると後片付けを経由せず、
   本番ホストに 35 プロセス（ppid 1）が溜まっていた。

追加で、人間から次の指示があった: 「web-research などが不必要に remote で作業しないようにして」。
調べると、案件の remote workspace が担当の道具を無視して**すべての子**に継がれる（ADR-0039 D2）
ことが根本原因で、「担当が cluster:<id> を持たなければ blocked」だけでは、明示していない継承の
ケースを塞げないと分かった。

## 2. 決定

### D1. master の keepalive（`[[clusters]] keepalive_secs`、既定 30 秒）

- master の argv（`start_publickey`/`start_totp` 両経路）に `keepalive_secs > 0` のとき
  `-o ServerAliveInterval=<n> -o ServerAliveCountMax=3 -o TCPKeepAlive=yes` を足す
  （`crates/task-worker/src/cluster_login.rs::keepalive_args`）。コマンドラインの `-o` は
  `~/.ssh/config` より優先されるので、人の `ServerAliveInterval 0` を書き換えずに効く。
- `0` で無効（既定は 30）。GUI 手動接続（`cluster_admin.rs`）・ディスパッチャの自動接続
  （`cluster_connector`）の両方に、クラスタごとの `keepalive_secs` を渡す。

### D2. master 越しの実通信による生存確定（`[[clusters]] liveness_probe_secs`、既定 300 秒）

- `-O check` は unix socket を見るだけで、NAT 越しの TCP 切断を検出できない。既存の
  `refresh_cluster_liveness`（5 秒に 1 回の `-O check`）に加えて、`-O check` が生存を返した
  クラスタのうち `liveness_probe_secs` が経過したものに対し、`ssh -o BatchMode=yes <host> -- true`
  （タイムアウト 10 秒。`task_worker::ssh::control_master_command_probe_blocking`）を 1 回流す。
  失敗したら「接続が死んだ」と判定し、`cluster_connected` を `false` にして cooldown へ落とし、
  `ssh -O exit`（`ClusterDisconnector`）で残骸を片付ける。
- 目的が違うので keepalive と両方持つ: keepalive は OS レベルの**検出**（早く気づく）、probe は
  taskd 側での**確定と後片付け**。
- `[[clusters]] keepalive_secs` / `liveness_probe_secs` はどちらも celeris の `build_dispatcher`
  では配線しない（テストから広く呼ばれるため、実 ssh を打つと CLAUDE.md の「テストで外部ネットワーク
  に出ない」を破る）。本番の起動経路だけで `wire_cluster_liveness_hooks` を呼ぶ。

### D3. master の終了を検出し、`Event::ClusterMasterExited` を 1 回残す

- `ClusterMaster` に `stderr_buf`（`spawn_master` の汲み出しタスクと共有）を持たせ、
  `try_wait_exit()`（非破壊）と `stderr_tail(max_bytes)` を足す。
- celeris は tick ごとに `ClusterMasterWatcher`（`cluster_master_watcher`）で `ClusterMasters` を
  非破壊にスキャンし、終了していたら取り除いて `(cluster, exit_code, stderr_tail)` を返す。
- ディスパッチャはこれを `cluster_disconnect_info` に貯め、次に `mark_cluster_unavailable` が
  そのクラスタを拾ったとき（`reason` を組み立てるとき）に **stderr の末尾（最大 300 バイト）と
  exit code を reason に足し**、`Event::ClusterMasterExited` を 1 回だけ記録する（`reported` フラグ。
  cooldown が明けて接続が戻れば消す＝次に切れたら新しい詳細でまた 1 回）。
- 同じ仕組み（`cluster_disconnect_info`）を D2 の probe 失敗でも使う（`exit_code = None`、
  `detail` に probe が失敗したことを書く）ので、報告の文面は原因を問わず一貫する。
- `auth = "totp"` のクラスタでは、`mark_cluster_unavailable` の reason に「GUI の『クラスタ』画面から
  TOTP を入力して再接続してください」を足す（ADR-0053 D3 の `report_for_cluster_login_needed` と同じ
  案内を、`ClusterUnavailable` の経路にも通す）。
- 同じ切断についての報告は、既存の `cluster_report_recently_recorded`（cooldown の間は 1 件）が
  そのまま効くので、5 分ごとの再送にはならない。

### D4. `cluster_of` が `None` の理由を分け、担当の道具不足は blocked + 質問にする

- `resolve_cluster`（新設。`cluster_of` はこれを畳んだだけ）が `ClusterResolution` を返す:
  `Local` / `Resolved` / `NotConfigured`（(a) 設定に無い）/ `AssigneeLacksTool`（(b) 担当が
  `cluster:<id>` を持たない）。
- (a) は従来どおり `warned_unroutable` の dedupe と「no such cluster in the config」の警告、
  `unroutable` に入れる。
- (b) は `block_task_missing_cluster_tool` を呼び、`assign_if_needed` の `Assignment::Unroutable`
  と同じ流儀（`approvals` に 1 件、`Trigger::Unroutable` で `ready → blocked`）で人に聞く。文面:
  「担当 `<node>` には道具 `cluster:<id>` が無いため、このタスクは `<cluster>` で実行できません。
  組織画面で担当の tools に `cluster:<id>` を足す、担当を `<candidates>` などに変える、または
  作業場所をローカルに変えてください」。候補ノード（`cluster:<id>` を持つノード）を列挙する。
- タスクは `blocked` になるので、次 tick の `ready_tasks()` には出てこず、2 秒ごとの warn の
  洪水は自然に止まる。人が答える（または担当・tools を変える）と `ready` に戻り、次 tick で
  `cluster_of` が再評価する（自動での再判定は行わない。既存の matching の unroutable と同じ設計）。
- `task_may_use_cluster` の warn も `warned_cluster_tool`（`TaskId` の `HashSet`）でタスクごとに
  1 回に dedupe する。

### D5. `cluster:<id>` の例外を廃止し、matching と `create_task` の検証で強制する

人間の追加指示により、ADR-0046 D8 の「道具を 1 つも宣言していないノードは remote も従来どおり通す」
という例外を、**クラスタの利用可否についてだけ**廃止した（`tavily`/`gh` などの他の道具の扱いは
変えていない）。本番の組織は既に `software-engineering` / `systems-performance` / `cluster-hpc` に
`cluster:*` を付けているので、この変更で実害のある担当は無い。

- `task_may_use_cluster`（dispatcher）: `effective.tools.is_empty()` の早期 `true` を削除。
- `task_ops::matching::decide`: `task.workspace` が `Remote{cluster}` なら、候補ノードを
  `cluster:<id>` を持つものだけに絞る（`harnesses.allowed` の条件に追加。tools が空のノードも
  除外される）。候補が無ければ `Unroutable`（質問の文面にクラスタの道具名を足す）。
- `task_ops::actions::create_task_action`（Console の `create_task`）: `workspace` が
  `Remote{cluster}` かつ `assignee` が明示され、その担当が `cluster:<id>` を持たないなら action
  全体を検証で落とす（`FailedAction`。候補ノードを列挙）。`assignee` 省略時は matching に委ねる
  （上記の候補フィルタが効く）。

### D6. 作業場所の継承は、担当が `cluster:<id>` を持つときだけ Remote のまま。持たなければ Local に落とす

ADR-0039 D2 の「子は案件・親の作業場所を継ぐ」は担当を見ていなかったため、案件が Remote なら
**担当を問わず**子が Remote になっていた（web-research の調査タスクが sirius 行きになった実例）。

- `task_core::delegate::downgrade_inherited_remote_if_needed`（純関数）: 継承した（そのタスク自身は
  `workspace` を明示していない）Remote workspace を、決まった担当（`resolve_child_defaults` が
  返す assignee）が `cluster:<id>` を持たなければ、タスク専用の Local（`workspace_root/<task_id>`
  と同じ形の相対パス）に落とす。**明示した workspace は落とさない**（そちらは D5 の検証で拒否する
  別経路）。担当が未定（matching に委ねる）なら、その場では判定せず Remote のまま返す
  （matching が `cluster:<id>` を持つノードだけを候補にするので、後で矛盾しない）。
- `task_core::delegate::materialize_delegated`（委譲の子）と `task_core::plan::materialize`
  （plan の子）の両方の workspace 組み立てにこの関数を通す。**関数のシグネチャ（`Vec<Task>` を返す）は
  変えていない**（既存の大量のテストを壊さないため）。ログ用に `materialize_delegated_logging` /
  `plan::materialize_logging`（`on_downgrade: &mut dyn FnMut(TaskId, &str)` を追加で受け取る変種）を
  新設し、`materialize_delegated`/`materialize` はこれを無視するクロージャで呼ぶ薄いラッパーにした。
  ディスパッチャ（`task-dispatch::dispatcher.rs`、`task_ops::delegate::plan_delegation` の
  `DelegationOutcome.workspace_downgrades`）はログ変種を使い、降格が起きるたびに tracing::info! で
  1 回残す（`"workspace inherited as local: assignee <id> has no cluster:<cluster>"`）。
- `create_task`（Console action）と `plan.json` の子タスクで**明示**された remote workspace は、
  D5 の検証でそのまま拒否する（継承だけがこの D6 の対象）。

### D7. テストの偽 ssh が残らないようにする

`cluster_login.rs` のテストが起こす偽 ssh（`-M -N cluster-host` を模す）は、`-M -N` の待ちを
`while true; do sleep 3600; done` ではなく **`while kill -0 "$PPID" 2>/dev/null; do sleep 0.2; done`**
に置き換えた（親＝テストのプロセスが消えたら自分も終わる）。これにより、アサーション失敗で
`ClusterMaster::kill()`/`wait_until_process_gone` を経由せずにテストが `panic!` しても、
テストバイナリの終了とともに偽 ssh が自分で終了する（ADR-0060 で `ClusterMaster` の Drop を
外した副作用の後始末）。本番コードの挙動（接続成立後は Drop で殺さない）は変えていない。
`detach.rs`/`releases.rs` の偽 `systemd-run` 経由のテストは確認したところ、すべて有限の
`sleep 2` 以下か即終了のスクリプトで、この問題を持たない（変更していない）。

## 3. 採らない

- **cluster:<id> を持たない担当への remote の許可を、`create_task`/plan.json 双方で「継承」扱いに
  する**。明示は明示のまま検証で拒否する（D5）。曖昧にすると「なぜ動かないか」が分かりにくくなる。
- **担当・tools の変更を検知して blocked のタスクを自動で ready に戻す**。ADR-0046 D5 の matching の
  unroutable と同じ設計（人が答えるまで待つ）に揃えた。
- **TOTP クラスタの自動再接続**。ADR-0032 D3 のまま、`publickey` だけが自動接続の対象。
- **`liveness_probe_secs`/`keepalive_secs` を celeris の起動経路以外（`build_dispatcher`）でも
  実 ssh を打つように配線する**。テストの安全（CLAUDE.md）を優先した。

## 4. 受け入れ条件（Phase 107）

1. `[[clusters]] keepalive_secs`（既定 30）/ `liveness_probe_secs`（既定 300）。master の argv に
   keepalive の `-o` が付く（`0` で付かない）ことをユニットテストで確認。
2. 実通信 probe が失敗したら接続を死んだ扱いにし、`-O exit` の片付けフックを 1 回呼ぶ
   （`cluster_disconnector`）。
3. `ClusterMaster::try_wait_exit`/`stderr_tail`、`ClusterMasterWatcher`、
   `Event::ClusterMasterExited`（`event.schema.json` 再生成）。celeris が保持する master が自分で
   終了したら、次の `mark_cluster_unavailable` で 1 回だけ報告に stderr の末尾と exit code が入る。
4. 担当に `cluster:<id>` が無い remote タスクは `blocked` + 質問（`no such cluster in the config`
   ではない）。matching は `cluster:<id>` を持つノードだけを remote タスクの候補にする（tools が
   空のノードも例外なし）。`create_task`（明示 assignee + 明示 remote）は検証で落ちる。
5. 継承した Remote workspace は、担当が `cluster:<id>` を持たなければ Local に落ちる。持てば
   Remote のまま。明示した workspace は落とさない。
6. `cargo test --workspace --no-fail-fast`（FAILED 0）、
   `cargo clippy --workspace --all-targets -- -D warnings`（exit 0）。
7. `cluster_login.rs` のテストが起こす偽 ssh は、テストバイナリの終了後に残らない
   （`pgrep -f 'ssh -M -N cluster-host'` が実プロセスとして 0 件）。
8. 実機（親エージェントが行う）: sirius に再接続し、keepalive 付きの master が 1 時間以上維持
   されること、`ClusterMasterExited` が切断時に stderr 付きで残ること、担当に道具が無い 3 件が
   blocked + 質問になること。

## Phase 108 追記（2026-09-23）

Phase 107 の本番反映で B1 が正しく `blocked` + 質問を出すようになったが、**その質問に答えて先へ進む
手段が無い**ことが分かった。`PATCH /tasks/{id}`（`TaskEdit`）は `workspace` を受けず、
`POST /tasks/{id}/retry` は元の `workspace` をそのまま複製する。web-research / literature-research の
remote タスク（案件から継承した `workspace`）を Local に直して進める道が無かった。加えて、codex
アダプタ（`sandbox_mode=workspace-write`）の run で `.celeris/remote-exec` が
`ssh: Bad owner or permissions on /etc/ssh/ssh_config.d/20-systemd-ssh-proxy.conf`（exit 255）で
止まった。このファイルはホスト上では root 所有の symlink で、人のシェルからの ssh は問題なく通るが、
codex のサンドボックス内ではその Include 先が別所有者に見えるか読めず、ssh が設定の検査で拒否したと
見られる。ワーカーの ssh はシステム全体の `/etc/ssh/ssh_config` を読む必要が無く、人の
`~/.ssh/config`（Host 別名・ControlPath・ControlMaster を持つ）だけで足りる。

### E1. `PATCH /tasks/{id}` と `POST /tasks/{id}/retry` が `workspace` を受ける

- `TaskEdit.workspace: Option<WorkspaceSpec>`。許す状態は `draft` / `ready` / `blocked` / `failed`
  だけ（`running` / `reviewing` / `done` / `cancelled` は 409 `invalid_transition`）。`failed` は
  他の項目の編集では終端として拒むが、`workspace` の差し替え〈やり直しの前準備〉だけは例外的に許す。
  検証は `create_task`/`PATCH /projects/{id}` と同じ規則（`Remote.cluster` は API 層の
  `validated_workspace` が設定の存在を見て `~` を展開する。`Local.path` は空でないこと。明示の
  `Remote` でその時点の担当が `cluster:<id>` を持たなければ 422 — B3 と同じ規則で、`assignee` を
  同じ `PATCH` で変える場合は変更後の担当で判定する）。
- `blocked`（B1 の unroutable）だったタスクは、`workspace` または `assignee` の変更で上の検証を
  通り経路が通るなら、**その場で `ready` に戻す**。実装は新しい判断を作らず、既存の「質問に答える」
  経路（`gate::answer` と同じ `Trigger::Answer` + `Event::Answered` + 未決の `approvals` の
  settle）に相乗りする（答えの文は固定で「解決済み（作業場所/担当の変更）」）。判定は
  `Trigger::Unroutable` の `Event::Transitioned{reason:"unroutable"}` が直近の `Blocked` 遷移かどうかで
  行い、worker が聞いた質問による `blocked`（`Trigger::WorkerQuestion`、reason `"worker_question"`）
  とは区別する（`task_ops::derive::latest_block_is_unroutable`）。
- `POST /tasks/{id}/retry` の本文に `workspace`（省略可）を足す。与えれば複製先の `workspace` を
  差し替える（検証は `PATCH` と同じ）。省略時は従来どおり元のタスクの `workspace` を複製する。
- 検証（`cluster:<id>` の有無）は `task_ops::matching::assignee_has_cluster_tool` に 1 か所へまとめ、
  既存の B3（`create_task_action`）と同じ規則をここからも使う。

### E2. `.celeris/remote-exec` は `ssh -F "$HOME/.ssh/config"`（無ければ何も足さない）

- ヘルパのスクリプト自身が実行時に `[ -f "$HOME/.ssh/config" ]` を判定し、あれば
  `set -- -F "$HOME/.ssh/config"` として `ssh` の直後にその引数を挿む（`sh` の生成物なので、
  celeris のプロセスではなく**ワーカーが実行する環境**の `$HOME` で判定される）。これにより
  システムの `/etc/ssh/ssh_config`（と Include 先）を一切読まず、codex サンドボックス内での
  所有者検査の拒否を避ける。
- celeris 自身が打つ ssh（`ControlMaster` の起動・`-O check`・D2 の実通信 probe・`Check::Command` の
  remote 実行・rsync の `-e`）は変えない（サンドボックスの外、celeris のプロセスとして動くため
  問題が起きない）。

### E3. 採らない

- B1 以外の理由（担当なしの一般的な matching unroutable、worker の質問）で `blocked` になったタスクを
  同じ経路で自動的に `ready` へ戻す一般化。今回は「`cluster:<id>` の検証を通った」という強い証拠がある
  ときだけ相乗りする。ワーカーの質問への回答は従来どおり `POST /tasks/{id}/answer` /
  コメント経由（ADR-0044 D2）。
- GUI のタスク編集フォームに作業場所の入力を足すこと（`docs/PROGRESS.md` の提案に送る。今回は API と
  ヘルパだけ）。

### E4. 受け入れ条件（Phase 108）

1. `TaskEdit.workspace` の状態ガードと検証、B1 blocked → ready の復帰（質問クローズ）が
   ユニットテストで確認できる（`crates/task-ops/src/edit.rs`）。
2. `retry_task` が `workspace` の差し替えを受け、検証が `PATCH` と同じであることがユニットテストで
   確認できる（`crates/task-ops/src/retry.rs`）。
3. `write_remote_exec_helper` が生成するスクリプトが `$HOME/.ssh/config` の有無で `-F` の有無を
   切り替えることが、生成文字列とスクリプトの実行の両方のテストで確認できる
   （`crates/task-worker/src/ssh.rs`）。
4. `cargo test --workspace --no-fail-fast`（FAILED 0）、
   `cargo clippy --workspace --all-targets -- -D warnings`（exit 0）。
5. 実機（親エージェントが行う）: web-research / literature-research の remote タスクを `PATCH`/`retry`
   で Local に直して進めること、software-engineering の codex run で `remote-exec` が
   `-F ~/.ssh/config` で通ること。
