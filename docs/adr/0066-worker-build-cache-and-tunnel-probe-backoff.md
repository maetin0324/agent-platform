# ADR-0066: 実装エージェントの I/O を減らし、tick からトンネルの target probe を外す

- 日付: 2026-09-23
- 状態: **Accepted**
- 関連: ADR-0043（ワークスペース。D2 の worktree 後始末、D3 のコンテナ実行）、ADR-0053（LLM source。D3 のトンネル、
  Phase 85 の listener/target probe 分離とバックオフ）、ADR-0042（`~/.local/celeris/` の層）

## 1. 文脈（本番で観測した事実）

- 本番ホストの `/home` は `/dev/loop0`（ext4）で、実体は LXC ホストが NFS からマウントした raw イメージファイル。
  fsync 1 回が 69 ms、I/O PSI full 47%。`~/.local/celeris/workspaces/` に 204 個の作業場所があり合計 322 GB
  （最大は 31 GB、29 GB、22 GB…）。実装タスクの claude-code / codex が各 worktree で
  `cargo test --workspace`（1978 件）と GUI ビルドを独立に行うので、`target/`（と `gui/node_modules`、
  `gui/build`）が worktree ごとに丸ごと作られ、2〜3 本の並走で I/O が飽和して SQLite が `db_busy`（503）になり
  GUI が固まった（2026-09-23 18:36〜18:55）。
- 別件: Qwen トンネル（`[[clusters]]` の forward、先方 bnode150:18000）の先方が無応答（`GET /llm/sources` で
  `reachable: false`）のため、dispatcher の tick ごとに `refresh_cluster_tunnels` 内の target probe
  （`crates/celeris/src/lib.rs` の `tunnel_probe`、`TUNNEL_TARGET_PROBE_TIMEOUT` 2 秒）がタイムアウトし、
  `slow tick phases`（`tunnel_ms` ≈ 2000）が 1 時間に 118 件、1 日中続いている。tick は 30 秒ごと。ADR-0053
  Phase 85 は `probe_interval_secs`（既定 30 秒）でバックオフを入れたが、tick の間隔（既定 30 秒）と
  `probe_interval_secs`（既定 30 秒）が同じ長さなので、probe は実質**毎 tick 期限が来て**しまい、
  バックオフが効いていなかった。

## 2. 決定

### D1. 同一リポジトリの worktree で cargo のビルドキャッシュを共有する

- ローカル（クラスタではない）の git worktree タスクのホスト実行（コンテナではない）に、
  `CARGO_TARGET_DIR=<workspace root の親>/build-cache/cargo/<repo-key>` を環境変数として与える。
  `<repo-key>` はリポジトリの実体（`project_repos` の `location.path`。worktree 元の絶対パス）の
  `basename` + そのパス文字列の sha256 先頭 10 桁。同じリポジトリを使う全タスクの worktree が同じ
  `CARGO_TARGET_DIR` を指すので、cargo 自身のディレクトリロック（`.cargo-lock`）で並走が直列化される
  代わりに、フルビルドのやり直しと 15〜30 GB の複製が無くなる。
- 対象外: コンテナ実行（`crates/task-worker/src/container.rs` は既に `[container] env` で
  `CARGO_TARGET_DIR` を扱えるので、ここでは触らない。コンテナは別プロセス名前空間なので worktree 間の
  共有ロックの心配が薄く、人が明示的に設定する余地を残す）。Remote 作業場所（ADR-0059）も対象外
  （クラスタ側のディスクの事情は別。触ると ADR-0018/0019 の契約が広がる）。
- GUI の `node_modules` は対象外: `gui/` は pnpm を使っており、pnpm の store（`~/.local/share/pnpm/store`
  等）は既定で共有されるため、`node_modules` はハードリンクで作られる（実体の複製ではない）。**pnpm 自身が
  既にこの問題を解決している**ので、cargo と同じ仕組みを GUI 側に足す必要はない。
- 設定 `[workspace] shared_build_cache = true`（既定 `true`）で切れる。`[workspace] build_cache_dir`
  （既定 `~/.local/celeris/build-cache`）で置き場を変えられる。
- 前置き（`crates/task-worker/src/preamble.rs`）の「作業場所」の節に、git の worktree があり
  `shared_build_cache` が有効なときだけ「`target/` はリポジトリ間で共有するビルドキャッシュにある
  （`CARGO_TARGET_DIR`）。worktree ごとに再ビルドしない。」の 1 行を足す。

### D2. 終端タスクの作業場所から、ビルド生成物だけを自動で刈る

- ADR-0043 D2 は「中止（cancel）のときだけ worktree ごと消す」と決めている。ここではそれを変えず
  （done / failed のまま残った worktree は差分を見るために必要）、**終端（done / failed / cancelled）に
  なってから `[workspace] prune_after_secs`（既定 86400 秒 = 24 時間）経った作業場所から、ビルド生成物
  だけを削る**: 各 worktree（git worktree。`dir` のシンボリックリンクは対象外 — シンボリックリンク先は
  人の実体なので触らない）の直下で次の相対パスが存在すれば消す: `target`、`node_modules`、`build`、
  `.venv`、`gui/node_modules`、`gui/build`。ソースツリー（`.git` を含む）と `artifacts/`、`.celeris/`
  （成果物・記録）は残す。
- `prune_after_secs = 0` で無効化できる（既定は有効）。
- 判定は「対象ディレクトリが存在するか」だけ（`du` はしない。サイズを数えるコストの方が高い）。
- dispatcher の tick に軽い phase を 1 つ足す（`prune_one_workspace`）: 対象を探すところ（DB の
  done/failed/cancelled を見て、`updated_at` から `prune_after_secs` 経っていて、かつ刈れる生成物が
  まだ残っている最初の 1 件）までは同期（メタデータ確認だけで軽い）。**実際の削除は別スレッド
  （`std::thread::spawn`）に逃がし、tick はそれを待たない**。削除が終わったら `workspace_pruned`
  イベントを積む（そのスレッドから直接 `store.append_event` する。ストアは `Mutex<Connection>` で
  直列化されているので複数スレッドからの追記は安全）。1 tick に最大 1 作業場所。
- `celerisctl workspace prune [--dry-run] [--older-than <secs>]` を足し、人が手で（本番の
  `prune_after_secs` を待たずに）回せるようにする。`--dry-run` は消さずに候補と刈れるパスを列挙する。
- `Event::WorkspacePruned { removed: Vec<String> }`（作業場所からの相対パス）を追加する。
  `docs/api/v1/event.schema.json` と GUI の生成型に影響するので `UPDATE_SCHEMA=1 cargo test -p task-core`
  と `pnpm -C gui gen:types` を通す。GUI のタイムライン表示は既存の `default: null`（バッジだけ）に自然に
  収まるので、表示コードの変更は不要（`workspace_mode_downgraded` 等、他の「バッジだけ」のイベントと同じ
  scope）。

### D3. トンネルの target probe を tick から外し、指数バックオフする

- `refresh_cluster_tunnels`（`crates/task-dispatch/src/dispatcher.rs`）は、forward の**リスナー**の有無
  （軽い TCP connect）は従来どおり毎 tick 見るが、**target の健康（`/v1/models`）は tick の同期経路から
  外す**。listener が有るとき:
  - その forward を初めて観測する（`tunnel_probe_state` にまだ記録が無い）ときだけ、**この tick の中で
    1 回だけ**同期に probe する（起動直後・listener が今初めて有りになった、の 2 通りだけに限られるので
    影響は小さい。Phase 85 までの「listener が有ればまず 1 回は確かめる」という前提を壊さない）。
  - 2 回目以降は、専用スレッド（`Dispatcher` が forward 一覧から遅延生成する 1 本の OS スレッド。
    `[[clusters.forwards]] probe_interval_secs`〈既定 30 秒〉を最小間隔として、forward ごとに
    `Instant` ベースで probe の要否を判断し、期限が来ていれば probe してから次の期限を計算する）が裏で
    probe し続け、結果を `Arc<Mutex<HashMap<forward, TargetProbeState>>>` に置く。**tick はこの
    `Mutex` を読むだけ**（ロックは一瞬で、ssh も HTTP も呼ばない）。
  - 専用スレッドの probe が**連続して失敗**すると、次の probe までの間隔を
    `probe_interval_secs`（既定 30） → 60 → 120 → 240 → 480 → 600（上限）と倍々に伸ばす。**1 回でも
    成功すれば `probe_interval_secs` に戻す**。バックオフの計算は純粋関数
    （`next_probe_interval_secs(current, healthy)`）として切り出し、スレッドやタイマーに依存せず
    ユニットテストする。
  - `Dispatcher` の `Drop` で専用スレッドに停止を伝える（`Arc<AtomicBool>`。join はしない — tick を
    止めない設計と同じ理由で、終了もブロックしない）。
- 状態遷移（`tunnel: state transition` のログと `TunnelEvent`）は変えない: 同じフェーズ
  （`Up`/`Down`/`TargetUnreachable`）が続く間は 1 回しか積まない（Phase 85 の規律をそのまま使う）。
- `crates/celeris/src/lib.rs` の変更は `tunnel_probe` / `tunnel_listener_probe` の doc コメントの
  更新（「毎 tick 呼ばれる」→「専用スレッドから呼ばれることがある」の訂正）だけに留める。この Phase は
  Phase 110a が同ファイルの DB / health 部分を並行して編集しているため、それ以外は触らない。

## 3. 採らない

- `[[clusters.forwards]] probe_interval_secs` を tick の間隔と連動させる・撤廃する（既存の設定面はその
  まま使い、上限だけ新しく足す）。
- ビルドキャッシュをコンテナ実行や Remote 作業場所にも広げる（別の事情〈イメージ内の `/root` 権限、
  クラスタ側のディスク〉が絡むので、この Phase のスコープ外）。
- 終端タスクの worktree そのものを消す・ブランチを消す（ADR-0043 D2 の決定はそのまま。ここで消すのは
  「生成物」だけ）。
- `du` によるサイズ計測（対象ディレクトリの存在だけで判断する。人の指示）。

## 4. 受け入れ条件

- **D1**: `[workspace] shared_build_cache`（既定 true）、`CARGO_TARGET_DIR` がローカル git worktree の
  ホスト実行にだけ注入される（コンテナ・Remote には注入されない）ことをテストで確認。前置きに 1 行。
- **D2**: `prune_after_secs`（既定 86400、`0` で無効）、tick の軽い phase（1 tick に最大 1 か所、削除は
  別スレッド）、`workspace_pruned` イベント、`celerisctl workspace prune [--dry-run] [--older-than]`。
  スキーマと GUI 型生成の差分ゼロ。
- **D3**: target probe が tick の同期経路から外れる（遅い偽 probe でも 2 回目以降の tick が速いことを
  テストで確認）、バックオフの純粋関数のユニットテスト、既存の Phase 85 のテスト（listener/target の
  区別、re-add しない、イベントは 1 回だけ）は原則そのまま通る（バックオフの間引きを直接テストしていた
  1 件だけ、新しい非同期モデルに合わせて書き直す）。
- 全条件: `cargo test --workspace --no-fail-fast` FAILED 0、`cargo clippy --workspace --all-targets -- -D
  warnings` 警告 0。GUI に触れた分は `pnpm -C gui typecheck && pnpm -C gui lint && pnpm -C gui test &&
  pnpm -C gui gen:types`（差分ゼロ）。
