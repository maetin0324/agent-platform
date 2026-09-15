# taskd への依頼

GUI 側で回避せず、taskd の API に足りない・仕様（`docs/taskd-api-v1.md`）と違う点を書く。書いたら `docs/PROGRESS.md` に `## Phase G<N> — BLOCKED` を書いて止まる。

## 未対応

## 対応済み

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
