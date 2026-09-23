# ADR-0064: DB をローカルディスクへ置き、I/O 遅延に強くする

- 日付: 2026-09-23
- 状態: **Accepted**（Phase 110a として実装する。本番の観測への対応）
- 関連: ADR-0013 D5（WAL・busy_timeout・`BEGIN IMMEDIATE`）、ADR-0040 D1/D2（selfdeploy の触ってよい
  場所）、ADR-0045 D2（`~/.local/celeris` の XDG 流の置き場）

## 1. 文脈

本番の SQLite DB（`~/.local/celeris/celeris.sqlite3`、26 MB、events 38k 行）は `/home` にあり、
`/home` は `/dev/loop0`（ext4）で、その実体は LXC ホストが NFS（truenas）からマウントした raw
イメージファイルだった。fsync 1 回が 69 ms（ローカル LVM `/` は 8 ms、tmpfs は 0.01 ms）で、
2026-09-23 18:36〜18:55 に claude-code の実装タスクが 2〜3 本並走し、別々の worktree（同じ loop
デバイス上）で `cargo test --workspace` と GUI ビルドを回した結果、I/O PSI が full 47% まで悪化。
SQLite の書き込みロック保持が `busy_timeout`（既定 5000 ms）を超え、API が 503 `db_busy`
（`POST /api/v1/console/instruct` 5005 ms）、GUI が 15 秒でタイムアウトした（`GET /board` 503
15007 ms）。

構造上の要因:

- `SqliteStore`（`crates/task-core/src/store.rs`）は書き込み用の `Mutex<Connection>` 1 本。デーモン
  内には dispatcher / task-api（`spawn_blocking` 経由）/ celeris-mcp / llm-proxy のログ用と別々の
  接続があり、接続間は `busy_timeout` で待つ（ADR-0013 D5）。読み取りも書き込みと同じ 1 本の
  `Mutex` を取っていたため、書き込みトランザクションの間は読み取りも待たされる。
- `wal_autocheckpoint` は既定（1000 ページ）のままで、チェックポイントは**コミットした接続の中で**
  走る（fsync がリクエストの中に落ちる）。
- `warn_if_db_on_network_filesystem`（`crates/celeris/src/lib.rs`）は `/proc/self/mountinfo` の
  fstype が nfs/fuse のときだけ警告し、loop デバイス上の ext4（実体は NFS 上の raw イメージ）は
  検出しない。

## 2. 決定

### D1. DB の置き場所の分離と設定・観測の拡張

- 状態ディレクトリ（`~/.local/celeris`: workspaces / releases / backups / tools）は `/home` の
  ままで、DB ファイルだけローカルディスク（例 `/var/lib/celeris/celeris.sqlite3`）に置けるようにする。
  `db` キーは既にあるので設定の追加は不要（`crates/celeris/src/config.rs`）。
- `db` は**文字列**（`db = "<path>"`。従来どおり、互換）と**テーブル**（`[db]` に `path` と
  `busy_timeout_ms`（既定 5000。本番では 15000 を勧める）・`checkpoint_interval_secs`（既定 30）・
  `backup_dir`（既定無し）・`backup_interval_secs`（既定 3600）・`backup_keep`（既定 48）を書く）の
  どちらも受け付ける。`Config.db: DbConfig` は独自の `Deserialize`（内部で `Path(PathBuf) |
  Table(..)` の untagged enum）でこの 2 形を吸収する。相対パス・`~` の展開は `Config::load` が
  `path` と `backup_dir` の両方に行う（既存の `db` / `workspace_root` と同じ規則）。
- 起動時の警告（`warn_if_db_on_network_filesystem`）を拡張する: マウントソース
  （`/proc/self/mountinfo` の該当行、`- <fstype> <source> <options>` の `<source>`）が
  `/dev/loop*` なら「実体がネットワーク越しの可能性」を WARN で出す。判定できない環境（`/proc`
  が無い等）では何もしない（既存と同じ規律）。マウント情報を引く純関数は
  `crates/task-core/src/mountinfo.rs`（`mount_info_for`/`is_network_filesystem`/`is_loop_device`）
  に切り出し、`celeris`（起動時 WARN）と `task-api`（下記）の両方から使う。
- `GET /api/v1/health` の `db` に `filesystem`・`device`（`/proc/self/mountinfo` から引けた範囲。
  判定できなければ `null`）を足す。**DB の絶対パス自体は足さない**: `/health` は無認証
  （`docs/gui/api.md` §1.1、`crates/task-api/tests/auth_and_guards.rs` の
  `health_is_unauthenticated_but_still_host_checked` が「health must not expose the DB path」を
  明示的に検査している）。絶対パスは既に認証済みの `GET /api/v1/config`（`ConfigView.db`）が返して
  いるので、そちらを使う。`relocate-db.sh`（D2）の最終確認も `/health` ではなく `/config` を見る。
  GUI の型生成（`pnpm -C gui gen:types`）を再実行し、`docs/api/v1/api-v1.schema.json` と
  `gui/app/celeris/types.ts` を更新する。

### D2. `relocate-db.sh`（人が実行する。実装エージェントは本番で実行しない）

`scripts/selfdeploy/relocate-db.sh <new-path> [--dry-run]`:

1. `GET /api/v1/tasks` の `counts_by_status`（DB 全体、フィルタに関係なく返る）から
   `running + reviewing` が 0 であることを確認する。0 でなければ何もせず止まる。
2. `current` の sha の `celeris@<sha>` / `celeris-gui@<sha>` を止める（見つからなければ「既に止まって
   いる」として続行）。
3. `sqlite3` があれば `VACUUM INTO`、無ければ**現在のリリースの** `celerisctl db backup`（新規
   サブコマンド。rusqlite の backup API、`task_core::backup_database`）で新しい場所へコピーする。
4. 新しいファイルに `PRAGMA integrity_check`（`sqlite3` か `celerisctl db integrity-check`。同じく
   新規サブコマンド、`task_core::integrity_check`）。`ok` でなければコピーを消して止まる（旧 DB と
   サービスには触れない）。
5. `config.toml` の `db =`（文字列 or `[db].path`）を書き換える。`config.toml.bak-<ts>` を残す
   （`python3` の正規表現置換。トップレベルの `db` 行の行末コメントも許す。テーブル形は `[db]`
   ブロックの `path` 行だけを置換し、`path` が無ければ先頭に足す）。
6. 旧ファイルを `<旧パス>.moved-<ts>` に**リネームする（削除しない）**（`-wal`/`-shm` も同様）。
7. unit を起こす。
8. `GET /api/v1/config` の `db` が新パスであることを確認する（D1 の理由により `/health` は使わない）。

各段階の失敗は、その時点までに済んだことと**手で戻す手順**を stderr に出して exit 1 で止まる
（DB を触る操作の自動巻き戻しは、巻き戻し自体が失敗する余地があるぶん危険なので採らない）。冪等
（設定の `db` が既に指定パスなら何もせず exit 0）。`--dry-run` は in-flight の確認までは行うが、
何も止めず・書かず・コピーしない。ディレクトリは作らない（無ければ「先に `sudo install -d` して
ください」と言って止まる。人が sudo で作る前提）。

### D3. 定期バックアップ

DB をローカルディスクに置くと NFS 側のスナップショットに乗らなくなるので、`[db] backup_dir` が
設定されていれば、デーモンが `backup_interval_secs` ごとに `backup_dir` へ
`celeris-<unix_ts>.sqlite3` を書き、`backup_keep` 世代だけ残す（`crates/celeris/src/db_maintenance.rs`
の `spawn_backup_task`）。バックアップは API/dispatcher の接続とは**別の専用接続**（rusqlite の
backup API、`task_core::backup_database`）で行い、`tokio::spawn` + 内部の `spawn_blocking` の中で
実行するので tick は止まらない。ディレクトリが無ければ（人が作っていなければ）WARN して次回に
リトライする（D2 と同じ「ディレクトリは作らない」規律）。`--mode verify` では起こさない（既存の
「verify は背景ジョブを持たない」規律のまま）。

### D4. 読み書き分離（読み取り専用の小さな接続プール）

`SqliteStore` に、ファイル DB のときだけ読み取り専用（`query_only=ON`。`?mode=ro` の別接続では
なく通常接続に立てる）の接続プールを足す（既定 2 本、`StoreOptions.read_pool_size`）。
`with_read_conn` ヘルパーが、プールがあればそこから 1 本借りて実行し（`Mutex<Vec<Connection>>` +
`Condvar` の簡易プール）、無ければ（インメモリ DB、`read_pool_size = 0`）従来どおり書き込み接続の
`Mutex` にフォールバックする。

`TaskStore` トレイト本体（`report`/`approval`/`notify` 等のサブトレイトは対象外。**今回の
Phase では触らない**、下記「未解決」）の SELECT だけを行うメソッド（`get`/`list`/`ready_tasks`/
`children`/`events_for*`/`events_since`/`latest_event_id`/`event_rows_for`/`list_page`/
`count_by_status`/`org_list`/`org_get`/`project_get`/`project_list`/`repo_get`/`repo_list`/
`repo_active_tasks`/`integration_get`/`integration_list_for_task`/`integration_latest`/
`integration_list_for_project`/`milestone_list`/`milestone_get`/`message_list`/`message_page`/
`comments_for`/`instance_list`/`cluster_settings_get`/`cluster_settings_list`）を `with_read_conn`
経由に変える。これにより GET /board のようなダッシュボード系の読み取りが、書き込みトランザクション
の間も `Mutex` で待たされない（WAL の性質上、読み取りは書き込みをブロックしない）。

### D5. 背景チェックポイント

`StoreOptions.background_checkpoint`（デーモンの接続だけ `true`。`celerisctl` 等デーモン外の接続は
既定 `false` のまま）を立てると `wal_autocheckpoint = 0` にし、`journal_size_limit` を 64 MB に
設定する（`configure_pragmas`）。デーモンは `checkpoint_interval_secs` ごとに専用の接続で
`PRAGMA wal_checkpoint(PASSIVE)` を打ち、WAL が 64 MB を超えていれば `TRUNCATE` も試みる
（`crates/celeris/src/db_maintenance.rs::spawn_checkpoint_task`）。これで fsync がリクエストや
tick の中に落ちない。デーモン内の全接続（dispatcher・API・MCP）を `background_checkpoint: true`
で開く（llm-proxy のリクエストごとのログ接続は対象外。「未解決」参照）。

## 3. 実装ファイル

- `crates/task-core/src/store.rs`: `StoreOptions`（`read_pool_size`/`background_checkpoint` 追加）、
  `ReadPool`/`with_read_conn`、`backup_database`/`integrity_check`（自由関数）、読み取りメソッドの
  移行、新規テスト。
- `crates/task-core/src/mountinfo.rs`（新規）: マウント情報の純関数。
- `crates/celeris/src/config.rs`: `DbConfig`（カスタム `Deserialize`）、`Config.db` の型変更。
- `crates/celeris/src/lib.rs`: `warn_if_db_on_network_filesystem` の loop デバイス検出、
  `build_dispatcher`/`build_mcp_state`/`build_llm_proxy_state`/`api_settings` の `[db]` 配線。
- `crates/celeris/src/db_maintenance.rs`（新規）: 背景チェックポイント・定期バックアップ。
- `crates/task-api/src/{types,state,handlers}.rs`: `DbInfo.filesystem`/`device`、`ApiSettings.
  background_checkpoint`。
- `crates/celeris-mcp/src/state.rs`: `McpState::open` に `background_checkpoint` 引数。
- `crates/celerisctl/src/commands/db.rs`（新規）: `db backup`/`db integrity-check`。
- `scripts/selfdeploy/relocate-db.sh`（新規）。`docs/selfdeploy.md` に §5b を追記。
- `docs/api/v1/api-v1.schema.json` / `gui/app/celeris/types.ts`: `DbInfo` の再生成。

## 4. 未解決・次に見るとよいこと

- D4 は `TaskStore` トレイト本体の SELECT 系だけを読み取りプールへ移した。`report`/`approval`/
  `notify`/`knowledge_run`/`delivery`/`node_session`/`mcp` の各サブトレイト（`crates/task-core/src/
  *.rs` にそれぞれ実装）は今回は触っていない（書き込み接続の `Mutex` のまま）。これらも読み取りが
  多ければ次の Phase で同じ `with_read_conn` パターンを適用できる。
- D5 の `background_checkpoint` は dispatcher・API・MCP の接続には配線したが、`llm-proxy` の
  リクエストごとのログ接続（`crates/llm-proxy/src/log.rs::open`。毎リクエスト開いて閉じる）には
  配線していない。単一の小さな追記専用テーブルへの INSERT なので実害は小さいと見ているが、
  `llm_proxy_requests` の書き込みが多い環境では次の Phase で見直すとよい。
- `relocate-db.sh` は本番の systemd unit・API・DB を直接操作するため、実機での実行はできていない
  （ADR-0009 P-34。CLAUDE.md の制約により、このエージェントは systemctl / systemd-run / 実 ssh に
  触れない）。config.toml の書き換えロジック（最もバグを仕込みやすい部分）は、トップレベル文字列・
  `[db]` テーブル・行末コメント・ファイル末尾の `[db]` など 8 通りの入力で手動検証済み（TOML として
  再パースできることを確認）。人が実機で `--dry-run` → 実行の順に確認すること。
