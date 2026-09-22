# ADR-0059: コマンドを実行するだけのオペレーションを、クラスタ上で worktree 無しに動かす

- 日付: 2026-09-22
- 状態: **Accepted**（Phase 99 として実装する。実機の障害への対応: 2026-09-22 11:37 UTC、タスク
  01M34CDBGFKSD8VGYMDCA5GAQM）
- 関連: ADR-0018（複数クラスタでのコマンド実行）D1・D4、ADR-0019（worktree 同期）D1・D3、
  ADR-0041 D1（ローカルの `WorkspaceMode`）、ADR-0046 D8（クラスタの道具の許可リスト）、
  ADR-0054 Phase 98 追記（CoS はクラスタ作業を組織に流す）

## 1. 文脈

Phase 98 の本番反映後、CoS に「pegasus のログインノードで `pegasusinfo` と `rbudgetcheck` を実行して
結果を報告」と依頼したところ、CoS は正しく断らず `cluster-hpc`（Operations 部署の課、tools に
`cluster:pegasus`）へ `create_task` を発行した。`workspace = {"kind":"remote","cluster":"pegasus",
"path":"~"}` を付けたが、pegasus が接続された後 3 回連続で次のエラーになり failed した:

```
adapter: workspace prepare: remote error: cannot prepare the git worktree on pegasus (exit Some(66)): no such directory: '~'
```

原因は 2 つ。

1. **`[[clusters]] pegasus` の `sync = "worktree"` が全タスクに適用される。** ADR-0019 D1 の worktree
   準備（`crates/task-worker/src/ssh.rs::ensure_worktree`）は `cd <path> && git rev-parse
   --git-common-dir` を前提にしており、コードの diff を一切作らない「コマンドを実行するだけ」の仕事
   （`pegasusinfo` を叩いて出力を報告する）でも、`path` を git リポジトリとして worktree を切ろうと
   する。exit 65 = not a git repository、exit 66 = no such directory（`ensure_worktree` の `script`
   内の判定）。
2. **`path` の `~` を `shq`（シングルクォート引用）で包むため、リモートのシェルで展開されない。**
   `cd '~'` はホームディレクトリではなく文字どおり `~` という名前のディレクトリを探す。

人間の要求（実機での指示）:「クラスタやローカルにおいてもコマンドを実行するだけのオペレーションタスクは
如何様にも発生しうる。ソースコードの diff が発生しないオペレーションについてはこの制限を撤廃する」。

## 2. 決定

### D1. `WorkspaceSpec::Remote` に `mode: Option<WorkspaceMode>` を足す

ADR-0041 D1 がローカルの作業場所に導入した語彙（`WorkspaceMode::Worktree | Shared`）を、そのまま
`WorkspaceSpec::Remote` にも使う（`crates/task-core/src/model.rs`）。

```rust
Remote {
    cluster: String,
    path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<WorkspaceMode>,
}
```

- 省略時は JSON に出さない。Phase 98 までの `{"kind":"remote","cluster":"…","path":"…"}` と 1 バイトも
  変わらない（既存の DB 行・イベントログ・スナップショットは無変更のまま読める）。
- **省略 / `worktree`**: 従来どおりクラスタの `[[clusters]] sync` 設定に従う（`worktree` / `rsync` /
  `none`。ADR-0018 D4 / ADR-0019 D1）。
- **`shared`**: **同期も worktree も行わない**（`SyncMode::None` 相当）。`path` をそのままクラスタ側の
  作業ディレクトリとして使う。手元の写し（`workspace_root/<task_id>`）は空のディレクトリ +
  `.taskd/remote-exec` + `artifacts/` だけになる（pull/push は無し）。受け入れ条件の `Check::Command`
  はこれまでどおり `path`（この場合は写しでなくクラスタ側そのもの）でクラスタ側実行する。

`mode` の意味は「クラスタ側の同期方針」であって、`WorkspaceSpec::Local.mode`（作業ツリーを分離するか
どうか）とは軸が違う。しかし人間が指定する語彙（`"worktree"` / `"shared"`）と、選ぶ理由（「コードの
diff を作る仕事か、コマンドを実行するだけの仕事か」）は同じなので、同じ enum・同じ文字列を再利用する。

### D2. `~` の展開はリモート側の `$HOME` で行う。`shq` は変えない

`path` の `~`（単独）と `~/…`（先頭）を、生成するリモートスクリプトの中で `"$HOME"`（クォート済みの
シェル変数）に置き換える。`crates/task-worker/src/ssh.rs` の `cd` を含むすべてのスクリプト生成箇所
（worktree 準備・rsync の宛先・`.taskd/remote-exec` の `cd`・`Check::Command` 実行の `cd`）に同じ展開を
通す。**他の部分の引用（`shq`）はこれまでどおり**（`~` 以外の値は従来どおりシングルクォート引用のまま
で、シェルインジェクションの扱いは変えない）。

`~user` のような別ユーザ指定は展開しない（celeris はリモート側の他ユーザの home を知らないため。
ADR-0039 D5 のローカル `expand_home` と同じ方針）。

celeris 側（`WorkspaceSpec::Remote.path`）の値そのものは書き換えない（DB には `~` のまま残る。展開は
実行時にシェルスクリプトの文字列としてのみ行う）。

### D3. 自動の格下げ: `mode` 省略・exit 65・`repos` が無ければ `shared` として続行する

`mode` が省略されたタスクで worktree 準備が **exit 65（not a git repository）** になり、かつタスクに
`repos`（ADR-0043 D2 のリポジトリ指定）が 1 つも無い（＝コードを触る予定が最初から無い）なら、**失敗に
せず** `SyncMode::None`（`shared` 相当）で同じ run の中で続行する。

- ストアの `task.workspace` を `mode: Some(Shared)` に書き戻す（`assign_if_needed` と同じ
  `TaskStore::update_task` の使い方。`status` / `attempts` / `lease` は触らない）。これにより、
  この後に別途 `SshSettings` を組み直す判定（`Check::Command`）の段でも同じ `shared` が使われる
  （worktree 準備をもう一度試みて同じ exit 65 で落ちることを防ぐ）。
- イベント `Event::WorkspaceModeDowngraded { cluster, path, reason }` を残す（GUI・監査から見える）。
- **`mode` を明示的に `"worktree"` にしたタスクは格下げしない**（利用者の意図を尊重し、従来どおり
  エラーにする。「本当は git リポジトリのはずなのに違った」という設定ミスを隠さない）。
- **exit 66（ディレクトリが無い）は従来どおり失敗**（D2 の `~` 展開で解決する問題であり、格下げでは
  直らない — ディレクトリが無いなら `shared` にしても `cd` が失敗するだけ）。

### D4. CoS の対話指示

`crates/task-worker/src/preamble.rs` の `conversation_instructions`（Secretary 分岐）に、Phase 98 の
クラスタ作業ルールの直後、次の 1 文を足す: 「コマンドを実行する・状態を見る・ジョブを流すだけで
コードの diff を作らない仕事は `workspace` に `"mode":"shared"` を付ける。`path` はクラスタの作業
ディレクトリを使う」（D6 の `work_dir` 追加により、`~` を既定にはしない。D6 参照）。
`actions_instructions()` の `create_task` 例の説明にも `workspace.mode` の使い方を足す。

### D5. `.taskd/remote-exec` を `.celeris/remote-exec` に改名する（プラットフォーム名の統一）

`crates/task-worker/src/ssh.rs` の**リモート実行ラッパ**（ADR-0018 D3。写しの直下に置き、クラスタで
コマンドを実行するための実行可能スクリプト）と、その管理ディレクトリは taskd 時代の名残で
`.taskd/remote-exec` のままだった。プラットフォーム名は Celeris に変わっているので、`.celeris/` に
改名する。

- `crates/task-worker/src/ssh.rs::write_remote_exec_helper` が作るパスを `.taskd/remote-exec` →
  `.celeris/remote-exec` にする。`remote_exec_instructions` の文面（ワーカーへの指示）も合わせる。
- `crates/celerisctl/src/commands/worker.rs`（`worker run --cluster`、ADR-0018 M6）のエラー文面も同じ
  改名に合わせる。
- **後方互換**: `SYNC_ALWAYS_EXCLUDED`（P-46）には `.taskd/` を残したまま `.celeris/` を追加する
  （両方とも同期の対象から外す。古い写し・クラスタ側に残った `.taskd/` の残骸を rsync に巻き込まない
  ため）。run 開始時（`write_remote_exec_helper`）に、古い `<写し>/.taskd/remote-exec` があれば消す
  （新しいラッパと混同しないため。`.taskd/artifacts/<task_id>/` は ADR-0036 D1 の**別の**規約〈共有
  workspace の成果物置き場〉で、この改名の対象ではない — 触らない）。
- 既存 ADR（ADR-0018 D3 本文）の `.taskd/remote-exec` という記述は書き換えない（過去の決定の記録として
  残す。この節が「以後は `.celeris/remote-exec`」の参照点になる）。

### D6. クラスタごとの「作業ディレクトリ」（`work_dir`）を持てるようにする

HPC クラスタではホーム（`~`）は作業用に使わないのが普通で（クォータが小さい、`/work/<group>/<user>` の
ような大容量領域が別にある）、`~` を既定の作業場所にしてはいけない。クラスタごとに「実効の作業
ディレクトリ」を持てるようにし、`path` を相対（または省略）で書けるようにする。

- **設定**: `[[clusters]] work_dir`（例 `/work/NBB/rmaeda`。省略可）を足す（`crates/celeris/src/config.rs`
  `ClusterConfig`。`Config` は `deny_unknown_fields` なので `config/celeris.example.toml` にも例を足す）。
- **DB の上書き**（GUI から変更できるように）: migration `0025_cluster_settings.sql`
  （`schema_version` 25）で `cluster_settings(cluster_id TEXT PRIMARY KEY, work_dir TEXT, updated_at TEXT
  NOT NULL)`。**実効値 = DB の上書きがあればそれ、無ければ設定の `work_dir`、どちらも無ければ `None`**。
- **API**: `GET /clusters` の `ClusterView` に `work_dir: Option<String>` と
  `work_dir_source: Option<String>`（`"settings"` = DB / `"config"` = 設定ファイル）を足す。
  `PUT /clusters/{id}/settings`（管理系、ADR-0017 M3 の流儀でトークン必須）で
  `{"work_dir": "/work/..." | "~/..." | null}` を受ける。絶対パスか `~`/`~/…` だけ許可（それ以外は 422
  `validation`）。`null` は上書きを消す（行を削除し、設定ファイルの値に戻る）。
- **`WorkspaceSpec::Remote.path` の扱い**: `path` を**省略可**にする（`#[serde(default)]`。省略すれば
  空の `PathBuf`）。`path` が絶対（`/` 始まり）または `~`/`~/…` ならそのまま使う（D2 の展開のとおり）。
  それ以外（空、または `/`・`~` で始まらない相対パス）は**実効 `work_dir` からの相対**として解決する
  （空なら `work_dir` そのもの）。実効 `work_dir` が無ければ、その run は**従来どおりエラー**にする
  （文面: 「クラスタ `<id>` の作業ディレクトリが未登録です。`PUT /clusters/{id}/settings` かクラスタ
  画面で登録してください」）。この解決はディスパッチャ側（`crates/task-dispatch/src/dispatcher.rs`
  `cluster_of` / `ClusterSpec::ssh_settings` 周辺）で行う純粋関数（`crates/task-worker/src/ssh.rs`）。
- **CoS の指示**（D4 と合わせて改める）: 「コマンド実行だけの仕事は `workspace` に
  `{"kind":"remote","cluster":"<id>","path":"<クラスタの作業ディレクトリ>","mode":"shared"}`。`path` は
  クラスタ一覧に出ている作業ディレクトリを使う。未登録のクラスタなら `path` を省略し（celeris が実効値
  を使う）、返事で『クラスタ画面で作業ディレクトリを登録してほしい』と一言添える。`~` は使わない」。
- **N-1 互換**: テーブルを 1 つ足すだけ（既存の列・行には触れない）ので、旧バイナリでの読み取りに影響
  しない（`verify.sh` の検査 5 が確認する）。

## 3. 採らないこと

- クラスタ設定 `[[clusters]] sync` の意味は変えない（`mode` は**タスクごとの上書き**であって、クラスタ
  の既定値を変えるものではない）。
- Operations 部署以外への割り当てロジック（ADR-0046 D5）は変えない。`mode` はワークスペースの同期方針
  だけを決め、担当の決定には関与しない。
- `mode` を `WorkspaceSpec::Local` と共通のフィールド定義にまとめる（型としては同じ `WorkspaceMode` を
  再利用するが、`Local`/`Remote` それぞれの variant に個別のフィールドとして持たせる。共通化すると
  「ローカルの worktree 分離」と「リモートの同期方針」という別の軸が 1 つのフィールドに混ざり、
  `Remote` の `shared` を「ローカルと同じ意味」と誤解しやすくなるため）。
- 格下げの判定を文字列（stderr のメッセージ）のパースで行う（`WorkspaceError` に専用のバリアント
  `NotAGitRepository` を足し、型で判定する。ADR-0013 D9 の「供給側失敗はエラーの型で分ける」と同じ
  方針）。

## 4. 結果

- `WorkspaceSpec::Remote.mode`（省略可、既定なし=従来どおり）。
- `crates/task-worker/src/ssh.rs`: `~` 展開、`SyncMode::None` の経路が `shared` から確実に選ばれる。
- `crates/task-dispatch/src/dispatcher.rs`: `ClusterSpec::ssh_settings` が `WorkspaceMode` を受け取り
  `shared` なら `SyncMode::None` を強制する。`run_worker` の worktree 準備が exit 65 のときの自動格下げ
  と `Event::WorkspaceModeDowngraded` の記録。
- `crates/task-core/src/console_action.rs::ConsoleAction::CreateTask.workspace` は `WorkspaceSpec` を
  そのまま持つ（Phase 98 で追加済み）ので、`"mode":"shared"` は追加のコード変更なしで受け付けられる。
  `crates/task-ops/src/actions.rs::create_task_action` と `crates/task-ops/src/add.rs::NewTaskSpec` に
  `workspace_mode` を通し、`POST /console/instruct` 経由の `create_task` からも DB のタスクまで
  `mode` が届くようにする。
- テストは localhost 相当のスタブ ssh（`SshSettings::ssh_command` の差し替え）で、外部ネットワークに
  出ない（CLAUDE.md の規則。実クラスタでの確認は人の操作を伴う手順として `docs/PROGRESS.md` に記録）。
- `.taskd/remote-exec` → `.celeris/remote-exec`（`SYNC_ALWAYS_EXCLUDED` は両方を除外。ADR-0036 D1 の
  `.taskd/artifacts/<task_id>/` は別の規約で対象外）。
- `[[clusters]] work_dir`、`cluster_settings` テーブル（schema 25）、`GET /clusters` の `work_dir` /
  `work_dir_source`、`PUT /clusters/{id}/settings`。相対・省略 `path` は実効 `work_dir` から解決する。

## Phase 99b 追記（2026-09-22）

本番（2026-09-22 13:19 UTC、タスク 01M34MACCEZ032A6YF8R4BMFM1）で、`work_dir` を登録した直後に CoS へ
コマンド実行だけの仕事を頼んだところ、継続中（resume）のセッションだったため `run_extras` の
`is_cos_conversation && !continuing` の条件でクラスタ一覧が空になり、CoS はクラスタも work_dir も
知れなかった。D6 の `clusters` は `active_projects`（前回からの**差分**で足りる）とは性質が違い、
`recent_work` / `knowledge` / `profile` / `role` と同じ「いまの状態」（3 行程度で軽い）なので、
継続中でも毎回渡すべきだった。`crates/task-dispatch/src/dispatcher.rs::run_extras` の条件を
`is_cos_conversation` だけに直した（`!continuing` を外す）。`crates/task-worker/src/preamble.rs` の
`render`/`clusters_section` はもともと単一の描画経路（差分専用の経路は無い）で、`context.clusters` が
渡ればそのまま描かれることを確認した。
