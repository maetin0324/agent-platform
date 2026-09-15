# ADR-0018: 複数クラスタへのタスク投入（ログインノードで ssh 実行）

- 日付: 2026-09-15
- 状態: **Proposed**（設計のみ。実装は人間の判断を待つ）
- 関連: DESIGN §5.8（Workspace。接続層との境界）、§6 非目標、ADR-0003（ワーカープロトコル）、ADR-0012（プロバイダ）、ADR-0013 D5（DB はローカルディスク）

## 文脈

`WorkspaceSpec::Remote { cluster, path }` は**型だけ**あり、ディスパッチャは remote のタスクを見つけると
「remote workspace is not supported」と警告して `ready` のまま放置する（`dispatcher.rs`）。pegasus / sirius のような複数クラスタに
タスクを投げる仕組みは無い。DESIGN §6 の非目標に「リモートワークスペースの実装」と書いて外していた部分で、本 ADR でその判断を改める。

人間の選択: **クラスタのログインノードに ssh して直接ワーカーを起動する**（ジョブスケジューラ経由は当面採らない）。

## 決定（案）

### D1. `[[clusters]]` 設定とリモートの実行単位

```toml
[[clusters]]
id = "pegasus"
host = "pegasus.example"          # ssh の宛先（~/.ssh/config の Host 名でよい）
workdir_root = "/work/USER/taskd" # クラスタ側のワークスペース置き場
concurrency = 4                   # このクラスタで同時に走らせる run の上限
setup = ["module load python/3.12"]  # run の前に流すコマンド（任意）
env = { CLAUDE_CONFIG_DIR = "/home/USER/.claude" }   # クラスタ側の認証の置き場
```

- 並列度は **プロバイダ（アカウント）× クラスタ**の二次元で数える。どちらかの上限に達したら、その組は飛ばして次を試す（ADR-0012 の選択手順の拡張）。

### D2. 実行: ssh 越しに同じワーカープロトコルを流す

- ローカルのアダプタが `ssh <host> -- 'cd <workdir> && <setup> && <adapter コマンド>'` を起動し、**stdin / stdout の JSON Lines をそのまま中継**する。
  プロトコルは変えない（`task-worker` の `subprocess` の起動コマンドを差し替えるだけ）。
- 認証は `ssh-agent`（鍵の転送はしない。パスワード認証は扱わない）。`BatchMode=yes` で対話を禁じ、`ServerAliveInterval` で切断を検出する。
- ssh の失敗（接続不可・認証不可・`setup` の失敗）は**供給側失敗**として扱い、そのクラスタを cooldown にして requeue する（attempts を消費しない。ADR-0010 D5 と同じ扱い）。

### D3. ワークスペースの同期

- 既定は **rsync で往復**する。run の前に `rsync -a --delete <local>/ <host>:<remote>/`、run の後に `rsync -a <host>:<remote>/ <local>/`。
- 共有ファイルシステムのクラスタでは `sync = "none"`（同期しない）を選べる。`.git` を含めるかは `rsync_excludes` で設定する。
- 成果物（`ArtifactProduced`）の sha256 は**同期後のローカル側で計算**する（真実はローカルの DB とワークスペース）。

### D4. 受け入れ条件の判定もリモートで行う

- `Check::Command` は**ワークスペースのある場所で実行する**必要がある（クラスタ上のモジュールやデータに依存するため）。
  リモートのタスクでは、判定コマンドも ssh 越しに実行する（`Workspace::exec` の実装をリモート用に差し替える）。
- `Check::ArtifactExists` は同期後のローカルで判定する。`Check::Reviewer` の run も、対象と同じクラスタで起動する。

### D5. 失敗と時間の扱い

- ssh の切断で run の出力が途切れた場合は、リースの期限切れと同じ回収経路に載せる（`lease_expired` → 再試行）。
- リモートの run にも `max_wall_secs` を適用する（ssh 側で `timeout` を掛け、ローカル側でも監視する。二重の安全弁）。
- クラスタが落ちている間はそのクラスタを cooldown にし、他のクラスタ・ローカルへフォールバックする（タスクが `cluster` を指定していない場合）。

### D6. タスクからの指定

- `WorkspaceSpec::Remote { cluster, path }` をそのまま使う。`cluster` が設定に無ければ、そのタスクは「経路なし」として扱う（`unroutable`。ADR-0012 の P-33 と同じ）。
- `taskctl add --cluster pegasus --workspace /work/... ` と、API の `NewTaskSpec.workspace` の拡張（`{"kind":"remote","cluster":"pegasus","path":"..."}`）で指定する。

### D7. 秘密と安全

- クラスタ側の認証情報（`CLAUDE_CONFIG_DIR` の中身）は taskd が触らない。設定に書くのは**場所だけ**。
- `ssh` の宛先・鍵・`known_hosts` は運用側の責任。taskd は `StrictHostKeyChecking` を変更しない。
- API は `[[clusters]]` の `host` / `workdir_root` / `concurrency` を返してよいが、`env` の値は返さない（ADR-0013 D11 と同じ）。

## 採らない（当面）

- ジョブスケジューラ（Slurm 等）への投入。ログインノードでの直接実行で足りるうちは増やさない。必要になったら `[[clusters]] kind = "slurm"` として D2 を差し替える形で足せる。
- クラスタ側に taskd を常駐させる構成（DB が分散すると真実が 1 つでなくなる）。
- ファイル同期に共有オブジェクトストレージを使う構成。

## 影響

- 新しいクレート `task-remote`（ssh と rsync の起動、リモート `Workspace` の実装）。`task-worker` のアダプタ起動部を差し替え可能にする。
- DESIGN §5.8 の「Remote は型のみ」と §6 非目標の「リモート実行」を改める。
- 受け入れ条件は Phase 12（DESIGN §6）に書く。**テストは ssh 先を localhost にして行う**（外部ネットワークに出ない。CLAUDE.md の規則を守る）。
