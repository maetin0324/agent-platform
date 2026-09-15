# ADR-0015: 要求・tick の所要時間ログ、ネットワーク FS の警告、`TaskRef` / `TaskSummary` の `actions`

- 日付: 2026-09-15
- 状態: Accepted（人間の判断: R1 は taskd 側で調査して直す、Inbox の cancel 判定は taskd が `actions` を返す）
- 関連: ADR-0013 D3 / D4 / D5（API 層・デーモン状態・WAL）、ADR-0014、`docs/gui/api.md` §5.4、`taskd-gui` の `docs/taskd-requests.md` R1 と G2-U6

## 文脈

`taskd-gui` の G フェーズから 2 件の指摘が来た。

- **R1（調査依頼）**: ブラウザ + SSE 中継が接続している間、変更系の `POST` の直後に taskd の tick・SSE・（時に）API が 10〜30 秒止まる。
  GUI 側からは原因を特定できない。要求ごとの所要時間ログが taskd にあれば切り分けられる、という依頼つき。
  - 現時点で分かっていること: GUI の fixture の DB は `/home`（**NFSv4**）にある。ADR-0013 D5 は「WAL はネットワークファイルシステム上では使えない。
    DB はローカルディスクに置く」と決めていたが、**その前提を破っていることを taskd は何も言わない**。
  - 同じ操作をローカルディスク（ext4）と NFS で比べると、承認→done の最悪値は 458 ms 対 817 ms で、NFS は明確に遅い。ただし 12 回程度の
    合成負荷では 10〜30 秒の停止は再現しなかった。原因の特定には taskd 側の計測が要る。
- **G2-U6**: 受信箱の `attention` 区画で、GUI が `status` を見て「非終端なら cancel できる」と判定していた。`docs/gui/api.md` §5.4 の規則の
  再実装であり、原則（GUI は派生値を再計算しない）に反する。`Inbox` の項目に `actions` が無いのが原因。

## 決定

### D1. API は要求ごとの所要時間を記録する

- `guard` ミドルウェア（全要求が通る）で所要時間を測り、`method` / `path` / `status` / `duration_ms` / `request_id`（`X-Request-Id` と同じ）を出す。
- 既定は `debug`。**1 秒以上かかった要求は `warn`**（`slow api request`）。長時間つなぎっぱなしが正常な `GET /stream` は警告の対象外。
- 応答本体やクエリの値は出さない（秘密が混ざらないよう、パスだけ）。

### D2. デーモンは tick の所要時間を記録し、遅い tick を警告する

- `tick()` の所要時間を測り、`max(1 秒, tick_ms × 2)` を超えたら `warn`（`slow tick`）。tick の間隔と実所要時間の差から、
  「止まっているのはディスパッチャか API か」を後から切り分けられる。

### D3. DB がネットワークファイルシステム上にあれば起動時に警告する

- `/proc/self/mountinfo` を最長一致で引き、DB の置き場のファイルシステム種別が `nfs` / `nfs4` / `cifs` / `smb3` / `9p` / `afs` / `ceph` /
  `lustre` / `gpfs` / `beegfs` / `glusterfs` / `fuse.*` のいずれかなら `warn`（起動は止めない。読めない環境では何もしない）。
- 理由: ADR-0013 D5 の前提（ローカルディスク）を破ると、SQLite の WAL は共有メモリのインデックスとロックが正しく働かず、
  停止・`database is locked`・破損の原因になる。**破っていることが分かるようにするのが taskd の責務**で、置き場を決めるのは運用側。

### D4. `TaskRef` と `TaskSummary` に `actions` を持たせる

- `task_ops::view::actions(task)`（§5.4 の規則）の結果を、参照（`TaskRef`）と一覧の項目（`TaskSummary`）にも入れる。
  受信箱・一覧・DAG・依存関係のどこから来た参照でも、GUI は `actions` を見るだけでボタンを決められる。
- `TaskDetail.actions` は従来どおり（`TaskDetail.task` は `Task` そのものなので重複しない）。
- 代替案「GUI が常にボタンを出し、無効なら 409 を見せる」は採らない。押せないものを押させるより、taskd が「今できること」を返す方が原則に合う。

## 結果

- `docs/api/v1/api-v1.schema.json` を再生成する。`taskd-gui` は `pnpm gen:types` の再生成が要る。
- `docs/gui/api.md` の §5.4 / §6.2 を更新する。
- R1 の調査は、この計測を入れた taskd で GUI の e2e を再現して続ける（結果は `docs/PROGRESS.md` に記録する）。
